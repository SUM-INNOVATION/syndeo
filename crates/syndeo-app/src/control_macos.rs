//! Agents, in the application: the socket they connect to, the person's
//! leave, their tabs, and what they do there. See `agent` for the protocol.
//!
//! - **Off unless the person turns it on**, in Syndeo > Agent Access…; the
//!   socket exists only while it is on. It is a Unix socket in a folder of
//!   its own, which only the person's account may enter (0700), and is
//!   itself 0600; no network port. Whoever connects, the kernel says which
//!   user it is, and anything but the person's own account is refused before
//!   it is read.
//! - **Nothing without the person's leave.** A program's first request
//!   raises Syndeo's prompt, saying who asks — the name it gives, which
//!   Syndeo cannot check, and the program that started it, with its
//!   signature — and what for. Allowed once, it holds for that connection;
//!   for the session, until Syndeo quits or the person takes it back.
//!   Running JavaScript is asked for, and allowed, on its own.
//! - **In the open.** An agent works only in tabs it opened, made as every
//!   window is (routed through the proxy and pinned to its authority), in
//!   windows of its own. Each says, below its title bar, who is in control,
//!   with Stop, which disconnects the agent and takes back everything it was
//!   allowed; its tabs become the person's. The person's own tabs are never
//!   shown to an agent.
//! - **Nothing else.** Settings, Clear Browsing Data, cookies, storage, the
//!   proxy's controls and the keystore have no request.
//!
//! The socket is served on threads of its own; everything to do with AppKit
//! and WebKit happens on the main thread, where each request is handed.

use crate::address;
use crate::agent::{self, Command, Failure, Proc, Request, Scope, Scopes, Step};
use crate::browser_macos::BrowserController;
use crate::peer_macos::{self, Peer};
use base64::Engine as _;
use objc2::rc::{Retained, Weak};
use objc2::runtime::{AnyObject, NSObject, NSObjectProtocol, ProtocolObject};
use objc2::{
    define_class, msg_send, sel, DeclaredClass, MainThreadMarker, MainThreadOnly, Message,
};
use objc2_app_kit::{
    NSAccessibility, NSAlert, NSAlertSecondButtonReturn, NSAlertStyle, NSAlertThirdButtonReturn,
    NSApplication, NSAutoresizingMaskOptions, NSBackingStoreType, NSBitmapImageFileType,
    NSBitmapImageRep, NSButton, NSColor, NSControlStateValueOff, NSControlStateValueOn, NSFont,
    NSImage, NSLayoutAttribute, NSPopUpButton, NSTextField, NSTitlebarAccessoryViewController,
    NSView, NSWindow, NSWindowOrderingMode, NSWindowStyleMask,
};
use objc2_foundation::{
    ns_string, NSActivityOptions, NSDictionary, NSError, NSPoint, NSProcessInfo, NSRect, NSSize,
    NSString, NSURLRequest, NSUserDefaults, NSURL,
};
use objc2_web_kit::{WKContentWorld, WKWebView};
use serde_json::{json, Value};
use std::cell::{Cell, RefCell};
use std::collections::{HashMap, VecDeque};
use std::io::{self, BufReader, Write};
use std::os::unix::fs::{DirBuilderExt, FileTypeExt, MetadataExt, PermissionsExt};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::mpsc::{channel, RecvTimeoutError, Sender};
use std::sync::Arc;
use std::time::{Duration, Instant};

/// What Syndeo does in a page for an agent, in its own content world.
const WORLD_JS: &str = include_str!("agent_world.js");
/// The name of that world.
const WORLD: &str = "SyndeoAgents";
/// The most connections at once.
const MAX_CONNECTIONS: usize = 16;
/// How long a request waits for the main thread: a prompt for the person,
/// or anything else.
const ASK_LIMIT: Duration = Duration::from_secs(600);
const ANSWER_LIMIT: Duration = Duration::from_secs(60);
/// After the person denies an agent, how long before it may ask again.
const AFTER_DENIAL: Duration = Duration::from_secs(10);

/// A browser window, made exactly as every other is, showing the start
/// page, with its controller.
pub type NewWindow = Box<dyn Fn() -> (Retained<NSWindow>, Retained<BrowserController>)>;
/// The controller of an open window.
pub type ControllerOf = Box<dyn Fn(&NSWindow) -> Option<Retained<BrowserController>>>;

/// What the application gives this module.
pub struct AppHooks {
    pub new_window: NewWindow,
    pub controller_of: ControllerOf,
}

/// An agent's tab, as a request finds it: its web view, its window's
/// controller, and its window.
type Found = (
    Retained<WKWebView>,
    Retained<BrowserController>,
    Retained<NSWindow>,
);

thread_local! {
    static CONTROL: RefCell<Option<Rc<Control>>> = const { RefCell::new(None) };
}

/// Run `f` with the control, on the main thread, if there is one.
fn with<R>(f: impl FnOnce(&Rc<Control>) -> R) -> Option<R> {
    let control = CONTROL.with(|c| c.borrow().clone());
    control.map(|control| f(&control))
}

/// Hand `job` to the main thread.
fn dispatch(job: Job) {
    dispatch2::DispatchQueue::main().exec_async(move || {
        with(|control| control.job(job));
    });
}

// ------------------------------------------------------------- the application's calls

/// Set up agents for this application: the socket, if the person turned
/// agents on before, and otherwise nothing, with any socket a previous run
/// left removed. The socket is under the person's home folder, as `HOME`
/// says, where a client looks for it.
pub fn install(mtm: MainThreadMarker, hooks: AppHooks) {
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_default();
    let own = std::env::current_exe()
        .ok()
        .and_then(|path| path.canonicalize().ok())
        .map(|path| path.to_string_lossy().into_owned())
        .unwrap_or_default();
    let control = Rc::new(Control {
        mtm,
        hooks,
        home: home.clone(),
        own,
        server: RefCell::new(None),
        conns: RefCell::new(HashMap::new()),
        agents: RefCell::new(HashMap::new()),
        tabs: RefCell::new(HashMap::new()),
        next_tab: Cell::new(1),
        next_agent: Cell::new(1),
        asking: Cell::new(false),
        waiting: RefCell::new(VecDeque::new()),
        denied: RefCell::new(HashMap::new()),
        activity: RefCell::new(None),
        access: RefCell::new(None),
        world: RefCell::new(None),
    });
    CONTROL.with(|c| *c.borrow_mut() = Some(control.clone()));
    if setting() {
        if let Err(err) = control.start() {
            tracing::error!("agents could not be allowed: {err}");
        }
    } else {
        remove_socket(&agent::socket_path(&home));
    }
}

/// The application is quitting: no more agents, and no socket left.
pub fn quit() {
    with(|control| control.stop_serving());
}

/// Syndeo > Agent Access….
pub fn show_access() {
    with(|control| control.show_access());
}

/// A window closed: if it was an agent's tab, it is no longer.
pub fn window_closed(window: &NSWindow) {
    with(|control| control.window_closed(window));
}

/// A page in `parent` opened `window` as a new tab. A tab opened from an
/// agent's tab is that agent's too.
pub fn opened_beside(parent: &NSWindow, window: &NSWindow) {
    with(|control| control.opened_beside(parent, window));
}

/// Whether `window` is one of an agent's tabs, which addresses the person
/// asks for do not open beside.
pub fn is_agents(window: &NSWindow) -> bool {
    with(|control| {
        control
            .tabs
            .borrow()
            .values()
            .any(|t| t.window.load().is_some_and(|w| std::ptr::eq(&*w, window)))
    })
    .unwrap_or(false)
}

/// Whether the person turned agents on.
fn setting() -> bool {
    NSUserDefaults::standardUserDefaults().boolForKey(&NSString::from_str(agent::SETTING_KEY))
}

fn set_setting(on: bool) {
    NSUserDefaults::standardUserDefaults()
        .setBool_forKey(on, &NSString::from_str(agent::SETTING_KEY));
}

// ------------------------------------------------------------- the socket

/// The socket, served while agents are allowed.
struct Server {
    stop: Arc<AtomicBool>,
    path: PathBuf,
}

/// The folder for the socket: made 0700 if missing; if there, required to
/// be a folder of this user's, and made 0700.
fn prepare_dir(dir: &Path) -> io::Result<()> {
    match std::fs::symlink_metadata(dir) {
        Err(e) if e.kind() == io::ErrorKind::NotFound => {
            std::fs::DirBuilder::new().mode(0o700).create(dir)?;
        }
        Err(e) => return Err(e),
        Ok(meta) => {
            if !meta.is_dir() || meta.uid() != unsafe { libc::geteuid() } {
                return Err(io::Error::other(format!(
                    "{} is not a folder of this user's",
                    dir.display()
                )));
            }
        }
    }
    std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))
}

/// Remove a socket left at `path`, and only a socket of this user's.
fn remove_socket(path: &Path) {
    if let Ok(meta) = std::fs::symlink_metadata(path) {
        if meta.file_type().is_socket() && meta.uid() == unsafe { libc::geteuid() } {
            let _ = std::fs::remove_file(path);
        }
    }
}

impl Server {
    fn start(path: PathBuf, own: String) -> io::Result<Server> {
        let dir = path.parent().ok_or_else(|| io::Error::other("no folder"))?;
        prepare_dir(dir)?;
        if std::fs::symlink_metadata(&path).is_ok() {
            remove_socket(&path);
            if std::fs::symlink_metadata(&path).is_ok() {
                return Err(io::Error::other(format!(
                    "{} is in the way, and is not Syndeo's socket",
                    path.display()
                )));
            }
        }
        let listener = UnixListener::bind(&path)?;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))?;
        listener.set_nonblocking(true)?;
        let stop = Arc::new(AtomicBool::new(false));
        let stopping = stop.clone();
        let live = Arc::new(AtomicUsize::new(0));
        std::thread::Builder::new()
            .name("agents".into())
            .spawn(move || accept(listener, stopping, live, own))?;
        Ok(Server { stop, path })
    }

    fn stop(&self) {
        self.stop.store(true, Ordering::SeqCst);
        remove_socket(&self.path);
    }
}

/// Take connections until told to stop, each served on a thread of its own.
fn accept(listener: UnixListener, stop: Arc<AtomicBool>, live: Arc<AtomicUsize>, own: String) {
    static NEXT: AtomicU64 = AtomicU64::new(1);
    while !stop.load(Ordering::SeqCst) {
        match listener.accept() {
            Ok((stream, _)) => {
                let _ = stream.set_nonblocking(false);
                if live.load(Ordering::SeqCst) >= MAX_CONNECTIONS {
                    let mut stream = stream;
                    let _ = write(
                        &mut stream,
                        &agent::err(
                            &Value::Null,
                            &Failure::new("busy", "too many agents are connected"),
                        ),
                    );
                    continue;
                }
                live.fetch_add(1, Ordering::SeqCst);
                let conn = NEXT.fetch_add(1, Ordering::SeqCst);
                let (live, own) = (live.clone(), own.clone());
                let spawned = std::thread::Builder::new()
                    .name("agent".into())
                    .spawn(move || {
                        serve(conn, stream, &own);
                        live.fetch_sub(1, Ordering::SeqCst);
                    });
                if spawned.is_err() {
                    tracing::warn!("an agent's connection could not be served");
                }
            }
            Err(e) if e.kind() == io::ErrorKind::WouldBlock => {
                std::thread::sleep(Duration::from_millis(50));
            }
            Err(_) => std::thread::sleep(Duration::from_millis(50)),
        }
    }
}

/// Write one answer line; false if the other end is gone.
fn write(stream: &mut UnixStream, value: &Value) -> bool {
    let mut line = value.to_string();
    line.push('\n');
    stream
        .write_all(line.as_bytes())
        .and_then(|_| stream.flush())
        .is_ok()
}

/// One connection, from its first byte to its last.
fn serve(conn: u64, stream: UnixStream, own: &str) {
    let Ok(mut writer) = stream.try_clone() else {
        return;
    };
    let own_uid = unsafe { libc::geteuid() };
    // Only this person's own programs: the kernel says whose the peer is,
    // and anyone else's is refused before a word of it is read.
    if peer_macos::uid_of(&stream).ok() != Some(own_uid) {
        tracing::warn!("refused an agent's connection from another user");
        let _ = write(
            &mut writer,
            &agent::err(
                &Value::Null,
                &Failure::new(
                    "other_user",
                    "only programs of this Mac user's own may control Syndeo",
                ),
            ),
        );
        return;
    }
    let Ok(pid) = peer_macos::pid_of(&stream) else {
        return;
    };
    let peer = peer_macos::describe(pid, own);
    let Ok(closer) = stream.try_clone() else {
        return;
    };
    dispatch(Job::Connected {
        conn,
        peer,
        stream: closer,
    });
    let mut reader = BufReader::new(stream);
    let mut bucket = agent::Bucket::new(Instant::now());
    loop {
        let line = match agent::read_line(&mut reader, agent::MAX_REQUEST) {
            Ok(agent::Line::Text(line)) => line,
            Ok(agent::Line::End) | Err(_) => break,
            Ok(agent::Line::TooLong) => {
                let failure = Failure::new("too_large", "that request is longer than Syndeo reads");
                if !write(&mut writer, &agent::err(&Value::Null, &failure)) {
                    break;
                }
                continue;
            }
            Ok(agent::Line::NotText) => {
                if !write(
                    &mut writer,
                    &agent::err(&Value::Null, &Failure::bad("not UTF-8")),
                ) {
                    break;
                }
                continue;
            }
        };
        if line.trim().is_empty() {
            continue;
        }
        let answer = if !bucket.take(Instant::now()) {
            agent::err(
                &Value::Null,
                &Failure::new("rate_limited", "too many requests; slow down"),
            )
        } else {
            match agent::parse_request(&line) {
                Err((id, failure)) => agent::err(&id, &failure),
                Ok(Request {
                    id,
                    command: Command::Bye,
                }) => {
                    let _ = write(&mut writer, &agent::ok(&id, json!({})));
                    break;
                }
                Ok(request) => perform(conn, request),
            }
        };
        if !write(&mut writer, &answer) {
            break;
        }
    }
    dispatch(Job::Closed { conn });
}

/// Do one request: on the main thread, then, for a navigation, wait here
/// for its page.
fn perform(conn: u64, request: Request) -> Value {
    let id = request.id.clone();
    if let Command::Wait { tab, timeout } = request.command {
        return match settle(conn, tab, Some(Scope::Read), timeout, Arrival::Now) {
            Ok(state) => agent::ok(&id, state),
            Err(failure) => agent::err(&id, &failure),
        };
    }
    let waits = request.command.waits();
    let limit = match request.command {
        Command::Hello { .. } | Command::Request { .. } => ASK_LIMIT,
        _ => ANSWER_LIMIT,
    };
    let (to, answered) = channel();
    dispatch(Job::Request {
        conn,
        request,
        reply: Reply { id: id.clone(), to },
    });
    let answer = match answered.recv_timeout(limit) {
        Ok(answer) => answer,
        Err(RecvTimeoutError::Timeout) => {
            return agent::err(
                &id,
                &Failure::new("timeout", "Syndeo did not answer in time"),
            )
        }
        Err(RecvTimeoutError::Disconnected) => {
            return agent::err(
                &id,
                &Failure::new("unavailable", "Syndeo is not taking requests"),
            )
        }
    };
    let (Some(timeout), Some(tab)) = (waits, answer["result"]["tab"].as_u64()) else {
        return answer;
    };
    if answer["ok"] != json!(true) {
        return answer;
    }
    // Toward the address asked for, unless it is the one already there;
    // Back, Forward and Reload toward whatever comes next.
    let target = answer["result"]["url"].as_str().map(str::to_string);
    let from = answer["result"]["from"].as_str();
    let arrival = match (target, from) {
        (Some(target), Some(from)) if target != from => Arrival::At(target),
        _ => Arrival::Next,
    };
    match settle(conn, tab, None, timeout, arrival) {
        Ok(state) => agent::ok(&id, state),
        Err(failure) => agent::err(&id, &failure),
    }
}

/// What a wait for a tab's page waits for.
enum Arrival {
    /// Only that it is not loading now.
    Now,
    /// A page at this address, just asked for, or whatever loads instead.
    /// A new tab's web view can take a second or more to start its web
    /// content process, and is not loading meanwhile: not loading counts
    /// only once it has been seen loading, or is at that address.
    At(String),
    /// Whatever loads next, just asked for: given a moment to start, since
    /// a page brought back from the back-forward cache may not be seen
    /// loading at all.
    Next,
}

/// Wait for a tab's page to finish loading, up to `timeout`, and say how it
/// is.
fn settle(
    conn: u64,
    tab: u64,
    needs: Option<Scope>,
    timeout: Duration,
    arrival: Arrival,
) -> Result<Value, Failure> {
    let start = Instant::now();
    let mut seen = false;
    loop {
        let mut state = probe(conn, tab, needs)?;
        let loading = state["loading"] == json!(true);
        seen |= loading;
        let arrived = match &arrival {
            Arrival::Now => true,
            Arrival::At(target) => seen || state["url"].as_str() == Some(target.as_str()),
            Arrival::Next => seen || start.elapsed() >= Duration::from_secs(1),
        };
        if !loading && arrived {
            // A failure page may follow a failed load at once.
            std::thread::sleep(Duration::from_millis(200));
            state = probe(conn, tab, needs)?;
            if state["loading"] != json!(true) {
                state["loaded"] = json!(true);
                return Ok(state);
            }
            seen = true;
        }
        if start.elapsed() >= timeout {
            state["loaded"] = json!(false);
            return Ok(state);
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

/// A tab's state, from the main thread.
fn probe(conn: u64, tab: u64, needs: Option<Scope>) -> Result<Value, Failure> {
    let (to, answered) = channel();
    dispatch(Job::Probe {
        conn,
        tab,
        needs,
        reply: to,
    });
    answered
        .recv_timeout(ANSWER_LIMIT)
        .unwrap_or_else(|_| Err(Failure::new("unavailable", "Syndeo is not taking requests")))
}

// ------------------------------------------------------------- on the main thread

/// Where an answer goes: back to the connection's thread.
struct Reply {
    id: Value,
    to: Sender<Value>,
}

impl Reply {
    fn done(self, result: Result<Value, Failure>) {
        let answer = match result {
            Ok(value) => agent::ok(&self.id, value),
            Err(failure) => agent::err(&self.id, &failure),
        };
        let _ = self.to.send(answer);
    }
}

enum Job {
    Connected {
        conn: u64,
        peer: Peer,
        stream: UnixStream,
    },
    Request {
        conn: u64,
        request: Request,
        reply: Reply,
    },
    Probe {
        conn: u64,
        tab: u64,
        needs: Option<Scope>,
        reply: Sender<Result<Value, Failure>>,
    },
    Closed {
        conn: u64,
    },
}

/// The agent: the process the person is asked about, which owns its tabs
/// and holds what was allowed for the session.
type AgentKey = Proc;

struct Conn {
    agent: AgentKey,
    stream: UnixStream,
    /// It said hello, and was allowed.
    allowed: bool,
    /// What was allowed for this connection only.
    once: Scopes,
}

struct Agent {
    /// The name it gives, as shown.
    name: String,
    /// Its number, for its Stop and its windows.
    number: u32,
    /// What was allowed until Syndeo quits.
    session: Scopes,
    peer: Peer,
}

struct Tab {
    agent: AgentKey,
    window: Weak<NSWindow>,
    controller: Retained<BrowserController>,
    view: Retained<WKWebView>,
    indicator: Retained<NSTitlebarAccessoryViewController>,
    _stop: Retained<StopTarget>,
}

/// A prompt waiting its turn.
struct Ask {
    conn: u64,
    scopes: Scopes,
    hello: bool,
    reply: Reply,
}

/// What the person answered.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Answer {
    Deny,
    Once,
    Session,
}

struct Control {
    mtm: MainThreadMarker,
    hooks: AppHooks,
    home: PathBuf,
    own: String,
    server: RefCell<Option<Server>>,
    conns: RefCell<HashMap<u64, Conn>>,
    agents: RefCell<HashMap<AgentKey, Agent>>,
    tabs: RefCell<HashMap<u64, Tab>>,
    next_tab: Cell<u64>,
    next_agent: Cell<u32>,
    /// A prompt is showing.
    asking: Cell<bool>,
    waiting: RefCell<VecDeque<Ask>>,
    denied: RefCell<HashMap<AgentKey, Instant>>,
    /// Keeps macOS from napping Syndeo while an agent is connected.
    activity: RefCell<Option<Retained<ProtocolObject<dyn NSObjectProtocol>>>>,
    access: RefCell<Option<Retained<AccessController>>>,
    /// Syndeo's own content world, kept: WebKit lets go of a named world,
    /// and of everything kept in it, once nothing holds it.
    world: RefCell<Option<Retained<WKContentWorld>>>,
}

impl Control {
    fn start(&self) -> io::Result<()> {
        if self.server.borrow().is_some() {
            return Ok(());
        }
        let server = Server::start(agent::socket_path(&self.home), self.own.clone())?;
        tracing::info!("agents are allowed to ask: the socket is up");
        *self.server.borrow_mut() = Some(server);
        Ok(())
    }

    /// No socket, no connection, and nothing allowed.
    fn stop_serving(&self) {
        if let Some(server) = self.server.borrow_mut().take() {
            server.stop();
            tracing::info!("agents are off: the socket is gone");
        }
        let keys: Vec<AgentKey> = self.agents.borrow().keys().cloned().collect();
        for key in keys {
            self.revoke(&key);
        }
        let conns: Vec<Conn> = self.conns.borrow_mut().drain().map(|(_, c)| c).collect();
        for conn in conns {
            let _ = conn.stream.shutdown(std::net::Shutdown::Both);
        }
        self.awake(false);
        self.refresh_access();
    }

    fn job(self: &Rc<Self>, job: Job) {
        match job {
            Job::Connected { conn, peer, stream } => self.connected(conn, peer, stream),
            Job::Request {
                conn,
                request,
                reply,
            } => self.request(conn, request, reply),
            Job::Probe {
                conn,
                tab,
                needs,
                reply,
            } => {
                let _ = reply.send(self.probe(conn, tab, needs));
            }
            Job::Closed { conn } => self.closed(conn),
        }
    }

    /// Keep macOS from napping Syndeo while an agent is connected, and let it
    /// once none is.
    fn awake(&self, on: bool) {
        let mut activity = self.activity.borrow_mut();
        let info = NSProcessInfo::processInfo();
        match (on, activity.is_some()) {
            (true, false) => {
                *activity = Some(info.beginActivityWithOptions_reason(
                    NSActivityOptions::UserInitiatedAllowingIdleSystemSleep,
                    ns_string!("An agent is driving Syndeo"),
                ));
            }
            (false, true) => {
                if let Some(token) = activity.take() {
                    unsafe { info.endActivity(&token) };
                }
            }
            _ => {}
        }
    }

    fn connected(&self, conn: u64, peer: Peer, stream: UnixStream) {
        let Some(key) = peer.agent().cloned() else {
            let _ = stream.shutdown(std::net::Shutdown::Both);
            return;
        };
        if self.server.borrow().is_none() {
            let _ = stream.shutdown(std::net::Shutdown::Both);
            return;
        }
        tracing::info!(conn, "an agent connected");
        let number = self.next_agent.get();
        let mut agents = self.agents.borrow_mut();
        let agent = agents.entry(key.clone()).or_insert_with(|| {
            self.next_agent.set(number + 1);
            Agent {
                name: agent::display_name(""),
                number,
                session: Scopes::NONE,
                peer: peer.clone(),
            }
        });
        agent.peer = peer;
        drop(agents);
        self.conns.borrow_mut().insert(
            conn,
            Conn {
                agent: key,
                stream,
                allowed: false,
                once: Scopes::NONE,
            },
        );
        self.awake(true);
    }

    fn closed(&self, conn: u64) {
        let Some(gone) = self.conns.borrow_mut().remove(&conn) else {
            return;
        };
        tracing::info!(conn, "an agent disconnected");
        self.forget_if_done(&gone.agent);
        if self.conns.borrow().is_empty() {
            self.awake(false);
        }
        self.refresh_access();
    }

    /// An agent with no connection and nothing allowed for the session no
    /// longer controls anything: its tabs become the person's.
    fn forget_if_done(&self, key: &AgentKey) {
        let connected = self.conns.borrow().values().any(|c| &c.agent == key);
        let session = self
            .agents
            .borrow()
            .get(key)
            .is_some_and(|a| !a.session.is_empty());
        if !connected && !session {
            self.release_tabs(key);
            self.agents.borrow_mut().remove(key);
        }
    }

    /// What `conn` may do: what was allowed for it, and for its agent's
    /// session.
    fn granted(&self, conn: &Conn) -> Scopes {
        let session = self
            .agents
            .borrow()
            .get(&conn.agent)
            .map_or(Scopes::NONE, |a| a.session);
        conn.once.union(session)
    }

    fn request(self: &Rc<Self>, conn: u64, request: Request, reply: Reply) {
        let command = request.command;
        let (key, allowed, granted) = {
            let conns = self.conns.borrow();
            let Some(c) = conns.get(&conn) else {
                return reply.done(Err(Failure::new(
                    "revoked",
                    "this connection is no longer allowed; the person stopped it, or turned agents off",
                )));
            };
            (c.agent.clone(), c.allowed, self.granted(c))
        };
        tracing::debug!(conn, command = command.name(), "an agent's request");
        match command {
            Command::Hello { client, scopes } => {
                if allowed {
                    return reply.done(Err(Failure::bad("hello was already said")));
                }
                if let Some(agent) = self.agents.borrow_mut().get_mut(&key) {
                    agent.name = agent::display_name(&client);
                }
                self.ask(Ask {
                    conn,
                    scopes,
                    hello: true,
                    reply,
                });
            }
            Command::Request { scope } => {
                if !allowed {
                    return reply.done(Err(not_allowed()));
                }
                self.ask(Ask {
                    conn,
                    scopes: Scopes::of(&[scope]),
                    hello: false,
                    reply,
                });
            }
            command => {
                if !allowed {
                    return reply.done(Err(not_allowed()));
                }
                if let Some(scope) = command.needs() {
                    if !granted.allows(scope) {
                        return reply.done(Err(Failure::new(
                            "not_granted",
                            format!(
                                "{} was not allowed; ask for it with {{\"cmd\": \"request\", \"scope\": \"{}\"}}",
                                scope.id(),
                                scope.id()
                            ),
                        )));
                    }
                }
                self.command(&key, granted, command, reply);
            }
        }
    }

    // ------------------------------------------------------------- the person's leave

    /// Ask the person, in turn: one prompt at a time. What is already
    /// allowed waits for no one's prompt.
    fn ask(self: &Rc<Self>, ask: Ask) {
        let already = self.conns.borrow().get(&ask.conn).map(|c| self.granted(c));
        if let Some(already) = already.filter(|a| a.covers(ask.scopes)) {
            return self.allowed(ask, already, "already");
        }
        self.waiting.borrow_mut().push_back(ask);
        if self.asking.get() {
            return;
        }
        loop {
            let Some(ask) = self.waiting.borrow_mut().pop_front() else {
                break;
            };
            self.asking.set(true);
            self.answer(ask);
            self.asking.set(false);
        }
    }

    fn answer(self: &Rc<Self>, ask: Ask) {
        let Some((key, already)) = self
            .conns
            .borrow()
            .get(&ask.conn)
            .map(|c| (c.agent.clone(), self.granted(c)))
        else {
            return ask
                .reply
                .done(Err(Failure::new("revoked", "this connection is gone")));
        };
        if already.covers(ask.scopes) {
            return self.allowed(ask, already, "already");
        }
        let recently = self
            .denied
            .borrow()
            .get(&key)
            .is_some_and(|at| at.elapsed() < AFTER_DENIAL);
        if recently {
            return ask.reply.done(Err(Failure::new(
                "denied",
                "the person refused this agent moments ago; wait before asking again",
            )));
        }
        let Some((name, peer)) = self
            .agents
            .borrow()
            .get(&key)
            .map(|a| (a.name.clone(), a.peer.clone()))
        else {
            return ask.reply.done(Err(Failure::new(
                "revoked",
                "this agent is no longer known",
            )));
        };
        let answer = self.prompt(&name, &peer, ask.scopes);
        // The person may have stopped it, or turned agents off, meanwhile.
        if !self.conns.borrow().contains_key(&ask.conn) {
            return ask
                .reply
                .done(Err(Failure::new("revoked", "this connection is gone")));
        }
        match answer {
            Answer::Deny => {
                tracing::info!(conn = ask.conn, "the person refused an agent");
                self.denied.borrow_mut().insert(key, Instant::now());
                ask.reply
                    .done(Err(Failure::new("denied", "the person did not allow it")));
            }
            Answer::Once => {
                tracing::info!(conn = ask.conn, scopes = ?ask.scopes.ids(), "the person allowed an agent, for its connection");
                if let Some(c) = self.conns.borrow_mut().get_mut(&ask.conn) {
                    c.once = c.once.union(ask.scopes);
                }
                let granted = self
                    .conns
                    .borrow()
                    .get(&ask.conn)
                    .map_or(Scopes::NONE, |c| self.granted(c));
                self.allowed(ask, granted, "connection");
            }
            Answer::Session => {
                tracing::info!(conn = ask.conn, scopes = ?ask.scopes.ids(), "the person allowed an agent, for the session");
                if let Some(agent) = self.agents.borrow_mut().get_mut(&key) {
                    agent.session = agent.session.union(ask.scopes);
                }
                let granted = self
                    .conns
                    .borrow()
                    .get(&ask.conn)
                    .map_or(Scopes::NONE, |c| self.granted(c));
                self.allowed(ask, granted, "session");
            }
        }
        self.refresh_access();
    }

    fn allowed(&self, ask: Ask, granted: Scopes, until: &str) {
        if ask.hello {
            if let Some(c) = self.conns.borrow_mut().get_mut(&ask.conn) {
                c.allowed = true;
            }
        }
        ask.reply
            .done(Ok(json!({"granted": granted.ids(), "until": until})));
    }

    /// Ask the person whether `name`, from `peer`, may have `scopes`.
    fn prompt(&self, name: &str, peer: &Peer, scopes: Scopes) -> Answer {
        let mtm = self.mtm;
        let script = scopes.allows(Scope::Script);
        let alert = NSAlert::new(mtm);
        alert.setAlertStyle(if script {
            NSAlertStyle::Critical
        } else {
            NSAlertStyle::Warning
        });
        let message = if script {
            format!("Allow “{name}” to run its own JavaScript in its tabs?")
        } else {
            format!("Allow “{name}” to control Syndeo?")
        };
        let program = peer
            .agent()
            .map(|p| format!("{} (process {})", p.path, p.pid))
            .unwrap_or_else(|| "unknown".into());
        let through = peer.through();
        let asks: Vec<String> = scopes
            .list()
            .iter()
            .map(|s| format!("• {}", s.describe()))
            .collect();
        let mut details = format!(
            "It calls itself “{name}”; Syndeo cannot check that.\n\nProgram: {program}\nSignature: {}\n",
            peer.signing
        );
        if !through.is_empty() {
            details.push_str(&format!("Connected through: {through}\n"));
        }
        details.push_str(&format!("\nIt asks to:\n{}\n\n", asks.join("\n")));
        if script {
            details.push_str(
                "Allow this only for a program you trust: JavaScript it sends acts with the page's \
                 own powers.",
            );
        } else {
            details.push_str(
                "It works only in tabs it opens, which say it is in control and offer Stop; your \
                 other tabs stay hidden from it. Its pages go through Syndeo as yours do.",
            );
        }
        alert.setMessageText(&NSString::from_str(&message));
        alert.setInformativeText(&NSString::from_str(&details));
        // Deny first: Return, and anything said without thinking, denies.
        for (title, id) in [
            ("Deny", "syndeo.agent-deny"),
            ("Allow Once", "syndeo.agent-allow-once"),
            ("Allow for This Session", "syndeo.agent-allow-session"),
        ] {
            let button = alert.addButtonWithTitle(&NSString::from_str(title));
            button.setAccessibilityIdentifier(Some(&NSString::from_str(id)));
        }
        #[allow(deprecated)] // `activate` is macOS 14 and later; this runs on 13.
        NSApplication::sharedApplication(mtm).activateIgnoringOtherApps(true);
        let response = alert.runModal();
        if response == NSAlertThirdButtonReturn {
            Answer::Session
        } else if response == NSAlertSecondButtonReturn {
            Answer::Once
        } else {
            Answer::Deny
        }
    }

    /// Take back everything an agent was allowed: its connections are closed,
    /// its session's leave is gone, and its tabs become the person's.
    fn revoke(&self, key: &AgentKey) {
        let closing: Vec<u64> = self
            .conns
            .borrow()
            .iter()
            .filter(|(_, c)| &c.agent == key)
            .map(|(&id, _)| id)
            .collect();
        for id in closing {
            if let Some(conn) = self.conns.borrow_mut().remove(&id) {
                let _ = conn.stream.shutdown(std::net::Shutdown::Both);
            }
        }
        self.release_tabs(key);
        self.agents.borrow_mut().remove(key);
        tracing::info!("an agent's access was taken back");
        if self.conns.borrow().is_empty() {
            self.awake(false);
        }
    }

    fn stop_agent(&self, number: u32) {
        let key = self
            .agents
            .borrow()
            .iter()
            .find(|(_, a)| a.number == number)
            .map(|(k, _)| k.clone());
        if let Some(key) = key {
            self.revoke(&key);
        }
        self.refresh_access();
    }

    // ------------------------------------------------------------- tabs

    /// The agent's tabs become the person's: no indicator, and out of its reach.
    fn release_tabs(&self, key: &AgentKey) {
        let released: Vec<Tab> = {
            let mut tabs = self.tabs.borrow_mut();
            let ids: Vec<u64> = tabs
                .iter()
                .filter(|(_, t)| &t.agent == key)
                .map(|(&id, _)| id)
                .collect();
            ids.into_iter().filter_map(|id| tabs.remove(&id)).collect()
        };
        for tab in released {
            tab.indicator.removeFromParentViewController();
        }
    }

    fn window_closed(&self, window: &NSWindow) {
        let gone: Vec<u64> = self
            .tabs
            .borrow()
            .iter()
            .filter(|(_, t)| t.window.load().is_none_or(|w| std::ptr::eq(&*w, window)))
            .map(|(&id, _)| id)
            .collect();
        for id in gone {
            self.tabs.borrow_mut().remove(&id);
        }
    }

    fn opened_beside(&self, parent: &NSWindow, window: &NSWindow) {
        let owner = self
            .tabs
            .borrow()
            .values()
            .find(|t| t.window.load().is_some_and(|w| std::ptr::eq(&*w, parent)))
            .map(|t| t.agent.clone());
        let Some(key) = owner else { return };
        let Some(controller) = (self.hooks.controller_of)(window) else {
            return;
        };
        let Some(view) = controller.web_view() else {
            return;
        };
        if let Some(number) = self.agents.borrow().get(&key).map(|a| a.number) {
            window.setTabbingIdentifier(&tabbing(number));
        }
        self.adopt(&key, window, controller, view);
    }

    /// Make `window` one of `key`'s tabs, saying so in it; its number.
    fn adopt(
        &self,
        key: &AgentKey,
        window: &NSWindow,
        controller: Retained<BrowserController>,
        view: Retained<WKWebView>,
    ) -> u64 {
        let (name, number) = self
            .agents
            .borrow()
            .get(key)
            .map(|a| (a.name.clone(), a.number))
            .unwrap_or_else(|| (agent::display_name(""), 0));
        let (indicator, stop) = indicator(self.mtm, &name, number);
        window.addTitlebarAccessoryViewController(&indicator);
        let id = self.next_tab.get();
        self.next_tab.set(id + 1);
        self.tabs.borrow_mut().insert(
            id,
            Tab {
                agent: key.clone(),
                window: Weak::from_retained(&window.retain()),
                controller,
                view,
                indicator,
                _stop: stop,
            },
        );
        id
    }

    /// An agent's tab, if it is still open and is the agent's.
    fn tab(&self, key: &AgentKey, id: u64) -> Result<Found, Failure> {
        let tabs = self.tabs.borrow();
        let found = tabs.get(&id).filter(|t| &t.agent == key).and_then(|t| {
            t.window
                .load()
                .map(|w| (t.view.clone(), t.controller.clone(), w))
        });
        found.ok_or_else(|| {
            Failure::new(
                "no_such_tab",
                format!("there is no tab {id} of yours; it may have been closed"),
            )
        })
    }

    /// The window an agent's newest tab is in, to open its next beside.
    fn window_of(&self, key: &AgentKey) -> Option<Retained<NSWindow>> {
        let tabs = self.tabs.borrow();
        let mut mine: Vec<(&u64, &Tab)> = tabs.iter().filter(|(_, t)| &t.agent == key).collect();
        mine.sort_by_key(|(id, _)| **id);
        mine.iter().rev().find_map(|(_, t)| t.window.load())
    }

    fn probe(&self, conn: u64, tab: u64, needs: Option<Scope>) -> Result<Value, Failure> {
        let (key, granted) = {
            let conns = self.conns.borrow();
            let c = conns
                .get(&conn)
                .ok_or_else(|| Failure::new("revoked", "this connection is no longer allowed"))?;
            if !c.allowed {
                return Err(not_allowed());
            }
            (c.agent.clone(), self.granted(c))
        };
        if let Some(scope) = needs {
            if !granted.allows(scope) {
                return Err(Failure::new(
                    "not_granted",
                    format!("{} was not allowed", scope.id()),
                ));
            }
        }
        let (view, _, _) = self.tab(&key, tab)?;
        Ok(state(tab, &view))
    }

    // ------------------------------------------------------------- requests

    fn command(&self, key: &AgentKey, granted: Scopes, command: Command, reply: Reply) {
        match command {
            Command::Status => {
                let name = self
                    .agents
                    .borrow()
                    .get(key)
                    .map(|a| a.name.clone())
                    .unwrap_or_default();
                let mut tabs: Vec<u64> = self
                    .tabs
                    .borrow()
                    .iter()
                    .filter(|(_, t)| &t.agent == key)
                    .map(|(&id, _)| id)
                    .collect();
                tabs.sort_unstable();
                reply.done(Ok(json!({
                    "version": env!("CARGO_PKG_VERSION"),
                    "agent": name,
                    "granted": granted.ids(),
                    "tabs": tabs,
                })));
            }
            Command::Tabs => {
                let mut list: Vec<(u64, Retained<WKWebView>)> = self
                    .tabs
                    .borrow()
                    .iter()
                    .filter(|(_, t)| &t.agent == key && t.window.load().is_some())
                    .map(|(&id, t)| (id, t.view.clone()))
                    .collect();
                list.sort_by_key(|(id, _)| *id);
                let tabs: Vec<Value> = list.iter().map(|(id, view)| state(*id, view)).collect();
                reply.done(Ok(json!({"tabs": tabs})));
            }
            Command::Open { url, .. } => reply.done(self.open(key, &url)),
            Command::Goto { tab, url, .. } => reply.done(self.goto(key, tab, &url)),
            Command::History { tab, step, .. } => reply.done(self.step(key, tab, step)),
            Command::Select { tab } => reply.done(self.tab(key, tab).map(|(view, _, window)| {
                window.makeKeyAndOrderFront(None);
                state(tab, &view)
            })),
            Command::Close { tab } => reply.done(self.tab(key, tab).map(|(_, _, window)| {
                window.performClose(None);
                json!({"tab": tab, "closed": true})
            })),
            Command::Read { tab, max_chars } => {
                self.in_world(key, tab, "read", json!({"max": max_chars}), reply)
            }
            Command::Snapshot { tab, max_elements } => {
                self.in_world(key, tab, "snapshot", json!({"max": max_elements}), reply)
            }
            Command::Click { tab, element } => {
                self.in_world(key, tab, "click", json!({"ref": element}), reply)
            }
            Command::Type {
                tab,
                element,
                text,
                submit,
            } => self.in_world(
                key,
                tab,
                "type",
                json!({"ref": element, "text": text, "submit": submit}),
                reply,
            ),
            Command::Scroll {
                tab,
                element,
                dx,
                dy,
            } => self.in_world(
                key,
                tab,
                "scroll",
                json!({"ref": element, "dx": dx, "dy": dy}),
                reply,
            ),
            Command::Screenshot { tab } => match self.tab(key, tab) {
                Ok((view, _, _)) => screenshot(tab, &view, reply),
                Err(failure) => reply.done(Err(failure)),
            },
            Command::Eval { tab, script } => match self.tab(key, tab) {
                Ok((view, _, _)) => {
                    let body = format!(
                        "const __syndeoValue = await (async () => {{\n{script}\n}})();\n\
                         return JSON.stringify(__syndeoValue === undefined ? null : __syndeoValue);"
                    );
                    let world = unsafe { WKContentWorld::pageWorld(self.mtm) };
                    run(&view, &world, &body, &[], move |result| {
                        reply.done(result.map(|text| {
                            let value: Value = serde_json::from_str(&text).unwrap_or(Value::Null);
                            json!({"tab": tab, "value": value})
                        }))
                    });
                }
                Err(failure) => reply.done(Err(failure)),
            },
            Command::Wait { .. }
            | Command::Hello { .. }
            | Command::Request { .. }
            | Command::Bye => reply.done(Err(Failure::bad("not here"))),
        }
    }

    /// Open `url` in a new tab of the agent's own: made as every window is,
    /// beside its other tabs, never among the person's.
    fn open(&self, key: &AgentKey, url: &str) -> Result<Value, Failure> {
        let request = web_request(url)?;
        let (window, controller) = (self.hooks.new_window)();
        // The tab browses in its window's own web view: routed through the
        // proxy and pinned to its authority, as every window's is.
        let Some(view) = controller.web_view() else {
            window.close();
            return Err(Failure::new(
                "unavailable",
                "Syndeo can't open websites right now; its windows say why",
            ));
        };
        let number = self.agents.borrow().get(key).map_or(0, |a| a.number);
        window.setTabbingIdentifier(&tabbing(number));
        match self.window_of(key) {
            Some(beside) => beside.addTabbedWindow_ordered(&window, NSWindowOrderingMode::Above),
            None => window.center(),
        }
        // In front, where the person can see it, without taking their keys.
        window.orderFront(None);
        let id = self.adopt(key, &window, controller.clone(), view.clone());
        controller.load(&request);
        tracing::info!(tab = id, "an agent opened a tab");
        Ok(json!({"tab": id, "url": request_url(&request), "from": ""}))
    }

    fn goto(&self, key: &AgentKey, tab: u64, url: &str) -> Result<Value, Failure> {
        let request = web_request(url)?;
        let (view, tab_controller, _) = self.tab(key, tab)?;
        let from = state(tab, &view)["url"].clone();
        tab_controller.load(&request);
        Ok(json!({"tab": tab, "url": request_url(&request), "from": from}))
    }

    /// Syndeo's own content world, made the first time.
    fn world(&self) -> Retained<WKContentWorld> {
        self.world
            .borrow_mut()
            .get_or_insert_with(|| unsafe {
                WKContentWorld::worldWithName(&NSString::from_str(WORLD), self.mtm)
            })
            .clone()
    }

    fn step(&self, key: &AgentKey, tab: u64, step: Step) -> Result<Value, Failure> {
        let (view, controller, _) = self.tab(key, tab)?;
        let nowhere = |which: &str| {
            Failure::new(
                "refused",
                format!("there is no page to go {which} to in tab {tab}"),
            )
        };
        match step {
            Step::Back if !unsafe { view.canGoBack() } => return Err(nowhere("back")),
            Step::Forward if !unsafe { view.canGoForward() } => return Err(nowhere("forward")),
            _ => {}
        }
        let none: Option<&AnyObject> = None;
        // The window's own Back, Forward, Reload and Stop, as its menu sends them.
        unsafe {
            match step {
                Step::Back => {
                    let () = msg_send![&*controller, syndeoBack: none];
                }
                Step::Forward => {
                    let () = msg_send![&*controller, syndeoForward: none];
                }
                Step::Reload => {
                    let () = msg_send![&*controller, reloadPage: none];
                }
                Step::Stop => {
                    let () = msg_send![&*controller, stopPage: none];
                }
            }
        }
        Ok(state(tab, &view))
    }

    /// One of Syndeo's own operations, in its own world in the tab's page.
    fn in_world(&self, key: &AgentKey, tab: u64, op: &'static str, args: Value, reply: Reply) {
        let view = match self.tab(key, tab) {
            Ok((view, _, _)) => view,
            Err(failure) => return reply.done(Err(failure)),
        };
        let world = self.world();
        let args = args.to_string();
        let route = route_of(&view);
        run(
            &view,
            &world,
            WORLD_JS,
            &[("op", op), ("a", &args)],
            move |result| {
                reply.done(result.and_then(|text| {
                    let mut value: Value = serde_json::from_str(&text).map_err(|_| {
                        Failure::new("page_error", "the page's answer could not be read")
                    })?;
                    if let Some(fields) = value.as_object_mut() {
                        fields.insert("tab".into(), json!(tab));
                        if op == "read" || op == "snapshot" {
                            fields.insert("route".into(), json!(route));
                        }
                    }
                    Ok(value)
                }))
            },
        );
    }

    // ------------------------------------------------------------- Agent Access

    fn show_access(self: &Rc<Self>) {
        let existing = self.access.borrow().clone();
        let access = existing.unwrap_or_else(|| {
            let made = AccessController::new(self.mtm, &self.own);
            *self.access.borrow_mut() = Some(made.clone());
            made
        });
        self.refresh_access();
        let window = access.ivars().window.borrow().clone();
        if let Some(window) = window {
            window.makeKeyAndOrderFront(None);
        }
    }

    /// Turn agents on or off, as the person chose.
    fn switch(&self, on: bool) {
        if on {
            match self.start() {
                Ok(()) => set_setting(true),
                Err(err) => {
                    tracing::error!("agents could not be allowed: {err}");
                    set_setting(false);
                    self.refresh_access();
                    if let Some(access) = self.access.borrow().as_ref() {
                        access.say(&format!("Agents could not be turned on: {err}"));
                    }
                    return;
                }
            }
        } else {
            set_setting(false);
            self.stop_serving();
        }
        self.refresh_access();
    }

    fn refresh_access(&self) {
        let Some(access) = self.access.borrow().clone() else {
            return;
        };
        let on = self.server.borrow().is_some();
        let mut lines = Vec::new();
        let mut choices = Vec::new();
        {
            let agents = self.agents.borrow();
            let conns = self.conns.borrow();
            let tabs = self.tabs.borrow();
            let mut list: Vec<(&AgentKey, &Agent)> = agents.iter().collect();
            list.sort_by_key(|(_, a)| a.number);
            for (key, agent) in list {
                let once = conns
                    .values()
                    .filter(|c| &c.agent == key && c.allowed)
                    .fold(Scopes::NONE, |all, c| all.union(c.once));
                if agent.session.is_empty() && once.is_empty() {
                    continue;
                }
                let count = tabs.values().filter(|t| &t.agent == key).count();
                let mut held = Vec::new();
                if !agent.session.is_empty() {
                    held.push(format!(
                        "{} for this session",
                        agent.session.ids().join(", ")
                    ));
                }
                if !once.is_empty() {
                    held.push(format!("{} for its connection", once.ids().join(", ")));
                }
                lines.push(format!(
                    "“{}” — {}; {count} tab{}",
                    agent.name,
                    held.join("; "),
                    if count == 1 { "" } else { "s" }
                ));
                choices.push((agent.number, agent.name.clone()));
            }
        }
        access.show_state(on, &lines, &choices);
    }
}

fn not_allowed() -> Failure {
    Failure::new(
        "not_approved",
        "say hello first, and wait for the person to allow it",
    )
}

/// The tabbing identifier of agent `number`'s windows: theirs alone, so
/// that macOS never puts an agent's tabs among the person's.
fn tabbing(number: u32) -> Retained<NSString> {
    NSString::from_str(&format!("com.sum.syndeo.agent.{number}"))
}

/// A request for an address an agent may open.
fn web_request(text: &str) -> Result<Retained<NSURLRequest>, Failure> {
    let url = agent::web_url(text)?;
    let nsurl = NSURL::URLWithString(&NSString::from_str(url.as_str()))
        .ok_or_else(|| Failure::new("refused", "that address could not be read"))?;
    Ok(NSURLRequest::requestWithURL(&nsurl))
}

fn request_url(request: &NSURLRequest) -> String {
    request
        .URL()
        .and_then(|url| url.absoluteString())
        .map(|s| s.to_string())
        .unwrap_or_default()
}

fn route_of(view: &WKWebView) -> Option<&'static str> {
    let url = unsafe { view.URL() }
        .and_then(|u| u.absoluteString())?
        .to_string();
    address::route_of(&url).map(address::Route::label)
}

/// How a tab is: its address, title, whether it is loading, and how its
/// page was reached, as its bar says.
fn state(tab: u64, view: &WKWebView) -> Value {
    let url = unsafe { view.URL() }
        .and_then(|u| u.absoluteString())
        .map(|s| s.to_string())
        .unwrap_or_default();
    let title = unsafe { view.title() }
        .map(|s| s.to_string())
        .unwrap_or_default();
    json!({
        "tab": tab,
        "url": url,
        "title": title,
        "loading": unsafe { view.isLoading() },
        "route": route_of(view),
    })
}

/// Run `body`, a function body, in `world` in the view's main frame, with
/// `args` as its string arguments, and hand what it returned, a string, to
/// `done`.
fn run(
    view: &WKWebView,
    world: &WKContentWorld,
    body: &str,
    args: &[(&str, &str)],
    done: impl FnOnce(Result<String, Failure>) + 'static,
) {
    let keys: Vec<Retained<NSString>> = args.iter().map(|(k, _)| NSString::from_str(k)).collect();
    let values: Vec<Retained<NSString>> = args.iter().map(|(_, v)| NSString::from_str(v)).collect();
    let key_refs: Vec<&NSString> = keys.iter().map(|k| &**k).collect();
    let value_refs: Vec<&AnyObject> = values.iter().map(|v| &***v as &AnyObject).collect();
    let arguments: Retained<NSDictionary<NSString, AnyObject>> =
        NSDictionary::from_slices(&key_refs, &value_refs);
    let done = Cell::new(Some(done));
    let handler = block2::RcBlock::new(move |result: *mut AnyObject, error: *mut NSError| {
        let Some(done) = done.take() else { return };
        if let Some(error) = unsafe { error.as_ref() } {
            let message = error
                .userInfo()
                .objectForKey(ns_string!("WKJavaScriptExceptionMessage"))
                .and_then(|m| m.downcast::<NSString>().ok())
                .map(|m| m.to_string())
                .unwrap_or_else(|| error.localizedDescription().to_string());
            return done(Err(Failure::new("page_error", message)));
        }
        let text = unsafe { result.as_ref() }
            .and_then(|r| r.downcast_ref::<NSString>())
            .map(|s| s.to_string())
            .unwrap_or_else(|| "null".into());
        if text.len() > agent::MAX_ANSWER / 2 {
            return done(Err(Failure::new(
                "too_large",
                "the page's answer is too large",
            )));
        }
        done(Ok(text));
    });
    unsafe {
        view.callAsyncJavaScript_arguments_inFrame_inContentWorld_completionHandler(
            &NSString::from_str(body),
            Some(&arguments),
            None,
            world,
            Some(&handler),
        )
    };
}

/// A picture of the tab's page, as PNG.
fn screenshot(tab: u64, view: &WKWebView, reply: Reply) {
    let reply = Cell::new(Some(reply));
    let handler = block2::RcBlock::new(move |image: *mut NSImage, error: *mut NSError| {
        let Some(reply) = reply.take() else { return };
        let png = unsafe { image.as_ref() }.and_then(|image| {
            let tiff = image.TIFFRepresentation()?;
            let rep = NSBitmapImageRep::imageRepWithData(&tiff)?;
            let png = unsafe {
                rep.representationUsingType_properties(
                    NSBitmapImageFileType::PNG,
                    &NSDictionary::new(),
                )
            }?;
            Some((png.to_vec(), rep.pixelsWide(), rep.pixelsHigh()))
        });
        match png {
            Some((bytes, width, height)) => reply.done(Ok(json!({
                "tab": tab,
                "png": base64::engine::general_purpose::STANDARD.encode(bytes),
                "width": width,
                "height": height,
            }))),
            None => {
                let why = unsafe { error.as_ref() }
                    .map(|e| e.localizedDescription().to_string())
                    .unwrap_or_else(|| "WebKit gave no picture".into());
                reply.done(Err(Failure::new("page_error", why)));
            }
        }
    });
    unsafe { view.takeSnapshotWithConfiguration_completionHandler(None, &handler) };
}

// ------------------------------------------------------------- what the person sees

/// The bar below an agent's tab's title bar: who is in control, and Stop.
fn indicator(
    mtm: MainThreadMarker,
    name: &str,
    number: u32,
) -> (
    Retained<NSTitlebarAccessoryViewController>,
    Retained<StopTarget>,
) {
    let width = 600.0;
    let view = NSView::initWithFrame(
        NSView::alloc(mtm),
        NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(width, 30.0)),
    );
    let label = NSTextField::labelWithString(
        &NSString::from_str(&format!(
            "Controlled by “{name}”. It works only in its own tabs, and its pages go through \
             Syndeo as yours do."
        )),
        mtm,
    );
    label.setFrame(NSRect::new(
        NSPoint::new(12.0, 7.0),
        NSSize::new(width - 110.0, 16.0),
    ));
    label.setAutoresizingMask(NSAutoresizingMaskOptions::ViewWidthSizable);
    label.setFont(Some(&NSFont::boldSystemFontOfSize(12.0)));
    label.setTextColor(Some(&NSColor::systemPurpleColor()));
    label.setAccessibilityIdentifier(Some(ns_string!("syndeo.agent-indicator")));
    let target = StopTarget::new(mtm, number);
    let stop = unsafe {
        NSButton::buttonWithTitle_target_action(
            ns_string!("Stop"),
            Some(&target),
            Some(sel!(stopAgent:)),
            mtm,
        )
    };
    stop.setFrame(NSRect::new(
        NSPoint::new(width - 88.0, 2.0),
        NSSize::new(80.0, 26.0),
    ));
    stop.setAutoresizingMask(NSAutoresizingMaskOptions::ViewMinXMargin);
    stop.setToolTip(Some(ns_string!(
        "Stop this agent: disconnect it, and take back everything it was allowed. Its tabs become yours."
    )));
    stop.setAccessibilityIdentifier(Some(ns_string!("syndeo.agent-stop")));
    view.addSubview(&label);
    view.addSubview(&stop);
    let accessory = NSTitlebarAccessoryViewController::new(mtm);
    accessory.setView(&view);
    accessory.setLayoutAttribute(NSLayoutAttribute::Bottom);
    (accessory, target)
}

pub struct StopIvars {
    number: u32,
}

define_class!(
    /// What an agent's Stop sends to.
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "SyndeoAgentStop"]
    #[ivars = StopIvars]
    pub struct StopTarget;

    unsafe impl NSObjectProtocol for StopTarget {}

    impl StopTarget {
        #[unsafe(method(stopAgent:))]
        fn stop_agent(&self, _sender: Option<&AnyObject>) {
            let number = self.ivars().number;
            // Once this action has returned: stopping lets go of this target,
            // and of the button that sent it.
            dispatch2::DispatchQueue::main().exec_async(move || {
                with(|control| control.stop_agent(number));
            });
        }
    }
);

impl StopTarget {
    fn new(mtm: MainThreadMarker, number: u32) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(StopIvars { number });
        unsafe { msg_send![super(this), init] }
    }
}

pub struct AccessIvars {
    window: RefCell<Option<Retained<NSWindow>>>,
    switch: RefCell<Option<Retained<NSButton>>>,
    status: RefCell<Option<Retained<NSTextField>>>,
    grants: RefCell<Option<Retained<NSTextField>>>,
    list: RefCell<Option<Retained<NSPopUpButton>>>,
    /// The agent each of the list's items is, by number.
    numbers: RefCell<Vec<u32>>,
}

define_class!(
    /// Syndeo > Agent Access…: agents on or off, and what each is allowed.
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "SyndeoAgentAccess"]
    #[ivars = AccessIvars]
    pub struct AccessController;

    unsafe impl NSObjectProtocol for AccessController {}

    impl AccessController {
        #[unsafe(method(agentsSwitched:))]
        fn agents_switched(&self, _sender: Option<&AnyObject>) {
            let on = self.ivars().switch.borrow().as_ref().is_some_and(|s| s.state() == NSControlStateValueOn);
            with(|control| control.switch(on));
        }

        #[unsafe(method(revokeChosen:))]
        fn revoke_chosen(&self, _sender: Option<&AnyObject>) {
            let index = self.ivars().list.borrow().as_ref().map_or(-1, |l| l.indexOfSelectedItem());
            let number = usize::try_from(index).ok().and_then(|i| self.ivars().numbers.borrow().get(i).copied());
            if let Some(number) = number {
                with(|control| control.stop_agent(number));
            }
        }

        #[unsafe(method(revokeAll:))]
        fn revoke_all(&self, _sender: Option<&AnyObject>) {
            with(|control| {
                let numbers: Vec<u32> = control.agents.borrow().values().map(|a| a.number).collect();
                for number in numbers {
                    control.stop_agent(number);
                }
            });
        }
    }
);

impl AccessController {
    fn new(mtm: MainThreadMarker, own: &str) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(AccessIvars {
            window: RefCell::new(None),
            switch: RefCell::new(None),
            status: RefCell::new(None),
            grants: RefCell::new(None),
            list: RefCell::new(None),
            numbers: RefCell::new(Vec::new()),
        });
        let this: Retained<Self> = unsafe { msg_send![super(this), init] };
        this.make_window(own);
        this
    }

    fn make_window(&self, own: &str) {
        let mtm = self.mtm();
        let target: &AnyObject = self.as_ref();
        let (width, height) = (560.0, 380.0);
        let window = unsafe {
            NSWindow::initWithContentRect_styleMask_backing_defer(
                NSWindow::alloc(mtm),
                NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(width, height)),
                NSWindowStyleMask::Titled | NSWindowStyleMask::Closable,
                NSBackingStoreType::Buffered,
                false,
            )
        };
        unsafe { window.setReleasedWhenClosed(false) };
        window.setTitle(ns_string!("Agent Access"));
        let content = NSView::initWithFrame(
            NSView::alloc(mtm),
            NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(width, height)),
        );
        let switch = unsafe {
            NSButton::checkboxWithTitle_target_action(
                ns_string!("Allow agents to control Syndeo"),
                Some(target),
                Some(sel!(agentsSwitched:)),
                mtm,
            )
        };
        switch.setFrame(NSRect::new(
            NSPoint::new(18.0, height - 44.0),
            NSSize::new(400.0, 24.0),
        ));
        switch.setAccessibilityIdentifier(Some(ns_string!("syndeo.agent-switch")));
        let program = if own.is_empty() {
            "Syndeo's own command"
        } else {
            own
        };
        let about = NSTextField::wrappingLabelWithString(
            &NSString::from_str(&format!(
                "Programs on this Mac — command-line agents, scripts and tests — can then ask to \
                 drive Syndeo, through Syndeo's own command:\n{program} mcp   (an MCP server)\n\
                 {program} control   (one request, or a session)\n\nEach program is asked about \
                 first, and works only in tabs of its own, which say so and offer Stop. Your tabs \
                 stay hidden from it. What you allow lasts until Syndeo quits, or until you take \
                 it back here or with Stop."
            )),
            mtm,
        );
        about.setFrame(NSRect::new(
            NSPoint::new(20.0, height - 190.0),
            NSSize::new(width - 40.0, 140.0),
        ));
        about.setSelectable(true);
        let status = NSTextField::wrappingLabelWithString(ns_string!(""), mtm);
        status.setFrame(NSRect::new(
            NSPoint::new(20.0, height - 222.0),
            NSSize::new(width - 40.0, 22.0),
        ));
        status.setAccessibilityIdentifier(Some(ns_string!("syndeo.agent-status")));
        let grants = NSTextField::wrappingLabelWithString(ns_string!(""), mtm);
        grants.setFrame(NSRect::new(
            NSPoint::new(20.0, 64.0),
            NSSize::new(width - 40.0, height - 222.0 - 64.0 - 8.0),
        ));
        grants.setAccessibilityIdentifier(Some(ns_string!("syndeo.agent-grants")));
        let list = NSPopUpButton::initWithFrame_pullsDown(
            NSPopUpButton::alloc(mtm),
            NSRect::new(NSPoint::new(16.0, 18.0), NSSize::new(250.0, 28.0)),
            false,
        );
        list.setAccessibilityIdentifier(Some(ns_string!("syndeo.agent-list")));
        let revoke = unsafe {
            NSButton::buttonWithTitle_target_action(
                ns_string!("Revoke"),
                Some(target),
                Some(sel!(revokeChosen:)),
                mtm,
            )
        };
        revoke.setFrame(NSRect::new(
            NSPoint::new(272.0, 16.0),
            NSSize::new(110.0, 32.0),
        ));
        revoke.setAccessibilityIdentifier(Some(ns_string!("syndeo.agent-revoke")));
        let revoke_all = unsafe {
            NSButton::buttonWithTitle_target_action(
                ns_string!("Revoke All"),
                Some(target),
                Some(sel!(revokeAll:)),
                mtm,
            )
        };
        revoke_all.setFrame(NSRect::new(
            NSPoint::new(388.0, 16.0),
            NSSize::new(150.0, 32.0),
        ));
        revoke_all.setAccessibilityIdentifier(Some(ns_string!("syndeo.agent-revoke-all")));
        for view in [
            &*switch as &NSView,
            &*about,
            &*status,
            &*grants,
            &*list,
            &*revoke,
            &*revoke_all,
        ] {
            content.addSubview(view);
        }
        window.setContentView(Some(&content));
        window.center();
        *self.ivars().window.borrow_mut() = Some(window);
        *self.ivars().switch.borrow_mut() = Some(switch);
        *self.ivars().status.borrow_mut() = Some(status);
        *self.ivars().grants.borrow_mut() = Some(grants);
        *self.ivars().list.borrow_mut() = Some(list);
    }

    fn say(&self, text: &str) {
        if let Some(status) = self.ivars().status.borrow().as_ref() {
            status.setStringValue(&NSString::from_str(text));
        }
    }

    fn show_state(&self, on: bool, lines: &[String], choices: &[(u32, String)]) {
        if let Some(switch) = self.ivars().switch.borrow().as_ref() {
            switch.setState(if on {
                NSControlStateValueOn
            } else {
                NSControlStateValueOff
            });
        }
        self.say(if on {
            "On: programs on this Mac can ask to control Syndeo."
        } else {
            "Off: no program can reach Syndeo."
        });
        if let Some(grants) = self.ivars().grants.borrow().as_ref() {
            let text = if lines.is_empty() {
                "No agent is allowed anything.".to_string()
            } else {
                format!("Allowed now:\n{}", lines.join("\n"))
            };
            grants.setStringValue(&NSString::from_str(&text));
        }
        if let Some(list) = self.ivars().list.borrow().as_ref() {
            list.removeAllItems();
            for (number, name) in choices {
                list.addItemWithTitle(&NSString::from_str(&format!("{number}. {name}")));
            }
            list.setEnabled(!choices.is_empty());
        }
        *self.ivars().numbers.borrow_mut() = choices.iter().map(|(n, _)| *n).collect();
    }
}
