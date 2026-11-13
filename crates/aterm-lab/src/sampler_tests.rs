// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

#[test]
fn loopback_endpoints_are_plain_others_are_external_and_each_names_its_pid() {
    let f = "p1\ncaterm-lab\nPTCP\ntIPv4\nn127.0.0.1:50130\nn127.0.0.1:50130->127.0.0.1:50141\n\
             p2\ncclaude\nPTCP\nn127.0.0.1:50141->127.0.0.1:50130\nn10.0.0.2:5000->160.79.104.10:443 (SYN_SENT)\n\
             PUDP\nn[::1]:53\nn*:*\n";
    let e: Vec<String> = endpoints(f).into_iter().collect();
    assert!(e.contains(&"aterm-lab[1] TCP listen 127.0.0.1:50130".to_string()));
    assert!(e.contains(&"aterm-lab[1] TCP conn 127.0.0.1:50141".to_string()));
    assert!(e.contains(&"claude[2] TCP conn 127.0.0.1:50130".to_string()));
    assert!(e.contains(&"claude[2] TCP conn 160.79.104.10:443  EXTERNAL".to_string()));
    assert!(e.contains(&"claude[2] UDP listen [::1]:53".to_string()));
    assert!(e.contains(&"claude[2] UDP unbound *:*".to_string()));
    let f = "p3\ncaterm\nPUDP\nn*:*\nn*:5353\nPTCP\nn*:8080\n";
    let e: Vec<String> = endpoints(f).into_iter().collect();
    assert_eq!(
        e,
        [
            "aterm[3] TCP listen *:8080",
            "aterm[3] UDP listen *:5353",
            "aterm[3] UDP unbound *:*",
        ]
    );
    // Untrusted lsof lines starting with multi-byte UTF-8 or empty lines never panic.
    let multibyte = "—banner\n\np4\ncλ-daemon\nPTCP\nn127.0.0.1:9000\n✓trailing\n";
    let e: Vec<String> = endpoints(multibyte).into_iter().collect();
    assert_eq!(e, ["λ-daemon[4] TCP listen 127.0.0.1:9000"]);
}

/// The empty directories under `top` that `tmp_cleaner`'s own test
/// (`find -dx . -fstype local ! -name . -type d -empty -mtime +3`) would
/// delete now.
fn cleaner_would_take(top: &Path) -> BTreeSet<std::path::PathBuf> {
    let out = aterm_uds::spawnfd::command("/usr/bin/find")
        .arg("-x")
        .arg(top)
        .args(["-type", "d", "-empty", "-mtime", "+3"])
        .output()
        .expect("find ran");
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .map(std::path::PathBuf::from)
        .collect()
}

/// A LIVE LAB IS KEPT FROM macOS'S `tmp_cleaner`, which clears the
/// `/private/tmp` the default state dir sits in: with every entry's times
/// five days old (judged four days ahead, since a test cannot age a change
/// time), the run dir's own files outside `scratch/` (`env.sh`, the
/// profile, the lab's program) and the socket dir's token get their access
/// time made now, their modification time kept; the directories the
/// profile pins (`scratch/`, its `SCRATCH_DIRS`, the socket dir's `proj`)
/// get their modification time made now, so the cleaner's own `find`
/// would take none of them.
#[test]
fn a_live_lab_is_kept_from_the_tmp_cleaner() {
    use std::os::unix::fs::MetadataExt as _;
    let top = crate::TestScratch::new("keep");
    let run = top.join("state/runs/t");
    let sock = top.join("at.sock");
    let scratch = run.join("scratch");
    let mut dirs = vec![run.join("bin"), scratch.join("unpinned"), sock.join("proj")];
    dirs.extend(
        crate::protected::SCRATCH_DIRS
            .iter()
            .map(|n| scratch.join(n)),
    );
    for d in &dirs {
        std::fs::create_dir_all(d).unwrap();
    }
    let own = [
        run.join("env.sh"),
        run.join("jail.sb"),
        run.join("bin/aterm-lab"),
        sock.join("a.sock.token"),
    ];
    let jailed = scratch.join("home/agent.json");
    let outside = top.join("outside");
    for f in own.iter().chain([&jailed, &outside]) {
        std::fs::write(f, "x\n").unwrap();
    }
    std::os::unix::fs::symlink(&outside, run.join("link")).unwrap();
    let sockdir = sock.display().to_string();
    assert_eq!(keep_from_the_cleaner(&run, &sockdir, SystemTime::now()), 0);
    let old = SystemTime::now() - Duration::from_secs(5 * 24 * 3600);
    let old_s = i64::try_from(
        old.duration_since(SystemTime::UNIX_EPOCH)
            .unwrap()
            .as_secs(),
    )
    .unwrap();
    let times = std::fs::FileTimes::new()
        .set_accessed(old)
        .set_modified(old);
    for f in own.iter().chain([&jailed, &outside]) {
        std::fs::OpenOptions::new()
            .write(true)
            .open(f)
            .unwrap()
            .set_times(times)
            .unwrap();
    }
    for d in dirs.iter().chain([&scratch, &run]) {
        std::fs::File::open(d).unwrap().set_times(times).unwrap();
    }
    let pinned: BTreeSet<std::path::PathBuf> = dirs
        .iter()
        .filter(|d| !d.ends_with("unpinned") && !d.ends_with("bin"))
        .filter(|d| std::fs::read_dir(d).unwrap().next().is_none())
        .cloned()
        .collect();
    assert!(
        pinned.contains(&sock.join("proj")) && pinned.len() > 5,
        "{pinned:?}"
    );
    let before = cleaner_would_take(&top);
    assert!(
        pinned.is_subset(&before) && before.contains(&scratch.join("unpinned")),
        "{before:?}"
    );
    let start = SystemTime::now() - Duration::from_secs(2);
    let start_s = i64::try_from(
        start
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap()
            .as_secs(),
    )
    .unwrap();
    let ahead = SystemTime::now() + Duration::from_secs(4 * 24 * 3600);
    let made = keep_from_the_cleaner(&run, &sockdir, ahead);
    let meta = |p: &Path| std::fs::symlink_metadata(p).unwrap();
    for f in &own {
        let m = meta(f);
        assert!(m.atime() >= start_s, "{} accessed now", f.display());
        assert_eq!(
            m.mtime(),
            old_s,
            "{} keeps its modification time",
            f.display()
        );
    }
    for d in &pinned {
        assert!(meta(d).mtime() >= start_s, "{} made new", d.display());
    }
    assert!(meta(&scratch).mtime() >= start_s);
    let after = cleaner_would_take(&top);
    assert!(pinned.is_disjoint(&after), "{after:?}");
    assert!(made >= own.len() + pinned.len(), "{made}");
    assert_eq!(
        meta(&jailed).atime(),
        old_s,
        "the scratch's files are the jail's"
    );
    assert!(after.contains(&scratch.join("unpinned")), "{after:?}");
    assert_eq!(meta(&outside).atime(), old_s, "no link is followed");
}
