//! Agents: programs on this Mac that drive Syndeo, with the person's leave.
//!
//! A program — a command-line agent, a script, a test — talks to the running
//! application through Syndeo's own executable, as `Syndeo control` (one JSON
//! request and its answer, or a session of them on stdin) or `Syndeo mcp` (a
//! Model Context Protocol server on stdio). That client connects to a Unix
//! socket only the person's own account can reach, which exists only while
//! Syndeo > Agent Access… > *Allow agents to control Syndeo* is on.
//!
//! Nothing happens until the person allows it. A program's first request
//! raises Syndeo's prompt, saying who asks — the name it gives, which Syndeo
//! cannot check, and the program that started it, with its signature — and
//! what for; the person allows it once (this connection), for the session
//! (until Syndeo quits, or they take it back), or not at all. Each kind of
//! access is a [`Scope`], granted on its own, and running JavaScript in pages
//! is never implied by the others: it is asked for, and allowed, separately.
//!
//! An agent works only in tabs it opened itself, which say that it is in
//! control and offer Stop; the person's own tabs stay hidden from it. Its
//! pages are opened as the person's are, through Syndeo's proxy and pinned to
//! its authority, and only the web's own addresses.
//!
//! This module is the protocol, plain Rust and tested everywhere: requests
//! and answers, scopes, limits, which addresses an agent may open, how the
//! executable decides whether it is the application or a client, and the MCP
//! server's side of things. `control_macos` serves it in the application;
//! `agent_client` is the client.
//!
//! The wire: one JSON object a line, each way. A request is
//! `{"id": …, "cmd": "<command>", …}` and its answer is
//! `{"id": …, "ok": true, "result": …}` or
//! `{"id": …, "ok": false, "error": {"code": "…", "message": "…"}}`.

use crate::address::{self, Search, Typed};
use serde_json::{json, Map, Value};
use std::ffi::OsString;
use std::io::{self, BufRead};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};
use url::Url;

/// The longest request line the application reads; a longer one is refused.
pub const MAX_REQUEST: usize = 1 << 20;
/// The longest answer line a client reads: a screenshot is the largest.
pub const MAX_ANSWER: usize = 64 << 20;
/// The most text `type` enters at once.
pub const MAX_TEXT: usize = 100_000;
/// The most characters `read` returns, and how many by default.
pub const MAX_READ: usize = 1_000_000;
pub const DEFAULT_READ: usize = 100_000;
/// The most elements `snapshot` lists, and how many by default.
pub const MAX_ELEMENTS: usize = 2_000;
pub const DEFAULT_ELEMENTS: usize = 300;
/// The longest a request waits for a page, and how long by default.
pub const MAX_WAIT: Duration = Duration::from_secs(120);
pub const DEFAULT_WAIT: Duration = Duration::from_secs(30);
/// The longest name a program may give itself, as shown.
const MAX_NAME: usize = 60;
/// The longest address an agent may open.
const MAX_URL: usize = 8_192;
/// How many requests a connection may make: this many a second, in bursts
/// of up to [`BURST`].
pub const RATE: f64 = 30.0;
pub const BURST: f64 = 60.0;

/// The user defaults key under which *Allow agents to control Syndeo* is
/// kept. Off unless the person turned it on.
pub const SETTING_KEY: &str = "AgentControl";

/// Where the socket is, under the person's home folder: in a folder of its
/// own, which only they can enter, in the application's own place.
pub fn socket_path(home: &Path) -> PathBuf {
    socket_dir(home).join("control.sock")
}

/// The socket's folder.
pub fn socket_dir(home: &Path) -> PathBuf {
    home.join("Library/Application Support/Syndeo/agents")
}

// ------------------------------------------------------------- scopes

/// One kind of access an agent may be allowed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Scope {
    /// Read the pages in its tabs: address, title, text, a list of their
    /// links and fields, and a picture of them.
    Read,
    /// Open, close, choose and navigate its own tabs.
    Navigate,
    /// Click, type and scroll in its tabs.
    Act,
    /// Run its own JavaScript in its tabs' pages. Never implied by the
    /// others, and asked for on its own.
    Script,
}

impl Scope {
    pub const ALL: [Scope; 4] = [Scope::Read, Scope::Navigate, Scope::Act, Scope::Script];

    pub fn id(self) -> &'static str {
        match self {
            Scope::Read => "read",
            Scope::Navigate => "navigate",
            Scope::Act => "act",
            Scope::Script => "script",
        }
    }

    pub fn parse(text: &str) -> Option<Scope> {
        Scope::ALL.into_iter().find(|scope| scope.id() == text)
    }

    /// What the person is told this allows.
    pub fn describe(self) -> &'static str {
        match self {
            Scope::Read => {
                "read the pages it opens: their text, a list of their links and fields, and \
                 pictures of them"
            }
            Scope::Navigate => "open, close and navigate tabs of its own",
            Scope::Act => "click, type and scroll in its tabs",
            Scope::Script => {
                "run its own JavaScript in the pages of its tabs, with everything those pages \
                 can read and change, including what you are signed in to there"
            }
        }
    }

    fn bit(self) -> u8 {
        match self {
            Scope::Read => 1,
            Scope::Navigate => 2,
            Scope::Act => 4,
            Scope::Script => 8,
        }
    }
}

/// A set of scopes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Hash)]
pub struct Scopes(u8);

impl Scopes {
    pub const NONE: Scopes = Scopes(0);

    pub fn of(scopes: &[Scope]) -> Scopes {
        scopes
            .iter()
            .fold(Scopes::NONE, |all, &scope| all.with(scope))
    }

    pub fn with(self, scope: Scope) -> Scopes {
        Scopes(self.0 | scope.bit())
    }

    pub fn union(self, other: Scopes) -> Scopes {
        Scopes(self.0 | other.0)
    }

    /// Whether these scopes allow `scope`: only if it is one of them.
    pub fn allows(self, scope: Scope) -> bool {
        self.0 & scope.bit() != 0
    }

    /// Whether these scopes include every one of `other`.
    pub fn covers(self, other: Scopes) -> bool {
        Scope::ALL
            .into_iter()
            .filter(|&scope| other.allows(scope))
            .all(|scope| self.allows(scope))
    }

    pub fn is_empty(self) -> bool {
        self.0 == 0
    }

    pub fn list(self) -> Vec<Scope> {
        Scope::ALL
            .into_iter()
            .filter(|&scope| self.allows(scope))
            .collect()
    }

    pub fn ids(self) -> Vec<&'static str> {
        self.list().into_iter().map(Scope::id).collect()
    }

    /// Parse `read,navigate,act`: each a scope, none twice; `script` only if
    /// `script` is true.
    pub fn parse_list(text: &str, script: bool) -> Result<Scopes, String> {
        let mut scopes = Scopes::NONE;
        for part in text.split(',').map(str::trim).filter(|p| !p.is_empty()) {
            let scope = Scope::parse(part).ok_or_else(|| {
                format!("{part} is not a scope; the scopes are read, navigate, act and script")
            })?;
            if scope == Scope::Script && !script {
                return Err(SCRIPT_ALONE.to_string());
            }
            scopes = scopes.with(scope);
        }
        if scopes.is_empty() {
            return Err("ask for at least one scope: read, navigate or act".into());
        }
        Ok(scopes)
    }
}

/// Why `script` is not asked for with the others.
pub const SCRIPT_ALONE: &str = "script is asked for on its own, with {\"cmd\": \"request\", \
    \"scope\": \"script\"}, so that the person is asked about it on its own";

/// The scopes a program asks for when it says nothing.
pub const DEFAULT_SCOPES: Scopes = Scopes(1 | 2 | 4);

// ------------------------------------------------------------- failures

/// A request that was not done, and why: a code a program can act on, and
/// words a person can read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Failure {
    pub code: &'static str,
    pub message: String,
}

impl Failure {
    pub fn new(code: &'static str, message: impl Into<String>) -> Failure {
        Failure {
            code,
            message: message.into(),
        }
    }

    pub fn bad(message: impl Into<String>) -> Failure {
        Failure::new("bad_request", message)
    }
}

/// The answer to request `id`: done, with `result`.
pub fn ok(id: &Value, result: Value) -> Value {
    json!({"id": id, "ok": true, "result": result})
}

/// The answer to request `id`: not done, and why.
pub fn err(id: &Value, failure: &Failure) -> Value {
    json!({"id": id, "ok": false, "error": {"code": failure.code, "message": failure.message}})
}

// ------------------------------------------------------------- requests

/// What Back, Forward, Reload and Stop are to an agent.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Step {
    Back,
    Forward,
    Reload,
    Stop,
}

/// What a program may ask.
#[derive(Debug, Clone, PartialEq)]
pub enum Command {
    /// Who it is, and what it asks to do. First, always.
    Hello {
        client: String,
        scopes: Scopes,
    },
    /// One more scope; `script` is asked for only this way.
    Request {
        scope: Scope,
    },
    Status,
    Tabs,
    Read {
        tab: u64,
        max_chars: usize,
    },
    Snapshot {
        tab: u64,
        max_elements: usize,
    },
    Screenshot {
        tab: u64,
    },
    Wait {
        tab: u64,
        timeout: Duration,
    },
    Open {
        url: String,
        wait: bool,
        timeout: Duration,
    },
    Goto {
        tab: u64,
        url: String,
        wait: bool,
        timeout: Duration,
    },
    History {
        tab: u64,
        step: Step,
        wait: bool,
        timeout: Duration,
    },
    Select {
        tab: u64,
    },
    Close {
        tab: u64,
    },
    Click {
        tab: u64,
        element: String,
    },
    Type {
        tab: u64,
        element: String,
        text: String,
        submit: bool,
    },
    Scroll {
        tab: u64,
        element: Option<String>,
        dx: i32,
        dy: i32,
    },
    Eval {
        tab: u64,
        script: String,
    },
    Bye,
}

impl Command {
    /// What the request is called, for logs: never what it carries.
    pub fn name(&self) -> &'static str {
        match self {
            Command::Hello { .. } => "hello",
            Command::Request { .. } => "request",
            Command::Status => "status",
            Command::Tabs => "tabs",
            Command::Read { .. } => "read",
            Command::Snapshot { .. } => "snapshot",
            Command::Screenshot { .. } => "screenshot",
            Command::Wait { .. } => "wait",
            Command::Open { .. } => "open",
            Command::Goto { .. } => "goto",
            Command::History { step, .. } => match step {
                Step::Back => "back",
                Step::Forward => "forward",
                Step::Reload => "reload",
                Step::Stop => "stop",
            },
            Command::Select { .. } => "select",
            Command::Close { .. } => "close",
            Command::Click { .. } => "click",
            Command::Type { .. } => "type",
            Command::Scroll { .. } => "scroll",
            Command::Eval { .. } => "eval",
            Command::Bye => "bye",
        }
    }

    /// The scope a request needs, beyond the program having been allowed at
    /// all; none for saying hello, asking, the status and goodbye.
    pub fn needs(&self) -> Option<Scope> {
        match self {
            Command::Hello { .. } | Command::Request { .. } | Command::Status | Command::Bye => {
                None
            }
            Command::Tabs
            | Command::Read { .. }
            | Command::Snapshot { .. }
            | Command::Screenshot { .. }
            | Command::Wait { .. } => Some(Scope::Read),
            Command::Open { .. }
            | Command::Goto { .. }
            | Command::History { .. }
            | Command::Select { .. }
            | Command::Close { .. } => Some(Scope::Navigate),
            Command::Click { .. } | Command::Type { .. } | Command::Scroll { .. } => {
                Some(Scope::Act)
            }
            Command::Eval { .. } => Some(Scope::Script),
        }
    }

    /// Whether, once done, the request waits for its page to load, and for
    /// how long at most.
    pub fn waits(&self) -> Option<Duration> {
        match self {
            Command::Open { wait, timeout, .. }
            | Command::Goto { wait, timeout, .. }
            | Command::History { wait, timeout, .. } => wait.then_some(*timeout),
            _ => None,
        }
    }
}

/// One request: its id, echoed in the answer, and what it asks.
#[derive(Debug, Clone, PartialEq)]
pub struct Request {
    pub id: Value,
    pub command: Command,
}

/// Read one request line. On failure, the id it gave, if any could be read,
/// and why.
pub fn parse_request(line: &str) -> Result<Request, (Value, Failure)> {
    let value: Value = serde_json::from_str(line)
        .map_err(|e| (Value::Null, Failure::bad(format!("not JSON: {e}"))))?;
    let Value::Object(fields) = value else {
        return Err((Value::Null, Failure::bad("a request is a JSON object")));
    };
    let id = fields.get("id").cloned().unwrap_or(Value::Null);
    if !matches!(id, Value::Null | Value::Number(_) | Value::String(_)) {
        return Err((Value::Null, Failure::bad("id is a number or a string")));
    }
    if let Value::String(s) = &id {
        if s.len() > 200 {
            return Err((Value::Null, Failure::bad("id is too long")));
        }
    }
    let command = command(&fields).map_err(|failure| (id.clone(), failure))?;
    Ok(Request { id, command })
}

/// The fields a command may have, beyond `id` and `cmd`.
fn only(fields: &Map<String, Value>, allowed: &[&str]) -> Result<(), Failure> {
    for key in fields.keys() {
        if key != "id" && key != "cmd" && !allowed.contains(&key.as_str()) {
            return Err(Failure::bad(format!(
                "{key} is not a field of this command"
            )));
        }
    }
    Ok(())
}

fn text<'a>(fields: &'a Map<String, Value>, key: &str, max: usize) -> Result<&'a str, Failure> {
    match fields.get(key) {
        Some(Value::String(s)) if s.len() <= max => Ok(s),
        Some(Value::String(_)) => Err(Failure::new(
            "too_large",
            format!("{key} is longer than {max} bytes"),
        )),
        Some(_) => Err(Failure::bad(format!("{key} is a string"))),
        None => Err(Failure::bad(format!("{key} is missing"))),
    }
}

fn tab_of(fields: &Map<String, Value>) -> Result<u64, Failure> {
    match fields.get("tab") {
        Some(Value::Number(n)) => n
            .as_u64()
            .filter(|&n| n > 0)
            .ok_or_else(|| Failure::bad("tab is a tab's number, from open or tabs")),
        Some(_) => Err(Failure::bad("tab is a number")),
        None => Err(Failure::bad("tab is missing")),
    }
}

fn flag(fields: &Map<String, Value>, key: &str, default: bool) -> Result<bool, Failure> {
    match fields.get(key) {
        None => Ok(default),
        Some(Value::Bool(b)) => Ok(*b),
        Some(_) => Err(Failure::bad(format!("{key} is true or false"))),
    }
}

fn count(fields: &Map<String, Value>, key: &str, default: u64, max: u64) -> Result<u64, Failure> {
    match fields.get(key) {
        None => Ok(default),
        Some(Value::Number(n)) => match n.as_u64() {
            Some(n) if n <= max => Ok(n),
            _ => Err(Failure::bad(format!("{key} is a whole number up to {max}"))),
        },
        Some(_) => Err(Failure::bad(format!("{key} is a number"))),
    }
}

fn offset(fields: &Map<String, Value>, key: &str) -> Result<i32, Failure> {
    match fields.get(key) {
        None => Ok(0),
        Some(Value::Number(n)) => n
            .as_i64()
            .filter(|n| n.unsigned_abs() <= 1_000_000)
            .map(|n| n as i32)
            .ok_or_else(|| Failure::bad(format!("{key} is a whole number of points"))),
        Some(_) => Err(Failure::bad(format!("{key} is a number"))),
    }
}

/// An element's reference, as `snapshot` gives it: `e` and a number.
fn element(fields: &Map<String, Value>) -> Result<String, Failure> {
    let reference = text(fields, "ref", 16)?;
    let digits = reference.strip_prefix('e').unwrap_or("");
    if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return Err(Failure::bad(
            "ref is an element's reference from snapshot, such as e12",
        ));
    }
    Ok(reference.to_string())
}

fn timeout(fields: &Map<String, Value>) -> Result<Duration, Failure> {
    let max = MAX_WAIT.as_millis() as u64;
    count(fields, "timeout_ms", DEFAULT_WAIT.as_millis() as u64, max).map(Duration::from_millis)
}

fn command(fields: &Map<String, Value>) -> Result<Command, Failure> {
    let name = match fields.get("cmd") {
        Some(Value::String(name)) => name.as_str(),
        Some(_) => return Err(Failure::bad("cmd is a string")),
        None => return Err(Failure::bad("cmd is missing")),
    };
    let tab_only = |allowed: &[&str]| -> Result<u64, Failure> {
        only(fields, allowed)?;
        tab_of(fields)
    };
    Ok(match name {
        "hello" => {
            only(fields, &["client", "scopes"])?;
            let client = match fields.get("client") {
                None => String::new(),
                Some(Value::String(s)) if s.len() <= 400 => s.clone(),
                Some(_) => return Err(Failure::bad("client is a short string: your name")),
            };
            let scopes = match fields.get("scopes") {
                None => DEFAULT_SCOPES,
                Some(Value::Array(list)) => {
                    let mut names = Vec::new();
                    for item in list {
                        match item {
                            Value::String(s) => names.push(s.as_str()),
                            _ => return Err(Failure::bad("scopes is a list of strings")),
                        }
                    }
                    Scopes::parse_list(&names.join(","), false).map_err(Failure::bad)?
                }
                Some(_) => return Err(Failure::bad("scopes is a list of strings")),
            };
            Command::Hello { client, scopes }
        }
        "request" => {
            only(fields, &["scope"])?;
            let name = text(fields, "scope", 16)?;
            let scope = Scope::parse(name).ok_or_else(|| {
                Failure::bad(format!(
                    "{name} is not a scope; the scopes are read, navigate, act and script"
                ))
            })?;
            Command::Request { scope }
        }
        "status" => {
            only(fields, &[])?;
            Command::Status
        }
        "tabs" => {
            only(fields, &[])?;
            Command::Tabs
        }
        "bye" => {
            only(fields, &[])?;
            Command::Bye
        }
        "read" => Command::Read {
            tab: tab_only(&["tab", "max_chars"])?,
            max_chars: count(fields, "max_chars", DEFAULT_READ as u64, MAX_READ as u64)? as usize,
        },
        "snapshot" => Command::Snapshot {
            tab: tab_only(&["tab", "max_elements"])?,
            max_elements: count(
                fields,
                "max_elements",
                DEFAULT_ELEMENTS as u64,
                MAX_ELEMENTS as u64,
            )? as usize,
        },
        "screenshot" => Command::Screenshot {
            tab: tab_only(&["tab"])?,
        },
        "wait" => Command::Wait {
            tab: tab_only(&["tab", "timeout_ms"])?,
            timeout: timeout(fields)?,
        },
        "open" => {
            only(fields, &["url", "wait", "timeout_ms"])?;
            Command::Open {
                url: text(fields, "url", MAX_URL)?.to_string(),
                wait: flag(fields, "wait", true)?,
                timeout: timeout(fields)?,
            }
        }
        "goto" => Command::Goto {
            tab: tab_only(&["tab", "url", "wait", "timeout_ms"])?,
            url: text(fields, "url", MAX_URL)?.to_string(),
            wait: flag(fields, "wait", true)?,
            timeout: timeout(fields)?,
        },
        "back" | "forward" | "reload" | "stop" => Command::History {
            tab: tab_only(&["tab", "wait", "timeout_ms"])?,
            step: match name {
                "back" => Step::Back,
                "forward" => Step::Forward,
                "reload" => Step::Reload,
                _ => Step::Stop,
            },
            wait: flag(fields, "wait", name != "stop")?,
            timeout: timeout(fields)?,
        },
        "select" => Command::Select {
            tab: tab_only(&["tab"])?,
        },
        "close" => Command::Close {
            tab: tab_only(&["tab"])?,
        },
        "click" => Command::Click {
            tab: tab_only(&["tab", "ref"])?,
            element: element(fields)?,
        },
        "type" => Command::Type {
            tab: tab_only(&["tab", "ref", "text", "submit"])?,
            element: element(fields)?,
            text: text(fields, "text", MAX_TEXT)?.to_string(),
            submit: flag(fields, "submit", false)?,
        },
        "scroll" => {
            let tab = tab_only(&["tab", "ref", "dx", "dy"])?;
            let element = if fields.contains_key("ref") {
                Some(element(fields)?)
            } else {
                None
            };
            Command::Scroll {
                tab,
                element,
                dx: offset(fields, "dx")?,
                dy: offset(fields, "dy")?,
            }
        }
        "eval" => Command::Eval {
            tab: tab_only(&["tab", "script"])?,
            script: text(fields, "script", MAX_REQUEST)?.to_string(),
        },
        other => {
            return Err(Failure::new(
                "unknown_command",
                format!("{other} is not a command Syndeo knows"),
            ))
        }
    })
}

/// The address an agent asked to open, read as the address field reads what
/// a person types, with no search: an `http` or `https` address, or a host,
/// which is opened over https. Anything else is refused, with why.
pub fn web_url(text: &str) -> Result<Url, Failure> {
    let refused = |why: String| Failure::new("refused", why);
    match address::interpret(text, Search::Off) {
        Typed::Load(url) if matches!(url.scheme(), "http" | "https") => Ok(url),
        Typed::Load(url) => Err(refused(format!(
            "agents open http and https addresses, not {}: ones",
            url.scheme()
        ))),
        Typed::Refused(why) => Err(refused(why)),
        Typed::Empty => Err(Failure::bad("url is empty")),
        Typed::Search(_) => Err(refused(format!("{text} is not a web address"))),
    }
}

// ------------------------------------------------------------- who asks

/// One process in the line from a client up to what started it.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Proc {
    pub pid: u32,
    /// Its executable's path, as the kernel has it.
    pub path: String,
    /// When it started, in seconds and microseconds: with the pid, which
    /// process this is, since pids are used again.
    pub started: (u64, u64),
}

/// Programs that only start others: a shell running a command, `env`,
/// `sudo`, `nohup` and the like. The agent is what started them.
const LAUNCHERS: [&str; 22] = [
    "sh",
    "bash",
    "zsh",
    "fish",
    "dash",
    "ksh",
    "tcsh",
    "csh",
    "env",
    "nohup",
    "timeout",
    "gtimeout",
    "xargs",
    "sudo",
    "login",
    "script",
    "time",
    "nice",
    "caffeinate",
    "stdbuf",
    "arch",
    "exec",
];

/// Which process in `chain` — the connecting process first, then its
/// parent, and so on — is the agent the person is asked about: the first
/// that is neither Syndeo's own executable, `own`, running as a client, nor
/// a program that only starts others. If every one is, the last.
pub fn root_of(chain: &[Proc], own: &str) -> usize {
    chain
        .iter()
        .position(|proc| {
            let name = proc.path.rsplit('/').next().unwrap_or(&proc.path);
            proc.path != own && !LAUNCHERS.contains(&name)
        })
        .unwrap_or(chain.len().saturating_sub(1))
}

/// A name a program gave itself, as Syndeo shows it: one line, no control
/// characters, and short.
pub fn display_name(name: &str) -> String {
    let clean: String = name
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    if clean.is_empty() {
        return "An unnamed program".to_string();
    }
    if clean.chars().count() > MAX_NAME {
        let short: String = clean.chars().take(MAX_NAME - 1).collect();
        return format!("{short}…");
    }
    clean
}

// ------------------------------------------------------------- limits

/// Requests a connection may make: [`RATE`] a second, in bursts of [`BURST`].
#[derive(Debug)]
pub struct Bucket {
    tokens: f64,
    last: Instant,
}

impl Bucket {
    pub fn new(now: Instant) -> Bucket {
        Bucket {
            tokens: BURST,
            last: now,
        }
    }

    /// Whether one more request may be made at `now`.
    pub fn take(&mut self, now: Instant) -> bool {
        let earned = now.saturating_duration_since(self.last).as_secs_f64() * RATE;
        self.tokens = (self.tokens + earned).min(BURST);
        self.last = now;
        if self.tokens >= 1.0 {
            self.tokens -= 1.0;
            true
        } else {
            false
        }
    }
}

/// One line read by [`read_line`].
#[derive(Debug, PartialEq, Eq)]
pub enum Line {
    Text(String),
    /// Longer than allowed: read past, and not kept.
    TooLong,
    /// Not UTF-8.
    NotText,
    /// The other end closed.
    End,
}

/// Read one line, without its newline, keeping no more than `max` bytes of
/// it: a longer one is read to its end and refused.
pub fn read_line(reader: &mut impl BufRead, max: usize) -> io::Result<Line> {
    let mut line = Vec::new();
    let mut too_long = false;
    loop {
        let buffer = match reader.fill_buf() {
            Ok(buffer) => buffer,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(e),
        };
        if buffer.is_empty() {
            if line.is_empty() && !too_long {
                return Ok(Line::End);
            }
            break;
        }
        let (part, found) = match buffer.iter().position(|&b| b == b'\n') {
            Some(at) => (&buffer[..at], Some(at)),
            None => (buffer, None),
        };
        if !too_long {
            if line.len() + part.len() > max {
                too_long = true;
                line = Vec::new();
            } else {
                line.extend_from_slice(part);
            }
        }
        let used = found.map_or(part.len(), |at| at + 1);
        reader.consume(used);
        if found.is_some() {
            break;
        }
    }
    if too_long {
        return Ok(Line::TooLong);
    }
    Ok(match String::from_utf8(line) {
        Ok(text) => Line::Text(text.trim_end_matches('\r').to_string()),
        Err(_) => Line::NotText,
    })
}

// ------------------------------------------------------------- the executable

/// What Syndeo's executable is, this time.
#[derive(Debug, PartialEq, Eq)]
pub enum Mode {
    /// The application.
    App,
    /// `Syndeo control …`: a client, with the arguments after `control`.
    Control(Vec<String>),
    /// `Syndeo mcp …`: an MCP server on stdio, a client of the application.
    Mcp(Vec<String>),
}

/// Decide, from the arguments and the parent's pid, what this process is.
///
/// Launched by launchd — as Launch Services launches every application,
/// from Finder, the Dock or `open`, whatever arguments it is given — it is
/// the application, always: `open -a Syndeo --args control …` opens Syndeo,
/// and never a client. Run by another program with `control` or `mcp` first,
/// it is that client, and never the application.
pub fn mode(args: &[OsString], parent: u32) -> Mode {
    if parent == 1 {
        return Mode::App;
    }
    let rest = || -> Vec<String> {
        args.iter()
            .skip(2)
            .map(|a| a.to_string_lossy().into_owned())
            .collect()
    };
    match args.get(1).and_then(|a| a.to_str()) {
        Some("control") => Mode::Control(rest()),
        Some("mcp") => Mode::Mcp(rest()),
        _ => Mode::App,
    }
}

/// What a client was told on its command line.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct ClientOptions {
    /// The name it gives the person.
    pub name: Option<String>,
    /// The scopes it asks for when it says hello.
    pub scopes: Option<Scopes>,
    /// For `control`: one request to make, as JSON.
    pub request: Option<String>,
    pub help: bool,
}

/// Read a client's arguments. `request` says whether one request may be
/// given (`control`) or not (`mcp`).
pub fn client_options(args: &[String], request: bool) -> Result<ClientOptions, String> {
    let mut options = ClientOptions::default();
    let mut args = args.iter();
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "-h" | "--help" => options.help = true,
            "--name" => {
                let name = args.next().ok_or("--name needs a name")?;
                options.name = Some(name.clone());
            }
            "--scopes" => {
                let list = args
                    .next()
                    .ok_or("--scopes needs a list, such as read,navigate")?;
                options.scopes = Some(Scopes::parse_list(list, false)?);
            }
            flag if flag.starts_with('-') => return Err(format!("{flag} is not an option")),
            json if request && options.request.is_none() => {
                options.request = Some(json.to_string())
            }
            other => return Err(format!("unexpected argument: {other}")),
        }
    }
    Ok(options)
}

pub const CONTROL_USAGE: &str = "\
usage: Syndeo control [--name NAME] [--scopes read,navigate,act] [REQUEST]

Drives the Syndeo application that is running, once it allows this program.
Turn on Syndeo > Agent Access… > Allow agents to control Syndeo first.

With REQUEST, a JSON object such as '{\"cmd\": \"open\", \"url\": \"https://example.com\"}',
makes that one request and prints its answer. Without it, reads one request a
line from stdin and prints one answer a line, as a session: the person is
asked once for the session, not once a request.

Commands: status, tabs, open, goto, back, forward, reload, stop, wait, select,
close, read, snapshot, screenshot, click, type, scroll, eval, request.
Exit status: 0 done, 1 refused or failed, 2 usage, 3 Syndeo not reachable.
";

pub const MCP_USAGE: &str = "\
usage: Syndeo mcp [--name NAME] [--scopes read,navigate,act]

A Model Context Protocol server on stdio, through which an agent drives the
Syndeo application that is running, once the person allows it. Turn on
Syndeo > Agent Access… > Allow agents to control Syndeo first. For example:

  claude mcp add syndeo -- /Applications/Syndeo.app/Contents/MacOS/Syndeo mcp
";

// ------------------------------------------------------------- MCP

/// The protocol versions this server speaks, newest first.
const MCP_VERSIONS: [&str; 3] = ["2025-06-18", "2025-03-26", "2024-11-05"];

/// What the MCP server needs from the application: one request, answered.
/// The real one connects and says hello when first asked, as `client`.
pub trait Backend {
    fn call(&mut self, client: &str, request: Value) -> Result<Value, Failure>;
}

/// The MCP server's side of the conversation: JSON-RPC requests in, answers
/// out, and each tool call made into requests to the application.
pub struct Mcp<B> {
    backend: B,
    /// The client's name, from `initialize`, or the one given on the
    /// command line, which wins.
    client: String,
    named: bool,
}

/// One tool: its name, what it does, and its arguments' schema.
fn tool(name: &str, description: &str, properties: Value, required: &[&str]) -> Value {
    json!({
        "name": name,
        "description": description,
        "inputSchema": {
            "type": "object",
            "properties": properties,
            "required": required,
            "additionalProperties": false,
        },
    })
}

/// Every tool, in the order `tools/list` gives them.
pub fn tools() -> Vec<Value> {
    let tab = json!({"type": "integer", "minimum": 1, "description": "The tab's number, from syndeo_open or syndeo_tabs."});
    let reference = json!({"type": "string", "description": "An element's reference from syndeo_snapshot, such as e12."});
    vec![
        tool(
            "syndeo_open",
            "Open a web address in a new Syndeo tab the person can see, and wait for it to load. Returns the tab's number, address, title and route. Only http and https addresses.",
            json!({"url": {"type": "string"}, "wait": {"type": "boolean"}}),
            &["url"],
        ),
        tool(
            "syndeo_goto",
            "Go to a web address in one of your tabs, and wait for it to load.",
            json!({"tab": tab, "url": {"type": "string"}}),
            &["tab", "url"],
        ),
        tool("syndeo_tabs", "List your tabs: number, address, title, whether loading, route.", json!({}), &[]),
        tool(
            "syndeo_read",
            "The text of a tab's page, with its address, title and route.",
            json!({"tab": tab, "max_chars": {"type": "integer", "minimum": 0}}),
            &["tab"],
        ),
        tool(
            "syndeo_snapshot",
            "The visible links, buttons and fields of a tab's page, each with a reference to click or type into.",
            json!({"tab": tab, "max_elements": {"type": "integer", "minimum": 0}}),
            &["tab"],
        ),
        tool(
            "syndeo_click",
            "Click an element, by its reference from syndeo_snapshot.",
            json!({"tab": tab, "ref": reference}),
            &["tab", "ref"],
        ),
        tool(
            "syndeo_type",
            "Type text into a field, replacing what is there; with submit, then submit its form.",
            json!({"tab": tab, "ref": reference, "text": {"type": "string"}, "submit": {"type": "boolean"}}),
            &["tab", "ref", "text"],
        ),
        tool(
            "syndeo_scroll",
            "Scroll a tab's page by dx and dy points, or bring an element into view.",
            json!({"tab": tab, "ref": reference, "dx": {"type": "integer"}, "dy": {"type": "integer"}}),
            &["tab"],
        ),
        tool(
            "syndeo_navigate",
            "Back, forward, reload or stop, in one of your tabs.",
            json!({"tab": tab, "action": {"type": "string", "enum": ["back", "forward", "reload", "stop"]}}),
            &["tab", "action"],
        ),
        tool(
            "syndeo_wait",
            "Wait for a tab's page to finish loading.",
            json!({"tab": tab, "timeout_ms": {"type": "integer", "minimum": 0}}),
            &["tab"],
        ),
        tool("syndeo_screenshot", "A picture of a tab's page, as PNG.", json!({"tab": tab}), &["tab"]),
        tool("syndeo_select", "Bring one of your tabs to the front.", json!({"tab": tab}), &["tab"]),
        tool("syndeo_close", "Close one of your tabs.", json!({"tab": tab}), &["tab"]),
        tool(
            "syndeo_eval",
            "Run JavaScript in a tab's page, as the body of an async function: use return to give back a JSON value. The person is asked separately before the first time.",
            json!({"tab": tab, "script": {"type": "string"}}),
            &["tab", "script"],
        ),
    ]
}

/// The application request a tool call makes, from its arguments.
fn request_for(tool: &str, arguments: &Map<String, Value>) -> Result<Value, String> {
    let mut request = arguments.clone();
    let cmd = match tool {
        "syndeo_open" => "open",
        "syndeo_goto" => "goto",
        "syndeo_tabs" => "tabs",
        "syndeo_read" => "read",
        "syndeo_snapshot" => "snapshot",
        "syndeo_click" => "click",
        "syndeo_type" => "type",
        "syndeo_scroll" => "scroll",
        "syndeo_wait" => "wait",
        "syndeo_screenshot" => "screenshot",
        "syndeo_select" => "select",
        "syndeo_close" => "close",
        "syndeo_eval" => "eval",
        "syndeo_navigate" => {
            let action = match request.remove("action") {
                Some(Value::String(action))
                    if ["back", "forward", "reload", "stop"].contains(&action.as_str()) =>
                {
                    action
                }
                _ => return Err("action is back, forward, reload or stop".into()),
            };
            request.insert("cmd".into(), Value::String(action));
            return Ok(Value::Object(request));
        }
        other => return Err(format!("{other} is not one of Syndeo's tools")),
    };
    if request.contains_key("cmd") || request.contains_key("id") {
        return Err("cmd and id are not arguments".into());
    }
    request.insert("cmd".into(), Value::String(cmd.into()));
    Ok(Value::Object(request))
}

impl<B: Backend> Mcp<B> {
    /// A server that talks to the application through `backend`; `name`,
    /// if given, is the name it gives the person, whatever `initialize`
    /// says.
    pub fn new(backend: B, name: Option<String>) -> Mcp<B> {
        Mcp {
            backend,
            named: name.is_some(),
            client: name.unwrap_or_else(|| "An MCP client".to_string()),
        }
    }

    #[cfg(test)]
    pub fn backend(&mut self) -> &mut B {
        &mut self.backend
    }

    /// Answer one line from the client: a JSON-RPC answer, or nothing for a
    /// notification.
    pub fn handle(&mut self, line: &str) -> Option<String> {
        let message: Value = match serde_json::from_str(line) {
            Ok(message) => message,
            Err(e) => {
                return Some(rpc_error(
                    &Value::Null,
                    -32700,
                    &format!("parse error: {e}"),
                ))
            }
        };
        let Value::Object(message) = message else {
            return Some(rpc_error(
                &Value::Null,
                -32600,
                "a message is a JSON object",
            ));
        };
        let id = message.get("id").cloned();
        let method = message.get("method").and_then(Value::as_str);
        let (Some(id), Some(method)) = (id, method) else {
            // A notification, or an answer to a request this server never
            // makes: nothing to say.
            return None;
        };
        let params = message
            .get("params")
            .and_then(Value::as_object)
            .cloned()
            .unwrap_or_default();
        let answer = match method {
            "initialize" => self.initialize(&params),
            "ping" => Ok(json!({})),
            "tools/list" => Ok(json!({"tools": tools()})),
            "tools/call" => self.call_tool(&params),
            other => Err((-32601, format!("{other} is not a method this server has"))),
        };
        Some(match answer {
            Ok(result) => json!({"jsonrpc": "2.0", "id": id, "result": result}).to_string(),
            Err((code, message)) => rpc_error(&id, code, &message),
        })
    }

    fn initialize(&mut self, params: &Map<String, Value>) -> Result<Value, (i64, String)> {
        let asked = params.get("protocolVersion").and_then(Value::as_str);
        let version = MCP_VERSIONS
            .into_iter()
            .find(|v| Some(*v) == asked)
            .unwrap_or(MCP_VERSIONS[0]);
        if !self.named {
            if let Some(info) = params.get("clientInfo").and_then(Value::as_object) {
                let name = info.get("name").and_then(Value::as_str).unwrap_or("");
                let version = info.get("version").and_then(Value::as_str).unwrap_or("");
                if !name.is_empty() {
                    self.client = format!("{name} {version}").trim().to_string();
                }
            }
        }
        Ok(json!({
            "protocolVersion": version,
            "capabilities": {"tools": {"listChanged": false}},
            "serverInfo": {"name": "syndeo", "version": env!("CARGO_PKG_VERSION")},
            "instructions": "Syndeo is a web browser on this Mac. These tools work only in tabs \
                you open with syndeo_open, which the person can see, and only once the person \
                allows it in Syndeo. Use syndeo_snapshot to find links and fields, then \
                syndeo_click and syndeo_type with their references. syndeo_eval needs a \
                separate permission, which the person is asked for the first time.",
        }))
    }

    fn call_tool(&mut self, params: &Map<String, Value>) -> Result<Value, (i64, String)> {
        let name = params
            .get("name")
            .and_then(Value::as_str)
            .ok_or((-32602, "name is missing".to_string()))?;
        let arguments = match params.get("arguments") {
            None => Map::new(),
            Some(Value::Object(arguments)) => arguments.clone(),
            Some(_) => return Err((-32602, "arguments is an object".to_string())),
        };
        let request = request_for(name, &arguments).map_err(|e| (-32602, e))?;
        let mut answer = self.backend.call(&self.client, request.clone());
        // The first JavaScript: ask for it, once, and try again.
        if name == "syndeo_eval" && matches!(&answer, Err(f) if f.code == "not_granted") {
            answer = self
                .backend
                .call(&self.client, json!({"cmd": "request", "scope": "script"}))
                .and_then(|_| self.backend.call(&self.client, request));
        }
        Ok(match answer {
            Ok(result) => tool_result(name, result),
            Err(failure) => json!({
                "content": [{"type": "text", "text": format!("{}: {}", failure.code, failure.message)}],
                "isError": true,
            }),
        })
    }
}

/// A tool's result, as MCP content: a screenshot as an image, everything
/// else as its JSON.
fn tool_result(name: &str, result: Value) -> Value {
    if name == "syndeo_screenshot" {
        if let Some(png) = result.get("png").and_then(Value::as_str) {
            let mut about = result.clone();
            if let Some(about) = about.as_object_mut() {
                about.remove("png");
            }
            return json!({
                "content": [
                    {"type": "image", "data": png, "mimeType": "image/png"},
                    {"type": "text", "text": about.to_string()},
                ],
                "isError": false,
            });
        }
    }
    let text = serde_json::to_string_pretty(&result).unwrap_or_else(|_| result.to_string());
    json!({"content": [{"type": "text", "text": text}], "isError": false})
}

fn rpc_error(id: &Value, code: i64, message: &str) -> String {
    json!({"jsonrpc": "2.0", "id": id, "error": {"code": code, "message": message}}).to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    fn parse(line: &str) -> Result<Command, Failure> {
        parse_request(line).map(|r| r.command).map_err(|(_, f)| f)
    }

    fn code(line: &str) -> &'static str {
        parse(line).expect_err(line).code
    }

    #[test]
    fn scopes_allow_only_what_they_hold() {
        let read = Scopes::of(&[Scope::Read]);
        assert!(read.allows(Scope::Read));
        for scope in [Scope::Navigate, Scope::Act, Scope::Script] {
            assert!(!read.allows(scope), "{scope:?}");
        }
        assert!(!Scopes::NONE.allows(Scope::Read));
        assert!(
            !DEFAULT_SCOPES.allows(Scope::Script),
            "script is never a default"
        );
        assert!(DEFAULT_SCOPES.covers(Scopes::of(&[Scope::Read, Scope::Act])));
        assert!(!DEFAULT_SCOPES.covers(Scopes::of(&[Scope::Script])));
        assert!(Scopes::NONE.covers(Scopes::NONE));
        assert_eq!(DEFAULT_SCOPES.ids(), ["read", "navigate", "act"]);
        assert!(read
            .union(Scopes::of(&[Scope::Script]))
            .allows(Scope::Script));
    }

    #[test]
    fn script_is_never_asked_for_with_the_others() {
        assert_eq!(
            Scopes::parse_list("read,script", false),
            Err(SCRIPT_ALONE.to_string())
        );
        assert_eq!(
            code(r#"{"cmd":"hello","scopes":["read","script"]}"#),
            "bad_request"
        );
        assert_eq!(
            parse(r#"{"cmd":"request","scope":"script"}"#),
            Ok(Command::Request {
                scope: Scope::Script
            })
        );
        assert!(Scopes::parse_list("read, act", false).is_ok());
        assert!(Scopes::parse_list("", false).is_err());
        assert!(Scopes::parse_list("read,everything", false).is_err());
    }

    #[test]
    fn every_command_needs_its_scope_and_eval_needs_script() {
        let cases = [
            (r#"{"cmd":"tabs"}"#, Some(Scope::Read)),
            (r#"{"cmd":"read","tab":1}"#, Some(Scope::Read)),
            (r#"{"cmd":"snapshot","tab":1}"#, Some(Scope::Read)),
            (r#"{"cmd":"screenshot","tab":1}"#, Some(Scope::Read)),
            (r#"{"cmd":"wait","tab":1}"#, Some(Scope::Read)),
            (
                r#"{"cmd":"open","url":"https://example.com"}"#,
                Some(Scope::Navigate),
            ),
            (
                r#"{"cmd":"goto","tab":1,"url":"example.com"}"#,
                Some(Scope::Navigate),
            ),
            (r#"{"cmd":"back","tab":1}"#, Some(Scope::Navigate)),
            (r#"{"cmd":"reload","tab":1}"#, Some(Scope::Navigate)),
            (r#"{"cmd":"select","tab":1}"#, Some(Scope::Navigate)),
            (r#"{"cmd":"close","tab":1}"#, Some(Scope::Navigate)),
            (r#"{"cmd":"click","tab":1,"ref":"e3"}"#, Some(Scope::Act)),
            (
                r#"{"cmd":"type","tab":1,"ref":"e3","text":"x"}"#,
                Some(Scope::Act),
            ),
            (r#"{"cmd":"scroll","tab":1,"dy":200}"#, Some(Scope::Act)),
            (
                r#"{"cmd":"eval","tab":1,"script":"return 1"}"#,
                Some(Scope::Script),
            ),
            (r#"{"cmd":"status"}"#, None),
            (r#"{"cmd":"hello"}"#, None),
            (r#"{"cmd":"bye"}"#, None),
        ];
        for (line, scope) in cases {
            assert_eq!(parse(line).expect(line).needs(), scope, "{line}");
        }
    }

    #[test]
    fn requests_are_read_with_their_defaults() {
        let request =
            parse_request(r#"{"id":7,"cmd":"open","url":"https://example.com"}"#).unwrap();
        assert_eq!(request.id, json!(7));
        assert_eq!(
            request.command,
            Command::Open {
                url: "https://example.com".into(),
                wait: true,
                timeout: DEFAULT_WAIT
            }
        );
        assert_eq!(request.command.waits(), Some(DEFAULT_WAIT));
        assert_eq!(
            parse(r#"{"cmd":"read","tab":2}"#),
            Ok(Command::Read {
                tab: 2,
                max_chars: DEFAULT_READ
            })
        );
        assert_eq!(
            parse(r#"{"cmd":"stop","tab":2}"#).unwrap().waits(),
            None,
            "Stop does not wait"
        );
        assert_eq!(
            parse(r#"{"cmd":"hello","client":"ci"}"#),
            Ok(Command::Hello {
                client: "ci".into(),
                scopes: DEFAULT_SCOPES
            })
        );
        assert_eq!(
            parse(r#"{"cmd":"scroll","tab":1,"ref":"e4"}"#),
            Ok(Command::Scroll {
                tab: 1,
                element: Some("e4".into()),
                dx: 0,
                dy: 0
            })
        );
    }

    #[test]
    fn malformed_requests_are_refused_never_guessed_at() {
        for (line, want) in [
            ("", "bad_request"),
            ("not json", "bad_request"),
            ("[1,2]", "bad_request"),
            ("{}", "bad_request"),
            (r#"{"cmd":5}"#, "bad_request"),
            (r#"{"cmd":"launch-missiles"}"#, "unknown_command"),
            (r#"{"cmd":"status","extra":1}"#, "bad_request"),
            (r#"{"cmd":"read"}"#, "bad_request"),
            (r#"{"cmd":"read","tab":0}"#, "bad_request"),
            (r#"{"cmd":"read","tab":-1}"#, "bad_request"),
            (r#"{"cmd":"read","tab":"1"}"#, "bad_request"),
            (r#"{"cmd":"read","tab":1.5}"#, "bad_request"),
            (
                r#"{"cmd":"read","tab":1,"max_chars":1000001}"#,
                "bad_request",
            ),
            (r##"{"cmd":"click","tab":1,"ref":"#x"}"##, "bad_request"),
            (r#"{"cmd":"click","tab":1,"ref":"e"}"#, "bad_request"),
            (
                r#"{"cmd":"click","tab":1,"ref":"e1; alert(1)"}"#,
                "bad_request",
            ),
            (r#"{"cmd":"click","tab":1}"#, "bad_request"),
            (r#"{"cmd":"type","tab":1,"ref":"e1"}"#, "bad_request"),
            (
                r#"{"cmd":"open","url":"https://example.com","wait":"yes"}"#,
                "bad_request",
            ),
            (
                r#"{"cmd":"open","url":"https://example.com","timeout_ms":120001}"#,
                "bad_request",
            ),
            (
                r#"{"cmd":"scroll","tab":1,"dy":99999999999}"#,
                "bad_request",
            ),
            (r#"{"cmd":"hello","scopes":"read"}"#, "bad_request"),
            (r#"{"cmd":"hello","scopes":[1]}"#, "bad_request"),
            (r#"{"cmd":"request","scope":"root"}"#, "bad_request"),
            (r#"{"id":{"a":1},"cmd":"status"}"#, "bad_request"),
        ] {
            assert_eq!(code(line), want, "{line}");
        }
        let long = format!(
            r#"{{"cmd":"type","tab":1,"ref":"e1","text":"{}"}}"#,
            "x".repeat(MAX_TEXT + 1)
        );
        assert_eq!(code(&long), "too_large");
        // The id comes back with the failure, when one could be read.
        let (id, _) = parse_request(r#"{"id":"q1","cmd":"nope"}"#).unwrap_err();
        assert_eq!(id, json!("q1"));
    }

    #[test]
    fn agents_open_only_the_web() {
        assert_eq!(
            web_url("https://example.com/a?b").unwrap().as_str(),
            "https://example.com/a?b"
        );
        assert_eq!(
            web_url("example.com").unwrap().as_str(),
            "https://example.com/"
        );
        for refused in [
            "file:///etc/passwd",
            "javascript:alert(1)",
            "data:text/html,hi",
            "about:blank",
            "mailto:a@example.com",
            "ftp://example.com/",
            "http://[fe80::1%25en0]/",
            "some words",
            "blob:https://example.com/x",
        ] {
            let failure = web_url(refused).expect_err(refused);
            assert_eq!(failure.code, "refused", "{refused}: {failure:?}");
        }
        assert_eq!(web_url("  ").unwrap_err().code, "bad_request");
    }

    #[test]
    fn a_launch_by_launch_services_is_always_the_application() {
        let args = |list: &[&str]| -> Vec<OsString> { list.iter().map(OsString::from).collect() };
        // Launch Services launches through launchd: pid 1 is the parent.
        for list in [
            &["Syndeo"][..],
            &["Syndeo", "control", r#"{"cmd":"status"}"#],
            &["Syndeo", "mcp"],
            &["Syndeo", "-psn_0_12345"],
        ] {
            assert_eq!(mode(&args(list), 1), Mode::App, "{list:?}");
        }
        assert_eq!(
            mode(&args(&["Syndeo", "control", "{}"]), 4321),
            Mode::Control(vec!["{}".into()])
        );
        assert_eq!(
            mode(&args(&["Syndeo", "mcp", "--name", "x"]), 4321),
            Mode::Mcp(vec!["--name".into(), "x".into()])
        );
        assert_eq!(mode(&args(&["Syndeo"]), 4321), Mode::App);
        assert_eq!(mode(&args(&["Syndeo", "controls"]), 4321), Mode::App);
        assert_eq!(mode(&args(&["Syndeo", "MCP"]), 4321), Mode::App);
    }

    #[test]
    fn client_options_are_read_strictly() {
        let strings =
            |list: &[&str]| -> Vec<String> { list.iter().map(|s| s.to_string()).collect() };
        let options = client_options(
            &strings(&["--name", "ci", "--scopes", "read", r#"{"cmd":"tabs"}"#]),
            true,
        )
        .unwrap();
        assert_eq!(options.name.as_deref(), Some("ci"));
        assert_eq!(options.scopes, Some(Scopes::of(&[Scope::Read])));
        assert_eq!(options.request.as_deref(), Some(r#"{"cmd":"tabs"}"#));
        assert!(client_options(&strings(&["--scopes", "read,script"]), true).is_err());
        assert!(client_options(&strings(&["--name"]), true).is_err());
        assert!(client_options(&strings(&["--bogus"]), true).is_err());
        assert!(client_options(&strings(&["{}", "{}"]), true).is_err());
        assert!(client_options(&strings(&["{}"]), false).is_err());
        assert!(client_options(&strings(&["--help"]), false).unwrap().help);
    }

    fn proc(pid: u32, path: &str) -> Proc {
        Proc {
            pid,
            path: path.into(),
            started: (pid as u64, 0),
        }
    }

    #[test]
    fn the_agent_is_what_started_the_shells_and_the_client() {
        let own = "/Applications/Syndeo.app/Contents/MacOS/Syndeo";
        let chain = [
            proc(10, own),
            proc(9, "/bin/zsh"),
            proc(8, "/usr/bin/env"),
            proc(7, "/Users/me/.local/bin/claude"),
            proc(6, "/Applications/Terminal.app/Contents/MacOS/Terminal"),
        ];
        assert_eq!(root_of(&chain, own), 3);
        // A program that connects itself is the agent.
        assert_eq!(
            root_of(&[proc(5, "/usr/bin/python3"), proc(4, "/bin/zsh")], own),
            0
        );
        // Only shells up to launchd: the last of them.
        assert_eq!(
            root_of(
                &[proc(5, own), proc(4, "/bin/zsh"), proc(3, "/usr/bin/login")],
                own
            ),
            2
        );
        assert_eq!(root_of(&[], own), 0);
    }

    #[test]
    fn names_are_shown_on_one_short_line() {
        assert_eq!(display_name("Claude Code 2.0"), "Claude Code 2.0");
        assert_eq!(display_name("a\nb\u{7}\tc"), "a b c");
        assert_eq!(display_name(" \n "), "An unnamed program");
        let long = display_name(&"x".repeat(500));
        assert_eq!(long.chars().count(), MAX_NAME);
        assert!(long.ends_with('…'));
    }

    #[test]
    fn requests_are_limited_to_a_rate_with_bursts() {
        let start = Instant::now();
        let mut bucket = Bucket::new(start);
        let allowed = (0..200).filter(|_| bucket.take(start)).count();
        assert_eq!(allowed, BURST as usize, "a burst, and no more");
        assert!(!bucket.take(start));
        assert!(
            bucket.take(start + Duration::from_millis(100)),
            "earned back over time"
        );
        let mut slow = Bucket::new(start);
        for i in 0..1000u64 {
            assert!(
                slow.take(start + Duration::from_millis(i * 40)),
                "25 a second is within the rate"
            );
        }
    }

    #[test]
    fn lines_longer_than_allowed_are_read_past_and_refused() {
        let mut input = Cursor::new(b"short\r\n0123456789ABC\nnext\n\xff\xfe\nlast".to_vec());
        assert_eq!(
            read_line(&mut input, 10).unwrap(),
            Line::Text("short".into())
        );
        assert_eq!(read_line(&mut input, 10).unwrap(), Line::TooLong);
        assert_eq!(
            read_line(&mut input, 10).unwrap(),
            Line::Text("next".into())
        );
        assert_eq!(read_line(&mut input, 10).unwrap(), Line::NotText);
        assert_eq!(
            read_line(&mut input, 10).unwrap(),
            Line::Text("last".into())
        );
        assert_eq!(read_line(&mut input, 10).unwrap(), Line::End);
        // A long line in many small reads.
        let mut chunked =
            std::io::BufReader::with_capacity(3, Cursor::new(b"abcdefgh\nok\n".to_vec()));
        assert_eq!(read_line(&mut chunked, 5).unwrap(), Line::TooLong);
        assert_eq!(read_line(&mut chunked, 5).unwrap(), Line::Text("ok".into()));
    }

    #[test]
    fn the_socket_is_in_a_folder_of_its_own() {
        let home = Path::new("/Users/someone");
        assert_eq!(
            socket_path(home),
            Path::new("/Users/someone/Library/Application Support/Syndeo/agents/control.sock")
        );
        assert_eq!(socket_path(home).parent(), Some(socket_dir(home).as_path()));
    }

    /// The application, as the MCP server sees it: records what it was asked,
    /// and answers from a script.
    #[derive(Default)]
    struct Fake {
        asked: Vec<(String, Value)>,
        script_granted: bool,
    }

    impl Backend for Fake {
        fn call(&mut self, client: &str, request: Value) -> Result<Value, Failure> {
            self.asked.push((client.to_string(), request.clone()));
            match request["cmd"].as_str() {
                Some("eval") if !self.script_granted => {
                    Err(Failure::new("not_granted", "script was not allowed"))
                }
                Some("request") => {
                    self.script_granted = true;
                    Ok(json!({"granted": ["read", "navigate", "act", "script"]}))
                }
                Some("screenshot") => {
                    Ok(json!({"tab": 1, "png": "iVBORw0KGgo=", "width": 2, "height": 1}))
                }
                Some("open") => Ok(json!({"tab": 1, "title": "Example Domain"})),
                _ => Ok(json!({"done": true})),
            }
        }
    }

    fn rpc(mcp: &mut Mcp<Fake>, message: Value) -> Value {
        serde_json::from_str(&mcp.handle(&message.to_string()).expect("an answer")).unwrap()
    }

    #[test]
    fn mcp_initializes_lists_tools_and_calls_them_as_the_client_it_said_it_was() {
        let mut mcp = Mcp::new(Fake::default(), None);
        let init = rpc(
            &mut mcp,
            json!({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {
                "protocolVersion": "2025-03-26", "capabilities": {},
                "clientInfo": {"name": "claude-code", "version": "2.0.1"}}}),
        );
        assert_eq!(init["result"]["protocolVersion"], "2025-03-26");
        assert_eq!(init["result"]["serverInfo"]["name"], "syndeo");
        assert!(mcp
            .handle(r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#)
            .is_none());
        let list = rpc(
            &mut mcp,
            json!({"jsonrpc": "2.0", "id": 2, "method": "tools/list"}),
        );
        let names: Vec<&str> = list["result"]["tools"]
            .as_array()
            .unwrap()
            .iter()
            .map(|t| t["name"].as_str().unwrap())
            .collect();
        assert!(
            names.contains(&"syndeo_open") && names.contains(&"syndeo_eval"),
            "{names:?}"
        );
        for tool in list["result"]["tools"].as_array().unwrap() {
            assert_eq!(tool["inputSchema"]["type"], "object", "{tool}");
        }
        let open = rpc(
            &mut mcp,
            json!({"jsonrpc": "2.0", "id": 3, "method": "tools/call",
                   "params": {"name": "syndeo_open", "arguments": {"url": "https://example.com"}}}),
        );
        assert_eq!(open["result"]["isError"], false);
        assert!(open["result"]["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("Example Domain"));
        assert_eq!(
            mcp.backend().asked[0],
            (
                "claude-code 2.0.1".to_string(),
                json!({"cmd": "open", "url": "https://example.com"})
            )
        );
    }

    #[test]
    fn mcp_asks_for_script_on_its_own_the_first_time_and_only_then() {
        let mut mcp = Mcp::new(Fake::default(), Some("ci".into()));
        let call = |mcp: &mut Mcp<Fake>, id: u64| {
            rpc(
                mcp,
                json!({"jsonrpc": "2.0", "id": id, "method": "tools/call",
                       "params": {"name": "syndeo_eval", "arguments": {"tab": 1, "script": "return 1"}}}),
            )
        };
        assert_eq!(call(&mut mcp, 1)["result"]["isError"], false);
        let asked: Vec<String> = mcp
            .backend()
            .asked
            .iter()
            .map(|(_, r)| r["cmd"].as_str().unwrap().to_string())
            .collect();
        assert_eq!(asked, ["eval", "request", "eval"]);
        assert_eq!(
            mcp.backend().asked[1].1,
            json!({"cmd": "request", "scope": "script"})
        );
        assert_eq!(
            mcp.backend().asked[0].0,
            "ci",
            "the name given on the command line wins"
        );
        call(&mut mcp, 2);
        assert_eq!(mcp.backend().asked.len(), 4, "asked once only");
    }

    #[test]
    fn mcp_gives_screenshots_as_images_and_failures_as_errors() {
        let mut mcp = Mcp::new(Fake::default(), None);
        let shot = rpc(
            &mut mcp,
            json!({"jsonrpc": "2.0", "id": 1, "method": "tools/call",
                   "params": {"name": "syndeo_screenshot", "arguments": {"tab": 1}}}),
        );
        assert_eq!(shot["result"]["content"][0]["type"], "image");
        assert_eq!(shot["result"]["content"][0]["mimeType"], "image/png");
        assert!(!shot["result"]["content"][1]["text"]
            .as_str()
            .unwrap()
            .contains("iVBOR"));
        let navigate = rpc(
            &mut mcp,
            json!({"jsonrpc": "2.0", "id": 2, "method": "tools/call",
                   "params": {"name": "syndeo_navigate", "arguments": {"tab": 1, "action": "back"}}}),
        );
        assert_eq!(navigate["result"]["isError"], false);
        assert_eq!(
            mcp.backend().asked.last().unwrap().1,
            json!({"cmd": "back", "tab": 1})
        );
        for (params, code) in [
            (json!({"name": "syndeo_launch"}), -32602),
            (
                json!({"name": "syndeo_navigate", "arguments": {"tab": 1, "action": "home"}}),
                -32602,
            ),
            (
                json!({"name": "syndeo_read", "arguments": {"tab": 1, "cmd": "eval"}}),
                -32602,
            ),
            (json!({"name": "syndeo_read", "arguments": [1]}), -32602),
        ] {
            let answer = rpc(
                &mut mcp,
                json!({"jsonrpc": "2.0", "id": 9, "method": "tools/call", "params": params}),
            );
            assert_eq!(answer["error"]["code"], code, "{answer}");
        }
        assert_eq!(
            rpc(
                &mut mcp,
                json!({"jsonrpc": "2.0", "id": 4, "method": "nope"})
            )["error"]["code"],
            -32601
        );
        let bad: Value = serde_json::from_str(&mcp.handle("{oops").unwrap()).unwrap();
        assert_eq!(bad["error"]["code"], -32700);
        assert_eq!(
            rpc(
                &mut mcp,
                json!({"jsonrpc": "2.0", "id": 5, "method": "ping"})
            )["result"],
            json!({})
        );
    }
}
