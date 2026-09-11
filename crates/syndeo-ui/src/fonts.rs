//! Script coverage for the reader pane.
//!
//! egui's bundled fonts are Ubuntu Light, Hack and Noto Emoji, which between
//! them cover Latin, Greek, Cyrillic and emoji and nothing else. A page in
//! Japanese, Chinese, Korean, Arabic, Hebrew, Thai or any Indic script renders
//! as a row of tofu boxes — not a missing feature, a wrong one: the text was
//! fetched, parsed and extracted correctly and then drawn as squares.
//!
//! Bundling a font that covers those scripts would add tens of megabytes to a
//! binary that is currently four, so the operating system's own is borrowed
//! instead. One file, the broadest available, appended at the lowest priority
//! so Latin text still renders in the bundled faces and only the characters
//! they lack fall through.
//!
//! What this cannot do is promise coverage. A machine with none of these
//! installed still draws tofu, and saying so in the log is better than leaving
//! someone to wonder whether the page or the browser is at fault.

use egui::epaint::text::{FontData, FontFamily, FontInsert, FontPriority, InsertFontFamily};
use egui::Context;

/// Ordered by breadth, not by preference. The first that exists wins, so the
/// single file covering the most scripts is tried before any that covers one.
#[cfg(target_os = "macos")]
const CANDIDATES: &[&str] = &[
    // One face, and close to everything: CJK, Arabic, Hebrew, Thai, Devanagari,
    // Cyrillic, Greek.
    "/System/Library/Fonts/Supplemental/Arial Unicode.ttf",
    // Then the ones that each cover a part of it, newest first. PingFang is
    // where macOS keeps Chinese, and carries kana with it.
    "/System/Library/Fonts/PingFang.ttc",
    "/System/Library/Fonts/Hiragino Sans GB.ttc",
    "/System/Library/Fonts/STHeiti Light.ttc",
    "/System/Library/Fonts/AppleSDGothicNeo.ttc",
];

#[cfg(target_os = "linux")]
const CANDIDATES: &[&str] = &[
    // Noto Sans CJK carries Chinese, Japanese and Korean in one file. The path
    // differs by distribution rather than by version, so all of them are tried.
    "/usr/share/fonts/opentype/noto/NotoSansCJK-Regular.ttc",
    "/usr/share/fonts/truetype/noto/NotoSansCJK-Regular.ttc",
    "/usr/share/fonts/noto-cjk/NotoSansCJK-Regular.ttc",
    "/usr/share/fonts/opentype/noto/NotoSansCJKsc-Regular.otf",
    "/usr/share/fonts/truetype/droid/DroidSansFallbackFull.ttf",
    "/usr/share/fonts/truetype/wqy/wqy-zenhei.ttc",
    "/usr/share/fonts/truetype/arphic/uming.ttc",
];

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
const CANDIDATES: &[&str] = &[];

/// Borrow one system font, if there is one to borrow.
pub fn install_fallback(context: &Context) {
    let Some((path, bytes)) = CANDIDATES.iter().find_map(|path| {
        std::fs::read(path)
            .ok()
            .filter(|bytes| !bytes.is_empty())
            .map(|bytes| (*path, bytes))
    }) else {
        tracing::warn!(
            "no system font covering CJK or the right-to-left scripts was found; \
             pages in those scripts will draw as empty boxes"
        );
        return;
    };

    tracing::debug!(font = path, bytes = bytes.len(), "system font fallback");

    // Lowest, in both families, and deliberately. Highest would hand every
    // character to this font, including the Latin the bundled faces were chosen
    // for, and the reader would silently change typeface on every page.
    context.add_font(FontInsert {
        name: "system-fallback".to_owned(),
        data: FontData::from_owned(bytes),
        families: vec![
            InsertFontFamily {
                family: FontFamily::Proportional,
                priority: FontPriority::Lowest,
            },
            InsertFontFamily {
                family: FontFamily::Monospace,
                priority: FontPriority::Lowest,
            },
        ],
    });
}
