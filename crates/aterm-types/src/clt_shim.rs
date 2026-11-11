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
/// `xcode-select` selected (`selected`, [`read_developer_selection`]'s answer), Xcode's own
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

/// Read the developer-directory selection symlink without starting Apple's selector.
/// Relative targets resolve beside the selection link, as the filesystem resolves them;
/// invocation paths retain their spelling rather than resolving application aliases.
///
/// # Errors
/// The selection link could not be read.
pub fn read_developer_selection(link: &std::path::Path) -> std::io::Result<std::path::PathBuf> {
    let dir = std::fs::read_link(link)?;
    Ok(if dir.is_absolute() {
        dir
    } else {
        link.parent()
            .unwrap_or_else(|| std::path::Path::new("."))
            .join(dir)
    })
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

/// [`developer_tool_in`] on this Mac: the developer tools' own `<tool>`, inspected without a
/// child process, so nothing can raise Apple's install dialog. `None` where no developer
/// directory carries a nonempty executable file for it, and off macOS. Compiler links are
/// followed, as Apple's toolchains use them; resolved paths inside `/usr/bin` are excluded.
#[must_use]
pub fn developer_tool(tool: &str) -> Option<std::path::PathBuf> {
    if !cfg!(target_os = "macos") {
        return None;
    }
    let developer_dir = std::env::var_os("DEVELOPER_DIR").map(std::path::PathBuf::from);
    let selected = read_developer_selection(std::path::Path::new(XCODE_SELECT_LINK)).ok();
    developer_tool_in(
        tool,
        &developer_dirs(developer_dir.as_deref(), selected.as_deref()),
        developer_executable,
    )
}

fn developer_executable(path: &std::path::Path) -> bool {
    let Ok(resolved) = std::fs::canonicalize(path) else {
        return false;
    };
    if resolved.starts_with("/usr/bin") {
        return false;
    }
    let Ok(meta) = std::fs::metadata(resolved) else {
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
#[path = "clt_shim_tests.rs"]
mod tests;
