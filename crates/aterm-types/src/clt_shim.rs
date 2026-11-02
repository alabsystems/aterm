// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! The git-and-friends shim set a new Mac carries while Apple's Command Line Tools are missing
//! (System State design §6.3).
//!
//! Before the tools are installed, `/usr/bin/git`, `/usr/bin/cc`, `/usr/bin/clang`,
//! `/usr/bin/make`, `/usr/bin/python3` and the rest are Apple's `xcode-select` install stubs:
//! running any of them raises macOS's "install the command line developer tools?" dialog. A new
//! Mac's setup is already installing those tools (`base.clt`), so a shell that runs `git` should
//! say so and exit, not raise a second dialog. aterm puts a shim directory ahead of `/usr/bin` in
//! the shells it spawns while the tools are missing; each entry is a symlink onto the one `aterm`
//! binary, dispatched by its `argv[0]` to [`crate::clt_shim`]: while the tools install it prints
//! one line and exits at once (never raising the dialog); once the tools are present it hands the
//! call to Apple's own `/usr/bin/<tool>` (which then sets the SDK and raises no dialog) for a tool
//! they carry ([`carried`]); and it hands the call there too once nothing installs them (a
//! person's Stop). New tabs stop getting the directory on their PATH the moment nothing wants
//! it; the directory itself stays until the next launch, for the shells that still have it.
//!
//! This module is the pure model shared by the dispatch (`aterm-cli`) and the shells' PATH
//! (`aterm-gui`): the names to shim and the one line each prints. The dispatch itself, the
//! presence probe and the `exec` live where they can reach the process and the filesystem.

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

/// The one line a shim prints to its standard error with the tools missing, naming the tool a
/// person ran and that it arrives with the install already under way.
#[must_use]
pub fn arrival_line(tool: &str) -> String {
    format!(
        "{tool} arrives with Apple's Command Line Tools, installing: about {INSTALL_ETA_MINUTES} min"
    )
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

/// The shim directory under this account's state home (the `ATERM_STATE_HOME` dev seam, or a
/// test binary's own root, isolates it), or `None` where there is no state home to anchor it to.
/// The window lays it, and a launch removes one an earlier launch left, unless it adopted live
/// shells that may still run through it; a shim strips it from the PATH it hands the real tool.
#[must_use]
pub fn shim_dir() -> Option<std::path::PathBuf> {
    crate::dirs::state_dir().map(|dir| dir.join("clt-shim"))
}

/// The mark beside the shim directory `shim` (`clt-shim.wanted`) that the window keeps exactly
/// while it is installing the Command Line Tools: a setup pass wants them, they are missing,
/// this account may install them, and no install of them ended unfinished since one last
/// started. With it, a new shell gets the shim on its PATH, each integration keeps it there, and
/// a shim says the tools are on their way; without it, a shell that still has the directory on
/// its PATH (it stays until the next launch) reaches a shim that hands the call to Apple's own
/// `/usr/bin/<tool>`: the tools' own once they are there, Apple's route while nothing installs
/// them.
#[must_use]
pub fn wanted_mark(shim: &std::path::Path) -> std::path::PathBuf {
    shim.with_extension("wanted")
}

/// Whether the window is installing the Command Line Tools now: this account's shim directory
/// carries its [`wanted_mark`].
#[must_use]
pub fn shim_wanted() -> bool {
    shim_dir().is_some_and(|dir| wanted_mark(&dir).is_file())
}

/// The developer directories a call to `<tool>` is served from, in the order `xcode-select -p`
/// answers, read without running it: `$DEVELOPER_DIR` (`developer_dir`), the directory
/// `xcode-select` selected (`selected`, [`XCODE_SELECT_LINK`]'s target), Xcode's own
/// ([`XCODE_DEVELOPER`]), then the Command Line Tools ([`CLT_DIR`]). Only absolute ones count,
/// each once.
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
    {
        if !dirs.iter().any(|known| known == dir) {
            dirs.push(dir.to_owned());
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
/// developer directory carries it, and off macOS.
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
        |path| std::fs::metadata(path).is_ok_and(|meta| meta.is_file()),
    )
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
    fn the_wanted_mark_sits_beside_the_shim_directory() {
        assert_eq!(
            wanted_mark(std::path::Path::new("/state/aterm/clt-shim")),
            std::path::PathBuf::from("/state/aterm/clt-shim.wanted")
        );
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
    fn the_arrival_line_names_the_tool_and_never_a_command() {
        let line = arrival_line("git");
        assert!(line.starts_with("git arrives with Apple's Command Line Tools"));
        assert!(line.contains("installing"));
        // It must not look like an instruction a person could paste.
        assert!(!line.contains("xcode-select"));
        assert!(!line.contains("sudo"));
    }
}
