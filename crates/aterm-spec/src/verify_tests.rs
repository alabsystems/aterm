// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

use super::is_pending_stub;

/// A `ty` search a resource limit stopped is never a verdict: exit 5, or a
/// `Model checking stopped:` line under exit 0 (the shape a memory budget
/// shrunk by concurrent processes takes), reads as stopped; a clean run and a
/// refutation do not.
#[cfg(unix)]
#[test]
fn a_stopped_search_is_never_a_verdict() {
    use std::os::unix::process::ExitStatusExt as _;
    let run = |code: i32, stdout: &str| std::process::Output {
        status: std::process::ExitStatus::from_raw(code << 8),
        stdout: stdout.as_bytes().to_vec(),
        stderr: Vec::new(),
    };
    assert!(super::stopped_early(&run(
        0,
        "States found: 2039\nModel checking stopped: memory limit reached\n"
    )));
    assert!(super::stopped_early(&run(5, "")));
    assert!(!super::stopped_early(&run(
        0,
        "No errors found (exhaustive).\nStates found: 43770\n"
    )));
    assert!(!super::stopped_early(&run(
        1,
        "Error: Invariant DesiredIsLatest is violated.\n"
    )));
}

/// The per-invariant form of [`super::uncaught_invariants`], an isolated `bmc` per invariant at
/// `Buggy = 1`: the oracle the one-walk form is held to.
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

/// The one-walk ghost sweep returns exactly what one isolated walk per invariant returns: the
/// same set in the same order, and a panic where the per-invariant form panics (a function-valued
/// model).
///
/// The sample is chosen by outcome, not name: registry models are filed by what the sweep says
/// (an uncaught invariant, fully caught, or uninterpretable) and the first `PER_OUTCOME` of each
/// are held to the oracle, so paying a ghost down never weakens the test (`tests/non_vacuity.rs`
/// checks the whole registry). Two controls built from the first fully-caught model cover every
/// outcome whatever the census: it plus a tautology (one ghost beside caught invariants, so the
/// walk runs to the end), and it plus a function-valued invariant (both forms must panic).
#[test]
fn the_one_walk_ghost_sweep_matches_the_per_invariant_oracle() {
    use std::panic::{AssertUnwindSafe, catch_unwind};

    use crate::derive::{Expr, Invariant};

    /// Models of each outcome held to the oracle. The oracle costs one walk per invariant, so the
    /// sample is a few of each kind.
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
        .expect("no registry model is fully caught — the sweep's census is broken");
    let control = |name: &'static str, expr: Expr| {
        assert!(
            base.invariants.iter().all(|inv| inv.name != name),
            "{}: already has an invariant named {name}",
            base.name
        );
        let mut m = (*base).clone();
        m.invariants.push(Invariant {
            name,
            expr,
            kind: crate::derive::InvariantKind::Claim,
        });
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

/// A pending-program stub on PATH is not the tool: discovery skips it, or a harness would read its
/// refusal as tool output.
#[test]
fn a_pending_stub_is_not_the_tool() {
    let dir = std::env::temp_dir().join(format!("aterm_stub_probe_{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("mk probe dir");
    let stub = dir.join("ty-probe");
    std::fs::write(
        &stub,
        "#!/bin/sh\n# atpkg pending-program stub v1\n# Replaced by the real shim when the program installs.\nexit 127\n",
    )
    .expect("write stub");
    assert!(is_pending_stub(&stub), "the stub names itself");

    let real = dir.join("ty-real");
    std::fs::write(&real, b"\x7fELF fake binary bytes").expect("write real");
    assert!(!is_pending_stub(&real), "a real binary is not a stub");

    let missing = dir.join("nope");
    assert!(
        !is_pending_stub(&missing),
        "unreadable candidates stay eligible — a false pending would hide an installed tool"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

use super::*;
use crate::test_models::{config_catalog_snapshot_model, ring_model};
use crate::ty_model;

/// A dangling store shim never satisfies discovery; a live one (a symlink or an exec stub)
/// resolves to the real store target (what the caller will execute), not the shim.
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
    // A tombstone (the refusal script atpkg writes for a yanked build) reads as absent, or a
    // revoked build would shadow a working tool on PATH.
    std::fs::remove_file(&shim).unwrap();
    std::fs::write(&shim, b"#!/bin/sh\necho revoked >&2\nexit 1\n").unwrap();
    assert_eq!(
        resolve_store_shim(&shim),
        None,
        "a tombstone shim must never satisfy discovery"
    );
    // Live shim → the resolved store path.
    std::fs::remove_file(&shim).unwrap();
    let target = d.join("real-ty");
    std::fs::write(&target, b"#!/bin/sh\n").unwrap();
    std::os::unix::fs::symlink(&target, &shim).unwrap();
    let got = resolve_store_shim(&shim).expect("live shim resolves");
    assert_eq!(got, std::fs::canonicalize(&target).unwrap());
    // The shape atpkg lays: a `#!/bin/sh` exec stub whose first `exec '<path>' "$@"` line is the
    // target, here a path with a quote in it.
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
    // A stub naming a reclaimed build is absent, and so is one naming a relative path, which
    // would resolve against the cwd (where `Cargo.toml` is a real file).
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
// Discovery. Every test below runs over a temp home and store prefix through the explicit-env
// resolver, so none reads or mutates the process environment or depends on what is installed.
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
}

/// The order: the store shim beats PATH, and a developer tree under home (a cargo build, a
/// bootstrap stage) is never a candidate: a stale one would shadow the store.
#[cfg(unix)]
#[test]
fn discovery_order_is_store_then_path_and_no_developer_tree() {
    let s = Scratch::new("order");
    s.file("home/trust/first-party/ty/target/release/ty");
    s.file("home/trust/build/host/stage1/bin/ty");
    assert_eq!(s.locate("ty", None), None, "no developer tree is probed");

    let on_path = s.file("pathdir/ty");
    let path_var = std::ffi::OsString::from(on_path.parent().unwrap());
    let got = s.locate("ty", Some(&path_var)).unwrap();
    assert_eq!((got.origin, got.path), (TrustBinOrigin::Path, on_path));

    let store = s.store_copy("ty", "1123", "ty");
    s.store_copy("trust", "9192", "ty");
    std::os::unix::fs::symlink(&store, s.store_bin().join("ty")).unwrap();
    let got = s.locate("ty", Some(&path_var)).unwrap();
    assert_eq!((got.origin, got.path), (TrustBinOrigin::Store, store));
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
}

/// A probe that never answers is killed at the bound it was given, and is no
/// answer: a wedged `ty --version` cannot hang the suite. The wedged stand-in
/// lives 300 s (`exec`, so the kill reaches the sleep itself) under a 300 ms
/// bound, so the minute hang detector still tells killed from waited out.
#[cfg(unix)]
#[test]
fn a_wedged_probe_is_killed_at_its_bound() {
    use std::time::{Duration, Instant};
    let s = Scratch::new("probe-wedged");
    let wedged = stand_in(&s.home(), "exec sleep 300\n");
    let began = Instant::now();
    assert!(
        probe_output_within(&wedged, &[], Duration::from_millis(300)).is_none(),
        "a probe past its bound is None"
    );
    assert!(
        began.elapsed() < crate::wait::MUST_HAPPEN,
        "killed at the bound, not waited out (the stand-in lives 300 s)"
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

/// A stand-in `ty` for the `--cases` pool (A10): it advertises the cases mode, runs `cases` (shell)
/// as `trace validate --cases -`, logging each such spawn to `<dir>/spawns`, and answers the
/// per-process form with `per-process` and exit 1, a refusal, so a case the pool did not answer is
/// visibly the fallback's. A child that must die abnormally dies by SIGKILL.
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

/// A request with no verdict line has no verdict, never an accept. A child that closes stdout,
/// echoes another request's id (with `ok: true`), or writes a non-verdict line leaves the case to
/// the per-process fallback, which here refuses.
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

/// The `ty` verdict memo replays a verdict only for the same rendered files asked of the same
/// binary. A `ty` rewritten in place is asked again, even at the same length with its mtime put
/// back; so is one whose path was repointed while it ran, even once the path names the first
/// binary again; and a binary that cannot be stat'ed is never memoized.
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

/// A PATH `ty` that is a wrapper is the checker its target is: when only the binary it execs is
/// rebuilt, the memo asks again, and the checker handed to the run (which keys the `--cases` pool)
/// is a new one naming the binary behind the wrapper.
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

/// The `--cases` pool and the verdict memo answer only for the build the provenance line names:
/// once `ty` is re-laid at its path by rename (the old build's live child keeps its own file), an
/// answered step is asked again and a new step of the same (spec, cfg) goes to a child of the new
/// build.
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

/// The five committed-dead `ConfigCatalogSnapshot` mutants are verified
/// negative controls (dial present, fire at Buggy=1, caught at Buggy=1), and the
/// relaxation fails closed: an empty `ty` dead set while the interpreter finds
/// dead actions (a `ty` output-format drift cannot silently grant it), an action
/// the model does not declare, and a model whose `Buggy` is not committed to 0
/// (no catch config, so real vacuity) are each refused.
#[test]
fn audit_dead_negative_controls_needs_tier_agreement_and_a_committed_dial() {
    let m = config_catalog_snapshot_model();
    let dead = [
        "AdmitStaleTrail",
        "AdmitStaleKitty",
        "AdmitStaleTheme",
        "AdmitStaleSparkle",
        "PublishLiveUnadmitted",
    ];
    assert_eq!(
        audit_dead_negative_controls(std::slice::from_ref(&m)),
        Ok(5)
    );
    assert_eq!(dead_actions_agree(&m, &dead).map(|d| d.len()), Ok(5));
    let uncommitted = interp::with_consts(&m, &[("Buggy", 2)]);
    for (case, verdict, refusal) in [
        (
            "under-reported",
            dead_actions_agree(&m, &[]).map(|d| d.len()),
            "TIER DISAGREEMENT",
        ),
        (
            "unknown action",
            dead_actions_agree(&m, &["NoSuchAction"]).map(|d| d.len()),
            "does not declare",
        ),
        (
            "uncommitted dial",
            audit_dead_negative_controls(&[uncommitted]),
            "no committed `Buggy = 0` dial",
        ),
    ] {
        assert!(
            verdict.as_ref().is_err_and(|e| e.contains(refusal)),
            "{case}: {verdict:?}"
        );
    }
}

/// An action that fires under no mutant dial either (a typo'd guard, not a mutant) is real
/// vacuity, and a mutant no invariant catches proves nothing: both are rejected.
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
    let r = audit_dead_negative_controls(&[never]);
    assert!(
        r.as_ref()
            .is_err_and(|e| e.contains("fires under no mutant dial")),
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
    let r = audit_dead_negative_controls(&[uncaught]);
    assert!(
        r.as_ref().is_err_and(|e| e.contains("caught by nothing")),
        "{r:?}"
    );
}

/// One caught mutant must not launder a second, harmless one through a shared `Buggy = 1`
/// counterexample: a dead action is credited only for a violation of its own.
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
    let r = audit_dead_negative_controls(&[partly_vacuous]);
    assert!(
        r.as_ref().is_err_and(|e| {
            e.contains("Harmless") && e.contains("independently caught by nothing")
        }),
        "{r:?}"
    );
}

/// A dead action that breaks a law from every lawful state it fires in is a violation step and is
/// credited, even where another `Buggy` branch (`Tab`) breaks a law too. One that fires only where
/// a law is already broken shows nothing of its own.
#[test]
fn audit_credits_a_violation_step_and_not_a_step_after_one() {
    let consequence = ty_model! {
        ConsequenceStep {
            const Buggy = 0;
            var sent = 0;
            var stray = 0;
            var tabbed = 0;
            var amended = 0;
            var wrong = 0;
            action Send when (sent == 0) { sent = 1; stray = if Buggy == 1 { 1 } else { stray }; }
            action Tab when (tabbed == 0) {
                tabbed = 1;
                amended = if Buggy == 1 { 1 } else { amended };
            }
            action ReadWrongly when (stray == 1 && wrong == 0) { wrong = 1; }
            invariant NeverAmends: amended == 0;
            invariant NeverReadWrongly: wrong == 0;
        }
    };
    assert_eq!(audit_dead_negative_controls(&[consequence]), Ok(1));

    // `NeverStray` breaks on the very step that enables `Shrug`, so every state `Shrug` fires in
    // is unlawful before it moves.
    let after = ty_model! {
        StepAfterAViolation {
            const Buggy = 0;
            var sent = 0;
            var stray = 0;
            var shrugged = 0;
            action Send when (sent == 0) { sent = 1; stray = if Buggy == 1 { 1 } else { stray }; }
            action Shrug when (stray == 1 && shrugged == 0) { shrugged = 1; }
            invariant NeverStray: stray == 0;
        }
    };
    let r = audit_dead_negative_controls(&[after]);
    assert!(
        r.as_ref()
            .is_err_and(|e| e.contains("Shrug") && e.contains("another branch")),
        "{r:?}"
    );
}

/// An action only a declared scenario reaches is not dead: it fires in a
/// configuration the real code runs under. Without the scenario it is.
#[test]
fn a_scenario_keeps_its_own_actions_alive() {
    let capped = ty_model! {
        CapWindow {
            const Buggy = 0;
            const Cap = 0;
            var used = 0;
            action Act when (Cap == 0 || used <= Cap - 1) {
                used = if Cap == 0 { used } else { used + 1 };
            }
            action WindowTurns when (used > 0) { used = 0; }
            bound UsedBounded: used <= 2;
        }
    };
    let scenario = interp::with_consts(&capped, &[("Cap", 2)]);
    let alone = audit_dead_negative_controls(std::slice::from_ref(&capped));
    assert!(
        alone
            .as_ref()
            .is_err_and(|e| e.contains("WindowTurns") && e.contains("no `scenario` declares")),
        "{alone:?}"
    );
    assert_eq!(audit_dead_negative_controls(&[capped, scenario]), Ok(0));
}

/// The state count is read from `ty`'s statistics block and only from it; on drift it is `None`,
/// which the caller turns into a failure.
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

/// The guard does not fail open on a clean `ty` verdict over a model the interpreter knows is
/// violated: the caller's report is `ty`'s exit status, which is exactly what cannot be trusted
/// here.
#[test]
fn a_clean_ty_verdict_over_a_violated_model_is_a_false_proof_not_a_coverage_gap() {
    // The guard forces `Buggy = 0` itself, so the violation must come from elsewhere:
    // `LenBounded` is `seq - lo + 1 <= Cap` and the ring starts at `seq = 0, lo = 1`, so
    // `Cap = -1` is violated in the initial state.
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

/// The explored-space law refuses every transcript that does not show `ty`
/// walking exactly the interpreter's space. A `ty` once reported "No errors
/// found (exhaustive)" for `RainbowJumpBurstLifecycle` after exploring ONE of
/// its 128 reachable states (a singleton ample set formed from the stutter
/// action): a SMALLER count is a false proof. A LARGER one means the emitted
/// TLA+ admits states the model does not, so the tiers are not checking the
/// same machine. A clean verdict with no count credits nothing.
#[test]
fn a_ty_verdict_over_another_space_is_not_a_proof() {
    let m = ring_model();
    for (case, transcript, refusal) in [
        (
            "smaller",
            "--- ty stdout ---\nModel checking complete: No errors found (exhaustive).\n\
             Statistics:\n  States found: 1\n",
            "TIER DISAGREEMENT ON THE EXPLORED SPACE",
        ),
        (
            "larger",
            "Statistics:\n  States found: 200\n",
            "TIER DISAGREEMENT ON THE EXPLORED SPACE",
        ),
        (
            "no count",
            "No errors found.",
            "no evidence it explored anything",
        ),
    ] {
        let panic = std::panic::catch_unwind(|| {
            assert_same_space_explored(&m, 128, transcript, "explored-space regression");
        })
        .expect_err(case);
        let message = panic
            .downcast_ref::<String>()
            .map(String::as_str)
            .or_else(|| panic.downcast_ref::<&str>().copied())
            .unwrap_or_default();
        assert!(message.contains(refusal), "{case}: {message}");
    }
}
