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
//!    way a script can);
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
//!  * The flag spellings `--fast` / `--full` / `--scope <crate>`, so
//!    docs/PROCESS.md and every agent instruction keep working unedited.
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
//!    [`plan::Lane`]) and the stages that MEASURE — the measuring tests
//!    ([`stages::MEASURING_TESTS`]) and the two smokes — run exclusively, because
//!    a gate that decides "present starvation" while a lint compiles on the other
//!    seven cores would be measuring the gate, not the build.
//!  * Exit codes distinguish FAILED from COULD-NOT-RUN (`1` vs `3`) — the
//!    distinction the 633-line BLOCKING `.githooks/pre-push` reasoned about
//!    before it was demoted to advisory on 2026-08-24 (it had separate "✗ LINT
//!    GATE COULD NOT RUN" and "✗ L0 TEMPORAL-SAFETY GATE COULD NOT RUN" arms).
//!    The hook is gone — deleted on 2026-09-25 under the owner's no-hooks
//!    mandate; the distinction is not, because it was never the hook's to own.
//!    `tools/verify.sh` documents all four codes at its hand-off and passes
//!    ours through untouched, and it reaches for `3` itself
//!    on every path where the gate was never built. The ladder line is `FAIL`
//!    either way — a broken environment still never reads as green.
//!  * `--scope=` with an empty value is a usage error instead of silently meaning
//!    "the whole workspace". In bash that spelling widened the claim to the merge
//!    contract; that is the exact class of bug the verdict discipline exists to
//!    stop. `--base=` is a usage error for the same reason: in bash it set an
//!    empty ref, which then failed to find a merge-base and WIDENED the run
//!    without the caller ever learning the flag was malformed.
//!  * Every run verifies a pinned SNAPSHOT of the caller's git checkout
//!    ([`snapshot`], 2026-09-13; a root git cannot open is COULD NOT RUN, and
//!    the `--in-place` and `--selftest` modes are gone since 2026-09-27), and
//!    a compiler or source tree that moves under a run stops every stage not
//!    yet started and adds a `source identity` COULD NOT RUN row
//!    ([`identity`]): a run with nothing failed ends COULD NOT RUN (exit 3),
//!    and a stage that already FAILED keeps FAIL (exit 1). The 14 h `--fast`
//!    run that motivated both was pulled four times mid-ladder in a live,
//!    shared checkout and still printed one verdict. Neither changes any
//!    stage's argv.
//!  * The change-scoped SELECTION is a pure function of (diff paths, manifests,
//!    members, inverted graph), so its seeds, its reverse-dependency closure and
//!    every one of its widening triggers are unit-testable without a repo. In
//!    bash the same logic could only be exercised by running the gate inside a
//!    git checkout with a Trust stage2 installed, which is to say: never.

pub mod changed;
pub mod cli;
pub mod disk;
pub mod exec;
pub mod glob;
pub mod identity;
pub mod ladder;
pub mod lease;
pub mod libtest;
pub mod plan;
pub mod receipt;
pub mod sched;
pub mod scope;
// The smokes drive a unix control socket with unix process plumbing, and the
// ladder that runs them is a unix program (`tools/verify.sh`); off unix the
// two smoke rows are named skips (`stages::run_stage`).
#[cfg(unix)]
pub mod smoke;
#[cfg(unix)]
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
pub use ladder::{Outcome, Report, Severity};
pub use scope::Scope;
pub use toolchain::Toolchain;
pub use verdict::{MERGE_CONTRACT_SENTENCE, Verdict};

/// Exit codes. FAILED and COULD-NOT-RUN are kept apart because a broken
/// environment is not a finding about the tree, and conflating them is how a
/// gate stops being read.
///
/// The blocking pre-push hook reasoned about that distinction, and this comment
/// used to cite it as the reason. It is no longer a caller — it was advisory
/// from 2026-08-24, read this run's RECEIPT ([`receipt`]) from 2026-09-17, and
/// was deleted on 2026-09-25 (the owner's no-hooks mandate) — but the reason
/// outlived it, and the distinction has a live consumer either way: `tools/verify.sh`
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
    pub trust_stage2_bin: Option<PathBuf>,
    pub trust_mc_sysroot: Option<PathBuf>,
    pub ay_bin_dir: Option<PathBuf>,
    /// `XDG_CONFIG_HOME` — where atpkg's own config (`aterm/aterm.toml`, the
    /// `[packages].prefix` override) lives when set; the store probe reads the
    /// same file atpkg does ([`toolchain::atpkg_prefix`]).
    pub xdg_config_home: Option<PathBuf>,
    /// `RUSTUP_HOME`, raw — where rustup keeps its toolchains when set (and not
    /// empty); [`toolchain::rustup_home`] turns it and `home` into the directory
    /// discovery probes, `~/.rustup` when unset.
    pub rustup_home: Option<OsString>,
    pub ssh_connection: Option<String>,
    /// `CARGO_BUILD_JOBS` — the caller's job count. The main lane and the lint
    /// lane inherit it as they inherit every variable; for the SIDE lanes, whose
    /// caps are constants ([`stages::lane_build_jobs`]), it is a CEILING
    /// ([`stages::lane_jobs`]): a 4-core machine that exports `4` must not have
    /// the driver lane override it back up to 8. Kept RAW here and parsed by the
    /// pure function: the snapshot's job is to read the environment exactly once,
    /// on the main thread.
    pub cargo_build_jobs: Option<OsString>,
    /// `RUSTDOC` or `CARGO_BUILD_RUSTDOC` — a caller-supplied doc-driver
    /// binding. Cargo prefers either over the config's `[build] rustdoc`, so
    /// the children inherit it and the doc-driver rule must account for it:
    /// an explicit export is the operator's own per-invocation override (the
    /// same escape hatch verify.sh's header sanctions for flag-spelling skew),
    /// never grounds for a COULD-NOT-RUN.
    pub rustdoc_override: Option<OsString>,
}

impl EnvSnapshot {
    /// Read the process environment.
    #[must_use]
    pub fn capture() -> Self {
        let var_path = |k: &str| std::env::var_os(k).map(PathBuf::from);
        Self {
            path: std::env::var_os("PATH").unwrap_or_default(),
            home: var_path("HOME").unwrap_or_default(),
            trust_stage2_bin: var_path("TRUST_STAGE2_BIN"),
            trust_mc_sysroot: var_path("TRUST_MC_SYSROOT"),
            ay_bin_dir: var_path("AY_BIN_DIR"),
            xdg_config_home: var_path("XDG_CONFIG_HOME").filter(|p| !p.as_os_str().is_empty()),
            rustup_home: std::env::var_os("RUSTUP_HOME"),
            ssh_connection: std::env::var("SSH_CONNECTION").ok(),
            cargo_build_jobs: std::env::var_os("CARGO_BUILD_JOBS"),
            // NO empty-filter, deliberately: cargo has no treat-empty-as-unset
            // rule for these (only RUSTC_WRAPPER gets one), so a set-but-empty
            // RUSTDOC still masks CARGO_BUILD_RUSTDOC and still reaches the
            // child — the snapshot mirrors that precedence exactly, and a
            // broken export fails under the "(caller's RUSTDOC)" label that
            // names whose binding it was.
            rustdoc_override: std::env::var_os("RUSTDOC")
                .or_else(|| std::env::var_os("CARGO_BUILD_RUSTDOC")),
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
    /// The caller's checkout this run's root is a SNAPSHOT of
    /// ([`Ctx::in_snapshot_of`]) — every run of the gate binary. `None` only
    /// for a context the gate's own tests build in-process on a root of their
    /// making, whose ladder runs on that root.
    pub snapshot_of: Option<PathBuf>,
    /// `verify: …` header lines — the snapshot's (cold or pruned lanes) and
    /// the pinned child facts — printed under the source line.
    pub notes: Vec<String>,
    /// Variables removed from every child's inherited environment
    /// ([`exec::ExecEnv::remove_env`]): [`CHILD_ENV_REMOVED`], always.
    pub child_env_remove: Vec<&'static str>,
    /// Variables given to every child ([`exec::ExecEnv::add_env`]): the
    /// constants in [`CHILD_ENV`], seeded here at construction, plus the run's
    /// pinned git stamp and its pinned test concurrency, appended by
    /// [`Ctx::with_pinned_child_facts`]. The resolved facts are resolved once,
    /// here, so every stage of one run agrees and a wrapper cannot change what
    /// the merge contract measured without the receipt saying so.
    ///
    /// BUILDERS APPEND TO THIS VECTOR. Assigning to it would silently drop the
    /// seeded constants on every context, and `CARGO_INCREMENTAL=0` is one of
    /// them — the 36 GB -> 55 GB lane growth would come back with nothing in
    /// the tree saying why.
    pub child_env_add: Vec<(std::ffi::OsString, std::ffi::OsString)>,
    /// The wall-clock ceiling on one stage child: [`exec::DEFAULT_CHILD_CEILING`]
    /// unless `--stage-timeout` moved it, `None` for `--stage-timeout off`.
    pub child_ceiling: Option<std::time::Duration>,
    /// `--skip-gui-smoke`: the GUI smoke answers a named SKIP.
    pub skip_gui_smoke: bool,
    /// The source state a snapshot's sync verified. The tripwire arms on it
    /// (and compares it with a fresh capture) rather than re-arming from
    /// whatever the root holds by the time the ladder starts.
    pub source_baseline: Option<identity::TreeState>,
    /// The numbers the disk preflight budgets this run with ([`disk`]):
    /// [`disk::Budget::MEASURED`] in every real run. The preflight laws scale
    /// them down to bytes, so a fixture's few kilobytes of lanes can stand for
    /// a snapshot's gigabytes.
    pub disk_budget: disk::Budget,
    /// `--disk-floor`: require exactly this much free and skip the estimate.
    /// `None` in a real run unless the flag was given. The gate's own fixture
    /// tests set `Some(0)`, and the preflight laws set their own, so none of
    /// them depends on how full the host volume happens to be.
    pub disk_floor: Option<u64>,
    /// What the preflight calls, with the run's root, to read the free space
    /// instead of [`disk::read_free`]'s `df`: `None` in a real run. The
    /// preflight laws that budget with scaled numbers set it, so what they
    /// decide is a function of the numbers they chose and never of the host
    /// volume. A function of the root rather than a fixed reading, so a law can
    /// answer from what the root holds WHEN it is called — which is how the
    /// order of the cap's removal and the read is pinned.
    pub disk_free: Option<fn(&Path) -> disk::Reading>,
    /// Where each stage's FINISH line goes the moment the stage finishes —
    /// the run's own log ([`Ctx::with_progress_log`]). The ladder prints in
    /// declared order, so a verdict decided in minute one used to stay unread
    /// until the hour-long test stage ahead of it printed; this line is the
    /// early read, and it names the outcome.
    pub progress_log: Option<std::fs::File>,
}

/// The git stamp [`Ctx::with_pinned_child_facts`] hands every child: the inputs
/// `crates/aterm-gui/build.rs` reads IN PLACE of asking git (its commit, full
/// commit and dev counter). Build-script inputs of the gate's own child builds,
/// set here and read by no shipped code — `aterm-update-core`'s `env_reads` gate
/// lists this table as one the gate WRITES.
pub const GIT_STAMP_ENV: [&str; 3] = [
    "ATERM_BUILD_GIT_COMMIT",
    "ATERM_BUILD_GIT_COMMIT_FULL",
    "ATERM_BUILD_DEV_COMMITS",
];

/// Variables REMOVED from every child's inherited environment, in every run.
///
/// `CARGO_TARGET_DIR` (2026-09-13; unconditional since 2026-09-27, when every
/// run became a snapshot). Every lane names its own directory under the run's
/// root ([`plan::lane_dir`]), and a caller's redirect reaching a main-lane
/// cargo child would build in the caller's contended target dir while the
/// stages looked for its binaries in the snapshot's.
pub const CHILD_ENV_REMOVED: [&str; 1] = ["CARGO_TARGET_DIR"];

/// Variables SET in every child's environment, in every run — after
/// [`CHILD_ENV_REMOVED`] and before a stage's own [`exec::Cmd::envs`], so the
/// gate's setting beats the caller's shell and a stage that names the variable
/// itself still wins ([`exec::ExecEnv::add_env`]).
///
/// These seed [`Ctx::child_env_add`] at construction, so they hold on EVERY
/// context — including one that never called
/// [`Ctx::with_pinned_child_facts`], which appends the run's two resolved
/// facts (the git stamp and the test concurrency) to the same vector. A pin
/// that only a builder installs is a pin a caller can forget.
///
/// `CARGO_INCREMENTAL=0` (2026-09-21). The merge-contract tiers build each
/// commit ONCE and reuse no cache afterwards, so incremental compilation buys
/// them nothing and costs them the disk. (The reason given here until
/// 2026-09-21 — "a snapshot nothing edits between runs" — was false for the
/// one tier where incremental would have paid: `--changed` re-syncs the
/// snapshot from an edited tree on every run, and gives up its incremental
/// rebuilds to keep the lanes bounded. That is a trade, and it is stated as
/// one.) MEASURED across incremental `--fast` runs:
/// the snapshot's `target/` grew 36 GB -> 55 GB, `target-tippy/` held 16-18 GB
/// and `target-drivers/` 16-20 GB, and on 2026-09-20 two contract runs died
/// mid-ladder with `No space left on device` — their logs, in the snapshot's
/// `.aterm-verify/logs`, name the stage: `13a8494eb`'s could not start its
/// test stage (`aterm-verify: cannot run …/targo: No space left on device`),
/// and `cb770c598`'s first run died inside its build. With those dirs deleted
/// and `CARGO_INCREMENTAL=0` in the environment, the next run of `cb770c598`
/// built them cold and left a snapshot measuring 23 GiB (`du -sh`, read by
/// hand) — the incremental artifacts were most of the bloat ([`disk`] has the
/// runs since). A
/// caller's own `CARGO_INCREMENTAL=1` is overridden on purpose: it would
/// re-create that growth on a run whose caches nobody reuses. The lane stamps
/// stopped recording the variable the same day (`snapshot::LANE_ENV_VARS`).
pub const CHILD_ENV: [(&str, &str); 1] = [("CARGO_INCREMENTAL", "0")];

/// THE RUN'S TOOLCHAIN, discovered from `env` under the pin `root` declares:
/// the one [`Toolchain::discover_with_store`] a run makes, under the atpkg
/// prefix resolved from the snapshot (the same file atpkg reads) — the prefix
/// every store probe is handed (the compiler's, and the trust-mc / ay lanes' in
/// `stages::kani_floor`).
///
/// ONE PER RUN, and handed to every consumer (review of 2026-09-25): the lane
/// stamp (`main`'s snapshot), `--changed`'s selection and the stages each ran
/// their own discovery, and since the staleness rule each discovery races two
/// `trustc -vV` date probes against [`toolchain`]'s 5 s bound — a date that
/// times out never demotes. After an atpkg update lays a new build, its first
/// exec can take many seconds (a first-exec assessment), so one discovery
/// timed out and kept the old rustup stage2 while the next demoted it: the
/// stamp named one compiler and the stages ran another, and the next run
/// PRUNED the lanes the new compiler had built. `main` calls this once and
/// builds its context with [`Ctx::new_with_tools`].
#[must_use]
pub fn run_toolchain(env: &EnvSnapshot, root: &Path) -> Toolchain {
    let prefix = toolchain::atpkg_prefix(&env.home, env.xdg_config_home.as_deref());
    Toolchain::discover_with_store(
        env.trust_stage2_bin.as_deref(),
        &toolchain::rustup_home(env.rustup_home.as_deref(), &env.home),
        Some(&prefix),
        &env.path,
        crate::toolchain::pinned_channel(root).as_deref(),
    )
}

impl Ctx {
    /// Build the run context, discovering its toolchain ([`run_toolchain`]).
    /// `scratch` must already exist. A caller that has already discovered the
    /// run's toolchain for another use hands it over with
    /// [`Self::new_with_tools`] instead: two discoveries in one run can
    /// disagree.
    #[must_use]
    pub fn new(
        root: PathBuf,
        mode: Mode,
        scope: Scope,
        env: EnvSnapshot,
        scratch: PathBuf,
    ) -> Self {
        let tools = run_toolchain(&env, &root);
        Self::new_with_tools(root, mode, scope, env, scratch, tools)
    }

    /// [`Self::new`] with the run's ONE toolchain already discovered
    /// ([`run_toolchain`]), so the lane stamp, the scope and every stage name
    /// the same compiler.
    #[must_use]
    pub fn new_with_tools(
        root: PathBuf,
        mode: Mode,
        scope: Scope,
        env: EnvSnapshot,
        scratch: PathBuf,
        tools: Toolchain,
    ) -> Self {
        let path_env = tools.path_with_stage2_first(&env.path);
        Self {
            root,
            mode,
            scope,
            tools,
            path_env,
            scratch,
            env,
            prelude: Vec::new(),
            snapshot_of: None,
            notes: Vec::new(),
            child_env_remove: CHILD_ENV_REMOVED.to_vec(),
            child_env_add: CHILD_ENV
                .iter()
                .map(|(k, v)| ((*k).into(), (*v).into()))
                .collect(),
            child_ceiling: Some(exec::DEFAULT_CHILD_CEILING),
            skip_gui_smoke: false,
            source_baseline: None,
            disk_budget: disk::Budget::MEASURED,
            disk_floor: None,
            disk_free: None,
            progress_log: None,
        }
    }

    /// This run's root is a snapshot of `caller`. `tree` is the state the
    /// sync verified; the run's tripwire arms on it.
    #[must_use]
    pub fn in_snapshot_of(
        mut self,
        caller: PathBuf,
        tree: identity::TreeState,
        notes: Vec<String>,
    ) -> Self {
        self.snapshot_of = Some(caller);
        self.source_baseline = Some(tree);
        self.notes.extend(notes);
        self
    }

    /// Extra `verify: …` header lines.
    /// PIN WHAT EVERY CHILD MUST AGREE ON, once, on the main thread.
    ///
    /// TWO FACTS, both of which a run was previously letting each child decide
    /// for itself, and both of which cost a measured defect:
    ///
    /// * THE GIT STAMP. `crates/aterm-gui/build.rs` derives the commit and the
    ///   dev counter from git, and therefore `rerun-if-changed`s the files those
    ///   answers come from. Those files live in the COMMON git dir, which for a
    ///   linked worktree is the MAIN checkout's `.git` — 29 worktrees share one
    ///   here. A `fetch`, `gc` or `pack-refs` in any of them invalidated this
    ///   crate's compile in all of them, mid-gate (`packed-refs` mtime landed
    ///   between two builds of one run, 2026-09-20). Resolved here and passed
    ///   down, the build script asks git nothing and watches nothing, and the
    ///   run's builds become reproducible as a side effect.
    /// * TEST CONCURRENCY. The gate set `RUST_TEST_THREADS` nowhere, so it
    ///   inherited whatever the invoking shell had. A wrapper capping it at 4
    ///   changed what the merge contract measured without saying so, and cost
    ///   2.17x per test on CPU-bound binaries (aterm-effects: 33.18 s uncapped,
    ///   78.38 s capped). A contract that measures a different thing depending
    ///   on who typed the command is not a contract. `test_threads`
    ///   (`--test-threads`) overrides, and whatever is used is recorded.
    /// * THE TOOLCHAIN (2026-09-24). The run resolves ONE physical stage2
    ///   directory ([`Toolchain`]) and prepends it to every child's PATH, but
    ///   the xtask verbs it drives — the Formatting stage's `gate lint
    ///   --fmt-only`, `gate forge`, `gate cells-foreign` — call `Toolchain::discover`
    ///   AGAIN in the child, and discovery ranks the rustup entry and the
    ///   store's MOVING `current` ahead of PATH. So an atpkg update landing
    ///   mid-run (runs of 33 min to 14 h are on record) formatted with the new
    ///   build's trustfmt while the pinned stages built, tested and tippied with
    ///   the old — and the tripwire, which stamps only the pinned directory's
    ///   files, could not see it. On a machine whose rustup link named another
    ///   tree the two disagreed on every run. `$TRUST_STAGE2_BIN` is every
    ///   discovery's explicit, never-fallen-back-from override, so handing the
    ///   children the resolved directory under that name makes each of them
    ///   answer with this run's compiler. Only when a `targo` was really found:
    ///   exporting a refused or absent directory would turn a child's own
    ///   diagnosis into "TRUST_STAGE2_BIN names no toolchain".
    #[must_use]
    pub fn with_pinned_child_facts(mut self, test_threads: Option<std::num::NonZeroU32>) -> Self {
        if self.tools.have_targo() {
            self.notes.push(format!(
                "child env: toolchain pinned to {} (TRUST_STAGE2_BIN)",
                self.tools.stage2_dir.display()
            ));
            self.child_env_add.push((
                "TRUST_STAGE2_BIN".into(),
                self.tools.stage2_dir.clone().into_os_string(),
            ));
        }
        let git = |args: &[&str]| -> Option<String> {
            let out = std::process::Command::new("git")
                .args(args)
                .current_dir(&self.root)
                .output()
                .ok()?;
            out.status
                .success()
                .then(|| String::from_utf8_lossy(&out.stdout).trim().to_string())
        };
        let commit = git(&["rev-parse", "--short=12", "HEAD"]).filter(|c| !c.is_empty());
        let full_commit = git(&["rev-parse", "HEAD"]).filter(|c| !c.is_empty());
        let dev_commits = git(&["describe", "--tags", "--match", "v*.*.0", "--abbrev=0"])
            .and_then(|tag| git(&["rev-list", &format!("{tag}..HEAD"), "--count"]))
            .filter(|c| !c.is_empty());
        // ALL OR NONE. The build script pins only when it has every fact, so half
        // a pin would leave the watch armed while looking pinned.
        if let (Some(commit), Some(full_commit), Some(dev)) = (commit, full_commit, dev_commits) {
            let dirty = git(&["status", "--porcelain"]).is_some_and(|s| !s.is_empty());
            let stamp = if dirty {
                format!("{commit}-dirty")
            } else {
                commit
            };
            self.notes.push(format!(
                "child env: git stamp pinned to {stamp} (+{dev} since tag)"
            ));
            let [commit_key, full_key, dev_key] = GIT_STAMP_ENV;
            self.child_env_add.push((commit_key.into(), stamp.into()));
            self.child_env_add
                .push((full_key.into(), full_commit.into()));
            self.child_env_add.push((dev_key.into(), dev.into()));
        } else {
            self.notes.push(
                "child env: git stamp NOT pinned (this root answers no commit) — the build \
                 script falls back to probing git, and its watch stays armed"
                    .to_string(),
            );
        }

        let threads = test_threads.map_or_else(
            || {
                std::thread::available_parallelism()
                    .map_or(1, |n| n.get())
                    .to_string()
            },
            |n| n.to_string(),
        );
        self.notes
            .push(format!("child env: RUST_TEST_THREADS pinned to {threads}"));
        self.child_env_add
            .push(("RUST_TEST_THREADS".into(), threads.into()));
        self
    }

    #[must_use]
    pub fn with_notes(mut self, notes: impl IntoIterator<Item = String>) -> Self {
        self.notes.extend(notes);
        self
    }

    /// Move the stage-child ceiling (`--stage-timeout`; `None` is `off`).
    #[must_use]
    pub fn with_child_ceiling(mut self, ceiling: Option<std::time::Duration>) -> Self {
        self.child_ceiling = ceiling;
        self
    }

    /// `--skip-gui-smoke`: the GUI smoke answers a named SKIP.
    #[must_use]
    pub fn with_gui_smoke_skipped(mut self, skip: bool) -> Self {
        self.skip_gui_smoke = skip;
        self
    }

    /// Attach a stage decided before the plan existed (see [`Ctx::prelude`]).
    #[must_use]
    pub fn with_prelude(mut self, report: Option<Report>) -> Self {
        self.prelude.extend(report);
        self
    }

    /// Write every stage's finish line to `log` as it happens
    /// ([`Ctx::progress_log`]). The handle should append: the ladder's own
    /// copy goes to the same file, and an appending write lands whole.
    #[must_use]
    pub fn with_progress_log(mut self, log: Option<std::fs::File>) -> Self {
        self.progress_log = log;
        self
    }

    /// Replace the disk preflight's estimate with a fixed requirement
    /// ([`Ctx::disk_floor`]): `--disk-floor <GiB>`
    /// ([`cli::Args::disk_floor_gib`]), and the gate's own fixture tests with
    /// `0`. The `verify: disk …` line says when a floor is in force.
    #[must_use]
    pub fn with_disk_floor(mut self, bytes: u64) -> Self {
        self.disk_floor = Some(bytes);
        self
    }

    /// Budget the disk preflight with other numbers ([`Ctx::disk_budget`]).
    #[must_use]
    pub fn with_disk_budget(mut self, budget: disk::Budget) -> Self {
        self.disk_budget = budget;
        self
    }

    /// Give the disk preflight this free-space reader instead of `df`
    /// ([`Ctx::disk_free`]).
    #[must_use]
    pub fn with_disk_free(mut self, read: fn(&Path) -> disk::Reading) -> Self {
        self.disk_free = Some(read);
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
            child_ceiling: self.child_ceiling,
            remove_env: &self.child_env_remove,
            add_env: &self.child_env_add,
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

/// The line under the header that says why the rustup `trust` toolchain a reader expects
/// is not the one named above it ([`toolchain::Demoted`]): the walk ranked it below the
/// atpkg store for being older. Without it the header names the store and a reader who
/// knows `~/.rustup/toolchains/trust` resolves elsewhere has to re-derive the rule.
#[must_use]
pub fn toolchain_demoted_line(demoted: &toolchain::Demoted) -> String {
    format!("verify: toolchain note — {}\n", demoted.sentence())
}

/// [`toolchain_header_line`] for a live run, plus [`toolchain_demoted_line`] when the
/// walk demoted a rustup entry to get there.
#[must_use]
pub fn toolchain_header(ctx: &Ctx) -> String {
    let mut out = toolchain_header_line(
        &ctx.tools.stage2_dir,
        crate::toolchain::pinned_channel(&ctx.root).as_deref(),
        ctx.tools.have_targo(),
    );
    if let Some(d) = &ctx.tools.demoted {
        out.push_str(&toolchain_demoted_line(d));
    }
    out
}

/// Run the whole gate: ladder and verdict. Returns the process exit code.
///
/// `out` receives, in this order: the [`toolchain_header`] line, the lease line,
/// the prelude rungs, the `verify: source …` line (a git root only) and any
/// `verify:` notes (the snapshot's lanes, the pinned child facts), `verify:
/// lanes over the cap: …` lines naming any lane the cap could not remove, the
/// `verify: disk …` line (the free space on the volume holding the run's root,
/// what its lanes hold, and what this run needs — with the terms of the sum,
/// or as the `--disk-floor` in force), the ladder in declared order with a
/// `  time  ` line under each stage — or, in its place, a `source identity`
/// COULD NOT RUN row for a git checkout the gate cannot read, or a `disk
/// preflight` COULD NOT RUN row for a volume with less free than that
/// ([`disk`]) — the `source identity` row when the toolchain or the source
/// tree moved mid-run, and the verdict (or the gate-defect `FAIL` and
/// `VERIFY: COULD NOT RUN` lines) — byte-for-byte in the vocabulary
/// `tools/verify.sh` established. Live progress goes to stderr so a long stage
/// is not silent without polluting the scannable part, and every stage's
/// [`finish_line`] — its outcome word included — goes to [`Ctx::progress_log`]
/// the moment the stage ends, in the order stages FINISH rather than the order
/// they print.
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
    // THE RUN'S LEASE on that compiler ([`lease`], 2026-09-26), held until this
    // function returns — the whole ladder. Between two stages no process runs from
    // the pinned directory, so without it atpkg could not tell the run was using it:
    // its gc reclaimed a superseded build a long run still needed (COULD NOT RUN),
    // and an unattended trust update re-laid the rustup view between two stages.
    // Said in one line either way; a lease that cannot be taken never stops the run.
    let _lease = if ctx.tools.have_targo() {
        let prefix = toolchain::atpkg_prefix(&ctx.env.home, ctx.env.xdg_config_home.as_deref());
        let who = format!(
            "aterm-verify (pid {}) \u{2014} the merge contract in {}",
            std::process::id(),
            ctx.root.display()
        );
        let taken = lease::take(&prefix, &ctx.tools.stage2_dir, &who, lease::WAIT);
        out.write_all(taken.header_line().as_bytes())?;
        Some(taken)
    } else {
        None
    };
    // The change-scope stage first: it is what CHOSE the scope every header
    // below prints, so a reader meets the narrowing before its consequences.
    for r in &ctx.prelude {
        out.write_all(r.render().as_bytes())?;
    }

    // WHAT THIS RUN IS VERIFYING, captured before anything is planned and
    // re-checked while it runs (2026-09-13). The 14 h run this answers was
    // pulled four times mid-ladder and printed one verdict over all of them.
    let tripwire = identity::Tripwire::arm_against(
        &ctx.root,
        &ctx.path_env,
        ctx.source_baseline.clone(),
        ctx.tools.identity(&ctx.path_env, &ctx.scratch),
    );
    if let Some(line) =
        tripwire.header_line(&snapshot::place(&ctx.root, ctx.snapshot_of.as_deref()))
    {
        out.write_all(line.as_bytes())?;
    }
    for note in &ctx.notes {
        writeln!(out, "{note}")?;
    }

    // A GIT CHECKOUT THE GATE CANNOT READ is not a root without a source
    // (2026-09-13): with no identity there is no tripwire, and a run on it
    // could go green on a tree nothing watched. No stage runs.
    if let identity::SourceIdentity::Unreadable(why) = &tripwire.source {
        let mut r = Report::new("source identity");
        r.cannot_run(identity::unreadable_label(why));
        out.write_all(r.render().as_bytes())?;
        let mut reports = ctx.prelude.clone();
        reports.push(r);
        let verdict = verdict::verdict(ctx.mode, &ctx.scope, &ladder::tally(&reports));
        out.write_all(verdict.text.as_bytes())?;
        out.flush()?;
        return Ok(verdict.exit);
    }

    // THE DISK, before anything is built (2026-09-21). Two contract runs died
    // mid-ladder on a full volume and printed FAIL rows about it; this run
    // budgets what it will write (2026-09-23: from what its lanes already
    // hold, where it used to demand a flat 40 GiB of every run) and refuses —
    // COULD NOT RUN, never a skip — when the volume has less free, printing
    // the arithmetic and the regenerable dirs. Lanes over the cap are removed
    // first, so the reading after it counts their bytes as free. A refusal
    // leaves no receipt: nothing was decided, so the last real judgement of
    // the commit stands.
    let lanes = disk::measure_lanes(&ctx.root);
    let mut plan = disk::plan(
        ctx.disk_budget,
        ctx.disk_floor,
        &lanes,
        disk::Owner::Snapshot,
    );
    if plan.remove {
        plan.unremoved = snapshot::remove_lanes(&ctx.root);
        for why in &plan.unremoved {
            writeln!(out, "verify: lanes over the cap: {why}")?;
        }
    }
    let reading = ctx
        .disk_free
        .map_or_else(|| disk::read_free(&ctx.root), |read| read(&ctx.root));
    out.write_all(disk::header_line(&reading, &plan, &ctx.root).as_bytes())?;
    if let Err(why) = disk::decide(&reading, &plan, &ctx.root) {
        let mut r = Report::new("disk preflight");
        r.cannot_run(why);
        // After a removal the remedy sizes what is left, not what was.
        let lanes = if plan.remove {
            disk::measure_lanes(&ctx.root)
        } else {
            lanes
        };
        r.raw(disk::remedy(
            &ctx.root,
            &plan,
            &lanes,
            &reading,
            ctx.snapshot_of.as_deref(),
        ));
        out.write_all(r.render().as_bytes())?;
        let mut reports = ctx.prelude.clone();
        reports.push(r);
        let verdict = verdict::verdict(ctx.mode, &ctx.scope, &ladder::tally(&reports));
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
    // turn to print arrives. The START line is stderr-only and terminal-only.
    // The FINISH line carries the stage's outcome word and goes to the run's
    // log the moment the stage finishes (2026-09-23): the ladder prints in
    // declared order, so a formatting FAIL decided in minute one stayed unread
    // behind an hour-long test stage. stdout stays the clean, ordered ladder;
    // the outcome word is the ladder's own vocabulary (`outcome_word`).
    let progress = std::io::stderr().is_terminal();
    let done = sched::run_stages(
        &plan,
        |spec| {
            let started = Instant::now();
            let begun = t0.elapsed();
            if progress {
                eprintln!("verify: start  {}", spec.title);
            }
            // A stage that would build or drive something must not start on a
            // tree or a compiler other than the one this run named.
            let report = if spec.lane != plan::Lane::Pure
                && let Some(why) = tripwire.check(identity::CHECK_CACHE)
            {
                let mut r = Report::new(spec.title.clone());
                r.cannot_run(format!("not run: {}", identity::tripped_label(&why)));
                r
            } else {
                stages::run_stage(ctx, spec)
            };
            tripwire.stage_finished();
            let ran = started.elapsed();
            let finished = finish_line(&spec.title, outcome_word(&report), begun, ran);
            if progress {
                eprint!("{finished}");
            }
            if let Some(mut log) = ctx.progress_log.as_ref() {
                // A record, never a decision: a log that cannot be written costs
                // the line, not the run.
                let _ = log.write_all(finished.as_bytes());
            }
            if let Some(i) = plan.iter().position(|s| std::ptr::eq(s, spec))
                && let Ok(mut c) = clocks.lock()
            {
                c[i] = Some((begun, ran));
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
    let verdict = verdict::verdict(ctx.mode, &ctx.scope, &tally);
    write_receipt(ctx, &tripwire, &verdict, &tally);
    out.write_all(verdict.text.as_bytes())?;
    out.flush()?;
    Ok(verdict.exit)
}

/// Record what this run decided about this commit, where the release cutter's
/// receipt report (`crates/aterm-release/src/gates.rs` `receipt_report`) reads
/// it ([`receipt`]).
///
/// Only a run with a SOURCE IDENTITY over a CLEAN tree leaves one: a root
/// that is not a git checkout has no commit to key a receipt by, and a run
/// over uncommitted work verified bytes no commit holds. A weaker receipt
/// never replaces the commit's whole-tree one ([`receipt::write`]). Failures
/// are announced on stderr and cost the commit its record — never this run its
/// verdict.
///
/// WHAT A RECEIPT SAYS ABOUT A RUN THAT COULD NOT RUN (2026-09-21). A verdict
/// of COULD NOT RUN is written as `verdict COULD-NOT-RUN`, `merge-contract no`
/// — a reader counts it as no pass and names it, never mistaking it for a
/// judgement.
/// A run that never reached its verdict writes NOTHING: no snapshot, an
/// unreadable source, a volume with less free than the run needs ([`disk`]),
/// or a ladder that could not be written (`main` exits 3 on the write error
/// before this is called), so the last real judgement of the commit stands.
/// What used to be wrong was upstream of here: a child that never spawned, or
/// died of `No space left on device`, was a `FAIL` row of a finding's
/// severity, so a run that limped to its verdict would have written `verdict
/// FAIL` about a tree nobody judged. [`ladder::Report::fail_child`] now
/// classifies those as COULD NOT RUN.
fn write_receipt(
    ctx: &Ctx,
    tripwire: &identity::Tripwire,
    verdict: &verdict::Verdict,
    tally: &ladder::Tally,
) {
    let identity::SourceIdentity::Git(tree) = &tripwire.source else {
        return;
    };
    if !tree.dirty.is_empty() {
        eprintln!(
            "verify: no gate receipt — this run verified {} plus uncommitted work, which no \
             commit holds; commit, then run the gate on the commit you push",
            tree.head
        );
        return;
    }
    let r = receipt::Receipt {
        head: tree.head.clone(),
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
    let caller = ctx.snapshot_of.clone().unwrap_or_else(|| ctx.root.clone());
    match receipt::write(&caller, &r) {
        Ok(receipt::Written::Stored(_)) => {}
        Ok(receipt::Written::KeptWholeTree(path)) => eprintln!(
            "verify: {} keeps its whole-tree receipt ({}); this run ({}, scope {}) does not \
             replace it",
            r.head,
            path.display(),
            r.verdict,
            r.scope
        ),
        Err(e) => eprintln!(
            "verify: cannot write the gate receipt for {} (from {}): {e} — the release \
             cutter will count this commit as ungated",
            r.head,
            caller.display()
        ),
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

/// The line a stage's end writes to the run's log as it happens: its title,
/// its outcome word, how long it ran and when it started.
#[must_use]
pub fn finish_line(title: &str, word: &str, begun: Duration, ran: Duration) -> String {
    format!(
        "verify: finish {title} — {word} ({:.1}s, started +{:.1}s)\n",
        ran.as_secs_f64(),
        begun.as_secs_f64()
    )
}

/// A stage's outcome in one word, for its timing row and its finish line.
#[must_use]
pub fn outcome_word(report: &Report) -> &'static str {
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

/// Where the gate keeps its own copy of each run's ladder, under
/// [`identity::GATE_STATE_DIR`]. Named here because `main` writes it and the
/// tripwire's remedy sentence points at it.
pub const LOG_DIR: &str = "logs";

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
/// mode — so "exists and is not a directory" is the whole of the test there.
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

    /// ONE RUN, ONE COMPILER (review of 2026-09-25). The lane stamp
    /// (`choose_source`), `--changed`'s selection (`resolve_scope`) and the
    /// stages (`Ctx::new`) each ran a discovery of their own, and each
    /// discovery races the staleness rule's `trustc -vV` probes against a 5 s
    /// bound: a store build's slow first exec made one of them keep the old
    /// rustup stage2 and the next demote it, so the stamp named one compiler
    /// and the stages ran another — and the next run pruned the new
    /// compiler's lanes. The entrypoint now discovers ONCE
    /// ([`run_toolchain`]) and hands that toolchain to all three, and the
    /// context keeps what it is handed. NEGATIVE CONTROL: [`Ctx::new`], the
    /// discovering constructor, finds none in an empty environment — the
    /// handed toolchain is what made the difference.
    #[test]
    fn a_run_discovers_its_toolchain_once_and_hands_it_to_every_consumer() {
        let tmp = mktemp_dir("atv-one-toolchain").expect("mktemp");
        let bin = tmp.join("handed/bin");
        let handed = Toolchain {
            stage2_dir: bin.clone(),
            targo: bin.join("targo"),
            trustdoc: bin.join("trustdoc"),
            tippy: None,
            refused: None,
            store_bin: None,
            demoted: None,
        };
        let ctx = |tools: Option<Toolchain>| {
            let (root, scratch) = (tmp.clone(), tmp.clone());
            let env = EnvSnapshot::default();
            match tools {
                Some(t) => Ctx::new_with_tools(root, Mode::Fast, Scope::Workspace, env, scratch, t),
                None => Ctx::new(root, Mode::Fast, Scope::Workspace, env, scratch),
            }
        };
        assert_eq!(
            ctx(Some(handed.clone())).tools,
            handed,
            "kept, never re-found"
        );
        assert_ne!(
            ctx(None).tools,
            handed,
            "a discovery of its own finds another"
        );
        std::fs::remove_dir_all(&tmp).ok();
    }

    #[test]
    fn every_child_inherits_the_wall_clock_ceiling_the_flag_set() {
        // The wiring, end to end: the flag's value (`--stage-timeout`, parsed by
        // `exec::parse_ceiling`) and the value every stage child is actually
        // launched with (`Ctx::exec_env`). A gate whose stages silently ran with
        // `child_ceiling: None` would hang exactly as it did before, and nothing
        // else in the tree would notice.
        let tmp = mktemp_dir("atv-ceiling").expect("mktemp");
        let ctx = || {
            Ctx::new(
                tmp.clone(),
                Mode::Fast,
                Scope::Workspace,
                EnvSnapshot::default(),
                tmp.clone(),
            )
        };
        assert_eq!(
            ctx().exec_env().child_ceiling,
            Some(exec::DEFAULT_CHILD_CEILING),
            "no flag still leaves the backstop in place"
        );
        let moved = exec::parse_ceiling("120").expect("seconds");
        assert_eq!(
            ctx().with_child_ceiling(moved).exec_env().child_ceiling,
            Some(std::time::Duration::from_secs(120))
        );
        let off = exec::parse_ceiling("off").expect("off");
        assert_eq!(
            ctx().with_child_ceiling(off).exec_env().child_ceiling,
            None,
            "and an operator who types `off` gets the old unbounded wait"
        );
        std::fs::remove_dir_all(&tmp).ok();
    }

    /// The wiring for [`CHILD_ENV`] and [`CHILD_ENV_REMOVED`]: every stage child
    /// is launched with `CARGO_INCREMENTAL=0` and without the caller's
    /// `CARGO_TARGET_DIR`, in a snapshot run and in an in-process context
    /// alike, and nothing the gate REMOVES is also something it sets. A gate
    /// whose children quietly compiled incrementally again would refill the
    /// 55 GB lane this exists to bound, and nothing else in the tree would
    /// notice until the volume did.
    #[test]
    fn every_child_is_launched_with_incremental_compilation_off() {
        let tmp = mktemp_dir("atv-incremental").expect("mktemp");
        let ctx = Ctx::new(
            tmp.clone(),
            Mode::Fast,
            Scope::Workspace,
            EnvSnapshot::default(),
            tmp.clone(),
        );
        let has = |c: &Ctx, key: &str, val: &str| {
            c.exec_env()
                .add_env
                .iter()
                .any(|(k, v)| k == key && v == val)
        };
        assert!(
            has(&ctx, "CARGO_INCREMENTAL", "0"),
            "{:?}",
            ctx.child_env_add
        );
        assert!(
            CHILD_ENV.contains(&("CARGO_INCREMENTAL", "0")),
            "{CHILD_ENV:?}"
        );
        let snap = Ctx::new(
            tmp.clone(),
            Mode::Fast,
            Scope::Workspace,
            EnvSnapshot::default(),
            tmp.clone(),
        )
        .in_snapshot_of(
            tmp.clone(),
            identity::TreeState {
                head: "0".repeat(40),
                dirty: std::collections::BTreeMap::new(),
            },
            Vec::new(),
        );
        assert!(
            has(&snap, "CARGO_INCREMENTAL", "0"),
            "{:?}",
            snap.child_env_add
        );
        // And the run's OWN pinned facts join it rather than replacing it: a
        // context that resolved the git stamp and the test concurrency still
        // carries the incremental pin.
        let pinned = Ctx::new(
            tmp.clone(),
            Mode::Fast,
            Scope::Workspace,
            EnvSnapshot::default(),
            tmp.clone(),
        )
        .with_pinned_child_facts(None);
        assert!(
            has(&pinned, "CARGO_INCREMENTAL", "0"),
            "{:?}",
            pinned.child_env_add
        );
        for (k, _) in CHILD_ENV {
            assert!(
                !snap.exec_env().remove_env.contains(&k),
                "{k} is both set and removed"
            );
        }
        for c in [&ctx, &snap, &pinned] {
            assert!(
                c.exec_env().remove_env.contains(&"CARGO_TARGET_DIR"),
                "a caller's target dir reaches a child: {:?}",
                c.child_env_remove
            );
        }
        assert_eq!(ctx.disk_floor, None, "a real run estimates");
        assert_eq!(ctx.disk_budget, disk::Budget::MEASURED);
        assert!(ctx.disk_free.is_none(), "and reads the real volume");
        std::fs::remove_dir_all(&tmp).ok();
    }

    /// THE TOOLCHAIN IS A PINNED CHILD FACT (2026-09-24). Discovery settled on a
    /// directory found on PATH — no override exported — and every child the run
    /// launches is handed exactly that physical directory as `$TRUST_STAGE2_BIN`, so
    /// the xtask verbs' own `Toolchain::discover` answers with it instead of walking
    /// the rustup entry and the store's moving `current` again. Driven through a real
    /// child, because `add_env` reaching the process is the property, not the vector.
    /// The negative control: a run that found no `targo` exports nothing.
    #[cfg(unix)]
    #[test]
    fn every_child_is_handed_the_runs_own_toolchain_as_the_override() {
        use std::os::unix::fs::PermissionsExt;
        let tmp = mktemp_dir("atv-child-tc").expect("mktemp");
        let stage2 = tmp.join("found-on-path/bin");
        std::fs::create_dir_all(&stage2).expect("mkdir");
        std::fs::write(stage2.join("targo"), b"#!/bin/sh\nexit 0\n").expect("write");
        std::fs::set_permissions(stage2.join("targo"), std::fs::Permissions::from_mode(0o755))
            .expect("chmod");
        let physical = std::fs::canonicalize(&stage2).expect("canonicalize");
        let ctx = |path: &std::path::Path| {
            let env = EnvSnapshot {
                path: path.as_os_str().to_os_string(),
                home: tmp.join("empty-home"),
                ..EnvSnapshot::default()
            };
            Ctx::new(tmp.clone(), Mode::Fast, Scope::Workspace, env, tmp.clone())
                .with_pinned_child_facts(None)
        };

        let pinned = ctx(&stage2);
        assert!(pinned.tools.have_targo(), "{:?}", pinned.tools);
        assert_eq!(pinned.tools.stage2_dir, physical);
        let seen = tmp.join("seen");
        let run = exec::run(
            &exec::Cmd::new("/bin/sh").arg("-c").arg(format!(
                "printf %s \"$TRUST_STAGE2_BIN\" > '{}'",
                seen.display()
            )),
            pinned.exec_env(),
        );
        assert!(run.ok, "the probe child ran");
        assert_eq!(
            std::fs::read_to_string(&seen).expect("the child wrote"),
            physical.display().to_string(),
            "the child must be handed the run's own resolved directory"
        );
        assert!(
            pinned
                .notes
                .iter()
                .any(|n| n.contains("toolchain pinned to") && n.contains("TRUST_STAGE2_BIN")),
            "{:?}",
            pinned.notes
        );

        let none = ctx(&tmp.join("nothing-here"));
        assert!(!none.tools.have_targo());
        assert!(
            !none
                .child_env_add
                .iter()
                .any(|(k, _)| k == "TRUST_STAGE2_BIN"),
            "no toolchain, no override: {:?}",
            none.child_env_add
        );
        std::fs::remove_dir_all(&tmp).ok();
    }

    #[test]
    fn pinned_display_and_native_update_identities_describe_the_same_tree() {
        let tmp = mktemp_dir("atv-git-stamp").expect("mktemp");
        let git = |args: &[&str]| {
            let output = std::process::Command::new("git")
                .args(args)
                .current_dir(&tmp)
                .output()
                .expect("git");
            assert!(output.status.success(), "{output:?}");
            String::from_utf8(output.stdout)
                .expect("git UTF-8")
                .trim()
                .to_string()
        };
        git(&["init", "-q"]);
        git(&[
            "-c",
            "user.name=fixture",
            "-c",
            "user.email=fixture@example.invalid",
            "-c",
            "commit.gpgsign=false",
            "commit",
            "--allow-empty",
            "-m",
            "fixture",
        ]);
        git(&["tag", "v0.1.0"]);
        let full = git(&["rev-parse", "HEAD"]);
        let short = git(&["rev-parse", "--short=12", "HEAD"]);
        for dirty in [false, true] {
            if dirty {
                std::fs::write(tmp.join("untracked"), "changed").expect("dirty fixture");
            }
            let ctx = Ctx::new(
                tmp.clone(),
                Mode::Fast,
                Scope::Workspace,
                EnvSnapshot::default(),
                tmp.clone(),
            )
            .with_pinned_child_facts(None);
            let fact = |key: &str| {
                ctx.child_env_add
                    .iter()
                    .find(|(k, _)| k == key)
                    .map(|(_, value)| value.to_string_lossy().into_owned())
                    .expect("pinned fact")
            };
            assert_eq!(fact(GIT_STAMP_ENV[1]), full);
            assert_eq!(
                fact(GIT_STAMP_ENV[0]),
                if dirty {
                    format!("{short}-dirty")
                } else {
                    short.clone()
                }
            );
            assert_eq!(fact(GIT_STAMP_ENV[2]), "0");
        }
        std::fs::remove_dir_all(tmp).expect("fixture cleanup");
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

        // A demoted rustup entry is SAID under the header, in one line of its own.
        let note = toolchain_demoted_line(&toolchain::Demoted {
            dir: std::path::PathBuf::from("/Users//x/trust/build/aarch64-apple-darwin/stage2/bin"),
            its: "2026-08-20".into(),
            store: "2026-09-17".into(),
            linked: true,
            view: false,
        });
        assert!(
            note.starts_with("verify: toolchain note — rustup `trust` ("),
            "{note}"
        );
        assert!(
            note.contains("a toolchain from 2026-08-20, older than the atpkg store's 2026-09-17"),
            "{note}"
        );
        assert!(
            note.ends_with('\n') && note.matches('\n').count() == 1,
            "{note}"
        );
    }
}
