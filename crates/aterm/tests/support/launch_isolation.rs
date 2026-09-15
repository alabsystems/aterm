// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Private host state for tests that launch the real window or session entry.

use std::path::Path;
use std::process::Command;

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
        "updates",
    ] {
        let path = root.join(relative);
        std::fs::create_dir_all(&path)?;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))?;
    }
    std::fs::write(
        root.join("cfg/aterm/aterm.toml"),
        "agents_auto_prime = false\n[packages]\nenabled = false\n\
         [machine]\nspotlight_noindex = false\nuniversal_control = \"leave\"\n",
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
        if text.starts_with("ATERM_") || text.starts_with("ATPKG_") {
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
        .env("ATERM_UPDATE_ROOT", root.join("updates"))
        .env("ATERM_CONTROL_SOCK", root.join("run/aterm/aterm.sock"))
        .env("ATERM_NO_REROUTE", "1")
        .env("ATERM_NO_AUTO_UPDATE", "1")
        .env("ATERM_NO_AUTO_APPLY", "1")
        .env("ATPKG_DISABLE", "1")
        .env("ATERM_LOG", "off")
        .env("SHELL", "/bin/sh");
}
