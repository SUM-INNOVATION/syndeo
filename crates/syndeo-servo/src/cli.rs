//! The command line of `syndeo-servo`, and what it says about itself.
//!
//! Outside the `renderer` gate, like [`crate::bridge`], so the words a user is
//! shown are tested on every commit rather than only when Servo is built.

use clap::Parser;
use std::path::PathBuf;

/// What `syndeo-servo -h` says in one line.
pub const ABOUT: &str = "Servo embedded, with every load going through the network process. \
     Experimental and for development only: unsafe for untrusted sites";

/// What `syndeo-servo --help` says before anything else.
///
/// The three reasons are the ones that make it unsafe to point at a site you
/// do not trust, each of them a way the embedding falls short of a browser:
/// every load is intercepted before Servo would apply its own cross-origin
/// checks, the interception has no request body to pass on, and the answer is
/// collected whole before Servo sees any of it.
pub const LONG_ABOUT: &str = "\
Servo embedded, with every load going through the network process.

EXPERIMENTAL, FOR DEVELOPMENT ONLY. UNSAFE FOR UNTRUSTED SITES:
  - it does not enforce cross-origin reads, so a page can read other origins'
    responses, including services on this machine and its network;
  - form POST bodies are sent empty;
  - responses are buffered completely, with no size cap.

It is in no release. To browse, use syndeo-webkit (macOS) or syndeo-ui.";

/// The line written to stderr every time it starts.
pub const STARTUP_WARNING: &str = "syndeo-servo is experimental and for development only, \
     and unsafe for untrusted sites: it does not enforce cross-origin reads, it sends \
     form POST bodies empty, and it buffers whole responses with no size cap.";

/// Say it, before anything else happens.
pub fn announce(to: &mut dyn std::io::Write) {
    let _ = writeln!(to, "{STARTUP_WARNING}");
}

#[derive(Parser)]
#[command(name = "syndeo-servo", version, about = ABOUT, long_about = LONG_ABOUT)]
pub struct Cli {
    /// The page to open.
    pub url: String,
    /// Where cache, keys and sockets live.
    #[arg(long)]
    pub home: Option<PathBuf>,
    /// system | dot:cloudflare | doh:cloudflare | doh:google | doh:quad9
    #[arg(long, default_value = "doh:cloudflare")]
    pub dns: String,
    /// Scroll this many times on its own, report how long each frame took, and
    /// exit.
    ///
    /// Driving a window from a test script means synthetic input, which means
    /// depending on which window the operating system thinks is focused — and
    /// that is decided by whoever is using the machine at the time. This drives
    /// the same code path a wheel event does, from inside, so the measurement
    /// is the renderer's rather than the window server's.
    #[arg(long, value_name = "COUNT")]
    pub scroll_bench: Option<usize>,
    /// Window size, as WIDTHxHEIGHT. Compositing cost is per pixel, so a
    /// measurement at the default size says nothing about a maximised window.
    #[arg(long, value_name = "WxH")]
    pub window_size: Option<String>,
    /// Milliseconds between synthetic scrolls in `--scroll-bench`. A trackpad
    /// is about 8; anything under one frame is what coalescing exists for.
    #[arg(long, default_value_t = 8, value_name = "MS")]
    pub scroll_rate_ms: u64,
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::CommandFactory;

    /// Each reason, as the words a reader would look for.
    const REASONS: [&str; 3] = [
        "does not enforce cross-origin reads",
        "POST bodies",
        "no size cap",
    ];

    #[test]
    fn help_leads_with_the_warning_and_all_three_reasons() {
        let help = Cli::command().render_long_help().to_string();
        let warning = help
            .find("UNSAFE FOR UNTRUSTED SITES")
            .expect("--help says it is unsafe for untrusted sites");
        assert!(help.contains("FOR DEVELOPMENT ONLY"), "{help}");
        assert!(
            warning < help.find("Usage:").expect("usage"),
            "the warning comes before the usage"
        );
        for reason in REASONS {
            assert!(
                help.contains(reason),
                "--help does not say: {reason}\n{help}"
            );
        }
    }

    #[test]
    fn short_help_says_it_is_unsafe_too() {
        let help = Cli::command().render_help().to_string();
        assert!(help.contains("unsafe for untrusted sites"), "{help}");
        assert!(help.contains("development only"), "{help}");
    }

    #[test]
    fn starting_it_writes_one_warning_line_with_all_three_reasons() {
        let mut stderr = Vec::new();
        announce(&mut stderr);
        let written = String::from_utf8(stderr).unwrap();
        assert_eq!(written.lines().count(), 1, "{written}");
        assert!(written.contains("unsafe for untrusted sites"), "{written}");
        assert!(written.contains("development only"), "{written}");
        for reason in REASONS {
            assert!(
                written.contains(reason),
                "the startup line does not say: {reason}"
            );
        }
    }

    #[test]
    fn the_command_line_still_parses() {
        let cli = Cli::try_parse_from(["syndeo-servo", "https://example.test/"]).unwrap();
        assert_eq!(cli.url, "https://example.test/");
        assert_eq!(cli.dns, "doh:cloudflare");
        assert_eq!(cli.scroll_rate_ms, 8);
    }
}
