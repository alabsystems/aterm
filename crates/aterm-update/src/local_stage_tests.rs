// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::*;
use ring::signature::{Ed25519KeyPair, KeyPair};

fn fixture(dir: &Path, revoked: bool) -> String {
    let master = Ed25519KeyPair::from_seed_unchecked(&[61; 32]).unwrap();
    let member = Ed25519KeyPair::from_seed_unchecked(&[62; 32]).unwrap();
    let b64 = |b: &[u8]| aterm_codec::base64::encode(b).unwrap();
    let roster = format!(
        "schema=1\nroster_seq=7\nvalid_until=\"2099-01-01T00:00:00Z\"\nrevoked=[{}]\n[[machine]]\nid=\"m\"\npubkey={:?}\nadded_at=\"2026-01-01T00:00:00Z\"\n",
        if revoked { "\"m\"" } else { "" },
        b64(member.public_key().as_ref())
    );
    let manifest = format!(
        "schema=1\nversion=\"0.10.0\"\nbuild_number=10\ncommit=\"{}\"\nsha256=\"{}\"\ndmg=\"aterm-0.10.0.dmg\"\nurl=\"https://github.com/alabsystems/aterm/releases/download/v0.10.0/aterm-0.10.0.dmg\"\nmachine_id=\"m\"\nroster_seq=7\n",
        "a".repeat(40),
        "b".repeat(64)
    );
    let manifest_sig = member.sign(manifest.as_bytes());
    let roster_sig = master.sign(roster.as_bytes());
    for (name, bytes) in [
        (APPCAST_ASSET, manifest.as_bytes()),
        (aterm_update_core::roster::ROSTER_ASSET, roster.as_bytes()),
        (APPCAST_SIG_ASSET, manifest_sig.as_ref()),
        (
            aterm_update_core::roster::ROSTER_SIG_ASSET,
            roster_sig.as_ref(),
        ),
    ] {
        std::fs::write(dir.join(name), bytes).unwrap();
    }
    b64(master.public_key().as_ref())
}

#[test]
fn offline_transport_keeps_the_ordinary_roster_and_manifest_authority() {
    let dir = crate::paths::scratch_dir("offline-auth");
    let key = fixture(dir.path(), false);
    let keys = [key.as_str()];
    let policy = RosterPolicy {
        master_pubkeys: &keys,
        floor_seq: 0,
        now_unix: 1_793_000_000,
        floor_refresh: None,
    };
    let floor = crate::manifest::Floor::default();
    assert!(
        authenticate(dir.path(), &policy, &floor)
            .unwrap()
            .selected
            .is_some()
    );
    std::fs::write(dir.path().join(APPCAST_SIG_ASSET), [0u8; 64]).unwrap();
    assert!(authenticate(dir.path(), &policy, &floor).is_err());
    fixture(dir.path(), true);
    assert!(authenticate(dir.path(), &policy, &floor).is_err());
    fixture(dir.path(), false);
    let stale_floor = RosterPolicy {
        floor_seq: 8,
        ..policy
    };
    assert!(authenticate(dir.path(), &stale_floor, &floor).is_err());
}

#[test]
fn local_transport_refuses_links_special_files_bounds_and_nonleaf_names() {
    let dir = crate::paths::scratch_dir("éééééééééé-local");
    assert!(
        dir.file_name()
            .and_then(|os| os.to_str())
            .is_some_and(
                |name| name.starts_with("aterm-upd-éééééééé-") && name.ends_with(".noindex")
            )
    );
    for staged in [
        stage_scratch(None).unwrap(),
        stage_scratch(Some(&dir)).unwrap(),
    ] {
        assert!(
            staged
                .path()
                .file_name()
                .and_then(|os| os.to_str())
                .is_some_and(|name| name.ends_with(".noindex")),
            "stage_scratch must carry .noindex: {}",
            staged.path().display()
        );
    }
    std::fs::write(dir.path().join("file"), b"abc").unwrap();
    assert_eq!(read_asset(dir.path(), "file", 3).unwrap(), b"abc");
    assert!(read_asset(dir.path(), "file", 2).is_err());
    std::os::unix::fs::symlink(dir.path().join("file"), dir.path().join("link")).unwrap();
    assert!(read_asset(dir.path(), "link", 3).is_err());
    assert!(read_asset(dir.path(), ".", 3).is_err());
    assert!(read_asset(dir.path(), "../file", 3).is_err());
    assert!(regular(dir.path()).is_err());
}
#[test]
fn final_publication_rechecks_installed_floor_and_existing_stage_without_losing_custody() {
    let dir = crate::paths::scratch_dir("final-pub");
    let live = at(dir.path().join("live"));
    std::fs::create_dir_all(live.staged_dir()).unwrap();
    let incoming = dir.path().join("incoming.app");
    std::fs::create_dir_all(incoming.join("Contents")).unwrap();
    let ready = Ready {
        build_number: 10,
        version: "0.10.0".into(),
        commit: Some("a".repeat(40)),
        dmg_sha256: "b".repeat(64),
        team_id: "T".into(),
        staged_at: install::now_rfc3339(),
        changelog: None,
        machine_id: Some("m".into()),
        roster_seq: Some(7),
        roster_confirmed_at: Some(install::now_rfc3339()),
    };
    std::fs::write(incoming.join("Contents/Info.plist"), format!(
        "<plist><dict><key>CFBundleVersion</key><string>10</string><key>ATermGitCommit</key><string>{}</string></dict></plist>", "a".repeat(40))).unwrap();
    let observation = crate::manifest::FloorObservation {
        high_water: 10,
        roster_seq: 7,
        machine: Some("m"),
        ..crate::manifest::FloorObservation::default()
    };
    // The late actual-install callback can revoke after all expensive checks completed.
    assert!(
        install::publish_local_checked(
            &live,
            &incoming,
            &ready,
            || Err("actual install changed".into()),
            &observation
        )
        .is_err()
    );
    assert!(incoming.exists());
    assert!(!live.ready.exists());
    // Unknown prior custody is not overwritten.
    std::fs::write(&live.ready, "unreadable prior stage").unwrap();
    assert!(
        install::publish_local_checked(&live, &incoming, &ready, || Ok(()), &observation).is_err()
    );
    assert_eq!(
        std::fs::read_to_string(&live.ready).unwrap(),
        "unreadable prior stage"
    );
    std::fs::remove_file(&live.ready).unwrap();
    install::publish_local_checked(&live, &incoming, &ready, || Ok(()), &observation).unwrap();
    assert_eq!(Ready::read_publishable(&live).unwrap().build_number, 10);
    assert!(!incoming.exists());
    // A repeated/equal candidate never replaces that existing verified decision.
    assert!(
        install::publish_local_checked(&live, &incoming, &ready, || Ok(()), &observation).is_err()
    );
    assert_eq!(Ready::read_publishable(&live).unwrap().build_number, 10);
}
#[test]
fn host_binding_uses_the_actual_loaded_image_and_rejects_same_path_replacement() {
    use aterm_uds::deadline::{KeptUntilRan, ad_hoc_copy, cat_echoes};
    use aterm_uds::spawnfd::Stdio;

    let mut dir = KeptUntilRan::new(crate::paths::scratch_dir("host-binding").keep());
    let app = dir.path().join("actual.app");
    let exe = app.join("Contents/MacOS/aterm");
    std::fs::create_dir_all(exe.parent().unwrap()).unwrap();
    ad_hoc_copy(Path::new("/bin/cat"), &exe);
    let mut child = aterm_uds::spawnfd::Command::new(&exe)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    // Keep the child alive after its echo proves this copy finished its first exec.
    // Without that proof its directory must survive a failed program assessment.
    let _input = cat_echoes(&mut child);
    dir.ran();
    let observed = (|| -> Result<(), String> {
        let pid = i32::try_from(child.id()).map_err(|e| e.to_string())?;
        let binding = HostBinding::capture(pid)?;
        if binding.app != app.canonicalize().map_err(|e| e.to_string())? {
            return Err("kernel target did not match actual executable".into());
        }
        // An identical unrelated copy is not the bound target and cannot substitute for it.
        let other = dir.path().join("copied.app/Contents/MacOS/aterm");
        std::fs::create_dir_all(other.parent().unwrap()).map_err(|e| e.to_string())?;
        std::fs::copy(&exe, &other).map_err(|e| e.to_string())?;
        binding.current()?;
        std::fs::rename(&exe, exe.with_extension("old")).map_err(|e| e.to_string())?;
        std::fs::copy(&other, &exe).map_err(|e| e.to_string())?;
        if binding.current().is_ok() {
            return Err("new inode at old name lent target authority".into());
        }
        Ok(())
    })();
    let _ = child.kill();
    let _ = child.wait();
    observed.unwrap();
}
