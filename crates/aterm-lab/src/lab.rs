// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A lab's life: `up` (the stub, the black-hole proxy and the jailed private
//! headless aterm, each in a session of its own with no controlling terminal),
//! `down` (everything ended by pid, the scratch removed, the logs kept), `ps`,
//! and launching the managed Claude Code in the lab's first tab.

use std::fs;
use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};
use std::time::Duration;

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

/// The FAKE claude.ai OAuth token of `claude TAG --subscriber`: no credential
/// anywhere; it goes only where `ANTHROPIC_BASE_URL` points (the stub), and
/// the jail keeps every other connect on loopback.
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
    /// `LAB_FIRST_RUN=1`: leave the one-time first-run notice (and its
    /// default-mode nudge) on the screen.
    pub first_run: bool,
    /// `LAB_RESTORE=1`: the headless-restore seam.
    pub restore: bool,
    /// `LAB_HARNESS_KEYS`: extra `[harness]` lines.
    pub harness_keys: String,
    /// `LAB_ATERM_TOML`: written at the top of the scratch `aterm.toml`.
    pub aterm_toml: String,
    /// `LAB_COLS` (144).
    pub cols: String,
    /// `LAB_LINES` (50).
    pub lines: String,
    /// `LAB_SCALE`: the instance's `--scale`.
    pub scale: Option<String>,
    /// `LAB_PROXY=dead`: the proxy at the dead port 9 instead of the black hole.
    pub proxy_dead: bool,
    /// The lab is the verb's own: its instance and stub hold a lifeline to this
    /// process and end with it, however it ends (a lab `up` makes outlives it).
    pub lifeline: bool,
    /// `LAB_INSTANCE_ENV='K=V K=V'`: variables the lab's instance gets at each
    /// launch of it ([`instance_env`] says which).
    pub instance_env: String,
    /// `LAB_CLOCK=1`: the lab's sleep clock loaded into its instance and its
    /// Claude Code ([`crate::clock`]), which `sleep` steps.
    pub clock: bool,
    /// The lab's instance OPENS A WINDOW on this desk, in the same jail
    /// ([`windowed`]): a verb's own choice (`forever --window`), never the
    /// environment's.
    pub window: bool,
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
        }
    }
}

/// The managed Claude Code, launched by path (not through its shim): the last
/// path of the arrow chain `aterm pkg which claude` prints under a clean
/// environment (`claude → <shim> → <store binary> — managed <ver> — …`,
/// [`pkg_which`]), or `$LAB_CLAUDE`.
///
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
/// ([`crate::paths::Paths::store_home`], the account's whatever `HOME` says),
/// as `aterm` reports it ([`pkg_which`]).
///
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

/// The store binary of a `pkg which claude` line: the path after its last `→`.
#[must_use]
pub fn managed_claude(line: &str) -> Option<String> {
    managed_bin("claude", line)
}

/// The store binary of a `pkg which <name>` line: the path after its last `→`.
/// A package linked from a checkout (`aterm pkg link`: `— dev-linked (…)`,
/// as the owner links codex-alab) is managed the same way, its binary the
/// last path too.
#[must_use]
pub fn managed_bin(name: &str, line: &str) -> Option<String> {
    let rest = line.strip_prefix(&format!("{name} → "))?;
    let (chain, _) = rest
        .split_once(" — managed ")
        .or_else(|| rest.split_once(" — dev-linked "))?;
    let last = chain.rsplit(" → ").next()?;
    (!last.is_empty()).then(|| last.to_string())
}

/// The store binary of the managed `name`: the last path of `aterm pkg which
/// <name>`'s arrow chain under a clean environment (not the atpkg shim,
/// [`pkg_which`]).
pub(crate) fn managed_store_bin(name: &str) -> Option<String> {
    managed_store_bin_in(paths(), paths().need_aterm().ok()?, name)
}

/// [`managed_store_bin`] in the package store the lab's facts `p` name
/// ([`crate::paths::Paths::store_home`]), as `aterm` reports it.
fn managed_store_bin_in(p: &crate::paths::Paths, aterm: &Path, name: &str) -> Option<String> {
    managed_bin(name, &pkg_which(aterm, p.store_home(), name).ok()?)
}

/// The first line `aterm pkg which <name>` prints, run under a clean
/// environment whose `HOME` is `home`: the home whose package store holds the
/// managed agents ([`crate::paths::Paths::store_home`], the account's), never
/// the caller's `HOME`, which under a scratch `HOME` holds no store and would
/// leave every agent to a `LAB_*` variable set by hand.
///
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
    if c.is_empty() || !crate::sys::executable(&c) {
        return None;
    }
    Some(fs::canonicalize(&c).map_or(c, |p| p.to_string_lossy().into_owned()))
}

/// The managed Codex, the same way: the last path of `aterm pkg which codex`'s
/// arrow chain (the store binary, not the atpkg shim), or `$LAB_CODEX`. A lab
/// without one runs Claude Code only (`up` says so); `codex` and `codex-run` need it.
#[must_use]
pub fn find_codex() -> Option<String> {
    let c = match std::env::var("LAB_CODEX") {
        Ok(c) if !c.is_empty() => c,
        _ => managed_store_bin("codex")?,
    };
    executable_at(c)
}

/// The Gemini CLI under test: `$LAB_GEMINI`, else Homebrew's
/// `/opt/homebrew/bin/gemini`, by its physical path (the bundle's script, which
/// runs Homebrew's node by its shebang). `None` when neither is there; `gemini`
/// and `gemini-captures` need one.
#[must_use]
pub fn find_gemini() -> Option<String> {
    let c = std::env::var("LAB_GEMINI")
        .ok()
        .filter(|c| !c.is_empty())
        .map_or_else(
            || "/opt/homebrew/bin/gemini".to_string(),
            |c| crate::paths::from_caller(&c),
        );
    executable_at(c)
}

/// The Antigravity CLI under test: `$LAB_AGY`, else the managed store's
/// `agy` (the last path of `aterm pkg which agy`'s arrow chain). `None` when
/// neither is there; `agy` and `agy-captures` need one.
#[must_use]
pub fn find_agy() -> Option<String> {
    let c = match std::env::var("LAB_AGY") {
        Ok(c) if !c.is_empty() => crate::paths::from_caller(&c),
        _ => managed_store_bin("agy")?,
    };
    executable_at(c)
}

/// The Muse Code under test: `$LAB_MUSE`, else the managed store's `muse`
/// (the last path of `aterm pkg which muse`'s arrow chain), by its physical
/// path (a store's `current` link resolved). `None` when neither is there;
/// `muse` and `muse-captures` need one.
#[must_use]
pub fn find_muse() -> Option<String> {
    let c = match std::env::var("LAB_MUSE") {
        Ok(c) if !c.is_empty() => crate::paths::from_caller(&c),
        _ => managed_store_bin("muse")?,
    };
    executable_at(c)
}

/// `text` when it is a version (a digit, then letters, digits, `.`, `+` and
/// `-`), else `?`: a version names files, and what printed it ran in the jail.
#[must_use]
pub fn clean_ver(text: &str) -> String {
    let ok = text.chars().next().is_some_and(|c| c.is_ascii_digit())
        && text
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || ".+-".contains(c));
    if ok { text.to_string() } else { "?".into() }
}

/// Gemini CLI's version: the `version` of the `@google/gemini-cli` package
/// its script sits in (read through no link), else `--version`'s last word,
/// inside the jail.
#[must_use]
pub fn gemini_version(run: &Run, bin: &str) -> String {
    let v = Path::new(bin)
        .ancestors()
        .skip(1)
        .take(4)
        .filter_map(|d| {
            crate::sys::scratch_read(&d.join("package.json"))
                .ok()
                .flatten()
        })
        .filter_map(|b| J::parse_bytes(&b).ok())
        .find(|j| j.get("name").and_then(J::as_str) == Some("@google/gemini-cli"))
        .and_then(|j| j.get("version").and_then(J::as_str).map(str::to_string))
        .unwrap_or_default();
    if v.is_empty() {
        seeded_version(run, crate::gemini::Agent::Gemini, bin)
    } else {
        clean_ver(&v)
    }
}

/// agy's version: the `version = "…"` of the record atpkg keeps beside its
/// store build (`<build>.vendor`, read through no link), else `--version`'s
/// last word, inside the jail.
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
        seeded_version(run, crate::gemini::Agent::Agy, bin)
    } else {
        clean_ver(&v)
    }
}

/// `--version`'s last word of Gemini CLI or agy, run in the jail with the
/// agent's own scratch HOME seeded first ([`crate::gemini::seed`]): agy
/// without its provider seed signs in with a Google account in a browser, so
/// neither runs before its seed, `--version` included (`?` when the seed
/// could not be written).
fn seeded_version(run: &Run, agent: crate::gemini::Agent, bin: &str) -> String {
    if crate::gemini::seed(run, agent, &crate::gemini::Launch::default()).is_err() {
        return "?".into();
    }
    jailed_version_in(run, bin, &run.w.join(agent.home()), |l| {
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

/// The build Muse Code's `--version` names: `Muse Code 1.4.10 (1.4.10-R6427.1)`
/// → `1.4.10-R6427.1`, the text in its parentheses (the name its fixtures carry).
#[must_use]
pub fn muse_build(version_line: &str) -> Option<&str> {
    let (_, rest) = version_line.split_once('(')?;
    rest.split_once(')').map(|(b, _)| b.trim())
}

/// Muse Code's version: the `version = "…"` of the record atpkg keeps beside
/// its store build (`<build>.vendor`, read through no link), else the build
/// `muse --version` names ([`muse_build`]), inside the jail.
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

/// `<bin> --version` run INSIDE the lab's jail, where the witness watches it,
/// from the scratch with the scratch HOME (the lab runs no agent build
/// unjailed, whatever wrote it): the word `pick` chooses of its first line.
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

/// The Codex version atpkg's package file beside the build names
/// (`<build>/codex-package.json`, read through no link), else `codex
/// --version`'s last word, inside the jail.
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

/// The lab's own program, copied into the run directory at `up`
/// (`bin/aterm-lab`): the egress witness and the stub start from it, the
/// shim's `security` and `open` lead to it, and the relay is a copy of it.
const BIN: &str = "bin/aterm-lab";

/// Written beside [`BIN`] once a program started from it showed it ran: the
/// stub's ready byte, which code running from that file writes. Until then
/// macOS may still be assessing the copy's first exec ([`copies_may_go`]).
const BIN_RAN: &str = "bin/ran";

/// Written in `relay/` once the relay copied there showed it ran (it made
/// its log, `relay.log`, which each launch behind it removes first).
const RELAY_RAN: &str = "relay/ran";

/// Whether a directory of copies of the lab's program may be removed: one
/// started from it showed it ran (`ran`), or the directory is older than the
/// first-exec hang at `now` ([`aterm_uds::deadline::FIRST_EXEC_HANG`]), so no
/// assessment of a copy in it is still going. A copy is a new file macOS
/// assesses at its first exec, after the exec, while the process waits at
/// `_dyld_start`; killing the process does not end it, and removing the file
/// before it ends crashes `syspolicyd`, which strands every launch on the
/// host. A directory that is not there may go.
fn copies_may_go(dir: &Path, ran: bool, now: std::time::SystemTime) -> bool {
    let Ok(made) = fs::symlink_metadata(dir).and_then(|m| m.modified()) else {
        return true;
    };
    ran || now
        .duration_since(made)
        .is_ok_and(|age| age >= aterm_uds::deadline::FIRST_EXEC_HANG)
}

/// The words for a directory of copies kept because none was shown to run:
/// `{LAB} <verb> <tag>` removes it once it may go.
fn kept_copies(dir: &Path, verb: &str, tag: &str) -> String {
    format!(
        "KEPT {}: the lab's program copied there was started and never showed it ran, so macOS \
         may still be assessing its first exec; {LAB} {verb} {tag} removes it once {} minutes \
         have passed",
        dir.display(),
        aterm_uds::deadline::FIRST_EXEC_HANG.as_secs() / 60
    )
}

/// Remove the directories of copies of the lab's program in `run_dir`
/// (`relay/`, `bin/`) that may go at `now` ([`copies_may_go`]); the ones
/// kept. `relay_ran`: the relay copied there showed it ran
/// ([`relay_shown_running`]).
fn sweep_copies(run_dir: &Path, relay_ran: bool, now: std::time::SystemTime) -> Vec<PathBuf> {
    let mut kept = Vec::new();
    for (d, ran) in [
        ("relay", relay_ran),
        ("bin", run_dir.join(BIN_RAN).exists()),
    ] {
        let dir = run_dir.join(d);
        if copies_may_go(&dir, ran, now) {
            let _ = fs::remove_dir_all(&dir);
        } else {
            kept.push(dir);
        }
    }
    kept
}

/// The end of `down`, once its scratch is gone: the copies that may go
/// ([`sweep_copies`]), the run directory's own files, then the directory. A
/// run directory that keeps a copy keeps its `env.sh` with it, so it is still
/// a lab: `ps` lists it, `up` refuses it, and a later `down` finishes the job
/// once the copy may go.
fn clear_run_dir(run_dir: &Path, relay_ran: bool, tag: &str, now: std::time::SystemTime) {
    let kept = sweep_copies(run_dir, relay_ran, now);
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
    let _ = fs::remove_dir(run_dir);
}

/// Whether `up` started a program from the lab's copy of its own program
/// ([`BIN`]: the witness or the stub) that has not shown it ran
/// ([`BIN_RAN`]).
fn bin_unproven(run: &Run) -> bool {
    (run.sampler_pid > 0 || run.stub_pid > 0) && !run.run.join(BIN_RAN).exists()
}

/// Undo a half-made lab (its pids, everything in its jail, its socket dir and
/// run directory); the failure to return. A lab that started a program from
/// its copy of the lab's program ([`BIN`]) which never showed it ran keeps its
/// run directory, `env.sh` and all, its processes SIGKILLed (a SIGTERM does
/// not end one still waiting at `_dyld_start`): `down` removes it once the
/// copy may go ([`copies_may_go`]).
fn abort(run: &Run, pids: &[i32], msg: &str) -> Fail {
    let unproven = bin_unproven(run);
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
    if unproven {
        eprintln!("aterm-lab: {}", kept_copies(&run.run, "down", &run.tag));
    } else {
        let _ = fs::remove_dir_all(&run.run);
    }
    Fail::die(msg)
}

/// A lifeline for a child of a lab this verb owns: the far end of a FIFO in
/// the run directory, which jailed code cannot write, to be its stdin with
/// [`lifeline_flag`] (`None` when not `armed`). This process holds the write
/// end until it ends, however it ends: the kernel then closes it and the
/// child reads end of file and quits. One FIFO per child: macOS hands a
/// FIFO's end of file to one reader.
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

/// How long a jailed child the lab started may take to say it is ready (its
/// `--ready-fd` byte): a hang detector, never a latency budget. The stub, the
/// first exec of the lab's fresh copy of its own program, waits
/// [`aterm_uds::deadline::FIRST_EXEC_HANG`] instead.
const READY_HANG: Duration = Duration::from_secs(60);

/// `sandbox-exec -f <jail> env -i <vars> <program…>` as a std `Command`, in a
/// session of its own, started in the scratch, its output appended to `out`,
/// every descriptor above stdio stripped first (`InheritNothing`): the jailed
/// launch [`launch_ready`] and [`launch_window`] spawn.
///
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

/// A WINDOWED instance launched in the jail ([`jailed_command`]): a window
/// takes no ready signal (`--ready-fd` is a headless launch's, refused for a
/// window a person sees come up), so it is ready once its control socket
/// answers `version` through the lab's door, asked every 250 ms up to `hang`
/// (the hang detector). `pid` is set to the child's as soon as it is spawned,
/// so a caller's cleanup ends it.
///
/// # Errors
/// The launch failed, the child ended before its socket answered, it did not
/// answer within `hang`, or the run was interrupted (`interrupted`).
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
    until_answered(
        || door.ctl(&["version"]).is_ok_and(|r| r.code == 0),
        || alive(child),
        hang,
        || stop::sleep(Duration::from_millis(250)).map_err(|_| "interrupted".to_string()),
    )
}

/// A window's readiness ([`launch_window`]): `answers` (its control socket
/// answered `version`) is asked first, then `alive` (the child still runs),
/// then the hang detector `hang`; between the asks, `pause` (250 ms, which
/// fails `interrupted` on an interrupt).
///
/// # Errors
/// The child ended before an answer, nothing answered within `hang`, or
/// `pause` failed.
fn until_answered(
    mut answers: impl FnMut() -> bool,
    mut alive: impl FnMut() -> bool,
    hang: Duration,
    mut pause: impl FnMut() -> Result<(), String>,
) -> Result<(), String> {
    let end = std::time::Instant::now() + hang;
    loop {
        if answers() {
            return Ok(());
        }
        if !alive() {
            return Err("the window's instance ended before its control socket answered".into());
        }
        if std::time::Instant::now() >= end {
            return Err("its control socket did not answer within the hang detector".into());
        }
        pause()?;
    }
}

/// Why `o` cannot bring a lab up, before anything is made: a windowed lab
/// (`forever --window`) under `LAB_RESTORE=1`, since the headless-restore
/// seam is a headless instance's. `None`: nothing in `o` conflicts.
#[must_use]
pub(crate) fn opts_refused(o: &UpOpts) -> Option<&'static str> {
    (o.window && o.restore).then_some(
        "a windowed lab takes no LAB_RESTORE=1: the headless-restore seam is a headless instance's",
    )
}

/// `sandbox-exec -f <jail> env -i <vars> <program…>`, in a session of its
/// own, started in the scratch, its output appended to `out` — its `program`
/// handed a READY SIGNAL
/// (`aterm_agent::ready`: `--ready-fd <n>` appended, a FIFO in the run
/// directory, which the jail cannot write), spawned and kept
/// ([`procs::keep_forked`]), and waited on: its pid once the child wrote its
/// byte. A std `Command`, the one spawn here that is not the seam's
/// (`spawnfd::command` places descriptors at 0-2 alone): it strips every
/// descriptor first (`InheritNothing`), then hands the one, and starts the
/// child in a session of its own.
///
/// # Errors
/// The launch failed, or the child closed its signal with nothing said (it
/// exited, or it has nothing to offer), said nothing within `hang` (the hang
/// detector: [`READY_HANG`], or the first-exec hang for a fresh copy), or the
/// run was interrupted (`interrupted`). `pid` is set to the child's as soon
/// as it is spawned, so a caller's cleanup ends it.
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
    // The wait on its own thread, which rings the lab's bell when it ends: an
    // interrupt ends this wait as it ends every wait of the lab.
    let said = std::sync::Arc::new(std::sync::Mutex::new(None));
    let slot = std::sync::Arc::clone(&said);
    std::thread::spawn(move || {
        let got = ready.wait(hang);
        *slot
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(got);
        stop::ring();
    });
    stop::wait_until(hang + Duration::from_secs(1), || {
        said.lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .is_some()
    })
    .map_err(|_| "interrupted".to_string())?;
    let got = said
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .take();
    match got {
        Some(aterm_agent::ready::Signal::Ready) => Ok(()),
        Some(aterm_agent::ready::Signal::Closed) => Err(format!(
            "{} closed its ready signal with nothing said (it exited, or has nothing to offer)",
            program.first().map_or("the child", String::as_str)
        )),
        Some(aterm_agent::ready::Signal::Failed(e)) => Err(format!("the ready signal: {e}")),
        Some(aterm_agent::ready::Signal::TimedOut) | None => {
            Err("said nothing within the hang detector".to_string())
        }
    }
}

/// `path` opened for appending, twice: a child's stdout and stderr.
fn append_to_std(path: &Path) -> std::io::Result<(std::process::Stdio, std::process::Stdio)> {
    let f = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)?;
    let g = f.try_clone()?;
    Ok((std::process::Stdio::from(f), std::process::Stdio::from(g)))
}

/// What the lab itself sets in its instance's environment (`env -i` drops
/// everything else): the shim's PATH, the jail's HOME, XDG dirs and state
/// root, the open recorder as `BROWSER` with its log, the headless-restore
/// seam when the lab runs under it (`SEAM=1`), the harness tape, and the
/// sleep clock when the lab is up with it (`CLOCK=1`: [`crate::clock::vars`]).
///
/// The recorder rides the instance's environment for the sessions it
/// spawns itself (a worker's Claude Code, `spawn agent=claude`): their login
/// shell rebuilds PATH with the shim LAST (macOS's `path_helper`), so a
/// bare-name `open` there is `/usr/bin/open`, which the jail refuses and
/// nothing logs; `BROWSER` passes that shell untouched, and Claude Code opens
/// a URL as `$BROWSER <url>`.
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
    // Every hosted loop the instance runs is recorded to a harness tape in its
    // state root (`crate::tape`): `down` keeps them with the logs, a scenario
    // run copies them into its results, the selftest replays them.
    vars.push(crate::tape::seam());
    vars.extend(crate::clock::vars(run));
    vars
}

/// Whether the lab sets `name` for its instance itself, under any of its
/// switches ([`lab_vars`] of a lab under the headless-restore seam): a name
/// `LAB_INSTANCE_ENV` never hands it.
fn lab_sets(name: &str) -> bool {
    let probe = Run {
        seam: "1".into(),
        clock: "1".into(),
        ..Run::default()
    };
    lab_vars(&probe, "").iter().any(|(k, _)| k == name)
}

/// `LAB_INSTANCE_ENV` read: `NAME=VALUE` words, each a variable the lab's
/// OWN instance gets at every launch of it (`up`'s, `restore`'s and `crash`'s
/// relaunches), after what the lab sets ([`lab_vars`]); nothing else the lab
/// starts gets them. Two kinds of name only: `TMPDIR` (an absolute path: with
/// a PTY keeper run outside the jail, `TMPDIR=/tmp` puts the keeper's socket
/// in the lab's socket dir, where both find it, `aterm_keeper::job::keeper_dir`)
/// and a dev seam `ATERM_DEBUG_<NAME>` or `ATERM_DEV_<NAME>` (a person's Paste
/// fed from a lab file, `ATERM_DEBUG_PASTE_FILE`; the card engine's `card-seen`
/// probe, `ATERM_DEV_CARD_PROBE`; …) — and never one the lab sets itself
/// ([`lab_sets`]: `ATERM_DEBUG_HEADLESS_RESTORE` is `LAB_RESTORE=1`'s). A
/// value has no space and no control character.
///
/// # Errors
/// A word that is no `NAME=VALUE`, a name outside those two kinds or one the
/// lab sets, a name given twice, an empty value or one with a control
/// character, or a `TMPDIR` that is not absolute.
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
        if !(name == "TMPDIR" || seam) || lab_sets(name) {
            return refuse(format!(
                "{name} is not a variable the lab hands its instance (TMPDIR, or an ATERM_DEBUG_* or ATERM_DEV_* seam the lab does not set itself)"
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

/// The instance's environment at every launch: what the lab sets
/// ([`lab_vars`]), then what `up` recorded of `LAB_INSTANCE_ENV`
/// ([`instance_env`], read off the record, never the caller's environment).
///
/// # Errors
/// The record holds what `instance_env` refuses (an `env.sh` edited by hand).
pub(crate) fn instance_vars(run: &Run, user: &str) -> Result<Vec<(String, String)>, String> {
    let mut vars = lab_vars(run, user);
    vars.extend(instance_env(&run.instance_env)?);
    Ok(vars)
}

/// Whether the lab's instance opens a window (`WINDOW=1`, `forever
/// --window`): the windowed twin of the headless instance, for what only a
/// window carries (`hwkey`, the OS event queue a key press takes).
#[must_use]
pub(crate) fn windowed(run: &Run) -> bool {
    run.window == "1"
}

/// The instance's command line: `aterm --headless`, the lifeline's flag when
/// it holds one, then the lab's own socket, project dir and grid (and
/// `--scale`): the same at `up` and at each relaunch on the same record. A
/// WINDOWED lab's ([`windowed`]) has no `--headless` and no lifeline flag (a
/// window refuses `--lifeline-fd`: it is a person's to close): where it holds
/// a lifeline, the lab's keeper ([`crate::instance::keeper_main`], the lab's
/// own copy of its program) leads the line, holds it on its stdin and ends
/// the window by pid when it is cut.
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
    prog.extend(
        [
            "--no-reroute",
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

/// The private headless instance (jailed): scratch HOME, XDG and state, the
/// harness at its defaults ([`instance_vars`], [`instance_argv`]). `up`
/// starts it; `restore` and `crash` start it again on the same state and
/// socket. With the seam (`SEAM=1`) the instance keeps its desk as a window
/// does. A windowed lab's ([`windowed`]) opens a window, in the same jail
/// ([`launch_window`]). Sets the instance pid (a keeper's, where one holds a
/// window's lifeline); fails when no socket came up.
pub fn start_instance(run: &mut Run, scale: Option<&str>, armed: bool) -> Result<(), String> {
    let aterm = run.need_aterm()?.to_string_lossy().into_owned();
    let vars = instance_vars(run, &paths().user)?;
    let stdin = lifeline_end(run, armed).map_err(|e| format!("the lifeline: {e}"))?;
    let prog = instance_argv(run, &aterm, scale, stdin.is_some());
    // NO LIFELINE unless the lab is the verb's own: a lab `up` makes outlives the verb, until `down` ends it by pid.
    // Ready is the instance's own word that its control socket listens (`--ready-fd`), never a poll of the socket;
    // a window takes no such word, so its socket's first answer is its ready.
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
    // the jailed tree must have no controlling terminal: the caller's would let it type into it
    if procs::tty_of(run.instance_pid) != "??" {
        return Err(format!(
            "instance {} kept a controlling terminal",
            run.instance_pid
        ));
    }
    Ok(())
}

/// Whether `pid` is a live process of the lab `run`: its command line names
/// the lab's run directory or its socket directory (each with its `/`). How
/// `down` and `crash` tell a pid `env.sh` records from one the system has
/// given to another process since: neither signals a pid this says is not
/// the lab's.
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

/// Whether `up` may make the lab `run` names at `now`: no lab of its tag is
/// up (no `env.sh`), and no copy of the lab's program an earlier lab of the
/// tag left is still unproven. Such copies (an aterm-lab before this one's
/// `down` dropped `env.sh` while keeping them) go once they may
/// ([`sweep_copies`]); until then the tag waits. So `up` never lays its copy
/// over a file whose first exec macOS may still be assessing, and never
/// hands the new lab an older lab's relay.
///
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
    match sweep_copies(&run.run, relay_shown_running(run), now).first() {
        Some(dir) => Err(Fail::die(format!(
            "lab '{tag}' cannot come up yet: {}",
            kept_copies(dir, "up", tag)
        ))),
        None => Ok(()),
    }
}

/// Bring a lab up (nothing printed); the record of it.
///
/// # Errors
/// Why it could not come up (whatever it had started is taken down first).
#[allow(clippy::too_many_lines)]
pub fn up(tag: &str, o: &UpOpts) -> Result<Run, Fail> {
    // options that conflict are refused first, before anything is made or caught
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
    // what the instance is handed beyond the lab's own, checked before anything is made and recorded, so each
    // relaunch hands it the same (never what a later caller's environment says)
    run.instance_env = instance_env(&o.instance_env)
        .map_err(Fail::usage)?
        .iter()
        .map(|(k, v)| format!("{k}={v}"))
        .collect::<Vec<_>>()
        .join(" ");
    let aterm = p.need_aterm()?.to_path_buf();
    run.aterm = aterm.to_string_lossy().into_owned();
    need_sandbox()?;
    // the jail hides the owner's data from the lab, so neither the lab's program nor the aterm under test may live in it:
    // the data of every home it guards (the account's, and HOME's when it names another)
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
    // the Codex, Gemini CLI, agy and Muse Code under test, when there is one: in aterm's package store (the jail re-allows its
    // build), or anywhere outside the owner's data
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
    if let Some(c) = find_gemini() {
        run.gemini_bin = outside("Gemini CLI", c)?;
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
    for d in crate::protected::SCRATCH_DIRS {
        fs::create_dir_all(w.join(d))
            .map_err(|e| Fail::die(format!("cannot make {}: {e}", w.join(d).display())))?;
    }
    // The sleep clock, before the jail is rendered: the jail names the Claude Code the lab launches, the clock's
    // ad hoc signed clone of it when its runtime is hardened.
    if o.clock {
        crate::clock::lay(&mut run, &p.repo)
            .map_err(|e| abort(&run, &[], &format!("the sleep clock: {e}")))?;
    }
    // The DECOY ~/.codex of the scratch HOME: Codex must keep to CODEX_HOME, and one that fell back to $HOME/.codex
    // would write here (codex-selftest checks it stayed as made); the owner's own ~/.codex is denied by the jail.
    let _ = fs::write(
        w.join("home/.codex/LAB-DECOY"),
        format!(
            "lab decoy: Codex runs with CODEX_HOME={}; nothing may be written here\n",
            w.join("codex-home").display()
        ),
    );
    // The same for Gemini CLI and agy, each run with a HOME of its own (gemini-selftest and agy-selftest check it).
    let _ = fs::write(
        w.join(crate::gemini_proof::DECOY),
        "lab decoy: Gemini CLI and agy run with HOME=gemini-home or agy-home; nothing may be written here\n",
    );
    run.sockdir = crate::sys::make_temp_dir("/tmp/at.XXXX")
        .map_err(|e| abort(&run, &[], &format!("cannot make a socket dir: {e}")))?;
    run.sock = format!("{}/a.sock", run.sockdir);
    run.proj = format!("{}/proj", run.sockdir);
    let _ = fs::create_dir_all(&run.proj);
    // The lab's own program, copied where jailed code cannot write: the stub, the egress witness, the keychain
    // tripwire, the open recorder and the relay run this copy. Beside its roles in the shim, `aterm` is the aterm
    // under test: the shim leads the launched agent's PATH, so its `aterm ctl …` (a manager starting workers)
    // runs the build the lab tests, never an installed release, and reaches this lab's instance. A session the
    // instance spawns (a worker) gets the PATH its login shell rebuilds, the shim LAST: there an `aterm` earlier
    // on that PATH would answer, and its opens reach the recorder by the BROWSER the instance hands on ([`lab_vars`]).
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
    spec.gemini.clone_from(&run.gemini_bin);
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
    // from here `ps` and `down` know this lab, whatever happens next
    run.save();
    // ---- the egress witness (outside the jail), before anything is jailed: it reads env.sh at every pass and lives
    // until `down` has emptied the jail, so no jailed process of this lab runs a moment without it. A lab this verb
    // owns hands it the verb's lifeline too: a verb SIGKILLed ends its jailed children by theirs, and the witness,
    // its lifeline cut, watches until the jail is empty and then ends (a lab `up` makes has none: `down` ends it).
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

    // ---- the stub (jailed): API port and black-hole proxy port are picked by the kernel
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
    // Ready is the stub's own word, written once both its port files are (`--ready-fd`). The stub is the first
    // exec of the lab's fresh copy of its program (the witness's started just before it), so it waits the
    // first-exec hang, and its word is the copy's proof that it ran: until then an abort keeps the copy.
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

    // ---- scratch config: the fake key helper, Claude's settings and first-run state, aterm.toml, zshrc
    let helper = run.run.join("apikey.sh");
    let script = format!(
        "#!/bin/sh\n# the lab's fake API key (the stub accepts anything); nothing real\nprintf \"%s\\n\" \"{}\"\n",
        api_key()
    );
    let _ = fs::write(&helper, script)
        .and_then(|()| fs::set_permissions(&helper, fs::Permissions::from_mode(0o755)));
    // the settings witness's ConfigChange hook (scratch_claude_config names it)
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
    // the tab shells' startup file, a person's profile: the lab's Gemini CLI on PATH with its fake key and the stub
    put_tab_shell(&w, &run.run, &run.gemini_bin, &run.port);

    // ---- the private headless instance (jailed)
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
    // The instance must have taken the private state root. ATERM_STATE_HOME is a dev seam (a debug build has
    // it, an optimized build needs --features dev-seams): without it the instance reaches for the owner's live
    // state, which the jail denies it.
    // Its logger opens before its socket binds, so once it said it listens the folder is there or never will be.
    let logs = w.join("state/logs");
    if !logs.is_dir() {
        let _ = down(&run.tag, false);
        return Err(Fail::die(format!(
            "the instance did not take ATERM_STATE_HOME ({} was built without the dev seams): targo --unverified build -p aterm --features dev-seams",
            aterm.display()
        )));
    }
    // The agents' versions, read inside the jail and where the witness watches: the lab runs neither build anywhere
    // else, so neither runs unjailed, whatever wrote it. Codex's store package names its version.
    run.claude_ver = jailed_version(&run, &run.claude_bin, |l| l.split_whitespace().next());
    if !run.codex_bin.is_empty() {
        run.codex_ver = codex_version(&run, &run.codex_bin);
    }
    if !run.gemini_bin.is_empty() {
        run.gemini_ver = gemini_version(&run, &run.gemini_bin);
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

/// A lab's scratch `aterm.toml`: `LAB_ATERM_TOML` (`aterm_toml`) leads, so
/// its root keys (`font_family`, `font_px`) stay at the root, ahead of every
/// table; the update and package lanes off; `LAB_HARNESS_KEYS`
/// (`harness_keys`) under `[harness]`; and the audible bell off unless
/// `LAB_ATERM_TOML` sets `bell_sound` ([`crate::instance::bell_off`]).
fn scratch_aterm_toml(aterm_toml: &str, harness_keys: &str) -> String {
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
        // at the first trust Claude Code `git clone`s the official plugin marketplace, over ssh first (a connect
        // to :22 that HTTPS_PROXY cannot catch; the jail does): off, it is noise in every egress log
        (
            "CLAUDE_CODE_DISABLE_OFFICIAL_MARKETPLACE_AUTOINSTALL",
            J::s("1"),
        ),
    ]);
    if o.survey {
        env.set("CLAUDE_FORCE_DISPLAY_SURVEY", J::s("1"));
    }
    // the settings witness: Claude Code reports each re-read of a settings file it detected
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
    // default: the "Auto mode is now the default" first-run notice is already seen, and so is its nudge
    // ("Make auto mode your default permission mode?"), which a settings `defaultMode` other than auto draws
    // at launch (2.1.301: answered, Claude Code writes `hasSeenAutoDefaultNudge`)
    if !o.first_run {
        cj.set("hasSeenAutoDefaultNotice", J::Bool(true));
        cj.set("hasSeenAutoDefaultNudge", J::Bool(true));
    }
    // default: leave the folder-trust dialog to the harness (the real flow)
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

/// `up [tag]`.
pub fn cmd_up(args: &[String]) -> Res {
    let tag = args
        .first()
        .cloned()
        .unwrap_or_else(crate::run::default_tag);
    let o = UpOpts::from_env();
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
            "gemini",
            &run.gemini_bin,
            &run.gemini_ver,
            "LAB_GEMINI names one; aterm installs none",
        ),
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

/// Take a lab down: everything in the lab's jail ended (found by its mark, an
/// orphan too, and only a pid that is still OURS: a process in this jail, or
/// one whose command line names this lab), the witness last, once the jail is
/// empty; the scratch removed, the logs kept. The kept directory.
///
/// # Errors
/// No such lab, or an `env.sh` whose scratch is not its own.
pub fn down(tag: &str, keep_dumps: bool) -> Result<PathBuf, Fail> {
    let mut r = Run::load(tag)?;
    if r.w != r.run.join("scratch") {
        return Err(Fail::die(format!(
            "env.sh of lab '{tag}' names a scratch dir that is not {}/scratch ({}): not touching it",
            r.run.display(),
            r.w.display()
        )));
    }
    // A stale env.sh whose pids were reused by something else must never kill that something: the ppid walk
    // beside the mark starts only from an instance whose command line names this lab.
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
    // everything in the jail (the instance's tree, codex-runs, an orphan any of them left) but the stub, then the
    // stub, and the witness last, once the jail is empty: it watches each jailed process until it is gone
    let pids = procs::jail_members(&r);
    // what they hold in a shared runtime directory, asked once more before they end (the witness asks each second)
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
    // what is in the jail now, asked afresh (a straggler, a child forked while the others ended), gets KILL
    for p in procs::jail_members(&r) {
        if alive(p) {
            eprintln!("aterm-lab: pid {p} ignored TERM -> KILL");
            let _ = crate::sys::kill(p, libc::SIGKILL);
        }
    }
    // nothing of the jail may outlive this point: the copies below read what it wrote
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
    if alive(r.sampler_pid) {
        let _ = crate::sys::kill(r.sampler_pid, libc::SIGTERM);
        let end = std::time::Instant::now() + Duration::from_secs(2);
        while alive(r.sampler_pid) && std::time::Instant::now() < end {
            std::thread::sleep(Duration::from_millis(50));
        }
        if alive(r.sampler_pid) {
            eprintln!("aterm-lab: pid {} ignored TERM -> KILL", r.sampler_pid);
            let _ = crate::sys::kill(r.sampler_pid, libc::SIGKILL);
        }
    }
    // the jail is empty: what its processes left in a shared runtime directory that none of the variables tried moves
    // (Muse Code's registry in /private/tmp) is taken back, exactly the entries that are this run's and nothing else
    if left.is_empty() {
        let took = crate::runtime_dirs::clean(&r.run);
        if !took.is_empty() {
            eprintln!(
                "aterm-lab: removed what this run left in the shared /tmp: {}",
                took.join(" ")
            );
        }
    }
    // keep the logs (the tripwire's with the account masked), drop the scratch
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
        "gemini.sh",
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
    // the relay's log is its proof that it ran ([`RELAY_RAN`]): read before the scratch goes
    let relay_ran = relay_shown_running(&r);
    // the scratch's files through the one reader that follows no link (the jail may have put one in their place)
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
    // what the stats readers read, so a fact measured with them keeps its evidence: `debuglog`, `statusline`,
    // `claude-json` and `transcript` (Claude Code's transcripts of the run, under claude-projects/)
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
    // Codex's own record of the run (the stub's texts only; the composed auth.json is not kept)
    for d in ["sessions", "log"] {
        keep_tree(
            &r.w.join("codex-home").join(d),
            &keep.join(format!("codex-{d}")),
        );
    }
    // agy's log, and what Gemini CLI wrote into its TMPDIR (its client error reports: the stub's answers)
    keep_tree(
        &r.w.join("agy-home/.gemini/antigravity-cli/log"),
        &keep.join("agy-log"),
    );
    keep_tree(&r.w.join("tmp"), &keep.join("tmp"));
    // Muse Code's session logs (the stub's texts and the lab's prompts; it holds no credential: the fake key is
    // in its environment only)
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
    crate::clock::keep(&r, &keep.join("clock"));
    keep_tree(&r.state().join("logs"), &keep.join("state-logs"));
    if keep_dumps {
        keep_tree(&r.w.join("dump"), &keep.join("dump"));
    }
    if r.sockdir.starts_with("/tmp/at.") {
        let _ = fs::remove_dir_all(&r.sockdir);
    }
    for d in ["scratch", "shim", "gbin", "twin-src", "clock"] {
        let _ = fs::remove_dir_all(r.run.join(d));
    }
    // the copies of the lab's program go once one of them showed it ran, or once no assessment of them can still be
    // going; else they stay, named, and the run directory with them, env.sh and all
    clear_run_dir(&r.run, relay_ran, tag, std::time::SystemTime::now());
    Ok(keep)
}

/// What `lab up` writes for its TAB SHELLS in scratch `w`: Gemini CLI's
/// wrapper (`<run>/gbin/gemini`, which runs `gemini_bin`) where the lab has
/// one, and `home/.zshrc`, the profile every tab's shell reads
/// ([`tab_profile`], the stub at `port`).
fn put_tab_shell(w: &Path, run: &Path, gemini_bin: &str, port: &str) {
    let gbin = (!gemini_bin.is_empty()).then(|| run.join("gbin"));
    if let Some(dir) = &gbin {
        let _ = crate::sys::put(
            &dir.join("gemini"),
            0o755,
            &crate::sys::PutSrc::Bytes(crate::forever::gemini_wrapper(gemini_bin).as_bytes()),
        );
    }
    let _ = crate::util::atomic_write(
        &w.join("home/.zshrc"),
        tab_profile(port, gbin.as_deref()).as_bytes(),
    );
}

/// THE LAB'S TAB SHELLS' STARTUP FILE (`<scratch>/home/.zshrc`, every tab's
/// shell reads it at its start): the lab's prompt, and Gemini CLI's
/// environment as a person who runs it on an API key keeps it in their
/// profile — the lab's fake key ([`crate::gemini::LAB_GEMINI_KEY`]) and
/// `GOOGLE_GEMINI_BASE_URL` the stub at `port` (`NO_BROWSER` too, and no
/// Google Cloud credential variable), and, where the lab has a Gemini CLI,
/// its wrapper's directory `gbin` leading `PATH`, so the word `gemini` runs
/// the lab's build. A line the harness types into a tab's shell — a Gemini
/// CLI's relaunch, `HOME=<its home> 'gemini' '--resume' '<id>'`, which
/// carries its `HOME` alone — then reaches the stub as the launch did,
/// where before it came up at Gemini CLI's API-key dialog (the key lived in
/// the lab's launcher's environment alone: `spawn-residuals-2`'s `p5g` and
/// `v4g`, 2026-10-21).
#[must_use]
pub fn tab_profile(port: &str, gbin: Option<&Path>) -> String {
    let mut s = String::from("PROMPT='lab$ '\n");
    if let Some(dir) = gbin {
        s.push_str(&format!(
            "export PATH={}:\"$PATH\"\n",
            shell_quote(&dir.to_string_lossy())
        ));
    }
    s.push_str(
        "unset GOOGLE_API_KEY GOOGLE_GENAI_USE_VERTEXAI GOOGLE_CLOUD_PROJECT GOOGLE_APPLICATION_CREDENTIALS\n",
    );
    s.push_str(&format!(
        "export GEMINI_API_KEY={} GOOGLE_GEMINI_BASE_URL=http://127.0.0.1:{port} NO_BROWSER=true\n",
        crate::gemini::LAB_GEMINI_KEY
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
    let r = Run::load(tag)?;
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
        // a run directory torn down between the listing and its read is no longer a lab
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
    /// `-- <claude args>`.
    pub args: Vec<String>,
}

/// Parse `claude`'s options.
///
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
            "--" => {
                l.args.extend(it.by_ref().cloned());
            }
            other => {
                return Err(format!(
                    "claude: unknown option '{other}' (use -e K=V, --no-wait, --relay, --subscriber, --debug-log, -- <claude args>)"
                ));
            }
        }
    }
    Ok(l)
}

/// The launcher the tab runs (`sh claude.sh`): it ends in `exec`, so the tab's
/// process IS the real binary, launched by path (not through the managed shim,
/// which would add its own environment); behind the relay with `--relay`. Its
/// opens are the open recorder's, logged to the scratch's `open.log`
/// ([`crate::recorder`]), and its keychain calls the tripwire's.
#[must_use]
pub fn launcher(run: &Run, l: &Launch) -> String {
    let q = shell_quote;
    let w = run.w.to_string_lossy();
    // the sleep clock, when the lab is up with it: the shell that runs this drops every DYLD_* variable
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
        // the relay is for the nudge, which acts on the alternate screen only
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
    // every open goes to the recorder's log: Claude Code runs `$BROWSER <url>`, or a bare-name `open` when it is
    // unset, which the shim's `open` leading PATH answers
    s.push_str(&format!(
        "  LAB_SECURITY_LOG={} LAB_OPEN_LOG={} BROWSER={} \\\n",
        q(&format!("{w}/security.log")),
        q(&format!("{w}/open.log")),
        q(&run.run.join("shim/open").to_string_lossy())
    ));
    for kv in &envs {
        s.push_str(&format!("  {} \\\n", q(kv)));
    }
    s.push_str(&format!("  {}", q(&run.claude_bin)));
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

/// Whether the relay copied into the lab's run directory ran: an earlier
/// launch recorded it ([`RELAY_RAN`]), or the relay made its log, which each
/// launch behind it removes first (`relay.log`, read through no link).
fn relay_shown_running(run: &Run) -> bool {
    run.run.join(RELAY_RAN).exists()
        || crate::sys::scratch_read(&run.w.join("relay.log")).is_ok_and(|b| b.is_some())
}

/// Lay the relay for a launch behind it: this program under the name `node`,
/// copied where jailed code cannot write (`relay/node`), ONCE per lab. The
/// copy is the same file at every launch (the lab's program does not change
/// while it is up), and a later launch never replaces it: an earlier launch's
/// relay may still be starting from it, its first exec under assessment, and
/// a file renamed over or removed then crashes `syspolicyd`. A relay that
/// made its log ran, which is recorded before the log goes ([`RELAY_RAN`]).
///
/// # Errors
/// The copy could not be made.
fn lay_relay(run: &Run) -> Result<(), Fail> {
    let dir = run.run.join("relay");
    if relay_shown_running(run) && dir.join("node").is_file() {
        let _ = fs::write(run.run.join(RELAY_RAN), "");
    }
    let node = dir.join("node");
    if !node.is_file() {
        // `node.new` is never started (the launcher names `node`), so a half-made one is replaced.
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

/// Type the launch line of the managed Claude Code into the lab's first tab;
/// the `launched: …` line that says what was launched.
///
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
        // The key helper comes out of the settings, so the fake OAuth login is the one Claude Code uses.
        crate::verbs::settings_merge(run, &J::obj([("apiKeyHelper", J::Null)]))?;
    }
    let script = run.run.join("claude.sh");
    atomic_write(&script, launcher(run, l).as_bytes())
        .map_err(|e| Fail::die(format!("cannot write {}: {e}", script.display())))?;
    let _ = fs::set_permissions(&script, fs::Permissions::from_mode(0o755));
    let door = Door::new(run)?;
    let line = format!("sh {}", shell_quote(&script.to_string_lossy()));
    let sent = door.ctl(&["send", &line])?;
    if sent.code != 0 {
        return Err(Fail::die("could not type the launch line"));
    }
    door.ctl(&["key", "enter"])?;
    Ok(format!(
        "launched: {} ({}) in lab '{}'{}  (launcher: {})",
        run.claude_bin,
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

/// `claude [tag] [-e K=V]... [--no-wait] [--relay] [--subscriber] [--debug-log] [-- claude args]`.
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

    /// THE TAB SHELLS' PROFILE CARRIES GEMINI CLI'S KEY: a line typed into a
    /// tab's shell as a Gemini CLI's relaunch is (`HOME=<its home> gemini
    /// --resume <id>`, its `HOME` alone) runs the lab's build with the lab's
    /// fake key and the stub's base URL, read here off a stand-in build that
    /// prints what it was handed, under zsh sourcing the profile as a tab's
    /// shell reads it — the very `home/.zshrc` and `gbin/gemini` `lab up`
    /// writes ([`put_tab_shell`]), so `up` writing any other profile fails
    /// it. NEGATIVE CONTROLS: the profile before (the prompt alone) finds no
    /// `gemini`; with no Gemini CLI in the lab, the key is still exported and
    /// `PATH` left alone.
    #[test]
    fn a_line_typed_in_a_tab_shell_runs_gemini_with_the_labs_key() {
        let tmp = aterm_tempfile::tempdir().unwrap();
        // the temporary directory's real path: `/var` is a link, and the
        // lab's writer refuses a link in a path
        let dir = tmp.path().canonicalize().unwrap();
        let build = dir.join("gemini-build");
        std::fs::write(
            &build,
            "#!/bin/sh\nprintf 'key=%s url=%s home=%s args=%s\\n' \"$GEMINI_API_KEY\" \"$GOOGLE_GEMINI_BASE_URL\" \"$HOME\" \"$*\"\n",
        )
        .unwrap();
        std::fs::set_permissions(&build, fs::Permissions::from_mode(0o755)).unwrap();
        let (w, run) = (dir.join("scratch"), dir.join("run"));
        put_tab_shell(&w, &run, &build.to_string_lossy(), "4242");
        let written = std::fs::read_to_string(w.join("home/.zshrc")).unwrap();
        let gbin = run.join("gbin");
        assert_eq!(written, tab_profile("4242", Some(&gbin)));
        let typed = |profile: &str| {
            let rc = dir.join("zshrc");
            std::fs::write(&rc, profile).unwrap();
            let out = aterm_uds::spawnfd::command("/bin/zsh")
                .args([
                    "-f",
                    "-c",
                    "source \"$1\"; HOME=/elsewhere/gemini-home gemini --resume abc",
                ])
                .arg("zsh")
                .arg(&rc)
                .env_clear()
                .env("PATH", "/usr/bin:/bin")
                .output()
                .unwrap();
            String::from_utf8_lossy(&out.stdout).trim().to_string()
        };
        assert_eq!(
            typed(&written),
            format!(
                "key={} url=http://127.0.0.1:4242 home=/elsewhere/gemini-home args=--resume abc",
                crate::gemini::LAB_GEMINI_KEY
            )
        );
        assert_eq!(typed("PROMPT='lab$ '\n"), "", "the profile before");
        let (w, run) = (dir.join("scratch-none"), dir.join("run-none"));
        put_tab_shell(&w, &run, "", "4242");
        let none = std::fs::read_to_string(w.join("home/.zshrc")).unwrap();
        assert_eq!(none, tab_profile("4242", None));
        assert!(!run.join("gbin").exists(), "no wrapper");
        assert!(!none.contains("PATH"), "{none}");
        assert!(
            none.contains(&format!("GEMINI_API_KEY={}", crate::gemini::LAB_GEMINI_KEY)),
            "{none}"
        );
    }

    /// Every lab's scratch Claude Code settings carry the settings witness: a
    /// `ConfigChange` hook that runs the run directory's `config-change.sh`
    /// (written from outside the jail), beside the fake key helper and the
    /// env. NEGATIVE CONTROL: another run's script is not the one named.
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
        // the written file's key order is its own: compare each hook's command
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

    /// `lab up`'s scratch `aterm.toml` starts with the audible bell off, and
    /// `LAB_ATERM_TOML` can turn it back on (`bell_sound = true`, the one key
    /// the file then holds). Its other lines keep their places: the root key
    /// it leads with stays at the root, the harness keys under `[harness]`.
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

    /// `ps <tag>` AND `down <tag>` OF A LAB THAT IS DOWN NAME THAT TAG
    /// (ATERM-KI-1016-1): each refuses with the lab `<tag>` is not up, never
    /// the default lab's words, and `down` touches no lab. Read through the
    /// verbs themselves (`take_tag` there left the tag as an argument, and
    /// the verb acted on the default lab). PRECONDITION: neither lab is up
    /// here, so no lab is ever taken down by this test.
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
        // the words the shell execs: env's assignments route every open to the recorder's log, and the binary
        // is the program with each user word one argument
        let words = exec_words(&s);
        assert!(opens_recorded(&words, &run), "{words:?}");
        let at = words.iter().position(|w| w == "/a b/claude").unwrap();
        assert_eq!(words[at + 1..], ["--model", "x y"], "{words:?}");
        let plain = launcher(&run, &Launch::default());
        assert!(opens_recorded(&exec_words(&plain), &run), "{plain}");
        // NEGATIVE CONTROLS: a line without LAB_OPEN_LOG, or without BROWSER, leaves an open unlogged; so does
        // the variable handed to Claude Code as an argument instead of in its environment
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

    /// The words of a launcher's `exec` command as `sh` reads them: its
    /// continuation lines joined, split at blanks outside quotes, single and
    /// double quotes and backslashes removed.
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

    /// Whether `env`'s assignments in a launcher's words (those between `env`
    /// and the program it runs) route every open of the program to the open
    /// recorder: the shim leads `PATH` (a bare-name `open`), `BROWSER` is the
    /// shim's `open`, and `LAB_OPEN_LOG` is the scratch's `open.log`.
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
        // NEGATIVE CONTROL: a record with no version line names none
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

    /// A package linked from a checkout (`aterm pkg link codex <pkg>`, as the
    /// owner links codex-alab) names its binary at the chain's end too.
    /// NEGATIVE CONTROL: a line that says neither managed nor linked names none.
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

    /// `LAB_INSTANCE_ENV` hands the lab's instance `TMPDIR` and dev seams
    /// alone, in the order given, and never a name the lab sets itself (its
    /// jail's HOME, XDG dirs, state root and PATH, the harness tape, or the
    /// headless-restore seam `LAB_RESTORE=1` owns, whether or not this lab
    /// runs under it).
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
        for bad in [
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

    /// Every launch of the instance gets what the lab sets and then what `up`
    /// recorded of `LAB_INSTANCE_ENV`: read off the record, so a relaunch
    /// gets the same whatever its caller's environment. A record edited to
    /// name one of the lab's own is refused, never handed on.
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
            vars.last(),
            Some(&("TMPDIR".to_string(), "/tmp".to_string()))
        );
        let seam = instance_vars(&recorded("", "1"), "owner").unwrap();
        assert!(seam.contains(&("ATERM_DEBUG_HEADLESS_RESTORE".to_string(), "1".to_string())));
        assert!(!seam.iter().any(|(k, _)| k == "TMPDIR"));
        for edited in ["HOME=/elsewhere", "ATERM_DEBUG_HEADLESS_RESTORE=1"] {
            assert!(
                instance_vars(&recorded(edited, "1"), "owner").is_err(),
                "{edited}"
            );
        }
    }

    /// THE STORE IS LOOKED UP IN THE ACCOUNT'S HOME UNDER A SCRATCH `HOME`:
    /// `aterm pkg which` reads the package store under the `HOME` it is given,
    /// and a scratch `HOME` holds none, so the lab hands that one call the
    /// account's home (the account database's, [`crate::paths::Paths::store_home`]).
    /// A stand-in `aterm` answers from the `HOME` it got. NEGATIVE CONTROL: the call handed `$HOME` (the
    /// lab before this) finds the scratch's store, which is not the
    /// account's; and with no account home known, `$HOME`'s is the one asked.
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
        // the lookups the lab makes: Claude Code's, and every other managed agent's
        assert_eq!(store_claude(&p, &stand_in), Ok(store(&account, "claude")));
        for name in ["codex", "agy", "muse"] {
            assert_eq!(
                managed_store_bin_in(&p, &stand_in, name),
                Some(store(&account, name)),
                "{name}"
            );
        }
        // NEGATIVE CONTROL: the facts of a lab that took HOME for the account's home find the
        // scratch's store, which is not the account's
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
        // NEGATIVE CONTROL: $HOME's lookup finds the scratch's store
        assert_eq!(p.caller_home, scratch.display().to_string());
        assert_ne!(asked(&p.caller_home), store(&account, "claude"));
        assert_eq!(asked(&p.caller_home), store(&scratch, "claude"));
        // no account home known: $HOME's is the one asked
        let only = crate::paths::Paths::resolve_with(None, Some(scratch.clone().into()));
        assert_eq!(only.store_home(), scratch.display().to_string());
    }

    /// Whether a shell's environment routes an agent's URL open to the open
    /// recorder: its log (`LAB_OPEN_LOG`) is the scratch's `open.log`, and the
    /// opener is the shim's `open`: `BROWSER` when it is set (Claude Code opens
    /// `$BROWSER <url>`), else the bare-name `open` the shell finds (`which`).
    fn opens_logged(browser: &str, log: &str, which: &str, run: &Run) -> bool {
        let shim = run.run.join("shim/open").display().to_string();
        let opener = if browser.is_empty() { which } else { browser };
        log == format!("{}/open.log", run.w.display()) && opener == shim
    }

    /// `BROWSER`, `LAB_OPEN_LOG` and `command -v open` as a login zsh started
    /// with exactly `vars` (`env -i`) reads them: the shell a session the
    /// instance spawns runs in, after `/etc/zprofile`'s `path_helper`.
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

    /// A SESSION THE INSTANCE SPAWNS OPENS THROUGH THE RECORDER: a worker's
    /// Claude Code (`spawn agent=claude`) is typed into a login shell the
    /// instance spawned, never behind `claude.sh`, and that shell's
    /// `path_helper` puts the shim LAST, so its bare-name `open` is
    /// `/usr/bin/open`, which the jail refuses and nothing logs. The
    /// instance's environment names the recorder as `BROWSER` with its log, and
    /// a real login zsh given exactly that environment keeps both. NEGATIVE
    /// CONTROL: the same login shell without `BROWSER` finds `/usr/bin/open`
    /// for a bare-name open (the shim on PATH is not enough), so nothing would
    /// be logged.
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
        // neither can be handed over by LAB_INSTANCE_ENV
        assert!(lab_sets("BROWSER") && lab_sets("LAB_OPEN_LOG"));
        let read = login_shell_reads(&vars, &run.w);
        assert_eq!(read.len(), 3, "{read:?}");
        assert_eq!(read[0], shim_open, "BROWSER passes the login shell");
        assert_eq!(read[1], log, "LAB_OPEN_LOG passes the login shell");
        assert_eq!(read[2], "/usr/bin/open", "path_helper puts the shim last");
        assert!(opens_logged(&read[0], &read[1], &read[2], &run), "{read:?}");
        // NEGATIVE CONTROL: the instance's environment without BROWSER (the lab before this) leaves a spawned
        // session's open to /usr/bin/open
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

    /// THE SLEEP CLOCK GOES WHERE `LAB_CLOCK=1` PUTS IT: every launch of the
    /// instance of a lab up with it (`up`'s and each relaunch's, off the
    /// record) loads it, and `claude.sh` hands it to Claude Code past the
    /// shell that drops it; a lab up without it hands it to neither, and
    /// neither `LAB_INSTANCE_ENV` nor `claude -e` may name its variables.
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

    /// The instance's command line is the lab's own socket, project dir and
    /// grid at `up` and at each relaunch on the same record; the lifeline's
    /// flag only for an instance that holds one.
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

    /// A WINDOWED LAB'S INSTANCE (`forever --window`) opens a window: no
    /// `--headless` and no lifeline flag (a window refuses `--lifeline-fd`),
    /// the same socket, project dir and grid after its program; where it
    /// holds a lifeline, the lab's own keeper leads the line and holds it.
    /// CONTROL: the headless record's line, which a window would refuse.
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
        // CONTROL: the record without the window, headless with its flag
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

    /// A WINDOWED LAB UNDER `LAB_RESTORE=1` IS REFUSED, as a usage error and
    /// before anything is made: the headless-restore seam is a headless
    /// instance's. CONTROLS: the window alone, the seam alone and neither
    /// are refused by nothing.
    #[test]
    fn a_windowed_lab_under_the_restore_seam_is_refused_before_anything_is_made() {
        let o = |window, restore| UpOpts {
            window,
            restore,
            ..UpOpts::default()
        };
        let why = opts_refused(&o(true, true)).expect("refused");
        assert!(why.contains("LAB_RESTORE=1"), "{why}");
        // `up` refuses it first, before it even judges the tag: under a tag no
        // lab may have, an `up` that went on would end at the tag (exit 1),
        // so nothing is made either way
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

    /// A WINDOW IS READY ONCE ITS CONTROL SOCKET ANSWERS: an answer at the
    /// third ask is ready, asked before whether the child lives. NEGATIVE
    /// CONTROLS: a child that ended before any answer, and one that never
    /// answers (a zero hang detector), each fail with their own reason; an
    /// interrupt during the pause ends the wait as `interrupted`.
    #[test]
    fn a_window_is_ready_once_its_socket_answers_and_never_on_a_dead_child_or_a_hang() {
        let asks = std::cell::Cell::new(0u32);
        let answered = until_answered(
            || {
                asks.set(asks.get() + 1);
                asks.get() == 3
            },
            || true,
            READY_HANG,
            || Ok(()),
        );
        assert_eq!(answered, Ok(()));
        assert_eq!(asks.get(), 3);
        // an answer stands even where the child has ended since
        assert_eq!(
            until_answered(|| true, || false, READY_HANG, || Ok(())),
            Ok(())
        );
        let e = until_answered(|| false, || false, READY_HANG, || Ok(())).expect_err("dead");
        assert!(
            e.contains("ended before its control socket answered"),
            "{e}"
        );
        let e = until_answered(|| false, || true, Duration::ZERO, || Ok(())).expect_err("hang");
        assert!(e.contains("hang detector"), "{e}");
        let e = until_answered(
            || false,
            || true,
            READY_HANG,
            || Err("interrupted".to_string()),
        )
        .expect_err("interrupted");
        assert_eq!(e, "interrupted");
    }

    /// A pid is the lab's only while its command line names the lab's run or
    /// socket directory: no pid of 1 or less, none whose command line names
    /// another directory. CONTROL: this test's own process, whose command
    /// line names its directory, is that directory's "lab".
    #[test]
    fn a_pid_is_the_lab_s_only_while_its_command_line_names_the_lab() {
        let me = i32::try_from(std::process::id()).unwrap();
        let elsewhere = recorded("", "0");
        for pid in [0, 1, -1, me] {
            assert!(!names_this_lab(&elsewhere, pid), "{pid}");
        }
        // the directory of the program this process runs, as its own command line spells it
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

    /// A run directory under a private scratch dir, its scratch made, its
    /// copy of the lab's program a plain file (nothing is started from it).
    /// The scratch dir by its physical path: the lab reads the scratch through
    /// no link, and the temporary directory's `/var` is one.
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

    /// AN ABORTED `up` KEEPS A COPY NOTHING SHOWED RAN (r4g-1): a lab that
    /// started its witness or stub from its fresh copy of the lab's program,
    /// and was aborted before the stub's ready byte proved that copy ran,
    /// keeps its run directory (macOS may still be assessing the copy's
    /// first exec); once the stub said it ran, the abort removes it.
    /// NEGATIVE CONTROLS: the proven lab's directory goes, and so does one
    /// that never started anything from its copy. No signal reaches any
    /// process: no pids are handed in, and the lab has no jail mark.
    #[test]
    fn an_aborted_up_keeps_a_copy_nothing_showed_ran() {
        let root = aterm_tempfile::tempdir().unwrap();
        let none = i32::MAX; // no such process: only "started" is read from it
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
            let _ = fs::remove_dir_all(root.path().join("runs"));
        }
    }

    /// A directory of copies may go once one of them showed it ran, or once
    /// it is older than the first-exec hang; a missing one may go.
    /// NEGATIVE CONTROL: unproven, a minute short of the hang, it stays.
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

    /// A run directory as `down` leaves it for its last step: the scratch
    /// gone, its own files (`env.sh` among them) there, its copy of the lab's
    /// program shown to run or not, and a relay laid beside it or not; when
    /// its copies were made.
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

    /// Past the first-exec hang for copies made at `made` or within a minute
    /// after it.
    fn past_the_hang(made: std::time::SystemTime) -> std::time::SystemTime {
        made + aterm_uds::deadline::FIRST_EXEC_HANG + Duration::from_secs(60)
    }

    /// A DOWN THAT KEEPS A COPY KEEPS ITS LAB (r5u-1): a run directory whose
    /// copy of the lab's program (the stub's or the relay's) nothing showed
    /// ran keeps its `env.sh` beside that copy, so `ps` still lists the lab
    /// and a later `down`, the first-exec hang past, removes the copy and the
    /// directory. NEGATIVE CONTROLS: copies that showed they ran go at the
    /// first `down`, `env.sh` and the directory with them.
    #[test]
    fn a_down_that_keeps_a_copy_keeps_its_lab_for_a_later_down() {
        let root = aterm_tempfile::tempdir().unwrap();
        // (what, the stub's copy ran, a relay laid, the relay ran, kept)
        for (what, bin_ran, relay, relay_ran, kept) in [
            ("stub's copy unproven", false, false, false, true),
            ("relay's copy unproven", true, true, false, true),
            ("both copies ran", true, true, true, false),
            ("stub's copy ran, no relay", true, false, false, false),
        ] {
            let (run, made) = torn_down(root.path(), bin_ran, relay);
            let state = run.run.parent().and_then(Path::parent).unwrap();
            clear_run_dir(&run.run, relay_ran, &run.tag, made);
            assert!(!run.run.join("jail.sb").exists(), "{what}");
            assert_eq!(run.run.join("env.sh").is_file(), kept, "{what}");
            let listed = if kept { vec![run.tag.clone()] } else { vec![] };
            assert_eq!(all_runs(state), listed, "{what}");
            if kept {
                // the later `down`, once nothing can still be assessing either copy
                clear_run_dir(&run.run, relay_ran, &run.tag, past_the_hang(made));
            }
            assert!(!run.run.exists(), "{what}: the later down finished");
        }
    }

    /// AN UP NEVER LAYS ITS COPY OVER AN UNPROVEN ONE (r5u-1): a tag whose
    /// run directory holds no `env.sh` but a copy an earlier lab left that
    /// nothing showed ran waits, that copy in place, until the first-exec
    /// hang has passed; then the copies go, so `up` copies onto a fresh path
    /// and lays a fresh relay. A tag with an `env.sh` is up already: nothing
    /// of it is touched, the hang past or not. NEGATIVE CONTROL: copies that
    /// showed they ran go at once.
    #[test]
    fn an_up_waits_out_a_copy_an_earlier_lab_left_unproven() {
        let root = aterm_tempfile::tempdir().unwrap();
        let runs = root.path().join("runs");
        let gone = |run: &Run| !run.run.join("bin").exists() && !run.run.join("relay").exists();
        let (up, made) = torn_down(root.path(), false, false);
        assert!(room_for(&up, past_the_hang(made)).is_err(), "up already");
        assert!(up.run.join(BIN).is_file(), "an up lab's copy stays");
        fs::remove_dir_all(&runs).unwrap();
        // (what, the stub's copy ran, the relay ran, the unproven copy)
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
            assert!(room_for(&left, past_the_hang(made)).is_ok(), "{what}");
            assert!(gone(&left), "{what}: the hang past, the copies went");
            fs::remove_dir_all(&runs).unwrap();
        }
        let (proven, made) = torn_down(root.path(), true, true);
        fs::remove_file(proven.run.join("env.sh")).unwrap();
        fs::write(proven.run.join(RELAY_RAN), "").unwrap();
        assert!(room_for(&proven, made).is_ok(), "both ran: room at once");
        assert!(gone(&proven));
    }

    /// THE RELAY'S COPY IS LAID ONCE AND NEVER REPLACED (r4g-1): a second
    /// launch behind the relay keeps the file the first one started from
    /// (the same inode), and records that relay ran once it made its log,
    /// before that log goes. NEGATIVE CONTROL: a launch whose earlier relay
    /// made no log records nothing.
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
        // the relay made its log: the next launch records it and removes the log
        fs::write(run.w.join("relay.log"), "").unwrap();
        assert!(relay_shown_running(&run));
        lay_relay(&run).unwrap();
        assert!(run.run.join(RELAY_RAN).exists(), "recorded");
        assert!(!run.w.join("relay.log").exists(), "its log went");
        assert_eq!(fs::metadata(&node).unwrap().ino(), first, "still kept");
        assert!(relay_shown_running(&run), "the record stands");
    }
}
