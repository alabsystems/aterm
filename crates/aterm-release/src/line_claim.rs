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
    fn compare_and_swap(&self, name: &str, before: &str, after: &str) -> Write;
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
        let mut cmd = crate::ledger::object_git(&self.repo);
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

    fn compare_and_swap(&self, name: &str, before: &str, after: &str) -> Write {
        if let Err(error) = self.running() {
            return Write::Offline(error);
        }
        match self
            .remote
            .write_ref(name, (!before.is_empty()).then_some(before), after)
        {
            Ok(crate::github_remote::RefWrite::Written) => Write::Written(after.into()),
            Ok(crate::github_remote::RefWrite::Conflict) => Write::Race,
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

/// A host observation is tri-state. Failure to reach a peer is never evidence of death.
#[derive(Clone, Debug)]
enum Observation {
    Dead,
    Live(Box<State>),
    Unknown,
}

fn progressing(state: &State, now: u64) -> bool {
    now.saturating_sub(state.at) <= 1800.max(state.p95_s.saturating_mul(2))
}

/// Positive host evidence is required for adoption. A locally advancing boundary takes
/// precedence over an old ref when GitHub is unavailable.
fn adoptable(state: &State, observed: &Observation, now: u64) -> bool {
    match observed {
        Observation::Dead => true,
        Observation::Live(live) if live.holder == state.holder => {
            !progressing(state, now) && !progressing(live, now)
        }
        Observation::Live(_) | Observation::Unknown => false,
    }
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
    let Some(sha) = git.ref_oid(name)? else {
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
}

fn replace(git: &dyn ClaimStore, name: &str, previous: &str, state: &State) -> Write {
    let sha = match commit(git, previous, state) {
        Ok(sha) => sha,
        Err(e) => return Write::Offline(e),
    };
    git.compare_and_swap(name, previous, &sha)
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
    let name = reference(version)?;
    let stopped = AtomicBool::new(false);
    let old = remote(&Network::new(repo, &stopped)?, &name)?.map(|(_, state)| state.line);
    current(&GitCli::new(repo), version, old)
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
pub fn follow(
    backend: &mut dyn crate::release::Backend,
    candidate: &mut crate::release::Candidate,
    journal: &mut Option<crate::release::Journal>,
) -> Result<(), crate::release::Refusal> {
    if let Some(selected) = backend.fleet_existing(&candidate.version)? {
        if let Some(record) = backend.fleet_supersession(&candidate.version)?
            && journal
                .as_ref()
                .is_some_and(|j| j.line().base != record.replacement.base)
        {
            return Err(crate::release::Refusal::new(
                format!(
                    "this journal's line was superseded by {}: {}",
                    record.replacement.base, record.reason
                ),
                "drop the retired journal with ship release --forget, then resume the current line",
            ));
        }
        let selected = journal
            .as_ref()
            .filter(|journal| journal.commit == candidate.commit)
            .map_or_else(
                || selected.clone(),
                |journal| preferred(&journal.line(), &selected),
            );
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
}

impl Lease {
    fn take(repo: &Path, line: &crate::line::Line, continued: bool) -> Result<Self, String> {
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
        loop {
            crate::github_remote::Remote::origin(repo)?.refresh_main()?;
            let current = crate::line::supersession(&GitCli::new(repo), &line.version)?;
            if current
                .as_ref()
                .is_some_and(|r| r.replacement.base != line.base)
            {
                return Err(
                    "this line was superseded on main; discard the retired journal and resume"
                        .into(),
                );
            }
            let old = remote(&git, &name)?;
            if let Some((_, state)) = &old {
                let observed = observe(state, &holder, &env.state);
                let continued = continues_holder(continued, &state.holder, &holder);
                if !continued && !adoptable(state, &observed, now()) {
                    if matches!(observed, Observation::Unknown)
                        || matches!(&observed, Observation::Live(live) if live.holder != state.holder)
                    {
                        return Err(format!(
                            "COULD NOT RUN: cannot establish fleet line holder {}'s process and progress; the existing cut continues",
                            state.holder.host
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
            };
            // Make the identity observable before publishing it. A failure here can never
            // strand a remote lease with no host record from which to prove its death.
            claim.keep_local()?;
            match replace(&git, &name, previous, &claim.state) {
                Write::Race => continue,
                Write::Offline(e) => return Err(format!("could not claim fleet line: {e}")),
                Write::Written(sha) => {
                    claim.sha = sha;
                    return Ok(claim);
                }
            }
        }
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
            Write::Offline(_) => Ok(false),
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
        continued: bool,
    ) -> Result<Self, String> {
        let lease = Lease::take(repo, line, continued)?;
        let line = lease.state.line.clone();
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

fn fail(error: String) {
    if FAILURE.set(error).is_ok() {
        #[cfg(unix)]
        if let Ok(pid) = i32::try_from(std::process::id()) {
            crate::interrupt::send(pid, crate::interrupt::SIGTERM);
        }
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
    let mut take = |key: &str| event.remove(key).unwrap_or(aterm_json::Value::Null);
    let summary = aterm_json::Value::Object(aterm_json::Map::from([
        ("event".to_string(), kind),
        ("stage".to_string(), take("stage")),
        ("check".to_string(), take("check")),
        ("test".to_string(), take("test")),
        ("outcome".to_string(), take("outcome")),
        ("detail".to_string(), take("detail")),
        ("findings".to_string(), take("findings")),
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
            matches!(event["outcome"].as_str(), Some("FAIL" | "fail" | "failed"))
                .then(|| event.to_string())
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
mod tests {
    use super::*;

    fn state(node: &str, at: u64) -> State {
        State {
            line: crate::line::Line::start("0.156.0", "base", 1),
            holder: Holder {
                node: node.into(),
                host: node.into(),
                sid: node.into(),
                pid: 1,
                boot: "boot".into(),
                start: 1,
            },
            step: "verify".into(),
            boundary: "test-binary-finished".into(),
            at,
            p95_s: 0,
            rows: Vec::new(),
        }
    }

    /// The exact holder survives native exec, and the authenticated pre-B1 parent may cross the
    /// one child-process deployment boundary. NEGATIVE CONTROLS: without local continuation or
    /// the independently live parent proof, and with another node, session or boot, the flag
    /// grants no fleet-line authority.
    #[test]
    fn an_exec_or_authenticated_pre_b1_parent_continues_the_fleet_holder() {
        let previous = state("m3", 1).holder;
        let no_parent = |_: &Holder| false;
        assert!(continues_holder_with(
            true, &previous, &previous, &no_parent
        ));
        assert!(!continues_holder_with(false, &previous, &previous, &|_| {
            true
        }));

        let mut child = previous.clone();
        child.pid += 1;
        child.start += 1;
        let parent = |holder: &Holder| holder.pid == previous.pid && holder.start == previous.start;
        assert!(continues_holder_with(true, &previous, &child, &parent));
        assert!(!continues_holder_with(true, &previous, &child, &no_parent));
        for field in ["node", "host", "sid", "boot"] {
            let mut changed = child.clone();
            match field {
                "node" => changed.node.push('x'),
                "host" => changed.host.push('x'),
                "sid" => changed.sid.push('x'),
                _ => changed.boot.push('x'),
            }
            assert!(
                !continues_holder_with(true, &previous, &changed, &parent),
                "changed {field}"
            );
        }
    }

    #[test]
    fn stalled_holder_is_adopted_but_live_progress_and_network_faults_are_not() {
        let old = state("m3", 1);
        assert!(adoptable(
            &old,
            &Observation::Live(Box::new(old.clone())),
            1802
        ));
        // Negative control: only the boundary changes, not a heartbeat or ref age.
        let live = state("m3", 1801);
        assert!(!adoptable(
            &old,
            &Observation::Live(Box::new(live.clone())),
            1802
        ));
        assert!(
            !adoptable(&live, &Observation::Live(Box::new(old.clone())), 1802),
            "a cached fleet read cannot overrule a fresh ref boundary"
        );
        assert!(!adoptable(&old, &Observation::Unknown, 99999));
        assert!(adoptable(&old, &Observation::Dead, 2));
        let mut long_step = old.clone();
        long_step.p95_s = 2000;
        assert!(!adoptable(
            &old,
            &Observation::Live(Box::new(long_step.clone())),
            4001
        ));
        assert!(adoptable(
            &old,
            &Observation::Live(Box::new(long_step)),
            4002
        ));
    }

    #[test]
    fn activity_without_a_completed_row_cannot_extend_the_progress_bound() {
        assert!(completed_row("compiler still busy").is_none());
        assert!(completed_row(r#"{"event":"stage-started","stage":"compile"}"#).is_none());
        assert!(
            completed_row(r#"{"event":"test-binary-finished","test":"db","outcome":"FAIL"}"#)
                .is_some()
        );
        assert_eq!(
            p95(
                "1\t0.155.0\tverify\t2000\n2\t0.155.0\tbuild\t9000",
                "verify"
            ),
            2000
        );
    }

    #[test]
    fn only_the_private_events_stream_advances_progress_and_partial_rows_wait() {
        let dir = aterm_tempfile::tempdir().unwrap();
        let event = r#"{"event":"check-finished","check":"lint","outcome":"PASS"}"#;
        let output = dir.path().join("06-verify.log");
        std::fs::write(output, format!("{event}\n")).unwrap();
        let events = dir.path().join("06-verify.events");
        std::fs::write(&events, event).unwrap();
        let mut rows = Rows::default();
        // Negative controls: output that resembles a row, and an incomplete private row.
        assert!(rows.read(dir.path()).is_empty());
        std::fs::write(&events, format!("{event}\n")).unwrap();
        assert_eq!(rows.read(dir.path()).len(), 1);
        assert!(
            rows.read(dir.path()).is_empty(),
            "a poll is not another boundary"
        );
    }

    #[test]
    fn progress_reader_discards_large_lines_without_rereading_partial_bytes() {
        use std::io::Write as _;
        let dir = aterm_tempfile::tempdir().unwrap();
        let path = dir.path().join("06-verify.events");
        let event = r#"{"event":"check-finished","check":"lint","outcome":"PASS"}"#;
        let mut reader = Rows::default();
        // Complete arbitrary output is bounded too, even though it is not JSON.
        std::fs::write(
            &path,
            format!("{}\n{event}\n", "x".repeat(PROGRESS_LINE_BYTES * 3)),
        )
        .unwrap();
        assert_eq!(reader.read(dir.path()).len(), 1);
        assert!(reader.read(dir.path()).is_empty());

        std::fs::write(&path, "x".repeat(PROGRESS_LINE_BYTES * 2)).unwrap();
        assert!(reader.read(dir.path()).is_empty());
        let cursor = reader.0.get(&path).unwrap();
        let consumed = cursor.offset;
        assert_eq!(consumed, (PROGRESS_LINE_BYTES * 2) as u64);
        assert!(cursor.partial.is_empty());
        assert!(cursor.discarding);
        assert!(reader.read(dir.path()).is_empty());
        assert_eq!(reader.0[&path].offset, consumed);
        // A valid-looking suffix of the oversized record must not become a row.
        let mut append = std::fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .unwrap();
        write!(append, "{event}\n{event}\n").unwrap();
        assert_eq!(reader.read(dir.path()).len(), 1);
        assert!(!reader.0[&path].discarding);
        assert!(reader.0[&path].partial.is_empty());
        assert!(reader.read(dir.path()).is_empty());
    }

    #[test]
    fn progress_reader_keeps_bounded_fragments_and_seeds_old_completed_rows() {
        use std::io::Write as _;
        let dir = aterm_tempfile::tempdir().unwrap();
        let path = dir.path().join("transcript.log");
        let event = "verify-event: {\"event\":\"check-finished\",\"check\":\"lint\",\"outcome\":\"PASS\"}\n";
        std::fs::write(&path, event).unwrap();
        let mut reader = Rows::default();
        reader.seed(dir.path());
        assert!(reader.read(dir.path()).is_empty());
        let split = event.len() / 2;
        let mut append = std::fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .unwrap();
        append.write_all(&event.as_bytes()[..split]).unwrap();
        assert!(reader.read(dir.path()).is_empty());
        assert_eq!(reader.0[&path].partial, event.as_bytes()[..split]);
        assert_eq!(reader.0[&path].offset, (event.len() + split) as u64);
        append.write_all(&event.as_bytes()[split..]).unwrap();
        assert_eq!(reader.read(dir.path()).len(), 1);
        assert!(reader.read(dir.path()).is_empty());
    }

    #[test]
    fn progress_seed_skips_dense_history_and_keeps_its_incomplete_tail() {
        use std::io::Write as _;
        let dir = aterm_tempfile::tempdir().unwrap();
        let path = dir.path().join("06-verify.events");
        let event = "{\"event\":\"check-finished\",\"check\":\"lint\",\"outcome\":\"PASS\"}\n";
        let next = "{\"event\":\"check-finished\",\"check\":\"doc\",\"outcome\":\"PASS\"}\n";
        let split = event.len() / 2;
        let mut file = std::fs::File::create(&path).unwrap();
        for _ in 0..10_000 {
            file.write_all(event.as_bytes()).unwrap();
        }
        file.write_all(&event.as_bytes()[..split]).unwrap();
        let mut reader = Rows::default();
        reader.seed(dir.path());
        assert_eq!(reader.0[&path].offset, file.metadata().unwrap().len());
        assert_eq!(reader.0[&path].partial, event.as_bytes()[..split]);
        assert!(reader.read(dir.path()).is_empty());
        file.write_all(&event.as_bytes()[split..]).unwrap();
        file.write_all(next.as_bytes()).unwrap();
        assert_eq!(
            reader.read(dir.path()),
            [completed_row(event).unwrap(), completed_row(next).unwrap()]
        );
        assert!(reader.read(dir.path()).is_empty());
    }

    #[test]
    fn progress_seed_keeps_oversize_discard_through_the_next_newline() {
        use std::io::Write as _;
        let dir = aterm_tempfile::tempdir().unwrap();
        let path = dir.path().join("transcript.log");
        std::fs::write(&path, "x".repeat(PROGRESS_LINE_BYTES * 2)).unwrap();
        let mut reader = Rows::default();
        reader.seed(dir.path());
        assert_eq!(reader.0[&path].offset, (PROGRESS_LINE_BYTES * 2) as u64);
        assert!(reader.0[&path].partial.is_empty());
        assert!(reader.0[&path].discarding);
        assert!(reader.read(dir.path()).is_empty());
        let event = "{\"event\":\"check-finished\",\"check\":\"lint\",\"outcome\":\"PASS\"}\n";
        let mut file = std::fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .unwrap();
        write!(file, "{event}{event}").unwrap();
        assert_eq!(reader.read(dir.path()), [completed_row(event).unwrap()]);
        assert!(!reader.0[&path].discarding);
        assert!(reader.read(dir.path()).is_empty());
    }

    #[test]
    fn progress_reader_never_joins_fragments_across_replacement_or_observed_truncation() {
        let dir = aterm_tempfile::tempdir().unwrap();
        let path = dir.path().join("06-verify.events");
        let event = r#"{"event":"check-finished","check":"lint","outcome":"PASS"}"#;
        let split = event.len() - 3;
        let mut reader = Rows::default();
        std::fs::write(&path, &event[..split]).unwrap();
        assert!(reader.read(dir.path()).is_empty());
        let old_identity = reader.0[&path].identity;
        let replacement = dir.path().join("replacement");
        std::fs::write(&replacement, format!("{}\n{event}\n", &event[split..])).unwrap();
        std::fs::rename(&replacement, &path).unwrap();
        assert_eq!(reader.read(dir.path()).len(), 1);
        assert_ne!(reader.0[&path].identity, old_identity);
        assert!(reader.read(dir.path()).is_empty());

        // Observe a shortened same-inode file while an old prefix is retained.
        std::fs::write(&path, "x".repeat(PROGRESS_LINE_BYTES / 2)).unwrap();
        assert!(reader.read(dir.path()).is_empty());
        let same_identity = reader.0[&path].identity;
        std::fs::write(&path, format!("{}\n{event}\n", &event[split..])).unwrap();
        assert_eq!(reader.read(dir.path()).len(), 1);
        assert_eq!(reader.0[&path].identity, same_identity);
        assert!(reader.read(dir.path()).is_empty());
    }

    #[test]
    #[cfg(any(target_vendor = "apple", target_os = "linux", target_os = "android"))]
    fn blocked_publication_cannot_hide_completed_progress_from_the_fleet() {
        const COMPLETIONS: usize = 16_384;
        let dir = aterm_tempfile::tempdir().unwrap();
        let logs = dir.path().join("logs");
        std::fs::create_dir(&logs).unwrap();
        let event = aterm_json::json!({
            "event":"test-binary-finished", "test":"db", "outcome":"FAIL",
            "detail":"x".repeat(256),
        })
        .to_string();
        let recording = format!("{event}\n").repeat(COMPLETIONS);
        let log = logs.join("06-verify.events");
        std::fs::write(&log, &recording).unwrap();
        let mut old = state("m3", 1);
        old.holder.pid = std::process::id();
        (old.holder.boot, old.holder.start) =
            aterm_agent::release_progress::identity(old.holder.pid).unwrap();
        let local = Arc::new(Local {
            state: Mutex::new(old.clone()),
            path: dir
                .path()
                .join(aterm_agent::release_progress::DIRECTORY)
                .join("line.json"),
        });
        let lease = Lease {
            repo: dir.path().into(),
            name: "unused".into(),
            sha: "old".into(),
            state: old.clone(),
            local: local.path.clone(),
        };
        local.update(|state| state.step = "build".into()).unwrap();
        let (tx, rx) = std::sync::mpsc::channel();
        let (entered_tx, entered_rx) = std::sync::mpsc::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        let publishing = Arc::clone(&local);
        let writer = std::thread::spawn(move || {
            publish_progress_with(lease, &publishing, &rx, &mut |_| {
                entered_tx.send(()).unwrap();
                release_rx
                    .recv_timeout(std::time::Duration::from_secs(60))
                    .unwrap();
                Ok(true)
            })
        });
        tx.send(Request::Wake).unwrap();
        let entered = entered_rx.recv_timeout(std::time::Duration::from_secs(60));
        // This is the production sampler path, while the publisher is held by a channel.
        let sampled = local.sample(&mut Rows::default(), &logs);
        let observed = local_observation(&old, dir.path());
        let current = local.snapshot();
        let local_bytes = std::fs::read(&local.path);
        let recorded_bytes = std::fs::read(&log);
        // Retire the held publisher before any assertion, including I/O failures.
        let stopped = tx.send(Request::Stop);
        let released = release_tx.send(());
        let joined = writer.join();
        entered.unwrap();
        stopped.unwrap();
        released.unwrap();
        joined.unwrap();
        sampled.unwrap();
        let current = current.unwrap();
        let local_bytes = local_bytes.unwrap();
        let full_history = aterm_json::to_vec(&current).unwrap();
        assert!(full_history.len() > 4 * 1024 * 1024);
        assert!(local_bytes.len() < 4 * 1024 * 1024);
        assert_eq!(current.rows.len(), COMPLETIONS);
        let mut expected_local = current.clone();
        expected_local.rows.clear();
        let local_record: State = aterm_json::from_slice(&local_bytes).unwrap();
        assert_eq!(local_record, expected_local);
        assert_eq!(recorded_bytes.unwrap(), recording.as_bytes());
        // Remote serialization still carries every outcome, including red details.
        let remote_text = aterm_toml::to_string(&current).unwrap();
        let remote_record =
            parse_state(&remote_text, &reference(&current.line.version).unwrap()).unwrap();
        assert_eq!(remote_record, current);
        assert!(!red_items(&remote_record).is_empty());
        assert!(matches!(&observed, Observation::Live(live) if **live == expected_local));
        assert!(!adoptable(&old, &observed, now()));
        // Negative control: without the sampler's fresh host observation this same old
        // ref and live process would be judged stalled and adopted.
        assert!(adoptable(
            &old,
            &Observation::Live(Box::new(old.clone())),
            now()
        ));
        // Negative control: the previous full-history registry shape is omitted by
        // the real survey reader before its own rows projection can run.
        std::fs::write(&local.path, full_history).unwrap();
        assert!(matches!(
            local_observation(&old, dir.path()),
            Observation::Unknown
        ));
    }

    #[cfg(unix)]
    fn repository(root: &Path, name: &str, origin: &Path) -> Network<'static> {
        static RUNNING: AtomicBool = AtomicBool::new(false);
        let path = root.join(name);
        std::fs::create_dir(&path).unwrap();
        let git = GitCli::new(&path);
        for args in [
            vec!["init"],
            vec!["config", "user.name", "Andrew Yates"],
            vec!["config", "user.email", "test@example.invalid"],
            vec![
                "remote",
                "add",
                "origin",
                "https://github.com/fixture/app.git",
            ],
        ] {
            git_ok(&git, &args).unwrap();
        }
        Network {
            remote: crate::github_remote::claim_fixture(&path, origin),
            repo: path,
            stopped: &RUNNING,
        }
    }

    #[cfg(unix)]
    #[test]
    fn native_fleet_status_reads_the_actual_claim_without_changing_its_holder() {
        let dir = aterm_tempfile::tempdir().unwrap();
        let origin = dir.path().join("origin.git");
        std::fs::create_dir(&origin).unwrap();
        git_ok(&GitCli::new(&origin), &["init", "--bare"]).unwrap();
        let writer = repository(dir.path(), "writer", &origin);
        let name = reference("0.156.0").unwrap();
        let initial = state("original-holder", 123);
        let Write::Written(sha) = replace(&writer, &name, "", &initial) else {
            panic!("fixture claim must be written");
        };
        let reader = repository(dir.path(), "reader", &origin);
        let root = dir.path().join("reader");
        let remote = crate::github_remote::tests::local(&root, &origin);
        assert_eq!(
            status_rows(&remote).unwrap(),
            [
                "v0.156.0 original-holder original-holder step=verify boundary=test-binary-finished at=123 head=base"
            ]
        );
        assert_eq!(
            crate::ledger::rev_parse(&GitCli::new(&origin), &name).unwrap(),
            sha
        );
        assert!(
            git_ok(&reader, &["for-each-ref", "--format=%(refname)"])
                .unwrap()
                .stdout_utf8()
                .lines()
                .all(|name| name.starts_with("refs/agh/"))
        );
        assert!(!root.join(".git/FETCH_HEAD").exists());
        let mut wrong = initial;
        wrong.line.version = "0.155.0".into();
        assert!(matches!(
            replace(&writer, &name, &sha, &wrong),
            Write::Written(_)
        ));
        assert!(
            status_rows(&remote)
                .unwrap_err()
                .contains("version does not match")
        );
    }

    #[cfg(unix)]
    #[test]
    fn two_macs_have_one_holder_and_stalled_cut_is_not_lost() {
        let dir = aterm_tempfile::tempdir().unwrap();
        let origin = dir.path().join("origin.git");
        std::fs::create_dir(&origin).unwrap();
        git_ok(&GitCli::new(&origin), &["init", "--bare"]).unwrap();
        let a = repository(dir.path(), "a", &origin);
        let b = repository(dir.path(), "b", &origin);
        // Before B6 each clone could acquire its own run claim for the same version.
        // These are the existing production local locks, not a model of them.
        let local_a = crate::release::RunClaim::take(
            &dir.path().join("a/running"),
            (101, "start-a"),
            dir.path(),
            &mut |_| None,
        )
        .unwrap();
        let local_b = crate::release::RunClaim::take(
            &dir.path().join("b/running"),
            (102, "start-b"),
            dir.path(),
            &mut |_| None,
        )
        .unwrap();
        let baseline_holders = [&local_a, &local_b].len();
        assert_eq!(baseline_holders, 2);
        let name = reference("0.156.0").unwrap();
        let initial = state("m3", 1);
        // Both Macs read the absent ref. Exactly one CAS can install a holder.
        let first = match replace(&a, &name, "", &initial) {
            Write::Written(sha) => sha,
            other => panic!("{other:?}"),
        };
        let mut holder = Lease {
            repo: dir.path().join("a"),
            name: name.clone(),
            sha: first.clone(),
            state: initial.clone(),
            local: dir.path().join("local.json"),
        };
        git_ok(
            &a,
            &[
                "remote",
                "set-url",
                "origin",
                dir.path().join("unavailable").to_str().unwrap(),
            ],
        )
        .unwrap();
        let stopped = AtomicBool::new(false);
        assert_eq!(
            holder.write(&stopped),
            Ok(false),
            "a transport fault is pending, never ownership loss"
        );
        assert_eq!(holder.sha, first);
        git_ok(
            &a,
            &[
                "remote",
                "set-url",
                "origin",
                "https://github.com/fixture/app.git",
            ],
        )
        .unwrap();
        assert_eq!(
            holder.write_with(&a),
            Ok(true),
            "the same candidate continues once transport returns"
        );
        let first = holder.sha.clone();
        assert_eq!(replace(&b, &name, "", &state("m22", 1)), Write::Race);
        let (old_sha, old) = remote(&b, &name).unwrap().unwrap();
        assert_eq!(old_sha, first);
        assert!(!adoptable(
            &old,
            &Observation::Live(Box::new(state("m3", 1801))),
            1802
        ));
        assert!(adoptable(
            &old,
            &Observation::Live(Box::new(old.clone())),
            1802
        ));
        let mut adopted = old.clone();
        adopted.holder = state("m22", 1802).holder;
        adopted.at = 1802;
        let new = match replace(&b, &name, &old_sha, &adopted) {
            Write::Written(sha) => sha,
            other => panic!("{other:?}"),
        };
        assert_eq!(replace(&a, &name, &first, &initial), Write::Race);
        assert!(
            holder.write_with(&a).is_err(),
            "positive ownership loss stops the old holder"
        );
        assert_eq!(
            remote(&a, &name).unwrap().unwrap().1.line,
            initial.line,
            "the exact candidate survives takeover"
        );
        // Negative control: removing the expected-old lease lets the stale holder overwrite
        // the adopter. The same stale write with the lease above was refused.
        let stale = commit(&a, &new, &initial).unwrap();
        git_ok(
            &a,
            &[
                "-c",
                "protocol.file.allow=always",
                "push",
                "--force",
                origin.to_str().unwrap(),
                &format!("{stale}:{name}"),
            ],
        )
        .unwrap();
        assert_eq!(remote(&b, &name).unwrap().unwrap().1.holder, initial.holder);
        println!(
            "B6 measured: before holders/version={baseline_holders}; after holders/version=1; unfenced stale overwrite=1; leased stale overwrite=0; stalled adoptions=1; cuts lost=0"
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_stopped_or_retargeted_monitor_cannot_publish_a_claim() {
        let dir = aterm_tempfile::tempdir().unwrap();
        let origin = dir.path().join("origin.git");
        std::fs::create_dir(&origin).unwrap();
        git_ok(&GitCli::new(&origin), &["init", "--bare"]).unwrap();
        let initial = repository(dir.path(), "writer", &origin);
        let stopped = AtomicBool::new(true);
        let writer = Network {
            repo: initial.repo,
            remote: initial.remote,
            stopped: &stopped,
        };
        let name = reference("0.156.0").unwrap();
        assert!(matches!(
            replace(&writer, &name, "", &state("m3", 1)),
            Write::Offline(_)
        ));
        stopped.store(false, Ordering::SeqCst);
        git_ok(
            &writer,
            &[
                "remote",
                "set-url",
                "origin",
                "https://github.com/foreign/app.git",
            ],
        )
        .unwrap();
        assert!(matches!(
            replace(&writer, &name, "", &state("m3", 1)),
            Write::Offline(_)
        ));
        assert!(
            git_ok(&GitCli::new(&origin), &["for-each-ref"])
                .unwrap()
                .stdout
                .is_empty()
        );
    }
}
