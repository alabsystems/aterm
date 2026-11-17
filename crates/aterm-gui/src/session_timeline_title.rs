// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A session's title is a claim. A person's rename, `meta set title`, `meta unset title` and
//! aterm's own pin are each a description claim of the session's `title` slot
//! ([`crate::session_log::SessionLog::claim_desc`]), kept beside every other author's; the
//! pinned title ([`super::SessionMeta::user_title`]) is what the last title write settled: the
//! claim then standing for the session, time-limited claims aside, when its author is a
//! person, the session's own program, its creator, a writer aterm ran or a control client,
//! and nothing otherwise. A title restored with the session is pinned with no claim, and
//! stays until a title write finds a claim standing or comes from an author whose claims can
//! stand.

use aterm_slog::{Actor, ClaimRefused, Described, Node, Rank, Src};

use super::{MetaChange, MetaField, MetaWriteError, sanitize_metadata_value};
use crate::session_log::{DescRefused, DescWrite, desc_stored};

/// The slot a title claims.
const TITLE_SLOT: &str = "title";

/// The title the session's claims pin: the text of the claim that stands for the session
/// itself (never one of an agent run, which lapses when the agent ends) once every
/// time-limited claim has run out, when its author ranks a control client or better, as the
/// pin stores it. A time-limited claim is never pinned, because the pin has no clock: nothing
/// would take the title back when the claim lapsed. So the claims are judged at the end of
/// time, where only those with no `ttl` are left to stand.
fn standing_title(desc: &Described) -> Option<String> {
    desc.shown_in(Node::SESSION, TITLE_SLOT, u64::MAX)
        .filter(|shown| shown.rank <= Rank::Ctl)
        .and_then(|shown| sanitize_metadata_value(TITLE_SLOT, &shown.claim.value))
}

/// A claim's refusal as a title write's.
const fn refusal(e: DescRefused) -> MetaWriteError {
    match e {
        DescRefused::Claim(ClaimRefused::Empty) => MetaWriteError::Empty,
        DescRefused::Claim(ClaimRefused::TooLong { max }) => {
            MetaWriteError::TooLongEncoded { cap: max }
        }
        DescRefused::Claim(ClaimRefused::Protocol) => MetaWriteError::ProtocolLine,
        DescRefused::Credential => MetaWriteError::Credential,
        DescRefused::Busy(retry_ms) => MetaWriteError::Busy { retry_ms },
        // A session's title names a fixed slot, no node and no `upto=`, and its writers are a
        // person, a control client or another session, which the rule refuses for no other
        // reason: of the rest only the text's formatting can be refused.
        DescRefused::Claim(_)
        | DescRefused::NoNode(_)
        | DescRefused::NotOpen(_)
        | DescRefused::NotAgent(_)
        | DescRefused::UptoAhead(_)
        | DescRefused::Log(_) => MetaWriteError::ForbiddenFormatting,
    }
}

/// WRITE THE TITLE (`value` validated; `None` clears) as `by`'s claim, under the metadata lock
/// then the timeline's. The claim is the session's, not its agent's, so a tab's title does
/// not lapse when an agent exits. A clear withdraws `by`'s own claim and nobody else's, unless
/// `by` is a person: a person's clear also removes every title a control client, a program or
/// another session put on the session ([`aterm_slog::Described::withdraws`]), so a person who
/// empties a rename field that held a title can take it off their own tab whoever set it. The
/// pinned title then becomes what stands ([`standing_title`]), and only when it moved is the
/// `meta` record written (the timeline row, `EVENT meta` and every `user_title=` reader hang
/// on that record) and `true` returned.
/// So a control client's title under a person's rename is recorded and listed, answers OK, and
/// changes nothing shown. Where no claim stands, the pinned title is cleared by an author
/// whose claims can stand and left alone by one whose claims never do (another session, the
/// bridge, another machine's driver).
///
/// A write that repeats `by`'s own claim (the same text, held with no time limit) or is a clear
/// that would remove no claim, and would leave the pinned title where it is, writes nothing and
/// returns `false`: what stands is already pinned, or nothing stands and `by`'s claims never
/// do, so the pin is kept whatever it is. A person's write is a claim like any other's, even of
/// the text already pinned: a rename field committed as it opened never reaches here
/// (`crate::App::commit_session_rename` judges that by what the field opened holding), so a
/// person's write is text the person changed.
/// Every author's write counts as a driver's, a repeat included (the restore graft keeps its
/// hands off the field). A log that takes no record (a closed session's) holds no claims to
/// rank, so there the title is set as given.
///
/// # Errors
/// A title over its slot's bound ([`aterm_slog::slot_bound`]) percent-encoded, one from anyone
/// but a person of which redaction leaves nothing but its notices (it is nothing but a
/// credential) or in which the harness's turn-end readers read a head
/// ([`MetaWriteError::ProtocolLine`]), and an author short of a mark token.
pub(super) fn apply_title_claim(
    ctx: &crate::SessionCtx,
    value: Option<String>,
    by: Actor,
) -> Result<bool, MetaWriteError> {
    let stored = value
        .as_deref()
        .map(|v| desc_stored(TITLE_SLOT, v, by, Src::Mark))
        .transpose()
        .map_err(refusal)?;
    let mut meta = ctx.meta.lock().unwrap_or_else(|p| p.into_inner());
    meta.driver_writes |= MetaField::Title.bit();
    let mut timeline = ctx.timeline.lock().unwrap_or_else(|p| p.into_inner());
    let at = timeline.slog().now();
    let (repeats, stands, listed) = {
        let desc = &timeline.slog().log().state().desc;
        let own = desc.claim_of(Node::SESSION, TITLE_SLOT, by, Src::Mark, None);
        let repeats = match stored.as_deref() {
            // A clear repeats where it would remove nothing: `by` holds no claim, and, for a
            // person, neither does anyone whose title a person's clear takes off.
            None => !desc.withdraws(Node::SESSION, TITLE_SLOT, by, Src::Mark, None),
            // The claim this write leaves has no time limit, so one `by` holds with a `ttl` (a
            // `mark`'s) is not repeated by it, whatever its text.
            Some(text) => {
                own.is_some_and(|claim| claim.value.as_str() == text && claim.live(u64::MAX))
            }
        };
        (
            repeats,
            standing_title(desc),
            // An author's rank hangs on who the session is, which no claim changes: it is
            // the same after the write below.
            !aterm_slog::rank_of(by, Src::Mark, desc.ident()).stands(),
        )
    };
    // A repeat changes no claim, so what stands after it is what stands now: the pin moves
    // only where that is not yet pinned, and a listed author keeps the pin where none stands.
    if repeats && (stands == meta.user_title || (listed && stands.is_none())) {
        return Ok(false);
    }
    let write = DescWrite::of_session(TITLE_SLOT, value.as_deref());
    let title = match timeline.slog_mut().claim_desc(&write, by, at) {
        Ok(_) => {
            let desc = &timeline.slog().log().state().desc;
            // An author whose claims never stand moves nothing shown: where no claim stands,
            // a pin nobody claims (a restored one) stays, as it would under a standing claim.
            standing_title(desc).or_else(|| listed.then(|| meta.user_title.clone()).flatten())
        }
        Err(DescRefused::Log(_)) => stored,
        Err(e) => return Err(refusal(e)),
    };
    let moved = meta.set(TITLE_SLOT, title).unwrap_or(false);
    if moved {
        let change = MetaChange::written_by(MetaField::Title.wire_name(), meta.get(TITLE_SLOT), by);
        timeline.record_meta_change(change, &meta);
    } else {
        // The claim alone moved the log: its readers are owed the wake a `meta` record's
        // ring would have been.
        timeline.slog_take_wake();
    }
    Ok(moved)
}

#[cfg(test)]
#[path = "session_timeline_title_tests.rs"]
mod tests;
