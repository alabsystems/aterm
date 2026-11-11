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
#[path = "menu_tests.rs"]
mod tests;
