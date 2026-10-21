//! The client: `Syndeo control` and `Syndeo mcp`, run by an agent.
//!
//! Syndeo's own executable, run by another program rather than launched as
//! an application (see `agent::mode`). It never starts the application or
//! anything of AppKit: it connects to the socket the running application
//! listens on while agents are allowed, says hello, and passes requests and
//! answers along. Everything it is allowed is decided in the application, by
//! the person.

use crate::agent::{
    self, client_options, display_name, read_line, Backend, Failure, Line, Mcp, Scopes,
    DEFAULT_SCOPES,
};
use serde_json::{json, Value};
use std::io::{self, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};

/// Exit statuses.
const DONE: i32 = 0;
const FAILED: i32 = 1;
const USAGE: i32 = 2;
const UNREACHABLE: i32 = 3;

/// What a client says when Syndeo cannot be reached.
const OFF: &str = "Syndeo is not running, or agents are not allowed: open Syndeo, choose \
    Syndeo > Agent Access…, and turn on Allow agents to control Syndeo";

/// A connection to the application.
pub struct Connection {
    writer: UnixStream,
    reader: BufReader<UnixStream>,
    next: u64,
}

impl Connection {
    /// Connect to the socket at `path`. Not there, or nobody listening, is
    /// the application not running or agents not allowed.
    pub fn open(path: &Path) -> Result<Connection, Failure> {
        let stream = UnixStream::connect(path).map_err(|e| match e.kind() {
            io::ErrorKind::NotFound | io::ErrorKind::ConnectionRefused => Failure::new("off", OFF),
            _ => Failure::new("off", format!("{OFF} ({e})")),
        })?;
        let reader = BufReader::new(
            stream
                .try_clone()
                .map_err(|e| Failure::new("off", e.to_string()))?,
        );
        Ok(Connection {
            writer: stream,
            reader,
            next: 1,
        })
    }

    /// Send `request`, with an id of this connection's if it has none, and
    /// read its answer, whole.
    pub fn send(&mut self, mut request: Value) -> Result<Value, Failure> {
        if let Some(fields) = request.as_object_mut() {
            if !fields.contains_key("id") {
                fields.insert("id".into(), json!(self.next));
                self.next += 1;
            }
        }
        let gone =
            |e: io::Error| Failure::new("closed", format!("Syndeo closed the connection: {e}"));
        let mut line = request.to_string();
        line.push('\n');
        self.writer.write_all(line.as_bytes()).map_err(gone)?;
        match read_line(&mut self.reader, agent::MAX_ANSWER).map_err(gone)? {
            Line::Text(text) => serde_json::from_str(&text)
                .map_err(|e| Failure::new("closed", format!("Syndeo's answer was not JSON: {e}"))),
            Line::End => Err(Failure::new(
                "closed",
                "Syndeo closed the connection: the person stopped this agent, or turned agents off",
            )),
            Line::TooLong | Line::NotText => {
                Err(Failure::new("closed", "Syndeo's answer could not be read"))
            }
        }
    }

    /// Send `request`, and take its result or its failure.
    pub fn call(&mut self, request: Value) -> Result<Value, Failure> {
        outcome(&self.send(request)?)
    }
}

/// An answer's result, or its failure.
fn outcome(answer: &Value) -> Result<Value, Failure> {
    if answer["ok"] == json!(true) {
        return Ok(answer["result"].clone());
    }
    let code = match answer["error"]["code"].as_str() {
        // The codes the application gives, kept as they are.
        Some(code) => known_code(code),
        None => "closed",
    };
    let message = answer["error"]["message"]
        .as_str()
        .unwrap_or("Syndeo refused")
        .to_string();
    Err(Failure::new(code, message))
}

/// `Failure` holds a static code: the application's own, or `failed`.
fn known_code(code: &str) -> &'static str {
    const CODES: [&str; 18] = [
        "bad_request",
        "unknown_command",
        "not_approved",
        "denied",
        "not_granted",
        "revoked",
        "no_such_tab",
        "refused",
        "busy",
        "rate_limited",
        "too_large",
        "timeout",
        "page_error",
        "unavailable",
        "other_user",
        "off",
        "closed",
        "not_found",
    ];
    CODES.into_iter().find(|&c| c == code).unwrap_or("failed")
}

/// Where the socket is, for this user.
fn socket() -> Result<PathBuf, Failure> {
    let home = std::env::var_os("HOME").ok_or_else(|| Failure::new("off", "HOME is not set"))?;
    Ok(agent::socket_path(Path::new(&home)))
}

/// Connect, and say hello as `name` asking for `scopes`.
fn connect(name: &str, scopes: Scopes) -> Result<(Connection, Value), (i32, Value)> {
    let fail = |status: i32, failure: &Failure| (status, agent::err(&Value::Null, failure));
    let path = socket().map_err(|f| fail(UNREACHABLE, &f))?;
    let mut connection = Connection::open(&path).map_err(|f| fail(UNREACHABLE, &f))?;
    let hello = connection
        .send(json!({"cmd": "hello", "client": name, "scopes": scopes.ids()}))
        .map_err(|f| fail(UNREACHABLE, &f))?;
    if hello["ok"] != json!(true) {
        return Err((FAILED, hello));
    }
    Ok((connection, hello))
}

fn print_line(value: &Value) -> io::Result<()> {
    let mut out = io::stdout().lock();
    writeln!(out, "{value}")?;
    out.flush()
}

/// `Syndeo control`. Returns the exit status.
pub fn control(args: Vec<String>) -> i32 {
    let options = match client_options(&args, true) {
        Ok(options) => options,
        Err(why) => {
            eprintln!("Syndeo control: {why}\n\n{}", agent::CONTROL_USAGE);
            return USAGE;
        }
    };
    if options.help {
        print!("{}", agent::CONTROL_USAGE);
        return DONE;
    }
    let name = display_name(options.name.as_deref().unwrap_or("Syndeo control"));
    let scopes = options.scopes.unwrap_or(DEFAULT_SCOPES);
    let one = match options
        .request
        .as_deref()
        .map(serde_json::from_str::<Value>)
    {
        None => None,
        Some(Ok(request)) if request.is_object() => Some(request),
        Some(_) => {
            eprintln!(
                "Syndeo control: REQUEST is one JSON object\n\n{}",
                agent::CONTROL_USAGE
            );
            return USAGE;
        }
    };
    let (mut connection, hello) = match connect(&name, scopes) {
        Ok(connected) => connected,
        Err((status, answer)) => {
            let _ = print_line(&answer);
            return status;
        }
    };
    if let Some(request) = one {
        return match connection.send(request) {
            Ok(answer) => {
                let _ = print_line(&answer);
                if answer["ok"] == json!(true) {
                    DONE
                } else {
                    FAILED
                }
            }
            Err(failure) => {
                let _ = print_line(&agent::err(&Value::Null, &failure));
                UNREACHABLE
            }
        };
    }
    // A session: the hello's answer, then one answer a request.
    if print_line(&hello).is_err() {
        return FAILED;
    }
    let mut input = io::stdin().lock();
    loop {
        let line = match read_line(&mut input, agent::MAX_REQUEST) {
            Ok(Line::Text(line)) => line,
            Ok(Line::End) | Err(_) => return DONE,
            Ok(Line::TooLong) => {
                let _ = print_line(&agent::err(
                    &Value::Null,
                    &Failure::new("too_large", "that request is longer than Syndeo reads"),
                ));
                continue;
            }
            Ok(Line::NotText) => {
                let _ = print_line(&agent::err(&Value::Null, &Failure::bad("not UTF-8")));
                continue;
            }
        };
        if line.trim().is_empty() {
            continue;
        }
        let answer = match serde_json::from_str::<Value>(&line) {
            Ok(request) if request.is_object() => match connection.send(request) {
                Ok(answer) => answer,
                Err(failure) => {
                    let _ = print_line(&agent::err(&Value::Null, &failure));
                    return UNREACHABLE;
                }
            },
            _ => agent::err(
                &Value::Null,
                &Failure::bad("a request is one JSON object a line"),
            ),
        };
        if print_line(&answer).is_err() {
            return FAILED;
        }
    }
}

/// The application, for the MCP server: connected, and hello said, when
/// first needed; connected again if the connection went away.
struct App {
    connection: Option<Connection>,
    scopes: Scopes,
}

impl Backend for App {
    fn call(&mut self, client: &str, request: Value) -> Result<Value, Failure> {
        if self.connection.is_none() {
            let (connection, _) =
                connect(&display_name(client), self.scopes).map_err(|(_, answer)| {
                    outcome(&answer)
                        .err()
                        .unwrap_or_else(|| Failure::new("off", OFF))
                })?;
            self.connection = Some(connection);
        }
        let connection = self.connection.as_mut().expect("connected above");
        let answer = connection.call(request);
        if matches!(&answer, Err(f) if f.code == "closed") {
            self.connection = None;
        }
        answer
    }
}

/// `Syndeo mcp`. Returns the exit status.
pub fn mcp(args: Vec<String>) -> i32 {
    let options = match client_options(&args, false) {
        Ok(options) => options,
        Err(why) => {
            eprintln!("Syndeo mcp: {why}\n\n{}", agent::MCP_USAGE);
            return USAGE;
        }
    };
    if options.help {
        eprint!("{}", agent::MCP_USAGE);
        return DONE;
    }
    let app = App {
        connection: None,
        scopes: options.scopes.unwrap_or(DEFAULT_SCOPES),
    };
    let mut server = Mcp::new(app, options.name);
    let mut input = io::stdin().lock();
    loop {
        let line = match read_line(&mut input, agent::MAX_REQUEST) {
            Ok(Line::Text(line)) => line,
            Ok(Line::End) | Err(_) => return DONE,
            Ok(Line::TooLong) | Ok(Line::NotText) => {
                let answer = json!({"jsonrpc": "2.0", "id": null,
                    "error": {"code": -32700, "message": "a message Syndeo cannot read"}});
                if print_line(&answer).is_err() {
                    return FAILED;
                }
                continue;
            }
        };
        if line.trim().is_empty() {
            continue;
        }
        if let Some(answer) = server.handle(&line) {
            let mut out = io::stdout().lock();
            if writeln!(out, "{answer}").and_then(|_| out.flush()).is_err() {
                return FAILED;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::BufRead;
    use std::os::unix::net::UnixListener;

    /// A server that answers each request with what `answer` makes of it.
    fn serve(
        answer: impl Fn(&Value) -> Option<Value> + Send + 'static,
    ) -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("s.sock");
        let listener = UnixListener::bind(&path).unwrap();
        std::thread::spawn(move || {
            if let Ok((stream, _)) = listener.accept() {
                let mut reader = io::BufReader::new(stream.try_clone().unwrap());
                let mut writer = stream;
                let mut line = String::new();
                while reader.read_line(&mut line).unwrap_or(0) > 0 {
                    let request: Value = serde_json::from_str(&line).unwrap();
                    match answer(&request) {
                        Some(reply) => {
                            writeln!(writer, "{reply}").unwrap();
                        }
                        None => return,
                    }
                    line.clear();
                }
            }
        });
        (dir, path)
    }

    #[test]
    fn a_connection_numbers_its_requests_and_reads_their_answers() {
        let (_dir, path) = serve(|request| {
            Some(json!({"id": request["id"], "ok": true, "result": {"echo": request["cmd"]}}))
        });
        let mut connection = Connection::open(&path).unwrap();
        assert_eq!(
            connection.call(json!({"cmd": "status"})).unwrap(),
            json!({"echo": "status"})
        );
        let answer = connection.send(json!({"cmd": "tabs"})).unwrap();
        assert_eq!(answer["id"], json!(2));
        let named = connection
            .send(json!({"id": "mine", "cmd": "tabs"}))
            .unwrap();
        assert_eq!(named["id"], json!("mine"));
    }

    #[test]
    fn failures_keep_the_applications_code() {
        let (_dir, path) = serve(|request| {
            Some(json!({"id": request["id"], "ok": false,
                        "error": {"code": "not_granted", "message": "no script"}}))
        });
        let mut connection = Connection::open(&path).unwrap();
        assert_eq!(
            connection.call(json!({"cmd": "eval"})).unwrap_err(),
            Failure::new("not_granted", "no script")
        );
        assert_eq!(known_code("something new"), "failed");
    }

    #[test]
    fn a_closed_connection_and_a_missing_socket_are_said_plainly() {
        let (_dir, path) = serve(|_| None);
        let mut connection = Connection::open(&path).unwrap();
        assert_eq!(
            connection.call(json!({"cmd": "status"})).unwrap_err().code,
            "closed"
        );
        let gone = Connection::open(Path::new("/nonexistent/syndeo.sock"))
            .err()
            .unwrap();
        assert_eq!(gone.code, "off");
        assert!(gone.message.contains("Allow agents to control Syndeo"));
    }
}
