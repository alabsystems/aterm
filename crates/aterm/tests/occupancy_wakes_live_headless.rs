// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The book of declared wakes through the one `aterm` binary against a real `aterm --headless`:
//! a process of the tab's own tree books a wake under the kernel's proof of its peer (passing the
//! verb's op-class fence on `@self`), and a client outside the tree is refused and sees the book
//! redacted.

#![cfg(target_os = "macos")]

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

/// `aterm ctl <args…>` from outside the session's tree must be refused as another session's; the
/// host knows the tab's shell from its spawn, so the refusal needs no machine snapshot.
#[track_caller]
fn refused(inst: &Instance, args: &[&str]) {
    const OTHER: &str = "ERR denied wake: another session's";
    let out = ctl(inst, args);
    let words = said(&out);
    assert!(
        !out.status.success() && words.contains(OTHER),
        "aterm ctl {args:?} must be refused {OTHER:?}:\n{words}"
    );
}

/// The book's rows as a `waits` client printed them (the client prints a lines reply's rows).
fn rows(printed: &str) -> Vec<&str> {
    printed
        .lines()
        .filter(|line| !line.starts_with("OK "))
        .collect()
}

/// An `aterm ctl @self` under a `sh` of the tab's shell books a wake and sees its label (`read`
/// keeps that `sh`, the process the wake is declared for, alive past the book's tick); the test's
/// own client, outside the tree, is refused `wake`, `await wake`, `wake rm` and `wake taken`, and
/// sees the book redacted.
#[test]
fn a_tabs_own_tree_books_a_wake_and_a_client_outside_it_is_refused() {
    let inst = boot();
    let (_, sid) = boot_session(&inst);
    let at = format!("@{sid}");

    refused(&inst, &[&at, "wake", "label=lunch", "when=in:1h"]);
    refused(&inst, &[&at, "await", "wake", "timeout=1000"]);
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
    // The instance serves: each request's accept waits the hang detector, not the 10 s cap.
    let accept = format!(
        "{}={}",
        aterm_ctl::ACCEPT_WITHIN_SEAM,
        MUST_HAPPEN.as_millis()
    );
    type_line(
        &inst,
        &sid,
        &format!(
            "/bin/sh -c \"{accept} '{aterm}' ctl @self wake label=lunch when=in:1h \
             >'{}' 2>&1; {accept} '{aterm}' ctl @self waits >'{}.part' 2>&1; \
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
    refused(&inst, &[&at, "wake", "label=dinner", "when=in:2h"]);
    refused(&inst, &[&at, "await", "wake", "timeout=1000"]);
    refused(&inst, &[&at, "wake", "rm", "#1"]);
    refused(&inst, &[&at, "wake", "taken", "#1"]);
    assert!(
        matches!(rows(&ctl_ok(&inst, &[&at, "waits"])).as_slice(), [row] if row.starts_with("#1 pending ")),
        "an outside rm or take leaves the wake"
    );
}
