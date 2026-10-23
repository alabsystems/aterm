// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Heuristic mail-to-turn link resolution for session-log folds ([`mail_link_of`]).

use crate::field::{Key, Val};
use crate::rec::Rec;
use crate::vocab::{Actor, Conf};

/// How long before a turn's open a mail may have been delivered and still be the turn's.
pub const MAIL_LINK_WINDOW_MS: u64 = 60_000;

/// The waiting mails a fold holds at most (the oldest gives way).
pub const MAX_MAIL_WAITING: usize = 4;

/// A delivered `kind=task` mail no turn has taken yet.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MailWaiting {
    /// Its inbox offset (`off=`).
    pub off: u64,
    /// When it was delivered (wall ms).
    pub t: u64,
}

/// A turn's link to the mail it most likely acted on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MailLink {
    /// The mail's inbox offset.
    pub off: u64,
    /// Always [`Conf::Heuristic`] as the fold makes it.
    pub conf: Conf,
}

/// What a turn's open makes of the waiting mails ([`mail_link_of`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MailLinkOf {
    /// No link: none waiting in the window, or the turn is a person's or names its cause.
    None,
    /// The one waiting mail, by its offset.
    Link(u64),
    /// Two or more waiting in the window.
    Ambiguous,
}

/// Whether `m` lies in the window of a turn opened at `turn_t`.
#[must_use]
pub const fn mail_in_window(m: &MailWaiting, turn_t: u64) -> bool {
    m.t <= turn_t && turn_t - m.t <= MAIL_LINK_WINDOW_MS
}

/// Whether the turn open `turn` declares what it acted on: `cause=`, or `cause_k=` on the bus.
#[must_use]
pub fn names_cause(turn: &Rec) -> bool {
    turn.f.get(Key::Cause).is_some() || turn.f.get(Key::CauseKind).is_some()
}

/// THE RULE: what the turn open `turn` makes of `waiting`.
#[must_use]
pub fn mail_link_of(waiting: &[MailWaiting], turn: &Rec) -> MailLinkOf {
    if turn.by == Actor::Human || names_cause(turn) {
        return MailLinkOf::None;
    }
    let mut inside = waiting.iter().filter(|m| mail_in_window(m, turn.t));
    match (inside.next(), inside.next()) {
        (None, _) => MailLinkOf::None,
        (Some(m), None) => MailLinkOf::Link(m.off),
        (Some(_), Some(_)) => MailLinkOf::Ambiguous,
    }
}

/// The mail `cause=@<off>` names, if the turn's cause is mail.
fn caused_off(turn: &Rec) -> Option<u64> {
    match turn.f.get(Key::Cause) {
        Some(Val::Id(id)) => id.as_str().strip_prefix('@')?.parse().ok(),
        _ => None,
    }
}

/// The waiting mails after the turn open `turn` decided `decided`: a person's turn takes them all;
/// an explicit cause the mail it names; a decided turn every mail it weighed (the one it linked, or
/// the ambiguous ones); and every turn the mails past its window, which no later turn can take.
pub fn after_turn(waiting: &mut Vec<MailWaiting>, turn: &Rec, decided: MailLinkOf) {
    if turn.by == Actor::Human {
        waiting.clear();
        return;
    }
    let named = caused_off(turn);
    waiting.retain(|m| {
        let aged = m.t <= turn.t && !mail_in_window(m, turn.t);
        let weighed = !matches!(decided, MailLinkOf::None) && mail_in_window(m, turn.t);
        !(aged || weighed || named == Some(m.off))
    });
}
