// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::port::{Fake, Ran, Stat};

const BLOB: &str = "AAAAC3NzaC1lZDI1NTE5AAAAIEsLWe7+eYemQuyP3+onlaAlCHNVLq1yojB4CPyPW4yR";

#[test]
fn the_sudoers_rule_names_exactly_apply_pinned_by_digest() {
    let text = sudoers_text("admin", &"ab".repeat(32));
    let rule = text.lines().last().unwrap();
    assert_eq!(
        rule,
        format!(
            "admin ALL=(root) NOPASSWD: sha256:{} /Library/Management/aterm/bin/aterm machines agent apply",
            "ab".repeat(32)
        )
    );
    assert!(!text.contains('*'), "a wildcard would admit install");
    assert!(text.contains("Defaults!/Library/Management/aterm/bin/aterm !env_keep"));
}

#[test]
fn visudo_must_accept_the_drop_in_before_it_is_used() {
    let fake = Fake::root();
    let layout = Layout { root: "/A".into() };
    fake.on(
        "/usr/sbin/visudo -cf /A/stage/sudoers.check",
        Ran {
            status: Some(1),
            stdout: b"/A/stage/sudoers.check:4:58: empty string\n".to_vec(),
            stderr: Vec::new(),
        },
    );
    let err = check_sudoers(&fake, &layout, "admin", &"ab".repeat(32)).unwrap_err();
    assert!(err.contains("empty string"), "{err}");
    assert!(fake.written("/A/stage/sudoers.check").is_none());
    assert!(check_sudoers(&fake, &layout, "ad min", &"ab".repeat(32)).is_err());
    assert!(check_sudoers(&fake, &layout, "admin", "*").is_err());
}

#[test]
fn install_arguments_are_all_required_and_never_repeated() {
    let ok = [
        "--admin",
        "admin",
        "--fleet-key",
        "ssh-ed25519 AAAA fleet@ctl",
        "--allowed-signers",
        "/A/.staging/allowed_signers",
        "--payload-sha",
        "ab",
        "--stage",
        "/A/.staging/payload",
    ];
    let a = parse(&ok).unwrap();
    assert_eq!(a.admin, "admin");
    assert!(parse(&ok[..8]).is_err());
    let mut twice = ok.to_vec();
    twice.extend(["--admin", "root"]);
    assert!(parse(&twice).is_err());
    assert!(parse(&["--shell", "x"]).is_err());
}

#[test]
fn the_fleet_key_line_is_restricted_and_replaces_its_own_earlier_line() {
    let line = fleet_line(&format!("ssh-ed25519 {BLOB} aterm-machines-fleet@ctl")).unwrap();
    assert!(line.starts_with(
        "restrict,command=\"/Library/Management/aterm/bin/aterm machines agent ssh\" ssh-ed25519 "
    ));
    assert!(line.ends_with(" aterm-machines:aterm-machines-fleet@ctl"));
    assert!(fleet_line(&format!("ssh-ed25519 {BLOB} a\"b")).is_err());
    assert!(fleet_line("command=\"/bin/sh\" ssh-ed25519 AAAA").is_err());
    let current = format!(
        "ssh-ed25519 OWNKEYAAAAAAAAAAAAAAAA admin@laptop\nrestrict,command=\"old\" ssh-ed25519 {BLOB} old\n"
    );
    let merged = merge_authorized_keys(&current, &line, BLOB);
    assert_eq!(merged.lines().count(), 2, "{merged}");
    assert!(merged.starts_with("ssh-ed25519 OWNKEY"));
    assert_eq!(merge_authorized_keys(&merged, &line, BLOB), merged);
    assert_eq!(
        strip_authorized_keys(&merged),
        "ssh-ed25519 OWNKEYAAAAAAAAAAAAAAAA admin@laptop\n"
    );
}

fn root_dir() -> Stat {
    Stat {
        uid: 0,
        gid: 0,
        mode: 0o755,
        dir: true,
        symlink: false,
        len: 0,
    }
}

fn admin() -> User {
    User {
        name: "admin".into(),
        uid: 501,
        gid: 20,
        home: "/Users//admin".into(),
        shell: "/bin/zsh".into(),
        admin: true,
    }
}

fn staged() -> (Fake, Layout, Args) {
    let fake = Fake::root();
    let layout = Layout {
        root: "/Library/Management/aterm".into(),
    };
    for d in [
        "/",
        "/Library",
        "/Library/Management",
        "/Library/Management/aterm",
        "/Library/Management/aterm/.staging",
        "/Library/Management/aterm/.staging/payload",
        "/Library/Management/aterm/bin",
        "/Library/Management/aterm/state",
        "/Library/Management/aterm/state/runs",
        "/Library/Management/aterm/cache",
        "/Library/Management/aterm/stage",
        "/Library/Management/aterm/inbox",
        "/Library/Management/aterm/agent",
    ] {
        fake.stat_of(d, root_dir());
    }
    fake.file(
        "/Library/Management/aterm/.staging/payload/aterm-machines-dev",
        b"agent",
    );
    fake.stat_of(
        "/Library/Management/aterm/.staging/payload/aterm-machines-dev",
        Stat {
            dir: false,
            ..root_dir()
        },
    );
    fake.stat_of(
        "/Library/Management/aterm/agent/aterm-machines-dev",
        Stat {
            dir: false,
            ..root_dir()
        },
    );
    fake.file(
        "/Library/Management/aterm/.staging/allowed_signers",
        format!("signing@ctl namespaces=\"aterm-machines\" ssh-ed25519 {BLOB}\n").as_bytes(),
    );
    fake.file(
        "/Library/Management/aterm/.staging/payload.zip",
        b"the archive",
    );
    let payload_sha = fake
        .sha256(Path::new("/Library/Management/aterm/.staging/payload.zip"))
        .unwrap();
    let args = Args {
        admin: "admin".into(),
        fleet_key: format!("ssh-ed25519 {BLOB} aterm-machines-fleet@ctl"),
        allowed_signers: "/Library/Management/aterm/.staging/allowed_signers".into(),
        payload_sha,
        stage: "/Library/Management/aterm/.staging/payload".into(),
    };
    (fake, layout, args)
}

fn script_install(fake: &Fake) {
    for c in [
        "/usr/sbin/chown -R root:wheel /Library/Management/aterm/.staging/payload",
        "/bin/chmod -R go-w /Library/Management/aterm/.staging/payload",
        "/bin/chmod 0755 /Library/Management/aterm/.staging/payload",
    ] {
        fake.on(c, Ran::ok(""));
    }
    fake.on(
        "/usr/sbin/visudo -cf /Library/Management/aterm/stage/sudoers.check",
        Ran::ok("/Library/Management/aterm/stage/sudoers.check: parsed OK\n"),
    );
    fake.on(
        "admin$ /bin/cat /Users//admin/.ssh/authorized_keys",
        Ran::exit(
            1,
            "cat: /Users//admin/.ssh/authorized_keys: No such file or directory",
        ),
    );
    fake.on(
        "admin$ /Library/Management/aterm/bin/aterm machines agent put-file /Users//admin/.ssh/authorized_keys 600",
        Ran::ok("ok /Users//admin/.ssh/authorized_keys\n"),
    );
}

const STAGED_EXE: &str = "/Library/Management/aterm/.staging/payload/aterm-machines-dev";

#[test]
fn install_refuses_an_archive_it_cannot_hash_or_that_hashes_wrong() {
    let (fake, layout, args) = staged();
    script_install(&fake);
    let mut wrong = args.clone();
    wrong.payload_sha = "ab".repeat(32);
    let mut out = Vec::new();
    let err = install(
        &fake,
        &layout,
        Path::new(STAGED_EXE),
        &wrong,
        &admin(),
        &mut out,
    )
    .unwrap_err();
    assert!(err.contains("hashes to"), "{err}");
    let _ = fake.remove(Path::new("/Library/Management/aterm/.staging/payload.zip"));
    let err = install(
        &fake,
        &layout,
        Path::new(STAGED_EXE),
        &args,
        &admin(),
        &mut out,
    )
    .unwrap_err();
    assert!(err.contains("payload.zip"), "{err}");
    assert!(!fake.calls().iter().any(|c| c.starts_with("rename")));
}

#[test]
fn enrollment_sweeps_each_upgrade_folder_left_behind_whose_hold_is_spent() {
    let (fake, layout, args) = staged();
    script_install(&fake);
    let stage = "/Library/Management/aterm/stage";
    fake.awake_at(10_000_000);
    for (name, stamp) in [("upgrade-1-3", "1000000"), ("upgrade-1-5", "9900000")] {
        let dir = format!("{stage}/{name}");
        fake.file(&dir, b"").stat_of(&dir, root_dir());
        fake.file(&format!("{dir}/aterm"), b"agent");
        fake.file(&format!("{dir}/exec"), stamp.as_bytes());
    }
    let mut out = Vec::new();
    install(
        &fake,
        &layout,
        Path::new(STAGED_EXE),
        &args,
        &admin(),
        &mut out,
    )
    .unwrap_or_else(|e| panic!("{e}\n{}", String::from_utf8_lossy(&out)));
    assert!(
        fake.written(&format!("{stage}/upgrade-1-3/aterm"))
            .is_none(),
        "its hold is spent"
    );
    assert!(
        fake.written(&format!("{stage}/upgrade-1-5/aterm"))
            .is_some(),
        "a later upgrade's sweep takes it"
    );
}

#[test]
fn a_reinstall_starts_a_renamed_principal_at_the_highest_floor() {
    let (fake, layout, args) = staged();
    script_install(&fake);
    fake.file(
        "/Library/Management/aterm/state/floors.toml",
        b"[[floor]]\nprincipal = \"old@ctl\"\nserial = 40\n",
    );
    let mut out = Vec::new();
    install(
        &fake,
        &layout,
        Path::new(STAGED_EXE),
        &args,
        &admin(),
        &mut out,
    )
    .unwrap_or_else(|e| panic!("{e}\n{}", String::from_utf8_lossy(&out)));
    let floors = state::floors_or_refuse(&fake, &layout).unwrap();
    assert_eq!(state::floor_of(&floors, "signing@ctl"), 40);
}

#[test]
fn install_puts_the_staged_agent_in_place_and_writes_only_root_files_as_root() {
    let (fake, layout, args) = staged();
    for c in [
        "/usr/sbin/chown -R root:wheel /Library/Management/aterm/.staging/payload",
        "/bin/chmod -R go-w /Library/Management/aterm/.staging/payload",
        "/bin/chmod 0755 /Library/Management/aterm/.staging/payload",
    ] {
        fake.on(c, Ran::ok(""));
    }
    fake.on(
        "/usr/sbin/visudo -cf /Library/Management/aterm/stage/sudoers.check",
        Ran::ok("/Library/Management/aterm/stage/sudoers.check: parsed OK\n"),
    );
    fake.on(
        "admin$ /bin/cat /Users//admin/.ssh/authorized_keys",
        Ran::exit(
            1,
            "cat: /Users//admin/.ssh/authorized_keys: No such file or directory",
        ),
    );
    let pid = std::process::id();
    fake.on(
        "admin$ /Library/Management/aterm/bin/aterm machines agent put-file /Users//admin/.ssh/authorized_keys 600",
        Ran::ok("ok /Users//admin/.ssh/authorized_keys\n"),
    );
    let exe = Path::new("/Library/Management/aterm/.staging/payload/aterm-machines-dev");
    let mut out = Vec::new();
    install(&fake, &layout, exe, &args, &admin(), &mut out)
        .unwrap_or_else(|e| panic!("{e}\n{}", String::from_utf8_lossy(&out)));
    let calls = fake.calls();
    assert!(
        calls.contains(
            &"rename /Library/Management/aterm/.staging/payload /Library/Management/aterm/agent"
                .to_owned()
        ),
        "{calls:#?}"
    );
    assert!(calls.iter().any(|c| c == "symlink /Library/Management/aterm/bin/.aterm.new -> /Library/Management/aterm/agent/aterm-machines-dev"), "{calls:#?}");
    let check = calls
        .iter()
        .position(|c| c.starts_with("/usr/sbin/visudo -cf"))
        .unwrap();
    let write = calls
        .iter()
        .position(|c| c == "write /etc/sudoers.d/aterm-machines 440")
        .unwrap();
    assert!(check < write);
    let sudoers =
        String::from_utf8(fake.written("/etc/sudoers.d/aterm-machines").unwrap()).unwrap();
    let sha = fake
        .sha256(Path::new(
            "/Library/Management/aterm/agent/aterm-machines-dev",
        ))
        .unwrap();
    assert!(
        sudoers.contains(&format!(
            "NOPASSWD: sha256:{sha} /Library/Management/aterm/bin/aterm machines agent apply"
        )),
        "{sudoers}"
    );
    assert_eq!(
        fake.mode_of("/Library/Management/aterm/allowed_signers"),
        Some(0o644)
    );
    assert!(fake.written("/Users//admin/.ssh/authorized_keys").is_none());
    assert!(
        calls.iter().all(|c| !c.starts_with("write /Users/")),
        "{calls:#?}"
    );
    assert!(
        fake.written(&format!("/Library/Management/aterm/stage/put-file.{pid}"))
            .is_none()
    );
    let managed = state::managed(&fake, &layout).unwrap();
    assert_eq!(managed.admin, "admin");
    assert_eq!(managed.executable, "agent/aterm-machines-dev");
}

#[test]
fn uninstall_reports_every_step_before_it_removes_the_agent_itself() {
    let fake = Fake::root();
    let layout = Layout {
        root: "/Library/Management/aterm".into(),
    };
    fake.file("/Library/Management/aterm/managed.toml", b"schema = 1\nadmin = \"admin\"\npayload_sha256 = \"\"\nagent_version = \"\"\nexecutable = \"agent/x\"\ninstalled = 0\n");
    fake.file("/etc/sudoers.d/aterm-machines", b"rule");
    fake.stat_of("/Library/Management/aterm", root_dir());
    let opened = Opened {
        envelope: crate::envelope::parse_body(
            b"schema = 1\nhost = \"U\"\nhost_name = \"h\"\nserial = 9\nissued = 1\nexpires = 2\naction = \"uninstall\"\n",
        )
        .unwrap(),
        principal: "signing@ctl".into(),
        key_id: String::new(),
    };
    let mut out = Vec::new();
    let _ = uninstall(
        &fake,
        &layout,
        Path::new("/Library/Management/aterm/bin/aterm"),
        &opened,
        "1-9",
        &mut out,
    );
    let calls = fake.calls();
    assert!(
        calls.contains(&"remove /etc/sudoers.d/aterm-machines".to_owned()),
        "{calls:#?}"
    );
    assert_eq!(
        calls.last().map(String::as_str),
        Some("remove /Library/Management/aterm")
    );
    let text = String::from_utf8(out).unwrap();
    let last: Line = aterm_json::from_str(text.lines().last().unwrap()).unwrap();
    let Line::Report(report) = last else {
        panic!("{text}");
    };
    let ids: Vec<&str> = report.items.iter().map(|r| r.id.as_str()).collect();
    assert!(ids.contains(&"uninstall.sudoers"), "{ids:?}");
    assert!(ids.contains(&"uninstall.authorized-keys"), "{ids:?}");
    assert!(ids.contains(&"uninstall.inbox"), "{ids:?}");
    assert!(!ids.contains(&"uninstall.unproven"), "{ids:?}");
    assert_eq!(report.serial, 9);
}

/// The agent root of [`seed_held_folders`].
const HELD_ROOT: &str = "/Library/Management/aterm";

/// An agent root holding enrollment's stage, three upgrade folders and the aterm.app's
/// first-run record, of which `.staging` (a stamp with no reading), the record (an
/// aterm.app started 50 seconds ago) and `stage/upgrade-1-5` (a program started 100
/// seconds ago) have not shown they ran; its uninstall request.
fn seed_held_folders(fake: &Fake) -> Opened {
    let root = HELD_ROOT;
    fake.file(&format!("{root}/managed.toml"), b"schema = 1\nadmin = \"admin\"\npayload_sha256 = \"\"\nagent_version = \"\"\nexecutable = \"agent/x\"\ninstalled = 0\n");
    fake.stat_of(root, root_dir());
    for dir in [
        "agent",
        "stage",
        ".staging",
        "stage/upgrade-1-3",
        "stage/upgrade-1-4",
        "stage/upgrade-1-5",
    ] {
        let dir = format!("{root}/{dir}");
        fake.file(&dir, b"").stat_of(&dir, root_dir());
        fake.file(&format!("{dir}/aterm"), b"agent");
    }
    for dir in ["state", "state/aterm.app-first-run"] {
        let dir = format!("{root}/{dir}");
        fake.file(&dir, b"").stat_of(&dir, root_dir());
    }
    fake.file(&format!("{root}/state/journal.ndjson"), b"{}\n");
    fake.file(
        &format!("{root}/state/aterm.app-first-run/exec"),
        b"9950000",
    );
    fake.awake_at(10_000_000);
    fake.file(&format!("{root}/stage/upgrade-1-3/exec"), b"1000000");
    fake.file(&format!("{root}/stage/upgrade-1-4/exec"), b"9900000");
    fake.file(&format!("{root}/stage/upgrade-1-4/ran"), b"");
    fake.file(&format!("{root}/stage/upgrade-1-5/exec"), b"9900000");
    fake.file(&format!("{root}/.staging/exec"), b"");
    fake.file(&format!("{root}/stage/aterm-open-1.sig"), b"sig");
    Opened {
        envelope: crate::envelope::parse_body(
            b"schema = 1\nhost = \"U\"\nhost_name = \"h\"\nserial = 9\nissued = 1\nexpires = 2\naction = \"uninstall\"\n",
        )
        .unwrap(),
        principal: "signing@ctl".into(),
        key_id: String::new(),
    }
}

/// Run uninstall on [`seed_held_folders`], checking the kept and removed paths; its
/// `uninstall.unproven` row.
fn uninstall_with_held_folders(fake: &Fake) -> ItemReport {
    let root = HELD_ROOT;
    let layout = Layout { root: root.into() };
    let opened = seed_held_folders(fake);
    let mut out = Vec::new();
    let _ = uninstall(
        fake,
        &layout,
        Path::new("/Library/Management/aterm/bin/aterm"),
        &opened,
        "1-9",
        &mut out,
    );
    for kept in ["stage/upgrade-1-5", ".staging"] {
        assert!(
            fake.written(&format!("{root}/{kept}/aterm")).is_some(),
            "{kept}: {:#?}",
            fake.calls()
        );
    }
    assert!(
        fake.written(&format!("{root}/state/aterm.app-first-run/exec"))
            .is_some(),
        "the record that holds /Applications/aterm.app: {:#?}",
        fake.calls()
    );
    for gone in ["agent", "stage/upgrade-1-3", "stage/upgrade-1-4"] {
        assert!(
            fake.written(&format!("{root}/{gone}/aterm")).is_none(),
            "{gone}: an aged stamp, a proof, or no stamp at all"
        );
    }
    assert!(
        fake.written(&format!("{root}/state/journal.ndjson"))
            .is_none()
    );
    assert!(
        fake.written(&format!("{root}/stage/aterm-open-1.sig"))
            .is_none(),
        "a file is no folder a program was started from"
    );
    assert!(fake.written(&format!("{root}/managed.toml")).is_none());
    let calls = fake.calls();
    for holder in [
        root.to_owned(),
        format!("{root}/stage"),
        format!("{root}/state"),
    ] {
        assert!(!calls.contains(&format!("remove {holder}")), "{calls:#?}");
    }
    let text = String::from_utf8(out).unwrap();
    let Line::Report(report) = aterm_json::from_str(text.lines().last().unwrap()).unwrap() else {
        panic!("{text}");
    };
    let row = report
        .items
        .iter()
        .find(|r| r.id == "uninstall.unproven")
        .unwrap_or_else(|| panic!("{:#?}", report.items));
    let why = row.why.as_deref().unwrap();
    assert!(
        why.contains(&format!(
            "{root}/.staging {root}/state/aterm.app-first-run {root}/stage/upgrade-1-5 stay"
        )) && why.contains("10 more minute(s)")
            && !why.contains("aterm-open-1.sig"),
        "{why}"
    );
    row.clone()
}

#[test]
fn an_unreadable_hold_inventory_keeps_the_agent_root_even_when_the_error_passes() {
    let root = HELD_ROOT;
    for (list, path) in [
        (true, format!("{root}/stage")),
        (false, format!("{root}/stage")),
        (false, format!("{root}/stage/upgrade-1-5")),
        (false, format!("{root}/.staging")),
    ] {
        let fake = Fake::root();
        let opened = seed_held_folders(&fake);
        if list {
            fake.list_fails_once(&path, std::io::ErrorKind::PermissionDenied);
        } else {
            fake.stat_fails_once(&path, std::io::ErrorKind::PermissionDenied);
        }
        let mut out = Vec::new();
        assert_eq!(
            uninstall(
                &fake,
                &Layout { root: root.into() },
                Path::new("/Library/Management/aterm/bin/aterm"),
                &opened,
                "1-9",
                &mut out,
            ),
            ExitCode::FAILURE,
        );
        let text = String::from_utf8(out).unwrap();
        let Line::Report(report) = aterm_json::from_str(text.lines().last().unwrap()).unwrap()
        else {
            panic!("{text}");
        };
        let row = report
            .items
            .iter()
            .find(|r| r.id == "uninstall.unproven")
            .unwrap();
        assert_eq!(row.outcome, Outcome::Failed);
        assert!(row.why.as_deref().unwrap().contains(&path));
        for file in ["agent/aterm", ".staging/aterm", "stage/upgrade-1-5/aterm"] {
            assert_eq!(
                fake.written(&format!("{root}/{file}")),
                Some(b"agent".to_vec())
            );
        }
        assert_eq!(
            fake.written(&format!("{root}/.staging/exec")),
            Some(Vec::new())
        );
        assert_eq!(
            fake.written(&format!("{root}/stage/upgrade-1-5/exec")),
            Some(b"9900000".to_vec())
        );
        assert!(fake.written(&format!("{root}/managed.toml")).is_some());
        let calls = fake.calls();
        assert!(
            !calls
                .iter()
                .any(|c| c.starts_with(&format!("remove {root}"))),
            "{calls:#?}"
        );
        assert!(
            !calls
                .iter()
                .any(|c| c.starts_with("/bin/launchctl bootstrap ")),
            "{calls:#?}"
        );
        if list {
            assert!(!fake.list_dir(Path::new(&path)).unwrap().is_empty());
        } else {
            assert!(fake.stat(Path::new(&path)).unwrap().dir);
        }
    }
}

#[test]
fn uninstall_leaves_each_held_folder_to_a_one_time_launchd_job_that_outlives_the_agent() {
    let fake = Fake::root();
    let root = HELD_ROOT;
    let boot_id = "55C317A3-F793-4F0D-BE04-262D2C6ED332";
    let clock = |sec: u64| format!("{{ sec = {sec}, usec = 0 }} Thu Oct 22 09:00:00 2026\n");
    fake.on(
        "/usr/sbin/sysctl -n kern.bootsessionuuid kern.boottime kern.waketime",
        Ran::ok(&format!("{boot_id}\n{}{}", clock(1_799_000_000), clock(0))),
    )
    .on(
        "/usr/sbin/sysctl -n kern.boottime",
        Ran::ok(&clock(1_799_000_000)),
    );
    let label = "systems.alab.aterm-machines.reclaim.1-9";
    let plist = format!("/Library/LaunchDaemons/{label}.plist");
    fake.on(
        &format!("/bin/launchctl bootstrap system {plist}"),
        Ran::ok(""),
    );
    let row = uninstall_with_held_folders(&fake);
    let doc = fake
        .written(&plist)
        .unwrap_or_else(|| panic!("the negative control: no job: {:#?}", fake.calls()));
    let crate::model::PValue::Dict(job) = crate::plist::parse(&doc).unwrap() else {
        panic!("{}", String::from_utf8_lossy(&doc));
    };
    let argv: Vec<String> = job
        .iter()
        .find(|(key, _)| key == "ProgramArguments")
        .and_then(|(_, argv)| match argv {
            crate::model::PValue::Array(argv) => Some(argv.clone()),
            _ => None,
        })
        .unwrap()
        .into_iter()
        .map(|arg| match arg {
            crate::model::PValue::Str(arg) => arg,
            other => panic!("{other:?}"),
        })
        .collect();
    let expected: Vec<String> = [label, "1-9", boot_id, "600"]
        .map(str::to_owned)
        .into_iter()
        .chain([
            format!("{root}/.staging"),
            format!("{root}/state/aterm.app-first-run"),
            format!("{root}/stage/upgrade-1-5"),
        ])
        .collect();
    assert_eq!(
        argv[3..],
        expected,
        "the longest hold left, and the boot it is held in"
    );
    for kept in [".staging", "state/aterm.app-first-run", "stage/upgrade-1-5"] {
        assert_eq!(
            fake.written(&format!("{root}/{kept}/{}", super::super::reclaim::KEPT)),
            Some(b"1-9".to_vec()),
            "{kept} is marked as the one this job takes"
        );
    }
    let calls = fake.calls();
    assert!(
        calls.contains(&format!("/bin/launchctl bootstrap system {plist}")),
        "{calls:#?}"
    );
    assert_eq!(row.outcome, Outcome::Changed);
    let why = row.why.unwrap();
    assert!(
        why.contains("a one-time launchd job removes them, and then itself, after 10 more")
            && why.contains("or at the next restart")
            && !why.contains("sudo"),
        "{why}"
    );
}

#[test]
fn uninstall_names_the_persons_command_when_the_job_does_not_start() {
    let fake = Fake::root();
    fake.on(
        "/bin/launchctl bootstrap system \
         /Library/LaunchDaemons/systems.alab.aterm-machines.reclaim.1-9.plist",
        Ran::exit(5, "Bootstrap failed: 5: Input/output error"),
    );
    let row = uninstall_with_held_folders(&fake);
    assert_eq!(row.outcome, Outcome::Person);
    let why = row.why.unwrap();
    assert!(
        why.contains(&format!("`sudo rm -rf {HELD_ROOT}` once"))
            && why.contains("Input/output error; it starts at the next restart"),
        "the person's command leaves nothing: {why}"
    );
}

#[test]
fn a_file_left_in_the_stage_is_not_held_on_this_mac() {
    let live = crate::port::Live::new();
    let tmp = aterm_tempfile::tempdir().unwrap();
    let layout = Layout {
        root: tmp.path().to_path_buf(),
    };
    let started = layout.stage().join("upgrade-1-5");
    std::fs::create_dir_all(&started).unwrap();
    std::fs::write(layout.stage().join("aterm-open-1.sig"), b"sig").unwrap();
    std::fs::write(layout.stage().join("sudoers.check"), b"").unwrap();
    crate::first_exec::stamp(&live, &started).unwrap();
    let held: Vec<PathBuf> = unproven(&live, &layout)
        .unwrap()
        .into_iter()
        .map(|(dir, _)| dir)
        .collect();
    assert_eq!(held, [started]);
}

#[test]
fn the_staged_agent_marks_its_stage_as_run_before_it_reads_its_arguments() {
    let layout = Layout { root: "/A".into() };
    let mut out = Vec::new();
    let staged = Fake::root();
    let _ = main(
        &staged,
        &layout,
        Path::new("/A/.staging/payload/aterm-machines-dev"),
        &["--shell", "x"],
        &mut out,
    );
    assert!(
        staged.written("/A/.staging/ran").is_some(),
        "a refusal is an exit of its own"
    );
    let installed = Fake::root();
    let _ = main(
        &installed,
        &layout,
        Path::new("/A/agent/aterm-machines-dev"),
        &["--shell", "x"],
        &mut out,
    );
    assert!(installed.written("/A/.staging/ran").is_none());
}

#[test]
fn uninstall_says_ok_for_what_was_never_there_and_changed_only_for_what_it_removed() {
    let fake = Fake::root();
    let layout = Layout {
        root: "/Library/Management/aterm".into(),
    };
    fake.file("/Library/Management/aterm/managed.toml", b"schema = 1\nadmin = \"admin\"\npayload_sha256 = \"\"\nagent_version = \"\"\nexecutable = \"agent/x\"\ninstalled = 0\n");
    fake.file("/etc/sudoers.d/aterm-machines", b"rule");
    let admin = admin();
    fake.on(
        "admin$ /bin/rm -fv /Users//admin/.ssh/aterm_github_ed25519 /Users//admin/.ssh/aterm_github_ed25519.pub",
        Ran::ok(""),
    )
    .on(
        "admin$ /bin/rm -rfv /Users//admin/Library/Caches/aterm-machines",
        Ran::ok("/Users//admin/Library/Caches/aterm-machines\n"),
    )
    .on(
        "admin$ /bin/cat /Users//admin/.ssh/authorized_keys",
        Ran::ok("ssh-ed25519 OWNKEY admin@laptop\n"),
    );
    fake.on(
        "/usr/bin/dscl . -list /Users UniqueID",
        Ran::ok("admin                    501\nroot                     0\n"),
    )
    .on(
        "/usr/bin/dscl . -read /Groups/admin GroupMembership",
        Ran::ok("GroupMembership: root admin\n"),
    )
    .on(
        "/usr/bin/dscl . -read /Users//admin UniqueID PrimaryGroupID NFSHomeDirectory UserShell",
        Ran::ok(&format!(
            "NFSHomeDirectory: {}\nPrimaryGroupID: 20\nUniqueID: 501\nUserShell: /bin/zsh\n",
            admin.home
        )),
    );
    let opened = Opened {
        envelope: crate::envelope::parse_body(
            b"schema = 1\nhost = \"U\"\nhost_name = \"h\"\nserial = 9\nissued = 1\nexpires = 2\naction = \"uninstall\"\n",
        )
        .unwrap(),
        principal: "signing@ctl".into(),
        key_id: String::new(),
    };
    let mut out = Vec::new();
    let _ = uninstall(
        &fake,
        &layout,
        Path::new("/Library/Management/aterm/bin/aterm"),
        &opened,
        "1-9",
        &mut out,
    );
    let text = String::from_utf8(out).unwrap();
    let Line::Report(report) = aterm_json::from_str(text.lines().last().unwrap()).unwrap() else {
        panic!("{text}");
    };
    let outcome = |id: &str| {
        report
            .items
            .iter()
            .find(|r| r.id == id)
            .map(|r| r.outcome)
            .unwrap_or_else(|| panic!("{id}: {:#?}", report.items))
    };
    assert_eq!(outcome("uninstall.github-key@admin"), Outcome::Ok);
    assert_eq!(
        outcome("uninstall.authorized-keys"),
        Outcome::Ok,
        "no line of ours"
    );
    assert_eq!(outcome("uninstall.inbox"), Outcome::Changed);
    assert_eq!(outcome("uninstall.sudoers"), Outcome::Changed);
}

#[test]
fn install_refuses_an_enrolled_mac_before_it_changes_anything() {
    let (fake, layout, args) = staged();
    script_install(&fake);
    fake.file(
        "/Library/Management/aterm/allowed_signers",
        format!("old@ctl namespaces=\"aterm-machines\" ssh-ed25519 {BLOB}\n").as_bytes(),
    );
    let mut out = Vec::new();
    let err = install(
        &fake,
        &layout,
        Path::new(STAGED_EXE),
        &args,
        &admin(),
        &mut out,
    )
    .unwrap_err();
    assert!(
        err.contains("/Library/Management/aterm/allowed_signers"),
        "{err}"
    );
    assert!(err.contains("`aterm machines unenroll <host>`"), "{err}");
    assert!(
        err.contains("`sudo rm -rf /Library/Management/aterm /etc/sudoers.d/aterm-machines`"),
        "{err}"
    );
    assert!(err.contains("/Users//admin/.ssh/authorized_keys"), "{err}");
    assert!(!err.contains('\n'), "one line: {err}");
    assert!(
        !err.contains("more minute(s)"),
        "nothing held, no wait: {err}"
    );
    assert_eq!(fake.calls(), Vec::<String>::new(), "nothing ran or changed");
    assert!(out.is_empty());
    assert_eq!(
        fake.written("/Library/Management/aterm/allowed_signers")
            .unwrap(),
        format!("old@ctl namespaces=\"aterm-machines\" ssh-ed25519 {BLOB}\n").as_bytes()
    );
    let record = "/Library/Management/aterm/state/aterm.app-first-run";
    fake.file(record, b"").stat_of(record, root_dir());
    // Five minutes into the record's hold: its own wait, not the whole one.
    fake.file(&format!("{record}/exec"), b"9700000");
    fake.awake_at(10_000_000);
    let err = refuse_enrolled(&fake, &layout, &admin()).unwrap_err();
    assert!(
        err.contains(
            "`sudo rm -rf /Library/Management/aterm /etc/sudoers.d/aterm-machines` once 5 \
             more minute(s) of awake time have passed, or after a restart"
        ),
        "a held folder's wait: {err}"
    );
    let (fake, layout, _) = staged();
    fake.file(
        "/Library/Management/aterm/allowed_signers",
        format!("old@ctl namespaces=\"aterm-machines\" ssh-ed25519 {BLOB}\n").as_bytes(),
    );
    fake.stat_fails(
        "/Library/Management/aterm/stage",
        std::io::ErrorKind::PermissionDenied,
    );
    let err = refuse_enrolled(&fake, &layout, &admin()).unwrap_err();
    assert!(
        err.contains("once 10 more minute(s) of awake time have passed"),
        "an inventory that cannot be read holds the whole wait: {err}"
    );
    let (fake, layout, args) = staged();
    script_install(&fake);
    fake.stat_fails(
        "/Library/Management/aterm/allowed_signers",
        std::io::ErrorKind::PermissionDenied,
    );
    let err = install(
        &fake,
        &layout,
        Path::new(STAGED_EXE),
        &args,
        &admin(),
        &mut out,
    )
    .unwrap_err();
    assert!(
        err.starts_with("/Library/Management/aterm/allowed_signers: "),
        "{err}"
    );
    assert_eq!(fake.calls(), Vec::<String>::new());
}

#[test]
fn an_install_that_failed_before_the_end_can_be_run_again() {
    let (fake, layout, args) = staged();
    for c in [
        "/usr/sbin/chown -R root:wheel /Library/Management/aterm/.staging/payload",
        "/bin/chmod -R go-w /Library/Management/aterm/.staging/payload",
        "/bin/chmod 0755 /Library/Management/aterm/.staging/payload",
    ] {
        fake.on(c, Ran::ok(""));
    }
    fake.on(
        "/usr/sbin/visudo -cf /Library/Management/aterm/stage/sudoers.check",
        Ran::ok("/Library/Management/aterm/stage/sudoers.check: parsed OK\n"),
    );
    fake.on(
        "admin$ /bin/cat /Users//admin/.ssh/authorized_keys",
        Ran::exit(
            1,
            "cat: /Users//admin/.ssh/authorized_keys: Permission denied",
        ),
    );
    let mut out = Vec::new();
    let err = install(
        &fake,
        &layout,
        Path::new(STAGED_EXE),
        &args,
        &admin(),
        &mut out,
    )
    .unwrap_err();
    assert!(err.contains("Permission denied"), "{err}");
    assert!(
        fake.written("/Library/Management/aterm/allowed_signers")
            .is_none(),
        "a failed install left the Mac looking enrolled"
    );
    assert!(refuse_enrolled(&fake, &layout, &admin()).is_ok());
}

#[test]
fn install_refuses_to_run_from_anywhere_but_the_stage() {
    let (fake, layout, args) = staged();
    let mut out = Vec::new();
    let err = install(
        &fake,
        &layout,
        Path::new("/Users//admin/aterm"),
        &args,
        &admin(),
        &mut out,
    )
    .unwrap_err();
    assert!(err.contains("runs from the staged agent"), "{err}");
    let mut other = args.clone();
    other.stage = "/tmp/payload".into();
    let exe = Path::new("/Library/Management/aterm/.staging/payload/aterm-machines-dev");
    assert!(install(&fake, &layout, exe, &other, &admin(), &mut out).is_err());
    let mut other = args;
    other.allowed_signers = "/Users//admin/allowed_signers".into();
    assert!(install(&fake, &layout, exe, &other, &admin(), &mut out).is_err());
    assert!(
        !fake
            .calls()
            .iter()
            .any(|c| c.starts_with("rename") || c.starts_with("write"))
    );
}

#[test]
fn install_refuses_an_executable_an_upgrade_would_not_find_in_the_archive() {
    let (fake, layout, args) = staged();
    script_install(&fake);
    let _ = fake.remove(Path::new(STAGED_EXE));
    let file = Stat {
        dir: false,
        ..root_dir()
    };
    for root in [
        "/Library/Management/aterm/.staging/payload",
        "/Library/Management/aterm/agent",
    ] {
        for d in [
            "aterm.app",
            "aterm.app/Contents",
            "aterm.app/Contents/MacOS",
        ] {
            fake.file(&format!("{root}/{d}"), b"")
                .stat_of(&format!("{root}/{d}"), root_dir());
        }
        for exe in ["aterm", "aterm-ctl"] {
            let path = format!("{root}/aterm.app/Contents/MacOS/{exe}");
            fake.file(&path, b"agent").stat_of(&path, file);
        }
    }
    let staged_exe = |name: &str| {
        PathBuf::from(format!(
            "/Library/Management/aterm/.staging/payload/aterm.app/Contents/MacOS/{name}"
        ))
    };
    let mut out = Vec::new();
    install(
        &fake,
        &layout,
        &staged_exe("aterm-ctl"),
        &args,
        &admin(),
        &mut out,
    )
    .unwrap_err();
    assert!(
        !fake
            .calls()
            .iter()
            .any(|c| c.starts_with("rename") || c.starts_with("mkdir") || c.starts_with("write")),
        "{:#?}",
        fake.calls()
    );
    install(
        &fake,
        &layout,
        &staged_exe("aterm"),
        &args,
        &admin(),
        &mut out,
    )
    .unwrap_or_else(|e| panic!("{e}\n{}", String::from_utf8_lossy(&out)));
    assert_eq!(
        state::managed(&fake, &layout).unwrap().executable,
        "agent/aterm.app/Contents/MacOS/aterm"
    );
}
