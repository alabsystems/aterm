// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `codex-alab-proof`: the codex-alab native harness, end to end (its contract, `alab/CONTRACT.md` §9.3, and Autopilot Cruise / Land, the owner's its §5 ports), against the REAL package in a private headless aterm sandboxed to one loopback port where the lab answers as the model ([`crate::codex_rig`], [`crate::codex_alab_model`]): no key, no account, no byte off the machine, nothing of the owner's touched. `--upgrade-package` selects another package for the handoff; `--upgrade-only` runs only that lane.

use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use aterm_agent::harness::acts::RULE_UPGRADE;
use aterm_agent::supervise::policy::turn_end::{RULE_LEFT, says_done};
use aterm_uds::spawnfd::Stdio;

use crate::codex_alab_model::{
    self as model, KEEP_GOING, NEED, NUDGES_BEFORE_DEFENSE, PERSON_ASKS, REOPEN, REPO_COMMIT_HOLD,
    REPO_PUSH_HOLD, SLOW_TURN,
};
use crate::codex_rig;

#[path = "codex_alab_policy_proof.rs"]
mod policy_proof;
#[path = "codex_alab_upgrade_proof.rs"]
mod upgrade_proof;
use crate::forever::pct_decode;
use crate::instance::{self, Instance, Launch, World};
use crate::pyjson::J;
use crate::screen::{composer_row, field};
use crate::stop::{self, Stopped};
use crate::{Fail, LAB, Res};

/// The act codex-alab records at a landing (never typed).
const LANDED: &str = "landed@v1";

/// The act older codex-alab builds recorded where the retired token ended
/// the task: nothing records it now.
const FINISHED: &str = "finished@v1";

/// Cruise's defend-and-find-more prompt's act.
const DEFEND: &str = "continue-done-check@v1";

/// The fleet the lab's own bus names (`/f/lab/…`).
const FLEET: &str = "lab";

/// The one-line nudge the removed `aterm drive task` typed into a worker
/// (`DESIGN-sessions-as-subagents-2026-10-15.md` §7): nothing types it now,
/// so the Autopilot Off step holds the screen and the ledger to its absence.
const NUDGE: &str = "Inbox: task @";

/// The proof's own bus (the Autopilot Off step's mail): a guarded broker the
/// lab starts outside the jail on a Unix socket in the scratch, under a mint
/// secret only the scratch holds, and the node ring the instance's bridge
/// attaches with (THE ring `aterm fabric on` mints,
/// `aterm_link::enable::node_grants`).
struct Fabric {
    broker: Option<aterm_uds::spawnfd::Child>,
    /// The `[fabric]` table the instance's `aterm.toml` gets.
    table: String,
}

impl Fabric {
    /// Lay the bus out under `dir` and start its broker with `aterm`'s own
    /// `link broker`.
    fn start(aterm: &Path, dir: &Path) -> Result<Fabric, String> {
        use std::os::unix::fs::PermissionsExt as _;
        let state = dir.join("state");
        std::fs::create_dir_all(&state).map_err(|e| format!("{}: {e}", state.display()))?;
        for d in [dir, state.as_path()] {
            std::fs::set_permissions(d, std::fs::Permissions::from_mode(0o700))
                .map_err(|e| format!("{}: {e}", d.display()))?;
        }
        let mut random = [0u8; 40];
        {
            use std::io::Read as _;
            std::fs::File::open("/dev/urandom")
                .and_then(|mut f| f.read_exact(&mut random))
                .map_err(|e| format!("/dev/urandom: {e}"))?;
        }
        let secret = dir.join("mint.secret");
        let write_private = |path: &Path, bytes: &[u8]| {
            std::fs::write(path, bytes)
                .and_then(|()| {
                    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
                })
                .map_err(|e| format!("{}: {e}", path.display()))
        };
        write_private(&secret, &random[..32])?;
        let node = format!(
            "n-{}",
            random[32..]
                .iter()
                .map(|b| format!("{b:02x}"))
                .collect::<String>()
        );
        write_private(&state.join("node"), format!("{node}\n").as_bytes())?;
        let program = dir.join("aterm");
        std::os::unix::fs::symlink(aterm, &program)
            .map_err(|e| format!("{}: {e}", program.display()))?;
        let mut caps = String::new();
        for grant in aterm_link::enable::node_grants(FLEET, &node) {
            let out = aterm_uds::spawnfd::command(&program)
                .args(["link", "mint", &grant, "--secret-file"])
                .arg(&secret)
                .stdin(Stdio::null())
                .output()
                .map_err(|e| format!("aterm link mint: {e}"))?;
            if !out.status.success() {
                return Err(format!(
                    "aterm link mint {grant}: {}",
                    String::from_utf8_lossy(&out.stderr).trim()
                ));
            }
            caps.push_str(&String::from_utf8_lossy(&out.stdout));
        }
        let cap = dir.join("node.cap");
        write_private(&cap, caps.as_bytes())?;
        let sock = dir.join("b.sock");
        let log = std::fs::File::create(dir.join("broker.out"))
            .map_err(|e| format!("broker.out: {e}"))?;
        let err = log.try_clone().map_err(|e| format!("broker.out: {e}"))?;
        let broker = aterm_uds::spawnfd::command(&program)
            .args(["link", "broker"])
            .arg(&sock)
            .arg(dir.join("b.log"))
            .arg("--secret-file")
            .arg(&secret)
            .stdin(Stdio::null())
            .stdout(Stdio::from(log))
            .stderr(Stdio::from(err))
            .spawn()
            .map_err(|e| format!("aterm link broker: {e}"))?;
        let mut fabric = Fabric {
            broker: Some(broker),
            table: format!(
                "[fabric]\ncommand = \"{program} link serve --fleet {FLEET} --broker {sock} \
                 --cap-file {cap} --state {state} --accept-from {node}\"\n",
                program = program.display(),
                sock = sock.display(),
                cap = cap.display(),
                state = state.display(),
            ),
        };
        let end = Instant::now() + Duration::from_secs(60);
        while !sock.exists() {
            if Instant::now() >= end
                || fabric
                    .broker
                    .as_mut()
                    .is_some_and(|b| matches!(b.try_wait(), Ok(Some(_))))
            {
                fabric.stop();
                let said = std::fs::read_to_string(dir.join("broker.out")).unwrap_or_default();
                return Err(format!("the lab's broker never listened: {}", said.trim()));
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        Ok(fabric)
    }

    /// End the broker (`SIGTERM`, then `SIGKILL`): only the pid the lab started.
    fn stop(&mut self) {
        let Some(mut broker) = self.broker.take() else {
            return;
        };
        if !matches!(broker.try_wait(), Ok(None)) {
            return;
        }
        if let Ok(pid) = i32::try_from(broker.id()) {
            let _ = crate::sys::kill(pid, libc::SIGTERM);
            let end = Instant::now() + Duration::from_secs(10);
            loop {
                if !matches!(broker.try_wait(), Ok(None)) {
                    return;
                }
                if Instant::now() >= end {
                    break;
                }
                std::thread::sleep(Duration::from_millis(20));
            }
        }
        let _ = broker.kill();
        let _ = broker.wait();
    }
}

impl Drop for Fabric {
    fn drop(&mut self) {
        self.stop();
    }
}

/// A `codex-package.json` root that carries `alab-build.json`: the package `given` names (a package directory, or a binary in one), else `$LAB_CODEX`, else the owner's managed `codex` — the binary its atpkg shim execs (the last path of `aterm pkg which codex`), since a dev-linked package's shim is a script in `pkg/bin` that no package directory holds; the shim's own path when atpkg names none.
/// # Errors
/// Why none is a codex-alab package (the verb's NOT RUN).
pub fn package(given: Option<&str>, home: &str) -> Result<PathBuf, String> {
    let (from, why) = match given {
        Some(g) => (PathBuf::from(crate::paths::from_caller(g)), "--package"),
        None => match std::env::var("LAB_CODEX") {
            Ok(v) if !v.is_empty() => (PathBuf::from(v), "$LAB_CODEX"),
            _ => (
                crate::lab::managed_store_bin("codex").map_or_else(
                    || Path::new(home).join("Library/Application Support/aterm/pkg/bin/codex"),
                    PathBuf::from,
                ),
                "the managed codex",
            ),
        },
    };
    let real = std::fs::canonicalize(&from).map_err(|_| {
        format!(
            "no codex-alab package ({why}: {} is not there; pass --package <dir>)",
            from.display()
        )
    })?;
    let root = real
        .ancestors()
        .take(4)
        .find(|d| {
            d.join(aterm_agent::harness::codex_alab::PACKAGE_JSON)
                .is_file()
        })
        .ok_or_else(|| format!("{why} ({}) is in no Codex package", real.display()))?
        .to_path_buf();
    let text = std::fs::read_to_string(root.join(aterm_agent::harness::codex_alab::PACKAGE_JSON))
        .unwrap_or_default();
    aterm_agent::harness::codex_alab::package_entrypoint(&text)
        .ok_or_else(|| format!("{} holds no layout-1 package manifest", root.display()))?;
    if !root
        .join(aterm_agent::harness::codex_alab::ALAB_BUILD_JSON)
        .is_file()
    {
        return Err(format!(
            "{why} ({}) is a vendor Codex, not a codex-alab package (no alab-build.json)",
            root.display()
        ));
    }
    Ok(root)
}

/// The instance's sandbox: the rig's network rule ([`codex_rig::jail_policy`],
/// # Errors
/// No home to fence, or the proof's scratch `s` lies in one (the instance
/// could write nothing).
fn policy(port: u16, p: &crate::paths::Paths, s: &Path) -> Result<String, String> {
    let homes = p.need_guarded_homes()?;
    if let Some(h) = homes.iter().find(|h| s.starts_with(h.as_str())) {
        return Err(format!(
            "codex-alab-proof: its scratch {} lies in the owner's home {h}, which the sandbox fences",
            s.display()
        ));
    }
    Ok(format!(
        "{}{}",
        codex_rig::jail_policy(port),
        codex_rig::home_guards(&homes)?
    ))
}

/// One leg's tab and files.
struct Leg {
    name: &'static str,
    sid: String,
    /// Its `CODEX_HOME`.
    home: PathBuf,
    proj: PathBuf,
}

/// The folder a leg's codex starts in.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Folder {
    /// A fresh repository holding one untracked file (`git init`, a
    /// `README.md`), as every leg had before the Land legs.
    Scratch,
    /// No repository at all: Land's probe finds nothing.
    Plain,
    /// A repository whose `main` is pushed to a local bare `origin`
    /// (`origin/HEAD` naming it), then one uncommitted file (`notes.txt`).
    Origin,
}

/// The file the repository leg leaves uncommitted.
const NOTES: &str = "notes.txt";

/// Whether a `meta` reply's attention entries hold the worker's NEEDS YOU
/// (`attention_kinds=` naming `ask:needs`, pct-encoded or not).
fn needs_raised(meta: &str) -> bool {
    field(meta, "attention_kinds")
        .map(|k| pct_decode(&k))
        .is_some_and(|k| k.split(',').any(|e| e == "ask:needs"))
}

/// Whether a `log kinds=act` reply holds the act mark a self-claim's `act`
/// writes for `holder`: `<seq> mark <node> act
/// word=<w> rule=<act, @.> holder=<holder> harness_act=<act>`, the word
/// map's ([`aterm_agent::harness::acts::self_claim_act`]; `word=unknown`
/// and no `rule=` for an act it has no word for), its holder the proven
/// claim's, which the server read, and the act whole (host tier: the
/// owner's `log` prints both), as the timeline's `harness-act act=<act>
/// holder=<holder>` row says it.
fn acted_in_log(log: &str, act: &str, holder: &str) -> bool {
    let worded = aterm_agent::harness::acts::self_claim_act(act);
    let word = worded.as_ref().map_or("unknown", |(w, _)| *w);
    let rule = worded.as_ref().map(|(_, r)| r.as_str());
    log.lines()
        .filter(|l| l.starts_with(|c: char| c.is_ascii_digit()))
        .filter(|l| l.split(' ').nth(3) == Some("act"))
        .any(|l| {
            field(l, "word").as_deref() == Some(word)
                && field(l, "rule").as_deref() == rule
                && field(l, "holder").as_deref() == Some(holder)
                && field(l, "harness_act").as_deref() == Some(act)
        })
}

/// The task face a `status` line reads (`task=`), `-` where none is named.
fn task_face(status: &str) -> String {
    field(status, "task").unwrap_or_else(|| "-".to_string())
}

/// Where a Land leg stood just before the live upgrade
/// ([`Proof::landed_before_upgrade`]): its codex-alab's proven pid, its
/// record's `execGeneration` and the resumes its journal names, or what was
/// read when its face never read `landed` beside a proven claim.
type Baseline = Result<(i32, i64, usize), String>;

/// Each leg with what was read of it: `Err` with what was read when it never settled.
type LegReadings<'l, T> = Vec<(&'l Leg, Result<T, String>)>;

/// Each of `legs` with what `read` says of it, in order: none left out, so a
/// leg `read` could not settle is still judged (a FAIL), never dropped.
fn landed_baselines<'l, T>(
    legs: &[&'l Leg],
    mut read: impl FnMut(&Leg) -> Result<Result<T, String>, Stopped>,
) -> Result<LegReadings<'l, T>, Stopped> {
    legs.iter().map(|leg| Ok((*leg, read(leg)?))).collect()
}

/// What a session's codex-alab rollout tap last answered, as `status` says
/// it (`rollout=<word> rollout_asks=<n>`): the answer's word and how many
/// asks it answered.
#[derive(Clone, Debug, PartialEq, Eq)]
struct TapSaid {
    word: String,
    asks: u64,
}

/// The tap's answer a `status` line names; `None` where it names none.
fn tap_said(status: &str) -> Option<TapSaid> {
    Some(TapSaid {
        word: field(status, "rollout")?,
        asks: field(status, "rollout_asks")?.parse().ok()?,
    })
}

/// THE TAP'S FENCE: how many asks past a look the tap answers before every
/// write it would make about what the rollout held at that look is in the
/// log.
const FENCE_ASKS: u64 = 2;

/// What reading past the fence found ([`past_the_fence`]).
#[derive(Debug, PartialEq, Eq)]
enum Fenced<T> {
    /// The tap answered [`FENCE_ASKS`] asks past the first look: its answer
    /// at the first look, its answer then, and the read made then.
    Past { from: TapSaid, to: TapSaid, got: T },
    /// No `status` line named a tap answer.
    NoTap,
    /// The tap answered too few asks past the first look before the hang
    /// detector ran out: its answer at that look, and its last.
    Stuck { from: TapSaid, to: TapSaid },
}

/// READ PAST THE FENCE (pure over its hands): `look` at the tap's answer,
/// `wait` (a hang detector, never a latency budget) until it answered
/// [`FENCE_ASKS`] more, and only then `read`.
fn past_the_fence<T>(
    mut look: impl FnMut() -> Option<TapSaid>,
    read: impl FnOnce() -> T,
    wait: impl FnOnce(&mut dyn FnMut() -> bool) -> Result<bool, Stopped>,
) -> Result<Fenced<T>, Stopped> {
    let Some(from) = look() else {
        return Ok(Fenced::NoTap);
    };
    let mut to = from.clone();
    let past = wait(&mut || {
        if let Some(now) = look() {
            to = now;
        }
        to.asks >= from.asks.saturating_add(FENCE_ASKS)
    })?;
    if !past {
        return Ok(Fenced::Stuck { from, to });
    }
    Ok(Fenced::Past {
        from,
        to,
        got: read(),
    })
}

/// THE ROLLOUT LEG'S VERDICT (pure), on the log read past the fence: on a
/// measured build the tap said `bound` and the log holds a turn from the
/// rollout (`src=transcript`); on any other the tap said `unmeasured` and
/// the log holds none.
fn rollout_holds(measured: bool, said: &TapSaid, turns: &[String]) -> bool {
    let from_rollout = turns
        .iter()
        .filter(|l| l.contains(" src=transcript "))
        .count();
    if measured {
        said.word == "bound" && from_rollout > 0
    } else {
        said.word == "unmeasured" && from_rollout == 0
    }
}

/// The Land nudges a leg's model log names (`<leg>:nudge<n>[+…]`), in order.
fn nudges_seen(tags: &[String]) -> Vec<usize> {
    tags.iter()
        .filter_map(|t| {
            let (_, step) = t.split_once(':')?;
            let n = step.strip_prefix("nudge")?;
            n.split('+').next()?.parse().ok()
        })
        .collect()
}

/// The `land-nudge@v1` acts of a timeline's acts before its first
/// `landed@v1` (all of them where none landed).
fn nudges_before_landing(acts: &[String]) -> usize {
    acts.iter()
        .take_while(|a| *a != LANDED)
        .filter(|a| *a == "land-nudge@v1")
        .count()
}

/// The time (unix ms) of the first model request of `rows` whose tag starts
/// with `tag`.
fn first_at(rows: &[J], tag: &str) -> Option<i64> {
    rows.iter()
        .find(|r| {
            r.get("tag")
                .and_then(J::as_str)
                .is_some_and(|t| t.starts_with(tag))
        })
        .and_then(|r| r.get("t").and_then(J::as_i64))
}

/// `ms` as seconds with one decimal, for a leg's evidence line.
fn secs(ms: i64) -> String {
    format!("{:.1} s", ms as f64 / 1000.0)
}

/// The last `n` characters of `text`.
fn tail(text: &str, n: usize) -> String {
    let chars: Vec<char> = text.chars().collect();
    chars[chars.len().saturating_sub(n)..].iter().collect()
}

/// `d` in milliseconds.
fn ms_of(d: Duration) -> i64 {
    i64::try_from(d.as_millis()).unwrap_or(i64::MAX)
}

/// A model log row's string field `k` (empty where it has none).
fn str_of(row: &J, k: &str) -> String {
    row.get(k)
        .and_then(J::as_str)
        .unwrap_or_default()
        .to_string()
}

/// A model log row's integer field `k` (0 where it has none).
fn int_of(row: &J, k: &str) -> i64 {
    row.get(k).and_then(J::as_i64).unwrap_or(0)
}

/// codex-alab's `min_work`: a turn under it is short.
const MIN_WORK_MS: i64 = 120_000;

/// The hang detector past a done claim's back-off before the Cruise check
/// counts its act missing.
const CLAIM_SLACK_MS: i64 = 90_000;

/// How long a landed leg is watched for an act that must not come: past the
/// 120 s a codex-alab that went on would wait after one short defense.
const LANDED_QUIET: Duration = Duration::from_secs(150);

/// The back-off before the act after `short` short turns in a row: 2, 4, 8 …
/// minutes, an hour at most (codex-alab's `Timing`, aterm's `TurnEndTiming`).
fn backoff_ms(short: u32) -> i64 {
    if short == 0 {
        0
    } else {
        (1i64 << short.min(6)).min(60) * 60_000
    }
}

/// A Cruise leg's done claims, read from the model's log (Autopilot Cruise).
#[derive(Debug, Default, PartialEq, Eq)]
struct Claims {
    met: usize,
    pending: usize,
    unmet: Vec<String>,
}

/// Each reply of `rows` (one leg's model log, in order) that says done
/// (aterm's `says_done`) and what came next: the defend prompt
/// (`<leg>:defend…`) meets it; any other request, or none past the claim's
/// back-off (after `short` short replies in a row, a done claim short
/// whatever it worked and real work ending the run; a hang detector past
/// it), leaves it unmet; none before then is pending.
fn claims_of(leg: &str, rows: &[J], now_ms: i64) -> Claims {
    let defend = format!("{leg}:defend");
    let mut claims = Claims::default();
    let mut short = 0u32;
    for (i, row) in rows.iter().enumerate() {
        if int_of(row, "status") != 200 {
            continue;
        }
        let said = str_of(row, "said");
        let hold = int_of(row, "hold_ms");
        let done = says_done(Some(&said));
        short = if done || hold < MIN_WORK_MS {
            short.saturating_add(1)
        } else {
            0
        };
        if !done {
            continue;
        }
        let tag = str_of(row, "tag");
        let backoff = backoff_ms(short);
        let due = int_of(row, "t") + hold + backoff + CLAIM_SLACK_MS;
        match rows.get(i + 1) {
            Some(next) if str_of(next, "tag").starts_with(&defend) => claims.met += 1,
            Some(next) => claims
                .unmet
                .push(format!("{tag} then {}", str_of(next, "tag"))),
            None if now_ms > due => claims.unmet.push(format!(
                "{tag} then nothing past its {} s back-off",
                backoff / 1000
            )),
            None => claims.pending += 1,
        }
    }
    claims
}

/// How long past the reply to the person's own turn the exchange leg is
/// watched for an act that must not come: after a turn of real work
/// codex-alab types the next act at once (after `human_grace_s`), as the
/// control leg shows, so a minute is decisive; the judge holds it past the
/// control's measured latency too ([`left_to_person`]).
const EXCHANGE_QUIET: Duration = Duration::from_secs(60);

/// The hang detector past the control's reply for its continuation: typed at
/// once after a turn of real work, it is late past this.
const CONTROL_ACT_MS: i64 = 120_000;

/// The continued reply's token must not trigger another request in this
/// window, starting at its server-finished-writing witness.
const CONTINUE_QUIET: Duration = Duration::from_secs(15);

/// codex-alab's journal words for a point it leaves to a person (its
/// decider's note, `<thread>: nothing (a person's own exchange: theirs to
/// answer)`): the rule aterm journals as `left-to-person@v1` ([`RULE_LEFT`]).
const EXCHANGE_WORDS: &str = "a person's own exchange";

/// Attach only an unambiguous successful write witness for this exact
/// request.
fn with_replies(mut rows: Vec<J>, replies: &[J]) -> Vec<J> {
    for i in 0..rows.len() {
        let row = &rows[i];
        let Some(id) = row
            .get("request_id")
            .and_then(J::as_i64)
            .filter(|id| *id > 0)
        else {
            continue;
        };
        if rows
            .iter()
            .filter(|r| r.get("request_id").and_then(J::as_i64) == Some(id))
            .count()
            != 1
        {
            continue;
        }
        let Some(at) = row.get("mono_ms").and_then(J::as_i64).filter(|at| *at >= 0) else {
            continue;
        };
        let matching: Vec<&J> = replies
            .iter()
            .filter(|r| {
                str_of(r, "event") == "response_written"
                    && r.get("request_id").and_then(J::as_i64) == Some(id)
            })
            .collect();
        let [reply] = matching.as_slice() else {
            continue;
        };
        if reply.get("status") != row.get("status") {
            continue;
        }
        let Some(written) = reply
            .get("mono_ms")
            .and_then(J::as_i64)
            .filter(|t| *t >= at)
        else {
            continue;
        };
        if let J::Obj(fields) = &mut rows[i] {
            fields.push(("reply_ms".into(), J::Int(written)));
        }
    }
    rows
}

/// The reply to a leg's typed turn (`<leg>:person`): its successful HTTP
/// write witness on the run's monotonic clock, and subsequent requests.
fn person_reply<'a>(leg: &str, rows: &'a [J]) -> Option<(i64, &'a [J])> {
    let head = format!("{leg}:person");
    let at = rows
        .iter()
        .position(|r| str_of(r, "tag").starts_with(&head))?;
    let row = &rows[at];
    let reply = written_reply(row)?;
    Some((reply, &rows[at + 1..]))
}

fn written_reply(row: &J) -> Option<i64> {
    let at = row.get("mono_ms")?.as_i64()?;
    let written = row.get("reply_ms")?.as_i64()?;
    (int_of(row, "status") == 200 && at >= 0 && written >= at).then_some(written)
}

/// A matched successful server-write witness, read on the same monotonic
/// clock.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct ReplyWrite {
    request_id: i64,
    written_ms: i64,
}

fn observe_reply(rows: &[J], head: &str, observed_ms: i64) -> Option<ReplyWrite> {
    let row = rows.iter().find(|r| str_of(r, "tag").starts_with(head))?;
    let written_ms = written_reply(row)?;
    let request_id = row.get("request_id")?.as_i64()?;
    (request_id > 0 && observed_ms >= written_ms).then_some(ReplyWrite {
        request_id,
        written_ms,
    })
}

fn continued_quiet(rows: &[J], reply: Option<ReplyWrite>, now_ms: i64) -> Result<(), String> {
    let reply = reply
        .filter(|r| observe_reply(rows, "continue:continue", now_ms) == Some(*r))
        .ok_or_else(|| "the continued reply has no observed successful write".to_string())?;
    let until = reply
        .written_ms
        .checked_add(ms_of(CONTINUE_QUIET))
        .ok_or_else(|| "the continued reply's quiet window overflowed".to_string())?;
    if now_ms < until {
        return Err("the continued reply's quiet window has not finished".to_string());
    }
    let mut tags = Vec::new();
    for row in rows {
        let at = row
            .get("mono_ms")
            .and_then(J::as_i64)
            .filter(|t| *t >= 0)
            .ok_or_else(|| "a model request has no readable monotonic time".to_string())?;
        if at <= until {
            tags.push(str_of(row, "tag"));
        }
    }
    if tags != ["continue:person", "continue:continue"] {
        return Err(format!("the model asked: {}", tags.join(" ")));
    }
    Ok(())
}

/// THE CONTROL'S VERDICT: its turn, typed as a control write, is codex-alab's
/// to go on with — the request after its reply is Cruise's continuation
/// (`<leg>:continue`), asked within [`CONTROL_ACT_MS`] of the reply, and the
/// first act on its timeline is `continue@v1`.
fn continued_after(leg: &str, rows: &[J], acts: &[String]) -> Result<i64, String> {
    let Some((reply, after)) = person_reply(leg, rows) else {
        return Err("its turn never reached the model".to_string());
    };
    let Some(next) = after.first() else {
        return Err("nothing asked after its reply".to_string());
    };
    let tag = str_of(next, "tag");
    let late = next
        .get("mono_ms")
        .and_then(J::as_i64)
        .and_then(|at| at.checked_sub(reply))
        .filter(|late| *late >= 0)
        .ok_or_else(|| "the continuation has no time after the witnessed reply".to_string())?;
    if !tag.starts_with(&format!("{leg}:continue")) {
        return Err(format!(
            "the request after its reply was {tag}, no continuation"
        ));
    }
    if late > CONTROL_ACT_MS {
        return Err(format!(
            "the continuation came {} after its reply, past {} s",
            secs(late),
            CONTROL_ACT_MS / 1000
        ));
    }
    if acts.first().map(String::as_str) != Some("continue@v1") {
        return Err(format!("its acts are {acts:?}, not continue@v1 first"));
    }
    Ok(late)
}

/// The acts of a leg's timeline that typed into its session: all of them but
/// the two codex-alab records with `meta set harness_act` and never types —
/// the point it left to a person's own exchange (`left-to-person@v1`,
/// [`RULE_LEFT`]), so aterm's face reads `theirs`, and its live upgrade's
/// re-exec onto a new build at rest (`upgrade@v1`, [`RULE_UPGRADE`]).
fn typed_acts(acts: &[String]) -> Vec<String> {
    acts.iter()
        .filter(|a| *a != RULE_LEFT && *a != RULE_UPGRADE)
        .cloned()
        .collect()
}

/// How many times a leg's codex-alab journal says a thread was resumed
/// (`<thread>: resumed (task=…)`): once per image that took a conversation
/// up again, a live upgrade's re-exec among them.
fn resumes(journal: &str) -> usize {
    journal
        .lines()
        .filter_map(|l| l.split_once("codex_alab_autopilot: "))
        .filter(|(_, rest)| rest.contains(": resumed (task="))
        .count()
}

/// THE FIRST DECISION AFTER A RESUME: the first line of a leg's codex-alab
/// journal after its resume number `n + 1` ([`resumes`] counted `n` before
/// it) that says what the image did at the point — `nothing (…)`, a `wait`
/// for an act, or an act — from its target on, on one line.
fn decided_after_resume(journal: &str, n: usize) -> Option<String> {
    let mut seen = 0;
    for line in journal.lines() {
        let Some((_, rest)) = line.split_once("codex_alab_autopilot: ") else {
            continue;
        };
        if seen <= n {
            if rest.contains(": resumed (task=") {
                seen += 1;
            }
            continue;
        }
        let what = rest.split_once(": ").map_or(rest, |(_, what)| what);
        if rest.starts_with("act ") || what.starts_with("nothing (") || what.starts_with("wait ") {
            return Some(crate::util::one_line(rest, 160));
        }
    }
    None
}

/// THE PERSON'S EXCHANGE, LEFT TO THEM: nothing asked of the model after the
/// reply to their turn and no typed act on the leg's timeline
/// ([`typed_acts`]), watched at least
/// [`EXCHANGE_QUIET`] past the reply and longer than the control's
/// continuation took after its own (`control_ms`), so the same path showed
/// its act inside a window this one outlasted.
fn left_to_person(
    leg: &str,
    rows: &[J],
    acts: &[String],
    observed: Option<ReplyWrite>,
    now_ms: i64,
    control_ms: Option<i64>,
) -> Result<String, String> {
    let Some((_, after)) = person_reply(leg, rows) else {
        return Err("the person's turn has no successful reply write".to_string());
    };
    let head = format!("{leg}:person");
    let observed = observed
        .filter(|r| observe_reply(rows, &head, now_ms) == Some(*r))
        .ok_or_else(|| "the person's reply has no local observation".to_string())?;
    let acts = typed_acts(acts);
    if !after.is_empty() || !acts.is_empty() {
        let asked: Vec<String> = after.iter().map(|r| str_of(r, "tag")).collect();
        return Err(format!(
            "TYPED into the person's exchange: the model asked {asked:?} after the reply, acts {acts:?}"
        ));
    }
    let Some(control) = control_ms else {
        return Err("the control got no continuation, so a quiet here proves nothing".to_string());
    };
    let watched = now_ms.checked_sub(observed.written_ms).unwrap_or(-1);
    if watched < ms_of(EXCHANGE_QUIET) || watched <= control {
        return Err(format!(
            "watched only {} since the successful server write (at least {} s, and past the control's {})",
            secs(watched),
            EXCHANGE_QUIET.as_secs(),
            secs(control)
        ));
    }
    Ok(format!(
        "nothing asked and no act for {} since the successful server write; the control's continuation came {} after its own",
        secs(watched),
        secs(control)
    ))
}

/// The line of a leg's codex-alab journal naming the rule it left a point to
/// a person by — codex-alab's words ([`EXCHANGE_WORDS`]) or aterm's id
/// ([`RULE_LEFT`]) — from its target on, on one line.
fn exchange_rule(journal: &str) -> Option<String> {
    let line = journal
        .lines()
        .find(|l| l.contains(EXCHANGE_WORDS) || l.contains(RULE_LEFT))?;
    let from = line
        .split_once("codex_alab_autopilot: ")
        .map_or(line, |(_, rest)| rest);
    Some(crate::util::one_line(from, 160))
}

/// The acts a leg's codex-alab journal says it typed (`act <rule>: typed …`),
/// in order.
fn journal_typed(journal: &str) -> Vec<String> {
    journal
        .lines()
        .filter_map(|l| {
            l.match_indices("act ").find_map(|(i, _)| {
                if l[..i].ends_with(char::is_alphanumeric) {
                    return None;
                }
                let (rule, words) = l[i + 4..].split_once(": ")?;
                (rule.contains('@') && !rule.contains(' ') && words.starts_with("typed"))
                    .then(|| rule.to_string())
            })
        })
        .collect()
}

/// A codex-alab record's `execGeneration`: how many re-execs its process has
/// made (0 before the first).
fn exec_generation(record: &J) -> i64 {
    record
        .get("execGeneration")
        .and_then(J::as_i64)
        .unwrap_or(0)
}

/// A person's gesture as a `status` line names it: how long ago
/// (`human_ms=`, `None` for `-`: none since the session started) and how
/// many (`human_seq=`, 0 before the first).
fn person_hand(status: &str) -> (Option<u64>, u64) {
    (
        field(status, "human_ms").and_then(|v| v.parse().ok()),
        field(status, "human_seq")
            .and_then(|v| v.parse().ok())
            .unwrap_or(0),
    )
}

/// What the lab typed into an exchange leg's codex, and what aterm said of
/// it around the turn.
struct Typed {
    /// When the Enter that took the draft went in (unix ms).
    pressed_ms: i64,
    /// `status` read before the message was typed, once the draft stood in
    /// the composer (before its Enter), and after its Enter.
    before: String,
    sent: String,
    after: String,
}

impl Typed {
    /// THE ENTER STAMPED A PERSON'S GESTURE: `human_ms` set and `human_seq`
    /// past its count once the draft stood ([`Self::sent`]).
    fn enter_stamped(&self) -> (bool, [u64; 3]) {
        let (_, before) = person_hand(&self.before);
        let (_, sent) = person_hand(&self.sent);
        let (ms, after) = person_hand(&self.after);
        (ms.is_some() && after > sent, [before, sent, after])
    }

    /// THE WRITE STAMPED NO GESTURE: a control write's message and Enter,
    /// `human_ms` unset and `human_seq` where it stood before the message.
    fn stamped_nothing(&self) -> bool {
        let (_, was) = person_hand(&self.before);
        let (ms, seq) = person_hand(&self.after);
        ms.is_none() && seq == was
    }
}

struct Proof {
    inst: Instance,
    /// The scratch root and HOME.
    s: PathBuf,
    h: PathBuf,
    world_env: Vec<(String, String)>,
    api_log: Arc<std::sync::Mutex<codex_rig::ApiLog>>,
    /// The fake API and observer share this clock, outside every re-exec.
    api_clock: Instant,
    port: u16,
    fails: Vec<String>,
    /// The bus the Autopilot Off step's mail rides.
    fabric: Fabric,
}

fn sleep(d: Duration) -> Result<(), Stopped> {
    stop::sleep(d)
}

impl Proof {
    fn check(&mut self, label: &str, held: bool) -> bool {
        if held {
            println!("  ok   {label}");
        } else {
            println!("  FAIL {label}");
            self.fails.push(label.to_string());
        }
        held
    }

    fn on(&self, sid: &str, args: &[&str]) -> instance::Reply {
        let at = format!("@{sid}");
        let mut v = vec![at.as_str()];
        v.extend_from_slice(args);
        self.inst.ctl(&v)
    }

    fn status(&self, sid: &str) -> String {
        self.on(sid, &["status"]).first()
    }

    /// `supervisor=codex-alab@<pid> supervisor_proven=1`: that pid.
    fn proven_pid(&self, sid: &str) -> Option<i32> {
        let st = self.status(sid);
        if field(&st, "supervisor_proven").as_deref() != Some("1") {
            return None;
        }
        field(&st, "supervisor")?
            .strip_prefix("codex-alab@")?
            .parse()
            .ok()
    }

    /// The `harness-act` acts of the session's timeline, in order.
    fn acts(&self, sid: &str) -> Vec<String> {
        self.on(sid, &["timeline"])
            .stdout
            .lines()
            .filter(|l| l.contains("kind=harness-act"))
            .filter_map(|l| field(l, "act"))
            .collect()
    }

    /// The fake model's log rows for `leg` (`t`, `tag`, `u`, `tool_output`, `status`).
    fn requests(&self, leg: &str) -> Vec<J> {
        let head = format!("{leg}:");
        let Some((rows, replies)) = self.api_log.lock().ok().and_then(|l| l.snapshot().ok()) else {
            return Vec::new();
        };
        with_replies(rows, &replies)
            .into_iter()
            .filter(|v| {
                v.get("tag")
                    .and_then(J::as_str)
                    .is_some_and(|t| t.starts_with(&head) && !t.ends_with(":side"))
            })
            .collect()
    }

    fn tags(&self, leg: &str) -> Vec<String> {
        self.requests(leg)
            .iter()
            .filter_map(|v| v.get("tag").and_then(J::as_str).map(str::to_string))
            .collect()
    }

    /// The record codex-alab keeps for `pid` under the leg's `CODEX_HOME`.
    fn record(leg: &Leg, pid: i32) -> Option<J> {
        let path = leg.home.join(format!("alab/sessions/{pid}.json"));
        J::parse(&std::fs::read_to_string(path).ok()?).ok()
    }

    fn aterm_log(&self) -> String {
        let bytes = std::fs::read(self.h.join("Library/Logs/aterm/aterm.log")).unwrap_or_default();
        String::from_utf8_lossy(&bytes).into_owned()
    }

    /// The host's log lines for one tab.
    fn host_lines(&self, sid: &str) -> Vec<String> {
        let at = format!("@{sid}:");
        self.aterm_log()
            .lines()
            .filter(|l| l.contains(&at))
            .map(str::to_string)
            .collect()
    }

    /// Every `step` until `ok` holds, at most `limit`: a hang detector, never
    /// a latency budget.
    fn wait_for(
        limit: Duration,
        step: Duration,
        mut ok: impl FnMut() -> bool,
    ) -> Result<bool, Stopped> {
        let end = Instant::now() + limit;
        loop {
            if ok() {
                return Ok(true);
            }
            if Instant::now() >= end {
                return Ok(false);
            }
            sleep(step)?;
        }
    }

    /// `git <args>` in `dir`, as the lab's own hand (outside the jail): the
    /// scratch HOME, a lab identity, no signing.
    fn git(&self, dir: &Path, args: &[&str]) -> Result<String, String> {
        let out = aterm_uds::spawnfd::command("/usr/bin/git")
            .env_clear()
            .envs(self.world_env.iter().map(|(k, v)| (k.as_str(), v.as_str())))
            .args([
                "-c",
                "user.name=aterm-lab",
                "-c",
                "user.email=lab@aterm.invalid",
                "-c",
                "commit.gpgsign=false",
                "-c",
                "init.defaultBranch=main",
            ])
            .args(args)
            .current_dir(dir)
            .stdin(Stdio::null())
            .output()
            .map_err(|e| format!("git {}: {e}", args.join(" ")))?;
        if out.status.success() {
            Ok(String::from_utf8_lossy(&out.stdout).into_owned())
        } else {
            Err(format!(
                "git {}: {}",
                args.join(" "),
                String::from_utf8_lossy(&out.stderr).trim()
            ))
        }
    }

    /// Lay out the leg's folder as `folder` says (its README written).
    fn lay_folder(&self, dir: &Path, proj: &Path, folder: Folder) -> Result<(), String> {
        let readme = proj.join("README.md");
        std::fs::write(&readme, "# lab scratch project\n")
            .map_err(|e| format!("{}: {e}", readme.display()))?;
        match folder {
            Folder::Plain => Ok(()),
            Folder::Scratch => self.git(proj, &["init", "-q"]).map(drop),
            Folder::Origin => {
                let origin = dir.join("origin.git");
                let origin_s = origin.to_string_lossy().into_owned();
                self.git(proj, &["init", "-q", "-b", "main"])?;
                self.git(proj, &["add", "README.md"])?;
                self.git(proj, &["commit", "-q", "-m", "lab: the scratch project"])?;
                self.git(dir, &["init", "-q", "--bare", "-b", "main", &origin_s])?;
                self.git(proj, &["remote", "add", "origin", &origin_s])?;
                self.git(proj, &["push", "-q", "-u", "origin", "main"])?;
                self.git(proj, &["remote", "set-head", "origin", "main"])?;
                let notes = proj.join(NOTES);
                std::fs::write(&notes, "# release notes\n\n- the lab's change\n")
                    .map_err(|e| format!("{}: {e}", notes.display()))
            }
        }
    }

    /// The leg's world (its `CODEX_HOME` and its folder, laid out as `folder`
    /// says), its tab, its Autopilot word where one is given (set on the tab
    /// before codex starts there, as the chip's write), and the launch line
    /// typed at the tab's shell as a person types it, its prompt naming the
    /// leg.
    fn spawn_leg(
        &mut self,
        name: &'static str,
        prompt: &str,
        extra: &str,
        folder: Folder,
        autopilot: Option<&str>,
    ) -> Result<Leg, Fail> {
        self.start_leg(name, Some(prompt), extra, folder, autopilot)
    }

    /// [`Self::spawn_leg`], or with no `prompt` a `codex` started bare, at its
    /// composer, where the leg's turn is typed later ([`Self::type_exchange`]).
    fn start_leg(
        &mut self,
        name: &'static str,
        prompt: Option<&str>,
        extra: &str,
        folder: Folder,
        autopilot: Option<&str>,
    ) -> Result<Leg, Fail> {
        let dir = self.s.join("legs").join(name);
        let home = dir.join("codex-home");
        let proj = dir.join("proj");
        let made = (|| -> Result<PathBuf, String> {
            let io = |e: std::io::Error| e.to_string();
            std::fs::create_dir_all(&home).map_err(io)?;
            std::fs::create_dir_all(&proj).map_err(io)?;
            let proj = std::fs::canonicalize(&proj).map_err(io)?;
            let dir = std::fs::canonicalize(&dir).map_err(io)?;
            self.lay_folder(&dir, &proj, folder)?;
            std::fs::write(
                home.join("config.toml"),
                codex_config(self.port, &proj, &home, extra),
            )
            .map_err(io)?;
            Ok(proj)
        })();
        let proj = made.map_err(|e| format!("cannot lay out leg {name}: {e}"))?;
        let spawned = self
            .inst
            .ctl(&["spawn", &format!("cwd={}", proj.display())]);
        let sid = spawned
            .first()
            .strip_prefix("OK ")
            .and_then(|r| r.split_whitespace().next())
            .map(str::to_string)
            .ok_or_else(|| format!("spawn for leg {name}: {}", spawned.first()))?;
        let _ = self.on(&sid, &["await", "idle", "400", "timeout=30000"]);
        if let Some(word) = autopilot {
            let set = self.on(&sid, &["meta", "set", "autopilot", word]);
            let now = field(&self.on(&sid, &["meta"]).first(), "autopilot").unwrap_or_default();
            self.check(
                &format!(
                    "{name}: Autopilot {word} set on its tab before codex starts ({}; meta autopilot={now})",
                    set.first()
                ),
                set.code == 0 && now == word,
            );
        }
        let codex = prompt.map_or_else(
            || "codex".to_string(),
            |p| format!("codex {}", sh_quote(&format!("LEG-{name} {p}"))),
        );
        let line = format!(
            "export CODEX_HOME={}; cd {} && {codex}\r",
            sh_quote(&home.to_string_lossy()),
            sh_quote(&proj.to_string_lossy()),
        );
        let sent = self
            .inst
            .ctl_stdin(&[&format!("@{sid}"), "send", "--stdin"], line.as_bytes());
        if sent.code != 0 {
            return Err(Fail::die(format!(
                "leg {name}: the launch line was not typed: {}",
                sent.first()
            )));
        }
        println!("  leg {name}: tab {sid}");
        Ok(Leg {
            name,
            sid,
            home,
            proj,
        })
    }

    /// `aterm pkg link codex <pkg> bin/codex` under the scratch HOME.
    fn link(&self, pkg: &Path) -> Result<(), String> {
        let out = aterm_uds::spawnfd::command(&self.inst.aterm)
            .env_clear()
            .envs(self.world_env.iter().map(|(k, v)| (k.as_str(), v.as_str())))
            .args(["pkg", "link", "codex"])
            .arg(pkg)
            .arg("bin/codex")
            .stdin(Stdio::null())
            .output()
            .map_err(|e| format!("aterm pkg link: {e}"))?;
        if out.status.success() {
            Ok(())
        } else {
            Err(format!(
                "aterm pkg link codex {} bin/codex: {}{}",
                pkg.display(),
                String::from_utf8_lossy(&out.stdout),
                String::from_utf8_lossy(&out.stderr)
            ))
        }
    }

    /// The claim of `leg` proven, and the host watching only beside it.
    fn claim(&mut self, leg: &Leg) -> Result<Option<i32>, Stopped> {
        self.claim_with_limit(leg, Duration::from_secs(180))
    }

    fn claim_with_limit(&mut self, leg: &Leg, limit: Duration) -> Result<Option<i32>, Stopped> {
        let mut pid = None;
        Self::wait_for(limit, Duration::from_millis(500), || {
            pid = self.proven_pid(&leg.sid);
            pid.is_some()
        })?;
        let name = leg.name;
        self.check(
            &format!(
                "{name}: the claim is proven (supervisor=codex-alab@<pid> supervisor_proven=1)"
            ),
            pid.is_some(),
        );
        Ok(pid)
    }

    /// `act` on the leg's timeline within `limit`.
    fn await_act(&self, leg: &Leg, act: &str, limit: Duration) -> Result<bool, Stopped> {
        Self::wait_for(limit, Duration::from_secs(1), || {
            self.acts(&leg.sid).iter().any(|a| a == act)
        })
    }

    /// Every act of the leg's timeline that is one of `acts`.
    fn has_any(&self, leg: &Leg, acts: &[&str]) -> Vec<String> {
        self.acts(&leg.sid)
            .into_iter()
            .filter(|a| acts.contains(&a.as_str()))
            .collect()
    }

    /// The model's first request of `leg` tagged `<leg>:<step>…` within
    /// `limit`: a hang detector, never a budget.
    fn await_tag(&self, leg: &Leg, step: &str, limit: Duration) -> Result<bool, Stopped> {
        let head = format!("{}:{step}", leg.name);
        Self::wait_for(limit, Duration::from_millis(250), || {
            self.tags(leg.name).iter().any(|t| t.starts_with(&head))
        })
    }

    /// Read a successful write of this request's whole response.
    fn await_reply(
        &self,
        leg: &Leg,
        step: &str,
        limit: Duration,
    ) -> Result<Option<ReplyWrite>, Stopped> {
        let head = format!("{}:{step}", leg.name);
        let mut reply = None;
        Self::wait_for(limit, Duration::from_millis(250), || {
            let rows = self.requests(leg.name);
            reply = observe_reply(&rows, &head, codex_rig::monotonic_ms(self.api_clock));
            reply.is_some()
        })?;
        Ok(reply)
    }

    fn sleep_past_reply(&self, reply: ReplyWrite, window: Duration) -> Result<(), Stopped> {
        let Some(until) = reply.written_ms.checked_add(ms_of(window)) else {
            // the judge refuses the unrepresentable window
            return Ok(());
        };
        while codex_rig::monotonic_ms(self.api_clock) < until {
            sleep(Duration::from_millis(250))?;
        }
        Ok(())
    }

    /// [`Self::await_tag`] with its hang detector anchored at unix ms `by_ms`
    /// (read off the run's own clock marks, never when the lab looked): no
    /// wait at all once it has passed.
    fn tag_by(&self, leg: &Leg, step: &str, by_ms: i64) -> Result<bool, Stopped> {
        let head = format!("{}:{step}", leg.name);
        loop {
            if self.tags(leg.name).iter().any(|t| t.starts_with(&head)) {
                return Ok(true);
            }
            if crate::util::now_ms() >= by_ms {
                return Ok(false);
            }
            sleep(Duration::from_millis(250))?;
        }
    }

    /// Autopilot Cruise on the instance's first session (module header).
    fn first_leg(&mut self, first: &Leg) -> Result<(), Stopped> {
        let defended = self.await_act(first, DEFEND, Duration::from_secs(240))?;
        let parked = self
            .host_lines(&first.sid)
            .iter()
            .any(|l| l.contains("WATCHING a proven self-claim supervises this session"));
        self.check("first: aterm's lane parked (its host watches only)", parked);
        let ended = self.has_any(first, &[FINISHED, LANDED]);
        self.check(
            &format!(
                "first: the reply ending on the retired token alone is a done claim: Cruise's defend-and-find-more prompt ({DEFEND}), never {FINISHED} nor {LANDED} (ends: {ended:?})"
            ),
            defended && ended.is_empty(),
        );
        let asked = self.await_tag(first, "defend", Duration::from_secs(60))?;
        let rows = self.requests("first");
        if let (Some(t1), Some(t2)) = (
            first_at(&rows, "first:person"),
            first_at(&rows, "first:defend"),
        ) {
            println!(
                "    first: the defend prompt came {} after the reply (the back-off after one short turn: 2 min)",
                secs(t2 - t1)
            );
        }
        self.check(
            "first: the session keeps going: the model's next turn is asked with aterm's defend words (DONE_CHECK)",
            asked,
        );
        sleep(Duration::from_secs(15))?;
        let ended = self.has_any(first, &[FINISHED, LANDED]);
        let face = task_face(&self.status(&first.sid));
        self.check(
            &format!(
                "first: a clean `Nothing left:` defense under Cruise lands nothing (15 s on: ends {ended:?}, status task={face})"
            ),
            asked && ended.is_empty() && face != "landed",
        );
        Ok(())
    }

    /// NEEDS YOU, loud and never blocking (module header).
    fn needs_leg(&mut self, leg: &Leg) -> Result<(), Stopped> {
        let reported = self.await_tag(leg, "person", Duration::from_secs(120))?;
        let mut meta = String::new();
        let mut status = String::new();
        let raised = Self::wait_for(Duration::from_secs(60), Duration::from_millis(250), || {
            meta = self.on(&leg.sid, &["meta"]).first();
            status = self.status(&leg.sid);
            needs_raised(&meta) && field(&status, "needs").as_deref() == Some("ask")
        })?;
        let seen_ms = crate::util::now_ms();
        let rows = self.requests("needs");
        let after = first_at(&rows, "needs:person").map_or_else(String::new, |t1| {
            format!(
                "; seen {} after the report was asked for",
                secs(seen_ms - t1)
            )
        });
        let text = pct_decode(&field(&meta, "attention").unwrap_or_default());
        self.check(
            &format!(
                "needs: the report's NEEDS YOU line raises attention owner=needs kind=ask at once (attention_kinds={} status needs={} attention={text:?}{after})",
                pct_decode(&field(&meta, "attention_kinds").unwrap_or_default()),
                field(&status, "needs").unwrap_or_default()
            ),
            reported && raised && text.contains(NEED),
        );
        let mut stood_after_act = false;
        let acked = Self::wait_for(Duration::from_secs(240), Duration::from_millis(250), || {
            let rows = self.requests("needs");
            let Some(at) = first_at(&rows, "needs:continue+ack") else {
                return false;
            };
            let up = needs_raised(&self.on(&leg.sid, &["meta"]).first());
            stood_after_act |= up && crate::util::now_ms() > at;
            stood_after_act || crate::util::now_ms() > at + 5_000
        })?;
        let acts = self.acts(&leg.sid);
        self.check(
            &format!(
                "needs: never blocked: the next act is still typed (continue@v1, carrying aterm's NEEDS YOU ack) while the need stands (stood after the act: {stood_after_act}; acts: {})",
                acts.join(" ")
            ),
            acked && stood_after_act && acts.iter().any(|a| a == "continue@v1"),
        );
        let replied = first_at(&self.requests("needs"), "needs:continue+ack")
            .map(|at| at + ms_of(model::NEEDS_HOLD));
        if let Some(at) = replied {
            while crate::util::now_ms() < at + 10_000 {
                sleep(Duration::from_millis(250))?;
            }
        }
        let meta = self.on(&leg.sid, &["meta"]).first();
        self.check(
            &format!(
                "needs: the report after it, which names no need at all, leaves the need up (10 s past it: attention_kinds={})",
                pct_decode(&field(&meta, "attention_kinds").unwrap_or_default())
            ),
            acked && replied.is_some() && needs_raised(&meta),
        );
        Ok(())
    }

    /// The NEEDS YOU leg's last report, `NEEDS YOU: nothing`, after the plain
    /// one (its act backed off four minutes): it takes the need down.
    fn needs_cleared(&mut self, leg: &Leg) -> Result<(), Stopped> {
        let reported = Self::wait_for(Duration::from_secs(420), Duration::from_secs(1), || {
            self.requests("needs")
                .iter()
                .any(|r| str_of(r, "said").contains("NEEDS YOU: nothing"))
        })?;
        let mut meta = String::new();
        let cleared = Self::wait_for(Duration::from_secs(60), Duration::from_millis(250), || {
            meta = self.on(&leg.sid, &["meta"]).first();
            !needs_raised(&meta)
        })?;
        self.check(
            &format!(
                "needs: the report after that, `NEEDS YOU: nothing`, takes it down (reported: {reported}; attention_kinds={})",
                pct_decode(&field(&meta, "attention_kinds").unwrap_or_default())
            ),
            reported && cleared,
        );
        Ok(())
    }

    fn continue_leg(&mut self, leg: &Leg) -> Result<(), Stopped> {
        let continued = self.await_act(leg, "continue@v1", Duration::from_secs(240))?;
        self.check(
            "continue: a turn without a done claim is continued (continue@v1)",
            continued,
        );
        let replied = self.await_tag(leg, "continue", Duration::from_secs(60))?;
        let keep = self
            .requests("continue")
            .into_iter()
            .filter(|r| {
                r.get("u")
                    .and_then(J::as_str)
                    .is_some_and(|u| u.contains(KEEP_GOING))
            })
            .count();
        self.check(
            &format!(
                "continue: the continuation sent aterm's Cruise words (`keep going`) once (x{keep})"
            ),
            replied && keep == 1,
        );
        let reply = self.await_reply(leg, "continue", Duration::from_secs(60))?;
        if let Some(reply) = reply {
            self.sleep_past_reply(reply, CONTINUE_QUIET)?;
        }
        let ended = self.has_any(leg, &[FINISHED, LANDED]);
        let rows = self.requests("continue");
        let quiet = continued_quiet(&rows, reply, codex_rig::monotonic_ms(self.api_clock));
        let tags: Vec<String> = rows.iter().map(|r| str_of(r, "tag")).collect();
        self.check(
            &format!(
                "continue: the continued turn's token ends nothing (ends {ended:?}; the model asked: {}; quiet window: {})",
                tags.join(" "),
                quiet.as_ref().map_or_else(Clone::clone, |()| "quiet".to_string())
            ),
            replied && ended.is_empty() && quiet.is_ok(),
        );
        Ok(())
    }

    fn approval_leg(&mut self, leg: &Leg) -> Result<(), Stopped> {
        let ran_back = self.await_tag(leg, "after-output", Duration::from_secs(180))?;
        let acts = self.acts(&leg.sid);
        self.check(
            "approval: the escalated exec is approved once (approve-all@v1)",
            acts.iter().filter(|a| *a == "approve-all@v1").count() == 1,
        );
        let ran = std::fs::read_to_string(leg.proj.join("ran.txt")).unwrap_or_default();
        self.check(
            "approval: the approved command ran (ran.txt) and its output went back to the model",
            ran.contains("approved-run") && ran_back,
        );
        Ok(())
    }

    /// The question leg under Land (module header).
    fn question_leg(&mut self, leg: &Leg) -> Result<(), Stopped> {
        let answered = Self::wait_for(Duration::from_secs(180), Duration::from_secs(1), || {
            self.requests("question").iter().any(|r| {
                r.get("tool_output")
                    .and_then(J::as_str)
                    .is_some_and(|o| o.contains("Postgres (Recommended)"))
            })
        })?;
        let acts = self.acts(&leg.sid);
        self.check(
            "question: the overlay is answered with its (Recommended) option (answer-recommended@v1: codex-alab's box answer under every word)",
            answered && acts.iter().any(|a| a == "answer-recommended@v1"),
        );
        let land_answer = self.await_act(leg, "land-answer@v1", Duration::from_secs(240))?;
        let asked = self.await_tag(leg, "land-answer", Duration::from_secs(60))?;
        let acts = self.acts(&leg.sid);
        self.check(
            &format!(
                "question: under Land the question in prose after it gets Land's answer (land-answer@v1, aterm's LAND_ANSWER words), never a Cruise answer (acts: {})",
                acts.join(" ")
            ),
            land_answer && asked && !acts.iter().any(|a| a == "answer@v1"),
        );
        Ok(())
    }

    /// The build the leg's own rollout names (its `session_meta`'s
    /// `cli_version`), read off the leg's scratch `CODEX_HOME`.
    fn rollout_build(leg: &Leg) -> Option<String> {
        let rollouts = aterm_agent::harness::codex_rollout::rollouts_by_write(&leg.home);
        let (path, _) = rollouts.last()?;
        let text = std::fs::read_to_string(path).ok()?;
        let meta = J::parse(text.lines().next()?).ok()?;
        meta.get("payload")?
            .get("cli_version")?
            .as_str()
            .map(str::to_string)
    }

    /// THE ROLLOUT TAP, LIVE:
    /// each done leg's tab logs turns from its codex-alab's own
    /// rollout when the build that rollout names is one the session log
    /// measured, and none when not.
    fn rollout_leg(&mut self, legs: &[&Leg]) -> Result<(), Stopped> {
        use aterm_agent::harness::codex_steps::CODEX_MEASURED_BUILDS;
        for leg in legs {
            let build = Self::rollout_build(leg);
            let measured = build
                .as_deref()
                .is_some_and(|b| CODEX_MEASURED_BUILDS.contains(&b));
            let turns = || -> Vec<String> {
                self.on(&leg.sid, &["log", "kinds=turn"])
                    .stdout
                    .lines()
                    .filter(|l| l.starts_with(|c: char| c.is_ascii_digit()))
                    .map(str::to_string)
                    .collect()
            };
            let fenced = past_the_fence(
                || tap_said(&self.status(&leg.sid)),
                turns,
                |ok| Self::wait_for(Duration::from_secs(60), Duration::from_millis(250), ok),
            )?;
            println!(
                "  {}: log @{} kinds=turn (its rollout names {}):",
                leg.name,
                leg.sid,
                build.as_deref().unwrap_or("no build")
            );
            let name = leg.name;
            let want = if measured {
                "the tab's log holds turns from its rollout (src=transcript, rollout=bound)"
            } else {
                "an unmeasured build's rollout steps no turn (rollout=unmeasured)"
            };
            match fenced {
                Fenced::Past { from, to, got } => {
                    for line in &got {
                        println!("    {line}");
                    }
                    self.check(
                        &format!(
                            "{name}: past the tap's fence (rollout_asks {} to {}, rollout={}), {want}",
                            from.asks, to.asks, to.word
                        ),
                        rollout_holds(measured, &to, &got),
                    );
                }
                Fenced::NoTap => {
                    self.check(
                        &format!(
                            "{name}: status names the rollout tap's answer (rollout=), {want}"
                        ),
                        false,
                    );
                }
                Fenced::Stuck { from, to } => {
                    self.check(
                        &format!(
                            "{name}: the rollout tap answers two asks past a look within a minute (rollout_asks {} to {}, rollout={}), {want}",
                            from.asks, to.asks, to.word
                        ),
                        false,
                    );
                }
            }
        }
        Ok(())
    }

    fn hold_leg(&mut self, leg: &Leg, pid: Option<i32>) -> Result<(), Stopped> {
        let answered = Self::wait_for(Duration::from_secs(90), Duration::from_secs(1), || {
            !self.requests("hold").is_empty()
        })?;
        let t1 = self
            .requests("hold")
            .first()
            .and_then(|r| r.get("t").and_then(J::as_i64))
            .unwrap_or(0);
        let since = Instant::now();
        let quiet = model::HOLD_FIRST_TURN + Duration::from_secs(25);
        while since.elapsed() < quiet
            && crate::util::now_secs() * 1000 < t1 + i64::try_from(quiet.as_millis()).unwrap_or(0)
        {
            sleep(Duration::from_secs(1))?;
        }
        let held_acts = self.acts(&leg.sid);
        self.check(
            "hold: nothing continued while the hold stood (25 s past a turn of real work)",
            answered
                && !held_acts.iter().any(|a| a.starts_with("continue"))
                && self.tags("hold") == ["hold:person"],
        );
        self.check(
            "hold: codex-alab kept its proven claim under the hold",
            pid.is_some() && self.proven_pid(&leg.sid) == pid,
        );
        let off = self.inst.ctl(&["hold", &leg.sid, "off"]);
        self.check(
            &format!("hold: lifted ({})", off.first()),
            off.first().contains("hold=0"),
        );
        let lifted = Instant::now();
        let continued = self.await_act(leg, "continue@v1", Duration::from_secs(120))?;
        println!(
            "    hold: continued {:.1} s after the lift",
            lifted.elapsed().as_secs_f64()
        );
        self.check(
            "hold: lifted, the turn is continued (continue@v1)",
            continued,
        );
        Ok(())
    }

    /// The usage wall (module header): waited out, then resumed once.
    fn usage_leg(&mut self, leg: &Leg) -> Result<(), Stopped> {
        let resumed_act = self.await_act(leg, "usage-resume@v1", Duration::from_secs(300))?;
        let asked = self.await_tag(leg, "continue", Duration::from_secs(60))?;
        let rows = self.requests("usage");
        let limited: Vec<&J> = rows.iter().filter(|r| int_of(r, "status") == 429).collect();
        let limit_at = limited.first().map(|r| int_of(r, "t"));
        let reset = limited
            .first()
            .map(|r| str_of(r, "tag"))
            .and_then(|t| t.split_once("resets_at=").map(|(_, n)| n.to_string()))
            .and_then(|n| n.parse::<i64>().ok());
        let floor = reset.map(|r| (r + 60) * 1000);
        let early: Vec<String> = rows
            .iter()
            .filter(|r| {
                let t = int_of(r, "t");
                limit_at.is_some_and(|l| t > l) && floor.is_some_and(|f| t < f)
            })
            .map(|r| str_of(r, "tag"))
            .collect();
        let resumed = first_at(&rows, "usage:continue");
        if let (Some(reset), Some(at)) = (reset, resumed) {
            println!(
                "    usage: resets_at={reset}; resumed {} after it",
                secs(at - reset * 1000)
            );
        }
        self.check(
            &format!(
                "usage: the 429 is waited out: one 429, and no request of any kind until resets_at + 60 s ({} 429s; early: {early:?})",
                limited.len()
            ),
            limited.len() == 1
                && early.is_empty()
                && floor.zip(resumed).is_some_and(|(f, at)| at >= f),
        );
        let sent: Vec<(String, i64)> = rows
            .iter()
            .filter(|r| str_of(r, "tag").starts_with("usage:continue"))
            .map(|r| (str_of(r, "turn"), int_of(r, "n")))
            .collect();
        let twice = sent.iter().enumerate().any(|(i, s)| sent[..i].contains(s));
        let acts = self.acts(&leg.sid);
        let resumes = acts.iter().filter(|a| *a == "usage-resume@v1").count();
        self.check(
            &format!(
                "usage: then resumed once (usage-resume@v1 x{resumes}) and the model asked again, no request sent twice (turn, items: {sent:?}; acts: {})",
                acts.join(" ")
            ),
            resumed_act && asked && resumes == 1 && !twice,
        );
        Ok(())
    }

    /// Autopilot Land, unbounded, its unclean defenses nudged (module header).
    fn land_leg(&mut self, leg: &Leg) -> Result<(), Stopped> {
        let started = Instant::now();
        let land = self.await_act(leg, "land@v1", Duration::from_secs(240))?;
        let land_asked = self.await_tag(leg, "land", Duration::from_secs(60))?;
        let word = field(&self.status(&leg.sid), "autopilot").unwrap_or_default();
        self.check(
            &format!(
                "land: Land's instruction is typed (land@v1, aterm's LAND words; status autopilot={word})"
            ),
            land && land_asked && word == "land",
        );
        if !land_asked {
            return Ok(());
        }
        let mut nudged = 0;
        for n in 1..=NUDGES_BEFORE_DEFENSE {
            if !self.await_tag(
                leg,
                &format!("nudge{n}"),
                SLOW_TURN + Duration::from_secs(150),
            )? {
                break;
            }
            nudged = n;
            println!(
                "    land: nudge {n} asked {:.0} s into the leg's check",
                started.elapsed().as_secs_f64()
            );
        }
        let seen = nudges_seen(&self.tags("land"));
        let landed = self.await_act(leg, LANDED, Duration::from_secs(90))?;
        let acts = self.acts(&leg.sid);
        let before = nudges_before_landing(&acts);
        let needs_acked = self
            .tags("land")
            .iter()
            .any(|t| t.starts_with("land:nudge2+ack"));
        self.check(
            &format!(
                "land: a `Nothing left:` defense that names a NEEDS YOU need is not landed: land-nudge@v1 follows it (nudges before the landing: {before}; the second carrying the NEEDS ack: {needs_acked})"
            ),
            before >= 2 && needs_acked,
        );
        self.check(
            &format!(
                "land: a `Nothing left:` defense that names a part left for later is not landed: land-nudge@v1 follows it (nudges before the landing: {before})"
            ),
            before >= 4,
        );
        let nudges = acts.iter().filter(|a| *a == "land-nudge@v1").count();
        self.check(
            &format!(
                "land: each yield that is no clean defense gets land-nudge@v1, again and again: {nudges} nudges, the model asked at {seen:?} (four: past the two an older Land stopped after, and any bound under four)"
            ),
            nudged == NUDGES_BEFORE_DEFENSE && nudges >= NUDGES_BEFORE_DEFENSE,
        );
        let mut face = String::new();
        let landed_face = landed
            && Self::wait_for(Duration::from_secs(30), Duration::from_millis(250), || {
                face = task_face(&self.status(&leg.sid));
                face == "landed"
            })?;
        self.check(
            &format!(
                "land: the clean `Nothing left:` defense in a folder that is no repository gives {LANDED}, and aterm's status task= for the self-claimed tab reads landed (FOREVER LAND §10.4; task={face})"
            ),
            landed && landed_face,
        );
        println!(
            "    land: landed {:.0} s into the leg's check",
            started.elapsed().as_secs_f64()
        );
        Ok(())
    }

    /// The reland leg's landing and the quiet after it, past the back-off a
    /// codex-alab that went on would wait; then a person's turn reopens it
    /// (module header).
    fn reland_landed(&mut self, leg: &Leg) -> Result<bool, Stopped> {
        let land = self.await_act(leg, "land@v1", Duration::from_secs(300))?;
        let landed = self.await_act(leg, LANDED, Duration::from_secs(120))?;
        let mut face = String::new();
        let landed_face = landed
            && Self::wait_for(Duration::from_secs(30), Duration::from_millis(250), || {
                face = task_face(&self.status(&leg.sid));
                face == "landed"
            })?;
        self.check(
            &format!(
                "reland: land@v1 after a turn of real work, and its clean defense lands ({LANDED}; status task={face})"
            ),
            land && landed && landed_face,
        );
        if !landed {
            return Ok(false);
        }
        let rows = self.requests("reland");
        let at = rows
            .iter()
            .rev()
            .find(|r| str_of(r, "tag").starts_with("reland:land"))
            .map_or_else(crate::util::now_ms, |r| int_of(r, "t"));
        let asked_before = rows.len();
        let acts_before = self.acts(&leg.sid).len();
        let until = at + ms_of(LANDED_QUIET);
        while crate::util::now_ms() < until {
            sleep(Duration::from_secs(1))?;
        }
        let asked_after = self.tags("reland");
        let acts_after = self.acts(&leg.sid);
        self.check(
            &format!(
                "reland: then NOTHING more: no request to the model and no act for {} s after the landing, past the 120 s a codex-alab that went on would wait (asked: {}; acts: {})",
                LANDED_QUIET.as_secs(),
                asked_after.join(" "),
                acts_after.join(" ")
            ),
            asked_after.len() == asked_before && acts_after.len() == acts_before,
        );
        let turn = self.on(
            &leg.sid,
            &[
                "turn",
                "settle=submitted",
                "timeout=30000",
                REOPEN,
                "one more look at the landed work",
            ],
        );
        let typed = Instant::now();
        let mut face = String::new();
        let reopened = Self::wait_for(Duration::from_secs(60), Duration::from_millis(250), || {
            face = task_face(&self.status(&leg.sid));
            face == "open"
        })?;
        let after = typed.elapsed().as_secs_f64();
        let asked = self.await_tag(leg, "reopen", Duration::from_secs(60))?;
        self.check(
            &format!(
                "reland: a person's later turn reopens it (status task={face} {after:.1} s after the turn was typed; the model asked: {asked}; turn: {})",
                turn.first()
            ),
            reopened && asked,
        );
        Ok(asked)
    }

    /// After the person's turn of real work, codex-alab acts again: a new
    /// episode's `land@v1`, whose clean defense lands again.
    fn reland_again(&mut self, leg: &Leg) -> Result<(), Stopped> {
        let reopen_at = first_at(&self.requests("reland"), "reland:reopen");
        let mut acts = Vec::new();
        let landed_again = Self::wait_for(
            SLOW_TURN + Duration::from_secs(120),
            Duration::from_secs(1),
            || {
                acts = self.acts(&leg.sid);
                let Some(first) = acts.iter().position(|a| a == LANDED) else {
                    return false;
                };
                acts[first + 1..]
                    .iter()
                    .position(|a| a == "land@v1")
                    .is_some_and(|again| acts[first + 1 + again..].iter().any(|a| a == LANDED))
            },
        )?;
        let rows = self.requests("reland");
        let relanded_at = rows
            .iter()
            .filter(|r| str_of(r, "tag").starts_with("reland:land"))
            .nth(1)
            .map(|r| int_of(r, "t"));
        if let (Some(r), Some(l)) = (reopen_at, relanded_at) {
            println!(
                "    reland: the new land@v1 asked {} after the person's turn (its real work {} s)",
                secs(l - r),
                SLOW_TURN.as_secs()
            );
        }
        self.check(
            &format!(
                "reland: after the person's turn of real work codex-alab acts again: a new land@v1, and its clean defense lands again (acts: {})",
                acts.join(" ")
            ),
            landed_again && relanded_at.is_some(),
        );
        Ok(())
    }

    /// The first request of `leg` whose tag starts with `head`: its tag and
    /// the tail of its text.
    fn request_named(&self, leg: &str, head: &str) -> (String, String) {
        self.requests(leg)
            .iter()
            .find(|r| str_of(r, "tag").starts_with(head))
            .map(|r| (str_of(r, "tag"), str_of(r, "u")))
            .unwrap_or_default()
    }

    /// A clean defense over a repository that still holds work: the file
    /// untracked (module header).
    fn repo_leg(&mut self, leg: &Leg) -> Result<(), Stopped> {
        let land = self.await_act(leg, "land@v1", Duration::from_secs(300))?;
        let defended = self.await_tag(leg, "land", Duration::from_secs(60))?;
        self.check(
            "repo: Land's instruction is typed (land@v1)",
            land && defended,
        );
        if !defended {
            return Ok(());
        }
        let nudged = self.await_tag(leg, "nudge1", Duration::from_secs(240))?;
        let ended = self.has_any(leg, &[LANDED]);
        let asked = first_at(&self.requests("repo"), "repo:nudge1").unwrap_or(0);
        let (tag, text) = self.request_named("repo", "repo:nudge1");
        self.check(
            &format!(
                "repo: the clean defense over a repository with an untracked file is NOT landed: a land-nudge@v1 follows naming the repository's work (ends {ended:?}; …{})",
                tail(&text, 180)
            ),
            nudged && ended.is_empty() && tag.ends_with("+repo") && text.contains(NOTES),
        );
        if !nudged {
            return Ok(());
        }
        let committed = self
            .git(&leg.proj, &["add", NOTES])
            .and_then(|_| self.git(&leg.proj, &["commit", "-q", "-m", "lab: the release notes"]))
            .and_then(|_| self.git(&leg.proj, &["status", "--porcelain", "--branch"]));
        let into = crate::util::now_ms() - asked;
        let late = into >= ms_of(REPO_COMMIT_HOLD);
        self.check(
            &format!(
                "repo: the lab committed {NOTES}, not pushing it, {} into the model's {} s hold ({})",
                secs(into),
                REPO_COMMIT_HOLD.as_secs(),
                committed
                    .as_ref()
                    .map_or_else(Clone::clone, |s| s.trim().replace('\n', " | "))
            ),
            committed.as_ref().is_ok_and(|s| s.contains("ahead 1")) && !late,
        );
        Ok(())
    }

    /// The repository leg after its commit: the clean defense is NOT landed
    /// over the commit the remote main lacks; pushed, the next one lands.
    fn repo_pushed(&mut self, leg: &Leg) -> Result<(), Stopped> {
        let nudged = self.await_tag(leg, "nudge2", REPO_COMMIT_HOLD + Duration::from_secs(360))?;
        let ended = self.has_any(leg, &[LANDED]);
        let asked = first_at(&self.requests("repo"), "repo:nudge2").unwrap_or(0);
        let (tag, text) = self.request_named("repo", "repo:nudge2");
        self.check(
            &format!(
                "repo: committed but not pushed, the next clean defense is NOT landed either: a land-nudge@v1 names the commit the remote main lacks (ends {ended:?}; …{})",
                tail(&text, 180)
            ),
            nudged && ended.is_empty() && tag.ends_with("+repo") && text.contains("not on origin/main"),
        );
        if !nudged {
            return Ok(());
        }
        let pushed = self
            .git(&leg.proj, &["push", "-q", "origin", "main"])
            .and_then(|_| self.git(&leg.proj, &["status", "--porcelain", "--branch"]));
        let into = crate::util::now_ms() - asked;
        let late = into >= ms_of(REPO_PUSH_HOLD);
        self.check(
            &format!(
                "repo: the lab pushed it to the local bare origin {} into the model's {} s hold ({})",
                secs(into),
                REPO_PUSH_HOLD.as_secs(),
                pushed
                    .as_ref()
                    .map_or_else(Clone::clone, |s| s.trim().replace('\n', " | "))
            ),
            pushed.is_ok() && !late,
        );
        let landed = self.await_act(leg, LANDED, Duration::from_secs(90))?;
        let mut face = String::new();
        let landed_face = landed
            && Self::wait_for(Duration::from_secs(30), Duration::from_millis(250), || {
                face = task_face(&self.status(&leg.sid));
                face == "landed"
            })?;
        if face.is_empty() {
            face = task_face(&self.status(&leg.sid));
        }
        self.check(
            &format!(
                "repo: pushed, the next clean defense lands it ({LANDED}; status task={face})"
            ),
            landed && landed_face,
        );
        Ok(())
    }

    /// The Land legs at the run's end: nothing after their landing (an
    /// observation: how long since says what a later act would have needed).
    fn landed_stay_quiet(&mut self, legs: &[&Leg]) {
        for leg in legs {
            let acts = self.acts(&leg.sid);
            let Some(at) = acts.iter().rposition(|a| a == LANDED) else {
                continue;
            };
            let rows = self.requests(leg.name);
            let since = rows
                .iter()
                .rev()
                .find(|r| str_of(r, "said").contains("Nothing left:"))
                .map_or(0, |r| {
                    crate::util::now_ms() - int_of(r, "t") - int_of(r, "hold_ms")
                });
            let after = &acts[at + 1..];
            self.check(
                &format!(
                    "{}: nothing after its last {LANDED} by the run's end ({} since; acts after it: {after:?})",
                    leg.name,
                    secs(since)
                ),
                after.is_empty(),
            );
        }
    }

    /// Autopilot Cruise, over every Cruise leg's whole run (module header):
    /// nothing finished or landed, and each done claim met by the defend
    /// prompt, by the model's own log ([`claims_of`]).
    fn cruise_rests_never(&mut self, cruise: &[&Leg]) {
        let ended: Vec<String> = cruise
            .iter()
            .flat_map(|leg| {
                self.has_any(leg, &[FINISHED, LANDED])
                    .into_iter()
                    .map(move |a| format!("{}:{a}", leg.name))
            })
            .collect();
        self.check(
            &format!("cruise: no Cruise leg's timeline holds {FINISHED} or {LANDED} ({ended:?})"),
            ended.is_empty(),
        );
        let now = crate::util::now_ms();
        let mut met = 0;
        let mut pending = 0;
        let mut unmet = Vec::new();
        let mut each = Vec::new();
        for leg in cruise {
            let c = claims_of(leg.name, &self.requests(leg.name), now);
            if c.met + c.pending + c.unmet.len() > 0 {
                each.push(format!("{} {}+{}", leg.name, c.met, c.pending));
            }
            met += c.met;
            pending += c.pending;
            unmet.extend(c.unmet);
        }
        self.check(
            &format!(
                "cruise: each done claim of every Cruise leg is followed by {DEFEND}'s request, or its back-off has not run out yet ({met} met, {pending} pending at the run's end: {}; unmet: {unmet:?})",
                each.join(", ")
            ),
            unmet.is_empty() && met > 0,
        );
    }

    /// `SIGKILL` the codex-alab `pid` of `leg`; the crash rule's relaunch
    /// proven again (a new pid).
    fn crash_once(&mut self, leg: &Leg, pid: i32, label: &str) -> Result<Option<i32>, Stopped> {
        let _ = crate::sys::kill(pid, libc::SIGKILL);
        println!("    {label}: SIGKILL {pid}");
        let mut next = None;
        Self::wait_for(Duration::from_secs(150), Duration::from_millis(50), || {
            next = self.proven_pid(&leg.sid).filter(|p| *p != pid);
            next.is_some()
        })?;
        self.check(
            &format!("{label}: relaunched on its conversation and proven again"),
            next.is_some(),
        );
        Ok(next)
    }

    fn crash_leg(&mut self, leg: &Leg, pid: Option<i32>) -> Result<(), Stopped> {
        let Some(pid1) = pid else {
            self.check("crash: no proven codex-alab to end", false);
            return Ok(());
        };
        let session = Self::record(leg, pid1)
            .and_then(|r| r.get("sessionId").and_then(J::as_str).map(str::to_string));
        let Some(pid2) = self.crash_once(leg, pid1, "crash (the instance's first session)")? else {
            return Ok(());
        };
        let written = leg
            .home
            .join(format!("alab/sessions/{pid2}.json"))
            .is_file();
        let Some(pid3) = self.crash_once(
            leg,
            pid2,
            &format!(
                "second crash (as soon as the relaunch's claim was proven; its record {})",
                if written {
                    "written"
                } else {
                    "not yet written"
                }
            ),
        )?
        else {
            return Ok(());
        };
        let relaunches = self
            .host_lines(&leg.sid)
            .iter()
            .filter(|l| l.contains("the self-supervised codex-alab crashed; relaunching it"))
            .count();
        self.check(
            "crash: each crash relaunched by the crash rule (two relaunches)",
            relaunches == 2,
        );
        let mut resumed = None;
        Self::wait_for(Duration::from_secs(60), Duration::from_secs(1), || {
            resumed = Self::record(leg, pid3)
                .and_then(|r| r.get("sessionId").and_then(J::as_str).map(str::to_string));
            resumed.is_some()
        })?;
        self.check(
            "crash: the twice-relaunched codex-alab resumed the same conversation",
            session.is_some() && resumed == session,
        );
        Ok(())
    }

    /// The leg's codex-alab journal ([`journal_path`]: every process of the
    /// leg appending to one file).
    fn codex_log(leg: &Leg) -> String {
        std::fs::read_to_string(journal_path(&leg.home)).unwrap_or_default()
    }

    /// The instance's harness upgrade ledger (`aterm harness ledger upgrade
    /// --json` under the scratch HOME, as the owner reads it), row by row.
    fn upgrade_ledger(&self) -> Vec<J> {
        let out = aterm_uds::spawnfd::command(&self.inst.aterm)
            .env_clear()
            .envs(self.world_env.iter().map(|(k, v)| (k.as_str(), v.as_str())))
            .args(["harness", "ledger", "upgrade", "4096", "--json"])
            .stdin(Stdio::null())
            .output();
        out.map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
            .unwrap_or_default()
            .lines()
            .filter_map(|l| J::parse(l).ok())
            .collect()
    }

    /// THE CARRY-ON AFTER A CRASH RELAUNCH (module header): `SIGKILL` the
    /// leg's codex-alab mid-turn, once its crash is relaunchable; the
    /// relaunched one carries the turn on by itself only when its launch probe
    /// saw aterm's relaunch lease.
    fn carry_leg(&mut self, leg: &Leg, pid: Option<i32>) -> Result<(), Stopped> {
        let Some(pid1) = pid else {
            self.check("carry: no proven codex-alab to end", false);
            return Ok(());
        };
        let renewed = "renewed the claim: the record's resume line moved";
        let ready = Self::wait_for(Duration::from_secs(120), Duration::from_millis(250), || {
            !self.requests("carry").is_empty()
                && Self::record(leg, pid1).is_some_and(|r| {
                    r.get("status").and_then(J::as_str) == Some("busy")
                        && r.get("resumeArgv")
                            .and_then(J::as_arr)
                            .is_some_and(|a| !a.is_empty())
                })
                && Self::codex_log(leg).contains(renewed)
        })?;
        self.check(
            "carry: mid-turn and relaunchable (its request at the model, its record busy naming its resume line, its claim renewed with it)",
            ready,
        );
        if !ready {
            return Ok(());
        }
        let killed_ms = crate::util::now_secs() * 1000;
        let Some(pid2) = self.crash_once(leg, pid1, "carry: crash mid-turn")? else {
            return Ok(());
        };
        let carried = Self::wait_for(Duration::from_secs(120), Duration::from_secs(1), || {
            self.tags("carry").iter().any(|t| t == "carry:continue")
        })?;
        let turned = self.await_act(leg, "harness-turn@v1", Duration::from_secs(60))?;
        let log = Self::codex_log(leg);
        let probe = format!("launch probe: hand=lease:aterm-harness@{} ", self.inst.pid);
        let probes: Vec<&str> = log
            .lines()
            .filter(|l| l.contains("launch probe:"))
            .map(|l| l.split_once("launch probe:").map_or(l, |(_, r)| r.trim()))
            .collect();
        self.check(
            &format!(
                "carry: the relaunched codex-alab's launch probe saw aterm's relaunch lease and resumed as relaunched (probes: {})",
                probes.join(" | ")
            ),
            log.contains(&probe) && log.contains("relaunched=true"),
        );
        let held = self
            .upgrade_ledger()
            .iter()
            .filter(|row| {
                row.get("step").and_then(J::as_str) == Some("done:resumed")
                    && row.get("pid").and_then(J::as_i64) == Some(i64::from(pid2))
            })
            .filter_map(|row| row.get("detail").and_then(J::as_str).map(str::to_string))
            .next_back()
            .unwrap_or_default();
        let held = held
            .rsplit_once("; ")
            .map_or(held.as_str(), |(_, h)| h)
            .to_string();
        self.check(
            &format!(
                "carry: aterm held its relaunch hand to the relaunched process's first idle (its ledger: {held:?})"
            ),
            held == "the hand held to its first idle",
        );
        let keep = self
            .requests("carry")
            .iter()
            .filter(|r| {
                r.get("u")
                    .and_then(J::as_str)
                    .is_some_and(|u| u.contains(KEEP_GOING))
                    && r.get("t").and_then(J::as_i64).unwrap_or(0) >= killed_ms
            })
            .count();
        let holder = format!("codex-alab@{pid2}");
        let in_timeline = self
            .on(&leg.sid, &["timeline"])
            .stdout
            .lines()
            .filter(|l| l.contains("kind=harness-act"))
            .any(|l| {
                field(l, "act").as_deref() == Some("harness-turn@v1")
                    && field(l, "holder").as_deref() == Some(holder.as_str())
            });
        let in_log = acted_in_log(
            &self.on(&leg.sid, &["log", "kinds=act"]).stdout,
            "harness-turn@v1",
            &holder,
        );
        let by_relaunched = turned && in_timeline && in_log;
        self.check(
            &format!(
                "carry: the relaunched codex-alab carried the interrupted turn on by itself, once (`keep going` x{keep} after the relaunch, harness-turn@v1 by {holder}: timeline {in_timeline}, log {in_log})"
            ),
            carried && keep == 1 && by_relaunched,
        );
        let defended = self.await_act(leg, DEFEND, Duration::from_secs(90))?;
        let ended = self.has_any(leg, &[FINISHED, LANDED]);
        self.check(
            &format!(
                "carry: the carried reply's done claim gets Cruise's defend prompt ({DEFEND}), never {FINISHED} (ends {ended:?})"
            ),
            defended && ended.is_empty(),
        );
        Ok(())
    }

    fn upgrade_leg(&mut self, leg: &Leg, pkg_b: &Path) -> Result<(), Stopped> {
        self.upgrade_leg_with_limits(
            leg,
            pkg_b,
            Duration::from_secs(150),
            Duration::from_secs(90),
        )
    }

    fn upgrade_leg_with_limits(
        &mut self,
        leg: &Leg,
        pkg_b: &Path,
        moved_limit: Duration,
        claim_limit: Duration,
    ) -> Result<(), Stopped> {
        let Some(pid) = self.proven_pid(&leg.sid) else {
            self.check("upgrade: no proven codex-alab to move", false);
            return Ok(());
        };
        let generation = exec_generation;
        let before = Self::record(leg, pid).map_or(0, |r| generation(&r));
        let linked = self.link(pkg_b);
        self.check(
            &format!(
                "upgrade: the package re-linked to its replacement ({})",
                linked.as_ref().err().map_or("ok", String::as_str)
            ),
            linked.is_ok(),
        );
        let b = std::fs::canonicalize(pkg_b).unwrap_or_else(|_| pkg_b.to_path_buf());
        let moved = Self::wait_for(moved_limit, Duration::from_secs(1), || {
            Self::record(leg, pid).is_some_and(|r| {
                generation(&r) > before
                    && r.get("exe")
                        .and_then(J::as_str)
                        .and_then(|e| std::fs::canonicalize(e).ok())
                        .is_some_and(|e| e.starts_with(&b))
            })
        })?;
        let acts = self.acts(&leg.sid);
        self.check(
            "upgrade: the running codex-alab re-exec'd onto the re-linked build (upgrade@v1, execGeneration up, exe the replacement's)",
            moved && acts.iter().any(|a| a == "upgrade@v1"),
        );
        let reproven = Self::wait_for(claim_limit, Duration::from_secs(1), || {
            self.proven_pid(&leg.sid) == Some(pid)
        })?;
        self.check("upgrade: the new image proved its claim again", reproven);
        Ok(())
    }

    /// The instance's bridge is connected to the lab's broker
    /// (`fabric status state=connected`): a hang detector, never a budget.
    fn bus_up(&mut self) -> Result<bool, Stopped> {
        let mut last = String::new();
        let up = Self::wait_for(Duration::from_secs(120), Duration::from_millis(500), || {
            last = self.inst.ctl(&["fabric", "status"]).first();
            field(&last, "state").as_deref() == Some("connected")
        })?;
        self.check(
            &format!("the lab's bus: the instance's bridge is connected ({last})"),
            up,
        );
        Ok(up)
    }

    /// THE AUTOPILOT OFF STEP's start (module header): Off while the leg's
    /// first turn, real work, runs; codex-alab gives its claim back.
    fn autopilot_off(&mut self, leg: &Leg) -> Result<Option<i64>, Stopped> {
        let name = leg.name;
        let off = self.on(&leg.sid, &["meta", "set", "autopilot", "off"]);
        let at = crate::util::now_ms();
        let set = off.code == 0 && off.first().starts_with("OK");
        self.check(
            &format!(
                "autopilot: Autopilot Off on {name} while its first turn (real work) runs ({})",
                off.first()
            ),
            set,
        );
        let mut status = String::new();
        let released =
            Self::wait_for(Duration::from_secs(120), Duration::from_millis(500), || {
                status = self.status(&leg.sid);
                field(&status, "supervisor_proven").as_deref() != Some("1")
                    && !field(&status, "supervisor").is_some_and(|s| s.starts_with("codex-alab@"))
            })?;
        self.check(
            &format!(
                "autopilot: under Autopilot Off codex-alab gives its claim back (supervisor={} supervisor_proven={})",
                field(&status, "supervisor").unwrap_or_default(),
                field(&status, "supervisor_proven").unwrap_or_default()
            ),
            released,
        );
        Ok(set.then_some(at))
    }

    /// THE AUTOPILOT OFF STEP (module header): the turn ends inside Off, an
    /// act due at once, and nothing is typed; the mail goes alone; On, the
    /// claim comes back and the act that fell due goes.
    fn autopilot_off_leg(
        &mut self,
        leg: &Leg,
        manager: &str,
        off_at: Option<i64>,
    ) -> Result<(), Stopped> {
        let name = leg.name;
        let to = format!("to=@{}", leg.sid);
        let text = format!("LEG-{name} lab mail sent under Autopilot Off");
        let task = self.on(manager, &["post", &to, "kind=task", "--", &text]);
        let said = task.first();
        let off_at_inbox = said
            .strip_prefix("OK ")
            .and_then(|r| r.split_whitespace().find_map(|w| w.strip_prefix("off=")))
            .map(str::to_string);
        self.check(
            &format!(
                "autopilot: the manager's `post kind=task` lands the mail ({said}{})",
                if task.code == 0 {
                    String::new()
                } else {
                    format!("; exit {}: {}", task.code, task.stderr.trim())
                }
            ),
            task.code == 0 && off_at_inbox.is_some(),
        );
        let mut rows = String::new();
        let landed = Self::wait_for(Duration::from_secs(60), Duration::from_millis(500), || {
            rows = self.on(&leg.sid, &["inbox", "--peek"]).stdout;
            rows.lines().any(|row| {
                row.starts_with("msg ")
                    && field(row, "kind").as_deref() == Some("task")
                    && off_at_inbox
                        .as_deref()
                        .is_some_and(|off| field(row, "off").as_deref() == Some(off))
            })
        })?;
        self.check(
            "autopilot: the task is in the worker's inbox (kind=task, its offset)",
            landed,
        );
        let due =
            first_at(&self.requests(name), &format!("{name}:person")).map(|t| t + ms_of(SLOW_TURN));
        let inside = off_at.zip(due).is_some_and(|(o, d)| o < d);
        let until = due.map_or_else(crate::util::now_ms, |d| d + 25_000);
        while crate::util::now_ms() < until {
            sleep(Duration::from_secs(1))?;
        }
        let screen = self.on(&leg.sid, &["text"]).stdout;
        let turns = self.on(&leg.sid, &["history"]).stdout;
        let asked = self.tags(name);
        let acts = self.acts(&leg.sid);
        self.check(
            &format!(
                "autopilot: under Off nothing is typed although an act fell due inside the window (the turn of real work ended {} after the Off; 25 s past it the model was asked {asked:?}, acts {acts:?}; no `Inbox: task @` line)",
                off_at
                    .zip(due)
                    .map_or_else(|| "?".to_string(), |(o, d)| secs(d - o))
            ),
            inside
                && asked == [format!("{name}:person")]
                && acts.is_empty()
                && !screen.contains(NUDGE)
                && !turns.contains(NUDGE),
        );
        let on = self.on(&leg.sid, &["meta", "set", "autopilot", "cruise"]);
        self.check(
            &format!("autopilot: back On (cruise) on {name} ({})", on.first()),
            on.code == 0 && on.first().starts_with("OK"),
        );
        let mut again = None;
        Self::wait_for(Duration::from_secs(120), Duration::from_millis(500), || {
            again = self.proven_pid(&leg.sid);
            again.is_some()
        })?;
        self.check(
            &format!(
                "autopilot: back On, codex-alab claims again (supervisor=codex-alab@{} supervisor_proven=1)",
                again.map_or_else(|| "-".to_string(), |p| p.to_string())
            ),
            again.is_some(),
        );
        let continued = self.await_act(leg, "continue@v1", Duration::from_secs(120))?;
        self.check(
            "autopilot: back On, the act that fell due under Off is typed (continue@v1)",
            continued,
        );
        Ok(())
    }

    /// THE EXCHANGE LEGS' TURN (module header): the message (`LEG-<leg>`
    /// and [`PERSON_ASKS`]) typed into the leg's codex composer and sent with
    /// Enter — as a PERSON types it where `as_person`, through aterm's
    /// development seam `send|key as=person` (a person's gesture through the
    /// window's own input seam, which stamps `status human_ms=`), else as a
    /// control write (`send`, `key`).
    fn type_exchange(&mut self, leg: &Leg, as_person: bool) -> Result<Option<Typed>, Stopped> {
        let name = leg.name;
        let text = format!("LEG-{name} {PERSON_ASKS}");
        let draft = |p: &Self| composer_row("codex", &p.on(&leg.sid, &["text"]).stdout);
        let ready = Self::wait_for(Duration::from_secs(60), Duration::from_millis(250), || {
            draft(self).is_some()
        })?;
        if !ready {
            self.check(&format!("{name}: codex's composer is on its screen"), false);
            return Ok(None);
        }
        let seam: &[&str] = if as_person { &["as=person"] } else { &[] };
        let how = if as_person {
            "`send|key as=person`"
        } else {
            "`send`, `key`"
        };
        let refused = |p: &mut Self, verb: &str, r: &instance::Reply| {
            p.check(
                &format!(
                    "{name}: COULD NOT RUN: `{verb}{}` refused ({}{}){}",
                    if as_person { " as=person" } else { "" },
                    r.stdout.trim(),
                    r.stderr.trim(),
                    if as_person {
                        ": a person's own exchange needs aterm's development seam, a debug or \
                         dev-seams build of this tree (targo --unverified build -p aterm)"
                    } else {
                        ""
                    }
                ),
                false,
            );
        };
        let before = self.status(&leg.sid);
        let mut send = vec!["send"];
        send.extend_from_slice(seam);
        send.push(&text);
        let sent = self.on(&leg.sid, &send);
        if sent.code != 0 {
            refused(self, "send", &sent);
            return Ok(None);
        }
        let mut seen = None;
        let stood = Self::wait_for(Duration::from_secs(60), Duration::from_millis(250), || {
            seen = draft(self);
            seen.as_deref() == Some(text.as_str())
        })?;
        if !stood {
            let older = seen.as_deref().is_some_and(|d| d.starts_with("as=person"));
            self.check(
                &if older {
                    format!(
                        "{name}: COULD NOT RUN: the aterm under test is older than `send|key \
                         as=person` (it typed the word into the composer: {seen:?}): build this \
                         tree's (targo --unverified build -p aterm)"
                    )
                } else {
                    format!("{name}: the message stood in codex's composer (it read {seen:?})")
                },
                false,
            );
            return Ok(None);
        }
        let drafted = self.status(&leg.sid);
        let mut key = vec!["key"];
        key.extend_from_slice(seam);
        key.push("enter");
        let mut pressed = None;
        for _ in 0..2 {
            let at = crate::util::now_ms();
            let r = self.on(&leg.sid, &key);
            if r.code != 0 {
                refused(self, "key", &r);
                return Ok(None);
            }
            let gone = Self::wait_for(Duration::from_secs(5), Duration::from_millis(250), || {
                draft(self).as_deref() != Some(text.as_str())
            })?;
            if gone {
                pressed = Some(at);
                break;
            }
        }
        let Some(pressed_ms) = pressed else {
            self.check(
                &format!("{name}: the message, typed by {how}, left codex's composer at its Enter"),
                false,
            );
            return Ok(None);
        };
        let after = self.status(&leg.sid);
        Ok(Some(Typed {
            pressed_ms,
            before,
            sent: drafted,
            after,
        }))
    }

    /// The exchange legs' turns typed (module header), and what aterm's
    /// `status` said of each hand: the person's Enter stamped `human_ms` and
    /// moved `human_seq` past the send's own stamp ([`Typed::enter_stamped`];
    /// else no person's Enter reached the session, and the exchange's checks
    /// say nothing of a person's exchange); the control's stamped nothing.
    fn type_exchanges(
        &mut self,
        person: &Leg,
        control: &Leg,
    ) -> Result<(Option<Typed>, Option<Typed>), Stopped> {
        let theirs = self.type_exchange(person, true)?;
        if let Some(t) = &theirs {
            let (ms, _) = person_hand(&t.after);
            let (stamped, [before, sent, after]) = t.enter_stamped();
            self.check(
                &format!(
                    "exchange: the person's message typed through aterm's development seam \
                     (`send|key as=person`), and their Enter stamped status human_ms as a window \
                     keystroke does (human_ms={} human_seq {before}->{sent} at the send, \
                     ->{after} at the Enter; task={} before it)",
                    ms.map_or_else(|| "-".to_string(), |m| m.to_string()),
                    task_face(&t.before)
                ),
                stamped,
            );
        }
        let ours = self.type_exchange(control, false)?;
        if let Some(t) = &ours {
            let ((_, was), (ms, seq)) = (person_hand(&t.before), person_hand(&t.after));
            self.check(
                &format!(
                    "control: the same message typed as a control write (`send`, `key`) stamped \
                     no person's gesture (human_ms={} human_seq {was}->{seq})",
                    ms.map_or_else(|| "-".to_string(), |m| m.to_string())
                ),
                t.stamped_nothing(),
            );
        }
        Ok((theirs, ours))
    }

    /// A PERSON'S OWN EXCHANGE, judged (module header): the control's turn
    /// continued, the person's left to them — nothing typed, the rule named
    /// in its journal, aterm's face `theirs`.
    fn exchange_leg(
        &mut self,
        person: &Leg,
        control: &Leg,
        typed: &(Option<Typed>, Option<Typed>),
    ) -> Result<(), Stopped> {
        let (Some(theirs), Some(ours)) = typed else {
            self.check(
                "exchange: both turns typed (the person's and the control's), so the exchange is \
                 judged against its control",
                false,
            );
            return Ok(());
        };
        let asked = self.tag_by(person, "person", theirs.pressed_ms + 60_000)?
            && self.tag_by(control, "person", ours.pressed_ms + 60_000)?;
        let control_reply =
            self.await_reply(control, "person", SLOW_TURN + Duration::from_secs(60))?;
        let remaining = control_reply.map_or(0, |r| {
            r.written_ms
                .saturating_add(CONTROL_ACT_MS)
                .saturating_sub(codex_rig::monotonic_ms(self.api_clock))
                .max(0)
        });
        if control_reply.is_some()
            && self.await_tag(
                control,
                "continue",
                Duration::from_millis(remaining.unsigned_abs()),
            )?
        {
            // Its act is on the timeline once it is typed.
            let _ = self.await_act(control, "continue@v1", Duration::from_secs(60))?;
        }
        let control_acts = self.acts(&control.sid);
        let continued = continued_after(control.name, &self.requests(control.name), &control_acts);
        self.check(
            &format!(
                "control: a turn typed as a control write through the same path is codex-alab's \
                 to go on with: Cruise's continuation (continue@v1) after its plain reply of real \
                 work ({}; acts: {})",
                continued
                    .as_ref()
                    .map_or_else(Clone::clone, |ms| format!("{} after the reply", secs(*ms))),
                control_acts.join(" ")
            ),
            asked && continued.is_ok(),
        );
        let control_ms = continued.ok();
        let reply = self.await_reply(person, "person", SLOW_TURN + Duration::from_secs(60))?;
        if let Some(reply) = reply {
            let need = ms_of(EXCHANGE_QUIET).max(control_ms.unwrap_or(0) + 10_000);
            self.sleep_past_reply(reply, Duration::from_millis(need.unsigned_abs()))?;
        }
        let acts = self.acts(&person.sid);
        let rows = self.requests(person.name);
        let left = left_to_person(
            person.name,
            &rows,
            &acts,
            reply,
            codex_rig::monotonic_ms(self.api_clock),
            control_ms,
        );
        let proven = self.proven_pid(&person.sid).is_some();
        self.check(
            &format!(
                "exchange: after the person's own turn and the model's plain reply of real work \
                 (successful reply write: {}), codex-alab types NOTHING, its claim still proven ({}; \
                 proven: {proven})",
                reply.map_or("not observed", |_| "observed"),
                left.as_ref().unwrap_or_else(|why| why)
            ),
            asked && left.is_ok() && proven,
        );
        let journal = Self::codex_log(person);
        let rule = exchange_rule(&journal);
        let typed_acts = journal_typed(&journal);
        self.check(
            &format!(
                "exchange: the leg's codex-alab journal names the rule it used, a person's own \
                 exchange (aterm's {RULE_LEFT}) ({}; typed: {typed_acts:?})",
                rule.as_deref().unwrap_or("no such line")
            ),
            rule.is_some() && typed_acts.is_empty(),
        );
        let control_journal = Self::codex_log(control);
        let control_typed = journal_typed(&control_journal);
        let control_rule = exchange_rule(&control_journal);
        self.check(
            &format!(
                "control: its journal names the act it typed (act continue@v1) and no person's \
                 exchange (typed: {control_typed:?}; rule line: {})",
                control_rule.as_deref().unwrap_or("none")
            ),
            control_typed.iter().any(|a| a == "continue@v1") && control_rule.is_none(),
        );
        let face = task_face(&self.status(&person.sid));
        self.check(
            &format!(
                "exchange: aterm's task face reads theirs at the point left to the person (status \
                 task={face}; task={} before their turn){}",
                task_face(&theirs.before),
                if face == "theirs" {
                    ""
                } else {
                    ": aterm tells a proven self-claim's face from the acts it records (FOREVER \
                     LAND §10.4), and codex-alab recorded no left-to-person@v1 at this point"
                }
            ),
            face == "theirs",
        );
        let control_face = task_face(&self.status(&control.sid));
        self.check(
            &format!(
                "control: aterm's task face never reads theirs at the control's point: open, told \
                 by codex-alab's continue@v1 (status task={control_face}; task={} before its turn)",
                task_face(&ours.before)
            ),
            control_face == "open",
        );
        Ok(())
    }

    /// Where the person's exchange's codex-alab stood before the live
    /// upgrade ([`Self::exchange_stays_left`]): its proven pid, its record's
    /// `execGeneration`, and the resumes its journal names ([`resumes`]).
    fn exchange_before_upgrade(&self, person: &Leg) -> Option<(i32, i64, usize)> {
        let pid = self.proven_pid(&person.sid)?;
        let generation = Self::record(person, pid).map_or(0, |r| exec_generation(&r));
        Some((pid, generation, resumes(&Self::codex_log(person))))
    }

    /// THE PERSON'S EXCHANGE AT THE RUN'S END, past the live upgrade: the
    /// re-link moves every running codex-alab, the exchange's too (`was`,
    /// [`Self::exchange_before_upgrade`]), and the image it re-execs into
    /// resumes the conversation and decides its point again, the person's
    /// own exchange; it must leave it to them as the image before did.
    fn exchange_stays_left(
        &mut self,
        person: &Leg,
        was: Option<(i32, i64, usize)>,
    ) -> Result<(), Stopped> {
        let rows = self.requests(person.name);
        let Some((reply, _)) = person_reply(person.name, &rows) else {
            self.check(
                "exchange: a successful reply write was observed before the final no-typing check",
                false,
            );
            return Ok(());
        };
        let Some((pid, generation, resumed)) = was else {
            self.check(
                "exchange: a proven codex-alab held the person's exchange before the upgrade",
                false,
            );
            return Ok(());
        };
        let mut now_at = generation;
        let moved = Self::wait_for(Duration::from_secs(150), Duration::from_secs(1), || {
            now_at = Self::record(person, pid).map_or(now_at, |r| exec_generation(&r));
            now_at > generation
        })?;
        self.check(
            &format!(
                "exchange: the upgrade re-exec'd the exchange's codex-alab too (pid {pid}, \
                 execGeneration {generation}->{now_at})"
            ),
            moved,
        );
        let mut decided = None;
        if moved {
            Self::wait_for(Duration::from_secs(90), Duration::from_secs(1), || {
                decided = decided_after_resume(&Self::codex_log(person), resumed);
                decided.is_some()
            })?;
        }
        let left = decided.as_deref().is_some_and(|d| {
            d.split_once(": ")
                .is_some_and(|(_, what)| what.starts_with("nothing ("))
        });
        self.check(
            &format!(
                "exchange: the new image, resumed into the person's exchange, decided nothing \
                 there ({})",
                decided.as_deref().unwrap_or("no decision after its resume")
            ),
            left,
        );
        let mut face = String::new();
        Self::wait_for(Duration::from_secs(60), Duration::from_secs(1), || {
            face = task_face(&self.status(&person.sid));
            face == "theirs"
        })?;
        let proven = self.proven_pid(&person.sid);
        self.check(
            &format!(
                "exchange: aterm's task face still reads theirs past the upgrade, the point the \
                 person's own exchange (status task={face}; proven: {proven:?}; acts: {:?})",
                self.acts(&person.sid)
            ),
            face == "theirs" && proven.is_some(),
        );
        let rows = self.requests(person.name);
        let reply_still_written = person_reply(person.name, &rows);
        let asked: Vec<String> = reply_still_written
            .map(|(_, after)| after.iter().map(|r| str_of(r, "tag")).collect())
            .unwrap_or_default();
        let acts = self.acts(&person.sid);
        let typed = typed_acts(&acts);
        let journal = journal_typed(&Self::codex_log(person));
        self.check(
            &format!(
                "exchange: nothing typed into the person's exchange by the run's end, past the \
                 upgrade ({} since the reply; write still witnessed: {}; asked after it: {asked:?}; acts: {acts:?}; \
                 journal typed: {journal:?})",
                secs(codex_rig::monotonic_ms(self.api_clock) - reply),
                reply_still_written.is_some()
            ),
            reply_still_written.is_some() && asked.is_empty() && typed.is_empty() && journal.is_empty(),
        );
        Ok(())
    }

    /// Every Land leg as it stood before the live upgrade
    /// ([`Self::landed_stand_past_upgrade`]): where its codex-alab stood
    /// ([`Self::exchange_before_upgrade`]'s triple) once its face reads
    /// `landed` beside a proven claim, or what was read when it never did
    /// within a hang detector (a face and a claim are briefly unread around a
    /// re-exec). No leg is left out ([`landed_baselines`]): one the run
    /// could not read before the upgrade is a FAIL past it, never a check
    /// that silently did not run (parity-5 review, R2: reland's dropped out
    /// and the run still printed PASS).
    fn landed_before_upgrade<'l>(
        &self,
        legs: &[&'l Leg],
    ) -> Result<Vec<(&'l Leg, Baseline)>, Stopped> {
        landed_baselines(legs, |leg| {
            let mut read = Err(String::new());
            Self::wait_for(Duration::from_secs(60), Duration::from_secs(1), || {
                read = self.landed_baseline(leg);
                read.is_ok()
            })?;
            Ok(read)
        })
    }

    /// One Land leg's [`Baseline`] now.
    fn landed_baseline(&self, leg: &Leg) -> Baseline {
        let face = task_face(&self.status(&leg.sid));
        match (face == "landed")
            .then(|| self.exchange_before_upgrade(leg))
            .flatten()
        {
            Some(was) => Ok(was),
            None => Err(format!(
                "status task={face}; proven: {:?}",
                self.proven_pid(&leg.sid)
            )),
        }
    }

    /// A LANDING STANDS PAST THE LIVE UPGRADE (Autopilot Land: the
    /// re-exec carries it, and `upgrade@v1` tells no face): each landed leg's
    /// codex-alab is re-exec'd by the re-link, its new image resumes the
    /// conversation, and aterm's face for the tab still reads `landed`, with
    /// no act typed after its landing. Hang detectors: the re-exec (its
    /// record's `execGeneration` up), the new image's first decision, and a
    /// face told otherwise in between coming back.
    fn landed_stand_past_upgrade(&mut self, landed: &[(&Leg, Baseline)]) -> Result<(), Stopped> {
        for (leg, baseline) in landed {
            let (pid, generation, resumed) = match baseline {
                Ok(was) => was,
                Err(read) => {
                    self.check(
                        &format!(
                            "{}: aterm's task face still reads landed past the upgrade (before \
                             it, no landed face beside a proven codex-alab within 60 s: {read})",
                            leg.name
                        ),
                        false,
                    );
                    continue;
                }
            };
            let mut now_at = *generation;
            let moved = Self::wait_for(Duration::from_secs(150), Duration::from_secs(1), || {
                now_at = Self::record(leg, *pid).map_or(now_at, |r| exec_generation(&r));
                now_at > *generation
            })?;
            let mut decided = None;
            if moved {
                Self::wait_for(Duration::from_secs(90), Duration::from_secs(1), || {
                    decided = decided_after_resume(&Self::codex_log(leg), *resumed);
                    decided.is_some()
                })?;
            }
            let mut face = String::new();
            Self::wait_for(Duration::from_secs(60), Duration::from_secs(1), || {
                face = task_face(&self.status(&leg.sid));
                face == "landed"
            })?;
            let acts = self.acts(&leg.sid);
            let after: Vec<String> = acts
                .iter()
                .rposition(|a| a == LANDED)
                .map(|at| typed_acts(&acts[at + 1..]))
                .unwrap_or_default();
            self.check(
                &format!(
                    "{}: aterm's task face still reads landed past the upgrade (pid {pid}, \
                     execGeneration {generation}->{now_at}; the new image: {}; status task={face}; \
                     typed after the landing: {after:?})",
                    leg.name,
                    decided.as_deref().unwrap_or("no decision after its resume")
                ),
                moved && face == "landed" && after.is_empty(),
            );
        }
        Ok(())
    }

    /// What each leg's host and codex-alab said, after a failed check.
    fn diagnose(&self, legs: &[&Leg]) {
        for leg in legs {
            let host = self.host_lines(&leg.sid);
            for l in &host[host.len().saturating_sub(25)..] {
                println!(
                    "  host {} {}",
                    leg.name,
                    l.chars().take(260).collect::<String>()
                );
            }
            println!("  status {} {}", leg.name, self.status(&leg.sid));
            println!("  acts {} {}", leg.name, self.acts(&leg.sid).join(" "));
            let log = Self::codex_log(leg);
            let alab: Vec<&str> = log.lines().filter(|l| l.contains("codex_alab")).collect();
            for l in &alab[alab.len().saturating_sub(15)..] {
                println!(
                    "  codex {} {}",
                    leg.name,
                    l.chars().take(260).collect::<String>()
                );
            }
            println!("  model {} {}", leg.name, self.tags(leg.name).join(" "));
        }
    }

    fn end_all(&mut self) {
        codex_rig::end_all(self.inst.pid, &self.s);
        self.inst.stop();
        self.fabric.stop();
    }
}

/// `s` as one POSIX shell word.
fn sh_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}

/// The folder of a leg's `CODEX_HOME` its codex writes its TUI log to
/// (`log_dir`), the leg's codex-alab journal.
const JOURNAL_DIR: &str = "journal";

/// The codex-alab journal of the leg whose `CODEX_HOME` is `home`.
fn journal_path(home: &Path) -> PathBuf {
    home.join(JOURNAL_DIR).join("codex-tui.log")
}

/// A leg's `config.toml`: the lab's model provider, Codex's own updater,
/// analytics, feedback and telemetry off, the TUI's file log in its home
/// ([`JOURNAL_DIR`]), and the project trusted as spelled and as resolved.
fn codex_config(port: u16, proj: &Path, home: &Path, extra: &str) -> String {
    let p = proj.to_string_lossy();
    let mut trusted = vec![p.to_string()];
    if let Some(short) = p.strip_prefix("/private") {
        trusted.push(short.to_string());
    }
    let projects: String = trusted
        .iter()
        .map(|d| format!("[projects.\"{d}\"]\ntrust_level = \"trusted\"\n"))
        .collect();
    format!(
        "# aterm-lab codex-alab-proof: codex talks only to the lab's model on 127.0.0.1:{port}\n\
         model_provider = \"lab\"\nmodel = \"gpt-5.5\"\napproval_policy = \"on-request\"\n\
         sandbox_mode = \"workspace-write\"\ncheck_for_update_on_startup = false\n\
         cli_auth_credentials_store = \"file\"\nmcp_oauth_credentials_store = \"file\"\n\
         log_dir = \"{log}\"\n\n\
         [analytics]\nenabled = false\n\n[feedback]\nenabled = false\n\n\
         [otel]\nexporter = \"none\"\nmetrics_exporter = \"none\"\ntrace_exporter = \"none\"\n\n\
         [model_providers.lab]\nname = \"lab fake model\"\nbase_url = \"http://127.0.0.1:{port}/v1\"\n\
         wire_api = \"responses\"\nexperimental_bearer_token = \"{key}\"\nrequest_max_retries = 0\n\
         stream_max_retries = 0\nstream_idle_timeout_ms = 300000\n\n{projects}\n{extra}",
        log = home.join(JOURNAL_DIR).display(),
        key = crate::stub::LAB_CODEX_KEY,
    )
}

/// `codex-alab-proof [--package DIR] [--upgrade-package DIR] [--aterm PATH] [--keep]
/// [--policy-only|--upgrade-only]`.
/// # Errors
/// Usage, a run that could not start, or an interrupt.
pub fn cmd_codex_alab_proof(args: &[String]) -> Res {
    stop::install();
    let mut a = args.to_vec();
    let keep = instance::take_flag(&mut a, "--keep");
    let policy_only = instance::take_flag(&mut a, "--policy-only");
    let upgrade_only = instance::take_flag(&mut a, "--upgrade-only");
    let aterm = instance::take_value(&mut a, "--aterm")?;
    let given = instance::take_value(&mut a, "--package")?;
    let upgrade_given = instance::take_value(&mut a, "--upgrade-package")?;
    if policy_only && (upgrade_only || upgrade_given.is_some()) {
        return Err(Fail::usage(
            "codex-alab-proof: --policy-only cannot be combined with upgrade options",
        ));
    }
    if let Some(extra) = a.first() {
        return Err(Fail::usage(format!(
            "codex-alab-proof: unexpected '{extra}' ({LAB} help)"
        )));
    }
    let not_run = |why: String| Fail::exit(3, format!("NOT RUN: {why}"));
    let aterm = instance::resolve_aterm(aterm).map_err(not_run)?;
    let home = crate::paths::paths().need_store_home().map_err(not_run)?;
    let pkg = package(given.as_deref(), home).map_err(not_run)?;
    let upgrade_pkg = upgrade_given
        .as_deref()
        .map(|given| package(Some(given), home))
        .transpose()
        .map_err(not_run)?;
    let build =
        std::fs::read_to_string(pkg.join(aterm_agent::harness::codex_alab::ALAB_BUILD_JSON))
            .ok()
            .and_then(|t| J::parse(&t).ok());
    let said = |k: &str| {
        build
            .as_ref()
            .and_then(|b| b.get(k).and_then(J::as_str).map(str::to_string))
            .unwrap_or_else(|| "?".to_string())
    };
    println!(
        "codex-alab {} (source {}) at {}; aterm {}",
        said("version"),
        said("source"),
        pkg.display(),
        aterm.display()
    );
    if let Some(upgrade_pkg) = &upgrade_pkg {
        upgrade_proof::describe_package(upgrade_pkg)?;
    } else if upgrade_only {
        println!("upgrade target: the same package bytes at another private path");
    }
    let mut world = World::new(
        "agents_auto_prime = false\n[update]\nenabled = false\nauto_apply = false\n\
         [packages]\nenabled = false\n[machine]\nuniversal_control = \"leave\"\n\
         [harness]\nenabled = true\nheadless = true\nhuman_grace_s = 5\n",
    )?;
    if keep {
        world.keep();
    }
    let s = world.dir.clone();
    let h = world.home.clone();
    let prefix = h.join("Library/Application Support/aterm/pkg");
    let api_clock = Instant::now();
    let api_log = Arc::new(std::sync::Mutex::new(codex_rig::ApiLog::new(
        s.join("api.log"),
        api_clock,
    )));
    world.keep();
    let made = (|| -> std::io::Result<u16> {
        for d in ["zdot", "legs"] {
            std::fs::create_dir_all(s.join(d))?;
        }
        codex_rig::stand_ins(&s.join("bin"))?;
        let listener = TcpListener::bind("127.0.0.1:0")?;
        let port = listener.local_addr()?.port();
        let log = Arc::clone(&api_log);
        std::thread::spawn(move || codex_rig::serve(listener, log, Arc::new(model::answer)));
        std::fs::write(
            s.join("zdot/.zshrc"),
            format!(
                "PS1='%# '\nunsetopt PROMPT_SP\nHISTFILE=/dev/null\n\
                 export PATH=\"{bin}:{agents}:/usr/bin:/bin:/usr/sbin:/sbin\"\n\
                 export RUST_LOG='codex_alab_autopilot=debug,codex_alab_aterm=debug,codex_alab_tui=debug,codex_tui=info'\n",
                bin = s.join("bin").display(),
                agents = prefix.join("agents").display(),
            ),
        )?;
        Ok(port)
    })();
    let port =
        made.map_err(|e| format!("codex-alab-proof: cannot lay out {}: {e}", s.display()))?;
    let fabric = Fabric::start(&aterm, &s.join("fabric"))
        .map_err(|e| format!("codex-alab-proof: cannot start the lab's bus: {e}"))?;
    {
        use std::io::Write as _;
        std::fs::OpenOptions::new()
            .append(true)
            .open(world.cfg.join("aterm/aterm.toml"))
            .and_then(|mut f| f.write_all(fabric.table.as_bytes()))
            .map_err(|e| format!("codex-alab-proof: cannot name the bridge: {e}"))?;
    }
    let (pkg_a, pkg_b) = (s.join("pkg-a"), s.join("pkg-b"));
    for (from, to) in [
        (&pkg, &pkg_a),
        (upgrade_pkg.as_ref().unwrap_or(&pkg), &pkg_b),
    ] {
        let cloned = aterm_uds::spawnfd::command("/bin/cp")
            .arg("-cR")
            .arg(from)
            .arg(to)
            .stdin(Stdio::null())
            .status()
            .is_ok_and(|st| st.success());
        if !cloned {
            return Err(Fail::die(format!(
                "codex-alab-proof: cannot copy {} to {}",
                from.display(),
                to.display()
            )));
        }
    }
    let sandbox = policy(port, crate::paths::paths(), &s).map_err(Fail::die)?;
    let sstr = |sub: &str| s.join(sub).to_string_lossy().into_owned();
    let env: Vec<(String, String)> = [
        (
            "PATH",
            format!("{}:/usr/bin:/bin:/usr/sbin:/sbin", sstr("bin")),
        ),
        ("SHELL", "/bin/zsh".to_string()),
        ("HOME", h.to_string_lossy().into_owned()),
        ("XDG_CONFIG_HOME", world.cfg.to_string_lossy().into_owned()),
        ("XDG_RUNTIME_DIR", world.run.to_string_lossy().into_owned()),
        ("ZDOTDIR", sstr("zdot")),
        ("TERM", "xterm-256color".to_string()),
        ("LANG", "en_US.UTF-8".to_string()),
    ]
    .map(|(k, v)| (k.to_string(), v))
    .to_vec();
    let world_env = {
        let mut e = world.env();
        e.retain(|(k, _)| k != "PATH");
        e.push(("PATH".into(), "/usr/bin:/bin:/usr/sbin:/sbin".into()));
        e
    };
    let inst = Instance::start(
        &world,
        Launch {
            aterm,
            headless: true,
            args: ["--columns", "120", "--lines", "40"]
                .map(str::to_string)
                .to_vec(),
            env,
            cwd: s.clone(),
            sandbox: Some(sandbox),
        },
    )?;
    let mut proof = Proof {
        inst,
        s,
        h,
        world_env,
        api_log,
        api_clock,
        port,
        fails: Vec::new(),
        fabric,
    };
    let outcome = if upgrade_only {
        upgrade_proof::legs(&mut proof, &pkg_a, &pkg_b)
    } else if policy_only {
        policy_proof::legs(&mut proof, &pkg_a)
    } else {
        legs(&mut proof, &pkg_a, &pkg_b)
    };
    proof.end_all();
    if let Err(f) = outcome
        && (f.stopped.is_some() || proof.fails.is_empty())
    {
        return Err(f);
    }
    if proof.fails.is_empty() {
        println!("PASS");
        Ok(0)
    } else {
        println!("FAIL: {} check(s)", proof.fails.len());
        for f in &proof.fails {
            println!("  - {f}");
        }
        Ok(1)
    }
}

fn legs(proof: &mut Proof, pkg: &Path, pkg_b: &Path) -> Result<(), Fail> {
    // The front door lays agents/codex and the stable exec path before any
    if let Err(why) = proof.link(pkg) {
        proof.check(&format!("the package is linked ({why})"), false);
        return Ok(());
    }
    if let Err(f) = proof.inst.await_socket(instance::BOOT) {
        proof.check("the headless aterm answers its socket", false);
        return Err(f);
    }
    let _ = proof.inst.ctl(&["ready"]);
    let began = Instant::now();
    let bus = proof.bus_up()?;
    let manager = {
        let spawned = proof
            .inst
            .ctl(&["spawn", &format!("cwd={}", proof.s.display())]);
        spawned
            .first()
            .strip_prefix("OK ")
            .and_then(|r| r.split_whitespace().next())
            .map(str::to_string)
    };
    proof.check(
        &format!(
            "the manager's tab ({})",
            manager.as_deref().unwrap_or("not spawned")
        ),
        manager.is_some(),
    );
    println!(
        "legs (one instance, each a tab; the model answers by the leg its prompt names and the act aterm's words name)"
    );
    let first = proof.spawn_leg("first", "Say hello and finish.", "", Folder::Scratch, None)?;
    let land = proof.spawn_leg("land", "Land the work.", "", Folder::Plain, Some("land"))?;
    let reland = proof.spawn_leg("reland", "Land the work.", "", Folder::Plain, Some("land"))?;
    let repo = proof.spawn_leg(
        "repo",
        "Write the release notes.",
        "",
        Folder::Origin,
        Some("land"),
    )?;
    let hold = proof.spawn_leg(
        "hold",
        "Write a plan, then stop.",
        "",
        Folder::Scratch,
        None,
    )?;
    let hold_pid = proof.claim(&hold)?;
    let on = proof.inst.ctl(&["hold", &hold.sid, "on", "reason=lab"]);
    proof.check(
        &format!("hold: put on while its first turn runs ({})", on.first()),
        on.first().contains("hold=1"),
    );
    let off = proof.spawn_leg("off", "Write a plan, then stop.", "", Folder::Scratch, None)?;
    let needs = proof.spawn_leg("needs", "Build the release.", "", Folder::Scratch, None)?;
    let cont = proof.spawn_leg(
        "continue",
        "Write a plan, then stop.",
        "",
        Folder::Scratch,
        None,
    )?;
    let approval = proof.spawn_leg("approval", "Run the build.", "", Folder::Scratch, None)?;
    let question = proof.spawn_leg(
        "question",
        "Set up the database.",
        "[features]\ndefault_mode_request_user_input = true\n",
        Folder::Scratch,
        Some("land"),
    )?;
    let usage = proof.spawn_leg("usage", "Start the task.", "", Folder::Scratch, None)?;
    let carry = proof.spawn_leg("carry", "Do the long task.", "", Folder::Scratch, None)?;
    let exchange = proof.start_leg("exchange", None, "", Folder::Scratch, None)?;
    let control = proof.start_leg("control", None, "", Folder::Scratch, None)?;
    let all = [
        &first, &land, &reland, &repo, &hold, &off, &needs, &cont, &approval, &question, &usage,
        &carry, &exchange, &control,
    ];
    let mut pids = vec![None; all.len()];
    for (i, leg) in all.iter().enumerate() {
        pids[i] = if leg.name == "hold" {
            hold_pid
        } else {
            proof.claim(leg)?
        };
    }
    let pid_of = |name: &str| {
        all.iter()
            .position(|leg| leg.name == name)
            .and_then(|i| pids[i])
    };
    let at_once = all
        .iter()
        .filter(|leg| proof.proven_pid(&leg.sid).is_some())
        .count();
    proof.check(
        &format!(
            "{at_once} codex-alab sessions in one aterm hold proven claims at once (each keeps a subscribe stream; {})",
            all.len()
        ),
        at_once == all.len(),
    );
    let off_at = proof.autopilot_off(&off)?;
    let typed = proof.type_exchanges(&exchange, &control)?;
    let timed = |proof: &Proof, leg: &str, from: Instant| {
        println!(
            "    ({leg}: checked {:.0} s after the socket, {:.0} s on this leg)",
            began.elapsed().as_secs_f64(),
            from.elapsed().as_secs_f64()
        );
        let _ = proof;
    };
    let t = Instant::now();
    proof.needs_leg(&needs)?;
    timed(proof, "needs", t);
    let t = Instant::now();
    proof.first_leg(&first)?;
    timed(proof, "first", t);
    let t = Instant::now();
    proof.crash_leg(&first, pid_of("first"))?;
    timed(proof, "crash", t);
    let t = Instant::now();
    proof.carry_leg(&carry, pid_of("carry"))?;
    timed(proof, "carry", t);
    let t = Instant::now();
    proof.continue_leg(&cont)?;
    timed(proof, "continue", t);
    let t = Instant::now();
    proof.approval_leg(&approval)?;
    timed(proof, "approval", t);
    let t = Instant::now();
    proof.question_leg(&question)?;
    timed(proof, "question", t);
    let t = Instant::now();
    proof.rollout_leg(&[&first, &cont, &approval, &question])?;
    timed(proof, "rollout", t);
    let t = Instant::now();
    match (bus, manager.as_deref()) {
        (true, Some(manager)) => proof.autopilot_off_leg(&off, manager, off_at)?,
        _ => {
            proof.check(
                "autopilot: the Autopilot Off step needs the lab's bus and the manager's tab",
                false,
            );
        }
    }
    timed(proof, "Autopilot Off", t);
    let t = Instant::now();
    proof.hold_leg(&hold, hold_pid)?;
    timed(proof, "hold", t);
    let t = Instant::now();
    proof.usage_leg(&usage)?;
    timed(proof, "usage", t);
    let t = Instant::now();
    proof.repo_leg(&repo)?;
    timed(proof, "repo (committed)", t);
    let t = Instant::now();
    let reopened = proof.reland_landed(&reland)?;
    timed(proof, "reland (landed, quiet, reopened)", t);
    if reopened {
        let t = Instant::now();
        proof.reland_again(&reland)?;
        timed(proof, "reland (landed again)", t);
    }
    let t = Instant::now();
    proof.exchange_leg(&exchange, &control, &typed)?;
    timed(proof, "exchange", t);
    let t = Instant::now();
    proof.repo_pushed(&repo)?;
    timed(proof, "repo (pushed)", t);
    let t = Instant::now();
    proof.land_leg(&land)?;
    timed(proof, "land", t);
    proof.needs_cleared(&needs)?;
    proof.landed_stay_quiet(&[&land, &reland, &repo]);
    proof.cruise_rests_never(&[
        &first, &needs, &cont, &approval, &hold, &off, &usage, &carry, &exchange, &control,
    ]);
    let exchange_was = proof.exchange_before_upgrade(&exchange);
    let landed_was = proof.landed_before_upgrade(&[&land, &reland, &repo])?;
    let t = Instant::now();
    proof.upgrade_leg(&cont, pkg_b)?;
    timed(proof, "upgrade", t);
    if typed.0.is_some() {
        let t = Instant::now();
        proof.exchange_stays_left(&exchange, exchange_was)?;
        timed(proof, "exchange (past the upgrade)", t);
    }
    let t = Instant::now();
    proof.landed_stand_past_upgrade(&landed_was)?;
    timed(proof, "landed (past the upgrade)", t);
    if !proof.fails.is_empty() {
        proof.diagnose(&all);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// What a child under the profile `profile` (none: unsandboxed) reaches:
    /// for each attempt of [`FENCE_SCRIPT`], `ok` or `no`.
    fn reached(profile: &str, tag: &str, a: &Path, s: &Path, w: &Path) -> Vec<String> {
        let mut cmd = aterm_uds::spawnfd::command("/usr/bin/sandbox-exec");
        cmd.args(["-p", profile, "/bin/sh", "-c", FENCE_SCRIPT])
            .env_clear()
            .env("PATH", "/usr/bin:/bin")
            .env("A", a)
            .env("S", s)
            .env("W", w)
            .env("TAG", tag)
            .stdin(Stdio::null());
        let out = aterm_uds::deadline::outputs_until(
            &mut [&mut cmd],
            Instant::now() + Duration::from_secs(60),
            Duration::from_secs(60),
        )
        .pop()
        .expect("one child")
        .expect("the child ran");
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8_lossy(&out.stdout)
            .lines()
            .map(str::to_string)
            .collect()
    }

    /// Each attempt a line: a write into the account's home, into the scratch
    /// `HOME`, a read of each one's `.ssh`, of the account's package store, of
    /// a plain file of the account's home, and a write into the proof's scratch.
    const FENCE_SCRIPT: &str = r#"try() { if "$@" >/dev/null 2>&1; then echo ok; else echo no; fi; }
try sh -c 'echo x > "$1/new-$2"' _ "$A" "$TAG"
try sh -c 'echo x > "$1/new-$2"' _ "$S" "$TAG"
try cat "$A/.ssh/id"
try cat "$S/.ssh/id"
try cat "$A/Library/Application Support/aterm/pkg/store/codex/1/bin/codex"
try cat "$A/plain"
try sh -c 'echo x > "$1/new-$2"' _ "$W" "$TAG"
"#;

    /// THE PROOF'S SANDBOX FENCES THE OWNER'S HOMES, APPLIED: a child under
    /// the profile [`policy`] makes from the lab's facts with a fake account
    /// home (handed in through [`crate::paths::Paths::resolve_with`]) and a
    /// scratch `HOME` writes into neither home and reads neither's `.ssh` nor
    /// the account's package store, while it reads a plain file of the account
    /// (the aterm under test lives in a checkout there) and writes the proof's
    /// scratch.
    #[test]
    fn the_proofs_sandbox_fences_the_account_home_and_a_scratch_home() {
        let top = aterm_tempfile::Builder::new()
            .prefix("lab-alab-fence-")
            .tempdir()
            .unwrap();
        let real = std::fs::canonicalize(top.path()).unwrap();
        let (account, scratch, world) =
            (real.join("account"), real.join("scratch"), real.join("w"));
        for h in [&account, &scratch] {
            for f in [
                ".ssh/id",
                "Library/Application Support/aterm/pkg/store/codex/1/bin/codex",
                "plain",
            ] {
                let path = h.join(f);
                std::fs::create_dir_all(path.parent().unwrap()).unwrap();
                std::fs::write(&path, "owned replica\n").unwrap();
            }
        }
        std::fs::create_dir_all(&world).unwrap();
        let p =
            crate::paths::Paths::resolve_with(Some(account.clone()), Some(scratch.clone().into()));
        let fenced = policy(4242, &p, &world).unwrap();
        assert!(
            fenced.starts_with(&codex_rig::jail_policy(4242)),
            "the network rule stays"
        );
        assert_eq!(
            reached(&fenced, "fenced", &account, &scratch, &world),
            ["no", "no", "no", "no", "no", "ok", "ok"]
        );
        assert!(!account.join("new-fenced").exists() && !scratch.join("new-fenced").exists());
        assert_eq!(
            reached(
                &codex_rig::jail_policy(4242),
                "control",
                &account,
                &scratch,
                &world
            ),
            ["ok"; 7]
        );
        assert!(policy(4242, &p, &account.join("w")).is_err());
        assert!(policy(4242, &p, &scratch.join("w")).is_err());
    }

    /// THE CARRY LEG'S HOLDER CHECK FROM THE SESSION LOG (phase 6, 6F-7):
    /// the act mark `harness-turn@v1` writes names its holder, as the
    /// timeline's row does.
    #[test]
    fn the_relaunched_holders_act_is_read_from_the_log() {
        let mark = "OK log ep=3\n41 mark t2 act word=dispatch rule=harness-turn.v1 \
                    holder=codex-alab@4242 harness_act=harness-turn@v1 by=s-abc src=mark \
                    conf=claim t=9\n";
        assert!(acted_in_log(mark, "harness-turn@v1", "codex-alab@4242"));
        assert!(
            !acted_in_log(
                &mark.replace(" harness_act=harness-turn@v1", ""),
                "harness-turn@v1",
                "codex-alab@4242"
            ),
            "a mark with no harness_act="
        );
        let unworded = "OK log ep=3\n42 mark t2 act word=unknown holder=codex-alab@4242 \
                        harness_act=finished@v1 by=s-abc src=mark conf=claim t=9\n";
        assert!(acted_in_log(unworded, "finished@v1", "codex-alab@4242"));
        assert!(!acted_in_log(unworded, "finished@v2", "codex-alab@4242"));
        assert!(
            !acted_in_log(
                &mark.replace(" holder=codex-alab@4242", ""),
                "harness-turn@v1",
                "codex-alab@4242"
            ),
            "a mark with no holder="
        );
        assert!(!acted_in_log(mark, "harness-turn@v1", "codex-alab@1"));
        assert!(!acted_in_log(mark, "land@v1", "codex-alab@4242"));
        assert!(!acted_in_log(
            "OK 7 t=1 kind=harness-act act=harness-turn@v1 holder=codex-alab@4242\n",
            "harness-turn@v1",
            "codex-alab@4242"
        ));
    }

    /// THE VERDICT READERS, on the lines aterm answers: the needs entry by
    /// its kind and owner, the task face, the nudges the model was asked at,
    /// a request's time by its tag.
    #[test]
    fn the_verdicts_read_aterm_s_own_lines() {
        let raised = "OK title=- attention=Codex%20needs%20you attention_owner=needs \
                      attention_owners=2 attention_kinds=ask:codex-alab,ask:needs supervisor=-";
        assert!(needs_raised(raised));
        assert!(
            needs_raised("OK attention_kinds=ask%3Aneeds"),
            "pct-encoded too"
        );
        assert!(
            !needs_raised("OK attention_kinds=ask:codex-alab"),
            "another owner's ask"
        );
        assert!(
            !needs_raised("OK attention_kinds=wait:needs"),
            "a wait is no need"
        );
        assert!(!needs_raised("OK attention_kinds=-"));
        assert_eq!(task_face("OK x=1 task=landed y=2"), "landed");
        assert_eq!(
            task_face("OK x=1 task_state=done"),
            "-",
            "task_state= is no face"
        );
        assert_eq!(
            nudges_seen(&[
                "land:person".to_string(),
                "land:land".to_string(),
                "land:nudge1".to_string(),
                "land:nudge2+ack".to_string(),
                "land:nudge3".to_string(),
            ]),
            vec![1, 2, 3]
        );
        assert!(nudges_seen(&["land:land".to_string()]).is_empty());
        let acts = |a: &[&str]| a.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert_eq!(
            nudges_before_landing(&acts(&[
                "land@v1",
                "land-nudge@v1",
                "land-nudge@v1",
                "landed@v1",
                "land-nudge@v1",
            ])),
            2,
            "a nudge after the landing is none before it"
        );
        assert_eq!(
            nudges_before_landing(&acts(&["land@v1", "land-nudge@v1"])),
            1,
            "no landing: every nudge"
        );
        assert_eq!(
            nudges_before_landing(&acts(&["land@v1", "landed@v1", "land-nudge@v1"])),
            0
        );
        let rows = vec![
            J::obj([("t", J::Int(10)), ("tag", J::s("first:person"))]),
            J::obj([("t", J::Int(130_010)), ("tag", J::s("first:defend"))]),
        ];
        assert_eq!(first_at(&rows, "first:defend"), Some(130_010));
        assert_eq!(first_at(&rows, "first:nudge"), None);
        assert_eq!(secs(130_000), "130.0 s");
    }

    /// A successful write of this exact reply must be observed before the
    /// quiet clock starts.
    #[test]
    fn a_continuation_is_quiet_through_its_own_reply_window() {
        let row = |id: i64, at: i64, tag: &str| {
            J::obj([
                ("request_id", J::Int(id)),
                ("mono_ms", J::Int(at)),
                ("t", J::Int(i64::MAX)),
                ("tag", J::s(tag)),
                ("hold_ms", J::Int(0)),
                ("status", J::Int(200)),
            ])
        };
        let response = |id, at, status| {
            J::obj([
                ("event", J::s("response_written")),
                ("request_id", J::Int(id)),
                ("mono_ms", J::Int(at)),
                ("status", J::Int(status)),
            ])
        };
        let asked = vec![
            row(1, 0, "continue:person"),
            row(2, 120_000, "continue:continue"),
        ];
        let sent = response(2, 190_000, 200);
        let initial = with_replies(asked.clone(), std::slice::from_ref(&sent));
        let seen = observe_reply(&initial, "continue:continue", 200_000);
        assert!(seen.is_some());
        let until = 205_000;
        let mut later = initial.clone();
        later.push(row(3, 370_000, "continue:defend+repo"));
        assert_eq!(continued_quiet(&later, seen, 500_000), Ok(()));
        assert_eq!(continued_quiet(&initial, seen, until), Ok(()));
        for extra_at in [191_000, until] {
            let mut spin = initial.clone();
            spin.push(row(3, extra_at, "continue:defend+repo"));
            assert!(continued_quiet(&spin, seen, 500_000).is_err(), "{extra_at}");
        }
        assert!(continued_quiet(&initial, seen, until - 1).is_err());
        assert!(continued_quiet(&initial[..1], seen, 500_000).is_err());
        assert_eq!(observe_reply(&initial, "continue:continue", 189_999), None);
        for replies in [
            vec![],
            vec![response(3, 190_000, 200)],
            vec![response(2, 190_000, 429)],
            vec![response(2, 119_999, 200)],
            vec![sent.clone(), sent],
        ] {
            let rows = with_replies(asked.clone(), &replies);
            let seen = observe_reply(&rows, "continue:continue", 500_000);
            assert_eq!(seen, None);
            assert!(continued_quiet(&rows, seen, i64::MAX).is_err());
        }
        let mut retried = asked;
        retried.push(row(3, 195_000, "continue:continue"));
        let rows = with_replies(retried, &[response(3, 196_000, 200)]);
        assert_eq!(observe_reply(&rows, "continue:continue", 500_000), None);
        let duplicate = vec![
            row(2, 120_000, "continue:continue"),
            row(2, 120_000, "continue:continue"),
        ];
        let rows = with_replies(duplicate, &[response(2, 190_000, 200)]);
        assert_eq!(observe_reply(&rows, "continue:continue", 500_000), None);
    }

    /// THE CRUISE CHECK'S READER ([`claims_of`]): a done claim met by the
    /// defend prompt, pending inside its back-off, unmet by another act or by
    /// nothing past it.
    #[test]
    fn a_done_claim_is_met_only_by_the_defend_prompt_within_its_back_off() {
        let row = |t: i64, tag: &str, said: &str, hold: i64| {
            J::obj([
                ("t", J::Int(t)),
                ("tag", J::s(tag)),
                ("said", J::s(said)),
                ("hold_ms", J::Int(hold)),
                ("status", J::Int(200)),
            ])
        };
        let rows = vec![
            row(0, "first:person", model::FIRST_DONE, 0),
            row(120_000, "first:defend+repo", model::CRUISE_DEFENSE, 0),
        ];
        assert_eq!(
            claims_of("first", &rows, 200_000),
            Claims {
                met: 1,
                pending: 1,
                unmet: Vec::new()
            }
        );
        let late = claims_of("first", &rows, 120_000 + 240_000 + CLAIM_SLACK_MS + 1);
        assert_eq!(
            (late.met, late.pending, late.unmet.len()),
            (1, 0, 1),
            "{late:?}"
        );
        let other = vec![
            row(0, "first:person", model::FIRST_DONE, 0),
            row(120_000, "first:continue", "Kept going.", 0),
        ];
        assert_eq!(
            claims_of("first", &other, 130_000).unmet,
            vec!["first:person then first:continue".to_string()]
        );
        let worked = vec![
            row(0, "hold:person", model::STEP_ONE, 130_000),
            row(191_000, "hold:continue", model::CONTINUED_DONE, 0),
        ];
        let due = 191_000 + 120_000 + CLAIM_SLACK_MS;
        let c = claims_of("hold", &worked, due - 1);
        assert_eq!((c.pending, c.unmet.len()), (1, 0));
        assert_eq!(claims_of("hold", &worked, due + 1).unmet.len(), 1);
        let mut wall = row(0, "usage:limit", "", 0);
        wall.set("status", J::Int(429));
        let none = vec![
            wall,
            row(120_000, "usage:continue", "Resumed after the reset.", 0),
        ];
        assert_eq!(claims_of("usage", &none, i64::MAX), Claims::default());
        assert_eq!(
            (backoff_ms(0), backoff_ms(1), backoff_ms(3), backoff_ms(9)),
            (0, 120_000, 480_000, 3_600_000)
        );
        assert_eq!(tail("abcdef", 3), "def");
        assert_eq!(tail("ab", 3), "ab");
    }

    /// A LEG'S JOURNAL OUTLIVES EACH OF ITS PROCESSES: the file
    /// [`Proof::codex_log`] reads is the one the leg's config logs to, and it
    /// is not `<CODEX_HOME>/log/codex-tui.log`, the file codex removes at every
    /// start.
    #[test]
    fn a_legs_journal_is_the_file_its_config_logs_to_and_no_start_removes_it() {
        let home = Path::new("/s/legs/exchange/codex-home");
        let config = codex_config(1234, Path::new("/s/legs/exchange/proj"), home, "");
        let dir = config
            .lines()
            .find_map(|l| l.strip_prefix("log_dir = "))
            .map(|v| v.trim_matches('"'));
        assert_eq!(
            dir.map(|d| Path::new(d).join("codex-tui.log")),
            Some(journal_path(home))
        );
        let removed = home.join("log").join("codex-tui.log");
        assert_ne!(journal_path(home), removed);
        assert_eq!(home.join("log").join("codex-tui.log"), removed);
    }

    /// THE EXCHANGE LEGS' JUDGES ([`continued_after`], [`left_to_person`]):
    /// the control continued at once after its reply, the person's exchange
    /// quiet past the same reply for a minute and past the control's latency.
    #[test]
    fn a_person_s_exchange_is_left_only_beside_a_control_that_was_continued() {
        let row = |t: i64, tag: &str, hold: i64| {
            J::obj([
                ("t", J::Int(t)),
                ("request_id", J::Int(t + 1)),
                ("mono_ms", J::Int(t)),
                ("reply_ms", J::Int(t + hold)),
                ("tag", J::s(tag)),
                ("hold_ms", J::Int(hold)),
                ("status", J::Int(200)),
            ])
        };
        let acts = |a: &[&str]| a.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        let reply = ms_of(SLOW_TURN);
        let control = vec![
            row(0, "control:person", reply),
            row(reply + 2_000, "control:continue", 0),
        ];
        assert_eq!(
            continued_after("control", &control, &acts(&["continue@v1"])),
            Ok(2_000)
        );
        assert!(continued_after("control", &control[..1], &[]).is_err());
        let defended = vec![
            row(0, "control:person", reply),
            row(reply + 2_000, "control:defend", 0),
        ];
        assert!(continued_after("control", &defended, &acts(&["continue-done-check@v1"])).is_err());
        let late = vec![
            row(0, "control:person", reply),
            row(reply + CONTROL_ACT_MS + 1, "control:continue", 0),
        ];
        assert!(continued_after("control", &late, &acts(&["continue@v1"])).is_err());
        assert!(continued_after("control", &control, &[]).is_err());
        assert!(continued_after("control", &[], &[]).is_err());
        let person = vec![row(0, "exchange:person", reply)];
        let seen = observe_reply(&person, "exchange:person", reply);
        let now = reply + 70_000;
        let held = left_to_person("exchange", &person, &[], seen, now, Some(2_000));
        assert!(
            held.as_ref().is_ok_and(|s| s.contains("70.0 s")),
            "{held:?}"
        );
        let typed = vec![
            row(0, "exchange:person", reply),
            row(reply + 2_000, "exchange:continue", 0),
        ];
        let why = left_to_person(
            "exchange",
            &typed,
            &acts(&["continue@v1"]),
            seen,
            now,
            Some(2_000),
        )
        .unwrap_err();
        assert!(
            why.contains("TYPED") && why.contains("exchange:continue"),
            "{why}"
        );
        assert!(
            left_to_person(
                "exchange",
                &person,
                &acts(&["answer@v1"]),
                seen,
                now,
                Some(2_000)
            )
            .is_err()
        );
        let left = acts(&[RULE_LEFT]);
        assert!(left_to_person("exchange", &person, &left, seen, now, Some(2_000)).is_ok());
        let after = acts(&[RULE_LEFT, "continue@v1"]);
        assert!(left_to_person("exchange", &person, &after, seen, now, Some(2_000)).is_err());
        let upgraded = acts(&[RULE_LEFT, RULE_UPGRADE]);
        assert!(left_to_person("exchange", &person, &upgraded, seen, now, Some(2_000)).is_ok());
        assert_eq!(
            typed_acts(&acts(&[RULE_UPGRADE, "continue@v1", RULE_LEFT])),
            acts(&["continue@v1"])
        );
        assert!(
            left_to_person("exchange", &person, &[], seen, reply + 59_000, Some(2_000)).is_err()
        );
        assert!(left_to_person("exchange", &person, &[], seen, now, Some(75_000)).is_err());
        assert!(left_to_person("exchange", &person, &[], seen, now, None).is_err());
        assert!(left_to_person("exchange", &[], &[], seen, now, Some(2_000)).is_err());
        assert!(left_to_person("exchange", &person, &[], None, i64::MAX, Some(2_000)).is_err());
        let late_seen = observe_reply(&person, "exchange:person", now);
        assert!(left_to_person("exchange", &person, &[], late_seen, now, Some(2_000)).is_ok());
        assert!(
            left_to_person(
                "exchange",
                &person,
                &[],
                late_seen,
                reply + 59_999,
                Some(2_000)
            )
            .is_err()
        );
        let before_reply = vec![
            row(0, "control:person", reply),
            row(reply - 1, "control:continue", 0),
        ];
        assert!(continued_after("control", &before_reply, &acts(&["continue@v1"])).is_err());
        let mut no_reply = person.clone();
        if let J::Obj(fields) = &mut no_reply[0] {
            fields.retain(|(k, _)| k != "reply_ms");
        }
        assert!(person_reply("exchange", &no_reply).is_none());
        assert!(left_to_person("exchange", &no_reply, &[], seen, i64::MAX, Some(2_000)).is_err());
    }

    /// THE JOURNAL AND THE HAND ([`exchange_rule`], [`journal_typed`],
    /// [`person_hand`]): the line naming the rule codex-alab left a point to
    /// a person by, the acts it says it typed, and `status`'s person's
    /// gesture.
    #[test]
    fn the_journal_names_the_rule_and_status_the_hand() {
        let left = "2026-10-21T10:00:00.000Z DEBUG codex_alab_autopilot: 019a-thread: nothing (a \
                    person's own exchange: theirs to answer)";
        assert_eq!(
            exchange_rule(left).as_deref(),
            Some("019a-thread: nothing (a person's own exchange: theirs to answer)")
        );
        assert!(
            exchange_rule("LEFT seq=4 rule=left-to-person@v1").is_some(),
            "aterm's id names it too"
        );
        let acted = "2026-10-21T10:00:00.000Z DEBUG codex_alab_autopilot: act continue@v1: typed \
                     \"keep going\" (continue after a plain turn; turn 7)\n\
                     2026-10-21T10:02:00.000Z DEBUG codex_alab_autopilot: 019a: wait 120s for \
                     continue-done-check@v1 (a done claim)";
        assert_eq!(exchange_rule(acted), None);
        assert_eq!(journal_typed(acted), vec!["continue@v1".to_string()]);
        assert!(journal_typed(left).is_empty());
        assert!(journal_typed("act landed@v1 not recorded: no proven claim is held").is_empty());
        assert!(journal_typed("exact continue@v1: typed").is_empty());
        assert!(journal_typed("the act after it: typed").is_empty());
        assert_eq!(
            person_hand("OK hold=0 human_ms=412 human_seq=3 task=-"),
            (Some(412), 3)
        );
        assert_eq!(
            person_hand("OK hold=0 human_ms=- human_seq=0"),
            (None, 0),
            "a control write stamps none"
        );
        assert_eq!(person_hand("OK hold=0"), (None, 0));
        assert_eq!(task_face("OK x=1 task=theirs pause=-"), "theirs");
    }

    /// THE PERSON'S ENTER IS JUDGED PAST THE SEND'S OWN STAMP
    /// ([`Typed::enter_stamped`]): `send as=person` stamps the person's
    /// clock itself, so the Enter is the person's only where `human_seq`
    /// moved past its count once the draft stood.
    #[test]
    fn the_persons_enter_is_judged_past_the_sends_own_stamp() {
        let typed = |before: &str, sent: &str, after: &str| Typed {
            pressed_ms: 0,
            before: format!("OK hold=0 {before} task=open"),
            sent: format!("OK hold=0 {sent} task=open"),
            after: format!("OK hold=0 {after} task=open"),
        };
        let theirs = typed(
            "human_ms=- human_seq=0",
            "human_ms=40 human_seq=1",
            "human_ms=3 human_seq=2",
        );
        assert_eq!(theirs.enter_stamped(), (true, [0, 1, 2]));
        assert!(!theirs.stamped_nothing());
        let unstamped = typed(
            "human_ms=- human_seq=0",
            "human_ms=40 human_seq=1",
            "human_ms=95 human_seq=1",
        );
        assert_eq!(
            unstamped.enter_stamped(),
            (false, [0, 1, 1]),
            "the send's stamp is no Enter's"
        );
        let ours = typed(
            "human_ms=- human_seq=0",
            "human_ms=- human_seq=0",
            "human_ms=- human_seq=0",
        );
        assert!(ours.stamped_nothing());
        assert_eq!(ours.enter_stamped(), (false, [0, 0, 0]));
        let stamped = typed(
            "human_ms=- human_seq=0",
            "human_ms=- human_seq=0",
            "human_ms=2 human_seq=1",
        );
        assert!(
            !stamped.stamped_nothing(),
            "a control write that stamped one"
        );
    }

    /// NO LAND LEG DROPS OUT OF THE PAST-UPGRADE CHECK (parity-5 review,
    /// R2): every leg named comes back, in order, with what was read of it,
    /// so a leg whose face never read `landed` beside a proven claim before
    /// the upgrade is judged (a FAIL past it) rather than left out while the
    /// run prints PASS. NEGATIVE CONTROL: a leg that settled keeps its
    /// reading. A stop while reading stops the whole read.
    #[test]
    fn no_land_leg_drops_out_of_the_past_upgrade_check() {
        let leg = |name: &'static str| Leg {
            name,
            sid: format!("s-{name}"),
            home: PathBuf::from("/nonexistent/home"),
            proj: PathBuf::from("/nonexistent/proj"),
        };
        let (land, reland, repo) = (leg("land"), leg("reland"), leg("repo"));
        let read = landed_baselines(&[&land, &reland, &repo], |leg| {
            Ok(if leg.name == "reland" {
                Err("status task=-; proven: None".to_string())
            } else {
                Ok(leg.name.len())
            })
        });
        let Ok(read) = read else {
            panic!("no stop was asked for");
        };
        let names: Vec<&str> = read.iter().map(|(leg, _)| leg.name).collect();
        assert_eq!(names, ["land", "reland", "repo"], "none left out");
        assert_eq!(read[0].1, Ok(4));
        assert_eq!(read[1].1, Err("status task=-; proven: None".to_string()));
        assert_eq!(read[2].1, Ok(4));
        let stopped = landed_baselines(&[&land, &reland], |_| {
            Err::<Result<(), String>, _>(Stopped(2))
        });
        assert!(stopped.is_err(), "a stop ends the read");
    }

    /// THE NEW IMAGE'S FIRST DECISION ([`resumes`], [`decided_after_resume`]):
    /// a live upgrade's re-exec resumes the conversation, and its first
    /// decision is the first deciding line after that resume — past a
    /// `passive` line of an image not yet proven again, never one of the
    /// image before.
    #[test]
    fn a_resumed_images_first_decision_is_read_after_its_resume() {
        let before = "2026-10-21T10:00:00.000Z DEBUG codex_alab_autopilot: 019a: resumed \
                      (task=none, relaunched=false, owed=none)\n\
                      2026-10-21T10:00:01.000Z DEBUG codex_alab_autopilot: 019a: nothing (a \
                      person's own exchange: theirs to answer)\n\
                      2026-10-21T10:00:02.000Z DEBUG codex_core: 019a: resumed (task=x)\n\
                      2026-10-21T10:09:00.000Z DEBUG codex_alab_autopilot: act upgrade@v1: \
                      re-exec onto a new build at rest\n";
        assert_eq!(resumes(before), 1, "codex_core's line is no resume");
        assert_eq!(decided_after_resume(before, 1), None, "not resumed yet");
        assert_eq!(
            decided_after_resume(before, 0).as_deref(),
            Some("019a: nothing (a person's own exchange: theirs to answer)"),
            "the image before's own decision"
        );
        let after = format!(
            "{before}2026-10-21T10:09:03.000Z DEBUG codex_alab_autopilot: 019a: resumed \
             (task=open, relaunched=false, owed=none)\n\
             2026-10-21T10:09:03.100Z DEBUG codex_alab_autopilot: 019a: passive (no proven \
             claim)\n"
        );
        assert_eq!(resumes(&after), 2);
        assert_eq!(
            decided_after_resume(&after, 1),
            None,
            "a passive image decided nothing"
        );
        let left = format!(
            "{after}2026-10-21T10:09:04.000Z DEBUG codex_alab_autopilot: 019a: nothing (a \
             person's own exchange: theirs to answer)\n"
        );
        assert_eq!(
            decided_after_resume(&left, 1).as_deref(),
            Some("019a: nothing (a person's own exchange: theirs to answer)")
        );
        let owed = format!(
            "{after}2026-10-21T10:09:04.000Z DEBUG codex_alab_autopilot: 019a: wait 120s for \
             continue@v1 (a plain turn)\n"
        );
        assert_eq!(
            decided_after_resume(&owed, 1).as_deref(),
            Some("019a: wait 120s for continue@v1 (a plain turn)")
        );
        let typed = format!(
            "{after}2026-10-21T10:09:04.000Z DEBUG codex_alab_autopilot: act continue@v1: typed \
             \"keep going\" (continue after a plain turn; turn 9)\n"
        );
        assert!(
            decided_after_resume(&typed, 1).is_some_and(|d| d.starts_with("act continue@v1")),
            "an act typed is a decision"
        );
        assert_eq!(decided_after_resume(&left, 2), None);
    }

    #[test]
    fn a_package_without_the_alab_mark_is_a_vendor_codex() {
        let scratch = aterm_tempfile::tempdir().expect("exclusive package fixture");
        let dir = scratch.path();
        std::fs::create_dir_all(dir.join("bin")).unwrap();
        std::fs::write(dir.join("bin/codex"), "").unwrap();
        std::fs::write(
            dir.join("codex-package.json"),
            r#"{"layoutVersion":1,"version":"0.164.0-dev.1+gabc","entrypoint":"bin/codex"}"#,
        )
        .unwrap();
        let bin = dir.join("bin/codex").to_string_lossy().into_owned();
        let vendor = package(Some(&bin), "/nonexistent").unwrap_err();
        assert!(vendor.contains("vendor Codex"), "{vendor}");
        std::fs::write(dir.join("alab-build.json"), "{}").unwrap();
        let root = package(Some(&bin), "/nonexistent").unwrap();
        assert_eq!(root, std::fs::canonicalize(dir).unwrap());
        assert!(package(Some("/nonexistent/codex"), "/nonexistent").is_err());
    }

    /// A tap stand-in for [`past_the_fence`]: each look after the first
    /// answers one more ask, and the planted write (a turn from the
    /// rollout, `src=transcript`) lands with the ask numbered `late`, before
    /// that ask is counted, as the host counts it; `stuck` answers none past
    /// the first look.
    struct Script {
        word: &'static str,
        asks: std::cell::Cell<u64>,
        looked: std::cell::Cell<bool>,
        late: Option<u64>,
        stuck: bool,
        log: std::cell::RefCell<Vec<String>>,
    }

    const LATE_TURN: &str =
        "12 open t1 turn par=r1 vref=turn-1 src=transcript conf=exact t=1760000001000";

    impl Script {
        fn new(word: &'static str, late: Option<u64>) -> Script {
            Script {
                word,
                asks: std::cell::Cell::new(40),
                looked: std::cell::Cell::new(false),
                late,
                stuck: false,
                log: std::cell::RefCell::new(vec![
                    "9 open t1 turn par=r1 src=screen conf=strong t=1760000000000".to_string(),
                ]),
            }
        }

        fn look(&self) -> Option<TapSaid> {
            if self.looked.replace(true) && !self.stuck {
                let next = self.asks.get() + 1;
                if self.late == Some(next) {
                    self.log.borrow_mut().push(LATE_TURN.to_string());
                }
                self.asks.set(next);
            }
            tap_said(&format!(
                "OK schema=1 need=- need_since_ms=- rollout={} rollout_asks={} editing=0",
                self.word,
                self.asks.get()
            ))
        }

        fn read(&self) -> Vec<String> {
            self.log.borrow().clone()
        }
    }

    /// The hang detector, by hand: at most 10 looks, no clock.
    fn ten_looks(ok: &mut dyn FnMut() -> bool) -> Result<bool, Stopped> {
        Ok((0..10).any(|_| ok()))
    }

    /// THE ROLLOUT LEG READS PAST THE TAP'S FENCE (B5c): the log is read
    /// only once the tap answered two asks past the first look, so a turn
    /// from the rollout that a tap writes late (with the second ask: the
    /// last that could still hold what the rollout held at the look) fails
    /// the unmeasured verdict, and the same tap with no late write passes
    /// it.
    #[test]
    fn the_rollout_leg_reads_past_the_taps_fence() {
        let verdict = |measured: bool, script: &Script| match past_the_fence(
            || script.look(),
            || script.read(),
            ten_looks,
        )
        .expect("not stopped")
        {
            Fenced::Past { from, to, got } => {
                assert_eq!(to.asks, from.asks + FENCE_ASKS, "read at the fence");
                Some(rollout_holds(measured, &to, &got))
            }
            Fenced::NoTap | Fenced::Stuck { .. } => None,
        };
        assert_eq!(verdict(false, &Script::new("unmeasured", None)), Some(true));
        let late = Script::new("unmeasured", Some(42));
        assert_eq!(verdict(false, &late), Some(false));
        assert!(late.read().iter().any(|l| l == LATE_TURN));
        let at_once = Script::new("unmeasured", Some(42));
        let first = at_once.look().expect("an answer");
        assert!(rollout_holds(false, &first, &at_once.read()));
        let mut stuck = Script::new("unmeasured", None);
        stuck.stuck = true;
        let fenced = past_the_fence(|| stuck.look(), || stuck.read(), ten_looks).expect("ran");
        assert!(
            matches!(fenced, Fenced::Stuck { ref from, ref to } if from.asks == 40 && to.asks == 40),
            "{fenced:?}"
        );
        assert_eq!(verdict(false, &Script::new("bound", None)), Some(false));
        assert_eq!(
            past_the_fence(|| None, Vec::<String>::new, ten_looks).expect("ran"),
            Fenced::NoTap
        );
        assert_eq!(tap_said("OK rollout=bound rollout_asks=x"), None);
        assert_eq!(verdict(true, &Script::new("bound", Some(41))), Some(true));
        assert_eq!(verdict(true, &Script::new("bound", None)), Some(false));
        assert_eq!(
            verdict(true, &Script::new("unmeasured", Some(41))),
            Some(false)
        );
    }
}
