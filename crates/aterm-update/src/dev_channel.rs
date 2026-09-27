// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! A DEV BUILD'S STANDING AGAINST THE CHANNEL (gap #30, 2026-09-26).
//!
//! A dev-marked bundle (`ATermDevBuild`, stamped by `tools/dev-app.sh`) is left alone by
//! the updater on purpose ([`crate::running_is_dev_marked`]; `bundle::resolve` refuses
//! it), so no check thread ever runs in it and nothing ever said how far behind the
//! channel it had fallen. Measured on the owner's Mac: a 0.91 `aterm (dev).app`, weeks
//! old, launched from the Dock and ran old code while the release it sits beside kept
//! updating — with nothing on screen or in `aterm update status` saying so.
//!
//! This module is the one thing a dev build now asks the channel: ONE anonymous,
//! read-only `HEAD` of the PUBLIC channel's evergreen appcast URL — the very request the
//! updater's own check opens with ([`aterm_update_core::pointer`]), on the compiled-in
//! channel ([`crate::DEFAULT_OWNER`]/[`crate::DEFAULT_REPO`]) whatever `[update]
//! owner/repo` a development build was repointed at, so it never reads a staging repo.
//! Redirects are refused, so the one request is the whole conversation: the `302`'s
//! `Location` names the newest release's tag and nothing is fetched from it. No GET, no
//! download, no ledger or staging write, nothing applied — this module writes NO file.
//!
//! What it answers is a [`DevLag`]: behind, at, or ahead of that release, by VERSION.
//! The running version is compared for display only — it never reaches an update
//! decision, which stays the build number's (`build_info`'s contract). The count of
//! releases between them is a MINOR-version count: the channel publishes one app release
//! per minor (measured 2026-09-26 with `gh release list -R alabsystems/aterm`: v0.86.0
//! through v0.93.0, contiguous), and a count across a MAJOR bump is not claimed at all.
//!
//! An unreachable channel says nothing: a transport failure, a 404, a 429/5xx or a
//! refused redirect is `None`, never an error and never a row — offline is weather, and a
//! dev build's standing is not worth a failure the person must act on.
//!
//! WHEN it asks: at the window's start and then daily ([`RECHECK_EVERY`], on the wall
//! clock), only in a dev-marked copy and only while "Check for updates automatically"
//! (`[update] enabled`) is on — [`should_watch`]. `aterm update check` asks it once when
//! typed; `aterm update status` asks it only while automatic checks are on.

use std::time::Duration;

use aterm_update_core::pointer::{self, PointerError};
use aterm_update_core::tag::{TagKind, parse_release_tag};
use aterm_update_core::{HeadAnswer, HttpError};

/// The asset whose evergreen URL names the channel head — the updater's own
/// (`github::APPCAST_ASSET` is this constant).
pub(crate) const APPCAST_ASSET: &str = "aterm-appcast.toml";

/// How often a running dev build asks again: daily. A dev build falls behind by
/// releases, which the channel publishes about once a day (v0.86.0 on 2026-09-15 to
/// v0.93.0 on 2026-09-25); the updater's ten-minute cadence buys a dev build nothing.
pub const RECHECK_EVERY: Duration = Duration::from_secs(24 * 60 * 60);

/// Where a dev build stands against the newest release on the public channel.
/// `latest` is the channel head's tag (`v0.93.0`), already proved canonical by the
/// pointer.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DevLag {
    /// Older than the newest release. `releases` is how many minor versions behind it
    /// is, or `None` across a major bump (a count this module does not claim).
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
    /// Newer than the newest release — a dev build of work not yet released.
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
    pub fn is_behind(&self) -> bool {
        matches!(self, Self::Behind { .. })
    }

    /// The standing in words, naming the release as every update surface does
    /// (`aterm v0.93.0`): `2 releases behind aterm v0.93.0`, `1 release behind …`,
    /// `older than aterm v1.0.0` (across a major), `at aterm v0.93.0, the newest
    /// release`, `newer than aterm v0.93.0, the newest release`.
    #[must_use]
    pub fn words(&self) -> String {
        let release = format!("aterm {}", self.latest());
        match self {
            Self::Behind {
                releases: Some(1), ..
            } => format!("1 release behind {release}"),
            Self::Behind {
                releases: Some(n), ..
            } => format!("{n} releases behind {release}"),
            Self::Behind { releases: None, .. } => format!("older than {release}"),
            Self::Current { .. } => format!("at {release}, the newest release"),
            Self::Ahead { .. } => format!("newer than {release}, the newest release"),
        }
    }
}

/// `MAJOR.MINOR.PATCH` of a canonical three-component version, `v`-prefixed or not;
/// `None` for anything else (a two-component legacy tag, garbage).
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

/// Where `running_version` (`0.91.0`, the workspace version compiled in) stands against
/// `latest_tag` (`v0.93.0`), or `None` when either is not a canonical three-component
/// version — a standing that cannot be read is not guessed.
#[must_use]
pub fn classify(running_version: &str, latest_tag: &str) -> Option<DevLag> {
    let running = triple(running_version)?;
    let latest = triple(latest_tag)?;
    let tag = latest_tag.to_string();
    Some(match running.cmp(&latest) {
        std::cmp::Ordering::Less => DevLag::Behind {
            releases: (running[0] == latest[0]).then(|| latest[1] - running[1]),
            latest: tag,
        },
        std::cmp::Ordering::Equal => DevLag::Current { latest: tag },
        std::cmp::Ordering::Greater => DevLag::Ahead { latest: tag },
    })
}

/// The one URL this module ever requests: the public channel's evergreen appcast URL
/// (`https://github.com/alabsystems/aterm/releases/latest/download/aterm-appcast.toml`).
#[must_use]
pub fn channel_url() -> Option<String> {
    pointer::latest_download_url(crate::DEFAULT_OWNER, crate::DEFAULT_REPO, APPCAST_ASSET)
}

/// ONE `HEAD` of the public channel through `head` (the redirect-refusing transport),
/// and `running_version`'s standing against the release it names — or `None` when the
/// channel said nothing usable (unreachable, no release, weather, a redirect this client
/// refuses, a head that is not an app release). Never a second request.
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
            // Offline is weather, not news: the log's debug level, nothing else.
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

/// [`standing_with`] over the real transport: one `curl` HEAD, one try, a five-second
/// deadline ([`aterm_update_core::head_no_redirect_quick`]) — a hint, never worth a
/// retry ladder.
#[must_use]
pub fn standing(running_version: &str) -> Option<DevLag> {
    standing_with(
        running_version,
        &mut aterm_update_core::head_no_redirect_quick,
    )
}

/// What decides whether a process watches the channel for itself: a dev-marked copy
/// ([`crate::running_is_dev_marked`]) with automatic checks on ([`crate::automatic`],
/// `[update] enabled`). Both are read by the caller, so a test drives every arm.
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

/// Whether the watch runs at all. `[update] enabled = false` is the one switch that
/// turns off everything aterm checks by itself (`settings`), and a copy that is not
/// dev-marked has the updater's own check thread instead.
#[must_use]
pub fn should_watch(gate: WatchGate) -> bool {
    gate.automatic && gate.dev_marked
}

/// The watch, over injected seams: ask ([`standing_with`]), tell `notify` when the
/// standing CHANGED since the last one told (the first is always new), then `wait`
/// [`RECHECK_EVERY`] and ask again — until `wait` answers `false`. A gate that says no
/// ([`should_watch`]) asks nothing at all. A cycle whose channel said nothing tells
/// nothing and keeps the last standing (an offline morning is not news either way).
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

/// Sleep until `period` has passed on the WALL clock, in slices of at most an hour, so a
/// Mac that slept through the day asks within the hour it wakes (a monotonic sleep
/// would not count the night). Always answers `true`: the watch lives as long as the
/// process.
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

/// Start this process's watch on its own thread when [`should_watch`] says so (a
/// dev-marked copy with automatic checks on), and answer whether it started. `notify`
/// runs on that thread, once per CHANGE of standing; it must only hand the standing to
/// the caller's loop.
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

    /// The public channel's evergreen URL — the one string any fetch below may see.
    fn evergreen() -> String {
        channel_url().expect("the compiled channel is URL-safe")
    }

    /// A HEAD transport that answers the channel head `tag` with the 302 GitHub serves
    /// (measured shape: `pointer`'s tests), counting every request and the URL asked.
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

    /// Run the watch for `cycles` cycles against `head`, answering what it told and how
    /// many requests it made.
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

    /// BEHIND: a 0.91.0 dev build against a channel at v0.93.0 is told "2 releases
    /// behind aterm v0.93.0" — from ONE HEAD, the evergreen appcast URL of the compiled
    /// public channel, and nothing else.
    #[test]
    fn a_dev_build_behind_the_channel_is_told_how_far() {
        let (told, requests, waits) = run(DEV, "0.91.0", 1, &mut channel_at("v0.93.0"));
        assert_eq!(requests, 1, "one HEAD per cycle, never a GET");
        assert_eq!(waits, vec![RECHECK_EVERY], "then it waits a day");
        assert_eq!(
            told,
            vec![DevLag::Behind {
                latest: "v0.93.0".into(),
                releases: Some(2),
            }]
        );
        assert_eq!(told[0].words(), "2 releases behind aterm v0.93.0");
        assert!(told[0].is_behind());
    }

    /// CURRENT: the same version as the channel head.
    #[test]
    fn a_dev_build_of_the_newest_release_is_current() {
        let (told, requests, _) = run(DEV, "0.93.0", 1, &mut channel_at("v0.93.0"));
        assert_eq!(requests, 1);
        assert_eq!(
            told,
            vec![DevLag::Current {
                latest: "v0.93.0".into()
            }]
        );
        assert_eq!(told[0].words(), "at aterm v0.93.0, the newest release");
        assert!(!told[0].is_behind());
    }

    /// AHEAD: a dev build of work not yet released is newer than the channel head —
    /// never "behind", never a negative count.
    #[test]
    fn a_dev_build_newer_than_the_channel_is_ahead() {
        let (told, requests, _) = run(DEV, "0.94.0", 1, &mut channel_at("v0.93.0"));
        assert_eq!(requests, 1);
        assert_eq!(
            told,
            vec![DevLag::Ahead {
                latest: "v0.93.0".into()
            }]
        );
        assert_eq!(
            told[0].words(),
            "newer than aterm v0.93.0, the newest release"
        );
    }

    /// UNREACHABLE: a transport failure (offline), a 404, a 429/5xx, a refused
    /// redirect and a head that is not an app release each say NOTHING — no standing,
    /// no error — and the watch keeps its daily schedule.
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

    /// DISABLED: `[update] enabled = false` asks the channel NOTHING — zero requests —
    /// and so does a copy that is not dev-marked (it has the updater's own check).
    /// The control: the same watch with both on makes its request.
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

    /// The watch tells a standing ONCE, and again only when it CHANGES: three days at
    /// v0.93.0 are one notice; the channel moving to v0.94.0 is a second; an offline
    /// day in between neither repeats nor clears it.
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

    /// The count is a MINOR count within one major; across a major it is not claimed,
    /// and a version that is not canonical is no standing at all.
    #[test]
    fn the_count_is_minors_within_a_major() {
        assert_eq!(
            classify("0.92.0", "v0.93.0").map(|l| l.words()),
            Some("1 release behind aterm v0.93.0".to_string())
        );
        assert_eq!(
            classify("0.93.0", "v1.0.0"),
            Some(DevLag::Behind {
                latest: "v1.0.0".into(),
                releases: None,
            })
        );
        assert_eq!(
            classify("0.93.0", "v1.0.0").map(|l| l.words()),
            Some("older than aterm v1.0.0".to_string())
        );
        assert_eq!(
            classify("1.0.0", "v0.99.0").map(|l| l.is_behind()),
            Some(false)
        );
        for (running, tag) in [("0.93", "v0.93.0"), ("0.93.0", "v0.93"), ("x", "v0.93.0")] {
            assert_eq!(classify(running, tag), None, "{running} / {tag}");
        }
    }

    /// The URL asked is the COMPILED public channel's — the updater's evergreen appcast —
    /// and the asset is the updater's own.
    #[test]
    fn the_one_url_is_the_public_evergreen_appcast() {
        assert_eq!(
            evergreen(),
            format!(
                "https://github.com/{}/{}/releases/latest/download/aterm-appcast.toml",
                crate::DEFAULT_OWNER,
                crate::DEFAULT_REPO
            )
        );
        assert_eq!(RECHECK_EVERY, Duration::from_secs(86_400));
    }
}
