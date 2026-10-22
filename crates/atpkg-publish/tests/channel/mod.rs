// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! The channel's tests: the index plan and its compare-and-swap, the mirror, the `rustc`
//! group's preflight and re-checks, the candidate gate, the seed pin, the release host
//! adapter, the signer's identity, and the producer→client proof.
//!
//! Every host is a [`FakeHost`] (or a wrapper adding one failure mode), every key is minted
//! in-process by the shipping signer, and every "verified" is the client's chain.

mod agh_read_contract;
mod conformance_index_publish;
mod gh;
mod group;
mod identity;
mod index_plan;
mod index_publish;
mod index_render;
mod mirror;
mod producer_to_client;
mod scratch;
mod seed;
mod status;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use atpkg_publish::channel::Log;
use atpkg_publish::ctx::Ctx;
use atpkg_publish::host::fake::FakeHost;
use atpkg_publish::host::{Hosts, Slug};
use atpkg_publish::quad::index_tag;
use atpkg_publish::testkit::{FAR_FUTURE, Keys};

use super::shared::{Made, made_once};

/// A scratch directory removed on drop.
pub fn scratch(label: &str) -> aterm_tempfile::TempDir {
    aterm_tempfile::Builder::new()
        .prefix(&format!("atpkg-publish-{label}-"))
        .tempdir()
        .expect("scratch dir")
}

/// An `index.toml` this machine would sign, with every channel key spelled out.
#[derive(Clone, Debug)]
pub struct Doc {
    pub index_build: u64,
    pub valid_until: String,
    pub programs: Vec<(String, String, Option<String>)>,
    pub min_build: u64,
    pub yanked: Vec<String>,
    pub pin: Vec<(String, u64)>,
    pub pin_by_target: BTreeMap<String, Vec<(String, u64)>>,
    pub extra_channel_line: Option<String>,
}

impl Doc {
    /// Index `n` pinning `pin`, every program a row of the table where it is one.
    pub fn new(n: u64, pin: &[(&str, u64)]) -> Self {
        let programs = pin
            .iter()
            .map(|(p, _)| {
                let def = atpkg_publish::programs::get(p);
                (
                    (*p).to_string(),
                    def.map_or_else(|| (*p).to_string(), |d| d.repo.to_string()),
                    def.and_then(|d| d.group).map(str::to_string),
                )
            })
            .collect();
        Self {
            index_build: n,
            valid_until: FAR_FUTURE.to_string(),
            programs,
            min_build: 0,
            yanked: Vec::new(),
            pin: pin.iter().map(|(p, b)| ((*p).to_string(), *b)).collect(),
            pin_by_target: BTreeMap::new(),
            extra_channel_line: None,
        }
    }

    pub fn overlay(mut self, triple: &str, cells: &[(&str, u64)]) -> Self {
        self.pin_by_target.insert(
            triple.to_string(),
            cells.iter().map(|(p, b)| ((*p).to_string(), *b)).collect(),
        );
        self
    }

    pub fn yanked(mut self, entries: &[&str]) -> Self {
        self.yanked = entries.iter().map(|e| (*e).to_string()).collect();
        self
    }

    pub fn min_build(mut self, m: u64) -> Self {
        self.min_build = m;
        self
    }

    pub fn render(&self, keys: &Keys) -> String {
        let mut s = format!(
            "schema = 2\nindex_build = {}\nvalid_until = \"{}\"\nmachine_id = \"{}\"\nroster_seq = {}\n",
            self.index_build, self.valid_until, keys.machine_id, keys.roster.roster_seq
        );
        for (name, repo, group) in &self.programs {
            s.push_str(&format!(
                "\n[programs.{name}]\nrepo = \"{repo}\"\npolicy = \"prebuilt-only\"\n"
            ));
            if let Some(g) = group {
                s.push_str(&format!("coherence_group = \"{g}\"\n"));
            }
        }
        s.push_str(&format!(
            "\n[[channels]]\nname = \"stable\"\nchannel_build = {}\nmin_build = {}\n",
            self.index_build, self.min_build
        ));
        if let Some(line) = &self.extra_channel_line {
            s.push_str(line);
            s.push('\n');
        }
        if !self.yanked.is_empty() {
            s.push_str(&format!(
                "yanked = [{}]\n",
                self.yanked
                    .iter()
                    .map(|y| format!("\"{y}\""))
                    .collect::<Vec<_>>()
                    .join(", ")
            ));
        }
        let cells = |v: &[(String, u64)]| {
            v.iter()
                .map(|(p, b)| format!("{p} = {b}"))
                .collect::<Vec<_>>()
                .join(", ")
        };
        s.push_str(&format!("pin = {{ {} }}\n", cells(&self.pin)));
        if !self.pin_by_target.is_empty() {
            s.push_str(&format!(
                "pin_by_target = {{ {} }}\n",
                self.pin_by_target
                    .iter()
                    .map(|(t, c)| format!("\"{t}\" = {{ {} }}", cells(c)))
                    .collect::<Vec<_>>()
                    .join(", ")
            ));
        }
        s
    }
}

/// One machine, one fake GitHub, one hermetic context.
pub struct Rig {
    pub tmp: aterm_tempfile::TempDir,
    pub keys: Keys,
    pub host: Arc<FakeHost>,
    pub ctx: Ctx,
}

impl Rig {
    pub fn new(label: &str) -> Self {
        Self::with_keys(label, Keys::mint("m3", &[]))
    }

    pub fn with_keys(label: &str, keys: Keys) -> Self {
        let tmp = scratch(label);
        let home = tmp.path().join("home");
        let host = Arc::new(FakeHost::new());
        let mut ctx = keys.ctx(&home, host.clone());
        // Hermetic: never the operator's dist/ roster.
        ctx.repo_root = tmp.path().join("repo");
        ctx.state_dir = tmp.path().join("state");
        std::fs::create_dir_all(ctx.tokens.channel.parent().expect("secrets dir"))
            .expect("mkdir secrets");
        std::fs::write(&ctx.tokens.channel, "channel-token\n").expect("token");
        Self {
            tmp,
            keys,
            host,
            ctx,
        }
    }

    /// Route the hosts through `wrapper` (a failure mode over this rig's fake).
    pub fn route(&mut self, wrapper: Arc<dyn atpkg_publish::host::ReleaseHost + Send + Sync>) {
        self.ctx.hosts = Hosts::single(wrapper);
    }

    pub fn path(&self, rel: &str) -> PathBuf {
        self.tmp.path().join(rel)
    }

    pub fn index_slug(&self) -> Slug {
        self.ctx.index_slug()
    }

    /// Publish `doc` as the channel's `atpkg-index-<n>`.
    pub fn publish_head(&self, doc: &Doc) {
        self.keys.publish_quad(
            &self.host,
            &self.index_slug(),
            doc.index_build,
            &doc.render(&self.keys),
        );
    }

    /// Write `doc`'s signed quad into `dir`.
    pub fn quad_dir(&self, dir: &Path, doc: &Doc) {
        self.keys.write_quad(dir, &doc.render(&self.keys));
    }

    /// A signed pack of `program` `build` carrying one row per target, laid on `owner`'s
    /// `<repo>` release `atpkg-<program>-<build>` (assets, manifest, signature).
    pub fn lay_pack(&self, owner: &str, program: &str, build: u64, targets: &[&str]) -> Pack {
        let pack = self.pack(program, build, targets);
        let slug = Slug::new(owner, &pack.repo);
        for (name, bytes) in &pack.files {
            self.host.put_asset(&slug, &pack.tag, name, bytes.clone());
        }
        pack
    }

    /// A signed pack, unlaid.
    pub fn pack(&self, program: &str, build: u64, targets: &[&str]) -> Pack {
        let repo = atpkg_publish::programs::get(program)
            .map_or_else(|| program.to_string(), |d| d.repo.to_string());
        let mut files = Vec::new();
        let mut toml = format!(
            "schema = 1\nprogram = \"{program}\"\nbuild_number = {build}\nversion = \"0.1.{build}\"\nexposes = [\"{program}\"]\n"
        );
        for t in targets {
            let asset = format!("{program}-{build}-{t}.tar.zst");
            let bytes = format!("payload {program} {build} {t}").into_bytes();
            let sha = sha256_hex(&bytes);
            toml.push_str(&format!(
                "\n[[artifact]]\ntarget = \"{t}\"\nkind = \"binary\"\nprotocol = \"github-release\"\nasset = \"{asset}\"\nsha256 = \"{sha}\"\nsize = {}\n",
                bytes.len()
            ));
            files.push((asset, bytes));
        }
        let mf = format!("pkg-{program}-{build}.toml");
        let sig = self.keys.sign(toml.as_bytes());
        files.push((format!("{mf}.sig"), sig));
        files.push((mf, toml.into_bytes()));
        Pack {
            repo,
            tag: format!("atpkg-{program}-{build}"),
            files,
        }
    }

    pub fn log() -> Log {
        Log::capture("test")
    }

    /// The index release tag `n` on the channel, if any.
    pub fn index_release(&self, n: u64) -> Option<atpkg_publish::host::fake::FakeRelease> {
        self.host.release_of(&self.index_slug(), &index_tag(n))
    }
}

/// A pack's files, by name.
pub struct Pack {
    pub repo: String,
    pub tag: String,
    pub files: Vec<(String, Vec<u8>)>,
}

/// SHA-256, hex — through the client's own hasher (a file in scratch).
pub fn sha256_hex(bytes: &[u8]) -> String {
    let dir = scratch("sha");
    let path = dir.path().join("f");
    std::fs::write(&path, bytes).expect("write");
    atpkg::sha256_file(&path).expect("hash")
}

/// `git -C <dir> <args…>` for a fixture ([`atpkg_publish::testkit::git`]), with a fixed
/// identity and no signing; its trimmed stdout.
///
/// # Panics
///
/// When git fails — a broken fixture.
pub fn git(dir: &Path, args: &[&str]) -> String {
    let out = atpkg_publish::testkit::git()
        .arg("-C")
        .arg(dir)
        .args([
            "-c",
            "user.name=fixture",
            "-c",
            "user.email=fixture@example.invalid",
            "-c",
            "commit.gpgsign=false",
            "-c",
            "init.defaultBranch=main",
            // No template: a fixture repository carries no sample hooks, no description and
            // no info/exclude, files every test lays down and removes and nothing reads.
            "-c",
            "init.templateDir=",
        ])
        .args(args)
        .env("GIT_TERMINAL_PROMPT", "0")
        .output()
        .expect("git runs");
    assert!(
        out.status.success(),
        "git {args:?} in {}: {}",
        dir.display(),
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

/// A fresh repository at `dir` with `commits` commits of one file, checked out; its
/// HEAD. The history is one `git fast-import` stream, not a commit per process, and each
/// count is made once per process and laid down after (no test reads its dates).
pub fn repo(dir: &Path, commits: usize) -> String {
    static MADE: Made<usize, String> = Mutex::new(BTreeMap::new());
    made_once(&MADE, &commits, dir, |dir| make_repo(dir, commits))
}

/// [`repo`], made.
fn make_repo(dir: &Path, commits: usize) -> String {
    use std::io::Write as _;
    std::fs::create_dir_all(dir).expect("mkdir repo");
    git(dir, &["init", "-q", "-b", "main"]);
    if commits == 0 {
        return git(dir, &["rev-parse", "HEAD"]);
    }
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("a clock past the epoch")
        .as_secs();
    let mut stream = String::new();
    for i in 0..commits {
        let (msg, body) = (format!("c{i}"), format!("{i}\n"));
        stream.push_str(&format!(
            "commit refs/heads/main\nmark :{}\ncommitter fixture <fixture@example.invalid> {now} +0000\ndata {}\n{msg}\n",
            i + 1,
            msg.len()
        ));
        if i > 0 {
            stream.push_str(&format!("from :{i}\n"));
        }
        stream.push_str(&format!("M 100644 inline f\ndata {}\n{body}\n", body.len()));
    }
    let mut child = atpkg_publish::testkit::git()
        .arg("-C")
        .arg(dir)
        // One pack, not a loose object (and a directory) per object: below its unpack limit
        // fast-import explodes what it wrote, and every test lays the tree down and removes it.
        .args(["-c", "fastimport.unpackLimit=0", "fast-import", "--quiet"])
        .stdin(aterm_uds::spawnfd::Stdio::piped())
        .spawn()
        .expect("spawn git fast-import");
    child
        .stdin
        .take()
        .expect("stdin")
        .write_all(stream.as_bytes())
        .expect("feed fast-import");
    assert!(child.wait().expect("wait fast-import").success());
    git(dir, &["reset", "-q", "--hard", "main"]);
    git(dir, &["rev-parse", "HEAD"])
}
