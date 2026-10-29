// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Shared test fixtures for in-process key minting, roster signing, and fake-host channels.

use std::path::Path;
use std::sync::Arc;

use atpkg_keys::roster_ops::{add, empty, revoke};

use crate::ctx::Ctx;
use crate::host::fake::FakeHost;
use crate::host::{Hosts, Slug};
use crate::quad::{QUAD_FILES, index_tag};

/// Fixture `git` command with automatic maintenance disabled.
#[must_use]
pub fn git() -> aterm_uds::spawnfd::Command {
    let mut git = aterm_uds::fixture_git::git();
    git.args(["-c", "maintenance.auto=false"]);
    git
}

/// Fixed fixture clock.
pub const NOW: i64 = 1_792_022_400;

/// Far-future timestamp for fixture expiry fields.
pub const FAR_FUTURE: &str = "2099-01-01T00:00:00Z";

/// A master, one rostered machine, and the master-signed roster naming it.
pub struct Keys {
    /// The master's public key (the test's anchor).
    pub master_pub: String,
    master_key: Vec<u8>,
    /// The machine's roster id.
    pub machine_id: String,
    /// The machine's pkcs8 key.
    pub machine_key: Vec<u8>,
    /// The machine's public key.
    pub machine_pub: String,
    /// The roster.
    pub roster: aterm_update_core::Roster,
    /// Its exact bytes.
    pub roster_bytes: Vec<u8>,
    /// The master's signature over them.
    pub roster_sig: Vec<u8>,
}

impl Keys {
    /// Mint a master and the machine `machine_id`, roster it, and revoke `revoked`.
    ///
    /// # Panics
    ///
    /// When the shipping signer refuses (a broken fixture).
    #[must_use]
    pub fn mint(machine_id: &str, revoked: &[&str]) -> Self {
        let now = u64::try_from(NOW).expect("a positive clock");
        let (master_key, master_pub) = atpkg_keys::generate().expect("master keypair");
        let (machine_key, machine_pub) = atpkg_keys::generate().expect("machine keypair");
        let mut roster =
            add(empty(now), machine_id, &machine_pub, now).expect("roster the machine");
        for id in revoked {
            roster = revoke(roster, id, now).expect("revoke");
        }
        let roster_bytes = roster.to_toml().expect("the roster emits").into_bytes();
        let roster_sig = atpkg_keys::sign(&master_key, &roster_bytes).expect("master signs");
        Self {
            master_pub,
            master_key,
            machine_id: machine_id.to_string(),
            machine_key,
            machine_pub,
            roster,
            roster_bytes,
            roster_sig,
        }
    }

    /// The machine's detached signature over `bytes`.
    ///
    /// # Panics
    ///
    /// When the signer refuses.
    #[must_use]
    pub fn sign(&self, bytes: &[u8]) -> Vec<u8> {
        atpkg_keys::sign(&self.machine_key, bytes).expect("machine signs")
    }

    /// The master's detached signature over `bytes` (a roster fixture of one's own).
    ///
    /// # Panics
    ///
    /// When the signer refuses.
    #[must_use]
    pub fn master_sign(&self, bytes: &[u8]) -> Vec<u8> {
        atpkg_keys::sign(&self.master_key, bytes).expect("master signs")
    }

    /// Build a context rooted at `home` and write the machine key and record into `~/.aterm/release-ring`.
    ///
    /// # Panics
    ///
    /// When the scratch files cannot be written.
    #[must_use]
    pub fn ctx(&self, home: &Path, host: Arc<FakeHost>) -> Ctx {
        let ctx = self.ctx_over(home, host);
        std::fs::create_dir_all(home.join(atpkg_keys::provision::RELEASE_RING_REL))
            .expect("mkdir ~/.aterm/release-ring");
        std::fs::write(&ctx.identity.key, &self.machine_key).expect("write machine key");
        std::fs::write(
            &ctx.identity.record,
            format!(
                "id = \"{}\"\npubkey = \"{}\"\n",
                self.machine_id, self.machine_pub
            ),
        )
        .expect("write machine record");
        ctx
    }

    /// Build a context rooted at `home` without writing the machine key or record.
    #[must_use]
    pub fn ctx_over(&self, home: &Path, host: Arc<FakeHost>) -> Ctx {
        let mut ctx = Ctx::new(home, None, None, NOW, Hosts::single(host));
        ctx.anchor = vec![self.master_pub.clone()];
        ctx
    }

    /// Render a schema-2 `index.toml` for `programs` and `stable` channel `pins`.
    #[must_use]
    pub fn index_toml(
        &self,
        index_build: u64,
        programs: &[(&str, &str)],
        pins: &[(&str, u64)],
    ) -> String {
        let mut s = format!(
            "schema = 2\nindex_build = {index_build}\nvalid_until = \"{FAR_FUTURE}\"\n\
             machine_id = \"{}\"\nroster_seq = {}\n",
            self.machine_id, self.roster.roster_seq
        );
        for (name, repo) in programs {
            s.push_str(&format!(
                "\n[programs.{name}]\nrepo = \"{repo}\"\npolicy = \"prebuilt-only\"\n"
            ));
        }
        let pin = pins
            .iter()
            .map(|(p, b)| format!("{p} = {b}"))
            .collect::<Vec<_>>()
            .join(", ");
        s.push_str(&format!(
            "\n[[channels]]\nname = \"stable\"\nchannel_build = {index_build}\nmin_build = 0\npin = {{ {pin} }}\n"
        ));
        s
    }

    /// The four quad files for `index`, signed, as `(name, bytes)`.
    #[must_use]
    pub fn quad(&self, index: &str) -> [(&'static str, Vec<u8>); 4] {
        let index = index.as_bytes().to_vec();
        let index_sig = self.sign(&index);
        [
            (QUAD_FILES[0], index),
            (QUAD_FILES[1], index_sig),
            (QUAD_FILES[2], self.roster_bytes.clone()),
            (QUAD_FILES[3], self.roster_sig.clone()),
        ]
    }

    /// Write the signed quad for `index` into `dir`.
    ///
    /// # Panics
    ///
    /// When the files cannot be written.
    pub fn write_quad(&self, dir: &Path, index: &str) {
        std::fs::create_dir_all(dir).expect("mkdir quad dir");
        for (name, bytes) in self.quad(index) {
            std::fs::write(dir.join(name), bytes).expect("write quad file");
        }
    }

    /// Publish the signed quad for `index` as `atpkg-index-<n>` on `slug`.
    pub fn publish_quad(&self, host: &FakeHost, slug: &Slug, n: u64, index: &str) {
        for (name, bytes) in self.quad(index) {
            host.put_asset(slug, &index_tag(n), name, bytes);
        }
    }
}
