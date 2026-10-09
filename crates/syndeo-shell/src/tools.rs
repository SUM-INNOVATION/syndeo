//! The example tools, given to a home that has no `tools` of its own.
//!
//! install.sh copies `tools/wordcount.wat` into `~/.syndeo/tools` when it
//! installs, and leaves an existing file alone. The macOS package cannot do
//! the same: it runs as root, for nobody in particular, and must write into no
//! one's home. So the shell does it, as the person running it, the first time
//! it starts with a home that has nothing called `tools` — and never once
//! anything by that name exists, even an empty directory, which is how to say
//! no.
//!
//! The directory appears whole or not at all. The tools are written into a
//! private staging directory in the home, which is then renamed into place by
//! a rename that refuses to replace anything (`renamex_np(RENAME_EXCL)` on
//! macOS, `renameat2(RENAME_NOREPLACE)` on Linux; where neither works,
//! nothing is seeded). Two first runs at once leave one directory; a crash
//! leaves at most a staging directory, which the next run looks past; and
//! nothing is followed through a symlink or written over.

use std::collections::BTreeSet;
use std::ffi::{CString, OsString};
use std::fs;
use std::io::{self, Read, Write};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

/// The extensions the agent loads as tools.
const EXTENSIONS: [&str; 2] = ["wat", "wasm"];

/// Larger than any example has reason to be.
const MAX_TOOL: u64 = 1024 * 1024;

const STAGING_PREFIX: &str = ".syndeo-tools-seed.";

/// A staging directory this old belonged to a run that never finished.
const STALE_AFTER: Duration = Duration::from_secs(60 * 60);

#[cfg(target_os = "macos")]
mod sys {
    pub const O_NOFOLLOW: i32 = 0x0100;
    pub const EEXIST: i32 = 17;
    pub const ENOTEMPTY: i32 = 66;
    pub const EINVAL: i32 = 22;
    pub const ENOTSUP: i32 = 45;
    pub const EOPNOTSUPP: i32 = 102;
    pub const ENOSYS: i32 = 78;
}

#[cfg(not(target_os = "macos"))]
mod sys {
    pub const O_NOFOLLOW: i32 = 0o400000;
    pub const EEXIST: i32 = 17;
    pub const ENOTEMPTY: i32 = 39;
    pub const EINVAL: i32 = 22;
    pub const ENOTSUP: i32 = 95;
    pub const EOPNOTSUPP: i32 = 95;
    pub const ENOSYS: i32 = 38;
}

/// What seeding did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Seeded {
    /// The home already has something called `tools`, which was left alone.
    Present,
    /// There are no tools beside this binary to give it.
    NoSource,
    /// This many tools were put in place.
    Seeded(usize),
    /// Another first run got there first, and its directory was left alone.
    Lost,
    /// The filesystem cannot rename without replacing, so nothing was seeded.
    Unsupported,
}

/// Give `home` the tools installed beside this binary, unless it already has
/// something called `tools`.
pub fn seed_example_tools(home: &Path) -> io::Result<Seeded> {
    match crate::supervisor::install_dir() {
        Some(dir) => seed_from(&dir.join("tools"), home),
        None => Ok(Seeded::NoSource),
    }
}

/// [`seed_example_tools`], from `source` rather than the installed directory.
pub fn seed_from(source: &Path, home: &Path) -> io::Result<Seeded> {
    seed_with(source, home, || {})
}

/// The whole of seeding, with a hook that runs just before the staging
/// directory is renamed into place.
fn seed_with(source: &Path, home: &Path, before_publish: impl FnOnce()) -> io::Result<Seeded> {
    let destination = home.join("tools");
    match fs::symlink_metadata(&destination) {
        Ok(_) => return Ok(Seeded::Present),
        Err(err) if err.kind() == io::ErrorKind::NotFound => {}
        Err(err) => return Err(err),
    }
    let tools = read_tools(source)?;
    if tools.is_empty() {
        return Ok(Seeded::NoSource);
    }
    let names: BTreeSet<OsString> = tools.iter().map(|(name, _)| name.clone()).collect();
    remove_stale(home, &names);

    let staging = make_staging(home)?;
    let mut written = Vec::new();
    let staged = (|| -> io::Result<()> {
        for (name, bytes) in &tools {
            let path = staging.join(name);
            let mut file = fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o644)
                .custom_flags(sys::O_NOFOLLOW)
                .open(&path)?;
            written.push(name.clone());
            file.write_all(bytes)?;
            file.sync_all()?;
            fs::set_permissions(&path, fs::Permissions::from_mode(0o644))?;
        }
        fs::set_permissions(&staging, fs::Permissions::from_mode(0o755))?;
        fs::File::open(&staging)?.sync_all()
    })();
    if let Err(err) = staged {
        discard(&staging, &written);
        return Err(err);
    }

    before_publish();
    match rename_exclusive(&staging, &destination) {
        Ok(()) => Ok(Seeded::Seeded(tools.len())),
        Err(err) => {
            discard(&staging, &written);
            match err.raw_os_error() {
                Some(code) if code == sys::EEXIST || code == sys::ENOTEMPTY => Ok(Seeded::Lost),
                Some(code)
                    if code == sys::EINVAL
                        || code == sys::ENOTSUP
                        || code == sys::EOPNOTSUPP
                        || code == sys::ENOSYS =>
                {
                    Ok(Seeded::Unsupported)
                }
                _ if err.kind() == io::ErrorKind::Unsupported => Ok(Seeded::Unsupported),
                _ => Err(err),
            }
        }
    }
}

/// Every tool in `source`: regular files only, read through a handle that is
/// checked to be the file that was looked at.
fn read_tools(source: &Path) -> io::Result<Vec<(OsString, Vec<u8>)>> {
    let entries = match fs::read_dir(source) {
        Ok(entries) => entries,
        Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(err) => return Err(err),
    };
    let mut tools = Vec::new();
    for entry in entries {
        let entry = entry?;
        let name = entry.file_name();
        if !is_tool_name(&name) {
            continue;
        }
        let path = entry.path();
        let seen = fs::symlink_metadata(&path)?;
        if !seen.file_type().is_file() || seen.len() > MAX_TOOL {
            continue;
        }
        let Ok(mut file) = fs::OpenOptions::new()
            .read(true)
            .custom_flags(sys::O_NOFOLLOW)
            .open(&path)
        else {
            continue;
        };
        let opened = file.metadata()?;
        if !opened.is_file() || opened.dev() != seen.dev() || opened.ino() != seen.ino() {
            continue;
        }
        let mut bytes = Vec::new();
        (&mut file).take(MAX_TOOL + 1).read_to_end(&mut bytes)?;
        if bytes.len() as u64 > MAX_TOOL {
            continue;
        }
        tools.push((name, bytes));
    }
    tools.sort();
    Ok(tools)
}

fn is_tool_name(name: &OsString) -> bool {
    let Some(name) = name.to_str() else {
        return false;
    };
    !name.starts_with('.')
        && Path::new(name)
            .extension()
            .and_then(|extension| extension.to_str())
            .is_some_and(|extension| EXTENSIONS.contains(&extension))
}

/// A new, private staging directory in `home`.
fn make_staging(home: &Path) -> io::Result<PathBuf> {
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    for _ in 0..16 {
        let nanos = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .map(|elapsed| elapsed.as_nanos() as u64)
            .unwrap_or(0);
        let nonce = nanos ^ COUNTER.fetch_add(1, Ordering::Relaxed).rotate_left(32);
        let path = home.join(format!(
            "{STAGING_PREFIX}{}.{nonce:016x}",
            std::process::id()
        ));
        match fs::DirBuilder::new().mode(0o700).create(&path) {
            Ok(()) => return Ok(path),
            Err(err) if err.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(err) => return Err(err),
        }
    }
    Err(io::Error::new(
        io::ErrorKind::AlreadyExists,
        "no unused name for a staging directory",
    ))
}

/// Remove what this run put in its staging directory, and the directory.
fn discard(staging: &Path, written: &[OsString]) {
    for name in written {
        let _ = fs::remove_file(staging.join(name));
    }
    let _ = fs::remove_dir(staging);
}

/// Staging directories left by runs that never finished, removed only when
/// each is plainly one of them: a real directory, this user's, over an hour
/// old, holding nothing but this user's regular files under names a seed
/// writes.
fn remove_stale(home: &Path, expected: &BTreeSet<OsString>) {
    let Ok(entries) = fs::read_dir(home) else {
        return;
    };
    let uid = current_uid();
    for entry in entries.flatten() {
        let named = entry
            .file_name()
            .to_str()
            .is_some_and(|name| name.starts_with(STAGING_PREFIX));
        if !named {
            continue;
        }
        let path = entry.path();
        let Ok(meta) = fs::symlink_metadata(&path) else {
            continue;
        };
        if !meta.file_type().is_dir() || meta.uid() != uid {
            continue;
        }
        let stale = meta
            .modified()
            .ok()
            .and_then(|modified| modified.elapsed().ok())
            .is_some_and(|age| age > STALE_AFTER);
        if !stale {
            continue;
        }
        let Some(files) = stale_contents(&path, expected, uid) else {
            continue;
        };
        for file in &files {
            let _ = fs::remove_file(path.join(file));
        }
        let _ = fs::remove_dir(&path);
    }
}

/// The files in a stale staging directory, if it holds nothing else.
fn stale_contents(dir: &Path, expected: &BTreeSet<OsString>, uid: u32) -> Option<Vec<OsString>> {
    let mut files = Vec::new();
    for entry in fs::read_dir(dir).ok()? {
        let entry = entry.ok()?;
        let name = entry.file_name();
        if !expected.contains(&name) {
            return None;
        }
        let meta = fs::symlink_metadata(entry.path()).ok()?;
        if !meta.file_type().is_file()
            || meta.uid() != uid
            || meta.nlink() != 1
            || meta.len() > MAX_TOOL
        {
            return None;
        }
        files.push(name);
    }
    Some(files)
}

fn current_uid() -> u32 {
    extern "C" {
        fn geteuid() -> u32;
    }
    // SAFETY: geteuid takes nothing and cannot fail.
    unsafe { geteuid() }
}

fn c_path(path: &Path) -> io::Result<CString> {
    CString::new(path.as_os_str().as_bytes())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "a path with a NUL in it"))
}

/// Rename `from` to `to`, failing rather than replacing anything at `to`.
#[cfg(target_os = "macos")]
fn rename_exclusive(from: &Path, to: &Path) -> io::Result<()> {
    use std::ffi::{c_char, c_int, c_uint};
    extern "C" {
        fn renamex_np(from: *const c_char, to: *const c_char, flags: c_uint) -> c_int;
    }
    const RENAME_EXCL: c_uint = 0x0000_0004;
    let (from, to) = (c_path(from)?, c_path(to)?);
    // SAFETY: both are NUL-terminated and outlive the call.
    if unsafe { renamex_np(from.as_ptr(), to.as_ptr(), RENAME_EXCL) } == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

/// Rename `from` to `to`, failing rather than replacing anything at `to`.
#[cfg(target_os = "linux")]
fn rename_exclusive(from: &Path, to: &Path) -> io::Result<()> {
    use std::ffi::{c_char, c_int, c_uint};
    extern "C" {
        fn renameat2(
            olddirfd: c_int,
            oldpath: *const c_char,
            newdirfd: c_int,
            newpath: *const c_char,
            flags: c_uint,
        ) -> c_int;
    }
    const AT_FDCWD: c_int = -100;
    const RENAME_NOREPLACE: c_uint = 1;
    let (from, to) = (c_path(from)?, c_path(to)?);
    // SAFETY: both are NUL-terminated and outlive the call.
    if unsafe {
        renameat2(
            AT_FDCWD,
            from.as_ptr(),
            AT_FDCWD,
            to.as_ptr(),
            RENAME_NOREPLACE,
        )
    } == 0
    {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

/// Nowhere else is an exclusive rename known to exist, so nothing is seeded.
#[cfg(not(any(target_os = "macos", target_os = "linux")))]
fn rename_exclusive(_from: &Path, _to: &Path) -> io::Result<()> {
    Err(io::Error::from(io::ErrorKind::Unsupported))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Barrier};

    const WORDCOUNT: &[u8] = b"(module ;; wordcount\n)\n";
    const OTHER: &[u8] = b"\0asm\x01\0\0\0";

    /// A tools directory to seed from: two tools, and things that are not.
    fn source(root: &Path) -> PathBuf {
        let dir = root.join("installed/tools");
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("wordcount.wat"), WORDCOUNT).unwrap();
        fs::write(dir.join("other.wasm"), OTHER).unwrap();
        fs::write(dir.join("notes.txt"), b"not a tool").unwrap();
        fs::write(dir.join(".hidden.wat"), b"hidden").unwrap();
        let elsewhere = root.join("elsewhere.wat");
        fs::write(&elsewhere, b"through a link").unwrap();
        std::os::unix::fs::symlink(&elsewhere, dir.join("linked.wat")).unwrap();
        fs::write(dir.join("huge.wat"), vec![b'x'; MAX_TOOL as usize + 1]).unwrap();
        dir
    }

    fn home(root: &Path) -> PathBuf {
        let home = root.join("home with a space");
        fs::create_dir_all(&home).unwrap();
        home
    }

    fn mode(path: &Path) -> u32 {
        fs::symlink_metadata(path).unwrap().permissions().mode() & 0o7777
    }

    fn staging_left(home: &Path) -> Vec<String> {
        fs::read_dir(home)
            .unwrap()
            .flatten()
            .map(|entry| entry.file_name().to_string_lossy().into_owned())
            .filter(|name| name.starts_with(STAGING_PREFIX))
            .collect()
    }

    fn assert_seeded(home: &Path) {
        let tools = home.join("tools");
        assert_eq!(mode(&tools), 0o755);
        let mut names: Vec<String> = fs::read_dir(&tools)
            .unwrap()
            .flatten()
            .map(|entry| entry.file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        assert_eq!(names, ["other.wasm", "wordcount.wat"]);
        assert_eq!(fs::read(tools.join("wordcount.wat")).unwrap(), WORDCOUNT);
        assert_eq!(fs::read(tools.join("other.wasm")).unwrap(), OTHER);
        assert_eq!(mode(&tools.join("wordcount.wat")), 0o644);
        assert!(fs::symlink_metadata(tools.join("wordcount.wat"))
            .unwrap()
            .file_type()
            .is_file());
    }

    fn age(path: &Path, by: Duration) {
        fs::File::open(path)
            .unwrap()
            .set_modified(SystemTime::now() - by)
            .unwrap();
    }

    #[test]
    fn a_home_without_tools_gets_the_regular_tool_files_and_nothing_else() {
        let root = tempfile::tempdir().unwrap();
        let home = home(root.path());
        let outcome = seed_from(&source(root.path()), &home).unwrap();
        assert_eq!(outcome, Seeded::Seeded(2));
        assert_seeded(&home);
        assert!(staging_left(&home).is_empty());
    }

    #[test]
    fn an_existing_empty_directory_is_left_empty() {
        let root = tempfile::tempdir().unwrap();
        let home = home(root.path());
        fs::create_dir(home.join("tools")).unwrap();
        assert_eq!(
            seed_from(&source(root.path()), &home).unwrap(),
            Seeded::Present
        );
        assert_eq!(fs::read_dir(home.join("tools")).unwrap().count(), 0);
    }

    #[test]
    fn an_edited_tool_is_never_overwritten() {
        let root = tempfile::tempdir().unwrap();
        let home = home(root.path());
        fs::create_dir(home.join("tools")).unwrap();
        fs::write(home.join("tools/wordcount.wat"), b"mine").unwrap();
        assert_eq!(
            seed_from(&source(root.path()), &home).unwrap(),
            Seeded::Present
        );
        assert_eq!(fs::read(home.join("tools/wordcount.wat")).unwrap(), b"mine");
    }

    #[test]
    fn a_symlinked_destination_is_not_followed() {
        let root = tempfile::tempdir().unwrap();
        let home = home(root.path());
        let source = source(root.path());
        let target = root.path().join("somewhere else");
        fs::create_dir(&target).unwrap();
        std::os::unix::fs::symlink(&target, home.join("tools")).unwrap();
        assert_eq!(seed_from(&source, &home).unwrap(), Seeded::Present);
        assert_eq!(fs::read_dir(&target).unwrap().count(), 0);

        // A dangling one too: nothing is created where it points.
        let home2 = root.path().join("second home");
        fs::create_dir(&home2).unwrap();
        let nowhere = root.path().join("nowhere");
        std::os::unix::fs::symlink(&nowhere, home2.join("tools")).unwrap();
        assert_eq!(seed_from(&source, &home2).unwrap(), Seeded::Present);
        assert!(fs::symlink_metadata(&nowhere).is_err());
    }

    #[test]
    fn a_file_named_tools_is_left_alone() {
        let root = tempfile::tempdir().unwrap();
        let home = home(root.path());
        fs::write(home.join("tools"), b"a file").unwrap();
        assert_eq!(
            seed_from(&source(root.path()), &home).unwrap(),
            Seeded::Present
        );
        assert_eq!(fs::read(home.join("tools")).unwrap(), b"a file");
    }

    #[test]
    fn no_regular_tool_files_beside_the_binary_means_nothing_to_seed() {
        let root = tempfile::tempdir().unwrap();
        let home = home(root.path());
        assert_eq!(
            seed_from(&root.path().join("absent"), &home).unwrap(),
            Seeded::NoSource
        );
        let only_links = root.path().join("links");
        fs::create_dir(&only_links).unwrap();
        let real = root.path().join("real.wat");
        fs::write(&real, WORDCOUNT).unwrap();
        std::os::unix::fs::symlink(&real, only_links.join("wordcount.wat")).unwrap();
        fs::create_dir(only_links.join("directory.wat")).unwrap();
        assert_eq!(seed_from(&only_links, &home).unwrap(), Seeded::NoSource);
        assert!(fs::symlink_metadata(home.join("tools")).is_err());
    }

    #[test]
    fn a_directory_that_appears_just_before_the_rename_is_never_replaced() {
        let root = tempfile::tempdir().unwrap();
        let home = home(root.path());
        let tools = home.join("tools");
        let outcome = seed_with(&source(root.path()), &home, || {
            fs::create_dir(&tools).unwrap();
        })
        .unwrap();
        assert_eq!(outcome, Seeded::Lost);
        assert_eq!(fs::read_dir(&tools).unwrap().count(), 0);
        assert!(staging_left(&home).is_empty());
    }

    #[test]
    fn a_seed_interrupted_before_the_rename_leaves_no_partial_tools() {
        let root = tempfile::tempdir().unwrap();
        let home = home(root.path());
        let source = source(root.path());
        // A crash between staging and publishing: the run unwinds without
        // cleaning up, as a killed process would leave it.
        let crashed = std::panic::catch_unwind(|| {
            let _ = seed_with(&source, &home, || panic!("crash"));
        });
        assert!(crashed.is_err());
        assert!(fs::symlink_metadata(home.join("tools")).is_err());
        assert_eq!(staging_left(&home).len(), 1);

        // The next run still sees no tools, and seeds.
        assert_eq!(seed_from(&source, &home).unwrap(), Seeded::Seeded(2));
        assert_seeded(&home);
    }

    #[test]
    fn a_stale_staging_directory_holding_only_seed_files_is_removed() {
        let root = tempfile::tempdir().unwrap();
        let home = home(root.path());
        let stale = home.join(format!("{STAGING_PREFIX}1.0000000000000001"));
        fs::create_dir(&stale).unwrap();
        fs::write(stale.join("wordcount.wat"), b"half").unwrap();
        age(&stale, STALE_AFTER * 2);
        assert_eq!(
            seed_from(&source(root.path()), &home).unwrap(),
            Seeded::Seeded(2)
        );
        assert!(fs::symlink_metadata(&stale).is_err());
        assert!(staging_left(&home).is_empty());
    }

    #[test]
    fn a_staging_directory_is_kept_unless_it_is_plainly_a_stale_seed() {
        let root = tempfile::tempdir().unwrap();
        let home = home(root.path());

        // Recent: perhaps another first run is still writing it.
        let recent = home.join(format!("{STAGING_PREFIX}2.0000000000000002"));
        fs::create_dir(&recent).unwrap();
        fs::write(recent.join("wordcount.wat"), b"in progress").unwrap();

        // Old, but holding something a seed never writes.
        let foreign = home.join(format!("{STAGING_PREFIX}3.0000000000000003"));
        fs::create_dir(&foreign).unwrap();
        fs::write(foreign.join("notes.txt"), b"not ours").unwrap();
        age(&foreign, STALE_AFTER * 2);

        // Old, but holding a symlink under a seed's name.
        let linked = home.join(format!("{STAGING_PREFIX}4.0000000000000004"));
        fs::create_dir(&linked).unwrap();
        let target = root.path().join("target.wat");
        fs::write(&target, b"target").unwrap();
        std::os::unix::fs::symlink(&target, linked.join("wordcount.wat")).unwrap();
        age(&linked, STALE_AFTER * 2);

        // A symlink with a staging name, pointing at a directory elsewhere.
        let elsewhere = root.path().join("elsewhere dir");
        fs::create_dir(&elsewhere).unwrap();
        fs::write(elsewhere.join("wordcount.wat"), b"keep").unwrap();
        let pointer = home.join(format!("{STAGING_PREFIX}5.0000000000000005"));
        std::os::unix::fs::symlink(&elsewhere, &pointer).unwrap();

        assert_eq!(
            seed_from(&source(root.path()), &home).unwrap(),
            Seeded::Seeded(2)
        );
        assert!(recent.join("wordcount.wat").exists());
        assert!(foreign.join("notes.txt").exists());
        assert!(fs::symlink_metadata(linked.join("wordcount.wat")).is_ok());
        assert_eq!(fs::read(&target).unwrap(), b"target");
        assert!(fs::symlink_metadata(&pointer).is_ok());
        assert_eq!(fs::read(elsewhere.join("wordcount.wat")).unwrap(), b"keep");
    }

    #[test]
    fn sixteen_first_runs_at_once_leave_exactly_one_tools_directory() {
        let root = tempfile::tempdir().unwrap();
        let home = Arc::new(home(root.path()));
        let source = Arc::new(source(root.path()));
        let barrier = Arc::new(Barrier::new(16));
        let runs: Vec<_> = (0..16)
            .map(|_| {
                let (home, source, barrier) = (home.clone(), source.clone(), barrier.clone());
                std::thread::spawn(move || {
                    barrier.wait();
                    seed_from(&source, &home).unwrap()
                })
            })
            .collect();
        let outcomes: Vec<Seeded> = runs.into_iter().map(|run| run.join().unwrap()).collect();
        assert_eq!(
            outcomes
                .iter()
                .filter(|outcome| **outcome == Seeded::Seeded(2))
                .count(),
            1,
            "{outcomes:?}"
        );
        assert!(outcomes
            .iter()
            .all(|outcome| matches!(outcome, Seeded::Seeded(2) | Seeded::Present | Seeded::Lost)));
        assert_seeded(&home);
        assert!(staging_left(&home).is_empty());
    }

    /// Set on the copies that [`sixteen_processes_at_once_leave_exactly_one_tools_directory`]
    /// starts: the home, the source, and a file to wait for.
    const CHILD_HOME: &str = "SYNDEO_SEED_TEST_HOME";
    const CHILD_SOURCE: &str = "SYNDEO_SEED_TEST_SOURCE";
    const CHILD_GO: &str = "SYNDEO_SEED_TEST_GO";

    /// Not a test of its own: the body each of those copies runs.
    #[test]
    fn process_child() {
        let (Some(home), Some(source), Some(go)) = (
            std::env::var_os(CHILD_HOME),
            std::env::var_os(CHILD_SOURCE),
            std::env::var_os(CHILD_GO),
        ) else {
            return;
        };
        let (home, source, go) = (
            PathBuf::from(home),
            PathBuf::from(source),
            PathBuf::from(go),
        );
        let deadline = std::time::Instant::now() + Duration::from_secs(60);
        while !go.exists() {
            assert!(std::time::Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(1));
        }
        println!("@@outcome {:?}", seed_from(&source, &home).unwrap());
    }

    #[test]
    fn sixteen_processes_at_once_leave_exactly_one_tools_directory() {
        let root = tempfile::tempdir().unwrap();
        let home = home(root.path());
        let source = source(root.path());
        let go = root.path().join("go");
        let children: Vec<_> = (0..16)
            .map(|_| {
                std::process::Command::new(std::env::current_exe().unwrap())
                    .args([
                        "--exact",
                        "tools::tests::process_child",
                        "--nocapture",
                        "--test-threads",
                        "1",
                    ])
                    .env(CHILD_HOME, &home)
                    .env(CHILD_SOURCE, &source)
                    .env(CHILD_GO, &go)
                    .stdout(std::process::Stdio::piped())
                    .spawn()
                    .unwrap()
            })
            .collect();
        fs::write(&go, b"").unwrap();
        let outcomes: Vec<String> = children
            .into_iter()
            .map(|child| {
                let output = child.wait_with_output().unwrap();
                assert!(output.status.success());
                let text = String::from_utf8(output.stdout).unwrap();
                text.split_once("@@outcome ")
                    .map(|(_, rest)| rest.lines().next().unwrap().to_string())
                    .unwrap_or_else(|| panic!("no outcome in {text}"))
            })
            .collect();
        assert_eq!(
            outcomes.iter().filter(|o| *o == "Seeded(2)").count(),
            1,
            "{outcomes:?}"
        );
        assert!(outcomes
            .iter()
            .all(|o| o == "Seeded(2)" || o == "Present" || o == "Lost"));
        assert_seeded(&home);
        assert!(staging_left(&home).is_empty());
    }

    #[test]
    fn the_exclusive_rename_refuses_an_existing_empty_directory() {
        let root = tempfile::tempdir().unwrap();
        let from = root.path().join("from");
        let to = root.path().join("to");
        fs::create_dir(&from).unwrap();
        fs::create_dir(&to).unwrap();
        let err = rename_exclusive(&from, &to).unwrap_err();
        assert!(
            matches!(err.raw_os_error(), Some(code) if code == sys::EEXIST || code == sys::ENOTEMPTY),
            "{err}"
        );
        assert!(from.exists() && to.exists());
    }
}
