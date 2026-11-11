// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A private registry with genuine signatures; no production anchor override.
use atpkg::flow::Fetcher;
use ring::signature::{Ed25519KeyPair, KeyPair as _};

pub(super) struct Registry {
    master: Ed25519KeyPair,
    machine: Ed25519KeyPair,
    roster: Vec<u8>,
    index: Vec<u8>,
    package: Vec<u8>,
}

impl Registry {
    pub(super) fn new(root: &std::path::Path) -> Self {
        let master = Ed25519KeyPair::from_seed_unchecked(&[0x37; 32]).unwrap();
        let machine = Ed25519KeyPair::from_seed_unchecked(&[0x49; 32]).unwrap();
        let roster = aterm_update_core::roster::Roster {
            schema: 1,
            roster_seq: 3,
            valid_until: "2099-01-01T00:00:00Z".into(),
            machines: vec![aterm_update_core::roster::Machine {
                id: "private-host-test".into(),
                pubkey: aterm_codec::base64::encode(machine.public_key().as_ref()).unwrap(),
                added_at: "2026-08-04T00:00:00Z".into(),
                not_after: None,
            }],
            revoked: vec![],
        }
        .to_toml()
        .unwrap()
        .into_bytes();
        let tree = atpkg::tree_root(root).unwrap();
        let identity = "machine_id=\"private-host-test\"\nroster_seq=3\n";
        let index = format!("schema=2\nindex_build=41\nvalid_until=\"2099-01-01T00:00:00Z\"\n{identity}[programs.ay]\nrepo=\"ay\"\n[[channels]]\nname=\"stable\"\npin={{ay=1}}\n").into_bytes();
        let package = format!("schema=2\n{identity}program=\"ay\"\nversion=\"0.1.0\"\nbuild_number=1\nexposes=[\"ay\"]\n[[artifact]]\ntarget=\"aarch64-apple-darwin\"\nkind=\"sysroot-bundle\"\nasset=\"unused.tar.zst\"\nsha256=\"{}\"\ntree_root=\"{tree}\"\nsize=1\n", "a".repeat(64)).into_bytes();
        Self {
            master,
            machine,
            roster,
            index,
            package,
        }
    }

    pub(super) fn acquire(
        &self,
        layout: &atpkg::store::Layout,
    ) -> std::io::Result<atpkg::prerequisite::HeldTool> {
        let anchor = atpkg::Anchor::of(
            vec![aterm_codec::base64::encode(self.master.public_key().as_ref()).unwrap()],
            0,
        );
        atpkg::prerequisite::Session::resolve(
            self,
            layout,
            &anchor,
            atpkg::BuildFloor {
                index_build: 0,
                roster_seq: 0,
            },
            1_785_801_600,
            "aarch64-apple-darwin",
        )
        .map_err(std::io::Error::other)?
        .acquire_with_lease_wait(
            atpkg::prerequisite::Prerequisite::Ay,
            std::time::Duration::ZERO,
        )
        .map_err(std::io::Error::other)
    }
}

impl Fetcher for Registry {
    fn index_candidates(&self) -> Result<Vec<atpkg::Candidate>, String> {
        Ok(vec![atpkg::Candidate {
            label: "private41".into(),
            index_bytes: self.index.clone(),
            sig: self.machine.sign(&self.index).as_ref().to_vec(),
            roster_bytes: self.roster.clone(),
            roster_sig: self.master.sign(&self.roster).as_ref().to_vec(),
        }])
    }
    fn pkg_manifest(
        &self,
        repo: &str,
        program: &str,
        build: u64,
    ) -> Result<(Vec<u8>, Vec<u8>), String> {
        assert_eq!((repo, program, build), ("ay", "ay", 1));
        Ok((
            self.package.clone(),
            self.machine.sign(&self.package).as_ref().to_vec(),
        ))
    }
    fn download(&self, _: &str, _: &str, _: &std::path::Path) -> Result<(), String> {
        Err("qualification never installs or downloads a payload".into())
    }
}
