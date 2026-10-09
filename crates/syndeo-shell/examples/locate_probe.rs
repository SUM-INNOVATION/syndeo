//! Diagnostic only, never shipped: a stand-in `syndeo` for the package-upgrade
//! diagnostic (ci/diagnose-pkg-upgrade.sh). It reports where the shell's own
//! sibling lookup, `Supervisor::locate`, leads.
//!
//!   syndeo --version    "syndeo <the running image's directory name>"
//!   syndeo locate       one lookup of every sibling, now
//!   syndeo hold DIR     find the running image's directory at once, as a
//!                       shell does at its first lookup; then, each time
//!                       DIR/ask holds a label, look up every sibling and
//!                       write DIR/answer.<label>; until DIR/stop appears

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use syndeo_shell::supervisor::{install_dir, running_image, Supervisor};

const SIBLINGS: [&str; 6] = [
    "syndeo-net",
    "syndeo-keystore",
    "syndeo-agent",
    "syndeo-proxy",
    "syndeo-ui",
    "syndeo-webkit",
];

fn name_of(dir: Option<&Path>) -> String {
    dir.and_then(Path::file_name)
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "none".into())
}

fn shown(path: Option<PathBuf>) -> String {
    path.map(|p| p.display().to_string())
        .unwrap_or_else(|| "none".into())
}

fn report() -> String {
    let own = install_dir();
    let exe = std::env::current_exe().ok();
    let canonical = exe.as_ref().and_then(|e| std::fs::canonicalize(e).ok());
    let mut out = format!(
        "pid {}\ninstall_dir {} (version {}, exists {})\nrunning_image_now {}\ncurrent_exe {}\ncurrent_exe_resolved {}\n",
        std::process::id(),
        shown(own.map(Path::to_path_buf)),
        name_of(own),
        own.is_some_and(Path::exists),
        shown(running_image()),
        shown(exe),
        shown(canonical),
    );
    for name in SIBLINGS {
        let line = match Supervisor::locate(name) {
            Ok(found) => {
                let resolved = std::fs::canonicalize(&found).ok();
                let version = name_of(resolved.as_deref().and_then(Path::parent));
                let verdict = if version == name_of(own) {
                    "own-version"
                } else {
                    "OTHER-VERSION"
                };
                format!(
                    "sibling {name} found {} -> {} (version {version}) {verdict}\n",
                    found.display(),
                    shown(resolved),
                )
            }
            Err(e) => format!(
                "sibling {name} NOT-FOUND {}\n",
                format!("{e:#}").replace('\n', " ")
            ),
        };
        out.push_str(&line);
    }
    out
}

fn hold(dir: &Path) {
    // Found now and kept, as in a shell that has started its children.
    let own = install_dir();
    println!(
        "holding: pid {} version {} install_dir {}",
        std::process::id(),
        name_of(own),
        shown(own.map(Path::to_path_buf))
    );
    let deadline = Instant::now() + Duration::from_secs(40 * 60);
    while !dir.join("stop").exists() && Instant::now() < deadline {
        if let Ok(label) = std::fs::read_to_string(dir.join("ask")) {
            let label = label.trim();
            if !label.is_empty()
                && label
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '.')
            {
                let text = report();
                let partial = dir.join(format!(".answer.{label}"));
                if std::fs::write(&partial, &text).is_ok() {
                    let _ = std::fs::rename(&partial, dir.join(format!("answer.{label}")));
                }
                println!("answered {label}");
            }
            let _ = std::fs::remove_file(dir.join("ask"));
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args
        .iter()
        .map(String::as_str)
        .collect::<Vec<_>>()
        .as_slice()
    {
        ["--version"] => println!("syndeo {}", name_of(install_dir())),
        ["locate"] => print!("{}", report()),
        ["hold", dir] => hold(Path::new(dir)),
        _ => {
            eprintln!("usage: syndeo --version | locate | hold DIR");
            std::process::exit(2);
        }
    }
}
