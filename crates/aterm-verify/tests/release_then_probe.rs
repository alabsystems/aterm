// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A LOCK THIS PROCESS RELEASED IS NOT FREE YET — the workspace-wide tripwire
//! for the shape that produces it.
//!
//! THE MECHANISM, measured 2026-09-17 with a 30-line probe (lock a file, drop
//! it, immediately re-lock, with one sibling thread running `/usr/bin/true`):
//! the first re-lock hits `WouldBlock`. `flock(2)` — and a listening socket —
//! is released only when EVERY descriptor on the open file description is
//! closed. A `fork`/`posix_spawn` anywhere else in the same test binary copies
//! every open descriptor into the child, which holds them until it `exec`s:
//! `FD_CLOEXEC` closes at exec, NEVER at fork. So a lock a thread released a
//! microsecond ago reads as HELD, and a just-dropped `UnixListener` still
//! ACCEPTS, for the length of somebody else's spawn — up to ~523 ms under load
//! (`aterm-pty/src/unix.rs:739-744`).
//!
//! Two tests hit this on 2026-09-17 (`atpkg::lock`'s unit test and
//! `aterm-link`'s dead-socket fixture) and were repaired one at a time, after
//! being taken for flakes. Three more had the same shape and had not fired yet:
//! `atpkg`'s `store_lock_wait` integration test, `atpkg::noindex`'s
//! build-in-progress probe, and `aterm-update`'s dedup-wait test. The shape is
//! what recurs, so the shape is what is checked.
//!
//! THE TWO SOUND SPELLINGS, in order of preference:
//!
//!  1. RELEASE DETERMINISTICALLY. `file.unlock()` (`LOCK_UN`) strips the lock
//!     from the open file description itself, every inherited duplicate
//!     included, so nothing has to be waited out. Reach for this whenever the
//!     property under test is NOT drop-release —
//!     `aterm-gui/src/control_auth.rs` and `atpkg::noindex`'s test helper both
//!     do, and both say why at the site. There is no `LOCK_UN` for a listening
//!     socket; unlink the path and bind a fresh one instead.
//!  2. POLL A BOUNDED WINDOW. When drop-release IS the property, the claim
//!     cannot change — so keep the assertion and relax only the INSTANT it must
//!     be true by, with a deadline whose expiry still fails loudly and names
//!     the last refusal.
//!
//! WHAT IS NOT A REMEDY: deleting the assertion, `#[ignore]`, a bare `sleep`
//! long enough to usually work, or calling it a flake. The mechanism is
//! measured; a test that fails for it is a test that is right about the
//! machine and wrong about the instant.
//!
//! A SHARED HELPER WOULD BE BETTER THAN SEVERAL SPELLINGS OF THE LOOP — this
//! workspace now has three (`aterm-link`'s `wait_until_abandoned`, `aterm-gui`'s
//! `settle_busy`, `pinned_dir`'s inline loop; `atpkg::lock`'s went on
//! 2026-09-23, when its guards took the first spelling) — and it belongs in a
//! dev-only crate both `atpkg` and `aterm-update` can take as a path
//! `[dev-dependencies]`. It is NOT what stops the next site
//! landing, though: a helper only helps whoever already knows to reach for it.
//! This tripwire is what makes the next one impossible to land unnoticed, so it
//! comes first.
#![cfg(unix)]

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::process::Command;

/// Files that DO carry the shape and are sound anyway. Each row is a reason
/// that must still be true; a row whose file no longer shows the shape is a
/// refusal of its own, so the registry cannot rot into permission.
const REGISTERED: &[(&str, &str)] = &[
    (
        "crates/aterm-uds/tests/roundtrip.rs",
        "the listener is dropped and the socket path is UNLINKED before the rebind, so the \
         bind creates a new inode — an inherited descriptor for the old one cannot refuse \
         it. The sibling case in the same file (a stale socket that must stop accepting) \
         already polls a bounded window.",
    ),
    (
        "crates/aterm-update/src/check_lane.rs",
        "`Lane::try_lock` is a `std::sync::Mutex`, not an `flock`: an in-process mutex is \
         not carried by a file descriptor and a fork cannot hold it. The FILE lock in the \
         same module is the one that had to be polled, and is.",
    ),
    (
        "crates/atpkg/src/lock.rs",
        "every lock released here is a `StoreLock` or a `Flock`, whose drop is `LOCK_UN` — \
         the deterministic release, pinned by \
         `a_dropped_store_lock_is_free_while_a_copy_of_its_descriptor_lives` with a live \
         duplicate of the descriptor — so a drop then one sample is sound (2026-09-23). \
         The waits re-acquire through `lock_store_waiting`, whose whole subject is waiting.",
    ),
    (
        "crates/aterm-ctl/src/lib.rs",
        "the dead-alias fallback test drops its listener and then UNLINKS both socket \
         paths before it probes, and the probe is `connect` on the unlinked path expecting \
         ENOENT — an inherited descriptor keeps the old inode accepting, but there is no \
         path left to reach it by, so the answer cannot depend on the spawn window. The \
         same reasoning as the `aterm-uds` roundtrip row. Every crash-state corpse in the \
         file (the dead-alias test's, the refused-socket probe's), dropped and deliberately \
         NOT unlinked, goes through `abandon_socket`, which polls (bounded) until it \
         refuses before anything is judged over it.",
    ),
];

/// This file's own control test writes the defect out as string literals, on
/// purpose, so the scan reads its own fixtures as sites unless it is told not
/// to. It is told not to: the fixtures are judged by
/// `the_scan_finds_the_defect_and_clears_both_repairs`, where the whole point
/// is that the shape IS present.
const SELF: &str = "crates/aterm-verify/tests/release_then_probe.rs";

/// How many lines after a release count as "immediately".
const WINDOW: usize = 12;

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .expect("the repo root is two levels above this crate")
}

/// Every tracked `.rs` file outside `vendor/` — third-party source aterm
/// mirrors rather than authors, and not ours to hold to this rule.
fn tracked_rs(root: &Path) -> Option<Vec<String>> {
    let out = Command::new("git")
        .args(["ls-files", "-z", "*.rs"])
        .current_dir(root)
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    Some(
        String::from_utf8_lossy(&out.stdout)
            .split('\0')
            .filter(|p| !p.is_empty() && !p.starts_with("vendor/") && *p != SELF)
            .map(str::to_owned)
            .collect(),
    )
}

/// `true` when `line` names a lock-ish or listener-ish binding being dropped, or
/// drops a listener in the expression that binds it (`drop(X::bind(..))`, the
/// socket file a crashed instance leaves).
fn releases(line: &str) -> bool {
    let t = line.trim_start();
    if t.starts_with("//") || t.starts_with('*') {
        return false;
    }
    let Some(rest) = t.split_once("drop(").map(|(_, r)| r) else {
        return false;
    };
    let Some((name, _)) = rest.split_once(')') else {
        return false;
    };
    let name = name.trim();
    if name.contains("Listener::bind(") {
        return true;
    }
    !name.is_empty()
        && name.chars().all(|c| c.is_alphanumeric() || c == '_')
        && ["lock", "listen", "guard", "lease", "held", "holder"]
            .iter()
            .any(|k| name.to_ascii_lowercase().contains(k))
}

/// The probes that ask "is it free NOW?".
fn probes(text: &str) -> bool {
    [
        "try_lock",
        "lock_store",
        "::bind(",
        "Stream::connect",
        "::connect(",
        "build_in_progress",
        "acquire_within",
        "FileLock::acquire",
        // aterm-ctl's discovery dial: a `connect` behind a name.
        "probe_lines(",
    ]
    .iter()
    .any(|p| text.contains(p))
}

/// Anything that shows the site already waits out the window.
fn is_patient(text: &str) -> bool {
    [
        "deadline",
        "loop {",
        "while ",
        "settle",
        "wait_until",
        "poll",
        "Instant::now() +",
        "sleep(",
    ]
    .iter()
    .any(|p| text.contains(p))
}

/// …and anything that shows the release itself was deterministic.
fn is_deterministic(text: &str) -> bool {
    text.contains("unlock(") || text.contains("LOCK_UN")
}

/// `path -> the lines carrying the shape`.
fn scan(root: &Path, files: &[String]) -> BTreeMap<String, Vec<String>> {
    let mut found: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for rel in files {
        let Ok(text) = std::fs::read_to_string(root.join(rel)) else {
            continue;
        };
        let lines: Vec<&str> = text.lines().collect();
        for (i, line) in lines.iter().enumerate() {
            if !releases(line) {
                continue;
            }
            let before = lines[i.saturating_sub(3)..=i].join("\n");
            if is_deterministic(&before) {
                continue;
            }
            let after = lines[i + 1..lines.len().min(i + 1 + WINDOW)].join("\n");
            if probes(&after) && !is_patient(&after) {
                found.entry(rel.clone()).or_default().push(format!(
                    "{}:{}: {}",
                    rel,
                    i + 1,
                    line.trim()
                ));
            }
        }
    }
    found
}

#[test]
fn no_test_samples_a_just_released_lock_or_listener_once() {
    let root = repo_root();
    let Some(files) = tracked_rs(&root) else {
        panic!(
            "no_test_samples_a_just_released_lock_or_listener_once: `git ls-files` could not \
             list {} — an empty scan is never a pass, so this fails rather than skipping.",
            root.display()
        );
    };
    assert!(
        files.len() > 100,
        "the file list is implausibly short ({}), so the scan covered nothing",
        files.len()
    );

    let found = scan(&root, &files);
    let registered: BTreeSet<&str> = REGISTERED.iter().map(|(p, _)| *p).collect();

    let unregistered: Vec<String> = found
        .iter()
        .filter(|(p, _)| !registered.contains(p.as_str()))
        .flat_map(|(_, v)| v.clone())
        .collect();
    assert!(
        unregistered.is_empty(),
        "a lock or listener is released and probed once, within {WINDOW} lines, with nothing \
         waiting out the spawn window:\n    {}\n\n\
         `flock` and a listening socket are released only when EVERY descriptor on the open \
         file description closes, and a `fork`/`posix_spawn` anywhere else in the same test \
         binary copies every descriptor into the child until it `exec`s. So this reads as \
         HELD for the length of someone else's spawn (~523 ms measured under load).\n\
         Fix it by releasing deterministically (`file.unlock()` / `LOCK_UN`, which strips the \
         lock from the description itself) where drop-release is not the property under \
         test, or by polling a bounded deadline that still fails loudly where it is. If the \
         site is sound for a reason, add it to REGISTERED with that reason.",
        unregistered.join("\n    ")
    );

    let stale: Vec<&str> = REGISTERED
        .iter()
        .map(|(p, _)| *p)
        .filter(|p| !found.contains_key(*p))
        .collect();
    assert!(
        stale.is_empty(),
        "REGISTERED names {stale:?}, which no longer carries the shape. A registry that \
         keeps entries it has stopped needing is a registry that will one day sanction \
         something nobody read: delete the row."
    );

    for (path, why) in REGISTERED {
        assert!(
            why.len() > 40,
            "{path}'s registration has no reason worth the name: {why:?}"
        );
    }
}

/// THE CONTROL. Every clause above is conditional on the scan being able to
/// SEE the shape, and a scan that finds nothing passes. So the same functions
/// are driven over text that is unambiguously the defect, and over each of the
/// two sound spellings — otherwise a typo in a pattern would read as a clean
/// workspace forever.
#[test]
fn the_scan_finds_the_defect_and_clears_both_repairs() {
    let bad = "        drop(guard);\n        assert!(try_lock_store(&b).is_ok());\n";
    let polled = "        drop(guard);\n        let deadline = now + TEN;\n        loop { \
                  if try_lock_store(&b).is_ok() { break; } }\n";
    let deterministic = "        lease.unlock().unwrap();\n        drop(lease);\n        \
                         assert!(lease.try_lock().is_ok());\n";

    let judge = |text: &str| -> bool {
        let lines: Vec<&str> = text.lines().collect();
        lines.iter().enumerate().any(|(i, line)| {
            releases(line) && !is_deterministic(&lines[i.saturating_sub(3)..=i].join("\n")) && {
                let after = lines[i + 1..lines.len().min(i + 1 + WINDOW)].join("\n");
                probes(&after) && !is_patient(&after)
            }
        })
    };

    assert!(judge(bad), "the sample-once shape must be found");
    assert!(!judge(polled), "a bounded poll must clear it");
    assert!(!judge(deterministic), "an explicit LOCK_UN must clear it");
    assert!(
        !judge("        drop(rows);\n        assert!(try_lock().is_ok());\n"),
        "a drop of something that is not a lock is not this shape"
    );
    // A listener dropped in the expression that bound it, then dialed once: the
    // shape `aterm-ctl`'s refused-socket probe carried until 2026-09-23.
    let bound_and_dropped = "        drop(aterm_uds::CtlListener::bind(&stale).expect(\"x\"));\n        \
                             let stale_s = stale.to_str().unwrap();\n        \
                             assert!(matches!(probe_lines(stale_s, \"v\", 0), Probe::Refused { .. }));\n";
    assert!(
        judge(bound_and_dropped),
        "a listener dropped where it was bound must be found"
    );
}
