// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! THE WATCH (design record 2026-09-28, "No upgrade stuck forever", §3.2 C4;
//! rollout step 7): a look at one tab's upgrade records OFF ANY POINT.
//!
//! Tab #1 sat on Claude Code 2.1.280 for three days because every bound the
//! upgrade has — the re-ask, the give-up, the rest, the re-arm, the retarget
//! — lived inside a step, and a step ran only at a point the session offered
//! (an idle point, a settled break). A session that offered none (a weekly
//! limit's wall at a break, a login box, a turn that never ended) had no clock
//! running at all: the thing keeping the time was the event being waited for.
//! The window's host now wakes at each record's deadline
//! ([`super::watch_at`], named to it by the owner's view) and hands the tab
//! here, whatever the session is doing.
//!
//! THE WATCH HAS NO HANDS. It reads by running the ordinary visit as a DRY
//! RUN ([`super::Opts::dry_run`]: no save, no ledger, no sweep lock, nothing
//! typed, signalled or ended — the one key a dry run could still press, the
//! Codex lane's clear of its own left-typed text, was closed with this step),
//! and it writes only under a `try_lock` of the sweep lock — a lock that is
//! busy is a real visit running, which is progress, and the watch skips. What
//! it writes is the watch's own fields (`looked_at`, `looked_by`, `point_at`,
//! `guard`), the fresh wait word the read found, and at most the three
//! changes of the record that are already sanctioned as hands-free:
//!
//! 1. THE RETARGET ([`St::for_target`]): a newer build than the record's,
//!    said once in the ledger as `retargeted:<from>-><to>`;
//! 2. THE LIMIT HOLD ([`St::hold_clock`]): a usage limit the transcript names
//!    a reset for holds the re-ask and READY clocks until then, with no loop
//!    episode needed;
//! 3. THE RE-ARM ([`super::rearm`]): a stopped round that has rested, where
//!    the reducer's own read says so.
//!
//! Typing, signalling and ending stay at the loop's points under today's
//! gates. `tools/grep_guard.sh` H1 fences this file lexically: `watch` and
//! every function of this file it reaches may not call a hand (`turn_fenced`,
//! `turn`, `type_line`, `terminate`, `restart`, a kill, a signal) nor name an
//! input verb.

use std::path::PathBuf;

use super::upgrade::{self, Candidate, Phase, Source, Step, Version};
use super::{
    Opts, Report, St, TAIL_BYTES, due_by, ledger, load, now_s, rearm, save_at, session_files,
    state_dir, step, sweep_lock, tail_to_end, transcript, watch_at,
};

/// WHO LOOKS, AND WHY (the window's host, [`watch`]): the aterm build that
/// looks (`looked_by`), what the tab's loop last offered and withheld
/// (`point_at`, `guard`: the worker's `Points`, copied onto the record), and
/// whether the host was TOLD to look — a worker's start (a hot swap, an
/// attach) or an activation notice (a newer build installed) — rather than
/// woken by a record's own deadline. A told watch reads every record of the
/// tab still owed a step; a woken one only those past their
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
    /// `would-announce`, `wait:limited`, `would-rearm:<why>`, …); empty when
    /// nothing was read.
    pub step: String,
    /// The fresh wait word ([`upgrade::wait_word`]) when that step waits;
    /// empty otherwise.
    pub wait: String,
    /// The target the read found (`<version>(<source>)`, or `-`).
    pub to: String,
    /// The reset of a usage limit the transcript names, unix seconds.
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

/// THE WATCH of tab `tab` (design record 2026-09-28, §3.2 C4), at this
/// second, reading through the ordinary step ([`step`]) as a dry run. See the
/// module header: it reads off any point, writes only under the sweep lock's
/// `try_lock`, and has no hands.
#[must_use]
pub fn watch(opts: &Opts, tab: &str, how: &Watcher) -> Watched {
    watch_with(opts, tab, how, now_s(), &step)
}

/// [`watch`] at `now`, reading through `read` (the product's [`step`]; a
/// test drives the real visit with its own kernel and targets). `read` is
/// handed a DRY RUN of `opts` for `tab`, always.
pub(super) fn watch_with(
    opts: &Opts,
    tab: &str,
    how: &Watcher,
    now: u64,
    read: &dyn Fn(&Opts) -> Report,
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
    // THE READ: the ordinary visit, as a dry run — it takes no lock, and
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
    let limit_until = session
        .as_deref()
        .and_then(|s| transcript(&opts.home, s))
        .and_then(|p| upgrade::transcript_limit_until(&tail_to_end(&p, TAIL_BYTES).0));
    let wait = r
        .step
        .strip_prefix("wait:")
        .map(|why| upgrade::wait_word(why, &status_of(opts, session.as_deref())))
        .unwrap_or_default();
    let mut out = Watched {
        step: r.step.clone(),
        wait,
        to: r.to.clone(),
        limit_until,
        wrote: Vec::new(),
        skipped: None,
    };
    // THE WRITE, under the sweep lock or not at all: a lock that is busy is
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
        // A TOLD watch of a record not yet due changes what it must — the
        // retarget, the hold, the re-arm — and no more: stamping its look
        // would push the record's own watch on by `WATCH_GAP` at every
        // worker start and activation notice, and a stream of them (hot
        // swaps, a reaped and restarted worker) would hold off for ever the
        // due watch that alone re-parks a worker that stopped asking (the
        // watch's review, 2026-09-28).
        if !due && changed.is_empty() {
            continue;
        }
        if ours && !out.wait.is_empty() {
            st.note_wait(&out.wait, now);
        }
        out.wrote.extend(changed);
        if due {
            st.looked_at = now;
            st.looked_by.clone_from(&how.by);
            // The guard said is the one that withheld the latest point: a
            // point offered since the one recorded, with no guard said after
            // it, leaves no guard standing (the watch's review, 2026-09-28:
            // `guard=wall` stood beside a later point).
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

/// Claude's own status of `session` (its session file), which a `not-idle`
/// wait word carries ([`upgrade::wait_word`]); empty where none is read.
fn status_of(opts: &Opts, session: Option<&str>) -> String {
    let Some(session) = session else {
        return String::new();
    };
    session_files(&opts.home)
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

/// THE THREE SANCTIONED CHANGES (design record 2026-09-28, §3.2 C4), made to
/// `st` — the record the read `r` was about — and each said in the ledger
/// where a step would say it. None of them types, signals or ends anything:
///
/// * the retarget, when the read found a newer target than the record's
///   (a Claude Code record that has not begun restarting): the record is
///   what the next visit's [`St::for_target`] would make it, and the owed
///   release the old notice leaves is carried as that visit carries it;
/// * the limit hold, when the transcript names a reset still to come;
/// * the re-arm, when the reducer's own dry read says a stopped round has
///   rested (`would-rearm:<why>`) and its rest reads due here too.
fn sanctioned(
    opts: &Opts,
    r: &Report,
    name: &str,
    st: &mut St,
    limit_until: Option<u64>,
    now: u64,
) -> Vec<String> {
    let mut wrote = Vec::new();
    let claude = st.agent == upgrade::Agent::Claude;
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
        *st = St::for_target(Some(st.clone()), &from, &to, None, now);
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
                "the watch read a newer build than the record's ({old} -> {}) off any point: the \
                 upgrade moves to it as the next visit would, pending again, with the release \
                 the old notice owes carried",
                to.version
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
