//! Drawing what the headless DOM extracted.
//!
//! This is a reader, not a renderer. Step four of the build order embeds Servo
//! and puts real layout behind this pane; until then `syndeo-dom` gives prose,
//! headings, links, forms and subresources, and that is what is on screen. The
//! window is honest about which of the two it is showing.

use egui::{Color32, RichText, Ui};
use syndeo_dom::Document;

/// One loaded page and everything the shell knows about how it arrived.
pub struct Page {
    pub url: String,
    pub status: u16,
    pub source: String,
    pub protocol: String,
    pub elapsed_ms: u64,
    pub bytes: usize,
    pub content: Option<String>,
    pub document: Document,
}

impl Page {
    pub fn title(&self) -> String {
        self.document.title().unwrap_or_else(|| self.url.clone())
    }
}

/// The prose, as headings and paragraphs.
pub fn reader(ui: &mut Ui, page: &Page) {
    let blocks = page.document.blocks();
    if blocks.is_empty() {
        ui.label(
            RichText::new("This page has no extractable text.")
                .italics()
                .color(Color32::GRAY),
        );
        return;
    }

    for block in blocks {
        match block.heading_level {
            Some(1) => {
                ui.add_space(10.0);
                ui.heading(&block.text);
            }
            Some(2) => {
                ui.add_space(8.0);
                ui.label(RichText::new(&block.text).size(19.0).strong());
            }
            Some(_) => {
                ui.add_space(6.0);
                ui.label(RichText::new(&block.text).size(16.0).strong());
            }
            None => {
                ui.add_space(4.0);
                ui.label(RichText::new(&block.text).size(14.0));
            }
        }
    }
}

/// Links, with the one that was clicked reported back.
pub fn links(ui: &mut Ui, page: &Page) -> Option<String> {
    let links = page.document.links();
    if links.is_empty() {
        ui.label(RichText::new("No links.").italics().color(Color32::GRAY));
        return None;
    }

    let mut clicked = None;
    ui.label(
        RichText::new(format!("{} links", links.len()))
            .color(Color32::GRAY)
            .size(12.0),
    );
    ui.add_space(6.0);
    for link in links {
        let label = if link.text.trim().is_empty() {
            link.url.clone()
        } else {
            link.text.clone()
        };
        let response = ui.link(RichText::new(truncate(&label, 70)).size(13.0));
        if response.on_hover_text(&link.url).clicked() {
            clicked = Some(link.url.clone());
        }
    }
    clicked
}

/// Subresources, and — the thing worth seeing — whether each declared a hash.
///
/// Declaring one is exactly what makes a resource eligible to come from a peer,
/// so this column is not decoration: it is the difference between a resource
/// that only the origin can serve and one that anybody can.
pub fn subresources(ui: &mut Ui, page: &Page) {
    let resources = page.document.subresources();
    if resources.is_empty() {
        ui.label(
            RichText::new("No subresources.")
                .italics()
                .color(Color32::GRAY),
        );
        return;
    }

    let with_integrity = resources.iter().filter(|r| r.integrity.is_some()).count();
    ui.label(
        RichText::new(format!(
            "{} subresources, {with_integrity} with a declared hash and therefore eligible for peer fetch",
            resources.len()
        ))
        .color(Color32::GRAY)
        .size(12.0),
    );
    ui.add_space(6.0);

    egui::Grid::new("subresources")
        .num_columns(3)
        .striped(true)
        .spacing([12.0, 4.0])
        .show(ui, |ui| {
            for resource in resources {
                ui.label(RichText::new(&resource.kind).size(12.0).monospace());
                ui.label(RichText::new(truncate(&resource.url, 60)).size(12.0))
                    .on_hover_text(&resource.url);
                match &resource.integrity {
                    Some(hash) => {
                        ui.label(
                            RichText::new("integrity declared")
                                .size(12.0)
                                .color(Color32::from_rgb(90, 160, 110)),
                        )
                        .on_hover_text(hash);
                    }
                    None => {
                        ui.label(
                            RichText::new("origin only")
                                .size(12.0)
                                .color(Color32::from_rgb(150, 130, 90)),
                        )
                        .on_hover_text(
                            "Without a declared hash there is nothing to check a peer's bytes \
                             against, so this can only come from the origin.",
                        );
                    }
                }
                ui.end_row();
            }
        });
}

/// Forms, as they were found. Nothing here submits one yet.
pub fn forms(ui: &mut Ui, page: &Page) {
    let forms = page.document.forms();
    if forms.is_empty() {
        ui.label(RichText::new("No forms.").italics().color(Color32::GRAY));
        return;
    }
    for form in forms {
        ui.add_space(6.0);
        ui.label(
            RichText::new(format!("{} {}", form.method, form.action))
                .monospace()
                .size(13.0),
        );
        for field in &form.fields {
            ui.label(
                RichText::new(format!(
                    "    {:<24} {}{}",
                    field.name,
                    field.kind,
                    if field.required { "  required" } else { "" }
                ))
                .monospace()
                .size(12.0)
                .color(Color32::GRAY),
            );
        }
    }
}

pub fn truncate(text: &str, width: usize) -> String {
    let text = text.replace('\n', " ");
    if text.chars().count() <= width {
        text
    } else {
        format!(
            "{}…",
            text.chars()
                .take(width.saturating_sub(1))
                .collect::<String>()
        )
    }
}
