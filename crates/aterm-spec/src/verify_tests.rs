// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

use super::is_pending_stub;

/// THE CLASSIFIER THE LOCK'S REMOVAL RESTS ON: a `ty check` its memory
/// budget stops is an `Err` from [`super::ty_output`], never an `Output`.
///
/// The stop is provoked, not waited for: 1 MB is below the footprint of
/// any `ty` process (it reports 7 MB at its first budget poll), so the
/// walk stops there on any machine. Store `ty` 0.13.0, armed, stops
/// `NativeDraftJournal` (43,770 reachable states) at 2,039 of them with
/// EXIT 0 and a `Model checking stopped:` line, six runs of six — the
/// shape a budget shrunk by concurrent processes takes.
/// A `ty` that exits 5 for it takes the other arm of the same guard.
#[test]
fn a_stopped_search_is_an_err_never_a_pass() {
    let Some(ty) = super::ty_escalation("a_stopped_search_is_an_err_never_a_pass") else {
        return;
    };
    let m = crate::derive::native_draft_journal_model();
    let dir = super::scratch_dir("aterm-stopped", m.name);
    std::fs::create_dir_all(&dir).expect("mk tempdir");
    let spec = dir.join(format!("{}.tla", m.name));
    let cfg = dir.join(format!("{}.cfg", m.name));
    std::fs::write(&spec, m.to_tla()).expect("write spec");
    std::fs::write(&cfg, m.to_cfg()).expect("write cfg");
    let mut cmd = std::process::Command::new(&ty);
    cmd.arg("check").arg(&spec).arg("--config").arg(&cfg);
    super::arm_whole_space_check(&mut cmd)
        .arg("--memory-limit")
        .arg("1");
    let verdict = super::ty_output(&mut cmd);
    let _ = std::fs::remove_dir_all(&dir);
    let err = match verdict {
        Ok(out) => panic!(
            "a `ty check` stopped by `--memory-limit 1` came back as an Output ({:?}) — \
             a stopped search would be read as a verdict\n{}{}",
            out.status,
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        ),
        Err(e) => e.to_string(),
    };
    assert!(
        err.starts_with("ty INCOMPLETE:"),
        "the stopped search must fail as INCOMPLETE, not as a spawn error: {err}"
    );
}

/// The per-invariant form [`super::uncaught_invariants`] computed before it
/// became one walk: an isolated `bmc` per invariant, `Buggy = 1`. Kept as
/// the oracle the one-walk form is held to.
fn uncaught_invariants_one_walk_per_invariant(m: &super::Model) -> Vec<&'static str> {
    let buggy = super::interp::with_buggy(m, 1);
    m.invariants
        .iter()
        .filter(|inv| {
            let mut alone = buggy.clone();
            alone.invariants.retain(|other| other.name == inv.name);
            super::interp::bmc(&alone).is_ok()
        })
        .map(|inv| inv.name)
        .collect()
}

/// The one-walk ghost sweep returns EXACTLY what one isolated walk per
/// invariant returned — the same set in the same order, and a panic
/// exactly where the per-invariant form panicked (a function-valued model).
///
/// THE SAMPLE IS CHOSEN BY OUTCOME, NOT BY NAME. The registry is read in
/// order and each model is filed by what the one-walk sweep says of it —
/// carries an uncaught invariant, fully caught, or uninterpretable (it
/// panics) — and the first `PER_OUTCOME` of each kind are held to the
/// oracle. Paying a ghost down, or teaching the interpreter a model, moves
/// that model to another kind and leaves this test as strong as it was;
/// the workspace ratchet (`non_vacuity_ratchet`) still checks the whole
/// registry's census.
///
/// Two CONTROLS are built from the first fully-caught model, so every
/// outcome is compared whatever the registry's census is: the model plus
/// a tautology (one ghost, by construction, alongside invariants that are
/// caught — the live set never empties and the walk runs to the end of the
/// space), and the model plus a function-valued invariant (both forms must
/// panic).
#[test]
fn the_one_walk_ghost_sweep_matches_the_per_invariant_oracle() {
    use std::panic::{AssertUnwindSafe, catch_unwind};

    use crate::derive::{Expr, Invariant};

    /// Models of each outcome held to the oracle. The oracle costs one walk
    /// per invariant — the cost the one-walk form exists to remove — so the
    /// sample is a few of each kind, not the registry.
    const PER_OUTCOME: usize = 3;

    /// Hold one model to the oracle; `Some(uncaught)` if it is
    /// interpretable, `None` if both forms panicked.
    fn compare(m: &super::Model) -> Option<Vec<&'static str>> {
        let oracle = catch_unwind(AssertUnwindSafe(|| {
            uncaught_invariants_one_walk_per_invariant(m)
        }));
        let one_walk = catch_unwind(AssertUnwindSafe(|| super::uncaught_invariants(m)));
        match (oracle, one_walk) {
            (Ok(a), Ok(b)) => {
                assert_eq!(a, b, "{}: one-walk sweep disagrees with the oracle", m.name);
                Some(a)
            }
            (Err(_), Err(_)) => None,
            (a, b) => panic!(
                "{}: oracle {} but one-walk sweep {}",
                m.name,
                if a.is_ok() { "returned" } else { "panicked" },
                if b.is_ok() { "returned" } else { "panicked" }
            ),
        }
    }

    let registry = crate::xref::model_registry();
    let (mut with_ghosts, mut fully_caught, mut panicked) = (Vec::new(), Vec::new(), Vec::new());
    for m in &registry {
        if [&with_ghosts, &fully_caught, &panicked]
            .iter()
            .all(|kind| kind.len() >= PER_OUTCOME)
        {
            break;
        }
        let kind = match catch_unwind(AssertUnwindSafe(|| super::uncaught_invariants(m))) {
            Ok(names) if names.is_empty() => &mut fully_caught,
            Ok(_) => &mut with_ghosts,
            Err(_) => &mut panicked,
        };
        if kind.len() < PER_OUTCOME {
            kind.push(m);
        }
    }
    for m in with_ghosts.iter().chain(&fully_caught).chain(&panicked) {
        compare(m);
    }

    let base = fully_caught
        .first()
        .expect("no registry model is fully caught — the ratchet's census is broken");
    let control = |name: &'static str, expr: Expr| {
        assert!(
            base.invariants.iter().all(|inv| inv.name != name),
            "{}: already has an invariant named {name}",
            base.name
        );
        let mut m = (*base).clone();
        m.invariants.push(Invariant { name, expr });
        m
    };
    assert_eq!(
        compare(&control("ControlTautology", Expr::Bool(true))),
        Some(vec!["ControlTautology"]),
        "{} plus a tautology must carry exactly that one ghost",
        base.name
    );
    assert_eq!(
        compare(&control(
            "ControlFunctionValued",
            Expr::FnAccess("control", Box::new(Expr::Int(1)))
        )),
        None,
        "{} plus a function-valued invariant must be uninterpretable",
        base.name
    );
}

/// A model with no invariants has no ghosts, and costs no walk: an
/// unexplorable action set is never touched.
#[test]
fn a_model_without_invariants_has_no_ghosts_and_is_not_walked() {
    let mut m = crate::derive::ring_model();
    m.invariants.clear();
    // Duplicate an action: any walk would trip the uniqueness assert.
    let dup = m.actions[0].clone();
    m.actions.push(dup);
    assert!(super::uncaught_invariants(&m).is_empty());
}

/// A pending-program stub on PATH is NOT the tool: discovery must skip it,
/// or a harness runs the placeholder and reads its refusal as tool output
/// (the spec-xref gate hard-failed exactly that way once a pending
/// `trust-ir` stub reached PATH).
#[test]
fn a_pending_stub_is_not_the_tool() {
    let dir = std::env::temp_dir().join(format!("aterm_stub_probe_{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("mk probe dir");
    let stub = dir.join("trust-ir-probe");
    std::fs::write(
        &stub,
        "#!/bin/sh\n# atpkg pending-program stub v1\n# Replaced by the real shim when the program installs.\nexit 127\n",
    )
    .expect("write stub");
    assert!(is_pending_stub(&stub), "the stub names itself");

    let real = dir.join("trust-ir-real");
    std::fs::write(&real, b"\x7fELF fake binary bytes").expect("write real");
    assert!(!is_pending_stub(&real), "a real binary is not a stub");

    let missing = dir.join("nope");
    assert!(
        !is_pending_stub(&missing),
        "unreadable candidates stay eligible — a false pending would hide an installed tool"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// The stub check reads the HEAD of a candidate and nothing more: a real
/// tool is ~160 MB, and every store copy of it is checked. Proved with a
/// FIFO whose writer sends a stub's head and then holds the pipe OPEN — a
/// whole-file read never sees EOF and blocks, a bounded one answers.
#[cfg(unix)]
#[test]
fn the_pending_stub_check_reads_only_the_head() {
    let dir = std::env::temp_dir().join(format!("aterm_stub_head_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("mk probe dir");
    let fifo = dir.join("endless");
    let made = std::process::Command::new("/usr/bin/mkfifo")
        .arg(&fifo)
        .status()
        .expect("run mkfifo");
    assert!(made.success(), "mkfifo");
    let (release_tx, release_rx) = std::sync::mpsc::channel::<()>();
    let writer_path = fifo.clone();
    let writer = std::thread::spawn(move || {
        use std::io::Write as _;
        let mut w = std::fs::OpenOptions::new()
            .write(true)
            .open(&writer_path)
            .expect("open fifo for write");
        let mut head = b"#!/bin/sh\n# atpkg pending-program stub v1\n".to_vec();
        // Longer than the bound, so a bounded read stops inside it.
        head.resize(320, b'#');
        assert!(head.len() as u64 > super::PENDING_STUB_HEAD);
        let _ = w.write_all(&head);
        // Hold the pipe open: no EOF until the check has answered. The
        // cap outlasts the answer's wait below, or a whole-file read
        // would see EOF inside it and pass.
        let _ = release_rx.recv_timeout(std::time::Duration::from_secs(120));
    });
    let (tx, rx) = std::sync::mpsc::channel();
    let reader_path = fifo.clone();
    std::thread::spawn(move || {
        let _ = tx.send(is_pending_stub(&reader_path));
    });
    // A hang detector: the bounded read answers at once; a whole-file one
    // never does while the writer holds the pipe.
    let answer = rx.recv_timeout(std::time::Duration::from_secs(60));
    let _ = release_tx.send(());
    let _ = writer.join();
    let _ = std::fs::remove_dir_all(&dir);
    assert_eq!(
        answer,
        Ok(true),
        "the check must answer from the head while the writer holds the pipe open — \
         a whole-file read blocks here waiting for an EOF that never comes"
    );
}

use super::*;
use crate::derive::{config_catalog_snapshot_model, ring_model, transact_model};
use crate::ty_model;

/// The arming is a LIST, and every entry on it is load-bearing for a
/// different reason — so the list is pinned exactly, not merely "contains".
///
/// `ty_drivers_are_armed` proves every driver CALLS this function. Nothing
/// proved what the function then emits, which is the half that actually
/// arms anything. An `assert!(contains)` per flag would let the list
/// be quietly reordered or extended; an exact match makes any edit to the
/// arming a deliberate, reviewed act.
///
/// It is also this file's REACH GUARD for the fast lane. `--bfs-only` is
/// what makes each spawn 0.03s instead of 0.58s, and it is invisible
/// everywhere else: drop it and every verdict in the workspace stays
/// green while `tools/verify.sh` silently grows ~3.5 minutes back. The
/// only place that regression can be caught is here.
#[test]
fn the_arming_emits_exactly_the_flags_it_documents() {
    let mut cmd = Command::new("ty");
    arm_whole_space_check(&mut cmd);
    let args: Vec<String> = cmd
        .get_args()
        .map(|a| a.to_string_lossy().into_owned())
        .collect();
    assert_eq!(
        args,
        [
            // Reduction OFF, both halves — the whole-space obligation, and
            // the only reason `assert_same_space_explored` can be written.
            "--no-auto-por",
            "--no-auto-symmetry",
            // Footprint hint: 4x the largest model, so it bounds nothing.
            "--initial-capacity",
            "8192",
            // The oracle engine, not the native codegen lane.
            "--backend",
            "interpreter",
            // The fused CDEMC symbolic lane OFF: 0.55s per spawn to report
            // `Winner: BFS` on every model in the tree. Removing this is a
            // ~3.5 minute regression on every `tools/verify.sh` run.
            "--bfs-only",
            // One worker: these spaces are small, and the worker pool's
            // start-up cost more than it saved (559 replayed checks, 57.1 s
            // -> 40.1 s, the same verdicts on all of them).
            "--workers",
            "1",
            // No cached verdicts. The cache is path-keyed, and the
            // hand-written ISOLATION specs live at a stable path, so
            // without this a repeat run replays a security PASS it never
            // re-derived.
            "--force",
        ],
        "the `ty` arming changed — every flag here is load-bearing; see the \
         doc on `arm_whole_space_check` before editing this list"
    );
}

/// The atpkg-store prefix mirror must keep matching
/// `atpkg::platform::default_prefix` + `/bin` (the deliberate
/// no-dependency duplication both sites cross-reference).
#[test]
fn store_bin_dir_mirrors_the_atpkg_default_prefix() {
    #[cfg(target_os = "macos")]
    assert_eq!(
        unix_store_bin_dir(Path::new("/Users//x")),
        Path::new("/Users//x/Library/Application Support/aterm/pkg/bin")
    );
    #[cfg(not(target_os = "macos"))]
    assert_eq!(
        unix_store_bin_dir(Path::new("/home/x")),
        Path::new("/home/x/.local/share/aterm/pkg/bin")
    );
}

/// Discovery asks THIS ACCOUNT's store whatever `$HOME` says: a test
/// process moves its `$HOME` (aterm-gui's handoff fixtures), and every
/// thread must still find the store the account installed
/// ([`home_dirs`]). Shown in a child of this test binary whose `HOME`
/// names an empty directory, so this process's environment is never
/// touched: the child must name the same store `bin/` the account's home
/// gives.
#[cfg(unix)]
#[test]
fn the_store_is_the_accounts_whatever_home_says() {
    const CHILD: &str = "ATERM_SPEC_STORE_HOME_CHILD";
    if std::env::var_os(CHILD).is_some() {
        let dir = atpkg_store_bin_dir().map(|d| d.display().to_string());
        println!("store-bin=[{}]", dir.unwrap_or_default());
        return;
    }
    let account = account_home().expect("getpwuid_r resolves this account's home");
    let moved = aterm_tempfile::TempDir::new().expect("scratch HOME");
    let out = std::process::Command::new(std::env::current_exe().expect("test exe"))
        .args([
            "--exact",
            "verify::tests::the_store_is_the_accounts_whatever_home_says",
            "--nocapture",
            "--test-threads",
            "1",
        ])
        .env(CHILD, "1")
        .env("HOME", moved.path())
        .stdin(std::process::Stdio::null())
        .output()
        .expect("run the child");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(out.status.success(), "the child failed: {stdout}");
    let want = format!("store-bin=[{}]", unix_store_bin_dir(&account).display());
    assert!(
        // Not a whole line: libtest's `test … ...` shares the child's.
        stdout.contains(&want),
        "with HOME={} the store must still be the account's ({want}):\n{stdout}",
        moved.path().display()
    );
}

/// A dangling store shim never satisfies discovery; a live one — a symlink an
/// older atpkg laid, or today's exec stub — resolves to the real store target
/// (what the caller will execute), not the shim.
#[cfg(unix)]
#[test]
fn store_shim_resolution_is_fail_closed() {
    let d = std::env::temp_dir().join(format!("aterm-spec-shim-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    let shim = d.join("ty");
    // Absent → None.
    assert_eq!(resolve_store_shim(&shim), None);
    // Dangling symlink → None (a GC'd store build must not satisfy).
    std::os::unix::fs::symlink(d.join("store/gone/ty"), &shim).unwrap();
    assert_eq!(resolve_store_shim(&shim), None);
    // A TOMBSTONE (regular-file refusal script atpkg writes for a yanked
    // build) must read as ABSENT — else a revoked build would shadow a
    // working tool on PATH, since this probe runs first.
    std::fs::remove_file(&shim).unwrap();
    std::fs::write(&shim, b"#!/bin/sh\necho revoked >&2\nexit 1\n").unwrap();
    assert_eq!(
        resolve_store_shim(&shim),
        None,
        "a tombstone shim must never satisfy discovery"
    );
    // Live shim → the RESOLVED store path.
    std::fs::remove_file(&shim).unwrap();
    let target = d.join("real-ty");
    std::fs::write(&target, b"#!/bin/sh\n").unwrap();
    std::os::unix::fs::symlink(&target, &shim).unwrap();
    let got = resolve_store_shim(&shim).expect("live shim resolves");
    assert_eq!(got, std::fs::canonicalize(&target).unwrap());
    // THE SHAPE ATPKG LAYS TODAY: a `#!/bin/sh` exec stub, never a symlink. Its first
    // `exec '<path>' "$@"` line is the target — including a path with a quote in it.
    std::fs::remove_file(&shim).unwrap();
    let quoted = d.join("store/it's/ty");
    std::fs::create_dir_all(quoted.parent().unwrap()).unwrap();
    std::fs::write(&quoted, b"#!/bin/sh\n").unwrap();
    let stub = |target: &str| {
        let _ = std::fs::remove_file(&shim);
        std::fs::write(
            &shim,
            format!(
                "#!/bin/sh\n# atpkg shim — exec so the tool authenticates at its real path.\n\
                 exec '{target}' \"$@\"\n"
            ),
        )
        .unwrap();
    };
    stub(&quoted.to_string_lossy().replace('\'', "'\\''"));
    assert_eq!(
        resolve_store_shim(&shim),
        Some(std::fs::canonicalize(&quoted).unwrap()),
        "an exec stub resolves to the file it execs"
    );
    // A stub whose build was reclaimed never satisfies discovery, and neither does one
    // naming a RELATIVE path — which would resolve against the cwd: `Cargo.toml` is a
    // real file relative to a `targo test` run's cwd, so only the absolute-path rule
    // refuses it.
    stub(&d.join("store/gone/ty").to_string_lossy());
    assert_eq!(
        resolve_store_shim(&shim),
        None,
        "a reclaimed build is absent"
    );
    assert!(
        Path::new("Cargo.toml").is_file(),
        "fixture: the relative file exists"
    );
    stub("Cargo.toml");
    assert_eq!(
        resolve_store_shim(&shim),
        None,
        "a relative target is absent"
    );
    // atpkg's pending-program stub execs `"$ATPKG"`, never a quoted literal path.
    std::fs::remove_file(&shim).unwrap();
    std::fs::write(
        &shim,
        b"#!/bin/sh\n# atpkg pending-program stub v1\nATPKG='/a/atpkg'\n\
          if [ -x \"$ATPKG\" ]; then\n  exec \"$ATPKG\" __pending 'ty' \"$@\"\nfi\nexit 127\n",
    )
    .unwrap();
    assert_eq!(resolve_store_shim(&shim), None, "a pending stub is absent");
    let _ = std::fs::remove_dir_all(&d);
}

// -----------------------------------------------------------------------
// PROVENANCE — the escalation tier names the binary it ran. Every test
// below runs over a TEMP home + store prefix through the explicit-env
// resolver, so nothing reads or mutates the process environment and no
// test depends on what this machine has installed.
// -----------------------------------------------------------------------

/// A scratch HOME and atpkg prefix for one test.
struct Scratch {
    root: aterm_tempfile::TempDir,
}

impl Scratch {
    fn new(tag: &str) -> Self {
        // Canonical, so `~` shortening matches the paths discovery returns
        // (macOS temp_dir is itself behind the /var -> /private/var link).
        let base = std::fs::canonicalize(std::env::temp_dir()).unwrap();
        let root = aterm_tempfile::Builder::new()
            .prefix(&format!("aterm-spec-prov-{tag}-"))
            .tempdir_in(base)
            .unwrap();
        std::fs::create_dir_all(root.path().join("home")).unwrap();
        std::fs::create_dir_all(root.path().join("prefix/bin")).unwrap();
        Self { root }
    }
    fn home(&self) -> PathBuf {
        self.root.path().join("home")
    }
    fn store_bin(&self) -> PathBuf {
        self.root.path().join("prefix/bin")
    }
    fn file(&self, rel: &str) -> PathBuf {
        let p = self.root.path().join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(&p, b"\x7fELF not really\n").unwrap();
        p
    }
    /// `prefix/store/<program>/<build>/bin/<exe>` + `current -> <build>`.
    #[cfg(unix)]
    fn store_copy(&self, program: &str, build: &str, exe: &str) -> PathBuf {
        let p = self.file(&format!("prefix/store/{program}/{build}/bin/{exe}"));
        let dir = self.root.path().join(format!("prefix/store/{program}"));
        std::os::unix::fs::symlink(dir.join(build), dir.join("current")).unwrap();
        p
    }
    fn locate(&self, bin: &str, path_var: Option<&std::ffi::OsStr>) -> Option<Located> {
        find_trust_bin(bin, Some(&self.store_bin()), path_var)
    }
    fn line(&self, bin: &str, found: Option<&Located>) -> String {
        let identity = found.and_then(|f| binary_identity(&f.path));
        provenance_line(
            bin,
            found,
            identity.as_ref(),
            Some("ty 9.9.9"),
            &[self.home()],
            Some(&self.store_bin()),
        )
    }
}

/// Store PRESENT, PATH empty. With no usable `ty` shim (a store can lose its
/// own-name `ty`/`ay` shims to a trust rollback) nothing is found, and the line names the
/// store copy and WHY discovery missed it instead of implying the tool is
/// not installed. With a live shim — the exec stub, or a symlink — the store
/// tier answers and the line says so.
#[cfg(unix)]
#[test]
fn provenance_store_present_names_the_unreached_copy() {
    let s = Scratch::new("nobs-st");
    s.store_copy("ty", "3007", "ty");
    let shim = s.store_bin().join("ty");
    let unreached = |why: &str| {
        assert_eq!(s.locate("ty", None), None, "{why}");
        let line = s.line("ty", None);
        assert!(
            line.starts_with("VERIFY ESCALATION TIER: ty = NOT FOUND"),
            "{line}"
        );
        assert!(
            line.contains("the managed store holds store/ty/3007/bin/ty (built 2")
                && line.ends_with(&format!("but discovery does not reach it: {why}")),
            "{line}"
        );
    };
    unreached("no `ty` shim in the store's bin/ exposes it");

    std::fs::write(&shim, "#!/bin/sh\necho revoked >&2\nexit 1\n").unwrap();
    unreached("the store's `ty` shim is a refusal script (tombstone or pending stub)");

    let stub = |target: &Path| {
        std::fs::write(
            &shim,
            format!(
                "#!/bin/sh\n# atpkg shim — exec so the tool authenticates at its real path.\n\
                 exec '{}' \"$@\"\n",
                target.display()
            ),
        )
        .unwrap();
    };
    stub(&s.root.path().join("prefix/store/ty/2999/bin/ty"));
    unreached("the store's `ty` shim does not resolve to a file (a reclaimed build?)");

    // atpkg's CURRENT shim form, live: the store tier answers, and the copy
    // it resolves to is not listed as shadowing itself.
    let target = s.root.path().join("prefix/store/ty/3007/bin/ty");
    stub(&target);
    let found = s
        .locate("ty", None)
        .expect("a live exec stub is the store tier");
    assert_eq!(
        (found.origin, found.path.clone()),
        (TrustBinOrigin::Store, target.clone())
    );
    let line = s.line("ty", Some(&found));
    assert!(
        line.starts_with(&format!(
            "VERIFY ESCALATION TIER: ty = {} [atpkg managed store; ty 9.9.9; built 2",
            target.display()
        )),
        "{line}"
    );
    assert!(
        !line.contains(" — "),
        "the chosen copy is not its own shadow: {line}"
    );

    // An older atpkg's symlink shim is the store tier too.
    std::fs::remove_file(&shim).unwrap();
    std::os::unix::fs::symlink(&target, &shim).unwrap();
    let found = s
        .locate("ty", None)
        .expect("a symlink shim is the store tier");
    assert_eq!(found.origin, TrustBinOrigin::Store);
}

/// Nothing installed: not found, and the plain notice with no store clause.
#[test]
fn provenance_nothing_installed_is_a_plain_not_found() {
    let s = Scratch::new("none");
    assert_eq!(s.locate("ty", None), None);
    assert_eq!(
        s.line("ty", None),
        "VERIFY ESCALATION TIER: ty = NOT FOUND (probed the atpkg store shim, PATH)"
    );
}

/// THE order, pinned: the store shim beats PATH, and a developer tree under home
/// — a first-party cargo build, a bootstrap stage, a standalone `~/ay` — is never
/// a candidate (a stale one would shadow the store). Another copy in the store
/// is named beside the chosen one.
#[cfg(unix)]
#[test]
fn discovery_order_is_store_then_path_and_no_developer_tree() {
    let s = Scratch::new("order");
    s.file("home/trust/first-party/trust-ir/target/release/trust-ir");
    s.file("home/trust/build/host/stage1/bin/trust-ir");
    s.file("home/ay/target/release/ay");
    assert_eq!(
        s.locate("trust-ir", None),
        None,
        "no developer tree is probed"
    );
    assert_eq!(s.locate("ay", None), None, "no standalone ~/ay either");

    let on_path = s.file("pathdir/trust-ir");
    let path_var = std::ffi::OsString::from(on_path.parent().unwrap());
    let got = s.locate("trust-ir", Some(&path_var)).unwrap();
    assert_eq!((got.origin, got.path), (TrustBinOrigin::Path, on_path));

    let store = s.store_copy("trust-ir", "1123", "trust-ir");
    s.store_copy("trust", "9192", "trust-ir");
    std::os::unix::fs::symlink(&store, s.store_bin().join("trust-ir")).unwrap();
    let got = s.locate("trust-ir", Some(&path_var)).unwrap();
    assert_eq!(
        (got.origin, got.path.clone()),
        (TrustBinOrigin::Store, store)
    );
    let line = s.line("trust-ir", Some(&got));
    assert!(
        line.contains("[atpkg managed store; ")
            && line.contains(" — the managed store also holds store/trust/9192/bin/trust-ir"),
        "{line}"
    );
}

/// A PATH hit that is an atpkg exec stub (a shim dir on PATH other than
/// this prefix's `bin/`) names the store file it execs, and that store copy
/// is not listed as shadowed (it is the one that ran).
#[cfg(unix)]
#[test]
fn a_path_exec_stub_names_its_target() {
    let s = Scratch::new("pathstub");
    let target = s.store_copy("trust-ir", "1123", "trust-ir");
    let shim = s.root.path().join("pathbin/trust-ir");
    std::fs::create_dir_all(shim.parent().unwrap()).unwrap();
    std::fs::write(
        &shim,
        format!(
            "#!/bin/sh\n# atpkg shim\nexec '{}' \"$@\"\n",
            target.display()
        ),
    )
    .unwrap();
    let path_var = std::ffi::OsString::from(shim.parent().unwrap());
    let found = s.locate("trust-ir", Some(&path_var)).unwrap();
    assert_eq!(found.origin, TrustBinOrigin::Path);
    let line = s.line("trust-ir", Some(&found));
    let want = format!("[PATH (atpkg shim → {}); ", target.display());
    assert!(line.contains(&want), "{line}");
    assert!(!line.contains(" — "), "{line}");
}

/// The exec-stub reader mirrors `atpkg::platform::parse_sh_shim_target`.
#[test]
fn exec_stub_target_reads_atpkg_shims_and_rejects_refusals() {
    let s = Scratch::new("stubparse");
    let shim = s.root.path().join("shim");
    std::fs::write(
        &shim,
        "#!/bin/sh\n# atpkg shim — exec so the tool authenticates at its real path.\nexec '/a/it'\\''s/ty' \"$@\"\n",
    )
    .unwrap();
    assert_eq!(exec_stub_target(&shim), Some(PathBuf::from("/a/it's/ty")));
    std::fs::write(&shim, "#!/bin/sh\necho revoked >&2\nexit 1\n").unwrap();
    assert_eq!(exec_stub_target(&shim), None, "a tombstone has no target");
    assert_eq!(classify_store_shim(&shim), StoreShim::Refusal);
    assert_eq!(
        classify_store_shim(&s.root.path().join("nope")),
        StoreShim::Absent
    );
}

/// A line is claimed the first time a tool is discovered and again only when
/// what it resolves to changes: never for a repeat of the last line (the
/// hundreds of `find_ty` calls in a suite), always for a change — including
/// one that only the binary's identity sees, and a change undone (the last
/// line then no longer names what runs).
#[test]
fn provenance_is_claimed_again_only_when_the_resolution_changes() {
    let at = |p: &str, len: Option<u64>| -> ProvenanceKey {
        let path = PathBuf::from(p);
        let identity = len.map(|len| BinaryIdentity {
            links: vec![ChainLink {
                path: path.clone(),
                file: FileIdentity {
                    len,
                    modified: None,
                    dev: 1,
                    ino: 2,
                    ctime: (0, 0),
                },
                execs: None,
            }],
            end: ChainEnd::Binary,
        });
        Some((
            Located {
                path,
                origin: TrustBinOrigin::Store,
            },
            identity,
        ))
    };
    let (mut ty, mut ay) = (None, None);
    let one = at("/store/ty/1/bin/ty", Some(10));
    let two = at("/store/ty/2/bin/ty", Some(10));
    let one_rebuilt = at("/store/ty/1/bin/ty", Some(11));
    assert_eq!(claim_report(&mut ty, &one), Claim::First);
    assert_eq!(claim_report(&mut ty, &one), Claim::Unchanged);
    assert_eq!(claim_report(&mut ay, &None), Claim::First, "per tool");
    assert_eq!(claim_report(&mut ty, &two), Claim::Changed(one.clone()));
    assert_eq!(claim_report(&mut ty, &two), Claim::Unchanged);
    assert_eq!(
        claim_report(&mut ty, &one),
        Claim::Changed(two.clone()),
        "a rollback is a change too"
    );
    assert_eq!(
        claim_report(&mut ty, &one_rebuilt),
        Claim::Changed(one.clone()),
        "the same path with another identity is a change"
    );
    assert_eq!(
        claim_report(&mut ty, &None),
        Claim::Changed(one_rebuilt.clone())
    );
    assert_eq!(claim_report(&mut ty, &None), Claim::Unchanged);
    assert_eq!(claim_report(&mut ay, &None), Claim::Unchanged);

    // What a repeated line says it replaces.
    let loc = |k: &ProvenanceKey| k.as_ref().map(|(l, _)| l.clone());
    assert_eq!(
        change_note(&None, loc(&one).as_ref(), None),
        " — changed mid-process (was NOT FOUND)"
    );
    assert_eq!(
        change_note(&one, loc(&two).as_ref(), None),
        " — changed mid-process (was /store/ty/1/bin/ty)"
    );
    assert_eq!(
        change_note(&one, loc(&one_rebuilt).as_ref(), None),
        " — changed mid-process (was this same path; a file it runs was since rewritten, \
         replaced or had its metadata changed)",
        "a same-path change never claims another build: a chmod moves the identity too"
    );
    assert_eq!(
        change_note(&two, None, Some(Path::new("/store"))),
        " — changed mid-process (was ~/ty/2/bin/ty)"
    );
}

/// Rewrite the file at `p` in place (same inode) with `body`, of its
/// current length, and put its mtime back — until the filesystem's ctime
/// clock has ticked, so only the ctime differs from before.
#[cfg(unix)]
fn rewrite_keeping_mtime(p: &Path, body: &str) {
    use std::os::unix::fs::MetadataExt as _;
    use std::time::{Duration, Instant};
    let before = std::fs::metadata(p).unwrap();
    assert_eq!(before.len(), body.len() as u64, "a same-length rewrite");
    let deadline = Instant::now() + Duration::from_secs(60);
    let after = loop {
        std::fs::write(p, body).unwrap();
        std::fs::File::options()
            .write(true)
            .open(p)
            .unwrap()
            .set_modified(before.modified().unwrap())
            .unwrap();
        let after = std::fs::metadata(p).unwrap();
        if (after.ctime(), after.ctime_nsec()) != (before.ctime(), before.ctime_nsec()) {
            break after;
        }
        assert!(Instant::now() < deadline, "the ctime never moved");
        std::thread::sleep(Duration::from_millis(1));
    };
    let rest = |m: &std::fs::Metadata| (m.len(), m.modified().unwrap(), m.dev(), m.ino());
    assert_eq!(rest(&after), rest(&before), "only the ctime differs");
}

/// Write `body` at `p` (parents created), mode 0755.
#[cfg(unix)]
fn write_exec(p: &Path, body: &str) {
    use std::os::unix::fs::PermissionsExt as _;
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(p, body).unwrap();
    std::fs::set_permissions(p, std::fs::Permissions::from_mode(0o755)).unwrap();
}

/// atpkg's exec stub forwarding to `target` (see `resolve_store_shim`).
#[cfg(unix)]
fn atpkg_stub(target: &Path) -> String {
    format!(
        "#!/bin/sh\n# atpkg shim — exec so the tool authenticates at its real path.\n\
         exec '{}' \"$@\"\n",
        target.display()
    )
}

/// A forwarder's exec target is read only when the shell would expand
/// nothing in it, and the rest of the line comes back as written (its
/// separating blank included), quoted or bare.
#[test]
fn a_shell_literal_word_is_read_only_when_nothing_expands() {
    let cases: &[(&str, Option<(&str, &str)>)] = &[
        ("'/a b/ty' \"$@\"", Some(("/a b/ty", " \"$@\""))),
        ("'/x/ty'", Some(("/x/ty", ""))),
        ("''", Some(("", ""))),
        ("'/x/$HOME/ty'", Some(("/x/$HOME/ty", ""))),
        ("\"/x/ty\" \"$@\"", Some(("/x/ty", " \"$@\""))),
        ("\"/x/ty\"\t\"$@\"", Some(("/x/ty", "\t\"$@\""))),
        ("/x/ty \"$@\"", Some(("/x/ty", " \"$@\""))),
        ("/x/ty\t\"$@\"", Some(("/x/ty", "\t\"$@\""))),
        ("/x/ty", Some(("/x/ty", ""))),
        ("/x/ty  --fast", Some(("/x/ty", "  --fast"))),
        ("", None),
        ("'/x/ty'tail", None),
        ("\"/x/ty\"tail", None),
        ("'/x/ty", None),
        ("\"/x/ty", None),
        ("\"/x/$HOME/ty\"", None),
        ("\"/x/`id`/ty\"", None),
        ("\"/x\\\\ty\"", None),
        ("/x/$HOME/ty", None),
        ("/x/*/ty", None),
        ("/x/ty;true", None),
        ("/x/ty'", None),
        ("~/ty", None),
    ];
    for (s, want) in cases {
        assert_eq!(shell_literal_word(s), *want, "{s:?}");
    }
}

/// The identity follows a wrapper to the binary it execs — atpkg's shim, a
/// hand-written literal forwarder, and a chain of both — so replacing the
/// binary alone changes the identity while the wrapper's own bytes do not.
/// A wrapper whose target does not read statically stops the chain and is
/// named for what it is (`ChainEnd::OpaqueScript`, the documented limit),
/// and a dangling target or a loop ends it without hanging.
#[cfg(unix)]
#[test]
fn a_binary_identity_follows_a_wrapper_to_the_binary_it_execs() {
    let s = Scratch::new("chain");
    let real = s.file("real/ty");
    let wrapper = s.root.path().join("pathbin/ty");
    let shape = |id: &BinaryIdentity| {
        id.links
            .iter()
            .map(|l| (l.path.clone(), l.execs))
            .collect::<Vec<_>>()
    };

    assert_eq!(binary_identity(&s.root.path().join("nope")), None);
    let direct = binary_identity(&real).unwrap();
    assert_eq!(
        (shape(&direct), &direct.end),
        (vec![(real.clone(), None)], &ChainEnd::Binary)
    );

    write_exec(&wrapper, &atpkg_stub(&real));
    let id = binary_identity(&wrapper).unwrap();
    assert_eq!(
        (shape(&id), &id.end),
        (
            vec![
                (wrapper.clone(), Some(ExecForm::AtpkgShim)),
                (real.clone(), None)
            ],
            &ChainEnd::Binary
        )
    );
    assert_eq!(id.links[1].file, direct.links[0].file);

    // Only the binary behind the wrapper is replaced.
    std::fs::write(&real, b"\x7fELF a rebuilt binary, longer\n").unwrap();
    let after = binary_identity(&wrapper).unwrap();
    assert_eq!(
        after.links[0].file, id.links[0].file,
        "the wrapper's own file did not change"
    );
    assert_ne!(after, id, "the build it execs did, and so did the identity");

    // Hand-written forwarders that read statically.
    let plain = !real
        .to_string_lossy()
        .contains(|c: char| c.is_whitespace() || "$`\\\"'*?[]{}~;&|<>()".contains(c));
    let mut literal = vec![
        format!("#!/bin/sh\nexec \"{}\" \"$@\"\n", real.display()),
        format!(
            "#!/bin/bash\n# forwards to the real build\nset -e\nexec '{}'\n",
            real.display()
        ),
        format!(
            "#!/bin/bash\nset -euo pipefail\n\n  # indented comment\nexec '{}' \"$@\"\n\n",
            real.display()
        ),
        // atpkg's exec line without atpkg's header: held to the literal rule,
        // which it meets.
        format!("#!/bin/sh\nexec '{}' \"$@\"\n", real.display()),
    ];
    if plain {
        literal.push(format!("#!/bin/sh\n\nexec {} \"$@\"\n", real.display()));
    }
    for body in &literal {
        write_exec(&wrapper, body);
        let id = binary_identity(&wrapper).unwrap();
        assert_eq!(
            (shape(&id), &id.end),
            (
                vec![
                    (wrapper.clone(), Some(ExecForm::LiteralExec)),
                    (real.clone(), None)
                ],
                &ChainEnd::Binary
            ),
            "{body}"
        );
    }

    // atpkg's other shapes keep their header, so its own rule reads them:
    // exports ahead of the `exec`, and a route guard whose `exec` is not
    // the store file's (atpkg's readers all answer the store file).
    let routed = format!(
        "#!/bin/sh\n# atpkg shim — exec so the tool authenticates at its real path.\n\
         # shim_env is aterm's policy for this program: only this managed copy runs with it.\n\
         export TY_EXAMPLE='1'\n\
         # atpkg exec root: (note)\n\
         [ ! -h '/root/bin/ty' ] && [ -f '/root/bin/ty' ] && exec '/root/bin/ty' \"$@\"\n\
         exec '{}' \"$@\"\n",
        real.display()
    );
    write_exec(&wrapper, &routed);
    let id = binary_identity(&wrapper).unwrap();
    assert_eq!(
        (shape(&id), &id.end),
        (
            vec![
                (wrapper.clone(), Some(ExecForm::AtpkgShim)),
                (real.clone(), None)
            ],
            &ChainEnd::Binary
        )
    );

    // A chain: a forwarder into atpkg's shim into the binary.
    let outer = s.root.path().join("outer/ty");
    write_exec(&wrapper, &atpkg_stub(&real));
    write_exec(
        &outer,
        &format!("#!/bin/sh\nexec \"{}\" \"$@\"\n", wrapper.display()),
    );
    let id = binary_identity(&outer).unwrap();
    assert_eq!(
        (shape(&id), &id.end),
        (
            vec![
                (outer.clone(), Some(ExecForm::LiteralExec)),
                (wrapper.clone(), Some(ExecForm::AtpkgShim)),
                (real.clone(), None)
            ],
            &ChainEnd::Binary
        )
    );

    // THE LIMIT: what does not read statically is the script's own identity.
    let opaque = [
        "#!/bin/sh\nexec \"$(dirname \"$0\")/../real/ty\" \"$@\"\n".to_owned(),
        "#!/bin/sh\nexec \"$HOME/ty\" \"$@\"\n".to_owned(),
        "#!/bin/sh\nexec ty \"$@\"\n".to_owned(),
        format!("#!/bin/sh\nexec /usr/bin/env '{}' \"$@\"\n", real.display()),
        format!("#!/bin/sh\nexec \"{}\" --fast \"$@\"\n", real.display()),
        format!(
            "#!/bin/sh\n[ -n \"$FAST\" ] && exec \"{0}\" \"$@\"\nexec \"{0}\" \"$@\"\n",
            real.display()
        ),
        format!("#!/bin/sh\n\"{}\" \"$@\"\n", real.display()),
        "#!/usr/bin/env python3\nimport os, sys\nos.execv('/x/ty', sys.argv)\n".to_owned(),
        // Another program runs before the `exec` — including atpkg's exec
        // line in a script that is not atpkg's.
        format!(
            "#!/bin/sh\n/usr/bin/true\nexec '{}' \"$@\"\n",
            real.display()
        ),
        format!("#!/bin/sh\nexec '{}' \"$@\"\necho never\n", real.display()),
        format!(
            "#!/bin/sh\nset -- --fast\nexec '{}' \"$@\"\n",
            real.display()
        ),
    ];
    for body in &opaque {
        write_exec(&wrapper, body);
        let id = binary_identity(&wrapper).unwrap();
        assert_eq!(
            (shape(&id), &id.end),
            (vec![(wrapper.clone(), None)], &ChainEnd::OpaqueScript),
            "{body}"
        );
    }

    // A target that is gone, and a loop.
    let gone = s.root.path().join("reclaimed/ty");
    write_exec(&wrapper, &atpkg_stub(&gone));
    let id = binary_identity(&wrapper).unwrap();
    assert_eq!(
        (shape(&id), &id.end),
        (
            vec![(wrapper.clone(), Some(ExecForm::AtpkgShim))],
            &ChainEnd::Dangling(gone)
        )
    );
    write_exec(&wrapper, &atpkg_stub(&outer));
    let id = binary_identity(&outer).unwrap();
    assert_eq!(id.end, ChainEnd::TooDeep);
    assert_eq!(id.links.len(), 2);
}

/// The line's `built` and `bytes` describe the binary a wrapper execs, not
/// the wrapper, and the wrapper's hops are named; an opaque wrapper or a
/// dangling one says so rather than passing its own file off as the build.
#[cfg(unix)]
#[test]
fn the_provenance_line_names_the_build_behind_a_wrapper() {
    use std::time::{Duration, UNIX_EPOCH};
    let s = Scratch::new("linewrap");
    let real = s.file("real/ty");
    std::fs::write(&real, vec![0x7f_u8; 4096]).unwrap();
    std::fs::File::options()
        .write(true)
        .open(&real)
        .unwrap()
        .set_modified(UNIX_EPOCH + Duration::from_secs(1_000_000_000))
        .unwrap();
    let wrapper = s.root.path().join("pathbin/ty");
    write_exec(
        &wrapper,
        &format!("#!/bin/sh\nexec \"{}\" \"$@\"\n", real.display()),
    );
    let path_var = std::ffi::OsString::from(wrapper.parent().unwrap());
    let found = s.locate("ty", Some(&path_var)).unwrap();
    assert_eq!(
        (found.origin, &found.path),
        (TrustBinOrigin::Path, &wrapper)
    );
    // The evidence header's stamp follows the same chain.
    assert_eq!(
        ty_build_stamp(&wrapper),
        format!("mtime 1000000000 (unix) of {}", real.display())
    );
    assert_eq!(ty_build_stamp(&real), "mtime 1000000000 (unix)");
    assert_eq!(
        s.line("ty", Some(&found)),
        format!(
            "VERIFY ESCALATION TIER: ty = {} [PATH (wrapper → {}); ty 9.9.9; \
             built 2001-09-09T01:46:40Z; 4096 bytes]",
            wrapper.display(),
            real.display()
        )
    );

    let body = "#!/bin/sh\nexec \"$(dirname \"$0\")/../real/ty\" \"$@\"\n";
    write_exec(&wrapper, body);
    let line = s.line("ty", Some(&found));
    assert!(
        line.contains(
            "[PATH (script whose exec target does not read statically: built/bytes are \
             the script's own); ty 9.9.9; built 2"
        ) && line.ends_with(&format!("; {} bytes]", body.len())),
        "{line}"
    );

    let gone = s.root.path().join("reclaimed/ty");
    write_exec(&wrapper, &atpkg_stub(&gone));
    let line = s.line("ty", Some(&found));
    assert!(
        line.contains(&format!(
            "[PATH (atpkg shim → {} (missing)); ",
            gone.display()
        )),
        "{line}"
    );
}

/// One discovery over `s`'s store and `path_var`, reported through `last`
/// exactly as `discover_identified` reports a real one: what it found, and
/// the lines it printed.
#[cfg(unix)]
fn discover_in(
    s: &Scratch,
    last: &LastReported,
    path_var: &std::ffi::OsStr,
) -> (Option<PathBuf>, Vec<String>) {
    let found = s.locate("ty", Some(path_var)).map(|located| {
        let identity = binary_identity(&located.path);
        (located, identity)
    });
    let mut lines = Vec::new();
    report_on_change(
        last,
        "ty",
        found.as_ref(),
        &[s.home()],
        Some(&s.store_bin()),
        |_| (Some("ty 9.9.9".to_owned()), "; caps=none".to_owned()),
        |line| lines.push(line.to_owned()),
    );
    (found.map(|(located, _)| located.path), lines)
}

/// THE AUDIT TRAIL, END TO END. In ONE process: a store update repoints
/// the shim, the new build is rebuilt in place, the shim is yanked, a PATH
/// wrapper takes over, and then ONLY the binary that wrapper execs is
/// replaced. Each must print a fresh `VERIFY ESCALATION TIER` line naming
/// the build now in use (its path, its size, and what it replaced), and a
/// rediscovery that changes nothing must print nothing.
#[cfg(unix)]
#[test]
fn provenance_is_reported_again_whenever_the_resolved_binary_changes() {
    let s = Scratch::new("change");
    let last = LastReported::default();
    let shim = s.store_bin().join("ty");
    let build = |n: &str| s.root.path().join(format!("prefix/store/ty/{n}/bin/ty"));
    let behind = s.root.path().join("real/ty");
    let wrapper = s.root.path().join("pathbin/ty");
    std::fs::create_dir_all(wrapper.parent().unwrap()).unwrap();
    let path_var = std::ffi::OsString::from(wrapper.parent().unwrap());
    let elf = |what: &str| format!("\x7fELF {what}\n");
    let step = |want_path: Option<&Path>, want: &[(String, &str, String)]| {
        let (found, lines) = discover_in(&s, &last, &path_var);
        assert_eq!(found.as_deref(), want_path);
        assert_eq!(lines.len(), want.len(), "{lines:#?}");
        for (line, (prefix, bytes, replaced)) in lines.iter().zip(want) {
            assert!(
                line.starts_with(prefix.as_str()),
                "want {prefix}\n got {line}"
            );
            assert!(line.contains(bytes), "want {bytes}\n got {line}");
            assert!(
                line.ends_with(replaced.as_str()),
                "want {replaced:?}\n got {line}"
            );
            assert_eq!(
                line.contains("changed mid-process"),
                !replaced.is_empty(),
                "only a repeated line says what it replaced: {line}"
            );
        }
    };
    let store_line = |p: &Path| {
        format!(
            "VERIFY ESCALATION TIER: ty = {} [atpkg managed store; ty 9.9.9; built ",
            p.display()
        )
    };
    let bytes = |body: &str| format!("; {} bytes]; caps=none", body.len());
    let was = |what: &str| format!(" — changed mid-process (was {what})");
    let same = was(SAME_PATH_CHANGED);

    write_exec(&build("1"), &elf("build one"));
    write_exec(&shim, &atpkg_stub(&build("1")));
    step(
        Some(&build("1")),
        &[(
            store_line(&build("1")),
            &bytes(&elf("build one")),
            String::new(),
        )],
    );
    step(Some(&build("1")), &[]);
    step(Some(&build("1")), &[]);

    // An `aterm pkg` update: the shim forwards to build 2.
    write_exec(&build("2"), &elf("build two"));
    write_exec(&shim, &atpkg_stub(&build("2")));
    let from_one = was(&build("1").display().to_string());
    step(
        Some(&build("2")),
        &[(store_line(&build("2")), &bytes(&elf("build two")), from_one)],
    );
    step(Some(&build("2")), &[]);

    // Build 2 rebuilt at its own path.
    write_exec(&build("2"), &elf("build two, rebuilt"));
    step(
        Some(&build("2")),
        &[(
            store_line(&build("2")),
            &bytes(&elf("build two, rebuilt")),
            same.clone(),
        )],
    );

    // Rewritten in place AGAIN, at the same length and with its mtime put
    // back (`cp -p`, `touch -r`, a tar extract): the same path, length,
    // mtime, device and inode, so only the ctime tells.
    let relaid = elf("BUILD TWO, REBUILT");
    assert_eq!(relaid.len(), elf("build two, rebuilt").len());
    rewrite_keeping_mtime(&build("2"), &relaid);
    step(
        Some(&build("2")),
        &[(store_line(&build("2")), &bytes(&relaid), same.clone())],
    );
    step(Some(&build("2")), &[]);

    // A yank: the shim becomes a tombstone; PATH holds no `ty` yet.
    write_exec(
        &shim,
        "#!/bin/sh\necho 'ty: this build was yanked' >&2\nexit 1\n",
    );
    let not_found = "VERIFY ESCALATION TIER: ty = NOT FOUND (probed the atpkg store shim, PATH)";
    step(
        None,
        &[(
            not_found.to_owned(),
            "",
            was(&build("2").display().to_string()),
        )],
    );
    step(None, &[]);

    // A PATH wrapper takes over, forwarding to a build outside the store.
    write_exec(&behind, &elf("path build a"));
    write_exec(&wrapper, &atpkg_stub(&behind));
    let path_line = format!(
        "VERIFY ESCALATION TIER: ty = {} [PATH (atpkg shim → {}); ty 9.9.9; built ",
        wrapper.display(),
        behind.display()
    );
    step(
        Some(&wrapper),
        &[(
            path_line.clone(),
            &bytes(&elf("path build a")),
            was("NOT FOUND"),
        )],
    );

    // ONLY the binary behind the wrapper is replaced; the wrapper's bytes
    // are untouched.
    let before = std::fs::read(&wrapper).unwrap();
    write_exec(&behind, &elf("path build b, rebuilt"));
    assert_eq!(std::fs::read(&wrapper).unwrap(), before);
    step(
        Some(&wrapper),
        &[(path_line, &bytes(&elf("path build b, rebuilt")), same)],
    );
    step(Some(&wrapper), &[]);
}

/// Each tool keeps its own last line, and a line holds only ITS tool while
/// its probes run: while one tool's first line waits on a slow `--version`
/// (a fresh build's first exec on a loaded Mac took over five seconds), a
/// discovery of another tool still reports and returns, and a repeat of
/// that other tool stays silent.
#[test]
fn a_slow_provenance_probe_holds_up_only_its_own_tool() {
    use std::sync::{Arc, mpsc};
    use std::time::Duration;
    /// One discovery of `/nowhere/<bin>` reported through `last`; the probe
    /// runs `inside_probe` first. The lines it printed.
    fn report(last: &LastReported, bin: &'static str, inside_probe: impl FnOnce()) -> Vec<String> {
        let located = Located {
            path: PathBuf::from(format!("/nowhere/{bin}")),
            origin: TrustBinOrigin::Path,
        };
        let mut lines = Vec::new();
        report_on_change(
            last,
            bin,
            Some(&(located, None)),
            &[],
            None,
            |_| {
                inside_probe();
                (Some(format!("{bin} 1.0")), String::new())
            },
            |line| lines.push(line.to_owned()),
        );
        lines
    }
    let last = Arc::new(LastReported::default());
    let (release, held) = mpsc::channel::<()>();
    let (entered, probing) = mpsc::channel::<()>();
    let slow = {
        let last = Arc::clone(&last);
        std::thread::spawn(move || {
            report(&last, "ty", move || {
                let _ = entered.send(());
                let _ = held.recv();
            })
        })
    };
    probing
        .recv_timeout(Duration::from_secs(30))
        .expect("the slow ty probe started");
    let (done, other) = mpsc::channel();
    {
        let last = Arc::clone(&last);
        std::thread::spawn(move || {
            let first = report(&last, "ay", || {});
            let again = report(&last, "ay", || {});
            let _ = done.send((first, again));
        });
    }
    let ay = other.recv_timeout(Duration::from_secs(30));
    drop(release);
    let ty = slow.join().unwrap();
    let (first, again) = ay.expect("an `ay` discovery waited on `ty`'s probe");
    assert_eq!(
        first,
        [
            "VERIFY ESCALATION TIER: ay = /nowhere/ay [PATH; ay 1.0; built date unknown; size unknown]"
        ]
    );
    assert!(again.is_empty(), "{again:?}");
    assert_eq!(
        ty,
        [
            "VERIFY ESCALATION TIER: ty = /nowhere/ty [PATH; ty 1.0; built date unknown; size unknown]"
        ]
    );
    assert!(report(&last, "ty", || {}).is_empty(), "ty's line stands");
}

/// A tool's lines are WRITTEN in the order their keys were claimed, so the
/// last line written names what the tool's [`LastLine`] holds — the build
/// in use. Here a change to `/race/two` waits in its slow probe while a
/// second thread changes back to `/race/one`; then a repeat of `/race/one`
/// is silent, which is right only if the last line written names it. (Were
/// the slot released at the claim, the quick change back would be written
/// first and the slow line last, and the log would end naming `/race/two`
/// while `/race/one` runs, with nothing more to come.)
#[test]
fn a_tools_last_line_names_what_runs_when_its_changes_race() {
    use std::sync::{Arc, mpsc};
    use std::time::Duration;
    /// One report of `/race/<p>` through `last` into `out`; the probe runs
    /// `inside_probe` first.
    fn report(last: &LastReported, out: &Mutex<Vec<String>>, p: &str, inside_probe: impl FnOnce()) {
        let located = Located {
            path: PathBuf::from(format!("/race/{p}")),
            origin: TrustBinOrigin::Path,
        };
        report_on_change(
            last,
            "ty",
            Some(&(located, None)),
            &[],
            None,
            |_| {
                inside_probe();
                (Some("ty 1.0".to_owned()), String::new())
            },
            |line| out.lock().unwrap().push(line.to_owned()),
        );
    }
    let last = Arc::new(LastReported::default());
    let out = Arc::new(Mutex::new(Vec::<String>::new()));
    report(&last, &out, "one", || {});
    let (entered_tx, entered) = mpsc::channel::<()>();
    let (release, held) = mpsc::channel::<()>();
    let slow = {
        let (last, out) = (Arc::clone(&last), Arc::clone(&out));
        std::thread::spawn(move || {
            report(&last, &out, "two", move || {
                let _ = entered_tx.send(());
                let _ = held.recv_timeout(Duration::from_secs(30));
            });
        })
    };
    entered
        .recv_timeout(Duration::from_secs(30))
        .expect("the slow probe started");
    let back = {
        let (last, out) = (Arc::clone(&last), Arc::clone(&out));
        std::thread::spawn(move || report(&last, &out, "one", || {}))
    };
    // Time for the change back to reach its claim (it is made to wait there).
    std::thread::sleep(Duration::from_millis(500));
    drop(release);
    slow.join().unwrap();
    back.join().unwrap();
    report(&last, &out, "one", || {});
    let lines = out.lock().unwrap().clone();
    assert_eq!(lines.len(), 3, "{lines:#?}");
    assert!(
        lines[2].starts_with("VERIFY ESCALATION TIER: ty = /race/one ")
            && lines[2].ends_with("(was /race/two)"),
        "the last line written must name what runs now: {lines:#?}"
    );
}

/// `ty`'s capabilities are asked again for another build: another path, a
/// rebuild in place, or a wrapper whose target alone was replaced — the
/// `caps=` a re-printed provenance line carries is the new build's.
#[cfg(unix)]
#[test]
fn ty_capabilities_are_asked_again_for_another_build() {
    use std::cell::Cell;
    let s = Scratch::new("caps-build");
    let real = s.file("real/ty");
    let wrapper = s.root.path().join("pathbin/ty");
    write_exec(&wrapper, &atpkg_stub(&real));
    let cache = Mutex::new(BTreeMap::new());
    let probes = Cell::new(0);
    let ask = |ty: &Path| {
        caps_cached(&cache, ty, |_| {
            probes.set(probes.get() + 1);
            vec![format!("probe-{}", probes.get())]
        })
    };
    assert_eq!(ask(&wrapper), ["probe-1"]);
    assert_eq!(ask(&wrapper), ["probe-1"], "the same build: cached");
    std::fs::write(&real, b"\x7fELF rebuilt behind the wrapper\n").unwrap();
    assert_eq!(ask(&wrapper), ["probe-2"]);
    assert_eq!(ask(&real), ["probe-3"], "another path");
    assert_eq!(ask(&real), ["probe-3"]);
}

#[test]
fn build_dates_are_utc_civil_dates() {
    assert_eq!(civil_from_unix_days(0), (1970, 1, 1));
    assert_eq!(civil_from_unix_days(-1), (1969, 12, 31));
    assert_eq!(civil_from_unix_days(19_723), (2024, 1, 1));
    assert_eq!(civil_from_unix_days(20_719), (2026, 9, 23));
    let s = Scratch::new("date");
    let d = build_date(&s.file("x")).expect("a fresh file has an mtime");
    assert_eq!((d.len(), &d[4..5], &d[7..8]), (10, "-", "-"), "{d}");
}

/// The one spawn: `--version`'s first non-empty line. `/bin/echo` echoes
/// its argument on the BSD userland and names itself on GNU — either way a
/// line, from a binary that is not freshly written (a fresh file can stall
/// in exec assessment under load, measured on this host).
#[cfg(unix)]
#[test]
fn the_version_probe_returns_the_first_line() {
    let v = probe_version(Path::new("/bin/echo")).expect("echo answers");
    assert!(!v.is_empty() && !v.contains('\n'), "{v:?}");
    assert_eq!(probe_version(Path::new("/nonexistent/ty")), None);
}

/// A8: the capabilities are `ty capabilities --json`'s list, in its order;
/// a `ty` without the command (exit 2, `unrecognized subcommand`), a
/// killed probe or a reply that is not the document all have none — the
/// per-process protocol every `ty` speaks.
#[test]
fn ty_capabilities_are_the_json_list_and_an_older_ty_has_none() {
    let json = b"{\"ty\":\"0.15.0\",\"capabilities\":[\"check-no-cache-v1\",\"trace-cases-v1\"]}\n";
    assert_eq!(
        caps_from(Some(0), json),
        ["check-no-cache-v1", "trace-cases-v1"]
    );
    assert!(caps_from(Some(2), b"").is_empty(), "exit 2: an older ty");
    assert!(caps_from(None, json).is_empty(), "a killed probe");
    assert!(
        caps_from(Some(0), b"trace-cases-v1\n").is_empty(),
        "the plain listing is not the document"
    );
    assert_eq!(caps_clause(&[]), "; caps=none");
    assert_eq!(
        caps_clause(&["a-v1".to_string(), "b-v1".to_string()]),
        "; caps=a-v1,b-v1"
    );
}

/// The real probe over a `ty` that predates the command: store `ty` 0.13.0's
/// exact answer is the empty set.
#[cfg(unix)]
#[test]
fn an_older_ty_probes_as_no_capabilities() {
    let s = Scratch::new("caps");
    let ty = stand_in(
        &s.home(),
        "echo \"error: unrecognized subcommand 'capabilities'\" >&2\nexit 2\n",
    );
    assert!(ty_caps(&ty).is_empty());
}

/// THE PROBE BOUND IS A HANG DETECTOR, NOT A BUDGET. A probe that answers
/// slowly (the first exec of a fresh script on a loaded Mac took over five
/// seconds) is still answered and its capabilities read; one that never
/// answers is killed at the bound it was given, and is the empty set — the
/// per-process behaviour every `ty` supports. Pinned through the bound
/// seam, so no test waits a minute: the answering probe sleeps 1 s under a
/// 30 s bound, the wedged one 300 s (`exec`, so the kill reaches the
/// sleep itself) under a 300 ms bound, so the minute hang detector still
/// tells killed from waited out.
#[cfg(unix)]
#[test]
fn a_slow_probe_is_answered_and_a_wedged_one_is_killed_at_its_bound() {
    use std::time::{Duration, Instant};
    let s = Scratch::new("probe-bound");
    let slow = stand_in(&s.home(), "sleep 1\necho slow-answer\n");
    let out = probe_output_within(&slow, &[], Duration::from_secs(30))
        .expect("a probe that answers inside its bound is read");
    assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), "slow-answer");

    let wedged_dir = Scratch::new("probe-wedged");
    let wedged = stand_in(&wedged_dir.home(), "exec sleep 300\n");
    let began = Instant::now();
    assert!(
        probe_output_within(&wedged, &[], Duration::from_millis(300)).is_none(),
        "a probe past its bound is None"
    );
    assert!(
        began.elapsed() < crate::wait::MUST_HAPPEN,
        "killed at the bound, not waited out (the stand-in lives 300 s)"
    );
    assert!(
        PROBE_HANG >= Duration::from_secs(30),
        "the production bound must stay a hang detector (a loaded macOS first exec \
         took over 5 s), not a latency budget"
    );
}

/// An executable `/bin/sh` stand-in for a binary, in `dir`.
#[cfg(unix)]
fn stand_in(dir: &Path, body: &str) -> PathBuf {
    use std::os::unix::fs::PermissionsExt as _;
    let bin = dir.join("ty");
    std::fs::write(&bin, format!("#!/bin/sh\n{body}")).unwrap();
    std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755)).unwrap();
    bin
}

/// A stand-in `ty` for the `--cases` pool (A10): it advertises the cases
/// mode, runs `cases` (shell) as `trace validate --cases -`, logging each
/// such spawn to `<dir>/spawns`, and answers the per-process form with
/// `per-process` and exit 1 — a refusal, so a case the pool did not
/// answer is visibly the fallback's, and a pool accept could never hide
/// behind it. A child that must die abnormally dies by SIGKILL (AGENTS.md
/// rule 6).
#[cfg(unix)]
fn cases_stand_in(dir: &Path, model: &str, cases: &str) -> PathBuf {
    let header = format!(
        "{{\"ty_trace_cases\":1,\"ty\":\"0\",\"module\":\"{model}\",\"variables\":[\"x\"],\
         \"init\":\"Init\",\"next\":\"Next\",\"action_label_mode\":\"error\"}}"
    );
    stand_in(
        dir,
        &format!(
            "if [ \"$1\" = capabilities ]; then\n\
             echo '{{\"ty\":\"0\",\"capabilities\":[\"check-budget-self-v1\",\"trace-cases-v1\"]}}'\n\
             exit 0\nfi\n\
             case \"$*\" in *--cases*)\n\
             echo spawn >> '{log}'\nHEADER='{header}'\n{cases}\nexit 0;;\nesac\n\
             echo per-process\nexit 1\n",
            log = dir.join("spawns").display(),
        ),
    )
}

/// A stand-in's `--cases` body: the header, then one verdict per request,
/// `{"id":<the request's>,<rest>}`, then `after` (shell).
#[cfg(unix)]
fn answering(rest: &str, after: &str) -> String {
    format!(
        "echo \"$HEADER\"\nwhile IFS= read -r line; do\nid=${{line#'{{\"id\":'}}\nid=${{id%%,*}}\n\
         printf '{{\"id\":%s,{rest}}}\\n' \"$id\"\n{after}\ndone"
    )
}

/// `n` distinct two-step traces of `model`'s one variable `x`.
#[cfg(unix)]
fn x_trace(model: &str, n: u32) -> String {
    format!(
        "{{\"version\":\"1\",\"module\":\"{model}\",\"variables\":[\"x\"],\"steps\":[\
         {{\"index\":0,\"state\":{{\"x\":{{\"type\":\"int\",\"value\":{n}}}}}}},\
         {{\"index\":1,\"state\":{{\"x\":{{\"type\":\"int\",\"value\":{}}}}}}}]}}",
        n + 1
    )
}

/// How many `--cases` co-processes a stand-in was spawned as.
#[cfg(unix)]
fn spawns(dir: &Path) -> usize {
    std::fs::read_to_string(dir.join("spawns")).map_or(0, |s| s.lines().count())
}

/// The pool's positive control: verdicts come from the co-process, and a
/// verdict marked `end` retires its child without spending a try — each of
/// three cases is answered by a child of its own.
#[cfg(unix)]
#[test]
fn the_cases_pool_answers_and_an_end_verdict_retires_its_child() {
    let s = Scratch::new("cases-end");
    let model = "PoolEndControl";
    let ty = cases_stand_in(
        &s.home(),
        model,
        &answering(
            "\"ok\":true,\"steps\":2,\"warnings\":[],\"end\":\"first-seen tables moved\"",
            "exit 3",
        ),
    );
    let spec = format!("---- MODULE {model} ----\n====\n");
    for n in 0..3 {
        let (ok, why) = ty_trace_verdict(
            &ty,
            model,
            spec.clone(),
            "C\n".into(),
            x_trace(model, n),
            model,
        );
        assert!(ok && why.starts_with("ty (cases) "), "{why}");
    }
    assert_eq!(spawns(&s.home()), 3, "an `end` verdict retires its child");
}

/// A co-process that dies before its header (SIGKILL): every case is
/// answered per process, the (spec, cfg) gets one respawn, and then no
/// more co-processes for the rest of the process.
#[cfg(unix)]
#[test]
fn a_dead_cases_child_falls_back_per_process_and_respawns_once() {
    let s = Scratch::new("cases-dead");
    let model = "PoolDeadChild";
    let ty = cases_stand_in(&s.home(), model, "kill -KILL $$");
    let spec = format!("---- MODULE {model} ----\n====\n");
    for n in 0..4 {
        let verdict = ty_trace_verdict(
            &ty,
            model,
            spec.clone(),
            "C\n".into(),
            x_trace(model, n),
            model,
        );
        assert_eq!(verdict, (false, "per-process\n".to_string()));
    }
    assert_eq!(spawns(&s.home()), 2, "one spawn and one respawn, no more");
}

/// A12: a co-process `class: "error"` verdict is no refusal to agree with —
/// it panics, naming why.
#[cfg(unix)]
#[test]
#[should_panic(expected = "ty COULD NOT EVALUATE")]
fn an_evaluation_error_is_not_a_refutation() {
    let s = Scratch::new("cases-error");
    let model = "PoolEvalError";
    let ty = cases_stand_in(
        &s.home(),
        model,
        &answering(
            "\"ok\":false,\"class\":\"error\",\"kind\":\"action_expr_eval_failed\",\
             \"step\":1,\"message\":\"boom\",\"warnings\":[]",
            "",
        ),
    );
    let spec = format!("---- MODULE {model} ----\n====\n");
    let _ = ty_trace_verdict(&ty, model, spec, "C\n".into(), x_trace(model, 0), model);
}

/// THE CLIENT RULE: a request with no verdict line has no verdict — never
/// an accept. A child that closes stdout instead of answering, echoes
/// another request's id (with `ok: true`), or writes a line that is not a
/// verdict, each leaves the case to the per-process fallback, which here
/// refuses: a pool that read any of them as an accept would answer `true`.
#[cfg(unix)]
#[test]
fn a_case_with_no_verdict_line_is_never_an_accept() {
    let bad_answers = [
        ("PoolNoVerdict", "IFS= read -r line\nexit 0"),
        (
            "PoolWrongId",
            "IFS= read -r line\necho '{\"id\":99,\"ok\":true,\"steps\":2,\"warnings\":[]}'\n\
             IFS= read -r line",
        ),
        (
            "PoolNotJson",
            "IFS= read -r line\necho ok\nIFS= read -r line",
        ),
    ];
    for (model, answer) in bad_answers {
        let s = Scratch::new("cases-silent");
        let ty = cases_stand_in(&s.home(), model, &format!("echo \"$HEADER\"\n{answer}"));
        let spec = format!("---- MODULE {model} ----\n====\n");
        let verdict = ty_trace_verdict(&ty, model, spec, "C\n".into(), x_trace(model, 0), model);
        assert_eq!(verdict, (false, "per-process\n".to_string()), "{model}");
        assert_eq!(spawns(&s.home()), 1, "{model}: the co-process was asked");
    }
}

/// The `ty` verdict memo replays a verdict only for the same rendered files
/// asked of the same binary. A `ty` rewritten in place is asked again, even
/// at the same length with its old mtime put back (only the ctime moves); so
/// is one whose path was repointed while it ran, even once the path names
/// the first binary again (which build answered is not known); and a binary
/// that cannot be stat'ed is never memoized.
#[cfg(unix)]
#[test]
fn a_ty_verdict_is_replayed_only_for_the_same_binary_and_files() {
    use std::cell::Cell;
    use std::os::unix::fs::symlink;
    use std::time::{Duration, SystemTime};
    let s = Scratch::new("ty-memo");
    let (a, b, ty) = (s.home().join("a"), s.home().join("b"), s.home().join("ty"));
    let point_ty_at = |target: &Path| {
        let _ = std::fs::remove_file(&ty);
        symlink(target, &ty).unwrap();
    };
    let runs = Cell::new(0);
    let ask_while = |spec: &str, during_the_run: &dyn Fn()| {
        let run = |checker: Option<&TyChecker>, _: &str, _: &str, _: &str| {
            during_the_run();
            runs.set(runs.get() + 1);
            let note = if checker.is_some() {
                ""
            } else {
                " (no checker)"
            };
            (true, format!("run {}{note}", runs.get()))
        };
        memoized_verdict(&ty, spec.into(), "cfg".into(), "trace".into(), run).1
    };
    let ask = |spec: &str| ask_while(spec, &|| {});
    std::fs::write(&a, "v1").unwrap();
    point_ty_at(&a);
    assert_eq!(ask("spec"), "run 1");
    assert_eq!(ask("spec"), "run 1", "the same question to the same binary");
    assert_eq!(
        ask("other spec"),
        "run 2",
        "another spec is another question"
    );

    // Rewritten in place: same path, length and inode; the mtime moves.
    std::fs::write(&a, "v2").unwrap();
    let set_mtime = |t: SystemTime| {
        let f = std::fs::File::options().write(true).open(&a).unwrap();
        f.set_modified(t).unwrap();
    };
    set_mtime(SystemTime::now() + Duration::from_secs(60));
    assert_eq!(ask("spec"), "run 3", "a binary replaced in place");

    // Rewritten in place again with the mtime put back: only the ctime
    // tells.
    rewrite_keeping_mtime(&a, "v3");
    assert_eq!(
        ask("spec"),
        "run 4",
        "a same-length rewrite, mtime put back"
    );

    // Repointed while it ran, then pointed back.
    std::fs::write(&b, "another build").unwrap();
    assert_eq!(ask_while("asked mid-swap", &|| point_ty_at(&b)), "run 5");
    point_ty_at(&a);
    assert_eq!(
        ask("asked mid-swap"),
        "run 6",
        "a run the binary changed under is never memoized"
    );
    assert_eq!(ask("asked mid-swap"), "run 6");

    std::fs::remove_file(&a).unwrap();
    assert_eq!(ask("spec"), "run 7 (no checker)", "a vanished binary");
    assert_eq!(ask("spec"), "run 8 (no checker)", "is never memoized");
}

/// A PATH `ty` that is a wrapper script is the checker its TARGET is: when
/// only the binary the wrapper execs is rebuilt (the wrapper's own bytes
/// untouched), the memo asks again instead of replaying the old build's
/// verdict, and the checker handed to the run (which keys the `--cases`
/// pool) is a new one that names the binary behind the wrapper.
#[cfg(unix)]
#[test]
fn a_ty_verdict_behind_a_wrapper_is_asked_again_when_only_its_target_changes() {
    use std::cell::{Cell, RefCell};
    let s = Scratch::new("ty-memo-wrapper");
    let real = s.root.path().join("real/ty");
    let wrapper = s.root.path().join("pathbin/ty");
    write_exec(&real, "\x7fELF build a\n");
    write_exec(
        &wrapper,
        &format!("#!/bin/sh\nexec \"{}\" \"$@\"\n", real.display()),
    );
    let runs = Cell::new(0);
    let checkers = RefCell::new(Vec::new());
    let ask = || {
        let run = |checker: Option<&TyChecker>, _: &str, _: &str, _: &str| {
            checkers.borrow_mut().push(checker.cloned());
            runs.set(runs.get() + 1);
            (true, format!("run {}", runs.get()))
        };
        memoized_verdict(&wrapper, "spec".into(), "cfg".into(), "trace".into(), run).1
    };
    assert_eq!(ask(), "run 1");
    assert_eq!(ask(), "run 1", "the same build behind the same wrapper");

    let wrapper_bytes = std::fs::read(&wrapper).unwrap();
    write_exec(&real, "\x7fELF build b, rebuilt behind the wrapper\n");
    assert_eq!(std::fs::read(&wrapper).unwrap(), wrapper_bytes);
    assert_eq!(ask(), "run 2", "only the wrapper's target changed");
    assert_eq!(ask(), "run 2");

    let checkers = checkers.into_inner();
    assert_eq!(checkers.len(), 2);
    let (first, second) = (
        checkers[0].clone().expect("a checker"),
        checkers[1].clone().expect("a checker"),
    );
    assert_eq!((&first.0, &second.0), (&wrapper, &wrapper));
    assert_ne!(first, second, "the pool keys on a new checker");
    let ends: Vec<_> = [&first, &second]
        .iter()
        .map(|c| c.1.last().map(|l| l.path.clone()))
        .collect();
    assert_eq!(ends, [Some(real.clone()), Some(real.clone())]);
}

/// An `aterm pkg` update, a re-laid build and a yank, all MID-PROCESS, in a
/// scratch account store: [`find_ty`] answers the build the shim forwards
/// to NOW, and one transition is asked of that build every time — never
/// answered from the memo, or a live `--cases` co-process, of another.
/// Build 1 is an older `ty` (one process per case), build 2 a `--cases` one;
/// every build accepts, so the tiers agree and the verdict names its build.
///
/// The store is the ACCOUNT's ([`home_dirs`]), so the steps run in a child
/// of this test binary that seats a scratch home in [`ACCOUNT_HOME`] before
/// its first discovery, with a `PATH` that holds no `ty`.
#[cfg(unix)]
#[test]
fn find_ty_and_its_verdicts_follow_a_store_update_mid_process() {
    const CHILD: &str = "ATERM_SPEC_STORE_SWAP_CHILD";
    if let Some(home) = std::env::var_os(CHILD) {
        store_swap_steps(Path::new(&home));
        return;
    }
    let s = Scratch::new("store-swap");
    let no_ty = s.root.path().join("path-without-ty");
    std::fs::create_dir_all(&no_ty).unwrap();
    let out = std::process::Command::new(std::env::current_exe().expect("test exe"))
        .args([
            "--exact",
            "verify::tests::find_ty_and_its_verdicts_follow_a_store_update_mid_process",
            "--nocapture",
            "--test-threads",
            "1",
        ])
        .env(CHILD, s.home())
        .env("PATH", &no_ty)
        .stdin(std::process::Stdio::null())
        .output()
        .expect("run the child");
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(out.status.success(), "the store-swap child failed:\n{text}");
    assert!(
        text.contains("test result: ok. 1 passed"),
        "the child ran no test:\n{text}"
    );
}

/// The child's half of [`find_ty_and_its_verdicts_follow_a_store_update_mid_process`].
#[cfg(unix)]
fn store_swap_steps(home: &Path) {
    use std::os::unix::fs::PermissionsExt as _;
    assert!(
        ACCOUNT_HOME.set(home.to_path_buf()).is_ok(),
        "nothing in this child discovered a tool before its scratch home was seated"
    );
    let shim = unix_store_bin_dir(home).join("ty");
    let prefix = shim.parent().and_then(Path::parent).unwrap().to_path_buf();
    let build = |n: &str| prefix.join("store/ty").join(n).join("bin/ty");
    let canon = |p: PathBuf| std::fs::canonicalize(p).unwrap();
    // Laid by rename, as an install or a rebuild lays a binary: a live
    // co-process keeps running the file it was spawned from.
    let lay = |p: &Path, body: &str| {
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        let new = p.with_extension("new");
        std::fs::write(&new, format!("#!/bin/sh\n{body}")).unwrap();
        std::fs::set_permissions(&new, std::fs::Permissions::from_mode(0o755)).unwrap();
        std::fs::rename(&new, p).unwrap();
    };
    // atpkg's exec stub (see `resolve_store_shim`).
    let point_shim_at = |target: &Path| {
        lay(&shim, &format!("exec '{}' \"$@\"\n", target.display()));
    };

    let m = ring_model();
    let init = m.init_state();
    let (action, next) = m
        .actions
        .iter()
        .find_map(|a| {
            let next = m
                .successors(a.name, &init)
                .into_iter()
                .find(|n| *n != init)?;
            Some((a.name, next))
        })
        .expect("the ring model steps from its initial state");
    let ask = |label: &str| validate_transition_tiered(&m, &[], &init, &next, Some(action), label);
    let vars: Vec<String> = m.vars.iter().map(|v| format!("\"{}\"", v.name)).collect();
    let header = format!(
        "{{\"ty_trace_cases\":1,\"ty\":\"0\",\"module\":\"{}\",\"variables\":[{}],\
         \"init\":\"Init\",\"next\":\"Next\",\"action_label_mode\":\"error\"}}",
        m.name,
        vars.join(",")
    );
    let version = |name: &str| {
        format!("if [ \"$1\" = --version ]; then echo 'ty 0.0.0-{name}'; exit 0; fi\n")
    };
    // An older `ty`: no `capabilities` command, one process per case.
    let per_process = |name: &str| {
        format!(
            "{}if [ \"$1\" = capabilities ]; then exit 2; fi\necho 'FAKE TY {name} ACCEPTS'\n",
            version(name)
        )
    };
    // A `--cases` `ty`. A case left to one process of its own is refused,
    // so a pool that stopped answering would fail loudly here.
    let cases = |name: &str| {
        format!(
            "{}if [ \"$1\" = capabilities ]; then\n\
             echo '{{\"ty\":\"0\",\"capabilities\":[\"check-budget-self-v1\",\"trace-cases-v1\"]}}'\n\
             exit 0\nfi\n\
             case \"$*\" in *--cases*)\nHEADER='{header}'\n{}\nexit 0;;\nesac\n\
             echo 'per-process: refused'\nexit 1\n",
            version(name),
            answering(
                &format!("\"ok\":true,\"steps\":2,\"warnings\":[\"FAKE TY {name} ACCEPTS\"]"),
                ""
            ),
        )
    };
    lay(&build("1"), &per_process("build-1"));
    lay(&build("2"), &cases("build-2"));
    point_shim_at(&build("1"));

    assert_eq!(
        find_ty(),
        Some(canon(build("1"))),
        "the scratch store answers"
    );
    let (ok, text) = ask("store build 1");
    assert!(ok && text.contains("FAKE TY build-1 ACCEPTS"), "{text}");

    // An update: the shim forwards to build 2; build 1 stays on disk.
    point_shim_at(&build("2"));
    assert_eq!(
        find_ty(),
        Some(canon(build("2"))),
        "find_ty follows the shim to the build it forwards to now"
    );
    let (ok, text) = ask("store build 2");
    assert!(
        ok && text.starts_with("ty (cases) ") && text.contains("FAKE TY build-2 ACCEPTS"),
        "the step is asked of build 2's co-process, not answered with build 1's verdict:\n{text}"
    );

    // Build 2 re-laid at its own path (a linked checkout rebuilt, say).
    lay(&build("2"), &cases("build-2-rebuilt"));
    assert_eq!(find_ty(), Some(canon(build("2"))));
    let (ok, text) = ask("store build 2, re-laid");
    assert!(
        ok && text.contains("FAKE TY build-2-rebuilt ACCEPTS"),
        "a binary re-laid at the same path is asked again, by a child of its own:\n{text}"
    );

    // A yank: the shim becomes a tombstone (no exec line).
    lay(&shim, "echo 'ty: this build was yanked' >&2\nexit 1\n");
    assert_eq!(find_ty(), None, "a tombstoned shim reads as absent");
    let (ok, text) = ask("after the yank");
    assert!(
        ok && text.starts_with("interpreter: "),
        "with no ty, the interpreter answers alone:\n{text}"
    );
}

/// The `--cases` pool and the verdict memo answer only for the build the
/// provenance line names. Once `ty` is re-laid at its path (a rebuild or a
/// reinstall, by rename: the old build's live child keeps its own file),
/// the step already answered is asked again and a new step of the same
/// (spec, cfg) goes to a child of the NEW build — never replayed from the
/// old build's memo or asked of its live co-process.
#[cfg(unix)]
#[test]
fn the_cases_pool_and_the_memo_answer_only_for_the_build_in_use() {
    let s = Scratch::new("cases-relaid");
    let model = "PoolRelaid";
    let answer = |build: &str| {
        answering(
            &format!("\"ok\":true,\"steps\":2,\"warnings\":[\"answered by build {build}\"]"),
            "",
        )
    };
    let ty = cases_stand_in(&s.home(), model, &answer("a"));
    let spec = format!("---- MODULE {model} ----\n====\n");
    let ask = |n: u32| {
        ty_trace_verdict(
            &ty,
            model,
            spec.clone(),
            "C\n".into(),
            x_trace(model, n),
            model,
        )
    };
    let by = |(ok, why): (bool, String), build: &str| {
        let named = format!("warning: answered by build {build}");
        assert!(
            ok && why.starts_with("ty (cases) ") && why.lines().any(|l| l == named),
            "want a co-process verdict of build {build}: {why}"
        );
    };
    by(ask(0), "a");
    by(ask(1), "a");
    by(ask(0), "a");
    assert_eq!(spawns(&s.home()), 1);

    let relaid = s.root.path().join("relaid");
    std::fs::create_dir_all(&relaid).unwrap();
    let staged = cases_stand_in(&relaid, model, &answer("bb"));
    std::fs::rename(&staged, &ty).unwrap();
    by(ask(0), "bb");
    by(ask(2), "bb");
    by(ask(1), "bb");
    assert_eq!(
        (spawns(&s.home()), spawns(&relaid)),
        (1, 1),
        "one co-process per build, and the old one is never asked again"
    );
}

/// A SCALAR model can never report [`NotRun`] — that is the fact
/// [`prove_and_catch_scalar`] converts into an unconditional
/// `Covered`, and the reason ~21 call sites are allowed to ignore the
/// toolchain's presence. Pinned here rather than reasoned about: if the
/// interpreter tier ever grew a bail-out, this goes red instead of those
/// sites quietly returning coverage they no longer have.
#[test]
fn a_scalar_model_can_never_report_not_run() {
    let m = ring_model();
    assert!(m.fn_vars.is_empty(), "the probe model must be scalar");
    let d = check_model_tiered(&m, "scalar-discharge probe");
    assert!(
        matches!(d, Ok(Covered::Interpreter | Covered::InterpreterAndTy)),
        "{d:?}"
    );
}

/// The five committed-dead `ConfigCatalogSnapshot` mutants are verified
/// negative controls: dial present, fire at Buggy=1, caught at Buggy=1.
#[test]
fn audit_accepts_config_catalog_snapshot_mutants() {
    let m = config_catalog_snapshot_model();
    assert_eq!(
        audit_dead_negative_controls(
            &m,
            &[
                "AdmitStaleTrail",
                "AdmitStaleKitty",
                "AdmitStaleTheme",
                "AdmitStaleSparkle",
                "PublishLiveUnadmitted",
            ],
        ),
        Ok(5)
    );
}

/// Transact's `BuggyCommit` — the original hand-audited exception — now
/// passes the machine-verified criterion instead.
#[test]
fn audit_accepts_transact_buggy_commit() {
    assert_eq!(
        audit_dead_negative_controls(&transact_model(), &["BuggyCommit"]),
        Ok(1)
    );
}

/// A strictly non-vacuous model audits clean with an empty reported set.
#[test]
fn audit_accepts_strictly_nonvacuous_model() {
    assert_eq!(audit_dead_negative_controls(&ring_model(), &[]), Ok(0));
}

/// Tier agreement is required in BOTH directions: an empty ty report while
/// the interpreter finds dead actions fails (this is the fail-open guard —
/// a ty output-format drift cannot silently grant the relaxation), and an
/// action ty names that the model does not declare fails.
#[test]
fn audit_rejects_tier_disagreement_and_unknown_actions() {
    let m = config_catalog_snapshot_model();
    let under_reported = audit_dead_negative_controls(&m, &[]);
    assert!(
        under_reported
            .as_ref()
            .is_err_and(|e| e.contains("TIER DISAGREEMENT")),
        "{under_reported:?}"
    );
    let unknown = audit_dead_negative_controls(&m, &["NoSuchAction"]);
    assert!(
        unknown
            .as_ref()
            .is_err_and(|e| e.contains("does not declare")),
        "{unknown:?}"
    );
}

/// A dead action in a model whose `Buggy` is not committed to 0 has no
/// catch config — REAL vacuity, rejected.
#[test]
fn audit_rejects_dead_actions_without_committed_buggy_dial() {
    let m = interp::with_consts(&config_catalog_snapshot_model(), &[("Buggy", 2)]);
    let r = audit_dead_negative_controls(
        &m,
        &[
            "AdmitStaleTrail",
            "AdmitStaleKitty",
            "AdmitStaleTheme",
            "AdmitStaleSparkle",
            "PublishLiveUnadmitted",
        ],
    );
    assert!(
        r.as_ref()
            .is_err_and(|e| e.contains("no committed `Buggy = 0` dial")),
        "{r:?}"
    );
}

/// An action dead at BOTH configs (a typo'd guard, not a mutant) is a REAL
/// vacuity, and a Buggy=1 space no invariant catches proves nothing — both
/// rejected.
#[test]
fn audit_rejects_never_firing_and_uncaught_mutants() {
    // `Stuck` cannot fire at any config: its guard needs count > Cap while
    // count never exceeds Cap.
    let never = ty_model! {
        NeverFires {
            const Buggy = 0;
            const Cap = 2;
            var count = 0;
            action Step when (count <= Cap - 1) { count = count + 1; }
            action Stuck when (Buggy == 1 && count == Cap + 1) { count = 0; }
            invariant Bounded: count <= Cap;
        }
    };
    let r = audit_dead_negative_controls(&never, &["Stuck"]);
    assert!(
        r.as_ref()
            .is_err_and(|e| e.contains("never fires at Buggy=1")),
        "{r:?}"
    );

    // `Harmless` fires at Buggy=1 but violates nothing there — the "mutant"
    // is caught by no invariant, so it proves nothing.
    let uncaught = ty_model! {
        UncaughtMutant {
            const Buggy = 0;
            const Cap = 2;
            var count = 0;
            action Step when (count <= Cap - 1) { count = count + 1; }
            action Harmless when (Buggy == 1 && count == 0) { count = 1; }
            invariant Bounded: count <= Cap;
        }
    };
    let r = audit_dead_negative_controls(&uncaught, &["Harmless"]);
    assert!(
        r.as_ref().is_err_and(|e| e.contains("caught by nothing")),
        "{r:?}"
    );
}

/// One caught mutant must not launder a second harmless mutant through a
/// shared Buggy=1 counterexample. Each committed-dead action earns the
/// strict-vacuity relaxation only when it independently turns the safe
/// all-live baseline into an invariant violation.
#[test]
fn audit_rejects_two_mutants_when_only_one_is_independently_caught() {
    let partly_vacuous = ty_model! {
        PartlyVacuousMutants {
            const Buggy = 0;
            const Cap = 1;
            var count = 0;
            action Step when (count <= Cap - 1) { count = count + 1; }
            action Reset when (count == Cap) { count = 0; }
            action Caught when (Buggy == 1 && count == 0) { count = Cap + 1; }
            action Harmless when (Buggy == 1 && count == 0) { count = Cap; }
            invariant Bounded: count <= Cap;
        }
    };
    let r = audit_dead_negative_controls(&partly_vacuous, &["Caught", "Harmless"]);
    assert!(
        r.as_ref().is_err_and(|e| {
            e.contains("Harmless") && e.contains("independently caught by nothing")
        }),
        "{r:?}"
    );
}

/// The state count is read from `ty`'s real statistics block, and ONLY from
/// it. `None` on drift is the whole point: the caller turns `None` into a
/// failure, so a `ty` whose output format moves takes the gate RED rather
/// than silently retiring the explored-space obligation.
#[test]
fn ty_state_count_is_parsed_or_refused() {
    let real = "Model checking complete: No errors found (exhaustive).\n\n\
                Statistics:\n  States found: 128\n  Initial states: 1\n  Transitions: 587\n";
    assert_eq!(ty_states_explored(real), Some(128));
    assert_eq!(
        ty_states_explored("Statistics:\n  States found: 1\n"),
        Some(1)
    );
    // Drift / no statistics block / non-numeric — all refusals, not zeros.
    assert_eq!(ty_states_explored("No errors found (exhaustive)."), None);
    assert_eq!(ty_states_explored("  States explored: 128\n"), None);
    assert_eq!(ty_states_explored("  States found: many\n"), None);
}

/// The guard must not fail OPEN on the one input that matters most: a clean
/// `ty` verdict over a model the interpreter knows is violated.
///
/// This looks like a case the caller already reports — it is not. The
/// caller's report is `ty`'s exit status, and the whole premise of this guard
/// is that the discovered `ty` returns clean/exit-0 on spaces it never
/// entered. Returning early here would let exactly that combination through.
#[test]
fn a_clean_ty_verdict_over_a_violated_model_is_a_false_proof_not_a_coverage_gap() {
    // The guard forces `Buggy = 0` itself, so the violation has to come from
    // somewhere it does not override. `LenBounded` is `seq - lo + 1 <= Cap`,
    // and the ring starts at `seq = 0, lo = 1` — so `Cap = -1` is violated in
    // the INITIAL state, which is as committed as a config gets.
    let m = interp::with_consts(&ring_model(), &[("Cap", -1)]);
    assert!(
        interp::bmc(&interp::with_buggy(&m, 0)).is_err(),
        "fixture must be a model whose committed config is violated"
    );
    let panicked = std::panic::catch_unwind(|| {
        assert_ty_saw_whole_space(
            &m,
            "Model checking complete: No errors found (exhaustive).\n\
             Statistics:\n  States found: 1\n",
            "false-proof guard",
        );
    });
    assert!(
        panicked.is_err(),
        "a clean ty verdict over a violated committed config must never pass silently"
    );
}

/// The checker-identity header rides along with the transcript, so it must be
/// invisible to everything that reads one.
#[test]
fn the_ty_evidence_header_is_invisible_to_the_transcript_parsers() {
    let body = "Model checking complete: No errors found (exhaustive).\n\
                WARNING: 2 dead action(s) (never fired): Alpha, Beta\n\
                Statistics:\n  States found: 128\n";
    let header = ty_evidence_header(Path::new("/some/where/ty"));
    assert!(header.contains("/some/where/ty"), "names the binary");
    assert!(header.ends_with('\n'), "must not run into the first line");
    let with = format!("{header}{body}");
    assert_eq!(
        ty_states_explored(&with),
        ty_states_explored(body),
        "the header must not disturb the state count"
    );
    assert_eq!(ty_states_explored(&with), Some(128));
    // And the dead-action marker still resolves to the same first match.
    const MARKER: &str = "dead action(s) (never fired): ";
    let pick = |t: &str| -> Option<String> {
        t.lines()
            .find_map(|l| l.find(MARKER).map(|i| l[i + MARKER.len()..].to_string()))
    };
    assert_eq!(pick(&with), pick(body));
    assert_eq!(pick(&with).as_deref(), Some("Alpha, Beta"));
}

/// `spec_xref_closure` once hand-rolled its own `ty` flags without
/// `--no-auto-por`, and on `RainbowJumpBurstLifecycle` (128 reachable states
/// explored as 1) four of its six actions "never fired" — a dead set that was
/// an artifact of the reduction. The arming is now shared
/// ([`arm_whole_space_check`]); this is the second lock: a dead set carries no
/// record of the space it was measured on, so the transcript must be shown to
/// have covered the whole space before anything in it is believed.
#[test]
fn a_dead_set_from_a_reduced_run_is_refused_by_the_space_guard() {
    let m = ring_model();
    let full = interp::bmc(&interp::with_buggy(&m, 0)).expect("committed config is clean");
    // The honest transcript passes.
    assert_ty_saw_whole_space(
        &m,
        &format!(
            "Model checking complete: No errors found (exhaustive).\n\
             Statistics:\n  States found: {full}\n"
        ),
        "whole-space guard",
    );
    // The reduced one — ty's real shape when POR collapses the space — does not.
    let reduced = std::panic::catch_unwind(|| {
        assert_ty_saw_whole_space(
            &m,
            "POR: 1/1 states reduced (100.0%), 1 actions skipped\n\
             Model checking complete: No errors found (exhaustive).\n\
             Statistics:\n  States found: 1\n",
            "whole-space guard",
        );
    });
    assert!(
        reduced.is_err(),
        "a dead set measured on a 1-state reduction of a {full}-state model must be refused"
    );
}

/// A `ty` once reported "No errors found (exhaustive)" for
/// `RainbowJumpBurstLifecycle` after exploring ONE of its 128 reachable states
/// (a singleton ample set formed from the stutter action). The verdicts agreed;
/// the work behind them did not. This pins the CONSEQUENCE rather than the
/// cause: a smaller `ty` count is a false proof and must panic, whatever any
/// `ty` build's reduction does.
#[test]
#[should_panic(expected = "TIER DISAGREEMENT ON THE EXPLORED SPACE")]
fn a_ty_verdict_from_a_smaller_space_is_not_a_proof() {
    let m = ring_model();
    assert_same_space_explored(
        &m,
        128,
        "--- ty stdout ---\nModel checking complete: No errors found (exhaustive).\n\
         Statistics:\n  States found: 1\n",
        "explored-space regression",
    );
}

/// The other direction is a failure too, and for a different reason: a `ty`
/// count LARGER than the interpreter's means the emitted TLA+ admits states
/// the model does not, so the two tiers are not checking the same machine.
/// Neither direction may be waved through as "close enough".
#[test]
#[should_panic(expected = "TIER DISAGREEMENT ON THE EXPLORED SPACE")]
fn a_ty_verdict_from_a_larger_space_is_not_a_proof_either() {
    let m = ring_model();
    assert_same_space_explored(
        &m,
        128,
        "Statistics:\n  States found: 200\n",
        "explored-space regression",
    );
}

/// A clean verdict on an unparseable transcript credits nothing.
#[test]
#[should_panic(expected = "no evidence it explored anything")]
fn a_clean_verdict_without_a_state_count_credits_nothing() {
    let m = ring_model();
    assert_same_space_explored(&m, 128, "No errors found.", "explored-space regression");
}
