// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Read-only upgrade watch ([`super::watch_at`], [`super::Opts::dry_run`], [`look_word`]).

use std::path::PathBuf;

use super::upgrade::{self, Candidate, Phase, Source, Step, Version};
use super::{
    AT_POINT, Opts, Proc, Report, St, TAIL_BYTES, at_second, due_by, ledger, load, now_s, rearm,
    rounds_step, save_at, state_dir, step, sweep_lock, tail_to_end, watch_at,
};

/// Who looks and why ([`watch`], [`watch_at`]).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Watcher {
    /// The aterm build that looks (`<version>+g<commit>`).
    pub by: String,
    /// Latest withholding loop guard ([`crate::supervise::Guard::word`]); empty if none.
    pub guard: String,
    /// The last point the loop offered, unix seconds; `0`: none.
    pub point_at: u64,
    /// Told to look (a worker's start, an activation notice), not due.
    pub told: bool,
}

/// What one [`watch`] read and wrote.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Watched {
    /// The step the reducer would take now.
    pub step: String,
    /// Word recorded on the read's record ([`look_word`], [`St::held_past_go`]).
    pub wait: String,
    /// The target the read found (`<version>(<source>)`, or `-`).
    pub to: String,
    /// The reset of the usage limit the transcript (Codex: its wall) names, unix seconds.
    pub limit_until: Option<u64>,
    /// What was written, one word each.
    pub wrote: Vec<String>,
    /// Why nothing was written: `no-record`, `not-due`, `busy`, `moved`, or the lock's own refusal.
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
    pub const fn wrote(&self) -> bool {
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

/// The watch of tab `tab` at this second (see the module doc).
#[must_use]
pub fn watch(opts: &Opts, tab: &str, how: &Watcher) -> Watched {
    watch_with(opts, tab, how, now_s(), &step)
}

/// [`watch`] at `now`, reading through `read` ([`step`], [`look_word`]).
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "HarnessUpgradeProgress",
        action = "Watch",
        project = "aterm_agent::harness::upgrade_drive::progress_projection"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
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
    watch_in(opts, tab, how, now, read)
}

/// [`watch_with`]'s look ([`at_second`]).
pub(super) fn watch_in(
    opts: &Opts,
    tab: &str,
    how: &Watcher,
    now: u64,
    read: &dyn Fn(&Opts) -> Report,
) -> Watched {
    at_second(now, || look_at(opts, tab, how, now, read))
}

fn look_at(
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
    let dry = Opts {
        dry_run: true,
        only_sid: Some(tab.to_string()),
        background: false,
        ..opts.clone()
    };
    let r = read(&dry);
    let session = (r.tab == tab && r.session != "-").then(|| r.session.clone());
    let owed: Vec<(String, St)> = owed
        .into_iter()
        .filter(|(name, st)| !st.tab.is_empty() || session.as_deref() == Some(name.as_str()))
        .collect();
    if owed.is_empty() {
        return Watched::skipped("no-record");
    }
    let records = session
        .as_deref()
        .and_then(|s| owed.iter().find(|(name, _)| name == s))
        .map_or_else(
            || super::Records::of(&opts.home, None),
            |(_, st)| st.agent_records(&opts.home),
        );
    let tail = session
        .as_deref()
        .and_then(|s| records.transcript(s))
        .map(|p| tail_to_end(&p, TAIL_BYTES).0);
    let limit_until = tail.as_deref().and_then(upgrade::transcript_limit_until);
    let limit_reset = tail.as_deref().and_then(upgrade::transcript_limit_reset);
    let claude = owed
        .iter()
        .any(|(name, st)| session.as_deref() == Some(name.as_str()) && reads_transcript(st.agent));
    let wait = look_word(&r.step, claude, || status_of(&records, session.as_deref()));
    let mut out = Watched {
        step: r.step.clone(),
        wait,
        to: r.to.clone(),
        limit_until: limit_reset,
        wrote: Vec::new(),
        skipped: None,
    };
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
                tab.clone_into(&mut st.tab);
            }
        }
        if !due && changed.is_empty() {
            continue;
        }
        let rearmed = changed.iter().any(|w| w.starts_with("rearmed:"));
        if ours && st.held_past_go(&r.step) {
            out.wait.clone_from(&st.wait);
        } else if ours && !out.wait.is_empty() && (rearmed || !r.step.starts_with("would-rearm:")) {
            st.note_wait(&out.wait, now);
            st.restart_look = false;
            if out.wait == upgrade::NEXT_POINT {
                st.held_by.clear();
            }
        }
        if let Some(tail) = tail
            .as_deref()
            .filter(|_| ours && reads_transcript(st.agent))
        {
            st.limit_until = limit_reset.unwrap_or(0);
            st.limit_spend = upgrade::transcript_limit_spend(tail);
        }
        out.wrote.extend(changed);
        if due {
            st.looked_at = now;
            st.looked_by.clone_from(&how.by);
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

const fn reads_transcript(agent: upgrade::Agent) -> bool {
    match agent {
        upgrade::Agent::Claude => true,
        upgrade::Agent::Codex
        | upgrade::Agent::Muse
        | upgrade::Agent::Agy
        | upgrade::Agent::Gemini => false,
    }
}

/// Status word for `step` ([`upgrade::wait_word`], [`rounds_step`], or [`AT_POINT`]).
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
            let st = load(opts, &name).filter(|st| {
                (st.tab == tab || st.tab.is_empty()) && st.is_upgrade() && due_by(st).is_some()
            })?;
            Some((name, st))
        })
        .collect();
    out.sort_by(|a, b| a.0.cmp(&b.0));
    out
}

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
        let (looked_at, looked_by, point_at, guard) = (
            st.looked_at,
            st.looked_by.clone(),
            st.point_at,
            st.guard.clone(),
        );
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
