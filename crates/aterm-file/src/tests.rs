// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use crate::{
    Column, Face, File, Frontier, Kept, Name, Opened, Pass, Projected, Recorded, Root, Row,
    Snapshot, Source, Why, Word,
    cli::{self, Known, Run},
    exit,
    file::cut,
};
use aterm_json::Value;
use std::collections::BTreeSet;

const KNOWN: &[Known] = &[
    Known {
        name: "ai/accounts",
        kind: "table",
        about: "accounts",
    },
    Known {
        name: "ai/links",
        kind: "table",
        about: "links",
    },
];

fn name(text: &str) -> Name {
    Name::parse(text).expect("a name")
}

fn column(name: &str, source: Source, refers: Option<&str>) -> Column {
    Column {
        name: name.to_owned(),
        source,
        read_only: source != Source::Schema,
        refers: refers.map(str::to_owned),
    }
}

fn row(pk: &str, cells: &[(&str, &str)]) -> Row {
    Row {
        pk: pk.to_owned(),
        live: true,
        cells: cells
            .iter()
            .map(|(name, text)| ((*name).to_owned(), (*text).to_owned()))
            .collect(),
        values: cells
            .iter()
            .map(|(name, text)| ((*name).to_owned(), Value::from(*text)))
            .collect(),
    }
}

fn accounts(rows: Vec<Row>) -> Snapshot {
    Snapshot {
        name: name("ai/accounts"),
        kind: "table".to_owned(),
        ext: "tsv".to_owned(),
        rev: 0,
        complete: true,
        at_ms: 1,
        columns: vec![
            column("realm", Source::Schema, None),
            column("note", Source::Schema, None),
            column("slug", Source::Derived, None),
            column("email", Source::Derived, None),
            column("pk", Source::Pk, None),
        ],
        handles: vec!["slug".to_owned(), "email".to_owned()],
        rows,
    }
}

fn ada() -> Row {
    row(
        "ac-0123456789abcdef",
        &[
            ("realm", "claude"),
            ("slug", "claude-fb-alpha"),
            ("email", "ada.fb@alpha.example"),
        ],
    )
}

fn ada_at_oai() -> Row {
    row(
        "ac-fedcba9876543210",
        &[
            ("realm", "codex"),
            ("slug", "oai-fb-alpha"),
            ("email", "ada.fb@alpha.example"),
        ],
    )
}

/// The replica every frontier of these tests reads, but the ones that name another.
const REPLICA: [u8; 32] = [7; 32];

fn frontier(era: u64, ids: &[u8]) -> Frontier {
    Frontier {
        source: REPLICA,
        era,
        generation: 2,
        ids: ids.iter().map(|id| [*id; 32]).collect::<BTreeSet<_>>(),
    }
}

fn opened(newest: bool, frontier: Frontier, snapshot: Option<Snapshot>) -> Pass {
    Pass::Opened(Box::new(Opened {
        newest,
        frontier,
        projection: 1,
        withheld: 0,
        projected: snapshot.map(|snapshot| Projected {
            snapshot,
            history: Vec::new(),
            baseline: None,
        }),
    }))
}

struct Scratch {
    _dir: aterm_tempfile::TempDir,
    root: Root,
}

fn scratch() -> Scratch {
    let dir = aterm_tempfile::tempdir().expect("a scratch folder");
    let root = Root::at(&dir.path().join("files"));
    Scratch { _dir: dir, root }
}

fn answer(root: &Root, words: &[&str]) -> cli::Answer {
    let words: Vec<String> = words.iter().map(|word| (*word).to_owned()).collect();
    match cli::run(root, KNOWN, &words) {
        Run::Answer(answer) => answer,
        Run::Watch(..) => panic!("{words:?} is no watch"),
    }
}

#[test]
fn a_name_is_a_set_and_a_leaf_in_the_grammar() {
    for good in ["ai/accounts", "lab/machines", "a/b", "a1-b/c-9"] {
        let parsed = name(good);
        assert_eq!(parsed.to_string(), good);
        assert!(crate::name::well_formed(good));
    }
    for bad in [
        "",
        "ai",
        "ai/",
        "/accounts",
        "ai/accounts/x",
        "Ai/accounts",
        "ai/Accounts",
        "1ai/accounts",
        "ai/-accounts",
        "ai/acc ounts",
        "ai/../x",
        "ai/a.b",
        "ai/abcdefghijklmnopqrstu",
        "abcdefghijklmnopqrstu/a",
    ] {
        assert!(Name::parse(bad).is_err(), "{bad:?}");
        assert!(!crate::name::well_formed(bad), "{bad:?}");
    }
    assert!(Name::parse("ai/abcdefghijklmnopqrst").is_ok());
}

#[test]
fn a_face_is_recognised_by_its_path_alone() {
    let root = Root::at(std::path::Path::new("/h/.aterm/files"));
    assert_eq!(
        root.name_of(std::path::Path::new("/h/.aterm/files/ai/accounts.tsv")),
        Some(name("ai/accounts"))
    );
    for other in [
        "/h/.aterm/files/ai/accounts",
        "/h/.aterm/files/accounts.tsv",
        "/h/.aterm/files/.ubd/state.noindex/ai/accounts.snap",
        "/h/.aterm/files/ai/x/accounts.tsv",
        "/h/elsewhere/ai/accounts.tsv",
    ] {
        assert_eq!(root.name_of(std::path::Path::new(other)), None, "{other}");
    }
    assert_eq!(
        Root::for_home(std::path::Path::new("/h")).snapshot(&name("ai/accounts")),
        std::path::Path::new("/h/.aterm/files/.ubd/state.noindex/ai/accounts.snap")
    );
}

#[test]
fn a_snapshot_round_trips_and_a_cut_or_changed_one_is_refused() {
    let mut snapshot = accounts(vec![
        ada(),
        row(
            "ac-1111111111111111",
            &[("note", "tab\there \"quoted\"\nline")],
        ),
    ]);
    snapshot.rev = 7;
    let bytes = snapshot.encode();
    assert_eq!(Snapshot::decode(&bytes).expect("decodes"), snapshot);

    // Cut anywhere, it is not a shorter table.
    for cut in [0, 1, bytes.len() / 2, bytes.len() - 1] {
        assert!(Snapshot::decode(&bytes[..cut]).is_err(), "cut at {cut}");
    }
    // Cut exactly after a row line, the row count and the end line still refuse it.
    let text = String::from_utf8(bytes.clone()).expect("text");
    let first_two: String = text.split_inclusive('\n').take(2).collect();
    assert!(Snapshot::decode(first_two.as_bytes()).is_err());
    // One changed byte is not the bytes written.
    let mut changed = bytes;
    let at = changed
        .iter()
        .position(|byte| *byte == b'f')
        .expect("a letter");
    changed[at] = b'g';
    assert!(Snapshot::decode(&changed).is_err());
}

#[test]
fn a_face_is_the_export_tsv_with_the_revisioned_key_last() {
    let mut snapshot = accounts(vec![
        ada(),
        row(
            "ac-1111111111111111",
            &[("note", "tab\there \"quoted\"\nline")],
        ),
    ]);
    snapshot.rev = 7;
    assert_eq!(
        snapshot.face(),
        "realm\tnote\t=slug\t=email\t=pk@7\n\
         claude\t\tclaude-fb-alpha\tada.fb@alpha.example\tac-0123456789abcdef\n\
         \t\"tab\there \"\"quoted\"\"\nline\"\t\t\tac-1111111111111111\n"
    );
}

#[test]
fn a_snapshot_is_replaced_only_by_a_frontier_that_contains_it_or_a_later_home() {
    let old = frontier(0, &[1, 2]);
    assert!(frontier(0, &[1, 2]).admits_over(&old));
    assert!(frontier(0, &[1, 2, 3]).admits_over(&old));
    assert!(
        !frontier(0, &[1]).admits_over(&old),
        "a view missing a record"
    );
    assert!(!frontier(0, &[1, 3]).admits_over(&old), "a diverged view");
    assert!(frontier(1, &[]).admits_over(&old), "a later home");
    assert!(!old.admits_over(&frontier(1, &[])), "an older home");
    let later_table = Frontier {
        generation: 3,
        ..frontier(0, &[])
    };
    assert!(later_table.admits_over(&old));
    assert!(!old.admits_over(&later_table));
    // Another replica's read is not ordered against this one's: it replaces it, either way.
    let elsewhere = Frontier {
        source: [8; 32],
        ..frontier(0, &[9])
    };
    assert!(elsewhere.admits_over(&frontier(3, &[1, 2])));
    assert!(frontier(3, &[1, 2]).admits_over(&elsewhere));
    assert_ne!(
        elsewhere.fingerprint(),
        frontier(0, &[9]).fingerprint(),
        "the replica is part of what a pass compares"
    );

    let round = Frontier::decode(&frontier(4, &[9, 7]).encode()).expect("decodes");
    assert_eq!(round, frontier(4, &[9, 7]));
    assert_ne!(round.fingerprint(), frontier(4, &[9]).fingerprint());
    assert!(Frontier::decode(b"aterm-file-frontier/1\nshort").is_none());
}

#[test]
fn a_read_before_any_pass_answers_never_and_empty() {
    let scratch = scratch();
    let file = File::at(&scratch.root, &name("ai/accounts"));
    let read = file.read();
    assert_eq!(read.status.word, Word::Never);
    assert_eq!(read.status.why, None);
    assert!(read.rows().is_empty() && read.columns().is_empty());
    assert_eq!(read.status.line(), "never");
    assert_eq!(file.face(), Face::Missing);
    // The read made nothing: not even the root.
    assert!(!scratch.root.path().exists());
}

#[test]
fn a_pass_records_its_word_and_a_refusal_leaves_the_snapshot_as_it_was() {
    let scratch = scratch();
    let file = File::at(&scratch.root, &name("ai/accounts"));

    // A refusal before anything was materialized: never, with the why.
    let look = file.look();
    let refused = Pass::Refused {
        word: Word::Held,
        why: Why::Membership,
    };
    assert_eq!(
        file.record(look, refused, 10).expect("records"),
        Recorded::Recorded { replaced: false }
    );
    let read = file.read();
    assert_eq!(
        (read.status.word, read.status.why),
        (Word::Never, Some(Why::Membership))
    );
    assert_eq!(read.status.line(), "never (membership)");

    // An open on the newest home: live, revision 1, the face written.
    let look = file.look();
    let pass = opened(true, frontier(0, &[1]), Some(accounts(vec![ada()])));
    assert_eq!(
        file.record(look, pass, 20).expect("records"),
        Recorded::Recorded { replaced: true }
    );
    let read = file.read();
    assert_eq!((read.status.word, read.status.why), (Word::Live, None));
    assert_eq!(read.status.rev(), 1);
    assert_eq!(read.status.line(), "live · rev 1 · 1 rows");
    assert_eq!(read.rows(), [ada()]);
    assert_eq!(file.face(), Face::Written);
    let face = scratch.root.face(&name("ai/accounts"), "tsv");
    let text = std::fs::read_to_string(&face).expect("the face");
    assert!(
        text.starts_with("realm\tnote\t=slug\t=email\t=pk@1\n"),
        "{text}"
    );
    let snapshot_bytes = std::fs::read(scratch.root.snapshot(&name("ai/accounts"))).expect("snap");

    // Each refusal after it: held with its why, the snapshot's bytes untouched.
    for why in [Why::Succession, Why::Membership, Why::Busy, Why::Keys] {
        let look = file.look();
        let refused = Pass::Refused {
            word: Word::Held,
            why,
        };
        file.record(look, refused, 30).expect("records");
        let read = file.read();
        assert_eq!((read.status.word, read.status.why), (Word::Held, Some(why)));
        assert_eq!(read.rows(), [ada()], "{why:?}");
        assert_eq!(read.status.rev(), 1);
        assert_eq!(
            std::fs::read(scratch.root.snapshot(&name("ai/accounts"))).expect("snap"),
            snapshot_bytes
        );
    }
    assert_eq!(file.read().status.line(), "held (keys) · rev 1 · 1 rows");

    // Policy turns it off: the word is off, the rows stay.
    let look = file.look();
    let off = Pass::Refused {
        word: Word::Off,
        why: Why::Policy,
    };
    file.record(look, off, 40).expect("records");
    let read = file.read();
    assert_eq!(
        (read.status.word, read.status.why),
        (Word::Off, Some(Why::Policy))
    );
    assert_eq!(read.rows(), [ada()]);

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        let mode = |path: &std::path::Path| {
            std::fs::metadata(path).expect("there").permissions().mode() & 0o777
        };
        assert_eq!(mode(&face), 0o600);
        assert_eq!(mode(&scratch.root.snapshot(&name("ai/accounts"))), 0o600);
        assert_eq!(mode(&scratch.root.face_dir(&name("ai/accounts"))), 0o700);
        assert_eq!(mode(&scratch.root.state_dir(&name("ai/accounts"))), 0o700);
        assert_eq!(mode(scratch.root.path()), 0o700);
    }
}

#[test]
fn off_before_anything_was_materialized_reads_off() {
    let scratch = scratch();
    let file = File::at(&scratch.root, &name("ai/accounts"));
    let off = Pass::Refused {
        word: Word::Off,
        why: Why::Policy,
    };
    file.record(0, off, 1).expect("records");
    let read = file.read();
    assert_eq!(
        (read.status.word, read.status.why),
        (Word::Off, Some(Why::Policy))
    );
    assert!(read.rows().is_empty());
}

#[test]
fn a_fenced_home_is_held_and_an_older_view_never_replaces_the_snapshot() {
    let scratch = scratch();
    let file = File::at(&scratch.root, &name("ai/accounts"));
    let both = accounts(vec![ada(), ada_at_oai()]);
    file.record(0, opened(true, frontier(1, &[1, 2]), Some(both)), 1)
        .expect("records");
    assert_eq!(file.read().status.rev(), 1);

    // The fenced original, an earlier home, read after the successor's: its word is recorded
    // (held, succession), its rows are not.
    let look = file.look();
    let older = opened(false, frontier(0, &[1, 2, 3]), Some(accounts(vec![ada()])));
    assert_eq!(
        file.record(look, older, 2).expect("records"),
        Recorded::Kept(Kept::Behind)
    );
    let read = file.read();
    assert_eq!(
        (read.status.word, read.status.why),
        (Word::Held, Some(Why::Succession))
    );
    assert_eq!(read.rows().len(), 2);
    assert_eq!(read.status.rev(), 1);
    assert_eq!(file.frontier(), Some(frontier(1, &[1, 2])));

    // The same home with a record missing: not replaced either, and not called live, since
    // the rows read are not that pass's.
    let look = file.look();
    let partial = opened(true, frontier(1, &[1]), Some(accounts(vec![ada()])));
    assert_eq!(
        file.record(look, partial, 3).expect("records"),
        Recorded::Kept(Kept::Behind)
    );
    let read = file.read();
    assert_eq!(read.rows().len(), 2);
    assert_eq!(
        (read.status.word, read.status.why),
        (Word::Held, Some(Why::Custody))
    );
    assert_eq!(
        read.status.line(),
        "held (custody) · rev 1 · 2 rows · last read not taken (behind)"
    );
    assert_eq!(file.frontier(), Some(frontier(1, &[1, 2])));

    // The same content at a grown frontier keeps the revision; new content moves it by one.
    let look = file.look();
    let same = opened(
        true,
        frontier(1, &[1, 2, 3]),
        Some(accounts(vec![ada(), ada_at_oai()])),
    );
    file.record(look, same, 4).expect("records");
    assert_eq!(file.read().status.rev(), 1);
    let look = file.look();
    let fewer = opened(
        true,
        frontier(1, &[1, 2, 3, 4]),
        Some(accounts(vec![ada()])),
    );
    file.record(look, fewer, 5).expect("records");
    let read = file.read();
    assert_eq!(read.status.rev(), 2);
    assert_eq!(read.rows(), [ada()]);
    assert_eq!(read.status.word, Word::Live);
}

#[test]
fn a_pass_that_another_pass_overtook_records_nothing() {
    let scratch = scratch();
    let file = File::at(&scratch.root, &name("ai/accounts"));
    file.record(
        0,
        opened(true, frontier(0, &[1]), Some(accounts(vec![ada()]))),
        1,
    )
    .expect("records");

    // A refresh notes the count and opens live custody; before it records, the home is fenced
    // and the fenced pass records held.
    let stale_look = file.look();
    let fenced = Pass::Refused {
        word: Word::Held,
        why: Why::Succession,
    };
    file.record(file.look(), fenced, 2).expect("records");
    let before = std::fs::read(scratch.root.state(&name("ai/accounts"))).expect("state");

    let late = opened(
        true,
        frontier(0, &[1, 2]),
        Some(accounts(vec![ada(), ada_at_oai()])),
    );
    assert_eq!(
        file.record(stale_look, late, 3).expect("no error"),
        Recorded::Raced
    );
    let read = file.read();
    assert_eq!(
        (read.status.word, read.status.why),
        (Word::Held, Some(Why::Succession)),
        "the late live view did not turn held into live"
    );
    assert_eq!(read.rows(), [ada()]);
    assert_eq!(
        std::fs::read(scratch.root.state(&name("ai/accounts"))).expect("state"),
        before
    );
}

#[test]
fn an_edited_face_is_left_alone_and_a_missing_one_is_written_again() {
    let scratch = scratch();
    let accounts_name = name("ai/accounts");
    let file = File::at(&scratch.root, &accounts_name);
    file.record(
        0,
        opened(true, frontier(0, &[1]), Some(accounts(vec![ada()]))),
        1,
    )
    .expect("records");
    let face = scratch.root.face(&accounts_name, "tsv");
    let written = std::fs::read_to_string(&face).expect("the face");

    std::fs::write(&face, "a person's bytes\n").expect("edit");
    assert_eq!(file.face(), Face::Edited);
    let look = file.look();
    let grown = opened(
        true,
        frontier(0, &[1, 2]),
        Some(accounts(vec![ada(), ada_at_oai()])),
    );
    file.record(look, grown, 2).expect("records");
    assert_eq!(
        std::fs::read_to_string(&face).expect("the face"),
        "a person's bytes\n"
    );
    assert_eq!(file.face(), Face::Edited);
    assert_eq!(file.read().rows().len(), 2, "programs read the snapshot");

    // Removed, the next pass writes it from the snapshot, even one that projected nothing.
    std::fs::remove_file(&face).expect("remove");
    assert_eq!(file.face(), Face::Missing);
    let look = file.look();
    file.record(look, opened(true, frontier(0, &[1, 2]), None), 3)
        .expect("records");
    assert_eq!(file.face(), Face::Written);
    let again = std::fs::read_to_string(&face).expect("the face");
    assert_ne!(again, written);
    assert!(
        again.starts_with("realm\tnote\t=slug\t=email\t=pk@2\n"),
        "{again}"
    );
    assert_eq!(again.lines().count(), 3);
}

#[test]
fn a_snapshot_that_cannot_be_read_answers_as_none_with_the_problem() {
    let scratch = scratch();
    let accounts_name = name("ai/accounts");
    let file = File::at(&scratch.root, &accounts_name);
    file.record(
        0,
        opened(true, frontier(0, &[1]), Some(accounts(vec![ada()]))),
        1,
    )
    .expect("records");
    let path = scratch.root.snapshot(&accounts_name);
    let bytes = std::fs::read(&path).expect("snap");
    std::fs::write(&path, &bytes[..bytes.len() / 2]).expect("cut");
    let read = file.read();
    assert_eq!(read.status.word, Word::Never);
    assert!(read.rows().is_empty());
    assert!(
        read.status
            .problem
            .as_deref()
            .is_some_and(|why| why.contains("snapshot not read")),
        "{:?}",
        read.status.problem
    );
    // The next pass that opens writes it again.
    let look = file.look();
    file.record(
        look,
        opened(true, frontier(0, &[1]), Some(accounts(vec![ada()]))),
        2,
    )
    .expect("records");
    assert_eq!(file.read().rows(), [ada()]);

    // A snapshot no pass vouches for is held, never live.
    std::fs::remove_file(scratch.root.state(&accounts_name)).expect("remove");
    let read = file.read();
    assert_eq!(
        (read.status.word, read.status.why),
        (Word::Held, Some(Why::Custody))
    );
    assert_eq!(read.rows(), [ada()]);
}

#[test]
fn the_read_verbs_answer_in_every_state() {
    let scratch = scratch();
    let root = &scratch.root;

    // Before any pass: every verb answers, the table empty, the word never.
    let ls = answer(root, &["ls"]);
    assert_eq!(ls.code, exit::OK);
    assert!(
        ls.out
            .contains("ai/accounts\ttable\tnever\tface=missing\taccounts"),
        "{}",
        ls.out
    );
    let cat = answer(root, &["cat", "ai/accounts"]);
    assert_eq!((cat.code, cat.out.as_str()), (exit::OK, ""));
    assert_eq!(cat.err, "ai/accounts: never · face=missing\n");
    let json: Value =
        aterm_json::from_str(&answer(root, &["cat", "ai/accounts", "--json"]).out).expect("json");
    assert_eq!(json["state"].as_str(), Some("never"));
    assert_eq!(json["rows"], Value::Array(Vec::new()));
    assert_eq!(
        answer(root, &["rows", "ai/accounts", "--where", "realm=claude"]).code,
        exit::OK
    );
    assert_eq!(answer(root, &["history", "ai/accounts"]).code, exit::OK);
    assert_eq!(
        answer(root, &["path", "ai/accounts"]).out,
        format!("{}\n", root.face(&name("ai/accounts"), "tsv").display())
    );

    let file = File::at(root, &name("ai/accounts"));
    let both = accounts(vec![ada(), ada_at_oai()]);
    file.record(0, opened(true, frontier(0, &[1]), Some(both)), 1)
        .expect("records");
    let held = Pass::Refused {
        word: Word::Held,
        why: Why::Succession,
    };
    file.record(file.look(), held, 2).expect("records");

    let cat = answer(root, &["cat", "ai/accounts", "--published"]);
    assert_eq!(cat.code, exit::OK);
    assert_eq!(cat.out, file.read().snapshot.expect("a snapshot").face());
    assert_eq!(
        cat.err,
        "ai/accounts: held (succession) · rev 1 · 2 rows · face=written\n"
    );
    let status: Value =
        aterm_json::from_str(&answer(root, &["status", "ai/accounts", "--json"]).out)
            .expect("json");
    assert_eq!(status["state"].as_str(), Some("held"));
    assert_eq!(status["why"].as_str(), Some("succession"));
    assert_eq!(status["rev"].as_u64(), Some(1));
    assert_eq!(status["face"].as_str(), Some("written"));

    // A row by its key, its slug, and its address where that names one row.
    for word in ["ac-0123456789abcdef", "claude-fb-alpha"] {
        let got = answer(root, &["get", "ai/accounts", word, "email"]);
        assert_eq!(
            (got.code, got.out.as_str()),
            (exit::OK, "ada.fb@alpha.example\n"),
            "{word}"
        );
    }
    let whole = answer(root, &["get", "ai/accounts", "oai-fb-alpha"]);
    assert_eq!(whole.code, exit::OK);
    assert!(
        whole.out.contains("realm\tcodex\n") && whole.out.contains("pk\tac-fedcba9876543210\n")
    );
    // Exact only: no prefix names a row, and an address two rows share lists both.
    let none = answer(root, &["get", "ai/accounts", "claude-fb"]);
    assert_eq!(none.code, exit::USAGE);
    let two = answer(root, &["get", "ai/accounts", "ada.fb@alpha.example"]);
    assert_eq!(two.code, exit::USAGE);
    assert!(
        two.err.contains("claude-fb-alpha") && two.err.contains("oai-fb-alpha"),
        "{}",
        two.err
    );
    assert_eq!(
        answer(root, &["get", "ai/accounts", "claude-fb-alpha", "nope"]).code,
        exit::USAGE
    );

    let rows = answer(root, &["rows", "ai/accounts", "--where", "realm=codex"]);
    assert_eq!(rows.code, exit::OK);
    assert_eq!(
        rows.out,
        "realm\tnote\tslug\temail\tpk\n\
         codex\t\toai-fb-alpha\tada.fb@alpha.example\tac-fedcba9876543210\n"
    );
    assert_eq!(
        answer(root, &["rows", "ai/accounts", "--where", "nope=1"]).code,
        exit::USAGE
    );

    assert_eq!(answer(root, &["cat", "ai/nope"]).code, exit::USAGE);
    assert_eq!(answer(root, &["cat", "Not A Name"]).code, exit::USAGE);
    assert_eq!(answer(root, &["frobnicate"]).code, exit::USAGE);
    assert_eq!(answer(root, &[]).out, cli::USAGE);
}

#[test]
fn rows_joins_a_column_to_the_file_it_names() {
    let scratch = scratch();
    let root = &scratch.root;
    File::at(root, &name("ai/accounts"))
        .record(
            0,
            opened(true, frontier(0, &[1]), Some(accounts(vec![ada()]))),
            1,
        )
        .expect("records");
    let links = Snapshot {
        name: name("ai/links"),
        columns: vec![
            column("machine", Source::Schema, None),
            column("account", Source::Schema, Some("ai/accounts")),
            column("pk", Source::Pk, None),
        ],
        handles: Vec::new(),
        rows: vec![
            row(
                "pf-1",
                &[("machine", "m2"), ("account", "ac-0123456789abcdef")],
            ),
            row("pf-2", &[("machine", "m3"), ("account", "ac-gone")]),
        ],
        ..accounts(Vec::new())
    };
    File::at(root, &name("ai/links"))
        .record(0, opened(true, frontier(0, &[1]), Some(links)), 1)
        .expect("records");

    let joined = answer(root, &["rows", "ai/links", "--join", "account"]);
    assert_eq!(joined.code, exit::OK);
    assert_eq!(
        joined.out,
        "machine\taccount\tpk\taccount.realm\taccount.note\taccount.slug\taccount.email\n\
         m2\tac-0123456789abcdef\tpf-1\tclaude\t\tclaude-fb-alpha\tada.fb@alpha.example\n\
         m3\tac-gone\tpf-2\t\t\t\t\n"
    );
    assert_eq!(
        answer(root, &["rows", "ai/links", "--join", "machine"]).code,
        exit::USAGE
    );
}

#[test]
fn watch_writes_a_line_for_each_recorded_change() {
    let scratch = scratch();
    let file = File::at(&scratch.root, &name("ai/accounts"));
    let words: Vec<String> = ["watch", "ai/accounts", "--changes", "2"]
        .iter()
        .map(|word| (*word).to_owned())
        .collect();
    let Run::Watch(watched, watch) = cli::run(&scratch.root, KNOWN, &words) else {
        panic!("a watch");
    };
    let mut lines = Vec::new();
    let mut step = 0;
    let code = cli::watch(
        &watched,
        watch,
        &mut |line| {
            lines.push(line.to_owned());
            true
        },
        &mut |_| {
            // Each look's pause is where a pass lands in this test: no clock is waited on.
            step += 1;
            let pass = match step {
                1 => opened(true, frontier(0, &[1]), Some(accounts(vec![ada()]))),
                // The same state again: no line.
                2 => opened(true, frontier(0, &[1]), None),
                _ => Pass::Refused {
                    word: Word::Held,
                    why: Why::Busy,
                },
            };
            file.record(file.look(), pass, step).expect("records");
        },
    );
    assert_eq!(code, exit::OK);
    assert_eq!(
        lines,
        [
            "ai/accounts: never · face=missing",
            "ai/accounts: live · rev 1 · 1 rows · face=written",
            "ai/accounts: held (busy) · rev 1 · 1 rows · face=written",
        ]
    );
}

/// A read of another replica (this Mac joined another lab, or enrolled again as a new Mac) is
/// not behind the snapshot, whatever its home, generation and records: it replaces it.
/// NEGATIVE CONTROL: with `admits_over` comparing two replicas' reads as one's, the other lab's
/// rows never arrive: `` left: Kept(Behind) right: Recorded { replaced: true } ``.
#[test]
fn another_replicas_read_replaces_the_snapshot_outright() {
    let scratch = scratch();
    let file = File::at(&scratch.root, &name("ai/accounts"));
    let both = accounts(vec![ada(), ada_at_oai()]);
    file.record(0, opened(true, frontier(2, &[1, 2, 3]), Some(both)), 1)
        .expect("records");

    let elsewhere = Frontier {
        source: [8; 32],
        ..frontier(0, &[9])
    };
    let look = file.look();
    assert_eq!(
        file.record(
            look,
            opened(true, elsewhere.clone(), Some(accounts(vec![ada_at_oai()]))),
            2
        )
        .expect("records"),
        Recorded::Recorded { replaced: true }
    );
    let read = file.read();
    assert_eq!((read.status.word, read.status.why), (Word::Live, None));
    assert_eq!(read.rows(), [ada_at_oai()]);
    assert_eq!(read.status.rev(), 2);
    assert_eq!(file.frontier(), Some(elsewhere));
}

/// What a recording writes, in order: the frontier, the snapshot, the history, the face, the
/// status.
const WRITES: u32 = 5;

/// A pass killed before any one of its writes leaves files the next pass takes up: a read
/// answers one pass's rows whole, the face a pass wrote is never taken for a person's edit, and
/// a read of the replica older than the one cut short does not replace what it wrote. The rows
/// a pass wrote before it was cut are never called `live` on the word of the pass before it,
/// which matters most where the pass cut short read a fenced home.
/// NEGATIVE CONTROLS: with the face judged by the status file's digest alone, the pass cut
/// before its status leaves `` 4 writes landed … left: Edited right: Written ``; with the
/// snapshot written before the frontier, a pass cut after one write has already replaced the
/// rows under the old frontier: `` 1 writes landed … left: 2 right: 1 ``; with `read` taking the
/// status's word for a snapshot at a revision the status does not record, the fenced pass cut
/// after its snapshot leaves its rows `live`:
/// `` 2 writes landed: the fenced home's rows are not live … left: (Live, None) ``.
#[test]
fn a_pass_cut_short_before_any_write_leaves_files_the_next_pass_takes_up() {
    let accounts_name = name("ai/accounts");
    let two = || accounts(vec![ada(), ada_at_oai()]);
    for landed in 0..WRITES {
        let scratch = scratch();
        let file = File::at(&scratch.root, &accounts_name);
        file.record(
            0,
            opened(true, frontier(0, &[1]), Some(accounts(vec![ada()]))),
            1,
        )
        .expect("records");

        cut::after(Some(landed));
        let cut_short = file.record(
            file.look(),
            opened(true, frontier(0, &[1, 2, 3]), Some(two())),
            2,
        );
        cut::after(None);
        assert!(cut_short.is_err(), "{landed} writes landed");

        // Whatever landed, the face is a pass's own and the read is one pass's rows.
        assert_eq!(file.face(), Face::Written, "{landed} writes landed");
        let rows = file.read().rows().len();
        assert_eq!(
            rows,
            if landed >= 2 { 2 } else { 1 },
            "{landed} writes landed"
        );
        assert!(file.read().status.problem.is_none());
        // The status still records the pass before: its `live` is the old rows', never the
        // ones the cut pass wrote.
        let status = file.read().status;
        assert_eq!(
            (status.word, status.why),
            if landed >= 2 {
                (Word::Held, Some(Why::Custody))
            } else {
                (Word::Live, None)
            },
            "{landed} writes landed"
        );

        if landed >= 1 {
            // The frontier of the read cut short is here: an older read is behind it.
            let stale = opened(
                true,
                frontier(0, &[1, 2]),
                Some(accounts(vec![ada_at_oai()])),
            );
            assert_eq!(
                file.record(file.look(), stale, 3).expect("records"),
                Recorded::Kept(Kept::Behind),
                "{landed} writes landed"
            );
            assert_eq!(file.read().rows().len(), rows);
        }

        // The next pass over the same replica finishes what was cut.
        let again = opened(true, frontier(0, &[1, 2, 3]), Some(two()));
        assert_eq!(
            file.record(file.look(), again, 4).expect("records"),
            Recorded::Recorded { replaced: true },
            "{landed} writes landed"
        );
        let read = file.read();
        assert_eq!((read.status.word, read.status.why), (Word::Live, None));
        assert_eq!(read.rows().len(), 2);
        assert_eq!(read.status.rev(), 2, "{landed} writes landed");
        assert_eq!(file.face(), Face::Written);
        assert_eq!(
            std::fs::read_to_string(scratch.root.face(&accounts_name, "tsv")).expect("the face"),
            read.snapshot.expect("a snapshot").face()
        );
    }

    // The first pass there ever was, cut before its status: the face is still a pass's own.
    for landed in 0..WRITES {
        let scratch = scratch();
        let file = File::at(&scratch.root, &accounts_name);
        cut::after(Some(landed));
        let cut_short = file.record(0, opened(true, frontier(0, &[1]), Some(two())), 1);
        cut::after(None);
        assert!(cut_short.is_err());
        assert_ne!(file.face(), Face::Edited, "{landed} writes landed");
        assert_eq!(
            file.record(0, opened(true, frontier(0, &[1]), Some(two())), 2)
                .expect("records"),
            Recorded::Recorded { replaced: true }
        );
        assert_eq!(file.face(), Face::Written, "{landed} writes landed");
        assert_eq!(file.read().status.line(), "live · rev 1 · 2 rows");
    }

    // A fenced home's pass cut short over a `live` status: once its rows are the ones a read
    // answers, the word is not `live`, though the status file still says the pass before was.
    for landed in 0..WRITES {
        let scratch = scratch();
        let file = File::at(&scratch.root, &accounts_name);
        file.record(
            0,
            opened(true, frontier(0, &[1]), Some(accounts(vec![ada()]))),
            1,
        )
        .expect("records");
        let fenced = || opened(false, frontier(0, &[1, 2, 3]), Some(two()));

        cut::after(Some(landed));
        let cut_short = file.record(file.look(), fenced(), 2);
        cut::after(None);
        assert!(cut_short.is_err(), "{landed} writes landed");
        let read = file.read();
        assert_eq!(
            file.state().and_then(|state| state.word),
            Some(Word::Live),
            "the status is the live pass's still"
        );
        if landed >= 2 {
            assert_eq!(read.rows(), [ada(), ada_at_oai()], "{landed} writes landed");
            assert_eq!(
                (read.status.word, read.status.why),
                (Word::Held, Some(Why::Custody)),
                "{landed} writes landed: the fenced home's rows are not live"
            );
        } else {
            assert_eq!(read.rows(), [ada()], "{landed} writes landed");
            assert_eq!((read.status.word, read.status.why), (Word::Live, None));
        }

        // The fenced pass again records its own word.
        assert_eq!(
            file.record(file.look(), fenced(), 3).expect("records"),
            Recorded::Recorded { replaced: true },
            "{landed} writes landed"
        );
        let read = file.read();
        assert_eq!(
            (read.status.word, read.status.why),
            (Word::Held, Some(Why::Succession))
        );
        assert_eq!((read.status.rev(), read.rows().len()), (2, 2));
    }
}

/// The monotone rule fails closed. A snapshot whose frontier file is missing, or holds bytes
/// that are no frontier, is not replaced by a fenced home's read, which cannot be shown to be no
/// older than it; the newest home's read replaces it and writes the frontier again. A frontier
/// too large for the next pass to read back is not written, nor the snapshot with it: the pass
/// after would compare its read with nothing.
/// NEGATIVE CONTROLS: with `record` admitting any read over a frontier it cannot read, the
/// fenced original's rows replace the successor's:
/// `` left: Recorded { replaced: true } right: Kept(Behind) ``; with the frontier written
/// whatever its size, `` left: Recorded { replaced: true } right: Kept(TooLarge) ``.
#[test]
fn a_fenced_homes_read_never_replaces_a_snapshot_whose_frontier_is_not_here() {
    let scratch = scratch();
    let accounts_name = name("ai/accounts");
    let file = File::at(&scratch.root, &accounts_name);
    let sidecar = scratch.root.frontier(&accounts_name);
    let both = || accounts(vec![ada(), ada_at_oai()]);
    file.record(0, opened(true, frontier(1, &[1, 2]), Some(both())), 1)
        .expect("records");

    // The fenced original's read, an earlier home, with the successor's frontier removed and
    // then damaged: behind either way.
    let garbage: &[u8] = b"aterm-file-frontier/0\n";
    for damage in [None, Some(garbage)] {
        match damage {
            None => std::fs::remove_file(&sidecar).expect("remove"),
            Some(bytes) => std::fs::write(&sidecar, bytes).expect("damage"),
        }
        assert_eq!(file.frontier(), None);
        let older = opened(false, frontier(0, &[1, 2, 3]), Some(accounts(vec![ada()])));
        assert_eq!(
            file.record(file.look(), older, 2).expect("records"),
            Recorded::Kept(Kept::Behind),
            "{damage:?}"
        );
        let read = file.read();
        assert_eq!(read.rows(), [ada(), ada_at_oai()], "{damage:?}");
        assert_eq!(
            (read.status.word, read.status.why),
            (Word::Held, Some(Why::Succession))
        );
        assert_eq!(read.status.rev(), 1);
        assert_eq!(file.frontier(), None, "a read not taken writes no frontier");
    }

    // The newest home's read replaces it, and its frontier is here again.
    let newest = opened(true, frontier(1, &[1, 2, 3]), Some(accounts(vec![ada()])));
    assert_eq!(
        file.record(file.look(), newest, 3).expect("records"),
        Recorded::Recorded { replaced: true }
    );
    let read = file.read();
    assert_eq!((read.status.word, read.status.why), (Word::Live, None));
    assert_eq!((read.status.rev(), read.rows()), (2, &[ada()][..]));
    assert_eq!(file.frontier(), Some(frontier(1, &[1, 2, 3])));

    // The largest frontier a pass reads back is taken; one record more is not written.
    let records = |count: usize| Frontier {
        ids: (0..count as u64)
            .map(|at| {
                let mut id = [0xff; 32];
                id[..8].copy_from_slice(&at.to_be_bytes());
                id
            })
            .chain(frontier(1, &[1, 2, 3]).ids)
            .collect(),
        ..frontier(1, &[])
    };
    let limit = usize::try_from(crate::snapshot::MAX_BYTES).expect("fits");
    let fits = (limit - frontier(1, &[1, 2, 3]).encode().len()) / 32;
    assert_eq!(
        file.record(file.look(), opened(true, records(fits), Some(both())), 4)
            .expect("records"),
        Recorded::Recorded { replaced: true }
    );
    assert_eq!(
        file.frontier().map(|frontier| frontier.ids.len()),
        Some(fits + 3)
    );
    assert_eq!(
        file.record(
            file.look(),
            opened(true, records(fits + 1), Some(accounts(vec![ada()]))),
            5
        )
        .expect("records"),
        Recorded::Kept(Kept::TooLarge)
    );
    let read = file.read();
    assert_eq!(read.rows(), [ada(), ada_at_oai()]);
    assert_eq!(
        (read.status.word, read.status.why, read.status.rev()),
        (Word::Held, Some(Why::Custody), 3)
    );
    assert_eq!(
        file.frontier().map(|frontier| frontier.ids.len()),
        Some(fits + 3),
        "the frontier the next pass compares with still reads"
    );
}

/// A projection whose snapshot a read would refuse for its size is not taken: the last snapshot
/// a read takes stays, at its revision, and the word is not `live`. A face too large for the
/// grid is not written, and the one a pass wrote before it is removed rather than left to be
/// taken for an edit once the table is small again.
/// NEGATIVE CONTROLS: with `record` writing whatever it encoded, the oversized pass is taken:
/// `` left: Recorded { replaced: true } right: Kept(TooLarge) ``; with the face a pass wrote
/// left on disk, `` the smaller table's face is not left behind ``.
#[test]
fn a_projection_too_large_to_read_is_not_taken_and_a_face_too_large_is_removed() {
    let scratch = scratch();
    let accounts_name = name("ai/accounts");
    let file = File::at(&scratch.root, &accounts_name);
    let face = scratch.root.face(&accounts_name, "tsv");
    file.record(
        0,
        opened(true, frontier(0, &[1]), Some(accounts(vec![ada()]))),
        1,
    )
    .expect("records");
    assert_eq!(file.face(), Face::Written);

    // A face over the grid's limit in a snapshot a read still takes.
    let wide = "n".repeat(usize::try_from(crate::MAX_FACE_BYTES).expect("fits") + 1);
    let large = accounts(vec![row("ac-1111111111111111", &[("note", &wide)])]);
    assert_eq!(
        file.record(
            file.look(),
            opened(true, frontier(0, &[1, 2]), Some(large)),
            2
        )
        .expect("records"),
        Recorded::Recorded { replaced: true }
    );
    assert_eq!(file.read().status.rev(), 2);
    assert!(
        !face.exists(),
        "the smaller table's face is not left behind"
    );
    assert_eq!(file.face(), Face::Missing);

    // A snapshot over what a read takes: not written at all.
    let wider = "n".repeat(usize::try_from(crate::snapshot::MAX_BYTES / 2).expect("fits") + 1);
    let huge = accounts(vec![row("ac-1111111111111111", &[("note", &wider)])]);
    assert_eq!(
        file.record(
            file.look(),
            opened(true, frontier(0, &[1, 2, 3]), Some(huge)),
            3
        )
        .expect("records"),
        Recorded::Kept(Kept::TooLarge)
    );
    let read = file.read();
    assert!(read.status.problem.is_none(), "{:?}", read.status.problem);
    assert_eq!(
        read.status.rev(),
        2,
        "no revision for a snapshot not written"
    );
    assert_eq!(read.rows().len(), 1);
    assert_eq!(read.rows()[0].cell("note").len(), wide.len());
    assert_eq!(
        (read.status.word, read.status.why),
        (Word::Held, Some(Why::Custody))
    );
    assert_eq!(
        read.status.line(),
        "held (custody) · rev 2 · 1 rows · last read not taken (too-large)"
    );
    assert_eq!(file.frontier(), Some(frontier(0, &[1, 2])));

    // The table small again: its snapshot is taken, and its face is written.
    assert_eq!(
        file.record(
            file.look(),
            opened(
                true,
                frontier(0, &[1, 2, 3, 4]),
                Some(accounts(vec![ada()]))
            ),
            4
        )
        .expect("records"),
        Recorded::Recorded { replaced: true }
    );
    assert_eq!(file.read().status.line(), "live · rev 3 · 1 rows");
    assert_eq!(file.face(), Face::Written);
}

/// `watch --timeout` takes any count of seconds: one past what the clock can add is no timeout,
/// not a panic.
#[test]
fn a_watch_timeout_past_the_clock_is_no_timeout() {
    let scratch = scratch();
    let words: Vec<String> = [
        "watch",
        "ai/accounts",
        "--timeout",
        "18446744073709551615",
        "--changes",
        "0",
    ]
    .iter()
    .map(|word| (*word).to_owned())
    .collect();
    let Run::Watch(watched, watch) = cli::run(&scratch.root, KNOWN, &words) else {
        panic!("a watch");
    };
    assert_eq!(
        watch.timeout,
        Some(std::time::Duration::from_secs(u64::MAX))
    );
    let mut lines = 0;
    let code = cli::watch(
        &watched,
        watch,
        &mut |_| {
            lines += 1;
            true
        },
        &mut |_| panic!("no change is waited for"),
    );
    assert_eq!((code, lines), (exit::OK, 1));
}

fn edit(key: &str, column: &str, text: &str, expected: &[&str]) -> crate::Edit {
    crate::Edit {
        key: key.to_owned(),
        column: column.to_owned(),
        elem: None,
        value: Value::from(text),
        text: text.to_owned(),
        expected: expected.iter().map(|entry| (*entry).to_owned()).collect(),
    }
}

fn draft(edits: Vec<crate::Edit>) -> crate::Draft {
    crate::Draft {
        born: false,
        revert: None,
        edits,
    }
}

fn person() -> crate::OwnerAct {
    crate::OwnerAct::verb("person", Some("s-1")).expect("an owner act")
}

/// An operation digest as an intent names one.
fn op(byte: u8) -> String {
    aterm_codec::hex::encode_lower(&[byte; 32])
}

/// A marker of the publisher's: `op` signed in the table generation these tests' frontiers read
/// ([`frontier`]), leaving nothing said.
fn mark(op: Option<String>) -> crate::Mark {
    crate::Mark {
        op,
        generation: 2,
        left: std::collections::BTreeMap::new(),
    }
}

/// An accepted write is one durable line of the outbox, read back whole; a read lays it over
/// the snapshot while `--published` and the status's word do not; and a draft that is no
/// intent is refused with nothing queued.
#[test]
fn a_write_is_queued_durably_and_read_back_over_the_snapshot() {
    let scratch = scratch();
    let file = File::at(&scratch.root, &name("ai/accounts"));
    // Before anything is materialized: the table is this Mac's own writes.
    let first = file
        .write(
            &person(),
            draft(vec![edit("ac1", "label", "desk", &[])]),
            10,
        )
        .expect("accepted");
    assert!(crate::outbox::is_id(&first.id));
    assert_eq!(
        (first.by.as_str(), first.session.as_deref()),
        ("person", Some("s-1"))
    );
    let read = file.read();
    assert_eq!(read.status.word, Word::Never);
    assert_eq!((read.status.queued, read.status.held), (1, 0));
    assert_eq!(read.rows().len(), 1);
    assert_eq!(read.rows()[0].cell("label"), "desk");
    assert!(file.published().snapshot.is_none());

    file.record(
        file.look(),
        opened(true, frontier(0, &[1]), Some(accounts(vec![ada()]))),
        20,
    )
    .expect("recorded");
    let second = file
        .write(
            &person(),
            draft(vec![edit(
                "ac1",
                "label",
                "lab",
                &[&format!("intent:{}", first.id)],
            )]),
            30,
        )
        .expect("accepted");
    let outbox = file.outbox();
    assert_eq!(
        outbox
            .queued
            .iter()
            .map(|i| i.id.as_str())
            .collect::<Vec<_>>(),
        [first.id.as_str(), second.id.as_str()]
    );
    assert_eq!(
        outbox.queued[1], second.intent,
        "an intent reads back as it was accepted"
    );
    assert!(outbox.problem.is_none());
    // The later write of the cell is the one a read shows; the snapshot is as the pass left it.
    let row = |read: &crate::Read| {
        read.rows()
            .iter()
            .find(|row| row.pk == "ac1")
            .map(|row| row.cell("label").to_owned())
    };
    assert_eq!(row(&file.read()).as_deref(), Some("lab"));
    assert_eq!(row(&file.published()), None);
    assert_eq!(file.read().status.word, Word::Live);
    let cat = answer(&scratch.root, &["cat", "ai/accounts"]);
    let published = answer(&scratch.root, &["cat", "ai/accounts", "--published"]);
    assert!(cat.out.contains("lab") && !published.out.contains("lab"));
    let listed = answer(&scratch.root, &["outbox", "ai/accounts"]);
    assert_eq!(listed.code, exit::OK);
    assert!(listed.out.contains(&first.id) && listed.out.contains("ac1.label"));
    assert!(!listed.out.contains("desk"), "a listing shows no value");

    // Not an intent: nothing is queued.
    for bad in [
        draft(Vec::new()),
        draft(vec![edit("", "label", "x", &[])]),
        draft(vec![edit("ac1.@self", "label", "x", &[])]),
        draft(vec![edit("ac1", "label", "x", &["not a digest"])]),
        crate::Draft {
            born: true,
            revert: None,
            edits: vec![edit("ac1", "label", "x", &[])],
        },
        crate::Draft {
            born: false,
            revert: Some("zz".to_owned()),
            edits: Vec::new(),
        },
    ] {
        assert!(matches!(
            file.write(&person(), bad, 40),
            Err(crate::Refused::Malformed(_))
        ));
    }
    assert!(crate::OwnerAct::verb("Not A Word", None).is_none());
    assert_eq!(file.outbox().queued.len(), 2);
}

/// The outbox is bounded: at 1,024 intents a new owner write is refused, nothing queued is
/// dropped to make room, and a held intent still counts. A Mac's own statements coalesce to
/// the latest per cell instead.
#[test]
fn a_full_outbox_refuses_the_new_write_and_mine_intents_coalesce() {
    let scratch = scratch();
    let file = File::at(&scratch.root, &name("ai/accounts"));
    // The bound, reached by one write of the file as a pass killed mid-queue would leave it.
    let first = file
        .write(&person(), draft(vec![edit("ac0", "label", "v", &[])]), 1)
        .expect("accepted");
    let line = std::fs::read_to_string(scratch.root.outbox(file.name())).expect("the outbox");
    let mut text = String::new();
    for n in 0..crate::outbox::MAX_INTENTS {
        text.push_str(&line.replace(&first.id, &format!("{n:032x}")));
    }
    std::fs::write(scratch.root.outbox(file.name()), &text).expect("written");
    assert_eq!(file.outbox().queued.len(), crate::outbox::MAX_INTENTS);
    let refused = file.write(&person(), draft(vec![edit("ac9", "label", "v", &[])]), 2);
    assert!(
        matches!(refused, Err(crate::Refused::Full { intents, .. }) if intents == crate::outbox::MAX_INTENTS),
        "{refused:?}"
    );
    assert_eq!(
        std::fs::read_to_string(scratch.root.outbox(file.name())).expect("the outbox"),
        text,
        "a refused write changes nothing"
    );
    // A held intent is out of the outbox and still counts against the bound.
    assert!(file.hold(&format!("{:032x}", 0), "moved", 3).expect("held"));
    assert_eq!(file.outbox().queued.len(), crate::outbox::MAX_INTENTS - 1);
    assert!(matches!(
        file.write(&person(), draft(vec![edit("ac9", "label", "v", &[])]), 4),
        Err(crate::Refused::Full { .. })
    ));
    // A person's drop makes room.
    assert_eq!(
        file.drop_intent(&format!("{:032x}", 0)).expect("dropped"),
        crate::Dropped::Dropped
    );
    file.write(&person(), draft(vec![edit("ac9", "label", "v", &[])]), 5)
        .expect("accepted once there is room");

    // Mine: the latest statement of a cell replaces the earlier one; another cell's stays.
    let links = File::at(&scratch.root, &name("ai/links"));
    let mine = |column: &str, text: &str, ms: u64| {
        links
            .state_mine(
                "harness",
                draft(vec![edit("claude.@self", column, text, &[])]),
                ms,
            )
            .expect("accepted")
    };
    mine("state", "one", 1);
    mine("seen", "then", 2);
    let last = mine("state", "two", 3);
    let queued = links.outbox().queued;
    assert_eq!(queued.len(), 2);
    assert_eq!(queued[0].edits[0].column, "seen");
    assert_eq!(queued[1], last.intent);
    assert!(queued.iter().all(crate::Intent::mine));
    // An owner's write is never coalesced away, and a mine statement is never an owner's.
    assert!(matches!(
        links.state_mine("harness", draft(vec![edit("ac1", "state", "x", &[])]), 4),
        Err(crate::Refused::Malformed(_))
    ));
}

/// The marker is written before the intent leaves the outbox, and only a marked or held intent
/// leaves it; a held intent is never applied, shows `⚠ mine ‖ theirs`, and goes only by a
/// person's word; a published intent stays laid over the snapshot until a pass projects its
/// operation.
#[test]
fn an_intent_leaves_the_outbox_only_marked_or_held_and_a_held_one_is_shown_not_applied() {
    let scratch = scratch();
    let file = File::at(&scratch.root, &name("ai/accounts"));
    file.record(
        file.look(),
        opened(true, frontier(0, &[1]), Some(accounts(vec![ada()]))),
        10,
    )
    .expect("recorded");
    let pk = ada().pk;
    let theirs = ada().cell("label").to_owned();
    let intent = file
        .write(
            &person(),
            draft(vec![edit(&pk, "label", "mine", &[&op(1)])]),
            20,
        )
        .expect("accepted");
    let shown = |file: &File| {
        file.read()
            .rows()
            .iter()
            .find(|row| row.pk == pk)
            .map(|row| row.cell("label").to_owned())
            .unwrap_or_default()
    };
    // Neither marked nor held: it stays.
    assert!(!file.dequeue(&intent.id).expect("read"));
    assert_eq!(file.outbox().queued.len(), 1);
    // Marked, and the publisher killed before the dequeue: still queued, no longer waiting.
    assert!(
        file.mark_done(&intent.id, &mark(Some(op(2))), 30)
            .expect("marked")
    );
    let outbox = file.outbox();
    assert_eq!((outbox.queued.len(), outbox.waiting().count()), (1, 0));
    assert!(file.dequeue(&intent.id).expect("dequeued"));
    assert!(file.outbox().queued.is_empty());
    // Published, and the snapshot does not hold the operation yet: the read still says it.
    assert_eq!(shown(&file), "mine");
    file.settle_done().expect("settled");
    assert!(!file.outbox().done[0].seen);
    file.record(
        file.look(),
        opened(true, frontier(0, &[1, 2]), Some(accounts(vec![ada()]))),
        40,
    )
    .expect("recorded");
    // The pass projected the operation: the snapshot is what a read shows.
    assert_eq!(shown(&file), theirs);
    file.settle_done().expect("settled");
    let done = file.outbox().done;
    assert!(done[0].seen && done[0].intent.edits.is_empty());
    assert_eq!(
        (done[0].intent.id.as_str(), done[0].intent.by.as_str()),
        (intent.id.as_str(), "person")
    );

    // Held: durable, marked, never applied.
    let held = file
        .write(
            &person(),
            draft(vec![edit(&pk, "label", "mine again", &[&op(1)])]),
            50,
        )
        .expect("accepted");
    assert!(
        file.hold(&held.id, "changed since it was written", 60)
            .expect("held")
    );
    let outbox = file.outbox();
    assert_eq!((outbox.queued.len(), outbox.held.len()), (0, 1));
    assert_eq!(outbox.held[0].intent, held.intent);
    assert_eq!(shown(&file), format!("⚠ mine again ‖ {theirs}"));
    let typed = file.read();
    let row = typed
        .rows()
        .iter()
        .find(|row| row.pk == pk)
        .expect("the row");
    assert_eq!(
        row.values.get("label"),
        ada().values.get("label"),
        "the typed value is still theirs"
    );
    assert_eq!(file.read().status.held, 1);
    assert!(file.read().status.line().contains("1 held back"));
    // The owner keeps their side: a new intent against what the table shows now.
    let again = file
        .requeue(
            &person(),
            &held.id,
            draft(vec![edit(&pk, "label", "mine again", &[&op(2)])]),
            70,
        )
        .expect("accepted")
        .expect("it was held");
    let outbox = file.outbox();
    assert_eq!((outbox.queued.len(), outbox.held.len()), (1, 0));
    assert_eq!(outbox.queued[0].edits, again.edits);
    assert_eq!(outbox.queued[0].resolves.as_deref(), Some(held.id.as_str()));
    assert_ne!(again.id, held.id);
    // `outbox drop` is a person's word.
    let dropped = answer(&scratch.root, &["outbox", "drop", &again.id]);
    assert_eq!(dropped.out, format!("dropped {} (ai/accounts)\n", again.id));
    assert!(file.outbox().queued.is_empty());
    assert_eq!(
        answer(&scratch.root, &["outbox", "drop", &again.id]).code,
        exit::USAGE
    );
}

/// An outbox write whose rename landed and whose folder's flush then failed is accepted: the
/// intent is queued and the answer says the flush failed, never a refusal that would have the
/// person write it a second time.
///
/// NEGATIVE CONTROL, observed: with `put_accepted` refusing a write whose flush failed after the
/// rename (`?` on the landed result), the write is refused while the outbox holds it:
/// `accepted: the outbox holds it: Io(Custom { … "the directory's flush failed (test seam)" })`.
#[test]
fn a_write_that_landed_is_accepted_whatever_its_folders_flush_said() {
    let scratch = scratch();
    let file = File::at(&scratch.root, &name("ai/accounts"));
    crate::outbox::seam::unflush_next();
    let accepted = file
        .write(&person(), draft(vec![edit("ac1", "label", "desk", &[])]), 1)
        .expect("accepted: the outbox holds it");
    assert!(accepted.unflushed.is_some());
    assert_eq!(file.outbox().queued, std::slice::from_ref(&accepted.intent));
    let next = file
        .write(&person(), draft(vec![edit("ac2", "label", "lab", &[])]), 2)
        .expect("accepted");
    assert!(next.unflushed.is_none());
    assert_eq!(file.outbox().queued.len(), 2);
}

/// A Mac's statement of its own row replaces what earlier statements say of the same cells, held
/// ones included, so its statements never fill the bound an owner's write needs.
///
/// NEGATIVE CONTROL, observed: with coalescing over queued statements alone, the held one keeps
/// its cell: `left: [["state", "seen"]] right: [["seen"]]`.
#[test]
fn a_held_statement_of_a_macs_own_row_is_replaced_by_its_latest() {
    let scratch = scratch();
    let links = File::at(&scratch.root, &name("ai/links"));
    let held = links
        .state_mine(
            "harness",
            draft(vec![
                edit("claude.@self", "state", "one", &[]),
                edit("claude.@self", "seen", "then", &[]),
            ]),
            1,
        )
        .expect("accepted");
    assert!(
        links
            .hold(&held.id, "a typed cell key breaks its grammar", 2)
            .expect("held")
    );
    let later = links
        .state_mine(
            "harness",
            draft(vec![edit("claude.@self", "state", "two", &[])]),
            3,
        )
        .expect("accepted");
    let columns = |outbox: &crate::Outbox| -> Vec<Vec<String>> {
        outbox
            .held
            .iter()
            .map(|held| held.intent.edits.iter().map(|e| e.column.clone()).collect())
            .collect()
    };
    let outbox = links.outbox();
    assert_eq!(columns(&outbox), [["seen".to_owned()]]);
    assert_eq!(outbox.queued, std::slice::from_ref(&later.intent));
    links
        .state_mine(
            "harness",
            draft(vec![edit("claude.@self", "seen", "now", &[])]),
            4,
        )
        .expect("accepted");
    let outbox = links.outbox();
    assert!(outbox.held.is_empty(), "{:?}", columns(&outbox));
    assert_eq!(outbox.queued.len(), 2);
}

/// A value shaped like a credential or a phone number is never queued, by an owner's write or a
/// Mac's statement, and a line another build left holding one is never shown by a read: the
/// guard's mark stands in, queued or held.
///
/// NEGATIVE CONTROLS, each observed: with `well_formed` not asking the guard, the owner's write
/// is accepted (`matches!(…, Err(Refused::Malformed(_)))` fails); with the overlay laying an
/// edit's text as it is, the read shows the planted value (`left: "kkkk…"`).
#[test]
fn a_value_the_guard_refuses_is_never_queued_nor_shown() {
    let scratch = scratch();
    let file = File::at(&scratch.root, &name("ai/accounts"));
    let token = "k".repeat(48);
    let phone = format!("+1 {} {} {}", 415, 555, "0100");
    for value in [token.as_str(), phone.as_str()] {
        assert!(matches!(
            file.write(&person(), draft(vec![edit("ac1", "note", value, &[])]), 1),
            Err(crate::Refused::Malformed(_))
        ));
        assert!(matches!(
            file.state_mine(
                "harness",
                draft(vec![edit("ac1.@self", "note", value, &[])]),
                1
            ),
            Err(crate::Refused::Malformed(_))
        ));
    }
    assert!(file.outbox().queued.is_empty());

    // A line another build wrote: a read shows the guard's mark, queued and then held.
    file.record(
        file.look(),
        opened(true, frontier(0, &[1]), Some(accounts(vec![ada()]))),
        10,
    )
    .expect("recorded");
    let pk = ada().pk;
    let planted = file
        .write(&person(), draft(vec![edit(&pk, "note", "plain", &[])]), 20)
        .expect("accepted");
    let path = scratch.root.outbox(file.name());
    let line = std::fs::read_to_string(&path).expect("the outbox");
    std::fs::write(&path, line.replace("plain", &token)).expect("planted");
    let shown = |file: &File| {
        file.read()
            .rows()
            .iter()
            .find(|row| row.pk == pk)
            .map(|row| row.cell("note").to_owned())
            .unwrap_or_default()
    };
    assert_eq!(shown(&file), aterm_value_guard::REFUSED);
    assert!(file.hold(&planted.id, "refused", 30).expect("held"));
    let marked = shown(&file);
    assert!(
        marked.starts_with(crate::outbox::CONFLICT)
            && marked.contains(aterm_value_guard::REFUSED)
            && !marked.contains(&token),
        "{marked}"
    );
}

/// `resolve <id> mine` names the held intent in the one it queues (`resolves`): from the moment
/// that one is written the held one is resolved, so a crash before its held line goes leaves it
/// neither shown as held nor resolvable a second time, and the next settle removes the line.
///
/// NEGATIVE CONTROL, observed: with `outbox()` ignoring `resolves`, the held line comes back
/// beside its replacement: `left: (1, 1) right: (1, 0)`.
#[test]
fn a_held_intent_is_resolved_once_whatever_a_crash_between_its_two_writes_left() {
    let scratch = scratch();
    let file = File::at(&scratch.root, &name("ai/accounts"));
    file.record(
        file.look(),
        opened(true, frontier(0, &[1]), Some(accounts(vec![ada()]))),
        10,
    )
    .expect("recorded");
    let pk = ada().pk;
    let held = file
        .write(
            &person(),
            draft(vec![edit(&pk, "label", "mine", &[&op(1)])]),
            20,
        )
        .expect("accepted");
    assert!(
        file.hold(&held.id, "changed since it was written", 30)
            .expect("held")
    );
    let held_path = scratch.root.held(file.name());
    let held_line = std::fs::read(&held_path).expect("the held list");
    let mine = file
        .requeue(
            &person(),
            &held.id,
            draft(vec![edit(&pk, "label", "mine", &[&op(2)])]),
            40,
        )
        .expect("accepted")
        .expect("it was held");
    assert_eq!(mine.resolves.as_deref(), Some(held.id.as_str()));
    // The crash: the replacement is written, the held line is not gone.
    std::fs::write(&held_path, &held_line).expect("put back");
    let outbox = file.outbox();
    assert_eq!((outbox.queued.len(), outbox.held.len()), (1, 0));
    assert!(
        file.requeue(
            &person(),
            &held.id,
            draft(vec![edit(&pk, "label", "mine", &[&op(2)])]),
            50,
        )
        .expect("read")
        .is_none(),
        "resolved once"
    );
    assert_eq!(
        file.drop_intent(&held.id).expect("read"),
        crate::Dropped::Absent
    );
    assert_eq!(file.outbox().queued, std::slice::from_ref(&mine.intent));
    file.settle_done().expect("settled");
    let left = std::fs::read_to_string(&held_path).expect("the held list");
    assert!(!left.contains(&held.id), "{left}");
}

/// A done marker whose operation the snapshot never names again is still seen: once the
/// snapshot is read from a later table generation (a roll restates every row as new records) or
/// from another replica, the read stops laying the published write over it.
///
/// NEGATIVE CONTROL, observed: with `Done::projected` asking only whether the frontier names the
/// operation, the old write stays laid over the restated row: `left: "mine" right: "claude"`.
#[test]
fn a_marker_a_later_generation_restated_is_seen() {
    let scratch = scratch();
    let file = File::at(&scratch.root, &name("ai/accounts"));
    file.record(
        file.look(),
        opened(true, frontier(0, &[1]), Some(accounts(vec![ada()]))),
        10,
    )
    .expect("recorded");
    let pk = ada().pk;
    let realm = |file: &File| {
        file.read()
            .rows()
            .iter()
            .find(|row| row.pk == pk)
            .map(|row| row.cell("realm").to_owned())
            .unwrap_or_default()
    };
    let intent = file
        .write(
            &person(),
            draft(vec![edit(&pk, "realm", "mine", &[&op(1)])]),
            20,
        )
        .expect("accepted");
    assert!(
        file.mark_done(&intent.id, &mark(Some(op(5))), 30)
            .expect("marked")
    );
    assert!(file.dequeue(&intent.id).expect("dequeued"));
    // The same generation, without the operation: still laid over.
    file.record(
        file.look(),
        opened(true, frontier(0, &[1, 2]), Some(accounts(vec![ada()]))),
        40,
    )
    .expect("recorded");
    file.settle_done().expect("settled");
    assert!(!file.outbox().done[0].seen);
    assert_eq!(realm(&file), "mine");
    // The roll: generation 3 restates the row as a new record; the operation is never named.
    let rolled = crate::Frontier {
        generation: 3,
        ..frontier(0, &[9])
    };
    file.record(
        file.look(),
        opened(true, rolled, Some(accounts(vec![ada()]))),
        50,
    )
    .expect("recorded");
    assert_eq!(realm(&file), "claude");
    file.settle_done().expect("settled");
    assert!(file.outbox().done[0].seen);
}

/// The publisher's claim holds the outbox from its check that an intent is still queued until
/// the intent's marker: a person's drop meanwhile waits, and is told the intent is published
/// rather than that it was dropped. A drop before the claim leaves nothing to claim.
///
/// NEGATIVE CONTROL, observed: with `drop_intent` not asking the markers, the waiting drop
/// answers that it dropped a write a record already carries: `left: Dropped right: Published`.
#[test]
fn a_drop_while_the_publisher_holds_its_claim_is_told_the_intent_is_published() {
    let scratch = scratch();
    let file = File::at(&scratch.root, &name("ai/accounts"));
    let signed = file
        .write(&person(), draft(vec![edit("ac1", "label", "desk", &[])]), 1)
        .expect("accepted");
    let claim = file.claim(&signed.id).expect("read").expect("queued");
    let (other, id) = (file.clone(), signed.id.clone());
    let dropping = std::thread::spawn(move || other.drop_intent(&id));
    assert!(claim.mark_done(&mark(Some(op(3))), 2).expect("marked"));
    assert_eq!(
        dropping.join().expect("joined").expect("read"),
        crate::Dropped::Published
    );
    let answered = answer(&scratch.root, &["outbox", "drop", &signed.id]);
    assert_eq!(answered.code, exit::PROBLEMS, "{}", answered.err);
    assert!(
        answered.err.contains("already published"),
        "{}",
        answered.err
    );

    let dropped = file
        .write(&person(), draft(vec![edit("ac2", "label", "lab", &[])]), 3)
        .expect("accepted");
    assert_eq!(
        file.drop_intent(&dropped.id).expect("read"),
        crate::Dropped::Dropped
    );
    assert!(file.claim(&dropped.id).expect("read").is_none());
}
