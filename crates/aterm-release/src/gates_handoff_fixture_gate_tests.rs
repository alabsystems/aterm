// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::ledger::RunOut;
use std::sync::Mutex;

struct Scratch(aterm_tempfile::TempDir);

impl Scratch {
    fn new(label: &str) -> Self {
        Scratch(
            aterm_tempfile::Builder::new()
                .prefix(&format!("aterm-release-handoff-{label}-"))
                .tempdir()
                .expect("scratch repo"),
        )
    }
    fn write(&self, rel: &str, text: &str) {
        let path = self.0.path().join(rel);
        fs::create_dir_all(path.parent().expect("a parent")).expect("mkdir");
        fs::write(path, text).expect("write");
    }
    fn ledger(&self, versions: &[&str]) {
        let mut text = String::from("# aterm release ledger\n");
        for (i, v) in versions.iter().enumerate() {
            text.push_str(&format!("{} {v}\n", 1_790_000_000 + i));
        }
        self.write(crate::ledger::LEDGER_FILE, &text);
    }
    fn pins(&self, rows: &[(&str, &str)]) {
        self.pins_requiring(rows, "v0.97.0", &["history", "twelve-panes"]);
    }
    fn pins_requiring(&self, rows: &[(&str, &str)], from: &str, required: &[&str]) {
        let required: Vec<(&str, &str)> = required.iter().map(|desk| (from, *desk)).collect();
        self.pins_required_rows(rows, &required);
    }
    fn pins_required_rows(&self, rows: &[(&str, &str)], required: &[(&str, &str)]) {
        let mut text = String::from(
            "/// Every `(release, desk)` checked in — \"pinned\".\n\
             const PINNED_DESKS: &[(&str, &str)] = &[\n",
        );
        for (release, desk) in rows {
            text.push_str(&format!("    (\"{release}\", \"{desk}\"),\n"));
        }
        text.push_str("];\n\nconst REQUIRED_DESKS: &[(&str, &str)] = &[\n");
        for (from, desk) in required {
            text.push_str(&format!("    (\"{from}\", \"{desk}\"),\n"));
        }
        text.push_str("];\n\nconst OTHER: &[&str] = &[\"not\", \"a\", \"row\"];\n");
        self.write(HANDOFF_FIXTURE_GUARD, &text);
    }
    fn desk(&self, release: &str, desk: &str) {
        self.write(
            &format!("{HANDOFF_FIXTURE_ROOT}/v{release}/{desk}/parent.toml"),
            "producer_version = \"x\"\n",
        );
    }
}

struct Origin {
    tags: Vec<&'static str>,
    asked: Mutex<Vec<String>>,
}

impl Origin {
    fn with(tags: &[&'static str]) -> Self {
        Origin {
            tags: tags.to_vec(),
            asked: Mutex::new(Vec::new()),
        }
    }
}

impl GitRunner for Origin {
    fn git(&self, args: &[&str]) -> Result<RunOut> {
        let line = args.join(" ");
        self.asked.lock().expect("asked").push(line.clone());
        assert_eq!(
            line, "ls-remote --tags origin",
            "the gate asks one question"
        );
        let mut stdout = String::new();
        for tag in &self.tags {
            stdout.push_str(&format!("{:040x}\trefs/tags/{tag}\n", 1));
            stdout.push_str(&format!("{:040x}\trefs/tags/{tag}^{{}}\n", 2));
        }
        Ok(RunOut {
            status: 0,
            stdout: stdout.into_bytes(),
            stderr: vec![],
        })
    }
}

fn all_shipped(_: &str) -> bool {
    true
}

fn handoff_fixture_gate(
    git: &dyn GitRunner,
    repo: &Path,
    version: &str,
) -> Result<Option<HandoffFixtures>> {
    handoff_fixture_gate_in(
        &fixture_origin::FixtureOrigin(git),
        &GateTree::Dir(repo),
        version,
    )
}

fn refusal(scratch: &Scratch, origin: &Origin, version: &str) {
    handoff_fixture_gate(origin, scratch.0.path(), version)
        .expect_err("the cut must be refused before the claim");
}

#[test]
fn a_missing_release_directory_is_refused_naming_it() {
    let scratch = Scratch::new("missing");
    scratch.ledger(&["0.91.0", "0.92.0"]);
    scratch.desk("0.91.0", "history");
    scratch.pins(&[("v0.91.0", "history"), ("v0.92.0", "history")]);
    let origin = Origin::with(&["v0.91.0", "v0.92.0"]);
    refusal(&scratch, &origin, "0.93.0");
}

#[test]
fn a_directory_without_a_parent_toml_is_refused() {
    let scratch = Scratch::new("no-parent");
    scratch.ledger(&["0.91.0", "0.92.0"]);
    scratch.write(
        &format!("{HANDOFF_FIXTURE_ROOT}/v0.92.0/history/s0.meta.json"),
        "{}",
    );
    scratch.write(&format!("{HANDOFF_FIXTURE_ROOT}/v0.92.0/notes.txt"), "");
    scratch.pins(&[("v0.92.0", "history")]);
    let origin = Origin::with(&["v0.91.0", "v0.92.0"]);
    refusal(&scratch, &origin, "0.93.0");
}

#[test]
fn a_release_the_guard_does_not_pin_is_refused() {
    let scratch = Scratch::new("unpinned");
    scratch.ledger(&["0.91.0", "0.92.0"]);
    scratch.desk("0.92.0", "history");
    scratch.desk("0.92.0", "twelve-panes");
    scratch.pins(&[("v0.91.0", "history")]);
    let origin = Origin::with(&["v0.91.0", "v0.92.0"]);
    refusal(&scratch, &origin, "0.93.0");
}

#[test]
fn checked_in_and_pinned_passes() {
    let scratch = Scratch::new("present");
    scratch.ledger(&["0.91.0", "0.92.0"]);
    scratch.desk("0.92.0", "twelve-panes");
    scratch.desk("0.92.0", "history");
    scratch.pins(&[
        ("v0.91.0", "history"),
        ("v0.92.0", "history"),
        ("v0.92.0", "twelve-panes"),
    ]);
    let origin = Origin::with(&["v0.91.0", "v0.92.0"]);
    let found = handoff_fixture_gate(&origin, scratch.0.path(), "0.93.0")
        .expect("the predecessor's fixtures are checked in and pinned");
    assert_eq!(
        found,
        Some(HandoffFixtures {
            release: "0.92.0".to_string(),
            desks: vec!["history".to_string(), "twelve-panes".to_string()],
            pinned: vec!["history".to_string(), "twelve-panes".to_string()],
        })
    );
    assert_eq!(origin.asked.lock().expect("asked").len(), 1, "one listing");
}

#[test]
fn inclusive_release_candidates_keep_history_and_skip_short_labels() {
    let ledger = "1 0.61\n2 0.60.0\n3 0.62.0\n4 0.62.0\n5 1.20261109.2\n6 1.20261109.3\n";
    assert_eq!(
        release_candidates_through(ledger, "0.193.0").unwrap(),
        ["0.62.0", "0.60.0"]
    );
    assert_eq!(
        release_candidates_through(ledger, "1.20261109.2").unwrap(),
        ["1.20261109.2", "0.62.0", "0.60.0"]
    );
    let asked = std::cell::RefCell::new(Vec::new());
    assert_eq!(
        release_through(ledger, "1.20261109.2", &|candidate| {
            asked.borrow_mut().push(candidate.to_string());
            candidate == "0.62.0"
        })
        .unwrap(),
        Some("0.62.0".to_string())
    );
    assert_eq!(*asked.borrow(), ["1.20261109.2", "0.62.0"]);
    assert!(release_candidates_through(ledger, "0.193").is_err());
    assert!(release_candidates_through("not a ledger record\n", "0.193.0").is_err());
    // Required-desk floors remain strict; the repair belongs in the ledger
    // candidate reader, not in relaxed release/floor comparison.
    assert!(release_at_least("0.193.0", "0.61").is_err());
}

#[test]
fn a_recut_judges_the_release_before_the_version_it_recuts() {
    let once = "1 0.91.0\n2 0.92.0\n3 0.93.0\n";
    let twice = "1 0.91.0\n2 0.92.0\n3 0.93.0\n4 0.92.0\n5 0.93.0\n";
    for ledger in [once, twice] {
        assert_eq!(
            predecessor_release(ledger, "0.93.0", &all_shipped).expect("walks"),
            Some("0.92.0".to_string()),
            "{ledger}"
        );
    }
    assert_eq!(
        predecessor_release(once, "0.94.0", &all_shipped).expect("walks"),
        Some("0.93.0".to_string())
    );
    let scratch = Scratch::new("recut");
    scratch.ledger(&["0.91.0", "0.92.0", "0.93.0", "0.93.0"]);
    scratch.desk("0.92.0", "history");
    scratch.pins(&[("v0.92.0", "history")]);
    let origin = Origin::with(&["v0.91.0", "v0.92.0"]);
    let found = handoff_fixture_gate(&origin, scratch.0.path(), "0.93.0")
        .expect("a recut of 0.93.0 succeeds 0.92.0")
        .expect("a predecessor");
    assert_eq!(found.release, "0.92.0");
}

#[test]
fn a_claim_abandoned_and_skipped_is_passed_over() {
    let scratch = Scratch::new("abandoned");
    scratch.ledger(&["0.91.0", "0.92.0", "0.93.0"]);
    scratch.desk("0.92.0", "history");
    scratch.pins(&[("v0.92.0", "history")]);
    let origin = Origin::with(&["v0.91.0", "v0.92.0"]);
    let found = handoff_fixture_gate(&origin, scratch.0.path(), "0.94.0")
        .expect("the abandoned claim is not a release")
        .expect("a predecessor");
    assert_eq!(found.release, "0.92.0");
}

#[test]
fn an_origin_carrying_none_of_the_ledger_fails_closed() {
    let scratch = Scratch::new("foreign-origin");
    scratch.ledger(&["0.91.0", "0.92.0"]);
    scratch.desk("0.92.0", "history");
    scratch.pins(&[("v0.92.0", "history")]);
    let origin = Origin::with(&["v9.9.9"]);
    let refused = handoff_fixture_gate(&origin, scratch.0.path(), "0.93.0")
        .expect_err("the cut must be refused before the claim")
        .to_string();
    let ledger = std::fs::read_to_string(scratch.0.path().join(crate::ledger::LEDGER_FILE))
        .expect("the ledger");
    let unshipped = predecessor_release(&ledger, "0.93.0", &|_| false)
        .expect_err("no earlier version shipped")
        .to_string();
    assert_eq!(
        refused,
        format!("handoff-fixture gate: {unshipped}"),
        "the gate that asked names itself"
    );
}

#[test]
fn a_ledger_with_no_other_version_has_no_predecessor() {
    assert_eq!(
        predecessor_release("# seed\n1 0.1.0\n", "0.1.0", &all_shipped).expect("walks"),
        None
    );
}

#[test]
fn the_pinned_table_is_read_from_source_and_fails_closed() {
    let source = "const PINNED_DESKS: &[(&str, &str)] = &[\n\
                  \x20   // (\"v0.1.0\", \"commented-out\"),\n\
                  \x20   (\"v0.91.0\", \"history\"), // \"trailing\"\n\
                  \x20   (\n\
                  \x20       \"v0.92.0\",\n\
                  \x20       \"twelve-panes\",\n\
                  \x20   ),\n\
                  ];\n\
                  const NEXT: &[&str] = &[\"x\"];\n";
    assert_eq!(
        pinned_desks(source).expect("parses"),
        vec![
            ("v0.91.0".to_string(), "history".to_string()),
            ("v0.92.0".to_string(), "twelve-panes".to_string()),
        ]
    );
    for broken in [
        "const PINNED: &[(&str, &str)] = &[(\"v0.92.0\", \"history\")];",
        "const PINNED_DESKS: &[(&str, &str)] = &[(\"v0.92.0\", \"history\")",
        "const PINNED_DESKS: &[(&str, &str)] = &[(\"v0.92.0\")];",
    ] {
        pinned_desks(broken).expect_err("an unreadable table stops the cut");
    }
}

#[test]
fn a_predecessor_missing_a_required_desk_is_refused() {
    let required = ["history", "stalled-sequence", "twelve-panes"];
    let scratch = Scratch::new("required-missing");
    scratch.ledger(&["0.95.0", "0.97.0"]);
    scratch.desk("0.97.0", "history");
    scratch.desk("0.97.0", "twelve-panes");
    scratch.desk("0.97.0", "claude-code-1049");
    scratch.pins_requiring(
        &[
            ("v0.97.0", "claude-code-1049"),
            ("v0.97.0", "history"),
            ("v0.97.0", "twelve-panes"),
        ],
        "v0.97.0",
        &required,
    );
    let origin = Origin::with(&["v0.95.0", "v0.97.0"]);
    refusal(&scratch, &origin, "0.98.0");
    scratch.desk("0.97.0", "stalled-sequence");
    refusal(&scratch, &origin, "0.98.0");
    scratch.pins_requiring(
        &[
            ("v0.97.0", "claude-code-1049"),
            ("v0.97.0", "history"),
            ("v0.97.0", "stalled-sequence"),
            ("v0.97.0", "twelve-panes"),
        ],
        "v0.97.0",
        &required,
    );
    let found = handoff_fixture_gate(&origin, scratch.0.path(), "0.98.0")
        .expect("the whole set is checked in and pinned")
        .expect("a predecessor");
    assert_eq!(found.release, "0.97.0");
}

#[test]
fn a_release_before_the_floor_is_not_held_to_the_required_set() {
    let scratch = Scratch::new("required-floor");
    scratch.ledger(&["0.94.0", "0.95.0"]);
    scratch.desk("0.95.0", "history");
    scratch.pins_requiring(
        &[("v0.95.0", "history")],
        "v0.97.0",
        &["history", "stalled-sequence"],
    );
    let origin = Origin::with(&["v0.94.0", "v0.95.0"]);
    let found = handoff_fixture_gate(&origin, scratch.0.path(), "0.97.0")
        .expect("v0.95.0 predates the floor")
        .expect("a predecessor");
    assert_eq!(found.release, "0.95.0");
    assert!(release_at_least("0.100.0", "0.97.0").expect("orders"));
    assert!(!release_at_least("0.9.0", "0.97.0").expect("orders"));
    assert!(release_at_least("0.97", "0.97.0").is_err(), "not X.Y.Z");
}

#[test]
fn a_desk_added_for_a_later_release_asks_nothing_of_an_earlier_one() {
    let scratch = Scratch::new("required-per-desk");
    scratch.ledger(&["0.97.0", "0.98.0"]);
    scratch.desk("0.97.0", "history");
    scratch.desk("0.98.0", "history");
    let required = [("v0.97.0", "history"), ("v0.98.0", "split-sequences")];
    scratch.pins_required_rows(&[("v0.97.0", "history"), ("v0.98.0", "history")], &required);
    let origin = Origin::with(&["v0.97.0"]);
    let found = handoff_fixture_gate(&origin, scratch.0.path(), "0.98.0")
        .expect("v0.97.0 is not asked for a desk it can never have")
        .expect("a predecessor");
    assert_eq!(found.release, "0.97.0");
    let origin = Origin::with(&["v0.97.0", "v0.98.0"]);
    refusal(&scratch, &origin, "0.99.0");
    scratch.desk("0.98.0", "split-sequences");
    scratch.pins_required_rows(
        &[
            ("v0.97.0", "history"),
            ("v0.98.0", "history"),
            ("v0.98.0", "split-sequences"),
        ],
        &required,
    );
    let found = handoff_fixture_gate(&origin, scratch.0.path(), "0.99.0")
        .expect("the whole set from each floor")
        .expect("a predecessor");
    assert_eq!(found.release, "0.98.0");
}

#[test]
fn the_required_table_is_read_from_source_and_fails_closed() {
    let source = "const REQUIRED_DESKS: &[(&str, &str)] = &[\n\
                  \x20   (\"v0.97.0\", \"history\"),\n\
                  \x20   // (\"v0.97.0\", \"commented-out\"),\n\
                  \x20   (\"v0.98.0\", \"split-sequences\"), // trailing\n\
                  ];\n";
    assert_eq!(
        required_desks(source).expect("parses"),
        [
            RequiredDesk {
                from: "0.97.0".to_string(),
                desk: "history".to_string(),
            },
            RequiredDesk {
                from: "0.98.0".to_string(),
                desk: "split-sequences".to_string(),
            },
        ]
    );
    for broken in [
        "const REQUIRED: &[(&str, &str)] = &[(\"v0.97.0\", \"history\")];",
        "const REQUIRED_DESKS: &[(&str, &str)] = &[];",
        "const REQUIRED_DESKS: &[&str] = &[\"history\"];",
        "const REQUIRED_DESKS: &[(&str, &str)] = &[(\"0.97\", \"history\")];",
        "const REQUIRED_DESKS: &[(&str, &str)] = &[(\"history\", \"v0.97.0\")];",
    ] {
        required_desks(broken).expect_err("an unreadable set stops the cut");
    }
}

#[test]
fn tag_listing_folds_peeled_rows_and_ignores_other_refs() {
    let listing = "aaa\trefs/tags/v0.92.0\nbbb\trefs/tags/v0.92.0^{}\n\
                   ccc\trefs/tags/public/v0.92.0\nddd\trefs/heads/main\n";
    let tags = tag_commits(listing);
    assert_eq!(tags["v0.92.0"], "bbb", "the commit an annotated tag names");
    assert_eq!(tags["public/v0.92.0"], "ccc", "kept, but never `v…`");
    assert_eq!(tags.len(), 2, "{tags:?}");
}
