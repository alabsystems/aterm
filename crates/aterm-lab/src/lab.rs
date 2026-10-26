// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Lifecycle management (`up`, `down`, `ps`) for isolated lab instances.

use std::fs;
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
use crate::util::{atomic_write, keep_file, keep_tree, now_secs, shell_quote};
use crate::{Fail, Res, stop};

/// The fake API key `apiKeyHelper` prints (the stub accepts anything).
fn api_key() -> String {
    format!("sk-ant-api03-LABLABLAB{}AA", "A".repeat(85))
}

/// The fake claude.ai OAuth token of `claude TAG --subscriber`: no credential anywhere.
pub fn oauth_token() -> String {
    format!("sk-ant-oat01-LABLABLAB{}AA", "A".repeat(80))
}

/// How `up` makes a lab (each field has its `LAB_*` variable).
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
    /// `LAB_HOME_IMAGE=<size>` (`16g`): the scratch's `home/` is a sparse APFS image of that size,
    /// a volume of its own, which the `down` that removes the scratch detaches
    /// ([`home_image_refused`], [`detach_home_image`]).
    pub home_image: Option<String>,
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
            home_image: Some(var("LAB_HOME_IMAGE")).filter(|v| !v.is_empty()),
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

/// The managed Claude Code, launched by path (not through its shim) ([`pkg_which`]), or
/// `$LAB_CLAUDE`.
/// # Errors
/// Neither names an executable.
pub fn find_claude() -> Result<String, String> {
    let c = match std::env::var("LAB_CLAUDE") {
        Ok(c) if !c.is_empty() => c,
        _ => store_claude(paths(), paths().need_aterm()?)?,
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

/// The store binary of the managed `name` ([`pkg_which`]).
pub(crate) fn managed_store_bin(name: &str) -> Option<String> {
    managed_store_bin_in(paths(), paths().need_aterm().ok()?, name)
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
/// ([`sweep_copies`]), the run directory's own files and its tabs' launch records
/// ([`crate::verbs::record_launch`], which `down` keeps in its logs), then the directory.
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

/// How long a jailed child the lab started may go without progress before it says it is ready
/// (its `--ready-fd` byte, or a window's answer): a hang detector, never a latency budget.
const READY_HANG: Duration = Duration::from_secs(60);

/// How often a wait for a starting child looks at its progress ([`while_starting`]).
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
/// The launch failed, the child ended before its socket answered, it made no progress for `hang`
/// before it answered, or the run was interrupted (`interrupted`).
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
        || progress_of(child, out),
        hang,
        Instant::now(),
        || {
            stop::sleep(Duration::from_millis(250)).map_err(|_| "interrupted".to_string())?;
            Ok(Instant::now())
        },
    )
}

/// A starting child's progress: its CPU time used plus the length of its output `out`.
fn progress_of(pid: i32, out: &Path) -> u64 {
    crate::sys::cpu_used(pid)
        .unwrap_or(0)
        .saturating_add(fs::metadata(out).map_or(0, |m| m.len()))
}

/// A starting child's readiness, waited for as a hang detector on its `progress`.
/// # Errors
/// The child ended before it was ready, it made no progress for `hang`, or `pause` failed.
fn while_starting(
    mut look: impl FnMut() -> Result<bool, String>,
    mut progress: impl FnMut() -> u64,
    hang: Duration,
    start: Instant,
    mut pause: impl FnMut() -> Result<Instant, String>,
) -> Result<(), String> {
    let mut still: Option<(u64, Instant)> = None;
    let mut now = start;
    loop {
        if look()? {
            return Ok(());
        }
        let moved = progress();
        let since = match still {
            Some((seen, since)) if seen == moved => since,
            _ => {
                still = Some((moved, now));
                now
            }
        };
        if now.saturating_duration_since(since) >= hang {
            return Err(format!(
                "said nothing and made no progress (no CPU time used, no output written) for {} s: \
                 the hang detector",
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
    (o.reroute && o.clock).then_some(
        "LAB_REROUTE=1 takes no LAB_CLOCK=1: the sleep clock runs its own re-signed clone of Claude Code, not the store build the reroute prefix routes to",
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
        || progress_of(child, out),
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
    let f = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)?;
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
    vars.extend(crate::instance::seams());
    vars.extend(crate::clock::vars(run));
    vars
}

/// Whether the lab sets `name` for its instance itself ([`lab_vars`]).
fn lab_sets(name: &str) -> bool {
    let probe = Run {
        seam: "1".into(),
        clock: "1".into(),
        ..Run::default()
    };
    lab_vars(&probe, "").iter().any(|(k, _)| k == name)
}

/// `LAB_INSTANCE_ENV` read: `NAME=VALUE` words ([`lab_vars`]).
/// # Errors
/// A word that is no `NAME=VALUE`, a name outside those kinds (`TMPDIR`, an `ATERM_DEBUG_*` or
/// `ATERM_DEV_*` seam, or the occupancy nudge's seam,
/// [`NUDGE_SEAM`](aterm_agent::supervise::occupancy::rules::NUDGE_SEAM)) or one the lab sets, a
/// name given twice, an empty value or one with a control character, or a `TMPDIR` that is not
/// absolute.
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
        if !(name == "TMPDIR" || seam || named) || lab_sets(name) {
            return refuse(format!(
                "{name} is not a variable the lab hands its instance (TMPDIR, an ATERM_DEBUG_* or ATERM_DEV_* seam the lab does not set itself, or ATERM_OCCUPANCY_NUDGE_S)"
            ));
        }
        if value.is_empty() || value.chars().any(char::is_control) {
            return refuse(format!("{name} needs a value with no control character"));
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
/// [`instance_argv`]).
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
    Ok(())
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
/// stamped ([`stamp_holds`]) and swept ([`sweep_copies`]).
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
        None => Ok(()),
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
    run.claude_bin = find_claude()?;
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
    if let Some(c) = find_codex() {
        run.codex_bin = outside("Codex", c)?;
    }
    if let Some(c) = find_agy() {
        run.agy_bin = outside("agy", c)?;
    }
    if let Some(c) = find_muse() {
        run.muse_bin = outside("Muse Code", c)?;
    }
    run.agent = "claude".into();
    run.cols.clone_from(&o.cols);
    run.rows.clone_from(&o.lines);
    run.seam = if o.restore { "1" } else { "0" }.into();
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
        fs::set_permissions(&home, fs::Permissions::from_mode(0o700))
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
    let _ = fs::write(
        w.join("home/.codex/LAB-DECOY"),
        format!(
            "lab decoy: Codex runs with CODEX_HOME={}; nothing may be written here\n",
            w.join("codex-home").display()
        ),
    );
    let _ = fs::write(
        w.join(crate::agy_proof::DECOY),
        "lab decoy: agy runs with HOME=agy-home; nothing may be written here\n",
    );
    run.sockdir = crate::sys::make_temp_dir("/tmp/at.XXXX")
        .map_err(|e| abort(&run, &[], &format!("cannot make a socket dir: {e}")))?;
    run.sock = format!("{}/a.sock", run.sockdir);
    run.proj = format!("{}/proj", run.sockdir);
    let _ = fs::create_dir_all(&run.proj);
    let bin = run.run.join(BIN);
    let made = fs::create_dir_all(run.run.join("bin"))
        .and_then(|()| fs::copy(&p.exe, &bin).map(|_| ()))
        .and_then(|()| fs::create_dir_all(run.run.join("shim")))
        .and_then(|()| std::os::unix::fs::symlink(&bin, run.run.join("shim/security")))
        .and_then(|()| std::os::unix::fs::symlink(&bin, run.run.join("shim/open")))
        .and_then(|()| std::os::unix::fs::symlink(&aterm, run.run.join("shim/aterm")));
    if let Err(e) = made {
        return Err(abort(
            &run,
            &[],
            &format!("cannot lay the lab's program in {}: {e}", run.run.display()),
        ));
    }
    run.mark = crate::jail::new_mark(tag);
    let mut spec = crate::jail::Spec::for_lab(
        &run.run.to_string_lossy(),
        &run.claude_bin,
        &run.codex_bin,
        &run.sockdir,
        &run.mark,
    );
    spec.agy.clone_from(&run.agy_bin);
    spec.muse.clone_from(&run.muse_bin);
    let jail = crate::jail::render(&spec).map_err(|e| abort(&run, &[], &e))?;
    fs::write(run.run.join("jail.sb"), jail).map_err(|e| abort(&run, &[], &e.to_string()))?;
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
    let _ = fs::write(w.join("mode"), "ok\n");
    let _ = fs::write(w.join("reply.txt"), "");
    let _ = fs::write(w.join("security.log"), "");

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
        let _ = fs::write(run.run.join(BIN_RAN), "");
    }
    let started = [run.sampler_pid, run.stub_pid];
    if let Err(e) = launched {
        if e == "interrupted" {
            return Err(abort(&run, &started, "interrupted"));
        }
        eprintln!("aterm-lab: the stub: {e}");
    }
    let filled = |f: &str| fs::metadata(w.join(f)).is_ok_and(|m| m.len() > 0);
    if !(filled("stub.port") && filled("proxy.port")) {
        eprint!(
            "{}",
            fs::read_to_string(run.run.join("stub.out")).unwrap_or_default()
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
    let _ = fs::write(&helper, script)
        .and_then(|()| fs::set_permissions(&helper, fs::Permissions::from_mode(0o755)));
    let witness = run.run.join(crate::settings_witness::SCRIPT);
    let _ = fs::write(
        &witness,
        crate::settings_witness::script(&w.join(crate::settings_witness::LOG)),
    )
    .and_then(|()| fs::set_permissions(&witness, fs::Permissions::from_mode(0o755)));
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
    put(
        "cfg/aterm/aterm.toml",
        &scratch_aterm_toml(&o.aterm_toml, &o.harness_keys),
    );
    put_tab_shell(&w, &run.port);

    if let Err(e) = start_instance(&mut run, o.scale.as_deref(), o.lifeline) {
        eprint!(
            "{}",
            fs::read_to_string(run.run.join("aterm.out")).unwrap_or_default()
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
        let _ = fs::write(run.run.join(CLOCK_RAN), "");
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
    run.save();
    Ok(run)
}

/// A lab's scratch `aterm.toml`: `LAB_ATERM_TOML`, the update and package lanes off,
/// `LAB_HARNESS_KEYS` under `[harness]`, and the audible bell off unless `LAB_ATERM_TOML` sets
/// `bell_sound` ([`crate::instance::bell_off`]).
pub(crate) fn scratch_aterm_toml(aterm_toml: &str, harness_keys: &str) -> String {
    crate::instance::bell_off(&format!(
        "{aterm_toml}\n[update]\nenabled = false\n[packages]\nenabled = false\n[harness]\nprobe_api = false\n{harness_keys}\n"
    ))
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

/// `up [tag] [--window]`: the existing jailed instance, headless by default.
pub fn cmd_up(args: &[String]) -> Res {
    let mut tag = None;
    let mut o = UpOpts::from_env();
    for arg in args {
        if arg == "--window" {
            o.window = true;
        } else if arg.starts_with('-') {
            return Err(Fail::usage(format!("up: unknown option '{arg}'")));
        } else if tag.is_none() {
            tag = Some(arg.clone());
        } else {
            return Err(Fail::usage("up: takes one optional tag and --window"));
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
        "  instance    pid {}   socket {}   {}x{}   {aterm}",
        run.instance_pid, run.sock, run.cols, run.rows
    );
    println!("  claude      {} ({})", run.claude_bin, run.claude_ver);
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
    ] {
        if let Ok(Some(b)) = crate::sys::scratch_read(&r.w.join(f)) {
            let _ = fs::write(keep.join(f), b);
        }
    }
    for (f, to) in [
        ("claude-debug.log", "claude-debug.log"),
        ("statusline.jsonl", "statusline.jsonl"),
        ("home/.claude/.claude.json", "claude.json"),
    ] {
        keep_file(&r.w.join(f), &keep.join(to));
    }
    let projects = r.w.join("home/.claude/projects");
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
    let sec = r.w.join("security.log");
    if let Ok(Some(b)) = crate::sys::scratch_read(&sec) {
        let masked: String = String::from_utf8_lossy(&b)
            .lines()
            .map(|l| format!("{}\n", mask_account(l)))
            .collect();
        let _ = fs::write(keep.join("security.log"), masked);
    }
    keep_tree(&r.state().join("drive"), &keep.join("drive"));
    keep_tree(&r.run.join(LAUNCHES), &keep.join(LAUNCHES));
    crate::clock::keep(&r, &keep.join("clock"));
    keep_tree(&r.state().join("logs"), &keep.join("state-logs"));
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
    if live.is_empty() {
        println!("  nothing of it is alive: {LAB} down {tag} removes its run directory");
    } else if let Ok(out) = aterm_uds::spawnfd::command("/bin/ps")
        .args(["-o", "pid,ppid,etime,comm", "-p", &live.join(",")])
        .output()
    {
        for l in String::from_utf8_lossy(&out.stdout).lines() {
            println!("  {l}");
        }
    }
    Ok(())
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

/// How `claude` launches.
#[derive(Debug, Default)]
pub struct Launch {
    /// `-e K=V`: extra environment.
    pub envs: Vec<String>,
    /// `--no-wait`.
    pub no_wait: bool,
    /// `--relay`: behind the PTY relay, on the fullscreen renderer.
    pub relay: bool,
    /// `--subscriber`: a fake claude.ai login.
    pub subscriber: bool,
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
            "--relay" => l.relay = true,
            "--subscriber" => l.subscriber = true,
            "--debug-log" => l.debug_log = true,
            "--by-name" => l.by_name = true,
            "--" => {
                l.args.extend(it.by_ref().cloned());
            }
            other => {
                return Err(format!(
                    "claude: unknown option '{other}' (use -e K=V, --no-wait, --relay, --subscriber, --debug-log, --by-name, -- <claude args>)"
                ));
            }
        }
    }
    Ok(l)
}

/// The launcher the tab runs.
#[must_use]
pub fn launcher(run: &Run, l: &Launch) -> String {
    let q = shell_quote;
    let w = run.w.to_string_lossy();
    let mut envs: Vec<String> = crate::clock::vars(run)
        .into_iter()
        .map(|(k, v)| format!("{k}={v}"))
        .collect();
    envs.extend(l.envs.iter().cloned());
    let mut pre = String::new();
    if l.relay {
        pre = format!(
            "{} --log {} -- ",
            q(&run.run.join("relay/node").to_string_lossy()),
            q(&format!("{w}/relay.log"))
        );
        envs.push("CLAUDE_CODE_NO_FLICKER=1".into());
    }
    if l.subscriber {
        envs.push(format!("CLAUDE_CODE_OAUTH_TOKEN={}", oauth_token()));
    }
    let px = &run.proxy;
    let mut s = String::from("#!/bin/sh\nclear\n");
    s.push_str(&format!(
        "exec {pre}env PATH=\"{}:$PATH\" CLAUDE_CONFIG_DIR={} ANTHROPIC_BASE_URL=http://127.0.0.1:{} \\\n",
        run.run.join("shim").display(),
        q(&format!("{w}/home/.claude")),
        run.port
    ));
    s.push_str(&format!(
        "  HTTP_PROXY={px} HTTPS_PROXY={px} http_proxy={px} https_proxy={px} ALL_PROXY={px} \\\n"
    ));
    s.push_str(
        "  NO_PROXY=127.0.0.1,localhost no_proxy=127.0.0.1,localhost DISABLE_AUTOUPDATER=1 \\\n",
    );
    s.push_str(&format!(
        "  LAB_SECURITY_LOG={} LAB_OPEN_LOG={} BROWSER={} \\\n",
        q(&format!("{w}/security.log")),
        q(&format!("{w}/open.log")),
        q(&run.run.join("shim/open").to_string_lossy())
    ));
    for kv in &envs {
        s.push_str(&format!("  {} \\\n", q(kv)));
    }
    if l.by_name {
        s.push_str("  claude");
    } else {
        s.push_str(&format!("  {}", q(&run.claude_bin)));
    }
    if l.debug_log {
        s.push_str(&format!(
            " --debug-file {}",
            q(&crate::stats::debug_log(run).to_string_lossy())
        ));
    }
    for a in &l.args {
        s.push(' ');
        s.push_str(&q(a));
    }
    s.push('\n');
    s
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
        let _ = fs::write(run.run.join(RELAY_RAN), "");
    }
    let node = dir.join("node");
    if !node.is_file() {
        let new = dir.join("node.new");
        fs::create_dir_all(&dir)
            .and_then(|()| fs::copy(run.run.join(BIN), &new).map(|_| ()))
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

/// Type the launch line of the managed Claude Code into the lab's first tab.
/// # Errors
/// The launch line could not be typed, or the relay could not be laid.
pub fn launch_claude(run: &Run, l: &Launch) -> Result<String, Fail> {
    procs::need_witness(run)?;
    let mut run = run.clone();
    run.agent = "claude".into();
    run.save();
    let run = &run;
    if l.relay {
        lay_relay(run)?;
    }
    if l.subscriber {
        crate::verbs::settings_merge(run, &J::obj([("apiKeyHelper", J::Null)]))?;
    }
    let script = run.run.join("claude.sh");
    let text = launcher(run, l);
    atomic_write(&script, text.as_bytes())
        .map_err(|e| Fail::die(format!("cannot write {}: {e}", script.display())))?;
    let _ = fs::set_permissions(&script, fs::Permissions::from_mode(0o755));
    let door = Door::new(run)?;
    let sid = crate::verbs::active_sid(&door);
    if !sid.is_empty() {
        crate::verbs::record_launch(run, &sid, &text)?;
    }
    let line = format!("sh {}", shell_quote(&script.to_string_lossy()));
    let sent = door.ctl(&["send", &line])?;
    if sent.code != 0 {
        return Err(Fail::die("could not type the launch line"));
    }
    door.ctl(&["key", "enter"])?;
    Ok(format!(
        "launched: {} ({}) in lab '{}'{}  (launcher: {})",
        if l.by_name {
            format!(
                "claude by name (PATH's first: in a LAB_REROUTE=1 lab the reroute stub, its twin, {})",
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

/// `claude [tag] [-e K=V]... [--no-wait] [--relay] [--subscriber] [--debug-log] [--by-name]
/// [-- claude args]`.
pub fn cmd_claude(args: &[String]) -> Res {
    let mut args = args.to_vec();
    let tag = take_tag(&mut args)?;
    let run = Run::load(&tag)?;
    let l = parse_launch(&args)?;
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
mod tests {
    use super::*;

    #[test]
    fn a_line_typed_in_a_tab_shell_reaches_the_stub_with_the_labs_key() {
        let tmp = aterm_tempfile::tempdir().unwrap();
        let dir = tmp.path().canonicalize().unwrap();
        let bin = dir.join("bin");
        std::fs::create_dir_all(&bin).unwrap();
        let build = bin.join("client");
        std::fs::write(
            &build,
            "#!/bin/sh\nprintf 'key=%s url=%s google=%s args=%s\\n' \"$GEMINI_API_KEY\" \"$GOOGLE_GEMINI_BASE_URL\" \"${GOOGLE_API_KEY-unset}\" \"$*\"\n",
        )
        .unwrap();
        std::fs::set_permissions(&build, fs::Permissions::from_mode(0o755)).unwrap();
        let w = dir.join("scratch");
        put_tab_shell(&w, "4242");
        let written = std::fs::read_to_string(w.join("home/.zshrc")).unwrap();
        assert_eq!(written, tab_profile("4242"));
        assert!(!written.contains("PATH"), "{written}");
        let typed = |profile: &str| {
            let rc = dir.join("zshrc");
            std::fs::write(&rc, profile).unwrap();
            let out = aterm_uds::spawnfd::command("/bin/zsh")
                .args(["-f", "-c", "source \"$1\"; client --resume abc"])
                .arg("zsh")
                .arg(&rc)
                .env_clear()
                .env("PATH", format!("{}:/usr/bin:/bin", bin.display()))
                .env("GOOGLE_API_KEY", "an-owner-key")
                .output()
                .unwrap();
            String::from_utf8_lossy(&out.stdout).trim().to_string()
        };
        assert_eq!(
            typed(&written),
            format!(
                "key={} url=http://127.0.0.1:4242 google=unset args=--resume abc",
                crate::agy::LAB_GEMINI_KEY
            )
        );
        assert_eq!(
            typed("PROMPT='lab$ '\n"),
            "key= url= google=an-owner-key args=--resume abc",
            "the profile before"
        );
    }

    #[test]
    fn a_lab_ups_claude_settings_carry_the_settings_witness_hook() {
        let run = Run {
            run: "/l/runs/t".into(),
            w: "/l/runs/t/scratch".into(),
            port: "1".into(),
            proxy: "http://127.0.0.1:2".into(),
            ..Run::default()
        };
        let (settings, _) =
            scratch_claude_config(&run, "/l/runs/t/apikey.sh", "/p", &UpOpts::default());
        let d = J::parse(&settings).unwrap();
        let commands = |hooks: Option<&J>| -> Vec<String> {
            hooks
                .and_then(|h| h.get("ConfigChange"))
                .and_then(J::as_arr)
                .unwrap_or_default()
                .iter()
                .filter_map(|m| m.get("hooks").and_then(J::as_arr))
                .flatten()
                .filter(|c| c.get("type").and_then(J::as_str) == Some("command"))
                .filter_map(|c| c.get("command").and_then(J::as_str).map(str::to_string))
                .collect()
        };
        let named = |run: &str| {
            commands(Some(&crate::settings_witness::hooks(
                &Path::new(run).join(crate::settings_witness::SCRIPT),
            )))
        };
        assert_eq!(commands(d.get("hooks")), named("/l/runs/t"));
        assert_eq!(named("/l/runs/t"), ["/l/runs/t/config-change.sh"]);
        assert_ne!(commands(d.get("hooks")), named("/l/runs/u"));
        assert_eq!(
            d.get("apiKeyHelper").and_then(J::as_str),
            Some("/l/runs/t/apikey.sh")
        );
    }

    #[test]
    fn a_lab_up_config_starts_with_the_audible_bell_off() {
        let read =
            |text: &str| aterm_toml::from_str::<aterm_toml::Value>(text).expect("one document");
        let bell =
            |doc: &aterm_toml::Value| doc.get("bell_sound").and_then(aterm_toml::Value::as_bool);

        let plain = read(&scratch_aterm_toml("font_px = 13", "usage_tap = true"));
        assert_eq!(bell(&plain), Some(false));
        assert_eq!(
            plain.get("font_px").and_then(aterm_toml::Value::as_integer),
            Some(13)
        );
        assert_eq!(
            plain
                .get("harness")
                .and_then(|t| t.get("usage_tap"))
                .and_then(aterm_toml::Value::as_bool),
            Some(true),
        );

        let asked = read(&scratch_aterm_toml("bell_sound = true", ""));
        assert_eq!(
            bell(&asked),
            Some(true),
            "LAB_ATERM_TOML's bell is the file's"
        );
    }

    #[test]
    fn ps_and_down_of_a_lab_that_is_down_name_its_tag() {
        let gone = "ki1016-ps-gone";
        let default = crate::run::default_tag();
        assert!(
            !crate::run::is_run(gone) && !crate::run::is_run(&default),
            "PRECONDITION: no lab '{gone}' or '{default}' is up"
        );
        let refusal = |got: Res| got.err().and_then(|f| f.msg).unwrap_or_default();
        for (verb, got) in [
            ("ps", refusal(cmd_ps(&[gone.to_string()]))),
            (
                "down",
                refusal(cmd_down(&[gone.to_string(), "--keep-dumps".to_string()])),
            ),
        ] {
            assert!(
                got.contains(&format!("no lab '{gone}' is up")),
                "{verb}: {got:?}"
            );
            assert!(
                !got.contains(&format!("'{default}'")),
                "{verb} names the default lab: {got:?}"
            );
        }
    }

    #[test]
    fn the_managed_claude_is_the_last_path_of_the_arrow_chain() {
        let line = "claude → /S/pkg/reroute/claude → /S/pkg/agents/claude → /S/Application Support/aterm/pkg/store/claude/1000002000001000300/bin/claude — managed 2.1.300 — Anthropic latest";
        assert_eq!(
            managed_claude(line).as_deref(),
            Some("/S/Application Support/aterm/pkg/store/claude/1000002000001000300/bin/claude")
        );
        assert_eq!(managed_claude("claude: not managed here"), None);
    }

    #[test]
    fn claude_options_parse_and_the_launcher_execs_the_binary_by_path() {
        let a: Vec<String> = [
            "-e",
            "K=v 1",
            "--relay",
            "--subscriber",
            "--",
            "--model",
            "x y",
        ]
        .map(String::from)
        .to_vec();
        let l = parse_launch(&a).unwrap();
        assert!(l.relay && l.subscriber && !l.no_wait);
        assert!(parse_launch(&["--bogus".to_string()]).is_err());
        let run = Run {
            tag: "t".into(),
            run: "/s/runs/t".into(),
            w: "/s/runs/t/scratch".into(),
            port: "5".into(),
            proxy: "http://127.0.0.1:6".into(),
            claude_bin: "/a b/claude".into(),
            ..Run::default()
        };
        let s = launcher(&run, &l);
        assert!(s.starts_with("#!/bin/sh\nclear\nexec /s/runs/t/relay/node --log /s/runs/t/scratch/relay.log -- env PATH=\"/s/runs/t/shim:$PATH\""), "{s}");
        assert!(s.contains("  LAB_SECURITY_LOG=/s/runs/t/scratch/security.log LAB_OPEN_LOG=/s/runs/t/scratch/open.log BROWSER=/s/runs/t/shim/open \\\n"), "{s}");
        assert!(s.contains("  'K=v 1' \\\n  CLAUDE_CODE_NO_FLICKER=1 \\\n  CLAUDE_CODE_OAUTH_TOKEN=sk-ant-oat01-LABLABLAB"), "{s}");
        assert!(s.ends_with("  '/a b/claude' --model 'x y'\n"), "{s}");
        let words = exec_words(&s);
        assert!(opens_recorded(&words, &run), "{words:?}");
        let at = words.iter().position(|w| w == "/a b/claude").unwrap();
        assert_eq!(words[at + 1..], ["--model", "x y"], "{words:?}");
        let plain = launcher(&run, &Launch::default());
        assert!(opens_recorded(&exec_words(&plain), &run), "{plain}");
        for gone in [
            " LAB_OPEN_LOG=/s/runs/t/scratch/open.log",
            " BROWSER=/s/runs/t/shim/open",
        ] {
            let cut = plain.replace(gone, "");
            assert_ne!(cut, plain);
            assert!(!opens_recorded(&exec_words(&cut), &run), "{cut}");
        }
        let as_arg = Launch {
            args: vec!["LAB_OPEN_LOG=/s/runs/t/scratch/open.log".into()],
            ..Launch::default()
        };
        let arg_only =
            launcher(&run, &as_arg).replacen(" LAB_OPEN_LOG=/s/runs/t/scratch/open.log", "", 1);
        let arg_words = exec_words(&arg_only);
        assert_eq!(
            arg_words.last().map(String::as_str),
            Some("LAB_OPEN_LOG=/s/runs/t/scratch/open.log"),
            "{arg_only}"
        );
        assert!(!opens_recorded(&arg_words, &run), "{arg_only}");
    }

    fn exec_words(script: &str) -> Vec<String> {
        let line = script
            .split_once("exec ")
            .map_or("", |(_, rest)| rest)
            .replace("\\\n", "");
        let line = line.lines().next().unwrap_or("");
        let (mut words, mut cur, mut open) = (Vec::new(), String::new(), false);
        let mut it = line.chars();
        while let Some(c) = it.next() {
            match c {
                '\'' => {
                    open = true;
                    cur.extend(it.by_ref().take_while(|&d| d != '\''));
                }
                '"' => {
                    open = true;
                    while let Some(d) = it.next() {
                        match d {
                            '"' => break,
                            '\\' => cur.extend(it.next()),
                            _ => cur.push(d),
                        }
                    }
                }
                '\\' => cur.extend(it.next()),
                c if c.is_whitespace() => {
                    if open || !cur.is_empty() {
                        words.push(std::mem::take(&mut cur));
                    }
                    open = false;
                }
                c => cur.push(c),
            }
        }
        if open || !cur.is_empty() {
            words.push(cur);
        }
        words
    }

    fn opens_recorded(words: &[String], run: &Run) -> bool {
        let Some(env) = words.iter().position(|w| w == "env") else {
            return false;
        };
        let is_assignment = |w: &str| {
            w.split_once('=').is_some_and(|(k, _)| {
                !k.is_empty()
                    && !k.starts_with(|c: char| c.is_ascii_digit())
                    && k.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
            })
        };
        let vars: Vec<&str> = words[env + 1..]
            .iter()
            .map(String::as_str)
            .take_while(|w| is_assignment(w))
            .collect();
        let has = |want: &str| vars.contains(&want);
        let shim = run.run.join("shim");
        vars.iter()
            .any(|v| v.starts_with(&format!("PATH={}:", shim.display())))
            && has(&format!("BROWSER={}", shim.join("open").display()))
            && has(&format!("LAB_OPEN_LOG={}/open.log", run.w.display()))
    }

    #[test]
    fn an_atpkg_build_record_names_its_version() {
        let rec = "schema = 1\nprogram = \"agy\"\nversion = \"1.4.0\"\nvendor = \"Google\"\n";
        assert_eq!(vendor_version(rec), "1.4.0");
        assert_eq!(vendor_version("program = \"agy\"\n"), "");
        let line = "agy → /S/pkg/reroute/agy → /S/pkg/store/agy/1000001000004000000/bin/agy — managed 1.4.0 — Google";
        assert_eq!(
            managed_bin("agy", line).as_deref(),
            Some("/S/pkg/store/agy/1000001000004000000/bin/agy")
        );
    }

    #[test]
    fn muse_code_s_build_is_read_from_its_record_or_its_version_line() {
        let rec =
            "schema = 1\nprogram = \"muse\"\nversion = \"1.4.10-R6427.1\"\nvendor = \"Meta\"\n";
        assert_eq!(vendor_version(rec), "1.4.10-R6427.1");
        assert_eq!(vendor_version("program = \"muse\"\nversion_note = 2\n"), "");
        assert_eq!(
            muse_build("Muse Code 1.4.10 (1.4.10-R6427.1)"),
            Some("1.4.10-R6427.1")
        );
        assert_eq!(muse_build("Muse Code 1.4.10"), None);
        let line = "muse → /S/pkg/reroute/muse → /S/pkg/store/muse/2001004010006427002/bin/muse — managed 1.4.10-R6427.1 — Meta latest";
        assert_eq!(
            managed_bin("muse", line).as_deref(),
            Some("/S/pkg/store/muse/2001004010006427002/bin/muse")
        );
        assert_eq!(managed_bin("muse", "codex → /x — managed 1 — y"), None);
    }

    #[test]
    fn a_dev_linked_package_names_its_binary() {
        let line = "codex → /S/pkg/bin/codex → /D/artifacts/codex/0.164.0-dev.1+g51ba34fe1/bin/codex — dev-linked (managed from that checkout, not the index)";
        assert_eq!(
            managed_bin("codex", line).as_deref(),
            Some("/D/artifacts/codex/0.164.0-dev.1+g51ba34fe1/bin/codex")
        );
        assert_eq!(
            managed_bin("codex", "codex → /usr/local/bin/codex — on PATH"),
            None
        );
    }

    #[test]
    fn the_tripwire_log_is_kept_with_its_account_masked() {
        assert_eq!(
            mask_account(
                "12:16:51 find-generic-password account=\"owner\" service=\"Claude Code-0123abcd\" via=argv"
            ),
            "12:16:51 find-generic-password account=\"<user>\" service=\"Claude Code-0123abcd\" via=argv"
        );
    }

    fn pairs(words: &[(&str, &str)]) -> Vec<(String, String)> {
        words
            .iter()
            .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
            .collect()
    }

    #[test]
    fn a_home_image_is_sized_in_hdiutils_spelling_and_needs_room_past_itself() {
        assert_eq!(home_image_bytes("16g"), Ok(16 << 30));
        assert_eq!(home_image_bytes("2048m"), Ok(2 << 30));
        for bad in [
            "", "g", "16", "16G", "-1g", "+1g", "1.5g", "16gb", "16é", "512m", "65g",
        ] {
            assert!(home_image_bytes(bad).is_err(), "{bad:?}");
        }
        assert!(home_image_bytes(&format!("{}g", u64::MAX)).is_err());
        let need = (16 + HOME_IMAGE_SPARE_GIB) << 30;
        assert_eq!(home_image_refused("16g", Some(need)), None);
        let short = home_image_refused("16g", Some(need - 1)).expect("refused");
        assert!(short.contains("needs 24 GiB"), "{short}");
        assert!(
            home_image_refused("16g", None).is_some(),
            "unknown free space"
        );
        assert!(
            home_image_refused("16", Some(u64::MAX)).is_some(),
            "no size"
        );
    }

    #[test]
    fn the_instance_is_handed_tmpdir_and_dev_seams_alone() {
        assert_eq!(instance_env(""), Ok(Vec::new()));
        assert_eq!(
            instance_env(" TMPDIR=/tmp  ATERM_DEBUG_PASTE_FILE=/s/p.txt "),
            Ok(pairs(&[
                ("TMPDIR", "/tmp"),
                ("ATERM_DEBUG_PASTE_FILE", "/s/p.txt")
            ]))
        );
        assert_eq!(
            instance_env("ATERM_DEBUG_CLIPBOARD_FILE=/s/c=d"),
            Ok(pairs(&[("ATERM_DEBUG_CLIPBOARD_FILE", "/s/c=d")]))
        );
        assert_eq!(
            instance_env("ATERM_DEV_CARD_PROBE=1"),
            Ok(pairs(&[("ATERM_DEV_CARD_PROBE", "1")]))
        );
        assert_eq!(
            instance_env("ATERM_OCCUPANCY_NUDGE_S=60"),
            Ok(pairs(&[("ATERM_OCCUPANCY_NUDGE_S", "60")]))
        );
        for bad in [
            "ATERM_UPGRADE_QUIET_S=1",
            "ATERM_OCCUPANCY_NUDGE=60",
            "ATERM_OCCUPANCY_NUDGE_S=",
            "HOME=/x",
            "PATH=/x",
            "XDG_RUNTIME_DIR=/x",
            "ATERM_STATE_HOME=/x",
            "ATERM_DEBUG_HEADLESS_RESTORE=1",
            "ATERM_HARNESS_TAPE=0",
            "CLAUDE_CONFIG_DIR=/x",
            "TMP=/x",
            "ATERM_DEBUG_=1",
            "ATERM_DEBUG_paste=1",
            "ATERM_DEV_=1",
            "ATERM_DEV_card=1",
            "ATERM_DEVX=1",
            "TMPDIR",
            "=x",
            "TMPDIR=",
            "TMPDIR=tmp",
            "TMPDIR=/a TMPDIR=/b",
            "ATERM_DEBUG_X=a\u{7}b",
        ] {
            assert!(instance_env(bad).is_err(), "{bad}");
        }
    }

    fn recorded(instance_env: &str, seam: &str) -> Run {
        Run {
            run: "/s/runs/k1".into(),
            w: "/s/runs/k1/scratch".into(),
            sockdir: "/tmp/at.AbCd".into(),
            sock: "/tmp/at.AbCd/a.sock".into(),
            proj: "/tmp/at.AbCd/proj".into(),
            cols: "144".into(),
            rows: "50".into(),
            seam: seam.into(),
            instance_env: instance_env.into(),
            ..Run::default()
        }
    }

    #[test]
    fn every_launch_hands_the_instance_what_up_recorded() {
        let vars = instance_vars(&recorded("TMPDIR=/tmp", "0"), "owner").unwrap();
        let get = |k: &str| {
            vars.iter()
                .filter(|(n, _)| n == k)
                .map(|(_, v)| v.as_str())
                .collect::<Vec<_>>()
        };
        assert_eq!(get("TMPDIR"), vec!["/tmp"]);
        assert_eq!(get("HOME"), vec!["/s/runs/k1/scratch/home"]);
        assert_eq!(get("XDG_RUNTIME_DIR"), vec!["/tmp/at.AbCd"]);
        assert_eq!(get("ATERM_STATE_HOME"), vec!["/s/runs/k1/scratch/state"]);
        assert!(get("ATERM_DEBUG_HEADLESS_RESTORE").is_empty());
        assert_eq!(
            get("ATERM_DEBUG_SYSTEM_PRESSURE"),
            vec!["none"],
            "the host's memory pressure stays out of a lab run"
        );
        assert_eq!(
            vars.last(),
            Some(&("TMPDIR".to_string(), "/tmp".to_string()))
        );
        let seam = instance_vars(&recorded("", "1"), "owner").unwrap();
        assert!(seam.contains(&("ATERM_DEBUG_HEADLESS_RESTORE".to_string(), "1".to_string())));
        assert!(!seam.iter().any(|(k, _)| k == "TMPDIR"));
        for edited in [
            "HOME=/elsewhere",
            "ATERM_DEBUG_HEADLESS_RESTORE=1",
            "ATERM_DEBUG_SYSTEM_PRESSURE=critical",
        ] {
            assert!(
                instance_vars(&recorded(edited, "1"), "owner").is_err(),
                "{edited}"
            );
        }
    }

    #[test]
    fn the_store_lookup_is_handed_the_account_home_under_a_scratch_home() {
        let top = aterm_tempfile::Builder::new()
            .prefix("lab-which-")
            .tempdir()
            .expect("a scratch dir");
        let root = fs::canonicalize(top.path()).expect("its physical path");
        let (account, scratch) = (root.join("account"), root.join("scratch-home"));
        for d in [&account, &scratch] {
            fs::create_dir_all(d).expect("a home");
        }
        let stand_in = root.join("aterm");
        fs::write(
            &stand_in,
            "#!/bin/sh\n[ \"$1 $2\" = 'pkg which' ] || exit 2\n\
             printf '%s → %s → %s — managed 1.0 — x\\n' \"$3\" \"$HOME/pkg/bin/$3\" \"$HOME/pkg/store/$3/1/bin/$3\"\n",
        )
        .and_then(|()| fs::set_permissions(&stand_in, fs::Permissions::from_mode(0o755)))
        .expect("the stand-in");
        let p =
            crate::paths::Paths::resolve_with(Some(account.clone()), Some(scratch.clone().into()));
        assert_eq!(p.store_home(), account.display().to_string());
        let store =
            |home: &Path, name: &str| format!("{}/pkg/store/{name}/1/bin/{name}", home.display());
        assert_eq!(store_claude(&p, &stand_in), Ok(store(&account, "claude")));
        for name in ["codex", "agy", "muse"] {
            assert_eq!(
                managed_store_bin_in(&p, &stand_in, name),
                Some(store(&account, name)),
                "{name}"
            );
        }
        let bare =
            crate::paths::Paths::resolve_with(Some(scratch.clone()), Some(scratch.clone().into()));
        assert_eq!(
            store_claude(&bare, &stand_in),
            Ok(store(&scratch, "claude"))
        );
        assert_eq!(
            managed_store_bin_in(&bare, &stand_in, "codex"),
            Some(store(&scratch, "codex"))
        );
        let asked = |home: &str| {
            let line = pkg_which(&stand_in, home, "claude").expect("the stand-in runs");
            managed_claude(&line).unwrap_or_else(|| panic!("no managed line: {line}"))
        };
        assert_eq!(asked(p.store_home()), store(&account, "claude"));
        assert_eq!(
            managed_bin(
                "agy",
                &pkg_which(&stand_in, p.store_home(), "agy").expect("agy")
            ),
            Some(format!("{}/pkg/store/agy/1/bin/agy", account.display()))
        );
        assert_eq!(p.caller_home, scratch.display().to_string());
        assert_ne!(asked(&p.caller_home), store(&account, "claude"));
        assert_eq!(asked(&p.caller_home), store(&scratch, "claude"));
        let only = crate::paths::Paths::resolve_with(None, Some(scratch.clone().into()));
        assert_eq!(only.store_home(), scratch.display().to_string());
    }

    fn opens_logged(browser: &str, log: &str, which: &str, run: &Run) -> bool {
        let shim = run.run.join("shim/open").display().to_string();
        let opener = if browser.is_empty() { which } else { browser };
        log == format!("{}/open.log", run.w.display()) && opener == shim
    }

    fn login_shell_reads(vars: &[(String, String)], cwd: &Path) -> Vec<String> {
        let mut cmd = aterm_uds::spawnfd::command("/bin/zsh");
        cmd.env_clear()
            .args([
                "-l",
                "-c",
                r#"printf '%s\n' "${BROWSER-}" "${LAB_OPEN_LOG-}" "$(command -v open)""#,
            ])
            .current_dir(cwd)
            .stdin(Stdio::null());
        for (k, v) in vars {
            cmd.env(k, v);
        }
        let out = aterm_uds::deadline::output_until(
            &mut cmd,
            std::time::Instant::now() + Duration::from_secs(60),
        )
        .expect("a login zsh ends within the hang detector");
        assert!(
            out.status.success(),
            "{:?}: {}",
            out.status,
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8_lossy(&out.stdout)
            .lines()
            .map(str::to_string)
            .collect()
    }

    #[test]
    fn a_session_the_instance_spawns_opens_through_the_recorder() {
        let top = aterm_tempfile::Builder::new()
            .prefix("lab-browser-")
            .tempdir()
            .expect("a scratch dir");
        let root = fs::canonicalize(top.path()).expect("its physical path");
        let mut run = recorded("", "0");
        run.run = root.join("runs/k1");
        run.w = run.run.join("scratch");
        fs::create_dir_all(run.w.join("home")).expect("the scratch home");
        fs::create_dir_all(run.run.join("shim")).expect("the shim");
        std::os::unix::fs::symlink("/usr/bin/true", run.run.join("shim/open")).expect("its open");
        let vars = instance_vars(&run, "owner").expect("the instance's environment");
        let get = |k: &str| {
            vars.iter()
                .filter(|(n, _)| n == k)
                .map(|(_, v)| v.as_str())
                .collect::<Vec<_>>()
        };
        let shim_open = run.run.join("shim/open").display().to_string();
        let log = format!("{}/open.log", run.w.display());
        assert_eq!(get("BROWSER"), [shim_open.as_str()]);
        assert_eq!(get("LAB_OPEN_LOG"), [log.as_str()]);
        assert!(get("PATH")[0].starts_with(&format!("{}/shim:", run.run.display())));
        assert!(lab_sets("BROWSER") && lab_sets("LAB_OPEN_LOG"));
        let read = login_shell_reads(&vars, &run.w);
        assert_eq!(read.len(), 3, "{read:?}");
        assert_eq!(read[0], shim_open, "BROWSER passes the login shell");
        assert_eq!(read[1], log, "LAB_OPEN_LOG passes the login shell");
        assert_eq!(read[2], "/usr/bin/open", "path_helper puts the shim last");
        assert!(opens_logged(&read[0], &read[1], &read[2], &run), "{read:?}");
        let bare: Vec<(String, String)> = vars
            .iter()
            .filter(|(k, _)| k != "BROWSER")
            .cloned()
            .collect();
        let read = login_shell_reads(&bare, &run.w);
        assert_eq!(read.first().map(String::as_str), Some(""), "{read:?}");
        assert!(
            !opens_logged(&read[0], &read[1], &read[2], &run),
            "{read:?}"
        );
    }

    #[test]
    fn the_sleep_clock_reaches_the_instance_and_claude_code_of_a_lab_up_with_it_alone() {
        let mut run = recorded("TMPDIR=/tmp", "0");
        run.port = "5".into();
        run.claude_bin = "/s/runs/k1/clock/claude".into();
        let off = instance_vars(&run, "owner").unwrap();
        let script = launcher(&run, &parse_launch(&[]).unwrap());
        assert!(
            !off.iter().any(|(k, _)| crate::clock::is_clock_var(k)),
            "{off:?}"
        );
        assert!(
            !script.contains("DYLD_") && !script.contains("LABCLOCK_"),
            "{script}"
        );
        run.clock = "1".into();
        let on = instance_vars(&run, "owner").unwrap();
        let clock = crate::clock::vars(&run);
        assert_eq!(clock.len(), 4);
        for kv in &clock {
            assert!(on.contains(kv), "{kv:?} not in {on:?}");
        }
        let l = parse_launch(&["-e".to_string(), "K=v".to_string()]).unwrap();
        let script = launcher(&run, &l);
        let at = |needle: &str| {
            script
                .find(needle)
                .unwrap_or_else(|| panic!("no {needle} in {script}"))
        };
        assert!(
            at("  DYLD_INSERT_LIBRARIES=/s/runs/k1/clock/libaterm_lab_clock.dylib \\\n")
                < at("  K=v \\\n")
        );
        at("  LABCLOCK_FILE=/s/runs/k1/clock/offset \\\n");
        at("  LABCLOCK_DIR=/s/runs/k1/scratch/clock \\\n");
        at("  LABCLOCK_CONTINUOUS=1 \\\n");
        assert!(script.ends_with("  /s/runs/k1/clock/claude\n"), "{script}");
        for bad in [
            "DYLD_INSERT_LIBRARIES=/x.dylib",
            "LABCLOCK_FILE=/x",
            "LABCLOCK_CONTINUOUS",
        ] {
            assert!(
                parse_launch(&["-e".to_string(), bad.to_string()]).is_err(),
                "claude -e {bad}"
            );
            assert!(instance_env(bad).is_err(), "LAB_INSTANCE_ENV={bad}");
        }
        assert!(lab_sets("DYLD_INSERT_LIBRARIES") && lab_sets("LABCLOCK_DIR"));
    }

    #[test]
    fn the_relaunch_runs_the_instance_on_the_lab_s_own_socket_and_grid() {
        let run = recorded("", "0");
        let argv = instance_argv(&run, "/b/aterm", None, false);
        assert_eq!(
            argv,
            [
                "/b/aterm",
                "--headless",
                "--no-reroute",
                "--control-sock",
                "/tmp/at.AbCd/a.sock",
                "-d",
                "/tmp/at.AbCd/proj",
                "--columns",
                "144",
                "--lines",
                "50"
            ]
            .map(str::to_string)
        );
        let armed = instance_argv(&run, "/b/aterm", Some("2"), true);
        assert_eq!(armed[2..4], lifeline_flag()[..]);
        assert_eq!(
            armed[armed.len() - 2..],
            ["--scale", "2"].map(str::to_string)
        );
    }

    /// A `LAB_REROUTE=1` lab's instance line drops only `--no-reroute`; every other lab keeps it.
    #[test]
    fn a_reroute_lab_s_instance_drops_no_reroute_and_keeps_the_rest() {
        let plain = instance_argv(&recorded("", "0"), "/b/aterm", None, false);
        let run = Run {
            reroute: "1".into(),
            ..recorded("", "0")
        };
        let rerouting = instance_argv(&run, "/b/aterm", None, false);
        let mut without = plain.clone();
        without.retain(|a| a != "--no-reroute");
        assert_eq!(plain.len(), without.len() + 1, "{plain:?}");
        assert_eq!(rerouting, without);
        assert_eq!(
            opts_refused(&UpOpts {
                reroute: true,
                clock: true,
                ..UpOpts::default()
            })
            .map(|why| why.starts_with("LAB_REROUTE=1 takes no LAB_CLOCK=1")),
            Some(true)
        );
    }

    /// `--by-name` launches `claude` found on `PATH`; without it the launcher names the binary by
    /// path.
    #[test]
    fn by_name_launches_claude_through_path() {
        let run = recorded("", "0");
        let named = launcher(
            &run,
            &Launch {
                by_name: true,
                ..Launch::default()
            },
        );
        let last = |s: &str| s.lines().last().unwrap_or("").trim().to_string();
        assert_eq!(last(&named), "claude");
        let pathed = launcher(&run, &Launch::default());
        assert_eq!(last(&pathed), shell_quote(&run.claude_bin));
        assert_eq!(
            parse_launch(&["--by-name".to_string()]).map(|l| l.by_name),
            Ok(true)
        );
    }

    #[test]
    fn a_windowed_lab_s_instance_opens_a_window_its_keeper_holding_the_lifeline() {
        let tail = [
            "--no-reroute",
            "--control-sock",
            "/tmp/at.AbCd/a.sock",
            "-d",
            "/tmp/at.AbCd/proj",
            "--columns",
            "144",
            "--lines",
            "50",
        ]
        .map(str::to_string);
        let run = Run {
            window: "1".into(),
            ..recorded("", "0")
        };
        assert!(windowed(&run));
        let bare = instance_argv(&run, "/b/aterm", None, false);
        assert_eq!(bare[0], "/b/aterm");
        assert_eq!(bare[1..], tail[..]);
        let kept = instance_argv(&run, "/b/aterm", None, true);
        assert_eq!(
            kept[..4],
            ["/s/runs/k1/bin/aterm-lab", "keeper", "--", "/b/aterm"].map(str::to_string)
        );
        assert_eq!(kept[4..], tail[..]);
        for line in [&bare, &kept] {
            assert!(
                !line
                    .iter()
                    .any(|a| a == "--headless" || a.starts_with("--lifeline")),
                "{line:?}"
            );
        }
        for headless in ["0", ""] {
            let run = Run {
                window: headless.into(),
                ..recorded("", "0")
            };
            assert!(!windowed(&run));
            let line = instance_argv(&run, "/b/aterm", None, true);
            assert_eq!(line[..2], ["/b/aterm", "--headless"].map(str::to_string));
            assert_eq!(line[2..4], lifeline_flag()[..]);
        }
    }

    #[test]
    fn a_windowed_lab_under_the_restore_seam_is_refused_before_anything_is_made() {
        let o = |window, restore| UpOpts {
            window,
            restore,
            ..UpOpts::default()
        };
        let why = opts_refused(&o(true, true)).expect("refused");
        assert!(why.contains("LAB_RESTORE=1"), "{why}");
        let f = up("not a tag!", &o(true, true)).expect_err("refused");
        assert_eq!(f.code, 2, "{f:?}");
        assert_eq!(f.msg.as_deref(), Some(why));
        for (window, restore) in [(true, false), (false, true), (false, false)] {
            assert_eq!(
                opts_refused(&o(window, restore)),
                None,
                "window {window} restore {restore}"
            );
        }
    }

    #[test]
    fn a_starting_child_is_waited_for_while_it_makes_progress() {
        let t0 = Instant::now();
        let s = |n: u64| t0 + Duration::from_secs(n);
        let looks = std::cell::Cell::new(0u64);
        let clock = std::cell::Cell::new(0u64);
        let tick = || {
            clock.set(clock.get() + 1);
            Ok(s(clock.get()))
        };
        let answered = while_starting(
            || {
                looks.set(looks.get() + 1);
                Ok(looks.get() == 3)
            },
            || 0,
            READY_HANG,
            t0,
            tick,
        );
        assert_eq!(answered, Ok(()));
        assert_eq!(looks.get(), 3);
        let e = while_starting(|| Err("ended".into()), || 0, READY_HANG, t0, || Ok(t0))
            .expect_err("ended");
        assert_eq!(e, "ended");
        // Starved: ready at the 300th second, its CPU time moving every 30 s.
        clock.set(0);
        let started = while_starting(
            || Ok(clock.get() >= 300),
            || clock.get() / 30,
            READY_HANG,
            t0,
            tick,
        );
        assert_eq!(started, Ok(()), "a starved start is no hang");
        // The negative control: the same start whose progress stood still from the 100th
        // second is a hang at the 160th.
        clock.set(0);
        let e = while_starting(
            || Ok(clock.get() >= 300),
            || clock.get().min(100),
            READY_HANG,
            t0,
            tick,
        )
        .expect_err("hang");
        assert!(e.contains("made no progress"), "{e}");
        assert_eq!(
            clock.get(),
            160,
            "the hang is named a minute after the last move"
        );
        let e = while_starting(
            || Ok(false),
            || 0,
            READY_HANG,
            t0,
            || Err("interrupted".to_string()),
        )
        .expect_err("interrupted");
        assert_eq!(e, "interrupted");
    }

    #[test]
    fn a_pid_is_the_lab_s_only_while_its_command_line_names_the_lab() {
        let me = i32::try_from(std::process::id()).unwrap();
        let elsewhere = recorded("", "0");
        for pid in [0, 1, -1, me] {
            assert!(!names_this_lab(&elsewhere, pid), "{pid}");
        }
        let line = procs::command_line(me);
        let program = line.split_whitespace().next().unwrap_or_default();
        let dir = Path::new(program).parent().unwrap_or(Path::new(""));
        assert!(!dir.as_os_str().is_empty(), "{line}");
        let ours = Run {
            run: dir.to_path_buf(),
            sockdir: String::new(),
            ..elsewhere
        };
        assert!(names_this_lab(&ours, me), "{line}");
    }

    #[test]
    fn a_saved_pid_is_signalled_only_while_it_is_the_process_that_started_then() {
        let mut child = aterm_uds::spawnfd::command("/bin/sleep")
            .arg("600")
            .spawn()
            .expect("a child");
        let pid = i32::try_from(child.id()).expect("a pid");
        let start = procs::start_time(pid);
        assert!(!start.is_empty(), "the child's start is read");
        assert_eq!(still_started(pid, &start), pid, "still that process");
        assert_eq!(
            still_started(pid, "Thu Jan  1 00:00:00 1970"),
            0,
            "another start"
        );
        assert_eq!(still_started(pid, ""), 0, "a start never read");
        for none in [0, 1, -1] {
            assert_eq!(still_started(none, &start), 0, "{none}");
        }
        let _ = child.kill();
        let _ = child.wait();
        assert_eq!(
            still_started(pid, &start),
            0,
            "gone: its number is no one's of then"
        );
    }

    fn half_made(root: &Path) -> Run {
        let root = fs::canonicalize(root).unwrap();
        let run = Run {
            tag: "abort-t".into(),
            run: root.join("runs/abort-t"),
            w: root.join("runs/abort-t/scratch"),
            ..Run::default()
        };
        fs::create_dir_all(&run.w).unwrap();
        fs::create_dir_all(run.run.join("bin")).unwrap();
        fs::write(run.run.join(BIN), "a copy").unwrap();
        run
    }

    #[test]
    fn an_aborted_up_keeps_a_copy_nothing_showed_ran() {
        let root = aterm_tempfile::tempdir().unwrap();
        let none = i32::MAX;
        for (what, sampler, stub, ran, kept) in [
            (
                "the witness started, the stub never ready",
                none,
                0,
                false,
                true,
            ),
            ("both started, no ready byte", none, none, false, true),
            ("the stub said it ran", none, none, true, false),
            ("nothing started from the copy", 0, 0, false, false),
        ] {
            let mut run = half_made(root.path());
            run.sampler_pid = sampler;
            run.stub_pid = stub;
            if ran {
                fs::write(run.run.join(BIN_RAN), "").unwrap();
            }
            let failed = abort(&run, &[], "x");
            assert_eq!(failed.msg.as_deref(), Some("x"), "{what}");
            assert_eq!(run.run.join(BIN).is_file(), kept, "{what}");
            let stamp = aterm_uds::deadline::exec_stamp(&run.run.join("bin"));
            assert_eq!(
                stamp.is_some(),
                kept,
                "{what}: its wait runs from the abort"
            );
            let _ = fs::remove_dir_all(root.path().join("runs"));
        }
    }

    #[test]
    fn a_scratch_hold_keeps_an_aborted_up_before_its_first_lab_child() {
        let root = aterm_tempfile::tempdir().unwrap();
        let state = fs::canonicalize(root.path()).unwrap();
        for (what, released, unread, kept) in [
            ("reroute probe failed", false, false, true),
            ("reroute probe showed it ran", true, false, false),
            ("hold inventory unread", false, true, true),
        ] {
            let run = half_made(root.path());
            let clone = run.w.join("home/clone");
            fs::create_dir_all(clone.parent().unwrap()).unwrap();
            fs::write(&clone, "data, not an executable").unwrap();
            hold_scratch(&run.run, "reroute").unwrap();
            if released {
                release_scratch(&run.run, "reroute");
            }
            if unread {
                fs::remove_dir_all(run.run.join(HELD)).unwrap();
                fs::write(run.run.join(HELD), "not a directory").unwrap();
            }
            let hold = run.run.join(HELD).join("reroute");
            if !released && !unread {
                aterm_uds::deadline::stamp_exec(&hold);
            }
            let at_exec = aterm_uds::deadline::exec_stamp(&hold);
            assert_eq!((run.sampler_pid, run.stub_pid, run.instance_pid), (0, 0, 0));
            let failed = abort(&run, &[], "probe failed");
            assert_eq!(failed.msg.as_deref(), Some("probe failed"), "{what}");
            assert_eq!(clone.is_file(), kept, "{what}");
            if kept {
                assert_eq!(all_runs(&state), ["abort-t"], "{what}: ps lists it");
                assert!(run.run.join("env.sh").is_file(), "{what}: down can load it");
                if !unread {
                    assert_eq!(aterm_uds::deadline::exec_stamp(&hold), at_exec, "{what}");
                }
                // A saved lab must be taken down before its tag can come up again.
                // The fake program is data, so isolate the clone's scratch hold.
                fs::write(run.run.join(BIN_RAN), "").unwrap();
                let now = std::time::SystemTime::now();
                assert!(room_for(&run, now).is_err(), "{what}: down is required");
                let unstamped = !stamp_holds(&run.run);
                clear_run_dir(&run.run, false, unstamped, &run.tag, past_the_hang(now));
                assert!(
                    clone.is_file(),
                    "{what}: wall time cannot spend an unread hold"
                );
                assert!(
                    run.run.join("env.sh").is_file(),
                    "{what}: down keeps its record"
                );
                if unread {
                    fs::remove_file(run.run.join(HELD)).unwrap();
                } else {
                    let hold = run.run.join(HELD).join("reroute");
                    assert!(aterm_uds::deadline::exec_stamp(&hold).is_some(), "{what}");
                    // A deterministic restart observation spends the awake-clock stamp.
                    fs::write(
                        hold.join(aterm_uds::deadline::EXEC_STAMP),
                        u64::MAX.to_string(),
                    )
                    .unwrap();
                }
                let unstamped = !stamp_holds(&run.run);
                clear_run_dir(&run.run, false, unstamped, &run.tag, now);
                assert!(!clone.exists(), "{what}: the later down may reclaim it");
                assert!(
                    !run.run.join("env.sh").exists(),
                    "{what}: down removed its record"
                );
                assert!(room_for(&run, now).is_ok(), "{what}: custody is resolved");
            }
            let _ = fs::remove_dir_all(root.path().join("runs"));
        }
    }

    /// A home image (`LAB_HOME_IMAGE`) holds the scratch's `home/`, so it comes down only with the
    /// scratch: an aborted up whose scratch a hold keeps leaves it attached, as does a down before
    /// the hold is spent, and the down that removes the scratch detaches it and removes its file.
    /// Before this `abort` and `down` detached it whatever was held, taking a held copy's
    /// directory away while macOS might still be assessing its first exec.
    #[test]
    fn a_home_image_comes_down_only_with_its_scratch() {
        let root = aterm_tempfile::tempdir().unwrap();
        let mut run = half_made(root.path());
        // The lab program's copy showed it ran, so only the scratch's hold keeps the run.
        fs::write(run.run.join(BIN_RAN), "").unwrap();
        let image = home_image_file(&run.run);
        fs::write(&image, "a stand-in for the image's file").unwrap();
        run.home_image = image.to_string_lossy().into_owned();
        run.save();
        let clone = run.w.join("home/clone");
        fs::create_dir_all(clone.parent().unwrap()).unwrap();
        fs::write(&clone, "data, not an executable").unwrap();
        hold_scratch(&run.run, "reroute").unwrap();
        let hold = run.run.join(HELD).join("reroute");
        aterm_uds::deadline::stamp_exec(&hold);
        let failed = abort(&run, &[], "probe failed");
        assert_eq!(failed.msg.as_deref(), Some("probe failed"));
        assert!(clone.is_file(), "an aborted up keeps a held scratch");
        assert!(image.is_file(), "and its home image");
        let now = std::time::SystemTime::now();
        let unstamped = !stamp_holds(&run.run);
        clear_run_dir(&run.run, false, unstamped, &run.tag, now);
        assert!(
            clone.is_file(),
            "a down before the hold is spent keeps the scratch"
        );
        assert!(image.is_file(), "and its home image");
        assert!(run.run.join("env.sh").is_file(), "and its record");
        // A deterministic restart observation spends the awake-clock stamp.
        fs::write(
            hold.join(aterm_uds::deadline::EXEC_STAMP),
            u64::MAX.to_string(),
        )
        .unwrap();
        let unstamped = !stamp_holds(&run.run);
        clear_run_dir(&run.run, false, unstamped, &run.tag, now);
        assert!(!clone.exists(), "the down that removes the scratch");
        assert!(
            !image.exists(),
            "detaches its home image and removes its file"
        );
        assert!(room_for(&run, now).is_ok(), "custody is resolved");
    }

    #[test]
    fn copies_go_once_shown_to_run_or_past_the_first_exec_hang() {
        let root = aterm_tempfile::tempdir().unwrap();
        let dir = root.path().join("bin");
        fs::create_dir_all(&dir).unwrap();
        let made = fs::metadata(&dir).unwrap().modified().unwrap();
        let hang = aterm_uds::deadline::FIRST_EXEC_HANG;
        let minute = Duration::from_secs(60);
        assert!(!copies_may_go(&dir, false, made), "just made, unproven");
        assert!(!copies_may_go(&dir, false, made + hang - minute));
        assert!(copies_may_go(&dir, true, made), "shown to run");
        assert!(copies_may_go(&dir, false, made + hang), "past the hang");
        assert!(copies_may_go(&root.path().join("gone"), false, made));
    }

    fn torn_down(root: &Path, bin_ran: bool, relay: bool) -> (Run, std::time::SystemTime) {
        let run = half_made(root);
        fs::remove_dir_all(&run.w).unwrap();
        for f in ["env.sh", "jail.sb", "stub.out"] {
            fs::write(run.run.join(f), "").unwrap();
        }
        if bin_ran {
            fs::write(run.run.join(BIN_RAN), "").unwrap();
        }
        let made = fs::metadata(run.run.join("bin")).unwrap().modified();
        if relay {
            fs::create_dir_all(run.run.join("relay")).unwrap();
            fs::write(run.run.join("relay/node"), "a copy").unwrap();
        }
        (run, made.unwrap())
    }

    fn past_the_hang(made: std::time::SystemTime) -> std::time::SystemTime {
        made + aterm_uds::deadline::FIRST_EXEC_HANG + Duration::from_secs(60)
    }

    #[test]
    fn a_down_that_keeps_a_copy_keeps_its_lab_for_a_later_down() {
        let root = aterm_tempfile::tempdir().unwrap();
        for (what, bin_ran, relay, relay_ran, kept) in [
            ("stub's copy unproven", false, false, false, true),
            ("relay's copy unproven", true, true, false, true),
            ("both copies ran", true, true, true, false),
            ("stub's copy ran, no relay", true, false, false, false),
        ] {
            let (run, made) = torn_down(root.path(), bin_ran, relay);
            let state = run.run.parent().and_then(Path::parent).unwrap();
            clear_run_dir(&run.run, relay_ran, false, &run.tag, made);
            assert!(!run.run.join("jail.sb").exists(), "{what}");
            assert_eq!(run.run.join("env.sh").is_file(), kept, "{what}");
            let listed = if kept { vec![run.tag.clone()] } else { vec![] };
            assert_eq!(all_runs(state), listed, "{what}");
            if kept {
                clear_run_dir(&run.run, relay_ran, false, &run.tag, past_the_hang(made));
            }
            assert!(!run.run.exists(), "{what}: the later down finished");
        }
    }

    /// The tabs' launch records (`launch/<sid>.sh`), which `down` keeps in its logs first, go
    /// with the run directory: a lab that launched Claude Code leaves nothing of its run behind.
    #[test]
    fn a_down_removes_the_tabs_launch_records_with_the_run() {
        let root = aterm_tempfile::tempdir().unwrap();
        let (run, made) = torn_down(root.path(), true, false);
        let record = run.run.join(LAUNCHES).join("s-c99d4225e7307a6cc1f4.sh");
        fs::create_dir_all(record.parent().unwrap()).unwrap();
        fs::write(
            &record,
            "#!/bin/sh\nexec env -u CLAUDE_CONFIG_DIR /c/claude\n",
        )
        .unwrap();
        clear_run_dir(&run.run, false, false, &run.tag, made);
        assert!(!run.run.exists(), "the run directory went whole");
    }

    /// A clone held in the scratch that never showed it ran keeps the scratch and `env.sh` past any
    /// wall time, until a restart since its stamp; one shown to run released its hold.
    #[test]
    fn a_down_keeps_a_scratch_a_clone_that_never_showed_it_ran_holds() {
        let root = aterm_tempfile::tempdir().unwrap();
        for (what, proven, kept) in [
            ("never showed it ran", false, true),
            ("showed it ran", true, false),
        ] {
            let (run, made) = torn_down(root.path(), true, false);
            fs::create_dir_all(&run.w).unwrap();
            hold_scratch(&run.run, "agy-1").unwrap();
            if proven {
                release_scratch(&run.run, "agy-1");
            }
            assert!(stamp_holds(&run.run), "{what}");
            clear_run_dir(&run.run, false, false, &run.tag, past_the_hang(made));
            assert_eq!(run.w.is_dir(), kept, "{what}");
            assert_eq!(run.run.join("env.sh").is_file(), kept, "{what}");
            if kept {
                let stamp = run.run.join(HELD).join("agy-1/exec");
                // above any reading the awake clock gives now: a restart since the stamp
                let restart = u64::MAX.to_string();
                fs::write(&stamp, &restart).unwrap();
                assert!(stamp_holds(&run.run), "{what}");
                assert_eq!(fs::read_to_string(&stamp).unwrap(), restart, "{what}");
                clear_run_dir(&run.run, false, false, &run.tag, made);
            }
            assert!(!run.run.exists(), "{what}: the scratch and its holds went");
        }
    }

    /// A clock clone with neither mark (started, and no version line at `up`) is stamped and kept
    /// past any wall time; with its version line, its never-started mark, or no clone, it goes at
    /// once, unstamped.
    #[test]
    fn the_clocks_clone_stays_until_it_showed_it_ran() {
        let root = aterm_tempfile::tempdir().unwrap();
        for (what, clone, mark, kept) in [
            ("its clone printed nothing", true, None, true),
            ("it printed its version", true, Some(CLOCK_RAN), false),
            ("it was never started", true, Some(CLOCK_UNSTARTED), false),
            ("no clone", false, None, false),
        ] {
            let (run, made) = torn_down(root.path(), true, false);
            fs::create_dir_all(run.run.join("clock")).unwrap();
            if clone {
                fs::write(run.run.join(CLOCK_CLONE), "a clone").unwrap();
            }
            if let Some(mark) = mark {
                fs::write(run.run.join(mark), "").unwrap();
            }
            assert!(stamp_holds(&run.run), "{what}");
            let stamp = run.run.join("clock").join(aterm_uds::deadline::EXEC_STAMP);
            assert_eq!(stamp.is_file(), kept, "{what}");
            let now = if kept { past_the_hang(made) } else { made };
            clear_run_dir(&run.run, false, false, &run.tag, now);
            assert_eq!(run.run.join(CLOCK_CLONE).is_file(), kept, "{what}");
            assert_eq!(run.run.join("env.sh").is_file(), kept, "{what}");
            let _ = fs::remove_dir_all(root.path().join("runs"));
        }
    }

    /// An unproven relay, lab-program copy or twin probe stamped at `down` waits on the awake clock
    /// from that stamp, neither its age nor a sleep freeing it; unstamped, its age is its mtime's.
    #[test]
    fn an_unproven_copy_ages_on_the_awake_clock_from_its_down() {
        let root = aterm_tempfile::tempdir().unwrap();
        for (what, copy, stamped, kept) in [
            ("a relay, stamped by its down", "relay/node", true, true),
            ("a relay, never stamped", "relay/node", false, false),
            ("the lab's copy, stamped by its down", BIN, true, true),
            ("the lab's copy, never stamped", BIN, false, false),
            (
                "a twin probe, stamped by its down",
                "twin-src/claude",
                true,
                true,
            ),
            (
                "a twin probe, never stamped",
                "twin-src/claude",
                false,
                false,
            ),
        ] {
            let (run, made) = torn_down(root.path(), copy != BIN, copy == "relay/node");
            let copy = run.run.join(copy);
            let dir = copy.parent().unwrap().to_path_buf();
            fs::create_dir_all(&dir).unwrap();
            fs::write(&copy, "a copy").unwrap();
            if stamped {
                assert!(stamp_holds(&run.run), "{what}");
            }
            clear_run_dir(&run.run, false, false, &run.tag, past_the_hang(made));
            assert_eq!(copy.is_file(), kept, "{what}");
            assert_eq!(run.run.join("env.sh").is_file(), kept, "{what}");
            if kept {
                let stamp = dir.join(aterm_uds::deadline::EXEC_STAMP);
                let restart = u64::MAX.to_string();
                fs::write(&stamp, &restart).unwrap();
                assert!(stamp_holds(&run.run), "{what}");
                assert_eq!(fs::read_to_string(&stamp).unwrap(), restart, "{what}");
                clear_run_dir(&run.run, false, false, &run.tag, made);
            }
            assert!(!run.run.exists(), "{what}: the copy went");
        }
    }

    /// A hold or unproven copy whose stamp could not be written (a full disk) keeps its directory
    /// and `env.sh` past any wall time, since its mtime can predate its exec; a later `down` that
    /// can stamp it starts its wait. By its mtime alone each would count as spent.
    #[test]
    fn a_down_that_cannot_stamp_a_hold_keeps_it() {
        use std::os::unix::fs::PermissionsExt as _;
        let root = aterm_tempfile::tempdir().unwrap();
        for (what, held, kept) in [
            ("a scratch hold", "held/agy-1", "scratch"),
            ("the clock's clone", "clock", "clock"),
            ("a twin probe", "twin-src", "twin-src"),
            ("the relay", "relay", "relay"),
            ("the lab's copy", "bin", "bin"),
        ] {
            let (run, made) = torn_down(root.path(), held != "bin", false);
            fs::create_dir_all(&run.w).unwrap();
            let dir = run.run.join(held);
            fs::create_dir_all(&dir).unwrap();
            if held != "held/agy-1" {
                fs::write(dir.join("claude"), "a clone").unwrap();
            }
            let mode = |m| fs::set_permissions(&dir, fs::Permissions::from_mode(m)).unwrap();
            mode(0o500);
            let landed = stamp_holds(&run.run);
            mode(0o700);
            assert!(!landed, "{what}: the stamp was refused");
            let past = past_the_hang(made);
            assert!(copies_may_go(&dir, false, past), "{what}: control");
            clear_run_dir(&run.run, false, true, &run.tag, past);
            let kept = run.run.join(kept);
            assert!(kept.is_dir(), "{what}: kept past any wall time");
            assert!(run.run.join("env.sh").is_file(), "{what}");
            assert!(stamp_holds(&run.run), "{what}: a later down stamps it");
            clear_run_dir(&run.run, false, false, &run.tag, past);
            assert!(kept.is_dir(), "{what}: its wait runs from that stamp");
            let restart = u64::MAX.to_string();
            fs::write(dir.join(aterm_uds::deadline::EXEC_STAMP), restart).unwrap();
            clear_run_dir(&run.run, false, false, &run.tag, made);
            assert!(!run.run.exists(), "{what}: the later down finished");
        }
    }

    /// A blank stamp (a write cut short) takes the awake clock's reading at the next `down`, so it
    /// stays past any wall time and goes on a restart since; one `down` cannot write again counts
    /// as unstamped. By its mtime alone it would count as spent.
    #[test]
    fn a_down_writes_a_blank_stamp_again() {
        use std::os::unix::fs::PermissionsExt as _;
        let root = aterm_tempfile::tempdir().unwrap();
        for (what, held, kept) in [
            ("a scratch hold", "held/agy-1", "scratch"),
            ("the clock's clone", "clock", "clock"),
            ("a twin probe", "twin-src", "twin-src"),
            ("the relay", "relay", "relay"),
            ("the lab's copy", "bin", "bin"),
        ] {
            let (run, made) = torn_down(root.path(), held != "bin", false);
            fs::create_dir_all(&run.w).unwrap();
            let dir = run.run.join(held);
            fs::create_dir_all(&dir).unwrap();
            if held != "held/agy-1" {
                fs::write(dir.join("claude"), "a clone").unwrap();
            }
            let stamp = dir.join(aterm_uds::deadline::EXEC_STAMP);
            fs::write(&stamp, "").unwrap();
            let past = past_the_hang(made);
            assert!(copies_may_go(&dir, false, past), "{what}: control");
            let mode = |m| fs::set_permissions(&stamp, fs::Permissions::from_mode(m)).unwrap();
            mode(0o400);
            let landed = stamp_holds(&run.run);
            mode(0o600);
            assert!(
                !landed,
                "{what}: the blank stamp could not be written again"
            );
            assert!(stamp_holds(&run.run), "{what}");
            assert!(
                aterm_uds::deadline::exec_stamp(&dir).is_some(),
                "{what}: written again"
            );
            clear_run_dir(&run.run, false, false, &run.tag, past);
            assert!(
                run.run.join(kept).is_dir(),
                "{what}: kept past any wall time"
            );
            assert!(run.run.join("env.sh").is_file(), "{what}");
            // a restart since the stamp
            fs::write(&stamp, u64::MAX.to_string()).unwrap();
            clear_run_dir(&run.run, false, false, &run.tag, made);
            assert!(!run.run.exists(), "{what}: the later down finished");
        }
    }

    #[test]
    fn an_up_waits_out_a_copy_an_earlier_lab_left_unproven() {
        let root = aterm_tempfile::tempdir().unwrap();
        let runs = root.path().join("runs");
        let gone = |run: &Run| !run.run.join("bin").exists() && !run.run.join("relay").exists();
        let (up, made) = torn_down(root.path(), false, false);
        assert!(room_for(&up, past_the_hang(made)).is_err(), "up already");
        assert!(up.run.join(BIN).is_file(), "an up lab's copy stays");
        fs::remove_dir_all(&runs).unwrap();
        for (what, bin_ran, relay_ran, unproven) in [
            ("stub's copy unproven", false, true, BIN),
            ("relay's copy unproven", true, false, "relay/node"),
        ] {
            let (left, made) = torn_down(root.path(), bin_ran, true);
            fs::remove_file(left.run.join("env.sh")).unwrap();
            if relay_ran {
                fs::write(left.run.join(RELAY_RAN), "").unwrap();
            }
            assert!(room_for(&left, made).is_err(), "{what}: the tag waits");
            assert!(left.run.join(unproven).is_file(), "{what}: in place");
            let dir = left.run.join(unproven).parent().unwrap().to_path_buf();
            assert!(
                room_for(&left, past_the_hang(made)).is_err(),
                "{what}: its wait runs on the awake clock from the up that stamped it"
            );
            // a restart since the stamp
            fs::write(
                dir.join(aterm_uds::deadline::EXEC_STAMP),
                u64::MAX.to_string(),
            )
            .unwrap();
            assert!(room_for(&left, made).is_ok(), "{what}");
            assert!(gone(&left), "{what}: the wait spent, the copies went");
            fs::remove_dir_all(&runs).unwrap();
        }
        let (proven, made) = torn_down(root.path(), true, true);
        fs::remove_file(proven.run.join("env.sh")).unwrap();
        fs::write(proven.run.join(RELAY_RAN), "").unwrap();
        assert!(room_for(&proven, made).is_ok(), "both ran: room at once");
        assert!(gone(&proven));
    }

    #[test]
    fn the_relay_is_laid_once_and_its_run_recorded() {
        use std::os::unix::fs::MetadataExt as _;
        let root = aterm_tempfile::tempdir().unwrap();
        let run = half_made(root.path());
        let node = run.run.join("relay/node");
        lay_relay(&run).unwrap();
        let first = fs::metadata(&node).unwrap().ino();
        assert!(!relay_shown_running(&run), "nothing ran yet");
        lay_relay(&run).unwrap();
        assert_eq!(fs::metadata(&node).unwrap().ino(), first, "kept");
        assert!(!run.run.join(RELAY_RAN).exists(), "no log, no record");
        fs::write(run.w.join("relay.log"), "").unwrap();
        assert!(relay_shown_running(&run));
        lay_relay(&run).unwrap();
        assert!(run.run.join(RELAY_RAN).exists(), "recorded");
        assert!(!run.w.join("relay.log").exists(), "its log went");
        assert_eq!(fs::metadata(&node).unwrap().ino(), first, "still kept");
        assert!(relay_shown_running(&run), "the record stands");
    }
}
