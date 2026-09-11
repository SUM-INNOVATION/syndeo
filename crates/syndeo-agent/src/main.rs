//! The agent process.
//!
//! Sandboxed by what it is given rather than by what it promises: two sockets,
//! one to the network process and one to the shell. It cannot open a connection
//! of its own, it does not know where the keystore listens, and it does not hold
//! the secret that would make a signing confirmation valid.
//!
//! When the agent needs something only a human can authorise, it asks the shell
//! and waits. The shell is free to say no.

mod sandbox;
mod tools;

use anyhow::{bail, Result};
use clap::Parser;
use std::path::PathBuf;
use syndeo_dom::Document;
use syndeo_ipc::protocol::{NetRequest, ShellRequest, ShellResponse, SignaturePurpose};
use syndeo_ipc::transport::{Channel, Endpoint};

#[derive(Parser)]
#[command(
    name = "syndeo-agent",
    version,
    about = "Reads pages; asks the shell for anything a human must approve"
)]
struct Cli {
    #[arg(long)]
    net_socket: PathBuf,
    #[arg(long)]
    shell_socket: PathBuf,
    /// `read <url>`, `crawl <url>`, `identity <origin>`, `sign <origin> <message>`,
    /// `tools`, or `tool <name> <url>`.
    #[arg(long)]
    task: String,
    /// Where `.wasm` tools live. Defaults to `<home>/tools`.
    #[arg(long)]
    tools: Option<PathBuf>,
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_env("SYNDEO_LOG")
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("syndeo_agent=info")),
        )
        .with_target(false)
        .with_writer(std::io::stderr)
        .init();

    let cli = Cli::parse();

    // A shell that was force-quit runs no destructors, so `kill_on_drop` never
    // fires and this process would outlive it holding a socket.
    syndeo_ipc::exit_when_parent_does();

    // Proof, at startup, that the boundary holds. If the shell ever leaked the
    // session secret into this process's environment, this is where we would
    // find out rather than in an incident report.
    if std::env::var_os("SYNDEO_SESSION_SECRET").is_some() {
        bail!("the session secret is visible to the agent; refusing to run");
    }

    let net_path = cli.net_socket.clone();
    let shell_path = cli.shell_socket.clone();
    let tool_directory = cli.tools.clone().unwrap_or_else(default_tool_directory);

    // Second layer. The process boundary above is the first, and the check just
    // made is the third; a sandbox does not replace either.
    let mut grant = sandbox::grant_for(&net_path, &shell_path);
    grant.readable.push(tool_directory.clone());
    let confinement = sandbox::confine(&grant);
    match &confinement {
        c if c.is_enforced() => tracing::info!(sandbox = c.describe(), "confined"),
        c => tracing::warn!(reason = c.describe(), "not confined by the platform"),
    }

    let net = Endpoint::new(cli.net_socket);
    let shell = Endpoint::new(cli.shell_socket);

    let mut words = cli.task.split_whitespace();
    let verb = words.next().unwrap_or("read");
    let rest: Vec<&str> = words.collect();

    match verb {
        "read" => read(&net, rest.first().copied().unwrap_or_default()).await,
        "crawl" => crawl(&net, rest.first().copied().unwrap_or_default()).await,
        "identity" => identity(&shell, rest.first().copied().unwrap_or_default()).await,
        "sign" => {
            let Some((origin, message)) = rest.split_first() else {
                bail!("sign needs an origin and a message");
            };
            request_signature(&shell, origin, &message.join(" ")).await
        }
        "tools" => list_tools(&tool_directory, &confinement),
        "tool" => {
            let Some((name, rest)) = rest.split_first() else {
                bail!("tool needs a name and a url");
            };
            run_tool(
                &net,
                &tool_directory,
                name,
                rest.first().copied().unwrap_or_default(),
            )
            .await
        }
        other => bail!("unknown task {other}; try read, crawl, identity, sign, tools or tool"),
    }
}

fn default_tool_directory() -> PathBuf {
    std::env::var_os("SYNDEO_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            std::env::var_os("HOME")
                .map(PathBuf::from)
                .unwrap_or_else(|| PathBuf::from("."))
                .join(".syndeo")
        })
        .join("tools")
}

/// What this agent can run, and what is stopping it doing anything else.
fn list_tools(directory: &std::path::Path, confinement: &sandbox::Confinement) -> Result<()> {
    println!("sandbox   {}", confinement.describe());
    println!("tools     {}", directory.display());
    println!();

    let found = tools::discover(directory);
    if found.is_empty() {
        println!("No tools. A tool is a .wasm or .wat module in that directory exporting");
        println!("`memory`, `alloc(len) -> ptr` and `run(ptr, len) -> packed`.");
        println!("It is loaded with no imports at all, so it can reach nothing");
        println!("the host does not hand it.");
        return Ok(());
    }
    for (path, tool) in found {
        match tool {
            Ok(tool) => println!("  {:<20} ready", tool.name),
            Err(err) => println!("  {:<20} refused: {err:#}", file_stem(&path)),
        }
    }
    Ok(())
}

fn file_stem(path: &std::path::Path) -> String {
    path.file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default()
}

/// Fetch a page and run it through a tool.
///
/// The tool gets the page's text and nothing else — not the URL, not the
/// headers, not a socket. Whatever it returns is printed.
async fn run_tool(
    net: &Endpoint,
    directory: &std::path::Path,
    name: &str,
    url: &str,
) -> Result<()> {
    if url.is_empty() {
        bail!("tool needs a url to run against");
    }
    let path = tools::EXTENSIONS
        .iter()
        .map(|extension| directory.join(format!("{name}.{extension}")))
        .find(|candidate| candidate.exists())
        .ok_or_else(|| anyhow::anyhow!("no tool named {name} in {}", directory.display()))?;
    let tool = tools::Tool::load(&path)?;

    let (body, source, elapsed) = fetch(net, url, None).await?;
    let document = Document::parse_bytes(&body, Some(url));
    let input = document.text();

    let started = std::time::Instant::now();
    let output = tool.run(input.as_bytes())?;
    println!(
        "{name}  {source} in {elapsed}ms, tool in {}ms",
        started.elapsed().as_millis()
    );
    println!();
    match std::str::from_utf8(&output) {
        Ok(text) => println!("{text}"),
        Err(_) => println!("{} bytes of non-text output", output.len()),
    }
    Ok(())
}

/// Fetch and summarise one page.
async fn read(net: &Endpoint, url: &str) -> Result<()> {
    if url.is_empty() {
        bail!("read needs a url");
    }
    let (body, source, elapsed) = fetch(net, url, None).await?;
    let document = Document::parse_bytes(&body, Some(url));

    println!("{}", document.title().unwrap_or_else(|| url.to_string()));
    println!("  {source} in {elapsed}ms, {} bytes", body.len());

    let blocks = document.blocks();
    println!(
        "  {} text blocks, {} links, {} subresources",
        blocks.len(),
        document.links().len(),
        document.subresources().len()
    );

    println!();
    for block in blocks.iter().take(12) {
        match block.heading_level {
            Some(level) => println!("{} {}", "#".repeat(level as usize), block.text),
            None => println!("{}", truncate(&block.text, 100)),
        }
    }

    // What the page says its subresources must hash to. This is the input to
    // peer fetch: without one of these, a body from a peer can never be believed
    // on a first fetch.
    let integrity = document.integrity_map();
    if !integrity.is_empty() {
        println!();
        println!("subresources with declared integrity, eligible for peer fetch:");
        for (url, _) in &integrity {
            println!("  {url}");
        }
    }
    Ok(())
}

/// Fetch a page, then fetch what it links to, and report what the cache did.
async fn crawl(net: &Endpoint, url: &str) -> Result<()> {
    if url.is_empty() {
        bail!("crawl needs a url");
    }
    let (body, source, elapsed) = fetch(net, url, None).await?;
    println!("{:<14} {:>6}ms  {url}", source, elapsed);

    let document = Document::parse_bytes(&body, Some(url));
    let mut seen = std::collections::HashSet::new();
    seen.insert(url.to_string());

    let mut targets: Vec<String> = Vec::new();
    for link in document.links() {
        if link.url.starts_with("http") && seen.insert(link.url.clone()) {
            targets.push(link.url);
        }
    }
    for resource in document.subresources() {
        if resource.url.starts_with("http") && seen.insert(resource.url.clone()) {
            targets.push(resource.url);
        }
    }

    for target in targets.iter().take(12) {
        match fetch(net, target, None).await {
            Ok((body, source, elapsed)) => println!(
                "{:<14} {:>6}ms  {} ({} bytes)",
                source,
                elapsed,
                truncate(target, 70),
                body.len()
            ),
            Err(err) => println!("{:<14} {:>8}  {}", "failed", "", err),
        }
    }

    // Fetch the first target once more: the second time should not touch the
    // network at all.
    if let Some(target) = targets.first() {
        let (_, source, elapsed) = fetch(net, target, None).await?;
        println!();
        println!(
            "refetch of {} → {source} in {elapsed}ms",
            truncate(target, 60)
        );
    }
    Ok(())
}

async fn fetch(
    net: &Endpoint,
    url: &str,
    integrity: Option<String>,
) -> Result<(Vec<u8>, String, u64)> {
    let mut channel = Channel::connect(net).await?;
    // The agent summarises whole pages, so it waits for the whole body. The
    // frames it arrives in are what stop the transport from capping how large a
    // page it can read.
    let response = channel
        .fetch(&NetRequest::Fetch {
            // A page fetched at the top level is its own partition.
            partition: syndeo_ipc::protocol::partition_for(url),
            method: "GET".into(),
            url: url.to_string(),
            headers: vec![("accept".into(), "text/html,*/*".into())],
            body: Vec::new(),
            integrity,
        })
        .await?;
    Ok((response.body, response.source, response.elapsed_ms))
}

async fn identity(shell: &Endpoint, origin: &str) -> Result<()> {
    let mut channel = Channel::connect(shell).await?;
    let response: ShellResponse = channel
        .call(&ShellRequest::IdentityFor {
            origin: origin.to_string(),
        })
        .await?;
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
        other => bail!("unexpected reply: {other:?}"),
    }
}

/// The agent asks. It does not sign, and it cannot.
async fn request_signature(shell: &Endpoint, origin: &str, message: &str) -> Result<()> {
    let mut channel = Channel::connect(shell).await?;
    let response: ShellResponse = channel
        .call(&ShellRequest::RequestSignature {
            origin: origin.to_string(),
            purpose: SignaturePurpose::Attestation,
            description: format!("The agent is asking to sign: {message}"),
            payload: message.as_bytes().to_vec(),
        })
        .await?;
    match response {
        ShellResponse::Signed {
            signature, address, ..
        } => {
            println!("signed by {address}");
            println!("{signature}");
            Ok(())
        }
        ShellResponse::Declined(reason) => {
            println!("the shell declined: {reason}");
            Ok(())
        }
        ShellResponse::Error(e) => bail!(e),
        other => bail!("unexpected reply: {other:?}"),
    }
}

fn truncate(s: &str, width: usize) -> String {
    let s = s.replace('\n', " ");
    if s.chars().count() <= width {
        s
    } else {
        format!("{}…", s.chars().take(width - 1).collect::<String>())
    }
}
