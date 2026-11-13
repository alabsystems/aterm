// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::{catalog::Descriptor, table::tests::Lab};
use aterm_secrets::commons::roster::Role;
use aterm_spec::interp::State;
use aterm_uds::private_file::PrivateLock;
use std::os::unix::fs::PermissionsExt;
use std::time::Duration;

#[derive(Debug, PartialEq, Eq)]
enum Contents {
    Directory,
    File(Vec<u8>),
    Link(PathBuf),
}

// Adversarial fixtures contain directory symlinks. Preserve their targets
// without following them, including each entry's permissions in the snapshot.
fn files(dir: &Path) -> BTreeMap<PathBuf, (u32, Contents)> {
    let mut snapshot = BTreeMap::new();
    for entry in std::fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        let metadata = std::fs::symlink_metadata(&path).unwrap();
        let contents = if metadata.is_symlink() {
            Contents::Link(std::fs::read_link(&path).unwrap())
        } else if metadata.is_dir() {
            snapshot.extend(files(&path));
            Contents::Directory
        } else {
            Contents::File(std::fs::read(&path).unwrap())
        };
        snapshot.insert(path, (metadata.permissions().mode(), contents));
    }
    snapshot
}

fn check(scenario: i64, mutant: u8) -> State {
    let lab = Lab::new(&[Role::Writer]);
    let paths = &lab.paths[0];
    crate::alab::Registry::at(paths.clone()).open().unwrap();
    let bootstrap = lab.bootstrap(0);
    let session = Session::open(&bootstrap).unwrap();
    let directory = crate::catalog::Directory::new(paths, session.trust());
    let scope = Descriptor::application(session.trust(), "contributor", "data", 1)
        .unwrap()
        .scope;
    let region = if matches!(scenario, 10 | 11 | 20 | 22) {
        Region::Recovery
    } else {
        Region::Primary
    };
    let path = Contributor::location(&session, scope, region).unwrap();
    let claims = region.claims(&session).unwrap();
    let claim = claims.join(claim_name(scope));
    let mut writer = None;
    if matches!(scenario, 2 | 6..=9 | 11 | 12) {
        let store = match region {
            Region::Primary => Store::join(&session, &path, scope),
            Region::Recovery => Store::join_recovery(&session, &path, scope),
        }
        .unwrap();
        if scenario == 8 {
            writer = Some(store);
        } else {
            drop(store);
        }
    }
    match scenario {
        1 => {
            private_file::create_private_dir(&path).unwrap();
        }
        3 | 15 => {
            // Exercise actual interrupted enrollment: the claim is durable
            // before the attempted data-directory creation fails.
            fsx::write_atomic(&path, b"blocked enrollment", 0o600).unwrap();
            assert!(Store::join(&session, &path, scope).is_err());
            assert!(validate_claim(&session, scope, &std::fs::read(&claim).unwrap()).unwrap());
            std::fs::rename(&path, path.with_extension("blocked")).unwrap();
            if scenario == 15 {
                std::fs::rename(&claim, claim.with_extension("saved")).unwrap();
            }
        }
        4 => {
            private_file::create_private_dir(&claims).unwrap();
            fsx::write_atomic(&claim, b"malformed", 0o600).unwrap();
        }
        5 => {
            private_file::create_private_dir(&path).unwrap();
            fsx::write_atomic(&path.join("unclaimed"), b"data", 0o600).unwrap();
        }
        6 => {
            std::fs::rename(&path, path.with_extension("saved")).unwrap();
        }
        7 => {
            std::fs::rename(path.join("table.lock"), path.join("saved-lock")).unwrap();
        }
        19 => {
            std::fs::set_permissions(
                path.parent().unwrap(),
                std::fs::Permissions::from_mode(0o755),
            )
            .unwrap();
        }
        20 => {
            let recovery = paths.dir.join("commons/recovery");
            private_file::create_private_dir(&recovery).unwrap();
            std::fs::set_permissions(recovery, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        21 | 22 => {
            let root = if scenario == 21 {
                paths.dir.join("commons/tables")
            } else {
                paths.dir.join("commons/recovery")
            };
            if root.exists() {
                std::fs::rename(&root, root.with_extension("saved")).unwrap();
            }
            let foreign = paths.home.join("foreign-parent");
            private_file::create_private_dir(&foreign).unwrap();
            std::os::unix::fs::symlink(foreign, root).unwrap();
        }
        _ => {}
    }
    let path = if scenario == 9 {
        directory.path(directory.catalog_scope()).unwrap()
    } else {
        path
    };
    let scope = if scenario == 17 {
        let other = Lab::new(&[Role::Writer]);
        let b = other.bootstrap(0);
        Descriptor::application(b.trust(), "contributor", "data", 1)
            .unwrap()
            .scope
    } else {
        scope
    };
    drop(session);
    let mut present = false;
    let mut held = true;
    let result = crate::generation::with_retained_session(
        paths,
        paths,
        crate::admission::deadline(),
        |session, _, validate| {
            let before = files(&paths.dir);
            let contributor = Contributor::observe(session, &path, scope, region);
            assert_eq!(before, files(&paths.dir), "observation wrote storage");
            let mut contributor = match contributor {
                Ok(contributor) => contributor,
                Err(_) if mutant == 1 && scenario == 3 => {
                    // Real discovery maps the same pending enrollment to None.
                    assert!(Store::observe_if_exists(session, &path, scope)?.is_none());
                    return Ok(());
                }
                Err(_) if mutant == 3 && scenario == 9 => {
                    let original = Contributor::location(session, scope, region)?;
                    Contributor::observe(session, &original, scope, region)?
                }
                Err(error) => return Err(error),
            };
            present = contributor.require_store().is_ok();
            if mutant == 4 {
                return Err(Error::new("injected available refusal"));
            }
            match scenario {
                12 => {
                    std::fs::rename(&path, path.with_extension("saved"))?;
                    private_file::create_private_dir(&path)?;
                }
                13 => {
                    private_file::create_private_dir(&path)?;
                }
                14 => {
                    private_file::create_private_dir(&path)?;
                    fsx::write_atomic(&path.join("new-data"), b"data", 0o600)?;
                }
                15 => {
                    std::fs::rename(claim.with_extension("saved"), &claim)?;
                }
                16 => {
                    private_file::create_private_dir(&claims)?;
                    fsx::write_atomic(&claim, b"malformed", 0o600)?;
                }
                18 => {
                    fsx::write_atomic(&path, b"file in place of a directory", 0o600)?;
                }
                23 => {
                    std::fs::set_permissions(
                        path.parent().unwrap(),
                        std::fs::Permissions::from_mode(0o755),
                    )?;
                    if mutant == 6 {
                        contributor.parents.clear();
                    }
                }
                _ => {}
            }
            if mutant != 2 || !(14..=16).contains(&scenario) {
                contributor.check(session)?;
                contributor.check_custody()?;
            }
            (validate.custody)()?;
            if mutant == 5 {
                // Lose custody after the final validation, immediately before
                // the simulated persistence point.
                contributor.release_guard_for_test();
            }
            if present {
                held = match PrivateLock::take(&path, "table.lock", Duration::ZERO, "probe") {
                    Ok(_) => false,
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => true,
                    Err(error) => panic!("guard probe: {error}"),
                };
            }
            Ok(())
        },
    );
    drop(writer);
    if matches!(scenario, 2 | 11) {
        assert!(PrivateLock::take(&path, "table.lock", Duration::ZERO, "released").is_ok());
    }
    State::from([
        ("scenario", scenario),
        ("done", 1),
        ("accepted", i64::from(result.is_ok())),
        ("present", i64::from(present)),
        ("held", i64::from(held)),
    ])
}

pub(crate) fn transition(state: &State, action: &str) -> State {
    match action {
        "Case" => {
            let mut next = state.clone();
            next.insert("scenario", state["scenario"] + 1);
            next
        }
        "Read" => check(state["scenario"], 0),
        _ => panic!("unknown contributor action {action}"),
    }
}

#[test]
fn physical_contributor_refines_storage_and_retained_custody() {
    let model = aterm_models_db::physical_contributor_model();
    aterm_spec::interp::prove_and_catch(&model);
    let mut cases = 0;
    for state in aterm_spec::interp::reachable_states(&model) {
        for action in &model.actions {
            if model.action_enabled(action.name, &state) {
                let next = transition(&state, action.name);
                assert!(
                    model.successors(action.name, &state).contains(&next),
                    "{state:?} -> {next:?}"
                );
                cases += usize::from(action.name == "Read");
            }
        }
    }
    assert_eq!(cases, 24);
    for (mutant, scenario, law) in [
        (1, 3, "Unavailable"),
        (2, 14, "Unavailable"),
        (2, 15, "Unavailable"),
        (2, 16, "Unavailable"),
        (3, 9, "Unavailable"),
        (4, 0, "Available"),
        (5, 2, "Custody"),
        (6, 23, "Unavailable"),
    ] {
        assert!(
            !model.check_invariant(law, &check(scenario, mutant)),
            "{law}"
        );
    }
}
