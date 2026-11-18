// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! What a traced read of a shared file records of its status file (`aterm_uds::trace`): the
//! status's standing, everything it holds but the count of passes and the pass's time, and no
//! stamp of the file, so a pass that changed nothing but fenced another pass's look, which
//! rewrites the status with only those two moved, leaves a run that read it standing; a reader
//! that decides by the count records it as well, and the one that answers with both poisons the
//! run.

use super::*;
use crate::Source;
use aterm_uds::trace::{self, Effect, Moved, Now, Trace, Unkept};
use std::time::{Duration, Instant};

/// How far past the clock the traced reads here are judged settled ([`trace::read_as_if_later`]):
/// past the settle window, so a file this test just wrote is read as one written long before.
const LATER: Duration = Duration::from_secs(10);

/// The wall reading every trace here is judged at. No read here compares a clock, so any
/// reading stands; this one keeps the tests off the wall clock.
const NOW: Now = Now::at(1_800_000_000);

fn name() -> Name {
    Name::parse("ai/accounts").expect("a name")
}

fn column(name: &str, source: Source) -> Column {
    Column {
        name: name.to_owned(),
        source,
        read_only: true,
        refers: None,
        ty: None,
        null: None,
    }
}

/// The table every projection here takes: one account.
fn accounts() -> Snapshot {
    Snapshot {
        name: name(),
        kind: "table".to_owned(),
        ext: "tsv".to_owned(),
        rev: 0,
        complete: true,
        at_ms: 1,
        columns: vec![column("slug", Source::Derived), column("pk", Source::Pk)],
        handles: vec!["slug".to_owned()],
        rows: vec![Row {
            pk: "ac-0123456789abcdef".to_owned(),
            live: true,
            cells: [("slug".to_owned(), "claude-fb-alpha".to_owned())]
                .into_iter()
                .collect(),
            values: [("slug".to_owned(), Value::from("claude-fb-alpha"))]
                .into_iter()
                .collect(),
        }],
    }
}

/// The replica's frontier every pass here reads: nothing arrives between passes.
fn frontier() -> Frontier {
    Frontier {
        source: [7; 32],
        era: 0,
        generation: 2,
        ids: [[1; 32]].into_iter().collect(),
    }
}

/// A pass that opened custody on the newest home and read [`frontier`], at the key its
/// projection is recorded at (the frontier's fingerprint): one that took `projected`, or, with
/// `None`, one that projected nothing, as the materializer's pass does at rest.
fn opened(projected: Option<Snapshot>) -> Pass {
    let frontier = frontier();
    let key = frontier.fingerprint();
    Pass::Opened(Box::new(Opened {
        newest: true,
        frontier,
        key,
        projection: projected.map_or(Projection::Unchanged, |snapshot| {
            Projection::Fresh(Box::new(Projected {
                snapshot,
                history: Vec::new(),
                baseline: None,
                withheld: 0,
                inert: 0,
            }))
        }),
    }))
}

/// A pass custody refused: another holder kept the vault.
fn busy() -> Pass {
    Pass::Refused {
        word: Word::Held,
        why: Why::Busy,
    }
}

/// Record `pass` at `now_ms` after its own look, as the materializer does.
fn pass(file: &File, pass: Pass, now_ms: u64) -> Recorded {
    file.record(file.look().expect("a look"), pass, now_ms)
        .expect("records")
}

/// What `f` returned and the trace it recorded, its reads judged settled ([`LATER`]).
fn traced<T>(f: impl FnOnce() -> T) -> (T, Trace) {
    trace::read_as_if_later(LATER, || trace::scope(f))
}

/// Whether `trace` stands.
fn stands(trace: &Trace) -> Result<(), Moved> {
    trace.stands(NOW, Instant::now())
}

/// A traced read of a shared file's status records the status's standing and no stamp of its
/// file. The materializer at rest changes nothing and, with no other pass's look outstanding,
/// writes nothing, so every read stands, the status file's stamp too; projecting the same rows
/// again writes nothing either. While another pass's look is outstanding the same pass fences
/// it, rewriting the status with only its count and time moved: that leaves the status read, the
/// status and the read standing, where a read of the file by its stamp moves. A pass that
/// replaced nothing but said another word moves the status read, and one that fenced a look and
/// said it again does not.
/// NEGATIVE CONTROL (2026-11-16, at the merge of main's write-nothing pass): with `read_state`
/// answering `state_at(path).ok().flatten()` with the scope armed (no apart read, no standing),
/// which records the status file's stamp as `read_regular` does, the fencing pass moves the
/// status read at the file: `left: Err(File(".../ai/accounts.state.json")) right: Ok(())`, "a
/// pass that fenced a look and changed nothing leaves the status read standing".
#[test]
fn a_pass_that_replaced_nothing_leaves_a_traced_status_read_standing() {
    let dir = aterm_tempfile::tempdir().expect("a scratch folder");
    let root = Root::at(&dir.path().join("files"));
    let file = File::at(&root, &name());
    let status_file = root.state(&name());
    assert_eq!(
        pass(&file, opened(Some(accounts())), 10_000),
        Recorded::Recorded { replaced: true }
    );

    let (marks, by_state) = traced(|| {
        file.state()
            .map(|state| (state.fingerprint, state.rev, state.word))
    });
    assert_eq!(
        marks.as_ref().map(|(_, rev, word)| (*rev, *word)),
        Some((1, Some(Word::Live)))
    );
    let (status, by_status) = traced(|| file.status());
    assert_eq!((status.word, status.rev()), (Word::Live, 1));
    let (read, by_read) = traced(|| file.read());
    assert_eq!(read.rows().len(), 1);
    let (_, by_stamp) = traced(|| aterm_uds::atomic_file::read_regular(&status_file, MAX_BYTES));
    for trace in [&by_state, &by_status, &by_read, &by_stamp] {
        assert_eq!(trace.unkept(), None);
        assert_eq!(stands(trace), Ok(()));
    }

    // The materializer at rest, no other look outstanding: it writes nothing.
    assert_eq!(pass(&file, opened(None), 70_000), Recorded::Settled);
    assert_eq!(
        pass(&file, opened(Some(accounts())), 100_000),
        Recorded::Settled,
        "the same rows projected again"
    );
    for trace in [&by_state, &by_status, &by_read, &by_stamp] {
        assert_eq!(
            stands(trace),
            Ok(()),
            "a pass that wrote nothing moves nothing"
        );
    }

    // The same pass while another pass's look is outstanding fences it.
    let outstanding = file.look().expect("a look");
    assert_eq!(pass(&file, opened(None), 130_000), Recorded::Fenced);
    drop(outstanding);
    let state = file.state().expect("a status");
    assert_eq!((state.seq, state.pass_ms), (2, 130_000), "the pass fenced");
    assert_eq!(
        stands(&by_stamp),
        Err(Moved::File(status_file.clone())),
        "the pass rewrote the status file"
    );
    assert_eq!(
        stands(&by_state),
        Ok(()),
        "a pass that fenced a look and changed nothing leaves the status read standing"
    );
    assert_eq!(stands(&by_status), Ok(()), "and the status");
    assert_eq!(stands(&by_read), Ok(()), "and the read");
    assert!(
        by_state.standings().any(|at| at == status_file),
        "a status read records the status's standing"
    );
    for trace in [&by_state, &by_status, &by_read] {
        assert!(
            trace.files().all(|(at, _)| at != status_file),
            "and no stamp of its file"
        );
    }

    assert_eq!(
        pass(&file, busy(), 140_000),
        Recorded::Recorded { replaced: false }
    );
    assert_eq!(
        stands(&by_state),
        Err(Moved::Standing(status_file.clone())),
        "a pass that replaced nothing but said another word moves it"
    );
    let (_, held) = traced(|| file.state());
    let outstanding = file.look().expect("a look");
    assert_eq!(pass(&file, busy(), 150_000), Recorded::Fenced);
    drop(outstanding);
    assert_eq!(
        stands(&held),
        Ok(()),
        "and a pass that fenced a look and said it again does not"
    );
}

/// The count of passes is what [`File::look`] answers and what [`File::record`] decides by, so
/// each records it beside the status's standing: a traced look moves once a pass moves the count
/// (one that fences that look), and a traced record that pass overtook records the count twice
/// and writes nothing, so its run is kept. A look that makes the file's folders, or sets a
/// folder's mode back, records a write, and one over folders already private records none.
/// NEGATIVE CONTROLS (2026-11-16, at the merge of main's look lease): with `look` reading the
/// count through `Self::state`, the traced look stands across the fencing pass: `left: Ok(())
/// right: Err(Standing(".../ai/accounts.state.json"))`, "a pass that moves the count moves a run
/// that took it". With `record` reading through `Self::state`, its trace holds one standing of
/// the status file: `left: 1 right: 2`, "the overtaken record recorded the count". With
/// `private_dir` recording no write, the look that made the folders is kept: `left: None right:
/// Some(Effect(Write))`, "a look that made the file's folders".
#[cfg(unix)]
#[test]
fn a_traced_look_and_record_record_the_count_of_passes() {
    use std::os::unix::fs::PermissionsExt as _;
    let dir = aterm_tempfile::tempdir().expect("a scratch folder");
    let root = Root::at(&dir.path().join("files"));
    let file = File::at(&root, &name());
    let status_file = root.state(&name());
    let (first, making) = traced(|| file.look().expect("a look"));
    assert_eq!(
        making.unkept(),
        Some(Unkept::Effect(Effect::Write)),
        "a look that made the file's folders"
    );
    drop(first);
    pass(&file, opened(Some(accounts())), 10_000);

    let (look, by_look) = traced(|| file.look().expect("a look"));
    assert_eq!(look.seq(), 1);
    assert_eq!(by_look.unkept(), None, "folders already private");
    assert_eq!(stands(&by_look), Ok(()));
    assert_eq!(pass(&file, opened(None), 70_000), Recorded::Fenced);
    assert_eq!(
        stands(&by_look),
        Err(Moved::Standing(status_file.clone())),
        "a pass that moves the count moves a run that took it"
    );

    // The pass that looked before the fencing one records.
    let (recorded, by_record) = traced(|| file.record(look, opened(None), 80_000));
    assert_eq!(recorded.expect("records"), Recorded::Raced);
    assert_eq!(
        by_record
            .standings()
            .filter(|at| *at == status_file)
            .count(),
        2,
        "the overtaken record recorded the count"
    );
    assert_eq!(
        by_record.unkept(),
        None,
        "an overtaken record writes nothing"
    );

    std::fs::set_permissions(
        root.state_dir(&name()),
        std::fs::Permissions::from_mode(0o755),
    )
    .expect("opened up");
    let (_, setting) = traced(|| file.look().expect("a look"));
    assert_eq!(
        setting.unkept(),
        Some(Unkept::Effect(Effect::Write)),
        "a look that set a folder's mode back"
    );
}

/// [`crate::Status::json`] answers with the last pass's count and time, which a traced read of
/// the status leaves out, so it poisons the run once a pass recorded; before any did, it holds
/// neither and poisons nothing.
/// NEGATIVE CONTROL (2026-11-15): with `Status::json` poisoning nothing, the run that took a
/// recorded status as JSON is kept: "a status as JSON poisons the run: None".
#[test]
fn a_status_as_json_poisons_a_traced_run_once_a_pass_recorded() {
    let dir = aterm_tempfile::tempdir().expect("a scratch folder");
    let root = Root::at(&dir.path().join("files"));
    let file = File::at(&root, &name());

    let (_, before) = traced(|| file.status().json());
    assert_eq!(before.unkept(), None, "no pass recorded: nothing to answer");
    pass(&file, opened(Some(accounts())), 10_000);
    let (json, after) = traced(|| file.status().json());
    assert_eq!(json.get("passes").and_then(Value::as_u64), Some(1));
    assert!(
        matches!(after.unkept(), Some(Unkept::Poison(_))),
        "a status as JSON poisons the run: {:?}",
        after.unkept()
    );
}

/// A read that found no status records a standing of none, with no retry: no rerun answers
/// otherwise until a status is there, and the pass that writes one moves the read. So does a
/// status file whose bytes are no status this build reads, and a folder where the status file
/// goes, which `read_regular` refuses as no regular file and records no retry for, unlike an
/// I/O fault.
/// NEGATIVE CONTROL (2026-11-15): with `read_state` recording `trace::fs::failed(path, error)`
/// for every failed read in place of the apart read's retry, the folder's read records a retry
/// (`left: Some(Retry(".../ai/accounts.state.json: not a regular file")) right: None`).
#[test]
fn a_status_that_is_no_status_answers_none_and_retries_nothing() {
    let dir = aterm_tempfile::tempdir().expect("a scratch folder");
    let root = Root::at(&dir.path().join("files"));
    let file = File::at(&root, &name());
    let status_file = root.state(&name());

    let (state, missing) = traced(|| file.state());
    assert!(state.is_none());
    assert_eq!(missing.unkept(), None);
    assert_eq!(stands(&missing), Ok(()));
    pass(&file, busy(), 5_000);
    assert_eq!(
        stands(&missing),
        Err(Moved::Standing(status_file.clone())),
        "the first pass moves a read that found no status"
    );

    std::fs::write(&status_file, b"{\"format\":\"aterm-file-state/9\"}\n").expect("written");
    let (state, other_format) = traced(|| file.state());
    assert!(state.is_none());
    assert_eq!(other_format.unkept(), None);
    assert_eq!(stands(&other_format), Ok(()));
    pass(&file, busy(), 10_000);
    assert_eq!(
        stands(&other_format),
        Err(Moved::Standing(status_file.clone())),
        "the pass that wrote a status moves it"
    );

    std::fs::remove_file(&status_file).expect("removed");
    std::fs::create_dir(&status_file).expect("a folder in its place");
    let (state, folder) = traced(|| file.state());
    assert!(state.is_none());
    assert_eq!(folder.unkept(), None, "a folder is no fault to retry");
    assert_eq!(stands(&folder), Ok(()));
    std::fs::remove_dir(&status_file).expect("removed");
    pass(&file, busy(), 20_000);
    assert_eq!(stands(&folder), Err(Moved::Standing(status_file)));
}
