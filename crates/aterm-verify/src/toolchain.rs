// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Finding THE toolchain — the directory `rust-toolchain.toml`'s `trust` pin
//! actually resolves to. ONE resolution order, and `aterm-release`'s
//! `gates::trust_stage2_bin` walks the same one (it cannot reach this crate
//! without a new edge, so it mirrors it and says so):
//!
//! 1. `$TRUST_STAGE2_BIN` — an explicit development override, never fallen back from;
//! 2. the rustup toolchain the pin names, `~/.rustup/toolchains/<channel>` — atpkg lays
//!    it as a view of its store, and Trust's `scripts/promote-toolchain.sh` points it at
//!    a SEALED from-source build, the sanctioned way to drive one (never the live tree);
//! 3. the atpkg store's `store/trust/current/bin`;
//! 4. `PATH`.
//!
//! A LIVE BUILD TREE IS NOT A CANDIDATE. `$HOME/trust/build/host/stage2/bin` (and the
//! `~/toolchains/<channel>-current` promote target) were probed here until
//! 2026-09-24, ahead of the store, a month after both were retired from the
//! delivery (2026-08-29). On m7 that made `aterm help rust` report a JULY stage2 —
//! one whose `targo` no longer knows `--unverified` — as "the gates' toolchain"
//! while PATH ran the store's. A build tree is also empty for the whole length of
//! every `x.py build --stage 2`.
//!
//! The rules, all load-bearing:
//!
//! 1. RESOLVE THE PHYSICAL PATH. The rustup entry and the store's `current` are
//!    symlinks and Trust's drivers reject a symlinked toolchain path, so the
//!    chosen directory is canonicalised before anything selects a tool out of it
//!    or puts it on PATH.
//! 2. PATH FIRST. Whatever cargo wins the caller's PATH otherwise (Homebrew's,
//!    typically) drives a stable rustc that rejects the workspace's `-Z` flags,
//!    and every stage then fails for a reason that has nothing to do with the
//!    code. Prepending also makes the driver's own children — trustc, build
//!    scripts that re-invoke it — resolve the trust-named tools.
//! 3. DRIVE `targo`, NOT `cargo`. They are the same binary switching on argv0:
//!    as `cargo` it accepts a bare verb and picks a lane silently; as `targo` it
//!    REFUSES one, because an artifact is either `targo trust <verb>` (verified,
//!    fail-closed) or `--unverified` (no proof claim) — never implicitly either.
//!    Riding the compat name would make this gate quietly unverified, which is
//!    the exact thing the two-lane design prevents. Every invocation names its
//!    lane; the workspace rides `--unverified` until the Trust-Std campaign
//!    greens, the same statement `.cargo/config.toml`'s off-switch already makes.
//!
//! 4. THE STORE PREFIX IS A MIRROR. It is resolved the way atpkg resolves it
//!    ([`atpkg_prefix`]: `[packages].prefix` from aterm.toml, else the platform
//!    default) without a dependency edge, because this crate has none by charter
//!    (see its Cargo.toml).
//!
//! 5. THE DIRECTORY MUST BE THE PIN. `rust-toolchain.toml` names `trust`; a
//!    directory carrying a file called `targo` is not evidence that it is that
//!    toolchain, and rule 2 above ADOPTS such a directory off the caller's PATH.
//!    Every candidate is therefore checked against the pinned channel before it
//!    becomes THE toolchain — see `is_pinned_toolchain`, private to this
//!    module — and a candidate that
//!    fails is REFUSED and named, never silently used. Without that check the
//!    gate would run a different frontend and a different lint set under the
//!    pinned one's name and still print the merge-contract sentence, which is
//!    exactly how six `-D warnings` violations reached main in the sibling
//!    `clean` repo (2026-08-22) after `rust-toolchain.toml` there moved off an
//!    upstream channel.
//!
//! Fail-closed: no targo means the gate FAILS honestly, never a stock-cargo pass.

use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};

use crate::is_executable_file;

/// The channel `rust-toolchain.toml` pins, parsed out of the file's text.
///
/// Deliberately a line scan and not a TOML parse: this crate has no
/// dependencies by charter (see its Cargo.toml), the key is written on one line
/// in every checkout of this repo, and the FAILURE MODE of a scan that misses is
/// `None` — which reads as "no pin declared" and restores the pre-guard
/// behaviour, never as "the pin is satisfied".
#[must_use]
pub fn pinned_channel_in(toml: &str) -> Option<String> {
    toml.lines()
        .map(str::trim)
        .filter(|l| !l.starts_with('#'))
        .find_map(|l| {
            let rest = l.strip_prefix("channel")?.trim_start();
            let rest = rest.strip_prefix('=')?.trim_start();
            let rest = rest.strip_prefix('"')?;
            let end = rest.find('"')?;
            Some(rest[..end].to_string())
        })
}

/// [`pinned_channel_in`] over `<root>/rust-toolchain.toml`.
#[must_use]
pub fn pinned_channel(root: &Path) -> Option<String> {
    std::fs::read_to_string(root.join("rust-toolchain.toml"))
        .ok()
        .as_deref()
        .and_then(pinned_channel_in)
}

/// Channels that resolve to an ORDINARY rust release. For those the historical
/// behaviour is correct — any cargo on PATH is the pinned one, near enough — so
/// the check below passes them through rather than inventing a driver name that
/// no upstream toolchain ships.
fn is_upstream_channel(channel: &str) -> bool {
    matches!(channel, "" | "stable" | "beta" | "nightly")
        || channel.starts_with("nightly-")
        || channel.starts_with("1.")
}

/// Is `dir` really the toolchain `pinned` names?
///
/// A non-upstream channel is a fork with BRANDED drivers, and Trust brands its
/// rustc `trustc` — `<channel>c` — beside the `targo` this directory was
/// selected for. Measured 2026-08-30 in `~/toolchains/trust-current/bin`:
/// `ay cargo clean rustc rustdoc targo targo-fmt targo-tippy targo-trust tippy
/// tippy-driver trust-analyzer trustc trustd trustdoc trustfmt ty`. A stock rust
/// install ships no branded driver at all, which is what makes the pairing
/// decisive.
///
/// AND THE DRIVER MUST ANSWER FOR ITSELF. Presence of the file was the whole
/// check until 2026-08-30, and three things got past it, all of them live:
///
///  * a MID-REBUILD stage tree still holding the previous run's `trustc` while
///    the rest of the tree is half-written;
///  * a bin directory with NO SYSROOT — a driver compiles nothing without
///    `lib/rustlib`, and the measured stage2 above had `bin` and no `lib`;
///  * ANY EXECUTABLE NAMED `trust`, which the second arm accepted on name alone.
///
/// So the candidate is asked `--print sysroot`, in the one dialect only a
/// rustc-family driver speaks, and the answer must name a real directory
/// carrying `lib/rustlib`. An EXIT CODE would not have been enough: a zero-byte
/// file with the execute bit set runs as an empty shell script and exits 0
/// (measured), so `--version` "succeeds" on a truncated driver. Demanding output
/// that must resolve to a directory is what makes the question decisive.
///
/// `None` means the repo declares no pin: pass through, unchanged behaviour.
fn is_pinned_toolchain(dir: &Path, pinned: Option<&str>) -> bool {
    let channel = match pinned {
        None => return true,
        Some(channel) if is_upstream_channel(channel) => return true,
        Some(channel) => channel,
    };
    let Some(driver) = [format!("{channel}c"), channel.to_string()]
        .into_iter()
        .map(|n| dir.join(n))
        .find(|p| is_executable_file(p))
    else {
        return false;
    };
    let Ok(out) = std::process::Command::new(&driver)
        .arg("--print")
        .arg("sysroot")
        .output()
    else {
        return false;
    };
    if !out.status.success() {
        return false;
    }
    let Ok(sysroot) = String::from_utf8(out.stdout) else {
        return false;
    };
    let sysroot = sysroot.trim();
    !sysroot.is_empty() && Path::new(sysroot).join("lib/rustlib").is_dir()
}

/// The atpkg install prefix, resolved the way atpkg resolves it — as a MIRROR,
/// not a dependency edge (this crate has none, by charter).
///
/// `[packages].prefix` from the user config (`<xdg_config_home>/aterm/aterm.toml`,
/// else `<home>/.config/aterm/aterm.toml` — `atpkg::config::config_path`'s rule),
/// `~`-expanded, absolute paths only; anything else falls back to the platform
/// default `atpkg::platform::default_prefix` lays down: `Library/Application
/// Support/aterm/pkg` under `home` on macOS, `.local/share/aterm/pkg` elsewhere.
/// atpkg's own `vet_prefix` chain check is the authority on whether a configured
/// prefix is SAFE; this only has to agree with it about where to LOOK, and a
/// prefix that would fail atpkg's check simply holds no store.
#[must_use]
pub fn atpkg_prefix(home: &Path, xdg_config_home: Option<&Path>) -> PathBuf {
    let cfg = xdg_config_home
        .filter(|d| !d.as_os_str().is_empty())
        .map_or_else(|| home.join(".config"), Path::to_path_buf)
        .join("aterm/aterm.toml");
    if let Ok(text) = std::fs::read_to_string(&cfg)
        && let Some(configured) = configured_prefix_in(&text, home)
    {
        return configured;
    }
    default_atpkg_prefix(home)
}

/// The platform default prefix (the mirror of `atpkg::platform::default_prefix`).
#[must_use]
pub fn default_atpkg_prefix(home: &Path) -> PathBuf {
    if cfg!(target_os = "macos") {
        home.join("Library/Application Support/aterm/pkg")
    } else {
        home.join(".local/share/aterm/pkg")
    }
}

/// `[packages].prefix` out of aterm.toml text, expanded against `home`.
///
/// A line scan for the same reason [`pinned_channel_in`] is one: the key is
/// written on one line, and a scan that misses yields `None` — the default
/// prefix — never a wrong prefix. Only the `[packages]` table is read; a
/// `prefix` key under any other table is ignored, as atpkg ignores it.
#[must_use]
pub fn configured_prefix_in(toml: &str, home: &Path) -> Option<PathBuf> {
    let mut in_packages = false;
    for line in toml.lines().map(str::trim) {
        if line.starts_with('#') {
            continue;
        }
        if line.starts_with('[') {
            in_packages = line == "[packages]";
            continue;
        }
        if !in_packages {
            continue;
        }
        let Some(rest) = line.strip_prefix("prefix") else {
            continue;
        };
        let rest = rest.trim_start().strip_prefix('=')?.trim_start();
        let rest = rest.strip_prefix('"')?;
        let value = rest[..rest.find('"')?].trim();
        return match value {
            "" => None,
            "~" => Some(home.to_path_buf()),
            v if v.starts_with("~/") => Some(home.join(&v[2..])),
            v if Path::new(v).is_absolute() => Some(PathBuf::from(v)),
            _ => None, // relative, or `~user`: not a prefix
        };
    }
    None
}

/// `<prefix>/store/trust/current/bin` — where `aterm pkg install trust` puts the
/// pinned toolchain's drivers (`atpkg::store::Layout::program_current("trust")`).
#[must_use]
pub fn store_stage2_bin(prefix: &Path) -> PathBuf {
    prefix.join("store/trust/current/bin")
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Toolchain {
    /// The resolved (physical) stage2 `bin` directory.
    pub stage2_dir: PathBuf,
    pub targo: PathBuf,
    pub trustdoc: PathBuf,
    /// `targo-tippy`, or the older `targo-clippy`, if either is installed.
    pub tippy: Option<PathBuf>,
    /// A candidate that carried a `targo` but was NOT the pinned toolchain, and
    /// was therefore refused. Kept so [`Self::missing_targo_label`] can say
    /// "there was one and it was the wrong one" — the diagnosis and the remedy
    /// for that are nothing like the ones for "there was none".
    pub refused: Option<PathBuf>,
    /// The atpkg store candidate (`store/trust/current/bin` under the resolved
    /// prefix), whether or not it held anything, so the diagnostic can name the place
    /// `aterm pkg install trust` would have filled. `None` for an explicit override.
    pub store_bin: Option<PathBuf>,
}

impl Toolchain {
    /// [`Self::discover_with_store`] under the atpkg prefix atpkg itself would resolve
    /// ([`atpkg_prefix`]; `$XDG_CONFIG_HOME` is the one environment read here, for the
    /// config file atpkg reads). `pinned` is the channel `rust-toolchain.toml` names
    /// ([`pinned_channel`]); `None` disables the pin check, which is what the pure unit
    /// tests below want and what a repo with no pin means.
    #[must_use]
    pub fn discover(
        stage2_bin: Option<&Path>,
        home: &Path,
        path_env: &OsStr,
        pinned: Option<&str>,
    ) -> Self {
        let xdg = std::env::var_os("XDG_CONFIG_HOME").map(PathBuf::from);
        let prefix = atpkg_prefix(home, xdg.as_deref());
        Self::discover_with_store(stage2_bin, home, Some(&prefix), path_env, pinned)
    }

    /// THE resolution order (the module header), first hit wins:
    ///
    /// 1. `stage2_bin` — `$TRUST_STAGE2_BIN`; checked, never fallen back from: naming a
    ///    toolchain that is not there is an error to surface, not a preference.
    /// 2. `<home>/.rustup/toolchains/<channel>/bin` — the rustup toolchain the pin names.
    /// 3. `<store_prefix>/store/trust/current/bin` — the atpkg store (`None` skips it).
    /// 4. every `path_env` directory.
    ///
    /// A candidate must carry a `targo` AND be the pin (`is_pinned_toolchain`); the
    /// first one that carries a targo and is not is remembered in `refused` for the
    /// diagnostic, and the search goes on past it. When nothing qualifies, the store
    /// (else the rustup entry) is the reported location, because that is where the
    /// remedy puts one.
    #[must_use]
    pub fn discover_with_store(
        stage2_bin: Option<&Path>,
        home: &Path,
        store_prefix: Option<&Path>,
        path_env: &OsStr,
        pinned: Option<&str>,
    ) -> Self {
        let physical = |dir: PathBuf| std::fs::canonicalize(&dir).unwrap_or(dir);
        let mut refused = None;
        let (tool_dir, store_bin) = if let Some(explicit) = stage2_bin {
            // `$TRUST_STAGE2_BIN` is an ordinary environment variable and can name a tree
            // that is not the pin as easily as PATH can: a refused directory stays the
            // reported one (it is what the operator asked for) and `refused` makes
            // `have_targo` answer no, the same fail-closed branch as an absent one.
            let dir = physical(explicit.to_path_buf());
            if is_executable_file(&dir.join("targo")) && !is_pinned_toolchain(&dir, pinned) {
                refused = Some(dir.clone());
            }
            (dir, None)
        } else {
            let rustup = home
                .join(".rustup/toolchains")
                .join(pinned.unwrap_or("trust"))
                .join("bin");
            let store_bin = store_prefix.map(store_stage2_bin);
            let candidates = std::iter::once(rustup.clone())
                .chain(store_bin.clone())
                .chain(std::env::split_paths(path_env));
            let mut chosen = None;
            for dir in candidates {
                if !is_executable_file(&dir.join("targo")) {
                    continue;
                }
                let dir = physical(dir);
                if is_pinned_toolchain(&dir, pinned) {
                    chosen = Some(dir);
                    refused = None;
                    break;
                }
                refused.get_or_insert(dir);
            }
            let reported = chosen.unwrap_or_else(|| store_bin.clone().unwrap_or(rustup));
            (reported, store_bin)
        };
        let tippy = ["targo-tippy", "targo-clippy"]
            .into_iter()
            .map(|n| tool_dir.join(n))
            .find(|p| is_executable_file(p));
        Self {
            targo: tool_dir.join("targo"),
            trustdoc: tool_dir.join("trustdoc"),
            tippy,
            stage2_dir: tool_dir,
            refused,
            store_bin,
        }
    }

    /// Is the verified driver actually there? Every cargo-shaped stage asks this
    /// first, and none of them falls back to a stock cargo.
    ///
    /// A REFUSED directory answers no even though the file is right there: a
    /// `targo` that is not the pinned toolchain's is not the verified driver,
    /// and the whole point of the check is that it fails the same closed way an
    /// absent one does rather than quietly becoming the gate's compiler.
    #[must_use]
    pub fn have_targo(&self) -> bool {
        self.refused.is_none() && is_executable_file(&self.targo)
    }

    /// A built Trust stage2 names its documentation driver `trustdoc`. When an
    /// EXECUTABLE one is there (a present file without an exec bit counts as
    /// not there — it could not drive anything), the doc-running stages bind
    /// it through `RUSTDOC`; otherwise cargo's own discovery decides — a caller-exported `RUSTDOC`/
    /// `CARGO_BUILD_RUSTDOC` first, else the config's bare
    /// `[build] rustdoc = "trustdoc"` resolved from the children's PATH (the
    /// `~/.local/bin` farm link). Either of those still runs fail-closed with
    /// real doctest verdicts; only when a doctest-compiling run has NOTHING to
    /// exec does the test stage declare COULD-NOT-RUN up front
    /// ([`Self::missing_trustdoc_label`]) and the later doc-running stages
    /// skip pointing at it, instead of cargo dying at exec with a raw OS error
    /// that names no remedy. The full rule lives in `stages::doc_driver`.
    #[must_use]
    pub fn have_trustdoc(&self) -> bool {
        is_executable_file(&self.trustdoc)
    }

    /// The compiler this run is using, for the mid-run tripwire
    /// ([`crate::identity::Tripwire`]) and the snapshot lanes' stamps.
    ///
    /// WHY (2026-09-13): the gate resolves one physical stage2 directory and
    /// runs every stage with it, but a directory is not a compiler — an atpkg
    /// update or a promote rewrites the files in place, and a 14 h run spans
    /// several of those. So the identity is the files: `(dev, ino, len,
    /// mtime)` of `targo`, `trustc`, `trustdoc` and `tippy`, plus the commit
    /// `trustc -vV` names. The version query runs under a one-minute ceiling in
    /// `scratch`, so a wedged driver costs the commit hash, never the run.
    #[must_use]
    pub fn identity(&self, path_env: &OsStr, scratch: &Path) -> crate::identity::ToolchainIdentity {
        let trustc = self.stage2_dir.join("trustc");
        let tippy = self
            .tippy
            .clone()
            .unwrap_or_else(|| self.stage2_dir.join("tippy"));
        let files = [&self.targo, &trustc, &self.trustdoc, &tippy]
            .into_iter()
            .map(|p| (p.clone(), crate::identity::FileStamp::of(p)))
            .collect();
        let commit = if is_executable_file(&trustc) {
            let log = scratch.join(format!("trustc-vV.{}.log", std::process::id()));
            let _ = std::fs::remove_file(&log);
            let run = crate::exec::run(
                &crate::exec::Cmd::new(&trustc)
                    .arg("-vV")
                    .capture(crate::exec::Capture::Append(log.clone())),
                crate::exec::ExecEnv {
                    cwd: scratch,
                    path: path_env,
                    scratch,
                    child_ceiling: Some(std::time::Duration::from_secs(60)),
                    remove_env: &[],
                    add_env: &[],
                    timings: None,
                },
            );
            let text = std::fs::read_to_string(&log).unwrap_or_default();
            let _ = std::fs::remove_file(&log);
            run.ok
                .then(|| crate::identity::commit_hash_in(&text))
                .flatten()
        } else {
            None
        };
        crate::identity::ToolchainIdentity { files, commit }
    }

    /// PATH for every child: the stage2 directory first, but only when a `targo`
    /// really lives there — the script guarded the export the same way, so a
    /// stale `TRUST_STAGE2_BIN` cannot shadow the caller's tools with nothing.
    #[must_use]
    pub fn path_with_stage2_first(&self, inherited: &OsStr) -> OsString {
        if !self.have_targo() {
            return inherited.to_os_string();
        }
        let mut p = OsString::from(self.stage2_dir.as_os_str());
        if !inherited.is_empty() {
            p.push(":");
            p.push(inherited);
        }
        p
    }

    /// The remedy every "no toolchain" diagnostic leads with. The signed
    /// package index is the ordinary source of the pinned toolchain; a from-source
    /// build is the developer alternative, SEALED first — `promote-toolchain.sh` points
    /// the rustup `trust` toolchain at the seal, never at the live build tree, which
    /// `x.py` empties for the length of every rebuild (discovery probes no build tree).
    pub const INSTALL_REMEDY: &'static str = "`aterm pkg install trust` (then `aterm pkg doctor` \
         to confirm the store); from source instead: python3 x.py build --stage 2 in $HOME/trust, \
         then seal it with $HOME/trust/scripts/promote-toolchain.sh";

    /// The diagnostic for no usable toolchain: none found, or one found and refused.
    #[must_use]
    pub fn missing_targo_label(&self) -> String {
        if let Some(dir) = &self.refused {
            return format!(
                "targo at {} is NOT the toolchain rust-toolchain.toml pins (no `trustc` beside \
                 it that answers with a real sysroot — a mid-rebuild stage tree fails this way), \
                 so running the gate there would use a different frontend and a different lint \
                 set under the pinned one's name. Refusing. Fix: {} — or point TRUST_STAGE2_BIN \
                 at a finished stage2 bin",
                dir.display(),
                Self::INSTALL_REMEDY,
            );
        }
        match &self.store_bin {
            Some(store) => format!(
                "targo not found — looked in the rustup `trust` toolchain, the atpkg store at \
                 {}, and PATH. Fix: {}; or point TRUST_STAGE2_BIN at a stage2 bin",
                store.display(),
                Self::INSTALL_REMEDY,
            ),
            // An explicit override (discovery always probes a store).
            None => format!(
                "targo not found at {} — TRUST_STAGE2_BIN names no toolchain, and an explicit \
                 override is never fallen back from: unset it to let discovery run. Fix: {}",
                self.targo.display(),
                Self::INSTALL_REMEDY,
            ),
        }
    }

    /// The diagnostic for a doc-running stage that cannot start: no `trustdoc`
    /// in the stage2, no caller-exported `RUSTDOC`, and no bare `trustdoc` on
    /// the children's PATH, so cargo's `[build] rustdoc = "trustdoc"`
    /// (.cargo/config.toml) has nothing to exec. The remedy is a stage2 that
    /// carries trustdoc — the store's does (`aterm pkg install trust` shims it
    /// onto PATH as well); a from-source stage2 needs the rebuild. Then `cargo
    /// ship provision` can link `~/.local/bin/trustdoc` for direct cargo runs.
    #[must_use]
    pub fn missing_trustdoc_label(&self) -> String {
        format!(
            "no doc driver: {} is not an executable doc driver and no `trustdoc` \
             resolves on PATH, so cargo's [build] rustdoc = \"trustdoc\" \
             (.cargo/config.toml) has nothing to exec for the doctest lane. Fix: {} — the \
             store's stage2 carries trustdoc and shims it onto PATH; the gate then binds it \
             directly, and `cargo ship provision` can link ~/.local/bin/trustdoc for direct \
             cargo runs",
            self.trustdoc.display(),
            Self::INSTALL_REMEDY,
        )
    }

    #[must_use]
    pub fn missing_tippy_label(&self) -> String {
        format!(
            "tippy lint (no targo-tippy or targo-clippy in {} — fix: {})",
            self.stage2_dir.display(),
            Self::INSTALL_REMEDY,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    /// `chmod +x`, where an execute bit exists. A no-op elsewhere BY DESIGN and
    /// not by neglect: [`is_executable_file`](crate::is_executable_file) is
    /// existence off unix, so a stub that was merely WRITTEN already satisfies
    /// every discovery predicate these tests drive. Keeping the helper (rather
    /// than gating each test) is what lets the discovery ORDER — store before
    /// from-source, sealed before live — stay pinned on a target with no mode.
    fn make_runnable(path: &Path) {
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(path, fs::Permissions::from_mode(0o755)).expect("chmod");
        }
        #[cfg(not(unix))]
        {
            let _ = path;
        }
    }

    fn exec_stub(path: &Path) {
        fs::write(path, b"#!/bin/sh\nexit 0\n").expect("write");
        make_runnable(path);
    }

    /// A stub that answers `--print sysroot` like a real driver, with the
    /// `lib/rustlib` that makes the answer credible. `exec_stub` alone is now a
    /// TRUNCATED driver as far as [`is_pinned_toolchain`] is concerned — which is
    /// the point of the strengthening, and why the pin tests use this instead.
    /// The sysroot is `<bin>/..`, the real layout.
    fn driver_stub(path: &Path) {
        let sysroot = path.parent().expect("bin dir").parent().expect("sysroot");
        fs::create_dir_all(sysroot.join("lib/rustlib")).expect("mkdir rustlib");
        fs::write(
            path,
            format!("#!/bin/sh\necho '{}'\n", sysroot.display()).as_bytes(),
        )
        .expect("write");
        make_runnable(path);
    }

    #[test]
    fn the_reported_location_when_nothing_resolves_is_the_store() {
        // Not a statement about PREFERENCE (see `one_resolution_order_and_no_build_tree`).
        // This pins what the gate REPORTS when no candidate exists at all: the store,
        // the place `aterm pkg install trust` fills — and, with no store probed, the
        // rustup entry.
        let home = Path::new("/nonexistent-home");
        let prefix = default_atpkg_prefix(home);
        let t = Toolchain::discover_with_store(None, home, Some(&prefix), OsStr::new(""), None);
        assert_eq!(t.targo, store_stage2_bin(&prefix).join("targo"));
        assert_eq!(t.trustdoc, store_stage2_bin(&prefix).join("trustdoc"));
        assert!(!t.have_targo());
        assert!(t.tippy.is_none());
        let t = Toolchain::discover_with_store(None, home, None, OsStr::new(""), None);
        assert_eq!(
            t.targo,
            Path::new("/nonexistent-home/.rustup/toolchains/trust/bin/targo")
        );
    }

    #[test]
    fn the_missing_trustdoc_diagnosis_names_the_config_key_and_both_remedies() {
        let home = Path::new("/nonexistent-home");
        let prefix = default_atpkg_prefix(home);
        let t = Toolchain::discover_with_store(None, home, Some(&prefix), OsStr::new(""), None);
        let label = t.missing_trustdoc_label();
        assert!(label.contains("x.py build --stage 2"), "{label}");
        assert!(label.contains("~/.local/bin/trustdoc"), "{label}");
        assert!(label.contains(".cargo/config.toml"), "{label}");
    }

    #[test]
    fn every_remedy_leads_with_the_package_manager_and_names_source_second() {
        // The signed index is the ordinary way to have the toolchain; x.py is the
        // developer alternative. Every diagnostic says them in that order.
        let home = Path::new("/nonexistent-home");
        let prefix = default_atpkg_prefix(home);
        let t = Toolchain::discover_with_store(None, home, Some(&prefix), OsStr::new(""), None);
        for label in [
            t.missing_targo_label(),
            t.missing_trustdoc_label(),
            t.missing_tippy_label(),
        ] {
            let pkg = label.find("aterm pkg install trust").unwrap_or_else(|| {
                panic!("the remedy must lead with the package manager: {label}")
            });
            let src = label
                .find("x.py build --stage 2")
                .unwrap_or_else(|| panic!("the from-source alternative must be named: {label}"));
            assert!(pkg < src, "package manager first, source second: {label}");
        }
        // The absent-toolchain label also names where the store WOULD have been.
        let label = t.missing_targo_label();
        assert!(label.contains("atpkg store at"), "{label}");
        assert!(label.contains("store/trust/current/bin"), "{label}");
    }

    /// A pinned toolchain bin under `root` — `targo` plus a `trustc` that answers with a
    /// real sysroot — returned as its physical path.
    fn pinned_bin(root: &Path) -> PathBuf {
        fs::create_dir_all(root).expect("mkdir");
        exec_stub(&root.join("targo"));
        driver_stub(&root.join("trustc"));
        fs::canonicalize(root).expect("canonicalize")
    }

    /// THE ORDER, and what is no longer in it. Unix-pinned on the SYMLINKS — the rustup
    /// entry and the store's `current` — which are the shapes really laid down.
    #[cfg(unix)]
    #[test]
    fn one_resolution_order_and_no_build_tree() {
        let home = crate::mktemp_dir("atv-order").expect("mktemp");
        let prefix = default_atpkg_prefix(&home);
        // Every candidate present and pinned, plus both retired spellings.
        let store = pinned_bin(&prefix.join("store/trust/6808/bin"));
        std::os::unix::fs::symlink(
            prefix.join("store/trust/6808"),
            prefix.join("store/trust/current"),
        )
        .expect("ln current");
        let linked = pinned_bin(&home.join("sealed/bin"));
        fs::create_dir_all(home.join(".rustup/toolchains")).expect("mkdir");
        std::os::unix::fs::symlink(home.join("sealed"), home.join(".rustup/toolchains/trust"))
            .expect("ln rustup");
        let on_path = pinned_bin(&home.join("elsewhere/bin"));
        let tree = pinned_bin(&home.join("trust/build/host/stage2/bin"));
        let promoted = pinned_bin(&home.join("toolchains/trust-current/bin"));
        let path = std::env::join_paths([&tree, &promoted, &on_path]).expect("join");
        let found = |explicit: Option<&Path>, path: &OsStr| {
            Toolchain::discover_with_store(explicit, &home, Some(&prefix), path, Some("trust"))
        };

        // 1. An explicit override wins, and never consults the store.
        let t = found(Some(&on_path), &path);
        assert_eq!(
            (t.stage2_dir.as_path(), t.store_bin.is_none()),
            (on_path.as_path(), true)
        );
        // 2. The rustup toolchain the pin names, resolved to its physical directory.
        let t = found(None, &path);
        assert!(t.have_targo());
        assert_eq!(t.stage2_dir, linked);
        assert_eq!(t.store_bin, Some(store_stage2_bin(&prefix)));
        // 3. The store.
        fs::remove_file(home.join(".rustup/toolchains/trust")).expect("rm rustup");
        assert_eq!(found(None, &path).stage2_dir, store);
        // 4. PATH — the build tree and the promote target are reached ONLY as PATH
        //    entries, never probed under home.
        fs::remove_file(prefix.join("store/trust/current")).expect("rm current");
        assert_eq!(found(None, &path).stage2_dir, tree);
        let t = found(None, OsStr::new(""));
        assert!(
            !t.have_targo(),
            "a build tree under home is not a candidate: {}",
            t.stage2_dir.display()
        );
        assert_eq!(t.stage2_dir, store_stage2_bin(&prefix));
        fs::remove_dir_all(&home).ok();
    }

    #[test]
    fn a_store_targo_that_is_not_the_pin_is_refused_and_the_search_goes_on() {
        let home = crate::mktemp_dir("atv-storepin").expect("mktemp");
        let prefix = default_atpkg_prefix(&home);
        let store = store_stage2_bin(&prefix);
        fs::create_dir_all(&store).expect("mkdir");
        exec_stub(&store.join("targo")); // no trustc beside it: an impostor
        let real = home.join("elsewhere/bin");
        fs::create_dir_all(&real).expect("mkdir");
        exec_stub(&real.join("targo"));
        driver_stub(&real.join("trustc"));

        let t = Toolchain::discover_with_store(
            None,
            &home,
            Some(&prefix),
            OsStr::new(real.to_string_lossy().as_ref()),
            Some("trust"),
        );
        assert!(t.have_targo(), "PATH still wins past a refused store");
        assert_eq!(t.stage2_dir, fs::canonicalize(&real).expect("canonicalize"));

        let t = Toolchain::discover_with_store(
            None,
            &home,
            Some(&prefix),
            OsStr::new(""),
            Some("trust"),
        );
        assert!(!t.have_targo(), "fail-closed: the impostor is not adopted");
        assert_eq!(
            t.refused,
            Some(fs::canonicalize(&store).expect("canonicalize"))
        );
        fs::remove_dir_all(&home).ok();
    }

    #[test]
    fn the_prefix_mirror_honours_a_configured_packages_prefix() {
        let home = Path::new("/fake-home");
        // Only the [packages] table's key counts, `~` expands against home, a
        // relative value is not a prefix, and a commented line is not read.
        assert_eq!(
            configured_prefix_in("[packages]\nprefix = \"~/lab/pkg\"\n", home),
            Some(PathBuf::from("/fake-home/lab/pkg"))
        );
        assert_eq!(
            configured_prefix_in(
                "[packages]\n# prefix = \"~/decoy\"\nprefix = \"/opt/aterm/pkg\"\n",
                home
            ),
            Some(PathBuf::from("/opt/aterm/pkg"))
        );
        assert_eq!(
            configured_prefix_in("[packages]\nprefix = \"relative/pkg\"\n", home),
            None
        );
        assert_eq!(
            configured_prefix_in("[packages]\nprefix = \"~user/pkg\"\n", home),
            None
        );
        assert_eq!(
            configured_prefix_in("[other]\nprefix = \"/opt/x\"\n", home),
            None
        );
        assert_eq!(configured_prefix_in("", home), None);

        // The file itself: XDG_CONFIG_HOME first, else <home>/.config; no file
        // means the platform default under home.
        let tmp = crate::mktemp_dir("atv-prefix").expect("mktemp");
        assert_eq!(atpkg_prefix(&tmp, None), default_atpkg_prefix(&tmp));
        let xdg = tmp.join("xdg");
        fs::create_dir_all(xdg.join("aterm")).expect("mkdir");
        fs::write(
            xdg.join("aterm/aterm.toml"),
            "[packages]\nprefix = \"/opt/lab\"\n",
        )
        .expect("write");
        assert_eq!(atpkg_prefix(&tmp, Some(&xdg)), PathBuf::from("/opt/lab"));
        fs::create_dir_all(tmp.join(".config/aterm")).expect("mkdir");
        fs::write(
            tmp.join(".config/aterm/aterm.toml"),
            "[packages]\nprefix = \"~/mine\"\n",
        )
        .expect("write");
        assert_eq!(atpkg_prefix(&tmp, None), tmp.join("mine"));
        assert_eq!(
            store_stage2_bin(&default_atpkg_prefix(Path::new("/h"))).to_string_lossy(),
            if cfg!(target_os = "macos") {
                "/h/Library/Application Support/aterm/pkg/store/trust/current/bin"
            } else {
                "/h/.local/share/aterm/pkg/store/trust/current/bin"
            }
        );
        fs::remove_dir_all(&tmp).ok();
    }

    /// Unix-pinned: the defect is a `build/host` DIRECTORY SYMLINK, and
    /// planting one off unix is `std::os::windows::fs::symlink_dir` behind a
    /// privilege the runner may not hold. `fs::canonicalize` — the fix under
    /// test — is portable and is exercised by the sibling tests everywhere.
    #[cfg(unix)]
    #[test]
    fn a_symlinked_stage2_resolves_to_its_physical_path() {
        // Trust's drivers reject a symlinked toolchain path, so `build/host` —
        // usually a target-triple symlink — must be resolved before use.
        let tmp = crate::mktemp_dir("atv-tc").expect("mktemp");
        let real = tmp.join("aarch64-apple-darwin/stage2/bin");
        fs::create_dir_all(&real).expect("mkdir");
        exec_stub(&real.join("targo"));
        std::os::unix::fs::symlink(tmp.join("aarch64-apple-darwin"), tmp.join("host")).expect("ln");

        let via_link = tmp.join("host/stage2/bin");
        let t = Toolchain::discover(Some(&via_link), Path::new("/unused"), OsStr::new(""), None);
        assert!(t.have_targo());
        assert_eq!(t.stage2_dir, fs::canonicalize(&real).expect("canonicalize"));
        assert!(
            !t.stage2_dir.to_string_lossy().contains("/host/"),
            "the symlinked spelling must not survive into the tool paths"
        );
        fs::remove_dir_all(&tmp).ok();
    }

    #[test]
    fn tippy_prefers_the_current_name_and_accepts_the_old_one() {
        let tmp = crate::mktemp_dir("atv-tippy").expect("mktemp");
        // The lookup happens in the RESOLVED directory (see the symlink test):
        // on macOS /tmp is itself a symlink to /private/tmp.
        let real = fs::canonicalize(&tmp).expect("canonicalize");
        exec_stub(&tmp.join("targo-clippy"));
        let t = Toolchain::discover(Some(&tmp), Path::new("/unused"), OsStr::new(""), None);
        assert_eq!(
            t.tippy.as_deref(),
            Some(real.join("targo-clippy").as_path())
        );

        exec_stub(&tmp.join("targo-tippy"));
        let t = Toolchain::discover(Some(&tmp), Path::new("/unused"), OsStr::new(""), None);
        assert_eq!(
            t.tippy.as_deref(),
            Some(real.join("targo-tippy").as_path()),
            "the Trust fork's own name wins when both exist"
        );
        fs::remove_dir_all(&tmp).ok();
    }

    #[test]
    fn path_is_only_rewritten_when_a_targo_is_really_there() {
        let tmp = crate::mktemp_dir("atv-path").expect("mktemp");
        let t = Toolchain::discover(Some(&tmp), Path::new("/unused"), OsStr::new(""), None);
        assert_eq!(
            t.path_with_stage2_first(OsStr::new("/usr/bin")),
            OsString::from("/usr/bin")
        );

        exec_stub(&tmp.join("targo"));
        let t = Toolchain::discover(Some(&tmp), Path::new("/unused"), OsStr::new(""), None);
        let want = format!("{}:/usr/bin", t.stage2_dir.display());
        assert_eq!(
            t.path_with_stage2_first(OsStr::new("/usr/bin")),
            OsString::from(want)
        );
        fs::remove_dir_all(&tmp).ok();
    }

    #[test]
    fn the_pinned_channel_is_the_one_line_the_manifest_declares() {
        assert_eq!(
            pinned_channel_in("# channel = \"decoy\"\n[toolchain]\nchannel = \"trust\"\n")
                .as_deref(),
            Some("trust"),
            "a commented-out channel must not be mistaken for the pin"
        );
        assert_eq!(pinned_channel_in("[toolchain]\n").as_deref(), None);
        // No pin readable means no check — never "the pin is satisfied".
        assert_eq!(
            pinned_channel(Path::new("/nonexistent-repo")).as_deref(),
            None
        );
    }

    #[test]
    fn a_targo_that_is_not_the_pinned_toolchain_is_refused_not_adopted() {
        // THE WHOLE POINT. A directory carrying a file called `targo` is not
        // evidence that it is the fork `rust-toolchain.toml` pins, and the PATH
        // fallback adopts such a directory sight unseen. Without the branded
        // driver beside it the gate would run a different frontend under the
        // pinned one's name and still print the merge-contract sentence.
        let tmp = crate::mktemp_dir("atv-pin").expect("mktemp");
        let bin = tmp.join("bin");
        fs::create_dir_all(&bin).expect("mkdir");
        exec_stub(&bin.join("targo"));

        let t = Toolchain::discover(
            Some(&bin),
            Path::new("/unused"),
            OsStr::new(""),
            Some("trust"),
        );
        assert!(!t.have_targo(), "fail-closed: not the pinned toolchain");
        assert!(t.refused.is_some());
        let label = t.missing_targo_label();
        assert!(label.contains("rust-toolchain.toml pins"), "{label}");
        assert!(label.contains("promote-toolchain.sh"), "{label}");
        assert!(label.contains("TRUST_STAGE2_BIN"), "{label}");

        // A `trustc` that is PRESENT but cannot answer for itself is the
        // mid-rebuild stage tree, and it must still be refused: `x.py build
        // --stage 2` leaves exactly this shape behind while it runs.
        exec_stub(&bin.join("trustc"));
        let t = Toolchain::discover(
            Some(&bin),
            Path::new("/unused"),
            OsStr::new(""),
            Some("trust"),
        );
        assert!(
            !t.have_targo(),
            "a driver that names no sysroot is not the pin (mid-rebuild stage tree)"
        );
        assert!(t.refused.is_some());

        // The branded rustc beside it, ANSWERING with a real sysroot, is what
        // makes the directory the pin.
        driver_stub(&bin.join("trustc"));
        let t = Toolchain::discover(
            Some(&bin),
            Path::new("/unused"),
            OsStr::new(""),
            Some("trust"),
        );
        assert!(t.have_targo());
        assert!(t.refused.is_none());

        // An UPSTREAM channel ships no branded driver, so it passes through —
        // the check must not invent a `stablec` nobody has.
        fs::remove_file(bin.join("trustc")).expect("rm");
        let t = Toolchain::discover(
            Some(&bin),
            Path::new("/unused"),
            OsStr::new(""),
            Some("stable"),
        );
        assert!(t.have_targo(), "upstream channels are a passthrough");
        fs::remove_dir_all(&tmp).ok();
    }

    #[test]
    fn the_path_fallback_adopts_only_the_pinned_toolchain() {
        // The golden path walks PATH looking for a targo. It must walk PAST one
        // that is not the pin rather than stopping at it.
        let tmp = crate::mktemp_dir("atv-pinpath").expect("mktemp");
        let impostor = tmp.join("impostor");
        let real = tmp.join("stage/bin");
        fs::create_dir_all(&impostor).expect("mkdir");
        fs::create_dir_all(&real).expect("mkdir");
        exec_stub(&impostor.join("targo"));
        exec_stub(&real.join("targo"));
        driver_stub(&real.join("trustc"));
        let path = format!("{}:{}", impostor.display(), real.display());

        let t = Toolchain::discover(
            None,
            Path::new("/nonexistent-home"),
            OsStr::new(&path),
            Some("trust"),
        );
        assert!(t.have_targo());
        assert_eq!(t.stage2_dir, fs::canonicalize(&real).expect("canonicalize"));

        // Impostor alone: refused, and the diagnostic names it.
        let t = Toolchain::discover(
            None,
            Path::new("/nonexistent-home"),
            OsStr::new(impostor.to_string_lossy().as_ref()),
            Some("trust"),
        );
        assert!(!t.have_targo());
        assert_eq!(
            t.refused,
            Some(fs::canonicalize(&impostor).expect("canonicalize"))
        );
        fs::remove_dir_all(&tmp).ok();
    }

    #[test]
    fn a_directory_named_targo_is_not_a_driver() {
        let tmp = crate::mktemp_dir("atv-dir").expect("mktemp");
        fs::create_dir_all(tmp.join("targo")).expect("mkdir");
        let t = Toolchain::discover(Some(&tmp), Path::new("/unused"), OsStr::new(""), None);
        assert!(
            !t.have_targo(),
            "fail-closed: a directory is not the verified driver"
        );
        fs::remove_dir_all(&tmp).ok();
    }
}
