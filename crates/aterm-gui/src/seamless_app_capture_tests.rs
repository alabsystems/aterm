// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE WHOLE-APP CAPTURE PROPERTY (the 2026-09-22/23 update audit, plan P1-6).
//!
//! Every other handoff test builds its `Terminal`s by hand and gives them to
//! [`carry_for_wire`] or straight to [`write_outgoing`]. The loop that decides
//! what an update actually carries — `App::capture_parked_screens`, walking the
//! real pool of every window, tab and split and pricing each session against
//! budgets the whole pool shares — had no test that drove it over more than a
//! hand-made session or two. Yet the desks that refused updates in the field
//! were App-shaped: a message band that took a row from every session between
//! a program's last frame and the park, a 5K window in full screen, a dozen
//! panes in two windows.
//!
//! So here a real headless [`crate::App`] is built: one to three windows of one
//! to four tabs with random splits, each window sized by the GUI's own grid law
//! from the real-display table of the per-grid-cap regression, every pane on a
//! real, quiet PTY. Every session is fed the hostile walk grammar
//! ([`hostile_walk_step`]), the message band takes or gives back a row, and the
//! fork lane's park runs over the pool ([`crate::App::park_desk_for_test`]).
//! The bytes then go through the shipping manifest writer, and this build's
//! successor reads them back ([`take_incoming_as`]) and proves the adoption.

use std::sync::PoisonError;

use aterm_core::terminal::TerminalCheckpoint;
use winit::dpi::PhysicalSize;

use super::ladder_tests::{assert_only_over_cap_links_dropped, hostile_walk_step, xorshift};
use super::tests::{ENV_LOCK, RestoreVar, child_proof_from, pipe_pair};
use super::*;
use crate::WindowId;

/// Every variable a handoff test sets, restored when the test ends.
fn restore_env() -> [RestoreVar; 11] {
    [
        RestoreVar::new("XDG_RUNTIME_DIR"),
        RestoreVar::new("HOME"),
        RestoreVar::new(ENV_MANIFEST),
        RestoreVar::new(ENV_NONCE),
        RestoreVar::new(ENV_FDS),
        RestoreVar::new(ENV_LAYOUT),
        RestoreVar::new(ENV_TARGET),
        RestoreVar::new(ENV_READY_FD),
        RestoreVar::new(ENV_COMMIT_FD),
        RestoreVar::new(ENV_PARENT_PID),
        RestoreVar::new(ENV_PARENT_BIRTH),
    ]
}

/// A private scratch HOME and runtime dir for one desk, set BEFORE its App is
/// built, so nothing the App or the manifest writer touches lands in the real
/// home. Removed by the caller.
fn enter_scratch(label: &str) -> std::path::PathBuf {
    let scratch =
        std::env::temp_dir().join(format!("aterm-app-capture-{label}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&scratch);
    std::fs::create_dir_all(&scratch).expect("scratch");
    // Every caller holds ENV_LOCK for its whole body.
    aterm_log::env::set("XDG_RUNTIME_DIR", &scratch);
    aterm_log::env::set("HOME", &scratch);
    let dir = crate::control_auth::socket_dir().expect("scratch control dir");
    std::fs::create_dir_all(&dir).expect("control dir");
    scratch
}

fn openpty() -> (i32, i32) {
    let (mut master, mut slave) = (-1_i32, -1_i32);
    // SAFETY: openpty(3) into two valid out-slots; no termios/winsize.
    let opened = unsafe {
        libc::openpty(
            &mut master,
            &mut slave,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
        )
    };
    assert_eq!(opened, 0, "openpty");
    (master, slave)
}

/// Every session of a desk on its own real, QUIET PTY. The capture polls a
/// mid-sequence session's master for queued output (a stub's `-1` would read
/// as activity and miss the park), and the manifest writer refuses a
/// descriptor below 3. The stub sessions keep their `-1` pid: the capture
/// never reads it, and a pid this test does not own must never be signalled.
#[derive(Default)]
struct DeskPtys {
    pairs: Vec<(u64, i32, i32)>,
}

impl DeskPtys {
    /// A new window's first session, on its PTY BEFORE the window is installed:
    /// `install_window_state` registers only a session with a real master (an
    /// inert `-1` stub is kept out of the registry), and a session missing
    /// from the registry is missing from the handoff manifest, which the
    /// writer then refuses as a capture that disagrees with itself.
    fn window_session(&mut self, id: u64) -> crate::Session {
        let (master, slave) = openpty();
        let mut session = crate::stub_session(id);
        session.master = master;
        self.pairs.push((id, master, slave));
        session
    }

    /// Every other pooled session (tabs and splits register whatever their
    /// master), each on its own PTY.
    fn attach_rest(&mut self, app: &mut crate::App) {
        let mut ids: Vec<u64> = app
            .pool
            .iter()
            .map(|session| session.id)
            .filter(|id| self.pairs.iter().all(|(attached, _, _)| attached != id))
            .collect();
        ids.sort_unstable();
        for id in ids {
            let (master, slave) = openpty();
            app.pool
                .sessions
                .get_mut(&id)
                .expect("the session is pooled")
                .session
                .master = master;
            self.pairs.push((id, master, slave));
        }
    }

    /// Unhook every master from its stub session before the App drops (the
    /// sink never owned it), then close both ends.
    fn release(self, mut app: crate::App) {
        for (id, _, _) in &self.pairs {
            if let Some(pooled) = app.pool.sessions.get_mut(id) {
                pooled.session.master = -1;
            }
        }
        drop(app);
        for (_, master, slave) in self.pairs {
            aterm_pty::close_fd(master);
            aterm_pty::close_fd(slave);
        }
    }
}

/// What one desk's handover came to, for the run's census.
#[derive(Default)]
struct Crossed {
    sessions: usize,
    /// Carried as the engine's own exact carry at the depth carried — the Full
    /// rung, or VisibleOnly (history dropped, screen exact).
    exact: usize,
    /// Carried exact but for the hyperlinks of line records over the wire's
    /// record cap — the StrippedLinks rung (plan P2-4).
    stripped_links: usize,
    history_lines: u64,
    /// The window carry the parent wrote, as the successor read it back.
    window: Option<crate::session_store::WindowCarry>,
    /// The screens as carried — and, asserted, as adopted.
    screens: Vec<(u64, TerminalCheckpoint)>,
    /// THE HISTORY CARRY (`crate::handoff_history`), run as the fork lane's
    /// worker runs it: the records the join named a scrollback sidecar on…
    history_sidecars: usize,
    /// …the adopted sessions that took one (asserted: every named one)…
    history_adopted: usize,
    /// …and the scrollback lines the join counted as left behind, over every
    /// record (`history_dropped`)…
    history_dropped: u64,
    /// …of which the successor read as WITHHELD by a handoff policy, off the
    /// wire (`SessionRecord::history_withheld` → `AdoptedHistory::withheld`):
    /// what its band row says as the new version's request, not a failure.
    history_withheld: u64,
}

/// THE PARK → WRITE → ADOPT PIPELINE over a whole App, with every assertion
/// the property makes, labelled `at` (which names the seed).
///
/// The parent half mirrors the fork lane and `prepare_outgoing_artifacts`: the
/// park (`App::park_desk_for_test`), the layout's round trip, the control
/// carry's export, the masters duplicated close-on-exec as the lanes do, the
/// history carry's export and join under the park's plan, the manifest and its
/// sidecars written under a fresh nonce, the layout sidecar, and the adoption
/// proof the parent would wait for. The child half is this build's successor
/// reading exactly those bytes.
fn hand_over(app: &mut crate::App, at: &str) -> Crossed {
    hand_over_as(app, at, None, Carried::Exact)
}

/// How a desk's screens must cross.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Carried {
    /// Every screen at an exact rung — the property of an honest desk.
    Exact,
    /// Every screen blank, marked for a redraw, with no control carry — what a
    /// successor's handoff policy of `carry = "repaint"` asks for (plan P0-5).
    Blank,
}

/// [`hand_over`] for the attempt `attempt`: the park reads the successor's
/// handoff policy for that ticket through `App::park_policy`, the call both
/// lanes make at their park, and every screen must cross as `carried` says.
fn hand_over_as(
    app: &mut crate::App,
    at: &str,
    attempt: Option<&crate::native_updater_service::ApplyAttemptTicket>,
    carried_as: Carried,
) -> Crossed {
    let build = attempt.map_or_else(crate::running_build_number, |attempt| {
        attempt.target_build()
    });
    let parked = app
        .park_desk_for_test(attempt)
        .unwrap_or_else(|failure| panic!("{at}: the park refused a desk the GUI built: {failure}"));

    let mut pool_ids: Vec<u64> = app.pool.iter().map(|session| session.id).collect();
    pool_ids.sort_unstable();
    let ids_of = |entries: &mut dyn Iterator<Item = u64>| {
        let mut ids: Vec<u64> = entries.collect();
        ids.sort_unstable();
        ids
    };
    assert_eq!(
        ids_of(&mut parked.live.iter().map(|(id, _, _)| *id)),
        pool_ids,
        "{at}: every pooled session is handed"
    );
    assert_eq!(
        ids_of(&mut parked.screens.iter().map(|(id, _)| *id)),
        pool_ids,
        "{at}: every handed session carries a screen"
    );
    let (repainted, with_differ) = match carried_as {
        Carried::Exact => (Vec::new(), pool_ids.clone()),
        Carried::Blank => (pool_ids.clone(), Vec::new()),
    };
    assert_eq!(
        ids_of(&mut parked.repaint.iter().copied()),
        repainted,
        "{at}: the sessions carried blank ({carried_as:?}) — an honest desk has none of its \
         own"
    );
    assert_eq!(
        ids_of(
            &mut parked
                .carries
                .iter()
                .map(crate::handoff_carry::CarrySource::local_id)
        ),
        pool_ids,
        "{at}: every session keeps its control carry (ledger and archive rows)"
    );
    assert_eq!(
        ids_of(
            &mut parked
                .carries
                .iter()
                .filter(|source| source.carries_differ())
                .map(crate::handoff_carry::CarrySource::local_id)
        ),
        with_differ,
        "{at}: every session carried at an exact rung keeps its differ's state, and a blank \
         one has none"
    );
    assert_eq!(
        parked.window.as_ref().map(|window| window.status_bar_rows),
        Some(app.message_band_rows),
        "{at}: the window carry names the band rows the grids were sized under"
    );

    // FIDELITY: each carried screen is the engine's own exact carry at the
    // depth it was carried, or that with only over-cap links dropped.
    let mut crossed = Crossed {
        sessions: parked.screens.len(),
        window: parked.window.clone(),
        screens: parked.screens.clone(),
        ..Crossed::default()
    };
    for (local_id, carried) in &parked.screens {
        let at = format!(
            "{at}, session {local_id} ({}x{}, {} history line(s))",
            carried.rows, carried.cols, carried.history_lines
        );
        assert!(
            carried.history_lines <= MAX_HANDOFF_HISTORY_LINES,
            "{at}: over the wire's history ceiling"
        );
        let term = app
            .pool
            .get(*local_id)
            .expect("a carried session is pooled")
            .term
            .clone();
        if carried_as == Carried::Blank {
            // The Repaint rung's own carry of this engine: a blank canonical
            // screen at its geometry, every scalar the rung keeps.
            let blank = repaint_carry_for_wire(
                &crate::term_lock(&term),
                *local_id,
                &mut 0,
                WireCaps::for_target(build),
                String::new(),
            )
            .0;
            assert_eq!(
                carried, &blank,
                "{at}: carried as the Repaint rung's blank screen"
            );
            continue;
        }
        let exact = crate::term_lock(&term)
            .checkpoint_carry_abandoning_partial(carried.history_lines as usize)
            .0;
        if &exact == carried {
            crossed.exact += 1;
        } else {
            let lines = assert_only_over_cap_links_dropped(&at, &exact, carried);
            assert!(
                lines > 0,
                "{at}: a carry that is not the engine's own must be one that lost over-cap links"
            );
            crossed.stripped_links += 1;
        }
        crossed.history_lines += u64::from(carried.history_lines);
    }

    // THE PARENT'S PREPARATION, as `prepare_outgoing_artifacts` does it.
    let layout_digest = parked
        .layout_digest
        .unwrap_or_else(|| panic!("{at}: the layout could not be committed canonically"));
    assert_eq!(
        parked
            .layout
            .to_toml()
            .ok()
            .and_then(|wire| crate::restore::RestoreManifest::from_toml(&wire))
            .as_ref(),
        Some(&parked.layout),
        "{at}: the layout round-trips, or the worker refuses to persist it"
    );
    let controls = crate::handoff_carry::export(&parked.carries);
    let mut manifest = parked.manifest.clone();
    manifest.next_turn_id =
        crate::handoff_carry::manifest_turn_id(crate::control::turn_ids_for_handoff());
    // The lanes hand the successor close-on-exec DUPLICATES of the masters;
    // the successor, played in this process, owns them from here (it closes
    // them itself if it refuses). A stub session has no child, so its pid on
    // the wire is a stand-in the handoff only ever hashes into the proof.
    let adoption: Vec<(u64, i32, i32)> = parked
        .live
        .iter()
        .map(|(local_id, master, _)| {
            // SAFETY: duplicates a live, test-owned PTY master as a fresh
            // close-on-exec descriptor numbered 3 or above.
            let duplicate = unsafe { libc::fcntl(*master, libc::F_DUPFD_CLOEXEC, 3) };
            assert!(duplicate >= 3, "{at}: F_DUPFD_CLOEXEC");
            let stand_in = 40_000 + i32::try_from(*local_id).expect("a small id");
            (*local_id, duplicate, stand_in)
        })
        .collect();
    let fds = HandoffFds {
        entries: adoption.clone(),
    };
    let nonce = mint_outgoing_nonce();
    // THE HISTORY CARRY, as the worker runs it before the manifest is written:
    // the park's plan exported (the fork lane's, after the capture), joined to
    // the park's heads, and each record stamped with its sidecar or its count.
    let dir = crate::control_auth::socket_dir().expect("the scratch control dir");
    let history_plan = format!("{:?}", parked.history);
    let verdicts = crate::handoff_history::stamp_manifest(
        &mut manifest,
        &parked.screens,
        &parked.history_heads,
        parked.history.results(&dir),
        &dir,
        &nonce,
    );
    assert_eq!(
        verdicts.len(),
        pool_ids.len(),
        "{at}: the join judged every handed session ({history_plan})"
    );
    let named = manifest
        .sessions
        .iter()
        .filter(|record| record.history.is_some())
        .count();
    let dropped = manifest
        .sessions
        .iter()
        .map(|record| record.history_dropped)
        .sum::<u64>();
    let outgoing = write_outgoing(
        &manifest,
        &fds,
        &parked.screens,
        &parked.repaint,
        parked.window.clone(),
        &controls,
        &nonce,
    )
    .unwrap_or_else(|failure| panic!("{at}: the manifest writer refused the park: {failure}"));
    assert_eq!(
        outgoing.screen_digest, parked.screen_digest,
        "{at}: the bytes written are the bytes the park committed to"
    );
    let layout_path = std::path::Path::new(&outgoing.manifest_path).with_extension("layout.toml");
    crate::restore::write_once_to(&layout_path, &parked.layout)
        .unwrap_or_else(|error| panic!("{at}: the layout sidecar: {error}"));
    let expected = adoption_proof(
        &nonce,
        build,
        crate::build_info::GIT_COMMIT,
        &layout_digest,
        &parked.screen_digest,
        &adoption,
    )
    .expect("the parent's expectation");

    // THE SUCCESSOR, reading exactly those bytes.
    let (ready_read, ready_write) = pipe_pair("ready");
    let (commit_read, commit_write) = pipe_pair("commit");
    // SAFETY: `getppid` is a side-effect-free libc getter.
    let parent_pid = unsafe { libc::getppid() };
    aterm_log::env::set(ENV_MANIFEST, &outgoing.manifest_path);
    aterm_log::env::set(ENV_NONCE, &outgoing.nonce);
    aterm_log::env::set(ENV_FDS, &outgoing.fds_wire);
    aterm_log::env::set(ENV_LAYOUT, &layout_path);
    aterm_log::env::set(ENV_READY_FD, ready_write.to_string());
    aterm_log::env::set(ENV_COMMIT_FD, commit_read.to_string());
    aterm_log::env::set(ENV_PARENT_PID, parent_pid.to_string());
    match read_process_birth(parent_pid) {
        Some(birth) => aterm_log::env::set(ENV_PARENT_BIRTH, birth.to_wire()),
        None => aterm_log::env::unset(ENV_PARENT_BIRTH),
    }
    aterm_log::env::set(
        ENV_TARGET,
        encode_target_identity(build, crate::build_info::GIT_COMMIT),
    );
    let incoming = take_incoming_as(ReceiverShape::Current);
    assert_eq!(
        ids_of(&mut incoming.adopted.iter().map(|adopted| adopted.local_id)),
        pool_ids,
        "{at}: EVERY SESSION ADOPTS"
    );
    let adopted_history = incoming
        .adopted
        .iter()
        .filter(|adopted| adopted.history.carry.is_some())
        .count();
    assert_eq!(
        adopted_history, named,
        "{at}: the successor takes every scrollback sidecar the join named, and no other"
    );
    let strays: Vec<_> = std::fs::read_dir(&dir)
        .expect("the control dir")
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| path.to_string_lossy().ends_with(".hist"))
        .collect();
    assert!(
        strays.is_empty(),
        "{at}: no scrollback file is left in the control dir once the successor took its \
         own: {strays:?}"
    );
    crossed.history_sidecars = named;
    crossed.history_adopted = adopted_history;
    crossed.history_dropped = dropped;
    crossed.history_withheld = incoming
        .adopted
        .iter()
        .map(|adopted| adopted.history.withheld)
        .sum();
    assert_eq!(
        incoming.screen_digest,
        Some(parked.screen_digest),
        "{at}: the digest the successor computes is the one the parent committed to"
    );
    assert_eq!(
        incoming.layout_digest,
        Some(layout_digest),
        "{at}: the layout digest matches"
    );
    assert_eq!(
        incoming.layout.as_ref(),
        Some(&parked.layout),
        "{at}: the layout is placed, naming exactly the adopted sessions"
    );
    assert_eq!(
        incoming.window, parked.window,
        "{at}: the window carry crosses"
    );
    for adopted in &incoming.adopted {
        let carried: &TerminalCheckpoint = parked
            .screens
            .iter()
            .find(|(local_id, _)| *local_id == adopted.local_id)
            .map(|(_, checkpoint)| checkpoint)
            .expect("an adopted session was carried");
        assert_eq!(
            adopted.repaint,
            carried_as == Carried::Blank,
            "{at}, session {}: the successor pulses a redraw exactly for a blank carry",
            adopted.local_id
        );
        assert_eq!(
            adopted.checkpoint.as_ref(),
            Some(carried),
            "{at}, session {}: adopted exactly as carried",
            adopted.local_id
        );
    }
    let ((proof, ready, _), adopted) = child_proof_from(incoming)
        .unwrap_or_else(|| panic!("{at}: the successor adopted but could not prove it"));
    assert_eq!(
        proof, expected,
        "{at}: THE HANDOFF COMPLETES — the successor's proof is the parent's expectation"
    );
    drop(ready);
    // Each adopted master is a `HandedMaster`, which closes its descriptor when
    // it drops; the pipes are plain descriptors.
    drop(adopted);
    for fd in [ready_read, commit_read, commit_write] {
        aterm_pty::close_fd(fd);
    }
    crossed
}

/// The four real Mac displays of the per-grid-cap regression
/// (`capture_budget_reservation_tests::every_full_screen_display_geometry_is_admitted_for_a_pool_of_panes`),
/// in physical pixels at 2x.
const DISPLAYS: [(u32, u32); 4] = [(3024, 1964), (3456, 2234), (5120, 2880), (6016, 3384)];

/// What a maximized window gives up on those displays: a 37 pt notched menu
/// bar and a 70 pt Dock, at 2x.
const MAXIMIZED_CHROME_PX: u32 = 74 + 140;

/// The font sizes a desk is sampled at: the per-grid-cap regression's range
/// but for its bottom two sizes, where three full-screen 6K windows (about
/// 1.4 Mi visible+alt cells each at 6 px) outgrow the 4 Mi aggregate ceiling.
/// This property is about the desks the wire can carry exactly; a pool past
/// the ceiling is carried blank on purpose, which
/// `a_pool_over_the_aggregate_lowers_its_largest_session` pins. Every case
/// asserts it stayed inside.
const FONT_PX: std::ops::RangeInclusive<u32> = 8..=48;

/// The largest pool a desk is built with.
const MAX_SESSIONS: usize = 12;

/// Give window `wid` the owner's measured chrome (pad 24, pad_top 4, head 80
/// at scale 2) at `font_px`, then size it to `size` through the GUI's own
/// resize path (`App::on_resize` → the grid law → every pane). Returns the
/// grid the law gave it.
fn size_window(
    app: &mut crate::App,
    wid: WindowId,
    font_px: u32,
    size: PhysicalSize<u32>,
) -> (u16, u16) {
    set_chrome(app, wid, font_px);
    app.on_resize(wid, size);
    let ws = app.windows.get(&wid).expect("the window");
    (ws.rows, ws.cols)
}

/// The owner's measured chrome (pad 24, pad_top 4, head 80 at scale 2) on
/// window `wid`, at `font_px` — what the grid law reads.
fn set_chrome(app: &mut crate::App, wid: WindowId, font_px: u32) {
    let ws = app.windows.get_mut(&wid).expect("the window");
    ws.metrics.pad = 24;
    ws.metrics.pad_top = 4;
    ws.metrics.head = 80;
    ws.metrics.font_px = font_px as f32;
}

/// CASE 0'S WINDOW: the field report's 99x338 grid — the window the old 32 Ki
/// per-grid cap refused on every update — at the park, with the band at
/// `capture_band` rows. The 5K display's width at the first font the grid law
/// gives 338 columns (24 px), and the height at which it gives 99 rows: the
/// headless cell box (15x28 px at 24 px) is not CoreText's, so the display's
/// full-screen height alone makes 101.
fn field_report_window(
    app: &mut crate::App,
    wid: WindowId,
    capture_band: u16,
) -> (u32, PhysicalSize<u32>) {
    let band = app.message_band_rows;
    app.message_band_rows = capture_band;
    let width = DISPLAYS[2].0;
    let smallest = crate::FONT_PX_MIN as u32;
    let font_px = (smallest..=48)
        .find(|font_px| {
            set_chrome(app, wid, *font_px);
            app.grid_dims_for(wid, PhysicalSize::new(width, DISPLAYS[2].1))
                .1
                == 338
        })
        .expect("PRECONDITION: some font makes the 5K display 338 columns");
    set_chrome(app, wid, font_px);
    let height = (1..=DISPLAYS[2].1)
        .find(|height| app.grid_dims_for(wid, PhysicalSize::new(width, *height)) == (99, 338))
        .expect("PRECONDITION: a 5K window height is 99 rows at that font");
    app.message_band_rows = band;
    (font_px, PhysicalSize::new(width, height))
}

/// The message band's committed rows move from what they are to `rows`, and
/// every window re-grids by the one law — what `App::sync_message_band_rows`
/// and `App::regrid_for_chrome_rows` do, the latter's per-window `on_resize`
/// spelled out because a headless window has no surface for it to find.
fn set_band_rows(app: &mut crate::App, rows: u16, sizes: &[(WindowId, PhysicalSize<u32>)]) {
    app.message_band_rows = rows;
    for (wid, size) in sizes {
        app.on_resize(*wid, *size);
    }
}

/// One desk: its App (every session on a PTY), the window sizes it was built
/// at, and a sentence describing it for a failure message.
struct Desk {
    app: crate::App,
    ptys: DeskPtys,
    sizes: Vec<(WindowId, PhysicalSize<u32>)>,
    shape: String,
}

/// Build the desk `seed` names: 1 to 3 windows, each of 1 to 4 tabs with 0 to
/// 2 splits a tab in random directions, at most [`MAX_SESSIONS`] sessions; a
/// display, shape and font per window; the band at 0 or 1 row (the park finds
/// it toggled). `field_report` makes window 0's first tab a single pane at the
/// field report's 99x338 ([`field_report_window`]).
fn build_desk(seed: u64, field_report: bool) -> (Desk, impl FnMut() -> u64) {
    let mut next = xorshift(seed);
    let mut app = crate::App::headless_for_test();
    let mut ptys = DeskPtys::default();
    let windows = 1 + usize::try_from(next() % 3).expect("small");
    let mut wids = vec![WindowId(0)];
    for _ in 1..windows {
        let id = app.next_session_id;
        wids.push(app.insert_logical_window(ptys.window_session(id), 24, 80));
    }
    let band = u16::from(next() % 2 == 1);
    app.message_band_rows = band;
    let mut sizes = Vec::new();
    let mut shape = Vec::new();
    for (index, wid) in wids.iter().enumerate() {
        let (font_px, size, what) = if field_report && index == 0 {
            let (font_px, size) = field_report_window(&mut app, *wid, 1 - band);
            (font_px, size, "the field report's window")
        } else {
            let (width, height) = DISPLAYS[usize::try_from(next() % 4).expect("small")];
            let maximized = next() % 2 == 1;
            let font_px = FONT_PX.start()
                + u32::try_from(next() % u64::from(FONT_PX.end() - FONT_PX.start() + 1))
                    .expect("small");
            let height = if maximized {
                height - MAXIMIZED_CHROME_PX
            } else {
                height
            };
            let what = if maximized {
                "maximized"
            } else {
                "full screen"
            };
            (font_px, PhysicalSize::new(width, height), what)
        };
        let (rows, cols) = size_window(&mut app, *wid, font_px, size);
        sizes.push((*wid, size));
        shape.push(format!(
            "{}x{} {what} at {font_px} px ({rows}x{cols}",
            size.width, size.height
        ));
    }
    for (index, wid) in wids.iter().enumerate() {
        let tabs = 1 + next() % 4;
        let mut splits_made = Vec::new();
        for tab in 0..tabs {
            if tab > 0 {
                if app.pool.iter().count() >= MAX_SESSIONS {
                    break;
                }
                let id = app.next_session_id;
                app.push_stub_tab(*wid, crate::stub_session(id));
            }
            let mut splits = 0;
            // The field report's window stays one pane in its first tab:
            // 99x338 is the grid the old per-grid cap refused, not a pane of it.
            let wanted = next() % 3;
            let wanted = if field_report && index == 0 && tab == 0 {
                0
            } else {
                wanted
            };
            for _ in 0..wanted {
                if app.pool.iter().count() >= MAX_SESSIONS {
                    break;
                }
                let dir = if next().is_multiple_of(2) {
                    crate::pane::SplitDir::Vertical
                } else {
                    crate::pane::SplitDir::Horizontal
                };
                app.split_active_stub_tab_dir(*wid, dir);
                splits += 1;
            }
            splits_made.push(splits);
        }
        // Every tab's panes at the window's geometry, not only the active one.
        app.resize_panes(*wid);
        shape[index].push_str(&format!(
            ", {} tab(s) with {splits_made:?} split(s))",
            splits_made.len()
        ));
    }
    ptys.attach_rest(&mut app);
    let shape = format!(
        "{} session(s), band {band} row(s); {}",
        app.pool.iter().count(),
        shape.join("; ")
    );
    (
        Desk {
            app,
            ptys,
            sizes,
            shape,
        },
        next,
    )
}

/// Prints the replay line if a desk fails, from wherever it panicked — an
/// assertion here or a panic inside the pipeline itself. A case number, not a
/// bare seed, replays it: the seed is derived from the number, and case 0 is
/// also the pinned field-report window.
struct ReplayOnPanic {
    case: usize,
    seed: u64,
}

impl Drop for ReplayOnPanic {
    fn drop(&mut self) {
        if std::thread::panicking() {
            eprintln!(
                "whole-App capture property FAILED at case {} (seed {:#018x}); replay that \
                 desk alone with ATERM_APP_CAPTURE_CASE={}",
                self.case, self.seed, self.case
            );
        }
    }
}

/// EVERY DESK THE GUI CAN BUILD HANDS EVERY SESSION OVER EXACTLY (the
/// 2026-09-22/23 update audit, plan P1-6).
///
/// Forty seeded desks, each a real headless App: 1 to 3 windows of 1 to 4 tabs
/// with random splits (at most twelve sessions), every window sized by the
/// GUI's grid law from the real-display table — case 0 is the field report's
/// 99x338 window on the 5K display, the grid that refused every update on the
/// per-grid cap — and every pane on a real PTY. Each session is fed up to 600
/// lines of styled output, then 24 to 63 steps of the hostile walk grammar
/// (NUL and 4 KiB directories, link-dense lines, wide/combining/ZWJ clusters,
/// unterminated strings, 1049, DECSC on the last row, geometry out to
/// 120x400); the panes are put back at their layout's geometry, and the
/// message band takes or gives back a row.
///
/// Then the fork lane's park runs over the whole pool, the manifest writer
/// writes it, and this build's successor reads it back. The property: every
/// session adopts; the screen digest the successor computes is the one the
/// parent committed to, and its adoption proof is the parent's expectation;
/// the layout and the window carry cross; and every screen crosses at an exact
/// rung — the engine's own carry at the depth carried (Full, or VisibleOnly) —
/// except a line of links past the wire's record cap, which costs exactly that
/// line's links (StrippedLinks, plan P2-4). No session of an honest desk is
/// ever carried clamped or blank.
///
/// A failure prints its case and seed; `ATERM_APP_CAPTURE_CASE=<case>` replays
/// that desk alone — and the same PARK of it: the capture walks the pool in
/// session-id order, not the pool's randomly seeded `HashMap` order, so which
/// session keeps its scrollback and which meets a spent budget is the same on
/// every run of a case.
///
/// RED against each fix it guards, measured 2026-09-24 by reverting that one
/// fix in place and running this test: the 32 Ki per-grid cap (plan P0-4a)
/// carries case 0's 99x338 window blank; the rows-bounded DECSC slot
/// (d8d26f0a3) clamps a case-0 session the band's shrink left with its slot
/// past the grid; a producer without the StrippedLinks rung (plan P2-4)
/// carries a case-1 session blank; a park that refuses every mid-sequence
/// parser, quiet PTY or not (the pre-ladder capture), refuses case 1; and the
/// column-bounded attribute check of the line decoder (plan P0-4b) carries a
/// case-2 session blank.
#[test]
fn every_app_desk_the_gui_can_build_hands_every_session_over_exactly() {
    const CASES: usize = 40;
    const BASE_SEED: u64 = 0x00A7_E5C0_91D6_5EED;

    let _env = ENV_LOCK.lock().unwrap_or_else(PoisonError::into_inner);
    let _restore = restore_env();
    let replay = std::env::var("ATERM_APP_CAPTURE_CASE").ok().map(|text| {
        text.trim()
            .parse::<usize>()
            .expect("ATERM_APP_CAPTURE_CASE is a case number")
    });
    let started = std::time::Instant::now();
    let mut total = Crossed::default();
    let mut mid_sequence = 0_usize;
    let mut alternate = 0_usize;
    let mut slot_past_grid = 0_usize;
    let cases: Vec<usize> = replay.map_or_else(|| (0..CASES).collect(), |case| vec![case]);
    for case in cases {
        // SplitMix64 of the case number: well spread, never zero.
        let mut z = BASE_SEED.wrapping_add(0x9E37_79B9_7F4A_7C15_u64.wrapping_mul(case as u64 + 1));
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        let seed = (z ^ (z >> 31)) | 1;
        let _replay = ReplayOnPanic { case, seed };
        let scratch = enter_scratch(&format!("{case}"));
        let (desk, mut next) = build_desk(seed, case == 0);
        let Desk {
            mut app,
            ptys,
            sizes,
            shape,
        } = desk;
        let at = format!("case {case} (seed {seed:#018x}: {shape})");

        // THE WALK: each session its own hostile program.
        let mut ids: Vec<u64> = app.pool.iter().map(|session| session.id).collect();
        ids.sort_unstable();
        for id in &ids {
            let term = app.pool.get(*id).expect("pooled").term.clone();
            let mut t = crate::term_lock(&term);
            // A prior life first: up to 600 lines of styled output, so most
            // sessions have scrollback for the capture to price and carry.
            let mut prior = Vec::new();
            for line in 0..next() % 600 {
                prior.extend_from_slice(
                    format!("\x1b[38;5;{}m{line:>5}\x1b[0m output\r\n", line % 256).as_bytes(),
                );
            }
            t.process(&prior);
            for _ in 0..24 + next() % 40 {
                hostile_walk_step(&mut t, &mut next);
            }
        }
        // The walk resized engines directly; the App puts every pane back at
        // its layout's geometry, as it would have all along.
        for (wid, _) in &sizes {
            app.resize_panes(*wid);
        }
        // THE BAND TOGGLE, between the program's last frame and the park.
        let before: Vec<u16> = sizes
            .iter()
            .map(|(wid, _)| app.windows.get(wid).map_or(0, |ws| ws.rows))
            .collect();
        let band = app.message_band_rows;
        set_band_rows(&mut app, 1 - band, &sizes);
        for ((wid, _), rows_before) in sizes.iter().zip(before) {
            let rows = app.windows.get(wid).map_or(0, |ws| ws.rows);
            assert_eq!(
                i32::from(rows) - i32::from(rows_before),
                if band == 0 { -1 } else { 1 },
                "{at}: the band's row moved window {wid:?}'s grid"
            );
        }
        if case == 0 {
            let term = app.pool.get(0).expect("window 0's first tab").term.clone();
            let t = crate::term_lock(&term);
            assert_eq!(
                (t.rows(), t.cols()),
                (99, 338),
                "{at}: case 0 parks the field report's 99x338 window"
            );
        }
        let mut mandatory = 0_u64;
        for id in &ids {
            let term = app.pool.get(*id).expect("pooled").term.clone();
            let t = crate::term_lock(&term);
            mandatory += mandatory_checkpoint_cells(t.rows(), t.cols());
            mid_sequence += usize::from(t.partial_sequence_state().is_some());
            alternate += usize::from(t.has_inactive_grid());
            let checkpoint = t.checkpoint_carry_abandoning_partial(0).0;
            slot_past_grid += usize::from(
                [checkpoint.saved_cursor_main, checkpoint.saved_cursor_alt]
                    .into_iter()
                    .flatten()
                    .any(|saved| saved.cursor_row >= t.rows()),
            );
        }
        assert!(
            mandatory <= max_handoff_aggregate_grid_cells(),
            "{at}: PRECONDITION — the desk's visible screens ({mandatory} cells) fit the \
             aggregate the wire carries"
        );

        let crossed = hand_over(&mut app, &at);
        total.sessions += crossed.sessions;
        total.exact += crossed.exact;
        total.stripped_links += crossed.stripped_links;
        total.history_lines += crossed.history_lines;
        ptys.release(app);
        let _ = std::fs::remove_dir_all(&scratch);
    }
    eprintln!(
        "whole-App capture: {} session(s) handed over in {:?} — {} exact, {} link-stripped, \
         {} history line(s); {mid_sequence} mid-sequence, {alternate} with an inactive grid, \
         {slot_past_grid} with a DECSC slot past the grid",
        total.sessions,
        started.elapsed(),
        total.exact,
        total.stripped_links,
        total.history_lines
    );
    if replay.is_none() {
        assert!(
            total.stripped_links > 0
                && total.history_lines > 0
                && mid_sequence > 0
                && alternate > 0
                && slot_past_grid > 0,
            "the property must reach the states it was written for: {} link-stripped, {} \
             history line(s), {mid_sequence} mid-sequence, {alternate} alternate, \
             {slot_past_grid} slot(s) past the grid",
            total.stripped_links,
            total.history_lines
        );
    }
}

/// THE OWNER'S DESK, PINNED (the 2026-09-22 incident; the handoff fixtures'
/// `incident-55x149` desks are the same desk as released producers wrote it,
/// built from bare engines). One
/// window, two tabs at 56x149 by the GUI's grid law. Tab 1 is a shell with 400
/// lines of styled history; tab 0 is Claude Code, entered from a prompt on the
/// LAST row, so 1049's DECSC slot is row 55. Then the update's row appears in
/// the message band — through the real messages engine — and takes one row
/// from every session: 55x149 at the park, and the Claude Code tab's slot names
/// a row its grid no longer has.
///
/// Both sessions must cross at the Full rung: the slot raw (row 55 of 55, which
/// the successor's 1049 exit needs once the band gives the row back), the
/// shell's full 256 lines of history, the band row and its message in the
/// window carry, the digest and the proof equal.
///
/// RED before d8d26f0a3 (measured 2026-09-24 by restoring the rows-bounded
/// DECSC check in `checkpoint_meta_bound_violation`): the Claude Code tab's
/// meta breaks the bound, the ladder clamps its slot, and the session is
/// carried marked for a repaint instead of as the exact screen. Before the
/// ladder the same bound refused the whole update, every attempt, from v0.87.0
/// through v0.90.0.
#[test]
fn the_owners_desk_hands_both_sessions_over_at_the_full_rung() {
    let _env = ENV_LOCK.lock().unwrap_or_else(PoisonError::into_inner);
    let _restore = restore_env();
    let scratch = enter_scratch("owners-desk");
    let mut app = crate::App::headless_for_test();
    let wid = WindowId(0);
    let id = app.next_session_id;
    app.push_stub_tab(wid, crate::stub_session(id));
    assert_eq!(app.pool.iter().count(), 2, "two tabs, one session each");

    // The window size the GUI's law makes 56x149 with no band row.
    let width = (1..20_000_u32)
        .find(|width| app.grid_dims_for(wid, PhysicalSize::new(*width, 20_000)).1 == 149)
        .expect("a width that is 149 columns");
    let height = (1..20_000_u32)
        .find(|height| app.grid_dims_for(wid, PhysicalSize::new(width, *height)).0 == 56)
        .expect("a height that is 56 rows");
    let size = PhysicalSize::new(width, height);
    app.on_resize(wid, size);
    app.resize_panes(wid);
    for session in app.pool.iter() {
        let t = crate::term_lock(&session.term);
        assert_eq!((t.rows(), t.cols()), (56, 149), "both tabs at 56x149");
    }

    let claude = app.pool.get(0).expect("tab 0").term.clone();
    {
        let mut t = crate::term_lock(&claude);
        t.process(b"\x1b]7;file://mac/Users//dev/src/aterm\x07\x1b]0;claude\x07");
        for line in 0..70 {
            t.process(format!("\x1b[38;5;{}mbuild step {line}\x1b[0m\r\n", line % 256).as_bytes());
        }
        t.process(b"\x1b[1;38;5;202muser@mac\x1b[0m ~/src/aterm % claude");
        assert_eq!(t.cursor().row, 55, "the prompt is on the last row");
        // Claude Code: 1049 saves the cursor (row 55) and draws its frame.
        t.process(
            b"\x1b[?1049h\x1b[H\x1b[2J\x1b[1m\xe2\x95\xad\xe2\x94\x80 Claude Code\x1b[0m\r\n",
        );
        t.process(
            b"\xe2\x94\x82 > fix the update handoff\r\n\x1b[38;2;215;119;87m* Thinking\x1b[0m",
        );
        t.process(b"\x1b[56;1H\x1b[2m? for shortcuts\x1b[0m\x1b[3;3H");
    }
    let shell = app.pool.get(1).expect("tab 1").term.clone();
    {
        let mut t = crate::term_lock(&shell);
        t.process(b"\x1b]7;file://mac/Users//dev/src/aterm\x07\x1b]0;zsh\x07");
        for line in 0..400 {
            t.process(
                format!(
                    "\x1b[38;2;{};{};200m{line:>4}\x1b[0m \x1b[4:3;58;5;{}mcompiling\x1b[0m \
                     \u{6f22}\u{5b57} e\u{301}\r\n",
                    line % 256,
                    (line * 7) % 256,
                    line % 16
                )
                .as_bytes(),
            );
        }
        t.process(b"\x1b[1;38;5;202muser@mac\x1b[0m ~/src/aterm % ");
    }

    // The update's row takes the band — through the real messages engine —
    // and the window re-grids: 56 rows become 55. A person is waiting on this
    // check (`aterm ctl update check`): since design ruling 220 a download
    // nobody waits on raises no row, and the incident's shape is a band row,
    // whatever raised it.
    app.note_update_check_asked(true);
    app.note_update_progress(&aterm_update::Progress::Downloading {
        version: "0.93.0".to_string(),
        bytes_done: 120 * 1024 * 1024,
        bytes_total: 480 * 1024 * 1024,
    });
    app.settle_messages(std::time::Instant::now() + aterm_messages::PROGRESS_GRACE * 2);
    assert_eq!(app.message_band_rows, 1, "the update's row is on the glass");
    app.on_resize(wid, size);
    assert_eq!(
        app.windows.get(&wid).map(|ws| (ws.rows, ws.cols)),
        Some((55, 149)),
        "the band's row came out of the grid"
    );
    {
        let t = crate::term_lock(&claude);
        assert_eq!((t.rows(), t.cols()), (55, 149));
        assert!(
            t.has_inactive_grid(),
            "Claude Code is on the alternate screen"
        );
    }

    let mut ptys = DeskPtys::default();
    ptys.attach_rest(&mut app);
    let at = "the owner's desk";
    let crossed = hand_over(&mut app, at);
    assert!(
        crossed
            .window
            .as_ref()
            .is_some_and(|window| window.status_bar_rows == 1 && !window.messages.is_empty()),
        "{at}: the band row and its message ride the window carry: {:?}",
        crossed.window
    );
    assert_eq!(
        (crossed.sessions, crossed.exact, crossed.stripped_links),
        (2, 2, 0),
        "{at}: both sessions cross exactly"
    );
    // The facts the incident turned on, read off what crossed.
    let carried = |local_id: u64| {
        crossed
            .screens
            .iter()
            .find(|(id, _)| *id == local_id)
            .map(|(_, checkpoint)| checkpoint)
            .expect("both sessions crossed")
    };
    let claude_carry = carried(0);
    assert_eq!((claude_carry.rows, claude_carry.cols), (55, 149));
    assert!(
        claude_carry.alt_grid.is_some(),
        "{at}: the Claude Code tab's saved primary crosses under its alternate screen"
    );
    assert_eq!(
        claude_carry.saved_cursor_main.map(|saved| saved.cursor_row),
        Some(55),
        "{at}: the Claude Code tab's DECSC slot rides raw — row 55 of a 55-row grid"
    );
    let shell_carry = carried(1);
    assert_eq!(
        (shell_carry.rows, shell_carry.history_lines),
        (55, MAX_HANDOFF_HISTORY_LINES),
        "{at}: the shell crosses with the full 256 lines of history the wire carries"
    );
    ptys.release(app);
    let _ = std::fs::remove_dir_all(&scratch);
}

/// Seed the App's pre-verification cache as a PASSED check of `attempt`'s
/// candidate whose bundle carried the handoff policy `text` — through the
/// handoff worker's own publication of a pass (`PreverifyPublisher`, built from
/// the attempt as the worker builds it, narrowing by the same `adopt`) — and
/// return what the park will follow.
fn seed_passed_preverification(
    app: &crate::App,
    attempt: &crate::native_updater_service::ApplyAttemptTicket,
    text: &str,
) -> Option<aterm_update_core::handoff_policy::HandoffPolicy> {
    use aterm_update_core::handoff_policy::{HandoffPolicy, PolicyRead};
    let read = PolicyRead::Parsed(HandoffPolicy::parse(text).expect("a schema-1 policy"));
    let published = app.publish_preverified_pass_for_test(attempt, &read);
    assert_eq!(
        app.park_policy(Some(attempt)).policy,
        published,
        "the park reads what the worker published, by the attempt's key"
    );
    published
}

/// THE SUCCESSOR'S SIGNED HANDOFF POLICY STEERS THE WHOLE PARK (the 2026-09-22/23
/// update audit, plan P0-5): the one-release escape hatch for a producer bug the
/// carry ladder cannot see. A real headless App — windows, tabs and splits, every
/// pane on a real PTY, every session with styled history and one of them in a
/// full-screen program — is parked for an attempt whose candidate passed its
/// pre-verification, with the policy that candidate's bundle carried published
/// into the App's own cache by the handoff worker's own publisher, and read for
/// the attempt's ticket by `App::park_policy`, the one call both lanes' parks
/// make. (The lanes' own control flow — the worker's order, the launched lane's
/// gate — is not driven here: this is the park they share.)
///
/// * A policy for OTHER producers changes nothing: its pass publishes no policy
///   for this build (`adopt` narrows it away), and every session crosses exact,
///   scrollback and all.
/// * `carry = "repaint"` for this build: EVERY session is carried at the Repaint
///   rung — a blank canonical screen, no control carry — and still adopts, with
///   `repaint = true` so its program redraws, the digest the successor computes
///   the one the parent committed to, and the adoption proof the parent's
///   expectation. The policy lowered fidelity and touched nothing the consumer
///   checks.
/// * `carry = "visible"`: every session crosses exact at depth 0.
/// * Under either of those two, NO SCROLLBACK CROSSES AT ALL — not in the
///   screen carry and not in the history carry beside it (the 2026-09-27 merge
///   review): the fork lane's plan is `HistoryPlan::Withheld`, nothing is
///   exported, no record names a sidecar, the successor adopts none, and every
///   line the park saw is COUNTED as left behind (`history_dropped`), never
///   dropped in silence. With no policy to follow the same desk's history does
///   cross in sidecars — the control that proves this pipeline runs the carry.
/// * A verdict that did NOT pass is never a policy the park follows, whatever
///   the entry holds.
///
/// RED without the clamp, measured 2026-09-24 by making `carry_for_wire_within`
/// ignore its ceiling: the repaint policy's park carries every session exact and
/// the blank-carry assertion fails on the first session. RED without the
/// history carry's ceiling, measured 2026-09-27 by making
/// `HistoryPlan::deferred_under` ignore it: the repaint policy's hand-over names
/// a sidecar on every session whose history the park saw, and the successor
/// adopts each one.
#[test]
fn a_successor_policy_of_repaint_carries_every_session_blank_and_every_one_adopts() {
    let _env = ENV_LOCK.lock().unwrap_or_else(PoisonError::into_inner);
    let _restore = restore_env();
    let scratch = enter_scratch("policy");
    let (desk, _next) = build_desk(0x0005_0CA5_E7B0_11C7, false);
    let Desk {
        mut app,
        ptys,
        sizes: _,
        shape,
    } = desk;
    let at = format!("the policy desk ({shape})");
    let mut ids: Vec<u64> = app.pool.iter().map(|session| session.id).collect();
    ids.sort_unstable();
    assert!(ids.len() >= 2, "{at}: PRECONDITION — more than one session");
    for (index, id) in ids.iter().enumerate() {
        let term = app.pool.get(*id).expect("pooled").term.clone();
        let mut t = crate::term_lock(&term);
        for line in 0..300 {
            t.process(format!("\x1b[38;5;{}m{line:>4}\x1b[0m built\r\n", line % 256).as_bytes());
        }
        t.process(b"\x1b[1;38;5;202muser@mac\x1b[0m ~ % ");
        if index == 0 {
            t.process(b"\x1b[?1049h\x1b[H\x1b[2J\x1b[1mediting\x1b[0m\x1b[3;5H");
        }
    }

    // Every line of scrollback the park sees, over the pool: what a policy
    // that carries none leaves behind, and must count.
    let seen: u64 = ids
        .iter()
        .map(|id| {
            crate::term_lock(&app.pool.get(*id).expect("pooled").term)
                .history_fence()
                .lines()
        })
        .sum();
    assert!(seen > 0, "{at}: PRECONDITION — the desk holds scrollback");

    let build = crate::running_build_number();
    let commit = crate::build_info::GIT_COMMIT;
    let artifact = "ab".repeat(32);
    let ticket =
        crate::native_updater_service::ApplyAttemptTicket::for_test(build, commit, &artifact);
    let attempt = Some(&ticket);

    // A policy for other producers: nothing to follow, and the park is exact.
    let other = build.wrapping_add(1);
    assert_eq!(
        seed_passed_preverification(
            &app,
            &ticket,
            &format!(
                "schema = 1\napplies_to_producers = [{other}, {other}]\ncarry = \"repaint\"\n"
            ),
        ),
        None,
        "{at}: a policy for other builds is not this one's"
    );
    let exact = hand_over_as(
        &mut app,
        &format!("{at}, other builds' policy"),
        attempt,
        Carried::Exact,
    );
    assert!(
        exact.history_lines > 0,
        "{at}: with no policy to follow, the sessions carry their scrollback"
    );
    assert_eq!(
        exact.history_withheld, 0,
        "{at}: CONTROL — with no policy to follow, nothing is read as withheld"
    );
    assert!(
        exact.history_sidecars > 0 && exact.history_adopted == exact.history_sidecars,
        "{at}: CONTROL — with no policy to follow, the history carry crosses in sidecars \
         ({} named, {} adopted)",
        exact.history_sidecars,
        exact.history_adopted
    );

    // `carry = "repaint"` for this build: every session blank, every one adopts.
    let policy = seed_passed_preverification(
        &app,
        &ticket,
        &format!("schema = 1\napplies_to_producers = [{build}, {build}]\ncarry = \"repaint\"\n"),
    );
    assert!(
        policy.is_some_and(|policy| policy.carry_ceiling()
            == aterm_update_core::handoff_policy::CarryCeiling::Repaint),
        "{at}: PRECONDITION — the repaint policy is this build's to follow: {policy:?}"
    );
    let blank = hand_over_as(
        &mut app,
        &format!("{at}, repaint policy"),
        attempt,
        Carried::Blank,
    );
    assert_eq!(blank.sessions, ids.len(), "{at}: every session crossed");
    assert_eq!(
        blank.history_lines, 0,
        "{at}: a blank carry has no scrollback"
    );
    assert_eq!(
        (blank.history_sidecars, blank.history_adopted),
        (0, 0),
        "{at}: under carry = \"repaint\" no scrollback sidecar is named or adopted"
    );
    assert_eq!(
        blank.history_dropped, seen,
        "{at}: every line of scrollback the park saw is counted as left behind"
    );
    assert_eq!(
        blank.history_withheld, seen,
        "{at}: and the successor reads every one as the policy's, not a failure"
    );
    // …and that park never projects a session's grids, the path a repaint
    // policy is sealed to route around: with that projection made to fail
    // (`checkpoint.carry_projection`), the same park still parks every session.
    let parked = aterm_core::fault::with_armed("checkpoint.carry_projection", || {
        app.park_desk_for_test(attempt)
    })
    .unwrap_or_else(|failure| panic!("{at}: the repaint park failed: {failure}"));
    let mut repainted = parked.repaint.clone();
    repainted.sort_unstable();
    assert_eq!(repainted, ids, "{at}: every session blank, none projected");

    // `carry = "visible"`: exact screens, no scrollback.
    seed_passed_preverification(&app, &ticket, "schema = 1\ncarry = \"visible\"\n")
        .expect("the visible policy applies to every build");
    let visible = hand_over_as(
        &mut app,
        &format!("{at}, visible policy"),
        attempt,
        Carried::Exact,
    );
    assert_eq!(
        (visible.exact, visible.history_lines),
        (ids.len(), 0),
        "{at}: every screen exact, none with scrollback"
    );
    assert_eq!(
        (visible.history_sidecars, visible.history_adopted),
        (0, 0),
        "{at}: under carry = \"visible\" no scrollback sidecar is named or adopted"
    );
    assert_eq!(
        visible.history_dropped, seen,
        "{at}: every line of scrollback the park saw is counted as left behind"
    );
    assert_eq!(
        visible.history_withheld, seen,
        "{at}: and the successor reads every one as the policy's, not a failure"
    );

    // A verdict that did not pass is never followed, even holding a policy —
    // and another artifact's verdict is not this attempt's.
    let repaint = seed_passed_preverification(&app, &ticket, "schema = 1\ncarry = \"repaint\"\n");
    assert!(repaint.is_some());
    assert_eq!(
        app.handoff_policy_for(build, commit, &"cd".repeat(32)),
        None,
        "{at}: another artifact's policy"
    );
    if let Some(entry) = app
        .handoff_preverified
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .as_mut()
    {
        entry.passed = false;
    }
    assert_eq!(
        app.handoff_policy_for(build, commit, &artifact),
        None,
        "{at}: a refused candidate's policy is never followed"
    );
    ptys.release(app);
    let _ = std::fs::remove_dir_all(&scratch);
}

/// One park of `app` written out as the worker writes it — the held panes'
/// screens included — and read back by a successor of `shape`: what it made
/// of the handoff, and the proof the parent waits for.
fn hand_over_held(
    app: &mut crate::App,
    shape: ReceiverShape,
    at: &str,
) -> (
    crate::app_update_handoff::ParkedDeskForTest,
    IncomingHandoff,
    String,
    AdoptionProof,
) {
    let build = crate::running_build_number();
    let parked = app
        .park_desk_for_test(None)
        .unwrap_or_else(|failure| panic!("{at}: the park refused: {failure}"));
    let layout_digest = parked.layout_digest.expect("the layout commits");
    let adoption: Vec<(u64, i32, i32)> = parked
        .live
        .iter()
        .map(|(local_id, master, _)| {
            // SAFETY: duplicates a live, test-owned PTY master as a fresh
            // close-on-exec descriptor numbered 3 or above.
            let duplicate = unsafe { libc::fcntl(*master, libc::F_DUPFD_CLOEXEC, 3) };
            assert!(duplicate >= 3, "{at}: F_DUPFD_CLOEXEC");
            (
                *local_id,
                duplicate,
                40_000 + i32::try_from(*local_id).expect("a small id"),
            )
        })
        .collect();
    let fds = HandoffFds {
        entries: adoption.clone(),
    };
    let nonce = mint_outgoing_nonce();
    let outgoing = write_outgoing_with_held(
        &parked.manifest,
        &fds,
        &parked.screens,
        &parked.repaint,
        parked.window.clone(),
        &[],
        &parked.held,
        &nonce,
    )
    .unwrap_or_else(|failure| panic!("{at}: the manifest writer refused the park: {failure}"));
    assert_eq!(
        outgoing.screen_digest, parked.screen_digest,
        "{at}: the held panes are outside the committed screen digest"
    );
    let layout_path = std::path::Path::new(&outgoing.manifest_path).with_extension("layout.toml");
    crate::restore::write_once_to(&layout_path, &parked.layout).expect("the layout sidecar");
    let expected = adoption_proof(
        &nonce,
        build,
        crate::build_info::GIT_COMMIT,
        &layout_digest,
        &parked.screen_digest,
        &adoption,
    )
    .expect("the parent's expectation");
    let (_ready_read, ready_write) = pipe_pair("ready");
    let (commit_read, _commit_write) = pipe_pair("commit");
    // SAFETY: `getppid` is a side-effect-free libc getter.
    let parent_pid = unsafe { libc::getppid() };
    aterm_log::env::set(ENV_MANIFEST, &outgoing.manifest_path);
    aterm_log::env::set(ENV_NONCE, &outgoing.nonce);
    aterm_log::env::set(ENV_FDS, &outgoing.fds_wire);
    aterm_log::env::set(ENV_LAYOUT, &layout_path);
    aterm_log::env::set(ENV_READY_FD, ready_write.to_string());
    aterm_log::env::set(ENV_COMMIT_FD, commit_read.to_string());
    aterm_log::env::set(ENV_PARENT_PID, parent_pid.to_string());
    match read_process_birth(parent_pid) {
        Some(birth) => aterm_log::env::set(ENV_PARENT_BIRTH, birth.to_wire()),
        None => aterm_log::env::unset(ENV_PARENT_BIRTH),
    }
    aterm_log::env::set(
        ENV_TARGET,
        encode_target_identity(build, crate::build_info::GIT_COMMIT),
    );
    let incoming = take_incoming_as(shape);
    (parked, incoming, nonce, expected)
}

/// Every visible row of `terminal`, one line each.
fn screen_text(terminal: &aterm_core::terminal::Terminal) -> String {
    (0..usize::from(terminal.rows()))
        .filter_map(|row| terminal.row_text(row))
        .collect::<Vec<_>>()
        .join("\n")
}

/// Every file of attempt `nonce` still in the control dir with `suffix`.
fn attempt_files(nonce: &str, suffix: &str) -> Vec<std::path::PathBuf> {
    let dir = crate::control_auth::socket_dir().expect("the scratch control dir");
    std::fs::read_dir(&dir)
        .expect("the control dir")
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| {
            let name = path.to_string_lossy();
            name.contains(nonce) && name.ends_with(suffix)
        })
        .collect()
}

/// ROUND FIVE, ITEM 19: a pane kept open by `--hold` after its command exited
/// keeps its final screen across an update while another session is live.
///
/// Before, it was not a handed session (it has no process to carry), so its
/// screen stayed behind with the old process and the successor showed a
/// "not carried" placeholder in its place. Now the park carries its visible
/// screen as the manifest's side list, and the successor shows it read-only
/// in that pane: the same text, no link to click, no process, `Exited`.
/// Typing into it goes nowhere; closing it closes it.
///
/// THE OLDER SUCCESSOR, played by the receiver built before the carries
/// (`ReceiverShape::PreCarry`), reads the same kind of manifest, skips the side
/// list, adopts every live session exactly and proves: the held screens move
/// no digest. Their sidecars are then retired with the control carry's.
///
/// AND AS THE PANE IT WAS (round six of the update audit, findings 46, 49 and
/// 35): drawn in the successor's configured theme, not the engine's built-in
/// colours; exited in the status observer, with the exit status its command
/// ended with (2), not re-read from its screen as an idle prompt; and wearing
/// the `meta set` identity and spawn identity the person gave it. RED before
/// the fix on each: the engine's default background, a phase that was not
/// `Exited`, and no user title.
#[test]
fn a_held_exited_pane_keeps_its_screen() {
    let _env = ENV_LOCK.lock().unwrap_or_else(PoisonError::into_inner);
    let _restore = restore_env();
    let scratch = enter_scratch("held-pane");
    let mut app = crate::App::headless_for_test();
    app.hold = true;
    let wid = WindowId(0);
    let held_id = app.next_session_id;
    app.push_stub_tab(wid, crate::stub_session(held_id));
    let mut ptys = DeskPtys::default();
    ptys.attach_rest(&mut app);
    {
        let held = app.pool.get(held_id).expect("the held pane").term.clone();
        let mut t = crate::term_lock(&held);
        t.process(b"\x1b]0;make\x07$ make\r\n");
        t.process(b"see \x1b]8;;https://example.com/log\x07the log\x1b]8;;\x07\r\n");
        t.process(b"build finished\r\n");
        assert!(
            t.checkpoint_carry_abandoning_partial(0)
                .0
                .grid
                .windows(b"example.com".len())
                .any(|w| w == b"example.com"),
            "PRECONDITION: the held screen holds a link"
        );
    }
    {
        let live = app.pool.get(0).expect("session 0").term.clone();
        crate::term_lock(&live).process(b"$ still running\r\n");
    }
    // What the reader's EOF does under `--hold`: the pane stays, exited.
    app.store
        .write()
        .unwrap_or_else(PoisonError::into_inner)
        .set_state(held_id, crate::session_store::SessionState::Exited);
    // Its command ended with status 2, collected at the exit; the person had
    // named it, and it was spawned under an identity.
    {
        let pooled = &mut app
            .pool
            .sessions
            .get_mut(&held_id)
            .expect("the held pane")
            .session;
        // As `Session::reap_child` keeps it: the status, and the reap latched.
        let _ = pooled.child_exit.set(aterm_pty::ChildExit::Code(2));
        pooled
            .child_reaped
            .store(true, std::sync::atomic::Ordering::Release);
        pooled.identity = Some("worker".to_string());
        let mut meta = pooled
            .ctx
            .meta
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        let _ = meta.set("title", Some("build log".to_string()));
        let _ = meta.set("icon", Some("hammer".to_string()));
        let _ = meta.set("role", Some("ci".to_string()));
    }

    // THE OLDER SUCCESSOR first: it skips the side list and adopts exactly.
    let at = "an older successor";
    let (parked, old, nonce, expected) = hand_over_held(&mut app, ReceiverShape::PreCarry, at);
    assert_eq!(
        parked.live.iter().map(|(id, _, _)| *id).collect::<Vec<_>>(),
        vec![0],
        "{at}: only the live session is handed"
    );
    assert_eq!(
        parked
            .held
            .iter()
            .map(|pane| pane.local_id)
            .collect::<Vec<_>>(),
        vec![held_id],
        "{at}: the park captured the held pane's screen"
    );
    assert!(
        old.held.is_empty(),
        "{at}: an older reader has no held panes"
    );
    assert_eq!(
        old.adopted.iter().map(|a| a.local_id).collect::<Vec<_>>(),
        vec![0],
        "{at}: every live session adopts"
    );
    let ((proof, ready, _), adopted) = child_proof_from(old).expect("the older successor proves");
    assert_eq!(proof, expected, "{at}: the held screens moved no digest");
    drop((ready, adopted));
    assert_eq!(
        attempt_files(&nonce, HELD_GRID_SUFFIX).len(),
        1,
        "{at}: an older reader leaves the held sidecar unread"
    );
    retire_outgoing_controls(&nonce);
    assert!(
        attempt_files(&nonce, HELD_GRID_SUFFIX).is_empty(),
        "{at}: and the parent retires it once the proof checks out"
    );

    // THIS BUILD'S SUCCESSOR.
    let at = "this build's successor";
    let (_parked, mut incoming, nonce, expected) =
        hand_over_held(&mut app, ReceiverShape::Current, at);
    assert!(
        attempt_files(&nonce, HELD_GRID_SUFFIX).is_empty(),
        "{at}: the successor took the held sidecar"
    );
    assert_eq!(
        incoming
            .adopted
            .iter()
            .map(|a| a.local_id)
            .collect::<Vec<_>>(),
        vec![0],
        "{at}: the live session adopts"
    );
    let held = std::mem::take(&mut incoming.held);
    assert_eq!(
        held.iter().map(|pane| pane.local_id).collect::<Vec<_>>(),
        vec![held_id],
        "{at}: the held pane's screen crossed"
    );
    let layout = incoming.layout.clone().expect("the layout is placed");
    let ((proof, ready, _), adopted) = child_proof_from(incoming).expect("the successor proves");
    assert_eq!(
        proof, expected,
        "{at}: the held screen is outside the proof"
    );
    drop((ready, adopted));
    let carried = &held[0].checkpoint;
    assert!(
        carried.current_working_directory.is_none() && carried.shell_integration_nonce.is_none(),
        "{at}: nothing that acts rides a held screen"
    );
    assert!(
        !carried
            .grid
            .windows(b"example.com".len())
            .any(|w| w == b"example.com"),
        "{at}: the link was dropped"
    );

    // The successor's restore: session 0 adopts shell 0; the held pane's
    // placeholder shows its screen. Its theme is not the engine's default.
    let themed_bg = aterm_types::Rgb { r: 1, g: 2, b: 3 };
    let themed_fg = aterm_types::Rgb {
        r: 250,
        g: 240,
        b: 230,
    };
    let mut new = crate::App::headless_for_test();
    new.session_factory.terminal_config = Some(aterm_core::config::TerminalConfig {
        default_background: themed_bg,
        default_foreground: themed_fg,
        ..aterm_core::config::TerminalConfig::default()
    });
    new.handoff_successor = true;
    new.pool
        .sessions
        .get_mut(&0)
        .expect("session 0")
        .session
        .handoff_local_id = Some(0);
    new.seamless_held = held;
    new.restore_into_window(wid, layout.windows[0].clone());
    assert!(
        new.seamless_held.is_empty(),
        "{at}: the placeholder took it"
    );
    let shown: Vec<u64> = new.windows[&wid]
        .tab_set
        .tabs()
        .iter()
        .flat_map(|tab| tab.root.leaves())
        .filter_map(|view| {
            new.view_store
                .get(view)
                .copied()
                .and_then(crate::tab_model::View::terminal_session)
        })
        .collect();
    assert_eq!(
        shown.len(),
        2,
        "{at}: two terminal panes, no placeholder: {shown:?}"
    );
    let restored = *shown
        .iter()
        .find(|id| **id != 0)
        .expect("the held pane's terminal");
    // 49: exited, with its status, and a sweep does not re-read it as idle.
    new.observe_session_statuses(std::time::Instant::now() + std::time::Duration::from_secs(2));
    assert_eq!(
        new.session_status
            .status(restored)
            .map(|status| (status.phase, status.last_outcome)),
        Some((
            crate::session_status::Phase::Exited,
            crate::session_status::Outcome::Failure { exit_code: 2 }
        )),
        "{at}: THE STATUS SAYS EXITED, WITH THE STATUS IT ENDED WITH"
    );
    let session = new.pool.get(restored).expect("pooled");
    let text = screen_text(&crate::term_lock(&session.term));
    assert!(
        text.contains("build finished") && text.contains("the log"),
        "{at}: THE FINAL SCREEN IS SHOWN, not the placeholder: {text:?}"
    );
    assert_eq!(crate::term_lock(&session.term).title(), "make");
    assert_eq!(
        (session.master, session.pid),
        (-1, -1),
        "{at}: no PTY, no process"
    );
    // 46: the configured theme.
    {
        let t = crate::term_lock(&session.term);
        assert_eq!(
            (t.default_background(), t.default_foreground()),
            (themed_bg, themed_fg),
            "{at}: THE HELD PANE IS DRAWN IN THIS BUILD'S THEME"
        );
    }
    // 35: its user identity.
    {
        let meta = session
            .ctx
            .meta
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        assert_eq!(
            (
                meta.user_title.as_deref(),
                meta.icon.as_deref(),
                meta.role.as_deref()
            ),
            (Some("build log"), Some("hammer"), Some("ci")),
            "{at}: THE NAME THE PERSON GAVE IT CROSSED"
        );
    }
    assert_eq!(
        session.identity.as_deref(),
        Some("worker"),
        "{at}: and its spawn identity"
    );

    assert_eq!(
        new.store
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .by_local(restored)
            .map(|handle| handle.state),
        Some(crate::session_store::SessionState::Exited),
        "{at}: the pane is exited, as it was"
    );
    assert!(
        !new.handoff_live_sessions()
            .iter()
            .any(|(id, _, _)| *id == restored),
        "{at}: and the next update does not hand it"
    );
    // Typing into it goes nowhere, harmlessly.
    assert!(
        session.ctx.sink.write_frame(b"ls\r").is_err(),
        "{at}: a keystroke reaches no descriptor"
    );
    assert!(
        screen_text(&crate::term_lock(&session.term)).contains("build finished"),
        "{at}: and changes nothing on it"
    );
    // Closing it closes it.
    assert!(new.close_session_by_id(restored).is_ok(), "{at}: it closes");
    assert!(new.pool.get(restored).is_none(), "{at}: and is gone");

    ptys.release(app);
    let _ = std::fs::remove_dir_all(&scratch);
}

/// The CONTROL for [`a_held_exited_pane_keeps_its_screen`]: the same held
/// pane with NO screen carried — an older producer, whose manifest has no
/// `held` key — is the placeholder it always was, and the restore places
/// everything else exactly.
#[test]
fn an_older_producers_held_pane_is_still_its_placeholder() {
    let _env = ENV_LOCK.lock().unwrap_or_else(PoisonError::into_inner);
    let _restore = restore_env();
    let scratch = enter_scratch("held-pane-old");
    let mut app = crate::App::headless_for_test();
    app.hold = true;
    let wid = WindowId(0);
    let held_id = app.next_session_id;
    app.push_stub_tab(wid, crate::stub_session(held_id));
    let mut ptys = DeskPtys::default();
    ptys.attach_rest(&mut app);
    crate::term_lock(&app.pool.get(held_id).expect("held").term).process(b"build finished\r\n");
    app.store
        .write()
        .unwrap_or_else(PoisonError::into_inner)
        .set_state(held_id, crate::session_store::SessionState::Exited);
    let at = "an older producer";
    // The older producer's manifest: the same park, written with no held panes.
    let parked = app.park_desk_for_test(None).expect("the park");
    let adoption: Vec<(u64, i32, i32)> = parked
        .live
        .iter()
        .map(|(local_id, master, _)| {
            // SAFETY: duplicates a live, test-owned PTY master as a fresh
            // close-on-exec descriptor numbered 3 or above.
            let duplicate = unsafe { libc::fcntl(*master, libc::F_DUPFD_CLOEXEC, 3) };
            assert!(duplicate >= 3);
            (*local_id, duplicate, 40_000)
        })
        .collect();
    let nonce = mint_outgoing_nonce();
    let outgoing = write_outgoing(
        &parked.manifest,
        &HandoffFds {
            entries: adoption.clone(),
        },
        &parked.screens,
        &parked.repaint,
        parked.window.clone(),
        &[],
        &nonce,
    )
    .expect("the writer");
    let wire = std::fs::read_to_string(&outgoing.manifest_path).expect("the manifest");
    assert!(
        !wire.contains("[[held]]"),
        "{at}: PRECONDITION — no `held` key: {wire}"
    );
    let layout_path = std::path::Path::new(&outgoing.manifest_path).with_extension("layout.toml");
    crate::restore::write_once_to(&layout_path, &parked.layout).expect("the layout sidecar");
    let expected = adoption_proof(
        &nonce,
        crate::running_build_number(),
        crate::build_info::GIT_COMMIT,
        &parked.layout_digest.expect("digest"),
        &parked.screen_digest,
        &adoption,
    )
    .expect("the parent's expectation");
    let (_ready_read, ready_write) = pipe_pair("ready");
    let (commit_read, _commit_write) = pipe_pair("commit");
    // SAFETY: `getppid` is a side-effect-free libc getter.
    let parent_pid = unsafe { libc::getppid() };
    aterm_log::env::set(ENV_MANIFEST, &outgoing.manifest_path);
    aterm_log::env::set(ENV_NONCE, &outgoing.nonce);
    aterm_log::env::set(ENV_FDS, &outgoing.fds_wire);
    aterm_log::env::set(ENV_LAYOUT, &layout_path);
    aterm_log::env::set(ENV_READY_FD, ready_write.to_string());
    aterm_log::env::set(ENV_COMMIT_FD, commit_read.to_string());
    aterm_log::env::set(ENV_PARENT_PID, parent_pid.to_string());
    match read_process_birth(parent_pid) {
        Some(birth) => aterm_log::env::set(ENV_PARENT_BIRTH, birth.to_wire()),
        None => aterm_log::env::unset(ENV_PARENT_BIRTH),
    }
    aterm_log::env::set(
        ENV_TARGET,
        encode_target_identity(crate::running_build_number(), crate::build_info::GIT_COMMIT),
    );
    let incoming = take_incoming_as(ReceiverShape::Current);
    assert!(incoming.held.is_empty(), "{at}: nothing held crossed");
    let layout = incoming.layout.clone().expect("the layout is placed");
    let ((proof, ready, _), adopted) = child_proof_from(incoming).expect("adopts and proves");
    assert_eq!(proof, expected, "{at}: adopts exactly");
    drop((ready, adopted));
    let mut new = crate::App::headless_for_test();
    new.handoff_successor = true;
    new.pool
        .sessions
        .get_mut(&0)
        .expect("session 0")
        .session
        .handoff_local_id = Some(0);
    new.restore_into_window(wid, layout.windows[0].clone());
    let terminals = new.windows[&wid]
        .tab_set
        .tabs()
        .iter()
        .flat_map(|tab| tab.root.leaves())
        .filter(|view| {
            new.view_store
                .get(*view)
                .copied()
                .and_then(crate::tab_model::View::terminal_session)
                .is_some()
        })
        .count();
    assert_eq!(
        (new.windows[&wid].tab_set.tabs().len(), terminals),
        (2, 1),
        "{at}: the live pane and the placeholder, as before"
    );
    ptys.release(app);
    let _ = std::fs::remove_dir_all(&scratch);
}
