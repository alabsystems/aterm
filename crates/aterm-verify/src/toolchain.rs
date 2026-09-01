// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Finding THE toolchain — a SEALED, PROMOTED toolchain directory, which is what
//! `rust-toolchain.toml`'s `trust` pin actually resolves to. A LIVE BUILD TREE IS
//! NOT A TOOLCHAIN: `x.py build --stage 2` empties the stage bins and refills
//! them only at the end, so a discovery order that reaches into `$HOME/trust/build`
//! first is broken for the entire length of every rebuild. Measured 2026-08-30
//! mid-rebuild on the dev box: `$HOME/trust/build/host/stage2/bin` held `targo` and
//! `targo-trust` — no `trustc`, and no sibling `lib/` at all. The build tree stays
//! reachable, explicitly, and LAST.
//!
//! Three rules carried over from the script, all load-bearing:
//!
//! 1. RESOLVE THE PHYSICAL PATH. `build/host` is commonly a target-triple
//!    symlink and Trust's drivers reject a symlinked toolchain path, so the
//!    stage2 directory is canonicalised before anything selects a tool out of it
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
//! 4. THE STORE IS A CANDIDATE, AND IT COMES BEFORE THE SOURCE TREE. The
//!    ordinary way to get the pinned toolchain is `aterm pkg install trust`,
//!    which lays it down under the atpkg prefix as `store/trust/current/bin`
//!    (the per-program live-build link atpkg flips on every update). That
//!    directory is probed right after an explicit `$TRUST_STAGE2_BIN` and
//!    BEFORE `$HOME/trust/build/host/stage2/bin`, the from-source developer
//!    alternative — the same order `aterm-release`'s gates use. The prefix is
//!    resolved the way atpkg resolves it ([`atpkg_prefix`]: `[packages].prefix`
//!    from aterm.toml, else the platform default) as a dependency-free MIRROR,
//!    because this crate has no dependencies by charter (see its Cargo.toml).
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

/// The `bin` of the sysroot `rustc` resolves to under `path_env`.
///
/// THIS IS HOW A rustup-LINKED PIN IS REACHED. `rust-toolchain.toml` is honoured
/// only by the rustup shim, so on a machine with no `$HOME/trust` checkout — where
/// the pinned toolchain is a `rustup toolchain link` into some build tree — the
/// stage2 default and the PATH scan both come up empty while the pinned drivers
/// are one `rustc --print sysroot` away. Canonicalised for the same reason
/// everything else here is: the link makes every path through rustup
/// non-canonical, and Trust's drivers refuse a symlinked toolchain path.
///
/// PATH is set explicitly rather than inherited so the probe answers about the
/// same environment the children will run in — and so a caller with an empty
/// `path_env` gets `None` instead of this process's own PATH.
fn sysroot_bin(path_env: &OsStr) -> Option<PathBuf> {
    let out = std::process::Command::new("rustc")
        .arg("--print")
        .arg("sysroot")
        .env("PATH", path_env)
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let sysroot = String::from_utf8(out.stdout).ok()?;
    let sysroot = PathBuf::from(sysroot.trim());
    if sysroot.as_os_str().is_empty() {
        return None;
    }
    let dir = std::fs::canonicalize(&sysroot)
        .unwrap_or(sysroot)
        .join("bin");
    dir.is_dir().then_some(dir)
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
    /// The atpkg store candidate that was probed (`store/trust/current/bin` under
    /// the resolved prefix), whether or not it held anything — so the diagnostic
    /// can name the place `aterm pkg install trust` would have filled.
    pub store_bin: Option<PathBuf>,
}

impl Toolchain {
    /// `$TRUST_STAGE2_BIN`, else the first PROMOTED toolchain under `home` that
    /// satisfies the pin, else the atpkg store's `store/trust/current/bin`, else
    /// `$HOME/trust/build/host/stage2/bin`, canonicalised when it exists.
    ///
    /// SEALED FIRST, BUILD TREE LAST. The default used to be
    /// `$HOME/trust/build/host/stage2/bin` outright — a live build tree, checked
    /// before anything finished. See the module header for why that is broken for
    /// the whole length of every rebuild. The order is now
    /// `~/toolchains/<channel>-current/bin` (the sealed promote target, moved
    /// atomically by `trust/scripts/promote-toolchain.sh` and nothing else), then
    /// `~/.rustup/toolchains/<channel>/bin` (the rustup link — the seam atpkg
    /// lays and re-asserts into its store, and an independent spelling that
    /// survives a layout change on either side), then the atpkg store itself,
    /// then the stage2 tree. A candidate must carry a `targo` AND be the pin to
    /// win; when none does, the stage2 path stays as the reported location so
    /// the "no targo" diagnosis keeps naming a place the reader recognises.
    ///
    /// THE STORE COMES AHEAD OF THE BUILD TREE among the defaults (only the
    /// promoted spellings above outrank it): `aterm pkg install trust` is the
    /// ordinary way to have the pinned toolchain at all, and the store's
    /// live-build link is the one path that keeps pointing at it across
    /// updates. The prefix is [`atpkg_prefix`] — `$XDG_CONFIG_HOME` is the one
    /// environment read here, for the config file atpkg itself reads; callers
    /// that hold a snapshot use [`Self::discover_with_store`].
    ///
    /// GOLDEN-PATH FALLBACK: when neither default tree carries a targo and no
    /// explicit `$TRUST_STAGE2_BIN` named one, the drivers are looked up on
    /// `path_env` — a machine whose store lives at a prefix this mirror cannot
    /// see still reaches its toolchain through PATH (shell.d, or the
    /// `tools/verify.sh` wrapper, which prepends the store's shim dir itself).
    /// Positional stage2-only discovery left every stage on such a machine
    /// skipping with "no targo" — the same skew class the aterm-grid compile
    /// probe had. An EXPLICIT override never falls back: naming a toolchain
    /// that is not there is an error to surface, not a preference to route
    /// around. `pinned` is the channel `rust-toolchain.toml` names
    /// ([`pinned_channel`]); `None` disables the check and restores the
    /// pre-guard behaviour, which is what the pure unit tests below want and
    /// what a repo with no pin means.
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

    /// [`Self::discover`] with the atpkg prefix supplied by the caller (`None`
    /// skips the store probe entirely). The resolution order, first hit wins:
    ///
    /// 1. `stage2_bin` — an explicit override; checked, never fallen back from.
    /// 2. `<home>/toolchains/<channel>-current/bin` — the sealed promote target.
    /// 3. `<home>/.rustup/toolchains/<channel>/bin` — the rustup link (the seam
    ///    atpkg lays into its store).
    /// 4. `<store_prefix>/store/trust/current/bin` — the atpkg store.
    /// 5. `<home>/trust/build/host/stage2/bin` — a from-source stage2.
    /// 6. every `path_env` directory, then the sysroot `rustc --print sysroot`
    ///    names (how a rustup-LINKED pin is reached).
    ///
    /// Every candidate must BE the pin (`is_pinned_toolchain`, private to this
    /// module); the first one
    /// that carries a targo and is not gets remembered in `refused` for the
    /// diagnostic, and the search goes on past it.
    #[must_use]
    pub fn discover_with_store(
        stage2_bin: Option<&Path>,
        home: &Path,
        store_prefix: Option<&Path>,
        path_env: &OsStr,
        pinned: Option<&str>,
    ) -> Self {
        let explicit = stage2_bin.is_some();
        let stage2_tree = home.join("trust/build/host/stage2/bin");
        let store_bin = (!explicit)
            .then(|| store_prefix.map(store_stage2_bin))
            .flatten();
        // The promoted spellings, ahead of everything but an explicit override:
        // the sealed promote target, then the rustup link (the seam atpkg lays
        // into its store). A hit here is final — the store probe below stands
        // down for it.
        let channel_hit = (!explicit)
            .then(|| {
                let channel = pinned.unwrap_or("trust");
                [
                    home.join(format!("toolchains/{channel}-current/bin")),
                    home.join(format!(".rustup/toolchains/{channel}/bin")),
                ]
                .into_iter()
                .find(|d| is_executable_file(&d.join("targo")) && is_pinned_toolchain(d, pinned))
            })
            .flatten();
        let settled_by_channel = channel_hit.is_some();
        let declared = stage2_bin
            .map(Path::to_path_buf)
            .or(channel_hit)
            .unwrap_or_else(|| stage2_tree.clone());
        let mut tool_dir = if declared.is_dir() {
            std::fs::canonicalize(&declared).unwrap_or(declared)
        } else {
            declared
        };
        // The declared directory is checked too, not just the fallbacks:
        // `$TRUST_STAGE2_BIN` is an ordinary environment variable and can name a
        // tree that is not the pin as easily as PATH can. A refused directory
        // stays in `stage2_dir` (it is what the operator asked for, and the
        // diagnostic has to name it) but `refused` makes `have_targo` answer no,
        // so every stage takes the same fail-closed branch as an absent one.
        let mut refused = (is_executable_file(&tool_dir.join("targo"))
            && !is_pinned_toolchain(&tool_dir, pinned))
        .then(|| tool_dir.clone());
        // The store, AHEAD of the source-tree default: a pinned targo there wins
        // outright; a targo there that is not the pin is refused and remembered,
        // exactly like a PATH impostor.
        let mut settled = settled_by_channel;
        if let Some(store) = &store_bin
            && !settled
            && is_executable_file(&store.join("targo"))
        {
            let store = std::fs::canonicalize(store).unwrap_or_else(|_| store.clone());
            if is_pinned_toolchain(&store, pinned) {
                tool_dir = store;
                refused = None;
                settled = true;
            } else {
                refused.get_or_insert(store);
            }
        }
        if !explicit
            && !settled
            && (refused.is_some() || !is_executable_file(&tool_dir.join("targo")))
        {
            // PATH next (the golden path for a store this mirror cannot see: a
            // store-provisioned machine reaches its toolchain that way), then
            // the rustup-linked sysroot. Every candidate must BE the pin; the
            // first one that carries a targo and is not gets remembered for the
            // diagnostic.
            // `once_with` keeps the sysroot probe LAZY: it spawns a process, and a
            // PATH hit must not pay for a candidate it never needed.
            let from_path = std::env::split_paths(path_env);
            let sysroot = std::iter::once_with(|| sysroot_bin(path_env)).flatten();
            for dir in from_path.chain(sysroot) {
                if !is_executable_file(&dir.join("targo")) {
                    continue;
                }
                let dir = std::fs::canonicalize(&dir).unwrap_or(dir);
                if is_pinned_toolchain(&dir, pinned) {
                    tool_dir = dir;
                    refused = None;
                    break;
                }
                refused.get_or_insert(dir);
            }
        }
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
    /// package index is the ordinary source of the pinned toolchain; building
    /// one from source is the developer alternative, named second.
    pub const INSTALL_REMEDY: &'static str = "`aterm pkg install trust` (then `aterm pkg doctor` \
         to confirm the store); from source instead: python3 x.py build --stage 2 in $HOME/trust";

    /// The diagnostic for a stage2 that is absent — or mid-rebuild, which empties
    /// the directory and refills it at the end.
    #[must_use]
    pub fn missing_targo_label(&self) -> String {
        if let Some(dir) = &self.refused {
            return format!(
                "targo at {} is NOT the toolchain rust-toolchain.toml pins (no branded rustc \
                 beside it), so running the gate there would use a different frontend and a \
                 different lint set under the pinned one's name. Refusing. Fix: {} — the gate \
                 finds the store on its own; or link the pinned toolchain and put the rustup \
                 shim first on PATH — `rustup toolchain link trust <stage-sysroot>` then \
                 `export PATH=\"$HOME/.cargo/bin:$PATH\"` — or point TRUST_STAGE2_BIN at the \
                 real stage2 bin",
                dir.display(),
                Self::INSTALL_REMEDY,
            );
        }
        let store = self.store_bin.as_ref().map_or_else(String::new, |s| {
            format!(" nor in the atpkg store at {}", s.display())
        });
        format!(
            "targo not found at {}{store}. Fix: {}; or set TRUST_STAGE2_BIN (a rustup-linked pin \
             is found through `rustc --print sysroot` when ~/.cargo/bin is on PATH)",
            self.targo.display(),
            Self::INSTALL_REMEDY,
        )
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
    use std::os::unix::fs::PermissionsExt;

    fn exec_stub(path: &Path) {
        fs::write(path, b"#!/bin/sh\nexit 0\n").expect("write");
        fs::set_permissions(path, fs::Permissions::from_mode(0o755)).expect("chmod");
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
        fs::set_permissions(path, fs::Permissions::from_mode(0o755)).expect("chmod");
    }

    #[test]
    fn the_reported_location_when_nothing_resolves_is_the_stage2_tree_under_home() {
        // Not a statement about PREFERENCE — the sealed toolchain is preferred,
        // see `the_sealed_toolchain_is_preferred_over_the_live_build_tree`. This
        // pins what the gate REPORTS when no candidate exists at all: the stage2
        // path, because that is the place the "no targo" remedy talks about.
        let t = Toolchain::discover(None, Path::new("/nonexistent-home"), OsStr::new(""), None);
        assert_eq!(
            t.targo,
            Path::new("/nonexistent-home/trust/build/host/stage2/bin/targo")
        );
        assert_eq!(
            t.trustdoc,
            Path::new("/nonexistent-home/trust/build/host/stage2/bin/trustdoc")
        );
        assert!(!t.have_targo());
        assert!(t.tippy.is_none());
    }

    #[test]
    fn the_missing_trustdoc_diagnosis_names_the_config_key_and_both_remedies() {
        let t = Toolchain::discover(None, Path::new("/nonexistent-home"), OsStr::new(""), None);
        let label = t.missing_trustdoc_label();
        assert!(label.contains("x.py build --stage 2"), "{label}");
        assert!(label.contains("~/.local/bin/trustdoc"), "{label}");
        assert!(label.contains(".cargo/config.toml"), "{label}");
    }

    #[test]
    fn every_remedy_leads_with_the_package_manager_and_names_source_second() {
        // The signed index is the ordinary way to have the toolchain; x.py is the
        // developer alternative. Every diagnostic says them in that order.
        let t = Toolchain::discover(None, Path::new("/nonexistent-home"), OsStr::new(""), None);
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

    #[test]
    fn the_atpkg_store_is_probed_ahead_of_the_from_source_default() {
        // `aterm pkg install trust` lays the toolchain down at
        // <prefix>/store/trust/current/bin, with `current` a symlink at the
        // numbered build — exactly the shape built here, under a fake HOME.
        let home = crate::mktemp_dir("atv-store").expect("mktemp");
        let prefix = default_atpkg_prefix(&home);
        let build = prefix.join("store/trust/6808/bin");
        fs::create_dir_all(&build).expect("mkdir");
        exec_stub(&build.join("targo"));
        driver_stub(&build.join("trustc"));
        std::os::unix::fs::symlink(
            prefix.join("store/trust/6808"),
            prefix.join("store/trust/current"),
        )
        .expect("ln current");
        // A from-source stage2 beside it, ALSO pinned — the store must still win.
        let source = home.join("trust/build/host/stage2/bin");
        fs::create_dir_all(&source).expect("mkdir");
        exec_stub(&source.join("targo"));
        driver_stub(&source.join("trustc"));

        let t = Toolchain::discover_with_store(
            None,
            &home,
            Some(&prefix),
            OsStr::new(""),
            Some("trust"),
        );
        assert!(t.have_targo());
        assert_eq!(
            t.stage2_dir,
            fs::canonicalize(&build).expect("canonicalize"),
            "the store's live build, resolved to its physical numbered dir"
        );
        assert_eq!(
            t.store_bin.as_deref(),
            Some(store_stage2_bin(&prefix).as_path())
        );

        // Without the store the from-source default is what it always was.
        let t = Toolchain::discover_with_store(None, &home, None, OsStr::new(""), Some("trust"));
        assert!(t.have_targo());
        assert_eq!(
            t.stage2_dir,
            fs::canonicalize(&source).expect("canonicalize")
        );
        assert!(t.store_bin.is_none());

        // An EXPLICIT override never consults the store.
        let t = Toolchain::discover_with_store(
            Some(&source),
            &home,
            Some(&prefix),
            OsStr::new(""),
            Some("trust"),
        );
        assert_eq!(
            t.stage2_dir,
            fs::canonicalize(&source).expect("canonicalize")
        );
        assert!(t.store_bin.is_none());
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
        assert!(label.contains("rustup toolchain link"), "{label}");
        assert!(label.contains("$HOME/.cargo/bin:$PATH"), "{label}");

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
    fn the_sealed_toolchain_is_preferred_over_the_live_build_tree() {
        // THE ITEM 2 REGRESSION NET. Both directories are the pin; the sealed one
        // must win, because the build tree is empty for the whole length of every
        // `x.py build --stage 2` and a gate that prefers it is dead while a
        // rebuild runs.
        let home = crate::mktemp_dir("atv-order").expect("mktemp");
        let sealed = home.join("toolchains/trust-current/bin");
        let tree = home.join("trust/build/host/stage2/bin");
        for d in [&sealed, &tree] {
            fs::create_dir_all(d).expect("mkdir");
            exec_stub(&d.join("targo"));
            driver_stub(&d.join("trustc"));
        }

        let t = Toolchain::discover(None, &home, OsStr::new(""), Some("trust"));
        assert!(t.have_targo());
        assert_eq!(
            t.stage2_dir,
            fs::canonicalize(&sealed).expect("canonicalize")
        );

        // With the sealed one gone, the build tree is still reachable — the
        // fallback is explicit, not deleted.
        fs::remove_dir_all(home.join("toolchains")).expect("rm");
        let t = Toolchain::discover(None, &home, OsStr::new(""), Some("trust"));
        assert!(t.have_targo());
        assert_eq!(t.stage2_dir, fs::canonicalize(&tree).expect("canonicalize"));

        // And a build tree that is MID-REBUILD (targo relinked, no answering
        // driver) fails closed rather than becoming the gate's compiler.
        fs::remove_file(tree.join("trustc")).expect("rm");
        let t = Toolchain::discover(None, &home, OsStr::new(""), Some("trust"));
        assert!(!t.have_targo(), "fail-closed on a half-built stage2");
        fs::remove_dir_all(&home).ok();
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
