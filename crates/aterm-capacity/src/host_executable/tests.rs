// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

fn scratch() -> aterm_tempfile::TempDir {
    // Keep room for the store's socket under SUN_LEN even when the verifier
    // supplies a long TMPDIR. Canonicalize macOS's /tmp alias before validation.
    let base = fs::canonicalize("/tmp").unwrap();
    aterm_tempfile::Builder::new()
        .prefix("he-")
        .suffix(".noindex")
        .tempdir_in(base)
        .unwrap()
}
fn store(root: &Path) -> Store {
    narrow_test_image();
    Store::prepare_at(&root.join("store")).unwrap()
}
fn clean_images(store: &Store) {
    for entry in fs::read_dir(store.root.join("images")).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            fs::set_permissions(path, fs::Permissions::from_mode(0o700)).unwrap();
        }
    }
}

#[test]
fn provenance_prune_retains_unknown_foreign_and_live_old_images() {
    let root = scratch();
    let s = store(root.path());
    let mut rows = Vec::new();
    for pid in 10..16 {
        rows.push(Record {
            pid,
            birth: 100,
            host: "here".into(),
            boot: "now".into(),
            digest: "0".repeat(64),
            image: FileId { dev: 1, ino: 1 },
            directory: FileId { dev: 1, ino: 2 },
            socket_hex: hex(s.root.join(format!("{pid}.sock")).as_os_str().as_bytes()),
            socket: FileId { dev: 1, ino: 3 },
        });
    }
    rows[0].boot = "prior-boot".into();
    rows[4].host = "foreign".into();
    rows[4].boot = "prior-boot".into();
    rows[5].digest = "f".repeat(64); // A live older executable remains discoverable.
    let bytes = Book {
        version: 1,
        instances: rows,
    }
    .encode()
    .unwrap();
    aterm_uds::atomic_file::write(&s.root.join("instances/book.json"), &bytes, 0o600).unwrap();
    let mut book = s.read_book().unwrap();
    let mut observed = Vec::new();
    prune_records(&mut book.instances, "here", "now", |pid| {
        observed.push(pid);
        match pid {
            11 => Lifetime::Stable(Birth(200)),
            12 => Lifetime::Absent,
            13 => Lifetime::Unknown,
            15 => Lifetime::Stable(Birth(100)),
            _ => panic!("foreign or prior-boot row queried locally"),
        }
    });
    assert_eq!(observed, [11, 12, 13, 15]);
    assert_eq!(
        book.instances.iter().map(|r| r.pid).collect::<Vec<_>>(),
        [13, 14, 15]
    );
    assert_eq!(book.instances[2].digest, "f".repeat(64));
}

#[test]
fn missing_partial_replaced_and_malformed_state_fail_closed() {
    let root = scratch();
    let path = root.path().join("store");
    assert!(Store::open_at(&path).is_err());
    assert!(!path.exists());
    let s = store(root.path());
    assert!(s.instances().unwrap().is_empty());
    fs::rename(s.root.join("instances"), s.root.join("old-instances")).unwrap();
    private_file::ensure_private_dir(&s.root.join("instances")).unwrap();
    assert!(s.instances().is_err());
    assert!(Store::prepare_at(&s.root).is_err());
    fs::remove_dir(s.root.join("instances")).unwrap();
    fs::rename(s.root.join("old-instances"), s.root.join("instances")).unwrap();
    for json in [
        b"{\"version\":1,\"instances\":[],\"extra\":true}".as_slice(),
        b"{\"version\":1,\"version\":1,\"instances\":[]}",
        b"{\"version\":1,\"instances\":[",
    ] {
        aterm_uds::atomic_file::write(&s.root.join("instances/book.json"), json, 0o600).unwrap();
        assert!(s.instances().is_err());
    }
    assert_eq!(unhex(&hex(b"/a/\xff\n")).unwrap(), b"/a/\xff\n");
    assert!(unhex("AB").is_err());
}

#[test]
fn actual_loaded_image_cache_rejects_mutation_and_original_inode_publication() {
    let root = scratch();
    let s = store(root.path());
    let p = s.cache_current().unwrap();
    p.validate().unwrap();
    p.validate_process(std::process::id(), own_identity().unwrap().birth)
        .unwrap();
    assert!(p.validate_process(std::process::id(), Birth(1)).is_err());
    // Publication rejects the original inode even though its content matches exactly.
    let listener_path = s.root.join("h.sock");
    let listener = aterm_uds::kept_out(|| CtlListener::bind(&listener_path)).unwrap();
    aterm_uds::atomic_file::set_mode_nofollow(&listener_path, 0o600).unwrap();
    assert!(s.publish(&p, &listener).is_err());
    fs::set_permissions(p.path(), fs::Permissions::from_mode(0o700)).unwrap();
    assert!(p.validate().is_err());
    fs::set_permissions(p.path(), fs::Permissions::from_mode(0o500)).unwrap();
    let other = s.root.join("alias");
    fs::hard_link(p.path(), &other).unwrap();
    assert!(p.validate().is_err());
    fs::remove_file(other).unwrap();
    p.validate().unwrap();
    clean_images(&s);
}

const CHILD_STORE: &str = "ATERM_HOST_EXECUTABLE_TEST_STORE";
#[test]
#[ignore = "entered only by the cached-image parent test"]
fn cached_image_child() {
    use std::io::Write as _;
    let Some(path) = std::env::var_os(CHILD_STORE) else {
        return;
    };
    // Proof of actual execution precedes every assertion, so parent may safely clean the copy.
    println!("cached-image-running");
    std::io::stdout().flush().unwrap();
    let s = Store::open_at(Path::new(&path)).unwrap();
    if std::env::var_os("ATERM_HOST_EXECUTABLE_TEST_DESCENDANT").is_some() {
        assert!(s.instances().unwrap()[0].is_ancestor_of_current().unwrap());
        return;
    }
    let p = s.cache_current().unwrap();
    let sock = s.root.join("h.sock");
    let listener = aterm_uds::kept_out(|| CtlListener::bind(&sock)).unwrap();
    aterm_uds::atomic_file::set_mode_nofollow(&sock, 0o600).unwrap();
    let instance = s.publish(&p, &listener).unwrap();
    assert!(instance.is_ancestor_of_current().unwrap());
    assert_eq!(s.instances().unwrap().len(), 1);
    let mut descendant = aterm_uds::spawnfd::command(p.path());
    descendant
        .args([
            "--ignored",
            "--exact",
            "host_executable::tests::cached_image_child",
            "--nocapture",
        ])
        .env(CHILD_STORE, &s.root)
        .env("ATERM_HOST_EXECUTABLE_TEST_DESCENDANT", "1");
    let result = aterm_uds::deadline::output_within(&mut descendant, Duration::from_secs(60));
    if !result.as_ref().is_ok_and(|out| {
        out.stdout
            .windows(b"cached-image-running".len())
            .any(|w| w == b"cached-image-running")
    }) {
        println!("retain-cached-image");
        std::io::stdout().flush().unwrap();
        panic!("descendant supplied no execution proof: {result:?}");
    }
    assert!(result.unwrap().status.success());
    fs::remove_file(&sock).unwrap();
    let replacement = aterm_uds::kept_out(|| CtlListener::bind(&sock)).unwrap();
    aterm_uds::atomic_file::set_mode_nofollow(&sock, 0o600).unwrap();
    assert!(instance.validate().is_err());
    assert!(s.publish(&p, &listener).is_err());
    drop(replacement);
}

#[test]
fn cached_process_publication_ancestry_and_rebound_listener() {
    let root = scratch();
    let s = store(root.path());
    let p = s.cache_current().unwrap();
    let mut command = aterm_uds::spawnfd::command(p.path());
    command
        .args([
            "--ignored",
            "--exact",
            "host_executable::tests::cached_image_child",
            "--nocapture",
        ])
        .env(CHILD_STORE, &s.root)
        .stdout(aterm_uds::spawnfd::Stdio::piped());
    let result = aterm_uds::deadline::output_within(&mut command, Duration::from_secs(60));
    let ran = result.as_ref().is_ok_and(|out| {
        out.stdout
            .windows(b"cached-image-running".len())
            .any(|w| w == b"cached-image-running")
    });
    if !ran
        || result.as_ref().is_ok_and(|out| {
            out.stdout
                .windows(b"retain-cached-image".len())
                .any(|w| w == b"retain-cached-image")
        })
    {
        // A copied Mach-O never loses its directory before an execution proof.
        let retained = root.keep();
        panic!(
            "child supplied no execution proof; retained {} ({result:?})",
            retained.display()
        );
    }
    clean_images(&s);
    let result = result.unwrap();
    assert!(result.status.success(), "{result:?}");
    assert_eq!(s.instances().unwrap().len(), 1);
    assert!(s.instances().unwrap()[0].validate().is_err());
}
