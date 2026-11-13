// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

fn argv(args: &[&str]) -> Vec<String> {
    args.iter().map(|s| (*s).to_owned()).collect()
}

#[test]
fn badge_follows_brew_env_rules() {
    assert_eq!(badge_from(false, None).as_deref(), Some("🍺"));
    assert_eq!(
        badge_from(false, Some(String::new())).as_deref(),
        Some("🍺")
    );
    assert_eq!(
        badge_from(false, Some("✅".to_string())).as_deref(),
        Some("✅")
    );
    assert_eq!(badge_from(true, None), None);
    assert_eq!(badge_from(true, Some("✅".to_string())), None);
}

#[test]
fn brew_help_texts_cover_commands_and_aliases() {
    assert!(help_texts::TOP.starts_with("Example usage:\n  brew search"));
    let install = brew_help_text("install");
    assert!(
        install.starts_with("Usage: brew install [options] formula|cask"),
        "{install:.60}"
    );
    assert!(install.ends_with("Show this message.\n"));
    // Aliases resolve to the canonical command's text.
    assert_eq!(brew_help_text("ls"), brew_help_text("list"));
    assert_eq!(brew_help_text("instal"), install);
    // Unknown topics fall back to the top-level message.
    assert_eq!(brew_help_text("frobnicate"), help_texts::TOP);
}

#[test]
fn command_names_merge_aliases_sorted() {
    let plain = command_names(false);
    assert_eq!(
        plain.len(),
        BREW_BUILTIN_COMMANDS.len() + BREW_DEVELOPER_COMMANDS.len()
    );
    assert!(!plain.contains(&"ls"));
    assert!(plain.windows(2).all(|w| w[0] < w[1]), "plain is sorted");
    let merged = command_names(true);
    assert_eq!(merged.len(), plain.len() + BREW_COMMAND_ALIASES.len());
    assert!(merged.windows(2).all(|w| w[0] < w[1]), "merged is sorted");
    for alias in BREW_COMMAND_ALIASES {
        assert!(merged.contains(alias), "{alias} listed");
        assert!(!plain.contains(alias), "{alias} not duplicated");
    }
}

#[test]
fn unsupported_arguments_are_refused_before_native_effects() {
    for (cmd, args) in [
        ("install", vec!["--HEAD", "wget"]),
        ("install", vec!["-s", "wget"]),
        (
            "install",
            vec!["--cask", "--ignore-dependencies", "firefox"],
        ),
        ("info", vec!["--analytics"]),
        ("info", vec!["--analytics", "--json", "wget"]),
        ("info", vec!["--analytics", "--installed", "wget"]),
        ("info", vec!["--analytics", "--days", "30"]),
        ("info", vec!["--analytics", "--category", "install"]),
        (
            "info",
            vec!["--analytics", "--days=30", "--category=install"],
        ),
        ("link", vec!["--HEAD", "wget"]),
        ("services", vec!["start", "wget"]),
        ("--cache", vec!["-s", "wget"]),
        ("--cache", vec!["--HEAD", "wget"]),
        ("desc", vec!["-s", "/(?=http)client/"]),
        ("missing", vec!["--hide=../outside"]),
        // `list --versions --json <extra>` rows moved to
        // native_admission_messages_are_precise (r332):
        // brew answers them with Failed usage errors, not
        // native refusals.
        ("search", vec!["/(?=http)client/"]),
        ("search", vec![r"/(\w)\1/"]),
        ("search", vec![r"/\p{L}+/"]),
        ("pin", vec!["../outside"]),
        // install other/tap/wget: admitted since r327 and
        // refused inside cmd_install (brew factory-errors /
        // delegates; never a parse-level refusal).
        ("uninstall", vec!["homebrew/cask/wget"]),
        ("uninstall", vec!["homebrew/core/firefox"]),
        ("uninstall", vec!["--formula", "homebrew/cask/wget"]),
        ("uninstall", vec!["--cask", "homebrew/core/firefox"]),
        ("pin", vec!["homebrew/cask/wget"]),
        ("link", vec!["homebrew/cask/wget"]),
        ("reinstall", vec!["homebrew/cask/wget"]),
        ("install", vec!["homebrew/core/wget"]),
        ("--prefix", vec!["homebrew/cask/wget"]),
        ("uninstall", vec!["--formula", "wget", "homebrew/cask/wget"]),
    ] {
        assert!(
            matches!(
                native_args(cmd, &argv(&args)),
                Err(InstallError::Unsupported(_))
            ),
            "{cmd} {args:?}"
        );
    }
    for (cmd, args) in [
        (
            "install",
            vec!["--formula", "--ignore-dependencies", "wget"],
        ),
        ("install", vec!["wget", "curl"]),
        ("uninstall", vec!["wget", "curl"]),
        ("upgrade", vec!["wget", "curl"]),
        ("upgrade", vec!["--cask", "firefox", "vlc"]),
        ("reinstall", vec!["--formula", "--force", "wget", "curl"]),
        ("reinstall", vec!["--cask", "--dry-run", "firefox", "vlc"]),
        ("install", vec!["--cask", "--dry-run", "firefox"]),
        ("cleanup", vec!["--dry-run", "wget"]),
        ("cleanup", vec!["wget", "tree"]),
        ("deps", vec!["--tree", "wget"]),
        ("deps", vec!["--installed"]),
        ("deps", vec!["--installed", "--tree", "wget"]),
        ("deps", vec!["--installed", "--direct", "wget"]),
        ("deps", vec!["--installed", "--tree"]),
        ("deps", vec!["--installed", "--direct"]),
        ("deps", vec!["--installed", "--tree", "--direct"]),
        ("deps", vec!["--tree", "--direct", "wget"]),
        ("deps", vec!["--1", "wget"]),
        ("deps", vec!["-1", "wget"]),
        ("deps", vec!["--declared", "wget"]),
        ("deps", vec!["--installed", "--1"]),
        ("deps", vec!["--installed", "-1", "wget"]),
        ("deps", vec!["--for-each", "wget", "curl"]),
        ("deps", vec!["--for-each", "--direct", "wget"]),
        ("deps", vec!["--for-each", "--installed"]),
        ("deps", vec!["--union", "wget", "curl"]),
        ("deps", vec!["-n", "wget"]),
        ("deps", vec!["--topological", "--direct", "wget"]),
        ("deps", vec!["--union", "-n", "wget", "curl"]),
        ("completions", vec![]),
        ("completions", vec!["state"]),
        ("completions", vec!["link"]),
        ("completions", vec!["unlink"]),
        ("--prefix", vec!["wget"]),
        ("--prefix", vec!["homebrew/core/wget"]),
        ("--cache", vec![]),
        ("--cache", vec!["wget"]),
        ("--cache", vec!["wget", "curl"]),
        ("--cache", vec!["--cask", "firefox"]),
        ("--cache", vec!["--formulae", "homebrew/core/wget"]),
        ("--repository", vec![]),
        ("--repository", vec!["homebrew/core"]),
        ("--repo", vec!["me/tools"]),
        ("install", vec!["--formula", "homebrew/core/wget"]),
        ("uninstall", vec!["--formula", "homebrew/core/wget"]),
        ("uninstall", vec!["--cask", "homebrew/cask/firefox"]),
        ("install", vec!["--cask", "homebrew/cask/firefox"]),
        ("fetch", vec!["--formula", "--deps", "homebrew/core/wget"]),
        ("fetch", vec!["--cask", "homebrew/cask/firefox"]),
        ("--cellar", vec!["wget"]),
        ("--cellar", vec!["homebrew/core/wget"]),
        ("pin", vec!["wget", "curl"]),
        ("unpin", vec!["wget", "curl"]),
        ("uninstall", vec!["--force", "wget"]),
        ("uninstall", vec!["-f", "wget"]),
        ("link", vec!["--dry-run", "wget"]),
        ("link", vec!["-n", "wget"]),
        ("unlink", vec!["--dry-run", "wget"]),
        ("list", vec!["wget", "curl"]),
        ("list", vec!["--versions", "wget"]),
        ("list", vec!["--versions", "wget", "curl"]),
        ("list", vec!["--multiple"]),
        ("list", vec!["--multiple", "wget", "curl"]),
        ("list", vec!["--versions", "--multiple"]),
        ("list", vec!["--versions", "--json"]),
        ("list", vec!["--versions", "--json", "--formula"]),
        ("list", vec!["--versions", "--json", "--casks"]),
        ("list", vec!["--full-name"]),
        ("outdated", vec!["wget"]),
        ("outdated", vec!["--json=v2", "wget", "tree"]),
        ("list", vec!["-l"]),
        ("list", vec!["--long", "wget"]),
        ("missing", vec!["wget"]),
        ("missing", vec!["--hide=openssl@3,icu4c"]),
        ("cat", vec!["--cask", "firefox"]),
        ("cat", vec!["--cask", "homebrew/cask/firefox"]),
        ("--caskroom", vec!["firefox"]),
        ("--caskroom", vec!["homebrew/cask/firefox"]),
        ("desc", vec!["--cask", "firefox"]),
        ("desc", vec!["--formula", "wget"]),
        ("desc", vec!["--search", "http", "client"]),
        ("desc", vec!["-s", "/^lib.*@\\d$/"]),
        ("desc", vec!["--name", "wget"]),
        ("desc", vec!["-n", "/wget/"]),
        ("desc", vec!["--description", "retriever"]),
        ("desc", vec!["-d", "--casks", "browser"]),
        ("leaves", vec!["--installed-as-dependency"]),
        ("options", vec![]),
        ("options", vec!["--compact", "wget"]),
        ("options", vec!["--installed"]),
        ("options", vec!["--installed", "wget"]),
        ("outdated", vec!["--json=v2"]),
        ("outdated", vec!["--json", "--formula"]),
        ("info", vec!["--json=v2", "wget"]),
        ("info", vec!["--analytics", "wget"]),
        ("info", vec!["--analytics", "--days=30", "wget"]),
        ("info", vec!["--analytics", "wget", "--days="]),
        ("info", vec!["--analytics", "wget", "--category="]),
        (
            "info",
            vec!["--analytics", "--days=30", "--days=90", "wget"],
        ),
        (
            "info",
            vec![
                "--analytics",
                "--category=install",
                "--category",
                "build-error",
                "wget",
            ],
        ),
        (
            "info",
            vec!["--analytics", "--days", "90", "--cask", "firefox"],
        ),
        ("info", vec!["--analytics", "--category", "install", "wget"]),
        ("info", vec!["--analytics", "--days=365", "wget"]),
        ("info", vec!["--analytics", "wget", "--days", "30"]),
        (
            "info",
            vec!["--analytics", "--category=install-on-request", "wget"],
        ),
        (
            "info",
            vec!["--analytics", "wget", "--category", "build-error"],
        ),
        (
            "info",
            vec![
                "--analytics",
                "--category=install",
                "--days",
                "90",
                "--formula",
                "homebrew/core/wget",
            ],
        ),
        ("info", vec!["--installed", "--json"]),
        ("doctor", vec!["--quiet"]),
        ("shellenv", vec!["fish"]),
        // Brew's shell fast paths: arguments are data or
        // ignored, help flags included.
        ("shellenv", vec!["--help"]),
        ("shellenv", vec!["--bogus", "fish"]),
        ("formulae", vec!["--help"]),
        ("casks", vec!["--bogus"]),
        ("--taps", vec!["anything"]),
        ("--repository", vec!["--help"]),
        ("--repository", vec!["notatap"]),
        ("--repo", vec!["a/b/c"]),
        ("tap", vec!["--list-official"]),
        ("cleanup", vec!["--prune=all"]),
        ("cleanup", vec!["--prune=30", "-n"]),
        ("cleanup", vec!["-s"]),
        ("list", vec!["--formulae"]),
        ("list", vec!["--casks", "-1"]),
        ("search", vec!["--casks", "firefox"]),
        ("search", vec!["--formulae", "/^wget$/"]),
        ("install", vec!["--formulae", "wget"]),
        ("install", vec!["--casks", "firefox"]),
        ("uninstall", vec!["--casks", "firefox"]),
        ("outdated", vec!["--formulae"]),
        ("upgrade", vec!["--casks", "firefox"]),
        ("search", vec!["/wget.+/"]),
        ("search", vec!["--desc", "/http.*/"]),
        ("search", vec!["/^lib.*@\\d$/"]),
        ("search", vec!["/http", "client/"]),
        ("-S", vec!["/(?i)WGET/"]),
    ] {
        assert!(native_args(cmd, &argv(&args)).is_ok(), "{cmd} {args:?}");
    }

    let max_days = 213_503_982_334_601u64;
    let max_arg = format!("--prune={max_days}");
    assert_eq!(
        cleanup_max_age(&argv(&[&max_arg])),
        Ok(Some(Some(std::time::Duration::from_secs(
            18_446_744_073_709_526_400
        ))))
    );
    assert!(native_args("cleanup", &argv(&[&max_arg])).is_ok());
    for args in [
        vec![],
        vec!["--prune=0"],
        vec!["--prune=30"],
        vec!["--prune=all"],
        vec!["--scrub"],
        vec!["-s", "--prune=30"],
    ] {
        assert!(native_args("cleanup", &argv(&args)).is_ok(), "{args:?}");
    }
    assert_eq!(cleanup_max_age(&[]), Ok(None));
    assert_eq!(
        cleanup_max_age(&argv(&["--prune=0"])),
        Ok(Some(Some(std::time::Duration::ZERO)))
    );
    assert_eq!(
        cleanup_max_age(&argv(&["--prune=30"])),
        Ok(Some(Some(std::time::Duration::from_secs(2_592_000))))
    );
    for args in [
        vec!["--prune=all"],
        vec!["--scrub"],
        vec!["-s", "--prune=30"],
    ] {
        assert_eq!(cleanup_max_age(&argv(&args)), Ok(Some(None)), "{args:?}");
    }
    assert_eq!(
        cleanup_max_age(&argv(&["--prune=0", "--prune=30"])),
        Ok(Some(Some(std::time::Duration::ZERO))),
        "the first valid prune value keeps its existing precedence"
    );
    for bad in [
        "--prune=213503982334602",
        "--prune=18446744073709551615",
        "--prune=18446744073709551616",
    ] {
        for args in [vec![bad], vec!["-s", bad], vec!["--prune=all", bad]] {
            let args = argv(&args);
            assert!(cleanup_max_age(&args).is_err(), "{args:?}");
            assert!(
                matches!(native_args("cleanup", &args), Err(InstallError::Failed(_))),
                "{args:?}"
            );
        }
    }
    for bad in ["--prune=", "--prune=-1", "--prune=+1", "--prune=soon"] {
        assert!(cleanup_max_age(&argv(&[bad])).is_err(), "{bad}");
    }

    let later = InstallError::Unsupported("later recipe needs brew".to_string());
    assert_eq!(after_native_progress(later.clone(), false), later);
    let refused = after_native_progress(later, true);
    assert!(matches!(
        refused,
        InstallError::Failed(reason) if reason.contains("later recipe needs brew")
    ));
    for completed in [false, true] {
        let native = InstallError::Failed("native refusal".to_string());
        assert_eq!(after_native_progress(native.clone(), completed), native);
    }

    let fallback = || InstallError::Unsupported("later recipe needs brew".to_string());
    let initial: Result<(), _> = Err(fallback());
    assert!(!native_attempt_may_have_progress(&initial));
    assert_eq!(
        after_native_progress(fallback(), native_attempt_may_have_progress(&initial)),
        fallback()
    );
    for earlier in [
        Ok(()),
        Err(InstallError::Failed("retained dependency work".to_string())),
    ] {
        let possible_progress = native_attempt_may_have_progress(&earlier);
        assert!(possible_progress);
        assert!(matches!(
            after_native_progress(fallback(), possible_progress),
            InstallError::Failed(_)
        ));
        assert_eq!(
            after_native_progress(
                InstallError::Failed("ordinary failure".to_string()),
                possible_progress
            ),
            InstallError::Failed("ordinary failure".to_string())
        );
    }

    let mut possible_progress = false;
    let initial: Result<(), _> = after_native_attempt(&mut possible_progress, Err(fallback()));
    assert_eq!(initial, Err(fallback()));
    assert!(!possible_progress);
    for first in [
        Ok(()),
        Err(InstallError::Failed("retained dependency work".to_string())),
    ] {
        let mut possible_progress = false;
        let expected = first.clone();
        assert_eq!(
            after_native_attempt(&mut possible_progress, first),
            expected
        );
        assert!(possible_progress);
        let later: Result<(), _> = after_native_attempt(&mut possible_progress, Err(fallback()));
        assert!(matches!(later, Err(InstallError::Failed(_))));
        assert!(possible_progress);
    }
}

#[test]
fn brewfile_admission_and_check_preserve_missing_and_unsupported_outcomes() {
    let temp = aterm_tempfile::tempdir().unwrap();
    let prefix = Prefix::at(&temp.path().join("uncreated-prefix"));
    let file = temp.path().join("Brewfile");
    let path = file.to_str().unwrap();
    std::fs::write(&file, "brew \"wget\"\ncask \"firefox\"\ntap \"me/tools\"\n").unwrap();
    assert!(
        matches!(
            bundle_args(&argv(&["install", "--file", path])),
            Ok(Some(entries)) if entries.len() == 3
        ),
        "multi-entry bundle install must admit"
    );
    assert_eq!(
        dispatch(&prefix, &argv(&["bundle", "check", "--file", path])),
        ExitCode::from(1)
    );
    assert!(
        !prefix.root.exists(),
        "checking must not create or lock a prefix"
    );
    std::fs::write(&file, "brew \"wget\", args: [\"HEAD\"]\n").unwrap();
    assert!(matches!(
        bundle_args(&argv(&["install", "--file", path])),
        Err(InstallError::Unsupported(_))
    ));
    assert!(!prefix.root.exists());
    std::fs::write(&file, "brew \"other/tap/wget\"\n").unwrap();
    assert!(matches!(
        bundle_args(&argv(&["install", "--file", path])),
        Err(InstallError::Unsupported(_))
    ));
    std::fs::write(&file, "# empty Brewfile\n").unwrap();
    assert_eq!(
        dispatch(&prefix, &argv(&["bundle", "check", "--file", path])),
        ExitCode::SUCCESS
    );
    assert!(!prefix.root.exists());
    for args in [
        argv(&["bundle", "--file", path]),
        argv(&["bundle", "install", "--file", path]),
    ] {
        assert_eq!(dispatch(&prefix, &args), ExitCode::SUCCESS, "{args:?}");
        assert!(
            !prefix.homebrew_bundle().exists(),
            "native Brewfiles never lay Homebrew"
        );
    }
}

#[test]
fn deprecated_envs_escalate_only_for_developers() {
    let env = |pairs: &'static [(&'static str, &'static str)]| {
        move |key: &str| {
            pairs
                .iter()
                .find(|(k, _)| *k == key)
                .map(|(_, v)| (*v).to_string())
        }
    };
    // Absent or blank: nothing (and nothing printed).
    assert!(deprecated_env_warnings(WRAPPER_DEPRECATED_ENVS, env(&[])).is_none());
    assert!(
        deprecated_env_warnings(
            WRAPPER_DEPRECATED_ENVS,
            env(&[("HOMEBREW_FORCE_BREW_WRAPPER", "")])
        )
        .is_none()
    );
    // Developer mode turns the first present warning, in
    // read order, into the error.
    assert_eq!(
        deprecated_env_warnings(
            WRAPPER_DEPRECATED_ENVS,
            env(&[
                ("HOMEBREW_DEVELOPER", "1"),
                ("HOMEBREW_NO_FORCE_BREW_WRAPPER", "0")
            ])
        )
        .as_deref(),
        Some(
            "Error: Calling HOMEBREW_NO_FORCE_BREW_WRAPPER is deprecated! \
             Use an environment without $HOMEBREW_NO_FORCE_BREW_WRAPPER instead."
        )
    );
    assert_eq!(
        deprecated_env_warnings(
            BUNDLE_DEPRECATED_ENVS,
            env(&[
                ("HOMEBREW_DEVELOPER", "1"),
                ("HOMEBREW_BUNDLE_FORCE_INSTALL_CLEANUP", "1"),
                ("HOMEBREW_BUNDLE_INSTALL_CLEANUP", "1"),
            ])
        )
        .as_deref(),
        Some(
            "Error: Calling HOMEBREW_BUNDLE_INSTALL_CLEANUP is deprecated! \
             Use `brew bundle cleanup` after `brew bundle install` instead."
        ),
        "install-cleanup is read before its force sibling"
    );
}

#[test]
fn internal_api_env_error_scopes_like_brew() {
    let set = |_: &str| Some("1".to_string());
    let err = "Error: Calling HOMEBREW_USE_INTERNAL_API is disabled! \
               Use the default behaviour instead.";
    // Ruby commands fetch API files before parsing.
    assert_eq!(
        internal_api_env_error("info", &argv(&["jq"]), set).as_deref(),
        Some(err)
    );
    assert_eq!(
        internal_api_env_error("deps", &argv(&["--quiet", "--verbose"]), set).as_deref(),
        Some(err),
        "the fetch precedes parser conflict errors"
    );
    // Falsy-but-present values still error (boolean :set);
    // blank stays quiet.
    assert!(internal_api_env_error("info", &argv(&[]), |_| Some("0".to_string())).is_some());
    assert!(internal_api_env_error("info", &argv(&[]), |_| Some(String::new())).is_none());
    assert!(internal_api_env_error("info", &argv(&[]), |_| None).is_none());
    // Shell paths and unknown commands never reach it.
    assert!(internal_api_env_error("--prefix", &argv(&["jq"]), set).is_none());
    assert!(internal_api_env_error("--version", &argv(&[]), set).is_none());
    assert!(internal_api_env_error("which-formula", &argv(&["jq"]), set).is_none());
    assert!(internal_api_env_error("no-such-cmd", &argv(&[]), set).is_none());
    assert!(internal_api_env_error("list", &argv(&[]), set).is_none());
    assert!(internal_api_env_error("list", &argv(&["--versions"]), set).is_some());
    assert!(internal_api_env_error("tap", &argv(&[]), set).is_none());
    assert!(internal_api_env_error("--cache", &argv(&[]), set).is_none());
    assert!(internal_api_env_error("--cache", &argv(&["jq"]), set).is_some());
}

#[test]
fn unique_option_abbreviations_expand() {
    let (_, rest) = native_args("info", &argv(&["--j=v1", "wget"])).unwrap();
    assert_eq!(rest, argv(&["--json=v1", "wget"]));
    let (_, rest) = native_args("list", &argv(&["--pin"])).unwrap();
    assert_eq!(rest, argv(&["--pinned"]));
    // Ambiguous prefixes stay typed (brew error) and a
    // value after a value-flag is never rewritten.
    assert!(matches!(
        native_args("list", &argv(&["--p"])),
        Err(InstallError::Failed(_))
    ));
    let (_, rest) = native_args("deps", &argv(&["--os", "linux", "--tre", "wget"])).unwrap();
    assert_eq!(rest, argv(&["--os", "linux", "--tree", "wget"]));
    // Alias pairs are one switch: --form expands.
    let (_, rest) = native_args("info", &argv(&["--form", "wget"])).unwrap();
    assert_eq!(rest, argv(&["--formula", "wget"]));
    let failed = |cmd: &str, args: &[&str]| match native_args(cmd, &argv(args)) {
        Err(InstallError::Failed(m)) => m,
        other => panic!("expected usage failure for {cmd} {args:?}, got {other:?}"),
    };
    // Ruby optparse + did_you_mean suffixes, oracle-verified.
    assert_eq!(
        failed("install", &["--for", "wget"]),
        "Error: ambiguous option: --for\nDid you mean?  force"
    );
    assert_eq!(
        failed("info", &["--jsonn", "wget"]),
        "Error: invalid option: --jsonn\nDid you mean?  json"
    );
    assert_eq!(
        failed("info", &["--caskk", "wget"]),
        "Error: invalid option: --caskk\nDid you mean?  cask\n               casks"
    );
    assert_eq!(
        failed("info", &["--bogus-zz", "wget"]),
        "Error: invalid option: --bogus-zz"
    );
    // Subcommand-parent option lists come from the recorded
    // parser preambles.
    assert!(native_args("services", &argv(&["list", "--jso"])).is_ok());
    // bundle check then goes on to read a Brewfile; the
    // abbreviation must at least not be a native refusal.
    assert!(!matches!(
        native_args("bundle", &argv(&["check", "--verb"])),
        Err(InstallError::Unsupported(_))
    ));
}

#[test]
fn deps_os_arch_simulation_values_admit() {
    let failed = |cmd: &str, args: &[&str]| match native_args(cmd, &argv(args)) {
        Err(InstallError::Failed(m)) => m,
        other => panic!("expected usage failure for {cmd} {args:?}, got {other:?}"),
    };
    assert_eq!(
        failed("deps", &["--os=all", "curl"]),
        "Error: Invalid usage: `brew deps --os=all` is not supported."
    );
    assert_eq!(
        failed("deps", &["--arch=all", "curl"]),
        "Error: Invalid usage: `brew deps --arch=all` is not supported."
    );
    assert!(matches!(
        native_args("deps", &argv(&["--os=linux", "--arch=intel", "curl"])),
        Ok((None, _))
    ));
    assert!(matches!(
        native_args("deps", &argv(&["--os", "sequoia", "curl"])),
        Ok((None, _))
    ));
}

#[test]
fn minimum_version_arity_is_a_usage_error() {
    let arity =
        "Error: Invalid usage: `--minimum-version` requires exactly one formula or cask argument.";
    let failed = |cmd: &str, args: &[&str]| match native_args(cmd, &argv(args)) {
        Err(InstallError::Failed(m)) => m,
        other => panic!("expected usage failure for {cmd} {args:?}, got {other:?}"),
    };
    assert_eq!(failed("outdated", &["--minimum-version=1.0"]), arity);
    assert_eq!(
        failed("outdated", &["--min-version", "1.0", "jq", "wget"]),
        arity
    );
    assert!(matches!(
        native_args("outdated", &argv(&["--minimum-version=1.0", "jq"])),
        Ok((None, _))
    ));
    assert!(matches!(
        native_args("outdated", &argv(&["--minimum-version", "1.0", "jq"])),
        Ok((None, _))
    ));
    // A blank value is absent: no arity demand.
    assert!(matches!(
        native_args("outdated", &argv(&["--minimum-version=", "jq", "wget"])),
        Ok((None, _))
    ));
    // upgrade shares the arity error and the native flag.
    assert_eq!(failed("upgrade", &["--minimum-version=1.0"]), arity);
    assert!(matches!(
        native_args("upgrade", &argv(&["--minimum-version=1.0", "jq"])),
        Ok((None, _))
    ));
    // The long spelling wins over the alias, blank included.
    assert!(matches!(
        native_args(
            "outdated",
            &argv(&["--min-version=1.0", "--minimum-version=", "jq", "wget"])
        ),
        Ok((None, _))
    ));
}

#[test]
fn deps_brewfile_rows_gate_admission() {
    let temp = aterm_tempfile::tempdir().unwrap();
    let file = temp.path().join("Brewfile");
    let flag = format!("--brewfile={}", file.to_str().unwrap());
    // Options and other entry types are read past: deps
    // uses only the brew/cask names.
    std::fs::write(
        &file,
        "tap \"me/tools\"\nbrew \"wget\", link: false\ncask \"firefox\"\nmas \"Xcode\", id: 497799835\nvscode \"dbaeumer.vscode-eslint\"\ncask_args appdir: \"/Applications\"\n",
    )
    .unwrap();
    assert!(matches!(
        native_args("deps", &argv(&[&flag])),
        Ok((None, _))
    ));
    // Ruby the line parser cannot read delegates.
    std::fs::write(&file, "brew \"wget\" if OS.mac?\n").unwrap();
    assert!(matches!(
        native_args("deps", &argv(&[&flag])),
        Err(InstallError::Unsupported(_))
    ));
    // A missing file is admitted: the command reports
    // brew's own "No Brewfile found".
    std::fs::remove_file(&file).unwrap();
    assert!(matches!(
        native_args("deps", &argv(&[&flag])),
        Ok((None, _))
    ));
}

#[test]
fn bundle_edit_admission_requires_names_and_one_type() {
    let temp = aterm_tempfile::tempdir().unwrap();
    let file = temp.path().join("Brewfile");
    std::fs::write(&file, "brew \"wget\"\n").unwrap();
    let flag = format!("--file={}", file.to_str().unwrap());
    let failed = |args: &[&str]| match native_args("bundle", &argv(args)) {
        Err(InstallError::Failed(m)) => m,
        other => panic!("expected Failed, got {other:?}"),
    };
    assert_eq!(
        failed(&["add"]),
        "Error: Invalid usage: This command requires at least 1 named argument."
    );
    assert_eq!(
        failed(&["add", "--formula", "--cask", "wget"]),
        "Error: Invalid usage: `add` supports only one type of entry at a time."
    );
    assert_eq!(
        failed(&["add", "--mas", "Xcode"]),
        "Error: Invalid usage: The `add` subcommand does not accept the `--mas` switch."
    );
    assert!(matches!(
        native_args("bundle", &argv(&["add", "wget"])),
        Ok((None, _))
    ));
    assert!(matches!(
        native_args("bundle", &argv(&["remove", &flag, "wget"])),
        Ok((None, _))
    ));
    assert_eq!(
        failed(&["remove", &flag]),
        "Error: Invalid usage: This command requires at least 1 named argument."
    );
    // Bare remove (direct dialect) stays the payload verb.
    assert!(matches!(
        native_args("bundle", &argv(&["remove"])),
        Ok((None, _))
    ));
}

#[test]
fn bundle_exec_child_tokens_are_opaque_values() {
    let temp = aterm_tempfile::tempdir().unwrap();
    let file = temp.path().join("Brewfile");
    std::fs::write(&file, "brew \"wget\"\n").unwrap();
    let flag = format!("--file={}", file.to_str().unwrap());
    let args = argv(&[
        "exec",
        &flag,
        "--",
        "/usr/bin/printf",
        "token with spaces",
        "--formulae",
        "-vq",
        "zzz-nonexist-000",
    ]);
    let (prepared, rest) = native_args("bundle", &args).unwrap();
    assert!(prepared.is_some());
    assert_eq!(rest, args);

    let args = argv(&[
        "exec",
        &flag,
        "/usr/bin/printf",
        "--check",
        "--install",
        "-vq",
    ]);
    let (_, rest) = native_args("bundle", &args).unwrap();
    assert_eq!(rest, args);
    assert!(matches!(
        native_args("bundle", &argv(&["exec", &flag, "--services", "/bin/echo"])),
        Err(InstallError::Unsupported(_))
    ));
    assert!(matches!(
        native_args("bundle", &argv(&["sh", &flag, "--services"])),
        Err(InstallError::Unsupported(_))
    ));
    assert!(matches!(
        native_args("bundle", &argv(&["exec", &flag])),
        Err(InstallError::Failed(_))
    ));
}

#[test]
fn bundle_install_keeps_its_write_lock_when_child_requests_a_dry_run() {
    for child_flag in ["-n", "--dry-run"] {
        let rest = argv(&["exec", "--install", "/bin/echo", child_flag]);
        assert!(!dispatch_dry_run("bundle", &rest));
    }
    assert!(dispatch_dry_run("install", &argv(&["--dry-run", "wget"])));
}

#[test]
#[cfg(unix)]
fn bundle_exec_dispatch_passes_child_help_through() {
    let temp = aterm_tempfile::tempdir().unwrap();
    let prefix = prefix::Prefix::at(&temp.path().join("prefix"));
    let brewfile = temp.path().join("Brewfile");
    let marker = temp.path().join("child-ran");
    std::fs::write(&brewfile, "").unwrap();
    let args = argv(&[
        "bundle",
        "exec",
        "--file",
        brewfile.to_str().unwrap(),
        "/bin/sh",
        "-c",
        "printf '%s' \"$2\" > \"$1\"",
        "sh",
        marker.to_str().unwrap(),
        "--help",
    ]);
    assert_eq!(dispatch(&prefix, &args), std::process::ExitCode::SUCCESS);
    assert_eq!(std::fs::read_to_string(marker).unwrap(), "--help");
}

#[test]
fn plural_kind_switches_canonicalize_exact_tokens_only() {
    assert_eq!(
        canonical_kind_switches(&argv(&["--formulae", "wget", "--casks"])),
        argv(&["--formula", "wget", "--cask"])
    );
    let once = canonical_kind_switches(&argv(&["--formula", "--hide=--casks", "casks"]));
    assert_eq!(canonical_kind_switches(&once), once);
    assert_eq!(
        native_args("tap", &argv(&["--formulae"])),
        Err(InstallError::Failed(
            "Error: invalid option: --formulae".to_string()
        ))
    );
    assert_eq!(
        native_args("doctor", &argv(&["--casks"])),
        Err(InstallError::Failed(
            "Error: invalid option: --casks".to_string()
        ))
    );
}

#[test]
fn kind_switch_conflicts_are_brew_usage_errors() {
    let failed = |cmd: &str, args: &[&str]| match native_args(cmd, &argv(args)) {
        Err(InstallError::Failed(m)) => m,
        other => panic!("expected usage failure for {cmd} {args:?}, got {other:?}"),
    };
    assert_eq!(
        failed("install", &["--formula", "--cask", "acl"]),
        "Error: Invalid usage: Options --cask and --formulae are mutually exclusive."
    );
    assert_eq!(
        failed("install", &["--ask", "acl"]),
        "Error: Calling the `--ask` switch is disabled! Use the default behaviour instead."
    );
    // Brew's other `odisabled` switches: same parser refusal,
    // replacement text verbatim or "no replacement".
    assert_eq!(
        failed("deps", &["--eval-all", "acl"]),
        "Error: Calling the `--eval-all` switch is disabled! Use the default trusted-tap behaviour instead."
    );
    assert_eq!(
        failed("tap", &["--eval-all"]),
        "Error: Calling the `--eval-all` switch is disabled! Use the default tap verification behaviour instead."
    );
    assert_eq!(
        failed("info", &["--fetch-manifest", "acl"]),
        "Error: Calling the `--fetch-manifest` switch is disabled! There is no replacement."
    );
    assert_eq!(
        failed("ls", &["--installed-as-dependency"]),
        "Error: Calling the `--installed-as-dependency` switch is disabled! Use --no-installed-on-request instead."
    );
    assert_eq!(
        failed("options", &["--command=install"]),
        "Error: Calling the `--command` flag is disabled! There is no replacement."
    );
    // `brew update` parses in shell: admission admits and
    // cmd_update owns --merge/unknown-option ordering.
    assert!(native_args("up", &argv(&["--merge"])).is_ok());
    assert!(native_args("update", &argv(&["--bogus"])).is_ok());
    assert_eq!(
        failed("list", &["--versions", "--installed-on-request"]),
        "Error: Invalid usage: Options --versions and --installed-on-request are mutually exclusive."
    );
    assert_eq!(
        failed("list", &["--cask", "--poured-from-bottle"]),
        "Error: Invalid usage: Options --cask and --poured-from-bottle are mutually exclusive."
    );
    assert_eq!(
        failed("install", &["--env=std", "acl"]),
        "Error: Calling `brew install --env` is disabled! Use `env :std` in specific formula files instead."
    );
    // Developer commands carry the same parser refusals even
    // though the commands themselves are not native.
    assert_eq!(
        failed("audit", &["--no-signing", "acl"]),
        "Error: Calling the `--[no-]signing` switch is disabled! There is no replacement."
    );
    assert_eq!(
        failed("bottle", &["--committer=x"]),
        "Error: Calling the `--committer` flag is disabled! There is no replacement."
    );
    assert_eq!(
        failed("update-python-resources", &["--silent", "acl"]),
        "Error: Calling the `-s` switch is disabled! There is no replacement."
    );
    assert_eq!(
        failed("test-bot", &["--skip-recursive-dependents"]),
        "Error: Calling the `--[no-]skip-recursive-dependents` switch is disabled! Use the default behaviour instead."
    );
    // Parser conflict groups: every present member is listed
    // in declaration order, short spellings by long name.
    assert_eq!(
        failed("outdated", &["-v", "--json"]),
        "Error: Invalid usage: Options --verbose and --json are mutually exclusive."
    );
    assert_eq!(
        failed("outdated", &["--quiet", "--json", "--verbose"]),
        "Error: Invalid usage: Options --quiet and --verbose and --json are mutually exclusive."
    );
    assert_eq!(
        failed(
            "leaves",
            &["--installed-on-request", "--installed-as-dependency"]
        ),
        "Error: Invalid usage: Options --installed-on-request and --installed-as-dependency are mutually exclusive."
    );
    assert_eq!(
        failed("log", &["-1", "--max-count=2", "acl"]),
        "Error: Invalid usage: Options -1 and --max-count are mutually exclusive."
    );
    assert_eq!(
        failed("trust", &["--tap", "--formula", "--cask", "x/y"]),
        "Error: Invalid usage: Options --tap and --formula and --cask are mutually exclusive."
    );
    // Bundle toggles: canonical display names, --file last,
    // and --describe itself is a disabled switch.
    assert_eq!(
        failed("bundle", &["list", "--describe"]),
        "Error: Calling the `--describe` switch is disabled! Use the default behaviour instead."
    );
    assert_eq!(
        failed("bundle", &["check", "--formulae", "--no-brews"]),
        "Error: Invalid usage: Options --formula and --no-formula are mutually exclusive."
    );
    assert_eq!(
        failed("bundle", &["check", "--cask", "--no-cleanup-cask"]),
        "Error: Invalid usage: Options --cask and --no-cleanup-cask are mutually exclusive."
    );
    assert_eq!(
        failed("bundle", &["list", "--mas", "--no-dump-mas"]),
        "Error: Invalid usage: Options --mas and --no-dump-mas are mutually exclusive."
    );
    assert_eq!(
        failed("bundle", &["list", "--file=x", "--global"]),
        "Error: Invalid usage: Options --file and --global are mutually exclusive."
    );
    // Bundle subcommand switch scope, in argv order.
    assert_eq!(
        failed("bundle", &["list", "--no-describe"]),
        "Error: Invalid usage: The `list` subcommand does not accept the `--no-describe` switch."
    );
    assert_eq!(
        failed("bundle", &["list", "--force"]),
        "Error: Invalid usage: The `list` subcommand does not accept the `-f` switch."
    );
    assert_eq!(
        failed("bundle", &["env", "--formula"]),
        "Error: Invalid usage: The `env` subcommand does not accept the `--formula` switch."
    );
    assert_eq!(
        failed("bundle", &["check", "--no-mas"]),
        "Error: Invalid usage: The `check` subcommand does not accept the `--no-mas` switch."
    );
    assert_eq!(
        failed("bundle", &["--no-describe"]),
        "Error: Invalid usage: The `install` subcommand does not accept the `--no-describe` switch."
    );
    // Unknown subcommands (bundle and services) and
    // services switch scope.
    assert_eq!(
        failed("bundle", &["frobnicate"]),
        "Error: Invalid usage: unknown subcommand: `frobnicate`"
    );
    assert_eq!(
        failed("bundle", &["--file", "Brewfile", "frobnicate"]),
        "Error: Invalid usage: unknown subcommand: `frobnicate`"
    );
    assert_eq!(
        failed("services", &["frobnicate"]),
        "Error: Invalid usage: unknown subcommand: `frobnicate`"
    );
    // Subcommands declaring no named args: brew's arity
    // error, not a native refusal (r335).
    assert_eq!(
        failed("bundle", &["list", "foo"]),
        "Error: Invalid usage: This command does not take named arguments."
    );
    assert_eq!(
        failed("bundle", &["install", "foo"]),
        "Error: Invalid usage: This command does not take named arguments."
    );
    assert_eq!(
        failed("completions", &["link", "foo"]),
        "Error: Invalid usage: This command does not take named arguments."
    );
    assert_eq!(
        failed("developer", &["on", "foo"]),
        "Error: Invalid usage: This command does not take named arguments."
    );
    assert_eq!(
        failed("analytics", &["state", "foo"]),
        "Error: Invalid usage: This command does not take named arguments."
    );
    assert_eq!(
        failed("services", &["--file=x"]),
        "Error: Invalid usage: The `list` subcommand does not accept the `--file` flag."
    );
    assert_eq!(
        failed("services", &["stop", "--json"]),
        "Error: Invalid usage: The `stop` subcommand does not accept the `--json` switch."
    );
    // Parser depends_on constraints.
    assert_eq!(
        failed("deps", &["--prune", "acl"]),
        "Error: Invalid usage: `--prune` cannot be passed without `--tree`."
    );
    assert!(native_args("deps", &argv(&["--prune", "--tree", "acl"])).is_ok());
    assert_eq!(
        failed("info", &["--variations", "acl"]),
        "Error: Invalid usage: `--variations` cannot be passed without `--json`."
    );
    assert_eq!(
        failed("search", &["--open", "acl"]),
        "Error: Invalid usage: `--open` cannot be passed without `--pull-request`."
    );
    // Abbreviations expand before the gate, as in brew.
    assert_eq!(
        failed("deps", &["--prun", "acl"]),
        "Error: Invalid usage: `--prune` cannot be passed without `--tree`."
    );
    assert_eq!(
        failed("info", &["--cat", "install"]),
        "Error: Invalid usage: `--category` cannot be passed without `--analytics`."
    );
    // Boolean switches reject =values (needless argument),
    // and explicit JSON versions are validated per command.
    assert_eq!(
        failed("outdated", &["--quiet=x"]),
        "Error: needless argument: --quiet=x"
    );
    assert_eq!(
        failed("outdated", &["-q=x"]),
        "Error: needless argument: -q=x"
    );
    assert_eq!(
        failed("desc", &["--search=x", "acl"]),
        "Error: needless argument: --search=x"
    );
    assert_eq!(
        failed("outdated", &["--json=v1"]),
        "Error: `brew outdated --json=v1` is no longer supported. Use brew outdated --json=v2 instead."
    );
    assert_eq!(
        failed("tap-info", &["--json=v2", "homebrew/core"]),
        "Error: Invalid usage: invalid JSON version: v2"
    );
    assert!(native_args("cleanup", &argv(&["--prune=30"])).is_ok());
    // Required-value flags at the end of the option list.
    assert_eq!(
        failed("outdated", &["--severity"]),
        "Error: missing argument: --severity"
    );
    assert_eq!(
        failed("deps", &["--os", "--"]),
        "Error: missing argument: --os"
    );
    assert!(
        !matches!(
            native_args("deps", &argv(&["--brewfile"])),
            Err(InstallError::Failed(ref m)) if m.contains("missing argument")
        ),
        "optional-value --brewfile never lacks an argument"
    );
    // --os/--arch simulation values are validated as brew's
    // SimulateSystem validates them.
    assert_eq!(
        failed("deps", &["--os=plan9", "acl"]),
        "Error: Unknown OS: plan9"
    );
    assert_eq!(
        failed("deps", &["--os", "--tree", "acl"]),
        "Error: Unknown OS: --tree"
    );
    assert_eq!(
        failed("fetch", &["--arch=arm64", "acl"]),
        "Error: New arch must be :arm or :intel"
    );
    // Universally invalid regex syntax is brew's error,
    // not a delegation.
    assert_eq!(
        failed("search", &["/[/"]),
        "Error: /[/ is not a valid regex."
    );
    assert_eq!(
        failed("search", &["/+/"]),
        "Error: /+/ is not a valid regex."
    );
    assert_eq!(
        failed("desc", &["--search", "/(abc/"]),
        "Error: /(abc/ is not a valid regex."
    );
    assert!(native_args("outdated", &argv(&["--json"])).is_ok());
    assert!(
        native_args("deps", &argv(&["--", "--eval-all"])).is_ok(),
        "a disabled spelling after -- is a named argument"
    );
    assert!(native_args("install", &argv(&["--no-ask", "acl"])).is_ok());
    assert!(native_args("install", &argv(&["-y", "acl"])).is_ok());
    assert!(native_args("upgrade", &argv(&["--yes"])).is_ok());
    assert_eq!(
        failed("info", &["--formula", "--cask", "acl"]),
        "Error: Invalid usage: Options --formula and --cask are mutually exclusive."
    );
    assert_eq!(
        failed("uninstall", &["--zap", "--formula", "hello"]),
        "Error: Invalid usage: Options --formula and --zap are mutually exclusive."
    );
    assert_eq!(
        failed("list", &["--multiple", "--cask", "--pinned"]),
        "Error: Invalid usage: Options --multiple and --cask are mutually exclusive."
    );
    assert_eq!(
        failed("list", &["--multiple", "--pinned", "--full-name"]),
        "Error: Invalid usage: Options --pinned and --multiple are mutually exclusive."
    );
    assert_eq!(
        failed("list", &["--versions", "-l", "--full-name"]),
        "Error: Invalid usage: Options --versions and -l are mutually exclusive."
    );
    assert_eq!(
        failed("list", &["--full-name", "--multiple", "--versions"]),
        "Error: Invalid usage: Options --full-name and --versions are mutually exclusive."
    );
    assert_eq!(
        failed("list", &["--pinned", "-1"]),
        "Error: Invalid usage: Options --pinned and -1 are mutually exclusive."
    );
    // Named pinned listings are native.
    assert!(native_args("list", &argv(&["--pinned", "hello"])).is_ok());
    assert!(native_args("list", &argv(&["--pinned", "--versions", "hello"])).is_ok());
    // desc and search accept both kind switches (each handler
    // resolves them brew's way); plain list forms print
    // nothing on brew's Bash path, the rest conflict.
    assert!(native_args("desc", &argv(&["--formula", "--cask", "acl"])).is_ok());
    assert!(native_args("search", &argv(&["--formula", "--cask", "acl"])).is_ok());
    assert!(native_args("list", &argv(&["--formula", "--cask"])).is_ok());
    assert!(native_args("list", &argv(&["--formula", "--cask", "-l"])).is_ok());
    assert_eq!(
        failed("list", &["--formula", "--cask", "hello"]),
        "Error: Invalid usage: Options --formula and --cask are mutually exclusive."
    );
    assert_eq!(
        failed("list", &["--formula", "--cask", "--versions"]),
        "Error: Invalid usage: Options --formula and --cask are mutually exclusive."
    );
    assert_eq!(
        failed("list", &["--formula", "--cask", "--json", "--versions"]),
        "Error: `--formula` and `--cask` are mutually exclusive."
    );
    // list --json: brew's Bash fast path (plain errors) vs
    // the Ruby fall-through (Invalid usage) gate matrix.
    assert_eq!(
        failed("list", &["--json"]),
        "Error: `brew list --json` requires `--versions`."
    );
    assert_eq!(
        failed("list", &["--json", "jq"]),
        "Error: Invalid usage: `brew list --json` requires `--versions`."
    );
    assert_eq!(
        failed("list", &["--versions", "--json", "-l"]),
        "Error: `brew list --versions --json` cannot be combined with `-1`, `-l`, `-r` or `-t`."
    );
    assert_eq!(
        failed("list", &["--versions", "--json", "jq"]),
        "Error: Invalid usage: `brew list --versions --json` does not support named arguments."
    );
    assert_eq!(
        failed("list", &["--versions", "--json", "--pinned"]),
        "Error: Invalid usage: `brew list --versions --json` is only supported by the fast Bash path with `jq`."
    );
    assert_eq!(
        failed("list", &["--versions", "--json", "-l", "jq"]),
        "Error: Invalid usage: Options --versions and -l are mutually exclusive."
    );
    assert_eq!(
        failed("list", &["--versions", "-r"]),
        "Error: Invalid usage: Options --versions and -r are mutually exclusive."
    );
    // deps --installed/--missing is the handler's usage error.
    assert!(native_args("deps", &argv(&["--installed", "--missing"])).is_ok());
    assert_eq!(
        failed("pin", &["--formula", "--cask", "acl"]),
        "Error: Invalid usage: Options --formula and --cask are mutually exclusive."
    );
    assert!(native_args("pin", &argv(&["--cask", "caffeine"])).is_ok());
    assert!(native_args("tab", &argv(&["--installed-on-request", "wget"])).is_ok());
    assert!(native_args("which-formula", &argv(&["git"])).is_ok());
    assert!(native_args("which-formula", &argv(&["--explain", "g++"])).is_ok());
    assert!(native_args("which-formula", &argv(&[])).is_ok());
    assert!(matches!(
        native_args(
            "tab",
            &argv(&["--installed-on-request", "--no-installed-on-request", "wget"])
        ),
        Err(InstallError::Failed(m)) if m
            == "Error: Invalid usage: Options --installed-on-request and --no-installed-on-request are mutually exclusive."
    ));
    assert!(matches!(
        native_args("tab", &argv(&["--formula", "--cask", "--installed-on-request", "wget"])),
        Err(InstallError::Failed(m)) if m
            == "Error: Invalid usage: Options --formula and --cask are mutually exclusive."
    ));
    assert!(matches!(
        native_args("tab", &argv(&[])),
        Err(InstallError::Failed(m)) if m
            == "Error: Invalid usage: This command requires at least 1 installed formula or installed cask argument."
    ));
    assert!(native_args("unpin", &argv(&["--formula", "acl"])).is_ok());
    // brew pin/unpin declare min-1 installed named args;
    // bare invocation is the help-plus-Invalid-usage error.
    for c in ["pin", "unpin"] {
        assert!(matches!(
            native_args(c, &argv(&[])),
            Err(InstallError::Failed(m)) if m
                == "Error: Invalid usage: This command requires at least 1 installed formula or installed cask argument."
        ));
    }
    assert_eq!(
        failed("link", &["--formula", "--cask", "acl"]),
        "Error: Invalid usage: Options --formula and --cask are mutually exclusive."
    );
    assert!(native_args("link", &argv(&["--cask", "caffeine"])).is_ok());
    assert!(native_args("link", &argv(&["--force", "acl"])).is_ok());
    assert!(native_args("unlink", &argv(&["--formula", "-n", "acl"])).is_ok());
    assert!(native_args("link", &argv(&["--overwrite", "acl"])).is_ok());
    assert_eq!(
        native_args("unlink", &argv(&["--overwrite", "acl"])),
        Err(InstallError::Failed(
            "Error: invalid option: --overwrite".to_string()
        ))
    );
    assert!(matches!(
        native_args("link", &argv(&["--HEAD", "acl"])),
        Err(InstallError::Unsupported(_))
    ));
    assert_eq!(
        failed("log", &["--formula", "--cask", "acl"]),
        "Error: Invalid usage: Options --formula and --cask are mutually exclusive."
    );
    assert!(native_args("log", &argv(&["--cask", "caffeine"])).is_ok());
}

#[test]
fn deps_graph_forms_are_admitted() {
    // The browser form is native; the --tree/--graph conflict is
    // the handler's brew usage error, not an admission refusal.
    assert!(native_args("deps", &argv(&["--graph", "jq"])).is_ok());
    assert!(native_args("deps", &argv(&["--graph", "--dot", "jq"])).is_ok());
    assert!(native_args("deps", &argv(&["--tree", "--graph", "jq"])).is_ok());
}

#[test]
fn bundled_short_switches_expand_like_optionparser() {
    assert_eq!(
        canonical_kind_switches(&argv(&["-vq", "acl", "--", "-1l"])),
        argv(&["-v", "-q", "acl", "--", "-1l"])
    );
    assert!(native_args("info", &argv(&["-vq", "acl"])).is_ok());
    assert!(native_args("list", &argv(&["-1l"])).is_ok());
    assert!(native_args("info", &argv(&["-vz", "acl"])).is_err());
}

#[test]
fn brew_build_env_key_order_and_cc_drop_rule() {
    let temp = aterm_tempfile::tempdir().unwrap();
    let prefix = Prefix::at(temp.path());
    let vars = brew_build_env(&prefix);
    let keys: Vec<&str> = vars.iter().map(|(k, _)| k.as_str()).collect();
    assert_eq!(&keys[..5], &["CC", "CXX", "OBJC", "OBJCXX", "HOMEBREW_CC"]);
    assert_eq!(keys.last(), Some(&"PATH"));
    assert!(!vars_cc_differs(&vars));
    let custom = vec![
        ("CC".to_string(), "gcc-14".to_string()),
        ("HOMEBREW_CC".to_string(), "clang".to_string()),
    ];
    assert!(vars_cc_differs(&custom));
}

#[test]
fn native_admission_messages_are_precise() {
    let msg = |args: &[&str]| {
        let args = argv(args);
        match native_args(&args[0], &args[1..]) {
            Err(InstallError::Unsupported(m)) => m,
            other => panic!("expected native refusal, got {other:?}"),
        }
    };
    assert!(msg(&["services", "start", "wget"]).contains("'services' is not native"));
    let cat_args = argv(&["--analytics", "wget", "--category=other"]);
    assert_eq!(
        native_args("info", &cat_args),
        Err(InstallError::Failed(
            "Error: Invalid usage: `--category` must be one of install, \
             install-on-request, build-error when querying formulae."
                .replace("                 ", "")
        ))
    );
    assert_eq!(
        native_args("cleanup", &argv(&["--prune=soon"])),
        Err(InstallError::Failed(
            "Error: Invalid usage: `--prune` expects an integer or `all`.".to_string()
        ))
    );
    let days_args = argv(&["--analytics", "wget", "--days=40"]);
    assert_eq!(
        native_args("info", &days_args),
        Err(InstallError::Failed(
            "Error: Invalid usage: `--days` must be one of 30, 90, 365.".to_string()
        ))
    );
    assert!(valid_cask_version("4.2.1,20260531.1"));
    assert!(valid_cask_version("1.0"));
    assert!(!valid_cask_version("../1.0"));
    assert!(!valid_cask_version("1.0/2"));
    assert!(!valid_cask_version(""));
    let temp = aterm_tempfile::tempdir().unwrap();
    let fresh = Prefix::at(&temp.path().join("never-created"));
    assert_eq!(dispatch(&fresh, &argv(&["list"])), ExitCode::SUCCESS);
    assert!(!fresh.root.exists());
    assert_eq!(dispatch(&fresh, &argv(&["--version"])), ExitCode::SUCCESS);
    assert_eq!(
        dispatch(&fresh, &argv(&["--version", "extra"])),
        ExitCode::SUCCESS,
        "a leading --version ignores everything after it"
    );
    assert_eq!(
        dispatch(&fresh, &argv(&["--verbose"])),
        ExitCode::SUCCESS,
        "global flags alone are the no-arguments help case"
    );
    assert_eq!(dispatch(&fresh, &argv(&["-v", "-v"])), ExitCode::SUCCESS);
    assert_eq!(
        dispatch(&fresh, &argv(&["help", "frobnicate"])),
        ExitCode::from(1),
        "help for an unknown topic is brew's unknown-command error"
    );
    assert!(!fresh.root.exists());
    for q in [
        &["--repository", "homebrew/core"][..],
        &["--repo", "me/tools"][..],
        &["--cache"][..],
    ] {
        assert_eq!(dispatch(&fresh, &argv(q)), ExitCode::SUCCESS, "{q:?}");
        assert!(!fresh.root.exists(), "{q:?}");
    }
    assert!(matches!(
        native_args("frobnicate", &argv(&[])),
        Err(InstallError::Failed(m)) if m
            == "Error: Invalid usage: Unknown command: brew frobnicate"
    ));
    // Real brew commands abrew does not implement are known,
    // not unknown: they refuse as not native (and delegate
    // under the reroute) instead of claiming no such command.
    assert!(native_args("readall", &argv(&[])).is_ok());
    assert_eq!(
        native_args("readall", &argv(&["--os=plan9"])),
        Err(InstallError::Failed("Error: Unknown OS: plan9".to_string()))
    );
    assert!(matches!(
        native_args("vulns", &argv(&[])),
        Err(InstallError::Unsupported(m)) if m == "'vulns' is not native"
    ));
    assert!(native_args("missing", &argv(&["--hide="])).is_ok());
    assert!(matches!(
        native_args("search", &argv(&[])),
        Err(InstallError::Failed(m)) if m
            == "Error: Invalid usage: This command requires at least 1 text or regex argument."
    ));
    assert!(matches!(
        native_args("autoremove", &argv(&["wget"])),
        Err(InstallError::Failed(m)) if m
            == "Error: Invalid usage: This command does not take named arguments."
    ));
    assert!(matches!(
        native_args("info", &argv(&["--json=v9", "wget"])),
        Err(InstallError::Failed(m)) if m
            == "Error: Invalid usage: invalid JSON version: v9"
    ));
    assert!(matches!(
        native_args("untap", &argv(&["notatap"])),
        Err(InstallError::Failed(m)) if m == "Error: Invalid tap name: 'notatap'"
    ));
    assert!(matches!(
        native_args("list", &argv(&["--json"])),
        Err(InstallError::Failed(m)) if m
            == "Error: `brew list --json` requires `--versions`."
    ));
    // r316: desc/list qualified-name handling; core-qualified
    // read-only lookups admit, mutating commands delegate.
    assert!(matches!(
        native_args("desc", &argv(&["/^jq$/"])),
        Err(InstallError::Failed(m)) if m == "Error: Invalid tap name: '/^jq$'"
    ));
    assert!(matches!(
        native_args("desc", &argv(&["a//b"])),
        Err(InstallError::Failed(m)) if m == "Error: Invalid tap name: 'a/'"
    ));
    assert!(native_args("desc", &argv(&["homebrew/core/jq"])).is_ok());
    assert!(native_args("desc", &argv(&["foo/bar"])).is_ok());
    assert!(native_args("info", &argv(&["homebrew/core/jq"])).is_ok());
    assert!(native_args("info", &argv(&["homebrew/cask/caffeine"])).is_ok());
    assert!(native_args("info", &argv(&["foo/bar"])).is_ok());
    assert!(native_args("outdated", &argv(&["a/b/c"])).is_ok());
    assert!(native_args("deps", &argv(&["foo/bar"])).is_ok());
    assert!(native_args("uses", &argv(&["a/b/c"])).is_ok());
    assert!(native_args("log", &argv(&["foo/bar"])).is_ok());
    // r323: gist-logs admits brew's shape; exact-count
    // overflow phrases as the exact requirement.
    assert!(native_args("gist-logs", &argv(&["jq"])).is_ok());
    assert!(native_args("gist-logs", &argv(&["a/b/c"])).is_ok());
    assert!(matches!(
        native_args("gist-logs", &argv(&[])),
        Err(InstallError::Failed(m)) if m
            == "Error: Invalid usage: This command requires exactly 1 formula argument."
    ));
    assert!(matches!(
        native_args("gist-logs", &argv(&["jq", "wget"])),
        Err(InstallError::Failed(m)) if m
            == "Error: Invalid usage: This command requires exactly 1 formula argument."
    ));
    assert!(matches!(
        native_args("pin", &argv(&["foo/bar"])),
        Err(InstallError::Unsupported(_))
    ));
    assert!(native_args("outdated", &argv(&["homebrew/core/jq"])).is_ok());
    assert!(matches!(
        native_args("list", &argv(&["homebrew/core/jq"])),
        Err(InstallError::Failed(m)) if m
            == "Error: No available formula or cask with the name \"homebrew/core/jq\"."
    ));
    assert!(matches!(
        native_args("install", &argv(&["homebrew/core/jq"])),
        Err(InstallError::Unsupported(_))
    ));
    assert!(msg(&["audit"]).contains("'audit' is not native"));
}

#[test]
fn native_dispatch_refuses_unsupported_work_without_creating_a_prefix() {
    let temp = aterm_tempfile::tempdir().unwrap();
    let fresh = Prefix::at(&temp.path().join("never-created"));
    for args in [
        &["--real", "list"][..],
        &["services", "start", "wget"][..],
        &["install", "--HEAD", "wget"][..],
        &[
            "bundle",
            "--version",
            "4.5.8",
            "--sha256",
            "6a5a18e6c5bea214983ec102dea3126cbc2d162540f8d93387fa9730d1f03be3",
        ][..],
        &["bundle", "--force"][..],
        &["--repository"][..],
    ] {
        assert_eq!(dispatch(&fresh, &argv(args)), ExitCode::from(1), "{args:?}");
        assert!(
            !fresh.root.exists(),
            "{args:?}: unsupported work created a prefix"
        );
    }
    for args in [
        &["--version"][..],
        &["-v"][..],
        &["env"][..],
        &["--env"][..],
    ] {
        assert_eq!(dispatch(&fresh, &argv(args)), ExitCode::SUCCESS, "{args:?}");
        assert!(!fresh.root.exists(), "{args:?}");
    }
}

#[cfg(unix)]
#[test]
fn alias_refuses_non_utf8_arguments_without_replacing_the_bytes() {
    use std::os::unix::ffi::OsStringExt as _;
    assert_eq!(
        main_entry(vec![OsString::from_vec(b"not-utf8-\xff".to_vec())]),
        ExitCode::from(1)
    );
}

#[test]
fn subcommand_admission_shapes() {
    assert!(native_args("developer", &argv(&[])).is_ok());
    assert!(native_args("developer", &argv(&["on"])).is_ok());
    assert!(matches!(
        native_args("developer", &argv(&["bogus"])),
        Err(InstallError::Failed(_))
    ));
    assert!(native_args("analytics", &argv(&[])).is_ok());
    assert!(native_args("analytics", &argv(&["state"])).is_ok());
    assert!(matches!(
        native_args("analytics", &argv(&["on"])),
        Err(InstallError::Unsupported(_))
    ));
    assert!(matches!(
        native_args("analytics", &argv(&["bogus"])),
        Err(InstallError::Failed(_))
    ));
}

#[test]
fn disabled_env_vars_match_brew() {
    let env = |pairs: &[(&str, &str)]| {
        let owned: Vec<(String, String)> = pairs
            .iter()
            .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
            .collect();
        move |key: &str| owned.iter().find(|(k, _)| k == key).map(|(_, v)| v.clone())
    };
    // Any non-empty HOMEBREW_EVAL_ALL errors on eval commands…
    for value in ["1", "0", "false"] {
        assert_eq!(
            disabled_env_error("deps", env(&[("HOMEBREW_EVAL_ALL", value)])),
            Some(
                "Error: Calling HOMEBREW_EVAL_ALL is disabled! Use the default trusted-tap behaviour instead."
                    .to_string()
            )
        );
    }
    // …but an empty value is quiet, and other commands never read it.
    assert_eq!(
        disabled_env_error("deps", env(&[("HOMEBREW_EVAL_ALL", "")])),
        None
    );
    assert_eq!(
        disabled_env_error("list", env(&[("HOMEBREW_EVAL_ALL", "1")])),
        None
    );
    assert_eq!(
        disabled_env_error("tap", env(&[("HOMEBREW_EVAL_ALL", "1")])),
        None
    );
    // HOMEBREW_ASK is boolean: truthy errors on install family only.
    assert_eq!(
        disabled_env_error("install", env(&[("HOMEBREW_ASK", "1")])),
        Some(
            "Error: Calling HOMEBREW_ASK is disabled! Use the default behaviour instead."
                .to_string()
        )
    );
    for quiet in ["0", "false", ""] {
        assert_eq!(
            disabled_env_error("upgrade", env(&[("HOMEBREW_ASK", quiet)])),
            None
        );
    }
    assert_eq!(
        disabled_env_error("deps", env(&[("HOMEBREW_ASK", "1")])),
        None
    );
    // Cask-opts envs: presence (even `0`) errors, after ASK.
    assert_eq!(
        disabled_env_error("reinstall", env(&[("HOMEBREW_CASK_OPTS_BINARIES", "0")])),
        Some(
            "Error: Calling HOMEBREW_CASK_OPTS_BINARIES is disabled! Use HOMEBREW_CASK_OPTS instead."
                .to_string()
        )
    );
    assert_eq!(
        disabled_env_error(
            "install",
            env(&[
                ("HOMEBREW_ASK", "1"),
                ("HOMEBREW_CASK_OPTS_REQUIRE_SHA", "1")
            ])
        ),
        Some(
            "Error: Calling HOMEBREW_ASK is disabled! Use the default behaviour instead."
                .to_string()
        )
    );
    // Bundle envs: boolean with counter-vars, secrets first.
    assert_eq!(
        disabled_env_error(
            "bundle",
            env(&[
                ("HOMEBREW_BUNDLE_DESCRIBE", "1"),
                ("HOMEBREW_BUNDLE_NO_SECRETS", "1")
            ])
        ),
        Some(
            "Error: Calling HOMEBREW_BUNDLE_NO_SECRETS is disabled! Use the default behaviour instead."
                .to_string()
        )
    );
    assert_eq!(
        disabled_env_error(
            "bundle",
            env(&[
                ("HOMEBREW_BUNDLE_DESCRIBE", "1"),
                ("HOMEBREW_BUNDLE_NO_DESCRIBE", "1")
            ])
        ),
        None
    );
    assert_eq!(
        disabled_env_error("bundle", env(&[("HOMEBREW_BUNDLE_DUMP_DESCRIBE", "0")])),
        Some(
            "Error: Calling HOMEBREW_BUNDLE_DUMP_DESCRIBE is disabled! Use HOMEBREW_BUNDLE_DESCRIBE instead."
                .to_string()
        )
    );
    assert_eq!(
        disabled_env_error("list", env(&[("HOMEBREW_BUNDLE_DESCRIBE", "1")])),
        None
    );
}

#[test]
fn subcommand_parents_have_parser_preambles() {
    // brew's usage-error preamble for subcommand parents is
    // not its `brew help` text; each parent and subcommand
    // needs a recorded preamble.
    for name in [
        "analytics",
        "analytics.state",
        "analytics.on",
        "analytics.off",
        "developer",
        "developer.state",
        "developer.on",
        "developer.off",
        "completions",
        "completions.unlink",
        "completions.state",
        "completions.link",
    ] {
        assert!(
            help_texts::PREAMBLES.iter().any(|(n, _)| *n == name),
            "{name} preamble missing"
        );
    }
    assert_eq!(help_texts::PREAMBLES.len(), 34);
}

#[test]
fn maintainer_commands_are_known_but_unlisted() {
    // brew 7.0.13 resolves these for help/dispatch but omits
    // them from `commands` output and did-you-mean.
    for name in ["pr-pull", "vendor-install", "generate-cask-api"] {
        assert!(BREW_MAINTAINER_COMMANDS.contains(&name), "{name}");
        assert!(!command_names(true).contains(&name), "{name} listed");
        assert!(
            help_texts::TEXTS.iter().any(|(n, _)| *n == name),
            "{name} help missing"
        );
        match native_args(name, &argv(&[])) {
            Err(InstallError::Unsupported(m)) => {
                assert!(m.contains("is not native"), "{name}: {m}")
            }
            other => panic!("{name}: expected native refusal, got {other:?}"),
        }
    }
    assert_eq!(BREW_MAINTAINER_COMMANDS.len(), 27);
}

#[test]
fn completions_unknown_subcommand_is_invalid_usage() {
    assert!(matches!(
        native_args("completions", &argv(&["bogus"])),
        Err(InstallError::Failed(m)) if m == "Error: Invalid usage: unknown subcommand: `bogus`"
    ));
}

#[test]
fn double_dash_ends_switch_parsing() {
    let rest: Vec<String> = ["--json=v2", "--", "-v", "hello", "--cask"]
        .iter()
        .map(|s| s.to_string())
        .collect();
    assert_eq!(operands(&rest), ["-v", "hello", "--cask"]);
    assert!(has_flag(&rest, "--json=v2", ""));
    assert!(!has_flag(&rest, "--cask", ""));
    assert!(!has_flag(&rest, "-v", ""));
}
