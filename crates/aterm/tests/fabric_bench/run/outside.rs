// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE SHIPPED BROKER'S BAND (F02): each world's broker is this build's `aterm link broker`,
//! started with the argv its launchd job runs (`aterm_link::enable::Paths::broker_argv`) on the
//! world's own log, secret and key, as the task type a `ProcessType` `Background` job starts as
//! (`broker_process=background`: darwin-bg, every thread at priority 4) or an `Interactive` one
//! starts as (`broker_process=interactive`: 31 at the default class). Every other world runs
//! astream's `Broker` inside the bench at the bench's priority, which is no band that ships.
//!
//! Out of the bench's process the broker's atomics are not there to read, so these worlds report
//! the outside stamps alone, on the bench's one clock: a post's `OK` and its row in the
//! receiver's inbox (messaging), the ping-pong round trip back to back and paced, and a killed
//! bridge's replacement: its pid, its attach rows reaching a subscriber, and a live mail reaching
//! the inbox (attach), with a paced publisher on the broker timing each of its acks across the
//! restarts. Each world reads the base priority of the broker's every thread when it is ready and
//! again when its phases are done, and fails unless the arm's band holds both times: every
//! thread at 4 (background), the highest at 31 (interactive).

use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Instant;

use aterm_json::{Value, json};

use super::pingpong::{self, PACE};
use super::{
    PID_POLL, SUPERVISOR_HEALTHY, await_attach, await_note, backlog, list_since, newest_id, post,
    presence_subject, read_pid, spawn, wait_live, with_max,
};
use crate::clock::{Clock, ms};
use crate::harness::{self, DEADLINE, FLEET, SECRET, World, until};
use crate::spec::{BrokerProcess, Spec, Tier};
use crate::stats;

/// The base priority darwin-bg holds every thread of a background daemon at.
const BACKGROUND_PRIORITY: i32 = 4;

/// The base priority a thread at the default class runs at out of any band and clamp
/// (`BASEPRI_DEFAULT`).
const DEFAULT_PRIORITY: i32 = 31;

/// What the broker's start line on its stderr begins with (`aterm link broker`'s banner).
const BANNER: &str = "aterm link broker: unix socket on ";

/// One world's broker in a process of its own.
struct ShippedBroker {
    child: aterm_uds::spawnfd::Child,
    /// Held while it serves: taken, the broker ends itself at its next wake.
    lifeline: Option<aterm_uds::lifeline::Lifeline>,
    /// Its 0700 root: `bus.sock`, `bus.log`, `mint.secret`, `rest.key`, `broker.err`.
    root: aterm_tempfile::TempDir,
    sock: String,
    argv: Vec<String>,
    /// Its start line: the tier it serves, its rules, its limit, its residency, its seal.
    banner: String,
    process: BrokerProcess,
}

impl ShippedBroker {
    /// Start `aterm link broker` for `spec`'s arm under `tag` and wait for its `listening` line.
    fn start(tag: &str, spec: &Spec) -> Self {
        use std::os::unix::fs::PermissionsExt as _;
        let root = harness::sock_dir(tag);
        std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700))
            .expect("the broker's root is private");
        let paths = aterm_link::enable::Paths {
            home: root.path().to_path_buf(),
            login_home: None,
            root: root.path().to_path_buf(),
            aterm: harness::aterm_to_launch().to_string(),
            fleet: FLEET.to_string(),
            config: None,
            rendezvous: None,
            // What `aterm fabric on` names: no tier for the default, ordered at its horizon.
            durability: match spec.durability {
                Tier::Ordered => None,
                Tier::Strict => Some(aterm_link::durability::Durability::Strict),
                Tier::Relaxed => Some(aterm_link::durability::Durability::Relaxed),
            },
        };
        let secret = root.path().join(aterm_link::enable::MINT_SECRET);
        std::fs::write(&secret, SECRET).expect("write the mint secret");
        std::fs::set_permissions(&secret, std::fs::Permissions::from_mode(0o600))
            .expect("the secret is owner-only");
        let argv = paths.broker_argv();
        // The key goes beside the log the broker opens, read off its argv as it reads it.
        let log = aterm_link::enable::Paths::served_log(&argv)
            .unwrap_or_else(|| panic!("the shipped argv serves a log: {argv:?}"));
        if spec.at_rest {
            aterm_link::at_rest::mint(&log, aterm_uds::backup::Backups::INERT)
                .expect("mint the log's key")
                .expect("a fresh root holds no key")
                .expect("the key's backup mark");
        }
        let err = root.path().join("broker.err");
        let err_file = std::fs::File::create(&err).expect("broker.err");
        let (program, args) = argv.split_first().expect("a program word");
        let mut cmd = aterm_uds::spawnfd::command(program);
        harness::kept_from_runner(&mut cmd).args(args);
        harness::ship_durable_flushes(&mut cmd);
        match spec.broker_process {
            BrokerProcess::Background => cmd.enter_background_band(),
            BrokerProcess::Interactive => cmd.start_as_interactive_daemon(),
            BrokerProcess::Inproc => unreachable!("the in-process arm serves its own broker"),
        };
        cmd.stdout(aterm_uds::spawnfd::Stdio::piped())
            .stderr(err_file);
        let lifeline = aterm_uds::lifeline::Lifeline::arm(&mut cmd, root.path())
            .expect("arm the broker's lifeline");
        let mut child = cmd.spawn().expect("start `aterm link broker`");
        let sock = root.path().join("bus.sock").to_string_lossy().into_owned();
        let first = harness::first_line_within(&mut child, "the broker's `listening` line");
        assert_eq!(
            first.trim_end(),
            format!("listening {sock}"),
            "broker.err:\n{}",
            std::fs::read_to_string(&err).unwrap_or_default()
        );
        // A whole line: the broker writes its start line after `listening`, and a read can land
        // inside that write.
        let banner = until("the broker's start line on broker.err", || {
            std::fs::read_to_string(&err)
                .ok()?
                .split_inclusive('\n')
                .find(|l| l.starts_with(BANNER) && l.ends_with('\n'))
                .map(|l| l.trim_end().to_string())
        });
        Self {
            child,
            lifeline: Some(lifeline),
            root,
            sock,
            argv,
            banner,
            process: spec.broker_process,
        }
    }

    /// The base priority of each of the broker's threads now, held to the arm's band: every one
    /// at 4 in the background band, the highest at 31 out of it. Any other reading is the wrong
    /// arm, and the run fails rather than record it.
    fn band(&self, when: &str) -> Vec<i32> {
        let pid = i32::try_from(self.child.id()).expect("a pid");
        let read = priorities(pid)
            .filter(|p| !p.is_empty())
            .unwrap_or_else(|| panic!("the broker's threads ({pid}) read nothing {when}"));
        let holds = match self.process {
            BrokerProcess::Background => read.iter().all(|&p| p == BACKGROUND_PRIORITY),
            BrokerProcess::Interactive => read.iter().max() == Some(&DEFAULT_PRIORITY),
            BrokerProcess::Inproc => unreachable!("the in-process arm has no broker process"),
        };
        assert!(
            holds,
            "the broker started as the {} arm's task type reads {read:?} {when}: not that arm's \
             band ({}), so its numbers would be another band's",
            self.process.word(),
            match self.process {
                BrokerProcess::Background => "every thread at 4",
                _ => "the highest thread at 31",
            }
        );
        read
    }

    /// What this broker was, for the report.
    fn json(&self, start: &[i32], end: &[i32]) -> Value {
        json!({
            "process": self.process.word(),
            "root": self.root.path().to_string_lossy().into_owned(),
            "argv": self.argv.clone(),
            "banner": self.banner.clone(),
            "thread_base_priorities": { "ready": start.to_vec(), "done": end.to_vec() },
        })
    }

    /// Its line in the report: the task type asked of `posix_spawn`, and what the threads'
    /// priorities show of it, which is the band alone: an interactive daemon's threads read as a
    /// standard daemon's do, and as an untyped child's of the bench.
    fn line(&self, start: &[i32], end: &[i32]) -> String {
        let (asked, shown) = match self.process {
            BrokerProcess::Background => (
                "POSIX_SPAWN_PROC_TYPE_DAEMON_BACKGROUND",
                "every thread at 4: in the background band",
            ),
            _ => (
                "POSIX_SPAWN_PROC_TYPE_DAEMON_INTERACTIVE",
                "the highest at 31: out of the background band, which is all a priority shows of \
                 the type",
            ),
        };
        format!(
            "The world's broker is `aterm link broker` in a process of its own, started as {} \
             (`{asked}`), argv `{}`; its threads' base priorities read {} when it was ready and {} \
             when the phases were done ({shown}). Its start line: `{}`\n",
            self.process.runs_as(),
            self.argv.join(" "),
            spread(start),
            spread(end),
            self.banner.trim_start_matches("aterm link broker: ")
        )
    }
}

impl Drop for ShippedBroker {
    fn drop(&mut self) {
        self.lifeline.take();
        aterm_uds::deadline::kill_within(&mut self.child, DEADLINE);
    }
}

/// `pid`'s threads' base priorities ([`aterm_uds::qos::thread_base_priorities`]).
#[cfg(target_os = "macos")]
fn priorities(pid: i32) -> Option<Vec<i32>> {
    aterm_uds::qos::thread_base_priorities(pid)
}

/// No band to read off macOS, where `spec::parse` refuses these arms.
#[cfg(not(target_os = "macos"))]
fn priorities(_pid: i32) -> Option<Vec<i32>> {
    None
}

/// `4 x12 over 12 threads`, `20 x1, 31 x2 over 3 threads`: each priority read, lowest first,
/// with how many threads read it.
fn spread(read: &[i32]) -> String {
    let mut seen: std::collections::BTreeMap<i32, usize> = std::collections::BTreeMap::new();
    for &p in read {
        *seen.entry(p).or_default() += 1;
    }
    let parts: Vec<String> = seen.iter().map(|(p, n)| format!("{p} x{n}")).collect();
    format!("{} over {} threads", parts.join(", "), read.len())
}

/// One world booted on its own shipped broker, ready, with the broker's band read.
fn boot(tag: &str, spec: &Spec) -> (ShippedBroker, World, Vec<i32>) {
    let broker = ShippedBroker::start(&format!("{tag}-b"), spec);
    let w = World::boot_at(tag, &[], &[], Some(&broker.sock));
    w.wait_ready();
    let band = broker.band("once its world was ready");
    (broker, w, band)
}

/// The table head every section's rows sit under.
fn head(what: &str) -> [String; 2] {
    [
        format!("| {what} (ms) | n | mean | p50 | p95 | p99 | max |"),
        "|---|---:|---:|---:|---:|---:|---:|".to_string(),
    ]
}

fn row(what: &str, values: &[f64]) -> String {
    format!("| {what} | {} |", with_max(values))
}

/// The messaging world: `samples` posts from A to B one after the other, each waited for in B's
/// inbox before the next.
pub(super) fn messaging(spec: &Spec, clock: Clock) -> (Value, Vec<String>) {
    eprintln!(
        "fabric bench: booting the messaging world on a {} broker",
        spec.broker_process.word()
    );
    let (broker, w, ready) = boot("fbench-msg", spec);
    let (a, b) = w.two_sessions();
    wait_live(
        &w,
        &[a.clone(), b.clone()],
        0,
        "both sessions' presence rows to say live",
    );
    let mut ctl = w.ctl();
    let mut since = newest_id(&mut ctl, &b);
    let mut post_ok = Vec::with_capacity(spec.samples);
    let mut visible = Vec::with_capacity(spec.samples);
    eprintln!(
        "fabric bench: latency, {} round trips ({} warm-up)",
        spec.samples, spec.warmup
    );
    for i in 0..spec.warmup + spec.samples {
        let text = format!("lat-{i}");
        let (t_call, t_ret) = post(&mut ctl, clock, &a, &b, &text);
        let (woke_at, t_seen) = await_note(&mut ctl, clock, &b, since)
            .unwrap_or_else(|e| panic!("round trip {i}: {e}"));
        let rows = list_since(&mut ctl, &b, since);
        assert!(
            rows.len() == 1 && rows[0].text == text && rows[0].id == woke_at,
            "round trip {i}: woke at row {woke_at}, listed {rows:?}"
        );
        since = woke_at;
        if i >= spec.warmup {
            post_ok.push(ms(t_call, t_ret));
            visible.push(ms(t_call, t_seen));
        }
    }
    let done = broker.band("once the messaging phases were done");
    let mut md = vec![
        "## Messaging, on the broker's own process".to_string(),
        String::new(),
        broker.line(&ready, &done),
    ];
    md.extend(head("post A -> B, outside stamps"));
    md.push(row("post -> OK", &post_ok));
    md.push(row("post -> visible in B's inbox", &visible));
    md.push(String::new());
    let json = json!({
        "broker": broker.json(&ready, &done),
        "post_to_ok_ms": stats::to_json(&post_ok),
        "post_to_visible_ms": stats::to_json(&visible),
    });
    drop(w);
    drop(broker);
    (json, md)
}

/// The ping-pong world: A pings B and waits for B's pong, back to back (`pingpong`) and then
/// [`PACE`] apart (`paced`).
pub(super) fn pingpong(spec: &Spec, clock: Clock) -> (Value, Vec<String>) {
    eprintln!(
        "fabric bench: booting the ping-pong world on a {} broker",
        spec.broker_process.word()
    );
    let (broker, w, ready) = boot("fbench-pp", spec);
    let (a, b) = w.two_sessions();
    wait_live(
        &w,
        &[a.clone(), b.clone()],
        0,
        "both sessions' presence rows to say live",
    );
    let mut ctl = w.ctl();
    let mut a_since = newest_id(&mut ctl, &a);
    let mut b_since = newest_id(&mut ctl, &b);
    eprintln!(
        "fabric bench: ping-pong, {} rounds ({} warm-up), then {} paced {} ms apart",
        spec.pingpong,
        spec.warmup,
        spec.paced,
        PACE.as_millis()
    );
    let mut round =
        |i: usize| pingpong::one_round(&mut ctl, clock, &a, &b, i, &mut a_since, &mut b_since);
    let back_to_back: Vec<pingpong::Round> = (0..spec.warmup + spec.pingpong)
        .map(&mut round)
        .skip(spec.warmup)
        .collect();
    let first = spec.warmup + spec.pingpong;
    let paced: Vec<pingpong::Round> = (first..first + spec.paced)
        .map(|i| {
            std::thread::sleep(PACE);
            round(i)
        })
        .collect();
    let done = broker.band("once the ping-pong phases were done");
    let legs = |rounds: &[pingpong::Round]| {
        let trip: Vec<f64> = rounds.iter().map(|r| ms(r.t_call, r.t_a_seen)).collect();
        let ping: Vec<f64> = rounds.iter().map(|r| ms(r.t_call, r.t_b_seen)).collect();
        let pong: Vec<f64> = rounds.iter().map(|r| ms(r.t_b_ret, r.t_a_seen)).collect();
        (trip, ping, pong)
    };
    let (trip, ping, pong) = legs(&back_to_back);
    let (paced_trip, paced_ping, paced_pong) = legs(&paced);
    let mut md = vec![
        "## Ping-pong, on the broker's own process".to_string(),
        String::new(),
        broker.line(&ready, &done),
    ];
    md.extend(head("ping A -> B, pong B -> A, outside stamps"));
    md.push(row("round trip, back to back", &trip));
    md.push(row("ping post -> visible to B", &ping));
    md.push(row("pong OK -> visible to A", &pong));
    md.push(row(
        &format!("round trip, paced {} ms apart", PACE.as_millis()),
        &paced_trip,
    ));
    md.push(row("paced ping post -> visible to B", &paced_ping));
    md.push(row("paced pong OK -> visible to A", &paced_pong));
    md.push(String::new());
    let json = json!({
        "broker": broker.json(&ready, &done),
        "round_trip_ms": stats::to_json(&trip),
        "ping_post_to_visible_ms": stats::to_json(&ping),
        "pong_ok_to_visible_ms": stats::to_json(&pong),
        "pace_ms": u64::try_from(PACE.as_millis()).unwrap_or(u64::MAX),
        "paced_round_trip_ms": stats::to_json(&paced_trip),
        "paced_ping_post_to_visible_ms": stats::to_json(&paced_ping),
        "paced_pong_ok_to_visible_ms": stats::to_json(&paced_pong),
    });
    drop(w);
    drop(broker);
    (json, md)
}

/// One restart, as the outside saw it.
struct Restart {
    sessions: usize,
    t_kill: u64,
    t_pid: u64,
    /// When a subscriber on the presence faces had every row of the new bridge's attach.
    t_rows: u64,
    /// When the live mail posted right after the kill was in the inbox.
    t_mail: u64,
}

/// The attach world: for each `attach` population, `rounds` kills of the bridge, each timed to
/// its replacement's pid, to its attach rows reaching a subscriber, and to a live mail posted
/// right after the kill reaching the inbox; a paced publisher times its acks all the while.
pub(super) fn attach(spec: &Spec, clock: Clock) -> (Value, Vec<String>) {
    eprintln!(
        "fabric bench: booting the attach world on a {} broker",
        spec.broker_process.word()
    );
    let (broker, w, ready) = boot("fbench-att", spec);
    let first = until("aterm's boot session", || {
        w.sessions().first().map(|(_, sid, _)| sid.clone())
    });
    let node_subject = presence_subject(&w, "node");
    let presence_filter = format!("/f/{FLEET}/pub/{}/*/presence", w.node);
    let stop_steady = AtomicBool::new(false);
    let (restarts, beats) = std::thread::scope(|s| {
        let steady = s.spawn(|| backlog::steady(&w, clock, 1, &stop_steady));
        let mut mail_ctl = w.ctl();
        let mut mail_since = newest_id(&mut mail_ctl, &first);
        let mailer = first.clone();
        let mut sids = vec![first.clone()];
        let mut pid = w.bridge_pid();
        let mut up_since = Instant::now();
        let mut restarts = Vec::new();
        for &size in &spec.attach {
            while sids.len() < size {
                sids.push(spawn(&w));
            }
            wait_live(&w, &sids, 0, "every session's presence row to say live");
            let session_subjects: Vec<String> =
                sids.iter().map(|sid| presence_subject(&w, sid)).collect();
            eprintln!(
                "fabric bench: attach with {size} session(s), {} restart(s)",
                spec.rounds
            );
            for _ in 0..spec.rounds {
                if let Some(rest) = SUPERVISOR_HEALTHY.checked_sub(up_since.elapsed()) {
                    std::thread::sleep(rest);
                }
                let mut god = w.god();
                let (_, mark) = god
                    .oldest(&presence_filter)
                    .unwrap_or_else(|e| panic!("the head before restart: {e}"));
                let mut watch = god
                    .subscribe(mark, &presence_filter)
                    .unwrap_or_else(|e| panic!("subscribe to {presence_filter} from {mark}: {e}"));
                watch
                    .set_read_timeout(Some(DEADLINE))
                    .expect("bound the presence subscription");
                let t_kill = clock.us();
                harness::kill(pid, 9);
                let new_pid = {
                    let (world, killed) = (&w, pid);
                    s.spawn(move || {
                        let new_pid = aterm_spec::wait::within(DEADLINE, PID_POLL, || {
                            read_pid(world).filter(|p| *p != killed)
                        });
                        (new_pid, clock.us())
                    })
                };
                let mail = format!("live-{}", restarts.len());
                post(&mut mail_ctl, clock, &mailer, &mailer, &mail);
                let (new_pid, t_pid) = new_pid.join().expect("the new pid's watcher");
                let new_pid = new_pid
                    .unwrap_or_else(|| panic!("the bridge did not come back within {DEADLINE:?}"));
                pid = new_pid;
                up_since = Instant::now();
                let rows = {
                    let (node, sessions) = (node_subject.clone(), session_subjects.clone());
                    s.spawn(move || {
                        await_attach(&mut watch, &node, &sessions, new_pid);
                        clock.us()
                    })
                };
                let (woke_at, t_mail) = await_note(&mut mail_ctl, clock, &mailer, mail_since)
                    .unwrap_or_else(|e| panic!("the live mail `{mail}`: {e}"));
                let listed = list_since(&mut mail_ctl, &mailer, mail_since);
                assert!(
                    listed.len() == 1 && listed[0].text == mail && listed[0].id == woke_at,
                    "the live mail `{mail}`: woke at row {woke_at}, listed {listed:?}"
                );
                mail_since = woke_at;
                let t_rows = rows.join().expect("the attach's subscriber");
                restarts.push(Restart {
                    sessions: size,
                    t_kill,
                    t_pid,
                    t_rows,
                    t_mail,
                });
            }
        }
        stop_steady.store(true, Ordering::Relaxed);
        (restarts, steady.join().expect("the paced publisher"))
    });
    let done = broker.band("once the attach phases were done");
    let windows: Vec<(u64, u64)> = restarts
        .iter()
        .map(|r| (r.t_kill, r.t_rows.max(r.t_mail)))
        .collect();
    let (inside, outside) = backlog::split(&beats, &windows);
    let every: Vec<f64> = beats.iter().map(|b| b.ack_ms()).collect();
    let mut md = vec![
        "## Attach, on the broker's own process".to_string(),
        String::new(),
        broker.line(&ready, &done),
    ];
    let mut by_size = Vec::new();
    for &size in &spec.attach {
        let these: Vec<&Restart> = restarts.iter().filter(|r| r.sessions == size).collect();
        let series = |f: fn(&Restart) -> f64| these.iter().map(|r| f(r)).collect::<Vec<f64>>();
        let to_pid = series(|r| ms(r.t_kill, r.t_pid));
        let to_rows = series(|r| ms(r.t_kill, r.t_rows));
        let to_mail = series(|r| ms(r.t_kill, r.t_mail));
        md.extend(head(&format!(
            "a killed bridge, {size} session(s), outside stamps"
        )));
        md.push(row("kill -> new pid", &to_pid));
        md.push(row("kill -> every attach row at a subscriber", &to_rows));
        md.push(row("kill -> live mail in the inbox", &to_mail));
        md.push(String::new());
        by_size.push(json!({
            "sessions": size,
            "kill_to_pid_ms": stats::to_json(&to_pid),
            "kill_to_rows_ms": stats::to_json(&to_rows),
            "kill_to_live_mail_ms": stats::to_json(&to_mail),
        }));
    }
    md.extend(head(&format!(
        "the paced publisher's ack, one publish every {} ms",
        backlog::STEADY_EVERY.as_millis()
    )));
    md.push(row("every ack", &every));
    md.push(row(
        "an ack inside a restart (kill -> rows and mail)",
        &inside,
    ));
    md.push(row("an ack outside every restart", &outside));
    md.push(String::new());
    let json = json!({
        "broker": broker.json(&ready, &done),
        "by_size": by_size,
        "steady_every_ms": u64::try_from(backlog::STEADY_EVERY.as_millis()).unwrap_or(u64::MAX),
        "steady_ack_ms": stats::to_json(&every),
        "steady_ack_inside_ms": stats::to_json(&inside),
        "steady_ack_outside_ms": stats::to_json(&outside),
    });
    drop(w);
    drop(broker);
    (json, md)
}

/// The report's broker note for an arm whose broker runs in a process of its own.
pub(super) fn note(spec: &Spec) -> String {
    format!(
        "each world's broker is this build's `aterm link broker` in a process of its own, started \
         as {} with the argv its launchd job runs (`Paths::broker_argv`: durability {}, at-rest \
         {}), retained by aterm's default rules and limited by its default rate limit as it \
         ships. Only the outside stamps are measured, on the bench's clock (`run/outside.rs`): \
         the messaging world times no burst, and the handoff, ask and cross-node worlds are not \
         run.",
        spec.broker_process.runs_as(),
        spec.durability.word(),
        if spec.at_rest { "on" } else { "off" }
    )
}
