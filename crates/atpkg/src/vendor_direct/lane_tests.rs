// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::world::{self, CLAUDE, CODEX_HEAD, Fake, NOW, native_exe};
use super::*;

fn v(text: &str) -> Version {
    Version::parse(text).unwrap()
}

fn claude() -> &'static VendorSpec {
    super::super::spec("claude").unwrap()
}

fn codex() -> &'static VendorSpec {
    super::super::spec("codex").unwrap()
}

fn codex_old() -> &'static VendorSpec {
    super::super::spec("codex-old").unwrap()
}

fn run(layout: &Layout, fetcher: &Fake, policy: &Policy, spec: &'static VendorSpec) -> Outcome {
    run_with(layout, fetcher, policy, &world::trust(), spec, false)
}

fn run_with(
    layout: &Layout,
    fetcher: &Fake,
    policy: &Policy,
    trust: &Trust,
    spec: &'static VendorSpec,
    held: bool,
) -> Outcome {
    run_indexed(layout, fetcher, policy, trust, spec, held, None)
}

fn run_indexed(
    layout: &Layout,
    fetcher: &Fake,
    policy: &Policy,
    trust: &Trust,
    spec: &'static VendorSpec,
    held: bool,
    legacy_index: Option<&TrustedIndex>,
) -> Outcome {
    let lane = Lane {
        layout,
        fetcher,
        triple: world::triple(),
        policy,
        trust,
        legacy_index,
        asked: false,
        now: NOW,
        download_turn: None,
    };
    install_one(&lane, spec, held)
}

fn run_asked(layout: &Layout, fetcher: &Fake, spec: &'static VendorSpec) -> Outcome {
    let lane = Lane {
        layout,
        fetcher,
        triple: world::triple(),
        policy: &Policy::default(),
        trust: &world::trust(),
        legacy_index: None,
        asked: true,
        now: NOW,
        download_turn: None,
    };
    install_one(&lane, spec, false)
}

fn legacy_claude(l: &Layout, build: u64) {
    let legacy = l.build_dir("claude", build);
    std::fs::create_dir_all(legacy.join("bin")).unwrap();
    std::fs::write(legacy.join(world::bin("claude")), native_exe("legacy")).unwrap();
    crate::activate::install_shims(
        l,
        &legacy,
        &["claude".to_string()],
        crate::activate::Aliases::Off,
    )
    .unwrap();
    crate::store::mark_build_ready(&legacy).unwrap();
}

fn shim_build(layout: &Layout, program: &str) -> Option<u64> {
    crate::ops::active_builds(layout).get(program).copied()
}

fn assert_no_build_id(line: &str) {
    let digits: Vec<&str> = line
        .split(|c: char| !c.is_ascii_digit())
        .filter(|w| w.len() >= 19)
        .collect();
    assert!(digits.is_empty(), "a vendor build id on a line: {line}");
}

#[test]
fn a_fresh_claude_is_verified_staged_recorded_and_shimmed_with_its_compiled_env() {
    let l = world::layout("fresh-claude");
    let f = Fake::default();
    let payload = native_exe("claude 2.1.281");
    f.publish_claude("2.1.281", &payload);
    let o = run(&l, &f, &Policy::default(), claude());
    assert!(
        matches!(
            o.verdict,
            Verdict::Installed {
                reason: InstallReason::Fresh,
                ..
            }
        ),
        "{o:?}"
    );
    let dir = world::build_dir(&l, "claude", "2.1.281");
    assert_eq!(shim_build(&l, "claude"), Some(v("2.1.281").build_id()));
    assert_eq!(
        std::fs::read(dir.join(world::bin("claude"))).unwrap(),
        payload
    );
    let record = complete_record(&dir).expect("a complete record");
    assert_eq!(record.version, v("2.1.281"));
    let modes = crate::store::declared_modes(&dir).unwrap();
    assert_eq!(
        record.tree_root,
        crate::tree::tree_root_declared(&dir, &modes).unwrap()
    );
    assert_eq!(record.sha256, world::sha(&payload));
    assert_eq!(
        record.apple_team.as_deref(),
        cfg!(target_os = "macos").then_some("Q6L2SF6YDW")
    );
    assert!(record.build_date.is_some(), "claude's signed buildDate");
    let env = crate::shim_env::read_sidecar(&dir);
    assert_eq!(
        env.entries(),
        [
            ("DISABLE_AUTOUPDATER".to_string(), "1".to_string()),
            (
                "CLAUDE_CODE_DISABLE_FEEDBACK_SURVEY".to_string(),
                "1".to_string()
            ),
        ]
    );
    let shim =
        std::fs::read_to_string(l.shim(&crate::store::ToolName::new("claude").unwrap())).unwrap();
    assert!(shim.contains("DISABLE_AUTOUPDATER"), "{shim}");
    assert_eq!(
        crate::store::recorded_exposes(&dir),
        Some(vec![crate::store::ToolName::new("claude").unwrap()]),
        "the vendor lane records the commands its build puts on PATH, as every landing does"
    );
    let stamp = ProgramStamp::read(&l, "claude").unwrap();
    assert_eq!(stamp.last_head, Some(v("2.1.281")));
    assert_eq!(stamp.high_water, Some(v("2.1.281")));
    assert_eq!(
        f.downloads.borrow().as_slice(),
        [Fake::claude_payload_url("2.1.281")]
    );
}

use world::sha512_hex;

fn agy() -> &'static VendorSpec {
    super::super::spec("agy").unwrap()
}

fn muse() -> &'static VendorSpec {
    super::super::spec("muse").unwrap()
}

fn publish_agy(f: &Fake, version: &str, exe: &[u8]) -> String {
    let tarball = world::tarball(&[("antigravity", exe, 0o755)]);
    let (_, dir, file) = resolve::agy_platform(world::triple()).unwrap();
    let release = format!(
        "https://storage.googleapis.com/antigravity-public/antigravity-cli/{version}-42/{dir}/"
    );
    let url = format!("{release}{file}");
    let manifest = format!(
        "{{\"version\":\"{version}\",\"url\":\"{url}\",\"sha512\":\"{}\"}}",
        sha512_hex(&tarball)
    );
    f.doc(agy().head_url, manifest.as_bytes());
    f.doc(&format!("{release}manifest.json"), manifest.as_bytes());
    f.payload(&url, &tarball);
    url
}

/// Where the vendor serves no build for the real host (`muse` off darwin, `grok` on
/// Windows), assert `install_one` answers `Unavailable` without asking any host, fetching a
/// byte, writing a stamp or logging a digest, then let the caller return.
fn unserved_here(spec: &'static VendorSpec) -> bool {
    let host = crate::cli::current_triple();
    if spec.serves(host) {
        return false;
    }
    let l = world::layout(&format!("unserved-{}", spec.program));
    let f = Fake::default();
    let lane = Lane {
        layout: &l,
        fetcher: &f,
        triple: host,
        policy: &Policy::default(),
        trust: &world::trust(),
        legacy_index: None,
        asked: true,
        now: NOW,
        download_turn: None,
    };
    let o = install_one(&lane, spec, false);
    assert!(matches!(o.verdict, Verdict::Unavailable { .. }), "{o:?}");
    assert!(!o.is_failure(), "{o:?}");
    assert!(f.gets.borrow().is_empty(), "no host was asked");
    assert!(f.downloads.borrow().is_empty(), "nothing fetched");
    assert!(
        ProgramStamp::read(&l, spec.program).is_none(),
        "nothing recorded"
    );
    assert!(
        !super::super::digests::digests_path(&l, spec.program).exists(),
        "nothing logged"
    );
    true
}

fn publish_muse(f: &Fake, version: &str, exe: &[u8]) {
    let (key, file) = resolve::muse_platform(world::triple()).expect("muse is served on this host");
    let version = v(version);
    let url = resolve::muse_url(version, file);
    let manifest_url = resolve::muse_url(version, "manifest.json");
    let channel = format!(
        "{{\"channel\":\"muse-stable\",\"version\":\"{version}\",\"manifest_url\":\
         \"{manifest_url}\",\"urgency\":\"none\",\"min_version\":null}}"
    );
    let manifest = format!(
        "{{\"version\":\"{version}\",\"checksum_algorithm\":\"sha256\",\"artifacts\":\
         {{\"{key}\":{{\"url\":\"{}\",\"checksum\":\"{}\",\"size\":{}}}}}}}",
        url.replace('/', "\\/"),
        world::sha(exe),
        exe.len()
    );
    f.doc(muse().head_url, channel.as_bytes());
    f.doc(&manifest_url, manifest.as_bytes());
    f.payload(&url, exe);
}

fn kimi() -> &'static VendorSpec {
    super::super::spec("kimi").unwrap()
}

fn grok() -> &'static VendorSpec {
    super::super::spec("grok").unwrap()
}

/// Publish kimi `version` as Moonshot's latest: the head, the CDN manifest and the
/// GitHub release manifest (answered through GitHub's storage) naming `exe`'s sha256,
/// and the CDN payload; its URL.
fn publish_kimi(f: &Fake, version: &str, exe: &[u8]) -> String {
    let (key, file) = resolve::kimi_platform(world::triple()).unwrap();
    let tag = format!("@moonshot-ai/kimi-code@{version}");
    let manifest = format!(
        "{{\"version\":\"{version}\",\"tag\":\"{tag}\",\"platforms\":{{\"{key}\":\
         {{\"filename\":\"{file}\",\"checksum\":\"{}\"}}}}}}",
        world::sha(exe)
    );
    f.doc(kimi().head_url, format!("{version}\n").as_bytes());
    let cdn = format!("https://cdn.kimi.com/kimi-code/binaries/{version}/");
    f.doc(&format!("{cdn}manifest.json"), manifest.as_bytes());
    let github =
        format!("https://github.com/MoonshotAI/kimi-code/releases/download/{tag}/manifest.json");
    f.doc(&github, manifest.as_bytes());
    f.redirect(
        &github,
        "https://release-assets.githubusercontent.com/github-production-release-asset/1/2",
    );
    let url = format!("{cdn}{file}");
    f.payload(&url, exe);
    url
}

/// Publish grok `version` of `exe`: npm's signed tarball and xAI's bucket copy
/// ([`Fake::publish_grok`]). Callers guard unserved hosts with [`unserved_here`].
fn publish_grok(f: &Fake, version: &str, exe: &[u8]) -> world::GrokServed {
    f.publish_grok(version, exe)
        .expect("grok is served on this host")
}

fn staged(l: &Layout, program: &str) -> Vec<String> {
    std::fs::read_dir(l.staging_dir(program))
        .map(|d| {
            d.flatten()
                .map(|e| e.file_name().to_string_lossy().into_owned())
                .collect()
        })
        .unwrap_or_default()
}

#[test]
fn a_fresh_kimi_lands_the_binary_both_moonshot_manifests_name() {
    let l = world::layout("fresh-kimi");
    let f = Fake::default();
    let exe = native_exe("kimi 2.3.2");
    let url = publish_kimi(&f, "2.3.2", &exe);
    let o = run(&l, &f, &Policy::default(), kimi());
    assert!(
        matches!(
            o.verdict,
            Verdict::Installed {
                reason: InstallReason::Fresh,
                ..
            }
        ),
        "{o:?}"
    );
    let build = world::build_dir(&l, "kimi", "2.3.2");
    assert_eq!(std::fs::read(build.join(world::bin("kimi"))).unwrap(), exe);
    let record = complete_record(&build).expect("a complete record");
    assert_eq!(
        (record.sha256.as_str(), record.anchor),
        (
            world::sha(&exe).as_str(),
            super::super::Anchor::MoonshotTwoHost
        )
    );
    assert_eq!(record.source_url, url);
    let env = crate::shim_env::read_sidecar(&build);
    assert_eq!(env.spelled(), "KIMI_CODE_NO_AUTO_UPDATE=1");
    let shim =
        std::fs::read_to_string(l.shim(&crate::store::ToolName::new("kimi").unwrap())).unwrap();
    assert!(shim.contains("KIMI_CODE_NO_AUTO_UPDATE"), "{shim}");
    assert_eq!(f.downloads.borrow().as_slice(), [url]);
    let log = super::super::digests::digests_path(&l, "kimi");
    assert!(
        log.ends_with("vendor/digests-kimi.toml"),
        "{}",
        log.display()
    );
    assert!(
        std::fs::read_to_string(&log)
            .unwrap()
            .contains(&world::sha(&exe))
    );
}

/// A fresh grok: npm's registry-signed tarball (its sha512 logged before a byte moved)
/// lands its decoded `bin/grok` once xAI's bucket `.zst` decoded to the same bytes; the
/// bucket's raw binary is only sized, and nothing is left in staging.
#[test]
fn a_fresh_grok_lands_npms_signed_binary_once_xais_bucket_agrees() {
    if unserved_here(grok()) {
        return;
    }
    let l = world::layout("fresh-grok");
    let f = Fake::default();
    let exe = native_exe("grok 1.0.70");
    let served = publish_grok(&f, "1.0.70", &exe);
    let o = run(&l, &f, &Policy::default(), grok());
    assert!(
        matches!(
            o.verdict,
            Verdict::Installed {
                reason: InstallReason::Fresh,
                ..
            }
        ),
        "{o:?}"
    );
    assert_eq!(
        f.downloads.borrow().as_slice(),
        [served.zst.clone(), served.tarball.clone()],
        "the bucket's .zst, then npm's tarball; never {}",
        served.binary
    );
    let build = world::build_dir(&l, "grok", "1.0.70");
    assert_eq!(std::fs::read(build.join("bin/grok")).unwrap(), exe);
    let bin: Vec<_> = std::fs::read_dir(build.join("bin"))
        .unwrap()
        .flatten()
        .map(|e| e.file_name())
        .collect();
    assert_eq!(bin, ["grok"], "the .br is decoded, nothing else is laid");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        let mode = std::fs::metadata(build.join("bin/grok"))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o111, 0o111, "{mode:o}");
        let sealed = std::fs::metadata(build.join("bin"))
            .unwrap()
            .permissions()
            .mode()
            & 0o7777;
        assert_eq!(
            sealed,
            crate::store::SEALED_BIN_MODE,
            "a one-binary bin/: {sealed:o}"
        );
    }
    let record = complete_record(&build).expect("a complete record");
    assert_eq!(
        (
            record.sha512.as_str(),
            record.sha256.as_str(),
            record.anchor
        ),
        (
            served.sha512.as_str(),
            "",
            super::super::Anchor::XaiNpmRegistry
        )
    );
    assert_eq!(record.source_url, served.tarball);
    assert_eq!(
        record.measured_sha256.as_deref(),
        Some(world::sha(&exe).as_str())
    );
    assert_eq!(record.agreed_with.as_deref(), Some(served.zst.as_str()));
    assert_eq!(
        record.apple_team.as_deref(),
        cfg!(target_os = "macos").then_some("5Y6N3AJ54S")
    );
    let log = std::fs::read_to_string(super::super::digests::digests_path(&l, "grok")).unwrap();
    assert!(log.contains(&served.sha512), "{log}");
    assert_eq!(
        crate::shim_env::read_sidecar(&build).spelled(),
        "GROK_DISABLE_AUTOUPDATER=1"
    );
    assert!(staged(&l, "grok").is_empty(), "{:?}", staged(&l, "grok"));
}

/// A build xAI's bucket does not serve: the binary npm's signed tarball decodes to is
/// refused when the bucket's copy (as long, other bytes) is not it, and memoized for that
/// tarball's sha512, which was logged before the download; a person clearing the memo
/// lands it once the bucket agrees.
#[test]
fn a_binary_the_bucket_does_not_agree_with_is_refused_and_memoized() {
    if unserved_here(grok()) {
        return;
    }
    let l = world::layout("grok-disagree");
    let f = Fake::default();
    let exe = native_exe("grok 1.0.70");
    let served = publish_grok(&f, "1.0.70", &exe);
    let mut other = exe.clone();
    *other.last_mut().unwrap() ^= 1;
    f.publish_grok_bucket("1.0.70", &exe, &other);
    let o = run(&l, &f, &Policy::default(), grok());
    assert!(matches!(o.verdict, Verdict::Refused { .. }), "{o:?}");
    assert!(
        o.line().contains(&format!(
            "is not xAI's own build, sha256 {} ({} bytes)",
            world::sha(&other),
            other.len()
        )),
        "{}",
        o.line()
    );
    assert_eq!(f.downloads.borrow().len(), 2, "the .zst and the tarball");
    let stamp = ProgramStamp::read(&l, "grok").unwrap();
    assert_eq!(
        stamp.refused.as_ref().map(|(v, d)| (*v, d.as_str())),
        Some((v("1.0.70"), served.sha512.as_str()))
    );
    let log = std::fs::read_to_string(super::super::digests::digests_path(&l, "grok")).unwrap();
    assert!(log.contains(&served.sha512), "first seen at plan: {log}");
    assert!(staged(&l, "grok").is_empty(), "{:?}", staged(&l, "grok"));
    assert_eq!(shim_build(&l, "grok"), None, "nothing laid");
    assert!(!crate::store::build_is_complete(&world::build_dir(
        &l, "grok", "1.0.70"
    )));
    let o = run(&l, &f, &Policy::default(), grok());
    assert!(
        o.line().contains("refused these exact bytes"),
        "{}",
        o.line()
    );
    assert_eq!(f.downloads.borrow().len(), 2, "not fetched again");
    let memo = crate::store::stage_refusal(&world::build_dir(&l, "grok", "1.0.70"))
        .expect("the store's memo of the refusal");
    assert!(
        memo.why.contains(&world::sha(&other)) && memo.why.contains(&world::sha(&exe)),
        "the memo keeps both digests whole: {}",
        memo.why
    );
    ProgramStamp::clear_refusal(&l, "grok").unwrap();
    assert_eq!(
        crate::store::clear_stage_refusals(&l, "grok"),
        1,
        "a person's retry"
    );
    f.publish_grok_bucket("1.0.70", &exe, &exe);
    let o = run(&l, &f, &Policy::default(), grok());
    assert!(matches!(o.verdict, Verdict::Installed { .. }), "{o:?}");
}

/// npm's tarball must be the bytes its signed document names: one flipped byte, at the
/// size the registry answered, is a digest mismatch, refused before anything is staged.
/// As for every vendor the first retry is free; once two passes saw the same bytes the
/// refusal binds, and the bucket's copy is not fetched for a tarball the landing would
/// refuse before its download.
#[test]
fn a_tarball_other_than_the_signed_one_is_refused_before_staging() {
    if unserved_here(grok()) {
        return;
    }
    let l = world::layout("grok-tampered");
    let f = Fake::default();
    let exe = native_exe("grok 1.0.70");
    let served = publish_grok(&f, "1.0.70", &exe);
    let mut tgz = world::grok_tgz(world::grok_platform().unwrap(), "1.0.70", &exe);
    assert_eq!(
        world::sha512_hex(&tgz),
        served.sha512,
        "the signed tarball, rebuilt"
    );
    let middle = tgz.len() / 2;
    tgz[middle] ^= 1;
    f.payload(&served.tarball, &tgz);
    let o = run(&l, &f, &Policy::default(), grok());
    assert!(matches!(o.verdict, Verdict::Refused { .. }), "{o:?}");
    assert!(o.line().contains("sha512"), "{}", o.line());
    assert_eq!(f.downloads.borrow().len(), 2, "the .zst and the tarball");
    assert!(staged(&l, "grok").is_empty(), "{:?}", staged(&l, "grok"));
    assert_eq!(shim_build(&l, "grok"), None, "nothing laid");
    assert!(!crate::store::build_is_complete(&world::build_dir(
        &l, "grok", "1.0.70"
    )));
    let o = run(&l, &f, &Policy::default(), grok());
    assert!(matches!(o.verdict, Verdict::Refused { .. }), "{o:?}");
    assert_eq!(f.downloads.borrow().len(), 4, "the first retry is free");
    let o = run(&l, &f, &Policy::default(), grok());
    assert!(matches!(o.verdict, Verdict::Refused { .. }), "{o:?}");
    assert!(o.line().contains("not downloaded again"), "{}", o.line());
    assert_eq!(
        f.downloads.borrow().len(),
        4,
        "two identical verdicts: neither download again"
    );
}

/// A bucket copy that is not one frame decoding to the binary's declared size and checksum
/// fails the pass before npm's tarball is fetched, memoizing nothing (a torn or
/// half-published copy heals), and the next pass lands once it is.
#[test]
fn a_bucket_copy_that_does_not_decode_fails_unmemoized() {
    let exe = native_exe("grok 1.0.70");
    let mut longer = exe.clone();
    longer.push(b'!');
    let checksum_flipped = {
        let mut zst = world::zstd(&exe);
        *zst.last_mut().unwrap() ^= 1;
        zst
    };
    let unchecked = {
        let mut zst = Vec::new();
        aterm_zstd::frame::write_raw(&mut zst, &exe, aterm_zstd::frame::Checksum::Omitted).unwrap();
        zst
    };
    let one_frame = "more than the one zstd frame";
    let trailing_empty = [world::zstd(&exe), world::zstd(b"")].concat();
    let skippable = [0x184D_2A50u32.to_le_bytes(), 7u32.to_le_bytes()].concat();
    let trailing_skippable = [&world::zstd(&exe)[..], &skippable, b"padding"].concat();
    let leading_skippable = [&skippable[..], b"padding", &world::zstd(&exe)].concat();
    if unserved_here(grok()) {
        return;
    }
    for (label, zst, why) in [
        ("declares", world::zstd(&longer), "its zstd frame declares"),
        ("checksum", checksum_flipped, "could not be checked"),
        ("unchecked", unchecked, "could not be checked"),
        ("empty", Vec::new(), "no content length"),
        ("trailing-empty", trailing_empty, one_frame),
        ("trailing-skippable", trailing_skippable, one_frame),
        ("leading-skippable", leading_skippable, "not a frame"),
    ] {
        let l = world::layout(&format!("grok-zst-{label}"));
        let f = Fake::default();
        let served = publish_grok(&f, "1.0.70", &exe);
        f.payload(&served.zst, &zst);
        let o = run(&l, &f, &Policy::default(), grok());
        assert!(
            !matches!(o.verdict, Verdict::Installed { .. }),
            "{label}: {o:?}"
        );
        assert!(o.line().contains(why), "{label}: {}", o.line());
        assert!(
            !f.downloads.borrow().contains(&served.tarball),
            "{label}: npm's tarball is not fetched"
        );
        assert_eq!(
            ProgramStamp::read(&l, "grok").and_then(|s| s.refused),
            None,
            "{label}: nothing memoized"
        );
        assert!(
            staged(&l, "grok").is_empty(),
            "{label}: {:?}",
            staged(&l, "grok")
        );
        f.publish_grok_bucket("1.0.70", &exe, &exe);
        let o = run(&l, &f, &Policy::default(), grok());
        assert!(
            matches!(o.verdict, Verdict::Installed { .. }),
            "{label}: {o:?}"
        );
    }
}

/// A grok landing's disk gate holds room for npm's tarball, the bucket's `.zst` and the
/// binary at once; planning it downloads nothing.
#[test]
fn a_grok_landing_needs_room_for_the_tarball_the_bucket_copy_and_the_binary() {
    if unserved_here(grok()) {
        return;
    }
    let l = world::layout("grok-room");
    let f = Fake::default();
    let exe = native_exe("grok 1.0.70");
    let served = publish_grok(&f, "1.0.70", &exe);
    let lane = Lane {
        layout: &l,
        fetcher: &f,
        triple: world::triple(),
        policy: &Policy::default(),
        trust: &world::trust(),
        legacy_index: None,
        asked: false,
        now: NOW,
        download_turn: None,
    };
    let Step::Land(pending) = plan_one(&lane, grok(), false) else {
        panic!("a grok to land");
    };
    let tgz = world::grok_tgz(world::grok_platform().unwrap(), "1.0.70", &exe).len() as u64;
    let zst = world::zstd(&exe).len() as u64;
    let need = pending.landing_bytes().need();
    assert_eq!(need, tgz + zst + exe.len() as u64);
    assert_eq!(pending.download_bytes(), tgz + zst);
    assert_eq!(pending.disk_bytes(), Some(exe.len() as u64));
    assert!(f.downloads.borrow().is_empty(), "{:?}", served.tarball);
}

/// The agreement step downloads the bucket's `.zst` alone, decodes it to the binary's
/// sha256 and length, and deletes it before npm's tarball is requested.
#[test]
fn the_bucket_copy_is_decoded_and_deleted_before_the_tarball_is_fetched() {
    if unserved_here(grok()) {
        return;
    }
    let l = world::layout("grok-agree");
    let f = Fake::default();
    let exe = native_exe("grok 1.0.70");
    let served = publish_grok(&f, "1.0.70", &exe);
    let lane = Lane {
        layout: &l,
        fetcher: &f,
        triple: world::triple(),
        policy: &Policy::default(),
        trust: &world::trust(),
        legacy_index: None,
        asked: false,
        now: NOW,
        download_turn: None,
    };
    let Step::Land(pending) = plan_one(&lane, grok(), false) else {
        panic!("a grok to land");
    };
    let agreed = agree(&lane, &pending)
        .unwrap()
        .expect("grok has a bucket copy");
    assert_eq!(
        agreed,
        Agreement {
            url: served.zst.clone(),
            sha256: world::sha(&exe),
            len: exe.len() as u64,
        }
    );
    assert_eq!(f.downloads.borrow().as_slice(), [served.zst]);
    assert!(staged(&l, "grok").is_empty(), "{:?}", staged(&l, "grok"));
}

/// A grok landing's bucket copy and npm's tarball are one turn on the link: it keeps the
/// turn it took for the copy, so the tarball's download, which waits on that same ticket,
/// starts at once, and the next ticket's transfer starts only once the landing ends,
/// landed or failed.
#[test]
fn a_grok_landing_holds_its_download_turn_from_the_bucket_copy_to_the_tarball() {
    if unserved_here(grok()) {
        return;
    }
    let turns = crate::flow::DownloadTurns::default();
    let (first, second, third) = (turns.ticket(), turns.ticket(), turns.ticket());
    let (policy, trust) = (Policy::default(), world::trust());
    let lane = |l, f, ticket| Lane {
        layout: l,
        fetcher: f,
        triple: world::triple(),
        policy: &policy,
        trust: &trust,
        legacy_index: None,
        asked: false,
        now: NOW,
        download_turn: Some(ticket),
    };
    let l = world::layout("grok-turn");
    let f = Fake::default();
    publish_grok(&f, "1.0.70", &native_exe("grok 1.0.70"));
    let landing = lane(&l, &f, &first);
    let Step::Land(pending) = plan_one(&landing, grok(), false) else {
        panic!("a grok to land");
    };
    agree(&landing, &pending)
        .unwrap()
        .expect("grok has a bucket copy");
    assert!(
        first.holds_the_link() && !second.holds_the_link(),
        "the bucket copy gave up the turn npm's tarball still needs"
    );
    let o = land_one(&landing, &pending);
    assert!(matches!(o.verdict, Verdict::Installed { .. }), "{o:?}");
    assert!(
        second.holds_the_link(),
        "the landing ended: the next transfer may start"
    );
    let l = world::layout("grok-turn-failed");
    let f = Fake::default();
    publish_grok(&f, "1.0.70", &native_exe("grok 1.0.70"));
    f.downloads_fail.set(true);
    let failing = lane(&l, &f, &second);
    let Step::Land(pending) = plan_one(&failing, grok(), false) else {
        panic!("a grok to land");
    };
    let o = land_one(&failing, &pending);
    assert!(matches!(o.verdict, Verdict::Failed { .. }), "{o:?}");
    assert!(third.holds_the_link(), "a failed landing gave up its turn");
}

/// A short bucket download (fewer bytes than the bucket sized) fails unmemoized and lands on
/// the next pass.
#[test]
fn a_short_bucket_download_fails_unmemoized_and_lands_on_the_next_pass() {
    if unserved_here(grok()) {
        return;
    }
    let l = world::layout("grok-short");
    let f = Fake::default();
    let exe = native_exe("grok 1.0.70");
    let served = publish_grok(&f, "1.0.70", &exe);
    let lane = Lane {
        layout: &l,
        fetcher: &f,
        triple: world::triple(),
        policy: &Policy::default(),
        trust: &world::trust(),
        legacy_index: None,
        asked: false,
        now: NOW,
        download_turn: None,
    };
    let Step::Land(pending) = plan_one(&lane, grok(), false) else {
        panic!("a grok to land");
    };
    let zst = world::zstd(&exe);
    f.payload(&served.zst, &zst[..zst.len() - 1]);
    let o = land_one(&lane, &pending);
    assert!(matches!(o.verdict, Verdict::Failed { .. }), "{o:?}");
    assert!(o.line().contains("of its"), "{}", o.line());
    assert_eq!(
        f.downloads.borrow().as_slice(),
        std::slice::from_ref(&served.zst)
    );
    assert!(staged(&l, "grok").is_empty(), "{:?}", staged(&l, "grok"));
    assert_eq!(ProgramStamp::read(&l, "grok").unwrap().refused, None);
    f.payload(&served.zst, &zst);
    let o = run(&l, &f, &Policy::default(), grok());
    assert!(matches!(o.verdict, Verdict::Installed { .. }), "{o:?}");
}

/// A document or copy not served (npm's version document, the bucket's binary or its
/// `.zst`) is no verdict: nothing is downloaded or memoized, and the build lands once it
/// is served.
#[test]
fn an_unserved_npm_document_or_bucket_copy_is_no_verdict() {
    if unserved_here(grok()) {
        return;
    }
    let exe = native_exe("grok 1.0.70");
    for which in ["npm", "binary", "zst", "tarball"] {
        let l = world::layout(&format!("grok-unserved-{which}"));
        let f = Fake::default();
        let served = publish_grok(&f, "1.0.70", &exe);
        let version_doc = world::grok_platform().unwrap().version_doc(v("1.0.70"));
        let doc = f.body(&version_doc);
        match which {
            "npm" => f.forget(&version_doc),
            "binary" => f.forget_payload(&served.binary),
            "zst" => f.forget_payload(&served.zst),
            _ => f.forget_payload(&served.tarball),
        }
        let o = run(&l, &f, &Policy::default(), grok());
        assert!(
            matches!(o.verdict, Verdict::Unreachable { why: Some(_), .. }),
            "{which}: {o:?}"
        );
        assert!(!o.is_failure(), "{which}");
        assert!(f.downloads.borrow().is_empty(), "{which}: nothing fetched");
        assert_eq!(
            ProgramStamp::read(&l, "grok").and_then(|s| s.refused),
            None,
            "{which}"
        );
        f.doc(&version_doc, &doc);
        f.publish_grok_bucket("1.0.70", &exe, &exe);
        f.payload(
            &served.tarball,
            &world::grok_tgz(world::grok_platform().unwrap(), "1.0.70", &exe),
        );
        let o = run(&l, &f, &Policy::default(), grok());
        assert!(
            matches!(o.verdict, Verdict::Installed { .. }),
            "{which}: {o:?}"
        );
    }
}

/// npm's version document signed by a key aterm does not ship (the test key, against the
/// compiled registry key) is refused before a byte moves, naming the key.
#[test]
fn a_version_document_under_a_key_aterm_does_not_ship_is_refused_before_any_download() {
    if unserved_here(grok()) {
        return;
    }
    let l = world::layout("grok-unknown-key");
    let f = Fake::default();
    publish_grok(&f, "1.0.70", &native_exe("grok 1.0.70"));
    let production = Trust {
        npm_keys: super::super::npm::NPM_REGISTRY_KEYS,
        ..world::trust()
    };
    let o = run_with(&l, &f, &Policy::default(), &production, grok(), false);
    assert!(matches!(o.verdict, Verdict::Refused { .. }), "{o:?}");
    assert!(
        o.line().contains(&format!(
            "aterm does not ship ({})",
            world::npm_test_keys()[0].keyid
        )),
        "{}",
        o.line()
    );
    assert!(f.downloads.borrow().is_empty(), "nothing fetched");
    assert!(!super::super::digests::digests_path(&l, "grok").exists());
}

/// A tarball npm signed but the stager refuses — a member out of place or of another kind,
/// a binary decoding to other than the bucket's size, a package.json naming another
/// version — lays nothing.
#[test]
fn a_signed_tarball_the_stager_refuses_lays_nothing() {
    if unserved_here(grok()) {
        return;
    }
    let platform = world::grok_platform().expect("grok is served on this host");
    let exe = native_exe("grok 1.0.70");
    let mut longer = exe.clone();
    longer.push(b'!');
    let json = world::grok_package_json(platform, "1.0.70");
    let other_version = world::grok_package_json(platform, "1.0.69");
    let br = world::brotli(&exe);
    let mut trailing = br.clone();
    trailing.push(0);
    let cases: Vec<(&str, Vec<u8>, &[u8], &str)> = vec![
        (
            "symlink",
            world::npm_tgz(&[
                ("package/package.json", b'0', "", json.as_bytes(), 0o644),
                ("package/bin/grok.br", b'2', "../../evil", b"", 0o777),
            ]),
            &exe,
            "an npm binary package holds only regular files",
        ),
        (
            "directory",
            world::npm_tgz(&[
                ("package/", b'5', "", b"", 0o755),
                ("package/package.json", b'0', "", json.as_bytes(), 0o644),
                ("package/bin/grok.br", b'0', "", &br, 0o644),
            ]),
            &exe,
            "an npm binary package holds only regular files",
        ),
        (
            "beside-in-bin",
            world::npm_tgz(&[
                ("package/package.json", b'0', "", json.as_bytes(), 0o644),
                ("package/bin/grok.br", b'0', "", &br, 0o644),
                ("package/bin/helper", b'0', "", b"x", 0o755),
            ]),
            &exe,
            "\"package/bin/helper\"",
        ),
        (
            "deeper",
            world::npm_tgz(&[
                ("package/package.json", b'0', "", json.as_bytes(), 0o644),
                ("package/bin/grok.br", b'0', "", &br, 0o644),
                ("package/lib/x.js", b'0', "", b"x", 0o644),
            ]),
            &exe,
            "\"package/lib/x.js\"",
        ),
        (
            "second-br",
            world::npm_tgz(&[
                ("package/package.json", b'0', "", json.as_bytes(), 0o644),
                ("package/bin/grok.br", b'0', "", &br, 0o644),
                ("package/bin/grok.br", b'0', "", &br, 0o644),
            ]),
            &exe,
            "or a second one",
        ),
        (
            "no-manifest",
            world::npm_tgz(&[("package/bin/grok.br", b'0', "", &br, 0o644)]),
            &exe,
            "holds no package/package.json",
        ),
        (
            "trailing",
            world::npm_tgz(&[
                ("package/package.json", b'0', "", json.as_bytes(), 0o644),
                ("package/bin/grok.br", b'0', "", &trailing, 0o644),
            ]),
            &exe,
            "bytes follow the stream's last meta-block",
        ),
        (
            "shorter",
            world::grok_tgz(platform, "1.0.70", &exe),
            &longer,
            "decodes to a length other than the binary's",
        ),
        (
            "longer",
            world::grok_tgz(platform, "1.0.70", &longer),
            &exe,
            "decodes to a length other than the binary's",
        ),
        (
            "manifest",
            world::npm_tgz(&[
                (
                    "package/package.json",
                    b'0',
                    "",
                    other_version.as_bytes(),
                    0o644,
                ),
                ("package/bin/grok.br", b'0', "", &br, 0o644),
            ]),
            &exe,
            "is version \"1.0.69\", not 1.0.70",
        ),
    ];
    for (label, tgz, bucket, why) in cases {
        let l = world::layout(&format!("grok-tar-{label}"));
        let f = Fake::default();
        f.publish_grok_tarball("1.0.70", &tgz);
        f.publish_grok_bucket("1.0.70", bucket, bucket);
        let o = run(&l, &f, &Policy::default(), grok());
        assert!(
            !matches!(o.verdict, Verdict::Installed { .. }),
            "{label}: {o:?}"
        );
        assert!(
            o.line().contains(why),
            "{label}: want {why:?} in {}",
            o.line()
        );
        assert_eq!(shim_build(&l, "grok"), None, "{label}");
        assert!(
            !crate::store::build_is_complete(&world::build_dir(&l, "grok", "1.0.70")),
            "{label}"
        );
    }
}

/// The team pin, for each new row: a darwin Mach-O signed by another team is refused and
/// memoized, and its version is not fetched again until a person asks; kimi's and grok's
/// documented digests were logged before the download, as codex's is.
#[test]
fn a_build_signed_by_another_team_is_refused_and_memoized_for_kimi_and_grok() {
    let refusing = world::trust_refusing_signers();
    let l = world::layout("kimi-team");
    let f = Fake::default();
    publish_kimi(&f, "2.3.2", &native_exe("not Moonshot's"));
    let o = run_with(&l, &f, &Policy::default(), &refusing, kimi(), false);
    if !cfg!(target_os = "macos") {
        assert!(
            matches!(o.verdict, Verdict::Installed { .. }),
            "no gate off darwin: {o:?}"
        );
        return;
    }
    assert!(o.line().contains("signer refused"), "{}", o.line());
    let downloads = f.downloads.borrow().len();
    run_with(&l, &f, &Policy::default(), &refusing, kimi(), false);
    assert_eq!(f.downloads.borrow().len(), downloads, "memoized");
    assert_eq!(shim_build(&l, "kimi"), None);

    let l = world::layout("grok-team");
    let f = Fake::default();
    let served = publish_grok(&f, "1.0.70", &native_exe("not xAI's"));
    let o = run_with(&l, &f, &Policy::default(), &refusing, grok(), false);
    assert!(o.line().contains("signer refused"), "{}", o.line());
    let log = super::super::digests::digests_path(&l, "grok");
    assert!(
        std::fs::read_to_string(&log)
            .unwrap()
            .contains(&served.sha512),
        "npm's signed digest was logged before the download"
    );
    assert!(staged(&l, "grok").is_empty(), "{:?}", staged(&l, "grok"));
    let downloads = f.downloads.borrow().len();
    let o = run_with(&l, &f, &Policy::default(), &refusing, grok(), false);
    assert!(
        o.line().contains("refused these exact bytes"),
        "{}",
        o.line()
    );
    assert_eq!(f.downloads.borrow().len(), downloads, "not fetched again");
    ProgramStamp::clear_refusal(&l, "grok").unwrap();
    assert_eq!(
        crate::store::clear_stage_refusals(&l, "grok"),
        1,
        "a person's retry"
    );
    let o = run(&l, &f, &Policy::default(), grok());
    assert!(matches!(o.verdict, Verdict::Installed { .. }), "{o:?}");
}

#[test]
fn a_fresh_agy_lands_its_one_file_under_the_sha512_both_google_hosts_name() {
    let l = world::layout("fresh-agy");
    let f = Fake::default();
    let exe = native_exe("agy 1.3.7");
    let url = publish_agy(&f, "1.3.7", &exe);
    let sha512 = sha512_hex(&world::tarball(&[("antigravity", &exe, 0o755)]));
    let o = run(&l, &f, &Policy::default(), agy());
    assert!(
        matches!(
            o.verdict,
            Verdict::Installed {
                reason: InstallReason::Fresh,
                ..
            }
        ),
        "{o:?}"
    );
    let build = world::build_dir(&l, "agy", "1.3.7");
    assert_eq!(std::fs::read(build.join(world::bin("agy"))).unwrap(), exe);
    let record = complete_record(&build).expect("a complete record");
    assert_eq!(
        (record.sha256.as_str(), record.sha512.as_str()),
        ("", sha512.as_str())
    );
    assert_eq!(record.source_url, url);
    let env = crate::shim_env::read_sidecar(&build);
    assert_eq!(env.spelled(), "AGY_CLI_DISABLE_AUTO_UPDATE=true");
    let shim =
        std::fs::read_to_string(l.shim(&crate::store::ToolName::new("agy").unwrap())).unwrap();
    assert!(shim.contains("AGY_CLI_DISABLE_AUTO_UPDATE"), "{shim}");
    assert_eq!(f.downloads.borrow().as_slice(), [url]);
}

#[test]
fn muse_lands_where_it_is_served_and_is_unavailable_elsewhere() {
    let elsewhere = "x86_64-unknown-linux-gnu";
    let l = world::layout("muse");
    let f = Fake::default();
    let lane = Lane {
        layout: &l,
        fetcher: &f,
        triple: elsewhere,
        policy: &Policy::default(),
        trust: &world::trust(),
        legacy_index: None,
        asked: true,
        now: NOW,
        download_turn: None,
    };
    let o = install_one(&lane, muse(), false);
    assert!(matches!(o.verdict, Verdict::Unavailable { .. }));
    assert!(!o.is_failure());
    assert!(f.gets.borrow().is_empty(), "no host was asked");
    assert!(ProgramStamp::read(&l, "muse").is_none(), "nothing recorded");
    if unserved_here(muse()) {
        return;
    }
    let exe = native_exe("muse 1.4.9-R6298");
    publish_muse(&f, "1.4.9-R6298", &exe);
    let o = run(&l, &f, &Policy::default(), muse());
    assert!(matches!(o.verdict, Verdict::Installed { .. }), "{o:?}");
    let build = world::build_dir(&l, "muse", "1.4.9-R6298");
    assert_eq!(std::fs::read(build.join("bin/muse")).unwrap(), exe);
    assert_eq!(complete_record(&build).unwrap().sha256, world::sha(&exe));
    assert!(crate::shim_env::read_sidecar(&build).is_empty());
}

#[test]
fn an_upgrade_moves_and_an_unchanged_head_costs_one_conditional_get() {
    let l = world::layout("upgrade");
    let f = Fake::default();
    f.publish_claude("2.1.281", &native_exe("a"));
    run(&l, &f, &Policy::default(), claude());
    f.publish_claude("2.1.282", &native_exe("b"));
    let o = run(&l, &f, &Policy::default(), claude());
    assert!(matches!(o.verdict, Verdict::Installed { .. }), "{o:?}");
    assert_eq!(shim_build(&l, "claude"), Some(v("2.1.282").build_id()));
    let gets_before = f.gets.borrow().len();
    let downloads_before = f.downloads.borrow().len();
    let o = run(&l, &f, &Policy::default(), claude());
    assert!(matches!(o.verdict, Verdict::Current(_)), "{o:?}");
    let gets = f.gets.borrow()[gets_before..].to_vec();
    assert_eq!(gets.len(), 1, "{gets:?}");
    assert_eq!(gets[0].0, format!("{CLAUDE}latest"));
    assert!(gets[0].1.is_some(), "the check is conditional");
    assert_eq!(f.downloads.borrow().len(), downloads_before);
}

#[test]
fn a_regressed_head_is_kept_at_and_an_unreachable_one_is_kept_quietly() {
    let l = world::layout("regress");
    let f = Fake::default();
    f.publish_claude("2.1.286", &native_exe("new"));
    run(&l, &f, &Policy::default(), claude());
    f.publish_claude("2.1.283", &native_exe("old"));
    let o = run(&l, &f, &Policy::default(), claude());
    assert!(
        matches!(
            o.verdict,
            Verdict::Kept {
                reason: KeepReason::HeadOlder,
                ..
            }
        ),
        "{o:?}"
    );
    assert_eq!(shim_build(&l, "claude"), Some(v("2.1.286").build_id()));
    f.offline.set(true);
    let o = run(&l, &f, &Policy::default(), claude());
    assert!(!o.is_failure());
}

#[test]
fn a_refused_document_stages_nothing_and_downloads_nothing() {
    type Publish = Box<dyn Fn(&Fake)>;
    let cases: Vec<(&str, &'static VendorSpec, Publish)> = vec![
        (
            "bad-signature",
            claude(),
            Box::new(|f: &Fake| {
                f.publish_claude("2.1.282", &native_exe("x"));
                let other =
                    crate::openpgp::testkit::sign_detached(b"another document", 1_790_000_000);
                f.doc(&format!("{CLAUDE}2.1.282/manifest.json.sig"), &other);
            }),
        ),
        (
            "manifest-off-host",
            claude(),
            Box::new(|f: &Fake| {
                f.publish_claude("2.1.282", &native_exe("x"));
                let url = format!("{CLAUDE}2.1.282/manifest.json");
                f.redirect(&url, "https://evil.example/manifest.json");
            }),
        ),
        (
            "sums-disagree",
            codex(),
            Box::new(|f: &Fake| {
                f.publish_codex_default("0.157.0");
                let url = format!("{}rust-v0.157.0/codex-package_SHA256SUMS", world::CODEX_DL);
                let sums = format!("{}  {}\n", "a".repeat(64), Fake::codex_asset());
                f.doc(&url, sums.as_bytes());
            }),
        ),
        (
            "sums-off-host",
            codex(),
            Box::new(|f: &Fake| {
                f.publish_codex_default("0.157.0");
                let url = format!("{}rust-v0.157.0/codex-package_SHA256SUMS", world::CODEX_DL);
                f.redirect(&url, "https://raw.githubusercontent.com/openai/codex/x");
            }),
        ),
        (
            "tag-mismatch",
            codex(),
            Box::new(|f: &Fake| {
                f.publish_codex_default("0.157.0");
                let body = String::from_utf8(f.body(CODEX_HEAD)).unwrap();
                f.doc(
                    CODEX_HEAD,
                    body.replace("rust-v0.157.0", "v0.157.0").as_bytes(),
                );
            }),
        ),
        (
            "non-canonical-head",
            claude(),
            Box::new(|f: &Fake| {
                f.publish_claude("2.1.282", &native_exe("x"));
                f.doc(&format!("{CLAUDE}latest"), b"2.1.0281\n");
            }),
        ),
        (
            "kimi-manifests-disagree",
            kimi(),
            Box::new(|f: &Fake| {
                publish_kimi(f, "2.3.2", &native_exe("x"));
                let url = "https://cdn.kimi.com/kimi-code/binaries/2.3.2/manifest.json";
                let body = String::from_utf8(f.body(url)).unwrap();
                let other = body.replace(&world::sha(&native_exe("x")), &"a".repeat(64));
                f.doc(url, other.as_bytes());
            }),
        ),
        (
            "kimi-github-off-host",
            kimi(),
            Box::new(|f: &Fake| {
                publish_kimi(f, "2.3.2", &native_exe("x"));
                f.redirect(
                    "https://github.com/MoonshotAI/kimi-code/releases/download/\
                     @moonshot-ai/kimi-code@2.3.2/manifest.json",
                    "https://raw.githubusercontent.com/MoonshotAI/kimi-code/x",
                );
            }),
        ),
        (
            "duplicate-key",
            codex(),
            Box::new(|f: &Fake| {
                f.publish_codex_default("0.157.0");
                let body = String::from_utf8(f.body(CODEX_HEAD)).unwrap();
                let twice = body.replacen('{', "{\"tag_name\":\"rust-v0.157.0\",", 1);
                f.doc(CODEX_HEAD, twice.as_bytes());
            }),
        ),
    ];
    for (label, spec, publish) in cases {
        let l = world::layout(&format!("refuse-{label}"));
        let f = Fake::default();
        publish(&f);
        for pass in 0..2 {
            let o = run(&l, &f, &Policy::default(), spec);
            assert!(
                matches!(o.verdict, Verdict::Refused { .. }),
                "{label} pass {pass}: {o:?}"
            );
            assert!(o.is_failure(), "{label}");
            assert!(
                f.downloads.borrow().is_empty(),
                "{label}: nothing downloaded"
            );
            assert_eq!(
                shim_build(&l, spec.program),
                None,
                "{label}: nothing shimmed"
            );
            assert!(
                crate::ops::list_installed(&l).is_empty(),
                "{label}: nothing staged"
            );
        }
    }
}

#[test]
fn a_digest_mismatch_is_refused_memoized_and_never_downloaded_again() {
    let l = world::layout("digest");
    let f = Fake::default();
    f.publish_claude("2.1.281", &native_exe("installed"));
    run(&l, &f, &Policy::default(), claude());
    f.publish_claude("2.1.282", &native_exe("signed bytes!"));
    f.payload(
        &Fake::claude_payload_url("2.1.282"),
        &native_exe("swapped bytes"),
    );
    let o = run(&l, &f, &Policy::default(), claude());
    assert!(matches!(o.verdict, Verdict::Refused { .. }), "{o:?}");
    assert_eq!(shim_build(&l, "claude"), Some(v("2.1.281").build_id()));
    assert!(!crate::store::build_is_complete(&world::build_dir(
        &l, "claude", "2.1.282"
    )));
    let downloads = f.downloads.borrow().len();
    let o = run(&l, &f, &Policy::default(), claude());
    assert!(matches!(o.verdict, Verdict::Refused { .. }), "{o:?}");
    assert_eq!(
        f.downloads.borrow().len(),
        downloads + 1,
        "the first retry is free"
    );
    let downloads = f.downloads.borrow().len();
    let o = run(&l, &f, &Policy::default(), claude());
    assert!(matches!(o.verdict, Verdict::Refused { .. }), "{o:?}");
    assert_eq!(
        f.downloads.borrow().len(),
        downloads,
        "two identical verdicts: no re-download"
    );
    assert_no_build_id(&o.line());
}

#[test]
fn a_digest_mismatch_heals_when_the_bytes_do() {
    let l = world::layout("digest-heal");
    let f = Fake::default();
    f.publish_claude("2.1.281", &native_exe("installed"));
    run(&l, &f, &Policy::default(), claude());
    let signed = native_exe("signed bytes!");
    f.publish_claude("2.1.282", &signed);
    let url = Fake::claude_payload_url("2.1.282");
    f.payload(&url, &native_exe("torn bytes!!!"));
    let o = run(&l, &f, &Policy::default(), claude());
    assert!(matches!(o.verdict, Verdict::Refused { .. }), "{o:?}");
    f.payload(&url, &signed);
    let o = run(&l, &f, &Policy::default(), claude());
    assert!(matches!(o.verdict, Verdict::Installed { .. }), "{o:?}");
    assert_eq!(shim_build(&l, "claude"), Some(v("2.1.282").build_id()));
}

#[test]
fn a_document_the_host_does_not_serve_is_no_verdict() {
    let l = world::layout("unserved");
    let f = Fake::default();
    f.publish_codex_default("0.157.0");
    run(&l, &f, &Policy::default(), codex());
    f.publish_codex_default("0.157.1");
    f.forget(&format!(
        "{}rust-v0.157.1/codex-package_SHA256SUMS",
        world::CODEX_DL
    ));
    let yanked = Policy::from_entries(["codex@0.157.0"]);
    let o = run(&l, &f, &yanked, codex());
    assert!(
        matches!(o.verdict, Verdict::Unreachable { why: Some(_), .. }),
        "{o:?}"
    );
    assert!(!o.is_failure());
    assert!(
        o.line().contains("did not serve it") && o.line().contains("404"),
        "{}",
        o.line()
    );
    assert_eq!(
        shim_build(&l, "codex"),
        Some(v("0.157.0").build_id()),
        "not tombstoned"
    );
    f.forget(CODEX_HEAD);
    let o = run(&l, &f, &yanked, codex());
    assert!(
        matches!(o.verdict, Verdict::Unreachable { why: Some(_), .. }),
        "{o:?}"
    );
    assert!(!o.is_failure());
    assert_eq!(
        shim_build(&l, "codex"),
        Some(v("0.157.0").build_id()),
        "not tombstoned"
    );
}

#[test]
fn an_unreadable_digest_log_fetches_nothing() {
    let l = world::layout("digest-log");
    let f = Fake::default();
    f.publish_claude("2.1.281", &native_exe("a"));
    let log = super::super::digests::digests_path(&l, "claude");
    std::fs::create_dir_all(log.parent().unwrap()).unwrap();
    std::fs::write(&log, "not toml {{{").unwrap();
    let o = run(&l, &f, &Policy::default(), claude());
    assert!(matches!(o.verdict, Verdict::Failed { .. }), "{o:?}");
    assert!(o.line().contains("digest log"), "{}", o.line());
    assert!(f.downloads.borrow().is_empty(), "nothing fetched");
    assert_eq!(shim_build(&l, "claude"), None);
    assert_eq!(std::fs::read_to_string(&log).unwrap(), "not toml {{{");
}

/// A signed-yank rollback lays the table's exposes only — never a shim for another
/// executable the retained build's `bin/` happens to hold.
#[cfg(unix)]
#[test]
fn a_rollback_shims_the_tables_exposes_only() {
    let l = world::layout("rollback-exposes");
    let f = Fake::default();
    let publish = |version: &str| {
        let codex = native_exe(&format!("codex {version}"));
        let host = native_exe("code-mode-host");
        f.publish_codex(
            version,
            &[
                ("bin/codex", &codex, 0o755),
                ("bin/codex-code-mode-host", &host, 0o755),
            ],
        );
    };
    publish("0.157.0");
    run(&l, &f, &Policy::default(), codex());
    publish("0.157.1");
    run(&l, &f, &Policy::default(), codex());
    let host = crate::store::ToolName::new("codex-code-mode-host").unwrap();
    assert!(
        !l.shim(&host).exists(),
        "PRECONDITION: a landing shims the exposes only"
    );
    let o = run(&l, &f, &Policy::from_entries(["codex@0.157.1"]), codex());
    assert!(matches!(o.verdict, Verdict::RolledBack { .. }), "{o:?}");
    assert_eq!(shim_build(&l, "codex"), Some(v("0.157.0").build_id()));
    assert!(
        !l.shim(&host).exists(),
        "a rollback lays no shim the table does not expose"
    );
}

#[test]
fn a_signer_refusal_is_memoized_like_a_digest_refusal() {
    let l = world::layout("signer");
    let f = Fake::default();
    f.publish_claude("2.1.281", &native_exe("x"));
    let trust = world::trust_refusing_signers();
    let o = run_with(&l, &f, &Policy::default(), &trust, claude(), false);
    if cfg!(target_os = "macos") {
        assert!(matches!(o.verdict, Verdict::Refused { .. }), "{o:?}");
        assert!(o.line().contains("signer refused"), "{}", o.line());
        let downloads = f.downloads.borrow().len();
        run_with(&l, &f, &Policy::default(), &trust, claude(), false);
        assert_eq!(f.downloads.borrow().len(), downloads);
        assert_eq!(shim_build(&l, "claude"), None);
    } else {
        assert!(
            matches!(o.verdict, Verdict::Installed { .. }),
            "no Apple anchor off macOS"
        );
    }
}

#[test]
fn a_signature_check_that_did_not_finish_defers_and_the_next_pass_lands_it() {
    let l = world::layout("deferred");
    let f = Fake::default();
    f.publish_claude("2.1.281", &native_exe("a"));
    run(&l, &f, &Policy::default(), claude());
    f.publish_claude("2.1.282", &native_exe("b"));
    let slow = world::trust_timing_out();
    let o = run_with(&l, &f, &Policy::default(), &slow, claude(), false);
    if !cfg!(target_os = "macos") {
        assert!(
            matches!(o.verdict, Verdict::Installed { .. }),
            "no Apple anchor off macOS"
        );
        return;
    }
    assert!(matches!(o.verdict, Verdict::Deferred { .. }), "{o:?}");
    assert!(!o.is_failure(), "a wait is never a failure");
    assert_eq!(
        crate::cli::left_for_next_pass(&format!("atpkg: codex current\n{}\n", o.line())),
        [o.line()],
        "the pass's line leaves claude for the next pass"
    );
    assert_eq!(o.active_version(), Some(v("2.1.281")));
    assert_eq!(shim_build(&l, "claude"), Some(v("2.1.281").build_id()));
    let downloads = f.downloads.borrow().len();
    let o = run(&l, &f, &Policy::default(), claude());
    assert!(matches!(o.verdict, Verdict::Installed { .. }), "{o:?}");
    assert_eq!(shim_build(&l, "claude"), Some(v("2.1.282").build_id()));
    assert_eq!(
        f.downloads.borrow().len(),
        downloads,
        "the verified archive was kept for the retry"
    );
}

#[test]
fn a_yanked_head_is_skipped_and_a_hold_keeps_the_build() {
    let l = world::layout("yank-head");
    let f = Fake::default();
    f.publish_claude("2.1.281", &native_exe("a"));
    run(&l, &f, &Policy::default(), claude());
    f.publish_claude("2.1.282", &native_exe("b"));
    let yanked = Policy::from_entries(["claude@2.1.282"]);
    let o = run(&l, &f, &yanked, claude());
    assert!(
        matches!(
            o.verdict,
            Verdict::Kept {
                reason: KeepReason::HeadYanked,
                ..
            }
        ),
        "{o:?}"
    );
    assert!(
        f.downloads.borrow().len() == 1,
        "nothing fetched for a yanked head"
    );
    let o = run_with(&l, &f, &Policy::default(), &world::trust(), claude(), true);
    assert!(
        matches!(
            o.verdict,
            Verdict::Kept {
                reason: KeepReason::Held,
                ..
            }
        ),
        "{o:?}"
    );
    assert_eq!(shim_build(&l, "claude"), Some(v("2.1.281").build_id()));
}

#[test]
fn a_yanked_installed_version_moves_to_the_head_or_rolls_back() {
    let l = world::layout("yank-installed");
    let f = Fake::default();
    f.publish_claude("2.1.281", &native_exe("a"));
    run(&l, &f, &Policy::default(), claude());
    f.publish_claude("2.1.282", &native_exe("b"));
    run(&l, &f, &Policy::default(), claude());
    let yanked = Policy::from_entries(["claude@2.1.282"]);
    let o = run(&l, &f, &yanked, claude());
    assert!(matches!(o.verdict, Verdict::RolledBack { .. }), "{o:?}");
    assert_eq!(shim_build(&l, "claude"), Some(v("2.1.281").build_id()));
    assert_eq!(
        ProgramStamp::read(&l, "claude").unwrap().high_water,
        Some(v("2.1.281"))
    );
    f.publish_claude("2.1.283", &native_exe("c"));
    let o = run(&l, &f, &yanked, claude());
    assert!(
        matches!(
            o.verdict,
            Verdict::Installed {
                reason: InstallReason::Newer,
                ..
            }
        ),
        "{o:?}"
    );
    f.publish_claude("2.1.284", &native_exe("d"));
    let o = run(&l, &f, &Policy::from_entries(["claude@2.1.283"]), claude());
    assert!(
        matches!(
            o.verdict,
            Verdict::Installed {
                reason: InstallReason::ReplacesYanked,
                ..
            }
        ),
        "{o:?}"
    );
    let o = run(
        &l,
        &f,
        &Policy::from_entries(["claude@2.1.283", "claude@2.1.284"]),
        claude(),
    );
    assert!(matches!(o.verdict, Verdict::RolledBack { .. }), "{o:?}");
    assert_eq!(shim_build(&l, "claude"), Some(v("2.1.282").build_id()));
}

#[test]
fn a_yanked_version_with_nothing_admissible_is_tombstoned() {
    let l = world::layout("tombstone");
    let f = Fake::default();
    f.publish_claude("2.1.281", &native_exe("a"));
    run(&l, &f, &Policy::default(), claude());
    let yanked = Policy::from_entries(["claude@2.1.281"]);
    f.offline.set(true);
    let o = run(&l, &f, &yanked, claude());
    assert!(
        matches!(o.verdict, Verdict::Unreachable { .. }),
        "an unread head proves nothing inadmissible: {o:?}"
    );
    assert_eq!(shim_build(&l, "claude"), Some(v("2.1.281").build_id()));
    f.offline.set(false);
    let o = run(&l, &f, &yanked, claude());
    assert!(matches!(o.verdict, Verdict::Tombstoned { .. }), "{o:?}");
    assert!(o.is_failure());
}

/// The genuine codex shape: an executable-mode data file is demoted before the gate,
/// the record's root is the root of the tree as it stands, and the entry keeps its bits.
#[cfg(unix)]
#[test]
fn a_codex_tree_is_demoted_gated_and_recorded_as_it_stands() {
    use std::os::unix::fs::PermissionsExt as _;
    let l = world::layout("codex");
    let f = Fake::default();
    f.publish_codex_default("0.157.0");
    let o = run(&l, &f, &Policy::default(), codex());
    assert!(matches!(o.verdict, Verdict::Installed { .. }), "{o:?}");
    let dir = world::build_dir(&l, "codex", "0.157.0");
    let mode = |rel: &str| {
        std::fs::metadata(dir.join(rel))
            .unwrap()
            .permissions()
            .mode()
            & 0o777
    };
    assert_eq!(mode("codex-resources/voice/runtime.json"), 0o644);
    assert_eq!(mode("bin/codex"), 0o755);
    assert_eq!(mode("codex-path/rg"), 0o755);
    let record = complete_record(&dir).unwrap();
    assert_eq!(record.tree_root, crate::tree::tree_root(&dir).unwrap());
    assert!(crate::shim_env::read_sidecar(&dir).entries().is_empty());
    assert_eq!(
        crate::verify::verify_program(&l, "codex"),
        crate::verify::VerifyOutcome::VendorRoot {
            build: v("0.157.0").build_id(),
            vendor: "OpenAI".into(),
            staged_from: crate::verify::StagedFrom::Documents,
            drift: None,
        }
    );
    assert_eq!(f.downloads.borrow().len(), 1);
}

/// An interpreter script in a vendor tree never runs: the darwin gate refuses the
/// tree; elsewhere it lands without its execute bits.
#[cfg(unix)]
#[test]
fn a_script_in_a_codex_tree_is_refused_or_demoted() {
    use std::os::unix::fs::PermissionsExt as _;
    let l = world::layout("codex-script");
    let f = Fake::default();
    let codex_bin = native_exe("codex");
    f.publish_codex(
        "0.157.0",
        &[
            ("bin/codex", &codex_bin, 0o755),
            ("codex-path/rg", b"#!/bin/sh\nexec evil\n", 0o755),
        ],
    );
    let o = run(&l, &f, &Policy::default(), codex());
    if cfg!(target_os = "macos") {
        assert!(matches!(o.verdict, Verdict::Refused { .. }), "{o:?}");
        assert!(o.line().contains("interpreter script"), "{}", o.line());
        assert_eq!(shim_build(&l, "codex"), None);
    } else {
        assert!(matches!(o.verdict, Verdict::Installed { .. }), "{o:?}");
        let rg = world::build_dir(&l, "codex", "0.157.0").join("codex-path/rg");
        assert_eq!(
            std::fs::metadata(rg).unwrap().permissions().mode() & 0o111,
            0
        );
    }
}

#[test]
fn codex_old_lands_openais_build_under_its_own_name() {
    let l = world::layout("codex-old");
    let f = Fake::default();
    f.publish_codex_default("0.163.0");
    let o = run(&l, &f, &Policy::default(), codex_old());
    assert!(
        matches!(
            o.verdict,
            Verdict::Installed {
                reason: InstallReason::Fresh,
                ..
            }
        ),
        "{o:?}"
    );
    let dir = world::build_dir(&l, "codex-old", "0.163.0");
    let (entry, alias) = (
        dir.join(world::bin("codex")),
        dir.join(world::bin("codex-old")),
    );
    let meta = |p: &Path| std::fs::symlink_metadata(p).unwrap();
    assert!(
        meta(&alias).file_type().is_file(),
        "a regular file, never a link"
    );
    assert_eq!(std::fs::read(&alias).unwrap(), native_exe("codex 0.163.0"));
    assert_eq!(std::fs::read(&entry).unwrap(), native_exe("codex 0.163.0"));
    assert_eq!(meta(&alias).permissions(), meta(&entry).permissions());
    let tool = |name: &str| crate::store::ToolName::new(name).unwrap();
    assert_eq!(
        crate::platform::resolve_shim(&l.shim(&tool("codex-old"))),
        Some(alias.clone())
    );
    assert_eq!(
        crate::activate::twin_target(&l, "codex-old"),
        Some(alias.clone())
    );
    assert!(std::fs::symlink_metadata(l.shim(&tool("codex"))).is_err());
    assert_eq!(shim_build(&l, "codex-old"), Some(v("0.163.0").build_id()));
    assert_eq!(shim_build(&l, "codex"), None);
    let record = complete_record(&dir).unwrap();
    assert_eq!(record.program, "codex-old");
    let modes = crate::store::declared_modes(&dir).unwrap();
    assert_eq!(
        record.tree_root,
        crate::tree::tree_root_declared(&dir, &modes).unwrap()
    );
    assert_eq!(
        crate::verify::verify_program(&l, "codex-old"),
        crate::verify::VerifyOutcome::VendorRoot {
            build: v("0.163.0").build_id(),
            vendor: "OpenAI".into(),
            staged_from: crate::verify::StagedFrom::Documents,
            drift: None,
        }
    );
    assert!(
        f.gets
            .borrow()
            .iter()
            .all(|(url, _)| crate::vendor::vendor_direct_url_allowed("codex", url)),
        "only OpenAI's documents, under codex's pins"
    );
    assert_eq!(
        f.downloads.borrow().as_slice(),
        [Fake::codex_payload_url("0.163.0")]
    );
}

/// The gate judges a clone as a Mach-O of its own: a codesign verdict refusing
/// `bin/codex-old` alone refuses the landing (nothing swapped in, nothing shimmed), though
/// OpenAI's own entry passes.
#[cfg(target_os = "macos")]
#[test]
fn the_gate_judges_codex_olds_clone_as_a_mach_o_of_its_own() {
    fn refuse_the_alias(
        path: &Path,
        _: &str,
        _: std::time::Instant,
    ) -> Result<(), aterm_update_core::codesign::CodesignError> {
        if path.ends_with("bin/codex-old") {
            Err(aterm_update_core::codesign::CodesignError::Refused {
                code: Some(3),
                stderr: "test-requirement: code failed to satisfy specified code \
                         requirement(s)\n"
                    .into(),
            })
        } else {
            Ok(())
        }
    }
    let f = Fake::default();
    f.publish_codex_default("0.163.0");
    let refusing = Trust {
        team_check: refuse_the_alias,
        ..world::trust()
    };
    let l = world::layout("codex-old-gate");
    let o = run_with(&l, &f, &Policy::default(), &refusing, codex_old(), false);
    assert!(matches!(o.verdict, Verdict::Refused { .. }), "{o:?}");
    assert!(o.line().contains("bin/codex-old"), "{}", o.line());
    assert_eq!(shim_build(&l, "codex-old"), None);
    assert!(!crate::store::build_is_complete(&world::build_dir(
        &l,
        "codex-old",
        "0.163.0"
    )));
    let control = world::layout("codex-old-gate-control");
    let o = run(&control, &f, &Policy::default(), codex_old());
    assert!(matches!(o.verdict, Verdict::Installed { .. }), "{o:?}");
}

#[test]
fn a_codex_package_without_its_entry_lands_no_codex_old() {
    let f = Fake::default();
    f.publish_codex("0.163.0", &[("codex-path/rg", &native_exe("rg"), 0o755)]);
    let l = world::layout("codex-old-no-entry");
    let o = run(&l, &f, &Policy::default(), codex_old());
    assert!(o.is_failure(), "{o:?}");
    let missing_entry = format!("{} is not a regular file", world::bin("codex"));
    assert!(o.line().contains(&missing_entry), "{}", o.line());
    assert_eq!(shim_build(&l, "codex-old"), None);
    assert!(!crate::store::build_is_complete(&world::build_dir(
        &l,
        "codex-old",
        "0.163.0"
    )));
}

/// The files [`Fake::publish_codex_default`]'s package lays. Unix only, as the tests that
/// read a lent tree are ([`sibling_tree`]).
#[cfg(unix)]
fn codex_default_files() -> [String; 4] {
    [
        world::bin("codex"),
        "codex-path/rg".into(),
        "codex-resources/voice/runtime.json".into(),
        "codex-resources/voice/NOTICE.md".into(),
    ]
}

/// Assert `pkg verify` passes `program`'s `version`, its tree copied from `lender`'s build or
/// (`None`) downloaded.
fn verified(l: &Layout, program: &str, version: &str, lender: Option<&str>) {
    assert_eq!(
        crate::verify::verify_program(l, program),
        crate::verify::VerifyOutcome::VendorRoot {
            build: v(version).build_id(),
            vendor: "OpenAI".into(),
            staged_from: lender.map_or(crate::verify::StagedFrom::Documents, |p| {
                crate::verify::StagedFrom::Sibling { program: p.into() }
            }),
            drift: None,
        },
        "{program}"
    );
}

/// A store with codex `0.163.0` landed from OpenAI's package.
fn codex_landed(label: &str) -> (crate::testdir::ScratchLayout, Fake) {
    let l = world::layout(label);
    let f = Fake::default();
    f.publish_codex_default("0.163.0");
    let o = run(&l, &f, &Policy::default(), codex());
    assert!(matches!(o.verdict, Verdict::Installed { .. }), "{o:?}");
    (l, f)
}

/// Where codex is OpenAI's build, codex-old copies codex's verified build of the same
/// release instead of fetching it again: one download for both rows, a tree of its own
/// (each file a clone on its own inode, nothing linked), its alias, shim and twin in its own
/// build, a record naming the lender, and otherwise exactly the tree, record and modes a
/// download of its own lays.
///
/// Unix only: a build lends its tree only there ([`sibling_tree`]).
#[cfg(unix)]
#[test]
fn codex_old_takes_codexs_verified_tree_and_downloads_nothing() {
    let (l, f) = codex_landed("codex-old-taken");
    let o = run(&l, &f, &Policy::default(), codex_old());
    assert!(
        matches!(
            o.verdict,
            Verdict::Installed {
                reason: InstallReason::Fresh,
                ..
            }
        ),
        "{o:?}"
    );
    assert_eq!(
        f.downloads.borrow().as_slice(),
        [Fake::codex_payload_url("0.163.0")],
        "one download lands both rows"
    );
    let (from, dir) = (
        world::build_dir(&l, "codex", "0.163.0"),
        world::build_dir(&l, "codex-old", "0.163.0"),
    );
    let record = complete_record(&dir).unwrap();
    assert_eq!(record.tree_from.as_deref(), Some("codex"));
    verified(&l, "codex-old", "0.163.0", Some("codex"));
    assert!(
        std::fs::symlink_metadata(&dir).unwrap().is_dir(),
        "a tree of its own, never a link to codex's"
    );
    for rel in codex_default_files() {
        let (lent, own) = (from.join(&rel), dir.join(&rel));
        assert!(std::fs::symlink_metadata(&own).unwrap().is_file(), "{rel}");
        assert!(
            crate::clone::is_clone_of(&lent, &own),
            "{rel}: a clone on an inode of its own"
        );
    }
    let alias = dir.join(world::bin("codex-old"));
    assert_eq!(std::fs::read(&alias).unwrap(), native_exe("codex 0.163.0"));
    assert!(
        std::fs::symlink_metadata(from.join(world::bin("codex-old"))).is_err(),
        "codex's tree gained nothing"
    );
    let tool = |name: &str| crate::store::ToolName::new(name).unwrap();
    assert_eq!(
        crate::platform::resolve_shim(&l.shim(&tool("codex-old"))),
        Some(alias.clone())
    );
    assert_eq!(crate::activate::twin_target(&l, "codex-old"), Some(alias));
    verified(&l, "codex", "0.163.0", None);

    let control = world::layout("codex-old-taken-control");
    let o = run(&control, &f, &Policy::default(), codex_old());
    assert!(matches!(o.verdict, Verdict::Installed { .. }), "{o:?}");
    let alone = world::build_dir(&control, "codex-old", "0.163.0");
    assert_eq!(
        complete_record(&alone),
        Some(VendorRecord {
            tree_from: None,
            ..record
        }),
        "a download of its own records the same tree, only no lender"
    );
    assert_eq!(
        crate::store::declared_modes(&dir).unwrap(),
        crate::store::declared_modes(&alone).unwrap()
    );
}

/// A codex build whose tree no longer folds to its record's root lends nothing: codex-old
/// downloads its own, lays OpenAI's bytes, and nothing is memoized against it.
#[test]
fn a_drifted_codex_tree_is_downloaded_not_shared() {
    let (l, f) = codex_landed("codex-old-drift");
    let from = world::build_dir(&l, "codex", "0.163.0");
    std::fs::write(from.join("codex-path/rg"), native_exe("not rg")).unwrap();
    let o = run(&l, &f, &Policy::default(), codex_old());
    assert!(matches!(o.verdict, Verdict::Installed { .. }), "{o:?}");
    assert_eq!(
        f.downloads.borrow().as_slice(),
        [
            Fake::codex_payload_url("0.163.0"),
            Fake::codex_payload_url("0.163.0")
        ]
    );
    let dir = world::build_dir(&l, "codex-old", "0.163.0");
    assert_eq!(
        std::fs::read(dir.join("codex-path/rg")).unwrap(),
        native_exe("rg")
    );
    assert_eq!(complete_record(&dir).unwrap().tree_from, None);
    verified(&l, "codex-old", "0.163.0", None);
    assert!(
        crate::store::stage_refusal(&dir).is_none(),
        "a drifted lender is no verdict on the bytes"
    );
    let o = run(&l, &f, &Policy::default(), codex_old());
    assert!(matches!(o.verdict, Verdict::Current(_)), "{o:?}");
}

/// Only a codex build of exactly codex-old's candidate is copied: a record naming another
/// digest or URL, or no codex build of the version codex-old lands, means a download.
///
/// Unix only: a build lends its tree only there ([`sibling_tree`]).
#[cfg(unix)]
#[test]
fn only_the_same_bytes_are_shared() {
    let landed = |label: &str, edit: &dyn Fn(&mut VendorRecord)| {
        let (l, f) = codex_landed(label);
        let from = world::build_dir(&l, "codex", "0.163.0");
        let mut record = complete_record(&from).unwrap();
        edit(&mut record);
        super::super::durable::write_durable(
            &super::super::record_path(&from).unwrap(),
            &record.record_bytes().unwrap(),
        )
        .unwrap();
        assert_eq!(
            complete_record(&from),
            Some(record),
            "PRECONDITION: a record codex's own lane reads"
        );
        let o = run(&l, &f, &Policy::default(), codex_old());
        assert!(matches!(o.verdict, Verdict::Installed { .. }), "{o:?}");
        let dir = world::build_dir(&l, "codex-old", "0.163.0");
        let downloads = f.downloads.borrow().len();
        (downloads, complete_record(&dir).unwrap().tree_from)
    };
    assert_eq!(
        landed("same-bytes", &|_| {}),
        (1, Some("codex".into())),
        "the control"
    );
    assert_eq!(
        landed("other-digest", &|r| r.sha256 = "f".repeat(64)),
        (2, None)
    );
    let other_url = format!(
        "{}rust-v0.163.0/codex-package-other.tar.gz",
        world::CODEX_DL
    );
    assert_eq!(
        landed("other-url", &|r| r.source_url.clone_from(&other_url)),
        (2, None)
    );

    let (l, f) = codex_landed("other-version");
    f.publish_codex_default("0.164.0");
    let o = run(&l, &f, &Policy::default(), codex_old());
    assert!(matches!(o.verdict, Verdict::Installed { .. }), "{o:?}");
    assert_eq!(
        f.downloads.borrow().as_slice(),
        [
            Fake::codex_payload_url("0.163.0"),
            Fake::codex_payload_url("0.164.0")
        ]
    );
    let dir = world::build_dir(&l, "codex-old", "0.164.0");
    assert_eq!(complete_record(&dir).unwrap().tree_from, None);
}

/// A copied codex-old owns its tree outright: gc reclaiming the codex build it was copied
/// from, then codex's uninstall, leave its bytes, shim and verify as they were.
///
/// Unix only: a build lends its tree only there ([`sibling_tree`]).
#[cfg(unix)]
#[test]
fn codex_old_outlives_codex() {
    let (l, f) = codex_landed("codex-old-outlives");
    let o = run(&l, &f, &Policy::default(), codex_old());
    assert!(matches!(o.verdict, Verdict::Installed { .. }), "{o:?}");
    let dir = world::build_dir(&l, "codex-old", "0.163.0");
    assert_eq!(
        complete_record(&dir).unwrap().tree_from.as_deref(),
        Some("codex"),
        "PRECONDITION: copied"
    );
    for version in ["0.164.0", "0.165.0"] {
        f.publish_codex_default(version);
        let o = run(&l, &f, &Policy::default(), codex());
        assert!(matches!(o.verdict, Verdict::Installed { .. }), "{o:?}");
    }
    let report = crate::gc::run(&l, None);
    assert!(
        std::fs::symlink_metadata(world::build_dir(&l, "codex", "0.163.0")).is_err(),
        "PRECONDITION: gc reclaimed the build codex-old was copied from: {:?}",
        report.reclaimed
    );
    let alias = dir.join(world::bin("codex-old"));
    let tool = crate::store::ToolName::new("codex-old").unwrap();
    let intact = |when: &str| {
        verified(&l, "codex-old", "0.163.0", Some("codex"));
        assert_eq!(
            std::fs::read(&alias).ok(),
            Some(native_exe("codex 0.163.0")),
            "{when}"
        );
        for rel in codex_default_files() {
            assert!(
                std::fs::symlink_metadata(dir.join(&rel)).is_ok_and(|m| m.is_file()),
                "{when}: {rel}"
            );
        }
        assert_eq!(
            crate::platform::resolve_shim(&l.shim(&tool)),
            Some(alias.clone()),
            "{when}"
        );
    };
    intact("after gc");
    crate::ops::uninstall(&l, "codex").unwrap();
    assert!(
        std::fs::symlink_metadata(l.prefix.join("store/codex")).is_err(),
        "PRECONDITION: codex is gone"
    );
    intact("after codex's uninstall");
}

/// A codex-old the lane would not download is not copied either: a refusal of these exact
/// bytes, or a first-seen digest the candidate's differs from, refuses it before any codex
/// build is looked at.
#[test]
fn a_refused_or_conflicting_codex_old_is_never_shared() {
    let (l, f) = codex_landed("codex-old-memo");
    let sha256 = complete_record(&world::build_dir(&l, "codex", "0.163.0"))
        .unwrap()
        .sha256;
    let digest =
        AuthenticatedDigest::authenticated(crate::manifest::DigestAlg::Sha256, &sha256).unwrap();
    ProgramStamp::record_refusal(
        &l,
        "codex-old",
        "atpkg: codex-old 0.163.0 was refused",
        v("0.163.0"),
        &digest,
    )
    .unwrap();
    let o = run(&l, &f, &Policy::default(), codex_old());
    assert!(matches!(o.verdict, Verdict::Refused { .. }), "{o:?}");
    assert!(
        o.line()
            .contains("an earlier pass refused these exact bytes"),
        "{}",
        o.line()
    );
    assert!(std::fs::symlink_metadata(world::build_dir(&l, "codex-old", "0.163.0")).is_err());
    assert_eq!(shim_build(&l, "codex-old"), None);
    assert_eq!(f.downloads.borrow().len(), 1);

    let (l, f) = codex_landed("codex-old-recut");
    let first =
        AuthenticatedDigest::authenticated(crate::manifest::DigestAlg::Sha256, &"e".repeat(64))
            .unwrap();
    record_or_conflict(&l, "codex-old", v("0.163.0"), world::triple(), &first, true).unwrap();
    let o = run(&l, &f, &Policy::default(), codex_old());
    assert!(matches!(o.verdict, Verdict::Refused { .. }), "{o:?}");
    assert!(o.line().contains("first seen for 0.163.0"), "{}", o.line());
    assert!(std::fs::symlink_metadata(world::build_dir(&l, "codex-old", "0.163.0")).is_err());
    assert_eq!(shim_build(&l, "codex-old"), None);
    assert_eq!(f.downloads.borrow().len(), 1);
}

/// codex-old follows codex release by release, copying each: two releases, two downloads.
///
/// Unix only: a build lends its tree only there ([`sibling_tree`]).
#[cfg(unix)]
#[test]
fn codex_old_follows_codex_forward() {
    let l = world::layout("codex-old-forward");
    let f = Fake::default();
    for version in ["0.163.0", "0.164.0"] {
        f.publish_codex_default(version);
        for spec in [codex(), codex_old()] {
            let o = run(&l, &f, &Policy::default(), spec);
            assert!(
                matches!(o.verdict, Verdict::Installed { .. }),
                "{version} {}: {o:?}",
                spec.program
            );
        }
        let dir = world::build_dir(&l, "codex-old", version);
        assert_eq!(
            complete_record(&dir).unwrap().tree_from.as_deref(),
            Some("codex"),
            "{version}"
        );
    }
    assert_eq!(
        f.downloads.borrow().as_slice(),
        [
            Fake::codex_payload_url("0.163.0"),
            Fake::codex_payload_url("0.164.0")
        ]
    );
    assert_eq!(shim_build(&l, "codex-old"), Some(v("0.164.0").build_id()));
    verified(&l, "codex-old", "0.164.0", Some("codex"));
}

/// The gate judges a copied tree where codex-old's own stage lays it, as it judges a
/// download: a team check refusing what sits under codex-old's store refuses the copy and
/// swaps nothing in, and codex's build, shim and stamp are as they were.
#[cfg(target_os = "macos")]
#[test]
fn the_gate_judges_a_taken_tree_at_codex_olds_paths() {
    fn refuse_under_codex_old(
        path: &Path,
        _: &str,
        _: std::time::Instant,
    ) -> Result<(), aterm_update_core::codesign::CodesignError> {
        if path.components().any(|c| c.as_os_str() == "codex-old") {
            Err(aterm_update_core::codesign::CodesignError::Refused {
                code: Some(3),
                stderr: "test-requirement: code failed to satisfy specified code \
                         requirement(s)\n"
                    .into(),
            })
        } else {
            Ok(())
        }
    }
    let refusing = Trust {
        team_check: refuse_under_codex_old,
        ..world::trust()
    };
    let l = world::layout("taken-gate");
    let f = Fake::default();
    f.publish_codex_default("0.163.0");
    let o = run_with(&l, &f, &Policy::default(), &refusing, codex(), false);
    assert!(matches!(o.verdict, Verdict::Installed { .. }), "{o:?}");
    let o = run_with(&l, &f, &Policy::default(), &refusing, codex_old(), false);
    assert!(matches!(o.verdict, Verdict::Refused { .. }), "{o:?}");
    assert!(o.line().contains("signer refused"), "{}", o.line());
    assert_eq!(
        f.downloads.borrow().len(),
        1,
        "the copy was judged, no download"
    );
    assert!(
        std::fs::symlink_metadata(world::build_dir(&l, "codex-old", "0.163.0")).is_err(),
        "nothing swapped in"
    );
    assert_eq!(shim_build(&l, "codex-old"), None);
    assert_eq!(shim_build(&l, "codex"), Some(v("0.163.0").build_id()));
    verified(&l, "codex", "0.163.0", None);
    let from = world::build_dir(&l, "codex", "0.163.0");
    assert!(crate::store::stage_refusal(&from).is_none());
    assert_eq!(ProgramStamp::read(&l, "codex").unwrap().refused, None);
}

/// The scratch a stage left beside `program`'s store builds: a copy or a swap not cleaned up.
fn scratch_left(l: &Layout, program: &str) -> Vec<String> {
    let build = world::build_dir(l, program, "0.163.0");
    let Ok(entries) = std::fs::read_dir(build.parent().unwrap()) else {
        return Vec::new();
    };
    entries
        .flatten()
        .filter_map(|e| e.file_name().to_str().map(str::to_string))
        .filter(|n| n.contains(".incoming-") || n.contains(".superseded-"))
        .collect()
}

/// Set the extended attribute `name` on `path` to `value`.
#[cfg(target_os = "macos")]
fn set_xattr(path: &Path, name: &str, value: &[u8]) {
    use std::os::unix::ffi::OsStrExt as _;
    let path = std::ffi::CString::new(path.as_os_str().as_bytes()).unwrap();
    let name = std::ffi::CString::new(name).unwrap();
    // SAFETY: `path` and `name` are NUL-terminated C strings and `value` a readable slice of
    // the stated length, all outliving the call, which only reads them.
    let rc = unsafe {
        libc::setxattr(
            path.as_ptr(),
            name.as_ptr(),
            value.as_ptr().cast::<libc::c_void>(),
            value.len(),
            0,
            0,
        )
    };
    assert_eq!(rc, 0, "setxattr: {}", std::io::Error::last_os_error());
}

/// Finder information on a file of codex's build survives a clone and fails the Developer ID
/// gate's strict check, though the tree's root does not fold it: that build lends codex-old
/// nothing, and codex-old downloads its own, which carries none, leaving no scratch.
#[cfg(target_os = "macos")]
#[test]
fn a_finder_tagged_codex_tree_is_downloaded_not_copied() {
    let (l, f) = codex_landed("finder-tagged");
    let from = world::build_dir(&l, "codex", "0.163.0").join(world::bin("codex"));
    // Finder keeps only information that is not all zero: here, the file's hidden bit.
    let mut info = [0u8; 32];
    info[8] = 0x40;
    set_xattr(&from, "com.apple.FinderInfo", &info);
    let finder_info = |path: &Path| {
        crate::platform::xattr_names(path)
            .unwrap()
            .iter()
            .any(|name| name == b"com.apple.FinderInfo")
    };
    assert!(finder_info(&from), "the control");
    let o = run(&l, &f, &Policy::default(), codex_old());
    assert!(matches!(o.verdict, Verdict::Installed { .. }), "{o:?}");
    assert_eq!(
        f.downloads.borrow().len(),
        2,
        "codex-old downloaded its own"
    );
    let dir = world::build_dir(&l, "codex-old", "0.163.0");
    assert_eq!(complete_record(&dir).unwrap().tree_from, None);
    verified(&l, "codex-old", "0.163.0", None);
    assert_eq!(scratch_left(&l, "codex-old"), Vec::<String>::new());
    assert!(!finder_info(&dir.join(world::bin("codex"))));
}

/// Clears every owner-binding flag under a tree when it drops, so a scratch layout a test
/// locked a file in can be removed, whether the test passed or not.
#[cfg(target_os = "macos")]
struct Unlock(std::path::PathBuf);

#[cfg(target_os = "macos")]
impl Drop for Unlock {
    fn drop(&mut self) {
        fn walk(dir: &Path) {
            use std::os::macos::fs::MetadataExt as _;
            let Ok(entries) = std::fs::read_dir(dir) else {
                return;
            };
            for path in entries.flatten().map(|e| e.path()) {
                let Ok(meta) = std::fs::symlink_metadata(&path) else {
                    continue;
                };
                if meta.file_type().is_symlink() {
                    continue;
                }
                let bound = meta.st_flags() & crate::platform::OWNER_BINDING_FLAGS;
                if bound != 0 {
                    let _ = crate::platform::set_file_flags(&path, meta.st_flags() & !bound);
                }
                if meta.is_dir() {
                    walk(&path);
                }
            }
        }
        walk(&self.0);
    }
}

/// A clone keeps a file's BSD flags, so a locked file in codex's build would lay a copy no
/// cleanup can remove, and every later stage of codex-old would stop on that scratch: that
/// build lends nothing, nothing locked is laid, and codex-old downloads its own, leaving no
/// scratch.
#[cfg(target_os = "macos")]
#[test]
fn a_locked_codex_file_is_downloaded_not_copied() {
    use std::os::macos::fs::MetadataExt as _;
    let (l, f) = codex_landed("locked");
    let _unlock = Unlock(l.prefix.clone());
    let notice = "codex-resources/voice/NOTICE.md";
    let from = world::build_dir(&l, "codex", "0.163.0").join(notice);
    crate::platform::set_file_flags(&from, libc::UF_IMMUTABLE).unwrap();
    let o = run(&l, &f, &Policy::default(), codex_old());
    assert!(matches!(o.verdict, Verdict::Installed { .. }), "{o:?}");
    assert_eq!(
        f.downloads.borrow().len(),
        2,
        "codex-old downloaded its own"
    );
    let dir = world::build_dir(&l, "codex-old", "0.163.0");
    assert_eq!(complete_record(&dir).unwrap().tree_from, None);
    assert_eq!(scratch_left(&l, "codex-old"), Vec::<String>::new());
    let laid = std::fs::symlink_metadata(dir.join(notice)).unwrap();
    assert_eq!(
        laid.st_flags() & crate::platform::OWNER_BINDING_FLAGS,
        0,
        "nothing locked is laid"
    );
}

/// A copy holds the room its tree takes, as a download's second disk gate does: on a volume
/// that cannot clone it is a full copy. Short of room, codex-old's copy refuses as its
/// download does (the same verdict and sentence; only the room named differs, for a copy
/// needs none for the archive) and lays, fetches and memoizes nothing.
#[test]
fn a_copy_short_of_room_refuses_as_a_download_does() {
    let (l, f) = codex_landed("copy-short");
    let _full = crate::freespace::pinned::pin(&l.prefix, 0);
    let copy = run(&l, &f, &Policy::default(), codex_old());
    assert_eq!(f.downloads.borrow().len(), 1, "nothing fetched");
    let dir = world::build_dir(&l, "codex-old", "0.163.0");
    assert!(std::fs::symlink_metadata(&dir).is_err(), "nothing laid");
    assert_eq!(scratch_left(&l, "codex-old"), Vec::<String>::new());
    assert!(
        crate::store::stage_refusal(&dir).is_none(),
        "nothing memoized"
    );
    assert_eq!(shim_build(&l, "codex-old"), None);

    let alone = world::layout("download-short");
    let g = Fake::default();
    g.publish_codex_default("0.163.0");
    let _alone_full = crate::freespace::pinned::pin(&alone.prefix, 0);
    let download = run(&alone, &g, &Policy::default(), codex_old());
    assert!(g.downloads.borrow().is_empty(), "nothing fetched");
    let refusal = |o: &Outcome| {
        let Verdict::Failed { error, .. } = &o.verdict else {
            panic!("not a disk refusal: {o:?}");
        };
        assert!(error.starts_with(crate::flow::DISK_REFUSAL_HEAD), "{error}");
        let line = o.line();
        line.split(": needs ").next().unwrap().to_string()
    };
    assert_eq!(refusal(&copy), refusal(&download));
}

/// When the copy's stage fails on this machine rather than on its bytes (the Developer ID
/// gate could not judge it), codex-old downloads its own instead of failing the pass, and
/// that download does not wait on the turn on the link the copy gave up once it was laid.
#[cfg(target_os = "macos")]
#[test]
fn a_copy_the_gate_cannot_judge_is_downloaded_instead() {
    use std::sync::atomic::{AtomicBool, Ordering};
    static JUDGED: AtomicBool = AtomicBool::new(false);
    fn cannot_judge_the_first(
        path: &Path,
        _: &str,
        _: std::time::Instant,
    ) -> Result<(), aterm_update_core::codesign::CodesignError> {
        if path.components().any(|c| c.as_os_str() == "codex-old")
            && !JUDGED.swap(true, Ordering::SeqCst)
        {
            return Err(aterm_update_core::codesign::CodesignError::Refused {
                code: Some(1),
                stderr: format!(
                    "{}: resource fork, Finder information, or similar detritus not \
                     allowed\n",
                    path.display()
                ),
            });
        }
        Ok(())
    }
    let (l, f) = codex_landed("copy-unjudged");
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let trust = Trust {
            team_check: cannot_judge_the_first,
            ..world::trust()
        };
        let (policy, turns) = (Policy::default(), crate::flow::DownloadTurns::default());
        let ticket = turns.ticket();
        let lane = Lane {
            layout: &l,
            fetcher: &f,
            triple: world::triple(),
            policy: &policy,
            trust: &trust,
            legacy_index: None,
            asked: false,
            now: NOW,
            download_turn: Some(&ticket),
        };
        let o = install_one(&lane, codex_old(), false);
        let dir = world::build_dir(&l, "codex-old", "0.163.0");
        let _ = tx.send((
            format!("{o:?}"),
            matches!(o.verdict, Verdict::Installed { .. }),
            f.downloads.borrow().len(),
            complete_record(&dir).map(|r| r.tree_from),
            scratch_left(&l, "codex-old"),
        ));
    });
    let (outcome, installed, downloads, tree_from, scratch) = rx
        .recv_timeout(std::time::Duration::from_secs(60))
        .expect("the landing hung: its download waited on the turn its copy gave up");
    assert!(JUDGED.load(Ordering::SeqCst), "the copy reached the gate");
    assert!(installed, "{outcome}");
    assert_eq!(downloads, 2, "codex-old downloaded its own");
    assert_eq!(tree_from, Some(None));
    assert_eq!(scratch, Vec::<String>::new());
}

#[test]
fn a_stale_part_of_another_version_is_not_resumed() {
    let l = world::layout("part");
    let f = Fake::default();
    let payload = native_exe("claude 2.1.282");
    f.publish_claude("2.1.282", &payload);
    let staging = l.staging_dir("claude");
    std::fs::create_dir_all(&staging).unwrap();
    let stale = staging.join(format!("claude-2.1.281-{}.part", world::triple()));
    std::fs::write(&stale, b"half of another version").unwrap();
    let o = run(&l, &f, &Policy::default(), claude());
    assert!(matches!(o.verdict, Verdict::Installed { .. }), "{o:?}");
    assert_eq!(
        o.asset.as_deref(),
        Some(format!("claude-2.1.282-{}", world::triple()).as_str())
    );
    assert!(!stale.exists(), "the other version's partial was reclaimed");
    let dir = world::build_dir(&l, "claude", "2.1.282");
    assert_eq!(
        std::fs::read(dir.join(world::bin("claude"))).unwrap(),
        payload
    );
}

#[test]
fn a_legacy_index_build_is_replaced_with_no_index_and_no_line_names_a_vendor_id() {
    let l = world::layout("legacy");
    legacy_claude(&l, 2_026_091_901);
    let f = Fake::default();
    f.publish_claude("2.1.280", &native_exe("below the floor"));
    let o = run(&l, &f, &Policy::default(), claude());
    assert!(
        matches!(
            o.verdict,
            Verdict::Kept {
                reason: KeepReason::BelowFloor,
                ..
            }
        ),
        "{o:?}"
    );
    assert_eq!(shim_build(&l, "claude"), Some(2_026_091_901));
    f.publish_claude("2.1.281", &native_exe("vendor"));
    let o = run(&l, &f, &Policy::default(), claude());
    assert!(
        matches!(
            o.verdict,
            Verdict::Installed {
                reason: InstallReason::ReplacesLegacy,
                ..
            }
        ),
        "{o:?}"
    );
    assert_eq!(shim_build(&l, "claude"), Some(v("2.1.281").build_id()));
    for line in [
        o.line(),
        crate::vendor_direct::display_build("claude", v("2.1.281").build_id()),
    ] {
        assert_no_build_id(&line);
    }
}

#[test]
fn a_read_legacy_version_is_kept_and_no_later_pass_needs_the_index() {
    let l = world::layout("legacy-kept");
    legacy_claude(&l, 2_026_092_201);
    let f = Fake::default();
    *f.index.borrow_mut() = Some(world::signed_index(44, "2099-01-01T00:00:00Z", &[]));
    f.publish_legacy_pkg("claude", 2_026_092_201, "2.1.281");
    let index = crate::resolve_verified_index(
        &f,
        &l,
        &crate::sig::testkit::anchor(),
        crate::sig::BuildFloor {
            index_build: 0,
            roster_seq: crate::sig::testkit::SEQ,
        },
        NOW,
    )
    .expect("the index verifies");
    f.publish_claude("2.1.281", &native_exe("vendor"));
    let o = run_indexed(
        &l,
        &f,
        &Policy::default(),
        &world::trust(),
        claude(),
        false,
        Some(&index),
    );
    assert!(matches!(o.verdict, Verdict::Current(_)), "{o:?}");
    assert_eq!(
        ProgramStamp::read(&l, "claude")
            .unwrap()
            .legacy_version_of(2_026_092_201),
        Some(v("2.1.281"))
    );
    *f.index.borrow_mut() = None;
    f.forget_legacy_pkgs();
    let o = run(&l, &f, &Policy::default(), claude());
    assert!(matches!(o.verdict, Verdict::Current(_)), "{o:?}");
    assert!(f.downloads.borrow().is_empty(), "nothing fetched");
    f.publish_claude("2.1.282", &native_exe("next"));
    let o = run(&l, &f, &Policy::default(), claude());
    assert!(
        matches!(
            o.verdict,
            Verdict::Installed {
                reason: InstallReason::Newer,
                ..
            }
        ),
        "{o:?}"
    );
}

#[test]
fn a_legacy_root_nothing_holds_is_read_from_its_signed_manifest_once() {
    let l = world::layout("legacy-root-owed");
    let legacy = 2_026_092_201;
    legacy_claude(&l, legacy);
    let root = crate::tree::tree_root(&l.build_dir("claude", legacy)).unwrap();
    let f = Fake::default();
    f.publish_claude("2.1.282", &native_exe("vendor"));
    ProgramStamp::record_legacy_version(&l, "claude", legacy, v("2.1.281")).unwrap();
    let o = run(&l, &f, &Policy::default(), claude());
    assert!(matches!(o.verdict, Verdict::Installed { .. }), "{o:?}");
    let vendor = Some(v("2.1.282").build_id());
    assert_eq!(legacy_root_owed(&l, "claude", vendor), Some(legacy));
    assert_eq!(f.pkg_reads.borrow().len(), 0, "no index, no read");
    *f.index.borrow_mut() = Some(world::signed_index(44, "2099-01-01T00:00:00Z", &[]));
    f.publish_legacy_pkg_rooted("claude", legacy, "2.1.281", world::triple(), &root);
    let index = crate::resolve_verified_index(
        &f,
        &l,
        &crate::sig::testkit::anchor(),
        crate::sig::BuildFloor {
            index_build: 0,
            roster_seq: crate::sig::testkit::SEQ,
        },
        NOW,
    )
    .expect("the index verifies");
    let indexed = || {
        run_indexed(
            &l,
            &f,
            &Policy::default(),
            &world::trust(),
            claude(),
            false,
            Some(&index),
        )
    };
    assert!(matches!(indexed().verdict, Verdict::Current(_)));
    let kept = ProgramStamp::read(&l, "claude").unwrap();
    assert_eq!(kept.legacy_root_of(legacy), Some(root.as_str()));
    assert_eq!(f.pkg_reads.borrow().len(), 1);
    assert_eq!(legacy_root_owed(&l, "claude", vendor), None);
    indexed();
    assert_eq!(f.pkg_reads.borrow().len(), 1, "kept: no pass asks again");
    let path = crate::vendor_direct::stamp_path(&l, "claude").unwrap();
    let older: String = std::fs::read_to_string(&path)
        .unwrap()
        .lines()
        .filter(|line| {
            !line.starts_with("legacy_root_build") && !line.starts_with("legacy_tree_root")
        })
        .map(|line| format!("{line}\n"))
        .collect();
    std::fs::write(&path, older).unwrap();
    assert_eq!(legacy_root_owed(&l, "claude", vendor), Some(legacy));
    indexed();
    assert_eq!(f.pkg_reads.borrow().len(), 2);
    assert_eq!(
        ProgramStamp::read(&l, "claude")
            .unwrap()
            .legacy_root_of(legacy),
        Some(root.as_str())
    );
}

#[test]
fn an_unread_legacy_build_above_the_ceiling_waits_for_its_version_or_a_person() {
    let above = claude().legacy_ceiling + 1;
    let l = world::layout("legacy-above");
    legacy_claude(&l, above);
    let f = Fake::default();
    f.publish_claude("2.1.282", &native_exe("vendor"));
    let o = run(&l, &f, &Policy::default(), claude());
    assert!(
        matches!(
            o.verdict,
            Verdict::Kept {
                reason: KeepReason::LegacyUnread,
                ..
            }
        ),
        "{o:?}"
    );
    assert!(!o.is_failure());
    assert_eq!(shim_build(&l, "claude"), Some(above));
    assert!(f.downloads.borrow().is_empty(), "nothing fetched");
    *f.index.borrow_mut() = Some(world::signed_index(45, "2099-01-01T00:00:00Z", &[]));
    f.publish_legacy_pkg("claude", above, "2.1.283");
    let index = crate::resolve_verified_index(
        &f,
        &l,
        &crate::sig::testkit::anchor(),
        crate::sig::BuildFloor {
            index_build: 0,
            roster_seq: crate::sig::testkit::SEQ,
        },
        NOW,
    )
    .expect("the index verifies");
    let o = run_indexed(
        &l,
        &f,
        &Policy::default(),
        &world::trust(),
        claude(),
        false,
        Some(&index),
    );
    assert!(
        matches!(
            o.verdict,
            Verdict::Kept {
                reason: KeepReason::HeadOlder,
                ..
            }
        ),
        "{o:?}"
    );
    assert_eq!(shim_build(&l, "claude"), Some(above));
    let l = world::layout("legacy-above-asked");
    legacy_claude(&l, above);
    let o = run_asked(&l, &f, claude());
    assert!(
        matches!(
            o.verdict,
            Verdict::Installed {
                reason: InstallReason::ReplacesLegacy,
                ..
            }
        ),
        "{o:?}"
    );
    assert_eq!(shim_build(&l, "claude"), Some(v("2.1.282").build_id()));
}

#[test]
fn a_legacy_build_is_read_under_an_index_that_no_longer_names_its_program() {
    let above = claude().legacy_ceiling + 1;
    let l = world::layout("legacy-unnamed");
    legacy_claude(&l, above);
    let f = Fake::default();
    *f.index.borrow_mut() = Some(world::signed_index_naming(
        45,
        "2099-01-01T00:00:00Z",
        &[],
        &[],
    ));
    f.publish_legacy_pkg("claude", above, "2.1.281");
    f.publish_claude("2.1.282", &native_exe("vendor"));
    let index = crate::resolve_verified_index(
        &f,
        &l,
        &crate::sig::testkit::anchor(),
        crate::sig::BuildFloor {
            index_build: 0,
            roster_seq: crate::sig::testkit::SEQ,
        },
        NOW,
    )
    .expect("the index verifies");
    assert!(index.program("claude").is_none());
    let o = run_indexed(
        &l,
        &f,
        &Policy::default(),
        &world::trust(),
        claude(),
        false,
        Some(&index),
    );
    assert!(
        matches!(
            o.verdict,
            Verdict::Installed {
                reason: InstallReason::Newer,
                ..
            }
        ),
        "{o:?}"
    );
    assert_eq!(
        o.line(),
        "atpkg: claude 2.1.281 → 2.1.282 (Anthropic latest)"
    );
    assert_eq!(*f.pkg_reads.borrow(), [crate::manifest::INDEX_REPO]);
    assert_eq!(shim_build(&l, "claude"), Some(v("2.1.282").build_id()));
}

#[test]
fn the_head_is_read_short_and_the_documents_patiently() {
    let l = world::layout("head-bounds");
    let f = Fake::default();
    f.publish_claude("2.1.281", &native_exe("a"));
    let o = run(&l, &f, &Policy::default(), claude());
    assert!(matches!(o.verdict, Verdict::Installed { .. }), "{o:?}");
    let head = format!("{CLAUDE}latest");
    assert_eq!(
        f.head_reads.borrow().as_slice(),
        std::slice::from_ref(&head)
    );
    let patient: Vec<String> = f.gets.borrow().iter().map(|(u, _)| u.clone()).collect();
    assert!(
        patient.iter().any(|u| u.ends_with("2.1.281/manifest.json")),
        "{patient:?}"
    );
    f.publish_claude("2.1.282", &native_exe("b"));
    f.downloads_fail.set(true);
    run(&l, &f, &Policy::default(), claude());
    f.downloads_fail.set(false);
    f.head_reads.borrow_mut().clear();
    f.gets.borrow_mut().clear();
    let o = run(&l, &f, &Policy::default(), claude());
    assert!(matches!(o.verdict, Verdict::Installed { .. }), "{o:?}");
    assert_eq!(
        f.head_reads.borrow().as_slice(),
        std::slice::from_ref(&head)
    );
    let unconditional: Vec<String> = f
        .gets
        .borrow()
        .iter()
        .filter(|(u, etag)| *u == head && etag.is_none())
        .map(|(u, _)| u.clone())
        .collect();
    assert_eq!(unconditional, [head], "the head's document, patiently");
}

#[test]
fn a_fresh_install_is_bounded_by_the_floor_not_the_high_water() {
    let l = world::layout("fresh-hw");
    ProgramStamp::record_outcome(&l, "claude", "uninstalled", Some(v("2.1.291"))).unwrap();
    let f = Fake::default();
    f.publish_claude("2.1.290", &native_exe("pulled back"));
    let o = run(&l, &f, &Policy::default(), claude());
    assert!(
        matches!(
            o.verdict,
            Verdict::Installed {
                reason: InstallReason::Fresh,
                ..
            }
        ),
        "{o:?}"
    );
    assert_eq!(shim_build(&l, "claude"), Some(v("2.1.290").build_id()));
}

#[test]
fn an_unchanged_head_that_never_landed_is_fetched_again() {
    let l = world::layout("retry");
    let f = Fake::default();
    f.publish_claude("2.1.281", &native_exe("a"));
    f.downloads_fail.set(true);
    let o = run(&l, &f, &Policy::default(), claude());
    assert!(matches!(o.verdict, Verdict::Failed { .. }), "{o:?}");
    f.downloads_fail.set(false);
    let o = run(&l, &f, &Policy::default(), claude());
    assert!(matches!(o.verdict, Verdict::Installed { .. }), "{o:?}");
    let conditional = f
        .gets
        .borrow()
        .iter()
        .filter(|(_, etag)| etag.is_some())
        .count();
    assert_eq!(
        conditional, 1,
        "the second pass's head was a 304, then asked again"
    );
}

#[test]
fn a_same_version_digest_change_is_refused() {
    let mut rows = vec![
        (claude(), "2.1.282"),
        (codex(), "0.157.0"),
        (agy(), "1.3.7"),
        (kimi(), "2.3.2"),
    ];
    if world::grok_platform().is_some() {
        rows.push((grok(), "1.0.70"));
    }
    for (spec, version) in rows {
        let publish = |f: &Fake, cut: &str| {
            let exe = native_exe(cut);
            match spec.program {
                "claude" => f.publish_claude(version, &exe),
                "codex" => f.publish_codex(version, &[("bin/codex", &exe, 0o755)]),
                "kimi" => drop(publish_kimi(f, version, &exe)),
                "grok" => drop(publish_grok(f, version, &exe)),
                _ => drop(publish_agy(f, version, &exe)),
            }
        };
        let l = world::layout(&format!("recut-{}", spec.program));
        let f = Fake::default();
        publish(&f, "first cut");
        f.downloads_fail.set(true);
        let o = run(&l, &f, &Policy::default(), spec);
        assert!(matches!(o.verdict, Verdict::Failed { .. }), "{o:?}");
        f.downloads_fail.set(false);
        publish(&f, "second cut");
        let downloads = f.downloads.borrow().len();
        let o = run(&l, &f, &Policy::default(), spec);
        assert!(matches!(o.verdict, Verdict::Refused { .. }), "{o:?}");
        assert!(
            o.line().contains(&format!("first seen for {version}")),
            "{}",
            o.line()
        );
        assert_eq!(f.downloads.borrow().len(), downloads, "nothing downloaded");
        assert_eq!(shim_build(&l, spec.program), None);
    }
}

#[test]
fn a_muse_digest_is_logged_only_once_the_team_pin_passed_its_bytes() {
    if unserved_here(muse()) {
        return;
    }
    let l = world::layout("muse-pin");
    let f = Fake::default();
    publish_muse(&f, "1.4.9-R6298", &native_exe("not Meta's"));
    let refusing = world::trust_refusing_signers();
    let o = run_with(&l, &f, &Policy::default(), &refusing, muse(), false);
    assert!(o.line().contains("signer refused"), "{}", o.line());
    let log = super::super::digests::digests_path(&l, "muse");
    assert!(!log.exists(), "nothing the pin refused is logged");
    let genuine = native_exe("Meta's");
    publish_muse(&f, "1.4.9-R6298", &genuine);
    let o = run(&l, &f, &Policy::default(), muse());
    assert!(matches!(o.verdict, Verdict::Installed { .. }), "{o:?}");
    let logged = std::fs::read_to_string(&log).unwrap();
    assert!(logged.contains(&world::sha(&genuine)), "{logged}");
    crate::ops::uninstall(&l, "muse").unwrap();
    publish_muse(&f, "1.4.9-R6298", &native_exe("re-cut"));
    let downloads = f.downloads.borrow().len();
    let o = run(&l, &f, &Policy::default(), muse());
    assert!(
        o.line().contains("first seen for 1.4.9-R6298"),
        "{}",
        o.line()
    );
    assert_eq!(f.downloads.borrow().len(), downloads, "nothing downloaded");
}

#[test]
fn a_build_date_regression_is_kept_at() {
    let l = world::layout("date");
    let f = Fake::default();
    f.publish_claude_dated("2.1.281", &native_exe("a"), "2026-09-21T20:55:27Z");
    run(&l, &f, &Policy::default(), claude());
    f.publish_claude_dated("2.1.282", &native_exe("b"), "2026-09-01T00:00:00Z");
    let o = run(&l, &f, &Policy::default(), claude());
    assert!(
        matches!(
            o.verdict,
            Verdict::Kept {
                reason: KeepReason::BuildDateRegressed,
                ..
            }
        ),
        "{o:?}"
    );
    assert!(f.downloads.borrow().len() == 1);
}
