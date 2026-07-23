// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The "update ready" nudge STATE (proof-carrying DSU, RFC Rung 2). The nudge itself is
//! the ⬆️-suffixed VERSION menu title + its one-click "Update to v<staged> — restart
//! now" item (see `crate::menu::update_version_menu` / the palette's Version row), plus
//! the subtle LEADING `↻` icon in the off-macOS tab strip (see [`crate::tab_bar`]
//! `TabHit::Update`) — chrome, not a disruptive banner. This module holds only the small
//! pieces of App state that drive it: which build/version is staged (the repaint
//! fingerprint), and the post-update REALIZED-arrow timing (TTL + fade law + bucket).

use std::time::Duration;

/// How long the post-update "realized" ⬆️ arrow decorates the version menu (and the
/// palette's Version section) before it decays away — the owner's ask: a realized
/// upgrade arrow "that fades away after so many minutes", drawing attention to the
/// version-number menu without a permanent badge.
pub(crate) const REALIZED_ARROW_TTL: Duration = Duration::from_secs(10 * 60);

/// The realized-arrow fade re-present granularity: the palette fingerprint folds the
/// elapsed bucket at this quantum (and the `about_to_wait` sweep arms one wake per
/// bucket edge), so an OPEN palette fades progressively — one present per step, never
/// per-frame churn (the notice.rs quantized-fp pattern, stretched to minutes).
pub(crate) const REALIZED_BUCKET: Duration = Duration::from_secs(30);

/// Full-strength hold before the realized arrow starts fading (mirrors the notice
/// pill's hold-then-fade shape: the fresh-upgrade moment reads at full alpha first).
const REALIZED_HOLD: Duration = Duration::from_secs(120);

/// The realized-arrow alpha at `elapsed` since the post-update boot: `1.0` through the
/// hold, then a linear ramp to `0.0` at [`REALIZED_ARROW_TTL`]. Pure — the palette
/// painter and the unit tests share this one law. (Under `MotionPolicy::Reduced` the
/// caller freezes alpha at full instead of sampling this.)
pub(crate) fn realized_alpha(elapsed: Duration) -> f32 {
    if elapsed <= REALIZED_HOLD {
        return 1.0;
    }
    let fade = (REALIZED_ARROW_TTL - REALIZED_HOLD).as_secs_f32();
    ((REALIZED_ARROW_TTL.as_secs_f32() - elapsed.as_secs_f32()) / fade).clamp(0.0, 1.0)
}

/// The ~30s fade bucket index at `elapsed` — the quantized time term folded into the
/// palette fingerprint (and compared by the sweep) so each bucket edge repaints exactly
/// once while the arrow is live.
pub(crate) fn realized_bucket(elapsed: Duration) -> u64 {
    elapsed.as_secs() / REALIZED_BUCKET.as_secs()
}

/// The "a strictly-newer build is staged" nudge state (global, App-level). Set from a
/// `Wake::UpdateStaged`; drives the version-menu ⬆️ / tab-strip `↻` alert + the
/// `RepaintKey::relaunch_fp`.
#[derive(Clone, Debug)]
pub(crate) struct RelaunchNotice {
    /// The staged build number (the dismiss key: a newer build re-arms the nudge).
    pub(crate) build: u64,
    /// The staged human version (for the update window title).
    pub(crate) version: String,
}

impl RelaunchNotice {
    /// A repaint fingerprint folded into `RepaintKey::relaunch_fp` so the `↻` alert's
    /// appear / build-change forces exactly one present. Never `0` while shown.
    pub(crate) fn fingerprint(&self) -> u64 {
        use std::hash::{Hash, Hasher};
        let mut h = std::collections::hash_map::DefaultHasher::new();
        self.build.hash(&mut h);
        self.version.hash(&mut h);
        h.finish() | 1
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fingerprint_tracks_build_and_is_nonzero() {
        let a = RelaunchNotice {
            build: 828,
            version: "0.5.14".into(),
        }
        .fingerprint();
        assert_ne!(a, 0);
        let b = RelaunchNotice {
            build: 999,
            version: "0.5.14".into(),
        }
        .fingerprint();
        assert_ne!(a, b);
    }

    /// The realized-arrow fade law: full through the hold, strictly inside (0,1) mid-fade,
    /// exactly 0 at/after the TTL — the expiry the `about_to_wait` sweep keys off.
    #[test]
    fn realized_alpha_holds_fades_and_expires() {
        assert_eq!(realized_alpha(Duration::ZERO), 1.0, "full at boot");
        assert_eq!(
            realized_alpha(Duration::from_secs(60)),
            1.0,
            "full through the hold"
        );
        let mid = realized_alpha(Duration::from_secs(6 * 60));
        assert!(mid > 0.0 && mid < 1.0, "mid-fade alpha {mid}");
        // Monotone non-increasing across the fade.
        let later = realized_alpha(Duration::from_secs(9 * 60));
        assert!(later < mid, "fade is monotone: {later} < {mid}");
        assert_eq!(realized_alpha(REALIZED_ARROW_TTL), 0.0, "gone at TTL");
        assert_eq!(
            realized_alpha(REALIZED_ARROW_TTL + Duration::from_secs(60)),
            0.0
        );
    }

    /// The fade bucket steps once per [`REALIZED_BUCKET`] — the quantum the palette fp
    /// folds, so an open palette re-presents once per step and never every frame.
    #[test]
    fn realized_bucket_steps_at_the_quantum() {
        assert_eq!(realized_bucket(Duration::ZERO), 0);
        assert_eq!(realized_bucket(REALIZED_BUCKET - Duration::from_secs(1)), 0);
        assert_eq!(realized_bucket(REALIZED_BUCKET), 1);
        assert_eq!(
            realized_bucket(REALIZED_BUCKET * 7 + Duration::from_secs(3)),
            7
        );
    }
}
