// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE DISK LOOK'S WORKTREE STEP (DC-02a, `docs/DESIGN-disk-custody-2026-11-12.md` §3.2): a
//! linked worktree an agent made that `origin/main` contains, that is clean and that no session
//! uses goes, and with it its merged local branch, with no command from anyone. Its shared build
//! cache goes at the next cache pass, as the cache of a workspace whose folder is gone.
//!
//! Only an agent's worktree is a candidate, read off its path: a folder named `*.noindex` with a
//! `.ubd` folder above it (the shape AGENTS.md gives an agent's worktree), or a folder of its
//! checkout's `.claude/worktrees/` (Claude Code's own isolation worktrees). A person's worktree
//! anywhere else (`~/myapp-develop`), and every worktree of a bare repository, stays whatever it
//! holds (`atpkg::build_cache::retired::judge_worktree` refuses it).
//!
//! The rule needs no receipt of a session's start, so it covers every worktree standing today.
//! No session uses a worktree when no session's working directory is inside it, no open Claude
//! Code session's transcript or any of its subagents' names its folder (one whose transcript
//! cannot be named or read through keeps every worktree), nothing in it, in git's own files for
//! it or in its build folder was written for seven days, and no process uses it (the removal's
//! fence). The judgment and the removal are the retired verify gate's, opened to any linked
//! worktree (`atpkg::build_cache::retired::judge_worktree`, `remove_worktree`): a main checkout,
//! a locked worktree, one with any edit, untracked file, submodule change or commit
//! `origin/main` lacks stays, and so do the release's cut tree and the gate's snapshot. Each
//! removal is journalled before it starts and after it ends; one whose intent cannot be
//! journalled does not happen. The step never runs `fetch`, `stash`, `reset`,
//! `submodule deinit` or `worktree prune`, and it is bounded: judging stops at half its wall,
//! the transcripts are read by three quarters of it, no removal starts after it, and the git it
//! is handed (`retired::GitUntil`) kills a run and ends a walk that outlives its own bound.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime};

use aterm_sysprobe::volumes::Table;
use atpkg::build_cache::retired::{self, Admitted, Git};

use super::idle::{self, SessionCwd};
use super::{Refusal, SweepAction, WorktreeRow};

/// How often the step runs on one host at most.
pub const STEP_EVERY: Duration = Duration::from_secs(60 * 60);

/// The whole step's wall-clock bound: no judgment and no removal starts after it.
pub const STEP_WALL: Duration = Duration::from_secs(120);

/// How long past [`STEP_WALL`] the git runs of the judgment or removal under way at it may go
/// before they are killed (and the worktree stays, or its removal resumes from its receipt).
pub const GIT_GRACE: Duration = Duration::from_secs(60);

/// How much of a transcript one read holds.
const SCAN_CHUNK: usize = 8 << 20;

/// The most subagent transcripts one session's folder may hold before the step reads none of
/// them and keeps every worktree. One workflow may run a thousand agents, and a session runs
/// several workflows.
const MAX_SUBAGENTS: usize = 8192;

/// How many levels of folders under a session's `subagents/` the step reads: a subagent's
/// transcript lies at the first, a workflow's agents' at the third (`workflows/<run>/`).
const SUBAGENT_DEPTH: usize = 3;

/// The ends of the names of the worktrees the step never judges: the retired gate's snapshot,
/// which `aterm pkg machine apply` judges, and the release's cut tree, which each release reuses
/// (`aterm_release::gates::cut_tree_path`).
const NOT_JUDGED: &[&str] = &["-verify.noindex", "-cut.noindex"];

/// The most entries of a checkout's git directories one branch check lists.
const MAX_ENTRIES: usize = 4096;

/// What a folder is to git.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Kind {
    /// A main checkout: its `.git` is a folder.
    Main,
    /// A linked worktree of the main checkout `checkout`: its `.git` is a file naming
    /// `<checkout>/.git/worktrees/<id>`.
    Linked {
        /// Its main checkout, resolved.
        checkout: PathBuf,
    },
    /// Anything else: a submodule's checkout, another repository's gitfile, a link, no `.git`.
    Other,
}

/// What `dir` is to git, read off its `.git` alone.
#[must_use]
pub fn kind(dir: &Path) -> Kind {
    let gitfile = dir.join(".git");
    let Ok(meta) = std::fs::symlink_metadata(&gitfile) else {
        return Kind::Other;
    };
    if meta.is_dir() {
        return Kind::Main;
    }
    if !meta.is_file() {
        return Kind::Other;
    }
    linked_checkout(dir, &gitfile).map_or(Kind::Other, |checkout| Kind::Linked { checkout })
}

/// The main checkout whose worktree entry `gitfile`, the `.git` file of `dir`, names.
fn linked_checkout(dir: &Path, gitfile: &Path) -> Option<PathBuf> {
    let entry = entry_of(dir, gitfile)?;
    let worktrees = entry.parent()?;
    if worktrees.file_name()? != "worktrees" {
        return None;
    }
    let common = std::fs::canonicalize(worktrees.parent()?).ok()?;
    if common.file_name()? != ".git" {
        return None;
    }
    let checkout = common.parent()?;
    let own = checkout.join(".git");
    (std::fs::symlink_metadata(&own).is_ok_and(|m| m.is_dir())
        && std::fs::canonicalize(&own).ok().as_ref() == Some(&common))
    .then(|| checkout.to_path_buf())
}

/// The git entry the `.git` file `gitfile` of `dir` names (its `gitdir:` line).
fn entry_of(dir: &Path, gitfile: &Path) -> Option<PathBuf> {
    let text = super::discover::small_text(gitfile)?;
    let named = Path::new(text.lines().next()?.strip_prefix("gitdir:")?.trim());
    Some(if named.is_absolute() {
        named.to_path_buf()
    } else {
        dir.join(named)
    })
}

/// When git registered `wt`: its entry's `gitdir` file's time, the epoch when it cannot be read
/// (a worktree whose folder is gone sorts first, and its removal is finished first).
fn registered_at(wt: &Path) -> SystemTime {
    entry_of(wt, &wt.join(".git"))
        .and_then(|entry| std::fs::metadata(entry.join("gitdir")).ok())
        .and_then(|meta| meta.modified().ok())
        .unwrap_or(SystemTime::UNIX_EPOCH)
}

/// One open Claude Code session as the step reads it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OpenSession {
    /// Its process (0: the census itself could not be read).
    pub pid: u32,
    /// Its working directory, when the kernel says it.
    pub cwd: Option<PathBuf>,
    /// The transcript it writes, or why it cannot be named.
    pub transcript: Result<PathBuf, String>,
}

/// Every Claude Code session open now under `home` (`disk::discover::running_claude_census`),
/// each with the transcript it writes: one `pid 0` row whose transcript is an `Err` when the
/// census could not be read, so the step keeps everything.
#[must_use]
pub fn open_sessions(home: &Path) -> Vec<OpenSession> {
    let running = match super::discover::running_claude_census(Some(home)) {
        Ok(running) => running,
        Err(why) => {
            return vec![OpenSession {
                pid: 0,
                cwd: None,
                transcript: Err(why),
            }];
        }
    };
    running
        .claudes
        .into_iter()
        .map(|claude| {
            let transcript = match &claude.started {
                None => Err("its start time could not be read".to_owned()),
                Some(start) => transcript_of(home, claude.pid, start),
            };
            OpenSession {
                pid: claude.pid,
                cwd: claude.cwd,
                transcript,
            }
        })
        .collect()
}

/// The transcript the Claude Code process `pid`, started at `start`, writes, read through its
/// own environment as `upgrade_drive::snapshot_transcript_end` reads it.
fn transcript_of(home: &Path, pid: u32, start: &str) -> Result<PathBuf, String> {
    let args = atpkg::caller_shell::process_args(pid);
    let (config_dir, user_home, started_in) = match &args {
        Some(args) => (
            args.env_var(crate::harness::accounts::HOME_ENV),
            args.env_var(crate::harness::accounts::HOMEDIR_ENV),
            args.env_var("PWD"),
        ),
        None => (None, home.to_str(), None),
    };
    crate::harness::upgrade_drive::process_transcript(config_dir, user_home, started_in, pid, start)
        .map(|(path, _)| path)
        .ok_or_else(|| "no single transcript of it was found".to_owned())
}

/// What the step reads beyond the worktrees themselves.
#[derive(Debug, Clone, Copy)]
pub struct Facts<'a> {
    /// The home whose children are the checkouts.
    pub home: &'a Path,
    /// Where transcripts live: nothing at, under or over it goes.
    pub transcripts_root: Option<&'a Path>,
    /// Every session's working directory, each as clause 4a reads it.
    pub cwds: &'a [SessionCwd],
    /// Why some session's working directory is not known: while it stands nothing goes.
    pub cwd_unknown: Option<&'a str>,
    /// The open Claude Code sessions.
    pub open: &'a [OpenSession],
    /// The mount table.
    pub table: Option<&'a Table>,
    /// Every checkout and worktree discovery found: one inside a candidate keeps it.
    pub known: &'a [PathBuf],
}

/// One worktree the step removed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Removed {
    /// The worktree.
    pub path: PathBuf,
    /// Its main checkout.
    pub checkout: PathBuf,
    /// Its branch (the full `refs/heads/…`), when it was on one.
    pub branch: Option<String>,
    /// Whether its branch went, or why it stays.
    pub branch_deleted: Result<(), String>,
    /// Its bytes, walked before its removal.
    pub bytes: u64,
}

/// What one step did.
#[derive(Debug, Default)]
pub struct Done {
    /// What went.
    pub removed: Vec<Removed>,
    /// What stays, each with why.
    pub kept: Vec<(PathBuf, String)>,
    /// What was refused before anything of it was touched (an intent that could not be
    /// journalled).
    pub refused: Vec<Refusal>,
    /// Why the step stopped before it was through, the rest left for its next run.
    pub unfinished: Option<String>,
}

impl Done {
    /// The bytes the removed worktrees held.
    #[must_use]
    pub fn freed(&self) -> u64 {
        self.removed
            .iter()
            .map(|r| r.bytes)
            .fold(0, u64::saturating_add)
    }
}

/// When the step runs next on this host, and the candidate it reached last.
#[derive(Debug, Default)]
pub struct Schedule {
    next: Option<Instant>,
    after: Option<PathBuf>,
}

impl Schedule {
    /// A step not yet run: due at once.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            next: None,
            after: None,
        }
    }

    /// Whether the step is due at `at`.
    #[must_use]
    pub fn due(&self, at: Instant) -> bool {
        self.next.is_none_or(|next| at >= next)
    }

    /// The candidate the last run reached before it stopped short: the next run starts after it.
    #[must_use]
    pub fn after(&self) -> Option<&Path> {
        self.after.as_deref()
    }

    /// The step ran at `at`, reaching `reached` when it stopped short.
    pub fn ran(&mut self, at: Instant, reached: Option<PathBuf>) {
        self.next = Some(at.checked_add(STEP_EVERY).unwrap_or(at));
        self.after = reached;
    }
}

/// The receipt-free rule's own clauses for the worktree `wt` (the rest is the judge's): (a) no
/// session works in it; (b) no other checkout or worktree discovery found lies inside it; (c)
/// it is not at, under or over the transcripts root.
fn unused_here(facts: &Facts<'_>, wt: &Path) -> Result<(), String> {
    idle::sessions_clear(facts.cwds, facts.cwd_unknown, wt, wt, facts.table)?;
    if let Some(inside) = facts
        .known
        .iter()
        .find(|k| k.as_path() != wt && k.starts_with(wt))
    {
        return Err(format!(
            "{} lies inside it, and would go with it",
            inside.display()
        ));
    }
    if let Some(root) = facts.transcripts_root
        && (root.starts_with(wt) || wt.starts_with(root))
    {
        return Err(Refusal::Transcript(wt.to_path_buf()).describe());
    }
    Ok(())
}

/// The receipt-free rule's build-folder clause (d): the shared build cache's mirror of `wt`
/// (`<cache root><wt>`, under the current root and the one builds moved from) holds nothing
/// written in the last seven days.
fn build_folder_quiet(
    facts: &Facts<'_>,
    wt: &Path,
    now: SystemTime,
    until: Option<Instant>,
) -> Result<(), String> {
    let since = now
        .checked_sub(retired::UNWRITTEN)
        .ok_or("the clock reads too early to date the last seven days")?;
    let mut spellings = vec![wt.to_path_buf()];
    if let Ok(real) = std::fs::canonicalize(wt)
        && real != wt
    {
        spellings.push(real);
    }
    for root in [
        atpkg::build_cache::cache_root(facts.home),
        atpkg::build_cache::moved_from(facts.home),
    ] {
        for spelled in &spellings {
            let dir = atpkg::build_cache::build_dir(&root, spelled);
            let Some(mirror) = dir.parent() else {
                continue;
            };
            if std::fs::symlink_metadata(mirror).is_err() {
                continue;
            }
            if let Some(hit) = retired::written_since(mirror, since, until)? {
                return Err(format!(
                    "its build folder {} was written within 7d",
                    hit.display()
                ));
            }
        }
    }
    Ok(())
}

/// The step's judgment of every candidate it reached.
#[derive(Debug, Default)]
pub struct Plan {
    /// Each candidate reached, `(checkout, worktree, verdict)`, in the order judged.
    pub judged: Vec<(PathBuf, PathBuf, Result<Admitted, String>)>,
    /// Why judging stopped before every candidate was reached.
    pub unfinished: Option<String>,
    /// The candidate reached last, when judging stopped short.
    pub reached: Option<PathBuf>,
}

/// Judge each `(checkout, worktree)` of `found` at `now`, oldest registration first, starting
/// after `after` and wrapping, until half the time left to `wall` is spent: cheapest first (its
/// kind, the session and nesting clauses, the judge, the build folder), then every
/// still-admitted candidate against every open session's transcripts, read by three quarters of
/// it, so a long backlog still leaves the reads and some removals their time. While any open
/// session's transcript cannot be named, every candidate stays.
#[must_use]
pub fn plan(
    git: &dyn Git,
    facts: &Facts<'_>,
    found: &[(PathBuf, PathBuf)],
    after: Option<&Path>,
    now: SystemTime,
    wall: Instant,
) -> Plan {
    if let Some((pid, why)) = facts.open.iter().find_map(|s| match &s.transcript {
        Err(why) => Some((s.pid, why)),
        Ok(_) => None,
    }) {
        let why = format!(
            "open session pid {pid} has no readable transcript ({why}), so whether it names a \
             worktree is unknown"
        );
        return Plan {
            judged: found
                .iter()
                .map(|(c, w)| (c.clone(), w.clone(), Err(why.clone())))
                .collect(),
            unfinished: None,
            reached: None,
        };
    }
    let begun = Instant::now();
    let budget = wall.saturating_duration_since(begun);
    let judge_until = begun + budget / 2;
    let read_until = begun + budget * 3 / 4;
    let mut order: Vec<(SystemTime, PathBuf, PathBuf)> = found
        .iter()
        .map(|(c, w)| (registered_at(w), w.clone(), c.clone()))
        .collect();
    order.sort();
    if let Some(at) = after.and_then(|after| order.iter().position(|(_, w, _)| w == after)) {
        order.rotate_left(at + 1);
    }
    let mut out = Plan::default();
    for (_, wt, checkout) in order {
        if Instant::now() >= judge_until {
            out.unfinished =
                Some("the step's time ran out; the rest are judged at its next run".to_owned());
            break;
        }
        let verdict = judge(git, facts, &checkout, &wt, now);
        out.reached = Some(wt.clone());
        out.judged.push((checkout, wt, verdict));
    }
    if out.unfinished.is_none() {
        out.reached = None;
    }
    if let Some(why) = transcripts_clear(facts, &mut out.judged, read_until) {
        out.unfinished.get_or_insert(why);
    }
    out
}

/// One candidate's verdict before the transcripts are read.
fn judge(
    git: &dyn Git,
    facts: &Facts<'_>,
    checkout: &Path,
    wt: &Path,
    now: SystemTime,
) -> Result<Admitted, String> {
    if let Some(end) = wt
        .file_name()
        .and_then(|n| n.to_str())
        .and_then(|n| NOT_JUDGED.iter().find(|end| n.ends_with(**end)))
    {
        return Err(format!(
            "a `{end}` tree: the retired gate's snapshot or the release's cut tree, which the \
             step never judges"
        ));
    }
    let stands = std::fs::symlink_metadata(wt).is_ok();
    if stands {
        // A removal that stopped part way may have taken its `.git` and not written it back:
        // its receipt, not its `.git`, says what it is, and the judge holds it to that.
        if !retired::in_retirement(checkout, wt) {
            match kind(wt) {
                Kind::Main => return Err("a main checkout".to_owned()),
                Kind::Linked { checkout: of }
                    if std::fs::canonicalize(checkout).ok().as_ref() == Some(&of) => {}
                Kind::Linked { .. } | Kind::Other => {
                    return Err(format!("not a linked worktree of {}", checkout.display()));
                }
            }
        }
        unused_here(facts, wt)?;
    }
    let admitted = retired::judge_worktree(git, facts.home, checkout, wt, now)?;
    if stands {
        build_folder_quiet(facts, wt, now, git.until())?;
    }
    Ok(admitted)
}

/// What a transcript names `wt` by: its folder's name, which every spelling of its path holds
/// (absolute, resolved, `~/…`, `$HOME/…`, `../…`, under `/private` or not), as found and
/// resolved, each also as JSON escapes it. A name some unrelated text also holds keeps the
/// worktree, which is the safe side.
fn spellings(wt: &Path) -> Vec<String> {
    let mut out: Vec<String> = [Some(wt.to_path_buf()), std::fs::canonicalize(wt).ok()]
        .into_iter()
        .flatten()
        .filter_map(|path| path.file_name()?.to_str().map(str::to_owned))
        .collect();
    let escaped: Vec<String> = out
        .iter()
        .map(|s| s.replace('\\', "\\\\").replace('"', "\\\""))
        .collect();
    out.extend(escaped);
    out.retain(|s| !s.is_empty());
    out.sort();
    out.dedup();
    out
}

/// The transcripts of the session that writes `transcript` (`<session>.jsonl`): it, and every
/// `*.jsonl` under its subagents' folder to [`SUBAGENT_DEPTH`] levels down (a subagent's own,
/// `<session>/subagents/*.jsonl`, and a workflow's agents',
/// `<session>/subagents/workflows/<run>/*.jsonl`). Their working directory is the session's
/// own, so only what they wrote names a worktree they use. A folder that cannot be listed, a
/// folder deeper than [`SUBAGENT_DEPTH`], and more than [`MAX_SUBAGENTS`] transcripts are an
/// `Err`: what they hold cannot be read through.
fn session_transcripts(transcript: &Path) -> Result<Vec<PathBuf>, String> {
    let mut files = vec![transcript.to_path_buf()];
    let subagents = transcript.with_extension("").join("subagents");
    let mut folders = vec![(subagents, 1_usize)];
    while let Some((folder, depth)) = folders.pop() {
        let unlisted = |e: std::io::Error| format!("{} could not be listed: {e}", folder.display());
        let listing = match std::fs::read_dir(&folder) {
            Ok(listing) => listing,
            Err(e) if depth == 1 && e.kind() == std::io::ErrorKind::NotFound => continue,
            Err(e) => return Err(unlisted(e)),
        };
        for entry in listing {
            let entry = entry.map_err(unlisted)?;
            let path = entry.path();
            let kind = entry.file_type().map_err(unlisted)?;
            if kind.is_dir() {
                if depth >= SUBAGENT_DEPTH {
                    return Err(format!(
                        "{} is deeper than {SUBAGENT_DEPTH} levels of subagent folders",
                        path.display()
                    ));
                }
                folders.push((path, depth + 1));
            } else if path.extension().is_some_and(|x| x == "jsonl") {
                if files.len() > MAX_SUBAGENTS {
                    return Err(format!(
                        "{} holds more than {MAX_SUBAGENTS} transcripts",
                        folder.display()
                    ));
                }
                files.push(path);
            }
        }
    }
    Ok(files)
}

/// Hold every still-admitted candidate of `judged` against every open session's transcripts
/// (its own and its subagents'): one a transcript names, or every one when a transcript cannot
/// be read through, stays. `Some` when `wall` passed during the reads.
fn transcripts_clear(
    facts: &Facts<'_>,
    judged: &mut [(PathBuf, PathBuf, Result<Admitted, String>)],
    wall: Instant,
) -> Option<String> {
    let admitted: Vec<usize> = judged
        .iter()
        .enumerate()
        .filter(|(_, (_, _, verdict))| verdict.is_ok())
        .map(|(i, _)| i)
        .collect();
    if admitted.is_empty() {
        return None;
    }
    let needles: Vec<(usize, String)> = admitted
        .iter()
        .flat_map(|&i| spellings(&judged[i].1).into_iter().map(move |s| (i, s)))
        .collect();
    let mut named: Vec<Option<String>> = vec![None; judged.len()];
    for session in facts.open {
        let Ok(transcript) = &session.transcript else {
            continue;
        };
        let hits = match scan_session(transcript, &needles, wall) {
            Ok(hits) => hits,
            Err(why) => {
                let late = Instant::now() >= wall;
                for &i in &admitted {
                    judged[i].2 = Err(format!(
                        "the open session pid {}'s transcripts at {} could not be read through \
                         ({why}), so whether they name it is unknown",
                        session.pid,
                        transcript.display()
                    ));
                }
                return late.then(|| {
                    "the step's time ran out reading transcripts; the rest are judged at its \
                     next run"
                        .to_owned()
                });
            }
        };
        for (i, path) in hits {
            named[i].get_or_insert_with(|| {
                format!(
                    "the open session pid {}'s transcript {} names it",
                    session.pid,
                    path.display()
                )
            });
        }
    }
    for (i, why) in named.into_iter().enumerate() {
        if let Some(why) = why {
            judged[i].2 = Err(why);
        }
    }
    None
}

/// Which of `needles` the session that writes `transcript` names ([`session_transcripts`]),
/// each with the transcript that names it.
fn scan_session(
    transcript: &Path,
    needles: &[(usize, String)],
    wall: Instant,
) -> Result<Vec<(usize, PathBuf)>, String> {
    let mut hits = Vec::new();
    for path in session_transcripts(transcript)? {
        for i in scan(&path, needles, wall)? {
            hits.push((i, path.clone()));
        }
    }
    Ok(hits)
}

/// Which of `needles` the file at `path` holds, read in [`SCAN_CHUNK`] pieces that overlap by
/// the longest needle less one byte, so no needle is missed across a piece's edge.
fn scan(path: &Path, needles: &[(usize, String)], wall: Instant) -> Result<Vec<usize>, String> {
    use std::io::Read as _;
    let longest = needles.iter().map(|(_, n)| n.len()).max().unwrap_or(0);
    if longest == 0 {
        return Ok(Vec::new());
    }
    if !std::fs::symlink_metadata(path).is_ok_and(|m| m.is_file()) {
        return Err("it is not a regular file".to_owned());
    }
    let mut file = std::fs::File::open(path).map_err(|e| e.to_string())?;
    let mut found = vec![false; needles.len()];
    let mut held: Vec<u8> = Vec::new();
    let mut chunk = vec![0u8; SCAN_CHUNK];
    loop {
        if Instant::now() >= wall {
            return Err("the step's time ran out".to_owned());
        }
        let n = match file.read(&mut chunk) {
            Ok(n) => n,
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(e.to_string()),
        };
        if n == 0 {
            break;
        }
        held.extend_from_slice(&chunk[..n]);
        {
            let text = String::from_utf8_lossy(&held);
            for (seen, (_, needle)) in found.iter_mut().zip(needles) {
                if !*seen && text.contains(needle.as_str()) {
                    *seen = true;
                }
            }
        }
        let keep = longest - 1;
        if held.len() > keep {
            held.drain(..held.len() - keep);
        }
    }
    let mut hits: Vec<usize> = needles
        .iter()
        .zip(&found)
        .filter(|(_, seen)| **seen)
        .map(|((i, _), _)| *i)
        .collect();
    hits.sort_unstable();
    hits.dedup();
    Ok(hits)
}

/// What the step calls to act: whether it still holds the pass lock, the remover of one
/// worktree of a checkout, and the ledger's append of one row (`false`: not on record).
pub struct Hooks<'a> {
    /// Whether the pass lock is still this host's.
    pub still_held: &'a dyn Fn() -> bool,
    /// Remove the worktree (second) of the checkout (first), judged again just before.
    pub remove: &'a mut dyn FnMut(&Path, &Path) -> Result<(), String>,
    /// Append one row to the ledger; `false` when it is not on record.
    pub journal: &'a mut dyn FnMut(&str) -> bool,
}

/// One step: [`plan`] the candidates, then remove each admitted one until `wall`, journalling
/// its intent before (a removal whose intent is not on record does not happen) and its outcome
/// after, and deleting its merged branch once it went ([`delete_merged_branch`]). Returns what
/// it did and the candidate it reached last when it stopped short.
#[must_use]
pub fn step(
    git: &dyn Git,
    facts: &Facts<'_>,
    found: &[(PathBuf, PathBuf)],
    after: Option<&Path>,
    now: SystemTime,
    wall: Instant,
    hooks: Hooks<'_>,
) -> (Done, Option<PathBuf>) {
    let at = now
        .duration_since(SystemTime::UNIX_EPOCH)
        .ok()
        .and_then(|d| i64::try_from(d.as_secs()).ok())
        .unwrap_or(0);
    let planned = plan(git, facts, found, after, now, wall);
    let mut done = Done {
        unfinished: planned.unfinished,
        ..Done::default()
    };
    // The candidate the removals reached last, and whether they stopped before the end.
    let mut handled: Option<PathBuf> = None;
    let mut stopped = false;
    for (checkout, wt, verdict) in planned.judged {
        let admitted = match verdict {
            Ok(admitted) => admitted,
            Err(why) => {
                handled = Some(wt.clone());
                done.kept.push((wt, why));
                continue;
            }
        };
        if Instant::now() >= wall {
            done.unfinished.get_or_insert_with(|| {
                "the step's time ran out; the rest go at its next run".to_owned()
            });
            stopped = true;
            break;
        }
        if !(hooks.still_held)() {
            done.unfinished = Some("the pass lock was lost".to_owned());
            stopped = true;
            break;
        }
        handled = Some(wt.clone());
        let bytes = super::dir_bytes(&wt).0;
        let intent = super::worktree_row(
            at,
            SweepAction::Removing,
            &row(&wt, &checkout, &admitted, &admitted.why, None, bytes),
        );
        if !(hooks.journal)(&intent) {
            let refusal = Refusal::Unjournalled(wt.clone());
            (hooks.journal)(&super::denial_row(at, "", &refusal));
            done.refused.push(refusal);
            continue;
        }
        match (hooks.remove)(&checkout, &wt) {
            Ok(()) => {
                let branch_deleted = match &admitted.branch {
                    Some(branch) => delete_merged_branch(git, &checkout, branch, &admitted.head),
                    None => Err("it was on no branch the removal knew of (detached, or its \
                                 removal resumed from a receipt, which records none)"
                        .to_owned()),
                };
                (hooks.journal)(&super::worktree_row(
                    at,
                    SweepAction::Removed,
                    &row(
                        &wt,
                        &checkout,
                        &admitted,
                        &admitted.why,
                        Some(&branch_deleted),
                        bytes,
                    ),
                ));
                done.removed.push(Removed {
                    path: wt.clone(),
                    checkout: checkout.clone(),
                    branch: admitted.branch.clone(),
                    branch_deleted,
                    bytes,
                });
            }
            Err(why) => {
                (hooks.journal)(&super::worktree_row(
                    at,
                    SweepAction::Kept,
                    &row(&wt, &checkout, &admitted, &why, None, 0),
                ));
                done.kept.push((wt.clone(), why));
            }
        }
    }
    // Removals that stopped short name where the next run starts: after the last candidate they
    // handled (where this run started when they handled none), so an admitted one left for lack
    // of time comes first next time; else judging that stopped short names the last it reached.
    let reached = if stopped {
        handled.or_else(|| after.map(Path::to_path_buf))
    } else if done.unfinished.is_some() {
        planned.reached
    } else {
        None
    };
    (done, reached)
}

/// One ledger row's facts of the worktree `wt` of `checkout` the judge admitted.
fn row<'a>(
    wt: &'a Path,
    checkout: &'a Path,
    admitted: &'a Admitted,
    text: &'a str,
    branch_deleted: Option<&'a Result<(), String>>,
    bytes: u64,
) -> WorktreeRow<'a> {
    WorktreeRow {
        path: wt,
        checkout,
        branch: admitted.branch.as_deref(),
        head: &admitted.head,
        bytes,
        text,
        branch_deleted,
    }
}

/// Delete the local branch `branch` (the full `refs/heads/…`) of `checkout`, at `head`, once its
/// worktree went. It stays when it is `main` or origin's default branch, when any worktree has
/// it checked out, or is rebasing or bisecting it, when it no longer names `head`, or when
/// `origin/main` does not contain `head`; the deletion itself is compare-and-delete on `head`
/// (`git update-ref --no-deref -d`), so a branch moved meanwhile stays.
///
/// # Errors
/// Why the branch stays.
pub fn delete_merged_branch(
    git: &dyn Git,
    checkout: &Path,
    branch: &str,
    head: &str,
) -> Result<(), String> {
    let Some(name) = branch.strip_prefix("refs/heads/") else {
        return Err(format!("{branch} is not a local branch"));
    };
    if name == "main" {
        return Err("it is main".to_owned());
    }
    let default = git.run(
        checkout,
        &["symbolic-ref", "-q", "refs/remotes/origin/HEAD"],
    );
    match default.code {
        Some(0) => {
            if default.out.trim().strip_prefix("refs/remotes/origin/") == Some(name) {
                return Err("it is origin's default branch".to_owned());
            }
        }
        Some(_) => {}
        None => {
            return Err(format!(
                "git could not say which branch is origin's default: {}",
                default.err
            ));
        }
    }
    let listing = git.run(checkout, &["worktree", "list", "--porcelain"]);
    if listing.code != Some(0) {
        return Err(format!("git worktree list failed: {}", listing.err));
    }
    if listing
        .out
        .lines()
        .any(|line| line.strip_prefix("branch ") == Some(branch))
    {
        return Err("a worktree has it checked out".to_owned());
    }
    let common = checkout.join(".git");
    let mut admins = vec![common.clone()];
    match std::fs::read_dir(common.join("worktrees")) {
        Ok(entries) => {
            for entry in entries.take(MAX_ENTRIES) {
                let entry = entry
                    .map_err(|e| format!("git's worktree entries could not be listed: {e}"))?;
                admins.push(entry.path());
            }
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(format!("git's worktree entries could not be listed: {e}")),
    }
    for admin in &admins {
        for held in [
            admin.join("rebase-merge").join("head-name"),
            admin.join("rebase-apply").join("head-name"),
            admin.join("BISECT_START"),
        ] {
            match std::fs::symlink_metadata(&held) {
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
                Err(e) => return Err(format!("{} could not be read: {e}", held.display())),
                Ok(_) => {}
            }
            let Some(text) = super::discover::small_text(&held) else {
                return Err(format!("{} could not be read", held.display()));
            };
            let named = text.trim();
            if named == branch || named == name {
                return Err(format!(
                    "git is rebasing or bisecting it ({})",
                    held.display()
                ));
            }
        }
    }
    let now_at = git.run(checkout, &["rev-parse", "--verify", "--quiet", branch]);
    if now_at.code != Some(0) || now_at.out.trim() != head {
        return Err(format!(
            "it no longer names {}",
            head.get(..10).unwrap_or(head)
        ));
    }
    let merged = git.run(
        checkout,
        &[
            "merge-base",
            "--is-ancestor",
            head,
            "refs/remotes/origin/main",
        ],
    );
    if merged.code != Some(0) {
        return Err(format!(
            "origin/main does not contain {}",
            head.get(..10).unwrap_or(head)
        ));
    }
    let deleted = git.run(checkout, &["update-ref", "--no-deref", "-d", branch, head]);
    if deleted.code != Some(0) {
        return Err(format!("git did not delete it: {}", deleted.err));
    }
    Ok(())
}

#[cfg(all(test, unix, debug_assertions))]
#[path = "disk_worktree_tests.rs"]
mod tests;
