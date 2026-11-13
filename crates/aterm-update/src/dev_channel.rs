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
    if let Some(due) = std::time::SystemTime::now().checked_add(period) {
        while let Ok(left) = due.duration_since(std::time::SystemTime::now()) {
            if left.is_zero() {
                break;
            }
            std::thread::sleep(left.min(SLICE));
        }
    } else {
        let mut remaining = period;
        while !remaining.is_zero() {
            let step = remaining.min(SLICE);
            std::thread::sleep(step);
            remaining = remaining.saturating_sub(step);
        }
    }
    true
}

/// Start this process's watch on its own thread when [`should_watch`] says so, once this process's
/// `[update] enabled` has an answer ([`crate::once_automatic_known`]). False where it already says
/// the watch does not run.
pub fn spawn_watch(running_version: &'static str, notify: Box<dyn Fn(DevLag) + Send>) -> bool {
    if !crate::running_is_dev_marked() || crate::automatic_at_launch() == Some(false) {
        return false;
    }
    crate::once_automatic_known("dev channel watches", move || {
        let gate = WatchGate::of_this_process();
        if !should_watch(gate) {
            return;
        }
        let started = std::thread::Builder::new()
            .name("aterm-dev-channel".into())
            .spawn(move || {
                watch_with(
                    gate,
                    running_version,
                    &mut aterm_update_core::head_no_redirect_quick,
                    &mut |lag| notify(lag),
                    &mut wait_on_wall_clock,
                );
            });
        if let Err(error) = started {
            aterm_log::warn!("aterm-update: could not start the dev channel watch: {error}");
        }
    });
    true
}

#[cfg(test)]
#[path = "dev_channel_tests.rs"]
mod tests;
