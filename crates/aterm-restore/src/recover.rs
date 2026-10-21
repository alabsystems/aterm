// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The restore (the crash-restore plan §2.7, step 3): a dead window's region in, an
//! exact checkpoint out, with every byte it could not reproduce counted.
//!
//! [`recover`] runs only after the keeper has proved the region's writer dead, over a
//! read-only view, and is a function of the region's bytes and the scratch engine the
//! caller builds:
//!
//! 1. **Header.** The magic, a layout version this build knows, a length equal to the
//!    view's, and section offsets that fit ([`Geometry::validate`]). Anything else is the
//!    META rung (a blank screen with the keeper's scalars, today's recovery).
//! 2. **Keyframe.** The newer valid slot: commit word nonzero and in its own slot, a
//!    length inside the slot, the step record's CRC and step equal to the commit word,
//!    and a body that passes every strict decode (`records::decode_keyframe`). No valid
//!    slot: META.
//! 3. **Scratch engine.** The caller's `make_engine(rows, cols)` (the window builds it
//!    exactly as it builds a live session's, with its config and no callbacks), then
//!    `restore_checkpoint`, then, when the keyframe says the live engine had a
//!    shell-integration nonce in use, a nonce of the caller's in its place
//!    ([`RecoverPolicy::scratch_nonce`]): the checkpoint carries none, and the replay of
//!    an OSC 7 at an authenticated prompt depends on one being in use. The replay never
//!    touches a live engine: the checkpoint excludes the response buffer and the OSC
//!    event queue, so a replay into a live engine would send DA and DSR replies a
//!    second time and fire OSC 52 clipboard writes, and a panic would leave it half
//!    replayed.
//! 4. **Tail** (only with [`RecoverPolicy::tail`]). The live window `[tail_floor,
//!    tail_commit)` is walked record by record, bounds-checked; from the keyframe's
//!    `tail_from` on, BYTES go through `process` and RESIZE through
//!    `resize_with_policy`. A floor past `tail_from` means the tail the keyframe needs
//!    was overwritten (`tail_overflow`): nothing is replayed onto that keyframe, because
//!    replaying the rest would skip bytes. RESIZES_LOST and ENGINE_REPLACED cost
//!    exactness, and so does a SUPERSEDED record wherever the walk meets it (the session
//!    journaled on in another region after it, [`Losses::superseded`]); an invalid record
//!    stops the walk and is counted. Every reply, OSC event and bell the replay produces
//!    stays in the scratch engine, which has no callbacks
//!    and no PTY, is dropped at the end of the call, and leaves it only as a checkpoint,
//!    which excludes all three: a restore re-sends no reply, writes no clipboard and
//!    rings no bell. A kitty-graphics command costs exactness too
//!    ([`Losses::graphics_replayed`]), and so does a Kitty unscroll on the main screen,
//!    which pulls history the scratch engine may not hold ([`Losses::unscrolls_replayed`]).
//! 5. **Result.** `checkpoint_continuing_swallowing()` of the scratch engine (no nonce,
//!    the parser carry kept). The plan fell back to the keyframe alone when the replay
//!    ended inside a hooked DCS query (which no carry holds), but that undoes everything
//!    the replay did, and the replay can hold an ED3 or a RIS whose forced keyframe K5
//!    deferred behind the query: the fallback brought back the screen and history the
//!    clear removed. So the screen the replay reached is restored, the query riding as a
//!    string to swallow, and any sequence the result can only swallow (that query, an
//!    over-cap OSC, an APC under way, a hooked sixel string) is counted
//!    ([`Losses::swallowed`]): what it would have done (a reply, a clipboard write, a
//!    picture) is lost, and the restore says so.
//! 6. **History.** The history ring's run is read and returned with the scratch
//!    engine's fence before and after the replay. [`Restored::join_facts`] makes them the
//!    join's inputs (the head the replay moved on, the lines the checkpoint carries, the
//!    ring as an export), which the window hands the update handoff's own join;
//!    [`Restored::history_admissible`] is the identity check that join takes, as the
//!    Tier-1 sweep binds it.
//! 7. **Counts and exactness.** `exact` holds only when the keyframe was valid, the
//!    tail replayed to its end with no fault and no overflow, nothing was gathered and
//!    left unprocessed, no resize was lost or unjournaled, no verified mark was left in
//!    the tail, no kitty-graphics command and no main-screen unscroll was replayed, the
//!    config fingerprint is still the host's (no setter after the keyframe,
//!    [`Losses::config_moved`]) and, when the replay applied any record, the
//!    restorer's, the replay ended outside any sequence the result can only
//!    swallow, and the region was not superseded by another.
//!
//!    The restorer's config matters only to a replay. With nothing past the keyframe (an
//!    idle session, which K1 catches up within 200 ms of its last output) the restored
//!    checkpoint is the keyframe's own: the engine's config shapes nothing in it that the
//!    adopting window does not set afresh, as it applies its own config to every adopted
//!    engine, the way an update's successor does. So a tab running a theme of its own,
//!    whose keyframes are stamped with that theme's config, restores exactly when it sat
//!    idle; only a tail replayed under the restorer's other config costs the exactness.
//!
//! The CONTROL SLOT is read apart ([`read_control`]): the window's control state (the
//! session's turn ledger) rides a restore on every rung once the header holds, screen or
//! no screen.
//!
//! Panic-free on any bytes: every read is bounds-checked through the view, every length
//! is checked before it indexes or allocates. The engine's own `process` is the
//! engine's; the window runs the whole call under `catch_unwind` and takes the META
//! rung on a panic.

use std::ops::Range;

use astream_wire::steps::{StepEnd, StepFault, scan_steps_from};
use aterm_core::grid::{HistoryFence, MAX_GRID_COLS, MAX_GRID_ROWS};
use aterm_core::terminal::{Terminal, TerminalCheckpoint};

use crate::layout::{
    ERASED_COUNT_V1, Geometry, LAYOUT_VERSION, LayoutError, MAGIC, MAX_REGION_LEN, OFF_CELL_PX,
    OFF_CONFIG_FP, OFF_CTL_COMMIT, OFF_CTL_COUNT, OFF_CTL_LEN, OFF_ENGINE_RESIZE_LIFETIME,
    OFF_ENGINE_RESIZE_ORDINAL, OFF_HIST_CAP, OFF_HIST_CAP_PHYS, OFF_HIST_END_ROW, OFF_HIST_HI,
    OFF_HIST_IDENTITY, OFF_HIST_LO, OFF_HIST_LO_SEQ, OFF_HIST_OFF, OFF_KF_CAP, OFF_KF_OFF,
    OFF_LOCAL_ID, OFF_MAGIC, OFF_REGION_LEN, OFF_SID, OFF_STREAM_GATHERED, OFF_STREAM_PROCESSED,
    OFF_TAIL_CAP, OFF_TAIL_COMMIT, OFF_TAIL_FLOOR, OFF_TAIL_MARKS, OFF_TAIL_OFF, OFF_WRITER_BIRTH,
    OFF_WRITER_BUILD, OFF_WRITER_PID, SID_LEN, cell_px_of, kf_commit_word, kf_len_word, slot_of,
};
use crate::records::{
    ChunkFault, ControlFault, HistoryChunk, HistoryIdentity, KIND_PAD, Keyframe, KeyframeFault,
    KeyframeHead, TAIL_RECORD_HEADER, TailFault, TailHeader, TailRecord, decode_control,
    decode_history_chunk, decode_keyframe, decode_tail_payload,
};
use crate::store::RegionView;

/// How a restore runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RecoverPolicy {
    /// Replay the tail. The keeper's `attempts == 0`: a restorer that died once (its
    /// replay may be what killed it) retries with the keyframe alone, and one that died
    /// twice gets the META rung without calling this.
    pub tail: bool,
    /// The restorer's own config fingerprint: a tail recorded under another config is
    /// not reproduced exactly by an engine built under this one (module doc, step 7).
    pub config_fp: u64,
    /// The nonce the scratch engine authorizes when the keyframe says the live engine
    /// had one in use ([`KeyframeHead::integration_on`]): a fresh one from the OS's
    /// CSPRNG (the window's `aterm_core::shell_integration::generate_nonce`), never the
    /// live one, which no region holds. It puts the scratch engine in the live engine's
    /// posture, so the replay holds an OSC 7 a background job prints at the shell's
    /// prompt as the live engine held it, rather than land it. A mark in the tail was
    /// signed with the live nonce and fails against this one; the live engine verified
    /// it, and every such mark is counted (`tail_marks`, [`Losses::marks_lost`]).
    /// Unguessable, so no program's output can sign a mark the scratch engine accepts
    /// where the live one dropped it.
    pub scratch_nonce: [u8; 32],
}

/// Why a restore fell to the META rung.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MetaWhy {
    /// The header is not a region this build reads.
    Header(HeaderFault),
    /// Neither slot holds a keyframe this build restores.
    NoKeyframe([SlotVerdict; 2]),
}

/// Why a header was refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HeaderFault {
    /// A header word could not be read.
    Unreadable,
    /// Not a journal region (or its creator died before writing the magic).
    Magic,
    /// A layout version this build does not read.
    Version(u8),
    /// The length word is not the view's, or is past [`MAX_REGION_LEN`].
    RegionLen(u64),
    /// The section words do not fit.
    Layout(LayoutError),
}

/// What one keyframe slot holds.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SlotVerdict {
    /// Commit word 0: empty, or being written when the writer died.
    Empty,
    /// A commit word whose parity is the other slot's.
    WrongSlot(u64),
    /// A length of zero or past the slot.
    Length(u64),
    /// The slot could not be read.
    Unreadable,
    /// The record is not a keyframe this build restores.
    Keyframe(KeyframeFault),
    /// A valid keyframe with this sequence number.
    Valid(u64),
}

/// The header's facts: reported, never trusted for anything but bounds.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HeaderFacts {
    /// The writer's pid.
    pub writer_pid: u32,
    /// The writer's birth, microseconds.
    pub writer_birth_us: u64,
    /// The session's local id in the writer.
    pub local_id: u64,
    /// The writer's build number.
    pub writer_build: u64,
    /// The session id the writer recorded (informational).
    pub sid: String,
    /// The validated layout.
    pub geometry: Geometry,
    /// The writer's config fingerprint as of its last config change.
    pub config_fp: u64,
    /// The writer's cell size as of its last cell-size change ([`OFF_CELL_PX`]): the
    /// restorer builds its scratch engine with it, so an inline image in the tail spans
    /// the rows it spanned live. `None`: never set.
    pub cell_px: Option<(u16, u16)>,
    /// The window's control count ([`OFF_CTL_COUNT`]: `aterm-gui`'s turn-id count as it last
    /// stored it), read whatever the control slot holds; 0: never stored.
    pub ctl_count: u64,
}

/// Everything a restore could not reproduce.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Losses {
    /// PTY bytes the writer read and never committed to the tail (gathered and not
    /// processed, or processed in the slice it died in).
    pub in_flight_lost: u64,
    /// PTY bytes the tail held past the keyframe that the restore does not reflect: an
    /// overflow, a fault, or a restore without the tail.
    pub tail_unreplayed: u64,
    /// The floor had passed the keyframe's `tail_from`.
    pub tail_overflow: bool,
    /// The first invalid tail record, which ended the walk.
    pub tail_fault: Option<TailFault>,
    /// Verified marks the keyframe budget left in the tail (`tail_marks`).
    pub marks_lost: u64,
    /// Resizes the engine's journal overran before the tail caught up.
    pub resizes_lost: u64,
    /// Resizes the host applied that neither the keyframe nor a RESIZE record past it
    /// reached (the header's resize word ahead of the last RESIZE past the keyframe's
    /// `tail_from`, or of the keyframe's own ordinal when none follows it; any lifetime
    /// mismatch counts).
    pub resizes_unjournaled: u64,
    /// Resizes the replay applied whose outcome depends on engine state no checkpoint
    /// carries, so such a restore is not called exact:
    ///
    /// - a width change rewraps the scratch engine's history from where the keyframe put
    ///   it (a tiered store), where the live engine rewrapped it largely from its ring, and
    ///   the engine numbers the rows it rewraps from the ring differently: the text comes
    ///   out the same, the absolute rows (shell marks, the history fence) do not;
    /// - a rows grow reveals history at the top from the grid's ring, which the restorer's
    ///   grid starts without (the keyframe's history sits in its store), and hands back the
    ///   rows a resize undo stashed, which no checkpoint carries.
    ///
    /// A rows shrink is reproduced exactly (it demotes or trims, the same in both). The
    /// engine conformance test found both cases; the policy keyframes soon after such a
    /// resize (K7), so a tail rarely holds one.
    pub regrids_replayed: u64,
    /// The replay crossed an ENGINE_REPLACED record.
    pub engine_replaced: bool,
    /// The tail ends in a SUPERSEDED record (`records::KIND_SUPERSEDED`): the session's
    /// journal went on in another region (a memory-pressure shed replaced this one), so
    /// whatever the session read after it is not here, and nothing here counts it. Read
    /// wherever the walk meets it, replayed or not: it is a fact about the region, not
    /// about the replay.
    pub superseded: bool,
    /// Kitty-graphics commands the replay ran (`Terminal::kitty_graphics_commands`).
    /// What one does depends on state no checkpoint carries and the scratch engine lacks:
    /// the image store (an `a=p` placing an image transmitted before the keyframe), a
    /// chunked transmission the keyframe cut (its `m=0` chunk), the image numbers, and
    /// the host's file and shared-memory medium resolver (`t=f`, `t=t`, `t=s`, which
    /// `kitten icat` uses on a local terminal). Live, such a command places an image and
    /// moves the cursor by the rows it spans; in the replay it fails and moves nothing,
    /// so the text after it lands on other rows. Every one is counted, a transmission
    /// whole inside the tail too (the count cannot tell which depended on what), and K1
    /// keyframes an idle session within 200 ms of its last output, so a tail holds one
    /// only in the moments after a picture.
    pub graphics_replayed: u64,
    /// Kitty unscrolls (`CSI n + T`) the replay ran on the main screen
    /// (`Terminal::kitty_unscrolls`). One pulls the newest history lines onto the
    /// screen, and the scratch engine starts with only the keyframe's bridge as its
    /// history, none at all once the history ring has caught up, kept in its store
    /// where the live engine read its ring first: live, the unscroll pulled history
    /// lines; in the replay it scrolls in blank rows, or other lines. So every one is
    /// counted (the count cannot tell which found what it needed), and the program is
    /// pulsed to redraw over the result. K1 keyframes an idle session within 200 ms, so
    /// a tail holds one only in the moments after it.
    pub unscrolls_replayed: u64,
    /// The host changed the engine-relevant config or the cell size after the newest
    /// keyframe and no keyframe followed: the header's `config_fp`, stored in the
    /// setter's own hold, is not the keyframe's. The tail past the keyframe may then
    /// hold bytes the live engine processed under the new settings, which a replay
    /// under the keyframe's does not reproduce (a forced keyframe, K4, closes the gap
    /// within the parse thread's next turn, so only a death inside it shows this). A
    /// header word of 0 is a region whose host never stored one (the headless tests'):
    /// the window stores its fingerprint, never 0, before its first keyframe.
    pub config_moved: bool,
    /// The replay ended inside a sequence the result can only swallow to its terminator,
    /// named by the parser state it sits in: a hooked DCS query (`DcsPassthrough`; its
    /// reply lives in its handler), an OSC over the carry's cap (an OSC 52 write, an OSC
    /// 1337 image) or an APC whose consumer started (`SosPmApcString`, a kitty-graphics
    /// command), a hooked sixel string. The live engine would have delivered it at its
    /// terminator; the restored one swallows the rest of it, so its effect is lost. The
    /// journal never keyframes inside one (`Terminal::checkpoint_journal` refuses, K5),
    /// so only a replay ends there.
    pub swallowed: Option<&'static str>,
}

/// Why a history ring's run ended before its commit word.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RingFault {
    /// The ring's words are inconsistent (a cap past the ring, a run longer than it).
    Words,
    /// A chunk is damaged or out of sequence where the commit word vouches for it.
    Chunk(StepEnd),
    /// A chunk's head is unreadable.
    Head(ChunkFault),
    /// A chunk names another identity than the ring's.
    Identity,
    /// Chunks whose rows do not follow one another, or end elsewhere than the end row.
    Rows,
}

/// The history ring's run: the chunks between its words, oldest first, as compressed
/// frames (the restorer copies them out of the dead mapping and decodes them off the
/// UI thread).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct RingRun {
    /// The identity the ring's chunks were exported under.
    pub identity: HistoryIdentity,
    /// The absolute row one past the newest exported line.
    pub end_row: u64,
    /// The chunks, oldest first. Their rows follow one another.
    pub chunks: Vec<HistoryChunk>,
    /// What ended the run early (its intact prefix is kept).
    pub fault: Option<RingFault>,
}

impl RingRun {
    /// The absolute row of the run's oldest line.
    #[must_use]
    pub fn first_row(&self) -> Option<u64> {
        self.chunks.first().map(|c| c.first_row)
    }

    /// How many lines the run holds.
    #[must_use]
    pub fn lines(&self) -> u64 {
        self.chunks.iter().map(|c| u64::from(c.n_lines)).sum()
    }
}

/// A restore that produced a screen.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Restored {
    /// The scratch engine's `checkpoint_continuing_swallowing()`: no nonce, the parser
    /// carry kept, a sequence it can only swallow riding as one to swallow
    /// ([`Losses::swallowed`]).
    pub checkpoint: TerminalCheckpoint,
    /// Every loss is zero and, when the replay applied a record, the config is the
    /// restorer's (module doc, step 7).
    pub exact: bool,
    /// The slot the keyframe came from.
    pub slot: usize,
    /// The keyframe's facts.
    pub keyframe: KeyframeHead,
    /// What the other slot held.
    pub other_slot: SlotVerdict,
    /// Tail records replayed onto the keyframe.
    pub replayed_records: u64,
    /// What it could not reproduce.
    pub losses: Losses,
    /// The scratch engine's fence right after the keyframe's restore.
    pub fence_before: HistoryFence,
    /// The scratch engine's fence after the replay.
    pub fence_after: HistoryFence,
    /// The history lines the scratch engine carries (the bridge plus what the replay
    /// pushed).
    pub carried_lines: u64,
    /// The history ring's run.
    pub ring: RingRun,
    /// The header's facts.
    pub header: HeaderFacts,
}

impl Restored {
    /// Whether the history ring's lines may be joined below the restored screen: the
    /// scratch engine's history identity held across the replay (no clear, reflow or
    /// reveal inside the tail, which would renumber or empty what the keyframe's fence
    /// names), and the ring was exported under the keyframe's identity. Anything else
    /// would import lines from before a clear the restored engine has seen, or rows the
    /// engine no longer numbers that way; the history is then counted lost instead.
    #[must_use]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "CrashRestoreJournal",
            action = "Import",
            project = "aterm_restore::conformance_region::Rig::project"
        )
    )]
    pub fn history_admissible(&self) -> bool {
        !self.keyframe.fence.detached
            && !self.fence_before.detached
            && !self.fence_after.detached
            && HistoryIdentity::of(&self.fence_before) == HistoryIdentity::of(&self.fence_after)
            && self.ring.identity == HistoryIdentity::of(&self.keyframe.fence)
    }

    /// What the history join reads of this restore (the plan's §3.2 N3), or why there is no
    /// history older than the restored checkpoint's to join below it.
    ///
    /// The plan's first design joined the ring against the KEYFRAME's fence, which ignores
    /// the replay: the tail pushes history lines of its own, and the ring can run ahead of
    /// the newest keyframe (the worker exports lines the tail also carries), so the join
    /// would read a fence that moved, or import the wrong span. So:
    ///
    /// - `head` is the keyframe's fence with its end advanced by the lines the replay
    ///   pushed (the scratch engine's end after the replay less its end before it: the
    ///   restore continues the dead window's absolute numbering, so both count in it). It
    ///   is the history as the dead window had it, as far as the region says; its oldest
    ///   row stays the keyframe's, the dead engine's own retention moving on from there.
    /// - `carried` is the scratch engine's history line count after the replay: the
    ///   bridge and what the replay pushed, the newest `carried` lines of `head`.
    /// - `export` is the ring's run as an export the join can fence: its identity, its
    ///   first row (never older than `head`'s oldest: a line the dead engine had already
    ///   evicted when it keyframed is not brought back), its end clamped to `head`'s end.
    ///   `None` when the ring holds nothing, or holds it under another identity than the
    ///   keyframe's (a reflow or a reveal the worker had not caught up with, a ring a
    ///   build without one left empty): the join then counts every line `head` names and
    ///   the checkpoint does not carry.
    ///
    /// The join itself is the update handoff's (`aterm-gui`'s `handoff_history::join`,
    /// bound to `NativeUpdateHistoryCarry`'s `Park`), called on these verbatim.
    ///
    /// # Errors
    ///
    /// The scratch engine's history identity moved across the replay, or a fence was taken
    /// while a reflow held the store, so its rows cannot be matched to the ring's
    /// ([`HeadLost`]): a clear in the tail (nothing to count: the person cleared what the
    /// ring held) or a reflow or reveal in it (the keyframe's history below its bridge is
    /// counted lost).
    pub fn join_facts(&self) -> Result<JoinFacts, HeadLost> {
        let held = !self.keyframe.fence.detached
            && !self.fence_before.detached
            && !self.fence_after.detached
            && HistoryIdentity::of(&self.fence_before) == HistoryIdentity::of(&self.fence_after);
        if !held {
            if self.fence_after.clear_gen != self.fence_before.clear_gen {
                return Err(HeadLost::Cleared);
            }
            return Err(HeadLost::Moved {
                lines: self
                    .keyframe
                    .fence
                    .lines()
                    .saturating_sub(self.fence_before.lines()),
            });
        }
        let pushed = self.fence_after.end.saturating_sub(self.fence_before.end);
        let head = HistoryFence {
            end: self.keyframe.fence.end.saturating_add(pushed),
            ..self.keyframe.fence
        };
        // The identity check is `history_admissible`'s, the one the Tier-1 sweep binds to the
        // model's `Import` (its scratch-identity half held above).
        let export = self
            .ring
            .first_row()
            .filter(|_| self.history_admissible())
            .map(|ring_first| {
                let first = ring_first.max(head.oldest);
                let end = self.ring.end_row.min(head.end).max(first);
                RingExport {
                    fence: HistoryFence {
                        oldest: first,
                        end,
                        detached: false,
                        ..head
                    },
                    first,
                    lines: end - first,
                    skip: first - ring_first,
                }
            });
        Ok(JoinFacts {
            head,
            carried: self.carried_lines,
            export,
        })
    }

    /// The ring's chunks an import of `take` lines from `export` decodes, oldest first, as
    /// a range of [`RingRun::chunks`], and how many lines at the front of the first of
    /// them precede `export.first`: exactly the chunks holding a line of the run's
    /// `[skip, skip + take)`. A chunk wholly below `export.first` (lines the dead engine
    /// had evicted, or older than the import's bound) is never decoded, so the import
    /// skips fewer lines than one chunk holds; the chunks past the last line it takes stay
    /// behind too (the import reads the first lines of a frame stream and never decodes
    /// past them). Nothing at all for `take == 0`.
    #[must_use]
    pub fn ring_chunks_for(&self, export: &RingExport, take: u64) -> (Range<usize>, u64) {
        if take == 0 {
            return (0..0, 0);
        }
        let end_line = export.skip.saturating_add(take);
        // Lines of the run before chunk `i`.
        let mut lines = 0u64;
        let mut first = None;
        let mut end = 0;
        for (i, chunk) in self.ring.chunks.iter().enumerate() {
            if lines >= end_line {
                break;
            }
            let next = lines.saturating_add(u64::from(chunk.n_lines));
            if first.is_none() && next > export.skip {
                first = Some((i, export.skip - lines));
            }
            end = i + 1;
            lines = next;
        }
        match first {
            Some((start, skip)) => (start..end, skip),
            None => (end..end, 0),
        }
    }
}

/// The ring's run, as the history join fences it ([`Restored::join_facts`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RingExport {
    /// The ring's identity, its oldest row `first` and its end clamped to the head's.
    pub fence: HistoryFence,
    /// The absolute row of the first line the join may take.
    pub first: u64,
    /// How many lines from `first` the ring holds, up to the head's end.
    pub lines: u64,
    /// The ring's lines older than `first` (the dead engine had evicted them when it
    /// keyframed): skipped at the front of the run's frames.
    pub skip: u64,
}

impl RingExport {
    /// The newest `max` lines of this export at most, as an update's export keeps the
    /// newest `handoff_history::MAX_EXPORT_LINES` of a history deeper than that: `first`
    /// moves up to `fence.end - max`, and the lines below it join `skip`. The join then
    /// counts every older line the head names as dropped, exactly as for an update, and
    /// never hands the import more lines than a sidecar may hold, which the import
    /// refuses whole (`read_sidecar`): a ring of compressible lines can hold well over a
    /// million in its 6.9 MiB.
    #[must_use]
    pub fn newest(self, max: u64) -> Self {
        let first = self.first.max(self.fence.end.saturating_sub(max));
        Self {
            fence: HistoryFence {
                oldest: first,
                ..self.fence
            },
            first,
            lines: self.fence.end - first,
            skip: self.skip + (first - self.first),
        }
    }
}

/// The history join's inputs ([`Restored::join_facts`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct JoinFacts {
    /// The history as the dead window last had it: the keyframe's fence, its end advanced
    /// by the lines the replay pushed.
    pub head: HistoryFence,
    /// How many of `head`'s newest lines the restored checkpoint carries.
    pub carried: u64,
    /// The ring's run, when it holds lines under the keyframe's identity.
    pub export: Option<RingExport>,
}

/// Why a restore has no head to join the ring below ([`Restored::join_facts`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HeadLost {
    /// The tail cleared the history (an ED3, a RIS): what the ring held is what the
    /// person erased, so nothing is joined and nothing is counted.
    Cleared,
    /// The tail rewrapped or revealed the history, or a fence was taken mid-reflow: the
    /// keyframe's lines below its bridge cannot be placed, and are counted.
    Moved {
        /// The keyframe's history lines the restored checkpoint does not carry.
        lines: u64,
    },
}

/// What a restore produced.
#[derive(Debug, Clone, PartialEq, Eq)]
#[allow(
    clippy::large_enum_variant,
    reason = "the restored arm is boxed; the META arm carries two slot verdicts"
)]
pub enum Recovery {
    /// A screen, with its losses counted.
    Restored(Box<Restored>),
    /// No screen: today's META rung.
    Meta(MetaWhy),
}

fn load<V: RegionView + ?Sized>(view: &V, at: usize) -> Option<u64> {
    view.load_u64_acquire(at).ok()
}

fn as_usize(v: u64) -> Option<usize> {
    usize::try_from(v).ok()
}

/// Read only the turn count of a privacy-erased v1 region. This marker authorizes no
/// geometry, allocation, ledger or screen bytes; the caller still takes the META rung.
#[must_use]
pub fn read_erased_turn_count<V: RegionView + ?Sized>(view: &V) -> Option<u64> {
    if view.len() > MAX_REGION_LEN || load(view, OFF_MAGIC)? != ERASED_COUNT_V1 {
        return None;
    }
    load(view, OFF_CTL_COUNT)
}

/// Read and check a region's header (module doc, step 1) without restoring anything:
/// what a restorer reads first, to build its scratch engine as the writer's was built
/// ([`HeaderFacts::cell_px`]) and to fingerprint its own config under that cell size.
///
/// # Errors
///
/// The [`HeaderFault`] that refuses the region; [`recover`] refuses it the same way.
pub fn read_header<V: RegionView + ?Sized>(view: &V) -> Result<HeaderFacts, HeaderFault> {
    let magic = load(view, OFF_MAGIC)
        .ok_or(HeaderFault::Unreadable)?
        .to_le_bytes();
    if magic[..4] != MAGIC || magic[5..] != [0, 0, 0] {
        return Err(HeaderFault::Magic);
    }
    if magic[4] == 0 || magic[4] > LAYOUT_VERSION {
        return Err(HeaderFault::Version(magic[4]));
    }
    let region_len = load(view, OFF_REGION_LEN).ok_or(HeaderFault::Unreadable)?;
    if as_usize(region_len) != Some(view.len()) || view.len() > MAX_REGION_LEN {
        return Err(HeaderFault::RegionLen(region_len));
    }
    let word = |at| {
        load(view, at)
            .and_then(as_usize)
            .ok_or(HeaderFault::Unreadable)
    };
    let geometry = Geometry {
        region_len: view.len(),
        kf_off: word(OFF_KF_OFF)?,
        kf_cap: word(OFF_KF_CAP)?,
        tail_off: word(OFF_TAIL_OFF)?,
        tail_cap: word(OFF_TAIL_CAP)?,
        hist_off: word(OFF_HIST_OFF)?,
        hist_cap_phys: word(OFF_HIST_CAP_PHYS)?,
    };
    geometry.validate().map_err(HeaderFault::Layout)?;
    let mut sid = [0u8; SID_LEN];
    view.read(OFF_SID, &mut sid)
        .map_err(|_| HeaderFault::Unreadable)?;
    let sid: String = sid
        .iter()
        .take_while(|b| **b != 0)
        .filter(|b| b.is_ascii_graphic())
        .map(|b| char::from(*b))
        .collect();
    let pid = load(view, OFF_WRITER_PID).ok_or(HeaderFault::Unreadable)?;
    Ok(HeaderFacts {
        writer_pid: u32::try_from(pid).unwrap_or(0),
        writer_birth_us: load(view, OFF_WRITER_BIRTH).ok_or(HeaderFault::Unreadable)?,
        local_id: load(view, OFF_LOCAL_ID).ok_or(HeaderFault::Unreadable)?,
        writer_build: load(view, OFF_WRITER_BUILD).ok_or(HeaderFault::Unreadable)?,
        sid,
        geometry,
        config_fp: load(view, OFF_CONFIG_FP).ok_or(HeaderFault::Unreadable)?,
        cell_px: cell_px_of(load(view, OFF_CELL_PX).ok_or(HeaderFault::Unreadable)?),
        ctl_count: load(view, OFF_CTL_COUNT).ok_or(HeaderFault::Unreadable)?,
    })
}

/// Judge one keyframe slot.
pub(crate) fn read_slot<V: RegionView + ?Sized>(
    view: &V,
    geometry: &Geometry,
    slot: usize,
) -> (SlotVerdict, Option<Keyframe>) {
    let Some(commit) = load(view, kf_commit_word(slot)) else {
        return (SlotVerdict::Unreadable, None);
    };
    if commit == 0 {
        return (SlotVerdict::Empty, None);
    }
    if slot_of(commit) != slot {
        return (SlotVerdict::WrongSlot(commit), None);
    }
    let Some(len) = load(view, kf_len_word(slot)) else {
        return (SlotVerdict::Unreadable, None);
    };
    let Some(n) = as_usize(len).filter(|n| *n > 0 && *n <= geometry.kf_cap) else {
        return (SlotVerdict::Length(len), None);
    };
    let mut record = Vec::new();
    if record.try_reserve_exact(n).is_err() {
        return (SlotVerdict::Unreadable, None);
    }
    record.resize(n, 0);
    if view.read(geometry.kf_slot_off(slot), &mut record).is_err() {
        return (SlotVerdict::Unreadable, None);
    }
    match decode_keyframe(&record, commit) {
        Ok(kf) => (SlotVerdict::Valid(commit), Some(kf)),
        Err(fault) => (SlotVerdict::Keyframe(fault), None),
    }
}

/// What a region's control slot holds (the crash-restore plan's increment 7,
/// [`read_control`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ControlVerdict {
    /// Commit word 0: never written, or being written when the writer died. No carry.
    Empty,
    /// The layout has no control slot (a test's geometry).
    NoSlot,
    /// A length of zero or past the slot.
    Length(u64),
    /// The slot could not be read.
    Unreadable,
    /// The record is not one this build reads, or not the one its commit word names. A
    /// writer of this protocol never leaves one: a fault, so no carry.
    Record(ControlFault),
    /// The newest complete record: its sequence number and the window's carry.
    Valid {
        /// The record's sequence number (its commit word).
        seq: u64,
        /// The window's carry, decompressed.
        carry: Vec<u8>,
    },
}

impl ControlVerdict {
    /// The carry, when the slot holds a valid record.
    #[must_use]
    pub fn into_carry(self) -> Option<Vec<u8>> {
        match self {
            Self::Valid { carry, .. } => Some(carry),
            _ => None,
        }
    }
}

/// Read a region's CONTROL SLOT (the crash-restore plan's increment 7): the carry its
/// writer's newest complete record holds, or why there is none. Apart from [`recover`]
/// and its rungs: the slot is the window's control state (the session's turn ledger),
/// which a restore carries whether or not it brings back a screen, once the header
/// ([`read_header`]) has vouched for the geometry. Strict as a keyframe slot is read: a
/// commit word nonzero, a length inside the slot, one step record whose step is that word
/// and whose CRC holds, a body version this build reads and an LZ4 size bounded before
/// anything is allocated (`records::decode_control`). Panic-free on any bytes.
#[must_use]
pub fn read_control<V: RegionView + ?Sized>(view: &V, geometry: &Geometry) -> ControlVerdict {
    let cap = geometry.ctl_cap();
    if cap == 0 {
        return ControlVerdict::NoSlot;
    }
    let Some(seq) = load(view, OFF_CTL_COMMIT) else {
        return ControlVerdict::Unreadable;
    };
    if seq == 0 {
        return ControlVerdict::Empty;
    }
    let Some(len) = load(view, OFF_CTL_LEN) else {
        return ControlVerdict::Unreadable;
    };
    let Some(n) = as_usize(len).filter(|n| *n > 0 && *n <= cap) else {
        return ControlVerdict::Length(len);
    };
    let mut record = Vec::new();
    if record.try_reserve_exact(n).is_err() {
        return ControlVerdict::Unreadable;
    }
    record.resize(n, 0);
    if view.read(geometry.ctl_off(), &mut record).is_err() {
        return ControlVerdict::Unreadable;
    }
    match decode_control(&record, seq) {
        Ok(carry) => ControlVerdict::Valid { seq, carry },
        Err(fault) => ControlVerdict::Record(fault),
    }
}

/// What the tail walk did.
#[derive(Debug, Default)]
struct Walk {
    records: u64,
    /// The stream position the engine reflects.
    reached: u64,
    /// The stream position of the tail's last record, as far as the walk read.
    committed: Option<u64>,
    /// The last RESIZE past the keyframe's `tail_from`: `(lifetime, ordinal)`.
    last_resize: Option<(u64, u64)>,
    overflow: bool,
    fault: Option<TailFault>,
    resizes_lost: u64,
    regrids: u64,
    engine_replaced: bool,
    superseded: bool,
}

/// Walk the tail's live window, replaying from the keyframe's `tail_from` when `apply`.
fn walk_tail<V: RegionView + ?Sized>(
    view: &V,
    geometry: &Geometry,
    keyframe: &KeyframeHead,
    apply: bool,
    engine: &mut Terminal,
) -> Walk {
    let mut walk = Walk {
        reached: keyframe.stream_at,
        ..Walk::default()
    };
    let (Some(commit), Some(floor)) = (load(view, OFF_TAIL_COMMIT), load(view, OFF_TAIL_FLOOR))
    else {
        walk.fault = Some(TailFault::Unreadable);
        return walk;
    };
    let cap = geometry.tail_cap as u64;
    if floor > commit && floor - commit <= cap {
        // A writer stopped between a floor that evicted the whole window and the commit
        // of the record it made room for: the tail holds nothing replayable, and the
        // keyframe's tail is gone (an overflow, not damage).
        walk.overflow = true;
        return walk;
    }
    if floor > commit || commit - floor > cap {
        walk.fault = Some(TailFault::PastCommit);
        return walk;
    }
    let from = keyframe.tail_from;
    if from > commit {
        walk.fault = Some(TailFault::Misplaced);
        return walk;
    }
    walk.overflow = from < floor;
    let replay = apply && !walk.overflow;
    let mut pos = floor;
    let mut prev_end: Option<u64> = None;
    let mut started = from == floor;
    while pos < commit {
        if pos == from {
            started = true;
        } else if pos > from && !started && !walk.overflow {
            walk.fault = Some(TailFault::Misplaced);
            return walk;
        }
        let off = pos % cap;
        let room = cap - off;
        if room < TAIL_RECORD_HEADER as u64 {
            // A gap under a header before the ring's end: the writer skipped it.
            let Some(end) = pos.checked_add(room).filter(|end| *end <= commit) else {
                walk.fault = Some(TailFault::PastCommit);
                return walk;
            };
            pos = end;
            continue;
        }
        if pos
            .checked_add(TAIL_RECORD_HEADER as u64)
            .is_none_or(|end| end > commit)
        {
            walk.fault = Some(TailFault::PastCommit);
            return walk;
        }
        let at = geometry.tail_off + usize::try_from(off).unwrap_or(0);
        let mut head = [0u8; TAIL_RECORD_HEADER];
        if view.read(at, &mut head).is_err() {
            walk.fault = Some(TailFault::Unreadable);
            return walk;
        }
        let header = TailHeader::read(&head);
        if header.kind == KIND_PAD {
            if header.record_len() as u64 != room || header.flags != 0 || header.reserved != 0 {
                walk.fault = Some(TailFault::BadPad);
                return walk;
            }
            let Some(end) = pos.checked_add(room).filter(|end| *end <= commit) else {
                walk.fault = Some(TailFault::PastCommit);
                return walk;
            };
            pos = end;
            continue;
        }
        let len = header.record_len() as u64;
        if len > room {
            walk.fault = Some(TailFault::BadLength);
            return walk;
        }
        let Some(end) = pos.checked_add(len).filter(|end| *end <= commit) else {
            walk.fault = Some(TailFault::PastCommit);
            return walk;
        };
        let payload_len = usize::try_from(header.payload_len).unwrap_or(usize::MAX);
        let mut payload = vec![0u8; payload_len];
        if view.read(at + TAIL_RECORD_HEADER, &mut payload).is_err() {
            walk.fault = Some(TailFault::Unreadable);
            return walk;
        }
        let record = match decode_tail_payload(&header, &payload) {
            Ok(record) => record,
            Err(fault) => {
                walk.fault = Some(fault);
                return walk;
            }
        };
        let behind = prev_end.is_some_and(|prev| header.stream_end < prev)
            || (started && header.stream_end < keyframe.stream_at);
        if behind {
            walk.fault = Some(TailFault::StreamBackwards);
            return walk;
        }
        prev_end = Some(header.stream_end);
        walk.committed = Some(header.stream_end);
        // Only a RESIZE past the keyframe speaks for a later resize: the keyframe's own
        // ordinal covers everything before it, a resize its capture absorbed (with no
        // record) included, so an older record must not stand in for it.
        if let TailRecord::Resize {
            lifetime, ordinal, ..
        } = record
            && started
        {
            walk.last_resize = Some((lifetime, ordinal));
        }
        if record == TailRecord::Superseded {
            walk.superseded = true;
        }
        if replay && started {
            match record {
                TailRecord::Bytes { bytes, .. } => engine.process(bytes),
                TailRecord::Resize {
                    rows, cols, policy, ..
                } => {
                    if rows == 0 || cols == 0 || rows > MAX_GRID_ROWS || cols > MAX_GRID_COLS {
                        walk.fault = Some(TailFault::BadLength);
                        return walk;
                    }
                    if cols != engine.cols() || rows > engine.rows() {
                        walk.regrids = walk.regrids.saturating_add(1);
                    }
                    engine.resize_with_policy(rows, cols, policy);
                }
                TailRecord::ResizesLost { n } => {
                    walk.resizes_lost = walk.resizes_lost.saturating_add(u64::from(n));
                }
                TailRecord::EngineReplaced => walk.engine_replaced = true,
                TailRecord::Superseded => {}
            }
            walk.records += 1;
            walk.reached = header.stream_end;
        }
        pos = end;
    }
    // A keyframe cut at the commit itself has nothing past it to replay.
    if !started && !walk.overflow && pos != from {
        walk.fault = Some(TailFault::Misplaced);
    }
    walk
}

/// Read the history ring's run between its words (module doc, step 6).
fn read_ring<V: RegionView + ?Sized>(view: &V, geometry: &Geometry) -> RingRun {
    let mut run = RingRun::default();
    let words = (
        load(view, OFF_HIST_LO),
        load(view, OFF_HIST_HI),
        load(view, OFF_HIST_LO_SEQ),
        load(view, OFF_HIST_END_ROW),
        load(view, OFF_HIST_CAP).and_then(as_usize),
    );
    let identity = (0..4)
        .map(|i| load(view, OFF_HIST_IDENTITY + 8 * i))
        .collect::<Option<Vec<u64>>>();
    let (
        (Some(lo), Some(hi), Some(lo_seq), Some(end_row), Some(cap)),
        Some(&[renumber_epoch, clear_gen, reveal_gen, cols]),
    ) = (words, identity.as_deref())
    else {
        run.fault = Some(RingFault::Words);
        return run;
    };
    let Ok(cols) = u16::try_from(cols) else {
        run.fault = Some(RingFault::Words);
        return run;
    };
    run.identity = HistoryIdentity {
        renumber_epoch,
        clear_gen,
        reveal_gen,
        cols,
    };
    run.end_row = end_row;
    if lo >= hi {
        // Empty: never written, just reset, or a stop mid-way through an eviction.
        return run;
    }
    if cap == 0 || cap > geometry.hist_cap_phys || hi - lo > cap as u64 {
        run.fault = Some(RingFault::Words);
        return run;
    }
    let cap64 = cap as u64;
    let mut pos = lo;
    let mut expected = lo_seq;
    let mut accepted_end = None;
    while pos < hi {
        let off = pos % cap64;
        let Some(ring_end) = pos.checked_add(cap64 - off) else {
            run.fault = Some(RingFault::Words);
            return run;
        };
        let seg_end = hi.min(ring_end);
        let (Some(off_usize), Some(seg_len)) = (as_usize(off), as_usize(seg_end - pos)) else {
            run.fault = Some(RingFault::Words);
            return run;
        };
        let mut bytes = vec![0u8; seg_len];
        if view
            .read(geometry.hist_off + off_usize, &mut bytes)
            .is_err()
        {
            run.fault = Some(RingFault::Words);
            return run;
        }
        let mut scan = scan_steps_from(&bytes, expected);
        // The run's first chunk may be one step BELOW `hist_lo_seq`: an eviction stores
        // the new sequence word before the new position word, and a writer stopped
        // between the two leaves `hist_lo` at the chunk it was about to drop. That chunk
        // is still whole (nothing is copied before both words), so the run is read from
        // it; any other disorder is a fault as before.
        if pos == lo
            && let StepEnd::Fault {
                at: 0,
                kind: StepFault::OutOfOrder { found, .. },
            } = scan.end
            && found < expected
        {
            expected = found;
            scan = scan_steps_from(&bytes, expected);
        }
        for (step, range) in &scan.steps {
            let Some(body) = bytes.get(range.clone()) else {
                run.fault = Some(RingFault::Words);
                return run;
            };
            match decode_history_chunk(*step, body) {
                Ok(chunk) => {
                    if chunk.identity != run.identity {
                        run.fault = Some(RingFault::Identity);
                        return run;
                    }
                    let Some(chunk_end) = chunk
                        .first_row
                        .checked_add(u64::from(chunk.n_lines))
                        .filter(|end| *end <= end_row)
                    else {
                        run.fault = Some(RingFault::Rows);
                        return run;
                    };
                    if accepted_end.is_some_and(|end| end != chunk.first_row) {
                        run.fault = Some(RingFault::Rows);
                        return run;
                    }
                    accepted_end = Some(chunk_end);
                    run.end_row = chunk_end;
                    run.chunks.push(chunk);
                }
                Err(fault) => {
                    run.fault = Some(RingFault::Head(fault));
                    return run;
                }
            }
        }
        expected = expected.saturating_add(scan.steps.len() as u64);
        let valid = scan.valid_len as u64;
        if scan.end == StepEnd::Clean {
            pos += valid;
            continue;
        }
        // Not clean: the zeroed gap a chunk that would straddle the ring's end leaves
        // before it, when the run goes on at the ring's start; a fault otherwise.
        let gap_is_zero = bytes
            .get(scan.valid_len..)
            .is_some_and(|rest| rest.iter().all(|b| *b == 0));
        if seg_end == ring_end && ring_end < hi && gap_is_zero {
            pos = ring_end;
            continue;
        }
        run.fault = Some(RingFault::Chunk(scan.end));
        return run;
    }
    // Each admitted prefix was validated before any subsequent read could fail, and no
    // chunk ends past the end row. A run that ends BELOW it is whole: the writer stores
    // the end row before the commit word, so a stop between the two leaves the next
    // chunk's end row over a run the commit word still ends at the previous chunk. The
    // run's own end is what the join reads (`end_row` follows the chunks); discarding
    // the run there would throw away up to the whole ring for one uncommitted chunk.
    run
}

/// Restore a dead window's session from its region (module doc).
///
/// `make_engine(rows, cols)` builds the scratch engine at the keyframe's geometry,
/// exactly as the restorer builds a live session's engine, with its config and no
/// callbacks. Called at most once, and only once a valid keyframe is in hand.
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "CrashRestoreJournal",
        action = "Die",
        reason = "the environment: W dies without a word (a SIGKILL, a jetsam kill, a Force \
                  Quit, a crash) and no code performs it; Tier-1 kills the real writer's \
                  `CountdownStore` after each of its primitive stores and restores what it \
                  left with this fn"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "CrashRestoreJournal",
        action = "Poison",
        reason = "a ghost of the restorer: some tail slice kills a restorer that replays it, \
                  and no code makes one; Tier-1 plays it as a full try that dies, with the \
                  keeper's `attempts` brake"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "CrashRestoreJournal",
        action = "PoisonKf",
        reason = "a ghost of the restorer: the keyframe itself kills every restorer, and no \
                  code makes one; Tier-1 plays it as every try dying, down to the META rung"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "CrashRestoreJournal",
        action = "RestoreFullA",
        project = "aterm_restore::conformance_region::Rig::project"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "CrashRestoreJournal",
        action = "RestoreFullB",
        project = "aterm_restore::conformance_region::Rig::project"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "CrashRestoreJournal",
        action = "RestoreScreenA",
        project = "aterm_restore::conformance_region::Rig::project"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "CrashRestoreJournal",
        action = "RestoreScreenB",
        project = "aterm_restore::conformance_region::Rig::project"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "CrashRestoreJournal",
        action = "RestoreMeta",
        project = "aterm_restore::conformance_region::Rig::project"
    )
)]
pub fn recover<V: RegionView + ?Sized>(
    view: &V,
    policy: RecoverPolicy,
    make_engine: impl FnOnce(u16, u16) -> Terminal,
) -> Recovery {
    let header = match read_header(view) {
        Ok(header) => header,
        Err(fault) => return Recovery::Meta(MetaWhy::Header(fault)),
    };
    let geometry = header.geometry;
    let (v0, k0) = read_slot(view, &geometry, 0);
    let (v1, k1) = read_slot(view, &geometry, 1);
    let (slot, keyframe, other_slot) = match (k0, k1) {
        (Some(a), Some(b)) if b.head.seq > a.head.seq => (1, b, v0),
        (Some(a), _) => (0, a, v1),
        (None, Some(b)) => (1, b, v0),
        (None, None) => return Recovery::Meta(MetaWhy::NoKeyframe([v0, v1])),
    };

    let mut engine = make_engine(keyframe.checkpoint.rows, keyframe.checkpoint.cols);
    engine.restore_checkpoint(&keyframe.checkpoint);
    if keyframe.head.integration_on {
        // The live engine's posture, never its nonce (`RecoverPolicy::scratch_nonce`).
        engine.authorize_shell_integration(policy.scratch_nonce);
    }
    let fence_before = engine.history_fence();
    let kitty_before = engine.kitty_graphics_commands();
    let unscrolls_before = engine.kitty_unscrolls();

    let walk = walk_tail(view, &geometry, &keyframe.head, policy.tail, &mut engine);
    // What the replay produced for the PTY, the clipboard or the bell stays in this
    // engine, which nothing else holds and whose checkpoint below excludes it (module
    // doc, step 4): there is nothing to drain.
    let graphics_replayed = engine.kitty_graphics_commands().wrapping_sub(kitty_before);
    let unscrolls_replayed = engine.kitty_unscrolls().wrapping_sub(unscrolls_before);

    let fence_after = engine.history_fence();
    // The screen the replay reached, never the keyframe alone (module doc, step 5).
    let swallowed = engine
        .partial_sequence_uncarried()
        .or_else(|| engine.partial_sequence_swallowed());
    let checkpoint = engine.checkpoint_continuing_swallowing();

    let gathered = load(view, OFF_STREAM_GATHERED).unwrap_or(0);
    let processed_word = load(view, OFF_STREAM_PROCESSED).unwrap_or(0);
    // What the tail committed, as far as the walk read it (a fault stops the walk, and
    // the stream count stored just before the commit word bounds the rest).
    let committed = match (walk.fault, walk.committed) {
        (None, Some(end)) => end,
        // An overflow that left no record to read: the count stored before the commit.
        (None, None) if walk.overflow => processed_word,
        (None, None) => walk.reached,
        (Some(_), end) => end.unwrap_or(walk.reached).max(processed_word),
    };
    let committed = committed.max(walk.reached);
    let (journaled_lifetime, journaled_ordinal) = walk
        .last_resize
        .unwrap_or((keyframe.head.resize_lifetime, keyframe.head.resize_ordinal));
    let host_lifetime = load(view, OFF_ENGINE_RESIZE_LIFETIME).unwrap_or(0);
    let host_ordinal = load(view, OFF_ENGINE_RESIZE_ORDINAL).unwrap_or(0);
    // A lifetime of 0: no host resize since the region's creation.
    let resizes_unjournaled = if host_lifetime == 0 {
        0
    } else if host_lifetime == journaled_lifetime {
        host_ordinal.saturating_sub(journaled_ordinal)
    } else {
        host_ordinal.max(1)
    };
    let losses = Losses {
        in_flight_lost: gathered.saturating_sub(committed),
        tail_unreplayed: committed.saturating_sub(walk.reached),
        tail_overflow: walk.overflow,
        tail_fault: walk.fault,
        marks_lost: load(view, OFF_TAIL_MARKS).unwrap_or(0),
        resizes_lost: walk.resizes_lost,
        resizes_unjournaled,
        regrids_replayed: walk.regrids,
        engine_replaced: walk.engine_replaced,
        superseded: walk.superseded,
        graphics_replayed,
        unscrolls_replayed,
        config_moved: header.config_fp != 0 && header.config_fp != keyframe.head.config_fp,
        swallowed,
    };
    // The restorer's config is weighed only against a replay (module doc, step 7).
    let config_ok = keyframe.head.config_fp == policy.config_fp || walk.records == 0;
    let exact = policy.tail && losses == Losses::default() && config_ok;
    let ring = read_ring(view, &geometry);
    Recovery::Restored(Box::new(Restored {
        checkpoint,
        exact,
        slot,
        keyframe: keyframe.head,
        other_slot,
        replayed_records: walk.records,
        losses,
        fence_before,
        carried_lines: fence_after.lines(),
        fence_after,
        ring,
        header,
    }))
}

#[cfg(test)]
#[path = "recover_tests.rs"]
mod tests;
