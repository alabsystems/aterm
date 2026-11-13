// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

/// `agents_auto_prime` resolves as the window resolves it: only a boolean `false` turns it off; no key, a non-boolean and a file that does not parse are the default.
#[test]
fn agents_auto_prime_is_off_only_for_a_written_false() {
    use super::parse_agents_auto_prime as prime;
    assert!(!prime("agents_auto_prime = false\n"));
    assert!(!prime(
        "tab_status = true\nagents_auto_prime = false\n[packages]\n"
    ));
    assert!(prime("agents_auto_prime = true\n"));
    assert!(prime(""));
    assert!(
        prime("[packages]\nagents_auto_prime = false\n"),
        "a table's key"
    );
    assert!(prime("agents_auto_prime = \"no\"\n"));
    assert!(
        prime("agents_auto_prime = false\n[[[\n"),
        "a file that does not parse"
    );
}

#[test]
fn abrew_config_fails_closed_on_malformed_table() {
    assert!(!parse_abrew("[abrew]\nenabled = maybe\n").enabled());
    assert!(!parse_abrew("[abrew]\nenabled = false\n").enabled());
    assert!(parse_abrew("[abrew]\nenabled = true\n").enabled());
    assert!(parse_abrew("").enabled());
    for malformed in [
        "[ abrew ]\nenabled = maybe\n",
        "['abrew']\nenabled = maybe\n",
        "[\"abrew\"]\nenabled = maybe\n",
        "abrew.enabled = maybe\n",
        "[packages]\nbroken = [\n",
    ] {
        assert!(!parse_abrew(malformed).enabled());
    }
    assert!(parse_abrew("[packages]\nexclude = []\n").enabled());
    assert!(parse_abrew("[ abrew ]\n").enabled());
    assert!(!parse_abrew("['abrew']\nenabled = false\n").enabled());
}

use super::*;

/// `unix` (a rooted `/…` path) as an absolute path on this host: Windows roots it on a drive, since `/opt/…` alone is drive-relative there.
fn abs(unix: &str) -> String {
    if cfg!(windows) {
        format!("C:{unix}")
    } else {
        unix.to_string()
    }
}

#[test]
fn machine_table_defaults_act_and_opt_outs_are_explicit() {
    let none = parse_machine("font_px = 12.0\n[packages]\nexclude = [\"trust\"]\n");
    assert_eq!(none.universal_control(), UniversalControlPolicy::Off);
    let off = parse_machine("[machine]\nuniversal_control = \"leave\"\n");
    assert_eq!(off.universal_control(), UniversalControlPolicy::Leave);
    let explicit = parse_machine("[machine]\nuniversal_control = \"off\"\n");
    assert_eq!(explicit.universal_control(), UniversalControlPolicy::Off);
    assert_eq!(
        parse_machine("[machine]\nuniversal_control = \" OFF \"\n").universal_control(),
        UniversalControlPolicy::Leave,
        "an unknown spelling never acts"
    );
    let unreadable = parse_machine("[machine]\nuniversal_control = 1\n");
    assert_eq!(
        unreadable.universal_control(),
        UniversalControlPolicy::Leave,
        "a [machine] table that does not parse falls to the INERT reading, loudly: \
         these defaults ACT, and unreadable words are no licence to write the host"
    );
    assert!(none.rosetta(), "the default installs it");
    assert!(
        off.rosetta(),
        "the Universal Control opt-out is not this one"
    );
    assert!(!parse_machine("[machine]\nrosetta = false\n").rosetta());
    assert!(parse_machine("[machine]\nrosetta = true\n").rosetta());
    assert!(
        !parse_machine("[machine]\nrosetta = \"no\"\n").rosetta(),
        "a value that is not a boolean makes the table unreadable, and that reading is inert"
    );
    assert!(!unreadable.rosetta(), "{unreadable:?}");
    assert!(!MachineConfig::inert().rosetta());
    let both = "[packages]\nexclude = [\"ay\"]\n[machine]\nuniversal_control = \"leave\"\n";
    assert_eq!(parse_packages(both).exclude(), ["ay".to_string()]);
    assert_eq!(
        parse_machine(both).universal_control(),
        UniversalControlPolicy::Leave
    );
}

/// A typo in `[packages]` never re-arms the acting `[machine]` defaults: each table is parsed on its own, and an unreadable `[machine]` table falls to the inert reading.
#[test]
fn a_packages_typo_never_re_arms_the_machine_defaults() {
    let typo = "[packages]\nauto_install = \"yes\"\n[machine]\n\
                universal_control = \"leave\"\n";
    let m = parse_machine(typo);
    assert_eq!(
        m.universal_control(),
        UniversalControlPolicy::Leave,
        "a [packages] type error must not cancel an explicit universal_control opt-out"
    );
    let other = "[packages]\nexclude = [\"ay\"]\n[machine]\nuniversal_control = 3\n";
    assert_eq!(
        parse_packages(other).exclude(),
        ["ay".to_string()],
        "a [machine] type error must not discard the [packages] table"
    );
    let broken = parse_machine("[machine\nnot toml");
    assert_eq!(broken.universal_control(), UniversalControlPolicy::Leave);
    let (note, _) = not_toml_note("[packages]\nexclude = [\"ay\"]\n[window]\ntitle = \"x\n")
        .expect("an unclosed quote is not TOML");
    assert!(note.starts_with("aterm.toml is not valid TOML"), "{note}");
    assert!(!note.contains("[packages] in"), "{note}");
    assert_eq!(not_toml_note("[machine]\nuniversal_control = 1\n"), None);
}

/// A host's aterm.log keeps no value and no line of aterm.toml: the universal_control
/// value by its length, an error by its position ([`aterm_toml::Error::log_words`]), while a
/// verb's stderr still says them whole (DESIGN §3.6).
#[test]
fn a_hosts_log_keeps_no_aterm_toml_value_or_line() {
    // Each text is said once per process ([`crate::notice::say_once_with_log`]), so every
    // sink reads its own sentinel.
    let texts = |n: u8| {
        [
            format!("[machine]\nuniversal_control = \"QZP7E7J{n}a\"\n"),
            format!("[machine]\nrosetta = \"QZP7E7J{n}b\"\n"),
            format!("[packages]\nexclude = \"QZP7E7J{n}c\"\n"),
            format!("# x\nfont_family = \"QZP7E7J{n}d\n"),
        ]
    };
    let run = |[control, machine, packages, broken]: [String; 4]| {
        assert_eq!(
            parse_machine(&control).universal_control(),
            UniversalControlPolicy::Leave
        );
        assert!(parse_machine(&machine).unreadable);
        let _ = parse_packages_at(&packages, false, None);
        assert!(parse_machine(&broken).unreadable);
    };
    let host = crate::notice::heard::while_running(crate::notice::Sink::HostLog, || {
        run(texts(1));
    });
    assert!(host.stderr.is_empty(), "a host's notice reached stderr");
    for head in [
        "[machine] universal_control = 9B is not \"off\" or \"leave\"",
        "[machine] in aterm.toml: TOML error at line 2, column 11 ",
        "[packages] in aterm.toml: TOML error at line 2, column 11 ",
        "aterm.toml is not valid TOML (TOML error at line 2, column 25) ",
    ] {
        assert!(
            host.log.iter().any(|line| line.starts_with(head)),
            "the log names {head:?}: {} lines",
            host.log.len()
        );
    }
    assert!(
        !host.log.iter().any(|line| line.contains("QZP7E7J")),
        "a host's log keeps aterm.toml's text"
    );
    // A pass whose stderr the window copies into its aterm.log (the package lanes,
    // [`crate::notice::STDERR_IS_HOST_LOG_ENV`]) prints the same log forms there.
    let copied = crate::notice::heard::while_running(crate::notice::Sink::HostReadStderr, || {
        run(texts(4));
    });
    assert!(
        copied.log.is_empty(),
        "a copied stderr's notice is not logged"
    );
    for head in [
        "atpkg: [machine] universal_control = 9B is not \"off\" or \"leave\"",
        "atpkg: [machine] in aterm.toml: TOML error at line 2, column 11 ",
        "atpkg: [packages] in aterm.toml: TOML error at line 2, column 11 ",
        "atpkg: aterm.toml is not valid TOML (TOML error at line 2, column 25) ",
    ] {
        assert!(
            copied.stderr.lines().any(|line| line.starts_with(head)),
            "the copied stderr names {head:?}: {} lines",
            copied.stderr.lines().count()
        );
    }
    assert!(
        !copied.stderr.contains("QZP7E7J"),
        "a stderr the host's log reads keeps aterm.toml's text"
    );
    // CONTROL: a verb's stderr says every value and line whole.
    let verb = crate::notice::heard::while_running(crate::notice::Sink::Stderr, || {
        run(texts(2));
    });
    assert!(verb.log.is_empty(), "a verb's notice is not logged");
    for planted in ["QZP7E7J2a", "QZP7E7J2b", "QZP7E7J2c", "QZP7E7J2d"] {
        assert!(verb.stderr.contains(planted), "stderr says {planted}");
    }

    // The ignored prefix: Settings, doctor and a verb say the path, a host's log its length.
    let prefix = Path::new("/opt/QZP7E7J3");
    let logged = ignored_prefix_note_logged(prefix);
    assert!(!logged.contains("QZP7E7J"), "the log form keeps the path");
    assert_eq!(
        logged,
        ignored_prefix_note(prefix).replace("/opt/QZP7E7J3", "13B"),
        "the log form is the said form with the path by its length"
    );
    // Both forms fill one template, so a path that shares text with the notice's own words
    // is still the one thing the log form replaces.
    let shared = Path::new("packages");
    assert_eq!(
        ignored_prefix_note_logged(shared),
        ignored_prefix_note(shared).replacen("prefix = packages", "prefix = 8B", 1),
        "the log form fills the template's place for the path, and only it"
    );
}

#[test]
fn absent_table_yields_the_batteries_included_defaults() {
    let cfg = parse_packages("font_px = 12.0\n[matrix_rain]\nenabled = true\n");
    assert_eq!(cfg.account(), None);
    assert!(cfg.enabled(), "automatic updates default ON");
    assert!(
        cfg.auto_install(),
        "the one install consent defaults ON (batteries included, §9.1 — installing \
         aterm is wanting the toolset)"
    );
    assert!(cfg.exclude().is_empty());
    assert!(cfg.links.is_empty());
    assert!(cfg.config_notes().is_empty(), "nothing to rename");
    assert_eq!(cfg.enabled, None);
    assert_eq!(cfg.auto_update, None);
}

#[test]
fn full_table_parses_every_key() {
    let cfg = parse_packages_with(
        "[packages]\nenabled = true\nauto_install = false\n\
         account = \"alabsystems\"\nexclude = [\"trust\"]\n\
         [packages.links]\nay = \"~/ay\"\norc = \"alabsystems/orc\"\n",
        true,
    );
    assert_eq!(cfg.enabled, Some(true));
    assert!(cfg.enabled());
    assert!(!cfg.auto_install());
    assert_eq!(cfg.account(), Some("alabsystems"));
    assert_eq!(cfg.exclude(), ["trust".to_string()]);
    assert_eq!(cfg.links.get("ay").map(String::as_str), Some("~/ay"));
    assert_eq!(
        cfg.links.get("orc").map(String::as_str),
        Some("alabsystems/orc")
    );
}

/// `auto_update` is folded into `enabled`: its `false` still reads as off, whatever `enabled` says, and doctor names the rename; `true` changes nothing.
#[test]
fn a_retired_auto_update_false_still_switches_automatic_updates_off() {
    let old = parse_packages("[packages]\nauto_update = false\n");
    assert!(
        !old.enabled(),
        "an existing `auto_update = false` is honoured"
    );
    assert!(
        old.config_notes()
            .iter()
            .any(|n| n.contains("auto_update") && n.contains("enabled = false")),
        "{:?}",
        old.config_notes()
    );
    let both = parse_packages("[packages]\nenabled = true\nauto_update = false\n");
    assert!(
        !both.enabled(),
        "the old off switch stands until the file is rewritten (Settings drops it)"
    );
    assert!(parse_packages("[packages]\nauto_update = true\n").enabled());
    assert!(!parse_packages("[packages]\nenabled = false\n").enabled());
}

/// The rename is said only where a rename parses: with `enabled` already written, renaming `auto_update` would write a duplicate key, a file atpkg reads as automatic updates on.
#[test]
fn the_auto_update_note_never_asks_for_a_duplicate_key() {
    let both = parse_packages("[packages]\nenabled = true\nauto_update = false\n");
    let notes = both.config_notes();
    assert!(
        notes.iter().all(|n| !n.contains("rename")),
        "a rename would write `enabled` twice: {notes:?}"
    );
    assert!(
        notes
            .iter()
            .any(|n| n.contains("remove it and set `enabled = false`")),
        "{notes:?}"
    );
    let followed = parse_packages("[packages]\nenabled = false\n");
    assert!(!followed.unreadable && !followed.enabled());
    for text in [
        "[packages]\nenabled = false\nauto_update = false\n",
        "[packages]\nenabled = true\nauto_update = true\n",
    ] {
        let notes = parse_packages(text).config_notes();
        assert!(
            notes
                .iter()
                .any(|n| n.ends_with("`enabled` decides; remove it")),
            "{text}: {notes:?}"
        );
    }
    assert!(
        parse_packages("[packages]\nauto_update = false\n")
            .config_notes()
            .iter()
            .any(|n| n.contains("rename it to `enabled`"))
    );
}

/// A prefix this build ignores stops unattended installs: a shipped binary drops a `[packages] prefix` naming another store, and until the line goes the seed and the set completion install nothing.
#[test]
fn an_ignored_prefix_stops_unattended_installs_until_the_line_is_removed() {
    let home = Path::new("/Users//someone");
    let opt = abs("/opt/aterm/pkg");
    let shared = parse_packages_at(
        &format!("[packages]\nprefix = \"{opt}\"\n"),
        false,
        Some(home),
    );
    assert_eq!(shared.prefix, None, "dropped");
    assert_eq!(shared.ignored_prefix.as_deref(), Some(Path::new(&opt)));
    assert!(
        shared.auto_install(),
        "the consent itself is untouched (Settings shows it)"
    );
    assert!(!shared.installs_unattended(), "no second store is filled");
    assert!(
        ignored_prefix_note(Path::new("/opt/aterm/pkg")).contains("remove the line"),
        "the note names the fix"
    );
    assert!(
        !shared.config_notes().iter().any(|n| n.contains("prefix")),
        "the ignored-prefix note alone says it, not a second drop note: {:?}",
        shared.config_notes()
    );
    let default = crate::store::default_prefix(home);
    let same = parse_packages_at(
        &format!("[packages]\nprefix = {:?}\n", default.display().to_string()),
        false,
        Some(home),
    );
    assert_eq!(
        same.ignored_prefix, None,
        "the default named is the default used"
    );
    assert!(same.installs_unattended());
    let dev = parse_packages_at(
        &format!("[packages]\nprefix = \"{opt}\"\n"),
        true,
        Some(home),
    );
    assert_eq!(dev.ignored_prefix, None, "a development build honours it");
    assert!(dev.installs_unattended());
    let removed = parse_packages_at("[packages]\n", false, Some(home));
    assert!(
        removed.installs_unattended(),
        "removing the line is the whole fix"
    );
}

/// One install consent: `auto_install` wins when written, else the retired `seed_install`, else on; a table we could not read is off.
#[test]
fn one_install_consent_migrates_both_retired_spellings() {
    let consent = |toml: &str| parse_packages(toml).auto_install();
    assert!(consent(""));
    assert!(
        !consent("[packages]\nseed_install = false\n"),
        "`not on my disk` stands"
    );
    assert!(consent("[packages]\nseed_install = true\n"));
    assert!(
        consent("[packages]\nseed_install = false\nauto_install = true\n"),
        "the old explicit network-bootstrap consent still installs"
    );
    assert!(
        !consent("[packages]\nseed_install = true\nauto_install = false\n"),
        "auto_install decides when written"
    );
    assert!(!consent("[packages]\nauto_install = false\n"));
    let notes = parse_packages("[packages]\nseed_install = false\n").config_notes();
    assert!(
        notes
            .iter()
            .any(|n| n.contains("seed_install") && n.contains("auto_install = false")),
        "{notes:?}"
    );
    let notes = parse_packages("[packages]\nauto_install = false\n").config_notes();
    assert!(
        notes.iter().any(|n| n.contains("unattended")),
        "an explicit auto_install = false is told what it now means: {notes:?}"
    );
}

/// `channel` and `include` are retired: parsed as any value (a stale key never fails the table), never read, and named by doctor.
#[test]
fn retired_channel_and_include_are_ignored_and_named() {
    let cfg = parse_packages(
        "[packages]\nchannel = \"nightly\"\ninclude = [\"ay\"]\nexclude = [\"trust\"]\n",
    );
    assert!(
        !cfg.unreadable,
        "a retired key does not make the table unreadable"
    );
    assert_eq!(
        cfg.exclude(),
        ["trust".to_string()],
        "the rest of the table reads"
    );
    let notes = cfg.config_notes();
    assert!(
        notes.iter().any(|n| n.contains("channel is retired")),
        "{notes:?}"
    );
    assert!(
        notes.iter().any(|n| n.contains("include is retired")),
        "{notes:?}"
    );
    assert!(
        !parse_packages("[packages]\nchannel = 7\n").unreadable,
        "any value type is admitted for a retired key"
    );
}

/// A shipped binary drops `prefix`, `account` and every `owner/repo` link at load; a checkout link stays.
#[test]
fn a_shipped_binary_drops_the_development_settings() {
    let text = "[packages]\nprefix = \"/opt/pkg\"\naccount = \"fork-org\"\n\
                [packages.links]\nay = \"/src/ay\"\norc = \"fork-org/orc\"\n";
    let shipped = parse_packages_with(text, false);
    assert_eq!(shipped.prefix, None);
    assert_eq!(shipped.account(), None);
    assert_eq!(
        crate::resolve_account(shipped.account()).owner,
        aterm_update_core::ATPKG_INDEX_OWNER
    );
    assert!(
        repo_overrides(&shipped).is_empty(),
        "no fetch override ships"
    );
    assert_eq!(
        shipped.links.get("ay").map(String::as_str),
        Some("/src/ay"),
        "a checkout dev link is kept"
    );
    let dev = parse_packages_with(text, true);
    assert_eq!(dev.prefix.as_deref(), Some("/opt/pkg"));
    assert_eq!(crate::resolve_account(dev.account()).owner, "fork-org");
    assert_eq!(repo_overrides(&dev).len(), 1);
    let bad = parse_packages_with("[packages]\naccount = \"evil.com/x\"\n", true);
    assert_eq!(
        crate::resolve_account(bad.account()).owner,
        aterm_update_core::ATPKG_INDEX_OWNER
    );
}

#[test]
fn malformed_config_reads_as_unreadable_fail_closed() {
    let cfg = parse_packages("[packages\nnot toml");
    assert!(
        !cfg.auto_install(),
        "an unreadable table installs nothing new"
    );
    assert!(cfg.enabled(), "what is installed keeps updating");
    assert!(cfg.links.is_empty());
    assert!(cfg.unreadable, "the table was not read");
    let cfg = parse_packages("packages = 3\n");
    assert!(cfg.exclude().is_empty());
    assert!(cfg.unreadable);
}

/// A `[packages]` table we could not read must not widen: the install consent defaults true and `exclude` empty, so its defaults would switch on the multi-GB paths those keys block.
#[test]
fn an_unreadable_packages_table_never_widens_the_download_gates() {
    let typed = parse_packages("[packages]\nauto_install = false\nenabled = \"yes\"\n");
    assert!(typed.unreadable);
    assert!(
        !typed.auto_install(),
        "a table we could not read is not consent to lay the toolset down: the \
         resolver's TRUE default is the expensive one, and on a lean install it is a \
         multi-GB download the owner declined in writing"
    );
    let elsewhere = parse_packages("[packages]\nauto_install = false\n[machine\nbroken\n");
    assert!(elsewhere.unreadable);
    assert!(!elsewhere.auto_install());
    let excluded = parse_packages("[packages]\nexclude = [\"trust\"]\nenabled = 7\n");
    assert!(excluded.unreadable);
    assert!(
        excluded.exclude().is_empty(),
        "the exclusions are genuinely gone — which is exactly why the pass that \
         would act on their absence must not run"
    );
    let fine = parse_packages("[packages]\nexclude = []\n");
    assert!(!fine.unreadable);
    assert!(fine.auto_install());
    assert!(!PackagesConfig::default().unreadable);
    assert!(PackagesConfig::default().auto_install());
    assert!(PackagesConfig::unreadable_table().unreadable);
    assert!(!PackagesConfig::unreadable_table().auto_install());
}

#[cfg(unix)]
#[test]
fn config_fifo_returns_defaults_and_symlinked_config_remains_supported() {
    use std::os::unix::ffi::OsStrExt as _;

    let root = crate::testdir::dir("atpkg-config-admission-");
    let fifo = root.join("fifo.toml");
    let fifo_c = std::ffi::CString::new(fifo.as_os_str().as_bytes()).unwrap();
    // SAFETY: `fifo_c` is a live NUL-terminated path in our private fixture.
    assert_eq!(unsafe { libc::mkfifo(fifo_c.as_ptr(), 0o600) }, 0);
    assert!(
        load_from_path(&fifo).exclude().is_empty(),
        "a writerless config FIFO must return the finite default immediately"
    );

    let target = root.join("target.toml");
    let logical = root.join("aterm.toml");
    std::fs::write(&target, "[packages]\nexclude = [\"ay\"]\n").unwrap();
    std::os::unix::fs::symlink(&target, &logical).unwrap();
    assert_eq!(load_from_path(&logical).exclude(), ["ay".to_string()]);
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn oversized_sparse_config_returns_bounded_defaults() {
    let root = crate::testdir::dir("atpkg-config-oversized-");
    let path = root.join("aterm.toml");
    let file = std::fs::File::create(&path).unwrap();
    file.set_len((MAX_PACKAGES_CONFIG_BYTES + 1) as u64)
        .unwrap();
    assert!(load_from_path(&path).exclude().is_empty());
    let _ = std::fs::remove_dir_all(&root);
}

/// Store discovery must honour the same admitted config as the package reader, including
/// a prefix after a large valid comment. A file beyond their shared budget remains refused.
#[cfg(any(debug_assertions, feature = "dev-seams"))]
#[test]
fn store_discovery_reads_a_prefix_through_the_full_config_budget() {
    let root = crate::testdir::dir("atpkg-discovery-config-budget-");
    let home = root.join("home");
    let xdg = root.join("config");
    let path = xdg.join("aterm/aterm.toml");
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    let settings = "[packages]\nprefix = '~/.configured-store'\n";
    let expected = home.join(".configured-store");
    for bytes in [settings.len(), super::MAX_PACKAGES_CONFIG_BYTES] {
        let body = if bytes == settings.len() {
            settings.to_string()
        } else {
            format!("#{}\n{settings}", "x".repeat(bytes - settings.len() - 2))
        };
        assert_eq!(body.len(), bytes);
        std::fs::write(&path, &body).unwrap();
        let admitted = super::load_live(&path).expect("admitted package config");
        assert_eq!(admitted.prefix.as_deref(), Some("~/.configured-store"));
        assert_eq!(
            aterm_toolchain::discovery::atpkg_prefix(&home, Some(&xdg)),
            expected,
            "the actual package reader admits this {bytes}-byte config"
        );
    }
    let file = std::fs::OpenOptions::new().write(true).open(&path).unwrap();
    file.set_len((super::MAX_PACKAGES_CONFIG_BYTES + 1) as u64)
        .unwrap();
    assert!(super::load_live(&path).is_none());
    assert_eq!(
        aterm_toolchain::discovery::atpkg_prefix(&home, Some(&xdg)),
        aterm_toolchain::discovery::default_atpkg_prefix(&home)
    );
}

/// `[reroute].announce` defaults on, `false` silences, and a table that does not parse announces.
#[test]
fn reroute_announce_defaults_on_and_a_typo_announces() {
    assert!(parse_reroute("").announce());
    assert!(parse_reroute("[reroute]\nannounce = true\n").announce());
    assert!(!parse_reroute("[reroute]\nannounce = false\n").announce());
    assert!(parse_reroute("[reroute]\nannounce = \"no\"\n").announce());
    assert!(
        !parse_reroute("[packages]\nenabled = 3\n[reroute]\nannounce = false\n").announce(),
        "a typo in another table does not repeal it"
    );
}

/// `[reroute].strict` defaults off (warn and run); only an explicit `true` refuses, and a value that does not parse is the default.
#[test]
fn reroute_strict_defaults_off_and_only_true_turns_it_on() {
    assert!(!parse_reroute("").strict());
    assert!(!parse_reroute("[reroute]\nannounce = false\n").strict());
    assert!(!parse_reroute("[reroute]\nstrict = false\n").strict());
    assert!(parse_reroute("[reroute]\nstrict = true\n").strict());
    assert!(!parse_reroute("[reroute]\nstrict = \"yes\"\n").strict());
    let both = parse_reroute("[reroute]\nannounce = false\nstrict = true\n");
    assert!(
        both.strict() && !both.announce(),
        "the two keys are independent"
    );
}

#[test]
fn classify_link_covers_the_three_shapes() {
    let home = Path::new("/home/u");
    assert_eq!(
        classify_link(&abs("/src/ay"), Some(home)),
        LinkTarget::Checkout(PathBuf::from(abs("/src/ay")))
    );
    assert_eq!(
        classify_link("~/ay", Some(home)),
        LinkTarget::Checkout(PathBuf::from("/home/u").join("ay"))
    );
    assert_eq!(
        classify_link("~", Some(home)),
        LinkTarget::Checkout(PathBuf::from("/home/u"))
    );
    assert_eq!(classify_link("~/ay", None), LinkTarget::Invalid);
    assert_eq!(classify_link("~bob/ay", Some(home)), LinkTarget::Invalid);
    assert_eq!(
        classify_link("alabsystems/orc", Some(home)),
        LinkTarget::Repo("alabsystems/orc".into())
    );
    assert_eq!(classify_link("src/ay/x", Some(home)), LinkTarget::Invalid);
    assert_eq!(
        classify_link("evil.com?x/y", Some(home)),
        LinkTarget::Invalid
    );
    assert_eq!(classify_link("a b/repo", Some(home)), LinkTarget::Invalid);
    assert_eq!(classify_link("", Some(home)), LinkTarget::Invalid);
}

#[test]
fn repo_overrides_extracts_only_validated_slug_entries() {
    let cfg = parse_packages_with(
        "[packages.links]\nay = \"/src/ay\"\norc = \"alabsystems/orc\"\n\
         bad = \"evil host/x\"\n",
        true,
    );
    let map = repo_overrides(&cfg);
    assert_eq!(map.len(), 1, "only the validated owner/repo entry survives");
    assert_eq!(map.get("orc").map(String::as_str), Some("alabsystems/orc"));
}

/// `[packages].prefix` shapes; whether a prefix is safe is `store::vet_prefix`'s call.
#[test]
fn prefix_path_resolves_absolute_and_tilde_and_refuses_relative() {
    let home = Path::new("/home/someone");
    let of = |v: Option<&str>| {
        PackagesConfig {
            prefix: v.map(str::to_string),
            ..PackagesConfig::default()
        }
        .prefix_path(Some(home))
    };

    assert_eq!(of(None), None, "absent ⇒ the default prefix");
    assert_eq!(of(Some("   ")), None, "blank ⇒ the default prefix");
    assert_eq!(
        of(Some(&abs("/opt/aterm/pkg"))),
        Some(PathBuf::from(abs("/opt/aterm/pkg"))),
        "an absolute system prefix survives to vet_prefix"
    );
    assert_eq!(
        of(Some("~/pkgs")),
        Some(home.join("pkgs")),
        "`~/…` expands against home"
    );
    assert_eq!(of(Some("~")), Some(home.to_path_buf()), "bare `~` is home");
    assert_eq!(of(Some("~root/pkg")), None, "`~user` is not supported");
    assert_eq!(
        of(Some("relative/pkg")),
        None,
        "a CWD-dependent prefix is not a prefix"
    );
}
