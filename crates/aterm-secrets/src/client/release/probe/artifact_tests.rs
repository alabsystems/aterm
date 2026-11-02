// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::client::release::probe::model::{Action, Repository, Subject};

#[test]
fn certificate_prefix_is_one_option_and_keeps_native_path_bytes() {
    use std::os::unix::ffi::{OsStrExt as _, OsStringExt as _};
    let prefix = PathBuf::from(std::ffi::OsString::from_vec(
        b"/private/scratch/cert-\xff".to_vec(),
    ));
    let target = Path::new("/private/scratch/aterm verification.app");
    let command = certificate_command(&prefix, target);
    let args: Vec<_> = command.get_args().collect();
    assert_eq!(
        args.len(),
        3,
        "the optional prefix must not become a code target"
    );
    assert_eq!(args[0], "--display");
    assert_eq!(
        args[1].as_bytes(),
        b"--extract-certificates=/private/scratch/cert-\xff"
    );
    assert_eq!(args[2], target.as_os_str());
    assert_eq!(
        super::super::apple::operation(&command),
        "step=certificate program=codesign"
    );
}

fn journal() -> Journal {
    Journal {
        nonce: "12".repeat(16),
        subject: Subject {
            uid: crate::fsx::euid(),
            node: "34".repeat(16),
            member: "56".repeat(16),
            writer: "bc".repeat(32),
            incarnation: "78".repeat(16),
            network: "9a".repeat(16),
            authority: "cd".repeat(32),
            genesis: "de".repeat(32),
            vault: "ef".repeat(32),
        },
        repositories: vec![Repository::new("fixture-owner".into())],
        created_ms: 1,
        updated_ms: 1,
        artifact: None,
        submission: None,
        apple_accepted: false,
        pending: Some(Action::Sign),
        cleanup_only: true,
    }
}

fn bundle(dir: &Path, journal: &Journal, bytes: &[u8]) {
    for path in [
        app(dir),
        app(dir).join("Contents"),
        app(dir).join("Contents/MacOS"),
    ] {
        private_dir(&path).unwrap();
    }
    crate::fsx::write_new(
        &app(dir).join("Contents/Info.plist"),
        plist(&journal.nonce).as_bytes(),
        0o600,
    )
    .unwrap();
    crate::fsx::write_new(&executable(dir), bytes, 0o700).unwrap();
}

#[test]
fn unsigned_cleanup_keeps_the_original_inode_and_refuses_other_effects() {
    let scratch = aterm_tempfile::tempdir().unwrap();
    let j = journal();
    bundle(scratch.path(), &j, &source_bytes().unwrap());
    let identity = || {
        let meta = std::fs::symlink_metadata(executable(scratch.path())).unwrap();
        (meta.dev(), meta.ino(), meta.mode(), meta.len())
    };
    let before = identity();
    assert!(unsigned_retained(scratch.path(), &j).unwrap());
    assert_eq!(identity(), before);
    let mut other = j.clone();
    other.cleanup_only = false;
    assert!(!unsigned_retained(scratch.path(), &other).unwrap());
    other = j.clone();
    other.pending = Some(Action::Submit);
    assert!(!unsigned_retained(scratch.path(), &other).unwrap());
    crate::fsx::write_new(
        &scratch.path().join("archive.intent"),
        b"an intent exists",
        0o600,
    )
    .unwrap();
    assert!(!unsigned_retained(scratch.path(), &j).unwrap());
    assert_eq!(identity(), before);
    assert_eq!(
        read(&executable(scratch.path())).unwrap(),
        source_bytes().unwrap()
    );
}

#[test]
fn unsigned_cleanup_rejects_changed_bytes_links_and_metadata() {
    let scratch = aterm_tempfile::tempdir().unwrap();
    let j = journal();
    bundle(scratch.path(), &j, b"changed payload");
    assert!(!unsigned_retained(scratch.path(), &j).unwrap());
    crate::fsx::write_atomic(&executable(scratch.path()), &source_bytes().unwrap(), 0o700).unwrap();
    std::fs::hard_link(executable(scratch.path()), scratch.path().join("linked")).unwrap();
    assert!(unsigned_retained(scratch.path(), &j).is_err());
}

#[cfg(target_os = "macos")]
#[test]
fn independent_ad_hoc_preparations_match_and_the_final_path_executes() {
    struct Local;
    impl Tool for Local {
        fn run(
            &self,
            command: &mut aterm_uds::spawnfd::Command,
            deadline: Instant,
        ) -> std::result::Result<std::process::Output, super::super::apple::ToolError> {
            let started = std::cell::Cell::new(false);
            aterm_uds::deadline::output_until_complete_stdout_observed(command, deadline, || {
                started.set(true)
            })
            .map_err(|error| {
                super::super::apple::ToolError::observed(command, &error, started.get())
            })
        }
    }
    let deadline = Instant::now() + aterm_spec::wait::MUST_HAPPEN;
    let a = prepared_bytes(&Local, deadline).unwrap();
    let b = prepared_bytes(&Local, deadline).unwrap();
    assert_eq!(a, b, "scratch paths must not alter the prepared payload");
    assert_ne!(a, source_bytes().unwrap());
    let scratch = aterm_tempfile::tempdir().unwrap().keep();
    let mut kept = aterm_uds::deadline::KeptUntilRan::new(scratch);
    let j = journal();
    bundle(kept.path(), &j, &a);
    execute(&Local, kept.path(), &j.nonce, deadline).unwrap();
    kept.ran();
}
