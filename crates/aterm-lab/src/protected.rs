// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE list of the owner's data the jail denies, neither read nor written.
//!
//! One list: [`crate::jail::render`] makes a rule of each entry, the isolation
//! probes walk the same entries (also against a replica home), and the lab's
//! state directory is kept off it. A secret store that is not here is readable
//! (the crate docs: what the jail does not do); add an entry to close one.

/// How a rule matches its path, in the sandbox profile language.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    /// The path and the whole tree under it.
    Subpath,
    /// The path and anything whose name begins with it (`~/.claude.json.bak`).
    Prefix,
    /// The path itself.
    Literal,
}

impl Kind {
    /// The profile language's word for it.
    #[must_use]
    pub fn word(self) -> &'static str {
        match self {
            Kind::Subpath => "subpath",
            Kind::Prefix => "prefix",
            Kind::Literal => "literal",
        }
    }
}

/// What an entry is relative to.
#[derive(Clone, Copy, Debug)]
enum Base {
    Home,
    CcSocks,
}

/// Where the owner's Claude Code sessions keep their messaging sockets.
pub const CCSOCKS: &str = "/private/tmp/cc-socks";

/// The Mach services of the keychain daemons, which Security.framework and
/// `/usr/bin/security` talk to (and their iCloud, CoreAuthentication,
/// SecureBackup and token-daemon kin).
pub const MACH_DENY: &[&str] = &[
    "com.apple.SecurityServer",
    "com.apple.secd",
    "com.apple.security.agentd",
    "com.apple.securityd.xpc",
    "com.apple.security.XPCKeychainSandboxCheck",
    "com.apple.security.cloudkeychainproxy3",
    "com.apple.CoreAuthentication.agent",
    "com.apple.CoreAuthentication.daemon",
    "com.apple.SecureBackupDaemon",
    "com.apple.ctkd.token-client",
];

/// The Launch Services doors that OPEN something on the person's desktop: a
/// URL handed to NSWorkspace or LSOpen (Codex's sign-in opens the browser that
/// way; no `BROWSER` variable or `PATH` shim can catch it) goes through these
/// daemons, which launch the browser OUTSIDE the jail. The Launch Services
/// database stays readable.
pub const MACH_DENY_OPEN: &[&str] = &[
    "com.apple.lsd.open",
    "com.apple.lsd.openurl",
    "com.apple.coreservices.launchservicesd",
    "com.apple.coreservices.uiagent",
];

/// The directories of a lab's scratch that the lab, OUTSIDE the jail, writes
/// and reads through (`config.toml`, `auth.json`, the rollouts,
/// `settings.json`, `aterm.toml`, the stub's modes, the decoys, Gemini CLI's
/// and agy's homes and seeds, agy's log, their `TMPDIR`, the sleep clock's
/// records: [`crate::clock`]). `up` makes
/// each, and the jail may neither remove nor move one, nor the scratch itself,
/// nor the project dir: a link a jailed process put in the place of one would
/// lead the lab into the owner's data (a link needs no access to its target).
/// A path the lab writes below them goes through [`crate::sys::put`], which
/// follows no link on its whole path.
pub const SCRATCH_DIRS: &[&str] = &[
    "home",
    "home/.claude",
    "home/.codex",
    "home/.gemini",
    "cfg",
    "cfg/aterm",
    "state",
    "modes",
    "dump",
    "codex-home",
    "control-home",
    "control-home/.codex",
    "tmp",
    "gemini-home",
    "gemini-home/.gemini",
    "agy-home",
    "agy-home/.gemini",
    "agy-home/.gemini/antigravity-cli",
    "clock",
];

const LIST: &[(Kind, Base, &str)] = &[
    (Kind::Subpath, Base::Home, ".claude"),
    (Kind::Prefix, Base::Home, ".claude.json"),
    (Kind::Subpath, Base::Home, ".ssh"),
    (Kind::Subpath, Base::Home, ".aws"),
    (Kind::Subpath, Base::Home, ".gnupg"),
    (Kind::Subpath, Base::Home, ".config"),
    (Kind::Subpath, Base::Home, ".docker"),
    (Kind::Subpath, Base::Home, ".kube"),
    (Kind::Subpath, Base::Home, ".codex"),
    (Kind::Subpath, Base::Home, ".gemini"),
    (Kind::Subpath, Base::Home, ".cursor"),
    // aterm's machine key and a release credential (`machine.key`, `release-credentials.toml`)
    (Kind::Subpath, Base::Home, ".aterm"),
    (Kind::Subpath, Base::Home, ".secrets"),
    // Muse Code's sessions (its config is under `.config`)
    (Kind::Subpath, Base::Home, ".local/share/muse"),
    // the Antigravity app's extensions and server (`.antigravity`, `.antigravity-server`, …): agy's kin, whose
    // binary links `os/user`, which names the account's home whatever HOME says
    (Kind::Prefix, Base::Home, ".antigravity"),
    (Kind::Literal, Base::Home, ".netrc"),
    (Kind::Literal, Base::Home, ".npmrc"),
    (Kind::Literal, Base::Home, ".git-credentials"),
    (Kind::Prefix, Base::Home, ".cargo/credentials"),
    (Kind::Subpath, Base::Home, "Library/Keychains"),
    (Kind::Subpath, Base::Home, "Library/Cookies"),
    (Kind::Subpath, Base::Home, "Library/Safari"),
    (
        Kind::Subpath,
        Base::Home,
        "Library/Application Support/Google",
    ),
    (
        Kind::Subpath,
        Base::Home,
        "Library/Application Support/Firefox",
    ),
    (
        Kind::Subpath,
        Base::Home,
        "Library/Application Support/BraveSoftware",
    ),
    (Kind::Subpath, Base::Home, "Library/Application Support/Arc"),
    (
        Kind::Subpath,
        Base::Home,
        "Library/Application Support/Microsoft Edge",
    ),
    (
        Kind::Subpath,
        Base::Home,
        "Library/Application Support/aterm",
    ),
    // Muse Code's session-name authority (`session-name-authority/session-names.db`) and its computer-use state:
    // 1.4.10 opened the account's own, whatever HOME and the XDG variables said
    (
        Kind::Subpath,
        Base::Home,
        "Library/Application Support/Muse",
    ),
    // the Antigravity app's data (`Antigravity`, `Antigravity Nightly`, …: its sign-in among it)
    (
        Kind::Prefix,
        Base::Home,
        "Library/Application Support/Antigravity",
    ),
    (Kind::Subpath, Base::CcSocks, ""),
];

/// The list for a home and a socket directory: `(kind, path)` per entry.
#[must_use]
pub fn protected_paths(home: &str, ccsocks: &str) -> Vec<(Kind, String)> {
    LIST.iter()
        .map(|&(kind, base, rel)| {
            let path = match base {
                Base::Home => format!("{home}/{rel}"),
                Base::CcSocks => ccsocks.to_string(),
            };
            (kind, path)
        })
        .collect()
}

/// Whether `path` is, or lies below, an entry of the list.
#[must_use]
pub fn inside_protected(path: &str, home: &str, ccsocks: &str) -> bool {
    let p = format!("{path}/");
    protected_paths(home, ccsocks)
        .iter()
        .any(|(kind, q)| match kind {
            Kind::Prefix => p.starts_with(q.as_str()),
            Kind::Subpath | Kind::Literal => p.starts_with(&format!("{q}/")),
        })
}

/// `protected HOME CCSOCKS`: the list as `<kind> TAB <path>` lines.
#[must_use]
pub fn render_lines(home: &str, ccsocks: &str) -> String {
    protected_paths(home, ccsocks)
        .iter()
        .map(|(k, p)| format!("{}\t{p}\n", k.word()))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn inside_means_the_path_or_below_it_and_prefix_means_the_name_too() {
        let h = "/Users//o";
        assert!(inside_protected("/Users//o/.ssh", h, CCSOCKS));
        assert!(inside_protected("/Users//o/.ssh/id_rsa", h, CCSOCKS));
        assert!(!inside_protected("/Users//o/.sshx", h, CCSOCKS));
        assert!(inside_protected("/Users//o/.claude.json.bak", h, CCSOCKS));
        assert!(inside_protected("/private/tmp/cc-socks/1.sock", h, CCSOCKS));
        assert!(!inside_protected(
            "/Users//o/Developer/aterm/target/real-claude-lab",
            h,
            CCSOCKS
        ));
    }

    /// Muse Code's and the Antigravity app's own data are on the list: Muse
    /// Code 1.4.10 opened the account's `session-names.db` with HOME pointed
    /// at the scratch. NEGATIVE CONTROLS: a sibling whose name only begins
    /// like a subpath entry, and the rest of `.local/share`, are not.
    #[test]
    fn muse_code_s_and_antigravity_s_own_data_are_on_the_list() {
        let h = "/Users//o";
        for p in [
            "/Users//o/Library/Application Support/Muse/session-name-authority/session-names.db",
            "/Users//o/Library/Application Support/Muse",
            "/Users//o/.local/share/muse/sessions/2026/10/17/x/session.jsonl",
            "/Users//o/Library/Application Support/Antigravity/User/globalStorage/state.vscdb",
            "/Users//o/Library/Application Support/Antigravity Nightly",
            "/Users//o/.antigravity/extensions",
            "/Users//o/.antigravity-server",
        ] {
            assert!(inside_protected(p, h, CCSOCKS), "{p}");
        }
        for p in [
            "/Users//o/Library/Application Support/Museum",
            "/Users//o/.local/share/muse-other",
            "/Users//o/.local/share/fish",
        ] {
            assert!(!inside_protected(p, h, CCSOCKS), "{p}");
        }
        let lines = render_lines(h, CCSOCKS);
        for want in [
            "subpath\t/Users//o/Library/Application Support/Muse\n",
            "subpath\t/Users//o/.local/share/muse\n",
            "prefix\t/Users//o/Library/Application Support/Antigravity\n",
            "prefix\t/Users//o/.antigravity\n",
        ] {
            assert!(lines.contains(want), "{want}");
        }
    }

    /// aterm's own secrets are on the list: `~/.aterm` holds the machine key
    /// (`machine.key`) and a release credential (`release-credentials.toml`),
    /// and `~/.secrets` is a common home for tokens. NEGATIVE CONTROLS:
    /// siblings whose names only begin like them, and aterm's install-time
    /// scratch beside the home, are not.
    #[test]
    fn aterm_s_own_secrets_and_a_secrets_dir_are_on_the_list() {
        let h = "/Users//o";
        for p in [
            "/Users//o/.aterm",
            "/Users//o/.aterm/machine.key",
            "/Users//o/.aterm/release-credentials.toml",
            "/Users//o/.aterm/shell.d/zsh.zsh",
            "/Users//o/.secrets",
            "/Users//o/.secrets/token",
        ] {
            assert!(inside_protected(p, h, CCSOCKS), "{p}");
        }
        for p in [
            "/Users//o/.aterm-update",
            "/Users//o/.aterm-copy-1",
            "/Users//o/.secretsx",
            "/Users//o/Developer/aterm/.aterm-verify",
        ] {
            assert!(!inside_protected(p, h, CCSOCKS), "{p}");
        }
        let lines = render_lines(h, CCSOCKS);
        for want in ["subpath\t/Users//o/.aterm\n", "subpath\t/Users//o/.secrets\n"] {
            assert!(lines.contains(want), "{want}");
        }
    }
}
