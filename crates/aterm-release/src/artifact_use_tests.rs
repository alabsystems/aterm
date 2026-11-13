// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

fn remove_with(
    dir: &Path,
    read: impl FnMut() -> Result<Vec<Reference>, String>,
) -> Result<bool, String> {
    remove_observed(dir, Instant::now() + aterm_spec::wait::MUST_HAPPEN, read)
}

fn artifact(root: &Path, name: &str) -> PathBuf {
    let dir = root.join(name);
    std::fs::create_dir_all(dir.join("aterm.app/Contents/MacOS")).unwrap();
    std::fs::write(dir.join("aterm.app/Contents/MacOS/aterm"), "image").unwrap();
    dir
}

fn reference(program: PathBuf) -> Reference {
    Reference {
        program,
        source: "test service".into(),
    }
}

#[test]
fn live_and_configured_artifacts_stay_and_unused_artifacts_go() {
    let root = aterm_tempfile::tempdir().unwrap();
    let live = artifact(root.path(), "live");
    let configured = artifact(root.path(), "configured");
    let unused = artifact(root.path(), "unused");
    let refs = vec![
        reference(live.join("aterm.app/Contents/MacOS/aterm")),
        reference(configured.join("missing-image")),
    ];
    assert!(remove_with(&live, || Ok(refs.clone())).is_err());
    assert!(remove_with(&configured, || Ok(refs.clone())).is_err());
    let removed = remove_with(&unused, || Ok(refs.clone()));
    #[cfg(unix)]
    {
        assert!(removed.unwrap());
        assert!(!unused.exists());
    }
    #[cfg(not(unix))]
    {
        assert!(removed.is_err(), "directory identity is unavailable");
        assert!(unused.exists());
    }
    assert!(live.exists() && configured.exists());
}

#[test]
fn incomplete_or_late_observation_never_removes_an_artifact() {
    let root = aterm_tempfile::tempdir().unwrap();
    let dir = artifact(root.path(), "entry");
    assert!(remove_with(&dir, || Err("unreadable process or service".into())).is_err());
    let mut count = 0;
    assert!(
        remove_with(&dir, || {
            count += 1;
            Ok(if count == 1 {
                vec![]
            } else {
                vec![reference(dir.join("aterm.app/Contents/MacOS/aterm"))]
            })
        })
        .is_err()
    );
    assert_eq!(count, 2);
    assert!(dir.exists());
}

#[test]
#[cfg(unix)]
fn a_complete_observation_after_a_scheduling_delay_can_remove_unused_bytes() {
    let root = aterm_tempfile::tempdir().unwrap();
    for (elapsed, completes) in [(6, true), (61, false)] {
        let dir = artifact(root.path(), &format!("entry-{elapsed}"));
        let started = Instant::now() - Duration::from_secs(elapsed);
        let mut reads = 0;
        let result = remove_observed(&dir, started + OBSERVATION_HANG, || {
            reads += 1;
            Ok(vec![])
        });
        if completes {
            assert!(result.unwrap());
            assert_eq!(reads, 2);
            assert!(!dir.exists());
        } else {
            assert!(result.is_err());
            assert_eq!(reads, 0);
            assert!(dir.exists());
        }
    }
}

#[test]
fn an_expired_observation_removes_nothing() {
    let root = aterm_tempfile::tempdir().unwrap();
    let dir = artifact(root.path(), "entry");
    assert!(remove_observed(&dir, Instant::now(), || panic!("expired before reading")).is_err());
    assert!(dir.exists());
}

#[test]
fn a_replaced_directory_or_new_first_execution_is_never_removed() {
    let root = aterm_tempfile::tempdir().unwrap();
    let dir = artifact(root.path(), "entry");
    let moved = root.path().join("old-entry");
    let mut count = 0;
    assert!(
        remove_with(&dir, || {
            count += 1;
            if count == 2 {
                std::fs::rename(&dir, &moved).unwrap();
                artifact(root.path(), "entry");
            }
            Ok(vec![])
        })
        .is_err()
    );
    assert!(dir.exists() && moved.exists());
    let mut count = 0;
    assert!(
        remove_with(&dir, || {
            count += 1;
            if count == 2 {
                aterm_uds::deadline::stamp_exec(&dir);
            }
            Ok(vec![])
        })
        .is_err()
    );
    assert!(dir.exists());
}

#[test]
fn first_execution_is_retained_and_sibling_prefix_is_not_a_reference() {
    let root = aterm_tempfile::tempdir().unwrap();
    let held = artifact(root.path(), "entry");
    aterm_uds::deadline::stamp_exec(&held);
    assert!(remove_with(&held, || Ok(vec![])).is_err());
    let sibling = artifact(root.path(), "entry-other");
    assert!(!points_into(&sibling.join("aterm.app/Contents/MacOS/aterm"), &held).unwrap());
    assert!(held.exists());
}

#[cfg(target_os = "macos")]
#[test]
fn symlink_and_real_service_unit_preserve_the_target() {
    let root = aterm_tempfile::tempdir().unwrap();
    let dir = artifact(root.path(), "entry");
    let alias = root.path().join("alias");
    std::os::unix::fs::symlink(dir.join("aterm.app/Contents/MacOS/aterm"), &alias).unwrap();
    assert!(points_into(&alias, &dir).unwrap());
    let parent_alias = root.path().join("alias-directory");
    std::os::unix::fs::symlink(&dir, &parent_alias).unwrap();
    assert!(points_into(&parent_alias.join("missing-image"), &dir).unwrap());
    let services = root.path().join("LaunchAgents");
    std::fs::create_dir(&services).unwrap();
    let text = format!(
        "<plist><dict><key>ProgramArguments</key><array><string>{}</string><string>node</string><string>serve</string></array></dict></plist>",
        alias.display()
    );
    std::fs::write(services.join("systems.alab.aterm-node.plist"), text).unwrap();
    assert!(remove_with(&dir, || configured(std::slice::from_ref(&services))).is_err());
    std::fs::write(services.join("systems.alab.aterm-node.plist"), "broken").unwrap();
    assert!(configured(&[services]).is_err());
    assert!(dir.exists());
}

#[cfg(target_os = "macos")]
#[test]
fn an_inactive_broker_keeps_its_configured_artifact() {
    let root = aterm_tempfile::tempdir().unwrap();
    let dir = artifact(root.path(), "entry");
    let services = root.path().join("LaunchAgents");
    std::fs::create_dir(&services).unwrap();
    let program = dir.join("aterm.app/Contents/MacOS/aterm");
    let unit = services.join("systems.alab.astream-broker.plist");
    let text = format!(
        "<plist><dict><key>ProgramArguments</key><array><string>{}</string><string>broker</string></array></dict></plist>",
        program.display()
    );
    std::fs::write(&unit, &text).unwrap();
    let references = configured(&[services]).unwrap();
    assert_eq!(references.len(), 1);
    assert_eq!(references[0].program, program);
    assert!(remove_with(&dir, || Ok(references.clone())).is_err());
    assert!(dir.exists());
    assert_eq!(std::fs::read_to_string(unit).unwrap(), text);
}

#[cfg(target_os = "macos")]
#[test]
fn an_explicit_configured_program_retains_its_artifact_when_argv0_differs() {
    let root = aterm_tempfile::tempdir().unwrap();
    let unused = artifact(root.path(), "unused");
    assert!(remove_with(&unused, || Ok(vec![])).unwrap());
    assert!(!unused.exists());
    let services = root.path().join("LaunchAgents");
    std::fs::create_dir(&services).unwrap();
    let unit = services.join("systems.alab.aterm-node.plist");
    let ordinary = artifact(root.path(), "ordinary");
    let ordinary_program = ordinary.join("aterm.app/Contents/MacOS/aterm");
    let xml = |program: Option<&Path>, argv0: &Path| {
        format!(
            "<plist><dict>{}<key>ProgramArguments</key><array><string>{}</string></array></dict></plist>",
            program.map_or_else(String::new, |program| format!(
                "<key>Program</key><string>{}</string>",
                aterm_codec::xml::escape(&program.display().to_string())
            )),
            aterm_codec::xml::escape(&argv0.display().to_string())
        )
    };
    std::fs::write(&unit, xml(None, &ordinary_program)).unwrap();
    assert!(remove_with(&ordinary, || configured(std::slice::from_ref(&services))).is_err());
    assert!(
        ordinary.exists(),
        "ordinary ProgramArguments retains its artifact"
    );

    let dir = artifact(root.path(), "explicit-program");
    let program = dir.join("aterm.app/Contents/MacOS/aterm");
    let text = xml(Some(&program), Path::new("/usr/bin/true"));
    std::fs::write(&unit, &text).unwrap();
    let removed = remove_with(&dir, || configured(std::slice::from_ref(&services)));
    assert!(
        dir.exists(),
        "Program selects the actual executable: {removed:?}"
    );
    assert!(removed.is_err());
    assert_eq!(std::fs::read_to_string(unit).unwrap(), text);
}

#[cfg(unix)]
#[test]
fn malformed_exec_evidence_never_becomes_an_expired_first_execution() {
    let root = aterm_tempfile::tempdir().unwrap();
    let unused = artifact(root.path(), "unused");
    assert!(remove_with(&unused, || Ok(vec![])).unwrap());
    assert!(!unused.exists());
    for (label, bytes) in [
        ("blank", b"".as_slice()),
        ("invalid", b"not-a-clock".as_slice()),
    ] {
        let dir = artifact(root.path(), label);
        let stamp = dir.join(aterm_uds::deadline::EXEC_STAMP);
        std::fs::write(&stamp, bytes).unwrap();
        // A retry may overwrite an existing marker without updating its old parent mtime.
        // Do not let malformed current evidence borrow that old age and authorize removal.
        std::fs::File::open(&dir)
            .unwrap()
            .set_times(std::fs::FileTimes::new().set_modified(std::time::SystemTime::UNIX_EPOCH))
            .unwrap();
        let removed = remove_with(&dir, || Ok(vec![]));
        assert!(
            dir.exists(),
            "unreadable exec evidence must retain {label}: {removed:?}"
        );
        assert!(removed.is_err());
        assert_eq!(std::fs::read(stamp).unwrap(), bytes);
    }
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
#[test]
fn a_writerless_exec_fifo_retains_the_artifact_without_blocking() {
    use std::os::unix::ffi::OsStrExt as _;
    #[cfg(target_os = "macos")]
    type Mode = u16;
    #[cfg(target_os = "linux")]
    type Mode = u32;
    unsafe extern "C" {
        fn mkfifo(path: *const std::ffi::c_char, mode: Mode) -> std::ffi::c_int;
    }
    let root = aterm_tempfile::tempdir().unwrap();
    let dir = artifact(root.path(), "fifo-marker");
    let path = std::ffi::CString::new(
        dir.join(aterm_uds::deadline::EXEC_STAMP)
            .as_os_str()
            .as_bytes(),
    )
    .unwrap();
    // SAFETY: path is a live NUL-terminated owned scratch name; the cfg-specific Mode
    // matches this platform's mode_t, and mkfifo keeps no pointer after the call.
    assert_eq!(unsafe { mkfifo(path.as_ptr(), 0o600) }, 0);
    let observed = dir.clone();
    let (send, received) = std::sync::mpsc::channel();
    let worker = std::thread::spawn(move || {
        let _ = send.send(remove_with(&observed, || Ok(vec![])));
    });
    // A regression must fail at the hang detector rather than blocking the test runner's
    // join. Only inert scratch is involved; this test is never run on the OLD reader.
    let result = received
        .recv_timeout(aterm_spec::wait::MUST_HAPPEN)
        .expect("nonblocking marker observation within the one-minute hang detector");
    worker.join().unwrap();
    assert!(result.is_err());
    assert!(dir.exists());
}

#[cfg(unix)]
#[test]
fn path_bytes_and_data_volume_aliases_remain_references() {
    use std::os::unix::ffi::OsStringExt as _;
    let root = aterm_tempfile::tempdir().unwrap();
    let dir = root
        .path()
        .join(std::ffi::OsString::from_vec(b"entry-\xff".to_vec()));
    // APFS rejects such names; lexical retention also covers a missing image,
    // so this byte-level check needs no filesystem entry.
    assert!(points_into(&dir.join("missing-image"), &dir).unwrap());
    assert!(descendant(
        Path::new("/System/Volumes/Data/Users//a/cache/entry/aterm"),
        Path::new("/Users//a/cache/entry")
    ));
    assert!(!descendant(
        Path::new("/System/Volumes/Data/Users//a/cache/entry-other/aterm"),
        Path::new("/Users//a/cache/entry")
    ));
}

#[test]
fn loaded_jobs_are_selected_without_reading_unrelated_environment() {
    assert_eq!(labels("services = {\n 7 0 systems.alab.aterm-node\n 8 0 another.job\n 9 0 systems.alab.aterm-secrets\n 0 0 systems.alab.astream-broker.n-1\n 0 0 com.aterm.aterm.acaffeine\n 0 0 systems.alab.aterm-auto-alab\n}").unwrap(), vec!["com.aterm.aterm.acaffeine", "systems.alab.astream-broker.n-1", "systems.alab.aterm-auto-alab", "systems.alab.aterm-node", "systems.alab.aterm-secrets"]);
    assert!(labels("unknown output").is_err());
    assert!(labels("services = {\n 7 0 systems.alab.aterm-node").is_err());
    assert!(labels("services = {\n unexpected\n}").is_err());
    assert_eq!(loaded_program("job = {\n\tprogram = /Applications/aterm.app/Contents/MacOS/aterm\n\tenvironment = {\n\t\tSAFE = value\n\t}\n}").unwrap(), Path::new("/Applications/aterm.app/Contents/MacOS/aterm"));
    assert!(loaded_program("state = waiting").is_err());
    assert!(loaded_program("\tprogram = /one\n\tprogram = /two").is_err());
    assert!(loaded_program("job = {\n\tenvironment = {\n\t\tprogram = /injected\n\t}\n}").is_err());
}

#[test]
#[cfg(target_os = "macos")]
#[ignore = "read-only smoke against this Mac's configured and loaded managed services"]
fn this_macs_managed_service_references() {
    let references = service_references(Instant::now() + aterm_spec::wait::MUST_HAPPEN).unwrap();
    assert!(
        !references.is_empty(),
        "this smoke requires existing managed services"
    );
    for reference in references {
        assert!(reference.program.is_absolute());
        println!("{}: {}", reference.source, reference.program.display());
    }
}
