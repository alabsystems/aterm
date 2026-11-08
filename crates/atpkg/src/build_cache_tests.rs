// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::*;
use std::collections::HashMap;
use std::ffi::OsString;
#[cfg(unix)]
use std::time::{Duration, SystemTime};

struct Fixture {
    _dir: crate::testdir::Dir,
    root: PathBuf,
    home: PathBuf,
}

#[cfg(unix)]
#[test]
fn cleanup_reports_each_process_gate_keep_without_a_removal_intent() {
    use aterm_uds::holders::{PathScan, PathUse};
    for (scan, reason) in [
        (
            Ok(PathScan {
                users: vec![PathUse {
                    pid: 29,
                    how: "exe",
                    path: PathBuf::from("/fixture/program"),
                }],
                ..PathScan::default()
            }),
            KeepReason::ProcessUsed {
                pid: 29,
                how: "executable",
                observed: PathBuf::from("/fixture/program"),
            },
        ),
        (
            Ok(PathScan {
                unreadable: vec![(31, None)],
                ..PathScan::default()
            }),
            KeepReason::ProcessUnreadable(31),
        ),
        (
            Err(std::io::Error::other("fixture scan unavailable")),
            KeepReason::ProcessUnavailable,
        ),
    ] {
        let f = Fixture::new("build-cache-reports-process-keep");
        let root = cache_root(&f.home);
        let first = lay(&root, &f.home.join("gone-first"), 2 * DAY);
        let second = lay(&root, &f.home.join("gone-second"), 2 * DAY);
        let expected = [first.clone(), second.clone()];
        let processes = |_: &[PathBuf], _: &[u32]| match &scan {
            Ok(scan) => Ok(scan.clone()),
            Err(error) => Err(std::io::Error::new(error.kind(), "fixture unavailable")),
        };
        let fence = fence::Fence::new(&processes).with_reporting();
        let mut asked = Vec::new();
        let before = plan(
            &root,
            Some(&f.home),
            SystemTime::now(),
            ReclaimAfter::CALM,
            None,
        );
        assert_eq!(before.len(), 2, "the same entries are advisory candidates");
        let outcomes = reclaim_watched_on(
            &root,
            Some(&f.home),
            SystemTime::now(),
            ReclaimAfter::CALM,
            None,
            &mut |path, why| {
                asked.push((path.to_path_buf(), why));
                true
            },
            &fence,
        );
        assert!(
            outcomes.is_empty() && asked.is_empty(),
            "a safety refusal grants no removal intent"
        );
        let retained = fence.retained();
        assert_eq!(
            retained.len(),
            2,
            "every retained entry is named, not just the first warning"
        );
        for path in expected {
            assert!(retained.contains(&RetainedCheck {
                path: path.clone(),
                reason: reason.clone()
            }));
            assert!(path.join("debug/deps/x").is_file());
        }
    }
}

#[cfg(unix)]
#[test]
fn cleanup_reports_lock_and_custody_guards_without_weakening_them() {
    let f = Fixture::new("build-cache-reports-lock-keep");
    let root = cache_root(&f.home);
    let leaf = lay(&root, &f.home.join("gone"), 2 * DAY);
    std::fs::write(leaf.join("debug/.cargo-lock"), b"").unwrap();
    let lock = crate::metadata_io::open_regular(&leaf.join("debug/.cargo-lock")).unwrap();
    let held = crate::lock::Flock::try_lock(lock).unwrap();
    let processes = |_: &[PathBuf], _: &[u32]| Ok(aterm_uds::holders::PathScan::default());
    let fence = fence::Fence::new(&processes).with_reporting();
    let run = |fence: &fence::Fence<'_>| {
        reclaim_watched_on(
            &root,
            Some(&f.home),
            SystemTime::now(),
            ReclaimAfter::CALM,
            None,
            &mut |_, _| true,
            fence,
        )
    };
    assert!(run(&fence).is_empty());
    assert!(fence.retained().contains(&RetainedCheck {
        path: leaf.clone(),
        reason: KeepReason::CargoLocks
    }));
    assert!(leaf.join("debug/deps/x").is_file());
    drop(held);
    std::fs::write(
        leaf.join(registered::MARKER),
        b"unreadable custody declaration",
    )
    .unwrap();
    let fence = fence::Fence::new(&processes).with_reporting();
    assert!(run(&fence).is_empty());
    assert!(fence.retained().contains(&RetainedCheck {
        path: leaf.clone(),
        reason: KeepReason::Custody
    }));
    assert!(leaf.join("debug/deps/x").is_file());
    std::fs::remove_file(leaf.join(registered::MARKER)).unwrap();
    let fence = fence::Fence::new(&processes).with_reporting();
    let removed = run(&fence);
    assert_eq!(removed.len(), 1);
    assert_eq!(removed[0].0, leaf);
    assert!(matches!(removed[0].1, Unused::Gone { .. }));
    assert!(
        fence.retained().is_empty(),
        "a clear guard must not fabricate a keep"
    );
    assert!(!leaf.exists());
}

#[cfg(unix)]
#[test]
fn cleanup_reports_legacy_retirement_scope_without_reclassifying_the_cache() {
    let f = Fixture::new("build-cache-reports-legacy-scope");
    let root = cache_root(&f.home);
    let leaf = lay(&root, &f.home.join("gone"), 2 * DAY);
    let mut asked = false;
    let error = registered::retire(&f.home, &leaf, &mut |_, _| {
        asked = true;
        true
    })
    .unwrap_err();
    assert!(
        error.contains("machine cache clean"),
        "the refusal must name the applicable cleanup API: {error}"
    );
    assert!(error.contains("registered ordinary cache roots"));
    assert!(!asked);
    assert!(leaf.join("debug/deps/x").is_file());
}

#[test]
fn cleanup_reports_render_the_observed_reason_without_a_freed_space_claim() {
    let kept = RetainedCheck {
        path: PathBuf::from("/fixture/cache"),
        reason: KeepReason::ProcessUnreadable(31),
    };
    let words = kept.words();
    assert!(
        words.contains("/fixture/cache") && words.contains("process 31 could not be ruled out")
    );
    assert!(words.contains("nothing removed by this check"));
    assert!(!words.contains("freed"));
}

#[cfg(unix)]
#[test]
fn cleanup_reports_postrename_and_resumed_guard_subjects() {
    for resumed in [false, true] {
        for (allowed, reason) in [
            (false, KeepReason::PermissionChanged),
            (true, KeepReason::Custody),
        ] {
            let f = Fixture::new("build-cache-reports-stage-subject");
            let root = cache_root(&f.home);
            let original = lay(&root, &f.home.join("gone"), 2 * DAY);
            let stage = f.dir("stages").join(DOOMED);
            std::fs::rename(&original, &stage).unwrap();
            if allowed {
                // This original-path policy must still be read after the rename.
                // The stage's independent parent has no such custody marker.
                std::fs::write(
                    original.parent().unwrap().join(registered::MARKER),
                    b"unknown custody",
                )
                .unwrap();
                assert!(!registered::retention::allows_tree(&original));
                assert!(registered::retention::allows_walk(&stage));
            }
            let scans = std::cell::Cell::new(0);
            let processes = |_: &[PathBuf], _: &[u32]| {
                scans.set(scans.get() + 1);
                Ok(aterm_uds::holders::PathScan::default())
            };
            let fence = fence::Fence::new(&processes).with_reporting();
            let mut record = |_: &Path, _| true;
            let mut intents = Intents::new(&mut record);
            let why = if resumed {
                Unused::Unfinished
            } else {
                Unused::Gone { idle: 2 * DAY }
            };
            if resumed {
                assert!(intents.ask_staged(&stage, why));
            } else {
                assert!(intents.ask(&original, why));
                intents.moved(&stage);
            }
            let permitted = || allowed;
            {
                let mut sweep = Sweep {
                    root: &root,
                    home: Some(&f.home),
                    now: SystemTime::now(),
                    after: ReclaimAfter::CALM.into(),
                    placing: None,
                    moved: false,
                    compiler_store: None,
                    intents: &mut intents,
                    fence: &fence,
                    permitted: &permitted,
                    pruning: Vec::new(),
                    declined: false,
                };
                let went = if resumed {
                    stage.clone()
                } else {
                    original.clone()
                };
                assert!(!sweep.take(&stage, went, why, &original));
            }
            assert_eq!(
                scans.get(),
                0,
                "permission/custody still precede the process scan"
            );
            assert_eq!(
                intents.finish(),
                [(
                    stage.clone(),
                    Unused::Refused {
                        by: Refusal::Staged
                    }
                )]
            );
            assert_eq!(
                fence.retained(),
                [RetainedCheck {
                    path: stage.clone(),
                    reason
                }]
            );
            assert!(!original.exists());
            assert_eq!(
                std::fs::read(stage.join("debug/deps/x")).unwrap(),
                b"0123456789"
            );
        }
    }
}

#[cfg(unix)]
#[test]
fn cleanup_reports_absent_metadata_subjects_as_stopped_checks() {
    let f = Fixture::new("build-cache-reports-absent-subject");
    let original = f.home.join("absent-original");
    let pending = f.home.join("absent-pending");
    let processes = |_: &[PathBuf], _: &[u32]| -> std::io::Result<aterm_uds::holders::PathScan> {
        panic!("this is a metadata guard, not a process scan")
    };
    let fence = fence::Fence::new(&processes).with_reporting();
    assert!(!fence.admit(&original, KeepReason::RegistrationChanged, false));
    let observations = fence.retained();
    assert_eq!(
        observations,
        [RetainedCheck {
            path: original.clone(),
            reason: KeepReason::RegistrationChanged
        }]
    );
    assert!(!original.exists() && !pending.exists());
    let words = observations[0].words();
    assert!(words.contains("stopped") && !words.starts_with("kept "));
    assert!(words.contains("nothing removed by this check") && !words.contains("freed"));
}

#[cfg(unix)]
#[test]
fn cleanup_reports_prune_decline_and_no_staged_parts_without_an_intent() {
    for prior_decline in [true, false] {
        let f = Fixture::new("build-cache-reports-prune-stop");
        let root = cache_root(&f.home);
        let owner = f.dir("home/live-owner");
        std::fs::write(owner.join("Cargo.toml"), "[workspace]\n").unwrap();
        let leaf = build_dir(&root, &owner);
        let profile = leaf.join("debug");
        std::fs::create_dir_all(&profile).unwrap();
        std::fs::write(profile.join(".cargo-build-lock"), b"").unwrap();
        let incremental = profile.join("incremental/ws-1k116trr0a0xf");
        std::fs::create_dir_all(incremental.join("s-hneouhc0uf-1tn8o9j-2awpw")).unwrap();
        age_tree(&incremental, 4 * DAY);
        let permission_reads = std::cell::Cell::new(0);
        let permitted = || {
            assert!(
                !prior_decline,
                "an earlier pass decline still short-circuits this step"
            );
            let before = permission_reads.get();
            permission_reads.set(before + 1);
            before == 0
        };
        let processes = |_: &[PathBuf], _: &[u32]| Ok(aterm_uds::holders::PathScan::default());
        let fence = fence::Fence::new(&processes).with_reporting();
        let mut record =
            |_: &Path, _| -> bool { panic!("no current-leaf removal intent was reached") };
        let mut intents = Intents::new(&mut record);
        {
            let mut sweep = Sweep {
                root: &root,
                home: Some(&f.home),
                now: SystemTime::now(),
                after: ReclaimAfter::CALM.into(),
                placing: None,
                moved: false,
                compiler_store: None,
                intents: &mut intents,
                fence: &fence,
                permitted: &permitted,
                pruning: Vec::new(),
                declined: prior_decline,
            };
            assert!(!sweep.prune(leaf.parent().unwrap(), &leaf));
        }
        assert!(intents.finish().is_empty());
        let expected = if prior_decline {
            KeepReason::IntentDeclined
        } else {
            KeepReason::NoPartsStaged
        };
        let observations = fence.retained();
        assert!(observations.contains(&RetainedCheck {
            path: leaf.clone(),
            reason: expected
        }));
        assert!(
            observations
                .iter()
                .all(|one| one.reason != KeepReason::CannotStage)
        );
        assert_eq!(permission_reads.get(), if prior_decline { 0 } else { 2 });
        assert!(incremental.is_dir());
        assert!(stages_in(leaf.parent().unwrap(), OsStr::new(DOOMED)).is_empty());
    }
}

impl Fixture {
    fn new(label: &str) -> Self {
        // Ordinary placement needs an ordinary workspace even when TMPDIR is a .noindex lane.
        // The shared-root temporary-directory policy keeps this private scratch under .ubd.
        #[cfg(target_os = "macos")]
        let dir = crate::testdir::dir_in(Path::new("/tmp"), label);
        #[cfg(not(target_os = "macos"))]
        let dir = crate::testdir::dir(label);
        let root = std::fs::canonicalize(&*dir).unwrap();
        let home = root.join("home");
        // Private whatever the umask, as macOS makes a home, its `Library` and its `Caches`.
        #[cfg(unix)]
        {
            use std::os::unix::fs::DirBuilderExt as _;
            std::fs::DirBuilder::new()
                .recursive(true)
                .mode(0o700)
                .create(home.join("Library/Caches"))
                .unwrap();
        }
        #[cfg(not(unix))]
        std::fs::create_dir_all(home.join("Library/Caches")).unwrap();
        // As the shim's observed call lays them before any build makes the shared cache.
        registered::lay_shared(&home);
        Self {
            _dir: dir,
            root,
            home,
        }
    }

    fn dir(&self, rel: &str) -> PathBuf {
        let p = self.root.join(rel);
        std::fs::create_dir_all(&p).unwrap();
        p
    }

    #[cfg(unix)]
    fn file(&self, rel: &str, text: &str) {
        let p = self.root.join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, text).unwrap();
    }

    fn value(&self) -> String {
        config_value(&cache_root(&self.home)).unwrap()
    }
}

fn strings(args: &[&str]) -> Vec<String> {
    args.iter().map(|s| (*s).to_owned()).collect()
}

fn rust(
    home: &Path,
    cwd: &Path,
    env: &HashMap<&str, String>,
    args: &[String],
    tool: Tool,
) -> Vec<String> {
    let m = Machine {
        root: None,
        home: Some(home),
        cwd: Some(cwd),
        env: &|name| env.get(name).map(OsString::from),
        read: &read_config,
        writable: &writable,
    };
    match decide(args, tool, &m) {
        Some(BuildDir::TrustEnv(dir)) => std::iter::once(format!("env={dir}"))
            .chain(args.iter().cloned())
            .collect(),
        _ => match with_build_dir(args, tool, &m) {
            Some(placed) => loses_config(args, tool)
                .then(|| directory_from_placed_args(&placed).map(|dir| format!("env={dir}")))
                .flatten()
                .into_iter()
                .chain(placed.iter().cloned())
                .collect(),
            None => args.to_vec(),
        },
    }
}

#[cfg(unix)]
#[derive(Debug, Clone, Copy)]
enum Want<'a> {
    Typed,
    Cache,
    Target(&'a str),
    Trust(&'a str),
    /// [`Want::Cache`] for a frontend that loses the `--config` ([`loses_config`]), exported too.
    CacheHanded,
    /// [`Want::Target`] for a frontend that loses the `--config`, exported too.
    TargetHanded(&'a str),
}

#[cfg(unix)]
impl From<bool> for Want<'_> {
    fn from(cache: bool) -> Self {
        if cache { Self::Cache } else { Self::Typed }
    }
}

#[cfg(unix)]
fn shells() -> Vec<Vec<&'static str>> {
    let mut out = vec![vec!["/bin/sh"]];
    for (path, extra) in [
        ("/bin/dash", None),
        ("/bin/ksh", None),
        ("/bin/zsh", Some("--emulate")),
    ] {
        if Path::new(path).is_file() {
            let mut v = vec![path];
            if let Some(flag) = extra {
                v.extend([flag, "sh"]);
            }
            out.push(v);
        }
    }
    out
}

fn block(tool: Tool) -> String {
    let name = match tool {
        Tool::Tippy => "targo-tippy",
        Tool::Targo | Tool::Cargo => "targo",
    };
    shim_block(name).unwrap()
}

#[cfg(unix)]
fn sh(
    shell: &[&str],
    tool: Tool,
    home: &Path,
    cwd: &Path,
    env: &HashMap<&str, String>,
    args: &[String],
) -> Vec<String> {
    // Each export the block made: any value the variable has that the row's own environment
    // did not give it.
    let exported = if env.contains_key(BUILD_DIR_ENV) {
        ""
    } else {
        "[ -z \"${CARGO_BUILD_BUILD_DIR+1}\" ] || printf 'env=%s\\n' \"$CARGO_BUILD_BUILD_DIR\"\n"
    };
    let mut cmd = aterm_uds::spawnfd::command(shell[0]);
    cmd.args(&shell[1..])
        .arg("-c")
        .arg(format!(
            "{}{exported}[ $# -eq 0 ] || printf '%s\\n' \"$@\"",
            block(tool)
        ))
        .arg("sh")
        .args(args)
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .env("HOME", home)
        .env("PWD", cwd)
        .current_dir(cwd);
    for (k, v) in env {
        cmd.env(k, v);
    }
    let out = cmd.output().unwrap();
    assert!(
        out.status.success(),
        "{shell:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        out.stderr.is_empty(),
        "{shell:?} said: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout)
        .unwrap()
        .lines()
        .map(str::to_owned)
        .collect()
}

#[cfg(unix)]
fn check<'a>(
    f: &Fixture,
    cwd: &Path,
    env: &[(&'static str, &str)],
    args: &[&str],
    want: impl Into<Want<'a>>,
) {
    check_tool(f, Tool::Targo, cwd, env, args, want);
}

#[cfg(unix)]
fn check_tool<'a>(
    f: &Fixture,
    tool: Tool,
    cwd: &Path,
    env: &[(&'static str, &str)],
    args: &[&str],
    want: impl Into<Want<'a>>,
) {
    let env: HashMap<&str, String> = env.iter().map(|(k, v)| (*k, (*v).to_owned())).collect();
    let args = strings(args);
    let first = match want.into() {
        Want::Typed => vec![],
        Want::Cache => vec!["--config".to_owned(), f.value()],
        Want::Target(dir) => vec!["--config".to_owned(), format!("build.build-dir='{dir}'")],
        Want::Trust(dir) => vec![format!("env={dir}")],
        Want::CacheHanded => vec![
            format!("env={}", directory_from_config(&f.value()).unwrap()),
            "--config".to_owned(),
            f.value(),
        ],
        Want::TargetHanded(dir) => vec![
            format!("env={dir}"),
            "--config".to_owned(),
            format!("build.build-dir='{dir}'"),
        ],
    };
    let want: Vec<String> = first.into_iter().chain(args.iter().cloned()).collect();
    check_exact(tool, &f.home, cwd, &env, &args, &want);
}

/// Both halves of the rule turn `args` into `want`.
#[cfg(unix)]
fn check_exact(
    tool: Tool,
    home: &Path,
    cwd: &Path,
    env: &HashMap<&str, String>,
    args: &[String],
    want: &[String],
) {
    assert_eq!(
        rust(home, cwd, env, args, tool),
        want,
        "rust {tool:?}: {args:?} in {}",
        cwd.display()
    );
    for shell in shells() {
        assert_eq!(
            sh(&shell, tool, home, cwd, env, args),
            want,
            "{shell:?} {tool:?}: {args:?} in {}",
            cwd.display()
        );
    }
}

/// The shim this version lays names the cache root the Rust rule uses, so a build that follows
/// the shim on `$PATH` ([`shim_root`]) and one that follows the rule agree once it is laid.
#[test]
fn the_laid_shim_names_this_versions_cache_root() {
    let home = Path::new("/Users//u");
    for name in ["targo", "targo-tippy"] {
        let block = shim_block(name).expect("a shim");
        assert_eq!(
            root_named_by_shim(home, &block),
            Some(cache_root(home)),
            "{name}"
        );
    }
}

/// A TEST RUN'S SCRATCH GOES QUIET: under every shell the block hands `targo --unverified test`
/// (and `t`, `bench`) `$TMPDIR`'s `.ubd` folder once that folder is this user's, so what the
/// tests make there never reaches fseventsd's clients. NEGATIVE CONTROL: a build, an absent
/// folder, a link in its place, and a `$TMPDIR` already quiet keep `$TMPDIR` as it was.
#[cfg(unix)]
#[test]
fn a_test_run_gets_the_quiet_tmpdir_once_it_is_this_users() {
    struct Unlink(PathBuf);
    impl Drop for Unlink {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(&self.0);
        }
    }
    let f = Fixture::new("build-cache-tmpdir");
    let ws = f.dir("home/ws");
    // The fixture is itself in a quiet folder, which the block takes for quiet already: the
    // `$TMPDIR` it reads is a name outside one, a link to the fixture's folder.
    let temp = Unlink(Path::new("/tmp").join(format!("atb-tmpdir-{}", std::process::id())));
    let _ = std::fs::remove_file(&temp.0);
    std::os::unix::fs::symlink(f.dir("T"), &temp.0).unwrap();
    let tmpdir = format!("{}/", temp.0.display());
    let quiet = temp.0.join(".ubd");
    let run = |shell: &[&str], args: &[&str], tmpdir: &str| -> String {
        let out = aterm_uds::spawnfd::command(shell[0])
            .args(&shell[1..])
            .arg("-c")
            .arg(format!("{}printf '%s' \"$TMPDIR\"", block(Tool::Targo)))
            .arg("sh")
            .args(args)
            .env_clear()
            .env("PATH", "/usr/bin:/bin")
            .env("HOME", &f.home)
            .env("PWD", &ws)
            .env("TMPDIR", tmpdir)
            .current_dir(&ws)
            .output()
            .unwrap();
        assert!(out.status.success(), "{shell:?}: {out:?}");
        String::from_utf8(out.stdout).unwrap()
    };
    let test = ["--unverified", "test", "-p", "x"];
    for shell in shells() {
        assert_eq!(
            run(&shell, &test, &tmpdir),
            tmpdir,
            "{shell:?}: no folder yet"
        );
    }
    std::os::unix::fs::symlink(&ws, &quiet).unwrap();
    for shell in shells() {
        assert_eq!(run(&shell, &test, &tmpdir), tmpdir, "{shell:?}: a link");
    }
    std::fs::remove_file(&quiet).unwrap();
    std::fs::create_dir(&quiet).unwrap();
    let moved = format!("{}/", quiet.display());
    for shell in shells() {
        for args in [
            &test[..],
            &["--unverified", "t"],
            &["--unverified", "bench"],
        ] {
            assert_eq!(run(&shell, args, &tmpdir), moved, "{shell:?} {args:?}");
        }
        assert_eq!(
            run(&shell, &["--unverified", "build"], &tmpdir),
            tmpdir,
            "{shell:?}"
        );
        assert_eq!(
            run(&shell, &test, &moved),
            moved,
            "{shell:?}: quiet already"
        );
    }
}

/// THE SHIM RAISES A LOW SOFT DESCRIPTOR LIMIT to the floor its stages need
/// (4096, capped at the hard limit) and never lowers one: a test binary's
/// threads pass a stock shell's 256 together, so without this a direct
/// `targo test` fails `Too many open files` on the caller's limit alone.
/// NEGATIVE CONTROL: a soft limit already past the floor stands as it was.
#[cfg(unix)]
#[test]
fn the_shim_raises_a_low_soft_descriptor_limit_and_never_lowers_one() {
    let f = Fixture::new("build-cache-nofile");
    let ws = f.dir("home/ws");
    let outside = f.dir("elsewhere/lint");
    let run = |shell: &[&str],
               tool: Tool,
               cwd: &std::path::Path,
               soft: u64,
               hard: Option<u64>,
               verb: &str|
     -> (String, String, String) {
        let setup = hard.map_or_else(String::new, |hard| {
            format!("ulimit -Hn {hard} 2>/dev/null; ")
        });
        let out = aterm_uds::spawnfd::command(shell[0])
            .args(&shell[1..])
            .arg("-c")
            .arg(format!(
                "{setup}ulimit -Sn {soft} 2>/dev/null; __s0=$(ulimit -Sn); {}printf '%s %s %s' \
                 \"$__s0\" \"$(ulimit -Sn)\" \"$(ulimit -Hn)\"",
                block(tool)
            ))
            .arg("sh")
            .arg("--unverified")
            .arg(verb)
            .env_clear()
            .env("PATH", "/usr/bin:/bin")
            .env("HOME", &f.home)
            .env("PWD", cwd)
            .current_dir(cwd)
            .output()
            .unwrap();
        assert!(
            out.status.success() && out.stderr.is_empty(),
            "{shell:?}: {out:?}"
        );
        let words: Vec<String> = String::from_utf8(out.stdout)
            .unwrap()
            .split_whitespace()
            .map(str::to_owned)
            .collect();
        assert_eq!(words.len(), 3, "{shell:?}: {words:?}");
        (words[0].clone(), words[1].clone(), words[2].clone())
    };
    let want = |before: &str, hard: &str| -> Option<u64> {
        let before: u64 = before.parse().ok()?;
        let floor = aterm_uds::spawnfd::SOFT_NOFILE_FLOOR.max(before);
        Some(match hard.parse::<u64>() {
            Ok(hard) => floor.min(hard),
            Err(_) => floor,
        })
    };
    let mut asserted = 0;
    for shell in shells() {
        // A verb that builds nothing leaves the caller's limit alone (the
        // read that decides is a fork, paid only by verbs that need room).
        let (before, after, _) = run(&shell, Tool::Targo, &ws, 256, None, "fmt");
        assert_eq!(before, after, "{shell:?}: fmt raised {before} -> {after}");
        // Outside $HOME the verb scan never runs, so a targo verb is unknown
        // there and the limit stands; the tippy shim knows its own name
        // without a scan and still raises.
        let (before, after, _) = run(&shell, Tool::Targo, &outside, 256, None, "test");
        assert_eq!(
            before, after,
            "{shell:?}: unscanned test raised {before} -> {after}"
        );
        let (before, after, hard) = run(&shell, Tool::Tippy, &outside, 256, None, "test");
        if let Some(want) = want(&before, &hard) {
            assert_eq!(
                after.parse::<u64>().ok(),
                Some(want),
                "{shell:?}: tippy outside home: soft {before} -> {after} (hard {hard})"
            );
        }
        for (soft, hard) in [(256_u64, None), (8192, None), (256, Some(1024))] {
            let (before, after, hard) = run(&shell, Tool::Targo, &ws, soft, hard, "test");
            let Some(want) = want(&before, &hard) else {
                continue;
            };
            asserted += 1;
            assert_eq!(
                after.parse::<u64>().ok(),
                Some(want),
                "{shell:?}: soft {before} -> {after} (hard {hard})"
            );
        }
    }
    assert!(asserted > 0, "no shell rendered a numeric soft limit");
}

#[cfg(unix)]
#[test]
fn the_shim_and_the_rust_rule_decide_every_case_alike() {
    let f = Fixture::new("build-cache-rules");
    let ws = f.dir("home/ws");
    let home = f.home.display().to_string();

    for args in [
        &["--unverified", "build"][..],
        &["--unverified", "test", "-p", "x"],
        &["build", "--unverified"],
        &["clean"],
        &["clean", "-p", "x"],
        &["--unverified", "b"],
        &["--unverified", "t", "--lib"],
        &["--color", "always", "--unverified", "check"],
        &["-Z", "unstable-options", "--unverified", "doc"],
        &["--unverified", "run", "--", "--target-dir", "x"],
        &["--unverified", "build", "--config", "build.jobs=2"],
        &["--unverified", "build", "--config=build.jobs=2"],
        &["--unverified", "build", "--manifest-path", "sub/Cargo.toml"],
    ] {
        check(&f, &ws, &[], args, true);
    }
    for args in [&["--unverified", "tippy", "--all-targets"][..], &["clippy"]] {
        check(&f, &ws, &[], args, Want::CacheHanded);
    }
    let in_home = format!("{home}/ws/Cargo.toml");
    check(
        &f,
        &ws,
        &[],
        &["--unverified", "build", "--manifest-path", &in_home],
        true,
    );

    for args in [
        &[][..],
        &["--version"],
        &["build"],
        &["test"],
        &["--unverified", "metadata"],
        &["--unverified", "install", "x"],
        &["new", "x"],
        &["--unverified", "verify"],
        &["--unverified", "build", "--target-dir"],
        &["--unverified", "build", "--target-dir="],
        &["--unverified", "build", "--target-dir", "x.noindex"],
        &[
            "--unverified",
            "build",
            "--target-dir",
            "/t/lane.noindex/sub",
        ],
        &["--unverified", "build", "--target-dir", "o'x"],
        &["--unverified", "build", "--target-dir", "x{y}"],
        &["--config", "build.target-dir='x'", "--unverified", "build"],
        &["--unverified", "build", "--config=build.build-dir='y'"],
        &["--config", "my.toml", "--unverified", "build"],
        &["--unverified", "build", "--config"],
        &["-C", "elsewhere", "--unverified", "build"],
        &[
            "--unverified",
            "build",
            "--manifest-path",
            "/elsewhere/Cargo.toml",
        ],
        &[
            "--unverified",
            "build",
            "--manifest-path",
            "../x/Cargo.toml",
        ],
        &[
            "--unverified",
            "build",
            "--manifest-path",
            "{{project_name}}/Cargo.toml",
        ],
        &["--unverified", "build", "--manifest-path=a}b/Cargo.toml"],
    ] {
        check(&f, &ws, &[], args, false);
    }
    let cached = format!("{}/x/Cargo.toml", cache_root(&f.home).display());
    check(
        &f,
        &ws,
        &[],
        &["--unverified", "build", "--manifest-path", &cached],
        false,
    );
    check(
        &f,
        &ws,
        &[],
        &[
            "--unverified",
            "build",
            &format!("--manifest-path={cached}"),
        ],
        false,
    );

    for dir in [
        "home/brace{x}",
        "home/open{only",
        "home/close}only",
        "home/cc/{{name}}",
    ] {
        check(&f, &f.dir(dir), &[], &["--unverified", "build"], false);
    }

    for args in [
        &[][..],
        &[
            "--workspace",
            "--all-targets",
            "--keep-going",
            "--",
            "-D",
            "warnings",
        ],
        &["-p", "x", "--lib"],
    ] {
        check_tool(&f, Tool::Tippy, &ws, &[], args, Want::CacheHanded);
    }
    let ws_t = format!("{}/t/build.noindex", ws.display());
    check_tool(
        &f,
        Tool::Tippy,
        &ws,
        &[],
        &["--target-dir", "t"],
        Want::TargetHanded(&ws_t),
    );
    check_tool(
        &f,
        Tool::Tippy,
        &ws,
        &[("CARGO_TARGET_DIR", "/t")],
        &[],
        Want::TargetHanded("/t/build.noindex"),
    );
    check_tool(&f, Tool::Tippy, &f.dir("elsewhere/lint"), &[], &[], false);

    let build = &["--unverified", "build"][..];
    let trust = &["trust", "check", "--lib"][..];
    let x = Want::Target("/x/build.noindex");
    for var in ["CARGO_TARGET_DIR", "CARGO_BUILD_TARGET_DIR"] {
        check(&f, &ws, &[(var, "/x")], build, x);
        check(&f, &ws, &[(var, "/x")], &["clean"], x);
        check(
            &f,
            &ws,
            &[(var, "/x")],
            trust,
            Want::Trust("/x/build.noindex"),
        );
        check(&f, &ws, &[(var, "")], build, false);
        check(&f, &ws, &[(var, "/x/lane.noindex")], build, false);
    }
    check(&f, &ws, &[("CARGO_BUILD_BUILD_DIR", "/x")], build, false);
    check(&f, &ws, &[("CARGO_BUILD_BUILD_DIR", "")], build, false);
    check(&f, &ws, &[("CARGO_BUILD_BUILD_DIR", "/x")], trust, false);

    let lane = format!("{}/target/lane/build.noindex", ws.display());
    check(
        &f,
        &ws,
        &[("CARGO_TARGET_DIR", "target/lane")],
        build,
        Want::Target(&lane),
    );
    check(
        &f,
        &ws,
        &[],
        &["--unverified", "build", "--target-dir=target/lane"],
        Want::Target(&lane),
    );
    check(
        &f,
        &ws,
        &[("CARGO_TARGET_DIR", "/b")],
        &["--unverified", "test", "--target-dir", "target/lane"],
        Want::Target(&lane),
    );
    check(
        &f,
        &ws,
        &[("CARGO_TARGET_DIR", "/x"), ("CARGO_BUILD_TARGET_DIR", "/b")],
        build,
        x,
    );
    check(
        &f,
        &ws,
        &[("CARGO_BUILD_TARGET_DIR", "/x")],
        &["--unverified", "build", "--", "--target-dir", "y"],
        x,
    );
    check(&f, &ws, &[("CARGO_TARGET_DIR", "o'q")], build, false);

    let default = Want::Trust("{workspace-root}/target/build.noindex");
    check(&f, &ws, &[], trust, default);
    check(&f, &ws, &[], &["trust", "test", "-p", "x"], default);
    check(&f, &f.dir("elsewhere/trusted"), &[], trust, false);
    check(&f, &f.dir("home/lab.noindex/ws"), &[], trust, false);
    check(&f, &f.dir("home/lab.noindex/ws"), &[], build, true);

    f.file(
        "home/chosen/.cargo/config.toml",
        "[build]\ntarget-dir = \"out\"\n",
    );
    let chosen = f.dir("home/chosen");
    check(&f, &chosen, &[], build, false);
    check(&f, &chosen, &[], trust, false);
    check(&f, &chosen, &[("CARGO_TARGET_DIR", "/x")], build, x);
    f.file(
        "home/commented/.cargo/config.toml",
        "# target-dir = \"out\"\n[build]\n  \t# build-dir = \"b\"\njobs = 2\n",
    );
    check(&f, &f.dir("home/commented"), &[], build, true);
    f.file(
        "home/trailing/.cargo/config.toml",
        "[build]\njobs = 2 # not a build-dir\n",
    );
    check(&f, &f.dir("home/trailing"), &[], build, false);
    f.file(
        "home/inline/.cargo/config.toml",
        "build = { rustflags = [\"#\"], target-dir = \"x\" }\n",
    );
    check(&f, &f.dir("home/inline"), &[], build, false);
    f.file("home/legacy/.cargo/config", "build.build-dir = 'b'");
    check(&f, &f.dir("home/legacy"), &[], build, false);
    f.file("home/up/.cargo/config.toml", "[build]\nbuild-dir = \"b\"\n");
    let up = f.dir("home/up/a/b");
    check(&f, &up, &[], build, false);
    check(&f, &up, &[("CARGO_TARGET_DIR", "/x")], build, false);
    check(&f, &up, &[("CARGO_TARGET_DIR", "/x")], trust, false);
    f.file("cargohome/config.toml", "[build]\ntarget-dir = \"t\"\n");
    let cargo_home = f.root.join("cargohome").display().to_string();
    check(&f, &ws, &[("CARGO_HOME", &cargo_home)], build, false);
    f.file("home/.cargo/config.toml", "[net]\noffline = true\n");
    check(&f, &ws, &[], build, true);
    f.file("home/.cargo/config.toml", "build.target-dir = \"t\"\n");
    check(&f, &ws, &[], build, false);
    std::fs::remove_file(f.root.join("home/.cargo/config.toml")).unwrap();

    f.file(
        "home/aliased/.cargo/config.toml",
        "alias.ship = \"run --profile gate -p aterm-release --\"\n[build]\njobs = 2\n\
         [alias]\n# lint = \"tippy\"\ntip = \"tippy\"\nverify = \"run -q -p aterm-verify --\"\n\
         lab = [\"run\", \"-q\", \"-p\", \"aterm-lab\", \"--\"]\ntree2 = \"tree -d\"\n\
         1b = \"build\"\n[env]\nfake = \"build\"\n",
    );
    let aliased = f.dir("home/aliased/sub");
    // An alias to the lint, too: cargo hands it on without the `--config`, which is its whole
    // placement (the limit `loses_config` names). No alias's name exports, a digit-led one
    // included: cargo runs `1b` itself and would hand the variable to its build scripts.
    for verb in ["verify", "lab", "ship", "tip", "1b"] {
        check(&f, &aliased, &[], &["--unverified", verb, "x"], true);
    }
    for args in [
        &["--unverified", "tree2"][..],
        &["--unverified", "fake"],
        &["--unverified", "lint"],
        &["--unverified", "verif"],
        &["--unverified", "verifyx"],
        &["--unverified", "nope"],
        &["verify"],
        &["--unverified"],
    ] {
        check(&f, &aliased, &[], args, false);
    }
    check(
        &f,
        &aliased,
        &[("CARGO_TARGET_DIR", "/x")],
        &["--unverified", "verify"],
        x,
    );
    f.file(
        "cargohome2/config.toml",
        "[alias]\nxt = \"run -p xtask --\"\n",
    );
    let cargo_home2 = f.root.join("cargohome2").display().to_string();
    check(
        &f,
        &ws,
        &[("CARGO_HOME", &cargo_home2)],
        &["--unverified", "xt"],
        true,
    );
    check(&f, &ws, &[], &["--unverified", "xt"], false);

    check(&f, &f.dir("elsewhere/ws"), &[], build, false);
    check(&f, &f.home, &[], build, false);
    let cache = cache_root(&f.home).join("x");
    std::fs::create_dir_all(&cache).unwrap();
    check(&f, &cache, &[], build, false);
    std::fs::remove_dir_all(f.home.join("Library/Caches/aterm")).unwrap();

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        let own = Want::Target("{workspace-root}/target/build.noindex");
        let caches = f.home.join("Library/Caches");
        std::fs::set_permissions(&caches, std::fs::Permissions::from_mode(0o555)).unwrap();
        check(&f, &ws, &[], build, own);
        std::fs::set_permissions(&caches, std::fs::Permissions::from_mode(0o755)).unwrap();
        check(&f, &ws, &[], build, true);
        std::fs::remove_dir(&caches).unwrap();
        check(&f, &ws, &[], build, own);
        check(&f, &f.dir("home/lab.noindex/ws"), &[], build, false);
    }
}

/// A placed call gets the build-dir pair ahead of its words and nothing else: a dev or release
/// `build`, `test`, `t`, `bench`, a call that names `-Zfeature-unification` or the workspace's
/// `--config` itself, another verb and an alias each keep every word as typed, in both halves of
/// the rule (TARGO-MEMBER-FEATURE-SCOPE-1: no placement chooses a feature unification).
/// [`a_placed_member_build_resolves_the_unit_graph_plain_targo_does`] holds the real `targo`'s
/// unit graphs to the same.
#[cfg(unix)]
#[test]
fn a_placed_build_keeps_every_word_as_typed() {
    let f = Fixture::new("build-cache-graph");
    let ws = f.dir("home/ws");
    f.file(
        "home/ws/.cargo/config.toml",
        "[alias]\nxt = \"test -p x\"\n",
    );
    let words = |line: &str| -> Vec<String> {
        line.split_whitespace()
            .flat_map(|w| match w {
                "C" => TEST_GRAPH_CONFIG.to_vec(),
                "F" => vec![TEST_GRAPH_FLAG],
                _ => vec![w],
            })
            .map(str::to_owned)
            .collect()
    };
    for typed in [
        "--unverified test -p x",
        "--unverified t --lib",
        "--unverified bench --no-run",
        "--color always -Z unstable-options --unverified test -q",
        "--config build.jobs=2 --manifest-path sub/Cargo.toml --unverified test",
        "--unverified test -- --exact -Zfeature-unification",
        "--unverified test -Zfeature-unification -p x",
        "--unverified C test F -p x",
        "--unverified build -p x",
        "--unverified b -p x",
        "--unverified build --profile dev",
        "--unverified build --profile=test",
        "--unverified build --release",
        "--unverified b -r",
        "--unverified build --profile=gate",
        "--unverified run -p x -- test",
        "--unverified xt",
    ] {
        let want: Vec<String> = ["--config".to_owned(), f.value()]
            .into_iter()
            .chain(words(typed))
            .collect();
        check_exact(
            Tool::Targo,
            &f.home,
            &ws,
            &HashMap::new(),
            &words(typed),
            &want,
        );
    }
}

/// THE SHIM BUILDS WHAT `targo` BUILDS (TARGO-MEMBER-FEATURE-SCOPE-1): over a scratch workspace
/// where one member alone turns on a shared path dependency's feature, the real `targo`'s unit
/// graph of `-p narrow`'s `test`, `bench` and dev `build`, as each half of the rule places the
/// call (the Rust rule, and the block under every shell), is byte for byte the graph of the call
/// as typed: the dependency without the feature. POSITIVE CONTROLS: each call was placed (it
/// leads with the build-dir pair, so the comparison is not of two unplaced calls), and the same
/// call under the workspace's feature unification ([`with_test_graph`]) holds the feature, so
/// the instrument sees a widened graph when there is one.
#[cfg(unix)]
#[test]
fn a_placed_member_build_resolves_the_unit_graph_plain_targo_does() {
    let f = Fixture::new("build-cache-member-graph");
    let ws = f.dir("home/ws");
    f.file(
        "home/ws/Cargo.toml",
        "[workspace]\nresolver = '2'\nmembers = ['wide', 'narrow', 'shared']\n",
    );
    for (name, extra) in [
        ("shared", "[features]\nextra = []\n"),
        (
            "wide",
            "[dependencies]\nshared = { path = '../shared', features = ['extra'] }\n",
        ),
        (
            "narrow",
            "[dependencies]\nshared = { path = '../shared' }\n",
        ),
    ] {
        f.file(
            &format!("home/ws/{name}/Cargo.toml"),
            &format!("[package]\nname = '{name}'\nversion = '1.0.0'\nedition = '2021'\n{extra}"),
        );
        f.file(&format!("home/ws/{name}/src/lib.rs"), "");
    }
    let cargo_home = f.dir("cargo-home");
    // The unit graph `args` resolve, run by the `targo` that built this test, at the workspace,
    // in a CARGO_HOME of its own, without what `targo test` said about this crate or a target
    // directory it chose.
    let graph = |args: &[String]| -> String {
        let mut cmd = aterm_uds::spawnfd::command(env!("CARGO"));
        cmd.args(args)
            .args(["--offline", "--unit-graph", "-Z", "unstable-options"])
            .current_dir(&ws)
            .env("CARGO_HOME", &cargo_home)
            .env_remove("CARGO_TARGET_DIR")
            .env_remove("CARGO_BUILD_TARGET_DIR")
            .env_remove(BUILD_DIR_ENV);
        for (name, _) in std::env::vars_os() {
            if name.to_str().is_some_and(|name| {
                said_by_cargo_run(name)
                    || name.starts_with("CARGO_RESOLVER_")
                    || name.starts_with("CARGO_UNSTABLE_")
            }) {
                cmd.env_remove(name);
            }
        }
        let out = cmd.output().unwrap();
        let text = String::from_utf8_lossy(&out.stdout).into_owned();
        assert!(
            out.status.success(),
            "{args:?}: {}{text}",
            String::from_utf8_lossy(&out.stderr)
        );
        text.lines()
            .find(|line| line.starts_with("{\"version\":1"))
            .unwrap_or_else(|| panic!("{args:?}: no unit graph in {text}"))
            .to_owned()
    };
    // The features the graph turns on for `shared`.
    let shared = |graph: &str| -> Vec<String> {
        let value: aterm_json::Value = aterm_json::from_str(graph).unwrap();
        let mut features: Vec<String> = value["units"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|unit| {
                unit["pkg_id"]
                    .as_str()
                    .is_some_and(|id| id.contains("/shared#"))
            })
            .flat_map(|unit| unit["features"].as_array().unwrap().iter())
            .map(|feature| feature.as_str().unwrap().to_owned())
            .collect();
        features.sort();
        features.dedup();
        features
    };
    let env = HashMap::new();
    for verb in ["test", "bench", "build"] {
        let typed = strings(&["--unverified", verb, "-p", "narrow"]);
        let plain = graph(&typed);
        assert!(shared(&plain).is_empty(), "{verb}: the fixture: {plain}");
        assert_eq!(
            shared(&graph(&with_test_graph(&typed))),
            ["extra"],
            "{verb}: the control"
        );
        let mut placed = vec![(
            "rust".to_owned(),
            rust(&f.home, &ws, &env, &typed, Tool::Targo),
        )];
        for shell in shells() {
            placed.push((
                format!("{shell:?}"),
                sh(&shell, Tool::Targo, &f.home, &ws, &env, &typed),
            ));
        }
        for (half, args) in placed {
            assert_eq!(
                args[..2],
                ["--config".to_owned(), f.value()],
                "{half} {verb}: placed"
            );
            let got = graph(&args);
            assert!(
                shared(&got).is_empty(),
                "{half} {verb}: widened by {args:?}"
            );
            assert_eq!(got, plain, "{half} {verb}: {args:?}");
        }
    }
}

#[cfg(unix)]
#[test]
fn a_home_cargo_cannot_take_is_left_alone() {
    let f = Fixture::new("build-cache-home");
    let args = strings(&["--unverified", "build"]);
    for (dir, home) in [
        ("o'home", "o'home"),
        ("h{b}", "h{b}"),
        ("open{", "open{"),
        ("slash", "slash/"),
    ] {
        let real = f.dir(dir);
        std::fs::create_dir_all(real.join("Library/Caches")).unwrap();
        let ws = f.dir(&format!("{dir}/ws"));
        let home = PathBuf::from(format!("{}/{home}", f.root.display()));
        assert_eq!(
            rust(&home, &ws, &HashMap::new(), &args, Tool::Targo),
            args,
            "{home:?}"
        );
        for shell in shells() {
            assert_eq!(
                sh(&shell, Tool::Targo, &home, &ws, &HashMap::new(), &args),
                args,
                "{shell:?} {home:?}"
            );
        }
    }
    assert_eq!(config_value(Path::new("/o'h")), None);
    assert_eq!(config_value(Path::new("/h{b}")), None);
}

#[cfg(unix)]
#[test]
fn a_working_directory_reached_through_a_link_is_left_alone() {
    let f = Fixture::new("build-cache-link");
    let build = strings(&["--unverified", "build"]);
    for (link, real) in [
        ("home/in", "home/plain"),
        ("home/braced", "home/real{x}"),
        ("home/out", "elsewhere/ws"),
    ] {
        let real = f.dir(real);
        let link = f.root.join(link);
        std::os::unix::fs::symlink(&real, &link).unwrap();
        let env = HashMap::from([("PWD", link.display().to_string())]);
        assert_eq!(
            rust(&f.home, &real, &env, &build, Tool::Targo),
            build,
            "{link:?}"
        );
        for shell in shells() {
            assert_eq!(
                sh(&shell, Tool::Targo, &f.home, &real, &env, &build),
                build,
                "{shell:?} {link:?}"
            );
        }
    }
    check(
        &f,
        &f.root.join("home/plain"),
        &[],
        &["--unverified", "build"],
        true,
    );
}

/// A tool that starts a nested `targo` at a checkout it names gets the shim's answer for a build
/// typed there, whatever `$PWD` it inherited; a checkout named through a link is left alone.
#[cfg(unix)]
#[test]
fn a_build_started_at_a_named_folder_gets_that_folders_answer() {
    let f = Fixture::new("build-cache-named");
    let ws = f.dir("home/ws");
    let typed_in = f.dir("home/elsewhere");
    let bench = strings(&["--unverified", "bench", "--no-run"]);
    let env = HashMap::from([("PWD", typed_in.display().to_string())]);
    let m = Machine {
        root: None,
        home: Some(&f.home),
        cwd: Some(&typed_in),
        env: &|name| env.get(name).map(OsString::from),
        read: &read_config,
        writable: &writable,
    };
    // CONTROL: read with the inherited `$PWD`, the checkout looks reached through a link.
    let read_pwd = Machine {
        cwd: Some(&ws),
        ..m
    };
    assert_eq!(with_build_dir(&bench, Tool::Targo, &read_pwd), None);
    let mut cached = vec!["--config".to_owned(), f.value()];
    cached.extend(bench.iter().cloned());
    assert_eq!(
        with_build_dir_at(&bench, Tool::Targo, &ws, &m),
        Some(cached)
    );
    let link = f.root.join("home/link");
    std::os::unix::fs::symlink(&ws, &link).unwrap();
    assert_eq!(with_build_dir_at(&bench, Tool::Targo, &link, &m), None);
}

#[test]
fn an_upstream_cargo_keeps_its_toolchain_first() {
    let f = Fixture::new("build-cache-cargo");
    let ws = f.dir("home/ws");
    let env = HashMap::new();
    let v = f.value();
    let run = |args: &[&str]| rust(&f.home, &ws, &env, &strings(args), Tool::Cargo);
    assert_eq!(run(&["build"]), strings(&["--config", &v, "build"]));
    assert_eq!(
        run(&["+1.97.1", "test"]),
        strings(&["+1.97.1", "--config", &v, "test"])
    );
    assert_eq!(
        run(&["+nightly", "metadata"]),
        strings(&["+nightly", "metadata"])
    );
    assert_eq!(run(&["install", "x"]), strings(&["install", "x"]));
    assert_eq!(run(&["trust", "check"]), strings(&["trust", "check"]));
    let t = format!("build.build-dir='{}/t/build.noindex'", ws.display());
    assert_eq!(
        run(&["+1.97.1", "build", "--target-dir", "t"]),
        strings(&["+1.97.1", "--config", &t, "build", "--target-dir", "t"])
    );
}

#[test]
fn the_release_gate_reads_the_version_line() {
    for yes in [
        "1.91.0",
        "1.91.1",
        "1.92.0-nightly",
        "1.97.1",
        "1.99.0-dev",
        "2.0.0",
    ] {
        assert!(release_reads_build_dir(yes), "{yes}");
    }
    for no in [
        "1.90.0",
        "1.91.0-nightly",
        "1.85.1",
        "0.99.0",
        "",
        "x.y",
        "1",
    ] {
        assert!(!release_reads_build_dir(no), "{no}");
    }
}

#[test]
fn every_workspace_gets_its_own_folder_under_the_cache() {
    let root = Path::new("/h/Library/Caches/aterm/cargo-build.noindex");
    assert_eq!(
        config_value(root).unwrap(),
        "build.build-dir='/h/Library/Caches/aterm/cargo-build.noindex{workspace-root}/.cargo-build'"
    );
    let dirs: Vec<PathBuf> = [
        "/h/p",
        "/h/p/sub",
        "/h/p.cargo-build",
        "/h/p/.cargo-build",
        "/h/q",
    ]
    .iter()
    .map(|w| build_dir(root, Path::new(w)))
    .collect();
    assert_eq!(
        dirs[0],
        Path::new("/h/Library/Caches/aterm/cargo-build.noindex/h/p/.cargo-build")
    );
    for (i, a) in dirs.iter().enumerate() {
        for b in &dirs[i + 1..] {
            assert_ne!(a, b);
        }
    }
}

#[test]
fn the_targo_shim_carries_the_block_and_reads_as_before() {
    let targo = Path::new("/p/store/trust/9192/bin/targo");
    let env = crate::shim_env::ShimEnv::admit(&["A=1".to_owned()]).unwrap();
    let body = crate::platform::sh_shim_content_routed(targo, &env, None);
    assert_eq!(
        body.contains(&block(Tool::Targo)),
        cfg!(target_os = "macos"),
        "{body}"
    );
    let lint = crate::platform::sh_shim_content_routed(
        Path::new("/p/store/trust/9192/bin/targo-tippy"),
        &env,
        None,
    );
    assert_eq!(
        lint.contains(&block(Tool::Tippy)),
        cfg!(target_os = "macos"),
        "{lint}"
    );
    assert_ne!(block(Tool::Tippy), block(Tool::Targo));
    assert_eq!(
        crate::platform::parse_sh_shim_target(&body).as_deref(),
        Some(targo)
    );
    assert_eq!(crate::platform::parse_sh_shim_env(&body), env);
    assert!(
        body.ends_with("exec '/p/store/trust/9192/bin/targo' \"$@\"\n"),
        "{body}"
    );
    for line in SH_BLOCK.lines() {
        let line = line.trim();
        assert!(!line.starts_with("exec"), "{line}");
        assert!(
            !line.starts_with("export ") || line == "export CARGO_BUILD_BUILD_DIR",
            "{line}"
        );
    }
    let tippy = crate::platform::sh_shim_content_routed(
        Path::new("/p/store/trust/9192/bin/tippy"),
        &env,
        None,
    );
    assert!(!tippy.contains("__atb"), "{tippy}");
}

#[cfg(target_os = "macos")]
#[test]
fn the_rendered_shim_hands_targo_the_config() {
    let f = Fixture::new("build-cache-shim");
    let ws = f.dir("home/ws");
    let bin = f.dir("store/bin");
    std::os::unix::fs::symlink("/bin/echo", bin.join("targo")).unwrap();
    let shim = f.root.join("shim");
    let body = crate::platform::sh_shim_content_routed(
        &bin.join("targo"),
        &crate::shim_env::ShimEnv::NONE,
        None,
    );
    std::fs::write(&shim, body).unwrap();
    let run = |args: &[&str]| {
        let out = aterm_uds::spawnfd::command("/bin/sh")
            .arg(&shim)
            .args(args)
            .env_clear()
            .env("PATH", "/usr/bin:/bin")
            .env("HOME", &f.home)
            .env("PWD", &ws)
            .current_dir(&ws)
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8(out.stdout).unwrap()
    };
    assert_eq!(
        run(&["--unverified", "test", "-q"]),
        format!("--config {} --unverified test -q\n", f.value())
    );
    assert_eq!(run(&["trust", "check"]), "trust check\n");
    assert_eq!(
        run(&["--unverified", "build", "--target-dir", "t"]),
        format!(
            "--config build.build-dir='{}/t/build.noindex' --unverified build --target-dir t\n",
            ws.display()
        )
    );
}

/// The real shim's config survives as environment when an external dispatcher drops all
/// global words. Its inner driver goes through another generated shim, which must not place
/// it a second time. These are owned shell recorders, not a compiler.
#[cfg(target_os = "macos")]
#[test]
fn generated_placement_survives_external_dispatch_and_nested_shims() {
    use std::os::unix::fs::PermissionsExt as _;
    let f = Fixture::new("build-cache-external");
    let ws = f.dir("home/ws");
    let bin = f.dir("drivers");
    let checker = bin.join("inner/targo");
    std::fs::create_dir_all(checker.parent().unwrap()).unwrap();
    std::fs::write(&checker, "#!/bin/sh\nprintf 'inner-dir=%s\\n' \"${CARGO_BUILD_BUILD_DIR-unset}\"\n[ $# -eq 0 ] || printf 'inner-arg=%s\\n' \"$@\"\n").unwrap();
    let inner = f.root.join("inner-shim");
    std::fs::write(
        &inner,
        crate::platform::sh_shim_content_routed(&checker, &crate::shim_env::ShimEnv::NONE, None),
    )
    .unwrap();
    let driver = bin.join("targo");
    std::fs::write(&driver, "#!/bin/sh\nprintf 'outer-dir=%s\\n' \"${CARGO_BUILD_BUILD_DIR-unset}\"\n[ $# -eq 0 ] || printf 'outer-arg=%s\\n' \"$@\"\nwhile [ $# -gt 0 ]; do case $1 in --config) shift; shift ;; --unverified) shift ;; *) break ;; esac; done\ncase ${1-} in tippy|clippy) shift; exec \"$ATPKG_TEST_INNER\" --unverified check \"$@\" ;; *) exec \"$ATPKG_TEST_INNER\" \"$@\" ;; esac\n").unwrap();
    let direct = bin.join("targo-tippy");
    std::fs::write(&direct, "#!/bin/sh\nprintf 'outer-dir=%s\\n' \"${CARGO_BUILD_BUILD_DIR-unset}\"\n[ $# -eq 0 ] || printf 'outer-arg=%s\\n' \"$@\"\nexec \"$ATPKG_TEST_INNER\" --unverified check \"$@\"\n").unwrap();
    for path in [&checker, &inner, &driver, &direct] {
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    let shim = f.root.join("outer-shim");
    let run = |target: &Path, cwd: &Path, words: &[&str], env: &[(&str, &str)]| {
        std::fs::write(
            &shim,
            crate::platform::sh_shim_content_routed(target, &crate::shim_env::ShimEnv::NONE, None),
        )
        .unwrap();
        let mut command = aterm_uds::spawnfd::command("/bin/sh");
        command
            .arg(&shim)
            .args(words)
            .env_clear()
            .env("HOME", &f.home)
            .env("PWD", cwd)
            .env("ATPKG_TEST_INNER", &inner)
            .env("PATH", "/usr/bin:/bin")
            .current_dir(cwd);
        for (name, value) in env {
            command.env(name, value);
        }
        let output =
            aterm_uds::deadline::output_within(&mut command, aterm_spec::wait::MUST_HAPPEN)
                .unwrap();
        assert!(output.status.success(), "{output:?}");
        assert!(output.stderr.is_empty(), "{output:?}");
        String::from_utf8(output.stdout).unwrap()
    };
    let cache = f.value();
    let shared = directory_from_config(&cache).unwrap();
    let output = run(&driver, &ws, &["--unverified", "build"], &[]);
    assert!(
        output.contains("outer-dir=unset\n") && output.contains("inner-dir=unset\n"),
        "a verb cargo runs itself keeps the config words, and a nested cargo inherits nothing: \
         {output}"
    );
    assert!(output.contains(&format!("outer-arg={cache}\n")), "{output}");
    let cargo_config = ws.join(".cargo");
    std::fs::create_dir_all(&cargo_config).unwrap();
    std::fs::write(
        cargo_config.join("config.toml"),
        "[alias]\nbo = \"build -q\"\n",
    )
    .unwrap();
    let output = run(&driver, &ws, &["--unverified", "bo"], &[]);
    assert!(
        output.contains("outer-dir=unset\n") && output.contains("inner-dir=unset\n"),
        "cargo expands an alias itself and keeps the config words: {output}"
    );
    assert!(output.contains(&format!("outer-arg={cache}\n")), "{output}");
    std::fs::remove_dir_all(&cargo_config).unwrap();
    for (target, words) in [
        (&driver, &["--unverified", "tippy", "-p", "demo"][..]),
        (&driver, &["clippy", "-p", "demo"][..]),
        (&direct, &["-p", "demo"][..]),
    ] {
        let output = run(target, &ws, words, &[]);
        assert!(
            output.contains(&format!("outer-dir={shared}\n")),
            "{output}"
        );
        assert!(
            output.contains(&format!("inner-dir={shared}\n")),
            "{output}"
        );
        assert!(output.contains(&format!("outer-arg={cache}\n")), "{output}");
        assert_eq!(
            output.matches("inner-arg=--config\n").count(),
            usize::from(target == &direct),
            "the inner shim must not add placement again: {output}"
        );
    }
    for (target, words, env, expected) in [
        (
            &driver,
            &["--unverified", "tippy", "--target-dir", "custom"][..],
            vec![],
            format!("{}/custom/build.noindex", ws.display()),
        ),
        (
            &driver,
            &["--unverified", "tippy"][..],
            vec![("CARGO_TARGET_DIR", "custom")],
            format!("{}/custom/build.noindex", ws.display()),
        ),
        (
            &driver,
            &["trust", "check"][..],
            vec![],
            "{workspace-root}/target/build.noindex".into(),
        ),
        (
            &driver,
            &["--unverified", "tippy"][..],
            vec![("CARGO_BUILD_BUILD_DIR", "/explicit")],
            "/explicit".into(),
        ),
        (
            &driver,
            &["--unverified", "tippy"][..],
            vec![("CARGO_BUILD_BUILD_DIR", "")],
            "".into(),
        ),
        (
            &direct,
            &["--config", "build.build-dir='user'"][..],
            vec![],
            "unset".into(),
        ),
        (
            &driver,
            &["--unverified", "tippy", "--target-dir", "chosen.noindex"][..],
            vec![],
            "unset".into(),
        ),
        (&driver, &["metadata"][..], vec![], "unset".into()),
    ] {
        let output = run(target, &ws, words, &env);
        assert!(
            output.contains(&format!("outer-dir={expected}\n")),
            "{words:?}: {output}"
        );
        assert!(
            output.contains(&format!("inner-dir={expected}\n")),
            "{words:?}: {output}"
        );
    }
    let outside = f.dir("outside");
    let output = run(&driver, &outside, &["--unverified", "tippy"], &[]);
    assert!(output.contains("inner-dir=unset\n"), "{output}");
    f.file("home/ws/.cargo/config.toml", "[build]\nbuild-dir='user'\n");
    let output = run(&driver, &ws, &["--unverified", "tippy"], &[]);
    assert!(
        output.contains("outer-dir=unset\n") && output.contains("inner-dir=unset\n"),
        "{output}"
    );
}

/// THE RENDERED HOP (BUILD-ADMIT): a macOS `targo` shim laid beside an atpkg runs its front once
/// the block has placed the build, handing it the program, the placed words and what the block
/// did (`config`, `env` for the Trust lane's export, `-` where it placed nothing), and every
/// reader still reads the shim's target from its one `exec '` line, its last. NEGATIVE CONTROL:
/// the same shim rendered with no atpkg beside it (none stamped) has no hop, and runs the target
/// with the placed words itself.
#[cfg(target_os = "macos")]
#[test]
fn the_rendered_hop_hands_its_front_the_placed_call() {
    use std::os::unix::fs::PermissionsExt as _;
    let f = Fixture::new("build-cache-hop");
    let ws = f.dir("home/ws");
    let bin = f.dir("store/bin");
    std::os::unix::fs::symlink("/bin/echo", bin.join("targo")).unwrap();
    let helper = f.dir("pkg").join("atpkg");
    std::fs::write(
        &helper,
        format!(
            "#!/bin/sh\n[ \"$*\" = '__build_admit --capabilities' ] && {{ echo {}; exit 0; }}\n\
             printf '%s\\n' \"$@\" \"build-dir=${{CARGO_BUILD_BUILD_DIR-unset}}\"\n",
            crate::build_admit::HOP_CAPABILITY
        ),
    )
    .unwrap();
    std::fs::set_permissions(&helper, std::fs::Permissions::from_mode(0o755)).unwrap();
    let shim = f.dir("prefix/bin").join("targo");
    let marker = aterm_json::to_string(helper.to_string_lossy().as_ref()).unwrap();
    std::fs::write(&shim, format!("# atpkg cache helper {marker}\n")).unwrap();
    let target = bin.join("targo");
    let none = crate::shim_env::ShimEnv::NONE;
    let body = crate::platform::shim_executable_to_env(&shim, &target, &none)
        .unwrap()
        .body;
    let body = String::from_utf8(body).unwrap();
    std::fs::write(&shim, &body).unwrap();
    assert_eq!(
        crate::platform::parse_sh_shim_target(&body).as_deref(),
        Some(target.as_path())
    );
    let execs: Vec<&str> = body
        .lines()
        .map(str::trim)
        .filter(|l| l.starts_with("exec '"))
        .collect();
    assert_eq!(execs.len(), 1, "{body}");
    assert!(body.ends_with(&format!("{}\n", execs[0])), "{body}");
    let run = |cwd: &Path, args: &[&str]| {
        let out = aterm_uds::spawnfd::command("/bin/sh")
            .arg(&shim)
            .args(args)
            .env_clear()
            .env("PATH", "/usr/bin:/bin")
            .env("HOME", &f.home)
            .env("PWD", cwd)
            .current_dir(cwd)
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8(out.stdout).unwrap()
    };
    let front = |placed: &str, rest: &[&str], dir: &str| {
        let mut w = vec![
            "__build_admit".to_owned(),
            "--shim".to_owned(),
            "targo".to_owned(),
            placed.to_owned(),
            target.display().to_string(),
        ];
        w.extend(rest.iter().map(|r| (*r).to_owned()));
        w.push(format!("build-dir={dir}"));
        w.join("\n") + "\n"
    };
    assert_eq!(
        run(&ws, &["--unverified", "test", "-q"]),
        front(
            "config",
            &["--config", &f.value(), "--unverified", "test", "-q"],
            "unset"
        )
    );
    assert_eq!(
        run(&ws, &["--unverified", "tippy"]),
        front(
            "config",
            &["--config", &f.value(), "--unverified", "tippy"],
            directory_from_config(&f.value()).unwrap()
        )
    );
    assert_eq!(
        run(&ws, &["trust", "check"]),
        front(
            "env",
            &["trust", "check"],
            "{workspace-root}/target/build.noindex"
        )
    );
    let outside = f.dir("elsewhere");
    assert_eq!(
        run(&outside, &["--unverified", "test"]),
        front("-", &["--unverified", "test"], "unset")
    );
    let plain = f.dir("plain/bin").join("targo");
    let body = crate::platform::shim_executable_to_env(&plain, &target, &none)
        .unwrap()
        .body;
    assert!(!String::from_utf8(body).unwrap().contains("__build_admit"));
}

/// THE FRONT THE RENDERED HOP EXECS IS NAMED BY ITS TARGET: the argv a macOS `targo` shim's hop
/// hands its helper (`$0` and every word, as exec'd), in each placement, reads through
/// [`aterm_uds::build_front::front`] as the store's `targo` on the typed words, which every
/// reader names a front by while it waits. NEGATIVE CONTROL: the helper's own capability
/// question, which the renderer asked of it, and the same helper's registration call, which a
/// shim laid for an older helper makes, read as no front.
#[cfg(target_os = "macos")]
#[test]
fn the_rendered_hops_front_is_named_by_its_target() {
    use std::os::unix::fs::PermissionsExt as _;
    let f = Fixture::new("build-cache-hop-name");
    let ws = f.dir("home/ws");
    let bin = f.dir("store/bin");
    std::os::unix::fs::symlink("/bin/echo", bin.join("targo")).unwrap();
    let helper = f.dir("pkg").join("atpkg");
    let log = f.root.join("argv");
    std::fs::write(
        &helper,
        format!(
            "#!/bin/sh\nprintf '%s\\0' \"$0\" \"$@\" > {}\n\
             [ \"$*\" = '__build_admit --capabilities' ] && echo {}\nexit 0\n",
            crate::platform::sh_quote_str(&log.to_string_lossy()),
            crate::build_admit::HOP_CAPABILITY
        ),
    )
    .unwrap();
    std::fs::set_permissions(&helper, std::fs::Permissions::from_mode(0o755)).unwrap();
    let shim = f.dir("prefix/bin").join("targo");
    let marker = aterm_json::to_string(helper.to_string_lossy().as_ref()).unwrap();
    std::fs::write(&shim, format!("# atpkg cache helper {marker}\n")).unwrap();
    let target = bin.join("targo");
    let none = crate::shim_env::ShimEnv::NONE;
    let argv = || -> Vec<OsString> {
        std::fs::read(&log)
            .unwrap()
            .split(|b| *b == 0)
            .filter(|w| !w.is_empty())
            .map(|w| OsString::from(String::from_utf8(w.to_vec()).unwrap()))
            .collect()
    };
    let body = crate::platform::shim_executable_to_env(&shim, &target, &none)
        .unwrap()
        .body;
    let asked = argv();
    assert_eq!(
        asked[1..],
        [
            OsString::from("__build_admit"),
            OsString::from("--capabilities")
        ],
        "the renderer asked the helper"
    );
    assert!(aterm_uds::build_front::front(&asked).is_none());
    std::fs::write(&shim, &body).unwrap();
    let outside = f.dir("elsewhere");
    for (cwd, typed, placed) in [
        (&ws, &["--unverified", "build", "-p", "x"][..], "config"),
        (&ws, &["trust", "check"], "env"),
        (&outside, &["--unverified", "test"], "-"),
    ] {
        let out = aterm_uds::spawnfd::command("/bin/sh")
            .arg(&shim)
            .args(typed)
            .env_clear()
            .env("PATH", "/usr/bin:/bin")
            .env("HOME", &f.home)
            .env("PWD", cwd)
            .current_dir(cwd)
            .output()
            .unwrap();
        assert!(out.status.success(), "{out:?}");
        let words = argv();
        assert_eq!(
            words[0].as_os_str(),
            helper.as_os_str(),
            "argv[0] is the helper's path"
        );
        assert_eq!(words[4], OsString::from(placed), "{words:?}");
        let front = aterm_uds::build_front::front(&words).expect("a front");
        assert_eq!(front.target, target.as_os_str());
        assert_eq!(
            std::path::Path::new(front.target).file_name(),
            Some(std::ffi::OsStr::new("targo"))
        );
        assert!(
            front.args.ends_with(
                &strings(typed)
                    .iter()
                    .map(OsString::from)
                    .collect::<Vec<_>>()
            ),
            "{words:?}"
        );
    }
    let registration = [
        helper.clone().into_os_string(),
        OsString::from(crate::build_cache::registered::REGISTER_VERB),
        OsString::from("targo"),
        OsString::from("--unverified"),
        OsString::from("build"),
    ];
    assert!(aterm_uds::build_front::front(&registration).is_none());
}

/// A HELPER THAT DOES NOT SPEAK THE HOP KEEPS THE REGISTRATION CALL'S FORM: a macOS `targo` shim
/// whose standing helper advertises the registration capability and the older hop-v1, but
/// refuses the newer front, is laid with the registration call ahead of its block and no hop.
/// Its default call runs the target with its words once registration succeeds. NEGATIVE CONTROL:
/// the hop rendered for that helper anyway, as the renderer laid it before it asked, ends at exit
/// 2 and never runs the target; and (by hand: `helper_speaks_hop` answering yes for every helper)
/// the laid shim carries the hop and the test fails.
#[cfg(target_os = "macos")]
#[test]
fn a_helper_that_does_not_speak_the_hop_keeps_the_registration_call() {
    use std::os::unix::fs::PermissionsExt as _;
    let f = Fixture::new("build-cache-old-helper");
    let bin = f.dir("store/bin");
    std::os::unix::fs::symlink("/bin/echo", bin.join("targo")).unwrap();
    let helper = f.dir("pkg").join("atpkg");
    std::fs::write(
        &helper,
        format!(
            "#!/bin/sh\ncase \"$1\" in\n\
             {verb}) [ \"$2\" = --capabilities ] && echo {cap}; exit 0 ;;\n\
             __build_admit) [ \"$2\" = --capabilities ] && echo 'aterm-build-admit-hop-v1' && exit 0; exit 2 ;;\n\
             *) echo \"atpkg: unknown verb '$1'\" >&2; exit 2 ;;\n\
             esac\n",
            verb = crate::build_cache::registered::REGISTER_VERB,
            cap = crate::build_cache::registered::REGISTER_CAPABILITY,
        ),
    )
    .unwrap();
    std::fs::set_permissions(&helper, std::fs::Permissions::from_mode(0o755)).unwrap();
    assert!(!crate::build_cache::registered::helper_speaks_hop(&helper));
    let shim = f.dir("prefix/bin").join("targo");
    let marker = aterm_json::to_string(helper.to_string_lossy().as_ref()).unwrap();
    std::fs::write(&shim, format!("# atpkg cache helper {marker}\n")).unwrap();
    let target = bin.join("targo");
    let none = crate::shim_env::ShimEnv::NONE;
    let body = crate::platform::shim_executable_to_env(&shim, &target, &none)
        .unwrap()
        .body;
    let body = String::from_utf8(body).unwrap();
    assert!(
        body.contains(crate::build_cache::registered::REGISTER_VERB),
        "{body}"
    );
    assert!(!body.contains("__build_admit"), "{body}");
    assert_eq!(
        crate::platform::parse_sh_shim_target(&body).as_deref(),
        Some(target.as_path())
    );
    let run = |body: &str| {
        std::fs::write(&shim, body).unwrap();
        aterm_uds::spawnfd::command("/bin/sh")
            .arg(&shim)
            .args(["--unverified", "check"])
            .env_clear()
            .env("PATH", "/usr/bin:/bin")
            .env("HOME", &f.home)
            .current_dir(&f.home)
            .output()
            .unwrap()
    };
    let out = run(&body);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&out.stdout), "--unverified check\n");
    let block = crate::build_cache::shim_block("targo").unwrap();
    let plain = body.replacen(
        &crate::build_cache::registered::shim_call(&helper, "targo"),
        "",
        1,
    );
    let at = plain.find(&block).unwrap() + block.len();
    let mut hopped = plain.clone();
    hopped.insert_str(
        at,
        &crate::build_cache::registered::shim_hop(
            &helper,
            "targo",
            &crate::platform::sh_shim_quote(&target),
            None,
        ),
    );
    let out = run(&hopped);
    assert_eq!(out.status.code(), Some(2), "{out:?}");
    assert!(out.stdout.is_empty(), "the target never ran: {out:?}");
}

#[cfg(unix)]
fn age_by(path: &Path, ago: Duration) {
    let t = SystemTime::now() - ago;
    std::fs::File::open(path).unwrap().set_modified(t).unwrap();
}

#[cfg(unix)]
fn lay(root: &Path, workspace: &Path, ago: Duration) -> PathBuf {
    let leaf = build_dir(root, workspace);
    let deps = leaf.join("debug/deps");
    std::fs::create_dir_all(&deps).unwrap();
    std::fs::write(deps.join("x"), b"0123456789").unwrap();
    for p in [
        deps.join("x"),
        deps.clone(),
        leaf.join("debug"),
        leaf.clone(),
    ] {
        age_by(&p, ago);
    }
    leaf
}

#[cfg(unix)]
const DAY: Duration = Duration::from_secs(24 * 60 * 60);

#[cfg(unix)]
#[test]
fn reclaim_removes_only_what_nobody_builds_into() {
    let f = Fixture::new("build-cache-reclaim");
    let root = cache_root(&f.home);
    std::fs::create_dir_all(&root).unwrap();
    let home = Some(f.home.as_path());

    let live = f.dir("home/live");
    std::fs::write(live.join("Cargo.toml"), "[workspace]\n").unwrap();
    let live_leaf = lay(&root, &live, 2 * DAY);
    let native = f.dir("home/native");
    std::fs::write(native.join("Targo.toml"), "[workspace]\n").unwrap();
    let native_leaf = lay(&root, &native, 2 * DAY);
    let gone = f.root.join("home/gone"); // removed, its parent `home` stands
    let gone_leaf = lay(&root, &gone, 2 * DAY);
    let fresh = f.root.join("home/fresh");
    let fresh_leaf = lay(&root, &fresh, Duration::from_secs(60 * 60));
    let emptied = f.dir("home/emptied"); // the folder stands, its manifest is gone
    let emptied_leaf = lay(&root, &emptied, 3 * DAY);
    let away = f.root.join("home/away/ws"); // gone with its parent folder
    let away_leaf = lay(&root, &away, 2 * DAY);
    let private = f.root.join("home/Documents/ws"); // never looked into
    let private_leaf = lay(&root, &private, 2 * DAY);
    let volume = Path::new("/Volumes/aterm-test-no-such-volume/ws");
    let volume_leaf = lay(&root, volume, 2 * DAY);
    let mirror = |ws: &Path| root.join(ws.strip_prefix("/").unwrap());
    let leftover = mirror(&f.root.join("home/left")).join(DOOMED);
    std::fs::create_dir_all(leftover.join("debug")).unwrap();
    let beside = mirror(&emptied).join(DOOMED);
    std::fs::create_dir_all(beside.join("debug")).unwrap();

    let outside = f.dir("outside");
    std::fs::write(outside.join("keep"), b"k").unwrap();
    let linked_leaf = build_dir(&root, &f.root.join("home/linked"));
    std::fs::create_dir_all(linked_leaf.parent().unwrap()).unwrap();
    std::os::unix::fs::symlink(&outside, &linked_leaf).unwrap();
    let elsewhere = f.dir("elsewhere");
    let planted = lay(&elsewhere, Path::new("/x"), 40 * DAY);
    let via = root.join("via");
    std::os::unix::fs::symlink(&elsewhere, &via).unwrap();

    let mut removed = reclaim(&root, home, SystemTime::now());
    removed.sort();
    let mut want = vec![
        emptied_leaf.clone(),
        gone_leaf.clone(),
        fresh_leaf.clone(),
        leftover.clone(),
        beside.clone(),
    ];
    want.sort();
    assert_eq!(removed, want);
    assert!(!mirror(&gone).exists() && !mirror(&emptied).exists());
    assert!(!leftover.exists() && !mirror(&f.root.join("home/left")).exists());
    assert!(!beside.exists());
    for kept in [
        &live_leaf,
        &native_leaf,
        &away_leaf,
        &private_leaf,
        &volume_leaf,
        &planted,
    ] {
        assert!(kept.is_dir(), "{} kept", kept.display());
    }
    assert!(
        std::fs::symlink_metadata(&linked_leaf)
            .unwrap()
            .file_type()
            .is_symlink()
    );
    assert!(outside.join("keep").is_file());
    assert!(
        std::fs::symlink_metadata(&via)
            .unwrap()
            .file_type()
            .is_symlink()
    );

    let mut removed = reclaim(&root, home, SystemTime::now() + 29 * DAY);
    removed.sort();
    let mut want = vec![live_leaf, native_leaf, away_leaf, private_leaf, volume_leaf];
    want.sort();
    assert_eq!(removed, want);
    assert!(planted.is_dir());

    let old = lay(&root, &f.root.join("home/old"), 40 * DAY);
    let linked_root = f.root.join("linked-root");
    std::os::unix::fs::symlink(&root, &linked_root).unwrap();
    assert!(reclaim(&linked_root, home, SystemTime::now()).is_empty());
    assert!(old.is_dir());
}

/// A REMOVED WORKSPACE'S BUILD DIRECTORY GOES AT ONCE, at the calm daily pass as below the floor:
/// nobody builds into a removed workspace again (m20, 2026-11-05: 52.8 GiB of removed worktrees'
/// caches waited out a day's window while a verify's claim waited five hours). [`gone_leaves`]
/// lists it, and only it, without reading its tree. NEGATIVE CONTROLS: a build holding its
/// cargo lock keeps it, and a live workspace's build directory written a minute ago stays.
#[cfg(unix)]
#[test]
fn a_removed_workspaces_build_directory_goes_at_once() {
    let f = Fixture::new("build-cache-gone-now");
    let root = cache_root(&f.home);
    std::fs::create_dir_all(&root).unwrap();
    let home = Some(f.home.as_path());
    let minute = Duration::from_secs(60);
    let gone = lay(&root, &f.root.join("home/wt-just-removed"), minute);
    let busy = lay(&root, &f.root.join("home/wt-removed-mid-build"), minute);
    let lock = busy.join("debug/.cargo-lock");
    std::fs::write(&lock, b"").unwrap();
    age_tree(&busy, minute);
    let live = f.dir("home/live");
    std::fs::write(live.join("Cargo.toml"), "[workspace]\n").unwrap();
    let live_leaf = lay(&root, &live, minute);
    // A checkout mid-merge: its folder stands, its `Cargo.toml` unlinked a moment.
    let rewriting = f.dir("home/wt-rewriting");
    let rewriting_leaf = lay(&root, &rewriting, minute);

    let listed: Vec<PathBuf> = gone_leaves(&root, home).into_iter().collect();
    let mut want = vec![gone.clone(), busy.clone()];
    want.sort();
    assert_eq!(listed, want, "the gone workspaces' build directories alone");

    let held = crate::lock::Flock::try_lock(std::fs::File::open(&lock).unwrap()).unwrap();
    let went = reclaim_watched(
        &root,
        home,
        SystemTime::now(),
        ReclaimAfter::CALM,
        None,
        &mut |_, _| true,
    );
    assert!(
        matches!(went.as_slice(), [(path, Unused::Gone { idle })] if *path == gone && *idle < DAY),
        "{went:?}"
    );
    assert!(!gone.exists());
    assert!(busy.is_dir(), "a build holds its lock");
    assert!(live_leaf.is_dir(), "its workspace stands");
    assert!(
        rewriting_leaf.is_dir(),
        "a folder standing without a manifest waits the stale window"
    );

    drop(held);
    let went = reclaim_watched(
        &root,
        home,
        SystemTime::now(),
        ReclaimAfter::CALM,
        None,
        &mut |_, _| true,
    );
    assert_eq!(
        went.iter().map(|(p, _)| p.clone()).collect::<Vec<_>>(),
        vec![busy.clone()],
        "once the build lets go"
    );
    assert!(live_leaf.is_dir());
    assert!(gone_leaves(&root, home).is_empty());
}

#[cfg(unix)]
fn pressure(
    root: &Path,
    home: Option<&Path>,
    record: &mut dyn FnMut(&Path) -> bool,
) -> Vec<PathBuf> {
    reclaim_watched(
        root,
        home,
        SystemTime::now(),
        ReclaimAfter::UNDER_PRESSURE,
        None,
        &mut |p, _| record(p),
    )
    .into_iter()
    .map(|(p, _)| p)
    .collect()
}

#[cfg(unix)]
#[test]
fn reclaim_watched_says_why_each_directory_went() {
    let f = Fixture::new("build-cache-why");
    let root = cache_root(&f.home);
    std::fs::create_dir_all(&root).unwrap();
    let home = Some(f.home.as_path());
    let quarter_hour = Duration::from_secs(15 * 60);
    let gone_leaf = lay(&root, &f.root.join("home/wt-removed"), quarter_hour);
    let live = f.dir("home/live");
    std::fs::write(live.join("Cargo.toml"), "[workspace]\n").unwrap();
    let live_leaf = lay(&root, &live, 40 * DAY);
    let recent = f.dir("home/recent");
    std::fs::write(recent.join("Cargo.toml"), "[workspace]\n").unwrap();
    let recent_leaf = lay(&root, &recent, 2 * DAY);
    let stage = build_dir(&root, &f.root.join("home/orphan")).with_file_name(DOOMED);
    std::fs::create_dir_all(&stage).unwrap();

    let mut asked = Vec::new();
    let mut went = reclaim_watched(
        &root,
        home,
        SystemTime::now(),
        ReclaimAfter::UNDER_PRESSURE,
        None,
        &mut |p, why| {
            asked.push((p.to_path_buf(), why));
            true
        },
    );
    went.sort_by(|a, b| a.0.cmp(&b.0));
    let mut want = vec![gone_leaf.clone(), live_leaf.clone(), stage.clone()];
    want.sort();
    assert_eq!(
        went.iter().map(|(p, _)| p.clone()).collect::<Vec<_>>(),
        want
    );
    for (path, why) in &went {
        match why {
            Unused::Gone { idle } if *path == gone_leaf => {
                assert!(*idle >= quarter_hour && *idle < DAY, "{idle:?}");
            }
            Unused::Idle { idle } if *path == live_leaf => {
                assert!(*idle >= 40 * DAY && *idle < 41 * DAY, "{idle:?}");
            }
            Unused::Unfinished if *path == stage => {}
            other => panic!("{}: {other:?}", path.display()),
        }
        assert!(asked.contains(&(path.clone(), *why)), "asked first");
    }
    assert!(!stage.exists());
    assert!(recent_leaf.is_dir());
    assert!(!asked.iter().any(|(p, _)| *p == recent_leaf));
    assert!(live.join("Cargo.toml").is_file(), "its workspace stands");
}

/// Under pressure a removed workspace's build directory, however recently written, is asked for
/// before it goes (a record that says no keeps it); a live workspace's, one gone with its parent
/// folder and a linked one stay.
#[cfg(unix)]
#[test]
fn under_pressure_a_removed_workspaces_cache_is_asked_for_then_goes() {
    let f = Fixture::new("build-cache-pressure");
    let root = cache_root(&f.home);
    std::fs::create_dir_all(&root).unwrap();
    let home = Some(f.home.as_path());
    let quarter_hour = Duration::from_secs(15 * 60);

    let gone = f.root.join("home/wt-removed");
    let gone_leaf = lay(&root, &gone, quarter_hour);
    let just_built = f.root.join("home/wt-just-built");
    let just_leaf = lay(&root, &just_built, Duration::from_secs(60));
    let live = f.dir("home/live");
    std::fs::write(live.join("Cargo.toml"), "[workspace]\n").unwrap();
    let live_leaf = lay(&root, &live, 2 * DAY);
    let away = f.root.join("home/away/ws");
    let away_leaf = lay(&root, &away, quarter_hour);
    let outside = f.dir("outside");
    std::fs::write(outside.join("keep"), b"k").unwrap();
    let linked_leaf = build_dir(&root, &f.root.join("home/linked"));
    std::fs::create_dir_all(linked_leaf.parent().unwrap()).unwrap();
    std::os::unix::fs::symlink(&outside, &linked_leaf).unwrap();

    let mut asked = Vec::new();
    assert!(
        pressure(&root, home, &mut |p| {
            asked.push(p.to_path_buf());
            false
        })
        .is_empty()
    );
    asked.sort();
    let mut both = vec![gone_leaf.clone(), just_leaf.clone()];
    both.sort();
    assert_eq!(asked, both);
    assert!(gone_leaf.join("debug/deps/x").is_file(), "unrecorded, kept");

    let mut stood = Vec::new();
    let mut went = pressure(&root, home, &mut |p| {
        stood.push(p.join("debug/deps/x").is_file());
        true
    });
    went.sort();
    assert_eq!(went, both);
    assert_eq!(stood, vec![true, true]);
    assert!(!gone_leaf.exists() && !just_leaf.exists());
    for kept in [&live_leaf, &away_leaf] {
        assert_eq!(
            std::fs::read(kept.join("debug/deps/x")).unwrap(),
            b"0123456789",
            "{} kept",
            kept.display()
        );
    }
    assert!(
        std::fs::symlink_metadata(&linked_leaf)
            .unwrap()
            .file_type()
            .is_symlink()
    );
    assert_eq!(std::fs::read(outside.join("keep")).unwrap(), b"k");
}

#[cfg(unix)]
#[test]
fn pressure_reclamation_records_an_orphaned_stage_before_removing_it() {
    let f = Fixture::new("build-cache-orphan-intent");
    let root = cache_root(&f.home);
    let home = Some(f.home.as_path());

    let leaf = lay(
        &root,
        &f.root.join("home/removed"),
        Duration::from_secs(15 * 60),
    );
    let mut recorded = Vec::new();
    assert_eq!(
        pressure(&root, home, &mut |p| {
            recorded.push((
                p.to_path_buf(),
                std::fs::read(p.join("debug/deps/x")).unwrap(),
            ));
            true
        }),
        vec![leaf.clone()]
    );
    assert_eq!(recorded, vec![(leaf.clone(), b"0123456789".to_vec())]);
    assert!(!leaf.exists());

    let stage = build_dir(&root, &f.root.join("home/orphan")).with_file_name(DOOMED);
    std::fs::create_dir_all(&stage).unwrap();
    let payload = stage.join("retained");
    std::fs::write(&payload, b"staged bytes need an intent").unwrap();
    let mut denied = Vec::new();
    assert!(
        pressure(&root, home, &mut |p| {
            denied.push(p.to_path_buf());
            false
        })
        .is_empty()
    );
    assert_eq!(
        std::fs::read(&payload).ok().as_deref(),
        Some(b"staged bytes need an intent".as_slice()),
        "a denied orphan-stage intent must preserve its bytes"
    );
    assert_eq!(denied, vec![stage.clone()]);

    let mut allowed = Vec::new();
    let _ = pressure(&root, home, &mut |p| {
        allowed.push((p.to_path_buf(), std::fs::read(p.join("retained")).unwrap()));
        true
    });
    assert_eq!(
        allowed,
        vec![(stage.clone(), b"staged bytes need an intent".to_vec())]
    );
    assert!(!stage.exists());
}

/// `path`'s permission bits set to `bits`.
#[cfg(unix)]
pub(super) fn mode(path: &Path, bits: u32) {
    use std::os::unix::fs::PermissionsExt as _;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(bits)).unwrap();
}

/// `path`'s own permission bits.
#[cfg(unix)]
pub(super) fn mode_of(path: &Path) -> u32 {
    use std::os::unix::fs::PermissionsExt as _;
    std::fs::symlink_metadata(path)
        .unwrap()
        .permissions()
        .mode()
        & 0o7777
}

/// Every entry under `dir`, sorted, never through a link.
#[cfg(target_os = "macos")]
fn entries(dir: &Path) -> Vec<PathBuf> {
    let mut found = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(at) = stack.pop() {
        for entry in std::fs::read_dir(&at).unwrap().flatten() {
            if entry.file_type().unwrap().is_dir() {
                stack.push(entry.path());
            }
            found.push(entry.path());
        }
    }
    found.sort();
    found
}

/// The entry a test scratch's run left on its folder (`install_sh`'s ACL rows): it refuses
/// every remover, `rm -rf` and `std::fs::remove_dir_all` alike.
#[cfg(target_os = "macos")]
pub(super) const DENY_DELETE: &str = "group:everyone deny delete";

/// `chmod +a` each of `aces` on `path` itself (`-h`: a link's own ACL, never its target's).
#[cfg(target_os = "macos")]
pub(super) fn acl(path: &Path, aces: &[&str]) {
    for ace in aces {
        let status =
            aterm_uds::spawnfd::command(aterm_uds::deadline::stand_in(Path::new("/bin/chmod")))
                .args(["-h", "+a", ace])
                .arg(path)
                .status()
                .expect("chmod +a");
        assert!(
            status.success(),
            "chmod +a {ace} {}: {status}",
            path.display()
        );
    }
}

/// Whether `path`'s own ACL holds `ace` (`ls -led`).
#[cfg(target_os = "macos")]
pub(super) fn holds_ace(path: &Path, ace: &str) -> bool {
    let out = aterm_uds::spawnfd::command(aterm_uds::deadline::stand_in(Path::new("/bin/ls")))
        .arg("-led")
        .arg(path)
        .output()
        .expect("ls -led");
    String::from_utf8_lossy(&out.stdout).contains(ace)
}

/// A flag that binds even its owner (`uchg`), cleared off whichever of `at` holds it when
/// dropped, so a failed test leaves its scratch removable.
#[cfg(target_os = "macos")]
struct Unflag(Vec<PathBuf>);

#[cfg(target_os = "macos")]
impl Drop for Unflag {
    fn drop(&mut self) {
        for at in &self.0 {
            let _ = crate::platform::set_file_flags(at, 0);
        }
    }
}

/// HOW A TEST KEEPS ONE ENTRY OF A STAGE FROM GOING: read as another user's (the remover's seam:
/// it leaves such an entry as it is), on every unix; or flagged immutable (`uchg`: the remover
/// clears ACLs and owner bits, never a flag), on macOS.
#[cfg(unix)]
#[derive(Debug, Clone, Copy)]
enum Keep {
    Theirs,
    #[cfg(target_os = "macos")]
    Flagged,
}

#[cfg(unix)]
impl Keep {
    /// Why an entry kept so stays ([`remove::Why`]).
    const fn why(self) -> remove::Why {
        match self {
            Self::Theirs => remove::Why::AnotherUser,
            #[cfg(target_os = "macos")]
            Self::Flagged => remove::Why::StillRefusing,
        }
    }
}

/// A file a test keeps from going ([`Keep`]), let go when dropped, so a failed test leaves its
/// scratch removable: the flagged one by its path.
#[cfg(unix)]
enum Kept {
    Theirs,
    #[cfg(target_os = "macos")]
    Flagged(PathBuf),
}

#[cfg(unix)]
impl Kept {
    /// `path` written and kept as `keep` says. A flagged one refuses `std::fs::remove_file`
    /// itself.
    fn lay(path: &Path, keep: Keep) -> Self {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, b"held").unwrap();
        match keep {
            Keep::Theirs => {
                use std::os::unix::fs::MetadataExt as _;
                remove::seams::other_user(std::fs::symlink_metadata(path).unwrap().ino());
                Self::Theirs
            }
            #[cfg(target_os = "macos")]
            Keep::Flagged => {
                crate::platform::set_file_flags(path, libc::UF_IMMUTABLE).unwrap();
                assert!(
                    std::fs::remove_file(path).is_err(),
                    "the flag refuses its removal"
                );
                Self::Flagged(path.to_path_buf())
            }
        }
    }

    /// Let it go: it reads as this user's again, or its flag is cleared.
    fn release(&self) {
        match self {
            Self::Theirs => remove::seams::clear(),
            #[cfg(target_os = "macos")]
            Self::Flagged(path) => crate::platform::set_file_flags(path, 0).unwrap(),
        }
    }
}

#[cfg(unix)]
impl Drop for Kept {
    fn drop(&mut self) {
        remove::seams::clear();
        #[cfg(target_os = "macos")]
        if let Self::Flagged(path) = self {
            let _ = crate::platform::set_file_flags(path, 0);
        }
    }
}

/// ONE ENTRY THAT WILL NOT GO STRANDS NOTHING ELSE: a gone workspace's build directory holding,
/// beside a tree of its own, a file kept as `keep` says (read as another user's, which the
/// remover leaves as it is; or flagged `uchg`: the remover clears ACLs and owner bits, never a
/// flag, and no aterm tool sets one in a build directory) is renamed aside, and that pass removes
/// everything else in it and returns the stage as `Stayed`, never the leaf as removed: the one
/// entry counted, the bytes that went counted, and it named as what stayed; `reclaim` returns
/// nothing. The next pass is asked for the stage again, and it stays staged as it was, nothing
/// more of it gone; once the file is let go, the pass after finishes it, returned as `Unfinished`
/// after its intent, and the emptied mirror folder goes. NEGATIVE CONTROLS: a flagged file
/// refuses `std::fs::remove_file` itself ([`Kept::lay`]), and once let go the same file goes.
#[cfg(unix)]
fn a_removal_cut_short(keep: Keep) {
    let f = Fixture::new("build-cache-cut-short");
    let root = cache_root(&f.home);
    let home = Some(f.home.as_path());
    let ago = Duration::from_secs(15 * 60);
    let leaf = lay(&root, &f.root.join("home/removed"), ago);
    let stage = leaf.with_file_name(DOOMED);
    for i in 0..16 {
        let out = leaf.join(format!("debug/build/unit-{i}/out"));
        std::fs::create_dir_all(&out).unwrap();
        std::fs::write(out.join("generated.rs"), vec![b'x'; 8192]).unwrap();
    }
    age_tree(&leaf, ago);
    #[cfg(target_os = "macos")]
    let _unflag = Unflag(vec![
        leaf.join("debug/deps/stuck"),
        stage.join("debug/deps/stuck"),
    ]);
    let _kept = Kept::lay(&leaf.join("debug/deps/stuck"), keep);
    // The new name wrote `debug/deps`: aged back, so the leaf still reads unwritten.
    age_by(&leaf.join("debug/deps"), ago);
    let pass = |asked: &mut Vec<(PathBuf, Unused)>| {
        reclaim_watched(
            &root,
            home,
            SystemTime::now(),
            ReclaimAfter::UNDER_PRESSURE,
            None,
            &mut |p, w| {
                asked.push((p.to_path_buf(), w));
                true
            },
        )
    };

    let mut asked = Vec::new();
    let first = pass(&mut asked);
    assert_eq!(asked.len(), 1, "{asked:?}");
    assert_eq!(asked[0].0, leaf);
    assert!(matches!(asked[0].1, Unused::Gone { .. }), "{asked:?}");
    assert_eq!(first.len(), 1, "{first:?}");
    assert_eq!(first[0].0, stage, "the stage, never the leaf as removed");
    let Unused::Stayed { stayed, freed } = first[0].1 else {
        panic!("returned as removed before it went: {first:?}");
    };
    assert_eq!(stayed, 1);
    assert!(
        freed >= 16 * 8192,
        "the bytes that went are counted: {freed}"
    );
    assert!(!leaf.exists());
    assert_eq!(
        remove::stayed(&stage, 10),
        [stage.join("debug/deps/stuck")],
        "every other entry went"
    );
    assert_eq!(
        remove::named(&stage, stayed).first,
        [(PathBuf::from("debug/deps/stuck"), keep.why())]
    );
    assert!(
        reclaim(&root, home, SystemTime::now()).is_empty(),
        "reclaim returns only what went whole"
    );

    asked.clear();
    let again = pass(&mut asked);
    assert_eq!(asked, [(stage.clone(), Unused::Unfinished)], "asked again");
    assert_eq!(
        again,
        [(
            stage.clone(),
            Unused::Stayed {
                stayed: 1,
                freed: 0
            }
        )]
    );
    assert_eq!(remove::stayed(&stage, 10), [stage.join("debug/deps/stuck")]);
    assert_eq!(
        std::fs::read(stage.join("debug/deps/stuck")).unwrap(),
        b"held"
    );

    // Let go by its moved path: a flagged `Kept` holds the leaf's, which the rename left stale.
    match keep {
        Keep::Theirs => remove::seams::clear(),
        #[cfg(target_os = "macos")]
        Keep::Flagged => {
            crate::platform::set_file_flags(&stage.join("debug/deps/stuck"), 0).unwrap();
        }
    }
    asked.clear();
    let went = pass(&mut asked);
    assert_eq!(went, [(stage.clone(), Unused::Unfinished)]);
    assert_eq!(asked, went, "its intent asked before it went");
    assert!(!stage.exists() && !leaf.parent().unwrap().exists());
}

#[cfg(target_os = "macos")]
#[test]
fn a_removal_cut_short_is_returned_by_the_pass_that_finishes_it() {
    a_removal_cut_short(Keep::Flagged);
}

#[cfg(unix)]
#[test]
fn a_stage_keeping_an_entry_is_asked_again_and_counted_until_it_goes() {
    a_removal_cut_short(Keep::Theirs);
}

/// A STUCK STAGE COSTS ONLY ITSELF (DISK4-1): beside `.cargo-build.removing` (the one name an
/// older aterm staged under) keeping one entry as `keep` says, a gone workspace's build directory
/// is staged under the next free name (`.cargo-build.removing-1`) and goes whole in the same
/// pass, while the stuck stage is asked for and reported once a pass (`Stayed`, its other bytes
/// counted the first time), never twice. The plan lists each once, the leaf as going. The
/// workspace builds again and the next pass takes that build directory too; once the entry lets
/// go, the old fixed-name stage is finished, returned as `Unfinished`, and its mirror folder goes.
#[cfg(unix)]
fn a_stuck_stage_costs_only_itself(keep: Keep) {
    let f = Fixture::new("build-cache-stuck-stage");
    let root = cache_root(&f.home);
    let home = Some(f.home.as_path());
    let ago = Duration::from_secs(15 * 60);
    let workspace = f.root.join("home/removed");
    let leaf = lay(&root, &workspace, ago);
    let stage = leaf.with_file_name(DOOMED);
    let next = leaf.with_file_name(format!("{DOOMED}-1"));
    std::fs::create_dir_all(stage.join("debug/build")).unwrap();
    std::fs::write(stage.join("debug/build/spare"), vec![b'x'; 64 * 1024]).unwrap();
    let held = stage.join("debug/deps/held");
    let kept = Kept::lay(&held, keep);
    let gone = |why: &Unused| matches!(why, Unused::Gone { .. });
    let planned = || {
        plan(
            &root,
            home,
            SystemTime::now(),
            ReclaimAfter::UNDER_PRESSURE,
            None,
        )
    };
    let pass = |asked: &mut Vec<(PathBuf, Unused)>| {
        let mut went = reclaim_watched(
            &root,
            home,
            SystemTime::now(),
            ReclaimAfter::UNDER_PRESSURE,
            None,
            &mut |p, w| {
                asked.push((p.to_path_buf(), w));
                true
            },
        );
        went.sort_by(|a, b| a.0.cmp(&b.0));
        asked.sort_by(|a, b| a.0.cmp(&b.0));
        went
    };

    let before = planned();
    assert_eq!(
        before.iter().map(|p| p.path.clone()).collect::<Vec<_>>(),
        [leaf.clone(), stage.clone()],
        "{before:?}"
    );
    assert!(gone(&before[0].why) && before[1].why == Unused::Unfinished);

    for round in 0..2 {
        if round > 0 {
            assert_eq!(
                lay(&root, &workspace, ago),
                leaf,
                "the workspace built again"
            );
        }
        let mut asked = Vec::new();
        let went = pass(&mut asked);
        assert_eq!(asked.len(), 2, "round {round}, each asked once: {asked:?}");
        assert!(asked[0].0 == leaf && gone(&asked[0].1), "{asked:?}");
        assert_eq!(asked[1], (stage.clone(), Unused::Unfinished));
        assert_eq!(went.len(), 2, "round {round}, each once: {went:?}");
        assert!(
            went[0].0 == leaf && gone(&went[0].1),
            "round {round}: the leaf went whole in the same pass: {went:?}"
        );
        let (path, Unused::Stayed { stayed, freed }) = &went[1] else {
            panic!("round {round}: {went:?}");
        };
        assert_eq!((path, *stayed), (&stage, 1));
        if round == 0 {
            assert!(*freed >= 64 * 1024, "the bytes that went: {freed}");
        } else {
            assert_eq!(*freed, 0);
        }
        assert!(!leaf.exists() && !next.exists(), "round {round}");
        assert_eq!(remove::stayed(&stage, 10), std::slice::from_ref(&held));
        let after = planned();
        assert_eq!(
            after.iter().map(|p| (&p.path, p.why)).collect::<Vec<_>>(),
            [(&stage, Unused::Unfinished)],
            "round {round}"
        );
    }

    kept.release();
    let mut asked = Vec::new();
    let went = pass(&mut asked);
    assert_eq!(
        went,
        [(stage.clone(), Unused::Unfinished)],
        "the old fixed-name stage is finished"
    );
    assert_eq!(asked, went, "its intent asked before it went");
    assert!(!stage.exists() && !leaf.parent().unwrap().exists());
}

#[cfg(unix)]
#[test]
fn a_stage_keeping_another_users_entry_costs_only_itself() {
    a_stuck_stage_costs_only_itself(Keep::Theirs);
}

/// THE DRY PLAN FORECASTS WHAT OF A STAGE WILL STAY: of two stages an earlier pass left in one
/// mirror folder, the one holding a file read as another user's (the remover's seam: a removal
/// leaves it as it is) is planned as unfinished with that file named under it for that reason,
/// read and never acted on (the file is still there, and the plan asks nothing); the one holding
/// only this user's entries is planned and worded as an unfinished stage alone, as before the
/// forecast. NEGATIVE CONTROL: the other user's file is there to be named, a plan without the
/// forecast saying no more of its stage than of the other.
#[cfg(unix)]
#[test]
fn a_dry_plan_forecasts_what_of_a_stage_will_stay() {
    let f = Fixture::new("build-cache-forecast");
    let root = cache_root(&f.home);
    let leaf = build_dir(&root, &f.root.join("home/ws"));
    let (stuck, clean) = (
        leaf.with_file_name(DOOMED),
        leaf.with_file_name(format!("{DOOMED}-1")),
    );
    std::fs::create_dir_all(clean.join("debug/deps")).unwrap();
    std::fs::write(clean.join("debug/deps/x"), b"x").unwrap();
    std::fs::create_dir_all(stuck.join("debug/build")).unwrap();
    std::fs::write(stuck.join("debug/build/spare"), b"s").unwrap();
    let held = stuck.join("debug/deps/held");
    let _kept = Kept::lay(&held, Keep::Theirs);

    let planned = plan(
        &root,
        Some(&f.home),
        SystemTime::now(),
        ReclaimAfter::CALM,
        None,
    );
    assert_eq!(
        planned.iter().map(|p| (&p.path, p.why)).collect::<Vec<_>>(),
        [(&stuck, Unused::Unfinished), (&clean, Unused::Unfinished)],
        "{planned:#?}"
    );
    let stays = planned[0].stays.clone().expect("a forecast of the stage");
    assert_eq!(
        (stays.count, stays.first, stays.whole),
        (
            1,
            vec![(PathBuf::from("debug/deps/held"), remove::Why::AnotherUser)],
            true
        )
    );
    assert_eq!(std::fs::read(&held).unwrap(), b"held", "the plan acted");
    let words = crate::cli::cache_plan_line(&planned[0]);
    let said = planned[0].stays.as_ref().and_then(|f| f.words(&stuck));
    assert_eq!(
        words,
        format!(
            "{}; {}",
            crate::cli::cache_plan_words(Unused::Unfinished),
            said.clone().unwrap_or_default()
        )
    );
    assert!(
        said.is_some_and(
            |w| w.contains("debug/deps/held") && w.contains(&remove::Why::AnotherUser.words(&held))
        ),
        "{words}"
    );

    let none = planned[1].stays.clone().expect("a forecast of the stage");
    assert_eq!((none.count, none.whole), (0, true), "{none:?}");
    assert_eq!(none.words(&clean), None);
    assert_eq!(
        crate::cli::cache_plan_line(&planned[1]),
        crate::cli::cache_plan_words(Unused::Unfinished),
        "a stage with nothing to stay is worded as before"
    );
}

#[cfg(target_os = "macos")]
#[test]
fn a_stage_keeping_a_flagged_entry_costs_only_itself() {
    a_stuck_stage_costs_only_itself(Keep::Flagged);
}

/// A STUCK STAGE KEEPS NO PRUNE: beside a stage that keeps one entry ([`Keep::Theirs`]), a live
/// workspace's stale incremental folder moves into a new stage of its own
/// (`.cargo-build.removing-1`) and goes in the same pass, returned as `Stale`, while the stuck
/// stage is asked for and reported once; the build directory stays.
#[cfg(unix)]
#[test]
fn a_stuck_stage_beside_a_live_build_directory_keeps_no_prune() {
    let f = Fixture::new("build-cache-stuck-prune");
    let root = cache_root(&f.home);
    let live = f.dir("home/live");
    std::fs::write(live.join("Cargo.toml"), "[workspace]\n").unwrap();
    let leaf = build_dir(&root, &live);
    let profile = leaf.join("debug");
    std::fs::create_dir_all(&profile).unwrap();
    std::fs::write(profile.join(".cargo-build-lock"), b"").unwrap();
    let stale_crate = profile.join("incremental/ws-1k116trr0a0xf");
    std::fs::create_dir_all(stale_crate.join("s-hneouhc0uf-1tn8o9j-2awpw")).unwrap();
    age_tree(&stale_crate, 4 * DAY);
    let stage = leaf.with_file_name(DOOMED);
    let held = stage.join("tmp/held");
    let _kept = Kept::lay(&held, Keep::Theirs);

    let mut asked = Vec::new();
    let mut went = reclaim_watched(
        &root,
        Some(f.home.as_path()),
        SystemTime::now(),
        ReclaimAfter::CALM,
        None,
        &mut |p, w| {
            asked.push((p.to_path_buf(), w));
            true
        },
    );
    went.sort_by(|a, b| a.0.cmp(&b.0));
    asked.sort_by(|a, b| a.0.cmp(&b.0));
    let pruned = leaf.with_file_name(format!("{DOOMED}-1"));
    let stale = |why: &Unused| matches!(why, Unused::Stale { incremental: 1, .. });
    assert_eq!(asked.len(), 2, "{asked:?}");
    assert_eq!(asked[0], (stage.clone(), Unused::Unfinished));
    assert!(asked[1].0 == pruned && stale(&asked[1].1), "{asked:?}");
    assert_eq!(went.len(), 2, "{went:?}");
    assert_eq!(
        went[0],
        (
            stage.clone(),
            Unused::Stayed {
                stayed: 1,
                freed: 0
            }
        )
    );
    assert!(went[1].0 == pruned && stale(&went[1].1), "{went:?}");
    assert!(!stale_crate.exists() && !pruned.exists());
    assert!(
        profile.join(".cargo-build-lock").is_file(),
        "the leaf stays"
    );
    assert_eq!(std::fs::read(&held).unwrap(), b"held");
}

/// A STUCK ASIDE KEEPS NO TARGET, AND IS SAID: beside two asides of earlier calls that each keep
/// one entry ([`Keep::Theirs`]), `<dir>.removing` (the one name an older aterm used) and the
/// numbered `<dir>.removing-2`, a target directory nothing builds in is renamed aside under the
/// lowest free name (`-1`) and goes whole, each earlier aside tried and kept as it was, and the
/// call says what stayed of each (`stayed_words`, joined), though the target went; with the target
/// gone, a call says the same; once their entries let go, a call finishes both (`Ok(false)`: no
/// target), and nothing is left beside the target.
#[cfg(unix)]
#[test]
fn a_stuck_aside_keeps_no_target_from_going() {
    let f = Fixture::new("build-cache-stuck-aside");
    let target = f.dir("target/verify/cells.noindex");
    std::fs::create_dir_all(target.join("debug/deps")).unwrap();
    std::fs::write(target.join("debug/deps/x.rmeta"), b"x").unwrap();
    let bare = target.with_file_name("cells.noindex.removing");
    let numbered = target.with_file_name("cells.noindex.removing-2");
    let held = [bare.join("debug/deps/held"), numbered.join("held")];
    let kept = held.each_ref().map(|h| Kept::lay(h, Keep::Theirs));
    let words = || {
        format!(
            "{}; {}",
            remove::stayed_words(&bare, 1),
            remove::stayed_words(&numbered, 1)
        )
    };

    let said = remove_unbuilt(&target).unwrap_err().to_string();
    assert_eq!(said, words(), "each stuck aside is said, the target gone");
    assert!(!target.exists());
    assert!(!target.with_file_name("cells.noindex.removing-1").exists());
    for h in &held {
        assert_eq!(std::fs::read(h).unwrap(), b"held");
    }
    let said = remove_unbuilt(&target).unwrap_err().to_string();
    assert_eq!(said, words(), "no target");

    kept[0].release();
    assert!(!remove_unbuilt(&target).unwrap(), "no target");
    let listed: Vec<_> = std::fs::read_dir(target.parent().unwrap())
        .unwrap()
        .flatten()
        .map(|e| e.file_name())
        .collect();
    assert!(listed.is_empty(), "nothing beside it: {listed:?}");
}

/// A LEAF THAT WILL NOT MOVE IS NEITHER PLANNED NOR ASKED FOR: a gone workspace's build directory
/// its owner flagged `uchg`, or whose own ACL denies its deletion, in a folder that takes new
/// names, cannot be renamed aside (the kernel refuses its rename as it would its removal), so the
/// plan does not list it, its intent is never asked, and it stays whole. NEGATIVE CONTROLS: each
/// refuses `std::fs::rename` itself; with the flag off and the ACL cleared, the plan lists both
/// and a pass takes both.
#[cfg(target_os = "macos")]
#[test]
fn a_leaf_that_will_not_move_is_neither_planned_nor_asked_for() {
    /// Clears the flag and the ACL off each leaf when dropped, so a failed test leaves its
    /// scratch removable.
    struct Let(Vec<PathBuf>);
    impl Drop for Let {
        fn drop(&mut self) {
            for at in &self.0 {
                let _ = crate::platform::set_file_flags(at, 0);
                let _ = aterm_uds::spawnfd::command(aterm_uds::deadline::stand_in(Path::new(
                    "/bin/chmod",
                )))
                .arg("-N")
                .arg(at)
                .stderr(aterm_uds::spawnfd::Stdio::null())
                .status();
            }
        }
    }
    let f = Fixture::new("build-cache-unmovable");
    let root = cache_root(&f.home);
    let home = Some(f.home.as_path());
    let flagged = lay(&root, &f.root.join("home/flagged"), 2 * DAY);
    let denied = lay(&root, &f.root.join("home/denied"), 2 * DAY);
    let both = Let(vec![denied.clone(), flagged.clone()]);
    crate::platform::set_file_flags(&flagged, libc::UF_IMMUTABLE).unwrap();
    acl(&denied, &[DENY_DELETE]);
    for leaf in &both.0 {
        let folder = leaf.parent().unwrap();
        assert!(crate::platform::dir_writable_by_caller(folder));
        assert!(
            std::fs::rename(leaf, folder.join("moved")).is_err(),
            "{} refuses its rename",
            leaf.display()
        );
    }
    assert!(plan(&root, home, SystemTime::now(), ReclaimAfter::CALM, None).is_empty());
    let mut asked = Vec::new();
    let went = reclaim_watched(
        &root,
        home,
        SystemTime::now(),
        ReclaimAfter::CALM,
        None,
        &mut |p, w| {
            asked.push((p.to_path_buf(), w));
            true
        },
    );
    assert!(went.is_empty() && asked.is_empty(), "{went:?} {asked:?}");
    for leaf in &both.0 {
        assert!(leaf.join("debug/deps/x").is_file());
    }

    crate::platform::set_file_flags(&flagged, 0).unwrap();
    let cleared =
        aterm_uds::spawnfd::command(aterm_uds::deadline::stand_in(Path::new("/bin/chmod")))
            .arg("-N")
            .arg(&denied)
            .status()
            .unwrap();
    assert!(cleared.success());
    let mut planned: Vec<PathBuf> = plan(&root, home, SystemTime::now(), ReclaimAfter::CALM, None)
        .into_iter()
        .map(|p| p.path)
        .collect();
    planned.sort();
    assert_eq!(planned, both.0);
    let mut went = reclaim(&root, home, SystemTime::now());
    went.sort();
    assert_eq!(went, both.0);
}

/// A LEAF THAT CANNOT BE STAGED IS NEITHER PLANNED NOR ASKED FOR: in a mirror folder this user
/// may add no name to (0o555), a gone workspace's build directory is not listed by the plan, its
/// intent is never asked, and it stays whole. NEGATIVE CONTROL: the folder's write bit back, the
/// plan lists it and a pass takes it.
#[cfg(unix)]
#[test]
fn a_leaf_that_cannot_be_staged_is_neither_planned_nor_asked_for() {
    struct Writable(PathBuf);
    impl Drop for Writable {
        fn drop(&mut self) {
            use std::os::unix::fs::PermissionsExt as _;
            let _ = std::fs::set_permissions(&self.0, std::fs::Permissions::from_mode(0o755));
        }
    }
    let f = Fixture::new("build-cache-unstageable");
    let root = cache_root(&f.home);
    let home = Some(f.home.as_path());
    let leaf = lay(&root, &f.root.join("home/removed"), 2 * DAY);
    let folder = Writable(leaf.parent().unwrap().to_path_buf());
    mode(&folder.0, 0o555);
    assert!(plan(&root, home, SystemTime::now(), ReclaimAfter::CALM, None).is_empty());
    let mut asked = Vec::new();
    let went = reclaim_watched(
        &root,
        home,
        SystemTime::now(),
        ReclaimAfter::CALM,
        None,
        &mut |p, w| {
            asked.push((p.to_path_buf(), w));
            true
        },
    );
    assert!(went.is_empty() && asked.is_empty(), "{went:?} {asked:?}");
    assert!(leaf.join("debug/deps/x").is_file());

    mode(&folder.0, 0o755);
    let planned: Vec<PathBuf> = plan(&root, home, SystemTime::now(), ReclaimAfter::CALM, None)
        .into_iter()
        .map(|p| p.path)
        .collect();
    assert_eq!(planned, std::slice::from_ref(&leaf));
    assert_eq!(
        reclaim(&root, home, SystemTime::now()),
        std::slice::from_ref(&leaf)
    );
    assert!(!leaf.exists());
}

/// A RENAME REFUSED ONCE THE INTENT IS ON RECORD CLOSES THAT INTENT (DISK4's residual): a gone
/// workspace's build directory passes the check, its intent is asked and put on record, and
/// only then, before its rename aside ([`seams::on_staging`]), is the rename made to fail: its
/// folder set to take no new name (0o555, every Unix, not as root), or, on macOS, the leaf
/// flagged `uchg` (which its owner may set). The pass returns it refused by the rename's error
/// kind, 0 bytes gone, and it stays whole; [`reclaim`] returns nothing of it; a later pass with
/// the refusal cleared takes it. NEGATIVE CONTROL: without the outcome, the pass returns nothing
/// for the intent it put on record.
#[cfg(unix)]
#[test]
fn a_rename_refused_after_its_intent_closes_that_intent_with_nothing_gone() {
    /// Forgets the call and undoes the refusal when dropped, so a failed test leaves its scratch
    /// removable.
    struct Undo(PathBuf, fn(&Path));
    impl Drop for Undo {
        fn drop(&mut self) {
            seams::clear();
            (self.1)(&self.0);
        }
    }
    type Refuse = (&'static str, fn(&Path), fn(&Path));
    let mut refusals: Vec<Refuse> = Vec::new();
    if crate::platform::our_uid() != 0 {
        refusals.push((
            "a folder that takes no new name",
            |leaf| mode(leaf.parent().unwrap(), 0o555),
            |leaf| {
                use std::os::unix::fs::PermissionsExt as _;
                let _ = std::fs::set_permissions(
                    leaf.parent().unwrap(),
                    std::fs::Permissions::from_mode(0o755),
                );
            },
        ));
    }
    #[cfg(target_os = "macos")]
    refusals.push((
        "uchg",
        |leaf| crate::platform::set_file_flags(leaf, libc::UF_IMMUTABLE).unwrap(),
        |leaf| {
            let _ = crate::platform::set_file_flags(leaf, 0);
        },
    ));
    assert!(!refusals.is_empty(), "no refusal this user can set");
    let f = Fixture::new("build-cache-refused-rename");
    let root = cache_root(&f.home);
    let home = Some(f.home.as_path());
    let refused = Unused::Refused {
        by: Refusal::Rename(std::io::ErrorKind::PermissionDenied),
    };
    for (n, (what, refuse, undo)) in refusals.into_iter().enumerate() {
        let leaf = lay(&root, &f.root.join(format!("home/removed-{n}")), 2 * DAY);
        let undone = Undo(leaf.clone(), undo);
        seams::on_staging(refuse);
        assert!(
            reclaim(&root, home, SystemTime::now()).is_empty(),
            "{what}: nothing went"
        );
        undo(&leaf);
        let mut asked = Vec::new();
        let went = reclaim_watched(
            &root,
            home,
            SystemTime::now(),
            ReclaimAfter::CALM,
            None,
            &mut |p, _| {
                asked.push(p.to_path_buf());
                true
            },
        );
        assert_eq!(asked, std::slice::from_ref(&leaf), "{what}: its intent");
        assert_eq!(went, [(leaf.clone(), refused)], "{what}: its outcome");
        assert!(leaf.join("debug/deps/x").is_file(), "{what}: whole");
        assert!(
            stages_in(leaf.parent().unwrap(), OsStr::new(DOOMED)).is_empty(),
            "{what}: never staged"
        );
        drop(undone);
        assert_eq!(
            reclaim(&root, home, SystemTime::now()),
            std::slice::from_ref(&leaf),
            "{what}: the next pass takes it"
        );
        assert!(!leaf.exists(), "{what}");
    }
}

/// A REMOVAL STOPPED ONCE ITS ENTRY IS STAGED CLOSES AS ITS STAGE: a gone workspace's build
/// directory whose rename aside went through, and that a process comes to use once its intent is
/// on record (the fence reports it in its stage), is returned as that stage with nothing of it
/// gone ([`Refusal::Staged`]), never as the leaf it was asked for, which no longer holds it; the
/// stage holds it all. A stage an earlier pass left, stopped the same way after its intent, is
/// returned as that stage, `Staged`; and the next pass, the process gone, finishes it
/// ([`Unused::Unfinished`]). NEGATIVE CONTROL: the move not marked on the open intent
/// ([`Intents::moved`]), the pass returns the gone leaf as [`Refusal::Stopped`].
#[cfg(unix)]
#[test]
fn a_removal_stopped_once_staged_closes_as_its_stage() {
    let f = Fixture::new("build-cache-stopped-staged");
    let root = cache_root(&f.home);
    let leaf = lay(&root, &f.root.join("home/removed"), 2 * DAY);
    let dir = leaf.parent().unwrap().to_path_buf();
    let (held, asked) = (std::cell::Cell::new(true), std::cell::Cell::new(false));
    // A process in a stage of the leaf's folder, from the moment an intent is on record.
    let processes = |dirs: &[PathBuf], _: &[u32]| {
        let users = dirs
            .iter()
            .filter(|d| {
                held.get()
                    && asked.get()
                    && d.file_name()
                        .is_some_and(|name| is_stage(name, OsStr::new(DOOMED)))
            })
            .map(|stage| aterm_uds::holders::PathUse {
                pid: 34,
                how: "cwd",
                path: stage.join("debug"),
            })
            .collect();
        Ok(aterm_uds::holders::PathScan {
            users,
            ..aterm_uds::holders::PathScan::default()
        })
    };
    let fence = fence::Fence::new(&processes);
    let pass = || {
        asked.set(false);
        reclaim_watched_on(
            &root,
            Some(&f.home),
            SystemTime::now(),
            ReclaimAfter::CALM,
            None,
            &mut |_, _| {
                asked.set(true);
                true
            },
            &fence,
        )
    };
    let staged = Unused::Refused {
        by: Refusal::Staged,
    };
    let went = pass();
    let stages = stages_in(&dir, OsStr::new(DOOMED));
    assert_eq!(stages.len(), 1, "moved aside: {went:?}");
    let stage = stages[0].clone();
    assert_eq!(
        went,
        [(stage.clone(), staged)],
        "its stage, nothing of it gone"
    );
    assert!(!leaf.exists(), "the leaf no longer holds it");
    assert!(
        stage.join("debug/deps/x").is_file(),
        "the stage holds it all"
    );
    assert_eq!(
        pass(),
        [(stage.clone(), staged)],
        "a stage left, stopped after its intent, is that stage"
    );
    held.set(false);
    assert_eq!(
        pass(),
        [(stage.clone(), Unused::Unfinished)],
        "the next pass finishes it"
    );
    assert!(!stage.exists());
}

/// AN IMMUTABLE LEAF IS NEITHER PLANNED NOR ASKED FOR, ON LINUX: a gone workspace's build
/// directory in a folder that takes new names, whose inode `statx` reads as immutable
/// (`STATX_ATTR_IMMUTABLE`, set and reported; the stand-in `platform::seams`, since `chattr +i`
/// needs `CAP_LINUX_IMMUTABLE`), is not listed by the plan, its intent is never asked, and it
/// stays whole; read with `statx` unanswered (`ENOSYS`), it is read as before, by its folder's
/// write bit, and is planned and goes. As root, where the scratch's filesystem takes it, the
/// real `chattr +i` as well, under a guard that clears it. NEGATIVE CONTROL: the attribute read
/// taken out of `may_take_name`, the immutable leaf is planned.
#[cfg(all(target_os = "linux", target_env = "gnu"))]
#[test]
fn an_immutable_leaf_is_neither_planned_nor_asked_for_on_linux() {
    use crate::platform::seams;
    struct Clear(Option<PathBuf>);
    impl Drop for Clear {
        fn drop(&mut self) {
            seams::clear();
            if let Some(leaf) = &self.0 {
                let _ = aterm_uds::spawnfd::command("chattr")
                    .arg("-i")
                    .arg(leaf)
                    .stderr(aterm_uds::spawnfd::Stdio::null())
                    .status();
            }
        }
    }
    let f = Fixture::new("build-cache-immutable");
    let root = cache_root(&f.home);
    let home = Some(f.home.as_path());
    let planned = |root: &Path| -> Vec<PathBuf> {
        plan(root, home, SystemTime::now(), ReclaimAfter::CALM, None)
            .into_iter()
            .map(|p| p.path)
            .collect()
    };
    let leaf = lay(&root, &f.root.join("home/removed"), 2 * DAY);
    let mut clear = Clear(None);
    seams::attributes_of(&leaf, Some(seams::IMMUTABLE));
    assert!(crate::platform::dir_writable_by_caller(
        leaf.parent().unwrap()
    ));
    assert!(
        planned(&root).is_empty(),
        "an immutable leaf is not planned"
    );
    let mut asked = Vec::new();
    let went = reclaim_watched(
        &root,
        home,
        SystemTime::now(),
        ReclaimAfter::CALM,
        None,
        &mut |p, w| {
            asked.push((p.to_path_buf(), w));
            true
        },
    );
    assert!(went.is_empty() && asked.is_empty(), "{went:?} {asked:?}");
    assert!(leaf.join("debug/deps/x").is_file());

    seams::attributes_of(&leaf, None);
    assert_eq!(
        planned(&root),
        std::slice::from_ref(&leaf),
        "statx unanswered"
    );
    assert_eq!(
        reclaim(&root, home, SystemTime::now()),
        std::slice::from_ref(&leaf)
    );
    assert!(!leaf.exists());
    seams::clear();

    if crate::platform::our_uid() != 0 {
        return;
    }
    let leaf = lay(&root, &f.root.join("home/removed-real"), 2 * DAY);
    clear.0 = Some(leaf.clone());
    let set = aterm_uds::spawnfd::command("chattr")
        .arg("+i")
        .arg(&leaf)
        .stderr(aterm_uds::spawnfd::Stdio::null())
        .status()
        .is_ok_and(|s| s.success());
    if !set {
        eprintln!("chattr +i not taken here: the real leg is not run");
        return;
    }
    assert!(planned(&root).is_empty(), "chattr +i: not planned");
    let mut asked = Vec::new();
    let went = reclaim_watched(
        &root,
        home,
        SystemTime::now(),
        ReclaimAfter::CALM,
        None,
        &mut |p, _| {
            asked.push(p.to_path_buf());
            true
        },
    );
    assert!(went.is_empty() && asked.is_empty(), "{went:?} {asked:?}");
    drop(clear);
    assert_eq!(
        reclaim(&root, home, SystemTime::now()),
        std::slice::from_ref(&leaf)
    );
}

#[cfg(unix)]
#[test]
fn unproducible_stage_ordinals_do_not_retire_live_workspace_caches() {
    for suffix in [
        "4294967296",
        "18446744073709551615",
        "9999999999999999999999999",
    ] {
        let f = Fixture::new("build-cache-stage-ordinal");
        let root = cache_root(&f.home);
        let workspace = f.home.join(format!("{DOOMED}-{suffix}"));
        std::fs::create_dir_all(&workspace).unwrap();
        std::fs::write(workspace.join("Cargo.toml"), "[workspace]\n").unwrap();
        let leaf = lay(&root, &workspace, 2 * DAY);
        let artifact = leaf.join("debug/deps/x");
        let before = std::fs::read(&artifact).unwrap();
        let stage = leaf
            .parent()
            .unwrap()
            .parent()
            .unwrap()
            .join(stage_name(OsStr::new(DOOMED), u32::MAX - 1));
        std::fs::create_dir(&stage).unwrap();
        std::fs::write(stage.join("retired"), b"completed removal stage").unwrap();

        let removed = reclaim(&root, Some(f.home.as_path()), SystemTime::now());

        assert_eq!(std::fs::read(&artifact).unwrap(), before);
        assert_eq!(removed, std::slice::from_ref(&stage));
        assert!(
            !stage.exists(),
            "a producer-valid stranded stage is still retired"
        );
    }
}

/// EVERY STAGE NAME IS READ AS A STAGE, AND NOTHING ELSE IS: `.cargo-build.removing` and
/// `.cargo-build.removing-<n>` (n from 1, no leading zero) are stages, planned and finished once
/// each and never looked into for a workspace, while `-0`, `-01`, `-x`, a bare `-` and other
/// suffixes are mirror folders, walked as any other.
#[cfg(unix)]
#[test]
fn every_stage_name_is_read_as_a_stage_and_nothing_else_is() {
    let base = OsStr::new(DOOMED);
    for (name, stage) in [
        (DOOMED.to_owned(), true),
        (format!("{DOOMED}-1"), true),
        (format!("{DOOMED}-12"), true),
        (format!("{DOOMED}-0"), false),
        (format!("{DOOMED}-01"), false),
        (format!("{DOOMED}-x"), false),
        (format!("{DOOMED}-1x"), false),
        (format!("{DOOMED}-"), false),
        (format!("{DOOMED}x"), false),
        (LEAF.to_owned(), false),
    ] {
        assert_eq!(is_stage(OsStr::new(&name), base), stage, "{name}");
    }
    assert_eq!(stage_name(base, 0), DOOMED);
    assert_eq!(stage_name(base, 7), format!("{DOOMED}-7").as_str());

    let f = Fixture::new("build-cache-stage-names");
    let root = cache_root(&f.home);
    let ws = f.root.join("home/ws");
    let mirror = root.join(ws.strip_prefix("/").unwrap());
    let numbered = mirror.join(format!("{DOOMED}-12"));
    std::fs::create_dir_all(numbered.join(LEAF).join("debug")).unwrap();
    let walked = mirror.join(format!("{DOOMED}-01"));
    std::fs::create_dir_all(walked.join(LEAF)).unwrap();
    assert_eq!(workspaces(&root), [ws.join(format!("{DOOMED}-01"))]);
    let planned: Vec<(PathBuf, Unused)> = plan(
        &root,
        Some(f.home.as_path()),
        SystemTime::now(),
        ReclaimAfter::CALM,
        None,
    )
    .into_iter()
    .map(|p| (p.path, p.why))
    .collect();
    assert_eq!(planned, [(numbered.clone(), Unused::Unfinished)]);
    assert_eq!(
        reclaim(&root, Some(f.home.as_path()), SystemTime::now()),
        std::slice::from_ref(&numbered)
    );
    assert!(!numbered.exists() && walked.join(LEAF).is_dir());
}

/// THE STRANDED STAGE GOES IN ONE PASS (found live 2026-10-27, 252 GB stranded): under the root
/// the cache moved from, a stage whose FIRST entry in readdir order holds, as its `tmp/` did, a
/// test scratch's folder carrying `group:everyone deny delete` and the allow entry that run
/// added, beside a large tree. One pass clears the ACL aterm's own test put there, removes every
/// entry, returns the stage whole as `Unfinished` after its intent, and the emptied mirror
/// folders and the root go. NEGATIVE CONTROL: the same stage laid outside the cache,
/// `std::fs::remove_dir_all` — every pass's removal before — fails at that folder and leaves
/// every entry.
#[cfg(target_os = "macos")]
#[test]
fn a_stage_led_by_a_folder_denying_its_removal_goes_whole_in_one_pass() {
    let f = Fixture::new("build-cache-acl-stage");
    let old = moved_from(&f.home);
    let stage = build_dir(&old, &f.root.join("home/p2")).with_file_name(DOOMED);
    let control = f.root.join("control").join(DOOMED);
    let lay_stage = |stage: &Path| {
        for top in ["tmp", "debug", "release", "build"] {
            std::fs::create_dir_all(stage.join(top)).unwrap();
        }
        let first = || {
            std::fs::read_dir(stage)
                .unwrap()
                .next()
                .unwrap()
                .unwrap()
                .path()
        };
        let lead = first();
        let locked = lead.join("install-sh.20116_7cc62c51a515c111_119/tmp/d");
        std::fs::create_dir_all(&locked).unwrap();
        for sibling in ["tmp", "debug", "release", "build"]
            .map(|top| stage.join(top))
            .into_iter()
            .filter(|top| *top != lead)
        {
            for i in 0..8 {
                let deps = sibling.join(format!("deps-{i}"));
                std::fs::create_dir_all(&deps).unwrap();
                for j in 0..64 {
                    std::fs::write(deps.join(format!("lib{j}.rlib")), b"rlib").unwrap();
                }
            }
        }
        acl(
            &locked,
            &[DENY_DELETE, "group:everyone allow add_subdirectory"],
        );
        assert_eq!(
            first(),
            lead,
            "the locked folder's entry leads the readdir order"
        );
        locked
    };

    let locked = lay_stage(&control);
    let before = entries(&control);
    assert!(before.len() > 3 * 8 * 64, "{}", before.len());
    assert!(
        std::fs::remove_dir_all(&control).is_err(),
        "the folder denies its removal"
    );
    assert_eq!(
        entries(&control),
        before,
        "remove_dir_all stopped at the first entry and left every entry"
    );
    assert!(holds_ace(&locked, DENY_DELETE));

    lay_stage(&stage);
    let mut asked = Vec::new();
    let went = reclaim_moved(&old, SystemTime::now(), &mut |p, w| {
        asked.push((p.to_path_buf(), w));
        true
    });
    assert_eq!(went, [(stage.clone(), Unused::Unfinished)]);
    assert_eq!(asked, went, "its intent asked before it went");
    assert!(
        !stage.exists() && !old.exists(),
        "the stage, its mirror folders and the emptied root went"
    );
}

/// An uninspectable folder keeps the stage without changing its metadata or hidden bytes.
/// Once the fixture makes that folder readable, normal reclamation completes the same stage.
#[cfg(unix)]
#[test]
fn a_stage_keeps_an_uninspectable_folder_until_it_is_readable() {
    use std::os::unix::fs::MetadataExt as _;
    let f = Fixture::new("build-cache-mode-stage");
    let root = cache_root(&f.home);
    let stage = build_dir(&root, &f.root.join("home/gone")).with_file_name(DOOMED);
    let control = f.root.join("control").join(DOOMED);
    let lay_stage = |stage: &Path| {
        let deps = stage.join("debug/deps");
        std::fs::create_dir_all(deps.join("sealed")).unwrap();
        std::fs::write(deps.join("x"), b"x").unwrap();
        std::fs::write(deps.join("sealed/y"), b"y").unwrap();
        mode(&deps.join("sealed"), 0o000);
        mode(&deps, 0o500);
    };
    lay_stage(&control);
    assert!(std::fs::remove_dir_all(&control).is_err());
    assert!(
        control.join("debug/deps/x").is_file(),
        "remove_dir_all left it"
    );

    lay_stage(&stage);
    let sealed = stage.join("debug/deps/sealed");
    let before = std::fs::symlink_metadata(&sealed).unwrap();
    let mut asked = Vec::new();
    let went = reclaim_watched(
        &root,
        Some(f.home.as_path()),
        SystemTime::now(),
        ReclaimAfter::CALM,
        None,
        &mut |p, w| {
            asked.push((p.to_path_buf(), w));
            true
        },
    );
    assert!(matches!(went.as_slice(), [(path, Unused::Revoked { .. })] if path == &stage));
    assert_eq!(asked, [(stage.clone(), Unused::Unfinished)]);
    let after = std::fs::symlink_metadata(&sealed).unwrap();
    assert_eq!(
        (after.ino(), after.mode(), after.ctime(), after.ctime_nsec()),
        (
            before.ino(),
            before.mode(),
            before.ctime(),
            before.ctime_nsec()
        )
    );
    mode(&sealed, 0o700);
    assert_eq!(std::fs::read(sealed.join("y")).unwrap(), b"y");
    let resumed = reclaim_watched(
        &root,
        Some(f.home.as_path()),
        SystemTime::now(),
        ReclaimAfter::CALM,
        None,
        &mut |_, _| true,
    );
    assert_eq!(resumed, [(stage.clone(), Unused::Unfinished)]);
    assert!(!stage.exists());
}

/// A STAGE WHOSE OWN FOLDER DENIES ITS SEARCH GOES WHOLE: a stage at 0o600 (read, no search),
/// and on macOS one whose ACL denies `search`, each holding `debug/deps/x`, goes whole in one
/// pass, asked as unfinished: the pass reads custody by path above the stage alone, and its
/// removal reads the stage's own through the folder it holds, once it has let its owner search
/// it. NEGATIVE CONTROL: before the pass, nothing in the stage, its custody marker included, can
/// be read by its path.
#[cfg(unix)]
#[test]
fn a_stage_whose_own_folder_denies_its_search_goes_whole() {
    /// How a stage is made to deny its owner search.
    type Deny = fn(&Path);
    let denials: &[(&str, Deny)] = &[
        ("mode", |stage| mode(stage, 0o600)),
        #[cfg(target_os = "macos")]
        ("acl", |stage| acl(stage, &["group:everyone deny search"])),
    ];
    for &(how, deny) in denials {
        let f = Fixture::new(&format!("build-cache-unsearched-stage-{how}"));
        let root = cache_root(&f.home);
        let stage = build_dir(&root, &f.root.join("home/gone")).with_file_name(DOOMED);
        std::fs::create_dir_all(stage.join("debug/deps")).unwrap();
        std::fs::write(stage.join("debug/deps/x"), b"x").unwrap();
        deny(&stage);
        assert!(std::fs::symlink_metadata(stage.join("debug")).is_err());
        assert!(
            std::fs::symlink_metadata(stage.join(registered::MARKER))
                .is_err_and(|e| e.kind() != std::io::ErrorKind::NotFound),
            "{how}"
        );

        let mut asked = Vec::new();
        let went = reclaim_watched(
            &root,
            Some(f.home.as_path()),
            SystemTime::now(),
            ReclaimAfter::CALM,
            None,
            &mut |p, w| {
                asked.push((p.to_path_buf(), w));
                true
            },
        );
        assert_eq!(went, [(stage.clone(), Unused::Unfinished)], "{how}");
        assert_eq!(asked, went, "{how}");
        assert!(!stage.exists(), "{how}");
    }
}

/// A LINK IN A STAGE GOES AS ITSELF: a stage holding, in a folder at 0o500, links to a folder
/// and a file outside the cache — the folder at 0o500 holding a file and, on macOS, the link
/// and the folder each carrying `deny delete` — goes whole in one pass; the links go, and
/// nothing they point to changes: the folder keeps its mode, its file and its ACL, the file
/// its bytes. NEGATIVE CONTROL (macOS): the link's own ACL refuses `std::fs::remove_file` before
/// the pass, so the pass cleared that link's ACL, and the folder's stayed.
#[cfg(unix)]
#[test]
fn a_link_in_a_stage_goes_and_what_it_points_to_stays_as_it_was() {
    let f = Fixture::new("build-cache-link-stage");
    let root = cache_root(&f.home);
    let stage = build_dir(&root, &f.root.join("home/gone")).with_file_name(DOOMED);
    let outside = f.dir("outside");
    let target = outside.join("folder");
    std::fs::create_dir(&target).unwrap();
    std::fs::write(target.join("keep"), b"keep").unwrap();
    std::fs::write(outside.join("file"), b"bytes").unwrap();
    mode(&target, 0o500);
    let links = stage.join("tmp");
    std::fs::create_dir_all(&links).unwrap();
    std::os::unix::fs::symlink(&target, links.join("folder")).unwrap();
    std::os::unix::fs::symlink(outside.join("file"), links.join("file")).unwrap();
    #[cfg(target_os = "macos")]
    {
        acl(&target, &[DENY_DELETE]);
        acl(&links.join("folder"), &[DENY_DELETE]);
        assert!(
            std::fs::remove_file(links.join("folder")).is_err(),
            "the link's own ACL refuses its removal"
        );
    }
    mode(&links, 0o500);

    let went = reclaim_watched(
        &root,
        Some(f.home.as_path()),
        SystemTime::now(),
        ReclaimAfter::CALM,
        None,
        &mut |_, _| true,
    );
    assert_eq!(went, [(stage.clone(), Unused::Unfinished)]);
    assert!(!stage.exists());
    assert_eq!(mode_of(&target), 0o500, "the link's target was chmodded");
    assert_eq!(std::fs::read(target.join("keep")).unwrap(), b"keep");
    assert_eq!(std::fs::read(outside.join("file")).unwrap(), b"bytes");
    #[cfg(target_os = "macos")]
    assert!(
        holds_ace(&target, DENY_DELETE),
        "the link's target lost its ACL"
    );
}

/// EVERY REMOVAL A PASS MAKES IS THE ONE REMOVER: a gone workspace's build directory taken
/// whole, a stale incremental folder pruned from a live one, a stage an earlier pass left, a
/// tooling entry, and a target nothing builds in, each holding a folder at 0o500 and in it one
/// that (on macOS) denies its removal by ACL, all go whole. NEGATIVE CONTROL: such a tree
/// refuses `std::fs::remove_dir_all`, each of these removals before.
#[cfg(unix)]
#[test]
fn every_removal_a_pass_makes_takes_what_remove_dir_all_cannot() {
    let f = Fixture::new("build-cache-one-remover");
    let root = cache_root(&f.home);
    let home = Some(f.home.as_path());
    let seal = |dir: &Path| {
        let sealed = dir.join("sealed");
        std::fs::create_dir_all(sealed.join("held")).unwrap();
        std::fs::write(sealed.join("held/y"), b"y").unwrap();
        #[cfg(target_os = "macos")]
        acl(&sealed.join("held"), &[DENY_DELETE]);
        mode(&sealed, 0o500);
    };
    let control = f.dir("control");
    seal(&control);
    assert!(std::fs::remove_dir_all(&control).is_err());
    assert!(
        control.join("sealed/held").is_dir(),
        "remove_dir_all left it"
    );

    let gone_leaf = lay(&root, &f.root.join("home/gone"), 2 * DAY);
    seal(&gone_leaf.join("debug"));
    age_tree(&gone_leaf, 2 * DAY);

    let live = f.dir("home/live");
    std::fs::write(live.join("Cargo.toml"), "[workspace]\n").unwrap();
    let live_leaf = build_dir(&root, &live);
    let profile = live_leaf.join("debug");
    std::fs::create_dir_all(&profile).unwrap();
    std::fs::write(profile.join(".cargo-build-lock"), b"").unwrap();
    let stale_crate = profile.join("incremental/ws-1k116trr0a0xf");
    std::fs::create_dir_all(stale_crate.join("s-hneouhc0uf-1tn8o9j-2awpw")).unwrap();
    seal(&stale_crate);
    age_tree(&stale_crate, 4 * DAY);

    let mirror = |ws: &Path| root.join(ws.strip_prefix("/").unwrap());
    let left = mirror(&f.root.join("home/left")).join(DOOMED);
    std::fs::create_dir_all(&left).unwrap();
    seal(&left);

    let mut went = reclaim_watched(
        &root,
        home,
        SystemTime::now(),
        ReclaimAfter::CALM,
        None,
        &mut |_, _| true,
    );
    went.sort_by(|a, b| a.0.cmp(&b.0));
    let pruned = live_leaf.with_file_name(DOOMED);
    let mut want = vec![gone_leaf.clone(), pruned.clone(), left.clone()];
    want.sort();
    assert_eq!(
        went.iter().map(|(p, _)| p.clone()).collect::<Vec<_>>(),
        want,
        "{went:?}"
    );
    for (path, why) in &went {
        let right = if *path == gone_leaf {
            matches!(why, Unused::Gone { .. })
        } else if *path == pruned {
            matches!(why, Unused::Stale { incremental: 1, .. })
        } else {
            *why == Unused::Unfinished
        };
        assert!(right, "{}: {why:?}", path.display());
    }
    assert!(!gone_leaf.exists() && !stale_crate.exists() && !pruned.exists() && !left.exists());
    assert!(
        profile.join(".cargo-build-lock").is_file(),
        "the live leaf stays"
    );

    let tooling = tooling::root(&f.home);
    let entry = tooling.join(".provision-reclaim-tool-1");
    std::fs::create_dir_all(&entry).unwrap();
    seal(&entry);
    assert_eq!(
        tooling::reclaim(&tooling, SystemTime::now(), &mut |_, _| true),
        [(entry.clone(), Unused::Unfinished)]
    );
    assert!(!entry.exists());

    let target = f.dir("target/verify/cells.noindex");
    seal(&target);
    assert!(remove_unbuilt(&target).unwrap());
    assert!(!target.exists());
}

/// AN UNRECORDED STAGE KEEPS ITS BYTES AND ONLY THEM: a stage whose intent is refused stays as
/// it was, its bytes untouched, while the gone workspace's build directory beside it, its own
/// intent on record, is staged under a name of its own (`.cargo-build.removing-1`, never over
/// the old stage) and goes. Each intent is asked once.
#[cfg(unix)]
#[test]
fn pressure_reclamation_keeps_an_unrecorded_stage_and_takes_the_leaf_beside_it() {
    let f = Fixture::new("build-cache-beside-intent");
    let root = cache_root(&f.home);
    let home = Some(f.home.as_path());
    let leaf = lay(
        &root,
        &f.root.join("home/removed"),
        Duration::from_secs(15 * 60),
    );
    let stage = leaf.with_file_name(DOOMED);
    std::fs::create_dir_all(&stage).unwrap();
    let payload = stage.join("retained");
    std::fs::write(&payload, b"the previous generation").unwrap();
    let mut intents = Vec::new();
    let removed = pressure(&root, home, &mut |p| {
        intents.push(p.to_path_buf());
        p != stage
    });
    assert_eq!(
        std::fs::read(&payload).ok(),
        Some(b"the previous generation".to_vec()),
        "denying the old stage must not delete its bytes"
    );
    assert_eq!(
        removed,
        std::slice::from_ref(&leaf),
        "the leaf, its intent on record"
    );
    assert!(!leaf.exists() && !leaf.with_file_name(format!("{DOOMED}-1")).exists());
    intents.sort();
    assert_eq!(intents, [leaf, stage]);
}

#[cfg(unix)]
fn age_tree(dir: &Path, ago: Duration) {
    for entry in std::fs::read_dir(dir).unwrap().flatten() {
        let t = entry.file_type().unwrap();
        if t.is_dir() {
            age_tree(&entry.path(), ago);
        } else if t.is_file() {
            age_by(&entry.path(), ago);
        }
    }
    age_by(dir, ago);
}

/// A holder appearing during intent recording keeps the original directory, and the intent on
/// record closes with nothing gone ([`Refusal::Stopped`]). A later probe can leave only an empty
/// registered stage: the next pass recovers that intent, then retires the still-identical cache
/// normally. A refused stale intent retains a newly held stage.
#[cfg(unix)]
#[test]
fn a_process_arriving_at_intent_keeps_the_cache_before_staging() {
    let f = Fixture::new("build-cache-late-process");
    let root = cache_root(&f.home);
    let owner = f.dir("home/owner");
    let leaf = lay(&root, &owner, RECLAIM_IDLE + DAY);
    let busy = std::cell::Cell::new(false);
    let processes = |dirs: &[PathBuf], _: &[u32]| {
        let users = busy
            .get()
            .then(|| {
                assert!(dirs.iter().any(|dir| leaf.starts_with(dir)));
                aterm_uds::holders::PathUse {
                    pid: 31,
                    how: "exe",
                    path: leaf.join("debug/deps/x"),
                }
            })
            .into_iter()
            .collect();
        Ok(aterm_uds::holders::PathScan {
            users,
            ..aterm_uds::holders::PathScan::default()
        })
    };
    let removed = reclaim_watched_on(
        &root,
        Some(&f.home),
        SystemTime::now(),
        ReclaimAfter::CALM,
        None,
        &mut |path, _| {
            assert_eq!(path, leaf);
            busy.set(true);
            true
        },
        &fence::Fence::new(&processes),
    );
    assert!(
        busy.get(),
        "the late holder was introduced by the actual intent callback"
    );
    assert_eq!(
        removed,
        [(
            leaf.clone(),
            Unused::Refused {
                by: Refusal::Stopped
            }
        )],
        "its intent closed, nothing of it gone"
    );
    assert!(leaf.join("debug/deps/x").is_file());
    assert!(!leaf.parent().unwrap().join(DOOMED).exists());

    let owner = f.dir("home/registered-owner");
    std::fs::write(owner.join("Cargo.toml"), "[workspace]\n").unwrap();
    let target = f.home.join("explicit.noindex");
    assert!(registered::create_checked(&f.home, &target, &owner).unwrap());
    std::fs::create_dir_all(target.join("debug/deps")).unwrap();
    std::fs::write(target.join("debug/deps/keep"), b"registered output").unwrap();
    std::fs::write(target.join("debug/.cargo-lock"), b"").unwrap();
    age_tree(&target, 2 * DAY);
    std::fs::remove_dir_all(&owner).unwrap();
    let pending_observed = std::cell::Cell::new(false);
    let late = |_: &[PathBuf], _: &[u32]| {
        let pending = registered::roots(&f.home)
            .iter()
            .any(|root| root.root == target && !root.is_current(&f.home));
        pending_observed.set(pending_observed.get() || pending);
        Ok(aterm_uds::holders::PathScan {
            users: pending
                .then(|| aterm_uds::holders::PathUse {
                    pid: 32,
                    how: "exe",
                    path: target.join("debug/deps/keep"),
                })
                .into_iter()
                .collect(),
            ..aterm_uds::holders::PathScan::default()
        })
    };
    let pass = |processes: fence::Processes<'_>| {
        registered::reclaim_on(
            &f.home,
            SystemTime::now(),
            ReclaimAfter::CALM,
            None,
            &mut |_, _| true,
            &fence::Fence::new(processes),
        )
    };
    assert_eq!(
        pass(&late),
        [(
            target.clone(),
            Unused::Refused {
                by: Refusal::Stopped
            }
        )],
        "its intent closed, nothing of it gone"
    );
    assert!(
        pending_observed.get(),
        "the holder arrived after the receipt had durable Pending state"
    );
    let stages = || {
        std::fs::read_dir(&f.home)
            .unwrap()
            .flatten()
            .map(|entry| entry.path())
            .filter(|path| {
                path.file_name()
                    .unwrap()
                    .to_string_lossy()
                    .starts_with("atpkg-cache-retiring-")
            })
            .collect::<Vec<_>>()
    };
    let stage = stages()
        .into_iter()
        .next()
        .expect("the private empty stage");
    assert_eq!(std::fs::read_dir(&stage).unwrap().count(), 0);
    let clear = |_: &[PathBuf], _: &[u32]| Ok(aterm_uds::holders::PathScan::default());
    assert!(
        pass(&clear).is_empty(),
        "recovery does not remove the original"
    );
    assert!(stage.symlink_metadata().is_err());
    assert!(target.join("debug/deps/keep").is_file());
    assert!(
        registered::roots(&f.home)
            .iter()
            .any(|root| { root.root == target && root.is_current(&f.home) })
    );
    assert!(matches!(pass(&clear).as_slice(), [(path, Unused::Gone { .. })] if path == &target));
    assert!(!target.exists() && stages().is_empty());

    let owner = f.dir("home/stale-owner");
    std::fs::write(owner.join("Cargo.toml"), "[workspace]\n").unwrap();
    let leaf = build_dir(&root, &owner);
    let profile = leaf.join("debug");
    std::fs::create_dir_all(&profile).unwrap();
    std::fs::write(profile.join(".cargo-build-lock"), b"").unwrap();
    let incremental = profile.join("incremental/ws-1k116trr0a0xf");
    std::fs::create_dir_all(incremental.join("s-hneouhc0uf-1tn8o9j-2awpw")).unwrap();
    age_tree(&incremental, 4 * DAY);
    let staged = std::cell::RefCell::new(None::<PathBuf>);
    let newly_held = |dirs: &[PathBuf], _: &[u32]| {
        let users = staged
            .borrow()
            .as_ref()
            .map(|stage| {
                assert!(
                    dirs.contains(stage),
                    "the rollback probes the actual staged spelling"
                );
                aterm_uds::holders::PathUse {
                    pid: 33,
                    how: "cwd",
                    path: stage.join("0"),
                }
            })
            .into_iter()
            .collect();
        Ok(aterm_uds::holders::PathScan {
            users,
            ..Default::default()
        })
    };
    let mut intent = |stage: &Path, why| {
        assert!(matches!(why, Unused::Stale { incremental: 1, .. }));
        *staged.borrow_mut() = Some(stage.to_path_buf());
        false
    };
    let mut intents = Intents::new(&mut intent);
    let mut sweep = Sweep {
        root: &root,
        home: Some(&f.home),
        now: SystemTime::now(),
        after: ReclaimAfter::CALM.into(),
        placing: None,
        moved: false,
        compiler_store: None,
        intents: &mut intents,
        fence: &fence::Fence::new(&newly_held),
        permitted: &|| true,
        pruning: Vec::new(),
        declined: false,
    };
    assert!(!sweep.prune(leaf.parent().unwrap(), &leaf));
    assert!(intents.finish().is_empty());
    let stage = staged
        .borrow_mut()
        .take()
        .expect("the refused stale intent");
    assert!(
        stage.join("0").is_dir() && !incremental.exists(),
        "the held stage was not rolled back"
    );
    let went = reclaim_watched_on(
        &root,
        Some(&f.home),
        SystemTime::now(),
        ReclaimAfter::CALM,
        None,
        &mut |_, _| true,
        &fence::Fence::new(&clear),
    );
    assert!(
        went.iter()
            .any(|(path, why)| path == &stage && *why == Unused::Unfinished)
    );
    assert!(!stage.exists() && profile.join(".cargo-build-lock").is_file());
}

/// THE ROOT THE CACHE MOVED FROM EMPTIES ITSELF: under it a build directory an hour quiet goes
/// whole whatever its workspace's state (a live one's too), one a build holds or wrote within the
/// hour stays, and the root goes with the last of them; `plan_moved` names what a pass would
/// take, a held one marked held. NEGATIVE CONTROL: the same live workspace's two-hour-quiet build
/// directory under the cache root itself stays (only a gone or a month-old one goes there).
#[cfg(unix)]
#[test]
fn the_root_the_cache_moved_from_goes_once_nothing_builds_there() {
    let f = Fixture::new("build-cache-moved");
    let old = moved_from(&f.home);
    let hour = Duration::from_secs(60 * 60);
    let live = f.dir("home/live");
    std::fs::write(live.join("Cargo.toml"), "[workspace]\n").unwrap();
    let quiet = lay(&old, &live, 2 * hour);
    let fresh = lay(&old, &f.root.join("home/fresh"), Duration::from_secs(60));
    let busy = lay(&old, &f.root.join("home/busy"), 2 * hour);
    let lock = busy.join("debug/.cargo-lock");
    std::fs::write(&lock, b"").unwrap();
    age_tree(&busy, 2 * hour);
    let held = crate::lock::Flock::try_lock(std::fs::File::open(&lock).unwrap()).unwrap();
    let now = SystemTime::now();

    let planned: Vec<(PathBuf, bool)> = plan_moved(&old, now)
        .into_iter()
        .map(|p| {
            assert!(matches!(p.why, Unused::Moved { after, .. } if after == MOVED_AFTER));
            (p.path, p.held)
        })
        .collect();
    let mut want = vec![(quiet.clone(), false), (busy.clone(), true)];
    want.sort();
    assert_eq!(planned, want);

    let went = reclaim_moved(&old, now, &mut |_, _| true);
    assert_eq!(went.len(), 1, "{went:?}");
    assert_eq!(went[0].0, quiet);
    assert!(
        matches!(went[0].1, Unused::Moved { idle, after } if idle >= 2 * hour && after == MOVED_AFTER)
    );
    assert!(!quiet.exists() && fresh.is_dir() && busy.is_dir() && old.is_dir());

    drop(held);
    let mut went: Vec<PathBuf> = reclaim_moved(&old, now + 2 * hour, &mut |_, _| true)
        .into_iter()
        .map(|(p, _)| p)
        .collect();
    went.sort();
    let mut want = vec![busy, fresh];
    want.sort();
    assert_eq!(went, want);
    assert!(!old.exists(), "the emptied root goes too");

    let root = cache_root(&f.home);
    let kept = lay(&root, &live, 2 * hour);
    assert!(reclaim(&root, Some(f.home.as_path()), now).is_empty());
    assert!(kept.is_dir());
}

/// A TARGET DIRECTORY NOTHING BUILDS IN GOES WHOLE: verify's old side lane goes once no build
/// holds a lock in it, a cells lane's deep in its cell and triple included, and nothing is left
/// beside it. NEGATIVE CONTROL: while a build holds that deep lock it stays, whole.
#[cfg(unix)]
#[test]
fn a_target_nothing_builds_in_goes_whole_and_a_held_one_stays() {
    let f = Fixture::new("build-cache-unbuilt");
    let old = f.dir("target/verify/cells.noindex");
    let deep = old.join("verify/.ubd/side-lanes/cells/win/x86_64-pc-windows-msvc/debug");
    std::fs::create_dir_all(deep.join("deps")).unwrap();
    std::fs::write(deep.join(".cargo-lock"), b"").unwrap();
    std::fs::write(deep.join("deps/x.rmeta"), b"x").unwrap();
    let held = crate::lock::Flock::try_lock(std::fs::File::open(deep.join(".cargo-lock")).unwrap())
        .unwrap();
    assert!(!remove_unbuilt(&old).unwrap(), "a build holds it");
    assert!(deep.join("deps/x.rmeta").is_file(), "kept whole");
    drop(held);
    let obstruction = deep.join(".cargo-artifact-lock");
    std::fs::create_dir(&obstruction).unwrap();
    assert!(!remove_unbuilt(&old).unwrap());
    assert_eq!(std::fs::read(deep.join("deps/x.rmeta")).unwrap(), b"x");
    assert!(obstruction.is_dir());
    std::fs::remove_dir(&obstruction).unwrap();
    assert!(remove_unbuilt(&old).unwrap());
    assert!(!old.exists());
    let listed: Vec<_> = std::fs::read_dir(old.parent().unwrap())
        .unwrap()
        .flatten()
        .map(|e| e.file_name())
        .collect();
    assert!(listed.is_empty(), "nothing beside it: {listed:?}");
    assert!(!remove_unbuilt(&old).unwrap(), "gone already");
}

/// A RETAINED TARGET NOTHING BUILDS IN STAYS WHERE IT STANDS: verify's old side lane retained by
/// the keep-only API ([`registered::retain`]) is never set aside: each call keeps it at its own
/// path, `Ok(false)`, with its marker and what it holds, and nothing stands beside it. With the
/// folder holding the lanes retained instead, an aside an earlier call left there stays,
/// untouched and said as one root whose removal's permission the custody marker of the folder
/// above it revoked, that folder named (never an entry of it as refusing its removal), and the
/// lane beside it stays where it stands. NEGATIVE CONTROL: each keep withdrawn, the same lane
/// and aside go whole.
#[cfg(unix)]
#[test]
fn a_retained_target_nothing_builds_in_stays_where_it_stands() {
    let clear = |_: &[PathBuf], _: &[u32]| Ok(aterm_uds::holders::PathScan::default());
    let lay = |lane: &Path| {
        std::fs::create_dir_all(lane.join("debug/deps")).unwrap();
        std::fs::write(lane.join("debug/deps/x"), b"retained").unwrap();
        std::fs::write(lane.join("debug/.cargo-lock"), b"").unwrap();
        mode(lane, 0o700);
    };
    let listed = |at: &Path| {
        let mut names: Vec<String> = std::fs::read_dir(at)
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        names
    };

    let f = Fixture::new("build-cache-unbuilt-kept");
    let lane = f.home.join("target/verify/tippy.noindex");
    lay(&lane);
    assert_eq!(registered::retain(&f.home, &lane).unwrap(), lane);
    let marker = std::fs::read(lane.join(registered::MARKER)).unwrap();
    for _ in 0..2 {
        assert!(!remove_unbuilt_with_processes(&lane, &clear).unwrap());
        assert_eq!(listed(lane.parent().unwrap()), ["tippy.noindex"]);
        assert_eq!(
            std::fs::read(lane.join("debug/deps/x")).unwrap(),
            b"retained"
        );
        assert_eq!(
            std::fs::read(lane.join(registered::MARKER)).unwrap(),
            marker
        );
    }
    std::fs::remove_file(lane.join(registered::MARKER)).unwrap();
    assert!(remove_unbuilt_with_processes(&lane, &clear).unwrap());
    assert!(listed(lane.parent().unwrap()).is_empty());

    let f = Fixture::new("build-cache-unbuilt-kept-above");
    let verify = f.home.join("target/verify");
    let lane = verify.join("checkers.noindex");
    let aside = verify.join("checkers.noindex.removing");
    lay(&lane);
    lay(&aside);
    mode(&verify, 0o700);
    assert_eq!(registered::retain(&f.home, &verify).unwrap(), verify);
    let said = remove_unbuilt_with_processes(&lane, &clear)
        .unwrap_err()
        .to_string();
    assert!(said.contains(&*aside.to_string_lossy()), "{said}");
    assert!(
        said.contains(&format!(
            "{} stayed: removal permission revoked; remaining contents not inspected; the \
             custody marker of {}, a folder above it, keeps it",
            aside.display(),
            verify.display()
        )),
        "{said}"
    );
    assert!(!said.contains("refuses"), "{said}");
    for at in [&lane, &aside] {
        assert_eq!(std::fs::read(at.join("debug/deps/x")).unwrap(), b"retained");
    }
    assert_eq!(
        listed(&verify),
        [
            registered::MARKER,
            "checkers.noindex",
            "checkers.noindex.removing"
        ]
    );
    std::fs::remove_file(verify.join(registered::MARKER)).unwrap();
    assert!(remove_unbuilt_with_processes(&lane, &clear).unwrap());
    assert!(listed(&verify).is_empty());
}

/// Clears this thread's staging hook when dropped, so a failed test leaves none set.
#[cfg(unix)]
struct Staged;

#[cfg(unix)]
impl Drop for Staged {
    fn drop(&mut self) {
        staged::clear();
    }
}

/// A custody marker that cannot be read, published in `at` the first time `then` is called.
#[cfg(unix)]
fn late_keep(at: PathBuf) -> impl FnMut(&Path) + 'static {
    move |_| {
        let marker = at.join(registered::MARKER);
        if !marker.exists() {
            std::fs::write(&marker, b"keep published now").unwrap();
        }
    }
}

/// A KEEP PUBLISHED AS A TARGET IS SET ASIDE STAYS WHERE IT STANDS: verify's old side lane that
/// nothing builds in passes its custody read by path and is renamed aside, and at that instant
/// (the `staged` hook, before custody is read again) a custody marker that cannot be read is
/// published in it. Custody read again at the aside finds it: the lane is put back at its own
/// path, never over another entry, with what it holds and the marker, the call returns
/// `Ok(false)`, and nothing stands aside; a later call keeps it where it stands too. An aside
/// a keep inside stops is said by the folder holding the keep, its removal's permission revoked,
/// never the marker as an entry that refuses its removal. NEGATIVE CONTROL: the hook fired with
/// the aside (the rename happened), and once the marker is withdrawn the same lane goes whole.
#[cfg(unix)]
#[test]
fn a_keep_published_as_a_target_is_set_aside_stays_where_it_stands() {
    use std::cell::RefCell;
    use std::rc::Rc;
    let clear = |_: &[PathBuf], _: &[u32]| Ok(aterm_uds::holders::PathScan::default());
    let f = Fixture::new("build-cache-unbuilt-late-keep");
    let verify = f.home.join("target/verify");
    let lane = verify.join("tippy.noindex");
    std::fs::create_dir_all(lane.join("debug/deps")).unwrap();
    std::fs::write(lane.join("debug/deps/x"), b"retained").unwrap();
    std::fs::write(lane.join("debug/.cargo-lock"), b"").unwrap();
    mode(&lane, 0o700);
    let aside = verify.join("tippy.noindex.removing");
    let marker = lane.join(registered::MARKER);
    let listed = || {
        let mut names: Vec<String> = std::fs::read_dir(&verify)
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        names
    };

    let _staged = Staged;
    let seen = Rc::new(RefCell::new(Vec::new()));
    let (saw, mut publish) = (Rc::clone(&seen), late_keep(aside.clone()));
    staged::on(move |stage| {
        saw.borrow_mut().push(stage.to_path_buf());
        publish(stage);
    });
    let first = remove_unbuilt_with_processes(&lane, &clear);
    staged::clear();
    assert_eq!(
        *seen.borrow(),
        std::slice::from_ref(&aside),
        "never set aside"
    );
    for call in 0..2 {
        assert!(
            !first.as_ref().unwrap(),
            "call {call}: the keep went: {first:?}"
        );
        assert_eq!(listed(), ["tippy.noindex"], "call {call}");
        assert_eq!(
            std::fs::read(lane.join("debug/deps/x")).unwrap(),
            b"retained"
        );
        assert_eq!(std::fs::read(&marker).unwrap(), b"keep published now");
        assert!(!remove_unbuilt_with_processes(&lane, &clear).unwrap());
    }

    // An aside an earlier call left that a keep inside stops is named by the keep's folder.
    std::fs::rename(&lane, &aside).unwrap();
    std::fs::rename(
        aside.join(registered::MARKER),
        aside.join("debug").join(registered::MARKER),
    )
    .unwrap();
    let said = remove_unbuilt_with_processes(&lane, &clear)
        .unwrap_err()
        .to_string();
    assert!(
        said.contains(&format!(
            "1 entry of {} stayed: debug (removal permission revoked",
            aside.display()
        )),
        "{said}"
    );
    assert!(!said.contains("refuses"), "{said}");
    assert_eq!(
        std::fs::read(aside.join("debug/deps/x")).unwrap(),
        b"retained"
    );

    std::fs::remove_file(aside.join("debug").join(registered::MARKER)).unwrap();
    assert!(!remove_unbuilt_with_processes(&lane, &clear).unwrap());
    assert!(listed().is_empty());
}

/// A KEEP PUBLISHED AS A BUILD DIRECTORY IS STAGED STAYS WHERE IT STANDS: a gone workspace's
/// build directory is renamed aside by a pass, and at that instant (the `staged` hook) a custody
/// marker that cannot be read is published in it. Custody read again at the stage finds it: the
/// build directory is put back at its own path with what it holds, nothing is staged beside it,
/// and nothing is returned as gone: the intent its pass put on record closes with nothing of it
/// gone, at its own path, where it stands again ([`Refusal::Stopped`]), never as the stage it
/// left. NEGATIVE CONTROLS: the hook fired, and once the marker is withdrawn the next pass takes
/// the same build directory whole; by hand, the stage counted as moved before custody is read
/// there again ([`Intents::moved`] ahead of [`stage_unkept`]'s answer) closes the intent as the
/// empty stage, [`Refusal::Staged`].
#[cfg(unix)]
#[test]
fn a_keep_published_as_a_build_directory_is_staged_stays_where_it_stands() {
    let f = Fixture::new("build-cache-leaf-late-keep");
    let root = cache_root(&f.home);
    let home = Some(f.home.as_path());
    let leaf = lay(
        &root,
        &f.root.join("home/removed"),
        Duration::from_secs(15 * 60),
    );
    let stage = leaf.with_file_name(DOOMED);
    let _staged = Staged;
    let fired = std::rc::Rc::new(std::cell::Cell::new(false));
    let (once, mut publish) = (std::rc::Rc::clone(&fired), late_keep(stage.clone()));
    staged::on(move |at| {
        once.set(true);
        publish(at);
    });
    let mut asked = Vec::new();
    let went = reclaim_watched(
        &root,
        home,
        SystemTime::now(),
        ReclaimAfter::UNDER_PRESSURE,
        None,
        &mut |p, _| {
            asked.push(p.to_path_buf());
            true
        },
    );
    staged::clear();
    assert!(fired.get(), "never staged");
    assert_eq!(asked, std::slice::from_ref(&leaf), "its intent");
    assert_eq!(
        went,
        [(
            leaf.clone(),
            Unused::Refused {
                by: Refusal::Stopped
            }
        )],
        "its outcome"
    );
    assert!(!stage.exists(), "staged beside it");
    assert_eq!(
        std::fs::read(leaf.join("debug/deps/x")).unwrap(),
        b"0123456789"
    );
    let marker = leaf.join(registered::MARKER);
    assert_eq!(std::fs::read(&marker).unwrap(), b"keep published now");
    assert!(pressure(&root, home, &mut |_| true).is_empty());
    assert!(leaf.join("debug/deps/x").is_file());

    std::fs::remove_file(&marker).unwrap();
    // Its marker's coming and going wrote the folder: unwritten again, as it was laid.
    age_tree(&leaf, Duration::from_secs(15 * 60));
    assert_eq!(
        pressure(&root, home, &mut |_| true),
        std::slice::from_ref(&leaf)
    );
    assert!(!leaf.exists() && !stage.exists());
}

/// BELOW THE FLOOR A LIVE WORKTREE GIVES UP WHAT NO BUILD WROTE FOR AN HOUR: merging a moving
/// main leaves crate folders no build reads again, well inside the three days that keep a
/// worktree's state over a weekend. A pass with room keeps the folder unwritten for two hours. A
/// pass below the floor takes it and says the hour it waited, and keeps the folder written ten
/// minutes ago. NEGATIVE CONTROL: a build holding the lock keeps it from that pass too.
#[cfg(unix)]
#[test]
fn below_the_floor_a_live_worktree_gives_up_what_no_build_wrote_for_an_hour() {
    let f = Fixture::new("build-cache-prune-under-pressure");
    let root = cache_root(&f.home);
    let live = f.dir("home/live");
    std::fs::write(live.join("Cargo.toml"), "[workspace]\n").unwrap();
    let leaf = build_dir(&root, &live);
    let profile = leaf.join("debug");
    std::fs::create_dir_all(&profile).unwrap();
    let lock = profile.join(".cargo-build-lock");
    std::fs::write(&lock, b"").unwrap();
    let dead = profile.join("incremental/ws-1k116trr0a0xf");
    std::fs::create_dir_all(dead.join("s-hneouhc0uf-1tn8o9j-2awpw")).unwrap();
    age_tree(&dead, Duration::from_secs(2 * 60 * 60));
    let current = profile.join("incremental/ws-3g63pv341xugh");
    std::fs::create_dir_all(current.join("s-hneouhc0ug-1tn8o9j-2awpw")).unwrap();
    age_tree(&current, Duration::from_secs(10 * 60));
    let pass = |below_floor| {
        reclaim_watched(
            &root,
            Some(f.home.as_path()),
            SystemTime::now(),
            ReclaimAfter::by_floor(below_floor),
            None,
            &mut |_, _| true,
        )
    };
    let pruned = |went: &[(PathBuf, Unused)]| {
        went.iter()
            .any(|(_, why)| matches!(why, Unused::Stale { .. }))
    };

    let went = pass(false);
    assert!(!pruned(&went), "{went:?}");
    assert!(dead.is_dir());

    let held = crate::lock::Flock::try_lock(std::fs::File::open(&lock).unwrap()).unwrap();
    let went = pass(true);
    assert!(!pruned(&went), "{went:?}");
    assert!(dead.is_dir());
    drop(held);

    let went = pass(true);
    assert!(
        went.iter().any(|(_, why)| matches!(
            why,
            Unused::Stale { incremental: 1, units: 0, superseded: 0, after }
                if *after == RECLAIM_STALE_UNDER_PRESSURE
        )),
        "{went:?}"
    );
    assert!(!dead.exists());
    assert!(current.is_dir());
}

#[cfg(unix)]
#[test]
fn explicit_cleanup_observes_each_leaf_and_registered_root_without_spilling_pressure() {
    let f = Fixture::new("explicit-cache-pressure");
    let root = cache_root(&f.home);
    let mut leaves = Vec::new();
    for name in ["pressured", "roomy", "unknown", "exact-floor"] {
        let workspace = f.dir(&format!("home/{name}"));
        std::fs::write(workspace.join("Cargo.toml"), "[workspace]\n").unwrap();
        leaves.push(build_dir(&root, &workspace));
    }
    let owner = f.dir("home/registered-owner");
    std::fs::write(owner.join("Cargo.toml"), "[workspace]\n").unwrap();
    let target = f.home.join("registered.noindex");
    assert!(registered::create_checked(&f.home, &target, &owner).unwrap());
    leaves.push(target);
    for leaf in &leaves {
        let profile = leaf.join("debug");
        let old = profile.join("incremental/old");
        let young = profile.join("incremental/young");
        std::fs::create_dir_all(&old).unwrap();
        std::fs::create_dir_all(&young).unwrap();
        std::fs::create_dir_all(profile.join("deps")).unwrap();
        std::fs::write(profile.join(".cargo-build-lock"), b"").unwrap();
        std::fs::write(profile.join("deps/preserve.rlib"), b"existing dependency").unwrap();
        age_tree(&old, Duration::from_secs(2 * 60 * 60));
        age_tree(&young, Duration::from_secs(10 * 60));
    }
    let observed = std::cell::RefCell::new(Vec::new());
    let observe = |path: &Path| {
        observed.borrow_mut().push(path.to_path_buf());
        match leaves.iter().position(|leaf| leaf == path) {
            Some(0 | 4) => Some((9, 10)),
            Some(1) => Some((11, 10)),
            Some(3) => Some((10, 10)),
            _ => None,
        }
    };
    let planned = plan_home_on(&f.home, SystemTime::now(), &observe);
    let selected: std::collections::BTreeSet<_> = planned
        .iter()
        .filter(|plan| matches!(plan.why, Unused::Stale { after, .. } if after == RECLAIM_STALE_UNDER_PRESSURE))
        .map(|plan| plan.path.clone()).collect();
    assert_eq!(
        selected,
        [leaves[0].clone(), leaves[3].clone(), leaves[4].clone()]
            .into_iter()
            .collect()
    );
    observed.borrow_mut().clear();
    let fence = fence::Fence::default().with_reporting();
    let removed = reclaim_home_report_on(&f.home, SystemTime::now(), &observe, &fence);
    for (index, leaf) in leaves.iter().enumerate() {
        assert!(
            observed.borrow().contains(leaf),
            "missing actual-root observation: {leaf:?}"
        );
        assert_eq!(
            observed
                .borrow()
                .iter()
                .filter(|path| *path == leaf)
                .count(),
            1,
            "one observation per prune: {leaf:?}"
        );
        assert_eq!(
            leaf.join("debug/incremental/old").is_dir(),
            matches!(index, 1 | 2),
            "root{index}, removed={removed:?}"
        );
        assert!(leaf.join("debug/incremental/young").is_dir());
        assert_eq!(
            std::fs::read(leaf.join("debug/deps/preserve.rlib")).unwrap(),
            b"existing dependency"
        );
    }
    for index in [1, 2] {
        assert!(fence.retained().iter().any(|kept| kept.path == leaves[index] && matches!(&kept.reason, KeepReason::NoStaleParts { after, observed } if *after == RECLAIM_STALE && observed.incremental.not_quiet == 2 && observed.incremental.quiet == 0)));
    }
    assert_eq!(removed.iter().filter(|(_, why)| matches!(why, Unused::Stale { after, .. } if *after == RECLAIM_STALE_UNDER_PRESSURE)).count(), 3);
}

#[cfg(unix)]
#[test]
fn explicit_cleanup_pressure_preserves_custody_locks_and_uncertain_process_use() {
    use aterm_uds::holders::{PathScan, PathUse};
    let f = Fixture::new("explicit-cache-pressure-guards");
    let root = cache_root(&f.home);
    let mut leaves = Vec::new();
    for name in ["retained", "locked", "busy", "unknown-process"] {
        let workspace = f.dir(&format!("home/{name}"));
        std::fs::write(workspace.join("Cargo.toml"), "[workspace]\n").unwrap();
        let leaf = build_dir(&root, &workspace);
        std::fs::create_dir_all(leaf.join("debug/incremental/old")).unwrap();
        std::fs::write(leaf.join("debug/.cargo-build-lock"), b"").unwrap();
        age_tree(
            &leaf.join("debug/incremental/old"),
            Duration::from_secs(2 * 60 * 60),
        );
        leaves.push(leaf);
    }
    std::fs::write(
        leaves[0].join(registered::MARKER),
        b"unreadable or unknown custody keeps output",
    )
    .unwrap();
    let _held = crate::lock::Flock::try_lock(
        std::fs::File::open(leaves[1].join("debug/.cargo-build-lock")).unwrap(),
    )
    .unwrap();
    let processes = |paths: &[PathBuf], _: &[u32]| {
        if paths.iter().any(|path| path == &leaves[3]) {
            return Err(std::io::Error::other("fixture scan unavailable"));
        }
        let users = paths
            .iter()
            .any(|path| path == &leaves[2])
            .then(|| PathUse {
                pid: 31,
                how: "exe",
                path: leaves[2].join("debug/deps/program"),
            })
            .into_iter()
            .collect();
        Ok(PathScan {
            users,
            ..PathScan::default()
        })
    };
    let fence = fence::Fence::new(&processes).with_reporting();
    let removed = reclaim_home_report_on(&f.home, SystemTime::now(), &|_| Some((0, 10)), &fence);
    assert!(removed.is_empty(), "{removed:?}");
    assert!(
        leaves
            .iter()
            .all(|leaf| leaf.join("debug/incremental/old").is_dir())
    );
    for (index, reason) in [
        (0, KeepReason::Custody),
        (1, KeepReason::CargoLocks),
        (3, KeepReason::ProcessUnavailable),
    ] {
        assert!(
            fence
                .retained()
                .iter()
                .any(|kept| kept.path == leaves[index] && kept.reason == reason),
            "missing guard{index}: {:?}",
            fence.retained()
        );
    }
    assert!(fence.retained().iter().any(|kept| kept.path == leaves[2]
        && matches!(kept.reason, KeepReason::ProcessUsed { pid: 31, .. })));
}

#[cfg(unix)]
#[test]
fn explicit_cleanup_pressure_does_not_shorten_whole_cache_age_for_a_standing_folder() {
    let f = Fixture::new("explicit-cache-pressure-whole");
    let workspace = f.dir("home/standing-without-manifest");
    let leaf = lay(
        &cache_root(&f.home),
        &workspace,
        Duration::from_secs(2 * 60 * 60),
    );
    let planned = plan_home_on(&f.home, SystemTime::now(), &|_| Some((0, 10)));
    assert!(
        !planned.iter().any(|plan| plan.path == leaf
            && matches!(plan.why, Unused::Gone { .. } | Unused::Idle { .. }))
    );
    let removed = reclaim_home_report_on(
        &f.home,
        SystemTime::now(),
        &|_| Some((0, 10)),
        &fence::Fence::default(),
    );
    assert!(
        leaf.is_dir(),
        "observed pressure cannot promote whole-cache age: {removed:?}"
    );
    assert_eq!(
        std::fs::read(leaf.join("debug/deps/x")).unwrap(),
        b"0123456789"
    );
    assert!(!removed.iter().any(
        |(path, why)| path == &leaf && matches!(why, Unused::Gone { .. } | Unused::Idle { .. })
    ));
}

/// BESIDE A SESSION, THE FLOOR DECIDES WHAT THE LOOK SWEEPS: the window's look with a supervised
/// session leaves the shared cache alone with room on the volume, even a gone workspace's build
/// directory a calm sweep would take. Below the floor it takes that, and prunes a live worktree's
/// stale folder, which nothing else reaches while its profile is in use. NEGATIVE CONTROL: the
/// record saying the volume holds enough keeps both, and the next pass takes them.
#[cfg(target_os = "macos")]
#[test]
fn beside_a_session_the_floor_decides_whether_the_shared_cache_is_swept() {
    let f = Fixture::new("build-cache-beside-sessions");
    let root = cache_root(&f.home);
    let live = f.dir("home/live");
    std::fs::write(live.join("Cargo.toml"), "[workspace]\n").unwrap();
    let profile = build_dir(&root, &live).join("debug");
    std::fs::create_dir_all(&profile).unwrap();
    std::fs::write(profile.join(".cargo-build-lock"), b"").unwrap();
    let dead = profile.join("incremental/ws-1k116trr0a0xf");
    std::fs::create_dir_all(dead.join("s-hneouhc0uf-1tn8o9j-2awpw")).unwrap();
    age_tree(&dead, Duration::from_secs(2 * 60 * 60));
    let removed = lay(&root, &f.root.join("home/removed"), 2 * DAY);
    let pass = |below_floor, record: &mut dyn FnMut(&Path, Unused) -> bool| {
        reclaim_beside_sessions(&f.home, SystemTime::now(), below_floor, None, record)
    };
    let ticks = aterm_uds::progress::ticks_here;

    let before = ticks();
    assert_eq!(pass(false, &mut |_, _| true), Vec::new());
    assert!(dead.is_dir() && removed.is_dir());
    assert_eq!(ticks(), before, "no walk, so nothing to hear");
    let went = pass(true, &mut |_, _| false);
    assert!(went.iter().all(|(_, why)| why.nothing_went()), "{went:?}");
    assert!(dead.is_dir() && removed.is_dir());
    assert!(ticks() > before, "the walk says it is working");

    let went = pass(true, &mut |_, _| true);
    assert!(
        went.iter()
            .any(|(_, why)| matches!(why, Unused::Stale { incremental: 1, .. })),
        "{went:?}"
    );
    assert!(!dead.exists() && !removed.exists());
}

/// WHAT COSTS NOTHING GOES FIRST, AND ONE DECLINE ENDS THE PRUNING: a pass that stops at its goal
/// takes a gone workspace's build directory before any live worktree's stale folder, whichever
/// way the walk reaches them, so a goal the dead directory meets costs nobody a cold compile. The
/// first prune the record declines is the last it is asked: the next would only move its parts
/// aside and back. Every folder stands where it stood.
#[cfg(unix)]
#[test]
fn a_pass_takes_what_costs_nothing_first_and_stops_pruning_once_declined() {
    let f = Fixture::new("build-cache-dead-first");
    let root = cache_root(&f.home);
    let dead: Vec<PathBuf> = (0..6)
        .map(|n| {
            let live = f.dir(&format!("home/live-{n}"));
            std::fs::write(live.join("Cargo.toml"), "[workspace]\n").unwrap();
            let profile = build_dir(&root, &live).join("debug");
            std::fs::create_dir_all(&profile).unwrap();
            std::fs::write(profile.join(".cargo-build-lock"), b"").unwrap();
            let folder = profile.join("incremental/ws-1k116trr0a0xf");
            std::fs::create_dir_all(folder.join("s-hneouhc0uf-1tn8o9j-2awpw")).unwrap();
            age_tree(&folder, Duration::from_secs(2 * 60 * 60));
            folder
        })
        .collect();
    let removed = lay(&root, &f.root.join("home/removed"), 2 * DAY);

    let mut asked = Vec::new();
    reclaim_watched(
        &root,
        Some(f.home.as_path()),
        SystemTime::now(),
        ReclaimAfter::UNDER_PRESSURE,
        None,
        &mut |_, why| {
            let first = asked.is_empty();
            asked.push(why);
            first
        },
    );

    assert!(
        matches!(
            asked.as_slice(),
            [Unused::Gone { .. }, Unused::Stale { .. }]
        ),
        "{asked:?}"
    );
    assert!(!removed.exists());
    assert!(dead.iter().all(|folder| folder.is_dir()));
}

/// EACH LONG STEP OF A PASS SAYS IT IS WORKING ON ITS OWN: a walk that takes nothing, a prune that
/// moves nothing and a removal, so none hides behind another where it alone is under way. A hung
/// step still goes quiet. NEGATIVE CONTROL: a walk of nothing says nothing. Each says so through
/// [`aterm_uds::progress`], heard only by a hook on the pass's own thread.
#[cfg(unix)]
#[test]
fn each_long_step_of_a_pass_says_it_is_working() {
    fn says(root: &Path, home: &Path, step: impl FnOnce(&mut Sweep<'_, '_>)) -> bool {
        let fence = fence::Fence::default();
        let mut decline = |_: &Path, _| false;
        let mut intents = Intents::new(&mut decline);
        let before = aterm_uds::progress::ticks_here();
        step(&mut Sweep {
            root,
            home: Some(home),
            now: SystemTime::now(),
            after: ReclaimAfter::CALM.into(),
            placing: None,
            moved: false,
            compiler_store: None,
            intents: &mut intents,
            fence: &fence,
            permitted: &|| true,
            pruning: Vec::new(),
            declined: false,
        });
        aterm_uds::progress::ticks_here() > before
    }
    let f = Fixture::new("build-cache-says-working");
    let root = cache_root(&f.home);
    std::fs::create_dir_all(&root).unwrap();
    let walks = |sweep: &mut Sweep<'_, '_>| {
        sweep.walk(&root, 0);
    };
    assert!(!says(&root, &f.home, walks));

    let plain = root.join("no-build-directory-in-it");
    std::fs::create_dir(&plain).unwrap();
    assert!(says(&root, &f.home, walks), "a walk that judges nothing");
    std::fs::remove_dir(&plain).unwrap();

    let leaf = lay(&root, &f.root.join("home/removed"), 2 * DAY);
    assert!(says(&root, &f.home, walks), "a walk the record declines");
    assert!(leaf.is_dir());
    assert!(
        says(&root, &f.home, |sweep| {
            sweep.prune(leaf.parent().unwrap(), &leaf);
        }),
        "a prune with nothing stale to move"
    );

    let stage = f.dir("stage");
    std::fs::write(stage.join("x"), b"x").unwrap();
    assert!(
        says(&root, &f.home, |sweep| {
            sweep.take(&stage, stage.clone(), Unused::Unfinished, &stage);
        }),
        "a removal"
    );
    assert!(!stage.exists());

    let tooling = tooling::root(&f.home);
    let entry = tooling.join(".provision-reclaim-tool-1");
    std::fs::create_dir_all(&entry).unwrap();
    std::fs::write(entry.join("x"), b"x").unwrap();
    let before = aterm_uds::progress::ticks_here();
    tooling::reclaim_on(
        &tooling,
        SystemTime::now(),
        &mut |_, _| true,
        &fence::Fence::default(),
    );
    assert!(!entry.exists());
    assert!(
        aterm_uds::progress::ticks_here() > before,
        "a tooling removal"
    );
}

/// A KEEP PUBLISHED AS A BUILD DIRECTORY'S STALE PARTS MOVE KEEPS THEM: a live workspace's build
/// directory holds a crate's incremental folder unwritten for days; a pass moves it into a stage
/// of its own, and at that instant (the `staged` hook) a custody marker that cannot be read is
/// published in the build directory. Custody read again there once the parts moved finds it:
/// every part moves back to where it stood, the stage goes, and nothing is pruned. NEGATIVE
/// CONTROL: the hook fired with the parts in the stage, and once the marker is withdrawn the
/// next pass prunes the same folder.
#[cfg(unix)]
#[test]
fn a_keep_published_as_stale_parts_move_keeps_them() {
    let f = Fixture::new("build-cache-prune-late-keep");
    let root = cache_root(&f.home);
    let live = f.dir("home/live");
    std::fs::write(live.join("Cargo.toml"), "[workspace]\n").unwrap();
    let leaf = build_dir(&root, &live);
    let profile = leaf.join("debug");
    std::fs::create_dir_all(&profile).unwrap();
    std::fs::write(profile.join(".cargo-build-lock"), b"").unwrap();
    let stale_crate = profile.join("incremental/ws-1k116trr0a0xf");
    std::fs::create_dir_all(stale_crate.join("s-hneouhc0uf-1tn8o9j-2awpw")).unwrap();
    age_tree(&stale_crate, 4 * DAY);
    let pass = || {
        reclaim_watched(
            &root,
            Some(f.home.as_path()),
            SystemTime::now(),
            ReclaimAfter::CALM,
            None,
            &mut |_, _| true,
        )
    };
    let _staged = Staged;
    let filled = std::rc::Rc::new(std::cell::Cell::new(false));
    let (held, mut publish) = (std::rc::Rc::clone(&filled), late_keep(leaf.clone()));
    staged::on(move |stage| {
        held.set(std::fs::read_dir(stage).unwrap().count() == 1);
        publish(stage);
    });
    let went = pass();
    staged::clear();
    assert!(filled.get(), "the part never moved");
    assert!(
        !went
            .iter()
            .any(|(_, why)| matches!(why, Unused::Stale { .. })),
        "{went:?}"
    );
    assert!(stale_crate.join("s-hneouhc0uf-1tn8o9j-2awpw").is_dir());
    assert!(!leaf.with_file_name(DOOMED).exists());
    let marker = leaf.join(registered::MARKER);
    assert_eq!(std::fs::read(&marker).unwrap(), b"keep published now");

    std::fs::remove_file(&marker).unwrap();
    let went = pass();
    assert!(
        went.iter()
            .any(|(_, why)| matches!(why, Unused::Stale { incremental: 1, .. })),
        "{went:?}"
    );
    assert!(!stale_crate.exists());
}

/// A STALE PART WHOSE NAME WAS TAKEN AS IT MOVED IS NEVER PUT BACK OVER WHAT TOOK IT: as above, a
/// custody marker that cannot be read is published in the build directory at the instant its
/// stale incremental folder stands in a stage, and at that instant an empty folder (as a build
/// makes one) is made at the part's own path. The part may not be pruned, and it may not go back
/// over the folder made there: that folder stands, the part stands in its stage beside the build
/// directory, and a later pass, which reads custody at the build directory before it finishes a
/// stage left there, leaves both while the keep stands. NEGATIVE CONTROL: the hook fired with
/// the part in the stage, and once the marker is withdrawn the next pass finishes the stage.
#[cfg(unix)]
#[test]
fn a_stale_part_whose_name_was_taken_as_it_moved_is_never_put_back_over_it() {
    use std::os::unix::fs::MetadataExt as _;
    let f = Fixture::new("build-cache-prune-late-keep-taken");
    let root = cache_root(&f.home);
    let live = f.dir("home/live");
    std::fs::write(live.join("Cargo.toml"), "[workspace]\n").unwrap();
    let leaf = build_dir(&root, &live);
    let profile = leaf.join("debug");
    std::fs::create_dir_all(&profile).unwrap();
    std::fs::write(profile.join(".cargo-build-lock"), b"").unwrap();
    let stale_crate = profile.join("incremental/ws-1k116trr0a0xf");
    let session = "s-hneouhc0uf-1tn8o9j-2awpw";
    std::fs::create_dir_all(stale_crate.join(session)).unwrap();
    std::fs::write(stale_crate.join(session).join("query-cache.bin"), b"stale").unwrap();
    age_tree(&stale_crate, 4 * DAY);
    let pass = || {
        reclaim_watched(
            &root,
            Some(f.home.as_path()),
            SystemTime::now(),
            ReclaimAfter::CALM,
            None,
            &mut |_, _| true,
        )
    };
    let _staged = Staged;
    let made = std::rc::Rc::new(std::cell::Cell::new(0));
    let (taken, at, mut publish) = (
        std::rc::Rc::clone(&made),
        stale_crate.clone(),
        late_keep(leaf.clone()),
    );
    staged::on(move |stage| {
        if std::fs::read_dir(stage).unwrap().count() == 1 && taken.get() == 0 {
            std::fs::create_dir(&at).unwrap();
            taken.set(std::fs::symlink_metadata(&at).unwrap().ino());
        }
        publish(stage);
    });
    let went = pass();
    staged::clear();
    assert_ne!(made.get(), 0, "the part never moved");
    assert!(
        !went
            .iter()
            .any(|(_, why)| matches!(why, Unused::Stale { .. })),
        "{went:?}"
    );
    let marker = leaf.join(registered::MARKER);
    let staged_part = |n: usize| {
        let stages = stages_in(leaf.parent().unwrap(), OsStr::new(DOOMED));
        assert_eq!(stages.len(), n, "{stages:?}");
        stages.first().map(|stage| stage.join("0").join(session))
    };
    for again in 0..2 {
        let now = std::fs::symlink_metadata(&stale_crate).unwrap().ino();
        assert_eq!(
            now,
            made.get(),
            "pass {again}: the folder made at the part's path"
        );
        assert!(!stale_crate.join(session).exists(), "pass {again}");
        let part = staged_part(1).unwrap();
        assert_eq!(
            std::fs::read(part.join("query-cache.bin")).unwrap(),
            b"stale",
            "pass {again}"
        );
        assert_eq!(std::fs::read(&marker).unwrap(), b"keep published now");
        let went = pass();
        assert!(
            !went
                .iter()
                .any(|(_, why)| matches!(why, Unused::Stale { .. } | Unused::Unfinished)),
            "pass {again}: {went:?}"
        );
    }

    std::fs::remove_file(&marker).unwrap();
    let went = pass();
    assert!(
        went.iter()
            .any(|(_, why)| matches!(why, Unused::Unfinished)),
        "{went:?}"
    );
    assert!(staged_part(0).is_none());
}

#[cfg(unix)]
#[test]
fn reclaim_reads_a_build_with_a_target_and_keeps_one_running() {
    let f = Fixture::new("build-cache-reclaim-running");
    let root = cache_root(&f.home);
    let home = Some(f.home.as_path());
    let triple = "aarch64-apple-darwin";
    let lay_triple = |name: &str| {
        let workspace = f.dir(&format!("home/{name}"));
        std::fs::write(workspace.join("Cargo.toml"), "[workspace]\n").unwrap();
        let leaf = build_dir(&root, &workspace);
        for dir in ["debug/build", &format!("{triple}/debug/deps")] {
            std::fs::create_dir_all(leaf.join(dir)).unwrap();
        }
        std::fs::write(leaf.join(format!("{triple}/debug/deps/x")), b"x").unwrap();
        for lock in [
            "debug/.cargo-build-lock",
            &format!("{triple}/debug/.cargo-build-lock"),
        ] {
            std::fs::write(leaf.join(lock), b"").unwrap();
        }
        age_tree(&leaf, 40 * DAY);
        leaf
    };
    let cross = lay_triple("cross");
    age_by(&cross.join(format!("{triple}/debug/deps")), Duration::ZERO);
    let idle = lay_triple("idle");
    let running = lay_triple("running");
    let running_cross = lay_triple("running-cross");
    let linked = lay_triple("linked");
    let lock = linked.join("debug/.cargo-build-lock");
    std::fs::remove_file(&lock).unwrap();
    std::os::unix::fs::symlink(f.dir("elsewhere").join("lock"), &lock).unwrap();
    age_tree(&linked, 40 * DAY);
    let hold = |p: PathBuf| crate::lock::Flock::try_lock(std::fs::File::open(p).unwrap()).unwrap();
    let held = [
        hold(running.join("debug/.cargo-build-lock")),
        hold(running_cross.join(format!("{triple}/debug/.cargo-build-lock"))),
    ];

    assert_eq!(reclaim(&root, home, SystemTime::now()), vec![idle]);
    for kept in [&cross, &running, &running_cross, &linked] {
        assert!(kept.is_dir(), "{} kept", kept.display());
    }
    drop(held);
    let mut removed = reclaim(&root, home, SystemTime::now());
    removed.sort();
    assert_eq!(removed, vec![running, running_cross]);
    assert!(cross.is_dir() && linked.is_dir());
}

#[cfg(unix)]
#[test]
fn a_workspace_reached_through_a_link_is_never_looked_through() {
    let f = Fixture::new("build-cache-link-gone");
    let target = f.dir("home/Documents/viaLink");
    f.dir("home/links");
    std::os::unix::fs::symlink(&target, f.home.join("links/doc")).unwrap();
    std::os::unix::fs::symlink(f.home.join("Documents"), f.home.join("via")).unwrap();
    for (checkout, link) in [
        (f.home.join("links/doc"), f.home.join("links/doc")),
        (f.home.join("via/viaLink"), f.home.join("via")),
    ] {
        let mut looked = Vec::new();
        let verdict = checkout::gone_by(&checkout, Some(&f.home), &mut |p| {
            looked.push(p.to_path_buf());
            std::fs::symlink_metadata(p).map(|m| m.is_dir())
        });
        assert!(
            verdict
                .as_ref()
                .is_err_and(|why| why.contains("a link is not followed")),
            "{}: {verdict:?}",
            checkout.display()
        );
        assert_eq!(looked.last(), Some(&link), "{looked:?}");
        assert!(
            !looked.iter().any(|p| p.starts_with(&link) && *p != link),
            "{} looked through the link: {looked:?}",
            checkout.display()
        );
    }
    let real = f.dir("home/real/ws");
    let mut looked = Vec::new();
    let verdict = checkout::gone_by(&real, Some(&f.home), &mut |p| {
        looked.push(p.to_path_buf());
        std::fs::symlink_metadata(p).map(|m| m.is_dir())
    });
    assert!(verdict.is_ok(), "{verdict:?}");
    assert!(looked.contains(&real.join("Cargo.toml")), "{looked:?}");
}

#[cfg(unix)]
#[test]
fn a_live_build_directory_loses_only_what_no_build_reads_again() {
    let f = Fixture::new("build-cache-stale");
    let root = cache_root(&f.home);
    let home = Some(f.home.as_path());
    let live = f.dir("home/live");
    std::fs::write(live.join("Cargo.toml"), "[workspace]\n").unwrap();
    let leaf = build_dir(&root, &live);
    let profile = leaf.join("debug");
    std::fs::create_dir_all(profile.join("deps")).unwrap();
    let program = crate::store::default_prefix(&f.home).join("store/trust");
    let sysroot = program.join("2");
    std::fs::create_dir_all(&sysroot).unwrap();
    for dir in sysroot.ancestors().take_while(|d| d.starts_with(&f.home)) {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    std::os::unix::fs::symlink("2", program.join("current")).unwrap();
    let info = format!(
        r#"{{"rustc_fingerprint":1,"outputs":{{"9":{{"success":true,"status":"","code":0,"stdout":"___\nlib___.rlib\n{}\noff\n","stderr":""}}}},"successes":{{}}}}"#,
        sysroot.display()
    );
    std::fs::write(leaf.join(".rustc_info.json"), info).unwrap();
    let lock = profile.join(".cargo-build-lock");
    std::fs::write(&lock, b"").unwrap();
    let unit = |hash: &str, rustc: u64| {
        let fp = profile.join(format!(".fingerprint/ws-{hash}"));
        std::fs::create_dir_all(&fp).unwrap();
        let json = format!(r#"{{"rustc":{rustc},"features":"[]"}}"#);
        std::fs::write(fp.join("lib-ws.json"), json).unwrap();
        let rlib = profile.join(format!("deps/libws-{hash}.rlib"));
        std::fs::write(&rlib, b"rlib").unwrap();
        (fp, rlib)
    };
    let (new_fp, new_rlib) = unit("2222222222222222", 2);
    let (old_fp, old_rlib) = unit("1111111111111111", 1);
    for p in [old_fp.join("lib-ws.json"), old_fp.clone(), old_rlib.clone()] {
        age_by(&p, 4 * DAY);
    }
    let stale_crate = profile.join("incremental/ws-1k116trr0a0xf");
    let fresh_crate = profile.join("incremental/ws-3g63pv341xugh");
    for folder in [&stale_crate, &fresh_crate] {
        std::fs::create_dir_all(folder.join("s-hneouhc0uf-1tn8o9j-2awpw")).unwrap();
        std::fs::write(folder.join("s-hneouhc0uf-1tn8o9j.lock"), b"").unwrap();
    }
    age_tree(&stale_crate, 4 * DAY);
    let stage = leaf.with_file_name(DOOMED);
    let doomed = {
        let mut d = vec![stale_crate.clone(), old_fp.clone(), old_rlib.clone()];
        d.sort();
        d
    };
    let why = Unused::Stale {
        incremental: 1,
        units: 1,
        superseded: 0,
        after: RECLAIM_STALE,
    };
    let dry = |held: bool| {
        let mut planned = plan(&root, home, SystemTime::now(), ReclaimAfter::CALM, None);
        for p in &mut planned {
            p.paths.sort();
        }
        assert_eq!(
            planned,
            vec![Planned {
                path: leaf.clone(),
                why,
                paths: doomed.clone(),
                held,
                stays: None,
            }]
        );
    };
    let kept = || {
        for p in [
            &new_fp,
            &new_rlib,
            &fresh_crate,
            &lock,
            &leaf.join(".rustc_info.json"),
        ] {
            assert!(p.exists(), "{} kept", p.display());
        }
    };
    let pass = |answer: bool, asked: &mut Vec<(PathBuf, Unused, usize)>| {
        reclaim_watched(
            &root,
            home,
            SystemTime::now(),
            ReclaimAfter::CALM,
            None,
            &mut |p, w| {
                let staged = std::fs::read_dir(p).map_or(0, |d| d.count());
                asked.push((p.to_path_buf(), w, staged));
                answer
            },
        )
    };

    dry(false);
    assert!(
        doomed.iter().all(|p| p.exists()),
        "the dry run moves nothing"
    );

    let held = crate::lock::Flock::try_lock(std::fs::File::open(&lock).unwrap()).unwrap();
    dry(true);
    let mut asked = Vec::new();
    assert!(pass(true, &mut asked).is_empty());
    assert!(
        asked.is_empty(),
        "nothing asked while a build holds the lock"
    );
    assert!(doomed.iter().all(|p| p.exists()));
    drop(held);

    assert!(pass(false, &mut asked).is_empty());
    assert_eq!(
        asked,
        vec![(stage.clone(), why, 3)],
        "the stage holds what goes"
    );
    assert!(
        doomed.iter().all(|p| p.exists()),
        "refused, every entry moved back"
    );
    assert_eq!(std::fs::read(&old_rlib).unwrap(), b"rlib");
    assert!(!stage.exists());
    kept();

    asked.clear();
    assert_eq!(pass(true, &mut asked), vec![(stage.clone(), why)]);
    assert_eq!(asked, vec![(stage.clone(), why, 3)]);
    assert!(doomed.iter().all(|p| !p.exists()) && !stage.exists());
    kept();
    assert!(plan(&root, home, SystemTime::now(), ReclaimAfter::CALM, None).is_empty());
}

/// THE DAILY BOUND TAKES WHAT THE LAST PASSING RELEASE RUN SUPERSEDED, under every custody
/// recheck ([`stale::USED`]): a release clone's build directory holds the test unit of the
/// candidate before (unwritten five days, which that run, begun four days ago, did not use), the
/// test unit it used (unwritten as long: cargo found it fresh) and a test unit built an hour ago,
/// all of a package the run built tests of. While a build
/// holds the profile's cargo lock the pass takes nothing; once it is let go, the pass takes the
/// superseded unit's folder whole, saying so, and keeps the other two; the listing (`aterm pkg
/// machine cache`) named it first. NEGATIVE CONTROL: the same directory with no record of the
/// run loses nothing.
#[cfg(unix)]
#[test]
fn the_daily_bound_takes_what_the_last_passing_release_run_superseded() {
    let f = Fixture::new("build-cache-superseded");
    let root = cache_root(&f.home);
    let clone = f.dir("home/aterm-cut");
    std::fs::write(clone.join("Cargo.toml"), "[workspace]\n").unwrap();
    let leaf = build_dir(&root, &clone);
    let profile = leaf.join("debug");
    std::fs::create_dir_all(&profile).unwrap();
    let lock = profile.join(".cargo-build-lock");
    std::fs::write(&lock, b"").unwrap();
    let unit = |hash: &str, ago: Duration| {
        let dir = profile.join("build/aterm-grid").join(hash);
        std::fs::create_dir_all(dir.join("fingerprint")).unwrap();
        std::fs::create_dir_all(dir.join("out")).unwrap();
        std::fs::write(dir.join("fingerprint/test-lib-aterm_grid"), b"h").unwrap();
        std::fs::write(
            dir.join("fingerprint/test-lib-aterm_grid.json"),
            b"{\"rustc\":2,\"features\":\"[]\"}",
        )
        .unwrap();
        std::fs::write(dir.join(format!("out/aterm_grid-{hash}")), b"exe").unwrap();
        age_tree(&dir, ago);
        dir
    };
    const DAY: Duration = Duration::from_secs(24 * 60 * 60);
    let superseded = unit("1111111111111111", 5 * DAY);
    let used = unit("2222222222222222", 5 * DAY);
    let fresh = unit("3333333333333333", Duration::from_secs(60 * 60));
    let began = SystemTime::now() - 4 * DAY - Duration::from_secs(60 * 60);
    let mut record = stale::Used {
        began_ms: u64::try_from(
            began
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_millis(),
        )
        .unwrap(),
        members: std::collections::BTreeSet::from(["aterm-grid".to_owned()]),
        ..stale::Used::default()
    };
    record.add(&leaf, &used.join("out/aterm_grid-2222222222222222"));
    record.keep(&leaf).unwrap();
    age_by(&leaf.join(stale::USED), 4 * DAY);
    let pass = || {
        reclaim_watched(
            &root,
            Some(f.home.as_path()),
            SystemTime::now(),
            ReclaimAfter::CALM,
            None,
            &mut |_, _| true,
        )
    };
    let took = |went: &[(PathBuf, Unused)]| {
        went.iter().any(|(_, why)| {
            matches!(
                why,
                Unused::Stale {
                    superseded: 1,
                    units: 0,
                    ..
                }
            )
        })
    };

    let listed = plan(
        &root,
        Some(&f.home),
        SystemTime::now(),
        ReclaimAfter::CALM,
        None,
    );
    let row = listed.iter().find(|p| p.path == leaf).expect("listed");
    assert!(
        matches!(row.why, Unused::Stale { superseded: 1, .. }),
        "{:?}",
        row.why
    );
    assert_eq!(row.paths, vec![superseded.clone()]);

    let held = crate::lock::Flock::try_lock(std::fs::File::open(&lock).unwrap()).unwrap();
    let went = pass();
    assert!(!took(&went), "{went:?}");
    assert!(superseded.is_dir());
    drop(held);

    let went = pass();
    assert!(took(&went), "{went:?}");
    assert!(!superseded.exists());
    assert!(used.is_dir() && fresh.is_dir());

    std::fs::remove_file(leaf.join(stale::USED)).unwrap();
    let again = unit("1111111111111111", 5 * DAY);
    let went = pass();
    assert!(!took(&went), "{went:?}");
    assert!(again.is_dir() && used.is_dir() && fresh.is_dir());
}
