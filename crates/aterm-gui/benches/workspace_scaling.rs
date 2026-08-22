// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
//
// THE WORKSPACE AXIS — what `frame_latency` cannot see.
//
// `frame_latency`'s `many_tabs_idle/{2,8,32}` is flat across tab count, and
// that result is TRUE and GOOD: the compositor really is tab-count-flat. But it
// measures `App::redraw_compose`, which receives the tab-strip fingerprint as a
// PARAMETER and sits BELOW `redraw_window`'s wrapper. Four of the six
// multi-pane/tab/window findings live in that wrapper —
// `resize_panes_scoped` (twice: the settle and the per-frame drag tick),
// `present_latency_ns`, `redraw_tab_strip_state` — and two on the
// `Wake::Output` arm (`observe_session_statuses`, `observe_title_drift`). The
// flat compose number says nothing about any of them, so this target exists to
// price the passes the wrapper runs, at the scale a real power user reaches:
// 30 tabs x 4 panes on a 4K display, i.e. 120 live sessions behind a visible
// working set of 4.
//
// WHAT IS TIMED, EXACTLY — the `frame_latency` contract, verbatim:
//
//     arm(..);                     // UNTIMED: grid flip, quiesce, stamp arm
//     let t0 = Instant::now();     //  ── timed span opens
//     black_box(<pass>);           //  the SHIPPING entry point, called directly
//     total += t0.elapsed();       //  ── timed span closes
//
// Every workload is VERIFIED before it is timed, two-sided: a lower guard that
// the workload really reaches the state it claims (a settle that offloaded
// nothing, a latency walk over an all-zero pool, or a strip read with the strip
// disabled would each price an early return and call it a measurement), and an
// upper guard on the design bound the finding is about.
//
// THE CONCURRENCY IS AN ASSERTED NUMBER, NOT A CLAIMED ONE. `resize_settle`
// reads the process-global `aterm-reflow` gauge across one settle and pins both
// sides: at least one worker was observed driving a job (reach — the settle
// really did offload), and never more than `reflow_thread_ceiling(jobs)` were
// driving at the same instant (the bound). That ceiling is a function of the
// SHIPPING code, not a literal in this file — today it returns `jobs`, because
// the hand-off spawns one OS thread per job with no pool and no semaphore, so a
// 30x4 settle puts 120 threads in flight from one drag of one window edge. A
// fix that bounds the concurrency tightens this file's assertion by editing
// that one function; the recorded `os_threads_per_settle` and
// `peak_reflow_workers` counts in the volume group are then the numbers that
// move, stored and A/B'd exactly like a time.
//
// PEAK, NOT THREADS-CREATED, IS WHAT IS ASSERTED. A peak is instantaneous: the
// design either guarantees it or does not, and the high-water mark either
// exceeded it or did not. "Threads created per settle" is only a bound in the
// absence of worker turnover part-way through a settle, and a guard that can be
// satisfied for the wrong reason is not a guard — so it is recorded, not
// asserted (beyond the trivially-true `threads <= jobs`).
//
// TWO AXES, DELIBERATELY SEPARATED. `status_gate` sweeps SESSIONS at a fixed
// one tab (`stage_workspace(1, N)`), because `observe_session_statuses` folds
// the session POOL; `title_gate`, `strip_state` and `strip_refresh` sweep TABS
// at one pane each (`stage_workspace(N, 1)`), because those fold the window's
// TAB list for a single session id. A single-axis sweep would confound the two
// findings; only the pair tells them apart.
//
// WHAT IS OUT OF REACH, cut honestly rather than modeled: the `Wake::Output`
// arm and `about_to_wait` themselves (both take `&ActiveEventLoop`, which no
// headless fixture can mint), the OS present, and the AppKit toolbar push that
// `refresh_window_tabs` feeds on macOS. `title_gate` and `status_gate` are the
// two SHIPPING calls the wake arm opens with, called directly; the arm's own
// dispatch is not modeled and is not claimed.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{Duration, Instant};

use aterm_gui::bench_support::BenchApp;
use criterion::measurement::WallTime;
use criterion::{
    BenchmarkGroup, BenchmarkId, Criterion, black_box, criterion_group, criterion_main,
};

// ------------------------------------------------------------------ dials --

/// The TAB sweep. 30 is the finding's realistic power-user ceiling; 1 is the
/// single-tab baseline every "flat in N" claim is measured against.
const TABS: [usize; 3] = [1, 8, 30];

/// The PANE sweep, per tab. 4 is the finding's shape; 1 isolates the tab axis.
const PANES: [usize; 2] = [1, 4];

/// Off-screen history fed into EVERY session before a resize workload. Small
/// on purpose: it exists to make each reflow job carry REAL work (so the
/// concurrency the settle creates is observable rather than instantaneous),
/// not to price the rewrap — `aterm-scrollback`'s own `push_line`/`iterate`
/// and `aterm-grid`'s `reflow_step_timing` are the per-session denominators
/// and stay where they are. 400 lines x 120 sessions is ~50k history lines
/// resident, which is a workspace, not a stress test.
const HISTORY_LINES: usize = 250;

/// The two grids the resize workloads alternate between. Only the COLUMN count
/// differs: a row-only resize is bounded and offloads nothing, so a workload
/// that changed rows would hand off zero jobs and measure an early return.
const GRID_WIDE: (u16, u16) = (36, 200);
/// See [`GRID_WIDE`].
const GRID_NARROW: (u16, u16) = (36, 176);

/// How long a settle is given to converge before the workload declares the
/// fixture broken. Generous: it is an ARM, never inside a timed span, and a
/// too-tight bound would turn a slow machine into a spurious failure.
const QUIESCE: Duration = Duration::from_secs(60);

/// Turns of the flood the wake-arm gates are sampled over. Sized so the whole
/// sampling window (`SAMPLE_TURNS * BURST_DT` = ~33 ms) fits inside the
/// SHORTEST observation interval the config can resolve to
/// (`MIN_TAB_STATUS_OBSERVE_INTERVAL_MS` = 50), so the "nothing is due" guard
/// below cannot fail on a config that dialled the dwell right down.
const SAMPLE_TURNS: usize = 100;

/// The PTY reader's batch cadence under a flood (~3000 wakes/s) — the rate the
/// two `Wake::Output` gates actually run at, and slow enough that the
/// 250 ms status interval never comes due inside a sampling window.
const BURST_DT: Duration = Duration::from_micros(333);

// ------------------------------------------------------------------ report --

/// The one human-readable line each verify pass prints: what state the workload
/// reached, in the numbers its guards assert on.
fn report(name: &str, detail: &str) {
    println!("REACH {name:<28} | {detail}");
}

// ---------------------------------------------------------------- fixtures --

/// A staged workspace: `tabs` tabs x `panes` panes, every pane sized to
/// [`GRID_WIDE`]. The grid is set BEFORE staging because each stub split runs
/// the real `resize_panes` against the window's current grid.
fn f_workspace(tabs: usize, panes: usize) -> BenchApp {
    let mut b = BenchApp::headless();
    b.set_grid(GRID_WIDE.0, GRID_WIDE.1);
    b.stage_workspace(tabs, panes);
    b
}

/// [`f_workspace`] plus real off-screen history in every pane — the resize
/// fixtures' shape. Returns the workspace and its hand-off count (one per
/// pane, because a column change moves EVERY pane's engine width).
fn f_resize(tabs: usize, panes: usize) -> (BenchApp, usize) {
    let mut b = f_workspace(tabs, panes);
    for sid in b.session_ids() {
        b.feed_history(sid, HISTORY_LINES);
    }
    let jobs = b.pane_count();
    (b, jobs)
}

/// The session ids of the ACTIVE tab's panes (the last tab staged) and of a
/// BACKGROUND tab's pane, as `(active_hi, background_lo)`. `stage_workspace`
/// mints ids in order and leaves the last tab active with the last split
/// focused, so the highest id is on the active tab and id 0 is on tab 0.
fn active_and_background(b: &BenchApp) -> (u64, u64) {
    let ids = b.session_ids();
    (*ids.last().expect("staged workspace"), ids[0])
}

/// Arm EVERY session's output stamp exactly `age` ns in the past ON THE APP'S
/// OWN EPOCH, and return the stamp written.
///
/// THE WAIT IS THE POINT. `lat_epoch` starts when the fixture's `App` is built,
/// so a freshly staged workspace can be YOUNGER than `age`: `now - age` then
/// saturates to 0, which is the walk's "unarmed" sentinel, and the whole pool
/// reads as dark — a timed span over an all-zero pool, priced under an armed
/// name. (That is not hypothetical: it is what this file did before the guard
/// on the next line caught it, on an epoch ~3 us old.) Waiting the epoch out is
/// untimed setup and costs at most `age` once per fixture; inside the timed
/// loop the epoch is already old and the wait is a single load.
fn arm_aged(b: &BenchApp, age: u64) -> u64 {
    while b.lat_now_ns() <= age {
        std::thread::sleep(Duration::from_micros(100));
    }
    let armed = b.lat_now_ns() - age;
    assert!(armed > 0, "arm_aged: a zero stamp is the unarmed sentinel");
    b.arm_output_stamps(armed);
    armed
}

// ------------------------------------------------------------------ verify --

/// What ONE verified settle actually did, kept so the volume group can record
/// the same numbers the guards asserted on rather than re-derive them.
#[derive(Clone, Copy)]
struct SettleFacts {
    /// Rewrap hand-offs — one per pane whose engine width changed.
    jobs: usize,
    /// OS threads the hand-off created. THE number MPT-1 moves.
    threads: usize,
    /// Most workers driving jobs at the same instant.
    peak: usize,
}

/// PROVE the resize-settle workload: the workspace is the shape it claims, its
/// panes own real history, ONE settle hands off exactly one job per pane, the
/// OS-thread count that settle creates is inside the SHIPPING bound, and the
/// history is still there afterwards.
///
/// The last guard is the behaviour half, and it is not ceremony: the whole
/// point of the hand-off is that history survives an off-thread rewrap, and a
/// concurrency change that dropped a job on the floor would leave
/// `scrollback_detached_for_reflow` latched and the tiered history permanently
/// invisible — which reads, from outside, as `history_lines() == 0`.
fn verify_resize_settle(tabs: usize, panes: usize) -> (BenchApp, SettleFacts) {
    let (mut b, jobs) = f_resize(tabs, panes);
    assert_eq!(
        b.pane_count(),
        tabs * panes,
        "resize_settle: fixture staged the wrong workspace shape"
    );
    let min_hist = b
        .session_ids()
        .iter()
        .map(|&sid| b.history_lines(sid))
        .min()
        .expect("staged workspace");
    assert!(
        min_hist >= HISTORY_LINES / 2,
        "resize_settle: a pane owns only {min_hist} history lines — the workload \
         would price a hand-off with no rewrap behind it"
    );

    assert!(
        b.reflow_quiesce(QUIESCE),
        "resize_settle: the staging's own resizes never converged"
    );
    b.reflow_gauge_reset_peak();
    let (_, _, sub0, _, thr0) = b.reflow_gauge();

    b.set_grid(GRID_NARROW.0, GRID_NARROW.1);
    b.resize_settle();
    let (_, _, sub1, _, _) = b.reflow_gauge();
    let handed_off = usize::try_from(sub1 - sub0).expect("hand-off count fits usize");
    assert_eq!(
        handed_off, jobs,
        "resize_settle: a column change must move EVERY pane's engine width, so \
         the settle must hand off exactly one rewrap per pane"
    );

    assert!(
        b.reflow_quiesce(QUIESCE),
        "resize_settle: the settle never converged — a job was dropped without \
         re-attaching or aborting, which wedges that pane's tiered history"
    );
    let (running, peak, sub2, fin2, thr2) = b.reflow_gauge();
    assert_eq!(
        running, 0,
        "resize_settle: quiescent means no worker is running"
    );
    assert_eq!(
        sub2, fin2,
        "resize_settle: every hand-off was accounted for"
    );

    // TWO-SIDED, against the SHIPPING bound (see the file header). The asserted
    // number is PEAK CONCURRENCY, not threads-created: the peak is an
    // INSTANTANEOUS property the hand-off design either guarantees or does not,
    // whereas "threads created per settle" is only a bound in the absence of
    // worker turnover part-way through, and a guard that can be right for the
    // wrong reason is not a guard. Threads-created is recorded beside it in the
    // volume group, where it is the headline number MPT-1 moves.
    let threads = usize::try_from(thr2 - thr0).expect("thread count fits usize");
    let peak = usize::try_from(peak).expect("peak fits usize");
    let ceiling = BenchApp::reflow_thread_ceiling(jobs);
    assert!(
        peak >= 1,
        "resize_settle: no worker was ever observed driving a job — the settle \
         offloaded nothing and this workload prices an early return"
    );
    assert!(
        peak <= ceiling,
        "resize_settle: {peak} reflow workers were driving jobs at the same \
         instant for {jobs} hand-offs, above the declared bound of {ceiling} \
         (app_render::reflow_thread_ceiling)"
    );
    assert!(
        threads <= jobs.max(1),
        "resize_settle: one settle created {threads} OS threads for {jobs} \
         hand-offs — more threads than jobs means the hand-off is thrashing"
    );

    let min_hist_after = b
        .session_ids()
        .iter()
        .map(|&sid| b.history_lines(sid))
        .min()
        .expect("staged workspace");
    assert!(
        min_hist_after >= HISTORY_LINES / 2,
        "resize_settle: a pane came back from the rewrap with {min_hist_after} \
         history lines — the offload lost history"
    );

    report(
        &format!("resize_settle/{tabs}x{panes}"),
        &format!(
            "panes {jobs} | hand-offs {handed_off} | os threads {threads} | peak workers \
             {peak} (bound {ceiling}) | history {min_hist}->{min_hist_after} lines"
        ),
    );
    (
        b,
        SettleFacts {
            jobs,
            threads,
            peak,
        },
    )
}

/// PROVE the live-drag frame workload (MPT-2): the window is `panes_stale`
/// (which is what makes `redraw_window` run this on EVERY presented frame), the
/// ACTIVE tab is at the NEW grid, the BACKGROUND tabs are still at the OLD one,
/// and repeating the pass moves NOTHING — so the timed span is purely the
/// background-tab layout plans that are built and thrown away.
///
/// That "moves nothing" guard is the whole measurement: without it the number
/// would silently include real engine resizes on the first pass and be
/// unreproducible on the rest.
fn verify_panes_stale(tabs: usize, panes: usize) -> BenchApp {
    // NO history here on purpose: MPT-2 is about the discarded PLANS, and a
    // rewrap in the span would drown them.
    let mut b = f_workspace(tabs, panes);
    assert!(
        b.reflow_quiesce(QUIESCE),
        "panes_stale_frame: staging never converged"
    );
    let (active, background) = active_and_background(&b);

    b.set_grid(GRID_NARROW.0, GRID_NARROW.1);
    b.resize_scoped_active();
    let active_size = b.engine_size(active);
    let background_size = b.engine_size(background);

    if tabs > 1 {
        assert!(
            b.panes_stale(),
            "panes_stale_frame: a deferred background tab must mark the window \
             panes_stale — that flag is what makes redraw_window run this per frame"
        );
        assert_ne!(
            active_size, background_size,
            "panes_stale_frame: the background tab was NOT deferred, so the pass \
             under test is not the scoped one this workload claims"
        );
    }

    // Settle-invariance: after the first scoped pass every pane the filter
    // admits is already at its derived size, so further passes early-out per
    // pane and change nothing at all.
    let before: Vec<(u64, (u16, u16))> = b
        .session_ids()
        .iter()
        .map(|&sid| (sid, b.engine_size(sid)))
        .collect();
    // `submitted` is only ever incremented on THIS thread (the hand-off is
    // booked before any worker can exist), so comparing it across the repeats
    // is exact and needs no quiescence wait.
    let (_, _, submitted_before, _, _) = b.reflow_gauge();
    for _ in 0..16 {
        b.resize_scoped_active();
    }
    let after: Vec<(u64, (u16, u16))> = b
        .session_ids()
        .iter()
        .map(|&sid| (sid, b.engine_size(sid)))
        .collect();
    assert_eq!(
        before, after,
        "panes_stale_frame: a repeated scoped pass moved an engine — the timed \
         span is not pure waste and the number would not reproduce"
    );
    let (_, _, submitted_after, _, _) = b.reflow_gauge();
    assert_eq!(
        submitted_before, submitted_after,
        "panes_stale_frame: a repeated scoped pass handed off a rewrap, so the \
         timed span carries real work and is not the plan-only waste this \
         workload claims to price"
    );

    report(
        &format!("panes_stale_frame/{tabs}x{panes}"),
        &format!(
            "panes {} | stale {} | active {active_size:?} bg {background_size:?} | \
             16 repeats moved 0 engines",
            b.pane_count(),
            b.panes_stale()
        ),
    );
    b
}

/// PROVE the present-latency walk (MPT-3): with every session's stamp armed the
/// walk books a real number, and after it the ACTIVE panes' stamps are consumed.
/// The DARK control is the same call on an unarmed workspace — it must return 0,
/// otherwise the armed number would prove nothing about the walk.
fn verify_present_latency(tabs: usize, panes: usize) -> BenchApp {
    let mut b = f_workspace(tabs, panes);
    let (active, _) = active_and_background(&b);

    // DARK: nothing armed anywhere.
    b.arm_output_stamps(0);
    assert_eq!(
        b.present_latency(),
        0,
        "present_latency_scan: an unarmed workspace must book nothing"
    );

    // LIT: arm every session one millisecond ago on the app's own epoch.
    let armed = arm_aged(&b, 1_000_000);
    let dt = b.present_latency();
    assert!(
        dt >= 1_000_000,
        "present_latency_scan: an armed visible pane must book at least the \
         interval it was armed for (got {dt} ns)"
    );
    assert!(
        dt < 5_000_000_000,
        "present_latency_scan: booked {dt} ns, above the honesty cap — the \
         fixture's epoch arithmetic is wrong"
    );
    // The visible pane's stamp is CONSUMED by the walk (`swap(0)`), so an
    // immediate second walk books nothing.
    assert_eq!(
        b.present_latency(),
        0,
        "present_latency_scan: the walk must consume the stamps it books"
    );
    report(
        &format!("present_latency_scan/{tabs}x{panes}"),
        &format!(
            "panes {} | active session {active} | armed {armed} (1ms ago) -> booked \
             {dt} ns, consumed on the next walk",
            b.pane_count()
        ),
    );
    b
}

/// PROVE the status gate (MPT-4): the subsystem is ON, and on the sampled turns
/// NO session is due — the exact steady state the finding names, "O(sessions)
/// probes per burst to usually find zero". The two-sided control advances the
/// clock past the observation interval and requires the sweep to classify
/// again, so the zero above is a rate limit and not a dead subsystem.
fn verify_status_gate(sessions: usize) -> (BenchApp, Instant) {
    let mut b = f_workspace(1, sessions);
    assert!(
        b.tab_status_on(),
        "status_gate: tab_status is off — observe_session_statuses is one early \
         return and this workload prices nothing"
    );
    let mut now = Instant::now();
    // First sweep classifies everything and arms every session's interval.
    let first = b.observe_statuses(now);
    let ids = b.session_ids();
    assert_eq!(
        ids.len(),
        sessions,
        "status_gate: fixture staged the wrong session count"
    );

    let mut due_seen = 0usize;
    let mut changed_seen = 0usize;
    for _ in 0..SAMPLE_TURNS {
        now += BURST_DT;
        due_seen += ids.iter().filter(|&&sid| b.status_due(sid, now)).count();
        changed_seen += b.observe_statuses(now);
    }
    assert_eq!(
        due_seen, 0,
        "status_gate: {due_seen} session-turns were DUE inside the sampling \
         window — the workload is measuring the classify path, not the gate"
    );
    assert_eq!(
        changed_seen, 0,
        "status_gate: the rate-limited steady state published a status change"
    );
    // CONTROL: past the interval, the sweep must still work.
    let later = now + Duration::from_secs(2);
    assert!(
        ids.iter().any(|&sid| b.status_due(sid, later)),
        "status_gate: nothing ever comes due again — the classifier is dead, and \
         the zero above would prove nothing"
    );
    report(
        &format!("status_gate/{sessions}"),
        &format!(
            "sessions {sessions} | first sweep classified {first} | \
             {SAMPLE_TURNS} steady turns: 0 due, 0 changed | control comes due at +2s"
        ),
    );
    (b, now)
}

/// PROVE the title-drift gate (MPT-5): in the steady state the session's
/// consumed epoch EQUALS its live epoch, which is precisely when the gate's
/// cheap first disjunct is false and the whole-workspace scan is what runs.
/// The control moves the title and requires the gate to consume the new epoch.
fn verify_title_gate(tabs: usize) -> (BenchApp, Instant) {
    let mut b = f_workspace(tabs, 1);
    let (active, _) = active_and_background(&b);
    let mut now = Instant::now();
    // One flush consumes the current epoch and puts the gate in its steady
    // state (the debounce anchor is set, so later calls take the same path a
    // flood takes).
    b.title_gate(active, now);
    for _ in 0..16 {
        now += BURST_DT;
        b.title_gate(active, now);
    }
    let live = b.title_epoch(active);
    assert_eq!(
        b.title_seen_epoch(active),
        Some(live),
        "title_gate: the consumed epoch has not caught up with the live one, so \
         the gate is taking its CHANGED path — the flood steady state is the \
         opposite, and that is what this workload must measure"
    );
    // CONTROL: a real title change must move the epoch and be consumed.
    b.set_title(active, "workspace-scaling control");
    let moved = b.title_epoch(active);
    assert!(
        moved > live,
        "title_gate: set_title did not move title_epoch — the control cannot \
         distinguish a working gate from a dead one"
    );
    now += Duration::from_millis(500);
    b.title_gate(active, now);
    assert_eq!(
        b.title_seen_epoch(active),
        Some(moved),
        "title_gate: a real title change was not consumed"
    );
    // Back to the steady state for the timed run.
    for _ in 0..16 {
        now += BURST_DT;
        b.title_gate(active, now);
    }
    report(
        &format!("title_gate/{tabs}"),
        &format!("tabs {tabs} | steady epoch {moved} consumed | control moved {live}->{moved}"),
    );
    (b, now)
}

/// PROVE the tab-strip read (MPT-6, read side): the strip is ENABLED, its
/// fingerprint is nonzero and STABLE across repeated reads of a settled
/// workspace (the frames the RepaintKey early-out exists to make free — and
/// which still pay this whole pass today), and a single tab's title change
/// MOVES it (so a fast path that stopped hashing would be caught).
fn verify_strip(tabs: usize) -> BenchApp {
    let mut b = f_workspace(tabs, 1);
    b.enable_tab_strip();
    let fp = b.strip_state();
    assert_ne!(
        fp, 0,
        "strip_state: a disabled or empty strip returns 0 — this workload would \
         price the else-arm"
    );
    for _ in 0..8 {
        assert_eq!(
            b.strip_state(),
            fp,
            "strip_state: a settled strip's fingerprint moved on its own"
        );
    }
    let (active, _) = active_and_background(&b);
    b.set_title(active, "workspace-scaling strip control");
    let moved = b.strip_state();
    assert_ne!(
        moved, fp,
        "strip_state: a tab's title change did NOT move the fingerprint — the \
         RepaintKey would miss it, and a 'settled' number measured here would be \
         measuring a broken read"
    );
    // Settle again so the timed loop reads the steady-state (unchanged) path.
    let settled = b.strip_state();
    assert_eq!(settled, moved, "strip_state: did not re-settle");
    report(
        &format!("strip_state/{tabs}"),
        &format!(
            "tabs {tabs} | settled fp {settled:#x} stable over 8 reads | title change moved it"
        ),
    );
    b
}

// ---------------------------------------------------------------- counting --

/// Record a COUNT as a criterion measurement — 1 ns == 1 item, verbatim the
/// convention `frame_latency` and the aterm-effects benches use (see
/// `frame_latency::bench_count` for why the spin loop and the `k % 4` jitter
/// are not ceremony).
fn bench_count(g: &mut BenchmarkGroup<'_, WallTime>, id: &str, count: usize) {
    assert!(
        count > 0,
        "{id}: a zero count cannot be recorded as a duration"
    );
    let n = count as u64;
    let mut k = 0u64;
    g.bench_function(BenchmarkId::from_parameter(id), |b| {
        b.iter_custom(|iters| {
            let mut spin = 0u64;
            for i in 0..iters {
                spin = spin.wrapping_add(black_box(i));
            }
            black_box(spin);
            k = k.wrapping_add(1);
            Duration::from_nanos(n.saturating_mul(iters).saturating_add(k % 4))
        });
    });
}

// -------------------------------------------------------------- contenders --

/// Real cross-thread writers on the very cells the present walk does its
/// read-modify-write on. WHY: `present_latency_ns` swaps every session's
/// `last_output_ns`, and each swap invalidates a cache line that session's PTY
/// reader is writing. Without contenders those atomics are uncontended L1 hits
/// and the microbenchmark would report a coherence cost of zero for the exact
/// property the finding is about.
struct Contenders {
    stop: Arc<AtomicBool>,
    /// LIVE write count, published in batches while the threads run — not
    /// summed at join. A counter only readable after `join_all` can prove the
    /// writers ran SOMETIME; only a live one can prove they were still hammering
    /// when the timed span ended.
    writes: Arc<AtomicU64>,
    joins: Vec<std::thread::JoinHandle<()>>,
}

/// Writes each contender does between two publishes of the live counter. Big
/// enough that the shared counter is not itself the contention being measured,
/// small enough that a progress check resolves immediately.
const CONTENDER_BATCH: u64 = 1024;

impl Contenders {
    fn spawn(b: &BenchApp) -> Self {
        let stop = Arc::new(AtomicBool::new(false));
        let writes = Arc::new(AtomicU64::new(0));
        let started = Arc::new(AtomicU64::new(0));
        let ids = b.session_ids();
        let want = ids.len() as u64;
        let joins: Vec<_> = ids
            .into_iter()
            .map(|sid| {
                let cell = b.output_stamp_cell(sid);
                let stop = stop.clone();
                let writes = writes.clone();
                let started = started.clone();
                std::thread::spawn(move || {
                    started.fetch_add(1, Ordering::Relaxed);
                    let mut n = 0u64;
                    while !stop.load(Ordering::Relaxed) {
                        // Exactly the reader's own arming shape: CAS 0 -> now,
                        // so a consumed stamp is re-armed and a live one is
                        // left alone.
                        let _ = cell.compare_exchange(0, 1, Ordering::Relaxed, Ordering::Relaxed);
                        n = n.wrapping_add(1);
                        if n % CONTENDER_BATCH == 0 {
                            writes.fetch_add(CONTENDER_BATCH, Ordering::Relaxed);
                        }
                    }
                    writes.fetch_add(n % CONTENDER_BATCH, Ordering::Relaxed);
                })
            })
            .collect();
        // HANDSHAKE, not hope: every writer has entered its loop before the
        // caller can time anything. Without it the "did they run?" guard is a
        // race — the spawn-to-first-CAS window is longer than a criterion
        // registration pass, which is exactly how a contended arm can end up
        // measuring the uncontended path.
        let deadline = Instant::now() + Duration::from_secs(30);
        while started.load(Ordering::Relaxed) < want {
            assert!(
                Instant::now() < deadline,
                "Contenders: only {}/{want} writer threads ever started",
                started.load(Ordering::Relaxed)
            );
            std::hint::spin_loop();
        }
        Self {
            stop,
            writes,
            joins,
        }
    }

    /// PROVE the writers were still making progress at this instant — i.e. the
    /// span that just closed was contended for its whole length, not merely
    /// preceded by threads that once existed. Blocks until the live counter
    /// moves, which is immediate for running writers and never for stalled ones.
    fn assert_progressing(&self, ctx: &str) {
        let base = self.writes.load(Ordering::Relaxed);
        let deadline = Instant::now() + Duration::from_secs(30);
        while self.writes.load(Ordering::Relaxed) <= base {
            assert!(
                Instant::now() < deadline,
                "{ctx}: the contending writers made no progress, so this arm \
                 measured the uncontended path under a contended name"
            );
            std::hint::spin_loop();
        }
    }

    fn join_all(self) -> u64 {
        self.stop.store(true, Ordering::Relaxed);
        for j in self.joins {
            let _ = j.join();
        }
        self.writes.load(Ordering::Relaxed)
    }
}

// -------------------------------------------------------------- the groups --

#[allow(
    clippy::too_many_lines,
    reason = "one linear registry of verified workloads, the frame_latency shape"
)]
fn workspace_scaling(c: &mut Criterion) {
    // PROVE FIRST, TIME SECOND. Every fixture is staged, warmed and verified
    // before a nanosecond is measured, and the timed run continues from the
    // verified state.
    let mut settles: Vec<(usize, usize, BenchApp, SettleFacts)> = Vec::new();
    let mut drags: Vec<(usize, usize, BenchApp)> = Vec::new();
    let mut latency: Vec<(usize, usize, BenchApp)> = Vec::new();
    for &tabs in &TABS {
        for &panes in &PANES {
            let (b, facts) = verify_resize_settle(tabs, panes);
            settles.push((tabs, panes, b, facts));
            drags.push((tabs, panes, verify_panes_stale(tabs, panes)));
            latency.push((tabs, panes, verify_present_latency(tabs, panes)));
        }
    }
    let mut statuses: Vec<(usize, BenchApp, Instant)> = TABS
        .iter()
        .map(|&n| {
            let (b, now) = verify_status_gate(n);
            (n, b, now)
        })
        .collect();
    let mut titles: Vec<(usize, BenchApp, Instant)> = TABS
        .iter()
        .map(|&n| {
            let (b, now) = verify_title_gate(n);
            (n, b, now)
        })
        .collect();
    let mut strips: Vec<(usize, BenchApp)> = TABS.iter().map(|&n| (n, verify_strip(n))).collect();

    {
        let mut group = c.benchmark_group("workspace_scaling");
        // A settle spawns real OS threads and waits for them; ten samples is
        // plenty and keeps the target's wall time honest.
        group
            .sample_size(10)
            .measurement_time(Duration::from_secs(20));
        // 1. MPT-1 + MPT-2: the AllTabs settle's MAIN-THREAD span — where the
        // per-pane `Builder::spawn` storm and the whole-workspace layout both
        // land. The arm converges the previous settle (a pane with detached
        // history hands off nothing, so an un-quiesced iteration would measure
        // an empty settle) and flips the grid so every iteration is a real
        // width change.
        for (tabs, panes, b_app, _) in &mut settles {
            let id = format!("tabs{tabs}_panes{panes}");
            group.bench_function(BenchmarkId::new("resize_settle", &id), |b| {
                b.iter_custom(|iters| {
                    let mut total = Duration::ZERO;
                    for i in 0..iters {
                        assert!(
                            b_app.reflow_quiesce(QUIESCE),
                            "resize_settle: a settle never converged mid-run"
                        );
                        let (r, cols) = if i % 2 == 0 { GRID_NARROW } else { GRID_WIDE };
                        b_app.set_grid(r, cols);
                        let t0 = Instant::now();
                        b_app.resize_settle();
                        total += t0.elapsed();
                    }
                    total
                });
            });
        }
        group
            .sample_size(100)
            .measurement_time(Duration::from_secs(5));
        // 2. MPT-2: the per-frame drag tick `redraw_window` runs while
        // `panes_stale` stands. Every admitted pane is already at its derived
        // size, so the whole span is background-tab plans built and dropped.
        for (tabs, panes, b_app) in &mut drags {
            let id = format!("tabs{tabs}_panes{panes}");
            group.bench_function(BenchmarkId::new("panes_stale_frame", &id), |b| {
                b.iter(|| b_app.resize_scoped_active());
            });
        }
        // 3. MPT-3: the latency walk every successful present runs. The arm
        // re-arms every session's stamp (the walk consumes them), untimed.
        for (tabs, panes, b_app) in &mut latency {
            let id = format!("tabs{tabs}_panes{panes}");
            group.bench_function(BenchmarkId::new("present_latency_scan", &id), |b| {
                b.iter_custom(|iters| {
                    let mut total = Duration::ZERO;
                    for _ in 0..iters {
                        arm_aged(b_app, 1_000_000);
                        let t0 = Instant::now();
                        black_box(b_app.present_latency());
                        total += t0.elapsed();
                    }
                    total
                });
            });
        }
        // 4. MPT-4: the status gate at the head of the wake arm, in its
        // rate-limited steady state (the guards pin that nothing is due).
        for (n, b_app, now) in &mut statuses {
            // THE CLOCK IS FROZEN INSIDE THE SPAN, DELIBERATELY. The gate is a
            // comparison of `now` against per-session deadlines, and criterion
            // runs this loop tens of thousands of times: an advancing clock
            // would cross the observation interval mid-run and the loop would
            // silently start timing the CLASSIFY path (a try_lock and a block
            // peek per session) under the gate's name. The verified instant is
            // inside every session's interval — the rate-limited steady state
            // this workload exists to price, and the one its guards proved.
            let at = *now;
            let id = *n;
            group.bench_function(BenchmarkId::new("status_gate", id), |b| {
                b.iter(|| black_box(b_app.observe_statuses(at)));
            });
        }
        // 5. MPT-5: the title-drift gate, one session, N tabs — the axis that
        // separates it from the session-count fold above.
        for (n, b_app, now) in &mut titles {
            // Frozen for the same reason as `status_gate` above: in the steady
            // state the gate returns before it ever consults the debounce
            // clock, so `now` is not an input to the path being timed — but an
            // advancing one could cross the debounce window on the single turn
            // where it IS consulted and put a flush inside the span.
            let sid = active_and_background(b_app).0;
            let at = *now;
            let id = *n;
            group.bench_function(BenchmarkId::new("title_gate", id), |b| {
                b.iter(|| b_app.title_gate(sid, at));
            });
        }
        // 6. MPT-6 read side: the whole-strip title read + SipHash that runs on
        // EVERY redraw, before the early-out, on a SETTLED workspace.
        for (n, b_app) in &mut strips {
            group.bench_function(BenchmarkId::new("strip_state", *n), |b| {
                b.iter(|| black_box(b_app.strip_state()));
            });
        }
        // 7. MPT-6 write side: the whole-strip rebuild one tab's label change
        // funnels into.
        for (n, b_app) in &mut strips {
            group.bench_function(BenchmarkId::new("strip_refresh", *n), |b| {
                b.iter(|| b_app.refresh_tabs());
            });
        }
        group.finish();
    }

    {
        // The coherence half of MPT-3, priced under its own name: the SAME walk
        // with a real writer on every stamp cell. The difference between this
        // and `present_latency_scan` is the cross-thread cost the product pays
        // and an uncontended microbenchmark cannot see.
        let mut group = c.benchmark_group("workspace_scaling_seams");
        for (tabs, panes, b_app) in &mut latency {
            let id = format!("tabs{tabs}_panes{panes}");
            let contenders = Contenders::spawn(b_app);
            group.bench_function(BenchmarkId::new("present_latency_contended", &id), |b| {
                b.iter_custom(|iters| {
                    let mut total = Duration::ZERO;
                    for _ in 0..iters {
                        let t0 = Instant::now();
                        black_box(b_app.present_latency());
                        total += t0.elapsed();
                    }
                    total
                });
            });
            contenders.assert_progressing(&format!("present_latency_contended/{id}"));
            let writes = contenders.join_all();
            assert!(
                writes > 0,
                "present_latency_contended/{id}: the contending writers never ran, \
                 so this arm measured the uncontended path under a contended name"
            );
        }
        group.sample_size(10);
        // THE STALL PROBE for MPT-1, and the one number a concurrency BOUND can
        // make worse. `resize_settle` above times only the MAIN-THREAD span —
        // exactly what a spawn storm inflates and a queue collapses — but a
        // ceiling also decides how many workers drain the queue, and fewer
        // workers means a longer wall-clock convergence. This span is the
        // user-facing whole: the settle PLUS the wait until every pane has its
        // off-screen history back. A fix that wins the first number by losing
        // this one is not a win, and without this arm the finding could not
        // tell the difference.
        for (tabs, panes, b_app, _) in &mut settles {
            let id = format!("tabs{tabs}_panes{panes}");
            group.bench_function(BenchmarkId::new("settle_to_quiesce", &id), |b| {
                b.iter_custom(|iters| {
                    let mut total = Duration::ZERO;
                    for i in 0..iters {
                        assert!(
                            b_app.reflow_quiesce(QUIESCE),
                            "settle_to_quiesce: a settle never converged mid-run"
                        );
                        let (r, cols) = if i % 2 == 0 { GRID_NARROW } else { GRID_WIDE };
                        b_app.set_grid(r, cols);
                        let t0 = Instant::now();
                        b_app.resize_settle();
                        let converged = b_app.reflow_quiesce(QUIESCE);
                        total += t0.elapsed();
                        assert!(
                            converged,
                            "settle_to_quiesce: the settle never converged, so this \
                             span is a timeout and not a convergence"
                        );
                    }
                    total
                });
            });
        }
        // The resize workload's untimed arm, priced under a name that cannot be
        // mistaken for the settle: waiting out the previous settle's workers.
        for (tabs, panes, b_app, _) in &mut settles {
            let id = format!("tabs{tabs}_panes{panes}");
            group.bench_function(BenchmarkId::new("settle_quiesce_arm", &id), |b| {
                b.iter(|| b_app.reflow_quiesce(QUIESCE));
            });
        }
        group.finish();
    }

    {
        // The guard-asserted COUNTS as measurements (1 ns == 1 item), so a
        // count regression is stored and A/B'd exactly like a time regression.
        // These are the numbers MPT-1 moves: hand-offs stay put (the same work
        // is still owed), OS threads and peak workers collapse.
        let mut group = c.benchmark_group("workspace_scaling_volume");
        group
            .warm_up_time(Duration::from_millis(1))
            .measurement_time(Duration::from_millis(10))
            .sample_size(10);
        for (tabs, panes, b_app, facts) in &mut settles {
            let id = format!("tabs{tabs}_panes{panes}");
            bench_count(&mut group, &format!("panes/{id}"), b_app.pane_count());
            bench_count(&mut group, &format!("handoffs_per_settle/{id}"), facts.jobs);
            // THE HEADLINE PAIR. `handoffs` is the work owed and does not move;
            // `os_threads` and `peak_workers` are what a bounded hand-off
            // collapses. Recorded as measurements (1 ns == 1 item) so a
            // regression in either is stored and A/B'd like a time regression.
            bench_count(
                &mut group,
                &format!("os_threads_per_settle/{id}"),
                facts.threads.max(1),
            );
            bench_count(
                &mut group,
                &format!("peak_reflow_workers/{id}"),
                facts.peak.max(1),
            );
            bench_count(
                &mut group,
                &format!("declared_ceiling/{id}"),
                BenchApp::reflow_thread_ceiling(facts.jobs),
            );
        }
        group.finish();
    }
}

criterion_group!(benches, workspace_scaling);
criterion_main!(benches);
