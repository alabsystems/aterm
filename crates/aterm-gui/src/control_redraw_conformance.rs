// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The Phase 1a exit gate: `aterm-control`'s verb matrix run against a
//! [`GuiHost`] that holds a REAL `EventLoopProxy`, plus the one claim only that
//! host can support — a control-socket `select` reaches the window as a `Wake`
//! redraw.
//!
//! # WHY THIS IS A BINARY AND NOT A `#[test]` — do not convert it
//!
//! `EventLoop` construction panics unless it runs on the process MAIN thread, on
//! every target, and libtest runs every `#[test]` body on a spawned thread. So no
//! test in this crate can mint an `EventLoopProxy<Wake>`, and the unit matrix in
//! `control_host.rs` must build its host with `proxy: None`. That is the WHOLE
//! delta from the shipped host, and it reaches exactly two things:
//! [`SessionHost::request_redraw`] becomes a no-op and `capabilities().event_loop`
//! reads false — so nothing over there can show a `select` repainting anything.
//! Only a target that owns `fn main` can, which is what `src/bin/
//! aterm-redraw-conformance.rs` exists to be.
//!
//! A `harness = false` integration test also owns `fn main`, and was rejected for
//! a second reason: a test target is a separate crate, and [`GuiHost`] is private
//! to `control`. Moving the gate there would mean widening the shipped API to
//! satisfy a test.
//!
//! # Exit codes — CI gates on these, not on the prose
//!
//! * `0` — every check passed AND every promised redraw was delivered.
//! * `1` — a check failed, or a redraw the host accepted never arrived.
//! * `2` — NOT RUN: no event loop is constructible here (headless, no display).
//!
//! `2` is deliberately not `0`: a gate that could not run must never be readable
//! as a gate that passed.

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use aterm_control::conformance;
use aterm_control::selection::cmd_select;
use aterm_control::{HostCapabilities, SessionHost};
use aterm_core::terminal::Terminal;
use aterm_session::sink::SinkWriter;
use aterm_session::{EdgeTable, LaunchNonce, SessionId};
use winit::application::ApplicationHandler;
use winit::event::WindowEvent;
use winit::event_loop::{ActiveEventLoop, EventLoop};
use winit::platform::pump_events::{EventLoopExtPumpEvents, PumpStatus};
use winit::window::WindowId as WinitWindowId;

use super::control_host::GuiHost;
use crate::session_store::{self, SessionHandle};
use crate::{SessionCtx, Wake, term_lock};

/// Everything passed and every redraw landed.
const PASS: i32 = 0;
/// A check failed, or a redraw the host accepted never reached the loop.
const FAIL: i32 = 1;
/// The gate could not execute here. NOT a pass; see the module docs.
const NOT_RUN: i32 = 2;

/// The session the host serves.
const SID: u64 = 0;
/// A registered SIBLING — resolvable fleet-wide, served by nobody here, so the
/// negative phase has a sid that is real rather than merely unknown.
const SIBLING_SID: u64 = 1;

/// Long enough for macOS to launch its `NSApplication` (the first pump only gets
/// that far) and to drain what the matrix already posted.
const WARMUP: Duration = Duration::from_millis(500);
/// A redraw crosses an in-process channel plus one run-loop turn; this is slack,
/// not an expected latency.
const DELIVERY_BUDGET: Duration = Duration::from_secs(2);
/// How long "nothing arrived" is observed for before it counts as nothing.
const QUIET: Duration = Duration::from_millis(250);

/// Every `Wake` the loop actually DELIVERED. Delivery is the claim under test, so
/// the harness asserts on this — never on the proxy having accepted the send.
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

/// Pump until `want` redraws for `sid` have landed, or `budget` expires; answers
/// how many landed. Loops rather than pumping once because macOS spends its first
/// two pumps launching the app and dispatching init events.
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

/// Pump for the WHOLE budget whatever arrives — the shape a negative claim needs,
/// since "nothing was delivered" cannot short-circuit on success.
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

/// A session registered exactly as the GUI registers a tab — shared engine `Arc`,
/// fabric identity, sink — so the host under test is the SHIPPED shape.
///
/// The `-1` master is not a shortcut: the only frame the matrix writes is empty,
/// and `write_input` answers that one before it reaches the fd. Bytes-actually-land
/// is proven next to the real sink, in `control_host.rs`.
fn registered_session(local_id: u64, term: &Arc<Mutex<Terminal>>) -> SessionHandle {
    let sid = SessionId::generate();
    let nonce = LaunchNonce::generate();
    let ctx = Arc::new(SessionCtx {
        sink: Arc::new(SinkWriter::new(-1)),
        edges: Mutex::new(EdgeTable::new()),
        turn_lease: Mutex::new(None),
        self_id: sid.clone(),
        nonce,
        cast: Arc::new(Mutex::new(crate::cast::CastRecorder::new(80, 24))),
        temporal: Arc::new(Mutex::new(crate::temporal::TemporalRecorder::new())),
        byte_fanout: Arc::new(crate::cast::ByteFanout::new()),
        turns: Arc::new(Mutex::new(crate::turn_ledger::TurnLedger::default())),
        meta: Mutex::new(crate::session_timeline::SessionMeta::default()),
        app_kitty: Mutex::new(crate::app_kitty::AppKittySlot::default()),
        timeline: Arc::new(Mutex::new(
            crate::session_timeline::SessionTimeline::default(),
        )),
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

/// Run the gate on THIS thread, which the caller guarantees is the process main
/// thread. Answers the process exit code; see the module docs for what each means.
#[must_use]
pub fn run_redraw_conformance() -> i32 {
    // First, because everything below is meaningless without a proxy — and because
    // the honest SKIP lives on this one error.
    let mut event_loop = match EventLoop::<Wake>::with_user_event().build() {
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

    // Real OSC-133 blocks, so the block checks are not satisfied by an empty
    // screen and the suite's save/restore runs over real selection state.
    let term = Arc::new(Mutex::new(Terminal::new(24, 80)));
    term_lock(&term).process(
        b"\x1b]133;A\x07$ \x1b]633;E;echo hi\x07\x1b]133;B\x07echo hi\n\x1b]133;C\x07hi\n\x1b]133;D;0\x07",
    );
    let sibling_term = Arc::new(Mutex::new(Terminal::new(24, 80)));
    let subscribers = crate::subscribe::new_registry();
    let store = session_store::new_store();
    let handle = registered_session(SID, &term);
    {
        let mut registry = store.write().unwrap_or_else(|p| p.into_inner());
        registry.register(handle.clone());
        registry.register(registered_session(SIBLING_SID, &sibling_term));
    }
    let host = GuiHost::with_fleet(
        SID,
        &term,
        Some(&proxy),
        &subscribers,
        &store,
        &handle.ctx.sink,
    );

    let caps = host.capabilities();
    println!(
        "aterm-redraw-conformance: caps event_loop={} roster={} input_sink={} clipboard={}",
        caps.event_loop, caps.roster, caps.input_sink, caps.clipboard
    );
    if !caps.event_loop {
        eprintln!(
            "aterm-redraw-conformance: FAIL — a live proxy is held, yet event_loop reads false"
        );
        return FAIL;
    }

    // Why DECLARED: this gate holds a live proxy, so unlike the #[test] host every
    // capability is genuinely present — `run_all` would let one that stopped
    // delivering take the easy arm of its check and still report green.
    let outcomes = conformance::run_all_declared(
        &host,
        SID,
        HostCapabilities {
            frame_source: true,
            event_loop: true,
            clipboard: true,
            roster: true,
            input_sink: true,
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
    // Drain what the matrix itself posted, so each phase below counts only its own
    // verb's redraws.
    pump_until_quiet(&mut event_loop, &mut delivered, WARMUP);
    println!(
        "aterm-redraw-conformance: the matrix posted {} redraw(s) of its own",
        delivered.redraws_for(SID)
    );

    let mut redraw_failures = 0usize;
    // THE CLAIM THE UNIT MATRIX CANNOT MAKE: `select` answered OK on the wire AND
    // the repaint reached the event loop. Both selection forms, because they take
    // the two different `request_redraw` sites in `cmd_select`.
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

    // FAIL-CLOSED, end to end: a sibling sid this host does not serve must post
    // NOTHING. Anything delivered here would repaint on a borrowed number.
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
