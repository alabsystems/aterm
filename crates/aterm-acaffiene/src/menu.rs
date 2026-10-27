// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! aCaffiene status-item click classification, right-click menu composition, and menu item tags.

use crate::launchd::Startup;
use crate::model::{Choice, Clocks, Mode, describe_left};

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
    /// Enable at Startup: add or remove the login job.
    ToggleStartup,
    /// Quit: turn off and end the mug.
    Quit,
}

impl MenuAction {
    /// Every action, in menu order.
    pub const ALL: [Self; 6] = [
        Self::Activate(Choice::Hours1),
        Self::Activate(Choice::Hours8),
        Self::Activate(Choice::Hours24),
        Self::Activate(Choice::Forever),
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
        "aterm aCaffiene {version} \u{2014} keeps this Mac awake, {}",
        aterm_types::identity::ORIGIN_LINE
    )
}

/// The title of Quit.
pub const QUIT_LABEL: &str = "Quit aCaffiene";
/// The title of Enable at Startup.
pub const STARTUP_LABEL: &str = "Enable at Startup";
/// Menu title for Enable at Startup when blocked in Login Items ([`Startup::Blocked`]).
pub const STARTUP_BLOCKED_LABEL: &str = "Enable at Startup \u{2014} Allow in Login Items\u{2026}";

/// Compose the menu rows at `now` for `mode`, `startup`, and `version`.
#[must_use]
pub fn compose(mode: Mode, now: Clocks, startup: Startup, version: &str) -> Vec<Row> {
    let current = mode.choice(now);
    let left = mode.remaining(now);
    let mut rows = vec![Row::Info(about_line(version)), Row::Separator];
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

/// Status-item tooltip for `mode` at `now`.
#[must_use]
pub fn tooltip(mode: Mode, now: Clocks) -> String {
    match mode.settled(now) {
        Mode::Off => "aCaffiene is off \u{2014} click to keep this Mac awake".to_string(),
        Mode::Forever => "aCaffiene is keeping this Mac awake until you click".to_string(),
        Mode::Until { deadline, .. } => format!(
            "aCaffiene is keeping this Mac awake for {} more",
            describe_left(deadline - now.since_boot)
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

    #[test]
    fn tags_round_trip_and_unknown_is_inert() {
        for (i, action) in MenuAction::ALL.into_iter().enumerate() {
            assert_ne!(action.tag(), 0);
            assert_eq!(MenuAction::from_tag(action.tag()), Some(action));
            for other in &MenuAction::ALL[i + 1..] {
                assert_ne!(action.tag(), other.tag());
            }
        }
        for tag in [0, -1, 7, isize::MAX, isize::MIN] {
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
        let rows = compose(Mode::Off, NOW, Startup::Off, "0.106.0");
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
                "info", "-", "item", "item", "item", "item", "-", "item", "-", "item"
            ]
        );
        let items = actions(&rows);
        let order: Vec<_> = items.iter().map(|(_, a, _)| *a).collect();
        assert_eq!(order, MenuAction::ALL);
        assert!(items.iter().all(|(_, _, checked)| !checked));
        assert_eq!(items[0].0, "Activate for 1 Hour");
        assert_eq!(items[3].0, "Activate Forever");
        assert_eq!(items[4].0, STARTUP_LABEL);
        assert_eq!(items[5].0, QUIT_LABEL);
    }

    #[test]
    fn the_about_line_is_one_line_with_the_version_and_origin() {
        let line = about_line("0.106.0");
        assert!(line.starts_with("aterm aCaffiene 0.106.0 "), "{line}");
        assert!(line.ends_with(aterm_types::identity::ORIGIN_LINE), "{line}");
        assert!(!line.contains('\n'));
    }

    #[test]
    fn the_choice_in_effect_is_checked_with_its_time_left() {
        let mode = Mode::chosen(Choice::Hours8, NOW);
        let items = actions(&compose(mode, NOW + 48 * 60, Startup::On, "1"));
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
        let forever = actions(&compose(Mode::Forever, NOW, Startup::Off, "1"));
        let checked: Vec<_> = forever
            .iter()
            .filter(|(_, _, c)| *c)
            .map(|(l, _, _)| l.as_str())
            .collect();
        assert_eq!(checked, ["Activate Forever"]);
        let expired = actions(&compose(mode, NOW + 8 * HOUR, Startup::Off, "1"));
        assert!(expired.iter().all(|(l, _, c)| !c && !l.contains("left")));
    }

    #[test]
    fn a_blocked_startup_points_at_login_items() {
        let startup_row = |startup| {
            actions(&compose(Mode::Off, NOW, startup, "1"))
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
        assert!(tooltip(Mode::Off, NOW).contains("off"));
        assert!(tooltip(Mode::Forever, NOW).contains("until you click"));
        let timed = tooltip(Mode::chosen(Choice::Hours1, NOW), NOW + 8 * 60);
        assert!(timed.contains(&describe_left(52 * 60)), "{timed}");
        assert!(tooltip(Mode::chosen(Choice::Hours1, NOW), NOW + HOUR).contains("off"));
    }

    /// Eight minutes into Activate for 1 Hour, the wall clock set back a day: the checked row and
    /// the tooltip say the 52 minutes it has left, not a day more.
    #[test]
    fn a_clock_set_back_shows_the_time_really_left() {
        let mode = Mode::chosen(Choice::Hours1, NOW);
        let back = Clocks {
            since_boot: NOW.since_boot + 8 * 60,
            wall: NOW.wall - 24 * HOUR + 8 * 60,
        };
        let items = actions(&compose(mode, back, Startup::Off, "1"));
        assert_eq!(items[0].0, "Activate for 1 Hour \u{2014} 52 min left");
        assert!(items[0].2);
        assert_eq!(
            tooltip(mode, back),
            "aCaffiene is keeping this Mac awake for 52 min more"
        );
    }
}
