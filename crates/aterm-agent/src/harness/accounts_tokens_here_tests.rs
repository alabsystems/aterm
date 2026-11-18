// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A window's hold on Claude Code's reading of the rows on their vault token here: what it
//! vouches for, how another process reads that, and how the vouch stands down.

use super::super::{Row, State, Store, TOKENS_HERE_FILE, TOKENS_HERE_FRESH_S, on_token_where};
use super::{TOKENS_HERE_LOCK_FILE, TokensHereHolder, elsewhere};
use aterm_uds::trace::{self, Unkept};
use std::collections::BTreeMap;
use std::path::Path;

const NOW: i64 = 1_790_000_000;
const HOUR: i64 = 3600;
const DAY: i64 = 24 * HOUR;

/// A scratch harness state and the Claude Code store in it.
fn scratch(tag: &str) -> (aterm_tempfile::TempDir, Store) {
    let dir = aterm_tempfile::Builder::new()
        .prefix(&format!("aterm-tokens-here-{tag}-"))
        .tempdir()
        .expect("temp dir");
    let store = Store::at(dir.path());
    store.ensure().expect("the store");
    (dir, store)
}

/// The vault's reading naming `pool-a`'s token as in use here until a day after [`NOW`].
fn read() -> BTreeMap<String, u64> {
    [("pool-a".to_owned(), u64::try_from(NOW + DAY).unwrap())].into()
}

/// The file's inode and modification time, which a write moves.
fn written(store: &Store) -> (u64, i128) {
    use std::os::unix::fs::MetadataExt as _;
    let meta = std::fs::symlink_metadata(store.dir.join(TOKENS_HERE_FILE)).expect("the file");
    (
        meta.ino(),
        i128::from(meta.mtime()) * 1_000_000_000 + i128::from(meta.mtime_nsec()),
    )
}

/// When `pool-a`'s token ends by `store`'s reading at `now`, on a host where a launch starts on a
/// token ([`on_token_where`]).
fn on_token(store: &Store, now: i64) -> Option<i64> {
    let row = Row {
        label: "pool-a".to_owned(),
        ..Row::default()
    };
    let state = State {
        tokens_here: store.read_tokens_here(),
        ..State::default()
    };
    on_token_where(true, &row, &state, now)
}

/// A READING THE HOLDER VOUCHES FOR IS WRITTEN ONLY WHERE IT CHANGES, AND NO AGE OF IT IS JUDGED.
/// The first read is written at its time; the holder's turn then vouches for it. The same
/// reading read again two hours later writes nothing (the file's inode and time stay), still
/// stands for its row at that time, and reads `vouched`; a reading with another end is written at
/// its time. CONTROL: a store nobody holds writes the same reading again at each read, and a
/// reading over an hour old says nothing, as before.
///
/// NEGATIVE CONTROLS (2026-11-15), each the one mutation of its mutant build that reached this
/// test:
/// - `on_token_where` judging the age of a vouched reading too (without `!read.vouched &&`):
///   `vouched, it stands at any age` failed, left `None`, right `Some(1790086400)`;
/// - `note_tokens_here` writing nothing for the same rows whether or not anyone vouches (its
///   `kept.vouched && kept.ends == ends` as `kept.ends == ends`): `CONTROL: nobody holds it, so
///   it is written again` failed;
/// - `note_tokens_here` writing the reading at every read (that test as `false`): `the same
///   reading writes nothing` failed.
///
/// Each restored, the file touched.
#[test]
fn a_reading_the_holder_vouches_for_is_written_only_where_it_changes_and_its_age_is_not_judged() {
    let (_dir, store) = scratch("vouched");
    let holder = TokensHereHolder::of(&store).expect("Claude Code's store is held");
    assert!(
        store.note_tokens_here(Some(&read()), NOW).unwrap(),
        "written"
    );
    assert!(!holder.vouching(), "no turn has vouched yet");
    holder.turn(NOW);
    assert!(holder.vouching(), "the turn vouches for the read reading");

    let first = written(&store);
    let later = NOW + 2 * HOUR;
    assert!(
        !store.note_tokens_here(Some(&read()), later).unwrap(),
        "the same reading writes nothing"
    );
    holder.turn(later);
    assert_eq!(written(&store), first, "the file was not written");
    let kept = store.read_tokens_here().expect("kept");
    assert_eq!((kept.read_at, kept.vouched), (NOW, true));
    assert_eq!(
        on_token(&store, later),
        Some(NOW + DAY),
        "vouched, it stands at any age"
    );

    let moved: BTreeMap<String, u64> =
        [("pool-a".to_owned(), u64::try_from(NOW + 2 * DAY).unwrap())].into();
    assert!(
        store.note_tokens_here(Some(&moved), later).unwrap(),
        "another end is written"
    );
    assert_eq!(
        store.read_tokens_here().map(|kept| kept.read_at),
        Some(later)
    );
    drop(holder);

    // CONTROL: with no holder the reading is written again at each read, and ages.
    let (_dir, store) = scratch("unheld");
    assert!(store.note_tokens_here(Some(&read()), NOW).unwrap());
    assert!(
        store.note_tokens_here(Some(&read()), NOW + HOUR).unwrap(),
        "CONTROL: nobody holds it, so it is written again"
    );
    let kept = store.read_tokens_here().expect("kept");
    assert_eq!((kept.read_at, kept.vouched), (NOW + HOUR, false));
    assert_eq!(
        on_token(&store, NOW + HOUR + TOKENS_HERE_FRESH_S),
        Some(NOW + DAY)
    );
    assert_eq!(
        on_token(&store, NOW + HOUR + TOKENS_HERE_FRESH_S + 1),
        None,
        "CONTROL: over an hour old, it says nothing"
    );
}

/// ANOTHER PROCESS READS THE VOUCH BY THE LOCK BESIDE THE READING. A look from a process with no
/// holder of the store ([`elsewhere`], which takes the lock shared) finds a holder while one
/// vouches, and none before its first turn, after it stood down, and with no lock file at all;
/// in this process the holder's own word decides, and a look made under an armed trace scope
/// poisons it, since another process's lock moves no stamp.
///
/// NEGATIVE CONTROL (2026-11-15): `Probe::held_elsewhere` answering `false` where the shared take
/// blocks failed `a holder vouches`; restored, the file touched.
#[test]
fn another_process_reads_the_vouch_by_the_lock_beside_the_reading() {
    let (_dir, store) = scratch("elsewhere");
    assert!(!elsewhere(&store.dir), "no lock file");
    let holder = TokensHereHolder::of(&store).expect("held");
    assert!(store.note_tokens_here(Some(&read()), NOW).unwrap());
    assert!(!elsewhere(&store.dir), "no turn has vouched");
    holder.turn(NOW);
    assert!(store.dir.join(TOKENS_HERE_LOCK_FILE).is_file());
    assert!(elsewhere(&store.dir), "a holder vouches");
    assert!(
        store.read_tokens_here().is_some_and(|kept| kept.vouched),
        "the holder's word, here"
    );
    let (vouched, looked) = trace::scope(|| elsewhere(&store.dir));
    assert!(vouched);
    assert!(
        matches!(looked.unkept(), Some(Unkept::Poison(_))),
        "{looked:?}"
    );
    holder.turn(NOW + 60);
    assert!(!elsewhere(&store.dir), "stood down");
}

/// A TURN THAT READ NO VAULT STANDS THE VOUCH DOWN, AND THE READING AGES FROM THEN. A vault that
/// could not be read keeps the reading, records a retry in the armed trace scope and writes
/// nothing; the turn after it writes the reading again at its time, lets the lock go (another
/// process finds no holder), and from then the reading ages as one nobody holds: it stands for
/// [`TOKENS_HERE_FRESH_S`] after the turn and not a second longer. A read that answers again is
/// written at its time and vouched for again. A turn whose passes noted nothing at all (the
/// accounts off, no roster) stands it down the same way.
///
/// NEGATIVE CONTROLS (2026-11-15), each the one mutation of its mutant build that reached this
/// test:
/// - `stand_down` without its `stamp_tokens_here`: `the turn wrote it at its time` failed, left
///   `(1790000000, false)`, right `(1790007200, false)`;
/// - `note_tokens_here` without its `trace::retry`: `the unknown read is not kept: None` failed;
/// - `note_tokens_here` writing nothing for the same rows whether or not anyone vouches (its
///   `kept.vouched && kept.ends == ends` as `kept.ends == ends`): `with no vouch, a read is
///   written at its time` failed.
///
/// Each restored, the file touched.
#[test]
fn a_turn_that_read_no_vault_stands_the_vouch_down_and_the_reading_ages_from_then() {
    let (_dir, store) = scratch("unknown");
    let holder = TokensHereHolder::of(&store).expect("held");
    assert!(store.note_tokens_here(Some(&read()), NOW).unwrap());
    holder.turn(NOW);
    let first = written(&store);

    let unknown = NOW + 2 * HOUR;
    let (noted, looked) = trace::scope(|| store.note_tokens_here(None, unknown));
    assert!(!noted.unwrap(), "nothing written");
    assert!(
        matches!(looked.unkept(), Some(Unkept::Retry(_))),
        "the unknown read is not kept: {:?}",
        looked.unkept()
    );
    assert_eq!(written(&store), first, "the read wrote nothing");
    holder.turn(unknown);
    assert!(!holder.vouching(), "stood down");
    assert!(!elsewhere(&store.dir), "the lock was let go");
    let kept = store.read_tokens_here().expect("kept");
    assert_eq!(
        (kept.read_at, kept.vouched),
        (unknown, false),
        "the turn wrote it at its time"
    );
    assert_eq!(
        on_token(&store, unknown + TOKENS_HERE_FRESH_S),
        Some(NOW + DAY)
    );
    assert_eq!(
        on_token(&store, unknown + TOKENS_HERE_FRESH_S + 1),
        None,
        "aged out"
    );
    holder.turn(unknown + 60);
    assert_eq!(
        store.read_tokens_here().map(|kept| kept.read_at),
        Some(unknown),
        "a turn with no vouch writes nothing"
    );

    let back = unknown + 3 * HOUR;
    assert!(
        store.note_tokens_here(Some(&read()), back).unwrap(),
        "with no vouch, a read is written at its time"
    );
    holder.turn(back);
    assert!(holder.vouching(), "and vouched for again");

    let off = back + HOUR;
    holder.turn(off);
    assert!(
        !holder.vouching(),
        "a turn that noted nothing stands it down"
    );
    assert_eq!(store.read_tokens_here().map(|kept| kept.read_at), Some(off));
}

/// THE LAST HOLDER WRITES THE READING AT ITS TIME AND LETS THE LOCK GO AS IT DROPS, so a reader
/// after the window quits finds the hour a reading always had: the reading's time is the wall
/// clock's at the drop, and another process finds no holder. A holder dropped while another of
/// the same store stands changes nothing.
///
/// NEGATIVE CONTROLS (2026-11-15), each the one mutation of its mutant build that reached this
/// test:
/// - the drop without its `stand_down` failed `written at the drop: 1790000000 not in
///   1794799830..=1794799830` (the hold it removes still lets the lock go);
/// - `stand_down` without its `stamp_tokens_here` failed the same way.
///
/// Each restored, the file touched.
#[test]
fn the_last_holder_writes_the_reading_at_its_time_and_lets_the_lock_go_as_it_drops() {
    let (_dir, store) = scratch("dropped");
    let holder = TokensHereHolder::of(&store).expect("held");
    let second = TokensHereHolder::of(&store).expect("held twice");
    assert!(store.note_tokens_here(Some(&read()), NOW).unwrap());
    holder.turn(NOW);
    drop(second);
    assert!(holder.vouching(), "another holder of the store stands");
    assert_eq!(store.read_tokens_here().map(|kept| kept.read_at), Some(NOW));

    let clock = || i64::try_from(aterm_time::unix_secs().expect("a wall clock")).unwrap();
    let before = clock();
    drop(holder);
    let after = clock();
    assert!(!elsewhere(&store.dir), "the drop let the lock go");
    let kept = store.read_tokens_here().expect("kept");
    assert!(
        (before..=after).contains(&kept.read_at),
        "written at the drop: {} not in {before}..={after}",
        kept.read_at
    );
    assert!(!kept.vouched);
}

/// A HOLDER THAT QUITS STANDS ITS VOUCH DOWN AT ONCE AND VOUCHES NO MORE: the window that stops
/// its keeper quits the hold on its own thread, while the keeper's thread, which still holds it,
/// may be in a pass and may never finish before the process ends. The reading is written at the
/// quit's time and the lock let go, so another process ages it out an hour later; a read after
/// the quit, written at its time as one nobody vouches for, leaves the turn after it vouching no
/// more, and the drop that follows changes nothing a reader judges by.
///
/// NEGATIVE CONTROLS (2026-11-16), each the one mutation of its mutant build that reached this
/// test:
/// - `quit` without its `stand_down` failed `the quit let the lock go`;
/// - a holder vouching whatever `quit` said (`turn`'s `&& !self.quit.load(Ordering::SeqCst)` and
///   `vouch`'s re-check of it under the holds' lock both dropped) failed `a turn after the quit
///   vouches no more`.
///
/// Each restored, the file touched.
#[test]
fn a_holder_that_quits_stands_its_vouch_down_at_once_and_vouches_no_more() {
    let (_dir, store) = scratch("quit");
    let holder = TokensHereHolder::of(&store).expect("held");
    assert!(store.note_tokens_here(Some(&read()), NOW).unwrap());
    holder.turn(NOW);
    assert!(holder.vouching(), "vouched");

    let quit = NOW + 2 * HOUR;
    holder.quit(quit);
    assert!(!elsewhere(&store.dir), "the quit let the lock go");
    assert!(!holder.vouching());
    let kept = store.read_tokens_here().expect("kept");
    assert_eq!(
        (kept.read_at, kept.vouched),
        (quit, false),
        "written at the quit's time"
    );
    assert_eq!(
        on_token(&store, quit + TOKENS_HERE_FRESH_S),
        Some(NOW + DAY)
    );
    assert_eq!(
        on_token(&store, quit + TOKENS_HERE_FRESH_S + 1),
        None,
        "aged out an hour after the quit"
    );

    assert!(
        store.note_tokens_here(Some(&read()), quit + 30).unwrap(),
        "nobody vouches, so the same reading is written at its time"
    );
    holder.turn(quit + 30);
    assert!(
        !holder.vouching() && !elsewhere(&store.dir),
        "a turn after the quit vouches no more"
    );
    drop(holder);
    assert!(!elsewhere(&store.dir));
    assert!(
        store
            .read_tokens_here()
            .is_some_and(|kept| !kept.vouched && kept.read_at >= quit),
        "the drop leaves the reading unvouched"
    );
}

/// Another CLI's store keeps no reading, so it has no holder; Claude Code's is held.
///
/// NEGATIVE CONTROL (2026-11-16): `TokensHereHolder::of` without its `store.cli != Cli::Claude`
/// test failed `another CLI's store has no holder`. Restored, the file touched.
#[test]
fn only_claude_codes_store_is_held() {
    let dir = aterm_tempfile::tempdir().expect("temp dir");
    let codex = Store::at_for(dir.path(), crate::ai::connector::Cli::Codex, Path::new("/"));
    assert!(
        TokensHereHolder::of(&codex).is_none(),
        "another CLI's store has no holder"
    );
    let claude = Store::at(dir.path());
    assert!(
        TokensHereHolder::of(&claude).is_some(),
        "Claude Code's store is held"
    );
}
