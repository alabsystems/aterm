// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! A dev build's standing against the channel.

use std::time::Duration;

use aterm_update_core::pointer::{self, PointerError};
use aterm_update_core::tag::{TagKind, parse_release_tag};
use aterm_update_core::{HeadAnswer, HttpError};

/// The asset whose evergreen URL names the channel head (`github::APPCAST_ASSET`).
pub(crate) const APPCAST_ASSET: &str = "aterm-appcast.toml";

/// How often a running dev build asks again.
pub const RECHECK_EVERY: Duration = Duration::from_secs(24 * 60 * 60);

/// Where a dev build stands against the newest release on the public channel.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DevLag {
    /// Older than the newest release.
    Behind {
        /// The newest release's tag.
        latest: String,
        /// Minor versions behind, when the majors agree.
        releases: Option<u64>,
    },
    /// The same version as the newest release.
    Current {
        /// The newest release's tag.
        latest: String,
    },
    /// Newer than the newest release: unreleased work.
    Ahead {
        /// The newest release's tag.
        latest: String,
    },
}

impl DevLag {
    /// The newest release's tag, whatever the standing.
    #[must_use]
    pub fn latest(&self) -> &str {
        match self {
            Self::Behind { latest, .. } | Self::Current { latest } | Self::Ahead { latest } => {
                latest
            }
        }
    }

    /// Whether the dev build is older than the newest release.
    #[must_use]
    pub const fn is_behind(&self) -> bool {
        matches!(self, Self::Behind { .. })
    }

    /// The standing in words.
    #[must_use]
    pub fn words(&self) -> String {
        let release = format!("aterm {}", self.latest());
        match self {
            Self::Behind {
                releases: None | Some(0),
                ..
            } => format!("older than {release}"),
            Self::Behind {
                releases: Some(1), ..
            } => format!("1 release behind {release}"),
            Self::Behind {
                releases: Some(n), ..
            } => format!("{n} releases behind {release}"),
            Self::Current { .. } => format!("at {release}, the newest release"),
            Self::Ahead { .. } => format!("newer than {release}, the newest release"),
        }
    }
}

fn triple(version: &str) -> Option<[u64; 3]> {
    let tagged;
    let tag = if version.starts_with('v') {
        version
    } else {
        tagged = format!("v{version}");
        &tagged
    };
    match parse_release_tag(tag) {
        Ok(TagKind::Candidate(parts)) => <[u64; 3]>::try_from(parts).ok(),
        _ => None,
    }
}

/// Where `running_version` (`0.91.0`, compiled in) stands against `latest_tag` (`v0.93.0`).
#[must_use]
pub fn classify(running_version: &str, latest_tag: &str) -> Option<DevLag> {
    let running = triple(running_version)?;
    let latest = triple(latest_tag)?;
    let tag = latest_tag.to_string();
    Some(match running.cmp(&latest) {
        std::cmp::Ordering::Less => DevLag::Behind {
            releases: (running[0] == latest[0] && running[1] < latest[1])
                .then(|| latest[1] - running[1]),
            latest: tag,
        },
        std::cmp::Ordering::Equal => DevLag::Current { latest: tag },
        std::cmp::Ordering::Greater => DevLag::Ahead { latest: tag },
    })
}

/// One `HEAD` of the public channel through `head` (redirect-refusing).
pub fn standing_with(
    running_version: &str,
    head: &mut dyn FnMut(&str) -> Result<HeadAnswer, HttpError>,
) -> Option<DevLag> {
    let answer = pointer::resolve_with(
        crate::DEFAULT_OWNER,
        crate::DEFAULT_REPO,
        APPCAST_ASSET,
        &pointer::canonical_app_tag,
        head,
    );
    match answer {
        Ok(found) => classify(running_version, &found.tag),
        Err(error) => {
            let why = match &error {
                PointerError::Transport(_) => "unreachable",
                PointerError::NoRelease { .. } => "no published release",
                PointerError::Transient { .. } => "the host asked to wait",
                _ => "no app release to compare with",
            };
            aterm_log::debug!("aterm-update: dev build: channel not read ({why}): {error}");
            None
        }
    }
}

/// [`standing_with`] over the real transport.
#[must_use]
pub fn standing(running_version: &str) -> Option<DevLag> {
    standing_with(
        running_version,
        &mut aterm_update_core::head_no_redirect_quick,
    )
}

/// Whether a process watches the channel: a dev-marked copy with automatic checks on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct WatchGate {
    /// `[update] enabled` on a platform the updater runs on.
    pub automatic: bool,
    /// The running copy carries the dev mark.
    pub dev_marked: bool,
}

impl WatchGate {
    /// This process's gate, read now.
    #[must_use]
    pub fn of_this_process() -> Self {
        Self {
            automatic: crate::automatic(),
            dev_marked: crate::running_is_dev_marked(),
        }
    }
}

/// Whether the watch runs at all.
#[must_use]
pub const fn should_watch(gate: WatchGate) -> bool {
    gate.automatic && gate.dev_marked
}

/// The watch.
pub fn watch_with(
    gate: WatchGate,
    running_version: &str,
    head: &mut dyn FnMut(&str) -> Result<HeadAnswer, HttpError>,
    notify: &mut dyn FnMut(DevLag),
    wait: &mut dyn FnMut(Duration) -> bool,
) {
    if !should_watch(gate) {
        return;
    }
    let mut told: Option<DevLag> = None;
    loop {
        if let Some(lag) = standing_with(running_version, head)
            && told.as_ref() != Some(&lag)
        {
            aterm_log::info!("aterm-update: dev build {running_version}: {}", lag.words());
            told = Some(lag.clone());
            notify(lag);
        }
        if !wait(RECHECK_EVERY) {
            return;
        }
    }
}

fn wait_on_wall_clock(period: Duration) -> bool {
    const SLICE: Duration = Duration::from_secs(60 * 60);
    let due = std::time::SystemTime::now() + period;
    while let Ok(left) = due.duration_since(std::time::SystemTime::now()) {
        if left.is_zero() {
            break;
        }
        std::thread::sleep(left.min(SLICE));
    }
    true
}

/// Start this process's watch on its own thread when [`should_watch`] says so.
pub fn spawn_watch(running_version: &'static str, notify: Box<dyn Fn(DevLag) + Send>) -> bool {
    let gate = WatchGate::of_this_process();
    if !should_watch(gate) {
        return false;
    }
    std::thread::Builder::new()
        .name("aterm-dev-channel".into())
        .spawn(move || {
            watch_with(
                gate,
                running_version,
                &mut aterm_update_core::head_no_redirect_quick,
                &mut |lag| notify(lag),
                &mut wait_on_wall_clock,
            );
        })
        .is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn evergreen() -> String {
        pointer::latest_download_url(crate::DEFAULT_OWNER, crate::DEFAULT_REPO, APPCAST_ASSET)
            .expect("the compiled channel is URL-safe")
    }

    fn channel_at(tag: &'static str) -> impl FnMut(&str) -> Result<HeadAnswer, HttpError> {
        move |url: &str| {
            assert_eq!(
                url,
                evergreen(),
                "only the public evergreen appcast is asked"
            );
            Ok(HeadAnswer {
                code: 302,
                location: Some(format!(
                    "https://github.com/{}/{}/releases/download/{tag}/{APPCAST_ASSET}",
                    crate::DEFAULT_OWNER,
                    crate::DEFAULT_REPO
                )),
            })
        }
    }

    const DEV: WatchGate = WatchGate {
        automatic: true,
        dev_marked: true,
    };

    fn run(
        gate: WatchGate,
        running: &str,
        cycles: usize,
        head: &mut dyn FnMut(&str) -> Result<HeadAnswer, HttpError>,
    ) -> (Vec<DevLag>, usize, Vec<Duration>) {
        let mut requests = 0usize;
        let mut told = Vec::new();
        let mut waits = Vec::new();
        let mut counted = |url: &str| {
            requests += 1;
            head(url)
        };
        watch_with(
            gate,
            running,
            &mut counted,
            &mut |lag| told.push(lag),
            &mut |period| {
                waits.push(period);
                waits.len() < cycles
            },
        );
        (told, requests, waits)
    }

    #[test]
    fn a_dev_build_is_told_its_standing_from_one_head() {
        let latest = || "v0.93.0".to_string();
        for (running, want) in [
            (
                "0.91.0",
                DevLag::Behind {
                    latest: latest(),
                    releases: Some(2),
                },
            ),
            ("0.93.0", DevLag::Current { latest: latest() }),
            ("0.94.0", DevLag::Ahead { latest: latest() }),
        ] {
            let (told, requests, waits) = run(DEV, running, 1, &mut channel_at("v0.93.0"));
            assert_eq!(requests, 1, "{running}: one HEAD per cycle, never a GET");
            assert_eq!(waits, vec![RECHECK_EVERY], "{running}: then it waits a day");
            assert_eq!(told[0].is_behind(), running == "0.91.0", "{running}");
            assert_eq!(told, vec![want], "{running}");
        }
    }

    #[test]
    fn an_unreachable_channel_says_nothing() {
        let answers: [Box<dyn Fn() -> Result<HeadAnswer, HttpError>>; 5] = [
            Box::new(|| {
                Err(HttpError::Transport(
                    "curl: (6) Could not resolve host".into(),
                ))
            }),
            Box::new(|| {
                Ok(HeadAnswer {
                    code: 404,
                    location: None,
                })
            }),
            Box::new(|| {
                Ok(HeadAnswer {
                    code: 503,
                    location: None,
                })
            }),
            Box::new(|| {
                Ok(HeadAnswer {
                    code: 302,
                    location: Some("https://evil.example/x/aterm-appcast.toml".into()),
                })
            }),
            Box::new(|| {
                Ok(HeadAnswer {
                    code: 302,
                    location: Some(format!(
                        "https://github.com/{}/{}/releases/download/atpkg-index-50/{APPCAST_ASSET}",
                        crate::DEFAULT_OWNER,
                        crate::DEFAULT_REPO
                    )),
                })
            }),
        ];
        for (i, answer) in answers.iter().enumerate() {
            let (told, requests, waits) = run(DEV, "0.91.0", 3, &mut |_| answer());
            assert!(told.is_empty(), "answer {i}: nothing is told");
            assert_eq!(requests, 3, "answer {i}: one HEAD a day, no retry storm");
            assert_eq!(waits.len(), 3, "answer {i}: the schedule holds");
        }
    }

    #[test]
    fn automatic_checks_off_or_a_release_copy_asks_nothing() {
        for gate in [
            WatchGate {
                automatic: false,
                dev_marked: true,
            },
            WatchGate {
                automatic: true,
                dev_marked: false,
            },
            WatchGate {
                automatic: false,
                dev_marked: false,
            },
        ] {
            let (told, requests, waits) = run(gate, "0.91.0", 3, &mut channel_at("v0.93.0"));
            assert_eq!(requests, 0, "{gate:?}: no request at all");
            assert!(told.is_empty() && waits.is_empty(), "{gate:?}");
            assert!(!should_watch(gate));
        }
        let (_, requests, _) = run(DEV, "0.91.0", 1, &mut channel_at("v0.93.0"));
        assert_eq!(requests, 1, "the control: a dev build with checks on asks");
    }

    #[test]
    fn the_watch_tells_a_change_once() {
        let heads = ["v0.93.0", "v0.93.0", "offline", "v0.93.0", "v0.94.0"];
        let mut day = 0usize;
        let mut head = |_: &str| {
            let tag = heads[day.min(heads.len() - 1)];
            day += 1;
            if tag == "offline" {
                return Err(HttpError::Transport("offline".into()));
            }
            Ok(HeadAnswer {
                code: 302,
                location: Some(format!(
                    "https://github.com/{}/{}/releases/download/{tag}/{APPCAST_ASSET}",
                    crate::DEFAULT_OWNER,
                    crate::DEFAULT_REPO
                )),
            })
        };
        let (told, requests, _) = run(DEV, "0.91.0", heads.len(), &mut head);
        assert_eq!(requests, heads.len());
        assert_eq!(
            told,
            vec![
                DevLag::Behind {
                    latest: "v0.93.0".into(),
                    releases: Some(2),
                },
                DevLag::Behind {
                    latest: "v0.94.0".into(),
                    releases: Some(3),
                },
            ]
        );
    }

    #[test]
    fn the_count_is_minors_within_a_major() {
        assert_eq!(
            classify("0.92.0", "v0.93.0"),
            Some(DevLag::Behind {
                latest: "v0.93.0".into(),
                releases: Some(1),
            })
        );
        let patch_lag = classify("0.93.0", "v0.93.1").expect("patch lag");
        assert_eq!(
            patch_lag,
            DevLag::Behind {
                latest: "v0.93.1".into(),
                releases: None,
            }
        );
        assert_eq!(patch_lag.words(), "older than aterm v0.93.1");
        assert_eq!(
            DevLag::Behind {
                latest: "v0.93.1".into(),
                releases: Some(0),
            }
            .words(),
            "older than aterm v0.93.1"
        );
        assert_eq!(
            classify("0.93.0", "v1.0.0"),
            Some(DevLag::Behind {
                latest: "v1.0.0".into(),
                releases: None,
            })
        );
        assert_eq!(
            classify("1.0.0", "v0.99.0").map(|l| l.is_behind()),
            Some(false)
        );
        for (running, tag) in [("0.93", "v0.93.0"), ("0.93.0", "v0.93"), ("x", "v0.93.0")] {
            assert_eq!(classify(running, tag), None, "{running} / {tag}");
        }
    }
}
