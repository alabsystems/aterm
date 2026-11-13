// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

/// UNDER A SCRATCH `HOME` THE PROBE OF THIS MACHINE IS REFUSED: its
/// controls would stat and open the account's protected entries; the
/// replica alone runs.
#[test]
fn under_a_scratch_home_only_the_replica_is_probed() {
    let top = aterm_tempfile::Builder::new()
        .prefix("lab-probe-homes-")
        .tempdir()
        .unwrap();
    let (account, scratch) = (top.path().join("account"), top.path().join("scratch"));
    for d in [&account, &scratch] {
        fs::create_dir_all(d).unwrap();
    }
    let agent = crate::paths::Paths::resolve_with(Some(account.clone()), Some(scratch.into()));
    let why = machine_probe_refused(&agent, false).expect("refused under a scratch HOME");
    assert!(why.contains("--replica"), "{why}");
    assert_eq!(machine_probe_refused(&agent, true), None);
    let owner = crate::paths::Paths::resolve_with(Some(account.clone()), Some(account.into()));
    assert_eq!(machine_probe_refused(&owner, false), None);
    assert_eq!(machine_probe_refused(&owner, true), None);
}

fn world(replica: bool) -> World {
    World {
        name: "w".into(),
        replica,
        home: "/H".into(),
        caller_home: "/caller-H".into(),
        cc: "/C".into(),
        run: "/R".into(),
        sockdir: "/S".into(),
        jail: "/R/jail.sb".into(),
        claude: "/claude".into(),
        codex: "/H/Library/Application Support/aterm/pkg/store/codex/7/bin/codex".into(),
        shim: "/shim".into(),
        state: "/S/state".into(),
        lab_dirs: vec!["/L".into()],
        aterm: "/A/aterm".into(),
        usertmp: "/T".into(),
        build_dirs: Vec::new(),
    }
}

/// NEGATIVE CONTROL: `Serve(None)` and `Serve(Some(""))` stay distinct across `encode`/`decode`,
/// fields holding tabs, newlines, carriage returns, or backslashes round-trip through the
/// line-framed spec file without splitting or spoofing operations, and malformed frames or
/// invalid backslash escapes decode as `None`.
#[test]
fn every_operation_crosses_into_the_jail_unchanged() {
    for c in plan(&world(true), "/tmp/at.x/a.sock", &["/tmp/at.y".into()]) {
        assert_eq!(
            Op::decode(&c.op.encode()).as_ref(),
            Some(&c.op),
            "{}",
            c.label
        );
    }
    let edge_ops = [
        Op::ShmRead("/apple.cfprefs.owned-fixture".into()),
        Op::Serve(None),
        Op::Serve(Some(String::new())),
        Op::Serve(Some("/tmp/dir\twith\ttabs\nand\\slashes\r".into())),
        Op::Reach("/H/weird\t1\nudp\r\\file".into(), true),
        Op::Write("/H/a\\t\\nb".into(), WriteHow::Replace),
        Op::PreferencePolicy("scope\t1".into(), "domain\n2\\3".into()),
        Op::Exec("recorder".into(), "/shim\tdir\nnext".into()),
    ];
    let spec_file: String = edge_ops
        .iter()
        .map(|op| format!("{}\n", op.encode()))
        .collect();
    let decoded: Vec<Option<Op>> = spec_file.lines().map(Op::decode).collect();
    assert_eq!(decoded, edge_ops.into_iter().map(Some).collect::<Vec<_>>());
    for malformed in [
        "",
        "reach\t/p",
        "reach\t/p\t2",
        "reach\t/p\t1\textra",
        "reach\t/p\\x\t1",
        "reach\t/p\\\t1",
        "unix\t/p\ttrue",
        "meta\t/p\tx",
        "udp\textra",
        "udp\nserve",
        "udp\rserve",
        "tiocsti\textra",
        "serve\t/d\textra",
    ] {
        assert_eq!(Op::decode(malformed), None, "{malformed:?}");
    }
}

#[test]
fn the_replica_plan_covers_every_protected_path_and_every_mach_service() {
    let p = plan(&world(true), "/d", &[]);
    let data = p
        .iter()
        .filter(|c| {
            matches!(c.op, Op::Reach(..) | Op::Write(..)) && c.label.starts_with("<replica>")
        })
        .count();
    assert!(data >= protected_paths("/H", "/C").len() * 2);
    assert_eq!(
        p.iter().filter(|c| matches!(c.op, Op::Mach(_))).count(),
        MACH_DENY.len() + MACH_DENY_OPEN.len()
    );
    assert!(p.iter().any(|c| c.label.starts_with("<replica>/.ssh")));
    for n in SCRATCH_DIRS {
        assert!(
            p.iter()
                .any(|c| c.label
                    == format!("move scratch/{n} (the lab writes and reads through it)")),
            "{n}"
        );
    }
    assert!(p.iter().any(
        |c| c.label == "list <replica>/Library/Application Support/aterm" && c.want == Want::Denied
    ));
    assert!(p.iter().any(|c| c.label
        == "stat <replica>/Library/Application Support/aterm/pkg/store/codex (metadata only)"));
}

#[test]
fn a_denial_counts_only_where_the_control_got_through() {
    let denied = Check {
        label: "l".into(),
        want: Want::Denied,
        op: Op::Udp,
        control: true,
        absent_ok: true,
    };
    assert_eq!(
        judge(false, &denied, Some("EPERM"), Some("yes")).0,
        Status::Ok
    );
    assert_eq!(judge(false, &denied, Some("EPERM"), None).0, Status::Ok);
    assert_eq!(
        judge(false, &denied, Some("EPERM"), Some("EPERM")).0,
        Status::Unproven
    );
    assert_eq!(
        judge(true, &denied, Some("EPERM"), Some("EACCES")).0,
        Status::Fail
    );
    assert_eq!(
        judge(false, &denied, Some("ENOENT"), Some("ENOENT")).0,
        Status::Absent
    );
    assert_eq!(
        judge(true, &denied, Some("ENOENT"), Some("ENOENT")).0,
        Status::Fail
    );
    assert_eq!(
        judge(false, &denied, Some("yes"), Some("yes")).0,
        Status::Fail
    );
    let allowed = Check {
        want: Want::Allowed,
        ..denied
    };
    assert_eq!(judge(false, &allowed, Some("yes"), None).0, Status::Ok);
    assert_eq!(judge(false, &allowed, Some("EPERM"), None).0, Status::Fail);
}
