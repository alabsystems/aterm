// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::records::CHUNK_LINES;
use crate::store::{RegionStore, VecStore};
use crate::writer::{JournalWriter, KeyframeInput, WriteError, WriterIdentity};
use aterm_spec::rng::XorShift;

const WRITER: WriterIdentity<'static> = WriterIdentity {
    pid: 7,
    birth_us: 1,
    local_id: 2,
    build: 105,
    sid: "s-recover",
};

const CONFIG: u64 = 0x5EED;

/// A live engine and the writer journaling it, as the parse thread runs them.
struct Live {
    term: Terminal,
    writer: JournalWriter<VecStore>,
    gathered: u64,
}

impl Live {
    fn new(rows: u16, cols: u16, geometry: Geometry) -> Self {
        let writer = JournalWriter::create(VecStore::new(geometry.region_len), geometry, &WRITER)
            .expect("created");
        let mut live = Self {
            term: Terminal::new(rows, cols),
            writer,
            gathered: 0,
        };
        live.keyframe(512);
        live
    }

    fn v1(rows: u16, cols: u16) -> Self {
        Self::new(rows, cols, Geometry::v1())
    }

    /// A slice read, processed and appended, as the reader does.
    fn slice(&mut self, bytes: &[u8]) {
        self.gathered += bytes.len() as u64;
        self.writer.set_gathered(self.gathered).expect("gathered");
        self.term.process(bytes);
        self.writer.append_slice(bytes).expect("appended");
    }

    fn keyframe(&mut self, bridge: usize) {
        let checkpoint = self.term.checkpoint_journal(bridge).expect("ground");
        let input = KeyframeInput {
            checkpoint: &checkpoint,
            fence: self.term.history_fence(),
            resize_lifetime: self.term.resize_lifetime(),
            resize_ordinal: self.term.resize_ordinal(),
            bridge_lines: u32::try_from(bridge).unwrap_or(u32::MAX),
            config_fp: CONFIG,
            integration_on: self.term.shell_integration_posture()
                == aterm_core::terminal::ShellIntegrationPosture::On,
        };
        self.writer.write_keyframe(&input).expect("keyframe");
    }

    fn recover(&self, tail: bool) -> Recovery {
        let dead = self.writer.store().snapshot();
        recover(
            &dead,
            RecoverPolicy {
                tail,
                config_fp: CONFIG,
                scratch_nonce: [0xA5; 32],
            },
            Terminal::new,
        )
    }
}

fn restored(r: Recovery) -> Restored {
    match r {
        Recovery::Restored(r) => *r,
        Recovery::Meta(why) => panic!("expected a screen, got the META rung: {why:?}"),
    }
}

/// The visible state a person sees: the restored checkpoint, rebuilt and projected
/// without history.
fn visible(c: &TerminalCheckpoint) -> TerminalCheckpoint {
    Terminal::from_checkpoint(c)
        .checkpoint_journal(0)
        .expect("ground")
}

/// A session restored from its region is the live session: the keyframe plus the
/// replayed tail, exact, with nothing lost.
#[test]
fn a_restore_is_the_live_session() {
    let mut live = Live::v1(6, 40);
    for i in 0..30 {
        live.slice(format!("\x1b[1;3{}mline {i}\x1b[0m\r\n", i % 8).as_bytes());
    }
    live.keyframe(512);
    live.slice(b"after the keyframe \xe2\x9c\x93 \x1b[7mreverse");
    live.slice(b"\x1b[0m\r\n$ ");
    let r = restored(live.recover(true));
    assert!(r.exact, "{:?}", r.losses);
    assert_eq!(r.replayed_records, 2);
    assert_eq!(r.slot, 0, "the second keyframe, seq 2");
    assert_eq!(
        visible(&r.checkpoint),
        visible(&live.term.checkpoint_journal(0).expect("g"))
    );
    assert_eq!(r.header.writer_pid, 7);
    assert_eq!(r.header.sid, "s-recover");
    assert_eq!(r.other_slot, SlotVerdict::Valid(1));
}

/// The cursor and the visible rows a person sees.
fn screen_of(t: &Terminal) -> ((u16, u16), String) {
    let cursor = t.cursor();
    ((cursor.row, cursor.col), t.visible_content())
}

/// A kitty-graphics command in the tail is never called exact, because what it does
/// depends on state no keyframe carries: an image transmitted before the keyframe and
/// placed after it, and a chunked transmission the keyframe cut, each place an image live
/// (moving the cursor by the rows it spans, so the text after it lands lower) and fail in
/// the replay (the image store and the chunk under way are not in the checkpoint). The
/// screens differ, and the restore says so (`graphics_replayed`), so the program is
/// pulsed to redraw. A tail with no kitty command restores exactly over the same store.
#[test]
fn a_kitty_command_in_the_tail_is_never_called_exact() {
    let cases: [(&str, &[u8], &[u8]); 2] = [
        (
            "transmitted before the keyframe, placed after it",
            b"\x1b[2;3H\x1b_Ga=t,f=32,s=1,v=1,i=7,q=2;AAAA/w==\x1b\\",
            b"\x1b_Ga=p,i=7,c=4,r=3,q=2\x1b\\after",
        ),
        (
            "a chunked transmission the keyframe cut",
            b"\x1b[2;3H\x1b_Ga=T,f=32,s=1,v=1,c=4,r=3,m=1,q=2;AAAA\x1b\\",
            b"\x1b_Gm=0;/w==\x1b\\after",
        ),
    ];
    for (label, before, after) in cases {
        let mut live = Live::v1(10, 20);
        live.slice(before);
        live.keyframe(0);
        live.slice(after);
        let r = restored(live.recover(true));
        assert_ne!(
            screen_of(&Terminal::from_checkpoint(&r.checkpoint)),
            screen_of(&live.term),
            "{label}: premise, the replay cannot reproduce the command"
        );
        assert!(!r.exact, "{label}: {:?}", r.losses);
        assert_eq!(r.losses.graphics_replayed, 1, "{label}");
    }
    // NEGATIVE CONTROL: the same stored image, and a tail with no kitty command.
    let mut live = Live::v1(10, 20);
    live.slice(cases[0].1);
    live.keyframe(0);
    live.slice(b"plain text");
    let r = restored(live.recover(true));
    assert!(r.exact, "{:?}", r.losses);
    assert_eq!(
        screen_of(&Terminal::from_checkpoint(&r.checkpoint)),
        screen_of(&live.term)
    );
}

/// A Kitty unscroll in the tail is never called exact: it pulls the newest history lines
/// onto the screen, and the scratch engine holds only the keyframe's bridge as its
/// history (here none, as a caught-up history ring leaves it), so the replay scrolls in
/// blank rows where the live engine pulled lines. The screens differ, and the restore says
/// so (`unscrolls_replayed`), so the program is pulsed to redraw. On the alternate screen,
/// which keeps no history, the same unscroll scrolls in blank rows in both engines: not
/// counted, and exact.
#[test]
fn a_kitty_unscroll_in_the_tail_is_never_called_exact() {
    let shell = || {
        let mut live = Live::v1(6, 20);
        for i in 0..30 {
            live.slice(format!("history {i}\r\n").as_bytes());
        }
        live
    };
    let mut live = shell();
    live.keyframe(0);
    live.slice(b"\x1b[3+T");
    let r = restored(live.recover(true));
    assert_ne!(
        screen_of(&Terminal::from_checkpoint(&r.checkpoint)),
        screen_of(&live.term),
        "premise: the live unscroll pulled lines the replay does not hold"
    );
    assert!(!r.exact, "{:?}", r.losses);
    assert_eq!(r.losses.unscrolls_replayed, 1);

    // NEGATIVE CONTROL: the same unscroll on the alternate screen.
    let mut live = shell();
    live.slice(b"\x1b[?1049h\x1b[2J\x1b[Ha frame\r\nits second row");
    live.keyframe(0);
    live.slice(b"\x1b[3+T");
    let r = restored(live.recover(true));
    assert!(r.exact, "{:?}", r.losses);
    assert_eq!(
        screen_of(&Terminal::from_checkpoint(&r.checkpoint)),
        screen_of(&live.term)
    );
}

/// The checkpoint carries no shell-integration nonce, but the replay depends on whether
/// one was in use: an OSC 7 a background job prints while an authenticated shell sits at
/// its prompt is HELD live (it waits for the shell's next prompt, and the shell's `C`
/// drops it), and lands at once in an engine with no nonce in use. So the keyframe says
/// the posture (`integration_on`) and the replay authorizes a nonce of its own: the
/// restored cwd is the live one. NEGATIVE CONTROL: the same region recovered as if the
/// keyframe said nothing (the flag cleared) lands the report.
#[test]
fn a_cwd_report_held_at_an_authenticated_prompt_stays_held_in_the_replay() {
    const NONCE: [u8; 32] = [0x3C; 32];
    let hex = aterm_core::terminal::ShellIntegrationNonce(NONCE).to_hex();
    let mut live = Live::v1(6, 40);
    live.term.set_require_shell_integration_nonce(true);
    live.term.authorize_shell_integration(NONCE);
    live.slice(
        format!(
            "\x1b]133;A;id={hex}\x07$ \x1b]133;B;id={hex}\x07ls\r\n\x1b]133;C;id={hex}\x07out\r\n\
             \x1b]133;D;0;id={hex}\x07\x1b]7;file://h/Users//me\x07\x1b]133;A;id={hex}\x07$ \
             \x1b]133;B;id={hex}\x07"
        )
        .as_bytes(),
    );
    live.keyframe(0);
    // A background job reports a directory while the shell sits at its prompt.
    live.slice(b"\x1b]7;file://h/private/tmp/elsewhere\x07");
    assert_eq!(
        live.term.current_working_directory(),
        Some("/Users//me"),
        "premise: the live engine held the job's report"
    );
    let r = restored(live.recover(true));
    assert!(r.keyframe.integration_on);
    assert!(r.exact, "{:?}", r.losses);
    assert_eq!(
        Terminal::from_checkpoint(&r.checkpoint).current_working_directory(),
        live.term.current_working_directory()
    );

    // The control: the keyframe's flag cleared (its body re-encoded), so the replay runs
    // with no nonce in use, as before the flag existed.
    let dead = live.writer.store().snapshot();
    let geometry = read_header(&dead).expect("header").geometry;
    let slot = r.slot;
    let (_, kf) = read_slot(&dead, &geometry, slot);
    let kf = kf.expect("the newest keyframe");
    let head = KeyframeHead {
        integration_on: false,
        ..kf.head
    };
    let record = crate::records::encode_keyframe(&head, &kf.checkpoint).expect("encodes");
    let len = u64::try_from(record.len()).expect("len");
    dead.write(geometry.kf_slot_off(slot), &record)
        .expect("the slot");
    dead.store_u64_release(crate::layout::kf_len_word(slot), len)
        .expect("the length");
    let unflagged = restored(recover(
        &dead,
        RecoverPolicy {
            tail: true,
            config_fp: CONFIG,
            scratch_nonce: [0xA5; 32],
        },
        Terminal::new,
    ));
    assert!(!unflagged.keyframe.integration_on);
    assert_eq!(
        Terminal::from_checkpoint(&unflagged.checkpoint).current_working_directory(),
        Some("/private/tmp/elsewhere"),
        "control: with no nonce in use the replay lands the job's report"
    );
}

/// Without the tail (the keeper's second try) the keyframe alone comes back, its tail
/// counted as unreplayed; with a config the restorer does not run, a replayed tail is not
/// exact. With nothing past the keyframe (an idle session K1 caught up) the restorer's
/// config shapes nothing restored, and the restore is exact under any config.
#[test]
fn the_keyframe_alone_and_a_foreign_config_are_not_exact() {
    let mut live = Live::v1(4, 30);
    live.slice(b"before the keyframe\r\n");
    live.keyframe(512);
    let idle = restored(recover(
        &live.writer.store().snapshot(),
        RecoverPolicy {
            tail: true,
            config_fp: CONFIG + 1,
            scratch_nonce: [0xA5; 32],
        },
        Terminal::new,
    ));
    assert_eq!(idle.replayed_records, 0);
    assert!(
        idle.exact,
        "nothing replayed under the other config: {:?}",
        idle.losses
    );
    live.slice(b"one\r\n");
    let alone = restored(live.recover(false));
    assert!(!alone.exact);
    assert_eq!(alone.losses.tail_unreplayed, 5);
    assert_eq!(alone.replayed_records, 0);
    let dead = live.writer.store().snapshot();
    let foreign = restored(recover(
        &dead,
        RecoverPolicy {
            tail: true,
            config_fp: CONFIG + 1,
            scratch_nonce: [0xA5; 32],
        },
        Terminal::new,
    ));
    assert!(!foreign.exact);
    assert_eq!(foreign.losses, Losses::default(), "nothing else was lost");
}

/// A host setter after the newest keyframe (its fingerprint stored in the setter's own
/// hold) that no keyframe followed costs exactness, counted as `config_moved`: the tail
/// past the keyframe may hold bytes processed under the new settings. The keyframe that
/// K4 forces next makes the restore exact again. The cell size the host stored rides the
/// header facts, for the restorer's scratch engine.
#[test]
fn a_setter_after_the_keyframe_is_counted_until_a_keyframe_follows() {
    use crate::writer::{store_cell_px, store_config_fp};
    let mut live = Live::v1(4, 30);
    store_config_fp(live.writer.store(), CONFIG).expect("config");
    live.slice(b"before the setter\r\n");
    let before = restored(live.recover(true));
    assert!(before.exact, "the host's word agrees: {:?}", before.losses);
    assert_eq!(before.header.cell_px, None, "never set");

    // A font zoom: the cell size and the fingerprint folding it in, in that order.
    store_cell_px(live.writer.store(), Some((18, 36))).expect("cell");
    store_config_fp(live.writer.store(), CONFIG + 1).expect("config");
    live.slice(b"after the setter\r\n");
    let moved = restored(live.recover(true));
    assert!(!moved.exact);
    assert_eq!(
        moved.losses,
        Losses {
            config_moved: true,
            ..Losses::default()
        },
        "nothing else was lost"
    );
    assert_eq!(moved.header.cell_px, Some((18, 36)));

    // K4: the forced keyframe, under the new fingerprint, ends the gap.
    let checkpoint = live.term.checkpoint_journal(512).expect("ground");
    live.writer
        .write_keyframe(&KeyframeInput {
            checkpoint: &checkpoint,
            fence: live.term.history_fence(),
            resize_lifetime: live.term.resize_lifetime(),
            resize_ordinal: live.term.resize_ordinal(),
            bridge_lines: 512,
            config_fp: CONFIG + 1,
            integration_on: live.term.shell_integration_posture()
                == aterm_core::terminal::ShellIntegrationPosture::On,
        })
        .expect("keyframe");
    let dead = live.writer.store().snapshot();
    let after = restored(recover(
        &dead,
        RecoverPolicy {
            tail: true,
            config_fp: CONFIG + 1,
            scratch_nonce: [0xA5; 32],
        },
        Terminal::new,
    ));
    assert!(after.exact, "{:?}", after.losses);
    assert_eq!(read_header(&dead).map(|h| h.cell_px), Ok(Some((18, 36))));
}

/// A tail that overflowed past the keyframe is not replayed onto it (replaying the rest
/// would skip bytes): the keyframe alone comes back, the overflow counted.
#[test]
fn an_overflowed_tail_is_counted_never_replayed_across_a_gap() {
    let geometry = Geometry::custom(64 * 1024, 256, 4096).expect("a small tail");
    let mut live = Live::new(4, 30, geometry);
    live.slice(b"kept\r\n");
    live.keyframe(64);
    for i in 0..40 {
        live.slice(format!("flood {i}\r\n").as_bytes());
    }
    let r = restored(live.recover(true));
    assert!(r.losses.tail_overflow);
    assert!(!r.exact);
    assert_eq!(r.replayed_records, 0);
    assert!(r.losses.tail_unreplayed > 0);
    let screen = Terminal::from_checkpoint(&r.checkpoint);
    assert_eq!(
        screen.row_text(0).as_deref().map(str::trim_end),
        Some("kept"),
        "the keyframe's screen, not a mix"
    );
}

/// Bytes the gather thread read and the parse thread never committed are counted.
#[test]
fn gathered_but_unprocessed_bytes_are_counted() {
    let mut live = Live::v1(4, 30);
    live.slice(b"processed\r\n");
    live.writer
        .set_gathered(live.gathered + 4096)
        .expect("gathered");
    let r = restored(live.recover(true));
    assert_eq!(r.losses.in_flight_lost, 4096);
    assert!(!r.exact);
}

/// A region a memory shed superseded is never called exact, whichever rung reads it: the
/// SUPERSEDED record it ends with is counted where the walk meets it, replayed or not, and
/// the screen it held is still the screen it restores (what a window that died before the
/// keeper took the new region's COMPANION gets back, pulsed to redraw over it).
#[test]
fn a_superseded_region_restores_its_screen_and_is_never_exact() {
    let mut live = Live::v1(4, 30);
    live.slice(b"before the shed\r\n");
    live.keyframe(0);
    live.slice(b"tail");
    let before = restored(live.recover(true));
    assert!(before.exact, "{:?}", before.losses);
    live.writer.append_superseded().expect("the last record");
    let r = restored(live.recover(true));
    assert!(r.losses.superseded);
    assert!(
        !r.exact,
        "the region speaks for the session only up to the shed"
    );
    assert_eq!(
        visible(&r.checkpoint),
        visible(&before.checkpoint),
        "the screen the region held"
    );
    let keyframe_only = restored(live.recover(false));
    assert!(
        keyframe_only.losses.superseded,
        "counted with no replay too: a fact of the region, not of the replay"
    );
    // Keyframed after its marker (a writer never does: nothing is written after it), the
    // marker is still in the walk's window, and still counted.
    live.keyframe(0);
    assert!(restored(live.recover(true)).losses.superseded);
}

/// A region that takes a session's journal over mid-stream (the memory shed's replacement)
/// continues the stream count: the gather thread's running count it inherits counts from
/// the session's first byte, and the restore counts in flight only what was gathered and
/// never processed. Resuming once anything was written is refused: the count it was
/// stamped with stands.
#[test]
fn a_region_resumed_mid_stream_counts_in_flight_only_what_it_missed() {
    let geometry = Geometry::v1();
    let mut live = Live {
        term: Terminal::new(4, 30),
        writer: JournalWriter::create(VecStore::new(geometry.region_len), geometry, &WRITER)
            .expect("created"),
        gathered: 5_000,
    };
    live.writer.resume_stream(5_000).expect("before anything");
    live.writer
        .set_gathered(5_000)
        .expect("the inherited count");
    live.term.process(b"before the takeover\r\n");
    live.keyframe(0);
    assert_eq!(
        live.writer.resume_stream(1),
        Err(WriteError::Sequence),
        "a keyframe was stamped"
    );
    live.slice(b"after it\r\n");
    let r = restored(live.recover(true));
    assert!(r.exact, "nothing missed: {:?}", r.losses);
    assert_eq!(r.keyframe.stream_at, 5_000);
    live.writer
        .set_gathered(live.gathered + 64)
        .expect("a read the parse thread never processed");
    let r = restored(live.recover(true));
    assert_eq!(r.losses.in_flight_lost, 64, "only what it missed");
}

/// A shed history ring reads as no history, never as a fault: its cap is 0, which a reader
/// refuses under any run, and the ring holds none; a run found under a cap of 0 (a writer
/// that exported into a shed ring, which the window's never does) is refused, not read.
#[test]
fn a_shed_ring_reads_as_no_history_and_a_run_under_it_is_refused() {
    let geometry = Geometry::custom(64 * 1024, 64 * 1024, 4096).expect("a small ring");
    let mut live = Live::new(4, 30, geometry);
    live.writer.shed_history().expect("shed");
    for i in 0..20 {
        live.slice(format!("history {i}\r\n").as_bytes());
    }
    live.keyframe(0);
    let r = restored(live.recover(true));
    assert_eq!(r.ring.fault, None, "an empty shed ring is no fault");
    assert!(r.ring.chunks.is_empty());
    assert!(
        r.exact,
        "the screen's exactness is the screen's: {:?}",
        r.losses
    );
    let facts = r.join_facts().expect("the head held");
    assert!(facts.export.is_none(), "nothing to join");
    assert!(
        facts.head.lines() > facts.carried,
        "the lines not carried are the join's to count"
    );

    // A run under the shed cap: the reader refuses the words rather than guess a modulus.
    let fence = live.term.history_fence();
    live.writer
        .reset_history(HistoryIdentity::of(&fence), fence.oldest, 4096, false)
        .expect("a reset gives the ring a cap");
    let lines = live
        .term
        .history_lines_since_fence(&fence, fence.oldest, 4)
        .expect("holds");
    live.writer
        .append_history(fence.oldest, &lines)
        .expect("fits");
    assert_eq!(
        restored(live.recover(true)).ring.fault,
        None,
        "a reset ring reads"
    );
    live.writer
        .shed_history()
        .expect("shed again, over the run");
    assert_eq!(
        restored(live.recover(true)).ring.fault,
        Some(RingFault::Words),
        "a run under a shed cap is never read"
    );
}

/// A resize the host applied with no output after it (so no RESIZE record) is counted,
/// as is a mark the keyframe budget left in the tail and a lost resize.
#[test]
fn unjournaled_resizes_lost_resizes_and_left_marks_are_counted() {
    let mut live = Live::v1(4, 30);
    live.term.resize(5, 30);
    live.writer
        .set_resize_word(live.term.resize_lifetime(), live.term.resize_ordinal())
        .expect("the host's word");
    let r = restored(live.recover(true));
    assert_eq!(r.losses.resizes_unjournaled, 1);
    assert!(!r.exact);

    // A journaled shrink replays exactly; a journaled grow is replayed too, and counted
    // (it depends on where the history sits, which no checkpoint carries).
    let mut journaled = Live::v1(4, 30);
    journaled.term.resize(3, 30);
    let (lifetime, ordinal) = (
        journaled.term.resize_lifetime(),
        journaled.term.resize_ordinal(),
    );
    journaled
        .writer
        .set_resize_word(lifetime, ordinal)
        .expect("word");
    journaled
        .writer
        .append_resize(
            3,
            30,
            aterm_core::grid::ResizePolicy::Native,
            lifetime,
            ordinal,
        )
        .expect("record");
    let r = restored(journaled.recover(true));
    assert_eq!(r.losses.resizes_unjournaled, 0);
    assert!(r.exact, "{:?}", r.losses);
    assert_eq!(r.checkpoint.rows, 3, "the shrink replays");
    journaled.term.resize(6, 30);
    let (lifetime, ordinal) = (
        journaled.term.resize_lifetime(),
        journaled.term.resize_ordinal(),
    );
    journaled
        .writer
        .set_resize_word(lifetime, ordinal)
        .expect("word");
    journaled
        .writer
        .append_resize(
            6,
            30,
            aterm_core::grid::ResizePolicy::Native,
            lifetime,
            ordinal,
        )
        .expect("record");
    let r = restored(journaled.recover(true));
    assert_eq!(r.checkpoint.rows, 6, "the grow replays");
    assert_eq!(r.losses.regrids_replayed, 1, "and is counted");
    assert!(!r.exact);

    let mut marked = Live::v1(4, 30);
    marked.writer.note_marks(1).expect("a mark past the budget");
    assert_eq!(restored(marked.recover(true)).losses.marks_lost, 1);

    let mut lost = Live::v1(4, 30);
    lost.writer.append_resizes_lost(2).expect("lost");
    let r = restored(lost.recover(true));
    assert_eq!(r.losses.resizes_lost, 2);
    assert!(!r.exact);
}

/// A region that is no journal, or a journal with no valid keyframe, takes the META rung.
#[test]
fn no_header_or_no_keyframe_is_the_meta_rung() {
    let blank = VecStore::new(Geometry::v1().region_len);
    assert_eq!(
        recover(
            &blank,
            RecoverPolicy {
                tail: true,
                config_fp: 0,
                scratch_nonce: [0xA5; 32],
            },
            Terminal::new
        ),
        Recovery::Meta(MetaWhy::Header(HeaderFault::Magic))
    );
    let geometry = Geometry::v1();
    let writer = JournalWriter::create(VecStore::new(geometry.region_len), geometry, &WRITER)
        .expect("created");
    match recover(
        writer.store(),
        RecoverPolicy {
            tail: true,
            config_fp: 0,
            scratch_nonce: [0xA5; 32],
        },
        Terminal::new,
    ) {
        Recovery::Meta(MetaWhy::NoKeyframe([SlotVerdict::Empty, SlotVerdict::Empty])) => {}
        other => panic!("{other:?}"),
    }
    let live = Live::v1(4, 20);
    let dead = live.writer.store().snapshot();
    dead.store_u64_release(crate::layout::OFF_MAGIC, {
        let mut magic = [0u8; 8];
        magic[..4].copy_from_slice(&crate::layout::MAGIC);
        magic[4] = crate::layout::LAYOUT_VERSION + 1;
        u64::from_le_bytes(magic)
    })
    .expect("store");
    assert_eq!(
        recover(
            &dead,
            RecoverPolicy {
                tail: true,
                config_fp: 0,
                scratch_nonce: [0xA5; 32],
            },
            Terminal::new
        ),
        Recovery::Meta(MetaWhy::Header(HeaderFault::Version(
            crate::layout::LAYOUT_VERSION + 1
        )))
    );
}

/// The history ring's run comes back across its wrap, in order, under its identity; its
/// admission holds when nothing moved the identity, and is refused across a clear in the
/// replayed tail or a ring left under an older identity.
#[test]
fn the_ring_run_reads_back_and_its_admission_checks_identity() {
    let geometry = Geometry::custom(64 * 1024, 64 * 1024, 2048).expect("a small ring");
    let mut live = Live::new(4, 30, geometry);
    for i in 0..400 {
        live.slice(format!("history {i}\r\n").as_bytes());
    }
    let fence = live.term.history_fence();
    live.writer
        .reset_history(HistoryIdentity::of(&fence), fence.oldest, 2048, false)
        .expect("reset");
    let mut row = fence.oldest;
    while row < fence.end {
        let lines = live
            .term
            .history_lines_since_fence(&fence, row, 10)
            .expect("holds");
        live.writer.append_history(row, &lines).expect("fits");
        row += lines.len() as u64;
    }
    live.keyframe(512);
    let r = restored(live.recover(true));
    assert_eq!(r.ring.fault, None);
    assert_eq!(r.ring.end_row, fence.end);
    assert!(
        r.ring.lines() > 0 && r.ring.lines() < fence.lines(),
        "the oldest were evicted"
    );
    let mut expected = r.ring.first_row().expect("a run");
    for chunk in &r.ring.chunks {
        assert_eq!(chunk.first_row, expected);
        let lines = chunk.lines().expect("decodes");
        let want = live
            .term
            .history_lines_since_fence(&fence, chunk.first_row, lines.len())
            .expect("holds");
        assert_eq!(
            aterm_core::scrollback::serialize_lines(&lines),
            aterm_core::scrollback::serialize_lines(&want)
        );
        expected += u64::from(chunk.n_lines);
        assert!(chunk.n_lines as usize <= CHUNK_LINES);
    }
    assert!(r.history_admissible());

    // A clear inside the replayed tail: the scratch engine's identity moved.
    live.slice(b"\x1b[3J");
    let cleared = restored(live.recover(true));
    assert!(!cleared.history_admissible(), "never across a clear");

    // A keyframe after the clear, the ring still under the old identity.
    live.keyframe(512);
    let stale = restored(live.recover(true));
    assert!(!stale.history_admissible(), "never from an older identity");
}

/// Panic-free on any bytes: random regions, and a valid region with random bytes
/// overwritten anywhere, recover to a screen or the META rung and never panic.
#[test]
fn recovery_is_panic_free_on_any_bytes() {
    let geometry = Geometry::custom(16 * 1024, 2048, 2048).expect("a small region");
    let mut live = Live::new(4, 20, geometry);
    for i in 0..10 {
        live.slice(format!("\x1b[3{}mrow {i}\r\n", i % 8).as_bytes());
    }
    live.keyframe(16);
    live.slice(b"tail \x1b[1mbold");
    let fence = live.term.history_fence();
    live.writer
        .reset_history(HistoryIdentity::of(&fence), fence.oldest, 2048, false)
        .expect("reset");
    let lines = live
        .term
        .history_lines_since_fence(&fence, fence.oldest, 4)
        .expect("holds");
    live.writer
        .append_history(fence.oldest, &lines)
        .expect("fits");
    let valid = live.writer.store().snapshot();
    let len = valid.len();
    let mut image = vec![0u8; len];
    valid.read(0, &mut image).expect("whole");

    let mut rng = XorShift::new(0x00C0_FFEE);
    let policy = RecoverPolicy {
        tail: true,
        config_fp: CONFIG,
        scratch_nonce: [0xA5; 32],
    };
    // A header word at random: small (inside the sections) or near the top of the
    // range, where unchecked arithmetic on it would overflow.
    let near = |r: u64| {
        let small = (r >> 1) % (1 << 20);
        if r & 1 == 0 { small } else { u64::MAX - small }
    };
    for round in 0usize..400 {
        let damaged = VecStore::new(len);
        damaged.write(0, &image).expect("copy");
        // Overwrite a few spans: header words, slots, tail and ring alike.
        for _ in 0..1 + rng.next_u64() % 4 {
            let at = (rng.next_u64() as usize) % len;
            let n = 1 + (rng.next_u64() as usize) % 16;
            let junk: Vec<u8> = (0..n).map(|_| rng.next_u64() as u8).collect();
            let _ = damaged.write(at.min(len - 1), &junk[..n.min(len - at.min(len - 1))]);
        }
        if round.is_multiple_of(4) {
            // The header words that bound everything else, at random.
            for word in [
                OFF_TAIL_COMMIT,
                OFF_TAIL_FLOOR,
                OFF_HIST_LO,
                OFF_HIST_HI,
                OFF_HIST_CAP,
                kf_len_word(0),
                kf_len_word(1),
            ] {
                if rng.next_u64().is_multiple_of(2) {
                    damaged
                        .store_u64_release(word, near(rng.next_u64()))
                        .expect("aligned");
                }
            }
        }
        let _ = recover(&damaged, policy, Terminal::new);
    }
    // Positions near the top of the u64 range, consistent enough to pass the window
    // checks and reach the walks: the floor and the commit within a ring of each other,
    // the run's words within the cap, so every step of the walk sits near u64::MAX
    // (overflow checks are on in this build; a wrapping add there panics).
    // Within 96 bytes of the top, a position's distance to the ring's end is more than
    // its distance to u64::MAX (both rings are powers of two), so every step past it
    // overflows unless checked, the gap under a header before the ring's end included.
    let hist_cap = 2048u64;
    for _ in 0..200 {
        let damaged = VecStore::new(len);
        damaged.write(0, &image).expect("copy");
        let top = u64::MAX - rng.next_u64() % 48;
        let floor = top - rng.next_u64() % 48;
        let lo = u64::MAX - rng.next_u64() % 96;
        let hi = lo.saturating_add(rng.next_u64() % 96);
        for (word, value) in [
            (OFF_TAIL_COMMIT, top),
            (OFF_TAIL_FLOOR, floor),
            (OFF_HIST_CAP, hist_cap),
            (OFF_HIST_LO, lo),
            (OFF_HIST_HI, hi.max(lo)),
        ] {
            damaged.store_u64_release(word, value).expect("aligned");
        }
        let _ = recover(&damaged, policy, Terminal::new);
    }
    let random = VecStore::new(len);
    for round in 0usize..50 {
        let junk: Vec<u8> = (0..len).map(|_| rng.next_u64() as u8).collect();
        random.write(0, &junk).expect("fill");
        // A valid magic and length, so the walk reaches the sections.
        if round.is_multiple_of(2) {
            random.write(0, &image[..256]).expect("a real header");
        }
        let _ = recover(&random, policy, Terminal::new);
    }
}

/// A writer stopped between a floor that evicted the whole tail window and the commit of
/// the record it made room for leaves a floor past the commit: the keyframe's tail is
/// gone, which is an overflow (counted, the keyframe alone), not damage.
#[test]
fn a_death_after_a_floor_that_evicted_everything_is_an_overflow() {
    let geometry = Geometry::custom(64 * 1024, 64, 4096).expect("a one-record ring");
    let build = |budget: Option<usize>| {
        let store = crate::store::CountdownStore::new(geometry.region_len, budget);
        let mut writer = JournalWriter::create(store, geometry, &WRITER).expect("created");
        let mut term = Terminal::new(4, 30);
        term.process(b"kept\r\n");
        let checkpoint = term.checkpoint_journal(0).expect("ground");
        writer
            .write_keyframe(&KeyframeInput {
                checkpoint: &checkpoint,
                fence: term.history_fence(),
                resize_lifetime: term.resize_lifetime(),
                resize_ordinal: term.resize_ordinal(),
                bridge_lines: 0,
                config_fp: CONFIG,
                integration_on: term.shell_integration_posture()
                    == aterm_core::terminal::ShellIntegrationPosture::On,
            })
            .expect("keyframe");
        writer
            .append_slice(&[b'x'; 40])
            .expect("one record, [0, 56)");
        let before = writer.store().stores();
        // The next record cannot fit before the ring's end: it goes to 64 and its floor
        // evicts the only record, landing past the commit at 56.
        writer.append_slice(&[b'y'; 40]).expect("the second record");
        (writer, before)
    };
    let (_, before) = build(None);
    let (dying, _) = build(Some(before + 1));
    assert!(dying.store().died());
    let dead = dying.store().snapshot();
    assert!(
        dead.load_u64_acquire(OFF_TAIL_FLOOR).expect("floor")
            > dead.load_u64_acquire(OFF_TAIL_COMMIT).expect("commit")
    );
    let r = restored(recover(
        &dead,
        RecoverPolicy {
            tail: true,
            config_fp: CONFIG,
            scratch_nonce: [0xA5; 32],
        },
        Terminal::new,
    ));
    assert!(r.losses.tail_overflow, "{:?}", r.losses);
    assert_eq!(r.losses.tail_fault, None);
    assert_eq!(
        r.losses.tail_unreplayed, 40,
        "the first record's bytes are counted"
    );
    assert!(!r.exact);
    assert_eq!(
        Terminal::from_checkpoint(&r.checkpoint)
            .row_text(0)
            .as_deref()
            .map(str::trim_end),
        Some("kept")
    );
}

/// Header words are absolute counters, not wrapping ring offsets. Both the skipped
/// tail gap and history wrap must refuse an unrepresentable next boundary.
#[test]
fn absolute_ring_counters_refuse_wraparound_without_reading_it() {
    let geometry = Geometry::custom(64 * 1024, 256, 2048).expect("small region");
    let live = Live::new(4, 24, geometry);
    assert!(restored(live.recover(true)).exact, "ordinary zero counters");
    let mut outcomes = Vec::new();
    for (remaining, pad) in [(7, false), (15, false), (31, false), (31, true)] {
        let dead = live.writer.store().snapshot();
        let floor = u64::MAX - remaining;
        dead.store_u64_release(OFF_TAIL_FLOOR, floor)
            .expect("floor");
        dead.store_u64_release(OFF_TAIL_COMMIT, u64::MAX)
            .expect("commit");
        if remaining == 31 {
            let mut record = Vec::new();
            if pad {
                record.extend_from_slice(&crate::records::pad_header(32, 0));
            } else {
                TailRecord::Bytes {
                    synthesized: false,
                    bytes: &[0; 16],
                }
                .encode(0, &mut record);
            }
            let off = usize::try_from(floor % geometry.tail_cap as u64).expect("offset");
            dead.write(geometry.tail_off + off, &record)
                .expect("bounded record");
        }
        let result = std::panic::catch_unwind(|| {
            recover(
                &dead,
                RecoverPolicy {
                    tail: true,
                    config_fp: CONFIG,
                    scratch_nonce: [0xA5; 32],
                },
                Terminal::new,
            )
        });
        outcomes.push(
            result.is_ok_and(|r| restored(r).losses.tail_fault == Some(TailFault::PastCommit)),
        );
    }

    // Refuse physical history reads so even an old release build cannot loop around
    // the wrapped absolute boundary; also observe that a correct reader never asks.
    struct HistoryReadFence<'a> {
        store: &'a VecStore,
        off: usize,
        reads: std::sync::atomic::AtomicUsize,
    }
    impl RegionView for HistoryReadFence<'_> {
        fn len(&self) -> usize {
            self.store.len()
        }
        fn read(&self, at: usize, out: &mut [u8]) -> Result<(), crate::store::StoreError> {
            if at >= self.off {
                self.reads
                    .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                return Err(crate::store::StoreError::ReadOnly);
            }
            self.store.read(at, out)
        }
        fn load_u64_acquire(&self, at: usize) -> Result<u64, crate::store::StoreError> {
            self.store.load_u64_acquire(at)
        }
    }
    let dead = live.writer.store().snapshot();
    dead.store_u64_release(OFF_HIST_CAP, 2048).expect("cap");
    dead.store_u64_release(OFF_HIST_LO, u64::MAX - 7)
        .expect("lo");
    dead.store_u64_release(OFF_HIST_HI, u64::MAX).expect("hi");
    let fenced = HistoryReadFence {
        store: &dead,
        off: geometry.hist_off,
        reads: std::sync::atomic::AtomicUsize::new(0),
    };
    let result = std::panic::catch_unwind(|| read_ring(&fenced, &geometry));
    outcomes.push(
        result.is_ok_and(|r| r.fault == Some(RingFault::Words))
            && fenced.reads.load(std::sync::atomic::Ordering::Relaxed) == 0,
    );
    assert_eq!(outcomes, vec![true; 5]);
}

/// An early damaged record may salvage a validated prefix, never an unchecked
/// identity or a gap hidden in that prefix.
#[test]
fn every_salvaged_history_prefix_is_valid_before_admission() {
    use crate::records::encode_history_chunk;
    use aterm_core::scrollback::Line;
    let geometry = Geometry::custom(64 * 1024, 256, 2048).expect("small region");
    let mut live = Live::new(4, 24, geometry);
    let identity = HistoryIdentity::of(&live.term.history_fence());
    live.writer
        .reset_history(identity, 0, 2048, false)
        .expect("reset");
    let line = [Line::from("history")];
    let first = encode_history_chunk(0, 0, identity, &line).expect("first");
    let next = encode_history_chunk(1, 1, identity, &line).expect("next");
    let recover_records = |records: &[Vec<u8>], end_row| {
        let dead = live.writer.store().snapshot();
        let bytes: Vec<u8> = records.iter().flat_map(|r| r.iter().copied()).collect();
        assert!(bytes.len() < geometry.hist_cap_phys);
        dead.write(geometry.hist_off, &bytes)
            .expect("bounded chunks");
        for (word, value) in [
            (OFF_HIST_LO, 0),
            (OFF_HIST_HI, bytes.len() as u64),
            (OFF_HIST_LO_SEQ, 0),
            (OFF_HIST_END_ROW, end_row),
        ] {
            dead.store_u64_release(word, value).expect("header");
        }
        restored(recover(
            &dead,
            RecoverPolicy {
                tail: true,
                config_fp: CONFIG,
                scratch_nonce: [0xA5; 32],
            },
            Terminal::new,
        ))
    };
    let ordinary = recover_records(&[first.clone(), next], 2);
    assert!(ordinary.history_admissible());
    assert_eq!(
        (
            ordinary.ring.fault.as_ref(),
            ordinary.ring.lines(),
            ordinary.ring.end_row
        ),
        (None, 2, 2)
    );
    let bad_head =
        |seq| astream_wire::steps::encode_step(seq, &[2]).expect("valid CRC, bad HCR head");
    let salvage = recover_records(&[first.clone(), bad_head(1)], 2);
    assert!(salvage.history_admissible());
    assert_eq!(salvage.ring.lines(), 1, "valid prefix is retained");
    assert!(matches!(salvage.ring.fault, Some(RingFault::Head(_))));

    let mut wrong_identity = identity;
    wrong_identity.clear_gen += 1;
    let foreign = encode_history_chunk(0, 0, wrong_identity, &line).expect("foreign");
    let foreign_head = recover_records(&[foreign.clone(), bad_head(1)], 2);
    let mut broken_frame = encode_history_chunk(1, 1, identity, &line).expect("next frame");
    *broken_frame.last_mut().expect("body") ^= 0x80;
    let foreign_frame = recover_records(&[foreign, broken_frame], 2);
    let gap = encode_history_chunk(1, 2, identity, &line).expect("gap");
    let gap = recover_records(&[first, gap, bad_head(2)], 3);
    let foreign_refused = [foreign_head, foreign_frame]
        .iter()
        .all(|r| r.ring.chunks.is_empty() && r.ring.fault == Some(RingFault::Identity));
    let overflow = encode_history_chunk(0, u64::MAX, identity, &line).expect("overflowing rows");
    let overflow = recover_records(&[overflow, bad_head(1)], u64::MAX);
    let overflow_refused =
        overflow.ring.chunks.is_empty() && overflow.ring.fault == Some(RingFault::Rows);
    let gap_refused =
        gap.ring.lines() == 1 && gap.ring.end_row == 1 && gap.ring.fault == Some(RingFault::Rows);
    let past_end = encode_history_chunk(0, 2, identity, &line).expect("past declared end");
    let past_end = recover_records(&[past_end, bad_head(1)], 1);
    let past_end_refused =
        past_end.ring.chunks.is_empty() && past_end.ring.fault == Some(RingFault::Rows);
    assert_eq!(
        (
            salvage.ring.end_row,
            foreign_refused,
            gap_refused,
            overflow_refused,
            past_end_refused
        ),
        (1, true, true, true, true)
    );
}

/// A verified mark's slice is counted before its append, so a writer that dies after the
/// append and before the K6 keyframe the mark triggers commits (before its begin, or
/// inside its encode and copy, once a prompt) restores as NOT exact with the mark
/// counted, never as exact without the prompt's state; once that keyframe commits, the
/// restore holds the mark and is exact.
#[test]
fn a_death_before_a_marks_keyframe_counts_the_mark() {
    use crate::policy::{KeyframePolicy, Reason};
    use aterm_core::terminal::ShellIntegrationNonce;
    let key = [0x3C; 32];
    let mut live = Live::v1(10, 40);
    live.term.set_require_shell_integration_nonce(true);
    live.term.authorize_shell_integration(key);
    live.keyframe(512);
    let mut policy = KeyframePolicy::new(live.writer.geometry().tail_cap, 0, CONFIG);
    let bytes = format!(
        "\x1b]133;A;id={}\x07$ ",
        ShellIntegrationNonce(key).to_hex()
    );
    live.gathered += bytes.len() as u64;
    live.writer.set_gathered(live.gathered).expect("gathered");
    let before = live.term.shell_marks_verified();
    live.term.process(bytes.as_bytes());
    let verified = live.term.shell_marks_verified() - before;
    assert_eq!(verified, 1);
    live.writer
        .note_marks(verified)
        .expect("counted before the append");
    live.writer
        .append_slice(bytes.as_bytes())
        .expect("appended");
    let decision = policy.after_slice(500, live.writer.tail_since_keyframe(), verified);
    assert_eq!(decision.keyframe, Some(Reason::Mark));

    let checkpoint = live.term.checkpoint_journal(512).expect("ground");
    let input = KeyframeInput {
        checkpoint: &checkpoint,
        fence: live.term.history_fence(),
        resize_lifetime: live.term.resize_lifetime(),
        resize_ordinal: live.term.resize_ordinal(),
        bridge_lines: 512,
        config_fp: CONFIG,
        integration_on: live.term.shell_integration_posture()
            == aterm_core::terminal::ShellIntegrationPosture::On,
    };
    let before_begin = live.recover(true);
    live.writer.begin_keyframe(&input).expect("begun");
    let mid_keyframe = live.recover(true);
    for (when, r) in [
        ("before its begin", before_begin),
        ("mid-keyframe", mid_keyframe),
    ] {
        let r = restored(r);
        assert!(!r.exact, "{when}: a dropped mark is never exact");
        assert_eq!(r.losses.marks_lost, 1, "{when}: the mark is counted");
    }
    live.writer.finish_keyframe().expect("committed");
    let r = restored(live.recover(true));
    assert!(r.exact, "{:?}", r.losses);
    assert_eq!(
        Terminal::from_checkpoint(&r.checkpoint)
            .checkpoint_journal(0)
            .expect("ground")
            .shell,
        live.term.checkpoint_journal(0).expect("ground").shell,
        "the keyframe holds the prompt's state"
    );
}

/// A replay that ends inside a sequence the restored engine can only swallow is not
/// exact, whatever else held: an APC whose consumer started (a kitty-graphics query the
/// live engine answers at its terminator, the restored one never) is counted, and the
/// restored engine swallows the rest of it rather than print it.
#[test]
fn a_replay_ending_inside_a_swallowed_sequence_is_not_exact() {
    let mut live = Live::v1(6, 40);
    live.slice(b"$ icat\r\n");
    live.slice(b"\x1b_Gi=31,s=1,v=1,a=q,t=d,f=24;AAAA");
    assert!(
        live.term.checkpoint_journal(0).is_none(),
        "the live engine would defer its keyframe here (K5)"
    );
    let r = restored(live.recover(true));
    assert!(!r.exact, "the query's reply is lost: {:?}", r.losses);
    assert_eq!(r.losses.swallowed, Some("SosPmApcString"));
    let mut swallowed = r.losses.clone();
    swallowed.swallowed = None;
    assert_eq!(swallowed, Losses::default(), "nothing else was lost");
    // The rest of the string reaches the restored engine as the live one: swallowed.
    let mut restored = Terminal::from_checkpoint(&r.checkpoint);
    restored.process(b"\x1b\\after");
    live.term.process(b"\x1b\\after");
    assert_eq!(
        restored.row_text(1).as_deref().map(str::trim_end),
        live.term.row_text(1).as_deref().map(str::trim_end),
        "no garbage where the string ended"
    );
}

/// A replay that ends inside a hooked DCS query (whose reply lives in its handler, so no
/// carry holds it) restores the screen the replay reached, the query swallowed and
/// counted, not the keyframe alone: a keyframe from before an ED3 the tail replayed would
/// bring back the screen and the history the clear removed.
#[test]
fn a_replay_ending_inside_a_query_keeps_what_it_replayed() {
    let mut live = Live::new(6, 40, Geometry::v1());
    for i in 0..50 {
        live.slice(format!("secret {i}\r\n").as_bytes());
    }
    live.keyframe(512);
    live.slice(b"\x1b[H\x1b[2J\x1b[3Jcleared\r\n");
    live.slice(b"\x1bP+q544e");
    assert!(
        live.term.checkpoint_journal(512).is_none(),
        "the query defers the clear's keyframe (K5)"
    );
    let r = restored(live.recover(true));
    assert!(!r.exact);
    assert_eq!(r.losses.swallowed, Some("DcsPassthrough"));
    assert_eq!(
        r.checkpoint.history_lines, 0,
        "the clear's history stays cleared"
    );
    let mut restored = Terminal::from_checkpoint(&r.checkpoint);
    assert_eq!(
        restored.row_text(0).as_deref().map(str::trim_end),
        Some("cleared"),
        "the replayed screen, not the keyframe's"
    );
    // The query's tail is swallowed, not printed: the live engine prints nothing of it
    // either (it answers it instead, a reply the restore counts as lost).
    restored.process(b"\x1b\\$ ");
    live.term.process(b"\x1b\\$ ");
    assert_eq!(
        restored.row_text(1).as_deref().map(str::trim_end),
        live.term.row_text(1).as_deref().map(str::trim_end)
    );
}

/// A writer that dies anywhere inside a history append leaves a ring run that is the run
/// before the append or the run after it: never empty where both were whole. Two windows
/// once threw the whole run away: between the end row's store and the commit (the
/// committed run ends below the end row the uncommitted chunk stored), and between an
/// eviction's sequence word and its position word (the run's first chunk is one the
/// eviction was about to drop, still intact because nothing is copied before both
/// words).
#[test]
fn a_death_inside_a_history_append_keeps_the_committed_run() {
    use crate::store::CountdownStore;
    let geometry = Geometry::custom(16 * 1024, 1024, 2048).expect("a small ring");
    let mut t = Terminal::new(4, 30);
    for i in 0..200 {
        t.process(format!("history line {i:03} of the ring\r\n").as_bytes());
    }
    let fence = t.history_fence();
    let identity = HistoryIdentity::of(&fence);
    let mut chunks = Vec::new();
    let mut row = fence.oldest;
    while row + 10 <= fence.end {
        let lines = t.history_lines_since_fence(&fence, row, 10).expect("holds");
        chunks.push((row, lines));
        row += 10;
    }
    let checkpoint = t.checkpoint_journal(0).expect("ground");
    // The region after the first `n` appends, the writer dying after `budget` stores
    // when given; also where the last append's stores begin and how many it made.
    let build = |n: usize, budget: Option<usize>| {
        let mut writer = JournalWriter::create(
            CountdownStore::new(geometry.region_len, budget),
            geometry,
            &WRITER,
        )
        .expect("created");
        writer
            .write_keyframe(&KeyframeInput {
                checkpoint: &checkpoint,
                fence,
                resize_lifetime: t.resize_lifetime(),
                resize_ordinal: t.resize_ordinal(),
                bridge_lines: 0,
                config_fp: CONFIG,
                integration_on: t.shell_integration_posture()
                    == aterm_core::terminal::ShellIntegrationPosture::On,
            })
            .expect("keyframe");
        writer
            .reset_history(identity, fence.oldest, 2048, false)
            .expect("reset");
        for (first, lines) in &chunks[..n - 1] {
            writer.append_history(*first, lines).expect("fits");
        }
        let start = writer.store().stores();
        let (first, lines) = &chunks[n - 1];
        writer.append_history(*first, lines).expect("fits");
        (
            writer.store().snapshot(),
            start,
            writer.store().stores() - start,
        )
    };
    let run_of = |region: &VecStore| {
        let r = restored(recover(
            region,
            RecoverPolicy {
                tail: true,
                config_fp: CONFIG,
                scratch_nonce: [0xA5; 32],
            },
            Terminal::new,
        ));
        (
            r.ring.first_row(),
            r.ring.end_row,
            r.ring.lines(),
            r.ring.fault,
        )
    };
    let mut windows = 0;
    for n in 2..=chunks.len() {
        let before = run_of(&build(n - 1, None).0);
        let (after_region, start, stores) = build(n, None);
        let after = run_of(&after_region);
        assert_eq!(
            (before.3.clone(), after.3.clone()),
            (None, None),
            "append {n}"
        );
        for budget in 0..stores {
            let got = run_of(&build(n, Some(start + budget)).0);
            let whole = got.3.is_none()
                && [before.0, after.0].contains(&got.0)
                && [before.1, after.1].contains(&got.1)
                && got.2 > 0;
            assert!(
                whole,
                "append {n}, a death after {budget} of its {stores} stores: {got:?} (before \
                 {before:?}, after {after:?})"
            );
            windows += 1;
        }
    }
    assert!(windows > 50, "{windows} death points");
}

/// A resize a keyframe absorbed (the capture seeds the journal's watermark to the
/// engine's ordinal, so no RESIZE record follows it) is journaled by that keyframe: only
/// RESIZE records past the keyframe's `tail_from` speak for later resizes, and an older
/// record before it says nothing about the host's word.
#[test]
fn a_resize_the_keyframe_absorbed_is_not_unjournaled() {
    let mut live = Live::v1(6, 30);
    live.term.resize(5, 30);
    let (lifetime, first) = (live.term.resize_lifetime(), live.term.resize_ordinal());
    live.writer
        .set_resize_word(lifetime, first)
        .expect("the host's word");
    live.writer
        .append_resize(
            5,
            30,
            aterm_core::grid::ResizePolicy::Native,
            lifetime,
            first,
        )
        .expect("journaled");
    live.slice(b"between\r\n");
    live.term.resize(4, 30);
    let second = live.term.resize_ordinal();
    assert!(second > first);
    live.writer
        .set_resize_word(lifetime, second)
        .expect("the host's word");
    // A quiet keyframe absorbs the second resize: no record for it.
    live.keyframe(512);
    let r = restored(live.recover(true));
    assert_eq!(r.keyframe.resize_ordinal, second);
    assert_eq!(r.losses.resizes_unjournaled, 0, "{:?}", r.losses);
    assert!(r.exact, "{:?}", r.losses);
}

/// Export the whole history of `live`'s engine into the ring (a fresh reset at its
/// oldest), as the window's journal worker does.
fn export_all(live: &mut Live, cap: usize) -> HistoryFence {
    let fence = live.term.history_fence();
    live.writer
        .reset_history(HistoryIdentity::of(&fence), fence.oldest, cap, false)
        .expect("reset");
    let mut row = fence.oldest;
    while row < fence.end {
        let lines = live
            .term
            .history_lines_since_fence(&fence, row, 64)
            .expect("holds");
        live.writer.append_history(row, &lines).expect("fits");
        row += lines.len() as u64;
    }
    fence
}

/// The history join's inputs (the plan's §3.2 N3): the head is the keyframe's fence moved
/// on by what the replay pushed, the carried lines the scratch engine's history, and the
/// ring's run an export fenced under the keyframe's identity with its end clamped to the
/// head's. The rows line up: the ring's lines up to where the carried ones begin, then the
/// carried ones, are the live history, line for line.
#[test]
fn the_join_facts_line_the_ring_up_below_the_replay() {
    let mut live = Live::v1(6, 40);
    for i in 0..300 {
        live.slice(format!("before the keyframe {i}\r\n").as_bytes());
    }
    export_all(&mut live, 1 << 20);
    // A keyframe with a 20-line bridge, then a tail that pushes 50 more lines.
    live.keyframe(20);
    for i in 0..50 {
        live.slice(format!("in the tail {i}\r\n").as_bytes());
    }
    let r = restored(live.recover(true));
    assert!(r.exact, "{:?}", r.losses);
    let facts = r.join_facts().expect("the identity held");
    let fence = live.term.history_fence();
    assert_eq!(
        facts.head.end, fence.end,
        "the head is the live history's end"
    );
    assert_eq!(facts.head.oldest, r.keyframe.fence.oldest);
    assert_eq!(facts.carried, 70, "the bridge and the replay's pushes");
    assert_eq!(
        r.fence_before.end, r.keyframe.fence.end,
        "the same numbering"
    );
    let export = facts.export.expect("a ring under the keyframe's identity");
    assert_eq!(export.first, fence.oldest);
    assert_eq!(export.skip, 0);
    assert_eq!(
        export.fence.end, r.keyframe.fence.end,
        "the ring ends where it was exported"
    );
    assert!(export.fence.holds_at(&facts.head), "the join's fence holds");
    // What a join takes: the ring's lines from its first to where the carried ones begin.
    let carried_from = facts.head.end - facts.carried;
    let take = carried_from - export.first;
    let mut ring_lines = Vec::new();
    let (chunks, skip) = r.ring_chunks_for(&export, take);
    assert_eq!(skip, 0, "nothing below the run's first row");
    for chunk in &r.ring.chunks[chunks] {
        ring_lines.extend(chunk.lines().expect("decodes"));
    }
    assert!(ring_lines.len() as u64 >= take, "the chunks hold the take");
    ring_lines.truncate(usize::try_from(take).expect("take"));
    let restored_engine = Terminal::from_checkpoint(&r.checkpoint);
    let restored_fence = restored_engine.history_fence();
    let carried = restored_engine
        .history_lines_since_fence(&restored_fence, restored_fence.oldest, 1 << 16)
        .expect("holds");
    assert_eq!(
        restored_fence.oldest, carried_from,
        "the carried lines' rows"
    );
    let joined: Vec<_> = ring_lines.into_iter().chain(carried).collect();
    let want = live
        .term
        .history_lines_since_fence(&fence, fence.oldest, 1 << 16)
        .expect("holds");
    assert_eq!(
        aterm_core::scrollback::serialize_lines(&joined),
        aterm_core::scrollback::serialize_lines(&want),
        "the joined history is the live history"
    );
}

/// The ring's lines the dead engine had already evicted when it keyframed are skipped,
/// never brought back: the export starts at the head's oldest row.
#[test]
fn the_join_never_reaches_below_the_head_s_oldest_row() {
    let mut live = Live::v1(6, 40);
    live.term.set_scrollback_line_limit(Some(100));
    for i in 0..150 {
        live.slice(format!("line {i}\r\n").as_bytes());
    }
    let exported = export_all(&mut live, 1 << 20);
    for i in 150..180 {
        live.slice(format!("line {i}\r\n").as_bytes());
    }
    live.keyframe(40);
    let r = restored(live.recover(true));
    let facts = r.join_facts().expect("held");
    let export = facts.export.expect("a ring");
    assert!(
        facts.head.oldest > exported.oldest,
        "premise: the engine evicted"
    );
    assert_eq!(export.first, facts.head.oldest);
    assert_eq!(export.skip, facts.head.oldest - exported.oldest);
}

/// A ring far older than the engine (it kept exporting while the engine's line limit
/// evicted): the chunks an import decodes start at the one holding the export's first
/// row, never a chunk wholly below it, and skip fewer lines than that chunk holds; the
/// lines they give from there are the live history's at the same rows. And the newest
/// bound (`RingExport::newest`, an update's `MAX_EXPORT_LINES`) moves the export's first
/// row up, its skip with it, so a join takes no more than the bound and counts the rest.
#[test]
fn an_import_decodes_only_the_chunks_it_takes_within_the_newest_bound() {
    let mut live = Live::v1(6, 40);
    live.term.set_scrollback_line_limit(Some(100));
    let fence = live.term.history_fence();
    live.writer
        .reset_history(HistoryIdentity::of(&fence), fence.oldest, 1 << 20, false)
        .expect("reset");
    let mut row = fence.oldest;
    for batch in 0..12 {
        for i in 0..50 {
            live.slice(format!("line {}\r\n", batch * 50 + i).as_bytes());
        }
        // The worker's export after each batch: never behind the engine's eviction.
        let f = live.term.history_fence();
        while row < f.end {
            let lines = live
                .term
                .history_lines_since_fence(&f, row, 64)
                .expect("holds");
            live.writer.append_history(row, &lines).expect("fits");
            row += lines.len() as u64;
        }
    }
    live.keyframe(40);
    let r = restored(live.recover(true));
    let facts = r.join_facts().expect("held");
    let export = facts.export.expect("a ring");
    assert!(
        export.skip > 3 * 64,
        "premise: whole chunks of the ring lie below the engine's oldest row: {export:?}"
    );
    let carried_from = facts.head.end - facts.carried;
    let lines_of = |export: &RingExport, take: u64| {
        let (range, skip) = r.ring_chunks_for(export, take);
        assert!(
            range.start > 0,
            "the chunks wholly below the first row stay behind"
        );
        assert!(
            skip < u64::from(r.ring.chunks[range.start].n_lines),
            "the import skips less than one chunk: {skip}"
        );
        let mut lines = Vec::new();
        for chunk in &r.ring.chunks[range] {
            lines.extend(chunk.lines().expect("decodes"));
        }
        let lines: Vec<_> = lines
            .into_iter()
            .skip(usize::try_from(skip).expect("skip"))
            .take(usize::try_from(take).expect("take"))
            .collect();
        aterm_core::scrollback::serialize_lines(&lines)
    };
    let live_lines = |from: u64, n: u64| {
        let f = live.term.history_fence();
        aterm_core::scrollback::serialize_lines(
            &live
                .term
                .history_lines_since_fence(&f, from, usize::try_from(n).expect("n"))
                .expect("holds"),
        )
    };
    let take = carried_from - export.first;
    assert_eq!(
        lines_of(&export, take),
        live_lines(export.first, take),
        "the live history below the carried lines"
    );

    // The newest 80 lines: 40 of the ring's below the 40 the checkpoint carries.
    let newest = export.newest(80);
    assert_eq!(newest.first, export.fence.end - 80);
    assert_eq!(newest.fence.oldest, newest.first);
    assert_eq!(newest.lines, 80);
    assert_eq!(newest.skip, export.skip + (newest.first - export.first));
    assert_eq!(
        export.newest(u64::MAX),
        export,
        "a bound past the ring changes nothing"
    );
    let take = carried_from - newest.first;
    assert_eq!(take, 40);
    assert_eq!(lines_of(&newest, take), live_lines(newest.first, take));
}

/// No head across a clear or a reflow inside the replay: a clear counts nothing (the
/// person erased what the ring held), a reflow counts the keyframe's lines below its
/// bridge. A ring under another identity than the keyframe's is no export.
#[test]
fn a_clear_or_a_reflow_in_the_tail_leaves_no_head() {
    let mut live = Live::v1(6, 40);
    for i in 0..200 {
        live.slice(format!("line {i}\r\n").as_bytes());
    }
    export_all(&mut live, 1 << 20);
    live.keyframe(30);
    let kf_lines = live.term.history_fence().lines();
    live.slice(b"\x1b[3J");
    assert_eq!(
        restored(live.recover(true)).join_facts(),
        Err(HeadLost::Cleared)
    );

    let mut live = Live::v1(6, 40);
    for i in 0..200 {
        live.slice(format!("line {i}\r\n").as_bytes());
    }
    export_all(&mut live, 1 << 20);
    live.keyframe(30);
    live.term.resize(6, 30);
    let (lifetime, ordinal) = (live.term.resize_lifetime(), live.term.resize_ordinal());
    live.writer
        .append_resize(
            6,
            30,
            aterm_core::grid::ResizePolicy::Native,
            lifetime,
            ordinal,
        )
        .expect("journaled");
    assert_eq!(
        restored(live.recover(true)).join_facts(),
        Err(HeadLost::Moved {
            lines: kf_lines - 30
        })
    );
    // The keyframe after the reflow, the ring still under the old identity.
    live.keyframe(30);
    let facts = restored(live.recover(true))
        .join_facts()
        .expect("nothing moved past the keyframe");
    assert_eq!(facts.export, None, "a stale ring is no export");
}
