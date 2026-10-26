// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! [`TOOLS`]: every outside program aterm, its checks, and its tests run, each with its reason.

use super::{MACOS_HOSTS, Pin, Provider, Tool, When};

/// A program macOS (or, for a Linux or Windows lane, that OS) ships: spawned by absolute
/// path where the code has one.
const fn os(name: &'static str, at: &'static [&'static str], why: &'static str) -> Tool {
    Tool {
        name,
        why,
        at,
        missing: None,
        pin: None,
        provider: Provider::Os,
    }
}

/// A program of Apple's Command Line Tools.
const fn clt(name: &'static str, at: &'static [&'static str], why: &'static str) -> Tool {
    Tool {
        name,
        why,
        at,
        missing: None,
        pin: None,
        provider: Provider::Clt,
    }
}

/// A program ALab's default set carries, in `program`.
const fn toolset(name: &'static str, program: &'static str, why: &'static str) -> Tool {
    Tool {
        name,
        why,
        at: &[],
        missing: None,
        pin: None,
        provider: Provider::Toolset { program },
    }
}

/// A tool the checkout pins, which `verify provision` (with `when`'s flag) fetches or
/// builds into this user's cache ([`Provider::Workspace`]).
const fn workspace(
    name: &'static str,
    when: When,
    pin: Option<Pin>,
    at: &'static [&'static str],
    why: &'static str,
) -> Tool {
    Tool {
        name,
        why,
        at,
        missing: None,
        pin,
        provider: Provider::Workspace { when },
    }
}

/// A release host's own tool.
const fn release_host(name: &'static str, at: &'static [&'static str], why: &'static str) -> Tool {
    Tool {
        name,
        why,
        at,
        missing: None,
        pin: None,
        provider: Provider::ReleaseHost,
    }
}

/// A tool the Linux guest's provisioning provides (`linux_guest::provision_steps`).
const fn guest(name: &'static str, at: &'static [&'static str], why: &'static str) -> Tool {
    Tool {
        name,
        why,
        at,
        missing: None,
        pin: None,
        provider: Provider::Guest,
    }
}

/// A tool aterm provides no copy of: `because` says what removes the need, or whose
/// dependency it is.
const fn unprovided(
    name: &'static str,
    at: &'static [&'static str],
    because: &'static str,
    why: &'static str,
) -> Tool {
    Tool {
        name,
        why,
        at,
        missing: None,
        pin: None,
        provider: Provider::Unprovided { because },
    }
}

/// `tool`, whose refusal names `what` is missing rather than its bare name.
const fn missing(tool: Tool, what: &'static str) -> Tool {
    Tool {
        missing: Some(what),
        ..tool
    }
}

/// A host property no command changes: `what` it is.
const fn hardware(name: &'static str, what: &'static str, why: &'static str) -> Tool {
    Tool {
        name,
        why,
        at: &[],
        missing: None,
        pin: None,
        provider: Provider::Hardware(what),
    }
}

/// Every outside program aterm runs, and why.
pub const TOOLS: &[Tool] = &[
    os(
        "sh",
        &["/bin/sh"],
        "the POSIX shell: every `sh -c` child and every stand-in script a test writes runs under it",
    ),
    os(
        "bash",
        &[
            "/bin/bash",
            "/usr/bin/bash",
            r"C:\Program Files\Git\bin\bash.exe",
        ],
        "a shell aterm integrates: macOS's 3.2 is what people run, so the bash integration's tests \
         run it; on Windows, Git for Windows' bash, the one Claude Code's Bash tool runs",
    ),
    workspace(
        "bash-5.3",
        When::Always,
        Some(Pin {
            package: "bash",
            version: "5.3.20",
            exe: "bin/bash",
            built: true,
            hosts: MACOS_HOSTS,
        }),
        &[],
        "the bash integration's 5.3 paths (the `${ ...; }` reads, PS0's file-free hook) run only \
         on a bash 5.3 or later, the bash people install beside macOS's 3.2, which never takes \
         them: only a 5.3 can judge them. On macOS `verify provision` builds GNU's 5.3 with its \
         official patches from source",
    ),
    guest(
        "bash-3.2",
        &["/usr/local/bin/bash-3.2", "/bin/bash"],
        "the spare hold's SIGWINCH negative control needs a bash whose `read < fifo` fails when a \
         signal interrupts the open: 3.2, macOS's /bin/bash (bash 5 restarts it); the Linux guest \
         builds GNU's 3.2.57 as /usr/local/bin/bash-3.2",
    ),
    os(
        "zsh",
        &[
            "/bin/zsh",
            "/usr/bin/zsh",
            "/usr/local/bin/zsh",
            "/opt/homebrew/bin/zsh",
        ],
        "macOS's login shell, which aterm integrates; the zsh integration and the live headless tests run it",
    ),
    workspace(
        "fish",
        When::Always,
        Some(Pin {
            package: "fish",
            version: "4.9.3",
            exe: "bin/fish",
            built: true,
            hosts: MACOS_HOSTS,
        }),
        &[
            "/opt/homebrew/bin/fish",
            "/usr/local/bin/fish",
            "/usr/bin/fish",
        ],
        "only fish can judge the fish integration aterm ships (a fish integration once shipped broken \
         while every fish test passed without running fish). Two lanes: on macOS `verify provision` \
         builds the pin from upstream's source; the Linux guest runs apt's fish, the floor (§6.2)",
    ),
    workspace(
        "pwsh",
        When::Always,
        Some(Pin {
            package: "pwsh",
            version: "7.6.7",
            exe: "pwsh",
            built: false,
            hosts: MACOS_HOSTS,
        }),
        &[
            "/usr/local/bin/pwsh",
            "/opt/homebrew/bin/pwsh",
            "/usr/bin/pwsh",
            "/snap/bin/pwsh",
        ],
        "only PowerShell can judge the PowerShell integration aterm ships: the 3 unix tests and \
         atpkg's every-dialect lane are the only PowerShell coverage that runs anywhere; Microsoft's \
         signed archive, pinned by the tree (§6.3)",
    ),
    os(
        "cat",
        &["/bin/cat"],
        "an idle stdin reader and an echo child for the process, pty and containment tests",
    ),
    os(
        "sleep",
        &["/bin/sleep"],
        "the child that outlives its parent in the reaper, lifeline, deadline and kill tests",
    ),
    os(
        "true",
        &["/usr/bin/true", "/bin/true"],
        "a child that exits 0 at once, for spawn and exit-watch tests",
    ),
    os(
        "false",
        &["/usr/bin/false"],
        "a child that exits 1 at once, for the failure arms of spawn tests",
    ),
    os(
        "echo",
        &["/bin/echo"],
        "a child that prints its argv, for argv and quoting tests",
    ),
    os(
        "env",
        &["/usr/bin/env"],
        "prints a child's environment, for the environment scrub tests",
    ),
    os(
        "kill",
        &["/bin/kill"],
        "signals a test's own child by PID (never by name)",
    ),
    os(
        "ps",
        &["/bin/ps", "/usr/bin/ps"],
        "the process table off macOS, the release's and verify's process reads, and the oracle the \
         in-process readers are held to (row 8)",
    ),
    os(
        "pgrep",
        &["/usr/bin/pgrep"],
        "finds a test's own child by its unique argv, and init's activation check",
    ),
    os(
        "pkill",
        &["/usr/bin/pkill"],
        "the perf arena stops the ghostty copy it opened, matched by that copy's own path",
    ),
    os(
        "killall",
        &["/usr/bin/killall"],
        "init restarts the Dock and NotificationCenter after it writes their preferences",
    ),
    os(
        "lsof",
        &["/usr/sbin/lsof"],
        "the oracle libproc readers are held to in tests (the harness's row 8, kproc's and fabric's image inode: quiet children, and launchd, whose descriptors a user may not read), fabric's image inode on a unix other than macOS and Linux, and the lab's socket endpoints of a process `jobs::sockets_readable` passes",
    ),
    os(
        "ls",
        &["/bin/ls"],
        "a plain child whose output a test reads",
    ),
    os(
        "cp",
        &["/bin/cp"],
        "copies an app or a fixture tree with its modes (the perf arena, the lab, release)",
    ),
    os(
        "rm",
        &["/bin/rm"],
        "the machines agent's root removals, and the rm breaker's subject",
    ),
    os(
        "mkdir",
        &["/bin/mkdir"],
        "the machines GitHub item makes a checkout's parent as its user",
    ),
    os(
        "chmod",
        &["/bin/chmod"],
        "sets a fixture's mode as a child would, in the browser and atpkg tests",
    ),
    os(
        "chown",
        &["/usr/sbin/chown"],
        "the machines agent hands a user's file back to that user",
    ),
    os(
        "touch",
        &["/usr/bin/touch"],
        "marks a file from a child, in the lab's proofs",
    ),
    os(
        "readlink",
        &["/usr/bin/readlink"],
        "the machines time item reads /etc/localtime's link",
    ),
    os(
        "find",
        &["/usr/bin/find"],
        "the machines apps item lists an app's bundle",
    ),
    os(
        "mkfifo",
        &["/usr/bin/mkfifo"],
        "a FIFO a shell test holds open, so a write waits for the reader",
    ),
    os(
        "mktemp",
        &["/usr/bin/mktemp"],
        "verify's scratch directories",
    ),
    os(
        "ln",
        &["/bin/ln"],
        "a link made from a child, in atpkg's tests",
    ),
    os(
        "id",
        &["/usr/bin/id"],
        "the user a child runs as, for init's and the machines agent's account checks",
    ),
    os(
        "uname",
        &["/usr/bin/uname", "/bin/uname"],
        "the kernel and machine a child sees",
    ),
    os(
        "hostname",
        &["/bin/hostname"],
        "the host name a lab tape or agent records",
    ),
    os(
        "date",
        &["/bin/date"],
        "the oracle the zone reader (`aterm_uds::zone`) is held to (row 9)",
    ),
    os(
        "df",
        &["/bin/df"],
        "the free disk a release step needs, and the machines facts'",
    ),
    os(
        "pwd",
        &["/bin/pwd"],
        "the working directory a spawned child sees",
    ),
    os(
        "yes",
        &["/usr/bin/yes"],
        "a child that floods its stdout, for the backpressure tests",
    ),
    os(
        "stty",
        &["/bin/stty"],
        "a pty's modes as the child sees them",
    ),
    os(
        "nohup",
        &["/usr/bin/nohup"],
        "a release step that outlives its terminal",
    ),
    os(
        "sudo",
        &["/usr/bin/sudo"],
        "the one elevation: init's `base.clt` and the machines agent's install",
    ),
    os(
        "visudo",
        &[],
        "checks the machines agent's sudoers drop-in before it is placed",
    ),
    os(
        "gzip",
        &["/usr/bin/gzip"],
        "init's accounts item reads a gzipped plist",
    ),
    os(
        "unzip",
        &["/usr/bin/unzip"],
        "unpacks the pinned browser archives",
    ),
    os(
        "tar",
        &["/usr/bin/tar"],
        "install.sh unpacks the Linux release archive; `verify provision` rewrites fish's .tar.xz \
         source as a plain tar for atpkg's tar reader",
    ),
    os(
        "patch",
        &["/usr/bin/patch"],
        "`verify provision` applies GNU's official patches to the pinned bash's source",
    ),
    os(
        "zipinfo",
        &["/usr/bin/zipinfo"],
        "lists a release zip's members",
    ),
    os(
        "shasum",
        &["/usr/bin/shasum"],
        "install.sh checks the downloaded release archive's sha256",
    ),
    os(
        "script",
        &["/usr/bin/script"],
        "the harness tests' pty another process opened (macOS; the shell tests use aterm-pty, row 7)",
    ),
    os(
        "perl",
        &["/usr/bin/perl"],
        "an Apple platform binary whose environment is hidden: the harness tests' property (row 6)",
    ),
    os(
        "nc",
        &["/usr/bin/nc"],
        "another program's code identity for the keeper's designated-requirement test, and a socket peer (row 6)",
    ),
    os(
        "vim",
        &["/usr/bin/vim"],
        "what people run in a tab: the foreground handback lane runs it as a job (row 5)",
    ),
    os(
        "web2",
        &["/usr/share/dict/web2"],
        "a word list, the fixture of the text and search tests (row 5)",
    ),
    os(
        "curl",
        &["/usr/bin/curl"],
        "the product's hardened network transport (the updater, atpkg, abrew, init's GitHub check, \
         verify and release); install.sh needs it on Linux too (row 10)",
    ),
    os(
        "openssl",
        &["/usr/bin/openssl"],
        "release signing must use macOS's LibreSSL: an OpenSSL 3 writes a PKCS#12 `security import` \
         cannot read (row 35)",
    ),
    os(
        "sqlite3",
        &["/usr/bin/sqlite3"],
        "reads TCC.db (containment, and machines over SSH) and agy's WAL; a second reader of Apple's \
         schema would gain nothing (row 3)",
    ),
    os(
        "sandbox-exec",
        &["/usr/bin/sandbox-exec"],
        "every contained tab runs under it; an in-process `sandbox_init` calls the same API (row 2)",
    ),
    os(
        "ssh",
        &["/usr/bin/ssh"],
        "the machines and fleet transport (row 4)",
    ),
    os(
        "scp",
        &["/usr/bin/scp"],
        "the machines controller copies an enrolment payload (row 4)",
    ),
    os(
        "ssh-keygen",
        &["/usr/bin/ssh-keygen"],
        "signs and checks the machines envelope, and makes a host key (row 4)",
    ),
    os(
        "ssh-keyscan",
        &["/usr/bin/ssh-keyscan"],
        "the machines controller pins a discovered host's key (row 4)",
    ),
    os(
        "ssh-add",
        &["/usr/bin/ssh-add"],
        "the machines controller's agent-held key (row 4)",
    ),
    os(
        "codesign",
        &["/usr/bin/codesign"],
        "signs and checks every bundle, stand-in and update",
    ),
    os(
        "spctl",
        &["/usr/sbin/spctl"],
        "Gatekeeper's verdict on an update or a release",
    ),
    os(
        "hdiutil",
        &["/usr/bin/hdiutil"],
        "mounts and builds the disk images the installer and release use",
    ),
    os(
        "ditto",
        &["/usr/bin/ditto"],
        "copies a bundle with its metadata and signature intact",
    ),
    os(
        "security",
        &["/usr/bin/security"],
        "the keychain: release identities and the lab's probes",
    ),
    os(
        "launchctl",
        &["/bin/launchctl"],
        "the keeper's, the machines agent's and caffeine's launchd jobs",
    ),
    os(
        "defaults",
        &["/usr/bin/defaults"],
        "reads and writes the preferences init and machines converge",
    ),
    os(
        "plutil",
        &["/usr/bin/plutil"],
        "reads and writes property lists",
    ),
    os(
        "PlistBuddy",
        &["/usr/libexec/PlistBuddy"],
        "reads one key of a bundle's Info.plist",
    ),
    os(
        "tccutil",
        &["/usr/bin/tccutil"],
        "resets a privacy grant in the containment tests",
    ),
    os(
        "log",
        &["/usr/bin/log"],
        "reads the unified log for a crash or a privacy decision",
    ),
    os(
        "taskpolicy",
        &["/usr/sbin/taskpolicy"],
        "runs a verify child at background priority",
    ),
    os(
        "scutil",
        &["/usr/sbin/scutil"],
        "the computer and host names, and the network state",
    ),
    os(
        "pmset",
        &[],
        "the perf lanes' thermal read (`pmset -g therm`); as root, the machines power item and facts",
    ),
    os(
        "softwareupdate",
        &[],
        "installs the Command Line Tools headlessly as root, and OS updates",
    ),
    os(
        "hidutil",
        &["/usr/bin/hidutil"],
        "init's live Caps Lock remap",
    ),
    os("installer", &[], "the machines apps item installs a pkg"),
    os(
        "pkgutil",
        &["/usr/sbin/pkgutil"],
        "the machines apps item reads a pkg's receipt",
    ),
    os(
        "xcode-select",
        &["/usr/bin/xcode-select"],
        "where the Command Line Tools are, for prereq and the developer item",
    ),
    os(
        "DevToolsSecurity",
        &[],
        "the Developer Tools grant the debugger ask needs",
    ),
    os(
        "dns-sd",
        &["/usr/bin/dns-sd"],
        "the fleet's and the machines controller's Bonjour discovery",
    ),
    os(
        "open",
        &["/usr/bin/open"],
        "opens a URL, a pane of System Settings or an app",
    ),
    os(
        "osascript",
        &["/usr/bin/osascript"],
        "the notification fallback and init's account sign-outs",
    ),
    os(
        "sysctl",
        &["/usr/sbin/sysctl"],
        "the kernel's process, memory and CPU facts",
    ),
    os(
        "ioreg",
        &["/usr/sbin/ioreg"],
        "the machine's hardware facts",
    ),
    os("sw_vers", &["/usr/bin/sw_vers"], "the macOS version"),
    os(
        "dscl",
        &["/usr/bin/dscl"],
        "the machines facts' account records",
    ),
    os(
        "dscacheutil",
        &["/usr/bin/dscacheutil"],
        "containment's account lookups",
    ),
    os(
        "dsmemberutil",
        &["/usr/bin/dsmemberutil"],
        "containment's group membership",
    ),
    os(
        "fdesetup",
        &["/usr/bin/fdesetup"],
        "the machines facts' FileVault state",
    ),
    os(
        "mdfind",
        &["/usr/bin/mdfind"],
        "the consent observer finds an app by bundle id",
    ),
    os(
        "xattr",
        &["/usr/bin/xattr"],
        "a release image's quarantine and extended attributes",
    ),
    os(
        "chflags",
        &["/usr/bin/chflags"],
        "atpkg clears a store file's immutable flag",
    ),
    os(
        "caffeinate",
        &["/usr/bin/caffeinate"],
        "the machines agent holds the Mac awake through an apply",
    ),
    os(
        "vm_stat",
        &["/usr/bin/vm_stat"],
        "the machines facts' memory pressure",
    ),
    os(
        "vmmap",
        &["/usr/bin/vmmap"],
        "the lab's memory reads of a live process",
    ),
    os(
        "arch",
        &["/usr/bin/arch"],
        "runs a release step under one architecture",
    ),
    os(
        "trash",
        &["/usr/bin/trash"],
        "moves a stale copy to the Trash, never deleting it",
    ),
    os(
        "lsregister",
        &[
            "/System/Library/Frameworks/CoreServices.framework/Frameworks/LaunchServices.framework/Support/lsregister",
        ],
        "re-registers an installed dev app with Launch Services, so its bundle id opens that copy",
    ),
    os(
        "gsettings",
        &["/usr/bin/gsettings"],
        "GNOME's settings, for aterm-gui on Linux",
    ),
    os("xdg-open", &["/usr/bin/xdg-open"], "opens a URL on Linux"),
    os(
        "systemctl",
        &["/usr/bin/systemctl"],
        "the Linux user units of aterm-link",
    ),
    os(
        "systemd-analyze",
        &["/usr/bin/systemd-analyze"],
        "the Linux publishing lane checks its units",
    ),
    os(
        "cmd",
        &[],
        "Windows' shell, for the Windows spawn and shell-integration tests",
    ),
    os(
        "wsl.exe",
        &[],
        "the WSL bridge of the Windows shell-integration tests",
    ),
    os(
        "notepad.exe",
        &[],
        "opens a file for the person on Windows (the messages host)",
    ),
    os(
        "reg",
        &[],
        "the perf trend's machine id on Windows (MachineGuid)",
    ),
    os(
        "certutil",
        &[],
        "a file's sha256 on Windows (`certutil -hashfile`)",
    ),
    os(
        "ping",
        &[],
        "a child that runs until it is stopped, for the quit-safety and settings tests",
    ),
    clt(
        "cc",
        &[
            "/usr/bin/cc",
            "/Library/Developer/CommandLineTools/usr/bin/cc",
        ],
        "every Rust link, and the objc ABI tests' C half: Apple forbids redistributing it",
    ),
    clt(
        "git",
        &[
            "/usr/bin/git",
            "/Library/Developer/CommandLineTools/usr/bin/git",
        ],
        "the change cone, the forge, the release and every checkout; CLT's git also holds the \
         GitHub credential through its osxkeychain helper (row 22)",
    ),
    clt(
        "make",
        &["/usr/bin/make"],
        "`verify provision` builds the pinned bash by its own Makefile",
    ),
    clt(
        "lipo",
        &["/usr/bin/lipo"],
        "joins the shipped universal binary's slices",
    ),
    clt("strip", &["/usr/bin/strip"], "strips the shipped binary"),
    clt(
        "dsymutil",
        &["/usr/bin/dsymutil"],
        "the release's debug symbols",
    ),
    clt(
        "dwarfdump",
        &["/usr/bin/dwarfdump"],
        "checks a dSYM matches its binary",
    ),
    clt("swift", &["/usr/bin/swift"], "the AppKit measuring helpers"),
    clt(
        "xcrun",
        &["/usr/bin/xcrun"],
        "notarytool and stapler, for the release's notarization",
    ),
    clt(
        "install_name_tool",
        &["/usr/bin/install_name_tool"],
        "the producer relocates a packed Mach-O's load paths",
    ),
    clt(
        "otool",
        &["/usr/bin/otool"],
        "the producer's relocation reads, and the oracle atpkg::macho's load-command reader is \
         checked against (abrew reads with atpkg::macho, row 12)",
    ),
    unprovided(
        "xcodebuild",
        &["/Applications/Xcode.app/Contents/Developer/usr/bin/xcodebuild"],
        "it is full Xcode's; abrew reads the selected Xcode's version plist instead (row 13)",
        "the oracle abrew's Xcode probe is checked against: the selected Xcode's own `-version`, \
         never the /usr/bin shim, and only where an Xcode is selected (row 13)",
    ),
    toolset(
        "targo",
        "trust",
        "builds and tests the workspace: Trust is the toolchain",
    ),
    toolset("trustfmt", "trust", "verify fmt's formatter"),
    toolset("targo-tippy", "trust", "verify lint's linter"),
    toolset("trustdoc", "trust", "verify doc's doctests"),
    toolset(
        "ty",
        "ty",
        "checks every registered model under `verify spec` and `verify checkers` (release); plain \
         tests never run it",
    ),
    toolset(
        "ay",
        "ay",
        "checks the proof bundles' certificates (verify spec)",
    ),
    toolset(
        "claude",
        "claude",
        "the agent aterm's harness drives, by the owner's roster (row 31)",
    ),
    toolset(
        "codex",
        "codex",
        "an agent of the roster; the codex-alab proof runs the packaged build (row 31)",
    ),
    toolset("agy", "agy", "an agent of the roster (row 31)"),
    toolset("muse", "muse", "an agent of the roster (row 31)"),
    workspace(
        "rustup",
        When::Always,
        None,
        &[],
        "stable's foreign standard libraries and wasm32 (STOCK EXCEPTION), until Trust ships them: \
         the cells, the web modules and the shipped binary's x86_64 slice (rows 23, 24)",
    ),
    workspace(
        "chrome-headless-shell",
        When::Always,
        None,
        &[],
        "the browser lane tests the web modules in Chromium itself, and the wasm smoke, glue and \
         perf lanes and aterm-verify's glue tests run their drivers in it; pinned by version and \
         sha256 in `aterm_uds::host::chrome`, which the harness's headless sign-ins fetch by too \
         (rows 17, 23)",
    ),
    workspace(
        "firefox",
        When::Always,
        None,
        &[],
        "the browser lane's second engine (row 23)",
    ),
    workspace(
        "wasm-opt",
        When::Always,
        Some(Pin {
            package: "binaryen",
            version: "133",
            exe: "bin/wasm-opt",
            built: false,
            hosts: MACOS_HOSTS,
        }),
        &[],
        "binaryen's optimizer: the site ships the engine only after its -O3 pass, and the perf lane \
         measures what ships; an optimizer is not worth rebuilding (row 21)",
    ),
    workspace(
        "wasm-bindgen",
        When::Record,
        Some(Pin {
            package: "wasm-bindgen",
            version: "0.2.108",
            exe: "bin/wasm-bindgen",
            built: true,
            hosts: &[],
        }),
        &[],
        "the wasm-glue oracle, only to re-record its goldens: no module aterm ships is bound with it \
         (row 20)",
    ),
    workspace(
        "limactl",
        When::Linux,
        Some(Pin {
            package: "lima",
            version: "2.2.1",
            exe: "bin/limactl",
            built: false,
            hosts: MACOS_HOSTS,
        }),
        &["/opt/homebrew/bin/limactl", "/usr/local/bin/limactl"],
        "the Linux lane's VM (vz backend), for the Linux-only tests and the fish floor (row 25)",
    ),
    missing(
        workspace(
            "ubuntu-root",
            When::Linux,
            Some(Pin {
                package: "ubuntu-noble-root",
                version: "20260926",
                exe: "noble-server-cloudimg-arm64.img",
                built: false,
                hosts: &["macos-aarch64"],
            }),
            &[],
            "not a program: Ubuntu 24.04's root file system, a whole raw ext4 disk the lane's own VM \
             clones its disks from; held by aterm-vz's pins (docs/DESIGN-vz-guest-2026-10-23.md §3, \
             row 25)",
        ),
        "Ubuntu 24.04's pinned root image",
    ),
    missing(
        workspace(
            "ubuntu-kernel",
            When::Linux,
            Some(Pin {
                package: "ubuntu-noble-kernel",
                version: "20260926",
                exe: "ubuntu-24.04-server-cloudimg-arm64-vmlinuz-generic",
                built: false,
                hosts: &["macos-aarch64"],
            }),
            &[],
            "not a program: the kernel and initrd of the same release, beside each other, which the \
             lane's own VM boots from the host; held by aterm-vz's pins \
             (docs/DESIGN-vz-guest-2026-10-23.md §3, row 25)",
        ),
        "Ubuntu 24.04's pinned kernel",
    ),
    unprovided(
        "zstd",
        &["/opt/homebrew/bin/zstd", "/usr/local/bin/zstd"],
        "it is the outside reference the tests judge aterm-zstd's encoder by, so aterm builds no \
         copy; nothing packs or ships with it (row 18)",
        "the oracle aterm-zstd's encoder is held to: its -t and -d of the encoder's frames, and \
         its -19 sizes, which level 19 stays within 3% of (tests/encode_cli.rs); and the one \
         producer test that decodes a pack with it; no lane or publish host runs it (row 18)",
    ),
    unprovided(
        "gitleaks",
        &[],
        "it is the publication engine's own dependency, put where `pub stage` finds it (row 19)",
        "the publication engine's own guard in `pub stage` and `ship check --export`; aterm never \
         runs it (row 19)",
    ),
    release_host(
        "pub",
        &[],
        "the publication engine (`~/publication`): `ship check --export` and `ship release` (row 33)",
    ),
    missing(
        guest(
            "python3",
            &["/usr/bin/python3"],
            "the Linux installer's embedded verifier, until a release pins the Rust one: the \
             install_sh tests that run it need 3.11+ (tomllib, hashlib.file_digest), so they run \
             in the Linux guest (row 14)",
        ),
        "Python 3.11+",
    ),
    missing(
        guest(
            "openssl3",
            &["/usr/bin/openssl"],
            "the Linux installer's embedded verifier checks Ed25519 with `pkeyutl -rawin`, which \
             macOS's LibreSSL lacks, so its tests run in the Linux guest (row 14)",
        ),
        "OpenSSL 3",
    ),
    guest(
        "xdg-desktop-portal",
        &[
            "/usr/libexec/xdg-desktop-portal",
            "/usr/libexec/xdg-desktop-portal-gtk",
        ],
        "the Settings portal aterm's motion read asks, in the Linux guest's accessibility lane",
    ),
    hardware(
        "gpu",
        "a GPU wgpu can open",
        "the gpu-oracle lane renders on a real adapter",
    ),
    hardware(
        "desktop",
        "a free desktop",
        "the appkit and measure checks open windows, which `--no-windows` turns into named skips",
    ),
    hardware(
        "rosetta",
        "Apple silicon with Rosetta",
        "the Linux lane runs its x86_64 half translated",
    ),
    hardware(
        "virtualization",
        "Apple silicon with Virtualization.framework",
        "the Linux lane's own VM runs on Virtualization.framework through aterm-vz's helper, in a \
         console session, since the framework's VM process opens a WindowServer connection at \
         each boot; the lane reads `kern.hv_support` by sysctl, with no framework loaded",
    ),
];
