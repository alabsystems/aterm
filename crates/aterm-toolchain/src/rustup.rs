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

/// The one stock toolchain every STOCK EXCEPTION dev lane of the checkout compiles with
/// (`verify cells` and its windows-sys oracles, the web modules' lanes, the Linux lane's guest
/// agent, the libc oracle's cross cells). It is code, so the public snapshot, which carries no
/// `publish/`, pins the same release; [`stock_pin`] holds it to the engine gate's wasm32 clause.
/// No dev lane falls back to rustup's floating `stable`, whose release is whatever a machine last
/// installed: one tree's verdict would then differ from one Mac to the next. (The release chain's
/// own slices, and the lanes that rehearse them, still build on `stable`.)
pub const STOCK_PIN: &str = "1.97.1";

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
mod tests {
    use super::*;

    #[test]
    fn a_set_but_empty_rustup_home_is_the_default() {
        let home = Path::new("/h");
        assert_eq!(rustup_home(None, home), PathBuf::from("/h/.rustup"));
        assert_eq!(
            rustup_home(Some(OsStr::new("")), home),
            PathBuf::from("/h/.rustup")
        );
        assert_eq!(
            rustup_home(Some(OsStr::new("/r")), home),
            PathBuf::from("/r")
        );
    }

    #[test]
    fn a_target_is_listed_only_by_its_exact_line() {
        let listing = "aarch64-apple-darwin\nwasm32-wasip1\nwasm32-unknown-unknown-nightly\n";
        assert!(lists_target(listing, "aarch64-apple-darwin"));
        assert!(!lists_target(listing, "wasm32-unknown-unknown"));
        assert!(lists_target(
            "  wasm32-unknown-unknown  \n",
            "wasm32-unknown-unknown"
        ));
    }

    #[test]
    fn toolchain_names_drop_rustups_markers() {
        let listing = "stable-aarch64-apple-darwin (active, default)\nnightly-2026-01-01\ntrust\n";
        assert_eq!(
            toolchain_names(listing),
            ["stable-aarch64-apple-darwin", "nightly-2026-01-01", "trust"]
        );
    }

    const HOST: &str = "aarch64-apple-darwin";

    #[test]
    fn a_toolchain_is_held_by_its_name_on_rustups_host_or_its_bare_linked_name() {
        let listing = "stable-aarch64-apple-darwin (active, default)\n1.97.1-aarch64-apple-darwin\n\
                       1.97.10-aarch64-apple-darwin\ntrust\n";
        assert!(holds_toolchain(listing, "1.97.1", HOST));
        assert!(holds_toolchain(listing, "trust", HOST));
        assert!(holds_toolchain(listing, "stable", HOST));
        assert!(
            !holds_toolchain(listing, "1.97", HOST),
            "a prefix of a release is not that release"
        );
        assert!(!holds_toolchain(listing, "1.99.1", HOST));
        assert!(!holds_toolchain("", "1.97.1", HOST));
    }

    /// Only rustup's host's own install of a release is that release: a cross-host install and
    /// a linked toolchain that begins with the release's name are other toolchains, which
    /// `rustup which --toolchain 1.97.1` never resolves to.
    #[test]
    fn a_cross_host_or_custom_toolchain_is_not_the_hosts_pin() {
        let listing = "1.97.1-x86_64-apple-darwin\n1.97.1-mine\n";
        assert!(!holds_toolchain(listing, "1.97.1", HOST), "{listing}");
        assert!(holds_toolchain(listing, "1.97.1", "x86_64-apple-darwin"));
    }

    /// rustup's host is its settings' `default_host_triple`, else this build's own triple.
    #[test]
    fn rustups_host_is_its_settings_default_host_else_this_builds_own() {
        let home = aterm_tempfile::tempdir().unwrap();
        assert_eq!(
            default_host(home.path()),
            NATIVE_TRIPLE,
            "no settings at all"
        );
        let settings = home.path().join("settings.toml");
        std::fs::write(&settings, "version = \"12\"\nprofile = \"minimal\"\n").unwrap();
        assert_eq!(
            default_host(home.path()),
            NATIVE_TRIPLE,
            "settings that name none"
        );
        std::fs::write(
            &settings,
            "version = \"12\"\ndefault_host_triple = \"x86_64-apple-darwin\"\n\
             [overrides]\n",
        )
        .unwrap();
        assert_eq!(default_host(home.path()), "x86_64-apple-darwin");
        std::fs::write(
            &settings,
            "version = \"12\"\n[overrides]\ndefault_host_triple = \"x86_64-apple-darwin\"\n",
        )
        .unwrap();
        assert_eq!(
            default_host(home.path()),
            NATIVE_TRIPLE,
            "a key under a table is not the setting"
        );
        assert!(
            !NATIVE_TRIPLE.is_empty(),
            "every host a lane runs on has a triple"
        );
    }

    #[test]
    fn the_wasm32_clause_is_read_out_of_the_engines_own_line() {
        let good = "CHECK_CMD_DEFAULT=\"rustup run trust targo trust check && rustup target add \
                    --toolchain 1.95.0 wasm32-unknown-unknown && sh -c \
                    'RUSTFLAGS=-Ctarget-feature=+simd128 RUSTC=$(rustup which --toolchain 1.95.0 \
                    rustc) rustup run 1.95.0 cargo check --locked --target wasm32-unknown-unknown \
                    -p aterm-wasm'\"\n";
        let clause = wasm_clause(good).unwrap();
        assert_eq!(clause.pin, "1.95.0");
        assert!(
            clause
                .clause
                .starts_with("sh -c 'RUSTFLAGS=-Ctarget-feature=+simd128 RUSTC=")
        );
        for config in [
            "CHECK_CMD_DEFAULT=\"targo check\"\n",
            &good.replace(
                "rustup target add --toolchain 1.95.0 wasm32-unknown-unknown && ",
                "",
            ),
            &good.replace("RUSTC=$(rustup which --toolchain 1.95.0 rustc) ", ""),
            &good.replace("rustup run 1.95.0 cargo", "cargo"),
        ] {
            assert!(wasm_clause(config).is_err(), "{config}");
        }
    }

    /// An engine gate whose wasm32 clause builds on `pin`, as publish/config.sh spells it.
    fn gate(pin: &str) -> String {
        format!(
            "CHECK_CMD_DEFAULT=\"rustup run trust targo trust check && rustup target add \
             --toolchain {pin} wasm32-unknown-unknown && sh -c \
             'RUSTFLAGS=-Ctarget-feature=+simd128 RUSTC=$(rustup which --toolchain {pin} rustc) \
             rustup run {pin} cargo check --locked --target wasm32-unknown-unknown -p \
             aterm-wasm'\"\n"
        )
    }

    /// A public snapshot carries no `publish/`, and still names the stock pin.
    #[test]
    fn a_public_snapshot_without_publish_names_the_stock_pin() {
        let public = aterm_tempfile::tempdir().unwrap();
        assert_eq!(stock_pin(public.path()), Ok(STOCK_PIN.to_string()));
    }

    /// Where the checkout holds the engine gate, the pin is held to its wasm32 clause: a clause
    /// on the pin agrees, one on another release or one the gate refuses is an error naming why,
    /// never a pin read off either alone. This checkout, public or not, agrees.
    #[test]
    fn the_stock_pin_is_held_to_the_engine_gates_wasm32_clause() {
        let repo = aterm_tempfile::manifest_dir().join("../..");
        assert_eq!(stock_pin(&repo), Ok(STOCK_PIN.to_string()));

        let tree = aterm_tempfile::tempdir().unwrap();
        std::fs::create_dir_all(tree.path().join("publish")).unwrap();
        let config = tree.path().join(STOCK_PIN_FILE);
        std::fs::write(&config, gate(STOCK_PIN)).unwrap();
        assert_eq!(stock_pin(tree.path()), Ok(STOCK_PIN.to_string()));

        std::fs::write(&config, gate("1.95.0")).unwrap();
        let other = stock_pin(tree.path()).unwrap_err();
        assert!(
            other.why.contains("builds the engine on 1.95.0")
                && other.why.contains(&format!("stock pin is {STOCK_PIN}")),
            "{other}"
        );

        std::fs::write(&config, "CHECK_CMD_DEFAULT=\"true\"\n").unwrap();
        let refused = stock_pin(tree.path()).unwrap_err();
        assert!(refused.why.contains("no wasm32 clause"), "{refused}");
    }
}
