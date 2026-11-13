// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! §9's table conformance tests for member-signed table cutoffs
//! (docs/DESIGN-alab-one-mesh-2026-11-05.md §4.10, §11 B11), each with its negative control.

use super::fixture::Lab;
use super::store::{Signing, Store};
use super::*;
use aterm_secrets::client;

/// Three writers on one table, Mac `n`'s replica at `dirs[n]`.
struct Table {
    lab: Lab,
    dirs: [std::path::PathBuf; 3],
    scope: Scope,
}

fn table() -> Table {
    let lab = Lab::with_foundation(&[Role::Writer, Role::Writer, Role::Writer]);
    client::seed_vault_genesis(&lab.paths[0], &lab._home.path().join("genesis.v2")).unwrap();
    for n in 1..3 {
        client::save_records(
            &lab.paths[n],
            &client::load_records(&lab.paths[0]).unwrap().encode(),
        )
        .unwrap();
    }
    let dirs = [0, 1, 2].map(|n| lab._home.path().join(format!("table-m{n}")));
    let b0 = lab.bootstrap(0);
    let session = Session::open(&b0).unwrap();
    let scope = Store::create(&session, &dirs[0]).unwrap().scope();
    drop(session);
    for (n, dir) in dirs.iter().enumerate().skip(1) {
        let bootstrap = lab.bootstrap(n);
        let session = Session::open(&bootstrap).unwrap();
        Store::join(&session, dir, scope).unwrap();
    }
    Table { lab, dirs, scope }
}

impl Table {
    fn actor(&self, n: usize) -> Actor {
        let bootstrap = self.lab.bootstrap(n);
        Session::open(&bootstrap).unwrap().actor()
    }

    /// Mac `n` writes `payload` in its replica and m0 takes it in.
    fn write(&self, n: usize, payload: &[u8]) {
        let bootstrap = self.lab.bootstrap(n);
        let session = Session::open(&bootstrap).unwrap();
        let mut store = Store::open(&session, &self.dirs[n], self.scope).unwrap();
        let receipt = store.commit_own(&session, payload).unwrap();
        let wire = store.wire(&session, &receipt.id).unwrap().unwrap().to_vec();
        drop((store, session));
        if n != 0 {
            let b0 = self.lab.bootstrap(0);
            let session = Session::open(&b0).unwrap();
            Store::open(&session, &self.dirs[0], self.scope)
                .unwrap()
                .ingest(&session, &wire)
                .unwrap();
        }
    }

    /// Mac `to` takes in every operation Mac `from` holds.
    fn ship(&self, from: usize, to: usize) {
        let bf = self.lab.bootstrap(from);
        let session = Session::open(&bf).unwrap();
        let store = Store::open(&session, &self.dirs[from], self.scope).unwrap();
        let wires: Vec<Vec<u8>> = store
            .metadata_evidence(&session)
            .unwrap()
            .into_iter()
            .map(|evidence| {
                store
                    .wire(&session, &evidence.header.id)
                    .unwrap()
                    .unwrap()
                    .to_vec()
            })
            .collect();
        drop((store, session));
        let bt = self.lab.bootstrap(to);
        let session = Session::open(&bt).unwrap();
        let mut store = Store::open(&session, &self.dirs[to], self.scope).unwrap();
        for wire in wires {
            let _ = store.ingest(&session, &wire);
        }
    }

    /// Mac `n` signs its cutoff of `actor` in its replica.
    fn sign(&self, n: usize, actor: Actor, signing: Signing) -> bool {
        let bootstrap = self.lab.bootstrap(n);
        let session = Session::open(&bootstrap).unwrap();
        let mut store = Store::open(&session, &self.dirs[n], self.scope).unwrap();
        store.sign_cutoff(&session, actor, signing).unwrap()
    }

    /// Mac `n`'s log takes in every record Mac `from`'s holds.
    fn sync_log(&self, from: usize, n: usize) {
        client::save_records(
            &self.lab.paths[n],
            &client::load_records(&self.lab.paths[from])
                .unwrap()
                .encode(),
        )
        .unwrap();
    }

    /// What m0 reads of the table.
    fn read(&self) -> Vec<Vec<u8>> {
        let b0 = self.lab.bootstrap(0);
        let session = Session::open(&b0).unwrap();
        Store::open(&session, &self.dirs[0], self.scope)
            .unwrap()
            .catalog(&session)
            .unwrap()
            .into_iter()
            .map(|record| record.payload().to_vec())
            .collect()
    }

    /// m0 signs its cutoff of m2 in its replica, then removes m2 in its log, as every removal
    /// path does; m2 hears of neither.
    fn remove_m2(&self) {
        let m2 = self.actor(2);
        let b0 = self.lab.bootstrap(0);
        let session = Session::open(&b0).unwrap();
        let mut store = Store::open(&session, &self.dirs[0], self.scope).unwrap();
        assert!(store.sign_cutoff(&session, m2, Signing::Fresh).unwrap());
        assert!(
            !store.sign_cutoff(&session, m2, Signing::Fresh).unwrap(),
            "signed once"
        );
        drop((store, session));
        client::remove_member_in_log(&self.lab.paths[0], m2).unwrap();
    }
}

/// A removed writer's operation past its table cutoff never projects, though it was sealed under
/// an epoch the removed Mac still held; its operations inside the cutoff stay, and the cutoff
/// itself is no row. CONTROL: with the cutoffs off, the operation past the cutoff projects.
#[test]
fn a_removed_writers_operation_past_its_table_cutoff_never_projects() {
    let table = table();
    table.write(0, b"m0-before");
    table.write(2, b"m2-before");
    table.remove_m2();
    table.write(2, b"m2-after");
    let read = table.read();
    assert!(read.contains(&b"m0-before".to_vec()));
    assert!(read.contains(&b"m2-before".to_vec()), "inside the cutoff");
    assert!(!read.contains(&b"m2-after".to_vec()), "past the cutoff");
    assert_eq!(read.len(), 2, "the cutoff is no row: {read:?}");

    TABLE_CUTOFFS_OFF.with(|off| off.set(true));
    let control = table.read();
    TABLE_CUTOFFS_OFF.with(|off| off.set(false));
    assert!(
        control.contains(&b"m2-after".to_vec()),
        "CONTROL: without the cutoff the removed writer's late operation counts: {control:?}"
    );
}

/// The partition rule matches F21: an honest writer's operation sealed under the epoch a removal
/// cut, concurrently with the cutoff (m1 had not heard of it), stays readable; only the removed
/// writer's own operations past the cutoff stop counting. CONTROL: a rule that cuts by epoch
/// (`TABLE_CUT_BY_EPOCH`) withholds the honest operation too.
#[test]
fn an_honest_writers_operation_under_a_cut_epoch_stays_readable() {
    let table = table();
    table.write(0, b"m0-before");
    table.write(2, b"m2-before");
    let cut_epoch = {
        let b1 = table.lab.bootstrap(1);
        *Session::open(&b1).unwrap().access.basis()
    };
    table.remove_m2();
    table.write(1, b"m1-concurrent");
    table.write(2, b"m2-after");
    let read = table.read();
    assert!(
        read.contains(&b"m1-concurrent".to_vec()),
        "the honest operation under the cut epoch stays: {read:?}"
    );
    assert!(!read.contains(&b"m2-after".to_vec()));
    let b1 = table.lab.bootstrap(1);
    let session = Session::open(&b1).unwrap();
    assert_eq!(
        *session.access.basis(),
        cut_epoch,
        "CONTROL: m1 wrote under the epoch the removal cut"
    );
    drop(session);

    TABLE_CUT_BY_EPOCH.with(|on| on.set(true));
    let control = table.read();
    TABLE_CUT_BY_EPOCH.with(|on| on.set(false));
    assert!(
        !control.contains(&b"m1-concurrent".to_vec()),
        "CONTROL: cutting by epoch withholds the honest operation: {control:?}"
    );
    assert!(
        control.contains(&b"m0-before".to_vec()),
        "inside the cutoff's past"
    );
}

/// A cutoff counts only once the reader's log shows its writer removed: one signed before a
/// `Remove` that never came (refused, or a crash between the two) withholds nothing.
#[test]
fn a_cutoff_without_its_remove_changes_nothing() {
    let table = table();
    table.write(2, b"m2-before");
    let m2 = table.actor(2);
    let b0 = table.lab.bootstrap(0);
    let session = Session::open(&b0).unwrap();
    let mut store = Store::open(&session, &table.dirs[0], table.scope).unwrap();
    assert!(store.sign_cutoff(&session, m2, Signing::Fresh).unwrap());
    drop((store, session));
    table.write(2, b"m2-after");
    assert!(table.read().contains(&b"m2-after".to_vec()));
}

/// A cutoff a refused or failed `Remove` left is replaced when the removal is tried again: the
/// remover signs anew at the writer's operations held then, and a reader counts only each
/// remover's newest, so the writer's honest operations between the two attempts stay. CONTROL:
/// the stale cutoff, had it stood, withheld them (the first attempt's cut alone).
#[test]
fn a_cutoff_left_by_a_failed_remove_is_replaced_by_the_next_attempts() {
    let table = table();
    table.write(2, b"m2-first");
    let m2 = table.actor(2);
    let sign = |signing| {
        let b0 = table.lab.bootstrap(0);
        let session = Session::open(&b0).unwrap();
        let mut store = Store::open(&session, &table.dirs[0], table.scope).unwrap();
        store.sign_cutoff(&session, m2, signing).unwrap()
    };
    assert!(sign(Signing::Fresh), "the first attempt's cutoff");
    table.write(2, b"m2-between");
    table.remove_m2();
    table.write(2, b"m2-after");
    let read = table.read();
    assert!(
        read.contains(&b"m2-between".to_vec()),
        "the honest operation between the attempts stays: {read:?}"
    );
    assert!(!read.contains(&b"m2-after".to_vec()));
}

/// Only a remover of a writer cuts it: a cutoff another member signed, one that did not sign the
/// `Remove`, counts for nothing, so no writer can erase another's history with a cutoff of its
/// own. CONTROL: m1's cutoff withholds all of m2's operations, so had it counted, m2's operation
/// inside m0's cutoff would be gone.
#[test]
fn a_cutoff_by_a_member_that_did_not_remove_the_writer_counts_for_nothing() {
    let table = table();
    table.write(2, b"m2-before");
    let m2 = table.actor(2);
    {
        let b1 = table.lab.bootstrap(1);
        let session = Session::open(&b1).unwrap();
        let mut store = Store::open(&session, &table.dirs[1], table.scope).unwrap();
        assert!(
            store
                .sign_cutoff(&session, m2, Signing::Repair { saved: Some(None) })
                .unwrap(),
            "m1 holds none of m2's operations here, so its cutoff names none"
        );
        let id = store
            .metadata_evidence(&session)
            .unwrap()
            .into_iter()
            .find(|evidence| evidence.cutoff)
            .unwrap()
            .header
            .id;
        let wire = store.wire(&session, &id).unwrap().unwrap().to_vec();
        drop((store, session));
        let b0 = table.lab.bootstrap(0);
        let session = Session::open(&b0).unwrap();
        Store::open(&session, &table.dirs[0], table.scope)
            .unwrap()
            .ingest(&session, &wire)
            .unwrap();
    }
    table.remove_m2();
    let read = table.read();
    assert!(
        read.contains(&b"m2-before".to_vec()),
        "m1 removed nobody, so its cutoff cuts nothing: {read:?}"
    );
}

/// The repair signs the frontier kept before the `Remove`, not the removed writer's latest:
/// `saved: Some(None)` (a table where it held none of the writer's operations when it removed it)
/// withholds the operation the writer sealed later. CONTROL: with nothing kept, the repair cuts
/// at the latest held, and the late operation stays.
#[test]
fn the_repair_signs_the_frontier_kept_before_the_remove() {
    for (saved, late_stays) in [(Some(None), false), (None, true)] {
        let table = table();
        let m2 = table.actor(2);
        table.write(2, b"m2-late");
        client::remove_member_in_log(&table.lab.paths[0], m2).unwrap();
        let b0 = table.lab.bootstrap(0);
        let session = Session::open(&b0).unwrap();
        let mut store = Store::open(&session, &table.dirs[0], table.scope).unwrap();
        assert!(
            store
                .sign_cutoff(&session, m2, Signing::Repair { saved })
                .unwrap()
        );
        assert!(
            !store
                .sign_cutoff(&session, m2, Signing::Repair { saved })
                .unwrap(),
            "a repair signs only where none stands"
        );
        drop((store, session));
        assert_eq!(
            table.read().contains(&b"m2-late".to_vec()),
            late_stays,
            "saved {saved:?}"
        );
    }
}

/// A document table reads a cutoff as a causal parent, not as a refused command: the remover's
/// own document history stays writable (before `MetadataEvidence::cutoff`, its payload was a
/// refusal that blocked the remover's writes).
#[test]
fn a_document_table_reads_a_cutoff_as_no_refusal() {
    let table = table();
    let m2 = table.actor(2);
    let b0 = table.lab.bootstrap(0);
    let session = Session::open(&b0).unwrap();
    let mut store = Store::open(&session, &table.dirs[0], table.scope).unwrap();
    assert!(store.sign_cutoff(&session, m2, Signing::Fresh).unwrap());
    fn refuse(_: &[u8]) -> crate::Result<crate::database::Command> {
        Err(crate::Error::new("not a document command"))
    }
    let log = crate::documents::log::Log::read_with(&session, &store, table.scope, refuse).unwrap();
    log.writable(session.actor()).unwrap();
    assert!(
        log.snapshot()
            .operations
            .iter()
            .all(|op| op.refusal.is_none())
    );
}

/// The one entry point every removal path takes signs a cutoff in each table this Mac holds,
/// once; a table it cannot open for writing keeps the frontier it read for the repair, which
/// signs it and forgets it.
/// NEGATIVE CONTROL: with `Session::try_open` refusing a journal nobody holds, the last removal
/// keeps a frontier.
#[test]
fn the_removal_entry_point_cuts_each_held_table_once() {
    use aterm_secrets::client::CutPass;
    let table = table();
    let m2 = table.actor(2);
    let b0 = table.lab.bootstrap(0);
    let session = Session::open(&b0).unwrap();
    let directory = crate::catalog::Directory::new(&table.lab.paths[0], b0.trust());
    directory.snapshot(&session).unwrap();
    let held = directory
        .open_store(&session, directory.catalog_scope())
        .unwrap();
    assert_eq!(
        aterm_uds::file_lock::with_lock_wait_observer(
            |_| panic!("a removal must retain its frontier without waiting for the writer"),
            || store::cut_removed_writer(&table.lab.paths[0], &m2, CutPass::BeforeRemove),
        ),
        0,
        "another local writer holds the journal and the catalog"
    );
    let kept = table.lab.paths[0].dir.join("commons/table-cutoffs");
    assert_eq!(
        std::fs::read_dir(&kept).unwrap().count(),
        1,
        "its frontier is kept"
    );
    drop((held, session));
    assert_eq!(
        store::cut_removed_writer(&table.lab.paths[0], &m2, CutPass::Repair),
        1
    );
    assert_eq!(
        std::fs::read_dir(&kept).unwrap().count(),
        0,
        "and forgotten once signed"
    );
    assert_eq!(
        store::cut_removed_writer(&table.lab.paths[0], &m2, CutPass::Repair),
        0
    );
    assert_eq!(
        store::cut_removed_writer(&table.lab.paths[0], &m2, CutPass::BeforeRemove),
        0,
        "the same frontier is not signed twice"
    );
    assert_eq!(
        std::fs::read_dir(&kept).unwrap().count(),
        0,
        "nor kept: a journal nobody holds opens"
    );
}

/// The repair signs the frontier kept before the effective `Remove` even where a cutoff an earlier,
/// refused attempt left stands, so the writer's honest operations between the two attempts stay.
/// CONTROL: with nothing kept, the held stale cutoff stands and withholds them.
#[test]
fn the_repair_replaces_a_stale_cutoff_with_the_frontier_kept() {
    for kept in [true, false] {
        let table = table();
        let m2 = table.actor(2);
        table.write(2, b"m2-first");
        assert!(
            table.sign(0, m2, Signing::Fresh),
            "the refused attempt's cutoff"
        );
        table.write(2, b"m2-between");
        let saved = {
            let b0 = table.lab.bootstrap(0);
            let session = Session::open(&b0).unwrap();
            Store::open(&session, &table.dirs[0], table.scope)
                .unwrap()
                .last_of(m2)
        };
        client::remove_member_in_log(&table.lab.paths[0], m2).unwrap();
        let signed = table.sign(
            0,
            m2,
            Signing::Repair {
                saved: kept.then_some(saved),
            },
        );
        assert_eq!(signed, kept);
        assert_eq!(
            table.read().contains(&b"m2-between".to_vec()),
            kept,
            "kept {kept}"
        );
    }
}

/// A remover that is itself removed later cannot un-cut the writer it removed by signing a newer
/// cutoff past its own: that one lies outside its allowed prefix and does not count, and its
/// earlier cutoff, inside the prefix, still does. CONTROL: m2's late operation is withheld only
/// through m0's first cutoff, so had m0's later one displaced it, the operation would project.
#[test]
fn a_removed_removers_later_cutoff_neither_cuts_nor_uncuts() {
    let table = table();
    let (m0, m2) = (table.actor(0), table.actor(2));
    table.write(2, b"m2-before");
    table.remove_m2();
    // m1 hears of m0's removal of m2, then removes m0, cutting it in its own replica first.
    table.ship(0, 1);
    table.sync_log(0, 1);
    assert!(table.sign(1, m0, Signing::Fresh));
    client::remove_member_in_log(&table.lab.paths[1], m0).unwrap();
    // m2 writes on; m0, removed now, takes that in and signs a newer cutoff of m2 past it.
    table.write(2, b"m2-after");
    assert!(table.sign(0, m2, Signing::Fresh), "m0's later cutoff");
    table.ship(0, 1);
    let b1 = table.lab.bootstrap(1);
    let session = Session::open(&b1).unwrap();
    let read: Vec<Vec<u8>> = Store::open(&session, &table.dirs[1], table.scope)
        .unwrap()
        .catalog(&session)
        .unwrap()
        .into_iter()
        .map(|record| record.payload().to_vec())
        .collect();
    assert!(read.contains(&b"m2-before".to_vec()), "{read:?}");
    assert!(
        !read.contains(&b"m2-after".to_vec()),
        "m0's first cutoff still bounds m2: {read:?}"
    );
}

/// A share signs no first cutoff of its own, only replaces one an earlier attempt of its left.
#[test]
fn a_share_only_replaces_its_own_earlier_cutoff() {
    let table = table();
    let m2 = table.actor(2);
    table.write(2, b"m2-first");
    assert!(!table.sign(0, m2, Signing::Supersede), "no first cutoff");
    assert!(table.sign(0, m2, Signing::Fresh));
    assert!(!table.sign(0, m2, Signing::Supersede), "nothing moved");
    table.write(2, b"m2-between");
    assert!(
        table.sign(0, m2, Signing::Supersede),
        "the stale one is replaced"
    );
}
