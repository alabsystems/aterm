// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::model::HOUR;

const NOW: Clocks = Clocks {
    since_boot: 300_000,
    wall: 1_790_000_000,
};

/// The menu for `startup` and `version`, the lid option off and no pause.
fn shown(startup: Startup, version: &str) -> Shown<'_> {
    Shown {
        startup,
        lid_awake: false,
        paused: None,
        version,
    }
}

const HELD: Face = Face::Held { lid: false };

#[test]
fn tags_round_trip_and_unknown_is_inert() {
    for (i, action) in MenuAction::ALL.into_iter().enumerate() {
        assert_ne!(action.tag(), 0);
        assert_eq!(MenuAction::from_tag(action.tag()), Some(action));
        for other in &MenuAction::ALL[i + 1..] {
            assert_ne!(action.tag(), other.tag());
        }
    }
    for tag in [0, -1, 8, isize::MAX, isize::MIN] {
        assert_eq!(MenuAction::from_tag(tag), None, "{tag}");
    }
}

fn actions(rows: &[Row]) -> Vec<(String, MenuAction, bool)> {
    rows.iter()
        .filter_map(|r| match r {
            Row::Action {
                label,
                action,
                checked,
            } => Some((label.clone(), *action, *checked)),
            _ => None,
        })
        .collect()
}

#[test]
fn the_menu_is_about_activate_startup_quit() {
    let rows = compose(Mode::Off, NOW, shown(Startup::Off, "0.106.0"));
    assert_eq!(rows[0], Row::Info(about_line("0.106.0")));
    assert_eq!(rows.iter().filter(|r| matches!(r, Row::Info(_))).count(), 1);
    let shape: Vec<_> = rows
        .iter()
        .map(|r| match r {
            Row::Info(_) => "info",
            Row::Separator => "-",
            Row::Action { .. } => "item",
        })
        .collect();
    assert_eq!(
        shape,
        [
            "info", "-", "item", "item", "item", "item", "-", "item", "item", "-", "item"
        ]
    );
    let items = actions(&rows);
    let order: Vec<_> = items.iter().map(|(_, a, _)| *a).collect();
    assert_eq!(order, MenuAction::ALL);
    assert!(items.iter().all(|(_, _, checked)| !checked));
    assert_eq!(items[0].0, "Activate for 1 Hour");
    assert_eq!(items[3].0, "Activate Forever");
    assert_eq!(items[4].0, LID_LABEL);
    assert_eq!(items[5].0, STARTUP_LABEL);
    assert_eq!(items[6].0, QUIT_LABEL);
}

#[test]
fn the_about_line_is_one_line_with_the_version_and_origin() {
    let line = about_line("0.106.0");
    assert!(line.starts_with("aterm aCaffeine 0.106.0 "), "{line}");
    assert!(line.ends_with(aterm_types::identity::ORIGIN_LINE), "{line}");
    assert!(!line.contains('\n'));
}

#[test]
fn the_choice_in_effect_is_checked_with_its_time_left() {
    let mode = Mode::chosen(Choice::Hours8, NOW);
    let items = actions(&compose(mode, NOW + 48 * 60, shown(Startup::On, "1")));
    let checked: Vec<_> = items
        .iter()
        .filter(|(_, _, c)| *c)
        .map(|(l, a, _)| (l.as_str(), *a))
        .collect();
    assert_eq!(
        checked,
        [
            (
                "Activate for 8 Hours \u{2014} 7 h 12 min left",
                MenuAction::Activate(Choice::Hours8)
            ),
            (STARTUP_LABEL, MenuAction::ToggleStartup),
        ]
    );
    let forever = actions(&compose(Mode::Forever, NOW, shown(Startup::Off, "1")));
    let checked: Vec<_> = forever
        .iter()
        .filter(|(_, _, c)| *c)
        .map(|(l, _, _)| l.as_str())
        .collect();
    assert_eq!(checked, ["Activate Forever"]);
    // A span past its deadline has ended: nothing is checked.
    let expired = actions(&compose(mode, NOW + 8 * HOUR, shown(Startup::Off, "1")));
    assert!(expired.iter().all(|(l, _, c)| !c && !l.contains("left")));
}

#[test]
fn a_blocked_startup_points_at_login_items() {
    let startup_row = |startup| {
        actions(&compose(Mode::Off, NOW, shown(startup, "1")))
            .into_iter()
            .find(|(_, a, _)| *a == MenuAction::ToggleStartup)
            .map(|(l, _, c)| (l, c))
            .expect("the startup row")
    };
    assert_eq!(
        startup_row(Startup::Off),
        (STARTUP_LABEL.to_string(), false)
    );
    assert_eq!(startup_row(Startup::On), (STARTUP_LABEL.to_string(), true));
    assert_eq!(
        startup_row(Startup::Blocked),
        (STARTUP_BLOCKED_LABEL.to_string(), false)
    );
    assert_eq!(
        startup_row(Startup::Disabled),
        (STARTUP_LABEL.to_string(), false)
    );
}

#[test]
fn the_tooltip_says_whether_and_how_long() {
    assert!(tooltip(Mode::Off, NOW, Face::Refused).contains("off"));
    assert!(tooltip(Mode::Forever, NOW, HELD).contains("until you click"));
    let timed = tooltip(Mode::chosen(Choice::Hours1, NOW), NOW + 8 * 60, HELD);
    assert!(timed.contains(&describe_left(52 * 60)), "{timed}");
    assert!(tooltip(Mode::chosen(Choice::Hours1, NOW), NOW + HOUR, Face::Refused).contains("off"));
    for on in [Mode::Forever, Mode::chosen(Choice::Hours1, NOW)] {
        assert_eq!(tooltip(on, NOW, Face::Refused), NOT_HELD_TIP, "{on:?}");
    }
}

/// Eight minutes into Activate for 1 Hour, the wall clock set back a day: the checked row and
/// the tooltip say the 52 minutes the span has left, not a day more.
#[test]
fn a_clock_set_back_shows_the_time_really_left() {
    let mode = Mode::chosen(Choice::Hours1, NOW);
    let back = Clocks {
        since_boot: NOW.since_boot + 8 * 60,
        wall: NOW.wall - 24 * HOUR + 8 * 60,
    };
    let items = actions(&compose(mode, back, shown(Startup::Off, "1")));
    assert_eq!(items[0].0, "Activate for 1 Hour \u{2014} 52 min left");
    assert!(items[0].2);
    assert_eq!(
        tooltip(mode, back, HELD),
        "aCaffeine is keeping this Mac awake for 52 min more"
    );
}

/// Keep Awake with Lid Closed is a checkbox the menu shows as saved, and a mug paused for a
/// closed lid or a sleep says so under its about line while the mode is on; an off mode
/// shows no pause.
#[test]
fn the_lid_row_is_checked_as_chosen_and_a_pause_is_said() {
    let lid_row = |lid_awake| {
        actions(&compose(
            Mode::Forever,
            NOW,
            Shown {
                lid_awake,
                ..shown(Startup::Off, "1")
            },
        ))
        .into_iter()
        .find(|(_, a, _)| *a == MenuAction::ToggleLid)
        .map(|(l, _, c)| (l, c))
        .expect("the lid row")
    };
    assert_eq!(lid_row(false), (LID_LABEL.to_string(), false));
    assert_eq!(lid_row(true), (LID_LABEL.to_string(), true));
    assert!(
        LID_LABEL.contains("AC Power"),
        "it says where macOS honours it"
    );
    for pause in [Pause::LidClosed, Pause::Asleep] {
        let paused = Shown {
            paused: Some(pause),
            ..shown(Startup::Off, "1")
        };
        let rows = compose(Mode::Forever, NOW, paused);
        assert_eq!(
            rows[1],
            Row::Info(pause_line(pause).to_string()),
            "{pause:?}"
        );
        let off = compose(Mode::Off, NOW, paused);
        assert_eq!(off[1], Row::Separator, "{pause:?}: off shows no pause");
    }
    assert!(pause_line(Pause::LidClosed).contains("lid is closed"));
}

/// A paused mug's tooltip says why and when it holds again; a held one with the lid option
/// says it holds lid-closed on AC power; an off mode says off whatever its face.
#[test]
fn the_tooltip_says_a_pause_and_the_lid_option() {
    let paused = tooltip(Mode::Forever, NOW, Face::Paused(Pause::LidClosed));
    assert!(paused.contains("lid is closed"), "{paused}");
    assert!(paused.contains("when it opens"), "{paused}");
    let lid = tooltip(Mode::Forever, NOW, Face::Held { lid: true });
    assert!(lid.contains("lid closed too while on AC power"), "{lid}");
    assert!(
        !tooltip(Mode::Forever, NOW, HELD).contains("lid"),
        "CONTROL"
    );
    assert!(tooltip(Mode::Off, NOW, Face::Paused(Pause::LidClosed)).contains("off"));
}
