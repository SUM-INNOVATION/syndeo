//! The shell.
//!
//! It owns the process model. It spawns the network process, the keystore and
//! the agent; it decides what each of them is told; and it is the only thing in
//! the tree that ever speaks to the keystore.

use anyhow::{bail, Result};
use clap::{Parser, Subcommand};
use std::path::PathBuf;
use std::sync::Arc;
use syndeo_dom::Document;
use syndeo_ipc::confirm::{Confirmer, SessionSecret};
use syndeo_ipc::protocol::{
    KeystoreRequest, KeystoreResponse, NetRequest, NetResponse, ShellRequest, ShellResponse,
};
use syndeo_ipc::startup::StartupSecrets;
use syndeo_ipc::transport::{Channel, Endpoint, Server};
use syndeo_ipc::SecretString;
use syndeo_shell::prompt::{NonInteractive, Prompter, TerminalPrompter};
use syndeo_shell::signing::{self, unseal, Consent};
use syndeo_shell::supervisor::InstallDir;
use syndeo_shell::{Shell, Supervisor};

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
    #[arg(long, global = true, default_value = "doh:cloudflare")]
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
    Agent { task: String },
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
    Identity { origin: String },
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

fn main() -> Result<()> {
    // First, while this is still one thread: a scripted passphrase leaves the
    // environment before a runtime, a logger, or any child process exists to
    // read or inherit it. `run` owns it from here.
    let secrets = syndeo_ipc::startup::capture(&[syndeo_ipc::startup::PASSPHRASE]);
    // Then, before anything can start a sibling: the directory of the image
    // this process runs. An upgrade that replaces this version later cannot
    // change the answer, and for the macOS package it is the only place a
    // sibling is looked for. Nothing that starts one can be made without it.
    let install = syndeo_shell::supervisor::capture_install_dir()?;
    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .map_err(|err| anyhow::anyhow!("starting the runtime: {err}"))?
        .block_on(run(secrets, install))
}

/// `secrets` is what `main` took out of the environment. A scripted
/// passphrase goes to the terminal prompter, which hands it over once; it is
/// wiped when that is dropped, used or not, which is before this returns.
async fn run(mut secrets: StartupSecrets, install: InstallDir) -> Result<()> {
    let terminal = TerminalPrompter::new(
        secrets
            .take(syndeo_ipc::startup::PASSPHRASE)
            .map(SecretString::from),
    );
    drop(secrets);

    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_env("SYNDEO_LOG").unwrap_or_else(|_| {
                tracing_subscriber::EnvFilter::new("syndeo=warn,syndeo_shell=info")
            }),
        )
        .with_target(false)
        .with_writer(std::io::stderr)
        .init();

    let cli = Cli::parse();
    let home = home(cli.home);
    std::fs::create_dir_all(&home)?;
    // The example tools, the first time a home has none. Never a reason for
    // the command itself to fail.
    match syndeo_shell::tools::seed_example_tools(install, &home) {
        Ok(outcome) => tracing::debug!(?outcome, "example tools"),
        Err(err) => tracing::debug!(%err, "the example tools were not seeded"),
    }

    match cli.command {
        Command::Browse {
            url,
            full,
            json,
            twice,
        } => {
            browse(
                install, &home, &cli.dns, &cli.peers, &url, full, json, twice,
            )
            .await
        }
        Command::Agent { task } => {
            agent(install, &home, &cli.dns, &cli.peers, &task, terminal).await
        }
        Command::Sign {
            origin,
            message,
            purpose,
            yes,
        } => sign(install, &home, &origin, &message, &purpose, yes, terminal).await,
        Command::Identity { origin } => identity(install, &home, &origin, terminal).await,
        Command::Stats { json } => stats(&home, json),
        Command::Peer { command } => match command {
            PeerCommand::Serve { listen, serve_only } => {
                peer_serve(install, &home, &cli.dns, &cli.peers, &listen, serve_only).await
            }
            PeerCommand::Status { json } => {
                peer_status(install, &home, &cli.dns, &cli.peers, json).await
            }
        },
        Command::Doctor => doctor(install, &home).await,
    }
}

// ------------------------------------------------------------------- browse

#[allow(clippy::too_many_arguments)]
async fn browse(
    install: InstallDir,
    home: &std::path::Path,
    dns: &str,
    peers: &[String],
    url: &str,
    full: bool,
    json: bool,
    twice: bool,
) -> Result<()> {
    let mut supervisor = Supervisor::new(home, install);
    let net = supervisor.start_net(dns, peers).await?;

    let fetch = || async {
        let channel = Channel::connect(&net).await?;
        let response = channel
            .fetch(&NetRequest::Fetch {
                // A page fetched at the top level is its own partition.
                partition: syndeo_ipc::protocol::partition_for(url),
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
            "cut_short": document.cut_short().map(|cut| serde_json::json!({
                "parsed": cut.parsed, "of": cut.of
            })),
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

    let report = syndeo_shell::browse_view::Fetch {
        url: url.to_string(),
        status,
        source,
        protocol,
        elapsed_ms,
        bytes: body.len(),
        content,
        content_type: headers
            .iter()
            .find(|(n, _)| n.eq_ignore_ascii_case("content-type"))
            .map(|(_, v)| v.clone()),
        again: second.map(|s| (s.source, s.protocol, s.elapsed_ms)),
    };
    print!(
        "{}",
        syndeo_shell::browse_view::human(&report, &document, full)
    );
    Ok(())
}

// -------------------------------------------------------------------- agent

async fn agent(
    install: InstallDir,
    home: &std::path::Path,
    dns: &str,
    peers: &[String],
    task: &str,
    terminal: TerminalPrompter,
) -> Result<()> {
    let secret = SessionSecret::generate();
    let mut supervisor = Supervisor::new(home, install);

    let net = supervisor.start_net(dns, peers).await?;
    let keystore = supervisor.start_keystore(&secret).await?;
    unseal(&keystore, &terminal).await?;

    // The shell's own socket. This is what the agent is given; the keystore
    // endpoint stays in this process.
    let shell_endpoint = Endpoint::new(supervisor.runtime_dir().join("shell.sock"));
    // The agent's requests go in front of whoever is at the terminal. When
    // nobody is, they are declined rather than left waiting.
    let prompter: Arc<dyn Prompter> = if std::io::IsTerminal::is_terminal(&std::io::stdin()) {
        Arc::new(terminal)
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
    install: InstallDir,
    home: &std::path::Path,
    origin: &str,
    message: &str,
    purpose: &str,
    typed_consent: bool,
    terminal: TerminalPrompter,
) -> Result<()> {
    // Both checked before anything is started: an unknown purpose here, and
    // the request itself inside `sign_message`, which starts and unseals a
    // keystore only for a request that passed.
    let purpose = signing::parse_purpose(purpose)?;
    let consent = if typed_consent {
        Consent::Typed
    } else {
        Consent::Prompted
    };
    let signed =
        signing::sign_message(install, home, origin, message, purpose, consent, terminal).await?;

    // The canonical origin, which is the one the key was derived for.
    println!("origin      {}", signed.origin);
    println!("address     {}", signed.address);
    println!("public key  {}", signed.public_key);
    println!("signature   {}", signed.signature);
    Ok(())
}

async fn identity(
    install: InstallDir,
    home: &std::path::Path,
    origin: &str,
    terminal: TerminalPrompter,
) -> Result<()> {
    let secret = SessionSecret::generate();
    let mut supervisor = Supervisor::new(home, install);
    let keystore = supervisor.start_keystore(&secret).await?;
    unseal(&keystore, &terminal).await?;

    let shell = Shell::new(
        Arc::new(Confirmer::new(secret)),
        keystore,
        Arc::new(terminal),
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

// --------------------------------------------------------------------- peer

/// Join the swarm and stay in it.
async fn peer_serve(
    install: InstallDir,
    home: &std::path::Path,
    dns: &str,
    peers: &[String],
    listen: &[String],
    serve_only: bool,
) -> Result<()> {
    let mut supervisor = Supervisor::new(home, install);
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
    install: InstallDir,
    home: &std::path::Path,
    dns: &str,
    peers: &[String],
    json: bool,
) -> Result<()> {
    let mut supervisor = Supervisor::new(home, install);
    let net = supervisor.start_peer_node(dns, peers, &[], false).await?;
    let status = peer_status_value(&net).await?;
    supervisor.shutdown().await;

    if json {
        println!("{}", serde_json::to_string_pretty(&status)?);
        return Ok(());
    }

    println!(
        "peer          {}",
        status["peer_id"].as_str().unwrap_or("?")
    );
    println!("serving       {}", status["serving"]);
    println!("routing table {} peers", status["routing_table"]);
    println!("announced     {} bodies", status["announced"]);
    let connected = status["connected"].as_array().cloned().unwrap_or_default();
    println!("connected     {}", connected.len());
    if !connected.is_empty() {
        println!();
        println!(
            "  {:<54} {:>6} {:>6} {:>9}",
            "peer", "gave", "took", "standing"
        );
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

async fn doctor(install: InstallDir, home: &std::path::Path) -> Result<()> {
    println!("version         {}", env!("CARGO_PKG_VERSION"));
    println!("home            {}", home.display());

    // The first thing to go wrong in an install is a half-copied one, and the
    // symptom is a timeout somewhere much later. Say it here instead.
    let mut missing = Vec::new();
    let mut removed = false;
    for name in ["syndeo-net", "syndeo-keystore", "syndeo-agent"] {
        match install.locate(name) {
            Ok(path) => println!("{name:<16}{}", path.display()),
            Err(err) => {
                println!("{name:<16}NOT FOUND");
                removed |= err
                    .downcast_ref::<syndeo_shell::supervisor::RemovedByUpgrade>()
                    .is_some();
                missing.push(name);
            }
        }
    }
    if !missing.is_empty() {
        println!();
        if removed {
            println!(
                "            {}: this version was removed during an upgrade;",
                missing.join(", ")
            );
            println!("                quit and restart Syndeo.");
        } else {
            println!(
                "            {} is missing. Every Syndeo binary has to be",
                missing.join(", ")
            );
            println!("                installed into one directory; re-run install.sh.");
        }
        return Ok(());
    }

    let cache_root = home.join("cache");
    match syndeo_cache::Cache::open(&cache_root) {
        Ok(cache) => {
            let stats = cache.stats()?;
            println!(
                "cache           {} entries, {} blobs",
                stats.entries, stats.blobs
            );
        }
        Err(err) => println!("cache           unavailable: {err}"),
    }

    let secret = SessionSecret::generate();
    let mut supervisor = Supervisor::new(home, install);
    let session = match supervisor.start_keystore(&secret).await {
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
                println!("keystore        initialized {initialized}, unsealed {unsealed}");
                println!("                passphrase required {passphrase_required}");
                println!("                platform presence enforced {presence_enforced}");
                match idle_timeout_secs {
                    Some(secs) => println!("                idle auto-lock after {secs}s"),
                    None => println!("                idle auto-lock off"),
                }
            }
            syndeo_shell::doctor::session_facts(&mut channel).await
        }
        Err(err) => {
            println!("keystore        not reachable: {err}");
            syndeo_shell::doctor::SessionFacts::Unreachable
        }
    };
    supervisor.shutdown().await;

    println!();
    println!("boundaries");
    for line in syndeo_shell::doctor::boundaries(syndeo_shell::doctor::Platform::this(), session) {
        println!("  {line}");
    }
    Ok(())
}
