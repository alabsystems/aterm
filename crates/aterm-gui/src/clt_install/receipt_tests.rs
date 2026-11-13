// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

const SID: &str = "s-0123456789abcdef0123";

/// The claim a tab's program holds, of `build`.
fn claim(build: &str) -> aterm_types::clt_mark::Claim {
    aterm_types::clt_mark::Claim {
        asker: aterm_types::clt_mark::Asker {
            build: build.to_owned(),
            pid: 23,
            birth: 45_000_123,
        },
        at: 1_800_000_000,
        n: 67,
    }
}

#[test]
fn immutable_child_receipts_are_exact_bounded_and_removed_only_when_matching() {
    let root = aterm_tempfile::tempdir().unwrap();
    let mark = root.path().join("command-line-tools-tab");
    let receipt = Receipt::for_child(&mark, SID, 23, Some(45), None).unwrap();
    receipt.write_exclusive().unwrap();
    {
        use std::os::unix::fs::PermissionsExt as _;
        assert_eq!(
            std::fs::metadata(&receipt.path)
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
    }
    assert!(receipt.matches());
    assert_eq!(
        receipt.write_exclusive().unwrap_err().kind(),
        io::ErrorKind::AlreadyExists
    );
    for (sid, pid, birth) in [("s-abcdef", 23, 45), (SID, 24, 45), (SID, 23, 46)] {
        assert!(
            !Receipt::for_child(&mark, sid, pid, Some(birth), None)
                .unwrap()
                .matches()
        );
    }
    std::fs::write(&receipt.path, b"partial\n").unwrap();
    assert!(!receipt.matches());
    receipt.remove_if_matching();
    assert_eq!(std::fs::read(&receipt.path).unwrap(), b"partial\n");
    std::fs::write(&receipt.path, &receipt.text).unwrap();
    receipt.remove_if_matching();
    assert!(!receipt.path.exists());
    for (sid, pid, birth) in [
        ("../escaped", 23, Some(45)),
        (SID, 1, Some(45)),
        (SID, 23, Some(0)),
        (SID, 23, None),
    ] {
        assert!(Receipt::for_child(&mark, sid, pid, birth, None).is_none());
    }
}

/// A receipt names the exact claim of the record its tab's program holds, and an adopting
/// launch reads it back with the receipt: `claimed <build> <pid> <birth> <at> <n>` or
/// `unclaimed`, the rest of the receipt exactly that child's. A claim line that is neither
/// (an earlier aterm's one-word line under this format's `v3` included), and a build no line
/// of the record could name, give no receipt.
/// NEGATIVE CONTROL: the receipt of another child, or none at all, adopts nothing.
#[test]
fn a_receipt_names_the_claim_its_program_holds() {
    const BUILD: &str = "0.9.0+gabc1234";
    let root = aterm_tempfile::tempdir().unwrap();
    let mark = root.path().join("command-line-tools-tab");
    assert!(
        Receipt::adopt(&mark, SID, 23, Some(45)).is_none(),
        "none written"
    );
    for claimed in [Some(claim(BUILD)), None] {
        let receipt = Receipt::for_child(&mark, SID, 23, Some(45), claimed.as_ref()).unwrap();
        receipt.write_exclusive().unwrap();
        let adopted = Receipt::adopt(&mark, SID, 23, Some(45)).expect("its own receipt");
        assert_eq!(adopted.claimed(), claimed.as_ref());
        assert!(adopted.matches(), "{claimed:?}");
        assert!(
            Receipt::adopt(&mark, SID, 24, Some(45)).is_none(),
            "another child's"
        );
        std::fs::remove_file(&receipt.path).unwrap();
    }
    for (head, line) in [
        (format!("v3\n{SID}\n23\n45\n"), format!("claimed {BUILD}\n")),
        (format!("v3\n{SID}\n23\n45\n"), "claimed\n".to_owned()),
        (
            format!("v3\n{SID}\n23\n45\n"),
            format!("claimed {BUILD} 23 45 x\n"),
        ),
        (
            format!("v3\n{SID}\n23\n45\n"),
            format!("claimed {BUILD} 23 45 1\n"),
        ),
        (
            format!("v3\n{SID}\n23\n45\n"),
            format!("claimed {BUILD} 23 45 1 2 3\n"),
        ),
        (format!("v3\n{SID}\n23\n45\n"), "spent\n".to_owned()),
        (format!("v3\n{SID}\n23\n45\n"), String::new()),
        (format!("v3\n{SID}\n23\n45\n"), "unclaimed".to_owned()),
    ] {
        std::fs::write(
            Receipt::for_child(&mark, SID, 23, Some(45), None)
                .unwrap()
                .path,
            format!("{head}{line}"),
        )
        .unwrap();
        assert!(
            Receipt::adopt(&mark, SID, 23, Some(45)).is_none(),
            "{head:?} {line:?}"
        );
    }
    for unnameable in [
        "",
        "two words",
        &"x".repeat(aterm_types::clt_mark::BUILD_BYTES + 1),
    ] {
        assert!(
            Receipt::for_child(&mark, SID, 23, Some(45), Some(&claim(unnameable))).is_none(),
            "{unnameable:?}"
        );
    }
}

/// An earlier aterm's `v2` receipt of exactly the live child, whose last line names at most
/// the build that claimed, gives custody of its tab with no claim of this build's record, and
/// says what it named ([`Receipt::earlier`]). Carried ([`Receipt::carry`]), it becomes this
/// build's own receipt naming the claim carried, or none, which a later launch adopts as
/// such, and its exit still finds it.
/// NEGATIVE CONTROL: a `v2` receipt of another child, with a last line that is neither, with
/// a build no line could name, or one changed on disk since it was read, gives no custody and
/// is not rewritten.
#[test]
fn an_earlier_aterms_receipt_gives_custody_and_is_carried_as_this_builds() {
    const BUILD: &str = "0.193.0+g5a297ec3fd7c";
    let root = aterm_tempfile::tempdir().unwrap();
    let mark = root.path().join("command-line-tools-tab");
    let head = format!("v2\n{SID}\n23\n45\n");
    for (line, build, carried) in [
        (format!("claimed {BUILD}"), Some(BUILD), Some(claim(BUILD))),
        ("unclaimed".to_owned(), None, Some(claim("0.9.0+gabc1234"))),
        (format!("claimed {BUILD}"), Some(BUILD), None),
    ] {
        let path = Receipt::for_child(&mark, SID, 23, Some(45), None)
            .unwrap()
            .path;
        let _ = std::fs::remove_file(&path);
        Receipt::for_child(&mark, SID, 23, Some(45), None)
            .unwrap()
            .write_exclusive()
            .unwrap();
        std::fs::write(&path, format!("{head}{line}\n")).unwrap();
        assert!(
            Receipt::adopt(&mark, SID, 24, Some(45)).is_none(),
            "another child's"
        );
        let mut adopted = Receipt::adopt(&mark, SID, 23, Some(45)).expect("its tab's custody");
        assert_eq!(
            adopted.earlier(),
            Some(&Earlier {
                build: build.map(str::to_owned)
            }),
            "{line}"
        );
        assert_eq!(adopted.claimed(), None, "{line}");
        assert!(
            adopted.matches() && adopted.owns(SID, 23, Some(45)),
            "{line}"
        );
        adopted.carry(carried.as_ref()).unwrap();
        assert_eq!(adopted.earlier(), None, "{line}");
        assert_eq!(adopted.claimed(), carried.as_ref(), "{line}");
        let again = Receipt::adopt(&mark, SID, 23, Some(45)).expect("this build's own now");
        assert_eq!(again.earlier(), None, "{line}");
        assert_eq!(again.claimed(), carried.as_ref(), "{line}");
        assert_eq!(
            std::fs::read_dir(&adopted.directory).unwrap().count(),
            1,
            "{line}: no staged file left beside it"
        );
        adopted.remove_if_matching();
        assert!(!path.exists(), "{line}: its exit still finds it");
    }
    let path = Receipt::for_child(&mark, SID, 23, Some(45), None)
        .unwrap()
        .path;
    for line in [
        "claimed".to_owned(),
        format!("claimed {BUILD} 23"),
        "spent".to_owned(),
        format!(
            "claimed {}",
            "x".repeat(aterm_types::clt_mark::BUILD_BYTES + 1)
        ),
    ] {
        std::fs::write(&path, format!("{head}{line}\n")).unwrap();
        assert!(
            Receipt::adopt(&mark, SID, 23, Some(45)).is_none(),
            "{line:?}"
        );
    }
    std::fs::write(&path, format!("{head}unclaimed\n")).unwrap();
    let mut stale = Receipt::adopt(&mark, SID, 23, Some(45)).unwrap();
    std::fs::write(&path, format!("{head}claimed {BUILD}\n")).unwrap();
    assert_eq!(
        stale.carry(Some(&claim(BUILD))).unwrap_err().kind(),
        io::ErrorKind::InvalidData
    );
    assert_eq!(
        std::fs::read_to_string(&path).unwrap(),
        format!("{head}claimed {BUILD}\n"),
        "changed since it was read: left as found"
    );
}

#[test]
fn malformed_and_nonregular_receipts_neither_match_nor_get_replaced() {
    let root = aterm_tempfile::tempdir().unwrap();
    let mark = root.path().join("command-line-tools-tab");
    let receipt = Receipt::for_child(&mark, SID, 23, Some(45), None).unwrap();
    std::fs::create_dir(&receipt.directory).unwrap();
    let owner = root.path().join("owner");
    std::fs::write(&owner, &receipt.text).unwrap();
    for kind in ["partial", "oversized", "link", "directory"] {
        match kind {
            "partial" => std::fs::write(&receipt.path, b"v1\n").unwrap(),
            "oversized" => std::fs::write(&receipt.path, vec![b'x'; MAX_BYTES + 1]).unwrap(),
            "link" => std::os::unix::fs::symlink(&owner, &receipt.path).unwrap(),
            _ => std::fs::create_dir(&receipt.path).unwrap(),
        }
        assert!(!receipt.matches(), "{kind}");
        assert!(receipt.write_exclusive().is_err(), "{kind}");
        receipt.remove_if_matching();
        assert!(std::fs::symlink_metadata(&receipt.path).is_ok(), "{kind}");
        if kind == "directory" {
            std::fs::remove_dir(&receipt.path).unwrap();
        } else {
            std::fs::remove_file(&receipt.path).unwrap();
        }
    }
    assert_eq!(std::fs::read_to_string(&owner).unwrap(), receipt.text);
}

#[test]
fn other_writers_or_a_link_at_the_recovery_directory_give_no_custody() {
    use std::os::unix::fs::PermissionsExt as _;
    let root = aterm_tempfile::tempdir().unwrap();
    let mark = root.path().join("command-line-tools-tab");
    let receipt = Receipt::for_child(&mark, SID, 23, Some(45), None).unwrap();
    receipt.write_exclusive().unwrap();
    for mode in [0o770, 0o777] {
        std::fs::set_permissions(&receipt.directory, std::fs::Permissions::from_mode(mode))
            .unwrap();
        assert!(!receipt.matches());
        assert_eq!(
            receipt.write_exclusive().unwrap_err().kind(),
            io::ErrorKind::PermissionDenied
        );
        receipt.remove_if_matching();
        assert_eq!(
            std::fs::read_to_string(&receipt.path).unwrap(),
            receipt.text
        );
    }
    std::fs::set_permissions(&receipt.directory, std::fs::Permissions::from_mode(0o755)).unwrap();
    assert!(
        receipt.matches(),
        "a non-writable owned directory is sufficient"
    );
    receipt.remove_if_matching();
    std::fs::remove_dir(&receipt.directory).unwrap();
    let elsewhere = root.path().join("elsewhere");
    std::fs::create_dir(&elsewhere).unwrap();
    std::os::unix::fs::symlink(&elsewhere, &receipt.directory).unwrap();
    assert!(receipt.write_exclusive().is_err());
    assert!(!receipt.matches());
    assert_eq!(std::fs::read_dir(&elsewhere).unwrap().count(), 0);
}

/// A launch reads back, whole, each receipt whose child no longer runs, and none whose child
/// does; a dot-named file, a stranger's name and a partial receipt are no receipt. This
/// process runs at the birth [`alive`] reads, and at no other.
/// NEGATIVE CONTROL: the mutant whose look skips `alive` reads the running child's receipt as
/// gone, which its adoption still needs.
#[test]
fn a_receipt_whose_child_is_gone_is_an_orphan() {
    const BUILD: &str = "0.9.0+gabc1234";
    const OTHER: &str = "s-fedcba9876543210fedc";
    let root = aterm_tempfile::tempdir().unwrap();
    let mark = root.path().join("command-line-tools-tab");
    assert!(
        Receipt::orphans(&mark, |_, _| false).is_empty(),
        "no folder"
    );
    let gone = Receipt::for_child(&mark, SID, 23, Some(45), Some(&claim(BUILD))).unwrap();
    gone.write_exclusive().unwrap();
    let running = Receipt::for_child(&mark, OTHER, 24, Some(46), None).unwrap();
    running.write_exclusive().unwrap();
    let directory = gone.directory.clone();
    std::fs::write(directory.join(format!(".{SID}-23-45.claim-7")), &gone.text).unwrap();
    std::fs::write(directory.join("notes"), b"v2\n").unwrap();
    std::fs::write(directory.join(format!("{OTHER}-25-47")), b"partial\n").unwrap();

    let orphans = Receipt::orphans(&mark, |pid, birth| (pid, birth) == (24, 46));
    assert_eq!(orphans.len(), 1, "{orphans:?}");
    assert_eq!(orphans[0].text, gone.text);
    assert_eq!(orphans[0].claimed(), Some(&claim(BUILD)));
    assert!(orphans[0].matches());
    assert_eq!(Receipt::orphans(&mark, |_, _| true).len(), 0, "all running");

    let own = i32::try_from(std::process::id()).unwrap();
    let birth = aterm_pty::process_birth(own).expect("this process's birth");
    assert!(alive(own, birth));
    assert!(!alive(own, birth + 1), "another process at this pid");
}
