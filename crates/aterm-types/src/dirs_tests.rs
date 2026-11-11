// Copyright 2026 Andrew Yates, Inc.
// SPDX-License-Identifier: Apache-2.0

use super::*;

#[cfg(unix)]
#[test]
fn aterm_toml_reader_holds_the_opened_targets_ownership_rule() {
    use std::os::unix::fs::{PermissionsExt as _, symlink};
    let dir = crate::test_scratch_dir("dirs");
    let path = dir.path().join("managed.toml");
    let link = dir.path().join("aterm.toml");
    let text = "[harness]\nenabled = false\n";
    assert!(read_aterm_config(&path).unwrap().is_none());
    std::fs::write(&path, text).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
    symlink(&path, &link).unwrap();
    for name in [&path, &link] {
        assert_eq!(read_aterm_config_text(name).unwrap().as_deref(), Some(text));
    }
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o664)).unwrap();
    assert!(aterm_config_file_refusal(&std::fs::metadata(&path).unwrap()).is_some());
    for name in [&path, &link] {
        assert!(
            matches!(
                read_aterm_config(name),
                Err(AtermConfigReadError::Refused(_))
            ),
            "a writable target must be refused through {name:?}"
        );
    }
}

/// A symlink there is followed; an oversized file is refused; and only no entry at all reads
/// as absent, since a link to nothing stands for settings that could not be read.
#[cfg(unix)]
#[test]
fn aterm_toml_is_read_through_a_link_and_a_dangling_one_is_no_absence() {
    use std::os::unix::fs::{PermissionsExt as _, symlink};
    let dir = crate::test_scratch_dir("dirs");
    let real = dir.path().join("dotfiles-aterm.toml");
    std::fs::write(&real, "shell = \"fish\"\n").unwrap();
    std::fs::set_permissions(&real, std::fs::Permissions::from_mode(0o600)).unwrap();
    let link = dir.path().join("aterm.toml");
    symlink(&real, &link).unwrap();
    assert_eq!(
        read_aterm_config_text(&link).unwrap().as_deref(),
        Some("shell = \"fish\"\n")
    );

    std::fs::write(&real, vec![b'#'; 1024 * 1024 + 1]).unwrap();
    assert!(read_aterm_config(&link).is_err());

    std::fs::remove_file(&real).unwrap();
    assert!(matches!(read_aterm_config(&link),
        Err(AtermConfigReadError::Unavailable(error))
            if error.kind() == std::io::ErrorKind::InvalidData));
    assert!(
        read_aterm_config(&dir.path().join("gone.toml"))
            .unwrap()
            .is_none()
    );
    std::fs::write(&real, [0xff]).unwrap();
    std::fs::set_permissions(&real, std::fs::Permissions::from_mode(0o600)).unwrap();
    assert!(matches!(read_aterm_config_text(&link),
        Err(AtermConfigReadError::Unavailable(error))
            if error.kind() == std::io::ErrorKind::InvalidData));
    assert!(matches!(
        read_aterm_config(dir.path()),
        Err(AtermConfigReadError::Unavailable(_))
    ));
}

/// `aterm.toml` passes for this user's file or root's, with no group or other write and no
/// set-id bit; another user's, or one others may write, is refused, saying why.
#[test]
fn aterm_toml_is_this_users_or_roots_and_nobody_elses_to_write() {
    let me = 501;
    assert_eq!(aterm_config_refusal(me, 0o100_600, me), None);
    assert_eq!(aterm_config_refusal(me, 0o100_644, me), None);
    assert_eq!(
        aterm_config_refusal(0, 0o100_444, me),
        None,
        "home-manager's store"
    );
    let other = aterm_config_refusal(502, 0o100_600, me).expect("another user's");
    assert!(other.contains("uid 502"), "{other}");
    for mode in [0o100_664, 0o100_646, 0o104_600, 0o102_600] {
        assert!(aterm_config_refusal(me, mode, me).is_some(), "{mode:o}");
        assert!(
            aterm_config_refusal(0, mode, me).is_some(),
            "root's {mode:o}"
        );
    }
}

/// The account's own shell is the one whose `$HOME` is the account record's home (as
/// written or as resolved), not root and not under a private state root; each other
/// case names itself.
#[cfg(unix)]
#[test]
fn the_accounts_own_shell_is_its_home_not_root_and_no_private_root() {
    let scratch = crate::test_scratch_dir("dirs");
    let dir = scratch.path();
    let home = dir.join("home");
    std::fs::create_dir(&home).unwrap();
    let link = dir.join("home-link");
    std::os::unix::fs::symlink(&home, &link).unwrap();
    let own = |private, euid, env: &std::path::Path| {
        not_own_account_with(private, euid, Some(env), Some(&home))
    };
    assert_eq!(own(false, 501, &home), None);
    assert_eq!(own(false, 501, &link), None, "the same home, resolved");
    assert_eq!(own(false, 0, &home), Some("it runs as root"));
    assert_eq!(
        own(false, 501, dir),
        Some("its HOME is not this account's home")
    );
    assert_eq!(own(true, 501, &home), Some(NOT_OWN_PRIVATE_ROOT));
    assert_eq!(
        not_own_account_with(false, 501, None, Some(&home)),
        Some("its HOME is not this account's home"),
        "no HOME is no match"
    );
}

/// One directory under every spelling: its path, a link to it and, on macOS, its data
/// volume firmlink, which `canonicalize` keeps apart; two directories, or a missing path
/// beside a real one, are not one.
/// NEGATIVE CONTROL: the firmlink alias and the directory resolve to different paths, so a
/// comparison of resolved paths (the rule before this one) calls them two.
#[cfg(unix)]
#[test]
fn same_directory_is_one_directory_under_every_spelling() {
    let scratch = crate::test_scratch_dir("dirs");
    let home = std::fs::canonicalize(scratch.path()).unwrap().join("home");
    let other = home.with_file_name("other");
    std::fs::create_dir(&home).unwrap();
    std::fs::create_dir(&other).unwrap();
    let link = home.with_file_name("home-link");
    std::os::unix::fs::symlink(&home, &link).unwrap();
    let missing = home.with_file_name("missing");
    assert!(same_directory(&home, &home));
    assert!(same_directory(&link, &home));
    assert!(same_directory(&missing, &missing), "a spelling is itself");
    assert!(!same_directory(&home, &other));
    assert!(!same_directory(&missing, &home));
    assert_eq!(
        not_own_account_with(false, 501, Some(&other), Some(&home)),
        Some("its HOME is not this account's home")
    );
    if cfg!(target_os = "macos") {
        let alias =
            std::path::Path::new("/System/Volumes/Data").join(home.strip_prefix("/").unwrap());
        assert_ne!(std::fs::canonicalize(&alias).unwrap(), home);
        assert!(same_directory(&alias, &home), "{}", alias.display());
        assert_eq!(
            not_own_account_with(false, 501, Some(&alias), Some(&home)),
            None
        );
    }
}

/// An item's journal goes where its account is. A test's `aterm doctor` or `atpkg
/// repair` child, its `$HOME` a scratch under the person's uid, journals under that
/// scratch's state root, the one every state reader resolves; negative control: the
/// account-record rule (root's, the old rule for every account), evaluated on the same
/// inputs, sends that journal into the person's own state.
#[cfg(unix)]
#[test]
fn the_journal_root_is_the_state_root_the_items_act_on_and_roots_own_under_sudo() {
    let state_of = |home: &str| {
        resolve_state_dir(
            None,
            StatePlatform {
                home: Some(home.into()),
                ..StatePlatform::default()
            },
        )
    };
    let person = PathBuf::from("/Users//alice");
    let scratch = state_of("/scratch/home");
    assert!(scratch.is_some());
    let child = account_state_dir_with(false, false, scratch.clone(), Some(person.clone()));
    assert_eq!(
        child, scratch,
        "a scratch HOME's run journals in that scratch"
    );
    let record_rule = account_state_dir_with(false, true, scratch.clone(), Some(person.clone()));
    assert_eq!(
        record_rule,
        state_of("/Users//alice"),
        "the account record's home is the person's state"
    );
    assert_ne!(child, record_rule);
    assert_eq!(
        account_state_dir_with(
            false,
            true,
            state_of("/Users//alice"),
            Some("/var/root".into())
        ),
        state_of("/var/root")
    );
    let private = Some(PathBuf::from("/scratch/state"));
    for root in [false, true] {
        assert_eq!(
            account_state_dir_with(true, root, private.clone(), Some("/var/root".into())),
            private
        );
        assert_eq!(
            account_state_dir_with(true, root, None, Some(person.clone())),
            None
        );
    }
    assert_eq!(
        account_state_dir_with(false, false, None, Some(person)),
        None,
        "no state root is no journal, never the account record's"
    );
    assert_eq!(account_state_dir_with(false, true, None, None), None);
    assert_eq!(account_state_dir(), state_dir());
    let live = account_state_dir_as(false);
    assert_eq!(
        live,
        state_dir(),
        "a non-root account journals where it acts"
    );
    let record = resolve_state_dir(
        None,
        StatePlatform {
            home: aterm_uds::account::home(),
            ..StatePlatform::default()
        },
    );
    assert!(record.is_some(), "this account has a home on record");
    assert_ne!(
        live, record,
        "the account record's home is the person's state"
    );
}

/// An empty or relative `$HOME` is not a home.
#[cfg(unix)]
#[test]
fn an_empty_or_relative_home_is_no_home() {
    use std::ffi::OsStr;
    assert_eq!(home_from_env(None), None);
    assert_eq!(home_from_env(Some(OsStr::new(""))), None);
    assert_eq!(home_from_env(Some(OsStr::new("Library"))), None);
    assert_eq!(home_from_env(Some(OsStr::new("./home"))), None);
    assert_eq!(
        home_from_env(Some(OsStr::new("/Users//someone"))),
        Some(PathBuf::from("/Users//someone"))
    );
}

/// The aterm.toml rule: a set `XDG_CONFIG_HOME` wins everywhere, then `%APPDATA%` on
/// Windows, else `HOME/.config` (macOS included); an empty value is unset.
#[test]
fn aterm_config_path_follows_the_one_rule_the_window_loads_by() {
    let xdg = OsStr::new("/xdg");
    let appdata = OsStr::new(r"C:\Users\who\AppData\Roaming");
    let home = OsStr::new("/Users//who");
    let under_home = PathBuf::from(home).join(".config/aterm/aterm.toml");

    assert_eq!(
        resolve_aterm_config_path(Some(xdg), Some(appdata), Some(home)),
        Some(PathBuf::from(xdg).join("aterm").join("aterm.toml")),
        "a set XDG_CONFIG_HOME wins everywhere, APPDATA and HOME notwithstanding"
    );
    assert_eq!(
        resolve_aterm_config_path(Some(OsStr::new("")), None, Some(home)),
        Some(under_home.clone()),
        "an empty XDG_CONFIG_HOME is unset"
    );
    let with_appdata = resolve_aterm_config_path(None, Some(appdata), Some(home));
    if cfg!(windows) {
        assert_eq!(
            with_appdata,
            Some(PathBuf::from(appdata).join("aterm").join("aterm.toml")),
            "Windows: %APPDATA%\\aterm\\aterm.toml"
        );
        assert_eq!(
            resolve_aterm_config_path(None, Some(OsStr::new("")), Some(home)),
            Some(under_home.clone()),
            "an empty APPDATA is unset, and HOME is the last resort"
        );
    } else {
        assert_eq!(
            with_appdata,
            Some(under_home.clone()),
            "APPDATA means nothing off Windows"
        );
    }
    assert_eq!(
        resolve_aterm_config_path(None, None, None),
        None,
        "nothing to resolve from is None, not a panic"
    );
    assert_eq!(
        aterm_config_path(),
        resolve_aterm_config_path(
            config_home().as_deref(),
            std::env::var_os("APPDATA").as_deref(),
            std::env::var_os("HOME").as_deref(),
        )
    );
}

/// The config directory: on every row of the XDG / APPDATA-only / HOME-only / none matrix
/// it is the folder holding `aterm.toml`, and the themes folder sits beside that file.
#[test]
fn the_themes_folder_sits_beside_aterm_toml_on_every_row_of_the_matrix() {
    let xdg = OsStr::new("/xdg");
    let appdata = OsStr::new(r"C:\Users\who\AppData\Roaming");
    let home = OsStr::new("/Users//who");
    let empty = OsStr::new("");
    type Row<'a> = (
        &'a str,
        Option<&'a OsStr>,
        Option<&'a OsStr>,
        Option<&'a OsStr>,
    );
    let rows: [Row<'_>; 6] = [
        ("all three set", Some(xdg), Some(appdata), Some(home)),
        ("XDG only", Some(xdg), None, None),
        ("APPDATA only", None, Some(appdata), None),
        ("HOME only", None, None, Some(home)),
        (
            "empty XDG and APPDATA",
            Some(empty),
            Some(empty),
            Some(home),
        ),
        ("none", None, None, None),
    ];
    for (row, x, a, h) in rows {
        let dir = resolve_aterm_config_dir(x, a, h);
        let file = resolve_aterm_config_path(x, a, h);
        assert_eq!(
            file.as_deref().and_then(std::path::Path::parent),
            dir.as_deref(),
            "{row}: the config dir is the folder aterm.toml is in"
        );
        assert_eq!(
            file.as_ref().map(|f| f.with_file_name("themes")),
            dir.as_ref().map(|d| d.join("themes")),
            "{row}: themes == config_dir/themes, beside aterm.toml"
        );
    }

    let appdata_only = resolve_aterm_config_dir(None, Some(appdata), None);
    if cfg!(windows) {
        assert_eq!(
            appdata_only,
            Some(PathBuf::from(appdata).join("aterm")),
            "Windows with only %APPDATA%: the config dir (and its themes) is there"
        );
    } else {
        assert_eq!(appdata_only, None, "APPDATA means nothing off Windows");
    }
    assert_eq!(
        resolve_aterm_config_dir(None, None, Some(home)),
        Some(PathBuf::from(home).join(".config").join("aterm"))
    );
    assert_eq!(
        resolve_aterm_config_dir(Some(xdg), Some(appdata), Some(home)),
        Some(PathBuf::from(xdg).join("aterm")),
        "a set XDG_CONFIG_HOME wins everywhere"
    );

    let live = resolve_aterm_config_dir(
        config_home().as_deref(),
        std::env::var_os("APPDATA").as_deref(),
        std::env::var_os("HOME").as_deref(),
    );
    assert_eq!(aterm_config_dir(), live);
    assert_eq!(
        crate::scheme::user_theme_dir(),
        live.map(|d| d.join("themes"))
    );
}

/// The state root: an absolute override wins, a relative one is refused, and otherwise the
/// platform default applies.
#[test]
fn state_dir_honours_an_absolute_override_and_refuses_a_relative_one() {
    let platform = StatePlatform {
        home: Some(PathBuf::from("/Users//who")),
        xdg_state_home: Some(PathBuf::from("/xdg/state")),
        local_app_data: Some(PathBuf::from(r"C:\Users\who\AppData\Local")),
    };
    #[cfg(not(windows))]
    let absolute_override = PathBuf::from("/tmp/aterm-state");
    #[cfg(windows)]
    let absolute_override = PathBuf::from(r"C:\tmp\aterm-state");
    assert_eq!(
        resolve_state_dir(Some(absolute_override.clone()), platform.clone()),
        Some(absolute_override),
        "the override is the root itself, no `aterm` appended"
    );
    assert_eq!(
        resolve_state_dir(Some(PathBuf::from("relative/state")), platform.clone()),
        None,
        "a relative override is refused, never joined onto the cwd"
    );
    let default = resolve_state_dir(None, platform.clone()).expect("a default resolves");
    #[cfg(target_os = "macos")]
    assert_eq!(
        default,
        PathBuf::from("/Users//who/Library/Application Support/aterm")
    );
    #[cfg(all(unix, not(target_os = "macos")))]
    assert_eq!(default, PathBuf::from("/xdg/state/aterm"));
    #[cfg(windows)]
    assert_eq!(default, PathBuf::from(r"C:\Users\who\AppData\Local\aterm"));
    assert!(default.is_absolute());
    #[cfg(all(unix, not(target_os = "macos")))]
    assert_eq!(
        resolve_state_dir(
            None,
            StatePlatform {
                xdg_state_home: Some(PathBuf::from("relative")),
                ..platform.clone()
            }
        ),
        Some(PathBuf::from("/Users//who/.local/state/aterm")),
        "a relative XDG_STATE_HOME falls through to the home default"
    );
    assert_eq!(
        resolve_state_dir(None, StatePlatform::default()),
        None,
        "nothing to resolve from is None, not a panic"
    );
}

/// The log directory: macOS `~/Library/Logs/aterm`, other Unix the XDG state dir's
/// `aterm/logs` (a relative XDG value falls through to home), under an absolute override
/// root, and `None` for a relative override or nothing to resolve from.
#[test]
fn logs_dir_follows_each_platforms_convention() {
    let platform = StatePlatform {
        home: Some(PathBuf::from("/Users//who")),
        xdg_state_home: Some(PathBuf::from("/xdg/state")),
        local_app_data: Some(PathBuf::from("C:\\Users\\who\\AppData\\Local")),
    };
    #[cfg(not(windows))]
    let absolute_override = PathBuf::from("/tmp/aterm-state");
    #[cfg(windows)]
    let absolute_override = PathBuf::from(r"C:\tmp\aterm-state");
    assert_eq!(
        resolve_logs_dir(Some(absolute_override.clone()), platform.clone()),
        Some(absolute_override.join("logs"))
    );
    assert_eq!(
        resolve_logs_dir(Some(PathBuf::from("relative/state")), platform.clone()),
        None
    );
    #[cfg(target_os = "macos")]
    assert_eq!(
        resolve_logs_dir(None, platform.clone()),
        Some(PathBuf::from("/Users//who/Library/Logs/aterm"))
    );
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        assert_eq!(
            resolve_logs_dir(None, platform.clone()),
            Some(PathBuf::from("/xdg/state/aterm/logs"))
        );
        assert_eq!(
            resolve_logs_dir(
                None,
                StatePlatform {
                    xdg_state_home: Some(PathBuf::from("relative")),
                    ..platform.clone()
                }
            ),
            Some(PathBuf::from("/Users//who/.local/state/aterm/logs"))
        );
    }
    let _ = &platform;
    assert_eq!(resolve_logs_dir(None, StatePlatform::default()), None);
}

/// The data root: an absolute state-home seam is the root itself, a relative one is
/// refused, and unset it is the OS data dir's `aterm` on each platform arm.
#[test]
fn aterm_data_dir_follows_the_state_home_seam_and_else_the_os_data_dir() {
    let platform = DataPlatform {
        home: Some(PathBuf::from("/Users//who")),
        xdg_data_home: Some(PathBuf::from("/xdg/data")),
        local_app_data: Some(PathBuf::from(r"C:\Users\who\AppData\Local")),
    };
    #[cfg(not(windows))]
    let absolute_override = PathBuf::from("/tmp/aterm-state");
    #[cfg(windows)]
    let absolute_override = PathBuf::from(r"C:\tmp\aterm-state");
    assert_eq!(
        resolve_aterm_data_dir(Some(absolute_override.clone()), platform.clone()),
        Some(absolute_override.clone()),
        "the override is the root itself, no `aterm` appended"
    );
    assert_eq!(
        resolve_aterm_data_dir(Some(absolute_override.clone()), platform.clone()),
        resolve_state_dir(Some(absolute_override.clone()), StatePlatform::default()),
        "under the seam the data root IS the state root"
    );
    for relative in ["relative/state", "./state", ""] {
        assert_eq!(
            resolve_aterm_data_dir(Some(PathBuf::from(relative)), platform.clone()),
            None,
            "a relative override ({relative:?}) is refused, never the owner's folder"
        );
    }
    let unset = resolve_aterm_data_dir(None, platform.clone());
    assert_eq!(
        unset,
        resolve_data_dir(platform.clone()).map(|data| data.join("aterm")),
        "unset, it is the OS data dir's `aterm`"
    );
    #[cfg(target_os = "macos")]
    assert_eq!(
        unset,
        Some(PathBuf::from(
            "/Users//who/Library/Application Support/aterm"
        ))
    );
    #[cfg(target_os = "linux")]
    {
        assert_eq!(unset, Some(PathBuf::from("/xdg/data/aterm")));
        assert_eq!(
            resolve_aterm_data_dir(
                None,
                DataPlatform {
                    xdg_data_home: Some(PathBuf::from("relative")),
                    ..platform.clone()
                }
            ),
            Some(PathBuf::from("/Users//who/.local/share/aterm")),
            "a relative XDG_DATA_HOME falls through to the home default — the data \
             dir, never the state dir's `.local/state`"
        );
    }
    #[cfg(windows)]
    assert_eq!(
        unset,
        Some(PathBuf::from(r"C:\Users\who\AppData\Local\aterm"))
    );
    assert_eq!(
        resolve_aterm_data_dir(None, DataPlatform::default()),
        None,
        "nothing to resolve from is None, not a panic"
    );
    if test_root().is_none() {
        assert_eq!(data_dir(), resolve_data_dir(data_platform()));
    }
}

/// In a Cargo test binary every aterm folder this module resolves lies under
/// the binary's own root unless the test named one, so no test reads or writes
/// the person's state, logs or config.
#[cfg(not(target_family = "wasm"))]
#[test]
fn test_binary_folders_lie_under_its_own_root() {
    let root = test_root().expect("this test binary's root");
    let state = crate::dev_seam!("ATERM_STATE_HOME").map(PathBuf::from);
    let want_state = state.clone().unwrap_or_else(|| root.join("state"));
    assert_eq!(state_dir(), Some(want_state.clone()));
    assert_eq!(aterm_data_dir(), Some(want_state.clone()));
    assert_eq!(logs_dir(), Some(want_state.join("logs")));
    assert_eq!(data_dir(), Some(root.join("data")));
    assert!(runs_under_private_state_root());
    let config = std::env::var_os("XDG_CONFIG_HOME")
        .filter(|x| !x.is_empty())
        .map_or_else(|| root.join("config"), PathBuf::from);
    assert_eq!(aterm_config_dir(), Some(config.join("aterm")));
    assert_eq!(aterm_config_path(), Some(config.join("aterm/aterm.toml")));

    let person = resolve_state_dir(
        None,
        StatePlatform {
            home: home_dir(),
            xdg_state_home: std::env::var_os("XDG_STATE_HOME").map(PathBuf::from),
            local_app_data: std::env::var_os("LOCALAPPDATA").map(PathBuf::from),
        },
    );
    if let Some(person) = person {
        assert!(!person.starts_with(root), "{person:?}");
        assert_ne!(state_dir(), Some(person));
    }
}

#[cfg(unix)]
#[test]
fn passwd_parsing_finds_the_uids_home() {
    let passwd = b"root:x:0:0:root:/root:/bin/bash\n\
                  nobody:x:65534:65534:nobody:/nonexistent:/usr/sbin/nologin\n\
                  testuser:x:1000:1000:Test User:/home/testuser:/bin/zsh\n";
    assert_eq!(home_from_passwd(passwd, 0), Some(PathBuf::from("/root")));
    assert_eq!(
        home_from_passwd(passwd, 1000),
        Some(PathBuf::from("/home/testuser"))
    );
    assert_eq!(home_from_passwd(passwd, 9999), None);
    let passwd = ["broken:", "x", ":500:500:Broken User::/bin/sh\n"].concat();
    assert_eq!(home_from_passwd(passwd.as_bytes(), 500), None);
    let relative = ["rel:", "x", ":501:501:Rel User:relative/home:/bin/sh\n"].concat();
    assert_eq!(home_from_passwd(relative.as_bytes(), 501), None);
    let passwd = b"short:x\n\
                  valid:x:42:42:User:/home/valid:/bin/sh\n\
                  \n";
    assert_eq!(
        home_from_passwd(passwd, 42),
        Some(PathBuf::from("/home/valid"))
    );
}
