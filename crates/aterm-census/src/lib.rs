// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Lexical source censuses and guards over an aterm checkout.
use std::borrow::Cow;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

pub mod export;
pub mod export_scan;
mod guards;
mod held_guard;
pub mod inputs;
pub mod items;
mod lazy_init;
mod lex;
mod lock_order;
mod memo;
pub mod mirror;
pub mod scan_set;
mod scope_census;
pub mod seal;
mod tree;
mod wasm_census;
mod wasm_cfg;
pub use guards::{Finding, GUARDS, GuardReport, Rule, run_guards};
pub use lazy_init::{LAZY_INIT_PRECISION_NOTE, run_lazy_init_census};
pub use lock_order::run_lock_order_census;
pub use memo::{Memo, MemoReport, Rejudged, on_one_worker, pin_code, without_parallel_workers};
pub use scope_census::{SCOPE_PRECISION_NOTE, run_scope_census};
/// Browser `cdylib` root crates re-exported for `aterm-forge`'s `[OB-17]` check.
pub use wasm_census::WASM_ROOT_CRATES;
pub use wasm_census::run_wasm_census;

/// Verdict, transcript, and floor counts produced by a census walk.
pub struct CensusOutcome {
    /// `true` when every obligation and floor held.
    pub ok: bool,
    /// Single-line verdict summary.
    pub verdict: String,
    /// Full census transcript ending with `verdict`.
    pub log: String,
    /// Floor ranges checked on a clean walk.
    pub floors: Vec<Floor>,
    title: Cow<'static, str>,
}

impl CensusOutcome {
    /// Constructs a failing [`CensusOutcome`] with findings in `log`.
    pub(crate) fn red(title: &'static str, log: String, why: impl std::fmt::Display) -> Self {
        Self::finish(false, title, log, format!("{title}: FAILED — {why}"))
    }

    /// Constructs a passing [`CensusOutcome`].
    pub(crate) fn green(title: &'static str, log: String, summary: impl std::fmt::Display) -> Self {
        Self::finish(true, title, log, format!("{title}: GREEN — {summary}"))
    }

    fn finish(ok: bool, title: &'static str, mut log: String, verdict: String) -> Self {
        log.push_str(&verdict);
        log.push('\n');
        Self {
            ok,
            verdict,
            log,
            floors: Vec::new(),
            title: Cow::Borrowed(title),
        }
    }

    /// The outcome as a memo record keeps it.
    fn kept(&self) -> memo::KeptOutcome {
        memo::KeptOutcome {
            ok: self.ok,
            verdict: self.verdict.clone(),
            log: self.log.clone(),
            title: self.title.to_string(),
            floors: self
                .floors
                .iter()
                .map(|f| {
                    (
                        f.what.to_string(),
                        f.seen,
                        *f.range.start(),
                        *f.range.end(),
                        f.missed.to_string(),
                    )
                })
                .collect(),
        }
    }

    /// The outcome a memo record kept ([`Self::kept`]).
    fn from_kept(kept: &memo::KeptOutcome) -> Self {
        Self {
            ok: kept.ok,
            verdict: kept.verdict.clone(),
            log: kept.log.clone(),
            floors: kept
                .floors
                .iter()
                .map(|(what, seen, min, max, missed)| Floor {
                    what: Cow::Owned(what.clone()),
                    seen: *seen,
                    range: *min..=*max,
                    missed: Cow::Owned(missed.clone()),
                })
                .collect(),
            title: Cow::Owned(kept.title.clone()),
        }
    }

    /// Attaches floor checks enforced by [`run_censuses`].
    pub(crate) fn with_floors(mut self, floors: Vec<Floor>) -> Self {
        self.floors = floors;
        self
    }

    fn hold_floors(&mut self) {
        let missed: Vec<String> = self
            .floors
            .iter()
            .filter(|f| !f.met())
            .map(|f| format!("  ✗ FAIL [floor] {f}\n"))
            .collect();
        if !self.ok || missed.is_empty() {
            return;
        }
        self.log
            .truncate(self.log.len().saturating_sub(self.verdict.len() + 1));
        self.log.extend(missed.iter().map(String::as_str));
        self.ok = false;
        self.verdict = format!(
            "{}: FAILED — {} floor(s) missed; the walk itself found no violation",
            self.title,
            missed.len()
        );
        self.log.push_str(&self.verdict);
        self.log.push('\n');
    }
}

/// Count observed by a clean walk and its required inclusive range.
#[derive(Debug, Clone)]
pub struct Floor {
    /// Description of the counted item.
    pub what: Cow<'static, str>,
    /// Observed count.
    pub seen: usize,
    /// Required inclusive range.
    pub range: std::ops::RangeInclusive<usize>,
    /// Explanation and repair when `seen` falls outside `range`.
    pub missed: Cow<'static, str>,
}

impl Floor {
    /// Requires `seen` to be at least `min`.
    pub(crate) const fn at_least(
        what: &'static str,
        seen: usize,
        min: usize,
        missed: &'static str,
    ) -> Self {
        Self {
            what: Cow::Borrowed(what),
            seen,
            range: min..=usize::MAX,
            missed: Cow::Borrowed(missed),
        }
    }

    /// Requires `seen` to be zero.
    pub(crate) const fn none(what: &'static str, seen: usize, missed: &'static str) -> Self {
        Self {
            what: Cow::Borrowed(what),
            seen,
            range: 0..=0,
            missed: Cow::Borrowed(missed),
        }
    }

    /// Returns `true` when `seen` is within `range`.
    #[must_use]
    pub fn met(&self) -> bool {
        self.range.contains(&self.seen)
    }
}

impl std::fmt::Display for Floor {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let (min, max) = (*self.range.start(), *self.range.end());
        let bound = match (min, max) {
            (_, usize::MAX) => format!("at least {min}"),
            (0, 0) => "none".to_string(),
            _ => format!("{min}..={max}"),
        };
        write!(
            f,
            "{}: {} (an aterm checkout shows {bound}) — {}",
            self.what, self.seen, self.missed
        )
    }
}

/// The publication engine's forbidden-content baseline as the caller read it, `(where, text)`,
/// or why it has none. A census spawns nothing and reads no network, so whoever runs the
/// censuses reads one commit of the engine (aterm-verify's `engine` module) and hands its
/// baseline in; `capture-privacy` holds test captures to it.
pub type EngineBaseline = Result<(String, String), String>;

/// CLI-selectable census descriptor and runner.
pub struct Census {
    /// Selection flag name without leading dashes.
    pub name: &'static str,
    /// Short description for `--help`.
    pub about: &'static str,
    /// Runner over one walk of a checkout.
    run: fn(&Walk<'_>) -> CensusOutcome,
}

/// What one census run reads: the checkout root, the engine's baseline its caller read, the
/// run's snapshot of the checkout's source ([`Walk::snapshot`]), and the memo run that keeps a
/// record of it, when one does.
pub(crate) struct Walk<'a> {
    pub(crate) root: &'a Path,
    pub(crate) engine: &'a EngineBaseline,
    pub(crate) source: &'a OnceLock<std::sync::Arc<memo::Snapshot>>,
    pub(crate) run: Option<&'a memo::Run>,
}

impl<'a> Walk<'a> {
    /// The run's snapshot of the checkout's source: a memo run's, read at its start, else read
    /// when a census first asks for it, so a census that reads none (all but the guards) reads
    /// only what it walks.
    pub(crate) fn snapshot(&self) -> &'a memo::Snapshot {
        self.source
            .get_or_init(|| std::sync::Arc::new(memo::Snapshot::read(self.root)))
    }

    /// `judge`'s outcome as one memo unit, `id`: the outcome the memo run kept for it while
    /// every question it asked keeps its answer, else judged now and kept. A census that panics
    /// keeps nothing.
    fn whole(&self, id: &str, judge: impl FnOnce() -> CensusOutcome) -> CensusOutcome {
        let Some(run) = self.run else {
            return judge();
        };
        let why = match run.reusable(id, self.snapshot()) {
            Ok(record) => match &record.result {
                memo::Verdict::Outcome(kept) => {
                    let outcome = CensusOutcome::from_kept(kept);
                    run.keep_reused(record);
                    return outcome;
                }
                memo::Verdict::Findings(_) => Rejudged::Changed("its record is a guard's".into()),
            },
            Err(why) => why,
        };
        let (judged, trace) = memo::unit(&run.shared, || {
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(judge))
        });
        match judged {
            Ok(outcome) => {
                run.keep_judged(
                    id,
                    why,
                    Some((trace, memo::Verdict::Outcome(outcome.kept()))),
                );
                outcome
            }
            Err(panic) => {
                run.keep_judged(id, why, None);
                std::panic::resume_unwind(panic)
            }
        }
    }
}

/// Every census, in the order a run prints them (they run in parallel).
pub const CENSUSES: &[Census] = &[
    Census {
        name: "mainloop",
        about: "main-thread reach to unbounded work or a frame wait; unbounded work under a held \
                `term` guard (OB-1..OB-6, OB-22, OB-23)",
        run: |walk| walk.whole("census:mainloop", || run_mainloop_census(walk.root)),
    },
    Census {
        name: "locks",
        about: "lock-order cycles across the GUI process (OB-7)",
        run: |walk| walk.whole("census:locks", || run_lock_order_census(walk.root)),
    },
    Census {
        name: "wasm",
        about: "wasm reach to unbounded work; no threads (OB-8..OB-10, OB-12)",
        run: |walk| walk.whole("census:wasm", || run_wasm_census(walk.root)),
    },
    Census {
        name: "scope",
        about: "window-scoped enforcers that multiply (OB-13..OB-16)",
        run: |walk| walk.whole("census:scope", || run_scope_census(walk.root)),
    },
    Census {
        name: "lazy-init",
        about: "lazy cells re-entered by their own initializer (OB-19..OB-21)",
        run: |walk| walk.whole("census:lazy-init", || run_lazy_init_census(walk.root)),
    },
    Census {
        name: "guards",
        about: "source rules no type or lint can hold (aterm_census::GUARDS)",
        run: guards::run_guard_census,
    },
];

/// Runs `selected` censuses in parallel over `root`, with the engine's baseline `engine`, and
/// enforces their floors. With `memo`, each guard rule and each other census reuses the result
/// the memo kept for it while every question it asked of the checkout keeps its answer, and the
/// run keeps its own record there ([`Memo`]); the report says what was reused.
#[must_use]
pub fn run_censuses(
    root: &Path,
    selected: &[&Census],
    engine: &EngineBaseline,
    memo: Option<&Memo>,
) -> (Vec<CensusOutcome>, Option<MemoReport>) {
    let Some(memo) = memo else {
        let walk = Walk {
            root,
            engine,
            source: &OnceLock::new(),
            run: None,
        };
        return (walk_censuses(&walk, selected), None);
    };
    let (run, snapshot) = memo::Run::start(root, Some(memo), engine);
    let source = OnceLock::from(std::sync::Arc::clone(&snapshot));
    let walk = Walk {
        root,
        engine,
        source: &source,
        run: run.keeps().then_some(&run),
    };
    let outcomes = memo::loader(&run.shared, || walk_censuses(&walk, selected));
    let report = run.finish(&snapshot);
    (outcomes, Some(report))
}

/// `selected` over `walk`, each on its own thread, floors enforced.
fn walk_censuses(walk: &Walk<'_>, selected: &[&Census]) -> Vec<CensusOutcome> {
    memo::threads(|scope| {
        let running: Vec<_> = selected
            .iter()
            .map(|&census| {
                scope.spawn_named(format!("census-{}", census.name), 8 << 20, move || {
                    (census.run)(walk)
                })
            })
            .collect();
        running
            .into_iter()
            .zip(selected)
            .map(|(handle, census)| {
                let joined = handle.map_err(|e| e.to_string()).and_then(|h| {
                    h.join()
                        .map_err(|_| "it panicked (message above)".to_string())
                });
                let mut outcome = joined.unwrap_or_else(|why| {
                    CensusOutcome::red(
                        census.name,
                        format!("=== {} census ===\n", census.name),
                        format!("the census could not run: {why}"),
                    )
                });
                outcome.hold_floors();
                outcome
            })
            .collect()
    })
}

/// Run one census on the calling thread, with no memo, enforcing the same floor and panic policy
/// as the parallel runner. Its reads stay on this thread and the threads it starts through
/// [`memo::threads`], so an input capture around the call ([`inputs::capture`]) holds every one.
pub fn run_census(root: &Path, census: &Census, engine: &EngineBaseline) -> CensusOutcome {
    let walk = Walk {
        root,
        engine,
        source: &OnceLock::new(),
        run: None,
    };
    let mut outcome =
        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| (census.run)(&walk)))
            .unwrap_or_else(|_| {
                CensusOutcome::red(census.name, String::new(), "it panicked (message above)")
            });
    outcome.hold_floors();
    outcome
}

/// Runs every census over `root` in parallel and returns each outcome under its
/// name, in [`CENSUSES`] order.
#[must_use]
pub fn run_all(root: &Path, engine: &EngineBaseline) -> Vec<(&'static str, CensusOutcome)> {
    let every: Vec<&Census> = CENSUSES.iter().collect();
    CENSUSES
        .iter()
        .map(|c| c.name)
        .zip(run_censuses(root, &every, engine, None).0)
        .collect()
}

/// The walker's limits, printed after a red verdict that found an
/// unbounded-work site or a frame wait.
pub const PRECISION_NOTE: &str = "    PRECISION / SCOPE (this census's limits):
      - LEXICAL, name-based: fns are segmented by rustfmt's
        closing-brace-at-fn-indent layout and calls matched as `<ident>(`,
        with no type, trait or borrow information. Same-named fns merge into
        one node (an over-approximation, sound for a fail-closed census).
        Macro-generated calls, fn pointers and aliasing can evade it; a
        synchronous `term_lock(..).resize(..)` on a main-thread path is
        caught, with its path printed.
      - SCOPE: the call graph covers non-test `crates/aterm-gui/src` and
        `crates/aterm-gui-*/src` (OB-22's held-guard scan also reads
        `crates/aterm-control/src`), plus one modeled `term_lock` hop into
        Terminal methods. Test, bench and proof files are dropped by
        path; an in-file item is blanked only when its `cfg` entails `test`
        (`all(test, ..)` yes; `any(test, unix)` and `not(test)` no). The walk
        does not enter aterm-core or aterm-grid: the unbounded sinks are
        pinned by `// COST: UNBOUNDED(<dim>)` markers, swept over the derived
        GUI-process closure (workspace src/ only, the OB-7 scan set) and held
        to the registry both ways. Markers outside the closure are reported,
        never registry-checked.
";

struct UnboundedSink {
    /// The fn name as it appears at its `fn <symbol>` definition.
    symbol: &'static str,
    /// The cost dimension named in the marker (documentation + failure text).
    dim: &'static str,
    /// Repo-relative file the marked definition lives in (for the failure text).
    def_file: &'static str,
}

const UNBOUNDED_REGISTRY: &[UnboundedSink] = &[
    UnboundedSink {
        symbol: "reflow_scrollback_lines",
        dim: "session-history-cells",
        def_file: "crates/aterm-grid/src/grid/scrollback_reflow.rs",
    },
    UnboundedSink {
        symbol: "take_scrollback_lines",
        dim: "ring+tiered-history-lines",
        def_file: "crates/aterm-grid/src/grid/scrollback_reflow.rs",
    },
    UnboundedSink {
        symbol: "resize_with_reflow_mode",
        dim: "scrollback-width-reflow",
        def_file: "crates/aterm-grid/src/grid/reflow.rs",
    },
    UnboundedSink {
        symbol: "diskwalk_slice",
        dim: "entries",
        def_file: "crates/aterm-diskwalk/src/walk.rs",
    },
    UnboundedSink {
        symbol: "diskwalk_size_full",
        dim: "entries",
        def_file: "crates/aterm-diskwalk/src/walk.rs",
    },
];

const MAIN_THREAD_ROOTS: &[&str] = &[
    "new_events",
    "resumed",
    "user_event",
    "window_event",
    "about_to_wait",
    "flush_pending_resize",
];

const UNBOUNDED_TERM_METHODS: &[&str] = &["resize"];

struct OffloadBoundary {
    /// The method name as called on the `term_lock` guard.
    symbol: &'static str,
    /// Repo-relative file holding the `fn <symbol>` definition (existence-checked).
    def_file: &'static str,
    /// The audited justification: why this is genuinely an offload boundary.
    justification: &'static str,
}

const OFFLOAD_ALLOWLIST: &[OffloadBoundary] = &[
    OffloadBoundary {
        symbol: "resize_offloading_scrollback",
        def_file: "crates/aterm-core/src/terminal/callback_setters.rs",
        justification: "The detach half of the L0 fix: O(1) detach of the tiered \
            scrollback store BEFORE the resize, so the synchronous reflow under the \
            lock touches only the bounded in-memory ring (O(ring), independent of \
            session lifetime). The unbounded decompress+rewrap leaves as a `Send` \
            PendingScrollbackReflow for a worker (enforced off-lock by construction: \
            `reflow()` consumes the detached value).",
    },
    OffloadBoundary {
        symbol: "finish_resize_offload",
        def_file: "crates/aterm-core/src/terminal/callback_setters.rs",
        justification: "The re-attach half: O(1) store re-attach plus a bounded \
            lazy-buffer drain (O(window output), not O(history)) under a brief lock, \
            after the worker did the unbounded rewrap off-thread; when the width changed \
            meanwhile it re-detaches and returns a new job for the worker. Guarded against \
            clobbering (reset raced) and against resurrecting erased history \
            (clear_gen check).",
    },
    OffloadBoundary {
        symbol: "abort_resize_offload",
        def_file: "crates/aterm-core/src/terminal/callback_setters.rs",
        justification: "The wedge-recovery half: O(1) flag-clear + bounded \
            lazy-buffer discard when a reflow worker dies and will never re-attach. \
            Without it the detach window stays open forever (unbounded lazy growth).",
    },
    OffloadBoundary {
        symbol: "reattach_reflowed_scrollback",
        def_file: "crates/aterm-grid/src/grid/scrollback_offload.rs",
        justification: "The Grid-level re-attach that `finish_resize_offload` reaches \
            through `reattach_reflowed_scrollback_or_redetach`: O(1) attach of the \
            already-rewrapped store + bounded lazy drain. \
            Registered so GUI code driving a Grid directly (no Terminal wrapper) \
            still has a sanctioned re-attach shape; it IS the underlying boundary, \
            not a suppression.",
    },
];

const WHOLE_FRAME_WAITS: &[&str] = &["term_lock_whole", "term_lock_whole_when"];

const WAITING_READ_ON: &[&str] = &["ReadOn::Control", "ReadOn::Committed"];

struct WholeFrameGate {
    /// The fn name at its `fn <symbol>` definition.
    symbol: &'static str,
    /// Repo-relative file holding the definition (existence-checked).
    def_file: &'static str,
    /// Why the event loop's call through it never waits for a frame.
    justification: &'static str,
}

const WHOLE_FRAME_GATES: &[WholeFrameGate] = &[
    WholeFrameGate {
        symbol: "input_if_fenced_observed",
        def_file: "crates/aterm-gui/src/control_input.rs",
        justification: "Its `needs` is `on.waits() && fence.reads_rows()`. The event loop's \
            fenced presses (the footer's slash press, a worker's launch line) pass \
            ReadOn::EventLoop, whose waits() is false, so the hold is taken as the screen stands \
            and a row fence on an unfinished frame is refused (frame_unfinished_now); only a \
            control thread waits, and an event-loop fn naming a waiting arm is itself red.",
    },
    WholeFrameGate {
        symbol: "search_terminal",
        def_file: "crates/aterm-gui/src/control_query.rs",
        justification: "Its `needs` is `on.waits()`. The find bar searches with \
            ReadOn::EventLoop, whose waits() is false, so its plan hold is taken as the screen \
            stands; only the `search` verb's control thread waits.",
    },
];

/// A fn the event loop only starts a thread for, which the lexical call graph reads as a call: what
/// it reaches runs off the loop, so a whole-frame read there parks that thread alone.
const OFF_LOOP_ENTRIES: &[WholeFrameGate] = &[
    WholeFrameGate {
        symbol: "serve_here",
        def_file: "crates/aterm-gui/src/control_lanes.rs",
        justification: "A control connection is served here, by `Lanes::drive` on the lane thread \
            `spawn_lane_thread` started for it (or the wait lane it was handed to); the event loop \
            never serves a connection, it only posts to the lanes.",
    },
    WholeFrameGate {
        symbol: "run_worker_launch",
        def_file: "crates/aterm-gui/src/task_launch.rs",
        justification: "A worker's launch is prepared here, on the `aterm-worker-<local>` thread \
            `launch_worker` spawns for it and nowhere else; the event loop only starts that \
            thread.",
    },
];

/// Whether `fits` holds at the start of one of `needle`'s non-overlapping matches in `hay`,
/// tried left to right: the starts `hay.match_indices(needle)` yields. A `memchr` scan for
/// `needle`'s first char finds them, where `match_indices` builds a two-way searcher per call
/// whose setup outweighs the scan of one short line.
fn any_match(hay: &str, needle: &str, mut fits: impl FnMut(usize) -> bool) -> bool {
    let Some(first) = needle.chars().next() else {
        return hay.match_indices(needle).any(|(at, _)| fits(at));
    };
    let mut from = 0;
    while let Some(rel) = hay[from..].find(first) {
        let at = from + rel;
        if !hay[at..].starts_with(needle) {
            from = at + first.len_utf8();
        } else if fits(at) {
            return true;
        } else {
            from = at + needle.len();
        }
    }
    false
}

/// Whether `needle` occurs in `hay`: [`str::contains`] by [`any_match`]'s scan.
fn mentions(hay: &str, needle: &str) -> bool {
    any_match(hay, needle, |_| true)
}

fn first_naming<'a>(
    body: &'a [String],
    names: &[&str],
    fits: impl Fn(&str, &str) -> bool,
) -> Option<&'a str> {
    let ident = |c: char| c.is_alphanumeric() || c == '_';
    body.iter()
        .find(|line| {
            names.iter().any(|name| {
                any_match(line, name, |at| {
                    line[..at].chars().next_back().is_none_or(|c| !ident(c))
                        && fits(&line[..at], &line[at + name.len()..])
                })
            })
        })
        .map(|line| line.trim())
}

fn whole_frame_wait(body: &[String]) -> Option<&str> {
    first_naming(body, WHOLE_FRAME_WAITS, |_, rest| rest.starts_with('('))
}

fn waiting_read_on(body: &[String]) -> Option<&str> {
    first_naming(body, WAITING_READ_ON, |before, rest| {
        let path = |c: char| c.is_alphanumeric() || c == '_' || c == ':';
        let before = before.trim_end_matches(path).trim_end();
        rest.trim_start().starts_with([',', ')', ';'])
            && !before.ends_with("==")
            && !before.ends_with("!=")
    })
}

/// `path` as text with `/` between its components on every host: the spelling every
/// registration, span and pattern of the census uses, where Windows joins with `\`.
pub(crate) fn slash(path: &Path) -> String {
    let text = path.to_string_lossy();
    if cfg!(windows) {
        text.replace('\\', "/")
    } else {
        text.into_owned()
    }
}

/// `path` relative to `root`, spelled as [`slash`] spells it: the census's name for a file.
pub(crate) fn rel_path(root: &Path, path: &Path) -> String {
    slash(path.strip_prefix(root).unwrap_or(path))
}

/// Returns `true` if `path` is a test-only source file excluded from implementation scans.
pub fn is_test_file(path: &Path) -> bool {
    let s = slash(path);
    s.contains("/tests/")
        || s.ends_with("_tests.rs")
        || path
            .file_name()
            .is_some_and(|n| n == "tests.rs" || n.to_string_lossy().starts_with("tests_"))
        || s.contains("/benches/")
        || s.ends_with("/proofs.rs")
        || s.contains("proofs_")
}

/// The files among `files` that only a test build compiles, as only a test build compiles a
/// `tests.rs` ([`is_test_file`]): the files a `#[cfg(test)] mod <name>;` in one of them brings
/// in, which are the module's own file and, where the module has a folder, every file under it.
/// A file that some other declaration also brings in, from a file that is not itself
/// test-only, stays in, and so do the modules it declares in turn. That other declaration is
/// typically `#[cfg(not(test))] mod <name>;` beside the gated one.
///
/// Only a declaration at the start of a line is read, with a literal `#[path]` honoured.
/// Inline and indented declarations are outside this textual scan; this is not a full Rust
/// module-resolution or cfg analysis. Comments and literal contents cannot declare modules.
pub fn test_gated_module_files(files: &[PathBuf]) -> std::collections::BTreeSet<PathBuf> {
    // Each declaration: the file it is in, whether it is test-only, the module's own file, and
    // the folder its submodules sit in.
    let mut decls: Vec<(&PathBuf, bool, PathBuf, Option<PathBuf>)> = Vec::new();
    for file in files {
        let (Ok(text), Some(dir)) = (tree::read_to_string(file), file.parent()) else {
            continue;
        };
        for decl in mod_decls(&text) {
            let (module_file, folder) = if let Some(path) = decl.path {
                let module_file = dir.join(path);
                let folder = (module_file.file_name() == Some("mod.rs".as_ref()))
                    .then(|| module_file.parent().map(Path::to_path_buf))
                    .flatten();
                (module_file, folder)
            } else {
                let owns_folder = file
                    .file_name()
                    .is_some_and(|n| n == "mod.rs" || n == "lib.rs" || n == "main.rs");
                let base = match (owns_folder, file.file_stem()) {
                    (false, Some(stem)) => dir.join(stem),
                    _ => dir.to_path_buf(),
                };
                let flat = base.join(format!("{}.rs", decl.name));
                let folder = base.join(&decl.name);
                let module_file = if files.contains(&flat) {
                    flat
                } else {
                    folder.join("mod.rs")
                };
                (module_file, Some(folder))
            };
            decls.push((file, decl.test_only, module_file, folder));
        }
    }
    let mut gated: std::collections::BTreeSet<PathBuf> = files
        .iter()
        .filter(|f| {
            decls.iter().any(|(_, test_only, module_file, folder)| {
                *test_only
                    && (*f == module_file || folder.as_ref().is_some_and(|d| f.starts_with(d)))
            })
        })
        .cloned()
        .collect();
    // A module a file outside the test-only set declares for every build is shipped, and so is
    // what that module declares: repeat until nothing more comes back.
    loop {
        let shipped: Vec<&PathBuf> = decls
            .iter()
            .filter(|(from, test_only, _, _)| !test_only && !gated.contains(*from))
            .map(|(_, _, module_file, _)| module_file)
            .collect();
        let before = gated.len();
        gated.retain(|f| !shipped.contains(&f));
        if gated.len() == before {
            return gated;
        }
    }
}

/// One `mod <name>;` declaration: its name, its `#[path]` if it has one, and whether
/// `#[cfg(test)]` is among its attributes.
struct ModDecl {
    name: String,
    path: Option<String>,
    test_only: bool,
}

/// Each `mod <name>;` declaration that starts a line of `text`, with the attributes on the
/// lines just above it (comment lines may stand between them) and on its own line.
fn mod_decls(text: &str) -> Vec<ModDecl> {
    let lexed = lex::lex(text);
    let lines: Vec<&str> = lexed.code.lines().collect();
    let sources: Vec<&str> = lexed.text.lines().collect();
    let spans = lex::outer_attributes(text);
    let line_starts: Vec<usize> = std::iter::once(0)
        .chain(text.match_indices('\n').map(|(at, _)| at + 1))
        .collect();
    let mut out = Vec::new();
    for (i, line) in lines.iter().enumerate() {
        if line.starts_with(char::is_whitespace) {
            continue;
        }
        let mut attrs: Vec<&str> = Vec::new();
        let mut item = *line;
        let mut source = sources[i];
        while let Some(end) = item.strip_prefix("#[").and_then(|_| item.find(']')) {
            attrs.push(&source[..=end]);
            item = item[end + 1..].trim_start();
            source = source[end + 1..].trim_start();
        }
        let item = item.split("//").next().unwrap_or_default().trim();
        let item = match item.strip_prefix("pub") {
            Some(scoped) if scoped.starts_with('(') => scoped
                .split_once(')')
                .map_or("", |(_, after)| after.trim_start()),
            Some(public) => public.trim_start(),
            None => item,
        };
        let Some(name) = item
            .strip_prefix("mod ")
            .and_then(|m| m.trim().strip_suffix(';'))
            .map(str::trim)
        else {
            continue;
        };
        if name.is_empty() || !name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
            continue;
        }
        let item_start = line_starts[i];
        let mut before = item_start;
        while let Some(&(start, end)) = spans.iter().rev().find(|&&(_, end)| end <= before) {
            if !lexed.code[end..before].trim().is_empty() {
                break;
            }
            attrs.push(&lexed.text[start..end]);
            before = start;
        }
        let path = attrs.iter().find_map(|attr| {
            attr.strip_prefix("#[path")
                .and_then(|a| a.trim_start().strip_prefix('='))
                .and_then(|a| a.trim().strip_suffix(']'))
                .and_then(|a| a.trim().strip_prefix('"'))
                .and_then(|a| a.strip_suffix('"'))
                .map(str::to_string)
        });
        if path.is_none() && attrs.iter().any(|attr| attr.contains("path")) {
            // An unrecognized path spelling must not exclude a different, default-path file.
            continue;
        }
        out.push(ModDecl {
            name: name.to_string(),
            path,
            test_only: attrs.iter().any(|attr| cfg_attr_never_ships(attr)),
        });
    }
    out
}

/// Recursively collects `*.rs` files under `dir`, skipping `target/` and hidden directories.
pub fn collect_rs_files(dir: &Path, out: &mut Vec<PathBuf>) -> std::io::Result<()> {
    let entries = match tree::read_dir_typed(dir) {
        Ok(entries) => entries,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(e),
    };
    for entry in entries {
        let (path, is_dir) = entry?;
        if is_dir {
            let name = path
                .file_name()
                .map_or(&b""[..], std::ffi::OsStr::as_encoded_bytes);
            if name == b"target" || matches!(name, [b'.', ..]) {
                continue;
            }
            collect_rs_files(&path, out)?;
        } else if path
            .extension()
            .map_or(&b""[..], std::ffi::OsStr::as_encoded_bytes)
            == b"rs"
        {
            out.push(path);
        }
    }
    Ok(())
}

struct GuiFn {
    name: String,
    /// Repo-relative `file:line` of the definition (for the failure path).
    span: String,
    body: Vec<String>,
}

/// Returns `src` directories of `crates/aterm-gui-*` split-out libraries under `root`.
pub(crate) fn gui_split_src_dirs(root: &Path) -> Vec<PathBuf> {
    let mut dirs: Vec<PathBuf> = tree::read_dir(&root.join("crates"))
        .into_iter()
        .flatten()
        .flatten()
        .filter(|path| {
            path.file_name()
                .is_some_and(|name| name.to_string_lossy().starts_with("aterm-gui-"))
        })
        .map(|path| path.join("src"))
        .filter(|src| tree::is_dir(src))
        .collect();
    dirs.sort();
    dirs
}

fn gui_source_files(root: &Path) -> Vec<PathBuf> {
    let mut files = Vec::new();
    let _ = collect_rs_files(&root.join("crates/aterm-gui/src"), &mut files);
    for src in gui_split_src_dirs(root) {
        let _ = collect_rs_files(&src, &mut files);
    }
    files.retain(|p| !is_test_file(p));
    files.sort();
    files
}

fn mask_unshipped_items(text: &str) -> String {
    crate::lock_order::mask_items_where(text, &|attr| cfg_attr_never_ships(attr))
}

fn shipped_lines(code: &str) -> Vec<bool> {
    crate::lock_order::unmasked_lines(code, &|attr| cfg_attr_never_ships(attr))
}

fn cfg_attr_never_ships(attr: &str) -> bool {
    let Some(rest) = attr.strip_prefix("#[cfg(") else {
        return false;
    };
    let Some(pred) = rest.strip_suffix(")]") else {
        return false;
    };
    cfg_pred_entails_unshipped(pred)
}

/// The shared strict evaluator is also used by closure-anchor attributes.
fn cfg_pred_entails_unshipped(pred: &str) -> bool {
    crate::wasm_cfg::when_test_disabled(pred) == crate::wasm_cfg::Truth::False
}

fn split_top_level_commas(args: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let mut depth = 0usize;
    let mut in_str = false;
    let mut start = 0usize;
    for (i, c) in args.char_indices() {
        match c {
            '"' => in_str = !in_str,
            '(' if !in_str => depth += 1,
            ')' if !in_str => depth = depth.saturating_sub(1),
            ',' if !in_str && depth == 0 => {
                let piece = args[start..i].trim();
                if !piece.is_empty() {
                    out.push(piece);
                }
                start = i + c.len_utf8();
            }
            _ => {}
        }
    }
    let piece = args[start..].trim();
    if !piece.is_empty() {
        out.push(piece);
    }
    out
}

fn strip_line_comment(line: &str) -> &str {
    let t = line.trim_start();
    if t.starts_with("//") {
        return "";
    }
    match line.find("//") {
        Some(i) => &line[..i],
        None => line,
    }
}

fn ident_after_fn(after_fn: &str) -> Option<String> {
    let rest = after_fn.trim_start();
    let len = rest
        .find(|c: char| !c.is_alphanumeric() && c != '_')
        .unwrap_or(rest.len());
    if len == 0 {
        None
    } else {
        Some(rest[..len].to_string())
    }
}

fn parse_fn_def(line: &str) -> Option<(usize, String)> {
    let indent = line.len() - line.trim_start().len();
    let mut rest = line.trim_start();
    loop {
        if let Some(after) = rest.strip_prefix("fn ") {
            return ident_after_fn(after).map(|n| (indent, n));
        }
        let mut advanced = false;
        for kw in [
            "pub(crate) ",
            "pub(super) ",
            "pub(self) ",
            "pub ",
            "const ",
            "async ",
            "unsafe ",
            "default ",
        ] {
            if let Some(s) = rest.strip_prefix(kw) {
                rest = s.trim_start();
                advanced = true;
                break;
            }
        }
        if !advanced
            && let Some(s) = rest.strip_prefix("pub(in ")
            && let Some((_, after)) = s.split_once(") ")
        {
            rest = after.trim_start();
            advanced = true;
        }
        if !advanced && let Some(s) = rest.strip_prefix("extern ") {
            rest = s.trim_start().trim_start_matches('"');
            rest = rest
                .split_once('"')
                .map(|(_, r)| r.trim_start())
                .unwrap_or(rest);
            advanced = true;
        }
        if !advanced {
            return None;
        }
    }
}

pub(crate) fn is_closing_brace_at_indent(line: &str, indent: usize) -> bool {
    let bytes = line.as_bytes();
    bytes.len() == indent + 1 && bytes[indent] == b'}' && bytes[..indent].iter().all(|&b| b == b' ')
}

fn parse_source_fns(text: &str, rel: &str, out: &mut Vec<GuiFn>) {
    let lines: Vec<&str> = text.lines().collect();
    let mut i = 0;
    while i < lines.len() {
        let Some((indent, name)) = parse_fn_def(lines[i]) else {
            i += 1;
            continue;
        };
        if is_bodyless_declaration(&lines, i) {
            i += 1;
            continue;
        }
        let def_line = strip_line_comment(lines[i]);
        let opens = def_line.matches('{').count();
        let single_line = opens > 0 && opens == def_line.matches('}').count();
        let mut end = i;
        if !single_line {
            let mut j = i + 1;
            while j < lines.len() {
                if is_closing_brace_at_indent(lines[j], indent) {
                    end = j;
                    break;
                }
                j += 1;
            }
        }
        let body: Vec<String> = lines[i..=end]
            .iter()
            .map(|l| strip_line_comment(l).to_string())
            .collect();
        out.push(GuiFn {
            name,
            span: format!("{rel}:{}", i + 1),
            body,
        });
        i = end + 1;
    }
}

/// Returns `true` if the `fn` starting at `at` is a body-less declaration ending with `;`.
pub(crate) fn is_bodyless_declaration(lines: &[&str], at: usize) -> bool {
    let mut depth = 0i32;
    for line in lines.iter().skip(at).take(24) {
        for b in strip_line_comment(line).bytes() {
            match b {
                b'(' | b'[' => depth += 1,
                b')' | b']' => depth -= 1,
                b'{' if depth <= 0 => return false,
                b';' if depth <= 0 => return true,
                _ => {}
            }
        }
    }
    false
}

fn ident_ending_at(s: &str, end: usize) -> Option<&str> {
    let bytes = s.as_bytes();
    let mut start = end;
    while start > 0 {
        let c = bytes[start - 1];
        if c.is_ascii_alphanumeric() || c == b'_' {
            start -= 1;
        } else {
            break;
        }
    }
    if start == end {
        None
    } else {
        Some(&s[start..end])
    }
}

fn guard_vars(body: &[String]) -> std::collections::BTreeSet<String> {
    let mut guards = std::collections::BTreeSet::new();
    for line in body {
        let Some(eq) = line
            .find("= term_lock(")
            .or_else(|| line.find("= crate::term_lock_ui("))
            .or_else(|| line.find("= term_lock_ui("))
            .or_else(|| line.find("= crate::term_lock_whole("))
            .or_else(|| line.find("= term_lock_whole("))
            .or_else(|| line.find("= crate::term_lock_whole_when("))
            .or_else(|| line.find("= term_lock_whole_when("))
        else {
            continue;
        };
        let lhs = line[..eq].trim_end();
        let start = lhs
            .rfind(|c: char| !c.is_alphanumeric() && c != '_')
            .map_or(0, |i| i + lhs[i..].chars().next().map_or(0, char::len_utf8));
        let name = &lhs[start..];
        if !name.is_empty() {
            guards.insert(name.to_string());
        }
    }
    guards
}

struct TermHopHazard {
    method: String,
    line: String,
}

fn term_hop_calls(body: &[String]) -> (Vec<TermHopHazard>, usize) {
    // Every acquire `guard_vars` binds and every chain read below names `term_lock`: a body
    // that never does holds no guard and chains on none.
    if !body.iter().any(|line| mentions(line, "term_lock")) {
        return (Vec::new(), 0);
    }
    let guards = guard_vars(body);
    let mut hazards = Vec::new();
    let mut offloads = 0usize;
    let classify = |method: &str, line: &str, hz: &mut Vec<TermHopHazard>, off: &mut usize| {
        if UNBOUNDED_TERM_METHODS.contains(&method) {
            hz.push(TermHopHazard {
                method: method.to_string(),
                line: line.trim().to_string(),
            });
        } else if OFFLOAD_ALLOWLIST.iter().any(|b| b.symbol == method) {
            *off += 1;
        }
    };
    for line in body {
        // A call chained on the lock guard; the `_ui`/`_whole`/`_whole_when` infixes break the plain needle.
        if line.contains("term_lock(")
            || line.contains("term_lock_ui(")
            || line.contains("term_lock_whole(")
            || line.contains("term_lock_whole_when(")
        {
            let mut search = line.as_str();
            while let Some(rel) = search.find(").") {
                let after = &search[rel + 2..];
                let len = after
                    .find(|c: char| !c.is_alphanumeric() && c != '_')
                    .unwrap_or(after.len());
                let method = &after[..len];
                let is_call = after[method.len()..].starts_with('(');
                if is_call && !method.is_empty() {
                    classify(method, line, &mut hazards, &mut offloads);
                }
                search = &search[rel + 2..];
            }
        }
        for g in &guards {
            let needle = format!("{g}.");
            let mut from = 0;
            while let Some(rel) = line[from..].find(&needle) {
                let at = from + rel;
                let boundary = at == 0 || {
                    let prev = line.as_bytes()[at - 1];
                    !(prev.is_ascii_alphanumeric() || prev == b'_')
                };
                let after = &line[at + needle.len()..];
                let len = after
                    .find(|c: char| !c.is_alphanumeric() && c != '_')
                    .unwrap_or(after.len());
                let method = &after[..len];
                let is_call = after[method.len()..].starts_with('(');
                if boundary && is_call && !method.is_empty() {
                    classify(method, line, &mut hazards, &mut offloads);
                }
                from = at + needle.len();
            }
        }
    }
    (hazards, offloads)
}

fn callee_names(body: &[String]) -> std::collections::BTreeSet<String> {
    let mut out = std::collections::BTreeSet::new();
    for line in body {
        let bytes = line.as_bytes();
        for (idx, &b) in bytes.iter().enumerate() {
            if b == b'('
                && let Some(name) = ident_ending_at(line, idx)
            {
                out.insert(name.to_string());
            }
        }
    }
    out
}

struct Marker {
    symbol: String,
    dim: String,
    span: String,
}

fn marker_sweep_file(p: &Path) -> bool {
    !is_test_file(p) && !slash(p).contains("/aterm-census/")
}

fn sweep_markers(root: &Path, files: &[PathBuf]) -> Vec<Marker> {
    let mut found = Vec::new();
    for file in files {
        let Ok(text) = tree::read_to_string(file) else {
            continue;
        };
        if !text.contains("// COST: UNBOUNDED(") {
            continue;
        }
        let rel = crate::rel_path(root, file);
        let lines: Vec<&str> = text.lines().collect();
        for (i, line) in lines.iter().enumerate() {
            let t = line.trim_start();
            let Some(rest) = t.strip_prefix("// COST: UNBOUNDED(") else {
                continue;
            };
            let dim: String = rest.chars().take_while(|c| *c != ')').collect();
            for cand in lines.iter().skip(i + 1) {
                let ct = cand.trim_start();
                if ct.is_empty() || ct.starts_with("//") || ct.starts_with("#[") {
                    continue;
                }
                if let Some((_, name)) = parse_fn_def(cand) {
                    found.push(Marker {
                        symbol: name,
                        dim: dim.clone(),
                        span: format!("{rel}:{}", i + 1),
                    });
                }
                break;
            }
        }
    }
    found
}

fn collect_closure_markers(root: &Path, scan_dirs: &[String]) -> Vec<Marker> {
    let mut files = Vec::new();
    for dir in scan_dirs {
        let _ = collect_rs_files(&root.join(dir), &mut files);
    }
    files.retain(|p| marker_sweep_file(p));
    files.sort();
    sweep_markers(root, &files)
}

fn collect_out_of_closure_markers(root: &Path, scan_dirs: &[String]) -> Vec<Marker> {
    let closure_crate_dirs: std::collections::BTreeSet<&str> = scan_dirs
        .iter()
        .filter_map(|d| d.strip_suffix("/src"))
        .collect();
    let mut files = Vec::new();
    if let Ok(entries) = tree::read_dir(&root.join("crates")) {
        for path in entries.into_iter().flatten() {
            if !tree::is_dir(&path) {
                continue;
            }
            let rel = crate::rel_path(root, &path);
            if closure_crate_dirs.contains(rel.as_str()) {
                continue;
            }
            let _ = collect_rs_files(&path, &mut files);
        }
    }
    files.retain(|p| marker_sweep_file(p));
    files.sort();
    sweep_markers(root, &files)
}

fn fn_defined_in(root: &Path, rel_file: &str, symbol: &str) -> bool {
    let Ok(text) = tree::read_to_string(&root.join(rel_file)) else {
        return false;
    };
    text.lines()
        .any(|l| parse_fn_def(strip_line_comment(l)).is_some_and(|(_, name)| name == symbol))
}

fn append_why_and_repair(log: &mut String) {
    let _ = writeln!(
        log,
        "    WHY THIS IS L0: work that grows with session history (or, for a\n\
         \x20        diskwalk sink, with the entries walked) runs on the winit\n\
         \x20        event-loop thread or under the per-session `term` mutex the\n\
         \x20        event loop takes to draw; input, redraw and every window this\n\
         \x20        loop serves stall until it finishes.\n\
         \x20   HOW TO REPAIR (pick one):\n\
         \x20     1. OFFLOAD (the canonical fix): replace `.resize(rows, cols)` with\n\
         \x20        `.resize_offloading_scrollback(rows, cols)` inside the lock; hand\n\
         \x20        the returned `PendingScrollbackReflow` to a worker (its `reflow()`\n\
         \x20        runs off the lock), re-attach with `.finish_resize_offload(..)`\n\
         \x20        and drive any job it returns the same way, and call\n\
         \x20        `.abort_resize_offload()` if the worker dies. Canonical shape:\n\
         \x20        `resize_panes_scoped_with_active_plan` in\n\
         \x20        crates/aterm-gui/src/app_render.rs.\n\
         \x20     2. BOUND the work: if this site provably never reflows unbounded\n\
         \x20        history (alt-screen grid, fixed-size scratch terminal), use a path\n\
         \x20        that cannot reach the width reflow (e.g. `Grid::resize_no_reflow`)\n\
         \x20        and write down why at the call site.\n\
         \x20     3. NEW BOUNDARY (last resort): wrap the bounded or deferred variant\n\
         \x20        in a dedicated Terminal method and register it in\n\
         \x20        OFFLOAD_ALLOWLIST (crates/aterm-census/src/lib.rs) with a written\n\
         \x20        justification; OB-3 fails an undefined or unjustified entry."
    );
}

const MAINLOOP: &str = "main-loop census";

/// A breadth-first walk over fns by index: the fn each reached fn was first reached from (`None`
/// for a root).
struct Reach {
    from: Vec<Option<Option<usize>>>,
    reached: usize,
}

impl Reach {
    fn new(fns: usize) -> Self {
        Self {
            from: vec![None; fns],
            reached: 0,
        }
    }

    /// Records `idx` as reached from `from`; `false` when it already was.
    fn reach(&mut self, idx: usize, from: Option<usize>) -> bool {
        if self.from[idx].is_some() {
            return false;
        }
        self.from[idx] = Some(from);
        self.reached += 1;
        true
    }

    fn contains(&self, idx: usize) -> bool {
        self.from[idx].is_some()
    }

    /// The names along the walk from its root to `idx`, joined by ` -> `.
    fn path(&self, fns: &[GuiFn], mut idx: usize) -> String {
        let mut path = vec![fns[idx].name.clone()];
        while let Some(Some(p)) = self.from[idx] {
            path.push(fns[p].name.clone());
            idx = p;
        }
        path.reverse();
        path.join(" -> ")
    }
}

/// OB-22's held-guard walk ([`held_guard::scan_tree`]) on a thread of its own, beside the
/// call-graph checks, which share none of its work. The thread carries the census's memo scope
/// ([`memo::threads`]), so what the walk reads is the census unit's question.
struct HeldGuardWalk<'scope>(
    Option<memo::Joined<'scope, Result<held_guard::HeldGuardScan, String>>>,
);

impl<'scope> HeldGuardWalk<'scope> {
    fn start(scope: memo::Threads<'scope, '_>, root: &'scope Path) -> Self {
        let walk = scope.spawn_named(
            "census-mainloop-held-guard".to_string(),
            8 << 20,
            move || held_guard::scan_tree(root),
        );
        Self(walk.ok())
    }

    /// The walk's result, run here when its thread could not start; its panic resumes here.
    fn result(mut self, root: &Path) -> Result<held_guard::HeldGuardScan, String> {
        match self.0.take() {
            Some(walk) => walk
                .join()
                .unwrap_or_else(|panic| std::panic::resume_unwind(panic)),
            None => held_guard::scan_tree(root),
        }
    }
}

impl Drop for HeldGuardWalk<'_> {
    /// The census gives its verdict without the walk when no main-thread root resolves: the
    /// walk's outcome, a panic included, is then dropped and cannot change that verdict.
    fn drop(&mut self) {
        if let Some(walk) = self.0.take() {
            let _ = walk.join();
        }
    }
}

/// Runs the main-loop census (`OB-1`..`OB-6`, `OB-22`, `OB-23`) over `root`.
pub fn run_mainloop_census(root: &Path) -> CensusOutcome {
    memo::threads(|scope| mainloop_census(scope, root))
}

fn mainloop_census<'scope>(scope: memo::Threads<'scope, '_>, root: &'scope Path) -> CensusOutcome {
    let mut log = String::new();
    let mut failures = 0usize;
    let _ = writeln!(
        log,
        "=== main-loop census (L0 freeze class) ===\n\
         \x20   root: {}",
        root.display()
    );

    let scan = match scan_set::derive_gui_scan_set(root) {
        Ok(s) => s,
        Err(e) => {
            let _ = writeln!(
                log,
                "  ✗ FAIL [OB-1] SCAN-SET DERIVATION FAILED — the aterm-gui process closure \
                 cannot be derived from the workspace manifests, so no marker scope is \
                 guessed.\n\
                 \x20       {e}"
            );
            return CensusOutcome::red(MAINLOOP, log, "1 obligation violation(s).");
        }
    };
    let held_walk = HeldGuardWalk::start(scope, root);
    let _ = writeln!(
        log,
        "    marker sweep: the DERIVED GUI-process closure ({} workspace crates, src/ \
         only, the OB-7 scan set); vendored code is not swept. Markers outside the \
         closure are reported below, never registry-checked.",
        scan.scan_dirs.len()
    );

    let markers = collect_closure_markers(root, &scan.scan_dirs);
    for m in &markers {
        if !UNBOUNDED_REGISTRY.iter().any(|u| u.symbol == m.symbol) {
            let _ = writeln!(
                log,
                "  ✗ FAIL [OB-1] `{}` carries a `// COST: UNBOUNDED` marker (at {}) but is \
                 NOT in UNBOUNDED_REGISTRY; register the sink there \
                 (crates/aterm-census/src/lib.rs).",
                m.symbol, m.span
            );
            failures += 1;
        }
    }
    for sink in UNBOUNDED_REGISTRY {
        if !markers.iter().any(|m| m.symbol == sink.symbol) {
            let _ = writeln!(
                log,
                "  ✗ FAIL [OB-1] registered sink `{}` ({}) has LOST its `// COST: UNBOUNDED` \
                 marker at {}: restore the marker at its definition, or drop the registry \
                 entry. Only a marker inside the derived GUI-process closure counts.",
                sink.symbol, sink.dim, sink.def_file
            );
            failures += 1;
        }
    }

    let out_markers = collect_out_of_closure_markers(root, &scan.scan_dirs);
    for m in &out_markers {
        let _ = writeln!(
            log,
            "  • NOTE [OB-1 scope] `// COST: UNBOUNDED({})` marker on fn `{}` at {} is \
             outside the aterm-gui process closure: reported, not registry-checked. It \
             becomes an OB-1 obligation if its crate enters aterm-gui's dependency \
             graph.",
            m.dim, m.symbol, m.span
        );
    }

    for b in OFFLOAD_ALLOWLIST {
        if b.justification.trim().is_empty() {
            let _ = writeln!(
                log,
                "  ✗ FAIL [OB-3] offload boundary `{}` has an EMPTY justification; every \
                 allowlist entry says why it is a boundary.",
                b.symbol
            );
            failures += 1;
        }
        if !fn_defined_in(root, b.def_file, b.symbol) {
            let _ = writeln!(
                log,
                "  ✗ FAIL [OB-3] offload boundary `{}` is allowlisted but `fn {}` is no longer \
                 defined in {}: update def_file if it moved, or remove the entry.",
                b.symbol, b.symbol, b.def_file
            );
            failures += 1;
        }
    }

    let mut fns: Vec<GuiFn> = Vec::new();
    for file in gui_source_files(root) {
        let Ok(text) = tree::read_to_string(&file) else {
            continue;
        };
        let rel = crate::rel_path(root, &file);
        parse_source_fns(&mask_unshipped_items(&text), &rel, &mut fns);
    }
    let mut by_name: std::collections::BTreeMap<String, Vec<usize>> = Default::default();
    for (idx, f) in fns.iter().enumerate() {
        by_name.entry(f.name.clone()).or_default().push(idx);
    }
    let edges: Vec<std::collections::BTreeSet<String>> =
        fns.iter().map(|f| callee_names(&f.body)).collect();
    let hop: Vec<(Vec<TermHopHazard>, usize)> =
        fns.iter().map(|f| term_hop_calls(&f.body)).collect();
    let offload_sites: usize = hop.iter().map(|(_, n)| *n).sum();
    let hazards: Vec<&Vec<TermHopHazard>> = hop.iter().map(|(h, _)| h).collect();

    let mut parent = Reach::new(fns.len());
    let mut queue: std::collections::VecDeque<usize> = Default::default();
    let mut roots_found = 0;
    for r in MAIN_THREAD_ROOTS {
        if let Some(indices) = by_name.get(*r) {
            roots_found += 1;
            for &idx in indices {
                if parent.reach(idx, None) {
                    queue.push_back(idx);
                }
            }
        } else {
            let _ = writeln!(
                log,
                "  ✗ FAIL [OB-2] declared main-thread root `{r}` not found in \
                 crates/aterm-gui/src or crates/aterm-gui-*/src; if the handler was renamed, \
                 rename it in MAIN_THREAD_ROOTS (crates/aterm-census/src/lib.rs)."
            );
            failures += 1;
        }
    }
    if roots_found == 0 {
        let _ = writeln!(
            log,
            "  ✗ FAIL [OB-2] no MAIN_THREAD_ROOTS resolved — the census walked nothing \
             (parser broke, or this root is not an aterm checkout?)."
        );
        return CensusOutcome::red(
            MAINLOOP,
            log,
            format!("{} obligation violation(s).", failures + 1),
        );
    }
    // Each fn's callees that name a GUI fn, in `edges` order, with the fns so named: both walks
    // below follow these, so each name is looked up once.
    let calls: Vec<Vec<(&str, &[usize])>> = edges
        .iter()
        .map(|callees| {
            callees
                .iter()
                .filter_map(|c| by_name.get(c).map(|defs| (c.as_str(), defs.as_slice())))
                .collect()
        })
        .collect();
    while let Some(cur) = queue.pop_front() {
        for &(_, defs) in &calls[cur] {
            for &cidx in defs {
                if parent.reach(cidx, Some(cur)) {
                    queue.push_back(cidx);
                }
            }
        }
    }
    let reconstruct = |idx: usize| parent.path(&fns, idx);

    let mut hazard_hits = 0usize;
    for idx in 0..fns.len() {
        if !parent.contains(idx) || hazards[idx].is_empty() {
            continue;
        }
        for hz in hazards[idx] {
            let _ = writeln!(
                log,
                "  ✗ FAIL [OB-5] L0 OBLIGATION VIOLATED — a main-thread root synchronously \
                 reaches an\n\
                 \x20        UNBOUNDED sink under the per-session `term` mutex (the L0 freeze \
                 class).\n\
                 \x20   PATH:  {}\n\
                 \x20   SITE:  {} (fn `{}`)\n\
                 \x20   CALL:  `.{}(..)` on a `term_lock(..)` guard\n\
                 \x20   LINE:  {}\n\
                 \x20   SINK:  Terminal::{} -> Grid::resize -> Grid::resize_with_reflow_mode\n\
                 \x20          [COST: UNBOUNDED({}) @ {}], whose width branch synchronously runs\n\
                 \x20          take_scrollback_lines [UNBOUNDED({})] then reflow_scrollback_lines\n\
                 \x20          [UNBOUNDED({})] over the ENTIRE session history.",
                reconstruct(idx),
                fns[idx].span,
                fns[idx].name,
                hz.method,
                hz.line,
                hz.method,
                sink_dim("resize_with_reflow_mode"),
                sink_file("resize_with_reflow_mode"),
                sink_dim("take_scrollback_lines"),
                sink_dim("reflow_scrollback_lines"),
            );
            append_why_and_repair(&mut log);
            failures += 1;
            hazard_hits += 1;
        }
    }

    for idx in 0..fns.len() {
        if !parent.contains(idx) {
            continue;
        }
        for sink in UNBOUNDED_REGISTRY {
            if edges[idx].contains(sink.symbol) {
                let _ = writeln!(
                    log,
                    "  ✗ FAIL [OB-6] L0 OBLIGATION VIOLATED — a main-thread root reaches a \
                     DIRECT call to\n\
                     \x20        registered unbounded sink `{}` [COST: UNBOUNDED({}) @ {}].\n\
                     \x20   PATH:  {}\n\
                     \x20   SITE:  {} (fn `{}`)",
                    sink.symbol,
                    sink.dim,
                    sink.def_file,
                    reconstruct(idx),
                    fns[idx].span,
                    fns[idx].name,
                );
                append_why_and_repair(&mut log);
                failures += 1;
                hazard_hits += 1;
            }
        }
    }

    for g in WHOLE_FRAME_GATES {
        if g.justification.trim().is_empty() {
            let _ = writeln!(
                log,
                "  ✗ FAIL [OB-23] whole-frame gate `{}` has an EMPTY justification; every gate \
                 says why the event loop's call never waits.",
                g.symbol
            );
            failures += 1;
        }
        if !fn_defined_in(root, g.def_file, g.symbol) {
            let _ = writeln!(
                log,
                "  ✗ FAIL [OB-23] whole-frame gate `{}` is allowlisted but `fn {}` is no longer \
                 defined in {}: update def_file if it moved, or remove the entry.",
                g.symbol, g.symbol, g.def_file
            );
            failures += 1;
        } else if !by_name.get(g.symbol).is_some_and(|defs| {
            defs.iter()
                .any(|&i| whole_frame_wait(&fns[i].body).is_some())
        }) {
            let _ = writeln!(
                log,
                "  ✗ FAIL [OB-23] whole-frame gate `{}` no longer takes a whole-frame read: \
                 remove the entry, or teach `whole_frame_wait` the read's new spelling.",
                g.symbol
            );
            failures += 1;
        }
    }
    for e in OFF_LOOP_ENTRIES {
        if e.justification.trim().is_empty() || !fn_defined_in(root, e.def_file, e.symbol) {
            let _ = writeln!(
                log,
                "  ✗ FAIL [OB-23] off-loop entry `{}` is unjustified or no longer defined in \
                 {}: update def_file if it moved, or remove the entry.",
                e.symbol, e.def_file
            );
            failures += 1;
        }
    }
    let mut on_loop = Reach::new(fns.len());
    let mut queue: std::collections::VecDeque<usize> = MAIN_THREAD_ROOTS
        .iter()
        .filter_map(|r| by_name.get(*r))
        .flatten()
        .copied()
        .filter(|&idx| on_loop.reach(idx, None))
        .collect();
    while let Some(cur) = queue.pop_front() {
        for &(callee, defs) in &calls[cur] {
            if OFF_LOOP_ENTRIES.iter().any(|e| e.symbol == callee) {
                continue;
            }
            for &cidx in defs {
                if on_loop.reach(cidx, Some(cur)) {
                    queue.push_back(cidx);
                }
            }
        }
    }
    let mut frame_waits = 0usize;
    for (idx, f) in fns.iter().enumerate() {
        let name = f.name.as_str();
        if !on_loop.contains(idx)
            || WHOLE_FRAME_WAITS.contains(&name)
            || WHOLE_FRAME_GATES.iter().any(|g| g.symbol == name)
        {
            continue;
        }
        let Some(line) = whole_frame_wait(&f.body).or_else(|| waiting_read_on(&f.body)) else {
            continue;
        };
        let _ = writeln!(
            log,
            "  ✗ FAIL [OB-23] the event loop reaches a whole-frame read, which parks until the \
             program's DEC 2026 frame closes.\n\
             \x20   PATH:  {}\n\
             \x20   SITE:  {} (fn `{}`)\n\
             \x20   LINE:  {line}\n\
             \x20   REPAIR: read with `term_lock` and ask `frame_unfinished_now` (refuse or wait \
             for the next poll on an unfinished frame), or pass a `ReadOn` whose event-loop arm \
             gives `term_lock_whole_when` a false `needs` and register the fn in \
             WHOLE_FRAME_GATES (crates/aterm-census/src/lib.rs) with its justification.",
            on_loop.path(&fns, idx),
            f.span,
            f.name,
        );
        failures += 1;
        frame_waits += 1;
    }

    let held = match held_walk.result(root) {
        Ok(scan) => {
            if scan.acquires == 0 {
                let _ = writeln!(
                    log,
                    "  ✗ FAIL [OB-22] the held-guard walk read {} file(s) and saw ZERO `term` \
                     acquires — a blind walk, not a clean one (renamed acquire, or a parser \
                     regression).",
                    scan.files
                );
                failures += 1;
            }
            for f in &scan.findings {
                let _ = writeln!(
                    log,
                    "  ✗ FAIL [OB-22] L0 OBLIGATION VIOLATED — unbounded work while the \
                     per-session `term` guard is held.\n\
                     \x20   SITE:  {}\n\
                     \x20   LINE:  {}",
                    f.span, f.line
                );
                append_why_and_repair(&mut log);
                failures += 1;
                hazard_hits += 1;
            }
            Some((scan.files, scan.acquires))
        }
        Err(e) => {
            let _ = writeln!(
                log,
                "  ✗ FAIL [OB-22] the held-guard walk could not run: {e} (scope: {}, \
                 crates/aterm-gui-*/src).",
                held_guard::HELD_GUARD_SCOPE.join(", ")
            );
            failures += 1;
            None
        }
    };

    if offload_sites == 0 {
        let _ = writeln!(
            log,
            "  ✗ FAIL [OB-4] ZERO calls to any allowlisted offload boundary ({}) found in \
             crates/aterm-gui/src or crates/aterm-gui-*/src — either the offload was removed \
             (the freeze class is unguarded) or the boundary was renamed (update \
             OFFLOAD_ALLOWLIST with a fresh justification).",
            allowlist_symbols().join("/")
        );
        failures += 1;
    }

    if failures > 0 {
        if hazard_hits > 0 || frame_waits > 0 {
            let _ = write!(log, "{PRECISION_NOTE}");
        }
        return CensusOutcome::red(
            MAINLOOP,
            log,
            format!(
                "{failures} obligation violation(s) ({hazard_hits} unbounded-work site(s), \
                 {frame_waits} frame wait(s))."
            ),
        );
    }
    let (held_files, held_acquires) = held.unwrap_or_default();
    let _ = writeln!(
        log,
        "    [OB-22] no unbounded work under a held `term` guard: {held_files} file(s) of {} + \
         crates/aterm-gui-*/src read, {held_acquires} acquiring line(s).",
        held_guard::HELD_GUARD_SCOPE.join(" + ")
    );
    let _ = writeln!(
        log,
        "    scope: lexical name-based walk of crates/aterm-gui/src + crates/aterm-gui-*/src + one \
         term_lock hop; \
         markers swept over the derived {}-crate GUI-process closure \
         (limits: PRECISION_NOTE in crates/aterm-census/src/lib.rs).",
        scan.scan_dirs.len()
    );
    let summary = format!(
        "{} fn(s) walked from {roots_found} main-thread root(s), none reaching an UNBOUNDED \
         sink synchronously; {offload_sites} offload-boundary call(s); {} sink(s) marked and \
         registered; {} boundaries defined and justified; {} out-of-closure marker(s) \
         reported; {held_acquires} `term` acquire(s), none holding unbounded work; no \
         whole-frame read reached but through {} gate(s) defined and justified.",
        parent.reached,
        UNBOUNDED_REGISTRY.len(),
        OFFLOAD_ALLOWLIST.len(),
        out_markers.len(),
        WHOLE_FRAME_GATES.len(),
    );
    CensusOutcome::green(MAINLOOP, log, summary).with_floors(vec![Floor::at_least(
        "`term` acquires the held-guard walk read",
        held_acquires,
        100,
        "the walk lost most of the GUI's lock sites (a renamed acquire or a parser \
         regression), so OB-22 holds vacuously; teach `held_guard` the new spelling",
    )])
}

fn allowlist_symbols() -> Vec<&'static str> {
    OFFLOAD_ALLOWLIST.iter().map(|b| b.symbol).collect()
}

fn sink_dim(symbol: &str) -> &'static str {
    UNBOUNDED_REGISTRY
        .iter()
        .find(|u| u.symbol == symbol)
        .map(|u| u.dim)
        .unwrap_or("history")
}

fn sink_file(symbol: &str) -> &'static str {
    UNBOUNDED_REGISTRY
        .iter()
        .find(|u| u.symbol == symbol)
        .map(|u| u.def_file)
        .unwrap_or("crates/aterm-grid")
}

/// Returns `(comments_and_literals_blanked, comments_blanked)` views of `source`.
#[must_use]
pub fn rust_source_views(source: &str) -> (String, String) {
    let views = lex::lex(source);
    (views.code, views.text)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scan_set::test_fixtures::synth_tree;

    #[test]
    fn a_tests_part_is_a_test_file_and_a_name_that_only_contains_it_is_not() {
        let test = |p: &str| is_test_file(Path::new(p));
        assert!(test(
            "crates/aterm-shell-integration/src/tests_pwsh_keys.rs"
        ));
        assert!(test("crates/aterm-pty/src/unix_tests.rs"));
        assert!(test("crates/aterm-pty/tests/smoke.rs"));
        assert!(!test("crates/aterm-pty/src/lib.rs"));
        assert!(!test("crates/aterm-pty/src/latests_seen.rs"));
        assert!(!test("crates/aterm-pty/src/tests_dir/lib.rs"));
    }

    #[test]
    fn a_standalone_tests_module_is_a_test_file() {
        for path in [
            "tests.rs",
            "crates/aterm-agent/src/vault_join/automatic/tests.rs",
        ] {
            assert!(is_test_file(Path::new(path)), "{path}");
        }
        for path in [
            "test.rs",
            "crates/aterm-agent/src/contest.rs",
            "crates/aterm-agent/src/automatic.rs",
        ] {
            assert!(!is_test_file(Path::new(path)), "{path}");
        }
    }

    #[test]
    fn a_test_gated_module_and_its_folder_are_test_code_and_nothing_else_is() {
        let lib = "#[cfg(test)]\n/// The sim.\nmod sim;\npub mod shipped;\n\
                   #[cfg(test)]\npub mod both;\n#[cfg(not(test))]\nmod both;\n\
                   #[cfg(test)]\n#[path = \"probe_impl.rs\"]\nmod probe;\n\
                   #[cfg(any(test, debug_assertions))]\nmod seams;\n\
                   #[cfg(test)]\nmod inner;\nmod outer {\n    mod inner;\n}\n";
        let shipped = "#[cfg(test)] pub(crate) mod fixture; // one line\nmod kept;\n";
        let files: Vec<(String, String)> = [
            ("crates/a/src/lib.rs", lib),
            ("crates/a/src/sim/mod.rs", "mod world;\n"),
            ("crates/a/src/sim/world.rs", ""),
            ("crates/a/src/simulate.rs", ""),
            ("crates/a/src/shipped.rs", shipped),
            ("crates/a/src/shipped/fixture.rs", ""),
            ("crates/a/src/shipped/kept.rs", ""),
            ("crates/a/src/probe_impl.rs", ""),
            ("crates/a/src/seams.rs", ""),
            ("crates/a/src/outer/inner.rs", ""),
            ("crates/a/src/inner.rs", ""),
            (
                "crates/a/src/both.rs",
                "mod child;\n#[cfg(test)]\nmod probe;\n",
            ),
            ("crates/a/src/both/child.rs", ""),
            ("crates/a/src/both/probe.rs", ""),
        ]
        .iter()
        .map(|(rel, text)| ((*rel).to_string(), (*text).to_string()))
        .collect();
        let root = synth_tree("aterm-census-gated", "gated", &files);
        let mut all = Vec::new();
        collect_rs_files(&root.join("crates"), &mut all).expect("every entry reads");
        let gated = test_gated_module_files(&all);
        let expected: std::collections::BTreeSet<PathBuf> = [
            "crates/a/src/sim/mod.rs",
            "crates/a/src/sim/world.rs",
            "crates/a/src/shipped/fixture.rs",
            "crates/a/src/probe_impl.rs",
            "crates/a/src/both/probe.rs",
            "crates/a/src/inner.rs",
        ]
        .iter()
        .map(|rel| root.join(rel))
        .collect();
        assert_eq!(gated, expected);
    }

    #[test]
    fn complete_false_cfg_loaders_exclude_files_until_a_live_loader_rescues_them() {
        let lib = "#[cfg(\r\n all(test, unix)\n)]\n#[path = \"dead.rs\"]\nmod hidden;\n#[cfg(not(not(test)))]\n#[path = \"shared.rs\"]\nmod dead_shared;\n#[cfg(any(test, feature = \"x\"))]\n#[path = \"shared.rs\"]\nmod live_shared;\n";
        let lib = lib.replace("\r\n", "\n").replace('\n', "\r\n");
        let files = [
            ("crates/a/src/lib.rs".to_owned(), lib),
            ("crates/a/src/dead.rs".to_owned(), String::new()),
            ("crates/a/src/shared.rs".to_owned(), String::new()),
        ];
        let root = synth_tree("aterm-census-cfg-loaders", "complete", &files);
        let mut all = Vec::new();
        collect_rs_files(&root.join("crates"), &mut all).expect("fixture files");
        assert_eq!(
            test_gated_module_files(&all),
            std::collections::BTreeSet::from([root.join("crates/a/src/dead.rs")])
        );
        let declarations = mod_decls(
            "#[cfg(all(test, broken(,)))]\nmod unknown;\n#[cfg(not(test))]\nmod production;\n",
        );
        assert_eq!(declarations.len(), 2);
        assert!(declarations.iter().all(|decl| !decl.test_only));
    }

    #[test]
    fn quoted_or_commented_module_examples_cannot_remove_shipped_sources() {
        let lib = r###"const EXAMPLE: &str = r#"
#[cfg(test)]
mod live;
"#;
/*
#[cfg(test)]
mod commented;
*/
include!("live.rs");
include!("commented.rs");
"###;
        let files = [
            ("crates/a/src/lib.rs".to_owned(), lib.to_owned()),
            ("crates/a/src/live.rs".to_owned(), String::new()),
            ("crates/a/src/commented.rs".to_owned(), String::new()),
        ];
        let root = synth_tree("aterm-census-quoted-mod", "quoted", &files);
        let mut all = Vec::new();
        collect_rs_files(&root.join("crates"), &mut all).expect("every entry reads");
        assert!(test_gated_module_files(&all).is_empty());
    }

    fn empty_files(name: &str, files: &[&str]) -> crate::tree::memory::Tree {
        let files: Vec<_> = files
            .iter()
            .map(|rel| ((*rel).to_string(), String::new()))
            .collect();
        synth_tree("aterm-census-walk", name, &files)
    }

    #[test]
    fn the_rs_walk_skips_target_and_dot_directories() {
        let root = empty_files(
            "skips",
            &[
                "crates/a/src/lib.rs",
                "crates/a/src/m/x.rs",
                "crates/a/src/notes.md",
                "crates/a/target/debug/build/out.rs",
                "crates/a/.claude/worktrees/w/src/lib.rs",
                "crates/a/src/.scratch/x.rs",
            ],
        );
        let mut files = Vec::new();
        collect_rs_files(&root.join("crates"), &mut files).expect("every entry reads");
        files.sort();
        assert_eq!(
            files,
            [
                root.join("crates/a/src/lib.rs"),
                root.join("crates/a/src/m/x.rs")
            ]
        );
    }

    #[test]
    fn the_rs_walk_stops_where_it_cannot_read() {
        use std::io::ErrorKind;
        let walk = |fault: fn(&crate::tree::memory::Tree)| {
            let root = empty_files("unread", &["crates/a/src/lib.rs", "crates/a/src/m/x.rs"]);
            fault(&root);
            let mut files = Vec::new();
            collect_rs_files(&root.join("crates"), &mut files)
                .map(|()| files.len())
                .map_err(|e| e.kind())
        };
        assert_eq!(walk(|_| {}), Ok(2), "read whole");
        assert_eq!(
            walk(|root| root.unreadable("crates/a/src/m/x.rs")),
            Err(ErrorKind::Other)
        );
        assert_eq!(
            walk(|root| root.unlistable("crates/a/src/m")),
            Err(ErrorKind::PermissionDenied)
        );
    }

    #[test]
    fn run_censuses_keeps_the_given_order_and_turns_a_panic_red() {
        fn clean(_: &Walk<'_>) -> CensusOutcome {
            CensusOutcome::green("a", String::new(), "clean")
        }
        fn broken(_: &Walk<'_>) -> CensusOutcome {
            panic!("a walker bug")
        }
        let a = Census {
            name: "a",
            about: "",
            run: clean,
        };
        let b = Census {
            name: "b",
            about: "",
            run: broken,
        };
        let (out, report) = run_censuses(Path::new("."), &[&b, &a], &Err(String::new()), None);
        assert!(report.is_none(), "no memo, no report");
        assert!(
            !out[0].ok && out[0].log.contains("b census"),
            "{}",
            out[0].log
        );
        assert!(out[1].ok, "{}", out[1].log);
    }

    #[test]
    fn run_censuses_turns_a_missed_floor_red() {
        fn thin(_: &Walk<'_>) -> CensusOutcome {
            CensusOutcome::green("thin", "=== thin ===\n".to_string(), "1 site").with_floors(vec![
                Floor::at_least("sites read", 1, 100, "the walk went blind"),
                Floor::at_least("files read", 3, 1, "no files"),
            ])
        }
        fn red(_: &Walk<'_>) -> CensusOutcome {
            CensusOutcome::red("red", String::new(), "1 violation").with_floors(vec![
                Floor::at_least("sites read", 0, 100, "the walk went blind"),
            ])
        }
        let (a, b) = (
            Census {
                name: "thin",
                about: "",
                run: thin,
            },
            Census {
                name: "red",
                about: "",
                run: red,
            },
        );
        let walk = Walk {
            root: Path::new("."),
            engine: &Err(String::new()),
            source: &OnceLock::from(std::sync::Arc::new(memo::Snapshot::empty())),
            run: None,
        };
        assert!(thin(&walk).ok, "the bare walk holds no floors");
        let (out, _) = run_censuses(Path::new("."), &[&a, &b], &Err(String::new()), None);
        assert!(!out[0].ok, "{}", out[0].log);
        assert!(
            out[0].log.contains("[floor] sites read") && !out[0].log.contains("files read"),
            "only the missed floor is named:\n{}",
            out[0].log
        );
        assert!(
            out[1].verdict.ends_with("1 violation"),
            "{}",
            out[1].verdict
        );
    }

    fn body(src: &str) -> Vec<String> {
        src.lines().map(|l| l.to_string()).collect()
    }

    #[test]
    fn parse_fn_def_recognises_qualified_definitions_not_fn_bounds() {
        assert_eq!(
            parse_fn_def("    pub(crate) fn resize_panes_scoped(&mut self) {"),
            Some((4, "resize_panes_scoped".to_string()))
        );
        assert_eq!(
            parse_fn_def("fn cross_resize(term: &Arc) -> String {"),
            Some((0, "cross_resize".to_string()))
        );
        assert!(parse_fn_def("    where F: Fn(u16) -> bool,").is_none());
        assert!(parse_fn_def("        let f = default_fn(x);").is_none());
    }

    #[test]
    fn a_bodyless_declaration_does_not_swallow_the_fns_after_it() {
        let src = "\
extern \"system\" {
    fn GetDC(hwnd: isize) -> isize;
    fn ReleaseDC(
        hwnd: isize,
        dc: isize,
    ) -> i32;
}

pub trait Present {
    fn present(&mut self, rects: &[u32; 4]) -> bool;
}

impl Present for Gdi {
    fn present(&mut self, rects: &[u32; 4]) -> bool {
        let dc = unsafe { GetDC(self.hwnd) };
        blit(dc, rects)
    }
}

fn blit(dc: isize, rects: &[u32; 4]) -> bool {
    term_lock(&TERM).resize(rects[0], rects[1]);
    dc != 0
}

fn table() -> [f32; 256] {
    [0.0; 256]
}
";
        let mut fns = Vec::new();
        parse_source_fns(src, "x.rs", &mut fns);
        let spans: Vec<(&str, &str)> = fns
            .iter()
            .map(|f| (f.name.as_str(), f.span.as_str()))
            .collect();
        assert_eq!(
            spans,
            [
                ("present", "x.rs:14"),
                ("blit", "x.rs:20"),
                ("table", "x.rs:25")
            ],
            "each defined fn is its own item; no declaration is one"
        );
        assert_eq!(fns[0].body.len(), 4);
        assert!(fns[1].body.iter().any(|l| l.contains("term_lock(")));
        assert_eq!(fns[2].body.len(), 3);
    }

    #[test]
    fn term_hop_flags_synchronous_guarded_resize_both_idioms() {
        let (hz, _) = term_hop_calls(&body("    let p = term_lock(term).resize(rows, cols);"));
        assert_eq!(hz.len(), 1, "chained term_lock(..).resize( must flag");
        assert_eq!(hz[0].method, "resize");
        let (hz, _) = term_hop_calls(&body(
            "    let mut term = term_lock(&s.term);\n    term.resize(sub_rows, sub_cols);",
        ));
        assert_eq!(hz.len(), 1, "guard.resize( must flag");
    }

    #[test]
    fn term_hop_passes_the_offloaded_shape_and_bounded_neighbours() {
        let (hz, off) = term_hop_calls(&body(
            "    let mut term = term_lock(&s.term);\n    \
             term.resize_offloading_scrollback(sub_rows, sub_cols)",
        ));
        assert!(hz.is_empty(), "resize_offloading_scrollback must not flag");
        assert_eq!(off, 1, "offload boundary should be counted");
        let (hz, off) = term_hop_calls(&body("    term_lock(&s.term).set_cell_pixel_size(w, h);"));
        assert!(hz.is_empty());
        assert_eq!(off, 0);
    }

    #[test]
    fn term_hop_ignores_vec_and_local_terminal_resize() {
        let (hz, _) = term_hop_calls(&body("    self.born_scratch.resize(rows * cols, None);"));
        assert!(hz.is_empty());
        let (hz, _) = term_hop_calls(&body(
            "    let mut term = Terminal::from_checkpoint(cp, host);\n    \
             term.resize(rows, cols);",
        ));
        assert!(hz.is_empty(), "local Terminal::resize must not flag");
        assert!(
            guard_vars(&body(
                "    let mut term = Terminal::from_checkpoint(cp, host);"
            ))
            .is_empty()
        );
    }

    #[test]
    fn guard_vars_captures_the_term_lock_whole_binding() {
        let g = guard_vars(&body("        let t = crate::term_lock_whole(term);"));
        assert!(
            g.contains("t"),
            "crate::term_lock_whole( binding yields `t`"
        );
        let g = guard_vars(&body("        let mut t = term_lock_whole(term);"));
        assert!(g.contains("t"), "bare term_lock_whole( binding yields `t`");
        let (hz, _) = term_hop_calls(&body(
            "    let p = crate::term_lock_whole(term).resize(rows, cols);",
        ));
        assert_eq!(hz.len(), 1, "chained term_lock_whole(..).resize( must flag");
        let g = guard_vars(&body(
            "        let t = crate::term_lock_whole_when(term, |_| on == ReadOn::Control);",
        ));
        assert!(
            g.contains("t"),
            "crate::term_lock_whole_when( binding yields `t`"
        );
        let g = guard_vars(&body(
            "        let mut t = term_lock_whole_when(term, waits);",
        ));
        assert!(
            g.contains("t"),
            "bare term_lock_whole_when( binding yields `t`"
        );
        let (hz, _) = term_hop_calls(&body(
            "    let p = crate::term_lock_whole_when(term, |_| true).resize(rows, cols);",
        ));
        assert_eq!(
            hz.len(),
            1,
            "chained term_lock_whole_when(..).resize( must flag"
        );
    }

    #[test]
    fn any_match_tries_exactly_the_starts_match_indices_yields() {
        for (hay, needle) in [
            ("aaaa", "aa"),
            ("tttterm term", "term"),
            ("ééx é", "éx"),
            ("ReadOn::Control, ReadOn::Committed)", "ReadOn::C"),
            ("ab", ""),
            ("", "x"),
        ] {
            let mut tried = Vec::new();
            assert!(!any_match(hay, needle, |at| {
                tried.push(at);
                false
            }));
            let starts: Vec<usize> = hay.match_indices(needle).map(|(at, _)| at).collect();
            assert_eq!(tried, starts, "{needle:?} in {hay:?}");
            assert_eq!(
                mentions(hay, needle),
                hay.contains(needle),
                "{needle:?} in {hay:?}"
            );
        }
        let mut tried = Vec::new();
        assert!(any_match("f(a(b(", "(", |at| {
            tried.push(at);
            at == 3
        }));
        assert_eq!(tried, [1, 3], "the scan stops at the first start that fits");
    }

    #[test]
    fn guard_vars_captures_the_term_lock_ui_binding() {
        let g = guard_vars(&body(
            "            let mut term = crate::term_lock_ui(&front_terminal.term, ui_waiting, site);",
        ));
        assert!(
            g.contains("term"),
            "crate::term_lock_ui( binding yields `term`"
        );
        let g = guard_vars(&body(
            "        let mut t = term_lock_ui(&term, &ui_waiting, site);",
        ));
        assert!(g.contains("t"), "bare term_lock_ui( binding yields `t`");
        let (hz, _) = term_hop_calls(&body(
            "    let mut term = crate::term_lock_ui(&s.term, w, site);\n    term.resize(r, c);",
        ));
        assert_eq!(
            hz.len(),
            1,
            "resize on a term_lock_ui-bound guard must flag"
        );
        assert_eq!(hz[0].method, "resize");
        let (hz, _) = term_hop_calls(&body(
            "    crate::term_lock_ui(&s.term, w, site).resize(r, c);",
        ));
        assert_eq!(hz.len(), 1, "chained term_lock_ui(..).resize( must flag");
    }

    fn synth_support_files() -> Vec<(String, String)> {
        let m = "// COST: UNBOUNDED";
        vec![
            (
                "crates/aterm-grid/src/grid/reflow.rs".to_string(),
                format!("{m}(scrollback-width-reflow)\npub fn resize_with_reflow_mode() {{}}\n"),
            ),
            (
                "crates/aterm-grid/src/grid/scrollback_reflow.rs".to_string(),
                format!(
                    "{m}(ring+tiered-history-lines)\npub fn take_scrollback_lines() {{}}\n\
                     {m}(session-history-cells)\npub fn reflow_scrollback_lines() {{}}\n"
                ),
            ),
            (
                "crates/aterm-core/src/terminal/callback_setters.rs".to_string(),
                "pub fn resize_offloading_scrollback() {}\n\
                 pub fn finish_resize_offload() {}\n\
                 pub fn abort_resize_offload() {}\n"
                    .to_string(),
            ),
            (
                "crates/aterm-grid/src/grid/scrollback_offload.rs".to_string(),
                "pub fn reattach_reflowed_scrollback() {}\n".to_string(),
            ),
            (
                "crates/aterm-control/src/lib.rs".to_string(),
                "// stub\n".to_string(),
            ),
            (
                "crates/aterm-diskwalk/src/walk.rs".to_string(),
                format!(
                    "{m}(entries)\npub fn diskwalk_slice() {{}}\n\
                     {m}(entries)\npub fn diskwalk_size_full() {{}}\n"
                ),
            ),
            (
                "crates/aterm-gui/src/control_input.rs".to_string(),
                "fn input_if_fenced_observed() {\n    \
                 let terminal = term_lock_whole_when(term, Lane::SERVED, |_| waits);\n}\n"
                    .to_string(),
            ),
            (
                "crates/aterm-gui/src/control_query.rs".to_string(),
                "fn search_terminal() {\n    \
                 let t = term_lock_whole_when(term, Lane::SERVED, |_| on == ReadOn::Control);\n}\n"
                    .to_string(),
            ),
            (
                "crates/aterm-gui/src/control_lanes.rs".to_string(),
                "fn serve_here() {\n    cmd_text(term);\n}\n\
                 fn cmd_text() {\n    let t = crate::term_lock_whole(term);\n}\n"
                    .to_string(),
            ),
            (
                "crates/aterm-gui/src/task_launch.rs".to_string(),
                "fn run_worker_launch() {}\n".to_string(),
            ),
        ]
    }

    fn synth_manifests() -> Vec<(String, String)> {
        let mut files = crate::scan_set::test_fixtures::workspace_manifests(&[
            ("aterm-core", ""),
            ("aterm-grid", ""),
            ("aterm-diskwalk", ""),
        ]);
        for stub in [
            "crates/aterm-types/src/lib.rs",
            "crates/aterm-core/src/lib.rs",
            "crates/aterm-grid/src/lib.rs",
            "crates/aterm-diskwalk/src/lib.rs",
        ] {
            files.push((stub.to_string(), "// stub\n".to_string()));
        }
        files
    }

    fn synth_base() -> Vec<(String, String)> {
        let mut files = synth_manifests();
        files.extend(synth_support_files());
        files
    }

    fn synth_gui(extra: &str) -> String {
        format!(
            "fn new_events() {{}}\n\
             fn resumed() {{}}\n\
             fn user_event() {{\n    window_event();\n}}\n\
             fn window_event() {{\n    resize_helper();\n}}\n\
             fn about_to_wait() {{}}\n\
             fn flush_pending_resize() {{}}\n\
             {extra}"
        )
    }

    #[test]
    fn synthetic_offloaded_tree_is_green() {
        let mut files = synth_base();
        let gui = synth_gui(
            "fn resize_helper() {\n    \
             let pending = term_lock(&s.term).resize_offloading_scrollback(rows, cols);\n}\n",
        );
        files.push(("crates/aterm-gui/src/main.rs".to_string(), gui));
        let root = synth_tree("aterm-census", "green", &files);
        let out = run_mainloop_census(&root);
        assert!(out.ok, "expected GREEN, got:\n{}", out.log);
        assert!(
            out.log.contains("main-loop census: GREEN"),
            "log:\n{}",
            out.log
        );
        assert!(
            out.log
                .contains("the DERIVED GUI-process closure (5 workspace crates"),
            "the transcript must state the derived marker-sweep scope; log:\n{}",
            out.log
        );
        assert!(
            out.log.contains("0 out-of-closure marker(s) reported"),
            "log:\n{}",
            out.log
        );
    }

    #[test]
    fn synthetic_prefix_resize_shape_is_red_with_path() {
        let mut files = synth_base();
        let gui = synth_gui(
            "fn resize_helper() {\n    \
             let mut term = term_lock(&s.term);\n    \
             term.resize(sub_rows, sub_cols);\n}\n\
             fn unrelated_offload() {\n    \
             term_lock(&s.term).finish_resize_offload(reflowed);\n}\n",
        );
        files.push(("crates/aterm-gui/src/main.rs".to_string(), gui));
        let root = synth_tree("aterm-census", "red-ob5", &files);
        let out = run_mainloop_census(&root);
        assert!(!out.ok, "expected RED, got:\n{}", out.log);
        assert!(out.log.contains("[OB-5]"), "log:\n{}", out.log);
        assert!(
            out.log.contains("window_event -> resize_helper"),
            "the diagnostic must print the root -> hazard path; log:\n{}",
            out.log
        );
        assert!(
            out.log.contains("term.resize(sub_rows, sub_cols);"),
            "the diagnostic must quote the offending line; log:\n{}",
            out.log
        );
        assert!(
            out.log.contains("HOW TO REPAIR"),
            "the diagnostic must carry the repair options; log:\n{}",
            out.log
        );
        assert!(
            out.log.contains("main-loop census: FAILED — "),
            "the verdict line must name the census; log:\n{}",
            out.log
        );
    }

    #[test]
    fn a_whole_frame_read_the_event_loop_reaches_is_red_ob23() {
        let tree = |name: &str, read: &str| {
            let mut files = synth_base();
            let gui = synth_gui(&format!(
                "fn resize_helper() {{\n    \
                 let pending = term_lock(&s.term).resize_offloading_scrollback(r, c);\n    \
                 claude_rows_now();\n    \
                 input_if_fenced_observed();\n    \
                 search_terminal();\n    \
                 serve_here();\n}}\n\
                 fn claude_rows_now() {{\n    let t = {read}(&entry.term);\n}}\n"
            ));
            files.push(("crates/aterm-gui/src/main.rs".to_string(), gui));
            run_mainloop_census(&synth_tree("aterm-census", name, &files))
        };
        let out = tree("red-ob23", "crate::term_lock_whole");
        assert!(!out.ok, "expected RED, got:\n{}", out.log);
        assert!(
            out.log
                .contains("PATH:  window_event -> resize_helper -> claude_rows_now"),
            "log:\n{}",
            out.log
        );
        assert!(
            out.verdict
                .contains("(0 unbounded-work site(s), 1 frame wait(s))"),
            "{}",
            out.log
        );
        let out = tree("green-ob23", "crate::term_lock");
        assert!(out.ok, "expected GREEN, got:\n{}", out.log);
    }

    #[test]
    fn a_waiting_read_on_the_event_loop_hands_a_gate_is_red_ob23() {
        let tree = |name: &str, on: &str| {
            let mut files = synth_base();
            let gui = synth_gui(&format!(
                "fn resize_helper() {{\n    \
                 let pending = term_lock(&s.term).resize_offloading_scrollback(r, c);\n    \
                 claude_rows_now();\n}}\n\
                 fn claude_rows_now() {{\n    \
                 input_if_fenced_observed(term, ctx, ev, &fence, crate::{on}, None);\n}}\n"
            ));
            files.push(("crates/aterm-gui/src/main.rs".to_string(), gui));
            run_mainloop_census(&synth_tree("aterm-census", name, &files))
        };
        for (name, on) in [
            ("red-ob23-control", "ReadOn::Control"),
            ("red-ob23-committed", "ReadOn::Committed"),
        ] {
            let out = tree(name, on);
            assert!(!out.ok, "expected RED for {on}, got:\n{}", out.log);
            assert!(
                out.log
                    .contains("PATH:  window_event -> resize_helper -> claude_rows_now"),
                "log:\n{}",
                out.log
            );
        }
        let out = tree("green-ob23-loop", "ReadOn::EventLoop");
        assert!(out.ok, "expected GREEN, got:\n{}", out.log);
        let gui = synth_gui(
            "fn resize_helper() {\n    \
             let pending = term_lock(&s.term).resize_offloading_scrollback(r, c);\n    \
             lane(on);\n}\n\
             fn lane(on: ReadOn) -> bool {\n    \
             let waits = on == ReadOn::Control;\n    \
             match on {\n        \
             ReadOn::Committed => waits,\n        \
             ReadOn::Control | ReadOn::EventLoop => !waits,\n    }\n}\n",
        );
        let mut files = synth_base();
        files.push(("crates/aterm-gui/src/main.rs".to_string(), gui));
        let out = run_mainloop_census(&synth_tree("aterm-census", "green-ob23-arms", &files));
        assert!(
            out.ok,
            "a pattern or a comparison hands no read: {}",
            out.log
        );
    }

    #[test]
    fn a_gate_that_no_longer_waits_is_red_ob23() {
        let files: Vec<(String, String)> = synth_base()
            .into_iter()
            .map(|(rel, text)| {
                if rel.ends_with("control_query.rs") {
                    (
                        rel,
                        "fn search_terminal() {\n    let t = term_lock(term);\n}\n".into(),
                    )
                } else {
                    (rel, text)
                }
            })
            .chain([(
                "crates/aterm-gui/src/main.rs".to_string(),
                synth_gui(
                    "fn resize_helper() {\n    \
                     let pending = term_lock(&s.term).resize_offloading_scrollback(r, c);\n}\n",
                ),
            )])
            .collect();
        let out = run_mainloop_census(&synth_tree("aterm-census", "red-ob23-gate", &files));
        assert!(!out.ok, "expected RED, got:\n{}", out.log);
        assert!(
            out.log
                .contains("whole-frame gate `search_terminal` no longer takes a whole-frame read"),
            "log:\n{}",
            out.log
        );
    }
    #[test]
    fn cfg_predicate_entails_unshipped_only_when_it_really_does() {
        for yes in [
            "#[cfg(test)]",
            "#[cfg(all(test, unix))]",
            "#[cfg(all(test, any(windows, target_os = \"linux\")))]",
            "#[cfg(all(unix, all(test, debug_assertions)))]",
        ] {
            assert!(cfg_attr_never_ships(yes), "`{yes}` must be masked");
        }
        for no in [
            "#[cfg(any(test, unix))]",
            "#[cfg(not(test))]",
            "#[cfg(all(not(test), unix))]",
            "#[cfg(feature = \"test\")]",
            "#[cfg(target_os = \"macos\")]",
            "#[cfg(all())]",
            "#[test]",
            "#[derive(Debug)]",
            "let x = 1;",
        ] {
            assert!(!cfg_attr_never_ships(no), "`{no}` must NOT be masked");
        }
    }

    #[test]
    fn top_level_comma_split_ignores_nesting_and_string_literals() {
        assert_eq!(
            split_top_level_commas("test, any(windows, target_os = \"linux\")"),
            vec!["test", "any(windows, target_os = \"linux\")"]
        );
        assert_eq!(
            split_top_level_commas("target_os = \"a,b\", unix"),
            vec!["target_os = \"a,b\"", "unix"]
        );
        assert!(split_top_level_commas("  ").is_empty());
    }

    #[test]
    fn conjunctive_cfg_test_helper_is_masked_not_graphed() {
        let mut files = synth_base();
        let gui = synth_gui(
            "fn resize_helper() {\n    \
             compose();\n}\n\
             fn compose() {}\n\
             fn unrelated_offload() {\n    \
             term_lock(&s.term).finish_resize_offload(reflowed);\n}\n\
             #[cfg(all(test, any(windows, target_os = \"linux\")))]\n\
             mod strip_band_landing_tests {\n    \
             fn compose() {\n        \
             let mut term = term_lock(&s.term);\n        \
             term.resize(rows, cols);\n    }\n}\n",
        );
        files.push(("crates/aterm-gui/src/main.rs".to_string(), gui));
        let root = synth_tree("aterm-census", "mask-cfg-all-test", &files);
        let out = run_mainloop_census(&root);
        assert!(
            out.ok,
            "a `cfg(all(test, ..))` helper never enters the shipped GUI \
             process and must not fail OB-5:\n{}",
            out.log
        );
    }

    #[test]
    fn a_disjunctive_cfg_test_gate_is_not_masked_and_stays_red() {
        let mut files = synth_base();
        let gui = synth_gui(
            "fn resize_helper() {\n    \
             compose();\n}\n\
             fn unrelated_offload() {\n    \
             term_lock(&s.term).finish_resize_offload(reflowed);\n}\n\
             #[cfg(any(test, unix))]\n\
             mod maybe_shipped {\n    \
             fn compose() {\n        \
             let mut term = term_lock(&s.term);\n        \
             term.resize(rows, cols);\n    }\n}\n",
        );
        files.push(("crates/aterm-gui/src/main.rs".to_string(), gui));
        let root = synth_tree("aterm-census", "no-mask-any-test", &files);
        let out = run_mainloop_census(&root);
        assert!(
            !out.ok,
            "`any(test, unix)` compiles on a shipped unix build — the hazard \
             under it must still fail OB-5:\n{}",
            out.log
        );
        assert!(out.log.contains("[OB-5]"), "log:\n{}", out.log);
    }

    #[test]
    fn synthetic_direct_sink_call_is_red_ob6() {
        let mut files = synth_base();
        let gui = synth_gui(
            "fn resize_helper() {\n    \
             let lines = reflow_scrollback_lines(&lines, cols);\n}\n\
             fn unrelated_offload() {\n    \
             term_lock(&s.term).finish_resize_offload(reflowed);\n}\n",
        );
        files.push(("crates/aterm-gui/src/main.rs".to_string(), gui));
        let root = synth_tree("aterm-census", "red-ob6", &files);
        let out = run_mainloop_census(&root);
        assert!(!out.ok, "expected RED, got:\n{}", out.log);
        assert!(out.log.contains("[OB-6]"), "log:\n{}", out.log);
        assert!(
            out.log.contains("reflow_scrollback_lines"),
            "log:\n{}",
            out.log
        );
    }

    #[test]
    fn synthetic_missing_boundary_def_is_red_ob3_and_missing_offload_is_red_ob4() {
        let files: Vec<(String, String)> = synth_base()
            .into_iter()
            .filter(|(rel, _)| !rel.ends_with("callback_setters.rs"))
            .chain([(
                "crates/aterm-gui/src/main.rs".to_string(),
                synth_gui("fn resize_helper() {}\n"),
            )])
            .collect();
        let root = synth_tree("aterm-census", "red-ob3-ob4", &files);
        let out = run_mainloop_census(&root);
        assert!(!out.ok, "expected RED, got:\n{}", out.log);
        assert!(out.log.contains("[OB-3]"), "log:\n{}", out.log);
        assert!(out.log.contains("[OB-4]"), "log:\n{}", out.log);
    }

    #[test]
    fn synthetic_missing_root_is_red_ob2() {
        let mut files = synth_base();
        files.push((
            "crates/aterm-gui/src/main.rs".to_string(),
            "fn new_events() {}\n\
             fn resumed() {}\n\
             fn user_event() {}\n\
             fn window_event() {}\n\
             fn about_to_wait() {}\n\
             fn some_offload() {\n    \
             term_lock(&s.term).resize_offloading_scrollback(r, c);\n}\n"
                .to_string(),
        ));
        let root = synth_tree("aterm-census", "red-ob2", &files);
        let out = run_mainloop_census(&root);
        assert!(!out.ok, "expected RED, got:\n{}", out.log);
        assert!(out.log.contains("[OB-2]"), "log:\n{}", out.log);
        assert!(
            out.log.contains("flush_pending_resize"),
            "log:\n{}",
            out.log
        );
    }

    #[test]
    fn out_of_closure_marker_is_not_swept_and_is_reported() {
        let m = "// COST: UNBOUNDED";
        let mut files = synth_base();
        files.push((
            "crates/aterm-gui/src/main.rs".to_string(),
            synth_gui(
                "fn resize_helper() {\n    \
                 term_lock(&s.term).resize_offloading_scrollback(rows, cols);\n}\n",
            ),
        ));
        files.push((
            "crates/aterm-clitool/src/main.rs".to_string(),
            format!("{m}(whole-archive-dump)\npub fn dump_everything() {{}}\n"),
        ));
        let root = synth_tree("aterm-census", "out-of-closure", &files);
        let out = run_mainloop_census(&root);
        assert!(
            out.ok,
            "an out-of-closure marker must NOT enter OB-1 coherence; log:\n{}",
            out.log
        );
        assert!(
            out.log.contains("NOTE [OB-1 scope]")
                && out.log.contains("dump_everything")
                && out.log.contains("crates/aterm-clitool/src/main.rs:1"),
            "the out-of-closure marker must be REPORTED with its span; log:\n{}",
            out.log
        );
        assert!(
            out.log.contains("1 out-of-closure marker(s) reported"),
            "log:\n{}",
            out.log
        );
    }

    #[test]
    fn out_of_closure_marker_cannot_satisfy_the_registry() {
        let m = "// COST: UNBOUNDED";
        let mut files: Vec<(String, String)> = synth_base()
            .into_iter()
            .map(|(rel, contents)| {
                if rel.ends_with("grid/reflow.rs") {
                    (rel, "pub fn resize_with_reflow_mode() {}\n".to_string())
                } else {
                    (rel, contents)
                }
            })
            .collect();
        files.push((
            "crates/aterm-gui/src/main.rs".to_string(),
            synth_gui(
                "fn resize_helper() {\n    \
                 term_lock(&s.term).resize_offloading_scrollback(rows, cols);\n}\n",
            ),
        ));
        files.push((
            "crates/aterm-clitool/src/lib.rs".to_string(),
            format!("{m}(scrollback-width-reflow)\npub fn resize_with_reflow_mode() {{}}\n"),
        ));
        let root = synth_tree("aterm-census", "moved-marker", &files);
        let out = run_mainloop_census(&root);
        assert!(!out.ok, "expected RED, got:\n{}", out.log);
        assert!(
            out.log.contains("[OB-1]")
                && out.log.contains("has LOST its")
                && out.log.contains("resize_with_reflow_mode"),
            "the registry must not be satisfiable from outside the closure; log:\n{}",
            out.log
        );
        assert!(
            out.log.contains("NOTE [OB-1 scope]"),
            "the stray out-of-closure marker must still be reported; log:\n{}",
            out.log
        );
    }

    #[test]
    fn underivable_workspace_fails_the_mainloop_census_closed() {
        let files: Vec<(String, String)> = synth_base()
            .into_iter()
            .filter(|(rel, _)| rel != "Cargo.toml")
            .chain([(
                "crates/aterm-gui/src/main.rs".to_string(),
                synth_gui("fn resize_helper() {}\n"),
            )])
            .collect();
        let root = synth_tree("aterm-census", "underivable", &files);
        let out = run_mainloop_census(&root);
        assert!(!out.ok, "expected RED, got:\n{}", out.log);
        assert!(
            out.log.contains("SCAN-SET DERIVATION FAILED"),
            "log:\n{}",
            out.log
        );
    }
}
