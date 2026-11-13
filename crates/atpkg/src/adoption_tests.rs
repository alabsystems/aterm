// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::*;

#[test]
fn each_refusal_holds_alone_and_the_first_one_answers() {
    let consenting = PackagesConfig::default();
    let off = PackagesConfig {
        auto_install: Some(false),
        ..PackagesConfig::default()
    };
    let ignored = PackagesConfig {
        ignored_prefix: Some(std::path::PathBuf::from("/elsewhere")),
        ..PackagesConfig::default()
    };
    let ignored_and_off = PackagesConfig {
        auto_install: Some(false),
        ignored_prefix: Some(std::path::PathBuf::from("/elsewhere")),
        ..PackagesConfig::default()
    };
    let unreadable = PackagesConfig {
        unreadable: true,
        ..PackagesConfig::default()
    };
    let rows: [(bool, &PackagesConfig, bool, Adoption); 9] = [
        (false, &consenting, true, Adoption::Consented),
        (true, &consenting, true, Adoption::Declined),
        (false, &ignored, true, Adoption::IgnoredPrefix),
        (false, &off, true, Adoption::AutoInstallOff),
        (false, &unreadable, true, Adoption::AutoInstallOff),
        (false, &consenting, false, Adoption::ManagerDisabled),
        (true, &ignored_and_off, false, Adoption::Declined),
        (false, &ignored_and_off, false, Adoption::IgnoredPrefix),
        (false, &off, false, Adoption::AutoInstallOff),
    ];
    for (declined, cfg, enabled, want) in rows {
        assert_eq!(
            adoption_with(declined, cfg, enabled),
            want,
            "declined={declined} enabled={enabled} cfg={cfg:?}"
        );
    }
}

/// EVERY OFF OF A PERSON'S IS NAMED, AND ONLY CONSENT WITH KEEP ALAB TOOLS UP TO DATE ON
/// INSTALLS: `enabled = false`, a decline, an ignored prefix, `auto_install = false` and an
/// unreadable table each refuse alone with their own words, a retired spelling
/// (`auto_update`, `seed_install`) is named as the key the file holds, a person's off is named
/// before an inert manager, and the default table on an armed build is the one `None`.
#[test]
fn unattended_installs_need_consent_with_automatic_updates_on() {
    let with = |f: fn(&mut PackagesConfig)| {
        let mut cfg = PackagesConfig::default();
        f(&mut cfg);
        cfg
    };
    let consenting = PackagesConfig::default();
    let updates_off = with(|c| c.enabled = Some(false));
    let retired_updates_off = with(|c| c.auto_update = Some(false));
    let retired_over_on = with(|c| {
        c.enabled = Some(true);
        c.auto_update = Some(false);
    });
    let both_updates_off = with(|c| {
        c.enabled = Some(false);
        c.auto_update = Some(false);
    });
    let off = with(|c| c.auto_install = Some(false));
    let retired_off = with(|c| c.seed_install = Some(false));
    let retired_off_overruled = with(|c| {
        c.auto_install = Some(true);
        c.seed_install = Some(false);
    });
    let ignored = with(|c| c.ignored_prefix = Some(std::path::PathBuf::from("/elsewhere")));
    let unreadable = PackagesConfig::unreadable_table();
    assert_eq!(
        unattended_off_with(false, &consenting, true),
        None,
        "CONTROL"
    );
    let said = |declined, cfg: &PackagesConfig, armed| {
        unattended_off_with(declined, cfg, armed).unwrap_or_else(|| panic!("{cfg:?}"))
    };
    assert_eq!(
        unattended_off_with(false, &retired_off_overruled, true),
        None,
        "CONTROL: `auto_install` decides over a retired `seed_install`"
    );
    assert_eq!(said(false, &updates_off, true), UPDATES_OFF);
    assert_eq!(said(false, &both_updates_off, true), UPDATES_OFF);
    assert_eq!(said(false, &retired_updates_off, true), RETIRED_UPDATES_OFF);
    assert_eq!(said(false, &retired_over_on, true), RETIRED_UPDATES_OFF);
    assert_eq!(
        said(false, &updates_off, false),
        UPDATES_OFF,
        "the person's off first"
    );
    assert!(said(true, &consenting, true).contains("uninstall --all"));
    assert!(said(true, &updates_off, true).contains("uninstall --all"));
    assert!(said(false, &ignored, true).contains("prefix"));
    for (cfg, key) in [(&off, "auto_install"), (&retired_off, "seed_install")] {
        let words = said(false, cfg, true);
        assert!(words.contains(&format!("{key} = false")), "{words}");
    }
    assert!(said(false, &unreadable, true).contains("cannot be read"));
    assert!(said(false, &consenting, false).contains("inert"));
    let l = crate::testdir::layout("atpkg-unattended-");
    assert_eq!(
        unattended_install_off(&l, &consenting),
        (!crate::manager_enabled()).then(|| said(false, &consenting, false)),
        "an empty store under the default table installs wherever the build is armed"
    );
    std::fs::create_dir_all(&l.prefix).unwrap();
    std::fs::write(l.declined(), "# declined\n").unwrap();
    assert!(
        unattended_install_off(&l, &consenting).is_some_and(|w| w.contains("uninstall --all")),
        "the decline marker on the store is read"
    );
}

#[test]
fn the_decline_marker_on_the_store_refuses() {
    let l = crate::testdir::layout("atpkg-adoption-");
    let cfg = PackagesConfig::default();
    assert_eq!(
        adoption_consented(&l, &cfg),
        crate::manager_enabled(),
        "an empty store under the default config consents wherever the build is armed"
    );
    std::fs::create_dir_all(&l.prefix).unwrap();
    std::fs::write(l.declined(), "# declined\n").unwrap();
    assert_eq!(adoption(&l, &cfg), Adoption::Declined);
    assert!(!adoption_consented(&l, &cfg));
}

#[test]
fn a_session_gets_bin_where_consent_or_the_hook_already_puts_it() {
    let l = crate::testdir::layout("atpkg-session-bin-");
    let home_dir = crate::testdir::dir("atpkg-session-bin-home-");
    let home: &std::path::Path = &home_dir;
    let consenting = PackagesConfig::default();
    let off = PackagesConfig {
        auto_install: Some(false),
        ..PackagesConfig::default()
    };
    let lanes_off = PackagesConfig {
        enabled: Some(false),
        ..PackagesConfig::default()
    };
    let bin = Some(l.bin_dir());
    let armed = crate::manager_enabled();
    assert_eq!(
        session_bin_dir(&l, &consenting, Some(home)),
        bin.clone().filter(|_| armed),
        "consent with the lanes on (where the build is armed)"
    );
    assert_eq!(
        session_bin_dir(&l, &consenting, None),
        bin.clone().filter(|_| armed)
    );
    assert_eq!(
        session_bin_dir(&l, &off, Some(home)),
        None,
        "auto_install = false"
    );
    assert_eq!(
        session_bin_dir(&l, &lanes_off, Some(home)),
        None,
        "lanes off"
    );
    std::fs::create_dir_all(&l.prefix).unwrap();
    std::fs::write(l.declined(), "# declined\n").unwrap();
    assert_eq!(
        session_bin_dir(&l, &consenting, Some(home)),
        None,
        "declined"
    );
    let shell_d = home.join(".aterm").join("shell.d");
    std::fs::create_dir_all(&shell_d).unwrap();
    std::fs::write(shell_d.join("00-atpkg.zsh"), "# hook\n").unwrap();
    for cfg in [&consenting, &off, &lanes_off] {
        assert_eq!(session_bin_dir(&l, cfg, Some(home)), bin, "{cfg:?}");
    }
    assert_eq!(
        session_bin_dir(&l, &off, None),
        None,
        "no home, no hook to read"
    );
}
