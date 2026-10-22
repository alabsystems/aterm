// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! The lanes' fixtures: a scratch home with a rostered machine, the channel on a
//! [`FakeHost`], real git repos for the program sources, and fakes of the packer and the
//! indexer that write what the real ones would (a manifest the client parses, signed by
//! the machine key, uploaded to staging; an index quad with the delta applied).

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use atpkg_publish::api::{
    Baseline, IndexRequest, Indexer, PackOutcome, PackRequest, Packer, Published, Signing,
};
use atpkg_publish::ctx::Ctx;
use atpkg_publish::error::{Error, Result};
use atpkg_publish::host::fake::{FakeHost, FakeRelease};
use atpkg_publish::host::{Replace, Slug};
use atpkg_publish::identity::MachineKey;
use atpkg_publish::lane::report::{Notifier as _, Recorded};
use atpkg_publish::testkit::{Keys, NOW};
use atpkg_publish::triple::Triple;

use super::super::shared::{Made, made_once, scratch};

/// `git -C <dir> <args>` ([`atpkg_publish::testkit::git`]) with a fixed identity, failing the
/// test on an error.
pub fn git(dir: &Path, args: &[&str]) -> String {
    git_at(dir, args, None)
}

/// [`git`] with the author and committer date fixed (`YYYY-MM-DDT12:00:00Z`).
pub fn git_at(dir: &Path, args: &[&str], day: Option<&str>) -> String {
    let mut cmd = atpkg_publish::testkit::git();
    cmd.arg("-C")
        .arg(dir)
        .args([
            "-c",
            "user.name=t",
            "-c",
            "user.email=t@t",
            "-c",
            "commit.gpgsign=false",
            "-c",
            "init.defaultBranch=main",
            "-c",
            "advice.detachedHead=false",
            // No template: a fixture repository carries no sample hooks, no description and
            // no info/exclude, files every test lays down and removes and nothing reads.
            "-c",
            "init.templateDir=",
        ])
        .args(args)
        .env("GIT_TERMINAL_PROMPT", "0");
    if let Some(day) = day {
        let stamp = format!("{day}T12:00:00Z");
        cmd.env("GIT_AUTHOR_DATE", &stamp)
            .env("GIT_COMMITTER_DATE", &stamp);
    }
    let out = cmd.output().expect("spawn git");
    assert!(
        out.status.success(),
        "git {args:?} in {}: {}",
        dir.display(),
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

/// A fresh repository on `main` with `commits` empty commits, written by ONE
/// `git fast-import` (a build number is a commit count, and pins run to the hundreds); its
/// commits, oldest first. The history is the same bytes every time (a fixed identity and
/// fixed dates), so each count is made once per process and laid down after.
pub fn repo(dir: &Path, commits: u64) -> Vec<String> {
    static MADE: Made<u64, Vec<String>> = Mutex::new(BTreeMap::new());
    made_once(&MADE, &commits, dir, |dir| make_repo(dir, commits))
}

/// [`repo`], made: its commits, oldest first.
fn make_repo(dir: &Path, commits: u64) -> Vec<String> {
    use std::io::Write as _;
    std::fs::create_dir_all(dir).expect("mkdir repo");
    git(dir, &["init", "-q"]);
    if commits == 0 {
        return Vec::new();
    }
    let mut stream = String::new();
    for i in 1..=commits {
        let msg = format!("c{i}");
        stream.push_str(&format!(
            "commit refs/heads/main\nmark :{i}\ncommitter t <t@t> {} +0000\ndata {}\n{msg}\n",
            1_780_000_000 + i,
            msg.len()
        ));
        if i > 1 {
            stream.push_str(&format!("from :{}\n", i - 1));
        }
        stream.push('\n');
    }
    // Each mark's commit, read back from fast-import itself rather than one more git.
    let marks = dir.join(".git/fixture-marks");
    let mut child = atpkg_publish::testkit::git()
        .arg("-C")
        .arg(dir)
        // One pack, not a loose object (and a directory) per commit: below its unpack limit
        // fast-import explodes what it wrote, and every test lays the tree down and removes it.
        .args(["-c", "fastimport.unpackLimit=0", "fast-import", "--quiet"])
        .arg(format!("--export-marks={}", marks.display()))
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
    let text = std::fs::read_to_string(&marks).expect("read fast-import marks");
    std::fs::remove_file(&marks).expect("drop fast-import marks");
    let mut history: Vec<(u64, String)> = text
        .lines()
        .map(|line| {
            let (mark, sha) = line.split_once(' ').expect("a `:mark sha` line");
            let mark = mark.trim_start_matches(':').parse().expect("a mark number");
            (mark, sha.to_string())
        })
        .collect();
    history.sort();
    assert_eq!(
        history.len(),
        usize::try_from(commits).expect("a count"),
        "{text}"
    );
    history.into_iter().map(|(_, sha)| sha).collect()
}

/// One more empty commit; its sha.
pub fn commit(dir: &Path, msg: &str, day: Option<&str>) -> String {
    git_at(dir, &["commit", "-q", "--allow-empty", "-m", msg], day);
    git(dir, &["rev-parse", "HEAD"])
}

/// A stable 40-hex "public commit" for `seed` (the engine rewrites history, so the public
/// sha is not the dev sha).
#[must_use]
pub fn hex40(seed: &str) -> String {
    let mut out = String::new();
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for round in 0u8..3 {
        for b in seed.bytes().chain([round]) {
            h ^= u64::from(b);
            h = h.wrapping_mul(0x0100_0000_01b3);
        }
        out.push_str(&format!("{h:016x}"));
    }
    out[..40].to_string()
}

/// A manifest the client parses: `program` `build`, one row per target in `rows`.
#[must_use]
pub fn manifest(program: &str, build: u64, rows: &[Triple]) -> String {
    let mut s = format!(
        "schema = 2\nprogram = \"{program}\"\nversion = \"1.0.0\"\nbuild_number = {build}\n"
    );
    for t in rows {
        s.push_str(&row(program, build, *t, false));
    }
    s
}

/// One `[[artifact]]` row (and its provenance, when `unproven`).
#[must_use]
pub fn row(program: &str, build: u64, t: Triple, unproven: bool) -> String {
    let mut s = format!(
        "\n[[artifact]]\ntarget = \"{t}\"\nkind = \"binary\"\nasset = \"{program}-{build}-{t}.tar.zst\"\nsha256 = \"{}\"\n",
        "ab".repeat(32)
    );
    if unproven {
        s.push_str("\n[artifact.provenance]\nlineage = \"unproven\"\n");
    }
    s
}

/// The scratch world one lane test runs in.
pub struct Fx {
    pub tmp: aterm_tempfile::TempDir,
    /// The master, the machine, the roster.
    pub keys: Arc<Keys>,
    /// Both owners.
    pub host: Arc<FakeHost>,
    /// The machine.
    pub ctx: Ctx,
    pub packer: FakePacker,
    pub indexer: FakeIndexer,
    pub notes: Recorded,
    ledger: Mutex<BTreeMap<String, BTreeMap<String, String>>>,
}

impl Fx {
    /// A rostered machine with both token files, no channel yet.
    #[must_use]
    pub fn new(label: &str) -> Self {
        Self::at(scratch(label))
    }

    /// [`Fx::new`] in `tmp`.
    #[must_use]
    pub fn at(tmp: aterm_tempfile::TempDir) -> Self {
        let keys = Arc::new(Keys::mint("m3", &[]));
        let host = Arc::new(FakeHost::new());
        let home = tmp.path().join("home");
        let ctx = keys.ctx(&home, host.clone());
        std::fs::create_dir_all(home.join(".secrets")).expect("mkdir secrets");
        std::fs::write(&ctx.tokens.staging, "ghp_staging_stub\n").expect("token");
        std::fs::write(&ctx.tokens.channel, "ghp_channel_stub\n").expect("token");
        Self {
            packer: FakePacker::default(),
            indexer: FakeIndexer::new(keys.clone(), host.clone()),
            notes: Recorded::default(),
            ledger: Mutex::new(BTreeMap::new()),
            tmp,
            keys,
            host,
            ctx,
        }
    }

    /// What this world holds besides its directory, for [`Fx::laid`].
    #[must_use]
    pub fn held(&self) -> Held {
        let root = self.tmp.path();
        Held {
            root: root.to_path_buf(),
            keys: self.keys.clone(),
            host: Arc::new(self.host.forked(root, root)),
            packs: self.packer.requests.lock().expect("requests").clone(),
            pack_fail: self.packer.fail.lock().expect("fail").clone(),
            unproven: self.packer.unproven.load(Ordering::SeqCst),
            lacks: self.packer.lacks.lock().expect("lacks").clone(),
            indexes: self.indexer.requests.lock().expect("requests").clone(),
            index_fail: self.indexer.fail.load(Ordering::SeqCst),
            notes: self.notes.shown(),
            ledger: self.ledger.lock().expect("ledger").clone(),
        }
    }

    /// The world `held` was taken from, in `tmp`, which holds its directory laid down
    /// (`super::super::shared::made_once`): the same keys, a fork of its host, and its
    /// fakes as it left them, every path under its root moved under this one.
    #[must_use]
    pub fn laid(tmp: aterm_tempfile::TempDir, held: &Held) -> Self {
        let root = tmp.path().to_path_buf();
        let to = |p: &Path| {
            p.strip_prefix(&held.root)
                .map_or_else(|_| p.to_path_buf(), |rel| root.join(rel))
        };
        let host = Arc::new(held.host.forked(&held.root, &root));
        let ctx = held.keys.ctx_over(&root.join("home"), host.clone());
        let packs = held
            .packs
            .iter()
            .cloned()
            .map(|mut r| {
                r.checkout = to(&r.checkout);
                r.out = to(&r.out);
                r.merge_from = r.merge_from.as_deref().map(to);
                r.toolchain = r.toolchain.as_deref().map(to);
                r
            })
            .collect();
        let indexes = held
            .indexes
            .iter()
            .cloned()
            .map(|mut r| {
                if let Baseline::Dir(dir) = &r.baseline {
                    r.baseline = Baseline::Dir(to(dir));
                }
                r.out = to(&r.out);
                r.pack_dir = r.pack_dir.as_deref().map(to);
                r
            })
            .collect();
        let notes = Recorded::default();
        for note in &held.notes {
            notes.notify("", note);
        }
        Self {
            packer: FakePacker {
                requests: Mutex::new(packs),
                fail: Mutex::new(held.pack_fail.clone()),
                unproven: AtomicBool::new(held.unproven),
                lacks: Mutex::new(held.lacks.clone()),
            },
            indexer: FakeIndexer {
                requests: Mutex::new(indexes),
                fail: AtomicBool::new(held.index_fail),
                ..FakeIndexer::new(held.keys.clone(), host.clone())
            },
            notes,
            ledger: Mutex::new(held.ledger.clone()),
            tmp,
            keys: held.keys.clone(),
            host,
            ctx,
        }
    }

    /// A path in the scratch dir.
    #[must_use]
    pub fn path(&self, rel: &str) -> PathBuf {
        self.tmp.path().join(rel)
    }

    /// The index text for `n` pinning `pins` (each pinned program gets its table repo).
    #[must_use]
    pub fn index_text(&self, n: u64, pins: &[(&str, u64)]) -> String {
        let programs: Vec<(&str, &str)> = pins
            .iter()
            .map(|(p, _)| (*p, atpkg_publish::programs::get(p).map_or(*p, |d| d.repo)))
            .collect();
        self.keys.index_toml(n, &programs, pins)
    }

    /// Publish index `n` pinning `pins` on the channel.
    pub fn publish_head(&self, n: u64, pins: &[(&str, u64)]) {
        self.publish_index_text(n, &self.index_text(n, pins));
    }

    /// Publish index `n` with exactly `text`.
    pub fn publish_index_text(&self, n: u64, text: &str) {
        self.keys
            .publish_quad(&self.host, &self.ctx.index_slug(), n, text);
    }

    /// A dev repo for `repo` with `commits` commits, cloneable from staging; where it is,
    /// and its commits, oldest first.
    #[must_use]
    pub fn dev_repo(&self, repo: &str, commits: u64) -> (PathBuf, Vec<String>) {
        let dir = self.path(&format!("dev/{repo}"));
        let history = self::repo(&dir, commits);
        self.host
            .add_clone_source(&self.ctx.staging_slug(repo), &dir);
        (dir, history)
    }

    /// A signed source release `tag` of `repo` on the public owner, whose SUMS names the
    /// public commit the ledger maps to `dev_sha`. `how` varies what is published.
    pub fn release(&self, repo: &str, tag: &str, dev_sha: &str, how: Release) {
        let public = hex40(&format!("{repo}@{tag}"));
        let slug = self.ctx.public_slug(repo);
        let named_tag = how.header_tag.unwrap_or(tag);
        let mut sums = format!(
            "# repo: {slug}\n# tag: {named_tag}\n# commit: {public}\n{}  {repo}-src.tar.gz\n",
            "cd".repeat(32)
        );
        for i in 0..how.listing {
            sums.push_str(&format!("{}  lib/file-{i}\n", "ef".repeat(32)));
        }
        let mut sig = self.keys.sign(sums.as_bytes());
        if how.tamper {
            sig[0] ^= 0xff;
        }
        let mut assets = BTreeMap::new();
        assets.insert("SHA256SUMS".to_string(), sums.into_bytes());
        if how.signed {
            assets.insert("SHA256SUMS.sig".to_string(), sig);
        }
        self.host.put(
            &slug,
            tag,
            FakeRelease {
                draft: false,
                prerelease: how.prerelease,
                assets,
            },
        );
        if how.ledger {
            self.ledger
                .lock()
                .expect("ledger")
                .entry(repo.to_string())
                .or_default()
                .insert(dev_sha.to_string(), public);
            self.write_ledger();
        }
    }

    fn write_ledger(&self) {
        let ledger = self.ledger.lock().expect("ledger");
        let mut s = String::from("{\"mappings\": {");
        for (i, (repo, rows)) in ledger.iter().enumerate() {
            if i > 0 {
                s.push(',');
            }
            s.push_str(&format!("\"{repo}\": {{"));
            for (j, (dev, public)) in rows.iter().enumerate() {
                if j > 0 {
                    s.push(',');
                }
                s.push_str(&format!("\"{dev}\": {{\"release_rev\": \"{public}\"}}"));
            }
            s.push('}');
        }
        s.push_str("}}");
        let path = self.ctx.home.join("publication/mappings.json");
        std::fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
        std::fs::write(path, s).expect("write ledger");
    }

    /// A signed `pkg-<program>-<build>.toml` on the staging owner carrying `rows` (an
    /// earlier pack, or a person's).
    pub fn stage(&self, program: &str, build: u64, rows: &[Triple]) {
        self.stage_text(program, build, &manifest(program, build, rows));
    }

    /// [`Fx::stage`] with exactly `text`.
    pub fn stage_text(&self, program: &str, build: u64, text: &str) {
        let repo = atpkg_publish::programs::get(program).map_or(program, |d| d.repo);
        let slug = self.ctx.staging_slug(repo);
        let tag = format!("atpkg-{program}-{build}");
        let name = format!("pkg-{program}-{build}.toml");
        self.host
            .put_asset(&slug, &tag, &name, text.as_bytes().to_vec());
        self.host.put_asset(
            &slug,
            &tag,
            &format!("{name}.sig"),
            self.keys.sign(text.as_bytes()),
        );
    }

    /// A published `pkg-<program>-<build>.toml`: the same signed bytes on both owners, as the
    /// mirror leaves them.
    pub fn publish_pkg(&self, program: &str, build: u64, text: &str) {
        self.stage_text(program, build, text);
        let repo = atpkg_publish::programs::get(program).map_or(program, |d| d.repo);
        let slug = self.ctx.public_slug(repo);
        let tag = format!("atpkg-{program}-{build}");
        let name = format!("pkg-{program}-{build}.toml");
        self.host
            .put_asset(&slug, &tag, &name, text.as_bytes().to_vec());
        self.host.put_asset(
            &slug,
            &tag,
            &format!("{name}.sig"),
            self.keys.sign(text.as_bytes()),
        );
    }

    /// The bytes of an asset on the public owner.
    #[must_use]
    pub fn public_asset(&self, program: &str, build: u64, name: &str) -> Option<Vec<u8>> {
        let repo = atpkg_publish::programs::get(program).map_or(program, |d| d.repo);
        self.host.asset(
            &self.ctx.public_slug(repo),
            &format!("atpkg-{program}-{build}"),
            name,
        )
    }

    /// The bytes of a staging asset.
    #[must_use]
    pub fn staging_asset(&self, program: &str, build: u64, name: &str) -> Option<Vec<u8>> {
        let repo = atpkg_publish::programs::get(program).map_or(program, |d| d.repo);
        self.host.asset(
            &self.ctx.staging_slug(repo),
            &format!("atpkg-{program}-{build}"),
            name,
        )
    }

    /// Every host call so far that starts with `prefix`.
    #[must_use]
    pub fn calls(&self, prefix: &str) -> Vec<String> {
        self.host
            .calls()
            .into_iter()
            .filter(|c| c.starts_with(prefix))
            .collect()
    }
}

/// What an [`Fx`] holds besides its directory ([`Fx::held`]): its keys, a fork of its host,
/// and its packer, indexer, notifier and ledger as it left them.
#[derive(Clone)]
pub struct Held {
    root: PathBuf,
    keys: Arc<Keys>,
    host: Arc<FakeHost>,
    packs: Vec<PackRequest>,
    pack_fail: BTreeSet<(String, Triple)>,
    unproven: bool,
    lacks: Option<String>,
    indexes: Vec<IndexRequest>,
    index_fail: bool,
    notes: Vec<String>,
    ledger: BTreeMap<String, BTreeMap<String, String>>,
}

/// How a release is published.
#[derive(Clone, Copy, Debug)]
pub struct Release {
    /// Carries `SHA256SUMS.sig`.
    pub signed: bool,
    /// The signature's bytes are flipped.
    pub tamper: bool,
    /// A prerelease.
    pub prerelease: bool,
    /// The ledger maps the public commit to the dev commit.
    pub ledger: bool,
    /// The SUMS header names this tag instead.
    pub header_tag: Option<&'static str>,
    /// More files the SUMS lists (a toolchain release lists thousands).
    pub listing: usize,
}

impl Release {
    /// A signed, bound, published release.
    pub const GOOD: Self = Self {
        signed: true,
        tamper: false,
        prerelease: false,
        ledger: true,
        header_tag: None,
        listing: 0,
    };
}

/// A packer that writes what the real one would: the asset, the manifest (fresh, or the
/// base's bytes verbatim with this target's row appended), its signature, and the upload.
#[derive(Default)]
pub struct FakePacker {
    /// Every request, in order.
    pub requests: Mutex<Vec<PackRequest>>,
    /// (program, target) packs that fail.
    pub fail: Mutex<BTreeSet<(String, Triple)>>,
    /// Rows are packed by a compiler with no lineage receipt.
    pub unproven: AtomicBool,
    /// What this machine lacks to pack at all ([`Packer::can_pack`]), if anything: every
    /// pack fails on it, as the real one does at its end.
    pub lacks: Mutex<Option<String>>,
}

impl FakePacker {
    /// The (program, target, merged?) of every request.
    #[must_use]
    pub fn legs(&self) -> Vec<(String, Triple, Option<PathBuf>)> {
        self.requests
            .lock()
            .expect("requests")
            .iter()
            .map(|r| (r.program.clone(), r.triple, r.merge_from.clone()))
            .collect()
    }

    /// Make `program`'s `t` pack fail.
    pub fn fail_on(&self, program: &str, t: Triple) {
        self.fail
            .lock()
            .expect("fail")
            .insert((program.to_string(), t));
    }

    /// Stop failing.
    pub fn heal(&self) {
        self.fail.lock().expect("fail").clear();
    }
}

impl Packer for FakePacker {
    fn can_pack(&self) -> std::result::Result<(), String> {
        self.lacks
            .lock()
            .expect("lacks")
            .clone()
            .map_or(Ok(()), Err)
    }

    fn pack(&self, ctx: &Ctx, req: &PackRequest) -> Result<PackOutcome> {
        self.requests.lock().expect("requests").push(req.clone());
        if let Some(lacks) = &*self.lacks.lock().expect("lacks") {
            return Err(Error::cannot_run(format!("fixture: {lacks}")));
        }
        if self
            .fail
            .lock()
            .expect("fail")
            .contains(&(req.program.clone(), req.triple))
        {
            return Err(Error::refused(format!(
                "fixture: the {} pack failed",
                req.triple
            )));
        }
        let build = req.build.expect("the lanes always name the build");
        std::fs::create_dir_all(&req.out).expect("mkdir out");
        let asset = req
            .out
            .join(format!("{}-{build}-{}.tar.zst", req.program, req.triple));
        std::fs::write(
            &asset,
            format!("payload {} {build} {}", req.program, req.triple),
        )
        .expect("write asset");
        let mut text = match &req.merge_from {
            Some(base) => std::fs::read_to_string(base).expect("read merge base"),
            None => manifest(&req.program, build, &[]),
        };
        text.push_str(&row(
            &req.program,
            build,
            req.triple,
            self.unproven.load(Ordering::SeqCst),
        ));
        let manifest_path = req.out.join(format!("pkg-{}-{build}.toml", req.program));
        std::fs::write(&manifest_path, &text).expect("write manifest");
        let signature = match req.signing {
            Signing::MachineKey => {
                Some(MachineKey::load(ctx, &ctx.identity.key)?.sign_file(&manifest_path)?)
            }
            Signing::Unsigned => None,
        };
        if req.upload {
            let repo =
                atpkg_publish::programs::get(&req.program).map_or(req.program.as_str(), |d| d.repo);
            let slug = ctx.staging_slug(repo);
            let tag = format!("atpkg-{}-{build}", req.program);
            let mut files = vec![asset.clone(), manifest_path.clone()];
            files.extend(signature.clone());
            let host = &ctx.hosts.staging;
            let r = if host.release(&slug, &tag).is_ok() {
                host.upload(&slug, &tag, &files, Replace::Clobber)
            } else {
                host.create(&slug, &tag, &tag, "", &files)
            };
            r.map_err(|e| Error::refused(format!("fixture upload: {e}")))?;
        }
        Ok(PackOutcome {
            program: req.program.clone(),
            build,
            version: "1.0.0".into(),
            triple: req.triple,
            asset,
            manifest: manifest_path,
            signature,
            exposes: vec![req.program.clone()],
            lineage: atpkg_publish::api::Lineage::Lineage,
            uploaded: req.upload,
        })
    }
}

/// An indexer that publishes the head's pins with the delta applied, signed by the
/// fixture's machine, as `atpkg-index-<head+1>`.
pub struct FakeIndexer {
    keys: Arc<Keys>,
    host: Arc<FakeHost>,
    /// Every request, in order.
    pub requests: Mutex<Vec<IndexRequest>>,
    /// The next publish fails.
    pub fail: AtomicBool,
}

impl FakeIndexer {
    fn new(keys: Arc<Keys>, host: Arc<FakeHost>) -> Self {
        Self {
            keys,
            host,
            requests: Mutex::new(Vec::new()),
            fail: AtomicBool::new(false),
        }
    }

    /// How many publishes were asked for.
    #[must_use]
    pub fn count(&self) -> usize {
        self.requests.lock().expect("requests").len()
    }

    /// The pins each publish moved.
    #[must_use]
    pub fn deltas(&self) -> Vec<BTreeMap<String, u64>> {
        self.requests
            .lock()
            .expect("requests")
            .iter()
            .map(|r| r.delta.pins.clone())
            .collect()
    }
}

impl Indexer for FakeIndexer {
    fn publish(&self, ctx: &Ctx, req: &IndexRequest) -> Result<Published> {
        self.requests.lock().expect("requests").push(req.clone());
        if self.fail.load(Ordering::SeqCst) {
            return Err(Error::refused("fixture: the index publish failed"));
        }
        let Baseline::Dir(dir) = &req.baseline else {
            return Err(Error::usage(
                "the lanes baseline on the verified head they read",
            ));
        };
        let head = atpkg_publish::quad::verify_dir(ctx, dir)?;
        let n = head.index.index_build + 1;
        let mut pins = head.index.channels[0].pin.clone();
        pins.extend(req.delta.pins.clone());
        let programs: Vec<(String, String)> = head
            .index
            .programs
            .iter()
            .map(|(p, row)| (p.clone(), row.repo.clone()))
            .collect();
        let programs: Vec<(&str, &str)> = programs
            .iter()
            .map(|(p, r)| (p.as_str(), r.as_str()))
            .collect();
        let pins: Vec<(&str, u64)> = pins.iter().map(|(p, b)| (p.as_str(), *b)).collect();
        let text = self.keys.index_toml(n, &programs, &pins);
        if req.upload {
            self.keys
                .publish_quad(&self.host, &ctx.index_slug(), n, &text);
        }
        Ok(Published {
            index_build: n,
            dir: req.out.clone(),
            uploaded: req.upload,
        })
    }
}

/// The fixture clock, `days` later.
#[must_use]
pub fn later(days: i64) -> i64 {
    NOW + days * 86_400
}

/// The public slug of `repo` in `fx`.
#[must_use]
pub fn public(fx: &Fx, repo: &str) -> Slug {
    fx.ctx.public_slug(repo)
}
