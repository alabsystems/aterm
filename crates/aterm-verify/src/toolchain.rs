// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Finding THE toolchain — the directory `rust-toolchain.toml`'s `trust` pin
//! actually resolves to. ONE resolution order, and `aterm-release`'s
//! `gates::trust_stage2_bin` walks the same one (it cannot reach this crate
//! without a new edge, so it mirrors it and says so):
//!
//! 1. `$TRUST_STAGE2_BIN` — an explicit development override, never fallen back from;
//! 2. the rustup toolchain the pin names, `<rustup home>/toolchains/<channel>` — rustup's
//!    home is `$RUSTUP_HOME`, else `~/.rustup` ([`rustup_home`]) — atpkg lays
//!    it as a view of its store, and Trust's `scripts/promote-toolchain.sh` points it at
//!    a SEALED from-source build, the sanctioned way to drive one (never the live tree);
//! 3. the atpkg store's `store/trust/current/bin`;
//! 4. `PATH`.
//!
//! EXCEPT that a rustup `trust` OLDER than the store's build ranks BELOW the store (2 and
//! 3 swap). That is atpkg's one staleness rule — `atpkg::seam::stale_against_store`,
//! mirrored here as [`Demoted`] — and the entry it catches is not hypothetical: measured
//! 2026-09-24 on the owner's Mac, `~/.rustup/toolchains/trust` was a hand-made link to
//! `$HOME/trust/build/host/stage2` (commit-date 2026-08-20) while the store held 9192
//! (2026-09-17), so every gate run without a hand-exported `$TRUST_STAGE2_BIN` built,
//! formatted and linted with a five-week-old compiler — and, that tree shipping no
//! `targo-tippy`, skipped tippy and could not mint a merge-contract receipt. atpkg's
//! unattended pass refused to re-point a link it did not lay, and only `aterm pkg repair`
//! healed it, so no toolchain update ever reached the gates. Since 2026-09-26 that pass
//! re-points this exact shape by itself — an older LIVE BUILD TREE
//! (`atpkg::seam::live_build_tree`) — but not a seal, a link put back after it, or one a
//! build is running through, so the demotion still has work to do. Demoted, not deleted: a
//! store that is not the pin (rule 5) still falls through to the rustup entry, which is
//! what the gate ran before; and an entry whose age cannot be read is never demoted.
//!
//! A LIVE BUILD TREE IS NOT A CANDIDATE. `$HOME/trust/build/host/stage2/bin` (and the
//! `~/toolchains/<channel>-current` promote target) were probed here until
//! 2026-09-24, ahead of the store, a month after both were retired from the
//! delivery (2026-08-29). On m7 that made `aterm help rust` report a JULY stage2 —
//! one whose `targo` no longer knows `--unverified` — as "the gates' toolchain"
//! while PATH ran the store's. A build tree is also empty for the whole length of
//! every `x.py build --stage 2`. Not PROBED is not unreachable: a hand-made rustup
//! link can still name one, which is the case the exception above exists for.
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
//! 4. ATPKG'S VIEW IS READ THROUGH (2026-09-28). When the rustup entry resolves into
//!    atpkg's own view of its store (`<prefix>/rustup/…`), the candidate is the build the
//!    view PRESENTS — `store/trust/current`, or a dev-linked sysroot checkout — never the
//!    view's own clones, exactly as `aterm-release`'s `gates::rustup_entry_source` reads
//!    it ([`view_presents`]). The receipt names the directory a run built with, and the
//!    cutter refuses a MEASURE receipt naming another than its own: a gate that built
//!    through the view named `<prefix>/rustup/trust/bin` while the cut built with
//!    `<prefix>/store/trust/<build>/bin`, so on a machine whose rustup `trust` is atpkg's
//!    view the cutter's own remedy — `tools/verify.sh --measure` — filed a receipt the
//!    cut then refused (v0.97.0, 2026-09-28).
//!
//! 5. THE STORE PREFIX IS A MIRROR, AND SO IS RUSTUP'S HOME. The prefix is
//!    resolved the way atpkg resolves it ([`atpkg_prefix`]: `[packages].prefix`
//!    from aterm.toml, else the platform default), and rustup's home the way
//!    `atpkg::seam::rustup_home_with` and `aterm-release` resolve it
//!    ([`rustup_home`]: a non-empty `$RUSTUP_HOME`, else `~/.rustup`), both
//!    without a dependency edge, because this crate has none by charter (see its
//!    Cargo.toml). `aterm-cli`'s tests, which reach both crates, hold the two
//!    rustup-home spellings to one table.
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
use std::time::Duration;

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
/// no upstream toolchain ships. PUBLIC BOUNDARY: this crate ships in the public
/// snapshot, whose export swaps in a stock pin (publish/transforms.sh); the dev
/// tree pins `trust`, so this arm is never taken here.
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

/// Rustup's home: a non-empty `$RUSTUP_HOME` (passed in as `env_rustup_home`), else
/// `<home>/.rustup` — rustup's own rule, and the mirror of
/// `atpkg::seam::rustup_home_with`, which `aterm-release` resolves through (rule 4 of
/// the module header). A SET-BUT-EMPTY `$RUSTUP_HOME` is unset here, as it is there:
/// `RUSTUP_HOME= cmd` means "the default", never "the current directory".
///
/// `tools/verify.sh` and `tools/test-trust-contract-probe.sh` spell the same rule
/// `${RUSTUP_HOME:-$HOME/.rustup}`, whose `:-` is the empty-is-unset half.
#[must_use]
pub fn rustup_home(env_rustup_home: Option<&OsStr>, home: &Path) -> PathBuf {
    match env_rustup_home {
        Some(v) if !v.is_empty() => PathBuf::from(v),
        _ => home.join(".rustup"),
    }
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

/// The channel the atpkg store holds, and so the one rustup entry [`Demoted`] can be
/// weighed against (`atpkg::seam::SEAM_PROGRAM`).
const STORE_CHANNEL: &str = "trust";

/// How long ONE `-vV` date probe may take: `atpkg::doctor`'s `PROBE_TIMEOUT`, mirrored, so
/// both copies of the one staleness rule give up at the same point. A wedged driver costs
/// the date — read as "unknown", which never demotes — never the gate.
const DATE_PROBE_BOUND: Duration = Duration::from_secs(5);

/// A rustup `trust` toolchain the walk ranked BELOW the atpkg store because it is older
/// than the store's build (the module header's exception to the order).
///
/// THE RULE IS ATPKG'S, mirrored without a dependency edge (this crate has none, by
/// charter): `atpkg::seam::stale_against_store` — the rule `aterm pkg doctor` warns by and
/// `aterm pkg repair` re-points by — so the gate never ranks a toolchain differently from
/// the verb that names the fix. Its three parts:
///
/// * the `commit-date:` each compiler's `-vV` reports (`bin/trustc`, else `bin/rustc`),
///   a `YYYY-MM-DD` date, compared as text; OLDER is strictly before, so two builds of one
///   day are not told apart and the entry keeps its rank;
/// * an age that cannot be read — no answer inside `DATE_PROBE_BOUND`, no date line, a
///   date of another shape — is never read as stale;
/// * a dev-linked trust (`<prefix>/links/trust`, atpkg's link marker) presents its
///   checkout, not the store, so nothing is stale against the store then. The mirror
///   reads the marker's PRESENCE only; atpkg also treats an unreadable marker as no link,
///   and there the mirror keeps the old order, the direction that changes nothing.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Demoted {
    /// The rustup entry's physical `bin` directory.
    pub dir: PathBuf,
    /// Its compiler's `commit-date`.
    pub its: String,
    /// The store build's `commit-date`.
    pub store: String,
    /// Whether the rustup entry (`~/.rustup/toolchains/trust` itself) is a SYMLINK — the
    /// one shape `aterm pkg repair` re-points. A real directory it refuses ([`Self::remedy`]).
    pub linked: bool,
}

/// The fix for a demoted entry that is a LINK: atpkg's `repair` replaces a stale foreign
/// link with its view (`atpkg::seam::repair`).
const REMEDY_LINK: &str = "`aterm pkg repair` re-points it at the store";

/// The fix for a demoted entry that is a REAL DIRECTORY. `aterm pkg repair` refuses an
/// entry that is not a link, whatever its age — a directory atpkg did not lay is not its
/// to delete — and names `atpkg::seam::DETACH_FIX` instead, restated here (this crate has
/// no atpkg edge; aterm-cli's tests hold the two spellings together).
pub const REMEDY_DIRECTORY: &str = "`aterm pkg repair` will not replace a directory it did \
     not lay: remove that entry yourself (e.g. `rustup toolchain uninstall trust`), then \
     `aterm pkg repair`";

impl Demoted {
    /// The verb that fixes it, for the entry's SHAPE: `aterm pkg repair` for a link, and
    /// the by-hand removal first for a real directory, which repair refuses — naming repair
    /// alone there sent a reader to a command that answers "refusing to touch it". atpkg's
    /// own view is never demoted: it is read through to the build it presents
    /// ([`view_presents`]).
    #[must_use]
    pub fn remedy(&self) -> &'static str {
        if self.linked {
            REMEDY_LINK
        } else {
            REMEDY_DIRECTORY
        }
    }

    /// The one sentence every surface says it in — the gate's header and `aterm help rust`.
    #[must_use]
    pub fn sentence(&self) -> String {
        format!(
            "rustup `trust` ({}) is a toolchain from {}, older than the atpkg store's {}: \
             ranked below the store, not used — {}",
            self.dir.display(),
            self.its,
            self.store,
            self.remedy()
        )
    }
}

/// The `commit-date:` the compiler in `bin` reports — `trustc -vV`, else `rustc -vV` (a
/// stage2 from before the Trust names) — when it is a `YYYY-MM-DD` date and the driver
/// answered inside [`DATE_PROBE_BOUND`]. The mirror of `atpkg::seam::commit_date`.
fn commit_date(bin: &Path) -> Option<String> {
    ["trustc", "rustc"]
        .iter()
        .map(|n| bin.join(n))
        .filter(|p| is_executable_file(p))
        .find_map(|driver| {
            let text = bounded_stdout(&driver, &["-vV"], DATE_PROBE_BOUND)?;
            let date = text
                .lines()
                .find_map(|l| l.strip_prefix("commit-date: "))?
                .trim()
                .to_string();
            let shaped = date.len() == 10
                && date.bytes().enumerate().all(|(i, b)| {
                    if i == 4 || i == 7 {
                        b == b'-'
                    } else {
                        b.is_ascii_digit()
                    }
                });
            shaped.then_some(date)
        })
}

/// `program args…`'s stdout when it exits 0 inside `bound`; `None` otherwise, the child
/// killed and reaped on the way out. POLLED with `try_wait`, for the reason
/// [`crate::exec`]'s ceiling is: std has no wait-with-deadline and this crate takes no
/// `libc`. A version answer is one short block, far inside a pipe buffer, so the child
/// cannot wedge on a full pipe before it exits.
fn bounded_stdout(program: &Path, args: &[&str], bound: Duration) -> Option<String> {
    use std::io::Read as _;
    use std::process::Stdio;
    let mut child = std::process::Command::new(program)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    let started = std::time::Instant::now();
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if started.elapsed() < bound => {
                std::thread::sleep(Duration::from_millis(10));
            }
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
        }
    };
    let mut out = String::new();
    child.stdout.take()?.read_to_string(&mut out).ok()?;
    status.success().then_some(out)
}

/// `(its, store)` commit-dates when the rustup `bin` at `rustup` is OLDER than the build
/// `<store_prefix>/store/trust/current` names — [`Demoted`]'s rule. `None` while trust is
/// dev-linked, without a store build, or when either age cannot be read.
fn older_than_store(rustup: &Path, store_prefix: &Path) -> Option<(String, String)> {
    if std::fs::symlink_metadata(store_prefix.join("links").join(STORE_CHANNEL)).is_ok() {
        return None;
    }
    let store = store_stage2_bin(store_prefix);
    std::fs::metadata(&store).ok()?;
    let its = commit_date(rustup)?;
    let theirs = commit_date(&store)?;
    (its < theirs).then_some((its, theirs))
}

/// The `bin` directory the rustup `entry` stands for when it resolves into atpkg's OWN view
/// of its store (`<store_prefix>/rustup/…`) — the build the view PRESENTS, as
/// `aterm-release`'s `gates::rustup_entry_source` reads it through `atpkg::seam::view_source`
/// (mirrored: this crate has no atpkg edge): a dev-linked trust (`<prefix>/links/trust`
/// naming a sysroot checkout outside the prefix) presents that checkout, and otherwise the
/// view presents `store/trust/current` — including a link atpkg cannot present, where the
/// cutter skips the entry and the store is next. A marker that cannot be read is no link,
/// as atpkg reads it. `None` when the entry is not the view: it is a candidate as it is.
fn view_presents(entry: &Path, store_prefix: &Path) -> Option<PathBuf> {
    let resolved = std::fs::canonicalize(entry).ok()?;
    let views = std::fs::canonicalize(store_prefix.join("rustup")).ok()?;
    if !resolved.starts_with(&views) {
        return None;
    }
    let store = || Some(store_stage2_bin(store_prefix));
    let Ok(marker) = std::fs::read_to_string(store_prefix.join("links").join(STORE_CHANNEL)) else {
        return store();
    };
    let Some(checkout) = marker.lines().find_map(|line| {
        let value = line
            .trim()
            .strip_prefix("checkout")?
            .trim_start()
            .strip_prefix('=')?;
        let value = value.trim().strip_prefix('"')?.strip_suffix('"')?;
        (!value.contains(['"', '\\'])).then(|| PathBuf::from(value))
    }) else {
        return store();
    };
    let prefix = std::fs::canonicalize(store_prefix).unwrap_or_else(|_| store_prefix.into());
    let inside = std::fs::canonicalize(&checkout).is_ok_and(|c| c.starts_with(&prefix));
    if !inside && checkout.join("bin").is_dir() && checkout.join("lib").is_dir() {
        Some(checkout.join("bin"))
    } else {
        store()
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Toolchain {
    /// The resolved (physical) stage2 `bin` directory.
    pub stage2_dir: PathBuf,
    pub targo: PathBuf,
    pub trustdoc: PathBuf,
    /// `targo-tippy`, if installed. The pre-rebrand `targo-clippy` is not a
    /// candidate: no shipped toolchain carries it, and `atpkg-pack-bundle.sh`
    /// lists it among the retired public names.
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
    /// The rustup entry the walk RANKED BELOW the store because it is older than the
    /// store's build ([`Demoted`]) — `Some` only when that changed the answer (another
    /// directory won), so the header and `aterm help rust` can say why the rustup
    /// toolchain a reader expects is not the one the gates run.
    pub demoted: Option<Demoted>,
}

impl Toolchain {
    /// [`Self::discover_with_store`] under the atpkg prefix atpkg itself would resolve
    /// ([`atpkg_prefix`]) and the rustup home rustup itself would ([`rustup_home`]);
    /// `$XDG_CONFIG_HOME` (for the config file atpkg reads) and `$RUSTUP_HOME` are the
    /// two environment reads here. `pinned` is the channel `rust-toolchain.toml` names
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
        let rustup = rustup_home(std::env::var_os("RUSTUP_HOME").as_deref(), home);
        Self::discover_with_store(stage2_bin, &rustup, Some(&prefix), path_env, pinned)
    }

    /// THE resolution order (the module header), first hit wins:
    ///
    /// 1. `stage2_bin` — `$TRUST_STAGE2_BIN`; checked, never fallen back from: naming a
    ///    toolchain that is not there is an error to surface, not a preference.
    /// 2. `<rustup_home>/toolchains/<channel>/bin` — the rustup toolchain the pin names;
    ///    `rustup_home` is rustup's home itself ([`rustup_home`] resolves it), not `$HOME`.
    /// 3. `<store_prefix>/store/trust/current/bin` — the atpkg store (`None` skips it).
    /// 4. every `path_env` directory.
    ///
    /// 2 and 3 SWAP when the rustup entry carries a `targo` and is older than the store's
    /// build ([`Demoted`]); `demoted` then names it whenever another directory won.
    ///
    /// A candidate must carry a `targo` AND be the pin (`is_pinned_toolchain`); the
    /// first one that carries a targo and is not is remembered in `refused` for the
    /// diagnostic, and the search goes on past it. When nothing qualifies, the store
    /// (else the rustup entry) is the reported location, because that is where the
    /// remedy puts one.
    #[must_use]
    pub fn discover_with_store(
        stage2_bin: Option<&Path>,
        rustup_home: &Path,
        store_prefix: Option<&Path>,
        path_env: &OsStr,
        pinned: Option<&str>,
    ) -> Self {
        let physical = |dir: PathBuf| std::fs::canonicalize(&dir).unwrap_or(dir);
        let mut refused = None;
        let mut demoted = None;
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
            let entry = rustup_home
                .join("toolchains")
                .join(pinned.unwrap_or("trust"));
            // atpkg's view is read through to the build it presents (rule 4).
            let rustup = store_prefix
                .and_then(|prefix| view_presents(&entry, prefix))
                .unwrap_or_else(|| entry.join("bin"));
            let store_bin = store_prefix.map(store_stage2_bin);
            // The date probes run only where there is a rustup entry to weigh — a
            // store-only machine spawns nothing new here.
            let stale = store_prefix
                .filter(|_| pinned.unwrap_or(STORE_CHANNEL) == STORE_CHANNEL)
                .filter(|_| is_executable_file(&rustup.join("targo")))
                .and_then(|prefix| older_than_store(&rustup, prefix));
            let head: Vec<PathBuf> = match (&stale, &store_bin) {
                (Some(_), Some(store)) => vec![store.clone(), rustup.clone()],
                _ => std::iter::once(rustup.clone())
                    .chain(store_bin.clone())
                    .collect(),
            };
            let candidates = head.into_iter().chain(std::env::split_paths(path_env));
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
            // Named only when it changed the answer: a demoted entry that won anyway (the
            // store was not the pin) is simply the toolchain, as it was before the rule.
            let rustup_dir = physical(rustup.clone());
            let linked =
                std::fs::symlink_metadata(&entry).is_ok_and(|m| m.file_type().is_symlink());
            demoted = stale
                .filter(|_| chosen.as_ref().is_some_and(|c| *c != rustup_dir))
                .map(|(its, store)| Demoted {
                    dir: rustup_dir,
                    its,
                    store,
                    linked,
                });
            let reported = chosen.unwrap_or_else(|| store_bin.clone().unwrap_or(rustup));
            (reported, store_bin)
        };
        let tippy = Some(tool_dir.join("targo-tippy")).filter(|p| is_executable_file(p));
        Self {
            targo: tool_dir.join("targo"),
            trustdoc: tool_dir.join("trustdoc"),
            tippy,
            stage2_dir: tool_dir,
            refused,
            store_bin,
            demoted,
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
        crate::identity::ToolchainIdentity {
            files,
            commit,
            checkers: None,
        }
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
             directly, and `targo --unverified ship provision` can link ~/.local/bin/trustdoc for direct \
             cargo runs",
            self.trustdoc.display(),
            Self::INSTALL_REMEDY,
        )
    }

    #[must_use]
    pub fn missing_tippy_label(&self) -> String {
        format!(
            "tippy lint (no targo-tippy in {} — fix: {})",
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
        let t = Toolchain::discover_with_store(
            None,
            &rustup_home(None, home),
            Some(&prefix),
            OsStr::new(""),
            None,
        );
        assert_eq!(t.targo, store_stage2_bin(&prefix).join("targo"));
        assert_eq!(t.trustdoc, store_stage2_bin(&prefix).join("trustdoc"));
        assert!(!t.have_targo());
        assert!(t.tippy.is_none());
        let t = Toolchain::discover_with_store(
            None,
            &rustup_home(None, home),
            None,
            OsStr::new(""),
            None,
        );
        assert_eq!(
            t.targo,
            Path::new("/nonexistent-home/.rustup/toolchains/trust/bin/targo")
        );
    }

    #[test]
    fn the_missing_trustdoc_diagnosis_names_the_config_key_and_both_remedies() {
        let home = Path::new("/nonexistent-home");
        let prefix = default_atpkg_prefix(home);
        let t = Toolchain::discover_with_store(
            None,
            &rustup_home(None, home),
            Some(&prefix),
            OsStr::new(""),
            None,
        );
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
        let t = Toolchain::discover_with_store(
            None,
            &rustup_home(None, home),
            Some(&prefix),
            OsStr::new(""),
            None,
        );
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
    /// real sysroot — returned as its physical path. Unix, with the test that pins
    /// the symlinked shapes.
    #[cfg(unix)]
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
            Toolchain::discover_with_store(
                explicit,
                &rustup_home(None, &home),
                Some(&prefix),
                path,
                Some("trust"),
            )
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

    /// RUSTUP'S HOME IS `$RUSTUP_HOME` WHEN SET (2026-09-25). This probe read
    /// `~/.rustup` directly while `aterm-release` honoured `$RUSTUP_HOME`, so on a
    /// machine with a relocated rustup the cutter and the gate named two different
    /// toolchains. The rule is rustup's (and `atpkg::seam::rustup_home_with`'s):
    /// a non-empty value wins, an empty one is unset.
    #[test]
    fn rustup_home_is_the_variable_when_set_and_home_dot_rustup_otherwise() {
        let home = Path::new("/h");
        assert_eq!(rustup_home(None, home), Path::new("/h/.rustup"));
        assert_eq!(
            rustup_home(Some(OsStr::new("")), home),
            Path::new("/h/.rustup")
        );
        assert_eq!(
            rustup_home(Some(OsStr::new("/opt/rustup")), home),
            Path::new("/opt/rustup")
        );
    }

    /// …and discovery looks where that answer points: a toolchain linked under a
    /// relocated rustup home is found, and the same entry under `~/.rustup` is not
    /// consulted when the variable names somewhere else. NEGATIVE CONTROL: the
    /// default home finds nothing there.
    #[cfg(unix)]
    #[test]
    fn a_relocated_rustup_home_is_where_the_pinned_toolchain_is_found() {
        let home = crate::mktemp_dir("atv-rustup-home").expect("mktemp");
        let relocated = home.join("elsewhere/rustup");
        let linked = pinned_bin(&home.join("sealed/bin"));
        fs::create_dir_all(relocated.join("toolchains")).expect("mkdir");
        std::os::unix::fs::symlink(home.join("sealed"), relocated.join("toolchains/trust"))
            .expect("ln rustup");
        let found = |rustup: &Path| {
            Toolchain::discover_with_store(None, rustup, None, OsStr::new(""), Some("trust"))
        };
        let t = found(&rustup_home(Some(relocated.as_os_str()), &home));
        assert!(t.have_targo(), "{}", t.stage2_dir.display());
        assert_eq!(t.stage2_dir, linked);
        let t = found(&rustup_home(None, &home));
        assert!(
            !t.have_targo(),
            "the default home holds no entry: {}",
            t.stage2_dir.display()
        );
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
            &rustup_home(None, &home),
            Some(&prefix),
            OsStr::new(real.to_string_lossy().as_ref()),
            Some("trust"),
        );
        assert!(t.have_targo(), "PATH still wins past a refused store");
        assert_eq!(t.stage2_dir, fs::canonicalize(&real).expect("canonicalize"));

        let t = Toolchain::discover_with_store(
            None,
            &rustup_home(None, &home),
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

    /// A toolchain bin under `sysroot` whose `trustc` answers like a real one on BOTH
    /// questions discovery asks: `--print sysroot` (the pin check) and `-vV` (the date,
    /// read from `<sysroot>/commit-date`), beside an executable `targo`. Returned physical.
    ///
    /// Every `trustc` is a HARD LINK to one script per fixture, run once here unbounded:
    /// macOS assesses a new executable file on its first exec (measured ~20 s on a loaded
    /// m7, 2026-09-24, in atpkg's copy of this fixture — past the 5 s date bound), and a
    /// link to an assessed file is not new. The probes under test time the date rule, not
    /// Gatekeeper.
    #[cfg(unix)]
    fn dated_bin(fixture: &Path, sysroot: &Path, date: &str) -> PathBuf {
        let script = fixture.join("dated-trustc");
        if !script.is_file() {
            fs::write(
                &script,
                "#!/bin/sh\nroot=$(cd \"$(dirname \"$0\")/..\" && pwd -P)\n\
                 case \"$1\" in\n  --print) echo \"$root\" ;;\n  \
                 -vV) d=$(cat \"$root/commit-date\"); echo \"rustc 1.99.0-dev (0000000 $d)\"; \
                 echo \"commit-date: $d\" ;;\nesac\n",
            )
            .expect("write");
            make_runnable(&script);
            let _ = std::process::Command::new(&script).output();
        }
        let bin = sysroot.join("bin");
        fs::create_dir_all(sysroot.join("lib/rustlib")).expect("mkdir rustlib");
        fs::create_dir_all(&bin).expect("mkdir bin");
        let _ = fs::remove_file(bin.join("trustc"));
        fs::hard_link(&script, bin.join("trustc")).expect("link trustc");
        if !bin.join("targo").is_file() {
            exec_stub(&bin.join("targo"));
        }
        fs::write(sysroot.join("commit-date"), date).expect("date");
        fs::canonicalize(&bin).expect("canonicalize")
    }

    /// THE STALE RUSTUP LINK (measured 2026-09-24 on the owner's Mac: rustup's `trust` a
    /// hand-made link to `$HOME/trust/build/host/stage2`, 2026-08-20, the store 9192 at
    /// 2026-09-17; `aterm help rust` named the stage2 "the gates' toolchain"). Older than
    /// the store ⇒ ranked below it and NAMED; newer, undated, or with trust dev-linked ⇒
    /// the rustup entry keeps its rank; a store that is not the pin ⇒ the demoted entry is
    /// still reached before PATH, exactly the pre-rule answer, and is not named.
    #[cfg(unix)]
    #[test]
    fn a_rustup_trust_older_than_the_store_ranks_below_it_and_is_named() {
        let home = crate::mktemp_dir("atv-demote").expect("mktemp");
        let prefix = default_atpkg_prefix(&home);
        let store = dated_bin(&home, &prefix.join("store/trust/9192"), "2026-09-17");
        std::os::unix::fs::symlink(
            prefix.join("store/trust/9192"),
            prefix.join("store/trust/current"),
        )
        .expect("ln current");
        let stage2 = home.join("trust/build/aarch64-apple-darwin/stage2");
        let tree = dated_bin(&home, &stage2, "2026-08-20");
        fs::create_dir_all(home.join("trust/build")).expect("mkdir");
        std::os::unix::fs::symlink(
            home.join("trust/build/aarch64-apple-darwin"),
            home.join("trust/build/host"),
        )
        .expect("ln host");
        fs::create_dir_all(home.join(".rustup/toolchains")).expect("mkdir");
        std::os::unix::fs::symlink(
            home.join("trust/build/host/stage2"),
            home.join(".rustup/toolchains/trust"),
        )
        .expect("ln rustup");
        let found = || {
            Toolchain::discover_with_store(
                None,
                &home.join(".rustup"),
                Some(&prefix),
                OsStr::new(""),
                Some("trust"),
            )
        };

        let t = found();
        assert!(t.have_targo());
        assert_eq!(t.stage2_dir, store, "the older rustup entry must not win");
        let d = t.demoted.clone().expect("the demotion is named");
        assert_eq!(
            (d.dir.as_path(), d.its.as_str(), d.store.as_str()),
            (tree.as_path(), "2026-08-20", "2026-09-17")
        );
        let said = d.sentence();
        assert!(
            said.contains("older than the atpkg store's 2026-09-17"),
            "{said}"
        );
        assert!(d.linked, "the measured entry is a link");
        assert!(said.ends_with(REMEDY_LINK), "{said}");

        // A REAL DIRECTORY at the entry, older than the store: demoted all the same, but
        // `aterm pkg repair` refuses an entry that is not a link, so the sentence names the
        // by-hand removal first — never repair alone, which would answer "refusing".
        let entry = home.join(".rustup/toolchains/trust");
        fs::remove_file(&entry).expect("rm the link");
        let entry_bin = dated_bin(&home, &entry, "2026-08-20");
        let t = found();
        assert_eq!(t.stage2_dir, store, "a directory entry is demoted too");
        let d = t.demoted.clone().expect("and named");
        assert_eq!((d.dir.as_path(), d.linked), (entry_bin.as_path(), false));
        let said = d.sentence();
        assert!(
            said.ends_with(REMEDY_DIRECTORY) && !said.contains(REMEDY_LINK),
            "{said}"
        );
        fs::remove_dir_all(&entry).expect("rm the directory");
        std::os::unix::fs::symlink(home.join("trust/build/host/stage2"), &entry)
            .expect("ln rustup again");

        // Negative controls: every way the rule must NOT fire.
        for (date, why) in [
            ("2026-09-18", "newer"),
            ("2026-09-17", "same day"),
            ("unknown", "undated"),
        ] {
            dated_bin(&home, &stage2, date);
            let t = found();
            assert_eq!(t.stage2_dir, tree, "{why}: the rustup entry keeps its rank");
            assert_eq!(t.demoted, None, "{why}");
        }
        dated_bin(&home, &stage2, "2026-08-20");
        fs::create_dir_all(prefix.join("links")).expect("mkdir links");
        fs::write(prefix.join("links/trust"), "a dev-link marker").expect("marker");
        let t = found();
        assert_eq!(
            t.stage2_dir, tree,
            "dev-linked: nothing is stale against the store"
        );
        assert_eq!(t.demoted, None);
        fs::remove_file(prefix.join("links/trust")).expect("rm marker");

        // A store that cannot answer for itself is refused by the pin check; the demoted
        // entry is the next candidate, ahead of PATH, and wins unnamed.
        fs::write(prefix.join("store/trust/9192/commit-date"), "2026-09-17").expect("date");
        fs::remove_dir_all(prefix.join("store/trust/9192/lib")).expect("rm sysroot");
        let on_path = pinned_bin(&home.join("elsewhere/bin"));
        let t = Toolchain::discover_with_store(
            None,
            &home.join(".rustup"),
            Some(&prefix),
            on_path.as_os_str(),
            Some("trust"),
        );
        assert_eq!(
            t.stage2_dir, tree,
            "demoted below the store, never below PATH"
        );
        assert_eq!(t.demoted, None, "it won, so nothing was demoted in effect");
        assert!(t.refused.is_none());
        fs::remove_dir_all(&home).ok();
    }

    /// ATPKG'S VIEW IS READ THROUGH (2026-09-28, the module header's rule 4): a rustup
    /// entry resolving into atpkg's view (`<prefix>/rustup/trust`) stands for the build the
    /// view presents — the store's `current`, or a dev-linked sysroot checkout — never the
    /// view's own clones, whose directory the cutter's MEASURE gate refused as another
    /// compiler than its own (v0.97.0). So a view LAGGING the store (review of 2026-09-25)
    /// is no demotion either: the store is the answer, and nothing is said. NEGATIVE
    /// CONTROL: an older foreign link is still demoted and sent to repair.
    #[cfg(unix)]
    #[test]
    fn atpkgs_view_is_read_through_to_the_build_it_presents() {
        let home = crate::mktemp_dir("atv-view").expect("mktemp");
        let prefix = default_atpkg_prefix(&home);
        let store = dated_bin(&home, &prefix.join("store/trust/9200"), "2026-09-24");
        std::os::unix::fs::symlink(
            prefix.join("store/trust/9200"),
            prefix.join("store/trust/current"),
        )
        .expect("ln current");
        let view = dated_bin(&home, &prefix.join("rustup/trust"), "2026-09-17");
        fs::create_dir_all(home.join(".rustup/toolchains")).expect("mkdir");
        let entry = home.join(".rustup/toolchains/trust");
        std::os::unix::fs::symlink(prefix.join("rustup/trust"), &entry).expect("ln rustup");
        let found = || {
            Toolchain::discover_with_store(
                None,
                &home.join(".rustup"),
                Some(&prefix),
                OsStr::new(""),
                Some("trust"),
            )
        };
        let t = found();
        assert_eq!(
            t.stage2_dir, store,
            "a lagging view presents the store's build"
        );
        assert_ne!(t.stage2_dir, view, "never the view's own clones");
        assert_eq!(t.demoted, None, "read through, so never demoted");
        dated_bin(&home, &prefix.join("rustup/trust"), "2026-09-24");
        assert_eq!(
            found().stage2_dir,
            store,
            "a current view: the store's own directory"
        );
        // Dev-linked to a sysroot checkout outside the prefix: the view presents that.
        let checkout = home.join("trust-checkout");
        let linked = dated_bin(&home, &checkout, "2026-09-20");
        fs::create_dir_all(prefix.join("links")).expect("mkdir links");
        fs::write(
            prefix.join("links/trust"),
            format!(
                "program = \"trust\"\ncheckout = \"{}\"\n",
                checkout.display()
            ),
        )
        .expect("marker");
        assert_eq!(
            found().stage2_dir,
            linked,
            "a linked view presents its checkout"
        );
        // A checkout atpkg cannot present (no lib/): the cutter skips it, the store is next.
        fs::remove_dir_all(checkout.join("lib")).expect("rm lib");
        assert_eq!(
            found().stage2_dir,
            store,
            "an unpresentable link: the store"
        );
        fs::write(prefix.join("links/trust"), "not a marker").expect("garbage");
        assert_eq!(found().stage2_dir, store, "an unreadable marker is no link");
        fs::remove_file(prefix.join("links/trust")).expect("rm marker");
        // Negative control: a foreign link to an older tree is re-pointed by repair.
        let tree = dated_bin(&home, &home.join("stage2"), "2026-09-17");
        fs::remove_file(&entry).expect("rm link");
        std::os::unix::fs::symlink(home.join("stage2"), &entry).expect("ln foreign");
        let d = found().demoted.expect("a foreign older link is said");
        assert_eq!(d.dir, tree);
        assert!(d.sentence().ends_with(REMEDY_LINK));
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
    fn tippy_is_found_by_its_trust_name_only() {
        let tmp = crate::mktemp_dir("atv-tippy").expect("mktemp");
        // The lookup happens in the RESOLVED directory (see the symlink test):
        // on macOS /tmp is itself a symlink to /private/tmp.
        let real = fs::canonicalize(&tmp).expect("canonicalize");
        exec_stub(&tmp.join("targo-clippy"));
        let t = Toolchain::discover(Some(&tmp), Path::new("/unused"), OsStr::new(""), None);
        assert_eq!(
            t.tippy, None,
            "the retired pre-rebrand name is not a tippy: the lane is NOT RUN"
        );

        exec_stub(&tmp.join("targo-tippy"));
        let t = Toolchain::discover(Some(&tmp), Path::new("/unused"), OsStr::new(""), None);
        assert_eq!(t.tippy.as_deref(), Some(real.join("targo-tippy").as_path()));
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
