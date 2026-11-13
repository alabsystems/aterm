// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

fn directory(path: &Path) {
    std::fs::DirBuilder::new().mode(0o700).create(path).unwrap();
}
fn owned_vm(ns: &Namespace) {
    ns.create_vm(|| {
        directory(&ns.vm_dir());
        Ok(())
    })
    .unwrap();
}
fn local(argv: &[String]) -> Result<String, String> {
    let out = aterm_uds::spawnfd::command("/bin/sh")
        .args(&argv[1..])
        .output()
        .unwrap();
    if out.status.success() {
        Ok(String::from_utf8(out.stdout).unwrap())
    } else {
        Err(format!(
            "local guest refused: {}",
            String::from_utf8_lossy(&out.stderr)
        ))
    }
}

#[test]
fn existing_names_and_failed_creations_never_authorize_vm_mutation() {
    let t = aterm_tempfile::tempdir().unwrap();
    let home = t.path().join("lima");
    directory(&home);
    std::fs::write(home.join("notes"), "keep").unwrap();
    assert!(Namespace::open(&home, "aterm-linux").is_err());
    assert!(!home.join("owner").exists());
    assert_eq!(std::fs::read_to_string(home.join("notes")).unwrap(), "keep");

    let ns = Namespace::open(&t.path().join("owned"), "aterm-linux").unwrap();
    assert!(
        ns.create_vm(|| {
            directory(&ns.vm_dir());
            Err::<(), _>("create failed".into())
        })
        .is_err()
    );
    let mut calls = 0;
    assert!(ns.with_vm(|| calls += 1).is_err());
    assert!(
        ns.create_vm(|| {
            calls += 1;
            Ok(())
        })
        .is_err()
    );
    assert_eq!(calls, 0);
    assert!(!ns.receipt("vm").exists());

    let known = Namespace::open(&t.path().join("known"), "aterm-linux").unwrap();
    owned_vm(&known);
    known.with_vm(|| calls += 1).unwrap();
    Namespace::open(&known.path, "aterm-linux")
        .unwrap()
        .with_vm(|| calls += 1)
        .unwrap();
    assert_eq!(calls, 2);
    std::fs::remove_file(known.vm_dir().join(VM_MARKER)).unwrap();
    assert!(
        known
            .with_vm(|| panic!("VM without its creation marker was mutated"))
            .is_err()
    );
    let default = Path::new("/cache/lima.noindex");
    assert_eq!(super::home(None, default).unwrap(), default);
    assert_eq!(super::home(Some(""), default).unwrap(), default);
    assert_eq!(
        super::home(Some("/explicit/lima"), default).unwrap(),
        Path::new("/explicit/lima")
    );
}

#[test]
fn replacement_and_linked_receipts_are_refused_without_borrowing_the_other_vm() {
    let t = aterm_tempfile::tempdir().unwrap();
    let cpu = Namespace::open(&t.path().join("lima"), "aterm-linux").unwrap();
    let gpu = Namespace::open(&cpu.path, "aterm-linux-gpu").unwrap();
    owned_vm(&cpu);
    owned_vm(&gpu);
    assert_ne!(cpu.lock_path(), gpu.lock_path());
    std::fs::rename(cpu.vm_dir(), cpu.path.join("retained-vm")).unwrap();
    directory(&cpu.vm_dir());
    assert!(
        cpu.with_vm(|| panic!("replacement VM was mutated"))
            .is_err()
    );
    gpu.with_vm(|| ()).unwrap();
    assert!(cpu.retired_vm().is_err());
    let owner = cpu.path.join("owner");
    let retained = cpu.path.join("retained-owner");
    std::fs::rename(&owner, &retained).unwrap();
    std::os::unix::fs::symlink(&retained, &owner).unwrap();
    assert!(
        gpu.with_vm(|| panic!("linked receipt was trusted"))
            .is_err()
    );
    assert!(Namespace::open(&cpu.path, "aterm-linux").is_err());
}

#[test]
fn guest_cleanup_preserves_unwitnessed_replacements_and_the_historical_literal() {
    let t = aterm_tempfile::tempdir().unwrap();
    let ns = Namespace::open(&t.path().join("lima"), "aterm-linux").unwrap();
    owned_vm(&ns);
    let home = t.path().to_str().unwrap();
    let literal = t.path().join("aterm-lane");
    directory(&literal);
    std::fs::write(literal.join("notes"), "keep").unwrap();
    let guest = ns.prepare_guest(home, local).unwrap();
    assert_eq!(
        std::fs::symlink_metadata(&guest.path).unwrap().mode() & 0o777,
        0o755
    );
    std::fs::set_permissions(
        &guest.path,
        std::os::unix::fs::PermissionsExt::from_mode(0o700),
    )
    .unwrap();
    std::fs::write(Path::new(&guest.path).join("build"), "output").unwrap();
    ns.clear_guest(local).unwrap();
    assert_eq!(
        std::fs::symlink_metadata(&guest.path).unwrap().mode() & 0o777,
        0o755
    );
    assert!(!Path::new(&guest.path).join("build").exists());
    assert!(Path::new(&guest.path).join(".aterm-owner").exists());
    assert_eq!(ns.prepare_guest(home, local).unwrap().path, guest.path);
    assert_eq!(
        std::fs::read_to_string(literal.join("notes")).unwrap(),
        "keep"
    );

    let marker = Path::new(&guest.path).join(".aterm-owner");
    let expected = std::fs::read_to_string(&marker).unwrap();
    std::fs::write(&marker, "foreign\n").unwrap();
    std::fs::write(Path::new(&guest.path).join("build"), "keep").unwrap();
    assert!(ns.clear_guest(local).is_err());
    assert_eq!(
        std::fs::read_to_string(Path::new(&guest.path).join("build")).unwrap(),
        "keep"
    );
    std::fs::write(&marker, expected).unwrap();

    let retained_marker = t.path().join("retained-marker");
    std::fs::rename(&marker, &retained_marker).unwrap();
    std::os::unix::fs::symlink(&retained_marker, &marker).unwrap();
    assert!(
        ns.clear_guest(local).is_err(),
        "a symlink cannot prove a guest marker"
    );
    assert_eq!(
        std::fs::read_to_string(Path::new(&guest.path).join("build")).unwrap(),
        "keep"
    );
    std::fs::remove_file(&marker).unwrap();
    std::fs::rename(&retained_marker, &marker).unwrap();

    let retained = t.path().join("retained-guest");
    std::fs::rename(&guest.path, &retained).unwrap();
    directory(Path::new(&guest.path));
    std::fs::write(Path::new(&guest.path).join("notes"), "foreign").unwrap();
    assert!(ns.clear_guest(local).is_err());
    assert_eq!(
        std::fs::read_to_string(Path::new(&guest.path).join("notes")).unwrap(),
        "foreign"
    );
    std::fs::remove_dir_all(&guest.path).unwrap();
    std::os::unix::fs::symlink(&retained, &guest.path).unwrap();
    assert!(ns.clear_guest(local).is_err());
    assert!(retained.join(".aterm-owner").exists());
    assert_eq!(
        std::fs::read_to_string(retained.join("build")).unwrap(),
        "keep"
    );
}

#[test]
fn malformed_or_linked_guest_receipts_never_dispatch_cleanup() {
    let t = aterm_tempfile::tempdir().unwrap();
    let ns = Namespace::open(&t.path().join("lima"), "aterm-linux").unwrap();
    owned_vm(&ns);
    let home = t.path().to_str().unwrap();
    assert!(
        ns.prepare_guest(home, |_| Ok(format!("{home}/aterm-lane\n1:2:3\n")))
            .is_err()
    );
    assert!(!ns.receipt("guest").exists());
    ns.prepare_guest(home, local).unwrap();
    let receipt = ns.receipt("guest");
    let retained = receipt.with_extension("saved");
    std::fs::rename(&receipt, &retained).unwrap();
    std::os::unix::fs::symlink(&retained, &receipt).unwrap();
    assert!(
        ns.clear_guest(|_| panic!("linked guest receipt dispatched"))
            .is_err()
    );
    std::fs::remove_file(&receipt).unwrap();
    write_new(&receipt, "malformed\n").unwrap();
    assert!(
        ns.clear_guest(|_| panic!("malformed guest receipt dispatched"))
            .is_err()
    );
    assert!(super::home(Some("../foreign"), t.path()).is_err());
}
