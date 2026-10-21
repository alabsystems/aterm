// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Shared test fixtures: a paper master, a machine and a roster minted in-process with
//! the shipping signer (`atpkg_keys`), and a channel on a [`crate::host::fake::FakeHost`],
//! so every test verifies through the client's real chain with no key literal and no
//! network. It lives in the library (never shipped) so unit and integration tests share it.

use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};

use atpkg_keys::roster_ops::{add, empty, revoke};

use crate::ctx::Ctx;
use crate::host::fake::FakeHost;
use crate::host::{Hosts, Slug};
use crate::quad::{QUAD_FILES, index_tag};

/// The executable a `git` from `PATH` ends up running, resolved once per process.
///
/// macOS's `/usr/bin/git` is xcode-select's trampoline: every call execs it, it looks up
/// the developer directory, and it execs that directory's git with the same argv, adding
/// only SDK variables git does not read. A fixture's git and the [`FakeHost`]'s clones and
/// fetches run that git directly (`xcrun --find git`, what the trampoline asks). Where the
/// `git` on `PATH` is no trampoline, or the lookup fails, it is that `git`.
#[must_use]
pub fn git_path() -> &'static Path {
    static GIT: OnceLock<PathBuf> = OnceLock::new();
    GIT.get_or_init(|| {
        let on_path = std::env::var_os("PATH")
            .and_then(|path| {
                std::env::split_paths(&path)
                    .map(|dir| dir.join("git"))
                    .find(|candidate| candidate.is_file())
            })
            .unwrap_or_else(|| PathBuf::from("git"));
        if !cfg!(target_os = "macos") || on_path != Path::new("/usr/bin/git") {
            return on_path;
        }
        aterm_uds::spawnfd::command("/usr/bin/xcrun")
            .args(["--find", "git"])
            .stdin(aterm_uds::spawnfd::Stdio::null())
            .output()
            .ok()
            .filter(|out| out.status.success())
            .map(|out| PathBuf::from(String::from_utf8_lossy(&out.stdout).trim()))
            .filter(|found| found.is_file())
            .unwrap_or(on_path)
    })
}

/// `git` for a fixture or the [`FakeHost`]: [`git_path`], with automatic maintenance off
/// (`maintenance.auto=false`) — every commit and fetch would otherwise start a `git
/// maintenance run --auto` that does nothing in a repository this small.
#[must_use]
pub fn git() -> aterm_uds::spawnfd::Command {
    let mut git = aterm_uds::spawnfd::command(git_path());
    git.args(["-c", "maintenance.auto=false"]);
    git
}

/// A fixed clock: 2026-10-15T00:00:00Z.
pub const NOW: i64 = 1_792_022_400;

/// A horizon no fixture outlives.
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

    /// A context rooted at `home` whose anchor is this master, clock [`NOW`], and whose
    /// hosts are `host`; the machine key and record are written where the context looks.
    ///
    /// # Panics
    ///
    /// When the scratch files cannot be written.
    #[must_use]
    pub fn ctx(&self, home: &Path, host: Arc<FakeHost>) -> Ctx {
        let mut ctx = Ctx::new(home, None, None, NOW);
        ctx.anchor = vec![self.master_pub.clone()];
        ctx.hosts = Hosts::single(host);
        std::fs::create_dir_all(home.join(".aterm")).expect("mkdir ~/.aterm");
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

    /// A schema-2 `index.toml` this machine would sign: `programs` are `(name, repo)`,
    /// one channel `stable` pinning `pins`.
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
