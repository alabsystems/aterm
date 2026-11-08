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

/// The development seam naming the sleep an instance's hosted harness loops read
/// (`aterm-gui`'s `harness_host::NO_HOST_SLEEP`): [`apply`] sets it to `none`, so each loop reads
/// a host that never sleeps, and a Mac with its lid closed, in and out of dark wake every few
/// seconds, puts no wake to settle into what a test's hang detector waits on.
pub const HOST_SLEEP: &str = aterm_types::domain::ENV_DEBUG_HOST_SLEEP;

/// The development seam setting, in milliseconds, how long an instance's spawn waits for its
/// folder check (`aterm-gui`'s `spawn_folder::CHECK_BUDGET_SEAM`): [`apply`] sets it to the hang
/// detector, since a test's folders are local and their check must answer, and a check a loaded
/// host held past the shipped 250 ms read its folder as one that did not answer.
pub const FOLDER_CHECK_WITHIN: &str = "ATERM_DEBUG_FOLDER_CHECK_WITHIN_MS";

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

/// An instance's lifeline FIFO paired with its subprocess admission lease.
#[cfg(unix)]
#[allow(dead_code)]
pub struct Lifeline {
    /// Closed first to cut the instance's lifeline FIFO before releasing the subprocess slot.
    _inner: aterm_uds::lifeline::Lifeline,
    _lease: std::sync::Arc<aterm_spec::subprocess_lane::SubprocessLease>,
}

/// Arm `cmd`, a `--headless` launch, with a lifeline (`aterm_uds::lifeline`) and a subprocess lease.
/// # Panics
/// The lifeline's FIFO could not be created in `root`.
#[cfg(unix)]
#[allow(dead_code)]
pub fn lifeline(cmd: &mut Command, root: &Path) -> Lifeline {
    let lease = aterm_spec::subprocess_lane::acquire().unwrap_or_else(|e| {
        aterm_spec::host::could_not_run(format!("subprocess admission refused ({e})"))
    });
    let inner = aterm_uds::lifeline::Lifeline::arm(cmd, root)
        .unwrap_or_else(|e| panic!("arm the instance's lifeline in {}: {e}", root.display()));
    Lifeline {
        _inner: inner,
        _lease: lease,
    }
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
    let text = format!("agents_auto_prime = false\n{CONFIG_OFF}");
    aterm_uds::atomic_file::write(&root.join("cfg/aterm/aterm.toml"), text.as_bytes(), 0o600)
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
        let reroute = text.starts_with("ATPKG_") || text.starts_with("__ATERM_");
        if aterm_uds::process::stripped_from_child(&name) || reroute {
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
        .env(SYSTEM_PRESSURE, "none")
        .env(HOST_SLEEP, "none")
        .env(
            FOLDER_CHECK_WITHIN,
            aterm_spec::wait::MUST_HAPPEN.as_millis().to_string(),
        );
    #[cfg(unix)]
    cmd.env("SHELL", "/bin/sh");
    #[cfg(windows)]
    cmd.env("USERPROFILE", root.join("home"))
        .env("LOCALAPPDATA", root.join("local"))
        .env("APPDATA", root.join("roaming"))
        .env("TEMP", root.join("tmp"))
        .env("TMP", root.join("tmp"));
}
