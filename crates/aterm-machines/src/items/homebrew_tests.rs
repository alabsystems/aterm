// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::items::apps::tests::{
    PKGUTIL_HOMEBREW, SPCTL_HOMEBREW_OFF, admin, ctx_with, dir, err, ok, out_status, sha_of,
    stage_path,
};
use crate::model::{Mode, Outcome};
use crate::port::{Fake, Ran, Stat};

const HOMEBREW_SHA: &str = "e021295947a4a9b7ca1a8eff310c9ea1db14d923c5acf7d79e325b60a81f1a15";

const INSTALLER_HOMEBREW: &str = "installer: Package name is Homebrew\ninstaller: Installing at base path /\ninstaller: The install was successful.\n";

const BREW_VERSION: &str = "Homebrew >=4.3.0 (shallow or no git repository)\n";

const ENV: &str = "HOMEBREW_NO_ANALYTICS=1 HOMEBREW_NO_ENV_HINTS=1";

fn table(sha: &str) -> Table {
    Table {
        pkg: Some(Artifact {
            url: "https://github.com/Homebrew/brew/releases/download/7.0.9/Homebrew-7.0.9.pkg"
                .into(),
            sha256: sha.into(),
            team_id: Some("927JGANW46".into()),
            unsigned: false,
        }),
        owner: None,
        formulae: vec!["jq".into()],
        casks: vec![],
    }
}

fn owned_by(uid: u32) -> Stat {
    Stat { uid, ..dir() }
}

fn brew_installed(sys: &Fake) {
    sys.stat_of(
        BREW,
        Stat {
            dir: false,
            ..owned_by(501)
        },
    );
    sys.stat_of(PREFIX, owned_by(501));
}

fn ruby_in_place(sys: &Fake) {
    sys.file(&format!("{VENDOR}/portable-ruby-version"), b"4.0.7\n");
    sys.link(&format!("{VENDOR}/portable-ruby/current"), "4.0.7");
    let ruby = format!("{VENDOR}/portable-ruby/4.0.7/bin/ruby");
    sys.file(&ruby, b"ruby").stat_of(
        &ruby,
        Stat {
            mode: 0o555,
            dir: false,
            ..owned_by(501)
        },
    );
}

#[test]
fn the_pkg_waits_for_command_line_tools_and_each_formula_waits_for_its_ruby() {
    let ctx = ctx_with(&Fake::root(), &[], true);
    let items = expand(&table(HOMEBREW_SHA), &ctx).unwrap();
    let ids: Vec<String> = items.iter().map(|i| i.id()).collect();
    assert_eq!(
        ids,
        ["homebrew.pkg", "homebrew.ruby", "homebrew.formula.jq"]
    );
    assert_eq!(items[0].after(), ["developer.command-line-tools"]);
    assert_eq!(items[1].after(), ["homebrew.pkg"]);
    assert_eq!(items[2].after(), ["homebrew.pkg", "homebrew.ruby"]);
}

#[test]
fn an_intel_mac_is_not_applicable_and_an_unread_architecture_is_unknown() {
    let sys = Fake::root();
    let mut ctx = ctx_with(&sys, &[], true);
    ctx.facts.arch = "x86_64".into();
    let items = expand(&table(HOMEBREW_SHA), &ctx).unwrap();
    for item in &items {
        assert!(item.after().is_empty());
        assert!(matches!(
            item.probe(&sys, &ctx),
            Probe::NotApplicable { .. }
        ));
    }
    let report = crate::engine::run(
        &items,
        Mode::Apply,
        &sys,
        &ctx,
        &mut crate::engine::NoJournal,
    );
    assert!(
        report
            .iter()
            .all(|row| row.outcome == Outcome::NotApplicable),
        "{report:?}"
    );
    ctx.facts.arch = String::new();
    assert!(matches!(items[0].probe(&sys, &ctx), Probe::Unusable { .. }));
    let unread = expand(&table(HOMEBREW_SHA), &ctx).unwrap();
    assert_eq!(unread[0].after(), ["developer.command-line-tools"]);
    assert_eq!(unread[1].after(), ["homebrew.pkg"]);
    assert_eq!(unread[2].after(), ["homebrew.pkg", "homebrew.ruby"]);
    assert!(sys.calls().is_empty());
}

#[test]
fn an_owner_this_mac_lacks_is_refused_at_expansion() {
    let ctx = ctx_with(&Fake::root(), &[], true);
    let mut t = table(HOMEBREW_SHA);
    t.owner = Some("nobody-here".into());
    assert!(expand(&t, &ctx).err().unwrap().contains("no such account"));
    let mut t = table(HOMEBREW_SHA);
    t.formulae = vec!["--force".into()];
    assert!(expand(&t, &ctx).err().unwrap().contains("not a formula"));
}

#[test]
fn without_homebrew_the_pkg_plans_an_install_from_the_cache() {
    let sys = Fake::root();
    let ctx = ctx_with(&sys, &[], true);
    let items = expand(&table(HOMEBREW_SHA), &ctx).unwrap();
    assert!(
        matches!(items[0].probe(&sys, &ctx), Probe::Blocked { why } if why.contains("not in the cache"))
    );
    let ctx = ctx_with(&sys, &[(HOMEBREW_SHA, b"pkg")], true);
    assert_eq!(
        items[0].probe(&sys, &ctx),
        Probe::Change {
            from: "absent".into(),
            to: "Homebrew in /opt/homebrew, owned by admin".into()
        }
    );
    assert!(matches!(items[1].probe(&sys, &ctx), Probe::Change { .. }));
    assert!(matches!(items[2].probe(&sys, &ctx), Probe::Change { .. }));
}

#[test]
fn installing_the_pkg_names_its_owner_in_a_root_only_file_first() {
    let sys = Fake::root();
    let bytes = b"Homebrew.pkg";
    let sha = sha_of(bytes);
    let stage = stage_path(0);
    let ctx = ctx_with(&sys, &[(&sha, bytes)], true);
    sys.on(
        &format!("/usr/sbin/pkgutil --check-signature {stage}/Homebrew-7.0.9.pkg"),
        ok(PKGUTIL_HOMEBREW),
    );
    sys.on(
        &format!("/usr/sbin/spctl -a -vv -t install {stage}/Homebrew-7.0.9.pkg"),
        err(0, SPCTL_HOMEBREW_OFF),
    );
    sys.on(&format!("/bin/chmod -N {PKG_USER_PLIST}"), ok(""));
    sys.on(
        &format!("/usr/sbin/installer -pkg {stage}/Homebrew-7.0.9.pkg -target /"),
        ok(INSTALLER_HOMEBREW),
    );
    let items = expand(&table(&sha), &ctx).unwrap();
    items[0].apply(&sys, &ctx).unwrap();
    let calls = sys.calls();
    let at = |c: &str| {
        calls
            .iter()
            .position(|x| x == c)
            .unwrap_or_else(|| panic!("{c}: {calls:#?}"))
    };
    assert!(!calls.iter().any(|c| c.starts_with("admin$")), "{calls:#?}");
    let write = at(&format!("write {PKG_USER_PLIST} 600"));
    assert!(write < at(&format!("chown 0:0 {PKG_USER_PLIST}")));
    assert!(
        at(&format!("/bin/chmod -N {PKG_USER_PLIST}"))
            < at(&format!(
                "/usr/sbin/installer -pkg {stage}/Homebrew-7.0.9.pkg -target /"
            ))
    );
    assert!(
        at(&format!(
            "/usr/sbin/spctl -a -vv -t install {stage}/Homebrew-7.0.9.pkg"
        )) < write
    );
    assert!(
        at(&format!("copy /c/{sha} {stage}/Homebrew-7.0.9.pkg 600"))
            < at(&format!(
                "/usr/sbin/pkgutil --check-signature {stage}/Homebrew-7.0.9.pkg"
            ))
    );
    let plist = String::from_utf8(sys.written(PKG_USER_PLIST).unwrap()).unwrap();
    assert!(
        plist.contains("<key>HOMEBREW_PKG_USER</key>\n\t<string>admin</string>"),
        "{plist}"
    );
    assert!(
        items[0]
            .detail(&sys, &ctx)
            .unwrap()
            .contains("override=security disabled")
    );
}

#[test]
fn a_pkg_signed_by_another_installer_never_runs() {
    let sys = Fake::root();
    let bytes = b"Homebrew.pkg";
    let sha = sha_of(bytes);
    let stage = stage_path(0);
    let ctx = ctx_with(&sys, &[(&sha, bytes)], true);
    sys.on(
        &format!("/usr/sbin/pkgutil --check-signature {stage}/Homebrew-7.0.9.pkg"),
        ok(&PKGUTIL_HOMEBREW.replace("927JGANW46", "AAAAAAAAAA")),
    );
    let items = expand(&table(&sha), &ctx).unwrap();
    assert!(
        items[0]
            .apply(&sys, &ctx)
            .unwrap_err()
            .contains("not a Developer ID Installer of team 927JGANW46")
    );
    assert!(
        !sys.calls()
            .iter()
            .any(|c| c.contains("installer -pkg") || c.starts_with("write"))
    );
}

#[test]
fn homebrew_owned_by_its_owner_is_ok_and_owned_by_another_is_blocked() {
    let sys = Fake::root();
    sys.stat_of(
        BREW,
        Stat {
            dir: false,
            ..owned_by(501)
        },
    );
    sys.stat_of(PREFIX, owned_by(501));
    sys.on(
        &format!("admin$ {ENV} HOMEBREW_NO_AUTO_UPDATE=1 {BREW} --version"),
        ok(BREW_VERSION),
    );
    let ctx = ctx_with(&sys, &[], true);
    let items = expand(&table(HOMEBREW_SHA), &ctx).unwrap();
    assert_eq!(
        items[0].probe(&sys, &ctx),
        Probe::Ok {
            now: "Homebrew >=4.3.0 (shallow or no git repository) in /opt/homebrew, owned by admin"
                .into()
        }
    );
    sys.stat_of(PREFIX, owned_by(502));
    assert!(
        matches!(items[0].probe(&sys, &ctx), Probe::Blocked { why } if why.contains("belongs to uid 502"))
    );
}

#[test]
fn a_formula_is_read_and_installed_as_the_owner_with_homebrew_s_quiet_switches() {
    let sys = Fake::root();
    brew_installed(&sys);
    ruby_in_place(&sys);
    let list =
        format!("admin$ {ENV} HOMEBREW_NO_AUTO_UPDATE=1 {BREW} list --formula --versions jq");
    sys.on(&list, out_status(1, ""));
    sys.on(&list, ok("jq 1.8.2\n"));
    sys.on(
        &format!("admin$ {ENV} {BREW} install --formula jq"),
        ok("==> Pouring jq--1.8.2.arm64_golden_gate.bottle.1.tar.gz\n"),
    );
    let ctx = ctx_with(&sys, &[], true);
    let items = expand(&table(HOMEBREW_SHA), &ctx).unwrap();
    let rows = crate::engine::run(
        &items[1..],
        Mode::Apply,
        &sys,
        &ctx,
        &mut crate::engine::NoJournal,
    );
    assert_eq!(rows[0].outcome, Outcome::Ok, "{:?}", rows[0]);
    assert_eq!(rows[1].outcome, Outcome::Changed, "{:?}", rows[1]);
    assert_eq!(rows[1].from.as_deref(), Some("absent"));
    assert_eq!(
        rows[1].revert.as_deref(),
        Some("sudo -u admin /opt/homebrew/bin/brew uninstall --formula jq")
    );
    assert_eq!(
        items[2].probe(&sys, &ctx),
        Probe::Ok {
            now: "jq 1.8.2".into()
        }
    );
}

#[test]
fn a_brew_list_that_fails_out_loud_is_no_answer_rather_than_absent() {
    let sys = Fake::root();
    brew_installed(&sys);
    ruby_in_place(&sys);
    sys.on(
        &format!("admin$ {ENV} HOMEBREW_NO_AUTO_UPDATE=1 {BREW} list --formula --versions jq"),
        err(
            1,
            "Error: Permission denied @ dir_s_mkdir - /opt/homebrew/var",
        ),
    );
    let ctx = ctx_with(&sys, &[], true);
    let items = expand(&table(HOMEBREW_SHA), &ctx).unwrap();
    assert!(
        matches!(items[2].probe(&sys, &ctx), Probe::Unusable { why } if why.contains("Permission denied")),
    );
}

#[test]
fn a_failed_brew_install_says_brew_s_last_words() {
    let sys = Fake::root();
    sys.stat_of(
        BREW,
        Stat {
            dir: false,
            ..owned_by(501)
        },
    );
    sys.on(
        &format!("admin$ {ENV} {BREW} install --cask nosuchcask"),
        Ran {
            status: Some(1),
            stdout: b"==> Searching\n".to_vec(),
            stderr: b"Error: Cask 'nosuchcask' is unavailable: No Cask with this name exists.\n"
                .to_vec(),
        },
    );
    let mut t = table(HOMEBREW_SHA);
    t.formulae.clear();
    t.casks = vec!["nosuchcask".into()];
    let ctx = ctx_with(&sys, &[], true);
    let items = expand(&t, &ctx).unwrap();
    assert_eq!(items[2].id(), "homebrew.cask.nosuchcask");
    let why = items[2].apply(&sys, &ctx).unwrap_err();
    assert!(why.contains("No Cask with this name exists"), "{why}");
}

#[test]
fn the_owner_plist_is_the_document_homebrew_s_scripts_read() {
    assert_eq!(
        pkg_user_plist(&admin().name),
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n<plist version=\"1.0\">\n<dict>\n\t<key>HOMEBREW_PKG_USER</key>\n\t<string>admin</string>\n</dict>\n</plist>\n"
    );
}

const CONFIG: &str = "admin$ HOMEBREW_NO_ANALYTICS=1 HOMEBREW_NO_ENV_HINTS=1 HOMEBREW_NO_AUTO_UPDATE=1 /opt/homebrew/bin/brew config";

#[test]
fn homebrew_without_its_ruby_fetches_only_the_ruby_as_the_owner() {
    let sys = Fake::root();
    brew_installed(&sys);
    sys.file(&format!("{VENDOR}/portable-ruby-version"), b"4.0.7\n");
    let ctx = ctx_with(&sys, &[], true);
    let items = expand(&table(HOMEBREW_SHA), &ctx).unwrap();
    let Probe::Change { from, .. } = items[1].probe(&sys, &ctx) else {
        panic!("{:?}", items[1].probe(&sys, &ctx));
    };
    assert_eq!(from, "not fetched");
    sys.on(CONFIG, ok(""));
    items[1].apply(&sys, &ctx).unwrap();
    assert_eq!(sys.calls(), [CONFIG]);
    ruby_in_place(&sys);
    assert_eq!(
        items[1].probe(&sys, &ctx),
        Probe::Ok {
            now: "portable Ruby 4.0.7".into()
        }
    );
}

#[test]
fn a_ruby_other_than_the_one_homebrew_names_is_fetched_again_and_an_unread_one_is_unknown() {
    let sys = Fake::root();
    brew_installed(&sys);
    ruby_in_place(&sys);
    sys.file(&format!("{VENDOR}/portable-ruby-version"), b"4.0.8\n");
    let ctx = ctx_with(&sys, &[], true);
    let items = expand(&table(HOMEBREW_SHA), &ctx).unwrap();
    assert!(
        matches!(items[1].probe(&sys, &ctx), Probe::Change { from, .. } if from.contains("4.0.7")),
    );
    sys.stat_fails(
        &format!("{VENDOR}/portable-ruby/current"),
        std::io::ErrorKind::PermissionDenied,
    );
    assert!(matches!(items[1].probe(&sys, &ctx), Probe::Unusable { .. }));
    assert!(matches!(items[2].probe(&sys, &ctx), Probe::Unusable { .. }));
    let sys = Fake::root();
    brew_installed(&sys);
    assert!(matches!(items[1].probe(&sys, &ctx), Probe::Unusable { .. }));
    assert!(sys.calls().is_empty(), "{:?}", sys.calls());
}

#[test]
fn a_formula_waits_for_the_ruby_and_never_starts_its_fetch() {
    let sys = Fake::root();
    brew_installed(&sys);
    sys.file(&format!("{VENDOR}/portable-ruby-version"), b"4.0.7\n");
    sys.on(
        &format!("admin$ {ENV} HOMEBREW_NO_AUTO_UPDATE=1 {BREW} --version"),
        ok(BREW_VERSION),
    );
    let ctx = ctx_with(&sys, &[], true);
    let items = expand(&table(HOMEBREW_SHA), &ctx).unwrap();
    let outcomes = |mode| {
        crate::engine::run(&items, mode, &sys, &ctx, &mut crate::engine::NoJournal)
            .into_iter()
            .map(|r| r.outcome)
            .collect::<Vec<_>>()
    };
    assert_eq!(
        outcomes(Mode::Plan),
        [Outcome::Ok, Outcome::Planned, Outcome::Planned]
    );
    sys.on(CONFIG, err(1, "curl: (6) Could not resolve host: ghcr.io"));
    assert_eq!(
        outcomes(Mode::Apply),
        [Outcome::Ok, Outcome::Failed, Outcome::Blocked]
    );
    assert!(
        !sys.calls().iter().any(|c| c.contains(" list ")),
        "{:#?}",
        sys.calls()
    );
}

#[test]
fn formula_names_reject_local_path_segments_and_empty_slash_parts() {
    for valid in ["jq", "python@3.13", "oven-sh/bun/bun", "g++", "node.js"] {
        check_name(valid).unwrap();
    }
    for invalid in [
        "", ".", "./jq", "jq/.", "jq/", "/jq", "tap//jq", "tap/./jq", "../jq", "-jq",
    ] {
        assert!(check_name(invalid).is_err(), "{invalid:?}");
    }
}
