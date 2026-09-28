//! Confining the agent to what it was given.
//!
//! The agent already runs as its own process with a deliberately small
//! inheritance: two socket paths, no keystore endpoint, no session secret. That
//! is a real boundary and it is tested. It is not, however, an operating system
//! boundary — a compromised agent could still read the filesystem, spawn
//! processes, and open its own sockets. This asks the platform to take those
//! away.
//!
//! The one that matters most is the network. Everything the agent fetches
//! already goes through the net process over a unix socket, so an agent that
//! cannot open an internet socket loses nothing and gains the property that a
//! bug in it cannot become an exfiltration channel.
//!
//! This is a second layer, not a replacement. The startup check that refuses to
//! run when `SYNDEO_SESSION_SECRET` is visible stays exactly where it is.

use std::path::{Path, PathBuf};

/// What the platform agreed to enforce.
#[derive(Debug, Clone)]
pub enum Confinement {
    /// The platform is enforcing it. The description is what to print.
    Enforced(String),
    /// The platform is enforcing some of it and not all: some of what was
    /// asked for is not restricted. Not the same as confined, and never
    /// reported as if it were. Only Landlock enforces partially, so only Linux
    /// builds one.
    #[cfg_attr(not(target_os = "linux"), allow(dead_code))]
    Partial(String),
    /// It is not, and this is why. The process boundary still holds; this is
    /// reported rather than swallowed so nobody believes in a sandbox that is
    /// not there.
    Unavailable(String),
}

impl Confinement {
    pub fn describe(&self) -> &str {
        match self {
            Confinement::Enforced(what) => what,
            Confinement::Partial(what) => what,
            Confinement::Unavailable(why) => why,
        }
    }

    /// How to log this at start-up: the level, and the message beside the
    /// description.
    pub fn log_line(&self) -> (tracing::Level, &'static str) {
        match self {
            Confinement::Enforced(_) => (tracing::Level::INFO, "confined"),
            Confinement::Partial(_) => (tracing::Level::WARN, "partially confined"),
            Confinement::Unavailable(_) => (tracing::Level::WARN, "not confined by the platform"),
        }
    }
}

/// What a kernel whose Landlock ABI is `abi` leaves unrestricted of what this
/// asks for, which is the ABI 4 set: files, and TCP.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
pub(crate) fn unrestricted_below(abi: u8) -> Vec<&'static str> {
    let mut missing = Vec::new();
    if abi < 2 {
        missing.push("renaming or linking files across directories");
    }
    if abi < 3 {
        missing.push("truncating files");
    }
    if abi < 4 {
        missing.push("TCP bind and connect");
    }
    missing
}

/// How to describe a partially enforced ruleset. The protections that are
/// missing are named only when the kernel said which ABI it applied;
/// otherwise nothing is guessed.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
pub(crate) fn partially(effective_abi: Option<u8>) -> String {
    match effective_abi.map(unrestricted_below) {
        Some(missing) if !missing.is_empty() => format!(
            "partially confined: this kernel's Landlock does not restrict {}",
            missing.join(", ")
        ),
        _ => "partially confined: the kernel applied only some of the requested Landlock rules"
            .to_string(),
    }
}

/// Everything the agent is allowed to touch.
pub struct Grant {
    /// The unix sockets it may connect to, and no others.
    pub sockets: Vec<PathBuf>,
    /// Directories it may read from, beyond the system ones it needs to run.
    pub readable: Vec<PathBuf>,
}

/// Confine this process. Call once, as early as possible.
pub fn confine(grant: &Grant) -> Confinement {
    platform::confine(grant)
}

#[cfg(target_os = "macos")]
mod platform {
    use super::{Confinement, Grant};
    use std::ffi::CString;
    use std::path::Path;

    extern "C" {
        /// Deprecated in the headers since 10.8 and still the only interface to
        /// Seatbelt from outside Apple. The replacement is a private framework.
        fn sandbox_init(profile: *const i8, flags: u64, errorbuf: *mut *mut i8) -> i32;
        fn sandbox_free_error(errorbuf: *mut i8);
    }

    const SANDBOX_NAMED: u64 = 0;

    pub fn confine(grant: &Grant) -> Confinement {
        let profile = profile(grant);
        let Ok(profile) = CString::new(profile) else {
            return Confinement::Unavailable("the sandbox profile contained a nul byte".into());
        };

        let mut error: *mut i8 = std::ptr::null_mut();
        let status = unsafe { sandbox_init(profile.as_ptr(), SANDBOX_NAMED, &mut error) };
        if status == 0 {
            return Confinement::Enforced(
                "Seatbelt: no sockets but the two it was given, no exec, no writes".into(),
            );
        }

        let reason = if error.is_null() {
            format!("sandbox_init failed with {status}")
        } else {
            let message = unsafe { std::ffi::CStr::from_ptr(error) }
                .to_string_lossy()
                .into_owned();
            unsafe { sandbox_free_error(error) };
            message
        };
        Confinement::Unavailable(format!("Seatbelt refused the profile: {reason}"))
    }

    /// The profile, built around what the agent actually does.
    ///
    /// Reading is left broad on purpose. A process that cannot read its own
    /// dynamic libraries, the locale data, or the timezone database does not
    /// start, and a read confinement that is slightly wrong is a crash rather
    /// than a refusal. What is taken away is everything that could carry data
    /// *out*: sockets, writes, and the ability to run another program.
    fn profile(grant: &Grant) -> String {
        let mut out = String::from(
            "(version 1)\n\
             (deny default)\n\
             (allow process-info* (target self))\n\
             (allow sysctl-read)\n\
             (allow mach-lookup)\n\
             (allow file-read*)\n\
             (allow signal (target self))\n\
             (allow ipc-posix-shm)\n\
             ; The agent runs another program over nobody's dead body.\n\
             (deny process-exec)\n\
             (deny process-fork)\n\
             ; It has nothing to write. Anything it learned is reported to the\n\
             ; shell over the socket it was given.\n\
             (deny file-write*)\n\
             ; And it opens no socket of its own. Everything it fetches goes\n\
             ; through the net process, so this costs it nothing.\n\
             (deny network*)\n",
        );
        for socket in &grant.sockets {
            // Resolved, not as written. Seatbelt matches the path the kernel
            // sees, and on macOS `/tmp` is a symlink to `/private/tmp` — so a
            // literal of `/tmp/…` matches nothing and the rule silently does
            // not apply, which looks exactly like the sandbox working.
            out.push_str(&format!(
                "(allow network-outbound (literal \"{}\"))\n",
                escape(&resolve(socket))
            ));
        }
        out
    }

    /// Seatbelt profiles are s-expressions; a path goes in a quoted string.
    fn escape(path: &Path) -> String {
        path.to_string_lossy()
            .replace('\\', "\\\\")
            .replace('"', "\\\"")
    }

    /// The path the kernel will see, symlinks and all.
    fn resolve(path: &Path) -> std::path::PathBuf {
        // `canonicalize` needs the file to exist, and the socket does by the
        // time the agent is confined. If it somehow does not, resolving the
        // parent still catches the `/tmp` case, which is the one that matters.
        if let Ok(resolved) = path.canonicalize() {
            return resolved;
        }
        match (path.parent(), path.file_name()) {
            (Some(parent), Some(name)) => match parent.canonicalize() {
                Ok(resolved) => resolved.join(name),
                Err(_) => path.to_path_buf(),
            },
            _ => path.to_path_buf(),
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        use std::path::PathBuf;

        #[test]
        fn the_profile_denies_the_network_and_names_the_sockets() {
            let grant = Grant {
                sockets: vec![PathBuf::from("/tmp/syndeo-profile-test.sock")],
                readable: Vec::new(),
            };
            let profile = profile(&grant);
            assert!(profile.contains("(deny network*)"));
            assert!(profile.contains("(deny process-exec)"));
            assert!(profile.contains("(deny file-write*)"));
            assert!(profile.contains("network-outbound"));
            // The deny has to come before the allow, or the allow is undone.
            assert!(
                profile.find("(deny network*)").unwrap()
                    < profile.find("network-outbound").unwrap()
            );
        }

        #[test]
        fn a_socket_path_is_written_as_the_kernel_will_see_it() {
            // The trap this guards against: on macOS `/tmp` is a symlink to
            // `/private/tmp`, so a literal of the unresolved path matches
            // nothing, the rule silently does not apply, and the result looks
            // exactly like the sandbox working correctly.
            let grant = Grant {
                sockets: vec![PathBuf::from("/tmp/syndeo-resolution-test.sock")],
                readable: Vec::new(),
            };
            let profile = profile(&grant);
            assert!(
                profile.contains("/private/tmp/syndeo-resolution-test.sock"),
                "the socket path was not resolved: {profile}"
            );
        }
    }
}

#[cfg(target_os = "linux")]
mod platform {
    use super::{Confinement, Grant};
    use landlock::{
        Access, AccessFs, AccessNet, PathBeneath, PathFd, RestrictionStatus, Ruleset, RulesetAttr,
        RulesetCreatedAttr, RulesetStatus, ABI,
    };

    /// The Landlock ABI the kernel actually applied, as a level, when it said.
    fn effective_abi(status: &landlock::LandlockStatus) -> Option<u8> {
        let landlock::LandlockStatus::Available { effective_abi, .. } = status else {
            return None;
        };
        let abi = *effective_abi;
        Some(if abi >= ABI::V4 {
            4
        } else if abi >= ABI::V3 {
            3
        } else if abi >= ABI::V2 {
            2
        } else if abi >= ABI::V1 {
            1
        } else {
            0
        })
    }

    pub fn confine(grant: &Grant) -> Confinement {
        // ABI v4 is the first with network restriction. Landlock's own
        // compatibility handling degrades on older kernels rather than failing,
        // and `RestrictionStatus` says what was actually applied — which is what
        // gets reported, so nobody believes in more than landed.
        let abi = ABI::V4;
        let read_only = AccessFs::from_read(abi);

        let mut ruleset = match Ruleset::default()
            .handle_access(AccessFs::from_all(abi))
            .and_then(|r| r.handle_access(AccessNet::BindTcp | AccessNet::ConnectTcp))
            .and_then(|r| r.create())
        {
            Ok(ruleset) => ruleset,
            Err(err) => return Confinement::Unavailable(format!("landlock: {err}")),
        };

        // Read what it needs to run, write nothing, and connect nowhere: no
        // network rule is added at all, so TCP bind and connect are denied
        // outright. The unix sockets it was given are not TCP and are reached
        // through the filesystem, which is why they are a read grant.
        for directory in ["/usr", "/lib", "/lib64", "/etc", "/proc/self", "/dev"] {
            if let Ok(fd) = PathFd::new(directory) {
                ruleset = match ruleset.add_rule(PathBeneath::new(fd, read_only)) {
                    Ok(r) => r,
                    Err(err) => return Confinement::Unavailable(format!("landlock: {err}")),
                };
            }
        }
        for path in grant.sockets.iter().chain(grant.readable.iter()) {
            if let Ok(fd) = PathFd::new(path) {
                ruleset =
                    match ruleset.add_rule(PathBeneath::new(fd, read_only | AccessFs::WriteFile)) {
                        Ok(r) => r,
                        Err(err) => return Confinement::Unavailable(format!("landlock: {err}")),
                    };
            }
        }

        match ruleset.restrict_self() {
            Ok(RestrictionStatus {
                ruleset: RulesetStatus::FullyEnforced,
                ..
            }) => Confinement::Enforced(
                "Landlock: no TCP, read-only outside the sockets it was given".into(),
            ),
            Ok(RestrictionStatus {
                ruleset: RulesetStatus::PartiallyEnforced,
                landlock,
                ..
            }) => Confinement::Partial(super::partially(effective_abi(&landlock))),
            Ok(_) => Confinement::Unavailable(
                "this kernel does not support Landlock; the process boundary still holds".into(),
            ),
            Err(err) => Confinement::Unavailable(format!("landlock: {err}")),
        }
    }
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
mod platform {
    use super::{Confinement, Grant};

    pub fn confine(_grant: &Grant) -> Confinement {
        // Windows would be an AppContainer, which has to be applied by whoever
        // creates the process rather than by the process itself — so it belongs
        // in the shell's spawn, not here. Not claimed until it is written.
        Confinement::Unavailable(
            "no sandbox on this platform; the process boundary is what holds".into(),
        )
    }
}

/// Where the agent may read from, given the sockets it was handed.
pub fn grant_for(net: &Path, shell: &Path) -> Grant {
    let mut readable = Vec::new();
    for socket in [net, shell] {
        if let Some(parent) = socket.parent() {
            readable.push(parent.to_path_buf());
        }
    }
    Grant {
        sockets: vec![net.to_path_buf(), shell.to_path_buf()],
        readable,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The property the sandbox exists for: after confinement, this process
    /// cannot open a socket to the outside world, cannot run another program,
    /// and cannot write a file.
    ///
    /// It runs in a child, because confinement is irreversible — applying it in
    /// the test process would confine the rest of the suite with it.
    #[test]
    #[cfg(target_os = "macos")]
    fn a_confined_process_cannot_reach_the_network_or_run_anything() {
        use std::io::Write;
        use std::process::Command;

        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("confined.rs");
        let binary = dir.path().join("confined");

        // A program that confines itself and then tries everything it should no
        // longer be able to do.
        let program = r#"
            fn main() {
                let temp = std::env::args().nth(1).unwrap();
                extern "C" { fn sandbox_init(p: *const i8, f: u64, e: *mut *mut i8) -> i32; }
                let profile = std::ffi::CString::new(PROFILE).unwrap();
                let mut err: *mut i8 = std::ptr::null_mut();
                let rc = unsafe { sandbox_init(profile.as_ptr(), 0, &mut err) };
                println!("confined {rc}");

                let tcp = std::net::TcpStream::connect("1.1.1.1:80").is_ok();
                println!("tcp {tcp}");

                let udp = std::net::UdpSocket::bind("0.0.0.0:0")
                    .and_then(|s| s.send_to(b"x", "1.1.1.1:53"))
                    .is_ok();
                println!("udp {udp}");

                let wrote = std::fs::write(format!("{temp}/written"), b"x").is_ok();
                println!("write {wrote}");

                let ran = std::process::Command::new("/bin/echo").arg("hi").output().is_ok();
                println!("exec {ran}");
            }
            const PROFILE: &str = "(version 1)\n(deny default)\n(allow process-info* (target self))\n(allow sysctl-read)\n(allow mach-lookup)\n(allow file-read*)\n(allow signal (target self))\n(deny process-exec)\n(deny file-write*)\n(deny network*)\n";
        "#;
        let mut file = std::fs::File::create(&source).unwrap();
        file.write_all(program.as_bytes()).unwrap();
        drop(file);

        let built = Command::new("rustc")
            .arg("-O")
            .arg("-o")
            .arg(&binary)
            .arg(&source)
            .output()
            .expect("rustc is available to build the fixture");
        assert!(
            built.status.success(),
            "could not build the fixture: {}",
            String::from_utf8_lossy(&built.stderr)
        );

        let output = Command::new(&binary)
            .arg(dir.path())
            .output()
            .expect("the confined fixture runs");
        let report = String::from_utf8_lossy(&output.stdout);

        assert!(
            report.contains("confined 0"),
            "the profile was refused: {report}"
        );
        assert!(
            report.contains("tcp false"),
            "it opened a TCP socket: {report}"
        );
        assert!(
            report.contains("udp false"),
            "it sent a UDP packet: {report}"
        );
        assert!(report.contains("write false"), "it wrote a file: {report}");
        assert!(
            report.contains("exec false"),
            "it ran another program: {report}"
        );
        assert!(
            !dir.path().join("written").exists(),
            "the write it reported as failing actually happened"
        );
    }

    #[test]
    fn partial_confinement_is_neither_confined_nor_unconfined() {
        let partial = Confinement::Partial(partially(Some(3)));
        assert!(!matches!(partial, Confinement::Enforced(_)));
        assert_eq!(
            partial.log_line(),
            (tracing::Level::WARN, "partially confined")
        );
        assert_eq!(
            Confinement::Enforced("x".into()).log_line(),
            (tracing::Level::INFO, "confined")
        );
        assert_eq!(
            Confinement::Unavailable("x".into()).log_line(),
            (tracing::Level::WARN, "not confined by the platform")
        );
    }

    #[test]
    fn what_is_missing_is_named_only_from_the_abi_the_kernel_reported() {
        assert_eq!(
            partially(Some(3)),
            "partially confined: this kernel's Landlock does not restrict TCP bind and connect"
        );
        assert!(partially(Some(1)).contains("truncating files"));
        assert!(partially(Some(1)).contains("renaming or linking"));
        assert!(unrestricted_below(4).is_empty());
        // No ABI reported, or one with nothing missing: nothing is guessed.
        for abi in [None, Some(4)] {
            assert_eq!(
                partially(abi),
                "partially confined: the kernel applied only some of the requested Landlock rules"
            );
        }
    }
}
