//! The proxy a renderer owns: started from the program's own install, heard
//! from on its own stdout, given its credential down its stdin, and gone when
//! its owner goes. Shared by `syndeo-webkit` and the Syndeo app, so both start
//! and keep their proxy exactly the same way.

use crate::{pin, proxied};
use anyhow::{Context, Result};
use std::path::PathBuf;

/// Where everything lives when nobody says otherwise.
pub fn default_home() -> PathBuf {
    std::env::var_os("SYNDEO_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            std::env::var_os("HOME")
                .map(PathBuf::from)
                .unwrap_or_else(|| PathBuf::from("."))
                .join(".syndeo")
        })
}

/// The proxy this browser started: that process and no other.
///
/// Dropping it kills and reaps the child, which covers returning from `run`,
/// an error on the way out, and unwinding. Until it is reaped the child's pid
/// cannot be given to anyone else, so the kill can only ever reach our own
/// proxy. What a destructor cannot cover — a force-quit, a crash, `kill -9` —
/// the child covers itself: it holds the other end of its stdin pipe and exits
/// when the kernel closes ours.
pub struct ProxyChild(std::process::Child);

impl Drop for ProxyChild {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// Our proxy, and where it said it listens.
///
/// Built only from a child this process spawned and heard from, so the
/// address is the one that child bound — never whatever happened to answer on
/// a port.
pub struct OwnedProxy {
    child: ProxyChild,
    pub endpoint: std::net::SocketAddr,
    control: ProxyControl,
}

impl OwnedProxy {
    pub fn pid(&self) -> u32 {
        self.child.0.id()
    }

    /// The way to ask this proxy to do something.
    pub fn control(&self) -> ProxyControl {
        self.control.clone()
    }
}

/// What this program asks its proxy, down the pipe to the proxy's stdin, and
/// what the proxy answers on its stdout, a line each: see `syndeo-proxy run
/// --control-stdin`. Only this program holds that pipe, so nothing else — no
/// page, no other process — can ask. Cloned, to ask from another thread; one
/// question at a time.
#[derive(Clone)]
pub struct ProxyControl(std::sync::Arc<std::sync::Mutex<Control>>);

struct Control {
    stdin: std::process::ChildStdin,
    /// The proxy's lines after its announcement.
    answers: std::sync::mpsc::Receiver<String>,
    /// An earlier question went unanswered in time, so whatever comes next
    /// may be its answer, not the next question's.
    out_of_step: bool,
}

/// The proxy's answer to `clear-cache`, before how many entries there were.
const CLEARED: &str = "SYNDEO-PROXY-CLEARED 1 ";
/// Its answer to a command that failed.
const FAILED: &str = "SYNDEO-PROXY-FAILED 1";

impl ProxyControl {
    /// Ask the proxy to empty its cache, and wait up to `within` for it to say
    /// it has. Returns how many entries there were.
    pub fn clear_cache(&self, within: std::time::Duration) -> Result<u64> {
        use std::io::Write;
        let mut control = self
            .0
            .lock()
            .map_err(|_| anyhow::anyhow!("an earlier question to the proxy failed part way"))?;
        if control.out_of_step {
            anyhow::bail!("the proxy stopped answering; quit Syndeo and open it again");
        }
        // Nothing the proxy said before this question answers it.
        while control.answers.try_recv().is_ok() {}
        let asked = control
            .stdin
            .write_all(b"clear-cache\n")
            .and_then(|()| control.stdin.flush());
        asked.context("asking the proxy to clear its cache")?;
        match control.answers.recv_timeout(within) {
            Ok(answer) => cleared(&answer),
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                control.out_of_step = true;
                anyhow::bail!(
                    "the proxy did not say it had cleared its cache within {} seconds",
                    within.as_secs()
                )
            }
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                anyhow::bail!("the proxy has stopped")
            }
        }
    }
}

/// How many entries the proxy said it cleared, from its answer.
fn cleared(answer: &str) -> Result<u64> {
    if let Some(entries) = answer.strip_prefix(CLEARED) {
        if !entries.is_empty() && entries.bytes().all(|b| b.is_ascii_digit()) {
            return entries
                .parse()
                .with_context(|| format!("not a count: {entries:?}"));
        }
    }
    if answer == FAILED {
        anyhow::bail!("the proxy could not clear its cache");
    }
    anyhow::bail!("the proxy answered {answer:?}, not that it had cleared its cache")
}

/// What `syndeo-proxy run --announce` writes once it is listening, before the
/// address. Kept in step with the proxy's `READY`.
const READY: &str = "SYNDEO-PROXY-READY 1 ";

/// The longest announcement worth reading: the prefix, the longest IPv4
/// socket address, and room to spare. Anything longer is not one.
const MAX_ANNOUNCEMENT: usize = 96;

/// Read a proxy's announcement, strictly: the exact prefix and version, one
/// socket address and nothing after it, on loopback IPv4 — which is what was
/// asked for — and a port the system actually chose.
fn parse_announcement(line: &[u8]) -> Result<std::net::SocketAddr> {
    let text = std::str::from_utf8(line).context("the announcement is not text")?;
    let rest = text
        .strip_prefix(READY)
        .with_context(|| format!("not a readiness announcement: {text:?}"))?;
    let address: std::net::SocketAddr = rest
        .parse()
        .with_context(|| format!("not an address: {rest:?}"))?;
    if !address.is_ipv4() || !address.ip().is_loopback() {
        anyhow::bail!("the proxy announced {address}, which is not loopback IPv4");
    }
    if address.port() == 0 {
        anyhow::bail!("the proxy announced port 0");
    }
    Ok(address)
}

/// The longest answer worth reading after the announcement. A longer line is
/// none, and is read past.
const MAX_ANSWER: usize = 96;

/// On the child's stdout, on a thread of its own: the one announcement line,
/// bounded, and then everything after it read until the child goes, so a
/// child that writes more can never block on a full pipe. Each line after the
/// announcement that is short enough to be an answer is sent to `answers`.
fn read_announcement(
    mut stdout: std::process::ChildStdout,
    found: std::sync::mpsc::Sender<Result<std::net::SocketAddr>>,
    answers: std::sync::mpsc::Sender<String>,
) {
    use std::io::Read;
    let mut line = Vec::with_capacity(MAX_ANNOUNCEMENT);
    let mut byte = [0u8; 1];
    loop {
        match stdout.read(&mut byte) {
            Ok(0) => {
                let _ = found.send(Err(anyhow::anyhow!(
                    "the proxy closed its output before saying where it listens"
                )));
                return;
            }
            Ok(_) if byte[0] == b'\n' => break,
            Ok(_) => {
                line.push(byte[0]);
                if line.len() > MAX_ANNOUNCEMENT {
                    let _ = found.send(Err(anyhow::anyhow!(
                        "the proxy's first line is longer than an announcement"
                    )));
                    return;
                }
            }
            Err(err) => {
                let _ = found.send(Err(
                    anyhow::Error::from(err).context("reading the proxy's announcement")
                ));
                return;
            }
        }
    }
    let _ = found.send(parse_announcement(&line));
    let mut chunk = [0u8; 8192];
    let mut answer: Vec<u8> = Vec::with_capacity(MAX_ANSWER);
    let mut too_long = false;
    while let Ok(n) = stdout.read(&mut chunk) {
        if n == 0 {
            return;
        }
        for &byte in &chunk[..n] {
            if byte == b'\n' {
                match (too_long, String::from_utf8(std::mem::take(&mut answer))) {
                    (false, Ok(text)) => {
                        let _ = answers.send(text);
                    }
                    _ => tracing::debug!("the proxy wrote a line that is no answer"),
                }
                too_long = false;
            } else if answer.len() < MAX_ANSWER {
                answer.push(byte);
            } else {
                too_long = true;
            }
        }
    }
}

/// Start `command` as this browser's proxy and wait until it says where it
/// listens.
///
/// Readiness is the child's own word, on its own stdout, and not a connection
/// to a port: something else listening there is never mistaken for our proxy,
/// because nothing else can write to that pipe. A child that exits, or says
/// anything but a well-formed announcement, is an error, and dropping it on
/// the way out kills and reaps it.
pub fn start_owned(
    mut command: std::process::Command,
    within: std::time::Duration,
    credential_frame: Option<Vec<u8>>,
) -> Result<OwnedProxy> {
    let child = command
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .spawn()
        .context("starting the proxy")?;
    let mut owned = ProxyChild(child);
    // The credential goes down the pipe first, whole, before anything else
    // could read it; the pipe then stays open, for the child's parent watch
    // and for what this program asks it later.
    let mut stdin = owned.0.stdin.take().context("the proxy's stdin")?;
    if let Some(mut frame) = credential_frame {
        use std::io::Write;
        let written = stdin.write_all(&frame).and_then(|()| stdin.flush());
        frame.fill(0);
        written.context("handing the proxy its credential")?;
    }
    let stdout = owned.0.stdout.take().context("the proxy's stdout")?;
    let (found, announced) = std::sync::mpsc::channel();
    let (answered, answers) = std::sync::mpsc::channel();
    std::thread::Builder::new()
        .name("proxy-stdout".into())
        .spawn(move || read_announcement(stdout, found, answered))
        .context("reading the proxy's output")?;
    let control = ProxyControl(std::sync::Arc::new(std::sync::Mutex::new(Control {
        stdin,
        answers,
        out_of_step: false,
    })));

    let deadline = std::time::Instant::now() + within;
    loop {
        if let Some(status) = owned.0.try_wait()? {
            anyhow::bail!("the proxy exited before it was ready ({status})");
        }
        match announced.recv_timeout(std::time::Duration::from_millis(50)) {
            Ok(Ok(endpoint)) => {
                tracing::info!(proxy = %endpoint, "the proxy is listening");
                return Ok(OwnedProxy {
                    child: owned,
                    endpoint,
                    control,
                });
            }
            Ok(Err(err)) => {
                // A child that ended its output by exiting is reported by how
                // it exited, which is the more useful of the two.
                if let Some(status) = exited_soon(&mut owned) {
                    anyhow::bail!("the proxy exited before it was ready ({status})");
                }
                return Err(err);
            }
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                anyhow::bail!("the proxy's output could not be read")
            }
        }
        if std::time::Instant::now() >= deadline {
            anyhow::bail!(
                "the proxy did not say where it listens within {} seconds",
                within.as_secs()
            );
        }
    }
}

/// How the child exited, if it does so within a moment.
fn exited_soon(owned: &mut ProxyChild) -> Option<std::process::ExitStatus> {
    for _ in 0..20 {
        if let Ok(Some(status)) = owned.0.try_wait() {
            return Some(status);
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    None
}

/// How a browser's own proxy is run: on a loopback port the system picks,
/// announced, with its credential on stdin, gone with its parent, taking its
/// commands on that same pipe (see [`ProxyControl`]), and not tracing
/// requests. A request trace is every URL in full, paths and queries
/// included, and those can hold a search, a token or a password-reset link;
/// the proxy's other logs show only a URL's origin.
pub const PROXY_ARGS: [&str; 8] = [
    "run",
    "--exit-with-parent",
    "--listen",
    "127.0.0.1:0",
    "--announce",
    "--auth-stdin",
    "--control-stdin",
    "--trace-requests=false",
];

/// Start the proxy this browser fetches through, and wait until it says where.
///
/// A sibling binary, found beside the image this process is running, as the
/// shell finds its own (see `syndeo_shell::supervisor::InstallDir`), so a
/// build tree and an install both work and an upgrade that switches a link
/// cannot hand this browser another release's proxy. For the macOS package,
/// whose upgrade removes the version it replaces, a missing proxy is that and
/// nothing else is tried: quit and restart. It listens on a loopback
/// port the system picks — never a fixed one that something else could already
/// hold — and goes when this process goes, however that happens: see
/// [`ProxyChild`].
pub fn start_proxy(
    install: syndeo_shell::supervisor::InstallDir,
    home: &std::path::Path,
) -> Result<(OwnedProxy, proxied::ProxyCredential)> {
    let beside = install.beside("syndeo-proxy")?;

    let mut command = std::process::Command::new(beside);
    command
        .args(PROXY_ARGS)
        // On the environment rather than a flag, because that is where the
        // proxy reads it from and inventing a flag it does not have is how the
        // first attempt at this failed.
        .env("SYNDEO_HOME", home);
    let (credential, frame) = proxied::ProxyCredential::generate()?;
    let owned = start_owned(command, std::time::Duration::from_secs(10), Some(frame))?;
    Ok((owned, credential))
}

/// Whether this home's authority — this exact certificate, by its SHA-256
/// fingerprint — is in a keychain WebKit's networking process consults.
///
/// Only a hint: it asks whether the certificate is present, not whether its
/// trust settings are right. But it is the difference between a clear message
/// and a window that renders nothing for no stated reason, and asking by name
/// alone was not enough for that: a certificate left behind by an earlier
/// home, with the same name and a different key, passed, and then every https
/// page failed silently.
pub fn authority_is_trusted(authority: &std::path::Path) -> bool {
    let Ok(pem) = std::fs::read_to_string(authority) else {
        return false;
    };
    let Ok(der) = pin::der_from_pem(&pem) else {
        return false;
    };
    let Ok(listing) = std::process::Command::new("/usr/bin/security")
        .args([
            "find-certificate",
            "-a",
            "-Z",
            "-c",
            "Syndeo Local Measurement CA",
        ])
        .stderr(std::process::Stdio::null())
        .output()
    else {
        return false;
    };
    listing_includes(&String::from_utf8_lossy(&listing.stdout), &der)
}

/// Whether `security find-certificate -a -Z` output lists the certificate
/// whose DER is `der`.
fn listing_includes(listing: &str, der: &[u8]) -> bool {
    use sha2::Digest;
    let ours: String = sha2::Sha256::digest(der)
        .iter()
        .map(|b| format!("{b:02X}"))
        .collect();
    listing
        .lines()
        .filter_map(|line| line.trim().strip_prefix("SHA-256 hash:"))
        .any(|hash| hash.trim().eq_ignore_ascii_case(&ours))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;
    use std::time::{Duration, Instant};

    fn alive(pid: u32) -> bool {
        Command::new("/bin/kill")
            .args(["-0", &pid.to_string()])
            .stderr(std::process::Stdio::null())
            .status()
            .map(|s| s.success())
            .unwrap_or(false)
    }

    fn sleeper() -> ProxyChild {
        ProxyChild(Command::new("/bin/sleep").arg("60").spawn().unwrap())
    }

    fn sh(script: &str) -> Command {
        let mut command = Command::new("/bin/sh");
        command.args(["-c", script]);
        command
    }

    #[test]
    fn a_browsers_own_proxy_does_not_trace_requests() {
        assert!(PROXY_ARGS.contains(&"--trace-requests=false"));
        assert!(!PROXY_ARGS
            .iter()
            .any(|arg| *arg == "--trace-requests" || *arg == "--trace-requests=true"));
    }

    #[test]
    fn the_proxy_goes_when_its_owner_is_dropped() {
        let owned = sleeper();
        let pid = owned.0.id();
        assert!(alive(pid));
        drop(owned);
        assert!(!alive(pid));
    }

    #[test]
    fn the_proxy_goes_when_its_owner_returns_an_error() {
        let mut pid = 0;
        let result: Result<()> = (|| {
            let owned = sleeper();
            pid = owned.0.id();
            anyhow::bail!("something failed after the proxy started")
        })();
        assert!(result.is_err());
        assert!(!alive(pid));
    }

    #[test]
    fn the_proxy_goes_when_its_owner_unwinds() {
        let pid = std::sync::atomic::AtomicU32::new(0);
        let unwound = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let owned = sleeper();
            pid.store(owned.0.id(), std::sync::atomic::Ordering::SeqCst);
            panic!("something panicked after the proxy started");
        }));
        assert!(unwound.is_err());
        assert!(!alive(pid.load(std::sync::atomic::Ordering::SeqCst)));
    }

    #[test]
    fn the_authority_counts_as_present_only_by_its_own_fingerprint() {
        use sha2::Digest;
        let ours = b"our authority's DER";
        let hash: String = sha2::Sha256::digest(ours)
            .iter()
            .map(|b| format!("{b:02X}"))
            .collect();
        let record = |sha256: &str| {
            format!(
                "SHA-256 hash: {sha256}\nSHA-1 hash: {}\nattributes:\n    \
                 \"labl\"<blob>=\"Syndeo Local Measurement CA\"\n",
                "0".repeat(40)
            )
        };
        let stale = record(&"A".repeat(64));
        assert!(
            !listing_includes(&stale, ours),
            "a same-named stranger passed"
        );
        assert!(!listing_includes("", ours));
        assert!(listing_includes(&(stale.clone() + &record(&hash)), ours));
        assert!(listing_includes(&record(&hash.to_lowercase()), ours));
    }

    #[test]
    fn only_a_well_formed_loopback_announcement_is_believed() {
        assert_eq!(
            parse_announcement(b"SYNDEO-PROXY-READY 1 127.0.0.1:4567").unwrap(),
            "127.0.0.1:4567".parse().unwrap()
        );
        for refused in [
            &b""[..],
            b"SYNDEO-PROXY-READY 1 ",
            b"SYNDEO-PROXY-READY 2 127.0.0.1:4567",
            b"syndeo-proxy-ready 1 127.0.0.1:4567",
            b"SYNDEO-PROXY-READY 1 127.0.0.1:4567 ",
            b"SYNDEO-PROXY-READY 1 127.0.0.1:4567 extra",
            b"SYNDEO-PROXY-READY 1  127.0.0.1:4567",
            b"SYNDEO-PROXY-READY 1 10.0.0.1:4567",
            b"SYNDEO-PROXY-READY 1 0.0.0.0:4567",
            b"SYNDEO-PROXY-READY 1 [::1]:4567",
            b"SYNDEO-PROXY-READY 1 127.0.0.1:0",
            b"SYNDEO-PROXY-READY 1 localhost:4567",
            b"SYNDEO-PROXY-READY 1 127.0.0.1:4567\r",
            b"\xffSYNDEO-PROXY-READY 1 127.0.0.1:4567",
        ] {
            assert!(
                parse_announcement(refused).is_err(),
                "{:?}",
                String::from_utf8_lossy(refused)
            );
        }
    }

    #[test]
    fn a_proxy_that_announces_is_kept_until_its_owner_goes() {
        let owned = start_owned(
            sh("printf 'SYNDEO-PROXY-READY 1 127.0.0.1:4567\\n'; exec /bin/sleep 60"),
            Duration::from_secs(10),
            None,
        )
        .unwrap();
        assert_eq!(owned.endpoint, "127.0.0.1:4567".parse().unwrap());
        let pid = owned.pid();
        assert!(alive(pid));
        drop(owned);
        assert!(!alive(pid));
    }

    #[test]
    fn something_else_listening_is_never_taken_for_our_proxy() {
        // The race this replaces: a port probed free, something else binding
        // it, and its answer taken for our proxy's. Here something listens and
        // accepts, and our child never says anything.
        let squatter = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let started = Instant::now();
        let mut silent = Command::new("/bin/sleep");
        silent.arg("60");
        let result = start_owned(silent, Duration::from_secs(1), None);
        let message = result
            .err()
            .expect("a silent child is not ready")
            .to_string();
        assert!(
            message.contains("did not say where it listens"),
            "{message}"
        );
        assert!(started.elapsed() >= Duration::from_secs(1));
        drop(squatter);
    }

    #[test]
    fn a_proxy_that_exits_first_is_noticed_at_once_with_its_status() {
        let started = Instant::now();
        let message = start_owned(
            Command::new("/usr/bin/false"),
            Duration::from_secs(10),
            None,
        )
        .err()
        .unwrap()
        .to_string();
        assert!(message.contains("exited before it was ready"), "{message}");
        assert!(message.contains("exit status: 1"), "{message}");
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "waited out the deadline"
        );
    }

    #[test]
    fn a_proxy_from_before_announce_existed_fails_with_its_exit_status() {
        // What a 0.1.3 syndeo-proxy does with `--announce`: clap refuses the
        // argument and exits 2.
        let message = start_owned(
            sh("echo \"error: unexpected argument '--announce' found\" >&2; exit 2"),
            Duration::from_secs(10),
            None,
        )
        .err()
        .unwrap()
        .to_string();
        assert_eq!(
            message,
            "the proxy exited before it was ready (exit status: 2)"
        );
    }

    #[test]
    fn a_malformed_or_endless_first_line_is_refused_and_the_child_goes() {
        for (n, output) in [
            "printf 'SYNDEO-PROXY-READY 1 10.0.0.1:80\\n'",
            "head -c 4096 /dev/zero | tr '\\0' 'x'",
        ]
        .into_iter()
        .enumerate()
        {
            let pidfile = std::env::temp_dir()
                .join(format!("syndeo-webkit-refused-{}-{n}", std::process::id()));
            let _ = std::fs::remove_file(&pidfile);
            let script = format!(
                "echo $$ > '{}'; {output}; exec /bin/sleep 60",
                pidfile.display()
            );
            let started = Instant::now();
            let result = start_owned(sh(&script), Duration::from_secs(10), None);
            assert!(result.is_err(), "{output}");
            assert!(
                started.elapsed() < Duration::from_secs(5),
                "refused only at the deadline, not when the line went wrong: {output}"
            );
            let pid: u32 = std::fs::read_to_string(&pidfile)
                .unwrap()
                .trim()
                .parse()
                .unwrap();
            assert!(!alive(pid), "a refused proxy was left running: {output}");
            let _ = std::fs::remove_file(&pidfile);
        }
    }

    #[test]
    fn the_credential_goes_down_stdin_whole_and_the_pipe_stays_open() {
        let dir = std::env::temp_dir().join(format!(
            "syndeo-webkit-frame-{}-{}",
            std::process::id(),
            Instant::now().elapsed().as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let received = dir.join("frame");
        let closed = dir.join("closed");
        // Take exactly the frame, announce, then wait on the rest of stdin:
        // `cat` ends, and the mark appears, only if the pipe is closed.
        let script = format!(
            "head -c 37 > '{}'; printf 'SYNDEO-PROXY-READY 1 127.0.0.1:4567\\n'; \
             cat > /dev/null; touch '{}'; exec /bin/sleep 60",
            received.display(),
            closed.display()
        );
        let mut frame = b"SYA1".to_vec();
        frame.extend_from_slice(&[0x42u8; 32]);
        frame.push(b'\n');
        let owned = start_owned(sh(&script), Duration::from_secs(10), Some(frame.clone())).unwrap();
        assert_eq!(std::fs::read(&received).unwrap(), frame);
        std::thread::sleep(Duration::from_millis(300));
        assert!(!closed.exists(), "stdin was closed after the frame");
        drop(owned);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_browsers_own_proxy_takes_commands_on_the_pipe_only_it_holds() {
        assert!(PROXY_ARGS.contains(&"--control-stdin"));
        assert!(PROXY_ARGS.contains(&"--announce"));
    }

    /// A stand-in proxy that announces, then answers each line it reads with
    /// `answer`, after writing a line too long to be an answer and an
    /// unrelated one.
    fn answering(answer: &str) -> OwnedProxy {
        let script = format!(
            "printf 'SYNDEO-PROXY-READY 1 127.0.0.1:4567\\n'; \
             while read line; do head -c 4096 /dev/zero | tr '\\0' x; printf '\\n'; \
             if [ \"$line\" = clear-cache ]; then printf '{answer}\\n'; else printf 'what?\\n'; fi; done; \
             exec /bin/sleep 60"
        );
        start_owned(sh(&script), Duration::from_secs(10), None).unwrap()
    }

    #[test]
    fn clearing_the_cache_is_asked_on_stdin_and_answered_on_stdout() {
        let owned = answering("SYNDEO-PROXY-CLEARED 1 42");
        let control = owned.control();
        assert_eq!(control.clear_cache(Duration::from_secs(5)).unwrap(), 42);
        // And again: each answer is the one to its own question.
        assert_eq!(control.clear_cache(Duration::from_secs(5)).unwrap(), 42);
    }

    /// A line the proxy wrote before the question is not its answer.
    #[test]
    fn an_answer_from_before_the_question_is_not_taken_for_its_answer() {
        let owned = start_owned(
            sh(
                "printf 'SYNDEO-PROXY-READY 1 127.0.0.1:4567\\nSYNDEO-PROXY-CLEARED 1 99\\n'; \
                read line; printf 'SYNDEO-PROXY-CLEARED 1 42\\n'; exec /bin/sleep 60",
            ),
            Duration::from_secs(10),
            None,
        )
        .unwrap();
        // Long enough for the stray line to have arrived.
        std::thread::sleep(Duration::from_millis(300));
        assert_eq!(
            owned.control().clear_cache(Duration::from_secs(5)).unwrap(),
            42
        );
    }

    #[test]
    fn a_failure_or_any_other_answer_is_an_error() {
        for answer in [
            "SYNDEO-PROXY-FAILED 1",
            "SYNDEO-PROXY-CLEARED 1 ",
            "SYNDEO-PROXY-CLEARED 1 -1",
            "SYNDEO-PROXY-CLEARED 2 1",
            "SYNDEO-PROXY-UNKNOWN 1",
        ] {
            let owned = answering(answer);
            assert!(
                owned.control().clear_cache(Duration::from_secs(5)).is_err(),
                "{answer}"
            );
        }
    }

    #[test]
    fn a_proxy_that_does_not_answer_is_not_asked_again() {
        let owned = start_owned(
            sh("printf 'SYNDEO-PROXY-READY 1 127.0.0.1:4567\\n'; exec /bin/sleep 60"),
            Duration::from_secs(10),
            None,
        )
        .unwrap();
        let control = owned.control();
        let message = control
            .clear_cache(Duration::from_millis(300))
            .unwrap_err()
            .to_string();
        assert!(message.contains("did not say"), "{message}");
        let started = Instant::now();
        let message = control
            .clear_cache(Duration::from_secs(5))
            .unwrap_err()
            .to_string();
        assert!(message.contains("stopped answering"), "{message}");
        assert!(started.elapsed() < Duration::from_secs(1));
    }

    #[test]
    fn output_after_the_announcement_never_blocks_the_proxy() {
        // A megabyte to stdout after announcing, then a mark on disk — only if
        // every byte was written. Undrained, the write blocks on a full pipe,
        // or fails outright once the reader has gone (the real proxy ignores
        // SIGPIPE, so for it that is a failed write), and the mark never comes.
        let marker = std::env::temp_dir().join(format!(
            "syndeo-webkit-drain-{}-{}",
            std::process::id(),
            Instant::now().elapsed().as_nanos()
        ));
        let _ = std::fs::remove_file(&marker);
        let script = format!(
            "printf 'SYNDEO-PROXY-READY 1 127.0.0.1:4567\\n'; \
             head -c 1048576 /dev/zero && touch '{}'; exec /bin/sleep 60",
            marker.display()
        );
        let owned = start_owned(sh(&script), Duration::from_secs(10), None).unwrap();
        let deadline = Instant::now() + Duration::from_secs(10);
        while !marker.exists() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(20));
        }
        assert!(marker.exists(), "the proxy blocked writing to its stdout");
        drop(owned);
        let _ = std::fs::remove_file(&marker);
    }
}
