// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The materializer over a family that keeps its records across a roll (tracker TK-41): a log's
//! shared file, its history and its frontier, and the per-cell reads beside them
//! (`att::values`), each over every generation's log. The family is `att::kept_tests`', which
//! no build registers; a stand-in lab of two Macs holds it. No live vault or table is read.

use super::{Pass, VAULT_WAIT, pass_over};
use crate::{
    att::{
        self, Handle, Source,
        kept_tests::{KEPT, REMARK, declare, said, state},
    },
    catalog::State,
    file::{Class, Computed, Store, Type, types::LOG},
    table::{fixture::repair_commons_among, tests::Lab},
    typed::{keys, owned},
};
use astream_wire::sync::Digest;
use aterm_codec::hex;
use aterm_file::{File, Word};
use aterm_json::{Value, json};
use aterm_secrets::commons::roster::Role;
use std::{collections::BTreeSet, time::Instant};

/// The kept family's remarks as a shared file: one row an entry, `<uuid32>.<n>`, as
/// `lab/measures` and `lab/notes` are of `lab/tracker`'s logs.
const REMARKS: Type = Type {
    name: "lab/remarks",
    about: "each machine's remarks",
    class: Class::Mac,
    store: Store::Table {
        family: &KEPT,
        collection: &REMARK,
    },
    computed: Computed::None,
    handles: &[],
};

const FILES: [Type; 1] = [REMARKS];

/// TK-41's exit test (docs/KNOWN_ISSUES.md ATERM-KI-1118-4040, which it closes): AFTER A ROLL
/// OF A FAMILY THAT KEEPS ITS RECORDS, A LOG'S SHARED FILE, ITS HISTORY, ITS FRONTIER AND THE
/// PER-CELL READS NAME THE ENTRIES OF THE SEALED GENERATION TOO. The family is
/// `kept_tests::KEPT`: two generations, each with remarks (a log, `<uuid32>.<n>`). Two Macs,
/// both Writers: the reader, which rolls the family and whose files are read, and an author.
///
/// Into the first generation the reader appends one remark and the author two, so the reader
/// holds log entries another machine signed. The reader's pass writes `lab/remarks` with the
/// three. The reader rolls the family, which restates no log entry, and the author appends a
/// fourth remark into the second generation. Then:
///
/// * the reader's pass writes `lab/remarks` with all four entries, each at its signer's key
///   with its text, the three of the sealed generation among them, and counts nothing inert;
/// * the file's history lists the operation of each of the four with the key it wrote and its
///   signer, the sealed three as history (`Handle::history`) names them: records of
///   generation 1;
/// * the snapshot's frontier is of generation 2 and names two stores, the sealed generation's
///   first, at least as far along as the frontier the pass before the roll recorded for it;
/// * on both Macs `cell_values` of each sealed entry's key lists that one record, as a record
///   of the sealed log ([`Source::Sealed`]) under its own signer's machine, standing, with its
///   text, and of the fourth's key a record of the second generation's log;
/// * on both Macs `wrote` of each sealed record names the one key it wrote, with that same
///   write.
///
/// NEGATIVE CONTROL, observed 2026-11-18: with `Family::read` opening no sealed generation
/// (`att::kept::earlier(first, compiled)` replaced by `Vec::new()`: the read of the current
/// generation alone that the materializer made before this), the test fails with ``assertion
/// `left == right` failed: lab/remarks lists every generation's remarks``, `left` holding the
/// fourth remark's key alone. With that restored and `att::values`' read opening none (`over`
/// given `&[]` for `&self.earlier()`), it fails with `Mac 0: cell_values lists the sealed
/// record of <key>`.
#[test]
fn a_kept_family_after_a_roll_lists_every_generations_log_entries_with_their_history_and_values() {
    const READER: usize = 0;
    const AUTHOR: usize = 1;
    att::register_for_test(&KEPT);
    assert!(att::keeps_records(&KEPT));

    let lab = Lab::new(&[Role::Writer, Role::Writer]);
    let config = lab._home.path().join("aterm.toml");
    let now_ms = aterm_time::unix_millis().unwrap();
    let clocks = [now_ms / 1000; 2];
    let exchange = |rounds| repair_commons_among(&lab, rounds, &[READER, AUTHOR], &clocks);
    let at = |n: usize| Handle::at(lab.paths[n].clone(), &KEPT);
    let saids = [
        said("s-reader", "TK-41: the reader's own remark, and the roll"),
        said("s-author", "TK-41: the author's remarks"),
    ];
    // Mac `n` appends a remark: its key.
    let remark = |n: usize, text: &str| {
        let row = json!({"text": text}).as_object().unwrap().clone();
        at(n)
            .saying(saids[n].clone())
            .append_log(&REMARK, &[row])
            .unwrap()
            .remove(0)
    };
    // The operation that stands at the remark `key`, as Mac `n`'s view reads it.
    let remark_op = |n: usize, key: &str| -> Digest {
        at(n).view().unwrap().logs[REMARK.name]
            .entries
            .iter()
            .find(|entry| owned::log_entry_key(&entry.machine, entry.n) == key)
            .unwrap_or_else(|| panic!("Mac {n}: the view holds the remark {key}"))
            .operation
    };
    // The reader's pass over `lab/remarks`, which ends live.
    let file = File::at(&crate::file::root(&lab.paths[READER]), &REMARKS.file());
    let pass = || {
        let mut outcomes = pass_over(
            &lab.paths[READER],
            &config,
            &FILES,
            &Pass {
                read_by: Instant::now() + aterm_spec::wait::MUST_HAPPEN,
                vault_wait: VAULT_WAIT,
                again: false,
                now_ms,
            },
        );
        assert_eq!(outcomes.len(), 1);
        let outcome = outcomes.remove(0);
        assert_eq!(
            (outcome.word, outcome.why, outcome.problem.as_deref()),
            (Word::Live, None, None),
            "{:?}",
            outcome.detail
        );
    };
    // The file's rows: each key with its text.
    let rows = || -> BTreeSet<(String, String)> {
        file.read()
            .rows()
            .iter()
            .map(|row| (row.pk.clone(), row.cell("text").to_owned()))
            .collect()
    };

    // The first generation: the reader's remark and the author's two, which the reader hears.
    declare(&lab.paths[READER], &KEPT);
    declare(&lab.paths[AUTHOR], &KEPT);
    exchange(6);
    let own = remark(READER, "the reader's");
    exchange(6);
    let first = remark(AUTHOR, "the author's first");
    let second = remark(AUTHOR, "the author's second");
    exchange(6);
    // Each entry of the first generation: its key, its text, its signer and its record.
    let sealed: Vec<(String, &str, usize, Digest)> = [
        (own, "the reader's", READER),
        (first, "the author's first", AUTHOR),
        (second, "the author's second", AUTHOR),
    ]
    .into_iter()
    .map(|(key, text, who)| {
        let op = remark_op(READER, &key);
        (key, text, who, op)
    })
    .collect();
    for (key, _, who, _) in &sealed {
        assert_eq!(
            owned::log_key(key).map(|(machine, _)| machine),
            Some(lab.machine(*who)),
            "a remark at its signer's key"
        );
    }
    let sealed_rows: BTreeSet<(String, String)> = sealed
        .iter()
        .map(|(key, text, _, _)| (key.clone(), (*text).to_owned()))
        .collect();
    pass();
    assert_eq!(rows(), sealed_rows, "lab/remarks before the roll");
    let before = file.frontier().expect("the pass recorded a frontier");
    assert_eq!((before.generation, before.stores.len()), (1, 1));

    // The roll restates no log entry and seals the first generation.
    let reader = || at(READER).saying(saids[READER].clone());
    let staged = reader().roll(true, now_ms).unwrap();
    assert!(!staged.sealed, "{staged:?}");
    exchange(6);
    assert!(at(AUTHOR).endorse().unwrap());
    exchange(6);
    let rolled = reader().roll(true, now_ms).unwrap();
    assert!(rolled.sealed && rolled.waiting.is_empty(), "{rolled:?}");
    exchange(6);
    for n in [READER, AUTHOR] {
        assert_eq!(at(n).compiled().generation, 2, "Mac {n} reads the second");
        assert_eq!(state(&lab.paths[n], &KEPT), Some(State::Kept), "Mac {n}");
    }

    // The second generation: the author's fourth remark, its counter past the sealed two.
    let later = remark(AUTHOR, "the author's, after the roll");
    assert_eq!(owned::log_key(&later), Some((lab.machine(AUTHOR), 3)));
    exchange(6);
    let later_op = remark_op(READER, &later);
    assert!(
        at(READER)
            .view()
            .unwrap()
            .ops
            .iter()
            .flat_map(|op| &op.changes)
            .filter(|change| change.collection == REMARK.name)
            .all(|change| change.key == later),
        "the second generation's own log holds the fourth remark alone"
    );

    // The file: every generation's entries.
    pass();
    let mut every = sealed_rows.clone();
    every.insert((later.clone(), "the author's, after the roll".to_owned()));
    assert_eq!(
        rows(),
        every,
        "lab/remarks lists every generation's remarks"
    );
    let read = file.read();
    assert_eq!(read.snapshot.as_ref().unwrap().kind, LOG);
    assert_eq!(read.status.state.as_ref().unwrap().inert, Some(0));

    // Its history: each generation's operations, each with the key it wrote and its signer.
    let history = file.history();
    let handle_history = at(READER).history().unwrap();
    let listed = |op: &Digest| -> &Value {
        let id = hex::encode_lower(op);
        let mut entries = history
            .iter()
            .filter(|entry| entry.get("op").and_then(Value::as_str) == Some(id.as_str()));
        let entry = entries
            .next()
            .unwrap_or_else(|| panic!("lab/remarks' history lists the operation {id}"));
        assert!(entries.next().is_none(), "{id} is listed once");
        entry
    };
    assert_eq!(history.len(), 4, "{history:?}");
    for (key, generation, who, op) in sealed
        .iter()
        .map(|(key, _, who, op)| (key, 1, *who, op))
        .chain([(&later, 2, AUTHOR, &later_op)])
    {
        let entry = listed(op);
        assert_eq!(entry["keys"], json!([key]), "{entry:?}");
        assert_eq!(entry["revert"], json!(false));
        let signed = handle_history
            .iter()
            .find(|signed| signed.operation == *op)
            .expect("the table's history lists the record");
        assert_eq!(signed.generation, generation, "{key}");
        assert_eq!(signed.machine, Some(lab.machine(who)), "{key}");
        assert_eq!(entry["by"], json!(keys::actor_key(&signed.actor)), "{key}");
        assert_eq!(entry["ms"], json!(signed.authored_ms), "{key}");
    }

    // Its frontier: the sealed generation's store, then the second generation's.
    let frontier = file.frontier().expect("the pass recorded a frontier");
    assert_eq!(frontier.generation, 2);
    assert_eq!(frontier.stores.len(), 2, "{frontier:?}");
    assert!(
        frontier.stores[0].extends(&before.stores[0]),
        "the sealed generation's store, as far along as the pass before the roll read it"
    );
    assert_ne!(frontier.stores[1].digest, before.stores[0].digest);
    assert!(frontier.admits_over(&before));

    // The per-cell reads, on both Macs.
    for n in [READER, AUTHOR] {
        let handle = at(n);
        for (key, text, who, op) in &sealed {
            let values = handle.cell_values(REMARK.name, key).unwrap();
            assert_eq!(
                values.len(),
                1,
                "Mac {n}: cell_values lists the sealed record of {key}"
            );
            let held = &values[0];
            assert_eq!(held.operation, *op, "Mac {n}: {key}");
            assert_eq!(held.source, Source::Sealed { generation: 1 }, "Mac {n}");
            assert_eq!(held.machine, Some(lab.machine(*who)), "Mac {n}: {key}");
            assert!(held.stands && held.inert.is_none(), "Mac {n}: {held:?}");
            assert!(!held.withheld && !held.refused, "Mac {n}: {held:?}");
            assert_eq!(held.value.as_ref().unwrap()["text"], json!(text));

            let wrote = handle.wrote(*op).unwrap();
            assert_eq!(
                wrote.len(),
                1,
                "Mac {n}: wrote names the sealed entry {key}"
            );
            assert_eq!(
                (wrote[0].collection.as_str(), wrote[0].key.as_str()),
                (REMARK.name, key.as_str()),
                "Mac {n}"
            );
            assert_eq!(wrote[0].wrote, values, "Mac {n}: {key}");
            assert!(
                wrote[0].replaced.is_empty()
                    && wrote[0].unseen.is_empty()
                    && wrote[0].since.is_empty(),
                "Mac {n}: a log entry replaces nothing and nothing writes its key again"
            );
        }
        let values = handle.cell_values(REMARK.name, &later).unwrap();
        assert_eq!(values.len(), 1, "Mac {n}: {values:?}");
        assert_eq!(
            (values[0].operation, values[0].source, values[0].stands),
            (later_op, Source::Log, true),
            "Mac {n}: the fourth remark is the second generation's own"
        );
        assert_eq!(handle.wrote(later_op).unwrap()[0].wrote, values, "Mac {n}");
    }
}
