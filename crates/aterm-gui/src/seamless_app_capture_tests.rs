// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Whole-app capture property.

use std::sync::PoisonError;

use aterm_core::terminal::TerminalCheckpoint;
use aterm_window::dpi::PhysicalSize;

use super::ladder_tests::{assert_only_over_cap_links_dropped, hostile_walk_step, xorshift};
use super::tests::{
    ScratchEnv, child_proof_from, env_alone, pipe_pair, restore_env, run_in_children,
};
use super::*;
use crate::WindowId;

/// Publish the successor's handoff environment for `outgoing` as the fork
/// lane does: its manifest, nonce and fds, the layout sidecar at
/// `layout_path`, the two pipe ends, this process as the parent (with its birth
/// record where it has one) and `build` as the target.
fn publish_successor_env(
    outgoing: &OutgoingHandoff,
    layout_path: &std::path::Path,
    ready_write: i32,
    commit_read: i32,
    build: u64,
) {
    // SAFETY: `getppid` is a side-effect-free libc getter.
    let parent_pid = unsafe { libc::getppid() };
    aterm_log::env::set(ENV_MANIFEST, &outgoing.manifest_path);
    aterm_log::env::set(ENV_NONCE, &outgoing.nonce);
    aterm_log::env::set(ENV_FDS, &outgoing.fds_wire);
    aterm_log::env::set(ENV_LAYOUT, layout_path);
    aterm_log::env::set(ENV_READY_FD, ready_write.to_string());
    aterm_log::env::set(ENV_COMMIT_FD, commit_read.to_string());
    aterm_log::env::set(ENV_PARENT_PID, parent_pid.to_string());
    match read_process_birth(parent_pid) {
        Some(birth) => aterm_log::env::set(ENV_PARENT_BIRTH, birth.to_wire()),
        None => aterm_log::env::unset(ENV_PARENT_BIRTH),
    }
    aterm_log::env::set(
        ENV_TARGET,
        encode_target_identity(build, crate::build_info::git_commit()),
    );
}

/// A private scratch HOME and runtime dir for one desk ([`ScratchEnv`]), set before its App is
/// built, so nothing the App or the manifest writer touches lands in the real home.
fn enter_scratch(label: &str) -> ScratchEnv {
    // Every caller holds ENV_LOCK for its whole body.
    ScratchEnv::home(&format!("app-{label}"))
}

/// A PTY pair, both ends close-on-exec from birth: no child another test
/// spawns meanwhile holds either end past its exec.
fn openpty() -> (i32, i32) {
    aterm_pty::open_pty_pair().expect("open_pty_pair")
}

/// Every session of a desk on its own real, quiet PTY. The capture polls a
/// mid-sequence session's master for queued output (a stub's `-1` would read
/// as activity), and the manifest writer refuses a descriptor below 3.
#[derive(Default)]
struct DeskPtys {
    pairs: Vec<(u64, i32, i32)>,
}

impl DeskPtys {
    /// A new window's first session, on its PTY before the window is
    /// installed: `install_window_state` registers only a session with a real
    /// master, and a session missing from the registry is missing from the
    /// manifest, which the writer then refuses as inconsistent.
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
    /// record cap: the StrippedLinks rung.
    stripped_links: usize,
    history_lines: u64,
    /// The window carry the parent wrote, as the successor read it back.
    window: Option<crate::session_store::WindowCarry>,
    /// The screens as carried — and, asserted, as adopted.
    screens: Vec<(u64, TerminalCheckpoint)>,
    /// The history carry (`crate::handoff_history`), run as the fork lane's
    /// worker runs it: the records the join named a scrollback sidecar on…
    history_sidecars: usize,
    /// …the adopted sessions that took one (asserted: every named one)…
    history_adopted: usize,
    /// …and the scrollback lines the join counted as left behind, over every
    /// record (`history_dropped`)…
    history_dropped: u64,
    /// …of which the successor read as withheld by a handoff policy
    /// (`SessionRecord::history_withheld` → `AdoptedHistory::withheld`): the
    /// new version's request, not a failure.
    history_withheld: u64,
}

/// The park → write → adopt pipeline over a whole App, with every assertion
/// the property makes, labelled `at` (which names the seed).
fn hand_over(app: &mut crate::App, at: &str) -> Crossed {
    hand_over_as(app, at, None, Carried::Exact)
}

/// How a desk's screens must cross.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Carried {
    /// Every screen at an exact rung — the property of an honest desk.
    Exact,
    /// Every screen blank, marked for a redraw, with no control carry — what a
    /// successor's handoff policy of `carry = "repaint"` asks for.
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
    let controls =
        crate::handoff_carry::export(&parked.carries, crate::control::turn_ids_for_handoff());
    let mut manifest = parked.manifest.clone();
    manifest.next_turn_id =
        crate::handoff_carry::manifest_turn_id(crate::control::turn_ids_for_handoff());
    let adoption: Vec<(u64, i32, i32)> = parked
        .live
        .iter()
        .map(|(local_id, master, _)| {
            // SAFETY: duplicates a live, test-owned PTY master as a fresh close-on-exec descriptor numbered 3 or above.
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
    let dir = crate::control_auth::socket_dir().expect("the scratch control dir");
    let history_plan = format!("{:?}", parked.history);
    let verdicts = crate::handoff_history::stamp_manifest(
        &mut manifest,
        &parked.screens,
        &parked.history_heads,
        parked.history.results_for_test(&dir),
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
        crate::build_info::git_commit(),
        &layout_digest,
        &parked.screen_digest,
        &adoption,
    )
    .expect("the parent's expectation");

    let (ready_read, ready_write) = pipe_pair("ready");
    let (commit_read, commit_write) = pipe_pair("commit");
    publish_successor_env(&outgoing, &layout_path, ready_write, commit_read, build);
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
    drop(adopted);
    for fd in [ready_read, commit_read, commit_write] {
        aterm_pty::close_fd(fd);
    }
    crossed
}

/// Four real Mac displays
/// (`capture_budget_reservation_tests::every_full_screen_display_geometry_is_admitted_for_a_pool_of_panes`),
/// in physical pixels at 2x.
const DISPLAYS: [(u32, u32); 4] = [(3024, 1964), (3456, 2234), (5120, 2880), (6016, 3384)];

/// What a maximized window gives up on those displays: a 37 pt notched menu
/// bar and a 70 pt Dock, at 2x.
const MAXIMIZED_CHROME_PX: u32 = 74 + 140;

/// The font sizes a desk is sampled at. Below 8 px, three full-screen 6K
/// windows (about 1.4 Mi visible+alt cells each at 6 px) outgrow the 4 Mi
/// aggregate ceiling; this property is about desks the wire carries exactly,
/// and a pool past the ceiling is carried blank on purpose
/// (`a_pool_over_the_aggregate_lowers_its_largest_session`).
const FONT_PX: std::ops::RangeInclusive<u32> = 8..=48;

/// The largest pool a desk is built with.
const MAX_SESSIONS: usize = 12;

/// Give window `wid` a real chrome (pad 24, pad_top 4, head 80 at scale 2) at
/// `font_px`, then size it to `size` through the GUI's resize path
/// (`App::on_resize` → the grid law → every pane).
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

/// A real chrome (pad 24, pad_top 4, head 80 at scale 2) on window `wid`, at
/// `font_px`: what the grid law reads.
fn set_chrome(app: &mut crate::App, wid: WindowId, font_px: u32) {
    let ws = app.windows.get_mut(&wid).expect("the window");
    ws.metrics.pad = 24;
    ws.metrics.pad_top = 4;
    ws.metrics.head = 80;
    ws.metrics.font_px = font_px as f32;
}

/// Case 0's window: a 99x338 grid at the park, with the band at
/// `capture_band` rows, on the 5K display at the largest font that fits that
/// grid there: the display's own width where it gives 338 columns (24 px of the
/// headless macOS cell box, 15x28 px, which is not CoreText's, so the display's
/// full-screen height alone gives 101), else the narrowest window that does (a
/// face whose cell is taller for its width, as Linux's bundled one is, fits 99
/// rows of 338 columns only at a smaller font).
fn field_report_window(
    app: &mut crate::App,
    wid: WindowId,
    capture_band: u16,
) -> (u32, PhysicalSize<u32>) {
    let band = app.message_band_rows;
    app.message_band_rows = capture_band;
    let (display_width, display_height) = DISPLAYS[2];
    let smallest = crate::FONT_PX_MIN as u32;
    let found = (smallest..=48)
        .rev()
        .find_map(|font_px| {
            set_chrome(app, wid, font_px);
            let cols = |width| {
                app.grid_dims_for(wid, PhysicalSize::new(width, display_height))
                    .1
            };
            let width = if cols(display_width) == 338 {
                display_width
            } else {
                (1..=display_width).find(|width| cols(*width) == 338)?
            };
            let height = (1..=display_height).find(|height| {
                app.grid_dims_for(wid, PhysicalSize::new(width, *height)) == (99, 338)
            })?;
            Some((font_px, PhysicalSize::new(width, height)))
        })
        .expect("PRECONDITION: some font makes a 99x338 window on the 5K display");
    set_chrome(app, wid, found.0);
    app.message_band_rows = band;
    found
}

/// The message band's committed rows move to `rows`, and every window re-grids
/// by the one law, as `App::sync_message_band_rows` and
/// `App::regrid_for_chrome_rows` do (the latter's per-window `on_resize`
/// spelled out, since a headless window has no surface for it to find).
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
/// it toggled).
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

/// Prints the replay line if a desk fails, from wherever it panicked. A case
/// number, not a bare seed, replays it: the seed derives from the number, and
/// case 0 is also the pinned 99x338 window.
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

/// What the capture property's desks came to: the handovers' census and the
/// states the walk reached, summed over the cases a process ran.
#[derive(Default)]
struct DeskCensus {
    sessions: usize,
    exact: usize,
    stripped_links: usize,
    history_lines: u64,
    mid_sequence: usize,
    alternate: usize,
    slot_past_grid: usize,
}

impl DeskCensus {
    /// The line a shard prints its census on, for the parent to sum.
    const PREFIX: &'static str = "app-capture census:";

    fn line(&self) -> String {
        format!(
            "{} {} {} {} {} {} {} {}",
            Self::PREFIX,
            self.sessions,
            self.exact,
            self.stripped_links,
            self.history_lines,
            self.mid_sequence,
            self.alternate,
            self.slot_past_grid
        )
    }

    /// The census on a shard's `line`, or `None` if `line` carries none. The
    /// census follows the runner's own `test <name> ... ` on that line: with
    /// `--nocapture` a test's output lands between the name and its verdict.
    fn parse(line: &str) -> Option<Self> {
        let fields: Vec<u64> = line
            .split_once(Self::PREFIX)?
            .1
            .split_whitespace()
            .map(|field| field.parse().ok())
            .collect::<Option<_>>()?;
        let &[
            sessions,
            exact,
            stripped,
            history_lines,
            mid,
            alternate,
            slot,
        ] = fields.as_slice()
        else {
            return None;
        };
        let count = |n: u64| usize::try_from(n).ok();
        Some(Self {
            sessions: count(sessions)?,
            exact: count(exact)?,
            stripped_links: count(stripped)?,
            history_lines,
            mid_sequence: count(mid)?,
            alternate: count(alternate)?,
            slot_past_grid: count(slot)?,
        })
    }

    fn add(&mut self, other: &Self) {
        self.sessions += other.sessions;
        self.exact += other.exact;
        self.stripped_links += other.stripped_links;
        self.history_lines += other.history_lines;
        self.mid_sequence += other.mid_sequence;
        self.alternate += other.alternate;
        self.slot_past_grid += other.slot_past_grid;
    }
}

/// One desk of the capture property: case `case`'s seeded desk built, walked,
/// parked and handed over, every assertion made, its census added to `census`.
fn app_capture_case(case: usize, census: &mut DeskCensus) {
    const BASE_SEED: u64 = 0x00A7_E5C0_91D6_5EED;

    let mut z = BASE_SEED.wrapping_add(0x9E37_79B9_7F4A_7C15_u64.wrapping_mul(case as u64 + 1));
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    let seed = (z ^ (z >> 31)) | 1;
    let _replay = ReplayOnPanic { case, seed };
    let _scratch = enter_scratch(&case.to_string());
    let (desk, mut next) = build_desk(seed, case == 0);
    let Desk {
        mut app,
        ptys,
        sizes,
        shape,
    } = desk;
    let at = format!("case {case} (seed {seed:#018x}: {shape})");

    let mut ids: Vec<u64> = app.pool.iter().map(|session| session.id).collect();
    ids.sort_unstable();
    for id in &ids {
        let term = app.pool.get(*id).expect("pooled").term.clone();
        let mut t = crate::term_lock(&term);
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
    for (wid, _) in &sizes {
        app.resize_panes(*wid);
    }
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
        census.mid_sequence += usize::from(t.partial_sequence_state().is_some());
        census.alternate += usize::from(t.has_inactive_grid());
        let checkpoint = t.checkpoint_carry_abandoning_partial(0).0;
        census.slot_past_grid += usize::from(
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
    census.sessions += crossed.sessions;
    census.exact += crossed.exact;
    census.stripped_links += crossed.stripped_links;
    census.history_lines += crossed.history_lines;
    ptys.release(app);
}

/// Every desk the GUI can build hands every session over exactly. Forty seeded desks, each a
/// real headless App: 1 to 3 windows of 1 to 4 tabs with random splits (at most twelve
/// sessions), every window sized by the grid law from the display table (case 0 is a 99x338
/// window on the 5K display), every pane on a real PTY.
#[test]
fn every_app_desk_the_gui_can_build_hands_every_session_over_exactly() {
    const CASES: usize = 40;
    const SHARDS: usize = 4;
    const SHARD: &str = "ATERM_APP_CAPTURE_SHARD";

    let replay = std::env::var("ATERM_APP_CAPTURE_CASE").ok().map(|text| {
        text.trim()
            .parse::<usize>()
            .expect("ATERM_APP_CAPTURE_CASE is a case number")
    });
    let shard = std::env::var(SHARD)
        .ok()
        .map(|text| text.parse::<usize>().expect("the shard is a number"));
    let started = std::time::Instant::now();
    let mut total = DeskCensus::default();
    if replay.is_some() || shard.is_some() {
        let cases: Vec<usize> = match (replay, shard) {
            (Some(case), _) => vec![case],
            (None, Some(shard)) => (shard..CASES).step_by(SHARDS).collect(),
            (None, None) => unreachable!(),
        };
        let Some(_env) = env_alone() else { return };
        let _restore = restore_env();
        for case in cases {
            app_capture_case(case, &mut total);
        }
        if shard.is_some() {
            println!("{}", total.line());
        }
    } else {
        for stdout in run_in_children(SHARD, SHARDS) {
            let census = stdout
                .lines()
                .find_map(DeskCensus::parse)
                .expect("every shard prints its census");
            total.add(&census);
        }
    }
    let DeskCensus {
        sessions,
        exact,
        stripped_links,
        history_lines,
        mid_sequence,
        alternate,
        slot_past_grid,
    } = total;
    eprintln!(
        "whole-App capture: {sessions} session(s) handed over in {:?} — {exact} exact, \
         {stripped_links} link-stripped, {history_lines} history line(s); {mid_sequence} \
         mid-sequence, {alternate} with an inactive grid, {slot_past_grid} with a DECSC slot \
         past the grid",
        started.elapsed()
    );
    if replay.is_none() && shard.is_none() {
        assert!(
            stripped_links > 0
                && history_lines > 0
                && mid_sequence > 0
                && alternate > 0
                && slot_past_grid > 0,
            "the property must reach the states it was written for: {stripped_links} \
             link-stripped, {history_lines} history line(s), {mid_sequence} mid-sequence, \
             {alternate} alternate, {slot_past_grid} slot(s) past the grid"
        );
    }
}

/// A pinned desk: one window, two tabs at 56x149 by the GUI's grid law. Tab 1 is a shell with
/// 400 lines of styled history; tab 0 is Claude Code, entered from a prompt on the last row, so
/// 1049's DECSC slot is row 55.
#[test]
fn the_owners_desk_hands_both_sessions_over_at_the_full_rung() {
    let Some(_env) = env_alone() else { return };
    let _restore = restore_env();
    let _scratch = enter_scratch("owner");
    let mut app = crate::App::headless_for_test();
    let wid = WindowId(0);
    let id = app.next_session_id;
    app.push_stub_tab(wid, crate::stub_session(id));
    assert_eq!(app.pool.iter().count(), 2, "two tabs, one session each");

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
        claude_carry
            .saved_cursor_main
            .map(|saved| (saved.cursor_row, saved.cursor_col)),
        Some((54, 31)),
        "{at}: the Claude Code tab's DECSC slot rode the band's shrink up with its \
         prompt — row 54 of a 55-row grid, after `% claude`"
    );
    let shell_carry = carried(1);
    assert_eq!(
        (shell_carry.rows, shell_carry.history_lines),
        (55, MAX_HANDOFF_HISTORY_LINES),
        "{at}: the shell crosses with the full 256 lines of history the wire carries"
    );
    ptys.release(app);
}

/// Seed the App's pre-verification cache with a passed check of `attempt`'s
/// candidate whose bundle carried the handoff policy `text`, through the
/// handoff worker's own publisher (`PreverifyPublisher`, built from the
/// attempt and narrowed by the same `adopt`), and return what the park will
/// follow.
fn seed_passed_preverification(
    app: &crate::App,
    attempt: &crate::native_updater_service::ApplyAttemptTicket,
    text: &str,
) -> Option<aterm_update_core::handoff_policy::HandoffPolicy> {
    use aterm_update_core::handoff_policy::{PolicyFile, PolicyRead};
    let read = PolicyRead::Parsed(PolicyFile::parse(text).expect("a schema-1 policy"));
    let published = app.publish_preverified_pass_for_test(attempt, &read);
    assert_eq!(
        app.park_policy(Some(attempt)).policy,
        published,
        "the park reads what the worker published, by the attempt's key"
    );
    published
}

/// The successor's signed handoff policy steers the whole park, the escape hatch for a producer
/// bug the carry ladder cannot see.
#[test]
fn a_successor_policy_of_repaint_carries_every_session_blank_and_every_one_adopts() {
    let Some(_env) = env_alone() else { return };
    let _restore = restore_env();
    let _scratch = enter_scratch("policy");
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
    let commit = crate::build_info::git_commit();
    let artifact = "ab".repeat(32);
    let ticket =
        crate::native_updater_service::ApplyAttemptTicket::for_test(build, commit, &artifact);
    let attempt = Some(&ticket);

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
    let parked = aterm_core::fault::with_armed("checkpoint.carry_projection", || {
        app.park_desk_for_test(attempt)
    })
    .unwrap_or_else(|failure| panic!("{at}: the repaint park failed: {failure}"));
    let mut repainted = parked.repaint.clone();
    repainted.sort_unstable();
    assert_eq!(repainted, ids, "{at}: every session blank, none projected");

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
}

/// How the held panes' scrollback was offered in [`hand_over_held`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum HeldPlan {
    /// The park's own export, as the worker takes it.
    Export,
    /// None: the attempt hands to a successor older than this build, which
    /// takes a held pane's screen alone (by design).
    OlderSuccessor,
    /// None: the successor's policy carries no scrollback (by request).
    Withheld,
    /// None, though one was offered: the export never reached the panes (it
    /// could not start, or had not got to them) — a failure.
    NotExported,
}

/// One park of `app` written out as the worker writes it (the held panes'
/// screens included) and read back by a successor of `shape`: what it made
/// of the handoff, and the proof the parent waits for.
fn hand_over_held(
    app: &mut crate::App,
    shape: ReceiverShape,
    plan: HeldPlan,
    at: &str,
) -> (
    crate::app_update_handoff::ParkedDeskForTest,
    IncomingHandoff,
    String,
    AdoptionProof,
) {
    let build = crate::running_build_number();
    let mut parked = app
        .park_desk_for_test(None)
        .unwrap_or_else(|failure| panic!("{at}: the park refused: {failure}"));
    let layout_digest = parked.layout_digest.expect("the layout commits");
    let exported = std::mem::replace(
        &mut parked.history,
        crate::handoff_history::HistoryPlan::Unexported,
    );
    let held_history = match plan {
        HeldPlan::Export => exported,
        HeldPlan::Withheld => crate::handoff_history::HistoryPlan::Withheld,
        HeldPlan::OlderSuccessor | HeldPlan::NotExported => {
            crate::handoff_history::HistoryPlan::Unexported
        }
    }
    .results_for_test(&crate::control_auth::socket_dir().expect("the scratch control dir"))
    .take_held(
        &parked
            .held
            .iter()
            .map(|pane| pane.local_id)
            .collect::<Vec<_>>(),
        plan == HeldPlan::OlderSuccessor,
    );
    let adoption: Vec<(u64, i32, i32)> = parked
        .live
        .iter()
        .map(|(local_id, master, _)| {
            // SAFETY: duplicates a live, test-owned PTY master as a fresh close-on-exec descriptor numbered 3 or above.
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
    let controls =
        crate::handoff_carry::export(&parked.carries, crate::control::turn_ids_for_handoff());
    let outgoing = write_outgoing_with_held(
        &parked.manifest,
        &fds,
        &parked.screens,
        &parked.repaint,
        parked.window.clone(),
        &controls,
        &parked.held,
        held_history,
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
        crate::build_info::git_commit(),
        &layout_digest,
        &parked.screen_digest,
        &adoption,
    )
    .expect("the parent's expectation");
    let (_ready_read, ready_write) = pipe_pair("ready");
    let (commit_read, _commit_write) = pipe_pair("commit");
    publish_successor_env(&outgoing, &layout_path, ready_write, commit_read, build);
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

/// A pane kept open by `--hold` after its command exited keeps its final
/// screen across an update while another session is live.
#[test]
fn a_held_exited_pane_keeps_its_screen() {
    let Some(_env) = env_alone() else { return };
    let _restore = restore_env();
    let _scratch = enter_scratch("held");
    let mut app = crate::App::headless_for_test();
    app.hold = true;
    let wid = WindowId(0);
    let held_id = app.next_session_id;
    app.push_stub_tab(wid, crate::stub_session(held_id));
    let mut ptys = DeskPtys::default();
    ptys.attach_rest(&mut app);
    let scrollback;
    let held_sid = app
        .pool
        .get(held_id)
        .expect("the held pane")
        .ctx
        .self_id
        .clone();
    {
        let held = app.pool.get(held_id).expect("the held pane").term.clone();
        let mut t = crate::term_lock(&held);
        for line in 0..100 {
            t.process(format!("compile {line}\r\n").as_bytes());
        }
        t.process(b"\x1b]0;make\x07$ make\r\n");
        t.process(b"see \x1b]8;;https://example.com/log\x07the log\x1b]8;;\x07\r\n");
        t.process(b"build finished\r\n");
        scrollback = t.main_grid().scrollback_lines();
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
    app.store
        .write()
        .unwrap_or_else(PoisonError::into_inner)
        .set_state(held_id, crate::session_store::SessionState::Exited);
    {
        let pooled = &mut app
            .pool
            .sessions
            .get_mut(&held_id)
            .expect("the held pane")
            .session;
        let _ = pooled.child_exit.set(aterm_pty::ChildExit::Code(2));
        pooled
            .child_reaped
            .store(true, std::sync::atomic::Ordering::Release);
        let mut meta = pooled
            .ctx
            .meta
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        let _ = meta.set("title", Some("build log".to_string()));
        let _ = meta.set("icon", Some("hammer".to_string()));
        let _ = meta.set("role", Some("ci".to_string()));
    }

    let at = "an older successor";
    let (parked, old, nonce, expected) =
        hand_over_held(&mut app, ReceiverShape::PreCarry, HeldPlan::Export, at);
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
    assert_eq!(
        attempt_files(&nonce, ".hist").len(),
        1,
        "{at}: and the held pane's scrollback sidecar unread"
    );
    retire_outgoing_controls(&nonce);
    assert!(
        attempt_files(&nonce, HELD_GRID_SUFFIX).is_empty()
            && attempt_files(&nonce, ".hist").is_empty(),
        "{at}: and the parent retires them once the proof checks out"
    );

    let at = "this build's successor";
    let (_parked, mut incoming, nonce, expected) =
        hand_over_held(&mut app, ReceiverShape::Current, HeldPlan::Export, at);
    assert!(
        attempt_files(&nonce, HELD_GRID_SUFFIX).is_empty()
            && attempt_files(&nonce, ".hist").is_empty(),
        "{at}: the successor took the held sidecars, its scrollback's too"
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
    assert!(scrollback > 0, "PRECONDITION: the held pane has scrollback");
    assert_eq!(
        (
            held[0]
                .history
                .carry
                .as_ref()
                .map(crate::handoff_history::HistoryCarry::take),
            held[0].history.dropped
        ),
        (Some(u64::try_from(scrollback).expect("a count")), 0),
        "{at}: ITS SCROLLBACK CROSSES, every line of it (round eight, finding 39)"
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
    {
        let t = crate::term_lock(&session.term);
        assert_eq!(
            (t.default_background(), t.default_foreground()),
            (themed_bg, themed_fg),
            "{at}: THE HELD PANE IS DRAWN IN THIS BUILD'S THEME"
        );
    }
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
        session.ctx.self_id, held_sid,
        "{at}: THE HELD PANE ANSWERS TO THE SID IT HAD"
    );
    assert_eq!(
        new.store
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .by_sid(&held_sid)
            .map(|handle| handle.local_id),
        Some(restored),
        "{at}: `@<sid>` resolves to the restored pane"
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
    assert!(
        session.ctx.sink.write_frame(b"ls\r").is_err(),
        "{at}: a keystroke reaches no descriptor"
    );
    assert!(
        screen_text(&crate::term_lock(&session.term)).contains("build finished"),
        "{at}: and changes nothing on it"
    );
    let scrollback = u64::try_from(scrollback).expect("a count");
    let loss = crate::handoff_history::carried_loss(
        new.pool
            .iter()
            .filter_map(|session| session.handoff_history.as_ref()),
    );
    assert_eq!(
        loss,
        crate::handoff_history::LossSummary::default(),
        "{at}: nothing is left behind"
    );
    new.say_carried_history_loss();
    assert!(
        !new.has_live_message("Update left scrollback behind")
            && !new.has_live_message("Couldn't carry all scrollback"),
        "{at}: and nothing is said"
    );
    let reports = crate::handoff_history::run_imports(new.pool.take_handoff_histories());
    assert_eq!(
        reports
            .iter()
            .map(|report| (report.session, report.imported, report.lost()))
            .collect::<Vec<_>>(),
        vec![(restored, scrollback, 0)],
        "{at}: the held pane's import puts every line back"
    );
    {
        let t = crate::term_lock(&new.pool.get(restored).expect("pooled").term);
        let grid = t.main_grid();
        assert_eq!(
            u64::try_from(grid.scrollback_lines()).expect("a count"),
            scrollback,
            "{at}: THE HELD PANE HAS ITS SCROLLBACK BACK"
        );
        assert_eq!(
            grid.get_history_line(0)
                .map(|line| line.to_string().trim_end().to_string()),
            Some("compile 0".to_string()),
            "{at}: from its oldest line"
        );
    }
    assert_eq!(
        new.store
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .history_lost(restored),
        0,
        "{at}: status counts nothing lost"
    );
    assert!(new.close_session_by_id(restored).is_ok(), "{at}: it closes");
    assert!(new.pool.get(restored).is_none(), "{at}: and is gone");

    for (plan, held, withheld, held_failed, title, words, not_said) in [
        (
            HeldPlan::OlderSuccessor,
            scrollback,
            0,
            0,
            "Update left scrollback behind",
            "kept open after their command ended",
            "Couldn't carry all scrollback",
        ),
        (
            HeldPlan::Withheld,
            0,
            scrollback,
            0,
            "Update left scrollback behind",
            "the new version asked this update to carry no scrollback",
            "Couldn't carry all scrollback",
        ),
        (
            HeldPlan::NotExported,
            0,
            0,
            scrollback,
            "Couldn't carry all scrollback",
            "which this update failed to carry",
            "came across",
        ),
    ] {
        let at = format!("a park with no scrollback export ({plan:?})");
        let (_parked, mut incoming, nonce, _expected) =
            hand_over_held(&mut app, ReceiverShape::Current, plan, &at);
        let held_panes = std::mem::take(&mut incoming.held);
        assert_eq!(
            (
                held_panes[0].history.carry.is_none(),
                held_panes[0].history.dropped
            ),
            (true, scrollback),
            "{at}: every line is counted"
        );
        assert!(
            attempt_files(&nonce, ".hist").is_empty(),
            "{at}: and no sidecar is named"
        );
        let layout = incoming.layout.clone().expect("the layout is placed");
        let ((_, ready, _), adopted) = child_proof_from(incoming).expect("the successor proves");
        drop((ready, adopted));
        let mut new = crate::App::headless_for_test();
        new.handoff_successor = true;
        new.pool
            .sessions
            .get_mut(&0)
            .expect("session 0")
            .session
            .handoff_local_id = Some(0);
        new.seamless_held = held_panes;
        new.restore_into_window(wid, layout.windows[0].clone());
        let loss = crate::handoff_history::carried_loss(
            new.pool
                .iter()
                .filter_map(|session| session.handoff_history.as_ref()),
        );
        assert_eq!(
            loss,
            crate::handoff_history::LossSummary {
                lines: scrollback,
                tabs: 1,
                withheld,
                held,
                held_failed,
            },
            "{at}: the held pane's scrollback is counted, by why"
        );
        new.say_carried_history_loss();
        assert!(
            new.has_live_message(title) && new.has_live_message(words),
            "{at}: the band says why"
        );
        assert!(!new.has_live_message(not_said), "{at}: and nothing false");
        assert!(
            !new.has_live_message("came across"),
            "{at}: never that the newest lines crossed"
        );
        let reports = crate::handoff_history::run_imports(new.pool.take_handoff_histories());
        assert_eq!(
            crate::handoff_history::loss_summary(&reports),
            loss,
            "{at}: the settle says what the Commit said"
        );
    }

    ptys.release(app);
}

/// A HELD PANE'S TURN LEDGER CROSSES. A pane a
/// manager drove with `turn` before its command ended keeps the records, so
/// `history` lists them (`carried=1`) and the report reader finds them: they
/// ride the pane's own control sidecar, read before the proof as a handed
/// session's is.
#[test]
fn a_held_panes_turn_ledger_crosses_the_update() {
    let Some(_env) = env_alone() else { return };
    let _restore = restore_env();
    let _scratch = enter_scratch("held-ledger");
    let mut app = crate::App::headless_for_test();
    app.hold = true;
    let wid = WindowId(0);
    let held_id = app.next_session_id;
    app.push_stub_tab(wid, crate::stub_session(held_id));
    let mut ptys = DeskPtys::default();
    ptys.attach_rest(&mut app);
    crate::term_lock(&app.pool.get(held_id).expect("the held pane").term)
        .process(b"$ claude\r\nbuild finished\r\n");
    app.pool
        .get(held_id)
        .expect("the held pane")
        .ctx
        .turns
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .push(crate::turn_ledger::TurnRecord {
            id: 7,
            started_ms: 1,
            dur_ms: 2,
            submitted: true,
            status: "settled",
            text: "run the build".into(),
            screen_hash: 9,
            seq: 3,
            arch: crate::turn_ledger::ArchMark::default(),
            by: crate::turn_ledger::TurnBy::Ctl,
            carried: false,
        });
    app.store
        .write()
        .unwrap_or_else(PoisonError::into_inner)
        .set_state(held_id, crate::session_store::SessionState::Exited);

    let at = "this build's successor";
    let (_parked, mut incoming, nonce, _expected) =
        hand_over_held(&mut app, ReceiverShape::Current, HeldPlan::Export, at);
    assert!(
        attempt_files(&nonce, &format!(".h{held_id}.ctl")).is_empty(),
        "{at}: the successor took the held pane's control sidecar"
    );
    let mut held = std::mem::take(&mut incoming.held);
    assert_eq!(
        held.iter().map(|pane| pane.local_id).collect::<Vec<_>>(),
        vec![held_id],
        "{at}: the held pane's screen crossed"
    );
    assert_eq!(
        held[0].control.as_ref().map(|control| control
            .turns
            .iter()
            .map(|t| t.id)
            .collect::<Vec<_>>()),
        Some(vec![7]),
        "{at}: AND ITS TURN LEDGER WITH IT"
    );
    assert!(
        crate::control::turn_ids_minted() >= 7,
        "{at}: the turn-id count goes on above the carried record"
    );
    let layout = incoming.layout.clone().expect("the layout is placed");
    let ((_, ready, _), adopted) = child_proof_from(incoming).expect("the successor proves");
    drop((ready, adopted));
    let restore = |held: Vec<crate::seamless::HeldPane>| {
        let mut new = crate::App::headless_for_test();
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
        let restored = new
            .pool
            .iter()
            .find(|session| session.id != 0)
            .map(|session| session.id)
            .expect("the held pane's terminal");
        let history = crate::control::cmd_history(&new.pool.get(restored).expect("pooled").ctx, "");
        drop(new);
        history
    };
    let history = restore(std::mem::take(&mut held));
    assert!(
        history.starts_with("OK 1\nturn 7 ") && history.contains("carried=1"),
        "{at}: `history` lists the turn a manager drove into the pane: {history:?}"
    );

    let at = "a held pane whose control carry did not come";
    let (_parked, mut incoming, _nonce, _expected) =
        hand_over_held(&mut app, ReceiverShape::Current, HeldPlan::Export, at);
    let mut held = std::mem::take(&mut incoming.held);
    held[0].control = None;
    let ((_, ready, _), adopted) = child_proof_from(incoming).expect("the successor proves");
    drop((ready, adopted));
    let history = restore(held);
    assert!(
        history.starts_with("OK 0") && history.contains("unheld_below="),
        "{at}: it says what it cannot vouch for: {history:?}"
    );
}

/// The control for [`a_held_exited_pane_keeps_its_screen`]: the same held
/// pane with no screen carried (a manifest with no `held` key) is a
/// placeholder, and the restore places everything else exactly.
#[test]
fn an_older_producers_held_pane_is_still_its_placeholder() {
    let Some(_env) = env_alone() else { return };
    let _restore = restore_env();
    let _scratch = enter_scratch("held-old");
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
    let parked = app.park_desk_for_test(None).expect("the park");
    let adoption: Vec<(u64, i32, i32)> = parked
        .live
        .iter()
        .map(|(local_id, master, _)| {
            // SAFETY: duplicates a live, test-owned PTY master as a fresh close-on-exec descriptor numbered 3 or above.
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
        crate::build_info::git_commit(),
        &parked.layout_digest.expect("digest"),
        &parked.screen_digest,
        &adoption,
    )
    .expect("the parent's expectation");
    let (_ready_read, ready_write) = pipe_pair("ready");
    let (commit_read, _commit_write) = pipe_pair("commit");
    publish_successor_env(
        &outgoing,
        &layout_path,
        ready_write,
        commit_read,
        crate::running_build_number(),
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
}

/// A held pane's failure the person had seen stays seen across an update, and its exit is not
/// replayed as news: the producer carries whether the failure was seen
/// (`HeldPaneCarry::failure_seen`), and the successor records the carried exit as the status it
/// is, not a `status-change` event.
#[test]
fn a_held_panes_seen_failure_stays_seen_across_the_update() {
    let Some(_env) = env_alone() else { return };
    let _restore = restore_env();
    let _scratch = enter_scratch("held-seen");
    let mut app = crate::App::headless_for_test();
    app.hold = true;
    let wid = WindowId(0);
    let held_id = app.next_session_id;
    app.push_stub_tab(wid, crate::stub_session(held_id));
    let mut ptys = DeskPtys::default();
    ptys.attach_rest(&mut app);
    crate::term_lock(&app.pool.get(held_id).expect("the held pane").term).process(
        b"\x1b]133;A\x07$ \x1b]133;B\x07make\r\n\x1b]133;C\x07error: build failed\r\n\
          \x1b]133;D;2\x07\x1b]133;A\x07$ ",
    );
    let t0 = std::time::Instant::now();
    app.observe_session_statuses(t0);
    app.observe_session_statuses(t0 + std::time::Duration::from_secs(1));
    assert_eq!(
        app.session_status
            .status(held_id)
            .map(|status| status.last_outcome),
        Some(crate::session_status::Outcome::Failure { exit_code: 2 }),
        "PRECONDITION: the live sweep read the failed block"
    );
    app.store
        .write()
        .unwrap_or_else(PoisonError::into_inner)
        .set_state(held_id, crate::session_store::SessionState::Exited);
    {
        let pooled = &mut app
            .pool
            .sessions
            .get_mut(&held_id)
            .expect("the held pane")
            .session;
        let _ = pooled.child_exit.set(aterm_pty::ChildExit::Code(2));
        pooled
            .child_reaped
            .store(true, std::sync::atomic::Ordering::Release);
    }
    app.note_session_exit(held_id);
    assert!(
        !app.session_status.failure_seen(held_id),
        "PRECONDITION: a failure nobody has looked at"
    );
    let status_rows = |app: &crate::App, session: u64| {
        app.pool
            .get(session)
            .expect("pooled")
            .ctx
            .timeline
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .since(None)
            .filter(|event| event.kind == crate::session_status::STATUS_CHANGE)
            .count()
    };
    let successor_of = |app: &mut crate::App, at: &str| {
        let (_parked, mut incoming, _nonce, _expected) =
            hand_over_held(app, ReceiverShape::Current, HeldPlan::Export, at);
        let held = std::mem::take(&mut incoming.held);
        let layout = incoming.layout.clone().expect("the layout is placed");
        let ((_, ready, _), adopted) = child_proof_from(incoming).expect("the successor proves");
        drop((ready, adopted));
        let carried_seen = held
            .iter()
            .map(|pane| pane.failure_seen)
            .collect::<Vec<_>>();
        let mut new = crate::App::headless_for_test();
        new.handoff_successor = true;
        new.pool
            .sessions
            .get_mut(&0)
            .expect("session 0")
            .session
            .handoff_local_id = Some(0);
        new.seamless_held = held;
        new.restore_into_window(wid, layout.windows[0].clone());
        let restored = new
            .window_terminal_sessions(wid)
            .into_iter()
            .find(|id| *id != 0)
            .expect("the held pane's terminal");
        (new, restored, carried_seen)
    };

    let at = "a failure nobody looked at";
    let (new, restored, carried_seen) = successor_of(&mut app, at);
    assert_eq!(carried_seen, vec![false], "{at}: nothing seen is carried");
    assert_eq!(
        new.session_status
            .status(restored)
            .map(|status| (status.phase, status.last_outcome)),
        Some((
            crate::session_status::Phase::Exited,
            crate::session_status::Outcome::Failure { exit_code: 2 }
        )),
        "{at}: exited, with its status, as before"
    );
    assert!(
        new.session_status
            .indicators(restored)
            .is_some_and(|bits| bits.status_attention),
        "{at}: the failure still marks its tab"
    );

    assert!(app.session_status.note_seen(held_id), "the mark goes quiet");
    assert!(
        app.session_status.failure_seen(held_id),
        "PRECONDITION: the failure is seen"
    );
    let at = "a failure the person had seen";
    let (new, restored, carried_seen) = successor_of(&mut app, at);
    assert_eq!(carried_seen, vec![true], "{at}: the producer carries it");
    assert_eq!(
        new.session_status
            .status(restored)
            .map(|status| (status.phase, status.last_outcome)),
        Some((
            crate::session_status::Phase::Exited,
            crate::session_status::Outcome::Failure { exit_code: 2 }
        )),
        "{at}: still exited, with its status"
    );
    assert!(
        new.session_status
            .indicators(restored)
            .is_some_and(|bits| !bits.status_attention),
        "{at}: THE SEEN FAILURE DOES NOT MARK THE TAB AGAIN"
    );
    assert!(
        new.session_status.failure_seen(restored),
        "{at}: and the next update carries it seen"
    );
    let mut new = new;
    assert!(
        crate::session_status::completed_block(&crate::term_lock(
            &new.pool.get(restored).expect("the held pane").term
        ))
        .is_some_and(|block| block.exit_code == Some(2)),
        "{at}: PRECONDITION: the held engine carries the failed command's block"
    );
    let swept_at = std::time::Instant::now() + std::time::Duration::from_secs(2);
    assert!(
        new.session_status.due(restored, swept_at),
        "{at}: PRECONDITION: the next sweep is due to read the held pane"
    );
    drop(
        new.pool
            .get(restored)
            .expect("the held pane")
            .term
            .try_lock()
            .expect("PRECONDITION: nothing holds the held engine, so the sweep is not refused"),
    );
    new.observe_session_statuses(swept_at);
    assert!(
        !new.session_status.due(restored, swept_at),
        "{at}: PRECONDITION: the sweep reached the held pane and read it"
    );
    assert!(
        new.session_status
            .indicators(restored)
            .is_some_and(|bits| !bits.status_attention),
        "{at}: STILL QUIET AFTER THE NEXT SWEEP, which reads the block again"
    );
    assert!(
        new.session_status.failure_seen(restored),
        "{at}: and still carried seen by the next update"
    );
    assert_eq!(
        status_rows(&new, restored),
        0,
        "{at}: NO `status-change` ROW FOR AN EXIT FROM BEFORE THE UPDATE"
    );
    ptys.release(app);
}

/// WHAT THE OWNER WAS TOLD AND HAS SEEN CROSSES THE UPDATE
/// (`SessionRecord::status_seen`): the park stamps each session's status item
/// memory on its manifest record (`App::status_carry_rows`), the shipping
/// writer writes it, and this build's successor reads it back by sid — the
/// asking session's told need, and the done its owner looked at, by when its
/// task entered it — and, on the manifest itself
/// (`SessionHandoff::fleet_told`), the fleet need its herald told.
#[test]
fn what_the_owner_was_told_and_has_seen_crosses_the_update() {
    use crate::status_item::StatusCarry;
    use crate::task_record::TaskState;
    let Some(_env) = env_alone() else { return };
    let _restore = restore_env();
    let _scratch = enter_scratch("status-seen");
    let mut app = crate::App::headless_for_test();
    let wid = WindowId(0);
    let quiet = app.next_session_id;
    app.push_stub_tab(wid, crate::stub_session(quiet));
    let mut ptys = DeskPtys::default();
    ptys.attach_rest(&mut app);
    let ctx = app.pool.get(0).expect("session 0").ctx.clone();
    for state in [TaskState::Working, TaskState::Done] {
        let mark = crate::task_record::TaskMark::new(state);
        crate::task_record::mark_session_task(&ctx, 0, &mark).expect("marked");
    }
    app.on_task_changed(0);
    app.windows
        .get_mut(&wid)
        .expect("window 0")
        .band_on_screen_for_test = true;
    app.switch_tab_in(wid, 0);
    app.note_done_seen_looking();
    app.switch_tab_in(wid, 1);
    app.windows
        .get_mut(&wid)
        .expect("window 0")
        .band_on_screen_for_test = false;
    ctx.meta
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .set("attention", Some("wedged on CI".to_string()));
    app.publish_need(0);
    app.headless = false;
    app.config.desktop_alerts = Some(true);
    crate::app_tabs::HERALD_REFUSAL.set(None);
    app.herald_session(0);
    let mac = crate::fleet_glance::FleetMac {
        name: "mac-17".to_string(),
        dial: "mac-17".to_string(),
        ..crate::fleet_glance::FleetMac::default()
    };
    let need = crate::fleet_glance::FleetNeed {
        sid: "s-w".to_string(),
        word: aterm_types::needs::NeedWord::Stalled,
        who: "claude".to_string(),
        worker_of: None,
        age_ms: None,
        what: None,
    };
    let fleet = crate::fleet_glance::escalation(&mac, &need);
    app.herald_deliver(fleet.session, Some(&fleet), false);
    app.fleet_heralded
        .insert(fleet.session, (mac.dial.clone(), need.word));
    app.headless = true;
    let since_ms = ctx
        .timeline
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .task()
        .expect("the task")
        .since_unix_ms;
    let sid_of = |app: &crate::App, id: u64| {
        app.store
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .by_local(id)
            .expect("registered")
            .sid
            .as_str()
            .to_string()
    };
    let (asking, untouched) = (sid_of(&app, 0), sid_of(&app, quiet));

    let at = "a told ask and a read done";
    let (_parked, incoming, _nonce, _expected) =
        hand_over_held(&mut app, ReceiverShape::Current, HeldPlan::Export, at);
    let carried: std::collections::HashMap<String, String> =
        incoming.status_seen.iter().cloned().collect();
    let carry = StatusCarry::parse(carried.get(&asking).expect("the asking session's row"));
    assert_eq!(
        carry.need.map(|(word, _)| word),
        Some(aterm_types::needs::NeedWord::Ask),
        "{at}"
    );
    assert!(carry.told, "{at}: told");
    assert_eq!(carry.done, Some(since_ms), "{at}: the done read");
    assert!(!carried.contains_key(&untouched), "{at}: nothing known");
    assert_eq!(
        incoming.fleet_told,
        vec![crate::fleet_glance::told_row(fleet.session, need.word)],
        "{at}: the told fleet need"
    );
    drop(incoming);
    ptys.release(app);
}

/// THE AUTOPILOT'S END RIDES THE PARK, THE WIRE AND THE INTAKE TO THE
/// SUCCESSOR'S GLASS (`docs/DESIGN-harness-autopilot-2026-10-15.md` §11.2.3,
/// item 8; the review of `c5af3a7507`): an end nobody saw, standing on
/// session 0's glass, is stamped by the test park (the fork lane's mirror,
/// `park_desk_for_test`), written by the shipping writer and read back by
/// the successor's real intake (`take_incoming_as`): the App's carry by sid
/// (`IncomingHandoff::end_carry`, `seamless::end_carry_of`) and the adopted
/// shell's row (`Adopted::autopilot_end`).
#[test]
fn the_autopilot_end_rides_the_park_and_the_intake_to_the_successor() {
    use crate::harness_end_carry::{CarriedEnd, EndCarry};
    use crate::harness_glass::EndFace;
    use aterm_agent::harness::acts::TaskFace;
    let Some(_env) = env_alone() else { return };
    let _restore = restore_env();
    let _scratch = enter_scratch("autopilot-end");
    let mut app = crate::App::headless_for_test();
    let wid = WindowId(0);
    let quiet = app.next_session_id;
    app.push_stub_tab(wid, crate::stub_session(quiet));
    let mut ptys = DeskPtys::default();
    ptys.attach_rest(&mut app);
    let at_ms: i64 = 1_760_978_557_123;
    let ctx = app.pool.get(0).expect("session 0").ctx.clone();
    ctx.timeline
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .set_task_face(Some(TaskFace::Landed), at_ms);
    crate::harness_glass::on_task(&mut app, 0, Some(TaskFace::Landed), at_ms);
    app.harness_glass
        .entry(0)
        .or_default()
        .tell_end(EndFace::Landed);
    assert!(app.end_unseen(0), "PRECONDITION: an end nobody saw");
    let sid_of = |app: &crate::App, id: u64| {
        app.store
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .by_local(id)
            .expect("registered")
            .sid
            .as_str()
            .to_string()
    };
    let (ended, untouched) = (sid_of(&app, 0), sid_of(&app, quiet));
    let at = "an unseen end";
    let (_parked, mut incoming, _nonce, _expected) =
        hand_over_held(&mut app, ReceiverShape::Current, HeldPlan::Export, at);

    let carried = EndCarry {
        end: Some(CarriedEnd {
            face: EndFace::Landed,
            at_ms,
            seen: false,
        }),
        worked_ms: 0,
    };
    let end_carry = std::mem::take(&mut incoming.end_carry);
    let mut adopted = std::mem::take(&mut incoming.adopted);
    drop(incoming);
    assert_eq!(
        end_carry.get(&ended),
        Some(&carried),
        "{at}: the App's carry"
    );
    assert!(
        !end_carry.contains_key(&untouched),
        "{at}: nothing for a shell"
    );
    let shell = crate::app_restore::take_handed_off_shell(&mut adopted, Some(0))
        .expect("session 0 adopted");
    drop(adopted);
    assert_eq!(shell.sid.as_str(), ended, "{at}: under its sid");
    assert_eq!(
        shell.autopilot_end.as_deref(),
        Some(format!("end=landed@{at_ms} seen=0 worked=0").as_str()),
        "{at}: the adopted shell's row"
    );

    let mut next = crate::App::headless_for_test();
    next.end_carry = end_carry;
    let id = next.next_session_id;
    crate::prime_test_timing();
    let session = crate::ptyless_session(
        id,
        shell.sid.clone(),
        aterm_core::terminal::Terminal::new(24, 80),
        std::sync::Arc::new(aterm_session::sink::SinkWriter::new(-1)),
    );
    *session
        .ctx
        .timeline
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .slog_mut() = crate::session_log::SessionLog::for_test();
    crate::spawn::seed_adopted_end(&session.ctx.timeline, shell.autopilot_end.as_deref());
    drop(shell);
    assert_eq!(
        session
            .ctx
            .timeline
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .task_face_at(TaskFace::Landed),
        Some(at_ms),
        "{at}: the face's time held for the loop's restate"
    );
    next.push_stub_tab(wid, session);
    assert!(
        !next.end_unseen(id),
        "PRECONDITION: not before its first publication"
    );
    next.refresh_presence_session(id);
    assert!(next.end_unseen(id), "{at}: the `•` crossed");
    assert!(next.end_carry.is_empty(), "{at}: spent once");
    ptys.release(app);
}
