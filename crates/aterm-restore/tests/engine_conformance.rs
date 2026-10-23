// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Crash-restore journal engine conformance and history-ring join tests.

use aterm_core::config::TerminalConfig;
use aterm_core::grid::ResizePolicy;
use aterm_core::terminal::{CursorStyle, ShellIntegrationNonce, Terminal, TerminalCheckpoint};
use aterm_restore::layout::Geometry;
use aterm_restore::{
    HistoryIdentity, HistoryWriter, JournalWriter, KeyframeInput, KeyframePolicy, Reason,
    RecoverPolicy, Recovery, Restored, VecStore, WriterIdentity, recover,
};
use aterm_spec::rng::XorShift;
use aterm_types::Appearance;

const ROWS: u16 = 64;
const COLS: u16 = 100;

const WRITER: WriterIdentity<'static> = WriterIdentity {
    pid: 11,
    birth_us: 1,
    local_id: 1,
    build: 1,
    sid: "engine-conformance",
};

/// The host's settings, applied to the live engine and to every scratch engine.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
struct Host {
    scheme: Appearance,
    cell_px: (u16, u16),
    cursor: CursorStyle,
}

impl Host {
    /// The fingerprint the window stores as `config_fp` (increment 4 hashes its real
    /// config; any injective summary serves here).
    fn fingerprint(&self) -> u64 {
        let scheme = match self.scheme {
            Appearance::Dark => 1,
            Appearance::Light => 2,
        };
        scheme
            | u64::from(self.cell_px.0) << 8
            | u64::from(self.cell_px.1) << 24
            | (self.cursor as u64) << 40
    }

    fn apply(&self, t: &mut Terminal) {
        let config = TerminalConfig {
            cursor_style: self.cursor,
            ..TerminalConfig::default()
        };
        t.apply_config(&config);
        t.set_color_scheme(self.scheme);
        t.set_cell_pixel_size(self.cell_px.0, self.cell_px.1);
    }

    fn engine(&self, rows: u16, cols: u16) -> Terminal {
        let mut t = Terminal::new(rows, cols);
        self.apply(&mut t);
        t
    }
}

/// One step of the scenario.
enum Step {
    /// PTY output, split into slices at seeded boundaries.
    Output(Vec<u8>),
    /// Bytes the host fed the engine itself (a foreground handback, a manual reset).
    Synthesized(&'static [u8]),
    /// A host resize.
    Resize(u16, u16),
    /// A host resize that changes the width (left out of the negative control's run, so
    /// the first resize its tap drops is the flap's).
    Width(u16, u16),
    /// A host setter.
    Host(Host),
    /// Time passes with no output (K1).
    Quiet,
    /// Time passes on a clean session, which waits with no timeout: nothing runs.
    Idle(u64),
    /// A verified shell-integration mark (OSC 133 with the session's nonce).
    Mark(&'static str),
    /// PTY output in exactly these slices: a read boundary inside a sequence no keyframe
    /// carries whole, so the keyframe a slice end makes due is deferred (K5).
    Pieces(Vec<Vec<u8>>),
}

const NONCE: [u8; 32] = [0x3C; 32];

fn scenario() -> Vec<Step> {
    let base = Host {
        scheme: Appearance::Dark,
        cell_px: (9, 18),
        cursor: CursorStyle::BlinkingBlock,
    };
    let mut lines = Vec::new();
    for i in 0..90 {
        lines.extend_from_slice(
            format!(
                "\x1b[1;3{}m{i:03}\x1b[0m wide \u{4e16}\u{754c} combining e\u{301}\u{302} \x1b[4:3mcurly\x1b[0m\r\n",
                i % 8
            )
            .as_bytes(),
        );
    }
    vec![
        Step::Mark("A"),
        Step::Output(b"$ build\r\n".to_vec()),
        Step::Mark("C"),
        Step::Output(lines),
        Step::Output(
            b"\x1b]8;id=x;https://example.com/a\x1b\\a link that spans slices\x1b]8;;\x1b\\\r\n"
                .to_vec(),
        ),
        Step::Mark("D;0"),
        Step::Quiet,
        Step::Output(b"\x1b[5;20r\x1b[5;1Hscrolling region\r\nline\r\nline\r\n\x1b[r".to_vec()),
        Step::Width(ROWS, COLS - 20),
        Step::Output(b"after a width change, the primary rewraps\r\n".to_vec()),
        Step::Width(ROWS, COLS),
        Step::Host(Host {
            scheme: Appearance::Light,
            ..base
        }),
        Step::Output(b"\x1b[>1u\x1b[>5u\x1b[?2026hsynchronized\x1b[?2026l\x1b[<u".to_vec()),
        Step::Output(b"\x1b[?1049h\x1b[2J\x1b[H".to_vec()),
        Step::Output({
            let mut paint = Vec::new();
            for row in 1..=ROWS {
                paint.extend_from_slice(format!("\x1b[{row};1Hrow {row:02} of the app").as_bytes());
            }
            paint
        }),
        Step::Resize(ROWS - 1, COLS),
        Step::Output(b"\x1b[1;1Hdrawn between the halves".to_vec()),
        Step::Resize(ROWS, COLS),
        Step::Output(b"\x1b[30;10Hafter the flap".to_vec()),
        Step::Quiet,
        Step::Host(Host {
            cell_px: (10, 21),
            cursor: CursorStyle::SteadyBar,
            ..base
        }),
        Step::Synthesized(b"\x1b[?1049l\x1b[?25h"),
        Step::Output(b"back on the primary, ED3 next\r\n\x1b[3J".to_vec()),
        Step::Quiet,
        Step::Output(b"after the clear\r\n\x1b]0;a title\x07".to_vec()),
        Step::Synthesized(b"\x1bc"),
        Step::Output(b"after a manual reset\r\n\x1b[38;2;10;20;30mtruecolour".to_vec()),
        Step::Quiet,
        Step::Idle(300),
        Step::Pieces(vec![
            format!(
                "\r\n\x1b]133;C;id={}\x07$ icat\r\n\x1b_Gi=31,s=1,v=1,a=q,t=d,f=24;AAAA",
                ShellIntegrationNonce(NONCE).to_hex()
            )
            .into_bytes(),
            b"\x1b\\".to_vec(),
        ]),
        Step::Pieces(vec![b"\x1bP+q544e".to_vec(), b"\x1b\\".to_vec()]),
        Step::Mark("A"),
        Step::Output(b"$ ".to_vec()),
        Step::Quiet,
    ]
}

/// The window's tap, in miniature: the resize watermark the reader's spine keeps
/// (`SpineMark`), catching the tail up from the engine's resize journal.
struct Tap {
    at: (u64, u64),
    drop_resizes: bool,
}

impl Tap {
    fn catch_up(&mut self, term: &Terminal, writer: &mut JournalWriter<VecStore>) {
        let lifetime = term.resize_lifetime();
        let since = if self.at.0 == lifetime { self.at.1 } else { 0 };
        let (reports, lost) = term.resize_journal_since(since);
        if lost > 0 {
            writer
                .append_resizes_lost(u32::try_from(lost).unwrap_or(u32::MAX))
                .expect("lost");
        }
        for report in reports {
            if !self.drop_resizes {
                writer
                    .append_resize(
                        report.to.0,
                        report.to.1,
                        ResizePolicy::Native,
                        lifetime,
                        report.ordinal,
                    )
                    .expect("resize");
            }
            self.at = (lifetime, report.ordinal);
        }
        self.at = (lifetime, term.resize_ordinal());
    }
}

struct Session {
    term: Terminal,
    writer: JournalWriter<VecStore>,
    policy: KeyframePolicy,
    tap: Tap,
    host: Host,
    now: u64,
    gathered: u64,
    keyframes: usize,
    checks: usize,
    counted_marks: usize,
    counted_regrids: usize,
    /// Checks that counted a kitty-graphics command the replay ran.
    counted_graphics: usize,
    /// Checks that counted a sequence the restore can only swallow.
    counted_swallowed: usize,
    /// Keyframes the capture deferred (K5).
    deferrals: usize,
    /// Clears settled after their keyframe.
    settles: usize,
    /// The history ring's writer, exporting as the window's journal worker does (in
    /// miniature: [`Session::export`]).
    ring: HistoryWriter,
    /// Slices since the last export, so the ring is caught up at some boundaries and behind
    /// at others.
    since_export: usize,
    /// The negative control: every chunk labelled one row later than its lines.
    ring_shifted: bool,
    /// Checks whose ring met the restored lines and supplied lines below them (a join that
    /// takes some), and checks whose join would count a gap.
    joined: usize,
    gaps: usize,
}

impl Session {
    /// The journal worker's export, in miniature: the ring started over when the fence's
    /// identity moved (at once: the window's 2 s settle is its own pacing, which the
    /// window's tests hold), or when the engine evicted lines it had not reached; then
    /// every history line it has not reached, in chunks.
    fn export(&mut self) {
        self.since_export = 0;
        let fence = self.term.history_fence();
        if fence.detached {
            return;
        }
        let identity = HistoryIdentity::of(&fence);
        let store = self.writer.store();
        let shift = u64::from(self.ring_shifted);
        let next = |ring: &HistoryWriter| ring.end_row().saturating_sub(shift);
        if identity != self.ring.identity() || next(&self.ring) < fence.oldest {
            let clear = self.ring.identity().cols != 0
                && identity.clear_gen != self.ring.identity().clear_gen;
            self.ring
                .reset_ring(store, identity, fence.oldest + shift, 1 << 20, clear)
                .expect("reset");
        }
        while next(&self.ring) < fence.end {
            let from = next(&self.ring);
            let lines = self
                .term
                .history_lines_since_fence(&fence, from, aterm_restore::records::CHUNK_LINES)
                .expect("the fence holds");
            self.ring
                .append_chunk(store, from + shift, &lines)
                .expect("a chunk fits");
        }
    }

    /// The history half of a check (the plan's §7.3, increment 5): an exact restore's head
    /// is the live history, the lines its checkpoint carries are the live history's newest,
    /// at the same rows, and where the ring meets them its lines below them are the live
    /// history's too, so imported lines plus carried lines are the live history over the
    /// same rows.
    fn check_history(&mut self, label: &str, restored: &Restored) -> Result<(), String> {
        let Ok(facts) = restored.join_facts() else {
            return Ok(());
        };
        let live = self.term.history_fence();
        if facts.head.end != live.end
            || HistoryIdentity::of(&facts.head) != HistoryIdentity::of(&live)
        {
            return Err(format!(
                "{label}: an exact restore's head {:?} is not the live history {live:?}",
                facts.head
            ));
        }
        let lines_at = |t: &Terminal, from: u64, n: u64| {
            let fence = t.history_fence();
            t.history_lines_since_fence(&fence, from, usize::try_from(n).unwrap_or(usize::MAX))
                .map(|lines| aterm_core::scrollback::serialize_lines(&lines))
        };
        let carried_from = facts.head.end - facts.carried;
        let scratch = Terminal::from_checkpoint(&restored.checkpoint);
        let carried = lines_at(&scratch, carried_from, facts.carried);
        if carried.is_err() || carried != lines_at(&self.term, carried_from, facts.carried) {
            return Err(format!(
                "{label}: the carried lines are not the live history's newest"
            ));
        }
        match facts.export {
            Some(export)
                if export.fence.holds_at(&facts.head)
                    && export.first <= carried_from
                    && carried_from <= export.fence.end
                    && export.first >= live.oldest =>
            {
                let take = carried_from - export.first;
                if take == 0 {
                    // Checkpoint already carries every line named by the head.
                    return Ok(());
                }
                let mut ring = Vec::new();
                let (chunks, skip) = restored.ring_chunks_for(&export, take);
                for chunk in &restored.ring.chunks[chunks] {
                    ring.extend(
                        chunk
                            .lines()
                            .map_err(|e| format!("{label}: a chunk: {e:?}"))?,
                    );
                }
                let skip = usize::try_from(skip).unwrap_or(usize::MAX);
                let ring: Vec<_> = ring
                    .into_iter()
                    .skip(skip)
                    .take(usize::try_from(take).unwrap_or(usize::MAX))
                    .collect();
                let want = lines_at(&self.term, export.first, take);
                if want.as_ref() != Ok(&aterm_core::scrollback::serialize_lines(&ring)) {
                    return Err(format!(
                        "{label}: the ring's {take} lines below the carried ones are not the \
                         live history's"
                    ));
                }
                self.joined += 1;
            }
            _ => self.gaps += 1,
        }
        Ok(())
    }

    fn keyframe(&mut self, reason: Reason) {
        let fence = self.term.history_fence();
        let lag = if HistoryIdentity::of(&fence) == self.ring.identity() {
            fence
                .end
                .saturating_sub(self.ring.end_row().max(fence.oldest))
        } else {
            fence.lines()
        };
        let bridge = reason
            .bridge_cap()
            .min(usize::try_from(lag).unwrap_or(usize::MAX));
        self.tap.catch_up(&self.term, &mut self.writer);
        let Some(checkpoint) = self
            .term
            .checkpoint_journal_marked(bridge, reason.bridge_cap())
        else {
            self.policy.keyframe_deferred(reason);
            self.deferrals += 1;
            return;
        };
        let input = KeyframeInput {
            checkpoint: &checkpoint,
            fence: self.term.history_fence(),
            resize_lifetime: self.term.resize_lifetime(),
            resize_ordinal: self.term.resize_ordinal(),
            bridge_lines: u32::try_from(bridge).unwrap_or(u32::MAX),
            config_fp: self.host.fingerprint(),
            integration_on: self.term.shell_integration_posture()
                == aterm_core::terminal::ShellIntegrationPosture::On,
        };
        self.writer.write_keyframe(&input).expect("a keyframe fits");
        if reason == Reason::Clear {
            self.writer.settle_clear().expect("settled");
            self.settles += 1;
        }
        self.policy.keyframe_written(self.now);
        self.keyframes += 1;
    }

    fn decide(&mut self, decision: aterm_restore::Decision) {
        if let Some(reason) = decision.keyframe {
            self.keyframe(reason);
        }
    }
    fn slice(&mut self, bytes: &[u8], synthesized: bool, label: &str) -> Result<(), String> {
        self.now += 7;
        self.tap.catch_up(&self.term, &mut self.writer);
        let clear_gen = self.term.history_fence().clear_gen;
        let marks = self.term.shell_marks_verified();
        if !synthesized {
            self.gathered += bytes.len() as u64;
            self.writer.set_gathered(self.gathered).expect("gathered");
        }
        self.term.process(bytes);
        let verified = self.term.shell_marks_verified() - marks;
        self.writer.note_marks(verified).expect("marks");
        if synthesized {
            self.writer.append_synthesized(bytes).expect("appended");
        } else {
            self.writer.append_slice(bytes).expect("appended");
        }
        self.since_export += 1;
        if self.since_export >= 3 {
            self.export();
        }
        if self.term.history_fence().clear_gen != clear_gen {
            // and the next slice end forces the clear's keyframe.
            self.policy.note_clear();
        }
        self.check(&format!("{label}, before its keyframe"))?;
        let decision =
            self.policy
                .after_slice(self.now, self.writer.tail_since_keyframe(), verified);
        if decision.keyframe.is_some() {
            self.decide(decision);
            self.check(&format!("{label}, after its keyframe"))?;
        }
        Ok(())
    }

    /// Restore the region and hold it to the live engine.
    fn check(&mut self, label: &str) -> Result<(), String> {
        self.checks += 1;
        let host = self.host;
        let recovery = recover(
            self.writer.store(),
            RecoverPolicy {
                tail: true,
                config_fp: host.fingerprint(),
                scratch_nonce: [0xA5; 32],
            },
            |rows, cols| host.engine(rows, cols),
        );
        let Recovery::Restored(restored) = recovery else {
            return Err(format!("{label}: no screen: {recovery:?}"));
        };
        if !restored.exact {
            let mut counted = restored.losses.clone();
            counted.marks_lost = 0;
            counted.regrids_replayed = 0;
            counted.graphics_replayed = 0;
            counted.unscrolls_replayed = 0;
            counted.swallowed = None;
            if counted == aterm_restore::recover::Losses::default() {
                self.counted_marks += usize::from(restored.losses.marks_lost > 0);
                self.counted_regrids += usize::from(restored.losses.regrids_replayed > 0);
                self.counted_graphics += usize::from(restored.losses.graphics_replayed > 0);
                self.counted_swallowed += usize::from(restored.losses.swallowed.is_some());
                return Ok(());
            }
            return Err(format!("{label}: not exact: {:?}", restored.losses));
        }
        let mut scratch = host.engine(restored.checkpoint.rows, restored.checkpoint.cols);
        scratch.restore_checkpoint(&restored.checkpoint);
        let (Some(got), Some(want)) = (visible(&scratch), visible(&self.term)) else {
            return Err(format!(
                "{label}: an exact restore while an engine holds a sequence no keyframe \
                 carries whole (restored: {:?}, live: {:?})",
                scratch.partial_sequence_state(),
                self.term.partial_sequence_state()
            ));
        };
        if got != want {
            return Err(format!(
                "{label}: the restored screen is not the live one (differing: {:?})\n  restored: {:?}\n  live:     {:?}",
                differing(&got, &want),
                scratch.visible_content(),
                self.term.visible_content()
            ));
        }
        self.check_history(label, &restored)
    }
}

/// The fields two visible checkpoints disagree on, for a failure that names them.
fn differing(a: &TerminalCheckpoint, b: &TerminalCheckpoint) -> Vec<String> {
    let mut out = Vec::new();
    macro_rules! field {
        ($($name:ident),*) => {
            $(if a.$name != b.$name {
                out.push(format!("{}: {:?} vs {:?}", stringify!($name), a.$name, b.$name));
            })*
        };
    }
    field!(
        rows,
        cols,
        history_lines,
        cursor,
        alt_cursor,
        saved_cursor_main,
        saved_cursor_alt,
        modes,
        style,
        charset,
        kitty_keyboard,
        xterm_keyboard,
        taskbar_progress,
        secure_keyboard_entry,
        current_working_directory,
        parser_ground,
        parser,
        absolute_row_counter,
        alt_absolute_row_counter,
        color,
        shell,
        title,
        output_state,
        holder
    );
    if a.grid != b.grid {
        out.push("grid".to_string());
    }
    if a.alt_grid != b.alt_grid {
        out.push("alt_grid".to_string());
    }
    out
}

/// A terminal's visible state as a restore can reproduce it: its visible-only journal
/// projection, less its lineage's seed uncertainty (`HolderRepr::seed_untrusted`: a
/// restore that took output after its install carries it by design, for the host's
/// adoption to read, and a live engine never does), with the kitty keyboard stack's
/// entries at and above each stack pointer cleared.
fn visible(t: &Terminal) -> Option<TerminalCheckpoint> {
    let mut c = t.checkpoint_journal(0)?.without_seed_lineage();
    let k = &mut c.kitty_keyboard;
    for (stack, sp) in [(&mut k.main_stack, k.main_sp), (&mut k.alt_stack, k.alt_sp)] {
        for entry in stack.iter_mut().skip(usize::from(sp)) {
            *entry = Default::default();
        }
    }
    Some(c)
}

/// Run the scenario with its slices cut at `seed`'s boundaries.
fn run(seed: u64, drop_resizes: bool, width_changes: bool) -> Result<Session, String> {
    run_with(seed, drop_resizes, width_changes, false)
}

/// [`run`], with the ring's export labelling every chunk one row later than its lines are
/// when `ring_shifted` (the history half's negative control).
fn run_with(
    seed: u64,
    drop_resizes: bool,
    width_changes: bool,
    ring_shifted: bool,
) -> Result<Session, String> {
    let geometry = Geometry::v1();
    let writer =
        JournalWriter::create(VecStore::new(geometry.region_len), geometry, &WRITER).expect("c");
    let host = Host {
        scheme: Appearance::Dark,
        cell_px: (9, 18),
        cursor: CursorStyle::BlinkingBlock,
    };
    let mut term = host.engine(ROWS, COLS);
    term.set_require_shell_integration_nonce(true);
    term.authorize_shell_integration(NONCE);
    writer.set_config_fp(host.fingerprint()).expect("config");
    let mut session = Session {
        policy: KeyframePolicy::new(geometry.tail_cap, 0, host.fingerprint()),
        tap: Tap {
            at: (term.resize_lifetime(), term.resize_ordinal()),
            drop_resizes,
        },
        term,
        writer,
        host,
        now: 0,
        gathered: 0,
        keyframes: 0,
        checks: 0,
        counted_marks: 0,
        counted_regrids: 0,
        counted_graphics: 0,
        counted_swallowed: 0,
        deferrals: 0,
        settles: 0,
        ring: HistoryWriter::new(&geometry),
        ring_shifted,
        since_export: 0,
        joined: 0,
        gaps: 0,
    };
    session.keyframe(Reason::Quiescence);
    session.check("the first keyframe")?;
    let mut rng = XorShift::new(seed);
    for (i, step) in scenario().into_iter().enumerate() {
        match step {
            Step::Output(bytes) => {
                let mut rest = bytes.as_slice();
                while !rest.is_empty() {
                    let n = (1 + rng.index(40)).min(rest.len());
                    let (slice, tail) = rest.split_at(n);
                    let label = format!("step {i}, a slice");
                    session.slice(slice, false, &label)?;
                    rest = tail;
                }
            }
            Step::Synthesized(bytes) => {
                let label = format!("step {i}, synthesized bytes");
                session.slice(bytes, true, &label)?;
            }
            Step::Mark(op) => {
                let hex = ShellIntegrationNonce(NONCE).to_hex();
                let label = format!("step {i}, a mark");
                session.slice(
                    format!("\x1b]133;{op};id={hex}\x07").as_bytes(),
                    false,
                    &label,
                )?;
            }
            Step::Width(..) if !width_changes => {}
            Step::Resize(rows, cols) | Step::Width(rows, cols) => {
                session.now += 7;
                let (old_rows, old_cols) = (session.term.rows(), session.term.cols());
                session.term.resize(rows, cols);
                session
                    .writer
                    .set_resize_word(
                        session.term.resize_lifetime(),
                        session.term.resize_ordinal(),
                    )
                    .expect("word");
                session.tap.catch_up(&session.term, &mut session.writer);
                session
                    .policy
                    .note_resize(session.now, cols != old_cols || rows > old_rows);
                session.check(&format!("step {i}, a resize to {rows}x{cols}"))?;
            }
            Step::Host(next) => {
                session.host = next;
                next.apply(&mut session.term);
                session
                    .writer
                    .set_config_fp(next.fingerprint())
                    .expect("config");
                session.policy.note_config(next.fingerprint());
                let decision = session.policy.on_quiet(session.now);
                assert!(decision.keyframe.is_some(), "a setter forces a keyframe");
                session.decide(decision);
                session.check(&format!("step {i}, a host setter"))?;
            }
            Step::Quiet => {
                session.now += 250;
                let decision = session.policy.on_quiet(session.now);
                session.decide(decision);
                session.check(&format!("step {i}, quiet"))?;
            }
            Step::Idle(ms) => {
                assert!(!session.policy.dirty(), "an idle session is clean");
                session.now += ms;
            }
            Step::Pieces(pieces) => {
                for (j, piece) in pieces.iter().enumerate() {
                    let label = format!("step {i}, piece {j}");
                    session.slice(piece, false, &label)?;
                }
            }
        }
    }
    Ok(session)
}

/// The scenario, cut at many seeded boundaries, restores exactly at every record and
/// every keyframe.
#[test]
fn every_boundary_restores_the_live_screen() {
    let runs: Vec<(u64, Result<Session, String>)> = std::thread::scope(|scope| {
        let seeds: Vec<_> = (1..=6)
            .map(|seed| (seed, scope.spawn(move || run(seed, false, true))))
            .collect();
        seeds
            .into_iter()
            .map(|(seed, handle)| {
                let run = handle.join().unwrap_or_else(|panic| {
                    std::panic::resume_unwind(panic);
                });
                (seed, run)
            })
            .collect()
    });
    for (seed, run) in runs {
        let session = run.unwrap_or_else(|why| panic!("seed {seed}: {why}"));
        assert!(
            session.keyframes > 5,
            "seed {seed}: {} keyframes",
            session.keyframes
        );
        assert!(
            session.checks > 100,
            "seed {seed}: {} checks",
            session.checks
        );
        assert!(
            session.counted_marks > 0,
            "seed {seed}: a mark inside the budget was counted"
        );
        assert!(
            session.counted_regrids > 0,
            "seed {seed}: a width change or rows grow in the tail was counted"
        );
        assert!(
            session.counted_graphics > 0,
            "seed {seed}: the kitty-graphics query in the tail was counted"
        );
        assert!(
            session.settles >= 2,
            "seed {seed}: the ED3 and the RIS were each settled after their keyframe"
        );
        assert!(
            session.joined > 5 && session.gaps > 0,
            "seed {seed}: the ring joined at {} checks, left a gap at {}",
            session.joined,
            session.gaps
        );
        assert!(
            session.deferrals > 0 && session.counted_swallowed > 0,
            "seed {seed}: a keyframe was deferred behind an open query ({}), and a restore \
             inside one counted it swallowed ({})",
            session.deferrals,
            session.counted_swallowed
        );
    }
}

/// NEGATIVE CONTROL: a tap that drops RESIZE records fails on the flap.
#[test]
fn a_tap_that_drops_resizes_fails_on_the_flap() {
    let Err(why) = run(1, true, false) else {
        panic!("dropping every RESIZE went unnoticed");
    };
    assert!(
        why.starts_with("step 15, a resize to 63x100"),
        "caught at the flap's first half: {why}"
    );
    run(1, false, false).unwrap_or_else(|why| panic!("control: {why}"));
}

/// NEGATIVE CONTROL of the history half: a ring whose chunks name rows one later than
/// their lines (an export that lost track of where it was) fails the check: the ring's
/// lines below the carried ones are not the live history's.
#[test]
fn a_ring_one_row_off_fails_the_history_check() {
    let why = match run_with(1, false, true, true) {
        Err(why) => why,
        Ok(s) => panic!(
            "a ring one row off went unnoticed (joined {}, gaps {})",
            s.joined, s.gaps
        ),
    };
    assert!(
        why.contains("not the live history's"),
        "caught by the history half: {why}"
    );
}
