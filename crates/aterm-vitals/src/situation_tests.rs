// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

fn lid(lid: Lid) -> Reading<LidReading> {
    Reading::Read(LidReading {
        lid,
        changed_ms: None,
    })
}

fn displays(external: u16) -> Reading<Displays> {
    Reading::Read(Displays { external })
}

/// A4 §15 `a_desktop_draws_no_docked_lid_or_battery_glyph`, the model's half: a desktop
/// (no lid) with two external displays reads no docked state at all, while the laptop with
/// one, open or in clamshell, reads docked, and with none reads undocked. An unread lid or
/// display reader reads unread, never undocked.
/// NEGATIVE CONTROL: deciding docked by the displays alone (`Read(Some(d.external > 0))`
/// for any lid) fails the first assertion: `Read(Some(true))` on the Mac mini.
#[test]
fn a_desktop_reads_no_docked_state_and_a_laptop_at_a_desk_reads_docked() {
    let mini = Situation {
        lid: lid(Lid::None),
        displays: displays(2),
        ..Situation::default()
    };
    assert_eq!(mini.docked(), Reading::Read(None), "a desktop draws none");
    for l in [Lid::Open, Lid::Closed] {
        let at_desk = Situation {
            lid: lid(l),
            displays: displays(1),
            ..Situation::default()
        };
        assert_eq!(at_desk.docked(), Reading::Read(Some(true)), "{l:?}");
        let away = Situation {
            lid: lid(l),
            displays: displays(0),
            ..Situation::default()
        };
        assert_eq!(away.docked(), Reading::Read(Some(false)), "{l:?}");
    }
    let unread = Situation {
        lid: lid(Lid::Open),
        ..Situation::default()
    };
    assert_eq!(unread.docked(), Reading::Unread);
    assert_eq!(Situation::default().docked(), Reading::Unread);
    let off_macos = Situation {
        lid: Reading::Unsupported,
        displays: Reading::Unsupported,
        ..Situation::default()
    };
    assert_eq!(off_macos.docked(), Reading::Unsupported);
}

/// Nothing unread reads as a value: each state names its own `sys` word, and the default
/// situation is unread throughout.
/// NEGATIVE CONTROL: a `why` that answers `None` for `Unread` fails the second assertion.
#[test]
fn an_unread_value_says_why_and_never_reads_as_a_value() {
    assert_eq!(Reading::Read(1u8).why(), None);
    assert_eq!(Reading::<u8>::Unread.why(), Some("not_sampled"));
    assert_eq!(Reading::<u8>::Unsupported.why(), Some("unsupported"));
    for word in ["not_sampled", "unsupported"] {
        assert!(crate::sys::is_unavailable_word(word), "{word}");
    }
    let s = Situation::default();
    assert_eq!(s.machine.read(), None);
    assert_eq!(s.lid.read(), None);
    assert_eq!(s.power.read(), None);
    assert_eq!(s.displays.read(), None);
    assert_eq!(s.awake.read(), None);
}

/// Awake since the last sleep's end, else since boot.
#[test]
fn awake_since_is_the_last_wake_else_the_boot() {
    let mut a = Awake {
        boot_ms: 1_000,
        last_sleep: None,
    };
    assert_eq!(a.since_ms(), 1_000);
    a.last_sleep = Some(SleepSpan {
        from_ms: 5_000,
        to_ms: 9_000,
        cause: Some("Clamshell Sleep".into()),
        wake: Some("RTP.multi-touch".into()),
    });
    assert_eq!(a.since_ms(), 9_000);
}
