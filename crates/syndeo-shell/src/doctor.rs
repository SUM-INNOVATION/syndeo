//! What `syndeo doctor` says about the boundaries, from facts rather than
//! from what the design intends.
//!
//! Each line is built from what this platform, this build and the running
//! keystore actually report. Where a fact is not known the line says so, and
//! makes no claim in its place.

use syndeo_ipc::protocol::{KeystoreRequest, KeystoreResponse, ScreenLockReport};

/// The platform, as far as these lines care.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Platform {
    MacOs,
    Other,
}

impl Platform {
    pub fn this() -> Self {
        if cfg!(target_os = "macos") {
            Platform::MacOs
        } else {
            Platform::Other
        }
    }
}

/// What the keystore said about when it forgets its seed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionFacts {
    /// No keystore could be started or reached.
    Unreachable,
    /// A keystore answered, but not this question — one from before 0.1.4,
    /// or one that failed to say.
    Unknown,
    Reported {
        initialized: bool,
        unsealed: bool,
        idle_timeout_secs: Option<u64>,
        sleep_detection: bool,
        screen_lock: ScreenLockReport,
    },
}

/// Ask a keystore on an open channel. Anything but a well-formed answer is
/// [`SessionFacts::Unknown`]: a keystore that does not know the request
/// closes the connection, and that is not an error for the doctor.
pub async fn session_facts<S>(channel: &mut syndeo_ipc::Framed<S>) -> SessionFacts
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
{
    if channel
        .send(&KeystoreRequest::SessionProtection)
        .await
        .is_err()
    {
        return SessionFacts::Unknown;
    }
    match channel.recv::<KeystoreResponse>().await {
        Ok(KeystoreResponse::SessionProtection {
            initialized,
            unsealed,
            idle_timeout_secs,
            sleep_detection,
            screen_lock,
        }) => SessionFacts::Reported {
            initialized,
            unsealed,
            idle_timeout_secs,
            sleep_detection,
            screen_lock,
        },
        _ => SessionFacts::Unknown,
    }
}

/// The "boundaries" section of `syndeo doctor`, line by line.
pub fn boundaries(platform: Platform, keystore: SessionFacts) -> Vec<String> {
    let mut lines = vec![
        "the shell's renderers (syndeo-ui, syndeo-servo) and the agent fetch only through \
         the net process"
            .to_string(),
    ];
    if platform == Platform::MacOs {
        lines.push(
            "syndeo-webkit is outside that: WebKit sends traffic to syndeo-proxy by its proxy \
             setting, which does not cover every transport (see the README)"
                .to_string(),
        );
    }
    lines.push("the agent has no keystore socket and no session secret".to_string());
    lines.push(
        "the keystore signs only what the shell confirmed, once, for one payload".to_string(),
    );
    lines.extend(retention(keystore));
    lines
}

/// When the seed stops being in memory, as far as the keystore says.
fn retention(keystore: SessionFacts) -> Vec<String> {
    match keystore {
        SessionFacts::Unreachable => {
            vec!["seed retention unknown: the keystore is not reachable".to_string()]
        }
        SessionFacts::Unknown => {
            vec!["seed retention unknown: the keystore did not say".to_string()]
        }
        SessionFacts::Reported {
            initialized: false, ..
        } => vec!["no seed exists: the keystore is not initialized".to_string()],
        SessionFacts::Reported {
            unsealed,
            idle_timeout_secs,
            sleep_detection,
            screen_lock,
            ..
        } => {
            let mut lines = vec![if unsealed {
                "a seed is in memory now".to_string()
            } else {
                "no seed is in memory now: the keystore is sealed".to_string()
            }];
            let mut forgotten = Vec::new();
            if sleep_detection {
                forgotten.push("on sleep".to_string());
            }
            if let Some(secs) = idle_timeout_secs {
                forgotten.push(format!("after {secs}s idle"));
            }
            if matches!(screen_lock, ScreenLockReport::Reported { .. }) {
                forgotten.push("on screen lock".to_string());
            }
            lines.push(if forgotten.is_empty() {
                "once unsealed, nothing forgets the seed until the keystore stops".to_string()
            } else {
                format!(
                    "once unsealed, the seed is forgotten {}",
                    forgotten.join(", ")
                )
            });
            if idle_timeout_secs.is_none() {
                lines.push("idle auto-lock is off".to_string());
            }
            if screen_lock == ScreenLockReport::NotReported {
                lines.push(
                    "screen lock is not reported for this session, so it does not forget the seed"
                        .to_string(),
                );
            }
            lines
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn reported(
        initialized: bool,
        unsealed: bool,
        idle: Option<u64>,
        screen_lock: ScreenLockReport,
    ) -> SessionFacts {
        SessionFacts::Reported {
            initialized,
            unsealed,
            idle_timeout_secs: idle,
            sleep_detection: true,
            screen_lock,
        }
    }

    fn joined(platform: Platform, facts: SessionFacts) -> String {
        boundaries(platform, facts).join("\n")
    }

    const LOCK: ScreenLockReport = ScreenLockReport::Reported { locked: false };
    const NO_LOCK: ScreenLockReport = ScreenLockReport::NotReported;

    #[test]
    fn nothing_is_claimed_about_retention_without_a_keystore_that_says() {
        for platform in [Platform::MacOs, Platform::Other] {
            for facts in [
                SessionFacts::Unreachable,
                SessionFacts::Unknown,
                reported(false, false, Some(300), LOCK),
            ] {
                let said = joined(platform, facts);
                assert!(!said.contains("forgotten"), "{facts:?}: {said}");
                assert!(!said.contains("screen lock"), "{facts:?}: {said}");
                assert!(!said.contains("in memory now"), "{facts:?}: {said}");
            }
        }
    }

    #[test]
    fn every_combination_says_only_what_was_reported() {
        for platform in [Platform::MacOs, Platform::Other] {
            for unsealed in [false, true] {
                for idle in [None, Some(300)] {
                    for lock in [LOCK, NO_LOCK] {
                        let said = joined(platform, reported(true, unsealed, idle, lock));
                        assert!(said.contains("forgotten on sleep"), "{said}");
                        assert_eq!(said.contains("after 300s idle"), idle.is_some(), "{said}");
                        assert_eq!(
                            said.contains("idle auto-lock is off"),
                            idle.is_none(),
                            "{said}"
                        );
                        assert_eq!(said.contains(", on screen lock"), lock == LOCK, "{said}");
                        assert_eq!(
                            said.contains("screen lock is not reported"),
                            lock == NO_LOCK,
                            "{said}"
                        );
                        assert_eq!(said.contains("a seed is in memory now"), unsealed, "{said}");
                    }
                }
            }
        }
    }

    #[test]
    fn a_platform_that_reports_no_screen_lock_never_claims_one() {
        // What Linux always reports, and macOS over ssh.
        let said = joined(Platform::Other, reported(true, true, Some(300), NO_LOCK));
        assert!(!said.contains("on screen lock"), "{said}");
    }

    #[test]
    fn webkit_is_named_as_outside_the_renderer_boundary_where_it_ships() {
        let mac = joined(Platform::MacOs, SessionFacts::Unknown);
        assert!(mac.contains("syndeo-webkit is outside that"), "{mac}");
        assert!(mac.contains("does not cover every transport"), "{mac}");
        let other = joined(Platform::Other, SessionFacts::Unknown);
        assert!(!other.contains("syndeo-webkit"), "{other}");
        for said in [mac, other] {
            assert!(
                !said.contains("renderers and the agent reach the network only"),
                "{said}"
            );
        }
    }

    #[tokio::test]
    async fn a_keystore_that_does_not_know_the_question_is_unknown_not_an_error() {
        // What a 0.1.3 keystore does with a request it cannot read: it drops
        // the connection.
        let (ours, theirs) = tokio::io::duplex(4096);
        let mut keystore = syndeo_ipc::Framed::new(theirs);
        let dropped = tokio::spawn(async move {
            let _ = keystore.recv::<serde_json::Value>().await;
            drop(keystore);
        });
        let mut channel = syndeo_ipc::Framed::new(ours);
        assert_eq!(session_facts(&mut channel).await, SessionFacts::Unknown);
        dropped.await.unwrap();
    }

    #[tokio::test]
    async fn a_keystore_that_answers_is_reported_as_it_answered() {
        let (ours, theirs) = tokio::io::duplex(4096);
        let mut keystore = syndeo_ipc::Framed::new(theirs);
        tokio::spawn(async move {
            let asked: KeystoreRequest = keystore.recv().await.unwrap();
            assert!(matches!(asked, KeystoreRequest::SessionProtection));
            keystore
                .send(&KeystoreResponse::SessionProtection {
                    initialized: true,
                    unsealed: false,
                    idle_timeout_secs: Some(60),
                    sleep_detection: true,
                    screen_lock: ScreenLockReport::NotReported,
                })
                .await
                .unwrap();
        });
        let mut channel = syndeo_ipc::Framed::new(ours);
        assert_eq!(
            session_facts(&mut channel).await,
            SessionFacts::Reported {
                initialized: true,
                unsealed: false,
                idle_timeout_secs: Some(60),
                sleep_detection: true,
                screen_lock: ScreenLockReport::NotReported,
            }
        );
    }
}
