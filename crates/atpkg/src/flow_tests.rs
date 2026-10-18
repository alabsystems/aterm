// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::*;
use ring::signature::{Ed25519KeyPair, KeyPair};
use std::collections::HashMap;
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;

use crate::sig::testkit;

/// The synthetic paper master, and the one machine its roster authorizes. The whole
/// crate signs with this same pair, so a flow test cannot prove something under a
/// trust shape no other layer uses. (`ROOT_SEED` still names the ROOT of trust — it
/// is just the paper master now, not a package-specific root.)
const ROOT_SEED: [u8; 32] = testkit::MASTER_SEED;
const RELEASE_SEED: [u8; 32] = testkit::MACHINE_SEED;

/// A durable build floor of `index_build`, recorded under the generation these fixtures
/// publish at — i.e. a floor that actually BINDS. Written as a helper rather than a bare
/// integer because `BuildFloor` carries the generation that set it: a floor stamped with
/// some OTHER generation is waived, and a floor test that accidentally built one would
/// pass vacuously. `fl(0)` is "nothing recorded", which admits everything.
fn fl(index_build: u64) -> BuildFloor {
    BuildFloor {
        index_build,
        roster_seq: testkit::SEQ,
    }
}

/// The anchor every flow test resolves under: armed with the synthetic master, roster
/// floor 0. `anchor_of` is the seam the negative tests use to arm a DIFFERENT master.
fn anchor() -> Anchor {
    anchor_of(&ROOT_SEED)
}

fn anchor_of(master_seed: &[u8; 32]) -> Anchor {
    Anchor::of(vec![pk(master_seed)], 0)
}

/// The attribution head every fixture index carries — `machine_id` + `roster_seq`
/// must name the machine that actually signs, or the bind refuses the index.
fn attribution() -> String {
    format!(
        "machine_id = \"{}\"\nroster_seq = {}\n",
        testkit::MACHINE_ID,
        testkit::SEQ
    )
}
const TRIPLE: &str = "aarch64-apple-darwin";

fn kp(seed: &[u8; 32]) -> Ed25519KeyPair {
    Ed25519KeyPair::from_seed_unchecked(seed).unwrap()
}
fn pk(seed: &[u8; 32]) -> String {
    aterm_codec::base64::encode(kp(seed).public_key().as_ref()).expect("32-byte key")
}
fn sign(seed: &[u8; 32], msg: &[u8]) -> Vec<u8> {
    kp(seed).sign(msg).as_ref().to_vec()
}

fn scratch(label: &str) -> crate::testdir::Dir {
    crate::testdir::dir(&format!("atpkg-flow-{label}-"))
}

/// A raw USTAR + zstd archive with `bin/ay` (+ a sensitive `bin/git` to prove the shim
/// gate). Returns the path.
fn make_archive(dir: &Path) -> PathBuf {
    make_archive_with(dir, b"#!/bin/true\nay", b"0000755\0")
}

/// As [`make_archive`], but with explicit `bin/ay` content + tar mode (the
/// resolve-check rollback tests ship a native-object magic with NO exec bit, so the
/// spawn fails and the fail-loud check takes its spawn-failure arm).
fn make_archive_with(dir: &Path, ay_content: &[u8], ay_mode: &[u8; 8]) -> PathBuf {
    fn entry(name: &str, content: &[u8], mode: &[u8; 8]) -> Vec<u8> {
        let mut h = [0u8; 512];
        let nb = name.as_bytes();
        h[..nb.len()].copy_from_slice(nb);
        h[100..108].copy_from_slice(mode);
        h[108..116].copy_from_slice(b"0000000\0");
        h[116..124].copy_from_slice(b"0000000\0");
        h[124..136].copy_from_slice(format!("{:011o}\0", content.len()).as_bytes());
        h[136..148].copy_from_slice(b"00000000000\0");
        h[148..156].copy_from_slice(b"        ");
        h[156] = b'0';
        h[257..263].copy_from_slice(b"ustar\0");
        h[263..265].copy_from_slice(b"00");
        let sum: u32 = h.iter().map(|&b| u32::from(b)).sum();
        h[148..156].copy_from_slice(format!("{sum:06o}\0 ").as_bytes());
        let mut out = h.to_vec();
        out.extend_from_slice(content);
        out.resize(out.len() + (512 - content.len() % 512) % 512, 0);
        out
    }
    let mut tar = Vec::new();
    tar.extend(entry("bin/ay", ay_content, ay_mode));
    tar.extend(entry(
        "bin/git",
        b"#!/bin/true\nnot-really-git",
        b"0000755\0",
    )); // sensitive → refused shim
    tar.resize(tar.len() + 1024, 0);
    let path = dir.join("ay-18.tar.zst");
    std::fs::write(&path, crate::extract::fixtures::zstd_bytes(&tar)).unwrap();
    path
}

/// A fake fetcher serving a fixed signed index, a fixed signed pkg manifest, and a
/// local archive (copied on download).
struct Fake {
    index: Vec<u8>,
    index_sig: Vec<u8>,
    pkg: HashMap<(String, u64), (Vec<u8>, Vec<u8>)>,
    archives: HashMap<String, PathBuf>,
}
impl Fetcher for Fake {
    fn index_candidates(&self) -> Result<Vec<Candidate>, String> {
        // The master-signed roster is published WITH the index, exactly as a real
        // release carries `aterm-machines.toml` beside `index.toml`.
        let (roster_bytes, roster_sig) = testkit::published_roster();
        Ok(vec![Candidate {
            label: "v0".into(),
            index_bytes: self.index.clone(),
            sig: self.index_sig.clone(),
            roster_bytes,
            roster_sig,
        }])
    }
    fn pkg_manifest(
        &self,
        _repo: &str,
        program: &str,
        build: u64,
    ) -> Result<(Vec<u8>, Vec<u8>), String> {
        self.pkg
            .get(&(program.to_string(), build))
            .cloned()
            .ok_or_else(|| "no such manifest".into())
    }
    fn download(&self, _repo: &str, asset: &str, dest: &Path) -> Result<(), String> {
        let src = self.archives.get(asset).ok_or("no such asset")?;
        std::fs::copy(src, dest)
            .map(|_| ())
            .map_err(|e| e.to_string())
    }
}

/// Build the whole synthetic signed release (index + pkg + archive) consistent with the
/// real sha256 + tree_root of the archive.
fn fixture(dir: &Path) -> Fake {
    fixture_with_kind(dir, "binary")
}

/// As [`fixture`], but with an explicit artifact `kind` (to exercise the
/// per-member dispatch — e.g. `sysroot-bundle`, which must fail closed).
fn fixture_with_kind(dir: &Path, kind: &str) -> Fake {
    fixture_from(dir, kind, make_archive(dir), None)
}

/// As [`fixture_with_kind`], but with explicit `bin/ay` content + tar mode (the
/// resolve-check rollback tests ship a native-object magic that cannot spawn).
#[cfg(unix)]
fn fixture_with(dir: &Path, kind: &str, ay_content: &[u8], ay_mode: &[u8; 8]) -> Fake {
    fixture_from(dir, kind, make_archive_with(dir, ay_content, ay_mode), None)
}

/// The `tree_root` a producer SIGNS for `archive`: a throwaway stage into `probe`
/// through the extractor's own fold — the digest the real stage computes, which on
/// Windows folds the DECLARED modes (`crate::tree`'s module docs). The plain on-disk
/// walk folds `0` there and would sign a root no client can reproduce; on Unix the
/// two are byte-identical.
fn signed_root_of(archive: &Path, probe: &Path) -> String {
    let _ = std::fs::remove_dir_all(probe);
    crate::extract::extract_tar_zst_tree(
        archive,
        probe,
        10_000_000,
        10_000,
        crate::extract::ExtractOptions::default(),
        crate::extract::FrameChecksum::IfPresent,
    )
    .unwrap()
    .root()
}

/// The signed release over an archive the caller already built.
///
/// `signed_root` overrides the `tree_root` the manifest carries. `None` is the honest
/// case — the real root of the real archive. `Some(..)` is how a test makes the SIGNED
/// value disagree with the bytes on disk, which is the only way to reach the apply-time
/// TOCTOU re-verify through the real flow: the artifact's `sha256` covers the compressed
/// asset, so no substituted archive can satisfy that gate and still fail a later one.
fn fixture_from(dir: &Path, kind: &str, archive: PathBuf, signed_root: Option<&str>) -> Fake {
    let sha = crate::tree::file_sha256(&archive).unwrap();
    // Learn the extracted tree_root by a throwaway stage.
    let probe = dir.join("probe");
    let root = signed_root.map_or_else(
        || signed_root_of(&archive, &probe),
        std::string::ToString::to_string,
    );

    let index_body = format!(
        "schema = 2\nindex_build = 41\nvalid_until = \"2026-07-05T12:00:00Z\"\n{attr}\
         [programs.ay]\nrepo = \"ay\"\n\
         [[channels]]\nname = \"stable\"\nchannel_build = 1\nmin_build = 0\n\
         pin = {{ ay = 18 }}\n",
        attr = attribution()
    );
    let pkg_body = format!(
        "schema = 2\nprogram = \"ay\"\nversion = \"0.1\"\nbuild_number = 18\n\
         exposes = [\"ay\", \"git\"]\n\
         [[artifact]]\ntarget = \"{TRIPLE}\"\nkind = \"{kind}\"\nasset = \"ay-18.tar.zst\"\n\
         sha256 = \"{sha}\"\ntree_root = \"{root}\"\nsize = 100\n\
         [artifact.cost]\ndisk_installed = 1048576\n"
    );
    let mut pkg = HashMap::new();
    pkg.insert(
        ("ay".to_string(), 18u64),
        (
            pkg_body.clone().into_bytes(),
            sign(&RELEASE_SEED, pkg_body.as_bytes()),
        ),
    );
    let mut archives = HashMap::new();
    archives.insert("ay-18.tar.zst".to_string(), archive);
    Fake {
        index: index_body.clone().into_bytes(),
        index_sig: sign(&RELEASE_SEED, index_body.as_bytes()),
        pkg,
        archives,
    }
}

/// A signed release whose ONE artifact row carries the archive's REAL digests
/// (sha256/tree_root/size) and, appended verbatim, `row` — the `kind`/`protocol` pair
/// and the vendor keys under test — so a row that passes admission can stage end to
/// end through a fetcher that serves the archive on `download_url`.
fn fixture_vendor(dir: &Path, row: &str) -> Fake {
    let archive = make_archive(dir);
    let sha = crate::tree::file_sha256(&archive).unwrap();
    let root = signed_root_of(&archive, &dir.join("probe"));
    let size = std::fs::metadata(&archive).unwrap().len();
    let index_body = format!(
        "schema = 2\nindex_build = 41\nvalid_until = \"2026-07-05T12:00:00Z\"\n{attr}\
         [programs.ay]\nrepo = \"ay\"\n\
         [[channels]]\nname = \"stable\"\nchannel_build = 1\nmin_build = 0\n\
         pin = {{ ay = 18 }}\n",
        attr = attribution()
    );
    let pkg_body = format!(
        "schema = 2\nprogram = \"ay\"\nversion = \"0.1\"\nbuild_number = 18\n\
         exposes = [\"ay\", \"git\"]\n\
         [[artifact]]\ntarget = \"{TRIPLE}\"\n\
         asset = \"ay-18.tar.zst\"\nsha256 = \"{sha}\"\ntree_root = \"{root}\"\n\
         size = {size}\n{row}\
         [artifact.cost]\ndisk_installed = 1048576\n"
    );
    let mut pkg = HashMap::new();
    pkg.insert(
        ("ay".to_string(), 18u64),
        (
            pkg_body.clone().into_bytes(),
            sign(&RELEASE_SEED, pkg_body.as_bytes()),
        ),
    );
    let mut archives = HashMap::new();
    archives.insert("ay-18.tar.zst".to_string(), archive);
    Fake {
        index: index_body.clone().into_bytes(),
        index_sig: sign(&RELEASE_SEED, index_body.as_bytes()),
        pkg,
        archives,
    }
}

/// The `(kind, protocol)` head of an https binary row.
const HTTPS_HEAD: &str = "kind = \"binary\"\nprotocol = \"https\"\n";

/// The vendor row every vendor-lane test starts from: a binary over https from a
/// real allow-listed host, the tar-zst lane (the archive fixture IS a tar.zst, and
/// that extractor predates the payload lanes), no raw-binary `entry`.
const VENDOR_ROW: &str = "kind = \"binary\"\nprotocol = \"https\"\nurl = \"https://github.com/openai/codex/releases/download/rust-v0.150.0/codex-package-aarch64-apple-darwin.tar.zst\"\npayload = \"tar-zst\"\nvendor = \"OpenAI\"\n";

fn layout(dir: &Path) -> Layout {
    Layout {
        prefix: dir.join("prefix"),
    }
}

fn tool(name: &str) -> ToolName {
    ToolName::new(name).unwrap()
}

/// `bin/<name>` for a name the test knows is admissible.
fn shim_of(layout: &Layout, name: &str) -> PathBuf {
    layout.shim(&tool(name))
}

/// The concrete executable path a `tool` shim forwards to inside `build_dir`
/// (`bin/<tool>` on Unix, `bin\<tool>.exe` on Windows) — what `ops::which` returns.
fn tool_bin(build_dir: &Path, name: &str) -> PathBuf {
    build_dir.join("bin").join(tool(name).exe_file())
}

/// The sidecar a FAILED stage wrote goes with the build that never came to exist, and
/// the sidecar of a build the failed stage left standing does not.
///
/// Nothing else reclaims an orphan sidecar — `gc::interrupted_debris` walks `store/`
/// for directories, and `store::discard_build` needs a tree. The second half guards
/// the other way: a re-stage over a LIVE build fails with the old tree still in place,
/// and that tree's shims still need its environment.
#[test]
fn a_failed_stage_takes_its_sidecar_and_spares_a_standing_build_s() {
    let dir = scratch("orphan-sidecar");
    let l = layout(&dir);
    let env = crate::shim_env::ShimEnv::admit(&["DISABLE_AUTOUPDATER=1".to_string()]).unwrap();
    // The reference `https` row: its signed sha256 is not this archive's, so the stage
    // refuses at step 1 — before any directory is created, and before the live tree in
    // the second half could be touched.
    let art = crate::vendor::testkit::row();
    let archive = dir.join("asset");
    std::fs::write(&archive, b"not the signed bytes").unwrap();

    // Fresh install: the lane writes the sidecar (creating `store/claude/` to do it),
    // the stage fails, no build directory exists — the sidecar must not survive.
    let build_dir = l.build_dir("claude", 2_026_091_301);
    crate::shim_env::write_sidecar(&build_dir, &env).unwrap();
    assert!(
        verify_and_stage(
            &art,
            &archive,
            &build_dir,
            &crate::install::StageHooks::NONE
        )
        .is_err(),
        "the archive does not match the signed sha256"
    );
    assert!(!build_dir.exists(), "no tree was staged");
    let sidecar = crate::shim_env::sidecar_path(&build_dir).unwrap();
    assert!(
        !sidecar.exists(),
        "an orphan sidecar nothing reclaims: {}",
        sidecar.display()
    );

    // Re-stage over a build that is already there: the stage fails, the standing tree
    // is untouched, and so is the environment its shims export.
    let live = l.build_dir("ay", 18);
    std::fs::create_dir_all(live.join("bin")).unwrap();
    crate::shim_env::write_sidecar(&live, &env).unwrap();
    assert!(verify_and_stage(&art, &archive, &live, &crate::install::StageHooks::NONE).is_err());
    assert!(
        live.exists(),
        "the standing tree survives a failed re-stage"
    );
    assert_eq!(
        crate::shim_env::read_sidecar(&live),
        env,
        "a standing build keeps the environment its shims export"
    );
}

// THE capstone: `ay` installs end-to-end from a synthetic SIGNED release — index verified
// under the root key, pkg verified under the delegated release key, artifact downloaded,
// sha256 + tree_root checked, activated, and shimmed (with the sensitive `git` refused).
#[test]
fn ay_installs_end_to_end_from_a_signed_release() {
    let dir = scratch("e2e");
    let fake = fixture(&dir);
    let layout = layout(&dir);
    let req = InstallRequest {
        channel: "stable",
        program: "ay",
        triple: TRIPLE,
        installed: None,
    };
    let report = install(&fake, &layout, &anchor(), &req, fl(0), 0).unwrap();
    assert_eq!(report.build, 18);
    assert!(!report.already_current);
    assert_eq!(report.shimmed, vec!["ay".to_string()]);
    assert_eq!(
        report.refused_shims,
        vec!["git".to_string()],
        "sensitive shim refused"
    );
    // The binary is staged + shimmed + active.
    assert_eq!(
        crate::ops::which(&layout, "ay").unwrap(),
        tool_bin(&layout.build_dir("ay", 18), "ay")
    );
    assert!(
        crate::ops::which(&layout, "git").is_none(),
        "no sensitive shim"
    );
    assert_eq!(
        std::fs::read_link(layout.program_current("ay")).unwrap(),
        layout.build_dir("ay", 18)
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// The env that turns [`probe_an_install_under_an_open_umask`] on: the scratch directory
/// it installs under.
#[cfg(unix)]
const OPEN_UMASK_ENV: &str = "ATPKG_FLOW_OPEN_UMASK_DIR";
/// That probe's name, as `--exact` wants it.
#[cfg(unix)]
const OPEN_UMASK_PROBE: &str = "flow::tests::probe_an_install_under_an_open_umask";

/// Every entry under `dir` (never through a link) whose mode lets its group or anyone
/// else write it, as `<path relative to base> <mode>`.
#[cfg(unix)]
fn writable_by_others(base: &Path, dir: &Path, out: &mut Vec<String>) {
    for entry in std::fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        let meta = std::fs::symlink_metadata(&path).unwrap();
        if meta.file_type().is_symlink() {
            continue;
        }
        let mode = meta.permissions().mode() & 0o7777;
        if mode & 0o022 != 0 {
            let rel = path.strip_prefix(base).unwrap().display().to_string();
            out.push(format!("{rel} {mode:o}"));
        }
        if meta.is_dir() {
            writable_by_others(base, &path, out);
        }
    }
}

/// The child half of [`an_install_under_an_open_umask_leaves_nothing_others_can_write`]:
/// under umask `000`, the end-to-end install of
/// [`ay_installs_end_to_end_from_a_signed_release`], a shim-environment sidecar and a
/// stage-refusal memo leave no directory or file in the prefix that its group or anyone
/// else could write. Outside that child (the env unset) it does nothing: the umask is
/// the whole process's.
#[cfg(unix)]
#[test]
fn probe_an_install_under_an_open_umask() {
    let Some(dir) = std::env::var_os(OPEN_UMASK_ENV) else {
        return;
    };
    // SAFETY: `umask(2)` swaps this process's file-creation mask and cannot fail; this
    // process runs this one test (`--exact`), so no other create sees the mask.
    unsafe { libc::umask(0) };
    let dir = PathBuf::from(dir);
    let fake = fixture(&dir);
    let layout = layout(&dir);
    let req = InstallRequest {
        channel: "stable",
        program: "ay",
        triple: TRIPLE,
        installed: None,
    };
    let report = install(&fake, &layout, &anchor(), &req, fl(0), 0).unwrap();
    assert_eq!(report.build, 18);
    // The two records a stage writes beside a build when there is cause, each the first
    // writer of its program's directory: the environment a vendor row's shims export,
    // and the memo of a refused stage.
    let env = crate::shim_env::ShimEnv::admit(&["DISABLE_AUTOUPDATER=1".to_string()]).unwrap();
    crate::shim_env::write_sidecar(&layout.build_dir("claude", 2_026_091_301), &env).unwrap();
    let digest = (crate::manifest::DigestAlg::Sha256, "aa");
    crate::store::record_stage_refusal(&layout.build_dir("nn", 3), digest, "bb", "probe", 0)
        .unwrap();
    let mut loose = Vec::new();
    writable_by_others(&layout.prefix, &layout.prefix, &mut loose);
    assert!(loose.is_empty(), "laid writable by others: {loose:#?}");
}

/// AT3's review (2026-10-10): an install's store must not depend on the umask it runs
/// under. A system prefix (`root`-owned, every user runs from it) staged by a `root`
/// whose umask is `002` or `000` laid `store/`, `store/<program>/`, the build root
/// itself, `staging/` and `staging/<program>/` at `0777 & ~umask`, and the records
/// beside a build at `0666 & ~umask`: its group or every user could rename a build's
/// `bin/` and put their own in its place, rename or rewrite the downloaded asset between
/// its digest check and its extraction, or rewrite a record a later verb believes. The
/// umask is process-wide, so the install runs in a child of this test binary that runs
/// only [`probe_an_install_under_an_open_umask`].
#[cfg(unix)]
#[test]
fn an_install_under_an_open_umask_leaves_nothing_others_can_write() {
    let d = scratch("open-umask");
    let out = aterm_uds::spawnfd::command(std::env::current_exe().unwrap())
        .args(["--exact", "--nocapture", OPEN_UMASK_PROBE])
        .env(OPEN_UMASK_ENV, d.as_os_str())
        .stdin(aterm_uds::spawnfd::Stdio::null())
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        out.status.success() && stdout.contains("1 passed"),
        "the install under umask 000: {}\n{stdout}\n{}",
        out.status,
        String::from_utf8_lossy(&out.stderr)
    );
}

/// Phase integration (R5): the SAME end-to-end install, run under a live
/// `--progress-file` pass. The hooks in `install_inner` must carry the row
/// through the honest phase sequence and record the pinned build; the terminal
/// snapshot must read "ended" (pid cleared), never a live-looking lie.
///
/// The ONE test in this binary that touches the process-global sink (every other
/// path runs with it disabled, which is also what keeps this isolated).
#[test]
fn install_phases_land_in_the_progress_file() {
    // The process-global sink has a thread owner; every test that begins a
    // pass serializes on the shared gate (see progress.rs).
    let _gate = crate::progress::PASS_TEST_GATE
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let dir = scratch("phases");
    let fake = fixture(&dir);
    let layout = layout(&dir);
    std::fs::create_dir_all(&layout.prefix).unwrap();
    let progress = layout.prefix.join("progress.json");
    assert!(crate::progress::begin_pass(&progress, "net"));
    if let Some(sink) = crate::progress::active() {
        sink.plan(&[("ay".to_string(), 100)]);
    }
    let req = InstallRequest {
        channel: "stable",
        program: "ay",
        triple: TRIPLE,
        installed: None,
    };
    let report = install(&fake, &layout, &anchor(), &req, fl(0), 0).unwrap();
    assert_eq!(report.build, 18);
    if let Some(sink) = crate::progress::active() {
        sink.finished("ay", crate::progress::Phase::Done, None);
    }
    crate::progress::end_pass();
    let file: crate::progress::ProgressFile =
        aterm_json::from_str(&std::fs::read_to_string(&progress).unwrap()).unwrap();
    assert_eq!(file.pass, "net");
    assert_eq!(file.pid, None, "the pass ended — no live pid claim");
    assert!(file.ended_unix.is_some());
    let ay = &file.programs["ay"];
    assert_eq!(ay.phase, crate::progress::Phase::Done);
    assert_eq!(
        ay.build,
        Some(18),
        "the pinned build was recorded by the flow hook"
    );
    assert_eq!(file.overall.programs_done, 1);
    assert!(file.queue.is_empty());
    let _ = std::fs::remove_dir_all(&dir);
}

/// THE GOVERNING STAGING INVARIANT, THROUGH THE REAL INSTALL FLOW: a re-install that
/// fails to stage must leave the toolchain already on the machine installed, complete,
/// and on PATH.
///
/// The sequence: a live `ay@18`, then a re-install whose signed `tree_root` does not
/// describe the bytes. The unit tests in `install.rs` pin the staging chain itself;
/// this pins that the flow the user actually runs goes through it.
#[test]
fn a_restage_that_fails_leaves_the_live_toolchain_installed_and_on_path() {
    let dir = scratch("restage-keeps-toolchain");
    let layout = layout(&dir);
    let req = InstallRequest {
        channel: "stable",
        program: "ay",
        triple: TRIPLE,
        installed: None,
    };
    install(&fixture(&dir), &layout, &anchor(), &req, fl(0), 0).unwrap();

    let build = layout.build_dir("ay", 18);
    assert!(
        crate::store::build_is_complete(&build),
        "the fixture is only interesting once ay@18 is really installed"
    );
    let ay = tool_bin(&build, "ay");
    let original = std::fs::read(&ay).unwrap();

    // The SAME archive bytes (so the sha256 gate passes and the failure lands late) under
    // a signed tree_root that cannot match — the apply-time TOCTOU re-verify, i.e. the
    // last point at which the old shape had already destroyed the live tree.
    let tampered = fixture_from(&dir, "binary", make_archive(&dir), Some(&"a".repeat(64)));
    let err = install(&tampered, &layout, &anchor(), &req, fl(0), 0).unwrap_err();
    assert!(
        matches!(err, FlowError::Stage(StageError::TreeRootMismatch { .. })),
        "the fixture must fail at the re-verify, else this proves nothing: got {err:?}"
    );

    assert!(
        crate::store::build_is_complete(&build),
        "the installed build must still be marked complete"
    );
    assert_eq!(
        std::fs::read(&ay).unwrap(),
        original,
        "the live binary must be the ORIGINAL one, byte for byte"
    );
    assert_eq!(
        crate::ops::which(&layout, "ay").unwrap(),
        ay,
        "and it must still be what the shim forwards to"
    );
    assert_eq!(
        crate::ops::list_installed(&layout),
        vec![("ay".to_string(), 18u64)],
        "the store must still report exactly the build that is really there"
    );
    assert_eq!(
        std::fs::read_link(layout.program_current("ay")).unwrap(),
        build
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// Every entry in `prefix/staging/<program>/`, sorted.
fn staged_assets(layout: &Layout, program: &str) -> Vec<String> {
    let Ok(entries) = std::fs::read_dir(layout.staging_dir(program)) else {
        return Vec::new();
    };
    let mut out: Vec<String> = entries
        .flatten()
        .filter_map(|e| e.file_name().to_str().map(str::to_string))
        .collect();
    out.sort();
    out
}

// THE COMPRESSED ASSET OF A FAILED STAGE IS RETAINED — ONE COPY, REUSED — WHEN THE
// BYTES WERE RIGHT, and reclaimed with its `.part` when they were not: a stage that
// failed for a reason that was not the bytes' (a permission refusal, a full disk) must
// not re-download the same archive (743 MB for `trust`) on every pass. The strandage
// bound still holds: `sweep_foreign_partials` keeps one archive per program, this pass's.
#[test]
fn a_failed_stage_retains_one_verified_archive_and_a_digest_failure_none() {
    let dir = scratch("staging-retain");
    let layout = layout(&dir);
    let req = InstallRequest {
        channel: "stable",
        program: "ay",
        triple: TRIPLE,
        installed: None,
    };
    // The same archive bytes under a signed tree_root that cannot match: the sha256 gate
    // passes, so the download really lands in `staging/` and the failure is late.
    let tampered = fixture_from(&dir, "binary", make_archive(&dir), Some(&"a".repeat(64)));
    let err = install(&tampered, &layout, &anchor(), &req, fl(0), 0).unwrap_err();
    assert!(
        matches!(err, FlowError::Stage(StageError::TreeRootMismatch { .. })),
        "PRECONDITION: the stage must fail AFTER the download: {err:?}"
    );
    assert_eq!(
        staged_assets(&layout, "ay"),
        vec!["ay-18.tar.zst".to_string()],
        "the verified archive is retained, once"
    );
    // A DIGEST failure keeps nothing: the bytes are wrong, and a `.part` of them would
    // seed the next attempt — so the sibling `.part` seeded here goes with the asset. A
    // fresh store, because a retained archive whose digest is the row's is reused
    // before any download.
    let ddir = scratch("staging-digest");
    let dlayout = Layout {
        prefix: ddir.join("prefix"),
    };
    std::fs::create_dir_all(dlayout.staging_dir("ay")).unwrap();
    std::fs::write(
        dlayout.staging_dir("ay").join("ay-18.tar.zst.part"),
        b"poisoned prefix",
    )
    .unwrap();
    let archive = make_archive(&ddir);
    let good = fixture_from(&ddir, "binary", archive.clone(), None);
    std::fs::write(&archive, b"not the bytes the row signed").unwrap();
    let err = install(&good, &dlayout, &anchor(), &req, fl(0), 0).unwrap_err();
    assert!(
        matches!(err, FlowError::Stage(StageError::DigestMismatch { .. })),
        "PRECONDITION: the digest gate must fail: {err:?}"
    );
    assert!(
        staged_assets(&dlayout, "ay").is_empty(),
        "a digest failure reclaims the archive: {:?}",
        staged_assets(&dlayout, "ay")
    );
    let _ = std::fs::remove_dir_all(&ddir);

    // Non-vacuity, and the reuse itself: the good row has the SAME bytes under the
    // right root, so the retained archive is what it stages — no fetch — and a
    // successful install leaves staging empty as it always did.
    install(&fixture(&dir), &layout, &anchor(), &req, fl(0), 0).unwrap();
    assert!(staged_assets(&layout, "ay").is_empty());
    assert!(crate::store::build_is_complete(&layout.build_dir("ay", 18)));
    let _ = std::fs::remove_dir_all(&dir);
}

/// A SIGNER refusal is handled as a digest refusal is: the archive and its `.part` go,
/// and the memo holds the next pass off the wire — from the first refusal and
/// whatever the clock says, because the refused bytes matched their digests — until
/// the digests move. A machine fault (codesign that could not answer) records
/// nothing and keeps the archive.
#[test]
fn a_signer_refusal_reclaims_its_archive_and_binds_until_the_digests_move() {
    let dir = scratch("signer-refusal");
    let layout = layout(&dir);
    let build = 1_000_002_000_001_000_280;
    let build_dir = layout.build_dir("claude", build);
    let art = crate::vendor::testkit::row();
    let dl = dir.join("staging").join(&art.asset);
    let part = dir.join("staging").join(format!("{}.part", art.asset));
    std::fs::create_dir_all(dir.join("staging")).unwrap();

    std::fs::write(&dl, b"verified bytes").unwrap();
    let fault = StageError::Io(std::io::Error::other("bin/claude: codesign timed out"));
    record_digest_refusal(&build_dir, &art, &fault);
    reclaim_after_failed_stage(&dl, &fault);
    assert!(dl.is_file(), "a machine fault keeps the verified archive");
    assert_eq!(
        digest_refusal_note(&layout, "claude", build, &art, false),
        None
    );

    std::fs::write(&part, b"partial").unwrap();
    let refused = StageError::SignerRefused(
        "bin/claude is not signed by Developer ID team Q6L2SF6YDW".into(),
    );
    record_digest_refusal(&build_dir, &art, &refused);
    reclaim_after_failed_stage(&dl, &refused);
    assert!(
        !dl.exists() && !part.exists(),
        "the refused bytes are reclaimed"
    );
    let note = digest_refusal_note(&layout, "claude", build, &art, false)
        .expect("the first signer refusal already binds");
    // The canonical held row ([`crate::state::held_failed`]) every surface repeats:
    // the verdict first, then the one retry.
    assert!(
        note.starts_with(crate::state::HELD_FAILED_PREFIX)
            && note.ends_with("now: aterm pkg install claude"),
        "{note}"
    );
    let mut moved = art.clone();
    moved.sha256 = "0".repeat(64);
    assert_eq!(
        digest_refusal_note(&layout, "claude", build, &moved, false),
        None,
        "new digests are a new question"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

// THE SAME BLIND SPOT ON THE SINGLE-PROGRAM PATH. `abort_activated_install` unwinds an
// install that failed AFTER activation, and its discard is sound for a build this call
// created. It is not sound for one that was already live: the failures that land there
// are `install_tools` (an EACCES/EROFS/full disk while writing `bin/` shims — a fact
// about the environment, not the tree) and the resolve check, and deleting a verified
// multi-gigabyte tree in response leaves the user with no toolchain at all.
#[test]
fn a_post_activation_abort_never_discards_a_build_that_was_already_live() {
    let dir = scratch("single-abort-live");
    let ndir = scratch("single-abort-fresh");
    // Both layouts before the binding shadows the constructor.
    let nlayout = layout(&ndir);
    let layout = layout(&dir);
    let req = InstallRequest {
        channel: "stable",
        program: "ay",
        triple: TRIPLE,
        installed: None,
    };
    install(&fixture(&dir), &layout, &anchor(), &req, fl(0), 0).unwrap();
    let build = layout.build_dir("ay", 18);
    assert!(
        crate::store::build_is_complete(&build),
        "PRECONDITION: ay@18 is really installed"
    );
    assert_eq!(
        std::fs::read_link(layout.program_current("ay")).unwrap(),
        build,
        "PRECONDITION: and really live"
    );

    // Break `install_tools` the way an unwritable prefix does: a regular FILE where the
    // `bin/` directory must be, so `ensure_private_dir` fails AFTER activation. The
    // `installed: None` request is the shim view being silent about a live program.
    std::fs::remove_dir_all(layout.bin_dir()).unwrap();
    std::fs::write(layout.bin_dir(), b"not a dir").unwrap();

    let err = install(&fixture(&dir), &layout, &anchor(), &req, fl(0), 0).unwrap_err();
    assert!(
        matches!(err, FlowError::Activate(_)),
        "PRECONDITION: the failure must land after activation: {err:?}"
    );

    assert!(
        build.is_dir(),
        "a failed shim write destroyed the live toolchain tree"
    );
    assert_eq!(
        std::fs::read(tool_bin(&build, "ay")).unwrap(),
        b"#!/bin/true\nay",
        "and the tree left behind is the verified one"
    );
    assert!(
        crate::store::build_is_complete(&build),
        "it must still read as installed, so the retry re-stages it in place"
    );

    // AND THE KEEP MUST OUTLIVE THE RUN. `was_live` is read off the two `current`
    // links, and the unwind removes both; the witness is re-pointed at the kept build
    // so a SECOND failure of the same unwritable `bin/` still knows it was live and
    // does not discard the tree the first abort kept.
    assert_eq!(
        std::fs::read_link(layout.program_current("ay")).unwrap(),
        build,
        "the kept tree lost its witness, so the retry cannot know it was ever live"
    );
    let again = install(&fixture(&dir), &layout, &anchor(), &req, fl(0), 0).unwrap_err();
    assert!(
        matches!(again, FlowError::Activate(_)),
        "PRECONDITION: the retry fails the same way: {again:?}"
    );
    assert!(
        crate::store::build_is_complete(&build),
        "a SECOND abort with `bin/` still unwritable discarded the live toolchain tree"
    );
    assert_eq!(
        std::fs::read(tool_bin(&build, "ay")).unwrap(),
        b"#!/bin/true\nay",
        "and the tree that survived the retry is still the verified one"
    );
    assert_eq!(
        std::fs::read_link(layout.program_current("ay")).unwrap(),
        build,
        "and the keep stays durable for the attempt after that"
    );

    // Non-vacuity: a build this call really did create IS still discarded. Restore the
    // prefix, then fail a FRESH install of a program that was never live.
    std::fs::remove_file(layout.bin_dir()).unwrap();
    let ndir = scratch("single-abort-fresh");
    std::fs::create_dir_all(nlayout.prefix.join("store")).unwrap();
    std::fs::write(nlayout.bin_dir(), b"not a dir").unwrap();
    let nerr = install(&fixture(&ndir), &nlayout, &anchor(), &req, fl(0), 0).unwrap_err();
    assert!(matches!(nerr, FlowError::Activate(_)), "got {nerr:?}");
    assert!(
        !nlayout.build_dir("ay", 18).exists(),
        "a build the failed install itself created is still discarded"
    );
    let _ = std::fs::remove_dir_all(&ndir);
    let _ = std::fs::remove_dir_all(&dir);
}

// THE RETRY IS BOUNDED TO WHAT ACTUALLY FAILED. A coherence group aborts all-or-nothing
// and discards every staged sibling's extracted tree — that invariant is not negotiable
// (a complete-but-inactive build would be mis-read as the active build and split the
// tuple). Their verified DOWNLOADS are kept: refetching a ~3.4 GB toolchain on every
// tick could leave a tuple on a slow link never converging.
//
// The saving is made OBSERVABLE the only way a local-copy fetcher can show it: pass 2
// runs with the sibling's asset REMOVED from the registry, so the sole way it can stage
// is from the archive pass 1 carried.
#[test]
fn an_aborted_tuple_carries_its_verified_siblings_so_the_retry_refetches_only_the_failure() {
    let dir = scratch("group-carry-archive");
    let mut fake = group_fixture(&dir);
    let layout = layout(&dir);
    let installed = std::collections::BTreeMap::from([("ay".to_string(), 17u64)]);

    // PASS 1: trust's asset is unfetchable (the flaky-link case). `ay` stages first and
    // COMPLETELY — downloaded, sha256-verified, tree_root-verified, extracted — and
    // then trust's download fails and takes the whole tuple down with it.
    let good_trust = fake.archives.remove("trust-4821.tar.zst").unwrap();
    let report = apply_channel(
        &fake,
        &layout,
        &anchor(),
        "stable",
        TRIPLE,
        &installed,
        &[],
        fl(0),
        0,
    )
    .unwrap();
    assert!(
        matches!(report.groups[0].1, TxnOutcome::Aborted { .. }),
        "PRECONDITION: the group aborted: {:?}",
        report.groups[0].1
    );
    assert!(
        !layout.build_dir("ay", 18).exists(),
        "PRECONDITION: the abort still discards the sibling's extracted TREE"
    );
    assert_eq!(
        staged_assets(&layout, "ay"),
        vec!["ay-18.tar.zst".to_string()],
        "the sibling's verified archive is carried to the next pass"
    );

    // The pass-end gc must not undo that — the CLI's wiring, verbatim.
    let _ = crate::gc::run_keeping_pinned_partials(&layout, None, &|p| {
        report.resolved_assets.get(p).cloned()
    });
    assert_eq!(
        staged_assets(&layout, "ay"),
        vec!["ay-18.tar.zst".to_string()],
        "the pass-closing sweep spares the carried archive, as it does the `.part`"
    );

    // PASS 2: trust is reachable again, but `ay`'s asset is GONE from the registry, so
    // re-downloading it cannot succeed. The tuple must converge anyway.
    fake.archives
        .insert("trust-4821.tar.zst".to_string(), good_trust);
    fake.archives.remove("ay-18.tar.zst");
    let retry = apply_channel(
        &fake,
        &layout,
        &anchor(),
        "stable",
        TRIPLE,
        &installed,
        &[],
        fl(0),
        0,
    )
    .unwrap();
    assert!(
        matches!(retry.groups[0].1, TxnOutcome::Applied(_)),
        "the retry must re-stage the sibling from its carried archive instead of \
         re-downloading it: {:?}",
        retry.groups[0].1
    );
    assert!(crate::store::build_is_complete(&layout.build_dir("ay", 18)));
    assert!(crate::store::build_is_complete(
        &layout.build_dir("trust", 4821)
    ));

    // A SETTLED tuple keeps nothing: the carry exists only BETWEEN passes, so the
    // strandage bound `a_member_stage_failure_aborts_the_group_and_a_retry_heals_it`
    // pins is not widened by any of this.
    assert!(
        staged_assets(&layout, "ay").is_empty(),
        "a settled tuple reclaims the carried archive: {:?}",
        staged_assets(&layout, "ay")
    );
    assert!(
        staged_assets(&layout, "trust").is_empty(),
        "{:?}",
        staged_assets(&layout, "trust")
    );
    let _ = std::fs::remove_dir_all(&dir);
}

// THE RESUME-ACROSS-PASSES WIRING (R4), update lane: a member whose DOWNLOAD fails
// mid-pass reaches `ChannelApplyReport::resolved_assets` anyway — the name is recorded
// when the verified manifest selects the artifact, BEFORE any byte moves — so the CLI's
// pass-end `gc::run_keeping_pinned_partials`, fed exactly that map, spares the `.part`
// the fetcher left and the next pass resumes instead of refetching from byte 0.
#[test]
fn a_failed_download_survives_its_own_pass_end_gc_and_a_plain_gc_still_sweeps_it() {
    let dir = scratch("resume-survives-pass-end");
    let mut fake = group_fixture(&dir);
    let layout = layout(&dir);
    // trust's asset is unfetchable: the download fails AFTER its manifest verified —
    // the window that leaves a `.part` behind in production. `Fake` copies whole
    // files or fails, so the test seeds the resume bytes the real fetcher would leave.
    fake.archives.remove("trust-4821.tar.zst");
    let staging = layout.staging_dir("trust");
    std::fs::create_dir_all(&staging).unwrap();
    let part = staging.join("trust-4821.tar.zst.part");
    std::fs::write(&part, b"resume-bytes").unwrap();

    let report = apply_channel(
        &fake,
        &layout,
        &anchor(),
        "stable",
        TRIPLE,
        &std::collections::BTreeMap::from([("ay".to_string(), 17u64)]),
        &[],
        fl(0),
        0,
    )
    .unwrap();
    assert!(
        matches!(report.groups[0].1, TxnOutcome::Aborted { .. }),
        "PRECONDITION: the failed download aborted the group: {:?}",
        report.groups[0].1
    );
    assert!(
        part.exists(),
        "PRECONDITION: the resume state survived the abort"
    );
    // The FAILED member is in the map — resolution happened before its download —
    // and so is the sibling that staged fine (harmless: it has no `.part` left).
    assert_eq!(
        report.resolved_assets.get("trust").map(String::as_str),
        Some("trust-4821.tar.zst"),
        "the failed member's pinned asset name must reach the report"
    );
    assert_eq!(
        report.resolved_assets.get("ay").map(String::as_str),
        Some("ay-18.tar.zst")
    );

    // The CLI's pass-end wiring, verbatim: the sparing closure IS the report's map.
    let _ = crate::gc::run_keeping_pinned_partials(&layout, None, &|p| {
        report.resolved_assets.get(p).cloned()
    });
    assert!(
        part.exists(),
        "the pass-end sweep spares the failed member's resume state"
    );

    // `uninstall --all` and the standalone `atpkg gc` keep the PLAIN form on purpose
    // (the user asked for the disk back): everything spared above stays reclaimable.
    let _ = crate::gc::run(&layout, None);
    assert!(
        !part.exists(),
        "a plain gc still reclaims the spared partial"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

// The same wiring on the singleton lane (`bootstrap_singleton` / `do_install`): a
// failed install returns NO report, so the resolved asset rides the collector
// out-param — filled before the download, surviving the `Err`.
#[test]
fn a_failed_singleton_download_still_fills_the_resolved_asset_collector() {
    let dir = scratch("resume-collector-singleton");
    let mut fake = fixture(&dir);
    fake.archives.remove("ay-18.tar.zst");
    let layout = layout(&dir);
    let staging = layout.staging_dir("ay");
    std::fs::create_dir_all(&staging).unwrap();
    let part = staging.join("ay-18.tar.zst.part");
    std::fs::write(&part, b"resume-bytes").unwrap();
    let req = InstallRequest {
        channel: "stable",
        program: "ay",
        triple: TRIPLE,
        installed: None,
    };

    let mut resolved = std::collections::BTreeMap::new();
    let err = install_collecting_assets(&fake, &layout, &anchor(), &req, fl(0), 0, &mut resolved)
        .unwrap_err();
    assert!(
        matches!(err, FlowError::Download(_)),
        "PRECONDITION: the download failed: {err:?}"
    );
    assert_eq!(
        resolved.get("ay").map(String::as_str),
        Some("ay-18.tar.zst"),
        "the collector must survive the Err path — that is its whole reason to exist"
    );

    let _ = crate::gc::run_keeping_pinned_partials(&layout, None, &|p| resolved.get(p).cloned());
    assert!(part.exists(), "the pass-end sweep spares the resume state");
    let _ = crate::gc::run(&layout, None);
    assert!(!part.exists(), "a plain gc still reclaims it");
    let _ = std::fs::remove_dir_all(&dir);
}

/// The FETCHER CONTRACT for the vendor lane: `download_url` is refused by default —
/// a test fetcher and the `dir:` registry fetcher never reach the network by accident.
#[test]
fn a_fetcher_refuses_vendor_urls_unless_it_opts_in() {
    struct Inert;
    impl Fetcher for Inert {
        fn index_candidates(&self) -> Result<Vec<Candidate>, String> {
            Err("inert".into())
        }
        fn pkg_manifest(&self, _: &str, _: &str, _: u64) -> Result<(Vec<u8>, Vec<u8>), String> {
            Err("inert".into())
        }
        fn download(&self, _: &str, _: &str, _: &Path) -> Result<(), String> {
            Err("inert".into())
        }
    }
    let dest = std::env::temp_dir().join("atpkg-flow-inert-never-written");
    assert!(
        Inert
            .download_url("https://github.com/x/y", &dest, 1)
            .is_err()
    );
    let dir_fetcher = crate::net::DirFetcher::new(std::env::temp_dir());
    assert!(
        dir_fetcher
            .download_url("https://github.com/x/y", &dest, 1)
            .is_err(),
        "the dir: registry keeps the fail-closed default"
    );
    assert!(!dest.exists(), "a refusal writes nothing");

    // The vendor-direct lanes answer "unreachable" by default: a fetcher with no
    // network leg keeps what is installed, quietly, and writes nothing.
    const HEAD: &str = "https://downloads.claude.ai/claude-code-releases/latest";
    let unreachable = |e: VendorFetchError| {
        assert!(matches!(e, VendorFetchError::Unreachable(_)), "{e:?}");
    };
    for fetcher in [&Inert as &dyn Fetcher, &dir_fetcher] {
        for sent in [None, Some("\"e\"")] {
            unreachable(fetcher.vendor_get("claude", HEAD, 64, sent).unwrap_err());
        }
        unreachable(fetcher.vendor_content_length("claude", HEAD).unwrap_err());
        unreachable(
            fetcher
                .vendor_download("claude", HEAD, &dest, 64)
                .unwrap_err(),
        );
    }
    assert!(!dest.exists(), "a refusal writes nothing");
}

/// A `github-release` row's `asset` is joined onto `staging/<program>/`, and the flow
/// unlinks that path and sweeps every `*.part` beside it BEFORE any download. An asset
/// that is a PATH — absolute (`Path::join` replaces the base) or climbing with `..` —
/// must be refused before either runs, so a signed row can never delete a file outside
/// staging.
#[test]
fn install_never_unlinks_outside_staging_for_a_release_row() {
    let req = InstallRequest {
        channel: "stable",
        program: "ay",
        triple: TRIPLE,
        installed: None,
    };
    for label in ["absolute", "climbing"] {
        let dir = scratch(&format!("release-asset-{label}"));
        let victim = dir.join("victim");
        std::fs::create_dir_all(&victim).unwrap();
        std::fs::write(victim.join("keep.txt"), b"keep").unwrap();
        std::fs::write(victim.join("other.part"), b"part").unwrap();
        // `layout(dir)` stages under `dir/prefix/staging/ay/`.
        let asset = if label == "absolute" {
            victim.join("keep.txt").to_string_lossy().into_owned()
        } else {
            String::from("../../../victim/keep.txt")
        };
        let base = fixture(&dir);
        let sha = "a".repeat(64);
        let root = "b".repeat(64);
        let pkg_body = format!(
            "schema = 2\nprogram = \"ay\"\nversion = \"0.1\"\nbuild_number = 18\n\
             exposes = [\"ay\"]\n\
             [[artifact]]\ntarget = \"{TRIPLE}\"\nkind = \"binary\"\n\
             protocol = \"github-release\"\nasset = \"{asset}\"\n\
             sha256 = \"{sha}\"\ntree_root = \"{root}\"\nsize = 1\n\
             [artifact.cost]\ndisk_installed = 1\n"
        );
        let mut pkg = HashMap::new();
        pkg.insert(
            ("ay".to_string(), 18u64),
            (
                pkg_body.clone().into_bytes(),
                sign(&RELEASE_SEED, pkg_body.as_bytes()),
            ),
        );
        let fake = Fake {
            index: base.index,
            index_sig: base.index_sig,
            pkg,
            archives: HashMap::new(),
        };
        let err = install(&fake, &layout(&dir), &anchor(), &req, fl(0), 0).unwrap_err();
        assert!(
            victim.join("keep.txt").exists(),
            "{label}: the file named by the asset was unlinked ({err:?})"
        );
        assert!(
            victim.join("other.part").exists(),
            "{label}: a partial beside it was swept ({err:?})"
        );
        assert!(
            matches!(err, FlowError::VendorRefused(_)),
            "{label}: refused as a row, not a download: {err:?}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}

/// THE VENDOR LANE, end to end through the real flow:
/// 1. a `binary` row over the `https` protocol is DISPATCHED (never
///    `UnsupportedKind`) and admitted BEFORE any byte moves — a row with no url is
///    `VendorRefused` and the staging dir stays empty;
/// 2. an admitted row downloads through `download_url` ONLY: the slug lane
///    (`download`/`download_for`) is never consulted, and a fetcher without the vendor
///    lane fails as a `Download` naming it, leaving nothing installed;
/// 3. a fetcher that serves the signed URL installs, shims and activates exactly like a
///    plain binary, under the signed sha256 + tree_root gates, with the signed `size`
///    as the exact cap and NO account slug or asset name on the request.
#[test]
fn an_https_row_is_admitted_then_downloaded_only_through_the_vendor_lane() {
    let req = InstallRequest {
        channel: "stable",
        program: "ay",
        triple: TRIPLE,
        installed: None,
    };
    // 1. Refused closed at admission.
    let rdir = scratch("vendor-refused");
    let refused = fixture_vendor(&rdir, HTTPS_HEAD);
    let rlay = layout(&rdir);
    let err = install(&refused, &rlay, &anchor(), &req, fl(0), 0).unwrap_err();
    assert!(
        matches!(err, FlowError::VendorRefused(_)),
        "dispatched and refused at admission, not UnsupportedKind: {err:?}"
    );
    assert!(
        !rlay.staging_dir("ay").exists(),
        "admission runs before the staging dir is even created"
    );
    // A host outside the allow-list is refused the same way, even over https.
    let hdir = scratch("vendor-host");
    let hostile = fixture_vendor(
        &hdir,
        "kind = \"binary\"\nprotocol = \"https\"\n\
         url = \"https://evil.example/codex.tar.zst\"\npayload = \"tar-zst\"\n",
    );
    let err = install(&hostile, &layout(&hdir), &anchor(), &req, fl(0), 0).unwrap_err();
    assert!(matches!(err, FlowError::VendorRefused(_)), "{err:?}");

    // 2. Admitted, but this fetcher has no vendor lane: the slug lane is NOT a fallback.
    let ndir = scratch("vendor-no-lane");
    let no_lane = fixture_vendor(&ndir, VENDOR_ROW);
    let nlay = layout(&ndir);
    let err = install(&no_lane, &nlay, &anchor(), &req, fl(0), 0).unwrap_err();
    assert!(
        matches!(&err, FlowError::Download(_)),
        "expected the vendor lane's refusal, got {err:?}"
    );
    assert!(
        crate::ops::which(&nlay, "ay").is_none(),
        "nothing installed"
    );

    // 3. A fetcher WITH the vendor lane installs end to end — and only that lane is used.
    struct VendorFake {
        inner: Fake,
        served: PathBuf,
        seen: RefCell<Vec<(String, u64)>>,
    }
    impl Fetcher for VendorFake {
        fn index_candidates(&self) -> Result<Vec<Candidate>, String> {
            self.inner.index_candidates()
        }
        fn pkg_manifest(&self, r: &str, p: &str, b: u64) -> Result<(Vec<u8>, Vec<u8>), String> {
            self.inner.pkg_manifest(r, p, b)
        }
        fn download(&self, _: &str, asset: &str, _: &Path) -> Result<(), String> {
            panic!("a vendor row must never reach the slug lane (asked for {asset})");
        }
        fn download_url(&self, url: &str, dest: &Path, cap: u64) -> Result<(), String> {
            self.seen.borrow_mut().push((url.to_string(), cap));
            std::fs::copy(&self.served, dest)
                .map(|_| ())
                .map_err(|e| e.to_string())
        }
    }
    let vdir = scratch("vendor-lane");
    let inner = fixture_vendor(&vdir, VENDOR_ROW);
    let served = inner.archives["ay-18.tar.zst"].clone();
    let size = std::fs::metadata(&served).unwrap().len();
    let vendor = VendorFake {
        inner,
        served,
        seen: RefCell::new(Vec::new()),
    };
    let vlay = layout(&vdir);
    let report = install(&vendor, &vlay, &anchor(), &req, fl(0), 0).unwrap();
    assert_eq!(report.build, 18);
    assert_eq!(report.shimmed, vec!["ay".to_string()]);
    assert_eq!(
        crate::ops::which(&vlay, "ay").unwrap(),
        tool_bin(&vlay.build_dir("ay", 18), "ay"),
        "activated and shimmed like a plain binary"
    );
    let seen = vendor.seen.borrow();
    assert_eq!(seen.len(), 1, "exactly one vendor download");
    assert!(
        seen[0]
            .0
            .starts_with("https://github.com/openai/codex/releases/download/"),
        "the SIGNED url, verbatim: {}",
        seen[0].0
    );
    assert_eq!(seen[0].1, size, "the cap is the signed size, exactly");
    for d in [rdir, hdir, ndir, vdir] {
        let _ = std::fs::remove_dir_all(&d);
    }
}

/// THE RELEASE LANE IS CAPPED BY THE ROW'S SIGNED SIZE, not by the fetcher's own
/// ceiling — the property the vendor lane above has always had, now proven for
/// `github-release` too.
///
/// `disk_gate(size + disk_installed)` is computed from the signed number and is
/// checked ONCE, before the transfer; nothing bounds the bytes as they land but the
/// cap the fetcher is given, and the sha256 gate sees the file only after it is
/// whole. A release lane capped at its own 8 GiB constant therefore let a
/// mis-uploaded or substituted asset (GitHub takes up to 2 GiB per asset) write
/// through the free-space floor the preflight had just defended — for a row whose
/// signed size says 100 bytes — and refused it only afterwards. So the number the
/// flow hands `download_for` is the row's own `size`, exactly as `download_url` has
/// always taken it.
#[test]
fn a_github_release_download_is_capped_at_the_rows_signed_size() {
    struct CapFake {
        inner: Fake,
        seen: RefCell<Vec<(String, u64)>>,
    }
    impl Fetcher for CapFake {
        fn index_candidates(&self) -> Result<Vec<Candidate>, String> {
            self.inner.index_candidates()
        }
        fn pkg_manifest(&self, r: &str, p: &str, b: u64) -> Result<(Vec<u8>, Vec<u8>), String> {
            self.inner.pkg_manifest(r, p, b)
        }
        fn download(&self, _: &str, asset: &str, _: &Path) -> Result<(), String> {
            panic!(
                "the artifact lane is `download_for`, which carries the cap (asked for {asset})"
            );
        }
        fn download_for(
            &self,
            _program: &str,
            repo: &str,
            asset: &str,
            dest: &Path,
            cap: u64,
        ) -> Result<(), String> {
            self.seen.borrow_mut().push((asset.to_string(), cap));
            self.inner.download(repo, asset, dest)
        }
    }
    let dir = scratch("release-lane-cap");
    // The fixture's signed row states `size = 100` — a number nothing else in the
    // flow reads back, so an assertion on it can only be satisfied by the SIGNED
    // value actually reaching the lane.
    let f = CapFake {
        inner: fixture(&dir),
        seen: RefCell::new(Vec::new()),
    };
    let lay = layout(&dir);
    let req = InstallRequest {
        channel: "stable",
        program: "ay",
        triple: TRIPLE,
        installed: None,
    };
    let report = install(&f, &lay, &anchor(), &req, fl(0), 0).unwrap();
    assert_eq!(report.build, 18);
    let seen = f.seen.borrow();
    assert_eq!(seen.len(), 1, "exactly one artifact download");
    assert_eq!(seen[0].0, "ay-18.tar.zst");
    assert_eq!(
        seen[0].1, 100,
        "the cap is the row's SIGNED size, never the fetcher's 8 GiB ceiling"
    );
    drop(seen);
    let _ = std::fs::remove_dir_all(&dir);
}

/// DESIGN S7, end to end through the real flow: a manifest declaring
/// `shim_env = ["DISABLE_AUTOUPDATER=1"]` installs like any vendor row, and the shim
/// it lays EXPORTS that variable before it execs — read back off the shim, recorded
/// beside the build in its `<build>.shim-env` sidecar (so the verbs that hold no
/// manifest re-lay it the same way), and still a shim `which` resolves. A manifest
/// whose `shim_env` breaks the rule is refused at PARSE as `ShimEnv` naming the entry,
/// before any byte moves.
#[test]
fn a_shim_env_manifest_lays_an_exporting_shim_and_a_bad_one_is_refused() {
    struct VendorFake {
        inner: Fake,
        served: PathBuf,
    }
    impl Fetcher for VendorFake {
        fn index_candidates(&self) -> Result<Vec<Candidate>, String> {
            self.inner.index_candidates()
        }
        fn pkg_manifest(&self, r: &str, p: &str, b: u64) -> Result<(Vec<u8>, Vec<u8>), String> {
            self.inner.pkg_manifest(r, p, b)
        }
        fn download(&self, _: &str, asset: &str, _: &Path) -> Result<(), String> {
            panic!("a vendor row must never reach the slug lane (asked for {asset})");
        }
        fn download_url(&self, _: &str, dest: &Path, _: u64) -> Result<(), String> {
            std::fs::copy(&self.served, dest)
                .map(|_| ())
                .map_err(|e| e.to_string())
        }
    }
    let req = InstallRequest {
        channel: "stable",
        program: "ay",
        triple: TRIPLE,
        installed: None,
    };
    // `fixture_vendor` appends `row` INSIDE the artifact table; a top-level key has to
    // come before it, so re-head the signed body: the same bytes with `shim_env` after
    // `exposes`, re-signed.
    let with_env = |dir: &Path, list: &str| {
        let mut fake = fixture_vendor(dir, VENDOR_ROW);
        let (body, _) = fake.pkg.remove(&("ay".to_string(), 18u64)).unwrap();
        let body = String::from_utf8(body).unwrap().replacen(
            "exposes = [\"ay\", \"git\"]\n",
            &format!("exposes = [\"ay\", \"git\"]\nshim_env = {list}\n"),
            1,
        );
        fake.pkg.insert(
            ("ay".to_string(), 18u64),
            (
                body.clone().into_bytes(),
                sign(&RELEASE_SEED, body.as_bytes()),
            ),
        );
        fake
    };
    let dir = scratch("shim-env-lane");
    let inner = with_env(&dir, "[\"DISABLE_AUTOUPDATER=1\"]");
    let served = inner.archives["ay-18.tar.zst"].clone();
    let vendor = VendorFake { inner, served };
    let l = layout(&dir);
    let report = install(&vendor, &l, &anchor(), &req, fl(0), 0).unwrap();
    assert_eq!(report.build, 18);
    let shim = l.shim(&tool("ay"));
    assert_eq!(
        crate::ops::which(&l, "ay").unwrap(),
        tool_bin(&l.build_dir("ay", 18), "ay"),
        "still a shim that resolves"
    );
    let env = crate::platform::shim_env_of(&shim);
    assert_eq!(
        env.spelled(),
        "DISABLE_AUTOUPDATER=1",
        "the shim exports it"
    );
    assert_eq!(
        crate::shim_env::read_sidecar(&l.build_dir("ay", 18)),
        env,
        "recorded beside the build for the verbs that hold no manifest"
    );
    // The rollback verb's path re-lays the PRIOR build's shims from ITS sidecar: a
    // newer build without the key flips live, then rolls back — the env returns.
    let b19 = bare_build(&l, "ay", 19, &["ay"]);
    crate::shim_env::write_sidecar(&b19, &crate::shim_env::ShimEnv::NONE).unwrap();
    activate_build(&l, &b19).unwrap();
    install_tools(&l, &b19, &[tool("ay")], Aliases::Off).unwrap();
    assert_eq!(
        crate::platform::shim_env_of(&shim),
        crate::shim_env::ShimEnv::NONE,
        "19 declares no env"
    );
    let staged = Staged {
        build: 19,
        build_dir: b19,
        exposes: vec![tool("ay")],
        prior_build: Some(18),
        was_live: false,
        reloc: None,
        tree_root: String::new(),
        aliases: Aliases::Off,
    };
    rollback_member(&l, "ay", &staged);
    assert!(
        crate::platform::resolve_shim(&shim).is_some_and(|t| t.starts_with(l.build_dir("ay", 18)))
    );
    assert_eq!(
        crate::platform::shim_env_of(&shim).spelled(),
        "DISABLE_AUTOUPDATER=1",
        "the prior build's env, from its sidecar"
    );
    // The sidecar goes with the tree.
    crate::store::discard_build(&l.build_dir("ay", 18));
    assert_eq!(
        crate::shim_env::read_sidecar(&l.build_dir("ay", 18)),
        crate::shim_env::ShimEnv::NONE
    );
    let _ = std::fs::remove_dir_all(&dir);

    // A `shim_env` the rule refuses: refused at parse, by name, nothing staged.
    let bdir = scratch("shim-env-refused");
    let bad = with_env(&bdir, "[\"PATH=/evil\"]");
    let served = bad.archives["ay-18.tar.zst"].clone();
    let bl = layout(&bdir);
    let err = install(
        &VendorFake { inner: bad, served },
        &bl,
        &anchor(),
        &req,
        fl(0),
        0,
    )
    .unwrap_err();
    match &err {
        FlowError::ShimEnv(why) => assert!(why.contains("PATH=/evil"), "{why}"),
        other => panic!("expected ShimEnv, got {other:?}"),
    }
    assert!(
        !bl.staging_dir("ay").exists(),
        "refused before any byte moved"
    );
    assert!(crate::ops::which(&bl, "ay").is_none());
    let _ = std::fs::remove_dir_all(&bdir);
}

/// Rule 2 on the `requires` path: a dependency the user's OWN copy satisfies
/// (`system = "<bin>"`, the binary on PATH outside the prefix) is reported
/// `DepResult::System(path)` and never installed — no manifest fetched, no byte
/// moved, no managed copy laid beside the user's.
#[cfg(unix)]
#[test]
fn a_system_satisfied_dependency_is_never_pulled_in_as_a_managed_copy() {
    let dir = scratch("requires-system");
    let sys = dir.join("usr-local-bin");
    let gh = lay_exe(&sys, "gh");
    let path = std::env::join_paths([sys.clone()]).unwrap();
    // `ay` requires `gh`; gh declares `system = "gh"` and is pinned, but its pkg
    // manifest is deliberately ABSENT: fetching it would fail loudly.
    let ay_dir = dir.join("ay");
    std::fs::create_dir_all(&ay_dir).unwrap();
    let fake = requires_fixture(&ay_dir, &[], &["gh"], &[]);
    let index_body = String::from_utf8(fake.index.clone()).unwrap().replace(
        "[programs.ny]\nrepo = \"ny\"\n",
        "[programs.ny]\nrepo = \"ny\"\n[programs.gh]\nrepo = \"gh\"\nsystem = \"gh\"\n",
    );
    let index_body = index_body.replace("ny = 9 }", "ny = 9, gh = 3 }");
    assert!(index_body.contains("gh = 3"), "{index_body}");
    let fake = Fake {
        index: index_body.clone().into_bytes(),
        index_sig: sign(&RELEASE_SEED, index_body.as_bytes()),
        pkg: fake.pkg,
        archives: fake.archives,
    };
    let lay = layout(&dir);
    let req = InstallRequest {
        channel: "stable",
        program: "ay",
        triple: TRIPLE,
        installed: None,
    };
    let report = crate::vendor::with_path_var(Some(&path), || {
        install(&fake, &lay, &anchor(), &req, fl(0), 0).unwrap()
    });
    assert_eq!(report.build, 18);
    assert_eq!(
        report.dependencies,
        vec![DepOutcome {
            program: "gh".into(),
            result: DepResult::System(gh.clone())
        }]
    );
    assert!(
        !crate::ops::active_builds(&lay).contains_key("gh"),
        "no managed gh was laid beside the user's copy"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[cfg(unix)]
fn lay_exe(dir: &Path, name: &str) -> PathBuf {
    std::fs::create_dir_all(dir).unwrap();
    let p = dir.join(name);
    std::fs::write(&p, b"#!/bin/sh\nexit 0\n").unwrap();
    std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
    p
}

#[test]
fn app_bundle_refused_closed_but_sysroot_bundle_installs() {
    // app-bundle is the app's own in-session apply topology, NOT a tool install — it
    // stays refused CLOSED on this path (atpkg never swaps the app), before any
    // download: the fetcher serves no archive, so a later refusal would read as one.
    let adir = scratch("dispatch-app");
    let mut app = fixture_with_kind(&adir, "app-bundle");
    app.archives.clear();
    let alay = layout(&adir);
    let areq = InstallRequest {
        channel: "stable",
        program: "ay",
        triple: TRIPLE,
        installed: None,
    };
    let err = install(&app, &alay, &anchor(), &areq, fl(0), 0).unwrap_err();
    // Refused as the app's own pair — a distinct refusal rather than a blanket
    // UnsupportedKind, so the pass can say the app updates itself.
    assert!(
        matches!(err, FlowError::AppBundleRefused(ref p) if p == "ay"),
        "got {err:?}"
    );
    assert!(
        crate::ops::which(&alay, "ay").is_none(),
        "refused install leaves no shim"
    );
    let _ = std::fs::remove_dir_all(&adir);

    // sysroot-bundle (self-contained default: pack-time-relocated, no rustup
    // needed) is NO LONGER refused — it stages, wires (no-op for self-contained),
    // activates + shims, and passes the fail-loud resolve check (the fixture bin
    // `#!/bin/true` runs to a clean exit, proving the loader resolved it).
    let sdir = scratch("dispatch-sr");
    let sr = fixture_with_kind(&sdir, "sysroot-bundle");
    let slay = layout(&sdir);
    let sreq = InstallRequest {
        channel: "stable",
        program: "ay",
        triple: TRIPLE,
        installed: None,
    };
    let rep = install(&sr, &slay, &anchor(), &sreq, fl(0), 0)
        .expect("self-contained sysroot-bundle should install");
    assert!(
        rep.shimmed.contains(&"ay".to_string()),
        "exposed tool shimmed"
    );
    assert_eq!(
        crate::ops::which(&slay, "ay").unwrap(),
        tool_bin(&slay.build_dir("ay", 18), "ay")
    );
    assert_eq!(
        std::fs::read_link(slay.program_current("ay")).unwrap(),
        slay.build_dir("ay", 18)
    );
    let _ = std::fs::remove_dir_all(&sdir);
}

/// Mach-O magic over garbage, shipped with tar mode 0644: the magic makes
/// [`crate::macho::is_native_object`] admit it to the resolve check, and the
/// missing exec bit makes the spawn fail (`EACCES`) — [`crate::sysroot::resolve_check`]'s
/// documented "cannot spawn" arm — so [`bundle_resolve_check`] errors. (A garbage
/// EXECUTABLE Mach-O is no good as a fixture: macOS reports the exec-format failure
/// as a NORMAL exit 126, which the check's run-to-completion contract accepts.)
#[cfg(unix)]
const BROKEN_NATIVE_BIN: &[u8] = &[0xcf, 0xfa, 0xed, 0xfe, 0, 0, 0, 0];
#[cfg(unix)]
const NO_EXEC_MODE: &[u8; 8] = b"0000644\0";

// THE resolve-failure unwind (fresh install): a sysroot-bundle whose exposed binary
// cannot load must NOT be left ACTIVE (`decide` would read it as UpToDate forever).
// The install path unwinds like the transactional flip: no pointer survives, the build
// is discarded, and a retry with a healthy bundle re-installs cleanly.
#[cfg(unix)]
#[test]
fn failed_bundle_resolve_check_unwinds_a_fresh_install() {
    let dir = scratch("resolve-fresh");
    let broken = fixture_with(&dir, "sysroot-bundle", BROKEN_NATIVE_BIN, NO_EXEC_MODE);
    let layout = layout(&dir);
    let req = InstallRequest {
        channel: "stable",
        program: "ay",
        triple: TRIPLE,
        installed: None,
    };
    let err = install(&broken, &layout, &anchor(), &req, fl(0), 0).unwrap_err();
    assert!(matches!(err, FlowError::Activate(_)), "got {err:?}");
    // Nothing points at the broken build, and the build itself is gone…
    assert!(crate::ops::which(&layout, "ay").is_none(), "no live shim");
    assert!(
        std::fs::symlink_metadata(layout.program_current("ay")).is_err(),
        "no witness link"
    );
    assert!(
        !layout.build_dir("ay", 18).exists(),
        "the broken build is discarded, never re-read as complete"
    );
    assert!(crate::ops::active_builds(&layout).is_empty());
    // …so a retry (healthy bundle, SAME store) re-installs instead of 'already current'.
    let hdir = scratch("resolve-fresh-retry");
    let healthy = fixture_with_kind(&hdir, "sysroot-bundle");
    let rep = install(&healthy, &layout, &anchor(), &req, fl(0), 0)
        .expect("a retry after the unwind re-installs");
    assert!(
        !rep.already_current,
        "the unwound install must not read as current"
    );
    assert_eq!(
        crate::ops::which(&layout, "ay").unwrap(),
        tool_bin(&layout.build_dir("ay", 18), "ay")
    );
    let _ = std::fs::remove_dir_all(&dir);
    let _ = std::fs::remove_dir_all(&hdir);
}

// The upgrade variant of the resolve-failure unwind: ay@17 is live, the pinned
// ay@18 bundle fails its resolve check — shims, witness, and channel `current` all
// revert to 17 (the prior working surface) and 18 is discarded, exactly what
// `flip_member` guarantees on the transactional path.
#[cfg(unix)]
#[test]
fn failed_bundle_resolve_check_reverts_an_upgrade_to_the_prior_build() {
    let dir = scratch("resolve-revert");
    let layout = layout(&dir);
    seed_build(&layout, "ay", 17, true); // ay@17 active + shimmed
    let broken = fixture_with(&dir, "sysroot-bundle", BROKEN_NATIVE_BIN, NO_EXEC_MODE);
    let req = InstallRequest {
        channel: "stable",
        program: "ay",
        triple: TRIPLE,
        installed: Some(17),
    };
    let err = install(&broken, &layout, &anchor(), &req, fl(0), 0).unwrap_err();
    assert!(matches!(err, FlowError::Activate(_)), "got {err:?}");
    let b17 = layout.build_dir("ay", 17);
    assert_eq!(
        crate::ops::which(&layout, "ay").unwrap(),
        tool_bin(&b17, "ay"),
        "the shim reverts to the prior working build"
    );
    assert_eq!(
        std::fs::read_link(layout.program_current("ay")).unwrap(),
        b17,
        "the witness reverts"
    );
    assert!(
        !layout.build_dir("ay", 18).exists(),
        "the broken build is discarded"
    );
    assert_eq!(
        crate::ops::active_builds(&layout).get("ay").copied(),
        Some(17),
        "the gate keeps seeing 17, so the update stays due — never 'already current'"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

// §7 (step 21): the single-program `install` path also DISABLES a tombstoned build's old
// working shim. ay@17 is active + shimmed; the channel pins ay=18 but yanks it, so decide()
// tombstones — install must both return Tombstoned AND replace bin/ay with a failing shim.
#[test]
fn install_tombstone_disables_the_old_shim() {
    let dir = scratch("install-tomb");
    let layout = layout(&dir);
    seed_build(&layout, "ay", 17, true); // ay@17 active + shimmed
    // A LIVE forwarding shim (a symlink on Unix, a forwarding `.cmd` on Windows).
    assert!(crate::platform::resolve_shim(&shim_of(&layout, "ay")).is_some());
    // Index pins ay=18 but yanks ay@18 → decide() == Tombstone for the installed ay@17.
    let fake = rollback_index(0, &["ay@18"]);
    let req = InstallRequest {
        channel: "stable",
        program: "ay",
        triple: TRIPLE,
        installed: Some(17),
    };
    let err = install(&fake, &layout, &anchor(), &req, fl(0), 0).unwrap_err();
    assert!(
        matches!(err, FlowError::Tombstoned(ref p) if p == "ay"),
        "got {err:?}"
    );
    let shim = shim_of(&layout, "ay");
    assert!(
        std::fs::symlink_metadata(&shim)
            .unwrap()
            .file_type()
            .is_file(),
        "old symlink shim replaced by a tombstone regular file"
    );
    assert!(
        crate::platform::resolve_shim(&shim).is_none(),
        "tombstone no longer forwards anywhere"
    );
    let out = std::process::Command::new(&shim).output().unwrap();
    assert!(!out.status.success(), "tombstone shim exits nonzero");
    let _ = std::fs::remove_dir_all(&dir);
}

/// An up-to-date pass re-points a MISSING `current` witness at the build the pin and
/// the shims already agree on — the `doctor` remedy for a program that is on PATH
/// with no link selecting it (`NoLiveWitness`). No fetch happens, and a second pass
/// over the now-healthy store is a no-op.
#[test]
fn an_up_to_date_pass_reasserts_a_missing_current_witness() {
    let dir = scratch("witness");
    let fake = fixture(&dir);
    let layout = layout(&dir);
    // Build 18 on disk and shimmed, exactly as an older manager left it: no
    // `store/ay/current`, no channel link.
    let build_dir = layout.build_dir("ay", 18);
    std::fs::create_dir_all(build_dir.join("bin")).unwrap();
    std::fs::write(build_dir.join("bin").join("ay"), b"#!/bin/true\n").unwrap();
    crate::activate::install_shims(&layout, &build_dir, &["ay".to_string()], Aliases::Off).unwrap();
    crate::store::mark_build_ready(&build_dir).unwrap();
    assert!(
        crate::gc::live_builds(&layout).get("ay").is_none(),
        "no witness before the pass"
    );
    let req = InstallRequest {
        channel: "stable",
        program: "ay",
        triple: TRIPLE,
        installed: Some(18),
    };
    let r = install(&fake, &layout, &anchor(), &req, fl(0), 0).unwrap();
    assert!(r.already_current && r.shimmed.is_empty());
    let live = crate::gc::live_builds(&layout);
    assert_eq!(
        live.get("ay").map(|w| w.build()),
        Some(18),
        "diverged: {:?}",
        live.diverged()
    );
    assert_eq!(
        std::fs::read_link(layout.program_current("ay")).unwrap(),
        build_dir
    );
    let r = install(&fake, &layout, &anchor(), &req, fl(0), 0).unwrap();
    assert!(r.already_current);
    let _ = std::fs::remove_dir_all(&dir);
}

/// THE ROSETTA REPAIR, driven through the real apply decision. A build that is LIVE —
/// its shims resolve into it, so [`crate::ops::active_builds`] reports it and every
/// apply path feeds that number to `decide` — but whose `<build>.ready` marker this
/// slice refuses must be RE-STAGED, not read as already-current.
///
/// [`installed_for_decide`] is what keeps `decide` from answering UpToDate off the shim
/// view, which would leave the store on the other architecture's binaries.
#[test]
fn a_live_build_this_slice_cannot_vouch_for_is_re_staged() {
    let dir = scratch("otherslice");
    let fake = fixture(&dir);
    let layout = layout(&dir);
    let build_dir = layout.build_dir("ay", 18);
    let fresh = InstallRequest {
        channel: "stable",
        program: "ay",
        triple: TRIPLE,
        installed: None,
    };
    // 1. A real install: ay@18 is live, shimmed, and marked by THIS slice.
    let first = install(&fake, &layout, &anchor(), &fresh, fl(0), 0).unwrap();
    assert!(!first.already_current);
    assert!(crate::store::build_is_complete(&build_dir));

    // 2. Forge the marker the x86_64 slice would have left on this machine: the
    //    SIBLING `<build>.ready`, the one file `store::build_is_complete` reads.
    let marker = build_dir.with_file_name("18.ready");
    std::fs::write(&marker, "ok\nplatform=some-other-arch-macos\n").unwrap();
    assert!(
        !crate::store::build_is_complete(&build_dir),
        "PRECONDITION: the tree is not a complete install for this slice"
    );
    assert_eq!(
        crate::ops::active_builds(&layout).get("ay").copied(),
        Some(18),
        "PRECONDITION: the shim view — what every apply path feeds `decide` — says 18"
    );

    // 3. The next native pass, fed exactly what the CLI feeds it.
    let live = InstallRequest {
        installed: Some(18),
        ..fresh
    };
    let again = install(&fake, &layout, &anchor(), &live, fl(0), 0).unwrap();
    assert!(
        !again.already_current,
        "a build this slice cannot vouch for was called up to date — nothing will ever \
         re-stage it, and doctor's `install ay` remedy is a no-op"
    );
    assert_eq!(again.shimmed, vec!["ay".to_string()]);
    assert!(
        crate::store::build_is_complete(&build_dir),
        "the re-stage must leave a marker THIS slice accepts"
    );
    assert!(
        crate::ops::list_installed(&layout)
            .iter()
            .any(|(p, b)| p == "ay" && *b == 18),
        "and the repaired build reads as installed again"
    );
    assert_eq!(
        crate::ops::which(&layout, "ay").unwrap(),
        tool_bin(&build_dir, "ay"),
        "the live tool still resolves into the re-staged tree"
    );

    // 4. …and a HEALTHY store is still a no-op: the marker is an input to the decision,
    //    never a re-download trigger.
    let again = install(&fake, &layout, &anchor(), &live, fl(0), 0).unwrap();
    assert!(again.already_current && again.shimmed.is_empty());
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn unreachable_program_is_refused() {
    let dir = scratch("unreach");
    let fake = fixture(&dir);
    let layout = layout(&dir);
    // "dotfiles" is not named in the signed index.
    let req = InstallRequest {
        channel: "stable",
        program: "dotfiles",
        triple: TRIPLE,
        installed: None,
    };
    let err = install(&fake, &layout, &anchor(), &req, fl(0), 0).unwrap_err();
    // The refusal answers the follow-up question from the same verified index.
    assert!(
        matches!(&err, FlowError::NotReachable(p, roster) if p == "dotfiles" && roster == &["ay"]),
        "got {err:?}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn missing_triple_is_a_clean_skip() {
    let dir = scratch("triple");
    let fake = fixture(&dir);
    let layout = layout(&dir);
    let req = InstallRequest {
        channel: "stable",
        program: "ay",
        triple: "x86_64-unknown-linux-gnu",
        installed: None,
    };
    let err = install(&fake, &layout, &anchor(), &req, fl(0), 0).unwrap_err();
    assert!(matches!(err, FlowError::NoArtifact(_)), "got {err:?}");
    let _ = std::fs::remove_dir_all(&dir);
}

// Disk preflight is a PURE gate over an injected `available`, so the flow's disk logic is
// unit-tested WITHOUT a real statvfs (keeping the e2e tests hermetic).
#[test]
fn disk_gate_is_pure_and_fails_only_on_a_real_shortfall() {
    // A genuine measured shortfall refuses.
    assert!(matches!(
        disk_gate(u64::MAX, Some(1000)),
        Err(FlowError::InsufficientDisk { .. })
    ));
    // The need shown includes the reserve the check holds back: 400 MiB to install
    // on 900 MiB free refuses, and must never read "needs 400 MiB, 900 MiB free".
    let mib = 1u64 << 20;
    let Err(
        e @ FlowError::InsufficientDisk {
            required,
            available,
        },
    ) = disk_gate(400 * mib, Some(900 * mib))
    else {
        panic!("400 MiB on 900 MiB free must refuse under the 1 GiB reserve");
    };
    assert_eq!(required, 400 * mib + crate::cost::FREE_FLOOR);
    assert_eq!(available, 900 * mib);
    assert!(e.to_string().starts_with(DISK_REFUSAL_HEAD), "{e}");
    // Query failure (None) => fail OPEN.
    assert!(disk_gate(0, None).is_ok());
    // Ample space => ok.
    assert!(disk_gate(1 << 20, Some(u64::MAX)).is_ok());
}

// --- coherence-group transactional apply (apply_channel over the REAL flow) --------

/// A single-file USTAR + zstd archive carrying `bin/<program>`.
fn prog_archive(dir: &Path, program: &str, build: u64) -> PathBuf {
    fn entry(name: &str, content: &[u8]) -> Vec<u8> {
        let mut h = [0u8; 512];
        let nb = name.as_bytes();
        h[..nb.len()].copy_from_slice(nb);
        h[100..108].copy_from_slice(b"0000755\0");
        h[108..116].copy_from_slice(b"0000000\0");
        h[116..124].copy_from_slice(b"0000000\0");
        h[124..136].copy_from_slice(format!("{:011o}\0", content.len()).as_bytes());
        h[136..148].copy_from_slice(b"00000000000\0");
        h[148..156].copy_from_slice(b"        ");
        h[156] = b'0';
        h[257..263].copy_from_slice(b"ustar\0");
        h[263..265].copy_from_slice(b"00");
        let sum: u32 = h.iter().map(|&b| u32::from(b)).sum();
        h[148..156].copy_from_slice(format!("{sum:06o}\0 ").as_bytes());
        let mut out = h.to_vec();
        out.extend_from_slice(content);
        out.resize(out.len() + (512 - content.len() % 512) % 512, 0);
        out
    }
    let mut tar = Vec::new();
    tar.extend(entry(
        &format!("bin/{program}"),
        format!("#!/bin/true\n{program}").as_bytes(),
    ));
    tar.resize(tar.len() + 1024, 0);
    let path = dir.join(format!("{program}-{build}.tar.zst"));
    std::fs::write(&path, crate::extract::fixtures::zstd_bytes(&tar)).unwrap();
    path
}

/// A signed release whose `stable` channel pins a `rustc` coherence group (trust@4821 +
/// ay@18) plus consistent per-build manifests + archives.
fn group_fixture(dir: &Path) -> Fake {
    group_fixture_with(dir, &[])
}

/// [`group_fixture`] with each program in `unserved` published only for a foreign
/// triple — how the release looks from a host the publisher does not build it for.
fn group_fixture_with(dir: &Path, unserved: &[&str]) -> Fake {
    let mut pkg = HashMap::new();
    let mut archives = HashMap::new();
    for (program, build) in [("trust", 4821u64), ("ay", 18u64)] {
        let target = if unserved.contains(&program) {
            "riscv64gc-unknown-linux-gnu"
        } else {
            TRIPLE
        };
        let archive = prog_archive(dir, program, build);
        let sha = crate::tree::file_sha256(&archive).unwrap();
        let root = signed_root_of(&archive, &dir.join(format!("probe-{program}")));
        let asset = format!("{program}-{build}.tar.zst");
        let pkg_body = format!(
            "schema = 2\nprogram = \"{program}\"\nversion = \"0.1\"\nbuild_number = {build}\n\
             exposes = [\"{program}\"]\n\
             [[artifact]]\ntarget = \"{target}\"\nkind = \"binary\"\nasset = \"{asset}\"\n\
             sha256 = \"{sha}\"\ntree_root = \"{root}\"\nsize = 100\n\
             [artifact.cost]\ndisk_installed = 1048576\n"
        );
        pkg.insert(
            (program.to_string(), build),
            (
                pkg_body.clone().into_bytes(),
                sign(&RELEASE_SEED, pkg_body.as_bytes()),
            ),
        );
        archives.insert(asset, archive);
    }
    let index_body = format!(
        "schema = 2\nindex_build = 41\nvalid_until = \"2026-07-05T12:00:00Z\"\n{attr}\
                      [programs.trust]\nrepo = \"trust\"\ncoherence_group = \"rustc\"\n\
         [programs.ay]\nrepo = \"ay\"\ncoherence_group = \"rustc\"\n\
         [[channels]]\nname = \"stable\"\nchannel_build = 1\nmin_build = 0\n\
         pin = {{ trust = 4821, ay = 18 }}\n",
        attr = attribution()
    );
    Fake {
        index: index_body.clone().into_bytes(),
        index_sig: sign(&RELEASE_SEED, index_body.as_bytes()),
        pkg,
        archives,
    }
}

/// Unix seconds the quiet-flip tests start from.
const QUIET_T0: i64 = 1_790_000_000;

/// A store whose `rustc` tuple is live at trust@4800 + ay@17 — what an unattended pass
/// would supersede with [`group_fixture`]'s trust@4821 + ay@18.
fn live_toolchain(layout: &Layout) -> std::collections::BTreeMap<String, u64> {
    seed_build(layout, "trust", 4800, true);
    seed_build(layout, "ay", 17, true);
    std::collections::BTreeMap::from([("trust".to_string(), 4800u64), ("ay".to_string(), 17)])
}

/// One unattended pass over `fake` at `now`, under the injected process table.
fn quiet_pass(
    fake: &dyn Fetcher,
    layout: &Layout,
    installed: &std::collections::BTreeMap<String, u64>,
    now: i64,
    running: &dyn Fn() -> Option<Vec<PathBuf>>,
) -> ChannelApplyReport {
    let index = resolve_verified_index(fake, layout, &anchor(), fl(0), 0).unwrap();
    let gate = crate::quiet::FlipGate {
        policy: crate::quiet::FlipPolicy::WhenQuiet,
        now,
        running,
    };
    apply_channel_gated(
        fake,
        layout,
        &index,
        "stable",
        TRIPLE,
        installed,
        &[],
        &gate,
    )
    .unwrap()
}

/// WHEN WAITING CANNOT HELP, A BUSY TOOLCHAIN FLIPS AT ONCE:
/// * a wait nothing can record is no wait — the ceiling is measured from the record's
///   `since`, so with the record unwritable every pass would begin the wait again and a
///   busy machine would hold the toolchain for good;
/// * only the compiler's move waits — trust already at its pin, a sibling moving, the
///   build running from the live trust keeps its compiler whatever flips;
/// * nothing live, nothing to wait for — trust has no live build (a tombstone being
///   re-staged, a member the tuple pulls back in), so a busy SIBLING held a disabled
///   toolchain disabled for up to four hours behind a process not using it;
/// * a revoked live build never waits — ay@17 yanked, the tuple force-upgrades.
#[test]
fn a_busy_toolchain_flips_at_once_when_waiting_cannot_help() {
    type Prep = fn(&Layout) -> std::collections::BTreeMap<String, u64>;
    type Fixture = fn(&Path) -> Fake;
    type Flip<'a> = (&'a str, u64, &'a str);
    let cases: [(&str, Fixture, Prep, Flip, &[&str]); 4] = [
        (
            "unrecordable",
            group_fixture,
            |l| {
                let installed = live_toolchain(l);
                // The record's own path is a directory: the write's rename over it fails.
                std::fs::create_dir_all(crate::quiet::record_path(l)).unwrap();
                installed
            },
            ("trust", 4800, "targo"),
            &["ay", "trust"],
        ),
        (
            "sibling-only",
            group_fixture,
            |l| {
                seed_build(l, "trust", 4821, true);
                seed_build(l, "ay", 17, true);
                crate::ops::active_builds(l)
            },
            ("trust", 4821, "targo"),
            &["ay"],
        ),
        (
            "no-live-compiler",
            group_fixture,
            |l| {
                seed_build(l, "ay", 17, true);
                crate::ops::active_builds(l)
            },
            ("ay", 17, "ay"),
            &["ay", "trust"],
        ),
        (
            "revoked",
            group_fixture_yanking_ay17,
            live_toolchain,
            ("trust", 4800, "targo"),
            &["ay", "trust"],
        ),
    ];
    for (label, make, prep, (program, build, exe), applied) in cases {
        let dir = scratch(&format!("quiet-{label}"));
        let fake = make(&dir);
        let layout = layout(&dir);
        let installed = prep(&layout);
        let busy = layout.build_dir(program, build).join("bin").join(exe);
        let report = quiet_pass(&fake, &layout, &installed, QUIET_T0, &|| {
            Some(vec![busy.clone()])
        });
        assert_eq!(
            report.groups[0].1,
            TxnOutcome::Applied(applied.iter().map(|m| (*m).to_string()).collect()),
            "{label}"
        );
        assert_eq!(
            crate::quiet::read(&layout),
            None,
            "{label}: no wait recorded"
        );
        assert_eq!(
            crate::ops::active_builds(&layout).get("ay").copied(),
            Some(18),
            "{label}"
        );
    }
}

/// STAGE NOW, FLIP WHEN QUIET (gap #12(b)). An unattended pass meets a build running from
/// the live toolchain: every member it would move has its signed archive in `staging/`,
/// nothing is extracted, nothing flips, and the wait is recorded. A second pass, still
/// busy, downloads nothing and keeps the FIRST deferral's clock. The pass that finds the
/// toolchain quiet flips the tuple from those archives, with no download, and the record
/// goes.
#[test]
fn an_unattended_pass_stages_the_toolchain_and_flips_it_when_quiet() {
    let dir = scratch("quiet-stage");
    let fake = Counting::of(group_fixture(&dir));
    let layout = layout(&dir);
    let installed = live_toolchain(&layout);
    let targo = layout.build_dir("trust", 4800).join("bin/targo");
    let busy = || Some(vec![PathBuf::from("/usr/bin/login"), targo.clone()]);

    let report = quiet_pass(&fake, &layout, &installed, QUIET_T0, &busy);
    let TxnOutcome::Deferred { members, since, .. } = &report.groups[0].1 else {
        panic!("a busy toolchain holds the flip: {:?}", report.groups[0].1);
    };
    assert_eq!(members, &["ay".to_string(), "trust".to_string()]);
    assert_eq!(*since, QUIET_T0);
    assert!(report.applied.is_empty());
    assert_eq!(
        crate::ops::active_builds(&layout).get("trust").copied(),
        Some(4800),
        "nothing flipped"
    );
    assert!(
        !layout.build_dir("trust", 4821).exists(),
        "nothing extracted while the toolchain is in use"
    );
    for asset in ["trust-4821.tar.zst", "ay-18.tar.zst"] {
        let program = asset.split('-').next().unwrap();
        assert!(
            layout.staging_dir(program).join(asset).is_file(),
            "{asset} is staged"
        );
        assert_eq!(fake.count(asset), 1);
        assert_eq!(
            report.resolved_assets.get(program).map(String::as_str),
            Some(asset),
            "the pass-end gc spares what waits"
        );
    }
    let record = crate::quiet::read(&layout).expect("the wait is recorded");
    assert_eq!(record.since, QUIET_T0);

    // Still busy an hour on: no download, the first deferral's clock kept.
    let report = quiet_pass(&fake, &layout, &installed, QUIET_T0 + 3600, &busy);
    assert!(
        matches!(report.groups[0].1, TxnOutcome::Deferred { since, .. } if since == QUIET_T0),
        "{:?}",
        report.groups[0].1
    );
    assert_eq!(
        fake.count("trust-4821.tar.zst"),
        1,
        "the staged archive is reused"
    );
    assert_eq!(
        crate::quiet::read(&layout).unwrap().checked,
        QUIET_T0 + 3600
    );
    // The record names each staged archive, and a gc that resolved none of them — a
    // person's `atpkg gc`, an install's or the seed's pass-end sweep — keeps them for the
    // flip, so the flip below still downloads nothing.
    assert_eq!(
        record.assets,
        std::collections::BTreeMap::from([
            ("ay".to_string(), "ay-18.tar.zst".to_string()),
            ("trust".to_string(), "trust-4821.tar.zst".to_string()),
        ])
    );
    let gc = crate::gc::run(&layout, None);
    assert_eq!(gc.held_staging.len(), 2, "{:?}", gc.held_staging);

    // Quiet: the tuple flips from the staged archives, and the record goes.
    let report = quiet_pass(&fake, &layout, &installed, QUIET_T0 + 7200, &|| {
        Some(vec![PathBuf::from("/usr/bin/login")])
    });
    assert_eq!(
        report.groups[0].1,
        TxnOutcome::Applied(vec!["ay".into(), "trust".into()])
    );
    assert_eq!(fake.count("trust-4821.tar.zst"), 1, "no second download");
    assert_eq!(
        crate::ops::active_builds(&layout).get("trust").copied(),
        Some(4821)
    );
    assert_eq!(crate::quiet::read(&layout), None, "the wait is over");
    let _ = std::fs::remove_dir_all(&dir);
}

/// A DOWNLOAD THAT FAILS ITS SIGNED DIGEST while the flip waits is the stage's failure,
/// said and recorded the stage's way: the pass aborts, the bad archive goes, and the
/// refusal memo keeps the next pass from paying the download to be told again.
#[test]
fn a_staged_archive_that_fails_its_digest_aborts_and_is_remembered() {
    let dir = scratch("quiet-poison");
    let fake = Counting::of(group_fixture(&dir));
    let layout = layout(&dir);
    let installed = live_toolchain(&layout);
    poison(&dir.join("trust-4821.tar.zst"));
    let targo = layout.build_dir("trust", 4800).join("bin/targo");
    let busy = || Some(vec![targo.clone()]);
    let report = quiet_pass(&fake, &layout, &installed, QUIET_T0, &busy);
    let TxnOutcome::Aborted { failed, why, .. } = &report.groups[0].1 else {
        panic!("a poisoned archive aborts: {:?}", report.groups[0].1);
    };
    assert_eq!(failed, "trust");
    assert!(why.contains("sha256"), "{why}");
    assert!(
        !layout
            .staging_dir("trust")
            .join("trust-4821.tar.zst")
            .exists()
    );
    assert!(
        crate::store::stage_refusal(&layout.build_dir("trust", 4821)).is_some(),
        "the refusal is remembered"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// A LEASE HOLDS THE FLIP TOO, with no process in sight: a release cut between
/// two steps. And an unreadable process table is priced as in use.
#[test]
fn a_lease_or_an_unreadable_table_holds_the_flip() {
    let dir = scratch("quiet-lease");
    let fake = group_fixture(&dir);
    let layout = layout(&dir);
    let installed = live_toolchain(&layout);
    let lease = crate::lease::take(
        &layout.prefix,
        &crate::lease::Subject::build("trust", 4800).unwrap(),
        "aterm-release (pid 7) \u{2014} a release cut in /w",
        crate::lease::DEFAULT_WAIT,
    )
    .unwrap();
    let report = quiet_pass(&fake, &layout, &installed, QUIET_T0, &|| Some(Vec::new()));
    let TxnOutcome::Deferred { why, .. } = &report.groups[0].1 else {
        panic!(
            "a leased toolchain holds the flip: {:?}",
            report.groups[0].1
        );
    };
    assert!(
        why.contains("aterm-release (pid 7)"),
        "the holder is named: {why}"
    );
    drop(lease);
    let report = quiet_pass(&fake, &layout, &installed, QUIET_T0 + 60, &|| None);
    let TxnOutcome::Deferred { .. } = &report.groups[0].1 else {
        panic!("unknown is in use: {:?}", report.groups[0].1);
    };
    assert_eq!(
        crate::ops::active_builds(&layout).get("trust").copied(),
        Some(4800)
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// THE WAIT IS BOUNDED: at four hours from the first deferral the flip lands however
/// busy the toolchain is. A PERSON NEVER WAITS: a pass without the unattended policy
/// flips at once over the same busy toolchain.
#[test]
fn the_ceiling_flips_a_busy_toolchain_and_a_person_never_waits() {
    {
        let dir = scratch("quiet-ceiling");
        let fake = group_fixture(&dir);
        let layout = layout(&dir);
        let installed = live_toolchain(&layout);
        let targo = layout.build_dir("trust", 4800).join("bin/targo");
        let busy = || Some(vec![targo.clone()]);
        let first = quiet_pass(&fake, &layout, &installed, QUIET_T0, &busy);
        assert!(matches!(first.groups[0].1, TxnOutcome::Deferred { .. }));
        let late = quiet_pass(
            &fake,
            &layout,
            &installed,
            QUIET_T0 + crate::quiet::CEILING_SECS - 1,
            &busy,
        );
        assert!(matches!(late.groups[0].1, TxnOutcome::Deferred { .. }));
        let ceiling = quiet_pass(
            &fake,
            &layout,
            &installed,
            QUIET_T0 + crate::quiet::CEILING_SECS,
            &busy,
        );
        assert!(
            matches!(ceiling.groups[0].1, TxnOutcome::Applied(_)),
            "{:?}",
            ceiling.groups[0].1
        );
        assert_eq!(crate::quiet::read(&layout), None);
        let _ = std::fs::remove_dir_all(&dir);
    }

    let dir = scratch("quiet-person");
    let fake = group_fixture(&dir);
    let person = layout(&dir);
    let installed = live_toolchain(&person);
    let index = resolve_verified_index(&fake, &person, &anchor(), fl(0), 0).unwrap();
    // `FlipGate::NOW`: what `aterm pkg update` runs.
    let report = apply_channel_gated(
        &fake,
        &person,
        &index,
        "stable",
        TRIPLE,
        &installed,
        &[],
        &crate::quiet::FlipGate::NOW,
    )
    .unwrap();
    assert!(matches!(report.groups[0].1, TxnOutcome::Applied(_)));
    let _ = std::fs::remove_dir_all(&dir);
}

/// A HELD FLIP NO GROUP HOLDS ANY LONGER IS OVER: a pass that skips the toolchain's
/// group whole — nothing of it installed any more — ends the record, so the window's
/// park stops looking at a flip nobody will make.
#[test]
fn a_held_flip_no_group_holds_is_ended_by_the_next_pass() {
    let dir = scratch("quiet-orphan");
    let fake = group_fixture(&dir);
    let layout = layout(&dir);
    let installed = live_toolchain(&layout);
    let targo = layout.build_dir("trust", 4800).join("bin/targo");
    let busy = || Some(vec![targo.clone()]);
    let report = quiet_pass(&fake, &layout, &installed, QUIET_T0, &busy);
    assert!(matches!(report.groups[0].1, TxnOutcome::Deferred { .. }));
    assert!(crate::quiet::read(&layout).is_some());
    let nothing = std::collections::BTreeMap::new();
    let report = quiet_pass(&fake, &layout, &nothing, QUIET_T0 + 60, &busy);
    assert!(report.groups.is_empty(), "{:?}", report.groups);
    assert_eq!(crate::quiet::read(&layout), None);
    let _ = std::fs::remove_dir_all(&dir);
}

/// THE LAST LOOK: quiet when the pass began, but a build started while the tree was
/// being extracted — the flip is held after all, the extracted trees go (no complete
/// inactive build is left above the live one), and the archives stay for the flip that
/// follows.
#[test]
fn a_build_started_during_the_extract_holds_the_flip_after_all() {
    let dir = scratch("quiet-last-look");
    let fake = Counting::of(group_fixture(&dir));
    let layout = layout(&dir);
    let installed = live_toolchain(&layout);
    let targo = layout.build_dir("trust", 4800).join("bin/targo");
    let looks = std::cell::Cell::new(0u32);
    let table = || {
        looks.set(looks.get() + 1);
        Some(if looks.get() == 1 {
            Vec::new()
        } else {
            vec![targo.clone()]
        })
    };
    let report = quiet_pass(&fake, &layout, &installed, QUIET_T0, &table);
    assert!(
        matches!(report.groups[0].1, TxnOutcome::Deferred { .. }),
        "{:?}",
        report.groups[0].1
    );
    assert_eq!(
        looks.get(),
        2,
        "one look before the stage, one before the flip"
    );
    assert!(
        !layout.build_dir("trust", 4821).exists(),
        "the extracted tree went"
    );
    assert!(!layout.build_dir("ay", 18).exists());
    assert!(
        layout
            .staging_dir("trust")
            .join("trust-4821.tar.zst")
            .is_file()
    );
    assert_eq!(
        crate::ops::active_builds(&layout).get("trust").copied(),
        Some(4800)
    );
    assert!(crate::quiet::read(&layout).is_some());
    let report = quiet_pass(&fake, &layout, &installed, QUIET_T0 + 60, &|| {
        Some(Vec::new())
    });
    assert!(matches!(report.groups[0].1, TxnOutcome::Applied(_)));
    assert_eq!(fake.count("trust-4821.tar.zst"), 1, "the archive was kept");
    let _ = std::fs::remove_dir_all(&dir);
}

/// [`group_fixture`]'s signed release, re-published with the `rustc` tuple pinned ONLY
/// through a `pin_by_target` overlay for [`TRIPLE`]: the platform-agnostic `pin` names
/// `trust@4820` (a build with no manifest this fetcher serves, i.e. another target's
/// pin) and never names `ay`, while the overlay REPLACES trust with 4821 and ADDS
/// ay@18. A lane that reads the raw pin plans `trust` alone and stages a build that
/// cannot be fetched; only the per-target view reaches the builds this host can run.
fn overlay_group_fixture(dir: &Path) -> Fake {
    let mut fake = group_fixture(dir);
    let index_body = format!(
        "schema = 2\nindex_build = 41\nvalid_until = \"2026-07-05T12:00:00Z\"\n{attr}\
         [programs.trust]\nrepo = \"trust\"\ncoherence_group = \"rustc\"\n\
         [programs.ay]\nrepo = \"ay\"\ncoherence_group = \"rustc\"\n\
         [[channels]]\nname = \"stable\"\nchannel_build = 1\nmin_build = 0\n\
         pin = {{ trust = 4820 }}\n\
         pin_by_target = {{ \"{TRIPLE}\" = {{ trust = 4821, ay = 18 }} }}\n",
        attr = attribution()
    );
    fake.index_sig = sign(&RELEASE_SEED, index_body.as_bytes());
    fake.index = index_body.into_bytes();
    fake
}

/// The update lane honours the per-target pin: `apply_channel` and `apply_program`
/// plan the tuple from `channel_for(.., TRIPLE)`, so the overlay-ADDED `ay` is a
/// member at all and `trust` lands on the overlay's 4821, never the raw channel's.
#[test]
fn update_lanes_apply_the_per_target_pin_overlay() {
    for lane in ["channel", "program"] {
        let dir = scratch(&format!("overlay-update-{lane}"));
        let fake = overlay_group_fixture(&dir);
        let layout = layout(&dir);
        let installed = std::collections::BTreeMap::from([("ay".to_string(), 17u64)]);
        let report = if lane == "channel" {
            apply_channel(
                &fake,
                &layout,
                &anchor(),
                "stable",
                TRIPLE,
                &installed,
                &[],
                fl(0),
                0,
            )
        } else {
            apply_program(
                &fake,
                &layout,
                &anchor(),
                "stable",
                TRIPLE,
                "ay",
                &installed,
                &[],
                fl(0),
                0,
            )
        }
        .unwrap_or_else(|e| panic!("{lane}: the overlaid tuple is planned: {e}"));
        assert_eq!(report.groups.len(), 1, "{lane}: one coherence group");
        assert_eq!(
            report.groups[0].1,
            TxnOutcome::Applied(vec!["ay".into(), "trust".into()]),
            "{lane}: the whole overlaid tuple moves"
        );
        assert_eq!(
            crate::ops::which(&layout, "trust").unwrap(),
            tool_bin(&layout.build_dir("trust", 4821), "trust"),
            "{lane}: trust lands on the overlay's build, not the raw pin"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}

/// The tuple-bootstrap lane stages what its caller planned: `bootstrap_group` resolves
/// the same per-target view the CLI's plan and `group_missing_triple` prescan used, so
/// a group the prescan says is served installs whole (the raw pin, pkg-trust-4820, is
/// not served for this host).
#[test]
fn bootstrap_group_stages_the_per_target_pin_overlay() {
    let dir = scratch("overlay-bootstrap");
    let fake = overlay_group_fixture(&dir);
    let layout = layout(&dir);
    let index = resolve_verified_index(&fake, &layout, &anchor(), fl(0), 0).unwrap();
    let ch = index.channel_for("stable", TRIPLE).unwrap();
    let group = crate::plan_groups(&index, &ch)
        .into_iter()
        .find(|g| g.group.as_deref() == Some("rustc"))
        .expect("the overlay pins the rustc tuple");
    assert_eq!(
        group_missing_triple(&fake, &index, "stable", TRIPLE, &group.members),
        None,
        "the prescan sees the tuple served on this target"
    );
    let mut resolved = BTreeMap::new();
    let (outcome, _) = bootstrap_group(
        &fake,
        &layout,
        &index,
        "stable",
        TRIPLE,
        &group,
        &BTreeMap::new(),
        &mut resolved,
        &crate::quiet::FlipGate::NOW,
    )
    .unwrap();
    assert_eq!(
        outcome,
        TxnOutcome::Applied(vec!["ay".into(), "trust".into()]),
        "the bootstrap stages the overlay's builds, not the raw pin"
    );
    assert_eq!(
        crate::ops::which(&layout, "trust").unwrap(),
        tool_bin(&layout.build_dir("trust", 4821), "trust")
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// `plan_update` decides against the per-target pin: trust@4821 installed from the
/// overlay is up to date (not a "move" to another target's 4820), and the overlay-added
/// `ay` is pinned (not `NotPinned`).
#[test]
fn plan_update_decides_against_the_per_target_pin_overlay() {
    let dir = scratch("overlay-plan");
    let fake = overlay_group_fixture(&dir);
    let layout = layout(&dir);
    let trust = plan_update(
        &fake,
        &layout,
        &anchor(),
        "stable",
        TRIPLE,
        "trust",
        Some(4821),
        fl(0),
        0,
    )
    .unwrap();
    assert_eq!(trust.decision, ApplyDecision::UpToDate);
    assert!(trust.current_build_ok);
    let ay = plan_update(
        &fake,
        &layout,
        &anchor(),
        "stable",
        TRIPLE,
        "ay",
        Some(17),
        fl(0),
        0,
    )
    .unwrap();
    assert_eq!(ay.decision, ApplyDecision::Install);
    let _ = std::fs::remove_dir_all(&dir);
}

// Atomicity under failure: corrupt one member's archive so its stage fails; the WHOLE
// group aborts during the stage phase, NOTHING is flipped, AND the already-staged
// sibling's build is DISCARDED (not left complete-but-inactive) — then a retry with the
// corruption cleared heals the tuple coherently.
#[test]
fn a_member_stage_failure_aborts_the_group_and_a_retry_heals_it() {
    let dir = scratch("group-abort");
    let fake = group_fixture(&dir);
    // Corrupt trust's archive → its sha256 re-verify fails at stage.
    let trust_archive = fake.archives.get("trust-4821.tar.zst").unwrap().clone();
    std::fs::write(&trust_archive, b"corrupt-not-the-signed-bytes").unwrap();
    let layout = layout(&dir);
    let installed = std::collections::BTreeMap::from([("ay".to_string(), 17u64)]);

    let report = apply_channel(
        &fake,
        &layout,
        &anchor(),
        "stable",
        TRIPLE,
        &installed,
        &[],
        fl(0),
        0,
    )
    .unwrap();
    assert!(
        matches!(
            report.groups[0].1,
            TxnOutcome::Aborted {
                during_flip: false,
                ..
            }
        ),
        "a stage failure aborts BEFORE any flip: {:?}",
        report.groups[0].1
    );
    // Nothing flipped.
    assert!(
        crate::ops::which(&layout, "trust").is_none(),
        "trust never flipped"
    );
    assert!(
        crate::ops::which(&layout, "ay").is_none(),
        "ay never flipped (group is atomic)"
    );
    // CRUCIAL — the already-staged ay@18 build was DISCARDED, so it can't be mis-read as
    // active on the next run (which would permanently split the tuple).
    assert!(
        !layout.build_dir("ay", 18).exists(),
        "aborted-group staged build is discarded"
    );
    assert!(
        !crate::store::build_is_complete(&layout.build_dir("ay", 18)),
        "no lingering completeness marker for the discarded build"
    );
    // The FAILED member's archive is reclaimed — nobody ever comes back for it — while
    // the sibling that staged fine BEFORE the abort KEEPS its verified archive: the
    // abort takes its extracted tree (it must), and taking the download with it made
    // the next pass refetch a member that never failed (the carry test above).
    assert!(
        staged_assets(&layout, "trust").is_empty(),
        "the failing member stranded its archive: {:?}",
        staged_assets(&layout, "trust")
    );
    assert_eq!(
        staged_assets(&layout, "ay"),
        vec!["ay-18.tar.zst".to_string()]
    );

    // Heal: clear the corruption (re-create the real signed archive) and retry.
    prog_archive(&dir, "trust", 4821);
    let report2 = apply_channel(
        &fake,
        &layout,
        &anchor(),
        "stable",
        TRIPLE,
        &installed,
        &[],
        fl(0),
        0,
    )
    .unwrap();
    assert_eq!(
        report2.groups[0].1,
        TxnOutcome::Applied(vec!["ay".into(), "trust".into()])
    );
    assert_eq!(
        crate::ops::which(&layout, "ay").unwrap(),
        tool_bin(&layout.build_dir("ay", 18), "ay")
    );
    assert_eq!(
        crate::ops::which(&layout, "trust").unwrap(),
        tool_bin(&layout.build_dir("trust", 4821), "trust")
    );
    let _ = std::fs::remove_dir_all(&dir);
}

// The abort discard must never delete a build that was ALREADY LIVE when this
// transaction re-staged it. The shim-derived `installed` view goes silent when a
// program's tools are gone (`atpkg unlink`, tombstones, dev-link mode) while the
// `current` authority links still name the live build, so `decide` legitimately
// returns Install for the build that is already active. A SIBLING member then failing
// to stage must not take that live toolchain down with it.
#[test]
fn a_group_abort_never_discards_a_member_that_was_already_live() {
    let dir = scratch("group-abort-live");
    let fake = group_fixture(&dir);
    let layout = layout(&dir);

    // 1. Really install the tuple: ay@18 + trust@4821 go live.
    let installed = std::collections::BTreeMap::from([("ay".to_string(), 17u64)]);
    apply_channel(
        &fake,
        &layout,
        &anchor(),
        "stable",
        TRIPLE,
        &installed,
        &[],
        fl(0),
        0,
    )
    .unwrap();
    let ay18 = layout.build_dir("ay", 18);
    assert!(
        crate::store::build_is_complete(&ay18),
        "PRECONDITION: ay@18 is installed and complete"
    );

    // 2. `atpkg unlink`-shaped state: ay's shims are gone — the primary AND the
    //    `alab-ay` alias the flip laid beside it (ay is ALab's own in this index), since
    //    the alias resolves into the same build and would keep the shim view talking —
    //    so the SHIM view is silent for ay while the authority link still names ay@18
    //    as live.
    std::fs::remove_file(layout.shim(&tool("ay"))).unwrap();
    std::fs::remove_file(layout.shim(&tool("alab-ay"))).unwrap();
    assert!(
        !crate::ops::active_builds(&layout).contains_key("ay"),
        "PRECONDITION: the shim view no longer knows ay — `decide` will re-Install 18"
    );
    assert_eq!(
        std::fs::read_link(layout.program_current("ay")).unwrap(),
        ay18,
        "PRECONDITION: the authority link still names ay@18, so it IS live"
    );

    // 3. A SIBLING member fails to stage → the group aborts after ay re-staged fine.
    std::fs::write(fake.archives.get("trust-4821.tar.zst").unwrap(), b"corrupt").unwrap();
    let report = apply_channel(
        &fake,
        &layout,
        &anchor(),
        "stable",
        TRIPLE,
        &std::collections::BTreeMap::from([("trust".to_string(), 4820u64)]),
        &[],
        fl(0),
        0,
    )
    .unwrap();
    assert!(
        matches!(
            report.groups[0].1,
            TxnOutcome::Aborted {
                during_flip: false,
                ..
            }
        ),
        "PRECONDITION: the group aborted in the stage phase: {:?}",
        report.groups[0].1
    );

    // The casualty test: the failing member is `trust`; `ay` must survive intact.
    assert!(
        ay18.is_dir(),
        "the abort discarded a build that was already LIVE — the toolchain is gone"
    );
    assert!(
        crate::store::build_is_complete(&ay18),
        "the abort took down the completeness marker of a LIVE build"
    );
    assert_eq!(
        std::fs::read_link(layout.program_current("ay")).unwrap(),
        ay18,
        "the authority link must still resolve"
    );
    assert!(
        crate::ops::list_installed(&layout)
            .iter()
            .any(|(p, b)| p == "ay" && *b == 18),
        "ay@18 must still read as installed after a sibling's failure"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// The same repair inside the GROUP transaction, which decides per member from its own
/// `decide` call: a tuple member whose store tree this slice cannot vouch for must be
/// re-staged, not held at UpToDate beside its healthy sibling. `update` routes every
/// coherence-group member (trust, ay, …) through here, so without this the Rosetta
/// store and the marker-less live tree were permanent for exactly the programs the
/// platform record was written for.
#[test]
fn a_group_member_this_slice_cannot_vouch_for_is_re_staged() {
    let dir = scratch("group-otherslice");
    let fake = group_fixture(&dir);
    let layout = layout(&dir);

    // 1. Really install the tuple: ay@18 + trust@4821 go live.
    apply_channel(
        &fake,
        &layout,
        &anchor(),
        "stable",
        TRIPLE,
        &std::collections::BTreeMap::from([("ay".to_string(), 17u64)]),
        &[],
        fl(0),
        0,
    )
    .unwrap();
    let ay18 = layout.build_dir("ay", 18);
    assert!(
        crate::store::build_is_complete(&ay18),
        "PRECONDITION: ay@18 is installed and complete"
    );

    // 2. ay@18 loses the marker this slice reads — the other slice's `platform=` record,
    //    or a restore/interrupted-swap recovery that left the tree unmarked. The shims
    //    still resolve into it, so the shim view keeps reporting 18.
    crate::store::clear_build_ready(&ay18).unwrap();
    let live = crate::ops::active_builds(&layout);
    assert_eq!(
        live.get("ay").copied(),
        Some(18),
        "PRECONDITION: the shim view the update lane feeds `decide` still says 18"
    );
    assert_eq!(live.get("trust").copied(), Some(4821));

    // 3. The next pass, fed that map exactly as `cmd_update_all` feeds it.
    let report = apply_channel(
        &fake,
        &layout,
        &anchor(),
        "stable",
        TRIPLE,
        &live,
        &[],
        fl(0),
        0,
    )
    .unwrap();
    assert_eq!(report.groups.len(), 1, "one coherence group");
    assert_eq!(
        report.groups[0].1,
        TxnOutcome::Applied(vec!["ay".into()]),
        "the tuple must re-stage the member this slice cannot vouch for (its healthy \
         sibling stays put)"
    );
    assert!(
        crate::store::build_is_complete(&ay18),
        "the re-stage leaves a marker THIS slice accepts"
    );
    assert_eq!(
        crate::ops::which(&layout, "ay").unwrap(),
        tool_bin(&ay18, "ay"),
        "and the tool still resolves into the re-staged tree"
    );

    // A healthy tuple is still a no-op — the completeness input never re-downloads a
    // store that is sound.
    let report = apply_channel(
        &fake,
        &layout,
        &anchor(),
        "stable",
        TRIPLE,
        &crate::ops::active_builds(&layout),
        &[],
        fl(0),
        0,
    )
    .unwrap();
    assert_eq!(report.groups[0].1, TxnOutcome::UpToDate);
    let _ = std::fs::remove_dir_all(&dir);
}

// The missing-triple hold at the flow layer: an installed tuple (ay@17 + trust@4820)
// whose new pin trust@4821 publishes no artifact for this triple is held whole on its
// current builds — never staged, never aborted, nothing resolved for download.
#[test]
fn an_installed_group_whose_new_pin_lacks_this_triple_is_held_whole() {
    let dir = scratch("group-unpublished");
    let fake = group_fixture_with(&dir, &["trust"]);
    let layout = layout(&dir);
    seed_build(&layout, "ay", 17, true);
    seed_build(&layout, "trust", 4820, true);
    let installed = std::collections::BTreeMap::from([
        ("ay".to_string(), 17u64),
        ("trust".to_string(), 4820u64),
    ]);
    let report = apply_channel(
        &fake,
        &layout,
        &anchor(),
        "stable",
        TRIPLE,
        &installed,
        &[],
        fl(0),
        0,
    )
    .unwrap();
    assert_eq!(
        report.groups[0].1,
        TxnOutcome::Unpublished {
            member: "trust".into(),
            build: 4821,
            triple: TRIPLE.into(),
        },
        "an unpublished pin holds the tuple, it does not abort it"
    );
    assert_eq!(
        crate::ops::which(&layout, "ay").unwrap(),
        tool_bin(&layout.build_dir("ay", 17), "ay")
    );
    assert_eq!(
        crate::ops::which(&layout, "trust").unwrap(),
        tool_bin(&layout.build_dir("trust", 4820), "trust")
    );
    for (p, b) in [("ay", 18u64), ("trust", 4821u64)] {
        assert!(!layout.build_dir(p, b).exists(), "{p}@{b} was staged");
        assert!(!layout.staging_dir(p).exists(), "{p} touched staging/");
    }
    assert!(
        report.resolved_assets.is_empty() && report.applied.is_empty(),
        "nothing resolved, nothing applied: {:?} {:?}",
        report.resolved_assets,
        report.applied
    );
    let _ = std::fs::remove_dir_all(&dir);
}

// The hold reads only the members the pass would move. ay is up to date on build 18,
// whose signed manifest has no row for this triple; trust moves 4820 → 4821, which is
// published here. Nothing of ay's is staged, so its manifest is never even fetched.
#[test]
fn an_up_to_date_member_whose_pin_lacks_this_triple_never_holds_its_sibling() {
    let dir = scratch("group-uptodate-unserved");
    let fake = group_fixture_with(&dir, &["ay"]);
    let layout = layout(&dir);
    seed_build(&layout, "ay", 18, true);
    seed_build(&layout, "trust", 4820, true);
    let installed = std::collections::BTreeMap::from([
        ("ay".to_string(), 18u64),
        ("trust".to_string(), 4820u64),
    ]);
    let report = apply_channel(
        &fake,
        &layout,
        &anchor(),
        "stable",
        TRIPLE,
        &installed,
        &[],
        fl(0),
        0,
    )
    .unwrap();
    assert_eq!(
        report.groups[0].1,
        TxnOutcome::Applied(vec!["trust".into()]),
        "an up-to-date member's manifest never holds the sibling that moves"
    );
    assert_eq!(
        crate::ops::which(&layout, "trust").unwrap(),
        tool_bin(&layout.build_dir("trust", 4821), "trust")
    );
    assert_eq!(
        crate::ops::which(&layout, "ay").unwrap(),
        tool_bin(&layout.build_dir("ay", 18), "ay")
    );
    let _ = std::fs::remove_dir_all(&dir);
}

// --- rollback + local pin + apply_program (steps 9/10/11) --------------------------

/// Lay down a COMPLETE build dir with `bin/<program>`; `activate` also shims + activates
/// it (making it the ACTIVE build).
fn seed_build(layout: &Layout, program: &str, build: u64, activate: bool) {
    let dir = layout.build_dir(program, build);
    std::fs::create_dir_all(dir.join("bin")).unwrap();
    std::fs::write(
        dir.join("bin").join(tool(program).exe_file()),
        b"#!/bin/true\n",
    )
    .unwrap();
    if activate {
        crate::activate::install_shims(
            layout,
            &dir,
            &[program.to_string()],
            crate::activate::Aliases::Off,
        )
        .unwrap();
        activate_build(layout, &dir).unwrap();
    }
    crate::store::mark_build_ready(&dir).unwrap();
}

/// An index-only Fake (empty pkg/archives — rollback never downloads) pinning `ay` with a
/// configurable `min_build` + `yanked` list, so the floor/yank gate is exercised.
fn rollback_index(min_build: u64, yanked: &[&str]) -> Fake {
    let yanked_toml = if yanked.is_empty() {
        String::new()
    } else {
        let items: Vec<String> = yanked.iter().map(|y| format!("\"{y}\"")).collect();
        format!("yanked = [{}]\n", items.join(", "))
    };
    let index_body = format!(
        "schema = 2\nindex_build = 41\nvalid_until = \"2026-07-05T12:00:00Z\"\n{attr}\
                      [programs.ay]\nrepo = \"ay\"\n\
         [[channels]]\nname = \"stable\"\nchannel_build = 1\nmin_build = {min_build}\n\
         {yanked_toml}pin = {{ ay = 18 }}\n",
        attr = attribution()
    );
    Fake {
        index: index_body.clone().into_bytes(),
        index_sig: sign(&RELEASE_SEED, index_body.as_bytes()),
        pkg: HashMap::new(),
        archives: HashMap::new(),
    }
}

/// [`rollback_index`] with the floor published PER PROGRAM (`min_build_by_program`), the
/// channel-wide floor left at 0 — the only shape that can floor one program of a channel
/// whose members have independent build counters without refusing the others' builds.
fn rollback_index_per_program(floor_ay: u64) -> Fake {
    let index_body = format!(
        "schema = 2\nindex_build = 41\nvalid_until = \"2026-07-05T12:00:00Z\"\n{attr}\
                      [programs.ay]\nrepo = \"ay\"\n\
         [[channels]]\nname = \"stable\"\nchannel_build = 1\nmin_build = 0\n\
         min_build_by_program = {{ ay = {floor_ay} }}\npin = {{ ay = 18 }}\n",
        attr = attribution()
    );
    Fake {
        index: index_body.clone().into_bytes(),
        index_sig: sign(&RELEASE_SEED, index_body.as_bytes()),
        pkg: HashMap::new(),
        archives: HashMap::new(),
    }
}

/// EXHAUSTIVE CONFORMANCE FOR THE REMOVED-MEMBER HOLD: what an unattended update does
/// to a coherence tuple when the user has deliberately uninstalled part of it. The
/// state space is small but too big to hold in your head: per member, {absent, safe,
/// revoked} × {recorded-removed or not}, against {clean, installed-build-yanked,
/// pin-yanked} — so it is enumerated instead of argued about. The assertions below are the
/// INVARIANTS, not the expected outputs of any particular branch — they are what
/// must hold whatever the implementation decides:
///
///   I1. A member that is recorded-removed AND absent is never installed. That is
///       the whole point of the record.
///   I2. A revoked installed build is never left RUNNABLE. It is either upgraded to
///       the valid pin or its tools are tombstoned — never silently kept.
///   I3. The pass never claims `UpToDate` while some installed member is revoked.
///
/// Each channel — clean, the INSTALLED build (`ay@17`) yanked, the PIN (`ay@18`) yanked —
/// enumerates its own 72 cases over ONE signed fixture: the release (two real archives,
/// their extracted tree roots, the signed manifests and the channel's signed index) is
/// built once per channel and only READ by the cases — `Fake::download` copies an
/// archive out, it never moves one — while every case gets a store of its own. Each
/// channel must decide at least 24 of its 72 cases.
#[test]
fn the_removed_member_hold_is_exhaustively_safe() {
    for (cname, make) in [
        ("clean", group_fixture as fn(&Path) -> Fake),
        ("yank-installed", group_fixture_yanking_ay17),
        ("yank-pin", group_fixture_yanking_ay18),
    ] {
        let checked = removed_member_hold_cases(cname, make);
        assert!(
            checked >= 24,
            "{cname}: the enumeration ran only {checked} cases"
        );
    }
}

/// Every case of the removed-member hold on ONE channel; returns how many cases
/// reached a report (a resolve/verify failure is not this predicate's business).
fn removed_member_hold_cases(cname: &str, make: fn(&Path) -> Fake) -> usize {
    #[derive(Clone, Copy, Debug)]
    enum Ay {
        Absent,
        Safe,
        Revoked,
    }
    let fixture_dir = scratch(&format!("hold-fixture-{cname}"));
    let fake = make(&fixture_dir);
    let mut checked = 0;
    for ay in [Ay::Absent, Ay::Safe, Ay::Revoked] {
        for trust_installed in [false, true] {
            for removed in [&[][..], &["ay"][..], &["trust"][..], &["ay", "trust"][..]] {
                // `[packages].exclude` rides the same deliberately-absent
                // predicate as the removal record, so it gets the same
                // exhaustive treatment — including the case where both
                // name the same member, and where an exclusion names a
                // PRESENT member (which must change nothing: exclude is
                // "do not pull in", never "drop what is here").
                for excluded in [&[][..], &["ay"][..], &["trust"][..]] {
                    let excluded: Vec<String> = excluded.iter().map(|s| s.to_string()).collect();
                    let label = format!(
                        "{cname}-{ay:?}-t{trust_installed}-r{}-x{}",
                        removed.len(),
                        excluded.len()
                    );
                    let dir = scratch(&format!("hold-{label}"));
                    let layout = layout(&dir);
                    let mut installed = std::collections::BTreeMap::new();
                    match ay {
                        Ay::Absent => {}
                        Ay::Safe => {
                            seed_build(&layout, "ay", 18, true);
                            installed.insert("ay".to_string(), 18u64);
                        }
                        Ay::Revoked => {
                            seed_build(&layout, "ay", 17, true);
                            installed.insert("ay".to_string(), 17u64);
                        }
                    }
                    if trust_installed {
                        seed_build(&layout, "trust", 4821, true);
                        installed.insert("trust".to_string(), 4821u64);
                    }
                    if !removed.is_empty() {
                        // The prefix exists only once something has been seeded, and
                        // the absent/absent cases seed nothing.
                        std::fs::create_dir_all(&layout.prefix).unwrap();
                        std::fs::write(layout.removed(), removed.join("\n")).unwrap();
                    }

                    let report = apply_channel(
                        &fake,
                        &layout,
                        &anchor(),
                        "stable",
                        TRIPLE,
                        &installed,
                        &excluded,
                        fl(0),
                        0,
                    );
                    let Ok(report) = report else {
                        // A resolve/verify failure is not this predicate's business.
                        continue;
                    };
                    checked += 1;
                    let after = crate::ops::active_builds(&layout);

                    // I1: a recorded-removed, ABSENT member is never installed.
                    for program in removed {
                        let was_absent = !installed.contains_key(*program);
                        if was_absent {
                            assert!(
                                !after.contains_key(*program),
                                "{label}: {program} was deleted on purpose and came back"
                            );
                        }
                    }

                    // I1x: an EXCLUDED, absent member is never pulled in — the
                    // promise `uninstall` makes when it names `[packages].exclude`
                    // as the way to drop one program and stay adopted (the
                    // coherence pull-in would reinstall it, multi-GB, unannounced).
                    for program in &excluded {
                        if !installed.contains_key(program) {
                            assert!(
                                !after.contains_key(program),
                                "{label}: excluded {program} was pulled back in"
                            );
                        }
                    }

                    // I1x-present: an exclusion naming a PRESENT member is not
                    // license to drop or freeze it — asserted where the channel
                    // gives survival a right answer. In `clean` nothing
                    // legitimately kills a present member; in `yank-installed`
                    // the excluded-but-present member must still UPGRADE to the
                    // valid pin (exclusion never freezes). `yank-pin` proves
                    // nothing here: it tombstones ay@18 with or without the
                    // exclusion, and this invariant's first draft asserting
                    // survival there was refuted by its own enumeration.
                    if cname == "clean" {
                        for program in &excluded {
                            if installed.contains_key(program) {
                                assert!(
                                    after.contains_key(program),
                                    "{label}: excluding present {program} dropped it"
                                );
                            }
                        }
                    }
                    if cname == "yank-installed"
                        && matches!(ay, Ay::Revoked)
                        && excluded.iter().any(|p| p == "ay")
                    {
                        assert_eq!(
                            after.get("ay").copied(),
                            Some(18),
                            "{label}: excluding present ay froze its upgrade"
                        );
                    }

                    // I2: a revoked installed build is never left runnable.
                    if matches!(ay, Ay::Revoked) && cname != "clean" {
                        let live = after.get("ay").copied();
                        let runnable = crate::ops::which(&layout, "ay").is_some();
                        let safe = live.is_some_and(|b| b == 18);
                        assert!(
                            safe || !runnable,
                            "{label}: revoked ay is still runnable at {live:?}"
                        );
                    }

                    // I4: A MEMBER WITH A VALID REPLACEMENT IS NEVER LEFT DEAD.
                    // I2 alone calls tombstoning "safe", and it is — but it is the
                    // WRONG safe answer when the channel is offering a fix, and
                    // that is exactly the shape that shipped: a routine yank
                    // disabled a program that had a working upgrade waiting, and
                    // the corpse then vanished from `active_builds` so no later
                    // pass could see or repair it. An invariant set that only asks
                    // "is anything unsafe running" cannot see this; it has to ask
                    // "did anything that should live, die".
                    if cname == "yank-installed"
                        && matches!(ay, Ay::Revoked)
                        && !(removed.contains(&"ay") && !installed.contains_key("ay"))
                    {
                        assert!(
                            crate::ops::which(&layout, "ay").is_some(),
                            "{label}: ay had a valid pin (18) and was killed instead \
                         of upgraded"
                        );
                    }

                    // I5: THE ORDINARY TICK STILL MOVES. The case this predicate
                    // meets on almost every run is "nothing is revoked", and freezing
                    // the members that ARE here — silently, forever — costs a lab
                    // machine its updates: a publisher usually ships a fix as a NEW
                    // PIN without yanking the old build.
                    if cname == "clean" && matches!(ay, Ay::Safe) {
                        // ay@18 is already the pin, so the assertion that matters is
                        // that the pass did not DROP it while excluding a sibling.
                        assert_eq!(
                            after.get("ay").copied(),
                            Some(18),
                            "{label}: a present member was lost while holding for an \
                         absent one"
                        );
                    }

                    // I6: THE HOLD ONLY FIRES ON A REAL ABSENCE. A stale record for
                    // a member that a signed `requires` pull-in has since
                    // reinstalled must not hold anything: it is present, and the
                    // promise the record encodes ("do not put it back") is already
                    // kept.
                    let stale_only =
                        !removed.is_empty() && removed.iter().all(|m| installed.contains_key(*m));
                    if stale_only && cname == "yank-installed" && matches!(ay, Ay::Revoked) {
                        assert!(
                            !crate::ops::which(&layout, "ay").is_some()
                                || after.get("ay").copied() == Some(18),
                            "{label}: a stale removal record froze a healthy tuple"
                        );
                    }

                    // I3: never "up to date" over a revocation.
                    if matches!(ay, Ay::Revoked) && cname != "clean" {
                        for (_, outcome) in &report.groups {
                            assert!(
                                !matches!(outcome, TxnOutcome::UpToDate),
                                "{label}: reported UpToDate over a revoked build"
                            );
                        }
                    }
                    let _ = std::fs::remove_dir_all(&dir);
                }
            }
        }
    }
    let _ = std::fs::remove_dir_all(&fixture_dir);
    eprintln!("removed-member hold, channel {cname}: {checked} of 72 cases decided");
    checked
}

/// [`group_fixture`] but whose stable channel YANKS `ay@18`, so `decide` tombstones the
/// group (to prove a pin never suppresses a tombstone).
fn group_fixture_yanking_ay18(dir: &Path) -> Fake {
    let mut f = group_fixture(dir);
    let index_body = format!(
        "schema = 2\nindex_build = 41\nvalid_until = \"2026-07-05T12:00:00Z\"\n{attr}\
                      [programs.trust]\nrepo = \"trust\"\ncoherence_group = \"rustc\"\n\
         [programs.ay]\nrepo = \"ay\"\ncoherence_group = \"rustc\"\n\
         [[channels]]\nname = \"stable\"\nchannel_build = 1\nmin_build = 0\n\
         yanked = [\"ay@18\"]\n\
         pin = {{ trust = 4821, ay = 18 }}\n",
        attr = attribution()
    );
    f.index = index_body.clone().into_bytes();
    f.index_sig = sign(&RELEASE_SEED, index_body.as_bytes());
    f
}

// THE pin-authority test: a local pin freezes the WHOLE group on its current builds; the
// upgrade is suppressed, nothing is staged, and the sibling is not pulled in.
#[test]
fn a_local_pin_holds_a_whole_group_and_never_moves_it() {
    let dir = scratch("pin-holds");
    let fake = group_fixture(&dir);
    let layout = layout(&dir);
    seed_build(&layout, "ay", 17, true); // ay@17 active
    crate::pin::set_pinned(&layout, "ay", true).unwrap();
    let installed = std::collections::BTreeMap::from([("ay".to_string(), 17u64)]);
    let report = apply_channel(
        &fake,
        &layout,
        &anchor(),
        "stable",
        TRIPLE,
        &installed,
        &[],
        fl(0),
        0,
    )
    .unwrap();
    assert_eq!(report.groups.len(), 1);
    assert_eq!(report.groups[0].1, TxnOutcome::Pinned(vec!["ay".into()]));
    assert_eq!(
        crate::ops::active_builds(&layout).get("ay").copied(),
        Some(17),
        "ay held at 17"
    );
    assert!(!layout.build_dir("ay", 18).exists(), "no upgrade staged");
    assert!(
        !layout.build_dir("trust", 4821).exists(),
        "sibling not pulled in"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// Fixture: the pin (ay=18) is VALID, but the currently-installed build ay@17 is YANKED.
/// decide() returns Install (force-upgrade off 17), NOT Tombstone.
fn group_fixture_yanking_ay17(dir: &Path) -> Fake {
    let mut f = group_fixture(dir);
    let index_body = format!(
        "schema = 2\nindex_build = 41\nvalid_until = \"2026-07-05T12:00:00Z\"\n{attr}\
                      [programs.trust]\nrepo = \"trust\"\ncoherence_group = \"rustc\"\n\
         [programs.ay]\nrepo = \"ay\"\ncoherence_group = \"rustc\"\n\
         [[channels]]\nname = \"stable\"\nchannel_build = 1\nmin_build = 0\n\
         yanked = [\"ay@17\"]\n\
         pin = {{ trust = 4821, ay = 18 }}\n",
        attr = attribution()
    );
    f.index = index_body.clone().into_bytes();
    f.index_sig = sign(&RELEASE_SEED, index_body.as_bytes());
    f
}

// THE critical-fix regression: a local pin must NOT keep a YANKED current build running.
// ay@17 is active + pinned, but 17 is yanked and the pin (18) is valid — the pin is IGNORED
// and the tuple force-upgrades off the revoked build, rather than freezing on it.
#[test]
fn a_local_pin_does_not_hold_a_yanked_current_build() {
    let dir = scratch("pin-yanked-current");
    let fake = group_fixture_yanking_ay17(&dir);
    let layout = layout(&dir);
    seed_build(&layout, "ay", 17, true); // ay@17 active (now yanked)
    crate::pin::set_pinned(&layout, "ay", true).unwrap();
    let installed = std::collections::BTreeMap::from([("ay".to_string(), 17u64)]);
    let report = apply_channel(
        &fake,
        &layout,
        &anchor(),
        "stable",
        TRIPLE,
        &installed,
        &[],
        fl(0),
        0,
    )
    .unwrap();
    assert!(
        !matches!(report.groups[0].1, TxnOutcome::Pinned(_)),
        "pin must be ignored when the current build is yanked: {:?}",
        report.groups[0].1
    );
    assert_eq!(
        crate::ops::active_builds(&layout).get("ay").copied(),
        Some(18),
        "force-upgraded off the yanked 17 to the valid pin 18"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

// A pin NEVER suppresses a tombstone: a yanked pin tombstones the group even when pinned.
#[test]
fn a_pin_never_suppresses_a_tombstone() {
    let dir = scratch("pin-tombstone");
    let fake = group_fixture_yanking_ay18(&dir);
    let layout = layout(&dir);
    seed_build(&layout, "ay", 17, true);
    crate::pin::set_pinned(&layout, "ay", true).unwrap();
    let installed = std::collections::BTreeMap::from([("ay".to_string(), 17u64)]);
    let report = apply_channel(
        &fake,
        &layout,
        &anchor(),
        "stable",
        TRIPLE,
        &installed,
        &[],
        fl(0),
        0,
    )
    .unwrap();
    assert!(
        matches!(report.groups[0].1, TxnOutcome::Tombstoned(_)),
        "pin is ignored when the gate tombstones: {:?}",
        report.groups[0].1
    );
    // The tombstoned member's OLD working shim is actively DISABLED, not merely reported.
    assert!(
        crate::platform::resolve_shim(&shim_of(&layout, "ay")).is_none(),
        "tombstone no longer forwards anywhere"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// [`fixture`] — the SINGLETON `ay` (no `coherence_group`) — with its own pin YANKED,
/// so `decide` tombstones it and the pass disables its live shims for real.
fn fixture_yanking_the_pin(dir: &Path) -> Fake {
    let mut f = fixture(dir);
    let index_body = format!(
        "schema = 2\nindex_build = 41\nvalid_until = \"2026-07-05T12:00:00Z\"\n{attr}\
         [programs.ay]\nrepo = \"ay\"\n\
         [[channels]]\nname = \"stable\"\nchannel_build = 1\nmin_build = 0\n\
         yanked = [\"ay@18\"]\n\
         pin = {{ ay = 18 }}\n",
        attr = attribution()
    );
    f.index = index_body.clone().into_bytes();
    f.index_sig = sign(&RELEASE_SEED, index_body.as_bytes());
    f
}

// THE TOMBSTONE'S OWN INSTRUCTION HAS TO WORK. The failing shim says "run `aterm pkg
// update`", and for a tombstoned SINGLETON (or a tuple every member of which was
// tombstoned) the plain pass must re-stage it although `installed` is the shim view and
// a tombstone resolves nowhere. On a machine that installed one program by name (never
// adopted the set) the set-completion lane cannot stand in.
#[test]
fn a_plain_update_revives_a_tombstoned_singleton_when_its_pin_is_good_again() {
    let dir = scratch("tombstone-singleton-revive");
    let layout = layout(&dir);
    // 1. ay@18 is live, and the publisher yanks the pin: the pass tombstones it.
    seed_build(&layout, "ay", 18, true);
    let installed = std::collections::BTreeMap::from([("ay".to_string(), 18u64)]);
    let report = apply_channel(
        &fixture_yanking_the_pin(&dir),
        &layout,
        &anchor(),
        "stable",
        TRIPLE,
        &installed,
        &[],
        fl(0),
        0,
    )
    .unwrap();
    assert!(
        matches!(report.groups[0].1, TxnOutcome::Tombstoned(_)),
        "the yanked pin tombstones the singleton: {:?}",
        report.groups[0].1
    );
    let shim = shim_of(&layout, "ay");
    assert!(
        crate::platform::resolve_shim(&shim).is_none(),
        "bin/ay is now the failing tombstone that says to run `aterm pkg update`"
    );
    // PRECONDITION: the shim view `cmd_update_all_code` feeds this lane is SILENT for
    // ay.
    let live = crate::ops::active_builds(&layout);
    assert!(
        !live.contains_key("ay"),
        "a tombstone resolves nowhere, so the shim view does not name ay"
    );
    // 2. The publisher re-points the pin at a good build. A PLAIN `update` pass — fed
    //    that silent map, exactly as the CLI feeds it — must re-stage the singleton.
    let report = apply_channel(
        &fixture(&dir),
        &layout,
        &anchor(),
        "stable",
        TRIPLE,
        &live,
        &[],
        fl(0),
        0,
    )
    .unwrap();
    assert_eq!(
        report.groups.len(),
        1,
        "the tombstoned singleton is still a group this machine HAS: {:?}",
        report.groups
    );
    assert_eq!(
        report.groups[0].1,
        TxnOutcome::Applied(vec!["ay".into()]),
        "decide sees None (the silence IS the self-heal) and returns Install: {:?}",
        report.groups[0].1
    );
    assert_eq!(
        crate::ops::which(&layout, "ay").unwrap(),
        tool_bin(&layout.build_dir("ay", 18), "ay"),
        "the tombstone is replaced by a working shim — ay runs again"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// Rollback lands on the highest retained build below the active one that the gate
/// still admits: never a yanked build, never one below the floor — the channel's, or the
/// floor published for THIS program (`min_build_by_program`), never another's — and with
/// none left it refuses. Landed, the shim, the witness and the `alab-` alias the verified
/// index calls for all name the target.
#[test]
fn rollback_selects_the_highest_gate_valid_build_below_current() {
    for (label, fake, want) in [
        ("plain", rollback_index(0, &[]), Some(17)),
        ("17 yanked", rollback_index(0, &["ay@17"]), Some(16)),
        ("channel floor 18", rollback_index(18, &[]), None),
        ("ay's own floor 18", rollback_index_per_program(18), None),
        (
            "ay's own floor 17",
            rollback_index_per_program(17),
            Some(17),
        ),
    ] {
        let dir = scratch(&format!("rollback-{}", label.replace(' ', "-")));
        let layout = layout(&dir);
        seed_build(&layout, "ay", 16, false);
        seed_build(&layout, "ay", 17, false);
        seed_build(&layout, "ay", 18, true);
        let got = rollback(&fake, &layout, &anchor(), "stable", "ay", &[], fl(0), 0);
        let Some(to) = want else {
            assert!(
                matches!(got, Err(FlowError::Rollback(_))),
                "{label}: {got:?}"
            );
            continue;
        };
        let r = got.unwrap_or_else(|e| panic!("{label}: {e}"));
        assert_eq!((r.from_build, r.to_build), (18, to), "{label}");
        let target = layout.build_dir("ay", to);
        assert_eq!(
            crate::ops::which(&layout, "ay"),
            Some(tool_bin(&target, "ay"))
        );
        assert_eq!(
            std::fs::read_link(layout.program_current("ay")).unwrap(),
            target
        );
        assert_eq!(
            crate::ops::which(&layout, "alab-ay"),
            crate::ops::which(&layout, "ay"),
            "{label}: the alias is restored with its primary"
        );
    }
}

/// A GUTTED ROLLBACK TARGET: a build with orphaned `lib/`, no `bin/`, and a `.ready`
/// marker still saying `ok`. Rolled back to, every shim would be removed and the
/// machine left with no compiler. A build on disk may carry a marker written by a
/// version that recorded nothing, so selection asks the tree directly. The fixture
/// forges exactly that: a LEGACY marker (a bare `ok\n`, which every older atpkg wrote)
/// over a gutted tree.
#[test]
fn rollback_refuses_a_target_whose_tools_are_gone_and_takes_the_one_below() {
    let dir = scratch("rollback-gutted");
    let layout = layout(&dir);
    let fake = rollback_index(0, &[]);
    seed_build(&layout, "ay", 16, false);
    seed_build(&layout, "ay", 17, false);
    seed_build(&layout, "ay", 18, true); // active
    // Gut 17 the way an interrupted removal does, and re-mark it the way every atpkg
    // before the contents record did: a bare `ok`, vouching for a tree with no `bin/`.
    std::fs::remove_dir_all(layout.build_dir("ay", 17).join("bin")).unwrap();
    std::fs::write(
        layout.build_dir("ay", 17).with_file_name("17.ready"),
        b"ok\n",
    )
    .unwrap();
    assert!(
        crate::ops::list_installed(&layout).contains(&("ay".to_string(), 17)),
        "the fixture must leave 17 looking installed, or it proves nothing"
    );

    let r = rollback(&fake, &layout, &anchor(), "stable", "ay", &[], fl(0), 0).unwrap();
    assert_eq!(
        r.to_build, 16,
        "the gutted build is skipped and the rollback lands on one that can run"
    );
    assert_eq!(
        crate::ops::which(&layout, "ay").unwrap(),
        tool_bin(&layout.build_dir("ay", 16), "ay"),
        "and the shim points at a binary that is actually there"
    );

    // …and when the gutted build is the ONLY candidate, the verb REFUSES rather than
    // disarming the machine. Nothing may be removed on the way to that refusal.
    std::fs::remove_dir_all(layout.build_dir("ay", 16).join("bin")).unwrap();
    std::fs::write(
        layout.build_dir("ay", 16).with_file_name("16.ready"),
        b"ok\n",
    )
    .unwrap();
    crate::activate::activate_build(&layout, &layout.build_dir("ay", 18)).unwrap();
    crate::activate::install_shims(
        &layout,
        &layout.build_dir("ay", 18),
        &["ay".to_string()],
        crate::activate::Aliases::Off,
    )
    .unwrap();
    rollback(&fake, &layout, &anchor(), "stable", "ay", &[], fl(0), 0)
        .expect_err("no landable build below current");
    assert_eq!(
        crate::ops::which(&layout, "ay").unwrap(),
        tool_bin(&layout.build_dir("ay", 18), "ay"),
        "a refused rollback leaves the live shim exactly where it was"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// THE PINS THAT HOLD A ROLLED-BACK PROGRAM where it lands, as the update pass's pin
/// gate reads them: its own when ungrouped, any member's of its coherence group, never an
/// uninstalled or excluded sibling's (the pass drops that member unread), none while a
/// member's current build is revoked (the pass moves the tuple whatever is pinned), and
/// none when nothing is pinned. Each case then runs that pass: it holds the group on
/// exactly the reported pins, or not at all.
#[test]
fn rollback_reports_the_pins_that_hold_it() {
    type Next = (Vec<String>, Option<(String, u64)>);
    fn next(
        label: &str,
        fixture: fn(&Path) -> Fake,
        trust: Option<u64>,
        pinned: &[&str],
        removed: &[&str],
        excluded: &[&str],
    ) -> Next {
        let dir = scratch(label);
        let lay = layout(&dir);
        let fake = fixture(&dir);
        seed_build(&lay, "ay", 17, false);
        seed_build(&lay, "ay", 18, true);
        if let Some(build) = trust {
            seed_build(&lay, "trust", build, true);
        }
        for p in pinned {
            crate::pin::set_pinned(&lay, p, true).unwrap();
        }
        if !removed.is_empty() {
            std::fs::write(lay.removed(), removed.join("\n")).unwrap();
        }
        let excluded: Vec<String> = excluded.iter().map(ToString::to_string).collect();
        let r = rollback(&fake, &lay, &anchor(), "stable", "ay", &excluded, fl(0), 0).unwrap();
        assert_eq!(r.to_build, 17);
        let pass = apply_channel(
            &fake,
            &lay,
            &anchor(),
            "stable",
            TRIPLE,
            &crate::ops::active_builds(&lay),
            &excluded,
            fl(0),
            0,
        );
        let pass_held = pass
            .ok()
            .into_iter()
            .flat_map(|report| report.groups)
            .find_map(|(_, outcome)| match outcome {
                TxnOutcome::Pinned(held) => Some(held),
                _ => None,
            })
            .unwrap_or_default();
        assert_eq!(pass_held, r.held_by, "{label}: the update pass disagrees");
        let _ = std::fs::remove_dir_all(&dir);
        (r.held_by, r.revoked_member)
    }
    fn ungrouped(_: &Path) -> Fake {
        rollback_index(0, &[])
    }
    fn yanking_trust_4820(dir: &Path) -> Fake {
        let mut f = group_fixture(dir);
        let index_body = format!(
            "schema = 2\nindex_build = 41\nvalid_until = \"2026-07-05T12:00:00Z\"\n{attr}\
             [programs.trust]\nrepo = \"trust\"\ncoherence_group = \"rustc\"\n\
             [programs.ay]\nrepo = \"ay\"\ncoherence_group = \"rustc\"\n\
             [[channels]]\nname = \"stable\"\nchannel_build = 1\nmin_build = 0\n\
             yanked = [\"trust@4820\"]\n\
             pin = {{ trust = 4821, ay = 18 }}\n",
            attr = attribution()
        );
        f.index = index_body.clone().into_bytes();
        f.index_sig = sign(&RELEASE_SEED, index_body.as_bytes());
        f
    }
    let held = |v: &[&str]| -> Next { (v.iter().map(ToString::to_string).collect(), None) };
    assert_eq!(
        next("rb-held-none", ungrouped, None, &[], &[], &[]),
        held(&[])
    );
    assert_eq!(
        next("rb-held-self", ungrouped, None, &["ay"], &[], &[]),
        held(&["ay"])
    );
    assert_eq!(
        next("rb-held-other", ungrouped, None, &["trust"], &[], &[]),
        held(&[]),
        "an ungrouped program is held by its own pin alone"
    );
    assert_eq!(
        next(
            "rb-held-group-none",
            group_fixture,
            Some(4821),
            &[],
            &[],
            &[]
        ),
        held(&[])
    );
    assert_eq!(
        next(
            "rb-held-sibling",
            group_fixture,
            Some(4821),
            &["trust"],
            &[],
            &[]
        ),
        held(&["trust"]),
        "one pinned member holds the whole tuple"
    );
    assert_eq!(
        next(
            "rb-held-both",
            group_fixture,
            Some(4821),
            &["ay", "trust"],
            &[],
            &[]
        ),
        held(&["ay", "trust"])
    );
    // A sibling uninstalled (or excluded) with its pin left behind: the pass drops the
    // member before its pin gate, so that pin holds nothing.
    assert_eq!(
        next(
            "rb-held-removed",
            group_fixture,
            None,
            &["trust"],
            &["trust"],
            &[]
        ),
        held(&[])
    );
    assert_eq!(
        next(
            "rb-held-excluded",
            group_fixture,
            None,
            &["trust"],
            &[],
            &["trust"]
        ),
        held(&[])
    );
    // A pinned sibling whose current build is revoked: the pass moves the whole tuple.
    assert_eq!(
        next(
            "rb-held-revoked",
            yanking_trust_4820,
            Some(4820),
            &["trust"],
            &[],
            &[]
        ),
        (Vec::new(), Some(("trust".to_string(), 4820)))
    );
}

// --- requires dependency pull-in (step 17) -----------------------------------------

/// A signed release naming `ay`@18 + `ny`@9 (NO coherence group), with configurable
/// per-program `requires` and channel `yanked`.
fn requires_fixture(
    dir: &Path,
    yanked: &[&str],
    ay_requires: &[&str],
    ny_requires: &[&str],
) -> Fake {
    fn req_line(reqs: &[&str]) -> String {
        if reqs.is_empty() {
            String::new()
        } else {
            let items: Vec<String> = reqs.iter().map(|r| format!("\"{r}\"")).collect();
            format!("requires = [{}]\n", items.join(", "))
        }
    }
    let mut pkg = HashMap::new();
    let mut archives = HashMap::new();
    for (program, build, reqs) in [("ay", 18u64, ay_requires), ("ny", 9u64, ny_requires)] {
        let archive = prog_archive(dir, program, build);
        let sha = crate::tree::file_sha256(&archive).unwrap();
        let root = signed_root_of(&archive, &dir.join(format!("probe-{program}")));
        let asset = format!("{program}-{build}.tar.zst");
        let pkg_body = format!(
            "schema = 2\nprogram = \"{program}\"\nversion = \"0.1\"\nbuild_number = {build}\n\
             exposes = [\"{program}\"]\n{reqs}\
             [[artifact]]\ntarget = \"{TRIPLE}\"\nkind = \"binary\"\nasset = \"{asset}\"\n\
             sha256 = \"{sha}\"\ntree_root = \"{root}\"\nsize = 100\n\
             [artifact.cost]\ndisk_installed = 1048576\n",
            reqs = req_line(reqs)
        );
        pkg.insert(
            (program.to_string(), build),
            (
                pkg_body.clone().into_bytes(),
                sign(&RELEASE_SEED, pkg_body.as_bytes()),
            ),
        );
        archives.insert(asset, archive);
    }
    let yanked_toml = if yanked.is_empty() {
        String::new()
    } else {
        let items: Vec<String> = yanked.iter().map(|y| format!("\"{y}\"")).collect();
        format!("yanked = [{}]\n", items.join(", "))
    };
    let index_body = format!(
        "schema = 2\nindex_build = 41\nvalid_until = \"2026-07-05T12:00:00Z\"\n{attr}\
                      [programs.ay]\nrepo = \"ay\"\n[programs.ny]\nrepo = \"ny\"\n\
         [[channels]]\nname = \"stable\"\nchannel_build = 1\nmin_build = 0\n\
         {yanked_toml}pin = {{ ay = 18, ny = 9 }}\n",
        attr = attribution()
    );
    Fake {
        index: index_body.clone().into_bytes(),
        index_sig: sign(&RELEASE_SEED, index_body.as_bytes()),
        pkg,
        archives,
    }
}

/// THE RETIRED `requires` (§5.3(b)) is refused on BOTH lanes: `ay`'s signed index row
/// names it, so `install` refuses ay before any manifest is fetched, and the update
/// lane aborts ay's group with the same sentence — held on its current build, nothing
/// resolved or staged — even though what it names (`ny`) is live. `ny`, which names
/// nothing, installs as ever; and an up-to-date group has nothing to refuse.
#[test]
fn a_program_naming_the_retired_requires_is_refused_on_both_lanes() {
    let dir = scratch("requires-retired");
    let fake = requires_fixture(&dir, &[], &[], &[]);
    let index_body = String::from_utf8(fake.index.clone()).unwrap().replace(
        "[programs.ay]\nrepo = \"ay\"\n",
        "[programs.ay]\nrepo = \"ay\"\nrequires = [\"ny\"]\n",
    );
    assert!(index_body.contains("requires = [\"ny\"]"), "{index_body}");
    let fake = Fake {
        index: index_body.clone().into_bytes(),
        index_sig: sign(&RELEASE_SEED, index_body.as_bytes()),
        pkg: fake.pkg,
        archives: fake.archives,
    };
    let layout = layout(&dir);
    let req = |program| InstallRequest {
        channel: "stable",
        program,
        triple: TRIPLE,
        installed: None,
    };
    let err = install(&fake, &layout, &anchor(), &req("ay"), fl(0), 0).unwrap_err();
    assert!(
        matches!(&err, FlowError::RequiresRetired(p) if p == "ay"),
        "{err:?}"
    );
    assert!(
        !layout.staging_dir("ay").exists(),
        "refused before any byte moved"
    );
    install(&fake, &layout, &anchor(), &req("ny"), fl(0), 0).expect("ny names nothing");
    // The update lane: ay installed at 17, ny live.
    seed_build(&layout, "ay", 17, true);
    let run = |installed: &BTreeMap<String, u64>| {
        apply_channel(
            &fake,
            &layout,
            &anchor(),
            "stable",
            TRIPLE,
            installed,
            &[],
            fl(0),
            0,
        )
        .unwrap()
    };
    let report = run(&crate::ops::active_builds(&layout));
    let ay = report
        .groups
        .iter()
        .find(|(g, _)| g.members == vec!["ay".to_string()])
        .expect("ay's group");
    assert_eq!(
        ay.1,
        TxnOutcome::Aborted {
            failed: "ay".into(),
            during_flip: false,
            why: FlowError::RequiresRetired("ay".into()).to_string(),
        }
    );
    assert!(
        !report.resolved_assets.contains_key("ay"),
        "nothing resolved for a refused group"
    );
    assert!(!layout.build_dir("ay", 18).exists(), "nothing staged");
    assert_eq!(
        crate::ops::active_builds(&layout).get("ay").copied(),
        Some(17),
        "held on its current build"
    );
    // At its pin already: nothing to install out of order, nothing refused.
    seed_build(&layout, "ay", 18, true);
    let report = run(&crate::ops::active_builds(&layout));
    assert!(
        report
            .groups
            .iter()
            .all(|(_, o)| !matches!(o, TxnOutcome::Aborted { .. })),
        "{:?}",
        report.groups
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn requires_pulls_in_missing_dep() {
    let dir = scratch("req-pull");
    let fake = requires_fixture(&dir, &[], &["ny"], &[]);
    let layout = layout(&dir);
    let req = InstallRequest {
        channel: "stable",
        program: "ay",
        triple: TRIPLE,
        installed: None,
    };
    let report = install(&fake, &layout, &anchor(), &req, fl(0), 0).unwrap();
    assert_eq!(report.build, 18);
    assert!(
        report
            .dependencies
            .iter()
            .any(|d| d.program == "ny"
                && matches!(d.result, DepResult::Installed { build: 9, .. })),
        "ny pulled in first: {:?}",
        report.dependencies
    );
    assert!(crate::ops::which(&layout, "ay").is_some());
    assert!(
        crate::ops::which(&layout, "ny").is_some(),
        "the dep is live too"
    );
    // The alias policy through the REAL pipeline (§17.11): ALab's own programs (no
    // `system`) get `alab-<tool>` beside `<tool>`, a pulled-in dependency included.
    for program in ["ay", "ny"] {
        let alias = format!("alab-{program}");
        assert!(
            crate::ops::which(&layout, &alias).is_some(),
            "{alias} is laid by the install"
        );
    }
    let _ = std::fs::remove_dir_all(&dir);
}

/// A dependency the gate refuses — a yanked build, or a program the signed index does
/// not list (requires can't install an unlisted repo) — is Skipped, not fatal: `ay`
/// still installs, and the gate is NOT bypassed for the dependency.
#[test]
fn requires_skips_a_yanked_or_unreachable_dep() {
    for (label, yanked, dep) in [
        ("req-yank", &["ny@9"][..], "ny"),
        ("req-unreach", &[][..], "ghost"),
    ] {
        let dir = scratch(label);
        let fake = requires_fixture(&dir, yanked, &[dep], &[]);
        let layout = layout(&dir);
        let req = InstallRequest {
            channel: "stable",
            program: "ay",
            triple: TRIPLE,
            installed: None,
        };
        let report = install(&fake, &layout, &anchor(), &req, fl(0), 0).unwrap();
        assert!(
            crate::ops::which(&layout, "ay").is_some(),
            "{label}: ay still installs"
        );
        assert!(
            report
                .dependencies
                .iter()
                .any(|d| d.program == dep && matches!(d.result, DepResult::Skipped(_))),
            "{label}: the dep is Skipped, not fatal: {:?}",
            report.dependencies
        );
        assert!(
            crate::ops::which(&layout, dep).is_none(),
            "{label}: the gate is NOT bypassed"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}

#[test]
fn requires_already_present_dep_not_reinstalled() {
    let dir = scratch("req-present");
    let fake = requires_fixture(&dir, &[], &["ny"], &[]);
    let layout = layout(&dir);
    // Install ny first.
    let ny = InstallRequest {
        channel: "stable",
        program: "ny",
        triple: TRIPLE,
        installed: None,
    };
    install(&fake, &layout, &anchor(), &ny, fl(0), 0).unwrap();
    // Now ay requires ny, which is already active.
    let ay = InstallRequest {
        channel: "stable",
        program: "ay",
        triple: TRIPLE,
        installed: None,
    };
    let report = install(&fake, &layout, &anchor(), &ay, fl(0), 0).unwrap();
    assert!(
        report
            .dependencies
            .iter()
            .any(|d| d.program == "ny" && d.result == DepResult::AlreadyPresent(9)),
        "an already-active dep is AlreadyPresent, not reinstalled"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn requires_cycle_terminates() {
    let dir = scratch("req-cycle");
    // ay requires ny AND ny requires ay.
    let fake = requires_fixture(&dir, &[], &["ny"], &["ay"]);
    let layout = layout(&dir);
    let req = InstallRequest {
        channel: "stable",
        program: "ay",
        triple: TRIPLE,
        installed: None,
    };
    let report = install(&fake, &layout, &anchor(), &req, fl(0), 0).unwrap();
    // No infinite recursion: ay completes, both active, the back-edge is a cycle skip.
    assert!(crate::ops::which(&layout, "ay").is_some());
    assert!(crate::ops::which(&layout, "ny").is_some());
    assert!(
        report
            .dependencies
            .iter()
            .any(|d| d.program == "ay" && matches!(d.result, DepResult::Skipped(_))),
        "the back-edge ny→ay is recorded as a cycle skip"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn install_report_carries_signed_tree_root() {
    let dir = scratch("req-treeroot");
    let fake = fixture(&dir);
    let layout = layout(&dir);
    let req = InstallRequest {
        channel: "stable",
        program: "ay",
        triple: TRIPLE,
        installed: None,
    };
    let report = install(&fake, &layout, &anchor(), &req, fl(0), 0).unwrap();
    assert!(
        !report.tree_root.is_empty(),
        "the signed tree_root is recorded"
    );
    // The walk `atpkg verify` runs: over the declared-mode record the stage left
    // beside the build on a filesystem without permission bits (Windows), and the
    // plain read-back walk where the inode has them (Unix, where the record is
    // empty and never consulted).
    let build_dir = layout.build_dir("ay", 18);
    let modes = crate::store::declared_modes(&build_dir).unwrap();
    assert_eq!(
        report.tree_root,
        crate::tree::tree_root_declared(&build_dir, &modes).unwrap(),
        "it equals the on-disk tree's recomputed root (what `atpkg verify` compares)"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

// --- dev-link skip + dir fetcher + same-source cache (steps 13/14) ------------------

/// Mark `program` dev-linked by writing a minimal marker (no real checkout needed to
/// exercise the is_linked HARD-SKIP).
fn mark_linked(layout: &Layout, program: &str) {
    std::fs::create_dir_all(layout.links_dir()).unwrap();
    std::fs::write(
        layout.link_marker(program),
        format!("schema = 2\nprogram = \"{program}\"\ncheckout = \"/nonexistent\"\nbins = []\n"),
    )
    .unwrap();
}

#[test]
fn linked_program_is_hard_skipped_by_install() {
    let dir = scratch("linked-skip");
    let fake = fixture(&dir);
    let layout = layout(&dir);
    mark_linked(&layout, "ay");
    assert!(crate::linkmode::is_linked(&layout, "ay"));
    let req = InstallRequest {
        channel: "stable",
        program: "ay",
        triple: TRIPLE,
        installed: None,
    };
    let err = install(&fake, &layout, &anchor(), &req, fl(0), 0).unwrap_err();
    assert!(matches!(err, FlowError::Linked(_)), "got {err:?}");
    assert!(
        crate::ops::which(&layout, "ay").is_none(),
        "no store shim/build created"
    );
    assert!(!layout.build_dir("ay", 18).exists());
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn apply_channel_skips_a_linked_group_member() {
    let dir = scratch("linked-group");
    let fake = group_fixture(&dir);
    let layout = layout(&dir);
    seed_build(&layout, "ay", 17, true); // ay@17 active, grouped member installed
    mark_linked(&layout, "ay");
    let installed = std::collections::BTreeMap::from([("ay".to_string(), 17u64)]);
    let report = apply_channel(
        &fake,
        &layout,
        &anchor(),
        "stable",
        TRIPLE,
        &installed,
        &[],
        fl(0),
        0,
    )
    .unwrap();
    assert!(
        report.groups.is_empty(),
        "the linked group is excluded from the apply"
    );
    assert!(
        report.skipped_linked.contains(&"ay".to_string()),
        "the linked member is reported"
    );
    assert!(
        !layout.build_dir("trust", 4821).exists(),
        "the locked sibling is untouched"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// A [`Fake`] wrapper whose index fetch can be toggled to fail — or to succeed
/// EMPTY (the pushed-off-the-page listing) — with a controllable `source_id`
/// (to exercise the same-source cache guard + the empty-success fallback).
struct FlakyFake {
    inner: Fake,
    fail: std::cell::Cell<bool>,
    empty: std::cell::Cell<bool>,
    source: String,
}
impl FlakyFake {
    fn new(inner: Fake, source: &str) -> Self {
        Self {
            inner,
            fail: std::cell::Cell::new(false),
            empty: std::cell::Cell::new(false),
            source: source.into(),
        }
    }
}
impl Fetcher for FlakyFake {
    fn index_candidates(&self) -> Result<Vec<Candidate>, String> {
        if self.fail.get() {
            Err("network down".into())
        } else if self.empty.get() {
            Ok(vec![])
        } else {
            self.inner.index_candidates()
        }
    }
    fn pkg_manifest(
        &self,
        repo: &str,
        program: &str,
        build: u64,
    ) -> Result<(Vec<u8>, Vec<u8>), String> {
        self.inner.pkg_manifest(repo, program, build)
    }
    fn download(&self, repo: &str, asset: &str, dest: &Path) -> Result<(), String> {
        self.inner.download(repo, asset, dest)
    }
    fn source_id(&self) -> String {
        self.source.clone()
    }
}

#[test]
fn cached_index_is_used_only_on_same_source_fetch_failure() {
    let dir = scratch("cache-fallback");
    let layout = layout(&dir);
    let f = FlakyFake::new(fixture(&dir), "src:A");
    let req = InstallRequest {
        channel: "stable",
        program: "ay",
        triple: TRIPLE,
        installed: None,
    };
    // 1. A good fetch installs AND caches the index under source "src:A".
    install(&f, &layout, &anchor(), &req, fl(0), 0).unwrap();
    // 2. Fetch now fails, SAME source → the install is served from the cache.
    f.fail.set(true);
    install(&f, &layout, &anchor(), &req, fl(0), 0).expect("cache fallback serves the index");
    // 3. A DIFFERENT source with a failing fetch has no cache to fall back on.
    //    The error must name TRANSPORT, not trust: this is the state an offline,
    //    proxied or rate-limited machine reaches, and its fix is the network, not keys.
    let f2 = FlakyFake::new(fixture(&dir), "src:B");
    f2.fail.set(true);
    let err = install(&f2, &layout, &anchor(), &req, fl(0), 0).unwrap_err();
    assert!(
        matches!(err, FlowError::Unreachable(_)),
        "a dir: cache never satisfies a github: fetch, and an unreachable source \
         must not be reported as a signature failure — got {err:?}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// THE TYPED LANES NAME THE NETWORK. `plan_update` (the typed `update <program>`
/// lane, like `rollback` and `apply_program`) resolves live with no cache: a fetch
/// that could not reach the source is `Unreachable` with the transport's reason,
/// and a source reached that lists nothing is `NoIndex`.
#[test]
fn a_typed_lane_reports_an_unreached_index_as_the_network() {
    let dir = scratch("typed-unreachable");
    let layout = layout(&dir);
    let f = FlakyFake::new(fixture(&dir), "src:typed");
    let plan = |f: &FlakyFake| {
        plan_update(
            f,
            &layout,
            &anchor(),
            "stable",
            TRIPLE,
            "ay",
            None,
            fl(0),
            0,
        )
    };
    plan(&f).expect("a reachable index plans");
    f.fail.set(true);
    let err = plan(&f).unwrap_err();
    assert!(
        matches!(&err, FlowError::Unreachable(why) if why == "network down"),
        "got {err:?}"
    );
    f.fail.set(false);
    f.empty.set(true);
    let err = plan(&f).unwrap_err();
    assert!(
        matches!(err, FlowError::NoIndex),
        "reached but empty stays a trust verdict: {err:?}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

// An EMPTY listing success is a fetch that FOUND nothing — the index tag pushed off
// the release page by app-release cadence, a repo with no index release — and must
// take the SAME §14 same-source fallback as a hard Err, never a repo-wide NoIndex
// while a good cache sits on disk.
#[test]
fn empty_candidates_success_takes_the_same_source_cache_fallback() {
    let dir = scratch("cache-empty");
    let layout = layout(&dir);
    let f = FlakyFake::new(fixture(&dir), "src:A");
    let req = InstallRequest {
        channel: "stable",
        program: "ay",
        triple: TRIPLE,
        installed: None,
    };
    // 1. A good fetch installs AND caches under "src:A".
    install(&f, &layout, &anchor(), &req, fl(0), 0).unwrap();
    // 2. The listing now succeeds EMPTY, SAME source → served from the cache, and the
    //    resolve says it ran on the cache: the cache's bytes are no verification of what
    //    the source served (no success stamp, no current yank latch).
    f.empty.set(true);
    let live = live_verifications();
    install(&f, &layout, &anchor(), &req, fl(0), 0)
        .expect("an empty success falls back to the last-good cache");
    let resolve = last_resolve().expect("the install resolved");
    assert!(
        (!resolve.reached, resolve.answered, live_verifications()) == (true, true, live),
        "the cache served: {resolve:?}"
    );
    // 3. Empty success + no cache (fresh store, different source) → NoIndex.
    let dir2 = scratch("cache-empty-fresh");
    let fresh_layout = super::tests::layout(&dir2);
    let f2 = FlakyFake::new(fixture(&dir2), "src:B");
    f2.empty.set(true);
    let err = install(&f2, &fresh_layout, &anchor(), &req, fl(0), 0).unwrap_err();
    assert!(matches!(err, FlowError::NoIndex), "got {err:?}");
    let _ = std::fs::remove_dir_all(&dir);
    let _ = std::fs::remove_dir_all(&dir2);
}

#[test]
fn stale_cached_index_is_still_refused() {
    let dir = scratch("cache-stale");
    let layout = layout(&dir);
    let f = FlakyFake::new(fixture(&dir), "src:A");
    let req = InstallRequest {
        channel: "stable",
        program: "ay",
        triple: TRIPLE,
        installed: None,
    };
    // A `now` past valid_until: even a good fetch is refused Stale — but the bytes are cached.
    assert!(matches!(
        install(&f, &layout, &anchor(), &req, fl(0), 2_000_000_000),
        Err(FlowError::Stale)
    ));
    // The fetch now fails → fallback to the cached bytes, which are STILL past valid_until.
    f.fail.set(true);
    assert!(
        matches!(
            install(&f, &layout, &anchor(), &req, fl(0), 2_000_000_000),
            Err(FlowError::Stale)
        ),
        "freshness still gates cached bytes"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn dir_fetcher_installs_end_to_end() {
    let dir = scratch("dirfetch-e2e");
    let fake = fixture(&dir);
    let reg = dir.join("registry");
    std::fs::create_dir_all(&reg).unwrap();
    std::fs::write(reg.join("index.toml"), &fake.index).unwrap();
    std::fs::write(reg.join("index.toml.sig"), &fake.index_sig).unwrap();
    // A `dir:` registry publishes the master-signed roster too, exactly as a release
    // does: index without the generation that authorized its signer is not a registry.
    let (rb, rs) = testkit::published_roster();
    std::fs::write(reg.join(aterm_update_core::roster::ROSTER_ASSET), &rb).unwrap();
    std::fs::write(reg.join(aterm_update_core::roster::ROSTER_SIG_ASSET), &rs).unwrap();
    let (raw, sig) = fake.pkg.get(&("ay".to_string(), 18u64)).unwrap();
    std::fs::write(reg.join("pkg-ay-18.toml"), raw).unwrap();
    std::fs::write(reg.join("pkg-ay-18.toml.sig"), sig).unwrap();
    std::fs::copy(
        fake.archives.get("ay-18.tar.zst").unwrap(),
        reg.join("ay-18.tar.zst"),
    )
    .unwrap();
    let layout = layout(&dir);
    let df = crate::net::DirFetcher::new(reg.clone());
    let req = InstallRequest {
        channel: "stable",
        program: "ay",
        triple: TRIPLE,
        installed: None,
    };
    // dir bytes pass the identical verify + floor + freshness + shim gate.
    let report = install(&df, &layout, &anchor(), &req, fl(0), 0).unwrap();
    assert_eq!(report.build, 18);
    assert_eq!(
        crate::ops::which(&layout, "ay").unwrap(),
        tool_bin(&layout.build_dir("ay", 18), "ay")
    );
    assert!(
        crate::ops::which(&layout, "git").is_none(),
        "sensitive shim refused via dir too"
    );
    // Wrong root key ⇒ NoIndex (verify-before-parse intact even offline).
    let df2 = crate::net::DirFetcher::new(reg);
    let err = install(&df2, &layout, &anchor_of(&RELEASE_SEED), &req, fl(0), 0).unwrap_err();
    assert!(
        matches!(err, FlowError::NoIndex),
        "wrong root refuses the dir index"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// A build dir with the given executables, created directly in the store (the
/// flip/rollback tests below need on-disk shape, not a signed archive).
fn bare_build(l: &Layout, program: &str, build: u64, bins: &[&str]) -> PathBuf {
    let dir = l.build_dir(program, build);
    std::fs::create_dir_all(dir.join("bin")).unwrap();
    for b in bins {
        std::fs::write(tool_bin(&dir, b), b"#!/bin/true\n").unwrap();
    }
    dir
}

/// ROLLBACK RESTORES THE PRIOR BUILD'S WHOLE SURFACE. A tool the prior build shipped
/// and the new build DROPPED has no shim by rollback time — the new build's
/// `install_tools` prune deleted it (same program, different build: exactly its job) —
/// so a rollback iterating only the NEW exposes list re-activates the prior build with
/// that tool missing from PATH. The restore set is the union with the prior `bin/`.
#[test]
fn rollback_restores_a_tool_the_new_build_dropped() {
    let dir = scratch("rb-union");
    let l = layout(&dir);
    let b18 = bare_build(&l, "ay", 18, &["ay", "aylint"]);
    let b19 = bare_build(&l, "ay", 19, &["ay"]);
    // 18 live with both tools, then the flip to 19 prunes aylint's stale shim.
    activate_build(&l, &b18).unwrap();
    install_tools(&l, &b18, &[tool("ay"), tool("aylint")], Aliases::Off).unwrap();
    activate_build(&l, &b19).unwrap();
    install_tools(&l, &b19, &[tool("ay")], Aliases::Off).unwrap();
    assert!(
        crate::platform::resolve_shim(&l.shim(&tool("aylint"))).is_none(),
        "fixture: the prune removed the dropped tool's shim"
    );

    let staged = Staged {
        build: 19,
        build_dir: b19,
        exposes: vec![tool("ay")],
        prior_build: Some(18),
        was_live: false,
        reloc: None,
        aliases: Aliases::Off,
        tree_root: String::new(),
    };
    rollback_member(&l, "ay", &staged);

    for t in ["ay", "aylint"] {
        let target = crate::platform::resolve_shim(&l.shim(&tool(t)))
            .unwrap_or_else(|| panic!("{t}'s shim is restored by the rollback"));
        assert!(
            target.starts_with(&b18),
            "{t} points into the prior build: {}",
            target.display()
        );
    }
    let _ = std::fs::remove_dir_all(&dir);
}

/// A TRUST ROLLBACK NEVER TAKES ANOTHER PROGRAM'S SHIM. A sysroot bundle's `bin/`
/// carries backends belonging to other atpkg programs (trust's carries `ay`, `clean`
/// and `ty`); were the rollback's LISTING half to re-point `bin/ty` at
/// `store/trust/<prior>`, the next trust flip's prune would delete it as a stale trust
/// shim. Covered here: a shim another program owns (ty), another
/// installed program's own name whose shim is already missing (ay), a dev link into a
/// checkout (clean) — each left alone. The tool the prior trust build ships and nobody
/// owns (`trustd`, absent) is still restored, AS IT ALWAYS WAS: that pins the old
/// behaviour for an unowned name, not a requirement. The listing half exists to restore a
/// tool the prior build EXPOSED and the new one dropped; `trustd` is in trust's `bin/`
/// but not its signed exposes, and laying it is a known over-reach this manager cannot
/// tell apart until each build records its signed exposes at stage time.
#[cfg(unix)]
#[test]
fn a_trust_rollback_never_takes_another_programs_shim() {
    let dir = scratch("rb-foreign");
    let l = layout(&dir);
    let ty = bare_build(&l, "ty", 3007, &["ty", "tla"]);
    activate_build(&l, &ty).unwrap();
    install_tools(&l, &ty, &[tool("ty"), tool("tla")], Aliases::Alab).unwrap();
    // ay is installed but its shim is gone: its own name still belongs to it.
    let ay = bare_build(&l, "ay", 8256, &["ay"]);
    activate_build(&l, &ay).unwrap();
    // clean is dev-linked into a checkout.
    let checkout = dir.join("checkout").join("bin");
    std::fs::create_dir_all(&checkout).unwrap();
    std::fs::write(checkout.join("clean"), b"#!/bin/true\n").unwrap();
    crate::platform::install_shim_env(
        &checkout,
        &tool("clean"),
        &l.shim(&tool("clean")),
        &crate::shim_env::ShimEnv::default(),
    )
    .unwrap();
    let bundle = ["trustc", "ty", "ay", "clean", "trustd"];
    let t1 = bare_build(&l, "trust", 1, &bundle);
    let t2 = bare_build(&l, "trust", 2, &bundle);
    activate_build(&l, &t1).unwrap();
    install_tools(&l, &t1, &[tool("trustc")], Aliases::Alab).unwrap();
    activate_build(&l, &t2).unwrap();
    install_tools(&l, &t2, &[tool("trustc")], Aliases::Alab).unwrap();

    let staged = Staged {
        build: 2,
        build_dir: t2.clone(),
        exposes: vec![tool("trustc")],
        prior_build: Some(1),
        was_live: false,
        reloc: None,
        aliases: Aliases::Alab,
        tree_root: String::new(),
    };
    rollback_member(&l, "trust", &staged);

    let into = |name: &str| crate::platform::resolve_shim(&l.shim(&tool(name)));
    let owned_by_ty = |label: &str| {
        for name in ["ty", "alab-ty"] {
            let target = into(name).unwrap_or_else(|| panic!("{label}: {name} is gone from bin/"));
            assert!(
                target.starts_with(&ty),
                "{label}: {name} was taken off the ty program: {}",
                target.display()
            );
        }
    };
    owned_by_ty("after the rollback");
    for name in ["ay", "alab-ay"] {
        assert!(
            std::fs::symlink_metadata(l.shim(&tool(name))).is_err(),
            "the rollback laid {name} for trust: {:?}",
            into(name)
        );
    }
    assert_eq!(
        into("clean"),
        Some(checkout.join("clean")),
        "the dev link is untouched"
    );
    assert!(
        into("trustc").is_some_and(|t| t.starts_with(&t1)),
        "trust's own tool is rolled back: {:?}",
        into("trustc")
    );
    assert!(
        into("trustd").is_some_and(|t| t.starts_with(&t1)),
        "an unowned tool of the prior build is still restored, as before: {:?}",
        into("trustd")
    );

    // The flip that follows prunes stale TRUST shims; ty's must survive it.
    activate_build(&l, &t2).unwrap();
    install_tools(&l, &t2, &[tool("trustc")], Aliases::Alab).unwrap();
    owned_by_ty("after the next trust flip");
    assert!(into("tla").is_some_and(|t| t.starts_with(&ty)));
    let _ = std::fs::remove_dir_all(&dir);
}

/// A LISTING NAME'S ALIAS GETS ITS OWN OWNERSHIP CHECK. The primary `bin/trustd` is
/// absent — free, so the rollback may restore it into the prior trust build — while
/// `alab-trustd` is a dev link into a checkout. Restoring the primary must not drag the
/// alias along: the alias is someone else's, and is left exactly where it points. The
/// counter-case in the same run: `trustc`, a signed expose, has its alias re-pointed
/// unconditionally, as before.
#[cfg(unix)]
#[test]
fn a_rollback_never_takes_a_foreign_alias_of_a_listing_name() {
    let dir = scratch("rb-foreign-alias");
    let l = layout(&dir);
    let checkout = dir.join("checkout").join("bin");
    std::fs::create_dir_all(&checkout).unwrap();
    std::fs::write(checkout.join("trustd"), b"#!/bin/true\n").unwrap();
    let bundle = ["trustc", "trustd"];
    let t1 = bare_build(&l, "trust", 1, &bundle);
    let t2 = bare_build(&l, "trust", 2, &bundle);
    activate_build(&l, &t1).unwrap();
    install_tools(&l, &t1, &[tool("trustc")], Aliases::Alab).unwrap();
    activate_build(&l, &t2).unwrap();
    install_tools(&l, &t2, &[tool("trustc")], Aliases::Alab).unwrap();
    crate::platform::install_shim_env(
        &checkout,
        &tool("trustd"),
        &l.shim(&tool("alab-trustd")),
        &crate::shim_env::ShimEnv::default(),
    )
    .unwrap();
    assert!(std::fs::symlink_metadata(l.shim(&tool("trustd"))).is_err());

    let staged = Staged {
        build: 2,
        build_dir: t2.clone(),
        exposes: vec![tool("trustc")],
        prior_build: Some(1),
        was_live: false,
        reloc: None,
        aliases: Aliases::Alab,
        tree_root: String::new(),
    };
    rollback_member(&l, "trust", &staged);

    let into = |name: &str| crate::platform::resolve_shim(&l.shim(&tool(name)));
    assert!(
        into("trustd").is_some_and(|t| t.starts_with(&t1)),
        "the free primary is restored into the prior build: {:?}",
        into("trustd")
    );
    assert_eq!(
        into("alab-trustd"),
        Some(checkout.join("trustd")),
        "the dev-linked alias of a listing name is left alone"
    );
    for name in ["trustc", "alab-trustc"] {
        assert!(
            into(name).is_some_and(|t| t.starts_with(&t1)),
            "{name}: a signed expose and its alias roll back: {:?}",
            into(name)
        );
    }
    let _ = std::fs::remove_dir_all(&dir);
}

/// AN OLDER PREFIX STILL OWNS ITS PROGRAMS' NAMES. A prefix laid before the
/// per-program `store/<program>/current` link has an installed `ay` — a COMPLETE build
/// under `store/ay/` — and no link. Its shim is gone. A trust rollback finds `ay` in
/// the prior bundle's `bin/` and must still refuse it: an absent shim is not a free
/// name when a program of that name is installed, or the next trust flip prunes it
/// and `ay` is lost. The negative half: the same rollback with the ay
/// build left INCOMPLETE (no marker, so not installed) does restore `ay` for trust.
#[cfg(unix)]
#[test]
fn a_rollback_respects_a_program_installed_without_a_current_link() {
    for complete in [true, false] {
        let dir = scratch(if complete {
            "rb-old-prefix"
        } else {
            "rb-old-prefix-partial"
        });
        let l = layout(&dir);
        let ay = bare_build(&l, "ay", 8256, &["ay"]);
        if complete {
            crate::store::mark_build_ready(&ay).unwrap();
        }
        assert!(std::fs::symlink_metadata(l.program_current("ay")).is_err());
        let bundle = ["trustc", "ay"];
        let t1 = bare_build(&l, "trust", 1, &bundle);
        let t2 = bare_build(&l, "trust", 2, &bundle);
        activate_build(&l, &t1).unwrap();
        install_tools(&l, &t1, &[tool("trustc")], Aliases::Off).unwrap();
        activate_build(&l, &t2).unwrap();
        install_tools(&l, &t2, &[tool("trustc")], Aliases::Off).unwrap();
        let staged = Staged {
            build: 2,
            build_dir: t2.clone(),
            exposes: vec![tool("trustc")],
            prior_build: Some(1),
            was_live: false,
            reloc: None,
            aliases: Aliases::Off,
            tree_root: String::new(),
        };
        rollback_member(&l, "trust", &staged);
        let laid = crate::platform::resolve_shim(&l.shim(&tool("ay")));
        if complete {
            assert_eq!(laid, None, "the installed ay's name was taken for trust");
        } else {
            assert!(
                laid.is_some_and(|t| t.starts_with(&t1)),
                "an incomplete ay build is not installed; the name is free: {:?}",
                crate::platform::resolve_shim(&l.shim(&tool("ay")))
            );
        }
        let _ = std::fs::remove_dir_all(&dir);
    }
}

/// A ROLLBACK TO AN AFFECTED TRUST BUILD LAYS ITS EXEC ROOT BEFORE ITS SHIMS. Live on
/// trust 8595 (routed through its root), rolled back to a retained 8590 that has no root
/// yet — both shipping `bin/rustc` as a separate file from `bin/trustc`: the undo lays
/// `compat/trust/8590` itself and the re-pointed `tippy` shim routes through it on the
/// first render, with no reconcile: the single-program update and install lanes run
/// no reconcile after a failed flip.
#[cfg(unix)]
#[test]
fn rollback_to_an_affected_trust_build_lays_its_root_and_routes() {
    let dir = scratch("rb-exec-root");
    let l = layout(&dir);
    let affected = |n: u64| {
        let d = bare_build(&l, "trust", n, &["trustc", "targo", "tippy"]);
        std::fs::write(
            d.join("bin").join("trustc"),
            format!("{n} signed as trustc"),
        )
        .unwrap();
        std::fs::write(d.join("bin").join("rustc"), format!("{n} signed as rustc_")).unwrap();
        d
    };
    let b8590 = affected(8590);
    let b8595 = affected(8595);
    let exposes = vec![tool("tippy"), tool("targo")];
    activate_build(&l, &b8595).unwrap();
    install_tools(&l, &b8595, &exposes, Aliases::Off).unwrap();
    let tippy = l.shim(&tool("tippy"));
    assert!(
        crate::compat::route_for_shim(&tippy, &b8595.join("bin").join("tippy")).is_some(),
        "fixture: 8595 routes"
    );
    assert!(std::fs::symlink_metadata(crate::compat::root_dir(&l, 8590)).is_err());

    let staged = Staged {
        build: 8595,
        build_dir: b8595,
        exposes,
        prior_build: Some(8590),
        was_live: true,
        reloc: None,
        aliases: Aliases::Off,
        tree_root: String::new(),
    };
    rollback_member(&l, "trust", &staged);

    let target = b8590.join("bin").join("tippy");
    assert_eq!(crate::platform::resolve_shim(&tippy), Some(target.clone()));
    let routed = crate::compat::root_dir(&l, 8590).join("bin").join("tippy");
    assert_eq!(
        crate::compat::route_for_shim(&tippy, &target),
        Some(routed.clone())
    );
    assert_eq!(
        std::fs::read_to_string(&tippy).unwrap(),
        crate::platform::sh_shim_content_routed(
            &target,
            &crate::shim_env::ShimEnv::NONE,
            Some(&routed)
        )
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// ALIASES RIDE WITH THEIR PRIMARY through the transaction's other exits: a rollback
/// re-points `alab-<tool>` at the prior build beside `<tool>` (and drops the alias of
/// a tool the prior build lacks), a fresh-install rollback removes it, and a
/// tombstone disables it — a revoked build must not stay runnable under its other
/// name.
#[test]
fn aliases_are_rolled_back_and_tombstoned_with_their_primary() {
    let dir = scratch("rb-alias");
    let l = layout(&dir);
    let b18 = bare_build(&l, "ay", 18, &["ay"]);
    let b19 = bare_build(&l, "ay", 19, &["ay", "aynew"]);
    activate_build(&l, &b18).unwrap();
    install_tools(&l, &b18, &[tool("ay")], Aliases::Alab).unwrap();
    activate_build(&l, &b19).unwrap();
    install_tools(&l, &b19, &[tool("ay"), tool("aynew")], Aliases::Alab).unwrap();
    for t in ["ay", "aynew", "alab-ay", "alab-aynew"] {
        assert!(
            crate::platform::resolve_shim(&l.shim(&tool(t))).is_some_and(|p| p.starts_with(&b19)),
            "fixture: {t} points into 19"
        );
    }
    let staged = Staged {
        build: 19,
        build_dir: b19.clone(),
        exposes: vec![tool("ay"), tool("aynew")],
        prior_build: Some(18),
        was_live: false,
        reloc: None,
        tree_root: String::new(),
        aliases: Aliases::Alab,
    };
    rollback_member(&l, "ay", &staged);
    for t in ["ay", "alab-ay"] {
        let target = crate::platform::resolve_shim(&l.shim(&tool(t)))
            .unwrap_or_else(|| panic!("{t} is restored by the rollback"));
        assert!(target.starts_with(&b18), "{t} points into the prior build");
    }
    assert_eq!(
        crate::platform::resolve_shim(&l.shim(&tool("alab-ay"))),
        crate::platform::resolve_shim(&l.shim(&tool("ay"))),
        "the alias and its primary agree"
    );
    for t in ["aynew", "alab-aynew"] {
        assert!(
            std::fs::symlink_metadata(l.shim(&tool(t))).is_err(),
            "{t}: a tool the prior build lacks loses its shim AND its alias"
        );
    }
    // The rollback VERB reads the policy off the disk: aliases laid ⇒ kept.
    assert_eq!(Aliases::laid_for(&l, "ay"), Aliases::Alab);

    // A fresh install's rollback removes the alias with the primary.
    let fresh = Staged {
        build: 19,
        build_dir: b19,
        exposes: vec![tool("ay")],
        prior_build: None,
        was_live: false,
        reloc: None,
        tree_root: String::new(),
        aliases: Aliases::Alab,
    };
    rollback_member(&l, "ay", &fresh);
    assert!(std::fs::symlink_metadata(l.shim(&tool("ay"))).is_err());
    assert!(std::fs::symlink_metadata(l.shim(&tool("alab-ay"))).is_err());

    // Tombstoning a revoked build disables the alias too.
    install_tools(&l, &b18, &[tool("ay")], Aliases::Alab).unwrap();
    install_tombstone_shims(&l, "ay", Some(18));
    for t in ["ay", "alab-ay"] {
        let shim = l.shim(&tool(t));
        assert!(
            std::fs::symlink_metadata(&shim).is_ok_and(|m| m.file_type().is_file()),
            "{t} is a tombstone file"
        );
        assert!(
            crate::platform::resolve_shim(&shim).is_none(),
            "{t} no longer forwards anywhere"
        );
    }
    let _ = std::fs::remove_dir_all(&dir);
}

/// THE FRONT-OF-PATH TWIN FOLLOWS THE ROLLBACK. An agent program is laid twice — at
/// `bin/<tool>` and again under `agents/`, which the shell hook MOVES TO THE FRONT of
/// PATH — and the twin names its store build directly, so it only ever changes when a
/// reconcile runs. This lane lays shims through `install_shim_env`, not
/// `install_tools_env`, so none did: `aterm pkg rollback claude` re-pointed
/// `bin/claude` at the prior build and said so while `agents/claude` — the copy that
/// actually runs — kept exec'ing the build the user rolled away from, until gc
/// reclaimed it (its claim union reads `bin/` and the `current` links, never
/// `agents/`) and the front-of-PATH name exec'd a deleted file. The fresh-install arm
/// is the same wound: the flip's own lay wrote the twin, and the abort is about to
/// delete the build it names.
#[test]
fn rollback_repoints_the_front_of_path_agents_twin() {
    let dir = scratch("rb-agents-twin");
    let l = layout(&dir);
    let claude = tool("claude");
    let b1 = bare_build(&l, "claude", 2_026_091_001, &["claude"]);
    let b2 = bare_build(&l, "claude", 2_026_091_002, &["claude"]);
    activate_build(&l, &b1).unwrap();
    install_tools(&l, &b1, std::slice::from_ref(&claude), Aliases::Off).unwrap();
    activate_build(&l, &b2).unwrap();
    install_tools(&l, &b2, std::slice::from_ref(&claude), Aliases::Off).unwrap();
    let twin = l.agent_shim(&claude);
    assert_eq!(
        crate::platform::resolve_shim(&twin),
        Some(tool_bin(&b2, "claude")),
        "fixture: the twin names the build that is live"
    );

    let staged = Staged {
        build: 2_026_091_002,
        build_dir: b2.clone(),
        exposes: vec![claude.clone()],
        prior_build: Some(2_026_091_001),
        was_live: true,
        reloc: None,
        tree_root: String::new(),
        aliases: Aliases::Off,
    };
    rollback_member(&l, "claude", &staged);

    assert_eq!(
        crate::platform::resolve_shim(&l.shim(&claude)),
        Some(tool_bin(&b1, "claude")),
        "fixture: bin/claude is back on the prior build"
    );
    assert_eq!(
        crate::platform::resolve_shim(&twin),
        Some(tool_bin(&b1, "claude")),
        "the front-of-PATH twin followed it, so `claude` runs what was rolled back to"
    );

    // The fresh-install arm: no prior build to re-point, so the primary goes — and
    // nothing in `bin/` is left to vouch for the twin.
    let fresh = Staged {
        build: 2_026_091_001,
        build_dir: b1,
        exposes: vec![claude.clone()],
        prior_build: None,
        was_live: false,
        reloc: None,
        tree_root: String::new(),
        aliases: Aliases::Off,
    };
    rollback_member(&l, "claude", &fresh);
    assert!(
        std::fs::symlink_metadata(l.shim(&claude)).is_err(),
        "fixture: the fresh-install undo removed the primary"
    );
    assert!(
        std::fs::symlink_metadata(&twin).is_err(),
        "the twin goes with it rather than exec'ing a build the abort deletes"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// `chmod` for the flip-failure fixtures below.
#[cfg(unix)]
fn set_mode(path: &Path, mode: u32) {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode)).unwrap();
}

/// A FAILED FLIP LEAVES THE WITNESS AGREEING WITH THE SHIMS. When the link write
/// fails, `flip_member` reports failure with `store/<program>/current` still naming
/// the prior build — the shims were never touched — never the build the abort
/// cleanup deletes (a broken own link makes GC abstain on the program until the next
/// successful activation). The write here fails the way a Windows junction swap does:
/// the old link is removed first, so without the repair the witness would be GONE.
#[test]
fn a_failed_flip_points_the_witness_back_at_the_prior_build() {
    let dir = scratch("flip-witness");
    let l = layout(&dir);
    let b18 = bare_build(&l, "ay", 18, &["ay"]);
    let b19 = bare_build(&l, "ay", 19, &["ay"]);
    activate_build(&l, &b18).unwrap();
    install_tools(&l, &b18, &[tool("ay")], Aliases::Off).unwrap();

    let staged = Staged {
        build: 19,
        build_dir: b19,
        exposes: vec![tool("ay")],
        prior_build: Some(18),
        was_live: false,
        reloc: None,
        aliases: Aliases::Off,
        tree_root: String::new(),
    };
    let flipped = flip_member_with(&l, "ay", &staged, |layout, _| {
        crate::platform::remove_link(&layout.program_current("ay"));
        Err(std::io::Error::other(
            "mklink failed after the junction was removed",
        ))
    });
    assert!(!flipped, "the flip must report failure");
    assert_eq!(
        std::fs::read_link(l.program_current("ay")).expect("the witness is repaired"),
        b18,
        "the witness names the prior build again, agreeing with the untouched shims"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// The fresh-install variant of the failed flip: no prior build exists, so the repair
/// REMOVES the witness link a half-made write left naming the new build — the abort
/// cleanup deletes the build it named, and a program that was never installed must
/// not keep a (dangling) witness.
#[test]
fn a_failed_fresh_install_flip_removes_the_witness_link() {
    let dir = scratch("flip-fresh");
    let l = layout(&dir);
    let b19 = bare_build(&l, "ay", 19, &["ay"]);

    let staged = Staged {
        build: 19,
        build_dir: b19,
        exposes: vec![tool("ay")],
        prior_build: None,
        was_live: false,
        reloc: None,
        aliases: Aliases::Off,
        tree_root: String::new(),
    };
    let flipped = flip_member_with(&l, "ay", &staged, |layout, build_dir| {
        activate_build(layout, build_dir)?;
        Err(std::io::Error::other(
            "the junction was made, then the write reported failure",
        ))
    });
    assert!(!flipped);
    assert!(
        std::fs::symlink_metadata(l.program_current("ay")).is_err(),
        "no witness link survives a failed fresh install"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// Where the link write fails ATOMICALLY (POSIX: `store/<program>/` read-only, so
/// the temp link cannot be made) the link is exactly as it was and the repair only
/// re-writes the same target — the shape every real POSIX failure takes.
#[cfg(unix)] // read-only dir injection
#[test]
fn an_atomic_link_write_failure_leaves_the_witness_on_the_prior_build() {
    let dir = scratch("flip-atomic");
    let l = layout(&dir);
    let b18 = bare_build(&l, "ay", 18, &["ay"]);
    let b19 = bare_build(&l, "ay", 19, &["ay"]);
    activate_build(&l, &b18).unwrap();
    install_tools(&l, &b18, &[tool("ay")], Aliases::Off).unwrap();
    let store_ay = l.prefix.join("store").join("ay");
    set_mode(&store_ay, 0o555);
    let staged = Staged {
        build: 19,
        build_dir: b19,
        exposes: vec![tool("ay")],
        prior_build: Some(18),
        was_live: false,
        reloc: None,
        aliases: Aliases::Off,
        tree_root: String::new(),
    };
    let flipped = flip_member(&l, "ay", &staged);
    set_mode(&store_ay, 0o755);
    assert!(!flipped, "the flip must report failure");
    assert_eq!(std::fs::read_link(l.program_current("ay")).unwrap(), b18);
    let _ = std::fs::remove_dir_all(&dir);
}

/// The strandage bound a resumable download needs: OUR partial survives (the next
/// attempt continues it), every other program-staging partial is reclaimed, and
/// nothing else in the directory is touched.
///
/// Without the sweep a resumable lane trades "re-fetch 630 MB on every retry" for
/// "leak a 600 MB prefix per abandoned build", which is not a trade worth making.
#[test]
fn a_resumable_download_strands_at_most_one_partial_per_program() {
    let dir = crate::testdir::dir("atpkg-sweep-part-");
    let dl = dir.join("trust-5520.tar.zst");
    let ours = dir.join("trust-5520.tar.zst.part");
    let stale = dir.join("trust-5100.tar.zst.part");
    let bystander = dir.join("notes.txt");
    let finished = dir.join("trust-5100.tar.zst");
    // A vendor-direct asset carries no extension: it is known by its program's name.
    let vendor = dir.join("claude-2.1.280-aarch64-apple-darwin");
    for p in [&ours, &stale, &bystander, &finished, &vendor] {
        std::fs::write(p, b"x").unwrap();
    }

    sweep_foreign_partials(&dl);

    assert!(
        ours.exists(),
        "OUR partial is what the next attempt resumes"
    );
    assert!(!stale.exists(), "a superseded build's partial is reclaimed");
    assert!(
        bystander.exists(),
        "a file that is neither partial nor archive is left alone"
    );
    // Since archives of a failed stage are RETAINED for reuse, a complete
    // archive of ANOTHER asset name is swept too: at most one of each survives per
    // program, and it is this pass's.
    assert!(
        !finished.exists(),
        "a superseded build's retained archive is reclaimed"
    );
    assert!(!vendor.exists(), "a vendor-direct asset is reclaimed");
    let _ = std::fs::remove_dir_all(&dir);
}

/// The GROUP path holds the same strandage bound as the singleton path: staging a
/// member reclaims every foreign partial in that program's staging dir (the mirror
/// of [`a_resumable_download_strands_at_most_one_partial_per_program`], driven
/// through `apply_channel` so it pins `stage_member`'s call, not just the helper).
#[test]
fn a_group_member_stage_strands_at_most_one_partial_per_program() {
    let dir = scratch("group-part-sweep");
    let fake = group_fixture(&dir);
    let layout = layout(&dir);
    let staging = layout.staging_dir("trust");
    std::fs::create_dir_all(&staging).unwrap();
    let stale = staging.join("trust-4700.tar.zst.part");
    let bystander = staging.join("notes.txt");
    for p in [&stale, &bystander] {
        std::fs::write(p, b"x").unwrap();
    }
    let installed = std::collections::BTreeMap::from([("ay".to_string(), 17u64)]);
    let report = apply_channel(
        &fake,
        &layout,
        &anchor(),
        "stable",
        TRIPLE,
        &installed,
        &[],
        fl(0),
        0,
    )
    .unwrap();
    assert!(
        layout.build_dir("trust", 4821).exists(),
        "reach guard: the group member actually staged — report {report:?}"
    );
    assert!(
        !stale.exists(),
        "a superseded build's partial is reclaimed on the group path too"
    );
    assert!(bystander.exists(), "only `.part` files are swept");
    let _ = std::fs::remove_dir_all(&dir);
}

/// A [`Fake`] that COUNTS the artifact downloads it is asked for, by asset name.
/// Transfers are the whole subject of the two tests below, and the only honest way to
/// measure "this pass moved no bytes" is to ask the fetcher.
struct Counting {
    inner: Fake,
    downloads: std::cell::RefCell<HashMap<String, usize>>,
}
impl Counting {
    fn of(inner: Fake) -> Self {
        Self {
            inner,
            downloads: std::cell::RefCell::new(HashMap::new()),
        }
    }
    fn count(&self, asset: &str) -> usize {
        self.downloads.borrow().get(asset).copied().unwrap_or(0)
    }
}
impl Fetcher for Counting {
    fn index_candidates(&self) -> Result<Vec<Candidate>, String> {
        self.inner.index_candidates()
    }
    fn pkg_manifest(
        &self,
        repo: &str,
        program: &str,
        build: u64,
    ) -> Result<(Vec<u8>, Vec<u8>), String> {
        self.inner.pkg_manifest(repo, program, build)
    }
    fn download(&self, repo: &str, asset: &str, dest: &Path) -> Result<(), String> {
        *self
            .downloads
            .borrow_mut()
            .entry(asset.to_string())
            .or_default() += 1;
        self.inner.download(repo, asset, dest)
    }
}

/// Append bytes to a fixture's archive AFTER its manifest signed the honest sha256, so
/// every download of it fails the signed digest gate — a publish-pipeline slip, exactly
/// as a machine meets it.
fn poison(archive: &Path) {
    use std::io::Write as _;
    let mut f = std::fs::OpenOptions::new()
        .append(true)
        .open(archive)
        .unwrap();
    f.write_all(b"poison").unwrap();
}

/// Rewrite a refusal memo's `at=` line so its cooldown has lapsed, the way the clock
/// does on a real machine.
fn age_refusal(memo: &Path) {
    let text = std::fs::read_to_string(memo).unwrap();
    let aged: Vec<String> = text
        .lines()
        .map(|l| {
            if l.starts_with("at=") {
                String::from("at=0")
            } else {
                l.to_string()
            }
        })
        .collect();
    std::fs::write(memo, aged.join("\n") + "\n").unwrap();
}

/// THE REFETCH BOUND, singleton lane. Re-proving that a published asset does not match
/// its pin's signed sha256 costs the whole asset, every six hours, for as long as the
/// pin stands. The first retry is still free — one mismatch can be a truncated
/// transfer, and a repair under the same pin must be found — but the SECOND identical
/// verdict arms a cooldown that holds the next pass off the wire entirely and SAYS
/// why; once it lapses the pass tries again, once, and the memo re-arms for longer.
#[test]
fn a_digest_refusal_is_not_refetched_on_every_tick() {
    let dir = scratch("digest-refusal");
    let fake = Counting::of(fixture(&dir));
    let layout = layout(&dir);
    poison(&dir.join("ay-18.tar.zst"));
    let req = InstallRequest {
        channel: "stable",
        program: "ay",
        triple: TRIPLE,
        installed: None,
    };
    let first = install(&fake, &layout, &anchor(), &req, fl(0), 0)
        .expect_err("a corrupted asset must fail the signed digest gate");
    assert!(
        matches!(&first, FlowError::Stage(StageError::DigestMismatch { .. })),
        "reach guard: the first attempt reached the digest gate — {first:?}"
    );
    assert_eq!(fake.count("ay-18.tar.zst"), 1, "it had to download it once");
    let memo = layout.build_dir("ay", 18).with_file_name("18.refused");
    assert!(memo.is_file(), "the verdict is recorded beside the build");

    // THE FIRST RETRY IS FREE — one mismatch can be a truncated transfer, and a
    // publisher who repairs the asset under the same pin must be found on the next
    // tick (`a_member_stage_failure_aborts_the_group_and_a_retry_heals_it` pins
    // exactly that healing). So this pass downloads again, and it is its IDENTICAL
    // verdict that arms the cooldown.
    let second = install(&fake, &layout, &anchor(), &req, fl(0), 0)
        .expect_err("a corrupted asset must fail the signed digest gate");
    assert!(
        matches!(&second, FlowError::Stage(StageError::DigestMismatch { .. })),
        "the first retry is never refused — {second:?}"
    );
    assert_eq!(fake.count("ay-18.tar.zst"), 2, "it really did retry");

    // The tick after THAT, pin unchanged: no transfer at all, and the refusal carries
    // the original stage failure's own words plus the way out.
    let third = install(&fake, &layout, &anchor(), &req, fl(0), 0)
        .expect_err("the pin still names digests the asset does not match");
    let FlowError::StageRefused(note) = &third else {
        panic!("the third tick must be refused by the memo, not re-staged: {third:?}");
    };
    assert!(
        note.contains("asset sha256 mismatch") && note.contains("aterm pkg install ay"),
        "the refusal names the original verdict and the way out: {note}"
    );
    assert_eq!(
        fake.count("ay-18.tar.zst"),
        2,
        "every six-hourly tick re-downloaded the whole asset; it must not"
    );

    // Once the cooldown lapses the machine asks again — exactly once — and the memo
    // re-arms, longer, so a publisher's repair under the same pin is never hidden
    // forever.
    age_refusal(&memo);
    let fourth = install(&fake, &layout, &anchor(), &req, fl(0), 0)
        .expect_err("the lapsed cooldown lets the attempt through");
    assert!(
        matches!(&fourth, FlowError::Stage(StageError::DigestMismatch { .. })),
        "a lapsed memo refuses nothing — {fourth:?}"
    );
    assert_eq!(
        fake.count("ay-18.tar.zst"),
        3,
        "one attempt, not four a day"
    );
    assert!(
        std::fs::read_to_string(&memo)
            .unwrap()
            .contains("attempts=3"),
        "the refusal re-armed, longer"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// THE HOLD, singleton lane. A `tree_root` mismatch is the same verdict on every
/// re-stage — the archive passed the signed sha256, the extraction is deterministic —
/// so re-staging it on every pass only repeats the failure. From the FIRST such verdict the
/// row is HELD: no download, no stage, the canonical `held: last attempt failed with
/// …` row naming the retry; the verified archive is kept so the retry is free of
/// transfer; the hold never lapses on its own; and the explicit door (what
/// `install <p>` and `update --retry` do) forgets it — after which the stage runs
/// again over the kept archive, honestly reaching the same verdict here because the
/// fixture's signed root is wrong on purpose.
#[test]
fn a_tree_root_mismatch_is_held_off_every_later_tick_until_asked() {
    let dir = scratch("tree-root-hold");
    let fake = Counting::of(fixture_from(
        &dir,
        "binary",
        make_archive(&dir),
        Some(&"a".repeat(64)),
    ));
    let layout = layout(&dir);
    let req = InstallRequest {
        channel: "stable",
        program: "ay",
        triple: TRIPLE,
        installed: None,
    };
    let first = install(&fake, &layout, &anchor(), &req, fl(0), 0)
        .expect_err("a wrong signed root must fail the re-verify");
    assert!(
        matches!(
            &first,
            FlowError::Stage(StageError::TreeRootMismatch { .. })
        ),
        "reach guard: the first attempt reached the re-verify — {first:?}"
    );
    assert_eq!(fake.count("ay-18.tar.zst"), 1, "it had to download it once");
    let memo = layout.build_dir("ay", 18).with_file_name("18.refused");
    let text = std::fs::read_to_string(&memo).expect("the verdict is recorded");
    assert!(text.contains("\nhold=1\n"), "held, not cooled down: {text}");
    let kept = staged_download_path(&layout, "ay", "ay-18.tar.zst").unwrap();
    assert!(kept.is_file(), "the verified archive is KEPT for the retry");

    // THE VERY NEXT TICK IS HELD: nothing fetched, nothing staged, and the row
    // says what failed and how to retry — in the words every surface repeats.
    let second = install(&fake, &layout, &anchor(), &req, fl(0), 0)
        .expect_err("the hold refuses the re-stage");
    let FlowError::StageRefused(row) = &second else {
        panic!("the second tick must be held by the memo, not re-staged: {second:?}");
    };
    assert!(
        row.starts_with(crate::state::HELD_FAILED_PREFIX),
        "the canonical held row: {row}"
    );
    assert!(
        row.contains("tree_root mismatch") && row.ends_with("now: aterm pkg install ay"),
        "the row names the verdict and the one retry: {row}"
    );
    assert_eq!(fake.count("ay-18.tar.zst"), 1, "held: nothing was fetched");
    assert!(
        kept.is_file(),
        "held: the archive still waits for the retry"
    );

    // A hold does not age: a memo whose clock says the cooldown lapsed long ago
    // (what ages a sha256 memo out) still holds.
    age_refusal(&memo);
    assert!(
        matches!(
            install(&fake, &layout, &anchor(), &req, fl(0), 0),
            Err(FlowError::StageRefused(_))
        ),
        "a held memo has no cooldown to lapse"
    );

    // THE EXPLICIT DOOR FORGETS IT (`install <p>`, `update --retry`): the next
    // attempt stages again — over the kept archive, so still no download — and
    // reaches the same honest verdict, which re-arms the hold.
    assert_eq!(crate::store::clear_stage_refusals(&layout, "ay"), 1);
    let fourth = install(&fake, &layout, &anchor(), &req, fl(0), 0)
        .expect_err("the signed root is still wrong");
    assert!(
        matches!(
            &fourth,
            FlowError::Stage(StageError::TreeRootMismatch { .. })
        ),
        "a forgotten hold lets the stage run: {fourth:?}"
    );
    assert_eq!(
        fake.count("ay-18.tar.zst"),
        1,
        "the kept archive was reused"
    );
    assert!(
        std::fs::read_to_string(&memo)
            .unwrap()
            .contains("\nhold=1\n"),
        "the hold re-arms on the same verdict"
    );

    // A memo ANOTHER atpkg wrote binds nothing: the fix a held row can get on the
    // client side ships as a new atpkg, which is allowed its one attempt.
    let foreign = std::fs::read_to_string(&memo)
        .unwrap()
        .lines()
        .map(|l| {
            if l.starts_with("client=") {
                format!("client={}+another", aterm_types::version::app_version())
            } else {
                l.to_string()
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
        + "\n";
    std::fs::write(&memo, foreign).unwrap();
    assert!(
        matches!(
            install(&fake, &layout, &anchor(), &req, fl(0), 0),
            Err(FlowError::Stage(StageError::TreeRootMismatch { .. }))
        ),
        "a newer client gets its attempt"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// THE REFETCH BOUND, group lane — the one the fleet meets, because the biggest
/// member of the `rustc` tuple is the multi-gigabyte one. The tuple still aborts (a
/// member that cannot stage aborts the group, as always) and still retries once for
/// free; what stops is paying for the download to be told the same thing a third
/// time, and the abort's own sentence says so.
#[test]
fn a_group_member_s_digest_refusal_is_not_refetched_on_every_tick() {
    let dir = scratch("group-digest-refusal");
    let fake = Counting::of(group_fixture(&dir));
    let layout = layout(&dir);
    poison(&dir.join("trust-4821.tar.zst"));
    let installed = std::collections::BTreeMap::from([("ay".to_string(), 17u64)]);
    let apply = |f: &Counting| {
        apply_channel(
            f,
            &layout,
            &anchor(),
            "stable",
            TRIPLE,
            &installed,
            &[],
            fl(0),
            0,
        )
        .unwrap()
    };

    let first = apply(&fake);
    let (_, outcome) = &first.groups[0];
    assert!(
        matches!(outcome, TxnOutcome::Aborted { failed, .. } if failed == "trust"),
        "reach guard: the tuple aborted on the poisoned member — {outcome:?}"
    );
    assert_eq!(fake.count("trust-4821.tar.zst"), 1);
    assert!(
        layout
            .build_dir("trust", 4821)
            .with_file_name("4821.refused")
            .is_file(),
        "the member's verdict is recorded beside its build"
    );

    // The free first retry: this pass really does fetch the bundle again.
    let second = apply(&fake);
    let (_, outcome) = &second.groups[0];
    assert!(
        matches!(outcome, TxnOutcome::Aborted { failed, .. } if failed == "trust"),
        "the first retry is never refused — {outcome:?}"
    );
    assert_eq!(fake.count("trust-4821.tar.zst"), 2, "it really did retry");

    // The tick after that: the same abort, having moved no bytes for the member.
    let third = apply(&fake);
    let (_, outcome) = &third.groups[0];
    let TxnOutcome::Aborted { failed, why, .. } = outcome else {
        panic!("the tuple still aborts on the member it cannot stage: {outcome:?}");
    };
    assert_eq!(failed, "trust");
    // The door is the pass that moves the tuple whole: `install trust` would land the
    // member alone, beside an `ay` on the old build.
    assert!(
        why.ends_with("now: aterm pkg update --retry") && !why.contains("pkg install"),
        "a tuple member's hold names the group's door: {why}"
    );
    assert_eq!(
        fake.count("trust-4821.tar.zst"),
        2,
        "every six-hourly tick re-downloaded the whole bundle; it must not"
    );
    // The default set's disk check reads the same memo: the tuple aborts at its held
    // member, so it lands nothing — and once the memo is forgotten, its figure is both
    // Install members' bytes, what its own gate sums.
    let index = resolve_verified_index(&fake, &layout, &anchor(), fl(0), 0).unwrap();
    let ch = index.channel_for("stable", TRIPLE).unwrap();
    let group = crate::plan_groups(&index, &ch).remove(0);
    assert_eq!(
        planned_landing(&fake, &layout, &index, &ch, &group, &installed, TRIPLE),
        PlannedLanding::Nothing
    );
    assert_eq!(crate::store::clear_stage_refusals(&layout, "trust"), 1);
    assert_eq!(
        planned_landing(&fake, &layout, &index, &ch, &group, &installed, TRIPLE),
        PlannedLanding::Bytes(LandingBytes {
            asset: 200,
            tree: 2 * 1_048_576
        })
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// The default set's disk check reads a tuple as its transaction does: a sibling's local
/// pin or a revoked pin lands nothing, so the tuple is left out of the set, and what it
/// does count is every member the transaction stages — an installed member whose tree is
/// incomplete for this slice beside the absent one, not the absent one alone.
#[test]
fn a_tuple_s_planned_landing_is_what_its_transaction_stages() {
    let dir = scratch("planned-tuple");
    let fake = group_fixture(&dir);
    let layout = layout(&dir);
    seed_build(&layout, "ay", 18, true);
    let installed = crate::ops::active_builds(&layout);
    let index = resolve_verified_index(&fake, &layout, &anchor(), fl(0), 0).unwrap();
    let ch = index.channel_for("stable", TRIPLE).unwrap();
    let group = crate::plan_groups(&index, &ch).remove(0);
    let planned = || planned_landing(&fake, &layout, &index, &ch, &group, &installed, TRIPLE);
    let bootstrap = || {
        bootstrap_group(
            &fake,
            &layout,
            &index,
            "stable",
            TRIPLE,
            &group,
            &installed,
            &mut BTreeMap::new(),
            &crate::quiet::FlipGate::NOW,
        )
        .unwrap()
        .0
    };
    let one = LandingBytes {
        asset: 100,
        tree: 1_048_576,
    };

    // ay is at its pin and pinned, trust absent: the pin holds the whole tuple.
    crate::pin::set_pinned(&layout, "ay", true).unwrap();
    assert_eq!(planned(), PlannedLanding::Nothing);
    assert_eq!(bootstrap(), TxnOutcome::Pinned(vec!["ay".into()]));
    assert!(!layout.build_dir("trust", 4821).exists());

    crate::pin::set_pinned(&layout, "ay", false).unwrap();
    assert_eq!(planned(), PlannedLanding::Bytes(one), "trust alone");

    // ay's tree is not complete for this slice: the transaction re-stages it with trust.
    std::fs::remove_file(layout.build_dir("ay", 18).with_file_name("18.ready")).unwrap();
    assert_eq!(planned(), PlannedLanding::Bytes(one.and(one)));
    assert_eq!(
        bootstrap(),
        TxnOutcome::Applied(vec!["ay".into(), "trust".into()])
    );

    // A revoked pin tombstones the tuple: nothing lands.
    let other = scratch("planned-tuple-yanked");
    let yanked = group_fixture_yanking_ay18(&other);
    let index = resolve_verified_index(&yanked, &layout, &anchor(), fl(0), 0).unwrap();
    let ch = index.channel_for("stable", TRIPLE).unwrap();
    assert_eq!(
        planned_landing(&yanked, &layout, &index, &ch, &group, &installed, TRIPLE),
        PlannedLanding::Nothing
    );
    let _ = std::fs::remove_dir_all(&dir);
    let _ = std::fs::remove_dir_all(&other);
}

/// The set's peak is what each unit's gate checks in the worst order the priority queue
/// can choose: with that much free (plus the reserve), every order passes every gate —
/// each unit's archives and tree on top of the trees landed before it — and with one
/// byte less some order fails one. Summing the trees alone (the figure the default set
/// once refused on) is short by the largest unit's archives.
#[test]
fn the_set_peak_is_what_every_unit_gate_checks_in_any_order() {
    const MIB: u64 = 1 << 20;
    let units = [
        LandingBytes {
            asset: 1024 * MIB,
            tree: 3072 * MIB,
        },
        LandingBytes {
            asset: 50 * MIB,
            tree: 100 * MIB,
        },
        LandingBytes {
            asset: 400 * MIB,
            tree: 900 * MIB,
        },
    ];
    let floor = crate::cost::FREE_FLOOR;
    let every_order_passes = |have: u64| {
        [
            [0, 1, 2],
            [0, 2, 1],
            [1, 0, 2],
            [1, 2, 0],
            [2, 0, 1],
            [2, 1, 0],
        ]
        .iter()
        .all(|order| {
            let mut landed = 0u64;
            order.iter().all(|&i| {
                let u = units[i];
                let ok = disk_gate(u.need(), Some(have - landed)).is_ok();
                landed += u.tree;
                ok
            })
        })
    };
    let peak = peak_need(units);
    assert_eq!(peak, (3072 + 100 + 900 + 1024) * MIB);
    assert!(every_order_passes(peak + floor));
    assert!(!every_order_passes(peak + floor - 1));
    let trees: u64 = units.iter().map(|u| u.tree).sum();
    assert!(!every_order_passes(trees + floor));
    // A tuple's members land together: their archives coexist.
    assert_eq!(
        units[1].and(units[2]).need(),
        units[1].need() + units[2].need()
    );
    // Two lanes at once hold the two largest archives together; enough lanes, every one.
    assert_eq!(peak_need_at_once(units, 1), peak);
    assert_eq!(peak_need_at_once(units, 2), trees + (1024 + 400) * MIB);
    assert_eq!(
        peak_need_at_once(units, 8),
        units.iter().map(|u| u.need()).sum::<u64>()
    );
}

// --- download ahead ([`ahead`]) and the side-by-side resolve checks ----------------------

/// A signed release pinning `programs` — `(name, build, signed size)` each, one real archive
/// apiece — with the ones `tuple` names in one `rustc` coherence group.
fn ahead_release(dir: &Path, programs: &[(&str, u64, u64)], tuple: &[&str]) -> Fake {
    let mut pkg = HashMap::new();
    let mut archives = HashMap::new();
    let mut named = String::new();
    let mut pins = Vec::new();
    for &(program, build, size) in programs {
        let archive = prog_archive(dir, program, build);
        let sha = crate::tree::file_sha256(&archive).unwrap();
        let root = signed_root_of(&archive, &dir.join(format!("probe-{program}")));
        let asset = format!("{program}-{build}.tar.zst");
        let pkg_body = format!(
            "schema = 2\nprogram = \"{program}\"\nversion = \"0.1\"\nbuild_number = {build}\n\
             exposes = [\"{program}\"]\n\
             [[artifact]]\ntarget = \"{TRIPLE}\"\nkind = \"binary\"\nasset = \"{asset}\"\n\
             sha256 = \"{sha}\"\ntree_root = \"{root}\"\nsize = {size}\n\
             [artifact.cost]\ndisk_installed = 1048576\n"
        );
        pkg.insert(
            (program.to_string(), build),
            (
                pkg_body.clone().into_bytes(),
                sign(&RELEASE_SEED, pkg_body.as_bytes()),
            ),
        );
        archives.insert(asset, archive);
        named.push_str(&format!("[programs.{program}]\nrepo = \"{program}\"\n"));
        if tuple.contains(&program) {
            named.push_str("coherence_group = \"rustc\"\n");
        }
        pins.push(format!("{program} = {build}"));
    }
    let index_body = format!(
        "schema = 2\nindex_build = 41\nvalid_until = \"2026-07-05T12:00:00Z\"\n{attr}{named}\
         [[channels]]\nname = \"stable\"\nchannel_build = 1\nmin_build = 0\n\
         pin = {{ {pins} }}\n",
        attr = attribution(),
        pins = pins.join(", ")
    );
    Fake {
        index: index_body.clone().into_bytes(),
        index_sig: sign(&RELEASE_SEED, index_body.as_bytes()),
        pkg,
        archives,
    }
}

/// The name the set completion gives its download-ahead thread.
const LANE_THREAD: &str = "atpkg-download-ahead";

/// A [`Fake`] whose downloads go into one ordered log beside the test's own marks
/// (`start:<asset>`, `end:<asset>`, and whatever the test notes), each with the thread that
/// asked; a download of the asset `hold` names waits until the test lets it go, one of an
/// asset in `refused` fails as a dropped link does, and the first of an asset in `flaky`
/// fails as a dropped transfer does.
struct Recorder {
    inner: Fake,
    log: std::sync::Mutex<Vec<(String, String)>>,
    moved: std::sync::Condvar,
    hold: std::sync::Mutex<Option<String>>,
    let_go: std::sync::Condvar,
    refused: Vec<String>,
    flaky: std::sync::Mutex<Vec<String>>,
}

impl Recorder {
    fn new(inner: Fake, refused: &[&str]) -> Self {
        Self {
            inner,
            log: std::sync::Mutex::default(),
            moved: std::sync::Condvar::new(),
            hold: std::sync::Mutex::default(),
            let_go: std::sync::Condvar::new(),
            refused: refused.iter().map(|a| (*a).to_string()).collect(),
            flaky: std::sync::Mutex::default(),
        }
    }

    /// The first download of each of `assets` fails.
    fn flaky(self, assets: &[&str]) -> Self {
        *self.flaky.lock().unwrap() = assets.iter().map(|a| (*a).to_string()).collect();
        self
    }

    fn note(&self, event: &str) {
        let thread = std::thread::current().name().unwrap_or("").to_string();
        self.log.lock().unwrap().push((event.to_string(), thread));
        self.moved.notify_all();
    }

    /// Wait for `event` — a one-minute hang detector, not a latency budget.
    fn wait_for(&self, event: &str) {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
        let mut log = self.log.lock().unwrap();
        while !log.iter().any(|(e, _)| e == event) {
            let left = deadline.saturating_duration_since(std::time::Instant::now());
            assert!(!left.is_zero(), "{event} never happened: {log:?}");
            log = self.moved.wait_timeout(log, left).unwrap().0;
        }
    }

    fn release(&self) {
        *self.hold.lock().unwrap() = None;
        self.let_go.notify_all();
    }

    fn at(&self, event: &str) -> usize {
        let log = self.log.lock().unwrap();
        log.iter()
            .position(|(e, _)| e == event)
            .unwrap_or_else(|| panic!("{event} never happened: {log:?}"))
    }

    /// Every download, in the order it started, with the thread that asked.
    fn starts(&self) -> Vec<(String, String)> {
        self.log
            .lock()
            .unwrap()
            .iter()
            .filter_map(|(e, t)| e.strip_prefix("start:").map(|a| (a.to_string(), t.clone())))
            .collect()
    }
}

impl Fetcher for Recorder {
    fn index_candidates(&self) -> Result<Vec<Candidate>, String> {
        self.inner.index_candidates()
    }
    fn pkg_manifest(
        &self,
        repo: &str,
        program: &str,
        build: u64,
    ) -> Result<(Vec<u8>, Vec<u8>), String> {
        self.inner.pkg_manifest(repo, program, build)
    }
    fn download(&self, repo: &str, asset: &str, dest: &Path) -> Result<(), String> {
        self.note(&format!("start:{asset}"));
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
        let mut hold = self.hold.lock().unwrap();
        while hold.as_deref() == Some(asset) {
            let left = deadline.saturating_duration_since(std::time::Instant::now());
            assert!(!left.is_zero(), "{asset} was held for a minute");
            hold = self.let_go.wait_timeout(hold, left).unwrap().0;
        }
        drop(hold);
        let dropped = {
            let mut flaky = self.flaky.lock().unwrap();
            let first = flaky.iter().position(|a| a == asset);
            first.map(|i| flaky.remove(i)).is_some()
        };
        let fetched = if self.refused.iter().any(|a| a == asset) {
            Err(String::from("curl: (7) Failed to connect"))
        } else if dropped {
            Err(String::from(
                "curl: (56) Recv failure: Connection reset by peer",
            ))
        } else {
            self.inner.download(repo, asset, dest)
        };
        self.note(&format!("end:{asset}"));
        fetched
    }
}

/// The lane over `units` of `fetcher`'s release, under `budget`.
fn lane_over(
    fetcher: &Recorder,
    layout: &Layout,
    units: &[(usize, Vec<String>)],
    budget: u64,
) -> std::sync::Arc<ahead::Ahead> {
    let index = resolve_verified_index(fetcher, layout, &anchor(), fl(0), 0).unwrap();
    let ch = index.channel_for("stable", TRIPLE).unwrap();
    ahead::Ahead::plan_within(fetcher, layout, &index, &ch, TRIPLE, units, budget)
        .expect("every member is fetchable ahead")
}

/// Start `lane`'s worker inside `scope`, named as the set completion names it.
fn spawn_lane<'scope, 'env>(
    scope: &'scope std::thread::Scope<'scope, 'env>,
    lane: &'env ahead::Ahead,
    fetcher: &'env Recorder,
    layout: &'env Layout,
) {
    std::thread::Builder::new()
        .name(LANE_THREAD.into())
        .spawn_scoped(scope, move || lane.work(fetcher, layout, &|_| {}))
        .unwrap();
}

fn singles(names: &[&str]) -> Vec<(usize, Vec<String>)> {
    names
        .iter()
        .enumerate()
        .map(|(unit, name)| (unit, vec![(*name).to_string()]))
        .collect()
}

fn archive_at(layout: &Layout, program: &str, build: u64) -> PathBuf {
    staged_download_path(layout, program, &format!("{program}-{build}.tar.zst")).unwrap()
}

// THE NEXT ARCHIVE MOVES WHILE THE PASS HOLDS THE ONE BEFORE. The test is the pass: it takes
// sa from the lane and, before it takes anything else — as if still staging sa — the lane
// is already fetching sb. Every archive is fetched once, on the lane's thread, into the
// staging path the pass then stages from.
#[test]
fn the_lane_fetches_the_next_archive_while_the_pass_holds_the_one_before() {
    let dir = scratch("ahead-overlap");
    let rec = Recorder::new(
        ahead_release(&dir, &[("sa", 1, 100), ("sb", 2, 100), ("sc", 3, 100)], &[]),
        &[],
    );
    let layout = layout(&dir);
    let lane = lane_over(&rec, &layout, &singles(&["sa", "sb", "sc"]), 1 << 20);
    std::thread::scope(|scope| {
        spawn_lane(scope, &lane, &rec, &layout);
        let _entered = lane.enter();
        assert_eq!(
            ahead::claim("sa", &archive_at(&layout, "sa", 1)),
            Some(Ok(()))
        );
        assert!(archive_at(&layout, "sa", 1).is_file());
        rec.wait_for("start:sb-2.tar.zst");
        assert_eq!(
            ahead::claim("sb", &archive_at(&layout, "sb", 2)),
            Some(Ok(()))
        );
        assert_eq!(
            ahead::claim("sc", &archive_at(&layout, "sc", 3)),
            Some(Ok(()))
        );
    });
    assert_eq!(
        rec.starts(),
        [
            ("sa-1.tar.zst".to_string(), LANE_THREAD.to_string()),
            ("sb-2.tar.zst".to_string(), LANE_THREAD.to_string()),
            ("sc-3.tar.zst".to_string(), LANE_THREAD.to_string()),
        ]
    );
    assert!(lane.untaken().is_empty(), "the pass took every archive");
    let _ = std::fs::remove_dir_all(&dir);
}

// THE BUDGET. Under 250 bytes ahead, the lane fetches sa and sb unasked and starts sc only
// once the pass has taken sa; sd, bigger than the whole budget, moves only when the pass asks
// for it. What the lane can hold beyond the pass's unit is the budget.
#[test]
fn the_lane_stays_within_its_budget_and_a_bigger_archive_waits_for_the_pass() {
    let dir = scratch("ahead-budget");
    let rec = Recorder::new(
        ahead_release(
            &dir,
            &[
                ("sa", 1, 100),
                ("sb", 2, 100),
                ("sc", 3, 100),
                ("sd", 4, 300),
            ],
            &[],
        ),
        &[],
    );
    let layout = layout(&dir);
    let lane = lane_over(&rec, &layout, &singles(&["sa", "sb", "sc", "sd"]), 250);
    assert_eq!(lane.reserve(), 250);
    std::thread::scope(|scope| {
        spawn_lane(scope, &lane, &rec, &layout);
        let _entered = lane.enter();
        rec.wait_for("end:sb-2.tar.zst");
        rec.note("claim:sa");
        assert_eq!(
            ahead::claim("sa", &archive_at(&layout, "sa", 1)),
            Some(Ok(()))
        );
        rec.wait_for("end:sc-3.tar.zst");
        assert_eq!(
            ahead::claim("sb", &archive_at(&layout, "sb", 2)),
            Some(Ok(()))
        );
        assert_eq!(
            ahead::claim("sc", &archive_at(&layout, "sc", 3)),
            Some(Ok(()))
        );
        rec.note("claim:sd");
        assert_eq!(
            ahead::claim("sd", &archive_at(&layout, "sd", 4)),
            Some(Ok(()))
        );
    });
    assert!(
        rec.at("start:sc-3.tar.zst") > rec.at("claim:sa"),
        "sc would have put 300 bytes ahead"
    );
    assert!(
        rec.at("start:sd-4.tar.zst") > rec.at("claim:sd"),
        "sd is bigger than the budget"
    );
    assert!(rec.starts().iter().all(|(_, thread)| thread == LANE_THREAD));
    let _ = std::fs::remove_dir_all(&dir);
}

/// A legacy release row may omit its compressed size, but zero is an unknown
/// download cap, not a zero-byte archive. If one member's size is unknown,
/// the whole tuple stays off the ahead lane and still installs inline. A
/// separate sized unit remains eligible for the worker.
#[test]
fn a_tuple_with_an_unknown_archive_size_downloads_inline_without_disabling_sized_units() {
    let dir = scratch("ahead-unknown-size");
    let rec = Recorder::new(
        ahead_release(
            &dir,
            &[("ta", 1, 100), ("tb", 2, 0), ("sc", 3, 100)],
            &["ta", "tb"],
        ),
        &[],
    );
    let layout = layout(&dir);
    let index = resolve_verified_index(&rec, &layout, &anchor(), fl(0), 0).unwrap();
    let ch = index.channel_for("stable", TRIPLE).unwrap();
    let group = crate::plan_groups(&index, &ch)
        .into_iter()
        .find(|group| group.group.as_deref() == Some("rustc"))
        .unwrap();
    let lane = ahead::Ahead::plan_within(
        &rec,
        &layout,
        &index,
        &ch,
        TRIPLE,
        &[(0, group.members.clone()), (1, vec!["sc".to_string()])],
        1 << 20,
    )
    .expect("the sized singleton is eligible");
    std::thread::scope(|scope| {
        spawn_lane(scope, &lane, &rec, &layout);
        let _entered = lane.enter();
        rec.wait_for("end:sc-3.tar.zst");
        assert_eq!(
            rec.starts(),
            [("sc-3.tar.zst".to_string(), LANE_THREAD.to_string())],
            "neither member of the unknown-size tuple is fetched ahead"
        );
        let (outcome, _) = bootstrap_group(
            &rec,
            &layout,
            &index,
            "stable",
            TRIPLE,
            &group,
            &BTreeMap::new(),
            &mut BTreeMap::new(),
            &crate::quiet::FlipGate::NOW,
        )
        .unwrap();
        assert_eq!(outcome, TxnOutcome::Applied(vec!["ta".into(), "tb".into()]));
        assert_eq!(
            ahead::claim("sc", &archive_at(&layout, "sc", 3)),
            Some(Ok(()))
        );
    });
    for (program, build) in [("ta", 1), ("tb", 2)] {
        assert_eq!(
            crate::ops::which(&layout, program).unwrap(),
            tool_bin(&layout.build_dir(program, build), program)
        );
    }
    let starts = rec.starts();
    assert_eq!(starts.len(), 3, "every archive once: {starts:?}");
    assert!(starts[1..].iter().all(|(_, thread)| thread != LANE_THREAD));
    let _ = std::fs::remove_dir_all(&dir);
}

// THE BUMP MOVES ONLY WHAT HAS NOT STARTED. A person bumps sb and sd while sb downloads: sb
// is not restarted, and sd — not started — comes before sc, the order the pass takes too.
#[test]
fn a_bump_reorders_only_the_archives_the_lane_has_not_started() {
    let dir = scratch("ahead-bump");
    let rec = Recorder::new(
        ahead_release(
            &dir,
            &[
                ("sa", 1, 100),
                ("sb", 2, 100),
                ("sc", 3, 100),
                ("sd", 4, 100),
            ],
            &[],
        ),
        &[],
    );
    *rec.hold.lock().unwrap() = Some("sb-2.tar.zst".to_string());
    let layout = layout(&dir);
    let lane = lane_over(&rec, &layout, &singles(&["sa", "sb", "sc", "sd"]), 1 << 20);
    std::thread::scope(|scope| {
        spawn_lane(scope, &lane, &rec, &layout);
        let _entered = lane.enter();
        rec.wait_for("start:sb-2.tar.zst");
        for name in ["sb", "sd"] {
            crate::progress::append_bump(
                &layout,
                &ToolName::new(name).unwrap(),
                crate::progress::Asker::Person,
            )
            .unwrap();
        }
        rec.release();
        // The lane's own next pick, before the pass asks for anything after sb.
        rec.wait_for("start:sd-4.tar.zst");
        for (program, build) in [("sa", 1), ("sb", 2), ("sd", 4), ("sc", 3)] {
            assert_eq!(
                ahead::claim(program, &archive_at(&layout, program, build)),
                Some(Ok(()))
            );
        }
    });
    let order: Vec<String> = rec.starts().into_iter().map(|(asset, _)| asset).collect();
    assert_eq!(
        order,
        [
            "sa-1.tar.zst",
            "sb-2.tar.zst",
            "sd-4.tar.zst",
            "sc-3.tar.zst"
        ]
    );
    let _ = std::fs::remove_dir_all(&dir);
}

// THE PASS'S OWN PIN WINS. A singleton's own resolve found another build of sa than the plan
// did: the lane's sa is not the pass's archive, so the claim takes nothing — and the lane
// never starts sa afterwards, beside the pass's own download into sa's staging directory,
// where each download's partial sweep would take the other's `.part`.
#[test]
fn a_claim_for_another_build_takes_nothing_and_that_download_never_starts() {
    let dir = scratch("ahead-other-pin");
    let rec = Recorder::new(
        ahead_release(&dir, &[("sb", 2, 100), ("sa", 1, 100)], &[]),
        &[],
    );
    *rec.hold.lock().unwrap() = Some("sb-2.tar.zst".to_string());
    let layout = layout(&dir);
    let lane = lane_over(&rec, &layout, &singles(&["sb", "sa"]), 1 << 20);
    std::thread::scope(|scope| {
        let worker = std::thread::Builder::new()
            .name(LANE_THREAD.into())
            .spawn_scoped(scope, || lane.work(&rec, &layout, &|_| {}))
            .unwrap();
        let entered = lane.enter();
        rec.wait_for("start:sb-2.tar.zst");
        assert_eq!(ahead::claim("sa", &archive_at(&layout, "sa", 9)), None);
        rec.release();
        assert_eq!(
            ahead::claim("sb", &archive_at(&layout, "sb", 2)),
            Some(Ok(()))
        );
        // The lane ends on its own, still entered and never halted: nothing is left for it.
        worker.join().unwrap();
        drop(entered);
    });
    let order: Vec<String> = rec.starts().into_iter().map(|(asset, _)| asset).collect();
    assert_eq!(order, ["sb-2.tar.zst"], "sa is the pass's own download now");
    assert!(!archive_at(&layout, "sa", 1).exists());
    let _ = std::fs::remove_dir_all(&dir);
}

// A FETCHED-AHEAD ARCHIVE WAITS AT ITS WHOLE SIZE. The lane's download is complete, the pass
// has not staged it: its progress row reads every byte in — what a reader takes for "waiting
// for the pass", never a download stuck short of its end (the `.part` poller's last look).
#[test]
fn a_download_the_lane_finished_waits_on_its_row_at_its_whole_size() {
    let _gate = crate::progress::PASS_TEST_GATE
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let dir = scratch("ahead-row");
    let rec = Recorder::new(ahead_release(&dir, &[("sa", 1, 100)], &[]), &[]);
    let layout = layout(&dir);
    std::fs::create_dir_all(&layout.prefix).unwrap();
    let lane = lane_over(&rec, &layout, &singles(&["sa"]), 1 << 20);
    assert!(crate::progress::begin_pass(
        &layout.prefix.join("progress.json"),
        "net"
    ));
    if let Some(sink) = crate::progress::active() {
        sink.plan(&[("sa".to_string(), 100)]);
    }
    std::thread::scope(|scope| {
        let lent = crate::progress::lend();
        std::thread::Builder::new()
            .name(LANE_THREAD.into())
            .spawn_scoped(scope, || {
                let _adopted = lent.adopt();
                lane.work(&rec, &layout, &|_| {});
            })
            .unwrap();
        let _entered = lane.enter();
        assert_eq!(
            ahead::claim("sa", &archive_at(&layout, "sa", 1)),
            Some(Ok(()))
        );
        let row = crate::progress::live_snapshot().unwrap().programs["sa"].clone();
        assert!(row.downloaded(), "{row:?}");
        assert_eq!((row.bytes_done, row.bytes_total), (100, 100));
    });
    crate::progress::end_pass();
    let _ = std::fs::remove_dir_all(&dir);
}

/// The `rustc` tuple of `fetcher`'s release, bootstrapped with the lane over it entered.
fn bootstrap_through_lane(
    fetcher: &Recorder,
    layout: &Layout,
    resolved: &mut BTreeMap<String, String>,
) -> (TxnOutcome, std::sync::Arc<ahead::Ahead>) {
    let index = resolve_verified_index(fetcher, layout, &anchor(), fl(0), 0).unwrap();
    let ch = index.channel_for("stable", TRIPLE).unwrap();
    let group = crate::plan_groups(&index, &ch)
        .into_iter()
        .find(|g| g.group.as_deref() == Some("rustc"))
        .unwrap();
    let lane = ahead::Ahead::plan(
        fetcher,
        layout,
        &index,
        &ch,
        TRIPLE,
        &[(0, group.members.clone())],
    )
    .expect("every member is fetchable ahead");
    let outcome = std::thread::scope(|scope| {
        spawn_lane(scope, &lane, fetcher, layout);
        let _entered = lane.enter();
        let (outcome, _) = bootstrap_group(
            fetcher,
            layout,
            &index,
            "stable",
            TRIPLE,
            &group,
            &BTreeMap::new(),
            resolved,
            &crate::quiet::FlipGate::NOW,
        )
        .unwrap();
        lane.passed(0);
        outcome
    });
    (outcome, lane)
}

// A TUPLE STAGES WHAT THE LANE FETCHED. Each member's archive comes off the lane, once, and
// the transaction stages, flips and reclaims exactly as with its own downloads.
#[test]
fn a_tuple_lands_whole_from_the_archives_the_lane_fetched() {
    let dir = scratch("ahead-tuple");
    let rec = Recorder::new(
        ahead_release(
            &dir,
            &[("ta", 1, 100), ("tb", 2, 100), ("tc", 3, 100)],
            &["ta", "tb", "tc"],
        ),
        &[],
    );
    let layout = layout(&dir);
    let mut resolved = BTreeMap::new();
    let (outcome, lane) = bootstrap_through_lane(&rec, &layout, &mut resolved);
    assert_eq!(
        outcome,
        TxnOutcome::Applied(vec!["ta".into(), "tb".into(), "tc".into()])
    );
    for (program, build) in [("ta", 1u64), ("tb", 2), ("tc", 3)] {
        assert_eq!(
            crate::ops::which(&layout, program).unwrap(),
            tool_bin(&layout.build_dir(program, build), program)
        );
        assert!(
            !archive_at(&layout, program, build).exists(),
            "{program}'s archive is reclaimed once the tuple lands"
        );
    }
    let starts = rec.starts();
    assert_eq!(starts.len(), 3, "each archive once: {starts:?}");
    assert!(starts.iter().all(|(_, thread)| thread == LANE_THREAD));
    assert!(lane.untaken().is_empty());
    let _ = std::fs::remove_dir_all(&dir);
}

// A DOWNLOAD LOST BY THE LANE AND BY THE PASS ABORTS THE TUPLE, AND IS THE PASS'S FETCH
// FAULT. The third member's archive does not arrive, the lane's download nor the pass's own
// after it: nothing is flipped, the staged siblings' trees are discarded (their archives
// kept for the next pass), and the pass's own fault tally — the offline verdict's input —
// counts the one fetch of its own that failed.
#[test]
fn a_tuple_whose_third_archive_the_lane_lost_aborts_with_nothing_flipped() {
    let dir = scratch("ahead-tuple-lost");
    let rec = Recorder::new(
        ahead_release(
            &dir,
            &[("ta", 1, 100), ("tb", 2, 100), ("tc", 3, 100)],
            &["ta", "tb", "tc"],
        ),
        &["tc-3.tar.zst"],
    );
    let layout = layout(&dir);
    let mut resolved = BTreeMap::new();
    let faults = fetch_faults();
    let (outcome, _) = bootstrap_through_lane(&rec, &layout, &mut resolved);
    assert!(
        matches!(
            &outcome,
            TxnOutcome::Aborted { failed, during_flip: false, .. } if failed == "tc"
        ),
        "{outcome:?}"
    );
    assert_eq!(fetch_faults().wrapping_sub(faults), 1, "one fetch fault");
    for (program, build) in [("ta", 1u64), ("tb", 2), ("tc", 3)] {
        assert!(
            !layout.build_dir(program, build).exists(),
            "{program} staged"
        );
        assert!(
            std::fs::symlink_metadata(layout.program_current(program)).is_err(),
            "{program} flipped"
        );
    }
    for (program, build) in [("ta", 1u64), ("tb", 2)] {
        assert!(
            archive_at(&layout, program, build).is_file(),
            "{program}'s verified archive waits for the next pass"
        );
    }
    assert!(!archive_at(&layout, "tc", 3).exists());
    let _ = std::fs::remove_dir_all(&dir);
}

// A DOWNLOAD ONLY THE LANE LOST IS THE PASS'S TO FETCH, AND THE TUPLE LANDS WHOLE. The lane's
// download of the third member's archive is cut short (a dropped transfer); the pass, reaching
// that member, fetches the archive itself, and the tuple lands in this pass with no fault
// counted — where a lane whose failure stood in for the pass's download aborted it.
#[test]
fn a_tuple_whose_archive_only_the_lane_lost_lands_whole() {
    let dir = scratch("ahead-tuple-lost-once");
    let rec = Recorder::new(
        ahead_release(
            &dir,
            &[("ta", 1, 100), ("tb", 2, 100), ("tc", 3, 100)],
            &["ta", "tb", "tc"],
        ),
        &[],
    )
    .flaky(&["tc-3.tar.zst"]);
    let layout = layout(&dir);
    let mut resolved = BTreeMap::new();
    let faults = fetch_faults();
    let (outcome, lane) = bootstrap_through_lane(&rec, &layout, &mut resolved);
    assert_eq!(
        outcome,
        TxnOutcome::Applied(vec!["ta".into(), "tb".into(), "tc".into()])
    );
    assert_eq!(fetch_faults().wrapping_sub(faults), 0, "no fetch fault");
    assert!(
        lane.all_fetched(&[0]),
        "the pass's own download of tc counts it in: the agents' gate opens on it"
    );
    let tc: Vec<String> = rec
        .starts()
        .into_iter()
        .filter(|(asset, _)| asset == "tc-3.tar.zst")
        .map(|(_, thread)| thread)
        .collect();
    assert_eq!(tc.len(), 2, "the lane's download, then the pass's: {tc:?}");
    assert_eq!(tc[0], LANE_THREAD);
    assert_ne!(tc[1], LANE_THREAD, "the pass fetched it itself");
    let _ = std::fs::remove_dir_all(&dir);
}

// A SINGLETON'S ARCHIVE ONLY THE LANE LOST IS THE PASS'S TO FETCH, AND THEN IN. The lane's
// download of sa is cut short and the lane goes on to sb; the pass's own install of sa fetches
// the archive itself, and the moment it is in, the lane holds every archive in — what opens
// the agents' early gate on a first run.
#[test]
fn a_singleton_whose_archive_only_the_lane_lost_installs_and_counts_in() {
    let dir = scratch("ahead-single-lost-once");
    let rec = Recorder::new(
        ahead_release(&dir, &[("sa", 1, 100), ("sb", 2, 100)], &[]),
        &[],
    )
    .flaky(&["sa-1.tar.zst"]);
    let layout = layout(&dir);
    let lane = lane_over(&rec, &layout, &singles(&["sa", "sb"]), 1 << 20);
    std::thread::scope(|scope| {
        spawn_lane(scope, &lane, &rec, &layout);
        let _entered = lane.enter();
        rec.wait_for("end:sb-2.tar.zst");
        assert!(!lane.all_fetched(&[0, 1]), "sa is not in");
        let req = InstallRequest {
            channel: "stable",
            program: "sa",
            triple: TRIPLE,
            installed: None,
        };
        let report = install(&rec, &layout, &anchor(), &req, fl(0), 0).unwrap();
        assert_eq!(report.build, 1);
        assert!(
            lane.all_fetched(&[0, 1]),
            "the pass's own download of sa counts it in"
        );
    });
    let sa: Vec<String> = rec
        .starts()
        .into_iter()
        .filter(|(asset, _)| asset == "sa-1.tar.zst")
        .map(|(_, thread)| thread)
        .collect();
    assert_eq!(sa.len(), 2, "the lane's download, then the pass's: {sa:?}");
    assert_eq!(sa[0], LANE_THREAD);
    assert_ne!(sa[1], LANE_THREAD, "the pass fetched it itself");
    let _ = std::fs::remove_dir_all(&dir);
}

// THE LANE SAYS WHEN EVERY ARCHIVE IS IN, NOT WHEN THE PASS HAS STAGED THEM. The pass takes
// nothing here: asked after each download that lands, the lane says the set is in only once
// sc — its last archive — is, never after ta or tb alone. A unit the lane holds no archive of
// is never in, nor is one whose archive the pass fetched itself while the lane was busy:
// only the pass knows when that is. An archive the lane's download lost is in once the
// pass's own download of it is, and the lane says so then.
#[test]
fn the_lane_says_every_archive_is_in_the_moment_its_last_download_lands() {
    let release = |dir: &Path| {
        ahead_release(
            dir,
            &[("ta", 1, 100), ("tb", 2, 100), ("sc", 3, 100)],
            &["ta", "tb"],
        )
    };
    let units = || {
        vec![
            (0, vec!["ta".to_string(), "tb".to_string()]),
            (1, vec!["sc".to_string()]),
        ]
    };
    let dir = scratch("ahead-all-in");
    let rec = Recorder::new(release(&dir), &[]);
    let layout = layout(&dir);
    let lane = lane_over(&rec, &layout, &units(), 1 << 20);
    std::thread::scope(|scope| {
        std::thread::Builder::new()
            .name(LANE_THREAD.into())
            .spawn_scoped(scope, || {
                lane.work(&rec, &layout, &|lane| {
                    rec.note(if lane.all_fetched(&[0]) {
                        "in"
                    } else {
                        "short"
                    });
                });
            })
            .unwrap();
        let _entered = lane.enter();
        rec.wait_for("in");
    });
    let said: Vec<String> = rec
        .log
        .lock()
        .unwrap()
        .iter()
        .map(|(event, _)| event.clone())
        .filter(|event| !event.starts_with("start:"))
        .collect();
    assert_eq!(
        said,
        [
            "end:ta-1.tar.zst",
            "short",
            "end:tb-2.tar.zst",
            "short",
            "end:sc-3.tar.zst",
            "in"
        ]
    );
    assert!(
        !lane.all_fetched(&[0, 2]),
        "a unit the lane holds nothing of"
    );

    // tb's download fails: the lane goes on to sc, and the set is not in until the pass's own
    // download of tb is — the moment it is, the lane says so.
    let lost = scratch("ahead-all-in-lost");
    let rec = Recorder::new(release(&lost), &[]).flaky(&["tb-2.tar.zst"]);
    let layout = self::layout(&lost);
    let lane = lane_over(&rec, &layout, &units(), 1 << 20);
    std::thread::scope(|scope| {
        std::thread::Builder::new()
            .name(LANE_THREAD.into())
            .spawn_scoped(scope, || {
                lane.work(&rec, &layout, &|lane| {
                    rec.note(if lane.all_fetched(&[0]) {
                        "in"
                    } else {
                        "short"
                    });
                });
            })
            .unwrap();
        let _entered = lane.enter();
        rec.wait_for("end:sc-3.tar.zst");
        let tb = archive_at(&layout, "tb", 2);
        assert_eq!(ahead::claim("tb", &tb), None, "tb is the pass's to fetch");
        assert!(!lane.all_fetched(&[0]));
        rec.download("tb", "tb-2.tar.zst", &tb).unwrap();
        ahead::fetched_inline("tb", &tb);
        rec.wait_for("in");
    });
    assert!(rec.at("end:tb-2.tar.zst") > rec.at("end:ta-1.tar.zst"));
    assert!(rec.at("end:sc-3.tar.zst") < rec.at("in"));
    assert!(lane.all_fetched(&[0]));

    // The lane is busy with ta when the pass asks for tb: the pass fetches tb itself.
    let inline = scratch("ahead-all-in-inline");
    let rec = Recorder::new(release(&inline), &[]);
    *rec.hold.lock().unwrap() = Some("ta-1.tar.zst".to_string());
    let layout = self::layout(&inline);
    let lane = lane_over(&rec, &layout, &units(), 1 << 20);
    std::thread::scope(|scope| {
        spawn_lane(scope, &lane, &rec, &layout);
        let _entered = lane.enter();
        rec.wait_for("start:ta-1.tar.zst");
        assert_eq!(ahead::claim("tb", &archive_at(&layout, "tb", 2)), None);
        rec.release();
        rec.wait_for("end:sc-3.tar.zst");
        assert!(!lane.all_fetched(&[0]), "tb is the pass's to fetch");
    });
    for dir in [dir, lost, inline] {
        let _ = std::fs::remove_dir_all(&dir);
    }
}

// THE RESOLVE CHECKS RUN SIDE BY SIDE, AND ANY SIGNAL DEATH REFUSES. Every native binary the
// bundle exposes is checked (a script is not); one death among five refuses the bundle, and
// with two the verdict names the first in `exposes` order, as when they ran in turn.
#[test]
fn every_resolve_check_runs_and_a_signal_death_in_any_refuses() {
    let dir = scratch("resolve-side-by-side");
    let build = dir.join("b");
    std::fs::create_dir_all(build.join("bin")).unwrap();
    let names = ["t1", "t2", "t3", "t4", "t5"];
    for name in names {
        // MH_MAGIC_64, little-endian: a native object to the sniff.
        std::fs::write(
            build.join("bin").join(name),
            [0xcf, 0xfa, 0xed, 0xfe, 0, 0, 0, 0],
        )
        .unwrap();
    }
    std::fs::write(build.join("bin/wrap"), b"#!/bin/sh\nexec t1 \"$@\"\n").unwrap();
    let mut exposed: Vec<String> = names.iter().map(|n| (*n).to_string()).collect();
    exposed.push("wrap".into());
    let exposes = crate::store::split_exposed(&exposed).0;
    let verdict = |killed: &[&str]| {
        let checked = std::sync::Mutex::new(Vec::new());
        let result = bundle_resolve_check_with(&build, &exposes, &|bin: &Path| {
            let name = bin.file_name().unwrap().to_string_lossy().into_owned();
            checked.lock().unwrap().push(name.clone());
            if killed.contains(&name.as_str()) {
                Err(format!("resolve check: {name} was killed by a signal"))
            } else {
                Ok(())
            }
        });
        let mut checked = checked.into_inner().unwrap();
        checked.sort();
        assert_eq!(checked, names, "every native binary, and only those");
        result
    };
    assert!(verdict(&[]).is_ok());
    assert!(
        matches!(verdict(&["t3"]), Err(FlowError::Activate(why)) if why.contains("t3 was killed"))
    );
    assert!(
        matches!(verdict(&["t4", "t2"]), Err(FlowError::Activate(why)) if why.contains("t2 was killed"))
    );
    let _ = std::fs::remove_dir_all(&dir);
}

// A SYMLINKED BIN IS CHECKED AS THE OBJECT IT REACHES: `bin/<tool>` linking to a native
// object elsewhere in the build is checked (under its own exposed name), one linking to a
// script is not, and one that dangles is not.
#[cfg(unix)]
#[test]
fn the_resolve_check_follows_a_symlinked_bin_to_its_object() {
    use std::os::unix::fs::symlink;
    let dir = scratch("resolve-symlinked");
    let build = dir.join("b");
    std::fs::create_dir_all(build.join("bin")).unwrap();
    std::fs::create_dir_all(build.join("libexec")).unwrap();
    std::fs::write(
        build.join("libexec/object"),
        [0xcf, 0xfa, 0xed, 0xfe, 0, 0, 0, 0],
    )
    .unwrap();
    std::fs::write(build.join("libexec/script"), b"#!/bin/sh\n").unwrap();
    symlink("../libexec/object", build.join("bin/linked")).unwrap();
    symlink("../libexec/script", build.join("bin/wrap")).unwrap();
    symlink("../libexec/gone", build.join("bin/dangling")).unwrap();
    let exposes = crate::store::split_exposed(&[
        "linked".to_string(),
        "wrap".to_string(),
        "dangling".to_string(),
    ])
    .0;
    let checked = std::sync::Mutex::new(Vec::new());
    bundle_resolve_check_with(&build, &exposes, &|bin: &Path| {
        checked.lock().unwrap().push(bin.to_path_buf());
        Ok(())
    })
    .unwrap();
    assert_eq!(checked.into_inner().unwrap(), [build.join("bin/linked")]);
    let _ = std::fs::remove_dir_all(&dir);
}

/// THE LINK GOES IN THE ORDER THE PROGRAMS WERE TAKEN, not in the order their landings reach
/// the transfer: a later ticket is not next while an earlier one — taken first, its landing
/// perhaps still short of its transfer — is open, and is next once that one is released or
/// dropped (its transfer over, or its landing ended or moved no byte), whichever ticket gave
/// up first; a transfer waiting on it starts then.
#[test]
fn a_later_download_ticket_waits_for_every_earlier_one() {
    let turns = DownloadTurns::default();
    let claude = turns.ticket();
    let codex = turns.ticket();
    let agy = turns.ticket();
    let next = |ticket: &DownloadTicket<'_>| {
        is_next(
            &turns
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner),
            ticket.n,
        )
    };
    assert!(next(&claude) && !next(&codex) && !next(&agy));
    agy.release();
    assert!(
        !next(&codex),
        "a later ticket giving up moves nobody ahead of claude"
    );
    drop(claude);
    assert!(next(&codex), "claude's landing ended: codex is next");
    let muse = turns.ticket();
    assert!(!next(&muse));
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::scope(|scope| {
        scope.spawn(|| {
            muse.wait();
            let _ = tx.send(());
        });
        codex.release();
        rx.recv_timeout(aterm_spec::wait::MUST_HAPPEN)
            .expect("hang detector: muse's transfer never started after codex's");
    });
}
