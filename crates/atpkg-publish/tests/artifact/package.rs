// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! `pack-package` over a package tree laid as codex-alab's `alab/package.sh` lays it (a real
//! ad hoc signed Mach-O, real `codesign`, a real git origin, the client's own stage), and the
//! index that pins it: every gate refuses before anything is signed, the pack the producer
//! signs is what a client stages, and an ALab pin is signed only over a pack that verifies.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use atpkg_publish::api::{Baseline, IndexRequest};
use atpkg_publish::artifact::package::{
    self, PackageOutcome, PackageRequest, binaries_sha256, compiler_id, dev_version, names_repo,
};
use atpkg_publish::channel::Log;
use atpkg_publish::channel::index::{parse_delta, publish_with};
use atpkg_publish::ctx::Ctx;
use atpkg_publish::error::Error;
use atpkg_publish::host::Slug;
use atpkg_publish::host::fake::FakeHost;
use atpkg_publish::quad::{self, QUAD_FILES};
use atpkg_publish::testkit::Keys;

use super::super::shared::{Made, made_once};
use super::fixture::{
    COMMIT, CPU_X86_64, DORMANT_LEDGER, FakeTools, bare_origin, client_verifies, git, macho, put,
    put_exe, scratch, toolchain as fixture_toolchain, trust_repo, version_text,
};

const MAC: &str = "aarch64-apple-darwin";
/// The live head's codex pin: a legacy vendor manifest on repo `aterm`.
const LIVE_CODEX: u64 = 2_026_101_501;
/// The first ALab build above it.
const ALAB: u64 = 2_026_101_601;

/// A rostered machine over the live channel head (index 71: `ay`, and `codex` on repo
/// `aterm`), a fixture Trust toolchain the client admits, and a codex-alab checkout whose
/// origin is `alabsystems/codex-alab`.
struct World {
    _tmp: aterm_tempfile::TempDir,
    root: PathBuf,
    ctx: Ctx,
    host: Arc<FakeHost>,
    /// The toolchain's `bin/`.
    bin: PathBuf,
    /// The codex-alab checkout.
    source: PathBuf,
    /// Its main's head.
    commit: String,
    tools: FakeTools,
}

fn world(label: &str) -> World {
    let tmp = scratch(label);
    let root = std::fs::canonicalize(tmp.path()).expect("root");
    let commit = codex_alab(&root);
    let source = root.join("codex-alab");
    let home = root.join("home");
    let keys = Keys::mint("m3", &[]);
    let host = Arc::new(FakeHost::new());
    let ctx = keys.ctx(&home, host.clone());
    keys.publish_quad(
        &host,
        &ctx.index_slug(),
        71,
        &keys.index_toml(
            71,
            &[("ay", "ay"), ("codex", "aterm")],
            &[("ay", 5), ("codex", LIVE_CODEX)],
        ),
    );
    trust_repo(&home.join("trust"), DORMANT_LEDGER);
    let bin = fixture_toolchain(&root.join("seal"));
    // package.sh hashes the `.dylib` drivers of a macOS sysroot.
    std::fs::rename(
        root.join("seal/lib/librustc_driver-0123456789abcdef.so"),
        root.join("seal/lib/librustc_driver-0123456789abcdef.dylib"),
    )
    .expect("rename the driver");
    World {
        _tmp: tmp,
        root,
        ctx,
        host,
        bin,
        source,
        commit,
        tools: FakeTools::new(MAC),
    }
}

/// Under `root` (which holds nothing yet), the codex-alab checkout `codex-alab` of two
/// commits, pushed to and fetched from its origin, the bare `remotes/alabsystems/
/// codex-alab.git`; its main's head. Thirteen git processes and a push's receive-pack, so
/// the pair is made once per process, by the first world, and laid down for every world
/// after, the checkout's origin moved to the bare beside it.
fn codex_alab(root: &Path) -> String {
    static MADE: Made<(), String> = Mutex::new(BTreeMap::new());
    made_once(&MADE, &(), root, |root| {
        let bare = root.join("remotes/alabsystems/codex-alab.git");
        bare_origin(&bare);
        let source = root.join("codex-alab");
        std::fs::create_dir_all(&source).expect("mkdir checkout");
        git(&source, &["init", "-q", "."]);
        git(
            &source,
            &["remote", "add", "origin", &bare.to_string_lossy()],
        );
        for n in ["one", "two"] {
            put(&source.join("README"), n.as_bytes());
            git(&source, &["add", "-A"]);
            git(&source, &["commit", "-q", "-m", n]);
        }
        git(&source, &["push", "-q", "origin", "HEAD:refs/heads/main"]);
        git(&source, &["fetch", "-q", "origin"]);
        git(&source, &["rev-parse", "HEAD"])
    })
}

/// A copy of `/usr/bin/true` after `codesign <args>` ran on it.
fn codesigned(dir: &Path, args: &[&str]) -> Vec<u8> {
    let exe = dir.join(format!("true-{}", args.join("")));
    std::fs::copy("/usr/bin/true", &exe).expect("copy /usr/bin/true");
    let out = aterm_uds::spawnfd::command("/usr/bin/codesign")
        .args(args)
        .arg(&exe)
        .output()
        .expect("spawn codesign");
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    std::fs::read(&exe).expect("read")
}

fn json_string(s: &str) -> String {
    let mut out = String::from("\"");
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// What a package says about itself; [`World::package`] lays it.
#[derive(Clone)]
struct Lay {
    version: String,
    source: String,
    target: &'static str,
    build_target: &'static str,
    entrypoint: &'static str,
    sysroot: String,
    vv: String,
    binaries: String,
    id: Option<String>,
    exe: Vec<u8>,
    alab_build: bool,
}

impl World {
    /// The record of a package `alab/package.sh` built from main's head with this toolchain.
    fn lay(&self) -> Lay {
        let seal = self.bin.parent().expect("root").to_path_buf();
        Lay {
            version: format!("0.164.0-dev.2+g{}", &self.commit[..9]),
            source: self.commit.clone(),
            target: MAC,
            build_target: MAC,
            entrypoint: "bin/codex",
            sysroot: seal.to_string_lossy().into_owned(),
            vv: version_text(COMMIT, MAC),
            binaries: binaries_sha256(&seal).expect("hash the toolchain"),
            id: None,
            exe: codesigned(&self.root, &["--force", "--sign", "-"]),
            alab_build: true,
        }
    }

    fn package(&self, name: &str, lay: &Lay) -> PathBuf {
        let dir = self.root.join("pkgs").join(name);
        put_exe(&dir.join("bin/codex"), &lay.exe);
        put(&dir.join("LICENSE"), b"Apache-2.0\n");
        put(
            &dir.join("codex-package.json"),
            format!(
                "{{\"layoutVersion\": 1, \"version\": \"{}\", \"target\": \"{}\", \"variant\": \
                 \"codex\", \"entrypoint\": \"{}\", \"resourcesDir\": \"codex-resources\", \
                 \"pathDir\": \"codex-path\"}}\n",
                lay.version, lay.target, lay.entrypoint
            )
            .as_bytes(),
        );
        if lay.alab_build {
            let id = lay
                .id
                .clone()
                .unwrap_or_else(|| compiler_id(&lay.vv, &lay.sysroot, &lay.binaries));
            put(
                &dir.join("alab-build.json"),
                format!(
                    "{{\"source\": \"{}\", \"version\": \"{}\", \"target\": \"{}\", \"compiler\": \
                     {{\"rustc\": \"rustc\", \"rustc_vV\": {}, \"sysroot\": {}, \
                     \"binaries_sha256\": \"{}\", \"id\": \"{id}\"}}, \"ripgrep\": \"15.2.0\"}}\n",
                    lay.source,
                    lay.version,
                    lay.build_target,
                    json_string(&lay.vv),
                    json_string(&lay.sysroot),
                    lay.binaries
                )
                .as_bytes(),
            );
        }
        dir
    }

    fn request(&self, package: &Path, build: u64) -> PackageRequest {
        PackageRequest {
            program: "codex".into(),
            package: package.to_path_buf(),
            build,
            source_repo: self.source.clone(),
            toolchain: Some(self.bin.clone()),
            out: self.root.join("out"),
            key: None,
            upload: false,
        }
    }

    fn pack(&self, request: &PackageRequest) -> Result<PackageOutcome, Error> {
        package::pack_package_with(&self.ctx, request, &self.tools)
    }

    fn signed(&self) -> Vec<PathBuf> {
        atpkg_publish::artifact::relocate::walk_files(&self.root.join("out"))
            .unwrap_or_default()
            .into_iter()
            .filter(|p| p.to_string_lossy().ends_with(".toml"))
            .collect()
    }

    fn staging(&self) -> Slug {
        self.ctx.staging_slug("codex-alab")
    }

    /// Lay `files` (name, bytes) as the staging release of `build`.
    fn stage(&self, build: u64, files: &[(String, Vec<u8>)]) {
        for (name, bytes) in files {
            self.host.put_asset(
                &self.staging(),
                &format!("atpkg-codex-{build}"),
                name,
                bytes.clone(),
            );
        }
    }

    fn index(&self, args: &[&str], pack_dir: Option<PathBuf>) -> (Result<(), Error>, Log) {
        let args: Vec<String> = args.iter().map(|s| (*s).to_string()).collect();
        let req = IndexRequest {
            baseline: Baseline::ChannelHead,
            delta: parse_delta(&args).expect("delta"),
            out: self.root.join("index-out"),
            upload: false,
            pack_dir,
        };
        let mut log = Log::capture("test");
        let r = publish_with(&self.ctx, &req, &mut log).map(|_| ());
        (r, log)
    }
}

fn read(p: &Path) -> Vec<u8> {
    std::fs::read(p).expect("read")
}

fn files_of(o: &PackageOutcome) -> Vec<(String, Vec<u8>)> {
    [&o.asset, &o.manifest, &o.signature]
        .iter()
        .map(|p| {
            (
                p.file_name().expect("name").to_string_lossy().into_owned(),
                read(p),
            )
        })
        .collect()
}

/// package.sh's own digest of a compiler, from its Python (known answers computed with it).
#[test]
fn the_compiler_digest_and_id_are_package_sh_s() {
    let tmp = scratch("pkg-kat");
    let root = tmp.path();
    put(&root.join("bin/trustc"), b"trustc-bytes");
    put(&root.join("lib/librustc_driver-a.dylib"), b"d1");
    put(&root.join("lib/librustc_driver-b.dylib"), b"d2");
    put(&root.join("lib/librustc_driver-c.so"), b"ignored");
    let bins = binaries_sha256(root).expect("hash");
    assert_eq!(
        bins,
        "4ea994d24d878bc8adef5e0e4ad3b70da66e08cc8d0b77db1c424dc88eb67054"
    );
    assert_eq!(
        compiler_id(
            "rustc 1.99.0-dev (trustc 0.1.0)\nbinary: trustc\n",
            "/x/sysroot",
            &bins
        ),
        "6984e4240c05b273"
    );
}

#[test]
fn the_version_form_and_the_origin_are_read_exactly() {
    assert_eq!(
        dev_version("0.164.0-dev.12393+g6efbf7518"),
        Some((12393, "6efbf7518"))
    );
    for bad in [
        "0.164.0",
        "0.164.1-dev.3+g6efbf7518",
        "0.164.0-dev.+g6efbf7518",
        "0.164.0-dev.3+g6efbf75",
        "0.164.0-dev.3+gXefbf7518",
        "0.164.0-rc.3+g6efbf7518",
    ] {
        assert_eq!(dev_version(bad), None, "{bad}");
    }
    for good in [
        "https://github.com/alabsystems/codex-alab.git",
        "https://github.com/alabsystems/codex-alab",
        "git@github.com:alabsystems/codex-alab.git",
        "/srv/remotes/alabsystems/codex-alab.git/",
    ] {
        assert!(names_repo(good, "alabsystems", "codex-alab"), "{good}");
    }
    for bad in [
        "https://github.com/openai/codex.git",
        "https://github.com/alabsystems/xcodex-alab.git",
        "https://github.com/notalabsystems/codex-alab.git",
    ] {
        assert!(!names_repo(bad, "alabsystems", "codex-alab"), "{bad}");
    }
}

/// The happy path: the package is packed whole, the signed pair is what the client verifies
/// under the head's roster, and the archive stages through the client's ALab pack gate into
/// exactly the package's own files. Nothing reaches a host without `--upload`; `--key`
/// names the signing key.
#[test]
fn a_package_packs_whole_and_signs_what_a_client_stages() {
    crate::shared::need_compressor();
    let w = world("pkg-ok");
    let pkg = w.package("ok", &w.lay());
    // The key at another path: the machine's own is moved away.
    let key = w.root.join("scratch-roster.key");
    std::fs::rename(&w.ctx.identity.key, &key).expect("move the key");
    let mut req = w.request(&pkg, ALAB);
    req.key = Some(key);
    let o = w.pack(&req).expect("packs");
    assert_eq!(o.version, format!("0.164.0-dev.2+g{}", &w.commit[..9]));
    assert_eq!(
        o.asset.file_name().unwrap().to_string_lossy(),
        "codex-2026101601.tar.zst"
    );
    let parsed = client_verifies(&w.ctx, &o.manifest, &o.signature).expect("the client verifies");
    assert_eq!(
        (parsed.program.as_str(), parsed.build_number),
        ("codex", ALAB)
    );
    assert_eq!(parsed.exposes, ["codex"]);
    assert_eq!(parsed.version, o.version);
    assert_eq!(parsed.artifacts.len(), 1);
    assert_eq!(parsed.artifacts[0].target, MAC);
    assert_eq!(parsed.artifacts[0].kind, "binary");
    // What the archive lays is the package, file for file.
    let staged = w.root.join("staged/store/codex/2026101601");
    atpkg::verify_and_stage(
        &parsed.artifacts[0],
        &o.asset,
        &staged,
        &atpkg::install::StageHooks::NONE,
    )
    .expect("stages");
    let mut got = atpkg_publish::artifact::relocate::walk_files(&staged).expect("walk");
    let mut want = atpkg_publish::artifact::relocate::walk_files(&pkg).expect("walk");
    got.sort();
    want.sort();
    assert_eq!(got, want);
    assert_eq!(
        read(&staged.join("bin/codex")),
        read(&pkg.join("bin/codex"))
    );
    assert!(
        w.host
            .calls()
            .iter()
            .all(|c| !c.contains("codex-alab") && !c.starts_with("create")),
        "no host is written without --upload: {:?}",
        w.host.calls()
    );
}

/// `--upload` stages the pack on the staging owner's `codex-alab`, release
/// `atpkg-codex-<build>` — and only there (the in-memory host: no test reaches a network).
#[test]
fn upload_goes_to_the_staging_codex_alab_release_only_when_asked() {
    crate::shared::need_compressor();
    let w = world("pkg-upload");
    let pkg = w.package("up", &w.lay());
    let mut req = w.request(&pkg, ALAB);
    req.upload = true;
    let o = w.pack(&req).expect("packs");
    assert!(o.uploaded);
    let release = w
        .host
        .release_of(&w.staging(), "atpkg-codex-2026101601")
        .expect("the staging release");
    let names: Vec<&str> = release.assets.keys().map(String::as_str).collect();
    assert_eq!(
        names,
        [
            "codex-2026101601.tar.zst",
            "pkg-codex-2026101601.toml",
            "pkg-codex-2026101601.toml.sig"
        ]
    );
    assert!(
        w.host
            .release_of(&w.ctx.public_slug("codex-alab"), "atpkg-codex-2026101601")
            .is_none(),
        "the public owner is the index's to write (mirror), never the pack's"
    );
}

/// Every gate refuses before a manifest exists: the build id outside the window, each
/// package fact, the source, the compiler, and the tree.
#[test]
fn each_gate_refuses_before_anything_is_signed() {
    crate::shared::need_compressor();
    let w = world("pkg-gates");
    let base = w.lay();
    let side = {
        git(&w.source, &["checkout", "-q", "-b", "side"]);
        put(&w.source.join("SIDE"), b"side");
        git(&w.source, &["add", "-A"]);
        git(&w.source, &["commit", "-q", "-m", "side"]);
        let c = git(&w.source, &["rev-parse", "HEAD"]);
        git(&w.source, &["checkout", "-q", "main"]);
        c
    };
    type Tweak = Box<dyn Fn(&mut Lay)>;
    let cases: Vec<(&str, u64, Tweak, &str)> = vec![
        (
            "at the ceiling",
            2_026_092_401,
            Box::new(|_| {}),
            "ALab store window",
        ),
        (
            "a vendor id",
            1_000_000_000_000_000_000,
            Box::new(|_| {}),
            "ALab store window",
        ),
        (
            "no alab-build.json",
            ALAB,
            Box::new(|l| l.alab_build = false),
            "not an ALab package",
        ),
        (
            "another entrypoint",
            ALAB,
            Box::new(|l| l.entrypoint = "LICENSE"),
            "entrypoint",
        ),
        (
            "a stable version",
            ALAB,
            Box::new(|l| l.version = "0.164.0".into()),
            "MAJOR.MINOR.0-dev",
        ),
        (
            "a version naming another commit",
            ALAB,
            Box::new(|l| l.version = "0.164.0-dev.2+g000000000".into()),
            "names commit",
        ),
        (
            "an Intel record",
            ALAB,
            Box::new(|l| l.build_target = "x86_64-apple-darwin"),
            "is for x86_64-apple-darwin",
        ),
        (
            "a count off by one",
            ALAB,
            Box::new(|l| l.version = l.version.replace("dev.2", "dev.3")),
            "is commit 2 of its history",
        ),
        (
            "a commit off main",
            ALAB,
            Box::new(move |l| {
                l.source.clone_from(&side);
                l.version = format!("0.164.0-dev.3+g{}", &side[..9]);
            }),
            "is not on alabsystems/codex-alab origin/main",
        ),
        (
            "another compiler's sysroot",
            ALAB,
            Box::new(|l| l.sysroot = "/usr".into()),
            "not the admitted Trust",
        ),
        (
            "another compiler's -vV",
            ALAB,
            Box::new(|l| l.vv = l.vv.replace("2026-09-30", "2026-09-29")),
            "the admitted one says",
        ),
        (
            "other compiler binaries",
            ALAB,
            Box::new(|l| l.binaries = "0".repeat(64)),
            "another build of the compiler",
        ),
        (
            "a forged id",
            ALAB,
            Box::new(|l| l.id = Some("0123456789abcdef".into())),
            "compiler id",
        ),
    ];
    for (what, build, tweak, needle) in cases {
        let mut lay = base.clone();
        tweak(&mut lay);
        let pkg = w.package(&what.replace(' ', "-"), &lay);
        let e = w.pack(&w.request(&pkg, build)).expect_err(what).to_string();
        assert!(e.contains(needle), "{what}: {e}");
        assert!(w.signed().is_empty(), "{what}: nothing signed");
    }

    // The tree: an unsigned entrypoint, an Intel-only object, a stray executable.
    let mut unsigned = base.clone();
    unsigned.exe = codesigned(&w.root, &["--remove-signature"]);
    let pkg = w.package("unsigned", &unsigned);
    let e = w
        .pack(&w.request(&pkg, ALAB))
        .expect_err("unsigned")
        .to_string();
    assert!(e.contains("codesign --verify --strict"), "{e}");
    let pkg = w.package("intel", &base);
    put_exe(&pkg.join("bin/helper"), &macho(CPU_X86_64));
    let e = w
        .pack(&w.request(&pkg, ALAB))
        .expect_err("intel")
        .to_string();
    assert!(e.contains("helper"), "{e}");
    let pkg = w.package("stray", &base);
    put_exe(&pkg.join("codex-path/hook.sh"), b"#!/bin/sh\necho hi\n");
    let e = w
        .pack(&w.request(&pkg, ALAB))
        .expect_err("stray")
        .to_string();
    assert!(e.contains("runs unsigned"), "{e}");
    // The control: the same package with nothing changed packs.
    let pkg = w.package("control", &base);
    w.pack(&w.request(&pkg, ALAB)).expect("the control packs");
}

/// THE INDEX PINS AN ALAB BUILD ONLY OVER A PACK THAT VERIFIES. `--pin codex=<b>` over the
/// live head switches the row to repo `codex-alab` once the staged pack stages as a client
/// stages it; the signed index routes codex to ALab on Apple silicon only. A tampered archive
/// or a manifest signed off the roster is refused and leaves no signed quad behind. A routine
/// publish reads no codex pack and keeps the live row. `--pack-dir` reads a local pack.
#[test]
fn the_index_pins_an_alab_build_only_over_a_pack_that_verifies() {
    crate::shared::need_compressor();
    let w = world("pkg-index");
    let pkg = w.package("pinned", &w.lay());
    let o = w.pack(&w.request(&pkg, ALAB)).expect("packs");
    let out = w.root.join("index-out");

    // A routine publish: no codex pack is read, and the row is the head's.
    let (r, log) = w.index(&["--pin", "ay=6"], None);
    r.expect("routine");
    let text = String::from_utf8(read(&out.join(QUAD_FILES[0]))).expect("utf8");
    assert!(
        text.contains("[programs.codex]\nrepo = \"aterm\"\n"),
        "{text}"
    );
    assert!(text.contains("codex = 2026101501"), "{text}");
    assert!(log.contains("only `--pin codex=<b>`"), "{}", log.text());
    assert!(
        w.host.calls().iter().all(|c| !c.contains("codex-alab")),
        "{:?}",
        w.host.calls()
    );

    // Nothing staged: refused, and the quad written for it is gone.
    let (r, _) = w.index(&["--pin", "codex=2026101601"], None);
    let e = r.expect_err("no pack").to_string();
    assert!(e.contains("REFUSING to pin codex@2026101601"), "{e}");
    assert!(!out.join(QUAD_FILES[0]).exists(), "no signed quad is left");

    // A flipped archive byte.
    let files = files_of(&o);
    let mut flipped = files[0].1.clone();
    let last = flipped.len() - 1;
    flipped[last] ^= 1;
    w.stage(ALAB, &[(files[0].0.clone(), flipped)]);
    w.stage(ALAB, &files[1..]);
    let (r, _) = w.index(&["--pin", "codex=2026101601"], None);
    let e = r.expect_err("flipped").to_string();
    assert!(e.contains("client's stage refuses"), "{e}");
    assert!(!out.join(QUAD_FILES[0]).exists());

    // A manifest signed by a machine the roster does not list.
    let (rogue, _) = atpkg_keys::generate().expect("keypair");
    let rogue_sig = atpkg_keys::sign(&rogue, &files[1].1).expect("sign");
    w.stage(ALAB, &[files[0].clone(), (files[2].0.clone(), rogue_sig)]);
    let (r, _) = w.index(&["--pin", "codex=2026101601"], None);
    let e = r.expect_err("off roster").to_string();
    assert!(e.contains("refuses the package manifest"), "{e}");
    assert!(!out.join(QUAD_FILES[0]).exists());

    // The real pair: signed, and the client routes codex to ALab on Apple silicon only.
    w.stage(ALAB, &files);
    let (r, log) = w.index(&["--pin", "codex=2026101601"], None);
    r.expect("verifies");
    assert!(log.contains("codex@2026101601"), "{}", log.text());
    let q = quad::verify_dir(&w.ctx, &out).expect("the client verifies the index");
    let text = String::from_utf8(read(&out.join(QUAD_FILES[0]))).expect("utf8");
    assert!(
        text.contains("[programs.codex]\nrepo = \"codex-alab\"\n"),
        "{text}"
    );
    assert_eq!(atpkg::alab::index_pin(&q.index, "codex", MAC), Some(ALAB));
    for t in ["x86_64-apple-darwin", "x86_64-unknown-linux-gnu"] {
        assert_eq!(atpkg::alab::index_pin(&q.index, "codex", t), None, "{t}");
    }

    // A local proof's pack dir, with the hosts holding nothing for the build.
    let w2 = world("pkg-index-local");
    let pkg = w2.package("local", &w2.lay());
    let o = w2.pack(&w2.request(&pkg, ALAB)).expect("packs");
    let dir = o.manifest.parent().expect("stage").to_path_buf();
    let (r, log) = w2.index(&["--pin", "codex=2026101601"], Some(dir));
    r.expect("the local pack verifies");
    assert!(
        log.contains("verifies as every client stages it"),
        "{}",
        log.text()
    );
    assert!(
        w2.host.calls().iter().all(|c| !c.contains("codex-alab")),
        "{:?}",
        w2.host.calls()
    );
}
