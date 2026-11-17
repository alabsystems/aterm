// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A readiness read reports the faults it folds into its answer ([`Judgement`]) and answers as it
//! did before it reported them.

use super::*;

/// The entries the fixture's vault holds, each read by name from the store.
const ENTRIES: [&str; 2] = ["credential", "inventory"];

/// A member of a signed vault at a path that holds [`ENTRIES`], applied.
fn fixture() -> (aterm_tempfile::TempDir, Paths) {
    let root = aterm_tempfile::tempdir().unwrap();
    let source = root.path().join("team.vault");
    let paths = Paths::for_home(&root.path().join("reader"));
    create(&paths, source.to_str().unwrap(), None, Some("reader")).unwrap();
    let opened = admin_open(&paths).unwrap();
    let mut document = opened.doc.clone();
    for name in ENTRIES {
        document
            .set_entry(&Audience::Everyone, name, b"fixture-first")
            .unwrap();
    }
    admin_publish(&paths, &opened, &document).unwrap();
    sync(&paths, None).unwrap();
    (root, paths)
}

/// The status read's answer (the path [`observe_materialization`] takes, its judgement kept) and
/// whether it folded a fault.
fn judged(paths: &Paths, configured: &Config) -> (String, bool) {
    let mut judgement = Judgement::default();
    let answer = match with_ready_state_reusing(
        paths,
        Some(configured),
        local_judged,
        None,
        None,
        OnWriter::Answer,
        &mut judgement,
        |_| Ok(()),
    ) {
        Ok(MaterializationObservation::Ready(_)) => "Ready".to_owned(),
        Ok(MaterializationObservation::Blocked(why)) => format!("{why:?}"),
        Err(_) => "Err".to_owned(),
    };
    (answer, judgement.faulted)
}

/// How many times one readiness read of the warm fixture opens each file under its home.
fn opens_of_one_read(paths: &Paths, configured: &Config) -> BTreeMap<PathBuf, usize> {
    judged(paths, configured);
    let before = fsx::reads_under(&paths.home);
    assert_eq!(judged(paths, configured), ("Ready".to_owned(), false));
    fsx::reads_under(&paths.home)
        .into_iter()
        .map(|(path, reads)| {
            let opens = reads - before.get(&path).copied().unwrap_or(0);
            (path, opens)
        })
        .filter(|(_, opens)| *opens > 0)
        .collect()
}

/// Each open of `path` that one read makes, failed in turn ([`fsx::fail_reads_after`]): the
/// answer and the judgement of the read it failed in, and whether the next read, unfaulted, read
/// the vault ready with no fault.
fn each_open_failed(
    paths: &Paths,
    configured: &Config,
    path: &Path,
    opens: usize,
) -> Vec<(usize, String, bool, bool)> {
    (0..opens)
        .map(|open| {
            fsx::fail_reads_after(path, open, 1);
            let (answer, faulted) = judged(paths, configured);
            fsx::fail_reads_after(path, 0, 0);
            let recovered = judged(paths, configured) == ("Ready".to_owned(), false);
            (open, answer, faulted, recovered)
        })
        .collect()
}

/// A read that fails on the held envelope or on the history claim is answered as main's
/// readiness read answers it, whose folds note nothing (the answers each file's failed opens give,
/// read by the same fixture and faults with `Judgement::ok` noting nothing), and the judgement
/// reports the fault wherever it moved the answer off Ready; unfaulted, it reports none, and the
/// next read is Ready again.
///
/// NEGATIVE CONTROL (2026-11-15, on the merge of main f581bd69910's vault that writes no file
/// under the home), run and then restored: with `Judgement::ok` noting nothing (`read.ok()`),
/// which leaves the read answering as main's does, every case answers the set it names and the
/// law fails only on the six opens it leaves unreported, from `.../reader/.config/aterm/secrets/
/// vault open 1: Revoked, unreported` to `.../records.frontiers open 2: ApplyIncomplete,
/// unreported` (the claim).
#[test]
fn a_fault_is_folded_as_before_and_reported() {
    let (_root, paths) = fixture();
    let configured = config(&paths).unwrap().unwrap();
    let opens = opens_of_one_read(&paths, &configured);
    let [claim, ..] = paths.acknowledged_history_files();
    let cases = [
        (
            paths.cache_file(),
            vec!["ApplyIncomplete", "Ready", "Revoked"],
        ),
        (claim, vec!["ApplyIncomplete", "Revoked"]),
    ];
    let mut wrong = Vec::new();
    for (path, main) in cases {
        let count = opens.get(&path).copied().unwrap_or(0);
        assert!(count > 0, "one read opens {}", path.display());
        let failed = each_open_failed(&paths, &configured, &path, count);
        let answers: BTreeSet<&str> = failed.iter().map(|(_, a, ..)| a.as_str()).collect();
        if answers != main.into_iter().collect() {
            wrong.push(format!("{}: answers {answers:?}", path.display()));
        }
        for (open, answer, faulted, recovered) in failed {
            if answer != "Ready" && !faulted {
                wrong.push(format!(
                    "{} open {open}: {answer}, unreported",
                    path.display()
                ));
            }
            if !recovered {
                wrong.push(format!("{} open {open}: the next read", path.display()));
            }
        }
    }
    assert!(wrong.is_empty(), "{wrong:#?}");
}

/// The opens of every file one readiness read of `paths` opens under its home, each failed in
/// turn, that moved the answer off Ready to a blocker unreported.
#[cfg(unix)]
fn unreported_faults(paths: &Paths) -> Vec<String> {
    let configured = config(paths).unwrap().unwrap();
    let opens = opens_of_one_read(paths, &configured);
    let mut unreported = Vec::new();
    for (path, count) in &opens {
        for (open, answer, faulted, _) in each_open_failed(paths, &configured, path, *count) {
            if answer != "Ready" && answer != "Err" && !faulted {
                unreported.push(format!("{} open {open}: {answer}", path.display()));
            }
        }
    }
    unreported
}

/// Every file one readiness read opens under the home, each open failed in turn, of a path
/// vault's member and of a member that holds its vault's membership log (`m7` of
/// [`super::log_entries_tests::two_macs`]): a failure that moves the answer off Ready to a blocker
/// is reported. (An `Err` answer is no fold: the read failed as a whole.)
///
/// NEGATIVE CONTROL (2026-11-14, run again 2026-11-15 on the merge of main f581bd69910's vault
/// that writes no file under the home), run and then restored: with the denial check folded
/// unnoted (`steward::local_recipient_denied_in_roster_held` in place of the judged call), the
/// failed first opens of `identity`, `records` and `records.frontiers` and the second of
/// `source.toml`, `state.toml` and `vault` read `Revoked`, unreported, on both members, and the
/// test names each of the twelve.
#[cfg(unix)]
#[test]
fn every_read_fault_that_moves_the_answer_is_reported() {
    let (_root, paths) = fixture();
    let entries = ENTRIES.map(|name| (Audience::Everyone, name, &b"fixture-first"[..]));
    let lab = super::log_entries_tests::two_macs(&entries);
    let mut unreported = unreported_faults(&paths);
    unreported.extend(unreported_faults(&lab.m7));
    assert!(unreported.is_empty(), "{unreported:#?}");
}
