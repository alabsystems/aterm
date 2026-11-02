// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The Macs asking to join ALab as exceptions entries ([`Family::Join`],
//! `docs/DESIGN-automatic-alab-lan-admission-2026-10-29.md` "No join alerts"): a fact for later,
//! never an alert on screen. A device nobody let in costs nothing while it waits (it keeps
//! asking), so it is one Info entry in the digest, filed under its join code, with the press that
//! lets it in beside it: Allow on a member's desk ([`Intent::AllowJoin`], a person's own press,
//! whose phrase shows in this Mac's sheet), or Approve on an admin Mac for a request its approver
//! left to a person ([`Intent::ApproveJoin`]). From [`MANY_AT`] asking Macs on, one entry stands
//! for them all. The name in a title is the one the device chose, so a title says "calling
//! itself" and shows it cleaned and cut ([`shown_name`]); Macs that chose one name are told as
//! that many.
//!
//! This module is the family's one producer: [`crate::App::refresh_join_entries`] files the desk's
//! rows and the approver's record together, every time either changes, and ends at once each
//! entry whose Mac no longer asks (its row lapsed, closed on an admission, was ignored, or was
//! heard on a network this Mac left).

use aterm_messages::Intent;
use aterm_messages::exceptions::{Entry, Family};
use aterm_messages::need::{Reason, Then, Why};
use aterm_secrets::client::Asking;
use aterm_secrets::members::desk::{Row, RowState};

/// From this many asking Macs on, one entry stands for them all (a few show one each).
pub(crate) const MANY_AT: usize = 4;

/// The cause an asking Mac's entry is filed under, its join code being the subject.
const CAUSE: &str = "asking";

/// The subject-less cause of the one entry for [`MANY_AT`] or more asking Macs.
const MANY_CAUSE: &str = "many";

/// Chars of a device's chosen name a title shows: the rest of the title fits the glass title's
/// 48 characters with room to spare.
const NAME_CHARS: usize = 20;

/// One Mac asking to join, as an entry shows it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Asker {
    /// Its join code, which the asking Mac shows too: the entry's subject.
    code: String,
    /// The name it chose.
    name: String,
    /// The press that lets it in from this Mac, where one does.
    nav: Option<Intent>,
    /// What the step says, `name` in it.
    then: String,
    /// The membership log removed it (F16): the removal's own words.
    removed: Option<String>,
}

/// The askers of a member desk's rows still asking: Allow where this Mac can allow, Allow again
/// for a Mac a plain removal took out, nothing for a quorum's removal. A row the default ALab
/// policy forwards to the vault's own admission (`Row::automatic`: an asking Mac on this member's
/// home ALab LAN) files nothing, since its Mac joins with nobody.
pub(crate) fn desk_askers(rows: &[Row], can_allow: bool) -> Vec<Asker> {
    rows.iter()
        .filter(|row| matches!(row.state, RowState::Asking | RowState::Allowed(_)))
        .filter(|row| !row.automatic)
        .map(|row| {
            let name = shown_name(&row.name);
            let (nav, then) = match &row.removed {
                Some(note) if note.quorum => {
                    (None, format!("Reset {name} to let it in as a new Mac"))
                }
                Some(_) if can_allow => (
                    Some(Intent::AllowJoinAgain { request: row.id }),
                    format!("Press Allow again, then type this Mac's phrase on {name}"),
                ),
                None if can_allow => (
                    Some(Intent::AllowJoin { request: row.id }),
                    format!("Press Allow, then type this Mac's phrase on {name}"),
                ),
                _ => (
                    None,
                    format!(
                        "Press Allow on a member that can allow, then type its phrase on {name}"
                    ),
                ),
            };
            Asker {
                code: row.code.clone(),
                name,
                nav,
                then,
                removed: row
                    .removed
                    .as_ref()
                    .map(aterm_secrets::client::RemovedNote::title),
            }
        })
        .collect()
}

/// The askers of an admin approver's record: Approve for exactly each key.
pub(crate) fn approver_askers(asking: &[Asking]) -> Vec<Asker> {
    asking
        .iter()
        .map(|asking| {
            let name = shown_name(&asking.host);
            Asker {
                code: asking.code.clone(),
                then: format!("Approve it if {name} shows code {}", asking.code),
                name,
                nav: Some(Intent::ApproveJoin {
                    recipient: asking.recipient.clone(),
                }),
                removed: None,
            }
        })
        .collect()
}

/// A device's chosen name as a title shows it: ASCII letters, digits, `-`, `_` and `.` kept and
/// every other char a `-` (no seam, quote or second word an asker could plant), at most
/// [`NAME_CHARS`] of them, "no name" for none.
pub(crate) fn shown_name(name: &str) -> String {
    let cleaned: String = name
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.') {
                c
            } else {
                '-'
            }
        })
        .collect();
    let cleaned = cleaned.trim_matches(|c| c == '.' || c == '-');
    if cleaned.is_empty() {
        return "no name".to_owned();
    }
    if cleaned.chars().count() <= NAME_CHARS {
        return cleaned.to_owned();
    }
    let head: String = cleaned.chars().take(NAME_CHARS - 1).collect();
    format!("{head}\u{2026}")
}

/// The entries for `askers`: one per join code (the first asker of a code wins), or one for
/// them all from [`MANY_AT`] on.
pub(crate) fn entries(askers: Vec<Asker>) -> Vec<Entry> {
    let mut seen = std::collections::BTreeSet::new();
    let askers: Vec<Asker> = askers
        .into_iter()
        .filter(|asker| seen.insert(asker.code.clone()))
        .collect();
    if askers.len() >= MANY_AT {
        return vec![
            Entry::new(
                Family::Join,
                "",
                MANY_CAUSE,
                &format!("{} Macs ask to join ALab", askers.len()),
                Why::OnlyYou(Reason::new("only a person lets a Mac into ALab")),
                Then::step("Run aterm members requests to see each one"),
            )
            .log_line(format!("members: {} Macs ask to join ALab", askers.len())),
        ];
    }
    askers
        .iter()
        .map(|asker| {
            let namesakes = askers.iter().filter(|a| a.name == asker.name).count();
            let what = match (namesakes, &asker.removed) {
                (1, None) => format!("New Mac calling itself {}", asker.name),
                (1, Some(_)) => format!("Removed Mac calling itself {}", asker.name),
                (n, _) => format!("{n} Macs calling themselves {}", asker.name),
            };
            let mut entry = Entry::new(
                Family::Join,
                &asker.code,
                CAUSE,
                &what,
                Why::OnlyYou(Reason::new(format!(
                    "only a person lets a Mac into ALab, and this one shows code {}",
                    asker.code
                ))),
                Then::step(&asker.then),
            )
            .log_line(format!(
                "members: a Mac calling itself {} asks to join ALab (code {})",
                asker.name, asker.code
            ));
            if let Some(removed) = &asker.removed {
                entry = entry.more(removed);
            }
            match &asker.nav {
                Some(nav) => entry.show(nav.clone()),
                None => entry,
            }
        })
        .collect()
}

#[cfg(test)]
#[path = "join_entries_tests.rs"]
mod tests;
