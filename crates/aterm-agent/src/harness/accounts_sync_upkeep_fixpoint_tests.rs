// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The upkeep's fixpoint over the scripted bed ([`super::Scripted`]): once a pass has caught its
//! files up with its inputs, the next pass over the same inputs, a minute later as the window's
//! cadence makes it, writes no file, starts nothing and moves no generation. Every address is
//! synthetic.

use super::*;
use aterm_uds::trace;
use std::sync::atomic::{AtomicUsize, Ordering};

/// Claude Code's row on its vault token here.
const ON_TOKEN: &str = "claude-tttttt";

/// When that token ends: a year after the bed's `NOW`.
const TOKEN_END: u64 = NOW.unsigned_abs() + 365 * 86_400;

/// The vault route's read in the bed: [`ON_TOKEN`]'s token is in use here until [`TOKEN_END`],
/// whenever it is read.
fn on_its_token(
    _: &Path,
    _: &accounts::Store,
    _: &Roster,
    _: aterm_uds::trace::Now,
) -> Option<BTreeMap<String, u64>> {
    Some(BTreeMap::from([(ON_TOKEN.to_owned(), TOKEN_END)]))
}

/// What the window's upkeep keeps between its passes: the memo of what it remembers, the guard
/// of its renewals and its sync cooldown, as its statics keep them.
struct Kept<'a> {
    rediscovery: &'a Watched<Rediscovery>,
    guard: std::sync::Arc<Watched<AutoRenewGuard>>,
    cooldown: Watched<Option<Instant>>,
}

impl Kept<'_> {
    /// The generation of each.
    fn generations(&self) -> [u64; 3] {
        [
            self.rediscovery.generation(),
            self.guard.generation(),
            self.cooldown.generation(),
        ]
    }
}

/// One whole pass of the window's upkeep (`UpkeepStep::Both`) over `script` at `now`, keeping
/// what it remembers in `kept`, with the vault's read [`on_its_token`], each renewal or vault
/// sync handed to `spawned` (counted, never run) and no fetch ([`no_fetch`] panics).
fn whole_pass(script: &Scripted, kept: &Kept<'_>, now: i64, spawned: &AtomicUsize) {
    let spawn = |_work: Box<dyn FnOnce() + Send>| -> io::Result<()> {
        spawned.fetch_add(1, Ordering::SeqCst);
        Ok(())
    };
    let lifetime = crate::harness::account_work::Lifetime::default();
    let fx = UpkeepEffects {
        scope: lifetime.enter(),
        fetch: &no_fetch,
        spawn_renewal: &spawn,
        sync_cooldown: &kept.cooldown,
        rediscovery: kept.rediscovery,
        renew_guard: &kept.guard,
        find_browser: &|_: &Path| false,
        vault_sync: |_: &Path, _scope| crate::harness::sign_in_ladder::Vault::NotMember,
        spawn_vault: &spawn,
        default_check: |_: &Path, _scope| {
            crate::harness::cli::LoginCheck::CouldNotRun(String::new())
        },
        mailbox: &ladder_mailbox_files,
        log: std::sync::Arc::new(|_: &str| {}),
        run_attempt: None,
        tokens_here: on_its_token,
        shared: shared_renewal::Effects::default(),
    };
    let bed = &script.bed;
    let _ = upkeep(
        &bed.home,
        &bed.state,
        (&claude_roster(bed), trace::Now::at(now), UpkeepStep::Both),
        &fx,
    );
}

/// Each file and folder under `root` with what an `lstat` shows of it: its inode, its length,
/// and its modification and change times. A file written again, even with the bytes it held, or
/// a folder an entry was made in or taken from, shows otherwise.
fn stamps(root: &Path) -> BTreeMap<PathBuf, (u64, u64, i128, i128)> {
    use std::os::unix::fs::MetadataExt as _;
    let mut out = BTreeMap::new();
    let mut walk = vec![root.to_path_buf()];
    while let Some(at) = walk.pop() {
        let Ok(meta) = fs::symlink_metadata(&at) else {
            continue;
        };
        let ns = |secs: i64, nsecs: i64| i128::from(secs) * 1_000_000_000 + i128::from(nsecs);
        out.insert(
            at.clone(),
            (
                meta.ino(),
                meta.len(),
                ns(meta.mtime(), meta.mtime_nsec()),
                ns(meta.ctime(), meta.ctime_nsec()),
            ),
        );
        if meta.is_dir()
            && let Ok(entries) = fs::read_dir(&at)
        {
            walk.extend(entries.flatten().map(|entry| entry.path()));
        }
    }
    out
}

/// The paths whose stamps differ between `before` and `after`, or that only one holds.
fn moved_paths(
    before: &BTreeMap<PathBuf, (u64, u64, i128, i128)>,
    after: &BTreeMap<PathBuf, (u64, u64, i128, i128)>,
) -> Vec<PathBuf> {
    before
        .keys()
        .chain(after.keys())
        .filter(|path| before.get(*path) != after.get(*path))
        .cloned()
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect()
}

/// THE SECOND PASS OVER THE SCRIPTED BED WRITES NO FILE. Over [`Scripted`] (Kimi's and Grok's
/// rosters synced from a lab that prefers one account of each here) with Claude Code's roster
/// holding one row whose login is made here and whose vault token the vault route reads as in
/// use here for a year, the window's upkeep thread makes whole passes a minute apart and turns
/// its hold on Claude Code's reading of the rows on their token (`accounts::TokensHereHolder`)
/// after each, as the window does. The first pass catches the files up: it adds the lab's rows
/// and writes `tokens_here.json` naming
/// the row; its turn vouches for that reading. The next pass, a minute later over the same
/// inputs, then leaves every file and folder under the scratch root as it was (bytes and
/// `lstat` alike, `tokens_here.json` included), records no effect in a trace scope armed around
/// it, hands no renewal or vault sync to be started, fetches nothing, moves none of the
/// generations the upkeep keeps and rings no bell on its thread; the reading still names the
/// row, vouched. CONTROL: the same reading with no holder (no window vouches for it) is written
/// again at the later pass's time, as every pass wrote it before, so a reader in another process
/// ages it from its last read.
///
/// NEGATIVE CONTROLS (2026-11-15), each the one mutation of its mutant build that reached this
/// test:
/// - `Store::note_tokens_here` writing the reading at every read, as main before this commit
///   did (its `kept.vouched && kept.ends == ends` as `false`): `the second pass rewrote` failed,
///   left `[".../state/accounts", ".../state/accounts/tokens_here.json"]`, right `[]`;
/// - `Store::note_tokens_here` writing nothing for the same rows whether or not anyone vouches
///   (that test as `kept.ends == ends`): `CONTROL: with no holder the pass writes the reading
///   again` failed.
///
/// Each restored, the file touched.
#[test]
fn a_second_pass_over_the_scripted_bed_writes_no_file() {
    let script = Scripted::new("upkeep-fixpoint");
    let bed = &script.bed;
    let store = accounts::Store::at_for(&bed.state, Cli::Claude, &bed.home);
    let login = store.logins_dir().join(ON_TOKEN);
    fs::create_dir_all(&login).unwrap();
    fs::write(login.join("settings.json"), "{}").unwrap();
    let row = accounts::Row {
        label: ON_TOKEN.to_owned(),
        class: accounts::Class::Preferred,
        dir: Some(login.to_string_lossy().into_owned()),
        email: Some("tess@example.com".to_owned()),
        ..accounts::Row::default()
    };
    fs::write(claude_roster(bed), accounts::row_text(&row).unwrap() + "\n").unwrap();
    let kept = Kept {
        rediscovery: &script.rediscovery,
        guard: AutoRenewGuard::default().shared(),
        cooldown: Watched::new("accounts_sync/sync_cooldown", None),
    };
    let spawned = AtomicUsize::new(0);
    let holder = accounts::TokensHereHolder::of(&store).expect("Claude Code's store is held");

    whole_pass(&script, &kept, NOW, &spawned);
    holder.turn(NOW);
    let reading = store
        .read_tokens_here()
        .expect("the first pass keeps the reading");
    assert_eq!(
        (reading.read_at, reading.ends.get(ON_TOKEN).copied()),
        (NOW, Some(i64::try_from(TOKEN_END).unwrap())),
        "the first pass's reading"
    );
    assert!(holder.vouching(), "the first turn vouches for it");

    let (root, later) = (bed._dir.path(), NOW + 60);
    let (tree, stamped, generations) = (script.tree(), stamps(root), kept.generations());
    let (spawns, rung) = (spawned.load(Ordering::SeqCst), trace::rung_here());
    let ((), pass) = trace::scope(|| whole_pass(&script, &kept, later, &spawned));
    holder.turn(later);
    assert_eq!(
        moved_paths(&stamped, &stamps(root)),
        Vec::<PathBuf>::new(),
        "the second pass rewrote"
    );
    assert_eq!(
        script.tree(),
        tree,
        "the second pass changed no file's bytes"
    );
    assert_eq!(pass.effects().collect::<Vec<_>>(), [], "{pass:?}");
    assert_eq!(
        spawned.load(Ordering::SeqCst),
        spawns,
        "the second pass started nothing"
    );
    assert_eq!(kept.generations(), generations, "no kept generation moved");
    assert_eq!(
        trace::rung_here(),
        rung,
        "no bell rang on the pass's thread"
    );
    let reading = store.read_tokens_here().expect("the reading stands");
    assert_eq!(
        (reading.read_at, reading.vouched),
        (NOW, true),
        "the reading the first pass wrote, vouched"
    );

    // CONTROL: with no holder, nothing vouches for the reading, and each pass writes it again.
    drop(holder);
    let after = NOW + 120;
    let stamped = stamps(root);
    whole_pass(&script, &kept, after, &spawned);
    assert!(
        moved_paths(&stamped, &stamps(root)).contains(&store.dir.join(accounts::TOKENS_HERE_FILE)),
        "CONTROL: with no holder the pass writes the reading again"
    );
    let reading = store.read_tokens_here().expect("the reading stands");
    assert_eq!(
        (reading.read_at, reading.vouched),
        (after, false),
        "CONTROL: written at the pass's time, vouched by nobody"
    );
}
