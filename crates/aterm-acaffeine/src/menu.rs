// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! aCaffeine status-item click classification, right-click menu composition, and menu item tags.

use crate::launchd::Startup;
use crate::model::{Choice, Clocks, Mode, Pause, describe_left};

/// What a click on the mug does.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Click {
    /// Turn keeping awake on (forever) or off.
    Toggle,
    /// Open the menu.
    Menu,
}

/// Classify a status-item click as [`Click::Menu`] (right/Control-click) or [`Click::Toggle`].
#[inline]
#[must_use]
pub const fn classify_click(right_button: bool, control_held: bool) -> Click {
    if right_button || control_held {
        Click::Menu
    } else {
        Click::Toggle
    }
}

/// One user action from the menu, carried by the item's tag.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MenuAction {
    /// Activate for a span, or forever.
    Activate(Choice),
    /// Keep Awake with Lid Closed: turn the person's opt-in on or off.
    ToggleLid,
    /// Enable at Startup: add or remove the login job.
    ToggleStartup,
    /// Quit: turn off and end the mug.
    Quit,
}

impl MenuAction {
    /// Every action, in menu order.
    pub const ALL: [Self; 7] = [
        Self::Activate(Choice::Hours1),
        Self::Activate(Choice::Hours8),
        Self::Activate(Choice::Hours24),
        Self::Activate(Choice::Forever),
        Self::ToggleLid,
        Self::ToggleStartup,
        Self::Quit,
    ];

    /// Nonzero `NSMenuItem.tag` value for this action.
    #[inline]
    #[must_use]
    pub const fn tag(self) -> isize {
        match self {
            Self::Activate(Choice::Hours1) => 1,
            Self::Activate(Choice::Hours8) => 2,
            Self::Activate(Choice::Hours24) => 3,
            Self::Activate(Choice::Forever) => 4,
            Self::ToggleStartup => 5,
            Self::Quit => 6,
            Self::ToggleLid => 7,
        }
    }

    /// The inverse of [`MenuAction::tag`]; an unknown tag is `None` (the item does nothing).
    #[inline]
    #[must_use]
    pub const fn from_tag(tag: isize) -> Option<Self> {
        match tag {
            1 => Some(Self::Activate(Choice::Hours1)),
            2 => Some(Self::Activate(Choice::Hours8)),
            3 => Some(Self::Activate(Choice::Hours24)),
            4 => Some(Self::Activate(Choice::Forever)),
            5 => Some(Self::ToggleStartup),
            6 => Some(Self::Quit),
            7 => Some(Self::ToggleLid),
            _ => None,
        }
    }
}

/// One row of the menu.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Row {
    /// A line of text that does nothing (the about line).
    Info(String),
    /// A separator.
    Separator,
    /// An item that does `action`, with a checkmark when `checked`.
    Action {
        /// Its title.
        label: String,
        /// What choosing it does.
        action: MenuAction,
        /// Whether it shows a checkmark.
        checked: bool,
    },
}

/// The menu's one-line about row.
#[must_use]
pub fn about_line(version: &str) -> String {
    format!(
        "aterm aCaffeine {version} \u{2014} keeps this Mac awake, {}",
        aterm_types::identity::ORIGIN_LINE
    )
}

/// The title of Quit.
pub const QUIT_LABEL: &str = "Quit aCaffeine";
/// The title of Keep Awake with Lid Closed: macOS honours the assertion it holds only on AC
/// power, so the title says so.
pub const LID_LABEL: &str = "Keep Awake with Lid Closed (on AC Power)";
/// The title of Enable at Startup.
pub const STARTUP_LABEL: &str = "Enable at Startup";
/// Menu title for Enable at Startup when blocked in Login Items ([`Startup::Blocked`]).
pub const STARTUP_BLOCKED_LABEL: &str = "Enable at Startup \u{2014} Allow in Login Items\u{2026}";

/// The line a paused mug's menu shows under its about line ([`Pause`]).
#[must_use]
pub const fn pause_line(pause: Pause) -> &'static str {
    match pause {
        Pause::LidClosed => "Paused while the lid is closed \u{2014} on again when it opens",
        Pause::Asleep => "Paused while this Mac sleeps \u{2014} on again when it wakes",
    }
}

/// What the menu shows besides the mode: Enable at Startup (`startup`), Keep Awake with Lid
/// Closed (`lid_awake`), the pause the mug is in, if any, and the `version` the about line
/// names.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Shown<'a> {
    /// Enable at Startup.
    pub startup: Startup,
    /// Keep Awake with Lid Closed.
    pub lid_awake: bool,
    /// Why the mode that is on holds nothing now.
    pub paused: Option<Pause>,
    /// The app version.
    pub version: &'a str,
}

/// Compose the menu rows at `now` for `mode` and what else is `shown`.
#[must_use]
pub fn compose(mode: Mode, now: Clocks, shown: Shown<'_>) -> Vec<Row> {
    let Shown {
        startup,
        lid_awake,
        paused,
        version,
    } = shown;
    let current = mode.choice(now);
    let left = mode.remaining(now);
    let mut rows = vec![Row::Info(about_line(version))];
    if let Some(pause) = paused.filter(|_| mode.is_on(now)) {
        rows.push(Row::Info(pause_line(pause).to_string()));
    }
    rows.push(Row::Separator);
    for choice in Choice::ALL {
        let checked = current == Some(choice);
        let label = match (checked, left) {
            (true, Some(secs)) => {
                format!("{} \u{2014} {} left", choice.label(), describe_left(secs))
            }
            _ => choice.label().to_string(),
        };
        rows.push(Row::Action {
            label,
            action: MenuAction::Activate(choice),
            checked,
        });
    }
    rows.push(Row::Separator);
    rows.push(Row::Action {
        label: LID_LABEL.to_string(),
        action: MenuAction::ToggleLid,
        checked: lid_awake,
    });
    rows.push(Row::Action {
        label: if startup == Startup::Blocked {
            STARTUP_BLOCKED_LABEL
        } else {
            STARTUP_LABEL
        }
        .to_string(),
        action: MenuAction::ToggleStartup,
        checked: startup == Startup::On,
    });
    rows.push(Row::Separator);
    rows.push(Row::Action {
        label: QUIT_LABEL.to_string(),
        action: MenuAction::Quit,
        checked: false,
    });
    rows
}

/// What the tooltip says while the mode is on but macOS refused the activity.
pub const NOT_HELD_TIP: &str =
    "aCaffeine could not keep this Mac awake \u{2014} it asks macOS again every minute";

/// What the mug holds for a mode that is on, as its tooltip says it ([`tooltip`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Face {
    /// It holds what it should: the activity, and with `lid` (Keep Awake with Lid Closed) the
    /// system assertion too.
    Held {
        /// Keep Awake with Lid Closed is on, and its assertion held.
        lid: bool,
    },
    /// macOS refused what it asked for ([`NOT_HELD_TIP`]).
    Refused,
    /// It holds nothing by design, the mode staying on.
    Paused(Pause),
}

/// Status-item tooltip for `mode` at `now`, the mug's hold being `face`: an on mode macOS refused
/// does not keep the Mac awake ([`NOT_HELD_TIP`]), a paused one says why and when it holds again,
/// and a held span says the time it has left, after which the Mac may sleep.
#[must_use]
pub fn tooltip(mode: Mode, now: Clocks, face: Face) -> String {
    let lid = |text: String, lid: bool| {
        if lid {
            format!("{text}, the lid closed too while on AC power")
        } else {
            text
        }
    };
    match (mode.settled(now), face) {
        (Mode::Off, _) => "aCaffeine is off \u{2014} click to keep this Mac awake".to_string(),
        (_, Face::Refused) => NOT_HELD_TIP.to_string(),
        (_, Face::Paused(pause)) => format!("aCaffeine: {}", pause_line(pause)),
        (Mode::Forever, Face::Held { lid: with }) => lid(
            "aCaffeine is keeping this Mac awake until you click".to_string(),
            with,
        ),
        (Mode::Until { deadline, .. }, Face::Held { lid: with }) => lid(
            format!(
                "aCaffeine is keeping this Mac awake for {} more",
                describe_left(deadline - now.since_boot)
            ),
            with,
        ),
    }
}

#[cfg(test)]
mod tests {
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
        assert!(
            tooltip(Mode::chosen(Choice::Hours1, NOW), NOW + HOUR, Face::Refused).contains("off")
        );
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
}
