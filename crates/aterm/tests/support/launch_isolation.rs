// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Private host state for launch tests ([`CONFIG_OFF`], [`NO_REROUTE`], [`control_sock`]).

#![allow(
    clippy::disallowed_types,
    reason = "test code: a stand-in child may inherit descriptors"
)]

use std::path::Path;
use std::process::Command;

/// The flag that starts a window or session with the upstream Rust names restored.
#[allow(dead_code)]
pub const NO_REROUTE: &str = "--no-reroute";

/// The window flag that binds its control socket at `<root>/run/aterm/aterm.sock`.
#[allow(dead_code)]
pub fn control_sock(root: &Path) -> [std::ffi::OsString; 2] {
    [
        "--control-sock".into(),
        root.join("run/aterm/aterm.sock").into_os_string(),
    ]
}

const MAX_SOCK_PATH: usize = 100;

/// Whether a unix socket at `sock` fits `sun_path` as a client dials it.
#[allow(dead_code)]
pub fn sock_fits(sock: &Path) -> bool {
    let mut base = sock;
    let mut rest = Vec::new();
    let resolved = loop {
        if let Ok(canonical) = std::fs::canonicalize(base) {
            break canonical;
        }
        let (Some(parent), Some(name)) = (base.parent(), base.file_name()) else {
            break base.to_path_buf();
        };
        rest.push(name);
        base = parent;
    };
    let dialled = rest.iter().rev().fold(resolved, |p, name| p.join(name));
    dialled.as_os_str().len() < MAX_SOCK_PATH
}

/// The development seam naming the system an instance hears (`aterm-gui`'s
/// `memory_pressure::SYSTEM_SEAM`): [`apply`] sets it to `none`, so the instance hears only
/// what its test drives with the `pressure` verb and a loaded host never sheds the scrollback
/// or history ring a test waits on; `normal`, `warn` or `critical` stands in for a system held
/// at that level.
pub const SYSTEM_PRESSURE: &str = "ATERM_DEBUG_SYSTEM_PRESSURE";

/// Config disabling automatic update, package, and agent supervisor lanes ([`HARNESS_OFF`]).
/// Its `[packages] enabled = false` is also atpkg's no to unattended installs
/// (`atpkg::adoption::unattended_install_off`), so a worker spawned in a world built on it runs the
/// test's stand-in and never installs its agent CLI from the network.
pub const CONFIG_OFF: &str = "[update]\nenabled = false\nauto_apply = false\n\
                              [packages]\nenabled = false\n\
                              [machine]\nuniversal_control = \"leave\"\n\
                              [harness]\nenabled = false\n";

/// [`CONFIG_OFF`]'s last table.
#[allow(dead_code)]
pub const HARNESS_OFF: &str = "[harness]\nenabled = false\n";

/// Arm `cmd`, a `--headless` launch, with a lifeline (`aterm_uds::lifeline`).
/// # Panics
/// The lifeline's FIFO could not be created in `root`.
#[cfg(unix)]
#[allow(dead_code)]
pub fn lifeline(cmd: &mut Command, root: &Path) -> aterm_uds::lifeline::Lifeline {
    aterm_uds::lifeline::Lifeline::arm(cmd, root)
        .unwrap_or_else(|e| panic!("arm the instance's lifeline in {}: {e}", root.display()))
}

/// Whether the control socket at `sock` is listening.
#[cfg(unix)]
#[allow(dead_code)]
pub fn control_listening(sock: impl AsRef<Path>) -> bool {
    std::os::unix::net::UnixStream::connect(sock).is_ok()
}

/// A fresh private root for one launch ([`prepare`], [`control_sock`]), removed on drop.
/// # Panics
/// The root could not be created or prepared: a broken fixture.
#[allow(dead_code)]
pub fn scratch_root(label: &str) -> aterm_tempfile::TempDir {
    #[cfg(unix)]
    let base = std::path::PathBuf::from("/tmp");
    #[cfg(not(unix))]
    let base = std::env::temp_dir();
    let root = aterm_tempfile::Builder::new()
        .prefix(&format!("{label}-"))
        .tempdir_in(base)
        .expect("create a private root");
    prepare(root.path()).expect("prepare a private root");
    root
}

/// Pin `path` in the login shell profiles under `root`'s scratch home.
/// # Errors
/// A profile could not be read or written.
#[cfg(unix)]
#[allow(dead_code)]
pub fn pin_login_path(root: &Path, path: &std::ffi::OsStr) -> std::io::Result<()> {
    use std::os::unix::ffi::OsStrExt as _;
    let home = root.join("home");
    let mut pin = b"# aterm live test: the PATH this instance was booted with, said again after\n\
                    # the system profile's (support/launch_isolation.rs, pin_login_path).\n\
                    PATH='"
        .to_vec();
    for &byte in path.as_bytes() {
        if byte == b'\'' {
            pin.extend_from_slice(b"'\\''");
        } else {
            pin.push(byte);
        }
    }
    pin.extend_from_slice(b"'\nexport PATH\n");
    for (name, always) in [
        (".zprofile", true),
        (".profile", true),
        (".bash_profile", false),
        (".bash_login", false),
    ] {
        let file = home.join(name);
        let own = match std::fs::read(&file) {
            Ok(text) => text,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound && always => Vec::new(),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
            Err(e) => return Err(e),
        };
        std::fs::write(&file, [pin.as_slice(), own.as_slice()].concat())?;
    }
    Ok(())
}

pub fn prepare(root: &Path) -> std::io::Result<()> {
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
        "tmp",
        "local",
        "roaming",
    ] {
        let path = root.join(relative);
        std::fs::create_dir_all(&path)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))?;
        }
    }
    std::fs::write(
        root.join("cfg/aterm/aterm.toml"),
        format!("agents_auto_prime = false\n{CONFIG_OFF}"),
    )
}

#[path = "aterm_under_test.rs"]
mod aterm_under_test;
pub use aterm_under_test::aterm_executable;

pub fn apply(cmd: &mut Command, root: &Path) {
    {
        use aterm_uds::spawnfd::InheritNothing as _;
        cmd.inherit_nothing();
    }
    let aterm = aterm_executable();
    if Path::new(cmd.get_program()) == aterm {
        aterm_spec::wait::first_exec(&aterm, &["--help"]);
    }
    let names: std::collections::BTreeSet<_> = std::env::vars_os()
        .map(|(name, _)| name)
        .chain(cmd.get_envs().map(|(name, _)| name.to_owned()))
        .collect();
    for name in names {
        let text = name.to_string_lossy();
        let stripped = text.starts_with("ATERM_")
            || text.starts_with("ATPKG_")
            || text.starts_with("__ATERM_");
        if stripped && text != aterm_uds::process::VERIFY_RUN_VAR {
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
        .env("TMPDIR", root.join("tmp"))
        .env("ATERM_LOG", "off")
        .env(SYSTEM_PRESSURE, "none");
    #[cfg(unix)]
    cmd.env("SHELL", "/bin/sh");
    #[cfg(windows)]
    cmd.env("USERPROFILE", root.join("home"))
        .env("LOCALAPPDATA", root.join("local"))
        .env("APPDATA", root.join("roaming"))
        .env("TEMP", root.join("tmp"))
        .env("TMP", root.join("tmp"));
}
