// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! App state behind the "update ready" nudge: the ⬆️-suffixed version menu title and
//! its "Install aterm `v<staged>` now" item (`crate::menu::update_version_menu`, the
//! palette's Version row), plus the leading `↻` in the off-macOS tab strip
//! ([`crate::tab_bar`] `TabHit::Update`). Holds the staged build/version (the repaint
//! fingerprint) and the post-update realized-arrow timing (TTL, fade law, bucket).

use std::time::Duration;

/// How long the post-update "realized" ⬆️ arrow decorates the version menu and the
/// palette's Version section before it has faded out.
pub(crate) const REALIZED_ARROW_TTL: Duration = Duration::from_secs(10 * 60);

/// Fade re-present quantum: the palette fingerprint folds the elapsed bucket and the
/// `about_to_wait` sweep arms one wake per bucket edge, so an open palette fades one
/// present per step rather than per frame.
pub(crate) const REALIZED_BUCKET: Duration = Duration::from_secs(30);

/// Full-strength hold before the realized arrow starts fading (mirrors the notice
/// pill's hold-then-fade shape: the fresh-upgrade moment reads at full alpha first).
const REALIZED_HOLD: Duration = Duration::from_secs(120);

/// Realized-arrow alpha at `elapsed` since the post-update boot: `1.0` through the
/// hold, then linear to `0.0` at [`REALIZED_ARROW_TTL`]. Under `MotionPolicy::Reduced`
/// the caller holds full alpha instead.
pub(crate) fn realized_alpha(elapsed: Duration) -> f32 {
    if elapsed <= REALIZED_HOLD {
        return 1.0;
    }
    let fade = (REALIZED_ARROW_TTL - REALIZED_HOLD).as_secs_f32();
    ((REALIZED_ARROW_TTL.as_secs_f32() - elapsed.as_secs_f32()) / fade).clamp(0.0, 1.0)
}

/// Fade bucket index at `elapsed`, folded into the palette fingerprint so each bucket
/// edge repaints once while the arrow is live.
pub(crate) const fn realized_bucket(elapsed: Duration) -> u64 {
    elapsed.as_secs() / REALIZED_BUCKET.as_secs()
}

/// App-level "a strictly newer build is staged" state, set from `Wake::UpdateStaged`;
/// drives the version-menu ⬆️, the tab-strip `↻` and `RepaintKey::relaunch_fp`.
#[derive(Clone, Debug)]
pub(crate) struct RelaunchNotice {
    /// The staged build number (the dismiss key: a newer build re-arms the nudge).
    pub(crate) build: u64,
    /// The staged human version (for the update window title).
    pub(crate) version: String,
}

/// Structural repaint key for [`RelaunchNotice`], folded into `RepaintKey::relaunch_fp`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct RelaunchRepaintKey {
    /// Staged build number (stored verbatim so build changes never collide).
    pub(crate) build: u64,
    /// Byte length of [`RelaunchNotice::version`].
    pub(crate) version_len: usize,
    /// Deterministic FNV-1a digest of [`RelaunchNotice::version`].
    pub(crate) version_hash: u64,
}

impl RelaunchNotice {
    /// Structural repaint key folded into `RepaintKey::relaunch_fp` so the `↻` alert's
    /// appear / build-change forces a present (`None` when hidden).
    pub(crate) fn fingerprint(&self) -> RelaunchRepaintKey {
        RelaunchRepaintKey {
            build: self.build,
            version_len: self.version.len(),
            version_hash: aterm_hash::fnv1a_64(self.version.as_bytes()),
        }
    }
}

#[cfg(test)]
#[path = "relaunch_notice_tests.rs"]
mod tests;
