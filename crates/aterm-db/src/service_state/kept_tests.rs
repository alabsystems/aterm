// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! An accounts sample keeps a standing only while every file it was read from shows what it
//! showed, gives it again stamped with its own times, and never keeps an answer of the moment; an
//! observer derives each service once while nothing moves.

use super::*;
use crate::{
    accounts::proposals::Proposals,
    catalog::{Descriptor, Directory},
    database::Change,
    service_state::{
        Derivations, Observer, control_busy, out_of_time,
        tests::{accounts, stages_reached},
    },
    table::{Session, store::Store, tests::Lab},
};
use aterm_secrets::commons::roster::Role;
use std::{
    cell::RefCell,
    collections::BTreeSet,
    time::{Duration, Instant},
};

/// Where in a derivation a test's hook runs ([`at_next`]).
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Hook {
    /// Once the paths it reads are listed, before their key is taken.
    Listed,
    /// Once the opened home's vault state lock is observed, before the read.
    Observed,
    /// Once it has read, before the paths are listed and their key taken again.
    Derived,
}

/// A test's hook and the point it runs at.
type Set = (Hook, Box<dyn FnMut()>);

std::thread_local! {
    /// What a test runs at a point of this thread's next derivation ([`at_next`]).
    static HOOK: RefCell<Option<Set>> = const { RefCell::new(None) };
}

/// Run the hook a test set for this point of this thread's next derivation, once ([`at_next`]).
pub(super) fn run_hook(at: Hook) {
    let hook = HOOK.with(|slot| {
        let mut slot = slot.borrow_mut();
        if slot.as_ref().is_some_and(|(point, _)| *point == at) {
            slot.take()
        } else {
            None
        }
    });
    if let Some((_, mut hook)) = hook {
        hook();
    }
}

/// Run `hook` once on this thread at the point `at` of the next derivation: a writer that
/// changes a file, or lets go of a lock, while a derivation reads.
fn at_next(at: Hook, hook: impl FnMut() + 'static) {
    HOOK.with(|slot| *slot.borrow_mut() = Some((at, Box::new(hook))));
}

/// `/bin/chmod` (its stand-in) run with `words` on `path`: an access list granted (`+a`) or
/// taken away (`-N`).
#[cfg(target_os = "macos")]
fn acl(path: &Path, words: &[&str]) {
    let chmod = aterm_uds::deadline::stand_in(Path::new("/bin/chmod"));
    let ran = aterm_uds::spawnfd::command(chmod)
        .args(words)
        .arg(path)
        .status()
        .expect("run chmod");
    assert!(ran.success(), "CONTROL: chmod {words:?} {}", path.display());
}

/// `path` given the mode bits `mode`.
fn chmod(path: &Path, mode: u32) {
    use std::os::unix::fs::PermissionsExt as _;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode)).unwrap();
}

/// A replica of the catalog of `lab`'s first home in its recovery region.
fn replicate_catalog(lab: &Lab, paths: &Paths) {
    let bootstrap = lab.bootstrap(0);
    let session = Session::open(&bootstrap).unwrap();
    let directory = Directory::new(paths, bootstrap.trust());
    let scope = directory.catalog_scope();
    let catalog = Store::open(&session, &directory.path(scope).unwrap(), scope).unwrap();
    let wires: Vec<Vec<u8>> = catalog
        .inventory(&session)
        .unwrap()
        .map(|id| catalog.wire(&session, id).unwrap().unwrap().to_vec())
        .collect();
    drop(catalog);
    let recovering = paths
        .dir
        .join("commons/recovery/tables")
        .join(aterm_codec::hex::encode_lower(&scope.digest()));
    let mut replica = Store::join_recovery(&session, &recovering, scope).unwrap();
    for wire in &wires {
        replica.ingest(&session, wire).unwrap();
    }
}

/// A read's own bound where a test holds it to none: the hang detector.
fn hang_detector() -> Instant {
    Instant::now() + aterm_spec::wait::MUST_HAPPEN
}

fn account(key: &str) -> Change {
    Change {
        collection: "accounts".into(),
        key: key.into(),
        value: Some(aterm_json::json!({"provider":"claude", "method":"oauth"})),
    }
}

/// A lab of one writer whose home holds the account table with one proposal, and the
/// configuration its sharing policy is read from (none there yet).
fn usable() -> (Lab, PathBuf) {
    let lab = Lab::new(&[Role::Writer]);
    let proposals = Proposals::at(lab.paths[0].clone());
    proposals.initialize().unwrap();
    proposals.write([1; 16], vec![account("one")]).unwrap();
    let config = lab.paths[0].home.join("config.toml");
    (lab, config)
}

fn cached(state: &ServiceState) -> bool {
    state.availability == ServiceAvailability::Cached
}

fn unavailable(state: &ServiceState) -> bool {
    state.availability == ServiceAvailability::Unavailable
}

/// A sharing policy that excludes the account table of `lab`'s first home.
fn excluding(lab: &Lab) -> Vec<u8> {
    let bootstrap = lab.bootstrap(0);
    let descriptor =
        Descriptor::application(bootstrap.trust(), "accounts", "proposals", 1).unwrap();
    format!(
        "[commons]\nexclude_tables = ['{}']\n",
        aterm_codec::hex::encode_lower(&descriptor.scope.digest())
    )
    .into_bytes()
}

/// An accounts sample taken through changes to its files, each sample judged against what a
/// sample with nothing kept derives then. Every failure is collected, so a law names each change
/// it fails at.
struct Steps<'p> {
    kept: Option<KeptAccounts>,
    paths: &'p Paths,
    config: &'p Path,
    now_ms: u64,
    wrong: Vec<String>,
}

impl<'p> Steps<'p> {
    fn new(paths: &'p Paths, config: &'p Path) -> Self {
        Self {
            kept: None,
            paths,
            config,
            now_ms: 1_000_000,
            wrong: Vec::new(),
        }
    }

    /// After the change `what`, a second later: the sample's state is the fresh one, and it was
    /// derived exactly where `derives`. `want` is the fixture's own check that the change did
    /// what it says to the fresh state.
    fn after(&mut self, what: &str, derives: bool, want: impl FnOnce(&ServiceState) -> bool) {
        self.now_ms += 1000;
        let sampled = sample(&mut self.kept, self.paths, self.config, self.now_ms);
        let fresh = accounts(self.paths, self.config, self.now_ms);
        assert!(want(&fresh), "CONTROL: {what} gives {fresh:?}");
        if sampled.state != fresh {
            self.wrong
                .push(format!("{what}: kept {:?}, fresh {fresh:?}", sampled.state));
        }
        if sampled.derived != derives {
            self.wrong.push(format!(
                "{what}: derived {}, where it {} derive",
                sampled.derived,
                if derives { "must" } else { "need not" }
            ));
        }
    }
}

/// After each kind of change to the files an accounts standing is read from, the sample's state
/// is the one a sample with nothing kept derives, and it derived exactly where a file its key
/// stamps moved: a new account proposal; a control-journal append (a roster rotation); a file
/// made among the saved cutoffs; the sharing policy written to exclude the account table, and
/// removed; the configuration made a symlink to a policy that excludes it, the link's target
/// rewritten to admit it, the link replaced by one to another file that excludes it, and the
/// link removed; a recovery region's replica of the catalog; `commons`, and then the vault folder,
/// opened to mode 0755 and given 0700 again, and (on macOS) given an access-list grant, which
/// moves no mode bit, and the grant taken away, each of which the custody read refuses; a
/// declaration that conflicts with the account table's. And over a succession: the original home
/// read, its succession completed (the read opens the successor), and a proposal made there.
///
/// NEGATIVE CONTROL (2026-11-15), run and then restored: with `Listed::of` listing no folder (no
/// [`folder_looks`] in the key), `commons opened to mode 0755: kept ServiceState { availability:
/// Cached, … }, fresh ServiceState { availability: Unknown, …, detail: "Commons custody could not
/// be verified" … }` and `commons opened to mode 0755: derived false, where it must derive`, and
/// so at `an access list granted on commons` and `an access list granted on the vault folder`,
/// with `derived false, where it must derive` at `commons given mode 0700 again` and at each grant
/// taken away; the vault folder's mode alone still moves the key without them, by the desk's
/// stamp of its mode.
///
/// NEGATIVE CONTROLS (2026-11-14), each run and then restored:
/// - `TABLE_TREES` without `commons/tables` and `commons/table-claims`: `a new account proposal:
///   kept ServiceState { availability: Cached, …, revision: Some("1 operations retained; 0
///   withheld"), detail: "1 account proposals, …" … }, fresh ServiceState { availability:
///   Cached, …, revision: Some("2 operations retained; 0 withheld"), detail: "2 account
///   proposals, …" … }` and `a new account proposal: derived false, where it must derive`, and
///   so at `a declaration that conflicts` (fresh `Unavailable`, `Account catalog declarations
///   conflict; automatic access is withheld`) and at `a proposal made at the successor`. Without
///   `commons/tables` alone this law passes: a table's append also rewrites its claim, which the
///   claims tree stamps; `a_file_outside_the_accounts_key_never_moves_the_standing` is the law
///   that fails then.
/// - `config_looks` giving only the configuration path, not the paths its link leads to: `the
///   link's target rewritten to admit the table: kept ServiceState { availability: Unavailable,
///   …, detail: "Account catalog is excluded by local sharing policy" … }, fresh ServiceState {
///   availability: Cached, … }` and `the link's target rewritten to admit the table: derived
///   false, where it must derive`, and at no other change.
#[test]
fn a_kept_account_standing_is_the_fresh_one_after_every_kind_of_change() {
    let (lab, config) = usable();
    let paths = &lab.paths[0];
    let excludes = excluding(&lab);
    let mut steps = Steps::new(paths, &config);
    steps.after("the first read", true, cached);
    steps.after("a read of unchanged files", false, cached);

    Proposals::at(paths.clone())
        .write([2; 16], vec![account("two")])
        .unwrap();
    steps.after("a new account proposal", true, |state| {
        cached(state) && state.detail.starts_with("2 account proposals")
    });

    lab.rotate(&[Role::Writer]);
    steps.after("a control-journal append", true, cached);

    let cutoffs = paths.dir.join("commons/table-cutoffs");
    aterm_uds::private_file::create_private_dir(&cutoffs).unwrap();
    fsx::write_atomic(&cutoffs.join("saved"), b"a removal's frontier", 0o600).unwrap();
    steps.after("a file among the saved cutoffs", true, cached);

    fsx::write_atomic(&config, &excludes, 0o600).unwrap();
    steps.after("the sharing policy excludes the table", true, unavailable);
    std::fs::remove_file(&config).unwrap();
    steps.after("the sharing policy removed", true, cached);

    let policy = paths.home.join("policy.toml");
    fsx::write_atomic(&policy, &excludes, 0o600).unwrap();
    std::os::unix::fs::symlink(&policy, &config).unwrap();
    steps.after(
        "the configuration a link to a policy that excludes the table",
        true,
        unavailable,
    );
    fsx::write_atomic(&policy, b"[commons]\nenabled = true\n", 0o600).unwrap();
    steps.after(
        "the link's target rewritten to admit the table",
        true,
        cached,
    );
    let other = paths.home.join("policy-2.toml");
    fsx::write_atomic(&other, &excludes, 0o600).unwrap();
    let replacing = paths.home.join("config.toml.new");
    std::os::unix::fs::symlink(&other, &replacing).unwrap();
    std::fs::rename(&replacing, &config).unwrap();
    steps.after(
        "the link replaced by one to another file",
        true,
        unavailable,
    );
    std::fs::remove_file(&config).unwrap();
    steps.after("the link removed", true, cached);

    replicate_catalog(&lab, paths);
    steps.after("a recovery region's replica of the catalog", true, cached);

    let commons = paths.dir.join("commons");
    for (what, folder) in [("commons", &commons), ("the vault folder", &paths.dir)] {
        chmod(folder, 0o755);
        steps.after(&format!("{what} opened to mode 0755"), true, |state| {
            !cached(state)
        });
        chmod(folder, 0o700);
        steps.after(&format!("{what} given mode 0700 again"), true, cached);
        #[cfg(target_os = "macos")]
        {
            acl(folder, &["+a", "everyone allow list,search"]);
            steps.after(
                &format!("an access list granted on {what}"),
                true,
                |state| !cached(state),
            );
            acl(folder, &["-N"]);
            steps.after(&format!("the grant on {what} taken away"), true, cached);
        }
    }

    {
        let bootstrap = lab.bootstrap(0);
        let session = Session::open(&bootstrap).unwrap();
        let directory = Directory::new(paths, bootstrap.trust());
        let scope = directory.catalog_scope();
        let mut incompatible =
            Descriptor::application(bootstrap.trust(), "accounts", "proposals", 1).unwrap();
        incompatible.version = 2;
        Store::open(&session, &directory.path(scope).unwrap(), scope)
            .unwrap()
            .commit_catalog(&session, &incompatible.encode().unwrap())
            .unwrap();
    }
    steps.after("a declaration that conflicts", true, unavailable);
    let mut wrong = std::mem::take(&mut steps.wrong);

    let mut lab = Lab::new(&[Role::Writer, Role::Writer]);
    let original = lab.paths[0].clone();
    Proposals::at(original.clone())
        .write([1; 16], vec![account("one")])
        .unwrap();
    let config = original.home.join("config.toml");
    let mut steps = Steps::new(&original, &config);
    steps.after("the original home's first read", true, cached);
    steps.after("a read of the unchanged original home", false, cached);
    assert_eq!(crate::generation::tests::succeed(&mut lab), config);
    steps.after("its succession completed", true, cached);
    steps.after("a read of the unchanged successor", false, cached);
    Proposals::at(lab.paths[1].clone())
        .write([2; 16], vec![account("two")])
        .unwrap();
    steps.after("a proposal made at the successor", true, |state| {
        cached(state) && state.detail.starts_with("2 account proposals")
    });
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

/// Each regular file and symlink under `root` that the kept accounts standing's key for `paths`
/// does not look at, deleted, and then in its place 64 bytes of garbage (a symlink: one to
/// nothing), each put back after (by `std::fs::copy`, which gives it new times): each change that
/// moved what a sample with nothing kept derives, with what it derived.
fn moved_by_files_outside_the_key(
    paths: &Paths,
    config: &Path,
    root: &Path,
    scratch: &Path,
) -> Vec<String> {
    std::fs::create_dir_all(scratch).unwrap();
    let mut kept = None;
    let baseline = sample(&mut kept, paths, config, 1000).state;
    assert!(cached(&baseline), "CONTROL: {baseline:?}");
    let stamped: BTreeSet<PathBuf> = kept
        .as_ref()
        .expect("a standing kept")
        .listed
        .stamped_paths()
        .into_iter()
        .collect();
    let outside: Vec<PathBuf> = files_under(root)
        .into_iter()
        .filter(|file| !stamped.contains(file))
        .collect();
    assert!(!outside.is_empty(), "CONTROL: files outside the key");
    let mut moved = Vec::new();
    let mut judge = |what: &str, file: &Path| {
        let now = accounts(paths, config, 1000);
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
            let held = scratch.join(at.to_string());
            std::fs::copy(file, &held).unwrap();
            std::fs::remove_file(file).unwrap();
            judge("deleted", file);
            std::fs::write(file, [0x5a; 64]).unwrap();
            judge("garbled", file);
            std::fs::remove_file(file).unwrap();
            std::fs::copy(&held, file).unwrap();
        }
    }
    assert_eq!(
        accounts(paths, config, 1000),
        baseline,
        "CONTROL: every file put back"
    );
    moved
}

/// No file outside the stamps a kept accounts standing stands under moves it: each, deleted or
/// garbled, leaves what a sample with nothing kept derives as it was. Swept are the files under
/// the home of a writer whose account table holds a proposal and whose catalog has a replica in
/// the recovery region, and under the original home of a writer whose succession completed
/// (its successor's home inside it).
///
/// NEGATIVE CONTROL (2026-11-14), run and then restored: with `TABLE_TREES` without
/// `commons/tables`, the sweep deletes and garbles the tables' files and fails naming each, 18 in
/// all over both homes: `…/commons/tables/<table>/table.anchor deleted: ServiceState {
/// availability: Unknown, …, detail: "Retained account storage could not be opened" … }`, and so
/// for each `table.astream` and `table.checkpoint`, garbled as well as deleted, and 6 of them with
/// `detail: "Shared catalog metadata could not be read"`.
#[test]
fn a_file_outside_the_accounts_key_never_moves_the_standing() {
    let scratch = aterm_tempfile::tempdir().unwrap();
    let (lab, config) = usable();
    let paths = &lab.paths[0];
    replicate_catalog(&lab, paths);
    let mut moved =
        moved_by_files_outside_the_key(paths, &config, &paths.home, &scratch.path().join("one"));

    let (_lab, original, config) = succeeded();
    moved.extend(moved_by_files_outside_the_key(
        &original,
        &config,
        &original.home,
        &scratch.path().join("successor"),
    ));
    assert!(moved.is_empty(), "{moved:#?}");
}

/// A lab of two writers whose original home holds one proposal and whose succession completed
/// (its successor's home inside it): the lab, the original home, and the configuration its
/// selection names.
fn succeeded() -> (Lab, Paths, PathBuf) {
    let mut lab = Lab::new(&[Role::Writer, Role::Writer]);
    let original = lab.paths[0].clone();
    Proposals::at(original.clone())
        .write([1; 16], vec![account("one")])
        .unwrap();
    let config = crate::generation::tests::succeed(&mut lab);
    (lab, original, config)
}

/// Each directory under `root` (not `root`) that the kept accounts standing's key for `paths`
/// looks at neither whole nor privately, opened to mode 0777, which every owner-only verdict
/// refuses, and given its mode back: each change that moved what a sample with nothing kept
/// derives, with what it derived.
fn moved_by_folders_outside_the_key(paths: &Paths, config: &Path, root: &Path) -> Vec<String> {
    use std::os::unix::fs::MetadataExt as _;
    let mut kept = None;
    let baseline = sample(&mut kept, paths, config, 1000).state;
    assert!(cached(&baseline), "CONTROL: {baseline:?}");
    let stamped: BTreeSet<PathBuf> = kept
        .as_ref()
        .expect("a standing kept")
        .listed
        .stamped_paths()
        .into_iter()
        .collect();
    let mut folders = Vec::new();
    let mut walking = vec![root.to_path_buf()];
    while let Some(folder) = walking.pop() {
        for entry in std::fs::read_dir(&folder).unwrap() {
            let path = entry.unwrap().path();
            if std::fs::symlink_metadata(&path).unwrap().is_dir() {
                walking.push(path.clone());
                if !stamped.contains(&path) {
                    folders.push(path);
                }
            }
        }
    }
    folders.sort();
    assert!(!folders.is_empty(), "CONTROL: folders outside the key");
    let mut moved = Vec::new();
    for folder in &folders {
        let mode = std::fs::symlink_metadata(folder).unwrap().mode() & 0o7777;
        chmod(folder, 0o777);
        let now = accounts(paths, config, 1000);
        chmod(folder, mode);
        if now != baseline {
            moved.push(format!("{} opened to 0777: {now:?}", folder.display()));
        }
    }
    assert_eq!(
        accounts(paths, config, 1000),
        baseline,
        "CONTROL: every mode put back"
    );
    moved
}

/// No directory outside the folders and trees a kept accounts standing looks at moves it: each
/// opened to mode 0777, which the owner-only rule refuses (strict and owned alike, the mode being
/// judged after the kind, the owner and the access list), leaves what a sample with nothing kept
/// derives as it was. So every directory the read holds to that rule is looked at whole (a tree's,
/// whose access-list grant moves its change time) or privately (whose verdict is taken). Swept
/// are the directories under the home of a writer whose catalog has a replica in the recovery
/// region, and under the original home of a writer whose succession completed.
///
/// NEGATIVE CONTROL (2026-11-15), run and then restored: with `Listed::of` listing no folder (no
/// [`folder_looks`] in the key), it fails naming the vault folder and `commons` of the writer's
/// home, of the original home and of its successor, six in all, as `.../m1/.config/aterm/secrets/
/// commons opened to 0777: ServiceState { availability: Unknown, …, detail: "Commons custody
/// could not be verified" … }` (the original home's two read `Commons writer recovery is
/// pending`).
#[test]
fn a_folder_outside_the_accounts_key_never_moves_the_standing() {
    let (lab, config) = usable();
    let paths = &lab.paths[0];
    replicate_catalog(&lab, paths);
    let mut moved = moved_by_folders_outside_the_key(paths, &config, &paths.home);
    let (_lab, original, config) = succeeded();
    moved.extend(moved_by_folders_outside_the_key(
        &original,
        &config,
        &original.home,
    ));
    assert!(moved.is_empty(), "{moved:#?}");
}

/// Ten samples of unchanged files derive each service once: the accounts read admits custody,
/// observes its session and opens the account table once, and the secrets reader derives once.
/// Nothing moved, so no part of a key is counted.
///
/// NEGATIVE CONTROL (2026-11-14, run again 2026-11-15 once `Derivations` counted samples, and
/// again on the merge of main f581bd69910, whose vault writes no file under the home and so leaves
/// no delivered part to count), run and then restored: with the observer keeping nothing (each
/// sample starting from a new `Reader` and no kept accounts standing, the sample before the
/// observer), it fails: `left: Derivations { samples: 10, secrets: 10, accounts: 10, moved: [0,
/// 0, 0, 0, 0] }`; and `a_derivation_moves_none_of_its_own_key` fails at its second sample:
/// `left: (2, 2)`.
#[test]
fn an_unchanged_home_is_derived_once_across_ten_samples() {
    let (lab, config) = usable();
    let paths = &lab.paths[0];
    let mut observer = Observer::new(paths.clone(), config);
    let before = stages_reached();
    let at = Instant::now();
    for n in 0..10u64 {
        observer.sample_with(move || {
            (
                at + Duration::from_secs(10 * n),
                Some(1_000_000 + 10_000 * n),
            )
        });
    }
    assert_eq!(
        observer.derivations(),
        Derivations {
            samples: 10,
            secrets: 1,
            accounts: 1,
            moved: [0; 5],
        }
    );
    let after = stages_reached();
    assert_eq!(
        [0, 1, 2].map(|stage| after[stage] - before[stage]),
        [1, 1, 1],
        "custody admitted, the session observed and the account table opened once"
    );
}

/// An answer of the moment is given but never kept, so the next sample derives again: (i) a
/// holder of this home's Commons custody, `control_busy`, and once it lets go the table read
/// Cached; (ii) a read whose own time was already spent, `out_of_time`, and a read with time
/// after it; (iii) a read during which the sharing policy came to exclude the table: its answer,
/// read from the files as they were, is not kept, and the next sample is the fresh one; (iv) a
/// read begun while a writer held the vault's state lock, which let go once the derivation had
/// observed the lock and before it read: its answer, Cached, is not kept; (v) a read during which
/// a succession completed, after the walk listed the homes and before the key was taken: its
/// answer, the successor's, is not kept, and after a proposal made at the successor the next
/// sample is the fresh one.
///
/// NEGATIVE CONTROLS (2026-11-15), run together and then restored, each failing its own case:
/// - `derive` keeping where its observation of the state lock met a writer (`unwritten` dropped
///   from what it keeps by): `(iv) the sample after the writer let go: ServiceState {
///   availability: Cached, … }, derived false`.
/// - `derive` keeping without listing the paths again (`relisted` dropped from what it keeps
///   by): `(v) the sample after: …, derived false` and `(v) after a proposal at the successor:
///   kept ServiceState { …, detail: "1 account proposals, …" … }, fresh ServiceState { …,
///   detail: "2 account proposals, …" … }`.
///
/// NEGATIVE CONTROLS (2026-11-14), each run and then restored:
/// - `derive` keeping a `Settled::No` answer as it keeps a settled one: `(i) once the holder let
///   go: ServiceState { availability: Unknown, …, detail: "Commons custody, control history or
///   the account table is busy; waiting to check account access" … }, derived false` and `(ii)
///   with time again: ServiceState { availability: Unknown, …, detail: "Ran out of time before
///   account access could be read; this machine may be heavily loaded" … }, derived false`.
/// - `derive` asking only that the key before the read was taken, not that it equals the key
///   after: `(iii) the sample after: kept ServiceState { availability: Cached, … }, fresh
///   ServiceState { availability: Unavailable, …, detail: "Account catalog is excluded by local
///   sharing policy" … }, derived false`.
#[test]
fn busy_custody_and_out_of_time_are_never_kept() {
    let (lab, config) = usable();
    let paths = &lab.paths[0];
    let mut wrong = Vec::new();

    let mut kept = None;
    let custody = aterm_uds::private_file::PrivateLock::take(
        &paths.dir,
        "commons.lock",
        Duration::ZERO,
        "held",
    )
    .unwrap();
    let busy = sample_until(
        &mut kept,
        paths,
        &config,
        1000,
        &Instant::now,
        hang_detector(),
    );
    drop(custody);
    assert_eq!(busy.state, control_busy(1000), "CONTROL: a holder met");
    let next = sample(&mut kept, paths, &config, 2000);
    if !cached(&next.state) || !next.derived {
        wrong.push(format!(
            "(i) once the holder let go: {:?}, derived {}",
            next.state, next.derived
        ));
    }

    let mut kept = None;
    let spent = sample_until(
        &mut kept,
        paths,
        &config,
        3000,
        &hang_detector,
        Instant::now(),
    );
    assert_eq!(spent.state, out_of_time(3000), "CONTROL: no time to read");
    let next = sample(&mut kept, paths, &config, 4000);
    if !cached(&next.state) || !next.derived {
        wrong.push(format!(
            "(ii) with time again: {:?}, derived {}",
            next.state, next.derived
        ));
    }

    let mut kept = None;
    let mut writer = Some(
        aterm_uds::private_file::PrivateLock::take(&paths.dir, ".lock", Duration::ZERO, "held")
            .unwrap(),
    );
    at_next(Hook::Observed, move || drop(writer.take()));
    let begun = sample(&mut kept, paths, &config, 4500);
    assert!(
        cached(&begun.state),
        "CONTROL: the read after the writer let go: {:?}",
        begun.state
    );
    let next = sample(&mut kept, paths, &config, 4600);
    if !cached(&next.state) || !next.derived {
        wrong.push(format!(
            "(iv) the sample after the writer let go: {:?}, derived {}",
            next.state, next.derived
        ));
    }

    let mut kept = None;
    let excludes = excluding(&lab);
    let policy = config.clone();
    at_next(Hook::Derived, move || {
        fsx::write_atomic(&policy, &excludes, 0o600).unwrap();
    });
    let raced = sample(&mut kept, paths, &config, 5000);
    assert!(
        cached(&raced.state),
        "CONTROL: the read saw the policy as it was: {:?}",
        raced.state
    );
    let next = sample(&mut kept, paths, &config, 6000);
    let now = accounts(paths, &config, 6000);
    assert!(unavailable(&now), "CONTROL: the policy excludes: {now:?}");
    if next.state != now || !next.derived {
        wrong.push(format!(
            "(iii) the sample after: kept {:?}, fresh {now:?}, derived {}",
            next.state, next.derived
        ));
    }

    let lab = std::rc::Rc::new(RefCell::new(Lab::new(&[Role::Writer, Role::Writer])));
    let original = lab.borrow().paths[0].clone();
    Proposals::at(original.clone())
        .write([1; 16], vec![account("one")])
        .unwrap();
    let config = original.home.join("config.toml");
    let mut kept = None;
    let succeeding = std::rc::Rc::clone(&lab);
    at_next(Hook::Listed, move || {
        crate::generation::tests::succeed(&mut succeeding.borrow_mut());
    });
    let raced = sample(&mut kept, &original, &config, 7000);
    let successor = lab.borrow().paths[1].clone();
    assert_ne!(successor, original, "CONTROL: the succession completed");
    assert_eq!(
        raced.state,
        accounts(&original, &config, 7000),
        "CONTROL: the read opened the successor"
    );
    let next = sample(&mut kept, &original, &config, 8000);
    if !next.derived {
        wrong.push(format!(
            "(v) the sample after: {:?}, derived false",
            next.state
        ));
    }
    Proposals::at(successor)
        .write([2; 16], vec![account("two")])
        .unwrap();
    let next = sample(&mut kept, &original, &config, 9000);
    let now = accounts(&original, &config, 9000);
    assert!(
        cached(&now) && now.detail.starts_with("2 account proposals"),
        "CONTROL: the successor's proposal: {now:?}"
    );
    if next.state != now {
        wrong.push(format!(
            "(v) after a proposal at the successor: kept {:?}, fresh {now:?}",
            next.state
        ));
    }
    assert!(wrong.is_empty(), "{wrong:#?}");
}

/// A derivation on quiet files moves none of its own key: the accounts key taken before it
/// equals the one it is kept under, and the files show it after; and an observer's second sample
/// of quiet files derives neither service again. Observing writes nothing that is stamped.
///
/// NEGATIVE CONTROL (2026-11-14, run again 2026-11-15 once `Derivations` counted samples, and
/// again on the merge of main f581bd69910, whose vault writes no file under the home and so leaves
/// no delivered part to count), run and then restored: with `accounts_until` writing
/// `commons.claim` again with its own bytes (`fsx::write_atomic`, a stand-in for an observation
/// that repairs custody) once custody is admitted, it fails: `a quiet derivation is kept`; and the
/// count law `an_unchanged_home_is_derived_once_across_ten_samples` fails: `left: Derivations {
/// samples: 10, secrets: 1, accounts: 10, moved: [0, 0, 0, 0, 0] }`.
#[test]
fn a_derivation_moves_none_of_its_own_key() {
    let (lab, config) = usable();
    let paths = &lab.paths[0];
    let before = Listed::of(paths, &config)
        .and_then(|listed| listed.key())
        .expect("CONTROL: the key can be taken");
    let mut kept = None;
    let first = sample(&mut kept, paths, &config, 1000);
    assert!(cached(&first.state), "CONTROL: {:?}", first.state);
    let held = kept.as_ref().expect("a quiet derivation is kept");
    assert!(
        held.key == before,
        "the key before the derivation is the one it is kept under"
    );
    assert!(
        held.listed.key().is_some_and(|now| now == before),
        "the files show that key after it"
    );

    let mut observer = Observer::new(paths.clone(), config);
    observer.sample();
    observer.sample();
    assert_eq!(
        (
            observer.derivations().secrets,
            observer.derivations().accounts
        ),
        (1, 1),
        "a second sample of quiet files derives nothing"
    );
}

/// A sample that gives a kept standing renews its lease exactly as a derivation would: sampled
/// at t₀ (derived) and t₀+50 s (given again), the accounts service at t₀+105 s is still Cached,
/// observed and last read successfully at t₀+50 s by the wall, and at t₀+111 s it is no longer
/// current.
///
/// NEGATIVE CONTROL (2026-11-14), run and then restored: with `KeptAccounts::stamped` giving the
/// template's own times (the derivation's), it fails: `at t₀+61 s: ServiceState { availability:
/// Unknown, observed_at_ms: 1000000, …, detail: "This service observation is no longer current.
/// …" … }`, `left: Unknown`, `right: Cached`.
#[test]
fn a_confirmation_renews_the_lease_exactly_as_a_derivation() {
    let (lab, config) = usable();
    let paths = &lab.paths[0];
    let mut observer = Observer::new(paths.clone(), config);
    let at = Instant::now();
    let wall = 1_000_000;
    let clock = |seconds: u64| {
        move || {
            (
                at + Duration::from_secs(seconds),
                Some(wall + seconds * 1000),
            )
        }
    };
    observer.sample_with(clock(0));
    let publication = observer.sample_with(clock(50));
    assert_eq!(observer.derivations().accounts, 1, "CONTROL: given again");
    let current = |seconds: u64| {
        publication
            .current_at(
                at + Duration::from_secs(seconds),
                Some(wall + seconds * 1000),
            )
            .accounts
    };
    for seconds in [61, 105] {
        let accounts = current(seconds);
        assert_eq!(
            accounts.availability,
            ServiceAvailability::Cached,
            "at t₀+{seconds} s: {accounts:?}"
        );
        assert_eq!(accounts.observed_at_ms, wall + 50_000);
        assert_eq!(accounts.last_success_ms, Some(wall + 50_000));
    }
    let expired = current(111);
    assert_eq!(expired.availability, ServiceAvailability::Unknown);
    assert!(
        expired
            .detail
            .starts_with("This service observation is no longer current"),
        "{expired:?}"
    );
}
