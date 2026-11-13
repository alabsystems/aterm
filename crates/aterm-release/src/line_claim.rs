// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Fleet exclusion for candidate lines. A line's ref is a lease: its holder renews it (the
//! ref's `at`) with each completed row, step boundary and ownership audit, and between those
//! by a heartbeat for as long as its own process tree shows work ([`Pulse`]), every write a
//! compare-and-swap on the ref; one not renewed within its bound ([`lease_s`]) is any cutter's
//! to take, so no Mac's state (dead, hung, asleep, unreachable, old or recordless) holds a line
//! longer. A holder whose lease was taken finds the ref moved at its next write or audit and
//! stops before it signs or publishes. Transport errors leave a holder's work running.

use crate::ledger::{GitCli, GitRunner, git_ok};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

/// A stopped monitor starts no further remote operation. An in-flight native write
/// settles under its existing deadline and durable intent before the worker joins;
/// killing a transport child cannot establish whether its write took effect.
/// The separate sampler keeps local progress observable throughout that wait.
struct Network<'a> {
    repo: PathBuf,
    remote: crate::github_remote::Remote,
    stopped: &'a AtomicBool,
}

/// The claim protocol has three remote operations, separate from local object Git.
trait ClaimStore: GitRunner {
    fn ref_oid(&self, name: &str) -> Result<Option<String>, String>;
    fn import(&self, oid: &str) -> Result<(), String>;
    fn compare_and_swap(&self, name: &str, before: &str, after: &str, main: Option<&str>) -> Write;
}

impl<'a> Network<'a> {
    fn new(repo: &Path, stopped: &'a AtomicBool) -> Result<Self, String> {
        if stopped.load(Ordering::SeqCst) {
            return Err("fleet claim monitor stopped".into());
        }
        Ok(Self {
            repo: repo.into(),
            remote: crate::github_remote::Remote::origin(repo)?,
            stopped,
        })
    }

    fn running(&self) -> Result<(), String> {
        if self.stopped.load(Ordering::SeqCst) {
            Err("fleet claim monitor stopped".into())
        } else {
            Ok(())
        }
    }
}

impl GitRunner for Network<'_> {
    fn git(&self, args: &[&str]) -> crate::ledger::Result<crate::ledger::RunOut> {
        self.running().map_err(crate::ledger::Error::new)?;
        let mut cmd = crate::ledger::object_git(&self.repo)?;
        cmd.args(["-c", "protocol.allow=never"]).args(args);
        let out = aterm_uds::deadline::output_until_from_unless(
            &mut cmd,
            aterm_uds::spawnfd::Stdio::null(),
            std::time::Instant::now() + std::time::Duration::from_secs(120),
            &mut || self.stopped.load(Ordering::SeqCst),
        )
        .map_err(|e| crate::ledger::Error::new(e.to_string()))?;
        Ok(crate::ledger::RunOut {
            status: out.status.code().unwrap_or(-1),
            stdout: out.stdout,
            stderr: out.stderr,
        })
    }
    fn git_input(
        &self,
        args: &[&str],
        input: &[u8],
    ) -> crate::ledger::Result<crate::ledger::RunOut> {
        self.running().map_err(crate::ledger::Error::new)?;
        // Object creation keeps Git's recorded author and committer context.
        // object_git deliberately drops it along with inherited Git routing.
        let mut local = vec!["-c", "protocol.allow=never"];
        local.extend_from_slice(args);
        GitCli::new(&self.repo).git_input(&local, input)
    }
}

impl ClaimStore for Network<'_> {
    fn ref_oid(&self, name: &str) -> Result<Option<String>, String> {
        self.running()?;
        self.remote.get(name)
    }

    fn import(&self, oid: &str) -> Result<(), String> {
        self.running()?;
        self.remote.fetch_exact(oid)
    }

    fn compare_and_swap(&self, name: &str, before: &str, after: &str, main: Option<&str>) -> Write {
        if let Err(error) = self.running() {
            return Write::Offline(error);
        }
        match self
            .remote
            .write_claim_ref(name, (!before.is_empty()).then_some(before), after, main)
        {
            Ok(crate::github_remote::ClaimWrite::Written) => Write::Written(after.into()),
            Ok(crate::github_remote::ClaimWrite::Conflict) => Write::Race,
            Ok(crate::github_remote::ClaimWrite::InventoryChanged { main, why }) => {
                Write::InventoryChanged { main, why }
            }
            Err(error) => Write::Offline(error),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct Holder {
    pub node: String,
    pub host: String,
    pub sid: String,
    pub pid: u32,
    pub boot: String,
    pub start: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct State {
    pub line: crate::line::Line,
    pub holder: Holder,
    pub step: String,
    pub boundary: String,
    pub at: u64,
    pub p95_s: u64,
    /// What the renewal at `at` rested on: the holder's last evidence of progress, in words a
    /// refusal quotes ([`admit`]). Empty in a claim a cutter older than the heartbeat wrote.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub evidence: String,
    pub rows: Vec<String>,
}

/// Whether a continued local run claim continues the fleet holder. A native exec keeps the exact
/// holder. The one deployment boundary also admits the live ancestor a pre-B1 cutter marked as
/// handed before it started this image: node, host, session and boot must still agree, and
/// `legacy_parent` independently proves that ancestor's recorded pid/start. No current writer
/// creates that local marker.
fn continues_holder_with(
    continued: bool,
    previous: &Holder,
    current: &Holder,
    legacy_parent: &dyn Fn(&Holder) -> bool,
) -> bool {
    continued
        && (previous == current
            || (previous.node == current.node
                && previous.host == current.host
                && previous.sid == current.sid
                && previous.boot == current.boot
                && legacy_parent(previous)))
}

fn continues_holder(continued: bool, previous: &Holder, current: &Holder) -> bool {
    let ancestors = crate::cutter::ancestors();
    continues_holder_with(continued, previous, current, &|holder| {
        ancestors.contains(&holder.pid)
            && aterm_agent::release_progress::identity(holder.pid)
                .is_some_and(|(boot, start)| boot == holder.boot && start == holder.start)
    })
}

/// What the holder's own Mac showed of its process. Only `Dead` is evidence; a failure to reach
/// or read that Mac, and any answer short of a kernel read of death, is `Unproven`, with why.
/// Neither ever holds a line: the lease does ([`admit`]).
#[derive(Clone, Debug)]
enum Observation {
    Dead,
    Unproven(String),
}

/// The longest any lease runs unrenewed, whatever p95 its holder's step history claims: a holder
/// Mac's own `steps.tsv` cannot stretch another Mac's wait past four hours.
pub(crate) const MAX_LEASE_S: u64 = 4 * 3600;

/// How long a claim's lease runs after its last renewal: the ref's `at`, which its holder
/// advances with each completed row, step boundary and ownership audit, and every
/// [`PULSE_RENEW_S`] between those while its process tree works ([`Pulse`]), each a
/// compare-and-swap on the ref. The bound is at least half an hour and twice the current step's
/// p95 ([`p95`]), and at most [`MAX_LEASE_S`].
fn lease_s(state: &State) -> u64 {
    1800.max(state.p95_s.saturating_mul(2)).min(MAX_LEASE_S)
}

/// Whether the ref's lease has run out at `now` (this Mac's clock; `at` is the holder's). An
/// `at` further ahead of this clock than a whole lease is no renewal any sane clock wrote, and
/// a Mac whose clock runs that fast must not hold the line forever.
fn expired(state: &State, now: u64) -> bool {
    let bound = lease_s(state);
    now.saturating_sub(state.at) > bound || state.at.saturating_sub(now) > bound
}

/// Whether this cutter may take `state`'s line at `now`. The holder keeps it only while its
/// lease is renewed on the ref; once that lapses any cutter takes it, whatever the holder's Mac
/// does or says. Its Mac's kernel proving the holder dead (`observe`, asked only while the lease
/// still runs) only shortens the wait. The taking write is a compare-and-swap from the exact ref
/// read here, so of several cutters at most one wins. `Err` says when the lease expires.
fn admit(
    state: &State,
    line: &crate::line::Line,
    now: u64,
    observe: impl FnOnce() -> Observation,
) -> Result<(), String> {
    if expired(state, now) {
        return Ok(());
    }
    let why = match observe() {
        Observation::Dead => return Ok(()),
        Observation::Unproven(why) => why,
    };
    let until = state.at.saturating_add(lease_s(state));
    let evidence = if state.evidence.is_empty() {
        "none recorded (its cutter predates the lease heartbeat)"
    } else {
        &state.evidence
    };
    Err(format!(
        "CUTTING ELSEWHERE: v{} by {} at {} head {}; lease renewed at {} runs until {until} \
         ({}s from now; its last evidence of progress: {evidence}; its process is not proven \
         dead: {why}); red: {}; help: land a fix with verify land; the holder carries it with \
         ship release --fix, or any cutter takes the line once its lease expires",
        line.version,
        state.holder.host,
        state.step,
        state.line.head(),
        state.at,
        until.saturating_sub(now),
        red_items(state)
    ))
}

/// How a contender asks for a version's fleet line.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Take {
    /// Start the line, or take it from a holder whose lease expired or whose death is proven
    /// ([`admit`]).
    Contend,
    /// The same process, or its exec'd successor, going on with the line it holds
    /// ([`continues_holder`] proves the identity; the caller's word alone grants nothing).
    Continue,
    /// `--supersede`: a session's explicit replacement of the unpublished line. It discards the
    /// holder's candidate whatever that holder is doing, so neither the holder's liveness nor its
    /// lease decides anything; the compare-and-swap is its fence as for every take.
    Supersede,
}

fn reference(version: &str) -> Result<String, String> {
    let is_digits = |part: &str| !part.is_empty() && part.bytes().all(|b| b.is_ascii_digit());
    let mut parts = version.split('.');
    let valid = matches!(
        (parts.next(), parts.next(), parts.next(), parts.next()),
        (Some(a), Some(b), Some(c), None) if is_digits(a) && is_digits(b) && is_digits(c)
    );
    if !valid {
        return Err("line version must be numeric X.Y.Z".into());
    }
    Ok(format!("refs/aterm-release/line/{version}"))
}

fn remote(git: &dyn ClaimStore, name: &str) -> Result<Option<(String, State)>, String> {
    remote_expected(git, name, None)
}

fn remote_expected(
    git: &dyn ClaimStore,
    name: &str,
    expected: Option<&str>,
) -> Result<Option<(String, State)>, String> {
    let observed = git.ref_oid(name)?;
    claim_ref_unchanged(expected, observed.as_deref().unwrap_or(""))?;
    let Some(sha) = observed else {
        return Ok(None);
    };
    git.import(&sha)?;
    state_at(git, name, &sha).map(|state| Some((sha, state)))
}

fn state_at(git: &dyn GitRunner, name: &str, sha: &str) -> Result<State, String> {
    let out = git_ok(git, &["show", &format!("{sha}:line.toml")]).map_err(|e| e.to_string())?;
    parse_state(&out.stdout_utf8(), name)
}

fn parse_state(text: &str, name: &str) -> Result<State, String> {
    let state: State =
        aterm_toml::from_str(text).map_err(|e| format!("invalid fleet line: {e}"))?;
    if reference(&state.line.version)? != name {
        return Err("fleet line version does not match its ref".into());
    }
    Ok(state)
}

/// A fresh state-only tree, with the previous state as parent. The candidate checkout and
/// its index are never used to store claim data.
fn commit(git: &dyn GitRunner, previous: &str, state: &State) -> Result<String, String> {
    let text = aterm_toml::to_string(state).map_err(|e| e.to_string())?;
    let blob = git
        .git_input(&["hash-object", "-w", "--stdin"], text.as_bytes())
        .map_err(|e| e.to_string())?;
    if !blob.success() {
        return Err(blob.stderr_utf8());
    }
    let entry = format!("100644 blob {}\tline.toml\0", blob.stdout_utf8().trim());
    let tree = git
        .git_input(&["mktree", "-z"], entry.as_bytes())
        .map_err(|e| e.to_string())?;
    if !tree.success() {
        return Err(tree.stderr_utf8());
    }
    let tree_sha = tree.stdout_utf8();
    let mut args = vec!["commit-tree", tree_sha.trim()];
    if !previous.is_empty() {
        args.extend(["-p", previous]);
    }
    let out = git
        .git_input(&args, b"release line progress\n")
        .map_err(|e| e.to_string())?;
    if !out.success() {
        return Err(out.stderr_utf8());
    }
    Ok(out.stdout_utf8().trim().to_owned())
}

#[derive(Debug, PartialEq, Eq)]
enum Write {
    Written(String),
    Race,
    Offline(String),
    InventoryChanged { main: String, why: String },
}

enum ClaimAttempt<T> {
    Done(T),
    InventoryChanged { main: String, why: String },
}

/// Only an unsent inventory race restarts the complete claim selection. The
/// callback must refresh main after this floor and recheck supersession and
/// holder evidence. A receive failure never enters this loop.
fn inventory_retry<T>(
    mut attempt: impl FnMut(Option<&str>) -> Result<ClaimAttempt<T>, String>,
) -> Result<T, String> {
    let mut floor = None;
    for index in 0..3 {
        match attempt(floor.as_deref())? {
            ClaimAttempt::Done(value) => return Ok(value),
            ClaimAttempt::InventoryChanged { main, why } => {
                if index == 2 {
                    return Err(why);
                }
                floor = Some(main);
            }
        }
    }
    unreachable!("the third attempt returns")
}

fn claim_main(
    remote: &crate::github_remote::Remote,
    repo: &Path,
    line: &crate::line::Line,
    floor: Option<&str>,
) -> Result<(String, Option<crate::line::Supersession>), String> {
    let main = remote.refresh_main_after(floor)?;
    let current = crate::line::supersession(&GitCli::new(repo), &line.version)?;
    if current
        .as_ref()
        .is_some_and(|r| r.replacement.base != line.base)
    {
        return Err(
            "this line was superseded on main; discard the retired journal and resume".into(),
        );
    }
    Ok((main, current))
}

fn claim_ref_unchanged(expected: Option<&str>, observed: &str) -> Result<(), String> {
    if expected.is_some_and(|expected| expected != observed) {
        return Err(claim_ref_conflict());
    }
    Ok(())
}

fn claim_ref_conflict() -> String {
    "could not claim fleet line: requested ref changed during an inventory retry (conflict; side_effect=none)".into()
}

fn replace(git: &dyn ClaimStore, name: &str, previous: &str, state: &State) -> Write {
    replace_at_main(git, name, previous, state, None)
}

fn replace_at_main(
    git: &dyn ClaimStore,
    name: &str,
    previous: &str,
    state: &State,
    main: Option<&str>,
) -> Write {
    let sha = match commit(git, previous, state) {
        Ok(sha) => sha,
        Err(e) => return Write::Offline(e),
    };
    git.compare_and_swap(name, previous, &sha, main)
}

/// What a holder's own host said of it: whether its progress record's process runs, and its
/// kernel's judgement of the claim's exact identity (`release_holder`). Both are kernel reads on
/// that host; `None` is no such read.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct HostAnswer {
    record: Option<Option<bool>>,
    judged: Option<Option<bool>>,
}

/// Whether a progress record names exactly `holder`.
fn names(record: &aterm_json::Value, holder: &Holder) -> bool {
    let named = &record["holder"];
    named["node"].as_str() == Some(&holder.node)
        && named["host"].as_str() == Some(&holder.host)
        && named["sid"].as_str() == Some(&holder.sid)
        && named["pid"].as_u64() == Some(u64::from(holder.pid))
        && named["boot"].as_str() == Some(&holder.boot)
        && named["start"].as_u64() == Some(holder.start)
}

fn identity_of(holder: &Holder) -> aterm_ctl::release_holder::Identity {
    aterm_ctl::release_holder::Identity {
        pid: holder.pid,
        boot: holder.boot.clone(),
        start: holder.start,
    }
}

fn alive_word(word: &str) -> Option<bool> {
    match word {
        "1" => Some(true),
        "0" => Some(false),
        _ => None,
    }
}

/// This host's records and its kernel's own judgement of the claim's holder identity: a holder
/// whose record was lost is still judged ([`aterm_agent::release_progress::judge`]).
fn local_observation(state: &State, directory: &Path) -> Observation {
    let identity = identity_of(&state.holder);
    let mut answer = HostAnswer {
        judged: identity.read(&aterm_agent::release_progress::holder_row(&identity)),
        ..HostAnswer::default()
    };
    for line in aterm_agent::release_progress::survey(directory) {
        let Some((alive, data)) = line
            .strip_prefix("release_line alive=")
            .and_then(|raw| raw.split_once(" data="))
        else {
            continue;
        };
        let Ok(record) = aterm_json::from_str::<aterm_json::Value>(&aterm_codec::pct::decode(data))
        else {
            continue;
        };
        if names(&record, &state.holder) {
            answer.record.get_or_insert(alive_word(alive));
        }
    }
    observation(&answer)
}

/// The holder host's answer, combined. A process cannot return from death, so either read's
/// death is the answer; a record that may be a moment old cannot outvote a fresh death. Every
/// other answer is unproven, with what it showed.
fn observation(answer: &HostAnswer) -> Observation {
    let why = match (answer.record, answer.judged) {
        (Some(Some(false)), _) | (_, Some(Some(false))) => return Observation::Dead,
        (Some(Some(true)), _) | (_, Some(Some(true))) => "its process runs",
        (Some(None), _) | (_, Some(None)) => {
            "its host's kernel could not establish whether its process runs"
        }
        (None, None) => {
            "its host's answer holds neither a progress record of it nor a judgement of its \
             process (an aterm too old to judge one)"
        }
    };
    Observation::Unproven(why.into())
}

/// This kernel judges a holder only where the claim names this very Mac: its node and its host.
/// A node alone may be shared by a state folder cloned onto another Mac, whose processes this
/// kernel cannot see.
fn observe(state: &State, here: &Holder, directory: &Path) -> Observation {
    if state.holder.node == here.node {
        if state.holder.host != here.host {
            return Observation::Unproven(format!(
                "it names this Mac's fleet node from host {}, not this host {}",
                state.holder.host, here.host
            ));
        }
        return local_observation(state, directory);
    }
    remote_observation(state, &installed_ctl)
}

/// The installed, signed aterm: the LAN broker admits only aterm's own code to the fleet, so
/// this tool, built from a checkout, reads a peer through it. The app bundle comes first; a
/// build left on PATH is not its code.
fn installed_aterm() -> Option<PathBuf> {
    [PathBuf::from(
        "/Applications/aterm.app/Contents/MacOS/aterm",
    )]
    .into_iter()
    .chain(std::env::var_os("HOME").map(|home| Path::new(&home).join(".local/bin/aterm")))
    .find(|path| path.is_file())
}

/// How long one installed `aterm ctl` read may take: its own `--timeout` bounds the fleet
/// exchange, and this its whole run.
const CTL_WITHIN: std::time::Duration = std::time::Duration::from_secs(30);

/// `aterm ctl <args>` of the installed aterm: its stdout, or why there is none. A survey whose
/// peer did not answer exits non-zero and still prints that peer's error row, which is kept.
fn installed_ctl(args: &[&str]) -> Result<String, String> {
    let aterm = installed_aterm().ok_or("no installed aterm on this Mac reads the fleet")?;
    let mut command = aterm_uds::spawnfd::command(&aterm);
    command.args(["ctl", "--timeout", "10"]).args(args);
    let out = aterm_uds::deadline::output_within(&mut command, CTL_WITHIN)
        .map_err(|e| format!("could not run {}: {e}", aterm.display()))?;
    let stdout = String::from_utf8_lossy(&out.stdout);
    if !stdout.trim().is_empty() {
        return Ok(stdout.into_owned());
    }
    let err = String::from_utf8_lossy(&out.stderr);
    Err(aterm_codec::tty::one_line(err.trim(), 300))
}

/// Ask the holder's own Mac, through the installed aterm (`run`), to judge the claim's exact
/// identity. A holder's Mac older than the `holder=` argument refuses it, and is then asked the
/// plain survey, which carries its progress record where it keeps one. An installed aterm older
/// than `--holder` names no proven node, and so is never read as an answer.
fn remote_observation(
    state: &State,
    run: &dyn Fn(&[&str]) -> Result<String, String>,
) -> Observation {
    let holder = &state.holder;
    let word = identity_of(holder).word();
    let plain = [
        "survey",
        "--host",
        holder.node.as_str(),
        "--timeout",
        "3",
        "--json",
    ];
    let mut asked = plain.to_vec();
    asked.extend(["--holder", word.as_str()]);
    let read = |args: &[&str]| run(args).and_then(|json| holder_answer(&json, holder));
    match read(&asked) {
        Ok(answer) => observation(&answer),
        Err(why_asked) => match read(&plain) {
            Ok(answer) => observation(&answer),
            Err(why_plain) => Observation::Unproven(format!(
                "its Mac's judgement could not be read ({why_asked}), nor its records ({why_plain})"
            )),
        },
    }
}

/// `aterm ctl survey --host … --json`'s answer about `holder`: only from a peer that proved, in
/// the handshake, the very node the claim names. Every other outcome is why there is none.
fn holder_answer(json: &str, holder: &Holder) -> Result<HostAnswer, String> {
    let hosts: aterm_json::Value =
        aterm_json::from_str(json).map_err(|e| format!("unreadable survey: {e}"))?;
    let peer = hosts
        .as_array()
        .and_then(|hosts| {
            hosts
                .iter()
                .find(|host| host["fleet"].as_bool() == Some(true))
        })
        .ok_or("the survey holds no answer from that Mac")?;
    if let Some(error) = peer["error"].as_str() {
        return Err(format!("it did not answer: {error}"));
    }
    if peer["unproven"].as_bool() == Some(true)
        || peer["proven_node"].as_str() != Some(holder.node.as_str())
    {
        return Err(
            "its answer did not prove that node's identity (or the installed aterm \
                    predates naming it)"
                .into(),
        );
    }
    let judged = match &peer["release_holder"] {
        aterm_json::Value::Null => None,
        value => Some(value["alive"].as_bool()),
    };
    let record = peer["release_lines"].as_array().and_then(|rows| {
        rows.iter()
            .find(|row| names(row, holder))
            .map(|row| row["alive"].as_bool())
    });
    Ok(HostAnswer { record, judged })
}

fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

pub fn existing(repo: &Path, version: &str) -> Result<Option<crate::line::Line>, String> {
    existing_with_holder(repo, version).map(|snapshot| snapshot.line)
}

/// The selected line and the exact head/holder evidence read from its fleet claim.
pub struct Existing {
    pub line: Option<crate::line::Line>,
    pub holder: Option<(String, String)>,
    /// The claim is held by another Mac ([`another_mac`]); false where there is no claim.
    pub elsewhere: bool,
}

/// Whether `holder` is on another Mac than this one, by the fleet node a claim records
/// (`Lease::take`). A Mac that cannot name its own node cannot show a claim is its own, and
/// could not take one either.
fn another_mac(holder: &Holder) -> bool {
    #[cfg(unix)]
    let node = aterm_agent::fleet_node();
    #[cfg(not(unix))]
    let node = aterm_agent::survey_facts::machine_label();
    node.is_none_or(|node| node != holder.node)
}

/// Keep the holder from the same claim read used for selection.
pub fn existing_with_holder(repo: &Path, version: &str) -> Result<Existing, String> {
    let name = reference(version)?;
    let stopped = AtomicBool::new(false);
    let old = remote(&Network::new(repo, &stopped)?, &name)?.map(|(_, state)| state);
    let holder = old
        .as_ref()
        .map(|s| (s.line.head().to_string(), holder_words(&s.holder)));
    let elsewhere = old.as_ref().is_some_and(|s| another_mac(&s.holder));
    let line = current(&GitCli::new(repo), version, old.map(|s| s.line))?;
    Ok(Existing {
        line,
        holder,
        elsewhere,
    })
}

fn holder_words(holder: &Holder) -> String {
    format!(
        "{} (node {}, session {}, pid {}, boot {}, start {})",
        holder.host, holder.node, holder.sid, holder.pid, holder.boot, holder.start
    )
}

/// Apply main's durable retirement to the independently read claim.
pub(crate) fn current(
    git: &dyn GitRunner,
    version: &str,
    old: Option<crate::line::Line>,
) -> Result<Option<crate::line::Line>, String> {
    let current = crate::line::supersession(git, version)?;
    Ok(match current {
        Some(record) => Some(old.as_ref().map_or_else(
            || record.replacement.clone(),
            |old| preferred(old, &record.replacement),
        )),
        None => old,
    })
}

/// A read cannot take ownership, but must steer preflight to the saved candidate so an
/// already-staged line is not rejected for a different commit chosen from moving main.
///
/// Only an unfinished journal of the candidate's version is this clone's line of that version:
/// it alone can be the line a supersession retired, or the line the fleet's claim extends. Any
/// other journal (a finished release, another version's) says nothing about this version's
/// line and carries nothing into it; it stays only where it already names the fleet's head.
pub fn follow(
    backend: &mut dyn crate::release::Backend,
    candidate: &mut crate::release::Candidate,
    journal: &mut Option<crate::release::Journal>,
) -> Result<(), crate::release::Refusal> {
    if let Some(selected) = backend.fleet_existing(&candidate.version)? {
        let own = journal
            .as_ref()
            .filter(|j| !j.finished() && j.version == candidate.version);
        if let Some(record) = backend.fleet_supersession(&candidate.version)?
            && own.is_some_and(|j| j.line().base != record.replacement.base)
        {
            return Err(crate::release::Refusal::new(
                format!(
                    "this journal's line was superseded by {}: {}",
                    record.replacement.base, record.reason
                ),
                "drop the retired journal with ship release --forget, then resume the current line",
            ));
        }
        let selected = own
            .filter(|journal| journal.commit == candidate.commit)
            .map_or_else(
                || selected.clone(),
                |journal| preferred(&journal.line(), &selected),
            );
        if own.is_none()
            && journal
                .as_ref()
                .is_some_and(|j| j.commit != selected.head())
        {
            *journal = None;
        }
        adopt(candidate, journal, selected);
    }
    Ok(())
}

fn preferred(proposed: &crate::line::Line, existing: &crate::line::Line) -> crate::line::Line {
    if proposed.base == existing.base && proposed.hops.starts_with(&existing.hops) {
        proposed.clone()
    } else {
        existing.clone()
    }
}

fn adopt(
    candidate: &mut crate::release::Candidate,
    journal: &mut Option<crate::release::Journal>,
    selected: crate::line::Line,
) {
    candidate.commit = selected.head().into();
    candidate.version = selected.version.clone();
    candidate.bump_from = None;
    candidate.why = "the fleet line".into();
    if let Some(journal) = journal
        .as_mut()
        .filter(|journal| journal.commit == selected.head())
    {
        journal.line = Some(selected);
        return;
    }
    let asks = journal.as_ref().map(|j| j.asks.clone()).unwrap_or_default();
    *journal = Some(crate::release::Journal {
        version: selected.version.clone(),
        commit: selected.head().into(),
        started: selected.started,
        line: Some(selected),
        asks,
        ..crate::release::Journal::default()
    });
}

/// Selection happens before carrying fixes or judging a newer main: an adopter resumes
/// the fleet's exact line, with this clone's records, and carries fixes onto that head.
pub fn select(
    backend: &mut dyn crate::release::Backend,
    continued: bool,
    candidate: &mut crate::release::Candidate,
    journal: &mut Option<crate::release::Journal>,
    started: u64,
) -> Result<(), String> {
    let proposed = journal
        .as_ref()
        .filter(|j| j.commit == candidate.commit)
        .map_or_else(
            || crate::line::Line::start(&candidate.version, &candidate.commit, started),
            crate::release::Journal::line,
        );
    let selected = backend.fleet_line(&proposed, continued)?;
    if selected != proposed {
        adopt(candidate, journal, selected);
    }
    Ok(())
}

/// Who takes a claim, and the clock and view of the holder's Mac it judges by.
struct Taker<'a> {
    repo: &'a Path,
    holder: &'a Holder,
    /// The state folder whose release-lines registry keeps this taker's host record.
    registry: &'a Path,
    take: Take,
    now: &'a dyn Fn() -> u64,
    observe: &'a dyn Fn(&State) -> Observation,
}

/// What one read-admit-write of the claim did.
enum Attempt {
    Taken(Box<Lease>),
    /// Another writer moved the ref after it was read: read and judge again.
    Race,
    InventoryChanged {
        main: String,
        why: String,
        previous: String,
    },
}

/// One attempt to take `name` for `taker`: read the ref, admit its holder ([`admit`], unless
/// this process continues it), record this taker on its host, and compare-and-swap from exactly
/// the ref read, so of several takers of one read at most one is written.
fn attempt(
    git: &dyn ClaimStore,
    taker: &Taker<'_>,
    name: &str,
    line: &crate::line::Line,
    current: Option<&crate::line::Supersession>,
    main: Option<&str>,
    expected_ref: Option<&str>,
) -> Result<Attempt, String> {
    let old = remote_expected(git, name, expected_ref)?;
    if let Some((_, state)) = &old
        && taker.take != Take::Supersede
        && !continues_holder(taker.take == Take::Continue, &state.holder, taker.holder)
    {
        admit(state, line, (taker.now)(), || (taker.observe)(state))?;
    }
    let selected = old.as_ref().map_or_else(
        || line.clone(),
        |(_, state)| {
            // A carried fix extends this exact line. A different base selected on a moving main
            // is discarded when adopting the fleet's existing line.
            if let Some(record) = current {
                preferred(line, &preferred(&state.line, &record.replacement))
            } else {
                preferred(line, &state.line)
            }
        },
    );
    let previous_holder = old
        .as_ref()
        .filter(|(_, state)| state.line.head() == selected.head())
        .map(|(_, state)| holder_words(&state.holder));
    let state = State {
        line: selected,
        holder: taker.holder.clone(),
        step: "commit".into(),
        boundary: "claimed".into(),
        at: (taker.now)(),
        p95_s: default_p95_s("commit"),
        evidence: "the claim itself, just taken".into(),
        rows: Vec::new(),
    };
    let previous = old.as_ref().map_or("", |(sha, _)| sha.as_str());
    let local = taker
        .registry
        .join(aterm_agent::release_progress::DIRECTORY)
        .join(format!(
            "{}-{}-{}.json",
            line.version, taker.holder.pid, taker.holder.start
        ));
    let mut claim = Lease {
        repo: taker.repo.into(),
        name: name.to_owned(),
        sha: previous.into(),
        state,
        local,
        previous_holder,
    };
    // Make the identity observable before publishing it. A failure here can never strand a
    // remote lease with no host record from which to prove its death.
    claim.keep_local()?;
    Ok(
        match replace_at_main(git, name, previous, &claim.state, main) {
            Write::Race => Attempt::Race,
            Write::Offline(e) => return Err(format!("could not claim fleet line: {e}")),
            Write::InventoryChanged { main, why } => Attempt::InventoryChanged {
                main,
                why: format!("could not claim fleet line: {why}"),
                previous: previous.to_owned(),
            },
            Write::Written(sha) => {
                claim.sha = sha;
                Attempt::Taken(Box::new(claim))
            }
        },
    )
}

/// One owned lease; the monitor retries its pending writes independently of progress.
struct Lease {
    repo: PathBuf,
    name: String,
    sha: String,
    pub state: State,
    local: PathBuf,
    previous_holder: Option<String>,
}

impl Lease {
    fn take(repo: &Path, line: &crate::line::Line, take: Take) -> Result<Self, String> {
        let env = aterm_agent::harness::cli::Env::from_process()?;
        let host =
            aterm_agent::survey_facts::machine_label().ok_or("no canonical fleet host label")?;
        #[cfg(unix)]
        let node = aterm_agent::fleet_node().ok_or("no authenticated fleet node identity")?;
        #[cfg(not(unix))]
        let node = host.clone();
        let pid = std::process::id();
        let (boot, start) = aterm_agent::release_progress::identity(pid)
            .ok_or("cannot read this process's identity")?;
        let holder = Holder {
            node,
            host,
            sid: std::env::var("ATERM_SESSION_ID").unwrap_or_default(),
            pid,
            boot,
            start,
        };
        let name = reference(&line.version)?;
        let stopped = AtomicBool::new(false);
        let git = Network::new(repo, &stopped)?;
        let taker = Taker {
            repo,
            holder: &holder,
            registry: &env.state,
            take,
            now: &now,
            observe: &|state| observe(state, &holder, &env.state),
        };
        let mut expected_ref = None;
        inventory_retry(|floor| {
            loop {
                let (main, current) = claim_main(
                    &crate::github_remote::Remote::origin(repo)?,
                    repo,
                    line,
                    floor,
                )?;
                match attempt(
                    &git,
                    &taker,
                    &name,
                    line,
                    current.as_ref(),
                    Some(&main),
                    expected_ref.as_deref(),
                )? {
                    Attempt::Taken(claim) => return Ok(ClaimAttempt::Done(*claim)),
                    Attempt::Race if expected_ref.is_some() => return Err(claim_ref_conflict()),
                    Attempt::Race => {}
                    Attempt::InventoryChanged {
                        main,
                        why,
                        previous,
                    } => {
                        expected_ref = Some(previous);
                        return Ok(ClaimAttempt::InventoryChanged { main, why });
                    }
                }
            }
        })
    }

    fn keep_local(&self) -> Result<(), String> {
        keep_local(&self.local, &self.state)
    }

    /// The public cut calls this even when no new row completed.
    fn write(&mut self, stopped: &AtomicBool) -> Result<bool, String> {
        let git = match Network::new(&self.repo, stopped) {
            Ok(git) => git,
            Err(_) => return Ok(false),
        };
        self.write_with(&git)
    }

    fn write_with(&mut self, git: &dyn ClaimStore) -> Result<bool, String> {
        match replace(git, &self.name, &self.sha, &self.state) {
            Write::Written(sha) => {
                self.sha = sha;
                Ok(true)
            }
            Write::Offline(_) | Write::InventoryChanged { .. } => Ok(false),
            Write::Race => match remote(git, &self.name) {
                Ok(Some((sha, state))) if state.holder == self.state.holder => {
                    self.sha = sha;
                    Ok(false)
                }
                Ok(_) => Err("COULD NOT RUN: fleet line adopted by another holder".into()),
                Err(_) => Ok(false),
            },
        }
    }
}

type Reply = std::sync::mpsc::Sender<Result<bool, String>>;
enum Request {
    Wake,
    Audit(Reply),
    Stop,
}

fn keep_local(path: &Path, state: &State) -> Result<(), String> {
    // Fleet surveys need identity and the latest boundary, and refuse registry
    // files over 4 MiB before stripping rows. Keep outcome history in State and
    // its remote Git record; do not copy it into this local observation.
    #[derive(Serialize)]
    struct LocalRecord<'a> {
        line: &'a crate::line::Line,
        holder: &'a Holder,
        step: &'a str,
        boundary: &'a str,
        at: u64,
        p95_s: u64,
        evidence: &'a str,
        rows: &'a [String],
    }
    let record = LocalRecord {
        line: &state.line,
        holder: &state.holder,
        step: &state.step,
        boundary: &state.boundary,
        at: state.at,
        p95_s: state.p95_s,
        evidence: &state.evidence,
        rows: &[],
    };
    let text = aterm_json::to_vec(&record).map_err(|e| e.to_string())?;
    std::fs::create_dir_all(path.parent().ok_or("no registry parent")?)
        .map_err(|e| e.to_string())?;
    crate::durable::write_atomic_durable(path, &text, 0o600).map_err(|e| e.to_string())
}

struct Local {
    state: Mutex<State>,
    path: PathBuf,
    /// Held across each claim write and each read of this process's tree ([`Local::beat`]): a
    /// write's own short-lived git children are then never among the processes a read counts,
    /// so the holder's renewals are no evidence of its work.
    claim_io: Mutex<()>,
}

impl Local {
    fn new(state: State, path: PathBuf) -> Self {
        Self {
            state: Mutex::new(state),
            path,
            claim_io: Mutex::new(()),
        }
    }

    fn snapshot(&self) -> Result<State, String> {
        self.state
            .lock()
            .map(|state| state.clone())
            .map_err(|e| e.to_string())
    }

    fn update(&self, update: impl FnOnce(&mut State)) -> Result<(), String> {
        let mut state = self.state.lock().map_err(|e| e.to_string())?;
        update(&mut state);
        keep_local(&self.path, &state)
    }

    fn sample(&self, reader: &mut Rows, logs: &Path) -> Result<(), String> {
        let rows = reader.read(logs);
        if rows.is_empty() {
            return Ok(());
        }
        self.update(|state| {
            state.boundary = rows.last().cloned().unwrap_or_default();
            state.rows.extend(rows);
            state.at = now();
            state.evidence = "a completed row".into();
        })
    }

    /// One heartbeat at `now`: read this process's tree with `read` (no claim write running),
    /// and renew the lease where `pulse` finds it due ([`Pulse::beat`]). Whether it renewed;
    /// the host record is rewritten only then. A heartbeat never ends a cut: a host record it
    /// could not rewrite is the next row's or boundary's to report, as it is today.
    fn beat(
        &self,
        pulse: &mut Pulse,
        read: &mut dyn FnMut() -> (Option<aterm_toolchain::stall::Work>, std::time::Instant),
        now: u64,
    ) -> bool {
        let (work, at) = {
            let _quiet = self.quiet();
            read()
        };
        let evidence = pulse.look(work, at);
        let Ok(mut state) = self.state.lock() else {
            return false;
        };
        if !pulse.beat(&mut state, evidence, now) {
            return false;
        }
        let _ = keep_local(&self.path, &state);
        true
    }

    /// No claim write and no read of this process's tree runs while this is held. It guards no
    /// data, so a holder that panicked leaves nothing to distrust.
    fn quiet(&self) -> std::sync::MutexGuard<'_, ()> {
        self.claim_io
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

/// How often a holder reads its own process tree for evidence of work: twice a minute, the
/// pace the release pump reads a run's tree at.
const PULSE_LOOK: std::time::Duration = std::time::Duration::from_secs(30);

/// The heartbeat renews a lease nothing else renewed for this long: every five minutes, a
/// sixth of the shortest lease ([`lease_s`]), so five renewal writes in a row may fail before
/// a working holder's lease can lapse.
const PULSE_RENEW_S: u64 = 300;

/// The longest the heartbeat alone keeps a line: past this long with no completed row and no
/// new step, working processes renew nothing more (an audit renews the lease itself and does
/// not restart this count). It is [`MAX_LEASE_S`], above
/// verify's three-hour ceiling on one check's work and more than twice the longest row-less
/// stretch measured in a qualification that passed (108 minutes, 0.199.0), so a tree that
/// spins without ever finishing anything still loses its line.
const PULSE_ALONE_S: u64 = MAX_LEASE_S;

/// The holder's heartbeat: whether its own process tree made progress since the last look, by
/// the judgement the release pump and every capacity wait already use
/// ([`aterm_toolchain::stall::Stall`]: its processes together runnable a quarter of a core or
/// writing a MiB a minute, or one started or ended, or waiting in turn behind a holder that
/// works). A look that finds progress renews a lease not renewed for [`PULSE_RENEW_S`]; a look
/// that finds none, or cannot read the tree, renews nothing. So `at` never advances without
/// work in the minute before it, and a holder whose tree stops (hung, stopped, waiting on what
/// never comes) is taken one lease after its last renewal, which is at most a minute after its
/// work stopped. A row, a step boundary and an audit renew as before, whatever the tree shows.
struct Pulse {
    pid: u32,
    stall: aterm_toolchain::stall::Stall,
    /// The reads of the last [`aterm_toolchain::stall::BUSY_OVER`] and the newest before
    /// them, oldest first: the stretch the tree's progress is judged over, which the
    /// evidence's words measure too. Empty before the first read and after one that read no
    /// tree: a lone read shows no rate.
    seen: std::collections::VecDeque<(std::time::Instant, aterm_toolchain::stall::Work)>,
    /// The run's own newest progress (its step and how many rows it completed) and when the
    /// heartbeat first saw it: what [`PULSE_ALONE_S`] counts from.
    mark: Option<((String, usize), u64)>,
}

impl Pulse {
    fn new(pid: u32) -> Self {
        Self {
            pid,
            stall: aterm_toolchain::stall::Stall::default(),
            seen: std::collections::VecDeque::new(),
            mark: None,
        }
    }

    /// Book one read of the holder's tree: the evidence it is, in words, where the tree made
    /// progress since the read before it; `None` where it made none, was not read, or this is
    /// the first read since it was.
    fn look(
        &mut self,
        work: Option<aterm_toolchain::stall::Work>,
        at: std::time::Instant,
    ) -> Option<String> {
        let idle = self.stall.read(self.pid, work, at);
        let Some(work) = work else {
            self.seen.clear();
            return None;
        };
        self.seen.push_back((at, work));
        while self.seen.get(1).is_some_and(|(then, _)| {
            at.saturating_duration_since(*then) >= aterm_toolchain::stall::BUSY_OVER
        }) {
            self.seen.pop_front();
        }
        let (then, was) = self.seen.front().copied().filter(|_| self.seen.len() > 1)?;
        if idle? != std::time::Duration::ZERO {
            return None;
        }
        let over = at.saturating_duration_since(then).as_secs();
        Some(format!(
            "its process tree, {} processes ({} {over} s earlier), ran {} s on cores and wrote \
             {} MiB in those {over} s",
            work.members,
            was.members,
            work.cpu_ns.saturating_sub(was.cpu_ns) / 1_000_000_000,
            work.written.saturating_sub(was.written) >> 20,
        ))
    }

    /// Renew `state`'s lease at `now` on `evidence`, where the heartbeat is due: there is
    /// evidence, nothing renewed the lease for [`PULSE_RENEW_S`], and the run itself made
    /// progress (a row or a step) within [`PULSE_ALONE_S`]. Whether it renewed.
    fn beat(&mut self, state: &mut State, evidence: Option<String>, now: u64) -> bool {
        let progress = (state.step.clone(), state.rows.len());
        if self.mark.as_ref().is_none_or(|(seen, _)| *seen != progress) {
            self.mark = Some((progress, now));
        }
        let since = self.mark.as_ref().map_or(now, |(_, since)| *since);
        let Some(evidence) = evidence else {
            return false;
        };
        if now.saturating_sub(state.at) < PULSE_RENEW_S || now.saturating_sub(since) > PULSE_ALONE_S
        {
            return false;
        }
        state.at = now;
        state.evidence = evidence;
        true
    }
}

/// The heartbeat's thread, until `stop` or the run ends: a run that lost its line or caught a
/// signal is ending, and renews nothing more.
fn pulse(
    local: &Local,
    wake: &std::sync::mpsc::Sender<Request>,
    stop: &std::sync::mpsc::Receiver<()>,
) {
    let pid = std::process::id();
    let mut pulse = Pulse::new(pid);
    while let Err(std::sync::mpsc::RecvTimeoutError::Timeout) = stop.recv_timeout(PULSE_LOOK) {
        if failure().is_some() || crate::interrupt::caught().is_some() {
            break;
        }
        let mut read = || {
            (
                aterm_toolchain::stall::tree_work(pid, &[]),
                std::time::Instant::now(),
            )
        };
        if local.beat(&mut pulse, &mut read, now()) && wake.send(Request::Wake).is_err() {
            break;
        }
    }
}

/// A monitor retries writes with bounded backoff, while the cutter keeps working. It renews the
/// lease on completed rows, step boundaries and audits, and between them on the heartbeat of a
/// process tree that works ([`Pulse`]), never on a timer alone or on arbitrary log activity: a
/// holder that stops making progress lets its lease lapse.
pub struct Claim {
    pub line: crate::line::Line,
    pub previous_holder: Option<String>,
    tx: std::sync::mpsc::Sender<Request>,
    worker: Option<std::thread::JoinHandle<()>>,
    local: Arc<Local>,
    sampler: Option<std::thread::JoinHandle<()>>,
    sample_stop: std::sync::mpsc::Sender<()>,
    heart: Option<std::thread::JoinHandle<()>>,
    heart_stop: std::sync::mpsc::Sender<()>,
    stopped: Arc<AtomicBool>,
}

impl Claim {
    pub fn take(
        repo: &Path,
        line: &crate::line::Line,
        logs: &Path,
        take: Take,
    ) -> Result<Self, String> {
        let lease = Lease::take(repo, line, take)?;
        let line = lease.state.line.clone();
        let previous_holder = lease.previous_holder.clone();
        let local = Arc::new(Local::new(lease.state.clone(), lease.local.clone()));
        let stopped = Arc::new(AtomicBool::new(false));
        let (tx, rx) = std::sync::mpsc::channel();
        let (heart_stop, heart_rx) = std::sync::mpsc::channel();
        let beating = Arc::clone(&local);
        let waking = tx.clone();
        let heart = std::thread::spawn(move || pulse(&beating, &waking, &heart_rx));
        let (sample_stop, sample_rx) = std::sync::mpsc::channel();
        let mut reader = Rows::default();
        // Seed before either thread starts: old rows cannot prove new progress.
        reader.seed(logs);
        let logs = logs.to_path_buf();
        let sampling = Arc::clone(&local);
        let sampler = std::thread::spawn(move || {
            while let Err(std::sync::mpsc::RecvTimeoutError::Timeout) =
                sample_rx.recv_timeout(std::time::Duration::from_secs(1))
            {
                if let Err(error) = sampling.sample(&mut reader, &logs) {
                    fail(error);
                    break;
                }
            }
        });
        let publishing = Arc::clone(&local);
        let stopping = Arc::clone(&stopped);
        let worker =
            std::thread::spawn(move || publish_progress(lease, &publishing, &stopping, &rx));
        Ok(Self {
            line,
            previous_holder,
            tx,
            worker: Some(worker),
            local,
            sampler: Some(sampler),
            sample_stop,
            heart: Some(heart),
            heart_stop,
            stopped,
        })
    }

    pub fn boundary(
        &mut self,
        line: &crate::line::Line,
        step: &str,
        history: &str,
    ) -> Result<(), String> {
        if let Some(error) = failure() {
            return Err(error.into());
        }
        self.local.update(|state| {
            state.line = line.clone();
            state.step = step.into();
            state.boundary = "step boundary".into();
            state.at = now();
            state.p95_s = p95(history, step);
            state.evidence = "a step boundary".into();
        })?;
        self.tx.send(Request::Wake).map_err(|e| e.to_string())?;
        self.line = line.clone();
        Ok(())
    }

    /// A public write needs a successful ownership round trip, which renews the lease. Offline
    /// retries wait here, preserving the qualified candidate, rather than turning a transport
    /// error into a red; a lease taken meanwhile fails the audit.
    pub fn audit(&mut self) -> Result<(), String> {
        loop {
            let (tx, rx) = std::sync::mpsc::channel();
            self.tx
                .send(Request::Audit(tx))
                .map_err(|e| e.to_string())?;
            if rx.recv().map_err(|e| e.to_string())?? {
                return Ok(());
            }
            if crate::interrupt::caught().is_some() {
                return Err("interrupted while checking fleet ownership".into());
            }
            std::thread::sleep(std::time::Duration::from_secs(1));
        }
    }
}

static FAILURE: std::sync::OnceLock<String> = std::sync::OnceLock::new();

/// Record the fleet line as lost and stop this process on its own account
/// ([`crate::interrupt::stop_self`]): the running step's group is sent the `SIGTERM`, and the
/// next boundary ends the run on the loss ([`failure`]), whose refusal still settles a private
/// head the run holds.
fn fail(error: String) {
    if FAILURE.set(error).is_ok() {
        crate::interrupt::stop_self();
    }
}

fn publish_progress(
    lease: Lease,
    local: &Local,
    stopped: &AtomicBool,
    rx: &std::sync::mpsc::Receiver<Request>,
) {
    publish_progress_with(lease, local, rx, &mut |lease| lease.write(stopped));
}

fn publish_progress_with(
    mut lease: Lease,
    local: &Local,
    rx: &std::sync::mpsc::Receiver<Request>,
    write: &mut dyn FnMut(&mut Lease) -> Result<bool, String>,
) {
    let mut written = lease.state.clone();
    let mut backoff = 1;
    let mut next = std::time::Instant::now();
    loop {
        let audit = match rx.recv_timeout(std::time::Duration::from_secs(1)) {
            Ok(Request::Stop) | Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
            Ok(Request::Audit(reply)) => Some(reply),
            Ok(Request::Wake) | Err(std::sync::mpsc::RecvTimeoutError::Timeout) => None,
        };
        if let Some(error) = failure() {
            if let Some(reply) = audit {
                let _ = reply.send(Err(error.into()));
            }
            continue;
        }
        // An audit precedes a signature or a publication: its write renews the lease, so the
        // holder that passes it holds the line a full bound from then.
        if audit.is_some()
            && let Err(error) = local.update(|state| {
                state.at = now();
                state.evidence = "an ownership audit".into();
            })
        {
            fail(error);
            if let Some(reply) = audit {
                let _ = reply.send(Err("could not renew the fleet line lease".into()));
            }
            continue;
        }
        let current = match local.snapshot() {
            Ok(state) => state,
            Err(error) => {
                fail(error);
                continue;
            }
        };
        if std::time::Instant::now() < next || (audit.is_none() && current == written) {
            if let Some(reply) = audit {
                let _ = reply.send(Ok(false));
            }
            continue;
        }
        lease.state = current;
        // No state lock is held during network I/O. The independent sampler and the
        // cutter's step boundaries keep the host record current throughout this call.
        let result = {
            let _quiet = local.quiet();
            write(&mut lease)
        };
        match &result {
            Ok(true) => {
                written = lease.state.clone();
                backoff = 1;
            }
            Ok(false) => backoff = (backoff * 2).min(60),
            Err(error) => fail(error.clone()),
        }
        next = std::time::Instant::now()
            + std::time::Duration::from_secs(if result == Ok(true) { 0 } else { backoff });
        if let Some(reply) = audit {
            let _ = reply.send(result);
        }
    }
}

pub fn failure() -> Option<&'static str> {
    FAILURE.get().map(String::as_str)
}

/// Lose the fleet line exactly as the progress worker does when another holder adopts it
/// ([`fail`]): record the loss and stop this process with its `SIGTERM`, so a caller first
/// installs the release's handlers ([`crate::interrupt::install`]). FAILURE and the caught
/// signal are process-global and never cleared, so only a test's re-executed child calls this;
/// the `it` binary is the only caller, so the seam is `cfg(test)`.
#[cfg(test)]
#[allow(
    dead_code,
    reason = "only the integration harness's re-executed child calls it; the binary's lint-only test target has no caller"
)]
pub fn lose_line_for_test(error: &str) {
    fail(error.to_owned());
}

impl Drop for Claim {
    fn drop(&mut self) {
        self.stopped.store(true, Ordering::SeqCst);
        let _ = self.sample_stop.send(());
        let _ = self.heart_stop.send(());
        let _ = self.tx.send(Request::Stop);
        if let Some(heart) = self.heart.take() {
            let _ = heart.join();
        }
        if let Some(sampler) = self.sampler.take() {
            let _ = sampler.join();
        }
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

/// A step's p95 where this Mac's `steps.tsv` holds no time for it (a first cut here, or a step
/// that never finished here), in seconds: a stated, conservative stand-in, never 0, which made
/// every such lease the half-hour floor. `qualify` is an hour and a half: above the p95 the
/// fleet's usual cutter recorded over its releases (4,958 to 5,236 s) and the longest qualify
/// branch of a 36-core Mac's 0.199.0 (4,718 s), read 2026-11-12. `checks` is 35 minutes, above
/// the 1,011 to 1,982 s it took on that Mac. Every other step is a quarter of an hour, above
/// the longest any took there (a publish, 655 s), and under the lease's floor either way.
fn default_p95_s(step: &str) -> u64 {
    match step {
        "qualify" => 5_400,
        "checks" => 2_100,
        _ => 900,
    }
}

/// How long `step` takes on this Mac at its 95th percentile, from its `steps.tsv` (`history`:
/// `epoch\tversion\trow\tsecs`): the largest p95 among the step's own row and the rows of its
/// parts (`qualify-build`, `publish-sign`: `<step>-<part>`). A step runs at least as long as its
/// longest part, and its own row alone is no measure: only a step that ends green writes one,
/// and a resumed step writes the short remainder (a 36-core Mac's one `qualify` row of 0.199.0
/// was 405 s beside a `qualify-build` of 4,718 s). With no row at all it is
/// [`default_p95_s`].
fn p95(history: &str, step: &str) -> u64 {
    let mut rows: std::collections::BTreeMap<&str, Vec<u64>> = std::collections::BTreeMap::new();
    for line in history.lines() {
        let mut fields = line.split('\t');
        let (Some(_), Some(_), Some(row), Some(secs), None) = (
            fields.next(),
            fields.next(),
            fields.next(),
            fields.next(),
            fields.next(),
        ) else {
            continue;
        };
        let of_step = row
            .strip_prefix(step)
            .is_some_and(|part| part.is_empty() || part.starts_with('-'));
        if of_step && let Ok(secs) = secs.parse() {
            rows.entry(row).or_default().push(secs);
        }
    }
    rows.into_values()
        .filter_map(|mut samples| {
            samples.sort_unstable();
            let at = samples
                .len()
                .saturating_mul(95)
                .div_ceil(100)
                .saturating_sub(1);
            samples.get(at).copied()
        })
        .max()
        .unwrap_or_else(|| default_p95_s(step))
}

#[derive(Default)]
struct Rows(std::collections::BTreeMap<PathBuf, RowCursor>);

const PROGRESS_LINE_BYTES: usize = 64 * 1024;

struct RowCursor {
    // Retain the physical file so its identity cannot be recycled between polls.
    file: std::fs::File,
    identity: (u64, u128),
    offset: u64,
    partial: Vec<u8>,
    discarding: bool,
}

impl RowCursor {
    fn consume(&mut self, mut bytes: &[u8], mut rows: Option<&mut Vec<String>>) {
        while !bytes.is_empty() {
            let newline = bytes.iter().position(|byte| *byte == b'\n');
            let length = newline.map_or(bytes.len(), |at| at + 1);
            let part = &bytes[..length];
            self.offset += length as u64;
            if !self.discarding {
                if self.partial.len() + part.len() <= PROGRESS_LINE_BYTES {
                    self.partial.extend_from_slice(part);
                } else {
                    self.partial.clear();
                    self.discarding = true;
                }
            }
            if newline.is_some() {
                if !self.discarding
                    && let Some(rows) = rows.as_mut()
                    && let Ok(text) = std::str::from_utf8(&self.partial)
                    && let Some(row) = completed_row(text)
                {
                    rows.push(row);
                }
                self.partial.clear();
                self.discarding = false;
            }
            bytes = &bytes[length..];
        }
    }
}

impl Rows {
    // Historical completions establish cursor position, never new progress.
    // Do not decode or collect summaries that the initial seed would discard.
    fn seed(&mut self, logs: &Path) {
        self.scan(logs, None);
    }

    fn read(&mut self, logs: &Path) -> Vec<String> {
        let mut rows = Vec::new();
        self.scan(logs, Some(&mut rows));
        rows
    }

    fn scan(&mut self, logs: &Path, mut rows: Option<&mut Vec<String>>) {
        use std::io::{Read as _, Seek as _};
        let Ok(entries) = std::fs::read_dir(logs) else {
            return;
        };
        for entry in entries.flatten() {
            let name = entry.file_name();
            if name != "transcript.log"
                && Path::new(&name)
                    .extension()
                    .is_none_or(|ext| ext != "events")
            {
                continue;
            }
            if !entry.file_type().is_ok_and(|kind| kind.is_file()) {
                continue;
            }
            let path = entry.path();
            let Ok(file) = crate::durable::open_regular_file(&path) else {
                continue;
            };
            let (Ok(stat), Ok(metadata)) = (aterm_dirfd::fstat(&file), file.metadata()) else {
                continue;
            };
            #[cfg(unix)]
            let inode = u128::from(stat.st_ino);
            #[cfg(not(unix))]
            let inode = stat.st_ino;
            let identity = (stat.st_dev, inode);
            if self
                .0
                .get(&path)
                .is_none_or(|cursor| cursor.identity != identity)
            {
                self.0.insert(
                    path.clone(),
                    RowCursor {
                        file,
                        identity,
                        offset: 0,
                        partial: Vec::new(),
                        discarding: false,
                    },
                );
            }
            let cursor = self.0.get_mut(&path).unwrap();
            // A different stream or an observed truncation cannot complete an
            // earlier stream's fragment. A truncate-and-regrow entirely between
            // samples is not observable here; these remain incomplete diagnostics.
            if metadata.len() < cursor.offset {
                cursor.offset = 0;
                cursor.partial.clear();
                cursor.discarding = false;
            }
            if cursor
                .file
                .seek(std::io::SeekFrom::Start(cursor.offset))
                .is_err()
            {
                continue;
            }
            // Snapshot the extent: a continuously appending writer cannot keep
            // this sample chasing its tail indefinitely.
            let mut remaining = metadata.len().saturating_sub(cursor.offset);
            let mut bytes = [0; 8192];
            while remaining != 0 {
                let limit = usize::try_from(remaining.min(bytes.len() as u64)).unwrap();
                let Ok(n) = cursor.file.read(&mut bytes[..limit]) else {
                    break;
                };
                if n == 0 {
                    break;
                }
                cursor.consume(&bytes[..n], rows.as_deref_mut());
                remaining -= n as u64;
            }
        }
    }
}

fn completed_row(text: &str) -> Option<String> {
    let trimmed = text.trim();
    let text = trimmed
        .strip_prefix("release-event: ")
        .or_else(|| trimmed.strip_prefix("verify-event: "))
        .unwrap_or(trimmed);
    if !text.starts_with('{') {
        return None;
    }
    let aterm_json::Value::Object(mut event) = aterm_json::from_str(text).ok()? else {
        return None;
    };
    let kind = event.get("event").and_then(aterm_json::Value::as_str)?;
    if !matches!(
        kind,
        "stage-finished" | "test-binary-finished" | "check-finished" | "test-case-failed"
    ) {
        return None;
    }
    let kind = event.remove("event")?;
    // This is the producer's reported name, possibly clipped, not a full case identity.
    let case = match event.remove("case") {
        Some(value @ aterm_json::Value::String(_)) => value,
        _ => aterm_json::Value::Null,
    };
    // Stage verdicts live in these reported decisions, not a top-level outcome.
    let decisions = match event.remove("decisions") {
        Some(aterm_json::Value::Array(rows)) if kind.as_str() == Some("stage-finished") => {
            aterm_json::Value::Array(
                rows.into_iter()
                    .map(
                        |row| match (row["outcome"].as_str(), row["detail"].as_str()) {
                            (Some(outcome), Some(detail)) => aterm_json::json!({
                                "outcome": outcome, "detail": detail,
                            }),
                            _ => aterm_json::Value::Null,
                        },
                    )
                    .collect(),
            )
        }
        _ => aterm_json::Value::Null,
    };
    let mut take = |key: &str| event.remove(key).unwrap_or(aterm_json::Value::Null);
    let summary = aterm_json::Value::Object(aterm_json::Map::from([
        ("event".to_string(), kind),
        ("stage".to_string(), take("stage")),
        ("check".to_string(), take("check")),
        ("test".to_string(), take("test")),
        ("case".to_string(), case),
        ("outcome".to_string(), take("outcome")),
        ("detail".to_string(), take("detail")),
        ("findings".to_string(), take("findings")),
        ("decisions".to_string(), decisions),
    ]));
    Some(summary.to_string())
}

fn red_items(state: &State) -> String {
    state
        .rows
        .iter()
        .filter_map(|row| {
            let event: aterm_json::Value = aterm_json::from_str(row).ok()?;
            // A `test-case-failed` is a red by its outcome: a case its block spares (refused,
            // out of disk) reads NOT RUN.
            let top_level_red =
                matches!(event["outcome"].as_str(), Some("FAIL" | "fail" | "failed"));
            let stage_red = event["event"].as_str() == Some("stage-finished")
                && event["decisions"].as_array().is_some_and(|decisions| {
                    decisions
                        .iter()
                        .any(|decision| decision["outcome"].as_str() == Some("FAIL"))
                });
            (top_level_red || stage_red).then(|| event.to_string())
        })
        .collect::<Vec<_>>()
        .join("; ")
}

pub fn status(repo: &Path) -> Result<(), String> {
    let remote = crate::github_remote::Remote::origin(repo)?;
    for row in status_rows(&remote)? {
        println!("{row}");
    }
    Ok(())
}

fn status_rows(remote: &crate::github_remote::Remote) -> Result<Vec<String>, String> {
    let mut rows = Vec::new();
    for name in remote.list("refs/aterm-release/line/")?.keys() {
        if let Some(sha) = remote.get(name)? {
            remote.fetch_exact(&sha)?;
            let bytes = remote.blob(&sha, Path::new("line.toml"))?;
            let text = std::str::from_utf8(&bytes)
                .map_err(|error| format!("invalid fleet line UTF-8: {error}"))?;
            let state = parse_state(text, name)?;
            rows.push(format!(
                "v{} {} {} step={} boundary={} at={} head={}",
                state.line.version,
                state.holder.host,
                state.holder.sid,
                state.step,
                state.boundary,
                state.at,
                state.line.head()
            ));
        }
    }
    Ok(rows)
}

#[cfg(test)]
#[path = "line_claim_tests.rs"]
mod tests;
