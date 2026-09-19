// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `aterm-verify` — aterm's merge gate, as a program instead of a script.
//!
//! aterm has NO CI by owner decision (docs/AUDIT.md, docs/PROCESS.md): the merge
//! contract is *the gate passing locally* before a slice enters the ff-only main
//! merge-queue. That gate lived in `tools/verify.sh`, whose header claimed it was
//! "intentionally dependency-free POSIX-ish bash" so it could "run on a clean
//! checkout with nothing installed but the pinned stable toolchain". The rationale
//! expired: every meaningful stage shells out to cargo, and the pin is a
//! multi-hour Trust build — on a bare machine the script does not verify anything,
//! it prints skips. So the ladder became this crate.
//!
//! WHAT STAYS IN BASH. Exactly one job: resolving `$TRUST_STAGE2_BIN` to a
//! physical path, putting it first on PATH, and printing the flag-spelling-skew
//! diagnostic (the trust 576db732cd rename partitions stage2s between
//! `-Zno-trust-verify=yes` and `-Ztrust-verify=off`, and no spelling satisfies
//! both). A Rust driver cannot diagnose a rustc that refuses to compile the
//! driver. That shim is not this crate's to write; this crate is what it drives.
//!
//! THE SHIM CONTRACT, for whoever writes it:
//!  * build this crate with the stage2 it just put on PATH, then `exec` the
//!    binary with the caller's arguments UNCHANGED and `--root <repo root>`
//!    appended (a compiled binary cannot derive the root from its own path the
//!    way a script can; `ATERM_VERIFY_ROOT` does the same job);
//!  * if that build fails, the shim itself must print the flag-spelling-skew
//!    diagnostic and exit `3` — a gate that cannot be compiled has decided
//!    NOTHING, and `3` is the code that says so;
//!  * pass the exit code through untouched: `0` pass, `1` a gate FAILED, `2`
//!    usage, `3` COULD NOT RUN.
//!  * everything else — the stage ladder, the skip accounting, the verdict — is
//!    here, and re-implementing any of it in the shim would recreate the split
//!    brain this port exists to remove.
//!
//! THE POINT, beyond taste: the gate's own logic is now TESTABLE. The scoping,
//! the skip accounting and — above all — the verdict claim are ordinary functions
//! with ordinary unit tests, and those tests run inside the merge contract they
//! describe (this crate is a workspace member, so `targo test --workspace` is
//! also the gate testing itself).
//!
//! WHAT WAS PRESERVED EXACTLY
//!  * The `ok`/`FAIL`/`skip` ladder vocabulary and its column layout, and the
//!    rule that a skip is an honest "tool absent" — never a silent pass.
//!  * The VERDICT DISCIPLINE. The script once printed "merge contract satisfied"
//!    on any `rc == 0`, so a `--scope` run over one of sixty-odd crates claimed
//!    the whole contract, as did any run with a skipped stage. It counts and NAMES
//!    skips and refuses the merge-contract sentence for any narrowed run. That is
//!    the single most important property in the gate and [`verdict`] is where it
//!    lives — one function, one sentence constant, exhaustively tested.
//!  * The flag spellings `--fast` / `--full` / `--scope <crate>` / `--selftest`,
//!    so docs/PROCESS.md and every agent instruction keep working unedited.
//!  * The change-scoped tier `--changed [--base <ref>]` ([`changed`]), including
//!    the part that makes it safe rather than merely fast: THE DIRECTION OF
//!    FAILURE IS FIXED. Anything the selection cannot answer honestly — absent
//!    targo, no merge-base, an unreadable graph, a manifest-level change that
//!    re-plans everything — WIDENS the run to the whole workspace and says so,
//!    because a narrower that guesses low is a false green and one that guesses
//!    high is only slow. And it is a NARROWING, so it forfeits the merge-contract
//!    sentence exactly as `--scope` does.
//!  * Fail-closed everywhere: no missing tool may produce a pass.
//!  * Every stage that shells out shells out to the SAME command with the same
//!    arguments and environment — tippy keeps its separate `CARGO_TARGET_DIR` and
//!    `TRUST_NO_MIGRATE_WARN`, the doc-running stages bind
//!    `RUSTDOC=<stage2>/trustdoc` whenever the stage2 carries an executable one
//!    (the full rule is `stages::doc_driver`), and every driver invocation
//!    still names its lane with `--unverified`.
//!
//! WHAT CHANGED ON PURPOSE
//!  * Independent stages run CONCURRENTLY ([`sched`]) while the OUTPUT stays in
//!    the ladder's declared order, so the run stays scannable. Concurrency is
//!    constrained by the resource each stage actually contends for (see
//!    [`plan::Lane`]) and the two timing-measuring smokes run exclusively, because
//!    a gate that decides "present starvation" while a lint compiles on the other
//!    seven cores would be measuring the gate, not the build.
//!  * Exit codes distinguish FAILED from COULD-NOT-RUN (`1` vs `3`) — the
//!    distinction the 633-line BLOCKING `.githooks/pre-push` reasoned about
//!    before it was demoted to advisory on 2026-08-24 (it had separate "✗ LINT
//!    GATE COULD NOT RUN" and "✗ L0 TEMPORAL-SAFETY GATE COULD NOT RUN" arms).
//!    The hook is gone as a caller; the distinction is not, because it was never
//!    the hook's to own. `tools/verify.sh` documents all four codes at its
//!    hand-off and passes ours through untouched, and it reaches for `3` itself
//!    on every path where the gate was never built. The ladder line is `FAIL`
//!    either way — a broken environment still never reads as green.
//!  * `--scope=` with an empty value is a usage error instead of silently meaning
//!    "the whole workspace". In bash that spelling widened the claim to the merge
//!    contract; that is the exact class of bug the verdict discipline exists to
//!    stop. `--base=` is a usage error for the same reason: in bash it set an
//!    empty ref, which then failed to find a merge-base and WIDENED the run
//!    without the caller ever learning the flag was malformed.
//!  * A run in a git checkout verifies a pinned SNAPSHOT of the caller's tree
//!    ([`snapshot`], 2026-09-13; `--in-place` opts out, and `--selftest` runs
//!    in place), and a compiler (or, in a git checkout, the source tree) that
//!    moves under a run stops every stage not yet started and adds a `source
//!    identity` COULD NOT RUN row ([`identity`]): a run with nothing failed ends
//!    COULD NOT RUN (exit 3), and a stage that already FAILED keeps FAIL (exit
//!    1). The 14 h `--fast` run that motivated both was pulled four times
//!    mid-ladder in a live, shared checkout and still printed one verdict.
//!    Neither changes any stage's argv.
//!  * The change-scoped SELECTION is a pure function of (diff paths, manifests,
//!    members, inverted graph), so its seeds, its reverse-dependency closure and
//!    every one of its widening triggers are unit-testable without a repo. In
//!    bash the same logic could only be exercised by running the gate inside a
//!    git checkout with a Trust stage2 installed, which is to say: never.

pub mod changed;
pub mod cli;
pub mod exec;
pub mod glob;
pub mod identity;
pub mod ladder;
pub mod libtest;
pub mod plan;
pub mod receipt;
pub mod sched;
pub mod scope;
pub mod smoke;
pub mod smoke_stages;
pub mod snapshot;
pub mod stages;
pub mod toolchain;
pub mod verdict;

use std::ffi::{OsStr, OsString};
use std::io::{IsTerminal, Write};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Mutex;
use std::time::{Duration, Instant};

pub use cli::Mode;
pub use ladder::{Entry, Outcome, Report, Severity, Tally};
pub use scope::Scope;
pub use toolchain::Toolchain;
pub use verdict::{MERGE_CONTRACT_SENTENCE, Verdict};

/// Exit codes. FAILED and COULD-NOT-RUN are kept apart because a broken
/// environment is not a finding about the tree, and conflating them is how a
/// gate stops being read.
///
/// The blocking pre-push hook reasoned about that distinction, and this comment
/// used to cite it as the reason. It is no longer a caller — it was advisory
/// from 2026-08-24, and since 2026-09-17 it reads this run's RECEIPT
/// ([`receipt`]) rather than running the gate — but the reason outlived it, and
/// the distinction now has a live consumer either way: `tools/verify.sh`
/// documents all four codes where it
/// `exec`s this binary, passes ours through untouched, and returns `3` itself
/// whenever the gate could not be built at all.
pub mod exit {
    /// Everything that ran was green (the verdict text says *which* green).
    pub const PASS: i32 = 0;
    /// A gate FAILED — a real finding about the tree.
    pub const FAILED: i32 = 1;
    /// Usage error.
    pub const USAGE: i32 = 2;
    /// COULD NOT RUN — the environment is broken and nothing was decided.
    /// Never mistakable for green, and never mistakable for a finding either.
    pub const COULD_NOT_RUN: i32 = 3;
}

/// The environment read ONCE, at startup, on the main thread.
///
/// Stages run concurrently and `std::env::var` is a global read against a global
/// that anything may mutate; snapshotting keeps every stage's decision a function
/// of a value that can be constructed in a test.
#[derive(Clone, Debug, Default)]
pub struct EnvSnapshot {
    pub path: OsString,
    pub home: PathBuf,
    pub cargo_target_dir: Option<OsString>,
    pub trust_stage2_bin: Option<PathBuf>,
    pub trust_mc_sysroot: Option<PathBuf>,
    pub ay_bin_dir: Option<PathBuf>,
    /// `XDG_CONFIG_HOME` — where atpkg's own config (`aterm/aterm.toml`, the
    /// `[packages].prefix` override) lives when set; the store probe reads the
    /// same file atpkg does ([`toolchain::atpkg_prefix`]).
    pub xdg_config_home: Option<PathBuf>,
    pub ssh_connection: Option<String>,
    pub skip_gui_smoke: Option<String>,
    /// `ATERM_VERIFY_BASE` — the default `--base` for `--changed`.
    pub verify_base: Option<String>,
    /// `ATERM_VERIFY_STAGE_TIMEOUT` — the wall-clock ceiling on one stage child,
    /// in seconds, or `0`/`off` to remove it (see [`exec::DEFAULT_CHILD_CEILING`]
    /// for why there is one at all). Kept RAW here and parsed in
    /// [`exec::ceiling_from_env`]: the snapshot's job is to read the environment
    /// exactly once, on the main thread, and the policy that interprets it is a
    /// pure function with its own tests.
    pub stage_timeout: Option<OsString>,
    /// `CARGO_BUILD_JOBS` — the caller's job count. The main lane and the lint
    /// lane inherit it as they inherit every variable; for the SIDE lanes, whose
    /// caps are constants ([`stages::lane_build_jobs`]), it is a CEILING
    /// ([`stages::lane_jobs`]): a 4-core machine that exports `4` must not have
    /// the driver lane override it back up to 8. Kept RAW, like
    /// [`Self::stage_timeout`], and parsed by the pure function.
    pub cargo_build_jobs: Option<OsString>,
    /// `RUSTDOC` or `CARGO_BUILD_RUSTDOC` — a caller-supplied doc-driver
    /// binding. Cargo prefers either over the config's `[build] rustdoc`, so
    /// the children inherit it and the doc-driver rule must account for it:
    /// an explicit export is the operator's own per-invocation override (the
    /// same escape hatch verify.sh's header sanctions for flag-spelling skew),
    /// never grounds for a COULD-NOT-RUN.
    pub rustdoc_override: Option<OsString>,
    /// `ATERM_VERIFY_SNAPSHOT` — where the snapshot lives, when not
    /// `<caller-root>-verify.noindex` ([`snapshot`]).
    pub verify_snapshot: Option<PathBuf>,
    /// `ATERM_VERIFY_TIMINGS` — the per-child timing TSV ([`exec::Timings`]).
    pub verify_timings: Option<PathBuf>,
    /// `ATERM_VERIFY_LOG` — where the gate writes its own copy of the ladder.
    /// SET-BUT-EMPTY IS MEANINGFUL and is kept, unlike every path above: it is
    /// how a caller turns the log off, which is not the same as not asking.
    pub verify_log: Option<PathBuf>,
}

impl EnvSnapshot {
    /// Read the process environment.
    #[must_use]
    pub fn capture() -> Self {
        let var_path = |k: &str| std::env::var_os(k).map(PathBuf::from);
        Self {
            path: std::env::var_os("PATH").unwrap_or_default(),
            home: var_path("HOME").unwrap_or_default(),
            cargo_target_dir: std::env::var_os("CARGO_TARGET_DIR"),
            trust_stage2_bin: var_path("TRUST_STAGE2_BIN"),
            trust_mc_sysroot: var_path("TRUST_MC_SYSROOT"),
            ay_bin_dir: var_path("AY_BIN_DIR"),
            xdg_config_home: var_path("XDG_CONFIG_HOME").filter(|p| !p.as_os_str().is_empty()),
            ssh_connection: std::env::var("SSH_CONNECTION").ok(),
            skip_gui_smoke: std::env::var("ATERM_SKIP_GUI_SMOKE").ok(),
            verify_base: std::env::var("ATERM_VERIFY_BASE")
                .ok()
                .filter(|s| !s.is_empty()),
            stage_timeout: std::env::var_os(exec::CEILING_ENV),
            cargo_build_jobs: std::env::var_os("CARGO_BUILD_JOBS"),
            // NO empty-filter, deliberately: cargo has no treat-empty-as-unset
            // rule for these (only RUSTC_WRAPPER gets one), so a set-but-empty
            // RUSTDOC still masks CARGO_BUILD_RUSTDOC and still reaches the
            // child — the snapshot mirrors that precedence exactly, and a
            // broken export fails under the "(caller's RUSTDOC)" label that
            // names whose binding it was.
            rustdoc_override: std::env::var_os("RUSTDOC")
                .or_else(|| std::env::var_os("CARGO_BUILD_RUSTDOC")),
            verify_snapshot: var_path(snapshot::SNAPSHOT_ENV).filter(|p| !p.as_os_str().is_empty()),
            verify_timings: var_path("ATERM_VERIFY_TIMINGS").filter(|p| !p.as_os_str().is_empty()),
            verify_log: var_path("ATERM_VERIFY_LOG"),
        }
    }
}

/// Everything a stage needs, immutable for the whole run so stages can share it
/// across threads without a lock.
#[derive(Debug)]
pub struct Ctx {
    pub root: PathBuf,
    pub mode: Mode,
    pub scope: Scope,
    pub selftest: bool,
    pub tools: Toolchain,
    /// PATH handed to every child: the resolved stage2 directory first, when a
    /// `targo` actually lives there, exactly as the script's `PATH=…` export did.
    /// Passed per-child rather than by mutating our own environment — with stages
    /// on threads, `set_var` is a data race, and an explicit value is testable.
    pub path_env: OsString,
    /// Private scratch directory for captured child output, removed at the end.
    pub scratch: PathBuf,
    pub env: EnvSnapshot,
    /// Stages already decided before the ladder was planned — today exactly one:
    /// `--changed`'s selection, which has to run BEFORE [`plan::plan`] because it
    /// is what produces the [`Scope`] the plan is built from. Printed first,
    /// tallied like any other stage, and subject to the same rule that a stage
    /// recording no outcome cannot be counted.
    pub prelude: Vec<Report>,
    /// Where the stages run: the caller's checkout, or a snapshot of it.
    pub source_mode: snapshot::SourceMode,
    /// `verify: …` header lines — the snapshot's (cold or pruned lanes), or why
    /// a root that is not a git checkout runs in place — printed under the
    /// source line.
    pub notes: Vec<String>,
    /// Variables removed from every child's inherited environment
    /// ([`exec::ExecEnv::remove_env`]).
    pub child_env_remove: Vec<&'static str>,
    /// The `ATERM_VERIFY_TIMINGS` sink, when one was opened.
    pub timings: Option<exec::Timings>,
    /// The source state a snapshot's sync verified. The tripwire arms on it
    /// (and compares it with a fresh capture) rather than re-arming from
    /// whatever the root holds by the time the ladder starts.
    pub source_baseline: Option<identity::TreeState>,
}

/// The gate's own side channels, removed from every child in every mode: a
/// child that re-invoked the gate would otherwise truncate this run's timings
/// TSV and aim at this run's own snapshot, which this run holds locked.
pub const GATE_CHANNELS: [&str; 2] = ["ATERM_VERIFY_TIMINGS", snapshot::SNAPSHOT_ENV];

impl Ctx {
    /// Build the run context. `scratch` must already exist.
    #[must_use]
    pub fn new(
        root: PathBuf,
        mode: Mode,
        scope: Scope,
        selftest: bool,
        env: EnvSnapshot,
        scratch: PathBuf,
    ) -> Self {
        // The atpkg prefix, resolved ONCE from the snapshot (the same file atpkg
        // reads) and handed to every store probe: the compiler's, and the
        // trust-mc / ay lanes' in `stages::kani_floor`.
        let prefix = toolchain::atpkg_prefix(&env.home, env.xdg_config_home.as_deref());
        let tools = Toolchain::discover_with_store(
            env.trust_stage2_bin.as_deref(),
            &env.home,
            Some(&prefix),
            &env.path,
            crate::toolchain::pinned_channel(&root).as_deref(),
        );
        let path_env = tools.path_with_stage2_first(&env.path);
        Self {
            root,
            mode,
            scope,
            selftest,
            tools,
            path_env,
            scratch,
            env,
            prelude: Vec::new(),
            source_mode: snapshot::SourceMode::InPlace,
            notes: Vec::new(),
            child_env_remove: GATE_CHANNELS.to_vec(),
            timings: None,
            source_baseline: None,
        }
    }

    /// This run's root is a snapshot of `caller`. The caller's
    /// `CARGO_TARGET_DIR` stops applying: the snapshot's lanes are its own
    /// directories, so the redirect is dropped from the snapshot AND removed
    /// from every child's environment — otherwise cargo would build in the
    /// caller's contended target dir while the stages looked for binaries in
    /// the snapshot's.
    ///
    /// `tree` is the state the sync verified; the run's tripwire arms on it.
    #[must_use]
    pub fn in_snapshot_of(
        mut self,
        caller: PathBuf,
        tree: identity::TreeState,
        notes: Vec<String>,
    ) -> Self {
        self.source_mode = snapshot::SourceMode::Snapshot { caller };
        self.source_baseline = Some(tree);
        self.env.cargo_target_dir = None;
        self.child_env_remove.push("CARGO_TARGET_DIR");
        self.notes.extend(notes);
        self
    }

    /// Extra `verify: …` header lines.
    #[must_use]
    pub fn with_notes(mut self, notes: impl IntoIterator<Item = String>) -> Self {
        self.notes.extend(notes);
        self
    }

    /// Write per-child timing rows to `timings`.
    #[must_use]
    pub fn with_timings(mut self, timings: Option<exec::Timings>) -> Self {
        self.timings = timings;
        self
    }

    /// Attach a stage decided before the plan existed (see [`Ctx::prelude`]).
    #[must_use]
    pub fn with_prelude(mut self, report: Option<Report>) -> Self {
        self.prelude.extend(report);
        self
    }

    /// The command environment: cwd is the repo root (the script `cd`s there and
    /// several stages pass root-relative paths), PATH is the computed one, and
    /// every child gets the wall-clock ceiling — a hung stage has to be able to
    /// end the run RED, because a gate that hangs has decided nothing and says
    /// nothing.
    #[must_use]
    pub fn exec_env(&self) -> exec::ExecEnv<'_> {
        exec::ExecEnv {
            cwd: &self.root,
            path: &self.path_env,
            scratch: &self.scratch,
            child_ceiling: exec::ceiling_from_env(self.env.stage_timeout.as_deref()),
            remove_env: &self.child_env_remove,
            timings: self.timings.as_ref(),
        }
    }

    /// `tools/` — where the guard scripts live.
    #[must_use]
    pub fn tools_dir(&self) -> PathBuf {
        self.root.join("tools")
    }
}

/// The one line that names the compiler every stage below runs.
///
/// Pure over the two facts that decide it, so the sentence is a test and not a
/// promise: the directory the toolchain walk settled on, and whether a `targo`
/// was actually found in it. A RELATIVE path is called out rather than printed
/// as if it were an answer — a gate that cannot say where its compiler is has
/// not pinned one, and the whole point of the line is that a reader never has
/// to take the pin on trust again.
#[must_use]
pub fn toolchain_header_line(
    stage2_dir: &std::path::Path,
    channel: Option<&str>,
    have_targo: bool,
) -> String {
    let channel = channel.unwrap_or("<unpinned>");
    if !have_targo {
        return format!(
            "verify: toolchain NONE — channel \"{channel}\" resolved to no usable targo \
             (looked last at {})\n",
            stage2_dir.display()
        );
    }
    if !stage2_dir.is_absolute() {
        return format!(
            "verify: toolchain {} — RELATIVE, so this run cannot say which compiler it \
             used (channel \"{channel}\")\n",
            stage2_dir.display()
        );
    }
    format!(
        "verify: toolchain {} (channel \"{channel}\", absolute and resolved once — every \
         stage below ran this targo)\n",
        stage2_dir.display()
    )
}

/// [`toolchain_header_line`] for a live run.
#[must_use]
pub fn toolchain_header(ctx: &Ctx) -> String {
    toolchain_header_line(
        &ctx.tools.stage2_dir,
        crate::toolchain::pinned_channel(&ctx.root).as_deref(),
        ctx.tools.have_targo(),
    )
}

/// Run the whole gate: hooks, ladder, verdict. Returns the process exit code.
///
/// `out` receives, in this order: the [`toolchain_header`] line, the prelude rungs,
/// the `hooks pinned:` note when `pin_hooks` had to set `core.hooksPath`, the
/// `verify: source …` line (a git root only) and any `verify:` notes (the
/// snapshot's lanes, or why the run is in place), the ladder in declared order
/// with a `  time  ` line under each stage — or, in its place, a `source
/// identity` COULD NOT RUN row for a git checkout the gate cannot read, except
/// under `--selftest`, which keeps its own ladder — the `source identity` row
/// when the toolchain (or, in a git checkout, the source tree) moved mid-run,
/// and the verdict
/// (or the gate-defect `FAIL` and `VERIFY: COULD NOT RUN` lines) — byte-for-byte
/// in the vocabulary `tools/verify.sh` established.
/// Live progress goes to stderr so a long stage is not silent without polluting the
/// scannable part.
///
/// # Errors
/// Propagates write failures on `out`.
pub fn run(ctx: &Ctx, out: &mut dyn Write) -> std::io::Result<i32> {
    // WHICH COMPILER DECIDED THIS, named before anything is decided.
    //
    // MEASURED 2026-09-10, and it cost an 80-minute gate: a run was killed on
    // the hypothesis that `verify.sh` resolved its toolchain through a mutable
    // rustup symlink and that a peer's re-seal had split the gate across two
    // compilers. It does not — the shim resolves a PHYSICAL path once (`cd
    // "$cand_dir" && pwd -P`) and prepends it for the whole run — but nothing
    // the gate printed said so, so two readers believed it and neither could
    // check in less than a code read. One line ends that question forever.
    out.write_all(toolchain_header(ctx).as_bytes())?;
    // The change-scope stage first: it is what CHOSE the scope every header
    // below prints, so a reader meets the narrowing before its consequences.
    for r in &ctx.prelude {
        out.write_all(r.render().as_bytes())?;
    }
    pin_hooks(ctx, out)?;

    // WHAT THIS RUN IS VERIFYING, captured before anything is planned and
    // re-checked while it runs (2026-09-13). The 14 h run this answers was
    // pulled four times mid-ladder and printed one verdict over all of them.
    let tripwire = identity::Tripwire::arm_against(
        &ctx.root,
        &ctx.path_env,
        ctx.source_baseline.clone(),
        ctx.tools.identity(&ctx.path_env, &ctx.scratch),
    );
    if let Some(line) = tripwire.header_line(&ctx.source_mode.place(&ctx.root)) {
        out.write_all(line.as_bytes())?;
    }
    for note in &ctx.notes {
        writeln!(out, "{note}")?;
    }

    // A GIT CHECKOUT THE GATE CANNOT READ is not a root without a source
    // (2026-09-13): with no identity there is no tripwire, and a run on it
    // could go green on a tree nothing watched. No stage runs. A selftest
    // builds nothing and claims nothing about the tree, so it keeps its own
    // ladder: turning it into SELFTEST FAIL over one unreadable file would be a
    // finding about the driver that is not true.
    if !ctx.selftest
        && let identity::SourceIdentity::Unreadable(why) = &tripwire.source
    {
        let mut r = Report::new("source identity");
        r.cannot_run(identity::unreadable_label(why));
        out.write_all(r.render().as_bytes())?;
        let mut reports = ctx.prelude.clone();
        reports.push(r);
        let verdict =
            verdict::verdict(ctx.mode, &ctx.scope, ctx.selftest, &ladder::tally(&reports));
        out.write_all(verdict.text.as_bytes())?;
        out.flush()?;
        return Ok(verdict.exit);
    }

    let plan = plan::plan(ctx);
    let mut reports: Vec<Report> = ctx.prelude.clone();
    reports.reserve(plan.len());
    let mut err = None;

    // Per-stage (start offset, running time), for the `  time  ` line under
    // each stage. STDOUT, not only a terminal's stderr (2026-09-13): 430 of a
    // 14 h run's minutes sat inside one stage, and the captured log could not
    // say which — the progress lines that could have were never written to a
    // file. The line decides nothing; `decisions` readers skip it.
    let t0 = Instant::now();
    let clocks: Mutex<Vec<Option<(Duration, Duration)>>> = Mutex::new(vec![None; plan.len()]);

    // Stages run concurrently, so a long one would otherwise be silent until its
    // turn to print arrives. Progress is stderr-only and terminal-only: stdout
    // stays a clean, diffable ladder, and a captured log stays a record of
    // decisions rather than of waiting. It deliberately does NOT report outcomes
    // — there is one vocabulary for those and it is the ladder's.
    let progress = std::io::stderr().is_terminal();
    let done = sched::run_stages(
        &plan,
        |spec| {
            let started = Instant::now();
            let begun = t0.elapsed();
            let stamp_start = ctx.timings.as_ref().map(exec::Timings::now);
            if progress {
                eprintln!("verify: start  {}", spec.title);
            }
            let lane = format!("{:?}", spec.lane);
            let report = exec::with_stage(&spec.title, &lane, || {
                // A stage that would build or drive something must not start
                // on a tree or a compiler other than the one this run named.
                if spec.lane != plan::Lane::Pure
                    && let Some(why) = tripwire.check(identity::CHECK_CACHE)
                {
                    let mut r = Report::new(spec.title.clone());
                    r.cannot_run(format!("not run: {}", identity::tripped_label(&why)));
                    return r;
                }
                stages::run_stage(ctx, spec)
            });
            tripwire.stage_finished();
            let ran = started.elapsed();
            if progress {
                eprintln!("verify: finish {} ({:.1}s)", spec.title, ran.as_secs_f64());
            }
            if let Some(i) = plan.iter().position(|s| std::ptr::eq(s, spec))
                && let Ok(mut c) = clocks.lock()
            {
                c[i] = Some((begun, ran));
            }
            if let (Some(t), Some(start)) = (&ctx.timings, stamp_start) {
                t.row(&exec::TimingRow {
                    stage: &spec.title,
                    child: "(stage)",
                    lane: &lane,
                    start,
                    end: t.now(),
                    how: outcome_word(&report),
                    load_start: None,
                    load_end: None,
                });
            }
            report
        },
        |i, report| {
            // Flush per stage: the ladder is a live record, and a developer
            // watching a ten-minute run must see each rung as it is decided —
            // the script wrote unbuffered, and a buffered port would look hung.
            let clock = clocks.lock().ok().and_then(|c| c.get(i).copied().flatten());
            let mut text = report.render();
            if let Some((begun, ran)) = clock {
                text.push_str(&time_line(begun, ran));
            }
            if err.is_none()
                && let Err(e) = out.write_all(text.as_bytes()).and_then(|()| out.flush())
            {
                err = Some(e);
            }
        },
    );
    if let Some(e) = err {
        return Err(e);
    }
    reports.extend(done);

    // The check a stage start may have cached is never cached here: a tree
    // that moved during the LAST stage is as unverified as one that moved
    // during the first.
    if let Some(why) = tripwire.check(Duration::ZERO) {
        let mut r = Report::new("source identity");
        r.cannot_run(identity::tripped_label(&why));
        out.write_all(r.render().as_bytes())?;
        reports.push(r);
    }

    // A PLANNED STAGE THAT DECIDED NOTHING IS INVISIBLE TO THE VERDICT.
    //
    // `tally` counts outcomes, not stages, so a stage whose every branch fell
    // through without recording one contributes no ok, no skip and no FAIL — and a
    // whole-tree run missing it would still print the merge-contract sentence. No
    // stage does that today (every terminating path in stages.rs and
    // smoke_stages.rs records an entry), which is exactly why this is worth
    // pinning: the invariant is currently true and nothing was holding it. A new
    // stage with one unhandled branch is the realistic way it breaks, and the
    // failure would be silent in the one direction that matters.
    //
    // Fail CLOSED and loudly: this is a defect in the gate, not a finding about
    // the tree, so it exits COULD-NOT-RUN rather than FAIL.
    if let Some(silent) = reports.iter().find(|r| r.outcomes().next().is_none()) {
        writeln!(
            out,
            "\n  FAIL  gate defect: stage `{}` was planned but recorded no outcome — \
             it cannot be counted, so this run cannot claim anything.",
            silent.title
        )?;
        writeln!(
            out,
            "  VERIFY: COULD NOT RUN — a stage decided nothing. This is NOT a finding \
             about your change."
        )?;
        out.flush()?;
        return Ok(exit::COULD_NOT_RUN);
    }

    let tally = ladder::tally(&reports);
    let verdict = verdict::verdict(ctx.mode, &ctx.scope, ctx.selftest, &tally);
    write_receipt(ctx, &tripwire, &verdict, &tally);
    out.write_all(verdict.text.as_bytes())?;
    out.flush()?;
    Ok(verdict.exit)
}

/// Record what this run decided about this commit, where `.githooks/pre-push`
/// can read it ([`receipt`]).
///
/// Only a run with a SOURCE IDENTITY leaves one: a root that is not a git
/// checkout has no commit to key a receipt by, and a selftest decided nothing
/// about the tree. Failures are announced on stderr and cost the next push a
/// refusal — never this run its verdict.
fn write_receipt(
    ctx: &Ctx,
    tripwire: &identity::Tripwire,
    verdict: &verdict::Verdict,
    tally: &ladder::Tally,
) {
    let identity::SourceIdentity::Git(tree) = &tripwire.source else {
        return;
    };
    if ctx.selftest {
        return;
    }
    let r = receipt::Receipt {
        head: tree.head.clone(),
        dirty: tree.dirty_digest(&ctx.path_env),
        mode: ctx.mode.as_str().to_string(),
        scope: match &ctx.scope {
            Scope::Workspace => "workspace".to_string(),
            Scope::Crate(c) => format!("crate:{c}"),
            Scope::Changed(_) => "changed".to_string(),
        },
        verdict: match verdict.exit {
            exit::PASS => "PASS",
            exit::FAILED => "FAIL",
            _ => "COULD-NOT-RUN",
        }
        .to_string(),
        merge_contract: verdict.claims_merge_contract,
        skipped: if tally.skips.is_empty() {
            "none".to_string()
        } else {
            let shown: Vec<&str> = tally.skips.iter().take(4).map(String::as_str).collect();
            let more = tally.skips.len().saturating_sub(shown.len());
            let mut s = shown.join(", ");
            if more > 0 {
                s.push_str(&format!(" and {more} more"));
            }
            // One line, always: a newline here would forge a second key.
            s.replace('\n', " ")
        },
        when: std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_secs()),
    };
    let caller = match &ctx.source_mode {
        snapshot::SourceMode::Snapshot { caller } => caller.clone(),
        snapshot::SourceMode::InPlace => ctx.root.clone(),
    };
    if let Err(e) = receipt::write(&caller, &r) {
        eprintln!(
            "verify: cannot write the gate receipt under {}: {e} — the next push will refuse \
             for want of one",
            receipt::dir(&caller).display()
        );
    }
}

/// The `  time  ` line under a stage: how long it ran, and when it started
/// relative to the ladder — that offset is time spent waiting to start: for its
/// lane, an earlier exclusive stage, or the lanes it is ordered after; an
/// EXCLUSIVE stage also waits for every earlier unfinished stage in any lane,
/// `Pure` included, and until nothing else is running.
#[must_use]
pub fn time_line(begun: Duration, ran: Duration) -> String {
    format!(
        "  time  {:.1}s (started +{:.1}s)\n",
        ran.as_secs_f64(),
        begun.as_secs_f64()
    )
}

/// A stage's outcome in one word, for its timing row.
fn outcome_word(report: &Report) -> &'static str {
    let mut word = "ok";
    for (o, _) in report.outcomes() {
        match o {
            Outcome::Fail(Severity::CouldNotRun) => return "could-not-run",
            Outcome::Fail(Severity::GateFailed) => word = "FAIL",
            Outcome::Skip if word == "ok" => word = "skip",
            _ => {}
        }
    }
    word
}

/// WHAT `.githooks/pre-push` DOES, in one clause — printed by [`pin_hooks`] to
/// every operator on a fresh clone, and MEASURED against the hook itself by
/// `tests/push_gate.rs`.
///
/// It is a constant rather than a literal because the previous spelling of this
/// sentence was wrong for thirteen months in two different directions (first
/// "L0 gate active" for a hook that ran nothing, then "ADVISORY" after the hook
/// gained teeth), and nothing compared the words with the file.
pub const HOOK_CLAIM: &str = "pre-push BLOCKS a push of any commit with no passing gate receipt \
     (a tag, the release cutter's claim over origin's tip — CHANGELOG.md + RELEASES.ledger only — \
     and a clean automatic merge of a receipted commit onto origin's tip bring no ungated code and \
     owe none of their own); ATERM_PUSH_NO_GATE=1 is the named exception";

/// Where the gate keeps its own copy of each run's ladder, under
/// [`identity::GATE_STATE_DIR`]. Named here because `main` writes it and the
/// tripwire's remedy sentence points at it.
pub const LOG_DIR: &str = "logs";

/// Stage 0 of the script: pin `core.hooksPath` at `.githooks`, so a clone runs
/// the hooks this repo committed rather than the empty `.git/hooks`.
///
/// WHAT THE PIN BUYS, AND WHAT IT DOES NOT — said here because the line this
/// function PRINTS used to oversell it, and that line is read by every operator
/// on a fresh clone. It said `(pre-push L0 gate active)`, which told them a
/// blocking L0 gate had just been switched on for them. Nothing was:
/// `.githooks/pre-push` was ADVISORY from 2026-08-24 to 2026-09-17 — its entire
/// body was one printf and `exit 0` — having been demoted on its own written
/// rule ("a hook slow enough to be bypassed is worse than none") once
/// `tools/paint_guard.sh` took a blocking push to twelve minutes.
///
/// SINCE 2026-09-17 IT GATES AGAIN, without running anything: the hook reads
/// the RECEIPT this run writes ([`receipt`]) and refuses a push of a commit no
/// gate discharged the merge contract on. It is microseconds, so it cannot
/// teach the bypass, and it races no other push, so it cannot lose a ref.
/// [`HOOK_CLAIM`] is the sentence, and `tests/push_gate.rs` measures the hook
/// against it — a claim about a hook that nothing checks is how this line came
/// to say "L0 gate active" for a hook that ran nothing.
///
/// The pin itself is still worth doing and still true: an unpinned clone runs
/// `.git/hooks`, which is empty, so the committed hooks may as well not exist.
///
/// The L0 obligations are ALSO enforced without any hook, by the unconditional
/// freeze-gate stage of THIS run ([`plan::StageId::FreezeGate`]) and by
/// `run_freeze_safety_gate` in `crates/aterm-release/src/publish.rs`, which is
/// mandatory and runs before the ledger claim.
///
/// Idempotent, and skipped entirely under `--selftest`, exactly as before.
fn pin_hooks(ctx: &Ctx, out: &mut dyn Write) -> std::io::Result<()> {
    if ctx.selftest {
        return Ok(());
    }
    let git = |args: &[&str]| -> Option<std::process::Output> {
        Command::new("git")
            .args(args)
            .current_dir(&ctx.root)
            .env("PATH", &ctx.path_env)
            .output()
            .ok()
            .filter(|o| o.status.success())
    };
    if git(&["rev-parse", "--git-dir"]).is_none() {
        return Ok(());
    }
    let current = git(&["config", "core.hooksPath"])
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .unwrap_or_default();
    if current != ".githooks" && git(&["config", "core.hooksPath", ".githooks"]).is_some() {
        writeln!(
            out,
            "  hooks pinned: core.hooksPath = .githooks ({HOOK_CLAIM})"
        )?;
    }
    Ok(())
}

/// Find the repo root by walking up from `start` until a directory holds both a
/// `Cargo.toml` and `tools/verify.sh`.
///
/// The script derived the root from its own path; a compiled binary lives in
/// `target/debug/`, which is not a stable anchor (`CARGO_TARGET_DIR` moves it),
/// so the driver looks upward from where it was invoked instead. The shim passes
/// `--root` explicitly, which skips this entirely.
#[must_use]
pub fn locate_root(start: &Path) -> Option<PathBuf> {
    let mut dir = Some(start);
    while let Some(d) = dir {
        if d.join("Cargo.toml").is_file() && d.join("tools/verify.sh").is_file() {
            return Some(d.to_path_buf());
        }
        dir = d.parent();
    }
    None
}

/// `mktemp -d` — the same tool the script used, for the same reason: it creates
/// the private directory atomically, and `/tmp` keeps unix-socket paths well
/// under macOS's 104-byte `sockaddr_un` ceiling however long `TMPDIR` is.
///
/// # Errors
/// Fails when `mktemp` is absent or refuses to create the directory.
pub fn mktemp_dir(tag: &str) -> std::io::Result<PathBuf> {
    let out = Command::new("mktemp")
        .arg("-d")
        .arg(format!("/tmp/{tag}.XXXXXX"))
        .output()?;
    if !out.status.success() {
        return Err(std::io::Error::other(format!(
            "mktemp -d /tmp/{tag}.XXXXXX failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        )));
    }
    let path = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if path.is_empty() {
        return Err(std::io::Error::other("mktemp -d printed no path"));
    }
    Ok(PathBuf::from(path))
}

/// `[ -x <path> ]` for a file: exists, is not a directory, and carries an
/// execute bit. Used for every "is the tool present" decision, so a
/// non-executable script is the same event as a missing one — the script's rule.
#[cfg(unix)]
#[must_use]
pub fn is_executable_file(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(path)
        .map(|m| !m.is_dir() && m.permissions().mode() & 0o111 != 0)
        .unwrap_or(false)
}

/// Windows has no execute bit — a file is runnable by extension (`PATHEXT`), not by
/// mode — so "exists and is not a directory" is the whole of the test there. Matches
/// `aterm_containment::allowlist`'s split of the same predicate.
#[cfg(not(unix))]
#[must_use]
pub fn is_executable_file(path: &Path) -> bool {
    std::fs::metadata(path)
        .map(|m| !m.is_dir())
        .unwrap_or(false)
}

/// `command -v <name>` against the PATH the gate hands its children.
#[must_use]
pub fn have_on_path(name: &str, path: &OsStr) -> bool {
    std::env::split_paths(path).any(|d| is_executable_file(&d.join(name)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn locate_root_walks_up_to_the_marker_pair() {
        let tmp = mktemp_dir("atv-root").expect("mktemp");
        let root = tmp.join("repo");
        let deep = root.join("crates/aterm-verify/src");
        std::fs::create_dir_all(&deep).expect("mkdir");
        std::fs::create_dir_all(root.join("tools")).expect("mkdir");
        // Only Cargo.toml: not a root yet — both markers are required, so a
        // nested crate manifest can never be mistaken for the workspace.
        std::fs::write(root.join("Cargo.toml"), b"[workspace]\n").expect("write");
        std::fs::write(root.join("crates/aterm-verify/Cargo.toml"), b"[package]\n").expect("write");
        assert_eq!(locate_root(&deep), None);

        std::fs::write(root.join("tools/verify.sh"), b"#!/bin/sh\n").expect("write");
        assert_eq!(locate_root(&deep).as_deref(), Some(root.as_path()));
        assert_eq!(locate_root(&root).as_deref(), Some(root.as_path()));
        std::fs::remove_dir_all(&tmp).ok();
    }

    /// The `#[cfg(unix)]` half of [`is_executable_file`]. Gated because the
    /// PREMISE is: a 0644 file is not executable, which is only true where a
    /// mode exists. The `not(unix)` twin below pins the other half, so neither
    /// arm of the predicate is unpinned on the target it ships to.
    #[cfg(unix)]
    #[test]
    fn executable_test_matches_the_shell_test() {
        use std::os::unix::fs::PermissionsExt;
        let tmp = mktemp_dir("atv-exec").expect("mktemp");
        let f = tmp.join("script.sh");
        std::fs::write(&f, b"#!/bin/sh\n").expect("write");
        assert!(!is_executable_file(&f), "0644 file is not executable");
        std::fs::set_permissions(&f, std::fs::Permissions::from_mode(0o755)).expect("chmod");
        assert!(is_executable_file(&f));
        assert!(
            !is_executable_file(&tmp),
            "a directory is not an executable file"
        );
        assert!(!is_executable_file(&tmp.join("absent")));
        std::fs::remove_dir_all(&tmp).ok();
    }

    /// The `not(unix)` half: no execute bit exists, so a plain file IS runnable
    /// (`PATHEXT` decides, not a mode) and only the directory and absent cases
    /// remain false. Until `xtask gate cells` began type-checking test targets
    /// this arm of the shipped predicate was compiled by no test anywhere.
    #[cfg(not(unix))]
    #[test]
    fn executable_test_is_existence_where_there_is_no_execute_bit() {
        let tmp = mktemp_dir("atv-exec").expect("mktemp");
        let f = tmp.join("script.sh");
        std::fs::write(&f, b"#!/bin/sh\n").expect("write");
        assert!(
            is_executable_file(&f),
            "no execute bit exists here: a plain file is runnable"
        );
        assert!(
            !is_executable_file(&tmp),
            "a directory is not an executable file"
        );
        assert!(!is_executable_file(&tmp.join("absent")));
        std::fs::remove_dir_all(&tmp).ok();
    }

    #[test]
    fn every_child_inherits_the_wall_clock_ceiling_from_the_snapshot() {
        // The wiring, end to end: the one environment read (`EnvSnapshot`), the
        // pure policy (`exec::ceiling_from_env`), and the value every stage
        // child is actually launched with (`Ctx::exec_env`). A gate whose stages
        // silently ran with `child_ceiling: None` would hang exactly as it did
        // before, and nothing else in the tree would notice.
        let tmp = mktemp_dir("atv-ceiling").expect("mktemp");
        let ctx = |raw: Option<&str>| {
            let env = EnvSnapshot {
                stage_timeout: raw.map(OsString::from),
                ..EnvSnapshot::default()
            };
            Ctx::new(
                tmp.clone(),
                Mode::Fast,
                Scope::Workspace,
                true,
                env,
                tmp.clone(),
            )
        };
        assert_eq!(
            ctx(None).exec_env().child_ceiling,
            Some(exec::DEFAULT_CHILD_CEILING),
            "an unset override still leaves the backstop in place"
        );
        assert_eq!(
            ctx(Some("120")).exec_env().child_ceiling,
            Some(std::time::Duration::from_secs(120))
        );
        assert_eq!(
            ctx(Some("off")).exec_env().child_ceiling,
            None,
            "and an operator who types `off` gets the old unbounded wait"
        );
        std::fs::remove_dir_all(&tmp).ok();
    }

    /// THE PIN for the question that killed an 80-minute gate on 2026-09-10.
    ///
    /// A run was aborted on the hypothesis that the gate resolved its compiler
    /// through a mutable rustup symlink and that a peer's re-seal had split it
    /// across two toolchains. The shim had in fact resolved a PHYSICAL path
    /// once and prepended it for the whole run — but the gate printed nothing
    /// about it, so the claim could be neither supported nor refuted without a
    /// code read, and two readers believed it. This holds the sentence that
    /// answers it in the record itself, and holds that the two ways of NOT
    /// being able to answer are said out loud rather than dressed as answers.
    #[test]
    fn the_gate_names_the_absolute_toolchain_it_resolved_and_admits_when_it_cannot() {
        let line = toolchain_header_line(
            std::path::Path::new("/Users//x/toolchains/trust-current/bin"),
            Some("trust"),
            true,
        );
        assert!(line.starts_with("verify: toolchain /Users//x/toolchains/trust-current/bin "));
        assert!(line.contains("channel \"trust\""));
        assert!(
            line.contains("absolute and resolved once"),
            "the line must say the path is PINNED, not merely print one: {line}"
        );
        assert!(line.ends_with('\n') && line.matches('\n').count() == 1);

        // A relative path is a gate that cannot say which compiler it used.
        let rel = toolchain_header_line(std::path::Path::new("bin"), Some("trust"), true);
        assert!(rel.contains("RELATIVE"), "{rel}");
        assert!(!rel.contains("resolved once"), "{rel}");

        // And no toolchain at all is NONE, never a bare path that reads like one.
        let none = toolchain_header_line(
            std::path::Path::new("/Users//x/trust/build/host/stage2/bin"),
            None,
            false,
        );
        assert!(none.contains("toolchain NONE"), "{none}");
        assert!(none.contains("<unpinned>"), "{none}");
    }
}
