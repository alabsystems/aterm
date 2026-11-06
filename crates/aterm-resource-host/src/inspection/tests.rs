// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
use super::*;
use aterm_capacity::host_executable::Store;
use std::{fs, io::Write as _, os::unix::fs::PermissionsExt as _};

fn observation() -> Value {
    #[cfg(target_os = "macos")]
    let kind = "macos_wall_microseconds";
    #[cfg(not(target_os = "macos"))]
    let kind = "linux_start_ticks";
    aterm_json::json!({"version":3,"ready":true,"session":"s-a","attempt":"ab".repeat(16),
        "host":"fixture","boot":"cd".repeat(16),"root_pid":7,"root_birth":8,"root_birth_kind":kind,
        "subject":{"mode":"root","pid":7,"birth":8},
        "cpu":{"mode":"retained_fixed","units":1,"native_manifest":{"device":1,"inode":2}},
        "memory":{"bytes":1024,"claim":{"v":3,"namespace":"ef".repeat(16),"marker":"123-7-0",
            "epoch":"ab".repeat(16),"guard":{"device":1,"inode":3}}}})
}
#[test]
fn malformed_custody_and_oversized_prefix_are_rejected() {
    let good = observation();
    validate(&good, "s-a").unwrap();
    let mut shared = good.clone();
    shared.as_object_mut().unwrap().insert(
        "cpu".into(),
        aterm_json::json!({
        "mode":"cooperative_shared","ceiling":4,"native_manifest":{"device":1,"inode":2}}),
    );
    validate(&shared, "s-a").unwrap();
    shared
        .as_object_mut()
        .unwrap()
        .get_mut("cpu")
        .unwrap()
        .as_object_mut()
        .unwrap()
        .insert("ceiling".into(), 0.into());
    assert!(validate(&shared, "s-a").is_err());
    let mut bad = good.clone();
    bad.as_object_mut()
        .unwrap()
        .get_mut("cpu")
        .unwrap()
        .as_object_mut()
        .unwrap()
        .insert("ceiling".into(), 4.into());
    assert!(validate(&bad, "s-a").is_err());
    for (field, value) in [
        ("ready", Value::Bool(false)),
        ("root_birth", 0.into()),
        ("attempt", "AB".repeat(16).into()),
    ] {
        let mut bad = good.clone();
        bad.as_object_mut().unwrap().insert(field.into(), value);
        assert!(validate(&bad, "s-a").is_err());
    }
    assert!(validate(&good, "s-b").is_err());
    for bytes in [
        ((MAX_REPLY + 1) as u32).to_be_bytes().to_vec(),
        vec![0, 0, 0, 4, 1],
    ] {
        let (a, mut b) = aterm_uds::CtlStream::pair().unwrap();
        b.write_all(&bytes).unwrap();
        drop(b);
        assert!(read_reply(&a, Instant::now() + Duration::from_secs(60)).is_err());
    }
}
#[test]
fn missing_store_lookup_creates_nothing() {
    let root = aterm_tempfile::tempdir().unwrap();
    let missing = root.path().join("absent");
    assert!(lookup_at(&missing, "s-a").is_err());
    assert!(!missing.exists());
}

const CHILD_STORE: &str = "ATERM_INSPECTION_TEST_STORE";
const ORIGINAL_IMAGE: &str = "ATERM_INSPECTION_TEST_ORIGINAL";
#[test]
fn cached_inspector_helper() {
    let Some(path) = std::env::var_os(CHILD_STORE) else {
        return;
    };
    let result = lookup_at(Path::new(&path), "s-a");
    assert_eq!(result.unwrap(), observation());
    println!("inspector-helper-ran");
}
#[test]
fn cached_custodian_helper() {
    let Some(path) = std::env::var_os(CHILD_STORE) else {
        return;
    };
    println!("cached-custodian-ran");
    std::io::stdout().flush().unwrap();
    let store = Store::open_at(Path::new(&path)).unwrap();
    let program = store.cache_current().unwrap();
    let sock = Path::new(&path).join("h.sock");
    let listener = aterm_uds::kept_out(|| aterm_uds::CtlListener::bind(&sock)).unwrap();
    aterm_uds::atomic_file::set_mode_nofollow(&sock, 0o600).unwrap();
    store.publish(&program, &listener).unwrap();
    let cloned = aterm_uds::kept_out(|| listener.try_clone()).unwrap();
    let server = std::thread::spawn(move || {
        for _ in 0..2 {
            let (stream, _) = aterm_uds::accept(&cloned).unwrap();
            let deadline = Instant::now() + Duration::from_secs(60);
            let request: Value =
                aterm_json::from_slice(&wire::read_frame(&stream, deadline).unwrap()).unwrap();
            assert_eq!(
                request,
                aterm_json::json!({"version":3,"role":"inspect","session":"s-a"})
            );
            wire::write_frame(
                &stream,
                &aterm_json::to_vec(&observation()).unwrap(),
                deadline,
            )
            .unwrap();
            assert_eq!(wire::read_frame(&stream, deadline).unwrap(), [1]);
        }
    });
    run_helper(program.path(), Path::new(&path));
    run_helper(
        Path::new(&std::env::var_os(ORIGINAL_IMAGE).unwrap()),
        Path::new(&path),
    );
    server.join().unwrap();
}
fn run_helper(program: &Path, store: &Path) {
    let mut command = aterm_uds::spawnfd::command(program);
    command
        .args([
            "--exact",
            "inspection::tests::cached_inspector_helper",
            "--nocapture",
        ])
        .env(CHILD_STORE, store);
    let output = aterm_uds::deadline::output_within(&mut command, Duration::from_secs(60)).unwrap();
    assert!(output.status.success(), "{output:?}");
    assert!(
        output
            .stdout
            .windows(b"inspector-helper-ran".len())
            .any(|w| w == b"inspector-helper-ran")
    );
}
#[test]
fn cached_host_queries_real_descendants_with_different_loaded_images() {
    let physical = fs::canonicalize(std::env::temp_dir()).unwrap();
    let root = aterm_tempfile::Builder::new()
        .prefix("ni-")
        .tempdir_in(physical)
        .unwrap();
    let store_path = root.path().join("s");
    let store = Store::prepare_at(&store_path).unwrap();
    let program = store.cache_current().unwrap();
    let mut command = aterm_uds::spawnfd::command(program.path());
    command
        .args([
            "--exact",
            "inspection::tests::cached_custodian_helper",
            "--nocapture",
        ])
        .env(CHILD_STORE, &store_path)
        .env(ORIGINAL_IMAGE, std::env::current_exe().unwrap());
    let result = aterm_uds::deadline::output_within(&mut command, Duration::from_secs(60));
    if !result.as_ref().is_ok_and(|out| {
        out.stdout
            .windows(b"cached-custodian-ran".len())
            .any(|w| w == b"cached-custodian-ran")
    }) {
        let retained = root.keep();
        panic!(
            "no execution proof; retained {}: {result:?}",
            retained.display()
        );
    }
    if !result.as_ref().is_ok_and(|out| out.status.success()) {
        let retained = root.keep();
        panic!(
            "cached helper failed; retained {}: {result:?}",
            retained.display()
        );
    }
    for entry in fs::read_dir(store_path.join("images")).unwrap() {
        fs::set_permissions(entry.unwrap().path(), fs::Permissions::from_mode(0o700)).unwrap();
    }
}
