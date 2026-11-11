// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Rustup's home and its listings, for [`crate::discovery`] and the STOCK EXCEPTION lanes, and
//! the one stock toolchain the checkout pins for them ([`STOCK_PIN`], held to the checkout's
//! engine gate by [`stock_pin`]). The rustup binary is the `rustup` row of
//! `aterm_uds::host::TOOLS`, found by that row's resolver alone.

use std::ffi::OsStr;
use std::path::{Path, PathBuf};

/// Rustup's home: a non-empty `$RUSTUP_HOME`, else `<home>/.rustup`.
#[must_use]
pub fn rustup_home(env_rustup_home: Option<&OsStr>, home: &Path) -> PathBuf {
    match env_rustup_home {
        Some(v) if !v.is_empty() => PathBuf::from(v),
        _ => home.join(".rustup"),
    }
}

/// [`rustup_home`] when `$HOME` may be unknown (`None`): a non-empty `$RUSTUP_HOME` still answers.
#[must_use]
pub fn rustup_home_with(env_rustup_home: Option<&OsStr>, home: Option<&Path>) -> Option<PathBuf> {
    match home {
        Some(h) => Some(rustup_home(env_rustup_home, h)),
        None => env_rustup_home.filter(|v| !v.is_empty()).map(PathBuf::from),
    }
}

/// Does `listing` (`rustup target list --installed`) name exactly `target`? Line-exact.
#[must_use]
pub fn lists_target(listing: &str, target: &str) -> bool {
    listing.lines().any(|l| l.trim() == target)
}

/// The toolchain names in `rustup toolchain list`, without rustup's ` (active, default)` marker.
#[must_use]
pub fn toolchain_names(listing: &str) -> Vec<String> {
    listing
        .lines()
        .filter_map(|l| l.split_whitespace().next())
        .map(str::to_string)
        .collect()
}

/// Does `listing` (`rustup toolchain list`) hold the toolchain `name` as rustup resolves it on
/// `host`, its default host ([`default_host`])? rustup lists a channel or a release with the host
/// it was installed for after it (`1.97.1-aarch64-apple-darwin`) and a linked toolchain by its
/// bare name (`trust`), and resolves `name` to `name-host` alone: another host's install
/// (`1.97.1-x86_64-apple-darwin` on an arm64 Mac) or a linked toolchain whose name only begins
/// with `name` (`1.97.1-mine`) is another toolchain, whose drivers `rustup which --toolchain
/// name` never names.
#[must_use]
pub fn holds_toolchain(listing: &str, name: &str, host: &str) -> bool {
    toolchain_names(listing).iter().any(|t| {
        t == name
            || t.strip_prefix(name)
                .and_then(|rest| rest.strip_prefix('-'))
                .is_some_and(|rest| rest == host)
    })
}

/// The target triple this program was built for: the host rustup takes when its settings name
/// none, as rustup reads its own machine (an Apple Silicon Mac is `aarch64-apple-darwin`); empty
/// on a platform no lane builds on.
pub const NATIVE_TRIPLE: &str = if cfg!(all(target_os = "macos", target_arch = "aarch64")) {
    "aarch64-apple-darwin"
} else if cfg!(all(target_os = "macos", target_arch = "x86_64")) {
    "x86_64-apple-darwin"
} else if cfg!(all(
    target_os = "linux",
    target_arch = "aarch64",
    target_env = "musl"
)) {
    "aarch64-unknown-linux-musl"
} else if cfg!(all(
    target_os = "linux",
    target_arch = "x86_64",
    target_env = "musl"
)) {
    "x86_64-unknown-linux-musl"
} else if cfg!(all(target_os = "linux", target_arch = "aarch64")) {
    "aarch64-unknown-linux-gnu"
} else if cfg!(all(target_os = "linux", target_arch = "x86_64")) {
    "x86_64-unknown-linux-gnu"
} else if cfg!(all(windows, target_arch = "aarch64")) {
    "aarch64-pc-windows-msvc"
} else if cfg!(all(windows, target_arch = "x86_64")) {
    "x86_64-pc-windows-msvc"
} else {
    ""
};

/// The host rustup resolves a bare channel or release to in `rustup_home`: its settings'
/// `default_host_triple`, else this machine's own ([`NATIVE_TRIPLE`]), as rustup chooses it.
#[must_use]
pub fn default_host(rustup_home: &Path) -> String {
    crate::discovery::read_bounded_text(
        &rustup_home.join("settings.toml"),
        crate::discovery::MAX_ATERM_CONFIG_BYTES,
    )
    .and_then(|settings| settings_host(&settings))
    .unwrap_or_else(|| NATIVE_TRIPLE.to_string())
}

/// [`default_host`] of the rustup home this process's environment names ([`rustup_home_with`]).
#[must_use]
pub fn default_host_from_env() -> String {
    let home = std::env::var_os("HOME").map(PathBuf::from);
    rustup_home_with(std::env::var_os("RUSTUP_HOME").as_deref(), home.as_deref())
        .map_or_else(|| NATIVE_TRIPLE.to_string(), |at| default_host(&at))
}

/// The top-level `default_host_triple = "…"` of rustup's `settings.toml`, if it names one.
fn settings_host(settings: &str) -> Option<String> {
    settings
        .lines()
        .map(str::trim)
        .take_while(|l| !l.starts_with('['))
        .find_map(|l| {
            l.strip_prefix("default_host_triple")?
                .trim_start()
                .strip_prefix('=')?
                .trim()
                .strip_prefix('"')?
                .strip_suffix('"')
                .filter(|host| !host.is_empty())
                .map(str::to_string)
        })
}

pub use crate::stock_pin::STOCK_PIN;

/// The file whose wasm32 clause ([`wasm_clause`]) builds the site's /terminal engine on a stock
/// channel, relative to the checkout's root: the publication engine's gate, which the public
/// snapshot does not carry.
pub const STOCK_PIN_FILE: &str = "publish/config.sh";

/// The wasm32 clause of `publish/config.sh`'s `CHECK_CMD_DEFAULT` and the stock channel it names.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WasmClause {
    pub clause: String,
    pub pin: String,
}

/// Why `publish/config.sh` names no usable wasm32 pin, and the fix.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClauseError {
    pub why: String,
    pub remedy: String,
}

impl ClauseError {
    fn new(why: impl Into<String>, remedy: impl Into<String>) -> Self {
        Self {
            why: why.into(),
            remedy: remedy.into(),
        }
    }
}

impl std::fmt::Display for ClauseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} (fix: {})", self.why, self.remedy)
    }
}

/// Reads the wasm32 clause out of `config` (`CHECK_CMD_DEFAULT="…"`).
/// # Errors
/// No clause; no stock channel; no target-add before it; no `RUSTC` naming the pin's own rustc.
pub fn wasm_clause(config: &str) -> Result<WasmClause, ClauseError> {
    let line = config
        .lines()
        .find_map(|l| l.strip_prefix("CHECK_CMD_DEFAULT=\""))
        .and_then(|l| l.trim_end().strip_suffix('"'))
        .unwrap_or_default();
    let Some(clause) = line
        .split(" && ")
        .find(|c| c.contains("--target wasm32-unknown-unknown"))
        .map(str::trim)
    else {
        return Err(ClauseError::new(
            "publish/config.sh's CHECK_CMD_DEFAULT has no wasm32 clause — nothing before the \
             promote compiles the site's /terminal engine",
            "restore its `cargo check --locked --target wasm32-unknown-unknown -p aterm-wasm` \
             clause (publish/config.sh, THE WASM32 CLAUSE)",
        ));
    };
    let pin = clause
        .split_once("rustup run ")
        .and_then(|(_, rest)| rest.split_once(" cargo "))
        .map(|(pin, _)| pin.to_string())
        .filter(|p| !p.is_empty() && !p.contains(' '));
    let Some(pin) = pin else {
        return Err(ClauseError::new(
            format!(
                "publish/config.sh's wasm32 clause names no stock channel — the wasm32 pin is the \
                 compiler this check and the stage's gate both run: {clause}"
            ),
            "spell the clause `rustup run <channel> cargo check …`",
        ));
    };
    let add = format!("rustup target add --toolchain {pin} wasm32-unknown-unknown");
    if !line.contains(&format!("{add} && ")) {
        return Err(ClauseError::new(
            format!(
                "publish/config.sh's CHECK_CMD_DEFAULT does not add the wasm32 target to {pin} \
                 before its wasm32 clause — `pub stage` on a host without that target fails after \
                 the verified build"
            ),
            format!("put `{add} && ` before the wasm32 clause"),
        ));
    }
    let rustc = format!("RUSTC=$(rustup which --toolchain {pin} rustc) ");
    if !clause.contains(&rustc) {
        return Err(ClauseError::new(
            format!(
                "publish/config.sh's wasm32 clause does not name {pin}'s own rustc — cargo would \
                 take `rustc` from the host PATH (the gate's fresh CARGO_HOME holds no rustup \
                 proxy): {clause}"
            ),
            format!("put `{rustc}` before `rustup run {pin} cargo` in the wasm32 clause"),
        ));
    }
    Ok(WasmClause {
        clause: clause.to_string(),
        pin,
    })
}

/// The stock pin of the checkout at `repo`: [`STOCK_PIN`], held to the wasm32 clause of its
/// [`STOCK_PIN_FILE`] where it holds one, since the release's gate and the site's /terminal
/// engine build on that clause's channel and the dev lanes judge what ships on the same release.
/// A checkout without the file (a public snapshot) rides [`STOCK_PIN`] alone.
///
/// # Errors
/// The file is there and cannot be read, its clause is refused ([`wasm_clause`]), or it names
/// another release than [`STOCK_PIN`].
pub fn stock_pin(repo: &Path) -> Result<String, ClauseError> {
    let path = repo.join(STOCK_PIN_FILE);
    if absent(&path) {
        return Ok(STOCK_PIN.to_string());
    }
    let config =
        crate::discovery::read_bounded_text(&path, crate::discovery::MAX_ATERM_CONFIG_BYTES)
            .ok_or_else(|| {
                ClauseError::new(
                    format!(
                        "{} is not a readable UTF-8 file of at most {} bytes",
                        path.display(),
                        crate::discovery::MAX_ATERM_CONFIG_BYTES
                    ),
                    format!("restore {STOCK_PIN_FILE}"),
                )
            })?;
    let clause = wasm_clause(&config)?;
    if clause.pin != STOCK_PIN {
        return Err(ClauseError::new(
            format!(
                "{STOCK_PIN_FILE}'s wasm32 clause builds the engine on {}, but the checkout's \
                 stock pin is {STOCK_PIN} (aterm_toolchain::rustup::STOCK_PIN): the dev lanes \
                 would compile on another release than the shipped engine",
                clause.pin
            ),
            "name one release in both, in one commit",
        ));
    }
    Ok(clause.pin)
}

/// Whether nothing at all is at `path` (a dangling link or an unreadable entry is something).
fn absent(path: &Path) -> bool {
    std::fs::symlink_metadata(path).is_err_and(|e| e.kind() == std::io::ErrorKind::NotFound)
}

#[cfg(test)]
#[path = "rustup_tests.rs"]
mod tests;
