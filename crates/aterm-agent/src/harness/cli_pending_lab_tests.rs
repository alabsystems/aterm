// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `accounts table`, `accounts list` and `accounts add` while the lab may name this Mac and is not
//! read yet (`alab_roster::while_pending`): each goes on with this Mac's own roster.

use super::*;
use crate::harness::alab_roster::while_pending;

/// The lines of `out` that say the lab is not read yet, whatever they start with.
fn pending_lines(out: &str) -> usize {
    out.lines()
        .filter(|line| line.contains("not read yet"))
        .count()
}

/// Whether `out` says the lab is not read yet on a `lab:` line.
fn says_pending(out: &str) -> bool {
    out.lines()
        .any(|line| line.starts_with("lab: ") && line.contains("not read yet"))
}

/// `ACCOUNTS TABLE` AND `ACCOUNTS LIST` WHILE THE LAB IS NOT READ YET LIST THE ROSTER, SAY WHY IN ONE
/// STATUS LINE AND EXIT 0, IN TEXT AND IN JSON (`lab_pending`): the list's `list:` origin line does
/// not say it again. CONTROL: with nothing pending the list says no word of the lab.
/// NEGATIVE CONTROL: with the table's pending arm removed, its source read refuses (exit 1); with
/// `write_combined_accounts_report`'s status line removed, the list says nothing of the lab; with
/// the `list:` line printing `FleetOrigin::AlabPending`'s words again, the list says it twice.
#[test]
fn table_and_list_go_on_with_the_roster_while_the_lab_is_unread() {
    let tmp = tmp_dir("pending-lab-table");
    let mut env = env_at(&tmp);
    env.sock = None;
    three_accounts(&tmp, &env);
    let control = run_line("accounts list", &env);
    assert_eq!(control.0, exit(0), "{control:?}");
    assert_eq!(pending_lines(&control.1), 0, "CONTROL: {}", control.1);
    for line in [
        "accounts table",
        "accounts table --cli claude",
        "accounts list",
    ] {
        let (code, out, err) = while_pending(|| run_line(line, &env));
        assert_eq!(code, exit(0), "{line}: {err}");
        for label in ["main", "work-2", "pool-a"] {
            assert!(out.contains(label), "{line}: {out}");
        }
        assert_eq!(pending_lines(&out), 1, "{line}: {out}");
        assert!(says_pending(&out), "{line}: {out}");
        let json = format!("{line} --json");
        let (code, out, err) = while_pending(|| run_line(&json, &env));
        assert_eq!(code, exit(0), "{json}: {err}");
        let doc: Value = aterm_json::from_str(out.trim()).expect(&json);
        assert!(
            doc["lab_pending"]
                .as_str()
                .is_some_and(|why| why.contains("not read yet")),
            "{json}: {out}"
        );
    }
}

/// `ACCOUNTS ADD <PROVIDER WORD> <ADDRESS>` WHILE THE LAB IS NOT READ YET LISTS A LOCAL ROW OF THAT
/// ACCOUNT, AND SAYS IN ONE `lab:` LINE THAT THE LAB IS NOT READ YET, SO THE ROW IS THIS ROSTER'S
/// ALONE MEANWHILE: no lab is asked for the account by name (`AutomaticSource::named_account`
/// answers none while pending, `accounts_source_pending`), and nothing refuses it.
/// NEGATIVE CONTROL: with the old pending refusal back in `chosen_captured`, the add fails (exit 1)
/// and lists nothing; with `run_accounts_add`'s status line removed, it says nothing of the lab.
#[cfg(unix)]
#[test]
fn accounts_add_lists_a_local_row_while_the_lab_is_unread() {
    let tmp = tmp_dir("pending-lab-add");
    let mut env = env_at(&tmp);
    env.sock = None;
    let (code, out, err) =
        while_pending(|| run_line("accounts add claude work-1@example.com", &env));
    assert_eq!(code, exit(0), "{out}{err}");
    assert_eq!(pending_lines(&out), 1, "{out}");
    assert!(says_pending(&out), "{out}");
    let roster = Roster::load(&env.accounts_roster().expect("a config dir")).expect("a roster");
    assert_eq!(
        roster
            .rows
            .iter()
            .filter(|row| row.matches_email("work-1@example.com"))
            .count(),
        1,
        "the local row: {:?}",
        roster.rows
    );
}
