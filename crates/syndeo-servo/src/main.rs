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
    /// Scroll this many times on its own, report how long each frame took, and
    /// exit.
    ///
    /// Driving a window from a test script means synthetic input, which means
    /// depending on which window the operating system thinks is focused — and
    /// that is decided by whoever is using the machine at the time. This drives
    /// the same code path a wheel event does, from inside, so the measurement
    /// is the renderer's rather than the window server's.
    #[arg(long, value_name = "COUNT")]
    scroll_bench: Option<usize>,
    /// Window size, as WIDTHxHEIGHT. Compositing cost is per pixel, so a
    /// measurement at the default size says nothing about a maximised window.
    #[arg(long, value_name = "WxH")]
    window_size: Option<String>,
    /// Milliseconds between synthetic scrolls in `--scroll-bench`. A trackpad
    /// is about 8; anything under one frame is what coalescing exists for.
    #[arg(long, default_value_t = 8, value_name = "MS")]
    scroll_rate_ms: u64,
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
    let proxy = event_loop.create_proxy();
    // The benchmark is paced from a thread rather than from a timer inside the
    // loop, so that a slow frame delays the next scroll exactly as a finger
    // would not: the input keeps coming at a steady rate whatever the renderer
    // is doing, which is the situation being measured.
    if let Some(count) = cli.scroll_bench {
        let proxy = proxy.clone();
        let rate = cli.scroll_rate_ms;
        std::thread::spawn(move || {
            // Long enough for the page to have loaded and settled.
            std::thread::sleep(std::time::Duration::from_secs(12));
            for _ in 0..count {
                if proxy.send_event(Woken).is_err() {
                    return;
                }
                std::thread::sleep(std::time::Duration::from_millis(rate));
            }
        });
    }
    let mut app = App::Initial {
        waker: Waker(proxy),
        net,
        runtime: runtime.handle().clone(),
        url,
        bench: cli.scroll_bench.unwrap_or(0),
        size: cli.window_size.as_deref().and_then(|s| {
            let (w, h) = s.split_once(['x', 'X'])?;
            Some((w.trim().parse().ok()?, h.trim().parse().ok()?))
        }),
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
    /// Which modifiers are held, so a shortcut can be told from a keystroke.
    modifiers: Cell<Modifiers>,
    /// Wheel deltas that have arrived but have not been handed to Servo yet.
    ///
    /// A trackpad produces around a hundred and twenty of these a second, and
    /// one composite costs about eight milliseconds — so notifying per event
    /// asks for roughly a full core just to scroll, and every hiccup then shows
    /// as the page falling behind the finger. They are summed here and handed
    /// over once per turn of the loop instead, which is what every other
    /// browser does: the cost becomes one per *frame* rather than one per
    /// *event*, and is bounded by the display rather than by the input.
    pending_scroll: Cell<(f32, f32)>,
    /// Scrolls still to perform in `--scroll-bench`, and how long each frame
    /// took while it ran.
    bench: Cell<usize>,
    fed: Cell<usize>,
    frames: RefCell<Vec<u128>>,
    last_paint: Cell<Option<std::time::Instant>>,
    /// Horizontal scroll accumulated since the last navigation.
    ///
    /// A swipe is not an event, it is a few dozen small deltas, so the decision
    /// is a running total against a threshold rather than a test on any one of
    /// them.
    swipe: Cell<f32>,
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

    /// Hand Servo everything the wheel has produced since the last turn.
    fn flush_scroll(&self) {
        let (dx, dy) = self.pending_scroll.replace((0.0, 0.0));
        if dx == 0.0 && dy == 0.0 {
            return;
        }
        let point = self.cursor_point();
        self.with_webview(|webview| {
            // Two notifications, and both are needed. The wheel event is what a
            // page listening for `wheel` sees and may cancel; the scroll is what
            // actually moves the viewport, for the overwhelming majority of
            // pages that listen for nothing. Sending only the first is a page
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
            // Negated, because the two conventions are opposites: a positive
            // wheel delta reveals content *above*, and a positive scroll delta
            // reveals content *below*.
            webview.notify_scroll_event(
                Scroll::Delta(WebViewVector::Page(euclid::Vector2D::new(-dx, -dy))),
                point,
            );
        });
    }

    fn benching(&self) -> bool {
        self.fed.get() > 0
    }

    /// Back and forward, however they were asked for.
    ///
    /// There is no button to press, and there cannot be one yet: drawing chrome
    /// over the page means one surface shared between our own renderer and
    /// WebRender, which is a piece of work in its own right. Until then this
    /// window is reached by gesture, by keyboard and by the two side buttons on
    /// a mouse — which is how most people navigate anyway, and none of which
    /// needs a pixel of chrome.
    fn navigate(&self, direction: Direction) {
        tracing::debug!(?direction, "navigating");
        self.swipe.set(0.0);
        self.with_webview(|webview| match direction {
            Direction::Back => {
                webview.go_back(1);
            }
            Direction::Forward => {
                webview.go_forward(1);
            }
        });
        self.window.request_redraw();
    }
}

impl App {
    /// One frame's work, timed: pump, composite, present.
    ///
    /// Called directly rather than through `RedrawRequested`, because macOS
    /// does not ask an occluded window to redraw — and a benchmark that waited
    /// to be asked measured nothing whenever another window was in front, which
    /// is most of the time on a machine somebody is using.
    fn compose(state: &Rc<Running>) {
        let started = std::time::Instant::now();
        state.network.deliver();
        state._servo.spin_event_loop();
        state.with_webview(|webview| webview.paint());
        state.rendering_context.present();
        state
            .frames
            .borrow_mut()
            .push(started.elapsed().as_millis());
    }
}

#[derive(Debug, Clone, Copy)]
enum Direction {
    Back,
    Forward,
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
        bench: usize,
        size: Option<(u32, u32)>,
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
            bench,
            size,
        } = self
        else {
            return;
        };
        let bench = *bench;
        let size = *size;
        let (waker, net, runtime, url) = (waker.clone(), net.clone(), runtime.clone(), url.clone());
        // Servo gets one, the delegate gets another: a finished fetch has to be
        // able to turn the loop just as much as a finished frame does.
        let waker_for_loads = waker.clone();

        let display_handle = event_loop
            .display_handle()
            .expect("a display handle for the window");
        let mut attributes = Window::default_attributes().with_title("Syndeo — renderer");
        if let Some((w, h)) = size {
            attributes = attributes.with_inner_size(winit::dpi::LogicalSize::new(w, h));
        }
        let window = event_loop.create_window(attributes).expect("a window");
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
            modifiers: Cell::new(Modifiers::empty()),
            swipe: Cell::new(0.0),
            pending_scroll: Cell::new((0.0, 0.0)),
            bench: Cell::new(bench),
            fed: Cell::new(0),
            frames: RefCell::new(Vec::new()),
            last_paint: Cell::new(None),
        });

        let webview = WebViewBuilder::new(&state._servo, state.rendering_context.clone())
            .url(url)
            .hidpi_scale_factor(Scale::new(state.window.scale_factor() as f32))
            .delegate(state.clone())
            .build();
        state.webviews.borrow_mut().push(webview);

        *self = App::Running(state);
    }

    /// Something woke the loop — a finished fetch, or a frame Servo wants.
    ///
    /// Deliberately empty. Waking is enough: winit calls `about_to_wait` once
    /// it has drained everything that was pending, and that is where the work
    /// happens. Pumping here as well would pump once per wake-up instead of
    /// once per batch.
    fn user_event(&mut self, event_loop: &winit::event_loop::ActiveEventLoop, _event: Woken) {
        let App::Running(state) = self else { return };
        let remaining = state.bench.get();
        if remaining == 0 {
            return;
        }

        // Fed into the same accumulator a real wheel event goes into, so the
        // benchmark exercises the coalescing rather than stepping around it.
        let (px, py) = state.pending_scroll.get();
        state.pending_scroll.set((px, py - 10.0));
        state.fed.set(state.fed.get() + 1);
        state.bench.set(remaining - 1);

        if remaining == 1 {
            // One last turn, so the deltas fed above are not left unflushed.
            state.flush_scroll();
            Self::compose(state);
            let mut sorted: Vec<u128> = state.frames.borrow().clone();
            sorted.sort_unstable();
            if sorted.is_empty() {
                println!("scroll-bench: no frames were painted");
            } else {
                let at = |q: f64| sorted[((sorted.len() - 1) as f64 * q) as usize];
                let over = sorted.iter().filter(|ms| **ms > 16).count();
                println!(
                    "scroll-bench: {} wheel events -> {} composites  \
                     median {}ms  p90 {}ms  max {}ms  over 16ms: {} ({:.0}%)",
                    state.fed.get(),
                    sorted.len(),
                    at(0.5),
                    at(0.9),
                    sorted[sorted.len() - 1],
                    over,
                    100.0 * over as f64 / sorted.len() as f64,
                );
            }
            event_loop.exit();
        }
    }

    /// One pump per batch of events, rather than one per event.
    ///
    /// `spin_event_loop` drives the whole of Servo — script, layout, the
    /// compositor. Calling it from the top of `window_event` meant doing all of
    /// that again for every single mouse move, and a trackpad produces those by
    /// the hundred per second. The work was quadratic in how much the user
    /// moved, which is exactly the shape of "it gets laggy when I scroll".
    ///
    /// winit calls this once it has nothing left to deliver, so a burst of
    /// forty scroll deltas now costs one pump instead of forty.
    fn about_to_wait(&mut self, _event_loop: &winit::event_loop::ActiveEventLoop) {
        if let App::Running(state) = self {
            let scrolled = state.pending_scroll.get() != (0.0, 0.0);
            state.flush_scroll();
            if scrolled && state.benching() {
                Self::compose(state);
            }

            // Answers first: a load that has come back should be applied before
            // Servo is asked what to do next.
            let started = std::time::Instant::now();

            // Loaded resources arrive in waves, not all at once: the document
            // names the stylesheets, the stylesheets name the fonts, and each
            // wave is only discovered once the previous one has been handed to
            // Servo and acted on. Doing one wave per turn of the event loop put
            // a frame between every wave — and on a page with seventy-eight
            // subresources that is most of two seconds, on a *warm cache*, with
            // no network involved at all. Measured, not guessed.
            //
            // So: keep going while answers are still landing, rather than
            // waiting to be woken again for each wave. Bounded, because a page
            // that keeps producing loads must not be allowed to hold the event
            // loop and with it every click and keystroke.
            const BUDGET: std::time::Duration = std::time::Duration::from_millis(100);
            let mut delivered = std::time::Duration::ZERO;
            loop {
                let applied = state.network.deliver();
                delivered = started.elapsed();
                state._servo.spin_event_loop();
                if applied == 0 || started.elapsed() > BUDGET {
                    break;
                }
            }
            let total = started.elapsed();

            // A frame is 16ms at sixty a second. Anything past that is a frame
            // the user did not get, so it is worth being able to see which half
            // took it — applying fetched bytes, or Servo's own script, layout
            // and compositing.
            if total > std::time::Duration::from_millis(16) {
                tracing::debug!(
                    total_ms = total.as_millis(),
                    deliver_ms = delivered.as_millis(),
                    "slow pump"
                );
            }
        }
    }

    fn window_event(
        &mut self,
        event_loop: &winit::event_loop::ActiveEventLoop,
        _window: winit::window::WindowId,
        event: WindowEvent,
    ) {
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
                    let started = std::time::Instant::now();
                    if let Some(webview) = state.webviews.borrow().last() {
                        webview.paint();
                    }
                    state.rendering_context.present();
                    let painted = started.elapsed();
                    if painted > std::time::Duration::from_millis(16) {
                        tracing::debug!(ms = painted.as_millis(), "slow paint");
                    }
                    // The interval between frames, which is what smoothness
                    // actually is: how long the previous frame stayed on screen.
                    if let Some(previous) = state.last_paint.replace(Some(started)) {
                        state
                            .frames
                            .borrow_mut()
                            .push(previous.elapsed().as_millis());
                    }
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

            WindowEvent::ModifiersChanged(changed) => {
                if let App::Running(state) = self {
                    let winit = changed.state();
                    let mut modifiers = Modifiers::empty();
                    modifiers.set(Modifiers::SHIFT, winit.shift_key());
                    modifiers.set(Modifiers::CONTROL, winit.control_key());
                    modifiers.set(Modifiers::ALT, winit.alt_key());
                    modifiers.set(Modifiers::META, winit.super_key());
                    state.modifiers.set(modifiers);
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
                    // The two side buttons on a mouse are navigation
                    // everywhere else; making them a page click here would be
                    // the surprising choice.
                    if action == MouseButtonAction::Down {
                        match button {
                            servo::MouseButton::Back => {
                                state.navigate(Direction::Back);
                                return;
                            }
                            servo::MouseButton::Forward => {
                                state.navigate(Direction::Forward);
                                return;
                            }
                            _ => {}
                        }
                    }

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

                    // A decisive sideways gesture is navigation, the way it is
                    // in every other browser. "Decisive" is doing real work
                    // here: a vertical scroll with a little sideways drift in it
                    // is not a swipe, and treating it as one would send someone
                    // back a page while they were reading.
                    if dx.abs() > dy.abs() * 2.0 {
                        let swiped = state.swipe.get() + dx;
                        state.swipe.set(swiped);
                        if swiped.abs() >= SWIPE_THRESHOLD {
                            // Swiping right reveals what was to the left of the
                            // page, which is where you came from.
                            state.navigate(if swiped > 0.0 {
                                Direction::Back
                            } else {
                                Direction::Forward
                            });
                            return;
                        }
                    } else if dy != 0.0 {
                        // Scrolling vertically ends whatever sideways gesture
                        // was half-finished, rather than leaving it to be
                        // completed minutes later by an unrelated nudge.
                        state.swipe.set(0.0);
                    }

                    tracing::trace!(dx, dy, "input: wheel");
                    let (px, py) = state.pending_scroll.get();
                    state.pending_scroll.set((px + dx, py + dy));
                    state.window.request_redraw();
                }
            }

            WindowEvent::KeyboardInput { event, .. } => {
                if let App::Running(state) = self {
                    if let Some(direction) = shortcut(&event, state.modifiers.get()) {
                        state.navigate(direction);
                        return;
                    }
                    if let Some(keyboard) = keyboard_event(&event, state.modifiers.get()) {
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
fn keyboard_event(event: &winit::event::KeyEvent, modifiers: Modifiers) -> Option<KeyboardEvent> {
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
        modifiers,
        event.repeat,
        false,
    ))
}

/// How far sideways a gesture has to travel before it means "go back".
///
/// Low enough that a deliberate swipe reaches it in one motion, high enough
/// that the sideways drift in an ordinary vertical scroll never does.
const SWIPE_THRESHOLD: f32 = 220.0;

/// The keyboard shortcuts that navigate, on the modifier each platform uses.
///
/// Command-arrow on macOS, Alt-arrow elsewhere — matching every browser on the
/// respective platform rather than picking one and making both wrong.
fn shortcut(event: &winit::event::KeyEvent, modifiers: Modifiers) -> Option<Direction> {
    use winit::keyboard::{Key as WinitKey, NamedKey as WinitNamed};

    if event.state != ElementState::Pressed {
        return None;
    }

    let navigating = if cfg!(target_os = "macos") {
        modifiers.contains(Modifiers::META)
    } else {
        modifiers.contains(Modifiers::ALT)
    };
    if !navigating {
        return None;
    }

    match &event.logical_key {
        WinitKey::Named(WinitNamed::ArrowLeft) => Some(Direction::Back),
        WinitKey::Named(WinitNamed::ArrowRight) => Some(Direction::Forward),
        WinitKey::Character(text) if text == "[" => Some(Direction::Back),
        WinitKey::Character(text) if text == "]" => Some(Direction::Forward),
        _ => None,
    }
}
