// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The process-wide session registry: resolves every live session by its stable [`SessionId`]
//! and by the process-local `u64` id `Wake`s carry, beside the GUI's pane model.
//!
//! It lives here rather than in `aterm-session` because a [`SessionHandle`] holds a `Terminal`
//! (`aterm-core`) and a `SessionCtx` (`aterm-gui`), neither of which `aterm-session` depends on;
//! the identity (`SessionId`/`LaunchNonce`) and authority (`EdgeTable`/`decide_edge`) types are
//! still `aterm-session`'s.
//!
//! Lock rule: a resolver clones the `(term, sink, sid, nonce, …)` tuple out of the store and
//! drops the store guard before locking the target `Terminal`, as `resolve_active` does. The
//! store lock is never held across a `Terminal` lock, so two agents driving each other cannot
//! deadlock on it.

use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex, RwLock};

use aterm_core::terminal::{RenderDebt, Terminal};
use aterm_session::{LaunchNonce, SessionId};
use aterm_types::control_request::Selector;

use crate::SessionCtx;

/// A session's lifecycle as the registry sees it. A handle is registered `Spawning` the instant
/// its PTY and engine exist, and its PTY reader flips it to `Alive` (`Wake::Ready`) on its first
/// live iteration. An `Exited` session stays readable until its pane is torn down.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum SessionState {
    /// Registered with engine and PTY live; the reader has not confirmed its first iteration.
    /// Input is safe (the PTY master and sink exist; bytes buffer in the kernel).
    Spawning,
    /// Live: a reader thread is feeding the engine.
    Alive,
    /// The command exited; the engine is still readable until the pane closes.
    Exited,
}

impl SessionState {
    /// The stable wire token for the `sessions` verb.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            SessionState::Spawning => "spawning",
            SessionState::Alive => "alive",
            SessionState::Exited => "exited",
        }
    }

    /// Inverse of [`Self::as_str`] (unknown ⇒ `Spawning`), for tests that round-trip the
    /// manifest's spellings; `take_incoming` re-derives live state instead of trusting them.
    #[cfg(test)]
    #[must_use]
    pub(crate) fn from_str(s: &str) -> Self {
        match s {
            "alive" => SessionState::Alive,
            "exited" => SessionState::Exited,
            _ => SessionState::Spawning,
        }
    }
}

/// One session's round-trippable metadata: the projection of a [`SessionHandle`] that survives
/// a seamless re-exec (the live `Arc`s and the `master` fd are re-established, not serialized).
/// See [`SessionHandoff`].
///
/// Every field after `title` is additive: absent reads as its default, and a default value is
/// not written, so an older reader sees the wire it always saw.
#[derive(Clone, PartialEq, Eq, Debug, serde::Serialize, serde::Deserialize)]
pub(crate) struct SessionRecord {
    pub local_id: u64,
    pub sid: String,
    pub parent: Option<String>,
    pub state: String,
    pub title: String,
    /// The session's engine screen at handoff time, so the successor repaints the pre-update
    /// content instead of booting blank over a live shell.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub screen: Option<ScreenCarry>,
    /// A sanitized projection of the operator-set metadata (`meta set …`), for manifest
    /// inspection only: adoption re-seeds user meta from the layout sidecar's restore leaves
    /// (`carry_restored_identity`), and these fields are kept in lockstep with that carrier.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub user_title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub icon: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub role: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attention: Option<String>,
    /// The `meta set questions` policy word: inspection-only, like the five above.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub questions: Option<String>,
    /// The `meta set autopilot` word: inspection-only, like `questions`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub autopilot: Option<String>,
    /// The instant a timed Cruise lands, unix ms (2026-10-16; additive,
    /// absent tolerated, not written when unset): in lockstep with the leaf
    /// carrier's `land_at`, beside `autopilot`, for the same inspection-only
    /// reason (`docs/DESIGN-harness-autopilot-2026-10-15.md` §9.3.6, item 2).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub land_at: Option<i64>,
    /// CONTROL CARRY (additive, absent tolerated; round 10): `"<len>
    /// <sha256hex>"` of this session's `seamless-<pid>-<nonce>.s<id>.ctl`
    /// sidecar — the turn ledger and the tail of the alt-screen archive
    /// (`crate::handoff_carry`). A plain string so an older reader skips it
    /// like any unknown key. Best-effort by design: the sidecar is NOT in
    /// either adoption-proof digest, a missing or mismatched one is ignored,
    /// and nothing about it can refuse the adoption.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub control: Option<String>,
    /// This session's shell was spawned by a build that does not put `<prefix>/agents/` in front
    /// of PATH, so its `claude`/`codex` are foreign copies. Per record so the fact survives a
    /// second handoff, where [`SessionHandoff::outgoing_build`] alone would count it as covered.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub frozen_path: bool,
    /// This session's `topic add` set, one `"<topic> <since>"` row each (`fabric::render_topics`).
    /// A topic set is consent (the only way a `post to=say:<topic>` reaches the session), so it
    /// rides every handoff; each row is re-validated on the way in (`fabric::parse_topics`), so a
    /// manifest cannot invent an opt-in.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub topics: Vec<String>,
    /// The process group the outgoing PTY reader last saw holding this session's terminal
    /// (`Session::fg_holder`, final once the readers are parked). The adopting reader starts
    /// its foreground cutter from it, so a job that died during the handoff is still a
    /// foreground edge at the first sample and its alt screen, mouse and kitty modes are handed
    /// back. On the record because the control sidecar can be dropped (the Sanitized or
    /// Repaint rung, a repaint, the 16 MiB aggregate budget). In neither proof digest.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fg_holder: Option<i32>,
    /// This session's shell was spawned with `ATERM_REKEY_PATH`, checked at every prompt
    /// (`shell_rekey`): a successor that adopts it without its nonce re-keys it through that file
    /// instead of leaving it `integration=degraded`. Per record, like `frozen_path`.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub rekey: bool,
    /// This session's shell runs an integration loader that reads a body pointer at every prompt
    /// (`shell_body`): spawned with `ATERM_INTEGRATION_POINTER`, or upgraded to one. A successor
    /// points it at its own body; a shell adopted without it and with no carried revision runs a
    /// pre-loader script (`status integration_rev=frozen`).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub loader: bool,
    /// `"<len> <sha256hex> <take>"` of this session's `seamless-<pid>-<nonce>.s<id>.hist`
    /// sidecar: the scrollback older than the screen carry, whose first `take` lines the
    /// successor imports after Commit (`crate::handoff_history`). In neither proof digest, like
    /// `control`: a missing or mismatched sidecar costs its `take` lines (counted), never the
    /// adoption.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub history: Option<String>,
    /// History lines this handoff's outgoing process could not carry (its history changed since
    /// the export, more output arrived than the screen carry holds, or it was deeper than the
    /// export bound): what the successor names on the band for this update.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub history_dropped: u64,
    /// `history_dropped` was withheld by choice: the successor's signed handoff policy carries no
    /// scrollback (`carry = "visible"` or `"repaint"`, `handoff_history::Fallback::Withheld`).
    /// Only the producer knows which it was; without it the successor's row would report the
    /// policy's withholding as a failure. In neither proof digest.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub history_withheld: bool,
    /// The session's running count of history lines its handoffs could not carry, this one's
    /// included: `status history_lost=`. Per record, so it survives every later handoff.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub history_lost: u64,
    /// `"<origin> <reason>"` (`fabric::render_hold`) of this session's first standing hold: the
    /// fleet's (`fabric-lost` included) when it stands, else the owner's `local` one; when both
    /// stand, the other rides [`Self::more_holds`]. Losing a hold would free a halted agent, so
    /// the successor re-seeds the holds before the session is registered
    /// (`SessionFabric::seed_holds`). In neither proof digest: the successor re-validates the
    /// grammar and fails closed (`fabric::parse_hold`), and a value of the wrong type is an
    /// unreadable row ([`lenient_hold`]), so a bad value can over-halt one session, never lift a
    /// halt or cost the update.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "lenient_hold"
    )]
    pub hold: Option<String>,
    /// One `"<origin> <reason>"` row (`fabric::render_holds`) per standing hold past
    /// [`Self::hold`]: a session can hold the fleet's halt beside the owner's own stop, and each
    /// comes back in its own slot, so lifting one never frees the other. Beside `hold` rather
    /// than in it, so a reader without this key (a rollback) still reads the stronger halt where
    /// it always did. Read fail-closed ([`lenient_hold_rows`]): a member that is not a string, or
    /// a value that is no list, is an unreadable row, which halts; never a manifest error.
    #[serde(
        default,
        skip_serializing_if = "Vec::is_empty",
        deserialize_with = "lenient_hold_rows"
    )]
    pub more_holds: Vec<String>,
    /// `"<holder> <remaining_ms>"` of a live `meta set supervisor <holder> ttl=` claim held by
    /// anyone but this process's own in-GUI host (`SessionMeta::handoff_supervisor`). The
    /// successor seeds the lease before the session is registered
    /// (`session_timeline::seed_carried_claims`), so the holder's next renewal renews it and the
    /// successor's host parks behind it rather than taking the session at Commit. A row that does
    /// not read seeds nothing; the host then waits its grace ([`Self::claim_grace`]).
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "lenient_row"
    )]
    pub supervisor: Option<String>,
    /// `supervisor` above is this session's whole claim state: no claim but the producer's own
    /// host's, or a lease carried there. `false` when it was claimed through a connection, which
    /// cannot cross (its holder claims again), and by absence from older producers. See
    /// [`Self::claim_grace`].
    #[serde(
        default,
        skip_serializing_if = "std::ops::Not::not",
        deserialize_with = "lenient_flag"
    )]
    pub claim_known: bool,
    /// THE KEYED ESCALATIONS (additive, absent ⇒ none; the round-four plan,
    /// item 9): each attention owner's line, `"<owner> [kind=<ask|wait>]
    /// <pct-text>"`, in write order (`SessionMeta::handoff_attention_owners`;
    /// the kind only where its writer named one, so a row of a kind-less
    /// entry is what every producer before the kind wrote). The restore leaf
    /// carries the bare entry alone, so an update used to erase every
    /// supervisor's "needs you" from the menu bar with nobody having
    /// answered it. Re-validated row by row on the way in
    /// (`session_timeline::parse_carried_attention`); a row that does not
    /// read is dropped, never the manifest.
    #[serde(
        default,
        skip_serializing_if = "Vec::is_empty",
        deserialize_with = "lenient_rows"
    )]
    pub attention_owners: Vec<String>,
    /// How many lines above the live bottom the view was scrolled at the park (`display_offset`),
    /// written only when scrolled back (`handoff_history::capture_head`, `stamp_manifest`). The
    /// successor scrolls back to it, clamped, once the import has put the lines back, unless the
    /// view moved in between (`handoff_history::run_imports`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub viewport_from_bottom: Option<u32>,
    /// THE RENDER DEBT (additive, absent ⇒ none; the render stream, R08):
    /// `"<shift>,<pushed>"` — the rows the ENGINE owed this session's alternate
    /// screen when it was parked (`Terminal::render_debt`), written only for a
    /// session on the alternate screen whose debt is not zero and whose screen
    /// is carried (not blank on purpose). A program that misses a resize of its
    /// window is left drawing one row off, and the update must not launder
    /// that: the successor seeds its engine with the debt
    /// (`Terminal::seed_render_debt`), and the SAME epoch judge and repaint
    /// nudge that would have judged the tab in the parent judge it. Absent
    /// means "owes no rows, or the producer predates the field" and nothing
    /// happens: the debt is carried, never a verdict about the program. In
    /// neither proof digest; a value that does not read (`lenient_row`) seeds
    /// nothing and never costs the manifest. An older reader skips the key.
    /// The columns owed ride [`Self::render_debt_cut`]; both are read and
    /// written together ([`Self::carried_render_debt`],
    /// [`Self::carry_render_debt`]).
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "lenient_row"
    )]
    pub render_debt: Option<String>,
    /// THE COLUMNS OF THE RENDER DEBT (additive, absent ⇒ 0; S2R-F3): the
    /// drawn columns a narrowing with no rewrap cut off this session's
    /// alternate screen (`RenderDebt::cut`), carried as [`Self::render_debt`]
    /// is and judged with it. A key of its own, so a reader older than the
    /// column judge skips it and still reads the rows [`Self::render_debt`]
    /// owes, which one row with both would have read as no debt at all. `0` is
    /// not written; a value that is not a count ([`lenient_count`]) is 0.
    #[serde(
        default,
        skip_serializing_if = "is_zero",
        deserialize_with = "lenient_count"
    )]
    pub render_debt_cut: u64,
    /// THE MAILBOX'S OUTBOUND HALF AND ITS ID COUNTERS (additive, absent ⇒
    /// none; round seven of the update audit, findings 6, 8 and 28): the posts
    /// still waiting for a bridge, the receipts still owed, and the last inbox,
    /// post and receipt ids this session handed out
    /// (`crate::fabric::FabricCarry`). The successor puts them back before the
    /// session is registered (`SessionFabric::seed_carry`), so nothing a sender
    /// was told is queued dies with the old process, and no id is handed out
    /// twice under the sid the update keeps. Per record, like `topics`, so it
    /// survives every later handoff; in neither proof digest; not written when
    /// empty, so an older reader sees the wire it always saw, and a value that
    /// does not read is dropped ([`lenient_fabric`]), never the manifest.
    #[serde(
        default,
        skip_serializing_if = "crate::fabric::FabricCarry::is_empty",
        deserialize_with = "lenient_fabric"
    )]
    pub fabric: crate::fabric::FabricCarry,
    /// The newest `timeline` event id this session recorded. The successor counts on above it
    /// (`SessionTimeline::continue_ids`), so a `timeline since=<id>` anchor taken before the
    /// update still sees the `handoff` row and everything after.
    #[serde(
        default,
        skip_serializing_if = "is_zero",
        deserialize_with = "lenient_count"
    )]
    pub timeline_id: u64,
    /// `"<pid>:<id>"`: the window pid and pool id by which the crash journal image the outgoing
    /// process held names this shell's pane, written only when that image is not the outgoing
    /// process's own (it took its parent's over at Commit and never replaced it). The successor's
    /// keeper registration names it beside the parent's pair (`keeper_link::Tag::keyed`), so a
    /// successor that dies before its first image still gets its kept shells back. A value that
    /// does not read is none ([`parse_journal_key`]).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub journal_key: Option<String>,
    /// THE TASK RECORD (additive, absent ⇒ none; the worker pillar,
    /// 2026-10-15): the session's [`crate::task_record::TaskRecord`] as one
    /// row (`TaskRecord::carry_row`: `rev= state= from= since=<unix ms>
    /// result_bytes= attention=` and the optional facts). A manager parked
    /// in `await task` across a self-update re-arms with the revs it was
    /// answered with, so the successor must hold the same record — rev,
    /// state and the wall-clock instant the state was entered — or a worker
    /// that finished during the update would read as never tasked. The
    /// successor seeds it before the session is registered
    /// (`task_record::seed_carried`), re-validated as `task mark` validates;
    /// a row that does not read seeds nothing (`lenient_row`), never costs
    /// the manifest. Per record, like `topics`, so it survives every later
    /// handoff; in neither proof digest; `None` is not written, so an older
    /// reader sees the wire it always saw.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "lenient_row"
    )]
    pub task: Option<String>,
    /// THE SESSION LOG'S EPOCH AND NODE-ID COUNTERS (additive, absent ⇒ none;
    /// the session log's slice 1, 2026-10-15): `"<ep> <run> <turn> <step>"`
    /// (`session_log::LogCarry`), the outgoing log's epoch and its last run,
    /// turn and step ids plus their overlap reserve
    /// (`session_log::LOG_ID_RESERVE`). The successor's log starts at the next
    /// epoch with `gap why=handoff` and continues the ids, so `t17` names one
    /// turn for as long as the session lives; a process that issues past the
    /// reserve refuses its Commit (`SessionTimeline::past_handoff_ceiling`).
    /// No records ride here (slice 1 carries none). A plain string an older
    /// reader skips like any unknown key; a value that does not read
    /// ([`lenient_row`], `LogCarry::parse`) carries nothing, and the successor
    /// starts at the first epoch, never failing the manifest. In neither proof
    /// digest.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "lenient_row"
    )]
    pub log_ids: Option<String>,
    /// WHAT THE OWNER WAS TOLD AND HAS LOOKED AT (additive, absent ⇒
    /// nothing; `docs/DESIGN-owner-surfaces-2026-10-16.md` §3): the status
    /// item's memory of this session as one row
    /// (`status_item::StatusCarry::row`: `need=<word>@<unix s>`, `told=1`,
    /// `hold=<word>@<unix s>`, `done=<unix ms>`), stamped by the park
    /// (`App::status_carry_rows`). Before this carry everything that said
    /// "already told" or "already seen" lived only in the process, so every
    /// seamless update posted each standing need's desktop notice again,
    /// brought back every Done for you row the owner had read, and restarted
    /// every age. The successor reads it at each adopted session's first
    /// publication (`App::adopt_status_carry`). A plain string an older
    /// reader skips; a value that does not read ([`lenient_row`],
    /// `StatusCarry::parse`) carries nothing — at worst a notice is told
    /// again — never failing the manifest. In neither proof digest.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "lenient_row"
    )]
    pub status_seen: Option<String>,
    /// THE AUTOPILOT'S END AND THE TASK'S WORK (additive, absent ⇒ none;
    /// `docs/DESIGN-harness-autopilot-2026-10-15.md` §11.2.3, item 8, slice
    /// 2b): the end of the session's supervised task standing on the glass —
    /// a landing, the wall time its face moved there, whether anyone looked
    /// at it — and the agent's busy time since the task opened, as one row
    /// (`harness_end_carry::EndCarry::row`: `end=landed@<unix ms>
    /// seen=<0|1> worked=<ms>`, or `end=- worked=<ms>` with no end), stamped
    /// by the park (`App::end_carry_rows`). The successor seeds the face's
    /// time onto the timeline before registration
    /// (`spawn::seed_adopted_end`), so the loop's restated face reads
    /// `landed 17:42`, not the update's clock; and spends the row at the
    /// session's first publication (`App::adopt_end_carry`): the tab's `•`
    /// stands until someone looks, and a task 25 s into its work needs only
    /// 5 s more for its end. The handoff's alone, as `lost` is: never on a
    /// durable layout, so a crash image never has it. A plain string an older
    /// reader skips; a value that does not read ([`lenient_row`],
    /// `EndCarry::parse`) carries nothing — the end is lost and the clock
    /// reads the restate time, as before the carry — never failing the
    /// manifest. In neither proof digest.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "lenient_row"
    )]
    pub autopilot_end: Option<String>,
    /// THE SESSION LOG'S SEQ COUNT (additive, absent ⇒ 0; Phase 2A of the
    /// session log, design §2.2): the outgoing log's last seq plus its overlap
    /// reserve (`session_log::LOG_SEQ_RESERVE`). The successor's log starts
    /// above it, so a seq names one record for as long as the session lives;
    /// a process that appends past the reserve refuses its Commit
    /// (`SessionTimeline::past_handoff_ceiling`). A separate count from
    /// `timeline_id`: the timeline's `events-dropped=` arithmetic assumes its
    /// own ids have no holes. `0` is not written; an older reader skips the
    /// key, and a value that does not read ([`lenient_count`]) is none, never
    /// the manifest. In neither proof digest.
    #[serde(
        default,
        skip_serializing_if = "is_zero",
        deserialize_with = "lenient_count"
    )]
    pub log_seq: u64,
}

/// The wire spelling of a crash journal key (`<pid>:<id>`,
/// [`SessionRecord::journal_key`]).
#[cfg(any(unix, test))]
pub(crate) fn journal_key_row((pid, id): (u32, u64)) -> String {
    format!("{pid}:{id}")
}

/// Read a carried crash journal key: a window pid that is a process (never
/// `0`) and a pool id, or none.
pub(crate) fn parse_journal_key(row: &str) -> Option<(u32, u64)> {
    let (pid, id) = row.split_once(':')?;
    Some((
        pid.parse::<u32>().ok().filter(|pid| *pid != 0)?,
        id.parse::<u64>().ok()?,
    ))
}

/// The keys a carried render debt rides ([`SessionRecord::render_debt`],
/// [`SessionRecord::render_debt_cut`]): its rows as `<shift>,<pushed>`, only
/// when it owes rows, and its columns. None, or a debt of nothing, is neither.
#[cfg(any(unix, test))]
pub(crate) fn render_debt_keys(debt: Option<RenderDebt>) -> (Option<String>, u64) {
    let RenderDebt { shift, pushed, cut } = debt.filter(|debt| debt.owed()).unwrap_or_default();
    (
        (shift != 0 || pushed != 0).then(|| format!("{shift},{pushed}")),
        u64::from(cut),
    )
}

/// Read a carried render debt: the rows `row` owes (`<shift>,<pushed>`, absent
/// for none) and the columns `cut` owes. A row that is not two integers, a
/// value past a screen, or a debt of nothing is no debt: the successor owes
/// the program nothing it was not told.
pub(crate) fn parse_render_debt(row: Option<&str>, cut: u64) -> Option<RenderDebt> {
    let (shift, pushed) = match row {
        Some(row) => {
            let (shift, pushed) = row.split_once(',')?;
            (
                shift.trim().parse::<i32>().ok()?,
                pushed.trim().parse::<i32>().ok()?,
            )
        }
        None => (0, 0),
    };
    let debt = RenderDebt {
        shift,
        pushed,
        cut: u16::try_from(cut).ok()?,
    };
    // The engine's debt is a handful of rows and columns; a value past a screen
    // is a bad row, not a debt (`seed_render_debt` would take any value).
    (debt.owed()
        && shift.unsigned_abs() <= 4096
        && pushed.unsigned_abs() <= 4096
        && debt.cut <= 4096)
        .then_some(debt)
}

impl SessionRecord {
    /// Carry `debt` on this record: its rows as [`Self::render_debt`]
    /// (`<shift>,<pushed>`, only when it owes rows), its columns as
    /// [`Self::render_debt_cut`]. None, or a debt of nothing, carries neither.
    #[cfg(unix)]
    pub(crate) fn carry_render_debt(&mut self, debt: Option<RenderDebt>) {
        (self.render_debt, self.render_debt_cut) = render_debt_keys(debt);
    }

    /// The render debt this record carries ([`parse_render_debt`] of
    /// [`Self::render_debt`] and [`Self::render_debt_cut`]).
    pub(crate) fn carried_render_debt(&self) -> Option<RenderDebt> {
        parse_render_debt(self.render_debt.as_deref(), self.render_debt_cut)
    }

    /// Every hold row this record carries, the first one first: [`Self::hold`]
    /// and then [`Self::more_holds`]. A record from before the two slots
    /// carries at most the one, which reads as exactly that hold.
    pub(crate) fn hold_rows(&self) -> Vec<String> {
        self.hold.iter().chain(&self.more_holds).cloned().collect()
    }

    /// Whether the successor's in-GUI host must hold off this adopted session for its
    /// adopted-claim grace (`harness_host::ADOPTED_CLAIM_GRACE`) before claiming it: unless the
    /// producer vouched that nobody else held it ([`Self::claim_known`] with no carried
    /// [`Self::supervisor`]).
    ///
    /// * An older producer says nothing about claims, so an external lease was dropped: the grace
    ///   lets its next renewal claim first.
    /// * A connection-bound claim could not cross: its holder claims again.
    /// * A carried lease is seeded, but a holder that renewed after the park carries a remainder
    ///   older than its lease, and a short `ttl=` may lapse before its renewal arrives.
    #[must_use]
    pub(crate) fn claim_grace(&self) -> bool {
        !self.claim_known || self.supervisor.is_some()
    }
}

/// Read a carried mailbox ([`SessionRecord::fabric`]); a value that does not
/// read as one is none at all, never an error that would fail the whole
/// manifest and adopt nothing.
fn lenient_fabric<'de, D>(deserializer: D) -> Result<crate::fabric::FabricCarry, D::Error>
where
    D: serde::Deserializer<'de>,
{
    #[derive(serde::Deserialize)]
    #[serde(untagged)]
    enum Carry {
        Read(crate::fabric::FabricCarry),
        Unreadable(serde::de::IgnoredAny),
    }
    Ok(
        match <Carry as serde::Deserialize>::deserialize(deserializer)? {
            Carry::Read(carry) => carry,
            Carry::Unreadable(_) => crate::fabric::FabricCarry::default(),
        },
    )
}

/// Read a carried count; anything but a non-negative integer reads as `0`.
fn lenient_count<'de, D>(deserializer: D) -> Result<u64, D::Error>
where
    D: serde::Deserializer<'de>,
{
    #[derive(serde::Deserialize)]
    #[serde(untagged)]
    enum Count {
        Read(u64),
        Unreadable(serde::de::IgnoredAny),
    }
    Ok(
        match <Count as serde::Deserialize>::deserialize(deserializer)? {
            Count::Read(n) => n,
            Count::Unreadable(_) => 0,
        },
    )
}

/// Read a carried one-line row; a value that is not a string reads as an
/// unreadable row (`Some("")`, which its parser refuses), never an error that
/// would fail the whole manifest and adopt nothing.
fn lenient_row<'de, D>(deserializer: D) -> Result<Option<String>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    lenient_hold(deserializer)
}

/// Read a carried list of rows, keeping the strings and dropping any element
/// of another type — or the whole value when it is no list at all.
fn lenient_rows<'de, D>(deserializer: D) -> Result<Vec<String>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    #[derive(serde::Deserialize)]
    #[serde(untagged)]
    enum Row {
        Text(String),
        Unreadable(serde::de::IgnoredAny),
    }
    #[derive(serde::Deserialize)]
    #[serde(untagged)]
    enum Rows {
        List(Vec<Row>),
        Unreadable(serde::de::IgnoredAny),
    }
    Ok(
        match <Rows as serde::Deserialize>::deserialize(deserializer)? {
            Rows::List(rows) => rows
                .into_iter()
                .filter_map(|row| match row {
                    Row::Text(text) => Some(text),
                    Row::Unreadable(_) => None,
                })
                .collect(),
            Rows::Unreadable(_) => Vec::new(),
        },
    )
}

/// Read a carried flag; anything but a boolean reads as `false` — the
/// conservative reading for [`SessionRecord::claim_known`] (the successor's
/// host then waits its grace) — never an error that fails the manifest.
fn lenient_flag<'de, D>(deserializer: D) -> Result<bool, D::Error>
where
    D: serde::Deserializer<'de>,
{
    #[derive(serde::Deserialize)]
    #[serde(untagged)]
    enum Flag {
        Bool(bool),
        Unreadable(serde::de::IgnoredAny),
    }
    Ok(matches!(
        <Flag as serde::Deserialize>::deserialize(deserializer)?,
        Flag::Bool(true)
    ))
}

/// Read a carried hold row; a value that is not a string is an unreadable row (`Some("")`,
/// which `fabric::parse_hold` fails closed on), never an error: a manifest that does not parse
/// (`SessionHandoff::from_toml`) adopts nothing, and the update would be refused for a halt it
/// could have kept.
fn lenient_hold<'de, D>(deserializer: D) -> Result<Option<String>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    #[derive(serde::Deserialize)]
    #[serde(untagged)]
    enum Row {
        Text(String),
        Unreadable(serde::de::IgnoredAny),
    }
    Ok(Some(
        match <Row as serde::Deserialize>::deserialize(deserializer)? {
            Row::Text(row) => row,
            Row::Unreadable(_) => String::new(),
        },
    ))
}

/// Read carried hold rows ([`SessionRecord::more_holds`]) fail-closed: a member that is not a
/// string is an unreadable row (`""`, which `fabric::parse_hold` halts on), not dropped as
/// [`lenient_rows`] drops one, since dropping a hold row lifts a halt; a value that is no list is
/// one such row. Never an error, for [`lenient_hold`]'s reason.
fn lenient_hold_rows<'de, D>(deserializer: D) -> Result<Vec<String>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    #[derive(serde::Deserialize)]
    #[serde(untagged)]
    enum Row {
        Text(String),
        Unreadable(serde::de::IgnoredAny),
    }
    #[derive(serde::Deserialize)]
    #[serde(untagged)]
    enum Rows {
        List(Vec<Row>),
        Unreadable(serde::de::IgnoredAny),
    }
    Ok(
        match <Rows as serde::Deserialize>::deserialize(deserializer)? {
            Rows::List(rows) => rows
                .into_iter()
                .map(|row| match row {
                    Row::Text(text) => text,
                    Row::Unreadable(_) => String::new(),
                })
                .collect(),
            Rows::Unreadable(_) => vec![String::new()],
        },
    )
}

/// `skip_serializing_if` for a count whose absence reads as zero.
#[allow(
    clippy::trivially_copy_pass_by_ref,
    reason = "serde's skip_serializing_if hands the field by reference"
)]
fn is_zero(count: &u64) -> bool {
    *count == 0
}

/// Whether a manifest written by `outgoing_build` hands off sessions whose PATH is frozen
/// without the managed `<prefix>/agents/` in front (a running shell never learns of an
/// `agents/` created after it started). Exactly the builds that put it in front write
/// [`SessionHandoff::outgoing_build`], so absence means the old build; the number is never
/// compared (a dev build stamps HEAD's committer epoch). A shell such a build itself adopted
/// frozen rides [`SessionRecord::frozen_path`].
#[must_use]
pub(crate) fn predates_path_self_heal(outgoing_build: Option<u64>) -> bool {
    outgoing_build.is_none()
}

/// The screen half of the seamless handoff: the checkpoint's scalar projection rides the TOML
/// manifest; the grid blobs (the binary `Line` codec scrollback offload uses) ride
/// nonce-stamped sidecar files beside it. The reader requires the declared schema and every
/// semantics-bearing meta key.
///
/// A carry that is missing, names a sidecar other than its own, or breaks a resource bound
/// (a sidecar's or the aggregate byte cap) rejects the whole adoption before ownership
/// transfers. A carry whose content this build refuses (a meta that does not parse or breaks a
/// bound, dimensions it will not admit, a non-canonical grid, alt presence that disagrees with
/// the meta) costs only that session's screen: its shell is adopted onto a blank engine and
/// repainted, and the adoption proof is still taken over the bytes as read.
#[derive(Clone, PartialEq, Eq, Debug, serde::Serialize, serde::Deserialize)]
pub(crate) struct ScreenCarry {
    /// Must equal [`Self::SCHEMA`]; any other value (including a defaulted 0)
    /// refuses the screen.
    #[serde(default)]
    pub schema: u32,
    /// The checkpoint minus its grid blobs (`aterm_core::terminal::CheckpointMeta`) as a JSON
    /// string: TOML cannot represent its `None` fields, and the reader (always the newer
    /// build) ignores unknown fields and defaults missing ones.
    pub meta: String,
    /// Sidecar file holding the main grid's `serialize_lines` bytes.
    pub grid_file: String,
    /// Sidecar file holding the alt grid's bytes, when an alt grid existed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub alt_grid_file: Option<String>,
    /// The producer could not carry this session's real screen and sent a canonical blank one,
    /// so the successor must make the program redraw (the PTY size pulse an alt-screen adoption
    /// gets, `spawn_session`); the consumer sets it too when it blanked the screen itself. Not
    /// written when false, and no digest covers it (`screen_wire_digest` hashes the local id,
    /// the meta string and the grid bytes).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub repaint: bool,
}

impl ScreenCarry {
    pub(crate) const SCHEMA: u32 = 1;
}

/// Window-frame carry: the outgoing window's grid size and outer position, so the post-update
/// window reappears where and as big as the old one was.
#[derive(Clone, PartialEq, Eq, Debug, serde::Serialize, serde::Deserialize)]
pub(crate) struct WindowCarry {
    /// Grid rows of the handed-off session's window.
    pub rows: u16,
    /// Grid cols.
    pub cols: u16,
    /// Outer window position (physical px), when known.
    #[serde(default)]
    pub outer_x: Option<i32>,
    #[serde(default)]
    pub outer_y: Option<i32>,
    /// The window's raw inner size, `[width, height]` in physical px (`WindowState::win_px`);
    /// absent reads as the exact frame of `rows`×`cols`. A font zoom re-grids without resizing
    /// the window, so a zoomed window can be up to one cell per axis larger than its grid's
    /// frame. The successor takes the size only when it holds exactly the carried grid at its
    /// own metrics (`App::attach_frame_px`). No digest covers it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub inner_px: Option<[u32; 2]>,
    /// THE WINDOW'S PRESENCE ROW (additive, absent ⇒ none): whether the
    /// outgoing window had its presence row committed under its chrome
    /// (`WindowState::presence`, the agent's status line), which its grid and
    /// `inner_px` hold. Per window, unlike the band's shared rows below, and
    /// the successor's presence rows are frozen until Commit, so it reserves
    /// the row before its first attach sizes the window
    /// (`WindowState::take_carried_frame`): without it the carried size read a
    /// cell taller than the frame it measured and was refused, and the
    /// Commit's lift then re-gridded the window a row shorter than its
    /// parent's. Written only while a row stands; no digest covers it.
    /// RETIRED with the row (owner surfaces, 2026-10-16): this build never
    /// sets it and reads it nowhere; it stays on the wire for an older
    /// successor.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub presence_row: bool,
    /// The message band rows the outgoing window had committed above its grid
    /// (`App::message_band_rows`), each row's message in `messages`. The successor reserves
    /// them before it sizes its first window, which is one row taller per row, and paints the
    /// carried words in them until Commit. The serde name is the old status bars': both mean
    /// "committed chrome rows above the grid". Absent: no rows.
    #[serde(default)]
    pub status_bar_rows: u16,
    /// The center's live rows at handoff time, glass rows first
    /// (`aterm_messages::MessageCenter::carried`).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub messages: Vec<CarriedMessage>,
    /// The parent's next message id, so the successor's ids continue past every carried one.
    /// No `messages.log` lines cross: the parent flushes its own record before it execs and the
    /// successor's begins at Commit.
    #[serde(default)]
    pub next_message_id: u64,
    /// When the outgoing process finished downloading the build this handoff installs and it
    /// passed its checks (wall clock, Unix ms; `update_words::verified_unix_ms`), so the
    /// successor can say how long the update took. Absent when the outgoing process did not
    /// download that build itself.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub update_verified_unix_ms: Option<u64>,
    /// THE LIVE FONT ZOOM (additive, absent ⇒ none; the round-four plan, item
    /// 15): the glyph size the outgoing process drew at, in thousandths of a
    /// DEVICE px, when a person had zoomed it (Cmd-= / Cmd--) away from its
    /// reset size. Written with [`Self::font_reset_px_milli`] as ONE pair, and
    /// only while zoomed, so an unzoomed window's carry is the wire it always
    /// was. A build that has one font-size unit writes the zoom in px at 1×
    /// ([`Self::font_size_milli`]) and keeps this device pair for an older
    /// successor; a successor reads the px-at-1× pair first and turns a lone
    /// device pair into sizes with its first window's scale.
    ///
    /// Why: `rows`/`cols` above are the zoomed grid. A successor that sized
    /// them at the config's font drew a smaller (or larger) window with
    /// smaller text at every update, and the person zoomed again. With the
    /// pair, the first window comes up at the same px, so text and frame stay
    /// put. Integers because `WindowCarry` is `Eq` and TOML has no NaN to
    /// refuse; the successor admits the pair only inside the zoom's own bounds
    /// (`app_config::successor_font_px`) and otherwise falls back to the
    /// config, today's behaviour.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub font_px_milli: Option<u32>,
    /// The size Cmd-0 resets to in the outgoing process, in thousandths of a
    /// device px — the other half of the pair. Carried rather than re-derived
    /// because the successor derives a reset size only for an UNZOOMED window:
    /// without it, Cmd-0 after an update reset to the zoom itself.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub font_reset_px_milli: Option<u32>,
    /// The live font zoom in thousandths of a px at 1× (points on macOS), the
    /// one font-size unit (additive, absent ⇒ none): the size the outgoing
    /// process drew at, written with [`Self::font_reset_size_milli`] as ONE
    /// pair and only while zoomed. Independent of the display, so a successor
    /// whose first window opens on another scale still draws the same size.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub font_size_milli: Option<u32>,
    /// The size Cmd-0 resets to in the outgoing process
    /// (`App::default_font_size`), in thousandths of a px at 1× — the other
    /// half of [`Self::font_size_milli`]'s pair.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub font_reset_size_milli: Option<u32>,
    /// WHAT THE OUTGOING PROCESS'S UPDATE-HEALTH WARNINGS SAID (additive,
    /// absent ⇒ none; round six, finding 54): one entry per kind it announced
    /// and no proof has answered since (`App::update_health_said`), so the
    /// successor whose landing IS the proof records "aterm updates work again"
    /// against it. Without it the record was written only for a warning row
    /// still live under a carried progress row — never on the automatic lane
    /// (which carries no progress row) nor after the row's 45 s fold. Empty is
    /// not written, so a carry with nothing to heal is the wire it always was;
    /// an older reader ignores the key (no `deny_unknown_fields`).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub update_health_said: Vec<CarriedHealthSaid>,
    /// When the messages above were captured, on the outgoing process's wall clock (Unix ms;
    /// absent: unknown). The parent records on from [`Self::next_message_id`] until Commit into
    /// the shared `messages.log`, and an id alone does not say which process wrote a record (a
    /// colliding one loads under a fresh id), so the successor reads a healing as the parent's
    /// only when it was recorded after this stamp
    /// (`App::forget_health_the_parked_parent_healed`). No stamp: no record is the parent's.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub messages_captured_unix_ms: Option<u64>,
}

/// One unanswered update-health warning a parent had said: its kind by row key
/// (`update_words::HealthKind::key`), the title and first line its healing record is written
/// against, and the sentence its "To look at" line quotes (empty from an older parent). The
/// successor skips a key it does not know.
#[derive(Clone, PartialEq, Eq, Debug, serde::Serialize, serde::Deserialize)]
pub(crate) struct CarriedHealthSaid {
    pub kind: String,
    pub title: String,
    #[serde(default)]
    pub line: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub body: String,
    /// The warning was said while no window of the parent's could show it, and is
    /// still owed (`App::owe_unless_seen`): the successor shows it. Absent from an
    /// older parent, and read as not owed.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub owed: bool,
}

/// The message fields of a [`WindowCarry`] as one value — what
/// `App::carried_messages` hands the two construction sites.
#[derive(Clone, PartialEq, Eq, Debug, Default)]
#[cfg(any(unix, test))]
pub(crate) struct MessagesCarry {
    pub messages: Vec<CarriedMessage>,
    pub next_message_id: u64,
    pub update_health_said: Vec<CarriedHealthSaid>,
    /// [`WindowCarry::messages_captured_unix_ms`].
    pub captured_unix_ms: Option<u64>,
}

#[cfg(any(unix, test))]
impl From<&aterm_messages::Carry> for MessagesCarry {
    fn from(carry: &aterm_messages::Carry) -> Self {
        Self {
            messages: carry.live.iter().map(CarriedMessage::from).collect(),
            next_message_id: carry.next_id,
            update_health_said: Vec::new(),
            captured_unix_ms: None,
        }
    }
}

impl WindowCarry {
    /// The carried messages in the center's own shape, for `MessageCenter::seed_carried`. An
    /// older parent's status-bar `bars` key is not read, so its rows are said by the
    /// successor's own reporters at Commit.
    pub fn message_carry(&self) -> aterm_messages::Carry {
        aterm_messages::Carry {
            next_id: self.next_message_id,
            live: self
                .messages
                .iter()
                .map(aterm_messages::CarriedMessage::from)
                .collect(),
        }
    }
}

/// One live message at handoff time: the serialisable twin of
/// [`aterm_messages::CarriedMessage`], every field a string or a number, every intent in its
/// codec form. No deadlines cross: the successor gives every carried row the handoff's
/// staleness cap until Commit.
#[derive(Clone, PartialEq, Eq, Debug, serde::Serialize, serde::Deserialize)]
pub struct CarriedMessage {
    /// The parent's id — the successor keeps it.
    pub id: u64,
    /// The wall clock at ingress, from the parent.
    pub unix_ms: u64,
    /// The tag word.
    pub tag: String,
    /// The current severity word.
    pub severity: String,
    /// The original posted severity when different from the current mark. Older
    /// parents omit it; the engine repairs that fallback at the log reload.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub posted_severity: Option<String>,
    /// The glyph.
    pub glyph: char,
    pub title: String,
    /// The title's short form for the band (`aterm_messages::Message::short_title`).
    /// Absent — an older parent's row, or one that declared none — elides
    /// the title.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub short_title: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub detail: Vec<String>,
    /// Encoded intents (`Intent::encode`).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub actions: Vec<String>,
    /// `default` | `for:<secs>` | `live:<secs>` | `standing` | `ask:<secs>`.
    pub hold: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub key: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fill_permille: Option<u16>,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub stats: String,
    /// The meter was busy (`aterm_messages::Meter::busy`): work with no known fraction,
    /// animated. Absent reads as a still row.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub busy: bool,
    /// Whether the row was on glass when the parent froze.
    #[serde(default)]
    pub on_glass: bool,
    /// Whether the band paints `detail[0]` beside the title (`Message::excerpt`). Absent reads
    /// `true`, the default, which is not written.
    #[serde(
        default = "carried_excerpt",
        skip_serializing_if = "is_carried_excerpt"
    )]
    pub excerpt: bool,
    /// The words the row finishes with (`Message::finished`). Absent derives them from the
    /// title.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub finished: Option<String>,
    /// A record about a previous run (`aterm_messages::Message::retrospective`), ranked one
    /// class lower. Absent reads `false`.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub retrospective: bool,
    /// The row is the wire's (`aterm_messages::Origin::wire_owned`), so a keyless
    /// `notice post` row stays the wire's across the handoff. Absent reads `false`.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub wire: bool,
    /// Duplicate posts folded into the row. Absent reads 1.
    #[serde(
        default = "carried_repeats",
        skip_serializing_if = "is_carried_repeats"
    )]
    pub repeats: u32,
}

/// [`CarriedMessage::repeats`]' default: posted once.
fn carried_repeats() -> u32 {
    1
}

/// The default is not written.
#[allow(
    clippy::trivially_copy_pass_by_ref,
    reason = "serde's skip_serializing_if hands the field by reference"
)]
fn is_carried_repeats(repeats: &u32) -> bool {
    *repeats == 1
}

/// [`CarriedMessage::excerpt`]'s default: the excerpt is painted.
fn carried_excerpt() -> bool {
    true
}

/// The default is not written.
#[allow(
    clippy::trivially_copy_pass_by_ref,
    reason = "serde's skip_serializing_if hands the field by reference"
)]
fn is_carried_excerpt(excerpt: &bool) -> bool {
    *excerpt
}

impl From<&aterm_messages::CarriedMessage> for CarriedMessage {
    fn from(m: &aterm_messages::CarriedMessage) -> Self {
        Self {
            id: m.id,
            unix_ms: m.unix_ms,
            tag: m.tag.clone(),
            severity: m.severity.clone(),
            posted_severity: m.posted_severity.clone(),
            glyph: m.glyph,
            title: m.title.clone(),
            short_title: m.short_title.clone(),
            detail: m.detail.clone(),
            actions: m.actions.clone(),
            hold: m.hold.clone(),
            key: m.key.clone(),
            fill_permille: m.fill_permille,
            stats: m.stats.clone(),
            busy: m.busy,
            on_glass: m.on_glass,
            excerpt: m.excerpt,
            finished: m.finished.clone(),
            retrospective: m.retrospective,
            wire: m.wire,
            repeats: m.repeats,
        }
    }
}

impl From<&CarriedMessage> for aterm_messages::CarriedMessage {
    fn from(m: &CarriedMessage) -> Self {
        Self {
            id: m.id,
            unix_ms: m.unix_ms,
            tag: m.tag.clone(),
            severity: m.severity.clone(),
            posted_severity: m.posted_severity.clone(),
            glyph: m.glyph,
            title: m.title.clone(),
            short_title: m.short_title.clone(),
            detail: m.detail.clone(),
            actions: m.actions.clone(),
            hold: m.hold.clone(),
            key: m.key.clone(),
            fill_permille: m.fill_permille,
            stats: m.stats.clone(),
            busy: m.busy,
            on_glass: m.on_glass,
            excerpt: m.excerpt,
            finished: m.finished.clone(),
            retrospective: m.retrospective,
            wire: m.wire,
            repeats: m.repeats,
        }
    }
}

/// One carried connection edge: the tokenless projection of a live `(src, dst, op)` row. Plain
/// strings because the wire spellings (`SessionId::as_str`, `Op::as_str`) are the manifest
/// format; an op the incoming build cannot parse is dropped and audited at re-mint rather than
/// failing the adoption. The bearer token and launch nonce have no field, so no secret is
/// written at rest.
#[derive(Clone, PartialEq, Eq, Debug, serde::Serialize, serde::Deserialize)]
pub(crate) struct ConnectionCarry {
    pub src: String,
    pub dst: String,
    pub op: String,
}

/// The SEAMLESS-RE-EXEC HANDOFF MANIFEST (proof-carrying DSU, RFC Rung 1a): the
/// serializable projection of the whole session set that the outgoing process writes
/// and the incoming (new) binary reads to re-materialize exactly the same tabs — no
/// session lost, none fabricated. [`SessionHandoff::roundtrips`] and the unit test
/// below check that obligation on this concrete serializer.
#[derive(Clone, PartialEq, Debug, serde::Serialize, serde::Deserialize)]
pub(crate) struct SessionHandoff {
    pub schema: u32,
    /// Additive delegated-resource carry; opaque here so malformed optional
    /// data cannot make an otherwise valid terminal adoption fail.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resources: Option<aterm_json::Value>,
    pub sessions: Vec<SessionRecord>,
    /// Window-frame carry.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub window: Option<WindowCarry>,
    /// The tokenless `(src, dst, op)` triple of every live cross-session edge
    /// (`EdgeTable::edges()`). The incoming process re-mints them with fresh tokens under fresh
    /// nonces once its adopted sessions are registered; the old rows die with the old process.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub connections: Vec<ConnectionCarry>,
    /// The last `turn` id the outgoing process minted. The incoming process continues above it
    /// while still single-threaded (`seamless::take_incoming`), so a `since-turn=`/`since=`
    /// anchor taken before the update still means "after that turn". Carried apart from the
    /// ledgers so a ledger that could not be carried cannot lower it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_turn_id: Option<u64>,
    /// The build number of the process that wrote this manifest. Its presence, never its
    /// value, says the adopted sessions' PATH is not frozen ([`predates_path_self_heal`]).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub outgoing_build: Option<u64>,
    /// The final screen of every pane kept open after its command exited (`--hold`). Not in
    /// `sessions`: a held pane has no process or descriptor, and a record with no PTY would
    /// make the successor refuse the whole handoff (`seamless::validated_identities` wants one
    /// descriptor per record). Best-effort and outside every proof digest: the successor shows
    /// each screen read-only in the pane its layout leaf names, or keeps the "not carried"
    /// placeholder (`seamless::take_held_panes`).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub held: Vec<HeldPaneCarry>,
    /// The outgoing store's roster journal sequence, the row id `exits since=` pages by. The
    /// successor's store counts on above it (`SessionStore::continue_roster_seq`) before it
    /// registers a session, so a pre-update `exits since=<id>` still sees every later exit.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub roster_seq: Option<u64>,
    /// The argv `fabric attach` armed the outgoing bridge supervisor with, when it is not the
    /// configured `[fabric] command` (`fabric_launch::handoff_attached`). A successor with no
    /// command configured arms it (`fabric_launch::carry_attached`), so an update does not
    /// leave the instance `fabric=absent` with its carried fleet holds unliftable. A value that
    /// does not read is none ([`lenient_rows`]).
    #[serde(
        default,
        skip_serializing_if = "Vec::is_empty",
        deserialize_with = "lenient_rows"
    )]
    pub fabric_attached: Vec<String>,
    /// THE FLEET NEEDS THE OUTGOING HERALD HAD TOLD (additive, absent ⇒
    /// none; `docs/DESIGN-owner-surfaces-2026-10-16.md` §3): one row per
    /// fleet need whose desktop notice the outgoing status item had posted
    /// and owed nothing for, `<herald key, 16 hex>=<need word>`
    /// (`fleet_glance::told_row`, `App::fleet_told_rows`), stamped by the
    /// park. A fleet need's notice is keyed by its Mac and session, not by a
    /// handed session, so it rides here rather than on a record. The
    /// successor's first fresh fleet read takes exactly these as told
    /// (`App::herald_fleet`), so a standing need is one notice across the
    /// update while a need the carry does not name — one that arose during
    /// it, or one the outgoing process never told — is told. Bounded
    /// (`fleet_glance::FLEET_TOLD_MAX`); a row that does not read is dropped
    /// (at worst a notice is told again). Outside every proof digest.
    #[serde(
        default,
        skip_serializing_if = "Vec::is_empty",
        deserialize_with = "lenient_rows"
    )]
    pub fleet_told: Vec<String>,
    /// THE BUS MIRROR's closed sessions (additive, absent tolerated; session
    /// log phase 4, ruling Q4): one `<sid> <ep>:<seq>:<ms>:<head>` row per
    /// closed session whose mirrored records the bus does not hold yet, which
    /// the successor says as one exact `gap why=dropped n=<k>` each
    /// (`session_log_mirror::Tombstone`). Empty, so not written, while no
    /// bridge mirrors. A row that does not read is dropped.
    #[serde(
        default,
        skip_serializing_if = "Vec::is_empty",
        deserialize_with = "lenient_rows"
    )]
    pub mirror_dropped: Vec<String>,
    /// THE BUS MIRROR was served here (additive; session log phase 4): the
    /// owning bridge's recorded `hello` said `log=turns`. The successor
    /// takes it as its own until its bridge's hello says (a session that
    /// opens and closes before then is still kept for the bus). `false`, so
    /// not written, while no bridge mirrors.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub mirror_served: bool,
    /// THIS MACHINE'S NAMES (additive, absent tolerated; 2026-10-17): every name
    /// the kernel gave the machine while the outgoing process looked
    /// (`tab_label::machine_names_for_handoff`). A carried shell's OSC 7 names the
    /// host it STARTED under (zsh's `$HOST`, bash's `$HOSTNAME`), which the network
    /// may have renamed since, and which the successor never read: it keeps these
    /// beside its own (`tab_label::adopt_machine_names`), so a citation in a carried
    /// tab is not refused as another machine's. Not written when empty; an older
    /// reader skips it; a value that does not read is none ([`lenient_rows`]).
    #[serde(
        default,
        skip_serializing_if = "Vec::is_empty",
        deserialize_with = "lenient_rows"
    )]
    pub machine_names: Vec<String>,
}

/// One held pane's final screen on the wire: the outgoing pool id its layout leaf names, and
/// its screen carried as a handed session's is (the meta inline, the grids in
/// `.h<id>.heldgrid`/`.h<id>.heldalt` sidecars, `seamless::write_outgoing`). Every field after
/// `screen` is additive and outside every proof digest.
#[derive(Clone, PartialEq, Eq, Debug, serde::Serialize, serde::Deserialize)]
pub(crate) struct HeldPaneCarry {
    pub local_id: u64,
    pub screen: ScreenCarry,
    /// How the pane's command ended, when the outgoing process knew: its exit status or the
    /// signal that ended it, at most one of the two. Absent: shown exited, with no status.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exit_code: Option<i32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exit_signal: Option<u8>,
    /// The pane's session id, so the successor registers it under the sid `aterm ctl @<sid>`
    /// named before the update; absent, it mints a fresh one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sid: Option<String>,
    /// The scrollback lines the held carry left behind, counted on the band like a handed
    /// tab's: every line when no sidecar is named, else the lines older than the sidecar's.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub history_dropped: u64,
    /// `history_dropped` stayed behind because the successor's signed handoff policy carries
    /// no scrollback (`handoff_history::HeldLoss::Withheld`), as
    /// [`SessionRecord::history_withheld`] says it for a handed session.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub history_withheld: bool,
    /// `history_dropped` stayed behind because the offered carry failed
    /// (`handoff_history::HeldLoss::Failed`), so the successor says those lines as a failure.
    /// With neither this nor `history_withheld`, the hop carried the screen alone by design
    /// (a successor older than the producer). A record with both reads as failed.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub history_failed: bool,
    /// `"<len> <sha256hex> <take>"` of this pane's `seamless-<pid>-<nonce>.h<id>.hist`
    /// sidecar, as [`SessionRecord::history`] carries a handed session's
    /// (`handoff_history::name_held_sidecar`). An older reader leaves the sidecar, which the
    /// outgoing process retires once the proof checks out.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub history: Option<String>,
    /// CONTROL CARRY (additive, absent tolerated; round eight of the update
    /// audit, finding 730): `"<len> <sha256hex>"` of this pane's
    /// `seamless-<pid>-<nonce>.h<id>.ctl` sidecar — its turn ledger and archive
    /// tail, exactly as a handed session's `SessionRecord::control` carries them
    /// (`crate::handoff_carry`), so `history` and the harness's report reader still read
    /// the turns a manager drove into the pane before its command ended. An
    /// older reader skips it and leaves the sidecar, which the outgoing process
    /// retires once the proof checks out; in neither proof digest, like the
    /// whole side list. A successor that reads none (an older producer, a carry
    /// left out) starts the pane's ledger saying which turn ids it cannot vouch
    /// for, as it does an adopted session's.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub control: Option<String>,
    /// The pane's exit marked its tab a failure and the person had looked at it
    /// (`session_status::StatusObserver::failure_seen`), so the successor, which shows the
    /// carried exit again, keeps the mark quiet. Absent: the mark shows.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub failure_seen: bool,
}

/// The parts of a session's handoff record its meta and fabric give: one draw for the park
/// ([`SessionHandoff::from_store`]) and the Commit's second reading
/// ([`CarriedClaims::of_session`]), so the two cannot disagree about a session nothing moved.
#[cfg(any(unix, test))]
struct Drawn {
    /// The operator metadata, sanitized as a record carries it.
    meta: crate::session_timeline::SessionMeta,
    /// The claim's row (`"<holder> <remaining_ms>"`) and whether the producer
    /// vouches for it ([`SessionRecord::claim_known`]).
    supervisor: Option<String>,
    claim_known: bool,
    /// The keyed escalations, in write order.
    attention_owners: Vec<String>,
    /// The broadcast opt-ins.
    topics: Vec<String>,
    /// The task record's carried row ([`SessionRecord::task`]).
    task: Option<String>,
}

#[cfg(any(unix, test))]
impl Drawn {
    /// `ctx`'s draw at `now_us`, its own host `own_holder`: the meta's leaf
    /// lock, then the topics', then the timeline's (the task record) — never
    /// across a `Terminal` lock or each other.
    fn of(ctx: &SessionCtx, now_us: u64, own_holder: &str) -> Self {
        let (meta, (supervisor, claim_known), attention_owners) = {
            let live = ctx.meta.lock().unwrap_or_else(|p| p.into_inner());
            (
                live.sanitized(),
                live.handoff_supervisor(now_us, own_holder),
                live.handoff_attention_owners(),
            )
        };
        Self {
            meta,
            supervisor,
            claim_known,
            attention_owners,
            topics: crate::fabric::render_topics(&ctx.fabric.topics()),
            task: ctx
                .timeline
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .task_carry_row(),
        }
    }
}

/// What a handoff record carries of a session that a control verb can move
/// after the park drew it, and that no other Commit fact compares (round eight
/// of the update audit, finding 751): its broadcast opt-ins (`topic add` /
/// `drop`), its keyed escalations (`meta set attention owner=…`), its
/// supervisor claim (`meta set supervisor …`) — the holder and whether the
/// producer vouches for the claim, never a lease's remaining time, which moves
/// by itself — and its task record (`task mark`: a worker the harness marks
/// `done` inside the window would otherwise cross still `working`). A change answered `OK` between the park's draw and the Commit
/// was in no manifest, and a Commit that landed lost it with this process.
///
/// The rest of a record is compared elsewhere: the six bare meta fields ride
/// the layout, which the Commit captures again (`exact_layout`); the hold and
/// the outbox ride the serials (`App::hold_serials`); the inbox and timeline
/// ids their reserves. The park keeps each record's as drawn
/// ([`Self::of_record`]), the Commit reads each live session's
/// ([`Self::of_session`]), and any difference stands the Commit down — a
/// lossless retry, whose next draw carries the change. Compared as final
/// state, so a change made and undone inside the window costs nothing, and a
/// change made just before the draw and undone after it is still caught.
#[cfg(unix)]
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct CarriedClaims {
    pub local_id: u64,
    topics: Vec<String>,
    attention_owners: Vec<String>,
    /// The claim's holder, without the lease's remaining time.
    supervisor: Option<String>,
    claim_known: bool,
    /// The task record's carried row.
    task: Option<String>,
}

#[cfg(unix)]
impl CarriedClaims {
    /// What `record` carries, as the park drew it.
    #[must_use]
    pub(crate) fn of_record(record: &SessionRecord) -> Self {
        Self {
            local_id: record.local_id,
            topics: record.topics.clone(),
            attention_owners: record.attention_owners.clone(),
            supervisor: record.supervisor.as_deref().map(lease_holder),
            claim_known: record.claim_known,
            task: record.task.clone(),
        }
    }

    /// What a record drawn from `ctx` now would carry, `own_holder` this
    /// process's own host ([`crate::harness_host::holder`]).
    #[must_use]
    pub(crate) fn of_session(
        local_id: u64,
        ctx: &SessionCtx,
        now_us: u64,
        own_holder: &str,
    ) -> Self {
        let drawn = Drawn::of(ctx, now_us, own_holder);
        Self {
            local_id,
            topics: drawn.topics,
            attention_owners: drawn.attention_owners,
            supervisor: drawn.supervisor.as_deref().map(lease_holder),
            claim_known: drawn.claim_known,
            task: drawn.task,
        }
    }
}

/// A carried supervisor row's holder (`"<holder> <remaining_ms>"`).
#[cfg(unix)]
fn lease_holder(row: &str) -> String {
    row.split_once(' ')
        .map_or(row, |(holder, _)| holder)
        .to_string()
}

impl SessionHandoff {
    /// Current schema of the handoff manifest.
    pub(crate) const SCHEMA: u32 = 1;

    /// Project a live [`SessionStore`] into the round-trippable manifest, in the
    /// store's stable `local_id` order (so restore preserves tab order).
    #[must_use]
    #[cfg(any(unix, test))]
    pub(crate) fn from_store(store: &SessionStore) -> Self {
        let handles = store.snapshot();
        // Every live edge row of every session's table, tokenless (the fold `flows`
        // aggregates), sorted so the manifest bytes are stable. Each triple once: the raw
        // `grant` verb mints a row per call, so repeats could push the manifest past the
        // successor's cap, and the successor re-mints per pair from the set of ops
        // (`App::remint_carried_connections`). A row it cannot re-mint still rides: its drop
        // is the `session_edge` audit of the edge's dissolution.
        let mut connections: Vec<ConnectionCarry> = Vec::new();
        for h in &handles {
            let rows = {
                let edges = h.ctx.edges.lock().unwrap_or_else(|p| p.into_inner());
                edges.edges()
            };
            connections.extend(rows.into_iter().map(|e| ConnectionCarry {
                src: e.src.as_str().to_string(),
                dst: e.dst.as_str().to_string(),
                op: e.op.as_str().to_string(),
            }));
        }
        connections.sort_by(|a, b| (&a.src, &a.dst, &a.op).cmp(&(&b.src, &b.dst, &b.op)));
        connections.dedup();
        // One clock and one name for every record's claim: a lease's remaining
        // time is counted from here, and this process's own host's claim is
        // left behind (its successor's host takes the session at Commit).
        let now_us = crate::metrics::now_us();
        let own_holder = crate::harness_host::holder();
        // One budget for every session's carried mail (posts and owed
        // receipts, at their escaped TOML cost): the rows ride the
        // manifest, whose cap the successor enforces whole.
        let mut post_budget = crate::fabric::CARRY_MAIL_BYTES;
        Self {
            schema: Self::SCHEMA,
            resources: crate::resource_jobs::carry(),
            window: None,
            connections,
            // Stamped by the handoff worker, which writes the manifest.
            next_turn_id: None,
            outgoing_build: Some(crate::running_build_number()),
            held: Vec::new(),
            // The exit ledger's row id, so the successor's count goes on above
            // it. Clamped to what TOML can write.
            roster_seq: Some(store.roster_seq().min(i64::MAX as u64)),
            #[cfg(unix)]
            fabric_attached: crate::fabric_launch::handoff_attached(),
            #[cfg(not(unix))]
            fabric_attached: Vec::new(),
            // Stamped by the park from the App's fleet herald
            // (`app_update_handoff::stamp_fleet_told`).
            fleet_told: Vec::new(),
            // Drawn with the manifest; a landing past the draw before the
            // Commit stands it down (`StoreMirror::past_draw`).
            mirror_dropped: store.mirror.draw_tombstones(),
            mirror_served: store.mirror.served(),
            // Every name the kernel gave this machine while this process looked: a
            // carried shell still reports the one it started under.
            machine_names: crate::tab_label::machine_names_for_handoff(),
            sessions: handles
                .into_iter()
                .map(|h| {
                    // The meta, claim, keyed escalations and topics: the one draw the
                    // Commit compares again ([`CarriedClaims`]). Leaf locks, never
                    // across a `Terminal` lock.
                    let Drawn {
                        meta,
                        supervisor,
                        claim_known,
                        attention_owners,
                        topics,
                        task,
                    } = Drawn::of(&h.ctx, now_us, &own_holder);
                    // The id the successor's timeline counts on from (the
                    // newest one plus the overlap's reserve: round seven,
                    // finding 50, and its review's item 2), and the session
                    // log's epoch, node-id counters and seq count with theirs, under ONE
                    // hold of the timeline's leaf lock. Taken here, not in the
                    // literal: a guard made in a field's initializer lives
                    // until the whole record is built, and a second one there
                    // would wait on the first.
                    let (timeline_id, log_ids, log_seq) = {
                        let mut tl = h.ctx.timeline.lock().unwrap_or_else(|p| p.into_inner());
                        (tl.handoff_id(), tl.handoff_log_ids(), tl.handoff_log_seq())
                    };
                    let mut hold_rows =
                        crate::fabric::render_holds(&h.ctx.fabric.holds()).into_iter();
                    SessionRecord {
                        local_id: h.local_id,
                        sid: h.sid.as_str().to_string(),
                        parent: h.parent.as_ref().map(|p| p.as_str().to_string()),
                        state: h.state.as_str().to_string(),
                        title: h.title.clone(),
                        // The screen carry is attached by the seamless writer, which
                        // owns the sidecar files; the store projection has no engine.
                        screen: None,
                        user_title: meta.user_title.clone(),
                        description: meta.description.clone(),
                        icon: meta.icon.clone(),
                        role: meta.role.clone(),
                        attention: meta.attention.clone(),
                        questions: meta.questions.clone(),
                        autopilot: meta.autopilot.clone(),
                        land_at: meta.land_at,
                        // Attached by the seamless writer with its sidecar.
                        control: None,
                        frozen_path: store.has_frozen_path(h.local_id),
                        topics,
                        // Stamped by the handoff's park capture, once the readers
                        // that write `Session::fg_holder` are parked
                        // (`app_update_handoff::stamp_fg_holders`).
                        fg_holder: None,
                        rekey: store.has_rekey_channel(h.local_id),
                        loader: store.has_body_loader(h.local_id),
                        // Named by the handoff worker with its sidecar
                        // (`handoff_history::stamp_manifest`), which also adds
                        // this handoff's own drop to the running count below.
                        history: None,
                        history_dropped: 0,
                        history_withheld: false,
                        history_lost: store.history_lost(h.local_id),
                        // The standing halts, read under the fabric's leaf lock as
                        // `status` reads them: the first (the fleet's, when it
                        // stands) where every reader looks, the other beside it.
                        hold: hold_rows.next(),
                        more_holds: hold_rows.collect(),
                        supervisor,
                        claim_known,
                        attention_owners,
                        // Stamped with the history carry, from the park's head
                        // (`handoff_history::stamp_manifest`).
                        viewport_from_bottom: None,
                        render_debt: None,
                        render_debt_cut: 0,
                        // The outbound mailbox and the id counters, under the
                        // fabric's leaf lock (round seven, findings 6/8/28).
                        fabric: h.ctx.fabric.handoff_carry(&mut post_budget),
                        timeline_id,
                        // Stamped by the park from the crash journal's state
                        // (`app_update_handoff::stamp_journal_keys`).
                        journal_key: None,
                        // The task record, in the draw the Commit compares.
                        task,
                        log_ids: Some(log_ids),
                        // Stamped by the park from the App's status item
                        // (`app_update_handoff::stamp_status_seen`).
                        status_seen: None,
                        // Stamped by the park from the harness's glass and the
                        // A/B book (`app_update_handoff::stamp_autopilot_end`).
                        autopilot_end: None,
                        log_seq,
                    }
                })
                .collect(),
        }
    }

    /// Serialize to the handoff blob (TOML).
    #[cfg(any(unix, test))]
    pub(crate) fn to_toml(&self) -> Result<String, String> {
        aterm_toml::to_string(self).map_err(|e| format!("serialize handoff: {e}"))
    }

    /// Parse a handoff blob; `None` on unreadable or incompatible input (the new process then
    /// starts fresh rather than restoring a corrupt manifest).
    #[must_use]
    pub(crate) fn from_toml(s: &str) -> Option<Self> {
        let h: Self = aterm_toml::from_str(s).ok()?;
        (h.schema == Self::SCHEMA).then_some(h)
    }

    /// The concrete round-trip: serialize →
    /// parse yields an identical manifest (no session lost, none fabricated, order
    /// preserved). Returns `true` iff the round-trip is the identity.
    #[must_use]
    #[cfg(test)]
    pub(crate) fn roundtrips(&self) -> bool {
        match self.to_toml() {
            Ok(text) => Self::from_toml(&text).as_ref() == Some(self),
            Err(_) => false,
        }
    }
}

/// The fd/pid channel of the seamless re-exec, `ATERM_SEAMLESS_FDS`, joined to the manifest
/// on `local_id`. Kept apart from [`SessionHandoff`] because an fd number and a pid are
/// process-lifetime values, and in the manifest they would break its round-trip identity
/// (`session_handoff_roundtrips_the_whole_set`).
///
/// Wire form: `"<local_id>=<fd>:<pid>,..."` (e.g. `"0=6:12345,1=7:12346"`). The
/// receiver refuses a wire with any unparseable entry whole, so a corrupt/spoofed
/// value can never fabricate a session — it can only fail closed.
// LOAD-BEARING: this is the LIVE `ATERM_SEAMLESS_FDS` channel. The handoff
// worker builds it (`app_update_handoff`) and `seamless::write_outgoing` encodes
// it; the receiver parses it with `seamless::decode_fds_bounded` and joins each
// entry to the manifest on `local_id`.
#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub(crate) struct HandoffFds {
    /// `(local_id, master fd number, child pid)` for each session handed across the exec.
    pub entries: Vec<(u64, i32, i32)>,
}

impl HandoffFds {
    /// Encode to the `ATERM_SEAMLESS_FDS` wire string.
    #[must_use]
    #[cfg(any(unix, test))]
    pub fn encode(&self) -> String {
        self.entries
            .iter()
            .map(|(lid, fd, pid)| format!("{lid}={fd}:{pid}"))
            .collect::<Vec<_>>()
            .join(",")
    }
}

/// One registered session: its stable fabric identity, the process-local id the GUI routes
/// with, the live engine and sink handles, and its lifecycle and title. `term`/`sink`/`ctx` are
/// the same `Arc`s the owning `Session` holds, so a cross-session read is `handle.term.lock()`.
#[derive(Clone)]
pub(crate) struct SessionHandle {
    /// Stable, pid-free fabric identity (the canonical registry key).
    pub sid: SessionId,
    /// This launch's nonce: an edge binds to it so a restart under a reused id fails closed.
    /// The cross-session gate reads the live `ctx.nonce` (the same value); this mirror is for
    /// restart safety and audit.
    pub nonce: LaunchNonce,
    /// The process-local id the GUI's `Wake`/`Vec<Session>` routing uses.
    pub local_id: u64,
    /// The spawning session's `sid`; `None` for user-opened tabs.
    pub parent: Option<SessionId>,
    /// Lifecycle as the registry observes it.
    pub state: SessionState,
    /// The live window title (best-effort; updated on relabel).
    pub title: String,
    /// The live engine, shared with the owning `Session`.
    pub term: Arc<Mutex<Terminal>>,
    /// This session's PTY master fd (for `signal`'s `tcgetpgrp`/`killpg`).
    pub master: i32,
    /// The per-session fabric context (sink + edge table + identity).
    pub ctx: Arc<SessionCtx>,
}

impl SessionHandle {
    /// THE SESSION'S NAME AS A SNAPSHOT SHOWS IT: the person's `meta set
    /// title`, whole, else the registry title without an agent's spinner frame
    /// (`chrome_label::without_spinner_frame`). Every surface that copies a
    /// session's name once — the tab tooltip's connection lines, Settings ▸
    /// Harness's rows — reads it here, so none of them freezes one frame of a
    /// title animation (the Together run's D7); the Agents surfaces (the
    /// Overview, the ❯ rows, the needs pill) name a tab by the same rule
    /// (`App::tab_label_of_session`).
    pub(crate) fn display_title(&self) -> String {
        self.ctx
            .meta
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .get("title")
            .map_or_else(
                || crate::chrome_label::without_spinner_frame(&self.title).to_string(),
                str::to_string,
            )
    }
}

/// How many roster lifecycle records the store retains, drop-oldest, like
/// [`crate::turn_ledger::LEDGER_CAP`]. A consumer that falls further behind has an exact
/// recovery path ([`SessionStore::roster_since`]); a 4 Hz consumer would have to miss about
/// two minutes of continuous churn.
pub(crate) const ROSTER_JOURNAL_CAP: usize = 512;

/// A change to the registry's membership. State changes (`Spawning → Alive → Exited`) are not
/// one: the session stays registered, and the `sessions` stream reports membership.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum RosterChange {
    /// The sid entered the registry (first registration; a replace is not one).
    Created,
    /// The sid left the registry (`deregister_local_as`).
    Exited,
    /// Not a membership change: an operator asked this instance's bridge to retire a presence
    /// row nobody hosts (`fabric retire <sid>`). It rides this journal because the journal is
    /// the one instance-level stream the bridge holds (`EVENT * …`); every other reader
    /// filters it out.
    RetireRequested,
    /// NOT a membership change either: this instance's last word to its
    /// bridge before the lanes close (`EVENT * bye <quit|handoff>`,
    /// [`crate::fabric_launch::bye`]) — a quit withdraws its sessions from the
    /// roster, a handoff leaves them for the successor. It rides this journal
    /// for the reason `RetireRequested` does; the record's sid is `*`.
    #[cfg(any(unix, test))]
    Bye(aterm_types::fabric_grammar::Bye),
}

/// Why a session left the registry: the `reason=` token of an `exits` row, a `closing`
/// timeline event and a `session-exited` push. `Unknown` is the answer for a path that did
/// not say, never a guess.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ExitReason {
    /// The child ended on its own (reader EOF → `Wake::Exit`).
    ShellExit,
    /// A control-socket `close` retired it (`by=` names the caller's sid).
    CtlClose,
    /// A human closed the tab/pane (✕, Cmd-W, the tab menu).
    UiClose,
    /// The window hosting it was closed (the red button).
    WindowClose,
    /// The application quit. Reserved: `app-quit` is in the wire vocabulary, but quit is
    /// `el.exit()` and nothing deregisters through the store, so no path constructs it.
    #[cfg(test)]
    AppQuit,
    /// The close path did not say.
    Unknown,
}

impl ExitReason {
    /// Every reason, in wire order; the `exits` formatting test walks it.
    #[cfg(test)]
    pub const ALL: [ExitReason; 6] = [
        ExitReason::ShellExit,
        ExitReason::CtlClose,
        ExitReason::UiClose,
        ExitReason::WindowClose,
        ExitReason::AppQuit,
        ExitReason::Unknown,
    ];

    /// The stable wire token (`reason=<token>`).
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            ExitReason::ShellExit => "shell-exit",
            ExitReason::CtlClose => "ctl-close",
            ExitReason::UiClose => "ui-close",
            ExitReason::WindowClose => "window-close",
            #[cfg(test)]
            ExitReason::AppQuit => "app-quit",
            ExitReason::Unknown => "unknown",
        }
    }
}

/// Who retired a session: the `by=` token. A control-socket close names the caller's session
/// when its token was granted to one, else the kind of client (`ctl`, `bridge`); a UI or
/// window close is the human; a path that cannot say writes `-`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum ExitActor {
    /// The sid of the control connection that issued the close.
    Sid(String),
    /// The human, through the window chrome or a keyboard shortcut.
    Human,
    /// An owner-token control client (`aterm ctl … close`, an in-session agent's `@self`
    /// connection). The token names the instance's owner, not a session, so this is all the
    /// socket knows of the caller.
    Ctl,
    /// The fabric bridge (the `aterm-link serve` child the instance spawned): a connection the
    /// server pre-resolves, never a token, so it has no sid; naming it apart from `ctl` shows
    /// a fleet order retired the session. Unix-only like `crate::control::Scope::Bridge`: the
    /// bridge is an inherited `socketpair`.
    #[cfg(any(unix, test))]
    Bridge,
    /// Not attributable on that path.
    Unknown,
}

impl ExitActor {
    /// The stable wire token (`by=<sid|human|ctl|bridge|->`).
    #[must_use]
    pub(crate) fn as_wire(&self) -> &str {
        match self {
            ExitActor::Sid(sid) => sid.as_str(),
            ExitActor::Human => "human",
            ExitActor::Ctl => "ctl",
            #[cfg(any(unix, test))]
            ExitActor::Bridge => "bridge",
            ExitActor::Unknown => "-",
        }
    }
}

/// One entry on the roster lifecycle journal.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct RosterRecord {
    /// Store-monotonic, gap-free, starting at 1; `0` is the watermark a fresh consumer seeds.
    pub seq: u64,
    /// The stable sid that entered or left.
    pub sid: String,
    /// The process-local id it was routed by (`local=` on an `exits` row).
    pub local_id: u64,
    /// Which way it moved.
    pub change: RosterChange,
    /// Milliseconds since the process epoch, the monotonic clock the session timeline and
    /// turn ledger stamp with ([`crate::turn_ledger::now_ms`]), so an `exit` row aligns
    /// against a `timeline` row.
    pub t_ms: u64,
    /// Why it left. Meaningful iff `change == Exited`; a `Created` record carries
    /// the neutral [`ExitReason::Unknown`].
    pub reason: ExitReason,
    /// The child's exit status when the shell-exit path recovered one
    /// ([`SessionStore::note_exit_code`]); `None` (`exit_code=-`) when the child was hung up
    /// by a close, died by signal, was not this process's to reap, or was not yet reapable at
    /// either of the ledger's two non-blocking looks (`App::shell_exit_code`).
    pub exit_code: Option<i32>,
    /// Who retired it. Meaningful iff `change == Exited`; neutral
    /// [`ExitActor::Unknown`] on a `Created` record.
    pub actor: ExitActor,
}

/// The process-wide registry. Keyed canonically by [`SessionId`]; a second index bridges the
/// GUI's `u64` ids to those sids. Both key spaces are mutated under the one outer `RwLock`, so
/// a register/deregister is atomic across them.
///
/// `roster` is a bounded, drop-oldest, strictly-increasing journal of membership changes,
/// appended under the same `&mut self` that mutates `by_id`, so a session is never registered
/// without its record or vice versa. A consumer keeps a `u64` watermark: one compare against
/// [`roster_seq`](Self::roster_seq) says whether anything changed, and
/// [`roster_since`](Self::roster_since) yields exactly the records it has not seen, both
/// halves of a spawn/exit pair inside one tick included (a snapshot diff would lose them).
#[derive(Default)]
pub(crate) struct SessionStore {
    by_id: HashMap<SessionId, SessionHandle>,
    by_local: HashMap<u64, SessionId>,
    /// The bounded roster lifecycle journal, oldest-first (see the type doc).
    roster: VecDeque<RosterRecord>,
    /// Monotone source of journal seqs; counts on past eviction, so a consumer's watermark
    /// stays meaningful after the ring rolls.
    roster_seq: u64,
    /// The INSTANCE's watchers: a `subscribe` with the `sessions` stream or
    /// `@*` registers here, and every journal append rings it, so a spawn or
    /// an exit reaches the push loop at once rather than on its liveness pass.
    instance_watchers: std::sync::Arc<crate::session_watch::Watchers>,
    /// Exit statuses noted by the shell-exit path (`Wake::Exit`), keyed by local
    /// id and CONSUMED by the deregister that follows. A side table rather than a
    /// handle field because the handle is cloned out per control request and its
    /// constructors are many; the status is known for the short window between
    /// the reader's EOF and the pane's teardown (indefinitely under `--hold`),
    /// which is exactly the window this table spans.
    exit_codes: HashMap<u64, i32>,
    /// Sessions whose shell has a frozen PATH ([`SessionRecord::frozen_path`]), marked at
    /// adoption (`App::register_session`) and counted by [`Self::frozen_path_tabs`].
    ///
    /// This and the side tables below are cleared by the deregister; all but
    /// `integration_frozen` are projected into the next manifest by
    /// [`SessionHandoff::from_store`].
    frozen_path: std::collections::HashSet<u64>,
    /// Sessions whose shell carries the re-key channel (`shell_rekey`): spawned with
    /// `ATERM_REKEY_PATH`, or adopted from a record that said so.
    rekey_channel: std::collections::HashSet<u64>,
    /// Sessions whose shell runs an integration loader (`shell_body`): spawned with
    /// `ATERM_INTEGRATION_POINTER`, or adopted from a record (or carried revision) that said so.
    body_loader: std::collections::HashSet<u64>,
    /// Sessions adopted with an integration from before loaders (no loader in their record,
    /// no revision carried): no body pointer reaches them, so `status integration_rev=frozen`
    /// until they sign a revision. Not carried: the next successor finds the same absence.
    integration_frozen: std::collections::HashSet<u64>,
    /// History lines each session's update handoffs could not carry, summed over every
    /// handoff it crossed: adopted from its record, raised when this process's import of its
    /// sidecar fails, answered by `status history_lost=`, projected into the next manifest.
    history_lost: HashMap<u64, u64>,
    /// The window's harness posture ([`crate::autopilot::HarnessPosture`]), under which a
    /// session's effective Autopilot is read off the main thread (the `sessions` roster's
    /// `autopilot=`). The App publishes it at launch and whenever the policy in force changes
    /// (`App::publish_harness_posture`); unpublished it is `Unhosted`, so a registry no window
    /// runs says nothing is supervised. Per window and not carried: the successor publishes its
    /// own.
    harness_posture: crate::autopilot::HarnessPosture,
    /// THE CLOSED-SESSION INDEX ([`crate::session_log::ClosedLogs`]): what
    /// `log @<sid>` and `brief @<sid>` answer once the session has closed in
    /// this process. Linked by `App::retire_session_registration` after the
    /// deregister, built outside this lock; read under the read lock by an
    /// `Arc` clone. Not carried.
    closed_logs: crate::session_log::ClosedLogs,
    /// Only an active journal arms this bounded writer handoff. Off stays None.
    journal_retirements: Option<crate::session_log_journal::Retirements>,
    /// WHAT THE CRASH JOURNAL HOLDS ON DISK OF THE WORDS ABOUT TYPING
    /// ([`crate::crash_journal::Durable`]): the window's lane installs it at
    /// launch (`App::arm_word_durability`), and the control verbs that move a
    /// word (`hold`, `meta set|unset autopilot`) answer once the image that
    /// carries it is durable. Unarmed (a registry no journaling lane runs), it
    /// answers at once. Not carried: the successor arms its own.
    journal_durable: crate::crash_journal::Durable,
    /// THE REPLIES OWED FOR INPUT WRITTEN INTO THESE SESSIONS
    /// ([`crate::control::InputReplies`]): counted by the control server's lanes
    /// from before an input verb's dispatch until its reply is delivered, and read
    /// by the App's update Commit and exit, which do not end the process over one.
    /// Not carried.
    input_replies: crate::control::InputReplies,
    /// THE BUS MIRROR's closed-session tails, tombstones and adoption latch
    /// (`session_log_mirror`, session log phase 4). Empty, holding no heap,
    /// while no bridge mirrors.
    mirror: crate::session_log_mirror::StoreMirror,
}

/// Shared handle to the registry, cloned into the control thread.
pub(crate) type Store = Arc<RwLock<SessionStore>>;

/// A new, empty, shared registry.
#[must_use]
pub(crate) fn new_store() -> Store {
    Arc::new(RwLock::new(SessionStore::default()))
}

impl SessionStore {
    /// Register (or replace) a handle, wiring both key spaces atomically. A first
    /// registration records `spawned` on the session's timeline; a replace (e.g. a relabel)
    /// does not.
    pub(crate) fn register(&mut self, handle: SessionHandle) {
        let first = !self.by_id.contains_key(&handle.sid);
        if first {
            let mut tl = handle
                .ctx
                .timeline
                .lock()
                .unwrap_or_else(|p| p.into_inner());
            // The one place every session passes: a context whose timeline
            // rings another list than `ctx.watchers` would leave every watcher
            // registered on it unwoken by its records.
            debug_assert!(
                std::sync::Arc::ptr_eq(tl.watchers(), &handle.ctx.watchers),
                "a session context not built from one session_watch::Observed"
            );
            // THE BUS MIRROR's state this store shares with the session's
            // log (`session_log_mirror::Shared`): nothing is engaged by it
            // while the node does not mirror.
            tl.slog_mut().bind_mirror(self.mirror.shared());
            // THE PERSON'S COUNT the log reconciles against (ruling T2).
            tl.slog_mut()
                .bind_person(std::sync::Arc::clone(&handle.ctx.human_input));
            tl.record("spawned", format!("state={}", handle.state.as_str()));
            // The session log's `open - spawn|adopt|restore parent=<sid> epoch=<ep>
            // state=<w>` in the row's hold (phase 6, 6F-7); an adopted session's log
            // already holds its handoff gap (`spawn::mark_handoff_gap`), and a carried one
            // its open, so the registration is its `state carried=1` mark.
            let at = tl.slog().now();
            tl.slog_mut().open_session(
                handle.parent.as_ref().map(SessionId::as_str),
                Some(handle.state.as_str()),
                at,
            );
        }
        // Journalled under the same `&mut self` (the same write lock) that mutates the two
        // indexes, so no reader sees a registry and a journal that disagree. A replace is
        // not a membership change and records nothing.
        let journal = first.then(|| (handle.sid.as_str().to_string(), handle.local_id));
        self.by_local.insert(handle.local_id, handle.sid.clone());
        self.by_id.insert(handle.sid.clone(), handle);
        if let Some((sid, local_id)) = journal {
            self.record_roster(
                sid,
                local_id,
                RosterChange::Created,
                ExitReason::Unknown,
                None,
                ExitActor::Unknown,
            );
        }
    }

    /// Append one roster lifecycle record, evicting the oldest past [`ROSTER_JOURNAL_CAP`].
    /// Private and `&mut self`: only a mutator holding the write lock reaches it.
    fn record_roster(
        &mut self,
        sid: String,
        local_id: u64,
        change: RosterChange,
        reason: ExitReason,
        exit_code: Option<i32>,
        actor: ExitActor,
    ) {
        self.roster_seq += 1;
        if self.roster.len() == ROSTER_JOURNAL_CAP {
            self.roster.pop_front();
        }
        self.roster.push_back(RosterRecord {
            seq: self.roster_seq,
            sid,
            local_id,
            change,
            t_ms: crate::turn_ledger::now_ms(),
            reason,
            exit_code,
            actor,
        });
        // The in-GUI supervisor host decides from membership: a session that
        // closed loses its worker at once (`harness_host`, which parks on this).
        crate::harness_host::ring();
        self.instance_watchers.ring_watchers();
    }

    /// The instance's watcher list, which every roster journal append rings:
    /// what a `sessions` or `@*` subscription registers on.
    #[must_use]
    pub(crate) fn instance_watchers(&self) -> std::sync::Arc<crate::session_watch::Watchers> {
        self.instance_watchers.clone()
    }

    /// Ask this instance's bridge to retire the presence row of `sid`, which no session here
    /// carries (`false`, and nothing recorded, when one does). The request is a journal row the
    /// events lane pushes as `EVENT * fabric-retire <sid>`; the bridge re-checks hosting and
    /// publishes `exited` through its own producer sequence.
    pub(crate) fn request_retire(&mut self, sid: &str) -> bool {
        if self.by_sid(&SessionId::new(sid.to_string())).is_some() {
            return false;
        }
        self.record_roster(
            sid.to_string(),
            0,
            RosterChange::RetireRequested,
            ExitReason::Unknown,
            None,
            ExitActor::Unknown,
        );
        true
    }

    /// Tell this instance's bridge how the instance is leaving: a journal row
    /// the events lane pushes as `EVENT * bye <quit|handoff>`
    /// ([`crate::fabric_launch::bye`] waits for the bridge to take it).
    #[cfg(any(unix, test))]
    pub(crate) fn say_bye(&mut self, bye: aterm_types::fabric_grammar::Bye) {
        self.record_roster(
            "*".to_string(),
            0,
            RosterChange::Bye(bye),
            ExitReason::Unknown,
            None,
            ExitActor::Unknown,
        );
    }

    /// Mark `local_id` as having a frozen PATH (see the field). Idempotent; an unknown id is
    /// remembered too (the registration may follow in the same spawn seam).
    pub(crate) fn mark_frozen_path(&mut self, local_id: u64) {
        self.frozen_path.insert(local_id);
    }

    /// Lower the mark: the shell's PATH was measured live, in the environment of a child it
    /// started fronting aterm's `agents/` (`session_status::program::leader_facts`). The next
    /// handoff then carries `false` and the managed-current count drops it. Idempotent. A
    /// frozen reading never raises the mark (`SessionTimeline::take_path_lowered`).
    pub(crate) fn clear_frozen_path(&mut self, local_id: u64) {
        self.frozen_path.remove(&local_id);
    }

    /// The local ids of every registered session whose shell has not exited,
    /// sorted — the tabs a person can still type into.
    #[must_use]
    pub(crate) fn running_ids(&self) -> Vec<u64> {
        let mut ids: Vec<u64> = self
            .by_local
            .keys()
            .copied()
            .filter(|id| !self.exit_codes.contains_key(id))
            .collect();
        ids.sort_unstable();
        ids
    }

    /// Add `lines` to the history `local_id`'s update handoffs could not
    /// carry (see the field). Zero adds nothing and records nothing.
    pub(crate) fn add_history_lost(&mut self, local_id: u64, lines: u64) {
        if lines > 0 {
            let lost = self.history_lost.entry(local_id).or_insert(0);
            *lost = lost.saturating_add(lines);
        }
    }

    /// The history lines `local_id`'s update handoffs could not carry, over
    /// every handoff it crossed — `status`'s `history_lost=`.
    #[must_use]
    pub(crate) fn history_lost(&self, local_id: u64) -> u64 {
        self.history_lost.get(&local_id).copied().unwrap_or(0)
    }

    /// Whether `local_id` was marked frozen ([`Self::mark_frozen_path`]) — what the
    /// next handoff manifest carries per record.
    #[must_use]
    pub(crate) fn has_frozen_path(&self, local_id: u64) -> bool {
        self.frozen_path.contains(&local_id)
    }

    /// The window's harness posture as the App last published it (see the field).
    #[must_use]
    pub(crate) fn harness_posture(&self) -> crate::autopilot::HarnessPosture {
        self.harness_posture
    }

    /// Publish the window's harness posture (see the field).
    pub(crate) fn set_harness_posture(&mut self, posture: crate::autopilot::HarnessPosture) {
        self.harness_posture = posture;
    }

    /// The replies owed for input written into these sessions (see the field).
    pub(crate) fn input_replies(&self) -> crate::control::InputReplies {
        self.input_replies.clone()
    }

    /// What the crash journal holds on disk of the words about typing (see
    /// the field), for a verb that waits on it outside this lock.
    pub(crate) fn journal_durable(&self) -> crate::crash_journal::Durable {
        self.journal_durable.clone()
    }

    /// Arm the journal's durability for the verbs (see the field).
    pub(crate) fn set_journal_durable(&mut self, durable: crate::crash_journal::Durable) {
        self.journal_durable = durable;
    }

    /// Mark `local_id` as carrying the re-key channel (see the field). Idempotent; an unknown
    /// id is remembered too (the registration may follow in the same spawn seam).
    pub(crate) fn mark_rekey_channel(&mut self, local_id: u64) {
        self.rekey_channel.insert(local_id);
    }

    /// Whether `local_id` carries the re-key channel
    /// ([`Self::mark_rekey_channel`]) — what the next handoff record carries.
    #[must_use]
    pub(crate) fn has_rekey_channel(&self, local_id: u64) -> bool {
        self.rekey_channel.contains(&local_id)
    }

    /// Mark `local_id` as running an integration loader (see the field). Idempotent.
    pub(crate) fn mark_body_loader(&mut self, local_id: u64) {
        self.body_loader.insert(local_id);
    }

    /// Whether `local_id` runs an integration loader
    /// ([`Self::mark_body_loader`]) — what the next handoff record carries.
    #[cfg(any(unix, test))]
    #[must_use]
    pub(crate) fn has_body_loader(&self, local_id: u64) -> bool {
        self.body_loader.contains(&local_id)
    }

    /// Mark `local_id` as adopted with an integration from before loaders (see the field).
    pub(crate) fn mark_integration_frozen(&mut self, local_id: u64) {
        self.integration_frozen.insert(local_id);
    }

    /// Whether `local_id` was adopted with an integration from before loaders
    /// ([`Self::mark_integration_frozen`]) — what `status integration_rev=` reads
    /// until the shell signs a revision.
    #[must_use]
    pub(crate) fn is_integration_frozen(&self, local_id: u64) -> bool {
        self.integration_frozen.contains(&local_id)
    }

    /// How many live tabs still run a frozen shell: marked, registered, and with no exit status
    /// noted (an exited `--hold` shell is no tab to source the hook in). The managed-current
    /// row's "N tabs from before this update" count, an upper bound: sourcing the hook is not
    /// reported back, so it falls only as tabs close or a live PATH is measured
    /// ([`Self::clear_frozen_path`]).
    #[must_use]
    pub(crate) fn frozen_path_tabs(&self) -> usize {
        self.frozen_path_ids().len()
    }

    /// The local ids [`Self::frozen_path_tabs`] counts, sorted; the managed record splits them
    /// by what runs in front (`App::tabs_behind`).
    #[must_use]
    pub(crate) fn frozen_path_ids(&self) -> Vec<u64> {
        let mut ids: Vec<u64> = self
            .frozen_path
            .iter()
            .copied()
            .filter(|id| self.by_local.contains_key(id) && !self.exit_codes.contains_key(id))
            .collect();
        ids.sort_unstable();
        ids
    }

    /// Note the exited child's status for `local_id` ahead of its deregistration, which moves
    /// it onto the journal row. `None` records nothing (the row says `exit_code=-`).
    pub(crate) fn note_exit_code(&mut self, local_id: u64, code: Option<i32>) {
        if let Some(code) = code {
            self.exit_codes.insert(local_id, code);
        }
    }

    /// The exit code [`Self::note_exit_code`] holds for `local_id` until its
    /// deregister moves it onto the journal row. TEST-ONLY.
    #[cfg(all(test, unix))]
    pub(crate) fn noted_exit_code(&self, local_id: u64) -> Option<i32> {
        self.exit_codes.get(&local_id).copied()
    }

    /// Deregister with no attribution: reason and actor `Unknown`, except that an `Exited`
    /// shell is journalled as a shell exit. Test-only, so no production path deregisters
    /// without saying why: every close goes through `App::retire_session_registration`, which
    /// passes its gesture's attribution to [`deregister_local_as`](Self::deregister_local_as).
    #[cfg(test)]
    pub(crate) fn deregister_local(&mut self, local_id: u64) -> Option<SessionId> {
        self.deregister_local_as(local_id, ExitReason::Unknown, ExitActor::Unknown)
    }

    /// Deregister `local_id` from both key spaces atomically, returning its sid; `None` if
    /// unknown (a late deregister).
    ///
    /// `reason`/`actor` say why and by whom. [`ExitReason::Unknown`] on an already `Exited`
    /// shell becomes [`ExitReason::ShellExit`]; an explicit reason wins (a human closing an
    /// exited `--hold` pane is `ui-close`). A known reason is recorded as the `closing`
    /// timeline event while the handle is still registered, so a live `subscribe @<sid>
    /// events` watch pushes `EVENT <local> closing reason= by=` ahead of `exited`; the `exits`
    /// journal row keeps the same facts.
    pub(crate) fn deregister_local_as(
        &mut self,
        local_id: u64,
        reason: ExitReason,
        actor: ExitActor,
    ) -> Option<SessionId> {
        // Consumed even on a late duplicate deregister, so no stale code is pinned on a
        // session that later reuses the local id.
        let exit_code = self.exit_codes.remove(&local_id);
        // A closed frozen tab is no longer a tab the row must name.
        self.frozen_path.remove(&local_id);
        self.rekey_channel.remove(&local_id);
        self.body_loader.remove(&local_id);
        self.integration_frozen.remove(&local_id);
        self.history_lost.remove(&local_id);
        let sid = self.by_local.remove(&local_id)?;
        // The death mark, written WHILE the handle is still registered: the
        // timeline is Arc-shared, so a holder that kept the ctx (pool teardown
        // races) or the timeline alone (a live subscribe watch's
        // `WatchSource`) reads the `closing` row and the final
        // `state-change` from the same object — and this write precedes the
        // index removal in program order, not merely inside the same lock hold,
        // so "before it is gone" is literally the code's order too.
        let retirements = &mut self.journal_retirements;
        let mirror = &self.mirror;
        let resolved = self.by_id.get(&sid).map(|h| {
            let reason = if reason == ExitReason::Unknown && h.state == SessionState::Exited {
                ExitReason::ShellExit
            } else {
                reason
            };
            let mut tl = h.ctx.timeline.lock().unwrap_or_else(|p| p.into_inner());
            if reason != ExitReason::Unknown {
                tl.record("closing", closing_payload(reason, &actor));
            }
            tl.record("state-change", "state=closed".to_string());
            // The session log's `close - out=<reason> by=<actor> code=<n> closer=<raw>`
            // (the raw actor at the content tier, ruling U12); everything still open
            // closes under it.
            let at = tl.slog().now();
            tl.slog_mut().close_session(
                crate::session_log::exit_out(reason),
                crate::session_log::exit_actor(&actor),
                actor.as_wire(),
                exit_code,
                at,
            );
            if let Some(retirements) = retirements.as_mut() {
                retirements.retire(&sid, &h.ctx.timeline, tl.slog());
            }
            // A mirrored log's tail the bus does not hold yet stays for it
            // (ruling Q4); a log nothing mirrored keeps nothing.
            mirror.retire(sid.as_str(), tl.slog_mut());
            reason
        });
        if let Some(reason) = resolved {
            self.by_id.remove(&sid);
            // Journalled only when the handle was there, so the journal records exactly
            // the transitions `by_id` performed.
            self.record_roster(
                sid.as_str().to_string(),
                local_id,
                RosterChange::Exited,
                reason,
                exit_code,
                actor,
            );
        }
        Some(sid)
    }

    /// Set the session's lifecycle state (e.g. `Exited` on `Wake::Exit`); a no-op if unknown.
    /// An actual transition is recorded on the timeline as `state-change`, and in the same
    /// hold as the session log's `mark - state state=<w>` (phase 6, 6F-5).
    pub(crate) fn set_state(&mut self, local_id: u64, state: SessionState) {
        if let Some(sid) = self.by_local.get(&local_id)
            && let Some(h) = self.by_id.get_mut(sid)
            && h.state != state
        {
            h.state = state;
            record_state_change(&h.ctx.timeline, state);
        }
    }

    /// Confirm a session's reader is live: `Spawning → Alive`, and only from `Spawning`, so a
    /// handle that already raced to `Exited` is not resurrected and a late `Wake::Ready` is a
    /// no-op. `true` iff this call made the transition.
    pub(crate) fn mark_alive(&mut self, local_id: u64) -> bool {
        if let Some(sid) = self.by_local.get(&local_id)
            && let Some(h) = self.by_id.get_mut(sid)
            && h.state == SessionState::Spawning
        {
            h.state = SessionState::Alive;
            record_state_change(&h.ctx.timeline, SessionState::Alive);
            return true;
        }
        false
    }

    /// Update the live title (on relabel); a no-op if unknown or unchanged.
    /// It records nothing: since phase 6 (6F-8, ruling U13) the timeline's
    /// `title-change` rows follow the session log's content-tier title
    /// marks, every session's, from the drift flush
    /// ([`crate::session_timeline::SessionTimeline::record_title`]), not
    /// this front tab's relabel.
    pub(crate) fn set_title(&mut self, local_id: u64, title: &str) {
        if let Some(sid) = self.by_local.get(&local_id)
            && let Some(h) = self.by_id.get_mut(sid)
            && h.title != title
        {
            h.title.clear();
            h.title.push_str(title);
        }
    }

    /// Link a closed session's entry into the closed-session index
    /// ([`crate::session_log::ClosedLogs::link`]); what it frees comes back
    /// for the caller to drop once this lock is released.
    pub(crate) fn link_closed_log(
        &mut self,
        entry: crate::session_log::ClosedLog,
    ) -> Vec<Arc<aterm_slog::Log>> {
        self.closed_logs.link(entry)
    }

    /// The closed-session index, for its reads.
    pub(crate) fn closed_logs(&self) -> &crate::session_log::ClosedLogs {
        &self.closed_logs
    }

    /// The bus mirror's state (`session_log_mirror`).
    pub(crate) fn mirror(&self) -> &crate::session_log_mirror::StoreMirror {
        &self.mirror
    }

    /// Look up a handle by its stable [`SessionId`].
    #[must_use]
    pub(crate) fn by_sid(&self, sid: &SessionId) -> Option<&SessionHandle> {
        self.by_id.get(sid)
    }

    /// Look up a handle by its process-local `u64` id.
    #[must_use]
    pub(crate) fn by_local(&self, local_id: u64) -> Option<&SessionHandle> {
        self.by_local
            .get(&local_id)
            .and_then(|sid| self.by_id.get(sid))
    }

    /// The handle a wire `@<selector>` names: a local number or a stable id.
    /// `None` for self (the connection's own session, which the registry does not
    /// know), an invalid spelling, and an id or number no session has.
    #[must_use]
    pub(crate) fn by_selector(&self, selector: &Selector<'_>) -> Option<&SessionHandle> {
        match selector {
            Selector::Local(local_id) => self.by_local(*local_id),
            Selector::Sid(sid) => self.by_id.get(sid.as_str()),
            Selector::SelfTok | Selector::Invalid(_) => None,
        }
    }

    /// Number of registered sessions.
    #[cfg(test)]
    #[must_use]
    pub fn len(&self) -> usize {
        self.by_id.len()
    }

    /// Whether the registry is empty.
    #[cfg(test)]
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.by_id.is_empty()
    }

    /// A snapshot of every registered handle, sorted by `local_id`. Cloned so the caller can
    /// drop the store guard before formatting or locking a `Terminal`.
    #[must_use]
    pub(crate) fn snapshot(&self) -> Vec<SessionHandle> {
        let mut v: Vec<SessionHandle> = self.by_id.values().cloned().collect();
        v.sort_by_key(|h| h.local_id);
        v
    }

    /// The fabric bridge's identity-only roster, sorted by local id: each row's local id, sid
    /// and launch nonce from the same handle, under one guard, with no terminal or context
    /// cloned.
    #[must_use]
    pub(crate) fn bridge_roster(&self) -> Vec<(u64, SessionId, aterm_session::LaunchNonce)> {
        let mut rows: Vec<_> = self
            .by_id
            .values()
            .map(|h| (h.local_id, h.sid.clone(), h.nonce))
            .collect();
        rows.sort_by_key(|row| row.0);
        rows
    }

    /// The set of live session sids, for the subscribe `sessions` stream's seed and its
    /// recovery diff. Clones only the sid strings and does not sort.
    #[must_use]
    pub(crate) fn live_sids(&self) -> std::collections::HashSet<String> {
        self.by_id
            .values()
            .map(|h| h.sid.as_str().to_string())
            .collect()
    }

    /// The roster journal's high-water: the seq of the newest membership change, or `0` when
    /// the registry has never gained or lost a session. One compare against a consumer's
    /// watermark says whether anything changed.
    #[must_use]
    pub(crate) fn roster_seq(&self) -> u64 {
        self.roster_seq
    }

    /// Raise the journal's sequence to the outgoing store's ([`SessionHandoff::roster_seq`])
    /// before the first session registers, so a pre-update `exits since=<id>` still means
    /// "after that row". Never lowers the count, nor passes what a manifest can carry.
    pub(crate) fn continue_roster_seq(&mut self, carried: u64) {
        self.roster_seq = self.roster_seq.max(carried.min(i64::MAX as u64));
    }

    /// The lowest retained journal seq, or `None` when nothing is retained. A consumer whose
    /// watermark `w` has `w + 1 < low_seq` missed evicted records and must rebuild from a
    /// whole-set diff ([`Self::live_sids`]).
    #[must_use]
    pub(crate) fn roster_low_seq(&self) -> Option<u64> {
        self.roster.front().map(|r| r.seq)
    }

    /// Journal records with `seq > after`, oldest-first; double-ended, so a
    /// reader after only the newest few walks from the back.
    ///
    /// Seqs are minted by one counter under the write lock and pushed to the
    /// back, so they are strictly increasing across the deque and
    /// `partition_point` seeks to the first unseen record in O(log n); the walk
    /// that follows is O(records the caller actually missed). A watermark below
    /// the retained low-water yields everything retained — callers must test
    /// [`roster_low_seq`](Self::roster_low_seq) first if they need to know that
    /// the prefix was lossy (see its doc).
    pub(crate) fn roster_since(
        &self,
        after: u64,
    ) -> impl DoubleEndedIterator<Item = &RosterRecord> {
        let start = self.roster.partition_point(|r| r.seq <= after);
        self.roster.range(start..)
    }

    /// Idempotent setup shared by the caller's pre-spawn arm and writer setup.
    pub(crate) fn arm_session_journal(
        &mut self,
        wake: Option<aterm_slog::Notify>,
        timeline_max: usize,
        dropped_max: usize,
    ) -> Arc<()> {
        self.journal_retirements
            .get_or_insert_with(|| {
                crate::session_log_journal::Retirements::new(wake, timeline_max, dropped_max)
            })
            .owner()
    }

    pub(crate) fn take_session_journal_retirements(
        &mut self,
    ) -> crate::session_log_journal::RetirementBatch {
        self.journal_retirements.as_mut().map_or_else(
            crate::session_log_journal::RetirementBatch::default,
            crate::session_log_journal::Retirements::take,
        )
    }

    pub(crate) fn session_journal_pending(&self) -> bool {
        self.journal_retirements
            .as_ref()
            .is_some_and(crate::session_log_journal::Retirements::pending)
    }

    /// Return ownership for destruction AFTER the caller drops the store lock.
    pub(crate) fn disarm_session_journal(
        &mut self,
        owner: &Arc<()>,
    ) -> Option<crate::session_log_journal::Retirements> {
        if self
            .journal_retirements
            .as_ref()
            .is_some_and(|r| r.owned_by(owner))
        {
            self.journal_retirements.take()
        } else {
            None
        }
    }

    /// Every registered handle, by reference and in `HashMap` order: unlike
    /// [`snapshot`](Self::snapshot) it neither clones nor sorts.
    pub(crate) fn live_handles(&self) -> impl Iterator<Item = &SessionHandle> {
        self.by_id.values()
    }
}

/// A session's lifecycle state moved to `state`: the timeline's `state-change state=<w>` row
/// and, in the same hold, the session log's `mark - state state=<w>` (phase 6, 6F-5), its
/// wake rung there. The close's `state-change state=closed` row is the session close's
/// record instead (`deregister_local_as`): no mark is written after a session closes.
fn record_state_change(
    timeline: &std::sync::Mutex<crate::session_timeline::SessionTimeline>,
    state: SessionState,
) {
    let mut tl = timeline.lock().unwrap_or_else(|p| p.into_inner());
    let at = tl.slog().now();
    tl.slog_mut().observe_state(state.as_str(), at);
    tl.record("state-change", format!("state={}", state.as_str()));
    let _ = tl.slog_take_wake();
}

/// The `closing` event's payload: `reason=<token> by=<sid|human|ctl|bridge|->`, the actor
/// pct-encoded like every free value on a timeline row.
fn closing_payload(reason: ExitReason, actor: &ExitActor) -> String {
    format!(
        "reason={} by={}",
        reason.as_str(),
        crate::control::pct_encode(actor.as_wire())
    )
}

thread_local! {
    /// The attribution the current close gesture carries (main thread only, where every
    /// deregistration happens). See [`CloseAttribution`].
    static CLOSE_ATTRIBUTION: std::cell::RefCell<Option<(ExitReason, ExitActor)>> =
        const { std::cell::RefCell::new(None) };
}

/// A scope naming why the sessions retired inside it are retired.
///
/// Every close reaches the registry through one funnel (`teardown_session` →
/// `retire_session_registration` → `deregister_local_as`), but the gesture that started it
/// (a control-socket `close`, the red window button, a tab ✕, Cmd-W, `tab close`) is several
/// calls up. The initiator enters a scope and the funnel reads it, so no intermediate
/// signature carries a reason.
///
/// The outermost initiator wins: `enter` is a no-op while a scope is open, so a `close` verb
/// that lands in `close_tab_at` keeps its `ctl-close` though `close_tab_at` claims `ui-close`
/// for gestures that reach it directly. Dropping the opening guard closes the scope; a close
/// with no scope open is `Unknown`.
pub(crate) struct CloseAttribution {
    /// Whether this guard opened the scope (and so must close it).
    opened: bool,
}

impl CloseAttribution {
    /// Open the scope for the current gesture unless one is already open. Bind the
    /// result (`let _closing = …`): the attribution lasts exactly as long as it.
    #[must_use = "the attribution lasts exactly as long as this guard lives"]
    pub(crate) fn enter(reason: ExitReason, actor: ExitActor) -> Self {
        let opened = CLOSE_ATTRIBUTION.with(|slot| {
            let mut slot = slot.borrow_mut();
            if slot.is_none() {
                *slot = Some((reason, actor));
                true
            } else {
                false
            }
        });
        Self { opened }
    }
}

impl Drop for CloseAttribution {
    fn drop(&mut self) {
        if self.opened {
            CLOSE_ATTRIBUTION.with(|slot| *slot.borrow_mut() = None);
        }
    }
}

/// The attribution of the close gesture in progress on this thread, or
/// `(Unknown, Unknown)` when none is open.
#[must_use]
pub(crate) fn current_close_attribution() -> (ExitReason, ExitActor) {
    CLOSE_ATTRIBUTION
        .with(|slot| slot.borrow().clone())
        .unwrap_or((ExitReason::Unknown, ExitActor::Unknown))
}

impl crate::App {
    /// Capture the exited child's status for the exit ledger, on the shell-exit path
    /// (`exit_session_logical`): after the reader's EOF and before teardown reaps and
    /// discards it. The deregister that follows moves it onto the journal row.
    pub(crate) fn note_shell_exit_for_ledger(&mut self, session: u64) {
        let code = self.shell_exit_code(session);
        self.store
            .write()
            .unwrap_or_else(|p| p.into_inner())
            .note_exit_code(session, code);
    }

    /// The exited child's status code, from whichever party reaped it.
    ///
    /// `None` (`exit_code=-`): the child died by signal, is not this process's to reap (an
    /// adopted session), or was not yet a zombie at either look. The reader sees EOF when the
    /// child's last pty descriptor closes, which the kernel does before it retires the
    /// process, so the classifier's `WNOHANG` probe at `Wake::Exit` can find nothing; the
    /// ledger looks once more later in the same dispatch (`ExitLook::Retry`) rather than block
    /// the UI thread. On unix the reader posts the exit only once the shell is collectable
    /// (a bounded wait on the reader's thread), so neither look waits; elsewhere the one
    /// bounded wait is the first look's, for a shell that may have failed at start
    /// ([`crate::App::exit_status`]), and its answer stands for the ledger too.
    fn shell_exit_code(&self, session: u64) -> Option<i32> {
        use crate::app_tabs::ExitLook;
        use crate::session_status::{Outcome, Phase};
        // `tab_status` on (the default): `note_session_exit` already made the first look
        // and classified it. Read the code back from it; a second `waitpid` would answer
        // ECHILD. Only an `Exited` phase is that answer.
        let mut look = ExitLook::First;
        if let Some(status) = self.session_status.status(session)
            && matches!(status.phase, Phase::Exited)
        {
            match status.last_outcome {
                Outcome::Success => return Some(0),
                Outcome::Failure { exit_code } => return Some(exit_code),
                // Killed by a signal: no code, and the child is reaped.
                Outcome::Signal { .. } => return None,
                // The classifier's look found nothing to reap yet: this is the retry.
                Outcome::None => look = ExitLook::Retry,
            }
        }
        // `tab_status` off (this is the first look), or the classifier found nothing:
        // `exit_status` reads back an earlier look or reaps with the classifier's
        // reap-and-latch, so a `--hold` session never `killpg`s a reissued pid.
        match self.exit_status(session, look)? {
            aterm_pty::ChildExit::Code(code) => Some(code),
            aterm_pty::ChildExit::Signal(_) => None,
        }
    }
}

/// A fully-formed, `Alive` [`SessionHandle`] with a fresh sid, a headless engine and a real
/// (fd-less) [`SessionCtx`], for tests elsewhere (the subscribe tests) that drive the roster
/// journal through the real mutators.
#[cfg(test)]
pub(crate) fn test_handle(local_id: u64) -> SessionHandle {
    handle_alive(local_id, None, -1)
}

/// [`test_handle`] over a test's own pty master, so a test reads the sink's kernel probes
/// (`SinkWriter::input_backlog`). The caller owns and closes `master`.
#[cfg(test)]
pub(crate) fn test_handle_on(local_id: u64, master: i32) -> SessionHandle {
    handle_alive(local_id, None, master)
}

/// [`test_handle`] over a given engine, for a test that needs its own grid
/// size or fills the engine before the handle exists. It locks `term` once,
/// for the context's mode mirror.
#[cfg(test)]
pub(crate) fn test_handle_with(local_id: u64, term: Arc<Mutex<Terminal>>) -> SessionHandle {
    handle_over(
        local_id,
        None,
        term,
        aterm_session::sink::SinkWriter::new(-1),
    )
}

/// [`test_handle`] whose sink OWNS `master`, as a spawned session's sink owns
/// its PTY master: the fd closes when the last clone of the context's sink
/// drops, so a test sees from the other end what still holds the context.
#[cfg(all(test, unix))]
pub(crate) fn test_handle_owning(local_id: u64, master: std::os::fd::OwnedFd) -> SessionHandle {
    let term = Arc::new(Mutex::new(Terminal::new(24, 80)));
    handle_over(
        local_id,
        None,
        term,
        aterm_session::sink::SinkWriter::new_owned(master),
    )
}

#[cfg(test)]
fn handle_alive(local_id: u64, parent: Option<SessionId>, master: i32) -> SessionHandle {
    let term = Arc::new(Mutex::new(Terminal::new(24, 80)));
    handle_over(
        local_id,
        parent,
        term,
        aterm_session::sink::SinkWriter::new(master),
    )
}

#[cfg(test)]
fn handle_over(
    local_id: u64,
    parent: Option<SessionId>,
    term: Arc<Mutex<Terminal>>,
    sink: aterm_session::sink::SinkWriter,
) -> SessionHandle {
    use aterm_session::EdgeTable;
    crate::prime_test_timing();
    let sid = SessionId::generate();
    let nonce = LaunchNonce::generate();
    let master = sink.master();
    let observed = crate::session_watch::Observed::default();
    observed.watchers.bind_frame(&term);
    // The test constructor's log (`ep=1`, ruling U46), in the timeline that
    // rings this context's watchers.
    *observed
        .timeline
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .slog_mut() = crate::session_log::SessionLog::for_test();
    let ctx = Arc::new(SessionCtx {
        sink: Arc::new(sink),
        output_echo: Arc::new(crate::app_input::OutputEchoTracker::default()),
        modes: crate::mode_mirror_of(&term),
        ui_waiting: Arc::default(),
        edges: std::sync::Arc::new(Mutex::new(EdgeTable::new())),
        self_id: sid.clone(),
        nonce,
        turn_lease: Mutex::new(None),
        cast: Arc::new(Mutex::new(crate::cast::CastRecorder::new(80, 24))),
        temporal: Arc::new(Mutex::new(crate::temporal::TemporalRecorder::new())),
        byte_fanout: Arc::new(crate::cast::ByteFanout::new()),
        turns: observed.turns,
        meta: std::sync::Mutex::new(crate::session_timeline::SessionMeta::default()),
        timeline: observed.timeline,
        fabric: std::sync::Arc::default(),
        idem: Default::default(),
        rewrap_gauge: std::sync::Arc::default(),
        human_input: Default::default(),
        licence: Default::default(),
        launch: Default::default(),
        reset_lane: Default::default(),
        crash_journal: Default::default(),
        typed: Default::default(),
        bells: Default::default(),
        update_parked: Default::default(),
        watchers: observed.watchers,
        frame: Default::default(),
        witness: Default::default(),
        fenced_writes: Default::default(),
    });
    SessionHandle {
        sid,
        nonce,
        local_id,
        parent,
        state: SessionState::Alive,
        title: format!("tab-{local_id}"),
        term,
        master,
        ctx,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// THE LOG'S STATE WORDS MIRROR THE GUI'S (Phase 6 step 6F-2): every
    /// session state `state-change` names (`SessionState::as_str`, and the
    /// close's `closed`), every state an OSC 7501 report names and every kind
    /// a `blocked` one waits on are words of the log's closed lists, so a
    /// `state` or `program` mark never reads `unknown`. The matches are
    /// exhaustive: a state added here or in the engine does not compile until
    /// its word is placed. NEGATIVE CONTROL: a word the lists do not hold is
    /// refused by the field.
    #[test]
    fn the_state_and_program_words_are_the_logs() {
        use aterm_core::terminal::program_state::{ProgramBlockedKind, ProgramStateWord};
        use aterm_slog::vocab::{ASK_WORDS, STATE_WORDS};
        let sessions = [
            SessionState::Spawning,
            SessionState::Alive,
            SessionState::Exited,
        ];
        for s in sessions {
            match s {
                SessionState::Spawning | SessionState::Alive | SessionState::Exited => {}
            }
            assert!(STATE_WORDS.contains(&s.as_str()), "{:?}", s.as_str());
        }
        assert!(STATE_WORDS.contains(&"closed"), "the close's row");
        let states = [
            ProgramStateWord::Idle,
            ProgramStateWord::Working,
            ProgramStateWord::Blocked,
            ProgramStateWord::Done,
            ProgramStateWord::Error,
        ];
        for s in states {
            match s {
                ProgramStateWord::Idle
                | ProgramStateWord::Working
                | ProgramStateWord::Blocked
                | ProgramStateWord::Done
                | ProgramStateWord::Error => {}
            }
            assert!(STATE_WORDS.contains(&s.as_str()), "{:?}", s.as_str());
        }
        let asks = [
            ProgramBlockedKind::Permission,
            ProgramBlockedKind::Question,
            ProgramBlockedKind::Auth,
        ];
        for k in asks {
            match k {
                ProgramBlockedKind::Permission
                | ProgramBlockedKind::Question
                | ProgramBlockedKind::Auth => {}
            }
            assert!(ASK_WORDS.contains(&k.as_str()), "{:?}", k.as_str());
        }
        assert_eq!(ASK_WORDS.len(), asks.len(), "no ask word the engine lacks");
        // NEGATIVE CONTROL: a word no list holds is no field.
        assert!(
            aterm_slog::Fields::new()
                .word(aterm_slog::Key::State, "teleporting")
                .is_err()
        );
    }

    /// THE HOLD ORIGINS ARE THE LOG'S `origin=` WORDS (6F-2, U2): every
    /// origin a standing hold names (`Holds::standing`: the person's, the
    /// fleet's and the owner's slots, which a carry seeds, and a remote
    /// node's, which no carry writes) is an `ORIGIN_WORDS` word, and the
    /// list holds no word the slots lack. NEGATIVE CONTROL: a word the list
    /// does not hold is refused by `Key::Origin`.
    #[test]
    fn the_hold_origins_are_the_logs() {
        use aterm_slog::vocab::ORIGIN_WORDS;
        let carried = aterm_gui_fabric::parse_holds(["person a", "fleet b", "local c"]);
        let mut origins: Vec<String> = carried.standing().map(|h| h.origin).collect();
        assert_eq!(origins.len(), 3, "every carried slot stands: {origins:?}");
        origins.push(aterm_gui_fabric::REMOTE_ORIGIN.to_string());
        for o in &origins {
            assert!(ORIGIN_WORDS.contains(&o.as_str()), "{o}");
            assert!(
                aterm_slog::Fields::new()
                    .word(aterm_slog::Key::Origin, o)
                    .is_ok(),
                "{o}"
            );
        }
        assert_eq!(
            ORIGIN_WORDS.len(),
            origins.len(),
            "no origin word the slots lack: {origins:?}"
        );
        // NEGATIVE CONTROL: a word no slot names is no field.
        assert!(
            aterm_slog::Fields::new()
                .word(aterm_slog::Key::Origin, "teleport")
                .is_err()
        );
    }

    /// A SPAWN AND A CLOSE RING THE INSTANCE'S WATCHERS: a `sessions` or `@*`
    /// subscription learns of them at once. NEGATIVE CONTROL: a replace (the
    /// same sid registered again) is no membership change, journals nothing
    /// and rings nobody.
    #[test]
    fn a_spawn_wakes_instance_watchers() {
        use std::time::Duration;
        let mut store = SessionStore::default();
        let watcher = crate::session_watch::Subscription::single(&store.instance_watchers());
        let h = test_handle(3);
        store.register(h.clone());
        assert!(watcher.wait(Duration::ZERO), "a spawn rings the instance");
        store.register(h);
        assert!(!watcher.wait(Duration::ZERO), "a replace rings nobody");
        store.deregister_local_as(3, ExitReason::CtlClose, ExitActor::Unknown);
        assert!(watcher.wait(Duration::ZERO), "a close rings the instance");
    }

    /// AN EXIT WAKES THE SESSION'S WATCHERS BY ITSELF: `set_state` records the
    /// transition, and the record rings them, so a parked `await` reads
    /// `Exited` with no notify of its own from the exit path. NEGATIVE
    /// CONTROL: a re-mark of the same state records nothing and wakes nobody.
    #[test]
    fn a_state_change_wakes_the_sessions_watchers_and_a_re_mark_does_not() {
        use std::time::Duration;
        let mut store = SessionStore::default();
        let h = test_handle(4);
        let watcher = crate::session_watch::Subscription::single(&h.ctx.watchers);
        store.register(h);
        while watcher.wait(Duration::ZERO) {}
        store.set_state(4, SessionState::Alive);
        assert!(!watcher.wait(Duration::ZERO), "a re-mark rings nobody");
        store.set_state(4, SessionState::Exited);
        assert!(watcher.wait(Duration::ZERO), "the exit rings the session");
    }

    /// A carried render debt round-trips, and a bad row is no debt: not two
    /// integers, zero, past a screen, or `i32::MIN` (whose `abs` overflows).
    /// The columns owed ride their own key, so the rows key stays the
    /// `<shift>,<pushed>` an older reader reads.
    #[test]
    fn a_carried_render_debt_round_trips_and_a_bad_row_is_none() {
        let debt = |shift, pushed, cut| RenderDebt { shift, pushed, cut };
        for d in [
            debt(-1, 3, 0),
            debt(-1, 0, 2),
            debt(0, 0, 2),
            debt(4096, -4096, 4096),
        ] {
            let (row, cut) = render_debt_keys(Some(d));
            assert_eq!(parse_render_debt(row.as_deref(), cut), Some(d), "{d:?}");
        }
        assert_eq!(
            render_debt_keys(Some(debt(-1, 0, 3))),
            (Some("-1,0".to_string()), 3),
            "the rows as ever, the columns beside them"
        );
        assert_eq!(render_debt_keys(Some(debt(0, 0, 2))), (None, 2));
        assert_eq!(render_debt_keys(Some(RenderDebt::default())), (None, 0));
        assert_eq!(render_debt_keys(None), (None, 0));

        let rows = |row| parse_render_debt(Some(row), 0);
        assert_eq!(rows("-1,0"), Some(debt(-1, 0, 0)));
        assert_eq!(rows("1"), None);
        assert_eq!(rows("a,b"), None);
        assert_eq!(rows("0,0"), None, "owes nothing");
        assert_eq!(rows("4097,0"), None);
        assert_eq!(rows("-2147483648,0"), None);
        assert_eq!(rows("0,-2147483648"), None);
        assert_eq!(rows("0,0,2"), None, "three fields");
        assert_eq!(parse_render_debt(None, 0), None);
        assert_eq!(parse_render_debt(None, 4097), None);
        assert_eq!(parse_render_debt(Some("1,0"), 4097), None);
        assert_eq!(parse_render_debt(Some("bad"), 2), None, "a bad row is none");
    }

    /// A carried crash journal key round-trips; a bad one (not `<pid>:<id>`, or pid `0`) and
    /// an absent one read none.
    #[test]
    fn a_carried_journal_key_round_trips_and_a_bad_one_is_none() {
        let key = (u32::MAX, u64::MAX);
        assert_eq!(parse_journal_key(&journal_key_row(key)), Some(key));
        assert_eq!(parse_journal_key("4141:7"), Some((4141, 7)));
        for bad in ["", "4141", "0:7", "a:7", "4141:-1", "4141:7:1", " 4141:7"] {
            assert_eq!(parse_journal_key(bad), None, "{bad:?}");
        }
        let old = SessionHandoff::from_toml(
            "schema = 1\n\n[[sessions]]\nlocal_id = 0\nsid = \"s-0123456789abcdef0123\"\nstate = \
             \"alive\"\ntitle = \"zsh\"\n",
        )
        .expect("an older producer's manifest parses");
        assert_eq!(old.sessions[0].journal_key, None);
    }

    fn handle(local_id: u64, parent: Option<SessionId>) -> SessionHandle {
        handle_in_state(local_id, parent, SessionState::Alive)
    }

    fn handle_in_state(
        local_id: u64,
        parent: Option<SessionId>,
        state: SessionState,
    ) -> SessionHandle {
        let mut h = handle_alive(local_id, parent, -1);
        h.state = state;
        h
    }

    /// A manifest without `outgoing_build` reads as "predates" (every session frozen); with
    /// it, any number heals, since presence is the whole test. This build's manifest
    /// carries its own number and, per record, the shells it adopted frozen; `false` stays
    /// off the wire.
    #[test]
    fn the_frozen_path_carry_reads_absent_as_predates_and_rides_per_record() {
        assert!(
            predates_path_self_heal(None),
            "absent: the old build wrote it"
        );
        assert!(
            !predates_path_self_heal(Some(crate::running_build_number())),
            "a dev build of this tree ({}) wrote the field, so it heals",
            crate::running_build_number()
        );
        assert!(
            !predates_path_self_heal(Some(1_789_468_652)),
            "a number below the anchor with the field present still heals: presence is the test"
        );
        assert!(!predates_path_self_heal(Some(1_789_584_999)));
        assert!(!predates_path_self_heal(Some(1_789_585_000)));
        assert!(
            !predates_path_self_heal(Some(0)),
            "an unnumbered build carries the field"
        );

        // The old wire: no `outgoing_build`, no `frozen_path`.
        let old_wire = "schema = 1

[[sessions]]
local_id = 3
sid = \"s-old\"
state = \"alive\"
title = \"zsh\"
";
        let read = SessionHandoff::from_toml(old_wire).expect("this build reads an old manifest");
        assert_eq!(read.outgoing_build, None);
        assert!(predates_path_self_heal(read.outgoing_build));
        assert!(
            !read.sessions[0].frozen_path,
            "the per-record flag defaults off"
        );

        // This build's wire: its own number, and the frozen shell it adopted.
        let mut store = SessionStore::default();
        store.register(handle(0, None));
        store.register(handle(1, None));
        store.mark_frozen_path(1);
        let manifest = SessionHandoff::from_store(&store);
        assert_eq!(manifest.outgoing_build, Some(crate::running_build_number()));
        assert_eq!(
            manifest
                .sessions
                .iter()
                .map(|r| r.frozen_path)
                .collect::<Vec<_>>(),
            vec![false, true]
        );
        let wire = manifest.to_toml().expect("serializes");
        assert!(wire.contains("outgoing_build = "), "{wire}");
        assert_eq!(wire.matches("frozen_path = true").count(), 1, "{wire}");
        assert!(
            !wire.contains("frozen_path = false"),
            "false stays off the wire: {wire}"
        );
        assert!(manifest.roundtrips());

        // The count the row takes: registered, frozen, not exited; a close clears it.
        assert_eq!(store.frozen_path_tabs(), 1);
        store.mark_frozen_path(1);
        assert_eq!(store.frozen_path_tabs(), 1, "idempotent");
        store.note_exit_code(1, Some(0));
        assert_eq!(
            store.frozen_path_tabs(),
            0,
            "an exited shell is not a tab to exec in"
        );
        assert!(store.has_frozen_path(1), "but the fact is still carried");
        store.deregister_local(1);
        assert!(!store.has_frozen_path(1));
        assert_eq!(store.frozen_path_tabs(), 0);
        // An id marked before its registration counts once it is registered.
        store.mark_frozen_path(9);
        assert_eq!(store.frozen_path_tabs(), 0);
        store.register(handle(9, None));
        assert_eq!(store.frozen_path_tabs(), 1);
    }

    /// A local hold, a fleet hold and both together cross the handoff (records, TOML, the
    /// spawn's seed): each adopted session is exactly as halted as before (same holds, same
    /// `ERR halted`, same fleet fence against an Owner's `hold off`) with a timeline row saying
    /// the halt was carried. An unheld session stays unheld and writes no key.
    #[test]
    fn session_handoff_carries_a_hold() {
        use crate::fabric::{HOLD_DENIED, Hold, HoldIssuer, Holds, apply_hold_for_test, cmd_hold};
        let local = Hold {
            reason: "stop%20driving".to_string(),
            origin: "local".to_string(),
        };
        let fleet = Hold {
            reason: "fabric-lost".to_string(),
            origin: "fleet".to_string(),
        };
        let mut store = SessionStore::default();
        // Session 3 holds both: the owner's stop and the fleet's halt.
        let held: [&[&Hold]; 4] = [&[&local], &[&fleet], &[], &[&local, &fleet]];
        let mut before: Vec<Holds> = Vec::new();
        for (local_id, holds) in (0..).zip(held) {
            let h = handle(local_id, None);
            for hold in holds {
                assert!(apply_hold_for_test(&h.ctx, Some((*hold).clone())));
            }
            before.push(h.ctx.fabric.holds());
            store.register(h);
        }
        let manifest = SessionHandoff::from_store(&store);
        let rows: Vec<(Option<&str>, &[String])> = manifest
            .sessions
            .iter()
            .map(|r| (r.hold.as_deref(), r.more_holds.as_slice()))
            .collect();
        let both = ["local stop%20driving".to_string()];
        assert_eq!(
            rows,
            [
                (Some("local stop%20driving"), &[][..]),
                (Some("fleet fabric-lost"), &[][..]),
                (None, &[][..]),
                (Some("fleet fabric-lost"), &both[..]),
            ],
            "every standing hold is drawn into its record, either origin, the fleet's first"
        );
        let wire = manifest.to_toml().expect("serializes");
        assert_eq!(
            (
                wire.matches("hold = ").count(),
                wire.matches("more_holds = ").count()
            ),
            (3, 1),
            "no key for no hold, and the second only beside a first: {wire}"
        );
        assert!(manifest.roundtrips());

        let read = SessionHandoff::from_toml(&wire).expect("the successor reads it");
        let adopted_store: Store = new_store();
        for (rec, before) in read.sessions.iter().zip(&before) {
            // A fresh session, seeded by the spawn's own seam.
            let fresh = handle(rec.local_id, None);
            crate::spawn::seed_adopted_fabric(
                &fresh.ctx.fabric,
                &fresh.ctx.timeline,
                &rec.topics,
                &rec.hold_rows(),
                &rec.fabric,
            );
            assert_eq!(&fresh.ctx.fabric.holds(), before, "{rec:?}");
            let refused = crate::fabric::halt_refusal(&fresh.ctx, "send");
            assert_eq!(
                refused.is_some(),
                before.is_held(),
                "the dispatch gate answers exactly as it did before the update"
            );
            let carried_rows = fresh
                .ctx
                .timeline
                .lock()
                .unwrap()
                .since(None)
                .filter(|e| e.kind == "hold" && e.payload.ends_with(" carried=1"))
                .count();
            assert_eq!(carried_rows, usize::from(before.is_held()), "{rec:?}");
            adopted_store.write().unwrap().register(fresh);
        }
        // The origins are kept, so the fleet fence stands: the owner may lift its own carried
        // halt, never the fleet's.
        let sid_of = |local_id: u64| {
            adopted_store
                .read()
                .unwrap()
                .by_local(local_id)
                .map(|h| h.sid.as_str().to_string())
                .expect("registered")
        };
        let owner_off = |local_id: u64| {
            cmd_hold(
                &adopted_store,
                &format!("{} off", sid_of(local_id)),
                HoldIssuer::Owner,
            )
        };
        assert_eq!(
            owner_off(1),
            HOLD_DENIED,
            "a carried FLEET hold is not the owner's to lift"
        );
        assert_eq!(
            owner_off(0),
            "OK hold=0\n",
            "a carried LOCAL hold is, as it was"
        );
        assert_eq!(
            owner_off(3),
            "OK hold=1\n",
            "beside a carried fleet hold the owner lifts its own, and the fleet's stands"
        );

        // An older producer's record has no key: nothing is seeded.
        let old = SessionHandoff::from_toml(
            "schema = 1\n\n[[sessions]]\nlocal_id = 3\nsid = \"s-old\"\nstate = \"alive\"\ntitle = \"zsh\"\n",
        )
        .expect("an old manifest reads");
        assert_eq!(old.sessions[0].hold_rows(), Vec::<String>::new());
        // A record from before the two slots, with one hold, is exactly that hold and origin.
        let one = SessionHandoff::from_toml(
            "schema = 1\n\n[[sessions]]\nlocal_id = 3\nsid = \"s-old\"\nstate = \"alive\"\ntitle = \"zsh\"\nhold = \"local stop%20driving\"\n",
        )
        .expect("a one-hold manifest reads");
        let fresh = handle(3, None);
        crate::spawn::seed_adopted_fabric(
            &fresh.ctx.fabric,
            &fresh.ctx.timeline,
            &[],
            &one.sessions[0].hold_rows(),
            &one.sessions[0].fabric,
        );
        assert_eq!(fresh.ctx.fabric.holds(), before[0], "one hold, that origin");
    }

    /// A carried hold row outside the grammar still halts (`reason=carried-unreadable`,
    /// under the stronger origin it could have meant): dropping it would lift the halt. A
    /// value of the wrong TOML type costs neither the manifest nor the halt.
    #[test]
    fn an_unreadable_carried_hold_fails_closed() {
        use crate::fabric::{CARRIED_UNREADABLE, Hold, parse_hold, render_hold};
        let held = |reason: &str, origin: &str| Hold {
            reason: reason.to_string(),
            origin: origin.to_string(),
        };
        for (row, want) in [
            ("local stop%20now", held("stop%20now", "local")),
            ("fleet fabric-lost", held("fabric-lost", "fleet")),
            ("fleet -", held("-", "fleet")),
            // Over-long and hostile reasons are rebuilt as a bridge's would be.
            (
                &*format!("local {}", "r".repeat(400)),
                held(&"r".repeat(128), "local"),
            ),
            ("local a%2", held("a", "local")),
            // Outside the grammar: halted anyway.
            ("", held(CARRIED_UNREADABLE, "local")),
            ("fleet", held(CARRIED_UNREADABLE, "fleet")),
            ("fleet a b", held(CARRIED_UNREADABLE, "fleet")),
            ("mars now", held(CARRIED_UNREADABLE, "local")),
            ("local ", held(CARRIED_UNREADABLE, "local")),
            (" fleet x", held(CARRIED_UNREADABLE, "local")),
        ] {
            assert_eq!(parse_hold(row), want, "{row:?}");
        }
        for hold in [held("stop%20now", "local"), held("fabric-lost", "fleet")] {
            assert_eq!(parse_hold(&render_hold(&hold)), hold, "round trip");
        }
        // A value of the wrong type: the manifest still reads, and the row is unreadable.
        let odd = SessionHandoff::from_toml(
            "schema = 1\n\n[[sessions]]\nlocal_id = 3\nsid = \"s-odd\"\nstate = \"alive\"\ntitle = \"zsh\"\nhold = 5\n",
        )
        .expect("a hold of the wrong type does not cost the manifest");
        assert_eq!(odd.sessions[0].hold.as_deref(), Some(""));
        assert_eq!(parse_hold(""), held(CARRIED_UNREADABLE, "local"));
        // So is a `more_holds` member of the wrong type, or a value that is no list: never
        // dropped, since dropping a row lifts a halt.
        for (more, want) in [
            ("[\"local mine\", 5]", vec!["local mine", ""]),
            ("5", vec![""]),
            ("[]", vec![]),
        ] {
            let odd = SessionHandoff::from_toml(&format!(
                "schema = 1\n\n[[sessions]]\nlocal_id = 3\nsid = \"s-odd\"\nstate = \"alive\"\ntitle = \"zsh\"\nhold = \"fleet stop\"\nmore_holds = {more}\n",
            ))
            .expect("more_holds of the wrong type does not cost the manifest");
            assert_eq!(odd.sessions[0].more_holds, want, "{more}");
        }
    }

    /// Another supervisor's lease and every keyed escalation cross the handoff (records, TOML,
    /// seed):
    ///
    /// * s0: an external `ttl=` lease returns with its remaining time and three escalations in
    ///   order; the server's stall line stays behind;
    /// * s1: a connection's claim cannot cross: nothing seeded, not vouched for;
    /// * s2: this process's own host's lease stays behind for the successor's host;
    /// * s3 unclaimed and s4 lapsed: vouched for.
    ///
    /// Negative controls: unreadable rows seed nothing; wrong-typed values cost neither the
    /// manifest nor the grace.
    #[test]
    fn a_ttl_claim_and_keyed_attention_cross_the_handoff() {
        use crate::session_timeline::{
            MetaEdit, claim_supervisor, parse_carried_attention, parse_carried_supervisor,
            seed_carried_claims, write_attention_owned, write_server_attention,
        };
        let now = crate::metrics::now_us();
        let mut store = SessionStore::default();
        let s0 = handle(0, None);
        claim_supervisor(&s0.ctx, "watch-bob", None, Some(now + 60_000_000), now).unwrap();
        for (owner, text) in [
            ("-", "check the build"),
            ("supervisor", "claude box (Bash: rm -rf x) — needs you"),
            ("ci@host", "red 100%"),
        ] {
            write_attention_owned(&s0.ctx, owner, MetaEdit::Set(text), aterm_slog::Actor::Ctl)
                .unwrap();
        }
        // The newest says it is a wait: its kind crosses with it.
        crate::session_timeline::write_attention_kind(
            &s0.ctx,
            "ci@host",
            MetaEdit::Set("red 100%"),
            Some(aterm_types::needs::AttentionKind::Wait),
            aterm_slog::Actor::Ctl,
        )
        .unwrap();
        assert!(write_server_attention(
            &s0.ctx,
            crate::input_stall::SERVER_ATTENTION_OWNER,
            Some("claude has not read 3 keys".to_string())
        ));
        store.register(s0);
        let s1 = handle(1, None);
        claim_supervisor(&s1.ctx, "relay-x", Some(7), None, now).unwrap();
        store.register(s1);
        let s2 = handle(2, None);
        claim_supervisor(
            &s2.ctx,
            &crate::harness_host::holder(),
            None,
            Some(now + 90_000_000),
            now,
        )
        .unwrap();
        store.register(s2);
        store.register(handle(3, None));
        let s4 = handle(4, None);
        claim_supervisor(&s4.ctx, "gone", None, Some(now.saturating_sub(1)), now).unwrap();
        store.register(s4);

        let manifest = SessionHandoff::from_store(&store);
        let r = &manifest.sessions;
        let (holder, ms) = r[0]
            .supervisor
            .as_deref()
            .and_then(|row| row.split_once(' '))
            .expect("s0's lease is carried");
        let ms: u64 = ms.parse().unwrap();
        assert_eq!(holder, "watch-bob");
        assert!((50_000..=60_000).contains(&ms), "{ms}");
        assert!(r[0].claim_known);
        assert_eq!(
            r[0].attention_owners,
            [
                "- check%20the%20build",
                "supervisor claude%20box%20(Bash:%20rm%20-rf%20x)%20%E2%80%94%20needs%20you",
                "ci@host kind=wait red%20100%25",
            ],
            "every owner's line in write order, with the kind its writer named, \
             the server's own left behind"
        );
        let claims: Vec<(Option<&str>, bool, bool)> = r
            .iter()
            .map(|rec| {
                (
                    rec.supervisor.as_deref(),
                    rec.claim_known,
                    rec.claim_grace(),
                )
            })
            .collect();
        assert_eq!(
            claims[1..],
            [
                (None, false, true),
                (None, true, false),
                (None, true, false),
                (None, true, false)
            ],
            "a connection's claim is unvouched; our own host's, none and a lapsed \
             lease are vouched for"
        );
        assert!(r[0].claim_grace(), "a carried lease is waited on too");
        let wire = manifest.to_toml().expect("serializes");
        assert_eq!(wire.matches("\nsupervisor = ").count(), 1, "{wire}");
        assert_eq!(wire.matches("\nclaim_known = true").count(), 4, "{wire}");
        assert!(manifest.roundtrips());

        // The successor, a moment later.
        let read = SessionHandoff::from_toml(&wire).expect("the successor reads it");
        let later = now + 2_000_000;
        let adopted: Vec<SessionHandle> = read
            .sessions
            .iter()
            .map(|rec| {
                let fresh = handle(rec.local_id, None);
                seed_carried_claims(
                    &fresh.ctx.meta,
                    &fresh.ctx.timeline,
                    rec.supervisor.as_deref(),
                    &rec.attention_owners,
                    later,
                );
                fresh
            })
            .collect();
        {
            let meta = adopted[0].ctx.meta.lock().unwrap();
            assert_eq!(meta.live_supervisor(later), Some("watch-bob"));
            let expiry = meta.supervisor_expiry().expect("a lease");
            assert_eq!(
                expiry,
                later + ms * 1000,
                "its remaining time, from the seed"
            );
            assert_eq!(meta.attention.as_deref(), Some("red 100%"));
            assert_eq!(
                meta.attention_owners.effective_owner(),
                Some("ci@host"),
                "the most recent line is the one shown, as before the update"
            );
            assert_eq!(
                meta.attention_owners.get("supervisor"),
                Some("claude box (Bash: rm -rf x) — needs you")
            );
            assert_eq!(meta.attention_owners.get("-"), Some("check the build"));
            assert_eq!(
                meta.attention_owners.kinds_field(),
                "ask:-,-:supervisor,wait:ci@host",
                "a wait is still a wait in the successor"
            );
            assert_eq!(
                meta.attention_owners
                    .get(crate::input_stall::SERVER_ATTENTION_OWNER),
                None
            );
            assert_eq!(meta.driver_writes, 0, "a seed is no driver's write");
        }
        // The successor's host is refused as it was in the parent, and the
        // holder's renewal renews.
        assert_eq!(
            claim_supervisor(
                &adopted[0].ctx,
                "aterm-harness@1",
                None,
                Some(later + 90_000_000),
                later
            ),
            Err("watch-bob".to_string())
        );
        assert_eq!(
            claim_supervisor(
                &adopted[0].ctx,
                "watch-bob",
                None,
                Some(later + 90_000_000),
                later
            ),
            Ok(false)
        );
        let seeded_rows: Vec<String> = adopted[0]
            .ctx
            .timeline
            .lock()
            .unwrap()
            .since(None)
            .filter(|e| e.kind == "meta-change")
            .map(|e| e.payload.to_string())
            .collect();
        assert_eq!(
            seeded_rows,
            [
                "field=attention value=red%20100%25",
                "field=supervisor value=watch-bob"
            ]
        );
        for fresh in &adopted[1..] {
            let meta = fresh.ctx.meta.lock().unwrap();
            assert_eq!(meta.live_supervisor(later), None, "nothing else is seeded");
            assert!(meta.attention_owners.is_empty());
        }

        // A producer from before the carry: nothing seeded, every session
        // waited on.
        let old = SessionHandoff::from_toml(
            "schema = 1\n\n[[sessions]]\nlocal_id = 3\nsid = \"s-old\"\nstate = \"alive\"\ntitle = \"zsh\"\n",
        )
        .expect("an old manifest reads");
        assert_eq!(old.sessions[0].supervisor, None);
        assert!(old.sessions[0].attention_owners.is_empty());
        assert!(old.sessions[0].claim_grace());

        // Negative controls: rows the verbs would refuse seed nothing.
        for (row, want) in [
            ("watch-bob 5000", Some(("watch-bob", 5000))),
            ("watch-bob 99999999", Some(("watch-bob", 600_000))),
            ("watch-bob 0", None),
            ("watch-bob -5", None),
            ("watch-bob soon", None),
            ("- 5000", None),
            ("bad holder! 5000", None),
            ("", None),
            ("watch-bob", None),
            (" watch-bob 5", None),
        ] {
            assert_eq!(parse_carried_supervisor(row), want, "{row:?}");
        }
        use aterm_types::needs::AttentionKind::{Ask, Wait};
        for (row, want) in [
            ("ci red", Some(("ci", None, "red".to_string()))),
            (
                "ci x%1B%5B31mred%E2%80%AEx",
                Some(("ci", None, "x[31mredx".to_string())),
            ),
            // The kind its writer named crosses; a kind outside the set, or
            // on the bare owner's row, is dropped and the entry kept.
            (
                "ci kind=wait limited:%20resets%207pm",
                Some(("ci", Some(Wait), "limited: resets 7pm".to_string())),
            ),
            (
                "ci kind=ask red",
                Some(("ci", Some(Ask), "red".to_string())),
            ),
            ("ci kind=later red", Some(("ci", None, "red".to_string()))),
            ("- kind=wait red", Some(("-", None, "red".to_string()))),
            // A kind-less entry whose text opens `kind=` is all text.
            ("ci kind=wait", Some(("ci", None, "kind=wait".to_string()))),
            ("aterm stalled", None),
            ("bad%20owner text", None),
            ("ci ", None),
            ("ci %0A%09", None),
            ("ci kind=wait %0A", None),
            ("ci", None),
        ] {
            assert_eq!(parse_carried_attention(row), want, "{row:?}");
        }
        let (long, bare) = (
            format!("ci {}", "a".repeat(400)),
            format!("- {}", "a".repeat(400)),
        );
        assert_eq!(
            parse_carried_attention(&long).unwrap().2.len(),
            crate::session_timeline::META_ATTENTION_KEYED_MAX
        );
        assert_eq!(
            parse_carried_attention(&bare).unwrap().2.len(),
            crate::session_timeline::META_ATTENTION_MAX
        );
        // Past the owners' bound, a new owner is refused as a live write is.
        let crowded = handle(9, None);
        let rows: Vec<String> = (0..12).map(|i| format!("owner{i} line{i}")).collect();
        seed_carried_claims(&crowded.ctx.meta, &crowded.ctx.timeline, None, &rows, later);
        assert_eq!(
            crowded.ctx.meta.lock().unwrap().attention_owners.len(),
            crate::session_timeline::ATTENTION_OWNERS_MAX - 1,
            "the bare owner's slot is kept"
        );
        // Values of the wrong type cost neither the manifest nor the grace.
        let odd = SessionHandoff::from_toml(
            "schema = 1\n\n[[sessions]]\nlocal_id = 3\nsid = \"s-odd\"\nstate = \"alive\"\n\
             title = \"zsh\"\nsupervisor = 5\nclaim_known = \"yes\"\nattention_owners = [1, \"ci x\"]\n",
        )
        .expect("rows of the wrong type do not cost the manifest");
        assert_eq!(odd.sessions[0].supervisor.as_deref(), Some(""));
        assert!(!odd.sessions[0].claim_known);
        assert_eq!(odd.sessions[0].attention_owners, ["ci x"]);
        assert!(odd.sessions[0].claim_grace());
        let not_a_list = SessionHandoff::from_toml(
            "schema = 1\n\n[[sessions]]\nlocal_id = 3\nsid = \"s-odd\"\nstate = \"alive\"\n\
             title = \"zsh\"\nattention_owners = \"ci x\"\nclaim_known = true\n",
        )
        .expect("a list that is no list does not cost the manifest");
        assert!(not_a_list.sessions[0].attention_owners.is_empty());
        assert!(!not_a_list.sessions[0].claim_grace());
    }

    /// Conformance to `harness_handoff_claim_model`: each pre-Commit claim (none, an external
    /// lease, a connection's claim, the outgoing host's own lease), from this build or an older
    /// producer, is driven through the real chain (claim, `from_store`, TOML, `from_toml`, the
    /// seed, the grace decision) and must match the model after `Commit`. Negative control: the
    /// mutant `CommitUncarried` breaks `NoTakeover` wherever an external holder was.
    #[test]
    fn the_real_claim_carry_conforms_to_the_handoff_claim_model() {
        use crate::session_timeline::{claim_supervisor, seed_carried_claims};
        let model = aterm_models_harness::harness_handoff_claim_model();
        let buggy = aterm_spec::interp::with_buggy(&model, 1);
        let grace = model.consts.iter().find(|c| c.0 == "Grace").unwrap().1;
        let new_keys = ["supervisor = ", "claim_known = ", "attention_owners = "];
        let mut driven = 0;
        for pre in ["", "ExternalLease", "ExternalConn", "HostHeld"] {
            for older in [false, true] {
                // The model's side.
                let mut state = model.init_state();
                let mut schedule: Vec<&str> = [pre].into_iter().filter(|a| !a.is_empty()).collect();
                if older {
                    schedule.push("OlderParent");
                }
                schedule.extend(["Park", "Commit"]);
                for action in &schedule {
                    assert!(model.fire(action, &mut state), "{action}: {state:?}");
                }

                // The real side: the claim as the outgoing instance held it.
                let now = crate::metrics::now_us();
                let live = handle(0, None);
                match pre {
                    "ExternalLease" => {
                        claim_supervisor(&live.ctx, "watch-bob", None, Some(now + 60_000_000), now)
                            .unwrap();
                    }
                    "ExternalConn" => {
                        claim_supervisor(&live.ctx, "relay-x", Some(7), None, now).unwrap();
                    }
                    "HostHeld" => {
                        claim_supervisor(
                            &live.ctx,
                            &crate::harness_host::holder(),
                            None,
                            Some(now + 90_000_000),
                            now,
                        )
                        .unwrap();
                    }
                    _ => {}
                }
                let mut store = SessionStore::default();
                store.register(live);
                let mut wire = SessionHandoff::from_store(&store)
                    .to_toml()
                    .expect("serializes");
                if older {
                    wire = wire
                        .lines()
                        .filter(|line| !new_keys.iter().any(|key| line.starts_with(key)))
                        .map(|line| format!("{line}\n"))
                        .collect();
                }
                let read = SessionHandoff::from_toml(&wire).expect("the successor reads it");
                let rec = &read.sessions[0];
                let fresh = handle(0, None);
                let later = now + 1_000_000;
                seed_carried_claims(
                    &fresh.ctx.meta,
                    &fresh.ctx.timeline,
                    rec.supervisor.as_deref(),
                    &rec.attention_owners,
                    later,
                );
                let shown = fresh
                    .ctx
                    .meta
                    .lock()
                    .unwrap()
                    .live_supervisor(later)
                    .map(str::to_owned);
                let server = match shown.as_deref() {
                    None => 0,
                    Some(holder) if holder == crate::harness_host::holder() => 2,
                    Some(_) => 1,
                };
                let real_grace = if rec.claim_grace() { grace } else { 0 };
                assert_eq!(
                    (server, real_grace),
                    (state["server"], state["grace"]),
                    "{schedule:?}: the successor's state is the model's after Commit\n{wire}"
                );
                // The successor's host could claim right now exactly where the
                // model's `HostClaims` is enabled.
                let host_may = claim_supervisor(
                    &fresh.ctx,
                    "aterm-harness@2",
                    None,
                    Some(later + 90_000_000),
                    later,
                )
                .is_ok()
                    && !rec.claim_grace();
                assert_eq!(
                    host_may,
                    model.action_enabled("HostClaims", &state),
                    "{schedule:?}"
                );

                // Negative control: a successor that seeds nothing and waits no grace.
                let mut shipped = buggy.init_state();
                for action in schedule.iter().map(|a| {
                    if *a == "Commit" {
                        "CommitUncarried"
                    } else {
                        *a
                    }
                }) {
                    assert!(buggy.fire(action, &mut shipped), "{action}: {shipped:?}");
                }
                assert_eq!((shipped["server"], shipped["grace"]), (0, 0));
                assert!(buggy.fire("HostClaims", &mut shipped));
                assert_eq!(
                    buggy.check_invariant("NoTakeover", &shipped),
                    matches!(pre, "" | "HostHeld"),
                    "{schedule:?}: the old successor takes exactly the externally held sessions"
                );
                driven += 1;
            }
        }
        assert_eq!(driven, 8, "every pre-Commit configuration of the machine");
    }

    /// Retired named identities follow the normal unknown-field policy: old
    /// handoffs remain readable without carrying the removed field forward.
    #[test]
    fn a_retired_identity_field_is_read_past() {
        let old_wire = "schema = 1

[[sessions]]
local_id = 3
sid = \"s-old\"
state = \"alive\"
title = \"zsh\"
identity = \"worker\"
";
        let read = SessionHandoff::from_toml(old_wire).expect("this build reads an old manifest");
        assert_eq!(read.sessions.len(), 1);
        assert_eq!(read.sessions[0].local_id, 3);
        assert_eq!(read.sessions[0].sid, "s-old");
        let wire = read.to_toml().expect("serializes");
        assert!(!wire.contains("identity ="), "{wire}");
        let back = SessionHandoff::from_toml(&wire).expect("round-trips");
        assert_eq!(back, read);
    }

    /// THE TASK RECORD rides per record (the worker pillar, 2026-10-15): a
    /// manifest from before the field reads none, and so does a value of the
    /// wrong type; a worker's record projects as its one row and only its,
    /// crosses the wire, is in the draw the Commit compares — a mark between
    /// the park and the Commit stands the Commit down — and seeds the adopting
    /// session's timeline with the same rev, state, facts, since and the agent
    /// its spawn started (whose reader the startup deadline reads it by, and
    /// whose product its stall names). A row that does not read seeds
    /// nothing. Unix's: the Commit's draw ([`CarriedClaims`]) is.
    #[cfg(unix)]
    #[test]
    fn the_task_record_rides_per_record_and_absent_reads_none() {
        use std::time::Duration;

        use crate::task_record::{
            MarkFenced, TaskMark, TaskState, mark_session_task, mark_session_task_within,
            seed_carried,
        };
        let old_wire = "schema = 1

[[sessions]]
local_id = 3
sid = \"s-old\"
state = \"alive\"
title = \"zsh\"
";
        let read = SessionHandoff::from_toml(old_wire).expect("this build reads an old manifest");
        assert_eq!(read.sessions[0].task, None, "absent reads None");
        let typo = SessionHandoff::from_toml(&format!("{old_wire}task = 5\n"))
            .expect("a wrong-typed row costs the row, never the manifest");
        let unread = handle(7, None);
        seed_carried(&unread.ctx, typo.sessions[0].task.as_deref());
        assert!(
            unread.ctx.timeline.lock().unwrap().task().is_none(),
            "and seeds nothing"
        );

        let mut store = SessionStore::default();
        store.register(handle(0, None));
        let worker = handle(1, None);
        mark_session_task(&worker.ctx, 1, &TaskMark::new(TaskState::Working)).expect("unfenced");
        let folder = "/Users//me/work proj";
        worker
            .ctx
            .timeline
            .lock()
            .unwrap()
            .set_task_worker(aterm_phase::Program::Gemini, Some(folder.to_string()));
        let mut done = TaskMark::new(TaskState::Done);
        done.why = Some("marker");
        done.result_bytes = 5;
        let (record, _) = mark_session_task(&worker.ctx, 1, &done).expect("unfenced");
        store.register(worker.clone());
        let manifest = SessionHandoff::from_store(&store);
        assert_eq!(manifest.sessions[0].task, None);
        let row = manifest.sessions[1].task.clone().expect("the worker's row");
        assert!(row.starts_with("rev=2 state=done from=ctl since="), "{row}");
        assert!(
            row.ends_with(&format!(
                " agent=gemini trusted_cwd={}",
                crate::control::pct_encode(folder)
            )),
            "a mark keeps the agent and the trusted folder: {row}"
        );
        assert_eq!(record.agent, Some(aterm_phase::Program::Gemini));
        assert_eq!(record.trusted_cwd.as_deref(), Some(folder));
        let wire = manifest.to_toml().expect("serializes");
        assert_eq!(
            wire.matches("\ntask = ").count(),
            1,
            "None stays off the wire: {wire}"
        );
        assert!(manifest.roundtrips());
        let back = SessionHandoff::from_toml(&wire).expect("round-trips");
        assert_eq!(back.sessions[1].task.as_deref(), Some(row.as_str()));

        let now = crate::metrics::now_us();
        let holder = crate::harness_host::holder();
        let drawn = CarriedClaims::of_record(&manifest.sessions[1]);
        assert_eq!(
            CarriedClaims::of_session(1, &worker.ctx, now, &holder),
            drawn
        );

        let adopted = handle(8, None);
        seed_carried(&adopted.ctx, back.sessions[1].task.as_deref());
        let seeded = adopted.ctx.timeline.lock().unwrap().task().cloned();
        let seeded = seeded.expect("seeded");
        assert_eq!(
            crate::task_record::TaskRecord {
                since: record.since,
                ..seeded
            },
            record,
            "the same record, its since re-anchored"
        );
        let seeded_folder = |h: &SessionHandle| {
            h.ctx
                .timeline
                .lock()
                .unwrap()
                .task()
                .and_then(|t| t.trusted_cwd.clone())
        };
        assert_eq!(seeded_folder(&adopted), Some(folder.to_string()));
        // NEGATIVE CONTROLS: a folder this build cannot vouch for — relative,
        // climbing, a second spelling, a control character — crosses as none,
        // the record's other facts with it; a row from before the field
        // carries none.
        let carried = back.sessions[1].task.as_deref().expect("the row");
        let encoded = crate::control::pct_encode(folder);
        for bad in [
            "work".to_string(),
            crate::control::pct_encode("/Users//me/../etc"),
            crate::control::pct_encode("/Users//me/"),
            "/Users//me/a%0Ab".to_string(),
        ] {
            let other = handle(10, None);
            seed_carried(&other.ctx, Some(&carried.replace(&encoded, &bad)));
            let seeded = other.ctx.timeline.lock().unwrap().task().cloned();
            let seeded = seeded.unwrap_or_else(|| panic!("{bad}: the record crosses"));
            assert_eq!(seeded.trusted_cwd, None, "{bad}");
            assert_eq!(seeded.agent, Some(aterm_phase::Program::Gemini), "{bad}");
        }
        let other = handle(11, None);
        seed_carried(
            &other.ctx,
            Some(&carried.replace(&format!(" trusted_cwd={encoded}"), "")),
        );
        assert!(other.ctx.timeline.lock().unwrap().task().is_some());
        assert_eq!(seeded_folder(&other), None, "a row from before the field");
        // The attention flag is the SEEDED META's, never the row's: a row
        // saying `attention=1` over a meta whose supervisor entry the claims
        // seed refused reports no news.
        let raised = back.sessions[1]
            .task
            .as_deref()
            .expect("the row")
            .replace(" attention=0", " attention=1");
        let unclaimed = handle(9, None);
        seed_carried(&unclaimed.ctx, Some(&raised));
        let seeded = unclaimed.ctx.timeline.lock().unwrap().task().cloned();
        assert!(!seeded.expect("seeded").attention, "no entry, no flag");

        mark_session_task(&worker.ctx, 1, &TaskMark::new(TaskState::Stalled)).expect("unfenced");
        assert_ne!(
            CarriedClaims::of_session(1, &worker.ctx, now, &holder),
            drawn,
            "a mark after the park's draw stands the Commit down"
        );
        // UNDER THE COMMIT'S FENCE a mark waits (here, not at all) and moves
        // nothing, so the draw the Commit compares is the one the successor
        // carries; once the attempt stands down and lifts it, the mark lands.
        let fenced = CarriedClaims::of_session(1, &worker.ctx, now, &holder);
        let _ = worker.ctx.fabric.fence_hold(false);
        let done = TaskMark::new(TaskState::Done);
        assert_eq!(
            mark_session_task_within(&worker.ctx, 1, &done, Duration::ZERO),
            Err(MarkFenced)
        );
        assert_eq!(
            CarriedClaims::of_session(1, &worker.ctx, now, &holder),
            fenced
        );
        worker.ctx.fabric.lift_hold_fence();
        let landed = mark_session_task_within(&worker.ctx, 1, &done, Duration::ZERO);
        assert!(landed.is_ok_and(|(_, moved)| moved), "lifted, it lands");
    }

    /// The re-key channel rides per record: absent reads `false`, only a marked session
    /// writes `rekey = true`, and a deregister forgets the mark.
    #[test]
    fn the_rekey_channel_rides_per_record_and_absent_reads_false() {
        let old_wire = "schema = 1

[[sessions]]
local_id = 3
sid = \"s-old\"
state = \"alive\"
title = \"zsh\"
";
        let read = SessionHandoff::from_toml(old_wire).expect("this build reads an old manifest");
        assert!(!read.sessions[0].rekey, "absent reads false");

        let mut store = SessionStore::default();
        store.register(handle(0, None));
        store.register(handle(1, None));
        store.mark_rekey_channel(1);
        let manifest = SessionHandoff::from_store(&store);
        assert_eq!(
            manifest
                .sessions
                .iter()
                .map(|r| r.rekey)
                .collect::<Vec<_>>(),
            vec![false, true]
        );
        let wire = manifest.to_toml().expect("serializes");
        assert_eq!(
            wire.matches("rekey").count(),
            1,
            "false stays off the wire: {wire}"
        );
        assert!(manifest.roundtrips());
        let back = SessionHandoff::from_toml(&wire).expect("round-trips");
        assert!(back.sessions[1].rekey);
        store.deregister_local(1);
        assert!(!store.has_rekey_channel(1), "a closed session forgets it");
    }

    /// The loader rides per record like the re-key channel: absent reads `false`, `false`
    /// stays off the wire, and a deregister forgets it and the frozen mark.
    #[test]
    fn the_loader_rides_per_record_and_absent_reads_false() {
        let old_wire = "schema = 1

[[sessions]]
local_id = 3
sid = \"s-old\"
state = \"alive\"
title = \"zsh\"
";
        let read = SessionHandoff::from_toml(old_wire).expect("this build reads an old manifest");
        assert!(!read.sessions[0].loader, "absent reads false");

        let mut store = SessionStore::default();
        store.register(handle(0, None));
        store.register(handle(1, None));
        store.mark_body_loader(1);
        store.mark_integration_frozen(0);
        let manifest = SessionHandoff::from_store(&store);
        assert_eq!(
            manifest
                .sessions
                .iter()
                .map(|r| r.loader)
                .collect::<Vec<_>>(),
            vec![false, true]
        );
        let wire = manifest.to_toml().expect("serializes");
        assert_eq!(
            wire.matches("loader").count(),
            1,
            "false stays off the wire: {wire}"
        );
        assert!(manifest.roundtrips());
        let back = SessionHandoff::from_toml(&wire).expect("round-trips");
        assert!(back.sessions[1].loader);
        assert!(store.is_integration_frozen(0));
        store.deregister_local(1);
        store.deregister_local(0);
        assert!(!store.has_body_loader(1), "a closed session forgets it");
        assert!(!store.is_integration_frozen(0), "and the frozen mark");
    }

    #[test]
    fn register_indexes_both_key_spaces_and_deregister_clears_both() {
        let mut store = SessionStore::default();
        let h = handle(7, None);
        let sid = h.sid.clone();
        store.register(h);

        assert_eq!(store.len(), 1);
        assert!(store.by_sid(&sid).is_some(), "resolvable by sid");
        assert!(store.by_local(7).is_some(), "resolvable by local id");
        assert_eq!(store.by_local(7).unwrap().sid, sid, "both keys agree");

        // Deregister clears both key spaces and yields the stable sid.
        assert_eq!(store.deregister_local(7), Some(sid.clone()));
        assert!(store.by_sid(&sid).is_none(), "sid index cleared");
        assert!(store.by_local(7).is_none(), "local index cleared");
        assert!(store.is_empty());
        // A second deregister is a no-op (late/duplicate close).
        assert_eq!(store.deregister_local(7), None);
    }

    #[test]
    fn snapshot_is_sorted_by_local_id_and_carries_parent() {
        let mut store = SessionStore::default();
        let root = handle(0, None);
        let root_sid = root.sid.clone();
        store.register(root);
        store.register(handle(2, Some(root_sid.clone())));
        store.register(handle(1, Some(root_sid.clone())));

        let snap = store.snapshot();
        assert_eq!(
            snap.iter().map(|h| h.local_id).collect::<Vec<_>>(),
            vec![0, 1, 2]
        );
        assert_eq!(snap[0].parent, None, "root has no parent");
        assert_eq!(
            snap[1].parent.as_ref(),
            Some(&root_sid),
            "child links to root"
        );
    }

    #[test]
    fn session_handoff_carries_additive_resource_identity_without_changing_sessions() {
        let mut store = SessionStore::default();
        store.register(handle(0, None));
        let mut manifest = SessionHandoff::from_store(&store);
        manifest.resources = Some(aterm_json::json!({
            "boot": vec![7; 16],
            "jobs": [{ "pid": 42, "start": 123456789, "session": "s-a", "ai": "codex",
                       "label": "tree:compiler", "members": [[42, 123456789], [43, 123456790]] }]
        }));
        let parsed = SessionHandoff::from_toml(&manifest.to_toml().unwrap()).unwrap();
        assert_eq!(parsed.resources, manifest.resources);
        assert_eq!(parsed.sessions, manifest.sessions);
        manifest.resources = None;
        assert!(
            SessionHandoff::from_toml(&manifest.to_toml().unwrap())
                .unwrap()
                .resources
                .is_none()
        );
    }

    /// RFC Rung 1a: the real `SessionHandoff` serializer round-trips a
    /// multi-session store EXACTLY — no session lost, none fabricated, order +
    /// parent links preserved.
    #[test]
    fn session_handoff_roundtrips_the_whole_set() {
        let mut store = SessionStore::default();
        let root = handle(0, None);
        let root_sid = root.sid.clone();
        store.register(root);
        let peer = handle_in_state(2, Some(root_sid.clone()), SessionState::Alive);
        let peer_sid = peer.sid.clone();
        let peer_ctx = peer.ctx.clone();
        store.register(peer);
        store.register(handle_in_state(
            1,
            Some(root_sid.clone()),
            SessionState::Exited,
        ));
        store.set_title(1, "vim");
        // A live `both` connection root → peer (three rows in the peer's table) rides the
        // manifest as tokenless triples.
        {
            let mut edges = peer_ctx.edges.lock().unwrap();
            let minted = edges.grant_connection(
                &root_sid,
                &peer_sid,
                aterm_session::ConnectionKind::Both,
                &peer_ctx.nonce,
            );
            assert_eq!(minted.len(), 3, "both mints its exact op set");
        }

        let manifest = SessionHandoff::from_store(&store);
        assert_eq!(manifest.sessions.len(), 3, "every live session is packed");
        // The carry is the `edges()` enumeration as sorted wire spellings, and nothing else.
        let carry = |op: &str| ConnectionCarry {
            src: root_sid.as_str().to_string(),
            dst: peer_sid.as_str().to_string(),
            op: op.to_string(),
        };
        assert_eq!(
            manifest.connections,
            vec![carry("read-screen"), carry("signal"), carry("write-input")],
            "the three rows one `both` mints, tokenless and sorted"
        );
        assert!(
            manifest.roundtrips(),
            "serialize -> parse must be the identity"
        );

        let text = manifest.to_toml().unwrap();
        // No secrets at rest: an `EdgeToken` is 64 hex chars, and no run that wide may appear
        // in the wire (sids are 20 hex).
        assert!(
            !text
                .as_bytes()
                .windows(64)
                .any(|w| w.iter().all(u8::is_ascii_hexdigit)),
            "no bearer-token-width hex blob may ride the manifest: {text}"
        );
        let restored = SessionHandoff::from_toml(&text).expect("parse");
        assert_eq!(restored, manifest);
        assert_eq!(
            restored.connections, manifest.connections,
            "the tokenless triples round-trip verbatim"
        );
        assert_eq!(
            restored
                .sessions
                .iter()
                .map(|r| r.local_id)
                .collect::<Vec<_>>(),
            vec![0, 1, 2],
            "tab order preserved"
        );
        assert_eq!(
            restored.sessions[1].parent.as_deref(),
            Some(root_sid.as_str())
        );
        assert_eq!(restored.sessions[1].state, "exited");
        assert_eq!(restored.sessions[1].title, "vim");
        assert_eq!(
            SessionState::from_str(&restored.sessions[1].state),
            SessionState::Exited
        );

        // A corrupt or wrong-schema blob parses to None.
        assert!(SessionHandoff::from_toml("not = [valid").is_none());
        assert!(SessionHandoff::from_toml("schema = 999\nsessions = []").is_none());
    }

    #[test]
    fn mark_alive_is_monotonic_idempotent_and_fail_safe() {
        let mut store = SessionStore::default();
        store.register(handle_in_state(8, None, SessionState::Spawning));

        // A second readiness signal (duplicate or late `Wake::Ready`) is a no-op.
        assert!(store.mark_alive(8));
        assert!(!store.mark_alive(8), "already Alive: no second transition");
        assert_eq!(store.by_local(8).unwrap().state, SessionState::Alive);

        // An instant-exit shell: a late readiness never resurrects an Exited handle.
        store.register(handle_in_state(9, None, SessionState::Exited));
        assert!(!store.mark_alive(9), "Exited never flips back to Alive");
        assert_eq!(store.by_local(9).unwrap().state, SessionState::Exited);

        // Unknown id is a fail-closed no-op, never a panic.
        assert!(!store.mark_alive(404));
    }

    /// The store mutators record the lifecycle events (`spawned` on first registration,
    /// `state-change`, pct-encoded `title-change`, the closing `state-change`) and nothing
    /// for a no-op.
    #[test]
    fn store_mutators_record_ordered_timeline_events() {
        let mut store = SessionStore::default();
        let h = handle_in_state(4, None, SessionState::Spawning);
        let timeline = h.ctx.timeline.clone();
        store.register(h.clone());
        // Re-registering the same sid (a replace) is not a second birth.
        store.register(h);
        assert!(store.mark_alive(4));
        assert!(!store.mark_alive(4), "no-op re-mark records nothing");
        // The store's title records nothing; since phase 6 (6F-8, ruling
        // U13) the `title-change` row is the drift flush's, its title mark's.
        store.set_title(4, "vim main.rs");
        assert!(timeline.lock().unwrap().record_title("vim main.rs"));
        assert!(
            !timeline.lock().unwrap().record_title("vim main.rs"),
            "unchanged: nothing recorded"
        );
        store.set_state(4, SessionState::Exited);
        store.set_state(4, SessionState::Exited); // unchanged: nothing recorded
        store.deregister_local(4);

        let tl = timeline.lock().unwrap();
        let got: Vec<(u64, &str, &str)> = tl
            .since(None)
            .map(|e| (e.id, e.kind, &*e.payload))
            .collect();
        assert_eq!(
            got,
            vec![
                (1, "spawned", "state=spawning"),
                (2, "state-change", "state=alive"),
                (3, "title-change", "title=vim%20main.rs"),
                (4, "state-change", "state=exited"),
                // An `Exited` session's deregister says why before it says it is gone.
                (5, "closing", "reason=shell-exit by=-"),
                (6, "state-change", "state=closed"),
            ],
            "one ordered, monotonic-id event per ACTUAL lifecycle change"
        );
    }

    /// The journal records exactly the membership transitions `by_id` performs (one
    /// `Created` per first registration, one `Exited` per real removal) with gap-free seqs,
    /// and replaying it from 0 reconstructs `live_sids()` after every step.
    #[test]
    fn roster_journal_mirrors_membership_exactly() {
        let mut store = SessionStore::default();
        assert_eq!(store.roster_seq(), 0, "a virgin store has no history");
        assert_eq!(store.roster_low_seq(), None);
        assert_eq!(store.roster_since(0).count(), 0);

        let replay = |st: &SessionStore| -> std::collections::HashSet<String> {
            let mut set = std::collections::HashSet::new();
            for r in st.roster_since(0) {
                match r.change {
                    RosterChange::Created => {
                        set.insert(r.sid.clone());
                    }
                    RosterChange::Exited => {
                        set.remove(&r.sid);
                    }
                    RosterChange::RetireRequested | RosterChange::Bye(_) => {}
                }
            }
            set
        };

        let a = handle(1, None);
        let b = handle(2, None);
        let (sid_a, sid_b) = (a.sid.as_str().to_string(), b.sid.as_str().to_string());
        store.register(a.clone());
        assert_eq!(replay(&store), store.live_sids());
        // A replace of the same sid is not a membership change.
        store.register(a);
        assert_eq!(store.roster_seq(), 1, "a replace journals nothing");
        store.register(b);
        assert_eq!(replay(&store), store.live_sids());
        // A late/duplicate deregister journals nothing either.
        store.deregister_local(1);
        assert_eq!(store.deregister_local(1), None);
        assert_eq!(
            store.roster_seq(),
            3,
            "the duplicate deregister journalled nothing"
        );
        assert_eq!(replay(&store), store.live_sids());

        let got: Vec<(u64, &str, RosterChange)> = store
            .roster_since(0)
            .map(|r| (r.seq, r.sid.as_str(), r.change))
            .collect();
        assert_eq!(
            got,
            vec![
                (1, sid_a.as_str(), RosterChange::Created),
                (2, sid_b.as_str(), RosterChange::Created),
                (3, sid_a.as_str(), RosterChange::Exited),
            ]
        );
        // `since` is a suffix keyed on the watermark, at every position.
        assert_eq!(store.roster_since(1).count(), 2);
        assert_eq!(store.roster_since(3).count(), 0);
        assert_eq!(store.roster_since(99).count(), 0, "past the high = nothing");
        assert_eq!(store.roster_low_seq(), Some(1));
    }

    /// Past the cap the journal drops oldest, `roster_seq` keeps counting, and
    /// `roster_low_seq` names where the retained window starts.
    #[test]
    fn roster_journal_is_bounded_and_advertises_its_low_water() {
        let mut store = SessionStore::default();
        // Two membership changes per iteration: the journal fills past the cap.
        for i in 0..(ROSTER_JOURNAL_CAP as u64) {
            store.register(handle(i, None));
            store.deregister_local(i);
        }
        let total = 2 * ROSTER_JOURNAL_CAP as u64;
        assert_eq!(
            store.roster_seq(),
            total,
            "seqs keep counting past eviction"
        );
        assert_eq!(
            store.roster_since(0).count(),
            ROSTER_JOURNAL_CAP,
            "retention is capped"
        );
        assert_eq!(
            store.roster_low_seq(),
            Some(total - ROSTER_JOURNAL_CAP as u64 + 1),
            "the low-water names the oldest retained seq"
        );
        assert!(
            store.is_empty(),
            "every session registered was deregistered"
        );
    }

    /// The timeline of a handle's ctx, as `(kind, payload)` pairs, oldest-first.
    fn timeline_of(h: &SessionHandle) -> Vec<(&'static str, String)> {
        h.ctx
            .timeline
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .since(None)
            .map(|e| (e.kind, e.payload.to_string()))
            .collect()
    }

    /// An attributed deregister journals reason, actor, `t_ms` and the noted exit code, and
    /// records `closing` on the session's timeline just before the death mark.
    #[test]
    fn attributed_deregister_records_reason_actor_clock_and_exit_code() {
        let mut store = SessionStore::default();
        let h = handle(7, None);
        let sid = h.sid.as_str().to_string();
        store.register(h.clone());
        let before = crate::turn_ledger::now_ms();
        store.note_exit_code(7, Some(3));
        assert_eq!(
            store.deregister_local_as(7, ExitReason::CtlClose, ExitActor::Sid("s-caller".into())),
            Some(h.sid.clone())
        );
        let rec = store
            .roster_since(0)
            .find(|r| r.change == RosterChange::Exited)
            .expect("the exit was journalled")
            .clone();
        assert_eq!(rec.sid, sid);
        assert_eq!(rec.local_id, 7);
        assert_eq!(rec.reason, ExitReason::CtlClose);
        assert_eq!(rec.actor, ExitActor::Sid("s-caller".into()));
        assert_eq!(rec.exit_code, Some(3));
        assert!(
            rec.t_ms >= before && rec.t_ms <= crate::turn_ledger::now_ms(),
            "stamped on the process-epoch clock: {} not in [{before}, now]",
            rec.t_ms
        );
        // A `Created` row carries the neutral values, never an invented reason.
        let created = store
            .roster_since(0)
            .find(|r| r.change == RosterChange::Created)
            .expect("the birth was journalled");
        assert_eq!(
            (created.reason, created.exit_code, &created.actor),
            (ExitReason::Unknown, None, &ExitActor::Unknown)
        );
        // Spawned, then `closing` with the reason, then the death mark.
        let tl = timeline_of(&h);
        assert_eq!(tl[0].0, "spawned");
        assert_eq!(
            &tl[1..],
            &[
                ("closing", "reason=ctl-close by=s-caller".to_string()),
                ("state-change", "state=closed".to_string()),
            ]
        );
    }

    /// An unattributed deregister of an `Exited` session is a shell exit; a live one stays
    /// `Unknown` and records no `closing` event.
    #[test]
    fn unattributed_deregister_infers_shell_exit_from_the_exited_state_only() {
        let mut store = SessionStore::default();
        let exited = handle_in_state(1, None, SessionState::Exited);
        let alive = handle(2, None);
        store.register(exited.clone());
        store.register(alive.clone());
        store.deregister_local(1);
        store.deregister_local(2);
        let reasons: Vec<(String, ExitReason, ExitActor)> = store
            .roster_since(0)
            .filter(|r| r.change == RosterChange::Exited)
            .map(|r| (r.sid.clone(), r.reason, r.actor.clone()))
            .collect();
        assert_eq!(
            reasons,
            vec![
                (
                    exited.sid.as_str().to_string(),
                    ExitReason::ShellExit,
                    ExitActor::Unknown
                ),
                (
                    alive.sid.as_str().to_string(),
                    ExitReason::Unknown,
                    ExitActor::Unknown
                ),
            ]
        );
        assert!(
            timeline_of(&exited)
                .iter()
                .any(|(k, p)| *k == "closing" && p == "reason=shell-exit by=-"),
            "the inferred shell exit is a known reason: {:?}",
            timeline_of(&exited)
        );
        assert!(
            !timeline_of(&alive).iter().any(|(k, _)| *k == "closing"),
            "an unknown reason records no closing event: {:?}",
            timeline_of(&alive)
        );
    }

    /// An explicit reason outranks the `Exited` state: a human closing a `--hold` pane is a
    /// `ui-close`.
    #[test]
    fn an_explicit_reason_outranks_the_exited_state() {
        let mut store = SessionStore::default();
        store.register(handle_in_state(4, None, SessionState::Exited));
        store.deregister_local_as(4, ExitReason::UiClose, ExitActor::Human);
        let rec = store
            .roster_since(0)
            .find(|r| r.change == RosterChange::Exited)
            .unwrap();
        assert_eq!(
            (rec.reason, &rec.actor),
            (ExitReason::UiClose, &ExitActor::Human)
        );
    }

    /// A noted exit code is consumed by the deregister that follows and never reaches a
    /// later session reusing the local id; a `None` note records nothing.
    #[test]
    fn a_noted_exit_code_is_consumed_exactly_once() {
        let mut store = SessionStore::default();
        store.register(handle(9, None));
        store.note_exit_code(9, None);
        store.deregister_local(9);
        store.register(handle(9, None));
        store.note_exit_code(9, Some(7));
        store.deregister_local(9);
        store.register(handle(9, None));
        store.deregister_local(9);
        let codes: Vec<Option<i32>> = store
            .roster_since(0)
            .filter(|r| r.change == RosterChange::Exited)
            .map(|r| r.exit_code)
            .collect();
        assert_eq!(codes, vec![None, Some(7), None]);
    }

    /// Past the cap the oldest exits drop, the newest survive with their attribution, and
    /// `since=<seq>` yields exactly the newer rows (the `exits` verb's cursor).
    #[test]
    fn attributed_exits_stay_bounded_and_page_by_seq() {
        let mut store = SessionStore::default();
        let n = ROSTER_JOURNAL_CAP as u64 + 5;
        for i in 0..n {
            store.register(handle(i, None));
            store.note_exit_code(i, Some(i32::try_from(i % 256).unwrap()));
            store.deregister_local_as(i, ExitReason::CtlClose, ExitActor::Sid(format!("s-{i}")));
        }
        let exits: Vec<&RosterRecord> = store
            .roster_since(0)
            .filter(|r| r.change == RosterChange::Exited)
            .collect();
        // Two rows per session; the retained window holds the newest CAP rows,
        // half of which are exits.
        assert_eq!(exits.len(), ROSTER_JOURNAL_CAP / 2);
        let newest = exits.last().unwrap();
        assert_eq!(newest.seq, store.roster_seq());
        assert_eq!(newest.actor, ExitActor::Sid(format!("s-{}", n - 1)));
        assert_eq!(
            newest.exit_code,
            Some(i32::try_from((n - 1) % 256).unwrap())
        );
        // `since=<seq of the third-newest exit>` → exactly the two newer exits.
        let third = exits[exits.len() - 3].seq;
        let newer: Vec<u64> = store
            .roster_since(third)
            .filter(|r| r.change == RosterChange::Exited)
            .map(|r| r.seq)
            .collect();
        assert_eq!(newer, vec![exits[exits.len() - 2].seq, newest.seq]);
        assert_eq!(store.roster_since(newest.seq).count(), 0, "caught up");
    }

    /// The close-attribution scope: absent is `Unknown`, the outermost initiator wins over an
    /// inner claim, and dropping the opener restores `Unknown`.
    #[test]
    fn close_attribution_scope_is_outermost_wins_and_restores() {
        assert_eq!(
            current_close_attribution(),
            (ExitReason::Unknown, ExitActor::Unknown)
        );
        {
            let _outer =
                CloseAttribution::enter(ExitReason::CtlClose, ExitActor::Sid("s-a".into()));
            {
                let _inner = CloseAttribution::enter(ExitReason::UiClose, ExitActor::Human);
                assert_eq!(
                    current_close_attribution(),
                    (ExitReason::CtlClose, ExitActor::Sid("s-a".into())),
                    "the inner claim does not override the initiator"
                );
            }
            assert_eq!(
                current_close_attribution().0,
                ExitReason::CtlClose,
                "dropping the inner (non-opener) guard leaves the scope open"
            );
        }
        assert_eq!(
            current_close_attribution(),
            (ExitReason::Unknown, ExitActor::Unknown),
            "dropping the opener closes the scope"
        );
    }

    /// Every reason and actor has its documented wire token.
    #[test]
    fn exit_reason_and_actor_wire_tokens() {
        let tokens: Vec<&str> = ExitReason::ALL.iter().map(|r| r.as_str()).collect();
        assert_eq!(
            tokens,
            [
                "shell-exit",
                "ctl-close",
                "ui-close",
                "window-close",
                "app-quit",
                "unknown"
            ]
        );
        assert_eq!(ExitActor::Sid("s-z".into()).as_wire(), "s-z");
        assert_eq!(ExitActor::Human.as_wire(), "human");
        assert_eq!(ExitActor::Ctl.as_wire(), "ctl");
        assert_eq!(ExitActor::Bridge.as_wire(), "bridge");
        assert_eq!(ExitActor::Unknown.as_wire(), "-");
    }
}
