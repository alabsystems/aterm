// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The harness's own self-tests, in ONE binary.
//!
//! `harness/mod.rs` is mounted by every e2e suite in this directory, so a
//! `#[test]` there ran once per mounting binary — nineteen times per gate run
//! for five tests. They live here instead and run once. What they guard is the
//! harness's own honesty: the fixture config that isolates a GUI from the
//! machine, the process-age reader the dirty-machine guard depends on, where a
//! found binary is looked for, and the staleness guard that refuses a binary
//! older than its sources.

mod harness;

use std::path::PathBuf;
use std::process::Command;
use std::time::Duration;

use harness::{
    depfile_inputs, is_stray, newest_input, prepare_fixture_config, prepare_gui_environment,
    process_age_secs, process_command, process_exe, target_dirs, target_root, this_checkouts,
    workspace_root, FIXTURE_GUI_CONFIG,
};

#[test]
fn gui_fixture_defaults_disable_machine_writes_and_preserve_explicit_edits() {
    let dir = std::env::temp_dir().join(format!("atl-isolation-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    prepare_fixture_config(&dir);
    let path = dir.join("cfg/aterm/aterm.toml");
    let text = std::fs::read_to_string(&path).expect("fixture config");
    let config: aterm_toml::Table = aterm_toml::from_str(&text).expect("valid fixture config");
    assert_eq!(
        config.get("agents_auto_prime").and_then(|v| v.as_bool()),
        Some(false)
    );
    let packages = config.get("packages").and_then(|v| v.as_table()).unwrap();
    let machine = config.get("machine").and_then(|v| v.as_table()).unwrap();
    assert_eq!(
        packages.get("enabled").and_then(|v| v.as_bool()),
        Some(false)
    );
    assert_eq!(
        machine.get("spotlight_noindex").and_then(|v| v.as_bool()),
        Some(false)
    );
    assert_eq!(
        machine.get("universal_control").and_then(|v| v.as_str()),
        Some("leave")
    );

    let edited = format!("{text}\n[fabric]\npresence = \"minimal\"\n");
    std::fs::write(&path, &edited).expect("intentional fixture edit");
    prepare_fixture_config(&dir);
    assert_eq!(std::fs::read_to_string(&path).unwrap(), edited);

    // The updater is off through the config, not the environment (2026-09-23).
    let update = aterm_toml::from_str::<aterm_toml::Table>(FIXTURE_GUI_CONFIG)
        .expect("fixture config")
        .get("update")
        .and_then(|v| v.as_table())
        .cloned()
        .expect("[update]");
    assert_eq!(update.get("enabled").and_then(|v| v.as_bool()), Some(false));

    // It must run first on a fresh Command (its own doc says why), so it can only
    // REMOVE: every inherited ATERM_* is removed, and nothing is set in its place.
    let mut command = Command::new("unused-fixture-program");
    prepare_gui_environment(&mut command);
    assert!(
        command
            .get_envs()
            .filter(|(key, _)| key.to_string_lossy().starts_with("ATERM_"))
            .all(|(_, value)| value.is_none()),
        "the baseline sets no ATERM_* variable: isolation is the fixture config"
    );
    std::fs::remove_dir_all(dir).expect("remove owned fixture");
}

/// The age reader must be able to read OUR OWN age.
///
/// This is the test that was missing. `refuse_a_dirty_machine` degrades to silence when
/// it cannot ask the question — correct behaviour, and precisely what hid a reader that
/// could never answer. So assert the reader directly against a process that certainly
/// exists: ourselves. If `ps` loses the `etime` keyword, or its format shifts, this fails
/// loudly instead of quietly disarming the guard.
#[test]
fn self_check_age_reader() {
    let me = std::process::id().to_string();
    let age = process_age_secs(&me).unwrap_or_else(|| {
        panic!("cannot read this process's own age; the dirty-machine guard is disarmed")
    });
    assert!(
        (0..86_400).contains(&age),
        "implausible age {age}s for the running test process"
    );
    assert_eq!(
        process_age_secs("0"),
        None,
        "pid 0 is not ours to see; the reader must answer None, not a bogus age"
    );
}

/// The binary lookup searches the target dir THIS test was built into first, so a
/// workspace built with `CARGO_TARGET_DIR` finds its own fresh binaries (2026-09-12:
/// every e2e suite here was red under a custom target dir because only `<root>/target`
/// was searched).
#[test]
fn the_binary_lookup_searches_the_target_dir_this_test_was_built_into_first() {
    let exe = std::env::current_exe().expect("current_exe");
    let dirs = target_dirs();
    let first = dirs.first().expect("at least one target dir");
    assert!(
        exe.starts_with(first),
        "{} is not under the first searched dir {}",
        exe.display(),
        first.display()
    );
    assert!(
        dirs.contains(&workspace_root().join("target")),
        "<root>/target is still searched as the last resort"
    );
    let mut sorted = dirs.clone();
    sorted.dedup();
    assert_eq!(
        sorted.len(),
        dirs.len(),
        "no dir is searched twice: {dirs:?}"
    );
}

/// THE STALENESS GUARD MUST SEE EVERY CRATE IN THE BINARY, not one.
///
/// It used to walk `crates/aterm-gui/src` alone, so an edit to `crates/aterm-uds` or
/// `crates/aterm-types` — both compiled into aterm-gui, both audited by the round the
/// guard was written for — left it silent and the whole e2e suite drove the previous
/// binary. This drives [`newest_input`] against a SYNTHETIC tree: a fake binary, cargo's
/// depfile beside it, and the newest input in a crate that is not aterm-gui.
///
/// Deterministic: the mtimes are SET, not raced. No sleep, no clock comparison against
/// wall time.
#[test]
fn the_stale_guard_sees_every_crate_the_binary_was_built_from() {
    let dir = std::env::temp_dir().join(format!("atl-stale-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let uds = dir.join("crates/aterm-uds/src");
    let gui = dir.join("crates/aterm-gui/src");
    let git = dir.join(".git");
    let out = dir.join("target/debug/build/x/out");
    for d in [&uds, &gui, &git, &out] {
        std::fs::create_dir_all(d).expect("scratch tree");
    }
    let bin = dir.join("target/debug/aterm-gui");
    let spawnfd = uds.join("spawnfd.rs");
    let app = gui.join("app.rs");
    let index = git.join("index");
    let generated = out.join("join_table.rs");
    for f in [&bin, &spawnfd, &app, &index, &generated] {
        std::fs::write(f, b"x").expect("write");
    }

    // The binary is one hour old; aterm-gui's own source is two hours old; the aterm-uds
    // source, the git stamp and the generated file are all NEWER than the binary.
    let hour = Duration::from_secs(3600);
    let now = std::time::SystemTime::now();
    set_mtime(&bin, now - hour);
    set_mtime(&app, now - hour - hour);
    set_mtime(&spawnfd, now);
    set_mtime(&index, now);
    set_mtime(&generated, now);

    std::fs::write(
        bin.with_extension("d"),
        format!(
            "{}: {} {} {} {}\n{}:\n",
            bin.display(),
            app.display(),
            spawnfd.display(),
            index.display(),
            generated.display(),
            app.display(),
        ),
    )
    .expect("depfile");

    let (_, newest) = newest_input(&bin, "aterm-gui").expect("the depfile names inputs");
    assert_eq!(
        newest, spawnfd,
        "a source in a crate that is NOT aterm-gui must be able to make the binary \
         stale; `.git` stamps and OUT_DIR generated files must not be the answer"
    );

    // THE TARGET IS NOT AN INPUT, and neither stamp class is.
    let inputs = depfile_inputs(
        &std::fs::read_to_string(bin.with_extension("d")).expect("read"),
        &target_root(&bin),
    );
    assert_eq!(
        inputs,
        vec![app.clone(), spawnfd.clone()],
        "the rule's target, `.git/*` and anything under target/ are not sources"
    );

    // A path with an escaped space survives the split as ONE path.
    assert_eq!(
        depfile_inputs(
            "/a/bin: /a/one\\ two.rs /a/three.rs",
            std::path::Path::new("/a/target")
        ),
        vec![PathBuf::from("/a/one two.rs"), PathBuf::from("/a/three.rs")],
        "cargo escapes a space in a path as `\\ `"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

/// Set a file's modification time. `filetime` is not a dependency of this crate and one
/// would not be added for a test, so this is the `utimensat(2)` the crate would wrap.
fn set_mtime(path: &std::path::Path, when: std::time::SystemTime) {
    let secs = when
        .duration_since(std::time::UNIX_EPOCH)
        .expect("after the epoch")
        .as_secs();
    let status = Command::new("touch")
        .arg("-t")
        .arg(stamp(secs))
        .arg(path)
        .status()
        .expect("touch");
    assert!(status.success(), "touch {}", path.display());
}

/// `touch -t`'s `[[CC]YY]MMDDhhmm[.ss]`, computed from a Unix second so the test needs
/// no date library and no locale.
fn stamp(secs: u64) -> String {
    let out = Command::new("date")
        .args(["-r", &secs.to_string(), "+%Y%m%d%H%M.%S"])
        .output()
        .expect("date -r");
    assert!(out.status.success(), "date -r {secs}");
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

/// The executable reader names the binary a process RUNS, not the words on its command
/// line — as an ABSOLUTE PATH, so the stray guard can tell whose build it is, however
/// the process was started: a shell whose script mentions `aterm-gui --headless` is a
/// shell, and one started by its bare name (a PATH search, which `ps -o comm=` answers
/// with the bare name) is read as the file it runs. Checked against two processes that
/// certainly exist — this test binary (one of this checkout's builds), and a `sh` child
/// whose arguments carry the daemon's pattern — so a reader that stops answering fails
/// here instead of silently disarming the stray guard.
#[test]
fn self_check_executable_reader() {
    let me = std::process::id().to_string();
    let exe = std::env::current_exe().expect("this test binary");
    let read = process_exe(&me).expect("the reader must read this process");
    assert_eq!(
        std::fs::canonicalize(&read).expect("the path read resolves"),
        std::fs::canonicalize(&exe).expect("this test binary resolves"),
        "the reader must name this test binary: {}",
        read.display()
    );
    assert!(
        this_checkouts(&read),
        "this test binary is this checkout's build: {} under {:?}",
        read.display(),
        target_dirs()
    );
    // Keep the shell blocked in its own builtin, with the daemon mention in
    // its command line. Piped stdin stays open until cleanup; no child escapes.
    let mut mention = std::process::Command::new("sh")
        .args(["-c", "read -r _; : aterm-gui --headless"])
        .stdin(std::process::Stdio::piped())
        .spawn()
        .expect("spawn a shell that names the daemon");
    let pid = mention.id().to_string();
    let line = process_command(&pid);
    let named = process_exe(&pid);
    let _ = mention.kill();
    let _ = mention.wait();
    assert!(
        line.contains("aterm-gui --headless"),
        "the probe's command line names the pattern, as `pgrep -f` sees it: {line}"
    );
    let named = named.expect("the reader must read a live child");
    assert!(
        named.is_absolute(),
        "a child started by its bare name is read as the file it runs: {}",
        named.display()
    );
    assert_ne!(
        named.file_name().and_then(|n| n.to_str()),
        Some("aterm-gui"),
        "a shell that only names `aterm-gui --headless` is not the daemon"
    );
    assert_eq!(
        process_exe("0"),
        None,
        "pid 0 is not ours to see; the reader must answer None"
    );
}

/// A STRAY IS THIS CHECKOUT'S (the stray guard, 2026-09-27): another worktree's live
/// suite runs its own `aterm-gui --headless` and `aterm-link serve`, older than this
/// process, from its OWN `target/` — and until this day the guard, reading only the
/// executable's name, answered COULD NOT RUN on them, so parallel sessions ran with the
/// guard switched off. Over the rule itself ([`is_stray`], the one `strays_now` applies to
/// every process `pgrep` matches) and the real ownership test ([`this_checkouts`]): the
/// same daemons, the same ages, built in a sibling checkout — or in a worktree NESTED in
/// this one, which is a checkout of its own — are nobody's here. NEGATIVE CONTROLS: this
/// checkout's own builds, older than this process, ARE strays — in the target dir this
/// test was built into and in any other lane's beside it (`target-gui/`: a stray a run
/// into another target dir left, which the 2026-09-27 review found this rule had
/// stopped catching) — the guard is not disarmed; while one younger than this process
/// (this run's own), a shell naming the pattern, and a process whose executable cannot
/// be read are not.
#[test]
fn another_checkouts_daemons_are_never_strays_here() {
    let root = workspace_root();
    let here = target_dirs()
        .into_iter()
        .next()
        .expect("the target dir this test was built into");
    let other = root
        .parent()
        .expect("a checkout has a parent directory")
        .join(format!("atl-other-checkout-{}", std::process::id()))
        .join("target");
    // A worktree parked inside this checkout, in a directory `.gitignore` keeps out of
    // the tree (`/target-*`) and no target dir of this run names; removed before any
    // assertion, so a red leaves nothing behind in the checkout.
    let nested = root.join(format!("target-atl-nested-{}", std::process::id()));
    std::fs::create_dir_all(&nested).expect("the nested checkout");
    std::fs::write(nested.join(".git"), "gitdir: /nonexistent\n").expect("its .git file");
    let nested_read: Vec<(bool, bool)> = ["aterm-gui", "aterm-link"]
        .iter()
        .map(|binary| {
            let exe = nested.join("target/debug").join(binary);
            (
                this_checkouts(&exe),
                is_stray(binary, Some(&exe), Some(3_600), 100, this_checkouts),
            )
        })
        .collect();
    std::fs::remove_dir_all(&nested).expect("remove the nested checkout");
    assert_eq!(
        nested_read,
        [(false, false), (false, false)],
        "a worktree nested in this checkout is another checkout: {}",
        nested.display()
    );
    let (my_age, old, young) = (100, Some(3_600), Some(101));
    for binary in ["aterm-gui", "aterm-link"] {
        let ours = here.join("debug").join(binary);
        let lane = root.join("target-gui").join("rv-stray").join(binary);
        let theirs = other.join("debug").join(binary);
        assert!(this_checkouts(&ours), "{}", ours.display());
        assert!(this_checkouts(&lane), "{}", lane.display());
        assert!(!this_checkouts(&theirs), "{}", theirs.display());
        assert!(
            !is_stray(binary, Some(&theirs), old, my_age, this_checkouts),
            "another checkout's live {binary} is not a stray here: {}",
            theirs.display()
        );
        for exe in [&ours, &lane] {
            assert!(
                is_stray(binary, Some(exe), old, my_age, this_checkouts),
                "this checkout's older {binary} is a stray: {}",
                exe.display()
            );
        }
        assert!(
            !is_stray(binary, Some(&ours), young, my_age, this_checkouts),
            "one this run started is not"
        );
        assert!(
            !is_stray(
                binary,
                Some(std::path::Path::new("/bin/sh")),
                old,
                my_age,
                this_checkouts
            ),
            "a shell naming the pattern is not"
        );
        assert!(
            !is_stray(binary, None, old, my_age, this_checkouts),
            "a process that cannot be read is not guessed at"
        );
    }
}
