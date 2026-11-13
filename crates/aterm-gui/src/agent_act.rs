// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE HOST'S WORD ON A SESSION'S AGENT (H6; the law is [`aterm_agent::harness::agent_image`]'s:
//! a name steers, the kernel confirms, an agent nothing confirms is observed).
//!
//! The foreground group's leader is read where the kernel keeps it: the image's path and the
//! file it was mapped from, the process's start, the team of a Developer ID signature the kernel
//! validated ([`live_team`]), its pid version (Apple's, which every exec moves) and, for a script
//! shell, its script. The verdict ([`Verdict`]) is kept per session under that key ([`Key`]):
//! the same process running the same file keeps it, and so does one whose image no longer stats
//! (unlinked since its exec) while its pid version stands; an exec (another file or version) or
//! a new process (another start) is read again. A seamless update carries each confirmation to
//! its successor ([`carry_row`], [`adopt_carried`]), which keeps it for an image no read can
//! confirm again (unlinked) while the pid version stands, and reads a linked one afresh; a crash's
//! successor has no carry and reads every image afresh. The hosted loop's
//! one gate asks [`hosted`] before each input verb (`HostCtl`'s `Ctl::agent_act`), and the
//! host's roster pass reads each agent session's ([`roster_read`]); the main thread's readers
//! (`status supervisor_act=`, the session's needs, a wall's answer) take only what a read kept
//! ([`cached`]), so they never reach the kernel.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError};

use aterm_agent::harness::agent_image::{self, Act, FileId, Image, Lane, Places, Proof};
use aterm_phase::Program;
use atpkg::image::ImageRoots;

use crate::session_store::{SessionState, Store};

/// What the kernel says of a running process, through one seam: [`Kernel::LIVE`]'s reads, or a
/// test's.
#[derive(Clone, Copy)]
pub(crate) struct Kernel {
    /// The image's path as the kernel names it (macOS `proc_pidpath`, Linux `/proc/<pid>/exe`).
    pub(crate) exe: fn(u32) -> Option<PathBuf>,
    /// The identity of the file the process `pid` was mapped from, its image at `exe`.
    pub(crate) file: fn(u32, &Path) -> Option<FileId>,
    /// The group leader's start, in microseconds.
    pub(crate) start_us: fn(i32) -> Option<u64>,
    /// The team of the process's Developer ID signature ([`live_team`]); read only where no
    /// vendor layout confirms the image.
    pub(crate) team: fn(u32) -> Option<String>,
    /// A script shell's script: its `argv[1]`.
    pub(crate) script: fn(i32) -> Option<PathBuf>,
    /// The group leader's pid version, which every exec moves and an unlink of its image does
    /// not (Apple's `p_idversion`, [`aterm_uds::process::Identity::idversion`]); `None` where the
    /// kernel keeps none (Linux, whose `/proc/<pid>/exe` keeps an unlinked image's inode).
    pub(crate) version: fn(i32) -> Option<i32>,
}

impl Kernel {
    /// The live reads.
    pub(crate) const LIVE: Self = Self {
        exe: aterm_sysprobe::process_path,
        file: mapped_file,
        start_us: crate::holder_image::start_us_of,
        team: live_team,
        script: live_script,
        version: live_version,
    };
}

fn live_version(group: i32) -> Option<i32> {
    aterm_uds::process::read_identity(group).and_then(|id| id.idversion)
}

/// The mapped file: on Linux `/proc/<pid>/exe` itself (an image unlinked since its exec is still
/// that inode), elsewhere the file at the path the kernel names.
fn mapped_file(pid: u32, exe: &Path) -> Option<FileId> {
    if cfg!(target_os = "linux") {
        FileId::of(Path::new(&format!("/proc/{pid}/exe")))
    } else {
        FileId::of(exe)
    }
}

/// Whether the kernel's read of a process's signature lets its team vouch for the image: the
/// kernel validated it as Developer ID (`CS_OPS_VALIDATION_CATEGORY`), its certificate chain
/// Apple's Developer ID authority's. The status flags cannot say it: the kernel reports no
/// `CS_ADHOC` for an ad hoc signature (measured 2026-11-01, `0x22000201` for a copy of `sleep`
/// signed ad hoc), so an ad hoc signature carrying a team field would pass them.
fn vouches(id: &aterm_sysprobe::identity::Identity) -> bool {
    id.voucher() == aterm_sysprobe::identity::Voucher::DeveloperId
}

/// The signing team of process `pid`'s image, where a certificate stands behind it: the kernel
/// validated the signature as Developer ID ([`vouches`]), names its team (`CS_OPS_TEAMID`), and
/// the running code meets the requirement that its leaf certificate names that team under
/// Apple's anchor ([`aterm_containment::consent::process_meets_team`]). `None` otherwise, and off
/// macOS.
pub(crate) fn live_team(pid: u32) -> Option<String> {
    let id = aterm_sysprobe::identity::identity(pid).filter(vouches)?;
    let team = id.team?;
    aterm_containment::consent::process_meets_team(pid, &team).then_some(team)
}

fn live_script(pgid: i32) -> Option<PathBuf> {
    crate::session_status::program::argv_head_of(pgid)?
        .1
        .map(PathBuf::from)
}

/// Where the vendors' layouts and the lab's stand-ins are, read only when a verdict is.
pub(crate) struct Where {
    /// Claude Code's native installer's versions directory.
    pub(crate) versions: Option<PathBuf>,
    /// The managed atpkg prefix's roots.
    pub(crate) roots: Option<Arc<ImageRoots>>,
    /// The lab's stand-ins (a build with debug assertions; none in a shipped one).
    pub(crate) standins: Vec<FileId>,
}

impl Where {
    fn live() -> Self {
        Self {
            versions: aterm_primer::home_dir().map(|home| agent_image::claude_versions(&home)),
            roots: crate::holder_image::roots(),
            standins: agent_image::standins(),
        }
    }
}

/// What a kept verdict stands on: the same leader of the same group, started at the same time,
/// running the same script, and not exec'd since: the same pid version where the kernel keeps
/// one, else mapped from the same file.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Key {
    group: i32,
    start_us: Option<u64>,
    file: Option<FileId>,
    script: Option<FileId>,
    version: Option<i32>,
}

impl Key {
    /// Whether a verdict kept under `kept` stands for this read, so that an exec or a new process
    /// moves it and nothing else does: only where the kernel gave the process's start, the same
    /// in both, with the same group and script, and either the same pid version in both (Apple:
    /// every exec moves it, and an image unlinked since its exec, which macOS's `proc_pidpath`
    /// no longer names, keeps it, so the verdict its live read earned stands with no `codesign`
    /// run) or, where either read has none (Linux), the image's identity in both and the same.
    /// A key with neither is read afresh each time.
    fn stands_for(&self, kept: &Self) -> bool {
        self.start_us.is_some()
            && (self.group, self.start_us, self.script) == (kept.group, kept.start_us, kept.script)
            && match (self.version, kept.version) {
                (Some(now), Some(then)) => now == then,
                _ => self.file.is_some() && self.file == kept.file,
            }
    }
}

/// The host's word on one session's agent.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Verdict {
    /// What the harness may do there.
    pub(crate) act: Act,
    /// What confirmed the agent, or `None`.
    pub(crate) proof: Option<Proof>,
}

const OBSERVED: Verdict = Verdict {
    act: Act::Observe,
    proof: None,
};

struct Kept {
    key: Key,
    agent: Program,
    verdict: Verdict,
    /// A confirmation the outgoing process of a seamless update carried ([`adopt_carried`]):
    /// it stands only for a read that gives no image identity (the image unlinked since its
    /// exec), and a read that gives one is read afresh, under this build's own rules.
    carried: bool,
}

/// Each session's kept verdict, by sid.
static KEPT: Mutex<BTreeMap<String, Kept>> = Mutex::new(BTreeMap::new());

/// Each session's act last told ([`take_told`]), by sid.
static TOLD: Mutex<BTreeMap<String, Act>> = Mutex::new(BTreeMap::new());

/// Past this many sessions kept, the host's roster pass forgets the ones gone ([`forget_gone`]).
const KEPT_MAX: usize = 256;

fn key_of(group: i32, kernel: &Kernel) -> (Key, Option<PathBuf>) {
    let pid = u32::try_from(group).ok().filter(|pid| *pid > 1);
    let exe = pid.and_then(kernel.exe);
    let file = pid
        .zip(exe.as_deref())
        .and_then(|(pid, exe)| (kernel.file)(pid, exe));
    let script = exe
        .as_deref()
        .filter(|exe| crate::holder_image::is_script_shell(atpkg::image::strip_deleted(exe)))
        .and_then(|_| (kernel.script)(group))
        .filter(|script| script.is_absolute())
        .and_then(|script| FileId::of(&script));
    let start_us = pid.and_then(|_| (kernel.start_us)(group));
    let version = pid.and_then(|_| (kernel.version)(group));
    (
        Key {
            group,
            start_us,
            file,
            script,
            version,
        },
        exe,
    )
}

/// The verdict on `sid`'s `agent`, its foreground group `group`, through `kernel`: kept while the
/// kernel's key stands ([`Key::stands_for`]), read afresh otherwise, the places read by `places` only when a verdict
/// is. Nothing readable of the leader is [`Act::Observe`]. A key that moved while the verdict
/// was read is not kept: the next read reads again.
pub(crate) fn read_with(
    sid: &str,
    agent: Program,
    group: i32,
    kernel: &Kernel,
    places: &dyn Fn() -> Where,
) -> Verdict {
    let (key, exe) = key_of(group, kernel);
    if let Some(kept) = KEPT
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .get(sid)
        .filter(|kept| {
            key.stands_for(&kept.key)
                && kept.agent == agent
                && (!kept.carried || key.file.is_none())
        })
    {
        return kept.verdict;
    }
    let verdict = match (exe.as_deref(), u32::try_from(group)) {
        (Some(exe), Ok(pid)) => {
            let at = places();
            let places = Places {
                claude_versions: at.versions.as_deref(),
                roots: at.roots.as_deref(),
            };
            let unsigned = Image {
                exe,
                file: key.file,
                team: None,
                script: key.script,
            };
            // The team's read runs `codesign` on the process: only where no layout confirms it.
            let team = if agent_image::in_vendor_layout(agent, &unsigned, &places) {
                None
            } else {
                (kernel.team)(pid)
            };
            let image = Image {
                team: team.as_deref(),
                ..unsigned
            };
            let proof = agent_image::proof_among(agent, &image, &places, &at.standins);
            Verdict {
                act: Act::of(proof),
                proof,
            }
        }
        _ => OBSERVED,
    };
    if key_of(group, kernel).0 == key {
        KEPT.lock().unwrap_or_else(PoisonError::into_inner).insert(
            sid.to_string(),
            Kept {
                key,
                agent,
                verdict,
                carried: false,
            },
        );
    }
    verdict
}

/// A confirmation as a seamless update carries it to its successor (`SessionRecord::agent_act`,
/// [`adopt_carried`]): `sid`'s kept verdict when it is [`Act::On`] under a key that names its
/// leader's start and pid version, as one row, `group=<g> start=<us> version=<v> agent=<name>
/// proof=<word>`, with ` script=<dev>.<ino>` for a script shell's. Only the pid version proves to
/// another process that no exec came since the live read (Linux keeps none, and its
/// `/proc/<pid>/exe` reads an unlinked image afresh), and an observed agent is read afresh.
#[cfg(any(unix, test))]
pub(crate) fn carry_row(sid: &str) -> Option<String> {
    let kept = KEPT.lock().unwrap_or_else(PoisonError::into_inner);
    let kept = kept.get(sid)?;
    let (Act::On, Some(proof), Some(start), Some(version)) = (
        kept.verdict.act,
        kept.verdict.proof,
        kept.key.start_us,
        kept.key.version,
    ) else {
        return None;
    };
    let mut row = format!(
        "group={} start={start} version={version} agent={} proof={}",
        kept.key.group,
        kept.agent.name(),
        proof.word()
    );
    if let Some(script) = kept.key.script {
        row.push_str(&format!(" script={}.{}", script.dev, script.ino));
    }
    Some(row)
}

/// THE SUCCESSOR'S TAKE OF A CARRIED CONFIRMATION ([`carry_row`]) for adopted session `sid`: kept
/// as the host's read under the carried key, where it stands for a read of the successor's own
/// only while the kernel gives the same leader the same start and pid version (no exec since the
/// outgoing process's live read, [`Key::stands_for`]) and gives no image identity: an image
/// unlinked since its exec, whose verdict no read can earn again (macOS's `proc_pidpath` names
/// no path for it, and `codesign` cannot verify it by its pid, measured on 27.0). Whether `row`
/// read; a row that does not keeps nothing.
pub(crate) fn adopt_carried(sid: &str, row: &str) -> bool {
    let Some((key, agent, proof)) = carried_of(row) else {
        return false;
    };
    KEPT.lock().unwrap_or_else(PoisonError::into_inner).insert(
        sid.to_string(),
        Kept {
            key,
            agent,
            verdict: Verdict {
                act: Act::On,
                proof: Some(proof),
            },
            carried: true,
        },
    );
    true
}

/// The key, agent and proof a [`carry_row`] row names: each field once, the five it needs and a
/// script's identity at most; another field is a later build's and is passed over.
fn carried_of(row: &str) -> Option<(Key, Program, Proof)> {
    let (mut group, mut start, mut version, mut agent, mut proof, mut script) =
        (None, None, None, None, None, None);
    for field in row.split_ascii_whitespace() {
        let (name, value) = field.split_once('=')?;
        let again = match name {
            "group" => group.replace(value.parse::<i32>().ok().filter(|g| *g > 1)?),
            "start" => start.replace(value.parse::<u64>().ok()?).map(|_| 0),
            "version" => version.replace(value.parse::<i32>().ok()?),
            "agent" => agent
                .replace(Program::ALL.into_iter().find(|p| p.name() == value)?)
                .map(|_| 0),
            "proof" => proof
                .replace(Proof::ALL.into_iter().find(|p| p.word() == value)?)
                .map(|_| 0),
            "script" => {
                let (dev, ino) = value.split_once('.')?;
                script
                    .replace(FileId {
                        dev: dev.parse().ok()?,
                        ino: ino.parse().ok()?,
                    })
                    .map(|_| 0)
            }
            _ => None,
        };
        if again.is_some() {
            return None;
        }
    }
    Some((
        Key {
            group: group?,
            start_us: Some(start?),
            file: None,
            script,
            version: Some(version?),
        },
        agent?,
        proof?,
    ))
}

/// The act a read kept for `sid` leading `group`, without reaching the kernel; `None` before any.
pub(crate) fn cached(sid: &str, group: i32) -> Option<Act> {
    KEPT.lock()
        .unwrap_or_else(PoisonError::into_inner)
        .get(sid)
        .filter(|kept| kept.key.group == group)
        .map(|kept| kept.verdict.act)
}

/// Whether the host's last read of `sid` leading `group` observes its agent only: the main
/// thread's word for the needs and a wall's answer. Before any read the host's roster pass makes,
/// nothing is known and the session is as it was (the gate itself reads afresh).
pub(crate) fn observed(sid: &str, group: i32) -> bool {
    cached(sid, group) == Some(Act::Observe)
}

/// Keep `act` as the host's read of `sid`'s `agent` leading `group`: a test's stand-in for the
/// roster pass's read.
#[cfg(test)]
pub(crate) fn keep_for_test(sid: &str, agent: Program, group: i32, act: Act) {
    KEPT.lock().unwrap_or_else(PoisonError::into_inner).insert(
        sid.to_string(),
        Kept {
            key: Key {
                group,
                start_us: None,
                file: None,
                script: None,
                version: None,
            },
            agent,
            verdict: Verdict { act, proof: None },
            carried: false,
        },
    );
}

/// Keep `act` as the host's read of `sid`'s `agent` leading `group`, a live group the kernel
/// reads (the test's own process), under the key the kernel gives it now, so that the gate's own
/// read of it ([`read_with`] through [`Kernel::LIVE`]) stands on it; and forget what the gate
/// said of `sid`.
#[cfg(test)]
pub(crate) fn keep_live_for_test(sid: &str, agent: Program, group: i32, act: Act) {
    let (key, _) = key_of(group, &Kernel::LIVE);
    assert!(
        key.stands_for(&key),
        "the kernel reads group {group}: {key:?}"
    );
    KEPT.lock().unwrap_or_else(PoisonError::into_inner).insert(
        sid.to_string(),
        Kept {
            key,
            agent,
            verdict: Verdict { act, proof: None },
            carried: false,
        },
    );
    WITHHELD
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .remove(sid);
    SAID.lock()
        .unwrap_or_else(PoisonError::into_inner)
        .remove(sid);
}

/// Forget what the host read of `sid`, as a seamless update's successor starts knowing nothing
/// of it. Built where the carry's tests are (Unix).
#[cfg(all(test, unix))]
pub(crate) fn forget_kept_for_test(sid: &str) {
    KEPT.lock()
        .unwrap_or_else(PoisonError::into_inner)
        .remove(sid);
}

/// The confirmation a Claude Code's tab carries when the host confirmed it by its team (leader
/// `group`, started at `start_us`, under pid version `version`), for the handoff fixture desks:
/// the producer's own row ([`carry_row`]). Built where the desks are (Unix).
#[cfg(all(test, unix))]
pub(crate) fn fixture_row(group: i32, start_us: u64, version: i32) -> String {
    let sid = format!("fixture-{group}-{start_us}");
    KEPT.lock().unwrap_or_else(PoisonError::into_inner).insert(
        sid.clone(),
        Kept {
            key: Key {
                group,
                start_us: Some(start_us),
                file: None,
                script: None,
                version: Some(version),
            },
            agent: Program::Claude,
            verdict: Verdict {
                act: Act::On,
                proof: Some(Proof::Team),
            },
            carried: false,
        },
    );
    let row = carry_row(&sid).expect("a confirmation carries a row");
    forget_kept_for_test(&sid);
    row
}

/// How many withheld lines the gate logged of `sid` ([`withheld`]).
#[cfg(test)]
pub(crate) fn withheld_said(sid: &str) -> usize {
    SAID.lock()
        .unwrap_or_else(PoisonError::into_inner)
        .get(sid)
        .copied()
        .unwrap_or(0)
}

/// Each session's count of the withheld lines [`withheld`] logged: a test's ear on `aterm.log`.
#[cfg(test)]
static SAID: Mutex<BTreeMap<String, usize>> = Mutex::new(BTreeMap::new());

/// Whether `act` is news for `sid`: not the act last told of it. It is told from here on.
pub(crate) fn take_told(sid: &str, act: Act) -> bool {
    TOLD.lock()
        .unwrap_or_else(PoisonError::into_inner)
        .insert(sid.to_string(), act)
        != Some(act)
}

/// Forget what was told of `sid`: its supervisor moved (a claim taken, released or lapsed), and
/// the session log's latest `field=supervisor` mark says no act, so the next read under a claim
/// tells it again (`session_timeline::record_supervisor`).
pub(crate) fn forget_told(sid: &str) {
    TOLD.lock()
        .unwrap_or_else(PoisonError::into_inner)
        .remove(sid);
}

/// Forget every session `live` does not name, once more than [`KEPT_MAX`] are kept.
pub(crate) fn forget_gone(live: &[&str]) {
    prune(
        &mut KEPT.lock().unwrap_or_else(PoisonError::into_inner),
        live,
    );
    prune(
        &mut TOLD.lock().unwrap_or_else(PoisonError::into_inner),
        live,
    );
    prune(
        &mut WITHHELD.lock().unwrap_or_else(PoisonError::into_inner),
        live,
    );
}

/// `map` less every sid `live` does not name, once it holds more than [`KEPT_MAX`].
fn prune<T>(map: &mut BTreeMap<String, T>, live: &[&str]) {
    if map.len() > KEPT_MAX {
        map.retain(|sid, _| live.contains(&sid.as_str()));
    }
}

/// The session log's `why=` for a session's act moving to `act`.
pub(crate) const fn why(act: Act) -> &'static str {
    match act {
        Act::On => "confirmed",
        Act::Observe => "unconfirmed",
    }
}

/// `status supervisor_act=`: the act the host's last read kept for the agent it admits in the
/// session ([`cached`]: the control thread reads no image), and `-` where the host admits no
/// agent or has read none of its image yet (the gate reads it before the harness types).
pub(crate) fn status_word(sid: &str, agent: Option<Program>, group: i32) -> &'static str {
    agent
        .and_then(|_| cached(sid, group))
        .map_or("-", Act::word)
}

/// THE HOSTED LOOP'S GATE READ: the act for `sid`'s agent now, read afresh against the kernel
/// (what [`read_with`] kept stands only while the key does). A session gone, or one whose foreground
/// the host admits as no agent, is observed. Each move of the act is told once ([`tell`]).
pub(crate) fn hosted(store: &Store, sid: &str) -> Act {
    hosted_with(store, sid, &Kernel::LIVE, &Where::live)
}

/// [`hosted`] through `kernel` and `places` ([`read_with`]).
pub(crate) fn hosted_with(
    store: &Store,
    sid: &str,
    kernel: &Kernel,
    places: &dyn Fn() -> Where,
) -> Act {
    let ctx = {
        let guard = store.read().unwrap_or_else(PoisonError::into_inner);
        let Some(h) = guard
            .by_sid(&aterm_session::SessionId::new(sid))
            .filter(|h| h.state != SessionState::Exited)
        else {
            return Act::Observe;
        };
        Arc::clone(&h.ctx)
    };
    let (agent, group) = {
        let timeline = ctx.timeline.lock().unwrap_or_else(PoisonError::into_inner);
        (
            crate::harness_host::agent_of(timeline.agent()),
            timeline.agent().program_pgid,
        )
    };
    let Some(agent) = crate::harness_host::admitted_as(&ctx, agent, group) else {
        return Act::Observe;
    };
    let verdict = read_with(sid, agent, group, kernel, places);
    tell(&ctx, sid, verdict);
    verdict.act
}

/// THE HOST'S ROSTER READ of `ctx`'s session `sid` ([`read_with`]), told ([`tell`]) once `holder`
/// holds its claim: the session log says the act as soon as the harness supervises the session,
/// not only at a write the gate weighs (an observed agent that never draws a box would otherwise
/// never say why nothing is typed). A session the host does not hold is not told, so its first
/// mark comes once the host claims it.
pub(crate) fn roster_read(ctx: &crate::SessionCtx, sid: &str, agent: Program, group: i32) {
    roster_read_with(
        ctx,
        sid,
        agent,
        group,
        &crate::harness_host::holder(),
        &Kernel::LIVE,
        &Where::live,
    );
}

/// [`roster_read`] through `kernel` and `places` ([`read_with`]), for `holder`.
pub(crate) fn roster_read_with(
    ctx: &crate::SessionCtx,
    sid: &str,
    agent: Program,
    group: i32,
    holder: &str,
    kernel: &Kernel,
    places: &dyn Fn() -> Where,
) {
    let verdict = read_with(sid, agent, group, kernel, places);
    let held = ctx
        .meta
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .live_supervisor(crate::metrics::now_us())
        == Some(holder);
    if held {
        tell(ctx, sid, verdict);
    }
}

/// The lanes whose write the gate withheld from each session, each said once ([`withheld`]):
/// `None` for a write that named no lane.
static WITHHELD: Mutex<BTreeMap<String, BTreeSet<Option<Lane>>>> = Mutex::new(BTreeMap::new());

/// THE ONE OBSERVE GATE, at the control server's input admission (`control::admit_with`), where
/// every write the window harness makes into a session goes through, whichever lane makes it
/// and whether or not that lane asked first: the refusal of a harness write into `ctx`'s
/// session that names `lane`, or `None` where it passes ([`agent_image::withholds`]). A
/// foreground that names no agent takes it (it goes into no agent: a shell, say, where a
/// relaunch types its line); an agent the host admits is judged by its kernel-confirmed act
/// ([`read_with`], kept per process), and one it does not admit (a Codex that is no codex-alab)
/// is observed. The first refusal of each lane in a session says so in `aterm.log`, words only,
/// and the session's next confirmation lets it be said again.
pub(crate) fn withheld(ctx: &crate::SessionCtx, lane: Option<Lane>) -> Option<String> {
    withheld_with(ctx, lane, &Kernel::LIVE, &Where::live)
}

/// [`withheld`] through `kernel` and `places` ([`read_with`]).
pub(crate) fn withheld_with(
    ctx: &crate::SessionCtx,
    lane: Option<Lane>,
    kernel: &Kernel,
    places: &dyn Fn() -> Where,
) -> Option<String> {
    let (named, agent, group) = {
        let timeline = ctx.timeline.lock().unwrap_or_else(PoisonError::into_inner);
        let publication = timeline.agent();
        (
            crate::harness_host::identified_agent(publication),
            crate::harness_host::agent_of(publication),
            publication.program_pgid,
        )
    };
    let sid = ctx.self_id.as_str();
    let act = match crate::harness_host::admitted_as(ctx, agent, group) {
        Some(agent) => Some(read_with(sid, agent, group, kernel, places).act),
        None => named.map(|_| Act::Observe),
    };
    if !agent_image::withholds(act, lane) {
        return None;
    }
    let news = WITHHELD
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .entry(sid.to_string())
        .or_default()
        .insert(lane);
    if news {
        aterm_log::info!(
            "harness @{sid}: {} write withheld: supervisor_act=observe why={}",
            lane.map_or("unnamed", Lane::word),
            why(Act::Observe)
        );
        #[cfg(test)]
        {
            *SAID
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .entry(sid.to_string())
                .or_default() += 1;
        }
    }
    Some(format!("{}\n", agent_image::OBSERVE_REFUSAL))
}

/// Tell `verdict` for `sid` once per move of its act: the session log's `meta field=supervisor
/// why=confirmed|unconfirmed` and an `aterm.log` line, words only.
fn tell(ctx: &crate::SessionCtx, sid: &str, verdict: Verdict) {
    if verdict.act == Act::On {
        WITHHELD
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .remove(sid);
    }
    if take_told(sid, verdict.act) {
        aterm_log::info!(
            "harness @{sid}: supervisor_act={} proof={}",
            verdict.act.word(),
            verdict.proof.map_or("-", Proof::word)
        );
        crate::session_timeline::record_supervisor_act(ctx, why(verdict.act));
    }
}

#[cfg(test)]
#[path = "agent_act_tests.rs"]
mod tests;
