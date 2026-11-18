// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! What a traced read of `state.toml` records (`aterm_uds::trace`): the file's standing, which
//! the subscriber's heard-only rewrites leave, and no stamp of it, so a run that read the rest
//! of the state stands across those rewrites; a reader that answers with `heard` records the
//! whole state, which they move.

use super::*;
use aterm_uds::trace::{self, Moved, Now, Trace};
use std::time::{Duration, Instant};

/// How far past the clock the traced reads here are judged settled ([`trace::read_as_if_later`]):
/// past the settle window, so a state this test just saved is read as one saved long before.
const LATER: Duration = Duration::from_secs(10);

fn scratch() -> (aterm_tempfile::TempDir, Paths) {
    let root = aterm_tempfile::tempdir().expect("tempdir");
    let paths = Paths::for_home(&root.path().join("mac"));
    (root, paths)
}

/// What `f` returned and the trace it recorded, its reads judged settled ([`LATER`]).
fn traced<T>(f: impl FnOnce() -> T) -> (T, Trace) {
    trace::read_as_if_later(LATER, || trace::scope(f))
}

/// Whether `trace` stands now.
fn stands(trace: &Trace) -> Result<(), Moved> {
    trace.stands(Now::at_now(), Instant::now())
}

/// A member's state at `seq`, the vault last heard at `heard`.
fn member(seq: u64, heard: u64) -> State {
    State {
        seq,
        heard,
        membership: Stored::In,
        ..State::default()
    }
}

/// A traced read of `state.toml` and of its standing records the file's standing and no stamp
/// of it: a save that moves only `heard` leaves the trace standing, where a read that records
/// the file's stamp moves at that save, and a save that moves the sequence moves it.
/// NEGATIVE CONTROL (2026-11-14): with `read_state_at` reading through `parse_state_at` alone
/// with a scope armed, which records the file's stamp as `state_read_by_stamp` makes it, the
/// heard-only save moves the trace at the file (`a heard-only save leaves it standing`).
#[test]
fn a_heard_only_save_leaves_a_traced_state_read_standing() {
    let (_root, paths) = scratch();
    let _lock = StateLock::take(&paths).unwrap();
    let file = paths.state_file();
    save_state(&paths, &member(7, 1_000)).unwrap();
    let ((state, standing), by_standing) = traced(|| {
        let state = read_state(&paths).unwrap();
        (state, state_standing(&paths).unwrap())
    });
    assert_eq!(state.map(|state| state.seq), Some(7));
    assert!(standing.is_some());
    let (_, by_stamp) = state_read_by_stamp(|| traced(|| read_state(&paths).unwrap()));
    assert_eq!(stands(&by_standing), Ok(()));
    assert_eq!(stands(&by_stamp), Ok(()));

    save_state(&paths, &member(7, 1_060)).unwrap();
    assert_eq!(
        stands(&by_standing),
        Ok(()),
        "a heard-only save leaves it standing"
    );
    assert_eq!(
        stands(&by_stamp),
        Err(Moved::File(file.clone())),
        "a read by the file's stamp moves at the same save"
    );
    assert!(
        by_standing.standings().any(|at| at == file),
        "a read of the state records its standing"
    );
    assert!(
        by_standing.files().all(|(at, _)| at != file),
        "and no stamp of it"
    );
    assert_eq!(by_standing.unkept(), None);

    save_state(&paths, &member(8, 1_060)).unwrap();
    assert_eq!(
        stands(&by_standing),
        Err(Moved::Standing(file.clone())),
        "a save that moves the sequence moves it"
    );
}

/// [`status`] answers with `heard`, so its trace records the whole state, which a heard-only
/// save moves.
/// NEGATIVE CONTROL (2026-11-14): with `status` not calling `note_whole_state`, its trace stands
/// across the heard-only save while its answer moved (`a status that answered heard moves`).
#[test]
fn a_status_that_answered_heard_moves_at_a_heard_only_save() {
    let (root, paths) = scratch();
    let _lock = StateLock::take(&paths).unwrap();
    save_config(
        &paths,
        &Config {
            source: Source::Path(root.path().join("vault.txt")),
            mirrors: vec![],
            name: "lab".to_owned(),
            signer: None,
            followed: vec![],
            host: "mac".to_owned(),
        },
    )
    .unwrap();
    save_state(&paths, &member(7, 1_000)).unwrap();
    let (answer, trace) = traced(|| status(&paths).unwrap());
    assert!(
        matches!(
            answer.membership,
            Membership::In {
                seq: 7,
                heard: 1_000
            }
        ),
        "{:?}",
        answer.membership
    );
    assert_eq!(trace.unkept(), None);
    assert_eq!(stands(&trace), Ok(()));

    save_state(&paths, &member(7, 1_060)).unwrap();
    assert_eq!(
        stands(&trace),
        Err(Moved::Standing(paths.state_file())),
        "a status that answered heard moves"
    );
}
