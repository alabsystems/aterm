// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The permission chip's box as rows: what each click and each mode says, whole.

use aterm_agent::harness::lights::Mode;
use aterm_agent::supervise::policy::claude_settings::{PermissionDefault, Resolved};

use super::*;
use crate::autopilot_menu::MenuRow;

const FILE: &str = "/etc/claude-code/managed-settings.json";

fn default(mode: Resolved, off: bool) -> PermissionDefault {
    PermissionDefault {
        mode,
        bypass_disabled: off.then(|| FILE.to_owned()),
    }
}

fn named(mode: Mode) -> Resolved {
    Resolved::Mode {
        mode,
        source: FILE.to_owned(),
    }
}

/// The notes of `rows` as the sentences they were wrapped from: a note's first line carries
/// what a reader is told for all of its lines.
fn sentences(rows: &[MenuRow]) -> Vec<String> {
    rows.iter()
        .filter_map(|row| match row {
            MenuRow::Note { spoken, .. } if !spoken.is_empty() => Some(spoken.clone()),
            _ => None,
        })
        .collect()
}

/// THE CLICK'S ONE DECISION: off, a click turns yolo on, unless the settings turn bypass off,
/// when it changes nothing and names the file; on, it leaves for the settings' default, read,
/// unset or unread alike, unless that default is yolo, when it changes nothing and names the
/// file. Only the clicks that change something stand a restart.
#[test]
fn a_click_stands_a_restart_only_where_it_would_change_the_mode() {
    for pill in [
        Mode::Auto,
        Mode::Plan,
        Mode::Manual,
        Mode::AcceptEdits,
        Mode::DontAsk,
    ] {
        assert_eq!(
            click_told(pill, default(named(Mode::Bypass), false)),
            (Told::TurningOn, true),
            "{pill:?}: the default is not asked on the way in"
        );
        assert_eq!(
            click_told(pill, default(Resolved::Unset, true)),
            (Told::BypassOff(FILE.to_owned()), false),
            "{pill:?}"
        );
    }
    for to in [
        named(Mode::Plan),
        Resolved::Unset,
        Resolved::Unread("its settings (x) do not parse".to_owned()),
    ] {
        assert_eq!(
            click_told(Mode::Bypass, default(to.clone(), false)),
            (Told::Leaving(to), true)
        );
    }
    assert_eq!(
        click_told(Mode::Bypass, default(named(Mode::Bypass), false)),
        (Told::DefaultIsYolo(FILE.to_owned()), false)
    );
}

/// EVERY BOX SAYS ITS MODE WHOLE: its header is the mode's name, its meaning stands as one
/// sentence however narrow the card (wrapped onto more rows, never cut with an ellipsis, each
/// row within the room), and a box for a switch says what the switch costs. A mode that asks
/// carries the approval line; yolo, which asks nothing, does not.
#[test]
fn every_box_says_its_mode_whole_at_every_width() {
    for mode in [
        Mode::Bypass,
        Mode::Auto,
        Mode::Plan,
        Mode::Manual,
        Mode::AcceptEdits,
        Mode::DontAsk,
    ] {
        for room in [24, 30, 44] {
            let told = if mode == Mode::Bypass {
                Told::TurningOn
            } else {
                Told::Leaving(named(mode))
            };
            let (rows, spoken) = rows(&told, room);
            assert_eq!(rows[0], MenuRow::Header(mode_name(mode).to_owned()));
            assert_eq!(spoken, format!("Permission mode, {}", mode_name(mode)));
            let said = sentences(&rows);
            assert!(
                said.iter().any(|s| s == mode_means(mode)),
                "{mode:?}@{room}: {said:?}"
            );
            assert!(said.iter().any(|s| s == COST), "{mode:?}@{room}: the cost");
            assert_eq!(
                said.iter().any(|s| s == APPROVE),
                mode != Mode::Bypass,
                "{mode:?}@{room}: the approval line"
            );
            for row in &rows {
                if let MenuRow::Note { text, .. } = row {
                    assert!(
                        crate::tab_menu::display_cells(text) <= room,
                        "{mode:?}@{room}: {text:?} fits"
                    );
                    assert!(!text.contains('\u{2026}'), "{mode:?}@{room}: {text:?}");
                }
            }
            assert!(
                !rows.iter().any(|row| matches!(row, MenuRow::Item { .. })),
                "{mode:?}: nothing to choose"
            );
        }
    }
}

/// WHAT THE BOX SAYS WHERE NOTHING CHANGED OR NOTHING IS KNOWN: a default of yolo and a bypass
/// turned off each name the settings file and its key and say no cost, since nothing restarts;
/// an unset default says Claude Code picks its own and what that was measured as; an unread one
/// says why, and names no mode.
#[test]
fn the_box_names_the_settings_file_and_what_it_could_not_read() {
    let all = |told: &Told| sentences(&rows(told, 44).0).join(" ");
    let yolo = all(&Told::DefaultIsYolo(FILE.to_owned()));
    assert!(yolo.contains(FILE) && yolo.contains("permissions.defaultMode"));
    assert!(!yolo.contains(COST), "{yolo}");
    let off = all(&Told::BypassOff(FILE.to_owned()));
    assert!(off.contains(FILE) && off.contains("disableBypassPermissionsMode"));
    assert!(!off.contains(COST), "{off}");
    let unset = all(&Told::Leaving(Resolved::Unset));
    assert!(
        unset.contains(UNSET_MEASURED.0) && unset.contains(mode_means(UNSET_MEASURED.1)),
        "what an unset default was measured as: {unset}"
    );
    assert!(unset.contains(COST) && unset.contains(APPROVE));
    let unread = all(&Told::Leaving(Resolved::Unread(
        "its settings (x.json) do not parse".to_owned(),
    )));
    assert!(
        unread.contains("its settings (x.json) do not parse"),
        "the reason: {unread}"
    );
    for mode in [Mode::Auto, Mode::Plan, Mode::Manual] {
        assert!(!unread.contains(mode_means(mode)), "no mode is named");
    }
}

/// A BOX OPENED WITHOUT A CLICK SAYS WHAT A CLICK WOULD DO: turn yolo on, leave it for the
/// named default, change nothing where the default is yolo or bypass is turned off, take a
/// waiting restart back, or nothing once the restart is signalled. Each says a sentence of its
/// own after the mode's; only the clicks that would stand a restart say its cost, and the ones
/// the settings decide name the file. Where the host restarts nothing the box is the mode
/// alone.
#[test]
fn a_box_opened_without_a_click_says_what_a_click_would_do() {
    let said = |mode: Mode, would: &Would| {
        let all = sentences(&rows(&Told::Shown(mode, Some(would.clone())), 44).0);
        let alone = sentences(&rows(&Told::Shown(mode, None), 44).0);
        let own: Vec<String> = all.into_iter().filter(|s| !alone.contains(s)).collect();
        own.join(" ")
    };
    let cases = [
        (
            Mode::Plan,
            would(Mode::Plan, default(Resolved::Unset, false)),
            false,
        ),
        (
            Mode::Bypass,
            would(Mode::Bypass, default(named(Mode::Auto), false)),
            true,
        ),
        (
            Mode::Bypass,
            would(Mode::Bypass, default(named(Mode::Bypass), false)),
            true,
        ),
        (
            Mode::Plan,
            would(Mode::Plan, default(Resolved::Unset, true)),
            true,
        ),
        (Mode::Auto, Would::TakeBack, false),
        (Mode::Auto, Would::Signalled, false),
    ];
    let mut seen = Vec::new();
    for (mode, would, names_file) in &cases {
        let own = said(*mode, would);
        assert_eq!(
            own.contains(COST),
            would.stands(),
            "{would:?}: the cost goes with a restart: {own}"
        );
        assert_eq!(own.contains(FILE), *names_file, "{would:?}: {own}");
        let words = own.replace(COST, "");
        assert!(!words.trim().is_empty(), "{would:?} says what a click does");
        assert!(!seen.contains(&words), "{would:?} says its own: {words}");
        seen.push(words);
    }
    assert!(said(Mode::Auto, &Would::TakeBack).contains(TAKE_BACK));
    assert!(said(Mode::Auto, &Would::Signalled).contains(SIGNALLED));
    let alone = rows(&Told::Shown(Mode::Plan, None), 44).0;
    assert!(
        !alone.iter().any(|row| matches!(row, MenuRow::Rule)),
        "no click, no second part"
    );
    assert_eq!(
        sentences(&alone),
        [mode_means(Mode::Plan).to_owned(), APPROVE.to_owned()]
    );
}
