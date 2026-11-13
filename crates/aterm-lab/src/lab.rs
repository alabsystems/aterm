// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Lifecycle management (`up`, `down`, `ps`) for isolated lab instances.

use std::fs;
#[cfg(test)]
use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use aterm_uds::spawnfd::Stdio;

use crate::LAB;
use crate::door::Door;
use crate::paths::{paths, valid_tag};
use crate::procs::{self, alive, descendants};
use crate::protected::{CCSOCKS, inside_any};
use crate::pyjson::J;
use crate::run::{Run, all_runs, take_only_tag, take_tag};
use crate::util::{atomic_write, keep_file, keep_tree, now_secs, shell_prepend_path, shell_quote};
use crate::{Fail, Res, stop};

/// The fake API key `apiKeyHelper` prints (the stub accepts anything).
fn api_key() -> String {
    format!("sk-ant-api03-LABLABLAB{}AA", "A".repeat(85))
}

/// The fake claude.ai OAuth token of `claude TAG --subscriber`: no credential anywhere.
pub fn oauth_token() -> String {
    format!("sk-ant-oat01-LABLABLAB{}AA", "A".repeat(80))
}

/// How `up` makes a lab (each field has its `LAB_*` variable, but `up`'s flags `--window`,
/// `--fabric` and `--tab-strip`).
#[derive(Clone, Debug, Default)]
pub struct UpOpts {
    /// `LAB_SURVEY=1`: the forced survey flag in the settings at launch.
    pub survey: bool,
    /// `LAB_TRUST=1`: the folder trust seeded (no dialog).
    pub trust: bool,
    /// `LAB_FIRST_RUN=1`.
    pub first_run: bool,
    /// `LAB_RESTORE=1`.
    pub restore: bool,
    /// `LAB_HARNESS_KEYS`.
    pub harness_keys: String,
    /// `LAB_ATERM_TOML`.
    pub aterm_toml: String,
    /// `LAB_COLS` (144).
    pub cols: String,
    /// `LAB_LINES` (50).
    pub lines: String,
    /// `LAB_SCALE`: the instance's `--scale`.
    pub scale: Option<String>,
    /// `LAB_PROXY=dead`.
    pub proxy_dead: bool,
    /// The lab is the verb's own.
    pub lifeline: bool,
    /// `LAB_INSTANCE_ENV='K=V K=V'` ([`instance_env`]).
    pub instance_env: String,
    /// `LAB_CLOCK=1`: the lab's sleep clock loaded into its instance and its Claude Code
    /// ([`crate::clock`]).
    pub clock: bool,
    /// The lab's instance opens a window on this desk, in the same jail ([`windowed`]).
    pub window: bool,
    /// `LAB_REROUTE=1`: the lab's Claude Code store build cloned into the scratch home's own atpkg
    /// prefix with its `agents/` twin and reroute stub ([`crate::reroute_prefix`]), and the
    /// instance rerouting, so `claude --by-name` runs stub → twin → store as the owner's sessions
    /// do.
    pub reroute: bool,
    /// The instance reads the stub's own base URL as Claude Code's default route
    /// ([`aterm_agent::harness::netwatch::LAB_ROUTE_SEAM`]), so a lab Claude Code on the stub
    /// with a stored login is a roster row's process: `claude-rotation-proof`'s, `forever
    /// --mode sysmsg`'s expired-login chains on Claude Code but under `--token-login`
    /// (`forever::sysmsg::login::stored_arm`), and `LAB_ROUTE_SEAM=1` at `up`, for a lab whose
    /// tab runs a roster row's login by hand (the accounts probe's live check,
    /// docs/SPEC-claude-accounts-probe-2026-10-01.md §6 L5).
    pub route_seam: bool,
    /// `LAB_HOME_IMAGE=<size>` (`16g`): the scratch's `home/` is a sparse APFS image of that size,
    /// a volume of its own, which the `down` that removes the scratch detaches
    /// ([`home_image_refused`], [`detach_home_image`]).
    pub home_image: Option<String>,
    /// `LAB_JAIL_SCAN=1`: the instance's disk scans read the jail's processes alone (the jail's
    /// mark in [`aterm_uds::holders::JAIL_SEAM`], a debug build's seam): from inside the jail the
    /// owner's processes cannot be read, and such a scan rules out no path while one stands
    /// unread, so `aterm harness disk free` frees nothing in a lab without it.
    pub jail_scan: bool,
    /// The agent under test is the lab's own stand-in ([`crate::occupancy_standin`]): no real
    /// agent is looked for or run, not even for its version (no `LAB_*` variable: a scenario's
    /// [`crate::scn::LabKw`] alone asks for it).
    pub standin: bool,
    /// `up --fabric`: the lab's own bus ([`crate::bus::lay`]): a private broker in the lab's jail
    /// and socket dir, the node's and an observer's caps, the instance's `[fabric]` table, the
    /// rendezvous file `aterm link` reads, and the bridge connected before `up` returns.
    pub fabric: bool,
    /// `up --tab-strip`: `tab_strip_rows = 1`, the strip whose tab menu holds the session clause
    /// (`ctl open tab-menu`); it takes a grid row.
    pub tab_strip: bool,
}

impl UpOpts {
    /// The options the environment sets.
    #[must_use]
    pub fn from_env() -> UpOpts {
        let var = |k: &str| std::env::var(k).unwrap_or_default();
        let set = |k: &str| var(k) == "1";
        let or = |k: &str, d: &str| {
            Some(var(k))
                .filter(|v| !v.is_empty())
                .unwrap_or_else(|| d.to_string())
        };
        UpOpts {
            survey: set("LAB_SURVEY"),
            trust: set("LAB_TRUST"),
            first_run: set("LAB_FIRST_RUN"),
            restore: set("LAB_RESTORE"),
            harness_keys: var("LAB_HARNESS_KEYS"),
            aterm_toml: var("LAB_ATERM_TOML"),
            cols: or("LAB_COLS", "144"),
            lines: or("LAB_LINES", "50"),
            scale: Some(var("LAB_SCALE")).filter(|v| !v.is_empty()),
            proxy_dead: var("LAB_PROXY") == "dead",
            lifeline: false,
            instance_env: var("LAB_INSTANCE_ENV"),
            clock: set("LAB_CLOCK"),
            window: false,
            reroute: set("LAB_REROUTE"),
            route_seam: set("LAB_ROUTE_SEAM"),
            home_image: Some(var("LAB_HOME_IMAGE")).filter(|v| !v.is_empty()),
            jail_scan: set("LAB_JAIL_SCAN"),
            fabric: false,
            tab_strip: false,
            standin: false,
        }
    }
}

/// The most a lab's home image may be ([`UpOpts::home_image`]), in GiB.
pub const HOME_IMAGE_MAX_GIB: u64 = 64;

/// What the host must keep free beyond a home image's size before `up` makes one: the image is
/// sparse, so it takes the host's space only as the lab fills it, and a lab that fills it must not
/// be what fills the host.
pub const HOME_IMAGE_SPARE_GIB: u64 = 8;

/// The size of a home image `size` names (`<n>g` or `<n>m`, `hdiutil`'s spelling), in bytes, or
/// why it is refused: none, a size of no image, or one past [`HOME_IMAGE_MAX_GIB`].
pub fn home_image_bytes(size: &str) -> Result<u64, String> {
    let (n, shift) = match (size.strip_suffix('g'), size.strip_suffix('m')) {
        (Some(n), _) => (n, 30),
        (_, Some(n)) => (n, 20),
        _ => return Err(format!("LAB_HOME_IMAGE={size}: a size is <n>g or <n>m")),
    };
    let n: u64 = n
        .parse()
        .ok()
        .filter(|_| n.bytes().all(|b| b.is_ascii_digit()))
        .ok_or_else(|| format!("LAB_HOME_IMAGE={size}: a size is <n>g or <n>m"))?;
    let bytes = n.saturating_mul(1u64 << shift);
    if !((1 << 30)..=HOME_IMAGE_MAX_GIB << 30).contains(&bytes) {
        return Err(format!(
            "LAB_HOME_IMAGE={size}: an image is 1 GiB to {HOME_IMAGE_MAX_GIB} GiB"
        ));
    }
    Ok(bytes)
}

/// Why `up` makes no home image of `size` with `free` bytes free on the host's volume (`None`:
/// unknown): a size [`home_image_bytes`] refuses, or less free than the image could take plus
/// [`HOME_IMAGE_SPARE_GIB`].
pub fn home_image_refused(size: &str, free: Option<u64>) -> Option<String> {
    let bytes = match home_image_bytes(size) {
        Ok(b) => b,
        Err(e) => return Some(e),
    };
    let need = bytes.saturating_add(HOME_IMAGE_SPARE_GIB << 30);
    match free {
        Some(f) if f >= need => None,
        free => Some(format!(
            "LAB_HOME_IMAGE={size}: the host has {} free, and a home image of {} GiB needs {} GiB \
             free so the lab cannot be what fills the host",
            free.map_or_else(
                || "an unknown amount".to_owned(),
                |f| format!("{} GiB", f >> 30)
            ),
            bytes >> 30,
            need >> 30
        )),
    }
}

/// The home image `up` attaches for the run in `run_dir` (`home.sparseimage` there), at its
/// scratch's `home/` ([`Run::home_image`]).
fn home_image_file(run_dir: &Path) -> PathBuf {
    run_dir.join("home.sparseimage")
}

/// Detach the home image of the run in `run_dir` from its scratch's `home/`, when one is attached
/// there, and remove its file ([`crate::disk_lane::detach_at`]). Only a scratch that may go is
/// detached ([`sweep_copies`], [`abort`]'s removal): a copy a scratch hold ([`HELD`]) keeps in
/// place lies on the image, and detaching the image takes the copy's directory away as surely as
/// removing it would.
/// # Errors
/// A volume is still mounted there.
fn detach_home_image(run_dir: &Path) -> Result<(), String> {
    crate::disk_lane::detach_at(
        &run_dir.join("scratch").join("home"),
        &home_image_file(run_dir),
    )
}

/// The managed Claude Code, launched by path (not through its shim) ([`pkg_which`]; before any
/// aterm is built, [`store_shim_target`]), or `$LAB_CLAUDE`.
/// # Errors
/// Neither names an executable.
pub fn find_claude() -> Result<String, String> {
    let p = paths();
    let c = match std::env::var("LAB_CLAUDE") {
        Ok(c) if !c.is_empty() => c,
        _ => match built_aterm(p) {
            Some(aterm) => store_claude(p, aterm)?,
            None => store_shim_target(p.store_home(), "claude")
                .ok_or_else(|| p.need_aterm().err().unwrap_or_default())?,
        },
    };
    if !crate::sys::executable(&c) {
        return Err(format!("cannot execute {c}"));
    }
    Ok(fs::canonicalize(&c).map_or(c, |p| p.to_string_lossy().into_owned()))
}

/// The managed Claude Code of the package store the lab's facts `p` name
/// ([`crate::paths::Paths::store_home`], [`pkg_which`]).
/// # Errors
/// `aterm` could not be run, or named no managed Claude Code.
fn store_claude(p: &crate::paths::Paths, aterm: &Path) -> Result<String, String> {
    let line = pkg_which(aterm, p.store_home(), "claude")?;
    managed_claude(&line).ok_or_else(|| {
        format!(
            "cannot tell which Claude Code is managed here (aterm pkg which claude said: {}); set LAB_CLAUDE=/path/to/claude",
            if line.is_empty() { "nothing" } else { &line }
        )
    })
}

/// The store binary of a `pkg which claude` line.
#[must_use]
pub fn managed_claude(line: &str) -> Option<String> {
    managed_bin("claude", line)
}

/// The store binary of a `pkg which <name>` line.
#[must_use]
pub fn managed_bin(name: &str, line: &str) -> Option<String> {
    let rest = line.strip_prefix(&format!("{name} → "))?;
    let (chain, _) = rest
        .split_once(" — managed ")
        .or_else(|| rest.split_once(" — dev-linked "))?;
    let last = chain.rsplit(" → ").next()?;
    (!last.is_empty()).then(|| last.to_string())
}

/// The store binary of the managed `name` ([`pkg_which`]; before any aterm is built,
/// [`store_shim_target`]).
pub(crate) fn managed_store_bin(name: &str) -> Option<String> {
    let p = paths();
    match built_aterm(p) {
        Some(aterm) => managed_store_bin_in(p, aterm, name),
        None => store_shim_target(p.store_home(), name),
    }
}

/// The lab's aterm when one is built ([`crate::paths::Paths::need_aterm`] without its refusal).
fn built_aterm(p: &crate::paths::Paths) -> Option<&Path> {
    p.aterm
        .as_deref()
        .filter(|a| crate::sys::executable(&a.to_string_lossy()))
}

/// What the `bin/<name>` shim of the package store under `home` (its default prefix) forwards
/// to, read by atpkg's own shim reader with no aterm run, as `pkg which` reads it: a verb that
/// builds its aterm (`claude-measure`, `codex-measure`) names its program before that build.
fn store_shim_target(home: &str, name: &str) -> Option<String> {
    let layout = atpkg::Layout {
        prefix: atpkg::store::default_prefix(Path::new(home)),
    };
    atpkg::which(&layout, name).map(|t| t.to_string_lossy().into_owned())
}

/// [`managed_store_bin`] in the package store the lab's facts `p` name
/// ([`crate::paths::Paths::store_home`]).
fn managed_store_bin_in(p: &crate::paths::Paths, aterm: &Path, name: &str) -> Option<String> {
    managed_bin(name, &pkg_which(aterm, p.store_home(), name).ok()?)
}

/// The first line `aterm pkg which <name>` prints ([`crate::paths::Paths::store_home`]).
/// # Errors
/// `aterm` could not be run.
fn pkg_which(aterm: &Path, home: &str, name: &str) -> Result<String, String> {
    let out = aterm_uds::spawnfd::command(aterm)
        .env_clear()
        .env("HOME", home)
        .env("PATH", "/usr/bin:/bin")
        .env("xcrun_nocache", "1")
        .args(["pkg", "which", name])
        .stdin(Stdio::null())
        .output()
        .map_err(|e| format!("cannot run {}: {e}", aterm.display()))?;
    let text = String::from_utf8_lossy(&out.stdout);
    Ok(text.lines().next().unwrap_or("").to_string())
}

/// `c` by its physical path when it is executable, else `None`.
fn executable_at(c: String) -> Option<String> {
    executable_by(c, crate::sys::executable)
}

/// [`executable_at`], with `runs` saying which paths are executable.
fn executable_by(c: String, runs: impl Fn(&str) -> bool) -> Option<String> {
    if c.is_empty() || !runs(&c) {
        return None;
    }
    Some(fs::canonicalize(&c).map_or(c, |p| p.to_string_lossy().into_owned()))
}

/// The managed Codex, the same way as [`find_claude`].
#[must_use]
pub fn find_codex() -> Option<String> {
    let c = match std::env::var("LAB_CODEX") {
        Ok(c) if !c.is_empty() => c,
        _ => managed_store_bin("codex")?,
    };
    executable_at(c)
}

/// The Antigravity CLI under test.
#[must_use]
pub fn find_agy() -> Option<String> {
    let c = match std::env::var("LAB_AGY") {
        Ok(c) if !c.is_empty() => crate::paths::from_caller(&c),
        _ => managed_store_bin("agy")?,
    };
    executable_at(c)
}

/// The Muse Code under test.
#[must_use]
pub fn find_muse() -> Option<String> {
    let c = match std::env::var("LAB_MUSE") {
        Ok(c) if !c.is_empty() => crate::paths::from_caller(&c),
        _ => managed_store_bin("muse")?,
    };
    executable_at(c)
}

/// The Grok Build under test: `LAB_GROK`, else the managed store's build.
#[must_use]
pub fn find_grok() -> Option<String> {
    let c = match std::env::var("LAB_GROK") {
        Ok(c) if !c.is_empty() => crate::paths::from_caller(&c),
        _ => managed_store_bin("grok")?,
    };
    executable_at(c)
}

/// The Kimi Code under test: `LAB_KIMI`, else the managed store's.
#[must_use]
pub fn find_kimi() -> Option<String> {
    let c = match std::env::var("LAB_KIMI") {
        Ok(c) if !c.is_empty() => crate::paths::from_caller(&c),
        _ => managed_store_bin("kimi")?,
    };
    executable_at(c)
}

/// `text` when it is a version (a digit, then letters, digits, `.`, `+` and `-`), else `?`.
#[must_use]
pub fn clean_ver(text: &str) -> String {
    let ok = text.chars().next().is_some_and(|c| c.is_ascii_digit())
        && text
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || ".+-".contains(c));
    if ok { text.to_string() } else { "?".into() }
}

/// agy's version: the `version = "…"` of the record atpkg keeps beside its store build, else
/// `--version`'s last word ([`seeded_version`]).
#[must_use]
pub fn agy_version(run: &Run, bin: &str) -> String {
    let vendor = Path::new(bin)
        .parent()
        .and_then(Path::parent)
        .map(|b| PathBuf::from(format!("{}.vendor", b.display())));
    let v = vendor
        .and_then(|p| crate::sys::scratch_read(&p).ok().flatten())
        .map(|b| vendor_version(&String::from_utf8_lossy(&b)))
        .unwrap_or_default();
    if v.is_empty() {
        seeded_version(run, bin)
    } else {
        clean_ver(&v)
    }
}

/// `--version`'s last word of agy, run on its seeded scratch home ([`crate::agy::seed`]).
fn seeded_version(run: &Run, bin: &str) -> String {
    if crate::agy::seed(run, &crate::agy::Launch::default()).is_err() {
        return "?".into();
    }
    jailed_version_in(run, bin, &run.w.join(crate::agy::HOME), |l| {
        l.split_whitespace().last()
    })
}

/// The `version = "…"` line of an atpkg build record, or empty.
#[must_use]
pub fn vendor_version(text: &str) -> String {
    text.lines()
        .filter_map(|l| l.split_once('='))
        .find(|(k, _)| k.trim() == "version")
        .map(|(_, v)| v.trim().trim_matches('"').to_string())
        .unwrap_or_default()
}

/// Grok Build's version: the `version = "…"` of the record atpkg keeps beside its store build,
/// else `?`. Never `--version`: a Grok Build run without the lab's key starts xAI's device
/// sign-in (`docs/DESIGN-harness-repos-2026-10-24.md` D27).
#[must_use]
pub fn grok_version(bin: &str) -> String {
    let vendor = Path::new(bin)
        .parent()
        .and_then(Path::parent)
        .map(|b| PathBuf::from(format!("{}.vendor", b.display())));
    let v = vendor
        .and_then(|p| crate::sys::scratch_read(&p).ok().flatten())
        .map(|b| vendor_version(&String::from_utf8_lossy(&b)))
        .unwrap_or_default();
    clean_ver(&v)
}

/// The build Muse Code's `--version` names.
#[must_use]
pub fn muse_build(version_line: &str) -> Option<&str> {
    let (_, rest) = version_line.split_once('(')?;
    rest.split_once(')').map(|(b, _)| b.trim())
}

/// Muse Code's version ([`muse_build`]).
#[must_use]
pub fn muse_version(run: &Run, bin: &str) -> String {
    let vendor = Path::new(bin)
        .parent()
        .and_then(Path::parent)
        .map(|b| PathBuf::from(format!("{}.vendor", b.display())));
    let v = vendor
        .and_then(|p| crate::sys::scratch_read(&p).ok().flatten())
        .map(|b| vendor_version(&String::from_utf8_lossy(&b)))
        .unwrap_or_default();
    if v.is_empty() {
        jailed_version(run, bin, muse_build)
    } else {
        clean_ver(&v)
    }
}

/// Kimi Code's version: the `version = "…"` of the record atpkg keeps beside its store build,
/// else the last word of its `--version`, run inside the jail.
#[must_use]
pub fn kimi_version(run: &Run, bin: &str) -> String {
    let vendor = Path::new(bin)
        .parent()
        .and_then(Path::parent)
        .map(|b| PathBuf::from(format!("{}.vendor", b.display())));
    let v = vendor
        .and_then(|p| crate::sys::scratch_read(&p).ok().flatten())
        .map(|b| vendor_version(&String::from_utf8_lossy(&b)))
        .unwrap_or_default();
    if v.is_empty() {
        jailed_version(run, bin, |l| l.split_whitespace().last())
    } else {
        clean_ver(&v)
    }
}

/// `<bin> --version` run inside the lab's jail.
#[must_use]
pub fn jailed_version(run: &Run, bin: &str, pick: fn(&str) -> Option<&str>) -> String {
    jailed_version_in(run, bin, &run.w.join("home"), pick)
}

/// [`jailed_version`] with `home` as its HOME.
fn jailed_version_in(run: &Run, bin: &str, home: &Path, pick: fn(&str) -> Option<&str>) -> String {
    let out = crate::procs::jailed(
        run,
        &run.w,
        &[("HOME", &home.to_string_lossy())],
        &[bin, "--version"],
        crate::procs::JailOut::Capture,
    )
    .map(|(_, o)| o)
    .unwrap_or_default();
    clean_ver(out.lines().next().and_then(pick).unwrap_or(""))
}

/// The Codex version atpkg's package file beside the build names, else `--version`'s last word.
#[must_use]
pub fn codex_version(run: &Run, bin: &str) -> String {
    let pkg = Path::new(bin)
        .parent()
        .and_then(Path::parent)
        .map(|b| b.join("codex-package.json"));
    let v = pkg
        .and_then(|p| crate::sys::scratch_read(&p).ok().flatten())
        .and_then(|b| J::parse_bytes(&b).ok())
        .and_then(|j| j.get("version").and_then(J::as_str).map(str::to_string))
        .unwrap_or_default();
    if v.is_empty() {
        jailed_version(run, bin, |l| l.split_whitespace().last())
    } else {
        clean_ver(&v)
    }
}

fn need_sandbox() -> Result<(), String> {
    if cfg!(target_os = "macos") && Path::new("/usr/bin/sandbox-exec").is_file() {
        Ok(())
    } else {
        Err("sandbox-exec is required (macOS): it is the lab's network/keychain jail".into())
    }
}

/// The lab's own program, copied into the run directory at `up` (`bin/aterm-lab`).
const BIN: &str = "bin/aterm-lab";

/// Written beside [`BIN`] once a program started from it showed it ran.
const BIN_RAN: &str = "bin/ran";

/// The run directory's record of what was launched in each tab, `<sid>.sh`
/// ([`crate::verbs::record_launch`]): `down` keeps it in its logs, then removes it.
pub const LAUNCHES: &str = "launch";

/// Written in `relay/` once the relay copied there showed it ran.
const RELAY_RAN: &str = "relay/ran";

/// The ad hoc signed clone of Claude Code a lab up with the clock runs ([`crate::clock::Layout`]).
const CLOCK_CLONE: &str = "clock/claude";

/// Written beside [`CLOCK_CLONE`] once it showed it ran: its version line at `up`.
const CLOCK_RAN: &str = "clock/ran";

/// Written beside [`CLOCK_CLONE`] before it is laid ([`crate::clock::lay`]) and removed before
/// `up` first starts it: a clone with it was never started and goes at once; one without it counts
/// as started.
pub(crate) const CLOCK_UNSTARTED: &str = "clock/unstarted";

/// The holds on a lab's scratch, one directory per program the lab cloned into it, from before its
/// first exec at the latest until it showed it ran ([`hold_scratch`], [`release_scratch`],
/// [`REROUTE_HOLD`]). The lab may start it until `down` ends the jail, so `down` stamps each hold
/// left then ([`stamp_holds`]), and the scratch stays until every stamp is spent
/// ([`scratch_may_go`]).
pub(crate) const HELD: &str = "held";

/// The hold on the scratch for the reroute prefix's clone of Claude Code, which
/// [`crate::reroute_prefix::lay`] makes before the clone's exec and removes once it showed it ran.
const REROUTE_HOLD: &str = "reroute";

/// Hold the lab's scratch for a program about to be cloned into it and started (`name` names the
/// clone).
/// # Errors
/// The hold could not be made.
pub(crate) fn hold_scratch(run_dir: &Path, name: &str) -> std::io::Result<()> {
    fs::create_dir_all(run_dir.join(HELD).join(name))
}

/// The program `name` [`hold_scratch`] held for showed it ran.
pub(crate) fn release_scratch(run_dir: &Path, name: &str) {
    let _ = fs::remove_dir_all(run_dir.join(HELD).join(name));
}

/// The scratch's holds, with an unread inventory kept unknown rather than empty.
fn scratch_holds(run_dir: &Path) -> std::io::Result<Vec<PathBuf>> {
    match fs::read_dir(run_dir.join(HELD)) {
        Ok(entries) => entries
            .map(|entry| entry.map(|entry| entry.path()))
            .collect(),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Vec::new()),
        Err(e) => Err(e),
    }
}

/// Stamp what `down` (or an `up` that aborted) keeps once its jail is ended, when nothing can
/// start a program from it any more: each scratch hold ([`HELD`]), a started [`CLOCK_CLONE`]
/// ([`CLOCK_UNSTARTED`]), the lab program's copies (`relay/`, `bin/`) and an older lab program's
/// `twin-src/`, each unproven and with no reading yet (none, or a blank one a cut-short write
/// left), takes the awake clock's reading now ([`aterm_uds::deadline::try_stamp_exec`]). A copy's
/// mtime is when it was laid, and a launch can first exec the relay long after that. A later
/// `down` leaves a reading as it is, so a hold is spent once. Returns whether every stamp was
/// written: an unwritten one leaves its directory's age to its mtime, so `down` then keeps every
/// unproven copy ([`sweep_copies`]).
#[must_use]
fn stamp_holds(run_dir: &Path) -> bool {
    let clock = (run_dir.join(CLOCK_CLONE).exists() && !run_dir.join(CLOCK_UNSTARTED).exists())
        .then(|| run_dir.join("clock"));
    let copies = ["relay", "bin", "twin-src"]
        .into_iter()
        .map(|d| run_dir.join(d))
        .filter(|d| d.exists());
    let mut stamped = true;
    let holds = scratch_holds(run_dir).unwrap_or_else(|e| {
        eprintln!("aterm-lab: cannot read the scratch's holds: {e}; this down keeps it");
        stamped = false;
        Vec::new()
    });
    for dir in holds.into_iter().chain(clock).chain(copies) {
        if !dir.join(aterm_uds::deadline::RAN).exists()
            && aterm_uds::deadline::exec_stamp(&dir).is_none()
            && let Err(e) = aterm_uds::deadline::try_stamp_exec(&dir)
        {
            eprintln!(
                "aterm-lab: cannot stamp {} ({e}): this down keeps it unspent, and a later \
                 down that can stamp it starts its wait",
                dir.display()
            );
            stamped = false;
        }
    }
    stamped
}

/// Whether the lab's scratch may go at `now`: every hold on it ([`HELD`]) released, or spent
/// ([`copies_may_go`]) when no stamp failed (`unstamped`, [`stamp_holds`]).
fn scratch_may_go(run_dir: &Path, unstamped: bool, now: std::time::SystemTime) -> bool {
    scratch_holds(run_dir).is_ok_and(|holds| {
        holds
            .iter()
            .all(|dir| !unstamped && copies_may_go(dir, false, now))
    })
}

/// Whether a directory of copies may be removed: one started from it showed it ran (`ran`, or its
/// `ran` marker), or no assessment of a copy in it is still going at `now`
/// ([`aterm_uds::deadline::copy_may_go`]: [`aterm_uds::deadline::FIRST_EXEC_HANG`] has passed
/// since its exec stamp on the awake clock, or since the directory changed when it has none).
fn copies_may_go(dir: &Path, ran: bool, now: std::time::SystemTime) -> bool {
    ran || fs::symlink_metadata(dir).is_err() || aterm_uds::deadline::copy_may_go(dir, now)
}

/// The words for a directory of copies kept because none was shown to run, or a scratch kept
/// because its home is a volume still attached ([`detach_home_image`]).
pub(crate) fn kept_copies(dir: &Path, verb: &str, tag: &str) -> String {
    if crate::disk_lane::mounted_at(&dir.join("home")) {
        return format!(
            "KEPT {}: its home is an image still attached; {LAB} {verb} {tag} detaches it and \
             removes it",
            dir.display()
        );
    }
    format!(
        "KEPT {}: a program copied there was started and never showed it ran, so macOS may \
         still be assessing its first exec; {LAB} {verb} {tag} removes it once {} minutes have \
         passed",
        dir.display(),
        aterm_uds::deadline::FIRST_EXEC_HANG.as_secs() / 60
    )
}

/// Remove the directories of copies in `run_dir` that may go at `now` ([`copies_may_go`]): the
/// lab's program's (`relay/`, `bin/`), the clock's clone of Claude Code (`clock/`), an older lab
/// program's twin probe (`twin-src/`), and the scratch with its holds ([`scratch_may_go`]), its
/// home image detached first ([`detach_home_image`]; a scratch whose image stays attached is
/// kept). Returns the ones kept. `unstamped`: a stamp [`stamp_holds`] owed was not written, so
/// what it stamps stays unless shown to run or gone.
fn sweep_copies(
    run_dir: &Path,
    relay_ran: bool,
    unstamped: bool,
    now: std::time::SystemTime,
) -> Vec<PathBuf> {
    let at = |d: &str| run_dir.join(d);
    let clock_ran =
        !at(CLOCK_CLONE).exists() || at(CLOCK_RAN).exists() || at(CLOCK_UNSTARTED).exists();
    let stamped_may_go = |d: &str, ran: bool| {
        let dir = at(d);
        (ran || !unstamped || fs::symlink_metadata(&dir).is_err()) && copies_may_go(&dir, ran, now)
    };
    let mut kept = Vec::new();
    for (d, may_go) in [
        ("relay", stamped_may_go("relay", relay_ran)),
        ("bin", stamped_may_go("bin", at(BIN_RAN).exists())),
        ("clock", stamped_may_go("clock", clock_ran)),
        ("twin-src", stamped_may_go("twin-src", false)),
        (
            "scratch",
            fs::symlink_metadata(at("scratch")).is_err() || scratch_may_go(run_dir, unstamped, now),
        ),
    ] {
        if !may_go {
            kept.push(at(d));
            continue;
        }
        // A home image comes down only with the scratch it holds: once no hold keeps a copy on it.
        if d == "scratch"
            && let Err(e) = detach_home_image(run_dir)
        {
            eprintln!("aterm-lab: {e}");
            kept.push(at(d));
            continue;
        }
        let _ = fs::remove_dir_all(at(d));
        if d == "scratch" {
            let _ = fs::remove_dir_all(at(HELD));
        }
    }
    kept
}

/// The end of `down`, once its logs are kept: the copies that may go, the scratch among them
/// ([`sweep_copies`]), the run directory's own files, its tabs' launch records
/// ([`crate::verbs::record_launch`]) and input-hold captures ([`crate::forever::input_hold`]),
/// both of which `down` keeps in its logs, and its bus keys ([`drop_bus_keys`]), then the
/// directory.
fn clear_run_dir(
    run_dir: &Path,
    relay_ran: bool,
    unstamped: bool,
    tag: &str,
    now: std::time::SystemTime,
) {
    let kept = sweep_copies(run_dir, relay_ran, unstamped, now);
    for dir in &kept {
        eprintln!("aterm-lab: {}", kept_copies(dir, "down", tag));
    }
    if let Ok(rd) = fs::read_dir(run_dir) {
        for e in rd.flatten() {
            let name = e.file_name().to_string_lossy().into_owned();
            if name == "env.sh" && !kept.is_empty() {
                continue;
            }
            let ext = Path::new(&name)
                .extension()
                .map(|x| x.to_string_lossy().into_owned())
                .unwrap_or_default();
            if ["log", "out", "pid", "pids", "port", "sh", "sb", "txt"].contains(&ext.as_str())
                || name == ".seen"
                || name == ".now"
            {
                let _ = fs::remove_file(e.path());
            }
        }
    }
    let _ = fs::remove_dir_all(run_dir.join(LAUNCHES));
    let _ = fs::remove_dir_all(run_dir.join(crate::forever::input_hold::DIR));
    drop_bus_keys(run_dir);
    let _ = fs::remove_dir(run_dir);
}

/// Whether `up` started a program from the lab's copy of its own program ([`BIN`]) that has not
/// shown it ran ([`BIN_RAN`]).
fn bin_unproven(run: &Run) -> bool {
    (run.sampler_pid > 0 || run.stub_pid > 0) && !run.run.join(BIN_RAN).exists()
}

/// Undo a half-made lab (its pids, everything in its jail, its socket dir and run directory).
/// A run directory a started copy has not shown it ran from ([`bin_unproven`]), or with a hold on
/// its scratch ([`HELD`]), stays, its holds stamped and its `env.sh` written, so `ps` lists it
/// and `down` removes it once nothing holds it.
fn abort(run: &Run, pids: &[i32], msg: &str) -> Fail {
    let unproven = bin_unproven(run);
    let held = scratch_holds(&run.run).map_or(true, |holds| !holds.is_empty());
    let sig = if unproven {
        libc::SIGKILL
    } else {
        libc::SIGTERM
    };
    for &p in pids {
        for q in descendants(p).into_iter().chain([p]) {
            if alive(q) {
                let _ = crate::sys::kill(q, sig);
            }
        }
    }
    if !run.mark.is_empty()
        && let Ok(in_jail) = crate::sys::jail_pids(&run.mark)
    {
        for q in in_jail {
            let _ = crate::sys::kill(q, sig);
        }
    }
    if run.sockdir.starts_with("/tmp/at.") {
        let _ = fs::remove_dir_all(&run.sockdir);
    }
    // A home image stays attached while its scratch stays: a copy a hold keeps lies on it, and
    // the `down` that removes the scratch detaches it ([`sweep_copies`]).
    let attached = !(unproven || held)
        && detach_home_image(&run.run)
            .map_err(|e| eprintln!("aterm-lab: {e}"))
            .is_err();
    if unproven || held || attached {
        // its wait runs from here; one that cannot be stamped is stamped by the next down or up
        let _ = stamp_holds(&run.run);
        let env = run.run.join("env.sh");
        if !env.exists() {
            run.save();
        }
        // `down` loads `env.sh`; when it could not be written, the next `up` sweeps (`room_for`)
        let verb = if env.is_file() { "down" } else { "up" };
        let dir = if attached { &run.w } else { &run.run };
        eprintln!("aterm-lab: {}", kept_copies(dir, verb, &run.tag));
    } else {
        let _ = fs::remove_dir_all(&run.run);
    }
    Fail::die(msg)
}

/// A lifeline for a child of a lab this verb owns ([`lifeline_flag`]).
fn lifeline_end(run: &Run, armed: bool) -> std::io::Result<Option<fs::File>> {
    if !armed {
        return Ok(None);
    }
    static HELD: std::sync::Mutex<Vec<aterm_uds::lifeline::Lifeline>> =
        std::sync::Mutex::new(Vec::new());
    let (held, far) = aterm_uds::lifeline::Lifeline::open(&run.run)?;
    HELD.lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .push(held);
    Ok(Some(far))
}

/// The flag that names a lifeline on stdin.
fn lifeline_flag() -> Vec<String> {
    vec![aterm_uds::lifeline::FLAG.to_string(), "0".to_string()]
}

/// How long a jailed child the lab started may stay idle before it says it is ready (its
/// `--ready-fd` byte, or a window's answer): a hang detector, never a latency budget.
const READY_HANG: Duration = Duration::from_secs(60);

/// How often a wait for a starting child looks at what it did ([`while_starting`]).
const READY_LOOK: Duration = Duration::from_secs(1);

/// `sandbox-exec -f <jail> env -i <vars> <program…>` as a std `Command` ([`launch_ready`],
/// [`launch_window`]).
/// # Errors
/// `out` could not be opened.
fn jailed_command(
    run: &Run,
    vars: &[(String, String)],
    program: &[String],
    out: &Path,
    stdin: Option<fs::File>,
) -> Result<aterm_uds::spawnfd::StdCommand, String> {
    use aterm_uds::spawnfd::InheritNothing as _;
    let (o, e) = append_to_std(out).map_err(|e| e.to_string())?;
    #[expect(
        clippy::disallowed_types,
        reason = "the jailed launch: a headless one hands a descriptor above stdio (--ready-fd)"
    )]
    let mut cmd = std::process::Command::new("/usr/bin/sandbox-exec");
    cmd.inherit_nothing();
    cmd.arg("-f")
        .arg(run.run.join("jail.sb"))
        .args(["/usr/bin/env", "-i", "xcrun_nocache=1"]);
    for (k, v) in vars {
        cmd.arg(format!("{k}={v}"));
    }
    cmd.args(program)
        .current_dir(&run.w)
        .stdin(stdin.map_or_else(std::process::Stdio::null, std::process::Stdio::from))
        .stdout(o)
        .stderr(e);
    crate::sys::in_own_session(&mut cmd);
    Ok(cmd)
}

/// A windowed instance launched in the jail ([`jailed_command`]).
/// # Errors
/// The launch failed, the child ended before its socket answered, it was idle for `hang` before
/// it answered, or the run was interrupted (`interrupted`).
fn launch_window(
    run: &Run,
    vars: &[(String, String)],
    program: &[String],
    out: &Path,
    stdin: Option<fs::File>,
    (pid, hang): (&mut i32, Duration),
) -> Result<(), String> {
    let mut cmd = jailed_command(run, vars, program, out, stdin)?;
    let child = cmd
        .spawn()
        .map_err(|e| format!("cannot start {}: {e}", program.join(" ")))?;
    *pid = procs::keep_forked(child);
    let door = Door::new(run)?;
    let child = *pid;
    let mut starting = Starting::of(child, out);
    while_starting(
        || {
            if door.ctl(&["version"]).is_ok_and(|r| r.code == 0) {
                Ok(true)
            } else if alive(child) {
                Ok(false)
            } else {
                Err("the window's instance ended before its control socket answered".into())
            }
        },
        || starting.active(),
        hang,
        Instant::now(),
        || {
            stop::sleep(Duration::from_millis(250)).map_err(|_| "interrupted".to_string())?;
            Ok(Instant::now())
        },
    )
}

/// What a starting child did between two looks ([`while_starting`]): whether its process group
/// (the lab starts it in a session of its own, so its pid is its group's) used CPU time or moved
/// bytes, by the one idle predicate ([`aterm_sysprobe::activity`]), or its output `out` grew.
struct Starting<'a> {
    pgid: u32,
    out: &'a Path,
    seen: aterm_sysprobe::activity::Members,
    said: Option<u64>,
}

impl<'a> Starting<'a> {
    /// The child `pid`, writing to `out`, before its first look.
    fn of(pid: i32, out: &'a Path) -> Self {
        Self {
            pgid: u32::try_from(pid).unwrap_or(0),
            out,
            seen: aterm_sysprobe::activity::Members::new(),
            said: None,
        }
    }

    /// Whether the child did anything since the last look: its group was not idle
    /// ([`aterm_sysprobe::activity::Used::idle`]) or its output's length moved. The first look
    /// finds it active.
    fn active(&mut self) -> bool {
        let members = aterm_sysprobe::group_usage(self.pgid).unwrap_or_default();
        let (used, seen) = aterm_sysprobe::activity::between(&self.seen, members);
        self.seen = seen;
        let said = fs::symlink_metadata(self.out).map_or(0, |m| m.len());
        let wrote = self.said != Some(said);
        self.said = Some(said);
        !used.idle() || wrote
    }
}

/// A starting child's readiness, waited for as a hang detector on whether it is `active`
/// ([`Starting::active`]).
/// # Errors
/// The child ended before it was ready, it was idle for `hang`, or `pause` failed.
fn while_starting(
    mut look: impl FnMut() -> Result<bool, String>,
    mut active: impl FnMut() -> bool,
    hang: Duration,
    start: Instant,
    mut pause: impl FnMut() -> Result<Instant, String>,
) -> Result<(), String> {
    let mut since = start;
    let mut now = start;
    loop {
        if look()? {
            return Ok(());
        }
        if active() {
            since = now;
        }
        if now.saturating_duration_since(since) >= hang {
            return Err(format!(
                "said nothing and was idle (no CPU time used, no bytes moved, no output written) \
                 for {} s: the hang detector",
                hang.as_secs()
            ));
        }
        now = pause()?;
    }
}

/// Why `o` cannot bring a lab up, before anything is made.
#[must_use]
pub(crate) fn opts_refused(o: &UpOpts) -> Option<&'static str> {
    if o.window && o.restore {
        return Some(
            "a windowed lab takes no LAB_RESTORE=1: the headless-restore seam is a headless instance's",
        );
    }
    if o.reroute && o.clock {
        return Some(
            "LAB_REROUTE=1 takes no LAB_CLOCK=1: the sleep clock runs its own re-signed clone of Claude Code, not the store build the reroute prefix routes to",
        );
    }
    // The scratch `aterm.toml` composes LAB_ATERM_TOML with what these flags add
    // ([`scratch_aterm_toml`]): a key or table both name is no TOML document at all.
    let named = |key: &str| {
        aterm_toml::from_str::<aterm_toml::Value>(&o.aterm_toml)
            .is_ok_and(|doc| doc.get(key).is_some())
    };
    if o.fabric && named("fabric") {
        return Some(
            "up --fabric takes no LAB_ATERM_TOML with a [fabric] table: the lab's own bus names its bridge there",
        );
    }
    (o.tab_strip && named("tab_strip_rows")).then_some(
        "up --tab-strip takes no LAB_ATERM_TOML that sets tab_strip_rows: --tab-strip sets it to 1",
    )
}

/// `sandbox-exec -f <jail> env -i <vars> <program…>` ([`procs::keep_forked`]).
/// # Errors
/// The launch failed, or the child closed its ready signal with nothing said (it exited, or it
/// has nothing to offer), made no progress for `hang` before its byte (the hang detector:
/// [`READY_HANG`], or the first-exec hang for a fresh copy), or the run was interrupted
/// (`interrupted`).
pub(crate) fn launch_ready(
    run: &Run,
    vars: &[(String, String)],
    program: &[String],
    out: &Path,
    stdin: Option<fs::File>,
    (pid, hang): (&mut i32, Duration),
) -> Result<(), String> {
    let mut cmd = jailed_command(run, vars, program, out, stdin)?;
    let mut ready =
        aterm_agent::ready::Ready::open(&run.run).map_err(|e| format!("the ready signal: {e}"))?;
    ready.hand(&mut cmd);
    let child = cmd
        .spawn()
        .map_err(|e| format!("cannot start {}: {e}", program.join(" ")))?;
    *pid = procs::keep_forked(child);
    let child = *pid;
    let mut starting = Starting::of(child, out);
    let name = program.first().map_or("the child", String::as_str);
    while_starting(
        || match ready.wait(READY_LOOK) {
            aterm_agent::ready::Signal::Ready => Ok(true),
            aterm_agent::ready::Signal::TimedOut => Ok(false),
            aterm_agent::ready::Signal::Closed => Err(format!(
                "{name} closed its ready signal with nothing said (it exited, or has nothing to offer)"
            )),
            aterm_agent::ready::Signal::Failed(e) => Err(format!("the ready signal: {e}")),
        },
        || starting.active(),
        hang,
        Instant::now(),
        || {
            stop::check().map_err(|_| "interrupted".to_string())?;
            Ok(Instant::now())
        },
    )
}

/// `path` opened for appending, twice.
fn append_to_std(path: &Path) -> std::io::Result<(std::process::Stdio, std::process::Stdio)> {
    use std::os::unix::fs::OpenOptionsExt as _;
    let f = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(path)?;
    if !f.metadata()?.is_file() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "not a regular file",
        ));
    }
    let g = f.try_clone()?;
    Ok((std::process::Stdio::from(f), std::process::Stdio::from(g)))
}

/// What the lab itself sets in its instance's environment ([`crate::clock::vars`]).
fn lab_vars(run: &Run, user: &str) -> Vec<(String, String)> {
    let w = run.w.to_string_lossy().into_owned();
    let mut vars: Vec<(String, String)> = [
        (
            "PATH",
            format!(
                "{}:/usr/bin:/bin:/usr/sbin:/sbin",
                run.run.join("shim").display()
            ),
        ),
        ("SHELL", "/bin/zsh".into()),
        ("HOME", format!("{w}/home")),
        ("USER", user.to_string()),
        ("LOGNAME", user.to_string()),
        ("LANG", "en_US.UTF-8".into()),
        ("XDG_CONFIG_HOME", format!("{w}/cfg")),
        ("XDG_RUNTIME_DIR", run.sockdir.clone()),
        ("ATERM_STATE_HOME", format!("{w}/state")),
        ("BROWSER", run.run.join("shim/open").display().to_string()),
        ("LAB_OPEN_LOG", format!("{w}/open.log")),
    ]
    .map(|(k, v)| (k.to_string(), v))
    .to_vec();
    if run.seam == "1" {
        vars.push(("ATERM_DEBUG_HEADLESS_RESTORE".into(), "1".into()));
    }
    if run.route_seam == "1" && !run.port.is_empty() {
        vars.push((
            aterm_agent::harness::netwatch::LAB_ROUTE_SEAM.into(),
            stub_base_url(run),
        ));
    }
    if run.jail_scan == "1" {
        vars.push((aterm_uds::holders::JAIL_SEAM.into(), run.mark.clone()));
    }
    vars.extend(crate::instance::seams());
    vars.extend(crate::clock::vars(run));
    vars
}

/// The stub's base URL, as the scratch settings name it to Claude Code.
#[must_use]
pub fn stub_base_url(run: &Run) -> String {
    format!("http://127.0.0.1:{}", run.port)
}

/// Whether the lab sets `name` for its instance itself ([`lab_vars`]).
fn lab_sets(name: &str) -> bool {
    let probe = Run {
        seam: "1".into(),
        route_seam: "1".into(),
        port: "1".into(),
        clock: "1".into(),
        jail_scan: "1".into(),
        ..Run::default()
    };
    lab_vars(&probe, "").iter().any(|(k, _)| k == name)
}

/// `LAB_INSTANCE_ENV` read: `NAME=VALUE` words ([`lab_vars`]).
/// # Errors
/// A word that is no `NAME=VALUE`, a name outside those kinds (`TMPDIR`, an `ATERM_DEBUG_*` or
/// `ATERM_DEV_*` seam, `NSZombieEnabled`, or the occupancy nudge's seam,
/// [`NUDGE_SEAM`](aterm_agent::supervise::occupancy::rules::NUDGE_SEAM)) or one the lab sets, a
/// name given twice, an empty value or one with a control character, or a `TMPDIR` that is not
/// absolute, or `NSZombieEnabled` other than `YES` or `NO`.
pub fn instance_env(spec: &str) -> Result<Vec<(String, String)>, String> {
    let refuse = |m: String| Err(format!("LAB_INSTANCE_ENV: {m}"));
    let mut out: Vec<(String, String)> = Vec::new();
    for word in spec.split_whitespace() {
        let Some((name, value)) = word.split_once('=') else {
            return refuse(format!("'{word}' is no NAME=VALUE"));
        };
        let seam = ["ATERM_DEBUG_", "ATERM_DEV_"].iter().any(|stem| {
            name.strip_prefix(stem).is_some_and(|rest| {
                !rest.is_empty()
                    && rest
                        .bytes()
                        .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit() || b == b'_')
            })
        });
        let named = name == aterm_agent::supervise::occupancy::rules::NUDGE_SEAM;
        if !(name == "TMPDIR" || name == "NSZombieEnabled" || seam || named) || lab_sets(name) {
            return refuse(format!(
                "{name} is not a variable the lab hands its instance (TMPDIR, NSZombieEnabled, an ATERM_DEBUG_* or ATERM_DEV_* seam the lab does not set itself, or ATERM_OCCUPANCY_NUDGE_S)"
            ));
        }
        if value.is_empty() || value.chars().any(char::is_control) {
            return refuse(format!("{name} needs a value with no control character"));
        }
        if name == "NSZombieEnabled" && !matches!(value, "YES" | "NO") {
            return refuse("NSZombieEnabled takes YES or NO".into());
        }
        if name == "TMPDIR" && !value.starts_with('/') {
            return refuse(format!("TMPDIR={value} is not an absolute path"));
        }
        if out.iter().any(|(n, _)| n == name) {
            return refuse(format!("{name} is given twice"));
        }
        out.push((name.to_string(), value.to_string()));
    }
    Ok(out)
}

/// The instance's environment at every launch ([`lab_vars`], [`instance_env`]).
/// # Errors
/// The record holds what `instance_env` refuses (an `env.sh` edited by hand).
pub(crate) fn instance_vars(run: &Run, user: &str) -> Result<Vec<(String, String)>, String> {
    let mut vars = lab_vars(run, user);
    vars.extend(instance_env(&run.instance_env)?);
    Ok(vars)
}

/// The person id of the lab bus's observer cap (`<run>/fabric/observer.cap`): an `a-` service
/// ring, which reads `/f/lab/pub/>` and its own lane and cannot publish a halt.
pub const BUS_OBSERVER: &str = "a-lab-observer";

/// The name of the lab bus's keys dir in the run directory ([`bus_keys`]).
const BUS_KEYS: &str = "fabric";

/// The lab bus's keys dir (`up --fabric`): `<run>/fabric`, which the jail reads and never
/// writes (its profile denies every write under the run directory but the scratch).
#[must_use]
pub fn bus_keys(run: &Run) -> PathBuf {
    run.run.join(BUS_KEYS)
}

/// The bus keys an earlier lab of the tag left in `run_dir` ([`bus_keys`]: its link to the aterm
/// under test, its mint secret and its caps), removed with no link followed: they are no running
/// lab's once `down` has ended its jail, or once no `env.sh` names a lab ([`room_for`]), and a
/// keys dir left in place makes the next `up --fabric` of the tag fail on its link.
fn drop_bus_keys(run_dir: &Path) {
    let _ = fs::remove_dir_all(run_dir.join(BUS_KEYS));
}

/// The lab bus's live dir: `<sockdir>/fabric`, the broker's socket and logs and the bridge's
/// state, which the jail writes.
fn bus_live(run: &Run) -> PathBuf {
    Path::new(&run.sockdir).join("fabric")
}

/// A file the jail may write, as the lab reads it: its socket dir resolved through `/tmp`'s link
/// (the jail may not move that dir), and no link followed below it ([`crate::sys::scratch_read`]).
fn jail_written(path: &Path) -> String {
    crate::util::scratch_text(&crate::paths::physical(path))
}

/// What the lab's broker said (`<sockdir>/fabric/broker.out`).
fn broker_said(run: &Run) -> String {
    jail_written(&bus_live(run).join("broker.out"))
        .trim()
        .to_string()
}

/// The environment the lab's broker and its mints run with in the jail: the scratch's homes and
/// the lab's socket dir, from [`lab_vars`] (none of its seams or its clock).
fn bus_vars(run: &Run, user: &str) -> Vec<(String, String)> {
    const NAMES: [&str; 8] = [
        "PATH",
        "HOME",
        "USER",
        "LOGNAME",
        "LANG",
        "XDG_CONFIG_HOME",
        "XDG_RUNTIME_DIR",
        "ATERM_STATE_HOME",
    ];
    lab_vars(run, user)
        .into_iter()
        .filter(|(k, _)| NAMES.contains(&k.as_str()))
        .collect()
}

/// `up --fabric`'s bus, before the stub: laid out ([`crate::bus::lay`], keys in [`bus_keys`],
/// the live dir in [`bus_live`], each grant minted in the jail), the rendezvous file at
/// `<sockdir>/aterm/fabric.toml` (where `aterm link` run with the lab's `XDG_RUNTIME_DIR` looks,
/// never the lab process's own control socket dir), and its broker started in the jail in a
/// session of its own (its lifeline on stdin when `armed`, so a CLI `up`'s broker outlives `up`
/// and ends with `down`'s jail sweep). Returns the `[fabric]` table.
/// # Errors
/// A file could not be made, a grant could not be minted, or the broker could not start or never
/// listened ([`crate::bus::await_listening`], naming what it said).
fn stand_bus(run: &mut Run, armed: bool) -> Result<String, String> {
    let vars = bus_vars(run, &paths().user);
    let live = bus_live(run);
    let laid = {
        let jailed: Vec<(&str, &str)> = vars.iter().map(|(k, v)| (&**k, &**v)).collect();
        let run: &Run = run;
        crate::bus::lay(
            &run.need_aterm()?,
            &bus_keys(run),
            &live,
            Some(BUS_OBSERVER),
            &mut |argv: &[String]| {
                let words: Vec<&str> = argv.iter().map(String::as_str).collect();
                let out =
                    procs::jailed_output(run, &run.w, &jailed, &words, procs::JailOut::CaptureAll)?;
                if out.status.success() {
                    Ok(out.stdout)
                } else {
                    Err(String::from_utf8_lossy(&out.stderr).trim().to_string())
                }
            },
        )?
    };
    run.fabric = live.to_string_lossy().into_owned();
    run.save();
    // `aterm_uds::control_socket_dir` under XDG_RUNTIME_DIR=<sockdir>: `<sockdir>/aterm`.
    let dir = Path::new(&run.sockdir).join("aterm");
    let file = dir.join(aterm_link::enable::RENDEZVOUS_FILE);
    crate::util::create_private_dir_all(&dir)
        .and_then(|()| {
            use std::io::Write as _;
            crate::util::create_new_regular_file(&file, 0o600)?
                .write_all(laid.rendezvous().render().as_bytes())
        })
        .map_err(|e| format!("the rendezvous file {}: {e}", file.display()))?;
    let stdin = lifeline_end(run, armed).map_err(|e| format!("the broker's lifeline: {e}"))?;
    let mut prog = laid.broker_argv();
    if stdin.is_some() {
        prog.extend(lifeline_flag());
    }
    let mut cmd = jailed_command(run, &vars, &prog, &live.join("broker.out"), stdin)?;
    let child = cmd
        .spawn()
        .map_err(|e| format!("cannot start the lab's broker: {e}"))?;
    run.broker_pid = procs::keep_jailed(&run.run, child);
    run.save();
    let pid = run.broker_pid;
    crate::bus::await_listening(
        &laid.sock,
        crate::bus::LISTEN_HANG,
        || !alive(pid),
        || broker_said(run),
    )?;
    Ok(laid.table)
}

/// The lab's bridge connected (`ctl fabric status` gives `state=connected`), waited for as a
/// one-minute hang detector ([`READY_HANG`]), never a latency budget: at `up --fabric`, and at
/// each relaunch of such a lab on its own state ([`crate::restore::relaunch_instance`]).
/// # Errors
/// It never connected (naming the last status), or the wait was interrupted.
pub(crate) fn await_bridge(run: &Run) -> Result<(), String> {
    let door = Door::new(run)?;
    let end = Instant::now() + READY_HANG;
    loop {
        let r = door.ctl(&["fabric", "status"])?;
        let status = r
            .stdout
            .lines()
            .chain(r.stderr.lines())
            .find(|l| l.starts_with("OK ") || l.starts_with("ERR"))
            .unwrap_or("")
            .to_string();
        if crate::screen::field(&status, "state").as_deref() == Some("connected") {
            return Ok(());
        }
        if Instant::now() >= end {
            return Err(format!(
                "no connected bridge in {} s: fabric status {status}",
                READY_HANG.as_secs()
            ));
        }
        stop::sleep(Duration::from_millis(250)).map_err(|_| "interrupted".to_string())?;
    }
}

/// Whether the lab's instance opens a window (`WINDOW=1`, `forever --window`).
#[must_use]
pub(crate) fn windowed(run: &Run) -> bool {
    run.window == "1"
}

/// The instance's command line.
pub(crate) fn instance_argv(
    run: &Run,
    aterm: &str,
    scale: Option<&str>,
    lifeline: bool,
) -> Vec<String> {
    let mut prog: Vec<String> = Vec::new();
    if windowed(run) {
        if lifeline {
            prog.extend([
                run.run.join(BIN).to_string_lossy().into_owned(),
                "keeper".to_string(),
                "--".to_string(),
            ]);
        }
        prog.push(aterm.to_string());
    } else {
        prog.extend([aterm, "--headless"].map(str::to_string));
        if lifeline {
            prog.extend(lifeline_flag());
        }
    }
    if run.reroute != "1" {
        prog.push("--no-reroute".to_string());
    }
    prog.extend(
        [
            "--control-sock",
            &run.sock,
            "-d",
            &run.proj,
            "--columns",
            &run.cols,
            "--lines",
            &run.rows,
        ]
        .map(str::to_string),
    );
    if let Some(s) = scale {
        prog.extend(["--scale".to_string(), s.to_string()]);
    }
    prog
}

/// The private instance (jailed), headless unless the run asks for a window ([`instance_vars`],
/// [`instance_argv`]); once its socket is up, the build it runs is recorded ([`running_build`]).
pub fn start_instance(run: &mut Run, scale: Option<&str>, armed: bool) -> Result<(), String> {
    let aterm = run.need_aterm()?.to_string_lossy().into_owned();
    let vars = instance_vars(run, &paths().user)?;
    let stdin = lifeline_end(run, armed).map_err(|e| format!("the lifeline: {e}"))?;
    let prog = instance_argv(run, &aterm, scale, stdin.is_some());
    let out = run.run.join("aterm.out");
    let mut pid = 0;
    let launched = if windowed(run) {
        launch_window(run, &vars, &prog, &out, stdin, (&mut pid, READY_HANG))
    } else {
        launch_ready(run, &vars, &prog, &out, stdin, (&mut pid, READY_HANG))
    };
    run.instance_pid = pid;
    run.save();
    launched.map_err(|e| {
        if e == "interrupted" {
            e
        } else {
            format!("no control socket came up: {e}")
        }
    })?;
    if procs::tty_of(run.instance_pid) != "??" {
        return Err(format!(
            "instance {} kept a controlling terminal",
            run.instance_pid
        ));
    }
    run.aterm_build = running_build(run);
    run.save();
    Ok(())
}

/// The build the instance of `run` runs, as it names itself (`ctl version`): `?` when it does
/// not say. The file at [`Run::aterm`] can be rebuilt while a lab runs, so only the process can
/// say which build ran.
fn running_build(run: &Run) -> String {
    Door::new(run)
        .and_then(|door| door.ctl(&["version"]))
        .ok()
        .filter(|reply| reply.code == 0)
        .and_then(|reply| build_named(&reply.stdout))
        .unwrap_or_else(|| "?".into())
}

/// `<version>+g<commit>` from a `ctl version` reply (`OK version=… build=… commit=… …`): the
/// token the instance's harness tapes carry as `build=` (`aterm_stamp::build_id`),
/// `-dirty` suffixed for a build of an uncommitted tree; `None` when either word is missing.
pub(crate) fn build_named(reply: &str) -> Option<String> {
    let line = reply.lines().next()?.strip_prefix("OK ")?;
    let word = |key| {
        crate::screen::field(line, key)
            .filter(|v| !v.is_empty() && v.bytes().all(|b| b.is_ascii_graphic()))
    };
    Some(format!("{}+g{}", word("version")?, word("commit")?))
}

/// Whether `pid` is a live process of the lab `run`.
#[must_use]
pub(crate) fn names_this_lab(run: &Run, pid: i32) -> bool {
    let marks = [
        format!("{}/", run.run.display()),
        format!(
            "{}/",
            if run.sockdir.is_empty() {
                run.run.display().to_string()
            } else {
                run.sockdir.clone()
            }
        ),
    ];
    alive(pid) && {
        let line = procs::command_line(pid);
        marks.iter().any(|m| line.contains(m.as_str()))
    }
}

/// `pid` while it is still the process that started at `start` ([`procs::start_time`]), else 0
/// (an unread start included, so nothing is signalled). [`down`] checks each saved pid once
/// ([`names_this_lab`]) and signals it again only after its TERM and a wait, when its exit is
/// expected and its number may already be another process's.
fn still_started(pid: i32, start: &str) -> i32 {
    if pid > 1 && !start.is_empty() && procs::start_time(pid) == start {
        pid
    } else {
        0
    }
}

/// Whether `up` may make the lab `run` names at `now`: what an earlier lab of the tag left is
/// stamped ([`stamp_holds`]) and swept ([`sweep_copies`]), and its bus keys removed
/// ([`drop_bus_keys`]).
/// # Errors
/// Why the tag cannot come up now.
fn room_for(run: &Run, now: std::time::SystemTime) -> Result<(), Fail> {
    let tag = &run.tag;
    if run.run.join("env.sh").exists() {
        return Err(Fail::die(format!(
            "lab '{tag}' is already up ({}): {LAB} down {tag} first ({LAB} ps lists what is running)",
            run.run.display()
        )));
    }
    let unstamped = !stamp_holds(&run.run);
    match sweep_copies(&run.run, relay_shown_running(run), unstamped, now).first() {
        Some(dir) => Err(Fail::die(format!(
            "lab '{tag}' cannot come up yet: {}",
            kept_copies(dir, "up", tag)
        ))),
        None => {
            drop_bus_keys(&run.run);
            Ok(())
        }
    }
}

/// Bring a lab up (nothing printed).
/// # Errors
/// Why it could not come up (whatever it had started is taken down first).
#[allow(clippy::too_many_lines)]
pub fn up(tag: &str, o: &UpOpts) -> Result<Run, Fail> {
    if let Some(why) = opts_refused(o) {
        return Err(Fail::usage(why));
    }
    stop::install();
    if !valid_tag(tag) {
        return Err(Fail::die(format!(
            "bad tag '{tag}' (letters, digits, _ and -, at most 24)"
        )));
    }
    let p = paths();
    let mut run = Run::new(tag);
    room_for(&run, std::time::SystemTime::now())?;
    run.instance_env = instance_env(&o.instance_env)
        .map_err(Fail::usage)?
        .iter()
        .map(|(k, v)| format!("{k}={v}"))
        .collect::<Vec<_>>()
        .join(" ");
    let aterm = p.need_aterm()?.to_path_buf();
    run.aterm = aterm.to_string_lossy().into_owned();
    need_sandbox()?;
    let homes = p.guarded_homes();
    if inside_any(&p.exe.to_string_lossy(), &homes, CCSOCKS) {
        return Err(Fail::die(format!(
            "the lab's own program sits inside the owner's data ({}): the jail would hide it from the stub; use a checkout elsewhere",
            p.exe.display()
        )));
    }
    if inside_any(&aterm.to_string_lossy(), &homes, CCSOCKS) {
        return Err(Fail::die(format!(
            "the aterm under test sits inside the owner's data ({}): the jail would hide it; build or copy it elsewhere",
            aterm.display()
        )));
    }
    if !o.standin {
        run.claude_bin = find_claude()?;
    }
    let stores: Vec<String> = homes
        .iter()
        .map(|h| format!("{h}/Library/Application Support/aterm/"))
        .collect();
    let outside = |what: &str, c: String| {
        if !stores.iter().any(|st| c.starts_with(st)) && inside_any(&c, &homes, CCSOCKS) {
            return Err(Fail::die(format!(
                "the {what} under test sits inside the owner's data ({c}): the jail would hide it"
            )));
        }
        Ok(c)
    };
    if !o.standin {
        if let Some(c) = find_codex() {
            run.codex_bin = outside("Codex", c)?;
        }
        if let Some(c) = find_agy() {
            run.agy_bin = outside("agy", c)?;
        }
        if let Some(c) = find_muse() {
            run.muse_bin = outside("Muse Code", c)?;
        }
        if let Some(c) = find_grok() {
            run.grok_bin = outside("Grok Build", c)?;
        }
        if let Some(c) = find_kimi() {
            run.kimi_bin = outside("Kimi Code", c)?;
        }
    }
    run.agent = "claude".into();
    run.cols.clone_from(&o.cols);
    run.rows.clone_from(&o.lines);
    run.seam = if o.restore { "1" } else { "0" }.into();
    run.route_seam = if o.route_seam { "1" } else { "0" }.into();
    run.window = if o.window { "1" } else { "0" }.into();
    let w = run.w.clone();
    if let Some(size) = &o.home_image {
        if let Some(why) =
            home_image_refused(size, aterm_agent::harness::disk::free_bytes(&p.state))
        {
            return Err(Fail::exit(3, why));
        }
        let home = w.join("home");
        let file = home_image_file(&run.run);
        let image = crate::disk_lane::Image::attach_at(&file, &home, size, "atermlabhome")
            .map_err(|e| abort(&run, &[], &format!("the home image: {e}")))?;
        crate::util::set_dir_mode_nofollow(&home, 0o700)
            .map_err(|e| abort(&run, &[], &format!("the home image's root: {e}")))?;
        image.leave();
        run.home_image = file.to_string_lossy().into_owned();
        run.save();
    }
    for d in crate::protected::SCRATCH_DIRS {
        fs::create_dir_all(w.join(d)).map_err(|e| {
            abort(
                &run,
                &[],
                &format!("cannot make {}: {e}", w.join(d).display()),
            )
        })?;
    }
    if o.reroute {
        let laid = crate::reroute_prefix::lay(
            &w.join("home"),
            Path::new(&run.claude_bin),
            Some(&run.run.join(HELD).join(REROUTE_HOLD)),
        )
        .map_err(|e| abort(&run, &[], &format!("the reroute prefix: {e}")))?;
        run.claude_bin = laid.exe.to_string_lossy().into_owned();
        run.reroute = "1".into();
    }
    if o.clock {
        crate::clock::lay(&mut run, &p.repo)
            .map_err(|e| abort(&run, &[], &format!("the sleep clock: {e}")))?;
    }
    let _ = crate::util::write_fresh(
        &w.join("home/.codex/LAB-DECOY"),
        format!(
            "lab decoy: Codex runs with CODEX_HOME={}; nothing may be written here\n",
            w.join("codex-home").display()
        ),
    );
    let _ = crate::util::write_fresh(
        &w.join(crate::agy_proof::DECOY),
        "lab decoy: agy runs with HOME=agy-home; nothing may be written here\n",
    );
    run.sockdir = crate::sys::make_temp_dir("/tmp/at.XXXX")
        .map_err(|e| abort(&run, &[], &format!("cannot make a socket dir: {e}")))?;
    run.sock = format!("{}/a.sock", run.sockdir);
    run.proj = format!("{}/proj", run.sockdir);
    let _ = fs::create_dir_all(&run.proj);
    let bin = run.run.join(BIN);
    let made = fs::create_dir_all(run.run.join("bin"))
        .and_then(|()| crate::util::copy_regular_file(&p.exe, &bin).map(|_| ()))
        .and_then(|()| fs::create_dir_all(run.run.join("shim")))
        .and_then(|()| std::os::unix::fs::symlink(&bin, run.run.join("shim/security")))
        .and_then(|()| std::os::unix::fs::symlink(&bin, run.run.join("shim/open")))
        .and_then(|()| std::os::unix::fs::symlink(&aterm, run.run.join("shim/aterm")))
        .and_then(|()| std::os::unix::fs::symlink(&aterm, run.run.join("shim/amacs")));
    if let Err(e) = made {
        return Err(abort(
            &run,
            &[],
            &format!("cannot lay the lab's program in {}: {e}", run.run.display()),
        ));
    }
    if o.standin {
        run.claude_bin = crate::occupancy_standin::lay(&run.run, &p.exe)
            .map_err(|e| abort(&run, &[], &format!("cannot lay the stand-in: {e}")))?
            .to_string_lossy()
            .into_owned();
    }
    run.mark = crate::jail::new_mark(tag);
    if o.jail_scan {
        run.jail_scan = "1".into();
    }
    let mut spec = crate::jail::Spec::for_lab(
        &run.run.to_string_lossy(),
        &run.claude_bin,
        &run.codex_bin,
        &run.sockdir,
        &run.mark,
    );
    spec.agy.clone_from(&run.agy_bin);
    spec.muse.clone_from(&run.muse_bin);
    spec.grok.clone_from(&run.grok_bin);
    spec.kimi.clone_from(&run.kimi_bin);
    let jail = crate::jail::render(&spec).map_err(|e| abort(&run, &[], &e))?;
    crate::util::write_fresh(&run.run.join("jail.sb"), jail)
        .map_err(|e| abort(&run, &[], &e.to_string()))?;
    if let Err(e) = crate::jail::check(&run.run.join("jail.sb")) {
        eprintln!("aterm-lab: {e}");
        return Err(abort(
            &run,
            &[],
            "the lab's sandbox profile does not apply here",
        ));
    }
    run.save();
    let witness_life = lifeline_end(&run, o.lifeline)
        .map_err(|e| abort(&run, &[], &format!("the witness's lifeline: {e}")))?;
    let mut sampler = aterm_uds::spawnfd::command(&bin);
    sampler.arg("sampler").arg(&run.run);
    match witness_life {
        Some(far) => {
            sampler.args(lifeline_flag()).stdin(Stdio::from(far));
        }
        None => {
            sampler.stdin(Stdio::null());
        }
    }
    sampler
        .setsid(true)
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    run.sampler_pid = procs::spawn_kept(&mut sampler)
        .map_err(|e| abort(&run, &[], &format!("cannot start the egress witness: {e}")))?;
    run.save();
    // The bus before the stub: nothing else of the lab runs in the jail yet, so what `up` makes
    // in the socket dir for it is the lab's own.
    let fabric_table = if o.fabric {
        match stand_bus(&mut run, o.lifeline) {
            Ok(table) => Some(table),
            Err(e) => {
                return Err(abort(
                    &run,
                    &[run.sampler_pid, run.broker_pid],
                    &format!("the lab's bus: {e}"),
                ));
            }
        }
    } else {
        None
    };
    if o.tab_strip {
        run.tab_strip = "1".into();
    }
    let _ = crate::util::write_fresh(&w.join("mode"), "ok\n");
    let _ = crate::util::write_fresh(&w.join("reply.txt"), "");
    let _ = crate::util::write_fresh(&w.join("security.log"), "");

    let ws = |f: &str| w.join(f).to_string_lossy().into_owned();
    let stub_prog: Vec<String> = [
        bin.to_string_lossy().as_ref(),
        "stub",
        "--port",
        "0",
        "--port-file",
        &ws("stub.port"),
        "--proxy-port",
        "0",
        "--proxy-port-file",
        &ws("proxy.port"),
        "--mode-file",
        &ws("mode"),
        "--modes-dir",
        &ws("modes"),
        "--log",
        &ws("stub.log"),
        "--dump-dir",
        &ws("dump"),
        "--reply-file",
        &ws("reply.txt"),
        "--tool-file",
        &ws("tool.json"),
        "--headers-file",
        &ws("headers.json"),
        "--account-file",
        &ws("account.json"),
        "--auth-modes-file",
        &ws(crate::verbs::AUTH_MODES),
    ]
    .map(str::to_string)
    .to_vec();
    let stub_vars = [("PATH", "/usr/bin:/bin"), ("LANG", "en_US.UTF-8")]
        .map(|(k, v)| (k.to_string(), v.to_string()));
    let stdin = lifeline_end(&run, o.lifeline)
        .map_err(|e| abort(&run, &[run.sampler_pid], &format!("the lifeline: {e}")))?;
    let stub_prog: Vec<String> = if stdin.is_some() {
        stub_prog.into_iter().chain(lifeline_flag()).collect()
    } else {
        stub_prog
    };
    let mut pid = 0;
    let launched = launch_ready(
        &run,
        &stub_vars,
        &stub_prog,
        &run.run.join("stub.out"),
        stdin,
        (&mut pid, aterm_uds::deadline::FIRST_EXEC_HANG),
    );
    run.stub_pid = pid;
    run.save();
    if launched.is_ok() {
        let _ = crate::util::atomic_write(&run.run.join(BIN_RAN), b"");
    }
    let started = [run.sampler_pid, run.stub_pid];
    if let Err(e) = launched {
        if e == "interrupted" {
            return Err(abort(&run, &started, "interrupted"));
        }
        eprintln!("aterm-lab: the stub: {e}");
    }
    let filled =
        |f: &str| fs::symlink_metadata(w.join(f)).is_ok_and(|m| m.is_file() && m.len() > 0);
    if !(filled("stub.port") && filled("proxy.port")) {
        eprint!(
            "{}",
            crate::util::read_regular_text(&run.run.join("stub.out")).unwrap_or_default()
        );
        return Err(abort(&run, &started, "the stub did not start"));
    }
    let read_port = |f: &str| {
        crate::util::scratch_text(&w.join(f))
            .trim()
            .chars()
            .filter(char::is_ascii_digit)
            .collect::<String>()
    };
    run.port = read_port("stub.port");
    run.pport = read_port("proxy.port");

    let helper = run.run.join("apikey.sh");
    let script = format!(
        "#!/bin/sh\n# the lab's fake API key (the stub accepts anything); nothing real\nprintf \"%s\\n\" \"{}\"\n",
        api_key()
    );
    let _ = crate::util::atomic_write_mode(&helper, script.as_bytes(), 0o755);
    let witness = run.run.join(crate::settings_witness::SCRIPT);
    let _ = crate::util::atomic_write_mode(
        &witness,
        crate::settings_witness::script(&w.join(crate::settings_witness::LOG)).as_bytes(),
        0o755,
    );
    run.proxy = if o.proxy_dead {
        "http://127.0.0.1:9".into()
    } else {
        format!("http://127.0.0.1:{}", run.pport)
    };
    run.save();
    let proj_real =
        fs::canonicalize(&run.proj).map_or(run.proj.clone(), |p| p.to_string_lossy().into_owned());
    let (settings, claude_json) =
        scratch_claude_config(&run, &helper.to_string_lossy(), &proj_real, o);
    let put = |rel: &str, text: &str| {
        let _ = crate::util::atomic_write(&w.join(rel), text.as_bytes());
    };
    put("home/.claude/settings.json", &settings);
    put("home/.claude/.claude.json", &claude_json);
    if o.reroute {
        // A reroute lab's `claude` is found by name too, outside the lab's launcher, which sets
        // `CLAUDE_CONFIG_DIR`: a Claude Code that aterm starts with no starter of its own (the
        // disk worker's plain `claude`) reads the default `~/.claude.json`, so it is laid as well,
        // or that Claude Code opens on its first-run onboarding.
        put("home/.claude.json", &claude_json);
    }
    put(
        "cfg/aterm/aterm.toml",
        &scratch_aterm_toml(
            &o.aterm_toml,
            &o.harness_keys,
            fabric_table.as_deref(),
            o.tab_strip,
        ),
    );
    put_tab_shell(&w, &run.port);

    if let Err(e) = start_instance(&mut run, o.scale.as_deref(), o.lifeline) {
        eprint!(
            "{}",
            crate::util::read_regular_text(&run.run.join("aterm.out")).unwrap_or_default()
        );
        let msg = if e == "interrupted" {
            e
        } else {
            format!("the private instance did not come up ({e})")
        };
        return Err(abort(
            &run,
            &[run.instance_pid, run.stub_pid, run.sampler_pid],
            &msg,
        ));
    }
    if procs::tty_of(run.stub_pid) != "??" {
        return Err(abort(
            &run,
            &[run.instance_pid, run.stub_pid, run.sampler_pid],
            &format!("process {} kept a controlling terminal", run.stub_pid),
        ));
    }
    let logs = w.join("state/logs");
    if !logs.is_dir() {
        let _ = down(&run.tag, false);
        return Err(Fail::die(format!(
            "the instance did not take ATERM_STATE_HOME ({} was built without the dev seams): targo --unverified build -p aterm --features dev-seams",
            aterm.display()
        )));
    }
    if o.fabric
        && let Err(e) = await_bridge(&run)
    {
        let said = broker_said(&run);
        let _ = down(&run.tag, false);
        return Err(Fail::die(format!(
            "the lab's bridge did not connect ({e}); its broker said: {said}"
        )));
    }
    let clone = Path::new(&run.claude_bin) == run.run.join(CLOCK_CLONE);
    // the clock's clone counts as started from here on
    if clone
        && let Err(e) = fs::remove_file(run.run.join(CLOCK_UNSTARTED))
        && e.kind() != std::io::ErrorKind::NotFound
    {
        let _ = down(&run.tag, false);
        return Err(Fail::die(format!(
            "cannot clear {}: {e}",
            run.run.join(CLOCK_UNSTARTED).display()
        )));
    }
    run.claude_ver = jailed_version(&run, &run.claude_bin, |l| l.split_whitespace().next());
    if run.claude_ver != "?" && clone {
        let _ = crate::util::atomic_write(&run.run.join(CLOCK_RAN), b"");
    }
    if !run.codex_bin.is_empty() {
        run.codex_ver = codex_version(&run, &run.codex_bin);
    }
    if !run.agy_bin.is_empty() {
        run.agy_ver = agy_version(&run, &run.agy_bin);
    }
    if !run.muse_bin.is_empty() {
        run.muse_ver = muse_version(&run, &run.muse_bin);
    }
    if !run.grok_bin.is_empty() {
        run.grok_ver = grok_version(&run.grok_bin);
    }
    if !run.kimi_bin.is_empty() {
        run.kimi_ver = kimi_version(&run, &run.kimi_bin);
    }
    run.save();
    Ok(run)
}

/// A lab's scratch `aterm.toml`: `LAB_ATERM_TOML`, the update and package lanes off,
/// `LAB_HARNESS_KEYS` under `[harness]`, the lab bus's `[fabric]` table when `up --fabric` stood
/// one ([`crate::bus::Laid::table`]), `tab_strip_rows = 1` at its head with `up --tab-strip`, and
/// the audible bell off unless `LAB_ATERM_TOML` sets `bell_sound` ([`crate::instance::bell_off`]).
pub(crate) fn scratch_aterm_toml(
    aterm_toml: &str,
    harness_keys: &str,
    fabric: Option<&str>,
    tab_strip: bool,
) -> String {
    let mut text = format!(
        "{aterm_toml}\n[update]\nenabled = false\n[packages]\nenabled = false\n[harness]\nprobe_api = false\n{harness_keys}\n"
    );
    if let Some(table) = fabric {
        text.push_str(table);
    }
    if tab_strip {
        text = format!("tab_strip_rows = 1\n{text}");
    }
    crate::instance::bell_off(&text)
}

/// The scratch `settings.json` and `.claude.json` texts.
fn scratch_claude_config(run: &Run, helper: &str, proj: &str, o: &UpOpts) -> (String, String) {
    let pretty = crate::pyjson::Style {
        indent: Some(2),
        compact: false,
        ascii: true,
    };
    let mut env = J::obj([
        (
            "ANTHROPIC_BASE_URL",
            J::s(format!("http://127.0.0.1:{}", run.port)),
        ),
        ("HTTP_PROXY", J::s(run.proxy.as_str())),
        ("HTTPS_PROXY", J::s(run.proxy.as_str())),
        ("NO_PROXY", J::s("127.0.0.1,localhost")),
        ("DISABLE_AUTOUPDATER", J::s("1")),
        (
            "CLAUDE_CODE_DISABLE_OFFICIAL_MARKETPLACE_AUTOINSTALL",
            J::s("1"),
        ),
    ]);
    if o.survey {
        env.set("CLAUDE_FORCE_DISPLAY_SURVEY", J::s("1"));
    }
    let hooks = crate::settings_witness::hooks(&run.run.join(crate::settings_witness::SCRIPT));
    let settings = J::obj([
        ("apiKeyHelper", J::s(helper)),
        ("env", env),
        ("hooks", hooks),
    ]);
    let mut cj = J::obj([
        ("hasCompletedOnboarding", J::Bool(true)),
        ("theme", J::s("dark")),
        ("lastOnboardingVersion", J::s("99.0.0")),
    ]);
    if !o.first_run {
        cj.set("hasSeenAutoDefaultNotice", J::Bool(true));
        cj.set("hasSeenAutoDefaultNudge", J::Bool(true));
    }
    if o.trust {
        let entry = J::obj([
            ("allowedTools", J::Arr(Vec::new())),
            ("mcpContextUris", J::Arr(Vec::new())),
            ("mcpServers", J::obj([])),
            ("enabledMcpjsonServers", J::Arr(Vec::new())),
            ("disabledMcpjsonServers", J::Arr(Vec::new())),
            ("hasTrustDialogAccepted", J::Bool(true)),
            ("hasClaudeMdExternalIncludesApproved", J::Bool(false)),
            ("hasClaudeMdExternalIncludesWarningShown", J::Bool(false)),
        ]);
        cj.set("projects", J::Obj(vec![(proj.to_string(), entry)]));
    }
    (settings.dump(pretty), cj.dump(pretty))
}

/// `up [tag] [--window] [--fabric] [--tab-strip]`: the existing jailed instance, headless by
/// default.
pub fn cmd_up(args: &[String]) -> Res {
    let mut tag = None;
    let mut o = UpOpts::from_env();
    for arg in args {
        match arg.as_str() {
            "--window" => o.window = true,
            "--fabric" => o.fabric = true,
            "--tab-strip" => o.tab_strip = true,
            _ if arg.starts_with('-') => {
                return Err(Fail::usage(format!("up: unknown option '{arg}'")));
            }
            _ if tag.is_none() => tag = Some(arg.clone()),
            _ => {
                return Err(Fail::usage(
                    "up: takes one optional tag beside --window, --fabric and --tab-strip",
                ));
            }
        }
    }
    let tag = tag.unwrap_or_else(crate::run::default_tag);
    let run = up(&tag, &o)?;
    let aterm = run.aterm.clone();
    println!("lab '{}' is up", run.tag);
    println!(
        "  stub        http://127.0.0.1:{} (pid {})   black-hole proxy {}",
        run.port, run.stub_pid, run.proxy
    );
    println!(
        "  instance    pid {}   socket {}   {}x{}   {aterm} ({})",
        run.instance_pid, run.sock, run.cols, run.rows, run.aterm_build
    );
    println!("  claude      {} ({})", run.claude_bin, run.claude_ver);
    if !run.fabric.is_empty() {
        let live = bus_live(&run);
        println!(
            "  fabric      broker {} (pid {}, fleet {}, node {})   observer cap {}   bridge connected",
            live.join("b.sock").display(),
            run.broker_pid,
            crate::bus::FLEET,
            jail_written(&live.join("state/node")).trim(),
            bus_keys(&run).join("observer.cap").display()
        );
    }
    if run.tab_strip == "1" {
        println!(
            "  tab strip   1 row (the tab menu holds the session clause: aterm ctl --sock {} open tab-menu)",
            run.sock
        );
    }
    if run.reroute == "1" {
        println!(
            "  reroute     the instance reroutes; `{LAB} claude {} --by-name` runs the stub, the twin, then that build",
            run.tag
        );
    }
    if run.clock == "1" {
        let l = crate::clock::Layout::of(&run);
        println!(
            "  clock       {}   offset {} (0 s)   records {}{}",
            l.lib.display(),
            l.offset.display(),
            l.records.display(),
            if Path::new(&run.claude_bin) == l.claude {
                "   (Claude Code: a clone signed ad hoc, no hardened runtime)"
            } else {
                ""
            }
        );
    }
    if run.codex_bin.is_empty() {
        println!(
            "  codex       none managed here ({LAB} codex needs one: aterm pkg install codex, or LAB_CODEX)"
        );
    } else {
        println!("  codex       {} ({})", run.codex_bin, run.codex_ver);
    }
    for (name, bin, ver, how) in [
        (
            "agy",
            &run.agy_bin,
            &run.agy_ver,
            "aterm pkg install agy, or LAB_AGY",
        ),
        (
            "muse",
            &run.muse_bin,
            &run.muse_ver,
            "aterm pkg install muse, or LAB_MUSE",
        ),
        (
            "grok",
            &run.grok_bin,
            &run.grok_ver,
            "LAB_GROK=<a scratch store's build>",
        ),
        (
            "kimi",
            &run.kimi_bin,
            &run.kimi_ver,
            "aterm pkg install kimi, or LAB_KIMI",
        ),
    ] {
        if bin.is_empty() {
            println!("  {name:<11} none here ({LAB} {name} needs one: {how})");
        } else {
            println!("  {name:<11} {bin} ({ver})");
        }
    }
    println!(
        "  scratch     {}/{{home,cfg,state}}   project {}",
        run.w.display(),
        run.proj
    );
    println!(
        "  next        {LAB} claude {0} ; {LAB} turn {0} hi ; {LAB} text {0}",
        run.tag
    );
    Ok(0)
}

/// Take a lab down: everything in the lab's jail ended, the logs kept, and the scratch removed
/// once no hold keeps it ([`sweep_copies`]).
/// # Errors
/// No such lab, or an `env.sh` whose scratch is not its own.
pub fn down(tag: &str, keep_dumps: bool) -> Result<PathBuf, Fail> {
    let mut r = Run::read(tag)?;
    if r.w != r.run.join("scratch") {
        return Err(Fail::die(format!(
            "env.sh of lab '{tag}' names a scratch dir that is not {}/scratch ({}): not touching it",
            r.run.display(),
            r.w.display()
        )));
    }
    let ours = [r.instance_pid, r.stub_pid, r.sampler_pid].map(|pid| names_this_lab(&r, pid));
    if !ours[0] {
        r.instance_pid = 0;
    }
    if !ours[1] {
        r.stub_pid = 0;
    }
    if !ours[2] {
        r.sampler_pid = 0;
    }
    let started = [r.instance_pid, r.stub_pid, r.sampler_pid].map(procs::start_time);
    let pids = procs::jail_members(&r);
    let mut recorded = crate::runtime_dirs::read_record(&r.run)
        .into_iter()
        .collect();
    crate::runtime_dirs::witness(&r.run, &pids, &mut recorded);
    for &p in &pids {
        if p != r.stub_pid {
            let _ = crate::sys::kill(p, libc::SIGTERM);
        }
    }
    if alive(r.stub_pid) {
        let _ = crate::sys::kill(r.stub_pid, libc::SIGTERM);
    }
    let end = std::time::Instant::now() + Duration::from_secs(4);
    while pids.iter().any(|&p| alive(p)) && std::time::Instant::now() < end {
        std::thread::sleep(Duration::from_millis(50));
    }
    r.instance_pid = still_started(r.instance_pid, &started[0]);
    r.stub_pid = still_started(r.stub_pid, &started[1]);
    for p in procs::jail_members(&r) {
        if alive(p) {
            eprintln!("aterm-lab: pid {p} ignored TERM -> KILL");
            let _ = crate::sys::kill(p, libc::SIGKILL);
        }
    }
    let end = std::time::Instant::now() + Duration::from_secs(2);
    let mut left = procs::jail_members(&r);
    while !left.is_empty() && std::time::Instant::now() < end {
        std::thread::sleep(Duration::from_millis(100));
        left = procs::jail_members(&r);
    }
    if !left.is_empty() {
        let l: Vec<String> = left.iter().map(ToString::to_string).collect();
        eprintln!("aterm-lab: still in the jail after KILL: {}", l.join(" "));
    }
    let unstamped = !stamp_holds(&r.run);
    r.sampler_pid = still_started(r.sampler_pid, &started[2]);
    if alive(r.sampler_pid) {
        let _ = crate::sys::kill(r.sampler_pid, libc::SIGTERM);
        let end = std::time::Instant::now() + Duration::from_secs(2);
        while alive(r.sampler_pid) && std::time::Instant::now() < end {
            std::thread::sleep(Duration::from_millis(50));
        }
        if still_started(r.sampler_pid, &started[2]) != 0 {
            eprintln!("aterm-lab: pid {} ignored TERM -> KILL", r.sampler_pid);
            let _ = crate::sys::kill(r.sampler_pid, libc::SIGKILL);
        }
    }
    if left.is_empty() {
        let took = crate::runtime_dirs::clean(&r.run);
        if !took.is_empty() {
            eprintln!(
                "aterm-lab: removed what this run left in the shared /tmp: {}",
                took.join(" ")
            );
        }
    }
    let keep = paths()
        .state
        .join("logs")
        .join(format!("{tag}-{}", crate::util::compact_stamp(now_secs())));
    let _ = fs::create_dir_all(&keep);
    for f in [
        "sockets.log",
        "egress.log",
        "aterm.out",
        "stub.out",
        "env.sh",
        "jail.sb",
        "claude.sh",
        "codex.sh",
        "codex-run.sh",
        "agy.sh",
        "muse.sh",
        "grok.sh",
        "jailed.pids",
        crate::workers::WORKER_TEXTS,
        "probe.txt",
        "lsof.txt",
        crate::runtime_dirs::RECORD,
    ] {
        keep_file(&r.run.join(f), &keep.join(f));
    }
    let relay_ran = relay_shown_running(&r);
    for f in [
        "stub.log",
        "relay.log",
        "open.log",
        crate::settings_witness::LOG,
        crate::occupancy_standin::LOG,
    ] {
        if let Ok(Some(b)) = crate::sys::scratch_read(&r.w.join(f)) {
            let _ = crate::util::write_fresh(&keep.join(f), b);
        }
    }
    for (f, to) in [
        ("claude-debug.log", "claude-debug.log"),
        ("statusline.jsonl", "statusline.jsonl"),
    ] {
        keep_file(&r.w.join(f), &keep.join(to));
    }
    keep_file(&r.claude_json(), &keep.join("claude.json"));
    for (_, log) in crate::stats::chain_debug_logs(&r) {
        if let Some(name) = log.file_name() {
            keep_file(&log, &keep.join(name));
        }
    }
    keep_tree(
        &crate::forever::input_hold::dir_of(&r),
        &keep.join(crate::forever::input_hold::DIR),
    );
    let projects = r.claude_dir().join("projects");
    let mut transcripts = Vec::new();
    crate::util::walk_files(&projects, &mut transcripts);
    for p in transcripts
        .iter()
        .filter(|p| p.extension().is_some_and(|e| e == "jsonl"))
    {
        if let Ok(rel) = p.strip_prefix(&projects) {
            let to = keep.join("claude-projects").join(rel);
            if let Some(d) = to.parent() {
                let _ = fs::create_dir_all(d);
            }
            keep_file(p, &to);
        }
    }
    for d in ["sessions", "log"] {
        keep_tree(
            &r.w.join("codex-home").join(d),
            &keep.join(format!("codex-{d}")),
        );
    }
    keep_tree(
        &r.w.join("agy-home/.gemini/antigravity-cli/log"),
        &keep.join("agy-log"),
    );
    keep_tree(&r.w.join("tmp"), &keep.join("tmp"));
    keep_tree(
        &r.w.join("home/.local/share/muse/sessions"),
        &keep.join("muse-sessions"),
    );
    keep_tree(&r.w.join(crate::grok::HOME), &keep.join("grok-home"));
    let sec = r.w.join("security.log");
    if let Ok(Some(b)) = crate::sys::scratch_read(&sec) {
        let masked: String = String::from_utf8_lossy(&b)
            .lines()
            .map(|l| format!("{}\n", mask_account(l)))
            .collect();
        let _ = crate::util::write_fresh(&keep.join("security.log"), masked);
    }
    keep_tree(&r.state().join("drive"), &keep.join("drive"));
    keep_tree(&r.run.join(LAUNCHES), &keep.join(LAUNCHES));
    crate::clock::keep(&r, &keep.join("clock"));
    keep_tree(&r.state().join("logs"), &keep.join("state-logs"));
    if !r.fabric.is_empty() {
        // The jail writes the bus's live dir: read through no link, before the socket dir goes.
        let live = crate::paths::physical(&bus_live(&r));
        for (f, to) in [
            ("broker.out", "fabric-broker.out"),
            ("b.log", "fabric-b.log"),
        ] {
            if let Ok(Some(b)) = crate::sys::scratch_read(&live.join(f)) {
                let _ = crate::util::write_fresh(&keep.join(to), b);
            }
        }
    }
    if keep_dumps {
        keep_tree(&r.w.join("dump"), &keep.join("dump"));
    }
    if r.sockdir.starts_with("/tmp/at.") {
        let _ = fs::remove_dir_all(&r.sockdir);
    }
    for d in ["shim", "gbin"] {
        let _ = fs::remove_dir_all(r.run.join(d));
    }
    // Once the logs above are kept (the transcripts they copy lie on a home image), and only with
    // the scratch, once no hold keeps a copy there: the sweep detaches a home image.
    clear_run_dir(
        &r.run,
        relay_ran,
        unstamped,
        tag,
        std::time::SystemTime::now(),
    );
    Ok(keep)
}

/// What `lab up` writes for its tab shells in scratch `w` ([`tab_profile`]).
fn put_tab_shell(w: &Path, port: &str) {
    let _ = crate::util::atomic_write(&w.join("home/.zshrc"), tab_profile(port).as_bytes());
}

/// The lab's tab shells' `.zshrc`: a Gemini API client typed in a tab (agy, which reads
/// these) is pointed at the stub with the lab's fake key ([`crate::agy::LAB_GEMINI_KEY`]),
/// never Google's.
#[must_use]
pub fn tab_profile(port: &str) -> String {
    let mut s = String::from("PROMPT='lab$ '\n");
    s.push_str(
        "unset GOOGLE_API_KEY GOOGLE_GENAI_USE_VERTEXAI GOOGLE_CLOUD_PROJECT GOOGLE_APPLICATION_CREDENTIALS\n",
    );
    s.push_str(&format!(
        "export GEMINI_API_KEY={} GOOGLE_GEMINI_BASE_URL=http://127.0.0.1:{port}\n",
        crate::agy::LAB_GEMINI_KEY
    ));
    s
}

/// `account="…"` masked as `account="<user>"`.
#[must_use]
pub fn mask_account(line: &str) -> String {
    let mut out = String::new();
    let mut rest = line;
    while let Some(i) = rest.find("account=\"") {
        out.push_str(&rest[..i]);
        out.push_str("account=\"<user>\"");
        let after = &rest[i + 9..];
        rest = after.find('"').map_or("", |j| &after[j + 1..]);
    }
    out.push_str(rest);
    out
}

/// `down [tag] [--keep-dumps]`.
pub fn cmd_down(args: &[String]) -> Res {
    let mut args = args.to_vec();
    let tag = take_only_tag(&mut args)?;
    let keep_dumps = args.first().map(String::as_str) == Some("--keep-dumps");
    let keep = down(&tag, keep_dumps)?;
    println!("lab '{tag}' is down; logs kept in {}", keep.display());
    Ok(0)
}

/// `ps`'s line for an `up --fabric` lab's broker, `live` or gone. A gone one names the way back:
/// `down`, then `up` of the tag with the flags it came up with (`--window`, `--fabric`,
/// `--tab-strip`, as its `env.sh` records them).
fn broker_line(r: &Run, live: bool) -> String {
    if live {
        return format!(
            "fabric broker {} live (in the jail; its socket {}/b.sock)",
            r.broker_pid, r.fabric
        );
    }
    let mut again = format!("{LAB} up {}", r.tag);
    for (on, flag) in [
        (windowed(r), "--window"),
        (true, "--fabric"),
        (r.tab_strip == "1", "--tab-strip"),
    ] {
        if on {
            again.push(' ');
            again.push_str(flag);
        }
    }
    format!(
        "fabric broker {} gone: `link broker` exits when the aterm it runs is rebuilt, and the lab \
         has no supervisor to start it again ({LAB} down {}, then {again})",
        r.broker_pid, r.tag
    )
}

/// One lab's pids, and which of them live.
fn ps_one(tag: &str) -> Result<(), Fail> {
    let r = Run::read(tag)?;
    let roots: Vec<String> = procs::jailed_roots(&r.run, false)
        .iter()
        .map(ToString::to_string)
        .collect();
    let mut live: Vec<String> = Vec::new();
    for p in [r.stub_pid, r.sampler_pid]
        .into_iter()
        .chain(procs::lab_tree(&r))
    {
        if alive(p) && !live.contains(&p.to_string()) {
            live.push(p.to_string());
        }
    }
    println!(
        "lab {tag}: stub {}  instance {}  sampler {}{}  ({})",
        r.stub_pid,
        r.instance_pid,
        r.sampler_pid,
        if roots.is_empty() {
            String::new()
        } else {
            format!("  jailed {}", roots.join(" "))
        },
        r.run.display()
    );
    if !r.fabric.is_empty() {
        println!(
            "  {}",
            broker_line(&r, alive(r.broker_pid) && names_this_lab(&r, r.broker_pid))
        );
    }
    if live.is_empty() {
        println!("  nothing of it is alive: {LAB} down {tag} removes its run directory");
    } else {
        for l in ps_rows(&live) {
            println!("  {l}");
        }
    }
    Ok(())
}

/// `ps`'s rows for `live`, its header first, read a pid at a time: asked for two or
/// more pids at once, a `ps` whose own device read is wedged never returns, while each
/// pid asked alone answers (the kproc oracle's lesson in `aterm-agent`). A pid `ps`
/// cannot read contributes no row.
fn ps_rows(live: &[String]) -> Vec<String> {
    let mut rows = Vec::new();
    for pid in live {
        let Ok(out) = aterm_uds::spawnfd::command("/bin/ps")
            .args(["-o", "pid,ppid,etime,comm", "-p", pid])
            .output()
        else {
            continue;
        };
        for (i, l) in String::from_utf8_lossy(&out.stdout).lines().enumerate() {
            if i == 0 && !rows.is_empty() {
                continue;
            }
            rows.push(l.to_string());
        }
    }
    rows
}

/// `ps [tag]`: that lab, or (no argument) every lab that has a run directory.
pub fn cmd_ps(args: &[String]) -> Res {
    if !args.is_empty() {
        let mut a = args.to_vec();
        ps_one(&take_only_tag(&mut a)?)?;
        return Ok(0);
    }
    let tags = all_runs(&paths().state);
    if tags.is_empty() {
        println!(
            "no lab has a run directory under {}/runs",
            paths().state.display()
        );
    }
    for t in tags {
        if let Err(f) = ps_one(&t) {
            eprintln!("aterm-lab: {}", f.msg.unwrap_or_default());
        }
    }
    Ok(0)
}

/// The proxy variables a launch on the stub's route alone unsets ([`Launch::stub_route_alone`]),
/// each a route of its own to the instance.
pub const PROXY_KEYS: [&str; 6] = [
    "HTTP_PROXY",
    "HTTPS_PROXY",
    "http_proxy",
    "https_proxy",
    "ALL_PROXY",
    "all_proxy",
];

/// How `claude` launches.
#[derive(Clone, Debug, Default)]
pub struct Launch {
    /// Replace the private interactive shell, so job control cannot reclaim a held TUI.
    pub replace_shell: bool,
    /// `-e K=V`: extra environment.
    pub envs: Vec<String>,
    /// `--no-wait`.
    pub no_wait: bool,
    /// Emit a jailed child launch plan without typing into the preparation host.
    pub prepare_only: bool,
    /// `--relay`: behind the PTY relay, on the fullscreen renderer.
    pub relay: bool,
    /// `--subscriber`: a fake claude.ai login.
    pub subscriber: bool,
    /// `--stored`: the lab's stored login ([`crate::forever::sysmsg::login::lay_stored`]), as a
    /// person's `/login` leaves one, on [`Self::stub_route_alone`], in a login directory of its
    /// own ([`stored_login_dir`]).
    pub stored: bool,
    /// `--own`: a person's own claude.ai login as aterm's managed launch runs it, the one line E
    /// claims (`docs/DESIGN-harness-glass-rainbow-2026-11-05.md` §7 step 4): the lab's stored
    /// login ([`lay_own_login`]) in the scratch HOME's own `.claude` with no `CLAUDE_CONFIG_DIR`,
    /// on [`Self::stub_route_alone`], and aterm's usage tap on its line
    /// ([`crate::forever::tap::tap_args`], the `--settings` the managed twin's launch carries).
    /// The harness then reads the process as the default home's own account (`auth=stored`), and
    /// the tap's row as its own. `--subscriber`'s token is a credential from outside, whose
    /// figures the harness credits to no account, and `--stored`'s login directory is a home of
    /// its own, whose records no footer reads: neither shows line E.
    pub own: bool,
    /// The stub's base URL its only route: no black-hole proxy in the environment, so a stored
    /// login's process is the default route's where the instance's seam admits that base URL
    /// ([`UpOpts::route_seam`]); the jail still denies every connection but loopback.
    pub stub_route_alone: bool,
    /// `--debug-log`: Claude Code's debug log into the scratch ([`crate::stats::debug_log`]).
    pub debug_log: bool,
    /// `--by-name`: `claude` found on `PATH`, not the binary by path: in a `LAB_REROUTE=1` lab the
    /// reroute stub, which execs the `agents/` twin, which execs the store build.
    pub by_name: bool,
    /// `-- <claude args>`.
    pub args: Vec<String>,
}

/// Parse `claude`'s options.
/// # Errors
/// An unknown option, or `-e` with no value.
pub fn parse_launch(args: &[String]) -> Result<Launch, String> {
    let mut l = Launch::default();
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "-e" => {
                let kv = it.next().cloned().ok_or("claude: -e needs K=V")?;
                let name = kv.split_once('=').map_or(kv.as_str(), |(k, _)| k);
                if crate::clock::is_clock_var(name) {
                    return Err(format!(
                        "claude: -e {name}: the sleep clock's variables are the lab's own (LAB_CLOCK=1 at up)"
                    ));
                }
                l.envs.push(kv);
            }
            "--no-wait" => l.no_wait = true,
            "--prepare-only" => l.prepare_only = true,
            "--relay" => l.relay = true,
            "--subscriber" => l.subscriber = true,
            "--stored" => {
                l.stored = true;
                l.stub_route_alone = true;
            }
            "--own" => {
                l.own = true;
                l.stub_route_alone = true;
            }
            "--debug-log" => l.debug_log = true,
            "--by-name" => l.by_name = true,
            "--" => {
                l.args.extend(it.by_ref().cloned());
            }
            other => {
                return Err(format!(
                    "claude: unknown option '{other}' (use -e K=V, --no-wait, --prepare-only, --relay, --subscriber, --stored, --own, --debug-log, --by-name, -- <claude args>)"
                ));
            }
        }
    }
    if [l.stored, l.subscriber, l.own]
        .into_iter()
        .filter(|on| *on)
        .count()
        > 1
    {
        return Err("claude: --stored, --subscriber and --own are three logins; give one".into());
    }
    Ok(l)
}

/// One launch of Claude Code as its parts, in order: what [`launcher`] writes as `claude.sh` and
/// [`typed_line`] types as a person would.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct LaunchWords {
    /// The relay's words before the program (`<relay> --log <L> --`), as shell words, when the
    /// launch is behind it.
    pub relay: Vec<String>,
    /// The environment the lab sets, as the launcher's lines: each pair a name and the shell word
    /// of its value (`PATH` keeps its `"<shim>:$PATH"` form).
    pub env: Vec<Vec<(String, String)>>,
    /// The `K=V` words after them, one per launcher line: the sleep clock's, each `-e`, the
    /// relay's renderer and the subscriber's token, as given.
    pub extra: Vec<String>,
    /// The program word: `claude` by name, else the binary by path (a shell word).
    pub program: String,
    /// Its arguments, as shell words.
    pub args: Vec<String>,
}

impl LaunchWords {
    /// Every name the launch sets, in order.
    #[must_use]
    pub fn names(&self) -> Vec<String> {
        self.env
            .iter()
            .flatten()
            .map(|(k, _)| k.clone())
            .chain(self.extra.iter().map(|kv| {
                kv.split_once('=')
                    .map_or(kv.as_str(), |(k, _)| k)
                    .to_string()
            }))
            .collect()
    }
}

/// The `--settings` value that puts aterm's usage tap on an `--own` launch's line, as the managed
/// twin's launch carries it ([`aterm_agent::harness::tap::settings_value`] of the lab's aterm);
/// `None` where that aterm's path cannot be named in the tap's command.
fn own_tap(run: &Run) -> Option<String> {
    aterm_agent::harness::tap::settings_value(Path::new(&run.aterm))
}

/// The parts of `l`'s launch in `run`.
#[must_use]
pub(crate) fn launch_words(run: &Run, l: &Launch) -> LaunchWords {
    let q = shell_quote;
    let w = run.w.to_string_lossy();
    let mut extra: Vec<String> = crate::clock::vars(run)
        .into_iter()
        .map(|(k, v)| format!("{k}={v}"))
        .collect();
    extra.extend(l.envs.iter().cloned());
    let mut relay = Vec::new();
    if l.relay {
        relay = vec![
            q(&run.run.join("relay/node").to_string_lossy()),
            "--log".to_string(),
            q(&format!("{w}/relay.log")),
            "--".to_string(),
        ];
        extra.push("CLAUDE_CODE_NO_FLICKER=1".into());
    }
    if l.subscriber {
        extra.push(format!("CLAUDE_CODE_OAUTH_TOKEN={}", oauth_token()));
    }
    let px = q(&run.proxy);
    let pair = |k: &str, v: String| (k.to_string(), v);
    // `--own` names no `CLAUDE_CONFIG_DIR`: its Claude Code reads the scratch HOME's own
    // `.claude` and `.claude.json`, the default home the harness credits to its account.
    let config = (!l.own).then(|| {
        pair(
            "CLAUDE_CONFIG_DIR",
            q(&if l.stored {
                stored_login_dir(run).to_string_lossy().into_owned()
            } else {
                format!("{w}/home/.claude")
            }),
        )
    });
    let env = vec![
        std::iter::once(pair(
            "PATH",
            shell_prepend_path(&run.run.join("shim").to_string_lossy()),
        ))
        .chain(config)
        .chain([pair(
            "ANTHROPIC_BASE_URL",
            q(&format!("http://127.0.0.1:{}", run.port)),
        )])
        .collect(),
        [
            "HTTP_PROXY",
            "HTTPS_PROXY",
            "http_proxy",
            "https_proxy",
            "ALL_PROXY",
        ]
        .map(|k| pair(k, px.clone()))
        .to_vec(),
        vec![
            pair("NO_PROXY", "127.0.0.1,localhost".into()),
            pair("no_proxy", "127.0.0.1,localhost".into()),
            pair("DISABLE_AUTOUPDATER", "1".into()),
        ],
        vec![
            pair("LAB_SECURITY_LOG", q(&format!("{w}/security.log"))),
            pair("LAB_OPEN_LOG", q(&format!("{w}/open.log"))),
            pair("BROWSER", q(&run.run.join("shim/open").to_string_lossy())),
        ],
    ];
    let mut env = env;
    if l.stub_route_alone {
        env.retain(|line| !line.iter().any(|(k, _)| PROXY_KEYS.contains(&k.as_str())));
    }
    let mut args = Vec::new();
    if l.own {
        // [`launch_claude`] refuses an `--own` launch whose aterm the tap cannot name first.
        if let Some(tap) = own_tap(run) {
            args.push("--settings".to_string());
            args.push(q(&tap));
        }
    }
    if l.debug_log {
        args.push("--debug-file".to_string());
        args.push(q(&crate::stats::debug_log(run).to_string_lossy()));
    }
    args.extend(l.args.iter().map(|a| q(a)));
    LaunchWords {
        relay,
        env,
        extra,
        program: if l.by_name {
            "claude".to_string()
        } else {
            q(&run.claude_bin)
        },
        args,
    }
}

/// The launcher the tab runs.
#[must_use]
pub fn launcher(run: &Run, l: &Launch) -> String {
    let words = launch_words(run, l);
    let mut s = String::from("#!/bin/sh\nclear\nexec ");
    for r in &words.relay {
        s.push_str(r);
        s.push(' ');
    }
    s.push_str("env");
    if l.stub_route_alone {
        for k in PROXY_KEYS {
            s.push_str(&format!(" -u {k}"));
        }
    }
    for (i, line) in words.env.iter().enumerate() {
        s.push_str(if i == 0 { " " } else { "  " });
        let pairs: Vec<String> = line.iter().map(|(k, v)| format!("{k}={v}")).collect();
        s.push_str(&pairs.join(" "));
        s.push_str(" \\\n");
    }
    for kv in &words.extra {
        s.push_str(&format!("  {} \\\n", shell_quote(kv)));
    }
    s.push_str("  ");
    s.push_str(&words.program);
    for a in &words.args {
        s.push(' ');
        s.push_str(a);
    }
    s.push('\n');
    s
}

/// A canonical CLI-only jail plan. The preparation host continues owning its own socket,
/// instance and fake provider; this plan can be passed after `lab instance --`.
pub(crate) struct PreparedChild {
    kind: &'static str,
    pub(crate) launcher: PathBuf,
    pub(crate) cwd: PathBuf,
    pub(crate) argv: Vec<String>,
}

impl PreparedChild {
    /// Construct the canonical jailed CLI argv without launching or saving its host.
    pub(crate) fn new(
        run: &Run,
        kind: &'static str,
        launcher: PathBuf,
        vars: &[(String, String)],
    ) -> Result<Self, Fail> {
        let pairs: Vec<_> = vars
            .iter()
            .map(|(name, value)| (name.as_str(), value.as_str()))
            .collect();
        let script_word = launcher
            .to_str()
            .ok_or_else(|| Fail::usage("lab launcher path is not UTF-8"))?;
        let argv = procs::jailed_argv(
            run,
            Some(Path::new(&run.proj)),
            &pairs,
            &["/bin/sh", script_word],
        );
        Ok(Self {
            kind,
            launcher,
            cwd: PathBuf::from(&run.proj),
            argv,
        })
    }

    pub(crate) fn json(&self, tag: &str) -> J {
        J::obj([
            ("schema", J::Int(1)),
            ("kind", J::s(self.kind)),
            ("tag", J::s(tag)),
            ("launcher", J::s(self.launcher.to_string_lossy())),
            ("cwd", J::s(self.cwd.to_string_lossy())),
            (
                "argv",
                J::Arr(self.argv.iter().cloned().map(J::s).collect()),
            ),
            ("requires_preparation_host", J::Bool(true)),
        ])
    }
}

/// Prepare without an input connection or a saved-run mutation. Validation, including the
/// existing witness identity check, finishes before any launcher or directory is written.
fn prepare_claude_with(
    run: &Run,
    l: &Launch,
    user: &str,
    witness: impl FnOnce(&Run) -> Result<(), String>,
) -> Result<PreparedChild, Fail> {
    if l.relay || l.subscriber || l.stored || l.own || l.by_name {
        return Err(Fail::usage(
            "claude --prepare-only takes no --relay, --subscriber, --stored, --own or --by-name",
        ));
    }
    let vars = prepared_child_vars(
        run,
        user,
        "claude",
        &l.envs,
        &launch_words(run, &Launch::default()).names(),
        &l.args,
    )?;
    witness(run).map_err(Fail::die)?;
    let plan = PreparedChild::new(
        run,
        "claude-prepared",
        run.run.join("claude-prepared.sh"),
        &vars,
    )?;
    atomic_write(&plan.launcher, launcher(run, l).as_bytes())
        .map_err(|error| Fail::die(format!("cannot write {}: {error}", plan.launcher.display())))?;
    Ok(plan)
}

/// The CLI child's private environment, validated before any preparation files are written.
pub(crate) fn prepared_child_vars(
    run: &Run,
    user: &str,
    program: &str,
    envs: &[String],
    fixed: &[String],
    args: &[String],
) -> Result<Vec<(String, String)>, Fail> {
    let mut vars = instance_vars(run, user).map_err(Fail::usage)?;
    let tmp = vars
        .iter()
        .find(|(name, _)| name == "TMPDIR")
        .map_or_else(|| run.w.join("tmp"), |(_, value)| PathBuf::from(value));
    if !tmp.starts_with(&run.w)
        || tmp
            .components()
            .any(|part| part == std::path::Component::ParentDir)
    {
        return Err(Fail::usage(format!(
            "{program} --prepare-only requires TMPDIR within the lab scratch"
        )));
    }
    let roots = [
        ("TMPDIR", tmp.clone()),
        ("TMP", tmp.clone()),
        ("TEMP", tmp),
        ("XDG_CACHE_HOME", run.w.join("home/.cache")),
        ("XDG_DATA_HOME", run.w.join("home/.local/share")),
        ("XDG_STATE_HOME", run.w.join("home/.local/state")),
    ];
    for (name, path) in &roots {
        vars.retain(|(key, _)| key != name);
        vars.push((name.to_string(), path.to_string_lossy().into_owned()));
    }
    // These are the interactive protocol values aterm puts on its PTY child.
    // The private child clears inherited environment, so restore them explicitly.
    vars.extend([
        ("TERM".into(), "xterm-256color".into()),
        ("COLORTERM".into(), "truecolor".into()),
        ("TERM_PROGRAM".into(), "aterm".into()),
    ]);
    for kv in envs {
        let Some((name, value)) = kv.split_once('=') else {
            return Err(Fail::usage(format!(
                "{program} --prepare-only -e needs NAME=value"
            )));
        };
        if !shell_name(name) || value.contains('\0') {
            return Err(Fail::usage(format!(
                "{program} --prepare-only -e needs a valid name and no NUL"
            )));
        }
        if vars.iter().any(|(key, _)| key == name)
            || fixed.iter().any(|key| key == name)
            || crate::clock::is_clock_var(name)
        {
            return Err(Fail::usage(format!(
                "{program} --prepare-only fixes {name} to its private launch value"
            )));
        }
    }
    if args.iter().any(|arg| arg.contains('\0')) {
        return Err(Fail::usage(format!(
            "{program} --prepare-only arguments cannot contain NUL"
        )));
    }
    Ok(vars)
}

/// The line `claude --by-name` types at the tab's prompt, as a person types it: the launch's
/// environment as assignment words (`K='v 1'`, the name never quoted: aterm's command reader and
/// the shell skip `NAME=value` words before the program word), then `exec` when the launch
/// replaces the tab's shell ([`Launch::replace_shell`]: after the assignments, where zsh and bash
/// still assign them, and a wrapper aterm's reader looks through), then the relay's words, then
/// `claude` bare, which the shell looks up in the assigned `PATH`, then its arguments.
#[must_use]
pub(crate) fn typed_line(run: &Run, l: &Launch) -> String {
    let words = launch_words(run, l);
    let mut line: Vec<String> = words
        .env
        .iter()
        .flatten()
        .map(|(k, v)| format!("{k}={v}"))
        .collect();
    line.extend(words.extra.iter().map(|kv| match kv.split_once('=') {
        Some((k, v)) => format!("{k}={}", shell_quote(v)),
        None => shell_quote(kv),
    }));
    if l.replace_shell {
        line.push("exec".to_string());
    }
    line.extend(words.relay);
    line.push(words.program);
    line.extend(words.args);
    line.join(" ")
}

/// Whether `name` is a shell variable name, `[A-Za-z_][A-Za-z0-9_]*`.
fn shell_name(name: &str) -> bool {
    let mut chars = name.chars();
    chars
        .next()
        .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// Why `l` cannot launch by name in `run`, before anything is typed: `claude` is on a tab's PATH
/// only in a `LAB_REROUTE=1` lab, and each `-e` must be an assignment word.
#[must_use]
pub(crate) fn by_name_refused(run: &Run, l: &Launch) -> Option<String> {
    if !l.by_name {
        return None;
    }
    if run.reroute != "1" {
        return Some(format!(
            "claude --by-name types `claude` at the prompt as a person does, which finds it only in a lab brought up with LAB_REROUTE=1 (lab '{0}' was not): {LAB} down {0}, then LAB_REROUTE=1 {LAB} up {0}",
            run.tag
        ));
    }
    l.envs
        .iter()
        .find(|kv| !kv.split_once('=').is_some_and(|(k, _)| shell_name(k)))
        .map(|kv| {
            format!(
                "claude --by-name: -e {kv} is no NAME=value assignment word (NAME is [A-Za-z_][A-Za-z0-9_]*)"
            )
        })
}

/// What [`launch_claude`] types: `sh <launcher>` (`exec sh <launcher>` when it replaces the
/// tab's shell, [`Launch::replace_shell`]), or with `--by-name` the line a person types
/// ([`typed_line`]).
#[must_use]
pub(crate) fn launch_line(run: &Run, l: &Launch, script: &Path) -> String {
    if l.by_name {
        typed_line(run, l)
    } else {
        format!(
            "{}sh {}",
            if l.replace_shell { "exec " } else { "" },
            shell_quote(&script.to_string_lossy())
        )
    }
}

/// Whether the relay copied into the lab's run directory ran ([`RELAY_RAN`]).
fn relay_shown_running(run: &Run) -> bool {
    run.run.join(RELAY_RAN).exists()
        || crate::sys::scratch_read(&run.w.join("relay.log")).is_ok_and(|b| b.is_some())
}

/// Lay the relay for a launch behind it ([`RELAY_RAN`]).
/// # Errors
/// The copy could not be made.
fn lay_relay(run: &Run) -> Result<(), Fail> {
    let dir = run.run.join("relay");
    if relay_shown_running(run) && dir.join("node").is_file() {
        let _ = crate::util::atomic_write(&run.run.join(RELAY_RAN), b"");
    }
    let node = dir.join("node");
    if !node.is_file() {
        let new = dir.join("node.new");
        fs::create_dir_all(&dir)
            .and_then(|()| crate::util::copy_regular_file(run.run.join(BIN), &new).map(|_| ()))
            .and_then(|()| fs::rename(&new, &node))
            .map_err(|e| {
                Fail::die(format!(
                    "claude --relay: could not lay the relay in {}: {e}",
                    dir.display()
                ))
            })?;
    }
    let _ = fs::remove_file(run.w.join("relay.log"));
    Ok(())
}

/// The login directory of `claude --stored` (its `CLAUDE_CONFIG_DIR`). It is not the scratch
/// home's own `.claude`: aterm reads a `CLAUDE_CONFIG_DIR` naming that directory as a custom
/// home overlapping the default login, and plans no relaunch of it.
fn stored_login_dir(run: &Run) -> PathBuf {
    run.w.join(crate::run::STORED_LOGIN)
}

/// Lay `claude --stored`'s login in its own directory ([`stored_login_dir`], which `run` names
/// as its [`Run::claude_dir`]): the scratch home's settings and profile as `up` laid them where
/// it holds none yet (a later launch keeps what `lab settings` and Claude Code wrote there, as a
/// launch in the home does), owner-only, then the stored login
/// [`crate::forever::sysmsg::login::lay_stored`] lays there. The scratch home's own `.claude` is
/// left as it was, so a later launch without `--stored` is the route `up` laid.
fn lay_stored_login(run: &Run) -> Result<(), Fail> {
    let (from, to) = (run.w.join("home/.claude"), stored_login_dir(run));
    debug_assert_eq!(
        run.claude_dir(),
        to,
        "launch_claude names the stored login first"
    );
    crate::util::create_private_dir_all(&to)
        .map_err(|e| Fail::die(format!("the stored login: {}: {e}", to.display())))?;
    for name in ["settings.json", ".claude.json"] {
        let at = to.join(name);
        let held = crate::sys::scratch_read(&at)
            .map_err(|e| Fail::die(format!("the stored login: {e}")))?;
        if held.is_some() {
            continue;
        }
        let text = crate::verbs::read_scratch(&from.join(name))?;
        crate::sys::put(&at, 0o600, &crate::sys::PutSrc::Bytes(text.as_bytes()))
            .map_err(|e| Fail::die(format!("the stored login: {name}: {e}")))?;
    }
    crate::forever::sysmsg::login::lay_stored(run)
}

/// The roster `claude --own` lays where the lab's config holds none ([`lay_own_login`]): one row,
/// `main`, the default login's (no `dir`), named by the composed profile's email, as an ALab Mac
/// lists its own account. The quota row is the outlook's, which forecasts a roster's accounts: a
/// Mac with no roster has no outlook (`docs/DESIGN-harness-glass-rainbow-2026-11-05.md`), so
/// line E stays Claude's there and line D names `this login`.
fn own_roster() -> String {
    format!(
        "[[account]]\nlabel = \"main\"\nclass = \"dedicated\"\nemail = \"{}\"\n",
        crate::stub::account::EMAIL
    )
}

/// Lay `claude --own`'s login in the scratch HOME itself, as a person's `/login` leaves one in
/// the default home: the HOME's `.claude.json` (Claude Code's profile where no
/// `CLAUDE_CONFIG_DIR` is set, [`Run::claude_json`]) as `up` laid the scratch `.claude`'s where it
/// holds none yet (a later launch keeps what Claude Code wrote there), owner-only; the roster
/// [`own_roster`] beside the instance's `aterm.toml` where none is there (one a lab verb or a
/// person wrote stays); then the stored login [`crate::forever::sysmsg::login::lay_stored`] lays
/// in the HOME's `.claude` and that profile. Every later launch in the scratch home runs on it, as
/// one after a person's `/login` does.
fn lay_own_login(run: &Run) -> Result<(), Fail> {
    let (from, to) = (run.w.join("home/.claude/.claude.json"), run.claude_json());
    debug_assert_eq!(
        to,
        run.w.join("home/.claude.json"),
        "launch_claude names the own login first"
    );
    let held =
        crate::sys::scratch_read(&to).map_err(|e| Fail::die(format!("the own login: {e}")))?;
    if held.is_none() {
        let text = crate::verbs::read_scratch(&from)?;
        crate::sys::put(&to, 0o600, &crate::sys::PutSrc::Bytes(text.as_bytes()))
            .map_err(|e| Fail::die(format!("the own login: .claude.json: {e}")))?;
    }
    let roster = run
        .w
        .join("cfg/aterm")
        .join(aterm_agent::harness::accounts::ACCOUNTS_FILE);
    let listed =
        crate::sys::scratch_read(&roster).map_err(|e| Fail::die(format!("the own login: {e}")))?;
    if listed.is_none() {
        atomic_write(&roster, own_roster().as_bytes())
            .map_err(|e| Fail::die(format!("the own login: {}: {e}", roster.display())))?;
    }
    crate::forever::sysmsg::login::lay_stored(run)
}

/// Why `run` cannot take an `--own` launch ([`Launch::own`]), `None` where it can: a stored
/// login's process is the default route's only where the instance admits the stub's base URL
/// ([`UpOpts::route_seam`]), and the usage tap's command must name the lab's aterm.
pub(crate) fn own_refused(run: &Run) -> Option<String> {
    if run.route_seam != "1" {
        return Some(
            "a stored login's process is the default route's only where the instance admits \
             the stub's base URL: up the lab with LAB_ROUTE_SEAM=1"
                .to_string(),
        );
    }
    own_tap(run).is_none().then(|| {
        format!(
            "the usage tap's command cannot name the lab's aterm ({})",
            run.aterm
        )
    })
}

/// `run` made ready for `--own` launches that are no `lab claude`'s (`autopilot-captures`'
/// Claude Code tabs): refused as [`own_refused`] says, else the run saved as one whose Claude
/// Code reads the default home ([`crate::run::OWN_LOGIN`], so [`Run::claude_json`] names the
/// HOME's own profile) and the login and roster laid ([`lay_own_login`]); the launch to write.
/// # Errors
/// [`own_refused`]'s reason (a usage error), or the login could not be laid.
pub(crate) fn ready_own(run: &mut Run) -> Result<Launch, Fail> {
    if let Some(why) = own_refused(run) {
        return Err(Fail::usage(format!("an own login: {why}")));
    }
    run.claude_login = crate::run::OWN_LOGIN.into();
    run.save();
    lay_own_login(run)?;
    Ok(Launch {
        own: true,
        stub_route_alone: true,
        ..Launch::default()
    })
}

/// Type the launch line of the managed Claude Code into the lab's first tab.
/// # Errors
/// The launch line could not be typed, or the relay could not be laid.
pub fn launch_claude(run: &Run, l: &Launch) -> Result<String, Fail> {
    if l.prepare_only {
        return Err(Fail::usage(
            "--prepare-only emits a child launch plan; it cannot type a launch",
        ));
    }
    if let Some(why) = by_name_refused(run, l) {
        return Err(Fail::usage(why));
    }
    if l.stored && run.route_seam != "1" {
        return Err(Fail::usage(
            "claude --stored: a stored login's process is the default route's only where the \
             instance admits the stub's base URL: up the lab with LAB_ROUTE_SEAM=1",
        ));
    }
    if let Some(why) = l.own.then(|| own_refused(run)).flatten() {
        return Err(Fail::usage(format!("claude --own: {why}")));
    }
    procs::need_witness(run)?;
    let mut run = run.clone();
    run.agent = "claude".into();
    run.claude_login = if l.stored {
        crate::run::STORED_LOGIN
    } else if l.own {
        crate::run::OWN_LOGIN
    } else {
        ""
    }
    .into();
    run.save();
    let run = &run;
    if l.relay {
        lay_relay(run)?;
    }
    if l.subscriber {
        crate::verbs::settings_merge(run, &J::obj([("apiKeyHelper", J::Null)]))?;
    }
    if l.stored {
        lay_stored_login(run)?;
    }
    if l.own {
        lay_own_login(run)?;
    }
    let script = run.run.join("claude.sh");
    let text = launcher(run, l);
    crate::util::atomic_write_mode(&script, text.as_bytes(), 0o755)
        .map_err(|e| Fail::die(format!("cannot write {}: {e}", script.display())))?;
    let door = Door::new(run)?;
    let sid = crate::verbs::active_sid(&door);
    if !sid.is_empty() {
        crate::verbs::record_launch(run, &sid, &text)?;
    }
    let line = launch_line(run, l, &script);
    let sent = door.ctl(&["send", &line])?;
    if sent.code != 0 {
        return Err(Fail::die("could not type the launch line"));
    }
    let entered = door.ctl(&["key", "enter"])?;
    if entered.code != 0 {
        return Err(Fail::die(format!(
            "could not submit the launch line (exit {}): {}{}",
            entered.code, entered.stdout, entered.stderr
        )));
    }
    Ok(format!(
        "launched: {} ({}) in lab '{}'{}  (launcher: {})",
        if l.by_name {
            format!(
                "`claude` typed at the prompt as a person does (PATH's first: the reroute stub, its twin, {})",
                run.claude_bin
            )
        } else {
            run.claude_bin.clone()
        },
        run.claude_ver,
        run.tag,
        if l.relay {
            ", behind the PTY relay"
        } else {
            ""
        },
        script.display()
    ))
}

/// `claude [tag] [-e K=V]... [--no-wait] [--prepare-only] [--relay] [--subscriber | --stored |
/// --own] [--debug-log] [--by-name] [-- claude args]`.
pub fn cmd_claude(args: &[String]) -> Res {
    let mut args = args.to_vec();
    let tag = take_tag(&mut args)?;
    let run = Run::load(&tag)?;
    let l = parse_launch(&args)?;
    if l.prepare_only {
        let plan = prepare_claude_with(&run, &l, &paths().user, procs::need_witness)?;
        println!("{}", plan.json(&tag).dump(crate::pyjson::COMPACT));
        return Ok(0);
    }
    println!("{}", launch_claude(&run, &l)?);
    if !l.no_wait
        && Door::new(&run)?
            .await_match("❯|Welcome|trust|Claude Code", 60)
            .unwrap_or(false)
    {
        println!("the TUI is drawing ({LAB} text {tag})");
    }
    Ok(0)
}

#[cfg(test)]
#[path = "claude_prepared_tests.rs"]
mod prepared_tests;

#[cfg(test)]
#[path = "lab_tests.rs"]
mod tests;
