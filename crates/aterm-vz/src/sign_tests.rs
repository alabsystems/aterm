// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

#[test]
fn the_signing_argv_is_ad_hoc_with_the_helpers_entitlements_only() {
    let args = codesign_args(
        Path::new("/h/x/aterm-vz-host.entitlements"),
        Path::new("/h/x/aterm-vz-host"),
    );
    let args: Vec<_> = args.iter().map(|a| a.to_str().unwrap()).collect();
    assert_eq!(
        args,
        [
            "--force",
            "--sign",
            "-",
            "--entitlements",
            "/h/x/aterm-vz-host.entitlements",
            "/h/x/aterm-vz-host"
        ]
    );
    assert_eq!(aterm_uds::os_path!("codesign"), "/usr/bin/codesign");
    // The plist names the virtualization entitlement and nothing else.
    let keys: Vec<_> = ENTITLEMENTS
        .lines()
        .filter_map(|l| l.trim().strip_prefix("<key>"))
        .collect();
    assert_eq!(keys, ["com.apple.security.virtualization</key>"]);
    assert!(ENTITLEMENTS.contains("<true/>"));
    // codesign's own XML reader (AMFIUnserializeXML) refuses a comment holding `--`,
    // which plutil passes (measured: "syntax error near line 8").
    let comment = ENTITLEMENTS
        .split_once("<!--")
        .and_then(|(_, rest)| rest.split_once("-->"))
        .map(|(inside, _)| inside)
        .expect("the plist's header comment");
    assert!(!comment.contains("--"), "a double hyphen in {comment:?}");
}

fn sha(bytes: &[u8]) -> String {
    let mut h = aterm_digest::Sha256::new();
    h.update(bytes);
    crate::hex(&h.finalize())
}

#[test]
fn the_copy_is_signed_in_its_partial_directory_then_placed_under_the_builds_hash() {
    let tmp = aterm_tempfile::tempdir().unwrap();
    let built = tmp.path().join("target/aterm-vz-host");
    std::fs::create_dir_all(built.parent().unwrap()).unwrap();
    std::fs::write(&built, b"\xcf\xfa\xed\xfe helper").unwrap();
    let host = tmp.path().join("vz.noindex/host");
    let mut signed = Vec::new();
    let placed = place(&built, &host, &mut |copy, ents| {
        signed.push((copy.to_path_buf(), ents.to_path_buf()));
        assert!(
            copy.parent()
                .unwrap()
                .file_name()
                .unwrap()
                .to_str()
                .unwrap()
                .starts_with(".partial-")
        );
        assert_eq!(std::fs::read_to_string(ents).unwrap(), ENTITLEMENTS);
        // What codesign does: rewrite the file in place.
        std::fs::write(copy, b"\xcf\xfa\xed\xfe helper, signed").unwrap();
        Ok(())
    })
    .unwrap();
    assert_eq!(signed.len(), 1);
    assert!(placed.fresh);
    assert_eq!(
        placed.sha256,
        sha(b"\xcf\xfa\xed\xfe helper"),
        "the built file's hash"
    );
    assert_eq!(placed.exe, host.join(&placed.sha256).join(EXE));
    assert_eq!(
        std::fs::read(&placed.exe).unwrap(),
        b"\xcf\xfa\xed\xfe helper, signed"
    );
    let mode = std::fs::metadata(&placed.exe).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode, 0o555);
    assert_eq!(
        std::fs::read(&built).unwrap(),
        b"\xcf\xfa\xed\xfe helper",
        "nothing is signed in place in target/"
    );
    // Placed again: kept, never signed twice.
    let again = place(&built, &host, &mut |_, _| panic!("signed twice")).unwrap();
    assert!(!again.fresh);
    assert_eq!(again.exe, placed.exe);
    let names: Vec<_> = std::fs::read_dir(&host)
        .unwrap()
        .map(|e| e.unwrap().file_name())
        .collect();
    assert_eq!(names.len(), 1, "no partial directory is left: {names:?}");
    // The helper names itself as the lab's `Attach` does: the built file's hash, which
    // its signed copy's bytes no longer have; one run from target/ hashes its own file.
    assert_eq!(helper_sha256(&placed.exe).unwrap(), placed.sha256);
    assert_eq!(helper_sha256(&built).unwrap(), placed.sha256);
    assert!(
        helper_sha256(&tmp.path().join("gone"))
            .unwrap_err()
            .contains("could not read")
    );
}

#[test]
fn a_failed_signing_places_nothing() {
    let tmp = aterm_tempfile::tempdir().unwrap();
    let built = tmp.path().join("aterm-vz-host");
    std::fs::write(&built, b"helper").unwrap();
    let host = tmp.path().join("host");
    let err = place(&built, &host, &mut |_, _| Err("codesign said no".into())).unwrap_err();
    assert_eq!(err, "codesign said no");
    assert_eq!(std::fs::read_dir(&host).unwrap().count(), 0);
}

fn copy(host: &Path, name: &str, markers: &[&str]) -> PathBuf {
    let dir = host.join(name);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join(EXE), b"helper").unwrap();
    for m in markers {
        std::fs::write(dir.join(m), b"pid 1\n").unwrap();
    }
    dir
}

#[test]
fn a_copy_is_reclaimed_only_once_it_proved_it_ran_and_no_record_names_it() {
    let tmp = aterm_tempfile::tempdir().unwrap();
    let host = tmp.path();
    let current = copy(host, &"a".repeat(64), &[STARTED, RAN]);
    let never_started = copy(host, &"b".repeat(64), &[]);
    let ran = copy(host, &"c".repeat(64), &[STARTED, RAN]);
    let unproven = copy(host, &"d".repeat(64), &[STARTED]);
    let named = copy(host, &"e".repeat(64), &[STARTED, RAN]);
    let other = host.join("not-a-hash");
    std::fs::create_dir_all(&other).unwrap();
    let record = Record {
        pid: 1,
        uniq: 1,
        birth_us: 1,
        helper_sha256: "e".repeat(64),
        proto: crate::proto::PROTO,
        live_disk: "/l".into(),
        started: 0,
        boot_id: None,
        agent_sha256: None,
    };
    let removed = reclaim(host, &"a".repeat(64), Some(&record));
    assert_eq!(removed, [never_started.clone(), ran.clone()]);
    for kept in [&current, &unproven, &named, &other] {
        assert!(kept.exists(), "{} kept", kept.display());
    }
    // The marker's own path: a helper outside a content-addressed directory writes none.
    let loose = tmp.path().join("loose/aterm-vz-host");
    assert_eq!(content_dir(&loose), None);
    assert_eq!(
        content_dir(&unproven.join(EXE)).map(|(_, n)| n.len()),
        Some(64)
    );
    let in_use = mark_ran(&unproven.join(EXE)).expect("the marker is held");
    assert!(
        std::fs::read_to_string(unproven.join(RAN))
            .unwrap()
            .starts_with("pid ")
    );
    // A helper still running from it holds its marker: kept, though no record names it
    // (its helper serves another state directory).
    assert!(reclaim(host, &"a".repeat(64), Some(&record)).is_empty());
    assert!(unproven.exists());
    // The negative control: once its helper is gone, the copy goes.
    drop(in_use);
    assert_eq!(reclaim(host, &"a".repeat(64), Some(&record)), [unproven]);
    // A marker with no line (a helper that died before it wrote one) proves nothing.
    let empty = copy(host, &"9".repeat(64), &[STARTED]);
    std::fs::write(empty.join(RAN), b"").unwrap();
    assert!(reclaim(host, &"a".repeat(64), Some(&record)).is_empty());
    assert!(empty.exists());
    // A symlinked `ran` marker proves nothing and is refused by `mark_ran`.
    let linked = copy(host, &"8".repeat(64), &[STARTED]);
    let outside = tmp.path().join("outside-ran");
    std::fs::write(&outside, b"pid 1\n").unwrap();
    std::os::unix::fs::symlink(&outside, linked.join(RAN)).unwrap();
    assert!(!super::ran(&linked));
    assert!(mark_ran(&linked.join(EXE)).is_none());
    assert_eq!(std::fs::read(&outside).unwrap(), b"pid 1\n");
}

#[test]
fn unknown_started_markers_keep_copies_until_their_run_is_proven_and_unheld() {
    let tmp = aterm_tempfile::tempdir().unwrap();
    let host = tmp.path().join("host");
    let never = copy(&host, &"a".repeat(64), &[]);
    let proved = copy(&host, &"b".repeat(64), &[STARTED, RAN]);
    assert_eq!(reclaim(&host, "", None), [never, proved]);

    for kind in ["symlink", "dangling", "directory"] {
        let dir = copy(&host, &sha(kind.as_bytes()), &[]);
        let marker = dir.join(STARTED);
        let outside = tmp.path().join(format!("outside-{kind}"));
        if kind == "directory" {
            std::fs::create_dir(&marker).unwrap();
        } else {
            if kind == "symlink" {
                std::fs::write(&outside, b"retained outside marker\n").unwrap();
            }
            std::os::unix::fs::symlink(&outside, &marker).unwrap();
        }
        assert!(std::fs::symlink_metadata(&marker).is_ok());
        assert!(!super::ran(&dir));
        assert!(
            reclaim(&host, "", None).is_empty(),
            "a {kind} marker proves no safe retirement"
        );
        assert_eq!(std::fs::read(dir.join(EXE)).unwrap(), b"helper");
        assert!(std::fs::symlink_metadata(&marker).is_ok());

        // These inert bytes are never executed: only the normal marker/lock API runs.
        let held = mark_ran(&dir.join(EXE)).expect("a witnessed marker is held");
        assert!(reclaim(&host, "", None).is_empty());
        drop(held);
        assert_eq!(reclaim(&host, "", None), [dir]);
        if kind == "symlink" {
            assert_eq!(
                std::fs::read(&outside).unwrap(),
                b"retained outside marker\n"
            );
        }
    }
}

#[test]
fn replacing_a_nonregular_executable_requires_safe_retirement_of_its_real_directory() {
    for state in ["never", "ran", "unproven", "in-use"] {
        let tmp = aterm_tempfile::tempdir().unwrap();
        let built = tmp.path().join("built");
        std::fs::write(&built, b"inert built helper").unwrap();
        let host = tmp.path().join("host");
        let dir = host.join(sha(b"inert built helper"));
        std::fs::create_dir_all(&dir).unwrap();
        let decoy = tmp.path().join("inert-decoy");
        std::fs::write(&decoy, b"retained decoy").unwrap();
        std::os::unix::fs::symlink(&decoy, dir.join(EXE)).unwrap();
        let payload = dir.join("retained-state");
        std::fs::write(&payload, b"unproven state must survive").unwrap();
        if state != "never" {
            std::fs::write(dir.join(STARTED), b"synthetic started\n").unwrap();
        }
        let held = matches!(state, "ran" | "in-use")
            .then(|| mark_ran(&dir.join(EXE)).expect("the inert copy's marker"));
        let held = if state == "ran" {
            drop(held);
            None
        } else {
            held
        };
        assert!(std::fs::symlink_metadata(&dir).unwrap().is_dir());
        assert!(
            std::fs::symlink_metadata(dir.join(EXE))
                .unwrap()
                .is_symlink()
        );
        let result = place(&built, &host, &mut |copy, _| {
            std::fs::write(copy, b"inert signed replacement").unwrap();
            Ok(())
        });
        if matches!(state, "never" | "ran") {
            let placed = result.expect("safe, unused copies can be replaced");
            assert!(placed.fresh);
            assert_eq!(
                std::fs::read(&placed.exe).unwrap(),
                b"inert signed replacement"
            );
        } else {
            assert!(
                result.is_err(),
                "the {state} real directory must be retained"
            );
            assert!(
                std::fs::symlink_metadata(dir.join(EXE))
                    .unwrap()
                    .is_symlink()
            );
            assert_eq!(
                std::fs::read(&payload).unwrap(),
                b"unproven state must survive"
            );
            assert_eq!(
                std::fs::read(dir.join(STARTED)).unwrap(),
                b"synthetic started\n"
            );
        }
        assert_eq!(std::fs::read(&decoy).unwrap(), b"retained decoy");
        let names: Vec<_> = std::fs::read_dir(&host)
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect();
        assert_eq!(names.len(), 1, "partial copies are not retained: {names:?}");
        drop(held);
    }
}

#[test]
fn place_replaces_a_symlinked_dir_or_exe_with_a_real_signed_copy() {
    let tmp = aterm_tempfile::tempdir().unwrap();
    let built = tmp.path().join("target/aterm-vz-host");
    std::fs::create_dir_all(built.parent().unwrap()).unwrap();
    std::fs::write(&built, b"helper-bytes").unwrap();
    let digest = sha(b"helper-bytes");
    let host = tmp.path().join("host");
    std::fs::create_dir_all(&host).unwrap();
    let decoy_dir = tmp.path().join("decoy");
    std::fs::create_dir_all(&decoy_dir).unwrap();
    let decoy_exe = decoy_dir.join(EXE);
    std::fs::write(&decoy_exe, b"unsigned-decoy").unwrap();
    std::fs::write(decoy_dir.join(STARTED), b"unproven decoy\n").unwrap();
    // A symlink at `host/<sha256>` pointing to `decoy_dir` must not be trusted as placed.
    std::os::unix::fs::symlink(&decoy_dir, host.join(&digest)).unwrap();
    let placed = place(&built, &host, &mut |copy, _| {
        std::fs::write(copy, b"signed-real").unwrap();
        Ok(())
    })
    .unwrap();
    assert!(placed.fresh);
    assert!(
        std::fs::symlink_metadata(host.join(&digest))
            .unwrap()
            .is_dir()
    );
    assert_eq!(std::fs::read(&placed.exe).unwrap(), b"signed-real");
    assert_eq!(std::fs::read(&decoy_exe).unwrap(), b"unsigned-decoy");
    assert_eq!(
        std::fs::read(decoy_dir.join(STARTED)).unwrap(),
        b"unproven decoy\n"
    );
}

#[test]
fn a_dead_placings_partial_directory_is_reclaimed_and_a_live_ones_kept() {
    let tmp = aterm_tempfile::tempdir().unwrap();
    let mine = tmp.path().join(format!(
        ".partial-{}-{}",
        "f".repeat(64),
        std::process::id()
    ));
    std::fs::create_dir_all(&mine).unwrap();
    // pid 0 is never a live placing (`pid_alive` refuses it).
    let dead = tmp.path().join(format!(".partial-{}-0", "f".repeat(64)));
    std::fs::create_dir_all(&dead).unwrap();
    let non_canonical = tmp.path().join(format!(".partial-{}-+0", "f".repeat(64)));
    std::fs::create_dir_all(&non_canonical).unwrap();
    assert_eq!(reclaim(tmp.path(), "", None), [dead]);
    assert!(mine.exists());
    assert!(non_canonical.exists());
}
