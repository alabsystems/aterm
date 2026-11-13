// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use aterm_netplace::{Kind, Place, Where};

const T: u64 = 1_800_000_000_000;

/// The join's words follow the place. On the ALab LAN, the automatic admission's; holding the
/// fleet's book off it (or where the place cannot tell), the proof that lets an ALab Mac back
/// in by itself; away from it with no book (another network, a phone hotspot, a cellular link,
/// no network), what lets this Mac in; and where the place cannot say (a Mac that was never a
/// member has no ALab LAN to compare a plain LAN with, a network it is still checking, a
/// remembered ALab LAN gone silent), both.
/// NEGATIVE CONTROLS: `let_in` and `asking_note` answering the home words whatever the place
/// fail at "hotspot"; `JoinPlace::of` that ignores the book (`proves` → `false`) fails at
/// "a hotspot, holding the book".
#[test]
fn the_joins_words_follow_the_place() {
    let home = aterm_netplace::sample::home(T, 3);
    let bookless = |at: Where| Place {
        at,
        recognised: None,
        known: None,
        ..home.clone()
    };
    let words = |at: JoinPlace| (let_in(at), asking_note(at));
    assert_eq!(JoinPlace::of(&home), JoinPlace::Home);
    assert_eq!(
        words(JoinPlace::Home),
        (LET_IN, aterm_types::macos_setup::VAULT_ASKING_NOTE)
    );
    let hotspot = Place {
        known: None,
        ..aterm_netplace::sample::hotspot(T, T)
    };
    let silent = Place {
        once_alab: true,
        ..bookless(Where::Remote(Kind::Lan))
    };
    for (place, at, why) in [
        (hotspot, JoinPlace::Away, "hotspot"),
        (
            bookless(Where::Remote(Kind::Lan)),
            JoinPlace::Away,
            "another network",
        ),
        (
            bookless(Where::Network(Kind::Cellular)),
            JoinPlace::Away,
            "cellular",
        ),
        (bookless(Where::Offline), JoinPlace::Away, "no network"),
        (
            bookless(Where::Network(Kind::Lan)),
            JoinPlace::Unknown,
            "a LAN with no book",
        ),
        (
            bookless(Where::Checking(aterm_netplace::Checking::Moved)),
            JoinPlace::Unknown,
            "checking",
        ),
        (
            silent,
            JoinPlace::Unknown,
            "a remembered ALab LAN gone silent",
        ),
        (
            aterm_netplace::sample::hotspot(T, T),
            JoinPlace::Proves,
            "a hotspot, holding the book",
        ),
        (
            Place {
                known: Some(1),
                ..bookless(Where::Network(Kind::Lan))
            },
            JoinPlace::Proves,
            "a LAN, holding the book",
        ),
    ] {
        assert_eq!(JoinPlace::of(&place), at, "{why}");
    }
    assert_eq!(
        words(JoinPlace::Away),
        (LET_IN_AWAY, ASKING_AWAY_NOTE),
        "hotspot"
    );
    assert_eq!(
        words(JoinPlace::Proves),
        (LET_IN_PROVES, ASKING_PROVES_NOTE)
    );
    assert_eq!(
        words(JoinPlace::Unknown),
        (LET_IN_UNKNOWN, ASKING_UNKNOWN_NOTE)
    );
}
