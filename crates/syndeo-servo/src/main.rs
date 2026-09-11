//! A window with a real renderer in it, loading nothing of its own.
//!
//! Deliberately small. It is not the shell — `syndeo-ui` is — and it is not
//! trying to be servoshell. It exists to demonstrate the one claim step four is
//! about: that a real renderer, with a real script engine and real layout,
//! fetches every byte through the network process and never opens a socket.
//!
//! Run it against a network process the shell started, or let it start one:
//!
//! ```sh
//! cargo run -p syndeo-servo --features renderer -- https://example.test/
//! ```

use anyhow::{Context, Result};
use clap::Parser;
use euclid::Scale;
use servo::{
    CSSPixel, Code, InputEvent, Key, KeyState, KeyboardEvent, Location, Modifiers,
    MouseButtonAction, MouseButtonEvent, MouseLeftViewportEvent, MouseMoveEvent, NamedKey,
    RenderingContext, Scroll, Servo, ServoBuilder, WebResourceLoad, WebView, WebViewBuilder,
    WebViewDelegate, WebViewPoint, WebViewVector, WheelDelta, WheelEvent, WheelMode,
    WindowRenderingContext,
};
use std::cell::{Cell, RefCell};
use std::path::PathBuf;
use std::rc::Rc;
use syndeo_ipc::transport::Endpoint;
use syndeo_servo::NetworkDelegate;
use url::Url;
use winit::application::ApplicationHandler;
use winit::event::{ElementState, MouseScrollDelta, WindowEvent};
use winit::event_loop::EventLoop;
use winit::raw_window_handle::{HasDisplayHandle, HasWindowHandle};
use winit::window::Window;

#[derive(Parser)]
#[command(
    name = "syndeo-servo",
    version,
    about = "A renderer whose every load goes through the network process"
)]
struct Cli {
    /// The page to open.
    url: String,
    /// Where cache, keys and sockets live.
    #[arg(long)]
    home: Option<PathBuf>,
    /// system | dot:cloudflare | doh:cloudflare | doh:google | doh:quad9
    #[arg(long, default_value = "system")]
    dns: String,
}

fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_env("SYNDEO_LOG")
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("syndeo_servo=info")),
        )
        .with_target(false)
        .with_writer(std::io::stderr)
        .init();

    let cli = Cli::parse();
    let home = cli.home.clone().unwrap_or_else(|| {
        std::env::var_os("SYNDEO_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| {
                std::env::var_os("HOME")
                    .map(PathBuf::from)
                    .unwrap_or_else(|| PathBuf::from("."))
                    .join(".syndeo")
            })
    });
    std::fs::create_dir_all(&home)?;
    let url = Url::parse(&cli.url).context("the url could not be parsed")?;

    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .context("starting the runtime")?;

    // The network process, exactly as every other front end starts it.
    let mut supervisor = syndeo_shell::Supervisor::new(&home);
    let net = runtime
        .block_on(supervisor.start_net(&cli.dns, &[]))
        .context("starting the network process")?;
    tracing::info!(socket = %net.path().display(), "the renderer will fetch through this and nothing else");

    let event_loop = EventLoop::with_user_event()
        .build()
        .context("creating the event loop")?;
    let mut app = App::Initial {
        waker: Waker(event_loop.create_proxy()),
        net,
        runtime: runtime.handle().clone(),
        url,
    };
    let outcome = event_loop.run_app(&mut app);

    runtime.block_on(supervisor.shutdown());
    outcome.context("the event loop failed")
}

struct Running {
    window: Window,
    _servo: Servo,
    rendering_context: Rc<WindowRenderingContext>,
    webviews: RefCell<Vec<WebView>>,
    /// Every resource load Servo is about to make is handed to this.
    network: Rc<NetworkDelegate>,
    /// Where the pointer was last seen, in physical pixels.
    ///
    /// A click and a wheel turn both have to say *where*, and winit only puts a
    /// position on the move event. Remembering the last one is the whole of it.
    cursor: Cell<(f32, f32)>,
}

impl Running {
    /// The cursor as Servo wants it: CSS pixels, not physical ones.
    ///
    /// Passing device pixels through on a Retina display puts every click at
    /// twice its real coordinates, which lands in the wrong element or in no
    /// element at all — and looks exactly like "clicking does nothing".
    fn cursor_point(&self) -> WebViewPoint {
        let (x, y) = self.cursor.get();
        let scale = self.window.scale_factor() as f32;
        euclid::Point2D::<f32, CSSPixel>::new(x / scale, y / scale).into()
    }

    fn with_webview(&self, act: impl FnOnce(&WebView)) {
        if let Some(webview) = self.webviews.borrow().last() {
            act(webview);
        }
    }
}

/// The delegate Servo talks to.
///
/// Two jobs: tell the window when there is a new frame, and — the one that
/// matters — hand every resource load to the network process.
impl WebViewDelegate for Running {
    fn notify_new_frame_ready(&self, _: WebView) {
        self.window.request_redraw();
    }

    fn load_web_resource(&self, webview: WebView, load: WebResourceLoad) {
        self.network.load_web_resource(webview, load);
    }
}

enum App {
    Initial {
        waker: Waker,
        net: Endpoint,
        runtime: tokio::runtime::Handle,
        url: Url,
    },
    Running(Rc<Running>),
}

impl ApplicationHandler<Woken> for App {
    fn resumed(&mut self, event_loop: &winit::event_loop::ActiveEventLoop) {
        let App::Initial {
            waker,
            net,
            runtime,
            url,
        } = self
        else {
            return;
        };
        let (waker, net, runtime, url) = (waker.clone(), net.clone(), runtime.clone(), url.clone());
        // Servo gets one, the delegate gets another: a finished fetch has to be
        // able to turn the loop just as much as a finished frame does.
        let waker_for_loads = waker.clone();

        let display_handle = event_loop
            .display_handle()
            .expect("a display handle for the window");
        let window = event_loop
            .create_window(Window::default_attributes().with_title("Syndeo — renderer"))
            .expect("a window");
        let window_handle = window.window_handle().expect("a handle for the window");

        let rendering_context = Rc::new(
            WindowRenderingContext::new(display_handle, window_handle, window.inner_size())
                .expect("a rendering context for the window"),
        );
        let _ = rendering_context.make_current();

        let servo = ServoBuilder::default()
            .event_loop_waker(Box::new(waker))
            .build();
        // Deliberately not `servo.setup_logging()`: it installs a global `log`
        // logger, and this process already has one. `tracing-subscriber`
        // bridges `log`, so Servo's records come out on the same stream as ours
        // rather than fighting it for the slot.

        let state = Rc::new(Running {
            window,
            _servo: servo,
            rendering_context,
            webviews: Default::default(),
            network: NetworkDelegate::new(net, runtime, Box::new(waker_for_loads)),
            cursor: Cell::new((0.0, 0.0)),
        });

        let webview = WebViewBuilder::new(&state._servo, state.rendering_context.clone())
            .url(url)
            .hidpi_scale_factor(Scale::new(state.window.scale_factor() as f32))
            .delegate(state.clone())
            .build();
        state.webviews.borrow_mut().push(webview);

        *self = App::Running(state);
    }

    fn user_event(&mut self, _event_loop: &winit::event_loop::ActiveEventLoop, _event: Woken) {
        if let App::Running(state) = self {
            // Answers first: a load that has come back should be applied before
            // Servo is asked what to do next.
            state.network.deliver();
            state._servo.spin_event_loop();
        }
    }

    fn window_event(
        &mut self,
        event_loop: &winit::event_loop::ActiveEventLoop,
        _window: winit::window::WindowId,
        event: WindowEvent,
    ) {
        if let App::Running(state) = self {
            state.network.deliver();
            state._servo.spin_event_loop();
        }
        match event {
            WindowEvent::CloseRequested => {
                if let App::Running(state) = self {
                    // Every load Servo made should be in this number. Anything
                    // missing from it went out over a socket we do not control,
                    // which is the failure this whole crate exists to prevent.
                    tracing::info!(
                        loads = state.network.intercepted(),
                        "every resource load went through the network process"
                    );
                }
                event_loop.exit()
            }
            WindowEvent::RedrawRequested => {
                if let App::Running(state) = self {
                    if let Some(webview) = state.webviews.borrow().last() {
                        webview.paint();
                    }
                    state.rendering_context.present();
                }
            }
            WindowEvent::Resized(size) => {
                if let App::Running(state) = self {
                    state.with_webview(|webview| webview.resize(size));
                }
            }

            // Everything below this line is what makes the page respond to a
            // person rather than only to itself. Without it Servo lays out,
            // paints, and then ignores the mouse entirely — which reads as a
            // broken renderer rather than an unfinished embedder.
            WindowEvent::CursorMoved { position, .. } => {
                if let App::Running(state) = self {
                    state.cursor.set((position.x as f32, position.y as f32));
                    let point = state.cursor_point();
                    state.with_webview(|webview| {
                        webview
                            .notify_input_event(InputEvent::MouseMove(MouseMoveEvent::new(point)));
                    });
                }
            }

            WindowEvent::CursorLeft { .. } => {
                if let App::Running(state) = self {
                    state.with_webview(|webview| {
                        webview.notify_input_event(InputEvent::MouseLeftViewport(
                            MouseLeftViewportEvent {
                                focus_moving_to_another_iframe: false,
                            },
                        ));
                    });
                }
            }

            WindowEvent::MouseInput {
                state: pressed,
                button,
                ..
            } => {
                if let App::Running(state) = self {
                    let action = match pressed {
                        ElementState::Pressed => MouseButtonAction::Down,
                        ElementState::Released => MouseButtonAction::Up,
                    };
                    let button = match button {
                        winit::event::MouseButton::Left => servo::MouseButton::Left,
                        winit::event::MouseButton::Right => servo::MouseButton::Right,
                        winit::event::MouseButton::Middle => servo::MouseButton::Middle,
                        winit::event::MouseButton::Back => servo::MouseButton::Back,
                        winit::event::MouseButton::Forward => servo::MouseButton::Forward,
                        winit::event::MouseButton::Other(other) => servo::MouseButton::Other(other),
                    };
                    let point = state.cursor_point();
                    // Logged, because "clicking does nothing" and "the click
                    // never reached the page" look identical from outside and
                    // are fixed in different places.
                    tracing::debug!(?action, ?button, ?point, "input: mouse button");
                    state.with_webview(|webview| {
                        webview.notify_input_event(InputEvent::MouseButton(MouseButtonEvent::new(
                            action, button, point,
                        )));
                    });
                    state.window.request_redraw();
                }
            }

            WindowEvent::MouseWheel { delta, .. } => {
                if let App::Running(state) = self {
                    // A line is not a unit Servo knows how to size, so lines are
                    // turned into pixels here and the mode is always pixels.
                    const LINE_HEIGHT: f32 = 38.0;
                    let (dx, dy) = match delta {
                        MouseScrollDelta::LineDelta(x, y) => (x * LINE_HEIGHT, y * LINE_HEIGHT),
                        MouseScrollDelta::PixelDelta(position) => {
                            (position.x as f32, position.y as f32)
                        }
                    };

                    let point = state.cursor_point();
                    tracing::debug!(dx, dy, ?point, "input: wheel");
                    state.with_webview(|webview| {
                        // Two notifications, and both are needed. The wheel
                        // event is what a page listening for `wheel` sees and
                        // may cancel; the scroll is what actually moves the
                        // viewport, for the overwhelming majority of pages that
                        // listen for nothing. Sending only the first is a page
                        // that reports scrolling and never moves.
                        webview.notify_input_event(InputEvent::Wheel(WheelEvent::new(
                            WheelDelta {
                                x: dx as f64,
                                y: dy as f64,
                                z: 0.0,
                                mode: WheelMode::DeltaPixel,
                            },
                            point,
                        )));
                        // Negated, because the two conventions are opposites: a
                        // positive wheel delta reveals content *above*, and a
                        // positive scroll delta reveals content *below*.
                        webview.notify_scroll_event(
                            Scroll::Delta(WebViewVector::Page(euclid::Vector2D::new(-dx, -dy))),
                            point,
                        );
                    });
                    state.window.request_redraw();
                }
            }

            WindowEvent::KeyboardInput { event, .. } => {
                if let App::Running(state) = self {
                    if let Some(keyboard) = keyboard_event(&event) {
                        state.with_webview(|webview| {
                            webview.notify_input_event(InputEvent::Keyboard(keyboard.clone()));
                        });
                        state.window.request_redraw();
                    }
                }
            }

            _ => {}
        }
    }
}

#[derive(Clone)]
struct Waker(winit::event_loop::EventLoopProxy<Woken>);

#[derive(Debug)]
struct Woken;

impl servo::EventLoopWaker for Waker {
    fn clone_box(&self) -> Box<dyn servo::EventLoopWaker> {
        Box::new(self.clone())
    }

    fn wake(&self) {
        if let Err(error) = self.0.send_event(Woken) {
            tracing::warn!(?error, "could not wake the event loop");
        }
    }
}

/// winit's key event, in the shape `keyboard_types` uses.
///
/// Deliberately partial. Text and the keys that move a page are here — which is
/// what typing into a field and scrolling with the keyboard need — and the rest
/// arrives as `Unidentified` rather than as nothing, so a page that inspects
/// `key` sees an event it can ignore instead of never being told.
fn keyboard_event(event: &winit::event::KeyEvent) -> Option<KeyboardEvent> {
    use winit::keyboard::{Key as WinitKey, NamedKey as WinitNamed};

    let state = match event.state {
        ElementState::Pressed => KeyState::Down,
        ElementState::Released => KeyState::Up,
    };

    let key = match &event.logical_key {
        WinitKey::Character(text) => Key::Character(text.to_string()),
        WinitKey::Named(named) => Key::Named(match named {
            WinitNamed::Enter => NamedKey::Enter,
            WinitNamed::Tab => NamedKey::Tab,
            WinitNamed::Space => {
                return Some(KeyboardEvent::new_without_event(
                    state,
                    Key::Character(" ".to_owned()),
                    Code::Space,
                    Location::Standard,
                    Modifiers::empty(),
                    event.repeat,
                    false,
                ))
            }
            WinitNamed::ArrowDown => NamedKey::ArrowDown,
            WinitNamed::ArrowLeft => NamedKey::ArrowLeft,
            WinitNamed::ArrowRight => NamedKey::ArrowRight,
            WinitNamed::ArrowUp => NamedKey::ArrowUp,
            WinitNamed::End => NamedKey::End,
            WinitNamed::Home => NamedKey::Home,
            WinitNamed::PageDown => NamedKey::PageDown,
            WinitNamed::PageUp => NamedKey::PageUp,
            WinitNamed::Backspace => NamedKey::Backspace,
            WinitNamed::Delete => NamedKey::Delete,
            WinitNamed::Escape => NamedKey::Escape,
            _ => return None,
        }),
        _ => return None,
    };

    Some(KeyboardEvent::new_without_event(
        state,
        key,
        Code::Unidentified,
        Location::Standard,
        Modifiers::empty(),
        event.repeat,
        false,
    ))
}
