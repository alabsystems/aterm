// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Fleet exclusion for candidate lines. Git's lease is the serialization point; timestamps
//! on refs are never liveness evidence. Transport errors leave a holder's work running.

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

/// A host observation is tri-state. Failure to reach a peer is never evidence of death; it is
/// also no evidence of life, so an unread holder keeps its line only while its claim advances
/// ([`lease_expired`]).
#[derive(Clone, Debug)]
enum Observation {
    Dead,
    Live(Box<State>),
    Unknown,
}

fn progressing(state: &State, now: u64) -> bool {
    now.saturating_sub(state.at) <= 1800.max(state.p95_s.saturating_mul(2))
}

/// The fleet line is a lease, and a lease no one can read still ends. A holder whose host no
/// contender can observe has only the claim itself to show: when that has recorded no completed
/// row for `max(2 hours, four times the step's recorded p95)`, twice the bound an observed
/// holder gets, the lease is over. The displaced run, if it is alive after all, loses nothing
/// public: its next claim write finds another holder and stops it, and a cut waits for an
/// ownership round trip before it publishes.
fn lease_expired(state: &State, now: u64) -> bool {
    now.saturating_sub(state.at) > lease_seconds(state)
}

fn lease_seconds(state: &State) -> u64 {
    7200.max(state.p95_s.saturating_mul(4))
}

/// Positive host evidence adopts at once (a dead process) or at the progress bound (a live one
/// that stopped completing rows). A locally advancing boundary takes precedence over an old ref
/// when GitHub is unavailable. With no host evidence at all, only the lease's own expiry adopts.
fn adoptable(state: &State, observed: &Observation, now: u64) -> bool {
    match observed {
        Observation::Dead => true,
        Observation::Live(live) if live.holder == state.holder => {
            !progressing(state, now) && !progressing(live, now)
        }
        Observation::Live(_) => false,
        Observation::Unknown => lease_expired(state, now),
    }
}

/// How a contender asks for a version's fleet line.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Take {
    /// Start the line, or adopt it from a holder that is dead, stalled or expired.
    Contend,
    /// The same process, or its exec'd successor, going on with the line it holds.
    Continue,
    /// `--supersede`: a session's explicit replacement of the unpublished line. It discards the
    /// holder's candidate whatever that holder is doing, so the holder's liveness decides nothing.
    Supersede,
}

/// Whether this take replaces the claim `state` records. Only [`Take::Contend`] is judged by the
/// holder's evidence; the expected-old write that follows is the fence for every kind.
fn may_replace(
    take: Take,
    continues: bool,
    state: &State,
    observed: &Observation,
    now: u64,
) -> bool {
    continues || take == Take::Supersede || adoptable(state, observed, now)
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

fn local_observation(state: &State, directory: &Path) -> Observation {
    observation(state, &aterm_agent::release_progress::survey(directory))
}

fn observation(state: &State, lines: &[String]) -> Observation {
    for line in lines {
        let Some(raw) = line.strip_prefix("release_line alive=") else {
            continue;
        };
        let Some((alive, data)) = raw.split_once(" data=") else {
            continue;
        };
        let data = aterm_codec::pct::decode(data);
        let Ok(live) = aterm_json::from_str::<State>(&data) else {
            continue;
        };
        if live.holder != state.holder {
            continue;
        }
        return match alive {
            "0" => Observation::Dead,
            "1" => Observation::Live(Box::new(live)),
            _ => Observation::Unknown,
        };
    }
    Observation::Unknown
}

fn observe(state: &State, here: &Holder, directory: &Path) -> Observation {
    if state.holder.node == here.node {
        return local_observation(state, directory);
    }
    #[cfg(unix)]
    if let Ok(read) = aterm_agent::survey_fleet(&aterm_ctl::FleetAsk {
        host: Some(&state.holder.node),
        within: std::time::Duration::from_secs(3),
    }) {
        for peer in read.peers {
            if !peer.unproven
                && peer.node.as_deref() == Some(&state.holder.node)
                && let Ok(lines) = peer.answer
            {
                return observation(state, &lines);
            }
        }
    }
    Observation::Unknown
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
}

/// Keep the holder from the same claim read used for selection.
pub fn existing_with_holder(repo: &Path, version: &str) -> Result<Existing, String> {
    let name = reference(version)?;
    let stopped = AtomicBool::new(false);
    let old = remote(&Network::new(repo, &stopped)?, &name)?.map(|(_, state)| state);
    let holder = old
        .as_ref()
        .map(|s| (s.line.head().to_string(), holder_words(&s.holder)));
    let line = current(&GitCli::new(repo), version, old.map(|s| s.line))?;
    Ok(Existing { line, holder })
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
        let continued = take == Take::Continue;
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
        let mut expected_ref = None;
        inventory_retry(|floor| {
            loop {
                let (main, current) = claim_main(
                    &crate::github_remote::Remote::origin(repo)?,
                    repo,
                    line,
                    floor,
                )?;
                let old = remote_expected(&git, &name, expected_ref.as_deref())?;
                if let Some((_, state)) = &old {
                    let observed = observe(state, &holder, &env.state);
                    let continued = continues_holder(continued, &state.holder, &holder);
                    if !may_replace(take, continued, state, &observed, now()) {
                        if matches!(observed, Observation::Unknown)
                            || matches!(&observed, Observation::Live(live) if live.holder != state.holder)
                        {
                            return Err(format!(
                                "COULD NOT RUN: cannot establish fleet line holder {}'s process and progress; the existing cut continues until its lease expires ({} min with no completed row; {} so far); to replace the line now: targo --unverified ship release --commit <a newer main commit> --supersede",
                                state.holder.host,
                                lease_seconds(state) / 60,
                                now().saturating_sub(state.at) / 60
                            ));
                        }
                        return Err(format!(
                            "CUTTING ELSEWHERE: v{} by {} at {} head {}; red: {}; help: land a fix with verify land; the holder carries it with ship release --fix",
                            line.version,
                            state.holder.host,
                            state.step,
                            state.line.head(),
                            red_items(state)
                        ));
                    }
                }
                let selected = old.as_ref().map_or_else(
                    || line.clone(),
                    |(_, state)| {
                        // A carried fix extends this exact line. A different base selected on a
                        // moving main is discarded when adopting the fleet's existing line.
                        if let Some(record) = &current {
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
                    holder: holder.clone(),
                    step: "commit".into(),
                    boundary: "claimed".into(),
                    at: now(),
                    p95_s: 0,
                    rows: Vec::new(),
                };
                let previous = old.as_ref().map_or("", |(sha, _)| sha.as_str());
                let local = env
                    .state
                    .join(aterm_agent::release_progress::DIRECTORY)
                    .join(format!(
                        "{}-{}-{}.json",
                        line.version, holder.pid, holder.start
                    ));
                let mut claim = Self {
                    repo: repo.into(),
                    name: name.clone(),
                    sha: previous.into(),
                    state,
                    local,
                    previous_holder,
                };
                // Make the identity observable before publishing it. A failure here can never
                // strand a remote lease with no host record from which to prove its death.
                claim.keep_local()?;
                match replace_at_main(&git, &name, previous, &claim.state, Some(&main)) {
                    Write::Race if expected_ref.is_some() => return Err(claim_ref_conflict()),
                    Write::Race => continue,
                    Write::Offline(e) => return Err(format!("could not claim fleet line: {e}")),
                    Write::InventoryChanged { main, why } => {
                        expected_ref = Some(previous.to_owned());
                        return Ok(ClaimAttempt::InventoryChanged {
                            main,
                            why: format!("could not claim fleet line: {why}"),
                        });
                    }
                    Write::Written(sha) => {
                        claim.sha = sha;
                        return Ok(ClaimAttempt::Done(claim));
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
        rows: &'a [String],
    }
    let record = LocalRecord {
        line: &state.line,
        holder: &state.holder,
        step: &state.step,
        boundary: &state.boundary,
        at: state.at,
        p95_s: state.p95_s,
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
}

impl Local {
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
        })
    }
}

/// A monitor retries writes with bounded backoff, while the cutter keeps working. It only
/// advances progress on completed rows, never on a heartbeat or arbitrary log activity.
pub struct Claim {
    pub line: crate::line::Line,
    pub previous_holder: Option<String>,
    tx: std::sync::mpsc::Sender<Request>,
    worker: Option<std::thread::JoinHandle<()>>,
    local: Arc<Local>,
    sampler: Option<std::thread::JoinHandle<()>>,
    sample_stop: std::sync::mpsc::Sender<()>,
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
        let local = Arc::new(Local {
            state: Mutex::new(lease.state.clone()),
            path: lease.local.clone(),
        });
        let stopped = Arc::new(AtomicBool::new(false));
        let (tx, rx) = std::sync::mpsc::channel();
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
        })?;
        self.tx.send(Request::Wake).map_err(|e| e.to_string())?;
        self.line = line.clone();
        Ok(())
    }

    /// A public write needs a successful ownership round trip. Offline retries wait here,
    /// preserving the qualified candidate, rather than turning a transport error into a red.
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
        let result = write(&mut lease);
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
        let _ = self.tx.send(Request::Stop);
        if let Some(sampler) = self.sampler.take() {
            let _ = sampler.join();
        }
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

fn p95(history: &str, step: &str) -> u64 {
    let mut samples: Vec<u64> = history
        .lines()
        .filter_map(|line| {
            let mut fields = line.split('\t');
            let _ = (fields.next()?, fields.next()?);
            let field_step = fields.next()?;
            let val = fields.next()?;
            (fields.next().is_none() && field_step == step)
                .then(|| val.parse().ok())
                .flatten()
        })
        .collect();
    samples.sort_unstable();
    samples
        .get(
            samples
                .len()
                .saturating_mul(95)
                .div_ceil(100)
                .saturating_sub(1),
        )
        .copied()
        .unwrap_or(0)
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
