// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! The watch: a look at one tab's upgrade that records off any point.
//!
//! Every bound the upgrade has (re-ask, give-up, rest, re-arm, retarget) lives
//! inside a step, and a step runs only at a point the session offers. A session
//! offering none (a limit's wall, a login box, a turn that never ends) would
//! have no clock running, so the window's host wakes at each record's deadline
//! ([`super::watch_at`]) and hands the tab here.
//!
//! The watch has no hands. It reads by running the ordinary visit as a dry run
//! ([`super::Opts::dry_run`]) and writes only under a `try_lock` of the sweep
//! lock (a held lock is a real visit, so the watch skips). It records its own
//! fields, the word its read found ([`look_word`]), the usage limit's reset
//! (`St::limit_until`) and whether it is money (`St::limit_spend`), and makes
//! at most three hands-free changes:
//!
//! 1. THE RETARGET ([`St::for_target`]): another build than the record's,
//!    said once in the ledger as `retargeted:<from>-><to>`;
//! 2. THE LIMIT HOLD ([`St::hold_clock`]): a usage limit the transcript names
//!    a reset for holds the re-ask and READY clocks until then, with no loop
//!    episode needed;
//! 3. THE RE-ARM ([`super::rearm`]): a stopped round that has rested, where
//!    the reducer's own read says so.
//!
//! The aterm-census guard `watch-hands` fences this file: `watch` and everything it reaches
//! here may not call a hand (`turn_fenced`, `turn`, `type_line`, `terminate`,
//! `restart`, a kill, a signal) nor name an input verb.

use std::path::PathBuf;

use super::upgrade::{self, Candidate, Phase, Source, Step, Version};
use super::{
    AT_POINT, Opts, Proc, Report, St, TAIL_BYTES, at_second, due_by, ledger, load, now_s, rearm,
    rounds_step, save_at, state_dir, step, sweep_lock, tail_to_end, watch_at,
};

/// Who looks and why (the window's host, [`watch`]): the aterm build that looks
/// (`looked_by`), what the tab's loop last offered and withheld (`point_at`,
/// `guard`: the worker's `Points`, copied onto the record), and whether the host
/// was told to look (a worker's start, or an activation notice for a newer
/// build) rather than woken by a record's deadline. A told watch reads every
/// record of the tab still owed a step; a woken one only those past their
/// [`watch_at`].
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Watcher {
    /// The aterm build that looks (`<version>+g<commit>`).
    pub by: String,
    /// The loop guard that withheld the latest point it did not offer
    /// ([`crate::supervise::Guard::word`]); empty: none said.
    pub guard: String,
    /// The last point the loop offered, unix seconds; `0`: none.
    pub point_at: u64,
    /// Told to look (a worker's start, an activation notice), not due.
    pub told: bool,
}

/// What one [`watch`] read and wrote.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Watched {
    /// The step the reducer would take now (the dry run's word:
    /// `would-announce`, `wait:limited`, `would-rearm:<why>`, and after a
    /// give-up or a re-arm read off any point what holds its point,
    /// `gave-up@box`); empty when nothing was read.
    pub step: String,
    /// The word the look records on the read's own record ([`look_word`]):
    /// the fresh wait word ([`upgrade::wait_word`]) when that step waits;
    /// for a Claude Code record's read that would take the round's own step,
    /// what holds that step's point, else [`upgrade::NEXT_POINT`] (a re-arm's
    /// only once this watch re-armed the round); empty otherwise. Where the
    /// record's own word is one the act's last checks recorded past where
    /// this read stops ([`St::held_past_go`]: a notice its fence refused, a
    /// restart its last look held), that word, KEPT.
    pub wait: String,
    /// The target the read found (`<version>(<source>)`, or `-`).
    pub to: String,
    /// The reset of a usage limit the transcript (a Codex record: its
    /// wall) names, unix seconds.
    pub limit_until: Option<u64>,
    /// What was written, one word each: `looked` (the watch's own fields),
    /// `retargeted:<from>-><to>`, `held:<until>`, `rearmed:<why>`.
    pub wrote: Vec<String>,
    /// Why nothing was written, when nothing was: `no-record` (the tab has
    /// no record owed a step), `not-due`, `busy` (a real visit holds the
    /// sweep lock), `moved` (a visit moved the record since the read), or
    /// the lock's own refusal.
    pub skipped: Option<String>,
}

impl Watched {
    fn skipped(why: &str) -> Watched {
        Watched {
            skipped: Some(why.to_string()),
            ..Watched::default()
        }
    }

    /// Whether anything reached the record.
    #[must_use]
    pub fn wrote(&self) -> bool {
        !self.wrote.is_empty()
    }

    /// One line for the host's log: `watch step=… to=… wrote=…|skipped=…`.
    #[must_use]
    pub fn line(&self) -> String {
        let dash = |s: &str| {
            if s.is_empty() {
                "-".to_string()
            } else {
                s.to_string()
            }
        };
        let limit = self
            .limit_until
            .map_or_else(|| "-".to_string(), |t| t.to_string());
        match &self.skipped {
            Some(why) => format!(
                "watch step={} to={} limit={limit} skipped={why}",
                dash(&self.step),
                dash(&self.to)
            ),
            None => format!(
                "watch step={} to={} limit={limit} wrote={}",
                dash(&self.step),
                dash(&self.to),
                self.wrote.join(",")
            ),
        }
    }
}

/// The watch of tab `tab` at this second, reading through the ordinary step
/// ([`step`]) as a dry run. See the module header: it reads off any point,
/// writes only under the sweep lock's `try_lock`, and has no hands.
#[must_use]
pub fn watch(opts: &Opts, tab: &str, how: &Watcher) -> Watched {
    watch_with(opts, tab, how, now_s(), &step)
}

/// [`watch`] at `now`, reading through `read` (the product's [`step`]; a test
/// drives the real visit with its own kernel and targets). `read` is always
/// handed a dry run of `opts` for `tab`.
///
/// The look and the hands-free re-arm of `HarnessUpgradeProgress`
/// (`aterm_spec::derive::harness_upgrade_progress_model`): the watch records
/// what holds the move now as its read found it ([`look_word`]), and re-arms a
/// rested round, typing nothing, in that order when it does both, so the word
/// recorded is the new round's. Bound in `upgrade_watch_tests.rs`
/// (`the_real_watch_is_the_derived_look`).
#[cfg_attr(
    any(test, feature = "spec-anchors"),
    aterm_spec::refines(
        machine = "HarnessUpgradeProgress",
        action = "Watch",
        project = "aterm_agent::harness::upgrade_drive::progress_projection"
    )
)]
#[cfg_attr(
    any(test, feature = "spec-anchors"),
    aterm_spec::refines(
        machine = "HarnessUpgradeProgress",
        action = "WatchRearms",
        project = "aterm_agent::harness::upgrade_drive::progress_projection"
    )
)]
pub(super) fn watch_with(
    opts: &Opts,
    tab: &str,
    how: &Watcher,
    now: u64,
    read: &dyn Fn(&Opts) -> Report,
) -> Watched {
    watch_in(opts, tab, how, now, read, &super::Live)
}

/// [`watch_with`] reading a Codex record's process facts through `k` (the
/// zone its wall's reset was printed in, `codex::keep_wall`): the product's
/// [`super::Live`], a test's stand-in kernel.
///
/// The whole look runs at `now` ([`at_second`]): its read decides whether a
/// limit still holds, a wait has run out or a reset has passed at the second
/// the watch was given, as the look itself does — never the wall clock, which
/// would read a limit handed a second before its reset as over.
pub(super) fn watch_in(
    opts: &Opts,
    tab: &str,
    how: &Watcher,
    now: u64,
    read: &dyn Fn(&Opts) -> Report,
    k: &dyn super::codex::CodexKernel,
) -> Watched {
    at_second(now, || look_at(opts, tab, how, now, read, k))
}

/// [`watch_in`]'s look, on a thread whose [`now_s`] reads `now`.
fn look_at(
    opts: &Opts,
    tab: &str,
    how: &Watcher,
    now: u64,
    read: &dyn Fn(&Opts) -> Report,
    k: &dyn super::codex::CodexKernel,
) -> Watched {
    let records = tab_records(opts, tab);
    if records.is_empty() {
        return Watched::skipped("no-record");
    }
    let owed: Vec<(String, St)> = records
        .into_iter()
        .filter(|(_, st)| how.told || watch_at(st).is_some_and(|at| at <= now))
        .collect();
    if owed.is_empty() {
        return Watched::skipped("not-due");
    }
    // The read: the ordinary visit, as a dry run — it takes no lock, and
    // types, signals and writes nothing.
    let dry = Opts {
        dry_run: true,
        only_sid: Some(tab.to_string()),
        background: false,
        ..opts.clone()
    };
    let r = read(&dry);
    let session = (r.tab == tab && r.session != "-").then(|| r.session.clone());
    // A record no build gave a tab (an older build recorded it only at the
    // announcement) is this tab's only when the read found its conversation
    // here — the proof a real visit records the tab by.
    let owed: Vec<(String, St)> = owed
        .into_iter()
        .filter(|(name, st)| !st.tab.is_empty() || session.as_deref() == Some(name.as_str()))
        .collect();
    if owed.is_empty() {
        return Watched::skipped("no-record");
    }
    // Where its agent keeps its records: the Claude home its record names
    // ([`St::home`]), else the user's own.
    let records = session
        .as_deref()
        .and_then(|s| owed.iter().find(|(name, _)| name == s))
        .map_or_else(
            || super::Records::of(&opts.home, None),
            |(_, st)| st.agent_records(&opts.home),
        );
    // The conversation's transcript, read once: until when its usage limit
    // holds (the reset it names, else a re-ask's wait after a row naming none),
    // which holds the round's clocks ([`sanctioned`]), and the reset it names,
    // which the record keeps (`St::limit_until`, never the wait). `None` where
    // it could not be read, and the record keeps what it had.
    let tail = session
        .as_deref()
        .and_then(|s| records.transcript(s))
        .map(|p| tail_to_end(&p, TAIL_BYTES).0);
    let limit_until = tail.as_deref().and_then(upgrade::transcript_limit_until);
    let limit_reset = tail.as_deref().and_then(upgrade::transcript_limit_reset);
    // A CODEX record's reset is its wall's, on the screen: its words read
    // once (`codex::screen_wall_text`; `None`: no screen read, and the
    // record keeps what it had), kept as its visit keeps them
    // (`codex::keep_wall`: placed in the zone of the Codex the read found,
    // `r.pid`, the same words kept as first placed).
    let codex = owed.iter().find(|(name, st)| {
        session.as_deref() == Some(name.as_str()) && reads_screen_wall(st.agent)
    });
    let codex_wall = codex.and_then(|_| super::codex::screen_wall_text(opts, tab));
    let keep_wall = |st: &mut St, text: Option<&str>| {
        let home = PathBuf::from(&st.codex_home);
        super::codex::keep_wall(st, text, k, r.pid, &home, now);
    };
    let codex_limit = codex.zip(codex_wall.as_ref()).map(|((_, st), (text, _))| {
        let mut preview = st.clone();
        keep_wall(&mut preview, text.as_deref());
        preview.limit_until
    });
    // The word the look records on the read's own record: only a Claude Code
    // visit's dry read says where it read, so only its act reads are worded.
    let claude = owed
        .iter()
        .any(|(name, st)| session.as_deref() == Some(name.as_str()) && reads_transcript(st.agent));
    let wait = look_word(&r.step, claude, || status_of(&records, session.as_deref()));
    let mut out = Watched {
        step: r.step.clone(),
        wait,
        to: r.to.clone(),
        limit_until: limit_reset.or(codex_limit.filter(|&until| until != 0)),
        wrote: Vec::new(),
        skipped: None,
    };
    // The write, under the sweep lock or not at all: a lock that is busy is
    // a real visit, which is progress.
    let _held = match sweep_lock(opts) {
        Ok(held) => held,
        Err("another-sweep") => {
            out.skipped = Some("busy".to_string());
            return out;
        }
        Err(why) => {
            out.skipped = Some(why.to_string());
            return out;
        }
    };
    let mut moved = false;
    for (name, before) in owed {
        let Some(mut st) = load(opts, &name) else {
            continue;
        };
        let ours = session.as_deref() == Some(name.as_str());
        // Moved since the read (a visit's step), or looked at: the read is
        // stale for it, and its next deadline is its own again.
        if (st.tab != tab && !(st.tab.is_empty() && ours))
            || st.progress_key != before.progress_key
            || st.looked_at != before.looked_at
        {
            moved = true;
            continue;
        }
        let due = watch_at(&st).is_some_and(|at| at <= now);
        if !how.told && !due {
            moved = true;
            continue;
        }
        let mut changed = Vec::new();
        if ours {
            changed = sanctioned(opts, &r, &name, &mut st, limit_until, now);
            if st.tab.is_empty() {
                // The read proved it here; a visit records it the same way.
                tab.clone_into(&mut st.tab);
            }
        }
        // A told watch of a record not yet due changes what it must (the
        // retarget, the hold, the re-arm) and no more: stamping its look would
        // push the record's watch on by `WATCH_GAP` at every worker start and
        // activation notice, and a stream of those would starve the due watch
        // that alone re-parks a worker that stopped asking.
        if !due && changed.is_empty() {
            continue;
        }
        // A re-arm read's word is the new round's ([`upgrade::held_point`]):
        // recorded only over the round this watch re-armed.
        let rearmed = changed.iter().any(|w| w.starts_with("rearmed:"));
        // A word the act's own last checks recorded past where this read stops
        // (the notice's fence, the restart's last look) is kept: the read
        // cannot see what held it ([`St::held_past_go`]).
        if ours && st.held_past_go(&r.step) {
            out.wait.clone_from(&st.wait);
        } else if ours && !out.wait.is_empty() && (rearmed || !r.step.starts_with("would-rearm:")) {
            st.note_wait(&out.wait, now);
            st.restart_look = false;
            // Nothing holds the move: no process a look that waited named
            // stands beside that word (`St::held_by`, the row's `held by`).
            if out.wait == upgrade::NEXT_POINT {
                st.held_by.clear();
            }
        }
        // The limit's reset as this read says it, for the owner's view
        // (`Row::limit_passed`): Claude Code's transcript's, Codex's wall's.
        // And whether that limit is money, no usage window
        // (`St::limit_spend`).
        if let Some(tail) = tail
            .as_deref()
            .filter(|_| ours && reads_transcript(st.agent))
        {
            st.limit_until = limit_reset.unwrap_or(0);
            st.limit_spend = upgrade::transcript_limit_spend(tail);
        }
        if ours
            && reads_screen_wall(st.agent)
            && let Some((text, spend)) = &codex_wall
        {
            keep_wall(&mut st, text.as_deref());
            st.limit_spend = *spend;
        }
        out.wrote.extend(changed);
        if due {
            st.looked_at = now;
            st.looked_by.clone_from(&how.by);
            // The guard said is the one that withheld the latest point: a point
            // offered since, with no guard said after it, leaves none standing.
            if !how.guard.is_empty() {
                st.guard.clone_from(&how.guard);
            } else if how.point_at > st.point_at {
                st.guard.clear();
            }
            st.point_at = st.point_at.max(how.point_at);
            out.wrote.push("looked".to_string());
        }
        save_at(opts, &name, &st, now);
    }
    if out.wrote.is_empty() {
        out.skipped = Some(if moved { "moved" } else { "not-due" }.to_string());
    }
    out
}

/// Whether the watch reads `agent`'s record as Claude Code's: through its
/// transcript, which names its limit's reset, and through its visit's dry
/// read, which says where it read and the target it found ([`look_word`],
/// [`sanctioned`]). A Codex record's limit is its screen's wall
/// ([`reads_screen_wall`]).
fn reads_transcript(agent: upgrade::Agent) -> bool {
    match agent {
        upgrade::Agent::Claude => true,
        upgrade::Agent::Codex => false,
    }
}

/// Whether `agent`'s limit is the wall on its screen, read and kept as its
/// lane keeps it (`codex::screen_wall_text`, `codex::keep_wall`): Codex's.
fn reads_screen_wall(agent: upgrade::Agent) -> bool {
    match agent {
        upgrade::Agent::Claude => false,
        upgrade::Agent::Codex => true,
    }
}

/// The word a look records for its read `step` (`St::wait`; the watch, and
/// `HarnessUpgradeProgress`'s `Watch`): what holds the move now.
///
/// * A read that waits (`wait:<why>`): the wait's word ([`upgrade::wait_word`],
///   with Claude's own `status` beside `not-idle`).
/// * A Claude Code read (`claude`) that would take the round's next step
///   ([`rounds_step`]): what holds that step's point ([`AT_POINT`],
///   [`upgrade::held_point`]), else [`upgrade::NEXT_POINT`].
/// * Anything else: no word, and the record keeps its own.
///
/// A word the act's own last checks recorded is not overridden
/// ([`watch_with`], [`St::held_past_go`]): the dry read stops before those
/// checks.
pub(super) fn look_word(step: &str, claude: bool, status: impl FnOnce() -> String) -> String {
    if let Some(why) = step.strip_prefix("wait:") {
        return upgrade::wait_word(why, &status());
    }
    if !claude || !rounds_step(step) {
        return String::new();
    }
    step.split_once(AT_POINT).map_or_else(
        || upgrade::NEXT_POINT.to_string(),
        |(_, held)| held.to_string(),
    )
}

/// The UPGRADE records of tab `tab` still owed a step ([`due_by`]), each
/// under its file's name. Read whole or skipped: a record that does not
/// parse is no record here.
fn tab_records(opts: &Opts, tab: &str) -> Vec<(String, St)> {
    let Ok(dir) = std::fs::read_dir(state_dir(opts)) else {
        return Vec::new();
    };
    let mut out: Vec<(String, St)> = dir
        .flatten()
        .filter_map(|e| {
            let path: PathBuf = e.path();
            if path.extension().is_none_or(|x| x != "json") {
                return None;
            }
            let name = path.file_stem()?.to_string_lossy().into_owned();
            // The relaunch files its records beside the upgrade's
            // (`St::cause`): those are no upgrade's, and not watched here.
            // A record with no recorded tab is a candidate here, and kept
            // only when the read finds its conversation in this tab.
            let st = load(opts, &name).filter(|st| {
                (st.tab == tab || st.tab.is_empty()) && st.is_upgrade() && due_by(st).is_some()
            })?;
            Some((name, st))
        })
        .collect();
    out.sort_by(|a, b| a.0.cmp(&b.0));
    out
}

/// Claude's own status of `session` (its session file, among `records`: the
/// Claude home its agent keeps them under), which a `not-idle` wait word
/// carries ([`upgrade::wait_word`]); empty where none is read.
fn status_of(records: &super::Records, session: Option<&str>) -> String {
    let Some(session) = session else {
        return String::new();
    };
    records
        .session_files()
        .and_then(|files| files.into_iter().find(|sf| sf.session_id == session))
        .map(|sf| sf.status)
        .unwrap_or_default()
}

/// The target a report names (`<version>(<source>)`), as a candidate to
/// retarget onto. The file is not needed: [`St::for_target`] records the
/// version and the source alone.
fn target_of(to: &str) -> Option<Candidate> {
    let (version, source) = to.strip_suffix(')')?.split_once('(')?;
    let source = match source {
        "managed" => Source::Managed,
        "native" => Source::Native,
        _ => return None,
    };
    Some(Candidate {
        exe: PathBuf::new(),
        version: Version::parse(version)?,
        source,
    })
}

/// The three sanctioned changes made to `st` (the record the read `r` was
/// about), each said in the ledger where a step would say it. None types,
/// signals or ends anything:
///
/// * the retarget, when the read found a newer target (a Claude Code record
///   not yet restarting): the record becomes what the next visit's
///   [`St::for_target`] would make it, and an owed release is carried as that
///   visit carries it;
/// * the limit hold, when the transcript names a reset still to come;
/// * the re-arm, when the reducer's dry read says a stopped round has rested
///   (`would-rearm:<why>`) and its rest reads due here too.
fn sanctioned(
    opts: &Opts,
    r: &Report,
    name: &str,
    st: &mut St,
    limit_until: Option<u64>,
    now: u64,
) -> Vec<String> {
    let mut wrote = Vec::new();
    let claude = reads_transcript(st.agent);
    let unbegun = matches!(
        st.phase,
        Phase::Pending | Phase::Announced { .. } | Phase::Failed(_)
    );
    if claude
        && unbegun
        && let Some(to) = target_of(&r.to)
        && Version::parse(&st.to).as_ref() != Some(&to.version)
        && let Some(from) = Version::parse(&r.from)
    {
        let old = st.to.clone();
        let tab = st.tab.clone();
        // The watch's own fields ride over the new record: a retarget is no
        // look forgotten.
        let (looked_at, looked_by, point_at, guard) = (
            st.looked_at,
            st.looked_by.clone(),
            st.point_at,
            st.guard.clone(),
        );
        // The model the next visit would hand it: an announced round's
        // promise ([`St::promised_model`]). A pending round's the watch cannot
        // read (it is the visit's model decision), and it has no notice to
        // lose.
        let promised = st.promised_model().map(str::to_owned);
        let how;
        (*st, how) = St::retarget(
            Some(st.clone()),
            &from,
            &to,
            promised.as_deref(),
            Proc::default(),
            now,
        );
        // A READY COVERS ANY VERSION (the owner's ruling of 2026-09-29): a
        // round not yet restarting, for the same model and on the build the
        // session still runs, moves onto the newer build in place.
        if st.phase == Phase::Pending {
            st.tab = tab;
        }
        (st.looked_at, st.looked_by, st.point_at, st.guard) =
            (looked_at, looked_by, point_at, guard);
        let word = format!("retargeted:{old}->{}", to.version);
        ledger(
            opts,
            &Report {
                session: name.to_string(),
                step: word.clone(),
                ..r.clone()
            },
            &format!(
                "upgrade target changed: {old} -> {}; {}",
                to.version,
                super::retarget_how(how, st)
            ),
        );
        wrote.push(word);
    }
    if claude
        && let Some(until) = limit_until.filter(|until| *until > now)
        && st.hold_clock(until)
    {
        wrote.push(format!("held:{until}"));
    }
    if r.step.starts_with("would-rearm")
        && matches!(st.phase, Phase::Failed(_))
        && upgrade::retry_due(&st.phase, st.failed_for(now), false)
        && upgrade::rearm_held(&st.request_for(&st.tab), Step::Rearm, &st.to, now) == Step::Rearm
    {
        let done = rearm(
            opts,
            Report {
                session: name.to_string(),
                ..r.clone()
            },
            st,
            now,
        );
        wrote.push(done.step);
    }
    wrote
}
