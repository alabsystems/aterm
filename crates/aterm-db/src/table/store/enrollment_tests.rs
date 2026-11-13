// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::{catalog::Directory, table::tests::Lab};
use aterm_secrets::commons::roster::Role;

/// E1: the enrollment probe answers as the observing open does, without opening a store
/// ([`Store::enrollment_region`] beside [`Store::observe_if_exists_region`]). In each region, the
/// primary and recovery (whose own steps are its canonical location, its claim's ATSR magic and
/// its claims directory), over these states of a table's local storage: no folder, an empty
/// reservation, a pending claim, a ready claim, a damaged claim, a missing claim beside a non-empty
/// folder, a claim of another scope, a refused context, a ready claim whose data folder is gone,
/// in recovery a noncanonical location (its ready store's folder named through `..`), and a ready
/// claim beside a journal that lost its acknowledged record, the probe is `Absent` or `Pending`
/// exactly where the observation is `None`, refuses only where the observation refuses, and opens
/// and checks no store. A ready claim is `Enrolled` and leaves its records to the open, which can
/// still refuse them: each region's last row, the lost journal, is the only one where the two
/// disagree on refusing, and both say the store exists.
/// NEGATIVE CONTROLS: ENROLLED_IF_CLAIMED, the probe reading a pending claim as `Enrolled`, fails
/// the pending row. PRIMARY_RULES, the probe judging every region's storage by the primary's
/// location, magic and claims directory, fails the recovery region's ready claim.
#[test]
fn enrollment_answers_as_the_observing_open() {
    let lab = Lab::new(&[Role::Writer]);
    let bootstrap = lab.bootstrap(0);
    let session = Session::open(&bootstrap).unwrap();
    let directory = Directory::new(&lab.paths[0], bootstrap.trust());
    let mut answers = Vec::new();
    for (name, region, tables) in [
        ("primary", Region::Primary, directory.clone()),
        ("recovery", Region::Recovery, directory.recovery()),
    ] {
        let claims = region.claims(&session).unwrap();
        let table = || {
            let scope = Scope::fresh(bootstrap.trust()).unwrap();
            let path = tables.path(scope).unwrap();
            let parent = path.parent().unwrap();
            aterm_journal::create_private_dir_durable(parent.parent().unwrap()).unwrap();
            aterm_journal::create_private_dir_durable(parent).unwrap();
            (scope, path)
        };
        let join = |path: &Path, scope: Scope| match region {
            Region::Primary => Store::join(&session, path, scope),
            Region::Recovery => Store::join_recovery(&session, path, scope),
        };
        // A ready claim beside one acknowledged record: authored in the primary, retained from
        // the primary's store in recovery, which authors none.
        let enrolled = || {
            let (scope, path) = table();
            let primary = directory.path(scope).unwrap();
            let mut source = Store::join(&session, &primary, scope).unwrap();
            source.commit_catalog(&session, b"enrolled").unwrap();
            if region == Region::Recovery {
                assert!(
                    join(&path, scope)
                        .unwrap()
                        .retain_replica_batch(&session, &source, 1)
                        .unwrap()
                );
            }
            (scope, path)
        };
        let claim = |scope: Scope| std::fs::read(claims.join(claim_name(scope))).unwrap();
        let write_claim = |scope: Scope, claim: &[u8]| {
            aterm_uds::private_file::write_private(&claims, &claim_name(scope), claim).unwrap();
        };

        let mut rows = vec![("no folder", table())];
        let (scope, path) = table();
        aterm_journal::create_private_dir_durable(&path).unwrap();
        rows.push(("empty reservation", (scope, path)));
        // A first enrollment cut short after its pending claim: its data path was taken.
        let (scope, path) = table();
        std::fs::write(&path, b"occupied data path").unwrap();
        assert!(join(&path, scope).is_err());
        std::fs::remove_file(&path).unwrap();
        rows.push(("pending claim", (scope, path)));
        rows.push(("ready claim", enrolled()));
        let (scope, path) = enrolled();
        let mut damaged = claim(scope);
        damaged[0] ^= 1;
        write_claim(scope, &damaged);
        rows.push(("damaged claim", (scope, path)));
        let (scope, path) = enrolled();
        std::fs::remove_file(claims.join(claim_name(scope))).unwrap();
        rows.push(("missing claim beside a non-empty folder", (scope, path)));
        let (other, _) = enrolled();
        let (scope, path) = table();
        write_claim(scope, &claim(other));
        rows.push(("foreign-scope claim", (scope, path)));
        let mut network = bootstrap.trust();
        network.genesis[0] ^= 1;
        let scope = Scope::new(
            network,
            *Scope::fresh(bootstrap.trust()).unwrap().table(),
            1,
        )
        .unwrap();
        rows.push((
            "context refused",
            (scope, lab._home.path().join("another-network")),
        ));
        let (scope, path) = enrolled();
        std::fs::remove_dir_all(&path).unwrap();
        rows.push(("ready claim whose data folder is gone", (scope, path)));
        if region == Region::Recovery {
            let (scope, path) = enrolled();
            let parent = path.parent().unwrap();
            let elsewhere = parent
                .join("..")
                .join(parent.file_name().unwrap())
                .join(path.file_name().unwrap());
            assert!(elsewhere.is_dir() && elsewhere != path);
            rows.push(("noncanonical location", (scope, elsewhere)));
        }
        let (scope, path) = enrolled();
        std::fs::write(path.join("table.astream"), b"").unwrap();
        rows.push(("ready claim beside a lost journal", (scope, path)));

        answers.extend(rows.iter().map(|(row, (scope, path))| {
            let before = test_opens_and_checks();
            let probe = Store::enrollment_region(&session, path, *scope, region);
            assert_eq!(
                test_opens_and_checks(),
                before,
                "{name} {row}: the probe opens and checks no store"
            );
            let hint = Store::repair_ids_region(&session, path, *scope, region);
            assert_eq!(
                test_opens_and_checks(),
                before,
                "{name} {row}: a repair hint opens and checks no store"
            );
            let observed = Store::observe_if_exists_region(&session, path, *scope, region);
            if let Ok(Some(store)) = &observed {
                assert_eq!(
                    hint.as_ref().unwrap().as_ref().unwrap(),
                    &store
                        .inventory_unchecked()
                        .copied()
                        .collect::<BTreeSet<_>>(),
                    "{name} {row}: healthy receipts match the full operation inventory"
                );
            }
            let observed = observed.map(|store| store.is_some());
            assert_eq!(
                hint.map(|ids| ids.is_some()).map_err(drop),
                observed.as_ref().copied().map_err(drop),
                "{name} {row}: hint custody follows the full observation"
            );
            assert_eq!(
                matches!(probe, Ok(Enrollment::Absent | Enrollment::Pending)),
                matches!(observed, Ok(false)),
                "{name} {row}: the probe is absent or pending exactly where the observation \
                 finds no store: {probe:?} beside {observed:?}"
            );
            assert!(
                probe.is_ok() || observed.is_err(),
                "{name} {row}: the probe refuses only where the observation refuses: {probe:?} \
                 beside {observed:?}"
            );
            (name, *row, probe.map_err(drop), observed.map_err(drop))
        }));
    }
    let (absent, pending, enrolled) = (
        Ok::<_, ()>(Enrollment::Absent),
        Ok::<_, ()>(Enrollment::Pending),
        Ok::<_, ()>(Enrollment::Enrolled),
    );
    assert_eq!(
        answers,
        [
            ("primary", "no folder", absent, Ok(false)),
            ("primary", "empty reservation", absent, Ok(false)),
            ("primary", "pending claim", pending, Ok(false)),
            ("primary", "ready claim", enrolled, Ok(true)),
            ("primary", "damaged claim", Err(()), Err(())),
            (
                "primary",
                "missing claim beside a non-empty folder",
                Err(()),
                Err(())
            ),
            ("primary", "foreign-scope claim", Err(()), Err(())),
            ("primary", "context refused", Err(()), Err(())),
            (
                "primary",
                "ready claim whose data folder is gone",
                Err(()),
                Err(())
            ),
            (
                "primary",
                "ready claim beside a lost journal",
                enrolled,
                Err(())
            ),
            ("recovery", "no folder", absent, Ok(false)),
            ("recovery", "empty reservation", absent, Ok(false)),
            ("recovery", "pending claim", pending, Ok(false)),
            ("recovery", "ready claim", enrolled, Ok(true)),
            ("recovery", "damaged claim", Err(()), Err(())),
            (
                "recovery",
                "missing claim beside a non-empty folder",
                Err(()),
                Err(())
            ),
            ("recovery", "foreign-scope claim", Err(()), Err(())),
            ("recovery", "context refused", Err(()), Err(())),
            (
                "recovery",
                "ready claim whose data folder is gone",
                Err(()),
                Err(())
            ),
            ("recovery", "noncanonical location", Err(()), Err(())),
            (
                "recovery",
                "ready claim beside a lost journal",
                enrolled,
                Err(())
            ),
        ],
        "each row reaches the state it names"
    );
}
