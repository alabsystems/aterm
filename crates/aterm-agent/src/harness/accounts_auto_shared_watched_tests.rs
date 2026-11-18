// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The window's renewal guard, watched ([`AutoRenewGuard::shared`]): a permit's end on another
//! thread moves its generation and rings the trace's bell there, while a preparation pass that
//! admits nothing leaves it as it found it.

use super::*;
use crate::harness::native_keys;
use aterm_uds::trace::{self, Effect, Moved, Now, Trace, Unkept};
use std::sync::atomic::{AtomicUsize, Ordering};

/// What the guard is known as in a trace.
const GUARD: &str = "accounts_sync/renew_guard";

/// The wall clock the preparation passes run at (Unix seconds).
const NOW: i64 = 1_800_000_000;

/// Whether `trace` stands now.
fn stands(trace: &Trace) -> Result<(), Moved> {
    trace.stands(Now::at(NOW), Instant::now())
}

/// A scratch home and state folder, and the roster path beside the state's `aterm.toml`.
struct Bed {
    _root: aterm_tempfile::TempDir,
    home: PathBuf,
    state: PathBuf,
    roster: PathBuf,
}

impl Bed {
    fn new() -> Self {
        let root = aterm_tempfile::tempdir().expect("scratch");
        let home = root.path().join("home");
        let state = root.path().join("state");
        std::fs::create_dir_all(&home).expect("the home");
        std::fs::create_dir_all(&state).expect("the state");
        let roster = state.join(accounts::ACCOUNTS_FILE);
        Self {
            _root: root,
            home,
            state,
            roster,
        }
    }

    /// One preparation pass ([`adopt_first`]) over `guard`, with `discover` as shared
    /// discovery, under a scope armed here: what the pass recorded. Nothing it may reach fetches,
    /// spawns, syncs the vault or checks a login.
    fn pass(&self, guard: &Arc<Watched<AutoRenewGuard>>, discover: &Discovery) -> Trace {
        let cooldown = Watched::new("accounts_sync/sync_cooldown", None);
        let rediscovery = Watched::new("accounts_sync/rediscovery", Rediscovery::default());
        let fx = UpkeepEffects {
            scope: None,
            fetch: &|_, _| panic!("a preparation pass fetched a source"),
            spawn_renewal: &|_| panic!("a preparation pass that admitted nothing spawned"),
            sync_cooldown: &cooldown,
            rediscovery: &rediscovery,
            renew_guard: guard,
            find_browser: &|_| false,
            vault_sync: |_, _| panic!("a preparation pass synced the vault"),
            spawn_vault: &|_| panic!("a preparation pass started a vault sync"),
            default_check: |_, _| panic!("a preparation pass checked the default login"),
            mailbox: &|_, _, _| (crate::harness::sign_in_ladder::Mailbox::Ready, None),
            log: Arc::new(|line: &str| panic!("a preparation pass logged {line:?}")),
            run_attempt: None,
            tokens_here: |_, _, _, _| None,
            shared: Effects {
                authority: None,
                checkpoint_authority: None,
                host: None,
                discover: Some(discover),
            },
        };
        trace::scope(|| {
            adopt_first(
                &self.home,
                &self.state,
                &self.roster,
                aterm_uds::trace::Now::at(NOW),
                &fx,
            )
        })
        .1
    }
}

/// A trace that found a renewal in flight reads the guard; the renewal's permit, dropped on the
/// thread that ran it, moves the guard's generation once and rings the bell once there, which
/// ends that trace. A preparation pass that asks native and shared discovery and admits nothing
/// takes the guard's discovery pass and gives it back, and so leaves the generation where it was,
/// rings nothing and records no change of memory, though it read the guard.
///
/// NEGATIVE CONTROLS (2026-11-15), each run alone and then restored:
/// - `AutoRenewPermit::drop` without its `guard.in_flight = false` failed with "the permit's end
///   rang 0 times on its thread", left `0`, right `1`.
/// - `PreparationOrder` hashing its `active` flag too failed with "a preparation pass that
///   admitted nothing moved the guard's generation", left `4`, right `2`.
#[test]
fn a_permit_dropped_on_another_thread_moves_the_guard_generation_and_rings_while_a_net_zero_preparation_pass_moves_nothing()
 {
    let guard = AutoRenewGuard::default().shared();
    let start = Instant::now();
    let permit =
        AutoRenewPermit::claim(&guard, "a", start, false).expect("the first renewal's permit");
    let claimed = guard.generation();
    let (busy, read) = trace::scope(|| AutoRenewPermit::claim(&guard, "b", start, false).is_none());
    assert!(
        busy,
        "a second renewal was admitted while the first was in flight"
    );
    assert_eq!(read.unkept(), None, "{read:?}");
    assert_eq!(read.memory().collect::<Vec<_>>(), [(GUARD, claimed)]);
    assert_eq!(stands(&read), Ok(()));

    let rang = std::thread::spawn(move || {
        let before = trace::rung_here();
        drop(permit);
        trace::rung_here() - before
    })
    .join()
    .expect("the renewal's thread");
    assert_eq!(rang, 1, "the permit's end rang {rang} times on its thread");
    assert_eq!(guard.generation(), claimed + 1);
    assert_eq!(
        stands(&read),
        Err(Moved::Memory(GUARD)),
        "a trace that found the renewal in flight stood after the permit's end"
    );

    let bed = Bed::new();
    let natives = Arc::new(AtomicUsize::new(0));
    let shared = Arc::new(AtomicUsize::new(0));
    let found = Arc::clone(&shared);
    let discover = move |_: &Path| -> Result<Vec<Candidate>, String> {
        found.fetch_add(1, Ordering::SeqCst);
        Ok(Vec::new())
    };
    let asked = Arc::clone(&natives);
    let native = Box::new(
        move |_: &Path,
              _: &Path,
              _: &Path,
              _: &mut dyn FnMut(&native_keys::PreparationId) -> bool|
              -> Result<Option<native_keys::PreparationTask>, String> {
            asked.fetch_add(1, Ordering::SeqCst);
            Ok(None)
        },
    );
    let generation = guard.generation();
    let rung = trace::rung_here();
    let pass = native_keys::with_test_discovery(native, || bed.pass(&guard, &discover));
    assert_eq!(
        (
            natives.load(Ordering::SeqCst),
            shared.load(Ordering::SeqCst)
        ),
        (1, 1),
        "the pass did not ask both discoveries once"
    );
    assert_eq!(
        guard.generation(),
        generation,
        "a preparation pass that admitted nothing moved the guard's generation"
    );
    assert_eq!(
        trace::rung_here(),
        rung,
        "a preparation pass that admitted nothing rang the bell"
    );
    assert!(
        pass.effects().all(|effect| effect != Effect::Memory),
        "a preparation pass that admitted nothing changed memory: {pass:?}"
    );
    assert!(
        pass.memory().any(|read| read == (GUARD, generation)),
        "the pass did not record the guard it read: {pass:?}"
    );
}

/// A preparation pass that finds another upkeep's pass active admits nothing and records a
/// retry, since the other pass's end moves no generation; that end leaves the guard's generation
/// where it was.
///
/// NEGATIVE CONTROLS (2026-11-15), each run alone and then restored:
/// - `PreparationPass::claim` returning `None` for an active pass with no `trace::retry` failed
///   with "a pass that found another active was kept", left `None`, right
///   `Some(Retry("another upkeep's preparation pass is active"))`.
/// - `PreparationOrder` hashing its `active` flag too failed with "the other pass's end moved
///   the guard's generation", left `2`, right `1`.
#[test]
fn a_preparation_pass_that_finds_another_active_records_a_retry() {
    let guard = AutoRenewGuard::default().shared();
    let bed = Bed::new();
    let other = PreparationPass::claim(&guard).expect("the other upkeep's pass");
    let generation = guard.generation();
    let held = bed.pass(&guard, &|_| {
        panic!("a pass that found another active discovered")
    });
    assert_eq!(
        held.unkept(),
        Some(Unkept::Retry(
            "another upkeep's preparation pass is active".into()
        )),
        "a pass that found another active was kept"
    );
    drop(other);
    assert_eq!(
        guard.generation(),
        generation,
        "the other pass's end moved the guard's generation"
    );
}
