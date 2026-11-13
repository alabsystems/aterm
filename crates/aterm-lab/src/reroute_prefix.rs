// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A scratch atpkg prefix that routes an agent the way the owner's does: the `reroute/` stub,
//! the `agents/` twin, then the store build (`docs/DESIGN-card-engine-2026-10-19.md` §8, launch
//! shape 2).
//!
//! The build is an APFS clone of one of the owner's store builds, at the same
//! `store/<program>/<build>` path under a new scratch HOME's default prefix, with its sidecar
//! records. This checkout's atpkg lays the rest as the owner's aterm does: the `current` flip,
//! the `bin/` shim, the `agents/` twin and the reroute stubs. The owner's prefix is only ever read.

use std::path::{Path, PathBuf};

use aterm_uds::spawnfd::Stdio;

/// What [`lay`] laid.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Laid {
    /// The scratch prefix (`<home>/Library/Application Support/aterm/pkg` on macOS).
    pub prefix: PathBuf,
    /// The program (`claude`).
    pub program: String,
    /// `store/<program>/<build>`.
    pub build_dir: PathBuf,
    /// The clone of the owner's executable in it.
    pub exe: PathBuf,
    /// `agents/<program>`: the twin the reroute stub execs inside aterm.
    pub twin: PathBuf,
    /// `reroute/<program>`: the stub a session's `PATH` finds first.
    pub stub: PathBuf,
    /// What the clone printed for `--version` (its first line), run once
    /// from its file before anything else execs it.
    pub version: String,
}

/// The prefix a store executable `exe` (`<prefix>/store/<program>/<build>/bin/<name>`)
/// lives in, with its program and build, or `None` when it is not one.
#[must_use]
pub fn store_of(exe: &Path) -> Option<(PathBuf, String, u64)> {
    let build_dir = exe.parent()?.parent()?;
    let prefix = build_dir.parent()?.parent()?.parent()?;
    let (program, build) = atpkg::ops::store_build_of(prefix, exe)?;
    (exe.parent()?.file_name()? == "bin" && build_dir.file_name()?.to_str()? == build.to_string())
        .then(|| (prefix.to_path_buf(), program, build))
}

/// Lay a prefix under the new HOME `home` (its default prefix) whose `store/<program>/<build>` is
/// a clone of the owner's build holding `source` (a store executable, [`store_of`]), activated,
/// with its `bin/` shim, `agents/` twin and reroute stubs, then run the clone once (`--version`)
/// so the first measured launch does not pay macOS's first-exec assessment. `hold`, a directory
/// whose presence keeps the caller's scratch, is made before cloning, stamped
/// ([`aterm_uds::deadline::stamp_exec`]) just before that exec, and removed once the clone printed
/// anything; when it printed nothing, or the run could not be started or read, `hold` stays, as
/// macOS may still be assessing the clone. A caller whose `home` nothing removes passes none.
/// # Errors
/// `source` is no store build's executable; the scratch prefix is the owner's or already holds a
/// store; a clone, copy or lay failed; the twin does not name the clone or the stub is not laid;
/// `hold` could not be made; the clone could not be run or never printed a version.
pub fn lay(home: &Path, source: &Path, hold: Option<&Path>) -> Result<Laid, String> {
    let source = std::fs::canonicalize(source)
        .map_err(|e| format!("cannot resolve {}: {e}", source.display()))?;
    let (owner, program, build) = store_of(&source).ok_or_else(|| {
        format!(
            "{} is no store build's executable (<prefix>/store/<program>/<build>/bin/<name>)",
            source.display()
        )
    })?;
    let home = std::fs::canonicalize(home)
        .map_err(|e| format!("cannot resolve the home {}: {e}", home.display()))?;
    let layout = atpkg::Layout {
        prefix: atpkg::store::default_prefix(&home),
    };
    if layout.prefix.starts_with(&owner) || owner.starts_with(&layout.prefix) {
        return Err(format!(
            "the scratch prefix {} overlaps the owner's {}: never re-laid",
            layout.prefix.display(),
            owner.display()
        ));
    }
    if layout.prefix.join("store").exists() {
        return Err(format!(
            "{} already holds a store: lay into a new home",
            layout.prefix.display()
        ));
    }
    let from = source
        .parent()
        .and_then(Path::parent)
        .ok_or("the source has no build directory")?;
    let build_dir = layout.build_dir(&program, build);
    if let Some(hold) = hold {
        std::fs::create_dir_all(hold)
            .map_err(|e| format!("cannot hold {}: {e}", hold.display()))?;
    }
    clone_tree(from, &build_dir).map_err(|e| {
        format!(
            "cannot clone {} into {}: {e}",
            from.display(),
            build_dir.display()
        )
    })?;
    for sidecar in ["ready", "shim-env", "vendor"] {
        let name = |dir: &Path| dir.with_file_name(format!("{build}.{sidecar}"));
        let (src, dest) = (name(from), name(&build_dir));
        if src.is_file() {
            crate::util::copy_regular_file(&src, &dest)
                .map_err(|e| format!("cannot copy {}: {e}", src.display()))?;
        }
    }
    let exe = build_dir.join(source.strip_prefix(from).map_err(|e| e.to_string())?);
    atpkg::activate_build(&layout, &build_dir)
        .map_err(|e| format!("cannot activate {}: {e}", build_dir.display()))?;
    let env = atpkg::shim_env::ShimEnv::from_lines(
        &crate::util::read_regular_text(&build_dir.with_file_name(format!("{build}.shim-env")))
            .unwrap_or_default(),
    );
    let tool = exe
        .file_name()
        .and_then(|n| n.to_str())
        .ok_or("the executable has no name")?
        .to_string();
    let refused = atpkg::activate::install_shims_env(
        &layout,
        &build_dir,
        std::slice::from_ref(&tool),
        atpkg::Aliases::Off,
        &env,
    )
    .map_err(|e| format!("cannot lay the shims: {e}"))?;
    if !refused.is_empty() {
        return Err(format!("atpkg refused the names {refused:?}"));
    }
    atpkg::reroute::lay(&layout).map_err(|e| format!("cannot lay the reroute stubs: {e}"))?;
    let twin_names = atpkg::activate::twin_target(&layout, &program);
    if twin_names.as_deref() != Some(exe.as_path()) {
        return Err(format!(
            "the twin names {twin_names:?}, not the clone {}",
            exe.display()
        ));
    }
    if atpkg::reroute::stub_state(&layout, &program) != atpkg::reroute::StubState::Laid {
        return Err(format!("no reroute stub for {program} was laid"));
    }
    if let Some(hold) = hold {
        std::fs::create_dir_all(hold)
            .map_err(|e| format!("cannot hold {}: {e}", hold.display()))?;
        aterm_uds::deadline::stamp_exec(hold);
    }
    let mut cmd = aterm_uds::spawnfd::command(&exe);
    cmd.arg("--version")
        .env_clear()
        .env("HOME", &home)
        .env("PATH", "/usr/bin:/bin")
        .env("DISABLE_AUTOUPDATER", "1")
        .stdin(Stdio::null());
    let out = aterm_uds::deadline::output_within(&mut cmd, aterm_uds::deadline::FIRST_EXEC_HANG)
        .map_err(|e| format!("cannot run {}: {e}", exe.display()))?;
    if let Some(hold) = hold
        && (!out.stdout.is_empty() || !out.stderr.is_empty())
    {
        let _ = std::fs::remove_dir_all(hold);
    }
    let version = String::from_utf8_lossy(&out.stdout)
        .lines()
        .next()
        .unwrap_or("")
        .trim()
        .to_string();
    if !out.status.success() || version.is_empty() {
        return Err(format!(
            "{} --version printed no version: {}",
            exe.display(),
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    Ok(Laid {
        twin: layout.agents_dir().join(&program),
        stub: atpkg::reroute::stub_path(&layout, &program),
        prefix: layout.prefix,
        program,
        build_dir,
        exe,
        version,
    })
}

/// Clone the tree `from` into the new `to`: directories made, regular files
/// cloned (`fclonefileat`, [`crate::sys::put`]) with their modes, symbolic
/// links as links.
fn clone_tree(from: &Path, to: &Path) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt as _;
    let meta = std::fs::symlink_metadata(from)?;
    let mode = meta.permissions().mode() & 0o7777;
    if meta.file_type().is_symlink() {
        std::os::unix::fs::symlink(std::fs::read_link(from)?, to)
    } else if meta.is_dir() {
        std::fs::create_dir_all(to)?;
        for entry in std::fs::read_dir(from)? {
            let entry = entry?;
            clone_tree(&entry.path(), &to.join(entry.file_name()))?;
        }
        Ok(())
    } else {
        crate::sys::put(to, mode, &crate::sys::PutSrc::File(from))
    }
}

#[cfg(test)]
#[path = "reroute_prefix_tests.rs"]
mod tests;
