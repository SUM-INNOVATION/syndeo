//! The agent process.
//!
//! Sandboxed by what it is given rather than by what it promises: two sockets,
//! one to the network process and one to the shell. It cannot open a connection
//! of its own, it does not know where the keystore listens, and it does not hold
//! the secret that would make a signing confirmation valid.
//!
//! When the agent needs something only a human can authorise, it asks the shell
//! and waits. The shell is free to say no.

use anyhow::{bail, Result};
use clap::Parser;
use std::path::PathBuf;
use syndeo_dom::Document;
use syndeo_ipc::protocol::{
    NetRequest, NetResponse, ShellRequest, ShellResponse, SignaturePurpose,
};
use syndeo_ipc::transport::{Channel, Endpoint};

#[derive(Parser)]
#[command(name = "syndeo-agent", version, about = "Reads pages; asks the shell for anything a human must approve")]
struct Cli {
    #[arg(long)]
    net_socket: PathBuf,
    #[arg(long)]
    shell_socket: PathBuf,
    /// `read <url>`, `crawl <url>`, `identity <origin>`, or `sign <origin> <message>`.
    #[arg(long)]
    task: String,
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

    // Proof, at startup, that the boundary holds. If the shell ever leaked the
    // session secret into this process's environment, this is where we would
    // find out rather than in an incident report.
    if std::env::var_os("SYNDEO_SESSION_SECRET").is_some() {
        bail!("the session secret is visible to the agent; refusing to run");
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
        other => bail!("unknown task {other}; try read, crawl, identity or sign"),
    }
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
    println!("  {} text blocks, {} links, {} subresources",
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
        println!("refetch of {} → {source} in {elapsed}ms", truncate(target, 60));
    }
    Ok(())
}

async fn fetch(net: &Endpoint, url: &str, integrity: Option<String>) -> Result<(Vec<u8>, String, u64)> {
    let mut channel = Channel::connect(net).await?;
    let response: NetResponse = channel
        .call(&NetRequest::Fetch {
            method: "GET".into(),
            url: url.to_string(),
            headers: vec![("accept".into(), "text/html,*/*".into())],
            body: Vec::new(),
            integrity,
        })
        .await?;
    match response {
        NetResponse::Fetched {
            body,
            source,
            elapsed_ms,
            ..
        } => Ok((body, source, elapsed_ms)),
        NetResponse::Error(e) => bail!(e),
        _ => bail!("unexpected reply from the network process"),
    }
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
            signature,
            address,
            ..
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
