// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Private host state for tests that launch the real window or session entry.
//!
//! ISOLATION IS A SCRATCH HOME AND A SCRATCH CONFIG, never an environment veto
//! (2026-09-23). The update system's user-facing environment knobs are gone
//! (`ATPKG_DISABLE`, `ATERM_NO_AUTO_UPDATE`, `ATERM_NO_AUTO_APPLY`, `ATERM_NO_REROUTE`;
//! owner: "NOT ENV VARS those are for development"), so a launch is kept off the
//! machine the way a person keeps it off theirs: `$HOME` is private (so the package
//! prefix, its reroute stubs and the `Updates/` ledger all live under it), and
//! `$XDG_CONFIG_HOME/aterm/aterm.toml` switches every automatic lane off —
//! [`CONFIG_OFF`]. The reroute's escape is the `--no-reroute` FLAG ([`NO_REROUTE`]),
//! which a caller adds to its window/session argv (before any `-e`/`--` payload).

use std::path::Path;
use std::process::Command;

/// The flag that starts a window or session with the upstream Rust names restored
/// — nothing laid under the scratch prefix, no launchd lane. A caller that does not
/// study the reroute adds it; the environment can no longer say it.
#[allow(dead_code)]
pub const NO_REROUTE: &str = "--no-reroute";

/// The automatic lanes, switched off the way Settings switches them off: `[update]
/// enabled = false` (Check for updates automatically) with `auto_apply = false`,
/// `[packages] enabled = false` (Automatic updates), and the `[machine]` settings left
/// alone.
pub const CONFIG_OFF: &str = "[update]\nenabled = false\nauto_apply = false\n\
                              [packages]\nenabled = false\n\
                              [machine]\nspotlight_noindex = false\nuniversal_control = \"leave\"\n";

/// Whether the control socket at `sock` is LISTENING: a connect the kernel
/// takes into the backlog, dropped at once (the server's own
/// `control_auth::socket_is_live` probe). The socket FILE is not that signal:
/// `bind(2)` creates it before `listen(2)`, and a client that dials in between
/// is refused (`ECONNREFUSED`). Boots that read the file as readiness failed
/// their first `aterm ctl` call, "Connection refused", about once in ten runs
/// of the supervise suite at a load average near 40 (2026-09-24).
#[allow(dead_code)]
pub fn control_listening(sock: impl AsRef<Path>) -> bool {
    std::os::unix::net::UnixStream::connect(sock).is_ok()
}

pub fn prepare(root: &Path) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt as _;

    for relative in [
        "",
        "home",
        "cfg",
        "cfg/aterm",
        "run",
        "run/aterm",
        "cache",
        "data",
        "state",
    ] {
        let path = root.join(relative);
        std::fs::create_dir_all(&path)?;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))?;
    }
    std::fs::write(
        root.join("cfg/aterm/aterm.toml"),
        format!("agents_auto_prime = false\n{CONFIG_OFF}"),
    )
}

pub fn apply(cmd: &mut Command, root: &Path) {
    // Remove both inherited context and explicit command overrides before
    // assigning this fixture's authority. Callers may then add the one setting
    // they are testing (for example the containment mode or @self session id).
    let names: std::collections::BTreeSet<_> = std::env::vars_os()
        .map(|(name, _)| name)
        .chain(cmd.get_envs().map(|(name, _)| name.to_owned()))
        .collect();
    for name in names {
        let text = name.to_string_lossy();
        // `__ATERM_` covers the internal protocol markers (the reroute passthrough), whose
        // leading underscores say "not a user knob" and would otherwise slip the filter.
        if text.starts_with("ATERM_") || text.starts_with("ATPKG_") || text.starts_with("__ATERM_")
        {
            cmd.env_remove(name);
        }
    }
    cmd.env_remove("TERM_PROGRAM")
        .env_remove("BASH_ENV")
        .env_remove("ENV")
        .env_remove("ZDOTDIR")
        .env("HOME", root.join("home"))
        .env("XDG_CONFIG_HOME", root.join("cfg"))
        .env("XDG_RUNTIME_DIR", root.join("run"))
        .env("XDG_CACHE_HOME", root.join("cache"))
        .env("XDG_DATA_HOME", root.join("data"))
        .env("XDG_STATE_HOME", root.join("state"))
        .env("ATERM_CONTROL_SOCK", root.join("run/aterm/aterm.sock"))
        .env("ATERM_LOG", "off")
        .env("SHELL", "/bin/sh");
}
