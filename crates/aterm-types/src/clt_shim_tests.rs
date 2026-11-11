// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

use super::*;

#[cfg(unix)]
#[test]
fn xcode_bundle_roots_and_aliases_expose_their_developer_tools() {
    use std::os::unix::fs::{PermissionsExt as _, symlink};
    let root = crate::test_scratch_dir("clt-shim");
    let bundle = root.path().join("Developer Suite");
    let developer = bundle.join("Contents/Developer");
    let git = developer.join("usr/bin/git");
    std::fs::create_dir_all(git.parent().unwrap()).unwrap();
    std::fs::write(&git, b"installed git").unwrap();
    std::fs::set_permissions(&git, std::fs::Permissions::from_mode(0o755)).unwrap();
    let alias = root.path().join("Alternate.app");
    symlink(&bundle, &alias).unwrap();
    for selected in [&bundle, &alias, &developer] {
        let dirs = developer_dirs(Some(selected), None);
        let expected = if selected == &developer {
            git.clone()
        } else {
            selected.join("Contents/Developer/usr/bin/git")
        };
        assert_eq!(
            developer_tool_in("git", &dirs, |path| path.starts_with(root.path())
                && developer_executable(path)),
            Some(expected)
        );
    }
    let selection = root.path().join("db/selection");
    std::fs::create_dir_all(selection.parent().unwrap()).unwrap();
    for (target, expected) in [
        (bundle.clone(), git.clone()),
        (
            std::path::PathBuf::from("../Developer Suite"),
            root.path()
                .join("db/../Developer Suite/Contents/Developer/usr/bin/git"),
        ),
    ] {
        symlink(target, &selection).unwrap();
        let selected = read_developer_selection(&selection).unwrap();
        assert_eq!(
            developer_tool_in("git", &developer_dirs(None, Some(&selected)), |path| {
                path.starts_with(root.path()) && developer_executable(path)
            }),
            Some(expected)
        );
        std::fs::remove_file(&selection).unwrap();
    }
}

#[cfg(unix)]
#[test]
fn invalid_developer_tools_do_not_hide_a_later_executable_and_links_still_work() {
    use std::os::unix::fs::{PermissionsExt as _, symlink};
    let root = crate::test_scratch_dir("clt-shim");
    let partial = root.path().join("partial");
    let complete = root.path().join("complete");
    let empty = partial.join("usr/bin/git");
    let not_executable = partial.join(DEFAULT_TOOLCHAIN_BIN).join("git");
    let good = complete.join("usr/bin/git");
    for (path, bytes, mode) in [
        (&empty, "", 0o755),
        (&not_executable, "not executable", 0o644),
        (&good, "executable fixture", 0o755),
    ] {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, bytes).unwrap();
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode)).unwrap();
    }
    let dirs = [partial, complete];
    let find = || developer_tool_in("git", &dirs, developer_executable);
    assert_eq!(find(), Some(good.clone()));
    std::fs::write(&empty, "repaired executable fixture").unwrap();
    assert_eq!(find(), Some(empty.clone()));
    std::fs::remove_file(&empty).unwrap();
    symlink(&good, &empty).unwrap();
    assert_eq!(find(), Some(empty));
}

#[cfg(target_os = "macos")]
#[test]
fn aliases_of_apple_stubs_do_not_hide_real_developer_tools() {
    use std::os::unix::fs::{PermissionsExt as _, symlink};
    let stub = std::path::Path::new("/usr/bin/git");
    let meta = std::fs::metadata(stub).unwrap();
    assert!(meta.is_file() && meta.len() > 0 && meta.permissions().mode() & 0o111 != 0);

    let root = crate::test_scratch_dir("clt-stub-alias");
    let root_alias = root.path().join("root");
    symlink("/", &root_alias).unwrap();
    let linked = root.path().join("linked");
    std::fs::create_dir_all(linked.join("usr/bin")).unwrap();
    symlink(stub, linked.join("usr/bin/git")).unwrap();
    let upper = root.path().join("upper");
    std::fs::create_dir_all(upper.join("usr/bin")).unwrap();
    symlink("/USR/BIN/git", upper.join("usr/bin/git")).unwrap();
    let complete = root.path().join("complete");
    let good = complete.join("usr/bin/git");
    std::fs::create_dir_all(good.parent().unwrap()).unwrap();
    std::fs::write(&good, "developer git fixture").unwrap();
    std::fs::set_permissions(&good, std::fs::Permissions::from_mode(0o755)).unwrap();

    for alias in [root_alias.clone(), root_alias.join(".."), linked, upper] {
        assert_eq!(
            developer_tool_in("git", std::slice::from_ref(&alias), developer_executable),
            None,
            "an alias of Apple's stub is not an installed developer tool: {alias:?}"
        );
        assert_eq!(
            developer_tool_in("git", &[alias, complete.clone()], developer_executable),
            Some(good.clone()),
            "the later real developer tool still wins"
        );
    }
}

#[test]
fn the_brief_s_named_tools_are_in_the_set() {
    for tool in [
        "git",
        "cc",
        "clang",
        "make",
        "python3",
        // The tools a build reaches directly, not only through a front end:
        // each is the same `/usr/bin` stub binary and raises the same dialog.
        "pip3",
        "ld",
        "as",
        "ar",
        "ranlib",
        "nm",
        "strip",
        "objdump",
        "libtool",
        "gnumake",
        "sourcekit-lsp",
    ] {
        assert!(is_shim_tool(tool), "{tool} must be shimmed");
    }
}

#[test]
fn a_name_not_apple_s_is_not_shimmed() {
    // A person's own tools, and aterm's own aliases, are never shimmed.
    for tool in ["aterm", "atpkg", "node", "ruby", "cargo", "rustc", "ls"] {
        assert!(!is_shim_tool(tool), "{tool} must not be shimmed");
    }
}

#[test]
fn the_set_has_no_name_twice_and_no_path() {
    for (i, tool) in SHIM_TOOLS.iter().enumerate() {
        assert!(!tool.is_empty(), "entry {i} is empty");
        assert!(
            !tool.contains('/') && !tool.contains('\\'),
            "{tool} names a path, not a tool"
        );
        assert!(!SHIM_TOOLS[..i].contains(tool), "{tool} is listed twice");
    }
}

#[test]
fn runtime_tools_keeps_only_what_exists() {
    // Only two of the canonical stubs are present on this (synthetic) Mac.
    let here = ["git", "cc"];
    let got = runtime_tools(|tool| here.contains(&tool));
    assert_eq!(got, vec!["git", "cc"]);
    // None present -> an empty shim set, so no directory is laid.
    assert!(runtime_tools(|_| false).is_empty());
}

/// The real tool is the selected developer directory's, else the Command Line Tools' own,
/// read by path; never Apple's `/usr/bin` stub (which would find the shim on PATH again),
/// never a relative directory, and never a name with a path in it.
#[test]
fn the_real_tool_is_a_developer_directorys_never_a_usr_bin_stub() {
    let xcode = std::path::Path::new("/Applications/Xcode.app/Contents/Developer");
    let dirs = developer_dirs(None, Some(xcode));
    assert_eq!(
        dirs,
        [xcode.to_owned(), std::path::PathBuf::from(CLT_DIR)],
        "the selected one first, the tools' own after"
    );
    let there = |names: &'static [&'static str]| {
        move |path: &std::path::Path| names.iter().any(|name| path == std::path::Path::new(name))
    };
    assert_eq!(
        developer_tool_in(
            "git",
            &dirs,
            there(&[
                "/Applications/Xcode.app/Contents/Developer/usr/bin/git",
                "/Library/Developer/CommandLineTools/usr/bin/git",
            ])
        ),
        Some(xcode.join("usr/bin/git"))
    );
    assert_eq!(
        developer_tool_in(
            "cc",
            &dirs,
            there(&["/Library/Developer/CommandLineTools/usr/bin/cc"])
        ),
        Some(std::path::PathBuf::from(
            "/Library/Developer/CommandLineTools/usr/bin/cc"
        ))
    );
    assert_eq!(
        developer_tool_in("pagestuff", &dirs, there(&["/usr/bin/pagestuff"])),
        None,
        "a name the tools lack has no real tool, whatever /usr/bin holds"
    );
    // `xcode-select --switch /` would make `/usr/bin` a developer directory's: never.
    assert_eq!(
        developer_tool_in(
            "git",
            &developer_dirs(Some(std::path::Path::new("/")), None),
            there(&["/usr/bin/git"])
        ),
        None
    );
    assert_eq!(
        developer_dirs(Some(std::path::Path::new("relative")), None),
        [
            std::path::PathBuf::from(XCODE_DEVELOPER),
            std::path::PathBuf::from(CLT_DIR)
        ]
    );
    for name in ["", "../git", "a/b", ".", ".."] {
        assert_eq!(developer_tool_in(name, &dirs, |_| true), None, "{name:?}");
    }
}

/// Xcode's own `cc`, `clang`, `ld`, `swift` and `lldb` live under its default toolchain
/// (`xcrun --find cc` names it), not in `Contents/Developer/usr/bin`, which holds `git`,
/// `make` and `xcodebuild`. With Xcode selected and the Command Line Tools absent,
/// `/usr/bin/cc` runs Xcode's toolchain `cc`, so the shim hands the call to that binary
/// rather than say the tools carry no `cc`.
#[test]
fn with_xcode_selected_the_shim_finds_xcodes_own_cc() {
    let xcode = std::path::Path::new(XCODE_DEVELOPER);
    let dirs = developer_dirs(None, Some(xcode));
    let git = xcode.join("usr/bin/git");
    let cc = xcode.join(DEFAULT_TOOLCHAIN_BIN).join("cc");
    let there = |path: &std::path::Path| path == git || path == cc;
    assert_eq!(developer_tool_in("git", &dirs, there), Some(git.clone()));
    assert_eq!(
        developer_tool_in("cc", &dirs, there),
        Some(cc.clone()),
        "Xcode's toolchain cc, which /usr/bin/cc runs"
    );
    // A directory's own `usr/bin` comes before its toolchain, as the Command Line Tools
    // carry both.
    let clt = std::path::Path::new(CLT_DIR);
    let both = [
        clt.join("usr/bin/cc"),
        clt.join(DEFAULT_TOOLCHAIN_BIN).join("cc"),
    ];
    assert_eq!(
        developer_tool_in("cc", &developer_dirs(None, Some(clt)), |path| both
            .iter()
            .any(|known| known == path)),
        Some(clt.join("usr/bin/cc"))
    );
}

/// `xcode-select -p` with no link set (`xcode-select --reset`, or Xcode never switched to)
/// names Xcode's own developer directory where Xcode is installed, ahead of the Command Line
/// Tools, so `/usr/bin/git` runs Xcode's git there: the shim reads the same directories in
/// the same order. `$DEVELOPER_DIR` and the link still come first.
#[test]
fn with_no_link_the_shim_reads_xcode_where_xcode_select_does() {
    let path = std::path::PathBuf::from;
    assert_eq!(
        developer_dirs(None, None),
        [path(XCODE_DEVELOPER), path(CLT_DIR)]
    );
    let beta = std::path::Path::new("/Applications/Xcode-beta.app/Contents/Developer");
    let own = std::path::Path::new("/opt/dev");
    assert_eq!(
        developer_dirs(Some(own), Some(beta)),
        [
            own.to_owned(),
            beta.to_owned(),
            path(XCODE_DEVELOPER),
            path(CLT_DIR)
        ]
    );
    let git = path(XCODE_DEVELOPER).join("usr/bin/git");
    assert_eq!(
        developer_tool_in("git", &developer_dirs(None, None), |found| found == git),
        Some(git.clone()),
        "the shim reads Xcode's git as present, as /usr/bin/git runs it"
    );
}

/// Once the tools are present a shim hands on `xcrun` (macOS's own, which no developer
/// directory carries) and every tool a developer directory carries, and nothing else: a
/// stub for a tool the tools lack would raise Apple's dialog.
#[test]
fn xcrun_and_the_tools_a_developer_directory_carries_are_handed_on() {
    let carries = |tool: &str| ["git", "cc"].contains(&tool);
    for tool in ["xcrun", "git", "cc"] {
        assert!(carried(tool, carries), "{tool}");
    }
    for tool in ["pagestuff", "xcodebuild", "llvm-gcc"] {
        assert!(!carried(tool, carries), "{tool}");
    }
    assert!(
        OS_TOOLS.iter().all(|tool| is_shim_tool(tool)),
        "each is shimmed"
    );
}

#[test]
fn the_marks_sit_beside_the_shim_directory() {
    let shim = std::path::Path::new("/state/aterm/clt-shim");
    assert_eq!(
        wanted_mark(shim),
        std::path::PathBuf::from("/state/aterm/clt-shim.wanted")
    );
    assert_eq!(
        installing_mark(shim),
        std::path::PathBuf::from("/state/aterm/clt-shim.installing")
    );
}

/// A process with the tools missing names the shim's developer directory for all it starts,
/// over an empty value too. Its own choice stands, and root's state home is not the person's.
/// Once the tools exist an inherited one goes.
/// NEGATIVE CONTROL: with the tools present nothing is set, and a person's own stays.
#[test]
fn a_process_names_the_shims_developer_dir_only_while_the_tools_are_missing() {
    use std::ffi::OsStr;
    use std::path::{Path, PathBuf};
    let shim = Some(Path::new("/state/clt-shim"));
    let ours = PathBuf::from("/state/clt-shim/developer");
    let set = DeveloperDirMove::Set(ours.clone());
    assert_eq!(developer_dir_move(None, false, shim, false), set);
    assert_eq!(
        developer_dir_move(Some(OsStr::new("")), false, shim, false),
        set
    );
    assert_eq!(
        developer_dir_move(None, false, shim, true),
        DeveloperDirMove::Keep
    );
    assert_eq!(
        developer_dir_move(None, false, None, false),
        DeveloperDirMove::Keep
    );
    let own = OsStr::new("/Applications/Xcode-beta.app/Contents/Developer");
    for present in [false, true] {
        assert_eq!(
            developer_dir_move(Some(own), present, shim, false),
            DeveloperDirMove::Keep
        );
    }
    assert_eq!(
        developer_dir_move(None, true, shim, false),
        DeveloperDirMove::Keep
    );
    assert_eq!(
        developer_dir_move(Some(ours.as_os_str()), false, shim, false),
        DeveloperDirMove::Keep
    );
    assert_eq!(
        developer_dir_move(Some(ours.as_os_str()), true, shim, false),
        DeveloperDirMove::Clear
    );
    assert_eq!(
        developer_dir_move(
            Some(OsStr::new("/elsewhere/clt-shim/developer")),
            true,
            shim,
            false
        ),
        DeveloperDirMove::Clear,
        "another state home's, inherited through a changed HOME"
    );
}

/// Its developer directory never reads as the tools: `$DEVELOPER_DIR` comes first among the
/// directories, and it holds `xcrun` alone.
#[test]
fn the_shims_developer_dir_never_reads_as_the_tools() {
    use std::path::Path;
    let ours = developer_dir(Path::new("/state/clt-shim"));
    let xcrun = ours.join(DEVELOPER_DIR_BIN).join("xcrun");
    let dirs = developer_dirs(Some(&ours), None);
    assert_eq!(dirs[0], ours);
    assert_eq!(developer_tool_in("git", &dirs, |path| path == xcrun), None);
    let tools = Path::new(CLT_DIR).join("usr/bin/git");
    assert_eq!(
        developer_tool_in("git", &dirs, |path| path == tools),
        Some(tools.clone()),
        "the tools themselves still read as present"
    );
}

#[test]
fn a_program_in_the_developer_dir_belongs_to_the_shim_above_it() {
    use std::path::{Path, PathBuf};
    assert_eq!(
        shim_of_program_dir(Path::new("/s/clt-shim/developer/usr/bin")),
        PathBuf::from("/s/clt-shim")
    );
    for dir in ["/s/clt-shim", "/s/other/developer/usr/bin", "/usr/bin"] {
        assert_eq!(shim_of_program_dir(Path::new(dir)), PathBuf::from(dir));
    }
}

#[test]
fn the_called_tool_is_xcruns_first_word_when_that_names_one() {
    use std::ffi::OsString;
    let words = |list: &[&str]| list.iter().map(OsString::from).collect::<Vec<_>>();
    assert_eq!(called_tool("xcrun", &words(&["git", "--version"])), "git");
    for rest in [&["--show-sdk-path"][..], &[], &["/bin/x"], &[""]] {
        assert_eq!(called_tool("xcrun", &words(rest)), "xcrun");
    }
    assert_eq!(called_tool("git", &words(&["status"])), "git");
}

#[test]
fn the_shim_leaves_the_path_it_hands_on() {
    let shim = std::path::Path::new("/state/clt-shim");
    assert_eq!(
        without_shim(
            std::ffi::OsStr::new("/opt/homebrew/bin:/state/clt-shim:/usr/bin:/state/clt-shim"),
            shim
        ),
        std::ffi::OsString::from("/opt/homebrew/bin:/usr/bin")
    );
    assert_eq!(
        without_shim(std::ffi::OsStr::new("/usr/bin:/bin"), shim),
        std::ffi::OsString::from("/usr/bin:/bin")
    );
}

#[test]
fn the_absent_line_names_the_tool_and_never_a_command() {
    let line = absent_line("pagestuff");
    assert!(line.starts_with("pagestuff is not part of Apple's Command Line Tools"));
    assert!(!line.contains("xcode-select") && !line.contains('`'));
}

#[test]
fn the_missing_line_names_the_tool_and_says_no_install_runs() {
    for may_install in [true, false] {
        let line = missing_line("git", may_install);
        assert!(line.starts_with("git needs Apple's Command Line Tools"));
        assert!(!line.contains("installing") && !line.contains("xcode-select"));
    }
}

/// An administrator is pointed at Install…; an account that may not install the tools, whose
/// System State has no Install…, is told who can.
/// NEGATIVE CONTROL: the administrator's line names Install….
#[test]
fn the_missing_line_offers_install_only_to_an_account_that_may_install() {
    assert!(missing_line("git", true).contains("Install\u{2026} on System State"));
    let standard = missing_line("git", false);
    assert!(!standard.contains("Install\u{2026}"), "{standard}");
    assert!(
        standard.contains("only an administrator of this Mac"),
        "{standard}"
    );
}

#[test]
fn the_arrival_line_names_the_tool_and_never_a_command() {
    let line = arrival_line("git");
    assert!(line.starts_with("git arrives with Apple's Command Line Tools"));
    assert!(line.contains("installing"));
    // It must not look like an instruction a person could paste.
    assert!(!line.contains("xcode-select"));
    assert!(!line.contains("sudo"));
}
