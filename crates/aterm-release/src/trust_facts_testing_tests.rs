// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

/// Creates a mock prover `bin/` directory under `root` for unit tests.
pub(crate) fn prover_bin(root: &Path, tag: &str, commit: &str) -> PathBuf {
    use std::os::unix::fs::PermissionsExt;
    let bin = root.join(format!("prover-{tag}/bin"));
    fs::create_dir_all(&bin).unwrap();
    fs::write(bin.join("targo"), format!("#!/bin/sh\n# {tag}\nexit 0\n")).unwrap();
    fs::write(
        bin.join("trustc"),
        format!("#!/bin/sh\necho 'commit-hash: {commit}'\n"),
    )
    .unwrap();
    for tool in ["targo", "trustc"] {
        fs::set_permissions(bin.join(tool), fs::Permissions::from_mode(0o755)).unwrap();
    }
    bin
}

/// Builds a [`Prover`] matching [`prover_bin`] without spawning `trustc`.
pub(crate) fn prover(root: &Path, tag: &str, commit: &str) -> Prover {
    let bin = prover_bin(root, tag, commit);
    Prover {
        identity: prover_identity(&bin).map(|(id, _)| id),
        trustc: commit.to_string(),
        bin,
    }
}

/// Files a synthetic whole-tree [`Facts`] record in `repo` for unit tests.
pub(crate) fn file(
    git: &dyn GitRunner,
    store: &Path,
    prover: &Prover,
    commit: &str,
    driver_id: &str,
    crates: Vec<Crate>,
) -> PathBuf {
    let tsr = tree_sans_floors(git, commit).unwrap();
    let id = prover.identity.clone().unwrap();
    let facts = Facts {
        driver: prover.bin.join("targo").display().to_string(),
        driver_id: driver_id.into(),
        trustd: "/tmp/t.sock".into(),
        grants: "1".into(),
        crates,
        provenance: Some(Provenance {
            key: facts_key(&tsr, &id, LANE_WHOLE_TREE),
            tree_sans_floors: tsr,
            prover: id,
            lane: LANE_WHOLE_TREE.into(),
            prover_root: prover.bin.parent().unwrap().display().to_string(),
            trustc: prover.trustc.clone(),
            head: commit.into(),
            ..Provenance::default()
        }),
        ..Facts::default()
    };
    file_record(store, &facts).unwrap()
}

/// Creates a gated [`Crate`] with `summary` for unit tests.
pub(crate) fn gated(name: &str, summary: &str) -> Crate {
    Crate::new(
        name,
        Decision::Gate(Measured {
            rc: "0".into(),
            summary: summary.into(),
            ..Measured::default()
        }),
    )
}
