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
//! binary, dispatched by its `argv[0]` to [`crate::clt_shim`]: it prints one line and exits at
//! once (never raising the dialog), and `exec`s the real tool once the tools are present. The
//! directory is gone from new tabs once the tools exist.
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
