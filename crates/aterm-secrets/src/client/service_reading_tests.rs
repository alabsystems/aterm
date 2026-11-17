// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A services reading ([`Reader`]) keeps a standing only while every file it was read from shows
//! what it showed, gives it again with no file opened, and never keeps an answer of the moment:
//! the laws `CL-SERVICE-READING` names.

use super::log_entries_tests::{TwoMacs, two_macs};
use super::service_reading::{At, Reader, Standing, with_hook};
use super::*;
use crate::entry_class::GH_HOSTS;
use std::os::unix::fs::PermissionsExt as _;

/// What the member fixture's vault holds as `credential`.
const CREDENTIAL: &[u8] = b"credential-v1";

/// What the member fixture's vault holds as gh's `hosts.yml`.
const HOSTS: &[u8] = b"hosts-v1";

/// A member of a vault at a URL (`m7` of [`two_macs`]), which holds [`CREDENTIAL`] and gh's
/// [`HOSTS`], each read by name from its store.
fn member() -> TwoMacs {
    two_macs(&[
        (Audience::Everyone, "credential", CREDENTIAL),
        (Audience::Everyone, GH_HOSTS, HOSTS),
    ])
}

/// A Mac that reads a plain vault: an unsigned source file outside its home, which holds a
/// `token` and gh's `hosts.yml`. The source file is the third value.
fn plain() -> (aterm_tempfile::TempDir, Paths, PathBuf) {
    let root = aterm_tempfile::tempdir().unwrap();
    let source = root.path().join("team.secrets");
    fsx::write_atomic(
        &source,
        b"token=inert-token\n~/.config/gh/hosts.yml=inert-value\n",
        0o600,
    )
    .unwrap();
    let paths = Paths::for_home(&root.path().join("reader"));
    let joined = join(
        &paths,
        &JoinOptions {
            source: source.to_str().unwrap().to_owned(),
            host: Some("reader".to_owned()),
            interactive: false,
            wait: Duration::ZERO,
            confirm: &|_| false,
            password: JoinPassword::Given(None),
            fetched: None,
            requested: &|| {},
            say: &|_| {},
        },
    )
    .unwrap();
    assert!(
        matches!(joined, JoinOutcome::Plain { .. }),
        "CONTROL: a plain vault"
    );
    (root, paths, source)
}

/// What a reader with nothing kept derives of `paths` now.
fn fresh(paths: &Paths) -> Standing {
    Reader::new(paths.clone()).read().standing
}

fn is_ready(standing: &Standing) -> bool {
    matches!(standing, Standing::Ready { .. })
}

fn incomplete(standing: &Standing) -> bool {
    *standing == Standing::Blocked(MaterializationBlocker::ApplyIncomplete)
}

/// The files under `paths`' home that `read` opened, with how many times.
fn reads_during<T>(paths: &Paths, read: impl FnOnce() -> T) -> (T, BTreeMap<PathBuf, usize>) {
    let before = fsx::reads_under(&paths.home);
    let answer = read();
    let mut reads = fsx::reads_under(&paths.home);
    reads.retain(|path, count| {
        *count -= before.get(path).copied().unwrap_or(0);
        *count > 0
    });
    (answer, reads)
}

/// `path` written again whole with the bytes and mode it holds: a new file, the same contents.
fn rewrite(path: &Path) {
    let bytes = std::fs::read(path).unwrap();
    let mode = std::fs::symlink_metadata(path)
        .unwrap()
        .permissions()
        .mode()
        & 0o777;
    fsx::write_atomic(path, &bytes, mode).unwrap();
}

/// `paths`' state written again with `change` made to it, under a writer's take of the lock.
fn change_state(paths: &Paths, change: impl FnOnce(&mut State)) {
    let _writer = StateLock::take(paths).unwrap();
    let mut state = load_state(paths).unwrap();
    change(&mut state);
    save_state(paths, &state).unwrap();
}

/// A reader taken through changes to its files, each reading judged against what a reader with
/// nothing kept derives then. Every failure is collected, so a law names each change it fails at.
struct Steps<'p> {
    reader: Reader,
    paths: &'p Paths,
    wrong: Vec<String>,
}

impl<'p> Steps<'p> {
    fn new(paths: &'p Paths) -> Self {
        Self {
            reader: Reader::new(paths.clone()),
            paths,
            wrong: Vec::new(),
        }
    }

    /// After the change `what`: the reading's standing is the fresh one, and it was derived
    /// exactly where `derives`. `want` is the fixture's own check that the change did what it
    /// says to the fresh standing.
    fn after(&mut self, what: &str, derives: bool, want: impl FnOnce(&Standing) -> bool) {
        let reading = self.reader.read();
        let fresh = fresh(self.paths);
        assert!(want(&fresh), "CONTROL: {what} gives {fresh:?}");
        if reading.standing != fresh {
            self.wrong.push(format!(
                "{what}: kept {:?}, fresh {fresh:?}",
                reading.standing
            ));
        }
        if reading.derived != derives {
            self.wrong.push(format!(
                "{what}: derived {}, where it {} derive",
                reading.derived,
                if derives { "must" } else { "need not" }
            ));
        }
    }
}

/// After each kind of change to the files a member's standing is read from, the reading's
/// standing is the one a reader with nothing kept derives, and the reading derived exactly where a
/// file its key stamps moved: `source.toml` written again; the identity replaced and put back; a
/// new envelope published with an entry added; a record appended, and a pass that applies it;
/// `state.toml` rewritten with only `heard` moved (derived again nowhere); the held envelope
/// garbled in place and put back; the state lock removed and made again; this Mac's Commons
/// device key written again. A plain vault's source rewritten, and a pass that applies it, are
/// judged the same way.
///
/// NEGATIVE CONTROLS (2026-11-15, on the merge of main f581bd69910's vault that writes no file
/// under the home), each run and then restored:
/// - `Kept::moved` not comparing the selection inputs: 23 findings, from `source.toml written
///   again: derived false, where it must derive` and `the identity replaced: kept Ready {
///   membership: In { seq: 5 }, entries: 2 }, fresh Blocked(ApplyIncomplete)` to `a pass applies
///   the plain source: derived false, where it must derive`.
/// - `plain_looks` answering `None`: `the plain source rewritten: kept Ready { membership: Plain,
///   entries: 2 }, fresh Blocked(ApplyIncomplete)`.
#[test]
fn a_kept_standing_is_the_fresh_one_after_every_kind_of_change() {
    let _answering = ServersAnswering::set();
    let lab = member();
    let m7 = &lab.m7;
    let mut steps = Steps::new(m7);
    steps.after("the first read", true, is_ready);
    steps.after("a read of unchanged files", false, is_ready);

    rewrite(&m7.source_file());
    steps.after("source.toml written again", true, is_ready);

    let identity = std::fs::read(m7.identity_file()).unwrap();
    let other = Paths::for_home(&lab.root.path().join("other"));
    machine_identity(&other).unwrap();
    fsx::write_atomic(
        &m7.identity_file(),
        &std::fs::read(other.identity_file()).unwrap(),
        0o600,
    )
    .unwrap();
    steps.after("the identity replaced", true, |standing| {
        !is_ready(standing)
    });
    fsx::write_atomic(&m7.identity_file(), &identity, 0o600).unwrap();
    steps.after("the identity put back", true, is_ready);

    {
        let admin = admin_open(&lab.m22).unwrap();
        let mut document = admin.doc.clone();
        document
            .set_entry(&Audience::Everyone, "added", b"added-v1")
            .unwrap();
        admin_publish(&lab.m22, &admin, &document).unwrap();
    }
    sync(m7, None).unwrap();
    steps.after(
        "a new envelope published, with an entry added",
        true,
        is_ready,
    );

    test_put_everyone(m7, "credential", b"credential-v2").unwrap();
    steps.after("a record appended", true, incomplete);
    sync(m7, None).unwrap();
    steps.after("a pass applies the record", true, is_ready);

    change_state(m7, |state| state.heard += 60);
    steps.after(
        "state.toml rewritten with heard alone moved",
        false,
        is_ready,
    );

    let envelope = std::fs::read(m7.cache_file()).unwrap();
    fsx::write_owned_in_place(m7.cache_file(), b"garbled in place");
    steps.after("the held envelope garbled in place", true, |standing| {
        !is_ready(standing)
    });
    fsx::write_owned_in_place(m7.cache_file(), &envelope);
    steps.after("the held envelope put back", true, is_ready);

    std::fs::remove_file(m7.lock_file()).unwrap();
    steps.after("the state lock removed", true, |standing| {
        *standing == Standing::Blocked(MaterializationBlocker::LockMissing)
    });
    drop(test_hold_state_lock(m7).unwrap());
    steps.after("a writer's take made the lock again", true, is_ready);

    let device = m7.dir.join("commons").join("device.age");
    assert!(device.exists(), "CONTROL: m7 holds a Commons device key");
    rewrite(&device);
    steps.after("the Commons device key written again", true, is_ready);
    let mut wrong = std::mem::take(&mut steps.wrong);
    lab.server.stop();

    let (_root, paths, source) = plain();
    let mut steps = Steps::new(&paths);
    steps.after("the plain vault's first read", true, is_ready);
    steps.after("a read of the unchanged plain vault", false, is_ready);
    fsx::write_atomic(
        &source,
        b"token=inert-token-2\n~/.config/gh/hosts.yml=inert-value\n",
        0o600,
    )
    .unwrap();
    steps.after("the plain source rewritten", true, incomplete);
    sync(&paths, None).unwrap();
    steps.after("a pass applies the plain source", true, is_ready);
    wrong.append(&mut steps.wrong);
    assert!(wrong.is_empty(), "{wrong:#?}");
}

/// Every regular file and symlink under `dir`, never through a link.
fn files_under(dir: &Path) -> Vec<PathBuf> {
    let mut files = Vec::new();
    let mut folders = vec![dir.to_path_buf()];
    while let Some(folder) = folders.pop() {
        for entry in std::fs::read_dir(&folder).unwrap() {
            let path = entry.unwrap().path();
            let kind = std::fs::symlink_metadata(&path).unwrap().file_type();
            if kind.is_dir() {
                folders.push(path);
            } else if kind.is_file() || kind.is_symlink() {
                files.push(path);
            }
        }
    }
    files.sort();
    files
}

/// Each regular file and symlink under `root` that the kept standing's key for `paths` does not
/// look at, deleted, and then in its place 64 bytes of garbage (a symlink: one to nothing), each
/// put back after (by `std::fs::copy`, which gives it new times): each change that moved what a
/// reader with nothing kept derives, with what it derived.
fn moved_by_files_outside_the_key(paths: &Paths, root: &Path, scratch: &Path) -> Vec<String> {
    std::fs::create_dir_all(scratch).unwrap();
    let mut reader = Reader::new(paths.clone());
    let baseline = reader.read().standing;
    assert!(is_ready(&baseline), "CONTROL: {baseline:?}");
    let stamped: BTreeSet<PathBuf> = reader
        .stamped_paths()
        .expect("a standing kept")
        .into_iter()
        .collect();
    let outside: Vec<PathBuf> = files_under(root)
        .into_iter()
        .filter(|file| !stamped.contains(file))
        .collect();
    assert!(!outside.is_empty(), "CONTROL: files outside the key");
    let mut moved = Vec::new();
    let mut judge = |what: &str, file: &Path| {
        let now = fresh(paths);
        if now != baseline {
            moved.push(format!("{} {what}: {now:?}", file.display()));
        }
    };
    for (at, file) in outside.iter().enumerate() {
        if std::fs::symlink_metadata(file)
            .unwrap()
            .file_type()
            .is_symlink()
        {
            let target = std::fs::read_link(file).unwrap();
            std::fs::remove_file(file).unwrap();
            judge("deleted", file);
            std::os::unix::fs::symlink(scratch.join("nothing"), file).unwrap();
            judge("linked to nothing", file);
            std::fs::remove_file(file).unwrap();
            std::os::unix::fs::symlink(&target, file).unwrap();
        } else {
            let kept = scratch.join(at.to_string());
            std::fs::copy(file, &kept).unwrap();
            std::fs::remove_file(file).unwrap();
            judge("deleted", file);
            std::fs::write(file, [0x5a; 64]).unwrap();
            judge("garbled", file);
            std::fs::remove_file(file).unwrap();
            std::fs::copy(&kept, file).unwrap();
        }
    }
    assert_eq!(fresh(paths), baseline, "CONTROL: every file put back");
    moved
}

/// No file outside the stamps the kept standing stands under (the selection inputs, a plain
/// vault's source) moves the standing: each, deleted or garbled, leaves what a reader with nothing
/// kept derives as it was. Swept are the files under the home of a member of a vault at a URL (the
/// Commons control journal, the custody claims and locks, the frontier journal's locks, the
/// records' write mark, the wrap gaps), and those beside a plain vault's source and under the home
/// of the Mac that reads it.
///
/// NEGATIVE CONTROL (2026-11-15, on the merge of main f581bd69910's vault that writes no file
/// under the home), run and then restored: with `plain_looks` answering `None`, the sweep deletes
/// and garbles the plain vault's source and fails naming both changes, as `.../team.secrets
/// deleted: Blocked(ApplyIncomplete)`.
#[test]
fn a_file_outside_the_key_never_moves_the_standing() {
    let _answering = ServersAnswering::set();
    let scratch = aterm_tempfile::tempdir().unwrap();
    let lab = member();
    let mut moved =
        moved_by_files_outside_the_key(&lab.m7, &lab.m7.home, &scratch.path().join("member"));
    lab.server.stop();
    let (root, paths, source) = plain();
    fsx::write_owned_in_place(source.with_file_name("notes.txt"), b"beside the source");
    moved.extend(moved_by_files_outside_the_key(
        &paths,
        root.path(),
        &scratch.path().join("plain"),
    ));
    assert!(moved.is_empty(), "{moved:#?}");
}

/// Ten readings of unchanged files derive once, and the nine after the first open no file under
/// the home. After a round that moves only `heard` in `state.toml`, the reading derives nothing
/// and opens only what its receipts are read from and `state.toml` for its standing: `state.toml`
/// twice (for its receipts, then for its standing), and the identity, `source.toml` and the held
/// envelope (the configuration's chain) once each.
///
/// NEGATIVE CONTROL (2026-11-14): with `Reader::read` keeping nothing (`self.kept = None`, the
/// sample before this reader, which derived each time), `ten readings derive once` fails: left
/// 10, right 1.
#[test]
fn an_unchanged_vault_is_derived_once_and_opens_nothing() {
    let _answering = ServersAnswering::set();
    let lab = member();
    let m7 = &lab.m7;
    let mut reader = Reader::new(m7.clone());
    let first = reader.read();
    assert!(is_ready(&first.standing), "CONTROL: {:?}", first.standing);
    let (readings, opened) =
        reads_during(m7, || (1..10).map(|_| reader.read()).collect::<Vec<_>>());
    let derived = usize::from(first.derived) + readings.iter().filter(|r| r.derived).count();
    assert_eq!(derived, 1, "ten readings derive once");
    assert_eq!(reader.derivations(), 1);
    assert!(
        readings
            .iter()
            .all(|reading| reading.standing == first.standing
                && reading.receipts == first.receipts),
        "each gives the first's standing and receipts"
    );
    assert_eq!(
        opened.into_iter().collect::<Vec<_>>(),
        vec![],
        "nine readings of unchanged files"
    );

    change_state(m7, |state| state.heard += 60);
    let (reading, opened) = reads_during(m7, || reader.read());
    assert!(!reading.derived, "a round that moves only heard");
    assert_eq!(reading.standing, first.standing);
    assert_eq!(
        opened.into_iter().collect::<Vec<_>>(),
        vec![
            (m7.identity_file(), 1),
            (m7.source_file(), 1),
            (m7.state_file(), 2),
            (m7.cache_file(), 1),
        ],
        "a round's receipts"
    );
    lab.server.stop();
}

/// An answer of the moment is given but never kept, so the next reading derives again: (i) while
/// a writer holds the state lock, `WriterBusy`, and once it lets go the vault read ready; (ii) a
/// failed read of the history claim the readiness guard's denial check reads, `Revoked` as the
/// services sample has always shown it, and ready at the next reading; a derivation during which (iii) the
/// identity was replaced or (iv) a plain vault's source was: its answer, read from the files as
/// they were, is not kept, and the next reading is the fresh one; (v) a derivation begun while a
/// writer held the state lock, which let go once the derivation had observed the lock and before
/// it read: its answer, ready, is not kept.
///
/// NEGATIVE CONTROLS (2026-11-15, on the merge of main f581bd69910's vault that writes no file
/// under the home), each run and then restored:
/// - (i) the settled observation and the answer's being of the files both dropped from what
///   `derive` keeps by: `(i) the second reading under the writer: Blocked(WriterBusy), derived
///   false` and `(i) once the writer let go: Blocked(WriterBusy), derived false`, with (v)'s
///   below. The answer's being of the files dropped alone passes, the settled observation
///   holding; the settled observation dropped alone fails (v) alone.
/// - (ii) `judgement.faulted` dropped from what `derive` keeps by: `(ii) the reading after the
///   failed read: Blocked(Revoked), derived false`.
/// - (iii) `derive` asking only that the selection inputs before the derivation were read, not
///   that they equal those after: `(iii) the reading after: kept Ready { membership:
///   In { seq: 5 }, entries: 2 }, fresh Blocked(ApplyIncomplete), derived false`.
/// - (iv) the plain source's stamps taken before the guard dropped from it: `(iv) the plain
///   source changed while read, the reading after: kept Ready { membership: Plain, entries: 2 },
///   fresh Blocked(ApplyIncomplete), derived false`.
/// - (v) the settled observation alone dropped from what `derive` keeps by: `(v) the reading
///   after the writer let go: Ready { membership: In { seq: 5 }, entries: 2 }, derived false`.
#[test]
fn a_moment_answer_is_never_kept() {
    let _answering = ServersAnswering::set();
    let lab = member();
    let m7 = &lab.m7;
    let ready = fresh(m7);
    assert!(is_ready(&ready), "CONTROL: {ready:?}");
    let mut wrong = Vec::new();

    let mut reader = Reader::new(m7.clone());
    {
        let _writer = test_hold_state_lock(m7).unwrap();
        for at in ["the first reading", "the second reading"] {
            let reading = reader.read();
            if reading.standing != Standing::Blocked(MaterializationBlocker::WriterBusy)
                || !reading.derived
            {
                wrong.push(format!(
                    "(i) {at} under the writer: {:?}, derived {}",
                    reading.standing, reading.derived
                ));
            }
        }
    }
    let reading = reader.read();
    if reading.standing != ready || !reading.derived {
        wrong.push(format!(
            "(i) once the writer let go: {:?}, derived {}",
            reading.standing, reading.derived
        ));
    }

    let mut reader = Reader::new(m7.clone());
    let mut writer = Some(test_hold_state_lock(m7).unwrap());
    let reading = with_hook(At::Observed, move || drop(writer.take()), || reader.read());
    assert_eq!(
        reading.standing, ready,
        "CONTROL: the read after the writer let go"
    );
    let reading = reader.read();
    if reading.standing != ready || !reading.derived {
        wrong.push(format!(
            "(v) the reading after the writer let go: {:?}, derived {}",
            reading.standing, reading.derived
        ));
    }

    let [claim, ..] = m7.acknowledged_history_files();
    let mut reader = Reader::new(m7.clone());
    fsx::fail_reads(&claim, 1);
    let reading = reader.read();
    fsx::fail_reads(&claim, 0);
    assert_eq!(
        reading.standing,
        Standing::Blocked(MaterializationBlocker::Revoked),
        "CONTROL: a failed read of the history claim"
    );
    let reading = reader.read();
    if reading.standing != ready || !reading.derived {
        wrong.push(format!(
            "(ii) the reading after the failed read: {:?}, derived {}",
            reading.standing, reading.derived
        ));
    }

    let identity = std::fs::read(m7.identity_file()).unwrap();
    let other = Paths::for_home(&lab.root.path().join("other"));
    machine_identity(&other).unwrap();
    let replacement = std::fs::read(other.identity_file()).unwrap();
    let mut reader = Reader::new(m7.clone());
    let identity_file = m7.identity_file();
    let mut replaced = false;
    let reading = with_hook(
        At::Derived,
        move || {
            if !std::mem::replace(&mut replaced, true) {
                fsx::write_atomic(&identity_file, &replacement, 0o600).unwrap();
            }
        },
        || reader.read(),
    );
    assert_eq!(
        reading.standing, ready,
        "CONTROL: the derivation read the old identity"
    );
    let reading = reader.read();
    let now = fresh(m7);
    assert!(!is_ready(&now), "CONTROL: the new identity's: {now:?}");
    if reading.standing != now || !reading.derived {
        wrong.push(format!(
            "(iii) the reading after: kept {:?}, fresh {now:?}, derived {}",
            reading.standing, reading.derived
        ));
    }
    fsx::write_atomic(&m7.identity_file(), &identity, 0o600).unwrap();
    lab.server.stop();

    let (_root, paths, source) = plain();
    wrong.extend(changed_while_derived(
        "(iv) the plain source",
        &paths,
        &source,
    ));
    assert!(wrong.is_empty(), "{wrong:#?}");
}

/// A reader with nothing kept, whose derivation reads `paths` while it stands ready and then sees
/// `file` written in place with other bytes, before its stamps are taken again: what is wrong
/// with the reading after it, which must derive again and give the fresh standing. `file` holds
/// what it held before at the end.
fn changed_while_derived(what: &str, paths: &Paths, file: &Path) -> Vec<String> {
    let held = std::fs::read(file).unwrap();
    let mut reader = Reader::new(paths.clone());
    let path = file.to_path_buf();
    let mut changed = false;
    let reading = with_hook(
        At::Derived,
        move || {
            if !std::mem::replace(&mut changed, true) {
                fsx::write_owned_in_place(&path, b"changed while read");
            }
        },
        || reader.read(),
    );
    assert!(
        is_ready(&reading.standing),
        "CONTROL: {what}: the derivation read it as it was: {:?}",
        reading.standing
    );
    let reading = reader.read();
    let now = fresh(paths);
    assert!(incomplete(&now), "CONTROL: {what} changed: {now:?}");
    let wrong = (reading.standing != now || !reading.derived).then(|| {
        format!(
            "{what} changed while read, the reading after: kept {:?}, fresh {now:?}, derived {}",
            reading.standing, reading.derived
        )
    });
    fsx::write_owned_in_place(file, &held);
    wrong.into_iter().collect()
}

/// `paths`' state given a 204 response at 2 000 ms and a refresh that `outcome` (`succeeded` or
/// `failed`) at `refresh_ms`, bound to its configuration and identity as the recorders bind them,
/// written by `writer`'s take of the state lock.
fn record_receipts(paths: &Paths, _writer: &StateLock, refresh_ms: u64, outcome: &str) {
    let configured = config(paths).unwrap().unwrap();
    let configuration = hex(&network_observation::configuration_binding(&configured));
    let recipient = hex(&read_identity(paths).unwrap().unwrap().recipient().0);
    let receipts: aterm_toml::Table = aterm_toml::from_str(&format!(
        "[http204_response]\nversion = 1\nconfiguration = \"{configuration}\"\n\
         recipient = \"{recipient}\"\nresponded_at_ms = 2000\npersisted_at_ms = 2000\n\
         [network_refresh]\nversion = 1\nconfiguration = \"{configuration}\"\n\
         recipient = \"{recipient}\"\ncompleted_at_ms = {refresh_ms}\noutcome = \"{outcome}\"\n",
    ))
    .unwrap();
    let mut state = load_state(paths).unwrap();
    state.http204_response = http204_observation::Receipt::parse(&receipts);
    state.network_refresh = network_refresh::Receipt::parse(&receipts);
    assert!(
        state.http204_response.is_some() && state.network_refresh.is_some(),
        "CONTROL: each receipt parses"
    );
    save_state(paths, &state).unwrap();
}

/// The 204 response at 2 000 ms and the refresh at `refresh_ms` that ended as `outcome`.
fn shows(
    reading: &service_reading::Reading,
    refresh_ms: u64,
    outcome: VaultNetworkRefreshOutcome,
) -> bool {
    reading.receipts.is_some_and(|receipts| {
        receipts
            .http204
            .is_some_and(|response| response.responded_at_ms() == 2_000)
            && receipts.refresh.is_some_and(|refresh| {
                refresh.completed_at_ms() == refresh_ms && refresh.outcome() == outcome
            })
    })
}

/// A reading's receipts stand through a writer: while one holds the state lock, having renamed
/// in a `state.toml` with a newer refresh, the reading gives the receipts it last read (the 204
/// response and the refresh that succeeded), never none or an unreadable history, and the
/// standing it kept, which those receipts leave as it was. Once the writer lets go, the next
/// reading shows the newer refresh.
///
/// NEGATIVE CONTROL (2026-11-14): with a busy observation of the receipts giving `None` (an
/// unreadable history), it fails: `under the writer, the receipts last read: None`.
#[test]
fn receipts_stand_through_a_writer_and_follow_each_rewrite() {
    let _answering = ServersAnswering::set();
    let lab = member();
    let m7 = &lab.m7;
    {
        let writer = StateLock::take(m7).unwrap();
        record_receipts(m7, &writer, 3_000, "succeeded");
    }
    let mut reader = Reader::new(m7.clone());
    let first = reader.read();
    assert!(is_ready(&first.standing), "CONTROL: {:?}", first.standing);
    assert!(
        shows(&first, 3_000, VaultNetworkRefreshOutcome::Succeeded),
        "the receipts as recorded: {:?}",
        first.receipts
    );

    {
        let writer = StateLock::take(m7).unwrap();
        record_receipts(m7, &writer, 4_000, "failed");
        let reading = reader.read();
        assert!(
            shows(&reading, 3_000, VaultNetworkRefreshOutcome::Succeeded),
            "under the writer, the receipts last read: {:?}",
            reading.receipts
        );
        assert_eq!(reading.standing, first.standing, "under the writer");
        assert!(
            !reading.derived,
            "the standing's files stand under the writer"
        );
    }
    let reading = reader.read();
    assert!(
        shows(&reading, 4_000, VaultNetworkRefreshOutcome::Failed),
        "once the writer let go, the newer refresh: {:?}",
        reading.receipts
    );
    assert_eq!(reading.standing, first.standing);
    assert!(!reading.derived);
    lab.server.stop();
}

/// A receipts read whose held envelope cannot be read, where the vault's signer was reached by a
/// handoff and `source.toml` names no `followed` (the chain every receipt is bound with is the
/// envelope's), reads no receipts: the reading gives none (an unreadable history) and keeps
/// nothing under its key, and the next reading reads them again and shows the refresh that failed.
///
/// NEGATIVE CONTROL (2026-11-15), run and then restored: with the receipts read taking the
/// configuration from `config` (which takes a failed read of the held envelope as no chain), it
/// fails: `the reading during the fault: Some(Receipts { materialization: None, http204: None,
/// refresh: None })`, then `the reading after: Some(Receipts { materialization: None, http204:
/// None, refresh: None })`, the empty receipts kept under a key that stood.
#[test]
fn a_held_envelope_that_cannot_be_read_keeps_no_receipts() {
    let _answering = ServersAnswering::set();
    let lab = two_macs(&[]);
    let m7 = &lab.m7;
    let opened = admin_open(&lab.m22).unwrap();
    admin_rotate(&lab.m22, &opened, &opened.doc.clone()).unwrap();
    save_records(m7, &load_records(&lab.m22).unwrap().encode()).unwrap();
    sync(m7, None).unwrap();
    let mut configured = config(m7).unwrap().expect("m7's vault");
    assert!(
        !std::mem::take(&mut configured.followed).is_empty(),
        "CONTROL: the rotated signer was reached by a handoff"
    );
    save_config(m7, &configured).unwrap();
    assert!(
        !config(m7).unwrap().unwrap().followed.is_empty(),
        "CONTROL: the chain is the held envelope's"
    );
    {
        let writer = StateLock::take(m7).unwrap();
        record_receipts(m7, &writer, 3_000, "failed");
    }
    let mut reader = Reader::new(m7.clone());
    let first = reader.read();
    assert!(
        shows(&first, 3_000, VaultNetworkRefreshOutcome::Failed),
        "CONTROL: the receipts as recorded: {:?}",
        first.receipts
    );

    let mut wrong = Vec::new();
    change_state(m7, |state| state.heard += 60);
    fsx::fail_reads(&m7.cache_file(), 1);
    let during = reader.read();
    fsx::fail_reads(&m7.cache_file(), 0);
    if during.receipts.is_some() {
        wrong.push(format!(
            "the reading during the fault: {:?}",
            during.receipts
        ));
    }
    let after = reader.read();
    if !shows(&after, 3_000, VaultNetworkRefreshOutcome::Failed) {
        wrong.push(format!("the reading after: {:?}", after.receipts));
    }
    assert!(wrong.is_empty(), "{wrong:#?}");
    lab.server.stop();
}
