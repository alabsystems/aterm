// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `@<sid> harness probe repo` (HX-22's probe, `docs/DESIGN-harness-interface-2026-11-12.md`
//! §2.4): a harness that holds the session's proven claim reads what the window harness's own
//! Land reads of the session's repository ([`aterm_agent::supervise::IdleHost::repo_work`]):
//! what it holds that no remote main has (changed, untracked and stashed work, unpushed and
//! unmerged commits, work parked on another branch the session used) and, where it holds
//! nothing, the branch and commit that was measured. The folder is the one the tab's agent works
//! in as the window harness's snapshot of it names it ([`crate::harness_host::snapshot_subject`]),
//! where that snapshot names the claimant, else the claimant's own working directory; the reader is
//! the window harness's ([`crate::harness_host::session_repo_work_under`]), run on a thread of
//! its own for each tab ([`Probing`]), so an ask never waits past [`PROBE_WAIT`]: a read that
//! goes on answers `read=pending`, and the tab's next ask answers it.

use std::path::PathBuf;
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use aterm_agent::harness::repo_work::{CleanAt, ProbeAnswer, Probing, RepoRead, RepoWork, Used};

use crate::control::pct_encode;
use crate::self_claim::Proven;

/// How long one ask waits for its tab's read before it answers `read=pending`: inside the 500 ms
/// a harness's loop bounds one exchange by, so the ask never costs the harness its exchange. The
/// read goes on (each of its git reads bounded by
/// [`aterm_agent::harness::repo_work::PROBE_LIMIT`]) and the tab's next ask answers it.
pub(crate) const PROBE_WAIT: Duration = Duration::from_millis(400);

/// The most tabs whose reads [`Probes`] keeps: past that, the one asked about longest ago is
/// dropped (a read of it that goes on ends unasked, and its next ask begins another).
const PROBED_TABS: usize = 64;

const DENIED: &str = "ERR denied\n";

/// Where a tab's probe reads ([`Subject`]; the reply's `from=`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Source {
    /// The window harness's snapshot of the tab's agent, which names the claimant: the very
    /// folder and branches its own Land reads.
    Snapshot,
    /// The claimant's own process (no snapshot names it): its working directory, and the
    /// branches its folder's `HEAD` moved to or from since it began.
    Claimant,
}

impl Source {
    const fn word(self) -> &'static str {
        match self {
            Self::Snapshot => "snapshot",
            Self::Claimant => "claimant",
        }
    }
}

/// What one probe of the claimant's tab reads: the folder, what the session used beyond its
/// `HEAD`, and where they came from.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Subject {
    /// The folder whose repository is read.
    pub(crate) dir: PathBuf,
    /// What the session used beyond that folder's `HEAD`.
    pub(crate) used: Used,
    /// Where `dir` and `used` came from.
    pub(crate) source: Source,
}

/// The subject of tab `sid` for its claimant `proven`, read live: the window harness's published
/// snapshot of the tab's agent where it names that very process (pid and kernel start), read by
/// [`crate::harness_host::snapshot_subject`] as the window harness's own probe reads it; else the
/// claimant's working directory while the process it proved is still the one at its pid, with
/// the branches its folder's `HEAD` moved to or from since its start; `None` where neither names
/// an absolute folder.
pub(crate) fn live_subject(sid: &str, proven: &Proven) -> Option<Subject> {
    if let Some((dir, used)) = crate::harness_host::published_snapshot(sid)
        .filter(|snap| snap.pid == proven.pid && snap.start == proven.start)
        .and_then(|snap| crate::harness_host::snapshot_subject(&snap))
    {
        return Some(Subject {
            dir,
            used,
            source: Source::Snapshot,
        });
    }
    let dir = aterm_sysprobe::process_cwd(proven.pid).filter(|dir| dir.is_absolute())?;
    proven.alive().then(|| Subject {
        dir,
        used: Used::of_session(
            aterm_agent::harness::unix_time::parse_lstart(&proven.start),
            None,
        ),
        source: Source::Claimant,
    })
}

/// One read's answer, and when it began (unix ms; `None` where the clock could not be read, or
/// for a read whose thread could not start or panicked).
#[derive(Debug)]
pub(crate) struct Timed {
    read: RepoRead,
    began_ms: Option<u64>,
}

impl ProbeAnswer for Timed {
    fn unread() -> Self {
        Self {
            read: RepoRead::Unread,
            began_ms: None,
        }
    }
}

/// The reads beside the verb, one for each tab and its subject ([`Probing`]): a read that outlasts
/// an ask goes on, and the next ask for the same tab and subject answers it. An ask for a tab
/// whose subject moved begins a read of the new one; the old read ends unasked. At most
/// [`PROBED_TABS`] tabs are kept, the one asked about longest ago dropped first.
#[derive(Default)]
pub(crate) struct Probes(Mutex<Vec<Probed>>);

/// One tab's reads in [`Probes`]: of `subject`, for tab `tab`.
struct Probed {
    tab: String,
    subject: Subject,
    probing: Arc<Probing<Timed>>,
}

impl Probes {
    /// The reads of tab `sid` for `subject`, moved to the front.
    fn of(&self, sid: &str, subject: &Subject) -> Arc<Probing<Timed>> {
        let mut tabs = self.0.lock().unwrap_or_else(PoisonError::into_inner);
        let kept = tabs
            .iter()
            .position(|probed| probed.tab == sid)
            .map(|at| tabs.remove(at))
            .filter(|probed| probed.subject == *subject);
        let probed = kept.unwrap_or_else(|| Probed {
            tab: sid.to_string(),
            subject: subject.clone(),
            probing: Arc::new(Probing::named("aterm-repo-probe-verb")),
        });
        let probing = Arc::clone(&probed.probing);
        tabs.insert(0, probed);
        tabs.truncate(PROBED_TABS);
        probing
    }
}

/// The window's reads ([`Probes`]).
pub(crate) static PROBES: Probes = Probes(Mutex::new(Vec::new()));

/// The verb's ask is never stopped: [`PROBE_WAIT`] alone bounds it.
static NEVER_STOPPED: AtomicBool = AtomicBool::new(false);

/// What the verb reads with (the dispatch passes [`Reads::live`]; tests their own).
pub(crate) struct Reads<'a> {
    /// The person's home, which the probe reads git's configuration under (none: it reads
    /// nothing, as the window harness's does not).
    pub(crate) home: Option<PathBuf>,
    /// aterm's state root, in whose summary folder the probe reads nothing.
    pub(crate) state: Option<PathBuf>,
    /// The subject of a tab for its claimant's proof ([`live_subject`]).
    pub(crate) subject: &'a dyn Fn(&str, &Proven) -> Option<Subject>,
    /// The tabs' reads ([`PROBES`]).
    pub(crate) probes: &'a Probes,
    /// How long an ask waits for its read ([`PROBE_WAIT`]).
    pub(crate) wait: Duration,
}

impl Reads<'static> {
    /// The window's: the home and state root the window harness's probe reads under, the live
    /// subject, the window's reads and [`PROBE_WAIT`].
    pub(crate) fn live() -> Self {
        Self {
            home: aterm_primer::home_dir(),
            state: aterm_types::dirs::state_dir(),
            subject: &live_subject,
            probes: &PROBES,
            wait: PROBE_WAIT,
        }
    }
}

/// THE PROOF OF THE SESSION'S STANDING CLAIM, AS THIS LINE MAY READ UNDER IT: the live claim's
/// proven process, a codex-alab self-claim's or a rostered harness's
/// ([`crate::session_timeline::SupervisorClaim::identity`]), where the line came on the claim's
/// connection `conn` or `prove` names that very process; `None` for an unproven claim, no claim,
/// or a line neither binds to it. `prove` is asked with no lock held.
fn claimant(
    ctx: &crate::SessionCtx,
    conn: Option<u64>,
    prove: &dyn Fn(&str) -> Option<Proven>,
) -> Option<Proven> {
    let (holder, proven, bound) = {
        let meta = ctx.meta.lock().unwrap_or_else(PoisonError::into_inner);
        let claim = meta.live_claim(crate::metrics::now_us())?;
        (
            claim.holder.clone(),
            claim.identity()?.clone(),
            conn.is_some() && claim.conn == conn,
        )
    };
    (bound || prove(&holder).as_ref() == Some(&proven)).then_some(proven)
}

/// The verb's control-thread half: `harness probe repo` (any other `probe` line is the `harness`
/// usage line). Only an Owner line under the session's standing proven claim ([`claimant`])
/// reads; anything else is `ERR denied`, with nothing read.
pub(crate) fn cmd(
    ctx: &crate::SessionCtx,
    rest: &str,
    owner: bool,
    conn: Option<u64>,
    prove: &dyn Fn(&str) -> Option<Proven>,
    reads: &Reads<'_>,
) -> String {
    if rest.split_whitespace().collect::<Vec<_>>() != ["probe", "repo"] {
        return crate::harness_recovery_verb::USAGE.to_string();
    }
    if !owner {
        return DENIED.to_string();
    }
    let Some(proven) = claimant(ctx, conn, prove) else {
        return DENIED.to_string();
    };
    let sid = ctx.self_id.as_str();
    let Some(subject) = (reads.subject)(sid, &proven) else {
        return reply(None, Some(&Timed::of(RepoRead::Clean)));
    };
    let probing = reads.probes.of(sid, &subject);
    let answer = probing.ask(
        || {
            let (home, state) = (reads.home.clone(), reads.state.clone());
            let Subject { dir, used, .. } = subject.clone();
            move || {
                let began_ms = aterm_time::unix_millis();
                let read = crate::harness_host::session_repo_work_under(
                    &dir,
                    home.as_deref(),
                    state.as_deref(),
                    &used,
                );
                Timed { read, began_ms }
            }
        },
        reads.wait,
        &NEVER_STOPPED,
    );
    reply(Some(subject.source), answer.as_ref())
}

impl Timed {
    /// `read`, begun now.
    fn of(read: RepoRead) -> Self {
        Self {
            read,
            began_ms: aterm_time::unix_millis(),
        }
    }
}

/// The reply line, newline-terminated, its keys in this order and only ever added to:
/// `read=pending from=<source>` while the read goes on; `read=unread clean=0 as_of_ms= from=` for
/// a read that could not read the repository; else `read=<clean|checked|found|gone> clean=<0|1>
/// as_of_ms= from=`, what was measured (`repo= branch= head= main_ref= remote=`) and the counts.
/// `from=none` (no folder named) reads clean, as the window harness's probe with no snapshot does.
fn reply(source: Option<Source>, answer: Option<&Timed>) -> String {
    let from = source.map_or("none", Source::word);
    let Some(answer) = answer else {
        return format!("OK read=pending from={from}\n");
    };
    let read = &answer.read;
    let word = match read {
        RepoRead::Clean => "clean",
        RepoRead::Checked(_) => "checked",
        RepoRead::Found(_) => "found",
        RepoRead::Gone(_) => "gone",
        RepoRead::Unread => "unread",
    };
    let as_of = answer
        .began_ms
        .map_or_else(|| "-".to_string(), |ms| ms.to_string());
    let mut line = format!(
        "OK read={word} clean={} as_of_ms={as_of} from={from}",
        u8::from(read.is_clean())
    );
    if read.is_unread() {
        line.push('\n');
        return line;
    }
    let none = RepoWork::default();
    let none_at = CleanAt::default();
    let work = read.found().unwrap_or(&none);
    let at = read.checked().unwrap_or(&none_at);
    let text = |v: &str| {
        if v.is_empty() {
            "-".to_string()
        } else {
            pct_encode(v)
        }
    };
    let repo = match read {
        RepoRead::Found(work) => work.repo.as_str(),
        RepoRead::Checked(at) => at.repo.as_str(),
        RepoRead::Gone(dir) => dir.as_str(),
        RepoRead::Clean | RepoRead::Unread => "",
    };
    let main_ref = if work.main_ref.is_empty() {
        &at.main_ref
    } else {
        &work.main_ref
    };
    let remote = match read {
        RepoRead::Checked(at) => {
            if at.remote {
                "1"
            } else {
                "0"
            }
        }
        _ => "-",
    };
    line.push_str(&format!(
        " repo={} branch={} head={} main_ref={} remote={remote}",
        text(repo),
        text(&at.branch),
        text(&at.head),
        text(main_ref),
    ));
    line.push_str(&format!(
        " changed={} untracked={} stashes={} unpushed={} unlanded={} sub_unpushed={} parked={}",
        work.changed,
        work.untracked,
        work.stashes,
        work.unpushed,
        work.unlanded,
        work.sub_unpushed,
        work.parked
    ));
    let state = work
        .state
        .map_or_else(|| "-".to_string(), |state| format!("{state:016x}"));
    line.push_str(&format!(" state={state}"));
    for (key, names) in [
        ("path", &work.paths),
        ("sub", &work.subs),
        ("parked_on", &work.parked_on),
    ] {
        line.push_str(&format!(" {key}_count={}", names.len()));
        for (i, name) in names.iter().enumerate() {
            line.push_str(&format!(" {key}_{i}={}", text(name)));
        }
    }
    line.push('\n');
    line
}

#[cfg(test)]
#[path = "harness_probe_verb_tests.rs"]
mod tests;
