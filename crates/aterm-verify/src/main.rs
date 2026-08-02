// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `aterm-verify` — the merge gate's entrypoint.
//!
//! Invoked by the thin `tools/verify.sh` shim, which keeps the ONE job that must
//! work when the compiler will not compile this program: resolving
//! `$TRUST_STAGE2_BIN` to a physical path, putting it first on PATH, and printing
//! the flag-spelling-skew diagnostic. Everything after that is here.
//!
//! stdout is the ladder and nothing else, so a run can be diffed, piped or pasted
//! into a review. Progress goes to stderr, and only when stderr is a terminal —
//! a captured log is a record of decisions, not of waiting.

use std::io::{IsTerminal, Write};
use std::path::{Path, PathBuf};
use std::time::Instant;

use aterm_verify::cli::{self, USAGE};
use aterm_verify::ladder::Report;
use aterm_verify::{Ctx, EnvSnapshot, Scope, Toolchain, changed, exit};

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let parsed = match cli::parse(args) {
        Ok(a) => a,
        Err(e) => {
            eprintln!("{}", e.message());
            print!("{USAGE}");
            std::process::exit(exit::USAGE);
        }
    };
    if parsed.help {
        print!("{USAGE}");
        std::process::exit(0);
    }

    let env = EnvSnapshot::capture();
    let Some(root) = resolve_root(parsed.root.clone()) else {
        eprintln!(
            "verify: cannot find the repo root (no directory above the cwd holds both \
             Cargo.toml and tools/verify.sh) — pass --root <dir> or set ATERM_VERIFY_ROOT"
        );
        std::process::exit(exit::COULD_NOT_RUN);
    };

    // The scratch directory holds captured child output. Without it the gate can
    // still decide nothing, so this is a COULD-NOT-RUN, not a silent degradation.
    let scratch = match aterm_verify::mktemp_dir("atv") {
        Ok(p) => p,
        Err(e) => {
            eprintln!("verify: cannot create a scratch directory: {e}");
            std::process::exit(exit::COULD_NOT_RUN);
        }
    };

    // `--changed` decides the scope BEFORE the ladder is planned, so it runs
    // here rather than as a stage: every header below names the scope it picks.
    let (scope, prelude) = resolve_scope(&parsed, &root, &env);

    let ctx = Ctx::new(
        root,
        parsed.mode,
        scope,
        parsed.selftest,
        env,
        scratch.clone(),
    )
    .with_prelude(prelude);

    let started = Instant::now();
    let stdout = std::io::stdout();
    let mut out = std::io::BufWriter::new(stdout.lock());
    let code = match aterm_verify::run(&ctx, &mut out) {
        Ok(code) => code,
        Err(e) => {
            let _ = writeln!(std::io::stderr(), "verify: cannot write the ladder: {e}");
            exit::COULD_NOT_RUN
        }
    };
    let _ = out.flush();
    drop(out);
    std::fs::remove_dir_all(&scratch).ok();

    if std::io::stderr().is_terminal() {
        let secs = started.elapsed().as_secs_f64();
        let _ = writeln!(
            std::io::stderr(),
            "verify: finished in {secs:.1}s (exit {code})"
        );
    }
    std::process::exit(code);
}

/// What this run is narrowed to, and the stage report that explains it.
///
/// `--scope` is the flag's value and nothing else. `--changed` has to READ the
/// repo — the diff, the workspace members, the inverted graph — and any part of
/// that it cannot read widens the run back to the whole workspace and says so;
/// see [`changed`]. The toolchain is discovered twice (here and in [`Ctx::new`])
/// because the selection needs `targo` before a `Ctx` exists: a handful of
/// `stat` calls, against a decision that must not be taken twice.
fn resolve_scope(parsed: &cli::Args, root: &Path, env: &EnvSnapshot) -> (Scope, Option<Report>) {
    if !parsed.changed {
        return (Scope::from_option(parsed.scope.clone()), None);
    }
    let base = parsed.base_ref(env.verify_base.as_deref());
    let tools = Toolchain::discover(env.trust_stage2_bin.as_deref(), &env.home);
    let path_env = tools.path_with_stage2_first(&env.path);
    let selection = changed::resolve(root, &tools, &path_env, &base);
    let (scope, report) = changed::stage_report(&base, &selection);
    (scope, Some(report))
}

/// `--root`, then `ATERM_VERIFY_ROOT`, then a walk up from the cwd.
fn resolve_root(flag: Option<PathBuf>) -> Option<PathBuf> {
    if let Some(r) = flag {
        return Some(r);
    }
    if let Some(r) = std::env::var_os("ATERM_VERIFY_ROOT") {
        return Some(PathBuf::from(r));
    }
    let cwd = std::env::current_dir().ok()?;
    aterm_verify::locate_root(&cwd)
}
