// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::items::apps::tests::{
    CODESIGN_WRONG_TEAM, SPCTL_KYA_ON, admin, codesign_key, ctx_with, dir, err, ok, out_status,
    sha_of, stage_path,
};
use crate::model::{Mode, Outcome};
use crate::port::{Fake, Ran, Stat};

/// [`first_run_record`] on this Mac.
const FIRST_RUN: &str = "/Library/Management/aterm/state/aterm.app-first-run";

const ZIP_SHA: &str = "457cee7e20d0d406fb6ce43275593d0f553eb0fccd46fcc812dfb8d02c0b427f";
const INSTALL_SH_SHA: &str = "dd92e2224c1c5a8e2c69992c4bafdf79142cf27f36a5bacfbfaa8efe12949b98";

const INSTALL_SH_OUT: &str = "install.sh: linked aterm 0.105.0 -> /Users//admin/.local/bin/aterm (follows the app's updates)\ninstall.sh: --no-toolchain: /Users//admin/.config/aterm/aterm.toml already says `auto_install = false` — left as it is\n";

const DOCTOR_EMPTY: &str = r#"{"schema":1,"mode":"check","complete":false,"items":[{"id":"base.pkg.path","family":"pkg","title":"the managed bin/ on PATH","outcome":"planned","from":"managed bin/ is not on PATH here","to":"on PATH","effect":"now","layer":"base","class":"person","explain":{"why":"managed bin/ is not on PATH here; a new aterm shell adds it, or add: export PATH=\"$PATH:/Users//admin/Library/Application Support/aterm/pkg/bin\"","remedy":{"command":"export PATH=\"$PATH:/Users//admin/Library/Application Support/aterm/pkg/bin\""}}},{"id":"base.shell.hooks","family":"shell","title":"the shell.d hooks","outcome":"planned","from":"shell.d hooks not generated yet (an install writes them)","to":"present","effect":"now","layer":"base","class":"relay","explain":{"why":"shell.d hooks not generated yet (an install writes them)","remedy":"repair"}},{"id":"base.pkg.toolset","family":"pkg","title":"the ALab toolset","outcome":"planned","from":"no ALab programs are installed","to":"the ALab default set, installed","effect":"now","layer":"base","class":"install","explain":{"why":"no ALab programs are installed","remedy":{"command":"aterm pkg install --default-set"}},"essential":true}]}"#;

const DOCTOR_FULL: &str = r#"{"schema":1,"mode":"check","complete":true,"items":[{"id":"base.pkg.path","family":"pkg","title":"the managed bin/ on PATH","outcome":"planned","from":"managed bin/ is not on PATH here","to":"on PATH","effect":"now","layer":"base","class":"person","explain":{"why":"managed bin/ is not on PATH here; a new aterm shell adds it, or add: export PATH=\"$PATH:/Users//admin/Library/Application Support/aterm/pkg/bin\"","remedy":{"command":"export PATH=\"$PATH:/Users//admin/Library/Application Support/aterm/pkg/bin\""}}},{"id":"base.pkg.toolset","family":"pkg","title":"the ALab toolset","outcome":"ok","to":"12 program(s) active","effect":"now","detail":"12","layer":"base","class":"install","essential":true}]}"#;

const DOCTOR_BROKEN_SHIM: &str = r#"{"schema":1,"mode":"check","complete":false,"items":[{"id":"base.pkg.shims@cargo","family":"pkg","title":"the bin/ shim cargo","outcome":"planned","from":"broken bin shim /Users//admin/Library/Application Support/aterm/pkg/bin/cargo","to":"forwarding to a build in the store","effect":"now","layer":"base","class":"relay","explain":{"why":"broken bin shim /Users//admin/Library/Application Support/aterm/pkg/bin/cargo","remedy":"repair"},"essential":true},{"id":"base.pkg.toolset","family":"pkg","title":"the ALab toolset","outcome":"ok","to":"12 program(s) active","effect":"now","detail":"12","layer":"base","class":"install","essential":true}]}"#;

const DOCTOR_DECLINED: &str = r#"{"schema":1,"mode":"check","complete":true,"items":[{"id":"base.pkg.toolset","family":"pkg","title":"the ALab toolset","outcome":"not-applicable","why":"the ALab toolset was removed on this machine (`aterm pkg install --default-set` reinstalls it)","effect":"now","layer":"base","class":"install","explain":{"why":"the ALab toolset was removed on this machine (`aterm pkg install --default-set` reinstalls it)","remedy":{"command":"aterm pkg install --default-set"}},"essential":true}]}"#;

fn plistbuddy(bundle: &str, key: &str) -> String {
    format!("/usr/libexec/PlistBuddy -c 'Print :{key}' {bundle}/Contents/Info.plist")
}

fn table() -> Table {
    Table {
        version: Some("0.104.0".into()),
        archive_sha256: Some(ZIP_SHA.into()),
        install_sh_sha256: Some(INSTALL_SH_SHA.into()),
        owner: None,
        cli_users: vec![],
        toolset: None,
        vault_server: None,
        vault_advertise: None,
    }
}

fn installed(sys: &Fake, version: &str, uid: u32) {
    sys.file(APP, b"").stat_of(APP, Stat { uid, ..dir() });
    sys.file(&format!("{APP}/Contents/Info.plist"), b"");
    sys.on(
        &plistbuddy(APP, "CFBundleIdentifier"),
        ok("com.aterm.aterm\n"),
    );
    sys.on(
        &plistbuddy(APP, "CFBundleShortVersionString"),
        ok(&format!("{version}\n")),
    );
    sys.on(&codesign_key(APP, TEAM_ID), ok(""));
}

#[test]
fn the_app_comes_first_then_each_user_s_command_then_each_toolset() {
    let ctx = ctx_with(&Fake::root(), &[], true);
    let mut t = table();
    t.toolset = Some(true);
    let items = expand(&t, &ctx).unwrap();
    let ids: Vec<String> = items.iter().map(|i| i.id()).collect();
    assert_eq!(
        ids,
        [
            "aterm.app",
            "aterm.cli@admin",
            "aterm.toolset@admin",
            "aterm.vault-server"
        ]
    );
    assert_eq!(
        items[1].after(),
        ["aterm.app", "developer.command-line-tools"]
    );
    assert_eq!(expand(&table(), &ctx).unwrap()[1].after(), ["aterm.app"]);
    assert_eq!(
        items[2].after(),
        ["aterm.cli@admin", "developer.command-line-tools"]
    );
    t.version = Some("v0.104.0".into());
    assert!(
        expand(&t, &ctx)
            .err()
            .unwrap()
            .contains("MAJOR.MINOR.PATCH")
    );
}

#[test]
fn versions_compare_by_number() {
    assert!(at_least("0.105.0", "0.104.0"));
    assert!(at_least("0.104.0", "0.104.0"));
    assert!(at_least("0.104.10", "0.104.9"));
    assert!(!at_least("0.103.9", "0.104.0"));
    assert!(!at_least("0.104.0-dev", "0.104.0"));
    assert!(is_version("0.104.0") && !is_version("0.104") && !is_version("01.2.3"));
}

#[test]
fn a_release_aterm_at_or_past_the_pin_owned_by_its_owner_is_ok() {
    let sys = Fake::root();
    installed(&sys, "0.105.0", 501);
    let ctx = ctx_with(&sys, &[], false);
    let items = expand(&table(), &ctx).unwrap();
    assert_eq!(
        items[0].probe(&sys, &ctx),
        Probe::Ok {
            now: "aterm 0.105.0 (team A66A9P66Z7), owned by admin".into()
        }
    );
}

#[test]
fn a_wrong_owner_is_a_chown_that_needs_no_artifact() {
    let sys = Fake::root();
    installed(&sys, "0.104.0", 0);
    sys.on(&format!("/usr/sbin/chown -R 501:20 {APP}"), ok(""));
    sys.on(&format!("/bin/chmod -R go-w,ug-s {APP}"), ok(""));
    let ctx = ctx_with(&sys, &[], false);
    let items = expand(&table(), &ctx).unwrap();
    assert_eq!(
        items[0].probe(&sys, &ctx),
        Probe::Change {
            from: "aterm 0.104.0, owned by uid 0".into(),
            to: "aterm 0.104.0, owned by admin".into()
        }
    );
    items[0].apply(&sys, &ctx).unwrap();
    assert!(!sys.calls().iter().any(|c| c.contains("ditto")));
}

#[test]
fn without_a_resolved_or_cached_release_the_app_is_blocked() {
    let sys = Fake::root();
    let ctx = ctx_with(&sys, &[], true);
    let mut t = table();
    t.archive_sha256 = None;
    let items = expand(&t, &ctx).unwrap();
    assert!(
        matches!(items[0].probe(&sys, &ctx), Probe::Blocked { why } if why.contains("controller resolves it"))
    );
    let items = expand(&table(), &ctx).unwrap();
    assert!(
        matches!(items[0].probe(&sys, &ctx), Probe::Blocked { why } if why.contains("not in the cache"))
    );
}

#[test]
fn an_older_aterm_that_is_running_waits_for_an_idle_mac() {
    let sys = Fake::root();
    installed(&sys, "0.103.0", 501);
    sys.running(&["/Applications/aterm.app/Contents/MacOS/aterm"]);
    let ctx = ctx_with(&sys, &[(ZIP_SHA, b"zip")], false);
    let items = expand(&table(), &ctx).unwrap();
    assert!(matches!(items[0].probe(&sys, &ctx), Probe::Deferred { .. }));
}

#[test]
fn the_release_zip_is_vetted_given_to_the_owner_and_swapped_in() {
    let sys = Fake::root();
    let bytes = b"aterm zip";
    let sha = sha_of(bytes);
    let stage = stage_path(0);
    let mut t = table();
    t.archive_sha256 = Some(sha.clone());
    let ctx = ctx_with(&sys, &[(&sha, bytes)], true);
    sys.on(
        &format!("/usr/bin/ditto -x -k --noacl --noqtn /c/{sha} {stage}/new"),
        ok(""),
    );
    let staged = format!("{stage}/new/aterm.app");
    sys.on_write(
        &format!("/usr/bin/ditto -x -k --noacl --noqtn /c/{sha} {stage}/new"),
        Path::new(&staged),
        b"",
    )
    .on_stat(
        &format!("/usr/bin/ditto -x -k --noacl --noqtn /c/{sha} {stage}/new"),
        Path::new(&staged),
        dir(),
    );
    sys.on_write(
        &format!("/usr/bin/ditto -x -k --noacl --noqtn /c/{sha} {stage}/new"),
        Path::new(&format!("{staged}/Contents/Info.plist")),
        b"",
    );
    sys.on(&format!("/usr/sbin/chown -R 501:20 {staged}"), ok(""));
    sys.on(&format!("/bin/chmod -R go-w,ug-s {staged}"), ok(""));
    sys.on(&codesign_key(&staged, TEAM_ID), ok(""));
    sys.on(
        &format!("/usr/sbin/spctl -a -vv -t exec {staged}"),
        err(0, SPCTL_KYA_ON),
    );
    sys.on(
        &plistbuddy(&staged, "CFBundleShortVersionString"),
        ok("0.104.0\n"),
    );
    sys.on(
        &plistbuddy(&staged, "CFBundleIdentifier"),
        ok("com.aterm.aterm\n"),
    );
    let first_run = format!("admin$ {EXE} --version");
    let budget = std::sync::Arc::new(std::sync::Mutex::new(None));
    let seen = std::sync::Arc::clone(&budget);
    let key = first_run.clone();
    sys.simulate(move |_, cmd| {
        (cmd.display() == key).then(|| {
            *seen.lock().unwrap() = Some(cmd.budget);
            ok("aterm 0.104.0\n")
        })
    });
    sys.awake_at(5_000_000);
    let items = expand(&t, &ctx).unwrap();
    assert!(matches!(items[0].probe(&sys, &ctx), Probe::Change { from, .. } if from == "absent"));
    items[0].apply(&sys, &ctx).unwrap();
    let calls = sys.calls();
    let at = |c: &str| {
        calls
            .iter()
            .position(|x| x.starts_with(c))
            .unwrap_or_else(|| panic!("{c}: {calls:#?}"))
    };
    assert!(at("/usr/sbin/chown -R 501:20") < at("/usr/bin/codesign"));
    assert!(at("/usr/bin/codesign") < at(&format!("rename {staged} {APP}")));
    let stamp = at(&format!("write {FIRST_RUN}/exec 644"));
    assert!(
        at(&format!("mkdir {FIRST_RUN} 755")) < stamp,
        "a plan without root reads it"
    );
    assert!(at(&format!("rename {staged} {APP}")) < stamp && stamp < at(&first_run));
    assert!(at(&first_run) < at(&format!("write {FIRST_RUN}/ran 644")));
    assert_eq!(
        *budget.lock().unwrap(),
        Some(aterm_uds::deadline::FIRST_EXEC_HANG)
    );
    assert_eq!(items[0].detail(&sys, &ctx), None, "Gatekeeper accepted it");
}

#[test]
fn a_first_run_that_did_not_show_it_ran_notes_how_long_its_bundle_is_held() {
    let note = |took_ms: u64, reply: Option<Ran>| {
        let sys = Fake::root();
        sys.awake_at(5_000_000);
        let key = format!("admin$ {EXE} --version");
        sys.simulate(move |sys, cmd| {
            if cmd.display() != key {
                return None;
            }
            sys.awake_at(5_000_000 + took_ms);
            reply.clone()
        });
        let app = App {
            version: None,
            archive: None,
            owner: admin(),
            notes: Notes::default(),
        };
        app.first_run(&sys).unwrap();
        assert!(sys.written(&format!("{FIRST_RUN}/ran")).is_none());
        app.notes.joined().unwrap()
    };
    let killed = Ran {
        status: None,
        stdout: Vec::new(),
        stderr: b"Killed: 9".to_vec(),
    };
    let held = note(0, Some(killed.clone()));
    assert!(
        held.contains(
            "nothing replaces it for 10 more minute(s) of awake time, or until a restart"
        ),
        "{held}"
    );
    let spent = note(600_000, Some(killed));
    assert!(
        spent.contains("its hold is spent, so the next apply may replace it"),
        "{spent}"
    );
    let unstarted = note(60_000, None);
    assert!(
        unstarted.contains("could not start") && unstarted.contains("for 9 more minute(s)"),
        "{unstarted}"
    );
}

#[test]
fn an_aterm_app_that_has_not_shown_it_ran_is_not_replaced_even_now() {
    let probe = |record: &[(&str, &[u8])], awake: u64| {
        let sys = Fake::root();
        installed(&sys, "0.103.0", 501);
        for (name, bytes) in record {
            sys.file(&format!("{FIRST_RUN}/{name}"), bytes);
        }
        sys.awake_at(awake);
        let ctx = ctx_with(&sys, &[(ZIP_SHA, b"zip")], true);
        expand(&table(), &ctx).unwrap()[0].probe(&sys, &ctx)
    };
    let started: &[(&str, &[u8])] = &[("exec", b"5000000")];
    assert!(
        matches!(
            probe(started, 5_100_000),
            Probe::Deferred { why } if why.contains("in 9 minute(s)")
        ),
        "{:?}",
        probe(started, 5_100_000)
    );
    let changes = |p: Probe| matches!(p, Probe::Change { from, .. } if from == "aterm 0.103.0");
    assert!(changes(probe(&[], 5_100_000)), "no record");
    assert!(
        changes(probe(&[("exec", b"5000000"), ("ran", b"")], 5_100_000)),
        "it showed it ran"
    );
    assert!(changes(probe(started, 5_600_000)), "its hang has passed");
    assert!(
        changes(probe(started, 4_000_000)),
        "the Mac restarted since"
    );
}

#[test]
fn a_release_zip_signed_by_another_team_is_refused() {
    let sys = Fake::root();
    let bytes = b"forged zip";
    let sha = sha_of(bytes);
    let stage = stage_path(0);
    let mut t = table();
    t.archive_sha256 = Some(sha.clone());
    let ctx = ctx_with(&sys, &[(&sha, bytes)], true);
    sys.on(
        &format!("/usr/bin/ditto -x -k --noacl --noqtn /c/{sha} {stage}/new"),
        ok(""),
    );
    let staged = format!("{stage}/new/aterm.app");
    sys.on_write(
        &format!("/usr/bin/ditto -x -k --noacl --noqtn /c/{sha} {stage}/new"),
        Path::new(&staged),
        b"",
    )
    .on_stat(
        &format!("/usr/bin/ditto -x -k --noacl --noqtn /c/{sha} {stage}/new"),
        Path::new(&staged),
        dir(),
    );
    sys.on(&format!("/usr/sbin/chown -R 501:20 {staged}"), ok(""));
    sys.on(&format!("/bin/chmod -R go-w,ug-s {staged}"), ok(""));
    sys.on(&codesign_key(&staged, TEAM_ID), err(3, CODESIGN_WRONG_TEAM));
    let items = expand(&t, &ctx).unwrap();
    let why = items[0].apply(&sys, &ctx).unwrap_err();
    assert!(why.contains("not by Developer ID team A66A9P66Z7"), "{why}");
    assert!(!sys.calls().iter().any(|c| c.starts_with("rename")));
}

#[test]
fn the_command_is_a_link_to_the_app_s_executable() {
    let sys = Fake::root();
    let link = "/Users//admin/.local/bin/aterm";
    sys.stat_of(
        link,
        Stat {
            symlink: true,
            dir: false,
            ..dir()
        },
    );
    sys.on(
        &format!("/usr/bin/readlink {link}"),
        ok("/Applications/aterm.app/Contents/MacOS/aterm\n"),
    );
    let ctx = ctx_with(&sys, &[], true);
    let items = expand(&table(), &ctx).unwrap();
    assert_eq!(
        items[1].probe(&sys, &ctx),
        Probe::Ok {
            now: "/Users//admin/.local/bin/aterm -> /Applications/aterm.app/Contents/MacOS/aterm"
                .into()
        }
    );
    let sys = Fake::root();
    sys.stat_of(
        link,
        Stat {
            symlink: true,
            dir: false,
            ..dir()
        },
    );
    sys.on(
        &format!("/usr/bin/readlink {link}"),
        ok("/Users//admin/Applications/aterm.app/Contents/MacOS/aterm\n"),
    );
    let ctx = ctx_with(&sys, &[(INSTALL_SH_SHA, b"#!/bin/bash")], true);
    assert!(
        matches!(items[1].probe(&sys, &ctx), Probe::Change { from, .. } if from.starts_with("a link to /Users//admin/Applications"))
    );
}

/// A link whose target differs from the app's executable only by trailing whitespace is a
/// link elsewhere, not the command. NEGATIVE CONTROL: the newline `readlink` ends its
/// answer with is still not part of the target.
#[test]
fn a_link_to_the_executable_with_a_trailing_space_is_not_the_command() {
    let link = "/Users//admin/.local/bin/aterm";
    for (printed, is_command) in [
        ("/Applications/aterm.app/Contents/MacOS/aterm \n", false),
        ("/Applications/aterm.app/Contents/MacOS/aterm\t\n", false),
        ("/Applications/aterm.app/Contents/MacOS/aterm\n", true),
    ] {
        let sys = Fake::root();
        sys.stat_of(
            link,
            Stat {
                symlink: true,
                dir: false,
                ..dir()
            },
        );
        sys.on(&format!("/usr/bin/readlink {link}"), ok(printed));
        let ctx = ctx_with(&sys, &[(INSTALL_SH_SHA, b"#!/bin/bash")], true);
        let items = expand(&table(), &ctx).unwrap();
        let probe = items[1].probe(&sys, &ctx);
        if is_command {
            assert!(matches!(probe, Probe::Ok { .. }), "{printed:?}: {probe:?}");
        } else {
            let target = printed.strip_suffix('\n').unwrap();
            assert!(
                matches!(&probe, Probe::Change { from, .. } if *from == format!("a link to {target}")),
                "{printed:?}: {probe:?}"
            );
        }
    }
}

#[test]
fn install_sh_runs_as_the_user_from_the_root_only_cache_on_standard_input() {
    let ctx = ctx_with(&Fake::root(), &[], true);
    let cli = Cli {
        user: admin(),
        script: Some(INSTALL_SH_SHA.into()),
        toolset: false,
    };
    let cmd = cli.command(Path::new("/c/x"));
    assert_eq!(
        cmd.display(),
        "admin$ /bin/bash -s -- --no-app --no-toolchain"
    );
    assert_eq!(cmd.stdin.as_deref(), Some(Path::new("/c/x")));
    assert_eq!(cmd.budget, CLI_BUDGET);
    let cli = Cli {
        toolset: true,
        ..cli
    };
    assert_eq!(
        cli.command(Path::new("/c/x")).display(),
        "admin$ /bin/bash -s -- --no-app"
    );
    let sys = Fake::root();
    let script = b"main \"$@\"\n";
    let sha = sha_of(script);
    let mut t = table();
    t.install_sh_sha256 = Some(sha.clone());
    let ctx2 = ctx_with(&sys, &[(&sha, script)], ctx.disrupt);
    let link = "/Users//admin/.local/bin/aterm";
    sys.on(
        &format!("/usr/bin/readlink {link}"),
        ok(&format!("{EXE}\n")),
    );
    sys.on(
        "admin$ /bin/bash -s -- --no-app --no-toolchain",
        ok(INSTALL_SH_OUT),
    );
    let items = expand(&t, &ctx2).unwrap();
    assert!(matches!(items[1].probe(&sys, &ctx2), Probe::Change { from, .. } if from == "absent"));
    items[1].apply(&sys, &ctx2).unwrap();
    sys.stat_of(
        link,
        Stat {
            symlink: true,
            dir: false,
            ..dir()
        },
    );
    assert!(matches!(items[1].probe(&sys, &ctx2), Probe::Ok { .. }));
}

#[test]
fn the_toolset_is_done_when_atpkg_s_doctor_is_and_a_held_lock_blocks_rather_than_fails() {
    let sys = Fake::root();
    sys.file(EXE, b"");
    let doctor = "admin$ /Applications/aterm.app/Contents/MacOS/aterm pkg doctor --json";
    sys.on(doctor, out_status(1, DOCTOR_EMPTY));
    let install = "admin$ /Applications/aterm.app/Contents/MacOS/aterm pkg install --default-set --wait-lock 1800";
    sys.on(
        install,
        Ran {
            status: Some(75),
            stdout: Vec::new(),
            stderr: b"atpkg: another atpkg run holds the store lock".to_vec(),
        },
    );
    let ctx = ctx_with(&sys, &[], true);
    let mut t = table();
    t.toolset = Some(true);
    let items = expand(&t, &ctx).unwrap();
    let toolset = &items[2..3];
    let rows = crate::engine::run(
        toolset,
        Mode::Plan,
        &sys,
        &ctx,
        &mut crate::engine::NoJournal,
    );
    assert_eq!(rows[0].outcome, Outcome::Planned);
    assert_eq!(
        rows[0].from.as_deref(),
        Some("base.pkg.toolset: no ALab programs are installed"),
        "the row that fails doctor, by its id; the advisories are not why"
    );
    let rows = crate::engine::run(
        toolset,
        Mode::Apply,
        &sys,
        &ctx,
        &mut crate::engine::NoJournal,
    );
    assert_eq!(rows[0].outcome, Outcome::Blocked, "{:?}", rows[0]);
    assert!(rows[0].why.as_deref().unwrap().contains("exit 75"));
    assert!(!rows[0].outcome.is_failure());
    let sys = Fake::root();
    sys.file(EXE, b"");
    sys.on(doctor, out_status(1, DOCTOR_EMPTY));
    sys.on(doctor, ok(DOCTOR_FULL));
    sys.on(install, ok(""));
    let rows = crate::engine::run(
        toolset,
        Mode::Apply,
        &sys,
        &ctx,
        &mut crate::engine::NoJournal,
    );
    assert_eq!(rows[0].outcome, Outcome::Changed, "{:?}", rows[0]);
    assert_eq!(
        items[2].probe(&sys, &ctx),
        Probe::Ok {
            now: "12 program(s) active; 1 thing(s) to look at".into()
        }
    );
}

#[test]
fn a_broken_shim_is_a_change_of_the_toolset_row_not_no_answer() {
    let sys = Fake::root();
    sys.file(EXE, b"");
    sys.on(
        "admin$ /Applications/aterm.app/Contents/MacOS/aterm pkg doctor --json",
        out_status(1, DOCTOR_BROKEN_SHIM),
    );
    let ctx = ctx_with(&sys, &[], true);
    let mut t = table();
    t.toolset = Some(true);
    let items = expand(&t, &ctx).unwrap();
    assert_eq!(
        items[2].probe(&sys, &ctx),
        Probe::Change {
            from: "base.pkg.shims@cargo: broken bin shim /Users//admin/Library/Application Support/aterm/pkg/bin/cargo".into(),
            to: "the ALab default set, complete".into(),
        }
    );
}

#[test]
fn a_doctor_exit_1_with_no_failing_row_is_no_answer() {
    let doctor = "admin$ /Applications/aterm.app/Contents/MacOS/aterm pkg doctor --json";
    let mut t = table();
    t.toolset = Some(true);
    let complete = DOCTOR_FULL;
    let unnamed = DOCTOR_FULL.replace(r#""complete":true"#, r#""complete":false"#);
    for (ran, says) in [
        (err(1, ""), "exit 1"),
        (out_status(1, complete), "exit 1"),
        (out_status(1, &unnamed), "names no row"),
        (out_status(0, DOCTOR_EMPTY), "exit 0"),
        (out_status(0, "doctor: healthy\n"), "not its report"),
    ] {
        let sys = Fake::root();
        sys.file(EXE, b"");
        sys.on(doctor, ran);
        let ctx = ctx_with(&sys, &[], true);
        let items = expand(&t, &ctx).unwrap();
        let probe = items[2].probe(&sys, &ctx);
        assert!(
            matches!(&probe, Probe::Unusable { why } if why.contains(says)),
            "{says}: {probe:?}"
        );
    }
}

#[test]
fn a_declined_toolset_is_a_change_the_owner_s_baseline_lifts() {
    let sys = Fake::root();
    sys.file(EXE, b"");
    sys.on(
        "admin$ /Applications/aterm.app/Contents/MacOS/aterm pkg doctor --json",
        ok(DOCTOR_DECLINED),
    );
    let ctx = ctx_with(&sys, &[], true);
    let mut t = table();
    t.toolset = Some(true);
    let items = expand(&t, &ctx).unwrap();
    assert_eq!(
        items[2].probe(&sys, &ctx),
        Probe::Change {
            from: "base.pkg.toolset: the ALab toolset was removed on this machine (`aterm pkg install --default-set` reinstalls it)".into(),
            to: "the ALab default set, complete".into(),
        }
    );
}

#[test]
fn a_report_of_another_schema_or_without_the_toolset_row_is_no_answer() {
    let doctor = "admin$ /Applications/aterm.app/Contents/MacOS/aterm pkg doctor --json";
    let mut t = table();
    t.toolset = Some(true);
    let schema_2 = DOCTOR_FULL.replace(r#""schema":1"#, r#""schema":2"#);
    let planned = DOCTOR_FULL.replace(r#""mode":"check""#, r#""mode":"plan""#);
    let no_toolset = DOCTOR_FULL.replace(r#""id":"base.pkg.toolset""#, r#""id":"base.pkg.other""#);
    for (ran, says) in [
        (out_status(0, &schema_2), "schema 2"),
        (
            out_status(
                1,
                &schema_2.replace(r#""complete":true"#, r#""complete":false"#),
            ),
            "schema 2",
        ),
        (out_status(0, &planned), "not a check"),
        (out_status(0, &no_toolset), "no toolset row"),
    ] {
        let sys = Fake::root();
        sys.file(EXE, b"");
        sys.on(doctor, ran);
        let ctx = ctx_with(&sys, &[], true);
        let items = expand(&t, &ctx).unwrap();
        let probe = items[2].probe(&sys, &ctx);
        assert!(
            matches!(&probe, Probe::Unusable { why } if why.contains(says)),
            "{says}: {probe:?}"
        );
    }
}

#[test]
fn an_aterm_without_doctor_json_is_no_answer_that_says_so() {
    let sys = Fake::root();
    sys.file(EXE, b"");
    sys.on(
        "admin$ /Applications/aterm.app/Contents/MacOS/aterm pkg doctor --json",
        err(2, "atpkg: doctor takes no arguments (got --json)"),
    );
    let ctx = ctx_with(&sys, &[], true);
    let mut t = table();
    t.toolset = Some(true);
    let items = expand(&t, &ctx).unwrap();
    let probe = items[2].probe(&sys, &ctx);
    assert!(
        matches!(&probe, Probe::Unusable { why } if why.contains("exit 2")
            && why.contains("an aterm older than `pkg doctor --json`")),
        "{probe:?}"
    );
}

#[test]
fn any_other_atpkg_failure_is_a_failure() {
    let sys = Fake::root();
    sys.on(
        "admin$ /Applications/aterm.app/Contents/MacOS/aterm pkg install --default-set --wait-lock 1800",
        err(1, "atpkg: default-set bootstrap failed"),
    );
    let ctx = ctx_with(&sys, &[], true);
    let mut t = table();
    t.toolset = Some(true);
    let items = expand(&t, &ctx).unwrap();
    assert!(
        items[2]
            .apply(&sys, &ctx)
            .unwrap_err()
            .contains("bootstrap failed")
    );
    assert_eq!(items[2].stood_aside(), None);
}

#[test]
fn always_on_desktop_installs_vault_server_launchdaemon_before_login_and_retires_user_agent() {
    let pmset_always_on = "AC Power:\n sleep\t\t0\n displaysleep\t15\n";
    let print_cmd = format!(
        "{} print system/{}",
        super::super::sharing::LAUNCHCTL,
        aterm_secrets::service::DAEMON_LABEL
    );
    let enable_cmd = format!(
        "{} enable system/{}",
        super::super::sharing::LAUNCHCTL,
        aterm_secrets::service::DAEMON_LABEL
    );
    let bootstrap_cmd = format!(
        "{} bootstrap system {}",
        super::super::sharing::LAUNCHCTL,
        aterm_secrets::service::DAEMON_PLIST
    );
    let kickstart_cmd = format!(
        "{} kickstart system/{}",
        super::super::sharing::LAUNCHCTL,
        aterm_secrets::service::DAEMON_LABEL
    );
    let user_agent_plist =
        "/Users//admin/Library/LaunchAgents/systems.alab.aterm-secrets-server.plist";

    let sys = Fake::root();
    sys.file(EXE, b"");
    // Negative control: before login (`console_user == None` after a reboot), a per-user
    // LaunchAgent plist on disk is not loaded in `system` and leaves the vault server absent.
    sys.file(user_agent_plist, b"<plist/>");
    sys.on("/usr/bin/pmset -g custom", ok(pmset_always_on));
    sys.on(&print_cmd, err(super::super::sharing::NOT_LOADED, ""));
    sys.on(&print_cmd, err(super::super::sharing::NOT_LOADED, ""));
    sys.on(
        &print_cmd,
        ok("system/systems.alab.aterm-vault-server = {\n\tpid = 7484\n}\n"),
    );

    let mut ctx = ctx_with(&sys, &[], true);
    ctx.facts.form = crate::model::Form::Desktop;
    ctx.facts.console_user = None;
    ctx.console = None;
    ctx.host_name = "m19-Mac-Studio-M4-Max".into();

    let t = table();
    assert_eq!(t.vault_server, None, "C7a LaunchDaemon defaults on (F6)");
    let items = expand(&t, &ctx).unwrap();
    let daemon = items
        .iter()
        .find(|item| item.id() == "aterm.vault-server")
        .unwrap();
    assert_eq!(daemon.after(), ["aterm.app", "power.ac.sleep"]);
    assert_eq!(
        daemon.probe(&sys, &ctx),
        Probe::Change {
            from: "not installed".into(),
            to: "systems.alab.aterm-vault-server running in system (advertising m19)".into(),
        }
    );

    sys.on("/usr/bin/pmset -g custom", ok(pmset_always_on));
    sys.on(
        &format!(
            "{} bootout gui/501/systems.alab.aterm-secrets-server",
            super::super::sharing::LAUNCHCTL
        ),
        err(super::super::sharing::NO_SUCH_PROCESS, ""),
    );
    sys.on(
        &format!(
            "{} bootout user/501/systems.alab.aterm-secrets-server",
            super::super::sharing::LAUNCHCTL
        ),
        err(super::super::sharing::NO_SUCH_PROCESS, ""),
    );
    sys.on(&enable_cmd, ok(""));
    sys.on(&bootstrap_cmd, ok(""));
    sys.on(&kickstart_cmd, ok(""));
    daemon.apply(&sys, &ctx).unwrap();

    assert!(
        sys.read_file(Path::new(user_agent_plist)).is_err(),
        "per-login LaunchAgent plist retired so port 7484 is not contested at login"
    );
    let written = String::from_utf8(
        sys.read_file(Path::new(aterm_secrets::service::DAEMON_PLIST))
            .unwrap(),
    )
    .unwrap();
    assert!(written.contains("<string>systems.alab.aterm-vault-server</string>"));
    assert!(written.contains("<string>--advertise</string>"));
    assert!(written.contains("<string>m19</string>"));
    assert!(!written.contains("LimitLoadToSessionType"));
    assert!(!written.contains("<key>HOME</key>"));

    // After reboot before any login (`console_user == None`), `system/systems.alab.aterm-vault-server`
    // is loaded and running.
    sys.on("/usr/bin/pmset -g custom", ok(pmset_always_on));
    assert_eq!(
        daemon.probe(&sys, &ctx),
        Probe::Ok {
            now: "systems.alab.aterm-vault-server running in system (pid 7484)".into(),
        }
    );
}

#[test]
fn laptops_and_sleeping_desktops_keep_launchagent_and_vault_server_off_removes_daemon() {
    let sys = Fake::root();
    sys.file(EXE, b"");
    let mut ctx = ctx_with(&sys, &[], true);
    ctx.host_name = "m22-MacBook-Pro".into();
    ctx.facts.form = crate::model::Form::Laptop;

    let mut t = table();
    assert_eq!(t.vault_server, None, "C7a LaunchDaemon defaults on (F6)");
    let items = expand(&t, &ctx).unwrap();
    let daemon = items.last().unwrap();
    assert!(daemon.after().is_empty());
    assert!(matches!(
        daemon.probe(&sys, &ctx),
        Probe::NotApplicable { ref why } if why.contains("LaunchAgent")
    ));
    let laptop_run = crate::engine::run(
        &items,
        Mode::Apply,
        &sys,
        &ctx,
        &mut crate::engine::NoJournal,
    );
    assert_eq!(laptop_run[0].outcome, Outcome::Blocked);
    assert_eq!(laptop_run.last().unwrap().outcome, Outcome::NotApplicable);

    ctx.facts.form = crate::model::Form::Desktop;
    sys.on("/usr/bin/pmset -g custom", ok("AC Power:\n sleep\t\t10\n"));
    assert!(matches!(
        daemon.probe(&sys, &ctx),
        Probe::NotApplicable { ref why } if why.contains("sleep = 10")
    ));

    t.vault_advertise = Some("-bad".into());
    assert!(expand(&t, &ctx).is_err());

    // Turning `vault_server = false` boots out and unlinks an installed daemon even when
    // `aterm.app` is blocked.
    let mut off_table = table();
    off_table.vault_server = Some(false);
    let off_items = expand(&off_table, &ctx).unwrap();
    let off = off_items.last().unwrap();
    assert!(off.after().is_empty());
    sys.file(aterm_secrets::service::DAEMON_PLIST, b"<plist/>");
    let print_cmd = format!(
        "{} print system/{}",
        super::super::sharing::LAUNCHCTL,
        aterm_secrets::service::DAEMON_LABEL
    );
    let bootout_cmd = format!(
        "{} bootout system/{}",
        super::super::sharing::LAUNCHCTL,
        aterm_secrets::service::DAEMON_LABEL
    );
    sys.on(
        &print_cmd,
        ok("system/systems.alab.aterm-vault-server = {\n\tpid = 7484\n}\n"),
    );
    sys.on(
        &print_cmd,
        ok("system/systems.alab.aterm-vault-server = {\n\tpid = 7484\n}\n"),
    );
    sys.on(&print_cmd, err(super::super::sharing::NOT_LOADED, ""));
    assert_eq!(
        off.probe(&sys, &ctx),
        Probe::Change {
            from: "running in system (pid 7484)".into(),
            to: "off".into(),
        }
    );
    sys.on(&bootout_cmd, ok(""));
    let off_run = crate::engine::run(
        &off_items,
        Mode::Apply,
        &sys,
        &ctx,
        &mut crate::engine::NoJournal,
    );
    assert_eq!(off_run[0].outcome, Outcome::Blocked);
    assert_eq!(off_run.last().unwrap().outcome, Outcome::Changed);
    assert!(
        sys.read_file(Path::new(aterm_secrets::service::DAEMON_PLIST))
            .is_err()
    );
}
