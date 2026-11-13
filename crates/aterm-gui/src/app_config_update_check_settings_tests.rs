// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

#[test]
fn persisted_check_settings_bind_source_and_manual_policy_in_one_parse() {
    let defaults = super::update_check_settings_from_text("").unwrap();
    assert!(defaults.auto_apply);
    assert_eq!(defaults.source, aterm_update::Source::resolve(None, None));
    let manual = super::update_check_settings_from_text(
        "[update]\nowner = \"private-org\"\nrepo = \"fork\"\nauto_apply = false\n",
    )
    .unwrap();
    assert!(!manual.auto_apply);
    assert_eq!(
        manual.source,
        aterm_update::Source::resolve(Some("private-org"), Some("fork"))
    );
    assert!(
        super::update_check_settings_from_text("[update]\nauto_apply = invalid").is_none(),
        "malformed existing policy must not fall back to automatic"
    );
}

/// A CONFIG FILE OTHERS CAN WRITE STOPS NO UPDATE: it paused the node job's checks and applies
/// for good on a Mac with no window, whatever the file said. It can still hold an update back,
/// and a source it names is not followed. A file with no text to read holds nothing back, as
/// the boot apply reads it. CONTROLS: the config's own readers refuse this file, and only text
/// naming `auto_apply` unreadably stays unknown.
#[cfg(unix)]
#[test]
fn a_config_file_others_can_write_can_hold_an_update_back_and_can_name_no_source() {
    use std::os::unix::fs::PermissionsExt as _;
    if !crate::test_alone::alone() {
        return;
    }
    let dir = aterm_tempfile::tempdir().expect("tempdir");
    crate::test_env::scoped("XDG_CONFIG_HOME", dir.path(), || {
        let path = super::config_path().expect("a config path");
        std::fs::create_dir_all(path.parent().expect("its folder")).expect("the folder");
        let write = |text: &str| {
            std::fs::write(&path, text).expect("write");
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o664))
                .expect("its mode");
        };
        write("font_px = 12.0\n");
        assert!(
            crate::native_config_service::VersionedConfigService::observe_path(&path, true)
                .is_err(),
            "CONTROL: the config's readers refuse this file"
        );
        let plain = super::update_check_settings_setting().expect("it says nothing of updates");
        assert!(plain.auto_apply);
        assert_eq!(plain.source, aterm_update::Source::resolve(None, None));
        write("[update]\nauto_apply = false\n");
        assert!(
            !super::update_check_settings_setting()
                .expect("the policy reads")
                .auto_apply,
            "it holds an update back"
        );
        write("[update]\nowner = \"someone\"\nrepo = \"else\"\n");
        let named = super::update_check_settings_setting().expect("it names a source");
        assert!(named.auto_apply);
        assert_eq!(
            named.source,
            aterm_update::Source::resolve(None, None),
            "a source it names is not followed"
        );
        assert_ne!(
            aterm_update::Source::resolve(Some("someone"), Some("else")),
            named.source,
            "CONTROL: this build would follow one from a file it trusts"
        );
        write("[update\n");
        assert!(
            super::update_check_settings_setting()
                .expect("text that is not TOML and names no such switch")
                .auto_apply
        );
        write("[update\nauto_apply = false\n");
        assert!(
            super::update_check_settings_setting().is_none(),
            "CONTROL: text that is not TOML and names the switch stays unknown"
        );
        std::fs::write(&path, b"[update]\nauto_apply = false\n# caf\xe9\n").expect("write");
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))
            .expect("this account's alone, so only the bytes can be refused");
        assert!(
            crate::native_config_service::VersionedConfigService::observe_path(&path, true)
                .is_err(),
            "CONTROL: the config's readers refuse bytes that are not UTF-8"
        );
        assert!(
            !super::update_check_settings_setting()
                .expect("the policy reads")
                .auto_apply,
            "a stray byte in a comment hides no false"
        );
        std::fs::remove_file(&path).expect("remove");
        assert!(
            super::update_check_settings_setting()
                .expect("no file")
                .auto_apply
        );
        std::os::unix::fs::symlink(dir.path().join("gone"), &path).expect("the link");
        assert!(
            super::update_check_settings_setting()
                .expect("a dangling link has no text to read")
                .auto_apply
        );
    });
}

/// A LAUNCH ON A FILE THAT DID NOT LOAD KEEPS ITS UPDATE SWITCHES: a window on `font_px = "12"`
/// ran on defaults and installed by itself against a readable `auto_apply = false`, while the
/// boot apply and the node job honoured it. Settings reads the service, so the service carries
/// them. It names no source. CONTROLS: the whole `Config` refuses the text; a file with no such
/// switch, or no file, leaves both on; and a write finds the broken file, not the stand-in.
#[test]
fn a_launch_on_a_file_that_did_not_load_keeps_its_update_switches() {
    let dir = aterm_tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("aterm.toml");
    let kept = |text: &str| {
        std::fs::write(&path, text).expect("write");
        crate::native_config_service::VersionedConfigService::new(
            super::update_switches_stand_in(Some(&path)).text,
        )
        .expect("the stand-in is a config")
    };
    let both_off = "font_px = \"12\"\n[update]\nenabled = false\nauto_apply = false\n\
                    owner = \"private-org\"\nrepo = \"fork\"\n";
    assert!(
        aterm_toml::from_str::<super::Config>(both_off).is_err(),
        "CONTROL: the whole Config refuses this text"
    );
    let service = kept(both_off);
    let snapshot = service.snapshot();
    assert!(
        !super::update_auto_apply(&snapshot.config),
        "auto_apply = false stands"
    );
    assert!(
        !super::update_checks_automatic(&snapshot.config),
        "enabled = false stands"
    );
    assert!(
        snapshot
            .config
            .update
            .as_ref()
            .is_none_or(|update| update.owner.is_none() && update.repo.is_none()),
        "the file names no source"
    );
    assert_eq!(
        snapshot
            .semantic_values
            .get("update.auto_apply")
            .map(String::as_str),
        Some("false"),
        "the values Settings seeds from say so too"
    );
    let plan = service.persistence_plan(service.snapshot());
    assert_ne!(
        plan.expected_text.as_bytes(),
        both_off.as_bytes(),
        "a write's check of the disk's bytes refuses, so the stand-in never reaches the file"
    );
    let one_off = kept("font_px = \"12\"\n[update]\nauto_apply = false\n").snapshot();
    assert!(!super::update_auto_apply(&one_off.config));
    assert!(super::update_checks_automatic(&one_off.config));
    for on in [kept("font_px = \"12\"\n"), kept("[update\n")] {
        let on = on.snapshot();
        assert!(super::update_auto_apply(&on.config));
        assert!(super::update_checks_automatic(&on.config));
    }
    let wrong_type = kept("[update]\nauto_apply = \"no\"\n").snapshot();
    assert!(
        !super::update_auto_apply(&wrong_type.config),
        "a wrong type may be a person's false"
    );
    assert!(super::update_checks_automatic(&wrong_type.config));
    assert_eq!(super::update_switches_stand_in(None).text, "");
    let broken = kept("[update\nauto_apply = false\n").snapshot();
    assert!(
        !super::update_auto_apply(&broken.config),
        "text that is not TOML and names it reads as the boot's reader takes it"
    );
    assert!(super::update_checks_automatic(&broken.config));
    assert!(super::update_auto_apply(
        &kept("[update\nauto_apply = true\n").snapshot().config
    ));
}

/// A LAUNCH OVER A FAULT THAT CAN PASS READS AS HELD: it ran on `auto_apply` on, so its lane
/// installed what the boot's own read had just held for a person's possible `false`. CONTROLS: a
/// fault that stands and a file with no text read as on, and a good read stands on its own text,
/// which is what the config watcher's next look loads over the stand-in. Only the fault's off
/// says it is a fault's, so only that one ends at whatever text the file gives next.
#[test]
fn a_launch_over_a_fault_that_can_pass_reads_as_held() {
    use std::io::{Error, ErrorKind};
    let of_read = |read: std::io::Result<Option<String>>| {
        let stand_in = super::update_switches_stand_in_of(read);
        let service = crate::native_config_service::VersionedConfigService::new(stand_in.text)
            .expect("the stand-in is a config");
        let snapshot = service.snapshot();
        (
            super::update_auto_apply(&snapshot.config),
            super::update_checks_automatic(&snapshot.config),
            stand_in.origin,
        )
    };
    let refused = super::UpdateSwitchOrigin::RefusedRead;
    assert_eq!(
        of_read(Err(Error::from(ErrorKind::TimedOut))),
        (false, true, super::UpdateSwitchOrigin::FaultHold),
        "only the switch that could install is held"
    );
    assert_eq!(
        of_read(Err(Error::from(ErrorKind::PermissionDenied))),
        (true, true, refused)
    );
    assert_eq!(of_read(Ok(None)), (true, true, refused));
    assert_eq!(
        of_read(Ok(Some("font_px = \"12\"\n".to_owned()))),
        (true, true, refused),
        "the fault passed and the file names no switch"
    );
    assert_eq!(
        of_read(Ok(Some("[update]\nauto_apply = false\n".to_owned()))),
        (false, true, refused),
        "CONTROL: that off is the file's own"
    );
}

/// A REFUSED READ GIVES WHAT THE FILE HOLDS, as a launch with no fault would read it. CONTROL:
/// while a fault that can pass lasts it gives nothing, so the switches stand.
#[test]
fn a_refused_read_gives_the_text_unless_a_fault_can_pass() {
    use std::io::{Error, ErrorKind};
    let text = super::refused_read_text;
    assert_eq!(text(Err(Error::from(ErrorKind::TimedOut))), None);
    assert_eq!(
        text(Err(Error::from(ErrorKind::PermissionDenied))).as_deref(),
        Some("")
    );
    assert_eq!(text(Ok(None)).as_deref(), Some(""));
    assert_eq!(
        text(Ok(Some("[update\nauto_apply = false\n".to_owned()))).as_deref(),
        Some("[update\nauto_apply = false\n")
    );
}

/// WHAT A READ OF A REFUSED FILE FOUND DECIDES THE POLICY: a fault that can pass holds this look,
/// since a person's `false` may be behind it, and one that stands reads as the defaults, or it
/// would stop updates for good.
#[test]
fn a_refused_file_s_read_decides_the_update_policy() {
    use std::io::{Error, ErrorKind};
    assert!(super::update_check_settings_of_read(Err(Error::from(ErrorKind::TimedOut))).is_none());
    assert!(
        super::update_check_settings_of_read(Err(Error::from(ErrorKind::PermissionDenied)))
            .expect("a fault that stands reads as the defaults")
            .auto_apply
    );
    assert!(
        super::update_check_settings_of_read(Ok(None))
            .expect("no text reads as the defaults")
            .auto_apply
    );
    assert!(
        !super::update_check_settings_of_read(Ok(Some(
            "[update]\nauto_apply = false\n".to_owned()
        )))
        .expect("the text reads")
        .auto_apply
    );
    assert!(
        super::update_check_settings_of_read(Ok(Some("[update\nauto_apply = false\n".to_owned())))
            .is_none(),
        "text that is not TOML and names the switch stays unknown"
    );
}

/// A RELOAD THAT DID NOT LOAD STILL TAKES THE SWITCHES IT CAN READ: `auto_apply = false` added to
/// a file with a mistake elsewhere was ignored until the mistake was fixed, and taking it out of
/// a file still broken went on holding, as did text that is not TOML, which every other reader
/// reads as on. A value of the wrong type reads as off, since it may be a person's `false`. CONTROLS:
/// `enabled` in text that is not TOML stays as it stands, and every other key of `[update]` stays.
#[test]
fn a_failed_reload_takes_the_update_switches_its_text_can_say() {
    let reload = |update: Option<super::UpdateConfig>, text: &str| {
        let mut update = update;
        super::UpdateSwitches::of_text(text).apply_to(&mut update);
        update
    };
    let off = reload(None, "font_px = \"12\"\n[update]\nauto_apply = false\n")
        .expect("the switch is off");
    assert_eq!(off.auto_apply, Some(false));
    assert_eq!(off.enabled, None);
    let out = reload(Some(off.clone()), "font_px = \"12\"\n");
    assert_eq!(out, None, "taking it out of a file still broken lets go");
    assert_eq!(
        reload(
            Some(off.clone()),
            "font_px = \"12\"\n[update]\nauto_apply = true\n"
        )
        .and_then(|update| update.auto_apply),
        Some(true)
    );
    assert_eq!(
        reload(Some(off.clone()), "[update\n"),
        None,
        "text that is not TOML and does not name it reads as on, as the boot's reader takes it"
    );
    let no_checks = super::UpdateConfig {
        enabled: Some(false),
        ..super::UpdateConfig::default()
    };
    assert_eq!(
        reload(Some(no_checks.clone()), "[update\n"),
        Some(no_checks),
        "CONTROL: of `enabled` such text says nothing"
    );
    assert_eq!(
        reload(None, "[update\nauto_apply = false\n").and_then(|update| update.auto_apply),
        Some(false),
        "unless it names the switch with any value but true"
    );
    assert_eq!(
        reload(Some(off.clone()), "[update\nauto_apply = true\n"),
        None,
        "nor when it names it true"
    );
    assert_eq!(
        reload(None, "[update]\nauto_apply = \"no\"\n").and_then(|update| update.auto_apply),
        Some(false),
        "a value of the wrong type may be a person's false"
    );
    let with_source = super::UpdateConfig {
        owner: Some("private-org".to_string()),
        ..super::UpdateConfig::default()
    };
    assert_eq!(
        reload(
            Some(with_source.clone()),
            "font_px = \"12\"\n[update]\nowner = 5\n"
        ),
        Some(with_source),
        "CONTROL: its source stays as it stands"
    );
}

/// A WRONG TYPE ELSEWHERE IN THE FILE SAYS NOTHING OF THE POLICY: the node job read a whole
/// `Config`, so `font_px = "12"` paused its checks and applies for good on a Mac with no window,
/// while a window on the same file kept updating. CONTROL: the whole `Config` refuses the same
/// text. Neither a wrong `owner` or `repo` nor text that is not TOML pauses it, and only
/// `auto_apply` itself reading unreadably does: a person's `false` may be what cannot be read.
#[test]
fn a_wrong_type_elsewhere_in_the_file_says_nothing_of_the_update_policy() {
    let manual = "font_px = \"12\"\n[update]\nenabled = \"yes\"\nowner = \"private-org\"\n\
                  repo = \"fork\"\nauto_apply = false\n";
    assert!(
        aterm_toml::from_str::<super::Config>(manual).is_err(),
        "CONTROL: the whole Config refuses this text"
    );
    let read = super::update_check_settings_from_text(manual).expect("the policy reads");
    assert!(!read.auto_apply, "the person's policy");
    assert_eq!(
        read.source,
        aterm_update::Source::resolve(Some("private-org"), Some("fork"))
    );
    for defaults in [
        "font_px = \"12\"\n",
        "[update]\nowner = 5\n",
        "[update]\nrepo = 5\n",
        "update = true\n",
        "font_px = big\n",
        "[update\n",
        "[update\nauto_apply = true\n",
        "[update\n# auto_apply = false\n",
        "update = true\nfont_px = big\n",
    ] {
        let read = super::update_check_settings_from_text(defaults)
            .unwrap_or_else(|| panic!("{defaults:?} pauses nothing"));
        assert!(read.auto_apply, "{defaults:?}");
        assert_eq!(
            read.source,
            aterm_update::Source::resolve(None, None),
            "{defaults:?}"
        );
    }
    for unread in [
        "[update]\nauto_apply = \"no\"\n",
        "update = 5\n",
        "update = false\n",
        "[[update]]\nauto_apply = false\n",
        "[update\nauto_apply = false\n",
        "[update\nauto_apply = maybe\n",
        "[update\nauto_apply\n",
        "update.auto_apply = false\n[update\n",
        "update = false\nfont_px = big\n",
    ] {
        assert!(
            super::update_check_settings_from_text(unread).is_none(),
            "{unread:?}"
        );
    }
}
