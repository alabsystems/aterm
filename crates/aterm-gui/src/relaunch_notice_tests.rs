// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

/// NEGATIVE CONTROL: `Some(notice.fingerprint())` is disjoint from `None` and stores `build` structurally.
#[test]
fn fingerprint_tracks_build_and_is_nonzero() {
    let a = RelaunchNotice {
        build: 828,
        version: "0.5.14".into(),
    }
    .fingerprint();
    assert_ne!(Some(a), None);
    assert_eq!(a.build, 828);
    assert_eq!(a.version_len, "0.5.14".len());
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
