// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! This test binary's Git environment: none of the machine it runs on. Every `git` the binary
//! runs, the cutter's own included (`ledger::GitCli`, `ledger::object_git`, a fixture's), and every
//! process it starts, reads what a test sets up, not a host's `insteadOf` rewrites, commit
//! signing, hooks path, ignore rules or attributes, which would change what its engine fixtures
//! see. [`hermetic_process`] runs when the binary loads, before `main` and so before any thread.

#[cfg(any(target_vendor = "apple", target_os = "linux"))]
use crate::ledger::testing::CHOSEN_GIT_ENV;

/// The global configuration [`hermetic_process`] names (`GIT_CONFIG_GLOBAL`, which stands for
/// both `~/.gitconfig` and `$XDG_CONFIG_HOME/git/config`): no ignore or attributes file, where
/// Git's defaults are `$XDG_CONFIG_HOME/git/ignore` and `…/git/attributes` (or
/// `~/.config/git/…`). It is global scope, so every repository or command setting a test makes
/// still outranks it.
#[cfg(any(target_vendor = "apple", target_os = "linux"))]
pub(crate) const HERMETIC_GLOBAL_CONFIG: &str =
    "[core]\n\texcludesFile = /dev/null\n\tattributesFile = /dev/null\n";

/// Make this process, and every process it starts, read no Git configuration of the machine it
/// runs on: every inherited `GIT_*` variable but `GIT_TRACE*` is removed (a `GIT_DIR` a hook
/// exported, a shell's `GIT_CONFIG_GLOBAL` or `GIT_EDITOR`), no system configuration or
/// attributes are read (`GIT_CONFIG_NOSYSTEM`, `GIT_ATTR_NOSYSTEM`), and the global configuration
/// is [`HERMETIC_GLOBAL_CONFIG`], written once under the temporary directory. A binary started
/// with [`CHOSEN_GIT_ENV`] keeps the Git environment its parent test chose.
///
/// # Safety
/// No other thread may be running: it sets environment variables.
///
/// # Panics
/// The global configuration cannot be written under the temporary directory.
#[cfg(any(target_vendor = "apple", target_os = "linux"))]
unsafe fn hermetic_process() {
    if std::env::var_os(CHOSEN_GIT_ENV).is_some() {
        return;
    }
    let global = hermetic_global_config()
        .unwrap_or_else(|why| panic!("this test binary's hermetic git configuration: {why}"));
    for (key, _) in std::env::vars_os() {
        let name = key.as_encoded_bytes();
        if name.starts_with(b"GIT_") && !name.starts_with(b"GIT_TRACE") {
            // SAFETY: the caller runs before any other thread exists.
            unsafe { std::env::remove_var(&key) };
        }
    }
    // SAFETY: the caller runs before any other thread exists.
    unsafe {
        std::env::set_var("GIT_CONFIG_GLOBAL", global);
        std::env::set_var("GIT_CONFIG_NOSYSTEM", "1");
        std::env::set_var("GIT_ATTR_NOSYSTEM", "1");
    }
}

/// [`HERMETIC_GLOBAL_CONFIG`]'s file under the temporary directory, written (by a rename, so a
/// reader never sees part of it) where it is missing or says anything else.
#[cfg(any(target_vendor = "apple", target_os = "linux"))]
fn hermetic_global_config() -> Result<std::path::PathBuf, String> {
    let dir = std::env::temp_dir().join("aterm-fixture-git.noindex");
    let path = dir.join("global.gitconfig");
    if std::fs::read(&path).is_ok_and(|text| text == HERMETIC_GLOBAL_CONFIG.as_bytes()) {
        return Ok(path);
    }
    std::fs::create_dir_all(&dir).map_err(|e| format!("create {}: {e}", dir.display()))?;
    let part = dir.join(format!("global.gitconfig.{}", std::process::id()));
    std::fs::write(&part, HERMETIC_GLOBAL_CONFIG)
        .and_then(|()| std::fs::rename(&part, &path))
        .map_err(|e| format!("write {}: {e}", path.display()))?;
    Ok(path)
}

/// [`hermetic_process`] when this binary loads, from its initializer list.
#[cfg(any(target_vendor = "apple", target_os = "linux"))]
extern "C" fn hermetic_at_load() {
    // SAFETY: a load-time initializer runs before `main`, so before any other thread.
    unsafe { hermetic_process() }
}

#[cfg(any(target_vendor = "apple", target_os = "linux"))]
#[used]
#[cfg_attr(
    target_vendor = "apple",
    unsafe(link_section = "__DATA,__mod_init_func,mod_init_funcs")
)]
#[cfg_attr(target_os = "linux", unsafe(link_section = ".init_array"))]
static HERMETIC_AT_LOAD: extern "C" fn() = hermetic_at_load;
