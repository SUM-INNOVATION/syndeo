//! The window.
//!
//! egui for the chrome, drawn on a wgpu surface in a winit window, with
//! accesskit publishing the tree — the same stack servoshell uses, so there is a
//! working reference for the day a renderer needs a surface to draw into.
//!
//! The window never opens a socket and never sees a key. It asks the network
//! process for bytes and the shell for signatures, which is the same boundary
//! the terminal front end sits behind.

use crate::page::{self, Page};
use crate::prompt::Ask;
use egui::{Align, Color32, Layout, RichText};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, Sender, SyncSender};
use std::sync::Arc;
use syndeo_dom::Document;
use syndeo_shell::prompt::{prompt_lines, Decision, SignatureRequest};

/// What the worker thread has been asked to do.
pub enum Work {
    Load(String),
    Stats,
    Peers,
    Quit,
}

/// What came back.
pub enum Done {
    /// The bytes and how they arrived. Parsed into a [`Page`] on the UI thread,
    /// because a parsed document holds `Rc`s and cannot cross one.
    Loaded {
        url: String,
        fetched: Box<syndeo_ipc::protocol::Fetched>,
    },
    Failed {
        url: String,
        error: String,
    },
    Stats(syndeo_cache::Stats),
    Peers(serde_json::Value),
    PeersUnavailable(String),
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Pane {
    Reader,
    Links,
    Subresources,
    Forms,
}

/// A question from the shell that is currently on screen.
enum Modal {
    Sign {
        request: Box<SignatureRequest>,
        answer: SyncSender<Decision>,
    },
    Confirm {
        title: String,
        detail: String,
        answer: SyncSender<Decision>,
    },
    Passphrase {
        label: String,
        typed: String,
        answer: SyncSender<Option<String>>,
    },
}

pub struct App {
    work: Sender<Work>,
    done: Receiver<Done>,
    asks: Receiver<Ask>,
    closed: Arc<AtomicBool>,

    address: String,
    page: Option<Page>,
    loading: bool,
    error: Option<String>,
    history: Vec<String>,
    pane: Pane,

    stats: Option<syndeo_cache::Stats>,
    peers: Option<serde_json::Value>,
    peers_note: Option<String>,
    show_cache: bool,
    show_peers: bool,

    modal: Option<Modal>,
    /// The page's own title, put on the window the way a browser does.
    title: Option<String>,
    titled: Option<String>,
}

impl App {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        context: &egui::Context,
        work: Sender<Work>,
        done: Receiver<Done>,
        asks: Receiver<Ask>,
        closed: Arc<AtomicBool>,
        start: Option<String>,
    ) -> Self {
        context.all_styles_mut(|style| {
            style.spacing.item_spacing = egui::vec2(8.0, 6.0);
            style.visuals.button_frame = true;
        });

        let address = start.clone().unwrap_or_default();
        if let Some(url) = start {
            let _ = work.send(Work::Load(url));
        }
        let _ = work.send(Work::Stats);

        App {
            work,
            done,
            asks,
            closed,
            address,
            page: None,
            loading: false,
            error: None,
            history: Vec::new(),
            pane: Pane::Reader,
            stats: None,
            peers: None,
            peers_note: None,
            show_cache: false,
            show_peers: false,
            modal: None,
            title: None,
            titled: None,
        }
    }

    fn load(&mut self, url: impl Into<String>) {
        let mut url = url.into();
        let trimmed = url.trim().to_string();
        if trimmed.is_empty() {
            return;
        }
        // A bare host is a URL somebody has not finished typing.
        url = if trimmed.contains("://") {
            trimmed
        } else {
            format!("https://{trimmed}")
        };

        if let Some(current) = &self.page {
            self.history.push(current.url.clone());
        }
        self.address = url.clone();
        self.loading = true;
        self.error = None;
        let _ = self.work.send(Work::Load(url));
    }

    fn back(&mut self) {
        if let Some(previous) = self.history.pop() {
            self.address = previous.clone();
            self.loading = true;
            self.error = None;
            let _ = self.work.send(Work::Load(previous));
        }
    }

    fn drain(&mut self) {
        while let Ok(done) = self.done.try_recv() {
            match done {
                Done::Loaded { url, fetched } => {
                    self.loading = false;
                    self.error = None;
                    self.address = url.clone();
                    let page = page_from(url, *fetched);
                    self.title = Some(page.title());
                    self.page = Some(page);
                    self.pane = Pane::Reader;
                    let _ = self.work.send(Work::Stats);
                }
                Done::Failed { url, error } => {
                    self.loading = false;
                    self.error = Some(format!("{url}: {error}"));
                }
                Done::Stats(stats) => self.stats = Some(stats),
                Done::Peers(value) => {
                    self.peers = Some(value);
                    self.peers_note = None;
                }
                Done::PeersUnavailable(why) => {
                    self.peers = None;
                    self.peers_note = Some(why);
                }
            }
        }

        // One question at a time. A second signing request while one is on
        // screen waits its turn rather than replacing it.
        if self.modal.is_none() {
            if let Ok(ask) = self.asks.try_recv() {
                self.modal = Some(match ask {
                    Ask::Sign { request, answer } => Modal::Sign { request, answer },
                    Ask::Confirm {
                        title,
                        detail,
                        answer,
                    } => Modal::Confirm {
                        title,
                        detail,
                        answer,
                    },
                    Ask::Passphrase { label, answer } => Modal::Passphrase {
                        label,
                        typed: String::new(),
                        answer,
                    },
                });
            }
        }
    }

    fn chrome(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            let can_go_back = !self.history.is_empty();
            if name(
                ui.add_enabled(can_go_back, egui::Button::new("←")),
                "Back to the previous page",
            )
            .on_hover_text("Back")
            .clicked()
            {
                self.back();
            }

            let reload = name(ui.button("⟳"), "Reload this page")
                .on_hover_text("Reload")
                .clicked();
            if reload {
                let url = self.address.clone();
                self.loading = true;
                let _ = self.work.send(Work::Load(url));
            }

            let field = egui::TextEdit::singleline(&mut self.address)
                .hint_text("Address")
                .desired_width(f32::INFINITY);
            let response = name(ui.add(field), "Address");
            if response.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                let url = self.address.clone();
                self.load(url);
            }
        });

        ui.horizontal(|ui| {
            if let Some(page) = &self.page {
                name(
                    badge(ui, &page.source, source_colour(&page.source)),
                    &format!("Served from {}", page.source),
                )
                .on_hover_text(
                    "Where the bytes came from. `cache` means nothing crossed the network.",
                );
                name(
                    badge(ui, &page.protocol, Color32::from_rgb(110, 120, 150)),
                    &format!("Carried over {}", page.protocol),
                )
                .on_hover_text("Which protocol carried it, when one did.");
                ui.label(
                    RichText::new(format!(
                        "{}  {}  {}",
                        page.status,
                        syndeo_cache::stats::human(page.bytes as u64),
                        format_args!("{}ms", page.elapsed_ms)
                    ))
                    .size(12.0)
                    .color(Color32::GRAY),
                );
                if let Some(content) = &page.content {
                    ui.label(
                        RichText::new(format!("blake3:{}", &content[..12.min(content.len())]))
                            .size(11.0)
                            .monospace()
                            .color(Color32::DARK_GRAY),
                    )
                    .on_hover_text(content);
                }
            } else if self.loading {
                ui.spinner();
                ui.label(RichText::new("loading").size(12.0).color(Color32::GRAY));
            }

            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if ui.selectable_label(self.show_peers, "Peers").clicked() {
                    self.show_peers = !self.show_peers;
                    if self.show_peers {
                        let _ = self.work.send(Work::Peers);
                    }
                }
                if ui.selectable_label(self.show_cache, "Cache").clicked() {
                    self.show_cache = !self.show_cache;
                    if self.show_cache {
                        let _ = self.work.send(Work::Stats);
                    }
                }
            });
        });
    }

    fn body(&mut self, ui: &mut egui::Ui) {
        if let Some(error) = &self.error {
            ui.add_space(20.0);
            ui.label(RichText::new(error).color(Color32::from_rgb(190, 90, 90)));
            return;
        }
        let Some(page) = &self.page else {
            ui.add_space(40.0);
            ui.vertical_centered(|ui| {
                ui.label(
                    RichText::new("Type an address above.")
                        .size(15.0)
                        .color(Color32::GRAY),
                );
                ui.add_space(6.0);
                ui.label(
                    RichText::new(
                        "Nothing in this window opens a socket. It asks the network process \
                         for bytes and the shell for signatures.",
                    )
                    .size(12.0)
                    .color(Color32::DARK_GRAY),
                );
            });
            return;
        };

        ui.horizontal(|ui| {
            ui.selectable_value(&mut self.pane, Pane::Reader, "Reader");
            ui.selectable_value(&mut self.pane, Pane::Links, "Links");
            ui.selectable_value(&mut self.pane, Pane::Subresources, "Subresources");
            ui.selectable_value(&mut self.pane, Pane::Forms, "Forms");
        });
        ui.separator();

        let mut follow = None;
        egui::ScrollArea::vertical()
            .auto_shrink([false, false])
            .show(ui, |ui| match self.pane {
                Pane::Reader => {
                    ui.label(
                        RichText::new(
                            "Reader view: this is the headless DOM, not a rendered page. \
                             Step four of the build order puts a renderer behind this pane.",
                        )
                        .size(11.0)
                        .italics()
                        .color(Color32::DARK_GRAY),
                    );
                    ui.add_space(6.0);
                    page::reader(ui, page);
                }
                Pane::Links => follow = page::links(ui, page),
                Pane::Subresources => page::subresources(ui, page),
                Pane::Forms => page::forms(ui, page),
            });

        if let Some(url) = follow {
            self.load(url);
        }
    }

    fn cache_panel(&mut self, host: &mut egui::Ui) {
        // Taken out of `self` so the panel can own it: dragging the edge closed
        // has to change the same flag the button does, or the two disagree.
        let mut expanded = self.show_cache;
        let mut closed_by_button = false;
        let work = self.work.clone();
        let stats = self.stats.clone();
        egui::Panel::right("cache")
            .resizable(true)
            .default_size(300.0)
            .show_collapsible(host, &mut expanded, |ui| {
                ui.horizontal(|ui| {
                    ui.heading("Cache");
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        if name(ui.small_button("✕"), "Close the cache panel").clicked() {
                            closed_by_button = true;
                        }
                        if name(ui.small_button("⟳"), "Refresh the cache statistics").clicked() {
                            let _ = work.send(Work::Stats);
                        }
                    });
                });
                ui.separator();

                let Some(stats) = &stats else {
                    ui.label(RichText::new("No statistics yet.").color(Color32::GRAY));
                    return;
                };
                egui::Grid::new("cache-stats")
                    .num_columns(2)
                    .striped(true)
                    .show(ui, |ui| {
                        let mut row = |name: &str, value: String| {
                            ui.label(RichText::new(name).size(12.0).color(Color32::GRAY));
                            ui.label(RichText::new(value).size(12.0).monospace());
                            ui.end_row();
                        };
                        row("requests", stats.requests.to_string());
                        row("hits", stats.hits.to_string());
                        row("stale hits", stats.stale_hits.to_string());
                        row("misses", stats.misses.to_string());
                        row("revalidations", stats.revalidations.to_string());
                        row("hit rate", format!("{:.1}%", stats.hit_rate() * 100.0));
                        row(
                            "byte hit rate",
                            format!("{:.1}%", stats.byte_hit_rate() * 100.0),
                        );
                        row("entries", stats.entries.to_string());
                        row("blobs", stats.blobs.to_string());
                        row("dedupe", format!("{:.2}x", stats.dedupe_ratio()));
                        row("compression", format!("{:.2}x", stats.compression_ratio()));
                        row("on disk", syndeo_cache::stats::human(stats.on_disk_bytes));
                        row("range hits", stats.range_hits.to_string());
                        row("partial stores", stats.partial_stores.to_string());
                        row("evictions", stats.evictions.to_string());
                        row("peer accepted", stats.peer_accepted.to_string());
                        row("peer rejected", stats.peer_rejected.to_string());
                    });
            });
        self.show_cache = expanded && !closed_by_button;
    }

    fn peers_panel(&mut self, host: &mut egui::Ui) {
        let mut expanded = self.show_peers;
        let mut closed_by_button = false;
        let work = self.work.clone();
        let peers = self.peers.clone();
        let note = self.peers_note.clone();
        egui::Panel::right("peers")
            .resizable(true)
            .default_size(360.0)
            .show_collapsible(host, &mut expanded, |ui| {
                ui.horizontal(|ui| {
                    ui.heading("Peers");
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        if name(ui.small_button("✕"), "Close the peers panel").clicked() {
                            closed_by_button = true;
                        }
                        if name(ui.small_button("⟳"), "Refresh the peer list").clicked() {
                            let _ = work.send(Work::Peers);
                        }
                    });
                });
                ui.separator();

                if let Some(note) = &note {
                    ui.label(RichText::new(note).color(Color32::GRAY).size(12.0));
                    ui.add_space(8.0);
                    ui.label(
                        RichText::new(
                            "Start with --peer to join the swarm. A peer is only ever asked \
                             for a body the page already named by hash.",
                        )
                        .size(11.0)
                        .italics()
                        .color(Color32::DARK_GRAY),
                    );
                    return;
                }
                let Some(peers) = &peers else {
                    ui.label(RichText::new("Asking…").color(Color32::GRAY));
                    return;
                };

                ui.label(
                    RichText::new(peers["peer_id"].as_str().unwrap_or("?"))
                        .monospace()
                        .size(11.0),
                );
                ui.label(
                    RichText::new(format!(
                        "routing table {}   announced {}",
                        peers["routing_table"], peers["announced"]
                    ))
                    .size(12.0)
                    .color(Color32::GRAY),
                );
                ui.add_space(6.0);

                let connected = peers["connected"].as_array().cloned().unwrap_or_default();
                if connected.is_empty() {
                    ui.label(
                        RichText::new("No peers connected.")
                            .color(Color32::GRAY)
                            .size(12.0),
                    );
                    return;
                }
                egui::Grid::new("peer-ledger")
                    .num_columns(3)
                    .striped(true)
                    .show(ui, |ui| {
                        ui.label(RichText::new("peer").size(11.0).color(Color32::GRAY));
                        ui.label(RichText::new("gave").size(11.0).color(Color32::GRAY));
                        ui.label(RichText::new("took").size(11.0).color(Color32::GRAY));
                        ui.end_row();
                        for report in &connected {
                            let id = report["peer"].as_str().unwrap_or_default();
                            ui.label(RichText::new(page::truncate(id, 22)).monospace().size(11.0))
                                .on_hover_text(id);
                            ui.label(RichText::new(report["received"].to_string()).size(11.0));
                            ui.label(RichText::new(report["served"].to_string()).size(11.0));
                            ui.end_row();
                        }
                    });
                ui.add_space(6.0);
                ui.label(
                    RichText::new(
                        "A peer never learns a URL from us — only which hashes we want, and \
                         when. That is not the same as anonymous.",
                    )
                    .size(11.0)
                    .italics()
                    .color(Color32::DARK_GRAY),
                );
            });
        self.show_peers = expanded && !closed_by_button;
    }

    /// The dialog this window exists for.
    ///
    /// What it shows of the request is [`prompt_lines`], the same lines the
    /// terminal prints: the canonical origin whose key will sign, the purpose,
    /// what the site says it wants, every byte of the payload, and its digest.
    /// Nothing is summarised, because a signature over bytes the user did not
    /// see is not consent, and nothing is laid out differently from the
    /// terminal, because two front ends that disagree about a request are two
    /// chances to show it wrong.
    fn modal(&mut self, context: &egui::Context) {
        let Some(modal) = self.modal.take() else {
            return;
        };
        let mut still_open = None;

        match modal {
            Modal::Sign { request, answer } => {
                let mut decision = None;
                // Wide enough for a wrapped payload line when the window is;
                // anything narrower scrolls sideways rather than rewrapping.
                let width = (context.content_rect().width() - 64.0).clamp(420.0, 800.0);
                egui::Modal::new(egui::Id::new("signing")).show(context, |ui| {
                    ui.set_width(width);
                    ui.heading("Signature requested");
                    ui.add_space(4.0);
                    ui.label(
                        RichText::new(
                            "A site is asking for your origin key to sign the bytes below.",
                        )
                        .size(12.0)
                        .color(Color32::GRAY),
                    );
                    ui.separator();

                    egui::Frame::group(ui.style()).show(ui, |ui| {
                        egui::ScrollArea::both()
                            .max_height(420.0)
                            .auto_shrink([false, true])
                            .show(ui, |ui| {
                                for line in prompt_lines(&request) {
                                    ui.add(
                                        egui::Label::new(
                                            RichText::new(line).monospace().size(12.0),
                                        )
                                        .extend(),
                                    );
                                }
                            });
                    });

                    ui.add_space(10.0);
                    ui.horizontal(|ui| {
                        if ui.button("Decline").clicked() {
                            decision = Some(Decision::No);
                        }
                        if ui
                            .add(egui::Button::new(
                                RichText::new("Sign these bytes").strong(),
                            ))
                            .clicked()
                        {
                            decision = Some(Decision::Yes);
                        }
                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                            ui.label(
                                RichText::new("The bytes above are exactly what gets signed.")
                                    .size(11.0)
                                    .italics()
                                    .color(Color32::DARK_GRAY),
                            );
                        });
                    });
                });

                match decision {
                    Some(decision) => {
                        let _ = answer.send(decision);
                    }
                    None => still_open = Some(Modal::Sign { request, answer }),
                }
            }

            Modal::Confirm {
                title,
                detail,
                answer,
            } => {
                let mut decision = None;
                egui::Modal::new(egui::Id::new("confirm")).show(context, |ui| {
                    ui.set_width(420.0);
                    ui.heading(&title);
                    if !detail.is_empty() {
                        ui.add_space(4.0);
                        ui.label(&detail);
                    }
                    ui.add_space(10.0);
                    ui.horizontal(|ui| {
                        if ui.button("No").clicked() {
                            decision = Some(Decision::No);
                        }
                        if ui.button("Yes").clicked() {
                            decision = Some(Decision::Yes);
                        }
                    });
                });
                match decision {
                    Some(decision) => {
                        let _ = answer.send(decision);
                    }
                    None => {
                        still_open = Some(Modal::Confirm {
                            title,
                            detail,
                            answer,
                        })
                    }
                }
            }

            Modal::Passphrase {
                label,
                mut typed,
                answer,
            } => {
                let mut settled = None;
                egui::Modal::new(egui::Id::new("passphrase")).show(context, |ui| {
                    ui.set_width(420.0);
                    ui.heading("Unseal the keystore");
                    ui.add_space(4.0);
                    ui.label(RichText::new(&label).size(12.0).color(Color32::GRAY));
                    ui.add_space(8.0);
                    let field = ui.add(
                        egui::TextEdit::singleline(&mut typed)
                            .password(true)
                            .desired_width(f32::INFINITY),
                    );
                    field.request_focus();
                    let entered =
                        field.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
                    ui.add_space(10.0);
                    ui.horizontal(|ui| {
                        if ui.button("Cancel").clicked() {
                            settled = Some(None);
                        }
                        if ui.button("Unseal").clicked() || entered {
                            settled = Some(Some(typed.clone()));
                        }
                    });
                });
                match settled {
                    Some(given) => {
                        let _ = answer.send(given);
                    }
                    None => {
                        still_open = Some(Modal::Passphrase {
                            label,
                            typed,
                            answer,
                        })
                    }
                }
            }
        }

        self.modal = still_open;
    }
}

impl eframe::App for App {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        self.drain();
        let context = ui.ctx().clone();

        // The window is named after what is in it. accesskit publishes the same
        // title, so a screen reader announces the page rather than the program.
        if self.title != self.titled {
            let title = match &self.title {
                Some(title) => format!("{title} — Syndeo"),
                None => "Syndeo".to_string(),
            };
            context.send_viewport_cmd(egui::ViewportCommand::Title(title));
            self.titled = self.title.clone();
        }
        if self.loading || self.modal.is_some() {
            context.request_repaint_after(std::time::Duration::from_millis(80));
        }

        egui::Panel::top("chrome").show(ui, |ui| {
            ui.add_space(4.0);
            self.chrome(ui);
            ui.add_space(4.0);
        });

        self.cache_panel(ui);
        self.peers_panel(ui);

        egui::CentralPanel::default().show(ui, |ui| self.body(ui));

        // Last, so it draws over everything else.
        self.modal(&context);
    }

    fn on_exit(&mut self) {
        // Anything still waiting on this window gets a refusal rather than a
        // thread that never wakes.
        self.closed.store(true, Ordering::SeqCst);
        let _ = self.work.send(Work::Quit);
    }
}

/// Give a widget an accessible name.
///
/// The reason this exists: an icon-only button announces itself as "←", and a
/// coloured badge announces itself as "cache", neither of which tells anyone
/// anything. egui takes a widget's visible text as its accessible name, so
/// where that text is a glyph or a bare word the name has to be set explicitly.
/// This is most of what accessibility costs when it is done from the start, and
/// all of what it costs to retrofit.
fn name(response: egui::Response, spoken: &str) -> egui::Response {
    response
        .ctx
        .accesskit_node_builder(response.id, |node| node.set_label(spoken.to_owned()));
    response
}

fn badge(ui: &mut egui::Ui, text: &str, colour: Color32) -> egui::Response {
    egui::Frame::new()
        .fill(colour.gamma_multiply(0.25))
        .inner_margin(egui::Margin::symmetric(6, 2))
        .corner_radius(4)
        .show(ui, |ui| {
            ui.label(RichText::new(text).size(11.0).color(colour).strong());
        })
        .response
}

fn source_colour(source: &str) -> Color32 {
    match source {
        "cache" | "revalidated" => Color32::from_rgb(90, 170, 110),
        "cache-stale" | "stale-on-error" => Color32::from_rgb(190, 160, 80),
        "peer" => Color32::from_rgb(120, 150, 210),
        _ => Color32::from_rgb(150, 150, 150),
    }
}

/// Build a page from a fetch, so the worker and any test agree on what one is.
pub fn page_from(url: String, fetched: syndeo_ipc::protocol::Fetched) -> Page {
    let document = Document::parse_bytes(&fetched.body, Some(&url));
    Page {
        url,
        status: fetched.status,
        source: fetched.source,
        protocol: fetched.protocol,
        elapsed_ms: fetched.elapsed_ms,
        bytes: fetched.body.len(),
        content: fetched.content,
        document,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use egui::accesskit::{NodeId, Role, TreeUpdate};
    use std::collections::HashMap;
    use syndeo_ipc::protocol::SignaturePurpose;

    /// Draw the real signing dialog for a request, with no window, and read
    /// back the text of every label it drew from the accessibility tree, in
    /// tree order.
    ///
    /// Read from what egui published rather than from what this module meant
    /// to draw: it is what a screen reader announces, and it is built from the
    /// widgets that were actually laid out.
    fn labels_drawn_for(request: SignatureRequest) -> Vec<String> {
        let context = egui::Context::default();
        context.enable_accesskit();

        let (work, _work) = std::sync::mpsc::channel();
        let (_done, done) = std::sync::mpsc::channel();
        let (asks, asked) = std::sync::mpsc::sync_channel(1);
        let (answer, _answer) = std::sync::mpsc::sync_channel(1);
        let closed = Arc::new(AtomicBool::new(false));
        let mut app = App::new(&context, work, done, asked, closed, None);
        asks.send(Ask::Sign {
            request: Box::new(request),
            answer,
        })
        .unwrap();

        // A few frames, so the modal's first sizing pass is behind it.
        let mut update = None;
        for _ in 0..3 {
            let input = egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(1100.0, 760.0),
                )),
                ..Default::default()
            };
            let mut output = context.run_ui(input, |ui| {
                app.drain();
                app.modal(ui.ctx());
            });
            update = output.platform_output.accesskit_update.take();
            // There is no renderer to upload the font atlas to.
            output.textures_delta.clear();
        }
        assert!(app.modal.is_some(), "the dialog closed without an answer");
        labels_in_order(&update.expect("no accessibility tree was published"))
    }

    fn labels_in_order(update: &TreeUpdate) -> Vec<String> {
        let nodes: HashMap<NodeId, &egui::accesskit::Node> =
            update.nodes.iter().map(|(id, node)| (*id, node)).collect();
        let mut labels = Vec::new();
        let mut stack = vec![update.tree.as_ref().expect("a tree").root];
        while let Some(id) = stack.pop() {
            let Some(node) = nodes.get(&id) else {
                continue;
            };
            if node.role() == Role::Label {
                labels.push(node.value().unwrap_or_default().to_string());
            }
            stack.extend(node.children().iter().rev().copied());
        }
        labels
    }

    fn shows_exactly(request: SignatureRequest) {
        let expected = prompt_lines(&request);
        let drawn = labels_drawn_for(request);
        assert!(
            drawn
                .windows(expected.len())
                .any(|window| window == expected.as_slice()),
            "the window drew\n{drawn:#?}\nand the shared prompt is\n{expected:#?}"
        );
    }

    #[test]
    fn the_window_draws_the_same_lines_as_the_terminal() {
        shows_exactly(
            SignatureRequest::new(
                "https://Wallet.Test:443/",
                SignaturePurpose::ChainTransaction,
                "Send 10 SUM to alice",
                b"transfer 10 SUM to alice\n\nmemo: rent\n".to_vec(),
            )
            .unwrap(),
        );
    }

    #[test]
    fn the_window_draws_a_hex_payload_the_same_way_too() {
        shows_exactly(
            SignatureRequest::new(
                "http://example.test:8080",
                SignaturePurpose::Attestation,
                "Attest",
                "pay alice\u{202E}01 MUS".as_bytes().to_vec(),
            )
            .unwrap(),
        );
    }

    #[test]
    fn the_window_draws_every_line_of_a_payload_at_the_limit() {
        // 64 lines, most of them below the fold: all of them are drawn, not
        // only the ones in view.
        let payload = format!("{}\n{}", "é".repeat(1500), "line\n".repeat(48));
        let request = SignatureRequest::new(
            "https://wallet.test",
            SignaturePurpose::Attestation,
            "A long one",
            payload.into_bytes(),
        )
        .unwrap();
        assert_eq!(
            request.rendered_payload().len(),
            syndeo_shell::prompt::MAX_TEXT_PAYLOAD_LINES
        );
        shows_exactly(request);
    }
}
