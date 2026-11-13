// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
// Author: Andrew Yates

use std::cell::Cell;
use std::os::unix::fs::PermissionsExt as _;

use super::*;
use crate::select::Candidate;
use crate::sig::testkit;

const TRIPLE: &str = "aarch64-apple-darwin";

#[test]
fn executable_handoff_survives_sender_process_exit() {
    use std::io::BufRead as _;
    const NAME: &str = "prerequisite::tests::executable_handoff_survives_sender_process_exit";
    const PREFIX: &str = "ATPKG_TEST_HANDOFF_PREFIX";
    const SOCKET: &str = "ATPKG_TEST_HANDOFF_SOCKET";
    if let Some(prefix) = std::env::var_os(PREFIX) {
        let fixture = Fixture::new(Prerequisite::Codex);
        let layout = Layout {
            prefix: prefix.into(),
        };
        let session = Session::resolve(
            &fixture,
            &layout,
            &testkit::anchor(),
            BuildFloor {
                index_build: 0,
                roster_seq: 0,
            },
            testkit::NOW,
            TRIPLE,
        )
        .unwrap();
        let tool = session.acquire_build(Prerequisite::Codex, 7).unwrap();
        let mut stream = aterm_uds::CtlStream::connect(std::env::var_os(SOCKET).unwrap()).unwrap();
        tool.send_handoff(&mut stream).unwrap();
        return;
    }
    if !crate::testdir::isolated_lifecycle(NAME) {
        return;
    }
    struct Sender(aterm_uds::spawnfd::Child);
    impl Drop for Sender {
        fn drop(&mut self) {
            if matches!(self.0.try_wait(), Ok(None)) {
                let _ = self.0.kill();
            }
            let _ = self.0.wait();
        }
    }
    let fixture = Fixture::new(Prerequisite::Codex);
    crate::store::mark_build_ready(&fixture.root).unwrap();
    let socket_dir = aterm_tempfile::Builder::new()
        .prefix("handoff-")
        .suffix(".noindex")
        .tempdir_in("/tmp")
        .unwrap();
    let socket = socket_dir.path().join("s");
    let listener = std::os::unix::net::UnixListener::bind(&socket).unwrap();
    listener.set_nonblocking(true).unwrap();
    let mut child = Sender(
        aterm_uds::spawnfd::command(std::env::current_exe().unwrap())
            .args(["--exact", NAME, "--nocapture"])
            .env(PREFIX, &fixture.layout.prefix)
            .env(SOCKET, &socket)
            .spawn()
            .unwrap(),
    );
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
    let stream = loop {
        match aterm_uds::accept(
            &listener,
            aterm_uds::AcceptWait::Ready,
            aterm_uds::AcceptedMode::Blocking,
        ) {
            Ok((stream, _)) => break stream,
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                assert!(
                    child.0.try_wait().unwrap().is_none(),
                    "sender exited before transfer"
                );
                assert!(
                    std::time::Instant::now() < deadline,
                    "sender connection hang detector"
                );
                std::thread::sleep(std::time::Duration::from_millis(1));
            }
            Err(error) => panic!("accept sender: {error}"),
        }
    };
    // The small fixture frame fits in the socket. Prove custody while its
    // descriptors are still queued in the kernel and the original sender is gone.
    let status = loop {
        if let Some(status) = child.0.try_wait().unwrap() {
            break status;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "sender exit hang detector"
        );
        std::thread::sleep(std::time::Duration::from_millis(1));
    };
    assert!(status.success(), "original sender exits successfully");
    let subject = Subject::build("codex", 7).unwrap();
    assert!(matches!(
        crate::lease::reclaim(&fixture.layout.prefix, &subject),
        crate::lease::Reclaim::Keep(_)
    ));
    let mut marker = [0];
    let received = aterm_uds::fdpass::recv_with_fds(&stream, &mut marker, 2).unwrap();
    assert_eq!((received.bytes, marker, received.fds.len()), (1, *b"X", 2));
    let mut frame = String::new();
    std::io::BufReader::new(stream)
        .read_line(&mut frame)
        .unwrap();
    let receipt: super::handoff::Receipt = aterm_json::from_str(&frame).unwrap();
    assert_eq!(receipt.root, fixture.root.canonicalize().unwrap());
    assert!(
        child.0.wait().unwrap().success(),
        "original sender exits successfully"
    );
    let subject = Subject::build("codex", 7).unwrap();
    assert!(
        matches!(
            crate::lease::reclaim(&fixture.layout.prefix, &subject),
            crate::lease::Reclaim::Keep(_)
        ),
        "a dead sender does not release received custody"
    );
    drop(received);
    assert!(matches!(
        crate::lease::reclaim(&fixture.layout.prefix, &subject),
        crate::lease::Reclaim::Clear(_)
    ));
}

#[test]
fn executable_handoff_retains_original_custody_after_sender_drops() {
    use std::io::BufRead as _;
    if !crate::testdir::isolated_lifecycle(
        "prerequisite::tests::executable_handoff_retains_original_custody_after_sender_drops",
    ) {
        return;
    }
    let fixture = Fixture::new(Prerequisite::Codex);
    crate::store::mark_build_ready(&fixture.root).unwrap();
    let session = fixture.session(TRIPLE).unwrap();
    let tool = session.acquire_build(fixture.want, 7).unwrap();
    let (mut sender, receiver) = aterm_uds::CtlStream::pair().unwrap();
    tool.send_handoff(&mut sender).unwrap();
    drop(sender);
    drop(tool);
    let mut marker = [0];
    let mut received = aterm_uds::fdpass::recv_with_fds(&receiver, &mut marker, 2).unwrap();
    assert_eq!((received.bytes, marker), (1, *b"X"));
    assert_eq!(received.fds.len(), 2);
    let image = std::fs::File::from(received.fds.pop().unwrap());
    let lease = received.fds.pop().unwrap();
    let mut frame = String::new();
    std::io::BufReader::new(receiver)
        .read_line(&mut frame)
        .unwrap();
    let receipt: super::handoff::Receipt = aterm_json::from_str(&frame).unwrap();
    assert_eq!((receipt.protocol, receipt.build), (1, 7));
    assert_eq!(receipt.program, "codex");
    assert_eq!(
        receipt.executable,
        fixture.executable().canonicalize().unwrap()
    );
    use std::os::unix::fs::MetadataExt as _;
    let metadata = image.metadata().unwrap();
    assert_eq!(
        (receipt.device, receipt.inode, receipt.size),
        (metadata.dev(), metadata.ino(), metadata.len())
    );
    let subject = Subject::build("codex", 7).unwrap();
    assert!(matches!(
        crate::lease::reclaim(&fixture.layout.prefix, &subject),
        crate::lease::Reclaim::Keep(_)
    ));
    drop(image);
    drop(lease);
    assert!(matches!(
        crate::lease::reclaim(&fixture.layout.prefix, &subject),
        crate::lease::Reclaim::Clear(_)
    ));
}

#[test]
fn executable_handoff_refuses_changed_image_before_sending_custody() {
    use std::io::Read as _;
    let fixture = Fixture::new(Prerequisite::Codex);
    crate::store::mark_build_ready(&fixture.root).unwrap();
    let session = fixture.session(TRIPLE).unwrap();
    let tool = session.acquire_build(fixture.want, 7).unwrap();
    std::fs::write(fixture.executable(), b"changed after admission").unwrap();
    let (mut sender, mut receiver) = aterm_uds::CtlStream::pair().unwrap();
    assert!(tool.send_handoff(&mut sender).is_err());
    drop(sender);
    assert_eq!(receiver.read(&mut [0]).unwrap(), 0);
}

#[test]
fn persistent_manifest_bytes_avoid_fetches_but_recheck_each_admission() {
    let mut fixture = Fixture::new(Prerequisite::Gitleaks);
    fixture.cache_source = Some("fixture:one".into());
    drop(
        fixture
            .session(TRIPLE)
            .unwrap()
            .acquire(fixture.want)
            .unwrap(),
    );
    fixture.manifest_unavailable.set(true);
    drop(
        fixture
            .session(TRIPLE)
            .unwrap()
            .acquire(fixture.want)
            .unwrap(),
    );
    assert_eq!(fixture.manifest_reads.get(), 1);
    assert_eq!(fixture.index_reads.get(), 2);

    std::fs::write(fixture.executable(), b"changed after cache population").unwrap();
    assert!(
        fixture
            .session(TRIPLE)
            .unwrap()
            .acquire(fixture.want)
            .is_err()
    );
    assert_eq!(fixture.manifest_reads.get(), 1);
}

#[test]
fn persistent_manifest_cache_never_supplies_authority() {
    for fault in [
        "source",
        "signature",
        "identity",
        "yank",
        "rotation",
        "corrupt",
    ] {
        let mut fixture = Fixture::new(Prerequisite::Gitleaks);
        fixture.cache_source = Some("fixture:one".into());
        drop(
            fixture
                .session(TRIPLE)
                .unwrap()
                .acquire(fixture.want)
                .unwrap(),
        );
        let layout = fixture.layout.clone();
        let source = fixture.cache_source.clone().unwrap();
        let key = crate::signed_artifact_cache::SignedArtifactKey::package(
            &source,
            fixture.want.program(),
            fixture.want.program(),
            7,
        );
        let cache = crate::signed_artifact_cache::SignedArtifactCache::for_layout(&layout);
        let document_cap = usize::try_from(crate::net::MANIFEST_CAP).unwrap();
        let signature_cap = usize::try_from(crate::net::SIG_CAP).unwrap();
        match fault {
            "source" => fixture.cache_source = Some("fixture:two".into()),
            "signature" => cache
                .store_verified(
                    &key,
                    fixture.package.as_bytes(),
                    &[0; 64],
                    document_cap,
                    signature_cap,
                )
                .unwrap(),
            "identity" => {
                fixture.change_package("program=\"gitleaks\"", "program=\"other\"");
                cache
                    .store_verified(
                        &key,
                        fixture.package.as_bytes(),
                        &fixture.signature,
                        document_cap,
                        signature_cap,
                    )
                    .unwrap();
            }
            "yank" => fixture.index.push_str("yanked=[\"gitleaks@7\"]\n"),
            "rotation" => fixture.index_seed = testkit::OUTSIDER_SEED,
            "corrupt" => {
                let object = cache.object_path_for_key(&key).unwrap();
                std::fs::write(object, b"invalid").unwrap();
            }
            _ => unreachable!(),
        }
        fixture.manifest_unavailable.set(true);
        assert!(
            fixture
                .session(TRIPLE)
                .unwrap()
                .acquire(fixture.want)
                .is_err(),
            "{fault}"
        );
        assert_eq!(
            fixture.manifest_reads.get(),
            if fault == "yank" { 1 } else { 2 },
            "{fault}"
        );
    }
}

#[test]
fn named_controller_retains_its_world_through_background_work_and_cancellation() {
    if !crate::testdir::isolated_lifecycle(
        "prerequisite::tests::named_controller_retains_its_world_through_background_work_and_cancellation",
    ) {
        return;
    }
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::time::{Duration, Instant};
    fn until(mut ready: impl FnMut() -> bool) {
        let deadline = Instant::now() + Duration::from_secs(60);
        while !ready() {
            assert!(
                Instant::now() < deadline,
                "native world fixture did not progress"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
    }
    struct Release(PathBuf);
    impl Drop for Release {
        fn drop(&mut self) {
            let _ = std::fs::write(self.0.join("release"), b"go");
        }
    }
    for cancel in [false, true] {
        let mut fixture = Fixture::new(Prerequisite::Lake);
        fixture.historical_lean();
        crate::store::mark_build_ready(&fixture.root).unwrap();
        let tool = fixture
            .session(TRIPLE)
            .unwrap()
            .acquire_build(Prerequisite::Lake, 7)
            .unwrap();
        let held = super::named::HeldBuild::new(Prerequisite::Lake, tool).unwrap();
        let home = held.elan_home().unwrap().to_path_buf();
        let scope = aterm_tempfile::tempdir().unwrap();
        let scope_path = scope.path().to_path_buf();
        let settled = Arc::new(AtomicBool::new(false));
        let signalled = Arc::new(AtomicBool::new(false));
        let (s, sig) = (settled.clone(), signalled.clone());
        let (prefix, home_copy, work) = (
            fixture.layout.prefix.clone(),
            home.clone(),
            scope_path.clone(),
        );
        let helper = std::thread::spawn(move || {
            let release = Release(work.clone());
            until(|| s.load(Ordering::Acquire) && work.join("ready").is_file());
            assert!(
                home_copy
                    .join("toolchains/leanprover--lean4---v4.8.0/bin/lake")
                    .is_file()
            );
            assert!(matches!(
                crate::lease::reclaim(&prefix, &Subject::build("lean4", 7).unwrap()),
                crate::lease::Reclaim::Keep(_)
            ));
            if cancel {
                // SAFETY: this isolated test owns its process and its armed native front.
                assert_eq!(unsafe { libc::kill(libc::getpid(), libc::SIGTERM) }, 0);
                until(|| sig.load(Ordering::Acquire));
            }
            drop(release);
        });
        let script = r#"
scope=$1
test "$LEAN_CC" = /usr/bin/cc || exit 90
(
  test -f "$ELAN_HOME/toolchains/leanprover--lean4---v4.8.0/bin/lake" || exit 91
  touch "$scope/ready"
  while [ ! -e "$scope/release" ]; do /bin/sleep 0.02; done
  test -f "$ELAN_HOME/toolchains/leanprover--lean4---v4.8.0/bin/lake" || exit 92
  touch "$scope/done"
) &
exit 7
"#;
        let outcome = held
            .run(
                "/bin/sh",
                &[
                    "-c".into(),
                    script.into(),
                    "fixture".into(),
                    scope_path.display().to_string(),
                ],
                &mut |line| {
                    if line.contains("root ended") {
                        settled.store(true, Ordering::Release);
                    }
                    if line.contains("cancellation") {
                        signalled.store(true, Ordering::Release);
                    }
                },
            )
            .unwrap();
        helper.join().unwrap();
        assert_eq!(outcome.root.code(), Some(7));
        assert_eq!(outcome.signal, cancel.then_some(libc::SIGTERM));
        assert!(scope.path().join("done").is_file());
        assert!(home.is_dir());
        drop(held);
        assert!(!home.exists());
    }
}

#[test]
fn named_lean_install_survives_gc_before_lease_and_expired_use_still_needs_custody() {
    if !crate::testdir::isolated_lifecycle(
        "prerequisite::tests::named_lean_install_survives_gc_before_lease_and_expired_use_still_needs_custody",
    ) {
        return;
    }
    let mut fixture = Fixture::new(Prerequisite::Lake);
    fixture.historical_lean();
    fixture.fresh_install();
    fixture.index = fixture.index.replace("lean4=7", "lean4=1");
    let current = fixture.layout.build_dir("lean4", 1);
    std::fs::create_dir_all(current.join("bin")).unwrap();
    std::fs::write(
        current.join("bin/lake"),
        b"selected default, never executed",
    )
    .unwrap();
    std::fs::set_permissions(
        current.join("bin/lake"),
        std::fs::Permissions::from_mode(0o755),
    )
    .unwrap();
    crate::store::mark_build_ready(&current).unwrap();
    crate::activate::install_tools(
        &fixture.layout,
        &current,
        &[crate::store::ToolName::new("lake").unwrap()],
        crate::activate::Aliases::Off,
    )
    .unwrap();
    crate::activate::activate_build(&fixture.layout, &current).unwrap();
    let selected = crate::ops::which(&fixture.layout, "lake");
    let install = || {
        crate::flow::install_build(
            &fixture,
            &fixture.layout,
            &testkit::anchor(),
            BuildFloor {
                index_build: 0,
                roster_seq: 0,
            },
            testkit::NOW,
            &crate::InstallRequest {
                channel: "stable",
                program: "lean4",
                triple: TRIPLE,
                installed: Some(1),
            },
            crate::flow::NamedAsk {
                build: 7,
                expect_root: None,
                retry: false,
            },
        )
        .unwrap()
    };
    let mark = fixture
        .root
        .with_file_name(format!("7{}", crate::trust_lock::LOCKED_SUFFIX));
    let expire = || {
        let at = std::time::SystemTime::now()
            - crate::trust_lock::LOCKED_KEEP
            - std::time::Duration::from_secs(60);
        std::fs::OpenOptions::new()
            .write(true)
            .open(&mark)
            .unwrap()
            .set_modified(at)
            .unwrap();
    };
    assert!(matches!(install(), crate::flow::NamedBuild::Staged { .. }));
    assert!(crate::gc::run(&fixture.layout, None).reclaimed.is_empty());
    assert!(
        fixture.root.is_dir(),
        "GC between install and lease retains the exact named build"
    );
    expire();
    assert!(matches!(install(), crate::flow::NamedBuild::Present { .. }));
    assert_eq!(
        fixture.downloads.get(),
        1,
        "retention refresh needs no new archive"
    );
    assert!(crate::gc::run(&fixture.layout, None).reclaimed.is_empty());
    assert_eq!(crate::ops::which(&fixture.layout, "lake"), selected);

    // Negative control: without the installation mark, GC produces the observed
    // supported-install -> absent-at-lease failure; a sidecar never authenticates bytes.
    std::fs::remove_file(&mark).unwrap();
    assert_eq!(
        crate::gc::run(&fixture.layout, None).reclaimed,
        vec![("lean4".to_string(), vec![7])]
    );
    let session = fixture.session(TRIPLE).unwrap();
    assert!(
        session
            .acquire_build(Prerequisite::Lake, 7)
            .unwrap_err()
            .contains("is not staged")
    );

    assert!(matches!(install(), crate::flow::NamedBuild::Staged { .. }));
    let held = session.acquire_build(Prerequisite::Lake, 7).unwrap();
    expire();
    assert!(
        crate::gc::run(&fixture.layout, None).reclaimed.is_empty(),
        "a real lease keeps expired recent use alive"
    );
    assert_eq!(held.root(), fixture.root.canonicalize().unwrap());
    drop(held);
    assert_eq!(
        crate::gc::run(&fixture.layout, None).reclaimed,
        vec![("lean4".to_string(), vec![7])]
    );
    assert_eq!(crate::ops::which(&fixture.layout, "lake"), selected);
}

#[test]
fn named_lean_build_keeps_default_selection_and_binds_its_import_world() {
    let mut fixture = Fixture::new(Prerequisite::Lake);
    fixture.historical_lean();
    fixture.index = fixture.index.replace("lean4=7", "lean4=9");
    crate::store::mark_build_ready(&fixture.root).unwrap();
    let before = crate::ops::which(&fixture.layout, "lake");
    let session = fixture.session(TRIPLE).unwrap();
    let tool = session.acquire_build(Prerequisite::Lake, 7).unwrap();
    assert_eq!(tool.version(), "4.8.0");
    assert_eq!(
        tool.environment().entries(),
        &[("LEAN_CC".into(), "/usr/bin/cc".into())]
    );
    assert_eq!(tool.root(), fixture.root.canonicalize().unwrap());
    let held = super::named::HeldBuild::new(Prerequisite::Lake, tool).unwrap();
    let world = held
        .elan_home()
        .unwrap()
        .join("toolchains/leanprover--lean4---v4.8.0");
    assert_eq!(
        world.canonicalize().unwrap(),
        fixture.root.canonicalize().unwrap()
    );
    assert!(
        held.child_path(std::ffi::OsStr::new("/usr/bin:/bin"))
            .is_ok()
    );
    assert_eq!(crate::ops::which(&fixture.layout, "lake"), before);
    std::fs::remove_file(&world).unwrap();
    std::fs::create_dir(&world).unwrap();
    assert!(
        held.child_path(std::ffi::OsStr::new("/usr/bin:/bin"))
            .is_err()
    );
}

#[test]
fn named_prerequisites_use_packaged_commands_instead_of_recipe_build_outputs() {
    for want in [
        Prerequisite::Gitleaks,
        Prerequisite::OpenSslSigning,
        Prerequisite::Python,
        Prerequisite::Lake,
    ] {
        let mut fixture = Fixture::new(want);
        if want == Prerequisite::Lake {
            fixture.historical_lean();
        }
        let recipe_output = want.recipe().unwrap().executable;
        if matches!(want, Prerequisite::Gitleaks | Prerequisite::OpenSslSigning) {
            assert_ne!(fixture.root.join(recipe_output), fixture.executable());
            assert!(!fixture.root.join(recipe_output).exists());
        }
        crate::store::mark_build_ready(&fixture.root).unwrap();
        let selected = crate::ops::which(&fixture.layout, want.executable());
        let session = fixture.session(TRIPLE).unwrap();
        let tool = session.acquire_build(want, 7).unwrap();
        assert_eq!(tool.path(), fixture.executable().canonicalize().unwrap());
        assert_eq!(
            tool.tree_root(),
            crate::tree::tree_root(&fixture.root).unwrap()
        );
        assert_eq!(want.recipe().unwrap().executable, recipe_output);
        tool.validate().unwrap();
        assert_eq!(
            crate::ops::which(&fixture.layout, want.executable()),
            selected
        );
    }
}

#[test]
fn named_prerequisites_refuse_signed_recipe_outputs_without_packaged_commands() {
    for want in [Prerequisite::Gitleaks, Prerequisite::OpenSslSigning] {
        let mut fixture = Fixture::new(want);
        let before = crate::tree::tree_root(&fixture.root).unwrap();
        let recipe_output = fixture.root.join(want.recipe().unwrap().executable);
        std::fs::create_dir_all(recipe_output.parent().unwrap()).unwrap();
        std::fs::rename(fixture.executable(), &recipe_output).unwrap();
        let after = crate::tree::tree_root(&fixture.root).unwrap();
        fixture.change_package(&before, &after);
        crate::store::mark_build_ready(&fixture.root).unwrap();
        assert!(recipe_output.is_file());
        assert!(!fixture.executable().exists());
        assert!(
            fixture
                .session(TRIPLE)
                .unwrap()
                .acquire_build(want, 7)
                .is_err()
        );
    }
}

#[test]
fn named_prerequisite_refuses_unknown_versions_unstaged_or_changed_payloads() {
    for fault in ["unknown-version", "unstaged", "changed"] {
        let mut fixture = Fixture::new(Prerequisite::Lake);
        fixture.change_package(
            "version=\"4.31.0\"",
            if fault == "unknown-version" {
                "version=\"99.0.0\""
            } else {
                "version=\"4.8.0\""
            },
        );
        if fault != "unknown-version" {
            fixture.change_package("exposes=[", "shim_env=[\"LEAN_CC=/usr/bin/cc\"]\nexposes=[");
        }
        if fault != "unstaged" {
            crate::store::mark_build_ready(&fixture.root).unwrap();
        }
        if fault == "changed" {
            std::fs::write(fixture.executable(), b"changed").unwrap();
        }
        assert!(
            fixture
                .session(TRIPLE)
                .unwrap()
                .acquire_build(Prerequisite::Lake, 7)
                .is_err(),
            "{fault}"
        );
    }
}

#[test]
fn named_lean_refuses_missing_alternate_and_hostile_signed_environments() {
    for entries in [
        "[]",
        "[\"LEAN_CC=/tmp/other-cc\"]",
        "[\"LEAN_CC=/usr/bin/cc\",\"PATH=/tmp\"]",
    ] {
        let mut fixture = Fixture::new(Prerequisite::Lake);
        fixture.historical_lean();
        fixture.change_package(
            "shim_env=[\"LEAN_CC=/usr/bin/cc\"]",
            &format!("shim_env={entries}"),
        );
        crate::store::mark_build_ready(&fixture.root).unwrap();
        let before = crate::ops::which(&fixture.layout, "lake");
        assert!(
            fixture
                .session(TRIPLE)
                .unwrap()
                .acquire_build(Prerequisite::Lake, 7)
                .is_err()
        );
        assert_eq!(crate::ops::which(&fixture.layout, "lake"), before);
    }
}

struct Fixture {
    layout: crate::testdir::ScratchLayout,
    want: Prerequisite,
    root: PathBuf,
    index: String,
    package: String,
    signature: Vec<u8>,
    index_reads: Cell<usize>,
    index_fails_after_first: Cell<bool>,
    manifest_reads: Cell<usize>,
    manifest_fails_after_first: Cell<bool>,
    after_first: Option<(Vec<u8>, Vec<u8>)>,
    archive: Option<Vec<u8>>,
    downloads: Cell<usize>,
    cache_source: Option<String>,
    manifest_unavailable: Cell<bool>,
    index_seed: [u8; 32],
}

fn fixture_version(want: Prerequisite) -> &'static str {
    want.recipe().map_or("0.0.0", |recipe| recipe.version)
}

impl Fixture {
    fn new(want: Prerequisite) -> Self {
        let layout = crate::testdir::layout("signed-prerequisite");
        let root = layout.build_dir(want.program(), 7);
        std::fs::create_dir_all(root.join("bin")).unwrap();
        let executable = root.join("bin").join(want.executable());
        std::fs::write(&executable, b"synthetic fixture, never executed").unwrap();
        std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o755)).unwrap();
        crate::activate::install_tools(
            &layout,
            &root,
            &[crate::store::ToolName::new(want.executable()).unwrap()],
            crate::activate::Aliases::Off,
        )
        .unwrap();
        let tree = crate::tree::tree_root(&root).unwrap();
        let attr = format!(
            "machine_id = \"{}\"\nroster_seq = {}\n",
            testkit::MACHINE_ID,
            testkit::SEQ
        );
        let program = want.program();
        let index = format!(
            "schema=2\nindex_build=41\nvalid_until=\"2099-01-01T00:00:00Z\"\n{attr}[programs.{program}]\nrepo=\"{program}\"\n[[channels]]\nname=\"stable\"\npin={{ {program}=7 }}\n"
        );
        let package = format!(
            "schema=2\n{attr}program=\"{program}\"\nversion=\"{}\"\nbuild_number=7\nexposes=[\"{}\"]\n[[artifact]]\ntarget=\"{TRIPLE}\"\nkind=\"sysroot-bundle\"\nasset=\"fixture.tar.zst\"\nsha256=\"{}\"\ntree_root=\"{tree}\"\nsize=1\n",
            fixture_version(want),
            want.executable(),
            "a".repeat(64)
        );
        let signature = testkit::sign(&testkit::MACHINE_SEED, package.as_bytes());
        Self {
            layout,
            want,
            root,
            index,
            package,
            signature,
            index_reads: Cell::new(0),
            index_fails_after_first: Cell::new(false),
            manifest_reads: Cell::new(0),
            manifest_fails_after_first: Cell::new(false),
            after_first: None,
            archive: None,
            downloads: Cell::new(0),
            cache_source: None,
            manifest_unavailable: Cell::new(false),
            index_seed: testkit::MACHINE_SEED,
        }
    }

    fn session(&self, triple: &'static str) -> Result<Session<'_>, crate::FlowError> {
        Session::resolve(
            self,
            &self.layout,
            &testkit::anchor(),
            BuildFloor {
                index_build: 0,
                roster_seq: 0,
            },
            testkit::NOW,
            triple,
        )
    }

    fn historical_lean(&mut self) {
        self.change_package("version=\"4.31.0\"", "version=\"4.8.0\"");
        self.change_package("exposes=[", "shim_env=[\"LEAN_CC=/usr/bin/cc\"]\nexposes=[");
    }

    fn change_package(&mut self, from: &str, to: &str) {
        assert!(self.package.contains(from), "nonvacuous mutation");
        self.package = self.package.replace(from, to);
        self.signature = testkit::sign(&testkit::MACHINE_SEED, self.package.as_bytes());
    }

    fn executable(&self) -> PathBuf {
        self.root.join("bin").join(self.want.executable())
    }

    fn fresh_install(&mut self) {
        let old_root = crate::tree::tree_root(&self.root).unwrap();
        let body = b"#!/bin/true\n";
        std::fs::write(self.executable(), body).unwrap();
        let new_root = crate::tree::tree_root(&self.root).unwrap();
        let tar = crate::extract::fixtures::tar_bytes(&[(
            &format!("bin/{}", self.want.executable()),
            b'0',
            "",
            body,
            0o755,
        )]);
        let archive = crate::extract::fixtures::zstd_bytes(&tar);
        self.change_package(&old_root, &new_root);
        self.change_package(&"a".repeat(64), &crate::tree::sha256_bytes(&archive));
        self.change_package("size=1\n", &format!("size={}\n", archive.len()));
        self.archive = Some(archive);
        std::fs::remove_file(
            self.layout
                .shim(&crate::store::ToolName::new(self.want.executable()).unwrap()),
        )
        .unwrap();
        std::fs::remove_dir_all(&self.root).unwrap();
    }
}

impl Fetcher for Fixture {
    fn package_source_id(&self, _: &str, _: &str) -> Option<String> {
        self.cache_source.clone().or_else(|| {
            Some(format!(
                "fixture:{}/{}",
                self.want.program(),
                self.want.program()
            ))
        })
    }

    fn index_candidates(&self) -> Result<Vec<Candidate>, String> {
        self.index_reads.set(self.index_reads.get() + 1);
        if self.index_reads.get() > 1 && self.index_fails_after_first.get() {
            return Err("curl: (56) channel freshness transport failed".to_string());
        }
        let mut roster = testkit::roster();
        roster.machines[0].pubkey = testkit::pubkey_b64(&self.index_seed);
        let roster_bytes = roster.to_toml().unwrap().into_bytes();
        let roster_sig = testkit::sign(&testkit::MASTER_SEED, &roster_bytes);
        Ok(vec![Candidate {
            label: "fixture41".into(),
            index_bytes: self.index.as_bytes().to_vec(),
            sig: testkit::sign(&self.index_seed, self.index.as_bytes()),
            roster_bytes,
            roster_sig,
        }])
    }

    fn pkg_manifest(
        &self,
        repo: &str,
        program: &str,
        build: u64,
    ) -> Result<(Vec<u8>, Vec<u8>), String> {
        assert_eq!(
            (repo, program, build),
            (self.want.program(), self.want.program(), 7)
        );
        self.manifest_reads.set(self.manifest_reads.get() + 1);
        if self.manifest_reads.get() > 1 && self.manifest_fails_after_first.get() {
            return Err("curl: (56) exact package download blocked".to_string());
        }
        if self.manifest_unavailable.get() {
            return Err("fixture offline".into());
        }
        if self.manifest_reads.get() > 1
            && let Some(pair) = &self.after_first
        {
            return Ok(pair.clone());
        }
        Ok((self.package.as_bytes().to_vec(), self.signature.clone()))
    }

    fn index_error_is_transport(&self, error: &str) -> bool {
        error.contains("freshness transport failed")
    }

    fn download(&self, _: &str, asset: &str, to: &Path) -> Result<(), String> {
        assert_eq!(asset, "fixture.tar.zst");
        self.downloads.set(self.downloads.get() + 1);
        let archive = self
            .archive
            .as_deref()
            .expect("a prerequisite lookup never downloads a payload");
        std::fs::write(to, archive).map_err(|e| e.to_string())
    }
}

/// NEGATIVE CONTROL: an index cache can authenticate old bytes, but it cannot answer whether
/// the channel moved. A publication prerequisite must surface the failed freshness transport so
/// its caller reports COULD NOT RUN instead of silently treating the cached head as current.
#[test]
fn a_cached_index_does_not_turn_a_failed_freshness_read_into_prerequisite_authority() {
    let fixture = Fixture::new(Prerequisite::Gitleaks);
    let first = fixture.session(TRIPLE);
    assert!(
        first.is_ok(),
        "the live read seeds the ordinary index fallback"
    );
    drop(first);

    fixture.index_fails_after_first.set(true);
    let error = match fixture.session(TRIPLE) {
        Ok(_) => panic!("cached channel bytes cannot answer a live freshness question"),
        Err(error) => error,
    };
    match error {
        crate::FlowError::Unreachable(why) => {
            assert!(why.contains("channel freshness transport failed"), "{why}");
            let admitted = super::admission_error(crate::FlowError::Unreachable(why));
            assert_eq!(
                super::freshness_transport(&admitted),
                Some("curl: (56) channel freshness transport failed"),
                "the real session error retains its transport class through admission"
            );
        }
        other => panic!("freshness transport was misclassified as {other:?}"),
    }
    assert_eq!(
        fixture.index_reads.get(),
        2,
        "the second run still asks the source"
    );
}

#[test]
fn a_second_run_reads_a_verified_package_copy_but_still_reads_freshness() {
    let fixture = Fixture::new(Prerequisite::Gitleaks);
    let first = fixture
        .session(TRIPLE)
        .unwrap()
        .acquire(Prerequisite::Gitleaks)
        .unwrap();
    drop(first);
    assert_eq!(
        (fixture.index_reads.get(), fixture.manifest_reads.get()),
        (1, 1)
    );

    fixture.manifest_fails_after_first.set(true);
    let second = fixture
        .session(TRIPLE)
        .unwrap()
        .acquire(Prerequisite::Gitleaks)
        .unwrap();
    second.validate().unwrap();
    assert_eq!(
        fixture.index_reads.get(),
        2,
        "the cache does not answer whether the channel head moved"
    );
    assert_eq!(
        fixture.manifest_reads.get(),
        1,
        "the second exact signed package read is wholly local"
    );
}

#[test]
fn corrupt_cached_package_bytes_fall_back_to_transport_and_cannot_authorize_a_mismatch() {
    let mut fixture = Fixture::new(Prerequisite::Gitleaks);
    let first = fixture
        .session(TRIPLE)
        .unwrap()
        .acquire(Prerequisite::Gitleaks)
        .unwrap();
    drop(first);

    let source =
        Fetcher::package_source_id(&fixture, fixture.want.program(), fixture.want.program())
            .unwrap();
    let key = crate::signed_artifact_cache::SignedArtifactKey::package(
        &source,
        fixture.want.program(),
        fixture.want.program(),
        7,
    );
    let cache = crate::signed_artifact_cache::SignedArtifactCache::for_layout(&fixture.layout);
    let object = cache
        .object_path_for_key(&key)
        .expect("the first admission caches its verified signed pair");
    let mut changed = std::fs::read(&object).unwrap();
    *changed.last_mut().unwrap() ^= 1;
    std::fs::write(&object, changed).unwrap();

    let wrong = fixture
        .package
        .replace("program=\"gitleaks\"", "program=\"foreign\"");
    assert_ne!(wrong, fixture.package, "nonvacuous network mismatch");
    fixture.after_first = Some((
        wrong.as_bytes().to_vec(),
        testkit::sign(&testkit::MACHINE_SEED, wrong.as_bytes()),
    ));
    let error = match fixture
        .session(TRIPLE)
        .unwrap()
        .acquire(Prerequisite::Gitleaks)
    {
        Ok(_) => panic!("the signed network mismatch cannot authorize gitleaks"),
        Err(error) => error,
    };
    assert!(error.contains("identity differs"), "{error}");
    assert_eq!(
        (fixture.index_reads.get(), fixture.manifest_reads.get()),
        (2, 2),
        "freshness and the network fallback both ran; corrupt local bytes authorized nothing"
    );
}

#[test]
fn content_addressed_but_wrong_cached_identity_is_reverified_before_network_fallback() {
    let mut fixture = Fixture::new(Prerequisite::Gitleaks);
    let first = fixture
        .session(TRIPLE)
        .unwrap()
        .acquire(Prerequisite::Gitleaks)
        .unwrap();
    drop(first);

    let source =
        Fetcher::package_source_id(&fixture, fixture.want.program(), fixture.want.program())
            .unwrap();
    let key = crate::signed_artifact_cache::SignedArtifactKey::package(
        &source,
        fixture.want.program(),
        fixture.want.program(),
        7,
    );
    let cache = crate::signed_artifact_cache::SignedArtifactCache::for_layout(&fixture.layout);
    let cached_wrong = fixture
        .package
        .replace("program=\"gitleaks\"", "program=\"cache-poison\"");
    assert_ne!(cached_wrong, fixture.package, "nonvacuous cached mismatch");
    let cached_wrong_sig = testkit::sign(&testkit::MACHINE_SEED, cached_wrong.as_bytes());
    // Deliberately violate store_verified's caller precondition to prove that content addressing
    // alone grants no authority: Session must rerun signature and semantic admission on the hit.
    cache
        .store_verified(
            &key,
            cached_wrong.as_bytes(),
            &cached_wrong_sig,
            usize::try_from(crate::net::MANIFEST_CAP).unwrap(),
            usize::try_from(crate::net::SIG_CAP).unwrap(),
        )
        .unwrap();

    let network_wrong = fixture.package.replace("build_number=7", "build_number=8");
    assert_ne!(
        network_wrong, fixture.package,
        "nonvacuous network mismatch"
    );
    fixture.after_first = Some((
        network_wrong.as_bytes().to_vec(),
        testkit::sign(&testkit::MACHINE_SEED, network_wrong.as_bytes()),
    ));
    let error = match fixture
        .session(TRIPLE)
        .unwrap()
        .acquire(Prerequisite::Gitleaks)
    {
        Ok(_) => panic!("neither wrong signed identity can authorize gitleaks"),
        Err(error) => error,
    };
    assert!(error.contains("identity differs"), "{error}");
    assert_eq!(
        fixture.manifest_reads.get(),
        2,
        "the hash-valid cache poison was rejected before the network mismatch"
    );
}

#[test]
fn declared_prerequisites_admit_only_signed_bytes_and_hold_their_build_until_drop() {
    if !crate::testdir::isolated_lifecycle(
        "prerequisite::tests::declared_prerequisites_admit_only_signed_bytes_and_hold_their_build_until_drop",
    ) {
        return;
    }
    for want in [
        Prerequisite::Gitleaks,
        Prerequisite::Python,
        Prerequisite::OpenSslSigning,
        Prerequisite::Lake,
        Prerequisite::AtpkgKeys,
        Prerequisite::Ay,
        Prerequisite::Codex,
    ] {
        let fixture = Fixture::new(want);
        let session = fixture.session(TRIPLE).unwrap();
        let tool = session.acquire(want).unwrap();
        assert_eq!(tool.path(), fixture.executable());
        assert_eq!((tool.program(), tool.build()), (want.program(), 7));
        assert!(tool.environment().is_empty());
        assert_eq!(fixture.index_reads.get(), 1);
        let subject = Subject::build(want.program(), 7).unwrap();
        assert!(matches!(
            crate::lease::reclaim(&fixture.layout.prefix, &subject),
            crate::lease::Reclaim::Keep(_)
        ));
        #[cfg(unix)]
        let payload = aterm_uds::kept_out(|| tool.lease_descriptor()).unwrap();
        drop(tool);
        #[cfg(unix)]
        {
            assert!(matches!(
                crate::lease::reclaim(&fixture.layout.prefix, &subject),
                crate::lease::Reclaim::Keep(_)
            ));
            drop(payload);
        }
        let reclaimed = crate::lease::reclaim(&fixture.layout.prefix, &subject);
        assert!(
            matches!(reclaimed, crate::lease::Reclaim::Clear(_)),
            "{} after releasing its only owned guard: {reclaimed:?}",
            want.executable()
        );
        assert!(
            session
                .acquire_with_lease_wait(want, std::time::Duration::ZERO)
                .is_err(),
            "a reclaimer's held exclusive gate must prevent a new selection"
        );
        drop(reclaimed);
        drop(
            session
                .acquire_with_lease_wait(want, std::time::Duration::ZERO)
                .unwrap(),
        );
    }
    #[cfg(any(target_vendor = "apple", target_os = "linux"))]
    a_dropped_prerequisite_is_reclaimable_while_a_forked_child_holds_its_copy();
}

#[test]
fn first_party_selected_and_named_leases_take_the_version_from_the_signed_package() {
    for want in [Prerequisite::AtpkgKeys, Prerequisite::Codex] {
        let mut fixture = Fixture::new(want);
        fixture.change_package("version=\"0.0.0\"", "version=\"0.1.0\"");
        crate::store::mark_build_ready(&fixture.root).unwrap();
        let session = fixture.session(TRIPLE).unwrap();
        let selected = session.acquire(fixture.want).unwrap();
        assert_eq!(selected.version(), "0.1.0");
        assert_eq!(selected.program(), want.program());
        selected.validate().unwrap();
        drop(selected);
        let named = session.acquire_build(fixture.want, 7).unwrap();
        assert_eq!(named.version(), "0.1.0");
        assert_eq!(named.path(), fixture.executable().canonicalize().unwrap());
        named.validate().unwrap();
    }
}

#[test]
fn first_party_admission_refuses_forged_identity_environment_target_and_payload() {
    for want in [Prerequisite::AtpkgKeys, Prerequisite::Codex] {
        for case in [
            "signature",
            "program",
            "command",
            "environment",
            "target",
            "payload",
        ] {
            let mut fixture = Fixture::new(want);
            crate::store::mark_build_ready(&fixture.root).unwrap();
            match case {
                "signature" => fixture.signature[0] ^= 1,
                "program" => fixture.change_package(
                    &format!("program=\"{}\"", want.program()),
                    "program=\"different\"",
                ),
                "command" => fixture.change_package(
                    &format!("exposes=[\"{}\"]", want.executable()),
                    "exposes=[\"different\"]",
                ),
                "environment" => fixture
                    .change_package("exposes=[", "shim_env=[\"LEAN_CC=/usr/bin/cc\"]\nexposes=["),
                "target" => fixture.change_package(TRIPLE, "x86_64-apple-darwin"),
                "payload" => {
                    std::fs::write(fixture.executable(), b"changed after signed publication")
                        .unwrap()
                }
                _ => unreachable!(),
            }
            let session = fixture.session(TRIPLE).unwrap();
            assert!(session.acquire(fixture.want).is_err(), "selected {case}");
            assert!(
                session.acquire_build(fixture.want, 7).is_err(),
                "named {case}"
            );
        }
    }
}

/// A dropped prerequisite lease lets go by `LOCK_UN`, so a child another thread forked, holding a
/// copy of its descriptor only until its `exec` closes it, does not keep the build held.
#[cfg(any(target_vendor = "apple", target_os = "linux"))]
fn a_dropped_prerequisite_is_reclaimable_while_a_forked_child_holds_its_copy() {
    let fixture = Fixture::new(Prerequisite::Gitleaks);
    let tool = fixture
        .session(TRIPLE)
        .unwrap()
        .acquire(fixture.want)
        .unwrap();
    let subject = Subject::build("gitleaks", 7).unwrap();
    let child = aterm_uds::fork_window::Parked::park();
    drop(tool);
    let reclaimed = crate::lease::reclaim(&fixture.layout.prefix, &subject);
    assert!(
        matches!(reclaimed, crate::lease::Reclaim::Clear(_)),
        "a dropped lease is let go beside a forked child: {reclaimed:?}"
    );
    drop(reclaimed);
    assert!(
        child.release().success(),
        "the owned child execs and is reaped"
    );
}

#[test]
fn provision_reuses_the_signed_pin_and_rechecks_current_bytes_with_a_lease() {
    let fixture = Fixture::new(Prerequisite::Gitleaks);
    crate::store::mark_build_ready(&fixture.root).unwrap();
    let session = fixture.session(TRIPLE).unwrap();
    assert!(session.install(fixture.want).unwrap());
    let held = session.acquire_all(&[fixture.want]).unwrap();
    assert_eq!(
        held.tool(fixture.want).unwrap().path(),
        fixture.executable()
    );
    assert_eq!(
        fixture.index_reads.get(),
        1,
        "one authenticated channel snapshot"
    );
    let subject = Subject::build("gitleaks", 7).unwrap();
    assert!(matches!(
        crate::lease::reclaim(&fixture.layout.prefix, &subject),
        crate::lease::Reclaim::Keep(_)
    ));
    drop(held);
    std::fs::write(fixture.executable(), b"a corrupted current installation").unwrap();
    // Being current is installer state, not permission to execute its bytes.
    assert!(session.install(fixture.want).unwrap());
    assert!(session.acquire_all(&[fixture.want]).is_err());
}

#[test]
fn provision_refuses_wrong_signed_recipe_before_changing_a_current_selection() {
    let mut fixture = Fixture::new(Prerequisite::Python);
    let before = std::fs::read(fixture.executable()).unwrap();
    let original = crate::lookup::read(Some(&fixture.layout), fixture.want.executable());
    fixture.change_package(
        &format!("version=\"{}\"", fixture_version(fixture.want)),
        "version=\"0.0.0\"",
    );
    assert!(
        fixture
            .session(TRIPLE)
            .unwrap()
            .install(fixture.want)
            .unwrap_err()
            .contains("shared recipe")
    );
    assert_eq!(std::fs::read(fixture.executable()).unwrap(), before);
    assert!(same_selection(
        &original,
        &crate::lookup::read(Some(&fixture.layout), fixture.want.executable())
    ));
}

#[test]
fn provision_freezes_one_signed_document_through_fresh_install_and_admission() {
    for wrong_first in [false, true] {
        let mut fixture = Fixture::new(Prerequisite::Gitleaks);
        fixture.fresh_install();
        let valid = fixture.package.clone();
        let wrong = valid.replace(
            &format!("version=\"{}\"", fixture_version(fixture.want)),
            "version=\"0.0.0\"",
        );
        assert_ne!(
            valid, wrong,
            "both documents carry the same program/build but different versions"
        );
        let (first, later) = if wrong_first {
            (wrong, valid)
        } else {
            (valid, wrong)
        };
        fixture.signature = testkit::sign(&testkit::MACHINE_SEED, first.as_bytes());
        fixture.package = first;
        fixture.after_first = Some((
            later.as_bytes().to_vec(),
            testkit::sign(&testkit::MACHINE_SEED, later.as_bytes()),
        ));
        let session = fixture.session(TRIPLE).unwrap();
        if wrong_first {
            assert!(
                session
                    .install(fixture.want)
                    .unwrap_err()
                    .contains("shared recipe")
            );
            assert!(!fixture.root.exists(), "refusal precedes installation");
            assert_eq!(fixture.downloads.get(), 0);
        } else {
            assert!(
                !session.install(fixture.want).unwrap(),
                "a real first installation"
            );
            let held = session.acquire_all(&[fixture.want]).unwrap();
            held.tool(fixture.want).unwrap().validate().unwrap();
            assert_eq!(
                crate::store::recorded_version(&fixture.root).as_deref(),
                Some(fixture_version(fixture.want))
            );
            assert_eq!(fixture.downloads.get(), 1);
        }
        assert_eq!(
            fixture.manifest_reads.get(),
            1,
            "the alternating signed document is never substituted"
        );
    }
}

#[test]
fn a_current_install_does_not_skip_snapshot_signature_parser_or_identity_checks() {
    let fixture = Fixture::new(Prerequisite::Gitleaks);
    crate::store::mark_build_ready(&fixture.root).unwrap();
    let session = fixture.session(TRIPLE).unwrap();
    let before = crate::lookup::read(Some(&fixture.layout), fixture.want.executable());
    let root = crate::tree::tree_root(&fixture.root).unwrap();
    for case in ["signature", "parser", "program", "build"] {
        let document = match case {
            "signature" => fixture.package.clone(),
            "parser" => "not a TOML document!".into(),
            "program" => fixture
                .package
                .replace("program=\"gitleaks\"", "program=\"foreign\""),
            "build" => fixture.package.replace("build_number=7", "build_number=8"),
            _ => unreachable!(),
        };
        let mut signature = testkit::sign(&testkit::MACHINE_SEED, document.as_bytes());
        if case == "signature" {
            signature[0] ^= 1;
        }
        let result = crate::flow::install_collecting_assets_with_snapshot(
            &fixture,
            &fixture.layout,
            &testkit::anchor(),
            &crate::InstallRequest {
                channel: "stable",
                program: "gitleaks",
                triple: TRIPLE,
                installed: Some(7),
            },
            BuildFloor::default(),
            testkit::NOW,
            Some(crate::flow::InstallSnapshot {
                index: session.index.clone(),
                package: Some((document.into_bytes(), signature)),
            }),
            &mut std::collections::BTreeMap::new(),
        );
        assert!(
            match case {
                "signature" => matches!(result, Err(crate::FlowError::PkgVerify)),
                "parser" => matches!(result, Err(crate::FlowError::PkgParse)),
                _ => matches!(result, Err(crate::FlowError::Mismatch)),
            },
            "{case}: {result:?}"
        );
        assert!(same_selection(
            &before,
            &crate::lookup::read(Some(&fixture.layout), fixture.want.executable())
        ));
        assert_eq!(crate::tree::tree_root(&fixture.root).unwrap(), root);
    }
    assert_eq!(
        fixture.manifest_reads.get(),
        0,
        "a provided snapshot never falls back to transport"
    );
    assert_eq!(fixture.downloads.get(), 0);
}

#[test]
fn authenticated_manifest_identity_and_local_bytes_are_both_required() {
    if !crate::testdir::isolated_lifecycle(
        "prerequisite::tests::authenticated_manifest_identity_and_local_bytes_are_both_required",
    ) {
        return;
    }
    for case in [
        "signature",
        "version",
        "program",
        "exposes",
        "target",
        "tree",
        "bytes",
        "alias",
        "environment",
    ] {
        let mut fixture = Fixture::new(Prerequisite::Python);
        match case {
            "signature" => fixture.signature[0] ^= 1,
            "version" => fixture.change_package(
                &format!("version=\"{}\"", fixture_version(Prerequisite::Python)),
                "version=\"0.0.0\"",
            ),
            "program" => fixture.change_package("program=\"cpython\"", "program=\"foreign\""),
            "exposes" => fixture.change_package("exposes=[\"python3.14\"]", "exposes=[\"lake\"]"),
            "target" => fixture.change_package(TRIPLE, "x86_64-apple-darwin"),
            "tree" => {
                let tree = crate::tree::tree_root(&fixture.root).unwrap();
                fixture.change_package(&tree, &"b".repeat(64));
            }
            "bytes" => {
                std::fs::write(fixture.executable(), b"changed after signed installation").unwrap()
            }
            "alias" => {
                std::fs::write(fixture.root.join("bin/other"), b"another packaged binary").unwrap();
                std::fs::set_permissions(
                    fixture.root.join("bin/other"),
                    std::fs::Permissions::from_mode(0o755),
                )
                .unwrap();
                let tree = crate::tree::tree_root(&fixture.root).unwrap();
                let old = fixture
                    .package
                    .split("tree_root=\"")
                    .nth(1)
                    .unwrap()
                    .split('"')
                    .next()
                    .unwrap()
                    .to_string();
                fixture.change_package(&old, &tree);
                let shim = fixture
                    .layout
                    .shim(&crate::store::ToolName::new("python3.14").unwrap());
                std::fs::write(
                    shim,
                    crate::platform::sh_shim_content(&fixture.root.join("bin/other")),
                )
                .unwrap();
            }
            "environment" => fixture.change_package(
                "exposes=[",
                "shim_env=[\"DISABLE_AUTOUPDATER=1\"]\nexposes=[",
            ),
            _ => unreachable!(),
        }
        assert!(
            fixture
                .session(TRIPLE)
                .unwrap()
                .acquire(Prerequisite::Python)
                .is_err(),
            "{case}"
        );
        let subject = Subject::build("cpython", 7).unwrap();
        assert!(
            matches!(
                crate::lease::reclaim(&fixture.layout.prefix, &subject),
                crate::lease::Reclaim::Clear(_)
            ),
            "failed {case} leaked a lease"
        );
    }
}

#[test]
fn unsupported_hosts_stale_indexes_and_withdrawn_pins_are_explicit_refusals() {
    for want in [
        Prerequisite::Gitleaks,
        Prerequisite::Python,
        Prerequisite::OpenSslSigning,
        Prerequisite::Lake,
    ] {
        assert!(want.supports("aarch64-apple-darwin"));
        assert!(want.supports("x86_64-apple-darwin"));
        for target in ["aarch64-unknown-linux-gnu", "x86_64-unknown-linux-gnu"] {
            assert_eq!(want.supports(target), want == Prerequisite::Gitleaks);
        }
        for target in [
            "aarch64-pc-windows-msvc",
            "x86_64-pc-windows-msvc",
            "x86_64-unknown-linux-musl",
            "unknown",
        ] {
            assert!(!want.supports(target), "{} on {target}", want.executable());
        }
    }
    let fixture = Fixture::new(Prerequisite::Python);
    assert!(
        fixture
            .session("aarch64-unknown-linux-gnu")
            .unwrap()
            .acquire(Prerequisite::Python)
            .unwrap_err()
            .contains("unavailable")
    );
    let mut stale = Fixture::new(Prerequisite::Gitleaks);
    stale.index = stale
        .index
        .replace("2099-01-01T00:00:00Z", "2000-01-01T00:00:00Z");
    assert!(matches!(
        stale.session(TRIPLE),
        Err(crate::FlowError::Stale)
    ));
    let mut yanked = Fixture::new(Prerequisite::Gitleaks);
    yanked.index.push_str("yanked=[\"gitleaks@7\"]\n");
    assert!(
        yanked
            .session(TRIPLE)
            .unwrap()
            .acquire(Prerequisite::Gitleaks)
            .unwrap_err()
            .contains("withdrawn")
    );
}

#[test]
fn a_changed_shim_is_refused_after_lease_acquisition_and_drops_the_hold() {
    if !crate::testdir::isolated_lifecycle(
        "prerequisite::tests::a_changed_shim_is_refused_after_lease_acquisition_and_drops_the_hold",
    ) {
        return;
    }
    let fixture = Fixture::new(Prerequisite::Python);
    let selected = crate::lookup::read(Some(&fixture.layout), "python3.14");
    admit_selection(&selected, Prerequisite::Python, 7).unwrap();
    let shim = fixture
        .layout
        .shim(&crate::store::ToolName::new("python3.14").unwrap());
    std::fs::write(
        &shim,
        crate::platform::sh_shim_content(Path::new("/unmanaged/python3.14")),
    )
    .unwrap();
    assert!(
        hold_selection(
            &fixture.layout,
            Prerequisite::Python,
            7,
            &selected,
            "fixture lease race",
            std::time::Duration::ZERO,
        )
        .is_err()
    );
    assert!(matches!(
        crate::lease::reclaim(
            &fixture.layout.prefix,
            &Subject::build("cpython", 7).unwrap()
        ),
        crate::lease::Reclaim::Clear(_)
    ));
}

#[test]
fn changed_images_and_lost_lease_custody_refuse_before_a_command_is_built() {
    for replace_slot in [false, true] {
        let fixture = Fixture::new(Prerequisite::Python);
        let session = fixture.session(TRIPLE).unwrap();
        let tool = session.acquire(Prerequisite::Python).unwrap();
        tool.validate().unwrap();
        let slot = tool.lease.path().to_path_buf();
        std::fs::remove_file(&slot).unwrap();
        if replace_slot {
            std::fs::write(&slot, b"replacement slot, not held\n").unwrap();
        }
        assert!(tool.command().err().unwrap().contains("lease lost custody"));
        assert!(
            fixture.executable().exists(),
            "the signed image remains present"
        );
    }
    for replace in [false, true] {
        let fixture = Fixture::new(Prerequisite::OpenSslSigning);
        let session = fixture.session(TRIPLE).unwrap();
        let tool = session.acquire(Prerequisite::OpenSslSigning).unwrap();
        tool.validate().unwrap();
        if replace {
            let replacement = fixture.root.join("replacement");
            std::fs::copy(fixture.executable(), &replacement).unwrap();
            std::fs::rename(replacement, fixture.executable()).unwrap();
        } else {
            std::fs::write(
                fixture.executable(),
                b"edited after admission, before execution",
            )
            .unwrap();
        }
        assert!(tool.command().is_err());
    }
}

#[test]
fn private_namespace_is_fixed_to_held_commands_without_exposing_lean_or_global_shim_flips() {
    let fixture = Fixture::new(Prerequisite::Lake);
    let session = fixture.session(TRIPLE).unwrap();
    let set = session
        .acquire_all(&[Prerequisite::Lake, Prerequisite::Lake])
        .unwrap();
    let entries: Vec<_> = std::fs::read_dir(set.directory.path())
        .unwrap()
        .map(|e| e.unwrap().file_name())
        .collect();
    assert_eq!(entries, [std::ffi::OsString::from("lake")]);
    assert_eq!(
        crate::platform::resolve_shim(&set.directory.path().join("lake")),
        Some(fixture.executable())
    );
    let global = fixture
        .layout
        .shim(&crate::store::ToolName::new("lake").unwrap());
    std::fs::write(
        &global,
        crate::platform::sh_shim_content(Path::new("/unmanaged/lake")),
    )
    .unwrap();
    let mut command = aterm_uds::spawnfd::command("/bin/sh");
    set.apply(&mut command, std::ffi::OsStr::new("/usr/bin:/bin"))
        .unwrap();
    assert_eq!(
        crate::platform::resolve_shim(&set.directory.path().join("lake")),
        Some(fixture.executable())
    );
    assert!(set.tool(Prerequisite::Lake).is_some());
    assert_eq!(fixture.index_reads.get(), 1);
    let python = Fixture::new(Prerequisite::Python);
    let held = python
        .session(TRIPLE)
        .unwrap()
        .acquire_all(&[Prerequisite::Python])
        .unwrap();
    for name in ["python3", "python3.14"] {
        assert_eq!(
            crate::platform::resolve_shim(&held.directory.path().join(name)),
            Some(python.executable())
        );
    }
    assert!(!held.directory.path().join("lean").exists());
    let initial = held
        .child_path(std::ffi::OsStr::new("/usr/bin:/bin"))
        .unwrap();
    assert_eq!(
        held.child_path(&initial).unwrap(),
        initial,
        "reapplying an operation namespace is idempotent"
    );
}
