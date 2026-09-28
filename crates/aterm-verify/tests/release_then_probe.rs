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
//! WHAT THE SCAN READS (round 3 of the update work, 2026-09-27). CODE, within
//! one function: a comment neither probes nor waits, so a doc comment naming
//! `FileLock::acquire` after a release is not a probe and one that says "poll"
//! is not a remedy. The one comment it reads is the release's own annotation —
//! its trailing comment, or the comment block directly above it — which may
//! say the release is `LOCK_UN` (a guard whose `Drop` unlocks; a lexical scan
//! cannot see the impl). The tree's lock helpers (`checker_gate`,
//! `lock_briefly`, …) are probes like the raw calls they wrap; a new one is a
//! row in `probes`, or the scan is blind to it.
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
         same module is an `aterm_update_core::FileLock`, whose drop is `LOCK_UN` \
         (2026-09-24), so it is sampled once.",
    ),
    (
        "crates/aterm-update-core/src/sys.rs",
        "every lock released here is an `aterm_update_core::FileLock`, whose drop is \
         `LOCK_UN` (2026-09-24) — pinned by \
         `a_dropped_lock_is_free_while_a_copy_of_its_descriptor_lives` with a live \
         duplicate of the descriptor — so a drop then one sample is sound; the bounded \
         tests beside it re-acquire through `acquire_within`, whose subject is waiting.",
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

/// How many CODE lines after a release count as "immediately" — and how many
/// above it the scan reads for the code that made the release deterministic.
/// Comment lines are not counted: a long explanation between a release and
/// its probe does not make the probe any later.
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

/// The CODE on `line`, which is all the scan judges (round 3 of the update
/// work, 2026-09-27: `probes` read comments, so a doc comment NAMING
/// `FileLock::acquire` after a release read as a probe, and a comment that
/// merely said "poll" or "LOCK_UN" read as the remedy). `None` for a comment
/// line — `//`, `///`, `//!`, `/*`, a block comment's `* ` continuation or its
/// `*/` — else the line with any trailing `//` comment cut off. The cut is
/// found outside a string literal by counting the unescaped quotes before it
/// (a `'"'` char literal is not one): lexical, so a raw string that ends in a
/// backslash can still miscount it — rare in this tree's tests, and the
/// fixtures below pin the common shapes.
fn code(line: &str) -> Option<&str> {
    let t = line.trim_start();
    if t.starts_with("//")
        || t.starts_with("/*")
        || t.starts_with("*/")
        || t == "*"
        || t.starts_with("* ")
    {
        return None;
    }
    let bytes = line.as_bytes();
    let mut quotes = 0usize;
    let mut i = 0;
    while i + 1 < bytes.len() {
        match bytes[i] {
            b'\\' => {
                i += 2;
                continue;
            }
            b'\'' if bytes.get(i + 1) == Some(&b'"') && bytes.get(i + 2) == Some(&b'\'') => {
                i += 3;
                continue;
            }
            b'"' => quotes += 1,
            b'/' if bytes[i + 1] == b'/' && quotes.is_multiple_of(2) => return Some(&line[..i]),
            _ => {}
        }
        i += 1;
    }
    Some(line)
}

/// The trailing comment of `line` (after [`code`]'s cut), or `""`.
fn trailing_comment(line: &str) -> &str {
    code(line).map_or("", |kept| &line[kept.len()..])
}

/// The binding `code` drops when it names a lock-ish or listener-ish one — or
/// the whole expression, when it drops a listener in the expression that binds
/// it (`drop(X::bind(..))`, the socket file a crashed instance leaves).
fn released(code: &str) -> Option<&str> {
    let rest = code.split_once("drop(")?.1;
    let name = rest.split_once(')')?.0.trim();
    if name.contains("Listener::bind(") {
        return Some(name);
    }
    (!name.is_empty()
        && name.chars().all(|c| c.is_alphanumeric() || c == '_')
        && ["lock", "listen", "guard", "lease", "held", "holder"]
            .iter()
            .any(|k| name.to_ascii_lowercase().contains(k)))
    .then_some(name)
}

/// The probes that ask "is it free NOW?" — the raw calls, and the tree's
/// helpers that take a lock behind a name of their own (round 3: a probe
/// through `aterm-update`'s `checker_gate`, a wrapper over `acquire_within`,
/// was invisible). `Lock::acquire(` covers every `…Lock::acquire(` type
/// (`FileLock`, `ProcessLock`); a helper that takes a lock under a new name is
/// a row here, or the scan cannot see the probe it makes.
fn probes(code: &str) -> bool {
    [
        "try_lock",
        "lock_store",
        "::bind(",
        "Stream::connect",
        "::connect(",
        "build_in_progress",
        "acquire_within",
        "Lock::acquire(",
        "lock_open(",
        // aterm-update's machine-wide `checker.lock`, over `acquire_within`.
        "checker_gate(",
        // aterm-update's recovery ledger, aterm-gui's save lock and identity
        // claim: a `try_lock` behind a name.
        "lock_briefly(",
        "lock_and_verify_target(",
        "lock_exclusive(",
        // aterm-ctl's discovery dial: a `connect` behind a name.
        "probe_lines(",
    ]
    .iter()
    .any(|p| code.contains(p))
}

/// Anything IN CODE that shows the site already waits out the window. A
/// comment that says "poll" waits for nothing.
fn is_patient(code: &str) -> bool {
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
    .any(|p| code.contains(p))
}

/// Whether a line starts a function — both reads stop there, so another
/// function's `unlock()` is never this release's evidence, and the next test's
/// first probe is never this release's "immediately after". An attribute is
/// not a boundary: `#[cfg(unix)]` on a statement is inside the function.
fn opens_an_item(code: &str) -> bool {
    for word in code.split_whitespace() {
        match word {
            "fn" => return true,
            "pub" | "async" | "unsafe" | "const" | "extern" => {}
            w if w.starts_with("pub(") => {}
            _ => return false,
        }
    }
    false
}

/// Whether `code` names `name` as a whole identifier — never as a piece of a
/// longer one. A substring test read every `unlock(` as naming a binding
/// called `lock` (`unlock` ends in it), and `lock` is the commonest name a
/// released guard has, so ANOTHER binding's `other.unlock()` cleared a
/// `drop(lock)` it had nothing to do with (the round-3 review of this scan).
fn names(code: &str, name: &str) -> bool {
    let ident = |c: char| c.is_alphanumeric() || c == '_';
    !name.is_empty()
        && code.match_indices(name).any(|(at, _)| {
            !code[..at].chars().next_back().is_some_and(ident)
                && !code[at + name.len()..].chars().next().is_some_and(ident)
        })
}

/// …and anything that shows the release at `at` of `name` was DETERMINISTIC
/// (`LOCK_UN`, which strips the lock from the open file description itself).
/// Two kinds of evidence count, each where it belongs to THIS release:
///
///  * CODE — a `LOCK_UN` or an `unlock(` on a line that NAMES the released
///    binding as a whole identifier ([`names`]: `lease.unlock()`,
///    `libc::flock(guard.as_raw_fd(), libc::LOCK_UN)`), on the release's
///    line or up to [`WINDOW`] code lines above it within the same function.
///    This used to be ANY `unlock(` or `LOCK_UN` within three lines, and then
///    any `LOCK_UN`, or an `unlock(` merely CONTAINING the name, within the
///    wider window — so another descriptor's `flock(other_fd, LOCK_UN)`, or
///    another binding's `other.unlock()` above a `drop(lock)`, cleared a
///    release that nothing had made deterministic. An unlock the scan cannot
///    tie to the binding (a raw fd taken earlier) is said at the release, in
///    its annotation — below.
///  * THE RELEASE'S OWN ANNOTATION — its trailing comment, or the comment
///    block directly above it (no blank line between) — naming `LOCK_UN`.
///    That is how the tree marks a guard whose `Drop` sends `LOCK_UN`
///    ("Released by `LOCK_UN` (the guard's drop)"): a `Drop` impl is not
///    visible to a lexical scan, so the site says it where the release is,
///    and each such guard has a test that pins it with a live duplicate of
///    its descriptor. The whole block counts, however long; a comment
///    anywhere else — a function's doc, a paragraph a blank line away — is
///    not about this release, and does not.
fn is_deterministic(lines: &[&str], at: usize, name: &str) -> bool {
    let code_says =
        |code: &str| (code.contains("LOCK_UN") || code.contains("unlock(")) && names(code, name);
    if code(lines[at]).is_some_and(code_says) || trailing_comment(lines[at]).contains("LOCK_UN") {
        return true;
    }
    let mut annotation = true;
    let mut read = 0;
    for line in lines[..at].iter().rev() {
        let Some(above) = code(line) else {
            if annotation && line.contains("LOCK_UN") {
                return true;
            }
            continue;
        };
        annotation = false;
        if above.trim().is_empty() {
            continue;
        }
        if opens_an_item(above) {
            break;
        }
        if code_says(above) {
            return true;
        }
        read += 1;
        if read >= WINDOW {
            break;
        }
    }
    false
}

/// The line indexes in `lines` that carry the shape: a release, not shown
/// deterministic, followed within [`WINDOW`] code lines of the same function
/// by a probe with nothing patient among them. The ONE judgement — the
/// workspace scan and the control tests both drive it, so a fixture can never
/// pass a rule the scan does not apply (the control used to re-spell it).
fn sites(lines: &[&str]) -> Vec<usize> {
    let mut found = Vec::new();
    for (i, line) in lines.iter().enumerate() {
        let Some(name) = code(line).and_then(released) else {
            continue;
        };
        if is_deterministic(lines, i, name) {
            continue;
        }
        let after = lines[i + 1..]
            .iter()
            .filter_map(|l| code(l))
            .filter(|c| !c.trim().is_empty())
            .take_while(|c| !opens_an_item(c))
            .take(WINDOW)
            .collect::<Vec<_>>()
            .join("\n");
        if probes(&after) && !is_patient(&after) {
            found.push(i);
        }
    }
    found
}

/// `path -> the lines carrying the shape`.
fn scan(root: &Path, files: &[String]) -> BTreeMap<String, Vec<String>> {
    let mut found: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for rel in files {
        let Ok(text) = std::fs::read_to_string(root.join(rel)) else {
            continue;
        };
        let lines: Vec<&str> = text.lines().collect();
        for i in sites(&lines) {
            found.entry(rel.clone()).or_default().push(format!(
                "{}:{}: {}",
                rel,
                i + 1,
                lines[i].trim()
            ));
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

/// Whether the scan finds the shape anywhere in `text` — the same judgement
/// [`scan`] applies to every tracked file.
fn judge(text: &str) -> bool {
    let lines: Vec<&str> = text.lines().collect();
    !sites(&lines).is_empty()
}

/// THE CONTROL. Every clause above is conditional on the scan being able to
/// SEE the shape, and a scan that finds nothing passes. So the same judgement
/// is driven over text that is unambiguously the defect, and over each of the
/// two sound spellings — otherwise a typo in a pattern would read as a clean
/// workspace forever.
#[test]
fn the_scan_finds_the_defect_and_clears_both_repairs() {
    let bad = "        drop(guard);\n        assert!(try_lock_store(&b).is_ok());\n";
    let polled = "        drop(guard);\n        let deadline = now + TEN;\n        loop { \
                  if try_lock_store(&b).is_ok() { break; } }\n";
    let deterministic = "        lease.unlock().unwrap();\n        drop(lease);\n        \
                         assert!(lease.try_lock().is_ok());\n";

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

/// ONLY CODE IS JUDGED (round 3, 2026-09-27), both ways. A comment that NAMES
/// a probe after a release is not a probe — the doc comment naming
/// `FileLock::acquire` beside `aterm-update`'s checker-gate test tripped the
/// scan — and a comment that SAYS "poll" or "deadline" is not patience. A real
/// probe with such a comment between it and the release is still found, and
/// a trailing comment on the probe's own line changes nothing.
#[test]
fn comments_are_neither_probes_nor_remedies() {
    let named_in_a_comment = "        drop(holder);\n        \
                              // The next cycle would block in `FileLock::acquire` here.\n        \
                              /* or in try_lock */\n        \
                              let seen = count(&path);\n";
    assert!(
        !judge(named_in_a_comment),
        "a probe named only in a comment is not a probe"
    );
    let trailing = "        drop(holder);\n        \
                    let seen = count(&path); // not FileLock::acquire, not try_lock\n";
    assert!(!judge(trailing), "nor is one named in a trailing comment");
    let url = "        drop(holder);\n        let seen = fetch(\"https://x//try_lock\");\n";
    assert!(
        judge(url),
        "a `//` inside a string literal is not a comment: the probe after it is code"
    );
    let quote_char = "        drop(holder);\n        \
                      let q = '\"'; let seen = count(&path); // not try_lock\n";
    assert!(
        !judge(quote_char),
        "a `'\"'` char literal is not a string's quote: the comment after it is still cut"
    );

    let comment_says_poll = "        drop(guard);\n        \
                             // no need to poll or set a deadline: it is free now\n        \
                             assert!(try_lock_store(&b).is_ok());\n";
    assert!(
        judge(comment_says_poll),
        "a comment that says `poll` waits for nothing: the probe is found"
    );
    let long_comment_between = format!(
        "        drop(guard);\n{}        assert!(try_lock_store(&b).is_ok());\n",
        "        // why the next line is fine, at length\n".repeat(WINDOW + 4)
    );
    assert!(
        judge(&long_comment_between),
        "comment lines do not push a probe out of the window"
    );
}

/// THE TREE'S LOCK HELPERS ARE PROBES (round 3, 2026-09-27), both ways: each
/// one, sampled once after a bare release, is found — `checker_gate` was
/// invisible, so a sample-once through it passed — and each is cleared by the
/// same remedies as a raw `try_lock`.
#[test]
fn the_lock_helpers_are_probes_and_their_remedies_clear_them() {
    for probe in [
        "crate::checker_gate(&path, Duration::from_millis(1))",
        "aterm_update_core::FileLock::acquire(&path)",
        "ProcessLock::acquire(&path)",
        "FileLock::acquire_within(&path, Duration::ZERO)",
        "FileLock::lock_open(open())",
        "lock_briefly(&file)",
        "lock_and_verify_target(&baseline)",
        "lock_exclusive(&file)",
    ] {
        let once = format!("        drop(holder);\n        let gate = {probe};\n");
        assert!(judge(&once), "{probe}: sampled once after a release");
        let polled = format!(
            "        drop(holder);\n        let deadline = Instant::now() + TEN;\n        \
             let gate = loop {{ if let Ok(g) = {probe} {{ break g; }} }};\n"
        );
        assert!(!judge(&polled), "{probe}: a bounded poll clears it");
        let unlocked = format!(
            "        holder.unlock().unwrap();\n        drop(holder);\n        let gate = {probe};\n"
        );
        assert!(!judge(&unlocked), "{probe}: an explicit LOCK_UN clears it");
    }
}

/// THE EVIDENCE OF A DETERMINISTIC RELEASE IS THIS RELEASE'S (round 3,
/// 2026-09-27), both ways. It was any `unlock(` or `LOCK_UN` in the three
/// lines above the drop: an annotation whose `LOCK_UN` sat four lines up read
/// as none, another binding's `unlock()` read as this one's, and a comment
/// saying a listener has NO `LOCK_UN` read as a deterministic release.
#[test]
fn the_deterministic_release_is_read_from_its_own_statement() {
    let probe = "        assert!(FileLock::acquire_within(&path, Duration::ZERO).is_ok());\n";
    // The release's own annotation, however long: cleared.
    let annotated = format!(
        "        // Released by `LOCK_UN` (the guard's drop), which strips the lock\n\
         {}        drop(holder);\n{probe}",
        "        // from the open file description itself, every copy included.\n".repeat(5)
    );
    assert!(
        !judge(&annotated),
        "a LOCK_UN annotation directly above the release, six lines up, clears it"
    );
    let trailing =
        format!("        drop(held); // released by LOCK_UN (the guard's drop)\n{probe}");
    assert!(!judge(&trailing), "so does one on the release's own line");
    // A comment that is not the release's own: found.
    let detached =
        format!("        // The guard's drop is LOCK_UN.\n\n        drop(holder);\n{probe}");
    assert!(
        judge(&detached),
        "a comment a blank line away is not about this release"
    );
    let in_the_doc = format!(
        "    /// A listener has no `LOCK_UN`.\n    fn abandon() {{\n        drop(holder);\n{probe}    }}\n"
    );
    assert!(
        judge(&in_the_doc),
        "a function's doc comment is not the release's annotation"
    );
    // Code evidence binds to the released binding, and reaches up the item.
    let far_unlock = format!(
        "        holder.unlock().unwrap();\n{}        drop(holder);\n{probe}",
        "        let n = step();\n".repeat(6)
    );
    assert!(
        !judge(&far_unlock),
        "the binding's own unlock() seven lines up clears it"
    );
    let other_unlock = format!(
        "        lease.unlock().unwrap();\n        drop(lease);\n        drop(holder);\n{probe}"
    );
    assert!(
        judge(&other_unlock),
        "another binding's unlock() is not evidence for this release"
    );
    let other_item = format!(
        "    fn setup() {{\n        holder.unlock().unwrap();\n    }}\n    #[test]\n    fn t() {{\n        \
         drop(holder);\n{probe}    }}\n"
    );
    assert!(
        judge(&other_item),
        "an unlock() in another function is not this release's"
    );
    // …and the window after a release ends with its function, but not at a
    // statement's attribute.
    let next_test =
        format!("        drop(holder);\n    }}\n\n    #[test]\n    fn next() {{\n{probe}    }}\n");
    assert!(
        !judge(&next_test),
        "the next test's first probe is not this release's"
    );
    let cfg_statement = format!("        drop(holder);\n        #[cfg(unix)]\n{probe}");
    assert!(
        judge(&cfg_statement),
        "a probe behind `#[cfg(unix)]` is still this function's"
    );
}

/// THE EVIDENCE NAMES THE BINDING AS A WHOLE WORD, and a `LOCK_UN` is evidence
/// only where it names it too (round 3 review, 2026-09-27). The rule this
/// replaced read `unlock(` plus a SUBSTRING of the name, and any `LOCK_UN` at
/// all, across twelve code lines: a binding called `lock` is a substring of
/// every `unlock(`, so another binding's unlock cleared it, and another
/// descriptor's `flock(.., LOCK_UN)` cleared any release below it. Both
/// shapes below were found by the three-line rule before round 3 and cleared
/// by the round-3 rule; they are found again.
#[test]
fn another_bindings_unlock_or_lock_un_is_not_this_releases_evidence() {
    let three_between =
        "        let a = step();\n        let b = step();\n        let c = step();\n";
    let other_unlock = format!(
        "        other.unlock().unwrap();\n{three_between}        drop(lock);\n        \
         assert!(FileLock::acquire_within(&a, Duration::ZERO).is_ok());\n"
    );
    assert!(
        judge(&other_unlock),
        "`other.unlock()` does not name `lock`: `unlock` merely ends in it"
    );
    let other_fd = format!(
        "        unsafe {{ libc::flock(other_fd, libc::LOCK_UN) }};\n{three_between}        \
         drop(guard);\n        assert!(try_lock_store(&b).is_ok());\n"
    );
    assert!(
        judge(&other_fd),
        "a LOCK_UN on another descriptor is not this release's"
    );
    // The same two statements, naming the released binding: cleared.
    let own_unlock = format!(
        "        lock.unlock().unwrap();\n{three_between}        drop(lock);\n        \
         assert!(FileLock::acquire_within(&a, Duration::ZERO).is_ok());\n"
    );
    assert!(!judge(&own_unlock), "the binding's own unlock() clears it");
    let own_fd = format!(
        "        unsafe {{ libc::flock(guard.as_raw_fd(), libc::LOCK_UN) }};\n{three_between}        \
         drop(guard);\n        assert!(try_lock_store(&b).is_ok());\n"
    );
    assert!(
        !judge(&own_fd),
        "a LOCK_UN on the binding's own descriptor clears it"
    );
    // The word test itself, at its edges.
    assert!(names("lock.unlock()", "lock"));
    assert!(names("FileExt::unlock(&lock)", "lock"));
    assert!(!names("other.unlock()", "lock"));
    assert!(!names("let locked = relock(x);", "lock"));
    assert!(!names("anything", ""));
}
