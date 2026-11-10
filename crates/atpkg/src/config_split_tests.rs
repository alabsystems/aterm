// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

#[cfg(unix)]
use std::ffi::OsStr;
use std::path::Path;

use super::split;

#[cfg(unix)]
#[test]
fn a_complete_launchd_answer_preserves_native_bytes_and_trims_only_newlines() {
    use std::os::unix::{ffi::OsStringExt as _, process::ExitStatusExt as _};
    let answer = |code, stdout| std::process::Output {
        status: std::process::ExitStatus::from_raw(code),
        stdout,
        stderr: Vec::new(),
    };
    assert_eq!(
        super::launchd_value(answer(0, b"/private/\xff config\n\n".to_vec())).unwrap(),
        Some(std::ffi::OsString::from_vec(
            b"/private/\xff config".to_vec()
        ))
    );
    assert_eq!(
        super::launchd_value(answer(0, b"\n\n".to_vec())).unwrap(),
        None
    );
    for (status, stdout) in [
        (1 << 8, b"/prefix-only".to_vec()),
        (1 << 8, Vec::new()),
        (9, Vec::new()),
    ] {
        assert!(super::launchd_value(answer(status, stdout)).is_err());
    }
    assert_eq!(
        super::launchd_value(answer(0, b"/trailing space \n".to_vec())).unwrap(),
        Some(std::ffi::OsString::from("/trailing space "))
    );
}

fn home() -> crate::testdir::Dir {
    crate::testdir::dir("atpkg-config-split-")
}

fn write(path: &Path, text: &str) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, text).unwrap();
}

/// No split where both read one folder: no `XDG_CONFIG_HOME` on either side, the same one,
/// `~/.config` spelled as the variable, or an `XDG_CONFIG_HOME/aterm` that is a link to
/// `~/.config/aterm` (the step this module offers, once taken).
#[cfg(unix)]
#[test]
fn one_folder_is_no_split() {
    let home = home();
    let h = home.as_path();
    let dots = h.join("dots");
    assert_eq!(split(None, None, h).unwrap(), None);
    assert_eq!(
        split(Some(dots.as_os_str()), Some(dots.as_os_str()), h).unwrap(),
        None
    );
    assert_eq!(
        split(Some(h.join(".config").as_os_str()), None, h).unwrap(),
        None
    );
    assert_eq!(
        split(Some(OsStr::new("")), None, h).unwrap(),
        None,
        "empty is unset"
    );
    std::fs::create_dir_all(h.join(".config/aterm")).unwrap();
    std::fs::create_dir_all(&dots).unwrap();
    std::os::unix::fs::symlink(h.join(".config/aterm"), dots.join("aterm")).unwrap();
    assert_eq!(
        split(Some(dots.as_os_str()), None, h).unwrap(),
        None,
        "linked: one folder"
    );
}

#[cfg(unix)]
#[test]
fn a_missing_config_folder_under_an_aliased_home_is_not_a_split() {
    let home = home();
    let h = home.as_path();
    let alias = h.join("home-alias");
    std::os::unix::fs::symlink(h, &alias).unwrap();
    assert!(!h.join(".config").exists());
    assert_eq!(
        split(Some(alias.join(".config").as_os_str()), None, h).unwrap(),
        None
    );

    let other = h.join("other");
    std::fs::create_dir(&other).unwrap();
    let other_alias = h.join("other-alias");
    std::os::unix::fs::symlink(&other, &other_alias).unwrap();
    assert!(
        split(Some(other_alias.join(".config").as_os_str()), None, h)
            .unwrap()
            .is_some()
    );
}

#[test]
fn an_unreadable_config_folder_is_not_treated_as_missing() {
    let home = home();
    let here = home.as_path().join("dots");
    let unread = super::split_with(Some(here.as_os_str()), None, home.as_path(), &|_: &Path| {
        Err(std::io::Error::from(std::io::ErrorKind::PermissionDenied))
    });
    assert!(unread.is_err(), "{unread:?}");
}

/// A shell's `XDG_CONFIG_HOME` that launchd does not have splits the two (round 13): the
/// person reads where each looks, which keys differ when both files exist, and a link offered
/// only where it replaces nothing — the app's folder linked to the shell's when the app's is
/// absent, the reverse when the shell's is, none when both exist.
#[test]
fn a_shell_s_xdg_config_home_launchd_lacks_is_a_split_said_plainly() {
    let home = home();
    let h = home.as_path();
    let dots = h.join("dots");

    write(
        &dots.join("aterm/aterm.toml"),
        "[update]\nenabled = false\n",
    );
    let s = split(Some(dots.as_os_str()), None, h)
        .unwrap()
        .expect("a split");
    assert_eq!(s.here, dots.join("aterm"));
    assert_eq!(s.app, h.join(".config/aterm"));
    assert_eq!(s.unify, Some((h.join(".config/aterm"), dots.join("aterm"))));
    assert_eq!(
        s.differs,
        Vec::<String>::new(),
        "one file: nothing to compare"
    );
    assert!(!s.compared);
    let said = s.said(h);
    assert!(!said.contains("the two say the same"), "{said}");
    for words in [
        "this aterm reads ~/dots/aterm/aterm.toml",
        "the app opened from Finder, the Dock or `open` reads ~/.config/aterm/aterm.toml",
        "launchd gives apps no XDG_CONFIG_HOME",
        "Settings writes that one",
    ] {
        assert!(said.contains(words), "{words:?} in {said}");
    }
    let command = s.unify_command().expect("a link step");
    assert!(
        command.ends_with(&format!(
            "ln -s {} {}",
            super::shell_word(&dots.join("aterm")),
            super::shell_word(&h.join(".config/aterm"))
        )),
        "{command}"
    );
    assert!(
        said.contains(&format!(
            "(undo: rm {})",
            super::shell_word(&h.join(".config/aterm"))
        )),
        "{said}"
    );

    let home = self::home();
    let h = home.as_path();
    let dots = h.join("dots");
    write(&h.join(".config/aterm/aterm.toml"), "font_px = 18\n");
    let s = split(Some(dots.as_os_str()), None, h)
        .unwrap()
        .expect("a split");
    assert_eq!(
        s.unify,
        Some((dots.join("aterm"), h.join(".config/aterm"))),
        "the shell's folder is absent: it links to the app's"
    );

    write(
        &dots.join("aterm/aterm.toml"),
        "font_px = 14\ntheme = \"dark\"\n[update]\nenabled = false\n[packages]\nenabled = true\n",
    );
    write(
        &h.join(".config/aterm/aterm.toml"),
        "font_px = 18\ntheme = \"dark\"\n[packages]\nenabled = true\n",
    );
    let s = split(Some(dots.as_os_str()), None, h)
        .unwrap()
        .expect("a split");
    assert_eq!(s.unify, None, "both exist: nothing is linked over either");
    assert_eq!(s.unify_command(), None);
    assert!(s.compared);
    assert_eq!(s.differs, ["font_px", "update"]);
    let said = s.said(h);
    assert!(said.contains("they differ in font_px, update"), "{said}");
    assert!(!said.contains("the two say the same"), "{said}");
    assert!(said.contains("Both exist, so nothing links them"), "{said}");

    write(
        &dots.join("aterm/aterm.toml"),
        "[packages]\nauto_install = false\n",
    );
    write(
        &h.join(".config/aterm/aterm.toml"),
        "[packages]\nauto_install = false\n",
    );
    let s = split(Some(dots.as_os_str()), None, h)
        .unwrap()
        .expect("a split");
    assert!(s.compared && s.differs.is_empty(), "{s:?}");
    let said = s.said(h);
    assert!(said.contains("; the two say the same today."), "{said}");
    assert!(!said.contains("they differ"), "{said}");
}

/// launchd's own `XDG_CONFIG_HOME` (a `launchctl setenv`) is the app's, and the split runs
/// the other way when the shell has none.
#[test]
fn launchd_s_xdg_config_home_is_the_app_s() {
    let home = home();
    let h = home.as_path();
    let app = h.join("appcfg");
    let s = split(None, Some(app.as_os_str()), h)
        .unwrap()
        .expect("a split");
    assert_eq!(s.here, h.join(".config/aterm"));
    assert_eq!(s.app, app.join("aterm"));
    assert!(
        s.said(h)
            .contains("launchd gives apps XDG_CONFIG_HOME=~/appcfg"),
        "{}",
        s.said(h)
    );
}
