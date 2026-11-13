// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use std::cell::Cell;
use std::time::Instant;

use aterm_containment::DevToolGrant;
use aterm_containment::consent::{
    DevToolList, DevToolListLabel, DevToolListing, DevToolProbe, DevToolProbeLabel, ProbeGate,
};

use super::{DevToolsHelperArm, FdaReach, Lane, Reach, Reading};

pub(crate) const SUBJECT: &str = "com.aterm.test";

thread_local! {
    static OPENS: Cell<u32> = const { Cell::new(0) };
    static OPENED_NOTES: Cell<u32> = const { Cell::new(0) };
    static DECLINE_NOTES: Cell<u32> = const { Cell::new(0) };
    static FDA_OPENS: Cell<u32> = const { Cell::new(0) };
    static FDA_OPENED_NOTES: Cell<u32> = const { Cell::new(0) };
    static FDA_DECLINE_NOTES: Cell<u32> = const { Cell::new(0) };
    static FDA_DECLINED_KEY: std::cell::RefCell<Option<String>> =
        const { std::cell::RefCell::new(None) };
    static FDA_OPENED_AGO: Cell<Option<u64>> = const { Cell::new(None) };
}

pub(crate) fn fda_counts() -> (u32, u32, u32) {
    (
        FDA_OPENS.with(Cell::get),
        FDA_OPENED_NOTES.with(Cell::get),
        FDA_DECLINE_NOTES.with(Cell::get),
    )
}

#[cfg(target_os = "macos")]
pub(crate) fn fda_opened_just_now() -> FdaReach {
    FdaReach {
        opened_within: |_| Some(30),
        ..fda_opening()
    }
}

pub(crate) fn fda_opening() -> FdaReach {
    FdaReach {
        open_pane: || {
            bump(&FDA_OPENS);
            true
        },
        note_opened: |_| bump(&FDA_OPENED_NOTES),
        note_declined: |_| bump(&FDA_DECLINE_NOTES),
        ..FdaReach::inert()
    }
}

/// [`fda_opening`], whose `fda-pane-opened` reads `opened_as` and is written by
/// `note_opened`.
#[cfg(target_os = "macos")]
pub(crate) fn fda_marking(
    opened_as: fn(&str) -> Option<(String, u64)>,
    note_opened: fn(&str),
) -> FdaReach {
    FdaReach {
        opened_as,
        note_opened,
        ..fda_opening()
    }
}

/// [`fda_opening`], with the owner's Not now on the panel holding, said under
/// approved setup revision 1.
#[cfg(target_os = "macos")]
pub(crate) fn fda_declined() -> FdaReach {
    FdaReach {
        declined: || Some((1, u64::MAX)),
        last_declined: || Some((atpkg::devtools::revision_key(1), 1)),
        ..fda_opening()
    }
}

/// [`fda_opening`], whose panel Not now is kept on this thread as
/// `fda-declined` keeps it: noted under its key, read back as the newest, and
/// held for its span; and whose page was opened as long ago as
/// [`set_fda_opened_ago`] says. [`forget_fda_declines`] starts both empty.
#[cfg(target_os = "macos")]
pub(crate) fn fda_keeping_declines() -> FdaReach {
    FdaReach {
        opened_within: |window| FDA_OPENED_AGO.with(Cell::get).filter(|ago| *ago < window),
        declined: || {
            FDA_DECLINED_KEY
                .with(|key| key.borrow().is_some())
                .then_some((1, u64::MAX))
        },
        last_declined: || FDA_DECLINED_KEY.with(|key| key.borrow().clone().map(|key| (key, 1))),
        note_declined: |noted| {
            bump(&FDA_DECLINE_NOTES);
            FDA_DECLINED_KEY.with(|key| *key.borrow_mut() = Some(noted.to_owned()));
        },
        ..fda_opening()
    }
}

/// No Not now kept for [`fda_keeping_declines`] on this thread, and no page
/// opened.
#[cfg(target_os = "macos")]
pub(crate) fn forget_fda_declines() {
    FDA_DECLINED_KEY.with(|key| *key.borrow_mut() = None);
    set_fda_opened_ago(None);
}

/// [`fda_keeping_declines`]'s page was opened `ago` seconds ago (`None`: not
/// within any window).
#[cfg(target_os = "macos")]
pub(crate) fn set_fda_opened_ago(ago: Option<u64>) {
    FDA_OPENED_AGO.with(|cell| cell.set(ago));
}

fn bump(counter: &'static std::thread::LocalKey<Cell<u32>>) {
    counter.with(|c| c.set(c.get() + 1));
}

#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
pub(crate) fn opens() -> u32 {
    OPENS.with(Cell::get)
}

#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
pub(crate) fn opened_notes() -> u32 {
    OPENED_NOTES.with(Cell::get)
}

pub(crate) fn decline_notes() -> u32 {
    DECLINE_NOTES.with(Cell::get)
}

pub(crate) fn list(
    listing: DevToolListing,
    label: DevToolListLabel,
    rows: Option<u32>,
) -> DevToolList {
    DevToolList {
        listing,
        label,
        modified_at_s: None,
        rows,
    }
}

pub(crate) fn absent(rows: u32) -> DevToolList {
    list(DevToolListing::Absent, DevToolListLabel::Read, Some(rows))
}

pub(crate) fn unread() -> DevToolList {
    DevToolList::refused(DevToolListLabel::NoFullDiskAccess)
}

pub(crate) fn on() -> DevToolList {
    list(DevToolListing::On, DevToolListLabel::Read, Some(2))
}

pub(crate) const fn checked_three() -> DevToolProbe {
    DevToolProbe {
        requests: Some(3),
        not_allowed: Some(3),
        ..DevToolProbe::refused(DevToolProbeLabel::Observed)
    }
}

pub(crate) fn reading(grant: DevToolGrant, list: DevToolList) -> Reading {
    Reading {
        grant,
        list,
        log: checked_three(),
        subject: Some(SUBJECT),
    }
}

pub(crate) fn opening() -> Reach {
    Reach {
        open_pane: || {
            bump(&OPENS);
            true
        },
        list_terminal: || Ok(()),
        read_list: |_, _| on(),
        note_opened: |_, _| bump(&OPENED_NOTES),
        note_declined: |_| bump(&DECLINE_NOTES),
        grant_stable: || true,
        refresh_verdict: || Ok(()),
        ..Reach::inert()
    }
}

/// [`opening`], whose verdict refresh does not settle: macOS's queue is backed up.
pub(crate) fn not_settling() -> Reach {
    refreshing_by(|| Err(atpkg::devtools::RefreshFailed::NotSettled(None)))
}

/// [`opening`], whose verdict refresh is `refresh_verdict`.
pub(crate) fn refreshing_by(
    refresh_verdict: fn() -> Result<(), atpkg::devtools::RefreshFailed>,
) -> Reach {
    Reach {
        refresh_verdict,
        ..opening()
    }
}

pub(crate) const DECLINED: (u64, u64) = (1_791_000_000, 1_791_000_000 + 30 * 86_400);

#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
pub(crate) fn declining() -> Reach {
    Reach {
        declined: || Some(DECLINED),
        last_declined: || Some((atpkg::devtools::revision_key(1), DECLINED.0)),
        ..opening()
    }
}

#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
pub(crate) fn marked(last_opened: fn() -> Option<(String, u64)>) -> Reach {
    Reach {
        last_opened,
        ..opening()
    }
}

#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
pub(crate) fn opened(opened_within: fn(u64) -> Option<u64>) -> Reach {
    Reach {
        opened_within,
        ..opening()
    }
}

#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
pub(crate) fn unstable() -> Reach {
    Reach {
        grant_stable: || false,
        ..opening()
    }
}

#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
pub(crate) fn spctl_failing() -> Reach {
    Reach {
        list_terminal: || Err("spctl exited 1".to_string()),
        ..opening()
    }
}

#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
pub(crate) fn refusing() -> Reach {
    Reach {
        open_pane: || {
            bump(&OPENS);
            false
        },
        ..opening()
    }
}

pub(crate) fn showing() -> DevToolsHelperArm {
    DevToolsHelperArm {
        show: |_, _| true,
        settings_running: || true,
        settings_frontmost: || true,
        ..DevToolsHelperArm::inert()
    }
}

/// [`showing`] with its show, in-place turn, close and System Settings' frontmost reading
/// given.
#[cfg(target_os = "macos")]
pub(crate) fn counted(
    show: fn(&super::Content, super::PanelEvents) -> bool,
    update: fn(&super::Content),
    close: fn(),
    settings_frontmost: fn() -> bool,
) -> DevToolsHelperArm {
    DevToolsHelperArm {
        show,
        update,
        close,
        settings_frontmost,
        ..showing()
    }
}

/// `arm`, whose System Settings shows one of its own windows over its main window as
/// `settings_asks` says, to both readings that live ones read by the same rule
/// (`settings_busy` and `settings_asks`).
#[cfg(target_os = "macos")]
pub(crate) fn asking(arm: DevToolsHelperArm, settings_asks: fn() -> bool) -> DevToolsHelperArm {
    DevToolsHelperArm {
        settings_busy: settings_asks,
        settings_asks,
        ..arm
    }
}

#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
pub(crate) fn stand_in(app: &mut crate::App, reach: Reach) {
    fn no_probe(_gate: ProbeGate, _subject: &str) -> DevToolProbe {
        DevToolProbe::refused(DevToolProbeLabel::RefusedDisabled)
    }
    fn subject() -> Option<&'static str> {
        Some(SUBJECT)
    }
    app.consent.set_devtools_arms_for_test(no_probe, subject);
    app.devtools_helper = Lane::windowed_for_test(reach, showing());
    app.devtools_helper
        .note_reading(reading(DevToolGrant::NotGranted, absent(2)), Instant::now());
}
