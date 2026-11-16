// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::super::{Profile, identity};
use super::*;
use crate::artifact::tests::{bundle, executable};
use crate::{Ctx, EnvSnapshot, MeasureArtifact, Scope};

fn context(root: &Path) -> Ctx {
    Ctx::new(
        root.to_owned(),
        Scope::workspace(),
        EnvSnapshot::default(),
        root.join("scratch"),
    )
}

#[test]
fn replacing_the_admitted_source_cannot_change_the_private_image_or_resources() {
    use std::os::unix::fs::symlink;

    let root = aterm_tempfile::tempdir().unwrap();
    let app = bundle(root.path(), &executable(true));
    let resources = app.join("Contents/Resources");
    std::fs::create_dir(&resources).unwrap();
    std::fs::write(resources.join("payload"), "admitted resources").unwrap();
    symlink("payload", resources.join("linked")).unwrap();
    let admitted = MeasureArtifact::for_profile(&app, Profile::Debug).unwrap();
    admitted.admit(&context(root.path())).unwrap();
    let source = admitted.executable.as_ref().unwrap();
    let snapshot = Snapshot::prepare(&app, source).unwrap();

    std::fs::rename(&app, root.path().join("original.app")).unwrap();
    bundle(root.path(), &executable(false));
    let copied =
        Executable::open(&snapshot.app().join("Contents/MacOS/aterm"), Profile::Debug).unwrap();
    assert_eq!(copied.digest, source.digest);
    assert_eq!(copied.facts, source.facts);
    assert_ne!(
        copied.identity, source.identity,
        "the image is an independent copy"
    );
    assert_eq!(
        std::fs::read_to_string(snapshot.app().join("Contents/Resources/linked")).unwrap(),
        "admitted resources"
    );
    let canonical = std::fs::canonicalize(snapshot.app().join("Contents/MacOS/aterm")).unwrap();
    for name in ["aterm-gui", "aterm-ctl"] {
        assert_eq!(
            std::fs::canonicalize(snapshot.app().join("Contents/MacOS").join(name)).unwrap(),
            canonical
        );
    }
    let directory = snapshot.directory.as_ref().unwrap().path().to_owned();
    drop(snapshot);
    assert!(
        !directory.exists(),
        "an image never attempted can be removed"
    );
}

#[test]
fn copying_the_held_image_does_not_move_its_shared_read_cursor() {
    use std::io::{Seek as _, SeekFrom};

    let root = aterm_tempfile::tempdir().unwrap();
    let mut bytes = executable(true);
    bytes.resize(2 << 20, 7);
    let app = bundle(root.path(), &bytes);
    let admitted = MeasureArtifact::for_profile(&app, Profile::Debug).unwrap();
    let source = admitted.executable.as_ref().unwrap();
    let mut shared = source.held.try_clone().unwrap();
    shared.seek(SeekFrom::Start(123)).unwrap();
    let snapshot = Snapshot::prepare(&app, source).unwrap();
    assert_eq!(shared.stream_position().unwrap(), 123);
    assert_eq!(
        std::fs::read(snapshot.app().join("Contents/MacOS/aterm")).unwrap(),
        bytes
    );
    assert_eq!(identity(&source.held).unwrap(), source.identity);
}

#[test]
fn resources_cannot_reach_out_of_the_private_bundle() {
    use std::os::unix::fs::symlink;

    for target in [Path::new("/tmp"), Path::new("../../../outside")] {
        let root = aterm_tempfile::tempdir().unwrap();
        let app = bundle(root.path(), &executable(true));
        let resources = app.join("Contents/Resources");
        std::fs::create_dir(&resources).unwrap();
        symlink(target, resources.join("external")).unwrap();
        let admitted = MeasureArtifact::for_profile(&app, Profile::Debug).unwrap();
        assert!(
            Snapshot::prepare(&app, admitted.executable.as_ref().unwrap()).is_err(),
            "an external bundle link cannot be carried into a private launch"
        );
    }
}

#[test]
fn composed_resource_links_cannot_redirect_the_private_image_write() {
    use std::os::unix::fs::symlink;

    let root = aterm_tempfile::tempdir().unwrap();
    let app = bundle(&root.path().join("source"), &executable(true));
    let admitted = MeasureArtifact::for_profile(&app, Profile::Debug).unwrap();
    admitted.admit(&context(root.path())).unwrap();
    std::fs::rename(app.join("Contents"), root.path().join("old-contents")).unwrap();
    std::fs::create_dir(app.join("d")).unwrap();
    symlink("..", app.join("d/up")).unwrap();
    symlink("d/up/d/up/../../victim", app.join("Contents")).unwrap();
    let victim = root.path().join("victim/MacOS");
    std::fs::create_dir_all(&victim).unwrap();
    let namespace = aterm_tempfile::Builder::new()
        .prefix("private-")
        .suffix(".noindex")
        .tempdir_in(root.path())
        .unwrap();
    assert!(Snapshot::prepare_in(&app, admitted.executable.as_ref().unwrap(), namespace).is_err());
    assert!(
        !victim.join("aterm").exists(),
        "rejecting a link after writing the image would already have left the private namespace"
    );
}

#[test]
fn a_callers_temporary_root_cannot_remove_a_retained_snapshot() {
    use std::os::unix::ffi::{OsStrExt as _, OsStringExt as _};
    const RECORD: &str = "ATERM_SNAPSHOT_NAMESPACE_RECORD";

    if let Some(record) = std::env::var_os(RECORD) {
        let source_root = aterm_tempfile::tempdir().unwrap();
        let app = bundle(source_root.path(), &executable(true));
        let admitted = MeasureArtifact::for_profile(&app, Profile::Debug).unwrap();
        let mut snapshot = Snapshot::prepare(&app, admitted.executable.as_ref().unwrap()).unwrap();
        let directory = snapshot.directory.as_ref().unwrap().path().to_owned();
        // Model a retained uncertain attempt without executing the synthetic Mach-O data.
        // The parent can therefore remove this fixture after checking its ancestor custody.
        snapshot.attempted = true;
        drop(snapshot);
        std::fs::write(record, directory.as_os_str().as_bytes()).unwrap();
        return;
    }

    let caller = aterm_tempfile::tempdir().unwrap();
    let report = aterm_tempfile::tempdir().unwrap();
    let record = report.path().join("namespace");
    let thread = std::thread::current();
    let name = thread.name().expect("libtest names its case thread");
    let mut command = aterm_uds::spawnfd::command(std::env::current_exe().unwrap());
    command
        .args(["--exact", name, "--nocapture"])
        .env(RECORD, &record)
        .env("TMPDIR", caller.path());
    let output =
        aterm_uds::deadline::output_within(&mut command, std::time::Duration::from_secs(60))
            .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let directory = PathBuf::from(std::ffi::OsString::from_vec(std::fs::read(record).unwrap()));
    drop(caller);
    assert!(
        directory.join("aterm.app/Contents/MacOS/aterm").is_file(),
        "an enclosing TMPDIR guard must not remove a retained executable"
    );
    remove_unexecuted_data(&directory);
}

fn remove_unexecuted_data(directory: &Path) {
    use std::os::unix::fs::PermissionsExt as _;

    std::fs::set_permissions(directory, std::fs::Permissions::from_mode(0o700)).unwrap();
    for entry in std::fs::read_dir(directory).unwrap() {
        let entry = entry.unwrap();
        if entry.file_type().unwrap().is_dir() {
            remove_unexecuted_data(&entry.path());
        }
    }
    std::fs::remove_dir_all(directory).unwrap();
}

#[cfg(target_os = "macos")]
#[test]
fn the_proved_consumer_executes_the_private_image_after_original_bundle_replacement() {
    use std::time::Duration;

    // This package owns the typed development binary, so the normal test compile builds it
    // and the verifier supplies Cargo's recorded executable mapping to its test children.
    let fixture = aterm_tempfile::bin_exe("aterm-verify-artifact-fixture");
    let bytes = std::fs::read(fixture).unwrap();
    let root = aterm_tempfile::tempdir().unwrap().keep();
    let app = bundle(&root, &bytes);
    let admitted = MeasureArtifact::for_profile(&app, Profile::Debug).unwrap();
    let ctx = context(&root);
    // The real executor captures a child into the run's already-created scratch directory.
    std::fs::create_dir(&ctx.scratch).unwrap();
    let result = admitted.with_admission(&ctx, |private| {
        // A failed negative may really launch the public spelling. Until that child writes its
        // identity, the original bundle and its directory must survive any assertion or error.
        let mut originals = aterm_uds::deadline::KeptUntilRan::new(&root);
        std::fs::rename(&app, root.join("old.app")).unwrap();
        bundle(&root, &bytes);
        let binary = private.bin("aterm");
        let mut command = aterm_uds::spawnfd::command(&binary);
        command.arg("__snapshot_identity");
        let output =
            aterm_uds::deadline::output_within(&mut command, Duration::from_secs(60)).unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let ran = PathBuf::from(std::str::from_utf8(&output.stdout).unwrap().trim_end());
        assert!(!ran.as_os_str().is_empty());
        originals.ran();
        assert_eq!(ran, std::fs::canonicalize(&binary).unwrap());
        assert_ne!(
            ran,
            std::fs::canonicalize(app.join("Contents/MacOS/aterm")).unwrap(),
            "a delayed consumer must execute the private admitted image"
        );
        // Entering this callback already required each private spelling to return its exact
        // immutable --build-facts record and exit successfully. The running identity above
        // names the same independently copied image even after the public bundle replacement.
        let copied = Executable::open(&binary, Profile::Debug).unwrap();
        assert_eq!(copied.facts, admitted.executable.as_ref().unwrap().facts);
        assert!(
            aterm_stamp::facts::decode(&copied.facts)
                .unwrap()
                .debug_assertions
        );
    });
    assert!(
        result.is_ok(),
        "{result:?}; original fixture retained at {}",
        root.display()
    );
}
