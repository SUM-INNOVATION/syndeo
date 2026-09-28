//! Text from a page, made safe to print to a terminal.
//!
//! A page chooses its title, its text, its link targets and every attribute.
//! Printed as they are, those can carry escape sequences: clear the screen,
//! move the cursor over what was printed before, set the clipboard (OSC 52),
//! or make a link that says one thing and goes to another. Bidirectional
//! controls and other invisible format characters reorder or hide what is
//! read. Everything a page supplies goes through one of these two before it
//! reaches a terminal.
//!
//! These are for display. They are not the check that refuses a signing
//! request containing such characters, which stays in the shell and refuses
//! rather than cleans.

use std::borrow::Cow;
use unicode_general_category::{get_general_category, GeneralCategory};

/// A character nothing a page supplies may put on a terminal: C0 and C1
/// controls, DEL, and every Unicode format character (the bidirectional
/// overrides and isolates among them).
fn unprintable(c: char) -> bool {
    matches!(
        get_general_category(c),
        GeneralCategory::Control | GeneralCategory::Format
    )
}

/// For prose that is meant to span lines — page text, a text block, a tool's
/// output. Keeps newlines and tabs, turns the Unicode line and paragraph
/// separators into newlines, and removes every other control and format
/// character.
pub fn multiline(text: &str) -> Cow<'_, str> {
    if !text
        .chars()
        .any(|c| (unprintable(c) && c != '\n' && c != '\t') || c == '\u{2028}' || c == '\u{2029}')
    {
        return Cow::Borrowed(text);
    }
    Cow::Owned(
        text.chars()
            .filter_map(|c| match c {
                '\n' | '\t' => Some(c),
                '\u{2028}' | '\u{2029}' => Some('\n'),
                c if unprintable(c) => None,
                c => Some(c),
            })
            .collect(),
    )
}

/// For anything that must stay on one line — a title, a URL, a link's text,
/// an attribute, a table cell. Line breaks, tabs, carriage returns and the
/// Unicode separators become spaces, every other control and format character
/// goes, and runs of whitespace collapse to one space. Apply before
/// truncating or padding, so widths are counted on what is shown.
pub fn single_line(text: &str) -> Cow<'_, str> {
    let clean = !text
        .chars()
        .any(|c| unprintable(c) || c == '\u{2028}' || c == '\u{2029}')
        && !text.contains("  ");
    if clean {
        return Cow::Borrowed(text);
    }
    let mut out = String::with_capacity(text.len());
    let mut space = false;
    for c in text.chars() {
        let c = match c {
            '\n' | '\t' | '\r' | '\u{2028}' | '\u{2029}' => ' ',
            c if unprintable(c) => continue,
            c => c,
        };
        if c == ' ' {
            if space {
                continue;
            }
            space = true;
        } else {
            space = false;
        }
        out.push(c);
    }
    Cow::Owned(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    const HOSTILE: &str =
        "a\u{1b}]52;c;cGF3bmVk\u{7}b\u{1b}[2Jc\u{9b}31md\u{7f}e\u{202e}f\u{2066}g\u{200b}h\u{0}i";

    fn has_unprintable(s: &str, allow_newline: bool) -> bool {
        s.chars().any(|c| {
            (unprintable(c) && !(allow_newline && (c == '\n' || c == '\t')))
                || c == '\u{2028}'
                || c == '\u{2029}'
        })
    }

    #[test]
    fn escape_sequences_and_format_characters_never_survive() {
        for filtered in [multiline(HOSTILE), single_line(HOSTILE)] {
            assert!(!has_unprintable(&filtered, true), "{filtered:?}");
            assert!(!filtered.contains('\u{1b}'));
        }
        // What is left is the visible text, in order.
        assert_eq!(multiline(HOSTILE), "a]52;c;cGF3bmVkb[2Jc31mdefghi");
    }

    #[test]
    fn multiline_keeps_its_lines_and_single_line_keeps_one() {
        let text = "line one\n\tindented\u{2028}line three\r\nfour";
        assert_eq!(multiline(text), "line one\n\tindented\nline three\nfour");
        assert_eq!(single_line(text), "line one indented line three four");
        assert!(!single_line(text).contains('\n'));
    }

    #[test]
    fn plain_text_is_passed_through_untouched() {
        for text in ["an ordinary title", "café — ünïcode", "日本語のページ"] {
            assert!(matches!(multiline(text), Cow::Borrowed(_)));
            assert!(matches!(single_line(text), Cow::Borrowed(_)));
        }
    }
}
