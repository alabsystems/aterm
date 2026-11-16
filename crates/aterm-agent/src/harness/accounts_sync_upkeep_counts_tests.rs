// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The accounts' upkeep's counts (`accounts_upkeep_counts`) over a scripted upkeep: a scratch
//! home whose Kimi and Grok rosters sync from the lab and whose Codex pool the lab lists no row
//! for, and one with no lab whose Kimi roster syncs from a fleet file. Every address is
//! synthetic.

use super::*;
use crate::harness::accounts_upkeep_counts::{UpkeepCounts, counts, counts_here};
use crate::harness::alab_roster::{
    Lab, LabAccount, ReceiptGuard, SourceGuard, SourceSnapshot, TestSourceOverride, test_machine,
};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

/// A receipt guard that counts how often it is asked, and admits every save unless told to
/// refuse.
struct Asked {
    asked: Arc<AtomicUsize>,
    refuses: Arc<AtomicBool>,
}

impl SourceGuard for Asked {
    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
    fn same_source(&self, other: &dyn SourceGuard) -> bool {
        other
            .as_any()
            .downcast_ref::<Self>()
            .is_some_and(|other| Arc::ptr_eq(&self.asked, &other.asked))
    }
    fn with_admitted(
        &self,
        save: Box<dyn FnOnce() -> Result<(), SyncError> + '_>,
    ) -> Result<(), SyncError> {
        self.asked.fetch_add(1, Ordering::SeqCst);
        if self.refuses.load(Ordering::SeqCst) {
            return Err(SyncError::retry(
                Retry::Changed,
                "the test source refuses this receipt",
            ));
        }
        save()
    }
}

fn account(cli: Cli, local: &str, label: &str, serial: usize) -> LabAccount {
    LabAccount {
        pk: format!("ac-{serial:0>16}"),
        cli,
        vendor: accounts::Vendor {
            region: (cli == Cli::Kimi).then_some(crate::ai::connector::Region::Global),
            ..accounts::Vendor::default()
        },
        label: label.to_owned(),
        legacy_label: None,
        email: format!("{local}@example.com"),
        identity: None,
        browser: None,
        joinable: true,
        preferred: vec![(test_machine("m16"), Some(1))],
    }
}

/// A settled lab that names this Mac `m16` and prefers `accounts` here.
fn lab_of(accounts: Vec<LabAccount>) -> Lab {
    Lab {
        me: Some(test_machine("m16")),
        settled: true,
        accounts,
        ..Lab::default()
    }
}

/// What the calling thread's counts moved by over `run`, as
/// `[passes, pool_syncs, pool_syncs_guarded, receipt_admissions]`. The
/// process's own counts ([`counts`], what `metrics` prints) must have moved by at least as
/// much: every event counts in both, and other threads only add.
fn moved(run: impl FnOnce()) -> [u64; 4] {
    let list = |c: UpkeepCounts| {
        [
            c.passes,
            c.pool_syncs,
            c.pool_syncs_guarded,
            c.receipt_admissions,
        ]
    };
    let (before, process_before) = (list(counts_here()), list(counts()));
    run();
    let (after, process_after) = (list(counts_here()), list(counts()));
    let here: [u64; 4] = std::array::from_fn(|i| after[i] - before[i]);
    let process: [u64; 4] = std::array::from_fn(|i| process_after[i] - process_before[i]);
    assert!(
        (0..4).all(|i| process[i] >= here[i]),
        "the process's counts moved by {process:?}, this thread's by {here:?}"
    );
    here
}

fn no_fetch(_: &Env, _: &str) -> Result<String, String> {
    panic!("no source is fetched")
}

/// One pass of the window's upkeep over `bed`, keeping what it remembers between passes in
/// `rediscovery` as a window does.
fn upkeep_pass(bed: &Bed, rediscovery: &Mutex<Rediscovery>, step: UpkeepStep) {
    with_logged_effects(
        (&|_work| Ok(()), &no_fetch),
        (rediscovery, std::sync::Arc::new(|_: &str| {})),
        |fx| {
            let _ = upkeep(&bed.home, &bed.state, (&claude_roster(bed), NOW, step), fx);
        },
    );
}

/// A scratch home that keeps Kimi's and Grok's rosters, under a settled lab that prefers one
/// account of each here and no Codex account, with a counting guard ([`Asked`]) and one memo
/// of what the upkeep remembers for all of its passes.
struct Scripted {
    bed: Bed,
    asked: Arc<AtomicUsize>,
    refuses: Arc<AtomicBool>,
    source: TestSourceOverride,
    rediscovery: Mutex<Rediscovery>,
}

impl Scripted {
    fn new(tag: &str) -> Self {
        let bed = Bed::new(tag);
        bed.keep_roster(Cli::Kimi);
        bed.keep_roster(Cli::Grok);
        let asked = Arc::new(AtomicUsize::new(0));
        let refuses = Arc::new(AtomicBool::new(false));
        let source = TestSourceOverride::new(SourceSnapshot::Present(Arc::new(Self::lab(&[]))))
            .with_guard(ReceiptGuard::new(Asked {
                asked: Arc::clone(&asked),
                refuses: Arc::clone(&refuses),
            }));
        Self {
            bed,
            asked,
            refuses,
            source,
            rediscovery: Mutex::new(Rediscovery::default()),
        }
    }

    /// The lab's two accounts and `more`.
    fn lab(more: &[LabAccount]) -> Lab {
        let mut accounts = vec![
            account(Cli::Kimi, "eve", "kimi-eeeeee", 1),
            account(Cli::Grok, "gil", "grok-gggggg", 2),
        ];
        accounts.extend(more.iter().cloned());
        lab_of(accounts)
    }

    /// What `run` moved the counts by ([`moved`]) and how often it asked the guard.
    fn counted(&self, run: impl FnOnce()) -> ([u64; 4], usize) {
        let before = self.asked.load(Ordering::SeqCst);
        let counts = moved(run);
        (counts, self.asked.load(Ordering::SeqCst) - before)
    }

    /// One pass of the window's upkeep ([`upkeep_pass`]), syncing only.
    fn pass(&self) -> ([u64; 4], usize) {
        self.pass_of(UpkeepStep::Sync)
    }

    fn pass_of(&self, step: UpkeepStep) -> ([u64; 4], usize) {
        self.counted(|| upkeep_pass(&self.bed, &self.rediscovery, step))
    }

    /// `aterm harness accounts sync --cli <cli> --json`, no source named, through the verb's
    /// own entry ([`run_cli_accounts_sync_with`]): its exit, its document (`Null` where it
    /// printed none) and its stderr.
    fn asked_for(&self, cli: Cli) -> (ExitCode, Value, String) {
        let env = quiet_env(&self.bed.home, &self.bed.state, &self.bed.aterm_toml(), NOW);
        let (mut out, mut err) = (Vec::new(), Vec::new());
        let code = run_cli_accounts_sync_with(
            cli,
            (&env, None),
            (None, None, true),
            (&mut out, &mut err),
            &no_fetch,
        );
        let doc = aterm_json::from_str(String::from_utf8_lossy(&out).trim()).unwrap_or(Value::Null);
        (code, doc, String::from_utf8(err).unwrap())
    }

    /// A person's sync of `cli`'s unchanged pool ([`Self::asked_for`]): it exits 0 and adds no
    /// row. What it moved the counts by and how often it asked the guard.
    fn asked_for_unchanged(&self, cli: Cli) -> ([u64; 4], usize) {
        self.counted(|| {
            let (code, doc, err) = self.asked_for(cli);
            assert_eq!(code, ExitCode::SUCCESS, "{cli:?}: {err}");
            assert_eq!(doc["added"], Value::Array(Vec::new()), "{cli:?}: {doc}");
        })
    }

    fn tree(&self) -> BTreeMap<PathBuf, Vec<u8>> {
        Bed::tree(self.bed._dir.path())
    }
}

/// THE UPKEEP'S COUNTS MOVE BY WHAT IT DID. Over a home that keeps Kimi's and Grok's rosters,
/// a lab that prefers one account of each here and no Codex account, and a guard that counts
/// how often it is asked (`asked`), the passes of one window (one memo of what the upkeep
/// remembers) move
/// `[passes, pool_syncs, pool_syncs_guarded, receipt_admissions]` by:
/// - the first pass: `[1, 3, 2, 4]`, `asked` 4. Codex syncs by default and stops where the
///   lab lists no row of its, before any roster publication; Grok and Kimi each reach their
///   roster's publication with the lab's guard, which admits the row each adds (2); and
///   Claude Code's and Codex's pools each retire the source the lab replaces, a publication
///   the guard admits (2);
/// - a second pass over the same inputs, which changes nothing: `[1, 2, 2, 0]`, `asked` 0.
///   The quiet rule holds Codex's sync back (its last one took nothing for the same inputs),
///   so two pools sync; Grok and Kimi still reach their publication with the guard and
///   neither plan asks it; no file under the scratch root moves;
/// - a pool sync a person asks for through the verb, unchanged: `[0, 0, 0, 1]`, `asked` 1:
///   no pass, no upkeep sync, one admission;
/// - a pass that only renews (`UpkeepStep::Renew`): `[1, 0, 0, 0]`: a pass, and no sync;
/// - with the accounts turned off, a pass through `upkeep`: `[1, 0, 0, 0]`: the pass is
///   counted where it is entered, before the off switch ends it; and `upkeep_pools` alone:
///   `[0, 0, 0, 0]`.
///
/// Each reading also holds the process's counts to this thread's ([`moved`]).
///
/// NEGATIVE CONTROLS (2026-11-13), each applied alone:
/// - `publish_plan_scoped`'s `Event::ReceiptAdmission` increment removed: `the first pass
///   adds rows` fails, left `([1, 3, 2, 0], 4)`, right `([1, 3, 2, 4], 4)`;
/// - `count`'s add to the process's counts removed: the first pass fails, `the process's
///   counts moved by [0, 0, 0, 0], this thread's by [1, 3, 2, 4]`;
/// - the `Event::PoolSync` increment moved above the quiet rule's test in `upkeep_pools`:
///   `an unchanged pass` fails, left `([1, 3, 2, 0], 0)`, right `([1, 2, 2, 0], 0)`;
/// - the `Event::Pass` increment moved below `upkeep`'s off-switch returns: `a pass the off
///   switch ends` fails, left `([0, 0, 0, 0], 0)`, right `([1, 0, 0, 0], 0)`.
#[test]
fn a_scripted_upkeep_moves_each_count_by_what_it_did() {
    let script = Scripted::new("upkeep-counts");
    let bed = &script.bed;
    assert_eq!(script.pass(), ([1, 3, 2, 4], 4), "the first pass adds rows");
    for (cli, label) in [(Cli::Kimi, "kimi-eeeeee"), (Cli::Grok, "grok-gggggg")] {
        assert!(
            bed.rows(cli).iter().any(|row| row.label == label),
            "{cli:?}: the first pass added the lab's row"
        );
    }
    assert!(
        !bed.roster(Cli::Codex).exists(),
        "the lab lists no Codex row"
    );
    for cli in [Cli::Claude, Cli::Codex] {
        assert!(
            accounts::Store::at_for(&bed.state, cli, &bed.home)
                .source_policy()
                .unwrap()
                .retired(),
            "{cli:?}: the first pass retired the source the lab replaces"
        );
    }

    let before = script.tree();
    assert_eq!(script.pass(), ([1, 2, 2, 0], 0), "an unchanged pass");
    assert_eq!(script.tree(), before, "an unchanged pass writes nothing");

    assert_eq!(
        script.asked_for_unchanged(Cli::Kimi),
        ([0, 0, 0, 1], 1),
        "a person's unchanged sync"
    );
    assert_eq!(
        script.pass_of(UpkeepStep::Renew),
        ([1, 0, 0, 0], 0),
        "a pass that only renews"
    );

    crate::test_config::write(
        bed.aterm_toml(),
        "[fleet]\nmachine = \"m16\"\n[harness]\naccounts = \"off\"\n",
    )
    .unwrap();
    assert_eq!(
        script.pass(),
        ([1, 0, 0, 0], 0),
        "a pass the off switch ends"
    );
    let off = script.counted(|| {
        with_effects(&|_work| Ok(()), &no_fetch, |fx| {
            upkeep_pools(
                &bed.home,
                &bed.state,
                (&claude_roster(bed), NOW, UpkeepStep::Sync),
                fx,
            );
        });
    });
    assert_eq!(off, ([0, 0, 0, 0], 0), "the accounts' sync turned off");
}

/// A POOL SYNCED FROM A FLEET FILE IS COUNTED AND HOLDS NO RECEIPT GUARD. Over a home with no
/// lab whose Kimi roster syncs from Kimi's fleet file, an entry of the home's test vault
/// (`accounts_bed`, which only a unix build seeds; `ProviderSource::receipt_guard` gives no
/// guard for such a source), by
/// `[passes, pool_syncs, pool_syncs_guarded, receipt_admissions]`:
/// - the first pass: `[1, 3, 0, 0]`: Codex, Grok and Kimi sync; Kimi reaches its roster's
///   publication, which adds this machine's row with no guard to ask; Codex and Grok keep no
///   roster and have no source;
/// - the second pass: `[1, 1, 0, 0]`: Kimi alone syncs (the quiet rule holds Codex's and
///   Grok's back), reaches its publication again, and changes nothing.
///
/// NEGATIVE CONTROL (2026-11-13): with `&& guard.is_some()` removed from the
/// `Event::PoolSyncGuarded` increment in `sync_cli_in_env`, `the first pass` fails: left
/// `[1, 3, 1, 0]`, right `[1, 3, 0, 0]`.
#[cfg(unix)]
#[test]
fn an_upkeep_sync_from_a_fleet_file_is_counted_without_a_guard() {
    let bed = accounts_bed("upkeep-unguarded", false);
    let rediscovery = Mutex::new(Rediscovery::default());
    let pass = || moved(|| upkeep_pass(&bed, &rediscovery, UpkeepStep::Sync));
    assert_eq!(pass(), [1, 3, 0, 0], "the first pass");
    assert!(
        bed.rows(Cli::Kimi).iter().any(|row| row.label == "kimi-1"),
        "Kimi's sync reached its publication and added this machine's row"
    );
    let before = Bed::tree(bed._dir.path());
    assert_eq!(pass(), [1, 1, 0, 0], "the second pass");
    assert_eq!(
        Bed::tree(bed._dir.path()),
        before,
        "an unchanged pass writes nothing"
    );
}

/// AN UPKEEP SYNC WHOSE PLAN CHANGES NOTHING ASKS NO ADMISSION; EVERY OTHER SYNC ASKS AS IT
/// DID. After the first pass of [`Scripted`] has added the lab's rows, by
/// `[passes, pool_syncs, pool_syncs_guarded, receipt_admissions]` and the
/// times the guard was asked (Codex's sync is held back by the quiet rule in every pass after
/// the first, so two pools sync):
/// - an upkeep pass that changes nothing: `[1, 2, 2, 0]`, asked 0, no file moved: no
///   receipt admission. That is not every strict read of the registry a sync can cause in a
///   window: its capture and each validation refresh the lab view, which reads the registry's
///   mail metadata strictly where its held lab does not stand
///   (`aterm_db::alab::AdmissionCounts::observations`). The scripted source here is a double
///   and makes no such read;
/// - the same unchanged sync a person asks for through the verb
///   (`run_cli_accounts_sync_with`), Kimi's then Grok's: `[0, 0, 0, 1]`, asked 1, each,
///   and the verb exits 0;
/// - the same unchanged syncs as init runs them (`sync_cli_accounts_with`): `[0, 0, 0, 2]`,
///   asked 2;
/// - an upkeep pass after the lab prefers a second Kimi account here: `[1, 2, 2, 1]`,
///   asked 1 (Kimi's plan adds the row; Grok's changes nothing), and the row is added;
/// - an upkeep pass after that row's login directory was removed, the roster's bytes equal:
///   `[1, 2, 2, 1]`, asked 1, and the directory is made again: the receipt owns it;
/// - with the guard refusing: an unchanged upkeep pass is `[1, 2, 2, 0]`, asked 0 and
///   moves no file; a person's unchanged sync through the verb is asked 1 and fails with the
///   guard's refusal on stderr; an upkeep pass whose plan would add a third Kimi row is
///   asked 1 and adds nothing.
///
/// NEGATIVE CONTROLS (2026-11-13), each applied alone:
/// - the admission restored for the upkeep (`SyncCaller::admits` answering `EveryPlan` for
///   `Upkeep`): `an unchanged upkeep pass` fails, left `([1, 2, 2, 2], 2)`, right
///   `([1, 2, 2, 0], 0)`;
/// - the admission dropped for a person's unchanged sync (`Changes` for `Person`):
///   `Kimi: a person's unchanged sync` fails, left `([0, 0, 0, 0], 0)`, right
///   `([0, 0, 0, 1], 1)`;
/// - the verb passing `SyncCaller::Upkeep` (`run_cli_accounts_sync_with`): `Kimi: a person's
///   unchanged sync` fails, left `([0, 0, 1, 0], 0)`, right `([0, 0, 0, 1], 1)`;
/// - the admission dropped for init's unchanged sync (`Changes` for `Init`):
///   `init's unchanged syncs` fails, left `([0, 0, 0, 0], 0)`, right `([0, 0, 0, 2], 2)`;
/// - the admission skipped for a changed plan (`publish_plan_scoped` asking the guard only
///   under `EveryPlan`, whatever `changed` says): `the first pass` fails, left
///   `([1, 3, 2, 2], 2)`, right `([1, 3, 2, 4], 4)`: the two plans that add a row
///   were published without the guard.
#[test]
fn an_unchanged_upkeep_sync_asks_no_admission_and_every_other_sync_asks_as_it_did() {
    let script = Scripted::new("upkeep-admission");
    let bed = &script.bed;
    assert_eq!(script.pass(), ([1, 3, 2, 4], 4), "the first pass");

    let before = script.tree();
    assert_eq!(script.pass(), ([1, 2, 2, 0], 0), "an unchanged upkeep pass");
    assert_eq!(script.tree(), before, "an unchanged pass writes nothing");

    for cli in [Cli::Kimi, Cli::Grok] {
        assert_eq!(
            script.asked_for_unchanged(cli),
            ([0, 0, 0, 1], 1),
            "{cli:?}: a person's unchanged sync"
        );
    }
    let init = script.counted(|| {
        for (cli, synced) in
            sync_cli_accounts_with(&bed.home, &bed.state, &claude_roster(bed), NOW, &no_fetch)
        {
            assert!(synced.is_ok(), "{cli:?}: init's sync: {synced:?}");
        }
    });
    assert_eq!(init, ([0, 0, 0, 2], 2), "init's unchanged syncs");
    assert_eq!(script.tree(), before, "unchanged syncs write nothing");

    let second = account(Cli::Kimi, "fay", "kimi-ffffff", 3);
    script
        .source
        .set(SourceSnapshot::Present(Arc::new(Scripted::lab(
            std::slice::from_ref(&second),
        ))));
    assert_eq!(
        script.pass(),
        ([1, 2, 2, 1], 1),
        "an upkeep pass that adds one row"
    );
    assert!(
        bed.rows(Cli::Kimi)
            .iter()
            .any(|row| row.label == "kimi-ffffff"),
        "the changed plan was published"
    );

    let directory = accounts::Store::at_for(&bed.state, Cli::Kimi, &bed.home)
        .login_dir("kimi-ffffff")
        .unwrap();
    let roster = fs::read(bed.roster(Cli::Kimi)).unwrap();
    fs::remove_dir(&directory).unwrap();
    assert_eq!(
        script.pass(),
        ([1, 2, 2, 1], 1),
        "an upkeep pass that makes a missing login directory"
    );
    assert!(directory.is_dir(), "the receipt made the directory again");
    assert_eq!(fs::read(bed.roster(Cli::Kimi)).unwrap(), roster);

    script.refuses.store(true, Ordering::SeqCst);
    let before = script.tree();
    assert_eq!(
        script.pass(),
        ([1, 2, 2, 0], 0),
        "an unchanged upkeep pass under a guard that refuses"
    );
    let refused = script.counted(|| {
        let (code, doc, err) = script.asked_for(Cli::Kimi);
        assert_eq!(code, ExitCode::FAILURE, "{doc}");
        assert!(
            err.contains("the test source refuses this receipt"),
            "{err}"
        );
    });
    assert_eq!(
        refused,
        ([0, 0, 0, 1], 1),
        "a person's unchanged sync under a guard that refuses"
    );
    script
        .source
        .set(SourceSnapshot::Present(Arc::new(Scripted::lab(&[
            second,
            account(Cli::Kimi, "hal", "kimi-hhhhhh", 4),
        ]))));
    assert_eq!(
        script.pass(),
        ([1, 2, 2, 1], 1),
        "an upkeep pass whose changed plan the guard refuses"
    );
    assert!(
        bed.rows(Cli::Kimi)
            .iter()
            .all(|row| row.label != "kimi-hhhhhh"),
        "a refused plan publishes nothing"
    );
    assert_eq!(
        script.tree(),
        before,
        "refused and unchanged syncs write nothing"
    );
}
