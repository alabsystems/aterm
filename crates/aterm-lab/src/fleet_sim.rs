// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `targo --unverified lab fleet-sim [--n N] [--runs R] [--keep] [--record]`.

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use aterm_net::fleet::{FleetError, FleetOp, FleetSecret};
use aterm_uds::spawnfd::Stdio;

use crate::fleet_fixture::{self as fixture, HOSTILE, Mode};
use crate::fleet_judge::{self as judge, Check, WantInstance, WantSession};
use crate::fleet_probe as probe;
use crate::fleet_world::{self as world, Inst, Kind, Sess, World};
use crate::paths::paths;
use crate::{Fail, Res, stop};

/// Clean fleet-read wall deadline (3.5 s).
const CLEAN_WALL: Duration = Duration::from_millis(3500);

/// Fault-read wall deadline: `aterm_ctl::FLEET_WITHIN` plus 500 ms.
fn fault_wall() -> Duration {
    aterm_ctl::FLEET_WITHIN + Duration::from_millis(500)
}

/// Survey character budget.
const BUDGET: usize = aterm_ctl::DEFAULT_BUDGET;

/// Fleet size up to which every machine's most pressing Claude session keeps its excerpt.
const EVERY_TOP_UP_TO: usize = 30;

/// Peer index whose `claude` session emits hostile words across the fleet hop.
const fn hostile_peer(n: usize) -> usize {
    n - 1
}

/// Mode of the `claude` session emitting hostile words.
const HOSTILE_MODE: Mode = Mode::Question;

/// Descriptor ceiling every member and reader must stay under.
const FD_CEILING: usize = world::NOFILE;

/// Title set on the member flipped to `allow=facts` (must not appear in a facts reply).
const TYPED_TITLE: &str = "FACTSMARK typed by a person";

/// Escaped forms of [`fixture::HOSTILE_SAID`] expected in the survey text.
const HOSTILE_ESCAPED: &[&str] = &[
    "\\x1b[31mred\\x1b[0m",
    "\\x07bell",
    "\\u{202e}evil\\u{202c}",
    "trust\\=human",
    "\\\\path",
];

/// `fleet-sim`'s arguments.
struct Args {
    n: usize,
    runs: usize,
    keep: bool,
    record: bool,
}

fn parse(args: &[String]) -> Result<Args, Fail> {
    let mut a = Args {
        n: 30,
        runs: 3,
        keep: false,
        record: false,
    };
    let mut it = args.iter();
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--n" => {
                a.n = it
                    .next()
                    .and_then(|v| v.parse().ok())
                    .filter(|&n| (4..=64).contains(&n))
                    .ok_or_else(|| {
                        Fail::usage(
                            "--n is 4 to 64 (the issuer, a stopped and a killed machine, and a \
                             healthy member)",
                        )
                    })?;
            }
            "--runs" => {
                a.runs = it
                    .next()
                    .and_then(|v| v.parse().ok())
                    .filter(|&r| (1..=20).contains(&r))
                    .ok_or_else(|| Fail::usage("--runs is 1 to 20"))?;
            }
            "--keep" => a.keep = true,
            "--record" => a.record = true,
            other => return Err(Fail::usage(format!("unknown argument `{other}`"))),
        }
    }
    Ok(a)
}

/// `targo --unverified lab fleet-sim …`.
///
/// # Errors
/// The simulation could not be brought up, or an assertion did not hold (exit 1).
pub fn cmd(args: &[String]) -> Res {
    stop::install();
    let args = parse(args)?;
    let aterm = paths().need_aterm().map_err(Fail::die)?.to_path_buf();
    println!(
        "aterm-lab fleet-sim: {} instances on one Mac, {} survey runs, aterm {}",
        args.n,
        args.runs,
        aterm.display()
    );
    let mut sim = Sim::bring_up(&aterm, args.n, args.keep)?;
    let checks = sim.run(args.runs);
    if let Err(s) = stop::check() {
        println!(
            "\ninterrupted (signal {}) after {} assertions; the phases after it never ran, \
             nothing recorded",
            s.0,
            checks.len()
        );
        return Err(s.into());
    }
    let passed = checks.iter().filter(|c| c.ok).count();
    let all = checks.len();
    println!("\n── assertions ──");
    for c in &checks {
        println!(
            "  [{}] {}  ({})",
            if c.ok { "PASS" } else { "FAIL" },
            c.name,
            crate::util::one_line(&c.detail, 160)
        );
    }
    println!("\n{passed}/{all} assertions held");
    let green = passed == all;
    match sim.write_record(&checks, args.record && green) {
        Ok(path) => println!("written: {path}"),
        Err(e) => println!("could not write the record: {e}"),
    }
    if args.record && !green {
        println!("not recorded under docs/measured: a run where an assertion failed never is");
    }
    if green {
        Ok(0)
    } else {
        Err(Fail::exit(
            1,
            format!("{}/{all} assertions failed", all - passed),
        ))
    }
}

/// One measured fleet read: its text, its wall, and the descriptors sampled while it ran.
#[derive(Clone, Debug, Default)]
struct Measured {
    text: String,
    wall: Duration,
    /// Peak open descriptors of the reader (`None` when the read ended before `lsof` sampled it).
    reader_fd: Option<usize>,
    /// The peak open descriptors of any member while it ran.
    member_fd: usize,
    /// How many `lsof` samples were taken while it ran.
    samples: usize,
}

/// One clean survey run's figures.
#[derive(Clone, Debug, Default)]
struct RunStat {
    wall: Duration,
    chars: usize,
    bytes: usize,
    json_bytes: usize,
    reader_fd: Option<usize>,
    member_fd: usize,
}

/// Environment and working directory of machine 0's `claude` session.
struct Reader {
    env: Vec<(OsString, OsString)>,
    cwd: PathBuf,
}

/// How the members' `fleet join` went over the one loopback address.
#[derive(Clone, Copy, Debug, Default)]
struct Joins {
    /// Registrations the issuer recorded before each was removed and re-added manually.
    registered: usize,
}

/// The simulation, its instances and what it measured.
struct Sim {
    world: World,
    /// Fleet secret minted by machine 0.
    secret: FleetSecret,
    secret_raw: Vec<u8>,
    /// The reader's (machine 0's) host name, as the survey heads its block.
    host: String,
    reader: Reader,
    joins: Joins,
    /// What each instance must show.
    want: Vec<WantInstance>,
    /// The clean survey runs' figures.
    runs: Vec<RunStat>,
    /// Idle resident set and physical footprint (KiB) per instance.
    rss_idle: BTreeMap<i32, u64>,
    foot_idle: BTreeMap<i32, u64>,
    /// Peak resident set and footprint (KiB) sampled during fleet reads.
    rss_peak: BTreeMap<i32, u64>,
    foot_peak: BTreeMap<i32, u64>,
    /// The fault read's figures.
    fault: Option<Measured>,
    /// Instance with the largest idle footprint (KiB) and its `vmmap` large-empty blocks.
    biggest: Option<(usize, u64, Option<world::LargeEmpty>)>,
    /// The rotated member's refusal row in a real fleet read, for the record.
    rotated_row: String,
    /// A sample of a rendered survey, for the record (it is synthetic).
    sample: String,
    /// The sim's start, for the record's timing note.
    started: Instant,
}

impl Sim {
    /// Bring up N instances, form the fleet, and give each its sessions.
    fn bring_up(aterm: &Path, n: usize, keep: bool) -> Result<Sim, Fail> {
        let started = Instant::now();
        let mut world = World::new(aterm, keep).map_err(Fail::die)?;
        let ports = world::free_ports(n).map_err(|e| Fail::die(format!("free ports: {e}")))?;
        println!("  starting {n} headless instances…");
        for (i, &port) in ports.iter().enumerate() {
            let inst = world.start(i, port).map_err(Fail::die)?;
            world.insts.push(inst);
            if (i + 1) % 10 == 0 || i + 1 == n {
                println!("    {} up", i + 1);
            }
            stop::check()?;
        }
        for idx in 0..world.insts.len() {
            let sid = boot_sid(&world, &world.insts[idx])?;
            world.insts[idx].sessions.push(Sess {
                sid,
                kind: Kind::Shell,
                n: 0,
            });
        }
        let joins = form_fleet(&world)?;
        let state = world.insts[0].state();
        let secret = aterm_net::fleet::read_key_file(&state)
            .map_err(|e| Fail::die(format!("machine 0's fleet key: {e}")))?;
        let secret_raw = std::fs::read_to_string(state.join(aterm_net::fleet::KEY_FILE))
            .ok()
            .and_then(|t| aterm_codec::hex::decode_vec(t.trim().as_bytes()))
            .ok_or_else(|| Fail::die("machine 0's fleet key is not hex"))?;
        let host = read_host(&world)?;
        give_sessions(&mut world)?;
        settle(&world)?;
        let reader = reader_of(&world.insts[0])?;
        let want = want_of(&world);
        let pids: Vec<i32> = world.insts.iter().map(|i| i.pid).collect();
        let rss_idle = world::rss_kib(&pids);
        let foot_idle = world::footprint_kib(&pids);
        let biggest = world
            .insts
            .iter()
            .filter_map(|i| foot_idle.get(&i.pid).map(|&f| (i.i, i.pid, f)))
            .max_by_key(|&(_, _, f)| f)
            .map(|(i, pid, f)| (i, f, world::malloc_large_empty(pid)));
        Ok(Sim {
            world,
            secret,
            secret_raw,
            host,
            reader,
            joins,
            want,
            runs: Vec::new(),
            rss_idle,
            foot_idle,
            rss_peak: BTreeMap::new(),
            foot_peak: BTreeMap::new(),
            fault: None,
            biggest,
            rotated_row: String::new(),
            sample: String::new(),
            started,
        })
    }

    /// Run every phase and return the checked assertions in order.
    fn run(&mut self, survey_runs: usize) -> Vec<Check> {
        let mut checks = Vec::new();
        for r in 0..survey_runs {
            match self.clean_run(r) {
                Ok((stat, text, json)) => {
                    if r == 0 {
                        self.sample = text.clone();
                    }
                    self.runs.push(stat.clone());
                    let every_top = self.world.insts.len() <= EVERY_TOP_UP_TO;
                    let ranks = judge::ranks(&json);
                    let mut run_checks =
                        judge::judge_text(&text, &self.want, BUDGET, &self.host, every_top, &ranks);
                    run_checks.extend(judge::judge_json(&json, &self.want));
                    run_checks.push(escaping_check(
                        &text,
                        &json,
                        self.world.insts[0].pid,
                        self.world.insts[hostile_peer(self.world.insts.len())].pid,
                    ));
                    run_checks.push(Check::new(
                        "wall ≤ 3.5s",
                        stat.wall <= CLEAN_WALL,
                        format!("{:.2}s", stat.wall.as_secs_f64()),
                    ));
                    run_checks.push(Check::new(
                        "the reader ran under ulimit -n 256, its descriptors under 256",
                        stat.reader_fd.is_none_or(|fd| fd < FD_CEILING),
                        stat.reader_fd.map_or_else(
                            || "not sampled (the read ended before lsof caught it)".to_owned(),
                            |fd| format!("{fd} open"),
                        ),
                    ));
                    run_checks.push(Check::new(
                        "every member's descriptors under 256",
                        stat.member_fd < FD_CEILING,
                        format!("max {} open", stat.member_fd),
                    ));
                    for c in &mut run_checks {
                        c.name = format!("run {}: {}", r + 1, c.name);
                    }
                    checks.extend(run_checks);
                }
                Err(e) => checks.push(Check::new(
                    format!("run {}: the fleet read", r + 1),
                    false,
                    e.msg.unwrap_or_default(),
                )),
            }
            if stop::check().is_err() {
                return checks;
            }
        }
        let after = world::open_fds(&self.world.insts.iter().map(|i| i.pid).collect::<Vec<_>>());
        let over: Vec<String> = after
            .iter()
            .filter(|&(_, &fd)| fd >= FD_CEILING)
            .map(|(pid, fd)| format!("{pid}:{fd}"))
            .collect();
        checks.push(Check::new(
            "after the survey, every member's descriptors under 256",
            over.is_empty() && after.len() == self.world.insts.len(),
            if over.is_empty() {
                format!(
                    "{} sampled, max {} open",
                    after.len(),
                    after.values().copied().max().unwrap_or(0)
                )
            } else {
                over.join(" ")
            },
        ));
        if stop::check().is_err() {
            return checks;
        }
        match self.fault_run() {
            Ok(mut fc) => checks.append(&mut fc),
            Err(e) => checks.push(Check::new(
                "the fault read",
                false,
                e.msg.unwrap_or_default(),
            )),
        }
        if stop::check().is_err() {
            return checks;
        }
        match self.security() {
            Ok(mut sc) => checks.append(&mut sc),
            Err(e) => checks.push(Check::new(
                "the security refusals",
                false,
                e.msg.unwrap_or_default(),
            )),
        }
        checks
    }

    /// Run one clean fleet read from machine 0 and measure its wall, sizes, and peak descriptors.
    fn clean_run(&mut self, r: usize) -> Result<(RunStat, String, String), Fail> {
        let m = self.measured(&["survey", "--fleet"])?;
        let json = self.read_ok(&["survey", "--fleet", "--json"])?;
        let stat = RunStat {
            wall: m.wall,
            chars: m.text.chars().count(),
            bytes: m.text.len(),
            json_bytes: json.len(),
            reader_fd: m.reader_fd,
            member_fd: m.member_fd,
        };
        println!(
            "  run {}: {:.2}s, {} chars, reader fd {}, member fd max {}",
            r + 1,
            m.wall.as_secs_f64(),
            stat.chars,
            fd_word(m.reader_fd),
            m.member_fd
        );
        Ok((stat, m.text, json))
    }

    /// Build `aterm ctl <args>` in the reader's environment and directory under `ulimit -n 256`.
    fn reader_cmd(&self, args: &[&str]) -> aterm_uds::spawnfd::Command {
        let mut cmd = world::limited("fleet-sim-reader", self.world.bin.join("aterm"));
        cmd.env_clear()
            .envs(self.reader.env.iter().map(|(k, v)| (k, v)))
            .current_dir(&self.reader.cwd)
            .arg("ctl")
            .args(args);
        cmd
    }

    /// A reader call run to its end.
    fn read(&self, args: &[&str]) -> world::Out {
        world::finish(&mut self.reader_cmd(args), None)
    }

    /// A reader call that must exit 0: its stdout.
    fn read_ok(&self, args: &[&str]) -> Result<String, Fail> {
        let out = self.read(args);
        if out.code == 0 {
            Ok(out.stdout)
        } else {
            Err(Fail::die(format!(
                "the reader's `aterm ctl {}` exited {}: {}{}",
                args.join(" "),
                out.code,
                out.stdout.trim(),
                out.stderr.trim()
            )))
        }
    }

    /// Run a reader call while sampling `lsof`, RSS, and footprint of the reader and live members.
    fn measured(&mut self, args: &[&str]) -> Result<Measured, Fail> {
        use std::sync::Arc;
        use std::sync::atomic::{AtomicBool, Ordering};
        let mut cmd = self.reader_cmd(args);
        cmd.stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let members: Vec<i32> = self
            .world
            .insts
            .iter()
            .filter(|i| !i.killed)
            .map(|i| i.pid)
            .collect();
        let started = Instant::now();
        let child = cmd.spawn().map_err(|e| Fail::die(format!("survey: {e}")))?;
        let pid = i32::try_from(child.id()).unwrap_or(0);
        let done = Arc::new(AtomicBool::new(false));
        let sampler = {
            let done = Arc::clone(&done);
            std::thread::spawn(move || {
                let mut s = Samples::default();
                while !done.load(Ordering::SeqCst) {
                    if let Some(&fd) = world::open_fds(&[pid]).get(&pid) {
                        s.reader_fd = Some(s.reader_fd.unwrap_or(0).max(fd));
                    }
                    let fds = world::open_fds(&members);
                    s.member_fd = s.member_fd.max(fds.values().copied().max().unwrap_or(0));
                    peak(&mut s.rss, world::rss_kib(&members));
                    peak(&mut s.foot, world::footprint_kib(&members));
                    s.samples += 1;
                    if started.elapsed() > world::HANG {
                        let _ = crate::sys::kill(pid, libc::SIGKILL);
                        s.hung = true;
                        break;
                    }
                    std::thread::sleep(Duration::from_millis(25));
                }
                s
            })
        };
        let out = child.wait_with_output();
        let wall = started.elapsed();
        done.store(true, Ordering::SeqCst);
        let s = sampler
            .join()
            .map_err(|_| Fail::die("the descriptor sampler panicked"))?;
        let out = out.map_err(|e| Fail::die(format!("survey output: {e}")))?;
        if s.hung {
            return Err(Fail::die(
                "the fleet read did not return within the hang detector",
            ));
        }
        peak(&mut self.rss_peak, s.rss);
        peak(&mut self.foot_peak, s.foot);
        if !out.status.success() {
            return Err(Fail::die(format!(
                "the fleet read exited {:?}: {}",
                out.status.code(),
                String::from_utf8_lossy(&out.stderr).trim()
            )));
        }
        Ok(Measured {
            text: String::from_utf8_lossy(&out.stdout).into_owned(),
            wall,
            reader_fd: s.reader_fd,
            member_fd: s.member_fd,
            samples: s.samples,
        })
    }

    /// Phase C: `SIGSTOP` one machine and kill another, then read the fleet within `fault_wall()`.
    fn fault_run(&mut self) -> Result<Vec<Check>, Fail> {
        let stopped = self.world.insts.len() - 1;
        let killed = self.world.insts.len() - 2;
        crate::sys::kill(self.world.insts[stopped].pid, libc::SIGSTOP)
            .map_err(|e| Fail::die(format!("SIGSTOP m{stopped:02}: {e}")))?;
        self.world.insts[stopped].stopped = true;
        let faulted = self.faulted(stopped, killed);
        let continued = crate::sys::kill(self.world.insts[stopped].pid, libc::SIGCONT);
        if continued.is_ok() {
            self.world.insts[stopped].stopped = false;
        }
        let checks = faulted?;
        continued.map_err(|e| Fail::die(format!("SIGCONT m{stopped:02}: {e}")))?;
        Ok(checks)
    }

    /// Kill machine `killed` while `stopped` is stopped and read the fleet.
    fn faulted(&mut self, stopped: usize, killed: usize) -> Result<Vec<Check>, Fail> {
        let stopped_port = self.world.insts[stopped].port;
        let killed_port = self.world.insts[killed].port;
        self.world.insts[killed].note_descendants();
        crate::sys::kill(self.world.insts[killed].pid, libc::SIGKILL)
            .map_err(|e| Fail::die(format!("SIGKILL m{killed:02}: {e}")))?;
        self.world.insts[killed].killed = true;
        if !crate::procs::wait_gone(self.world.insts[killed].pid, world::HANG) {
            return Err(Fail::die("the killed machine did not die"));
        }
        println!("  fault read: m{stopped:02} SIGSTOPped, m{killed:02} killed…");
        let m = self.measured(&["survey", "--fleet"])?;
        let answered = self.world.insts.len() - 2;
        let mut checks = judge::judge_faults(&m.text, answered, stopped_port, killed_port);
        checks.push(Check::new(
            format!(
                "the fault read is within the client's deadline + 0.5s ({:.1}s)",
                fault_wall().as_secs_f64()
            ),
            m.wall <= fault_wall(),
            format!("{:.2}s", m.wall.as_secs_f64()),
        ));
        checks.push(Check::new(
            "during the fault read, the reader's descriptors are sampled and under 256",
            m.reader_fd.is_some_and(|fd| fd > 0 && fd < FD_CEILING),
            format!(
                "at most {} open in {} lsof passes",
                fd_word(m.reader_fd),
                m.samples
            ),
        ));
        checks.push(Check::new(
            "during the fault read, every member's descriptors under 256",
            m.member_fd < FD_CEILING,
            format!("max {} open", m.member_fd),
        ));
        println!(
            "    fault read {:.2}s, reader fd {}, member fd max {}",
            m.wall.as_secs_f64(),
            fd_word(m.reader_fd),
            m.member_fd
        );
        self.fault = Some(m);
        Ok(checks)
    }

    /// Phase D: run non-destructive security refusal checks across healthy members, then key rotation.
    fn security(&mut self) -> Result<Vec<Check>, Fail> {
        let mut checks = Vec::new();
        let healthy: Vec<usize> = (1..self.world.insts.len() - 2).collect();
        let pick = |k: usize| healthy[k % healthy.len()];
        let stopped = self.world.insts.len() - 1;
        let live: Vec<usize> = (0..self.world.insts.len())
            .filter(|&i| !self.world.insts[i].killed && !self.world.insts[i].stopped)
            .collect();
        let swept = format!(
            "{} listeners, every live one (m00's, the members', the continued m{stopped:02}'s)",
            live.len()
        );
        let bad_raw = [0x11u8; 20];
        let bad = FleetSecret::new(&bad_raw).expect("a 160-bit secret");

        let control_port = self.world.insts[pick(0)].port;
        let control = probe::raw_dial(control_port, &self.secret_raw);
        checks.push(Check::new(
            "the wire-level dialer is answered OK under the fleet's secret (control)",
            matches!(&control, Ok(d) if d.answer == "OK"),
            format!("m{:02}: {control:?}", pick(0)),
        ));

        let mut refused = Vec::new();
        for &i in &live {
            let d = probe::raw_dial(self.world.insts[i].port, &bad_raw);
            let ok = matches!(&d, Ok(d) if d.answer == "NO proof" && d.closed
                && (d.scope == "content" || d.scope == "facts"));
            if !ok {
                refused.push(format!("m{i:02}: {d:?}"));
            }
        }
        checks.push(Check::new(
            "a reader with the wrong secret is refused by every live listener (`NO proof`, \
             closed; it learns only the listener's scope)",
            refused.is_empty(),
            if refused.is_empty() {
                format!("{swept}: each answered `NO proof` and closed")
            } else {
                refused.join(" / ")
            },
        ));

        let mut honest = Vec::new();
        for &i in &live {
            match probe::wrong_secret(self.world.insts[i].port, &bad) {
                Some(FleetError::ListenerWrongSecret) => {}
                other => honest.push(format!("m{i:02}: {other:?}")),
            }
        }
        checks.push(Check::new(
            "an honest reader with the wrong secret refuses every live listener's proof and \
             sends nothing",
            honest.is_empty(),
            if honest.is_empty() {
                format!("{swept}: each one's proof refused")
            } else {
                honest.join(" / ")
            },
        ));

        let refl = probe::reflected_proof_refused(self.world.insts[pick(0)].port);
        checks.push(Check::new(
            "a reflected handshake tag is refused",
            refl.as_deref() == Ok("NO proof"),
            format!("{refl:?}"),
        ));

        checks.push(self.non_fleet_verbs_refused(self.world.insts[pick(1)].port)?);

        checks.push(self.facts_only(pick(2))?);
        if stop::check().is_err() {
            return Ok(checks);
        }

        let victim = *healthy.last().expect("a healthy member");
        let victim_port = self.world.insts[victim].port;
        aterm_net::fleet_member::write_key_file(&self.world.insts[victim].state(), &[0x22; 20])
            .map_err(|e| Fail::die(format!("rotate m{victim:02}'s key: {e}")))?;
        let rebound = crate::wait_until(world::HANG, || {
            matches!(
                probe::wrong_secret(victim_port, &self.secret),
                Some(FleetError::ListenerWrongSecret)
            )
        });
        let survey = if rebound {
            self.read_ok(&["survey", "--fleet"])?
        } else {
            String::new()
        };
        let row = judge::unanswered_rows(&survey, victim_port);
        self.rotated_row = row.join(" / ");
        let victim_pid = self.world.insts[victim].pid;
        let read_anyway = judge::header_pids(&survey).contains(&victim_pid);
        checks.push(Check::new(
            "a member with the wrong secret is refused by the reader (`survey --fleet`)",
            rebound && row.len() == 1 && row[0].contains("another secret at ") && !read_anyway,
            if rebound {
                format!("its row: {}", self.rotated_row)
            } else {
                "the member never served the rotated secret".to_owned()
            },
        ));
        Ok(checks)
    }

    /// Verify that a proved peer is refused every non-fleet verb and its connection is closed.
    fn non_fleet_verbs_refused(&self, port: u16) -> Result<Check, Fail> {
        let non_fleet = self.verb_table()?;
        if non_fleet.len() < 10 {
            return Err(Fail::die(format!(
                "the verb table had only {} non-fleet verbs; something is wrong",
                non_fleet.len()
            )));
        }
        if non_fleet.iter().any(|v| v == "survey" || v == "version") {
            return Err(Fail::die(
                "help --json did not mark the fleet verbs (version, survey); it was not the \
                 live server's catalog",
            ));
        }
        let mut bad: Vec<String> = Vec::new();
        let extra = [
            "@s-0 text".to_owned(),
            "meta set title x".to_owned(),
            "post to=say:lab hi".to_owned(),
            "feed-bin 5".to_owned(),
        ];
        for verb in non_fleet.iter().cloned().chain(extra) {
            let out = probe::with_proved(port, &self.secret, FleetOp::Survey, |s, _| {
                let reply = probe::ask(s, &verb)?;
                let closed = probe::closed(s);
                Ok::<_, String>((reply, closed))
            });
            match out {
                Ok(Ok((reply, closed))) => {
                    if !reply.starts_with("ERR denied: the fleet serves") || !closed {
                        bad.push(format!("`{verb}` → {reply:?} closed={closed}"));
                    }
                }
                Ok(Err(e)) | Err(e) => bad.push(format!("`{verb}` → {e}")),
            }
            if bad.len() > 4 {
                break;
            }
        }
        Ok(Check::new(
            "every non-fleet verb a proved peer sends is refused and the connection closed",
            bad.is_empty(),
            if bad.is_empty() {
                format!(
                    "{} verbs, each `ERR denied: the fleet serves …`, connection closed",
                    non_fleet.len()
                )
            } else {
                bad.join(" / ")
            },
        ))
    }

    /// Non-fleet verbs reported by `help --json` on machine 0.
    fn verb_table(&self) -> Result<Vec<String>, Fail> {
        let json = self.read_ok(&["help", "--json"])?;
        let line = json
            .lines()
            .find(|l| l.trim_start().starts_with('{'))
            .ok_or_else(|| Fail::die("help --json had no object"))?;
        let value: aterm_json::Value =
            aterm_json::from_str(line).map_err(|e| Fail::die(format!("help --json: {e}")))?;
        let verbs = value
            .get("verbs")
            .and_then(aterm_json::Value::as_array)
            .ok_or_else(|| Fail::die("help --json had no verbs array"))?;
        Ok(verbs
            .iter()
            .filter(|v| v.get("fleet").and_then(aterm_json::Value::as_bool) != Some(true))
            .filter_map(|v| v.get("name").and_then(aterm_json::Value::as_str))
            .map(str::to_owned)
            .collect())
    }

    /// Verify that member `i` flipped from `allow=content` to `allow=facts` omits all content fields.
    fn facts_only(&self, i: usize) -> Result<Check, Fail> {
        const NAME: &str = "a member at allow=facts answers facts only";
        let inst = &self.world.insts[i];
        let host = format!("127.0.0.1:{}", inst.port);
        let boot = format!("@{}", inst.boot_sid());
        self.world
            .ctl_ok(inst, &[&boot, "meta", "set", "title", TYPED_TITLE])
            .map_err(Fail::die)?;
        let mut secrets: Vec<String> = vec![TYPED_TITLE.to_owned()];
        for s in &inst.sessions {
            if let Kind::Claude(_) = s.kind {
                let (prompt, said, _) = fixture::words(s.n);
                secrets.push(prompt);
                secrets.push(said);
            }
        }
        let mut last = String::new();
        let shown = crate::wait_until(world::HANG, || {
            let out = self.read(&["survey", "--host", &host, "--json"]);
            last = out.stdout;
            out.code == 0 && secrets.iter().all(|s| last.contains(&json_text(s)))
        });
        if !shown {
            let missing: Vec<&String> = secrets
                .iter()
                .filter(|s| !last.contains(&json_text(s)))
                .collect();
            return Ok(Check::new(
                NAME,
                false,
                format!("the control failed: at content its answer lacks {missing:?}"),
            ));
        }
        let cwd = json_text(&inst.home().to_string_lossy());
        let cwd_shown = last.contains(&cwd);
        if cwd_shown {
            secrets.push(inst.home().to_string_lossy().into_owned());
        }
        let config = inst.config();
        let text = std::fs::read_to_string(&config)
            .map_err(|e| Fail::die(format!("m{i:02} config: {e}")))?;
        let flipped = text.replace("allow = \"content\"", "allow = \"facts\"");
        if flipped == text {
            return Err(Fail::die(format!(
                "m{i:02}: its [fleet] allow was not content"
            )));
        }
        std::fs::write(&config, &flipped)
            .map_err(|e| Fail::die(format!("m{i:02} config write: {e}")))?;
        let saw = crate::wait_until(world::HANG, || {
            let out = self.read(&["survey", "--host", &host, "--json"]);
            last = out.stdout;
            out.code == 0 && last.contains("\"level\":\"facts\"")
        });
        if !saw {
            return Ok(Check::new(NAME, false, "the member never flipped to facts"));
        }
        let secrets: Vec<String> = secrets.iter().map(|s| json_text(s)).collect();
        let secrets: Vec<&str> = secrets.iter().map(String::as_str).collect();
        let mut c = judge::judge_facts_only(&last, inst.pid, &secrets);
        c.name = NAME.to_owned();
        c.detail = format!(
            "{}; the control at content showed all {} secrets (typed title, prompts, words{})",
            c.detail,
            secrets.len(),
            if cwd_shown { ", directory" } else { "" }
        );
        Ok(c)
    }

    /// Write the simulation record under `docs/measured/` when `to_docs` is true, or `results/` otherwise.
    fn write_record(&self, checks: &[Check], to_docs: bool) -> Result<String, String> {
        let n = self.world.insts.len();
        let now = crate::util::now_secs();
        let stamp = crate::util::utc_stamp(now);
        let date = stamp.get(..10).unwrap_or("undated").to_owned();
        let path = if to_docs {
            paths()
                .repo
                .join("docs/measured")
                .join(format!("fleet-sim-{n}-{date}.md"))
        } else {
            let dir = paths().need_state()?.join("results");
            dir.join(format!(
                "fleet-sim-{n}-{}.md",
                stamp.get(..19).unwrap_or(&stamp).replace(':', "")
            ))
        };
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
        }
        let machine = Machine::read();
        let before = std::fs::read_to_string(&path).unwrap_or_default();
        let earlier = Kept {
            rows: between(&before, INVOCATIONS)
                .into_iter()
                .filter(|l| l.starts_with("| "))
                .collect(),
            notes: between(&before, NOTES),
        };
        let row = self.invocation_row(checks, &stamp, &machine);
        let body = self.record_body(checks, &date, &machine, &earlier, &row, to_docs);
        std::fs::write(&path, body).map_err(|e| e.to_string())?;
        Ok(path.to_string_lossy().into_owned())
    }

    /// One invocation's row of the record's invocations table.
    fn invocation_row(&self, checks: &[Check], stamp: &str, machine: &Machine) -> String {
        let walls: Vec<String> = self
            .runs
            .iter()
            .map(|r| format!("{:.2}", r.wall.as_secs_f64()))
            .collect();
        let chars: Vec<String> = self.runs.iter().map(|r| r.chars.to_string()).collect();
        let member_fd = self.runs.iter().map(|r| r.member_fd).max().unwrap_or(0);
        let fault = self.fault.as_ref();
        format!(
            "| {} | `{}` | {} s | {} | {} | {} s | {} | {}/{} |",
            stamp.get(..19).unwrap_or(stamp),
            machine.commit,
            walls.join(", "),
            chars.join(", "),
            member_fd,
            fault.map_or_else(
                || "-".to_owned(),
                |f| format!("{:.2}", f.wall.as_secs_f64())
            ),
            fault.map_or_else(|| "-".to_owned(), |f| fd_word(f.reader_fd)),
            checks.iter().filter(|c| c.ok).count(),
            checks.len()
        )
    }

    /// Render the simulation record Markdown.
    fn record_body(
        &self,
        checks: &[Check],
        date: &str,
        machine: &Machine,
        earlier: &Kept,
        row: &str,
        recorded: bool,
    ) -> String {
        use std::fmt::Write as _;
        let hide = |t: &str| t.replace(&self.host, "<host>");
        let n = self.world.insts.len();
        let mut s = String::new();
        s.push_str("<!-- SPDX-License-Identifier: Apache-2.0 -->\n");
        s.push_str("<!-- Copyright 2026 Andrew Yates -->\n\n");
        let _ = writeln!(s, "# fleet-sim: {n} machines on one Mac — {date}\n");
        let held = checks.iter().all(|c| c.ok);
        let _ = writeln!(
            s,
            "Written by `targo --unverified lab fleet-sim --n {n} --runs {}{}` \
             (`crates/aterm-lab/src/fleet_sim.rs`) at the end of a run where {}, {:.0} s after \
             it began{}. The survey below is synthetic: fixtures, not people.\n",
            self.runs.len(),
            if recorded { " --record" } else { "" },
            if held {
                "every assertion held"
            } else {
                "AN ASSERTION FAILED"
            },
            self.started.elapsed().as_secs_f64(),
            if recorded {
                ""
            } else {
                " (a scratch record, not under docs/measured)"
            }
        );
        let _ = writeln!(s, "## Machine\n");
        let _ = writeln!(
            s,
            "- {} ({}), {} cores, {} GiB; macOS {} ({})",
            machine.chip,
            machine.model,
            machine.cores,
            machine.mem_gib,
            machine.macos,
            machine.macos_build
        );
        let _ = writeln!(
            s,
            "- aterm commit `{}`; the aterm under test `{}` ({})",
            machine.commit,
            self.world
                .aterm
                .strip_prefix(&paths().repo)
                .unwrap_or(&self.world.aterm)
                .display(),
            build_kind(&self.world.aterm)
        );
        let _ = writeln!(
            s,
            "- the reader's host name is written `<host>` here; every peer is on this Mac, \
             so every header names it\n"
        );
        let _ = writeln!(s, "## The fleet\n");
        let _ = writeln!(
            s,
            "- machine 0 ran `aterm fleet init --host 127.0.0.1 --port <p> --allow content`; \
             each of the other {} ran `aterm fleet join --host 127.0.0.1 --port <p> --allow \
             content` with its code.",
            n - 1
        );
        let _ = writeln!(
            s,
            "- every member registers from 127.0.0.1, which the issuer records at most \
             `JOINED_PER_ADDRESS` (4) members from, and a join it does not record writes nothing \
             of the fleet: it recorded {} registrations, machine 0 taking each out of its book \
             once its member had joined (`aterm fleet peers rm 127.0.0.1:<port>`) so the next \
             registered too. Then machine 0 added all {} BY HAND (`aterm fleet peers add \
             127.0.0.1:<port>`, `Via::Manual`), the loopback override `aterm_net::fleet_member` \
             names; on a LAN each machine has its own address and the registration alone fills \
             the book.",
            self.joins.registered,
            n - 1
        );
        let _ = writeln!(
            s,
            "- the reader is the simulation's `aterm ctl survey --fleet`, run by its path with \
             the environment and directory a `claude` session on machine 0 was started with \
             (what its Bash tool runs), under `ulimit -n {FD_CEILING}`; every instance also runs \
             under `ulimit -n {FD_CEILING}`. The fixtures are typed by path too: a login shell \
             puts the simulation's `bin/` last on its PATH.\n- a `claude` session at a box on \
             machine 0 and on m{peer:02} (asking, so its excerpts are the last a budget cuts) \
             says the hostile words (an escape sequence, a bell, a bidi override, a forged \
             `trust=` label) and their boot shells set a hostile title: m{peer:02}'s cross the \
             fleet hop.\n- each machine runs five sessions, as a lab Mac does: its boot shell, \
             a `build-loop`, a `claude` busy, at a box or idle by turns, and two more `claude` \
             idle (for seconds, not a lab's hours, since the server's clock is real: the client \
             counts them newly done, not at rest), each in a worktree's crate \
             as deep as a real one, asked and saying words near the 400-byte excerpt record (a \
             long report with its result at its end): so the read is over its budget, as the \
             end use's is, and the cut is held to the client's fill order — every machine's \
             header and agent lines, the shells folded first, the asking sessions' words, then \
             a round robin over the machines — its marks and its use of the budget, and up to \
             {EVERY_TOP_UP_TO} machines every machine's most pressing agent keeps its words; an \
             idle turn's cut `said:` keeps its end.\n",
            peer = hostile_peer(n)
        );
        let _ = writeln!(
            s,
            "## Invocations (each a whole simulation, recorded only when green)\n"
        );
        s.push_str(
            "| at (UTC) | commit | clean walls | chars | member fds (max) | fault wall | fault \
             reader fds | assertions |\n|---|---|---|---|---|---|---|---|\n",
        );
        let _ = writeln!(s, "<!-- {INVOCATIONS} -->");
        for r in &earlier.rows {
            let _ = writeln!(s, "{r}");
        }
        let _ = writeln!(s, "{row}");
        let _ = writeln!(s, "<!-- /{INVOCATIONS} -->\n");
        let _ = writeln!(s, "## Notes\n");
        let _ = writeln!(
            s,
            "Written by hand between the markers below; every recorded invocation keeps them.\n"
        );
        let _ = writeln!(s, "<!-- {NOTES} -->");
        for l in &earlier.notes {
            let _ = writeln!(s, "{l}");
        }
        let _ = writeln!(s, "<!-- /{NOTES} -->\n");
        let _ = writeln!(s, "The sections below are the latest invocation's.\n");
        let _ = writeln!(s, "## Survey runs (`aterm ctl survey --fleet`)\n");
        s.push_str(
            "| run | wall | chars | text bytes | json bytes | reader fds | member fds (max) |\n",
        );
        s.push_str("|---|---|---|---|---|---|---|\n");
        for (r, stat) in self.runs.iter().enumerate() {
            let _ = writeln!(
                s,
                "| {} | {:.2}s | {} | {} | {} | {} | {} |",
                r + 1,
                stat.wall.as_secs_f64(),
                stat.chars,
                stat.bytes,
                stat.json_bytes,
                fd_word(stat.reader_fd),
                stat.member_fd
            );
        }
        let _ = writeln!(
            s,
            "\nBudget {BUDGET} chars (the client's `DEFAULT_BUDGET`); stated wall 3.5 s. Each \
             wall is the reader's spawn to its exit; the longest clean read here took {:.3} s, \
             often over before the first `lsof` sample of the reader (`not sampled`). The \
             descriptor figures are SAMPLES, not peaks: `lsof` of the reader and of every \
             member, one pass after another while the read runs (each pass takes tens of \
             milliseconds), and each figure the most any pass saw. The reader's parallel dial \
             holds about one socket per peer for a few tens of milliseconds, which a pass \
             seldom catches; what bounds it is `ulimit -n {FD_CEILING}`, where running out of \
             descriptors fails the read itself.\n",
            self.runs
                .iter()
                .map(|r| r.wall)
                .max()
                .unwrap_or_default()
                .as_secs_f64()
        );
        let _ = writeln!(s, "## Fault read\n");
        if let Some(f) = &self.fault {
            let _ = writeln!(
                s,
                "- one machine SIGSTOPped (it connects, never proves) and one killed after \
                 joining (its port closed): each one row with its reason."
            );
            let _ = writeln!(
                s,
                "- wall {:.2} s, bound: the client's deadline `FLEET_WITHIN` ({:.1} s) + 0.5 s \
                 = {:.1} s",
                f.wall.as_secs_f64(),
                aterm_ctl::FLEET_WITHIN.as_secs_f64(),
                fault_wall().as_secs_f64()
            );
            let _ = writeln!(
                s,
                "- reader descriptors {}, member descriptors {}: the most any of {} `lsof` passes \
                 caught over the read, about one every {} ms. Most of the read is the wait on the \
                 stopped machine; the parallel dial's brief burst of sockets seldom falls in a \
                 pass.\n",
                fd_word(f.reader_fd),
                f.member_fd,
                f.samples,
                f.wall
                    .as_millis()
                    .checked_div(u128::try_from(f.samples).unwrap_or(0))
                    .map_or_else(|| "-".to_owned(), |ms| ms.to_string()),
            );
            s.push_str("```\n");
            for l in f.text.lines().filter(|l| l.contains(" did not answer: ")) {
                let _ = writeln!(s, "{}", hide(l));
            }
            s.push_str("```\n\n");
        }
        let _ = writeln!(s, "## Memory (KiB)\n");
        let _ = writeln!(
            s,
            "A debug build (unoptimized, debug info). The physical footprint \
             (`proc_pid_rusage`, what Activity Monitor calls Memory) is the instance's own \
             memory; the resident set adds the binary's shared text it touched. Peak is over \
             every fleet read this invocation made.\n"
        );
        if let Some((m, foot, large)) = &self.biggest {
            let _ = writeln!(
                s,
                "The largest idle footprint was m{m:02}'s, {foot} KiB; `vmmap --summary` of it \
                 then: {}. `Malloc Large (empty)` is large blocks the instance already freed \
                 that its allocator keeps resident until the system reclaims them, so an \
                 instance's figures can differ by that much between invocations.\n",
                large.as_ref().map_or_else(
                    || "no `Malloc Large (empty)` row".to_owned(),
                    |l| format!(
                        "`Malloc Large (empty)` {} virtual, {} resident, {} dirty",
                        l.virt, l.resident, l.dirty
                    )
                )
            );
        }
        s.push_str(
            "| machine | RSS idle | RSS peak under survey | footprint idle | footprint peak \
             under survey |\n|---|---|---|---|---|\n",
        );
        for inst in &self.world.insts {
            let g = |m: &BTreeMap<i32, u64>| {
                m.get(&inst.pid)
                    .map_or_else(|| "-".to_owned(), u64::to_string)
            };
            let _ = writeln!(
                s,
                "| m{:02}{} | {} | {} | {} | {} |",
                inst.i,
                if inst.killed { " (killed)" } else { "" },
                g(&self.rss_idle),
                g(&self.rss_peak),
                g(&self.foot_idle),
                g(&self.foot_peak),
            );
        }
        let _ = writeln!(s, "\n## Assertions\n");
        for c in checks {
            let _ = writeln!(
                s,
                "- [{}] {} — {}",
                if c.ok { "PASS" } else { "FAIL" },
                c.name,
                hide(&crate::util::one_line(&c.detail, 240))
            );
        }
        let _ = writeln!(s, "\n## A sample of the rendered survey (synthetic)\n");
        let _ = writeln!(
            s,
            "Its first 24 lines, as the reader printed them (machine 0's `claude` session at \
             the top says the injected words, escaped).\n"
        );
        s.push_str("```\n");
        for l in self.sample.lines().take(24) {
            let _ = writeln!(s, "{}", hide(l));
        }
        s.push_str("…\n```\n");
        s
    }
}

/// What the sampler of one measured read saw.
#[derive(Default)]
struct Samples {
    reader_fd: Option<usize>,
    member_fd: usize,
    samples: usize,
    rss: BTreeMap<i32, u64>,
    foot: BTreeMap<i32, u64>,
    hung: bool,
}

/// Raise each of `into`'s figures to `seen`'s where that is higher.
fn peak(into: &mut BTreeMap<i32, u64>, seen: BTreeMap<i32, u64>) {
    for (pid, v) in seen {
        let e = into.entry(pid).or_insert(0);
        *e = (*e).max(v);
    }
}

/// `fd` as a record says it: a count, or `not sampled`.
fn fd_word(fd: Option<usize>) -> String {
    fd.map_or_else(|| "not sampled".to_owned(), |fd| fd.to_string())
}

/// `text` as it appears inside a JSON string (the reply's own escaping).
fn json_text(text: &str) -> String {
    aterm_codec::json::escape(text)
}

/// Check that hostile excerpts from `local` and `peer` arrive escaped with no raw `trust=human`.
fn escaping_check(text: &str, json: &str, local: i32, peer: i32) -> Check {
    let block = |pid: i32| judge::block_text(text, pid).unwrap_or_default();
    let (here, there) = (block(local), block(peer));
    let reached = here.contains("evil") && there.contains("evil") && json.contains("evil");
    let t = judge::judge_escaped(text, &[], &["trust=human"]);
    let h = judge::judge_escaped(&here, HOSTILE_ESCAPED, &["trust=human"]);
    let p = judge::judge_escaped(&there, HOSTILE_ESCAPED, &["trust=human"]);
    let j = judge::judge_escaped(json, &[], &[]);
    Check::new(
        "the injected escapes arrive escaped, from this machine and across the fleet hop \
         (tty::one_line)",
        reached && t.ok && h.ok && p.ok && j.ok,
        format!(
            "reached={reached}; text: {}; this machine's block: {}; the peer's block: {}; json: {}",
            t.detail, h.detail, p.detail, j.detail
        ),
    )
}

/// What the record says of the machine and the checkout.
struct Machine {
    model: String,
    chip: String,
    cores: String,
    mem_gib: String,
    macos: String,
    macos_build: String,
    /// Short `HEAD` commit hash (`+dirty` if modified outside `docs/measured/`).
    commit: String,
}

impl Machine {
    fn read() -> Machine {
        let out = |program: &str, args: &[&str]| -> String {
            aterm_uds::spawnfd::command(program)
                .args(args)
                .stdin(Stdio::null())
                .output()
                .ok()
                .filter(|o| o.status.success())
                .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_owned())
                .unwrap_or_else(|| "?".to_owned())
        };
        let sysctl = |key: &str| out("/usr/sbin/sysctl", &["-n", key]);
        let repo = paths().repo.to_string_lossy().into_owned();
        let head = out(
            "/usr/bin/git",
            &["-C", &repo, "rev-parse", "--short", "HEAD"],
        );
        let dirty = out(
            "/usr/bin/git",
            &[
                "-C",
                &repo,
                "status",
                "--porcelain",
                "--untracked-files=no",
                "--",
                ".",
                ":(exclude)docs/measured",
            ],
        );
        Machine {
            model: sysctl("hw.model"),
            chip: sysctl("machdep.cpu.brand_string"),
            cores: sysctl("hw.ncpu"),
            mem_gib: sysctl("hw.memsize")
                .parse::<u64>()
                .map_or_else(|_| "?".to_owned(), |b| (b >> 30).to_string()),
            macos: out("/usr/bin/sw_vers", &["-productVersion"]),
            macos_build: out("/usr/bin/sw_vers", &["-buildVersion"]),
            commit: if dirty.is_empty() {
                head
            } else {
                format!("{head}+dirty")
            },
        }
    }
}

/// `debug build` or `release build`, by the aterm's place under `target/`.
fn build_kind(aterm: &Path) -> &'static str {
    let s = aterm.to_string_lossy();
    if s.contains("/target/debug/") {
        "a debug build"
    } else if s.contains("/target/release/") {
        "a release build"
    } else {
        "a build"
    }
}

/// The record's marker around the invocations table's rows.
const INVOCATIONS: &str = "invocations";
/// The record's marker around its hand-written notes.
const NOTES: &str = "notes";

/// What a record carries forward from the file it replaces.
#[derive(Default)]
struct Kept {
    /// The earlier invocations' rows.
    rows: Vec<String>,
    /// The hand-written notes.
    notes: Vec<String>,
}

/// Lines of `text` between `<!-- <mark> -->` and `<!-- /<mark> -->`.
fn between(text: &str, mark: &str) -> Vec<String> {
    let (open, close) = (format!("<!-- {mark} -->"), format!("<!-- /{mark} -->"));
    if !text.lines().any(|l| l == close) {
        return Vec::new();
    }
    text.lines()
        .skip_while(|l| *l != open)
        .skip(1)
        .take_while(|l| *l != close)
        .map(str::to_owned)
        .collect()
}

/// The boot session's sid of `inst` (the one session a fresh instance has).
fn boot_sid(world: &World, inst: &Inst) -> Result<String, Fail> {
    let body = world.ctl_ok(inst, &["sessions"]).map_err(Fail::die)?;
    body.lines()
        .find(|l| l.starts_with(|c: char| c.is_ascii_digit()))
        .and_then(|row| row.split_whitespace().nth(1))
        .map(str::to_owned)
        .ok_or_else(|| Fail::die(format!("m{:02}: no boot session: {body}", inst.i)))
}

/// Machine 0's host name, as the survey heads its own block (its `== <host>`).
fn read_host(world: &World) -> Result<String, Fail> {
    let text = world
        .ctl_ok(&world.insts[0], &["survey"])
        .map_err(Fail::die)?;
    text.lines()
        .find_map(|l| l.strip_prefix("== "))
        .and_then(|rest| rest.split(" pid ").next())
        .map(str::to_owned)
        .ok_or_else(|| Fail::die(format!("machine 0's host name: {text}")))
}

/// Load the environment and directory recorded by machine 0's `claude` fixture.
fn reader_of(inst: &Inst) -> Result<Reader, Fail> {
    use std::os::unix::ffi::OsStringExt as _;
    let dir = inst.home().join(fixture::ENV_DIR);
    let file = std::fs::read_dir(&dir)
        .ok()
        .and_then(|mut d| d.find_map(|e| e.ok().map(|e| e.path())))
        .ok_or_else(|| {
            Fail::die(format!(
                "m00's `claude` session left no environment under {}",
                dir.display()
            ))
        })?;
    let bytes = std::fs::read(&file).map_err(|e| Fail::die(format!("{}: {e}", file.display())))?;
    let env: Vec<(OsString, OsString)> = bytes
        .split(|&b| b == 0)
        .filter_map(|kv| {
            let at = kv.iter().position(|&b| b == b'=')?;
            Some((
                OsString::from_vec(kv[..at].to_vec()),
                OsString::from_vec(kv[at + 1..].to_vec()),
            ))
        })
        .collect();
    if !env.iter().any(|(k, _)| k == "PATH") {
        return Err(Fail::die(format!(
            "{}: the session's environment has no PATH",
            file.display()
        )));
    }
    Ok(Reader {
        env,
        cwd: inst.home(),
    })
}

/// Initialize machine 0 as fleet issuer, join all other instances, and register loopback peers manually.
fn form_fleet(world: &World) -> Result<Joins, Fail> {
    let issuer = &world.insts[0];
    let out = world.aterm_in(
        &issuer.root,
        &[
            "fleet",
            "init",
            "--host",
            "127.0.0.1",
            "--port",
            &issuer.port.to_string(),
            "--allow",
            "content",
        ],
        None,
    );
    if out.code != 0 {
        return Err(Fail::die(format!(
            "machine 0 `fleet init` exited {}: {}{}",
            out.code,
            out.stdout.trim(),
            out.stderr.trim()
        )));
    }
    world.await_listening(issuer).map_err(Fail::die)?;
    let code = std::fs::read_to_string(issuer.state().join("fleet-join-code"))
        .map_err(|e| Fail::die(format!("the join code: {e}")))?;
    println!(
        "  fleet init on m00 (issuer); joining {} members (loopback: each registration taken \
         out, then peers add on the issuer)…",
        world.insts.len() - 1
    );
    let mut joins = Joins::default();
    for inst in &world.insts[1..] {
        let i = inst.i;
        let out = world.aterm_in(
            &inst.root,
            &[
                "fleet",
                "join",
                "--host",
                "127.0.0.1",
                "--port",
                &inst.port.to_string(),
                "--allow",
                "content",
            ],
            Some(&code),
        );
        if out.code != 0 || !inst.state().join("fleet.key").exists() {
            return Err(Fail::die(format!(
                "m{i:02} `fleet join` exited {} and did not provision the member: {}{}",
                out.code,
                out.stdout.trim(),
                out.stderr.trim()
            )));
        }
        joins.registered += 1;
        world.await_listening(inst).map_err(Fail::die)?;
        let addr = format!("127.0.0.1:{}", inst.port);
        let out = world.aterm_in(
            issuer.root.as_path(),
            &["fleet", "peers", "rm", &addr],
            None,
        );
        if out.code != 0 {
            return Err(Fail::die(format!(
                "m00 `fleet peers rm {addr}` exited {}: {}{}",
                out.code,
                out.stdout.trim(),
                out.stderr.trim()
            )));
        }
        stop::check()?;
    }
    for inst in &world.insts[1..] {
        let addr = format!("127.0.0.1:{}", inst.port);
        let out = world.aterm_in(
            issuer.root.as_path(),
            &["fleet", "peers", "add", &addr],
            None,
        );
        if out.code != 0 {
            return Err(Fail::die(format!(
                "m00 `fleet peers add {addr}` exited {}: {}{}",
                out.code,
                out.stdout.trim(),
                out.stderr.trim()
            )));
        }
        stop::check()?;
    }
    Ok(joins)
}

/// Session plan beyond the boot shell for machine `idx` of `n`.
fn session_plan(idx: usize, n: usize) -> Vec<(Kind, usize)> {
    let modes = [Mode::Busy, Mode::Question, Mode::Prompt, Mode::Idle];
    let hostile = idx == 0 || idx == hostile_peer(n);
    vec![
        (Kind::Build, idx),
        if hostile {
            (Kind::Claude(HOSTILE_MODE), HOSTILE)
        } else {
            (Kind::Claude(modes[idx % 4]), idx)
        },
        (Kind::Claude(Mode::Idle), idx + 1),
        (Kind::Claude(Mode::Idle), idx + 2),
    ]
}

/// Worktree crate directory for machine `idx`'s session `k`.
fn workdir(idx: usize, k: usize) -> String {
    format!("$HOME/Developer/aterm-mesh-wt-{idx:02}-{k}/crates/aterm-net")
}

/// Spawn the planned sessions ([`session_plan`]) on each instance.
fn give_sessions(world: &mut World) -> Result<(), Fail> {
    for idx in 0..world.insts.len() {
        let hostile = idx == 0 || idx == hostile_peer(world.insts.len());
        for (k, (kind, n)) in session_plan(idx, world.insts.len()).into_iter().enumerate() {
            let sid = {
                let inst = &world.insts[idx];
                let body = world.ctl_ok(inst, &["spawn"]).map_err(Fail::die)?;
                body.split_whitespace()
                    .find(|w| w.starts_with("s-"))
                    .ok_or_else(|| Fail::die(format!("m{idx:02}: spawn: {body}")))?
                    .to_owned()
            };
            let bin = world.bin.display();
            let cmd = match kind {
                Kind::Build => format!("{bin}/build-loop {n}"),
                Kind::Claude(mode) => format!("{bin}/claude {} {n}", mode.word()),
                Kind::Shell => unreachable!("the boot session is the shell"),
            };
            let dir = workdir(idx, k);
            let inst = &world.insts[idx];
            world.type_line(inst, &sid, &format!("mkdir -p \"{dir}\" && cd \"{dir}\""))?;
            world.type_line(inst, &sid, &cmd)?;
            world.insts[idx].sessions.push(Sess { sid, kind, n });
        }
        if hostile {
            let boot = world.insts[idx].boot_sid().to_owned();
            let inst = &world.insts[idx];
            let _ = world.type_line(
                inst,
                &boot,
                "printf '\\033]2;SURVEYMARK \\342\\200\\256evil\\007'",
            );
            let _ = world.ctl(
                inst,
                &[
                    &format!("@{boot}"),
                    "meta",
                    "set",
                    "title",
                    "SURVEYMARK evil",
                ],
            );
        }
        stop::check()?;
    }
    Ok(())
}

/// Wait until every instance's `survey --json` shows all expected sessions and Claude excerpts.
fn settle(world: &World) -> Result<(), Fail> {
    println!("  waiting for sessions to settle…");
    for inst in &world.insts {
        let want = want_sessions(inst);
        let mut last = String::new();
        let ok = crate::wait_until(world::HANG, || {
            match world.ctl(inst, &["survey", "--json"]) {
                out if out.code == 0 => {
                    last = out.stdout.clone();
                    sessions_ready(&last, &want)
                }
                _ => false,
            }
        });
        if !ok {
            return Err(Fail::die(format!(
                "m{:02}: its sessions never settled; last survey:\n{}",
                inst.i,
                crate::util::one_line(&last, 400)
            )));
        }
        stop::check()?;
    }
    Ok(())
}

/// Whether `json` shows every expected session with non-empty excerpts for `claude` sessions.
fn sessions_ready(json: &str, want: &[WantSession]) -> bool {
    let Ok(value) = aterm_json::from_str::<aterm_json::Value>(json) else {
        return false;
    };
    let Some(rows) = value
        .as_array()
        .and_then(|a| a.first())
        .and_then(|h| h.get("sessions"))
        .and_then(aterm_json::Value::as_array)
    else {
        return false;
    };
    want.iter().all(|w| {
        rows.iter().any(|r| {
            let s = |k: &str| r.get(k).and_then(aterm_json::Value::as_str);
            let base = s("sid") == Some(&w.sid)
                && s("program") == Some(&w.program)
                && s("agent") == Some(&w.agent);
            if w.program == "claude" {
                base && s("prompt").is_some_and(|p| p != "-" && !p.is_empty())
                    && s("said").is_some_and(|p| p != "-" && !p.is_empty())
            } else {
                base
            }
        })
    })
}

/// What one instance's sessions must show (the boot shell is `zsh`).
fn want_sessions(inst: &Inst) -> Vec<WantSession> {
    inst.sessions
        .iter()
        .map(|s| WantSession {
            sid: s.sid.clone(),
            program: s.kind.program().to_owned(),
            agent: s.kind.agent().to_owned(),
        })
        .collect()
}

/// What every machine must show in the fleet read.
fn want_of(world: &World) -> Vec<WantInstance> {
    world
        .insts
        .iter()
        .map(|inst| WantInstance {
            i: inst.i,
            pid: inst.pid,
            port: inst.port,
            local: inst.i == 0,
            sessions: want_sessions(inst),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_size_floor_is_four_machines() {
        let a = |v: &[&str]| parse(&v.iter().map(|s| (*s).to_owned()).collect::<Vec<_>>());
        assert!(a(&["--n", "3"]).is_err());
        assert_eq!(a(&["--n", "4"]).map(|a| a.n).ok(), Some(4));
        assert!(a(&["--record"]).is_ok_and(|a| a.record && a.n == 30));
    }

    #[test]
    fn what_a_record_keeps_is_the_lines_between_its_markers() {
        let text = "x\n| at | head |\n<!-- invocations -->\n| a | 1 |\n| b | 2 |\n\
                    <!-- /invocations -->\n| run | 1 |\n<!-- notes -->\n- found\n\n\
                    <!-- /notes -->\n";
        assert_eq!(between(text, INVOCATIONS), ["| a | 1 |", "| b | 2 |"]);
        assert_eq!(between(text, NOTES), ["- found", ""]);
        assert!(between("no markers\n| a |\n", INVOCATIONS).is_empty());
        assert!(between("<!-- notes -->\n- a\n", NOTES).is_empty());
    }

    /// Verify that `tty::one_line` produces every expected substring in [`HOSTILE_ESCAPED`].
    #[test]
    fn the_hostile_words_escape_to_the_forms_the_check_looks_for() {
        let escaped = aterm_codec::tty::one_line(fixture::HOSTILE_SAID, 160);
        for form in HOSTILE_ESCAPED {
            assert!(escaped.contains(form), "{form} in {escaped}");
        }
        assert!(!escaped.contains("trust=human"), "{escaped}");
    }
}
