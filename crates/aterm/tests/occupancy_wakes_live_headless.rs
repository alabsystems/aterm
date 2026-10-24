// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE BOOK OF DECLARED WAKES THROUGH THE ONE `aterm` BINARY against a real `aterm --headless`
//! (the occupancy slice-2 plan's 2.5): `wake`, `waits` and `await wake` are served on a session;
//! a process of the tab's own tree books a wake under the kernel's proof of its peer (and passes
//! the verb's op-class fence on `@self`), and a client outside the tree is refused and sees the
//! book redacted.

#![cfg(unix)]

use aterm_spec::wait::{MUST_HAPPEN, until_within};

use crate::{headless_boot, launch_isolation};

use headless_boot::{Instance, boot_session, ctl, ctl_ok, field, said, status, type_line};

fn boot() -> Instance {
    headless_boot::boot_with(
        "atwk",
        |_, cmd| {
            cmd.args(["--lines", "24", "--columns", "100"]);
        },
        |_| true,
    )
}

/// `aterm ctl <args…>` from outside the session's tree must be refused with one of `refusals`.
#[track_caller]
fn refused(inst: &Instance, args: &[&str], refusals: &[&str]) {
    let out = ctl(inst, args);
    let words = said(&out);
    assert!(
        !out.status.success() && refusals.iter().any(|refusal| words.contains(refusal)),
        "aterm ctl {args:?} must be refused {refusals:?}:\n{words}"
    );
}

/// The book's rows as a `waits` client printed them (the client prints a lines reply's rows).
fn rows(printed: &str) -> Vec<&str> {
    printed
        .lines()
        .filter(|line| !line.starts_with("OK "))
        .collect()
}

/// A WAKE OF THE TAB'S OWN TREE, AND NONE FROM OUTSIDE IT: the test's own client (no process of
/// the tab's) is refused `wake`, `await wake`, `wake rm` and `wake taken`; an `aterm ctl @self`
/// run as an agent runs it, under a `sh` of the tab's shell (the process the wake is declared
/// for, which `read` keeps alive past the book's tick), books one, and its `waits` shows the
/// label that the outside client's `waits` never sees.
#[test]
fn a_tabs_own_tree_books_a_wake_and_a_client_outside_it_is_refused() {
    const OTHER: &str = "ERR denied wake: another session's";
    // The host knows the tab's shell from the machine's snapshot: until one names it (a busy
    // machine's first can come late), an outside client is refused as of a tree not known yet.
    const UNKNOWN: &str = "ERR denied wake: not in this session's tree";
    let inst = boot();
    let (_, sid) = boot_session(&inst);
    let at = format!("@{sid}");

    refused(
        &inst,
        &[&at, "wake", "label=lunch", "when=in:1h"],
        &[OTHER, UNKNOWN],
    );
    refused(
        &inst,
        &[&at, "await", "wake", "timeout=1000"],
        &[OTHER, UNKNOWN],
    );
    assert!(
        rows(&ctl_ok(&inst, &[&at, "waits"])).is_empty(),
        "no book yet"
    );
    let record = status(&inst, &sid);
    assert_eq!(
        (field(&record, "waits"), field(&record, "waits_ready")),
        (Some("0"), Some("0")),
        "{record}"
    );

    let aterm = launch_isolation::aterm_executable();
    let aterm = aterm.display();
    let declared = inst.tmp.join("wake.out");
    let listed = inst.tmp.join("waits.out");
    type_line(
        &inst,
        &sid,
        &format!(
            "/bin/sh -c \"'{aterm}' ctl @self wake label=lunch when=in:1h \
             >'{}' 2>&1; '{aterm}' ctl @self waits >'{}.part' 2>&1; \
             mv '{}.part' '{}'; read -r x\"",
            declared.display(),
            listed.display(),
            listed.display(),
            listed.display(),
        ),
    );
    let inside = until_within(MUST_HAPPEN, "the tab's own shell lists its book", || {
        std::fs::read_to_string(&listed).ok()
    });
    assert_eq!(
        std::fs::read_to_string(&declared).expect("the declaration's reply"),
        "OK wake #1 await_only=1\n",
        "a process of the tab's own tree books a wake"
    );
    let inside_rows = rows(&inside);
    assert!(
        matches!(inside_rows.as_slice(), [row] if row.starts_with("#1 pending ") && row.ends_with(" label=lunch")),
        "inside the tree the book shows its label: {inside:?}"
    );

    let outside = ctl_ok(&inst, &[&at, "waits"]);
    assert!(
        matches!(rows(&outside).as_slice(), [row] if row.starts_with("#1 pending ") && !row.contains("label=")),
        "outside the tree the book is redacted: {outside:?}"
    );
    let record = status(&inst, &sid);
    assert_eq!(field(&record, "waits"), Some("1"), "{record}");
    // The tab's own shell booked one, so the snapshot names the tab: the refusal is exact.
    refused(
        &inst,
        &[&at, "wake", "label=dinner", "when=in:2h"],
        &[OTHER],
    );
    refused(&inst, &[&at, "await", "wake", "timeout=1000"], &[OTHER]);
    refused(&inst, &[&at, "wake", "rm", "#1"], &[OTHER]);
    refused(&inst, &[&at, "wake", "taken", "#1"], &[OTHER]);
    assert!(
        matches!(rows(&ctl_ok(&inst, &[&at, "waits"])).as_slice(), [row] if row.starts_with("#1 pending ")),
        "an outside rm or take leaves the wake"
    );
}
