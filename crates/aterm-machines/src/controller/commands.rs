// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The owner's verbs (design §4, §6, §9): `init`, `ls`, `status`, `outstanding`, `plan`,
//! `apply`, `attach`, `run`, `person`, `screen`, `github revoke`, `rekey`, `unenroll`, and
//! the local `here`.
//!
//! A fleet command runs in three phases:
//!
//! 1. A `hello` sweep, in parallel: reachability, agent build, floors and cache.
//! 2. Envelopes built one after another, so the serial counter is never raced, then signed
//!    in one `ssh-keygen` run, so a passphrase or touch is asked for once per command (and
//!    not at all when ssh-agent already holds the key).
//! 3. Uploads and the run itself, in parallel again. `apply` first upgrades a host whose
//!    agent is not this controller's own build (design §4.5).
//!
//! Every host's newest plan or apply report lands in `state/` (a refusal is kept beside it,
//! never in its place), and `status` and `outstanding` read only that. Everything a host
//! said is printed through [`shown`].

use super::artifacts;
use super::enroll;
use super::inventory::{self, Host, HostState, Inventory, Paths};
use super::screen;
use super::ssh::{self, Target};
use super::templates;
use super::text::shown;
use crate::envelope::{self, Action, ArtifactRef, Disrupt, Envelope, Exec, Signing, Upgrade};
use crate::model::{Effect, HostReport, ItemReport, Mode, Outcome, Read};
use crate::port::{Cmd, Ran, System};
use crate::protocol::{Hello, Line};
use std::collections::{BTreeMap, BTreeSet};
use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::Duration;

/// How long a `hello` may take.
const HELLO_BUDGET: Duration = Duration::from_secs(30);

/// How long one `apply` request may stream before the controller lets it detach. The run
/// itself goes on on the host (design §4.4); `attach` picks it up again.
const APPLY_BUDGET: Duration = Duration::from_secs(4 * 3600);

/// How long an upload may take (the aterm release zip, Emacs' image: hundreds of MB).
const PUT_BUDGET: Duration = Duration::from_secs(3600);

/// How long an agent upgrade may take on the host (expand, codesign, swap, sudoers).
const UPGRADE_BUDGET: Duration = Duration::from_secs(900);

/// How long an envelope stays valid (design §3.3: the serial floor is the anti-replay
/// control, so this only bounds a forgotten envelope).
const ENVELOPE_LIFETIME: u64 = 24 * 3600;

/// How long the signing key stays in ssh-agent for one person flow (`ssh-add -t`). The flow
/// removes it when it ends; a flow that is interrupted (Ctrl-C) cannot, and the key then
/// expires from the agent after this long.
const AGENT_KEY_LIFETIME: &str = "3600";

/// What every verb needs.
pub struct Env<'a> {
    /// The port.
    pub sys: &'a dyn System,
    /// The controller directory.
    pub paths: Paths,
    /// Print NDJSON instead of tables.
    pub json: bool,
    /// Standard input and standard error are terminals (a person is there).
    pub tty: bool,
    /// Concurrent hosts (`--parallel`), over `[controller] parallel`.
    pub parallel: Option<usize>,
    /// This process's `SSH_AUTH_SOCK`: the ssh-agent a signing key with a passphrase may
    /// already be loaded in.
    pub agent_sock: Option<OsString>,
}

/// The exit status design §9 names: 0 nothing failed, 1 something failed, 2 nothing failed
/// but some hosts could not be reached. A command line that is refused is 2 as well.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Tally {
    /// An item failed or could not be measured, or a host refused or broke.
    pub failed: bool,
    /// A host could not be reached.
    pub unreachable: bool,
}

impl Tally {
    /// The exit code.
    #[must_use]
    pub fn code(self) -> ExitCode {
        if self.failed {
            ExitCode::FAILURE
        } else if self.unreachable {
            ExitCode::from(2)
        } else {
            ExitCode::SUCCESS
        }
    }
}

// ── init ────────────────────────────────────────────────────────────────────────────────

/// `aterm machines init`: the controller directory, the fleet SSH key, the signing and
/// recovery keys (their passphrases are asked by `ssh-keygen` itself), `allowed_signers`,
/// and the starter inventory and baseline. Nothing that exists is overwritten.
#[must_use]
pub fn init(env: &Env<'_>) -> ExitCode {
    if !env.tty {
        eprintln!(
            "aterm machines init: not run: it asks for the signing and recovery keys' \
             passphrases, and standard input is not a terminal"
        );
        return ExitCode::from(2);
    }
    let sys = env.sys;
    let root = &env.paths.root;
    if let Err(why) = env.paths.ensure(sys) {
        eprintln!("aterm machines init: {why}");
        return ExitCode::FAILURE;
    }
    if let Err(why) = env.paths.ensure_control_dir(sys) {
        eprintln!("aterm machines init: {why}");
        return ExitCode::FAILURE;
    }
    let host = controller_name(sys);
    let fleet_key = env.paths.ssh_key();
    if sys.stat(&fleet_key).is_err() {
        let made = sys.run(
            &Cmd::new("/usr/bin/ssh-keygen")
                .args(["-q", "-t", "ed25519", "-N", "", "-C"])
                .arg(format!("aterm-machines-fleet@{host}"))
                .arg("-f")
                .arg(fleet_key.as_os_str()),
        );
        match made {
            Ok(ran) if ran.success() => println!("made the fleet SSH key: {}", fleet_key.display()),
            Ok(ran) => {
                eprintln!(
                    "aterm machines init: ssh-keygen (fleet key): {}",
                    ran.describe()
                );
                return ExitCode::FAILURE;
            }
            Err(e) => {
                eprintln!("aterm machines init: ssh-keygen (fleet key): {e}");
                return ExitCode::FAILURE;
            }
        }
    }
    let keys = [
        (
            env.paths.signing_key(),
            format!("signing@{host}"),
            "the daily signing key. An empty passphrase lets automation on this Mac apply \
             baselines; a passphrase means only you can (design §3.1).",
        ),
        (
            root.join("keys").join("recovery"),
            format!("recovery@{host}"),
            "the recovery key. Give it a passphrase, then move keys/recovery (not the .pub) \
             to removable media: it re-keys the fleet if the signing key is lost or leaks.",
        ),
    ];
    for (path, comment, what) in &keys {
        let public = path.with_extension("pub");
        if sys.stat(&public).is_ok() {
            continue;
        }
        println!("\nNext: {what}");
        let made = sys.run_tty(
            &Cmd::new("/usr/bin/ssh-keygen")
                .args(["-t", "ed25519", "-C"])
                .arg(comment)
                .arg("-f")
                .arg(path.as_os_str()),
        );
        if !matches!(made, Ok(Some(0))) {
            eprintln!("aterm machines init: ssh-keygen ({comment}) did not make the key");
            return ExitCode::FAILURE;
        }
    }
    if sys.stat(&env.paths.allowed_signers()).is_err() {
        let mut signers = String::new();
        for (path, comment, _) in &keys {
            let Ok(public) = sys.read_file(&path.with_extension("pub")) else {
                eprintln!(
                    "aterm machines init: {} is missing",
                    path.with_extension("pub").display()
                );
                return ExitCode::FAILURE;
            };
            match envelope::signers_line(comment, String::from_utf8_lossy(&public).trim()) {
                Ok(line) => {
                    signers.push_str(&line);
                    signers.push('\n');
                }
                Err(why) => {
                    eprintln!(
                        "aterm machines init: {}: {why}",
                        path.with_extension("pub").display()
                    );
                    return ExitCode::FAILURE;
                }
            }
        }
        if let Err(e) = sys.write_file(&env.paths.allowed_signers(), signers.as_bytes(), 0o644) {
            eprintln!("aterm machines init: allowed_signers: {e}");
            return ExitCode::FAILURE;
        }
    }
    for (path, text) in [
        (env.paths.inventory(), templates::INVENTORY),
        (env.paths.baseline("default"), templates::DEFAULT_BASELINE),
    ] {
        if sys.stat(&path).is_ok() {
            continue;
        }
        if let Err(e) = sys.write_file(&path, text.as_bytes(), 0o600) {
            eprintln!("aterm machines init: {}: {e}", path.display());
            return ExitCode::FAILURE;
        }
        println!("wrote {}", path.display());
    }
    println!(
        "\nThe controller is ready in {}.\n\
         1. Move keys/recovery to removable media, then delete it here. Back up this \
         directory beside it (keys/ssh_ed25519, known_hosts, machines.toml, serial): the \
         recovery key re-keys the fleet from here, but without that backup a lost \
         controller means a visit to every Mac.\n\
         2. aterm needs Local Network access to reach your Macs: System Settings ▸ Privacy & \
         Security ▸ Local Network ▸ aterm.\n\
         3. At each Mac: `aterm machines help enroll` prints the one visit it needs.",
        root.display()
    );
    ExitCode::SUCCESS
}

/// This Mac's LocalHostName, for key comments and principals.
fn controller_name(sys: &dyn System) -> String {
    sys.run(&Cmd::new("/usr/sbin/scutil").args(["--get", "LocalHostName"]))
        .ok()
        .filter(crate::port::Ran::success)
        .map(|r| r.out().trim().to_owned())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "controller".to_owned())
}

// ── ls / status / outstanding ───────────────────────────────────────────────────────────

/// `aterm machines ls`: the inventory, one host per row.
#[must_use]
pub fn ls(env: &Env<'_>) -> ExitCode {
    let inv = match load(env) {
        Ok(inv) => inv,
        Err(code) => return code,
    };
    if env.json {
        for host in &inv.hosts {
            println!("{}", aterm_json::to_string(host).unwrap_or_default());
        }
        return ExitCode::SUCCESS;
    }
    if inv.hosts.is_empty() {
        println!("no hosts yet: `aterm machines help enroll`");
        return ExitCode::SUCCESS;
    }
    println!(
        "{:<16} {:<24} {:<12} {:<10} UUID",
        "NAME", "ADDRESS", "ADMIN", "BASELINE"
    );
    for host in &inv.hosts {
        println!(
            "{:<16} {:<24} {:<12} {:<10} {}",
            host.name,
            host.address,
            host.user,
            host.baseline.as_deref().unwrap_or("default"),
            host.uuid
        );
    }
    ExitCode::SUCCESS
}

/// One host's row in `status`: how it was last reached, its newest plan or apply report, a
/// refusal of a later run, an OS build that changed since its last apply, and the macOS and
/// agent it runs.
fn summary(host: &Host, state: &HostState, now: u64) -> String {
    let mut reach = match (state.unreachable_since, state.last_seen) {
        (Some(since), _) => format!("unreachable for {}", age(now.saturating_sub(since))),
        (None, 0) => "never reached".to_owned(),
        (None, seen) => format!("seen {} ago", age(now.saturating_sub(seen))),
    };
    if let Some((since, why)) = &state.contact_error {
        reach.push_str(&format!(
            "; failing for {}: {}",
            age(now.saturating_sub(*since)),
            shown(why)
        ));
    }
    let hello = saved_hello(state);
    let mut notes = String::new();
    if let Some((at, why)) = &state.last_refusal {
        notes.push_str(&format!(
            "; REFUSED {} ago: {}",
            age(now.saturating_sub(*at)),
            shown(why)
        ));
    }
    if let Some((was, is)) = os_changed(state, hello.as_ref()) {
        notes.push_str(&format!("; os-changed ({} → {})", shown(&was), shown(&is)));
    }
    let agent = hello
        .as_ref()
        .map(|h| {
            format!(
                "agent {}",
                shown(&build_name(&h.agent_version, &h.agent_sha256))
            )
        })
        .unwrap_or_default();
    let Some(report) = &state.report else {
        let agent = if agent.is_empty() {
            String::new()
        } else {
            format!("  [{agent}]")
        };
        return format!("{:<16} {reach}; no report yet{notes}{agent}", host.name);
    };
    let mut counts = String::new();
    for (outcome, word) in [
        (Outcome::Ok, "ok"),
        (Outcome::Changed, "changed"),
        (Outcome::Planned, "planned"),
        (Outcome::Failed, "failed"),
        (Outcome::Unknown, "unknown"),
        (Outcome::Person, "person"),
        (Outcome::Deferred, "deferred"),
        (Outcome::Blocked, "blocked"),
        (Outcome::NotApplicable, "n/a"),
    ] {
        let n = report.count(outcome);
        if n > 0 {
            counts.push_str(&format!(" {n} {word}"));
        }
    }
    let mode = match report.mode {
        Mode::Plan => "planned",
        Mode::Apply => "applied",
    };
    // A refusal saved as the report itself: a state file from before refusals were kept
    // beside the report, or a refused run `print_report` shows as it ends.
    let refused = report
        .refused
        .as_ref()
        .map(|r| format!("; REFUSED: {}", shown(r)))
        .unwrap_or_default();
    let agent = if agent.is_empty() {
        String::new()
    } else {
        format!(", {agent}")
    };
    format!(
        "{:<16} {reach}; {mode} {} ago;{counts}{refused}{notes}  [macOS {} {}{agent}]",
        host.name,
        age(now.saturating_sub(report.finished)),
        shown(&report.facts.os_version),
        shown(&report.facts.os_build)
    )
}

/// The newest `hello` the host's state keeps, when it parses.
fn saved_hello(state: &HostState) -> Option<Hello> {
    state
        .hello
        .as_deref()
        .and_then(|text| aterm_json::from_str(text).ok())
}

/// The OS build at the host's last apply and the one its newest `hello` reported, when both
/// are known and differ (design §4.4): a macOS update brings back what `[setup]` turned off,
/// so the host wants another apply. An unknown build on either side says nothing.
fn os_changed(state: &HostState, hello: Option<&Hello>) -> Option<(String, String)> {
    let was = state.applied_build.as_deref().filter(|b| !b.is_empty())?;
    let is = hello
        .map(|h| h.os_build.as_str())
        .filter(|b| !b.is_empty())?;
    (was != is).then(|| (was.to_owned(), is.to_owned()))
}

/// `3m`, `2h`, `5d`.
fn age(secs: u64) -> String {
    match secs {
        0..60 => format!("{secs}s"),
        60..3600 => format!("{}m", secs / 60),
        3600..86_400 => format!("{}h", secs / 3600),
        _ => format!("{}d", secs / 86_400),
    }
}

/// A host's saved state; a state file that is there and cannot be read is said once and
/// counts as a failure, never as a host with no report.
fn read_state(env: &Env<'_>, host: &Host, tally: &mut Tally) -> Option<HostState> {
    match inventory::load_state(env.sys, &env.paths, &host.uuid) {
        Ok(state) => Some(state),
        Err(why) => {
            tally.failed = true;
            println!("{:<16} state unreadable: {why}", host.name);
            None
        }
    }
}

/// `aterm machines status`: one row per host, from `state/` only.
#[must_use]
pub fn status(env: &Env<'_>, names: &[String], all: bool, by_item: bool) -> ExitCode {
    let inv = match load(env) {
        Ok(inv) => inv,
        Err(code) => return code,
    };
    let hosts = match inventory::select(&inv, names, all || names.is_empty()) {
        Ok(h) => h,
        Err(e) => {
            eprintln!("aterm machines: {e}");
            return ExitCode::from(2);
        }
    };
    let now = env.sys.now();
    let mut tally = Tally::default();
    let mut items: BTreeMap<String, BTreeMap<&'static str, usize>> = BTreeMap::new();
    for host in hosts {
        let Some(state) = read_state(env, host, &mut tally) else {
            continue;
        };
        tally.unreachable |= state.unreachable_since.is_some();
        tally.failed |= state.contact_error.is_some() || state.last_refusal.is_some();
        if let Some(r) = &state.report {
            tally.failed |= r.failed();
            for row in &r.items {
                *items
                    .entry(row.id.clone())
                    .or_default()
                    .entry(outcome_word(row.outcome))
                    .or_default() += 1;
            }
        }
        if env.json {
            println!(
                "{}",
                aterm_json::to_string(&aterm_json::json!({
                    "host": host.name, "uuid": host.uuid, "address": host.address,
                    "last_seen": state.last_seen, "unreachable_since": state.unreachable_since,
                    "contact_error": state.contact_error, "last_refusal": state.last_refusal,
                    "os_changed": os_changed(&state, saved_hello(&state).as_ref()),
                    "report": state.report,
                }))
                .unwrap_or_default()
            );
        } else if !by_item {
            println!("{}", summary(host, &state, now));
        }
    }
    if by_item && !env.json {
        for (id, counts) in items {
            let counts: Vec<String> = counts.iter().map(|(w, n)| format!("{n} {w}")).collect();
            println!("{:<48} {}", shown(&id), counts.join(", "));
        }
    }
    tally.code()
}

/// The report word for an outcome.
const fn outcome_word(outcome: Outcome) -> &'static str {
    match outcome {
        Outcome::Ok => "ok",
        Outcome::Planned => "planned",
        Outcome::Changed => "changed",
        Outcome::Person => "person",
        Outcome::Blocked => "blocked",
        Outcome::NotApplicable => "n/a",
        Outcome::Deferred => "deferred",
        Outcome::Unknown => "unknown",
        Outcome::Failed => "failed",
    }
}

/// `aterm machines outstanding`: one to-do list across the fleet: unreached hosts, hosts
/// that refused contact or a run, failures, person steps, deferred and blocked items, hosts
/// whose macOS changed since their last apply, and, when the newest report is an apply's,
/// the changes it made that wait for a login or a restart.
#[must_use]
pub fn outstanding(env: &Env<'_>) -> ExitCode {
    let inv = match load(env) {
        Ok(inv) => inv,
        Err(code) => return code,
    };
    let now = env.sys.now();
    let mut tally = Tally::default();
    let mut any = false;
    for host in &inv.hosts {
        let Some(state) = read_state(env, host, &mut tally) else {
            any = true;
            continue;
        };
        if let Some(since) = state.unreachable_since {
            tally.unreachable = true;
            any = true;
            println!(
                "{}: unreachable for {}",
                host.name,
                age(now.saturating_sub(since))
            );
        }
        if let Some((since, why)) = &state.contact_error {
            tally.failed = true;
            any = true;
            println!(
                "{}: failing for {}: {}",
                host.name,
                age(now.saturating_sub(*since)),
                shown(why)
            );
        }
        if let Some((at, why)) = &state.last_refusal {
            tally.failed = true;
            any = true;
            println!(
                "{}: the agent refused the last run {} ago: {}",
                host.name,
                age(now.saturating_sub(*at)),
                shown(why)
            );
        }
        if let Some((was, is)) = os_changed(&state, saved_hello(&state).as_ref()) {
            any = true;
            println!(
                "{}: os-changed ({} → {}): apply it again",
                host.name,
                shown(&was),
                shown(&is)
            );
        }
        let Some(report) = &state.report else {
            any = true;
            println!("{}: never planned or applied", host.name);
            continue;
        };
        if let Some(why) = &report.refused {
            tally.failed = true;
            any = true;
            println!(
                "{}: the agent refused the last run: {}",
                host.name,
                shown(why)
            );
        }
        for row in &report.items {
            let pending = match (report.mode, row.outcome, row.effect) {
                (Mode::Apply, Outcome::Changed, Effect::Login) => Some("at the next login"),
                (Mode::Apply, Outcome::Changed, Effect::Restart) => Some("at the next restart"),
                _ => None,
            };
            if let Some(when) = pending {
                any = true;
                println!("{}: takes effect {when}: {}", host.name, shown(&row.id));
                continue;
            }
            if matches!(
                row.outcome,
                Outcome::Ok | Outcome::Changed | Outcome::NotApplicable
            ) {
                continue;
            }
            tally.failed |= row.outcome.is_failure();
            any = true;
            println!(
                "{}: {} {} — {}{}",
                host.name,
                outcome_word(row.outcome),
                shown(&row.id),
                shown(&row.title),
                row.why
                    .as_ref()
                    .map(|w| format!(": {}", shown(w)))
                    .unwrap_or_default()
            );
        }
    }
    if !any {
        println!("nothing outstanding");
    }
    tally.code()
}

// ── plan / apply ────────────────────────────────────────────────────────────────────────

/// Why a host's `hello` did not answer.
#[derive(Debug, Clone, PartialEq, Eq)]
enum HelloError {
    /// `ssh` itself failed (exit status 255), as [`ssh::classify`] read its standard error.
    Ssh(ssh::Failure),
    /// `ssh` did not run to completion (it could not start, or the request outlived its
    /// budget after connecting).
    Run(String),
    /// The host answered, and not with this host's `hello`: its own refusal (another exit
    /// status), an answer that does not parse, or another Mac at the address.
    Answer(String),
}

impl HelloError {
    /// Whether the host simply could not be reached (design §9's exit status 2): only what
    /// [`ssh::Failure::unreachable`] says. A changed host key, a fleet key it no longer
    /// takes, an agent's refusal or another Mac's answer needs the owner: a failure.
    fn unreachable(&self) -> bool {
        matches!(self, Self::Ssh(f) if f.unreachable())
    }

    /// Whether this failure is one Local Network privacy could cause, or says nothing about
    /// it: no route (how the refusal fails a connection, at once: design §1.10), no address,
    /// or an `ssh` that did not run. Anything else reached the LAN: a host that answered at
    /// all, or a connection that waited for an answer and timed out, which the refusal never
    /// lets out.
    const fn blames_nothing_on_the_lan(&self) -> bool {
        matches!(
            self,
            Self::Ssh(ssh::Failure::NoRoute | ssh::Failure::Unresolved) | Self::Run(_)
        )
    }

    /// One line for the host's row.
    fn why(&self) -> String {
        match self {
            Self::Ssh(f) => f.why(),
            Self::Run(why) | Self::Answer(why) => why.clone(),
        }
    }
}

/// What phase 1 learned about one host.
struct Reached<'h> {
    host: &'h Host,
    hello: Result<Hello, HelloError>,
}

/// The row for a host whose `hello` did not answer, and what it adds to `tally`: exit status
/// 2 only when it could not be reached, a failure otherwise.
fn not_reached(tally: &mut Tally, host: &Host, why: &HelloError) {
    if why.unreachable() {
        tally.unreachable = true;
        println!("{:<16} unreachable: {}", host.name, shown(&why.why()));
    } else {
        tally.failed = true;
        println!("{:<16} failed: {}", host.name, shown(&why.why()));
    }
}

/// This controller's own agent build: the executable `enroll` installs and `apply`
/// upgrades hosts to.
struct OwnAgent {
    exe: PathBuf,
    sha256: String,
}

impl OwnAgent {
    fn of(sys: &dyn System) -> Result<Self, String> {
        let exe = enroll::own_exe()?;
        let sha256 = sys
            .sha256(&exe)
            .map_err(|e| format!("{}: {e}", exe.display()))?;
        Ok(Self { exe, sha256 })
    }

    /// Whether the host answering `hello` runs another build. An agent too old to say
    /// which build it runs is upgraded too: the controller's build is the fleet's.
    fn differs(&self, hello: &Hello) -> bool {
        !hello.agent_sha256.eq_ignore_ascii_case(&self.sha256)
    }
}

/// The agent archive one command uploads to the hosts it upgrades, built once.
struct AgentArchive<'s> {
    archive: enroll::Archive,
    /// `app-zip` when the bundle verifies as aterm's Developer ID here, else `dev-zip`.
    kind: &'static str,
    _scratch: crate::port::Scratch<'s>,
}

/// One host's work for phase 3.
struct Job<'h> {
    host: &'h Host,
    hello: Hello,
    needed: Vec<artifacts::Needed>,
    /// The upgrade envelope that goes first, when the host's agent is another build.
    upgrade: Option<Vec<u8>>,
    wire: Vec<u8>,
}

/// What one command resolves and checks once for every host it builds an envelope for.
#[derive(Default)]
struct Resolved {
    /// `[aterm]`, resolved to one release for the whole command.
    aterm: artifacts::AtermMemo,
    /// The artifacts whose `cache/` copy this command already checked, by SHA-256.
    ensured: BTreeSet<String>,
}

/// The hosts a fleet verb names, or the exit code once it said why not. `--all` over an
/// inventory with no hosts is nothing to do; a name that matches no host is a refused
/// command line.
fn fleet_hosts<'i>(
    inv: &'i Inventory,
    names: &[String],
    all: bool,
    verb: &str,
) -> Result<Vec<&'i Host>, ExitCode> {
    match inventory::select(inv, names, all) {
        Ok(h) if !h.is_empty() => Ok(h),
        Ok(_) => {
            println!("machines.toml has no hosts yet: `aterm machines help enroll`");
            Err(ExitCode::SUCCESS)
        }
        Err(e) => {
            eprintln!("aterm machines {verb}: {e}");
            Err(ExitCode::from(2))
        }
    }
}

/// `aterm machines plan|apply`.
#[must_use]
pub fn plan_or_apply(
    env: &Env<'_>,
    names: &[String],
    all: bool,
    mode: Mode,
    now: bool,
) -> ExitCode {
    let (action, verb) = match mode {
        Mode::Plan => (Action::Plan, "plan"),
        Mode::Apply => (Action::Apply, "apply"),
    };
    let inv = match load(env) {
        Ok(inv) => inv,
        Err(code) => return code,
    };
    let hosts = match fleet_hosts(&inv, names, all, verb) {
        Ok(h) => h,
        Err(code) => return code,
    };
    let parallel = parallel(env, &inv);
    let mut tally = Tally::default();
    let own = match OwnAgent::of(env.sys) {
        Ok(own) => own,
        Err(why) => {
            eprintln!("aterm machines: {why}");
            return ExitCode::FAILURE;
        }
    };

    // Phase 1.
    let reached = hello_sweep(env, &hosts, parallel);
    if let Some(stop) = local_network_wall(&reached) {
        eprintln!("{stop}");
        return ExitCode::FAILURE;
    }

    // Phase 2: one envelope per reachable host (two when its agent is upgraded first),
    // built in order, then signed together.
    let principals = principals(&inv, env);
    let mut resolved = Resolved::default();
    let mut agent_archive: Option<AgentArchive<'_>> = None;
    let mut built: Vec<(&Host, Hello, Vec<artifacts::Needed>, bool)> = Vec::new();
    let mut bodies: Vec<Envelope> = Vec::new();
    for r in reached {
        let hello = match r.hello {
            Ok(h) => h,
            Err(why) => {
                not_reached(&mut tally, r.host, &why);
                continue;
            }
        };
        // A run in progress would refuse the envelope (`busy`) after it raised the floor.
        if let Some(run) = &hello.busy {
            tally.failed = true;
            println!(
                "{:<16} busy run={}: nothing was sent; `aterm machines attach {}` follows it",
                r.host.name,
                shown(run),
                r.host.name
            );
            continue;
        }
        let upgrade = mode == Mode::Apply && own.differs(&hello);
        if upgrade && agent_archive.is_none() {
            match build_agent_archive(env.sys, &own) {
                Ok(a) => agent_archive = Some(a),
                Err(why) => {
                    eprintln!("aterm machines: the agent upgrade archive: {why}");
                    return ExitCode::FAILURE;
                }
            }
        }
        let floor = floor_of(&hello, &principals);
        let mut host_bodies = Vec::new();
        if let (true, Some(a)) = (upgrade, &agent_archive) {
            match upgrade_body(env, r.host, floor, a) {
                Ok(body) => host_bodies.push(body),
                Err(why) => {
                    tally.failed = true;
                    println!("{:<16} not sent: {why}", r.host.name);
                    continue;
                }
            }
        }
        match baseline_body(env, r.host, &hello, floor, action, now, &mut resolved) {
            Ok((body, needed)) => {
                host_bodies.push(body);
                bodies.extend(host_bodies);
                built.push((r.host, hello, needed, upgrade));
            }
            Err(why) => {
                tally.failed = true;
                println!("{:<16} not sent: {why}", r.host.name);
            }
        }
    }
    let mut wires = match seal_all(env, &bodies, &env.paths.signing_key()) {
        Ok(w) => w.into_iter(),
        Err(why) => {
            eprintln!("aterm machines: {why}");
            return ExitCode::FAILURE;
        }
    };
    let mut jobs: Vec<Job<'_>> = Vec::with_capacity(built.len());
    for (host, hello, needed, upgrade) in built {
        let upgrade = if upgrade { wires.next() } else { None };
        let Some(wire) = wires.next() else {
            break;
        };
        jobs.push(Job {
            host,
            hello,
            needed,
            upgrade,
            wire,
        });
    }

    // Phase 3.
    let outcomes = ssh::fan_out(&jobs, parallel, |job| {
        send(env, job, agent_archive.as_ref().map(|a| &a.archive))
    });
    for (job, outcome) in jobs.iter().zip(outcomes) {
        match outcome {
            Ok(mut report) => {
                if mode == Mode::Plan && own.differs(&job.hello) {
                    report
                        .items
                        .insert(0, pending_upgrade_row(&job.hello, &own));
                }
                tally.failed |= report.failed();
                print_report(env, job.host, &report);
                after_run(env, &inv, job.host, report, Source::Baseline);
                // The saved hello names the agent that answered before the upgrade; greet
                // the new one so `status` shows the build the host now runs.
                if job.upgrade.is_some() {
                    let _ = hello(env, job.host);
                }
            }
            Err(why) => {
                tally.failed = true;
                println!("{:<16} {}", job.host.name, shown(&why));
            }
        }
    }
    tally.code()
}

/// `--parallel`, else `[controller] parallel`, else 8.
fn parallel(env: &Env<'_>, inv: &Inventory) -> usize {
    env.parallel.unwrap_or_else(|| inv.parallel()).max(1)
}

/// `hello` every host, `parallel` at a time.
fn hello_sweep<'h>(env: &Env<'_>, hosts: &[&'h Host], parallel: usize) -> Vec<Reached<'h>> {
    let hellos = ssh::fan_out(hosts, parallel, |host| hello(env, host));
    hosts
        .iter()
        .zip(hellos)
        .map(|(host, hello)| Reached { host, hello })
        .collect()
}

/// One `hello`, recorded in the host's state: an answer as the last contact; a host that
/// could not be reached as when it was first missed; any other failure (a changed host key,
/// another Mac at the address) as the contact error `status` and `outstanding` show. A state
/// file that cannot be read is left as it is, never replaced by a fresh one (it holds the
/// host's GitHub key ids).
fn hello(env: &Env<'_>, host: &Host) -> Result<Hello, HelloError> {
    let answered = ask_hello(env, host);
    if let Ok(mut state) = inventory::load_state(env.sys, &env.paths, &host.uuid) {
        let now = env.sys.now();
        match &answered {
            Ok(hello) => {
                state.last_seen = now;
                state.unreachable_since = None;
                state.contact_error = None;
                state.hello = aterm_json::to_string(hello).ok();
            }
            Err(why) if why.unreachable() => {
                if state.unreachable_since.is_none() {
                    state.unreachable_since = Some(now);
                }
            }
            Err(why) => {
                state.unreachable_since = None;
                let since = state
                    .contact_error
                    .as_ref()
                    .map_or(now, |(since, _)| *since);
                state.contact_error = Some((since, why.why()));
            }
        }
        let _ = inventory::save_state(env.sys, &env.paths, &host.uuid, &state);
    }
    answered
}

fn ask_hello(env: &Env<'_>, host: &Host) -> Result<Hello, HelloError> {
    let target = Target::of(host);
    let ran = ssh::request(env.sys, &env.paths, &target, "hello", None, HELLO_BUDGET)
        .map_err(|e| HelloError::Run(e.to_string()))?;
    if !ran.success() {
        return Err(
            ssh::classify(&ran).map_or_else(|| HelloError::Answer(ran.describe()), HelloError::Ssh)
        );
    }
    let hello: Hello = aterm_json::from_str(ran.out().trim())
        .map_err(|e| HelloError::Answer(format!("hello did not parse: {e}")))?;
    if hello.uuid != host.uuid {
        return Err(HelloError::Answer(format!(
            "this is not {}: it answers as {} (serial {})",
            host.name, hello.uuid, hello.serial_number
        )));
    }
    Ok(hello)
}

/// The one line to print instead of a row per host (design §9): every host failed the way
/// macOS's Local Network privacy fails this process ([`HelloError::blames_nothing_on_the_lan`]),
/// and one at least found no route to a local address. A host that answered, or one that
/// timed out or is down (what a sleeping Mac's address reads before and after its no-route
/// window: [`ssh::HOST_DOWN`]), shows the LAN is reachable, so one Mac a router calls
/// unreachable never stops the rest. Every host inside that window at once still reads as
/// the refusal: no reading tells the two apart.
fn local_network_wall(reached: &[Reached<'_>]) -> Option<&'static str> {
    let no_route = reached.iter().any(|r| {
        matches!(&r.hello, Err(HelloError::Ssh(ssh::Failure::NoRoute)))
            && ssh::is_local_address(&r.host.address)
    });
    let none_reached = reached
        .iter()
        .all(|r| matches!(&r.hello, Err(why) if why.blames_nothing_on_the_lan()));
    (no_route && none_reached).then_some(ssh::LOCAL_NETWORK_ROW)
}

/// The resolved baseline text for `host`: its file, placeholders filled, `users` defaulted
/// from the inventory, and `[aterm]` pinned to the release this command resolved.
fn resolved_baseline(
    env: &Env<'_>,
    host: &Host,
    hello: &Hello,
    resolved: &mut Resolved,
) -> Result<String, String> {
    let name = host.baseline.as_deref().unwrap_or("default");
    if name.contains('/') || name.contains("..") {
        return Err(format!("baseline name {name:?} is not a plain name"));
    }
    let path = env.paths.baseline(name);
    let text = env
        .sys
        .read_file(&path)
        .map_err(|e| format!("{}: {e}", path.display()))?;
    let text = String::from_utf8(text).map_err(|_| format!("{} is not UTF-8", path.display()))?;
    let text = crate::baseline::fill(
        &text,
        &[
            ("name", host.name.as_str()),
            ("serial", hello.serial_number.as_str()),
            ("uuid", host.uuid.as_str()),
        ],
    )?;
    let text = default_users(&text, host)?;
    let text = artifacts::resolve_aterm_once(env.sys, &env.paths, &text, &mut resolved.aterm)?;
    crate::baseline::parse(&text)?;
    Ok(text)
}

/// Set `users` from the inventory when the baseline leaves it out (design §5).
fn default_users(text: &str, host: &Host) -> Result<String, String> {
    let mut doc: aterm_toml::Value =
        aterm_toml::from_str(text).map_err(|e| format!("baseline: {e}"))?;
    let Some(table) = doc.as_table_mut() else {
        return Err("baseline: not a table".into());
    };
    if table.get("users").is_none() {
        let users = if host.users.is_empty() {
            vec![host.user.clone()]
        } else {
            host.users.clone()
        };
        table.insert(
            "users".into(),
            aterm_toml::Value::Array(users.into_iter().map(aterm_toml::Value::String).collect()),
        );
    }
    aterm_toml::to_string(&doc).map_err(|e| format!("baseline: {e}"))
}

/// The highest serial floor `hello` reports for any of `principals` (the names this
/// controller's signing key has had: a host that has not taken a rekey yet still knows it
/// by the old one).
fn floor_of(hello: &Hello, principals: &[String]) -> u64 {
    hello
        .floors
        .iter()
        .filter(|f| principals.contains(&f.principal))
        .map(|f| f.serial)
        .max()
        .unwrap_or(0)
}

/// An envelope for `host` with the next serial over `floor`; the caller fills the payload.
fn body(
    env: &Env<'_>,
    host: &Host,
    floor: u64,
    action: Action,
    disrupt: Disrupt,
) -> Result<Envelope, String> {
    let serial = inventory::next_serial(env.sys, &env.paths, floor)?;
    let issued = env.sys.now();
    Ok(Envelope {
        schema: crate::SCHEMA,
        host: host.uuid.clone(),
        host_name: host.name.clone(),
        serial,
        issued,
        expires: issued + ENVELOPE_LIFETIME,
        action,
        disrupt,
        idle_secs: 900,
        baseline: None,
        artifacts: Vec::new(),
        exec: None,
        upgrade: None,
        rekey: None,
    })
}

/// One host's plan or apply envelope, and the artifacts it needs uploaded.
fn baseline_body(
    env: &Env<'_>,
    host: &Host,
    hello: &Hello,
    floor: u64,
    action: Action,
    now: bool,
    resolved: &mut Resolved,
) -> Result<(Envelope, Vec<artifacts::Needed>), String> {
    let text = resolved_baseline(env, host, hello, resolved)?;
    let baseline = crate::baseline::parse(&text)?;
    let needed = artifacts::needed(&baseline);
    for item in &needed {
        if !resolved.ensured.contains(&item.sha256) {
            artifacts::ensure(env.sys, &env.paths, item)?;
            resolved.ensured.insert(item.sha256.clone());
        }
    }
    let disrupt = if now { Disrupt::Now } else { Disrupt::WhenIdle };
    let mut envelope = body(env, host, floor, action, disrupt)?;
    envelope.baseline = Some(text);
    envelope.artifacts = needed
        .iter()
        .map(|n| ArtifactRef {
            sha256: n.sha256.clone(),
            name: n.name.clone(),
        })
        .collect();
    Ok((envelope, needed))
}

/// The archive of this controller's own agent, in a fresh private scratch directory, and
/// the kind its upgrade envelope names.
fn build_agent_archive<'s>(
    sys: &'s dyn System,
    own: &OwnAgent,
) -> Result<AgentArchive<'s>, String> {
    let scratch = envelope::fresh_scratch(sys, "upgrade")?;
    let archive = enroll::build_archive(sys, &own.exe, scratch.path())?;
    let kind = upgrade_kind(sys, &own.exe, &archive);
    Ok(AgentArchive {
        archive,
        kind,
        _scratch: scratch,
    })
}

/// The requirement an `app-zip` upgrade's bundle meets: the one the agent checks it against
/// (`agent::job`), Developer ID team A66A9P66Z7.
const APP_ZIP_REQUIREMENT: &str =
    "=anchor apple generic and certificate leaf[subject.OU] = \"A66A9P66Z7\"";

/// `app-zip` when the bundle `exe` runs from verifies here as aterm's Developer ID
/// (`codesign --verify --deep --strict -R`, as the agent will check it), else `dev-zip`,
/// which the agent trusts on the signed hash alone, as enrollment did. A locally built or
/// renamed bundle is a `dev-zip`, so the host never refuses an upgrade enrollment accepted.
fn upgrade_kind(sys: &dyn System, exe: &Path, archive: &enroll::Archive) -> &'static str {
    if !archive.bundle {
        return "dev-zip";
    }
    let (bundle, _, _) = enroll::payload_of(exe);
    let verified = sys
        .run(
            &Cmd::new("/usr/bin/codesign")
                .args(["--verify", "--deep", "--strict", "-R", APP_ZIP_REQUIREMENT])
                .arg(bundle.as_os_str())
                .budget(Duration::from_secs(300)),
        )
        .is_ok_and(|ran| ran.success());
    if verified { "app-zip" } else { "dev-zip" }
}

/// The `upgrade` envelope that puts `agent`'s archive in place of the host's agent (design
/// §4.5).
fn upgrade_body(
    env: &Env<'_>,
    host: &Host,
    floor: u64,
    agent: &AgentArchive<'_>,
) -> Result<Envelope, String> {
    let mut envelope = body(env, host, floor, Action::Upgrade, Disrupt::Now)?;
    envelope.artifacts = vec![ArtifactRef {
        sha256: agent.archive.sha256.clone(),
        name: "aterm agent".into(),
    }];
    envelope.upgrade = Some(Upgrade {
        sha256: agent.archive.sha256.clone(),
        kind: agent.kind.into(),
        version: env!("CARGO_PKG_VERSION").into(),
    });
    Ok(envelope)
}

/// `0.105.0 (ab62ff892b56)`: a version and the start of its executable's SHA-256.
fn build_name(version: &str, sha256: &str) -> String {
    match sha256.get(..12) {
        Some(short) => format!("{version} ({short})"),
        None => format!("{version} (build not reported)"),
    }
}

/// The row a plan shows for a host whose agent `apply` will upgrade first.
fn pending_upgrade_row(hello: &Hello, own: &OwnAgent) -> ItemReport {
    ItemReport {
        id: "agent.upgrade".into(),
        family: "agent".into(),
        title: "the controller's agent build".into(),
        outcome: Outcome::Planned,
        from: Some(build_name(&hello.agent_version, &hello.agent_sha256)),
        to: Some(build_name(env!("CARGO_PKG_VERSION"), &own.sha256)),
        why: Some("apply upgrades the agent first".into()),
        revert: None,
        effect: Effect::Now,
        detail: None,
    }
}

/// The names this controller's signing key has, the floors `hello` reports are kept under:
/// `[controller] principal` when the inventory sets one; else every principal
/// `allowed_signers` (then `allowed_signers.previous`, the list a rekey replaced) gives the
/// signing key's public half; and only when neither names it, `signing@<LocalHostName>`, the
/// name `init` wrote. A renamed controller, or a directory restored onto another Mac, still
/// finds its floors.
fn principals(inv: &Inventory, env: &Env<'_>) -> Vec<String> {
    if let Some(p) = &inv.controller.principal {
        return vec![p.clone()];
    }
    let public = public_half(&env.paths.signing_key());
    let mut names = Vec::new();
    for list in [env.paths.allowed_signers(), previous_signers(&env.paths)] {
        if let Ok(found) = principals_of_key(env.sys, &public, &list) {
            for name in found {
                if !names.contains(&name) {
                    names.push(name);
                }
            }
        }
    }
    if names.is_empty() {
        names.push(format!("signing@{}", controller_name(env.sys)));
    }
    names
}

/// `<key>.pub`: where `ssh-keygen` puts a private key's public half.
fn public_half(key: &Path) -> PathBuf {
    let mut public = key.as_os_str().to_owned();
    public.push(".pub");
    PathBuf::from(public)
}

/// `allowed_signers.previous`: the list the last rekey replaced on the controller.
fn previous_signers(paths: &Paths) -> PathBuf {
    paths.root.join("allowed_signers.previous")
}

/// Whether ssh-agent (at [`Env::agent_sock`]) holds `key`: `ssh-add -T` on its public half
/// signs a test message through the agent, so it answers without a prompt either way.
fn agent_holds(env: &Env<'_>, key: &Path) -> bool {
    let Some(sock) = &env.agent_sock else {
        return false;
    };
    env.sys
        .run(&ssh_add(sock, &["-T"]).arg(public_half(key).as_os_str()))
        .is_ok_and(|r| r.success())
}

/// How `key` signs this command's batch: unattended with no passphrase; through ssh-agent
/// when the key has one and the agent already holds it (no prompt: a key loaded once is never
/// asked for again); otherwise on the terminal, asked once.
fn signing(env: &Env<'_>, key: &Path) -> Result<Signing, String> {
    match envelope::needs_passphrase(env.sys, key) {
        Read::Value(false) => Ok(Signing::Unattended),
        Read::Value(true) if agent_holds(env, key) => Ok(Signing::Agent),
        Read::Value(true) if env.tty => Ok(Signing::Terminal),
        Read::Value(true) => Err(format!(
            "{} has a passphrase, ssh-agent does not hold it, and standard input is not a \
             terminal to ask it on",
            key.display()
        )),
        Read::Absent => Err(format!("{}: no key", key.display())),
        Read::Unusable(why) => Err(format!("{}: {why}", key.display())),
    }
}

/// Sign `bodies` with `key` in one `ssh-keygen` run: the wires, in order.
fn seal_all(env: &Env<'_>, bodies: &[Envelope], key: &Path) -> Result<Vec<Vec<u8>>, String> {
    if bodies.is_empty() {
        return Ok(Vec::new());
    }
    let how = signing(env, key)?;
    seal_with(env.sys, bodies, key, how)
}

fn seal_with(
    sys: &dyn System,
    bodies: &[Envelope],
    key: &Path,
    how: Signing,
) -> Result<Vec<Vec<u8>>, String> {
    let scratch = envelope::fresh_scratch(sys, "seal")?;
    envelope::seal_batch(sys, bodies, key, how, scratch.path())
}

/// Upload `path` as artifact `sha256` unless the host's cache holds it.
fn put(
    env: &Env<'_>,
    target: &Target,
    hello: &Hello,
    sha256: &str,
    name: &str,
    path: &Path,
) -> Result<(), String> {
    if hello.cached.iter().any(|c| c.eq_ignore_ascii_case(sha256)) {
        return Ok(());
    }
    let ran = ssh::request(
        env.sys,
        &env.paths,
        target,
        &format!("put {sha256}"),
        Some(path),
        PUT_BUDGET,
    )
    .map_err(|e| format!("upload of {name} failed: {e}"))?;
    if ran.success() {
        Ok(())
    } else {
        Err(format!("upload of {name} failed: {}", ran.describe()))
    }
}

/// Send one signed envelope with `apply` and read the run's stream, `budget` long. A stream
/// that ends without a report after the agent named its run says `detached run=<id>`; one
/// that never named a run (ssh or sudo failed first) has no run to attach to.
fn submit(env: &Env<'_>, host: &Host, wire: &[u8], budget: Duration) -> Result<HostReport, String> {
    let staged = crate::port::staging_file(env.sys, wire)
        .map_err(|e| format!("could not stage the envelope: {e}"))?;
    let ran = ssh::request(env.sys, &env.paths, &Target::of(host), "apply", Some(staged.path()), budget)
        .map_err(|e| format!("the run did not finish within this connection ({e}); if it started, `aterm machines attach {}` follows it", host.name))?;
    let (report, started) = read_stream(&ran.out(), env.sys.now());
    report.ok_or_else(|| match started {
        Some(run) => format!(
            "detached run={run} ({}); `aterm machines attach {} {run}` follows it",
            ran.describe(),
            host.name
        ),
        None => format!("the agent answered no report: {}", ran.describe()),
    })
}

/// Phase 3 for one host: the agent upgrade when there is one, the uploads, the run.
fn send(
    env: &Env<'_>,
    job: &Job<'_>,
    archive: Option<&enroll::Archive>,
) -> Result<HostReport, String> {
    let target = Target::of(job.host);
    let mut first: Vec<ItemReport> = Vec::new();
    if let (Some(wire), Some(archive)) = (&job.upgrade, archive) {
        put(
            env,
            &target,
            &job.hello,
            &archive.sha256,
            "the aterm agent",
            &archive.path,
        )?;
        let report = submit(env, job.host, wire, UPGRADE_BUDGET).map_err(|why| {
            format!("the agent upgrade did not finish, so the baseline was not sent: {why}")
        })?;
        if let Some(why) = &report.refused {
            return Err(format!(
                "the agent upgrade was refused, so the baseline was not sent: {why}"
            ));
        }
        if report.failed() {
            let why: Vec<String> = report.items.iter().filter_map(|r| r.why.clone()).collect();
            return Err(format!(
                "the agent upgrade failed, so the baseline was not sent: {}",
                why.join("; ")
            ));
        }
        first = report.items;
    }
    for item in &job.needed {
        put(
            env,
            &target,
            &job.hello,
            &item.sha256,
            &item.name,
            &env.paths.cached(&item.sha256),
        )?;
    }
    let mut report = submit(env, job.host, &job.wire, APPLY_BUDGET)?;
    if !first.is_empty() {
        first.append(&mut report.items);
        report.items = first;
    }
    Ok(report)
}

/// The report at the end of an `apply` or `attach` stream (a refusal becomes a report with
/// `refused` set, stamped `now`), and the run the stream's `started` line named.
fn read_stream(out: &str, now: u64) -> (Option<HostReport>, Option<String>) {
    let mut items: Vec<ItemReport> = Vec::new();
    let mut started: Option<String> = None;
    for line in out.lines().filter(|l| !l.trim().is_empty()) {
        match aterm_json::from_str::<Line>(line) {
            Ok(Line::Report(report)) => return (Some(*report), started),
            Ok(Line::Item(row)) => items.push(row),
            Ok(Line::Refused { why }) => {
                let report = HostReport {
                    schema: crate::SCHEMA,
                    mode: Mode::Plan,
                    serial: 0,
                    key_id: String::new(),
                    run_id: String::new(),
                    started: now,
                    finished: now,
                    facts: crate::model::HostFacts::default(),
                    items,
                    refused: Some(why),
                };
                return (Some(report), started);
            }
            Ok(Line::Started { run }) if crate::agent::is_run_id(&run) => started = Some(run),
            Ok(Line::Started { .. }) | Err(_) => {}
        }
    }
    (None, started)
}

/// Print one host's report: a summary line, then each row that needs attention.
fn print_report(env: &Env<'_>, host: &Host, report: &HostReport) {
    if env.json {
        println!(
            "{}",
            aterm_json::to_string(&aterm_json::json!({"host": host.name, "report": report}))
                .unwrap_or_default()
        );
        return;
    }
    let state = HostState {
        report: Some(report.clone()),
        last_seen: env.sys.now(),
        ..HostState::default()
    };
    println!("{}", summary(host, &state, env.sys.now()));
    for row in &report.items {
        if matches!(row.outcome, Outcome::Ok | Outcome::NotApplicable) {
            continue;
        }
        println!("    {}", row_line(row));
    }
}

/// One report row as `print_report` shows it: the outcome, the item, the change and why,
/// each value the host sent made safe to print.
fn row_line(row: &ItemReport) -> String {
    let change = match (&row.from, &row.to) {
        (Some(from), Some(to)) if row.outcome != Outcome::Person => {
            format!(" ({} → {})", shown(from), shown(to))
        }
        _ => String::new(),
    };
    let why = row
        .why
        .as_ref()
        .map(|w| format!(": {}", shown(w)))
        .unwrap_or_default();
    format!(
        "{:<9} {}{change}{why}",
        outcome_word(row.outcome),
        shown(&row.id)
    )
}

/// Where a report [`after_run`] records came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Source {
    /// A plan or apply this command sent.
    Baseline,
    /// A run `attach` followed, which may have been any action.
    Followed,
}

/// Whether `report` is a plan or apply run's: an `exec`, `rekey` or `upgrade` run reports
/// only its own row (`exec`, `agent.*`).
fn is_baseline_report(report: &HostReport) -> bool {
    report.items.is_empty()
        || !report
            .items
            .iter()
            .all(|r| r.family == "exec" || r.family == "agent")
}

/// Record a run in the host's state. A plan or apply report becomes the host's report and
/// clears an older refusal; an apply's also registers the GitHub keys it carries. A refusal
/// is kept beside the report, never in its place, and another action's report (`exec`,
/// `upgrade`, `rekey`) leaves it alone: either would otherwise stand for the host, its
/// failures and person steps gone. A state file that cannot be read is left alone: it holds
/// the host's GitHub key ids.
fn after_run(env: &Env<'_>, inv: &Inventory, host: &Host, report: HostReport, source: Source) {
    let mut state = match inventory::load_state(env.sys, &env.paths, &host.uuid) {
        Ok(s) => s,
        Err(why) => {
            eprintln!("aterm machines: {}'s report is not saved: {why}", host.name);
            return;
        }
    };
    let now = env.sys.now();
    state.last_seen = now;
    state.unreachable_since = None;
    state.contact_error = None;
    if let Some(why) = &report.refused {
        state.last_refusal = Some((now, why.clone()));
    } else if source == Source::Baseline || is_baseline_report(&report) {
        state.last_refusal = None;
        if report.mode == Mode::Apply {
            state.applied_build = Some(report.facts.os_build.clone());
            if let Some(github) = &inv.github {
                register_keys(env, &github.account, host, &report, &mut state);
            } else if report
                .items
                .iter()
                .any(|r| r.id.starts_with("github.key@") && r.detail.is_some())
            {
                println!(
                    "    github    keys made but not registered: machines.toml names no \
                     [github] account"
                );
            }
        }
        state.report = Some(report);
    }
    if let Err(e) = inventory::save_state(env.sys, &env.paths, &host.uuid, &state) {
        eprintln!("aterm machines: could not save {}'s state: {e}", host.name);
    }
}

/// The key a recorded GitHub key id registered, when it was recorded with one.
fn key_text_of(state: &HostState, id: u64) -> Option<&str> {
    state
        .github_key_text
        .iter()
        .find(|(i, _)| *i == id)
        .map(|(_, text)| text.as_str())
}

/// Register the public keys an apply reported (`github.key@<user>` rows) with the machine
/// user `account`, each by the key itself, never by its user alone: a user whose key is new
/// (a reset home, a deleted key) gets it registered and the key it replaced deleted; a key
/// already registered is not sent again; and a key `github revoke` deleted is never
/// registered again, whatever a report carries.
fn register_keys(
    env: &Env<'_>,
    account: &str,
    host: &Host,
    report: &HostReport,
    state: &mut HostState,
) {
    let mut reported: Vec<(String, String)> = Vec::new();
    for row in &report.items {
        let (Some(user), Some(line)) = (row.id.strip_prefix("github.key@"), &row.detail) else {
            continue;
        };
        let Some((kind, body)) = crate::items::github::key_material(line) else {
            println!(
                "    github    {}'s key is not an OpenSSH public key; not registered",
                shown(user)
            );
            continue;
        };
        let text = format!("{kind} {body}");
        if state.github_revoked.contains(&text) {
            println!(
                "    github    {}'s key was revoked (`aterm machines github revoke`); not \
                 registered again",
                shown(user)
            );
            continue;
        }
        reported.push((user.to_owned(), text));
    }
    let fresh: Vec<(String, String)> = reported
        .iter()
        .filter(|(user, text)| {
            !state
                .github_keys
                .iter()
                .any(|(u, id)| u == user && key_text_of(state, *id) == Some(text.as_str()))
        })
        .cloned()
        .collect();
    if !fresh.is_empty() {
        match super::github::register(env.sys, account, &host.name, &fresh) {
            Ok(done) => {
                for r in done {
                    let Some((_, text)) = fresh.iter().find(|(u, _)| *u == r.user) else {
                        continue;
                    };
                    println!(
                        "    github    registered {}'s key on {account} (id {})",
                        shown(&r.user),
                        r.id
                    );
                    if !state.github_keys.contains(&(r.user.clone(), r.id)) {
                        state.github_keys.push((r.user.clone(), r.id));
                    }
                    state.github_key_text.retain(|(id, _)| *id != r.id);
                    state.github_key_text.push((r.id, text.clone()));
                }
            }
            Err(why) => println!("    github    not registered: {}", shown(&why)),
        }
    }
    // A user's other ids, once the key it reports now is registered: the keys it replaced.
    let stale: Vec<u64> = state
        .github_keys
        .iter()
        .filter(|(user, id)| {
            reported.iter().any(|(u, text)| {
                u == user
                    && key_text_of(state, *id) != Some(text.as_str())
                    && state
                        .github_keys
                        .iter()
                        .any(|(u2, id2)| u2 == u && key_text_of(state, *id2) == Some(text.as_str()))
            })
        })
        .map(|(_, id)| *id)
        .collect();
    if stale.is_empty() {
        return;
    }
    match super::github::revoke(env.sys, account, &stale) {
        Ok(_) => {
            println!(
                "    github    deleted {} replaced key(s) from {account}",
                stale.len()
            );
            state.github_keys.retain(|(_, id)| !stale.contains(id));
            state.github_key_text.retain(|(id, _)| !stale.contains(id));
        }
        // Still recorded, so the next apply, `github revoke` or `unenroll` deletes them.
        Err(why) => println!(
            "    github    the replaced key(s) are still registered: {}",
            shown(&why)
        ),
    }
}

// ── attach / run ────────────────────────────────────────────────────────────────────────

/// The one host `name` selects.
fn one_host<'i>(inv: &'i Inventory, name: &str, verb: &str) -> Result<&'i Host, ExitCode> {
    match inventory::select(inv, &[name.to_owned()], false) {
        Ok(h) if h.len() == 1 => Ok(h[0]),
        Ok(_) => {
            eprintln!("aterm machines {verb}: {name:?} names more than one host");
            Err(ExitCode::from(2))
        }
        Err(e) => {
            eprintln!("aterm machines {verb}: {e}");
            Err(ExitCode::from(2))
        }
    }
}

/// `aterm machines attach <host> [<run>]`: follow a run that outlived its connection.
#[must_use]
pub fn attach(env: &Env<'_>, name: &str, run: Option<&str>) -> ExitCode {
    let inv = match load(env) {
        Ok(inv) => inv,
        Err(code) => return code,
    };
    let host = match one_host(&inv, name, "attach") {
        Ok(h) => h,
        Err(code) => return code,
    };
    let run = match run {
        Some(r) => r.to_owned(),
        None => match hello(env, host) {
            Ok(h) => match h.busy.or(h.last_run) {
                Some(r) => r,
                None => {
                    eprintln!("aterm machines attach: {} has no run to follow", host.name);
                    return ExitCode::from(2);
                }
            },
            Err(why) => {
                eprintln!(
                    "aterm machines attach: {}: {}",
                    host.name,
                    shown(&why.why())
                );
                return if why.unreachable() {
                    ExitCode::from(2)
                } else {
                    ExitCode::FAILURE
                };
            }
        },
    };
    if !run.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-') {
        eprintln!("aterm machines attach: {run:?} is not a run id");
        return ExitCode::from(2);
    }
    let ran = match ssh::request(
        env.sys,
        &env.paths,
        &Target::of(host),
        &format!("attach {run}"),
        None,
        APPLY_BUDGET,
    ) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("aterm machines attach: {}: {e}", host.name);
            return ExitCode::FAILURE;
        }
    };
    let Some(report) = read_stream(&ran.out(), env.sys.now()).0 else {
        eprintln!(
            "aterm machines attach: {}: no report ({})",
            host.name,
            shown(&ran.describe())
        );
        return ExitCode::FAILURE;
    };
    let failed = report.failed();
    print_report(env, host, &report);
    after_run(env, &inv, host, report, Source::Followed);
    if failed {
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    }
}

/// `aterm machines run <host…|--all> [--user <u>] -- <argv>`: one signed command, no shell
/// (design §4.6), as root unless `user` names an account.
#[must_use]
pub fn run(
    env: &Env<'_>,
    names: &[String],
    all: bool,
    user: Option<&str>,
    argv: &[String],
) -> ExitCode {
    let Some(program) = argv.first() else {
        eprintln!("aterm machines run: name a command after --");
        return ExitCode::from(2);
    };
    if !program.starts_with('/') {
        eprintln!(
            "aterm machines run: {program:?} must be an absolute path (no shell, no PATH lookup)"
        );
        return ExitCode::from(2);
    }
    let inv = match load(env) {
        Ok(inv) => inv,
        Err(code) => return code,
    };
    let hosts = match fleet_hosts(&inv, names, all, "run") {
        Ok(h) => h,
        Err(code) => return code,
    };
    let parallel = parallel(env, &inv);
    let reached = hello_sweep(env, &hosts, parallel);
    if let Some(stop) = local_network_wall(&reached) {
        eprintln!("{stop}");
        return ExitCode::FAILURE;
    }
    let principals = principals(&inv, env);
    let mut tally = Tally::default();
    let mut targets: Vec<&Host> = Vec::new();
    let mut bodies = Vec::new();
    for r in reached {
        let hello = match r.hello {
            Ok(h) => h,
            Err(why) => {
                not_reached(&mut tally, r.host, &why);
                continue;
            }
        };
        let exec = Exec {
            argv: argv.to_vec(),
            user: user.map(str::to_owned),
            budget_secs: 3600,
        };
        match exec_body(env, r.host, floor_of(&hello, &principals), exec) {
            Ok(b) => {
                bodies.push(b);
                targets.push(r.host);
            }
            Err(why) => {
                eprintln!("aterm machines run: {why}");
                return ExitCode::FAILURE;
            }
        }
    }
    let wires = match seal_all(env, &bodies, &env.paths.signing_key()) {
        Ok(w) => w,
        Err(why) => {
            eprintln!("aterm machines run: {why}");
            return ExitCode::FAILURE;
        }
    };
    let jobs: Vec<(&Host, Vec<u8>)> = targets.into_iter().zip(wires).collect();
    let outcomes = ssh::fan_out(&jobs, parallel, |(host, wire)| {
        submit(env, host, wire, Duration::from_secs(3600 + 60))
    });
    for ((host, _), outcome) in jobs.iter().zip(outcomes) {
        match outcome {
            Ok(report) => {
                tally.failed |= report.failed();
                for line in exec_lines(&report) {
                    println!("{:<16} {line}", host.name);
                }
            }
            Err(why) => {
                tally.failed = true;
                println!("{:<16} {}", host.name, shown(&why));
            }
        }
    }
    tally.code()
}

/// What `run` prints of one host's `exec` report: the refusal, or each row's status, its
/// standard output and its standard error, line by line. The program's output is whatever
/// is on the host (file names a student made, a crafted default), so each line is made safe
/// to print.
fn exec_lines(report: &HostReport) -> Vec<String> {
    let mut out = Vec::new();
    if let Some(why) = &report.refused {
        out.push(format!("refused: {}", shown(why)));
    }
    for row in &report.items {
        out.push(shown(row.to.as_deref().unwrap_or("did not run")));
        for line in row.detail.iter().flat_map(|d| d.lines()) {
            out.push(format!("  {}", shown(line)));
        }
        for line in row.why.iter().flat_map(|d| d.lines()) {
            out.push(format!("! {}", shown(line)));
        }
    }
    out
}

/// An `exec` envelope for `host`.
fn exec_body(env: &Env<'_>, host: &Host, floor: u64, exec: Exec) -> Result<Envelope, String> {
    let mut envelope = body(env, host, floor, Action::Exec, Disrupt::Now)?;
    envelope.idle_secs = 0;
    envelope.exec = Some(exec);
    Ok(envelope)
}

// ── person / screen ─────────────────────────────────────────────────────────────────────

/// `aterm machines person [<host>…|--all]`: what only a person can do, grouped by step,
/// from `state/` alone. A step whose row could not be read is listed as not measured, and a
/// host whose newest run was refused is named: neither is ever "nothing outstanding".
#[must_use]
pub fn person(env: &Env<'_>, names: &[String], all: bool) -> ExitCode {
    let inv = match load(env) {
        Ok(inv) => inv,
        Err(code) => return code,
    };
    let hosts = match inventory::select(&inv, names, all || names.is_empty()) {
        Ok(h) => h,
        Err(e) => {
            eprintln!("aterm machines person: {e}");
            return ExitCode::from(2);
        }
    };
    let now = env.sys.now();
    let mut tally = Tally::default();
    let mut by_step: BTreeMap<String, (String, Vec<String>)> = BTreeMap::new();
    let mut unknown: Vec<&str> = Vec::new();
    let mut notes: Vec<String> = Vec::new();
    for host in hosts {
        let Some(state) = read_state(env, host, &mut tally) else {
            continue;
        };
        let Some(report) = &state.report else {
            unknown.push(&host.name);
            continue;
        };
        // A refusal saved as the report itself (a state file from before refusals were
        // kept beside it) measured nothing.
        if let Some(why) = &report.refused {
            tally.failed = true;
            notes.push(format!(
                "{}: not measured (the last run was refused: {}); run `aterm machines plan {}`",
                host.name,
                shown(why),
                host.name
            ));
            continue;
        }
        if let Some((at, why)) = &state.last_refusal {
            tally.failed = true;
            notes.push(format!(
                "{}: the agent refused the newest run {} ago ({}); its steps are from the \
                 report before it",
                host.name,
                age(now.saturating_sub(*at)),
                shown(why)
            ));
        }
        for step in screen::prepare_steps(report) {
            let who = if step.measured {
                host.name.clone()
            } else {
                tally.failed = true;
                format!(
                    "{} (not measured: {})",
                    host.name,
                    shown(step.why.as_deref().unwrap_or("no reading"))
                )
            };
            by_step
                .entry(step.id())
                .or_insert_with(|| (step.how(), Vec::new()))
                .1
                .push(who);
        }
    }
    if by_step.is_empty() && unknown.is_empty() && notes.is_empty() {
        println!("no person steps outstanding");
    }
    for (id, (how, hosts)) in &by_step {
        println!("{id}: {how}");
        println!("    {}", hosts.join(" "));
    }
    for note in &notes {
        println!("{note}");
    }
    if !unknown.is_empty() {
        println!(
            "never planned (run `aterm machines plan` first): {}",
            unknown.join(" ")
        );
    }
    if !by_step.is_empty() {
        println!(
            "At the Mac: `aterm machines person <host> --here`. From here: `aterm machines \
             screen <host>`."
        );
    }
    tally.code()
}

/// The signing key, kept in ssh-agent for the length of one person flow, which signs one
/// `exec` envelope per re-probe: a passphrase is asked for once (`ssh-add`), not on every
/// probe. Removed from the agent again when dropped, when this flow added it; a flow that is
/// interrupted cannot remove it, and it expires after [`AGENT_KEY_LIFETIME`].
struct AgentKey<'a> {
    sys: &'a dyn System,
    sock: OsString,
    key: PathBuf,
}

impl Drop for AgentKey<'_> {
    fn drop(&mut self) {
        let _ = self
            .sys
            .run(&ssh_add(&self.sock, &["-d"]).arg(self.key.as_os_str()));
    }
}

/// `/usr/bin/ssh-add <args>` handed the agent socket `sock` (`SSH_AUTH_SOCK`).
fn ssh_add(sock: &OsStr, args: &[&str]) -> Cmd {
    Cmd::new("/usr/bin/ssh-add")
        .args(args)
        .secret_env("SSH_AUTH_SOCK", sock.to_string_lossy())
}

/// How a person flow signs: unattended with no passphrase; through ssh-agent with one,
/// loading the key there (`ssh-add -t`, asked once) unless it already is.
fn person_signing<'a>(env: &Env<'a>) -> Result<(Signing, Option<AgentKey<'a>>), String> {
    let key = env.paths.signing_key();
    match envelope::needs_passphrase(env.sys, &key) {
        Read::Value(false) => return Ok((Signing::Unattended, None)),
        Read::Value(true) => {}
        Read::Absent => return Err(format!("{}: no key", key.display())),
        Read::Unusable(why) => return Err(format!("{}: {why}", key.display())),
    }
    let Some(sock) = env.agent_sock.clone() else {
        return Err(format!(
            "{} has a passphrase and no ssh-agent is running (SSH_AUTH_SOCK is not set): \
             every re-probe signs an envelope, so the key must be in an agent",
            key.display()
        ));
    };
    if agent_holds(env, &key) {
        return Ok((Signing::Agent, None));
    }
    eprintln!(
        "The signing key goes into ssh-agent for this walk (removed when it ends, or after an \
         hour if the walk is interrupted); ssh-add asks its passphrase:"
    );
    match env.sys.run_tty(
        &Cmd::new("/usr/bin/ssh-add")
            .args(["-t", AGENT_KEY_LIFETIME])
            .arg(key.as_os_str()),
    ) {
        Ok(Some(0)) => Ok((
            Signing::Agent,
            Some(AgentKey {
                sys: env.sys,
                sock,
                key,
            }),
        )),
        Ok(code) => Err(format!("ssh-add did not load the key (status {code:?})")),
        Err(e) => Err(format!("ssh-add: {e}")),
    }
}

/// The host as the person flows reach it: each request is one signed `exec` envelope,
/// sent with `apply`, and its report row read back as the run's status, output and errors.
struct LiveRemote<'a> {
    env: &'a Env<'a>,
    host: &'a Host,
    floor: u64,
    how: Signing,
}

impl screen::Remote for LiveRemote<'_> {
    fn exec(&self, exec: &Exec) -> Result<Ran, String> {
        let body = exec_body(self.env, self.host, self.floor, exec.clone())?;
        let wire = seal_with(
            self.env.sys,
            std::slice::from_ref(&body),
            &self.env.paths.signing_key(),
            self.how,
        )?
        .pop()
        .ok_or_else(|| "ssh-keygen signed nothing".to_owned())?;
        let budget = Duration::from_secs(exec.budget_secs.saturating_add(60));
        let report = submit(self.env, self.host, &wire, budget)?;
        if let Some(why) = report.refused {
            return Err(format!("refused: {why}"));
        }
        let row = report
            .items
            .first()
            .ok_or_else(|| "the exec report has no row".to_owned())?;
        crate::agent::job::exec_ran(row)
    }
}

/// The host's newest plan or apply report, planning it first when there is none (a Mac just
/// enrolled) or when what is saved is a refusal (a state file from before refusals were kept
/// beside the report).
fn report_for(env: &Env<'_>, host: &Host) -> Result<HostReport, String> {
    let saved = inventory::load_state(env.sys, &env.paths, &host.uuid)?;
    if let Some(report) = saved.report.filter(|r| r.refused.is_none()) {
        return Ok(report);
    }
    println!(
        "{}: no plan or apply report yet; planning it first",
        host.name
    );
    let _ = plan_or_apply(
        env,
        std::slice::from_ref(&host.uuid),
        false,
        Mode::Plan,
        false,
    );
    inventory::load_state(env.sys, &env.paths, &host.uuid)?
        .report
        .filter(|r| r.refused.is_none())
        .ok_or_else(|| format!("{} answered no report to plan", host.name))
}

/// The per-app steps for aterm, set apart when `report` shows `/Applications/aterm.app` not
/// installed: aterm is in neither list until `apply` installs it, so the person would wait
/// at a pane for a switch that is not there. Each comes back as not attempted, saying to
/// apply first; the rest are walked.
fn waiting_for_aterm(
    report: &HostReport,
    steps: Vec<screen::Step>,
    host: &Host,
) -> (Vec<screen::Step>, Vec<screen::Walked>) {
    let missing = report.items.iter().any(|row| {
        row.id == "aterm.app"
            && row.from.as_deref() == Some("absent")
            && row.outcome != Outcome::Changed
    });
    if !missing {
        return (steps, Vec::new());
    }
    let (wait, walk): (Vec<screen::Step>, Vec<screen::Step>) = steps
        .into_iter()
        .partition(|s| s.step.bundle.as_deref() == Some(crate::items::person::APPS[0].1));
    let waiting = wait
        .into_iter()
        .map(|s| screen::Walked {
            step: s.id(),
            done: screen::Done::NotGranted(format!(
                "not attempted: aterm.app is not installed on {} yet; `aterm machines apply \
                 {}` first, then this again",
                host.name, host.name
            )),
        })
        .collect();
    (walk, waiting)
}

/// The steps a walk takes from `report` (`step` alone when one is named), and the ones it
/// sets apart unwalked ([`waiting_for_aterm`]). `screen` is refused while the report lists
/// Screen Sharing itself outstanding, whichever step was named: the check reads the whole
/// list, before it is narrowed.
fn walk_steps(
    report: &HostReport,
    host: &Host,
    at_console: bool,
    step: Option<&str>,
) -> Result<(Vec<screen::Step>, Vec<screen::Walked>), String> {
    let mut steps = screen::prepare_steps(report);
    if !at_console && let Some(off) = screen::screen_sharing_outstanding(&steps) {
        return Err(screen::screen_sharing_off(&host.address, off));
    }
    if let Some(id) = step {
        steps = screen::only(steps, id)?;
    }
    Ok(waiting_for_aterm(report, steps, host))
}

/// What the end of a walk prints, in the order it prints it ([`end_walk`]).
#[derive(Debug, Clone, PartialEq, Eq)]
enum Said {
    /// A step's line, on standard output.
    Step(String),
    /// The closing plan ([`plan_or_apply`], which prints its own lines).
    Plan,
    /// The last line, on standard error.
    Last(String),
}

/// The end of a walk that ran: one line per step ([`tell`]), the closing plan when any reads
/// granted, then, last, the line saying the Mac was left logged in and unlocked at its
/// display ([`screen::Session::unlocked_line`]), which fails the command whatever the steps
/// did: it is what the person must not miss. `emit` prints each in turn.
fn end_walk(
    host: &Host,
    verb: &str,
    walked: &[screen::Walked],
    session: &screen::Session,
    emit: &mut dyn FnMut(Said),
) -> ExitCode {
    let (failed, changed) = tell(host, walked, emit);
    if changed {
        emit(Said::Plan);
    }
    if let Some(line) = session.unlocked_line(&host.name, &host.user) {
        emit(Said::Last(format!(
            "aterm machines {verb}: {}",
            shown(&line)
        )));
        return ExitCode::FAILURE;
    }
    if failed {
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    }
}

/// One line per walked step, through `emit`. Returns whether any did not end granted, and
/// whether any reads granted now: flipped during the walk or before it, either leaves the
/// saved report out of date, so the walk plans the host again.
fn tell(host: &Host, walked: &[screen::Walked], emit: &mut dyn FnMut(Said)) -> (bool, bool) {
    let mut failed = false;
    let mut changed = false;
    for w in walked {
        let line = match &w.done {
            screen::Done::AlreadyGranted => {
                changed = true;
                "already granted".to_owned()
            }
            screen::Done::Granted => {
                changed = true;
                "granted".to_owned()
            }
            screen::Done::AtConsole(how) => format!("at the Mac's keyboard: {how}"),
            screen::Done::NotGranted(why) => {
                failed = true;
                format!("not granted: {}", shown(why))
            }
        };
        emit(Said::Step(format!("{}: {}: {line}", host.name, w.step)));
    }
    (failed, changed)
}

/// `aterm machines person <host> --here` (`at_console`) and `aterm machines screen <host>
/// [<step>]`: walk the host's outstanding person steps, at its keyboard or over Screen
/// Sharing, then plan it again so `status` shows what changed. When `screen` could not lock
/// a session the admin logged in to over the relay ([`screen::Session::Unlocked`]), its last
/// line says the Mac is logged in and unlocked at its display, and it exits 1.
///
/// A signing key with a passphrase is loaded into ssh-agent first and held until the walk's
/// closing plan, so the walk asks for it once: its first plan (a Mac with no report yet),
/// every re-probe and the closing plan all sign through the agent.
///
/// `opener` opens Screen Sharing.app on the controller (ignored at the Mac's keyboard, where
/// no viewer is wanted): the owner's command passes [`screen::LiveOpener`], and the dev verb
/// `screen-headless` an opener that only prints the URL, so a test host can be walked with no
/// window on the controller.
#[must_use]
pub fn walk(
    env: &Env<'_>,
    name: &str,
    at_console: bool,
    step: Option<&str>,
    terminals: screen::Terminals,
    opener: &dyn screen::Opener,
) -> ExitCode {
    use std::io::Write as _;
    let verb = if at_console {
        "person --here"
    } else {
        "screen"
    };
    if let Err(why) = screen::require_terminals(verb, terminals.stdin, terminals.stderr) {
        eprintln!("{why}");
        return ExitCode::from(2);
    }
    let inv = match load(env) {
        Ok(inv) => inv,
        Err(code) => return code,
    };
    let host = match one_host(&inv, name, verb) {
        Ok(h) => h,
        Err(code) => return code,
    };
    let (how, agent_key) = match person_signing(env) {
        Ok(signing) => signing,
        Err(why) => {
            eprintln!("aterm machines {verb}: {}: {why}", host.name);
            return ExitCode::FAILURE;
        }
    };
    let walked = (|| {
        let report = report_for(env, host)?;
        let admin = screen::account(&report, &host.user)?;
        let (steps, waiting) = walk_steps(&report, host, at_console, step)?;
        let hello = hello(env, host).map_err(|e| e.why())?;
        let remote = LiveRemote {
            env,
            host,
            floor: floor_of(&hello, &principals(&inv, env)),
            how,
        };
        let clock = screen::WallClock::new();
        // Never a panic: after SIGHUP the terminal is gone, and the walk must still close the
        // relay and lock the session.
        let say = |line: &str| {
            let _ = writeln!(std::io::stderr(), "{}: {}", host.name, shown(line));
        };
        let flow = screen::Flow {
            sys: env.sys,
            remote: &remote,
            clock: &clock,
            say: &say,
            hang: screen::PERSON_HANG,
        };
        let (mut walked, session) = if steps.is_empty() && !waiting.is_empty() {
            (Vec::new(), screen::Session::Kept)
        } else if at_console {
            (
                screen::here(&flow, &admin, &steps, terminals)?,
                screen::Session::Kept,
            )
        } else {
            let screened = screen::screen(
                &flow,
                &env.paths,
                &Target::of(host),
                &admin,
                &steps,
                opener,
                terminals,
            )?;
            (screened.walked, screened.session)
        };
        walked.extend(waiting);
        Ok::<_, String>((walked, session))
    })();
    let (walked, session) = match walked {
        Ok(w) => w,
        Err(why) => {
            eprintln!("aterm machines {verb}: {}: {}", host.name, shown(&why));
            return ExitCode::FAILURE;
        }
    };
    // The closing plan signs through the agent too; the key leaves it once the walk ends.
    // The walk's own lines never panic: after SIGHUP the terminal is gone (VM: `println!`
    // ended the walk with 101 after its lock), and the exit status is all that is left.
    let code = end_walk(host, verb, &walked, &session, &mut |said| match said {
        Said::Step(line) => {
            let _ = writeln!(std::io::stdout(), "{line}");
        }
        Said::Plan => {
            let _ = plan_or_apply(
                env,
                std::slice::from_ref(&host.uuid),
                false,
                Mode::Plan,
                false,
            );
        }
        Said::Last(line) => {
            let _ = writeln!(std::io::stderr(), "{line}");
        }
    });
    drop(agent_key);
    code
}

// ── github / unenroll / rekey ───────────────────────────────────────────────────────────

/// `aterm machines github revoke <host>`: delete the host's keys from the machine user, and
/// record each deleted key so no later report registers it again. A key the host makes anew
/// (another body) is registered by the next apply.
#[must_use]
pub fn github_revoke(env: &Env<'_>, name: &str) -> ExitCode {
    let inv = match load(env) {
        Ok(inv) => inv,
        Err(code) => return code,
    };
    let host = match one_host(&inv, name, "github revoke") {
        Ok(h) => h,
        Err(code) => return code,
    };
    let Some(github) = &inv.github else {
        eprintln!("aterm machines github revoke: machines.toml names no [github] account");
        return ExitCode::from(2);
    };
    let mut state = match inventory::load_state(env.sys, &env.paths, &host.uuid) {
        Ok(s) => s,
        Err(why) => {
            eprintln!("aterm machines github revoke: {why}");
            return ExitCode::FAILURE;
        }
    };
    if state.github_keys.is_empty() {
        println!("{}: no GitHub keys recorded", host.name);
        return ExitCode::SUCCESS;
    }
    let ids: Vec<u64> = state.github_keys.iter().map(|(_, id)| *id).collect();
    let deleted = match super::github::revoke(env.sys, &github.account, &ids) {
        Ok(deleted) => deleted,
        Err(why) => {
            eprintln!("aterm machines github revoke: {}: {why}", host.name);
            return ExitCode::FAILURE;
        }
    };
    // What the account held under those ids, and what the controller recorded for them (a
    // key already deleted on GitHub by hand stays revoked too).
    let recorded = ids
        .iter()
        .filter_map(|id| key_text_of(&state, *id).map(str::to_owned));
    let revoked: Vec<String> = deleted.into_iter().chain(recorded).collect();
    for key in revoked {
        if !state.github_revoked.contains(&key) {
            state.github_revoked.push(key);
        }
    }
    println!(
        "{}: deleted {} key(s) from {}; they are not registered again (a key the host makes \
         anew is)",
        host.name,
        ids.len(),
        github.account
    );
    state.github_keys.clear();
    state.github_key_text.clear();
    if let Err(why) = inventory::save_state(env.sys, &env.paths, &host.uuid, &state) {
        eprintln!("aterm machines github revoke: {why}");
        return ExitCode::FAILURE;
    }
    ExitCode::SUCCESS
}

/// `aterm machines unenroll <host> [--unreachable]` (a person decides: refused without a
/// terminal).
#[must_use]
pub fn unenroll(env: &Env<'_>, name: &str, unreachable: bool) -> ExitCode {
    if !env.tty {
        eprintln!(
            "aterm machines unenroll: not run: it removes a Mac from the fleet, and standard \
             input or error is not a terminal"
        );
        return ExitCode::from(2);
    }
    let inv = match load(env) {
        Ok(inv) => inv,
        Err(code) => return code,
    };
    let host = match one_host(&inv, name, "unenroll") {
        Ok(h) => h.clone(),
        Err(code) => return code,
    };
    let principals = principals(&inv, env);
    let mut out = std::io::stdout().lock();
    match enroll::unenroll(
        env.sys,
        &env.paths,
        &principals,
        &host,
        unreachable,
        &mut out,
    ) {
        Ok(()) => ExitCode::SUCCESS,
        Err(why) => {
            eprintln!("aterm machines unenroll: {}", shown(&why));
            ExitCode::FAILURE
        }
    }
}

/// The entries of an `allowed_signers` text that [`envelope::check_signers`] accepted:
/// `(principal, key type, base64)`, comment and blank lines skipped.
fn signer_entries(text: &str) -> Vec<(&str, &str, &str)> {
    text.lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .filter_map(|line| {
            let words: Vec<&str> = line.split_whitespace().collect();
            match words.as_slice() {
                [principal, _options, kind, blob] => Some((*principal, *kind, *blob)),
                _ => None,
            }
        })
        .collect()
}

/// Every principal the `allowed_signers` file `list` gives the public key in the file
/// `public`.
fn principals_of_key(sys: &dyn System, public: &Path, list: &Path) -> Result<Vec<String>, String> {
    let key = sys
        .read_file(public)
        .map_err(|e| format!("{}: {e}", public.display()))?;
    let key = String::from_utf8_lossy(&key);
    let (kind, blob) = envelope::public_key_parts(key.trim())?;
    let signers = sys
        .read_file(list)
        .map_err(|e| format!("{}: {e}", list.display()))?;
    let signers = String::from_utf8_lossy(&signers);
    let names: Vec<String> = signer_entries(&signers)
        .into_iter()
        .filter(|(_, k, b)| *k == kind && *b == blob)
        .map(|(p, ..)| p.to_owned())
        .collect();
    if names.is_empty() {
        return Err(format!("{} is not in {}", public.display(), list.display()));
    }
    Ok(names)
}

/// The public half of the private key `key` rekey signs with: `<key>.pub` beside it, else
/// the controller's `keys/<file name>.pub` (init keeps `keys/recovery.pub` when the private
/// half goes to removable media).
///
/// # Errors
/// One line naming both places.
fn public_of(env: &Env<'_>, key: &Path) -> Result<PathBuf, String> {
    let beside = public_half(key);
    if env.sys.stat(&beside).is_ok() {
        return Ok(beside);
    }
    let kept = key
        .file_name()
        .map(|name| public_half(&env.paths.keys().join(name)));
    match kept {
        Some(kept) if env.sys.stat(&kept).is_ok() => Ok(kept),
        Some(kept) => Err(format!(
            "no public key for {}: neither {} nor {} exists",
            key.display(),
            beside.display(),
            kept.display()
        )),
        None => Err(format!("{} names no key file", key.display())),
    }
}

/// `aterm machines rekey [<host>…|--all] --signers <file> [--key <private key>]`: replace
/// the `allowed_signers` every selected host trusts with `file`, signed with `key` (the
/// signing key, or the recovery key from removable media).
///
/// The controller's own copy, which enrollment hands new hosts, becomes `file` as soon as
/// one host took it: `file` lists this controller's signing key, so hosts still on the old
/// list keep accepting it. The old copy is kept as `allowed_signers.previous` (the signing
/// key's floors may sit under its old principal there), and the hosts not known to carry the
/// new list are named.
#[must_use]
pub fn rekey(
    env: &Env<'_>,
    names: &[String],
    all: bool,
    signers: &Path,
    key: Option<&Path>,
) -> ExitCode {
    let text = match env.sys.read_file(signers) {
        Ok(t) => String::from_utf8_lossy(&t).into_owned(),
        Err(e) => {
            eprintln!("aterm machines rekey: {}: {e}", signers.display());
            return ExitCode::from(2);
        }
    };
    if let Err(why) = envelope::check_signers(&text) {
        eprintln!("aterm machines rekey: {}: {why}", signers.display());
        return ExitCode::from(2);
    }
    // The controller must still be able to sign afterwards: its signing key is one of the
    // new list's entries (a commented-out line is no entry).
    let own = public_half(&env.paths.signing_key());
    let own_listed = env
        .sys
        .read_file(&own)
        .ok()
        .and_then(|k| {
            let k = String::from_utf8_lossy(&k).into_owned();
            envelope::public_key_parts(k.trim())
                .ok()
                .map(|(kind, blob)| {
                    signer_entries(&text)
                        .iter()
                        .any(|(_, k, b)| *k == kind && *b == blob)
                })
        })
        .unwrap_or(false);
    if !own_listed {
        eprintln!(
            "aterm machines rekey: {} does not list this controller's signing key ({}): the \
             fleet would stop accepting it",
            signers.display(),
            own.display()
        );
        return ExitCode::from(2);
    }
    let inv = match load(env) {
        Ok(inv) => inv,
        Err(code) => return code,
    };
    let hosts = match fleet_hosts(&inv, names, all, "rekey") {
        Ok(h) => h,
        Err(code) => return code,
    };
    let key = key.map_or_else(|| env.paths.signing_key(), Path::to_path_buf);
    let principals = match public_of(env, &key)
        .and_then(|public| principals_of_key(env.sys, &public, &env.paths.allowed_signers()))
    {
        Ok(p) => p,
        Err(why) => {
            eprintln!("aterm machines rekey: {why}");
            return ExitCode::from(2);
        }
    };
    let parallel = parallel(env, &inv);
    let mut tally = Tally::default();
    let mut targets = Vec::new();
    let mut bodies = Vec::new();
    let reached = hello_sweep(env, &hosts, parallel);
    if let Some(stop) = local_network_wall(&reached) {
        eprintln!("{stop}");
        return ExitCode::FAILURE;
    }
    for r in reached {
        let hello = match r.hello {
            Ok(h) => h,
            Err(why) => {
                not_reached(&mut tally, r.host, &why);
                continue;
            }
        };
        match body(
            env,
            r.host,
            floor_of(&hello, &principals),
            Action::Rekey,
            Disrupt::Now,
        ) {
            Ok(mut b) => {
                b.rekey = Some(text.clone());
                bodies.push(b);
                targets.push(r.host);
            }
            Err(why) => {
                eprintln!("aterm machines rekey: {why}");
                return ExitCode::FAILURE;
            }
        }
    }
    let wires = match seal_all(env, &bodies, &key) {
        Ok(w) => w,
        Err(why) => {
            eprintln!("aterm machines rekey: {why}");
            return ExitCode::FAILURE;
        }
    };
    let jobs: Vec<(&Host, Vec<u8>)> = targets.into_iter().zip(wires).collect();
    let outcomes = ssh::fan_out(&jobs, parallel, |(host, wire)| {
        submit(env, host, wire, Duration::from_secs(120))
    });
    let digest = text_sha256(&text);
    let mut took = 0;
    for ((host, _), outcome) in jobs.iter().zip(outcomes) {
        match outcome {
            Ok(r) if r.refused.is_none() && !r.failed() => {
                took += 1;
                println!("{:<16} rekeyed", host.name);
                record_signers(env, host, &digest);
            }
            Ok(r) => {
                tally.failed = true;
                let why: Vec<String> = r
                    .refused
                    .into_iter()
                    .chain(r.items.into_iter().filter_map(|i| i.why))
                    .collect();
                println!("{:<16} not rekeyed: {}", host.name, shown(&why.join("; ")));
            }
            Err(why) => {
                tally.failed = true;
                println!("{:<16} not rekeyed: {}", host.name, shown(&why));
            }
        }
    }
    if took == 0 {
        println!(
            "no host took the new list; the controller's {} is unchanged",
            env.paths.allowed_signers().display()
        );
        return tally.code();
    }
    if let Err(why) = replace_signers(env, &text) {
        tally.failed = true;
        eprintln!("aterm machines rekey: {why}");
        return tally.code();
    }
    println!(
        "the controller's {} is now {} (the old list is {})",
        env.paths.allowed_signers().display(),
        signers.display(),
        previous_signers(&env.paths).display()
    );
    let behind: Vec<&str> = inv
        .hosts
        .iter()
        .filter(|h| {
            inventory::load_state(env.sys, &env.paths, &h.uuid)
                .ok()
                .and_then(|s| s.signers_sha256)
                .is_none_or(|d| d != digest)
        })
        .map(|h| h.name.as_str())
        .collect();
    if !behind.is_empty() {
        println!(
            "not known to carry the new list yet (rekey them when they answer): {}",
            behind.join(" ")
        );
    }
    tally.code()
}

/// The SHA-256 of `text`, hex.
fn text_sha256(text: &str) -> String {
    aterm_codec::hex::encode_lower(&aterm_digest::Sha256::digest(text.as_bytes()))
}

/// Record in `host`'s state that it carries the `allowed_signers` whose SHA-256 is `digest`.
fn record_signers(env: &Env<'_>, host: &Host, digest: &str) {
    match inventory::load_state(env.sys, &env.paths, &host.uuid) {
        Ok(mut state) => {
            state.signers_sha256 = Some(digest.to_owned());
            if let Err(why) = inventory::save_state(env.sys, &env.paths, &host.uuid, &state) {
                eprintln!("aterm machines rekey: {why}");
            }
        }
        Err(why) => eprintln!("aterm machines rekey: {}: not recorded: {why}", host.name),
    }
}

/// Make `text` the controller's `allowed_signers`, keeping the old one as
/// `allowed_signers.previous` first. An old copy that is there and cannot be read stops it;
/// one that already is `text` (a rekey catching up the hosts that missed it) is left as it
/// is, so `.previous` keeps the list those hosts still hold.
fn replace_signers(env: &Env<'_>, text: &str) -> Result<(), String> {
    let current = env.paths.allowed_signers();
    match env.sys.read_file(&current) {
        Ok(old) if old == text.as_bytes() => return Ok(()),
        Ok(old) => env
            .sys
            .write_file(&previous_signers(&env.paths), &old, 0o644)
            .map_err(|e| format!("{}: {e}", previous_signers(&env.paths).display()))?,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(format!("{}: {e}; not replaced", current.display())),
    }
    env.sys
        .write_file(&current, text.as_bytes(), 0o644)
        .map_err(|e| format!("{}: {e}", current.display()))
}

// ── here ────────────────────────────────────────────────────────────────────────────────

/// `aterm machines here plan|apply <baseline.toml>`: run a baseline on this Mac directly,
/// with no controller and no envelope. Applying needs root (`sudo`), which a person types
/// for; a plan as a user probes what it can.
#[must_use]
pub fn here(sys: &dyn System, mode: Mode, baseline_path: &Path, json: bool) -> ExitCode {
    if mode == Mode::Apply && sys.euid() != 0 {
        eprintln!("aterm machines here apply: needs root: run it with sudo");
        return ExitCode::from(2);
    }
    let text = match sys.read_file(baseline_path) {
        Ok(b) => String::from_utf8_lossy(&b).into_owned(),
        Err(e) => {
            eprintln!("aterm machines here: {}: {e}", baseline_path.display());
            return ExitCode::FAILURE;
        }
    };
    let facts = crate::facts::gather(sys);
    let name = facts.local_host_name.clone();
    let text = match crate::baseline::fill(
        &text,
        &[
            ("name", &name),
            ("serial", &facts.serial),
            ("uuid", &facts.uuid),
        ],
    ) {
        Ok(t) => t,
        Err(e) => {
            eprintln!("aterm machines here: {e}");
            return ExitCode::FAILURE;
        }
    };
    let baseline = match crate::baseline::parse(&text) {
        Ok(b) => b,
        Err(e) => {
            eprintln!("aterm machines here: {e}");
            return ExitCode::FAILURE;
        }
    };
    let mut users = Vec::new();
    let wanted: Vec<String> = if baseline.users.is_empty() {
        facts.console_user.iter().cloned().collect()
    } else {
        baseline.users.clone()
    };
    for u in &wanted {
        match crate::facts::user(sys, u) {
            Ok(user) => users.push(user),
            Err(e) => {
                eprintln!("aterm machines here: {e}");
                return ExitCode::FAILURE;
            }
        }
    }
    let console = facts
        .console_user
        .as_ref()
        .and_then(|c| crate::facts::user(sys, c).ok());
    let ctx = crate::items::Ctx {
        cache_dir: Path::new(crate::AGENT_ROOT).join("cache"),
        disrupt: true,
        facts,
        users,
        console,
        host_name: name,
    };
    let items = match crate::items::expand(&baseline, &ctx) {
        Ok(i) => i,
        Err(e) => {
            eprintln!("aterm machines here: {e}");
            return ExitCode::FAILURE;
        }
    };
    let started = sys.now();
    let rows = crate::engine::run(&items, mode, sys, &ctx, &mut crate::engine::NoJournal);
    let report = HostReport {
        schema: crate::SCHEMA,
        mode,
        serial: 0,
        key_id: String::new(),
        run_id: String::new(),
        started,
        finished: sys.now(),
        facts: ctx.facts.clone(),
        items: rows,
        refused: None,
    };
    let host = Host {
        name: ctx.host_name.clone(),
        uuid: ctx.facts.uuid.clone(),
        address: "localhost".into(),
        user: String::new(),
        baseline: None,
        users: Vec::new(),
    };
    let env_sys = Env {
        sys,
        paths: Paths {
            root: std::path::PathBuf::from("/nonexistent"),
        },
        json,
        tty: false,
        parallel: None,
        agent_sock: None,
    };
    print_report(&env_sys, &host, &report);
    if report.failed() {
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    }
}

// ── helpers ─────────────────────────────────────────────────────────────────────────────

/// Load the inventory, or print why not and return the exit code: 1, since an inventory
/// that is missing, unreadable or invalid is a failure, never "only some hosts unreachable".
fn load(env: &Env<'_>) -> Result<Inventory, ExitCode> {
    inventory::load(env.sys, &env.paths).map_err(|e| {
        eprintln!("aterm machines: {e}");
        ExitCode::FAILURE
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::port::Fake;

    const ROOT: &str = "/ctl/machines";
    const UUID_1: &str = "0A1B2C3D-0000-4000-8000-000000000001";
    const UUID_2: &str = "0A1B2C3D-0000-4000-8000-000000000002";
    const SIGNING_BLOB: &str =
        "AAAAC3NzaC1lZDI1NTE5AAAAIHv2Xf9kgkNkpV3WltnLoVx25AQOEx+mch1DHT/oZdSa";
    const RECOVERY_BLOB: &str =
        "AAAAC3NzaC1lZDI1NTE5AAAAIJpFPtSPTOuLIj6oqQTj8ndNDVNHFp4renP47+wxFYao";

    /// What `ssh` printed for a changed host key, measured in the VM (ssh.rs's fixture).
    const HOST_KEY_CHANGED: &str = "@    WARNING: REMOTE HOST IDENTIFICATION HAS CHANGED!     @\r\nHost key for 0a1b2c3d-0000-4000-8000-000000000001 has changed and you have requested strict checking.\r\nHost key verification failed.\r\n";

    fn paths() -> Paths {
        Paths {
            root: PathBuf::from(ROOT),
        }
    }

    fn env(sys: &dyn System) -> Env<'_> {
        Env {
            sys,
            paths: paths(),
            json: false,
            tty: false,
            parallel: Some(1),
            agent_sock: None,
        }
    }

    fn lab(n: u8, uuid: &str) -> Host {
        Host {
            name: format!("lab-0{n}"),
            uuid: uuid.into(),
            address: format!("192.168.64.{n}"),
            user: "admin".into(),
            baseline: None,
            users: Vec::new(),
        }
    }

    /// A controller holding `hosts`, this test binary standing in for the agent build, and
    /// the signing key `allowed_signers` names `signing@old-name`.
    fn controller(hosts: &[&Host], github: bool) -> Fake {
        let sys = Fake::root();
        let mut inv = String::from("schema = 1\n");
        if github {
            inv.push_str("[github]\naccount = \"fleetbot\"\n");
        }
        for h in hosts {
            inv.push_str(&format!(
                "[[host]]\nname = \"{}\"\nuuid = \"{}\"\naddress = \"{}\"\nuser = \"{}\"\n",
                h.name, h.uuid, h.address, h.user
            ));
        }
        sys.file(&format!("{ROOT}/machines.toml"), inv.as_bytes());
        sys.file(
            &enroll::own_exe().unwrap().to_string_lossy(),
            b"this controller's agent",
        );
        sys.file(
            &format!("{ROOT}/keys/signing.pub"),
            format!("ssh-ed25519 {SIGNING_BLOB} signing@old-name\n").as_bytes(),
        );
        sys.file(
            &format!("{ROOT}/allowed_signers"),
            format!(
                "signing@old-name namespaces=\"aterm-machines\" ssh-ed25519 {SIGNING_BLOB}\n\
                 recovery@old-name namespaces=\"aterm-machines\" ssh-ed25519 {RECOVERY_BLOB}\n"
            )
            .as_bytes(),
        );
        sys
    }

    /// The command a request of `host` is scripted under.
    fn request(host: &Host, command: &str) -> String {
        ssh::request_cmd(&paths(), &Target::of(host), command)
            .unwrap()
            .display()
    }

    fn hello_of(host: &Host) -> Hello {
        Hello {
            schema: 1,
            agent_version: "0.106.0".into(),
            agent_sha256: "ab".repeat(32),
            uuid: host.uuid.clone(),
            serial_number: "ZNYVNTRJN5".into(),
            os_version: "27.0".into(),
            os_build: "26A428".into(),
            ..Hello::default()
        }
    }

    fn state_of(sys: &Fake, host: &Host) -> HostState {
        inventory::load_state(sys, &paths(), &host.uuid).unwrap()
    }

    fn row(id: &str, family: &str, outcome: Outcome) -> ItemReport {
        ItemReport {
            id: id.into(),
            family: family.into(),
            title: id.into(),
            outcome,
            from: None,
            to: None,
            why: None,
            revert: None,
            effect: Effect::Now,
            detail: None,
        }
    }

    fn report(mode: Mode, items: Vec<ItemReport>) -> HostReport {
        HostReport {
            schema: 1,
            mode,
            serial: 7,
            key_id: String::new(),
            run_id: "1800000000-7".into(),
            started: 1_799_999_000,
            finished: 1_799_999_900,
            facts: crate::model::HostFacts::default(),
            items,
            refused: None,
        }
    }

    fn refusal(why: &str) -> HostReport {
        let line = aterm_json::to_string(&Line::Refused { why: why.into() }).unwrap();
        read_stream(&format!("{line}\n"), 1_800_000_000).0.unwrap()
    }

    fn inventory_of(sys: &Fake) -> Inventory {
        inventory::load(sys, &paths()).unwrap()
    }

    /// No control character (an escape sequence's ESC, BEL, a carriage return) in `text`.
    fn inert(text: &str) -> bool {
        !text.chars().any(char::is_control)
    }

    #[test]
    fn the_starter_baseline_and_inventory_parse() {
        let b = crate::baseline::parse(templates::DEFAULT_BASELINE).unwrap();
        assert!(b.keyboard.is_some());
        assert_eq!(b.apps.len(), 3);
        let inv: Inventory = aterm_toml::from_str(templates::INVENTORY).unwrap();
        assert_eq!(inv.schema, 1);
        assert!(inv.hosts.is_empty());
    }

    #[test]
    fn exit_status_says_failed_before_unreachable() {
        assert_eq!(Tally::default().code(), ExitCode::SUCCESS);
        assert_eq!(
            Tally {
                failed: false,
                unreachable: true
            }
            .code(),
            ExitCode::from(2)
        );
        assert_eq!(
            Tally {
                failed: true,
                unreachable: true
            }
            .code(),
            ExitCode::FAILURE
        );
    }

    #[test]
    fn users_default_to_the_inventory_and_an_explicit_list_is_kept() {
        let host = Host {
            name: "lab-07".into(),
            uuid: "U".into(),
            address: "lab-07.local".into(),
            user: "labadmin".into(),
            baseline: None,
            users: Vec::new(),
        };
        let filled = default_users("schema = 1\n", &host).unwrap();
        assert_eq!(crate::baseline::parse(&filled).unwrap().users, ["labadmin"]);
        let kept = default_users("schema = 1\nusers = [\"alice\"]\n", &host).unwrap();
        assert_eq!(crate::baseline::parse(&kept).unwrap().users, ["alice"]);
    }

    #[test]
    fn a_stream_ends_in_its_report_and_a_refusal_is_a_refused_report_stamped_now() {
        let r = refusal("replay principal=p serial=3 floor=9");
        assert!(r.refused.unwrap().starts_with("replay"));
        assert_eq!(r.finished, 1_800_000_000, "never read as decades ago");
        assert_eq!(read_stream("garbage\n", 1).0, None);
        // A stream that named its run and then broke off: the run to attach to.
        let started = aterm_json::to_string(&Line::Started {
            run: "1800000000-7".into(),
        })
        .unwrap();
        assert_eq!(
            read_stream(&format!("{started}\n"), 1),
            (None, Some("1800000000-7".to_owned()))
        );
    }

    #[test]
    fn ages_read_in_the_largest_unit() {
        assert_eq!(age(5), "5s");
        assert_eq!(age(120), "2m");
        assert_eq!(age(7200), "2h");
        assert_eq!(age(3 * 86_400), "3d");
    }

    #[test]
    fn a_host_runs_another_agent_build_unless_it_reports_this_one() {
        let own = OwnAgent {
            exe: PathBuf::from("/x/aterm"),
            sha256: "ab".repeat(32),
        };
        let mut hello: Hello = aterm_json::from_str(
            r#"{"schema":1,"agent_version":"0.1.0","families":[],"uuid":"U","serial_number":"S",
                "computer_name":"c","local_host_name":"l","os_version":"27.0","os_build":"26A428",
                "arch":"arm64","floors":[],"cached":[]}"#,
        )
        .unwrap();
        // An agent too old to report its build is upgraded.
        assert!(own.differs(&hello));
        hello.agent_sha256 = "AB".repeat(32);
        assert!(!own.differs(&hello));
        hello.agent_sha256 = "cd".repeat(32);
        assert!(own.differs(&hello));
    }

    #[test]
    fn the_floor_is_the_highest_any_of_the_keys_names_has_and_zero_for_a_new_one() {
        let hello: Hello = aterm_json::from_str(
            r#"{"schema":1,"agent_version":"0.1.0","families":[],"uuid":"U","serial_number":"S",
                "computer_name":"c","local_host_name":"l","os_version":"27.0","os_build":"26A428",
                "arch":"arm64","floors":[{"principal":"signing@ctl","serial":41},
                {"principal":"signing@renamed","serial":12}],"cached":[]}"#,
        )
        .unwrap();
        let names = |n: &[&str]| n.iter().map(|s| (*s).to_owned()).collect::<Vec<_>>();
        assert_eq!(floor_of(&hello, &names(&["signing@ctl"])), 41);
        assert_eq!(
            floor_of(&hello, &names(&["signing@renamed", "signing@ctl"])),
            41
        );
        assert_eq!(floor_of(&hello, &names(&["recovery@ctl"])), 0);
    }

    // ── reaching hosts ──────────────────────────────────────────────────────────────────

    #[test]
    fn a_changed_host_key_fails_the_command_and_is_kept_with_its_reason_not_as_unreachable() {
        let (a, b) = (lab(1, UUID_1), lab(2, UUID_2));
        let sys = controller(&[&a, &b], false);
        sys.on(&request(&a, "hello"), Ran::exit(255, HOST_KEY_CHANGED));
        sys.on(
            &request(&b, "hello"),
            Ran::exit(
                255,
                "ssh: connect to host 192.168.64.2 port 22: Operation timed out\r\n",
            ),
        );
        let e = env(&sys);
        assert_eq!(
            plan_or_apply(&e, &[], true, Mode::Apply, false),
            ExitCode::FAILURE,
            "a possible impostor is never the harmless exit 2"
        );
        let changed = state_of(&sys, &a);
        assert_eq!(changed.unreachable_since, None);
        let (since, why) = changed.contact_error.clone().unwrap();
        assert_eq!(since, sys.now);
        assert!(why.contains("host key is not the one pinned"), "{why}");
        assert_eq!(state_of(&sys, &b).unreachable_since, Some(sys.now));
        assert_eq!(state_of(&sys, &b).contact_error, None);
        // status and outstanding read it back as a failure.
        assert_eq!(status(&e, &[], true, false), ExitCode::FAILURE);
        assert_eq!(outstanding(&e), ExitCode::FAILURE);
        assert!(
            summary(&a, &changed, sys.now + 3600).contains("failing for 1h: its host key"),
            "{}",
            summary(&a, &changed, sys.now + 3600)
        );
    }

    #[test]
    fn hosts_that_only_could_not_be_reached_exit_2_and_another_mac_answering_fails() {
        let (a, b) = (lab(1, UUID_1), lab(2, UUID_2));
        let sys = controller(&[&a, &b], false);
        sys.on(
            &request(&a, "hello"),
            Ran::exit(
                255,
                "ssh: connect to host 192.168.64.1 port 22: Connection refused\r\n",
            ),
        );
        sys.on(
            &request(&b, "hello"),
            Ran::exit(
                255,
                "ssh: connect to host 192.168.64.2 port 22: Operation timed out\r\n",
            ),
        );
        // The next time, a Mac at b's address that answers as another.
        sys.on(
            &request(&b, "hello"),
            Ran::ok(&aterm_json::to_string(&hello_of(&a)).unwrap()),
        );
        let e = env(&sys);
        assert_eq!(
            plan_or_apply(&e, &[], true, Mode::Plan, false),
            ExitCode::from(2)
        );
        assert_eq!(
            plan_or_apply(&e, &[], true, Mode::Plan, false),
            ExitCode::FAILURE
        );
        assert!(
            state_of(&sys, &b)
                .contact_error
                .unwrap()
                .1
                .contains("this is not lab-02")
        );
    }

    #[test]
    fn the_local_network_line_stands_for_the_hosts_only_when_none_answered() {
        let (a, b) = (lab(1, UUID_1), lab(2, UUID_2));
        let reached = |ha: Result<Hello, HelloError>, hb: Result<Hello, HelloError>| {
            local_network_wall(&[
                Reached {
                    host: &a,
                    hello: ha,
                },
                Reached {
                    host: &b,
                    hello: hb,
                },
            ])
        };
        let no_route = || Err(HelloError::Ssh(ssh::Failure::NoRoute));
        // One Mac a router calls unreachable never stops the one that answered.
        assert_eq!(reached(Ok(hello_of(&a)), no_route()), None);
        assert_eq!(
            reached(no_route(), no_route()),
            Some(ssh::LOCAL_NETWORK_ROW)
        );
        // A refused connection came back from a host: the LAN is reachable.
        assert_eq!(
            reached(no_route(), Err(HelloError::Ssh(ssh::Failure::Refused))),
            None
        );
        // A connection that waited and timed out (or found the host down) was let onto the
        // LAN: the refusal fails at once with no route. Measured: a dead address beside one
        // in its no-route window.
        assert_eq!(
            reached(no_route(), Err(HelloError::Ssh(ssh::Failure::TimedOut))),
            None
        );
        // A name that does not resolve says nothing either way.
        assert_eq!(
            reached(no_route(), Err(HelloError::Ssh(ssh::Failure::Unresolved))),
            Some(ssh::LOCAL_NETWORK_ROW)
        );
        // No route to a public address is not the privacy refusal.
        let far = Host {
            address: "8.8.8.8".into(),
            ..lab(3, UUID_1)
        };
        assert_eq!(
            local_network_wall(&[Reached {
                host: &far,
                hello: no_route()
            }]),
            None
        );
    }

    #[test]
    fn an_unreadable_inventory_is_a_failure_and_all_over_no_hosts_is_nothing_to_do() {
        let sys = Fake::root();
        sys.file(
            &format!("{ROOT}/machines.toml"),
            b"schema = 1\n[[host]]\nname = 3\n",
        );
        let e = env(&sys);
        assert_eq!(status(&e, &[], true, false), ExitCode::FAILURE);
        assert_eq!(
            plan_or_apply(&e, &[], true, Mode::Plan, false),
            ExitCode::FAILURE
        );
        assert_eq!(outstanding(&e), ExitCode::FAILURE);
        let missing = Fake::root();
        assert_eq!(status(&env(&missing), &[], true, false), ExitCode::FAILURE);
        let empty = controller(&[], false);
        assert_eq!(
            plan_or_apply(&env(&empty), &[], true, Mode::Apply, false),
            ExitCode::SUCCESS
        );
        // Naming no host is a refused command line: 2.
        assert_eq!(
            plan_or_apply(&env(&empty), &[], false, Mode::Apply, false),
            ExitCode::from(2)
        );
    }

    #[test]
    fn a_busy_host_is_sent_nothing_and_fails_the_command() {
        let a = lab(1, UUID_1);
        let sys = controller(&[&a], false);
        let mut busy = hello_of(&a);
        busy.busy = Some("1799999999-4".into());
        sys.on(
            &request(&a, "hello"),
            Ran::ok(&aterm_json::to_string(&busy).unwrap()),
        );
        assert_eq!(
            plan_or_apply(&env(&sys), &[], true, Mode::Apply, false),
            ExitCode::FAILURE
        );
        assert!(!sys.calls().contains(&request(&a, "apply")));
        assert!(
            sys.written(&format!("{ROOT}/serial")).is_none(),
            "no serial taken"
        );
    }

    // ── what a run leaves in state ──────────────────────────────────────────────────────

    #[test]
    fn a_refused_run_is_kept_beside_the_last_report_never_in_its_place() {
        let a = lab(1, UUID_1);
        let sys = controller(&[&a], false);
        let e = env(&sys);
        let inv = inventory_of(&sys);
        let applied = report(
            Mode::Apply,
            vec![row(
                "person.full_disk_access:aterm",
                "person",
                Outcome::Person,
            )],
        );
        after_run(&e, &inv, &a, applied.clone(), Source::Baseline);
        after_run(
            &e,
            &inv,
            &a,
            refusal("busy run=1800000000-9"),
            Source::Baseline,
        );
        let state = state_of(&sys, &a);
        assert_eq!(state.report.as_ref(), Some(&applied));
        assert_eq!(
            state.last_refusal,
            Some((sys.now, "busy run=1800000000-9".to_owned()))
        );
        assert!(summary(&a, &state, sys.now).contains("REFUSED 0s ago: busy run="));
        assert_eq!(status(&e, &[], true, false), ExitCode::FAILURE);
        assert_eq!(outstanding(&e), ExitCode::FAILURE);
        // `person` still lists the host's step, and names the refusal: never all clear.
        assert_eq!(person(&e, &[], true), ExitCode::FAILURE);
        // The next plan or apply report clears it.
        after_run(
            &e,
            &inv,
            &a,
            report(Mode::Plan, Vec::new()),
            Source::Baseline,
        );
        assert_eq!(state_of(&sys, &a).last_refusal, None);
    }

    #[test]
    fn an_exec_or_upgrade_report_attach_followed_leaves_the_hosts_report_alone() {
        let a = lab(1, UUID_1);
        let sys = controller(&[&a], false);
        let e = env(&sys);
        let inv = inventory_of(&sys);
        let applied = report(
            Mode::Apply,
            vec![row("power.ac.sleep", "power", Outcome::Failed)],
        );
        after_run(&e, &inv, &a, applied.clone(), Source::Baseline);
        for job in [
            report(Mode::Apply, vec![row("exec", "exec", Outcome::Changed)]),
            report(
                Mode::Apply,
                vec![row("agent.upgrade", "agent", Outcome::Changed)],
            ),
        ] {
            after_run(&e, &inv, &a, job, Source::Followed);
            assert_eq!(state_of(&sys, &a).report.as_ref(), Some(&applied));
        }
        // An apply attach followed is the host's report.
        let later = report(
            Mode::Apply,
            vec![row("power.ac.sleep", "power", Outcome::Changed)],
        );
        after_run(&e, &inv, &a, later.clone(), Source::Followed);
        assert_eq!(state_of(&sys, &a).report, Some(later));
    }

    #[test]
    fn a_report_whose_os_build_moved_since_the_last_apply_reads_os_changed_with_the_agent() {
        let a = lab(1, UUID_1);
        let mut hello = hello_of(&a);
        hello.os_build = "26B5".into();
        let state = HostState {
            hello: aterm_json::to_string(&hello).ok(),
            last_seen: 1_800_000_000,
            report: Some(report(Mode::Apply, Vec::new())),
            applied_build: Some("26A428".into()),
            ..HostState::default()
        };
        let line = summary(&a, &state, 1_800_000_000);
        assert!(line.contains("os-changed (26A428 \u{2192} 26B5)"), "{line}");
        assert!(line.contains("agent 0.106.0 (abababababab)"), "{line}");
        // Unknown on either side says nothing, never "unchanged" or "changed".
        let unknown = HostState {
            applied_build: None,
            ..state.clone()
        };
        assert_eq!(os_changed(&unknown, saved_hello(&unknown).as_ref()), None);
        let same = HostState {
            applied_build: Some("26B5".into()),
            ..state
        };
        assert_eq!(os_changed(&same, saved_hello(&same).as_ref()), None);
    }

    // ── host text on the terminal ───────────────────────────────────────────────────────

    #[test]
    fn what_a_host_sends_reaches_the_terminal_without_control_characters() {
        let osc52 = "\u{1b}]52;c;aGVsbG8=\u{7}";
        let mut r = row("defaults.user.x@alice", "defaults", Outcome::Planned);
        r.from = Some(format!("{{{osc52} = 1}}"));
        r.to = Some("{}".into());
        r.why = Some(format!("why{osc52}"));
        assert!(inert(&row_line(&r)), "{}", row_line(&r));
        let mut ran = report(Mode::Apply, vec![row("exec", "exec", Outcome::Changed)]);
        ran.items[0].to = Some("exit 0".into());
        ran.items[0].detail = Some(format!("file-{osc52}\r\nsecond\n"));
        ran.items[0].why = Some("\u{1b}[2J".to_owned());
        let lines = exec_lines(&ran);
        assert!(lines.iter().all(|l| inert(l)), "{lines:?}");
        assert_eq!(lines.len(), 4, "{lines:?}");
        let a = lab(1, UUID_1);
        let state = HostState {
            report: Some(refusal(&format!("busy {osc52}"))),
            last_refusal: Some((1, format!("busy {osc52}"))),
            contact_error: Some((1, osc52.to_owned())),
            ..HostState::default()
        };
        assert!(inert(&summary(&a, &state, 2)), "{}", summary(&a, &state, 2));
    }

    // ── person steps ────────────────────────────────────────────────────────────────────

    #[test]
    fn a_person_row_nobody_could_read_is_listed_unmeasured_and_fails_person() {
        let a = lab(1, UUID_1);
        let sys = controller(&[&a], false);
        let e = env(&sys);
        let mut unread = row("person.screen_sharing", "person", Outcome::Unknown);
        unread.why = Some("launchctl print-disabled: timed out".into());
        let state = HostState {
            report: Some(report(Mode::Plan, vec![unread])),
            ..HostState::default()
        };
        inventory::save_state(&sys, &paths(), &a.uuid, &state).unwrap();
        assert_eq!(person(&e, &[], true), ExitCode::FAILURE);
        // A refusal saved as the report (an older state file) measured nothing either.
        let old = HostState {
            report: Some(refusal("expired issued=1 host-clock=2")),
            ..HostState::default()
        };
        inventory::save_state(&sys, &paths(), &a.uuid, &old).unwrap();
        assert_eq!(person(&e, &[], true), ExitCode::FAILURE);
        // Every row read and nothing outstanding: all clear.
        let done = HostState {
            report: Some(report(
                Mode::Plan,
                vec![row("person.screen_sharing", "person", Outcome::Ok)],
            )),
            ..HostState::default()
        };
        inventory::save_state(&sys, &paths(), &a.uuid, &done).unwrap();
        assert_eq!(person(&e, &[], true), ExitCode::SUCCESS);
    }

    fn person_report(aterm_app: Option<(&str, Outcome)>) -> HostReport {
        let mut items = vec![
            row("person.screen_sharing", "person", Outcome::Person),
            row("person.full_disk_access:aterm", "person", Outcome::Person),
            row("person.developer_tools:aterm", "person", Outcome::Person),
        ];
        if let Some((from, outcome)) = aterm_app {
            let mut app = row("aterm.app", "aterm", outcome);
            app.from = Some(from.into());
            app.to = Some("aterm 0.106.0 (team A66A9P66Z7), owned by admin".into());
            items.insert(0, app);
        }
        report(Mode::Plan, items)
    }

    #[test]
    fn screen_is_refused_while_screen_sharing_is_outstanding_whichever_step_is_named() {
        let a = lab(1, UUID_1);
        let r = person_report(None);
        let err = walk_steps(&r, &a, false, Some("developer_tools:aterm")).unwrap_err();
        assert!(err.contains("Screen Sharing is not on"), "{err}");
        // At the Mac's keyboard it is just the first step.
        let (steps, waiting) = walk_steps(&r, &a, true, None).unwrap();
        assert_eq!(steps.len(), 3);
        assert!(waiting.is_empty());
    }

    #[test]
    fn before_apply_installs_aterm_its_switches_wait_and_the_rest_are_walked() {
        let a = lab(1, UUID_1);
        let (steps, waiting) = walk_steps(
            &person_report(Some(("absent", Outcome::Planned))),
            &a,
            true,
            None,
        )
        .unwrap();
        assert_eq!(
            steps.iter().map(screen::Step::id).collect::<Vec<_>>(),
            ["screen_sharing"]
        );
        assert_eq!(waiting.len(), 2);
        for w in &waiting {
            assert!(
                matches!(&w.done, screen::Done::NotGranted(why) if why.contains("aterm machines apply lab-01")),
                "{w:?}"
            );
        }
        // Installed by the apply this report is from, or already there: walked.
        for installed in [
            ("absent", Outcome::Changed),
            ("aterm 0.105.0", Outcome::Planned),
        ] {
            let (steps, waiting) =
                walk_steps(&person_report(Some(installed)), &a, true, None).unwrap();
            assert_eq!(steps.len(), 3, "{installed:?}");
            assert!(waiting.is_empty());
        }
    }

    #[test]
    fn a_walk_plans_again_when_a_step_reads_granted_even_one_granted_before_it() {
        let a = lab(1, UUID_1);
        let walked = |done: screen::Done| {
            vec![screen::Walked {
                step: "full_disk_access:aterm".into(),
                done,
            }]
        };
        let told = |done| tell(&a, &walked(done), &mut |_| {});
        assert_eq!(told(screen::Done::AlreadyGranted), (false, true));
        assert_eq!(told(screen::Done::Granted), (false, true));
        assert_eq!(told(screen::Done::NotGranted("hang".into())), (true, false));
        assert_eq!(
            told(screen::Done::AtConsole("at the Mac".into())),
            (false, false)
        );
    }

    #[test]
    fn a_mac_left_unlocked_is_the_walks_last_line_and_fails_it_whatever_the_steps_did() {
        let a = lab(1, UUID_1);
        let walked = |done: screen::Done| {
            vec![screen::Walked {
                step: "full_disk_access:aterm".into(),
                done,
            }]
        };
        let ended = |walked: &[screen::Walked], session: &screen::Session| {
            let mut said = Vec::new();
            let code = end_walk(&a, "screen", walked, session, &mut |s| said.push(s));
            (code, said)
        };
        let unlocked = screen::Session::Unlocked("SACLockScreenImmediate answered 22".into());
        let line = "aterm machines screen: lab-01: the Mac is still logged in as admin and \
                    unlocked at its display: the lock after the walk failed \
                    (SACLockScreenImmediate answered 22)";
        // Every step granted, so the walk plans again; the line still comes after the plan.
        assert_eq!(
            ended(&walked(screen::Done::Granted), &unlocked),
            (
                ExitCode::FAILURE,
                vec![
                    Said::Step("lab-01: full_disk_access:aterm: granted".into()),
                    Said::Plan,
                    Said::Last(line.into()),
                ]
            )
        );
        // A step the walk stopped at, with no plan: the line is still the last.
        let stopped = walked(screen::Done::NotGranted("stopped by SIGINT".into()));
        let (code, said) = ended(&stopped, &unlocked);
        assert_eq!(code, ExitCode::FAILURE);
        assert_eq!(said.last(), Some(&Said::Last(line.into())));
        assert_eq!(said.len(), 2);
        // Locked again, or never logged in over the relay: no line, and the steps decide.
        for session in [screen::Session::Locked, screen::Session::Kept] {
            assert_eq!(
                ended(&walked(screen::Done::Granted), &session),
                (
                    ExitCode::SUCCESS,
                    vec![
                        Said::Step("lab-01: full_disk_access:aterm: granted".into()),
                        Said::Plan,
                    ]
                )
            );
            assert_eq!(ended(&stopped, &session).0, ExitCode::FAILURE);
        }
        // What the host said reaches the terminal inert.
        let (_, said) = ended(
            &stopped,
            &screen::Session::Unlocked("\u{1b}]52;c;aGVsbG8=\u{7}".into()),
        );
        assert!(
            matches!(said.last(), Some(Said::Last(l)) if inert(l)),
            "{said:?}"
        );
    }

    #[test]
    fn a_key_with_a_passphrase_that_ssh_agent_holds_signs_through_it_without_a_prompt() {
        let sys = Fake::root();
        let key = format!("{ROOT}/keys/signing");
        sys.on(
            &format!("/usr/bin/ssh-keygen -y -P '' -f {key}"),
            Ran::exit(
                255,
                "Load key: incorrect passphrase supplied to decrypt private key\n",
            ),
        );
        let held = format!("SSH_AUTH_SOCK=\u{2026} /usr/bin/ssh-add -T {key}.pub");
        // In the agent at first; then no longer (another walk removed it).
        sys.on(&held, Ran::ok(""));
        sys.on(&held, Ran::exit(1, "Agent signature failed\n"));
        let mut e = env(&sys);
        e.agent_sock = Some("/tmp/agent.sock".into());
        assert_eq!(signing(&e, Path::new(&key)), Ok(Signing::Agent));
        // Not in the agent: the terminal when there is one, else a refusal.
        e.tty = true;
        assert_eq!(signing(&e, Path::new(&key)), Ok(Signing::Terminal));
        e.tty = false;
        assert!(
            signing(&e, Path::new(&key))
                .unwrap_err()
                .contains("ssh-agent does not hold it")
        );
        // No agent at all: never asked.
        e.agent_sock = None;
        e.tty = true;
        let before = sys.calls().len();
        assert_eq!(signing(&e, Path::new(&key)), Ok(Signing::Terminal));
        assert!(!sys.calls()[before..].iter().any(|c| c.contains("ssh-add")));
    }

    // ── principals, upgrades ────────────────────────────────────────────────────────────

    #[test]
    fn the_principal_comes_from_allowed_signers_by_key_before_the_local_host_name() {
        let a = lab(1, UUID_1);
        let sys = controller(&[&a], false);
        sys.on(
            "/usr/sbin/scutil --get LocalHostName",
            Ran::ok("renamed-mac\n"),
        );
        let e = env(&sys);
        let inv = inventory_of(&sys);
        assert_eq!(principals(&inv, &e), ["signing@old-name"]);
        // A stale serial file still gives max(stored, the floor the host reports) + 1.
        sys.file(&format!("{ROOT}/serial"), b"5\n");
        let mut hello = hello_of(&a);
        hello.floors = vec![crate::protocol::Floor {
            principal: "signing@old-name".into(),
            serial: 41,
        }];
        let floor = floor_of(&hello, &principals(&inv, &e));
        let b = body(&e, &a, floor, Action::Plan, Disrupt::WhenIdle).unwrap();
        assert_eq!(b.serial, 42);
        // A name the last rekey replaced still counts.
        sys.file(
            &format!("{ROOT}/allowed_signers.previous"),
            format!("signing@older namespaces=\"aterm-machines\" ssh-ed25519 {SIGNING_BLOB}\n")
                .as_bytes(),
        );
        assert_eq!(principals(&inv, &e), ["signing@old-name", "signing@older"]);
        // Only when no list names the key: the LocalHostName init used.
        let unlisted = Fake::root();
        unlisted.on(
            "/usr/sbin/scutil --get LocalHostName",
            Ran::ok("renamed-mac\n"),
        );
        assert_eq!(
            principals(&Inventory::default(), &env(&unlisted)),
            ["signing@renamed-mac"]
        );
    }

    #[test]
    fn an_upgrade_is_an_app_zip_only_when_the_bundle_verifies_as_aterms_team_here() {
        let exe = Path::new("/Applications/aterm (dev).app/Contents/MacOS/aterm");
        let archive = |bundle: bool| enroll::Archive {
            path: "/s/aterm-agent.zip".into(),
            sha256: "ab".repeat(32),
            exe: "aterm (dev).app/Contents/MacOS/aterm".into(),
            bundle,
        };
        let check = format!(
            "/usr/bin/codesign --verify --deep --strict -R {} '/Applications/aterm (dev).app'",
            crate::port::shell_word(APP_ZIP_REQUIREMENT)
        );
        let sys = Fake::root();
        sys.on(&check, Ran::ok(""));
        assert_eq!(upgrade_kind(&sys, exe, &archive(true)), "app-zip");
        let sys = Fake::root();
        sys.on(
            &check,
            Ran::exit(
                3,
                "test-requirement: code failed to satisfy specified code requirement(s)\n",
            ),
        );
        assert_eq!(upgrade_kind(&sys, exe, &archive(true)), "dev-zip");
        let sys = Fake::root();
        assert_eq!(
            upgrade_kind(&sys, Path::new("/w/aterm-machines-dev"), &archive(false)),
            "dev-zip"
        );
        assert!(sys.calls().is_empty(), "a bare binary is not checked");
    }

    // ── rekey ───────────────────────────────────────────────────────────────────────────

    #[test]
    fn a_new_list_that_only_mentions_the_signing_key_in_a_comment_is_refused() {
        let a = lab(1, UUID_1);
        let sys = controller(&[&a], false);
        sys.file(
            "/tmp/new_signers",
            format!(
                "# signing@old-name namespaces=\"aterm-machines\" ssh-ed25519 {SIGNING_BLOB}\n\
                 recovery@old-name namespaces=\"aterm-machines\" ssh-ed25519 {RECOVERY_BLOB}\n"
            )
            .as_bytes(),
        );
        let code = rekey(&env(&sys), &[], true, Path::new("/tmp/new_signers"), None);
        assert_eq!(code, ExitCode::from(2));
        assert!(
            !sys.calls().iter().any(|c| c.starts_with("/usr/bin/ssh ")),
            "no host asked"
        );
    }

    #[test]
    fn the_recovery_keys_public_half_is_found_beside_it_or_kept_on_the_controller() {
        let sys = Fake::root();
        let e = env(&sys);
        sys.file(
            &format!("{ROOT}/keys/recovery.pub"),
            b"ssh-ed25519 AAAA recovery@ctl\n",
        );
        assert_eq!(
            public_of(&e, Path::new("/Volumes/KEY/recovery")).unwrap(),
            PathBuf::from(format!("{ROOT}/keys/recovery.pub"))
        );
        sys.file(
            "/Volumes/KEY/recovery.pub",
            b"ssh-ed25519 AAAA recovery@ctl\n",
        );
        assert_eq!(
            public_of(&e, Path::new("/Volumes/KEY/recovery")).unwrap(),
            PathBuf::from("/Volumes/KEY/recovery.pub")
        );
        let err = public_of(&e, Path::new("/Volumes/KEY/other")).unwrap_err();
        assert!(
            err.contains("/Volumes/KEY/other.pub") && err.contains("keys/other.pub"),
            "{err}"
        );
    }

    #[test]
    fn the_controllers_list_is_replaced_keeping_the_old_one_and_hosts_behind_are_known() {
        let (a, b) = (lab(1, UUID_1), lab(2, UUID_2));
        let sys = controller(&[&a, &b], false);
        let e = env(&sys);
        let old = sys.written(&format!("{ROOT}/allowed_signers")).unwrap();
        let new = format!("signing@new namespaces=\"aterm-machines\" ssh-ed25519 {SIGNING_BLOB}\n");
        record_signers(&e, &a, &text_sha256(&new));
        replace_signers(&e, &new).unwrap();
        assert_eq!(
            sys.written(&format!("{ROOT}/allowed_signers.previous"))
                .unwrap(),
            old
        );
        assert_eq!(
            sys.written(&format!("{ROOT}/allowed_signers")).unwrap(),
            new.as_bytes()
        );
        assert_eq!(state_of(&sys, &a).signers_sha256, Some(text_sha256(&new)));
        assert_eq!(state_of(&sys, &b).signers_sha256, None);
        // A later rekey with the same list, catching up b, keeps the list b still holds.
        replace_signers(&e, &new).unwrap();
        assert_eq!(
            sys.written(&format!("{ROOT}/allowed_signers.previous"))
                .unwrap(),
            old
        );
        // The renamed principal and the old one both find the key's floors.
        assert_eq!(
            principals(&inventory_of(&sys), &e),
            ["signing@new", "signing@old-name"]
        );
    }

    // ── GitHub ──────────────────────────────────────────────────────────────────────────

    const GH: &str = "/opt/homebrew/bin/gh";
    const ALICE_OLD: &str = "AAAAC3NzaC1lZDI1NTE5AAAAIJpFPtSPTOuLIj6oqQTj8ndNDVNHFp4renP47+wxFYao";
    const ALICE_NEW: &str = "AAAAC3NzaC1lZDI1NTE5AAAAIHv2Xf9kgkNkpV3WltnLoVx25AQOEx+mch1DHT/oZdSa";

    /// A controller whose gh holds a token for `fleetbot`.
    fn with_gh(sys: &Fake) {
        sys.stat_of(
            GH,
            crate::port::Stat {
                uid: 0,
                gid: 0,
                mode: 0o755,
                dir: false,
                symlink: false,
                len: 1,
            },
        );
        sys.on(
            &format!("{GH} auth token --hostname github.com --user fleetbot"),
            Ran::ok("gho_ThisIsNotARealToken0123456789abcdef\n"), // gitleaks:allow
        );
        sys.on(
            &format!("GH_TOKEN=\u{2026} {GH} api user --jq .login"),
            Ran::ok("fleetbot\n"),
        );
    }

    fn list_keys() -> String {
        format!("GH_TOKEN=\u{2026} {GH} api user/keys --paginate --jq '.[] | [.id, .key] | @tsv'")
    }

    fn post_key(body: &str) -> String {
        format!(
            "GH_TOKEN=\u{2026} {GH} api --method POST user/keys -f title=aterm-machines:lab-01:alice -f 'key=ssh-ed25519 {body}' --jq .id"
        )
    }

    fn key_row(body: &str) -> ItemReport {
        let mut r = row("github.key@alice", "github", Outcome::Ok);
        r.detail = Some(format!("ssh-ed25519 {body} aterm-machines:lab-01:alice"));
        r
    }

    #[test]
    fn a_users_new_key_is_registered_and_the_one_it_replaced_deleted() {
        let a = lab(1, UUID_1);
        let sys = controller(&[&a], true);
        with_gh(&sys);
        let e = env(&sys);
        let inv = inventory_of(&sys);
        let state = HostState {
            github_keys: vec![("alice".into(), 5)],
            github_key_text: vec![(5, format!("ssh-ed25519 {ALICE_OLD}"))],
            ..HostState::default()
        };
        inventory::save_state(&sys, &paths(), &a.uuid, &state).unwrap();
        // The same key again: nothing is sent.
        after_run(
            &e,
            &inv,
            &a,
            report(Mode::Apply, vec![key_row(ALICE_OLD)]),
            Source::Baseline,
        );
        assert!(
            !sys.calls().iter().any(|c| c.contains("GH_TOKEN")),
            "{:?}",
            sys.calls()
        );
        // A reset home made a new key: registered, and the old id deleted.
        sys.on(
            &list_keys(),
            Ran::ok(&format!("5\tssh-ed25519 {ALICE_OLD}\n")),
        );
        sys.on(&post_key(ALICE_NEW), Ran::ok("9\n"));
        sys.on(
            &list_keys(),
            Ran::ok(&format!(
                "5\tssh-ed25519 {ALICE_OLD}\n9\tssh-ed25519 {ALICE_NEW}\n"
            )),
        );
        sys.on(
            &list_keys(),
            Ran::ok(&format!("9\tssh-ed25519 {ALICE_NEW}\n")),
        );
        sys.on(
            &format!("GH_TOKEN=\u{2026} {GH} api --method DELETE user/keys/5"),
            Ran::ok(""),
        );
        after_run(
            &e,
            &inv,
            &a,
            report(Mode::Apply, vec![key_row(ALICE_NEW)]),
            Source::Baseline,
        );
        let state = state_of(&sys, &a);
        assert_eq!(state.github_keys, [("alice".to_owned(), 9)]);
        assert_eq!(
            state.github_key_text,
            [(9, format!("ssh-ed25519 {ALICE_NEW}"))]
        );
        assert!(sys.calls().contains(&post_key(ALICE_NEW)));
    }

    #[test]
    fn a_revoked_key_is_never_registered_again_and_a_plan_registers_nothing() {
        let a = lab(1, UUID_1);
        let sys = controller(&[&a], true);
        with_gh(&sys);
        let e = env(&sys);
        let inv = inventory_of(&sys);
        let state = HostState {
            github_keys: vec![("alice".into(), 5)],
            ..HostState::default()
        };
        inventory::save_state(&sys, &paths(), &a.uuid, &state).unwrap();
        sys.on(
            &list_keys(),
            Ran::ok(&format!("5\tssh-ed25519 {ALICE_OLD}\n")),
        );
        sys.on(&list_keys(), Ran::ok(""));
        sys.on(
            &format!("GH_TOKEN=\u{2026} {GH} api --method DELETE user/keys/5"),
            Ran::ok(""),
        );
        assert_eq!(github_revoke(&e, "lab-01"), ExitCode::SUCCESS);
        let state = state_of(&sys, &a);
        assert!(state.github_keys.is_empty());
        assert_eq!(state.github_revoked, [format!("ssh-ed25519 {ALICE_OLD}")]);
        // The host still has that key and still reports it: not sent again.
        let calls = sys.calls().len();
        after_run(
            &e,
            &inv,
            &a,
            report(Mode::Apply, vec![key_row(ALICE_OLD)]),
            Source::Baseline,
        );
        assert!(!sys.calls()[calls..].iter().any(|c| c.contains("POST")));
        // A plan reports the key too, and registers nothing whatever it carries.
        after_run(
            &e,
            &inv,
            &a,
            report(Mode::Plan, vec![key_row(ALICE_NEW)]),
            Source::Baseline,
        );
        assert!(!sys.calls()[calls..].iter().any(|c| c.contains("POST")));
        // A key the host makes anew is registered by the next apply.
        sys.on(&list_keys(), Ran::ok(""));
        sys.on(&post_key(ALICE_NEW), Ran::ok("11\n"));
        after_run(
            &e,
            &inv,
            &a,
            report(Mode::Apply, vec![key_row(ALICE_NEW)]),
            Source::Baseline,
        );
        assert_eq!(state_of(&sys, &a).github_keys, [("alice".to_owned(), 11)]);
    }
}
