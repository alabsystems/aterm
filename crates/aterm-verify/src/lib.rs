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
//!    `TRUST_NO_MIGRATE_WARN`, doctests keep `RUSTDOC=<stage2>/trustdoc`, and
//!    every driver invocation still names its lane with `--unverified`.
//!
//! WHAT CHANGED ON PURPOSE
//!  * Independent stages run CONCURRENTLY ([`sched`]) while the OUTPUT stays in
//!    the ladder's declared order, so the run stays scannable. Concurrency is
//!    constrained by the resource each stage actually contends for (see
//!    [`plan::Lane`]) and the two timing-measuring smokes run exclusively, because
//!    a gate that decides "present starvation" while a lint compiles on the other
//!    seven cores would be measuring the gate, not the build.
//!  * Exit codes distinguish FAILED from COULD-NOT-RUN (`1` vs `3`), the same
//!    distinction `.githooks/pre-push` already reasons about. The ladder line is
//!    `FAIL` either way — a broken environment still never reads as green.
//!  * `--scope=` with an empty value is a usage error instead of silently meaning
//!    "the whole workspace". In bash that spelling widened the claim to the merge
//!    contract; that is the exact class of bug the verdict discipline exists to
//!    stop. `--base=` is a usage error for the same reason: in bash it set an
//!    empty ref, which then failed to find a merge-base and WIDENED the run
//!    without the caller ever learning the flag was malformed.
//!  * The change-scoped SELECTION is a pure function of (diff paths, manifests,
//!    members, inverted graph), so its seeds, its reverse-dependency closure and
//!    every one of its widening triggers are unit-testable without a repo. In
//!    bash the same logic could only be exercised by running the gate inside a
//!    git checkout with a Trust stage2 installed, which is to say: never.

pub mod changed;
pub mod cli;
pub mod exec;
pub mod glob;
pub mod ladder;
pub mod plan;
pub mod sched;
pub mod scope;
pub mod smoke;
pub mod smoke_stages;
pub mod stages;
pub mod toolchain;
pub mod verdict;

use std::ffi::{OsStr, OsString};
use std::io::{IsTerminal, Write};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Instant;

pub use cli::Mode;
pub use ladder::{Entry, Outcome, Report, Severity, Tally};
pub use scope::Scope;
pub use toolchain::Toolchain;
pub use verdict::{MERGE_CONTRACT_SENTENCE, Verdict};

/// Exit codes. The pre-push hook already reasons about FAILED vs COULD-NOT-RUN,
/// so the gate speaks the same distinction out loud.
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
    pub ssh_connection: Option<String>,
    pub skip_gui_smoke: Option<String>,
    /// `ATERM_VERIFY_BASE` — the default `--base` for `--changed`.
    pub verify_base: Option<String>,
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
            ssh_connection: std::env::var("SSH_CONNECTION").ok(),
            skip_gui_smoke: std::env::var("ATERM_SKIP_GUI_SMOKE").ok(),
            verify_base: std::env::var("ATERM_VERIFY_BASE")
                .ok()
                .filter(|s| !s.is_empty()),
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
}

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
        let tools = Toolchain::discover(env.trust_stage2_bin.as_deref(), &env.home);
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
        }
    }

    /// Attach a stage decided before the plan existed (see [`Ctx::prelude`]).
    #[must_use]
    pub fn with_prelude(mut self, report: Option<Report>) -> Self {
        self.prelude.extend(report);
        self
    }

    /// The command environment: cwd is the repo root (the script `cd`s there and
    /// several stages pass root-relative paths), PATH is the computed one.
    #[must_use]
    pub fn exec_env(&self) -> exec::ExecEnv<'_> {
        exec::ExecEnv {
            cwd: &self.root,
            path: &self.path_env,
            scratch: &self.scratch,
        }
    }

    /// `tools/` — where the guard scripts live.
    #[must_use]
    pub fn tools_dir(&self) -> PathBuf {
        self.root.join("tools")
    }
}

/// Run the whole gate: hooks, ladder, verdict. Returns the process exit code.
///
/// `out` receives the ladder — and only the ladder, in declared order, byte-for-byte
/// in the vocabulary `tools/verify.sh` established. Live progress goes to stderr so
/// a long stage is not silent without polluting the scannable part.
///
/// # Errors
/// Propagates write failures on `out`.
pub fn run(ctx: &Ctx, out: &mut dyn Write) -> std::io::Result<i32> {
    // The change-scope stage first: it is what CHOSE the scope every header
    // below prints, so a reader meets the narrowing before its consequences.
    for r in &ctx.prelude {
        out.write_all(r.render().as_bytes())?;
    }
    pin_hooks(ctx, out)?;

    let plan = plan::plan(ctx);
    let mut reports: Vec<Report> = ctx.prelude.clone();
    reports.reserve(plan.len());
    let mut err = None;

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
            if progress {
                eprintln!("verify: start  {}", spec.title);
            }
            let report = stages::run_stage(ctx, spec);
            if progress {
                let secs = started.elapsed().as_secs_f64();
                eprintln!("verify: finish {} ({secs:.1}s)", spec.title);
            }
            report
        },
        |_, report| {
            // Flush per stage: the ladder is a live record, and a developer
            // watching a ten-minute run must see each rung as it is decided —
            // the script wrote unbuffered, and a buffered port would look hung.
            if err.is_none()
                && let Err(e) = out
                    .write_all(report.render().as_bytes())
                    .and_then(|()| out.flush())
            {
                err = Some(e);
            }
        },
    );
    if let Some(e) = err {
        return Err(e);
    }
    reports.extend(done);

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
    out.write_all(verdict.text.as_bytes())?;
    out.flush()?;
    Ok(verdict.exit)
}

/// Stage 0 of the script: pin the hooks so the L0 pre-push gate is active.
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
        out.write_all(b"  hooks pinned: core.hooksPath = .githooks (pre-push L0 gate active)\n")?;
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
#[must_use]
pub fn is_executable_file(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(path)
        .map(|m| !m.is_dir() && m.permissions().mode() & 0o111 != 0)
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
}
