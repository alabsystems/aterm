// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! The git-and-friends shim set a Mac carries from aterm's first launch until Apple's Command Line
//! Tools exist (System State design §6.3).
//!
//! Before the tools are installed, `/usr/bin/git`, `/usr/bin/cc`, `/usr/bin/clang`,
//! `/usr/bin/make`, `/usr/bin/python3` and the rest are Apple's `xcode-select` install stubs:
//! running any of them raises macOS's "install the command line developer tools?" dialog. aterm
//! installs those tools itself, so nothing it starts may raise that dialog. Two layers keep it
//! away while they are missing, each ending in the one `aterm` binary, dispatched by its
//! `argv[0]` to [`crate::clt_shim`]:
//!
//! - PATH: a shim directory ahead of `/usr/bin` in the shells aterm spawns, a symlink per tool.
//! - `DEVELOPER_DIR` ([`developer_dir`], set by [`developer_dir_move`] in every aterm process):
//!   on macOS 26.6 with the tools installed, Apple's stubs hand a call to
//!   `$DEVELOPER_DIR/usr/bin/xcrun <tool> <args>` when the variable names a directory holding
//!   one, and fail plainly (`missing DEVELOPER_DIR path`, exit 1) when it names none. On a
//!   macOS 27 guest without the tools (`lab mac --no-clt`, rounds 4 and 6 of
//!   `docs/measured/nux-round9-2026-11-02.md`) the stubs did the same rather than raise their
//!   dialog: `/usr/bin/git` by its full path in an aterm tab printed this module's line (exit 69)
//!   while the no-aterm control raised Apple's dialog. So a process that inherits the variable is
//!   covered whatever its PATH says: a login shell's
//!   `path_helper`, an agent's tool shell whose PATH was frozen by a snapshot, and a program that
//!   runs `/usr/bin/git` by its absolute path.
//!
//! What neither layer reaches, as traced: a process with no aterm process among its ancestors
//! (an app Finder, the Dock or Spotlight opened, one an aterm shell started with `open`, which
//! Launch Services starts from launchd, a launchd agent or daemon, a login item, another
//! terminal app and every shell in it), a process that empties its environment (`env -i`,
//! `sudo`'s default `env_reset`), one running as root (the process start names nothing for root),
//! and one that sets its own `DEVELOPER_DIR`. Any of them that runs `git` or another stub while
//! the tools are missing can still raise Apple's dialog.
//!
//! With the tools missing a shim prints one line and exits at once, and it never runs Apple's
//! stub. Once they are present it hands the call to Apple's own `/usr/bin/<tool>` (which then sets
//! the SDK and raises no dialog) for a tool they carry ([`carried`]).
//!
//! This module is the pure model shared by the dispatch (`aterm-cli`), the process start
//! (`aterm`) and the shells' PATH (`aterm-gui`): the names to shim and the one line each prints.
//! The dispatch itself and the `exec` live where they can reach the process.

/// The exit a shim returns after it prints its one line, with the tools missing: `EX_UNAVAILABLE`
/// (the tool it stands in for is not here yet), chosen so a build that runs `cc` stops cleanly
/// rather than loop raising the dialog.
pub const SHIM_EXIT: u8 = 69;

/// About how long Apple's Command Line Tools take to download and install, for the one line a
/// shim prints. An estimate, not a promise: the real time rides the network.
pub const INSTALL_ETA_MINUTES: u32 = 10;

/// The canonical `xcode-select` install stubs aterm shims while the Command Line Tools are
/// missing: the whole `/usr/bin` hardlink set that shares Apple's stub binary on current macOS
/// (measured against `/usr/bin/git`'s device and inode), plus the `xcrun` and
/// `xcodebuild` dispatchers. Any one of them raises the install dialog when run with the tools
/// missing, including the linker and binutils a build reaches directly (`ld`, `ar`, `pip3`), so
/// the set is not trimmed to the front ends. The shim set is this list filtered to the ones that
/// are actually there ([`runtime_tools`]), so a name macOS dropped shims nothing.
///
/// Deliberately conservative: only the names Apple's Command Line Tools own as `/usr/bin` stubs,
/// so the shim never stands in front of a tool a person installed themselves (Homebrew's `git`,
/// say, which sits ahead of `/usr/bin` on PATH and wins before the shim is ever reached).
pub const SHIM_TOOLS: &[&str] = &[
    // Version control and its plumbing.
    "git",
    "git-receive-pack",
    "git-shell",
    "git-upload-archive",
    "git-upload-pack",
    // The C/C++ toolchain front ends.
    "c89",
    "c99",
    "cc",
    "c++",
    "clang",
    "clang++",
    "clangd",
    "cpp",
    "gcc",
    "g++",
    "gcov",
    "llvm-g++",
    "llvm-gcc",
    // Build drivers and source generators.
    "asa",
    "bison",
    "bm4",
    "ctags",
    "flex",
    "flex++",
    "gm4",
    "gnumake",
    "gperf",
    "indent",
    "lex",
    "m4",
    "make",
    "mig",
    "rpcgen",
    "unifdef",
    "unifdefall",
    "yacc",
    // The Swift toolchain and the scripting runtime the tools carry.
    "sourcekit-lsp",
    "swift",
    "swiftc",
    "pip3",
    "python3",
    // The debugger, the tool runners and the documentation tools.
    "gatherheaderdoc",
    "hdxml2manxml",
    "headerdoc2html",
    "lldb",
    "xcodebuild",
    "xcrun",
    "xml2man",
    // The linker, binutils and Mach-O tools a link step reaches for.
    "ar",
    "as",
    "c++filt",
    "cmpdylib",
    "codesign_allocate",
    "ctf_insert",
    "dsymutil",
    "dwarfdump",
    "dyld_info",
    "install_name_tool",
    "ld",
    "libtool",
    "lipo",
    "lorder",
    "nm",
    "nmedit",
    "objdump",
    "otool",
    "pagestuff",
    "ranlib",
    "resolveLinks",
    "segedit",
    "size",
    "strings",
    "strip",
    "vtool",
    // The resource and file tools.
    "DeRez",
    "GetFileInfo",
    "ResMerger",
    "Rez",
    "SetFile",
    "SplitForks",
];

/// Whether `name` is one of [`SHIM_TOOLS`]: the argv0 by which a shim symlink dispatches here.
#[must_use]
pub fn is_shim_tool(name: &str) -> bool {
    SHIM_TOOLS.contains(&name)
}

/// The shimmed names whose `/usr/bin` program is macOS's own, not one of the install stubs that
/// hand the call to a developer directory: `xcrun` (its own binary, not one of the stub
/// hardlinks) finds each tool and the SDK in the developer directory, and no developer directory
/// carries an `xcrun` of its own (measured: the Command Line Tools' `usr/bin` has none). With the
/// tools present it runs as it is; with them missing it raises the install dialog as the stubs
/// do, so it is shimmed all the same.
pub const OS_TOOLS: &[&str] = &["xcrun"];

/// The shim set for this Mac: [`SHIM_TOOLS`] filtered to the names that exist as `/usr/bin`
/// stubs now (`exists("/usr/bin/<name>")`), so the directory links only what is actually there.
/// The caller passes a cheap existence probe; the pure filter is what a test drives.
pub fn runtime_tools(exists: impl Fn(&str) -> bool) -> Vec<&'static str> {
    SHIM_TOOLS
        .iter()
        .copied()
        .filter(|tool| exists(tool))
        .collect()
}

/// The one line a shim prints to its standard error with the tools missing and an install of them
/// under way ([`installing_mark`]), naming the tool a person ran.
#[must_use]
pub fn arrival_line(tool: &str) -> String {
    format!(
        "{tool} arrives with Apple's Command Line Tools, installing: about {INSTALL_ETA_MINUTES} min"
    )
}

/// The one line a shim prints to its standard error with the tools missing and nothing installing
/// them now (a person's Stop, an install that ended unfinished, an account that may not install
/// them): the call fails plainly, and the line says who can install them, System State's
/// Install… for an account that `may_install` them (one of the Mac's administrators), an
/// administrator for one that may not, whose System State offers no Install….
#[must_use]
pub fn missing_line(tool: &str, may_install: bool) -> String {
    if may_install {
        format!(
            "{tool} needs Apple's Command Line Tools, which are not installed: Install\u{2026} on System State adds them"
        )
    } else {
        format!(
            "{tool} needs Apple's Command Line Tools, which are not installed: only an administrator of this Mac can install them"
        )
    }
}

/// The tool a call to the shim named `shim` is for, for its one line: `xcrun`'s own first word
/// when that names a tool (as Apple's stubs call `$DEVELOPER_DIR/usr/bin/xcrun git --version`),
/// else `shim`.
#[must_use]
pub fn called_tool<'a>(shim: &'a str, rest: &'a [std::ffi::OsString]) -> &'a str {
    if shim != "xcrun" {
        return shim;
    }
    rest.first()
        .and_then(|word| word.to_str())
        .filter(|word| !word.is_empty() && !word.starts_with('-') && !word.contains('/'))
        .unwrap_or(shim)
}

/// The one line a shim prints, with the tools installed, for a name they do not carry (Apple's
/// `/usr/bin` stub for it has nothing to hand the call to): it exits [`SHIM_EXIT`] instead of
/// running that stub, which would find the shim on PATH again.
#[must_use]
pub fn absent_line(tool: &str) -> String {
    format!("{tool} is not part of Apple's Command Line Tools on this Mac")
}

/// Where Apple's Command Line Tools install.
pub const CLT_DIR: &str = "/Library/Developer/CommandLineTools";

/// The link `xcode-select --switch` sets and `xcode-select -p` reads: the developer directory
/// this Mac's `/usr/bin` stubs hand their calls to.
pub const XCODE_SELECT_LINK: &str = "/var/db/xcode_select_link";

/// Xcode's own developer directory, which `xcode-select -p` names when no link is set and Xcode
/// is in `/Applications`.
pub const XCODE_DEVELOPER: &str = "/Applications/Xcode.app/Contents/Developer";

/// Where a developer directory keeps its default toolchain's tools (`cc`, `clang`, `ld`,
/// `swift`, `lldb`, the binutils), beside its own `usr/bin` (`git`, `make`, `xcodebuild`): Xcode
/// keeps them only here, the Command Line Tools in `usr/bin` as well.
pub const DEFAULT_TOOLCHAIN_BIN: &str = "Toolchains/XcodeDefault.xctoolchain/usr/bin";

/// The environment variable that names the shim directory ([`shim_dir`]) to a shell aterm spawns
/// while the shim is wanted, and only then: aterm's shell integrations move that directory back
/// to just ahead of `/usr/bin` once the profile files have run, since a login shell's
/// `path_helper` puts `/usr/bin` in front of it.
pub const SHIM_ENV: &str = "ATERM_CLT_SHIM";

/// The variable Apple's stubs read before anything else on this Mac: the developer directory.
pub const DEVELOPER_DIR_ENV: &str = "DEVELOPER_DIR";

/// The shim directory under this account's state home (the `ATERM_STATE_HOME` dev seam, or a
/// test binary's own root, isolates it), or `None` where there is no state home to anchor it to.
/// The window lays it, and a launch removes one an earlier launch left once the tools exist,
/// unless it adopted live shells that may still run through it; a shim strips it from the PATH
/// it hands the real tool.
#[must_use]
pub fn shim_dir() -> Option<std::path::PathBuf> {
    crate::dirs::state_dir().map(|dir| dir.join("clt-shim"))
}

/// The directory a process's `DEVELOPER_DIR` names while the tools are missing, inside the shim
/// directory `shim`. It holds `usr/bin/xcrun` alone, a link onto `aterm`: Apple's stubs refuse a
/// directory without one (`invalid DEVELOPER_DIR path … missing xcrun`, a plain error and no
/// dialog, which is also what a process gets before the window laid it), and it holds no `git`,
/// so [`developer_tool`] never reads it as the tools.
#[must_use]
pub fn developer_dir(shim: &std::path::Path) -> std::path::PathBuf {
    shim.join("developer")
}

/// Where [`developer_dir`] keeps its programs, under it.
pub const DEVELOPER_DIR_BIN: &str = "usr/bin";

/// Whether `dir` is a [`developer_dir`], whichever account's state home it sits under: a shell
/// that changed `HOME` still hands on the one it inherited.
#[must_use]
pub fn is_shim_developer_dir(dir: &std::path::Path) -> bool {
    dir.file_name() == Some(std::ffi::OsStr::new("developer"))
        && dir.parent().and_then(std::path::Path::file_name)
            == Some(std::ffi::OsStr::new("clt-shim"))
}

/// The shim directory a program that ran from `dir` belongs to: `dir` itself, or, for a program in
/// its [`developer_dir`]'s `usr/bin`, the directory above that.
#[must_use]
pub fn shim_of_program_dir(dir: &std::path::Path) -> std::path::PathBuf {
    dir.ancestors()
        .nth(2)
        .filter(|developer| {
            is_shim_developer_dir(developer) && developer.join(DEVELOPER_DIR_BIN) == dir
        })
        .and_then(std::path::Path::parent)
        .unwrap_or(dir)
        .to_owned()
}

/// What a process start does to its own `DEVELOPER_DIR`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DeveloperDirMove {
    /// Leave it: a person's own choice, or nothing to do.
    Keep,
    /// The tools are missing: name this [`developer_dir`], for this process and all it starts.
    Set(std::path::PathBuf),
    /// The tools arrived: an inherited [`developer_dir`] goes.
    Clear,
}

/// [`DeveloperDirMove`] for a process whose `DEVELOPER_DIR` is `current`, with the tools `present`,
/// this account's shim directory `shim`, and `root` when it runs as root (which `sudo` gave a
/// clean environment, and whose state home is not the person's). Empty reads as unset, as Apple's
/// stubs read it (measured on macOS 26.6).
#[must_use]
pub fn developer_dir_move(
    current: Option<&std::ffi::OsStr>,
    present: bool,
    shim: Option<&std::path::Path>,
    root: bool,
) -> DeveloperDirMove {
    let current = current.filter(|value| !value.is_empty());
    match current {
        Some(value) => {
            if present && is_shim_developer_dir(std::path::Path::new(value)) {
                DeveloperDirMove::Clear
            } else {
                DeveloperDirMove::Keep
            }
        }
        None if present || root => DeveloperDirMove::Keep,
        None => shim.map_or(DeveloperDirMove::Keep, |shim| {
            DeveloperDirMove::Set(developer_dir(shim))
        }),
    }
}

/// The mark beside the shim directory `shim` (`clt-shim.wanted`) that the window keeps exactly
/// while the tools are missing: with it, a new shell gets the shim on its PATH and each
/// integration keeps it there.
#[must_use]
pub fn wanted_mark(shim: &std::path::Path) -> std::path::PathBuf {
    shim.with_extension("wanted")
}

/// The mark beside the shim directory `shim` (`clt-shim.installing`) that the window keeps while
/// an install of the tools is on its way: a shim's one line says so ([`arrival_line`]), or that
/// they are missing ([`missing_line`]) without it.
#[must_use]
pub fn installing_mark(shim: &std::path::Path) -> std::path::PathBuf {
    shim.with_extension("installing")
}

/// Resolve an Xcode application root to its developer contents, leaving a developer directory
/// unchanged. Like Apple's selector, this recognizes the contents directory rather than an
/// `.app` suffix, so aliases and renamed bundles work too. The invocation path is preserved.
///
/// # Errors
/// The contents directory could not be inspected for a reason other than absence.
pub fn developer_content_dir(dir: &std::path::Path) -> std::io::Result<std::path::PathBuf> {
    developer_content_dir_with(dir, |contents| {
        std::fs::metadata(contents).map(|meta| meta.is_dir())
    })
}

/// [`developer_content_dir`] with the caller's directory inspection. The inspection must
/// follow symbolic links, as the native metadata read does; this path policy reads nothing.
///
/// # Errors
/// The inspection failed for a reason other than absence or a non-directory component.
pub fn developer_content_dir_with(
    dir: &std::path::Path,
    is_dir: impl FnOnce(&std::path::Path) -> std::io::Result<bool>,
) -> std::io::Result<std::path::PathBuf> {
    let contents = dir.join("Contents/Developer");
    match is_dir(&contents) {
        Ok(true) => Ok(contents),
        Ok(false) => Ok(dir.to_owned()),
        Err(error)
            if matches!(
                error.kind(),
                std::io::ErrorKind::NotFound | std::io::ErrorKind::NotADirectory
            ) =>
        {
            Ok(dir.to_owned())
        }
        Err(error) => Err(error),
    }
}

/// The developer directories a call to `<tool>` is served from, in the order `xcode-select -p`
/// answers, read without running it: `$DEVELOPER_DIR` (`developer_dir`), the directory
/// `xcode-select` selected (`selected`, [`XCODE_SELECT_LINK`]'s target), Xcode's own
/// ([`XCODE_DEVELOPER`]), then the Command Line Tools ([`CLT_DIR`]). Only absolute ones count,
/// each once. Application roots resolve to their developer contents; an unread contents
/// directory retains its original candidate for the tool's own presence check.
#[must_use]
pub fn developer_dirs(
    developer_dir: Option<&std::path::Path>,
    selected: Option<&std::path::Path>,
) -> Vec<std::path::PathBuf> {
    let mut dirs: Vec<std::path::PathBuf> = Vec::new();
    for dir in [
        developer_dir,
        selected,
        Some(std::path::Path::new(XCODE_DEVELOPER)),
        Some(std::path::Path::new(CLT_DIR)),
    ]
    .into_iter()
    .flatten()
    .filter(|dir| dir.is_absolute())
    .map(|dir| developer_content_dir(dir).unwrap_or_else(|_| dir.to_owned()))
    {
        if !dirs.contains(&dir) {
            dirs.push(dir);
        }
    }
    dirs
}

/// The developer tools' own `<tool>` among `dirs` ([`developer_dirs`]): the first of each
/// directory's `usr/bin/<tool>` and then its default toolchain's ([`DEFAULT_TOOLCHAIN_BIN`])
/// that `is_file` finds, never one in `/usr/bin` itself (Apple's stubs), and never a name with
/// a path in it.
#[must_use]
pub fn developer_tool_in(
    tool: &str,
    dirs: &[std::path::PathBuf],
    is_file: impl Fn(&std::path::Path) -> bool,
) -> Option<std::path::PathBuf> {
    if tool.is_empty() || tool.contains(['/', '\\']) || tool == "." || tool == ".." {
        return None;
    }
    dirs.iter()
        .flat_map(|dir| {
            [
                dir.join("usr/bin").join(tool),
                dir.join(DEFAULT_TOOLCHAIN_BIN).join(tool),
            ]
        })
        .filter(|path| !path.starts_with("/usr/bin"))
        .find(|path| is_file(path))
}

/// [`developer_tool_in`] on this Mac: the developer tools' own `<tool>`, read with a stat and a
/// readlink (no child process, so nothing can raise Apple's install dialog). `None` where no
/// developer directory carries a nonempty executable file for it, and off macOS. Compiler
/// links are followed, as Apple's toolchains use them.
#[must_use]
pub fn developer_tool(tool: &str) -> Option<std::path::PathBuf> {
    if !cfg!(target_os = "macos") {
        return None;
    }
    let developer_dir = std::env::var_os("DEVELOPER_DIR").map(std::path::PathBuf::from);
    let selected = std::fs::read_link(XCODE_SELECT_LINK).ok();
    developer_tool_in(
        tool,
        &developer_dirs(developer_dir.as_deref(), selected.as_deref()),
        developer_executable,
    )
}

fn developer_executable(path: &std::path::Path) -> bool {
    let Ok(meta) = std::fs::metadata(path) else {
        return false;
    };
    if !meta.is_file() || meta.len() == 0 {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        meta.permissions().mode() & 0o111 != 0
    }
    #[cfg(not(unix))]
    {
        true
    }
}

/// Whether `/usr/bin/<tool>` runs a tool once the developer tools are present: one of
/// [`OS_TOOLS`], or one a developer directory carries (`carries`, [`developer_tool`] on this
/// Mac). Any other name's `/usr/bin` stub has nothing to hand the call to and raises Apple's
/// install dialog for Xcode, so a shim never runs it.
#[must_use]
pub fn carried(tool: &str, carries: impl Fn(&str) -> bool) -> bool {
    OS_TOOLS.contains(&tool) || carries(tool)
}

/// Whether the developer tools' `git` is on this Mac ([`developer_tool`]): the one presence
/// check the shim, the window's shim directory and the harness's repository reads share. With
/// it, `/usr/bin/git` runs git; without it, `/usr/bin/git` is Apple's stub, which raises the
/// install dialog.
#[must_use]
pub fn developer_git() -> bool {
    developer_tool("git").is_some()
}

/// `path` (a `PATH` value, `:`-separated) with every entry that is `shim` removed: what a shim
/// hands the real tool, so nothing it starts reaches the shim again.
#[must_use]
pub fn without_shim(path: &std::ffi::OsStr, shim: &std::path::Path) -> std::ffi::OsString {
    let kept: Vec<std::path::PathBuf> = std::env::split_paths(path)
        .filter(|entry| entry != shim)
        .collect();
    std::env::join_paths(kept).unwrap_or_else(|_| path.to_owned())
}

#[cfg(test)]
mod tests {
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
            move |path: &std::path::Path| {
                names.iter().any(|name| path == std::path::Path::new(name))
            }
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
}
