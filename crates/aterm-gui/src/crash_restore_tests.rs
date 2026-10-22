// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The window's crash-restore journal over a real shared-memory region, driven as the
//! reader drives it (`spawn.rs`'s ingest: the reading before a hold, `process`, the
//! record, the decision at the hold's end, the keyframe written after it), and restored
//! as a recovering window restores it (`restore_offered` over a read-only duplicate of
//! the custody descriptor).

use std::os::fd::OwnedFd;
use std::sync::{Arc, Mutex};

use aterm_core::terminal::{Terminal, TerminalCheckpoint};
use aterm_keeper::client::OfferedCompanion;
use aterm_keeper::wire::COMPANION_JOURNAL;

use super::*;

/// One live session and its journal, as the reader holds them, the journal on a held clock
/// (`SessionJournal::create_held`): only `pass_ms` moves it, so what the keyframe policy
/// decides and what the ring's ticks stamp never depend on how long a loaded machine took
/// between two of a test's steps.
struct Rig {
    term: Arc<Mutex<Terminal>>,
    journal: Arc<SessionJournal>,
    custody: OwnedFd,
    len: u64,
    first_receipt: Option<Arc<aterm_keeper::client::CompanionReceipt>>,
    /// The config the session's engine was built with, and the restorer runs.
    cfg: Option<aterm_core::config::TerminalConfig>,
}

impl Rig {
    fn new(rows: u16, cols: u16) -> Self {
        Self::with_config(rows, cols, None)
    }

    /// A session built, journaled and restored under `cfg` (a theme of its own).
    fn with_config(rows: u16, cols: u16, cfg: Option<aterm_core::config::TerminalConfig>) -> Self {
        Self::with_recovery_count(rows, cols, cfg, None)
    }

    fn with_recovery_count(
        rows: u16,
        cols: u16,
        cfg: Option<aterm_core::config::TerminalConfig>,
        accepted: Option<Arc<AtomicBool>>,
    ) -> Self {
        let term = Arc::new(Mutex::new(crate::spawn::new_live_terminal(
            rows,
            cols,
            cfg.as_ref(),
            None,
        )));
        let journal = SessionJournal::create_held(
            7,
            "s-1234",
            &term,
            -1,
            cfg.as_ref(),
            Arc::new(crate::manual_reset::ResetLane::default()),
        )
        .expect("a journal");
        journal.bind_recovery_count(accepted);
        let JournalCustody {
            region: custody,
            len,
            receipt: first_receipt,
        } = journal.take_custody_with_receipt().expect("custody, once");
        assert!(journal.take_custody().is_none(), "the custody goes once");
        Self {
            term,
            journal,
            custody,
            len,
            first_receipt,
            cfg,
        }
    }

    /// One read through the reader's stages: the gather's count, then one hold that
    /// processes it, its reply drained as the reader drains it (to the PTY, never the
    /// journal's business), its record and decision, and the keyframe written after.
    fn slice(&self, bytes: &[u8]) {
        self.journal.gathered(bytes.len());
        let captured = {
            let mut t = crate::term_lock(&self.term);
            let mut pre = self.journal.pre(&t);
            t.process(bytes);
            let _ = t.take_response();
            self.journal.record(&t, &mut pre, bytes, false);
            self.journal.decide(&t, &pre)
        };
        if let Some(captured) = captured {
            self.journal.write(captured);
        }
    }

    /// The region as a recovering window is offered it: a read-only duplicate of the
    /// custody descriptor.
    fn offered(&self) -> OfferedCompanion {
        OfferedCompanion {
            region: self.custody.try_clone().expect("dup"),
            kind: COMPANION_JOURNAL,
            len: self.len,
        }
    }

    fn restore(&self, attempts: u8) -> Result<Restore, RestoreReport> {
        let mut used = 0u64;
        offered_restore(Some(self.offered()), attempts, self.cfg.as_ref(), &mut used)
    }

    /// The restore of a window running `cfg` (another config than the session's).
    fn restore_under(&self, cfg: &aterm_core::config::TerminalConfig) -> Restore {
        let mut used = 0u64;
        offered_restore(Some(self.offered()), 0, Some(cfg), &mut used).expect("a screen")
    }

    /// What `recover` makes of the region as it stands, under the session's config.
    fn recovered(&self) -> aterm_restore::Restored {
        let cell_px = crate::term_lock(&self.term).host_cell_pixel_size();
        let cfg = self.cfg.as_ref();
        match aterm_restore::recover(
            &self.journal.snapshot(),
            RecoverPolicy {
                tail: true,
                config_fp: config_fp(config_hash(cfg), cell_px),
                scratch_nonce: [0xA5; 32],
            },
            |rows, cols| scratch_engine(rows, cols, cfg, cell_px),
        ) {
            Recovery::Restored(restored) => *restored,
            Recovery::Meta(why) => panic!("expected a screen, got the META rung: {why:?}"),
        }
    }

    fn visible(&self) -> TerminalCheckpoint {
        visible(&crate::term_lock(&self.term))
    }
}

/// [`restore_offered`] for these tests: every region restored here is written only by this
/// test's thread, and only between restores, so its writer is inert for each call (the
/// function's contract).
fn offered_restore(
    companion: Option<OfferedCompanion>,
    attempts: u8,
    cfg: Option<&aterm_core::config::TerminalConfig>,
    used: &mut u64,
) -> Result<Restore, RestoreReport> {
    offered_restoration(companion, attempts, cfg, used).screen
}

/// [`offered_restore`] with the control slot's carry beside the screen.
fn offered_restoration(
    companion: Option<OfferedCompanion>,
    attempts: u8,
    cfg: Option<&aterm_core::config::TerminalConfig>,
    used: &mut u64,
) -> Restoration {
    // SAFETY: the region's one writer is this thread's journal, which writes nothing during
    // the call; the mapping is dropped before it returns.
    unsafe { restore_offered(companion, attempts, cfg, used) }
}

/// What a person sees of an engine: its visible-only journal projection.
fn visible(t: &Terminal) -> TerminalCheckpoint {
    // A restore whose replay ran a tail carries its lineage's seed uncertainty
    // (`HolderRepr::seed_untrusted`), which a live engine never has: not what it shows.
    t.checkpoint_journal(0)
        .expect("ground")
        .without_seed_lineage()
}

/// The engine a restored checkpoint lands in, built as the spawn builds an adopted one.
fn adopted(cp: &TerminalCheckpoint) -> Terminal {
    let mut t = crate::spawn::new_live_terminal(cp.rows, cp.cols, None, None);
    t.restore_checkpoint(cp);
    t
}

/// A session journaled as the reader journals it restores exactly: its screen, modes
/// and pen as the live engine has them, with no repaint, so a main-screen session gets
/// no size pulse (`adoption_needs_size_pulse` with `repaint = !exact`); an exact
/// alternate-screen restore gets the pulse every adoption onto it gets, and a restore
/// with a loss counted is repainted and pulsed.
#[test]
fn a_session_restores_exactly_and_only_an_inexact_or_alternate_screen_is_pulsed() {
    let rig = Rig::new(8, 40);
    for i in 0..30 {
        rig.slice(
            format!(
                "\x1b[1;3{}mline {i}\x1b[0m wide \u{4e16} e\u{301}\r\n",
                i % 8
            )
            .as_bytes(),
        );
    }
    rig.slice(b"$ \x1b[7mreverse");
    let exact = rig.restore(0).expect("a screen");
    assert_eq!(exact.report.rung, Rung::Exact, "{:?}", exact.report);
    assert!(!exact.repaint);
    assert_eq!(visible(&adopted(&exact.checkpoint)), rig.visible());
    assert_eq!(
        exact.checkpoint.shell_integration_nonce, None,
        "the shell re-keys"
    );
    assert!(
        !crate::spawn::adoption_needs_size_pulse(Some(&exact.checkpoint), exact.repaint),
        "an exact main-screen restore is not pulsed"
    );

    rig.slice(b"\x1b[?1049h\x1b[2J\x1b[Hthe app's frame");
    let alt = rig.restore(0).expect("a screen");
    assert_eq!(alt.report.rung, Rung::Exact, "{:?}", alt.report);
    assert!(alt.checkpoint.modes.alternate_screen);
    assert_eq!(visible(&adopted(&alt.checkpoint)), rig.visible());
    assert!(
        crate::spawn::adoption_needs_size_pulse(Some(&alt.checkpoint), alt.repaint),
        "an alternate-screen adoption is pulsed, exact or not"
    );

    // Read and never processed: the window died between the gather and the parse.
    rig.journal.gathered(5);
    let lost = rig.restore(0).expect("a screen");
    assert_eq!(lost.report.rung, Rung::Screen);
    assert_eq!(lost.report.why, "in-flight");
    assert_eq!(lost.report.in_flight_lost, 5);
    assert!(lost.repaint);
    assert!(crate::spawn::adoption_needs_size_pulse(
        Some(&lost.checkpoint),
        lost.repaint
    ));
}

/// A DA and a DSR query, an OSC 7 and an OSC 52 and a bell in the tail: the restore
/// replays them into its scratch engine, which nothing else holds, and hands on only a
/// checkpoint, which carries no response buffer, OSC event queue or bell. Landed the way
/// the window lands a recovered session (`spawn::hydrate_adopted_engine`, unchanged by the
/// journal), the adopted engine holds no reply to send the PTY, no OSC event to act on and
/// no bell to ring, and its screen is the live one with the replay's effects on it.
///
/// NEGATIVE CONTROL: the plan's rejected landing, the tail replayed into the adopted
/// engine itself, leaves all three to act on. This is what the test can catch: a landing
/// that processes the tail where the adopted engine's host effects are live. The scratch
/// engine's own replies need no drain (the plan drained them); they die with it.
#[test]
fn a_reply_an_osc_event_and_a_bell_from_the_replay_never_reach_the_adopted_engine() {
    let rig = Rig::new(6, 40);
    let tail: &[u8] =
        b"\x1b[c\x1b[6n\x1b]7;file://host/tmp\x07\x1b]52;c;aGVsbG8=\x07\x07after the queries\r\n";
    rig.slice(tail);
    let restore = rig.restore(0).expect("a screen");
    assert_eq!(restore.report.rung, Rung::Exact, "{:?}", restore.report);
    let land = |checkpoint: &TerminalCheckpoint| {
        let term = Mutex::new(crate::spawn::new_live_terminal(6, 40, None, None));
        let mut history = crate::handoff_history::AdoptedHistory::default();
        crate::spawn::hydrate_adopted_engine(
            &term,
            Some(checkpoint),
            None,
            None,
            None,
            7,
            &mut history,
        );
        term.into_inner().expect("the adopted engine")
    };
    let mut landed = land(&restore.checkpoint);
    assert_eq!(landed.take_response(), None, "no reply reaches the PTY");
    assert_eq!(landed.take_osc_event(), None, "no OSC event is acted on");
    assert!(!landed.drain_bell(), "no bell rings");
    assert_eq!(visible(&landed), rig.visible());
    assert_eq!(
        landed.current_working_directory(),
        Some("/tmp"),
        "the replay's state crossed: only its host effects stayed behind"
    );

    let mut rejected = land(&restore.checkpoint);
    rejected.process(tail);
    assert!(rejected.take_response().is_some(), "control: a reply");
    assert!(rejected.take_osc_event().is_some(), "control: an OSC event");
    assert!(rejected.drain_bell(), "control: a bell");
}

/// K1: a burst keeps the session dirty (the parse loop then waits with a timeout), the
/// quiet keyframes it once, and the session is clean again (the loop blocks): an idle
/// session's restore replays nothing.
#[test]
fn a_quiet_burst_is_keyframed_once_and_its_restore_replays_nothing() {
    let rig = Rig::new(6, 40);
    assert!(!rig.journal.dirty(), "a fresh session is clean");
    let (first, _) = rig.journal.counts();
    assert_eq!(first, 1, "the spawn's first keyframe");
    rig.slice(b"one burst\r\n");
    assert!(rig.journal.dirty());
    assert!(rig.journal.tail_bytes() > 0, "the burst is in the tail");
    rig.journal.on_quiet(&rig.term);
    assert_eq!(rig.journal.counts().0, 1, "inside the burst: no keyframe");
    rig.journal.pass_ms(aterm_restore::policy::QUIET_MS);
    rig.journal.on_quiet(&rig.term);
    assert_eq!(rig.journal.counts().0, 2, "the quiet keyframe");
    assert!(!rig.journal.dirty(), "clean: the loop blocks again");
    assert_eq!(rig.journal.tail_bytes(), 0, "`status journal_tail=0`");
    let dead = rig.journal.snapshot();
    let Recovery::Restored(restored) = aterm_restore::recover(
        &dead,
        RecoverPolicy {
            tail: true,
            config_fp: config_fp(config_hash(None), None),
            scratch_nonce: [0xA5; 32],
        },
        |rows, cols| scratch_engine(rows, cols, None, None),
    ) else {
        panic!("a screen");
    };
    assert!(restored.exact, "{:?}", restored.losses);
    assert_eq!(restored.replayed_records, 0, "nothing past the keyframe");
}

/// A host resize with no output: its word is stored in the resize's hold, so a crash
/// before the parse thread records it counts it (`resizes_unjournaled`); the nudge
/// records it with no output, and the restore is exact at the new geometry.
#[test]
fn a_resize_with_no_output_is_counted_until_the_nudge_records_it() {
    let rig = Rig::new(8, 40);
    rig.slice(b"before the resize\r\n");
    {
        let mut t = crate::term_lock(&rig.term);
        t.resize(7, 40);
        rig.journal.host_resized(&t);
    }
    let unrecorded = rig.restore(0).expect("a screen");
    assert_eq!(unrecorded.report.rung, Rung::Screen);
    assert_eq!(unrecorded.report.why, "resizes");
    assert_eq!(unrecorded.report.resizes_unjournaled, 1);
    rig.journal.on_nudge(&rig.term);
    let recorded = rig.restore(0).expect("a screen");
    assert_eq!(recorded.report.rung, Rung::Exact, "{:?}", recorded.report);
    assert_eq!(
        (recorded.checkpoint.rows, recorded.checkpoint.cols),
        (7, 40)
    );
    assert_eq!(visible(&adopted(&recorded.checkpoint)), rig.visible());
}

/// A host settings change (a font zoom's cell size): its fingerprint is stored in the
/// setter's hold, so the tail past the keyframe is not called exact (`config-moved`)
/// until the keyframe the nudge forces (K4); a restorer running another config never
/// calls the screen exact. The restorer replays under the dead window's cell size.
#[test]
fn a_settings_change_is_counted_until_its_forced_keyframe() {
    let rig = Rig::new(6, 40);
    rig.slice(b"before the zoom\r\n");
    let zoom = |px: (u16, u16)| {
        let mut t = crate::term_lock(&rig.term);
        t.set_cell_pixel_size(px.0, px.1);
        rig.journal.host_settings(&t, None);
    };
    zoom((18, 36));
    // A crash before the parse thread's next turn: the setter's word, not the keyframe.
    let moved = rig.restore(0).expect("a screen");
    assert_eq!(moved.report.rung, Rung::Screen);
    assert_eq!(moved.report.why, "config-moved");
    let (keyframes, _) = rig.journal.counts();
    rig.journal.on_nudge(&rig.term);
    assert_eq!(rig.journal.counts().0, keyframes + 1, "K4 at the nudge");
    // With no nudge (no reader to take it), the next slice's end forces it.
    zoom((20, 40));
    rig.slice(b"after the zoom\r\n");
    assert_eq!(rig.journal.counts().0, keyframes + 2, "K4 at the slice end");
    let settled = rig.restore(0).expect("a screen");
    assert_eq!(settled.report.rung, Rung::Exact, "{:?}", settled.report);
    assert_eq!(
        adopted(&settled.checkpoint).host_cell_pixel_size(),
        None,
        "the checkpoint carries no cell size: the window sets its own"
    );

    // A tail past the keyframe, replayed by a restorer running another config.
    rig.slice(b"past the keyframe\r\n");
    let other = aterm_core::config::TerminalConfig {
        bold_is_bright: false,
        ..aterm_core::config::TerminalConfig::default()
    };
    let foreign = rig.restore_under(&other);
    assert_eq!(foreign.report.rung, Rung::Screen);
    assert_eq!(foreign.report.why, "config");
}

/// The config half of the fingerprint, between two real configs (a tab with a theme of
/// its own, `session_theme`'s `host_settings(.., Some(&own))`): another config, another
/// fingerprint. A tail recorded under the session's own config and replayed by a
/// restorer running the instance's is not called exact (`restore_why=config`), and is
/// under the session's own; once K1 has caught the idle session up there is nothing to
/// replay, and the restore is exact under either. A host setter that applies another
/// config (`host_settings(.., Some(&cfg))`) moves the header's fingerprint in its hold
/// and forces a keyframe under it (K4).
#[test]
fn a_tail_recorded_under_one_config_is_exact_only_under_that_config() {
    let instance = aterm_core::config::TerminalConfig::default();
    let own = aterm_core::config::TerminalConfig {
        bold_is_bright: !instance.bold_is_bright,
        ..instance.clone()
    };
    assert_ne!(
        config_hash(Some(&instance)),
        config_hash(Some(&own)),
        "two configs, two hashes"
    );
    let rig = Rig::with_config(6, 40, Some(own.clone()));
    rig.slice(b"\x1b[1;31mbold red\x1b[0m under the tab's own theme\r\n");
    let under_own = rig.restore(0).expect("a screen");
    assert_eq!(under_own.report.rung, Rung::Exact, "{:?}", under_own.report);
    let under_instance = rig.restore_under(&instance);
    assert_eq!(under_instance.report.rung, Rung::Screen);
    assert_eq!(under_instance.report.why, "config");

    rig.journal.pass_ms(aterm_restore::policy::QUIET_MS);
    rig.journal.on_quiet(&rig.term);
    assert_eq!(rig.journal.tail_bytes(), 0, "K1 caught the idle tab up");
    let idle = rig.restore_under(&instance);
    assert_eq!(
        idle.report.rung,
        Rung::Exact,
        "nothing replayed under the instance's config: {:?}",
        idle.report
    );

    // The tab's theme is dropped: the setter applies the instance's config in its hold.
    let (keyframes, _) = rig.journal.counts();
    {
        let mut t = crate::term_lock(&rig.term);
        t.apply_config(&instance);
        rig.journal.host_settings(&t, Some(&instance));
    }
    let header_fp = aterm_restore::read_header(&rig.journal.snapshot())
        .expect("a header")
        .config_fp;
    assert_eq!(
        header_fp,
        config_fp(config_hash(Some(&instance)), None),
        "the setter's hold stored the new fingerprint"
    );
    rig.journal.on_nudge(&rig.term);
    assert_eq!(
        rig.journal.counts().0,
        keyframes + 1,
        "K4 under the new config"
    );
    rig.slice(b"after the theme was dropped\r\n");
    assert_eq!(rig.restore_under(&instance).report.rung, Rung::Exact);
    let stale = rig.restore_under(&own);
    assert_eq!(stale.report.why, "config", "{:?}", stale.report);
}

/// A line of history that only the scrollback held: a token no other bytes repeat, so the
/// raw tail holds it verbatim.
fn secret(i: usize) -> String {
    format!(
        "secret-{:016x}",
        aterm_hash::fnv1a_64(format!("line {i}").as_bytes())
    )
}

/// Every [`secret`]-shaped run of bytes anywhere in the region (`secret-` and the 16
/// bytes after it, whatever they are): a token is in the region's bytes exactly when it
/// is in this set. One copy of the region and one pass over it — searching the 8 MiB
/// copy afresh for each of 32 tokens was 2.3 CPU-s at opt-level 0.
fn region_secrets(journal: &SessionJournal) -> std::collections::BTreeSet<Vec<u8>> {
    use aterm_restore::RegionView as _;
    let region = journal.snapshot();
    let mut bytes = vec![0u8; region.len()];
    region.read(0, &mut bytes).expect("the copy reads");
    let len = secret(0).len();
    let mut found = std::collections::BTreeSet::new();
    for start in 0..bytes.len().saturating_sub(len - 1) {
        if bytes[start] == b's' && bytes[start..start + len].starts_with(b"secret-") {
            found.insert(bytes[start..start + len].to_vec());
        }
    }
    found
}

/// An ED3 forces a keyframe at the hold's end, after which the clear is settled out of
/// the region (the privacy claim: a line a person cleared survives in no byte the keeper
/// holds), and the restore after it is exact. The region is read for it: before the
/// clear the scrolled-off secrets sit in the raw tail and in the older keyframe's bridge;
/// after it, the older slot is empty and no secret is in any byte of the region.
#[test]
fn a_clear_forces_a_keyframe_and_restores_exactly_after_it() {
    let rig = Rig::new(4, 30);
    for i in 0..20 {
        rig.slice(format!("{}\r\n", secret(i)).as_bytes());
    }
    // A quiet keyframe first: the slot the clear's keyframe will not overwrite then holds
    // a bridge of the secrets, beside the raw tail that follows it.
    rig.journal.pass_ms(aterm_restore::policy::QUIET_MS);
    rig.journal.on_quiet(&rig.term);
    for i in 20..30 {
        rig.slice(format!("{}\r\n", secret(i)).as_bytes());
    }
    let scrolled_off: Vec<String> = (20..25).map(secret).collect();
    let held = region_secrets(&rig.journal);
    assert!(
        scrolled_off.iter().all(|s| held.contains(s.as_bytes())),
        "premise: the raw tail holds what scrolled off"
    );
    assert!(
        matches!(
            rig.recovered().other_slot,
            aterm_restore::recover::SlotVerdict::Valid(_)
        ),
        "premise: the older slot holds a keyframe from before the clear"
    );
    let (before, _) = rig.journal.counts();
    rig.slice(b"\x1b[3Jcleared\r\n");
    assert_eq!(rig.journal.counts().0, before + 1, "the clear's keyframe");
    assert_eq!(
        rig.recovered().other_slot,
        aterm_restore::recover::SlotVerdict::Empty,
        "the older slot, a keyframe from before the clear, is gone"
    );
    // Only what the scrollback held at the clear (secrets 0 to 26): the visible rows
    // still show the newest, and one of them scrolls into the new history after it.
    let cleared: Vec<String> = (0..27).map(secret).collect();
    let held = region_secrets(&rig.journal);
    for token in &cleared {
        assert!(
            !held.contains(token.as_bytes()),
            "{token}, cleared from the scrollback, survives in the region"
        );
    }
    let restore = rig.restore(0).expect("a screen");
    assert_eq!(restore.report.rung, Rung::Exact, "{:?}", restore.report);
    let live_history = crate::term_lock(&rig.term)
        .checkpoint_journal(512)
        .expect("ground")
        .history_lines;
    assert!(
        live_history <= 1,
        "the clear took the history: {live_history}"
    );
    assert_eq!(
        restore.checkpoint.history_lines, live_history,
        "nothing before the clear"
    );
    assert_eq!(visible(&adopted(&restore.checkpoint)), rig.visible());
}

/// The keeper's poison brake and the offers no restore can use: a restorer that died
/// once gets the keyframe alone (never exact), twice none; no region, a region of
/// another kind, and a descriptor that is not read-only (or not the offered size) take
/// the META rung.
#[test]
fn the_brake_and_unusable_offers_fall_to_the_rungs_below() {
    let rig = Rig::new(6, 40);
    rig.slice(b"past the keyframe\r\n");
    let once = rig.restore(1).expect("the keyframe alone");
    assert_eq!(once.report.rung, Rung::Screen);
    assert_eq!(once.report.why, "keyframe-only");
    assert_eq!(once.report.attempts, 1);
    let meta = |r: Result<Restore, RestoreReport>| match r {
        Ok(restore) => panic!("expected the META rung, got {:?}", restore.report),
        Err(report) => {
            assert_eq!(report.rung, Rung::Meta);
            report.why
        }
    };
    assert_eq!(meta(rig.restore(2)), "attempts");
    let mut used = 0u64;
    assert_eq!(meta(offered_restore(None, 0, None, &mut used)), "no-region");
    let mut other = rig.offered();
    other.kind = COMPANION_JOURNAL + 1;
    assert_eq!(
        meta(offered_restore(Some(other), 0, None, &mut used)),
        "not-a-journal"
    );
    let mut short = rig.offered();
    short.len = rig.len - 16 * 1024;
    assert_eq!(
        meta(offered_restore(Some(short), 0, None, &mut used)),
        "map"
    );
    let (_region, writable) =
        // SAFETY: the writable twin is only offered to a restore that refuses it before it
        // maps anything; nothing writes, resizes or maps it through the descriptor.
        unsafe { aterm_uds::shm::testing::create_with_writable_custody(REGION_LEN) }
            .expect("region");
    let offer = OfferedCompanion {
        region: writable,
        kind: COMPANION_JOURNAL,
        len: rig.len,
    };
    assert_eq!(
        meta(offered_restore(Some(offer), 0, None, &mut used)),
        "map"
    );
    // SAFETY: the blank region is never written: `_blank` stays inert while the restore
    // maps the custody descriptor read-only, and nothing else holds it.
    let (_blank, custody) = unsafe { aterm_uds::shm::Region::create(REGION_LEN) }.expect("region");
    let offer = OfferedCompanion {
        region: custody,
        kind: COMPANION_JOURNAL,
        len: rig.len,
    };
    assert_eq!(
        meta(offered_restore(Some(offer), 0, None, &mut used)),
        "header",
        "a region never laid out"
    );
}

/// A restored checkpoint's history is the keyframe's bridge plus what the tail's replay
/// pushed, not the handoff wire's 256 lines: a session that printed 2,000 lines past its
/// keyframe restores exactly with all of them, and once a quiet keyframe has caught it up
/// it restores with the bridge's 512.
#[test]
fn a_deep_history_restores_past_the_handoff_s_256_lines() {
    let rig = Rig::new(8, 40);
    for i in 0..2000 {
        rig.slice(format!("history line {i}\r\n").as_bytes());
    }
    let replayed = rig.restore(0).expect("a screen");
    assert_eq!(replayed.report.rung, Rung::Exact, "{:?}", replayed.report);
    assert!(
        replayed.checkpoint.history_lines > 1900,
        "the replay's history: {}",
        replayed.checkpoint.history_lines
    );
    assert_eq!(visible(&adopted(&replayed.checkpoint)), rig.visible());
    // Which lines, not only how many: the live engine's own projection of as much
    // history, line for line (the grid blob is the scrollback-then-visible line codec).
    let live_history = |lines: u32| {
        crate::term_lock(&rig.term)
            .checkpoint_journal(usize::try_from(lines).expect("lines"))
            .expect("ground")
    };
    let live = live_history(replayed.checkpoint.history_lines);
    assert_eq!(live.history_lines, replayed.checkpoint.history_lines);
    assert!(
        replayed.checkpoint.grid == live.grid,
        "the replay's history is the live history, line for line"
    );
    rig.journal.pass_ms(aterm_restore::policy::QUIET_MS);
    rig.journal.on_quiet(&rig.term);
    let bridged = rig.restore(0).expect("a screen");
    assert_eq!(bridged.report.rung, Rung::Exact, "{:?}", bridged.report);
    assert_eq!(
        usize::try_from(bridged.checkpoint.history_lines).ok(),
        Some(aterm_restore::policy::BRIDGE_CAP),
        "the quiet keyframe's bridge"
    );
    let live = live_history(bridged.checkpoint.history_lines);
    assert!(
        bridged.checkpoint.grid == live.grid,
        "the bridge is the newest 512 lines of the live history, line for line"
    );
    // 2,000 lines on 8 rows leave lines 0 to 1992 in the history; the bridge keeps the
    // newest 512 of them, from line 1481.
    let bridge = i32::try_from(aterm_restore::policy::BRIDGE_CAP).expect("cap");
    assert_eq!(
        Terminal::from_checkpoint(&bridged.checkpoint).get_line_text(-bridge, None),
        Some(format!(
            "history line {}",
            2000 - 8 + 1 - aterm_restore::policy::BRIDGE_CAP
        )),
        "the bridge's oldest line"
    );
}

/// K5 PARKS: output that stops inside a sequence no keyframe carries (an XTGETTCAP
/// query with no ST, as from `cat` of a binary or a program that died mid-query) defers
/// the quiet keyframe, and the session then reads clean to the parse loop's wait, so an
/// idle session inside the query wakes zero times rather than retry five times a
/// second under the term lock. Its restore meanwhile replays the tail and counts the
/// query swallowed. The next slice ends the query, unparks it, and its end writes the
/// deferred keyframe.
#[test]
fn a_deferred_keyframe_parks_the_session_until_its_next_slice() {
    let rig = Rig::new(6, 40);
    rig.slice(b"before the query\r\n\x1bP+q544e");
    assert!(rig.journal.dirty(), "a burst waits for its quiet");
    let (keyframes, deferred) = rig.journal.counts();
    rig.journal.pass_ms(aterm_restore::policy::QUIET_MS);
    rig.journal.on_quiet(&rig.term);
    assert_eq!(
        rig.journal.counts(),
        (keyframes, deferred + 1),
        "the quiet keyframe is deferred inside the query"
    );
    assert!(
        !rig.journal.dirty(),
        "parked: the parse loop blocks instead of waking every 200 ms"
    );
    let parked = rig.restore(0).expect("a screen");
    assert_eq!(parked.report.rung, Rung::Screen);
    assert_eq!(parked.report.why, "swallowed");

    rig.slice(b"\x1b\\after the query\r\n");
    assert_eq!(
        rig.journal.counts().0,
        keyframes + 1,
        "the slice that ended the query wrote the deferred keyframe"
    );
    assert!(!rig.journal.dirty());
    assert_eq!(rig.journal.tail_bytes(), 0);
    assert_eq!(rig.restore(0).expect("a screen").report.rung, Rung::Exact);
}

/// A panic inside the restore costs that one session its screen (the META rung,
/// `restore_why=panic`), and it is raised INSIDE the screen reader's panic fence
/// (`reader_guard`): the window's panic hook files no `crash-<pid>.log` for it, which
/// this launch's crash scan would otherwise show as the dead window's crash, hiding the
/// dead window's own report. The test seam panics where the replay builds its scratch
/// engine and records whether the fence was up there.
#[test]
fn a_panic_inside_the_restore_is_fenced_from_the_crash_report() {
    let rig = Rig::new(6, 40);
    rig.slice(b"a session whose restore panics\r\n");
    seam::arm_panic(true);
    let result = rig.restore(0);
    seam::arm_panic(false);
    match result {
        Ok(restore) => panic!("expected the META rung, got {:?}", restore.report),
        Err(report) => {
            assert_eq!(report.rung, Rung::Meta);
            assert_eq!(report.why, "panic");
        }
    }
    assert_eq!(
        seam::fenced(),
        Some(true),
        "the replay ran inside the fence the panic hook defers to"
    );
    let calm = rig.restore(0).expect("a screen");
    assert_eq!(calm.report.rung, Rung::Exact, "{:?}", calm.report);
    assert_eq!(seam::fenced(), Some(true));
}

/// The replay's test seam: `restore_offered` calls [`seam::replay_reached`] where its
/// scratch engine is built, inside `recover`.
pub(super) mod seam {
    use std::cell::Cell;

    std::thread_local! {
        static PANIC: Cell<bool> = const { Cell::new(false) };
        static FENCED: Cell<Option<bool>> = const { Cell::new(None) };
    }

    /// Note whether the screen reader's fence is up here, and panic when armed.
    pub(crate) fn replay_reached() {
        FENCED.with(|f| f.set(Some(crate::reader_guard::inside())));
        assert!(
            !PANIC.with(Cell::get),
            "a replay that panics (the crash-restore test seam)"
        );
    }

    pub(super) fn arm_panic(on: bool) {
        PANIC.with(|p| p.set(on));
    }

    /// What the last [`replay_reached`] on this thread saw, taken.
    pub(super) fn fenced() -> Option<bool> {
        FENCED.with(Cell::take)
    }
}

/// An alternate-screen session's history rides the saved primary (`alt_grid`), whose
/// lines `history_lines` (the alternate grid's, always 0) does not count: the restore
/// prices every one of them against the recovery's aggregate cell budget, as it prices a
/// main-screen session's, so a recovery of many `vim` or `less` sessions over deep
/// primaries cannot pass the aggregate the cap holds. Each restore here has room for its
/// visible grids and 100 more cells; 2,000 history lines do not fit on either screen.
#[test]
fn an_alternate_screen_session_s_history_is_priced_against_the_budget() {
    for alternate in [false, true] {
        let rig = Rig::new(8, 40);
        for i in 0..2000 {
            rig.slice(format!("history line {i}\r\n").as_bytes());
        }
        if alternate {
            rig.slice(b"\x1b[?1049h\x1b[2J\x1b[Hthe app");
        }
        let free = rig.restore(0).expect("a screen with no budget spent");
        assert_eq!(free.checkpoint.modes.alternate_screen, alternate);
        let cp = &free.checkpoint;
        let mut visible_charge = 0u64;
        crate::seamless::admit_restored_dimensions(
            &mut visible_charge,
            cp.rows,
            cp.cols,
            0,
            cp.alt_grid.is_some(),
        )
        .expect("the visible grids alone");
        let mut probe = u64::MAX / 2;
        let Err(crate::seamless::AdmitRefusal::Aggregate { cap, .. }) =
            crate::seamless::admit_restored_dimensions(&mut probe, 1, 1, 0, false)
        else {
            panic!("the aggregate refuses a spent budget");
        };
        let mut used = cap - visible_charge - 100;
        let report = offered_restore(Some(rig.offered()), 0, None, &mut used)
            .err()
            .unwrap_or_else(|| panic!("alternate={alternate}: 2,000 lines over 100 cells fit"));
        assert_eq!(report.why, "cells", "alternate={alternate}");
    }
}

/// THE READER'S OWN JOURNAL WIRING, on a real pseudo-terminal (raw slave, so byte
/// counts are exact) with the real reader attached (`spawn::attach_reader_with_post`,
/// the gather and parse stages every session runs), where the tests above drive the
/// journal by hand:
///
/// * the gather thread's read count (`SessionJournal::gathered`, stored before each
///   batch is sent): once the bytes are on screen the region's `stream_gathered` is
///   exactly what the program wrote, and the parse stage's `stream_processed` is too,
///   so a restore counts nothing in flight. Without the gather's call the word stays 0,
///   and a window killed between a read and its parse would restore a screen missing
///   those bytes and call it exact;
/// * the journal lane (`GatherMsg::Journal`): a host resize with no output to follow is
///   recorded by the parse loop at the nudge, so a restore stops counting it
///   unjournaled. Without the lane's handler nothing records it until the program
///   writes again, and the wait below (a one-minute hang detector) fails;
/// * a manual reset's synthesized bytes (the parse stage's reset branch): recorded in
///   the hold that applied them, so the restore carries the handback. Without the
///   record a clean session stays clean, and the restore brings back the modes the reset
///   took away while calling itself exact.
///
/// The resize and the reset are each made on a CLEAN session (K1's quiet keyframe
/// awaited first), so no quiet keyframe can catch them up in its capture and mask a
/// missing record.
#[test]
fn the_real_reader_journals_what_it_gathers_and_a_nudged_resize() {
    use aterm_restore::RegionView as _;
    const HANG: std::time::Duration = aterm_spec::wait::MUST_HAPPEN;
    let app = crate::App::headless_for_test();
    let (master, slave) = aterm_pty::open_pty_pair().expect("open_pty_pair");
    // SAFETY: a zeroed termios is a valid out-slot for tcgetattr, then cfmakeraw and
    // tcsetattr on the slave this test owns.
    unsafe {
        let mut tio: libc::termios = std::mem::zeroed();
        assert_eq!(libc::tcgetattr(slave, &mut tio), 0, "tcgetattr");
        libc::cfmakeraw(&mut tio);
        assert_eq!(libc::tcsetattr(slave, libc::TCSANOW, &tio), 0, "tcsetattr");
    }
    let mut session = crate::stub_session(41);
    session.master = master;
    let journal = SessionJournal::create(
        41,
        "s-reader",
        &session.term,
        master,
        None,
        Arc::clone(&session.ctx.reset_lane),
    )
    .expect("a journal");
    session.ctx.crash_journal.install(Arc::clone(&journal));

    /// Park the reader and close both ends, however the test ends.
    struct Teardown(crate::Session, i32);
    impl Drop for Teardown {
        fn drop(&mut self) {
            let _ = crate::spawn::park_reader(
                &mut self.0,
                std::time::Instant::now() + aterm_spec::wait::MUST_HAPPEN,
            );
            let master = std::mem::replace(&mut self.0.master, -1);
            aterm_pty::close_fd(self.1);
            aterm_pty::close_fd(master);
        }
    }
    let (ready_tx, ready_rx) = std::sync::mpsc::channel::<()>();
    let post: crate::spawn::WakePost = Arc::new(move |wake| {
        if matches!(wake, crate::Wake::Ready { .. }) {
            let _ = ready_tx.send(());
        }
        true
    });
    crate::spawn::attach_reader_with_post(
        &mut session,
        crate::WindowId(0),
        post,
        &app.session_factory,
    )
    .expect("attach the real reader");
    let rig = Teardown(session, slave);
    ready_rx
        .recv_timeout(HANG)
        .expect("the reader's gather started");

    let bytes: &[u8] = b"gathered, parsed and journaled\r\n";
    // SAFETY: a bounded write of a live slice to the slave this test owns.
    let wrote = unsafe { libc::write(slave, bytes.as_ptr().cast(), bytes.len()) };
    assert_eq!(wrote, bytes.len() as isize, "the program's write");
    let term = Arc::clone(&rig.0.term);
    let on_screen = aterm_spec::wait::within(HANG, std::time::Duration::from_millis(2), || {
        crate::term_lock(&term)
            .visible_content()
            .contains("gathered, parsed and journaled")
            .then_some(())
    });
    assert!(on_screen.is_some(), "the reader parsed the write");
    // The parse stage records a slice in the hold that processed it, and the gather
    // stored its count before it sent the batch: both are settled once it is on screen.
    let region = journal.snapshot();
    let word = |at| region.load_u64_acquire(at).expect("a header word");
    let written = bytes.len() as u64;
    assert_eq!(
        word(aterm_restore::layout::OFF_STREAM_GATHERED),
        written,
        "the gather thread's count"
    );
    assert_eq!(
        word(aterm_restore::layout::OFF_STREAM_PROCESSED),
        written,
        "the parse stage's count"
    );
    let restore = |journal: &SessionJournal| match aterm_restore::recover(
        &journal.snapshot(),
        RecoverPolicy {
            tail: true,
            config_fp: config_fp(config_hash(None), None),
            scratch_nonce: [0xA5; 32],
        },
        |rows, cols| scratch_engine(rows, cols, None, None),
    ) {
        Recovery::Restored(restored) => *restored,
        Recovery::Meta(why) => panic!("expected a screen: {why:?}"),
    };
    let caught_up = restore(&journal);
    assert!(caught_up.exact, "{:?}", caught_up.losses);
    assert_eq!(caught_up.losses.in_flight_lost, 0);

    // K1's quiet keyframe first (a hang detector on it, not a latency budget): the session
    // is then clean, so its parse loop blocks with no timeout and only the lane's nudge
    // can wake it. A resize made while the burst was still dirty would be caught up by
    // that quiet keyframe's capture instead, nudge or none.
    let clean = aterm_spec::wait::within(HANG, std::time::Duration::from_millis(2), || {
        (journal.tail_bytes() == 0 && !journal.dirty()).then_some(())
    });
    assert!(clean.is_some(), "the quiet keyframe caught the session up");
    // A host resize with no output: the word in the resize's hold, then the nudge.
    let (rows, cols) = {
        let mut t = crate::term_lock(&term);
        let (rows, cols) = (t.rows() - 1, t.cols());
        t.resize(rows, cols);
        journal.host_resized(&t);
        (rows, cols)
    };
    let recorded = aterm_spec::wait::within(HANG, std::time::Duration::from_millis(2), || {
        let r = restore(&journal);
        (r.losses.resizes_unjournaled == 0).then_some(r)
    })
    .expect("the parse loop recorded the nudged resize with no output to follow");
    assert!(recorded.exact, "{:?}", recorded.losses);
    assert_eq!(
        (recorded.checkpoint.rows, recorded.checkpoint.cols),
        (rows, cols)
    );

    // A manual reset (Edit ▸ Reset Terminal, the `reset` verb) of a clean session: a
    // program armed mouse tracking, and the reset hands the terminal back to its host
    // defaults through the parse stage, whose reset branch puts the synthesized bytes on
    // the tail in the hold that applied them. Without that record nothing would mark the
    // clean session dirty, no keyframe would follow, and the restore would bring the
    // armed modes back while calling itself exact.
    let armed: &[u8] = b"\x1b[?1000h\x1b[?1006harmed by a program\r\n";
    // SAFETY: a bounded write of a live slice to the slave this test owns.
    let wrote = unsafe { libc::write(slave, armed.as_ptr().cast(), armed.len()) };
    assert_eq!(wrote, armed.len() as isize, "the program's write");
    let on_screen = aterm_spec::wait::within(HANG, std::time::Duration::from_millis(2), || {
        crate::term_lock(&term)
            .visible_content()
            .contains("armed by a program")
            .then_some(())
    });
    assert!(on_screen.is_some(), "the reader parsed the program's modes");
    let clean = aterm_spec::wait::within(HANG, std::time::Duration::from_millis(2), || {
        (journal.tail_bytes() == 0 && !journal.dirty()).then_some(())
    });
    assert!(
        clean.is_some(),
        "the quiet keyframe caught the armed session up"
    );
    let report = crate::manual_reset::request(
        41,
        &term,
        &rig.0.ctx,
        crate::manual_reset::ResetSource::Verb,
        Some(HANG),
    )
    .expect("the parse stage ran the reset")
    .expect("and answered");
    assert!(
        !report.reverted.is_empty() && report.bytes > 0,
        "premise: the reset handed modes back: {report:?}"
    );
    let reset = restore(&journal);
    assert!(reset.exact, "{:?}", reset.losses);
    assert_eq!(
        reset.checkpoint.modes,
        *crate::term_lock(&term).modes(),
        "the restore carries the reset's handback"
    );
    drop(rig);
}

/// A restored screen is priced against the recovery's aggregate cell budget, as a
/// handoff's is: a recovery whose budget is spent takes the META rung. Its screen goes
/// blank and every history line the region held goes with it, which the restore read,
/// so it is counted: the session's `status history_lost=` and the ledger's, never 0. The
/// band row's count stays the restored screens' (the blank ones' are said apart).
#[test]
fn a_restore_over_the_cell_budget_takes_the_meta_rung() {
    let rig = Rig::new(6, 40);
    for i in 0..3_000 {
        rig.slice(format!("line {i}\r\n").as_bytes());
    }
    rig.export();
    rig.quiet();
    let mut used = u64::MAX - 1;
    let report = offered_restore(Some(rig.offered()), 0, None, &mut used)
        .err()
        .expect("over the budget");
    assert_eq!(report.why, "cells");
    assert_eq!(used, u64::MAX - 1, "a refusal charges nothing");
    let held = crate::term_lock(&rig.term).history_fence().lines();
    assert!(held > 2_900, "premise: a deep history: {held}");
    assert_eq!(report.history_lost, held, "every line the region held");
    let history = meta_history(&report);
    assert!(history.carry.is_none());
    assert_eq!(
        (history.dropped, history.lost),
        (held, held),
        "status history_lost="
    );

    let mut tally = RestoreTally::default();
    tally.note(&report);
    tally.note(&RestoreReport {
        rung: Rung::Exact,
        history_lost: 12,
        ..RestoreReport::meta("-", 0)
    });
    assert_eq!(tally.ledger().history_lost, held + 12, "the ledger's");
    assert_eq!(tally.ledger().history_short, 2);
    assert_eq!(tally.screens_history_lost(), 12, "the band row's");
}

/// `status` says whether a session journals and why not, and what a keeper recovery
/// restored for it.
#[test]
fn the_status_words_say_the_journal_and_the_restore() {
    let slot = JournalSlot::default();
    assert_eq!(slot.status_words(), "journal=off journal_off=keeper-off");
    slot.turn_off("region: no space".to_string());
    assert_eq!(
        slot.status_words(),
        "journal=off journal_off=region:%20no%20space"
    );
    let live = JournalSlot::default();
    live.install(Rig::new(4, 20).journal);
    let bytes = live.get().expect("installed").resident_bytes();
    assert!(
        bytes > 0 && bytes <= 4 * aterm_restore::layout::PAGE,
        "a fresh tab's region: its header and first keyframe's pages ({bytes})"
    );
    assert_eq!(
        live.status_words(),
        format!(
            "journal=live journal_tail=0 journal_ring_lag=0 journal_ring=live journal_bytes={bytes} \
             journal_ledger_lag=0"
        )
    );
    live.note_restored(RestoreReport {
        rung: Rung::Screen,
        why: "in-flight",
        attempts: 0,
        in_flight_lost: 12,
        tail_overflow: false,
        marks_lost: 0,
        resizes_unjournaled: 0,
        history_lost: 0,
    });
    assert_eq!(
        live.status_words(),
        format!(
            "journal=live journal_tail=0 journal_ring_lag=0 journal_ring=live \
             journal_bytes={bytes} journal_ledger_lag=0 restore=screen restore_why=in-flight \
             in_flight_lost=12 tail_overflow=0 marks_lost=0 resizes_unjournaled=0"
        )
    );
    live.get().expect("installed").request_shed();
    assert!(
        live.status_words()
            .starts_with("journal=live journal_tail=0 journal_ring_lag=0 journal_ring=shed "),
        "{}",
        live.status_words()
    );
    assert_eq!(live.restored().map(|r| r.rung), Some(Rung::Screen));
}

/// The fingerprint: one config and cell size, one value (never 0); another config or
/// another cell size, another value.
#[test]
fn the_fingerprint_follows_the_config_and_the_cell_size() {
    let base = config_hash(Some(&aterm_core::config::TerminalConfig::default()));
    assert_eq!(
        base,
        config_hash(Some(&aterm_core::config::TerminalConfig::default()))
    );
    assert_ne!(base, config_hash(None));
    let fp = config_fp(base, Some((9, 18)));
    assert_ne!(fp, 0);
    assert_eq!(fp, config_fp(base, Some((9, 18))));
    assert_ne!(fp, config_fp(base, Some((18, 36))));
    assert_ne!(fp, config_fp(base, None));
    assert_ne!(fp, config_fp(config_hash(None), Some((9, 18))));
}

/// The recovery's tally: the rungs for the band row, and every field `status` reports
/// per session summed, with how many sessions each `restore_why` word named, for the
/// recovery census's ledger row (`restored=`, `restore_lost=`, `restore_why=`), so the
/// ledger shows which loss real crashes hit.
#[test]
fn the_recovery_tally_carries_the_losses_to_the_ledger() {
    let screen = |why, in_flight_lost, tail_overflow, marks_lost| RestoreReport {
        rung: Rung::Screen,
        why,
        attempts: 0,
        in_flight_lost,
        tail_overflow,
        marks_lost,
        resizes_unjournaled: 0,
        history_lost: in_flight_lost / 4,
    };
    let mut tally = RestoreTally::default();
    tally.note(&RestoreReport {
        rung: Rung::Exact,
        why: "-",
        attempts: 0,
        in_flight_lost: 0,
        tail_overflow: false,
        marks_lost: 0,
        resizes_unjournaled: 0,
        history_lost: 0,
    });
    tally.note(&screen("in-flight", 4096, false, 0));
    tally.note(&screen("in-flight", 12, false, 1));
    tally.note(&screen("tail-overflow", 0, true, 0));
    tally.note(&RestoreReport::meta("no-region", 0));
    assert_eq!(
        tally.counts,
        RungCounts {
            exact: 1,
            screen: 3,
            meta: 1
        }
    );
    assert_eq!(
        tally.ledger(),
        aterm_update::recovery_ledger::Restores {
            exact: 1,
            screen: 3,
            meta: 1,
            in_flight_lost: 4108,
            tail_overflows: 1,
            marks_lost: 1,
            resizes_unjournaled: 0,
            history_lost: 1027,
            history_short: 2,
            whys: vec![
                ("in-flight".to_string(), 2),
                ("no-region".to_string(), 1),
                ("tail-overflow".to_string(), 1),
            ],
            refused: 0,
        }
    );
}

// The history ring (increment 5): the scrollback below the restored screen

impl Rig {
    /// The journal worker's ticks, run here until the ring has caught the history up (or
    /// stops moving: a settle, a reflow in flight).
    fn export(&self) {
        let mut last = u64::MAX;
        for _ in 0..64 {
            self.journal.export_tick(&self.term);
            let lag = self.journal.ring_lag();
            if lag == 0 || lag == last {
                return;
            }
            last = lag;
        }
    }

    /// K1's quiet keyframe, now.
    fn quiet(&self) {
        self.journal.pass_ms(aterm_restore::policy::QUIET_MS);
        self.journal.on_quiet(&self.term);
    }

    /// The region restored as a recovering window restores it, adopted as the spawn
    /// adopts it (`spawn::hydrate_adopted_engine`: the checkpoint, the import's keys
    /// reserved) and its history imported as the import worker imports it
    /// (`handoff_history::run_imports`): the restore, the adopted engine and the import's
    /// report.
    fn restore_and_import(&self) -> (RestoreReport, Arc<Mutex<Terminal>>, ImportReport) {
        let restore = self.restore(0).expect("a screen");
        let (term, report) =
            adopt_and_import(restore.checkpoint, restore.history, self.cfg.as_ref());
        (restore.report, term, report)
    }

    /// [`Self::restore_and_import`], held to the accounting every restore owes: the
    /// restored history is the live one's newest lines at the same rows, and every live
    /// line it lacks is either counted by the join (`history_lost`) or one of the carry's
    /// lines the adopted pane's own retention evicted at the attach (`take - imported`).
    /// Returns the report, the import, and how many the retention evicted.
    ///
    /// That retention is the line limit's store share, `limit - ring cap`
    /// (`Grid::set_scrollback_line_limit`), which the attach re-applies while the adopted
    /// pane's hot ring holds almost none of the history yet (`Grid::attach_older_history`):
    /// a tab at its limit keeps up to one hot ring (1,000 lines) fewer of its oldest lines
    /// until new output refills the ring. It is the update handoff's import too, the one
    /// wrinkle `handoff_history`'s module doc says its carry does not count, so it is held
    /// apart here, and the ring and the join are held exactly.
    fn restore_and_import_accounted(&self) -> (RestoreReport, ImportReport, u64) {
        let restore = self.restore(0).expect("a screen");
        let take = restore
            .history
            .carry
            .as_ref()
            .map_or(0, |carry| carry.take());
        let (adopted, import) =
            adopt_and_import(restore.checkpoint, restore.history, self.cfg.as_ref());
        assert_eq!(import.failed, None, "{import:?}");
        let live = history_rows(&self.term);
        let got = history_rows(&adopted);
        assert!(
            live.ends_with(&got),
            "the restored history is the live one's newest lines at the same rows ({} of \
             {} lines)",
            got.len(),
            live.len()
        );
        let evicted = take - import.imported;
        assert_eq!(
            (live.len() - got.len()) as u64,
            restore.report.history_lost + evicted,
            "every line it lacks is counted or evicted by the pane's retention: \
             {:?} {import:?} take {take}",
            restore.report
        );
        // Plus the lines the live engine still staged over its limit (its lazy buffer,
        // drained at its next turn), which the restored pane's attach drains at once.
        let staged_over = crate::term_lock(&self.term)
            .scrollback_line_limit()
            .map_or(0, |limit| live.len().saturating_sub(limit));
        assert!(
            evicted <= (aterm_core::terminal::TIERED_RING_CAP_DEFAULT + staged_over) as u64,
            "the retention evicts at most one hot ring and what the live engine staged \
             over its limit: {evicted} ({staged_over} staged over)"
        );
        (restore.report, import, evicted)
    }
}

use crate::handoff_history::{AdoptedHistory, ImportJob, ImportReport};

/// `checkpoint` adopted with `history` as the spawn and the import worker do it.
fn adopt_and_import(
    checkpoint: TerminalCheckpoint,
    mut history: AdoptedHistory,
    cfg: Option<&aterm_core::config::TerminalConfig>,
) -> (Arc<Mutex<Terminal>>, ImportReport) {
    let term = Arc::new(Mutex::new(crate::spawn::new_live_terminal(
        checkpoint.rows,
        checkpoint.cols,
        cfg,
        None,
    )));
    crate::spawn::hydrate_adopted_engine(
        &term,
        Some(&checkpoint),
        None,
        None,
        None,
        9,
        &mut history,
    );
    let mut reports = crate::handoff_history::run_imports(vec![ImportJob {
        session: 9,
        term: Arc::clone(&term),
        history,
    }]);
    let report = reports.pop().expect("one report");
    (term, report)
}

/// An engine's history as `(absolute row, text)`, oldest first: the rows say the
/// numbering, the text the lines.
fn history_rows(term: &Mutex<Terminal>) -> Vec<(u64, String)> {
    let t = crate::term_lock(term);
    let fence = t.history_fence();
    let lines = t
        .history_lines_since_fence(&fence, fence.oldest, usize::MAX)
        .expect("the fence holds");
    (fence.oldest..)
        .zip(
            lines
                .iter()
                .map(|line| line.to_string().trim_end().to_string()),
        )
        .collect()
}

/// THE WHOLE SCROLLBACK COMES BACK (the plan's increment 5): a session whose history ring
/// the journal worker has caught up restores its screen exactly with no bridge in its
/// keyframe at all, and the import puts every line of its history back below the screen,
/// at the same absolute rows, with nothing counted lost. A ring that lags the history by
/// some lines makes the next keyframe carry exactly those lines as its bridge, and the two
/// meet.
#[test]
fn a_crash_brings_the_whole_scrollback_back_below_the_exact_screen() {
    let rig = Rig::new(8, 40);
    for batch in 0..60 {
        let mut bytes = String::new();
        for i in 0..50 {
            bytes.push_str(&format!(
                "\x1b[3{}mhistory line {}\x1b[0m\r\n",
                i % 8,
                batch * 50 + i
            ));
        }
        rig.slice(bytes.as_bytes());
    }
    rig.export();
    assert_eq!(rig.journal.ring_lag(), 0, "the ring caught the history up");
    rig.quiet();
    let (report, adopted, import) = rig.restore_and_import();
    assert_eq!(report.rung, Rung::Exact, "{report:?}");
    assert_eq!(report.history_lost, 0, "{report:?}");
    let live = history_rows(&rig.term);
    assert!(
        live.len() > 2_900,
        "premise: a deep history: {}",
        live.len()
    );
    assert_eq!(import.failed, None, "{import:?}");
    assert_eq!(
        import.imported,
        live.len() as u64,
        "every line was imported: {import:?}"
    );
    assert_eq!(
        history_rows(&adopted),
        live,
        "the same lines at the same rows"
    );
    assert_eq!(
        visible(&crate::term_lock(&adopted)),
        rig.visible(),
        "the screen"
    );

    // A ring 100 lines behind: the quiet keyframe's bridge is those 100 lines, and the
    // restore joins the ring below them.
    for i in 0..100 {
        rig.slice(format!("after the export {i}\r\n").as_bytes());
    }
    rig.quiet();
    let restored = rig.recovered();
    assert_eq!(
        restored.keyframe.bridge_lines, 100,
        "the bridge is the ring's lag"
    );
    let (report, adopted, import) = rig.restore_and_import();
    assert_eq!(
        (report.rung, report.history_lost),
        (Rung::Exact, 0),
        "{report:?}"
    );
    assert_eq!(import.failed, None);
    assert_eq!(history_rows(&adopted), history_rows(&rig.term));
}

/// THE PROMPTS ABOVE THE SCREEN COME BACK WITH ITS SCROLLBACK: a shell whose history ring
/// has caught up keyframes with no bridge, and its keyframe still carries the shell marks
/// and output blocks whose prompts scrolled off within the bridge cap, so the restored
/// engine, with the ring's lines imported below it at their own rows, holds the live
/// engine's marks and blocks: prompt navigation reaches the same earlier prompt, and the
/// block history holds the same completed commands. Under a zero bridge's own filter it
/// would hold only the marks on the screen.
#[test]
fn a_caught_up_ring_keeps_the_prompts_above_the_screen() {
    let rig = Rig::new(8, 40);
    for command in 0..6 {
        let mut bytes = format!("\x1b]133;A\x07$ \x1b]133;B\x07ls {command}\r\n\x1b]133;C\x07");
        for i in 0..30 {
            bytes.push_str(&format!("output {command}.{i}\r\n"));
        }
        bytes.push_str(&format!("\x1b]133;D;{command}\x07"));
        rig.slice(bytes.as_bytes());
    }
    rig.slice(b"\x1b]133;A\x07$ \x1b]133;B\x07");
    rig.export();
    rig.quiet();
    assert_eq!(
        rig.recovered().keyframe.bridge_lines,
        0,
        "premise: the ring caught up, so the keyframe has no bridge"
    );
    let (live_marks, live_blocks, live_jump, top) = {
        let t = crate::term_lock(&rig.term);
        (
            t.command_marks().to_vec(),
            t.all_blocks().cloned().collect::<Vec<_>>(),
            crate::input::jump_prompt_target(&t, true),
            t.grid().top_visible_absolute_row(),
        )
    };
    assert_eq!(live_marks.len(), 6, "premise: six finished commands");
    assert!(
        live_marks
            .iter()
            .filter(|m| m.prompt_start_row < top)
            .count()
            >= 5,
        "premise: their prompts scrolled into the history"
    );
    let (report, adopted, import) = rig.restore_and_import();
    assert_eq!(
        (report.rung, report.history_lost),
        (Rung::Exact, 0),
        "{report:?}"
    );
    assert_eq!(import.failed, None, "{import:?}");
    assert_eq!(history_rows(&adopted), history_rows(&rig.term));
    let t = crate::term_lock(&adopted);
    assert_eq!(t.command_marks(), &live_marks[..], "the same marks");
    assert_eq!(
        t.all_blocks().cloned().collect::<Vec<_>>(),
        live_blocks,
        "the same blocks"
    );
    assert!(live_jump.is_some_and(|row| row < top));
    assert_eq!(
        crate::input::jump_prompt_target(&t, true),
        live_jump,
        "the previous prompt is reachable"
    );
}

/// A RING HOLDING MORE LINES THAN A SIDECAR MAY (`handoff_history::MAX_EXPORT_LINES`), as
/// an unlimited scrollback of compressible lines makes it (here two thousand, blank but
/// for a numbered marker every 997th, in well under the ring's 6.9 MiB): the restore
/// imports the newest `MAX_EXPORT_LINES` lines at their own rows, as an update's export
/// would carry, and counts every older one. Handed the whole run, the import refused it
/// whole (`more lines than a sidecar may hold`), after the recovery row had said the
/// scrollback came back.
#[test]
fn a_ring_past_the_export_bound_imports_its_newest_lines() {
    use crate::handoff_history::MAX_EXPORT_LINES;
    let cfg = aterm_core::config::TerminalConfig {
        scrollback_limit: None,
        ..aterm_core::config::TerminalConfig::default()
    };
    let rig = Rig::with_config(4, 20, Some(cfg));
    let total = MAX_EXPORT_LINES + 2_000;
    let mut n = 0u64;
    while n < total {
        let mut bytes = String::with_capacity(64 * 1024);
        for _ in 0..16_384 {
            if n.is_multiple_of(997) {
                bytes.push_str(&format!("marker {n}\r\n"));
            } else {
                bytes.push_str("\r\n");
            }
            n += 1;
        }
        rig.slice(bytes.as_bytes());
    }
    rig.export();
    assert_eq!(rig.journal.ring_lag(), 0, "the ring caught the history up");
    rig.quiet();
    let restored = rig.recovered();
    let export = restored.join_facts().expect("held").export.expect("a ring");
    assert!(
        export.lines > MAX_EXPORT_LINES,
        "premise: the ring holds more than a sidecar may: {export:?}"
    );
    let carried = restored.carried_lines;
    let (report, adopted, import) = rig.restore_and_import();
    assert_eq!(import.failed, None, "{import:?}");
    let live = history_rows(&rig.term);
    assert_eq!(
        import.imported + carried,
        MAX_EXPORT_LINES,
        "the newest lines a sidecar may hold, with the carried ones: {import:?}"
    );
    assert_eq!(
        report.history_lost,
        live.len() as u64 - (import.imported + carried),
        "every older line counted"
    );
    let got = history_rows(&adopted);
    assert!(live.ends_with(&got), "the newest lines, at their own rows");
}

/// A config with a scrollback line limit of `lines`.
fn limited(lines: usize) -> aterm_core::config::TerminalConfig {
    aterm_core::config::TerminalConfig {
        scrollback_limit: Some(lines),
        ..aterm_core::config::TerminalConfig::default()
    }
}

/// A line of `len` characters no LZ4 window compresses much, numbered `i`.
fn noisy_line(i: usize, len: usize) -> String {
    const ALPHABET: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789";
    let mut rng = aterm_spec::rng::XorShift::new(0x0151_0000 ^ (i as u64).wrapping_mul(0x9E37));
    let mut line = format!("{i} ");
    while line.len() < len {
        line.push(char::from(ALPHABET[rng.index(ALPHABET.len())]));
    }
    line
}

/// The ring's writer as the worker left it, read under its slot's lock.
fn ring_state(journal: &SessionJournal) -> (Option<u64>, u64, usize) {
    let ring = journal.ring.lock().expect("the ring");
    let ring = ring.as_ref().expect("no tick running");
    (
        ring.writer.first_row(),
        ring.writer.end_row(),
        ring.writer.cap(),
    )
}

/// THE NORMAL STATE OF A LONG-LIVED SHELL: past its scrollback line limit, the engine
/// evicts its oldest lines while the ring, sized at least 1.25 times the limit, still
/// holds them, so every restore joins with the ring's run starting below the engine's
/// oldest row and skips those lines (`skip > 0`, whole chunks of them never decoded). The
/// restored history is the live one's newest lines, line for line at the same rows, with
/// nothing counted by the join ([`Rig::restore_and_import_accounted`] holds what the
/// adopted pane's retention evicts apart). NEGATIVE CONTROL: the same carry with its skip
/// left out imports other lines at those rows.
///
/// Then a worker that fell behind the eviction (the engine evicted lines the ring had not
/// reached): its next chunk read finds a hole and starts the ring over at the engine's
/// oldest row, so its rows stay a run, and the restore after it holds the same accounting.
#[test]
fn a_shell_past_its_line_limit_restores_its_history_line_for_line() {
    let rig = Rig::with_config(6, 60, Some(limited(2_000)));
    let mut n = 0;
    let batch = |rig: &Rig, n: &mut usize, lines: usize| {
        let mut bytes = String::new();
        for _ in 0..lines {
            bytes.push_str(&format!("\x1b[3{}mline {n}\x1b[0m\r\n", *n % 8));
            *n += 1;
        }
        rig.slice(bytes.as_bytes());
    };
    // Batches of 173 lines, one export each: the chunk boundaries do not fall on the row
    // the engine evicts to, so the first chunk the import decodes begins below its first line
    // (the premise asserted below).
    for _ in 0..35 {
        batch(&rig, &mut n, 173);
        rig.export();
    }
    rig.quiet();
    let fence = crate::term_lock(&rig.term).history_fence();
    assert!(
        fence.oldest > 3_000,
        "premise: the engine evicted past its limit: {fence:?}"
    );
    let mut restored = rig.recovered();
    let export = restored.join_facts().expect("held").export.expect("a ring");
    assert!(
        export.skip > 1_024,
        "premise: the ring's run starts whole chunks below the engine's oldest row: \
         {export:?}"
    );
    let (chunks, residual) = restored.ring_chunks_for(&export, 1);
    assert!(
        chunks.start > 0 && residual > 0,
        "premise: the import leaves whole chunks behind and still skips lines inside the \
         first one it decodes ({chunks:?}, {residual})"
    );
    let (report, import, _) = rig.restore_and_import_accounted();
    assert_eq!(
        (report.rung, report.history_lost),
        (Rung::Exact, 0),
        "{report:?}"
    );
    assert!(import.imported > 0, "{import:?}");

    // NEGATIVE CONTROL: the run's lines from its own first row, the skip left out.
    let live = history_rows(&rig.term);
    let history = journal_history(&mut restored);
    let take = history.carry.as_ref().expect("a carry").take();
    let chunks = std::mem::take(&mut restored.ring.chunks);
    let wrong = crate::handoff_history::AdoptedHistory {
        carry: Some(crate::handoff_history::HistoryCarry::journal(
            chunks,
            export.fence.cols,
            0,
            take,
        )),
        ..crate::handoff_history::AdoptedHistory::default()
    };
    let (adopted, _) = adopt_and_import(restored.checkpoint.clone(), wrong, rig.cfg.as_ref());
    assert!(
        !live.ends_with(&history_rows(&adopted)),
        "control: a carry that ignores the skip imports other lines at those rows"
    );

    // The worker falls behind the eviction: 5,000 lines with no export between them.
    let (_, end_before, _) = ring_state(&rig.journal);
    for _ in 0..25 {
        batch(&rig, &mut n, 200);
    }
    let fence = crate::term_lock(&rig.term).history_fence();
    assert!(
        end_before < fence.oldest,
        "premise: the engine evicted lines the ring had not reached"
    );
    rig.export();
    let (first, end, _) = ring_state(&rig.journal);
    assert_eq!(
        (first, end),
        (Some(fence.oldest), fence.end),
        "the ring started over at the engine's oldest row and caught up"
    );
    rig.quiet();
    let (report, import, _) = rig.restore_and_import_accounted();
    assert_eq!(
        (report.rung, report.history_lost),
        (Rung::Exact, 0),
        "{report:?}"
    );
    assert!(import.imported > 0, "{import:?}");
}

/// THE ADAPTIVE CAP GROWS: a corpus that encodes at more bytes per line than the cap was
/// first sized for (32) fills the ring before the line limit does, so the ring evicts
/// lines the engine still holds, and a restore then counts the lines below the ring's
/// run. The worker's next tick grows the cap from the corpus's own rate, starts the ring
/// over at the engine's oldest row and re-exports it, and the restore after it counts
/// nothing (the accounting of [`Rig::restore_and_import_accounted`] at both).
#[test]
fn the_ring_s_cap_grows_to_the_corpus_and_the_history_comes_back_whole() {
    let rig = Rig::with_config(6, 100, Some(limited(20_000)));
    for batch in 0..25 {
        let mut bytes = String::new();
        for i in batch * 1_000..(batch + 1) * 1_000 {
            bytes.push_str(&noisy_line(i, 80));
            bytes.push_str("\r\n");
        }
        rig.slice(bytes.as_bytes());
    }
    rig.export();
    rig.quiet();
    let fence = crate::term_lock(&rig.term).history_fence();
    let (first, _, cap) = ring_state(&rig.journal);
    assert!(
        first.is_some_and(|first| first > fence.oldest),
        "premise: the ring evicted lines the engine holds ({first:?}, {fence:?}, cap {cap})"
    );
    let (report, _, _) = rig.restore_and_import_accounted();
    assert!(
        report.history_lost > 0,
        "the lines below the run: {report:?}"
    );

    rig.journal.export_tick(&rig.term);
    let (first, end, grown) = ring_state(&rig.journal);
    assert!(grown > cap, "the cap grew: {cap} -> {grown}");
    assert_eq!(
        (first, end),
        (Some(fence.oldest), fence.end),
        "the ring started over at the engine's oldest row and caught up"
    );
    let (report, import, _) = rig.restore_and_import_accounted();
    assert_eq!(
        (report.rung, report.history_lost),
        (Rung::Exact, 0),
        "{report:?}"
    );
    assert!(import.imported > 0, "{import:?}");
}

/// A chunk larger than the ring is appended as halves (and halves of halves), oldest
/// first, rows dense: what the ring then holds is the newest of them, every line in
/// order, the older ones evicted as the newer wrap over them (the join counts those). A
/// single line larger than the whole ring starts the ring over past it. Driven on a ring
/// cut to a few hundred bytes, as no adaptive cap is small enough for a real history to
/// need it.
#[test]
fn a_chunk_larger_than_the_ring_is_split_and_a_line_larger_restarts_it() {
    let rig = Rig::new(6, 200);
    let identity = HistoryIdentity {
        renumber_epoch: 0,
        clear_gen: 0,
        reveal_gen: 0,
        cols: 200,
    };
    let line = |text: &str| {
        let mut t = Terminal::new(2, 200);
        t.process(format!("{text}\r\n\r\n").as_bytes());
        let f = t.history_fence();
        t.history_lines_since_fence(&f, f.oldest, 1)
            .expect("holds")
            .remove(0)
    };
    let take_ring = || {
        rig.journal
            .ring
            .lock()
            .expect("the ring")
            .take()
            .expect("no tick running")
    };
    let put_ring = |ring| *rig.journal.ring.lock().expect("the ring") = Some(ring);

    let noisy: Vec<_> = (0..24).map(|i| line(&noisy_line(i, 150))).collect();
    let mut ring = take_ring();
    ring.writer
        .reset_ring(&rig.journal.store, identity, 100, 2_048, false)
        .expect("reset");
    rig.journal
        .append_split(&mut ring.writer, 100, &noisy)
        .expect("split");
    put_ring(ring);
    let restored = rig.recovered();
    assert_eq!(restored.ring.fault, None);
    assert!(
        restored.ring.chunks.iter().all(|chunk| chunk.n_lines < 24),
        "it was split: no chunk holds all 24 lines"
    );
    let first = restored.ring.first_row().expect("a run");
    assert!(
        first > 100,
        "premise: the newer halves wrapped over the older"
    );
    assert_eq!(restored.ring.end_row, 124, "up to the last line");
    let mut row = first;
    let mut lines = Vec::new();
    for chunk in &restored.ring.chunks {
        assert_eq!(chunk.first_row, row, "rows dense");
        row += u64::from(chunk.n_lines);
        lines.extend(chunk.lines().expect("decodes"));
    }
    assert_eq!(row, 124);
    let from = usize::try_from(first - 100).expect("offset");
    assert_eq!(
        aterm_core::scrollback::serialize_lines(&lines),
        aterm_core::scrollback::serialize_lines(&noisy[from..]),
        "the newest lines, in order, at their rows"
    );

    // A line no ring this small holds: the ring starts over past it.
    let mut ring = take_ring();
    ring.writer
        .reset_ring(&rig.journal.store, identity, 500, 256, false)
        .expect("reset");
    rig.journal
        .append_split(
            &mut ring.writer,
            500,
            &[line("a"), line(&noisy_line(99, 199)), line("b")],
        )
        .expect("restart");
    assert_eq!(
        (ring.writer.first_row(), ring.writer.end_row()),
        (Some(502), 503),
        "only the line after the giant one, at its row"
    );
    put_ring(ring);
    let restored = rig.recovered();
    assert_eq!(restored.ring.fault, None);
    assert_eq!(restored.ring.first_row(), Some(502));
    let lines: Vec<_> = restored
        .ring
        .chunks
        .iter()
        .flat_map(|chunk| chunk.lines().expect("decodes"))
        .collect();
    assert_eq!(
        aterm_core::scrollback::serialize_lines(&lines),
        aterm_core::scrollback::serialize_lines(&[line("b")])
    );
}

/// At every boundary of a session's life — slices with the ring caught up, behind, and
/// far behind the bridge cap; keyframes and none — the restore's history is the live
/// history's newest lines, line for line at the same rows, and every older line it lacks
/// is counted (`history_lost`): imported lines plus carried lines are the live history,
/// or a suffix of it with the gap counted, never other lines.
#[test]
fn at_every_boundary_the_restored_history_is_the_live_one_or_a_counted_suffix() {
    let rig = Rig::new(6, 50);
    let mut rng = aterm_spec::rng::XorShift::new(0x5C10);
    let mut joined = 0;
    let mut counted = 0;
    let mut n = 0u64;
    for step in 0..48 {
        // Mostly short bursts, sometimes a run longer than the bridge cap.
        let lines = if step % 11 == 7 {
            700
        } else {
            1 + rng.index(40)
        };
        let mut bytes = String::new();
        for _ in 0..lines {
            bytes.push_str(&format!("step {step} line {n} \u{4e16}\r\n"));
            n += 1;
        }
        rig.slice(bytes.as_bytes());
        match rng.index(4) {
            0 => rig.export(),
            1 => rig.quiet(),
            2 => {
                rig.export();
                rig.quiet();
            }
            _ => {}
        }
        let (report, adopted, import) = rig.restore_and_import();
        assert_eq!(import.failed, None, "step {step}: {import:?}");
        let live = history_rows(&rig.term);
        let got = history_rows(&adopted);
        assert!(
            live.ends_with(&got),
            "step {step}: the restored history is a suffix of the live one ({} of {} lines)",
            got.len(),
            live.len()
        );
        assert_eq!(
            (live.len() - got.len()) as u64,
            report.history_lost,
            "step {step}: every line it lacks is counted"
        );
        if report.history_lost == 0 && import.imported > 0 {
            joined += 1;
        } else if report.history_lost > 0 {
            counted += 1;
        }
    }
    assert!(
        joined > 5,
        "the ring met the keyframe at {joined} boundaries"
    );
    assert!(
        counted > 0,
        "a ring behind the bridge was counted at {counted} boundaries"
    );
}

/// NEGATIVE CONTROL: a chunk the ring lost (a damaged frame in the middle of its run)
/// costs a counted gap, never different lines. The ring's run stops at the damage, so it
/// no longer meets the keyframe's lines (`handoff_history::join`'s `Outrun`): nothing is
/// imported, the checkpoint's own lines stay exact, and every older line is counted.
#[test]
fn a_lost_chunk_is_a_counted_gap_never_other_lines() {
    use aterm_restore::{RegionStore as _, RegionView as _};
    let rig = Rig::new(8, 40);
    for batch in 0..40 {
        let mut bytes = String::new();
        for i in 0..100 {
            bytes.push_str(&format!("line {}\r\n", batch * 100 + i));
        }
        rig.slice(bytes.as_bytes());
    }
    rig.export();
    rig.quiet();
    let recover = |region: &aterm_restore::VecStore| {
        let cell_px = crate::term_lock(&rig.term).host_cell_pixel_size();
        match aterm_restore::recover(
            region,
            RecoverPolicy {
                tail: true,
                config_fp: config_fp(config_hash(None), cell_px),
                scratch_nonce: [0xA5; 32],
            },
            |rows, cols| scratch_engine(rows, cols, None, cell_px),
        ) {
            Recovery::Restored(restored) => restored,
            Recovery::Meta(why) => panic!("expected a screen: {why:?}"),
        }
    };
    // The control: the whole ring, every line back.
    let intact = rig.journal.snapshot();
    let mut whole = recover(&intact);
    assert!(
        whole.ring.chunks.len() >= 3,
        "premise: a run of several chunks"
    );
    let history = journal_history(&mut whole);
    let (adopted, import) = adopt_and_import(whole.checkpoint.clone(), history, None);
    assert_eq!(import.imported, history_rows(&rig.term).len() as u64);
    assert_eq!(history_rows(&adopted), history_rows(&rig.term));

    // One byte flipped inside the run's middle chunk.
    let damaged = intact.snapshot();
    let word = |at| damaged.load_u64_acquire(at).expect("a word");
    let (lo, hi) = (
        word(aterm_restore::layout::OFF_HIST_LO),
        word(aterm_restore::layout::OFF_HIST_HI),
    );
    let at = Geometry::v1().hist_off + usize::try_from(lo + (hi - lo) / 2).expect("offset");
    let mut byte = [0u8; 1];
    damaged.read(at, &mut byte).expect("inside");
    damaged.write(at, &[byte[0] ^ 0x5A]).expect("inside");
    let mut gap = recover(&damaged);
    assert!(
        gap.ring.fault.is_some(),
        "premise: the run stops at the damage"
    );
    let history = journal_history(&mut gap);
    assert!(history.carry.is_none(), "nothing imported across a gap");
    let carried = u64::from(gap.checkpoint.history_lines);
    let live = history_rows(&rig.term);
    assert_eq!(
        history.dropped,
        live.len() as u64 - carried,
        "every older line counted"
    );
    let (adopted, import) = adopt_and_import(gap.checkpoint.clone(), history, None);
    assert_eq!(import.imported, 0);
    let got = history_rows(&adopted);
    assert_eq!(got.len() as u64, carried);
    assert!(live.ends_with(&got), "the carried lines are the live ones");
}

/// A history line no other bytes repeat: 16 characters of 62, drawn from a seeded stream,
/// with no prefix in common with another (`secret`'s shared `secret-` prefix becomes an
/// LZ4 back-reference after its first line, so a byte search of a compressed chunk finds
/// only the first).
fn entropic(i: usize) -> String {
    const ALPHABET: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789";
    let mut rng = aterm_spec::rng::XorShift::new(0xE7_0000 ^ (i as u64).wrapping_mul(0x9E37));
    (0..16)
        .map(|_| char::from(ALPHABET[rng.index(ALPHABET.len())]))
        .collect()
}

/// What a byte search looks for of an [`entropic`] token: its middle ten characters. An
/// LZ4 chunk stores a line's text as literals except where a 4-byte run repeats an
/// earlier one, and the record bytes around each line's text are the same in every line,
/// so a run spanning the token's first or last character and its neighbours repeats and
/// is encoded as a match. Its middle repeats nowhere (62^4 runs, a few thousand lines).
fn needle(token: &str) -> &[u8] {
    &token.as_bytes()[3..13]
}

/// Every ten-byte window of `bytes` that is ASCII letters and digits alone, for [`needle`]
/// lookups: a needle is nothing else, so it occurs in `bytes` exactly when it is one of
/// these, and one pass over the bytes replaces a scan per token. Most of a region is zeroed
/// or never written, and a zero block holds no letter or digit, so each 4 KiB block of
/// zeros is passed over with one comparison; only the spans between them are walked byte by
/// byte, and only their runs of letters and digits (in a region, the tokens' literals) are
/// windowed.
fn windows_of(bytes: &[u8]) -> std::collections::HashSet<&[u8]> {
    const BLOCK: usize = 4096;
    const ZEROS: [u8; BLOCK] = [0; BLOCK];
    let mut spans = Vec::new();
    let mut span_from = None;
    for (i, block) in bytes.chunks(BLOCK).enumerate() {
        if block == &ZEROS[..block.len()] {
            if let Some(from) = span_from.take() {
                spans.push(&bytes[from..i * BLOCK]);
            }
        } else if span_from.is_none() {
            span_from = Some(i * BLOCK);
        }
    }
    if let Some(from) = span_from {
        spans.push(&bytes[from..]);
    }
    spans
        .into_iter()
        .flat_map(|span| span.split(|b| !b.is_ascii_alphanumeric()))
        .flat_map(|run| run.windows(10))
        .collect()
}

/// The history ring's bytes, the physical span.
fn ring_bytes(journal: &SessionJournal) -> Vec<u8> {
    use aterm_restore::RegionView as _;
    let region = journal.snapshot();
    let g = Geometry::v1();
    let mut ring = vec![0u8; g.hist_cap_phys];
    region.read(g.hist_off, &mut ring).expect("the ring");
    ring
}

/// The whole region's bytes.
fn region_bytes(journal: &SessionJournal) -> Vec<u8> {
    use aterm_restore::RegionView as _;
    let region = journal.snapshot();
    let mut bytes = vec![0u8; region.len()];
    region.read(0, &mut bytes).expect("the copy reads");
    bytes
}

/// `n` lines of [`entropic`] tokens journaled into `rig` and exported; returns the tokens
/// whose [`needle`] the ring holds, after requiring nearly all of them, the oldest and the
/// newest among them, and a ring past one 16 KiB page.
fn exported_tokens(rig: &Rig, n: usize) -> Vec<String> {
    for batch in 0..n / 100 {
        let mut bytes = String::new();
        for i in batch * 100..(batch + 1) * 100 {
            bytes.push_str(&entropic(i));
            bytes.push_str("\r\n");
        }
        rig.slice(bytes.as_bytes());
    }
    rig.quiet();
    rig.export();
    assert_eq!(rig.journal.ring_lag(), 0, "premise: the ring caught up");
    let ring = ring_bytes(&rig.journal);
    let windows = windows_of(&ring);
    let found: Vec<String> = (0..n)
        .map(entropic)
        .filter(|t| windows.contains(needle(t)))
        .collect();
    assert!(
        found.len() * 100 >= n * 95,
        "premise: the ring stores nearly every token verbatim ({} of {n})",
        found.len()
    );
    assert!(
        found.contains(&entropic(1)) && found.contains(&entropic(n - 10)),
        "premise: the oldest and the newest lines among them"
    );
    let touched = rig
        .journal
        .ring
        .lock()
        .expect("the ring")
        .as_ref()
        .map_or(0, |ring| ring.writer.touched());
    assert!(touched > 16 * 1024, "premise: past one page: {touched}");
    found
}

/// An ED3 empties and zeroes the history ring as soon as the worker sees it (the parse
/// thread wakes it), and with the clear's keyframe settled no byte of the region holds a
/// line the person cleared; the restore after it imports none of them and counts nothing.
/// The tokens span three chunks and several pages, and each one searched for after the
/// clear was found in the ring before it.
#[test]
fn a_clear_zeroes_the_ring_and_no_cleared_line_comes_back() {
    let rig = Rig::new(4, 30);
    let found = exported_tokens(&rig, 3_000);
    assert_eq!(rig.journal.ring_signal.load(Ordering::Relaxed), 0);
    rig.slice(b"\x1b[3Jcleared\r\n");
    assert_ne!(
        rig.journal.ring_signal.load(Ordering::Relaxed) & RING_NUDGED,
        0,
        "the clear wakes the worker"
    );
    rig.journal.export_tick(&rig.term);
    let region = region_bytes(&rig.journal);
    let windows = windows_of(&region);
    for token in &found {
        assert!(
            !windows.contains(needle(token)),
            "{token}, cleared from the scrollback, survives in the region"
        );
    }
    let (report, adopted, import) = rig.restore_and_import();
    assert_eq!(
        (report.rung, report.history_lost),
        (Rung::Exact, 0),
        "{report:?}"
    );
    assert_eq!(import.failed, None);
    let rows = history_rows(&adopted);
    assert!(
        rows.iter().all(|(_, text)| !found.contains(text)),
        "no cleared line imported: {rows:?}"
    );
    assert_eq!(rows, history_rows(&rig.term));
}

/// A clear made while an off-thread reflow holds the store (a window drag) still zeroes
/// the ring at the worker's next tick: the clear generation moves even while the store is
/// out, and the tick that cannot read a line still empties the ring, rather than wait for
/// a store a drag can keep out for as long as it lasts.
#[test]
fn a_clear_during_an_offloaded_reflow_still_zeroes_the_ring() {
    let rig = Rig::new(6, 40);
    let found = exported_tokens(&rig, 2_000);
    let pending = crate::term_lock(&rig.term)
        .resize_offloading_scrollback(6, 30)
        .expect("premise: a width change offloads the store");
    assert!(
        crate::term_lock(&rig.term).history_fence().detached,
        "premise: the store is out"
    );
    rig.slice(b"\x1b[3J");
    rig.journal.export_tick(&rig.term);
    let ring = ring_bytes(&rig.journal);
    let windows = windows_of(&ring);
    for token in &found {
        assert!(
            !windows.contains(needle(token)),
            "{token}, cleared while the store was out, survives in the ring"
        );
    }
    let region = region_bytes(&rig.journal);
    let windows = windows_of(&region);
    for token in &found {
        assert!(
            !windows.contains(needle(token)),
            "{token} survives elsewhere in the region"
        );
    }
    // The reflow lands (its store, from before the clear, is dropped at the re-attach),
    // and once the new width has settled the restore brings back no cleared line.
    let next = crate::term_lock(&rig.term).finish_resize_offload(pending.reflow());
    assert!(next.is_none(), "no second reflow");
    rig.journal.pass_ms(SETTLE_MS);
    rig.export();
    let (_, adopted, import) = rig.restore_and_import();
    assert_eq!(import.failed, None);
    let rows = history_rows(&adopted);
    assert!(
        rows.iter().all(|(_, text)| !found.contains(text)),
        "no cleared line imported: {rows:?}"
    );
}

/// A reflow renumbers the history: the ring waits two seconds of an unchanged fence before
/// it starts over (a window drag reflows sixty times a second), and a restore inside that
/// window counts the history it cannot join; after it, the ring is re-exported and the
/// history comes back whole.
#[test]
fn a_reflow_inside_the_settle_is_counted_then_the_ring_starts_over() {
    let rig = Rig::new(6, 40);
    for i in 0..300 {
        rig.slice(format!("a line long enough to rewrap at thirty columns {i}\r\n").as_bytes());
    }
    rig.export();
    rig.quiet();
    {
        let mut t = crate::term_lock(&rig.term);
        t.resize(6, 30);
        rig.journal.host_resized(&t);
    }
    rig.journal.on_nudge(&rig.term);
    rig.quiet();
    rig.journal.export_tick(&rig.term);
    assert!(
        rig.journal.ring_lag() > 0,
        "the ring is settling, not exporting"
    );
    let (report, adopted, import) = rig.restore_and_import();
    assert!(
        report.history_lost > 0,
        "the unjoinable history is counted: {report:?}"
    );
    assert_eq!(import.imported, 0);
    let live = history_rows(&rig.term);
    assert!(
        live.ends_with(&history_rows(&adopted)),
        "the carried lines are the live ones"
    );
    assert_eq!(
        (live.len() - history_rows(&adopted).len()) as u64,
        report.history_lost
    );

    rig.journal.pass_ms(SETTLE_MS);
    rig.export();
    assert_eq!(
        rig.journal.ring_lag(),
        0,
        "the ring started over under the new identity"
    );
    let (report, adopted, import) = rig.restore_and_import();
    assert_eq!(report.history_lost, 0, "{report:?}");
    assert_eq!(import.failed, None);
    assert_eq!(history_rows(&adopted), history_rows(&rig.term));
}

/// While the alternate screen is up the history lives on the saved primary: the ring
/// exports it from there, and the restore joins and imports it there.
#[test]
fn an_alternate_screen_session_s_history_joins_on_its_primary() {
    let rig = Rig::new(6, 40);
    for i in 0..1500 {
        rig.slice(format!("shell line {i}\r\n").as_bytes());
    }
    rig.slice(b"\x1b[?1049h\x1b[2J\x1b[Hthe app's frame");
    rig.export();
    rig.quiet();
    let (report, adopted, import) = rig.restore_and_import();
    assert_eq!(
        (report.rung, report.history_lost),
        (Rung::Exact, 0),
        "{report:?}"
    );
    assert!(crate::term_lock(&adopted).is_alternate_screen());
    assert_eq!(import.failed, None);
    let live = history_rows(&rig.term);
    assert!(
        live.len() > 1_400,
        "premise: the primary's history: {}",
        live.len()
    );
    assert_eq!(
        history_rows(&adopted),
        live,
        "the primary's history, line for line"
    );
}

/// The worker on its own thread: it wakes on a session's growth and exports until the ring
/// has caught up, with no tick asked for, a tick after its last (the journal's held clock
/// moved on here, never slept; each wait a one-minute hang detector, never a latency
/// budget). An adopted session's export waits for its import, and its settle starts it and
/// forces a keyframe of the renumbered history.
#[test]
fn the_worker_exports_on_growth_and_a_held_export_on_its_import() {
    const HANG: std::time::Duration = aterm_spec::wait::MUST_HAPPEN;
    struct Stop(Arc<HistoryWorker>);
    impl Drop for Stop {
        fn drop(&mut self) {
            self.0.stop_ring_worker();
        }
    }
    let gap = std::time::Duration::from_millis(5);
    let worker = Stop(HistoryWorker::start_held_ring_worker().expect("the worker's thread"));
    let rig = Rig::new(6, 40);
    rig.journal.start_export_with(&worker.0, &rig.term, false);
    let started = aterm_spec::wait::within(HANG, gap, || {
        let ring = rig.journal.ring.lock().expect("the ring");
        ring.as_ref().and_then(|ring| ring.last_tick)
    });
    assert_eq!(
        started,
        Some(0),
        "the start's nudged tick, at 0 on the held clock"
    );
    // A tick's worth of the clock, which wakes nobody: the worker sleeps with nothing due.
    // Then one read grows the history by 2,000 lines, and only its own wake can bring the
    // worker's next tick (a growth is due a tick after the last).
    rig.journal.pass_ms(TICK_MS);
    let lines: String = (0..2000).map(|i| format!("line {i}\r\n")).collect();
    rig.slice(lines.as_bytes());
    let caught_up =
        aterm_spec::wait::within(HANG, gap, || (rig.journal.ring_lag() == 0).then_some(()));
    assert!(caught_up.is_some(), "the worker caught the ring up");

    let held = Rig::new(6, 40);
    held.journal.start_export_with(&worker.0, &held.term, true);
    for i in 0..500 {
        held.slice(format!("line {i}\r\n").as_bytes());
    }
    assert!(
        held.journal.ring_due(held.journal.ring_now_ms()).is_none(),
        "held: nothing due"
    );
    held.quiet();
    let (before, _) = held.journal.counts();
    held.journal.import_settled();
    held.journal.on_nudge(&held.term);
    assert_eq!(
        held.journal.counts().0,
        before + 1,
        "the renumbering's keyframe"
    );
    let caught_up =
        aterm_spec::wait::within(HANG, gap, || (held.journal.ring_lag() == 0).then_some(()));
    assert!(caught_up.is_some(), "the settled export caught the ring up");
}

/// `status journal_ring_lag=` counts history lines, never rows: before the ring's first
/// export (and while an export waits for an import or a settle, when nothing is
/// published) a shell past its line limit lags by the lines it holds, not by its end row.
#[test]
fn the_ring_lag_counts_history_lines_never_rows() {
    let cfg = aterm_core::config::TerminalConfig {
        scrollback_limit: Some(1_000),
        ..aterm_core::config::TerminalConfig::default()
    };
    let rig = Rig::with_config(4, 40, Some(cfg));
    for batch in 0..50 {
        let mut bytes = String::new();
        for i in 0..100 {
            bytes.push_str(&format!("line {}\r\n", batch * 100 + i));
        }
        rig.slice(bytes.as_bytes());
    }
    let fence = crate::term_lock(&rig.term).history_fence();
    assert!(
        fence.oldest > 3_000,
        "premise: the engine evicted past its limit: {fence:?}"
    );
    assert_eq!(
        rig.journal.ring_lag(),
        fence.lines(),
        "nothing exported yet"
    );
    rig.export();
    assert_eq!(rig.journal.ring_lag(), 0, "caught up");
}

/// When the worker next ticks a session: now for a nudge, a second after the last tick for
/// a growth, five while the history floods, and never for an idle one. Read against the
/// tick's own clock reading (its `last_tick`), never a time this test takes around it.
#[test]
fn the_ring_is_due_on_a_nudge_a_growth_and_less_often_in_a_flood() {
    let rig = Rig::new(4, 40);
    let now = || rig.journal.ring_now_ms();
    let last_tick = || {
        rig.journal
            .ring
            .lock()
            .expect("the ring")
            .as_ref()
            .and_then(|ring| ring.last_tick)
            .expect("a tick ran")
    };
    assert_eq!(rig.journal.ring_due(now()), None, "idle: nothing due");
    rig.journal.nudge_ring();
    let at = now();
    assert_eq!(rig.journal.ring_due(at), Some(at), "a nudge: now");
    rig.slice(b"one\r\ntwo\r\nthree\r\nfour\r\nfive\r\nsix\r\n");
    rig.journal.export_tick(&rig.term);
    assert_eq!(rig.journal.ring_due(now()), None, "caught up: nothing due");
    rig.slice(b"a line that scrolls one into the history\r\n");
    assert_eq!(
        rig.journal.ring_due(now()),
        Some(last_tick() + TICK_MS),
        "a growth: a tick after the last"
    );
    // A flood: more lines than two ticks export (one tick exports exactly FLOOD_LINES).
    let flood = "x\r\n".repeat(2 * usize::try_from(FLOOD_LINES).expect("lines") + 10);
    rig.slice(flood.as_bytes());
    rig.journal.export_tick(&rig.term);
    assert!(
        rig.journal.ring_lag() > FLOOD_LINES,
        "premise: a backlog past the next tick too"
    );
    assert_eq!(
        rig.journal.ring_due(now()),
        Some(last_tick() + FLOOD_TICK_MS),
        "a flood: every five seconds"
    );
    // Quiescence (K1 nudges the worker) is never a flood: the backlog the nudged tick
    // leaves is caught up at the normal pace from there.
    rig.journal.nudge_ring();
    rig.journal.export_tick(&rig.term);
    assert!(
        rig.journal.ring_lag() > 0,
        "premise: a backlog past the nudged tick"
    );
    assert_eq!(
        rig.journal.ring_due(now()),
        Some(last_tick() + TICK_MS),
        "after a nudge, every second"
    );
}

// The memory shed (increment 6): history rings taken away under pressure

/// The region a custody descriptor names, restored as a recovering window restores it.
fn restore_custody(
    custody: &OwnedFd,
    len: u64,
    cfg: Option<&aterm_core::config::TerminalConfig>,
) -> Result<Restore, RestoreReport> {
    let mut used = 0u64;
    offered_restore(
        Some(OfferedCompanion {
            region: custody.try_clone().expect("dup"),
            kind: COMPANION_JOURNAL,
            len,
        }),
        0,
        cfg,
        &mut used,
    )
}

/// A session with a deep history of lines LZ4 barely compresses, its ring caught up and
/// its quiet keyframe written: a ring of a few hundred KiB, the shed's subject.
fn deep_rig() -> Rig {
    let rig = Rig::new(8, 80);
    for batch in 0..40 {
        let mut bytes = String::new();
        for i in 0..50 {
            bytes.push_str(&noisy_line(batch * 50 + i, 76));
            bytes.push_str("\r\n");
        }
        rig.slice(bytes.as_bytes());
    }
    rig.export();
    assert_eq!(rig.journal.ring_lag(), 0, "premise: the ring caught up");
    rig.quiet();
    rig
}

/// THE MEMORY SHED, ONE SESSION (the plan's increment 6): a session whose history ring
/// memory pressure took away journals into a fresh region whose ring is never written, so
/// the pages the ring touched leave this window; its screen still restores exactly, and the
/// restore counts every history line above the keyframe's bridge lost, line for line (the
/// restored history is the live one's newest lines at the same rows). The region it left is
/// closed with a SUPERSEDED record: a window that died before the keeper took the new
/// region's COMPANION restores its screen from it, pulsed, never as exact. Output while shed
/// exports nothing; once the shed eases the ring is exported again and a crash brings the
/// whole scrollback back.
#[test]
fn a_shed_session_restores_its_screen_exactly_and_counts_its_history_lost() {
    let rig = deep_rig();
    let ring = rig.journal.ring_bytes();
    let before = rig.journal.resident_bytes();
    assert!(ring > 128 * 1024, "premise: a ring worth shedding ({ring})");

    rig.journal.request_shed();
    assert!(rig.journal.ring_shed());
    assert_eq!(
        rig.journal.regions_replaced(),
        0,
        "replaced at the next tick"
    );
    rig.journal.export_tick(&rig.term);
    assert_eq!(rig.journal.regions_replaced(), 1);
    let after = rig.journal.resident_bytes();
    let geometry = Geometry::v1();
    assert!(
        !rig.journal
            .store
            .with(|region| region.touches(geometry.hist_off, geometry.hist_cap_phys)),
        "the fresh region's ring is never written"
    );
    assert!(
        after < before && after < ring,
        "the ring's pages left: {before} -> {after} bytes (ring {ring})"
    );
    assert_eq!(rig.journal.ring_bytes(), 0);

    // The fresh region: the link is off here, so its custody waits in the session's slot,
    // as a first one waits for the session's registration.
    let (fresh, len) = rig
        .journal
        .take_custody()
        .expect("the fresh region's custody, kept for a registration");
    let restore = restore_custody(&fresh, len, rig.cfg.as_ref()).expect("a screen");
    assert_eq!(restore.report.rung, Rung::Exact, "{:?}", restore.report);
    assert!(restore.history.carry.is_none(), "no ring to carry from");
    let carried = u64::from(restore.checkpoint.history_lines);
    assert!(carried > 0, "the keyframe's own bridge of history");
    let (adopted, import) = adopt_and_import(restore.checkpoint, restore.history, None);
    assert_eq!(import.imported, 0);
    let live = history_rows(&rig.term);
    let got = history_rows(&adopted);
    assert!(live.ends_with(&got), "the newest lines, at the same rows");
    assert_eq!(
        restore.report.history_lost,
        (live.len() - got.len()) as u64,
        "every line above the bridge counted"
    );
    assert_eq!(
        visible(&crate::term_lock(&adopted)),
        rig.visible(),
        "the screen"
    );

    // The region it left, as the keeper holds it until the COMPANION reaches it.
    let stale = rig.restore(0).expect("the old region's screen");
    assert_eq!(
        (stale.report.rung, stale.report.why),
        (Rung::Screen, "superseded"),
        "{:?}",
        stale.report
    );

    // Shed, the ring takes nothing, and the session keeps journaling its screen exactly.
    for i in 0..300 {
        rig.slice(format!("while shed {i}\r\n").as_bytes());
    }
    rig.export();
    assert_eq!(rig.journal.ring_bytes(), 0, "nothing exported while shed");
    rig.quiet();
    let restore = restore_custody(&fresh, len, rig.cfg.as_ref()).expect("a screen");
    assert_eq!(restore.report.rung, Rung::Exact, "{:?}", restore.report);

    // The shed eases: the ring takes the whole history again, and a crash brings it back.
    rig.journal.request_unshed();
    assert!(!rig.journal.ring_shed());
    rig.export();
    assert_eq!(rig.journal.ring_lag(), 0, "the ring caught up again");
    rig.quiet();
    let restore = restore_custody(&fresh, len, rig.cfg.as_ref()).expect("a screen");
    assert_eq!(
        (restore.report.rung, restore.report.history_lost),
        (Rung::Exact, 0),
        "{:?}",
        restore.report
    );
    let (adopted, import) = adopt_and_import(restore.checkpoint, restore.history, None);
    assert_eq!(import.failed, None, "{import:?}");
    assert_eq!(
        history_rows(&adopted),
        history_rows(&rig.term),
        "every line"
    );
}

/// A ring that never touched a page (an idle shell's) is shed in place: there is nothing
/// to free, so the region stays, and so does its custody.
#[test]
fn an_untouched_ring_is_shed_in_place() {
    let rig = Rig::new(6, 40);
    rig.slice(b"a prompt, no history\r\n");
    assert_eq!(rig.journal.ring_bytes(), 0, "premise: an untouched ring");
    rig.journal.request_shed();
    rig.journal.export_tick(&rig.term);
    assert!(rig.journal.ring_shed());
    assert_eq!(rig.journal.regions_replaced(), 0, "nothing to replace");
    assert!(
        rig.journal.take_custody().is_none(),
        "the same region, the same custody"
    );
    rig.quiet();
    assert_eq!(rig.restore(0).expect("a screen").report.rung, Rung::Exact);
}

/// A shed replacement the parser defers (K5: it holds a sequence no keyframe carries whole)
/// changes nothing, and waits for the program's next bytes as the parse loop does: never
/// due until the parse thread records them (an idle session parked mid-sequence costs the
/// worker nothing, where it used to cost a fresh region and a term-lock hold five times a
/// second), then tried no sooner than K1's quiet after its last try, with the region the
/// first try created (no second `shm_open`). Once the sequence ends it lands. Meanwhile the
/// session says `journal_ring=shedding`, the ring's pages still this window's.
#[test]
fn a_replacement_the_parser_defers_waits_for_the_programs_next_bytes() {
    let rig = deep_rig();
    let slot = JournalSlot::default();
    slot.install(Arc::clone(&rig.journal));
    let ring_word = || {
        slot.status_words()
            .split(' ')
            .find_map(|word| word.strip_prefix("journal_ring="))
            .expect("journal_ring=")
            .to_string()
    };
    let now = || rig.journal.ring_now_ms();
    // An unterminated XTGETTCAP query: the parser holds it, and no keyframe can.
    rig.slice(b"\x1bP+q544e");
    rig.journal.request_shed();
    assert_eq!(ring_word(), "shedding", "owed, not yet replaced");
    rig.journal.export_tick(&rig.term);
    assert_eq!(rig.journal.regions_replaced(), 0, "deferred");
    assert!(rig.journal.spare_kept(), "the region it created, kept");
    assert_eq!(rig.journal.ring_due(now()), None, "parked: never due");
    rig.journal.pass_ms(100 * aterm_restore::policy::QUIET_MS);
    assert_eq!(
        rig.journal.ring_due(now()),
        None,
        "however long the program stays silent"
    );
    assert_eq!(ring_word(), "shedding");

    // More of the query, still unterminated: due again, deferred again, parked again,
    // on the region the first try made.
    rig.slice(b"54");
    assert!(
        rig.journal.ring_due(now()).is_some_and(|due| due <= now()),
        "the program wrote: due"
    );
    let last_try = now();
    rig.journal.export_tick(&rig.term);
    assert_eq!(rig.journal.regions_replaced(), 0, "deferred again");
    assert!(rig.journal.spare_kept());
    assert_eq!(rig.journal.ring_due(now()), None, "parked again");

    rig.slice(b"\x1b\\done\r\n");
    let quiet = aterm_restore::policy::QUIET_MS;
    assert_eq!(
        rig.journal.ring_due(now()),
        Some(last_try + quiet),
        "due once the query ended, no sooner than K1's quiet after the last try"
    );
    rig.journal.export_tick(&rig.term);
    assert_eq!(rig.journal.regions_replaced(), 0, "not before that quiet");
    rig.journal.pass_ms(quiet);
    rig.journal.export_tick(&rig.term);
    assert_eq!(
        rig.journal.regions_replaced(),
        1,
        "landed once the query ended"
    );
    assert!(
        !rig.journal.spare_kept(),
        "the kept region is the session's now"
    );
    assert_eq!(rig.journal.ring_due(now()), None, "nothing owed");
    assert_eq!(ring_word(), "shed");
}

/// The bytes of the region `custody` names, as the keeper holds it: read while nothing
/// writes it (every region here is written only by this test's thread, and only between
/// reads, the contract [`offered_restore`] states too).
fn custody_bytes(custody: &OwnedFd, len: u64) -> Vec<u8> {
    use std::os::fd::AsFd as _;
    let len = usize::try_from(len).expect("a region's length");
    // SAFETY: the region's one writer is this thread's journal, which writes nothing while
    // the mapping lives; the mapping is dropped before this returns.
    let region = unsafe { aterm_uds::shm::Region::map_readonly(custody.as_fd(), len) }
        .expect("the custody maps");
    let mut bytes = vec![0u8; region.len()];
    region.read_into(0, &mut bytes).expect("the region reads");
    bytes
}

/// A CLEAR WHILE THE REPLACEMENT IS OWED STILL ZEROES THE RING AT ONCE (plan §5 claim 4).
/// A shed ring's region can wait to be replaced for as long as the program sits
/// mid-sequence (K5) or no region can be created, and the keeper holds that region until
/// the replacement's COMPANION lands: the scrollback the person clears meanwhile must not
/// stay in it. Here the replacement is deferred, then the program ends its sequence and
/// clears (ED3): the tick the clear wakes zeroes the ring in the region the keeper holds
/// before anything else, whether or not the replacement (due K1's quiet after the deferred
/// try) lands in that same tick, and no line the person cleared is left in that region.
/// NEGATIVE CONTROL: before the ED3 the keeper's region held every token; a shed tick that
/// only replaces (the code before this change) supersedes that region with its ring as it
/// was, or leaves it untouched while the replacement waits.
#[test]
fn a_clear_while_the_replacement_is_owed_zeroes_the_ring_at_once() {
    let rig = Rig::new(4, 30);
    let found = exported_tokens(&rig, 3_000);
    rig.slice(b"\x1bP+q544e");
    rig.journal.request_shed();
    rig.journal.export_tick(&rig.term);
    assert_eq!(rig.journal.regions_replaced(), 0, "premise: deferred");
    assert!(rig.journal.ring_shedding(), "premise: owed");
    let held = custody_bytes(&rig.custody, rig.len);
    let windows = windows_of(&held);
    assert!(
        found.iter().all(|token| windows.contains(needle(token))),
        "control: the keeper's region still holds the history"
    );

    rig.slice(b"\x1b\\\x1b[3Jcleared\r\n");
    let now = rig.journal.ring_now_ms();
    assert_eq!(
        rig.journal.ring_due(now),
        Some(now),
        "the clear wakes the worker"
    );
    rig.journal.export_tick(&rig.term);
    let held = custody_bytes(&rig.custody, rig.len);
    let windows = windows_of(&held);
    for token in &found {
        assert!(
            !windows.contains(needle(token)),
            "{token}, cleared while the shed's replacement was owed, survives in the region \
             the keeper holds"
        );
    }
    rig.journal.pass_ms(aterm_restore::policy::QUIET_MS);
    rig.journal.export_tick(&rig.term);
    assert_eq!(rig.journal.regions_replaced(), 1, "the replacement lands");
    assert!(!rig.journal.ring_shedding());
}

/// THE SHED POLICY: under WARN the largest rings go, largest first, until those left hold
/// at most half; under CRITICAL every ring, and every ring a session opens while it holds;
/// after the pressure has stayed eased a minute the rings come back one a second, oldest
/// session first; a report of pressure meanwhile cancels the wait, and a repeated NORMAL
/// keeps the first one's.
#[test]
fn the_shed_policy_sheds_the_largest_and_brings_rings_back_one_at_a_time() {
    const MIB: usize = 1 << 20;
    let ring = |id, bytes, shed| RingSize { id, bytes, shed };
    let mut p = ShedPolicy::default();
    let rings = [
        ring(1, MIB, false),
        ring(2, 4 * MIB, false),
        ring(3, MIB / 2, false),
        ring(4, 0, false),
    ];
    assert_eq!(p.plan(None, 0, &rings), ShedPlan::default(), "no pressure");
    assert_eq!(
        p.plan(Some(PressureLevel::Warn), 10, &rings).shed,
        vec![2],
        "the largest, until half of 5.5 MiB is gone"
    );
    let even = [
        ring(1, MIB, false),
        ring(2, MIB, false),
        ring(3, MIB, false),
    ];
    assert_eq!(
        ShedPolicy::default()
            .plan(Some(PressureLevel::Warn), 0, &even)
            .shed,
        vec![1, 2],
        "ties by session: two of three to keep at most half"
    );
    let after_warn = [
        ring(1, MIB, false),
        ring(2, 0, true),
        ring(3, MIB / 2, false),
        ring(4, 0, false),
    ];
    assert!(
        p.plan(None, 20, &after_warn).shed.is_empty(),
        "under WARN, no ring is shed between reports"
    );
    assert_eq!(
        p.plan(Some(PressureLevel::Critical), 30, &after_warn).shed,
        vec![1, 3, 4],
        "CRITICAL: every ring left, an untouched one too"
    );
    let all_shed = [
        ring(1, 0, true),
        ring(2, 0, true),
        ring(3, 0, true),
        ring(4, 0, true),
    ];
    let newcomer = [all_shed.as_slice(), &[ring(5, 0, false)]].concat();
    assert_eq!(
        p.plan(None, 40, &newcomer).shed,
        vec![5],
        "a ring opened under CRITICAL"
    );
    let all_shed = [all_shed.as_slice(), &[ring(5, 0, true)]].concat();

    let eased = 1_000;
    assert_eq!(
        p.plan(Some(PressureLevel::Normal), eased, &all_shed),
        ShedPlan::default()
    );
    assert_eq!(p.back_due(&all_shed), Some(eased + RING_BACK_HOLD_MS));
    assert_eq!(
        p.plan(None, eased + RING_BACK_HOLD_MS - 1, &all_shed).back,
        None,
        "not before the hold"
    );
    assert_eq!(
        p.plan(Some(PressureLevel::Normal), eased + 30_000, &all_shed),
        ShedPlan::default(),
        "a repeated NORMAL keeps the first one's hold"
    );
    let first_back = eased + RING_BACK_HOLD_MS;
    assert_eq!(
        p.plan(None, first_back, &all_shed).back,
        Some(1),
        "oldest first"
    );
    let one_back = [&[ring(1, 0, false)], &all_shed[1..]].concat();
    assert_eq!(
        p.plan(None, first_back + 1, &one_back).back,
        None,
        "one at a time"
    );
    assert_eq!(p.back_due(&one_back), Some(first_back + RING_BACK_GAP_MS));
    assert_eq!(
        p.plan(None, first_back + RING_BACK_GAP_MS, &one_back).back,
        Some(2)
    );

    // Pressure again while rings wait: the wait is cancelled, and starts over at the next
    // NORMAL.
    let two_back = [&[ring(1, 0, false), ring(2, 0, false)], &all_shed[2..]].concat();
    let warned = first_back + 5_000;
    assert_eq!(
        p.plan(Some(PressureLevel::Warn), warned, &two_back).back,
        None
    );
    assert_eq!(
        p.back_due(&two_back),
        None,
        "nothing comes back under pressure"
    );
    let again = warned + 1_000;
    p.plan(Some(PressureLevel::Normal), again, &two_back);
    assert_eq!(p.back_due(&two_back), Some(again + RING_BACK_HOLD_MS));
    assert_eq!(
        p.plan(None, again + RING_BACK_HOLD_MS, &two_back).back,
        Some(3)
    );
    let none_shed = [ring(1, 0, false)];
    assert_eq!(p.back_due(&none_shed), None, "nothing waits");
}

/// The worker on its own thread sheds on a reported level and answers once the regions it
/// shed were replaced; after a NORMAL it brings the ring back when the hold has passed, and
/// the ring catches the history up again. Its clocks, the journal's and the shed policy's,
/// are held and moved on here, never slept; each wait is a one-minute hang detector, never
/// a latency budget.
#[test]
fn the_worker_sheds_on_pressure_and_brings_the_ring_back_after_the_hold() {
    const HANG: std::time::Duration = aterm_spec::wait::MUST_HAPPEN;
    struct Stop(Arc<HistoryWorker>);
    impl Drop for Stop {
        fn drop(&mut self) {
            self.0.stop_ring_worker();
        }
    }
    let worker = Stop(HistoryWorker::start_held_ring_worker().expect("the worker's thread"));
    let rig = Rig::new(8, 80);
    for i in 0..2_000 {
        rig.slice(format!("{}\r\n", noisy_line(i, 76)).as_bytes());
    }
    // The start's nudged tick exports the whole history.
    rig.journal.start_export_with(&worker.0, &rig.term, false);
    let gap = std::time::Duration::from_millis(5);
    let caught_up =
        aterm_spec::wait::within(HANG, gap, || (rig.journal.ring_lag() == 0).then_some(()));
    assert!(caught_up.is_some(), "the worker caught the ring up");
    let before = rig.journal.resident_bytes();

    let apply = |level| {
        let (tx, rx) = std::sync::mpsc::channel();
        worker.0.note_shed_pressure(level, Some(tx));
        rx.recv_timeout(HANG).expect("the worker applied the level");
    };
    apply(PressureLevel::Critical);
    assert!(rig.journal.ring_shed());
    assert_eq!(
        rig.journal.regions_replaced(),
        1,
        "replaced before the answer"
    );
    assert!(rig.journal.resident_bytes() < before);
    let (level, journals, shed, back_in) = worker.0.shed_view();
    assert_eq!(
        (level, journals, shed, back_in),
        (Some(PressureLevel::Critical), 1, 1, None)
    );

    apply(PressureLevel::Normal);
    assert!(rig.journal.ring_shed(), "the hold first");
    let (level, _, _, back_in) = worker.0.shed_view();
    assert_eq!(level, Some(PressureLevel::Normal));
    assert_eq!(
        back_in,
        Some(RING_BACK_HOLD_MS),
        "back once the hold has passed"
    );
    // A pass a millisecond short of the hold (a repeated NORMAL keeps the first one's).
    worker.0.pass_shed_ms(RING_BACK_HOLD_MS - 1);
    apply(PressureLevel::Normal);
    assert!(rig.journal.ring_shed(), "not before the hold has passed");
    worker.0.pass_shed_ms(1);
    let back = aterm_spec::wait::within(HANG, gap, || {
        (!rig.journal.ring_shed() && rig.journal.ring_lag() == 0).then_some(())
    });
    assert!(back.is_some(), "the ring came back and caught up");
    assert!(rig.journal.ring_bytes() > 0);
}

/// A keyframe the parse thread captured for the region the shed then replaced is never
/// written into the fresh one: the shed's own keyframe, taken later, already holds the
/// engine that capture saw, and the fresh region's first keyframe stays its newest. The
/// capture here is made where the tail check alone would let it through: an idle session's
/// region that a shed had already replaced (its tail empty) captures a forced keyframe, and
/// a second shed lands before the write, so both regions' tails stand at 0.
#[test]
fn a_keyframe_captured_before_a_shed_is_never_written_after_it() {
    let rig = deep_rig();
    rig.journal.request_shed();
    rig.journal.export_tick(&rig.term);
    rig.journal.request_unshed();
    rig.export();
    assert!(
        rig.journal.ring_bytes() > 0,
        "premise: the ring re-exported"
    );
    let captured = {
        let t = crate::term_lock(&rig.term);
        rig.journal
            .lock()
            .capture(&t, Reason::Forced, None)
            .expect("the region takes it")
            .expect("ground")
    };
    assert_eq!(captured.tail_at, 0, "premise: an empty tail");
    rig.journal.request_shed();
    rig.journal.export_tick(&rig.term);
    assert_eq!(rig.journal.regions_replaced(), 2);
    let (keyframes, _) = rig.journal.counts();
    rig.journal.write(captured);
    assert_eq!(
        rig.journal.counts().0,
        keyframes,
        "the stale capture is dropped"
    );
    let restored = rig.recovered();
    assert_eq!(
        restored.keyframe.seq, 1,
        "the shed's keyframe is the fresh region's newest"
    );
    assert!(restored.exact, "{:?}", restored.losses);
}

/// The gather count is read under the actual shared swap lock. `swap_in` asks the
/// replacement's stable length before taking that lock, so this store can finish a
/// gather on the old region in that exact gap without a thread, sleep, or new hook.
#[test]
fn a_region_swap_keeps_a_gather_completed_before_its_lock() {
    use aterm_restore::layout::{OFF_STREAM_GATHERED, PAGE};
    use aterm_restore::{RegionStore, RegionView, StoreError, VecStore};
    use std::cell::RefCell;
    use std::sync::atomic::AtomicUsize;

    struct AtLength {
        inner: VecStore,
        before_length: RefCell<Option<Box<dyn FnOnce()>>>,
    }
    impl AtLength {
        fn new(before_length: Option<Box<dyn FnOnce()>>) -> Self {
            Self {
                inner: VecStore::new(PAGE),
                before_length: RefCell::new(before_length),
            }
        }
    }
    impl RegionView for AtLength {
        fn len(&self) -> usize {
            let before = self.before_length.borrow_mut().take();
            if let Some(before) = before {
                before();
            }
            self.inner.len()
        }
        fn read(&self, at: usize, out: &mut [u8]) -> Result<(), StoreError> {
            self.inner.read(at, out)
        }
        fn load_u64_acquire(&self, at: usize) -> Result<u64, StoreError> {
            self.inner.load_u64_acquire(at)
        }
    }
    impl RegionStore for AtLength {
        fn write(&self, at: usize, bytes: &[u8]) -> Result<(), StoreError> {
            self.inner.write(at, bytes)
        }
        fn store_u64_release(&self, at: usize, value: u64) -> Result<(), StoreError> {
            self.inner.store_u64_release(at, value)
        }
    }

    for count in [0, 37] {
        let shared = Shared::new(AtLength::new(None));
        let gathered = AtomicU64::new(count);
        store_gathered(&shared, count).expect("ordinary gathered word");
        let old = swap_gathered_region(&shared, AtLength::new(None), &gathered)
            .unwrap_or_else(|_| panic!("same stable length"));
        assert_eq!(old.load_u64_acquire(OFF_STREAM_GATHERED), Ok(count));
        assert_eq!(shared.load_u64_acquire(OFF_STREAM_GATHERED), Ok(count));
        let after = gathered.fetch_add(11, Ordering::Relaxed) + 11;
        store_gathered(&shared, after).expect("a gather after the swap");
        assert_eq!(shared.load_u64_acquire(OFF_STREAM_GATHERED), Ok(after));
        assert_eq!(old.load_u64_acquire(OFF_STREAM_GATHERED), Ok(count));
    }
    eprintln!("R6_GATHER_SEED_ORDINARY_CONTROLS_REACHED");

    let shared = Shared::new(AtLength::new(None));
    let gathered = Arc::new(AtomicU64::new(37));
    let calls = Arc::new(AtomicUsize::new(0));
    store_gathered(&shared, 37).expect("old region's initial count");
    let before_lock = {
        let shared = shared.clone();
        let gathered = Arc::clone(&gathered);
        let calls = Arc::clone(&calls);
        Box::new(move || {
            assert_eq!(calls.fetch_add(1, Ordering::Relaxed), 0, "one boundary");
            let total = gathered.fetch_add(23, Ordering::Relaxed) + 23;
            store_gathered(&shared, total).expect("gather completed on the old region");
        }) as Box<dyn FnOnce()>
    };
    let old = swap_gathered_region(&shared, AtLength::new(Some(before_lock)), &gathered)
        .unwrap_or_else(|_| panic!("same stable length"));
    assert_eq!(
        calls.load(Ordering::Relaxed),
        1,
        "the gap was exercised once"
    );
    assert_eq!(
        gathered.load(Ordering::Relaxed),
        60,
        "the batch was gathered"
    );
    assert_eq!(
        old.load_u64_acquire(OFF_STREAM_GATHERED),
        Ok(60),
        "the batch's count landed before the swap's lock"
    );
    let observed = shared.load_u64_acquire(OFF_STREAM_GATHERED);
    assert_eq!(
        observed,
        Ok(60),
        "R6_GATHERED_SEED_LOST_COMPLETED_BATCH: {observed:?}"
    );
}

/// A privacy clear zeroes the history's bytes, but the pages it wrote remain the
/// region's until it is unmapped. CRITICAL must replace that region too; an untouched
/// ring is still shed in place. This uses the real journal and mapping synchronously,
/// with no worker thread or injected pressure source.
#[test]
fn a_privacy_cleared_history_ring_still_releases_its_pages_when_shed() {
    let untouched = Rig::new(6, 40);
    assert_eq!(untouched.journal.ring_bytes(), 0);
    untouched.journal.request_shed();
    untouched.journal.export_tick(&untouched.term);
    assert!(untouched.journal.ring_shed());
    assert_eq!(untouched.journal.regions_replaced(), 0);

    let rig = deep_rig();
    let geometry = Geometry::v1();
    assert!(rig.journal.ring_bytes() > 128 * 1024, "a populated ring");
    rig.slice(b"\x1b[3J");
    rig.export();
    assert_eq!(
        rig.journal
            .ring
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .as_ref()
            .expect("the ring")
            .writer
            .touched(),
        0,
        "the privacy clear reset the logical history writer"
    );
    assert!(
        rig.journal
            .store
            .with(|region| region.touches(geometry.hist_off, geometry.hist_cap_phys)),
        "zeroing history did not unmap its written pages"
    );
    let before = rig.journal.resident_bytes();
    let screen = rig.visible();
    let sizes = [RingSize {
        id: rig.journal.id,
        bytes: rig.journal.ring_bytes(),
        shed: rig.journal.ring_shed(),
    }];
    let plan = ShedPolicy::default().plan(Some(PressureLevel::Critical), 0, &sizes);
    assert_eq!(
        plan.shed,
        vec![rig.journal.id],
        "CRITICAL selected the ring"
    );
    rig.journal.request_shed();
    rig.journal.export_tick(&rig.term);
    assert!(rig.journal.broken().is_none(), "the journal kept recording");
    assert!(rig.journal.ring_shed());
    assert_eq!(rig.visible(), screen, "shedding did not change the engine");
    let replaced = rig.journal.regions_replaced() == 1;
    let released = rig.journal.resident_bytes() < before;
    let fresh_history = !rig
        .journal
        .store
        .with(|region| region.touches(geometry.hist_off, geometry.hist_cap_phys));
    assert!(
        replaced && released && fresh_history,
        "SHED_KEPT_PRIVACY_CLEARED_HISTORY_PAGES: replaced={replaced} released={released} fresh_history={fresh_history}"
    );
}

/// A REPLACED REGION'S CUSTODY OUTLASTS A FULL LINK QUEUE (R6). The shed hands the keeper
/// the fresh region's only read-only descriptor; when the keeper link's queue is full (a
/// keeper slow to read, under the very pressure being shed) the descriptor comes back
/// (`keeper_link::CompanionOffer::Busy`) and the session keeps it, offering it again a
/// tick later, rather than leave the keeper on the superseded region for good. Once the
/// keeper has read, the next offer reaches it, and what it holds is the fresh region: a
/// crash then restores the session's own screen exactly, not the shed's stale one.
/// NEGATIVE CONTROL: the superseded region the keeper held until then restores only as a
/// pulsed screen (`restore_why=superseded`); with the old `let _ = link.enqueue(op)` in
/// `keeper_link::replace_companion`, the custody is not kept and no COMPANION for the fresh
/// region ever reaches the keeper (checked by hand against this test).
#[test]
fn a_full_link_queue_keeps_the_fresh_regions_custody_and_offers_it_again() {
    use aterm_keeper::client::LinkOp;
    const RDEV: u64 = 0x5eed;
    let rig = deep_rig();
    let link = Arc::new(crate::keeper_link::TestLink::stalled(rig.journal.id, RDEV));
    rig.journal.link_custody_to(Arc::clone(&link));
    let fresh_companions = |ops: Vec<LinkOp>| -> Vec<(OwnedFd, u64)> {
        ops.into_iter()
            .filter_map(|op| match op {
                LinkOp::Companion {
                    rdev, region, len, ..
                } if rdev == RDEV => Some((region, len)),
                _ => None,
            })
            .collect()
    };
    link.fill();
    rig.journal.request_shed();
    rig.journal.export_tick(&rig.term);
    assert_eq!(rig.journal.regions_replaced(), 1, "the region was replaced");
    assert!(
        rig.journal.custody_owed(),
        "the custody a full queue refused is kept, not dropped"
    );
    let now = rig.journal.ring_now_ms();
    assert_eq!(
        rig.journal.ring_due(now),
        Some(now + TICK_MS),
        "offered again a tick later"
    );
    assert!(
        fresh_companions(link.drain()).is_empty(),
        "the keeper read only what filled the queue"
    );

    rig.journal.pass_ms(TICK_MS);
    rig.journal.export_tick(&rig.term);
    assert!(
        rig.journal.custody_owed(),
        "queue admission still owes a read"
    );
    assert!(
        rig.journal.ring_due(rig.journal.ring_now_ms()).is_some(),
        "queue admission still owes a keeper read"
    );
    let mut handed = fresh_companions(link.drain());
    rig.journal.export_tick(&rig.term);
    assert!(
        !rig.journal.custody_owed(),
        "the captured read settled custody"
    );
    assert_eq!(
        rig.journal.ring_due(rig.journal.ring_now_ms()),
        None,
        "the captured keeper read settled the predecessor"
    );
    assert_eq!(handed.len(), 1, "one COMPANION, the fresh region's");
    let (region, len) = handed.pop().expect("one");
    let restore = restore_custody(&region, len, rig.cfg.as_ref()).expect("a screen");
    assert_eq!(restore.report.rung, Rung::Exact, "{:?}", restore.report);
    let (adopted, _) = adopt_and_import(restore.checkpoint, restore.history, None);
    assert_eq!(
        visible(&crate::term_lock(&adopted)),
        rig.visible(),
        "the session's own screen"
    );
    let stale = rig.restore(0).expect("the superseded region's screen");
    assert_eq!(
        (stale.report.rung, stale.report.why),
        (Rung::Screen, "superseded"),
        "control: the region the keeper held meanwhile"
    );
}

/// A CLEAR WHILE A REPLACED REGION'S CUSTODY IS OWED ERASES THE REGION THE KEEPER STILL
/// HOLDS (R6 review; plan §5 claim 4). With the keeper link's queue full, the shed's
/// replacement lands but its COMPANION cannot be queued, so the keeper keeps the old region
/// (its whole history ring, its keyframes and their bridges) until the queue takes the
/// fresh one's custody: as long as the keeper stays slow, the very case the custody retry
/// is for. The session keeps the old region mapped meanwhile (`journal_ring=shedding`: it
/// is still in memory), and a clear made then (ED3) erases it at once, so a crash before
/// the keeper reads restores none of the cleared lines: the erased region reads as no
/// region (the META rung). Once the queue takes the custody, the old region is let go
/// (`journal_ring=shed`) and the keeper is told the fresh one.
/// NEGATIVE CONTROL: before the clear the old region holds every token and restores the
/// shed's screen (`restore_why=superseded`) with them; a session that let the old region
/// go at the replacement (the code before this change) leaves every cleared token in it.
#[test]
fn a_clear_while_the_custody_is_owed_erases_the_region_the_keeper_holds() {
    use aterm_keeper::client::LinkOp;
    const RDEV: u64 = 0x5eed;
    let rig = Rig::new(4, 30);
    let found = exported_tokens(&rig, 3_000);
    let slot = JournalSlot::default();
    slot.install(Arc::clone(&rig.journal));
    let ring_word = || {
        slot.status_words()
            .split(' ')
            .find_map(|word| word.strip_prefix("journal_ring="))
            .expect("journal_ring=")
            .to_string()
    };
    let survivors = || {
        let held = custody_bytes(&rig.custody, rig.len);
        let windows = windows_of(&held);
        found
            .iter()
            .filter(|token| windows.contains(needle(token)))
            .count()
    };
    let link = Arc::new(crate::keeper_link::TestLink::stalled(rig.journal.id, RDEV));
    rig.journal.link_custody_to(Arc::clone(&link));
    link.fill();
    rig.journal.request_shed();
    rig.journal.export_tick(&rig.term);
    // The owed custody's own retry is a tick on; the clear must be due before it, at once.
    let owed_at = rig.journal.ring_now_ms();
    assert_eq!(rig.journal.regions_replaced(), 1, "premise: replaced");
    assert!(rig.journal.custody_owed(), "premise: the custody is owed");
    assert_eq!(
        ring_word(),
        "shedding",
        "the old region is still in memory, held for the keeper"
    );
    assert_eq!(
        survivors(),
        found.len(),
        "control: the keeper's region holds the history"
    );
    let stale = rig
        .restore(0)
        .expect("control: the superseded region's screen");
    assert_eq!(
        (stale.report.rung, stale.report.why),
        (Rung::Screen, "superseded")
    );

    assert_eq!(
        rig.journal.ring_due(owed_at),
        Some(owed_at + TICK_MS),
        "control: before the clear the worker is due only for the custody's retry, a tick on"
    );
    rig.slice(b"\x1b[3Jcleared\r\n");
    assert_eq!(
        rig.journal.ring_due(owed_at),
        Some(owed_at),
        "the clear wakes the worker"
    );
    rig.journal.export_tick(&rig.term);
    assert_eq!(
        survivors(),
        0,
        "lines cleared while the custody was owed survive in the region the keeper holds"
    );
    let erased = match rig.restore(0) {
        Ok(restore) => panic!("the erased region restores no screen: {:?}", restore.report),
        Err(report) => report,
    };
    assert_eq!(erased.rung, Rung::Meta, "{erased:?}");
    assert!(rig.journal.custody_owed(), "the queue is still full");
    assert_eq!(ring_word(), "shedding");

    // The keeper reads; the next offer lands, and the old region is let go.
    let read: Vec<LinkOp> = link.drain();
    assert!(
        !read
            .iter()
            .any(|op| matches!(op, LinkOp::Companion { rdev: RDEV, .. })),
        "nothing for the session reached the keeper meanwhile"
    );
    rig.journal.pass_ms(TICK_MS);
    rig.journal.export_tick(&rig.term);
    assert!(
        rig.journal.custody_owed(),
        "queue admission still owes a read"
    );
    assert_eq!(
        ring_word(),
        "shedding",
        "the queue has not confirmed a read"
    );
    let read = link.drain();
    rig.journal.export_tick(&rig.term);
    assert!(
        !rig.journal.custody_owed(),
        "the captured read settled custody"
    );
    assert_eq!(
        ring_word(),
        "shed",
        "the captured keeper read released the predecessor"
    );
    assert_eq!(
        read.iter()
            .filter(|op| matches!(op, LinkOp::Companion { rdev: RDEV, .. }))
            .count(),
        1,
        "the keeper is told the fresh region"
    );
}

/// A SECOND SHED WHILE A CUSTODY IS STILL OWED KEEPS THE REGION THE KEEPER HOLDS (R6
/// review). The pressure eases, the ring exports into the fresh region, and pressure comes
/// back before the keeper link's queue has taken that region's custody: the second
/// replacement's custody is owed too, and the keeper still holds the FIRST region, never
/// told of the one between. So that first region is the one kept for a clear to erase;
/// the one between goes with its custody. Once the queue takes the custody, the keeper is
/// told the newest region, which restores the session's own screen exactly.
/// NEGATIVE CONTROL: before the clear the first region holds every token; keeping the
/// region between instead (the region this replacement superseded) would leave every
/// cleared token in the one the keeper holds.
#[test]
fn a_second_shed_while_the_custody_is_owed_keeps_the_region_the_keeper_holds() {
    use aterm_keeper::client::LinkOp;
    const RDEV: u64 = 0x5eed;
    let rig = Rig::new(4, 30);
    let found = exported_tokens(&rig, 3_000);
    let survivors = || {
        let held = custody_bytes(&rig.custody, rig.len);
        let windows = windows_of(&held);
        found
            .iter()
            .filter(|token| windows.contains(needle(token)))
            .count()
    };
    let link = Arc::new(crate::keeper_link::TestLink::stalled(rig.journal.id, RDEV));
    rig.journal.link_custody_to(Arc::clone(&link));
    link.fill();
    rig.journal.request_shed();
    rig.journal.export_tick(&rig.term);
    assert!(
        rig.journal.custody_owed(),
        "premise: the first custody is owed"
    );
    rig.journal.request_unshed();
    rig.export();
    assert!(
        rig.journal.ring_bytes() > 0,
        "premise: the ring wrote the fresh region"
    );
    rig.journal.request_shed();
    rig.journal.export_tick(&rig.term);
    assert_eq!(rig.journal.regions_replaced(), 2, "premise: replaced again");
    assert!(rig.journal.custody_owed(), "premise: owed again");
    assert_eq!(
        survivors(),
        found.len(),
        "control: the keeper's region holds the history"
    );

    rig.slice(b"\x1b[3Jcleared\r\n");
    rig.journal.export_tick(&rig.term);
    assert_eq!(
        survivors(),
        0,
        "the clear erased the region the keeper holds, not the one between"
    );

    let _ = link.drain();
    rig.journal.pass_ms(TICK_MS);
    rig.journal.export_tick(&rig.term);
    assert!(
        rig.journal.custody_owed(),
        "queue admission still owes a read"
    );
    let mut handed: Vec<(OwnedFd, u64)> = link
        .drain()
        .into_iter()
        .filter_map(|op| match op {
            LinkOp::Companion {
                rdev, region, len, ..
            } if rdev == RDEV => Some((region, len)),
            _ => None,
        })
        .collect();
    rig.journal.export_tick(&rig.term);
    assert!(
        !rig.journal.custody_owed(),
        "the captured read settled custody"
    );
    assert_eq!(handed.len(), 1, "one COMPANION, the newest region's");
    let (region, len) = handed.pop().expect("one");
    let restore = restore_custody(&region, len, rig.cfg.as_ref()).expect("a screen");
    assert_eq!(restore.report.rung, Rung::Exact, "{:?}", restore.report);
}

/// How many of `tokens` a byte search still finds in the region `custody` names.
fn tokens_in(custody: &OwnedFd, len: u64, tokens: &[String]) -> usize {
    let held = custody_bytes(custody, len);
    let windows = windows_of(&held);
    tokens
        .iter()
        .filter(|token| windows.contains(needle(token)))
        .count()
}

/// A CLEAR MADE AFTER THE COMPANION IS QUEUED BUT BEFORE THE KEEPER READS IT ERASES THE
/// REGION THE KEEPER STILL HOLDS (R9 (b)). The queue takes the shed's COMPANION at once
/// (`CompanionOffer::Handed`), but taking it is not the keeper reading it: until it does,
/// the keeper holds the old region, whose ring and keyframes hold the history. The link
/// reports the read (`aterm_keeper::client::CompanionReceipt`, confirmed by the SYNC the
/// worker writes behind the COMPANION), the session keeps the old region until then
/// (`journal_ring=shedding`), and a clear made meanwhile (ED3) erases it: a crash before
/// the read restores none of the cleared lines (the META rung). Once the keeper has read,
/// the old region is let go (`journal_ring=shed`), and what the keeper holds is the fresh
/// region, which restores exactly.
/// NEGATIVE CONTROL: the same clear made after the old region was let go leaves every
/// token in it. A link that let it go at the hand-off (the queue's admission, as R6's did)
/// left exactly that in the region a keeper that had not read yet still held.
#[test]
fn a_clear_after_the_companion_is_queued_but_unread_erases_the_region_the_keeper_holds() {
    use aterm_keeper::client::LinkOp;
    const RDEV: u64 = 0x5eed;
    let fresh_companion = |ops: Vec<LinkOp>| -> Option<(OwnedFd, u64)> {
        ops.into_iter().find_map(|op| match op {
            LinkOp::Companion {
                rdev: RDEV,
                region,
                len,
                ..
            } => Some((region, len)),
            _ => None,
        })
    };
    // The keeper reads before (`read_first`) or after the clear.
    let run = |read_first: bool| -> (usize, usize, Rung) {
        let rig = Rig::new(4, 30);
        let found = exported_tokens(&rig, 3_000);
        let slot = JournalSlot::default();
        slot.install(Arc::clone(&rig.journal));
        let ring_word = || {
            slot.status_words()
                .split(' ')
                .find_map(|word| word.strip_prefix("journal_ring="))
                .expect("journal_ring=")
                .to_string()
        };
        let link = Arc::new(crate::keeper_link::TestLink::stalled(rig.journal.id, RDEV));
        rig.journal.link_custody_to(Arc::clone(&link));
        rig.journal.request_shed();
        rig.journal.export_tick(&rig.term);
        assert_eq!(rig.journal.regions_replaced(), 1, "premise: replaced");
        assert!(
            rig.journal.custody_owed(),
            "a queued COMPANION is not a read one"
        );
        assert_eq!(
            ring_word(),
            "shedding",
            "the old region is kept for the keeper"
        );
        assert_eq!(
            tokens_in(&rig.custody, rig.len, &found),
            found.len(),
            "control: the keeper's region holds the history"
        );
        let mut read = None;
        if read_first {
            read = fresh_companion(link.drain());
            rig.journal.export_tick(&rig.term);
            assert!(
                !rig.journal.custody_owed(),
                "the read let the old region go"
            );
            assert_eq!(ring_word(), "shed");
        }
        rig.slice(b"\x1b[3Jcleared\r\n");
        rig.journal.export_tick(&rig.term);
        let left = tokens_in(&rig.custody, rig.len, &found);
        let rung = match rig.restore(0) {
            Ok(restore) => restore.report.rung,
            Err(report) => report.rung,
        };
        if read.is_none() {
            assert_eq!(ring_word(), "shedding", "the keeper has still not read");
            read = fresh_companion(link.drain());
            rig.journal.export_tick(&rig.term);
            assert!(
                !rig.journal.custody_owed(),
                "the read let the old region go"
            );
            assert_eq!(ring_word(), "shed");
        }
        let (region, len) = read.expect("the keeper read the fresh region's COMPANION");
        let restore = restore_custody(&region, len, rig.cfg.as_ref()).expect("a screen");
        assert_eq!(restore.report.rung, Rung::Exact, "{:?}", restore.report);
        assert_eq!(
            tokens_in(&region, len, &found),
            0,
            "the fresh region never held the shed history"
        );
        (left, found.len(), rung)
    };

    let (left, _, rung) = run(false);
    assert_eq!(
        left, 0,
        "lines cleared before the keeper read the COMPANION survive in the region it holds"
    );
    assert_eq!(rung, Rung::Meta, "the erased region restores no screen");

    let (left, all, rung) = run(true);
    assert_eq!(
        left, all,
        "control: a region let go before the clear keeps every cleared line"
    );
    assert_eq!(rung, Rung::Screen, "control: the superseded screen");
}

/// A CLEAR WHOSE KEYFRAME A REPLACEMENT SKIPPED LEAVES NOTHING IN THE REGION THE KEEPER
/// HOLDS (R9 (c)). The parse thread records an ED3 into the region and captures the
/// keyframe the clear forces under the term lock, and writes it, and settles the clear
/// (`aterm_restore::JournalWriter::settle_clear`), after the lock. A shed replacing the
/// region in between keyframes the engine into the fresh region itself, so that capture
/// is skipped (`Writer::region_gen` moved), and the old region's settle with it: the
/// region the keeper still holds until it reads the replacement's COMPANION keeps the
/// pre-clear keyframes (the quiet one's bridge of history, the older slot's) and the
/// tail's pre-clear records, every line the person cleared. The replacement erases it,
/// as a clear made after it does (`aterm_restore::erase_region`): a crash before the read
/// restores none of them (the META rung), and the fresh region holds none either.
/// NEGATIVE CONTROL (in the tree): the region holds every cleared line before the clear,
/// and still holds every one once the clear is recorded and its keyframe captured, before
/// the replacement: only the clear's settle, whose write the replacement makes skip, or the
/// replacement's erase takes them out. The code without the erase failed this test at
/// `left.is_empty()`; that run was made once, before the fix, and is not kept.
#[test]
fn a_clear_whose_keyframe_a_replacement_skipped_leaves_nothing_in_the_region_the_keeper_holds() {
    use aterm_keeper::client::LinkOp;
    const RDEV: u64 = 0x5eed;
    let rig = Rig::new(4, 30);
    let mut cleared = exported_tokens(&rig, 3_000);
    // Lines past the quiet keyframe, in the tail as BYTES records; the newest few stay on
    // the screen, which an ED3 does not clear.
    let tail: Vec<String> = (10_000..10_040).map(entropic).collect();
    let mut bytes = String::new();
    for token in &tail {
        bytes.push_str(token);
        bytes.push_str("\r\n");
    }
    rig.slice(bytes.as_bytes());
    cleared.extend(tail[..tail.len() - 8].iter().cloned());
    assert_eq!(
        tokens_in(&rig.custody, rig.len, &cleared),
        cleared.len(),
        "control: the region holds every line before the clear"
    );
    let link = Arc::new(crate::keeper_link::TestLink::stalled(rig.journal.id, RDEV));
    rig.journal.link_custody_to(Arc::clone(&link));
    rig.journal.request_shed();

    // The parse thread's hold: the clear recorded and its keyframe captured, not written.
    let clear = b"\x1b[3Jcleared\r\n";
    rig.journal.gathered(clear.len());
    let captured = {
        let mut t = crate::term_lock(&rig.term);
        let mut pre = rig.journal.pre(&t);
        t.process(clear);
        let _ = t.take_response();
        rig.journal.record(&t, &mut pre, clear, false);
        rig.journal.decide(&t, &pre)
    }
    .expect("premise: the clear forces a keyframe");
    assert_eq!(
        tokens_in(&rig.custody, rig.len, &cleared),
        cleared.len(),
        "control: recorded and captured, the clear has taken no line out of the region yet"
    );
    // The worker's tick between the capture and the write: the shed replaces the region.
    rig.journal.export_tick(&rig.term);
    assert_eq!(rig.journal.regions_replaced(), 1, "premise: replaced");
    assert!(
        rig.journal.custody_owed(),
        "premise: the keeper has not read"
    );
    rig.journal.write(captured);
    assert!(rig.journal.broken().is_none(), "the journal kept recording");

    let left: Vec<&String> = {
        let held = custody_bytes(&rig.custody, rig.len);
        let windows = windows_of(&held);
        cleared
            .iter()
            .filter(|token| windows.contains(needle(token)))
            .collect()
    };
    assert!(
        left.is_empty(),
        "{} of {} cleared lines survive in the region the keeper holds: {:?}",
        left.len(),
        cleared.len(),
        &left[..left.len().min(4)]
    );
    match rig.restore(0) {
        Ok(restore) => panic!("the erased region restores no screen: {:?}", restore.report),
        Err(report) => assert_eq!(report.rung, Rung::Meta, "{report:?}"),
    }
    let (region, len) = link
        .drain()
        .into_iter()
        .find_map(|op| match op {
            LinkOp::Companion {
                rdev: RDEV,
                region,
                len,
                ..
            } => Some((region, len)),
            _ => None,
        })
        .expect("the fresh region's COMPANION");
    assert_eq!(
        tokens_in(&region, len, &cleared),
        0,
        "the fresh region holds no cleared line"
    );
    let restore = restore_custody(&region, len, rig.cfg.as_ref()).expect("a screen");
    assert_eq!(restore.report.rung, Rung::Exact, "{:?}", restore.report);
    let (adopted, _) = adopt_and_import(restore.checkpoint, restore.history, None);
    assert_eq!(
        visible(&crate::term_lock(&adopted)),
        rig.visible(),
        "the session's own screen"
    );
}

/// A LEVEL REPORTED BEFORE THE JOURNAL WORKER STARTS IS WHERE IT BEGINS (R6 review). The
/// worker starts with a window's first export; a CRITICAL the OS reported before it (or
/// with the keeper off, before any session journaled) used to be dropped, so `pressure
/// level=` said `-` and the first rings were not shed although the system was critical.
/// The level is kept whether a worker runs or not, and a worker that starts after it
/// begins from it, told once. NEGATIVE CONTROL: a worker started after the same report but
/// never told it (the old path) exports a session's ring as if no pressure had been
/// reported.
#[test]
fn a_pressure_level_reported_before_the_worker_starts_is_where_it_begins() {
    const HANG: std::time::Duration = aterm_spec::wait::MUST_HAPPEN;
    struct Stop(Arc<HistoryWorker>);
    impl Drop for Stop {
        fn drop(&mut self) {
            self.0.stop_ring_worker();
        }
    }
    let gap = std::time::Duration::from_millis(5);
    let grow = |rig: &Rig| {
        for i in 0..200 {
            rig.slice(format!("{}\r\n", noisy_line(i, 76)).as_bytes());
        }
    };
    let cell: PressureCell = std::sync::Mutex::new((None, false));
    let (tx, rx) = std::sync::mpsc::channel();
    note_pressure_in(&cell, || None, PressureLevel::Critical, Some(tx));
    rx.recv_timeout(HANG).expect("answered at once: no worker");
    assert_eq!(
        *cell.lock().expect("the cell"),
        (Some(PressureLevel::Critical), false),
        "kept, untold"
    );

    // Control: a worker that starts and is never told runs its passes at no level.
    let control = Stop(HistoryWorker::start_held_ring_worker().expect("a worker"));
    let unshed = Rig::new(8, 80);
    grow(&unshed);
    unshed
        .journal
        .start_export_with(&control.0, &unshed.term, false);
    let exported =
        aterm_spec::wait::within(HANG, gap, || (unshed.journal.ring_lag() == 0).then_some(()));
    assert!(exported.is_some(), "the control's worker exported the ring");
    assert!(!unshed.journal.ring_shed(), "control: nothing shed");
    assert_eq!(control.0.shed_view().0, None, "control: no level");

    let worker = Stop(HistoryWorker::start_held_ring_worker().expect("a worker"));
    seed_pressure_in(&cell, &worker.0);
    assert!(cell.lock().expect("the cell").1, "told");
    let rig = Rig::new(8, 80);
    rig.journal.start_export_with(&worker.0, &rig.term, false);
    grow(&rig);
    // The pass sheds the ring first and publishes its level at its end: both are waited for
    // (reading the level the moment the ring reads shed raced that pass's end).
    let shed = aterm_spec::wait::within(HANG, gap, || {
        (rig.journal.ring_shed() && worker.0.shed_view().0 == Some(PressureLevel::Critical))
            .then_some(())
    });
    assert!(
        shed.is_some(),
        "the ring a session opened under CRITICAL is shed, the worker at CRITICAL: {:?}",
        worker.0.shed_view().0
    );

    // Told once: seeding again does nothing; a later report goes to the worker itself.
    seed_pressure_in(&cell, &worker.0);
    let (tx, rx) = std::sync::mpsc::channel();
    let live = Arc::clone(&worker.0);
    note_pressure_in(&cell, || Some(live), PressureLevel::Normal, Some(tx));
    rx.recv_timeout(HANG).expect("the worker applied it");
    assert_eq!(worker.0.shed_view().0, Some(PressureLevel::Normal));
    assert_eq!(
        *cell.lock().expect("the cell"),
        (Some(PressureLevel::Normal), true)
    );
}

// The control slot (the plan's increment 7)

/// A turn record as `cmd_turn` records one.
fn turn(id: u64, text: &str) -> crate::turn_ledger::TurnRecord {
    crate::turn_ledger::TurnRecord {
        id,
        started_ms: id * 10,
        dur_ms: 5,
        submitted: true,
        status: "settled",
        text: text.into(),
        screen_hash: id,
        seq: id,
        arch: crate::turn_ledger::ArchMark {
            origin: 7,
            last: id,
        },
        by: crate::turn_ledger::TurnBy::Ctl,
        carried: false,
    }
}

/// A session's ledger, shared as `SessionCtx::turns` shares it.
fn ledger_of(
    records: Vec<crate::turn_ledger::TurnRecord>,
) -> Arc<Mutex<crate::turn_ledger::TurnLedger>> {
    let mut ledger = crate::turn_ledger::TurnLedger::default();
    for record in records {
        ledger.push(record);
    }
    Arc::new(Mutex::new(ledger))
}

impl Rig {
    /// The region restored as a recovering window restores it, the control slot's carry
    /// beside the screen.
    fn restoration(&self) -> Restoration {
        let mut used = 0u64;
        offered_restoration(Some(self.offered()), 0, self.cfg.as_ref(), &mut used)
    }

    /// A word of the live region's header, stored as a writer would.
    fn store_word(&self, at: usize, value: u64) {
        use aterm_restore::RegionStore as _;
        self.journal
            .store
            .store_u64_release(at, value)
            .expect("a header word");
    }
}

/// The turn ids a restoration's control carry holds (none without one).
fn carried_ids(restoration: &Restoration) -> Vec<u64> {
    restoration
        .control
        .as_ref()
        .map(|carry| carry.turns.iter().map(|t| t.id).collect())
        .unwrap_or_default()
}

/// THE TURN LEDGER ACROSS A CRASH (the plan's increment 7): a session's ledger rides its
/// region's control slot. An empty ledger writes nothing and costs the worker nothing; a
/// record is due at once, carried by the worker's tick, and read back by the restore with
/// its text, its archive mark and the writer's turn-id count, no archive and no log; the
/// next carry waits two seconds from the last; and the screen restores exactly beside it.
#[test]
fn a_turn_ledger_rides_the_control_slot_across_a_crash() {
    let rig = Rig::new(6, 40);
    rig.slice(b"$ ");
    let turns = ledger_of(Vec::new());
    rig.journal.carry_ledger(&turns);
    assert!(
        rig.restoration().control.is_none(),
        "an empty ledger writes nothing"
    );
    assert_eq!(
        rig.journal.control_due(rig.journal.ring_now_ms()),
        None,
        "nothing due while no record lands"
    );
    assert_eq!(rig.journal.ledger_lag(), 0);

    turns.lock().expect("ledger").push(turn(41, "echo one"));
    assert_eq!(rig.journal.ledger_lag(), 1, "a record the slot lacks");
    let now = rig.journal.ring_now_ms();
    assert_eq!(
        rig.journal.control_due(now),
        Some(now),
        "the first carry is due at once"
    );
    rig.journal.control_tick();
    assert_eq!(rig.journal.ledger_lag(), 0, "carried");
    assert_eq!(rig.journal.control_due(rig.journal.ring_now_ms()), None);
    let restoration = rig.restoration();
    assert_eq!(
        restoration.screen.as_ref().expect("a screen").report.rung,
        Rung::Exact,
        "the screen beside it"
    );
    let carry = restoration.control.expect("the ledger");
    assert_eq!(carry.turns.len(), 1);
    let record = &carry.turns[0];
    assert_eq!((record.id, &*record.text), (41, "echo one"));
    assert_eq!(record.arch.to_string(), "7:41");
    assert!(record.carried, "a carried record");
    assert!(
        carry.turn_ids <= crate::control::turn_ids_minted(),
        "the writer's turn-id count at the write rides it"
    );
    assert!(carry.archive.is_none() && carry.log.is_none());

    // The next carry waits two seconds from the last one, then carries both records.
    turns.lock().expect("ledger").push(turn(42, "echo two"));
    assert_eq!(
        rig.journal.control_due(now),
        Some(now + CONTROL_MS),
        "two seconds after the last carry"
    );
    rig.journal.pass_ms(CONTROL_MS);
    let now = rig.journal.ring_now_ms();
    assert_eq!(rig.journal.control_due(now), Some(now), "due now");
    rig.journal.control_tick();
    assert_eq!(carried_ids(&rig.restoration()), [41, 42]);
}

/// A TORN SLOT YIELDS NO CARRY, NEVER A WRONG ONE (the plan's increment 7 test), in the
/// window's restore: a slot whose commit word is 0 (a death inside the slot's write) or
/// names a record the slot does not hold is no ledger, and the screen restores exactly all
/// the same. The turn-id count beside the slot survives the tear: the recovery still
/// adopts the count, with an empty ledger that vouches for no id below it. The ledger
/// rides the META rung too: a region with no valid keyframe still carries it.
#[test]
fn a_torn_control_slot_carries_no_ledger_and_the_meta_rung_carries_a_whole_one() {
    use aterm_restore::layout::{OFF_CTL_COMMIT, kf_commit_word};
    let rig = Rig::new(6, 40);
    rig.slice(b"$ ");
    let turns = ledger_of(vec![turn(7, "make")]);
    rig.journal.carry_ledger(&turns);
    assert_eq!(carried_ids(&rig.restoration()), [7], "premise: carried");
    // Past any count a concurrent test's mint stores into this region.
    let count = 1 << 40;
    rig.journal.hold_turn_ids(count);
    let seq = {
        use aterm_restore::RegionView as _;
        rig.journal
            .store
            .load_u64_acquire(OFF_CTL_COMMIT)
            .expect("word")
    };
    assert_eq!(seq, 1);

    for (word, why) in [(0, "a write cut short"), (seq + 1, "another record's word")] {
        rig.store_word(OFF_CTL_COMMIT, word);
        let restoration = rig.restoration();
        assert!(restoration.control.is_none(), "{why}: no ledger");
        assert_eq!(
            restoration.screen.as_ref().expect("a screen").report.rung,
            Rung::Exact,
            "{why}: the screen all the same"
        );
        assert_eq!(restoration.turn_ids, count, "{why}: the count survives it");
        let carry = counted_carry(restoration.control, restoration.turn_ids)
            .unwrap_or_else(|| panic!("{why}: the count alone is carried"));
        assert!(carry.turns.is_empty() && carry.turn_ids == count, "{why}");
    }
    rig.store_word(OFF_CTL_COMMIT, seq);
    assert_eq!(carried_ids(&rig.restoration()), [7], "the slot whole again");

    // No keyframe: the META rung, and the ledger beside it.
    rig.store_word(kf_commit_word(0), 0);
    rig.store_word(kf_commit_word(1), 0);
    let restoration = rig.restoration();
    let report = restoration.screen.as_ref().err().expect("the META rung");
    assert_eq!((report.rung, report.why), (Rung::Meta, "no-keyframe"));
    assert_eq!(
        carried_ids(&restoration),
        [7],
        "the ledger rides the META rung"
    );
}

/// An ADOPTED session's ledger (an update's carry, a crash's) is written into its fresh
/// region at the spawn, before anything is told the region exists: a crash right after
/// the adoption still carries it, with its floor.
#[test]
fn an_adopted_ledger_is_carried_from_the_spawn() {
    let rig = Rig::new(6, 40);
    let turns = Arc::new(Mutex::new(crate::turn_ledger::TurnLedger::carried(
        vec![turn(5, "a"), turn(6, "b")],
        3,
    )));
    rig.journal.carry_ledger(&turns);
    assert_eq!(rig.journal.ledger_lag(), 0, "written at once");
    let restoration = rig.restoration();
    assert_eq!(carried_ids(&restoration), [5, 6]);
    assert!(restoration.control.expect("carried").unheld_below >= 3);
    // A ledger an update could not carry at all says only its floor: carried too.
    let floored = Rig::new(6, 40);
    let empty = Arc::new(Mutex::new(crate::turn_ledger::TurnLedger::carried(
        Vec::new(),
        900,
    )));
    floored.journal.carry_ledger(&empty);
    let carry = floored.restoration().control.expect("the floor rides");
    assert!(carry.turns.is_empty());
    assert_eq!(carry.unheld_below, 900);
}

/// The memory shed's fresh region is given the control slot's record and the turn-id count
/// before its custody goes anywhere: a crash restored from either region carries the same
/// ledger and a count as high, and the next mint's count lands in the fresh one.
#[test]
fn a_shed_region_s_replacement_carries_the_ledger() {
    let rig = deep_rig();
    let turns = ledger_of(vec![turn(11, "cargo test"), turn(12, "git status")]);
    rig.journal.carry_ledger(&turns);
    // Past any count a concurrent test's mint stores into this region.
    let count = 1 << 41;
    rig.journal.hold_turn_ids(count);
    let restoration = rig.restoration();
    assert_eq!(carried_ids(&restoration), [11, 12], "premise");
    assert_eq!(restoration.turn_ids, count, "premise");
    rig.journal.request_shed();
    rig.journal.export_tick(&rig.term);
    assert_eq!(rig.journal.regions_replaced(), 1);
    let (fresh, len) = rig
        .journal
        .take_custody()
        .expect("the fresh region's custody");
    let mut used = 0u64;
    let restoration = offered_restoration(
        Some(OfferedCompanion {
            region: fresh.try_clone().expect("dup"),
            kind: COMPANION_JOURNAL,
            len,
        }),
        0,
        rig.cfg.as_ref(),
        &mut used,
    );
    assert_eq!(
        carried_ids(&restoration),
        [11, 12],
        "the fresh region carries it"
    );
    assert_eq!(restoration.turn_ids, count, "and the count");
    // The worker goes on in the fresh region, and so does the count.
    turns.lock().expect("ledger").push(turn(13, "ls"));
    rig.journal.control_tick();
    rig.journal.hold_turn_ids(count + 1);
    let restoration = offered_restoration(
        Some(OfferedCompanion {
            region: fresh.try_clone().expect("dup"),
            kind: COMPANION_JOURNAL,
            len,
        }),
        0,
        rig.cfg.as_ref(),
        &mut used,
    );
    assert_eq!(carried_ids(&restoration), [11, 12, 13]);
    assert_eq!(restoration.turn_ids, count + 1);
}

/// The worker on its own thread carries a record with no tick asked for: the ledger's
/// watch wakes it (each wait a one-minute hang detector, never a latency budget). The record
/// lands once the worker has ticked the export's start and sleeps with nothing due, so only
/// that wake can bring the carry.
#[test]
fn the_worker_carries_a_record_the_ledger_takes() {
    const HANG: std::time::Duration = aterm_spec::wait::MUST_HAPPEN;
    struct Stop(Arc<HistoryWorker>);
    impl Drop for Stop {
        fn drop(&mut self) {
            self.0.stop_ring_worker();
        }
    }
    let gap = std::time::Duration::from_millis(5);
    let worker = Stop(HistoryWorker::start_held_ring_worker().expect("the worker's thread"));
    let rig = Rig::new(6, 40);
    let turns = ledger_of(Vec::new());
    rig.journal.carry_ledger(&turns);
    rig.journal.start_export_with(&worker.0, &rig.term, false);
    let started = aterm_spec::wait::within(HANG, gap, || {
        let ring = rig.journal.ring.lock().expect("the ring");
        ring.as_ref().and_then(|ring| ring.last_tick)
    });
    assert_eq!(started, Some(0), "the start's nudged tick");
    turns.lock().expect("ledger").push(turn(3, "date"));
    let carried =
        aterm_spec::wait::within(HANG, gap, || (rig.journal.ledger_lag() == 0).then_some(()));
    assert!(carried.is_some(), "the worker carried the record");
    assert_eq!(carried_ids(&rig.restoration()), [3]);
}

/// TURN IDS GO ON ABOVE EVERY CARRY: the count continues past the highest carried count or
/// record plus the update's reserve, and every carried ledger stops vouching for the ids
/// below where it goes on (a floor above it stands). No carry: nothing moves.
#[test]
fn a_recovery_continues_turn_ids_above_every_carry() {
    let carry =
        |turn_ids: u64, ids: &[u64], unheld_below: u64| crate::handoff_carry::ControlCarry {
            turns: ids.iter().map(|id| turn(*id, "t")).collect(),
            unheld_below,
            turn_ids,
            ..crate::handoff_carry::ControlCarry::default()
        };
    let mut a = carry(100, &[90, 99], 0);
    let mut b = carry(40, &[150], 7);
    let reserve = crate::control::TURN_ID_RESERVE;
    let mut far = carry(0, &[], 150 + reserve + 50);
    let ceiling = carried_turn_ceiling(&mut [&mut a, &mut b, &mut far]);
    assert_eq!(ceiling, Some(150 + reserve), "above the highest record");
    assert_eq!(a.unheld_below, 150 + reserve + 1);
    assert_eq!(b.unheld_below, 150 + reserve + 1);
    assert_eq!(
        far.unheld_below,
        150 + reserve + 50,
        "a floor above it stands"
    );
    assert_eq!(carried_turn_ceiling(&mut []), None);
    let mut top = carry(crate::handoff_carry::MAX_TURN_ID, &[], 0);
    assert_eq!(
        carried_turn_ceiling(&mut [&mut top]),
        Some(crate::handoff_carry::MAX_TURN_ID),
        "never past what a carry holds"
    );
}

/// THE TURN-ID COUNT RIDES EVERY REGION: a turn id minted anywhere in the process (a turn
/// begun in another session, one whose journal stopped, one since closed: none of them
/// moves this session's ledger, so no carry follows) is in this session's region before
/// the mint returns it, the control slot untouched; a restore reads it, and the recovery
/// goes on past it, the carried ledger vouching for no id up to there. NEGATIVE CONTROL:
/// without the mint's store (`control::mint_turn_id`'s `note_turn_ids`) the region holds
/// only the count at the journal's creation, and the minted id is above it.
#[test]
fn every_minted_turn_id_is_in_every_live_region_before_it_is_anyone_s() {
    let rig = Rig::new(6, 40);
    let turns = ledger_of(vec![turn(41, "echo one")]);
    rig.journal.carry_ledger(&turns);
    let id = crate::control::mint_turn_id_for_test();
    assert!(
        rig.journal.turn_ids_held() >= id,
        "the mint stored the count before returning the id"
    );
    let restoration = rig.restoration();
    assert!(restoration.turn_ids >= id, "the region holds it");
    assert_eq!(carried_ids(&restoration), [41], "a mint carries no ledger");
    let mut carry = counted_carry(restoration.control, restoration.turn_ids).expect("a carry");
    let ceiling = carried_turn_ceiling(&mut [&mut carry]).expect("a ceiling");
    assert!(
        ceiling >= id,
        "the count goes on past the id: {ceiling} < {id}"
    );
    assert!(
        carry.unheld_below > id,
        "and no carried floor vouches for it"
    );
}

/// THE REVIEW'S SCENARIO (a crash restore named an id again): a session's ledger carried
/// when the count was `c`; the window then minted ids up to `c + 5_000` in sessions whose
/// ledgers carried nothing and began a turn here under the last of them, which moves no
/// ledger, and died. Its slot's carry names `c`, beyond the update's reserve below the
/// last id; the region's count names the last id, and the recovery goes on past it: no id
/// the dead window used is minted again, and `history since=<it>` or a resumed
/// `since-turn=<it>` is told turns may be missing (the floor above it).
#[test]
fn a_crash_restore_continues_past_ids_no_carry_named() {
    let accepted = Arc::new(AtomicBool::new(false));
    let unplaced = Arc::new(AtomicBool::new(false));
    let one =
        crate::keeper_link::RecoveryCountAdmission::from_retained(vec![Arc::clone(&accepted)]);
    let all = crate::keeper_link::RecoveryCountAdmission::from_retained(vec![
        Arc::clone(&accepted),
        Arc::clone(&unplaced),
    ]);
    assert!(crate::keeper_link::RecoveryCountAdmission::default().ready());
    assert!(!one.ready() && !all.ready());
    let rig = Rig::with_recovery_count(6, 40, None, Some(Arc::clone(&accepted)));
    let initial = rig
        .first_receipt
        .as_ref()
        .expect("initial admission receipt");
    assert!(!initial.confirmed() && !one.ready());
    let turns = ledger_of(vec![turn(9, "make")]);
    rig.journal.carry_ledger(&turns);
    let carried = rig
        .restoration()
        .control
        .expect("premise: carried")
        .turn_ids;
    let reserve = crate::control::TURN_ID_RESERVE;
    // As the mints store it (`note_turn_ids`), no carry following: past any count a
    // concurrent test's mint stores into this region.
    let last = (1 << 42) + carried + reserve + 904;
    rig.journal.hold_turn_ids(last);
    turns.lock().expect("ledger").begin(last);
    assert_eq!(
        rig.journal.control_due(rig.journal.ring_now_ms()),
        None,
        "premise: a turn begun carries nothing"
    );
    let restoration = rig.restoration();
    let slot = restoration.control.as_ref().expect("the slot's carry");
    assert!(
        slot.turn_ids.saturating_add(reserve) < last,
        "premise: the slot's carry alone would have gone on below the last id"
    );
    let mut carry = counted_carry(restoration.control, restoration.turn_ids).expect("a carry");
    assert_eq!(carry.turn_ids, last, "the region's count");
    let ceiling = carried_turn_ceiling(&mut [&mut carry]).expect("a ceiling");
    assert_eq!(ceiling, last + reserve);
    assert!(carry.unheld_below > last, "the floor is past the last id");

    // A keeper that still holds the predecessor must observe later counts too,
    // both while the queue is full and after it takes a COMPANION but before SYNC.
    use aterm_keeper::client::LinkOp;
    const RDEV: u64 = 0xc017;
    for _ in 0..12 {
        rig.slice(b"a line to export\r\n");
    }
    rig.export();
    assert!(rig.journal.ring_bytes() > 0, "premise: a region to shed");
    let link = Arc::new(crate::keeper_link::TestLink::stalled(rig.journal.id, RDEV));
    rig.journal.link_custody_to(Arc::clone(&link));
    link.fill();
    rig.journal.request_shed();
    rig.journal.export_tick(&rig.term);
    assert!(rig.journal.custody_owed(), "the queue refused custody");
    let busy_count = last + reserve + 1;
    rig.journal.hold_turn_ids(busy_count);
    assert_eq!(
        rig.restoration().turn_ids,
        busy_count,
        "the old region remains counted"
    );
    assert!(
        link.drain()
            .iter()
            .all(|op| !matches!(op, LinkOp::Companion { .. }))
    );
    rig.journal.pass_ms(TICK_MS);
    rig.journal.export_tick(&rig.term);
    assert!(
        rig.journal.custody_owed(),
        "queue admission still owes a read"
    );
    let (fresh, len, receipt) = link
        .take_queued()
        .into_iter()
        .find_map(|op| match op {
            LinkOp::Companion {
                rdev: RDEV,
                region,
                len,
                receipt: Some(receipt),
            } => Some((region, len, receipt)),
            _ => None,
        })
        .expect("the queued replacement");
    assert!(!receipt.confirmed(), "queue admission is no read receipt");
    assert!(
        !one.ready() && !all.ready(),
        "a queued replacement grants no admission"
    );
    let unread_count = busy_count + reserve + 1;
    rig.journal.hold_turn_ids(unread_count);
    assert_eq!(
        rig.restoration().turn_ids,
        unread_count,
        "the unread predecessor stays counted"
    );

    // At most one predecessor can be queued and unread. No clock wait or rate
    // assumption turns the finite reserve into authority to drop another mapping.
    rig.journal.request_unshed();
    rig.export();
    rig.journal.request_shed();
    rig.journal.export_tick(&rig.term);
    assert_eq!(
        rig.journal.regions_replaced(),
        1,
        "another replacement awaits the read"
    );
    rig.journal.request_unshed();

    // Clearing privacy data must not clear the independent count. The old image
    // takes META, has no ledger, and carries ids minted after the clear as well.
    rig.slice(b"\x1b[3Jcleared\r\n");
    rig.journal.export_tick(&rig.term);
    let cleared_count = unread_count + reserve + 1;
    rig.journal.hold_turn_ids(cleared_count);
    rig.journal.hold_turn_ids(cleared_count - 1);
    let erased = rig.restoration();
    assert_eq!(erased.screen.err().expect("cleared image").rung, Rung::Meta);
    assert!(erased.control.is_none());
    assert_eq!(
        erased.turn_ids, cleared_count,
        "META preserves the count only"
    );
    let mut count_only = counted_carry(erased.control, erased.turn_ids).expect("count only");
    assert_eq!(
        carried_turn_ceiling(&mut [&mut count_only]),
        Some(cleared_count + reserve)
    );

    // The captured SYNC is the sole retirement authority. Later ids go to the
    // replacement, not the now-unreachable predecessor retained by this fixture.
    receipt.confirm_captured();
    assert!(
        !initial.confirmed(),
        "the first descriptor was superseded unread"
    );
    assert!(
        one.ready(),
        "the accepted replacement retires the same old authority"
    );
    drop(unplaced);
    assert!(
        !all.ready(),
        "a lost or unplaced ticket is not a retirement receipt"
    );
    rig.journal.export_tick(&rig.term);
    assert!(
        !rig.journal.custody_owed(),
        "the captured read settled custody"
    );
    rig.journal.hold_turn_ids(cleared_count + 1);
    assert_eq!(rig.restoration().turn_ids, cleared_count);
    let mut used = 0;
    let current = offered_restoration(
        Some(OfferedCompanion {
            region: fresh,
            kind: COMPANION_JOURNAL,
            len,
        }),
        0,
        rig.cfg.as_ref(),
        &mut used,
    );
    assert_eq!(current.turn_ids, cleared_count + 1);
}

/// A stored count only rises, whatever order the minting threads store in; a count read
/// before a higher one was stored is no lowering. The region keeps the highest.
#[test]
fn the_region_s_turn_id_count_only_rises() {
    let rig = Rig::new(6, 40);
    let high = 1 << 43;
    rig.journal.hold_turn_ids(high);
    rig.journal.hold_turn_ids(high - 1);
    rig.journal.hold_turn_ids(3);
    assert_eq!(rig.journal.turn_ids_held(), high);
    assert_eq!(rig.restoration().turn_ids, high);
}

/// A recovered session's carry is the slot's, its count raised to the region's; a region
/// with a count and no carry (a torn slot, a session that never took a turn) is adopted
/// with the count alone; neither: none. The recovery goes on above the counts as above the
/// carries' own.
#[test]
fn a_recovered_carry_takes_the_region_s_count() {
    let reserve = crate::control::TURN_ID_RESERVE;
    let slot = crate::handoff_carry::ControlCarry {
        turns: vec![turn(90, "t")],
        turn_ids: 100,
        ..crate::handoff_carry::ControlCarry::default()
    };
    let mut raised = counted_carry(Some(slot), 7_000).expect("the slot's carry");
    assert_eq!((raised.turns.len(), raised.turn_ids), (1, 7_000));
    let mut alone = counted_carry(None, 9_000).expect("the count alone");
    assert!(alone.turns.is_empty());
    assert_eq!((alone.turn_ids, alone.unheld_below), (9_000, 0));
    assert!(counted_carry(None, 0).is_none(), "no count, no carry: none");
    let lower = crate::handoff_carry::ControlCarry {
        turn_ids: 50,
        ..crate::handoff_carry::ControlCarry::default()
    };
    assert_eq!(
        counted_carry(Some(lower), 0).map(|c| c.turn_ids),
        Some(50),
        "a region with no count keeps the carry's"
    );
    let ceiling = carried_turn_ceiling(&mut [&mut raised, &mut alone]);
    assert_eq!(ceiling, Some(9_000 + reserve));
    assert_eq!(alone.unheld_below, 9_000 + reserve + 1);
    assert_eq!(raised.unheld_below, 9_000 + reserve + 1);
    assert_eq!(
        counted_carry(None, u64::MAX).map(|c| c.turn_ids),
        Some(crate::handoff_carry::MAX_TURN_ID),
        "never past what a carry holds"
    );
}

/// A LEDGER TOO LONG FOR THE SLOT IS SAID: a full ledger of long, varied prompts does not
/// fit the 64 KiB slot, so its carry sheds the oldest records, and `journal_ledger_lag=`
/// counts them: the slot does not hold them, and 0 would promise a crash restores every
/// turn the ledger holds. They stay counted while the ledger is that long.
#[test]
fn a_ledger_the_slot_cannot_hold_whole_is_counted_in_its_lag() {
    use crate::turn_ledger::LEDGER_CAP;
    // Varied text LZ4 cannot fold: each record's own letters (xorshift).
    let text = |id: u64| -> String {
        let mut x = id.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1;
        (0..300)
            .map(|_| {
                x ^= x << 13;
                x ^= x >> 7;
                x ^= x << 17;
                char::from(b'a' + u8::try_from(x % 26).expect("a letter"))
            })
            .collect()
    };
    let cap = u64::try_from(LEDGER_CAP).expect("cap");
    let rig = Rig::new(6, 40);
    let turns = ledger_of((1..=cap).map(|id| turn(id, &text(id))).collect());
    rig.journal.carry_ledger(&turns);
    let held = carried_ids(&rig.restoration());
    let held_len = u64::try_from(held.len()).expect("len");
    assert!(
        !held.is_empty() && held_len < cap,
        "premise: the slot holds only the newest, {held_len} of {cap}"
    );
    assert_eq!(held.last(), Some(&cap), "the newest are the ones held");
    assert_eq!(
        rig.journal.ledger_lag(),
        cap - held_len,
        "the records the slot does not hold are counted"
    );
    // The next carry sheds as many again: still counted, never 0.
    turns
        .lock()
        .expect("ledger")
        .push(turn(cap + 1, &text(cap + 1)));
    rig.journal.pass_ms(CONTROL_MS);
    rig.journal.control_tick();
    let held = carried_ids(&rig.restoration());
    assert_eq!(held.last(), Some(&(cap + 1)), "carried");
    assert_eq!(
        rig.journal.ledger_lag(),
        cap - u64::try_from(held.len()).expect("len"),
        "the ledger holds {cap} records, the slot {}",
        held.len()
    );
}
