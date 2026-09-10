//! The shell.
//!
//! It owns the process model. It spawns the network process, the keystore and
//! the agent; it decides what each of them is told; and it is the only thing in
//! the tree that ever speaks to the keystore.

use anyhow::{bail, Context, Result};
use clap::{Parser, Subcommand};
use std::path::PathBuf;
use std::sync::Arc;
use syndeo_shell::prompt::{self, NonInteractive, Prompter, TerminalPrompter};
use syndeo_shell::{Shell, Supervisor};
use syndeo_dom::Document;
use syndeo_ipc::confirm::{Confirmer, SessionSecret};
use syndeo_ipc::protocol::{
    KeystoreRequest, KeystoreResponse, NetRequest, NetResponse, ShellRequest, ShellResponse,
    SignaturePurpose,
};
use syndeo_ipc::transport::{Channel, Endpoint, Server};

#[derive(Parser)]
#[command(
    name = "syndeo",
    version,
    about = "A browser built cache-first, with the agent, the network and the keys in separate processes"
)]
struct Cli {
    /// Where cache, keys and sockets live.
    #[arg(long, global = true)]
    home: Option<PathBuf>,
    /// system | dot:cloudflare | doh:cloudflare | doh:google | doh:quad9
    #[arg(long, global = true, default_value = "system")]
    dns: String,
    /// Join the peer swarm. Repeat with a multiaddress to dial a bootstrap peer.
    /// A peer is only ever asked for a body the page already named by hash.
    #[arg(long = "peer", global = true)]
    peers: Vec<String>,

    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Fetch a page through the network process and read it.
    Browse {
        url: String,
        /// Also list links, subresources and forms.
        #[arg(long)]
        full: bool,
        /// Emit JSON.
        #[arg(long)]
        json: bool,
        /// Fetch again, to show the cache working.
        #[arg(long)]
        twice: bool,
    },
    /// Run the agent against a task, with the full process model up.
    Agent {
        task: String,
    },
    /// Ask for a signature over a message, the way a site would.
    Sign {
        #[arg(long)]
        origin: String,
        #[arg(long)]
        message: String,
        /// login | transaction | attestation
        #[arg(long, default_value = "attestation")]
        purpose: String,
        /// Take this invocation as the confirmation. Only valid here, where the
        /// user typed the payload; never for an agent's request.
        #[arg(long)]
        yes: bool,
    },
    /// Show the identity used for an origin.
    Identity {
        origin: String,
    },
    /// Cache statistics.
    Stats {
        #[arg(long)]
        json: bool,
    },
    /// Run or inspect a peer node.
    Peer {
        #[command(subcommand)]
        command: PeerCommand,
    },
    /// Report the state of the process model and its boundaries.
    Doctor,
}

#[derive(Subcommand)]
enum PeerCommand {
    /// Join the swarm and serve bodies until interrupted.
    ///
    /// `syndeo browse --peer on` joins for the life of one command and then
    /// exits, so nothing seeds. This is the mode that does.
    Serve {
        /// Multiaddress to listen on, repeatable.
        #[arg(long = "listen")]
        listen: Vec<String>,
        /// Answer, but never ask.
        #[arg(long)]
        serve_only: bool,
    },
    /// What the swarm looks like from a node started here.
    Status {
        #[arg(long)]
        json: bool,
    },
}

fn home(override_path: Option<PathBuf>) -> PathBuf {
    override_path.unwrap_or_else(|| {
        std::env::var_os("SYNDEO_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| {
                std::env::var_os("HOME")
                    .map(PathBuf::from)
                    .unwrap_or_else(|| PathBuf::from("."))
                    .join(".syndeo")
            })
    })
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_env("SYNDEO_LOG")
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("syndeo=warn,syndeo_shell=info")),
        )
        .with_target(false)
        .with_writer(std::io::stderr)
        .init();

    let cli = Cli::parse();
    let home = home(cli.home);
    std::fs::create_dir_all(&home)?;

    match cli.command {
        Command::Browse {
            url,
            full,
            json,
            twice,
        } => browse(&home, &cli.dns, &cli.peers, &url, full, json, twice).await,
        Command::Agent { task } => agent(&home, &cli.dns, &cli.peers, &task).await,
        Command::Sign {
            origin,
            message,
            purpose,
            yes,
        } => sign(&home, &origin, &message, &purpose, yes).await,
        Command::Identity { origin } => identity(&home, &origin).await,
        Command::Stats { json } => stats(&home, json),
        Command::Peer { command } => match command {
            PeerCommand::Serve { listen, serve_only } => {
                peer_serve(&home, &cli.dns, &cli.peers, &listen, serve_only).await
            }
            PeerCommand::Status { json } => peer_status(&home, &cli.dns, &cli.peers, json).await,
        },
        Command::Doctor => doctor(&home).await,
    }
}

// ------------------------------------------------------------------- browse

async fn browse(
    home: &std::path::Path,
    dns: &str,
    peers: &[String],
    url: &str,
    full: bool,
    json: bool,
    twice: bool,
) -> Result<()> {
    let mut supervisor = Supervisor::new(home);
    let net = supervisor.start_net(dns, peers).await?;

    let fetch = || async {
        let mut channel = Channel::connect(&net).await?;
        let response = channel
            .fetch(&NetRequest::Fetch {
                method: "GET".into(),
                url: url.to_string(),
                headers: vec![("accept".into(), "text/html,*/*".into())],
                body: Vec::new(),
                integrity: None,
            })
            .await?;
        anyhow::Ok(response)
    };

    let first = fetch().await?;
    let second = if twice { Some(fetch().await?) } else { None };
    supervisor.shutdown().await;

    let syndeo_ipc::protocol::Fetched {
        status,
        headers,
        body,
        source,
        protocol,
        elapsed_ms,
        content,
    } = first;

    let document = Document::parse_bytes(&body, Some(url));

    if json {
        let value = serde_json::json!({
            "url": url,
            "status": status,
            "source": source,
            "protocol": protocol,
            "elapsed_ms": elapsed_ms,
            "content": content,
            "bytes": body.len(),
            "title": document.title(),
            "text": document.text(),
            "links": document.links().iter().map(|l| serde_json::json!({"url": l.url, "text": l.text})).collect::<Vec<_>>(),
            "subresources": document.subresources().iter().map(|s| serde_json::json!({
                "url": s.url, "kind": s.kind, "integrity": s.integrity
            })).collect::<Vec<_>>(),
        });
        println!("{}", serde_json::to_string_pretty(&value)?);
        return Ok(());
    }

    println!("{url}");
    println!(
        "  {status}  {source}  {protocol}  {elapsed_ms}ms  {}",
        syndeo_cache::stats::human(body.len() as u64)
    );
    if let Some(content) = &content {
        println!("  content {}", &content[..16.min(content.len())]);
    }
    if let Some(ct) = headers
        .iter()
        .find(|(n, _)| n.eq_ignore_ascii_case("content-type"))
    {
        println!("  type    {}", ct.1);
    }
    if let Some(second) = second {
        println!(
            "  again   {}  {}  {}ms",
            second.source, second.protocol, second.elapsed_ms
        );
    }
    println!();

    if let Some(title) = document.title() {
        println!("# {title}");
        println!();
    }

    let text = document.text();
    let shown: Vec<&str> = text.lines().take(40).collect();
    println!("{}", shown.join("\n"));
    if text.lines().count() > 40 {
        println!("… {} more lines", text.lines().count() - 40);
    }

    if full {
        let links = document.links();
        if !links.is_empty() {
            println!();
            println!("links ({})", links.len());
            for link in links.iter().take(20) {
                println!("  {:<60} {}", truncate(&link.url, 60), truncate(&link.text, 40));
            }
        }
        let resources = document.subresources();
        if !resources.is_empty() {
            println!();
            println!("subresources ({})", resources.len());
            for resource in resources.iter().take(20) {
                let integrity = match &resource.integrity {
                    Some(_) => "integrity declared",
                    None => "no integrity — origin only",
                };
                println!(
                    "  {:<11} {:<50} {}",
                    resource.kind,
                    truncate(&resource.url, 50),
                    integrity
                );
            }
        }
        let forms = document.forms();
        if !forms.is_empty() {
            println!();
            println!("forms ({})", forms.len());
            for form in &forms {
                println!("  {} {}", form.method, form.action);
                for field in &form.fields {
                    println!(
                        "    {:<20} {:<10}{}",
                        field.name,
                        field.kind,
                        if field.required { " required" } else { "" }
                    );
                }
            }
        }
    }
    Ok(())
}

fn truncate(s: &str, width: usize) -> String {
    let s = s.replace('\n', " ");
    if s.chars().count() <= width {
        s
    } else {
        format!("{}…", s.chars().take(width - 1).collect::<String>())
    }
}

// -------------------------------------------------------------------- agent

async fn agent(home: &std::path::Path, dns: &str, peers: &[String], task: &str) -> Result<()> {
    let secret = SessionSecret::generate();
    let mut supervisor = Supervisor::new(home);

    let net = supervisor.start_net(dns, peers).await?;
    let keystore = supervisor.start_keystore(&secret).await?;
    unseal(&keystore).await?;

    // The shell's own socket. This is what the agent is given; the keystore
    // endpoint stays in this process.
    let shell_endpoint = Endpoint::new(supervisor.runtime_dir().join("shell.sock"));
    // The agent's requests go in front of whoever is at the terminal. When
    // nobody is, they are declined rather than left waiting.
    let prompter: Arc<dyn Prompter> = if std::io::IsTerminal::is_terminal(&std::io::stdin()) {
        Arc::new(TerminalPrompter)
    } else {
        Arc::new(NonInteractive)
    };
    let shell = Arc::new(Shell::new(
        Arc::new(Confirmer::new(secret)),
        keystore,
        prompter,
    ));
    let server = Server::bind(shell_endpoint.clone())?;
    let serving = tokio::spawn(shell.serve(server));

    supervisor.start_agent(&net, &shell_endpoint, task)?;
    let status = supervisor.wait_for_child("agent").await?;
    serving.abort();
    supervisor.shutdown().await;

    if !status.success() {
        bail!("the agent exited with {status}");
    }
    Ok(())
}

// --------------------------------------------------------------------- sign

async fn sign(
    home: &std::path::Path,
    origin: &str,
    message: &str,
    purpose: &str,
    typed_consent: bool,
) -> Result<()> {
    let purpose = match purpose {
        "login" => SignaturePurpose::OriginLogin,
        "transaction" => SignaturePurpose::ChainTransaction,
        "attestation" => SignaturePurpose::Attestation,
        other => bail!("unknown purpose {other}; use login, transaction or attestation"),
    };

    let secret = SessionSecret::generate();
    let mut supervisor = Supervisor::new(home);
    let keystore = supervisor.start_keystore(&secret).await?;
    unseal(&keystore).await?;

    let shell = Shell::new(
        Arc::new(Confirmer::new(secret)),
        keystore,
        Arc::new(TerminalPrompter),
    );
    let response = if typed_consent {
        shell
            .sign_with_typed_consent(
                origin.to_string(),
                purpose,
                message.to_string(),
                message.as_bytes().to_vec(),
            )
            .await
    } else {
        shell
            .handle(ShellRequest::RequestSignature {
                origin: origin.to_string(),
                purpose,
                description: message.to_string(),
                payload: message.as_bytes().to_vec(),
            })
            .await
    };
    supervisor.shutdown().await;

    match response {
        ShellResponse::Signed {
            signature,
            public_key,
            address,
        } => {
            println!("origin      {origin}");
            println!("address     {address}");
            println!("public key  {public_key}");
            println!("signature   {signature}");
            Ok(())
        }
        ShellResponse::Declined(reason) => bail!("declined: {reason}"),
        ShellResponse::Error(e) => bail!(e),
        _ => bail!("unexpected reply"),
    }
}

async fn identity(home: &std::path::Path, origin: &str) -> Result<()> {
    let secret = SessionSecret::generate();
    let mut supervisor = Supervisor::new(home);
    let keystore = supervisor.start_keystore(&secret).await?;
    unseal(&keystore).await?;

    let shell = Shell::new(
        Arc::new(Confirmer::new(secret)),
        keystore,
        Arc::new(TerminalPrompter),
    );
    let response = shell
        .handle(ShellRequest::IdentityFor {
            origin: origin.to_string(),
        })
        .await;
    supervisor.shutdown().await;

    match response {
        ShellResponse::Identity {
            public_key,
            address,
        } => {
            println!("origin      {origin}");
            println!("public key  {public_key}");
            println!("address     {address}");
            Ok(())
        }
        ShellResponse::Error(e) => bail!(e),
        _ => bail!("unexpected reply"),
    }
}

/// Ask the keystore what it needs, then supply it. The passphrase is read here,
/// in the shell, and sent to the keystore — it never reaches the agent.
async fn unseal(keystore: &Endpoint) -> Result<()> {
    let mut channel = Channel::connect(keystore).await?;
    let status: KeystoreResponse = channel.call(&KeystoreRequest::Status).await?;
    let KeystoreResponse::Status {
        initialized,
        passphrase_required,
        ..
    } = status
    else {
        bail!("the keystore did not report a status");
    };
    if !initialized {
        bail!("no keystore yet — run `syndeo-keystore init` first");
    }

    let passphrase = if passphrase_required {
        Some(prompt::read_passphrase("Keystore passphrase: ").context("reading the passphrase")?)
    } else {
        None
    };

    match channel.call(&KeystoreRequest::Unseal { passphrase }).await? {
        KeystoreResponse::Ok => Ok(()),
        KeystoreResponse::Error(e) => bail!(e),
        _ => bail!("unexpected reply from the keystore"),
    }
}

// --------------------------------------------------------------------- peer

/// Join the swarm and stay in it.
async fn peer_serve(
    home: &std::path::Path,
    dns: &str,
    peers: &[String],
    listen: &[String],
    serve_only: bool,
) -> Result<()> {
    let mut supervisor = Supervisor::new(home);
    let net = supervisor
        .start_peer_node(dns, peers, listen, serve_only)
        .await?;

    let status = peer_status_value(&net).await?;
    println!("peer      {}", status["peer_id"].as_str().unwrap_or("?"));
    for address in status["listeners"].as_array().into_iter().flatten() {
        println!("listening {}", address.as_str().unwrap_or_default());
    }
    println!("serving   {}", status["serving"]);
    println!();
    println!("Dial this node from another with:");
    if let Some(first) = status["listeners"].as_array().and_then(|a| a.first()) {
        println!(
            "  syndeo peer serve --peer {}/p2p/{}",
            first.as_str().unwrap_or_default(),
            status["peer_id"].as_str().unwrap_or_default()
        );
    }
    println!();
    println!("Serving until interrupted.");

    tokio::signal::ctrl_c().await?;
    println!();
    println!("stopping");
    supervisor.shutdown().await;
    Ok(())
}

async fn peer_status(
    home: &std::path::Path,
    dns: &str,
    peers: &[String],
    json: bool,
) -> Result<()> {
    let mut supervisor = Supervisor::new(home);
    let net = supervisor.start_peer_node(dns, peers, &[], false).await?;
    let status = peer_status_value(&net).await?;
    supervisor.shutdown().await;

    if json {
        println!("{}", serde_json::to_string_pretty(&status)?);
        return Ok(());
    }

    println!("peer          {}", status["peer_id"].as_str().unwrap_or("?"));
    println!("serving       {}", status["serving"]);
    println!("routing table {} peers", status["routing_table"]);
    println!("announced     {} bodies", status["announced"]);
    let connected = status["connected"].as_array().cloned().unwrap_or_default();
    println!("connected     {}", connected.len());
    if !connected.is_empty() {
        println!();
        println!("  {:<54} {:>6} {:>6} {:>9}", "peer", "gave", "took", "standing");
        for report in &connected {
            println!(
                "  {:<54} {:>6} {:>6} {:>9}",
                report["peer"].as_str().unwrap_or_default(),
                report["received"].as_u64().unwrap_or(0),
                report["served"].as_u64().unwrap_or(0),
                report["standing"].as_i64().unwrap_or(0),
            );
        }
        println!();
        println!("`gave` is bodies that peer handed us; `took` is bodies we handed it.");
    }
    Ok(())
}

async fn peer_status_value(net: &Endpoint) -> Result<serde_json::Value> {
    let mut channel = Channel::connect(net).await?;
    match channel.call(&NetRequest::PeerStatus).await? {
        NetResponse::PeerStatus(value) => Ok(value),
        NetResponse::Error(e) => bail!(e),
        _ => bail!("unexpected reply from the network process"),
    }
}

// -------------------------------------------------------------------- stats

fn stats(home: &std::path::Path, json: bool) -> Result<()> {
    let cache = syndeo_cache::Cache::open(home.join("cache"))?;
    let stats = cache.stats()?;
    if json {
        println!("{}", serde_json::to_string_pretty(&stats)?);
    } else {
        println!("{}", stats.render());
    }
    Ok(())
}

// ------------------------------------------------------------------- doctor

async fn doctor(home: &std::path::Path) -> Result<()> {
    println!("home        {}", home.display());

    let cache_root = home.join("cache");
    match syndeo_cache::Cache::open(&cache_root) {
        Ok(cache) => {
            let stats = cache.stats()?;
            println!("cache       {} entries, {} blobs", stats.entries, stats.blobs);
        }
        Err(err) => println!("cache       unavailable: {err}"),
    }

    let secret = SessionSecret::generate();
    let mut supervisor = Supervisor::new(home);
    match supervisor.start_keystore(&secret).await {
        Ok(endpoint) => {
            let mut channel = Channel::connect(&endpoint).await?;
            if let KeystoreResponse::Status {
                initialized,
                unsealed,
                passphrase_required,
                presence_enforced,
                idle_timeout_secs,
                ..
            } = channel.call(&KeystoreRequest::Status).await?
            {
                println!("keystore    initialized {initialized}, unsealed {unsealed}");
                println!("            passphrase required {passphrase_required}");
                println!("            platform presence enforced {presence_enforced}");
                match idle_timeout_secs {
                    Some(secs) => println!("            idle auto-lock after {secs}s"),
                    None => println!("            idle auto-lock off"),
                }
            }
        }
        Err(err) => println!("keystore    not reachable: {err}"),
    }
    supervisor.shutdown().await;

    println!();
    println!("boundaries");
    println!("  renderers and the agent reach the network only through the net process");
    println!("  the agent has no keystore socket and no session secret");
    println!("  the keystore signs only what the shell confirmed, once, for one payload");
    println!("  the seed is forgotten on idleness, on sleep, and on screen lock");
    Ok(())
}
