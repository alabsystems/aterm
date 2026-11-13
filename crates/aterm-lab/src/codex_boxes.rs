// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `codex-boxes`: how many of a codex-alab's approval boxes its tab's session log holds as claimed
//! steps (GAP 26, `program-claimed-boxes` in aterm-gui's SPECS), on the proof's own rig
//! ([`super::start`]): the real package, the loopback model ([`crate::codex_alab_model`]), a
//! private headless aterm. Each run is a tab whose model asks for [`BOXES`] escalated commands,
//! one a response; the lab holds the tab (`ctl hold`) before each is sent, so codex-alab's checks
//! wait and the box goes to codex's TUI, and lifts the hold once `status` shows the box, so
//! codex-alab answers it there. It reports the boxes forced against the claim steps the log
//! opened (the undercount: a box a sweep showed whose mark opened no step, such as a `blocked`
//! coalesced past the bucket; a box shown and answered between two sweeps never reaches the
//! count, since the hold keeps each box on screen until a sweep shows it), and what closed them
//! (`out=`, `by=`), never failing on those; it fails on a stalled run (no proven claim within
//! 180 s, a hold that did not go on or lift, a box or the done line past its one-minute hang
//! detector, or a log that did not read), a blocked mark with no claim step or a step left open
//! ([`box_steps`]; the wait for the last close is a one-minute hang detector too). This module
//! also holds the structure `codex-alab-proof`'s box legs check ([`Proof::claimed_boxes_hold`]).

use super::*;
use crate::codex_alab_model::{BOXES, box_tag, release_box};

/// A wait for something that must happen: a one-minute hang detector, never a budget.
const HANG: Duration = Duration::from_secs(60);

/// The leg each run takes, one a run: `--runs` is at most this many.
const RUN_LEGS: [&str; 9] = [
    "boxes1", "boxes2", "boxes3", "boxes4", "boxes5", "boxes6", "boxes7", "boxes8", "boxes9",
];

/// What `log kinds=` reads for the box structure.
const BOX_KINDS: &str = "kinds=program,approval,question,turn";

/// One record line of `log` (`<seq> <op> <node> <kind> <key>=<value>…`).
#[derive(Clone, Debug, PartialEq, Eq)]
struct LogLine {
    seq: u64,
    op: String,
    node: String,
    kind: String,
    words: Vec<(String, String)>,
}

impl LogLine {
    fn parse(line: &str) -> Option<Self> {
        let mut w = line.split(' ');
        let seq = w.next()?.parse().ok()?;
        let op = w.next()?.to_string();
        let node = w.next()?.to_string();
        let kind = w.next()?.to_string();
        let words = w
            .filter_map(|t| t.split_once('='))
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        Some(Self {
            seq,
            op,
            node,
            kind,
            words,
        })
    }

    fn get(&self, key: &str) -> Option<&str> {
        self.words
            .iter()
            .find(|(k, _)| k == key)
            .map(|(_, v)| v.as_str())
    }

    /// Whether it is the engine's claim (`src=engine conf=claim`).
    fn claimed(&self) -> bool {
        self.get("src") == Some("engine") && self.get("conf") == Some("claim")
    }
}

/// A claimed box step as the log holds it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct ClaimStep {
    pub(super) node: String,
    /// `approval` or `question`.
    pub(super) kind: String,
    /// An approval's `box=` (`other`).
    pub(super) boxed: Option<String>,
    /// The run it is under.
    run: String,
    /// The node it hangs from (`par=`): a turn, or the run itself.
    under: String,
    opened: u64,
    /// Its close's sequence, `out=` and `by=` (`-` for nobody).
    closed: Option<(u64, String, String)>,
}

impl ClaimStep {
    /// `p<n> approval out=passed by=-`, or `p<n> approval open`.
    fn said(&self) -> String {
        match &self.closed {
            Some((_, out, by)) => format!("{} {} out={out} by={by}", self.node, self.kind),
            None => format!("{} {} open", self.node, self.kind),
        }
    }
}

/// What a tab's log says of its claimed box steps.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(super) struct BoxSteps {
    pub(super) steps: Vec<ClaimStep>,
    /// Each `blocked` mark on a permission or a question with no claim step of its kind
    /// standing after it ([`box_steps`]): `<seq> ask=<ask>`.
    pub(super) unmet: Vec<String>,
    /// Each close that counts boxes: `<node> appr=<a> qs=<q>`.
    pub(super) counts: Vec<String>,
}

impl BoxSteps {
    /// The steps no close ended yet.
    pub(super) fn open(&self) -> Vec<&str> {
        self.steps
            .iter()
            .filter(|s| s.closed.is_none())
            .map(|s| s.node.as_str())
            .collect()
    }

    /// How many closed steps said each `out=` word (`by`: each `by=` word instead), as
    /// `<word>:<n>` in first-seen order, `-` for none.
    fn tally(&self, by: bool) -> String {
        let mut seen: Vec<(&str, usize)> = Vec::new();
        for (_, out, who) in self.steps.iter().filter_map(|s| s.closed.as_ref()) {
            let said = if by { who.as_str() } else { out.as_str() };
            match seen.iter_mut().find(|(w, _)| *w == said) {
                Some((_, n)) => *n += 1,
                None => seen.push((said, 1)),
            }
        }
        if seen.is_empty() {
            return "-".to_string();
        }
        seen.iter()
            .map(|(w, n)| format!("{w}:{n}"))
            .collect::<Vec<_>>()
            .join(" ")
    }
}

/// THE STRUCTURE OF A CODEX TAB'S CLAIMED BOXES, from its `log kinds=program,approval,question,turn`
/// record lines: each written `program state=blocked ask=permission` (or `ask=question`) mark
/// on a run has a claim step of its kind (`approval`, `question`; `src=engine conf=claim`)
/// under that run (or one of its turns) standing after it: opened before the run's next program
/// mark and ended by that mark or a later one, or by its turn's close before it (the sweep feeds
/// a turn's end edge before the mark that shows the rest, and a turn's close is its standing
/// box's leave); for the run's last mark, one not closed before it. What closed each step is
/// read, never judged.
pub(super) fn box_steps(lines: &[String]) -> BoxSteps {
    let recs: Vec<LogLine> = lines.iter().filter_map(|l| LogLine::parse(l)).collect();
    let turn_run: Vec<(&str, &str)> = recs
        .iter()
        .filter(|r| r.op == "open" && r.kind == "turn")
        .filter_map(|r| Some((r.node.as_str(), r.get("par")?)))
        .collect();
    let run_of = |par: &str| -> String {
        turn_run
            .iter()
            .find(|(t, _)| *t == par)
            .map_or(par, |&(_, r)| r)
            .to_string()
    };
    let mut out = BoxSteps::default();
    for r in &recs {
        let decision = r.kind == "approval" || r.kind == "question";
        if r.op == "open" && decision && r.claimed() {
            out.steps.push(ClaimStep {
                node: r.node.clone(),
                kind: r.kind.clone(),
                boxed: r.get("box").map(str::to_string),
                run: run_of(r.get("par").unwrap_or("-")),
                under: r.get("par").unwrap_or("-").to_string(),
                opened: r.seq,
                closed: None,
            });
        } else if r.op == "close"
            && decision
            && let Some(s) = out.steps.iter_mut().find(|s| s.node == r.node)
        {
            s.closed = Some((
                r.seq,
                r.get("out").unwrap_or("-").to_string(),
                r.get("by").unwrap_or("-").to_string(),
            ));
        }
        if r.op == "close" && (r.get("appr").is_some() || r.get("qs").is_some()) {
            out.counts.push(format!(
                "{} appr={} qs={}",
                r.node,
                r.get("appr").unwrap_or("0"),
                r.get("qs").unwrap_or("0")
            ));
        }
    }
    let marks: Vec<&LogLine> = recs
        .iter()
        .filter(|r| r.op == "mark" && r.kind == "program")
        .collect();
    let turn_closes: Vec<(&str, u64)> = recs
        .iter()
        .filter(|r| r.op == "close" && r.kind == "turn")
        .map(|r| (r.node.as_str(), r.seq))
        .collect();
    // Whether `s`'s turn closed after `s` did and before `next`: the turn's close ended it.
    let turn_ended = |s: &ClaimStep, closed: u64, next: u64| {
        turn_closes
            .iter()
            .any(|&(t, at)| t == s.under && closed < at && at < next)
    };
    for (i, m) in marks.iter().enumerate() {
        let kind = match (m.get("state"), m.get("ask")) {
            (Some("blocked"), Some("permission")) => "approval",
            (Some("blocked"), Some("question")) => "question",
            _ => continue,
        };
        let next = marks[i + 1..]
            .iter()
            .find(|n| n.node == m.node)
            .map(|n| n.seq);
        let stands = out.steps.iter().any(|s| {
            s.kind == kind
                && s.run == m.node
                && match (next, s.closed.as_ref().map(|(c, _, _)| *c)) {
                    (_, Some(c)) if c < m.seq => false,
                    (Some(n), None) => s.opened < n,
                    (Some(n), Some(c)) => s.opened < n && (c > n || turn_ended(s, c, n)),
                    (None, _) => true,
                }
        });
        if !stands {
            out.unmet
                .push(format!("{} ask={}", m.seq, m.get("ask").unwrap_or("-")));
        }
    }
    out
}

impl Proof {
    /// `sid`'s log as `log kinds=program,approval,question,turn` reads it, its record lines, as a
    /// reading ([`reading`]).
    fn box_log(&self, sid: &str) -> Result<Vec<String>, String> {
        let asked = format!("@{sid} log {BOX_KINDS}");
        let r = reading(&asked, self.on(sid, &["log", BOX_KINDS]))?;
        Ok(r.stdout
            .lines()
            .filter(|l| l.starts_with(|c: char| c.is_ascii_digit()))
            .map(str::to_string)
            .collect())
    }

    /// `sid`'s box structure once every claim step it holds closed, the wait a one-minute hang
    /// detector: the last reading, and whether they all closed.
    fn boxes_closed(&self, sid: &str) -> Result<(Result<BoxSteps, String>, bool), Stopped> {
        let mut said = Err("not read".to_string());
        let closed = Self::wait_for(HANG, Duration::from_millis(500), || {
            said = self.box_log(sid).map(|l| box_steps(&l));
            said.as_ref().is_ok_and(|s| s.open().is_empty())
        })?;
        Ok((said, closed))
    }

    /// GAP 26 ON A PROOF LEG: every `program state=blocked ask=permission|question` mark on the
    /// leg's run has its claim step ([`box_steps`]), and every claim step closed by the leg's end
    /// (the wait for the last close a one-minute hang detector); `boxed`, the approval steps
    /// `box=other` the leg must hold (the box leg's one box, which stands until `send y`).
    /// Reported, never failed: each step's `out=` and `by=` (whose `by=ctl` needs the leave
    /// inside `STEP_ACT_MS` of the input) and the turns' and runs' `appr=`/`qs=`.
    pub(super) fn claimed_boxes_hold(
        &mut self,
        leg: &Leg,
        boxed: Option<usize>,
    ) -> Result<(), Stopped> {
        let name = leg.name;
        let (said, closed) = self.boxes_closed(&leg.sid)?;
        let s = match said {
            Ok(s) => s,
            Err(why) => {
                self.check(
                    &format!("{name}: its log names its claimed box steps (unread: {why})"),
                    false,
                );
                return Ok(());
            }
        };
        println!(
            "    {name}: {} claimed box step(s) [{}]; counted: [{}]",
            s.steps.len(),
            s.steps
                .iter()
                .map(ClaimStep::said)
                .collect::<Vec<_>>()
                .join(", "),
            s.counts.join(", ")
        );
        self.check(
            &format!(
                "{name}: each program mark blocked on a permission or a question has its claim \
                 step standing after it, to a later mark or its turn's close (unmet: {})",
                if s.unmet.is_empty() {
                    "none".to_string()
                } else {
                    s.unmet.join(", ")
                }
            ),
            s.unmet.is_empty(),
        );
        self.check(
            &format!(
                "{name}: every claim step closed by the leg's end (open: {})",
                if closed {
                    "none".to_string()
                } else {
                    s.open().join(" ")
                }
            ),
            closed,
        );
        if let Some(n) = boxed {
            let approvals = s
                .steps
                .iter()
                .filter(|st| st.kind == "approval" && st.boxed.as_deref() == Some("other"))
                .count();
            self.check(
                &format!(
                    "{name}: its box is {n} approval step box=other src=engine conf=claim \
                     (the log holds {approvals})"
                ),
                approvals == n,
            );
        }
        Ok(())
    }
}

/// One run's reading.
#[derive(Debug, Default)]
struct BoxRun {
    leg: &'static str,
    sid: String,
    /// The boxes `status` showed under the lab's hold (`program_state=blocked:permission`).
    forced: usize,
    steps: BoxSteps,
    /// Whether every claim step closed within the hang detector.
    closed: bool,
    /// codex-alab's acts on the tab, in order.
    acts: Vec<String>,
    /// Why the run ended before its last box.
    stalled: Option<String>,
}

impl BoxRun {
    /// The claim approval steps opened.
    fn opened(&self) -> usize {
        self.steps
            .steps
            .iter()
            .filter(|s| s.kind == "approval")
            .count()
    }

    fn said(&self) -> String {
        let opened = self.opened();
        format!(
            "n={} forced, {opened} claim approval steps opened, undercount {}; closes out= {}; by= {}; \
             acts {} (approve-all@v1 {}); counted [{}]; unmet [{}]",
            self.forced,
            i64::try_from(self.forced).unwrap_or(i64::MAX)
                - i64::try_from(opened).unwrap_or(i64::MAX),
            self.steps.tally(false),
            self.steps.tally(true),
            self.acts.len(),
            self.acts.iter().filter(|a| *a == "approve-all@v1").count(),
            self.steps.counts.join(", "),
            self.steps.unmet.join(", "),
        )
    }
}

/// Whether `sid`'s `status` shows codex's approval box.
fn shows_box(proof: &Proof, sid: &str) -> bool {
    field(&proof.status(sid), "program_state").as_deref() == Some("blocked:permission")
}

/// One run: the leg `name`'s tab asked for [`BOXES`] boxes, each forced to the TUI under a hold.
fn run_boxes(proof: &mut Proof, name: &'static str) -> Result<BoxRun, Fail> {
    let leg = proof.spawn_leg(
        name,
        "Run the twenty commands, one at a time.",
        "",
        Folder::Scratch,
        None,
    )?;
    let mut run = BoxRun {
        leg: name,
        sid: leg.sid.clone(),
        ..BoxRun::default()
    };
    if proof
        .claim_with_limit(&leg, Duration::from_secs(180))?
        .is_none()
    {
        run.stalled = Some("codex-alab held no proven claim".to_string());
        return Ok(run);
    }
    let poll = Duration::from_millis(100);
    for k in 0..BOXES {
        let n = k + 1;
        if !proof.await_tag(&leg, &box_tag(k), HANG)? {
            run.stalled = Some(format!("box {n}: the model was not asked for it"));
            break;
        }
        let on = proof.inst.ctl(&["hold", &leg.sid, "on", "reason=lab"]);
        release_box(name, k);
        if !on.first().contains("hold=1") {
            run.stalled = Some(format!("box {n}: the hold did not go on ({})", on.first()));
            break;
        }
        let shown = Proof::wait_for(HANG, poll, || shows_box(proof, &leg.sid))?;
        let off = proof.inst.ctl(&["hold", &leg.sid, "off"]);
        if !shown {
            run.stalled = Some(format!("box {n}: never shown under the hold"));
            break;
        }
        run.forced += 1;
        if !off.first().contains("hold=0") {
            run.stalled = Some(format!("box {n}: the hold did not lift ({})", off.first()));
            break;
        }
        if !Proof::wait_for(HANG, poll, || !shows_box(proof, &leg.sid))? {
            run.stalled = Some(format!("box {n}: still shown after the hold lifted"));
            break;
        }
    }
    if run.stalled.is_none() && !proof.await_tag(&leg, "done", HANG)? {
        run.stalled = Some("the model never heard the last box ran".to_string());
    }
    // Parked: Cruise would go on typing into a tab the lab is done with.
    let _ = proof.inst.ctl(&["hold", &leg.sid, "on", "reason=lab"]);
    let (said, closed) = proof.boxes_closed(&leg.sid)?;
    run.closed = closed;
    match said {
        Ok(s) => run.steps = s,
        Err(why) => run.stalled = Some(format!("its log could not be read: {why}")),
    }
    run.acts = proof.acts(&leg.sid);
    Ok(run)
}

/// `codex-boxes [--runs N] [--package DIR] [--aterm PATH] [--keep]`.
/// # Errors
/// Usage, a rig that could not start, or an interrupt.
pub fn cmd_codex_boxes(args: &[String]) -> Res {
    stop::install();
    let mut a = args.to_vec();
    let keep = instance::take_flag(&mut a, "--keep");
    let aterm = instance::take_value(&mut a, "--aterm")?;
    let given = instance::take_value(&mut a, "--package")?;
    let runs = match instance::take_value(&mut a, "--runs")? {
        None => 3,
        Some(n) => match n.parse::<usize>() {
            Ok(n) if (1..=RUN_LEGS.len()).contains(&n) => n,
            _ => {
                return Err(Fail::usage(format!(
                    "codex-boxes: --runs takes 1 to {} ({LAB} help)",
                    RUN_LEGS.len()
                )));
            }
        },
    };
    if let Some(extra) = a.first() {
        return Err(Fail::usage(format!(
            "codex-boxes: unexpected '{extra}' ({LAB} help)"
        )));
    }
    let not_run = |why: String| Fail::exit(3, format!("NOT RUN: {why}"));
    let aterm = instance::resolve_aterm(aterm).map_err(not_run)?;
    let home = crate::paths::paths().need_store_home().map_err(not_run)?;
    let pkg = package(given.as_deref(), home).map_err(not_run)?;
    println!(
        "codex-boxes: {runs} run(s) of {BOXES} boxes each on {}; aterm {}",
        pkg.display(),
        aterm.display()
    );
    let mut proof = start(
        aterm,
        (&pkg, None),
        keep,
        (
            ["--columns", "120", "--lines", "40"]
                .map(str::to_string)
                .to_vec(),
            "",
        ),
    )?;
    let pkg_a = proof.s.join("pkg-a");
    let measured = (|| -> Result<Vec<BoxRun>, Fail> {
        match proof.ready_rig(&pkg_a) {
            Ok(_) => {}
            Err(Unready::Unlinked(why)) => {
                return Err(Fail::die(format!(
                    "codex-boxes: the package is not linked: {why}"
                )));
            }
            Err(Unready::NoSocket(f)) => return Err(f),
            Err(Unready::Stopped(stopped)) => return Err(stopped.into()),
        }
        let mut done = Vec::new();
        for name in RUN_LEGS.iter().take(runs) {
            let run = run_boxes(&mut proof, name)?;
            println!("  {} @{}: {}", run.leg, run.sid, run.said());
            done.push(run);
        }
        Ok(done)
    })();
    proof.end_all();
    let done = measured?;
    let forced: usize = done.iter().map(|r| r.forced).sum();
    let opened: usize = done.iter().map(BoxRun::opened).sum();
    let mut all = BoxSteps::default();
    for r in &done {
        all.steps.extend(r.steps.steps.iter().cloned());
    }
    let acts: usize = done.iter().map(|r| r.acts.len()).sum();
    let under =
        i64::try_from(forced).unwrap_or(i64::MAX) - i64::try_from(opened).unwrap_or(i64::MAX);
    println!(
        "total: n={forced} boxes forced, {opened} claim approval steps opened, undercount {under} \
         ({:.1}%); closes out= {}; by= {}; codex-alab acts {acts}",
        if forced == 0 {
            0.0
        } else {
            100.0 * under as f64 / forced as f64
        },
        all.tally(false),
        all.tally(true),
    );
    println!(
        "Stage B (box= acts, hits closed out=approved by=ctl, misses, miss lag_p50/p95/p99_ms): n/a: \
         no codex-alab act names its box yet"
    );
    let mut fails: Vec<String> = proof.fails.clone();
    for r in &done {
        if let Some(why) = &r.stalled {
            fails.push(format!("{}: stalled: {why}", r.leg));
        }
        if !r.steps.unmet.is_empty() {
            fails.push(format!(
                "{}: blocked marks with no claim step: {}",
                r.leg,
                r.steps.unmet.join(", ")
            ));
        }
        if !r.closed {
            fails.push(format!(
                "{}: claim steps left open: {}",
                r.leg,
                r.steps.open().join(" ")
            ));
        }
    }
    if fails.is_empty() {
        println!("PASS");
        Ok(0)
    } else {
        println!("FAIL: {} check(s)", fails.len());
        for f in &fails {
            println!("  - {f}");
        }
        Ok(1)
    }
}

#[cfg(test)]
#[path = "codex_boxes_tests.rs"]
mod tests;
