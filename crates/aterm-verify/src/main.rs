// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `aterm-verify` — the merge gate's entrypoint.
//!
//! Invoked by the thin `tools/verify.sh` shim, which keeps the ONE job that must
//! work when the compiler will not compile this program: resolving
//! `$TRUST_STAGE2_BIN` to a physical path, putting it first on PATH, and printing
//! the flag-spelling-skew diagnostic. Everything after that is here.
//!
//! stdout is what `aterm_verify::run` writes and nothing else — the toolchain
//! header, the ladder, the verdict; its doc lists every line — so a run can be
//! diffed, piped or pasted into a review. Progress goes to stderr, and only when
//! stderr is a terminal — a captured log is a record of decisions, not of waiting.
//!
//! The one exception is a run that could not get its SNAPSHOT (2026-09-13):
//! it prints the `snapshot:` FAIL and COULD NOT RUN and exits `3` before any
//! stage, because running in place instead would bring back the live-checkout
//! hazards the snapshot exists to remove ([`aterm_verify::snapshot`]).

use std::io::{IsTerminal, Write};
use std::path::{Path, PathBuf};
use std::time::Instant;

use aterm_verify::cli;
use aterm_verify::ladder::Report;
use aterm_verify::{Ctx, EnvSnapshot, Scope, Toolchain, changed, exec, exit, identity, snapshot};

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let parsed = match cli::parse(args) {
        Ok(a) => a,
        Err(e) => {
            eprintln!("{}", e.message());
            print!("{}", cli::usage());
            std::process::exit(exit::USAGE);
        }
    };
    if parsed.help {
        print!("{}", cli::usage());
        std::process::exit(0);
    }

    // Stage children lead process groups of their own, so the terminal's
    // Ctrl-C no longer reaches them: the gate forwards it, and SIGTERM/SIGHUP,
    // to every group still running before it dies (`exec::group`).
    #[cfg(unix)]
    exec::group::kill_on_interrupt();

    // WHERE THIS RUN'S OWN OUTPUT GOES, before anything reads the tree. A log
    // redirected into the checkout is an untracked file that GROWS for the
    // length of the run, and an untracked file is part of the source identity
    // by design — so without this the gate reads its own ladder as the tree
    // moving and decides nothing. See `identity::claim_own_output`.
    identity::claim_own_output();

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

    // Timings are a side channel: a file that cannot be opened is said on
    // stderr and costs the TSV, never the run.
    let timings = env.verify_timings.as_deref().and_then(|p| {
        exec::Timings::create(p)
            .map_err(|e| {
                eprintln!(
                    "verify: cannot open ATERM_VERIFY_TIMINGS={}: {e}",
                    p.display()
                )
            })
            .ok()
    });

    // What the claim above actually excluded, SAID rather than assumed: a run
    // whose ladder is being written into the tree it verifies should read that
    // on the ladder, not discover it in this source file.
    let excluded = identity::untracked_split(&root, &env.path)
        .map(|(_, own)| own)
        .unwrap_or_default();

    // ONE GATE PER MACHINE, before the source is chosen: a second gate waits for
    // the running one instead of running beside it (`snapshot::hold_machine` —
    // two gates at once poison each other's evidence). The self-test verifies
    // the gate itself and stays unserialized. Held until the process exits.
    // A gate started BY the holding gate (a stage driving this binary) runs
    // inside that hold instead of queueing on its own ancestor until the stage
    // ceiling kills the stage.
    let _machine = if parsed.selftest || snapshot::inside_machine_holder() {
        None
    } else {
        match snapshot::hold_machine(&root) {
            Ok(hold) => Some(hold),
            Err(why) => {
                print!("{}", snapshot::machine_could_not_run_text(&why));
                std::fs::remove_dir_all(&scratch).ok();
                std::process::exit(exit::COULD_NOT_RUN);
            }
        }
    };

    // THE SNAPSHOT, before anything reads the tree — `--changed` included, so
    // its selection is of the same tree the stages build.
    let (snap, notes) = match choose_source(&parsed, &root, &env, &scratch) {
        Ok(chosen) => chosen,
        Err(why) => {
            print!("{}", snapshot::could_not_run_text(&why));
            std::fs::remove_dir_all(&scratch).ok();
            std::process::exit(exit::COULD_NOT_RUN);
        }
    };
    let run_root = snap
        .as_ref()
        .map_or_else(|| root.clone(), |s| s.root.clone());

    // `--changed` decides the scope BEFORE the ladder is planned, so it runs
    // here rather than as a stage: every header below names the scope it picks.
    let (scope, prelude) = resolve_scope(&parsed, &run_root, &env);

    // THE GATE KEEPS ITS OWN COPY OF THE LADDER, so nobody has to `| tee` one
    // into the checkout (the failure above) to have a record afterwards. It
    // lives in the gate's state directory, which no `TreeState` reads and
    // `.gitignore` already covers, and `ATERM_VERIFY_LOG=<path>` moves it —
    // empty turns it off.
    let log = open_log(&env, &run_root);

    let mut ctx = Ctx::new(
        run_root,
        parsed.mode,
        scope,
        parsed.selftest,
        env,
        scratch.clone(),
    )
    .with_prelude(prelude)
    .with_timings(timings)
    .with_progress_log(log.as_ref().and_then(|(_, f)| f.try_clone().ok()))
    .with_notes(
        identity::own_output_note(&excluded)
            .into_iter()
            .chain(notes)
            .collect::<Vec<_>>(),
    );
    if let Some(s) = &snap {
        ctx = ctx.in_snapshot_of(s.caller.clone(), s.tree.clone(), s.notes.clone());
    }
    // AFTER the snapshot is chosen, because the git stamp is resolved from the
    // root this run will actually build — and BEFORE any stage runs, because the
    // whole point is that every child of one run is given the same answer.
    ctx = ctx.with_pinned_child_facts();
    // Every child learns which gate holds the machine, so a gate a stage
    // starts is recognised as part of this run (`snapshot::inside_machine_holder`).
    if _machine.is_some() {
        ctx.child_env_add.push((
            snapshot::MACHINE_HOLDER_ENV.into(),
            std::process::id().to_string().into(),
        ));
    }
    if let Some(gib) = parsed.disk_floor_gib {
        ctx = ctx.with_disk_floor(gib * aterm_verify::disk::GIB);
    }

    let started = Instant::now();
    let stdout = std::io::stdout();
    let mut out = Tee {
        out: std::io::BufWriter::new(stdout.lock()),
        log: log.as_ref().map(|(_, f)| f),
    };
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
    if let Some(s) = snap {
        s.finish();
    }
    // The kernel releases the machine lock when this process ends, however it
    // ends; release it now anyway, so a waiting gate starts while this one is
    // still printing its last lines.
    drop(_machine);

    if std::io::stderr().is_terminal() {
        let secs = started.elapsed().as_secs_f64();
        if let Some((path, _)) = &log {
            let _ = writeln!(std::io::stderr(), "verify: log {}", path.display());
        }
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
    let tools = Toolchain::discover(
        env.trust_stage2_bin.as_deref(),
        &env.home,
        &env.path,
        aterm_verify::toolchain::pinned_channel(root).as_deref(),
    );
    let path_env = tools.path_with_stage2_first(&env.path);
    let selection = changed::resolve(root, &tools, &path_env, &base);
    let (scope, report) = changed::stage_report(&base, &selection);
    (scope, Some(report))
}

/// Where this run's stages execute: a prepared snapshot (the default), or the
/// caller's checkout — for `--in-place`, for `--selftest` (it builds nothing, so
/// there is nothing to protect), and for a root that is not a git checkout at
/// all, which has no HEAD to pin and says so in the header. A root that holds a
/// `.git` git cannot open is neither: it is an `Err`.
///
/// # Errors
/// Why a snapshot that should have been had could not be.
fn choose_source(
    parsed: &cli::Args,
    root: &Path,
    env: &EnvSnapshot,
    scratch: &Path,
) -> Result<(Option<snapshot::Snapshot>, Vec<String>), String> {
    if parsed.in_place || parsed.selftest {
        return Ok((None, Vec::new()));
    }
    if !identity::is_git_toplevel(root, &env.path) {
        // A `.git` git cannot open is a checkout the run cannot pin, not a root
        // without one: falling back in place would run with no source tripwire.
        if identity::has_git_entry(root) {
            return Err(identity::unopenable_reason(root));
        }
        return Ok((
            None,
            vec![format!(
                "verify: {} is not a git checkout, so there is no HEAD to snapshot — this run is IN PLACE",
                root.display()
            )],
        ));
    }
    // The compiler's commit, for the lane stamps: the same discovery `Ctx::new`
    // makes, so the stamp names the compiler the stages will run.
    let prefix = aterm_verify::toolchain::atpkg_prefix(&env.home, env.xdg_config_home.as_deref());
    let tools = Toolchain::discover_with_store(
        env.trust_stage2_bin.as_deref(),
        &env.home,
        Some(&prefix),
        &env.path,
        aterm_verify::toolchain::pinned_channel(root).as_deref(),
    );
    let path_env = tools.path_with_stage2_first(&env.path);
    let snap = snapshot::prepare(&snapshot::Options {
        caller: root,
        snapshot: env
            .verify_snapshot
            .clone()
            .unwrap_or_else(|| snapshot::default_root(root)),
        path_env: &path_env,
        lane_env: snapshot::lane_env_from_process(),
        trustc_commit: tools.identity(&path_env, scratch).commit,
    })?;
    Ok((Some(snap), Vec::new()))
}

/// stdout, and the gate's own copy of the ladder.
///
/// A log write NEVER decides anything: a full disk costs the record, not the
/// run, exactly as `ATERM_VERIFY_TIMINGS` does. `write` reports what reached
/// STDOUT, so a short write on the log cannot be mistaken for a short write on
/// the ladder.
struct Tee<'a, W: Write> {
    out: W,
    log: Option<&'a std::fs::File>,
}

impl<W: Write> Write for Tee<'_, W> {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        let n = self.out.write(buf)?;
        if let Some(f) = &mut self.log {
            let _ = f.write_all(&buf[..n]);
        }
        Ok(n)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        if let Some(f) = &mut self.log {
            let _ = f.flush();
        }
        self.out.flush()
    }
}

/// How many of the gate's own logs are kept. A record worth writing is worth
/// not filling a disk with: the newest few runs are what anyone reads.
const LOGS_KEPT: usize = 20;

/// Open this run's log — `ATERM_VERIFY_LOG` when set (empty turns it off),
/// otherwise `<run root>/.aterm-verify/logs/verify-<pid>.log`.
///
/// Inside the gate's state directory ON PURPOSE: `identity::is_gate_state`
/// keeps that path out of every `TreeState`, and `.gitignore` keeps it out of
/// `git status`, so the gate writing a log cannot become the gate watching its
/// own log move. A path that cannot be opened is said once on stderr and costs
/// the record, never the run.
fn open_log(env: &EnvSnapshot, run_root: &Path) -> Option<(PathBuf, std::fs::File)> {
    let path = match &env.verify_log {
        Some(p) if p.as_os_str().is_empty() => return None,
        Some(p) => p.clone(),
        None => {
            let dir = run_root
                .join(identity::GATE_STATE_DIR)
                .join(aterm_verify::LOG_DIR);
            prune_logs(&dir);
            dir.join(format!("verify-{}.log", std::process::id()))
        }
    };
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    // Truncated, then reopened APPENDING: the ladder's copy and every stage's
    // finish line (written from the stage threads as they end) share this
    // file, and an appending write lands whole at the end.
    match std::fs::File::create(&path)
        .and_then(|_| std::fs::OpenOptions::new().append(true).open(&path))
    {
        Ok(f) => Some((path, f)),
        Err(e) => {
            eprintln!("verify: cannot write the run log {}: {e}", path.display());
            None
        }
    }
}

/// Keep the newest [`LOGS_KEPT`] logs in `dir`. Best effort throughout: this is
/// housekeeping for a side channel and may never fail a run.
fn prune_logs(dir: &Path) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    let mut logs: Vec<(std::time::SystemTime, PathBuf)> = entries
        .flatten()
        .filter(|e| e.file_type().is_ok_and(|t| t.is_file()))
        .filter_map(|e| {
            let t = e.metadata().ok()?.modified().ok()?;
            Some((t, e.path()))
        })
        .collect();
    if logs.len() < LOGS_KEPT {
        return;
    }
    logs.sort_unstable();
    let drop_n = logs.len() + 1 - LOGS_KEPT;
    for (_, p) in logs.into_iter().take(drop_n) {
        let _ = std::fs::remove_file(p);
    }
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
