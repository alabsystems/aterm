// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `ship release --pump`: the pump's world over this clone and the cutter.
//!
//! The decisions are [`aterm_release::pump`]'s. This file is what they read (origin/main and
//! its tags through agh, this clone's release journal and repair record, a repair worker's
//! report) and how a verb runs: this same executable, started again in the clone with the
//! verb's arguments, which selects and dispatches to the frozen cutter of the commit it judges
//! exactly as a run a person starts does. Nothing here judges, builds or publishes.

use std::io::{BufRead as _, Write as _};
use std::path::{Path, PathBuf};

use aterm_release::pump::{self, Every, Handed, Main, Memory, Opened, Progress, Ran, Verb};
use aterm_release::repair;

use crate::interrupt;
use crate::ledger::{GitCli, git_ok};
use crate::release::{
    JOURNAL_FILE, Journal, RELEASE_COMMAND, RUNNING_FILE, ReleaseOptions, SELECTION_FILE,
};

struct Host {
    /// The executable each verb is run with: this one.
    exe: PathBuf,
    repo: PathBuf,
    release: PathBuf,
    /// The Linux declaration the pump was started with, as `ship release` flags.
    declared: Vec<String>,
    /// The flags the pump was started with that its verbs take, each handed only to a verb
    /// that takes it ([`Verb::argv`]).
    handed: Handed,
    /// `--adopt`: the pump may take over a fleet line another holder started.
    adopt: bool,
    /// main's `publish/config.sh`, as last read.
    config: String,
    /// The supersede candidate last found, for the head and main it was found on.
    candidate: Option<((String, String), Option<String>)>,
    tip: String,
    /// This process's start, as `ps -o lstart=` read it when the pump started ([`pump::Doing`]).
    started: String,
    /// The harness state folder the pump publishes its survey record in ([`Host::record`]),
    /// where the harness's environment names one.
    state: Option<PathBuf>,
    /// The fact last published: a verb's run refreshes its `at` while it is attended.
    fact: Option<pump::Fact>,
    /// Why the last survey record could not be written, said once until one is written again.
    unpublished: Option<String>,
    /// The fleet line's holder as main's last read found it, whose Mac a standby asks after the
    /// pump it runs ([`pump::World::holder_pump`]).
    claimant: Option<crate::line_claim::Claimant>,
    /// The fleet line of main's version as main's last read found it (its claim with main's
    /// supersession record applied): the head a look's journal is claimed at ([`recorded_head`]).
    fleet: Option<crate::line::Line>,
    /// main's version and the base of the replacement main's supersession record of it names
    /// (`None` where main records none), as main's last read found them (the same scan of main's
    /// history): whether a journal of that version is retired ([`retired`]), and the head its
    /// undecided supersession is claimed at. `None` before a look has read a claim (and after a
    /// published version's look, which reads none): [`Host::seen`] then reads this clone's git.
    replaced: Option<(String, Option<String>)>,
    /// Why the last renewal of the line's claim failed, said once until one succeeds.
    unrenewed: Option<String>,
}

impl Host {
    fn git(&self) -> GitCli {
        GitCli::new(&self.repo)
    }

    /// main's own `publish/config.sh` at `tip`: what main asks of the pump (whether it publishes,
    /// its pause between versions, a run's handed declarations). A config that does not read is
    /// an error, which fails the look ([`pump::observe`]), never a config that asks nothing.
    fn config_at(&self, tip: &str) -> Result<String, String> {
        self.text(&["show", &format!("{tip}:publish/config.sh")])
            .ok_or_else(|| format!("{tip}:publish/config.sh does not read"))
    }

    fn text(&self, args: &[&str]) -> Option<String> {
        git_ok(&self.git(), args)
            .ok()
            .map(|out| out.stdout_utf8().trim().to_owned())
    }

    fn version_at(&self, rev: &str) -> Option<String> {
        version_at(&self.git(), rev)
    }

    /// The commit of main that opened `version`: the newest whose subject is the bump's and
    /// whose tree declares it where its parent's does not.
    fn opened(&self, version: &str, tip: &str) -> Option<String> {
        let subject = format!("release: {version} ");
        let named = self.text(&[
            "log",
            "-n",
            "16",
            "--format=%H",
            "--fixed-strings",
            &format!("--grep={subject}"),
            tip,
        ])?;
        named
            .lines()
            .find(|commit| {
                self.version_at(commit).as_deref() == Some(version)
                    && self.version_at(&format!("{commit}^")).as_deref() != Some(version)
            })
            .map(str::to_string)
    }

    /// When `rev` was committed: its committer time, epoch seconds.
    fn committed_at(&self, rev: &str) -> Option<u64> {
        self.text(&["show", "-s", "--format=%ct", rev])?
            .parse()
            .ok()
    }

    /// The Linux declaration of a line the pump starts: its own flags, else main's
    /// `PUMP_LINUX_DEFAULT`, which must parse as a release's Linux flags and nothing else.
    fn linux(&self) -> Result<Vec<String>, String> {
        if !self.declared.is_empty() {
            return Ok(self.declared.clone());
        }
        let Some(value) = pump::config_value(&self.config, pump::LINUX_KEY) else {
            return Err(format!(
                "the pump starts lines and a line needs its Linux declaration: pass --mac-only \
                 or --linux-artifacts DIRECTORY to --pump, or set {} in publish/config.sh",
                pump::LINUX_KEY
            ));
        };
        let words: Vec<String> = value.split_whitespace().map(str::to_string).collect();
        let mut args = vec!["release".to_string()];
        args.extend(words.iter().cloned());
        match crate::cli::parse(&args) {
            Ok(crate::cli::Cmd::Release(opts))
                if opts.linux.declared()
                    && opts
                        == (ReleaseOptions {
                            linux: opts.linux.clone(),
                            ..ReleaseOptions::default()
                        }) =>
            {
                Ok(words)
            }
            _ => Err(format!(
                "publish/config.sh's {}=\"{value}\" is not a Linux declaration (--mac-only, or \
                 --linux-artifacts DIRECTORY with its targets and workers)",
                pump::LINUX_KEY
            )),
        }
    }

    /// The child that runs `verb` with the Linux declaration `linux`: this executable, in the
    /// clone. It runs with [`repair::PUMP_ENV`] removed, whatever the pump inherited, so that a
    /// red of the cut is caller-owned repair: the cutter prints its briefing to the pump's log,
    /// opens no session and keeps no worker record, and the pump holds the red line
    /// ([`pump::Red`]).
    /// A re-base's child alone is marked [`pump::REBASE_ENV`]: its `--supersede` takes the claim
    /// only from a holder a contender could take it from.
    fn command(&self, verb: &Verb, linux: &[String]) -> aterm_uds::spawnfd::Command {
        let mut command = aterm_uds::spawnfd::command(&self.exe);
        command
            .args(verb.argv(linux, &self.handed))
            .current_dir(&self.repo)
            .env_remove(repair::PUMP_ENV)
            .stdin(aterm_uds::spawnfd::Stdio::null())
            .stdout(aterm_uds::spawnfd::Stdio::piped());
        if matches!(verb, Verb::Rebase { .. }) {
            command.env(pump::REBASE_ENV, "1");
        } else {
            command.env_remove(pump::REBASE_ENV);
        }
        command
    }

    /// This clone's pump's survey record, in the harness state folder's
    /// [`aterm_ctl::release_pump::DIRECTORY`], named by this clone ([`record_name`]): every other
    /// pump of the host keeps its own.
    fn record(&self) -> Option<PathBuf> {
        self.state.as_ref().map(|state| {
            state
                .join(aterm_ctl::release_pump::DIRECTORY)
                .join(record_name(&self.release))
        })
    }

    /// The Linux declaration `verb`'s run is handed ([`pump::declaration`]): this clone's
    /// unfinished journal says whether it holds one of its own.
    fn handed_linux(&self, verb: &Verb) -> Result<Vec<String>, String> {
        let declares = self
            .journal_file()
            .ok()
            .flatten()
            .is_some_and(|journal| !journal.finished() && journal.linux.declared());
        pump::declaration(verb, declares, self.linux())
    }

    /// This clone's journal: candidate.toml's pending one, else release.toml's. A proposal
    /// candidate.toml kept of a version that is cut or whose fleet line starts elsewhere is no
    /// journal of a line ([`proposal_dropped`]); the next run drops it.
    fn journal_file(&self) -> Result<Option<Journal>, String> {
        let judged = Journal::load(&self.release.join(JOURNAL_FILE))?;
        let Some(pending) = Journal::load(&self.release.join(SELECTION_FILE))? else {
            return Ok(judged);
        };
        let dropped = proposal_dropped(
            &pending,
            judged.as_ref(),
            &mut || self.cut(&pending.version),
            &mut || {
                Ok(
                    match crate::line_claim::existing(&self.repo, &pending.version)? {
                        Some(fleet) => {
                            crate::line::supersession(&self.git(), &pending.version)?.is_none()
                                && pending.never_claimed(judged.as_ref(), &fleet)
                        }
                        None => false,
                    },
                )
            },
        )?;
        Ok(if dropped { judged } else { Some(pending) })
    }

    /// Whether `version`'s tag is on origin.
    fn cut(&self, version: &str) -> Result<bool, String> {
        let origin = crate::publish::native_origin(&self.repo).map_err(|e| e.to_string())?;
        Ok(origin.get(&format!("refs/tags/v{version}"))?.is_some())
    }

    /// `journal` as the pump reads it ([`journal_seen`]), `cut` and `claimed` as the caller found
    /// them: the head the fleet's claim records for its line where that is not R
    /// ([`pump::Journal::recorded`], [`recorded_head`]), from `fleet` (the claim as this look's
    /// read of main found it; `None` for a read with no look, which this clone's git answers
    /// alone), and whether main retired it ([`retired`]). Both judge main's supersession record
    /// of the journal's version as main's last read of its claim found it, and before any look has
    /// read that version's claim (the pump's first fact, a cycle whose read of main failed) as this
    /// clone's git reads origin/main, which needs no network.
    fn seen(
        &self,
        journal: Journal,
        (cut, claimed): (bool, bool),
        fleet: Option<&crate::line::Line>,
    ) -> pump::Journal {
        let git = self.git();
        let replaced = self
            .replaced
            .as_ref()
            .filter(|(version, _)| *version == journal.version)
            .map_or_else(
                || {
                    crate::line::supersession(&git, &journal.version)
                        .ok()
                        .flatten()
                        .map(|record| record.replacement.base)
                },
                |(_, base)| base.clone(),
            );
        let recorded = recorded_head(
            &journal,
            fleet,
            &|line| {
                crate::line::recorded_prefix(&git, line)
                    .ok()
                    .map(|prefix| prefix.head().to_owned())
            },
            replaced.as_deref(),
        );
        let retired = retired(&journal, replaced.as_deref());
        journal_seen(journal, (cut, claimed), recorded, retired)
    }

    /// Whether this clone holds the cut claim of `version`: its cut journal
    /// (`dist/cut-state.toml`) names it, which is what a resumed run reads to finish a version
    /// whose tag is on origin.
    fn claimed(&self, version: &str) -> Result<bool, String> {
        crate::publish::JournalHeader::read_classified(&self.repo.join("dist/cut-state.toml"))
            .map(|header| header.is_some_and(|header| header.version == version))
            .map_err(|unread| match unread {
                crate::publish::JournalUnread::Read(e)
                | crate::publish::JournalUnread::Invalid(e) => {
                    format!("dist/cut-state.toml does not read: {e}")
                }
            })
    }

    /// The repair record's worker `pick` chooses, with what it reports now.
    fn worker(&self, pick: impl Fn(&repair::Worker) -> bool) -> Option<Opened> {
        let record = repair::Record::load(&self.release.join(repair::RECORD_FILE)).ok()?;
        let worker = record.workers.iter().rev().find(|worker| pick(worker))?;
        Some(Opened {
            sid: worker.sid.clone(),
            commit: worker.commit.clone(),
            at: worker.at,
            report: repair::report(&worker.sid),
        })
    }
}

/// This clone's journal as the pump reads it, `cut` saying its version's tag is on origin and
/// `claimed` that this clone holds that cut's claim. A tag on origin is this journal's cut only
/// with the claim: without it another clone took and cut the line, the bare run here is
/// refused for good ("this clone journals no cut of it"), and the journal is lost. A
/// supersession it holds undecided is read from it, with the head it retires; whether main
/// records one of its version is main's own read ([`pump::Main::superseded`]). `recorded` is the
/// head the fleet's claim records for its line where that is not R ([`recorded_head`]), and
/// `retired` says main's supersession of its version retired it ([`retired`]).
fn journal_seen(
    journal: Journal,
    (cut, claimed): (bool, bool),
    recorded: Option<String>,
    retired: bool,
) -> pump::Journal {
    let finished = journal.finished();
    let cut = cut && !finished;
    pump::Journal {
        recorded: recorded.filter(|head| *head != journal.commit),
        prepared: journal.prepared.is_some() && !cut && !finished,
        prepared_at: journal.prepared.as_ref().map_or(0, |prepared| prepared.at),
        undecided: journal.supersede.as_ref().map(|txn| pump::Undecided {
            from: txn.previous.head().to_owned(),
            onto: txn.replacement.base.clone(),
        }),
        version: journal.version,
        head: journal.commit,
        finished,
        published: cut && claimed,
        lost: cut && !claimed,
        rebased: false,
        retired: retired && !finished,
    }
}

/// Whether main's supersession of `journal`'s version retired it ([`pump::Journal::retired`]):
/// main records one (`replaced`: the base of the replacement its record names) onto another base
/// than the journal's line, by the same judgement as the release's own refusal of every run of
/// such a journal (`line_claim::follow`: "this journal's line was superseded").
fn retired(journal: &Journal, replaced: Option<&str>) -> bool {
    !journal.finished() && replaced.is_some_and(|base| journal.line().base != base)
}

/// Whether candidate.toml's `pending` journal is no journal of a line, as the release's own
/// selection drops it before it selects (`drop_never_claimed`): a proposal on which this clone
/// did nothing ([`Journal::proposal_only`]) of a version that is cut (`cut`: its tag is on
/// origin, another clone's release) or whose fleet line starts elsewhere (`elsewhere`,
/// [`Journal::never_claimed`]). A yielded pump's proposal of the version the other clone then cut
/// is no lost journal: the pump goes on to open, start or adopt the next version. A journal with
/// anything done on it stays, a lost one included, which only a session's `--forget` ends, since
/// that withdraws what the journal asked, drafted or reserved.
fn proposal_dropped(
    pending: &Journal,
    judged: Option<&Journal>,
    cut: &mut dyn FnMut() -> Result<bool, String>,
    elsewhere: &mut dyn FnMut() -> Result<bool, String>,
) -> Result<bool, String> {
    Ok(pending.proposal_only(judged) && (cut()? || elsewhere()?))
}

/// The head the fleet's claim records for `journal`'s line where it is not R
/// ([`pump::Journal::recorded`]). A supersession left undecided is claimed at the head it retires
/// until main records it (`replaced`: the base of the replacement main's record names, where it
/// names one), and at its replacement, R, once main does. Otherwise a journal whose line extends
/// the fleet's (`fleet`: the claim as the same look read it; its base, and its hops a prefix of
/// the journal's) is claimed at the fleet's head: the claim publishes a hop at the step boundary
/// after the hop's record merge, so a run that ends between the two leaves the claim a hop
/// behind a journal that no longer says its line is pending. A hop carried here whose record
/// merge has not landed (a pending line), read with no claim, is claimed at the newest head of
/// the line main retains (`retained`, [`crate::line::recorded_prefix`]), which is what the claim
/// publishes at each step boundary. Any other journal is claimed at R; a read that fails says R.
fn recorded_head(
    journal: &Journal,
    fleet: Option<&crate::line::Line>,
    retained: &dyn Fn(&crate::line::Line) -> Option<String>,
    replaced: Option<&str>,
) -> Option<String> {
    let line = journal.line();
    let extended = fleet.filter(|fleet| {
        fleet.version == line.version
            && fleet.base == line.base
            && line.hops.starts_with(&fleet.hops)
    });
    let head = match (&journal.supersede, extended) {
        (Some(txn), _) if replaced == Some(txn.replacement.base.as_str()) => None,
        (Some(txn), _) => Some(txn.previous.head().to_owned()),
        (None, Some(fleet)) => Some(fleet.head().to_owned()),
        (None, None) if journal.pending_line() => retained(&line),
        (None, None) => None,
    };
    head.filter(|head| *head != journal.commit)
}

/// The workspace version `rev`'s root manifest declares.
fn version_at(git: &GitCli, rev: &str) -> Option<String> {
    let manifest = git_ok(git, &["show", &format!("{rev}:Cargo.toml")]).ok()?;
    crate::publish::workspace_version(manifest.stdout_utf8().trim()).ok()
}

/// Whether `green` can take the line of `version` at `head` in its one re-base
/// ([`pump::World::onto`]), each answer read from `git` by `run` (one invocation's trimmed
/// stdout, `None` where it failed): `green` declares `version` (`version_at`), holds `fix`
/// (`holds`), and carries a commit past `head`, merges aside, that moves a path a build may read
/// and is neither `fix` nor a commit a hop of the line already carries (`hops`: main's commits
/// past `head` include each hopped fix's original, whose patch the line holds). A path no build
/// reads is repository Markdown or a change fragment: the cheap superset of the app's cone,
/// which counts a test crate's change as the app's (such a re-base costs what a hop costs) and
/// never misses an app change.
#[expect(
    clippy::too_many_arguments,
    reason = "each read is injected so the rule is tested apart from git"
)]
fn onto_with(
    version: &str,
    head: &str,
    green: &str,
    fix: Option<&str>,
    hops: &[String],
    version_at: &dyn Fn(&str) -> Option<String>,
    holds: &dyn Fn(&str, &str) -> bool,
    carried: &dyn Fn(&str, &str) -> Option<String>,
) -> Result<(), String> {
    let short = |sha: &str| sha.get(..9).unwrap_or(sha).to_owned();
    match version_at(green) {
        Some(declared) if declared == version => {}
        Some(declared) => {
            return Err(format!(
                "{} declares {declared}, not the line's {version}",
                short(green)
            ));
        }
        None => return Err(format!("{} declares no workspace version", short(green))),
    }
    if let Some(fix) = fix
        && !holds(fix, green)
    {
        return Err(format!(
            "{} does not hold the fix {}",
            short(green),
            short(fix)
        ));
    }
    let listed = carried(head, green).ok_or_else(|| {
        format!(
            "the commits from {} to {} do not read",
            short(head),
            short(green)
        )
    })?;
    if listed
        .lines()
        .any(|commit| Some(commit) != fix && !hops.iter().any(|hopped| hopped == commit))
    {
        Ok(())
    } else {
        Err(format!(
            "{} carries no commit past {} in the app's cone{}{}, so a hop carries the same",
            short(green),
            short(head),
            if fix.is_some() { " but the fix" } else { "" },
            if hops.is_empty() {
                ""
            } else {
                " and what the line's hops carry"
            }
        ))
    }
}

/// The non-merge commits past `head` in `green`'s history, newest first, that move a path a
/// build may read: anything but repository Markdown and `changes/` fragments ([`onto_with`]).
/// `None` where git does not answer.
fn cone_commits(git: &GitCli, head: &str, green: &str) -> Option<String> {
    git_ok(
        git,
        &[
            "rev-list",
            "--no-merges",
            &format!("{head}..{green}"),
            "--",
            ".",
            ":(exclude,glob)**/*.md",
            ":(exclude)changes",
        ],
    )
    .ok()
    .map(|out| out.stdout_utf8().trim().to_owned())
}

/// [`onto_with`] over the repository `repo`, with `hops` the commits the line's hops carry:
/// what [`pump::World::onto`] answers.
fn onto_in(
    repo: &Path,
    version: &str,
    head: &str,
    green: &str,
    fix: Option<&str>,
    hops: &[String],
) -> Result<(), String> {
    let git = GitCli::new(repo);
    onto_with(
        version,
        head,
        green,
        fix,
        hops,
        &|rev| version_at(&git, rev),
        &|commit, tip| git_ok(&git, &["merge-base", "--is-ancestor", commit, tip]).is_ok(),
        &|head, green| cone_commits(&git, head, green),
    )
}

/// The name of the survey record of the pump of the clone whose release folder is `release`: a
/// clone runs one pump (its `pump.lock`), so each clone's pump keeps one record that its own
/// restarts overwrite and no other pump touches.
fn record_name(release: &Path) -> String {
    let mut hash = aterm_digest::Sha256::new();
    hash.update(b"aterm-release-pump-record/1\0");
    hash.update(release.as_os_str().as_encoded_bytes());
    let hex = aterm_codec::hex::encode_lower(&hash.finalize());
    format!("{}.json", &hex[..16])
}

/// The pump's survey record ([`aterm_ctl::release_pump`]) of `fact`, at `record` (this clone's,
/// [`Host::record`]), with this process's identity, so a reader's kernel tells a live pump from a
/// dead one.
fn publish_fact(record_path: &Path, fact: &pump::Fact) -> Result<(), String> {
    let pid = std::process::id();
    let identity = aterm_agent::release_progress::identity(pid);
    let record = aterm_ctl::release_pump::Record {
        holder: fact.holder.clone(),
        line: fact.line.clone(),
        step: fact.step.clone(),
        own: fact.own,
        at: fact.at,
        decision: fact.decision.clone(),
        pid,
        boot: identity.as_ref().map(|(boot, _)| boot.clone()),
        start: identity.map(|(_, start)| start),
    };
    let folder = record_path
        .parent()
        .ok_or_else(|| format!("{} names no folder", record_path.display()))?;
    std::fs::create_dir_all(folder).map_err(|e| format!("create {}: {e}", folder.display()))?;
    crate::durable::write_atomic_durable(record_path, record.json().as_bytes(), 0o600)
        .map_err(|e| e.to_string())
}

/// The newest commit of origin/main past `head` that the follower (`verify watch`) judged a
/// pass, as the judged journal says now: a candidate for a session's `--commit REV
/// --supersede`, never a base the release selects by itself. The commits are main's past
/// `head` over every parent, newest first: the tips the follower judged are merges' second
/// parents as often as not, so main's first-parent chain holds few of them. `None` where there
/// is none, or the journal or GitHub cannot be read.
fn newest_pass(root: &Path, head: &str) -> Option<String> {
    let listed = git_ok(
        &GitCli::new(root),
        &[
            "rev-list",
            "--topo-order",
            "--ancestry-path",
            &format!("{head}..origin/main"),
        ],
    )
    .ok()?
    .stdout_utf8();
    if listed.trim().is_empty() {
        return None;
    }
    let remote = crate::github_remote::Remote::origin(root).ok()?;
    aterm_verify::judged::newest_pass(&remote.follower_records().ok()?, &listed)
}

/// The newest completed row among `lines`, the survey's `release_line` records of this Mac
/// ([`aterm_agent::release_progress::survey`]): the newest of the live holders of `version`'s
/// line. A dead or unidentified holder's record is no progress of a running verb.
fn newest_row(lines: &[String], version: &str) -> Option<Progress> {
    lines
        .iter()
        .filter_map(|line| {
            let (alive, data) = line
                .strip_prefix("release_line alive=")?
                .split_once(" data=")?;
            if alive != "1" {
                return None;
            }
            let record: aterm_json::Value =
                aterm_json::from_str(&aterm_codec::pct::decode(data)).ok()?;
            (record["line"]["version"].as_str()? == version).then_some(())?;
            Some(Progress {
                at: record["at"].as_u64()?,
                rows: record["completed"].as_u64().unwrap_or(0),
                p95_s: record["p95_s"].as_u64().unwrap_or(0),
                boundary: format!(
                    "{} — {}",
                    record["step"].as_str().unwrap_or("?"),
                    record["boundary"].as_str().unwrap_or("?")
                ),
            })
        })
        .max_by_key(|progress| progress.at)
}

/// A verb's child as the pump attends it ([`pump::attend`]).
struct Attended<'a> {
    ended: &'a mut dyn FnMut() -> Option<i32>,
    pid: i32,
    /// The version whose line the verb runs, and the harness state directory its holder
    /// records its rows under.
    version: String,
    state: Option<PathBuf>,
    /// This clone's pump's survey record ([`Host::record`]).
    record: Option<PathBuf>,
    /// How long the child's process tree has made no progress, as its reads find it.
    stall: aterm_toolchain::stall::Stall,
    /// The last look and when: the rows and the tree are read twice a minute, which is also
    /// the pace the tree's progress is judged over.
    looked: Option<(std::time::Instant, Option<Progress>, Option<u64>)>,
    /// The fact the pump published as it started the verb: each beat republishes it with its
    /// own time, so a peer reads a pump that attends a long run as live ([`pump::Fact`]).
    fact: Option<pump::Fact>,
    /// When the fact was last republished: at most twice a minute.
    beaten: Option<std::time::Instant>,
}

impl Attended<'_> {
    /// The run's newest completed row and how long its process tree has been idle, read at
    /// most twice a minute.
    fn look(&mut self) -> (Option<Progress>, Option<u64>) {
        if let Some((at, progress, idle)) = &self.looked
            && at.elapsed() < std::time::Duration::from_secs(30)
        {
            return (progress.clone(), *idle);
        }
        let unix_now = pump::Running::now(self);
        let progress = self.state.as_ref().and_then(|state| {
            newest_row(
                &aterm_agent::release_progress::survey(state, unix_now),
                &self.version,
            )
        });
        let now = std::time::Instant::now();
        let idle = u32::try_from(self.pid)
            .ok()
            .filter(|pid| *pid > 0)
            .and_then(|pid| {
                self.stall
                    .read(pid, aterm_toolchain::stall::tree_work(pid, &[]), now)
            })
            .map(|idle| idle.as_secs());
        self.looked = Some((now, progress.clone(), idle));
        (progress, idle)
    }
}

impl pump::Running for Attended<'_> {
    fn now(&mut self) -> u64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_secs())
    }

    fn ended(&mut self) -> Option<i32> {
        (self.ended)()
    }

    fn progress(&mut self) -> Option<Progress> {
        self.look().0
    }

    fn idle(&mut self) -> Option<u64> {
        self.look().1
    }

    fn stop_asked(&mut self) -> bool {
        interrupt::caught().is_some()
    }

    fn interrupt(&mut self) {
        if self.pid > 0 {
            interrupt::send(self.pid, 15);
        }
    }

    fn beat(&mut self) {
        if self
            .beaten
            .is_some_and(|at| at.elapsed() < std::time::Duration::from_secs(30))
        {
            return;
        }
        self.beaten = Some(std::time::Instant::now());
        let unix_now = pump::Running::now(self);
        if let (Some(record), Some(fact)) = (&self.record, &mut self.fact) {
            fact.at = unix_now;
            // A survey record that cannot be written is a stale fact, never a stop of the run.
            let _ = publish_fact(record, fact);
        }
    }

    fn pause(&mut self) {
        std::thread::sleep(std::time::Duration::from_millis(200));
    }

    fn say(&mut self, line: &str) {
        let mut out = std::io::stdout().lock();
        let _ = writeln!(out, "{line}");
        let _ = out.flush();
    }
}

impl pump::World for Host {
    fn now(&mut self) -> u64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_secs())
    }

    fn main(&mut self) -> Result<Main, String> {
        let origin = crate::publish::native_origin(&self.repo).map_err(|e| e.to_string())?;
        let tip = origin.refresh_main()?;
        let version = self
            .version_at(&tip)
            .ok_or_else(|| format!("{tip} declares no workspace version"))?;
        let published = origin.get(&format!("refs/tags/v{version}"))?.is_some();
        let fixtures = published
            && crate::fixtures::on_commit(&self.git(), &tip, &version)
                .map_err(|e| e.to_string())?
                .is_some();
        // The line, its holder and main's supersession record of the version come from one read
        // of the claim and one scan of main's history: the line's one re-base is read from main,
        // where every clone reads it, so a standby that adopts a re-based line re-bases it no
        // more than its first holder could have.
        // A look decides on the claim, so its age is counted from just before this read.
        let read_at = pump::World::now(self);
        let (fleet, holder, held, claimant, replaced, elsewhere) = if published {
            (None, None, None, None, None, false)
        } else {
            let (claim, held) = crate::line_claim::standing(&self.repo, &version, true)?;
            (
                claim.line,
                claim.holder.map(|(_, holder)| holder),
                held.map(|held| pump::Held {
                    until: held.until,
                    refusal: held.refusal,
                }),
                claim.claimant,
                claim.replaced,
                claim.elsewhere,
            )
        };
        let line = fleet.as_ref().map(|line| line.head().to_owned());
        let superseded = replaced.is_some();
        self.fleet = fleet;
        // A published version's look reads no claim, and so no record of its supersession.
        self.replaced = (!published).then(|| (version.clone(), replaced));
        let lease_until = claimant.as_ref().map(|claimant| claimant.until);
        let lease_ended = claimant.as_ref().is_some_and(|claimant| claimant.ended);
        let holder_id = claimant.as_ref().map(|claimant| pump::Process {
            pid: claimant.pid,
            boot: claimant.boot.clone(),
            start: claimant.start,
        });
        self.claimant = claimant;
        let opened = self.opened(&version, &tip);
        let opened_at = opened
            .as_deref()
            .and_then(|opened| self.committed_at(opened));
        let behind = opened
            .as_ref()
            .filter(|_| !published)
            .and_then(|opened| {
                self.text(&["rev-list", "--count", &format!("{opened}..{tip}")])?
                    .parse()
                    .ok()
            })
            .unwrap_or(0);
        self.config = self.config_at(&tip)?;
        self.tip.clone_from(&tip);
        Ok(Main {
            opened,
            opened_at,
            behind,
            tip,
            version,
            published,
            fixtures,
            line,
            holder,
            held,
            lease_until,
            lease_ended,
            elsewhere,
            holder_id,
            superseded,
            read_at,
        })
    }

    fn journal(&mut self) -> Result<Option<pump::Journal>, String> {
        let Some(journal) = self.journal_file()? else {
            return Ok(None);
        };
        let finished = journal.finished();
        let cut = !finished && self.cut(&journal.version)?;
        let claimed = cut && self.claimed(&journal.version)?;
        Ok(Some(self.seen(
            journal,
            (cut, claimed),
            self.fleet.as_ref(),
        )))
    }

    fn journal_here(&mut self) -> Option<pump::Journal> {
        let judged = Journal::load(&self.release.join(JOURNAL_FILE))
            .ok()
            .flatten();
        let pending = Journal::load(&self.release.join(SELECTION_FILE))
            .ok()
            .flatten();
        // A proposal candidate.toml kept of a line this clone never held is told apart only by
        // the fleet's claim, which this read does not ask: it reads as this clone's, the side a
        // standby waits on. The head the claim records is this clone's git's read, no network:
        // a run may have moved the claim since main's last read.
        pending
            .or(judged)
            .map(|journal| self.seen(journal, (false, false), None))
    }

    fn holder(&mut self) -> Option<String> {
        crate::release::live_holder(&self.release.join(RUNNING_FILE), &mut |pid| {
            crate::release_steps::started_of(pid)
        })
        .map(|holder| format!("pid {}, started {}", holder.pid, holder.started))
    }

    fn publishes(&mut self) -> bool {
        pump::config_value(&self.config, pump::PUBLISH_KEY) == Some("1")
    }

    fn every(&mut self) -> Every {
        pump::open_every(&self.config)
    }

    fn me(&mut self) -> (u32, String) {
        (std::process::id(), self.started.clone())
    }

    fn adopts(&mut self) -> bool {
        self.adopt
    }

    fn repair(&mut self, version: &str, commit: Option<&str>) -> Option<Opened> {
        // A red of --open-next is kept under the version it would have opened, at whatever tip
        // it judged: it is no red of a line, and `opening` reads it.
        self.worker(|worker| {
            worker.of_cut()
                && worker.version == version
                && commit.is_none_or(|commit| worker.commit == commit)
        })
    }

    fn opening(&mut self, from: &str) -> Option<Opened> {
        let hash = repair::opening_hash(from);
        self.worker(|worker| worker.hash == hash)
    }

    fn holds(&mut self, commit: &str, tip: &str) -> bool {
        git_ok(&self.git(), &["merge-base", "--is-ancestor", commit, tip]).is_ok()
    }

    fn admits(&mut self, fix: &str) -> Result<(), String> {
        let journal = self
            .journal_file()?
            .ok_or("this clone has no line to carry a fix onto")?;
        let git = self.git();
        crate::line::git_ready(&git).map_err(|stop| stop.to_string())?;
        crate::line::admit(&git, &journal.line(), fix)
            .map(|_| ())
            .map_err(|stop| stop.to_string())
    }

    fn onto(
        &mut self,
        version: &str,
        head: &str,
        green: &str,
        fix: Option<&str>,
    ) -> Result<(), String> {
        let journal = self
            .journal_file()?
            .ok_or("this clone has no line to re-base")?;
        let hops: Vec<String> = journal
            .line()
            .hops
            .into_iter()
            .map(|hop| hop.carries)
            .collect();
        onto_in(&self.repo, version, head, green, fix, &hops)
    }

    fn holder_pump(&mut self) -> Result<Option<Vec<pump::PumpRow>>, String> {
        let claimant = self
            .claimant
            .as_ref()
            .ok_or("the line's claim names no holder")?;
        Ok(crate::line_claim::holder_pump(claimant)?.map(|rows| {
            rows.into_iter()
                .map(|row| pump::PumpRow {
                    alive: row.alive,
                    line: row.line,
                    step: row.step,
                    own: row.own,
                    age: row.age_s,
                })
                .collect()
        }))
    }

    fn looked(&mut self) -> Option<u64> {
        let record = self.record()?;
        let text =
            crate::durable::read_regular_text(&record, aterm_ctl::release_pump::MAX_RECORD_BYTES)
                .ok()?;
        aterm_ctl::release_pump::Record::from_json(&text).map(|record| record.at)
    }

    fn publish(&mut self, fact: &pump::Fact) {
        self.fact = Some(fact.clone());
        let Some(record) = self.record() else {
            return;
        };
        match publish_fact(&record, fact) {
            Ok(()) => self.unpublished = None,
            Err(why) if self.unpublished.as_ref() != Some(&why) => {
                let line = format!(
                    "pump: cannot publish its survey record {} ({why}); a survey reads this \
                     pump's fact as stale",
                    record.display()
                );
                self.unpublished = Some(why);
                pump::World::say(self, &line);
            }
            Err(_) => {}
        }
    }

    fn green(&mut self, head: &str) -> Option<String> {
        let key = (head.to_string(), self.tip.clone());
        if let Some((found_for, found)) = &self.candidate
            && *found_for == key
        {
            return found.clone();
        }
        let found = newest_pass(&self.repo, head);
        self.candidate = Some((key, found.clone()));
        found
    }

    fn run(&mut self, verb: &Verb) -> Ran {
        let unstarted = |why: String| Ran {
            code: 2,
            last: format!("pump: {why}"),
            red: None,
            child: None,
            started: false,
        };
        let linux = match self.handed_linux(verb) {
            Ok(linux) => linux,
            Err(why) => return unstarted(why),
        };
        // The line the verb runs is of this clone's unfinished journal, else of main's version.
        let version = self
            .journal_file()
            .ok()
            .flatten()
            .filter(|journal| !journal.finished())
            .map(|journal| journal.version)
            .or_else(|| self.version_at(&self.tip))
            .unwrap_or_default();
        // A child that cannot even start (no process slot, a moment's EAGAIN) is a COULD NOT
        // RUN: it decided nothing, and runs again by that exit's backoff, a re-base included.
        let mut child = match self.command(verb, &linux).spawn() {
            Ok(child) => child,
            Err(error) => {
                return Ran {
                    code: 3,
                    last: format!("pump: cannot start {}: {error}", self.exe.display()),
                    red: None,
                    child: None,
                    started: false,
                };
            }
        };
        // The child as a fleet claim records its holder: the release re-execs its frozen cutter
        // in place, so a claim it takes names this process.
        let child_id = aterm_agent::release_progress::identity(child.id()).map(|(boot, start)| {
            pump::Process {
                pid: child.id(),
                boot,
                start,
            }
        });
        // The run's lines are passed on as they come. A reader of its own keeps the pump off a
        // pipe that a child the run left behind (its background pre-freeze) still holds.
        let (lines, said) = std::sync::mpsc::channel::<String>();
        let reader = child.stdout.take().map(|stdout| {
            std::thread::spawn(move || {
                for line in std::io::BufReader::new(stdout).lines() {
                    let Ok(line) = line else { break };
                    {
                        let mut out = std::io::stdout().lock();
                        let _ = writeln!(out, "{line}");
                        let _ = out.flush();
                    }
                    if lines.send(line).is_err() {
                        break;
                    }
                }
            })
        });
        // A signal to the pump is the run's: it ends INTERRUPTED at its next boundary, which
        // its journal resumes from, and the pump then stops. A run that prepares and has
        // stalled is interrupted the same way, and the pump resumes it; the publish is not.
        let pid = i32::try_from(child.id()).unwrap_or(0);
        let mut ended = || match child.try_wait() {
            Ok(Some(status)) => Some(status.code().unwrap_or(130)),
            Ok(None) => None,
            Err(_) => Some(3),
        };
        let code = pump::attend(
            &mut Attended {
                ended: &mut ended,
                pid,
                version,
                state: self.state.clone(),
                record: self.record(),
                stall: aterm_toolchain::stall::Stall::default(),
                looked: None,
                fact: self.fact.clone(),
                beaten: None,
            },
            !matches!(verb, Verb::Finish),
        );
        // What the run wrote before it ended is in the pipe: give the reader the release's own
        // five-second drain grace, then go on without it.
        let until = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while reader.as_ref().is_some_and(|reader| !reader.is_finished())
            && std::time::Instant::now() < until
        {
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
        let mut last = String::new();
        let mut red = None;
        while let Ok(line) = said.try_recv() {
            if let Some(hash) = pump::caller_owned(&line) {
                red = Some(hash.to_owned());
            }
            let kept = pump::final_line([last.as_str(), line.as_str()]).to_owned();
            last = kept;
        }
        Ran {
            code,
            last,
            red,
            child: child_id,
            started: true,
        }
    }

    fn renew(&mut self, version: &str, holder: &pump::Process) {
        let renewed = crate::line_claim::renew(
            &self.repo,
            version,
            (holder.pid, &holder.boot, holder.start),
        );
        match renewed {
            Ok(_) => self.unrenewed = None,
            Err(why) if self.unrenewed.as_ref() != Some(&why) => {
                let line = format!(
                    "pump: cannot renew the lease of v{version}'s fleet claim between its runs \
                     ({why}); it tries again at its next look, and a peer that cannot ask this \
                     Mac may take the line once the lease runs out"
                );
                self.unrenewed = Some(why);
                pump::World::say(self, &line);
            }
            Err(_) => {}
        }
    }

    fn wait(&mut self, secs: u64) -> bool {
        for _ in 0..secs {
            if interrupt::caught().is_some() {
                return false;
            }
            std::thread::sleep(std::time::Duration::from_secs(1));
        }
        interrupt::caught().is_none()
    }

    fn say(&mut self, line: &str) {
        let mut out = std::io::stdout().lock();
        let _ = writeln!(out, "{line}");
        let _ = out.flush();
    }

    fn remember(&mut self, memory: &Memory) {
        let path = self.release.join(pump::MEMORY_FILE);
        let kept = aterm_toml::to_string(memory)
            .map_err(|e| e.to_string())
            .and_then(|text| {
                crate::durable::write_atomic_durable(&path, text.as_bytes(), 0o600)
                    .map_err(|e| e.to_string())
            });
        if let Err(why) = kept {
            self.say(&format!(
                "pump: cannot keep {} ({why}); a restart may repeat the last run",
                path.display()
            ));
        }
    }
}

fn remembered(release: &Path) -> Memory {
    crate::durable::read_regular_text(&release.join(pump::MEMORY_FILE), 1024 * 1024)
        .ok()
        .and_then(|text| aterm_toml::from_str(&text).ok())
        .unwrap_or_default()
}

/// `ship release --pump`: 0 once asked to stop (or after `--once`'s cycle, or a dry look), 1
/// when another pump holds this clone, 2 where there is no clone to run in.
pub fn run(repo: &Path, opts: &ReleaseOptions) -> i32 {
    let release = match crate::release::release_dir(&GitCli::new(repo)) {
        Ok(release) => release,
        Err(why) => {
            eprintln!("release pump: {why}");
            return 2;
        }
    };
    if let Err(error) = std::fs::create_dir_all(&release) {
        eprintln!("release pump: create {}: {error}", release.display());
        return 2;
    }
    let cwd = std::env::current_dir().unwrap_or_else(|_| repo.to_path_buf());
    let opts = opts.anchored(&cwd);
    let handed = Handed {
        engine: opts.engine.as_ref().map(|path| path.display().to_string()),
        credentials: opts
            .release_credentials
            .as_ref()
            .map(|path| path.display().to_string()),
        verbose: opts.verbose,
    };
    let exe = match std::env::current_exe() {
        Ok(exe) => exe,
        Err(error) => {
            eprintln!("release pump: cannot name this executable: {error}");
            return 2;
        }
    };
    let mut host = Host {
        exe,
        repo: repo.to_path_buf(),
        release: release.clone(),
        declared: if opts.linux.declared() {
            opts.linux.cut_args()
        } else {
            Vec::new()
        },
        handed,
        adopt: opts.pump_adopt,
        config: String::new(),
        candidate: None,
        tip: String::new(),
        started: crate::release_steps::started_of(std::process::id()).unwrap_or_default(),
        state: aterm_agent::harness::cli::Env::from_process()
            .ok()
            .map(|env| env.state),
        fact: None,
        unpublished: None,
        claimant: None,
        fleet: None,
        replaced: None,
        unrenewed: None,
    };
    let memory = remembered(&release);
    if opts.dry_run {
        // One look and its decision; nothing runs and nothing is kept.
        let line = match pump::observe(&mut host, &memory) {
            Ok(seen) => match pump::decide(&seen, &memory) {
                pump::Decision::Run { verb, why, .. } => {
                    let linux = host.handed_linux(&verb).unwrap_or_default();
                    format!(
                        "pump: DRY RUN — {why}; would run {RELEASE_COMMAND} {}",
                        verb.argv(&linux, &host.handed)[1..].join(" ")
                    )
                }
                pump::Decision::Wait { why, note, .. } if note.is_empty() => {
                    format!("pump: DRY RUN — would wait: {why}")
                }
                pump::Decision::Wait { why, note, .. } => {
                    format!("pump: DRY RUN — would wait: {why} ({note})")
                }
            },
            Err(why) => format!("pump: DRY RUN — cannot read the world: {why}"),
        };
        pump::World::say(&mut host, &line);
        return 0;
    }
    // One pump per clone: a second one would run the same verbs against the same journal.
    let lock = release.join("pump.lock");
    let Ok(_held) =
        aterm_update_core::FileLock::acquire_within(&lock, std::time::Duration::from_secs(2))
    else {
        eprintln!(
            "release pump: REFUSED — another pump runs in this clone ({} is held); stop it, \
             or use a clone of its own",
            lock.display()
        );
        return 1;
    };
    interrupt::install();
    pump::World::say(
        &mut host,
        &format!(
            "pump: started in {} (pid {}); it runs --open-next, --prepare [--commit|--fix], a \
             line's one re-base where it must re-qualify anyway (--prepare --commit <judged \
             pass> --supersede) and, with {}=\"1\" on main, the publish — never --forget, \
             --bump or --abandon; {}",
            repo.display(),
            std::process::id(),
            pump::PUBLISH_KEY,
            if opts.pump_adopt {
                "with --adopt it takes over a fleet line whose holder is dead or stalled"
            } else {
                "it leaves a fleet line another holder started to that holder"
            }
        ),
    );
    pump::pump(&mut host, memory, opts.pump_once.then_some(1));
    pump::World::say(&mut host, "pump: stopped");
    0
}

/// The pump's launchd agent, one per user.
const LABEL: &str = "systems.alab.aterm-release-pump";

/// The seconds launchd gives the pump between SIGTERM and SIGKILL: the pump passes the stop on
/// to the release it runs and waits for it to end INTERRUPTED at a boundary.
const EXIT_TIMEOUT_SECS: u64 = 120;

/// How long an uninstall or a re-install waits for the loaded pump to be gone: its whole
/// `ExitTimeOut` and a margin, so a pump mid-run is waited out, never given up on.
const UNLOAD_WAIT: std::time::Duration = std::time::Duration::from_secs(EXIT_TIMEOUT_SECS + 30);

/// The arguments the installed agent runs the pump with: `release --pump`, the Linux
/// declaration and the flags it hands each verb. Never a mode of one invocation (`--install`,
/// `--once`, `--dry-run`).
fn service_args(opts: &ReleaseOptions) -> Vec<String> {
    let mut args = vec!["release".to_string(), "--pump".to_string()];
    if opts.pump_adopt {
        args.push("--adopt".into());
    }
    if opts.linux.declared() {
        args.extend(opts.linux.cut_args());
    }
    for (flag, path) in [
        ("--engine", &opts.engine),
        ("--release-credentials", &opts.release_credentials),
    ] {
        if let Some(path) = path {
            args.extend([flag.to_string(), path.display().to_string()]);
        }
    }
    if opts.verbose {
        args.push("--verbose".into());
    }
    args
}

/// The agent's property list. `ProcessType Interactive` because a launchd job otherwise runs,
/// and starts every child, at utility QoS, which starves the release's paint measurement
/// (docs/RELEASING.md § *MEASURE's paint rows must run at interactive QoS*); `KeepAlive`
/// because a pump that died is the gap the pump exists to close. The executable is an immutable
/// copy and the working directory the release clone, so neither depends on a worktree.
fn manifest(
    exe: &Path,
    repo: &Path,
    release: &Path,
    home: &Path,
    path: &str,
    args: &[String],
) -> String {
    use aterm_verify::launch_agent::xml;
    let words: String = std::iter::once(exe.to_string_lossy().into_owned())
        .chain(args.iter().cloned())
        .map(|word| format!("<string>{}</string>", xml(&word)))
        .collect();
    let (repo, home, path) = (
        xml(&repo.to_string_lossy()),
        xml(&home.to_string_lossy()),
        xml(path),
    );
    let log = xml(&release.join("pump.log").to_string_lossy());
    let err = xml(&release.join("pump.err").to_string_lossy());
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
<key>Label</key><string>{LABEL}</string>
<key>ProgramArguments</key><array>{words}</array>
<key>WorkingDirectory</key><string>{repo}</string>
<key>EnvironmentVariables</key><dict><key>HOME</key><string>{home}</string><key>PATH</key><string>{path}</string></dict>
<key>RunAtLoad</key><true/><key>KeepAlive</key><true/>
<key>ThrottleInterval</key><integer>60</integer>
<key>ExitTimeOut</key><integer>{EXIT_TIMEOUT_SECS}</integer>
<key>ProcessType</key><string>Interactive</string>
<key>StandardOutPath</key><string>{log}</string>
<key>StandardErrorPath</key><string>{err}</string>
</dict></plist>
"#
    )
}

/// `--pump --install` and `--pump --uninstall`: the line that says what was done.
/// # Errors
/// Why the agent is not as asked.
pub fn service(repo: &Path, opts: &ReleaseOptions) -> Result<String, String> {
    if !cfg!(target_os = "macos") {
        return Err("the pump's service is a launchd agent: macOS only".into());
    }
    let home = std::env::var_os("HOME").ok_or("no HOME")?;
    let home = Path::new(&home);
    if opts.pump_uninstall {
        use aterm_verify::launch_agent::Removed;
        return Ok(
            match aterm_verify::launch_agent::remove(home, LABEL, UNLOAD_WAIT)? {
                Removed::Unloaded => format!(
                    "release pump uninstalled: {LABEL} is unloaded and its property list removed"
                ),
                Removed::Absent => format!("release pump: {LABEL} was not installed"),
                Removed::Exiting(why) => format!(
                    "release pump uninstalled: {LABEL}'s property list is removed, so it does \
                     not start again; its job was still ending when the wait gave up ({why}) — \
                     launchd ends it, and the release it ran stops INTERRUPTED at its next \
                     boundary, which its journal resumes from"
                ),
            },
        );
    }
    let release = crate::release::release_dir(&GitCli::new(repo))?;
    std::fs::create_dir_all(&release).map_err(|e| format!("create {}: {e}", release.display()))?;
    let cwd = std::env::current_dir().unwrap_or_else(|_| repo.to_path_buf());
    let opts = opts.anchored(&cwd);
    // Immutable deployments: never overwrite a possibly executing Mach-O, and never depend on
    // the build directory this image ran from.
    let token = aterm_uds::rand::hex_token::<16>().map_err(|e| e.to_string())?;
    let deployment = home
        .join(".local/state/aterm-release/.ubd/pump/controllers")
        .join(token);
    std::fs::create_dir_all(&deployment).map_err(|e| e.to_string())?;
    let exe = deployment.join("aterm-release");
    std::fs::copy(std::env::current_exe().map_err(|e| e.to_string())?, &exe)
        .map_err(|e| e.to_string())?;
    let text = manifest(
        &exe,
        repo,
        &release,
        home,
        &std::env::var("PATH").unwrap_or_default(),
        &service_args(&opts),
    );
    let target = aterm_verify::launch_agent::replace(home, LABEL, &text, UNLOAD_WAIT)?;
    // Each install leaves one copy: remove the earlier ones no agent names and no process runs.
    if let Some(controllers) = deployment.parent() {
        let _ = crate::release_leftovers::sweep_controllers(
            controllers,
            &deployment,
            std::time::SystemTime::now(),
            &crate::artifact::remove_unused_dir,
        );
    }
    Ok(format!(
        "release pump installed: {target}\nclone={}\nlog={}\nstop it: {RELEASE_COMMAND} --pump --uninstall",
        repo.display(),
        release.join("pump.log").display()
    ))
}

#[cfg(test)]
#[path = "release_pump_tests.rs"]
mod tests;
