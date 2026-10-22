// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! When to keyframe (the crash-restore plan §2.5): a pure state machine the parse
//! thread runs and hands the time (milliseconds of a monotonic clock it reads itself).
//!
//! A keyframe rebases the tail, so the policy trades a keyframe's cost under the
//! terminal lock (9 µs for a shell's visible screen, about 100 µs with a 512-line
//! bridge) against how much a restore must replay and how fast the tail ring fills:
//!
//! | rule | trigger | rate |
//! |---|---|---|
//! | K1 quiescence | dirty, and no batch for 200 ms | once per burst; an idle session's tail is then empty, and its restore exact |
//! | K2 periodic | dirty, and ≥ 1 s since the last keyframe | ≤ 1/s |
//! | K3 tail pressure | the tail since the keyframe ≥ half the ring, ≥ 100 ms since the last keyframe | ≤ 10/s, the bridge capped at 256 lines |
//! | K4 forced | a clear (ED3, RIS) pending, the config fingerprint changed, or the host renumbered the history with no output (an import attached older history) | rare, never rate-limited; after a clear's keyframe the writer settles the clear (`JournalWriter::settle_clear`) |
//! | K5 deferred | the capture returned `None`: the parser holds a sequence no keyframe carries whole (a hooked DCS query, an over-cap OSC, an APC or a picture under way), or an OSC 133/633 mark whose nonce the carry would hold | retried at the next slice end, never on a quiet wait: the session parks |
//! | K6 verified mark | the slice verified an OSC 133/633 mark | shares K3's ≤ 10/s budget; the marks are counted until a keyframe commits either way |
//! | K7 regridded | a resize changed the width or grew the rows | shares K3's ≤ 10/s budget and bridge; K1 covers the end of a drag |
//!
//! K6 exists because a restore replays the tail into an engine that authorizes no
//! nonce (a restored shell re-keys), where a verified mark would be dropped: a keyframe
//! taken right after the mark's slice holds the mark's effect, so the tail almost never
//! holds one for long. The marks are not the policy's to count: the parse thread counts
//! every slice's verified marks (`JournalWriter::note_marks`) BEFORE that slice's append,
//! and the next keyframe's commit clears the count. A writer that dies anywhere between
//! the mark's append and that commit (the K6 keyframe's own encode and copy, a budget the
//! gap left, a capture K5 deferred) therefore leaves the mark counted, and the restore
//! says it was not exact rather than lose the prompt's state silently.
//!
//! K7 is not in the plan's table. A width change or a rows grow replayed from the tail
//! depends on engine state no checkpoint carries (where the history sits, ring or store,
//! and the rows a resize undo stashed), so the recovery counts one and the restore is not
//! exact (`Losses::regrids_replayed`); the engine conformance test found both cases.
//! Keyframing soon after such a resize keeps that window short without a keyframe per
//! drag event: a window drag resizes about 60 times a second.
//!
//! The parse thread waits for its next batch with a 200 ms timeout ONLY while the
//! session is [`dirty`](KeyframePolicy::dirty): an idle session wakes zero times. A
//! deferred keyframe (K5) does not keep it waking: what refused the capture is the
//! parser's state, which only new bytes (or a host reset, which arrives as bytes) can
//! move, so retrying on a quiet wait would fail again five times a second, each try
//! under the terminal lock, for as long as a program sat inside an unterminated query
//! (`cat` of a binary holding `ESC P + q`, a program that crashed mid-query). So a
//! deferral PARKS the session: it reads clean to the wait until the next slice or
//! resize reaches the tail, whose end retries the keyframe. The tail it holds meanwhile
//! is what a restore replays, and its unterminated sequence is counted
//! (`Losses::swallowed`). A keyframe the slot refused for size is not parser-bound: its
//! retry with no bridge ([`KeyframePolicy::keyframe_retry`]) waits for the next quiet.

/// No batch for this long makes a dirty session quiescent (K1).
pub const QUIET_MS: u64 = 200;
/// A dirty session keyframes at least this often (K2).
pub const PERIOD_MS: u64 = 1_000;
/// The least gap between two keyframes under tail pressure or for a mark (K3, K6).
pub const PRESSURE_GAP_MS: u64 = 100;
/// The bridge a keyframe carries normally: about 100 µs under the lock.
pub const BRIDGE_CAP: usize = 512;
/// The bridge under tail pressure (K3): about 50 µs, at up to ten keyframes a second.
pub const PRESSURE_BRIDGE_CAP: usize = 256;

/// Why a keyframe is due.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Reason {
    /// K1: the burst ended.
    Quiescence,
    /// K2: a second of output since the last keyframe.
    Periodic,
    /// K3: the tail since the keyframe filled half the ring.
    TailPressure,
    /// K4: a clear (ED3, RIS) is pending. Once this keyframe commits, the parse thread
    /// settles the clear (`JournalWriter::settle_clear`): the other slot, the newest
    /// slot past its record and the tail lose every byte from before the clear.
    Clear,
    /// K4: the config fingerprint changed, which a replay must not cross.
    Forced,
    /// K4: the host renumbered the history with no output to follow (an import attached
    /// older history below it, which advances the fence's renumber epoch): the newest
    /// keyframe names the old identity, under which the history ring, re-exported under the
    /// new one, would never join again until the program next wrote.
    Renumbered,
    /// K6: the slice verified a shell-integration mark.
    Mark,
    /// K7: a resize changed the width or grew the rows, which a replay does not reproduce
    /// exactly.
    Regrid,
}

impl Reason {
    /// The bridge cap a keyframe for this reason is captured with.
    #[inline]
    #[must_use]
    pub const fn bridge_cap(self) -> usize {
        match self {
            Self::TailPressure | Self::Regrid => PRESSURE_BRIDGE_CAP,
            Self::Quiescence
            | Self::Periodic
            | Self::Clear
            | Self::Forced
            | Self::Renumbered
            | Self::Mark => BRIDGE_CAP,
        }
    }
}

/// What the parse thread does after a slice or a quiet wait.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Decision {
    /// A keyframe is due, for this reason (its bridge cap is
    /// [`Reason::bridge_cap`]).
    pub keyframe: Option<Reason>,
}

/// The keyframe policy of one session.
#[derive(Debug, Clone)]
pub struct KeyframePolicy {
    /// The tail ring's capacity (K3 weighs the tail against half of it).
    tail_cap: u64,
    /// Something reached the tail since the last keyframe.
    dirty: bool,
    /// When the last batch was processed.
    last_batch_ms: u64,
    /// When the last keyframe was written (`None` before the first).
    last_keyframe_ms: Option<u64>,
    /// A clear is pending its forced keyframe (K4).
    clear_pending: bool,
    /// The config fingerprint the last keyframe was taken under, and the current one.
    config_fp_at_keyframe: u64,
    config_fp: u64,
    /// The keyframe the capture deferred (K5), retried at the next slice end.
    deferred: Option<Reason>,
    /// A width change or a rows grow reached the tail since the last keyframe (K7).
    regrid_pending: bool,
    /// A K5 deferral parked the session (module doc): it reads clean to the parse
    /// thread's wait until the next slice or resize reaches the tail.
    parked: bool,
    /// The host renumbered the history with no output ([`Reason::Renumbered`]).
    renumbered: bool,
}

impl KeyframePolicy {
    /// A policy for a tail ring of `tail_cap` bytes whose first keyframe was taken at
    /// `now_ms` under config `config_fp`.
    #[must_use]
    pub fn new(tail_cap: usize, now_ms: u64, config_fp: u64) -> Self {
        Self {
            tail_cap: u64::try_from(tail_cap).unwrap_or(u64::MAX),
            dirty: false,
            last_batch_ms: now_ms,
            last_keyframe_ms: Some(now_ms),
            clear_pending: false,
            config_fp_at_keyframe: config_fp,
            config_fp,
            deferred: None,
            regrid_pending: false,
            parked: false,
            renumbered: false,
        }
    }

    /// Whether the session has tail past its keyframe that a quiet wait can keyframe: the
    /// parse thread then waits for its next batch with a [`QUIET_MS`] timeout, and
    /// otherwise blocks. A session a K5 deferral parked is not dirty here, though its tail
    /// is past its keyframe (module doc).
    #[inline]
    #[must_use]
    pub const fn dirty(&self) -> bool {
        self.dirty && !self.parked
    }

    /// A clear (ED3, RIS) reset the history ring: the next slice end or quiet wait forces
    /// a keyframe ([`Reason::Clear`]), after which the writer settles the clear.
    #[inline]
    pub const fn note_clear(&mut self) {
        self.clear_pending = true;
    }

    /// The host renumbered the engine's history with no output (an import attached older
    /// history): a keyframe is forced at the next slice end or quiet wait
    /// ([`Reason::Renumbered`]).
    #[inline]
    pub const fn note_renumbered(&mut self) {
        self.renumbered = true;
    }

    /// The host changed the engine-relevant config (stored as `config_fp`): a keyframe
    /// is forced at the next slice end or quiet wait.
    #[inline]
    pub const fn note_config(&mut self, config_fp: u64) {
        self.config_fp = config_fp;
    }

    /// A resize reached the tail (the journal lane's nudge, or the catch-up before a
    /// slice): the session is dirty from `now_ms`, so a quiet keyframe follows at the new
    /// geometry; a resize that changed the width or grew the rows (`regridded`) makes a K7
    /// keyframe due as soon as the pressure gap allows.
    #[inline]
    pub const fn note_resize(&mut self, now_ms: u64, regridded: bool) {
        self.dirty = true;
        self.parked = false;
        self.last_batch_ms = now_ms;
        self.regrid_pending |= regridded;
    }

    #[inline]
    const fn since_keyframe(&self, now_ms: u64) -> u64 {
        match self.last_keyframe_ms {
            Some(at) => now_ms.saturating_sub(at),
            None => u64::MAX,
        }
    }

    /// A forced or deferred keyframe, which no rate applies to. A pending clear comes
    /// first, so the keyframe its settle follows is never reported as another reason.
    #[inline]
    const fn owed(&self) -> Option<Reason> {
        if self.clear_pending {
            Some(Reason::Clear)
        } else if self.config_fp != self.config_fp_at_keyframe {
            Some(Reason::Forced)
        } else if self.renumbered {
            Some(Reason::Renumbered)
        } else {
            self.deferred
        }
    }

    /// One slice was processed and appended at `now_ms`: `tail_since_keyframe` is the
    /// writer's tail since its newest keyframe, `marks_verified` how far the slice moved
    /// `Terminal::shell_marks_verified` (which the caller counted before the append).
    pub fn after_slice(
        &mut self,
        now_ms: u64,
        tail_since_keyframe: u64,
        marks_verified: u64,
    ) -> Decision {
        self.dirty = true;
        self.parked = false;
        self.last_batch_ms = now_ms;
        if let Some(reason) = self.owed() {
            return Decision {
                keyframe: Some(reason),
            };
        }
        let gap_ok = self.since_keyframe(now_ms) >= PRESSURE_GAP_MS;
        let keyframe = if gap_ok && marks_verified > 0 {
            Some(Reason::Mark)
        } else if gap_ok && self.regrid_pending {
            Some(Reason::Regrid)
        } else if gap_ok && tail_since_keyframe >= self.tail_cap / 2 {
            Some(Reason::TailPressure)
        } else if self.since_keyframe(now_ms) >= PERIOD_MS {
            Some(Reason::Periodic)
        } else {
            None
        };
        Decision { keyframe }
    }

    /// The parse thread's wait for its next batch timed out at `now_ms` (it waits only
    /// while [`dirty`](Self::dirty)).
    pub fn on_quiet(&mut self, now_ms: u64) -> Decision {
        let keyframe = if let Some(reason) = self.owed() {
            Some(reason)
        } else if self.dirty && now_ms.saturating_sub(self.last_batch_ms) >= QUIET_MS {
            Some(Reason::Quiescence)
        } else {
            None
        };
        Decision { keyframe }
    }

    /// A keyframe was written at `now_ms`: the tail is rebased past everything so far,
    /// and the clear and config it was forced for are settled.
    #[inline]
    pub const fn keyframe_written(&mut self, now_ms: u64) {
        self.dirty = false;
        self.last_keyframe_ms = Some(now_ms);
        self.clear_pending = false;
        self.config_fp_at_keyframe = self.config_fp;
        self.deferred = None;
        self.regrid_pending = false;
        self.parked = false;
        self.renumbered = false;
    }

    /// The capture for `reason` returned `None` (K5: the parser holds a sequence no
    /// keyframe carries whole, such as a hooked DCS query, an over-cap OSC or an APC under
    /// way, or sits inside an OSC 133/633 mark whose nonce the carry would hold): it is
    /// retried at the next slice end. The session PARKS until then: no quiet wait retries
    /// it, as nothing but new bytes moves the parser (module doc). The marks the deferred
    /// keyframe would have held stay counted until a later one commits.
    #[inline]
    pub const fn keyframe_deferred(&mut self, reason: Reason) {
        self.deferred = Some(reason);
        self.parked = true;
    }

    /// The keyframe for `reason` was captured and not written for a cause the next try
    /// can clear by itself (a slot too small for its bridge, which the writer drops for
    /// the retry): it is retried at the next slice end or quiet wait, and the session
    /// stays dirty.
    #[inline]
    pub const fn keyframe_retry(&mut self, reason: Reason) {
        self.deferred = Some(reason);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const CAP: usize = 256 * 1024;

    /// K1: a burst followed by 200 ms of quiet keyframes once, and an idle session never.
    #[test]
    fn a_quiet_burst_keyframes_once_and_an_idle_session_never() {
        let mut p = KeyframePolicy::new(CAP, 900, 7);
        assert!(!p.dirty(), "an idle session blocks with no timeout");
        assert_eq!(p.on_quiet(10_000).keyframe, None, "idle: no keyframe, ever");
        assert_eq!(p.after_slice(1_000, 100, 0).keyframe, None);
        assert!(p.dirty());
        assert_eq!(p.on_quiet(1_150).keyframe, None, "still inside the burst");
        assert_eq!(p.on_quiet(1_200).keyframe, Some(Reason::Quiescence));
        p.keyframe_written(1_200);
        assert!(!p.dirty());
        assert_eq!(p.on_quiet(5_000).keyframe, None, "once per burst");
    }

    /// K2: a session streaming steadily keyframes once a second, never more often.
    #[test]
    fn a_stream_keyframes_once_a_second() {
        let mut p = KeyframePolicy::new(CAP, 0, 7);
        let mut keyframes = Vec::new();
        for ms in (10..=3_000).step_by(10) {
            if let Some(reason) = p.after_slice(ms, 10, 0).keyframe {
                assert_eq!(reason, Reason::Periodic);
                keyframes.push(ms);
                p.keyframe_written(ms);
            }
        }
        assert_eq!(keyframes, vec![1_000, 2_000, 3_000]);
    }

    /// K3: a flood that fills half the ring keyframes with the small bridge, at most
    /// ten times a second.
    #[test]
    fn a_flood_keyframes_under_pressure_at_most_ten_times_a_second() {
        let mut p = KeyframePolicy::new(CAP, 0, 7);
        let half = (CAP / 2) as u64;
        assert_eq!(
            p.after_slice(50, half, 0).keyframe,
            None,
            "inside the 100 ms gap"
        );
        let d = p.after_slice(100, half, 0);
        assert_eq!(d.keyframe, Some(Reason::TailPressure));
        assert_eq!(Reason::TailPressure.bridge_cap(), PRESSURE_BRIDGE_CAP);
        p.keyframe_written(100);
        let mut count = 0;
        for ms in (105..=1_100).step_by(5) {
            if p.after_slice(ms, half, 0).keyframe.is_some() {
                count += 1;
                p.keyframe_written(ms);
            }
        }
        assert_eq!(count, 10, "ten a second");
    }

    /// K4: a clear and a config change force a keyframe at once, whatever the rate;
    /// the keyframe settles them. A clear pending beside a config change is reported as
    /// the clear, whose keyframe the writer's settle must follow.
    #[test]
    fn a_clear_or_a_config_change_forces_a_keyframe() {
        let mut p = KeyframePolicy::new(CAP, 0, 7);
        p.note_config(9);
        p.note_clear();
        assert_eq!(
            p.on_quiet(0).keyframe,
            Some(Reason::Clear),
            "the clear first"
        );
        p.keyframe_written(0);
        p.note_config(7);
        p.keyframe_written(0);
        p.note_clear();
        assert_eq!(p.after_slice(1, 0, 0).keyframe, Some(Reason::Clear));
        p.keyframe_written(1);
        assert_eq!(p.after_slice(2, 0, 0).keyframe, None);
        p.note_config(8);
        assert_eq!(p.on_quiet(3).keyframe, Some(Reason::Forced), "even quiet");
        p.keyframe_written(3);
        assert_eq!(p.after_slice(4, 0, 0).keyframe, None);
        p.note_config(8);
        assert_eq!(
            p.after_slice(5, 0, 0).keyframe,
            None,
            "the same config is no change"
        );
    }

    /// K5: a deferred keyframe is retried at the next slice end until it is written, and
    /// meanwhile the session is parked: the parse thread's wait blocks instead of retrying
    /// on every quiet, since only new bytes move the parser out of what refused the
    /// capture. A resize unparks it too. A keyframe the slot refused for size is retried
    /// at the next quiet, unparked.
    #[test]
    fn a_deferred_keyframe_is_retried_at_the_next_slice_end() {
        let mut p = KeyframePolicy::new(CAP, 0, 7);
        p.after_slice(10, 0, 0);
        assert_eq!(p.on_quiet(210).keyframe, Some(Reason::Quiescence));
        p.keyframe_deferred(Reason::Quiescence);
        assert!(
            !p.dirty(),
            "parked: an idle session inside a query wakes zero times"
        );
        assert_eq!(p.after_slice(220, 0, 0).keyframe, Some(Reason::Quiescence));
        assert!(p.dirty(), "the slice unparked it");
        p.keyframe_deferred(Reason::Quiescence);
        p.note_resize(230, false);
        assert!(p.dirty(), "a resize unparks it");
        assert_eq!(p.on_quiet(430).keyframe, Some(Reason::Quiescence));
        p.keyframe_written(430);
        assert_eq!(p.after_slice(440, 0, 0).keyframe, None);
        p.keyframe_retry(Reason::Quiescence);
        assert!(p.dirty(), "a retry for size stays awake");
        assert_eq!(p.on_quiet(450).keyframe, Some(Reason::Quiescence));
    }

    /// K6: a verified mark keyframes when the budget allows; inside the budget it waits
    /// for the next keyframe due (the writer counts its marks meanwhile).
    #[test]
    fn a_verified_mark_keyframes_when_the_budget_allows() {
        let mut p = KeyframePolicy::new(CAP, 0, 7);
        assert_eq!(p.after_slice(150, 0, 1).keyframe, Some(Reason::Mark));
        p.keyframe_written(150);
        assert_eq!(
            p.after_slice(160, 0, 2).keyframe,
            None,
            "inside the budget: no keyframe"
        );
        assert_eq!(p.on_quiet(360).keyframe, Some(Reason::Quiescence));
    }

    /// K7: a width change or a rows grow makes a keyframe due at the next slice end the pressure gap
    /// allows, with the small bridge.
    #[test]
    fn a_regrid_keyframes_soon_after() {
        let mut p = KeyframePolicy::new(CAP, 0, 7);
        p.note_resize(1_000, true);
        assert_eq!(p.after_slice(1_010, 10, 0).keyframe, Some(Reason::Regrid));
        assert_eq!(Reason::Regrid.bridge_cap(), PRESSURE_BRIDGE_CAP);
        p.keyframe_written(1_010);
        p.note_resize(1_020, true);
        assert_eq!(p.after_slice(1_030, 10, 0).keyframe, None, "inside the gap");
        assert_eq!(p.after_slice(1_110, 10, 0).keyframe, Some(Reason::Regrid));
        p.keyframe_written(1_110);
        p.note_resize(1_120, false);
        assert_eq!(
            p.after_slice(1_300, 10, 0).keyframe,
            None,
            "a shrink: no K7"
        );
    }

    /// A resize with no output makes the session dirty, so a quiet keyframe follows at
    /// the new geometry.
    #[test]
    fn a_resize_with_no_output_is_keyframed_when_quiet() {
        let mut p = KeyframePolicy::new(CAP, 0, 7);
        p.note_resize(1_000, false);
        assert!(p.dirty());
        assert_eq!(p.on_quiet(1_100).keyframe, None);
        assert_eq!(p.on_quiet(1_200).keyframe, Some(Reason::Quiescence));
    }

    /// K4 for a renumbering with no output (an import attached older history): owed at
    /// once on a clean session, quiet or not, with the normal bridge, and settled by the
    /// keyframe it forced.
    #[test]
    fn a_renumbering_with_no_output_forces_a_keyframe() {
        let mut p = KeyframePolicy::new(CAP, 0, 7);
        assert!(!p.dirty());
        p.note_renumbered();
        assert_eq!(p.on_quiet(5).keyframe, Some(Reason::Renumbered));
        assert_eq!(Reason::Renumbered.bridge_cap(), BRIDGE_CAP);
        p.keyframe_written(5);
        assert_eq!(p.on_quiet(10_000).keyframe, None, "settled");
    }
}
