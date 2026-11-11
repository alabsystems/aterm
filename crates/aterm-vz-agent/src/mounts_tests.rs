// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

fn share(name: &str, path: &str) -> Share {
    Share {
        name: name.into(),
        path: path.as_bytes().to_vec(),
    }
}

const BOOTED: &str = "\
25 1 253:0 / / rw,relatime shared:1 - ext4 /dev/vda rw
26 25 0:5 / /proc rw,nosuid shared:12 - proc proc rw
40 25 0:40 / /mnt/aterm-boot ro,relatime shared:20 - virtiofs aterm-boot ro
41 25 0:41 / /mnt/aterm-rosetta ro,relatime shared:21 - virtiofs rosetta ro
";

#[test]
fn mountinfo_lines_are_read_with_their_escapes() {
    let text = format!(
        "{BOOTED}50 25 0:42 /repo /Users//me/My\\040Work/aterm ro,relatime shared:30 - virtio\\146s aterm\\040tag ro\nnot a line\n"
    );
    let m = parse(&text);
    assert_eq!(m.len(), 5);
    assert_eq!(
        m[4],
        Mounted {
            root: "/repo".into(),
            point: "/Users//me/My Work/aterm".into(),
            fstype: "virtiofs".into(),
            source: "aterm tag".into(),
        }
    );
    assert_eq!(m[2].source, "aterm-boot");
    assert_eq!(unescape("a\\134b\\011c"), PathBuf::from("a\\b\tc"));
    assert_eq!(unescape("bad\\9x\\04"), PathBuf::from("bad\\9x\\04"));
}

#[test]
fn a_first_mount_mounts_the_share_then_binds_each_read_only_shallowest_first() {
    let ops = plan(
        &parse(BOOTED),
        &[
            share("git", "/Users//me/aterm/.git"),
            share("repo", "/Users//me/aterm"),
        ],
    )
    .unwrap();
    let p = PathBuf::from;
    assert_eq!(
        ops,
        [
            Op::Mkdir(p("/mnt/aterm")),
            Op::MountShare,
            Op::Mkdir(p("/Users//me/aterm")),
            Op::Bind(p("/mnt/aterm/repo"), p("/Users//me/aterm")),
            Op::ReadOnly(p("/Users//me/aterm")),
            Op::Mkdir(p("/Users//me/aterm/.git")),
            Op::Bind(p("/mnt/aterm/git"), p("/Users//me/aterm/.git")),
            Op::ReadOnly(p("/Users//me/aterm/.git")),
        ]
    );
}

#[test]
fn a_later_mount_detaches_every_earlier_bind_deepest_first_then_mounts_the_share_afresh() {
    let mounted = format!(
        "{BOOTED}\
         60 25 0:42 / /mnt/aterm ro,relatime shared:30 - virtiofs aterm ro\n\
         61 25 0:42 /repo /Users//me/old ro,relatime shared:30 - virtiofs aterm ro\n\
         62 61 0:42 /git /Users//me/old/.git ro,relatime shared:30 - virtiofs aterm ro\n"
    );
    let ops = plan(&parse(&mounted), &[share("repo", "/Users//me/new")]).unwrap();
    let p = PathBuf::from;
    assert_eq!(
        ops,
        [
            Op::Unmount(p("/Users//me/old/.git")),
            Op::Unmount(p("/Users//me/old")),
            Op::Unmount(p("/mnt/aterm")),
            Op::Mkdir(p("/mnt/aterm")),
            Op::MountShare,
            Op::Mkdir(p("/Users//me/new")),
            Op::Bind(p("/mnt/aterm/repo"), p("/Users//me/new")),
            Op::ReadOnly(p("/Users//me/new")),
        ],
        "the boot and rosetta shares are another tag's and stay"
    );

    let stacked = format!(
        "{BOOTED}\
         60 25 0:42 / /mnt/aterm ro,relatime shared:30 - virtiofs aterm ro\n\
         61 60 0:43 / /mnt/aterm ro,relatime shared:31 - virtiofs aterm ro\n\
         62 25 0:42 /repo /Users//me/old ro,relatime shared:30 - virtiofs aterm ro\n\
         63 62 0:43 /repo2 /Users//me/old ro,relatime shared:31 - virtiofs aterm ro\n"
    );
    let stacked_ops = plan(&parse(&stacked), &[]).unwrap();
    assert_eq!(
        stacked_ops,
        [
            Op::Unmount(p("/Users//me/old")),
            Op::Unmount(p("/Users//me/old")),
            Op::Unmount(p("/mnt/aterm")),
            Op::Unmount(p("/mnt/aterm")),
            Op::Mkdir(p("/mnt/aterm")),
            Op::MountShare,
        ],
        "stacked stale binds emit one detach per mount layer"
    );
}

#[test]
fn a_bind_is_believed_only_rooted_at_its_own_entry() {
    let shares = [share("repo-1a", "/Users//me/new")];
    let good = format!(
        "{BOOTED}\
         70 25 0:43 / /mnt/aterm ro,relatime shared:31 - virtiofs aterm ro\n\
         71 25 0:43 /repo-1a /Users//me/new ro,relatime shared:31 - virtiofs aterm ro\n"
    );
    assert_eq!(check(&parse(&good), &shares), Ok(()));
    // The negative controls: a bind through an entry the swap took away, a stale bind
    // under a fresh one (the topmost decides), another entry's, and none at all.
    let deleted = good.replace("/repo-1a /Users", "/repo-1a//deleted /Users");
    let err = check(&parse(&deleted), &shares).unwrap_err();
    assert!(err.contains("/repo-1a//deleted"), "{err}");
    let stacked = format!(
        "{good}72 71 0:43 /repo-0b /Users//me/new ro,relatime shared:31 - virtiofs aterm ro\n"
    );
    let err = check(&parse(&stacked), &shares).unwrap_err();
    assert!(err.contains("shows /repo-0b"), "{err}");
    let fresh_over_stale = format!(
        "{deleted}73 71 0:43 /repo-1a /Users//me/new ro,relatime shared:31 - virtiofs aterm ro\n"
    );
    assert_eq!(check(&parse(&fresh_over_stale), &shares), Ok(()));
    let other_fs = format!(
        "{BOOTED}74 25 253:0 /home /Users//me/new rw,relatime shared:1 - ext4 /dev/vda rw\n"
    );
    let err = check(&parse(&other_fs), &shares).unwrap_err();
    assert!(err.contains("ext4"), "{err}");
    let err = check(&parse(BOOTED), &shares).unwrap_err();
    assert!(err.contains("nothing is mounted at /Users//me/new"), "{err}");
}

#[test]
fn a_share_that_is_not_one_directory_or_not_bindable_is_refused() {
    let m = parse(BOOTED);
    for bad in [
        share("", "/x"),
        share(".", "/x"),
        share("..", "/x"),
        share("a/b", "/x"),
        share("a\0b", "/x"),
        share("/abs", "/x"),
        share("repo", "relative/path"),
        share("repo", "/"),
        share("repo", "/Users/."),
        share("repo", "/Users/./me"),
        share("repo", "/Users/../etc"),
        share("repo", "/Users//me\0x"),
        share("repo", "/mnt/aterm/repo"),
        share("repo", "/mnt/aterm"),
    ] {
        assert!(plan(&m, std::slice::from_ref(&bad)).is_err(), "{bad:?}");
    }
    assert!(
        plan(
            &m,
            &[share("a", "/Users//me/same"), share("b", "/Users//me/same")]
        )
        .is_err(),
        "duplicate target bind paths must be refused"
    );
    assert!(plan(&m, &[]).is_ok());

    #[cfg(unix)]
    {
        let dir = aterm_tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(dir.path()).unwrap();
        let plain = root.join("work/nested");
        ensure_plain_dir(&plain).unwrap();
        ensure_plain_dir(&plain).unwrap();
        assert!(plain.is_dir());

        let target = root.join("elsewhere");
        std::fs::create_dir(&target).unwrap();
        let link = root.join("work/escape");
        std::os::unix::fs::symlink(&target, &link).unwrap();
        let err = ensure_plain_dir(&link.join("child")).unwrap_err();
        assert!(err.contains("is a symbolic link"), "{err}");
        assert!(!target.join("child").exists());

        let file = root.join("work/plain-file");
        std::fs::write(&file, b"x").unwrap();
        let err = ensure_plain_dir(&file.join("child")).unwrap_err();
        assert!(err.contains("is not a directory"), "{err}");
    }
}
