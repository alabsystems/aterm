// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Runs the `aterm-control` verb matrix against a [`GuiHost`] with a live `EventLoopProxy` and verifies `Wake` redraw delivery.

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use aterm_control::conformance;
use aterm_control::selection::cmd_select;
use aterm_control::{HostCapabilities, SessionHost};
use aterm_core::terminal::Terminal;
use aterm_session::sink::SinkWriter;
use aterm_session::{EdgeTable, LaunchNonce, SessionId};
use aterm_window::application::ApplicationHandler;
use aterm_window::event::WindowEvent;
use aterm_window::event_loop::{ActiveEventLoop, EventLoop};
use aterm_window::platform::pump_events::{EventLoopExtPumpEvents, PumpStatus};
use aterm_window::window::WindowId as WinitWindowId;

use super::control_host::GuiHost;
use crate::session_store::{self, SessionHandle};
use crate::{SessionCtx, Wake, term_lock};

const PASS: i32 = 0;
const FAIL: i32 = 1;
const NOT_RUN: i32 = 2;

const SID: u64 = 0;
const SIBLING_SID: u64 = 1;

const WARMUP: Duration = Duration::from_millis(500);
const DELIVERY_BUDGET: Duration = Duration::from_secs(2);
const QUIET: Duration = Duration::from_millis(250);

/// Every `Wake` the loop delivered. The harness asserts on delivery, never on
/// the proxy having accepted the send.
#[derive(Default)]
struct DeliveredWakes {
    /// The `session` of each `Wake::Output` — what `request_redraw` posts.
    redraws: Vec<u64>,
    /// Anything else, counted so an unexpected wake shows up in the report.
    other: usize,
}

impl DeliveredWakes {
    fn redraws_for(&self, sid: u64) -> usize {
        self.redraws.iter().filter(|s| **s == sid).count()
    }
}

impl ApplicationHandler<Wake> for DeliveredWakes {
    fn resumed(&mut self, _event_loop: &ActiveEventLoop) {}

    fn window_event(&mut self, _: &ActiveEventLoop, _: WinitWindowId, _: WindowEvent) {}

    fn user_event(&mut self, _event_loop: &ActiveEventLoop, wake: Wake) {
        match wake {
            Wake::Output { session, .. } => self.redraws.push(session),
            _ => self.other += 1,
        }
    }
}

fn pump_for_redraws(
    event_loop: &mut EventLoop<Wake>,
    delivered: &mut DeliveredWakes,
    sid: u64,
    want: usize,
    budget: Duration,
) -> usize {
    let deadline = Instant::now() + budget;
    while delivered.redraws_for(sid) < want && Instant::now() < deadline {
        if matches!(
            event_loop.pump_app_events(Some(Duration::from_millis(5)), delivered),
            PumpStatus::Exit(_)
        ) {
            break;
        }
    }
    delivered.redraws_for(sid)
}

fn pump_until_quiet(
    event_loop: &mut EventLoop<Wake>,
    delivered: &mut DeliveredWakes,
    budget: Duration,
) {
    let deadline = Instant::now() + budget;
    while Instant::now() < deadline {
        if matches!(
            event_loop.pump_app_events(Some(Duration::from_millis(5)), delivered),
            PumpStatus::Exit(_)
        ) {
            break;
        }
    }
}

fn registered_session(local_id: u64, term: &Arc<Mutex<Terminal>>) -> SessionHandle {
    let sid = SessionId::generate();
    let nonce = LaunchNonce::generate();
    let observed = crate::session_watch::Observed::default();
    let ctx = Arc::new(SessionCtx {
        sink: Arc::new(SinkWriter::new(-1)),
        output_echo: Arc::new(crate::app_input::OutputEchoTracker::default()),
        modes: crate::mode_mirror_of(term),
        ui_waiting: Arc::default(),
        edges: std::sync::Arc::new(Mutex::new(EdgeTable::new())),
        turn_lease: Mutex::new(None),
        self_id: sid.clone(),
        nonce,
        cast: Arc::new(Mutex::new(crate::cast::CastRecorder::new(80, 24))),
        temporal: Arc::new(Mutex::new(crate::temporal::TemporalRecorder::new())),
        byte_fanout: Arc::new(crate::cast::ByteFanout::new()),
        turns: observed.turns,
        meta: Mutex::new(crate::session_timeline::SessionMeta::default()),
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
        screen_moves: Default::default(),
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
        parent: None,
        state: session_store::SessionState::Alive,
        title: format!("tab-{local_id}"),
        term: term.clone(),
        master: -1,
        ctx,
    }
}

/// Runs the redraw conformance gate on the main thread and returns its exit code.
#[must_use]
pub fn run_redraw_conformance() -> i32 {
    let mut event_loop = match crate::quiet_driver_event_loop_builder::<Wake>().build() {
        Ok(event_loop) => event_loop,
        Err(err) => {
            eprintln!("aterm-redraw-conformance: NOT RUN — no event loop here: {err}");
            eprintln!(
                "aterm-redraw-conformance: the redraw gate did not execute (exit {NOT_RUN}); this is not a pass"
            );
            return NOT_RUN;
        }
    };
    let proxy = event_loop.create_proxy();

    let term = Arc::new(Mutex::new(Terminal::new(24, 80)));
    term_lock(&term).process(
        b"\x1b]133;A\x07$ \x1b]633;E;echo hi\x07\x1b]133;B\x07echo hi\n\x1b]133;C\x07hi\n\x1b]133;D;0\x07",
    );
    let sibling_term = Arc::new(Mutex::new(Terminal::new(24, 80)));
    let store = session_store::new_store();
    let handle = registered_session(SID, &term);
    {
        let mut registry = store.write().unwrap_or_else(|p| p.into_inner());
        registry.register(handle.clone());
        registry.register(registered_session(SIBLING_SID, &sibling_term));
    }
    let host = GuiHost::with_fleet(SID, &term, Some(&proxy), &store, &handle.ctx);

    let caps = host.capabilities();
    println!(
        "aterm-redraw-conformance: caps event_loop={} roster={} clipboard={}",
        caps.event_loop, caps.roster, caps.clipboard
    );
    if !caps.event_loop {
        eprintln!(
            "aterm-redraw-conformance: FAIL — a live proxy is held, yet event_loop reads false"
        );
        return FAIL;
    }

    let outcomes = conformance::run_all_declared(
        &host,
        SID,
        HostCapabilities {
            frame_source: true,
            event_loop: true,
            clipboard: true,
            roster: true,
        },
    );
    for outcome in &outcomes {
        match &outcome.failure {
            None => println!("  ok   {}", outcome.check),
            Some(detail) => println!("  FAIL {}: {detail}", outcome.check),
        }
    }
    let check_failures = outcomes.iter().filter(|o| !o.passed()).count();
    println!(
        "aterm-redraw-conformance: {}/{} checks passed",
        outcomes.len() - check_failures,
        outcomes.len()
    );

    let mut delivered = DeliveredWakes::default();
    pump_until_quiet(&mut event_loop, &mut delivered, WARMUP);
    println!(
        "aterm-redraw-conformance: the matrix posted {} redraw(s) of its own",
        delivered.redraws_for(SID)
    );

    let mut redraw_failures = 0usize;
    for form in ["0 0 0 4", "clear"] {
        delivered.redraws.clear();
        let reply = cmd_select(&host, SID, form);
        if reply != "OK\n" {
            eprintln!("  FAIL select {form}: {reply:?}");
            redraw_failures += 1;
            continue;
        }
        let landed = pump_for_redraws(&mut event_loop, &mut delivered, SID, 1, DELIVERY_BUDGET);
        if landed == 0 {
            eprintln!(
                "  FAIL select {form}: OK on the wire, but no Wake::Output reached the event loop"
            );
            redraw_failures += 1;
        } else {
            println!("  ok   select {form}: {landed} Wake::Output(session={SID}) delivered");
        }
    }

    pump_until_quiet(&mut event_loop, &mut delivered, QUIET);
    delivered.redraws.clear();
    host.request_redraw(SIBLING_SID);
    pump_until_quiet(&mut event_loop, &mut delivered, QUIET);
    if delivered.redraws.is_empty() {
        println!("  ok   request_redraw(sibling {SIBLING_SID}): nothing delivered");
    } else {
        eprintln!(
            "  FAIL request_redraw(sibling {SIBLING_SID}): delivered {:?}",
            delivered.redraws
        );
        redraw_failures += 1;
    }
    if delivered.other > 0 {
        println!(
            "aterm-redraw-conformance: {} non-redraw wake(s) also arrived",
            delivered.other
        );
    }

    if check_failures == 0 && redraw_failures == 0 {
        println!("aterm-redraw-conformance: PASS");
        PASS
    } else {
        eprintln!(
            "aterm-redraw-conformance: FAIL — {check_failures} check(s), {redraw_failures} redraw gap(s)"
        );
        FAIL
    }
}
