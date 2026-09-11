//! The windowed shell.
//!
//! Step six of the build order. Same process model as the terminal front end —
//! it spawns the network process and the keystore, hands the agent two sockets
//! and nothing else, and is the only thing that ever speaks to the keystore —
//! with the confirmation dialog drawn in a window rather than in a terminal.
//!
//! The window is `winit` for the window, `wgpu` for the surface, `egui` for the
//! chrome and `accesskit` for the accessibility tree, which is the stack
//! servoshell uses. Accessibility is here from the start on purpose: skipping it
//! is how a browser becomes unshippable, and retrofitting it costs far more than
//! building with it.

mod app;
mod fonts;
mod page;
mod prompt;

use anyhow::{bail, Context, Result};
use app::{Done, Work};
use clap::Parser;
use prompt::{Ask, WindowPrompter};
use std::path::PathBuf;
use std::sync::atomic::AtomicBool;
use std::sync::mpsc;
use std::sync::Arc;
use syndeo_ipc::protocol::{KeystoreRequest, KeystoreResponse, NetRequest, NetResponse};
use syndeo_ipc::transport::{Channel, Endpoint, Server};
use syndeo_shell::prompt::Prompter;
use syndeo_shell::{Shell, Supervisor};

#[derive(Parser)]
#[command(
    name = "syndeo-ui",
    version,
    about = "The windowed shell: the same process model, with the confirmation dialog in a window"
)]
struct Cli {
    /// Open this address on start.
    url: Option<String>,
    /// Where cache, keys and sockets live.
    #[arg(long)]
    home: Option<PathBuf>,
    /// system | dot:cloudflare | doh:cloudflare | doh:google | doh:quad9
    #[arg(long, default_value = "system")]
    dns: String,
    /// Join the peer swarm. Repeat with a multiaddress to dial a bootstrap peer.
    #[arg(long = "peer")]
    peers: Vec<String>,
    /// Do not open the keystore. The window browses; nothing can be signed.
    #[arg(long)]
    no_keys: bool,
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
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_env("SYNDEO_LOG")
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("syndeo_ui=info")),
        )
        .with_target(false)
        .with_writer(std::io::stderr)
        .init();

    let cli = Cli::parse();
    let home = home(cli.home.clone());
    std::fs::create_dir_all(&home)?;

    // The window draws on the main thread, because that is where a platform
    // will let you have one. Everything that talks to another process runs on a
    // tokio runtime beside it.
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .context("starting the runtime")?;

    let (work_sender, work_receiver) = mpsc::channel::<Work>();
    let (done_sender, done_receiver) = mpsc::channel::<Done>();
    let (ask_sender, ask_receiver) = mpsc::sync_channel::<Ask>(4);
    let closed = Arc::new(AtomicBool::new(false));

    let prompter = Arc::new(WindowPrompter::new(ask_sender, closed.clone()));
    let session = runtime
        .block_on(Session::start(&home, &cli, prompter.clone()))
        .context("bringing up the process model")?;

    let net = session.net.clone();
    let worker = std::thread::Builder::new()
        .name("syndeo-ui-worker".into())
        .spawn({
            let handle = runtime.handle().clone();
            move || serve(handle, net, work_receiver, done_sender)
        })
        .context("starting the worker thread")?;

    let start = cli.url.clone();
    let native = eframe::NativeOptions {
        viewport: eframe::egui::ViewportBuilder::default()
            .with_title("Syndeo")
            .with_inner_size([1100.0, 760.0])
            .with_min_inner_size([640.0, 420.0]),
        ..Default::default()
    };

    let result = eframe::run_native(
        "syndeo",
        native,
        Box::new(move |context| {
            // accesskit publishes the tree through this context; giving the
            // prompter the same handle is what lets a signing request wake the
            // window rather than wait for the next frame.
            prompter.attach(context.egui_ctx.clone());
            // Before the first frame: a page in a script the bundled fonts do
            // not cover otherwise draws as boxes.
            fonts::install_fallback(&context.egui_ctx);
            Ok(Box::new(app::App::new(
                &context.egui_ctx,
                work_sender,
                done_receiver,
                ask_receiver,
                closed,
                start,
            )))
        }),
    );

    let _ = worker.join();
    runtime.block_on(session.shutdown());

    result.map_err(|err| anyhow::anyhow!("the window could not be opened: {err}"))
}

/// The processes this window owns.
struct Session {
    supervisor: Supervisor,
    net: Endpoint,
    /// Held so the shell keeps answering the agent for as long as the window is
    /// up. `None` when `--no-keys` was given.
    _shell: Option<Arc<Shell>>,
    _serving: Option<tokio::task::JoinHandle<()>>,
}

impl Session {
    async fn start(home: &std::path::Path, cli: &Cli, prompter: Arc<dyn Prompter>) -> Result<Self> {
        let mut supervisor = Supervisor::new(home);
        let net = supervisor.start_net(&cli.dns, &cli.peers).await?;

        if cli.no_keys {
            return Ok(Session {
                supervisor,
                net,
                _shell: None,
                _serving: None,
            });
        }

        let secret = syndeo_ipc::confirm::SessionSecret::generate();
        let keystore = supervisor.start_keystore(&secret).await?;
        unseal(&keystore, prompter.as_ref()).await?;

        // The agent is given this socket. The keystore endpoint stays here.
        let shell_endpoint = Endpoint::new(supervisor.runtime_dir().join("shell.sock"));
        let shell = Arc::new(Shell::new(
            Arc::new(syndeo_ipc::confirm::Confirmer::new(secret)),
            keystore,
            prompter,
        ));
        let server = Server::bind(shell_endpoint)?;
        let serving = tokio::spawn(shell.clone().serve(server));

        Ok(Session {
            supervisor,
            net,
            _shell: Some(shell),
            _serving: Some(serving),
        })
    }

    async fn shutdown(mut self) {
        if let Some(serving) = self._serving.take() {
            serving.abort();
        }
        self.supervisor.shutdown().await;
    }
}

/// Ask the keystore what it needs and supply it, through the window.
async fn unseal(keystore: &Endpoint, prompter: &dyn Prompter) -> Result<()> {
    let mut channel = Channel::connect(keystore).await?;
    let KeystoreResponse::Status {
        initialized,
        passphrase_required,
        ..
    } = channel.call(&KeystoreRequest::Status).await?
    else {
        bail!("the keystore did not report a status");
    };
    if !initialized {
        bail!("no keystore yet — run `syndeo-keystore init`, or start with --no-keys");
    }

    let passphrase = if passphrase_required {
        Some(prompter.read_passphrase("Keystore passphrase")?)
    } else {
        None
    };
    match channel
        .call(&KeystoreRequest::Unseal { passphrase })
        .await?
    {
        KeystoreResponse::Ok => Ok(()),
        KeystoreResponse::Error(e) => bail!(e),
        _ => bail!("unexpected reply from the keystore"),
    }
}

/// The worker: everything the window asks for that needs another process.
fn serve(
    handle: tokio::runtime::Handle,
    net: Endpoint,
    work: mpsc::Receiver<Work>,
    done: mpsc::Sender<Done>,
) {
    while let Ok(job) = work.recv() {
        match job {
            Work::Quit => return,
            Work::Load(url) => {
                let outcome = handle.block_on(load(&net, &url));
                let message = match outcome {
                    Ok(fetched) => Done::Loaded {
                        url,
                        fetched: Box::new(fetched),
                    },
                    Err(error) => Done::Failed {
                        url,
                        error: format!("{error:#}"),
                    },
                };
                if done.send(message).is_err() {
                    return;
                }
            }
            Work::Stats => {
                if let Ok(stats) = handle.block_on(stats(&net)) {
                    if done.send(Done::Stats(stats)).is_err() {
                        return;
                    }
                }
            }
            Work::Peers => {
                let message = match handle.block_on(peers(&net)) {
                    Ok(value) => Done::Peers(value),
                    Err(err) => Done::PeersUnavailable(format!("{err:#}")),
                };
                if done.send(message).is_err() {
                    return;
                }
            }
        }
    }
}

async fn load(net: &Endpoint, url: &str) -> Result<syndeo_ipc::protocol::Fetched> {
    let mut channel = Channel::connect(net).await?;
    Ok(channel
        .fetch(&NetRequest::Fetch {
            method: "GET".into(),
            url: url.to_string(),
            headers: vec![("accept".into(), "text/html,*/*".into())],
            body: Vec::new(),
            integrity: None,
        })
        .await?)
}

async fn stats(net: &Endpoint) -> Result<syndeo_cache::Stats> {
    let mut channel = Channel::connect(net).await?;
    match channel.call(&NetRequest::Stats).await? {
        NetResponse::Stats(value) => Ok(serde_json::from_value(value)?),
        NetResponse::Error(e) => bail!(e),
        _ => bail!("unexpected reply from the network process"),
    }
}

async fn peers(net: &Endpoint) -> Result<serde_json::Value> {
    let mut channel = Channel::connect(net).await?;
    match channel.call(&NetRequest::PeerStatus).await? {
        NetResponse::PeerStatus(value) => Ok(value),
        NetResponse::Error(e) => bail!(e),
        _ => bail!("unexpected reply from the network process"),
    }
}
