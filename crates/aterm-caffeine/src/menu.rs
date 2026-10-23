// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The mug's clicks and its right-click menu, pure: what a click means, which rows the menu
//! shows, and the stable tags its items carry to the AppKit target.

use crate::launchd::Startup;
use crate::model::{Choice, Mode, describe_left};

/// What a click on the mug does.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Click {
    /// Turn keeping awake on (forever) or off.
    Toggle,
    /// Open the menu.
    Menu,
}

/// A click with the right button, or with the left while Control is held, opens the menu;
/// any other click toggles.
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

    /// The `NSMenuItem.tag` this action rides in. Stable once assigned; `0` is never used, so
    /// an untagged item decodes to nothing.
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
        "aterm Caffeine {version} \u{2014} keeps this Mac awake, {}",
        aterm_types::identity::ORIGIN_LINE
    )
}

/// The title of Quit.
pub const QUIT_LABEL: &str = "Quit Caffeine";
/// The title of Enable at Startup.
pub const STARTUP_LABEL: &str = "Enable at Startup";
/// Its title while Login Items' switch keeps its login plist from loading
/// ([`Startup::Blocked`]): choosing it opens Login Items, where the person turns it on.
pub const STARTUP_BLOCKED_LABEL: &str = "Enable at Startup \u{2014} Allow in Login Items\u{2026}";

/// The menu at `now` for `mode`, with Enable at Startup checked when `startup` is on (and
/// titled for Login Items while it is blocked there; unchecked then, and while launchd has
/// it disabled, where choosing it lifts that): the about row, the four
/// Activate choices (the one in effect checked, a timed one with its time left), Enable at
/// Startup, and Quit.
#[must_use]
pub fn compose(mode: Mode, now: u64, startup: Startup, version: &str) -> Vec<Row> {
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

/// The mug's tooltip in `mode` at `now`. A keep-awake activity that could not begin has
/// already turned the mode off, so the tooltip reads off then too.
#[must_use]
pub fn tooltip(mode: Mode, now: u64) -> String {
    match mode.settled(now) {
        Mode::Off => "Caffeine is off \u{2014} click to keep this Mac awake".to_string(),
        Mode::Forever => "Caffeine is keeping this Mac awake until you click".to_string(),
        Mode::Until { deadline, .. } => format!(
            "Caffeine is keeping this Mac awake for {} more",
            describe_left(deadline - now)
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::HOUR;

    const NOW: u64 = 1_790_000_000;

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
        assert!(line.starts_with("aterm Caffeine 0.106.0 "), "{line}");
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
        // Expired: nothing checked, no time left shown.
        let expired = actions(&compose(mode, NOW + 8 * HOUR, Startup::Off, "1"));
        assert!(expired.iter().all(|(l, _, c)| !c && !l.contains("left")));
    }

    /// Blocked in Login Items, the row says where startup is turned on, unchecked: the plist
    /// is there, but no login loads it.
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
}
