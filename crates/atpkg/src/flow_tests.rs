// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::*;
use ring::signature::{Ed25519KeyPair, KeyPair};
use std::collections::HashMap;
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;

use crate::sig::testkit;

/// The synthetic paper master (the root of trust) and the one machine its roster
/// authorizes: the pair the whole crate signs with.
const ROOT_SEED: [u8; 32] = testkit::MASTER_SEED;
const RELEASE_SEED: [u8; 32] = testkit::MACHINE_SEED;

/// A durable build floor of `index_build`, recorded under the generation these fixtures
/// publish at so it binds (a floor stamped with another generation is waived, and a test
/// that built one would pass vacuously). `fl(0)` records nothing.
fn fl(index_build: u64) -> BuildFloor {
    BuildFloor {
        index_build,
        roster_seq: testkit::SEQ,
    }
}

/// The anchor every flow test resolves under: the synthetic master, roster floor 0.
/// `anchor_of` arms a different master for the negative tests.
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

/// The archive member a build's `bin/<name>` executable is: `bin/<name>` on Unix,
/// `bin/<name>.exe` on Windows ([`tool_bin`]'s file). Spelled with the suffix, not
/// [`ToolName::exe_file`], so a refused name (`git`) can be laid too.
fn bin_entry(name: &str) -> String {
    format!("bin/{name}{}", crate::platform::EXE_SUFFIX)
}

/// A raw USTAR + zstd archive with `bin/ay` (+ a sensitive `bin/git` to prove the shim
/// gate). Returns the path.
fn make_archive(dir: &Path) -> PathBuf {
    make_archive_with(dir, b"#!/bin/true\nay", b"0000755\0")
}

/// As [`make_archive`], but with explicit `bin/ay` content and tar mode (the resolve-check
/// tests ship a native-object magic with no exec bit, so the spawn fails).
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
    tar.extend(entry(&bin_entry("ay"), ay_content, ay_mode));
    tar.extend(entry(
        &bin_entry("git"),
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
        // The master-signed roster is published with the index, as a real release carries
        // `aterm-machines.toml` beside `index.toml`.
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

/// As [`fixture`], but with an explicit artifact `kind` (`app-bundle`, `sysroot-bundle`)
/// for the per-kind dispatch.
fn fixture_with_kind(dir: &Path, kind: &str) -> Fake {
    fixture_from(dir, kind, make_archive(dir), None)
}

/// As [`fixture_with_kind`], but with explicit `bin/ay` content + tar mode (the
/// resolve-check rollback tests ship a native-object magic that cannot spawn).
#[cfg(unix)]
fn fixture_with(dir: &Path, kind: &str, ay_content: &[u8], ay_mode: &[u8; 8]) -> Fake {
    fixture_from(dir, kind, make_archive_with(dir, ay_content, ay_mode), None)
}

/// The `tree_root` a producer signs for `archive`: a throwaway stage into `probe` through
/// the extractor's own fold, which on Windows folds the declared modes (`crate::tree`'s
/// module docs) where the plain on-disk walk folds `0`. On Unix the two agree.
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
/// `signed_root` overrides the manifest's `tree_root`: `None` is the real root; `Some(..)`
/// makes the signed value disagree with the bytes, the only way to reach the apply-time
/// re-verify through the real flow (`sha256` covers the compressed asset, so no substituted
/// archive passes that gate and fails a later one).
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

/// A signed release whose one artifact row carries the archive's real digests
/// (sha256/tree_root/size) plus `row` verbatim (the `kind`/`protocol` pair and the vendor
/// keys under test), so an admitted row stages through a fetcher serving `download_url`.
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

/// The vendor row every vendor-lane test starts from: a binary over https from a real
/// allow-listed host on the tar-zst lane (the archive fixture is a tar.zst), no raw-binary
/// `entry`.
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

/// The sidecar a failed stage wrote goes with the build that never came to exist, and a
/// build the failed stage left standing keeps its own: its shims still need their
/// environment.
#[test]
fn a_failed_stage_takes_its_sidecar_and_spares_a_standing_build_s() {
    let dir = scratch("orphan-sidecar");
    let l = layout(&dir);
    let env = crate::shim_env::ShimEnv::admit(&["DISABLE_AUTOUPDATER=1".to_string()]).unwrap();
    // The reference `https` row: its signed sha256 is not this archive's, so the stage
    // refuses before any directory is created.
    let art = crate::vendor::testkit::row();
    let archive = dir.join("asset");
    std::fs::write(&archive, b"not the signed bytes").unwrap();

    // Fresh install: the lane writes the sidecar (creating `store/claude/`), the stage fails,
    // no build directory exists, and the sidecar must not survive.
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

// `ay` installs end to end from a synthetic signed release: index verified under the root
// key, pkg under the delegated release key, sha256 and tree_root checked, then activated and
// shimmed (the sensitive `git` refused).
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

/// THE ANTI-REPLAY BIND (§4.2) at every manifest read: a GENUINELY signed manifest served
/// for a request it does not name — another build of the program, older or newer, or
/// another program at that build — is refused by the singleton install, the group stage
/// and the root recovery alike, and nothing is staged.
#[test]
fn a_signed_manifest_served_for_another_request_is_refused_at_every_read() {
    let control = scratch("pkg-bind-control");
    let honest = fixture(&control);
    let index = resolve_verified_index(&honest, &layout(&control), &anchor(), fl(0), 0).unwrap();
    assert!(
        signed_root_for_installed(&honest, &index, "ay", 18, TRIPLE).is_some(),
        "control: the honest manifest recovers its root"
    );
    let _ = std::fs::remove_dir_all(&control);
    for (from, to) in [
        ("build_number = 18", "build_number = 17"),
        ("build_number = 18", "build_number = 19"),
        ("program = \"ay\"", "program = \"ny\""),
    ] {
        let dir = scratch("pkg-bind");
        let (one, many) = (dir.join("one"), dir.join("group"));
        std::fs::create_dir_all(&one).unwrap();
        std::fs::create_dir_all(&many).unwrap();
        let (mut single, mut group) = (fixture(&one), group_fixture(&many));
        for fake in [&mut single, &mut group] {
            let key = ("ay".to_string(), 18u64);
            let body = String::from_utf8(fake.pkg[&key].0.clone()).unwrap();
            let body = body.replace(from, to);
            let sig = sign(&RELEASE_SEED, body.as_bytes());
            fake.pkg.insert(key, (body.into_bytes(), sig));
        }
        let layout = layout(&dir);
        let req = InstallRequest {
            channel: "stable",
            program: "ay",
            triple: TRIPLE,
            installed: None,
        };
        let err = install(&single, &layout, &anchor(), &req, fl(0), 0).unwrap_err();
        assert!(matches!(err, FlowError::Mismatch), "{to}: {err:?}");
        let index = resolve_verified_index(&single, &layout, &anchor(), fl(0), 0).unwrap();
        assert_eq!(
            signed_root_for_installed(&single, &index, "ay", 18, TRIPLE),
            None,
            "{to}"
        );
        let installed = std::collections::BTreeMap::from([("ay".to_string(), 17u64)]);
        let report = apply_channel(
            &group,
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
            matches!(&report.groups[0].1, TxnOutcome::Aborted { failed, .. } if failed == "ay"),
            "{to}: {:?}",
            report.groups[0].1
        );
        assert!(!layout.build_dir("ay", 18).exists(), "{to}: nothing staged");
        let _ = std::fs::remove_dir_all(&dir);
    }
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
/// under umask `000`, the end-to-end install plus a shim-env sidecar and a stage-refusal
/// memo leave nothing in the prefix writable by group or others. Without the env it does
/// nothing, since the umask is the whole process's.
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
    // writer of its program's directory: a shim-env sidecar and a refused stage's memo.
    let env = crate::shim_env::ShimEnv::admit(&["DISABLE_AUTOUPDATER=1".to_string()]).unwrap();
    crate::shim_env::write_sidecar(&layout.build_dir("claude", 2_026_091_301), &env).unwrap();
    let digest = (crate::manifest::DigestAlg::Sha256, "aa");
    crate::store::record_stage_refusal(&layout.build_dir("nn", 3), digest, "bb", "probe", 0)
        .unwrap();
    let mut loose = Vec::new();
    writable_by_others(&layout.prefix, &layout.prefix, &mut loose);
    assert!(loose.is_empty(), "laid writable by others: {loose:#?}");
}

/// An install's store must not depend on the umask it runs under: in a root-staged system
/// prefix, a `002`/`000` umask would leave `store/`, build roots, `staging/` and the records
/// beside a build writable by group or others, so another user could swap a build's `bin/`,
/// the asset between its digest check and extraction, or a record a later verb believes.
/// The umask is process-wide, so the install runs in a child that runs only
/// [`probe_an_install_under_an_open_umask`].
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
#[test]
fn install_phases_land_in_the_progress_file() {
    // A process runs one pass at a time; this test's is its own, beside the
    // others' (`progress::own_pass_slot`).
    let _slot = crate::progress::own_pass_slot();
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

/// Through the real install flow: a re-install that fails to stage (a live `ay@18`, then a
/// signed `tree_root` that does not describe the bytes) leaves the toolchain installed,
/// complete and on PATH. `install.rs` pins the staging chain itself.
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

    // The same archive bytes (the sha256 gate passes, so the failure lands late) under a
    // signed tree_root that cannot match: the apply-time re-verify.
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

// A failed stage's compressed asset is retained (one copy, reused) when the bytes were
// right, so a stage that failed for another reason does not re-download it every pass; it
// is reclaimed with its `.part` when they were wrong. `sweep_foreign_partials` still keeps
// at most one archive per program.
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

    // Non-vacuity and the reuse itself: the good row has the same bytes under the right
    // root, so the retained archive is what it stages, and success leaves staging empty.
    install(&fixture(&dir), &layout, &anchor(), &req, fl(0), 0).unwrap();
    assert!(staged_assets(&layout, "ay").is_empty());
    assert!(crate::store::build_is_complete(&layout.build_dir("ay", 18)));
    let _ = std::fs::remove_dir_all(&dir);
}

/// A signer refusal is handled as a digest refusal: the archive and its `.part` go, and the
/// memo holds the next pass off the wire from the first refusal until the digests move. A
/// machine fault (codesign could not answer) records nothing and keeps the archive.
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

// The same rule on the single-program path: `abort_activated_install` discards a build this
// call created, never one that was already live. The failures that land there
// (`install_tools` hitting EACCES/EROFS/a full disk, or the resolve check) are facts about
// the environment, not the tree.
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

    // Break `install_tools` as an unwritable prefix does: a regular file where `bin/` must
    // be, so `ensure_private_dir` fails after activation. `installed: None` is the shim view
    // being silent about a live program.
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

    // The keep must outlive the run: `was_live` is read off the `current` link the unwind
    // removes, so the witness is re-pointed at the kept build and a second failure still
    // knows it was live.
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

    // Non-vacuity: a build this call created is still discarded. Restore the prefix, then
    // fail a fresh install of a program that was never live.
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

// The retry is bounded to what failed: an aborted group discards every staged sibling's
// extracted tree (a complete-but-inactive build would split the tuple) but keeps their
// verified downloads, so a multi-GB toolchain is not refetched on every tick. Pass 2 runs
// with the sibling's asset removed from the registry, so it can stage only from the carry.
#[test]
fn an_aborted_tuple_carries_its_verified_siblings_so_the_retry_refetches_only_the_failure() {
    let dir = scratch("group-carry-archive");
    let mut fake = group_fixture(&dir);
    let layout = layout(&dir);
    let installed = std::collections::BTreeMap::from([("ay".to_string(), 17u64)]);

    // Pass 1: trust's asset is unfetchable. `ay` stages completely (downloaded, verified,
    // extracted), then trust's download fails and the tuple aborts.
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

    // The pass-end gc must not undo that (the CLI's wiring).
    let _ = crate::gc::run_keeping_pinned_partials(&layout, None, &|p| {
        report.resolved_assets.get(p).cloned()
    });
    assert_eq!(
        staged_assets(&layout, "ay"),
        vec!["ay-18.tar.zst".to_string()],
        "the pass-closing sweep spares the carried archive, as it does the `.part`"
    );

    // Pass 2: trust is reachable again, but `ay`'s asset is gone from the registry, so the
    // tuple can converge only from the carried archive.
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

    // A settled tuple keeps nothing: the carry exists only between passes.
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

// Resume across passes, update lane: a member whose download fails mid-pass still reaches
// `ChannelApplyReport::resolved_assets` (the name is recorded when the verified manifest
// selects the artifact), so the CLI's pass-end `gc::run_keeping_pinned_partials` spares the
// `.part` and the next pass resumes.
#[test]
fn a_failed_download_survives_its_own_pass_end_gc_and_a_plain_gc_still_sweeps_it() {
    let dir = scratch("resume-survives-pass-end");
    let mut fake = group_fixture(&dir);
    let layout = layout(&dir);
    // trust's download fails after its manifest verified, the window that leaves a `.part`.
    // `Fake` copies whole files or fails, so the test seeds the resume bytes.
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
    // The failed member is in the map (resolution precedes its download), and so is the
    // sibling that staged fine.
    assert_eq!(
        report.resolved_assets.get("trust").map(String::as_str),
        Some("trust-4821.tar.zst"),
        "the failed member's pinned asset name must reach the report"
    );
    assert_eq!(
        report.resolved_assets.get("ay").map(String::as_str),
        Some("ay-18.tar.zst")
    );

    // The CLI's pass-end wiring: the sparing closure is the report's map.
    let _ = crate::gc::run_keeping_pinned_partials(&layout, None, &|p| {
        report.resolved_assets.get(p).cloned()
    });
    assert!(
        part.exists(),
        "the pass-end sweep spares the failed member's resume state"
    );

    // `uninstall --all` and the standalone `atpkg gc` keep the plain form: everything spared
    // above stays reclaimable.
    let _ = crate::gc::run(&layout, None);
    assert!(
        !part.exists(),
        "a plain gc still reclaims the spared partial"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

// The same wiring on the singleton lane (`bootstrap_singleton` / `do_install`): a failed
// install returns no report, so the resolved asset rides the collector out-param, filled
// before the download.
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

/// The fetcher contract for the vendor lane: `download_url` is refused by default, so a
/// test fetcher and the `dir:` registry fetcher never reach the network by accident.
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

    // The vendor-direct lanes answer "unreachable" by default: a fetcher with no network
    // leg keeps what is installed and writes nothing.
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

/// A `github-release` row's `asset` is joined onto `staging/<program>/`, which the flow
/// unlinks and sweeps for `*.part` before any download, so an asset that is a path
/// (absolute, or climbing with `..`) is refused before either runs.
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
        // A TOML literal string: a Windows path's `\` is no escape there.
        let pkg_body = format!(
            "schema = 2\nprogram = \"ay\"\nversion = \"0.1\"\nbuild_number = 18\n\
             exposes = [\"ay\"]\n\
             [[artifact]]\ntarget = \"{TRIPLE}\"\nkind = \"binary\"\n\
             protocol = \"github-release\"\nasset = '{asset}'\n\
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

/// The vendor lane end to end through the real flow:
/// 1. a `binary` row over `https` is dispatched (never `UnsupportedKind`) and admitted
///    before any byte moves; a row with no url is `VendorRefused`, staging untouched;
/// 2. an admitted row downloads only through `download_url`: the slug lane is never
///    consulted, and a fetcher without the vendor lane fails as a `Download` naming it;
/// 3. a fetcher that serves the signed URL installs like a plain binary under the sha256
///    and tree_root gates, with the signed `size` as the exact cap.
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

    // 2. Admitted, but this fetcher has no vendor lane: the slug lane is not a fallback.
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

    // 3. A fetcher with the vendor lane installs end to end, through that lane only.
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

/// The release lane is capped by the row's signed size, as the vendor lane is: the disk
/// gate runs once before the transfer and the sha256 gate sees only the whole file, so the
/// cap the fetcher is given is all that bounds the bytes as they land. `download_for` gets
/// the row's own `size`.
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
    // The fixture's signed row states `size = 100`, which nothing else in the flow reads
    // back.
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

/// Design S7 end to end: a manifest declaring `shim_env = ["DISABLE_AUTOUPDATER=1"]` lays a
/// shim that exports it before it execs, records it in the `<build>.shim-env` sidecar for
/// the verbs that hold no manifest, and still resolves with `which`. A `shim_env` that
/// breaks the rule is refused at parse as `ShimEnv`, before any byte moves.
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
    // `fixture_vendor` appends `row` inside the artifact table, so a top-level key goes in
    // by re-heading the signed body: `shim_env` after `exposes`, re-signed.
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

/// A dependency the user's own copy satisfies (`system = "<bin>"`, on PATH outside the
/// prefix) is reported `DepResult::System(path)` and never installed: no manifest fetched,
/// no managed copy laid beside it.
#[cfg(unix)]
#[test]
fn a_system_satisfied_dependency_is_never_pulled_in_as_a_managed_copy() {
    let dir = scratch("requires-system");
    let sys = dir.join("usr-local-bin");
    let gh = lay_exe(&sys, "gh");
    let path = std::env::join_paths([sys.clone()]).unwrap();
    // `ay` requires `gh`; gh declares `system = "gh"` and is pinned, but its pkg manifest is
    // absent, so fetching it would fail.
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
    // app-bundle is the app's own in-session apply topology, not a tool install: refused
    // before any download (the fetcher serves no archive, so a later refusal would show).
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
    // Refused as the app's own pair, distinct from UnsupportedKind, so the pass can say the
    // app updates itself.
    assert!(
        matches!(err, FlowError::AppBundleRefused(ref p) if p == "ay"),
        "got {err:?}"
    );
    assert!(
        crate::ops::which(&alay, "ay").is_none(),
        "refused install leaves no shim"
    );
    let _ = std::fs::remove_dir_all(&adir);

    // sysroot-bundle (self-contained: relocated at pack time, no rustup needed) stages,
    // activates, shims and passes the resolve check (the fixture's `#!/bin/true` exits
    // cleanly).
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

// The resolve-failure unwind (fresh install): a sysroot-bundle whose exposed binary cannot
// load is not left active (`decide` would read it as UpToDate forever): no pointer
// survives, the build is discarded, and a retry re-installs.
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
    // …so a retry (healthy bundle, same store) re-installs instead of 'already current'.
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

// The upgrade variant: ay@17 is live and the pinned ay@18 fails its resolve check, so the
// shims and witness revert to 17 and 18 is discarded, as `flip_member` does on the
// transactional path.
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

// The single-program `install` path also disables a tombstoned build's old working shim:
// the channel pins ay=18 but yanks it, so install returns Tombstoned and bin/ay becomes a
// failing shim.
#[test]
fn install_tombstone_disables_the_old_shim() {
    let dir = scratch("install-tomb");
    let layout = layout(&dir);
    seed_build(&layout, "ay", 17, true); // ay@17 active + shimmed
    // A live forwarding shim (a symlink on Unix, a forwarding `.cmd` on Windows).
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

/// An up-to-date pass re-points a missing `current` witness at the build the pin and the
/// shims agree on (the `doctor` remedy for `NoLiveWitness`), with no fetch; a second pass
/// is a no-op.
#[test]
fn an_up_to_date_pass_reasserts_a_missing_current_witness() {
    let dir = scratch("witness");
    let fake = fixture(&dir);
    let layout = layout(&dir);
    // Build 18 on disk and shimmed, with no `store/ay/current`.
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

/// The Rosetta repair through the real apply decision: a live build (the shim view reports
/// it) whose `<build>.ready` this slice refuses is re-staged, not read as already current.
/// [`installed_for_decide`] keeps `decide` from answering UpToDate off the shim view.
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
    // 1. A real install: ay@18 is live, shimmed, and marked by this slice.
    let first = install(&fake, &layout, &anchor(), &fresh, fl(0), 0).unwrap();
    assert!(!first.already_current);
    assert!(crate::store::build_is_complete(&build_dir));

    // 2. Forge the sibling `<build>.ready` another slice would have left, the one file
    //    `store::build_is_complete` reads.
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

    // 3. The next native pass, fed what the CLI feeds it.
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

// Disk preflight is a pure gate over an injected `available`, tested without a real statvfs.
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

// --- coherence-group transactional apply -------------------------------------------

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
        &bin_entry(program),
        format!("#!/bin/true\n{program}").as_bytes(),
    ));
    tar.resize(tar.len() + 1024, 0);
    let path = dir.join(format!("{program}-{build}.tar.zst"));
    std::fs::write(&path, crate::extract::fixtures::zstd_bytes(&tar)).unwrap();
    path
}

/// A signed release whose `stable` channel pins a `rustc` coherence group (trust@4821 +
/// ay@18), with consistent per-build manifests and archives.
fn group_fixture(dir: &Path) -> Fake {
    group_fixture_with(dir, &[])
}

/// [`group_fixture`] with each program in `unserved` published only for a foreign
/// triple — how the release looks from a host the publisher does not build it for.
fn group_fixture_with(dir: &Path, unserved: &[&str]) -> Fake {
    group_fixture_of(dir, unserved, &[])
}

/// [`group_fixture_with`] with each program in `bundles` shipped as a `sysroot-bundle`
/// (the `self-contained` default), as the trust toolchain is.
fn group_fixture_of(dir: &Path, unserved: &[&str], bundles: &[&str]) -> Fake {
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
        let kind = if bundles.contains(&program) {
            "sysroot-bundle"
        } else {
            "binary"
        };
        let pkg_body = format!(
            "schema = 2\nprogram = \"{program}\"\nversion = \"0.1\"\nbuild_number = {build}\n\
             exposes = [\"{program}\"]\n\
             [[artifact]]\ntarget = \"{target}\"\nkind = \"{kind}\"\nasset = \"{asset}\"\n\
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

/// A store whose `rustc` tuple is live at trust@4800 + ay@17, which [`group_fixture`]'s
/// trust@4821 + ay@18 supersede.
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

/// Stage now, flip when quiet: an unattended pass meeting a build running from the live
/// toolchain stages every moving member's signed archive, extracts and flips nothing, and
/// records the wait. A second busy pass downloads nothing and keeps the first deferral's
/// clock; the pass that finds the toolchain quiet flips from those archives and ends the
/// record.
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
    // The record names each staged archive, so a gc that resolved none of them keeps them
    // for the flip.
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

/// A download that fails its signed digest while the flip waits is the stage's failure: the
/// pass aborts, the bad archive goes, and the refusal memo is written.
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

/// The wait is bounded: four hours from the first deferral the flip lands however busy the
/// toolchain is. A person never waits: a pass without the unattended policy flips at once.
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

/// A held flip no group holds any longer is over: a pass that skips the toolchain's group
/// whole (nothing of it installed) ends the record.
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

/// [`group_fixture`]'s release with the `rustc` tuple pinned only through a `pin_by_target`
/// overlay for [`TRIPLE`]: the raw `pin` names trust@4820 (no manifest served) and no `ay`;
/// the overlay replaces trust with 4821 and adds ay@18.
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

/// The update lanes honour the per-target pin: the overlay-added `ay` is a member and
/// `trust` lands on the overlay's 4821, never the raw channel's.
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

/// The tuple-bootstrap lane stages what its caller planned: `bootstrap_group` resolves the
/// same per-target view as the CLI's plan and the `group_missing_triple` prescan.
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

/// `plan_update` decides against the per-target pin: trust@4821 is up to date (not a move to
/// another target's 4820), and the overlay-added `ay` is pinned.
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

// Atomicity under failure: one member's stage fails, the whole group aborts before any
// flip, the already-staged sibling's build is discarded, and a retry heals the tuple.
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
    // The already-staged ay@18 was discarded, so the next run cannot read it as active and
    // split the tuple.
    assert!(
        !layout.build_dir("ay", 18).exists(),
        "aborted-group staged build is discarded"
    );
    assert!(
        !crate::store::build_is_complete(&layout.build_dir("ay", 18)),
        "no lingering completeness marker for the discarded build"
    );
    // The failed member's archive is reclaimed, while the sibling that staged fine keeps
    // its verified archive so the next pass does not refetch it.
    assert!(
        staged_assets(&layout, "trust").is_empty(),
        "the failing member stranded its archive: {:?}",
        staged_assets(&layout, "trust")
    );
    assert_eq!(
        staged_assets(&layout, "ay"),
        vec!["ay-18.tar.zst".to_string()]
    );

    // Heal: re-create the real signed archive and retry.
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

// The abort discard never deletes a build that was already live when this transaction
// re-staged it: with a program's shims gone the shim view is silent, so `decide` returns
// Install for the build `current` still names, and a sibling's stage failure must not take
// that toolchain down.
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

    // 2. `atpkg unlink`-shaped state: ay's shims are gone (the primary and its `alab-ay`
    //    alias, which would keep the shim view talking), so the shim view is silent for ay
    //    while the authority link still names ay@18.
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

    // 3. A sibling fails to stage, so the group aborts after ay re-staged fine.
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

    // The failing member is `trust`; `ay` must survive intact.
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

/// The same repair inside the group transaction, which decides per member: a tuple member
/// whose store tree this slice cannot vouch for is re-staged, not held at UpToDate beside
/// its healthy sibling.
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

    // 2. ay@18 loses the marker this slice reads (another slice's `platform=` record, or an
    //    unmarked recovery). The shims still resolve into it, so the shim view says 18.
    crate::store::clear_build_ready(&ay18).unwrap();
    let live = crate::ops::active_builds(&layout);
    assert_eq!(
        live.get("ay").copied(),
        Some(18),
        "PRECONDITION: the shim view the update lane feeds `decide` still says 18"
    );
    assert_eq!(live.get("trust").copied(), Some(4821));

    // 3. The next pass, fed that map as `cmd_update_all` feeds it.
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

    // A healthy tuple is still a no-op.
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

// The missing-triple hold: an installed tuple (ay@17 + trust@4820) whose new pin trust@4821
// publishes nothing for this triple is held whole: never staged or aborted, nothing
// resolved.
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

/// A complete build dir with `bin/<program>`; `activate` also shims and activates it.
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

/// An index-only Fake (rollback never downloads) pinning `ay`, with a configurable
/// `min_build` and `yanked` list.
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

/// [`rollback_index`] with the floor published per program (`min_build_by_program`) and the
/// channel-wide floor at 0.
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

/// Exhaustive conformance for the removed-member hold: what an unattended update does to a
/// coherence tuple part of which the user uninstalled, enumerated over ay {absent, safe,
/// revoked} × trust {absent, installed} × removal records × exclusions, against {clean,
/// installed-build-yanked, pin-yanked}. The assertions are invariants, not expected outputs;
/// the core three (the rest are stated inline):
///
/// - I1. A member recorded-removed and absent is never installed.
/// - I2. A revoked installed build is never left runnable: upgraded or tombstoned.
/// - I3. The pass never claims `UpToDate` while an installed member is revoked.
///
/// Each channel — clean, the INSTALLED build (`ay@17`) yanked, the PIN (`ay@18`) yanked —
/// enumerates its own 72 cases over ONE signed fixture: the release (two real archives,
/// their extracted tree roots, the signed manifests and the channel's signed index) is
/// built once per channel and only READ by the cases — `Fake::download` copies an
/// archive out, it never moves one — while every case gets a store of its own. Each
/// channel must decide at least 24 of its 72 cases.
#[test]
fn the_removed_member_hold_is_exhaustively_safe() {
    let channels = [
        ("clean", group_fixture as fn(&Path) -> Fake),
        ("yank-installed", group_fixture_yanking_ay17),
        ("yank-pin", group_fixture_yanking_ay18),
    ];
    let decided = std::thread::scope(|s| {
        let runs: Vec<_> = channels
            .map(|(cname, make)| s.spawn(move || removed_member_hold_cases(cname, make)))
            .into_iter()
            .collect();
        runs.into_iter().map(joined).collect::<Vec<_>>()
    });
    for ((cname, _), checked) in channels.iter().zip(decided) {
        assert!(
            checked >= 24,
            "{cname}: the enumeration ran only {checked} cases"
        );
    }
}

/// Every case of the removed-member hold on one channel; returns how many reached a report
/// (a resolve/verify failure is not this predicate's business).
fn removed_member_hold_cases(cname: &str, make: fn(&Path) -> Fake) -> usize {
    let fixture_dir = scratch(&format!("hold-fixture-{cname}"));
    let fake = make(&fixture_dir);
    // The three `ay` states run side by side (as the three channels do), each over 24
    // stores of its own: a case shares nothing with another but the read-only fixture.
    // A case is a few hundred directory, link and rename calls; on a volume busy with
    // other flushes each call waits its turn, and 216 cases in one sequence waited a
    // minute where nine lanes of 24 wait seconds.
    let checked = std::thread::scope(|s| {
        let lanes: Vec<_> = [Ay::Absent, Ay::Safe, Ay::Revoked]
            .into_iter()
            .map(|ay| {
                let fake = &fake;
                std::thread::Builder::new()
                    .name(format!("hold-{cname}-{ay:?}"))
                    .spawn_scoped(s, move || removed_member_hold_lane(cname, fake, ay))
                    .expect("spawn a hold lane")
            })
            .collect();
        lanes.into_iter().map(joined).sum::<usize>()
    });
    let _ = std::fs::remove_dir_all(&fixture_dir);
    eprintln!("removed-member hold, channel {cname}: {checked} of 72 cases decided");
    checked
}

/// What `ay` is in the store before the pass: absent, at the pin (18), or at the build
/// the `yank-installed` channel revokes (17).
#[derive(Clone, Copy, Debug)]
enum Ay {
    Absent,
    Safe,
    Revoked,
}

/// A scoped lane's result, its panic re-raised as the test's own failure.
fn joined<T>(lane: std::thread::ScopedJoinHandle<'_, T>) -> T {
    lane.join()
        .unwrap_or_else(|panic| std::panic::resume_unwind(panic))
}

/// The 24 cases of [`removed_member_hold_cases`] with `ay` in one state.
fn removed_member_hold_lane(cname: &str, fake: &Fake, ay: Ay) -> usize {
    let lane = scratch(&format!("hold-{cname}-{ay:?}"));
    let mut checked = 0;
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
                    "{cname}-{ay:?}-t{trust_installed}-r[{}]-x[{}]",
                    removed.join(","),
                    excluded.join(",")
                );
                let dir = crate::testdir::dir_in(&lane, "case-");
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
                    fake,
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
    checked
}

/// [`group_fixture`] with `ay@18` yanked, so `decide` tombstones the group.
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

// A local pin freezes the whole group on its current builds: no upgrade staged, no sibling
// pulled in.
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

/// Fixture: the pin (ay=18) is valid but the installed ay@17 is yanked, so `decide` returns
/// Install (force-upgrade off 17), not Tombstone.
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

// A local pin never keeps a yanked current build running: ay@17 is pinned but yanked and
// the pin (18) is valid, so the tuple force-upgrades.
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

// A pin never suppresses a tombstone: a yanked pin tombstones the group even when pinned.
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

/// [`fixture`] (the singleton `ay`, no `coherence_group`) with its own pin yanked, so
/// `decide` tombstones it and the pass disables its live shims.
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

// The tombstone's own instruction ("run `aterm pkg update`") has to work: the plain pass
// re-stages a tombstoned singleton although a tombstone resolves nowhere in the shim view
// `installed` is. The set-completion lane cannot stand in on a machine that never adopted
// the set.
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
    // Precondition: the shim view `cmd_update_all_code` feeds this lane is silent for ay.
    let live = crate::ops::active_builds(&layout);
    assert!(
        !live.contains_key("ay"),
        "a tombstone resolves nowhere, so the shim view does not name ay"
    );
    // 2. The publisher re-points the pin at a good build: a plain `update` pass fed that
    //    silent map, as the CLI feeds it, re-stages the singleton.
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

/// A gutted rollback target (orphaned `lib/`, no `bin/`, a legacy bare `ok\n` marker) would
/// remove every shim, so selection asks the tree directly and skips it.
#[test]
fn rollback_refuses_a_target_whose_tools_are_gone_and_takes_the_one_below() {
    let dir = scratch("rollback-gutted");
    let layout = layout(&dir);
    let fake = rollback_index(0, &[]);
    seed_build(&layout, "ay", 16, false);
    seed_build(&layout, "ay", 17, false);
    seed_build(&layout, "ay", 18, true); // active
    // Gut 17 as an interrupted removal does, under a legacy bare `ok` marker.
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

    // …and when the gutted build is the only candidate, the verb refuses, removing nothing.
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

// --- requires dependency pull-in ----------------------------------------------------

/// A signed release naming `ay`@18 + `ny`@9 (no coherence group), with configurable
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

/// The retired index-row `requires` is refused on both lanes: `install` refuses ay before any
/// manifest is fetched, and the update lane aborts ay's group with the same sentence, held on
/// its current build. `ny`, which names nothing, installs; an up-to-date group refuses
/// nothing.
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
    // The alias policy through the real pipeline: ALab's own programs (no `system`) get
    // `alab-<tool>` beside `<tool>`, a pulled-in dependency included.
    for program in ["ay", "ny"] {
        let alias = format!("alab-{program}");
        assert!(
            crate::ops::which(&layout, &alias).is_some(),
            "{alias} is laid by the install"
        );
    }
    let _ = std::fs::remove_dir_all(&dir);
}

/// A dependency the gate refuses (a yanked build, or a program the signed index does not
/// list) is skipped, not fatal: `ay` still installs, and the gate is not bypassed.
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
    // ay requires ny and ny requires ay.
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
    // The walk `atpkg verify` runs: over the declared-mode record where the filesystem has
    // no permission bits (Windows), the plain read-back walk where it does (Unix).
    let build_dir = layout.build_dir("ay", 18);
    let modes = crate::store::declared_modes(&build_dir).unwrap();
    assert_eq!(
        report.tree_root,
        crate::tree::tree_root_declared(&build_dir, &modes).unwrap(),
        "it equals the on-disk tree's recomputed root (what `atpkg verify` compares)"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

// --- dev-link skip + dir fetcher + same-source cache ---------------------------------

/// Mark `program` dev-linked with a minimal marker (no real checkout needed).
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

/// A [`Fake`] whose index fetch can be toggled to fail or to succeed empty, with a
/// controllable `source_id` for the same-source cache guard.
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
    // 1. A good fetch installs and caches the index under source "src:A".
    install(&f, &layout, &anchor(), &req, fl(0), 0).unwrap();
    // 2. Fetch now fails, same source: the install is served from the cache.
    f.fail.set(true);
    install(&f, &layout, &anchor(), &req, fl(0), 0).expect("cache fallback serves the index");
    // 3. A different source with a failing fetch has no cache to fall back on, and the
    //    error names transport, not trust: the fix is the network, not keys.
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

/// The typed lanes name the network: `plan_update` resolves live with no cache, so an
/// unreached source is `Unreachable` with the transport's reason, and a reached source that
/// lists nothing is `NoIndex`.
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

// An empty listing is a fetch that found nothing: it takes the same same-source cache
// fallback as a hard `Err`, never a NoIndex while a good cache sits on disk.
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
    // 1. A good fetch installs and caches under "src:A".
    install(&f, &layout, &anchor(), &req, fl(0), 0).unwrap();
    // 2. The listing now succeeds empty from the same source: served from the cache, and
    //    the resolve says so (no success stamp, no current yank latch).
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
    // The fetch now fails: fallback to the cached bytes, which are still past valid_until.
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
    // A `dir:` registry publishes the master-signed roster too, as a release does.
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

/// Rollback restores the prior build's whole surface: a tool the new build dropped lost its
/// shim to the new build's prune, so the restore set is the new exposes plus the prior `bin/`.
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

/// A trust rollback never takes another program's shim: a sysroot bundle's `bin/` carries
/// other programs' backends, and a `bin/ty` re-pointed into `store/trust/<prior>` would be
/// pruned by the next trust flip. A shim another program owns (ty), an installed program's
/// own name with no shim (ay) and a dev link (clean) are left alone. The unowned `trustd` is
/// still restored: it is in trust's `bin/` but not its signed exposes, a known over-reach
/// until each build records its signed exposes at stage time.
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

    // The flip that follows prunes stale trust shims; ty's must survive it.
    activate_build(&l, &t2).unwrap();
    install_tools(&l, &t2, &[tool("trustc")], Aliases::Alab).unwrap();
    owned_by_ty("after the next trust flip");
    assert!(into("tla").is_some_and(|t| t.starts_with(&ty)));
    let _ = std::fs::remove_dir_all(&dir);
}

/// A listing name's alias gets its own ownership check: the free primary `trustd` is
/// restored into the prior build, but the dev-linked `alab-trustd` is left alone. A signed
/// expose's alias (`alab-trustc`) is re-pointed unconditionally.
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

/// A program installed without a `current` link and with no shim still owns its names: a
/// trust rollback that finds `ay` in the prior bundle's `bin/` must not take it while a
/// complete `ay` build is in the store, or the next trust flip prunes it. With the ay build
/// incomplete (not installed) the rollback restores `ay` for trust.
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

/// A rollback to an affected trust build lays its exec root before its shims: rolled back
/// from 8595 to a retained 8590 with no root yet, the undo lays `compat/trust/8590` and the
/// re-pointed `tippy` shim routes through it at once, since the single-program lanes run no
/// reconcile after a failed flip.
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

/// Aliases ride with their primary: a rollback re-points `alab-<tool>` at the prior build
/// (dropping the alias of a tool the prior build lacks), a fresh-install rollback removes it,
/// and a tombstone disables it.
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
    // The rollback verb reads the policy off the disk: aliases laid ⇒ kept.
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

/// The front-of-PATH twin follows the rollback. An agent program is laid at `bin/<tool>` and
/// again under `agents/` (which the shell hook puts first on PATH), and the twin names its
/// store build directly; a rollback must re-point it with `bin/`, and a fresh-install undo
/// remove it, or `claude` keeps running the build being rolled away from (which gc's claim
/// union, blind to `agents/`, may then delete).
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

    // The fresh-install arm: no prior build, so the primary goes and the twin with it.
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

/// A failed flip leaves `store/<program>/current` naming the prior build, agreeing with the
/// untouched shims, never the build the abort deletes. The write here fails as a Windows
/// junction swap does, removing the old link first.
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
    let flipped = flip_member_with(&l, "ay", &staged, None, |layout, _| {
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

/// The fresh-install variant: no prior build exists, so the repair removes the witness link
/// a half-made write left naming the new build, which the abort cleanup deletes.
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
    let flipped = flip_member_with(&l, "ay", &staged, None, |layout, build_dir| {
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

/// PKG-6 #11 — A BUNDLE'S RESOLVE CHECK RUNS FROM ITS STAGE, AND THE FLIP TAKES ITS VERDICT.
/// The check starts on its own thread when the member is staged, before anything asks for
/// it: the first exec of a toolchain's compilers (2.4 s for the trust toolchain's driver)
/// runs beside the stages that follow instead of inside the flip. The flip then takes that
/// verdict rather than checking again: a bundle whose check failed ahead is rolled back,
/// whatever its tree would answer now. A plain member, or a policy with wiring the flip
/// applies first, starts nothing.
#[test]
fn a_bundle_resolve_check_runs_from_its_stage_and_the_flip_takes_its_verdict() {
    let dir = scratch("resolve-ahead");
    let l = layout(&dir);
    let b18 = bare_build(&l, "ay", 18, &["ay"]);
    let b19 = bare_build(&l, "ay", 19, &["ay"]);
    activate_build(&l, &b18).unwrap();
    install_tools(&l, &b18, &[tool("ay")], Aliases::Off).unwrap();
    let bundle = |reloc: &str| Staged {
        build: 19,
        build_dir: b19.clone(),
        exposes: vec![tool("ay")],
        prior_build: Some(18),
        was_live: false,
        reloc: Some(reloc.to_string()),
        aliases: Aliases::Off,
        tree_root: String::new(),
    };
    // It runs ahead: started, nobody waiting on it, it has begun (a wait for something
    // that must happen — a lazy check would never start here).
    let (began, started) = std::sync::mpsc::channel();
    let (release, go) = std::sync::mpsc::channel::<()>();
    let check = ResolveAhead::start_with(&bundle("self-contained"), move |build_dir, exposes| {
        // Neither wait outlives a test that already failed: a dropped check is joined.
        let _ = began.send((build_dir.to_path_buf(), exposes.to_vec()));
        let _ = go.recv_timeout(std::time::Duration::from_secs(60));
        Err(FlowError::Activate(
            "resolve check: ahead of the flip".into(),
        ))
    })
    .expect("a self-contained bundle starts its check at the stage");
    let (checked_dir, checked_tools) = started
        .recv_timeout(std::time::Duration::from_secs(60))
        .expect("the check began before the flip asked for it");
    assert_eq!(
        (checked_dir, checked_tools),
        (b19.clone(), vec![tool("ay")])
    );
    release.send(()).unwrap();
    // The bin here is a script, so a check run at the flip would pass: the flip refusing
    // is the verdict taken ahead.
    assert!(bundle_resolve_check(&b19, &[tool("ay")]).is_ok());
    assert!(
        !flip_member(&l, "ay", &bundle("self-contained"), Some(check)),
        "the failed verdict from the stage refuses the flip"
    );
    assert_eq!(
        crate::ops::which(&l, "ay").unwrap(),
        tool_bin(&b18, "ay"),
        "rolled back to the prior build"
    );
    // A healthy verdict flips.
    let ok = ResolveAhead::start_with(&bundle("self-contained"), |_, _| Ok(())).unwrap();
    assert!(flip_member(&l, "ay", &bundle("self-contained"), Some(ok)));
    assert_eq!(crate::ops::which(&l, "ay").unwrap(), tool_bin(&b19, "ay"));
    // Nothing starts for a plain member, nor ahead of wiring the flip would apply first.
    let mut plain = bundle("self-contained");
    plain.reloc = None;
    assert!(ResolveAhead::start_with(&plain, |_, _| Ok(())).is_none());
    assert!(ResolveAhead::start_with(&bundle("rpath-fixup"), |_, _| Ok(())).is_none());
    let _ = std::fs::remove_dir_all(&dir);
}

/// PKG-6 #11 — A GROUP STARTS A BUNDLE'S RESOLVE CHECK AT ITS STAGE, BEFORE THE NEXT
/// MEMBER STAGES. Over a live tuple, `ay` (a self-contained bundle here) stages first;
/// trust's download, the
/// start of the next member's stage, waits for `ay`'s check to begin (a hang detector: a
/// check left to the flip, or never started ahead, begins only after it). The flip takes
/// that verdict and the tuple lands.
#[test]
fn a_group_starts_a_bundles_resolve_check_before_the_next_member_stages() {
    type Began = Result<PathBuf, std::sync::mpsc::RecvTimeoutError>;
    struct AwaitsCheck {
        inner: Fake,
        began: std::sync::mpsc::Receiver<PathBuf>,
        before_trust: std::cell::RefCell<Option<Began>>,
    }
    impl Fetcher for AwaitsCheck {
        fn index_candidates(&self) -> Result<Vec<Candidate>, String> {
            self.inner.index_candidates()
        }
        fn pkg_manifest(&self, r: &str, p: &str, b: u64) -> Result<(Vec<u8>, Vec<u8>), String> {
            self.inner.pkg_manifest(r, p, b)
        }
        fn download(&self, repo: &str, asset: &str, dest: &Path) -> Result<(), String> {
            if asset == "trust-4821.tar.zst" {
                let began = self.began.recv_timeout(std::time::Duration::from_secs(60));
                *self.before_trust.borrow_mut() = Some(began);
            }
            self.inner.download(repo, asset, dest)
        }
    }
    let dir = scratch("resolve-ahead-group");
    let (tell, began) = std::sync::mpsc::channel();
    let fake = AwaitsCheck {
        inner: group_fixture_of(&dir, &[], &["ay"]),
        began,
        before_trust: std::cell::RefCell::new(None),
    };
    let layout = layout(&dir);
    let installed = live_toolchain(&layout);
    let report = with_resolve_check(
        std::sync::Arc::new(move |build_dir: &Path, _: &[ToolName]| {
            let _ = tell.send(build_dir.to_path_buf());
            Ok(())
        }),
        || {
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
            .unwrap()
        },
    );
    assert_eq!(
        fake.before_trust.into_inner(),
        Some(Ok(layout.build_dir("ay", 18))),
        "ay's resolve check began before trust staged"
    );
    assert_eq!(
        report.groups[0].1,
        TxnOutcome::Applied(vec!["ay".into(), "trust".into()])
    );
    assert_eq!(
        crate::ops::active_builds(&layout).get("ay").copied(),
        Some(18)
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
    let flipped = flip_member(&l, "ay", &staged, None);
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
    // A failed stage's archive is retained for reuse, so a complete archive of another
    // asset name is swept: at most one of each survives per program.
    assert!(
        !finished.exists(),
        "a superseded build's retained archive is reclaimed"
    );
    assert!(!vendor.exists(), "a vendor-direct asset is reclaimed");
    let _ = std::fs::remove_dir_all(&dir);
}

/// The group path holds the same strandage bound, driven through `apply_channel` so it pins
/// `stage_member`'s call.
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

/// A [`Fake`] that counts the artifact downloads it is asked for, by asset name.
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

/// Append bytes to a fixture's archive after its manifest signed the honest sha256, so every
/// download fails the signed digest gate.
fn poison(archive: &Path) {
    use std::io::Write as _;
    let mut f = std::fs::OpenOptions::new()
        .append(true)
        .open(archive)
        .unwrap();
    f.write_all(b"poison").unwrap();
}

/// Rewrite a refusal memo's `at=` line so its cooldown has lapsed.
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

/// The refetch bound on the singleton lane: re-proving a digest mismatch costs the whole
/// asset. The first retry is free; the second identical verdict arms a cooldown that keeps
/// the next pass off the wire and says why; once it lapses the pass tries once more and the
/// memo re-arms for longer.
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

    // The first retry is free: one mismatch can be a truncated transfer, and a repair under
    // the same pin must be found. Its identical verdict arms the cooldown.
    let second = install(&fake, &layout, &anchor(), &req, fl(0), 0)
        .expect_err("a corrupted asset must fail the signed digest gate");
    assert!(
        matches!(&second, FlowError::Stage(StageError::DigestMismatch { .. })),
        "the first retry is never refused — {second:?}"
    );
    assert_eq!(fake.count("ay-18.tar.zst"), 2, "it really did retry");

    // The tick after that: no transfer, and the refusal carries the original verdict and
    // the way out.
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

    // Once the cooldown lapses the machine asks again, once, and the memo re-arms longer.
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

/// The hold on the singleton lane: a `tree_root` mismatch repeats on every re-stage, so from
/// the first such verdict the row is held (no download, no stage, the canonical `held:` row
/// naming the retry), the verified archive is kept, the hold never lapses, and the explicit
/// door forgets it.
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

    // The very next tick is held: nothing fetched or staged, and the row names the verdict
    // and the retry.
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

    // The explicit door (`install <p>`, `update --retry`) forgets it: the next attempt
    // stages over the kept archive and re-arms the hold on the same verdict.
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

    // A memo another atpkg wrote binds nothing: a client-side fix ships as a new atpkg,
    // which gets its one attempt.
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

/// The refetch bound on the group lane: the tuple still aborts and retries once for free,
/// but the third download is skipped and the abort says so.
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
    // The default set's disk check reads the same memo: a held tuple lands nothing, and
    // once the memo is forgotten its figure is both members' bytes.
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

/// The default set's disk check reads a tuple as its transaction does: a sibling's local pin
/// or a revoked pin lands nothing, and it counts every member the transaction stages,
/// including an installed member incomplete for this slice.
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

/// The set's peak is what each unit's gate checks in the worst queue order: with that much
/// free (plus the reserve) every order passes, with one byte less some order fails. Summing
/// the trees alone is short by the largest unit's archives.
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
    let _slot = crate::progress::own_pass_slot();
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
// counts the one fetch of its own that failed. The archive was asked for twice, the lane's
// download first, and the pass took the lane's answer for every member — the failure among
// them — before fetching that one itself. Negative control: a pass that never asks the
// lane fails the same way inline, and leaves the lane's archives untaken.
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
    let (outcome, lane) = bootstrap_through_lane(&rec, &layout, &mut resolved);
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
    let tc: Vec<String> = rec
        .starts()
        .into_iter()
        .filter(|(asset, _)| asset == "tc-3.tar.zst")
        .map(|(_, thread)| thread)
        .collect();
    assert_eq!(tc.len(), 2, "the lane's download, then the pass's: {tc:?}");
    assert_eq!(tc[0], LANE_THREAD, "the lane lost it first");
    assert_ne!(
        tc[1], LANE_THREAD,
        "the pass fetched it itself after the lane's loss"
    );
    assert!(
        lane.untaken().is_empty(),
        "the pass took every archive the lane fetched or lost: {:?}",
        lane.untaken()
    );
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
        std::fs::write(tool_bin(&build, name), [0xcf, 0xfa, 0xed, 0xfe, 0, 0, 0, 0]).unwrap();
    }
    std::fs::write(tool_bin(&build, "wrap"), b"#!/bin/sh\nexec t1 \"$@\"\n").unwrap();
    let mut exposed: Vec<String> = names.iter().map(|n| (*n).to_string()).collect();
    exposed.push("wrap".into());
    let exposes = crate::store::split_exposed(&exposed).0;
    let verdict = |killed: &[&str]| {
        let checked = std::sync::Mutex::new(Vec::new());
        let result = bundle_resolve_check_with(&build, &exposes, &|bin: &Path| {
            // The tool's name: the file less the host's executable suffix.
            let name = bin.file_stem().unwrap().to_string_lossy().into_owned();
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
