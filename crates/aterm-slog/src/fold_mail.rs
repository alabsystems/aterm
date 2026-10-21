// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE HEURISTIC MAIL → TURN LINK (design §2.6, DESIGN:203-207; session log
//! phase 5, plan 5D-5; rulings T10, T36, T39): which delivered mail a turn
//! that named no cause most likely acted on, as the fold reads it from the
//! records alone.
//!
//! THE RULE ([`mail_link_of`], pure): a turn open that is not `by=human` and
//! names no cause (`cause=`, or `cause_k=` on the bus; at any confidence)
//! links to the one unlinked
//! `mail dir=in kind=task` waiting for it — delivered no more than
//! [`MAIL_LINK_WINDOW_MS`] before the turn by `t`, with no `human` mark
//! between the two (the fold forgets every waiting mail at a `human` mark;
//! since 5D-4 the person's first input after delivered mail that reached
//! the program writes one, T39, and a mark no such input came with says
//! `reached=0` and forgets nothing). With two or more waiting it links none
//! and counts the turn in `mail_amb=` (T10c): the rule never guesses
//! between two.
//!
//! A link is always [`Conf::Heuristic`], whatever the turn's own confidence,
//! and it is never written under `cause=`: an explicit cause outranks it
//! and is the turn's own field. One mail links one turn at most: a decided
//! turn takes the mails it weighed off the waiting list (the one it linked,
//! or the ambiguous ones), a person's turn takes them all (a person acted),
//! and an explicit `cause=@<off>` takes the mail it names.
//!
//! WHERE IT LIVES (T10a, T10b, T36): fold state, for the open turns and the
//! newest closed one, read by [`super::Fold::mail_link`]. It crosses a
//! seamless update in the local carry only, as `ml`-prefixed tokens on the
//! turn's own `fold open` / `fold last` line (`mloff=<off> mlconf=<conf>`),
//! which a reader that does not know them skips (the R13 skip rule). Since
//! Phase 6 (step 6C-5, ruling U19) the waiting mails and the mail latch cross
//! with it, on the totals line (`mlw=`, `mlh=`), so a task mail delivered
//! before an update still links the successor's next turn. One reader prints
//! the link: the local `brief`, as ` mail=@<off>~` on the turn's line, never
//! without the `~` (Phase 6 step
//! 6C-3, ruling U5). `log`, the bus and the ledger do not print it, and a
//! bus fold has none to print.

use crate::field::{Key, Val};
use crate::rec::Rec;
use crate::vocab::{Actor, Conf};

/// How long before a turn's open a mail may have been delivered and still be
/// the turn's (wall ms).
pub const MAIL_LINK_WINDOW_MS: u64 = 60_000;

/// The waiting mails a fold holds at most (the oldest gives way). Two in a
/// turn's window already link none.
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
    /// No link: none waiting in the window, or the turn is a person's or
    /// names its cause.
    None,
    /// The one waiting mail, by its offset.
    Link(u64),
    /// Two or more waiting in the window: none is linked.
    Ambiguous,
}

/// Whether `m` lies in the window of a turn opened at `turn_t`.
#[must_use]
pub const fn mail_in_window(m: &MailWaiting, turn_t: u64) -> bool {
    m.t <= turn_t && turn_t - m.t <= MAIL_LINK_WINDOW_MS
}

/// Whether the turn open `turn` declares what it acted on: `cause=`, or
/// on the bus (where the id stays on the host) its class `cause_k=`.
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

/// The waiting mails after the turn open `turn` decided `decided`: a
/// person's turn takes them all; an explicit cause the mail it names; a
/// decided turn every mail it weighed (the one it linked, or the ambiguous
/// ones); and every turn the mails past its window, which no later turn can
/// take.
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
