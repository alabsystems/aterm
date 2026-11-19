// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use std::collections::{BTreeMap, BTreeSet};

use super::*;

const TIP: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
const OPENED: &str = "cccccccccccccccccccccccccccccccccccccccc";
const MOVED: &str = "dddddddddddddddddddddddddddddddddddddddd";
const HOP: &str = "eeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee";
const FIX: &str = "1111111111111111111111111111111111111111";
const GREEN: &str = "2222222222222222222222222222222222222222";
const LATER: &str = "3333333333333333333333333333333333333333";
const FIX2: &str = "4444444444444444444444444444444444444444";

/// A scripted world: what it reads is its fields, and running a verb applies that verb's effect
/// to them, as the release would to origin and this clone, then ends with the scripted code.
#[derive(Default)]
struct Lab {
    now: u64,
    main: Main,
    journal: Option<Journal>,
    holder: Option<String>,
    publishes: bool,
    every: Every,
    adopt: bool,
    /// The input hash a run that ends exit 1 names in its caller-owned repair line, where it
    /// prints one.
    caller: Option<String>,
    /// A start that ends exit 1 has journaled its line first, as a red of R does.
    start_journals: bool,
    /// What the pump had kept of what it does when each run began.
    doing_at_run: Vec<Option<Doing>>,
    /// The repair record, oldest first.
    repairs: Vec<Kept>,
    /// The worker a red of opening the next version opened, and how many reds of it there were.
    opening: Option<(String, Report)>,
    opening_reds: usize,
    /// Commits the line refuses as a hop.
    unfit: BTreeMap<String, String>,
    /// (commit, tip): the tip's history holds the commit.
    landed: BTreeSet<(String, String)>,
    /// The newest judged pass that descends from each commit.
    greens: BTreeMap<String, String>,
    green: Option<String>,
    /// Judged passes that cannot take a line in a re-base, and why.
    unfit_onto: BTreeMap<String, String>,
    /// Each question whether a pass can take a line: (version, head, green, fix).
    onto_asked: Vec<(String, String, String, Option<String>)>,
    /// What the pump published, cycle by cycle.
    facts: Vec<Fact>,
    /// How many reads of main came before each fact was published.
    facts_after_reads: Vec<usize>,
    /// How many reads of main there were.
    main_reads: usize,
    /// What the fleet line's holder's Mac says of its pumps: `None` answers that it runs one,
    /// dead, that holds no journal of the line.
    holder_pump: Option<Result<Option<Vec<PumpRow>>, String>>,
    /// How often the holder's Mac was asked after its pump.
    holder_pump_asked: usize,
    /// A re-base that ends 3 or 130 leaves its supersession undecided in candidate.toml.
    stops_undecided: bool,
    /// The exit of each run, in order; a run past the script succeeds.
    exits: Vec<i32>,
    /// A read of main that fails, once.
    main_down: bool,
    /// A read of the journal through origin that fails, once (`Host::journal` asks origin for the
    /// version's tag); the journal's own files still read ([`World::journal_here`]).
    journal_down: bool,
    /// Each run's end makes the next read of the journal through origin fail.
    journal_down_after_run: bool,
    /// What a red run does to the repair record.
    opens: Option<(String, Report)>,
    ran: Vec<Verb>,
    said: Vec<String>,
    waited: Vec<u64>,
    remembered: Option<Memory>,
    /// How many waits the loop is given before it is told to stop.
    patience: usize,
    /// The time of the last look this clone's pump published before any fact here: its survey
    /// record from a process before this one.
    looked: Option<u64>,
    /// Seconds the Mac sleeps right after the next read of main returns, once.
    asleep_after_main: u64,
    /// Seconds the Mac sleeps inside the next read of main, after its read of the claim, once.
    asleep_in_main: u64,
    /// Where another holder moves the fleet line while the next run goes on, once.
    line_moves_during_run: Option<String>,
    /// The next run starts no release process (its verb has no Linux declaration to be handed),
    /// once.
    unstarted: bool,
    /// A re-base refused (exit 1 or 2) that found its supersession undecided restores the journal
    /// to the head it retires, as the release's restore of its selection does.
    refusal_restores: bool,
    /// Each renewal of a claim's lease the pump asked: the version and the holder.
    renewed: Vec<(String, Process)>,
}

/// One worker of the repair record: the version and R whose red opened it, its session, what
/// it reports now and when that red stopped its cut.
struct Kept {
    version: String,
    commit: String,
    sid: String,
    report: Report,
    at: u64,
}

impl Lab {
    /// Main declares 0.5.0, cut, with its fixtures on the tip: the state a finished release
    /// leaves.
    fn after_a_release() -> Self {
        Self {
            now: 1_000,
            main: Main {
                tip: TIP.into(),
                version: "0.5.0".into(),
                published: true,
                fixtures: true,
                ..Main::default()
            },
            patience: usize::MAX,
            ..Self::default()
        }
    }

    /// The next version is open and nothing holds a line of it.
    fn opened() -> Self {
        let mut lab = Self::after_a_release();
        lab.open();
        lab
    }

    fn open(&mut self) {
        self.main = Main {
            tip: OPENED.into(),
            version: "0.6.0".into(),
            published: false,
            fixtures: false,
            opened: Some(OPENED.into()),
            ..Main::default()
        };
    }

    fn line(&mut self, head: &str) -> &mut Journal {
        self.main.line = Some(head.into());
        self.journal.insert(Journal {
            version: self.main.version.clone(),
            head: head.into(),
            ..Journal::default()
        })
    }

    fn seen(&mut self) -> Seen {
        observe(self, &Memory::default()).unwrap()
    }

    fn seen_with(&mut self, memory: &Memory) -> Seen {
        observe(self, memory).unwrap()
    }

    /// The repair record gains, or its worker for these inputs now reports, `report`.
    fn report(&mut self, version: &str, commit: &str, sid: &str, report: Report) {
        let same = |kept: &Kept| kept.version == version && kept.commit == commit;
        let at = self
            .repairs
            .iter()
            .find(|kept| same(kept))
            .map_or(self.now, |kept| kept.at);
        self.repairs.retain(|kept| !same(kept));
        self.repairs.push(Kept {
            version: version.to_string(),
            commit: commit.to_string(),
            sid: sid.to_string(),
            report,
            at,
        });
    }

    /// `tip`'s history holds `commit`.
    fn lands(&mut self, commit: &str, tip: &str) {
        self.landed.insert((commit.to_string(), tip.to_string()));
    }
}

impl World for Lab {
    fn now(&mut self) -> u64 {
        self.now
    }
    fn main(&mut self) -> Result<Main, String> {
        self.main_reads += 1;
        if std::mem::take(&mut self.main_down) {
            return Err("origin did not answer".into());
        }
        // The claim is read first; the Mac may sleep in the rest of main's read.
        let main = Main {
            read_at: self.now,
            ..self.main.clone()
        };
        self.now += std::mem::take(&mut self.asleep_in_main);
        Ok(main)
    }
    fn journal(&mut self) -> Result<Option<Journal>, String> {
        // The look reads the journal right after main: the Mac sleeps between the two.
        self.now += std::mem::take(&mut self.asleep_after_main);
        if std::mem::take(&mut self.journal_down) {
            return Err("origin did not answer the tag lookup".into());
        }
        Ok(self.journal.clone())
    }
    fn journal_here(&mut self) -> Option<Journal> {
        self.journal.clone()
    }
    fn holder(&mut self) -> Option<String> {
        self.holder.clone()
    }
    fn publishes(&mut self) -> bool {
        self.publishes
    }
    fn every(&mut self) -> Every {
        self.every.clone()
    }
    fn me(&mut self) -> (u32, String) {
        (4242, "Tue Nov 17 10:00:00 2026".into())
    }
    fn adopts(&mut self) -> bool {
        self.adopt
    }
    fn opening(&mut self, _from: &str) -> Option<Opened> {
        self.opening.as_ref().map(|(sid, report)| Opened {
            sid: sid.clone(),
            commit: self.main.tip.clone(),
            at: 0,
            report: report.clone(),
        })
    }
    fn holds(&mut self, commit: &str, tip: &str) -> bool {
        self.landed.contains(&(commit.to_string(), tip.to_string()))
    }
    fn repair(&mut self, version: &str, commit: Option<&str>) -> Option<Opened> {
        self.repairs
            .iter()
            .rev()
            .find(|kept| kept.version == version && commit.is_none_or(|c| c == kept.commit))
            .map(|kept| Opened {
                sid: kept.sid.clone(),
                commit: kept.commit.clone(),
                at: kept.at,
                report: kept.report.clone(),
            })
    }
    fn admits(&mut self, fix: &str) -> Result<(), String> {
        self.unfit.get(fix).cloned().map_or(Ok(()), Err)
    }
    fn green(&mut self, head: &str) -> Option<String> {
        self.greens
            .get(head)
            .cloned()
            .or_else(|| self.green.clone())
    }
    fn holder_pump(&mut self) -> Result<Option<Vec<PumpRow>>, String> {
        self.holder_pump_asked += 1;
        self.holder_pump.clone().unwrap_or(Ok(Some(vec![PumpRow {
            alive: Some(false),
            line: format!("v{}", self.main.version),
            step: "wait".into(),
            own: Some(false),
            age: 0,
        }])))
    }
    fn onto(
        &mut self,
        version: &str,
        head: &str,
        green: &str,
        fix: Option<&str>,
    ) -> Result<(), String> {
        self.onto_asked.push((
            version.to_string(),
            head.to_string(),
            green.to_string(),
            fix.map(str::to_string),
        ));
        self.unfit_onto.get(green).cloned().map_or(Ok(()), Err)
    }
    fn publish(&mut self, fact: &Fact) {
        self.facts.push(fact.clone());
        self.facts_after_reads.push(self.main_reads);
    }
    fn looked(&mut self) -> Option<u64> {
        self.facts.last().map(|fact| fact.at).or(self.looked)
    }
    fn run(&mut self, verb: &Verb) -> Ran {
        self.ran.push(verb.clone());
        if std::mem::take(&mut self.unstarted) {
            return Ran {
                code: 2,
                last: "pump: the cut must declare Linux".into(),
                red: None,
                child: None,
                started: false,
            };
        }
        self.journal_down |= self.journal_down_after_run;
        if let Some(head) = self.line_moves_during_run.take() {
            self.main.line = Some(head);
        }
        self.doing_at_run.push(
            self.remembered
                .as_ref()
                .and_then(|memory| memory.doing.clone()),
        );
        self.now += 100;
        let code = if self.exits.is_empty() {
            0
        } else {
            self.exits.remove(0)
        };
        if code == 1 && *verb == Verb::OpenNext {
            // One worker per opened version, whatever tip each red was on
            // (`repair::opening_hash`): a later red names the worker already on it.
            self.opening_reds += 1;
            if self.opening.is_none() {
                self.opening = self.opens.take();
            }
        } else if code == 1
            && let Some(opened) = self.opens.take()
        {
            // The red is of R: this clone's line's head, else the commit the start named.
            let version = self.main.version.clone();
            let commit = match (verb, self.journal.as_ref()) {
                (Verb::Start(base), _) => base.clone(),
                (_, Some(journal)) => journal.head.clone(),
                _ => self.main.tip.clone(),
            };
            self.report(&version, &commit, opened.0.as_str(), opened.1);
        }
        if matches!(code, 3 | 130)
            && self.stops_undecided
            && let Verb::Rebase { onto, .. } = verb
        {
            // The run stopped after it journaled its supersession and before its write was
            // decided: candidate.toml holds it, at the replacement's base.
            let journal = self.journal.as_mut().unwrap();
            journal.undecided = Some(Undecided {
                from: journal.head.clone(),
                onto: onto.clone(),
            });
            journal.head.clone_from(onto);
        }
        if matches!(code, 1 | 2)
            && self.refusal_restores
            && let Verb::Rebase { .. } = verb
            && let Some(journal) = self.journal.as_mut()
            && let Some(pending) = journal.undecided.take()
        {
            // The release restores its selection: release.toml's journal, at the head the
            // supersession would have retired.
            journal.head = pending.from;
        }
        if code == 1
            && self.start_journals
            && let Verb::Start(base) = verb
        {
            let base = base.clone();
            self.line(&base);
        }
        if code == 0 {
            match verb {
                Verb::OpenNext => self.open(),
                Verb::Start(base) => {
                    let base = base.clone();
                    self.line(&base).prepared = true;
                }
                Verb::Adopt => {
                    let head = self.main.line.clone().unwrap();
                    self.line(&head).prepared = true;
                }
                Verb::Prepare => {
                    let journal = self.journal.as_mut().unwrap();
                    journal.prepared = true;
                    journal.prepared_at = self.now;
                }
                Verb::Fix(_) => {
                    let journal = self.journal.as_mut().unwrap();
                    journal.head = HOP.into();
                    journal.prepared = true;
                    journal.prepared_at = self.now;
                    self.main.line = Some(HOP.into());
                }
                // The supersession is recorded on main, which every clone reads, and the line
                // re-qualifies at its new base to PREPARED.
                Verb::Rebase { onto, .. } => {
                    let onto = onto.clone();
                    let journal = self.journal.as_mut().unwrap();
                    journal.head.clone_from(&onto);
                    journal.prepared = true;
                    journal.prepared_at = self.now;
                    self.main.line = Some(onto);
                    self.main.superseded = true;
                }
                Verb::Finish => {
                    let journal = self.journal.as_mut().unwrap();
                    journal.published = true;
                    journal.prepared = false;
                    journal.finished = true;
                    self.main.published = true;
                    self.main.fixtures = true;
                    self.main.line = None;
                }
            }
        }
        Ran {
            code,
            last: format!("final line of exit {code}"),
            red: self.caller.clone().filter(|_| code == 1),
            child: Some(child(self.ran.len())),
            started: true,
        }
    }
    fn renew(&mut self, version: &str, holder: &Process) {
        self.renewed.push((version.to_string(), holder.clone()));
        if self.main.holder_id.as_ref() == Some(holder) {
            self.main.lease_until = Some(self.now + LEASE_S);
            self.main.lease_ended = false;
        }
    }
    fn wait(&mut self, secs: u64) -> bool {
        self.waited.push(secs);
        self.now += secs;
        self.patience = self.patience.saturating_sub(1);
        self.patience > 0
    }
    fn say(&mut self, line: &str) {
        self.said.push(line.to_string());
    }
    fn remember(&mut self, memory: &Memory) {
        self.remembered = Some(memory.clone());
    }
}

/// The lease a claim of the fixture's step runs for after a renewal (`line_claim::lease_s`).
const LEASE_S: u64 = 1_800;

/// The release process the `n`th run of a [`Lab`] was, counting from one.
fn child(n: usize) -> Process {
    Process {
        pid: 9_000 + u32::try_from(n).unwrap(),
        boot: "boot-1".into(),
        start: 50_000 + u64::try_from(n).unwrap(),
    }
}

fn runs(decision: &Decision) -> Option<&Verb> {
    match decision {
        Decision::Run { verb, .. } => Some(verb),
        Decision::Wait { .. } => None,
    }
}

fn waits(decision: &Decision) -> &str {
    match decision {
        Decision::Wait { why, .. } => why,
        Decision::Run { verb, .. } => panic!("runs {verb:?}"),
    }
}

/// THE TABLE: what the world's state alone asks for, with nothing tried yet. Each row is one
/// state of main, this clone's journal and the repair record, and the one thing the pump does.
#[test]
fn each_state_of_main_the_journal_and_the_repair_record_has_one_decision() {
    type Row = (&'static str, fn(&mut Lab), Result<Verb, &'static str>);
    let rows: Vec<Row> = vec![
        (
            "a cut version whose fixtures are on main opens the next",
            |_| {},
            Ok(Verb::OpenNext),
        ),
        (
            "a cut version whose fixtures are not on main waits for the clone that cut it",
            |lab| lab.main.fixtures = false,
            Err("the clone that cut it lands them"),
        ),
        (
            "an unfinished after-cut here does not hold the next version",
            |lab| {
                let journal = lab.line(TIP);
                journal.version = "0.5.0".into();
                journal.published = true;
            },
            Ok(Verb::OpenNext),
        ),
        (
            "a cut version whose after-cut this clone owes resumes its journal",
            |lab| {
                lab.main.fixtures = false;
                let journal = lab.line(TIP);
                journal.version = "0.5.0".into();
                journal.published = true;
            },
            Ok(Verb::Finish),
        ),
        (
            "an open version with no fleet line starts at the commit that opened it",
            Lab::open,
            Ok(Verb::Start(OPENED.into())),
        ),
        (
            "main moving past the open commit does not move the base",
            |lab| {
                lab.open();
                lab.main.tip = MOVED.into();
            },
            Ok(Verb::Start(OPENED.into())),
        ),
        (
            "a version main's history names no opening commit for starts at the tip read",
            |lab| {
                lab.open();
                lab.main.opened = None;
                lab.main.tip = MOVED.into();
            },
            Ok(Verb::Start(MOVED.into())),
        ),
        (
            "past the stale bound the base is the newest judged pass that holds the opening",
            |lab| {
                lab.open();
                lab.main.tip = MOVED.into();
                lab.main.behind = STALE_BASE_COMMITS + 1;
                lab.greens.insert(OPENED.into(), GREEN.into());
            },
            Ok(Verb::Start(GREEN.into())),
        ),
        (
            "past the stale bound with no such pass the base is the tip as read",
            |lab| {
                lab.open();
                lab.main.tip = MOVED.into();
                lab.main.behind = STALE_BASE_COMMITS + 1;
            },
            Ok(Verb::Start(MOVED.into())),
        ),
        (
            "at the stale bound the base is still the opening commit",
            |lab| {
                lab.open();
                lab.main.tip = MOVED.into();
                lab.main.behind = STALE_BASE_COMMITS;
                lab.greens.insert(OPENED.into(), GREEN.into());
            },
            Ok(Verb::Start(OPENED.into())),
        ),
        (
            "another holder's fleet line is left to its holder, who is named",
            |lab| {
                lab.open();
                lab.main.line = Some(OPENED.into());
                lab.main.holder = Some("m9 (node 0eda, session s-9, pid 7)".into());
            },
            Err("held by m9 (node 0eda, session s-9, pid 7)"),
        ),
        (
            "with --adopt a fleet line with no journal here goes to the release's selection",
            |lab| {
                lab.open();
                lab.adopt = true;
                lab.main.line = Some(OPENED.into());
            },
            Ok(Verb::Adopt),
        ),
        (
            "a journal of a version another clone cut resumes nothing and asks for --forget",
            |lab| {
                lab.main.fixtures = false;
                let journal = lab.line(TIP);
                journal.version = "0.5.0".into();
                journal.lost = true;
            },
            Err("a session ends it with ship release --forget"),
        ),
        (
            "a lost journal starts no line of the next version either",
            |lab| {
                lab.open();
                let journal = lab.line(TIP);
                journal.version = "0.5.0".into();
                journal.lost = true;
                lab.main.line = None;
            },
            Err("this clone claimed no cut of it"),
        ),
        (
            // F9: another clone cut the line this journal held and landed its fixtures. The
            // version it would open could never start here while the journal stands.
            // NEGATIVE CONTROL: the after-cut this clone owes does not hold the opening (above).
            "a lost journal opens no next version either",
            |lab| {
                let journal = lab.line(TIP);
                journal.version = "0.5.0".into();
                journal.lost = true;
            },
            Err("a session ends it with ship release --forget"),
        ),
        (
            "a red of the opening whose worker is on it is not tried again",
            |lab| lab.opening = Some(("s-4".into(), Report::Working)),
            Err("repair worker s-4 is on it"),
        ),
        (
            "a red of the opening whose worker ended is tried again",
            |lab| lab.opening = Some(("s-4".into(), Report::NoFix("a credential".into()))),
            Ok(Verb::OpenNext),
        ),
        (
            "this clone's unfinished line resumes",
            |lab| {
                lab.open();
                lab.line(OPENED);
            },
            Ok(Verb::Prepare),
        ),
        (
            "PREPARED without the direction waits for a session",
            |lab| {
                lab.open();
                lab.line(OPENED).prepared = true;
            },
            Err("a session publishes it"),
        ),
        (
            "PREPARED with the direction publishes",
            |lab| {
                lab.open();
                lab.publishes = true;
                lab.line(OPENED).prepared = true;
            },
            Ok(Verb::Finish),
        ),
        (
            "a red of the publish is not run again while its worker is on it",
            |lab| {
                lab.open();
                lab.publishes = true;
                let journal = lab.line(OPENED);
                journal.prepared = true;
                journal.prepared_at = 900;
                lab.report("0.6.0", OPENED, "s-1", Report::Working);
            },
            Err("repair worker s-1 is on it"),
        ),
        (
            "a red of the publish is moved by its worker's fix, direction or none",
            |lab| {
                lab.open();
                let journal = lab.line(OPENED);
                journal.prepared = true;
                journal.prepared_at = 900;
                lab.report("0.6.0", OPENED, "s-1", Report::Fix(FIX.into()));
            },
            Ok(Verb::Fix(FIX.into())),
        ),
        (
            "a red the prepare has since passed does not hold a PREPARED line",
            |lab| {
                lab.open();
                lab.publishes = true;
                lab.report("0.6.0", OPENED, "s-1", Report::NoFix("this machine".into()));
                let journal = lab.line(OPENED);
                journal.prepared = true;
                journal.prepared_at = 1_001;
            },
            Ok(Verb::Finish),
        ),
        (
            "a red line whose worker is on it waits",
            |lab| {
                lab.open();
                lab.line(OPENED);
                lab.report("0.6.0", OPENED, "s-1", Report::Working);
            },
            Err("repair worker s-1 is on it"),
        ),
        (
            "a red line whose worker named an admitted fix carries it",
            |lab| {
                lab.open();
                lab.line(OPENED);
                lab.report("0.6.0", OPENED, "s-1", Report::Fix(FIX.into()));
            },
            Ok(Verb::Fix(FIX.into())),
        ),
        (
            "a fix the line refuses leaves it stopped",
            |lab| {
                lab.open();
                lab.line(OPENED);
                lab.unfit.insert(FIX.into(), "a merge commit".into());
                lab.report("0.6.0", OPENED, "s-1", Report::Fix(FIX.into()));
            },
            Err("which the line does not admit (a merge commit)"),
        ),
        (
            "a worker that could not finish leaves it stopped",
            |lab| {
                lab.open();
                lab.line(OPENED);
                lab.report(
                    "0.6.0",
                    OPENED,
                    "s-1",
                    Report::NoFix("the cause is this machine".into()),
                );
            },
            Err("ended without a fix (the cause is this machine)"),
        ),
        (
            "a report that cannot be read is no verdict",
            |lab| {
                lab.open();
                lab.line(OPENED);
                lab.report(
                    "0.6.0",
                    OPENED,
                    "s-1",
                    Report::Unread("no aterm answered".into()),
                );
            },
            Err("report cannot be read (no aterm answered)"),
        ),
        (
            "a red before any line starts the line at a tip of main that holds the fix",
            |lab| {
                lab.open();
                lab.main.tip = MOVED.into();
                lab.lands(FIX, MOVED);
                lab.report("0.6.0", OPENED, "s-1", Report::Fix(FIX.into()));
            },
            Ok(Verb::Start(MOVED.into())),
        ),
        (
            "a red before any line waits while main's tip does not hold the fix it was told",
            |lab| {
                lab.open();
                lab.main.tip = MOVED.into();
                lab.report("0.6.0", OPENED, "s-1", Report::Fix(FIX.into()));
            },
            Err("does not hold yet"),
        ),
        (
            "a fix the red commit already held is no fix of it",
            |lab| {
                lab.open();
                lab.main.tip = MOVED.into();
                lab.lands(FIX, OPENED);
                lab.lands(FIX, MOVED);
                lab.report("0.6.0", OPENED, "s-1", Report::Fix(FIX.into()));
            },
            Err("the commit that went red already holds it"),
        ),
        (
            "another run of this clone is waited on, whatever else is true",
            |lab| lab.holder = Some("pid 777".into()),
            Err("another release run holds this clone (pid 777)"),
        ),
    ];
    for (name, arrange, expected) in rows {
        let mut lab = Lab::after_a_release();
        arrange(&mut lab);
        let decision = decide(&lab.seen(), &Memory::default());
        match expected {
            Ok(verb) => assert_eq!(runs(&decision), Some(&verb), "{name}: {decision:?}"),
            Err(why) => assert!(waits(&decision).contains(why), "{name}: {decision:?}"),
        }
    }
}

fn attempt(lab: &mut Lab, code: i32, repeats: u32) -> Memory {
    let Decision::Run { verb, key, .. } = decide(&lab.seen(), &Memory::default()) else {
        panic!("the state asks for no run");
    };
    Memory {
        last: Some(Attempt {
            verb,
            key,
            code,
            last: "REFUSED at 1/10 checks: the clone is not clean".into(),
            at: lab.now,
            tip: lab.main.tip.clone(),
            journal: lab.journal.clone(),
            repeats,
        }),
        ..Memory::default()
    }
}

/// THE OUTCOMES OF §3.5 STEP 3, one row each: how the last run for the same thing ended decides
/// whether it runs again, and when.
#[test]
fn the_last_runs_exit_decides_whether_and_when_it_runs_again() {
    // Exit 3, could not run: the same journal resumes, with backoff, and nothing is re-selected.
    let mut lab = Lab::opened();
    lab.line(OPENED);
    let memory = attempt(&mut lab, 3, 0);
    assert!(waits(&decide(&lab.seen(), &memory)).contains("could not run"));
    lab.now += POLL_SECS;
    assert_eq!(runs(&decide(&lab.seen(), &memory)), Some(&Verb::Prepare));
    let slow = attempt(&mut lab, 3, 3);
    lab.now += 4 * POLL_SECS;
    assert!(
        runs(&decide(&lab.seen(), &slow)).is_none(),
        "the wait doubles"
    );
    lab.now += 4 * POLL_SECS;
    assert_eq!(runs(&decide(&lab.seen(), &slow)), Some(&Verb::Prepare));
    let capped = attempt(&mut lab, 3, 30);
    lab.now += RESUME_CAP_SECS;
    assert_eq!(
        runs(&decide(&lab.seen(), &capped)),
        Some(&Verb::Prepare),
        "and is capped"
    );

    // Exit 4, beside another holder of this clone's line: nothing is run again while the line's
    // record still keeps a contender off, however long; once it does not, it is tried again.
    let mut lab = Lab::opened();
    lab.line(OPENED);
    let memory = attempt(&mut lab, 4, 0);
    lab.main.held = Some(held(lab.now + 1800));
    lab.now += 100 * HOLDER_SECS;
    let waiting = decide(&lab.seen(), &memory);
    assert!(waits(&waiting).contains("still holds it"), "{waiting:?}");
    lab.main.held = None;
    assert_eq!(runs(&decide(&lab.seen(), &memory)), Some(&Verb::Prepare));
    // The run and the record disagreed (exit 4 from a line its record calls free): the tries
    // back off, never a tight loop.
    let memory = attempt(&mut lab, 4, 2);
    lab.now += 4 * POLL_SECS - 1;
    assert!(waits(&decide(&lab.seen(), &memory)).contains("no longer keeps a contender off"));
    lab.now += 1;
    assert_eq!(runs(&decide(&lab.seen(), &memory)), Some(&Verb::Prepare));

    // Exit 1 with no repair worker, an operational refusal: nothing starts until the journal or
    // main has changed, however long it waits.
    let mut lab = Lab::opened();
    lab.line(OPENED);
    let memory = attempt(&mut lab, 1, 0);
    lab.now += 10 * REFUSED_CAP_SECS;
    let stopped = decide(&lab.seen(), &memory);
    assert!(waits(&stopped).contains("stopped"), "{stopped:?}");
    assert!(
        waits(&stopped).contains("the clone is not clean"),
        "the final line is kept"
    );
    // main moved: it is tried again.
    lab.main.tip = MOVED.into();
    assert_eq!(runs(&decide(&lab.seen(), &memory)), Some(&Verb::Prepare));
    // The journal changed with main still: it is tried again too.
    let mut lab = Lab::opened();
    lab.line(OPENED);
    let memory = attempt(&mut lab, 1, 0);
    lab.journal.as_mut().unwrap().published = true;
    lab.now += REFUSED_CAP_SECS;
    assert!(runs(&decide(&lab.seen(), &memory)).is_some());
    // A usage error (exit 2) is a refusal with no worker as well.
    let mut lab = Lab::opened();
    let memory = attempt(&mut lab, 2, 0);
    lab.now += 10 * REFUSED_CAP_SECS;
    assert!(waits(&decide(&lab.seen(), &memory)).contains("stopped"));

    // Exit 0 with the world unchanged is looked at again, never run again at once.
    let mut lab = Lab::after_a_release();
    let memory = attempt(&mut lab, 0, 0);
    assert!(waits(&decide(&lab.seen(), &memory)).contains("ended 0"));

    // An attempt for something else gates nothing.
    let mut lab = Lab::after_a_release();
    let memory = attempt(&mut lab, 1, 0);
    lab.open();
    assert_eq!(
        runs(&decide(&lab.seen(), &memory)),
        Some(&Verb::Start(OPENED.into()))
    );
}

/// A LINE THE PUMP RESUMES HAS A LINUX DECLARATION (0.200.0, 2026-11-12: a resume adopted the
/// fleet's line into a journal with none, and `release --prepare` was refused "the cut must
/// declare Linux" at step 1). A resume whose journal holds no declaration is handed the pump's
/// own, as a start and an adoption always are; `--open-next` takes none.
/// NEGATIVE CONTROLS: a journal that holds a declaration keeps its own, so the resume is handed
/// nothing; a pump with no declaration starts no run that prepares such a line, and names what
/// is missing.
#[test]
fn a_resumed_line_with_no_linux_declaration_is_handed_the_pumps() {
    let mac = || Ok(vec!["--mac-only".to_string()]);
    let none = || Err::<Vec<String>, String>("the pump has no declaration".into());
    let resumes = [Verb::Prepare, Verb::Fix(FIX.into()), Verb::Finish];
    for verb in &resumes {
        let handed = declaration(verb, false, mac()).unwrap();
        assert_eq!(handed, ["--mac-only"], "{verb:?}");
        assert_eq!(verb.args(&handed).last().unwrap(), "--mac-only", "{verb:?}");
        assert!(
            declaration(verb, true, mac()).unwrap().is_empty(),
            "{verb:?}"
        );
        assert!(
            declaration(verb, true, none()).unwrap().is_empty(),
            "{verb:?}"
        );
    }
    for verb in [Verb::Start(OPENED.into()), Verb::Adopt] {
        for declares in [false, true] {
            assert_eq!(declaration(&verb, declares, mac()).unwrap(), ["--mac-only"]);
            assert!(declaration(&verb, declares, none()).is_err(), "{verb:?}");
        }
    }
    assert!(declaration(&Verb::Prepare, false, none()).is_err());
    assert!(declaration(&Verb::Fix(FIX.into()), false, none()).is_err());
    // The bare run of a journal with none is the release's to refuse or finish.
    assert!(
        declaration(&Verb::Finish, false, none())
            .unwrap()
            .is_empty()
    );
    assert!(
        declaration(&Verb::OpenNext, false, mac())
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        Verb::OpenNext.args(&mac().unwrap()),
        ["release", "--open-next"]
    );
}

/// A refused resume that moved this clone's journal (it adopted the fleet's line, so the head
/// its key names changed) is the last run of the journal it left: the same verb on that journal
/// is not run again as if untried, and waits for the journal or main to change.
/// NEGATIVE CONTROL: with the journal changed since that run, the verb runs.
#[test]
fn a_refusal_that_moved_the_journal_is_not_run_again_at_once() {
    let mut lab = Lab::opened();
    lab.line(OPENED);
    let mut memory = attempt(&mut lab, 1, 0);
    // The run adopted the line at MOVED and was refused there.
    lab.line(MOVED);
    memory.last.as_mut().unwrap().journal = lab.journal.clone();
    let again = decide(&lab.seen(), &memory);
    assert!(waits(&again).contains("stopped"), "{again:?}");
    lab.now += 10 * REFUSED_CAP_SECS;
    assert!(runs(&decide(&lab.seen(), &memory)).is_none());
    lab.main.tip = TIP.into();
    assert_eq!(runs(&decide(&lab.seen(), &memory)), Some(&Verb::Prepare));
    let mut changed = Lab::opened();
    changed.line(OPENED);
    let memory = attempt(&mut changed, 1, 0);
    changed.line(MOVED);
    assert_eq!(
        runs(&decide(&changed.seen(), &memory)),
        Some(&Verb::Prepare)
    );
    // Only a refusal is carried across the move: a run that ended 0 or could not run is of its
    // own key, and the journal it left is run at once.
    for code in [0, 3] {
        let mut lab = Lab::opened();
        lab.line(OPENED);
        let mut memory = attempt(&mut lab, code, 0);
        lab.line(MOVED);
        memory.last.as_mut().unwrap().journal = lab.journal.clone();
        assert_eq!(
            runs(&decide(&lab.seen(), &memory)),
            Some(&Verb::Prepare),
            "exit {code}"
        );
    }
}

/// A holder whose lease runs until `until`, as the line's record gives it to a contender.
fn held(until: u64) -> Held {
    Held {
        until,
        refusal: "CUTTING ELSEWHERE: v0.6.0 by m73 at qualify".into(),
    }
}

/// THE PUMP RUNS NOTHING IT CANNOT HOLD (0.200.0, 2026-11-12: m73 held the line, and the pump
/// ran step 1 again and again to be told so). With `--adopt`, a fleet line whose record keeps a
/// contender off is waited on from that record alone: no verb runs, and the wait is said once,
/// with the lease's end, however often the holder renews. Once the record frees the line, the
/// release's selection adopts it.
/// NEGATIVE CONTROL: the same line with a record that keeps nobody off is adopted at the first
/// look.
#[test]
fn a_held_line_is_waited_on_from_its_record_and_said_once() {
    let beside = |held_until: Option<u64>| {
        let mut lab = Lab::opened();
        lab.adopt = true;
        lab.main.line = Some(OPENED.into());
        lab.main.holder = Some("m73 (node n, session s, pid 1, boot b, start 1)".into());
        lab.main.held = held_until.map(held);
        lab
    };
    let mut lab = beside(Some(2_800));
    let (mut memory, mut said) = (Memory::default(), String::new());
    for _ in 0..90 {
        let secs = cycle(&mut lab, &mut memory, &mut said);
        assert_eq!(secs, POLL_SECS);
        lab.now += secs;
        // The holder completes a row: its lease is renewed.
        lab.main.held.as_mut().unwrap().until += 300;
    }
    assert!(lab.ran.is_empty(), "{:?}", lab.ran);
    assert_eq!(lab.said.len(), 1, "{:?}", lab.said);
    assert!(
        lab.said[0].contains("held by m73")
            && lab.said[0].contains("its lease runs until 2800")
            && lab.said[0].contains("CUTTING ELSEWHERE: v0.6.0 by m73"),
        "{:?}",
        lab.said
    );
    lab.main.held = None;
    assert_eq!(cycle(&mut lab, &mut memory, &mut said), 0);
    assert_eq!(lab.ran, [Verb::Adopt]);

    let mut free = beside(None);
    cycle(&mut free, &mut Memory::default(), &mut String::new());
    assert_eq!(free.ran, [Verb::Adopt]);
}

/// A run that still ends beside the holder (exit 4: the line was taken between the look and
/// the claim, or this clone's own line was taken over) is run once: while the record keeps a
/// contender off the pump waits, saying so once, and it resumes when the record frees the line.
/// NEGATIVE CONTROL: without the record's word the exit-4 line would be tried again each
/// backoff; here ninety looks beside a live holder run nothing.
#[test]
fn a_run_that_ended_beside_a_holder_is_not_run_again_while_the_line_is_held() {
    let mut lab = Lab::opened();
    lab.line(OPENED);
    lab.exits = vec![4];
    let (mut memory, mut said) = (Memory::default(), String::new());
    assert_eq!(cycle(&mut lab, &mut memory, &mut said), 0);
    assert_eq!(lab.ran, [Verb::Prepare]);
    lab.main.held = Some(held(lab.now + 1_800));
    let before = lab.said.len();
    for _ in 0..90 {
        lab.now += cycle(&mut lab, &mut memory, &mut said);
        lab.main.held.as_mut().unwrap().until += 300;
    }
    assert_eq!(lab.ran, [Verb::Prepare], "nothing ran beside the holder");
    assert_eq!(lab.said.len(), before + 1, "{:?}", lab.said);
    assert!(
        lab.said[before].contains("still holds it"),
        "{:?}",
        lab.said
    );
    lab.main.held = None;
    cycle(&mut lab, &mut memory, &mut said);
    assert_eq!(lab.ran, [Verb::Prepare, Verb::Prepare]);
}

/// A RED IS NEVER RUN AGAIN. A line that refused with a repair worker on it is not resumed when
/// main moves or time passes: only the worker's fix moves it.
/// NEGATIVE CONTROL: treating the red as an operational refusal re-runs it on each landing.
#[test]
fn a_red_line_is_moved_only_by_its_workers_fix() {
    let mut lab = Lab::opened();
    lab.line(OPENED);
    let memory = attempt(&mut lab, 1, 0);
    lab.report("0.6.0", OPENED, "s-9", Report::Working);
    lab.main.tip = MOVED.into();
    lab.now += 10 * REFUSED_CAP_SECS;
    assert!(runs(&decide(&lab.seen(), &memory)).is_none());
    lab.report("0.6.0", OPENED, "s-9", Report::Fix(FIX.into()));
    assert_eq!(
        runs(&decide(&lab.seen(), &memory)),
        Some(&Verb::Fix(FIX.into()))
    );
}

/// A RED OF THE PUBLISH IS NOT PUBLISHED AGAIN. The journal stays PREPARED through a red of the
/// publish, so PREPARED alone would run the bare release again on every landing. The worker of
/// that red governs: one the red opened, and equally the older worker of the same inputs that
/// the release named instead of opening a second.
/// NEGATIVE CONTROL: with PREPARED decided before the repair record, main's next movement
/// publishes again after a FAIL.
#[test]
fn a_refused_publish_is_its_workers_to_move_not_the_pumps_to_repeat() {
    // The pump's publish goes red and opens a worker.
    let mut lab = Lab::opened();
    lab.publishes = true;
    let journal = lab.line(OPENED);
    journal.prepared = true;
    journal.prepared_at = 900;
    lab.exits = vec![1];
    lab.opens = Some(("s-3".into(), Report::Working));
    let mut memory = Memory::default();
    let mut said = String::new();
    assert_eq!(cycle(&mut lab, &mut memory, &mut said), 0);
    assert_eq!(lab.ran, [Verb::Finish]);
    for tip in [MOVED, HOP] {
        lab.main.tip = tip.into();
        lab.now += 10 * REFUSED_CAP_SECS;
        assert_eq!(cycle(&mut lab, &mut memory, &mut said), POLL_SECS);
    }
    assert_eq!(lab.ran.len(), 1, "{:#?}", lab.said);
    lab.report("0.6.0", OPENED, "s-3", Report::Fix(FIX.into()));
    assert_eq!(cycle(&mut lab, &mut memory, &mut said), 0);
    assert_eq!(lab.ran[1], Verb::Fix(FIX.into()), "the fix is carried");

    // The same red on inputs that already had a worker, from before the prepare passed: the
    // release opens no second one, and the pump still does not publish again.
    let mut lab = Lab::opened();
    lab.publishes = true;
    lab.report("0.6.0", OPENED, "s-1", Report::NoFix("this machine".into()));
    let journal = lab.line(OPENED);
    journal.prepared = true;
    journal.prepared_at = 2_000;
    lab.exits = vec![1];
    let mut memory = Memory::default();
    assert_eq!(cycle(&mut lab, &mut memory, &mut said), 0);
    assert_eq!(
        lab.ran,
        [Verb::Finish],
        "the old worker did not hold PREPARED"
    );
    lab.main.tip = MOVED.into();
    lab.now += 10 * REFUSED_CAP_SECS;
    assert_eq!(cycle(&mut lab, &mut memory, &mut said), POLL_SECS);
    assert_eq!(lab.ran.len(), 1, "{:#?}", lab.said);
    assert!(
        lab.said.last().unwrap().contains("is stopped"),
        "{:#?}",
        lab.said
    );
}

/// A stopped line that has had its one re-base names the supersede candidate as a session's
/// decision, or says there is none; the pump runs neither. One whose candidate cannot take a
/// re-base says why.
#[test]
fn a_stopped_line_names_the_newest_judged_pass_past_its_head_as_a_candidate() {
    let mut lab = Lab::opened();
    lab.line(OPENED);
    lab.main.superseded = true;
    lab.report(
        "0.6.0",
        OPENED,
        "s-9",
        Report::NoFix("could not finish".into()),
    );
    let none = decide(&lab.seen(), &Memory::default());
    assert!(
        waits(&none).contains("there is no re-base or supersede candidate"),
        "{none:?}"
    );
    lab.green = Some(GREEN.into());
    let named = decide(&lab.seen(), &Memory::default());
    assert!(
        waits(&named).contains("has had its one re-base")
            && waits(&named).contains(&format!("--commit {GREEN} --supersede")),
        "{named:?}"
    );
    lab.main.superseded = false;
    lab.unfit_onto.insert(
        GREEN.into(),
        "it declares 0.7.0, not the line's 0.6.0".into(),
    );
    let unfit = decide(&lab.seen(), &Memory::default());
    assert!(
        waits(&unfit).contains("cannot take a re-base (it declares 0.7.0"),
        "{unfit:?}"
    );
}

/// The verbs' arguments: only a verb that can start a line carries the Linux declaration, and no
/// verb spells a flag the pump must never run.
#[test]
fn the_verbs_spell_only_the_existing_release_flags() {
    let linux = vec!["--mac-only".to_string()];
    let spelled = |verb: Verb| verb.args(&linux).join(" ");
    assert_eq!(spelled(Verb::OpenNext), "release --open-next");
    assert_eq!(
        spelled(Verb::Start(OPENED.into())),
        format!("release --prepare --commit {OPENED} --mac-only")
    );
    assert_eq!(spelled(Verb::Adopt), "release --prepare --mac-only");
    assert_eq!(spelled(Verb::Prepare), "release --prepare --mac-only");
    assert_eq!(spelled(Verb::Finish), "release --mac-only");
    assert_eq!(
        spelled(Verb::Fix(FIX.into())),
        format!("release --prepare --fix {FIX} --mac-only")
    );
    // The one re-base spells the supersession of the line onto the judged pass, and only it.
    let rebase = Verb::Rebase {
        onto: GREEN.into(),
        requalify: Requalify::Red("caller-owned repair, input hash ab12".into()),
    };
    assert_eq!(
        spelled(rebase.clone()),
        format!("release --prepare --commit {GREEN} --supersede --mac-only")
    );
    // Handed no declaration (the journal holds its own), a resume spells none.
    assert_eq!(Verb::Prepare.args(&[]).join(" "), "release --prepare");
    assert_eq!(Verb::Finish.args(&[]).join(" "), "release");
    for verb in [
        Verb::OpenNext,
        Verb::Start(OPENED.into()),
        Verb::Adopt,
        Verb::Prepare,
        Verb::Finish,
        Verb::Fix(FIX.into()),
        rebase.clone(),
    ] {
        let args = verb.args(&linux);
        for never in ["--forget", "--bump", "--abandon"] {
            assert!(
                !args.iter().any(|arg| arg == never),
                "{verb:?} spells {never}"
            );
        }
        assert_eq!(
            args.iter().any(|arg| arg == "--supersede"),
            verb == rebase,
            "{verb:?}: only the re-base supersedes"
        );
    }
}

/// THE PUMP STARTS THE NEXT LINE AND STOPS AT PREPARED WITHOUT THE DIRECTION. From a finished
/// release it opens the next version, starts its line at the commit that opened it and, with no
/// `PUMP_PUBLISH_DEFAULT`, runs nothing more: the cadence past PREPARED is a session's.
#[test]
fn the_pump_starts_the_next_line_and_stops_at_prepared_without_the_direction() {
    let mut lab = Lab::after_a_release();
    lab.patience = 4;
    pump(&mut lab, Memory::default(), None);
    assert_eq!(lab.ran, [Verb::OpenNext, Verb::Start(OPENED.into())]);
    let journal = lab.journal.clone().unwrap();
    assert!(journal.prepared && !journal.published);
    assert!(
        lab.said
            .iter()
            .any(|line| line.contains("a session publishes it")),
        "{:#?}",
        lab.said
    );
    assert_eq!(
        lab.said
            .iter()
            .filter(|line| line.contains("a session publishes it"))
            .count(),
        1,
        "an unchanged wait is said once: {:#?}",
        lab.said
    );
    let kept = lab.remembered.clone().unwrap().last.unwrap();
    assert_eq!((kept.verb, kept.code), (Verb::Start(OPENED.into()), 0));
}

/// WITH THE DIRECTION IT KEEPS RELEASING: open, start, publish, then the next version's open,
/// one line at a time.
#[test]
fn with_the_direction_the_pump_publishes_and_opens_the_version_after() {
    let mut lab = Lab::after_a_release();
    lab.publishes = true;
    pump(&mut lab, Memory::default(), Some(4));
    assert_eq!(
        lab.ran,
        [
            Verb::OpenNext,
            Verb::Start(OPENED.into()),
            Verb::Finish,
            Verb::OpenNext
        ]
    );
}

/// A RED WITH A REPAIR WORKER, END TO END: the prepare goes red and opens its worker; the pump
/// waits while it works, carries the fix it names as a hop, and the line is PREPARED at the
/// new head. It never starts another line beside the red one.
#[test]
fn a_red_waits_for_its_worker_then_carries_the_fix_as_a_hop() {
    let mut lab = Lab::opened();
    lab.exits = vec![1];
    lab.opens = Some(("s-7".into(), Report::Working));
    // The start refuses red before its journal: the red is of the commit that opened it.
    let mut memory = Memory::default();
    let mut said = String::new();
    assert_eq!(cycle(&mut lab, &mut memory, &mut said), 0);
    assert_eq!(lab.ran, [Verb::Start(OPENED.into())]);
    for _ in 0..3 {
        assert_eq!(cycle(&mut lab, &mut memory, &mut said), POLL_SECS);
    }
    assert_eq!(lab.ran.len(), 1, "nothing runs while the worker works");
    // The worker names a fix main does not hold yet (its landing is still running, or lost):
    // main's movement alone starts nothing.
    lab.main.tip = MOVED.into();
    lab.report("0.6.0", OPENED, "s-7", Report::Fix(FIX.into()));
    assert_eq!(cycle(&mut lab, &mut memory, &mut said), POLL_SECS);
    assert_eq!(lab.ran.len(), 1, "{:#?}", lab.said);
    // The fix lands.
    lab.lands(FIX, MOVED);
    assert_eq!(cycle(&mut lab, &mut memory, &mut said), 0);
    assert_eq!(
        lab.ran[1],
        Verb::Start(MOVED.into()),
        "the line starts past the fix"
    );

    // A start that goes red again waits on its own worker: main's later movement starts
    // nothing, so one red opens one worker and never a line per landing.
    // NEGATIVE CONTROL: keyed by the opening commit's old worker, each new tip is a new start.
    let mut lab = Lab::opened();
    lab.report("0.6.0", OPENED, "s-7", Report::Fix(FIX.into()));
    lab.main.tip = MOVED.into();
    lab.lands(FIX, MOVED);
    lab.exits = vec![1];
    lab.opens = Some(("s-8".into(), Report::Working));
    let mut memory = Memory::default();
    assert_eq!(cycle(&mut lab, &mut memory, &mut said), 0);
    assert_eq!(lab.ran, [Verb::Start(MOVED.into())]);
    lab.main.tip = HOP.into();
    lab.now += 10 * REFUSED_CAP_SECS;
    assert_eq!(cycle(&mut lab, &mut memory, &mut said), POLL_SECS);
    assert_eq!(lab.ran.len(), 1, "{:#?}", lab.said);
    assert!(
        lab.said
            .last()
            .unwrap()
            .contains("repair worker s-8 is on it")
    );

    // A red of a line that exists is carried as a hop.
    let mut lab = Lab::opened();
    lab.line(OPENED);
    lab.exits = vec![1];
    lab.opens = Some(("s-8".into(), Report::Fix(FIX.into())));
    let mut memory = Memory::default();
    assert_eq!(cycle(&mut lab, &mut memory, &mut said), 0);
    assert_eq!(cycle(&mut lab, &mut memory, &mut said), 0);
    assert_eq!(lab.ran, [Verb::Prepare, Verb::Fix(FIX.into())]);
    assert_eq!(lab.journal.as_ref().unwrap().head, HOP);
    assert!(lab.journal.as_ref().unwrap().prepared);
}

/// AN UNREADABLE WORLD IS NO DECISION: a failed read of origin runs nothing and is looked at
/// again; a restart with the kept memory repeats nothing.
#[test]
fn an_unread_origin_runs_nothing_and_kept_memory_survives_a_restart() {
    let mut lab = Lab::after_a_release();
    lab.main_down = true;
    let mut memory = Memory::default();
    let mut said = String::new();
    assert_eq!(cycle(&mut lab, &mut memory, &mut said), POLL_SECS);
    assert!(lab.ran.is_empty());
    assert!(
        lab.said[0].contains("origin did not answer"),
        "{:?}",
        lab.said
    );

    // An operational refusal, then a restart: the kept memory still stops the line.
    let mut lab = Lab::opened();
    lab.line(OPENED);
    lab.exits = vec![1];
    pump(&mut lab, Memory::default(), Some(1));
    let kept = lab.remembered.clone().unwrap();
    let text = aterm_toml::to_string(&kept).unwrap();
    let restored: Memory = aterm_toml::from_str(&text).unwrap();
    assert_eq!(restored, kept);
    lab.now += 10 * REFUSED_CAP_SECS;
    pump(&mut lab, restored, Some(3));
    assert_eq!(lab.ran, [Verb::Prepare], "a restarted pump repeats nothing");
}

/// The config keys are read as the engine reads the file: one flat `KEY="value"` line.
#[test]
fn the_direction_is_one_flat_config_line() {
    let config = "# PUMP_PUBLISH_DEFAULT=\"1\" in a comment\nSTAGING=\"x\"\n\
                  PUMP_LINUX_DEFAULT=\"--mac-only\"\nPUMP_PUBLISH_DEFAULT=\"1\"\n";
    assert_eq!(config_value(config, PUBLISH_KEY), Some("1"));
    assert_eq!(config_value(config, LINUX_KEY), Some("--mac-only"));
    for not_it in [
        "PUMP_PUBLISH_DEFAULT=1\n",
        " PUMP_PUBLISH_DEFAULT=\"1\"\n",
        "PUMP_PUBLISH_DEFAULT=\"1\" # yes\n",
        "export PUMP_PUBLISH_DEFAULT=\"1\"\n",
        "PUMP_PUBLISH_DEFAULT_OLD=\"1\"\n",
        "",
    ] {
        assert_eq!(config_value(not_it, PUBLISH_KEY), None, "{not_it:?}");
    }
}

/// A run's final line is its last line that is not detail or an event record.
#[test]
fn a_runs_final_line_skips_detail_and_event_records() {
    let out = "[ 1/10] checks ok\nrelease-event: {\"event\":\"x\"}\n        | detail\n\
               REFUSED at 4/10 qualify: red — nothing public. fix it\n\
               release-event: {\"event\":\"disk\"}\n\n";
    assert_eq!(
        final_line(out.lines()),
        "REFUSED at 4/10 qualify: red — nothing public. fix it"
    );
    assert_eq!(final_line("".lines()), "");
}

/// EACH VERB IS HANDED ONLY THE FLAGS IT TAKES. `--open-next` writes main alone and refuses a
/// release credential as a usage error, which the pump would retry for good.
/// NEGATIVE CONTROL: handing every flag to every verb puts --release-credentials on --open-next.
#[test]
fn a_verb_is_handed_only_the_flags_it_takes() {
    let handed = Handed {
        engine: Some("/e".into()),
        credentials: Some("/c.toml".into()),
        verbose: true,
    };
    assert_eq!(
        Verb::OpenNext.argv(&[], &handed).join(" "),
        "release --open-next --engine /e --verbose"
    );
    for verb in [
        Verb::Start(OPENED.into()),
        Verb::Adopt,
        Verb::Prepare,
        Verb::Finish,
        Verb::Fix(FIX.into()),
    ] {
        let argv = verb.argv(&["--mac-only".to_string()], &handed);
        assert!(
            argv.ends_with(&[
                "--engine".to_string(),
                "/e".to_string(),
                "--release-credentials".to_string(),
                "/c.toml".to_string(),
                "--verbose".to_string()
            ]),
            "{verb:?}: {argv:?}"
        );
    }
    assert_eq!(
        Verb::Prepare.argv(&[], &Handed::default()),
        Verb::Prepare.args(&[])
    );
}

/// ONE RED OF THE OPENING, ONE WORKER, WHATEVER MAIN DOES. The bump's gates are red on main;
/// each try judges a newer tip. While the worker works nothing is tried; once it ended, the
/// opening is tried again as main moves, with backoff, and every later red names the worker
/// already opened.
/// NEGATIVE CONTROL: without the hold, the first main movement runs --open-next again while
/// the worker is still on it.
#[test]
fn a_red_opening_is_not_tried_again_while_its_worker_is_on_it() {
    let mut lab = Lab::after_a_release();
    lab.exits = vec![1, 1];
    lab.opens = Some(("s-4".into(), Report::Working));
    let mut memory = Memory::default();
    let mut said = String::new();
    assert_eq!(cycle(&mut lab, &mut memory, &mut said), 0);
    assert_eq!(lab.ran, [Verb::OpenNext]);
    for tip in [MOVED, HOP, GREEN] {
        lab.main.tip = tip.into();
        lab.now += REFUSED_CAP_SECS;
        assert_eq!(cycle(&mut lab, &mut memory, &mut said), POLL_SECS);
    }
    assert_eq!(lab.ran.len(), 1, "{:#?}", lab.said);
    assert!(
        lab.said
            .last()
            .unwrap()
            .contains("repair worker s-4 is on it")
    );

    // The worker ended: the opening is tried again, red again, and no second worker opens.
    lab.opening = Some(("s-4".into(), Report::Fix(FIX.into())));
    lab.opens = Some(("s-5".into(), Report::Working));
    assert_eq!(cycle(&mut lab, &mut memory, &mut said), 0);
    assert_eq!(lab.ran, [Verb::OpenNext, Verb::OpenNext]);
    assert_eq!(lab.opening_reds, 2);
    assert_eq!(lab.opening.as_ref().unwrap().0, "s-4", "one worker");
    // That red is now an ordinary refusal: nothing until main moves, then backoff.
    lab.now += 10 * REFUSED_CAP_SECS;
    assert_eq!(cycle(&mut lab, &mut memory, &mut said), POLL_SECS);
    lab.main.tip = TIP.into();
    assert_eq!(cycle(&mut lab, &mut memory, &mut said), 0);
    assert_eq!(lab.ran.len(), 3);
    assert_eq!(*lab.ran.last().unwrap(), Verb::OpenNext);
}

/// A REFUSED START BACKS OFF ACROSS MAIN'S TIPS. The start after a worker's fix names main's
/// tip, which is another commit minutes later: its key is the version and the worker, so one
/// refusal with no worker (an unfit host, a cutter that does not build) is one attempt whose
/// backoff holds as main moves.
/// NEGATIVE CONTROL: keyed by the tip, every landing on main is a new key and runs at once.
#[test]
fn a_refused_start_is_one_attempt_however_main_moves() {
    let mut lab = Lab::opened();
    lab.report("0.6.0", OPENED, "s-7", Report::Fix(FIX.into()));
    for tip in [MOVED, HOP, GREEN] {
        lab.lands(FIX, tip);
    }
    lab.main.tip = MOVED.into();
    lab.exits = vec![1, 3];
    let mut memory = Memory::default();
    let mut said = String::new();
    assert_eq!(cycle(&mut lab, &mut memory, &mut said), 0);
    assert_eq!(lab.ran, [Verb::Start(MOVED.into())]);
    // main moves at once: the same attempt, still inside its first backoff.
    lab.main.tip = HOP.into();
    lab.now += POLL_SECS;
    assert!(cycle(&mut lab, &mut memory, &mut said) > 0);
    assert_eq!(lab.ran.len(), 1, "{:#?}", lab.said);
    // Past the backoff it starts at the tip as it is then.
    lab.now += 5 * POLL_SECS;
    assert_eq!(cycle(&mut lab, &mut memory, &mut said), 0);
    assert_eq!(lab.ran[1], Verb::Start(HOP.into()));
    // That try could not run (exit 3): the next is the same key again, after its own backoff.
    lab.main.tip = GREEN.into();
    assert!(cycle(&mut lab, &mut memory, &mut said) > 0);
    assert_eq!(lab.ran.len(), 2);
    assert_eq!(memory.last.as_ref().unwrap().key, "start 0.6.0 after s-7");

    // A version with no red starts under one key too, whatever commit its base is.
    let mut lab = Lab::opened();
    lab.main.opened = None;
    lab.exits = vec![1];
    let mut memory = Memory::default();
    assert_eq!(cycle(&mut lab, &mut memory, &mut said), 0);
    lab.main.tip = MOVED.into();
    lab.now += POLL_SECS;
    assert!(cycle(&mut lab, &mut memory, &mut said) > 0);
    assert_eq!(lab.ran, [Verb::Start(OPENED.into())], "{:#?}", lab.said);
}

/// A WORKER THAT CANNOT BE READ DOES NOT HOLD A RED LINE IN SILENCE. An unread report is no
/// verdict at first; unread at every look for half an hour, the line is red with no fix coming:
/// one that has had its re-base is said to be stopped, with what a session does and the
/// supersede candidate, and one that has not re-bases onto the judged pass (UB-15). A report
/// that reads again resets it.
#[test]
fn a_report_unread_for_half_an_hour_stops_the_line_for_a_session() {
    let mut lab = Lab::opened();
    lab.line(OPENED);
    lab.main.superseded = true;
    lab.green = Some(GREEN.into());
    lab.report(
        "0.6.0",
        OPENED,
        "s-1",
        Report::Unread("no aterm answered".into()),
    );
    let mut memory = Memory::default();
    let mut said = String::new();
    assert_eq!(cycle(&mut lab, &mut memory, &mut said), POLL_SECS);
    assert!(lab.said.last().unwrap().contains("cannot be read"));
    assert_eq!(memory.unread.as_ref().unwrap().since, lab.now);
    assert_eq!(lab.remembered.as_ref(), Some(&memory), "kept for a restart");
    lab.now += UNREAD_STOP_SECS - 1;
    cycle(&mut lab, &mut memory, &mut said);
    assert!(lab.said.last().unwrap().contains("cannot be read"));
    lab.now += 1;
    cycle(&mut lab, &mut memory, &mut said);
    let stopped = lab.said.last().unwrap();
    assert!(stopped.contains("is stopped"), "{stopped}");
    assert!(stopped.contains("aterm ctl @s-1 result"), "{stopped}");
    assert!(
        stopped.contains(&format!("--commit {GREEN} --supersede")),
        "{stopped}"
    );
    assert!(lab.ran.is_empty());
    // It reads again: the count is gone, and a later unread starts from then.
    lab.report("0.6.0", OPENED, "s-1", Report::Working);
    cycle(&mut lab, &mut memory, &mut said);
    assert_eq!(memory.unread, None);
    lab.report("0.6.0", OPENED, "s-1", Report::Unread("x".into()));
    lab.now += 10;
    cycle(&mut lab, &mut memory, &mut said);
    assert!(lab.said.last().unwrap().contains("cannot be read"));
    // A line that has not re-based re-bases once the half hour is out, and not before.
    lab.main.superseded = false;
    lab.greens.insert(OPENED.into(), GREEN.into());
    lab.now += UNREAD_STOP_SECS - 1;
    cycle(&mut lab, &mut memory, &mut said);
    assert!(lab.ran.is_empty(), "{:?}", lab.ran);
    lab.now += 1;
    cycle(&mut lab, &mut memory, &mut said);
    assert!(
        matches!(&lab.ran[..], [Verb::Rebase { onto, requalify: Requalify::Red(why) }]
            if onto == GREEN && why.contains("s-1")),
        "{:?}",
        lab.ran
    );
}

/// A scripted run: it ends when its script says, or `after_interrupt` seconds after it is
/// interrupted; its newest completed row is `row` (epoch seconds) until `advances` moves it,
/// and its process tree has been idle since `idle_since` unless it is `busy`.
struct Child {
    now: u64,
    /// The exit and the second it ends by itself.
    ends: Option<(u64, i32)>,
    /// Seconds after an interrupt it ends INTERRUPTED; `None` ignores the interrupt.
    after_interrupt: Option<u64>,
    row: Option<Progress>,
    /// Seconds between completed rows; 0 completes none.
    advances: u64,
    /// Seconds between the holder's heartbeat renewals of its record's `at` with no row completed;
    /// 0 renews nothing.
    renews: u64,
    /// Its processes work (compilers, tests): the tree is never idle.
    busy: bool,
    /// The second its tree last made progress; `None` where the tree is not read.
    idle_since: Option<u64>,
    stop_at: Option<u64>,
    interrupts: Vec<u64>,
    said: Vec<String>,
    /// The seconds the pump refreshed its fact at.
    beats: Vec<u64>,
}

const T0: u64 = 10_000;

impl Child {
    /// A run that completes no row and whose processes do nothing: the wait that never ends.
    fn silent() -> Self {
        Self {
            now: T0,
            ends: None,
            after_interrupt: Some(5),
            row: Some(Progress {
                at: T0,
                rows: 0,
                p95_s: 0,
                boundary: "qualify — test aterm-gui".into(),
            }),
            advances: 0,
            renews: 0,
            busy: false,
            idle_since: Some(T0),
            stop_at: None,
            interrupts: Vec::new(),
            said: Vec::new(),
            beats: Vec::new(),
        }
    }
}

impl Running for Child {
    fn now(&mut self) -> u64 {
        self.now
    }
    fn ended(&mut self) -> Option<i32> {
        if let Some((at, code)) = self.ends
            && self.now >= at
        {
            return Some(code);
        }
        let stopped = self.interrupts.first().zip(self.after_interrupt);
        stopped
            .filter(|(at, after)| self.now >= *at + after)
            .map(|_| 130)
    }
    fn progress(&mut self) -> Option<Progress> {
        if self.advances > 0
            && let Some(row) = &mut self.row
        {
            row.at = self.now - self.now % self.advances;
            row.rows = self.now / self.advances;
        }
        if self.renews > 0
            && let Some(row) = &mut self.row
        {
            row.at = row.at.max(self.now - self.now % self.renews);
        }
        self.row.clone()
    }
    fn idle(&mut self) -> Option<u64> {
        if self.busy {
            return Some(0);
        }
        self.idle_since.map(|since| self.now.saturating_sub(since))
    }
    fn stop_asked(&mut self) -> bool {
        self.stop_at.is_some_and(|at| self.now >= at)
    }
    fn interrupt(&mut self) {
        self.interrupts.push(self.now);
    }
    fn beat(&mut self) {
        self.beats.push(self.now);
    }
    fn pause(&mut self) {
        self.now += 1;
        assert!(self.now < T0 + 400 * 60 * 60, "the run never ended");
    }
    fn say(&mut self, line: &str) {
        self.said.push(line.to_string());
    }
}

/// THE PUMP IS NOT STUCK BEHIND A SILENT CHILD. A run that completes no row while its processes
/// do nothing (the 0.199.0 memory-admission wait that never admitted) is said to be stalled
/// and interrupted, once, as soon as both have lasted the fleet claim's progress bound; it
/// ends INTERRUPTED, which the pump resumes with backoff.
/// NEGATIVE CONTROL: waiting on the child alone never returns for this run.
#[test]
fn a_run_that_completes_no_row_and_does_nothing_is_stalled_and_interrupted() {
    let mut child = Child::silent();
    assert_eq!(attend(&mut child, true), 130);
    assert_eq!(
        child.interrupts,
        [T0 + STALL_FLOOR_SECS + 1],
        "interrupted once, past the bound"
    );
    assert_eq!(child.said.len(), 1, "{:#?}", child.said);
    let said = &child.said[0];
    assert!(said.contains("STALLED"), "{said}");
    assert!(said.contains("qualify — test aterm-gui"), "{said}");
    assert!(said.contains("interrupting it"), "{said}");

    // A recorded p95 widens the bound: twice the step's p95 where that is over the floor.
    assert_eq!(stall_bound(0), STALL_FLOOR_SECS);
    assert_eq!(stall_bound(600), STALL_FLOOR_SECS);
    assert_eq!(stall_bound(2_000), 4_000);
    let mut slow = Child::silent();
    slow.row.as_mut().unwrap().p95_s = 2_000;
    assert_eq!(attend(&mut slow, true), 130);
    assert_eq!(slow.interrupts, [T0 + 4_000 + 1]);

    // The tree went idle later than the row: the stall is counted from the later of the two.
    let mut late = Child::silent();
    late.idle_since = Some(T0 + 1_000);
    assert_eq!(attend(&mut late, true), 130);
    assert_eq!(late.interrupts, [T0 + 1_000 + STALL_FLOOR_SECS]);

    // A run that ignores the interrupt is asked once and waited on, never asked in a loop.
    let mut deaf = Child::silent();
    deaf.after_interrupt = None;
    deaf.ends = Some((T0 + 20 * STALL_FLOOR_SECS, 1));
    assert_eq!(attend(&mut deaf, true), 1);
    assert_eq!(deaf.interrupts.len(), 1);
    assert_eq!(deaf.said.len(), 1);

    // A publish is said to be stalled and left alone: its waits are a person's or a notary's.
    let mut publish = Child::silent();
    publish.ends = Some((T0 + 20 * STALL_FLOOR_SECS, 0));
    assert_eq!(attend(&mut publish, false), 0);
    assert!(publish.interrupts.is_empty());
    assert_eq!(publish.said.len(), 1);
    assert!(
        publish.said[0].contains("a publish is not interrupted"),
        "{:#?}",
        publish.said
    );
}

/// A LONG ROW IS NOT A STALL. A run whose processes work is not interrupted however long it
/// completes no row within the fleet claim's ceiling (a slow Mac's test stage: 108 minutes of
/// 0.199.0), nor is one that keeps completing rows while idle between them, one that records no
/// row at all while its processes work (it holds no fleet line), or one whose tree is not read,
/// up to that ceiling. A stop asked of the pump is passed on once. Each look of a run that is
/// not stalled refreshes the pump's fact.
/// NEGATIVE CONTROL: judged by rows alone, the working run is interrupted at its bound, and
/// again on every resume.
#[test]
fn a_run_whose_processes_work_or_that_holds_no_line_is_only_waited_on() {
    let mut working = Child::silent();
    working.busy = true;
    working.ends = Some((T0 + 108 * 60, 0));
    assert_eq!(attend(&mut working, true), 0);
    assert!(working.interrupts.is_empty() && working.said.is_empty());
    assert_eq!(working.beats.len(), 108 * 60, "every look beats");

    let mut rows = Child::silent();
    rows.advances = STALL_FLOOR_SECS - 60;
    rows.ends = Some((T0 + 40 * STALL_FLOOR_SECS, 0));
    assert_eq!(attend(&mut rows, true), 0);
    assert!(rows.interrupts.is_empty() && rows.said.is_empty());

    let mut lineless = Child::silent();
    lineless.row = None;
    lineless.busy = true;
    lineless.ends = Some((T0 + RUN_ALONE_SECS, 1));
    assert_eq!(attend(&mut lineless, true), 1);
    assert!(lineless.interrupts.is_empty() && lineless.said.is_empty());

    let mut unread = Child::silent();
    unread.idle_since = None;
    unread.ends = Some((T0 + RUN_ALONE_SECS, 1));
    assert_eq!(attend(&mut unread, true), 1);
    assert!(unread.interrupts.is_empty() && unread.said.is_empty());

    let mut asked = Child::silent();
    asked.stop_at = Some(T0 + 30);
    assert_eq!(attend(&mut asked, true), 130);
    assert_eq!(asked.interrupts, [T0 + 30]);
    assert!(asked.said.is_empty(), "a stop is no stall");
}

/// A RUN WITH NO ROW FOR THE CLAIM'S CEILING IS STALLED WHATEVER ITS PROCESSES DO (UB-15). The
/// fleet claim renews nothing for a holder whose newest row is `MAX_LEASE_S` old, so a tree that
/// spins without finishing anything (a livelocked step, a solver loop, a child busy before it
/// takes the claim) loses its line; the pump judges its run by the same ceiling, counted from the
/// look that first saw the run's newest row (its start where it records none): it says the run is
/// stalled, interrupts it, and stops refreshing its fact, so a standby is not kept off the line by
/// a fact the claim no longer backs. The holder's heartbeat renews its record's `at` every five
/// minutes while its tree works with no row (round 4 finding 6), so that renewal time counts for
/// nothing. A publish is not interrupted, but stops vouching at the same ceiling.
/// NEGATIVE CONTROL: a busy run that completes a row every hour is never stalled in twenty hours.
#[test]
fn a_run_with_no_row_for_the_claims_ceiling_is_stalled_whatever_its_processes_do() {
    assert_eq!(RUN_ALONE_SECS, aterm_ctl::release_holder::MAX_LEASE_S);
    let spinning = |row: bool| {
        let mut child = Child::silent();
        child.busy = true;
        if !row {
            child.row = None;
        }
        child.ends = Some((T0 + 40 * STALL_FLOOR_SECS, 0));
        child
    };
    for row in [true, false] {
        let mut child = spinning(row);
        assert_eq!(attend(&mut child, true), 130, "row: {row}");
        assert_eq!(child.interrupts, [T0 + RUN_ALONE_SECS + 1], "row: {row}");
        assert_eq!(child.said.len(), 1, "{:#?}", child.said);
        assert!(
            child.said[0].contains("STALLED") && child.said[0].contains("renews nothing"),
            "{:#?}",
            child.said
        );
        assert!(
            *child.beats.last().unwrap() <= T0 + RUN_ALONE_SECS,
            "no beat past the ceiling"
        );
    }
    // The heartbeat renews the record's `at` every five minutes, and no row completes.
    let mut renewed = spinning(true);
    renewed.renews = 300;
    assert_eq!(attend(&mut renewed, true), 130);
    assert_eq!(renewed.interrupts, [T0 + RUN_ALONE_SECS + 1]);
    assert!(*renewed.beats.last().unwrap() <= T0 + RUN_ALONE_SECS);
    assert!(
        renewed.said[0].contains(&format!("no row for {} min", RUN_ALONE_SECS / 60)),
        "{:#?}",
        renewed.said
    );
    // A publish past the ceiling is said and left alone, and stops vouching for the pump.
    let mut publish = spinning(false);
    assert_eq!(attend(&mut publish, false), 0);
    assert!(publish.interrupts.is_empty());
    assert_eq!(publish.said.len(), 1, "{:#?}", publish.said);
    assert!(*publish.beats.last().unwrap() <= T0 + RUN_ALONE_SECS);
    // NEGATIVE CONTROL: a busy run that keeps completing rows is never stalled.
    let mut rows = spinning(true);
    rows.advances = 60 * 60;
    assert_eq!(attend(&mut rows, true), 0);
    assert!(rows.interrupts.is_empty() && rows.said.is_empty());
    assert_eq!(rows.beats.last(), Some(&(T0 + 40 * STALL_FLOOR_SECS - 1)));
}

/// A STALLED RUN STOPS VOUCHING FOR THE PUMP, A ROW-LESS ONE INCLUDED (UB-15 finding 6). A run
/// that records no row (a child hung before it takes the claim) while its processes do nothing
/// for the floor bound is stalled and interrupted like one whose row went quiet, and from then on
/// the pump's fact is not refreshed: a run that ignores its interrupt (hung in its own code) lets
/// the fact age past `PUMP_STALE_SECS`, so the standby takes the line once its lease has ended,
/// rather than both pumps waiting for good. A publish is never interrupted and keeps vouching: a
/// line being published is not taken.
/// NEGATIVE CONTROL: the same row-less run whose processes work beats at every look and is never
/// interrupted.
#[test]
fn a_stalled_run_stops_vouching_for_the_pump_a_rowless_one_included() {
    let mut hung = Child::silent();
    hung.row = None;
    hung.after_interrupt = None;
    let end = T0 + STALL_FLOOR_SECS + PUMP_STALE_SECS + 100;
    hung.ends = Some((end, 3));
    assert_eq!(attend(&mut hung, true), 3);
    assert_eq!(
        hung.interrupts,
        [T0 + STALL_FLOOR_SECS],
        "interrupted once, at the bound"
    );
    assert_eq!(hung.said.len(), 1, "{:#?}", hung.said);
    assert!(
        hung.said[0].contains("STALLED") && hung.said[0].contains("records no row"),
        "{:#?}",
        hung.said
    );
    let last = *hung.beats.last().unwrap();
    assert!(last < T0 + STALL_FLOOR_SECS, "{last}");
    assert!(
        end - last > PUMP_STALE_SECS,
        "the fact ages past the stale bound"
    );
    // The stalled run with a row is the same.
    let mut quiet = Child::silent();
    quiet.after_interrupt = None;
    quiet.ends = Some((end, 3));
    attend(&mut quiet, true);
    assert!(*quiet.beats.last().unwrap() <= T0 + STALL_FLOOR_SECS);
    // A publish keeps vouching to its end.
    let mut publish = Child::silent();
    publish.row = None;
    publish.ends = Some((end, 0));
    assert_eq!(attend(&mut publish, false), 0);
    assert!(publish.interrupts.is_empty());
    assert_eq!(publish.beats.last(), Some(&(end - 1)));
    // NEGATIVE CONTROL: the row-less run whose processes work is only waited on.
    let mut working = Child::silent();
    working.row = None;
    working.busy = true;
    working.ends = Some((end, 0));
    assert_eq!(attend(&mut working, true), 0);
    assert!(working.interrupts.is_empty() && working.said.is_empty());
    assert_eq!(working.beats.last(), Some(&(end - 1)));
}

/// The input hash of a caller-owned repair line, as `repair::Opened::lines` prints it.
const RED: &str = "831fd1c0e6d2";

/// THE CUTTER'S CALLER-OWNED LINE IS WHAT THE PUMP READS A RED BY. The line the release prints
/// where its repair is its caller's names the input hash; a worker's lines, another refusal and
/// a copy of the line inside detail are no such red.
/// NEGATIVE CONTROL: the pump-route lines of the same red (a worker opened, or none opened for a
/// spawn failure) name the same hash and are not read as caller-owned.
#[test]
fn a_caller_owned_repair_line_names_its_input_hash() {
    let record = std::path::Path::new("/clone/.git/aterm-release/repairs.toml");
    let hash = "831fd1c0e6d2aa0011223344556677889900aabbccddeeff0011223344556677";
    let said = crate::repair::Opened::Caller {
        hash: hash.into(),
        briefing: "A release cut of aterm stopped at a failing gate or build.\nWhat failed: x"
            .into(),
    }
    .lines(record);
    assert_eq!(caller_owned(&said[0]), Some(RED), "{said:?}");
    assert!(said[1..].iter().all(|line| caller_owned(line).is_none()));
    // The line as a cutter frozen at 0.204.0's R printed it (aterm-release-0.204.0-3.log:1121).
    assert_eq!(
        caller_owned("repair: caller-owned repair (input hash 831fd1c0e6d2); no worker opened"),
        Some(RED)
    );
    for not_red in [
        crate::repair::Opened::Spawned {
            hash: hash.into(),
            sid: "s-1".into(),
        }
        .lines(record)[0]
            .clone(),
        crate::repair::Opened::Failed {
            hash: hash.into(),
            why: "no aterm answered".into(),
        }
        .lines(record)[0]
            .clone(),
        "        | repair: caller-owned repair (input hash 831fd1c0e6d2); no worker opened".into(),
        "repair: caller-owned repair (input hash ); no worker opened".into(),
        "repair: caller-owned repair (input hash not-hex); no worker opened".into(),
        "REFUSED at 1/10 checks: the clone is not clean — nothing public. clean it".into(),
    ] {
        assert_eq!(caller_owned(&not_red), None, "{not_red}");
    }
}

/// A RED OF A LINE IS HELD, NOT RUN AGAIN (judge's correction 3). A start goes red after it
/// journaled its line: the next look wants `--prepare` of that journal under another key, which
/// no attempt has tried. The pump's run named the red's input hash, so the line's version and
/// head are held however main moves and however long it waits, with no session opened; a
/// session moves it: a fix carried onto the line moves its head, and a prepare that passes at
/// that head (a session accepted the failure) is past the red.
/// NEGATIVE CONTROL: the same start refused with no caller-owned line (an operational refusal)
/// is resumed by `--prepare` at the next look.
#[test]
fn a_red_line_is_held_at_its_version_and_head_and_never_run_again() {
    let red_start = |caller: bool| {
        let mut lab = Lab::opened();
        lab.start_journals = true;
        lab.exits = vec![1];
        lab.caller = caller.then(|| RED.to_string());
        let mut memory = Memory::default();
        let mut said = String::new();
        assert_eq!(cycle(&mut lab, &mut memory, &mut said), 0);
        assert_eq!(lab.ran, [Verb::Start(OPENED.into())]);
        (lab, memory, said)
    };
    let (mut lab, mut memory, mut said) = red_start(true);
    assert_eq!(
        memory.red,
        Some(Red {
            version: "0.6.0".into(),
            head: Some(OPENED.into()),
            journaled: true,
            hash: RED.into(),
            at: lab.now,
            last: "final line of exit 1".into(),
        })
    );
    for tip in [MOVED, HOP, GREEN, TIP] {
        lab.main.tip = tip.into();
        lab.now += REFUSED_CAP_SECS;
        assert_eq!(cycle(&mut lab, &mut memory, &mut said), POLL_SECS);
    }
    assert_eq!(lab.ran.len(), 1, "{:#?}", lab.said);
    let held: Vec<_> = lab
        .said
        .iter()
        .filter(|line| line.contains("is red"))
        .collect();
    assert_eq!(held.len(), 1, "said once: {:#?}", lab.said);
    assert!(
        held[0].contains(RED) && held[0].contains("--fix") && held[0].contains("--forget"),
        "{}",
        held[0]
    );
    // A restart keeps the hold.
    let text = aterm_toml::to_string(&memory).unwrap();
    let restored: Memory = aterm_toml::from_str(&text).unwrap();
    assert_eq!(restored.red, memory.red);
    assert!(waits(&decide(&lab.seen(), &restored)).contains("is red"));
    // A session accepted the failure and prepared the line at the same head: past the red.
    let journal = lab.journal.as_mut().unwrap();
    journal.prepared = true;
    journal.prepared_at = lab.now;
    lab.publishes = true;
    assert_eq!(runs(&decide(&lab.seen(), &memory)), Some(&Verb::Finish));
    // A session carried a fix onto the line instead: its new head is not held.
    let journal = lab.journal.as_mut().unwrap();
    journal.prepared = false;
    journal.head = HOP.into();
    assert_eq!(runs(&decide(&lab.seen(), &memory)), Some(&Verb::Prepare));
    // A session forgot the line: no line here, and the version starts again.
    lab.journal = None;
    lab.main.line = None;
    assert!(matches!(
        runs(&decide(&lab.seen(), &memory)),
        Some(Verb::Start(_))
    ));

    // NEGATIVE CONTROL.
    let (mut lab, mut memory, mut said) = red_start(false);
    assert_eq!(memory.red, None);
    assert_eq!(cycle(&mut lab, &mut memory, &mut said), 0);
    assert_eq!(lab.ran, [Verb::Start(OPENED.into()), Verb::Prepare]);
}

/// A RED OF THE PUBLISH HOLDS THE PREPARED LINE. The bare run of a PREPARED line went red: the
/// red is newer than the prepare, so the line is held; with no red line the publish is tried
/// again with backoff as main moves.
#[test]
fn a_red_publish_holds_the_prepared_line() {
    let publish = |caller: bool| {
        let mut lab = Lab::opened();
        lab.publishes = true;
        let journal = lab.line(OPENED);
        journal.prepared = true;
        journal.prepared_at = 900;
        lab.exits = vec![1];
        lab.caller = caller.then(|| RED.to_string());
        let mut memory = Memory::default();
        let mut said = String::new();
        assert_eq!(cycle(&mut lab, &mut memory, &mut said), 0);
        lab.main.tip = MOVED.into();
        lab.now += REFUSED_CAP_SECS;
        cycle(&mut lab, &mut memory, &mut said);
        lab.ran
    };
    assert_eq!(publish(true), [Verb::Finish]);
    assert_eq!(publish(false), [Verb::Finish, Verb::Finish], "the control");
}

/// A START THAT WENT RED WITH NO JOURNAL HOLDS ITS VERSION. No worker can say main now holds a
/// fix, and a start at a newer commit is a guess that costs a whole qualification: none is made
/// until a session starts the line or main declares another version.
/// NEGATIVE CONTROL: with no caller-owned line the start is tried again as main moves, past
/// its backoff.
#[test]
fn a_start_that_went_red_with_no_journal_holds_its_version() {
    let red_start = |caller: bool| {
        let mut lab = Lab::opened();
        lab.exits = vec![1];
        lab.caller = caller.then(|| RED.to_string());
        let mut memory = Memory::default();
        let mut said = String::new();
        assert_eq!(cycle(&mut lab, &mut memory, &mut said), 0);
        // main moves on, far past the commit that opened the version.
        lab.main.tip = MOVED.into();
        lab.main.behind = STALE_BASE_COMMITS + 1;
        lab.now += REFUSED_CAP_SECS;
        cycle(&mut lab, &mut memory, &mut said);
        (lab, memory)
    };
    let (mut lab, memory) = red_start(true);
    assert_eq!(lab.ran, [Verb::Start(OPENED.into())], "{:#?}", lab.said);
    let red = memory.red.clone().unwrap();
    assert_eq!(
        (red.version.as_str(), red.head.as_deref(), red.journaled),
        ("0.6.0", Some(OPENED), false)
    );
    assert!(
        lab.said
            .last()
            .unwrap()
            .contains("the pump starts no line of it again")
    );
    // The next version is another version: nothing of the red holds it.
    lab.main.version = "0.7.0".into();
    assert!(matches!(
        runs(&decide(&lab.seen(), &memory)),
        Some(Verb::Start(_))
    ));
    let (lab, _) = red_start(false);
    assert_eq!(
        lab.ran,
        [Verb::Start(OPENED.into()), Verb::Start(MOVED.into())],
        "the control"
    );
}

/// A RED OF THE OPENING IS NO LINE'S. `--open-next` judges main's tip, which moves and can be
/// repaired by main's own landings: it is a refusal that backs off, and no line is held for it.
#[test]
fn a_red_opening_holds_no_line() {
    let mut lab = Lab::after_a_release();
    lab.exits = vec![1];
    lab.caller = Some(RED.into());
    let mut memory = Memory::default();
    let mut said = String::new();
    assert_eq!(cycle(&mut lab, &mut memory, &mut said), 0);
    assert_eq!(memory.red, None);
    lab.main.tip = MOVED.into();
    lab.now += REFUSED_CAP_SECS;
    assert_eq!(cycle(&mut lab, &mut memory, &mut said), 0);
    assert_eq!(lab.ran, [Verb::OpenNext, Verb::OpenNext]);
}

/// A REFUSAL BACKS OFF TO SIX HOURS, NOT ONE. main moves at nearly every look, so a write
/// refused for a cause no landing clears (the damaged write journal of 2026-11-16) would
/// otherwise be run every hour for good.
#[test]
fn a_refusal_backs_off_to_six_hours() {
    let mut lab = Lab::opened();
    lab.line(OPENED);
    let memory = attempt(&mut lab, 1, 30);
    lab.main.tip = MOVED.into();
    lab.now += 60 * 60;
    let waiting = decide(&lab.seen(), &memory);
    assert!(
        waits(&waiting).contains("refused with no repair worker"),
        "an hour on: {waiting:?}"
    );
    lab.now += 5 * 60 * 60 - 1;
    assert!(runs(&decide(&lab.seen(), &memory)).is_none());
    lab.now += 1;
    assert_eq!(runs(&decide(&lab.seen(), &memory)), Some(&Verb::Prepare));
}

/// THE PAUSE BETWEEN VERSIONS IS READ FROM MAIN. `PUMP_OPEN_EVERY_DEFAULT` is one flat config
/// line of whole seconds; a line that names the key and does not read is no pause and opens
/// nothing, never an unset pause.
#[test]
fn the_pause_between_versions_is_one_flat_config_line() {
    assert_eq!(
        open_every("STAGING=\"x\"\nPUMP_OPEN_EVERY_DEFAULT=\"28800\"\n"),
        Every::Secs(28_800)
    );
    assert_eq!(open_every("STAGING=\"x\"\n"), Every::Unset);
    assert_eq!(
        open_every("# PUMP_OPEN_EVERY_DEFAULT=\"60\" in a comment\n"),
        Every::Unset
    );
    for unread in [
        "PUMP_OPEN_EVERY_DEFAULT=\"8h\"\n",
        "PUMP_OPEN_EVERY_DEFAULT=\"\"\n",
        "PUMP_OPEN_EVERY_DEFAULT=\"-1\"\n",
        "PUMP_OPEN_EVERY_DEFAULT=28800\n",
        "PUMP_OPEN_EVERY_DEFAULT=\"28800\" # three a day\n",
    ] {
        assert!(
            matches!(open_every(unread), Every::Unread(_)),
            "{unread:?}: {:?}",
            open_every(unread)
        );
    }
}

/// THE NEXT VERSION OPENS NO SOONER THAN THE PAUSE AFTER THE COMMIT THAT OPENED THE CUT ONE.
/// The pause is measured from main's history, so a restarted pump, or one on another clone,
/// keeps it. The wait is said once, with its end; once it has passed, `--open-next` runs.
/// NEGATIVE CONTROL: with no pause on main the version after a cut one opens at once. A pause
/// that does not read opens nothing; an opening commit main's history does not name cannot be
/// measured, and the version opens, saying so.
#[test]
fn the_next_version_opens_no_sooner_than_the_pause_after_the_last_opened() {
    const EIGHT_HOURS: u64 = 8 * 60 * 60;
    let paused = |every: Every| {
        let mut lab = Lab::after_a_release();
        lab.now = 100_000;
        lab.every = every;
        lab.main.opened = Some(OPENED.into());
        lab.main.opened_at = Some(lab.now - 3 * 60 * 60);
        lab
    };
    let mut lab = paused(Every::Secs(EIGHT_HOURS));
    let mut memory = Memory::default();
    let mut said = String::new();
    while lab.now < 100_000 - 3 * 60 * 60 + EIGHT_HOURS {
        assert_eq!(cycle(&mut lab, &mut memory, &mut said), POLL_SECS);
        lab.now += POLL_SECS;
    }
    assert!(lab.ran.is_empty());
    assert_eq!(lab.said.len(), 1, "said once: {:#?}", lab.said);
    assert!(
        lab.said[0].contains("PUMP_OPEN_EVERY_DEFAULT=\"28800\"")
            && lab.said[0].contains("no sooner than")
            && lab.said[0].contains("18000 s from now"),
        "{}",
        lab.said[0]
    );
    assert_eq!(cycle(&mut lab, &mut memory, &mut said), 0);
    assert_eq!(lab.ran, [Verb::OpenNext]);

    let mut unset = paused(Every::Unset);
    assert_eq!(
        runs(&decide(&unset.seen(), &Memory::default())),
        Some(&Verb::OpenNext)
    );
    let mut unread = paused(Every::Unread("not seconds".into()));
    assert!(waits(&decide(&unread.seen(), &Memory::default())).contains("does not read"));
    let mut unmeasured = paused(Every::Secs(EIGHT_HOURS));
    unmeasured.main.opened_at = None;
    let decision = decide(&unmeasured.seen(), &Memory::default());
    assert_eq!(runs(&decision), Some(&Verb::OpenNext));
    let Decision::Run { why, .. } = decision else {
        unreachable!()
    };
    assert!(why.contains("is not measured"), "{why}");
}

/// WHAT THE PUMP DOES IS KEPT FOR A READER THAT IS NOT THE PUMP. Each reason it waits for is
/// kept with its pid, its process's start and since when, once, when it changes: a later look
/// with the same reason keeps the first `since`. A run is kept before it begins, and the pump's
/// end is kept as its last word.
#[test]
fn what_the_pump_does_and_since_when_is_kept_in_its_memory() {
    let mut lab = Lab::after_a_release();
    lab.every = Every::Secs(10 * POLL_SECS);
    lab.main.opened_at = Some(lab.now);
    let started = lab.now;
    lab.patience = 12;
    pump(&mut lab, Memory::default(), None);
    // Ten looks waited on the pause, then the opening ran, then the start, then it stopped.
    assert_eq!(lab.ran, [Verb::OpenNext, Verb::Start(OPENED.into())]);
    let waited = lab.doing_at_run[0].clone().unwrap();
    assert!(
        waited.reason.starts_with("main declares 0.5.0")
            && waited.reason.contains("running release --open-next"),
        "the run is kept before it begins: {waited:?}"
    );
    assert_eq!(
        (waited.pid, waited.started.as_str()),
        (4242, "Tue Nov 17 10:00:00 2026")
    );
    let kept = lab.remembered.clone().unwrap();
    let doing = kept.doing.clone().unwrap();
    assert_eq!(doing.reason, "stopped");
    let text = aterm_toml::to_string(&kept).unwrap();
    assert_eq!(aterm_toml::from_str::<Memory>(&text).unwrap(), kept);

    // A wait whose reason does not change keeps the first since.
    let mut lab = Lab::after_a_release();
    lab.every = Every::Secs(10 * POLL_SECS);
    lab.main.opened_at = Some(lab.now);
    let mut memory = Memory::default();
    let mut said = String::new();
    for _ in 0..5 {
        cycle(&mut lab, &mut memory, &mut said);
        lab.now += POLL_SECS;
    }
    let doing = memory.doing.clone().unwrap();
    assert_eq!(doing.since, started);
    assert!(
        doing.reason.starts_with("waiting — main declares 0.5.0"),
        "{doing:?}"
    );
    assert_eq!(lab.remembered.unwrap().doing, Some(doing));
}

/// The one re-base the pump runs of `head` onto `onto` for a red its own run met, `hash` the
/// caller-owned repair's input hash.
fn rebase_for_red(onto: &str, hash: &str) -> Verb {
    Verb::Rebase {
        onto: onto.into(),
        requalify: Requalify::Red(format!("caller-owned repair, input hash {hash}")),
    }
}

/// RE-BASE AT RE-QUALIFY (UB-15). A line that must re-qualify anyway re-bases once onto the
/// newest judged pass that descends from its head (`Seen::green`), by `--prepare --commit <it>
/// --supersede`, for its typed reason: a run of the line went red, or a repair worker's fix is
/// accepted, which the pass must hold. A hop and a re-base cost the same re-qualification, and a
/// hop leaves out every fix landed after the base (0.201.0 carried none of four incident fixes).
/// NEGATIVE CONTROLS: a line that need not re-qualify (it resumes) is not re-based however new
/// its judged pass, and nothing asks for a target; a pass that cannot take the line (it does not
/// hold the fix) or no pass at all leaves the fix to a hop; a red with no pass past it holds.
#[test]
fn a_requalifying_line_rebases_once_onto_the_newest_judged_pass() {
    // The line went red: the pump's own prepare ended at a caller-owned repair.
    let mut lab = Lab::opened();
    lab.line(OPENED);
    lab.exits = vec![1];
    lab.caller = Some("ab12".into());
    lab.greens.insert(OPENED.into(), GREEN.into());
    let (mut memory, mut said) = (Memory::default(), String::new());
    assert_eq!(cycle(&mut lab, &mut memory, &mut said), 0);
    assert_eq!(lab.ran, [Verb::Prepare]);
    assert_eq!(cycle(&mut lab, &mut memory, &mut said), 0);
    let rebase = rebase_for_red(GREEN, "ab12");
    assert_eq!(lab.ran, [Verb::Prepare, rebase.clone()], "{:#?}", lab.said);
    assert_eq!(
        rebase.args(&[]).join(" "),
        format!("release --prepare --commit {GREEN} --supersede")
    );
    assert_eq!(
        lab.onto_asked,
        [("0.6.0".into(), OPENED.into(), GREEN.into(), None)],
        "the newest judged pass past the head is the target, asked with no fix"
    );
    assert!(
        lab.said.iter().any(|line| line.contains(
            "requalify: the line went red (caller-owned repair, \
                                        input hash ab12)"
        ) && line.contains("--supersede")),
        "{:#?}",
        lab.said
    );
    let kept = memory.rebased.clone().expect("the re-base is remembered");
    assert_eq!(
        (
            kept.version.as_str(),
            kept.from.as_str(),
            kept.onto.as_str()
        ),
        ("0.6.0", OPENED, GREEN)
    );
    assert_eq!(
        (kept.code, kept.requalify),
        (
            0,
            Requalify::Red("caller-owned repair, input hash ab12".into())
        )
    );
    let journal = lab.journal.clone().unwrap();
    assert!(journal.head == GREEN && lab.main.superseded && journal.prepared);
    assert!(
        lab.seen_with(&memory).journal.unwrap().rebased,
        "read from main"
    );
    let text = aterm_toml::to_string(&memory).unwrap();
    assert_eq!(aterm_toml::from_str::<Memory>(&text).unwrap(), memory);

    // A repair worker's fix is accepted: the line re-bases onto a pass that holds it, and the
    // question names the fix.
    let mut lab = Lab::opened();
    lab.line(OPENED);
    lab.report("0.6.0", OPENED, "s-1", Report::Fix(FIX.into()));
    lab.greens.insert(OPENED.into(), GREEN.into());
    let for_fix = Verb::Rebase {
        onto: GREEN.into(),
        requalify: Requalify::Fix(FIX.into()),
    };
    assert_eq!(
        runs(&decide(&lab.seen(), &Memory::default())),
        Some(&for_fix)
    );
    assert_eq!(lab.onto_asked.last().unwrap().3.as_deref(), Some(FIX));
    // And a fix the line cannot carry as a hop, which the pass holds.
    lab.unfit.insert(FIX.into(), "a merge commit".into());
    assert_eq!(
        runs(&decide(&lab.seen(), &Memory::default())),
        Some(&for_fix)
    );
    lab.unfit.clear();
    // NEGATIVE CONTROL: a pass that cannot take the line is no target: the fix hops.
    lab.unfit_onto
        .insert(GREEN.into(), "it does not hold the fix".into());
    assert_eq!(
        runs(&decide(&lab.seen(), &Memory::default())),
        Some(&Verb::Fix(FIX.into()))
    );
    // NEGATIVE CONTROL: with no judged pass past the head the fix hops.
    lab.unfit_onto.clear();
    lab.greens.clear();
    assert_eq!(
        runs(&decide(&lab.seen(), &Memory::default())),
        Some(&Verb::Fix(FIX.into()))
    );

    // NEGATIVE CONTROL: a line that need not re-qualify resumes, whatever pass is judged past it,
    // and no target is read for it.
    let mut lab = Lab::opened();
    lab.line(OPENED);
    lab.green = Some(GREEN.into());
    assert_eq!(
        runs(&decide(&lab.seen(), &Memory::default())),
        Some(&Verb::Prepare)
    );
    assert!(lab.onto_asked.is_empty(), "{:?}", lab.onto_asked);
    // NEGATIVE CONTROL: a red with no judged pass past its head holds for a session.
    let mut lab = Lab::opened();
    lab.line(OPENED);
    lab.exits = vec![1];
    lab.caller = Some("ab12".into());
    let (mut memory, mut said) = (Memory::default(), String::new());
    cycle(&mut lab, &mut memory, &mut said);
    assert_eq!(cycle(&mut lab, &mut memory, &mut said), POLL_SECS);
    assert_eq!(lab.ran, [Verb::Prepare]);
    assert!(
        lab.said
            .last()
            .unwrap()
            .contains("there is no re-base or supersede candidate"),
        "{:#?}",
        lab.said
    );
}

/// A LINE RE-BASES AT MOST ONCE: every later red goes back to hops (UB-15). After its re-base a
/// red of the new head holds for a session, however new the pass judged past it, and a repair
/// worker's fix is carried as a hop. The re-base is spent by main's record of the supersession,
/// which every clone reads, and in this clone by the pump's memory of a re-base refused before
/// it recorded anything, so a refused re-base is not tried again here in place of the hop; that
/// spend is per clone, and another clone's memory holds none.
/// NEGATIVE CONTROL: the same red on a line that has not re-based re-bases.
#[test]
fn a_rebased_line_goes_back_to_hops() {
    let mut lab = Lab::opened();
    lab.publishes = true;
    lab.line(OPENED);
    lab.exits = vec![1, 0, 1];
    lab.caller = Some("ab12".into());
    lab.greens.insert(OPENED.into(), GREEN.into());
    lab.greens.insert(GREEN.into(), LATER.into());
    let (mut memory, mut said) = (Memory::default(), String::new());
    // The prepare goes red, the line re-bases, and the publish of its new head goes red.
    for _ in 0..3 {
        assert_eq!(cycle(&mut lab, &mut memory, &mut said), 0);
    }
    assert_eq!(
        lab.ran,
        [Verb::Prepare, rebase_for_red(GREEN, "ab12"), Verb::Finish]
    );
    for tip in [MOVED, HOP, TIP] {
        lab.main.tip = tip.into();
        lab.now += 10 * REFUSED_CAP_SECS;
        assert_eq!(cycle(&mut lab, &mut memory, &mut said), POLL_SECS);
    }
    assert_eq!(
        lab.ran.len(),
        3,
        "a red of a re-based line runs nothing: {:#?}",
        lab.said
    );
    let said_last = lab.said.last().unwrap();
    assert!(
        said_last.contains("has had its one re-base")
            && said_last.contains(&format!("--commit {LATER} --supersede")),
        "{said_last}"
    );
    assert!(
        lab.onto_asked.iter().all(|asked| asked.1 == OPENED),
        "a re-based line asks for no second target: {:?}",
        lab.onto_asked
    );
    // A worker's fix of the new head is a hop.
    lab.report("0.6.0", GREEN, "s-2", Report::Fix(FIX2.into()));
    // The decision itself refuses a second re-base, whatever the reads say of its target.
    let mut fit = lab.seen_with(&memory);
    assert_eq!(fit.green.as_deref(), Some(LATER));
    fit.onto = Some(Ok(()));
    assert_eq!(runs(&decide(&fit, &memory)), Some(&Verb::Fix(FIX2.into())));
    assert_eq!(cycle(&mut lab, &mut memory, &mut said), 0);
    assert_eq!(lab.ran.last(), Some(&Verb::Fix(FIX2.into())));

    // The memory alone spends it: a re-base refused before it recorded anything (main holds no
    // supersession) leaves the fix to a hop, not to a second try.
    let mut lab = Lab::opened();
    lab.line(OPENED);
    lab.report("0.6.0", OPENED, "s-1", Report::Fix(FIX.into()));
    lab.greens.insert(OPENED.into(), GREEN.into());
    lab.exits = vec![1];
    let (mut memory, mut said) = (Memory::default(), String::new());
    assert_eq!(cycle(&mut lab, &mut memory, &mut said), 0);
    assert!(!lab.main.superseded, "nothing reached main");
    assert_eq!(memory.rebased.as_ref().map(|kept| kept.code), Some(1));
    // That spend is this clone's memory alone: main records nothing, so another clone that takes
    // the line (a standby, once this pump is dead or stalled) may try its re-base once itself.
    assert!(
        matches!(
            runs(&decide(&lab.seen(), &Memory::default())),
            Some(Verb::Rebase { .. })
        ),
        "another clone's memory holds no spend"
    );
    assert_eq!(cycle(&mut lab, &mut memory, &mut said), 0);
    assert_eq!(
        lab.ran,
        [
            Verb::Rebase {
                onto: GREEN.into(),
                requalify: Requalify::Fix(FIX.into())
            },
            Verb::Fix(FIX.into())
        ]
    );
    // A re-base that could not run decided nothing: it runs again, by its exit's backoff.
    let mut lab = Lab::opened();
    lab.line(OPENED);
    lab.report("0.6.0", OPENED, "s-1", Report::Fix(FIX.into()));
    lab.greens.insert(OPENED.into(), GREEN.into());
    lab.exits = vec![3];
    let (mut memory, mut said) = (Memory::default(), String::new());
    cycle(&mut lab, &mut memory, &mut said);
    assert_eq!(memory.rebased, None);
    lab.now += POLL_SECS;
    cycle(&mut lab, &mut memory, &mut said);
    assert!(
        matches!(lab.ran[..], [Verb::Rebase { .. }, Verb::Rebase { .. }]),
        "{:?}",
        lab.ran
    );

    // A re-base the pump never started (no Linux declaration to hand its verb: a line adopted
    // inside a resume, main's config unread) decided nothing either (round 4 finding 9): it spends
    // nothing, and runs again once main moves.
    let mut lab = Lab::opened();
    lab.line(OPENED);
    lab.report("0.6.0", OPENED, "s-1", Report::Fix(FIX.into()));
    lab.greens.insert(OPENED.into(), GREEN.into());
    lab.unstarted = true;
    let (mut memory, mut said) = (Memory::default(), String::new());
    cycle(&mut lab, &mut memory, &mut said);
    assert!(
        matches!(lab.ran[..], [Verb::Rebase { .. }]),
        "{:?}",
        lab.ran
    );
    assert_eq!(memory.rebased, None, "no release process ran");
    lab.main.tip = MOVED.into();
    lab.now += 10 * REFUSED_CAP_SECS;
    cycle(&mut lab, &mut memory, &mut said);
    assert!(
        matches!(lab.ran[..], [Verb::Rebase { .. }, Verb::Rebase { .. }]),
        "{:?}",
        lab.ran
    );

    // NEGATIVE CONTROL: the line that has not re-based re-bases at the same red.
    let mut lab = Lab::opened();
    lab.line(GREEN);
    lab.report("0.6.0", GREEN, "s-2", Report::Fix(FIX2.into()));
    lab.greens.insert(GREEN.into(), LATER.into());
    let fresh = decide(&lab.seen(), &Memory::default());
    assert_eq!(
        runs(&fresh),
        Some(&Verb::Rebase {
            onto: LATER.into(),
            requalify: Requalify::Fix(FIX2.into())
        })
    );
    lab.main.superseded = true;
    assert_eq!(
        runs(&decide(&lab.seen(), &Memory::default())),
        Some(&Verb::Fix(FIX2.into()))
    );
}

/// The fact of a pump on the holder's Mac that ran its last look `age` s ago: `own`, it holds
/// the journal of v0.6.0's line at GREEN; `alive`, its process as that Mac's kernel judged it.
fn pump_there(
    alive: Option<bool>,
    own: bool,
    age: u64,
) -> Option<Result<Option<Vec<PumpRow>>, String>> {
    Some(Ok(Some(vec![PumpRow {
        alive,
        line: format!("v0.6.0@{}", &GREEN[..9]),
        step: "wait".into(),
        own: Some(own),
        age,
    }])))
}

/// ONE STANDBY TAKES THE LINE ONLY FROM A DEAD OR STALLED PUMP (UB-15). A second rostered cut
/// host runs `ship release --pump --adopt --mac-only` beside the line's holder. The holder a
/// claim names is the primary pump's release child, which is not running between that pump's
/// runs, so the claim frees the line at every pause of a live pump (a red it holds for a
/// session, a backoff, PREPARED without the direction): the standby judges the pump on the
/// holder's Mac too. It runs nothing while the record keeps a contender off, nor, once the record
/// frees the line, while the holder's pump is live (it holds the line's journal, its process is
/// not proven dead, and it looked within `PUMP_STALE_SECS`), whatever hours pass; it adopts once
/// that pump's process has been dead past `RESTART_GRACE_SECS` or it stops looking. A pump it
/// cannot read is waited on until the claim's lease ends unrenewed and it has been unreadable for
/// `PUMP_STALE_SECS`. A line its holder already re-based is adopted with its re-base spent, read
/// from main: the standby's next red goes to a hop, never a second re-base.
/// NEGATIVE CONTROLS: a live pump on that Mac that holds no journal of the line (a session held
/// it there) does not hold it; a pump that looked one second within the bound still holds it;
/// without `--adopt` nothing is taken, the holder dead or not; the adopted line read as not
/// re-based would re-base.
#[test]
fn a_standby_takes_the_line_only_from_a_dead_or_stalled_pump() {
    let standby = |adopt: bool| {
        let mut lab = Lab::opened();
        lab.adopt = adopt;
        lab.main.line = Some(GREEN.into());
        lab.main.holder = Some("m16 (node 7c1e, session s-1, pid 41)".into());
        lab.holder_pump = pump_there(Some(true), true, 30);
        lab
    };
    let mut lab = standby(true);
    let (mut memory, mut said) = (Memory::default(), String::new());
    // Eight hours of a primary pump that keeps its line: each hour its release child runs for
    // twenty minutes, renewing the claim's lease, then ends, and the pump holds the line (a red,
    // a backoff) with the claim's holder dead and its lease run out.
    for minute in 0..480u64 {
        let running = minute % 60 < 20;
        lab.main.held = running.then(|| held(lab.now + 1_800));
        lab.main.lease_until = Some(if running {
            lab.now + 1_800
        } else {
            lab.now.saturating_sub(1)
        });
        lab.now += cycle(&mut lab, &mut memory, &mut said);
    }
    assert!(lab.ran.is_empty(), "{:?}", lab.ran);
    assert!(
        lab.holder_pump_asked > 0,
        "the pump was judged between the runs"
    );
    assert!(
        lab.facts
            .iter()
            .all(|fact| fact.step == "wait" && fact.holder.starts_with("m16") && !fact.own)
    );
    assert!(
        lab.said
            .iter()
            .any(|line| line.contains("the pump on its Mac holds the line's journal and is live")),
        "{:#?}",
        lab.said
    );
    let free = |lab: &mut Lab| {
        lab.main.held = None;
        lab.main.lease_until = Some(lab.now.saturating_sub(1));
    };
    free(&mut lab);
    // NEGATIVE CONTROL: one second within the bound the pump still holds its line.
    lab.holder_pump = pump_there(Some(true), true, PUMP_STALE_SECS);
    assert!(runs(&decide(&lab.seen_with(&memory), &memory)).is_none());
    // A pump that has stopped looking is stalled, and the standby adopts.
    lab.holder_pump = pump_there(Some(true), true, PUMP_STALE_SECS + 1);
    assert_eq!(
        runs(&decide(&lab.seen_with(&memory), &memory)),
        Some(&Verb::Adopt)
    );
    // NEGATIVE CONTROL: a live pump there that holds no journal of the line does not hold it.
    lab.holder_pump = pump_there(Some(true), false, 30);
    assert_eq!(
        runs(&decide(&lab.seen_with(&memory), &memory)),
        Some(&Verb::Adopt)
    );
    // A pump its Mac's answer does not show is waited on until the lease ends unrenewed and it
    // has been unreadable for the stale bound.
    lab.holder_pump = Some(Err("it did not answer: timed out".into()));
    lab.main.lease_until = Some(lab.now + 600);
    let unread = decide(&lab.seen_with(&memory), &memory);
    assert!(
        waits(&unread).contains("cannot be read (its Mac's answer could not be read"),
        "{unread:?}"
    );
    lab.main.lease_until = Some(lab.now - 1);
    let unread_since = Memory {
        pump_unread: Some(PumpUnread {
            line: format!("v0.6.0@{}", &GREEN[..9]),
            since: lab.now - PUMP_STALE_SECS,
            last: lab.now - POLL_SECS,
        }),
        ..memory.clone()
    };
    assert_eq!(
        runs(&decide(&lab.seen_with(&unread_since), &unread_since)),
        Some(&Verb::Adopt)
    );
    // The primary pump dies: its process reads dead past the restart grace, and the standby
    // adopts.
    lab.main.lease_until = Some(lab.now + 600);
    lab.holder_pump = pump_there(Some(false), true, RESTART_GRACE_SECS + 1);
    assert_eq!(cycle(&mut lab, &mut memory, &mut said), 0);
    assert_eq!(lab.ran, [Verb::Adopt]);
    let asked = lab.holder_pump_asked;
    // The line it adopted is red, with a repair worker's fix and a newer judged pass.
    lab.report("0.6.0", GREEN, "s-5", Report::Fix(FIX2.into()));
    lab.greens.insert(GREEN.into(), LATER.into());
    // NEGATIVE CONTROL: read as not re-based, it would re-base.
    assert!(matches!(
        runs(&decide(&lab.seen_with(&memory), &memory)),
        Some(Verb::Rebase { .. })
    ));
    // Its first holder re-based it: main records the supersession, which the standby reads.
    lab.main.superseded = true;
    assert_eq!(cycle(&mut lab, &mut memory, &mut said), 0);
    assert_eq!(lab.ran, [Verb::Adopt, Verb::Fix(FIX2.into())]);
    assert_eq!(
        lab.holder_pump_asked, asked,
        "a line held here asks no other Mac after its pump"
    );

    // NEGATIVE CONTROL: without --adopt the holder's line is never taken, dead or not.
    let mut lab = standby(false);
    lab.holder_pump = pump_there(Some(false), true, RESTART_GRACE_SECS + 1);
    let (mut memory, mut said) = (Memory::default(), String::new());
    for _ in 0..30 {
        lab.now += cycle(&mut lab, &mut memory, &mut said);
    }
    assert!(lab.ran.is_empty(), "{:?}", lab.ran);
    assert_eq!(lab.holder_pump_asked, 0);
    assert!(
        lab.said
            .iter()
            .any(|line| line.contains("only when started with --adopt")),
        "{:#?}",
        lab.said
    );
}

/// WHETHER A LIVE PUMP HOLDS A LINE IS ITS FACT'S, READ STRICTLY (UB-15). A pump holds the fleet
/// line only where its process is not proven dead, it looked within `PUMP_STALE_SECS` and it
/// holds a journal of that very line: of the fleet's head (finding 9), or, while it runs a verb
/// that may move the line (a hop, a re-base), of any head of the version. A dead pump that held
/// it may be restarting (launchd's KeepAlive, a re-install) and holds it for `RESTART_GRACE_SECS`
/// (findings 5 and 15). Each clone's pump answers a row of its own, and one that holds the line
/// holds it whatever another says (findings 8 and 14: a `--once` of another clone, which exited
/// holding nothing). No answer, no row and a row that does not say whether it holds the journal
/// are unknown, never gone; a whole list with no such pump is gone. A live, fresh, waiting pump
/// whose journal of the version is at another head than the fleet's is unknown (round 4 finding
/// 1): its fact may name a hop the claim does not record yet (a run that ended between the hop's
/// record and the claim's next boundary), or a line a session moved from another clone and left,
/// either of which the lease and `PUMP_STALE_SECS` wait out as they do an unreadable pump.
/// NEGATIVE CONTROLS: a fresh fact of another version's line or of a pump that holds no journal is
/// gone; a fact one second past either bound is gone, at the fleet's head or another; the `--once`
/// row alone is gone.
#[test]
fn a_holders_pump_is_live_only_by_a_fresh_fact_of_the_lines_journal() {
    let row = |alive: Option<bool>, line: &str, own: Option<bool>, age: u64| PumpRow {
        alive,
        line: line.into(),
        step: "wait".into(),
        own,
        age,
    };
    let one = |row: PumpRow| Ok(Some(vec![row]));
    let judge = |answer| holder_pump(answer, "0.6.0", GREEN);
    let at = format!("v0.6.0@{}", &GREEN[..9]);
    assert_eq!(
        judge(one(row(Some(true), &at, Some(true), 30))),
        HolderPump::Live { age: 30 }
    );
    assert_eq!(
        judge(one(row(None, &at, Some(true), PUMP_STALE_SECS))),
        HolderPump::Live {
            age: PUMP_STALE_SECS
        },
        "a kernel that did not judge leaves the fact's age to speak"
    );
    let gone = |answer, why: &str| match holder_pump(answer, "0.6.0", GREEN) {
        HolderPump::Gone(said) => assert!(said.contains(why), "{said}"),
        other => panic!("{other:?}"),
    };
    // A waiting pump whose journal is at another head of the version is not known to hold the
    // fleet's line, nor known not to; one that runs a verb may be moving the line, and holds any
    // head of it.
    let elsewhere = format!("v0.6.0@{}", &OPENED[..9]);
    match judge(one(row(Some(true), &elsewhere, Some(true), 30))) {
        HolderPump::Unknown(why) => {
            assert!(
                why.contains("not of the fleet line v0.6.0@222222222"),
                "{why}"
            );
        }
        other => panic!("{other:?}"),
    }
    gone(
        one(row(Some(true), &elsewhere, Some(true), PUMP_STALE_SECS + 1)),
        "has not looked for",
    );
    gone(
        one(row(
            Some(false),
            &elsewhere,
            Some(true),
            RESTART_GRACE_SECS + 1,
        )),
        "is dead",
    );
    for step in ["prepare", "fix", "rebase", "start"] {
        let running = PumpRow {
            step: step.into(),
            ..row(Some(true), &elsewhere, Some(true), 30)
        };
        assert_eq!(judge(one(running)), HolderPump::Live { age: 30 }, "{step}");
    }
    // Findings 5 and 15: a dead pump that held the line may be restarting.
    assert_eq!(
        judge(one(row(Some(false), &at, Some(true), 30))),
        HolderPump::Restarting { age: 30 }
    );
    assert_eq!(
        judge(one(row(Some(false), &at, Some(true), RESTART_GRACE_SECS))),
        HolderPump::Restarting {
            age: RESTART_GRACE_SECS
        }
    );
    gone(
        one(row(Some(false), &at, Some(true), RESTART_GRACE_SECS + 1)),
        "is dead",
    );
    gone(
        one(row(Some(false), &at, Some(false), 0)),
        "holds no journal",
    );
    gone(
        one(row(Some(true), &at, Some(true), PUMP_STALE_SECS + 1)),
        "has not looked for",
    );
    gone(
        one(row(Some(true), "v0.7.0@abc", Some(true), 30)),
        "holds a journal of v0.7.0@abc, not of the fleet line",
    );
    gone(
        one(row(Some(true), "v0.6.00@abc", Some(true), 30)),
        "not of the fleet line",
    );
    gone(
        one(row(Some(true), &at, Some(false), 30)),
        "holds no journal",
    );
    // Findings 8 and 14: another clone's `--once` answers a row of its own beside the primary's.
    let once = row(Some(false), &at, Some(false), 5);
    let primary = row(Some(true), &at, Some(true), 40);
    for rows in [
        vec![once.clone(), primary.clone()],
        vec![primary, once.clone()],
    ] {
        assert_eq!(judge(Ok(Some(rows))), HolderPump::Live { age: 40 });
    }
    gone(Ok(Some(vec![once])), "holds no journal");
    gone(Ok(Some(Vec::new())), "no pump runs there");
    let unknown = |answer| matches!(judge(answer), HolderPump::Unknown(_));
    assert!(unknown(Err("timed out".into())));
    assert!(unknown(Ok(None)));
    assert!(unknown(one(row(Some(true), &at, None, 30))));
}

/// A standby on another Mac beside the primary pump of v0.6.0, whose line is at GREEN and whose
/// claim's holder (the primary's release child) is dead with its lease run out.
fn standby_lab() -> Lab {
    let mut lab = Lab::opened();
    lab.adopt = true;
    lab.main.line = Some(GREEN.into());
    lab.main.holder = Some("m16 (node 7c1e, session s-1, pid 41)".into());
    lab.main.elsewhere = true;
    lab.main.lease_until = Some(lab.now - 1);
    lab.main.lease_ended = true;
    lab.holder_pump = pump_there(Some(true), true, 30);
    lab
}

/// ONE UNREADABLE LOOK TAKES NO LINE FROM A LIVE PUMP (UB-15 finding 3). Nothing renews a claim's
/// lease between the primary pump's runs, so a held red, a backoff or PREPARED outlives it; once
/// it has run out the standby judges the pump alone. A pump it cannot read (a three-second survey
/// timeout on a loaded Mac, a moment's network, a Mac asleep) is waited out until it has been
/// unreadable at every look for `PUMP_STALE_SECS`, what a readable pump's fact may age before it
/// is stale; one live read between starts that count again.
/// NEGATIVE CONTROL: the pump unreadable at every look for the whole bound is taken over.
#[test]
fn one_unreadable_look_takes_no_line_from_a_live_pump() {
    let mut lab = standby_lab();
    let (mut memory, mut said) = (Memory::default(), String::new());
    // Eight hours of a live primary, one read in twenty failing.
    for look in 0..480u64 {
        lab.holder_pump = if look % 20 == 19 {
            Some(Err("it did not answer: timed out".into()))
        } else {
            pump_there(Some(true), true, 30)
        };
        lab.now += cycle(&mut lab, &mut memory, &mut said);
    }
    assert!(lab.ran.is_empty(), "{:?}", lab.ran);
    assert!(
        lab.said.iter().any(|line| line.contains("cannot be read")
            && line.contains("has been unreadable for")),
        "{:#?}",
        lab.said
    );
    // NEGATIVE CONTROL: unreadable at every look for the whole bound, the line is taken over.
    lab.holder_pump = Some(Err("it did not answer: timed out".into()));
    let first = lab.now;
    while lab.ran.is_empty() {
        lab.now += cycle(&mut lab, &mut memory, &mut said);
        assert!(lab.now < first + 2 * PUMP_STALE_SECS, "{:#?}", lab.said);
    }
    assert_eq!(lab.ran, [Verb::Adopt]);
    assert!(lab.now >= first + PUMP_STALE_SECS);
}

/// A STANDBY WAITS ON A LEASE ONLY AS LONG AS THE CLAIM'S OWN RULE HOLDS IT (UB-15 finding 10). A
/// claim renewed further ahead of this clock than a whole lease frees the line for any contender
/// (`expired`), though its `lease_until` is still that far ahead: the standby that cannot read
/// the holder's pump judges the lease by the same rule, never waiting out a fast clock's skew.
/// NEGATIVE CONTROL: the same lease read as running holds the line a day.
#[test]
fn a_standby_waits_on_a_lease_only_as_long_as_the_claims_rule_holds_it() {
    let mut lab = standby_lab();
    lab.holder_pump = Some(Err("no route to host".into()));
    lab.main.lease_until = Some(lab.now + 86_400);
    let memory = Memory {
        pump_unread: Some(PumpUnread {
            line: format!("v0.6.0@{}", &GREEN[..9]),
            since: lab.now - PUMP_STALE_SECS,
            last: lab.now - POLL_SECS,
        }),
        ..Memory::default()
    };
    assert_eq!(
        runs(&decide(&lab.seen_with(&memory), &memory)),
        Some(&Verb::Adopt)
    );
    // NEGATIVE CONTROL: a lease that has not ended holds the line until its end.
    lab.main.lease_ended = false;
    let held = decide(&lab.seen_with(&memory), &memory);
    assert!(waits(&held).contains("cannot be read"), "{held:?}");
    let Decision::Wait { note, .. } = held else {
        unreachable!()
    };
    assert!(note.contains("86400 s from now"), "{note}");
}

/// A RESTARTING PUMP KEEPS ITS LINE AND SAYS SO BEFORE ITS FIRST LOOK (UB-15 findings 5 and 15).
/// launchd restarts a pump that died, and `--pump --install` replaces it; between the two the
/// record names the dead process. A dead pump that held the line's journal keeps it for
/// `RESTART_GRACE_SECS` after its last look, and a starting pump publishes what its journal holds
/// before its first look reads origin, so its record never names its dead predecessor for long.
/// NEGATIVE CONTROLS: past the grace the dead pump is gone and the standby adopts; a starting pump
/// with no journal says it holds nothing.
#[test]
fn a_restarting_pump_keeps_its_line_and_says_so_before_its_first_look() {
    let mut lab = standby_lab();
    lab.holder_pump = pump_there(Some(false), true, 30);
    let (mut memory, mut said) = (Memory::default(), String::new());
    lab.now += cycle(&mut lab, &mut memory, &mut said);
    assert!(lab.ran.is_empty());
    assert!(
        lab.said.last().unwrap().contains("it may be restarting"),
        "{:#?}",
        lab.said
    );
    // NEGATIVE CONTROL: past the grace the dead pump is gone.
    lab.holder_pump = pump_there(Some(false), true, RESTART_GRACE_SECS + 1);
    cycle(&mut lab, &mut memory, &mut said);
    assert_eq!(lab.ran, [Verb::Adopt]);

    // The starting pump says what its journal holds before its first read of main.
    let mut lab = Lab::opened();
    lab.line(OPENED).prepared = true;
    pump(&mut lab, Memory::default(), Some(1));
    let started = &lab.facts[0];
    assert!(
        started.decision.contains("pump: started"),
        "{:#?}",
        lab.facts
    );
    assert_eq!(
        lab.facts_after_reads[0], 0,
        "published before main was read"
    );
    assert!(started.own, "{started:?}");
    assert_eq!(started.line, format!("v0.6.0@{}", &OPENED[..9]));
    let mut bare = Lab::opened();
    bare.main_down = true;
    pump(&mut bare, Memory::default(), Some(1));
    assert!(!bare.facts[0].own && bare.facts[0].line.is_empty());
}

/// A PUMP THAT CANNOT READ ORIGIN KEEPS SAYING IT HOLDS ITS LINE (UB-15 findings 1 and 11). A
/// cycle whose read of main fails publishes the line and whether it is this clone's from the
/// journal's own files, never through origin (`Host::journal` asks origin for the version's tag),
/// so a primary whose GitHub reads fail while its LAN works is still read as live by the standby.
/// NEGATIVE CONTROL: the clone with no journal says it holds nothing, and the standby reading that
/// fact finds no live pump.
#[test]
fn a_pump_that_cannot_read_origin_keeps_saying_it_holds_its_line() {
    let mut lab = Lab::opened();
    lab.line(OPENED).prepared = true;
    let (mut memory, mut said) = (Memory::default(), String::new());
    lab.now += cycle(&mut lab, &mut memory, &mut said);
    lab.main_down = true;
    lab.journal_down = true;
    assert_eq!(cycle(&mut lab, &mut memory, &mut said), POLL_SECS);
    let unread = lab.facts.last().unwrap().clone();
    assert!(
        unread.decision.contains("cannot read the world"),
        "{unread:?}"
    );
    assert_eq!(
        (unread.own, unread.line.as_str(), unread.step.as_str()),
        (true, format!("v0.6.0@{}", &OPENED[..9]).as_str(), "wait")
    );
    let read = |fact: &Fact| PumpRow {
        alive: Some(true),
        line: fact.line.clone(),
        step: fact.step.clone(),
        own: Some(fact.own),
        age: 30,
    };
    assert_eq!(
        holder_pump(Ok(Some(vec![read(&unread)])), "0.6.0", OPENED),
        HolderPump::Live { age: 30 },
        "the standby reads the primary as live"
    );
    // NEGATIVE CONTROL: with no journal the same cycle says it holds nothing.
    let mut bare = Lab::opened();
    bare.main_down = true;
    bare.journal_down = true;
    cycle(&mut bare, &mut Memory::default(), &mut String::new());
    let nothing = bare.facts.last().unwrap();
    assert!(!nothing.own && nothing.line.is_empty(), "{nothing:?}");
    assert!(matches!(
        holder_pump(Ok(Some(vec![read(nothing)])), "0.6.0", OPENED),
        HolderPump::Gone(_)
    ));
}

/// A RUN THAT LEAVES A JOURNAL SAYS SO FROM ITS START TO ITS END (UB-15 finding 2). A start or an
/// adoption leaves this clone a journal of the line, so the fact published as it runs, which its
/// attended run refreshes for hours, says the line is this pump's; the moment it ends the pump
/// says what the journal it left holds, before its slow reads of origin, so a standby that reads
/// the pump once its child is dead never finds a fresh fact that says it holds nothing.
/// NEGATIVE CONTROL: a start that went red before it journaled anything leaves a fact that holds
/// nothing once it ends.
#[test]
fn a_run_that_leaves_a_journal_says_so_from_its_start_to_its_end() {
    let mut lab = Lab::opened();
    lab.start_journals = true;
    lab.exits = vec![1];
    lab.caller = Some("ab12".into());
    let (mut memory, mut said) = (Memory::default(), String::new());
    assert_eq!(cycle(&mut lab, &mut memory, &mut said), 0);
    lab.now += cycle(&mut lab, &mut memory, &mut said);
    assert_eq!(lab.ran, [Verb::Start(OPENED.into())]);
    let [started, ended, held] = &lab.facts[..] else {
        panic!("{:#?}", lab.facts);
    };
    assert_eq!(
        (started.step.as_str(), started.line.as_str(), started.own),
        ("start", format!("v0.6.0@{}", &OPENED[..9]).as_str(), true)
    );
    assert_eq!((ended.step.as_str(), ended.own), ("wait", true));
    assert!(ended.decision.contains("pump: exit 1"), "{ended:?}");
    assert!(held.own && held.decision.contains("is red"), "{held:?}");

    // A standby's adoption holds the line from its start too.
    let mut lab = standby_lab();
    lab.holder_pump = pump_there(Some(false), true, RESTART_GRACE_SECS + 1);
    let (mut memory, mut said) = (Memory::default(), String::new());
    assert_eq!(cycle(&mut lab, &mut memory, &mut said), 0);
    assert_eq!(lab.ran, [Verb::Adopt]);
    let [adopting, adopted] = &lab.facts[..] else {
        panic!("{:#?}", lab.facts);
    };
    assert!(adopting.own && adopting.step == "adopt", "{adopting:?}");
    assert!(adopted.own, "{adopted:?}");

    // NEGATIVE CONTROL: a start refused before it journaled anything leaves a fact of nothing.
    let mut lab = Lab::opened();
    lab.exits = vec![2];
    cycle(&mut lab, &mut Memory::default(), &mut String::new());
    let [_, ended] = &lab.facts[..] else {
        panic!("{:#?}", lab.facts);
    };
    assert!(!ended.own, "{ended:?}");
}

/// A LINE ANOTHER MAC'S PUMP TOOK IS THAT PUMP'S (UB-15 findings 4 and 12). A standby adopts the
/// line of a primary that slept, was cut off or died; the adoption resumes the same line with no
/// hop. The primary that comes back still holds its journal, its red and its memory, and between
/// the standby's runs the claim keeps no contender off: it asks the claim's holder's Mac after its
/// pump, and while that pump holds the line's journal (live, or restarting) it runs nothing of the
/// line, no prepare, fix, re-base, supersession or publish, and says the line is not its own. A
/// pump there it cannot read is waited out as a standby waits one out.
/// NEGATIVE CONTROLS: once that pump is gone the line re-bases here; a claim held on this Mac asks
/// no other Mac at all.
#[test]
fn a_line_another_macs_pump_took_is_that_pumps() {
    let returned = || {
        let mut lab = Lab::opened();
        lab.line(OPENED);
        lab.exits = vec![1];
        lab.caller = Some("ab12".into());
        let (mut memory, mut said) = (Memory::default(), String::new());
        cycle(&mut lab, &mut memory, &mut said);
        lab.greens.insert(OPENED.into(), GREEN.into());
        // The standby on m9 adopted the line at the same head, with no hop.
        lab.main.holder = Some("m9 (node 0eda, session s-9, pid 7)".into());
        lab.main.elsewhere = true;
        lab.main.lease_until = Some(lab.now + 1_800);
        lab.holder_pump = Some(Ok(Some(vec![PumpRow {
            alive: Some(true),
            line: format!("v0.6.0@{}", &OPENED[..9]),
            step: "wait".into(),
            own: Some(true),
            age: 20,
        }])));
        (lab, memory, said)
    };
    let (mut lab, mut memory, mut said) = returned();
    let asked = lab.holder_pump_asked;
    for _ in 0..30 {
        lab.now += cycle(&mut lab, &mut memory, &mut said);
    }
    assert_eq!(lab.ran, [Verb::Prepare], "no re-base beside m9's pump");
    assert!(lab.holder_pump_asked > asked);
    assert!(
        lab.said
            .last()
            .unwrap()
            .contains("was taken by m9 (node 0eda, session s-9, pid 7), no run of this pump"),
        "{:#?}",
        lab.said
    );
    assert!(!lab.facts.last().unwrap().own, "{:?}", lab.facts.last());
    // Neither a worker's accepted fix, a resume, a publish nor an undecided supersession runs.
    lab.report("0.6.0", OPENED, "s-1", Report::Fix(FIX.into()));
    assert!(waits(&decide(&lab.seen_with(&memory), &memory)).contains("was taken by m9"));
    lab.greens.clear();
    assert!(waits(&decide(&lab.seen_with(&memory), &memory)).contains("was taken by m9"));
    let fresh = Memory::default();
    lab.repairs.clear();
    assert!(waits(&decide(&lab.seen_with(&fresh), &fresh)).contains("was taken by m9"));
    lab.publishes = true;
    lab.journal.as_mut().unwrap().prepared = true;
    assert!(waits(&decide(&lab.seen_with(&fresh), &fresh)).contains("was taken by m9"));
    lab.journal.as_mut().unwrap().undecided = Some(Undecided {
        from: OPENED.into(),
        onto: GREEN.into(),
    });
    assert!(waits(&decide(&lab.seen_with(&fresh), &fresh)).contains("was taken by m9"));
    // A restarting pump there keeps it; one there that cannot be read is waited out.
    let (mut lab, memory, _) = returned();
    lab.holder_pump = Some(Ok(Some(vec![PumpRow {
        alive: Some(false),
        line: format!("v0.6.0@{}", &OPENED[..9]),
        step: "wait".into(),
        own: Some(true),
        age: 20,
    }])));
    assert!(waits(&decide(&lab.seen_with(&memory), &memory)).contains("may be restarting"));
    lab.holder_pump = Some(Err("it did not answer: timed out".into()));
    assert!(waits(&decide(&lab.seen_with(&memory), &memory)).contains("cannot be read"));

    // NEGATIVE CONTROL: once that pump is gone the line re-bases here.
    let (mut lab, mut memory, mut said) = returned();
    lab.holder_pump = None;
    assert_eq!(cycle(&mut lab, &mut memory, &mut said), 0);
    assert_eq!(lab.ran, [Verb::Prepare, rebase_for_red(GREEN, "ab12")]);
    // NEGATIVE CONTROL: a claim held on this Mac asks no other Mac.
    let (mut lab, mut memory, mut said) = returned();
    lab.main.elsewhere = false;
    let asked = lab.holder_pump_asked;
    assert_eq!(cycle(&mut lab, &mut memory, &mut said), 0);
    assert_eq!(lab.holder_pump_asked, asked);
    assert_eq!(lab.ran.last(), Some(&rebase_for_red(GREEN, "ab12")));
}

/// THE PUMP'S OWN UNDECIDED RE-BASE IS ITS OWN THOUGH THE READ AFTER IT FAILED (UB-15 finding 13).
/// A re-base whose supersession write has an unknown result (exit 3) leaves candidate.toml
/// undecided, and that result comes from origin not answering, which the read of the journal
/// right after the run needs too: the pump knows its own supersession by the run itself, a
/// re-base of the head it retires onto the same commit that decided nothing, and runs the same
/// command again by the backoff of its exit.
/// NEGATIVE CONTROL: the same supersession after a refused re-base (exit 1) is a session's.
#[test]
fn the_pumps_own_undecided_rebase_is_its_own_though_the_read_after_it_failed() {
    let stopped = |exit: i32| {
        let mut lab = Lab::opened();
        lab.line(OPENED);
        lab.report("0.6.0", OPENED, "s-1", Report::Fix(FIX.into()));
        lab.greens.insert(OPENED.into(), GREEN.into());
        lab.stops_undecided = true;
        lab.journal_down_after_run = true;
        lab.exits = vec![exit];
        lab
    };
    let mut lab = stopped(3);
    let (mut memory, mut said) = (Memory::default(), String::new());
    assert_eq!(cycle(&mut lab, &mut memory, &mut said), 0);
    assert!(lab.journal.as_ref().unwrap().undecided.is_some());
    assert_eq!(
        memory.last.as_ref().and_then(|last| last.journal.clone()),
        Some(Journal {
            version: "0.6.0".into(),
            head: OPENED.into(),
            ..Journal::default()
        }),
        "the read after the run failed: the journal kept is the one before it"
    );
    lab.journal_down_after_run = false;
    lab.now += 2 * POLL_SECS;
    assert_eq!(cycle(&mut lab, &mut memory, &mut said), 0);
    assert_eq!(lab.ran.len(), 2, "{:#?}", lab.said);
    assert_eq!(lab.ran[0], lab.ran[1], "the same re-base, again");

    // NEGATIVE CONTROL: after a refused re-base the same undecided supersession is a session's.
    let mut lab = stopped(1);
    let (mut memory, mut said) = (Memory::default(), String::new());
    cycle(&mut lab, &mut memory, &mut said);
    let journal = lab.journal.as_mut().unwrap();
    journal.undecided = Some(Undecided {
        from: OPENED.into(),
        onto: GREEN.into(),
    });
    journal.head = GREEN.into();
    lab.now += 10 * REFUSED_CAP_SECS;
    assert_eq!(cycle(&mut lab, &mut memory, &mut said), POLL_SECS);
    assert_eq!(lab.ran.len(), 1);
    assert!(
        lab.said.last().unwrap().contains("a session's to decide"),
        "{:#?}",
        lab.said
    );
}

/// NOTHING MOVES A LINE ANOTHER PROCESS HOLDS LIVE (UB-15). The pump's re-base takes the claim
/// only from a holder a contender could take it from, so beside a live holder it would only end
/// beside it, as a hop's run would: the pump re-bases only a line no other process holds live
/// (the record keeps a contender off, and the holder is no run of this clone) and only while the
/// fleet line is this journal's, at its head. Beside a live holder a line that must
/// re-qualify (its own red, a worker's accepted fix) runs nothing, neither the re-base nor the
/// hop, and its wait names the holder, said once however often the holder renews.
/// NEGATIVE CONTROLS: once the record keeps no contender off the line re-bases; a fleet line
/// another holder moved past this journal's head is not re-based (its red holds).
#[test]
fn a_line_held_live_elsewhere_is_neither_rebased_nor_hopped() {
    let mut lab = Lab::opened();
    lab.line(OPENED);
    lab.exits = vec![1];
    lab.caller = Some("ab12".into());
    let (mut memory, mut said) = (Memory::default(), String::new());
    assert_eq!(cycle(&mut lab, &mut memory, &mut said), 0);
    // A pass is judged past the red head, and meanwhile another Mac's cutter took the line (its
    // run re-qualifies the same head with a live, renewed lease).
    lab.greens.insert(OPENED.into(), GREEN.into());
    lab.main.holder = Some("m9 (node 0eda, session s-9, pid 7)".into());
    lab.main.held = Some(held(lab.now + 1_800));
    let before = lab.said.len();
    for _ in 0..60 {
        lab.now += cycle(&mut lab, &mut memory, &mut said);
        lab.main.held.as_mut().unwrap().until += 300;
    }
    assert_eq!(lab.ran, [Verb::Prepare], "nothing ran beside the holder");
    assert_eq!(lab.said.len(), before + 1, "said once: {:#?}", lab.said);
    assert!(
        lab.said[before].contains("is held now by m9") && lab.said[before].contains("its lease"),
        "{:#?}",
        lab.said
    );
    assert!(memory.rebased.is_none(), "its re-base is unspent");
    // A worker's accepted fix of the same head is not carried beside the holder either.
    lab.report("0.6.0", OPENED, "s-1", Report::Fix(FIX.into()));
    let fix = decide(&lab.seen_with(&memory), &memory);
    assert!(waits(&fix).contains("is held now by m9"), "{fix:?}");
    lab.greens.clear();
    let hop = decide(&lab.seen_with(&memory), &memory);
    assert!(waits(&hop).contains("is held now by m9"), "{hop:?}");
    lab.greens.insert(OPENED.into(), GREEN.into());
    // NEGATIVE CONTROL: the record keeps no contender off: the line re-bases.
    lab.main.held = None;
    assert_eq!(cycle(&mut lab, &mut memory, &mut said), 0);
    assert_eq!(
        lab.ran,
        [
            Verb::Prepare,
            Verb::Rebase {
                onto: GREEN.into(),
                requalify: Requalify::Fix(FIX.into())
            }
        ]
    );

    // NEGATIVE CONTROL: the fleet line another holder moved past this head is not retired here.
    let mut lab = Lab::opened();
    lab.line(OPENED);
    lab.exits = vec![1];
    lab.caller = Some("ab12".into());
    let (mut memory, mut said) = (Memory::default(), String::new());
    cycle(&mut lab, &mut memory, &mut said);
    lab.greens.insert(OPENED.into(), GREEN.into());
    lab.main.line = Some(HOP.into());
    assert_eq!(cycle(&mut lab, &mut memory, &mut said), POLL_SECS);
    assert_eq!(lab.ran, [Verb::Prepare]);
    assert!(
        lab.said.last().unwrap().contains("is red"),
        "{:#?}",
        lab.said
    );
}

/// AN OLDER PUMP'S WORKER RE-BASES NOTHING ON A PUBLISH REFUSAL THAT IS NO RED (UB-15). A worker
/// an older pump kept for this head, from before the prepare passed, is the one on the line again
/// once the pump's own publish of it ends exit 1; its NO-FIX stops the line, and the line
/// re-bases only where that refused publish was itself a red of this head (the pump holds it,
/// `Red::holds`). A refusal that is no red (a notary's, a signing step's) leaves the line stopped
/// for a session, its re-base unspent.
/// NEGATIVE CONTROL: the same publish ending at a caller-owned red re-bases.
#[test]
fn an_older_workers_no_fix_rebases_only_after_a_red_publish() {
    let lab = |caller: Option<&str>| {
        let mut lab = Lab::opened();
        lab.publishes = true;
        lab.report("0.6.0", OPENED, "s-1", Report::NoFix("this machine".into()));
        let journal = lab.line(OPENED);
        journal.prepared = true;
        journal.prepared_at = 1_050;
        lab.greens.insert(OPENED.into(), GREEN.into());
        lab.exits = vec![1];
        lab.caller = caller.map(str::to_string);
        lab
    };
    let mut refused = lab(None);
    let (mut memory, mut said) = (Memory::default(), String::new());
    assert_eq!(cycle(&mut refused, &mut memory, &mut said), 0);
    assert_eq!(refused.ran, [Verb::Finish]);
    refused.main.tip = MOVED.into();
    refused.now += 10 * REFUSED_CAP_SECS;
    assert_eq!(cycle(&mut refused, &mut memory, &mut said), POLL_SECS);
    assert_eq!(refused.ran, [Verb::Finish], "{:#?}", refused.said);
    assert!(
        refused.said.last().unwrap().contains("is stopped"),
        "{:#?}",
        refused.said
    );
    assert_eq!(memory.rebased, None);

    let mut red = lab(Some("ab12"));
    let (mut memory, mut said) = (Memory::default(), String::new());
    cycle(&mut red, &mut memory, &mut said);
    assert_eq!(cycle(&mut red, &mut memory, &mut said), 0);
    assert_eq!(
        red.ran,
        [
            Verb::Finish,
            Verb::Rebase {
                onto: GREEN.into(),
                requalify: Requalify::Red("repair worker s-1 has no fix".into())
            }
        ]
    );
}

/// A SUPERSESSION LEFT UNDECIDED HOLDS THE LINE. The pump's own re-base, stopped after it
/// journaled its supersession and before its write was decided (candidate.toml holds the very
/// supersession its run left), is decided by the same command, by the backoff of the exit it
/// ended with. Any other undecided supersession is a session's: one a session ran with the
/// command the pump named after its own re-base was spent included. Beside a live holder of the
/// line even the pump's own runs nothing, and once the fleet line is at neither the head the
/// supersession retires nor its replacement a session decides it.
/// NEGATIVE CONTROLS: with the last run another verb, the same journal runs nothing; the pump's
/// spent re-base onto the same commit does not make a session's supersession its own.
#[test]
fn an_undecided_rebase_is_decided_by_the_same_command_and_a_sessions_is_held() {
    let mut lab = Lab::opened();
    lab.line(OPENED);
    lab.report("0.6.0", OPENED, "s-1", Report::Fix(FIX.into()));
    lab.greens.insert(OPENED.into(), GREEN.into());
    lab.stops_undecided = true;
    lab.exits = vec![130];
    let (mut memory, mut said) = (Memory::default(), String::new());
    cycle(&mut lab, &mut memory, &mut said);
    let pending = Undecided {
        from: OPENED.into(),
        onto: GREEN.into(),
    };
    assert_eq!(
        lab.journal.as_ref().unwrap().undecided,
        Some(pending.clone())
    );
    assert_eq!(memory.rebased, None, "it decided nothing");
    lab.now += POLL_SECS;
    // Beside a live holder of the line even its own supersession runs nothing.
    lab.main.holder = Some("m9 (node 0eda, session s-9, pid 7)".into());
    lab.main.held = Some(held(lab.now + 1_800));
    let beside = decide(&lab.seen_with(&memory), &memory);
    assert!(waits(&beside).contains("is held now by m9"), "{beside:?}");
    lab.main.held = None;
    // Once the fleet line is at neither head, another holder moved it: a session decides it.
    lab.main.line = Some(LATER.into());
    let moved = decide(&lab.seen_with(&memory), &memory);
    assert!(
        waits(&moved).contains("another holder moved it"),
        "{moved:?}"
    );
    lab.main.line = Some(OPENED.into());
    assert_eq!(cycle(&mut lab, &mut memory, &mut said), 0);
    assert_eq!(lab.ran.len(), 2);
    assert_eq!(lab.ran[0], lab.ran[1], "the same re-base, again");

    // The pump's re-base is refused before it records anything (its supersession is restored,
    // and the re-base spent); a session then runs the command the pump named, in this clone, and
    // that run stops undecided. It is the session's to decide.
    let mut lab = Lab::opened();
    lab.line(OPENED);
    lab.report("0.6.0", OPENED, "s-1", Report::Fix(FIX.into()));
    lab.greens.insert(OPENED.into(), GREEN.into());
    lab.exits = vec![1];
    let (mut memory, mut said) = (Memory::default(), String::new());
    cycle(&mut lab, &mut memory, &mut said);
    assert_eq!(memory.rebased.as_ref().map(|kept| kept.code), Some(1));
    let journal = lab.journal.as_mut().unwrap();
    journal.undecided = Some(pending);
    journal.head = GREEN.into();
    lab.main.tip = MOVED.into();
    lab.now += 10 * REFUSED_CAP_SECS;
    let sessions = decide(&lab.seen_with(&memory), &memory);
    assert!(
        waits(&sessions).contains("which is a session's to decide: ship release --commit"),
        "{sessions:?}"
    );
    assert_eq!(cycle(&mut lab, &mut memory, &mut said), POLL_SECS);
    assert_eq!(lab.ran.len(), 1, "{:?}", lab.ran);

    let mut lab = Lab::opened();
    lab.line(GREEN).undecided = Some(Undecided {
        from: OPENED.into(),
        onto: GREEN.into(),
    });
    let memory = Memory {
        last: Some(Attempt {
            verb: Verb::Prepare,
            key: format!("prepare 0.6.0 {OPENED}"),
            code: 0,
            last: "PREPARED".into(),
            at: 0,
            tip: OPENED.into(),
            journal: None,
            repeats: 0,
        }),
        ..Memory::default()
    };
    let held = decide(&lab.seen_with(&memory), &memory);
    assert!(
        waits(&held).contains("which is a session's to decide: ship release --commit"),
        "{held:?}"
    );
}

/// THE PUMP PUBLISHES ITS LINE, STEP AND DECISION EACH CYCLE (UB-15, `pump=`). Each cycle's fact
/// names the line it reads and who holds it, what it does (the verb's word or `wait`), the
/// cycle's time and its reason; a run's end is said at once; a world that does not read is still
/// a cycle, so a peer tells a live pump by the age of its newest fact.
#[test]
fn the_pump_publishes_its_line_step_and_decision_each_cycle() {
    let mut lab = Lab::opened();
    lab.main.holder = Some("m16 (node 7c1e)".into());
    let (mut memory, mut said) = (Memory::default(), String::new());
    cycle(&mut lab, &mut memory, &mut said);
    lab.now += cycle(&mut lab, &mut memory, &mut said);
    lab.main_down = true;
    cycle(&mut lab, &mut memory, &mut said);
    let [start, ended, wait, unread] = &lab.facts[..] else {
        panic!("one fact a cycle and one at a run's end: {:#?}", lab.facts);
    };
    assert_eq!(
        (
            start.step.as_str(),
            start.line.as_str(),
            start.holder.as_str()
        ),
        (
            "start",
            format!("v0.6.0@{}", &OPENED[..9]).as_str(),
            "m16 (node 7c1e)"
        )
    );
    assert!(start.decision.contains("no fleet line"), "{start:?}");
    assert_eq!((ended.step.as_str(), ended.own), ("wait", true));
    assert!(ended.decision.contains("pump: exit 0"), "{ended:?}");
    assert_eq!(wait.step, "wait");
    assert_eq!(wait.line, format!("v0.6.0@{}", &OPENED[..9]));
    assert!(wait.decision.contains("a session publishes it"), "{wait:?}");
    assert!(ended.at > start.at && wait.at >= ended.at && unread.at >= wait.at);
    assert!(
        unread.decision.contains("cannot read the world"),
        "{unread:?}"
    );
}

/// The abstract `ReleasePumpRebase` state of the pump's line as `lab` and `memory` read now,
/// with the history ghosts the walk counts.
fn rebase_state(
    lab: &mut Lab,
    memory: &Memory,
    (rebases, unjudged, hops, displaced): (i64, i64, i64, i64),
) -> aterm_spec::interp::State {
    let seen = lab.seen_with(memory);
    let journal = seen
        .journal
        .clone()
        .expect("the walk is of this clone's line");
    // The worker that governs the line is the one the pump's own decision reads (`red_worker`):
    // an older pump's worker counts only after the pump's own publish of the head was refused,
    // and makes the line re-qualify only where that refusal was a red.
    let worker = red_worker(&seen, memory, &journal);
    let phase = if journal.finished {
        3
    } else {
        match worker {
            Some((
                Repair {
                    report: Report::Fix(_),
                    admitted: Some(Ok(())),
                    ..
                },
                _,
            )) => 2,
            Some(_) => 1,
            None if memory.red.as_ref().is_some_and(|red| red.holds(&journal)) => 1,
            None => 0,
        }
    };
    let nored = worker.is_some_and(|(_, requalifies)| !requalifies);
    let green = seen.green.is_some() && !matches!(seen.onto, Some(Err(_)));
    // The fleet line is not at this clone's head: another holder moved it, or this clone's own
    // hop is recorded nowhere the claim reads yet.
    let moved = !journal.finished && seen.main.line.as_deref() != Some(journal.head.as_str());
    aterm_spec::interp::State::from([
        ("phase", phase),
        ("green", i64::from(green)),
        ("rebased", i64::from(rebased(memory, &journal))),
        ("rebases", rebases),
        ("unjudged", unjudged),
        ("hops", hops),
        ("held", i64::from(seen.main.held.is_some())),
        ("displaced", displaced),
        ("nored", i64::from(nored)),
        ("moved", i64::from(moved)),
    ])
}

/// A walk of the real pump against `ReleasePumpRebase`: the world, the pump's memory, the abstract
/// state the last step left, and the history ghosts (re-bases made, one onto no judged pass, hops,
/// one beside a live holder).
struct Walk {
    model: aterm_spec::derive::Model,
    lab: Lab,
    memory: Memory,
    said: String,
    state: aterm_spec::interp::State,
    ghosts: (i64, i64, i64, i64),
}

impl Walk {
    /// A walk from `lab`, whose abstract state must be the model's initial one.
    fn new(mut lab: Lab) -> Self {
        let model = aterm_models_release::release_pump_rebase_model();
        let memory = Memory::default();
        let state = rebase_state(&mut lab, &memory, (0, 0, 0, 0));
        assert_eq!(state, model.init_state());
        Self {
            model,
            lab,
            memory,
            said: String::new(),
            state,
            ghosts: (0, 0, 0, 0),
        }
    }

    /// One step: `act` changes the world, or runs a cycle and names the verb it ran. The abstract
    /// state it leaves is a transition the model admits by `action`, and every law holds there.
    fn step(
        &mut self,
        action: &str,
        act: &dyn Fn(&mut Lab, &mut Memory, &mut String) -> Option<Verb>,
    ) {
        let before = self.lab.seen_with(&self.memory);
        if let Some(verb) = act(&mut self.lab, &mut self.memory, &mut self.said) {
            match verb {
                // A re-base counts once it spent the line's re-base (`Memory::rebased`).
                Verb::Rebase { onto, .. } if self.memory.rebased.is_some() => {
                    self.ghosts.0 += 1;
                    self.ghosts.1 =
                        i64::from(Some(&onto) != before.green.as_ref() || self.ghosts.1 == 1);
                    self.ghosts.3 = i64::from(before.main.held.is_some() || self.ghosts.3 == 1);
                }
                Verb::Fix(_) => self.ghosts.2 += 1,
                _ => {}
            }
        }
        let next = rebase_state(&mut self.lab, &self.memory, self.ghosts);
        let (admitted, why) = aterm_spec::verify::validate_transition_tiered(
            &self.model,
            &[],
            &self.state,
            &next,
            Some(action),
            "ReleasePumpRebase conformance",
        );
        assert!(admitted, "{action}: {:?} -> {next:?}\n{why}", self.state);
        for law in [
            "AtMostOneRebasePerLine",
            "RebaseOnlyOntoAJudgedPass",
            "NoRebaseBesideALiveHolder",
        ] {
            assert!(self.model.check_invariant(law, &next), "{law} at {next:?}");
        }
        self.state = next;
    }

    fn enabled(&self, action: &str) -> bool {
        self.model.action_enabled(action, &self.state)
    }
}

/// One cycle of the pump, and the verb it ran.
fn run(lab: &mut Lab, memory: &mut Memory, said: &mut String) -> Option<Verb> {
    let ran = lab.ran.len();
    cycle(lab, memory, said);
    lab.ran.get(ran).cloned()
}

/// THE PUMP'S RE-BASE CONFORMS TO `ReleasePumpRebase` (crates/aterm-models-release). The real
/// pump is walked through a line's life: a red, a pass judged past it, another process taking the
/// line live and leaving it, the re-base, a red of the new head with a newer pass, a worker's
/// fix, the hop and the release. Each step's abstract state is a transition the model admits by
/// the action that names it, the model's `Rebase` is enabled exactly where the pump re-bases, and
/// every law holds at every step. Three more walks cover where the pump re-bases nothing at a
/// re-qualify: a re-base the release refuses (it spends the line's re-base, and the worker's fix
/// then goes as a hop), a line stopped by a publish refusal that is no red with an older pump's
/// worker on it (no re-base is due, and that worker's fix is carried as a hop though a judged pass
/// could take the line), and a fleet line another holder moved and left (no re-base is due, as the
/// model's `Move` disables it, and the fix is carried as a hop).
/// NEGATIVE CONTROLS: a pump that forgets its re-base (main and memory say none) re-bases the
/// line again at the later red and reaches a state AtMostOneRebasePerLine refuses; the model's
/// RebaseTwice, RebaseOntoUnjudged and RebaseBesideAHolder mutants each break their law.
#[test]
fn release_pump_rebase_conforms() {
    use aterm_spec::interp;
    let mut lab = Lab::opened();
    lab.publishes = true;
    lab.line(OPENED);
    lab.exits = vec![1, 0, 1, 0, 0];
    lab.caller = Some("ab12".into());
    let mut walk = Walk::new(lab);
    // The prepare goes red.
    walk.step("GoRed", &run);
    // The follower judges a pass past its head, and the pump re-bases onto it.
    walk.step("Judge", &|lab, _, _| {
        lab.greens.insert(OPENED.into(), GREEN.into());
        None
    });
    // Another process takes the line live: the pump runs nothing beside it, as the model has
    // no re-base for it; once its lease ends unrenewed the re-base is due.
    walk.step("Take", &|lab, _, _| {
        lab.main.held = Some(held(lab.now + 1_800));
        None
    });
    assert!(!walk.enabled("Rebase"));
    let ran = walk.lab.ran.len();
    cycle(&mut walk.lab, &mut walk.memory, &mut walk.said);
    assert_eq!(walk.lab.ran.len(), ran, "nothing runs beside the holder");
    walk.step("Leave", &|lab, _, _| {
        lab.main.held = None;
        None
    });
    assert!(walk.enabled("Rebase"));
    walk.step("Rebase", &run);
    // The publish of the new head goes red, and a newer pass is judged past it.
    walk.step("GoRed", &run);
    walk.step("Judge", &|lab, _, _| {
        lab.greens.insert(GREEN.into(), LATER.into());
        None
    });
    // The pump holds, as the model has no re-base for it.
    assert!(!walk.enabled("Rebase"));
    assert_eq!(
        runs(&decide(&walk.lab.seen_with(&walk.memory), &walk.memory)),
        None
    );
    let forgetful = (walk.lab.journal.clone(), walk.memory.clone());
    // A worker's fix is accepted and carried as a hop; the line releases.
    walk.step("AcceptFix", &|lab, _, _| {
        lab.report("0.6.0", GREEN, "s-2", Report::Fix(FIX2.into()));
        None
    });
    walk.step("Hop", &run);
    walk.step("Release", &run);
    assert_eq!(
        walk.lab.ran,
        [
            Verb::Prepare,
            rebase_for_red(GREEN, "ab12"),
            Verb::Finish,
            Verb::Fix(FIX2.into()),
            Verb::Finish
        ]
    );

    // A re-base the release refuses spends the line's re-base, and leaves the line as it was:
    // the worker's fix then goes as a hop, though the judged pass still could take the line.
    let mut lab = Lab::opened();
    lab.publishes = true;
    lab.line(OPENED);
    lab.exits = vec![1, 1, 0, 0];
    lab.caller = Some("ab12".into());
    let mut refused = Walk::new(lab);
    refused.step("GoRed", &run);
    refused.step("Judge", &|lab, _, _| {
        lab.greens.insert(OPENED.into(), GREEN.into());
        None
    });
    assert!(refused.enabled("Rebase") && refused.enabled("RebaseRefused"));
    refused.step("RebaseRefused", &run);
    assert_eq!(
        refused.memory.rebased.as_ref().map(|kept| kept.code),
        Some(1)
    );
    refused.step("AcceptFix", &|lab, _, _| {
        lab.report("0.6.0", OPENED, "s-1", Report::Fix(FIX.into()));
        None
    });
    assert!(!refused.enabled("Rebase"));
    refused.step("Hop", &run);
    refused.step("Release", &run);
    assert_eq!(
        refused.lab.ran,
        [
            Verb::Prepare,
            rebase_for_red(GREEN, "ab12"),
            Verb::Fix(FIX.into()),
            Verb::Finish
        ]
    );

    // A PREPARED line whose publish is refused, no red, with an older pump's worker on its head:
    // no re-base is due, and the worker's fix is carried as a hop beside a pass that could take
    // the line.
    let mut lab = Lab::opened();
    lab.publishes = true;
    lab.report("0.6.0", OPENED, "s-1", Report::Working);
    let journal = lab.line(OPENED);
    journal.prepared = true;
    journal.prepared_at = 1_050;
    lab.exits = vec![1, 0, 0];
    let mut older = Walk::new(lab);
    older.step("Judge", &|lab, _, _| {
        lab.greens.insert(OPENED.into(), GREEN.into());
        None
    });
    older.step("Refuse", &run);
    older.step("AcceptFix", &|lab, _, _| {
        lab.report("0.6.0", OPENED, "s-1", Report::Fix(FIX.into()));
        None
    });
    assert!(older.state["green"] == 1 && !older.enabled("Rebase"));
    older.step("Hop", &run);
    older.step("Release", &run);
    assert_eq!(
        older.lab.ran,
        [Verb::Finish, Verb::Fix(FIX.into()), Verb::Finish]
    );
    assert_eq!(older.memory.rebased, None);

    // Another holder moves the fleet line by a hop and leaves it (round 4 finding 8): the line is
    // not this clone's to retire, so no re-base is due though a judged pass could take it, and the
    // worker's fix is carried as a hop onto the line where it stands.
    let mut lab = Lab::opened();
    lab.publishes = true;
    lab.line(OPENED);
    lab.exits = vec![1, 0, 0];
    lab.caller = Some("ab12".into());
    let mut moved = Walk::new(lab);
    moved.step("GoRed", &run);
    moved.step("Judge", &|lab, _, _| {
        lab.greens.insert(OPENED.into(), GREEN.into());
        None
    });
    assert!(moved.enabled("Rebase"));
    moved.step("Move", &|lab, _, _| {
        lab.main.line = Some(HOP.into());
        None
    });
    assert!(moved.state["green"] == 1 && !moved.enabled("Rebase"));
    assert!(!moved.enabled("RebaseRefused"));
    let red = decide(&moved.lab.seen_with(&moved.memory), &moved.memory);
    assert!(waits(&red).contains("is red"), "{red:?}");
    moved.step("AcceptFix", &|lab, _, _| {
        lab.report("0.6.0", OPENED, "s-1", Report::Fix(FIX.into()));
        None
    });
    assert!(!moved.enabled("Rebase"));
    moved.step("Hop", &run);
    moved.step("Release", &run);
    assert_eq!(
        moved.lab.ran,
        [Verb::Prepare, Verb::Fix(FIX.into()), Verb::Finish]
    );
    assert_eq!(moved.memory.rebased, None);

    // NEGATIVE CONTROL: a pump that forgot the re-base re-bases the red line again, past the law.
    let model = aterm_models_release::release_pump_rebase_model();
    let (journal, mut memory) = forgetful;
    let mut lab_again = Lab::opened();
    lab_again.publishes = true;
    lab_again.journal = journal;
    lab_again.main.line = Some(GREEN.into());
    lab_again.greens.insert(GREEN.into(), LATER.into());
    memory.rebased = None;
    let mut said = String::new();
    cycle(&mut lab_again, &mut memory, &mut said);
    assert!(
        matches!(lab_again.ran[..], [Verb::Rebase { .. }]),
        "{:?}",
        lab_again.ran
    );
    let past = rebase_state(&mut lab_again, &memory, (2, 0, 0, 0));
    assert!(
        !model.check_invariant("AtMostOneRebasePerLine", &past),
        "{past:?}"
    );
    for (dial, law) in [
        (1, "AtMostOneRebasePerLine"),
        (2, "RebaseOnlyOntoAJudgedPass"),
        (3, "NoRebaseBesideALiveHolder"),
    ] {
        let broken = interp::violated_invariants(&interp::with_buggy(&model, dial));
        assert!(
            broken.contains(law),
            "Buggy = {dial} breaks {broken:?}, not {law}"
        );
    }
    assert!(interp::bmc(&model).is_ok());
}

/// What a standby reads of `fact`, a pump on the holder's Mac whose process runs and whose fact
/// is `age` s old, for v0.6.0's fleet line at `fleet`.
fn standby_reads(fact: &Fact, age: u64, fleet: &str) -> HolderPump {
    holder_pump(
        Ok(Some(vec![PumpRow {
            alive: Some(true),
            line: fact.line.clone(),
            step: fact.step.clone(),
            own: Some(fact.own),
            age,
        }])),
        "0.6.0",
        fleet,
    )
}

/// A PUMP SAYS IT HOLDS ITS LINE AT THE HEAD THE FLEET'S CLAIM RECORDS (UB-15). A hop carried
/// here whose record merge has not landed (a `--prepare --fix` refused between its carry and its
/// record, the pump's own run or a session's in its clone) leaves candidate.toml at the new head
/// while the claim still publishes the prefix main retains; a supersession left undecided leaves
/// it at the replacement's base while the claim names the head it retires. The pump names the
/// claim's head ([`Journal::claimed`]) at a run, at its end, at a wait and at a cycle that cannot
/// read origin, so the standby, which matches a waiting pump to the fleet's own head, reads it
/// live and takes nothing.
/// NEGATIVE CONTROL: the same journals named at R read as unknown, never live: the standby waits
/// them out by the lease and `PUMP_STALE_SECS`.
#[test]
fn a_pump_says_it_holds_its_line_at_the_head_the_claim_records() {
    let facts = |recorded: Option<&str>, head: &str, fleet: &str, undecided: Option<Undecided>| {
        let mut lab = Lab::opened();
        let journal = lab.line(head);
        journal.recorded = recorded.map(str::to_string);
        journal.undecided = undecided;
        lab.main.line = Some(fleet.into());
        lab.exits = vec![1];
        let (mut memory, mut said) = (Memory::default(), String::new());
        lab.now += cycle(&mut lab, &mut memory, &mut said);
        lab.now += cycle(&mut lab, &mut memory, &mut said);
        lab.main_down = true;
        cycle(&mut lab, &mut memory, &mut said);
        lab.facts
    };
    let pending = || None;
    let undecided = || {
        Some(Undecided {
            from: OPENED.into(),
            onto: GREEN.into(),
        })
    };
    // A pending hop at HOP over the fleet's GREEN; an undecided supersession of OPENED onto GREEN.
    for (head, fleet, txn) in [
        (HOP, GREEN, pending as fn() -> Option<Undecided>),
        (GREEN, OPENED, undecided),
    ] {
        let facts = facts(Some(fleet), head, fleet, txn());
        assert!(facts.len() >= 3, "{facts:#?}");
        for fact in &facts {
            assert_eq!(fact.line, format!("v0.6.0@{}", &fleet[..9]), "{fact:?}");
            assert!(fact.own, "{fact:?}");
            if fact.step == "wait" {
                assert_eq!(
                    standby_reads(fact, 30, fleet),
                    HolderPump::Live { age: 30 },
                    "{fact:?}"
                );
            }
        }
        // NEGATIVE CONTROL: named at R, a waiting pump's journal is not known to be of the fleet
        // line.
        let at_r = facts_at_r(head, fleet, txn());
        let wait = at_r.iter().rfind(|fact| fact.step == "wait").unwrap();
        assert!(
            matches!(standby_reads(wait, 30, fleet), HolderPump::Unknown(_)),
            "{wait:?}"
        );
    }

    fn facts_at_r(head: &str, fleet: &str, undecided: Option<Undecided>) -> Vec<Fact> {
        let mut lab = Lab::opened();
        lab.line(head).undecided = undecided;
        lab.main.line = Some(fleet.into());
        lab.exits = vec![1];
        let (mut memory, mut said) = (Memory::default(), String::new());
        lab.now += cycle(&mut lab, &mut memory, &mut said);
        lab.now += cycle(&mut lab, &mut memory, &mut said);
        lab.facts
    }
}

/// A PUMP THAT CANNOT READ THE WORLD AGES OUT AS ONE THAT STOPPED LOOKING (UB-15). A cycle that
/// cannot read the world (origin's credential gone, a fetch's lock left behind, DNS) still says
/// from the journal's own files that it holds its line, but with the time of its last look that
/// read the world, never now: a short outage keeps the line the pump's, one past the stale bound
/// frees it for the standby, since a pump that can never act must not hold the line for good. A
/// pump that starts says what it holds with its predecessor's last look as its survey record
/// keeps it, so one that dies before every first look (a crash loop under launchd's KeepAlive)
/// ages out too.
/// NEGATIVE CONTROLS: within the stale bound of its last look the pump reads live; a clone's first
/// pump, which has no record, says so with its start.
#[test]
fn a_pump_that_cannot_read_the_world_ages_out_as_one_that_stopped_looking() {
    let mut lab = Lab::opened();
    lab.line(OPENED).prepared = true;
    let (mut memory, mut said) = (Memory::default(), String::new());
    lab.now += cycle(&mut lab, &mut memory, &mut said);
    let looked = lab.facts.last().unwrap().at;
    for _ in 0..20 {
        lab.main_down = true;
        lab.now += cycle(&mut lab, &mut memory, &mut said);
    }
    let unread = lab.facts.last().unwrap().clone();
    assert!(
        unread.decision.contains("cannot read the world") && unread.own,
        "{unread:?}"
    );
    assert_eq!(
        unread.at, looked,
        "the time of its last look that read the world"
    );
    let age = lab.now - unread.at;
    assert!(age > PUMP_STALE_SECS, "{age}");
    assert!(matches!(
        standby_reads(&unread, age, OPENED),
        HolderPump::Gone(_)
    ));
    // NEGATIVE CONTROL: within the bound the same fact reads live.
    assert_eq!(
        standby_reads(&unread, PUMP_STALE_SECS, OPENED),
        HolderPump::Live {
            age: PUMP_STALE_SECS
        }
    );
    // A crash loop: each start says what it holds with the last look its record keeps.
    let mut looping = Lab::opened();
    looping.line(OPENED).prepared = true;
    looping.now += 3 * PUMP_STALE_SECS;
    looping.looked = Some(looping.now - 2 * PUMP_STALE_SECS);
    for _ in 0..3 {
        looping.main_down = true;
        pump(&mut looping, Memory::default(), Some(1));
        looping.now += POLL_SECS;
    }
    assert!(
        looping
            .facts
            .iter()
            .all(|fact| fact.at == looping.looked.unwrap() && fact.own),
        "{:#?}",
        looping.facts
    );
    // NEGATIVE CONTROL: a clone's first pump, with no record, says what it holds with its start.
    let mut first = Lab::opened();
    first.line(OPENED);
    first.main_down = true;
    pump(&mut first, Memory::default(), Some(1));
    assert_eq!(first.facts[0].at, first.now);
}

/// UNREADABLE LOOKS WITH A GAP BETWEEN THEM TAKE NO LINE (UB-15). A holder's pump that cannot be
/// read is waited out until it has been unreadable at every look for `PUMP_STALE_SECS`, the looks
/// at the pump's own pace: one unreadable look before a sleep (a laptop's lid, a dark wake) and
/// one after it, or before and after a run of looks that could not read the world at all, count
/// no time between them, and nor does a count kept from before the pump started.
/// NEGATIVE CONTROL: looks in a row unreadable for the whole bound take the line.
#[test]
fn unreadable_looks_with_a_gap_between_them_take_no_line() {
    let unknown = || Some(Err("it did not answer: timed out".into()));
    let mut lab = standby_lab();
    lab.holder_pump = unknown();
    let (mut memory, mut said) = (Memory::default(), String::new());
    lab.now += cycle(&mut lab, &mut memory, &mut said);
    // Eight hours asleep, then one more look that times out.
    lab.now += 8 * 60 * 60;
    lab.main.lease_until = Some(lab.now - 1);
    lab.now += cycle(&mut lab, &mut memory, &mut said);
    assert!(lab.ran.is_empty(), "{:?}", lab.ran);
    // Fifteen minutes in which this pump read nothing, then one more look that times out.
    for _ in 0..15 {
        lab.main_down = true;
        lab.now += cycle(&mut lab, &mut memory, &mut said);
    }
    lab.now += cycle(&mut lab, &mut memory, &mut said);
    assert!(lab.ran.is_empty(), "{:?}", lab.ran);
    let gapped = memory.pump_unread.clone().unwrap();
    // A pump that starts counts only its own looks.
    let restarted = Memory {
        pump_unread: Some(PumpUnread {
            since: lab.now - 2 * PUMP_STALE_SECS,
            last: lab.now - POLL_SECS,
            ..gapped.clone()
        }),
        ..memory.clone()
    };
    let mut fresh = standby_lab();
    fresh.holder_pump = unknown();
    fresh.now = lab.now;
    fresh.main.lease_until = Some(fresh.now - 1);
    pump(&mut fresh, restarted, Some(1));
    assert!(fresh.ran.is_empty(), "{:?}", fresh.ran);
    // NEGATIVE CONTROL: unreadable at every look for the bound, the line is taken over.
    let first = gapped.since;
    while lab.ran.is_empty() {
        lab.now += cycle(&mut lab, &mut memory, &mut said);
        assert!(lab.now < first + 2 * PUMP_STALE_SECS, "{:#?}", lab.said);
    }
    assert_eq!(lab.ran, [Verb::Adopt]);
    assert!(lab.now >= first + PUMP_STALE_SECS);
}

/// A STANDBY LEAVES OPENING AND STARTING A VERSION TO THE PRIMARY (UB-15). Both pumps see the
/// same pause end and the same version with no line; whichever child reaches the claim first
/// holds the line, and a primary without `--adopt` could never take back a line a standby
/// started. A standby runs `--open-next` and a start only once it has wanted the same one at every
/// look in a row for `STANDBY_DEFER_SECS`, the bound a live primary gives its own child that
/// records no row (round 4 finding 2: a start's child passes its commit gates, builds and execs
/// its frozen cutter before it reaches the claim, and the version has no line, so no holder's pump
/// to read, until it does), so the primary, which runs it at its first look, holds the line first,
/// and the standby then waits beside it. The looks count only in a row: a gap past
/// `UNREAD_GAP_SECS` (a sleep), a look that could not read the world (an outage both pumps saw,
/// after which the primary runs the start at its first look) and the pump's restart each start the
/// count again.
/// NEGATIVE CONTROL: with no primary doing either, the standby opens the version, then starts its
/// line, each once the bound has passed in a row; the primary runs both at once.
#[test]
fn a_standby_leaves_opening_and_starting_a_version_to_the_primary() {
    let mut primary = Lab::after_a_release();
    let (mut memory, mut said) = (Memory::default(), String::new());
    cycle(&mut primary, &mut memory, &mut said);
    cycle(&mut primary, &mut memory, &mut said);
    assert_eq!(primary.ran, [Verb::OpenNext, Verb::Start(OPENED.into())]);

    let mut standby = Lab::after_a_release();
    standby.adopt = true;
    let (mut memory, mut said) = (Memory::default(), String::new());
    let looks = |standby: &mut Lab, memory: &mut Memory, said: &mut String, until: u64| {
        while standby.now < until {
            standby.now += cycle(standby, memory, said);
        }
    };
    // Twenty-nine minutes of looks: the bound a primary's own child that records no row is
    // given before it is judged stalled, less a look.
    assert!(STANDBY_DEFER_SECS >= stall_bound(0));
    let first = standby.now;
    looks(&mut standby, &mut memory, &mut said, first + 29 * 60);
    assert!(standby.ran.is_empty(), "{:?}", standby.ran);
    assert!(
        standby
            .said
            .iter()
            .any(|line| line.contains("leaves opening the next version to the primary")),
        "{:#?}",
        standby.said
    );
    // The primary opens the version: the standby's wait starts again, for the start.
    standby.open();
    let opened = standby.now;
    looks(
        &mut standby,
        &mut memory,
        &mut said,
        opened + STANDBY_DEFER_SECS - POLL_SECS,
    );
    assert!(standby.ran.is_empty(), "{:?}", standby.ran);
    // Asleep for four minutes: the count starts again at the next look.
    standby.now += 2 * UNREAD_GAP_SECS;
    let woke = standby.now;
    standby.now += cycle(&mut standby, &mut memory, &mut said);
    assert_eq!(memory.due.as_ref().map(|due| due.since), Some(woke));
    looks(
        &mut standby,
        &mut memory,
        &mut said,
        woke + STANDBY_DEFER_SECS - POLL_SECS,
    );
    assert!(standby.ran.is_empty(), "{:?}", standby.ran);
    // A look that cannot read the world starts it again too.
    standby.main_down = true;
    standby.now += cycle(&mut standby, &mut memory, &mut said);
    assert_eq!(memory.due, None);
    standby.now += cycle(&mut standby, &mut memory, &mut said);
    assert!(standby.ran.is_empty(), "{:?}", standby.ran);
    // And so does the pump's restart, however long the count it kept.
    let mut kept = memory.clone();
    kept.due.as_mut().unwrap().since = standby.now - 2 * STANDBY_DEFER_SECS;
    pump(&mut standby, kept, Some(1));
    assert!(standby.ran.is_empty(), "{:?}", standby.ran);
    // The primary's start takes the line: the standby waits beside it.
    standby.main.line = Some(OPENED.into());
    standby.main.holder = Some("m16 (node 7c1e, session s-1, pid 41)".into());
    standby.main.held = Some(held(standby.now + 1_800));
    for _ in 0..30 {
        standby.now += cycle(&mut standby, &mut memory, &mut said);
    }
    assert!(standby.ran.is_empty(), "{:?}", standby.ran);

    // NEGATIVE CONTROL: no primary does either, and the standby does both, each past the bound.
    let mut alone = Lab::after_a_release();
    alone.adopt = true;
    let (mut memory, mut said) = (Memory::default(), String::new());
    let first = alone.now;
    while alone.ran.is_empty() {
        alone.now += cycle(&mut alone, &mut memory, &mut said);
    }
    assert_eq!(alone.ran, [Verb::OpenNext]);
    assert!(alone.now >= first + STANDBY_DEFER_SECS);
    let opened = alone.now;
    while alone.ran.len() < 2 {
        alone.now += cycle(&mut alone, &mut memory, &mut said);
    }
    assert_eq!(alone.ran[1], Verb::Start(OPENED.into()));
    assert!(alone.now >= opened + STANDBY_DEFER_SECS);
}

/// TWO PUMPS ON ONE MAC TELL THEIR LINES APART BY THE CLAIM'S HOLDER (UB-15). The installed pump
/// of one clone and a standby of another clone run on the same Mac, so the claim's fleet node
/// never says whose the line is. A pump whose own child is not the claim's holder treats the line
/// as another's: it asks this Mac's records after the other pump, its own left out, and while that
/// pump holds the line's journal it runs nothing of it and says the line is not its own. The pump
/// whose child holds the claim goes on, so the claim's holder alone breaks the tie.
/// NEGATIVE CONTROL: with the claim naming this pump's own child the line re-bases here, and no
/// pump is asked after.
#[test]
fn two_pumps_on_one_mac_tell_the_line_apart_by_the_claims_holder() {
    let returned = |holder_id: Process| {
        let mut lab = Lab::opened();
        lab.line(OPENED);
        lab.exits = vec![1];
        lab.caller = Some("ab12".into());
        let (mut memory, mut said) = (Memory::default(), String::new());
        cycle(&mut lab, &mut memory, &mut said);
        lab.greens.insert(OPENED.into(), GREEN.into());
        // The claim's holder is on this Mac's own node.
        lab.main.holder = Some("m16 (node 7c1e, session s-2, pid 8100)".into());
        lab.main.elsewhere = false;
        lab.main.holder_id = Some(holder_id);
        lab.main.lease_until = Some(lab.now + 1_800);
        lab.holder_pump = Some(Ok(Some(vec![PumpRow {
            alive: Some(true),
            line: format!("v0.6.0@{}", &OPENED[..9]),
            step: "wait".into(),
            own: Some(true),
            age: 20,
        }])));
        (lab, memory, said)
    };
    let other = Process {
        pid: 8_100,
        boot: "boot-1".into(),
        start: 77,
    };
    let (mut lab, mut memory, mut said) = returned(other);
    let asked = lab.holder_pump_asked;
    for _ in 0..30 {
        lab.now += cycle(&mut lab, &mut memory, &mut said);
    }
    assert_eq!(
        lab.ran,
        [Verb::Prepare],
        "nothing runs beside the other pump"
    );
    assert!(lab.holder_pump_asked > asked);
    assert!(
        lab.said.last().unwrap().contains("no run of this pump"),
        "{:#?}",
        lab.said
    );
    assert!(!lab.facts.last().unwrap().own);
    // NEGATIVE CONTROL: the claim names this pump's own first child.
    let (mut lab, mut memory, mut said) = returned(child(1));
    assert_eq!(memory.ran, [child(1)]);
    let asked = lab.holder_pump_asked;
    assert_eq!(cycle(&mut lab, &mut memory, &mut said), 0);
    assert_eq!(lab.holder_pump_asked, asked);
    assert_eq!(lab.ran.last(), Some(&rebase_for_red(GREEN, "ab12")));
}

/// A LOOK WHOSE READ OF MAIN WENT STALE DECIDES NOTHING (UB-15). A look decides on the claim its
/// read of main found; a pump that slept after that read (a laptop's lid) wakes to the rest of its
/// look, read from this clone alone, and would act on a claim a standby may have taken meanwhile.
/// Where the look ends more than `POLL_SECS` after the claim was read it is discarded, and the
/// next look reads main again: a sleep inside main's own read, after its read of the claim (its
/// scan of main's history, its config), counts as one after it (round 4 finding 4).
/// NEGATIVE CONTROL: a look that ends within a poll of its read of main decides, here the re-base.
#[test]
fn a_look_whose_read_of_main_went_stale_decides_nothing() {
    let mut lab = Lab::opened();
    lab.line(OPENED);
    lab.exits = vec![1];
    lab.caller = Some("ab12".into());
    let (mut memory, mut said) = (Memory::default(), String::new());
    cycle(&mut lab, &mut memory, &mut said);
    lab.greens.insert(OPENED.into(), GREEN.into());
    lab.asleep_in_main = PUMP_STALE_SECS + POLL_SECS;
    assert_eq!(cycle(&mut lab, &mut memory, &mut said), POLL_SECS);
    assert_eq!(lab.ran, [Verb::Prepare]);
    lab.asleep_after_main = PUMP_STALE_SECS + POLL_SECS;
    assert_eq!(cycle(&mut lab, &mut memory, &mut said), POLL_SECS);
    assert_eq!(lab.ran, [Verb::Prepare]);
    assert!(
        lab.said
            .last()
            .unwrap()
            .contains("its read of the line's claim is"),
        "{:#?}",
        lab.said
    );
    // NEGATIVE CONTROL: within a poll of its read the look decides.
    lab.asleep_after_main = POLL_SECS;
    assert_eq!(cycle(&mut lab, &mut memory, &mut said), 0);
    assert_eq!(lab.ran, [Verb::Prepare, rebase_for_red(GREEN, "ab12")]);
}

/// A RE-BASE REFUSED ONCE ANOTHER HOLDER MOVED THE LINE IS UNSPENT (UB-15). The pump judged the
/// line its own at its head, and its re-base's child spends minutes before it takes the claim; a
/// session that carried a hop meanwhile moved the fleet line, and the release refuses the re-base
/// (the line advanced), at preflight or at the claim. That run decided nothing of this line, so the
/// line's one re-base is not spent: it is not this pump's to spend on a line it no longer heads.
/// NEGATIVE CONTROLS: the same refusal with the fleet line still at the head, or at the pass the
/// re-base takes it onto (its own claim, left by a run that stopped before its record), spends it.
#[test]
fn a_rebase_refused_once_another_holder_moved_the_line_is_unspent() {
    let refused = |moved: Option<&str>| {
        let mut lab = Lab::opened();
        lab.line(OPENED);
        lab.exits = vec![1, 2];
        lab.caller = Some("ab12".into());
        let (mut memory, mut said) = (Memory::default(), String::new());
        cycle(&mut lab, &mut memory, &mut said);
        lab.greens.insert(OPENED.into(), GREEN.into());
        lab.line_moves_during_run = moved.map(str::to_string);
        assert_eq!(cycle(&mut lab, &mut memory, &mut said), 0);
        assert_eq!(lab.ran, [Verb::Prepare, rebase_for_red(GREEN, "ab12")]);
        memory
    };
    assert_eq!(refused(Some(HOP)).rebased, None);
    // NEGATIVE CONTROLS: refused at the same head, or with its own claim of the pass standing,
    // the line's re-base is spent.
    assert_eq!(refused(None).rebased.map(|kept| kept.code), Some(2));
    assert_eq!(refused(Some(GREEN)).rebased.map(|kept| kept.code), Some(2));
}

/// A FACT A HOP AHEAD OF THE CLAIM TAKES NO LINE FROM A LIVE PUMP (UB-15 round 4 finding 1). A run
/// that records its hop on main and stops before the claim's next step boundary (a refusal in the
/// rest of its checks, a pump's stop) leaves the claim at the head before the hop, held by a dead
/// child, and a journal that no longer says its line is pending. The pump's next look names the
/// claim's head (`release_pump::recorded_head` reads the claim the same look read), but its fact
/// of the run's end, read with no claim, names the hop: a standby that reads that fact reads a
/// live, waiting pump of the line's version at another head, which is unknown, never gone, and
/// waits it out by the lease and the stale bound while the pump's next look names the claim's
/// head again.
/// NEGATIVE CONTROL: the same pump dead past the restart grace, at the same head, frees the line.
#[test]
fn a_fact_a_hop_ahead_of_the_claim_takes_no_line_from_a_live_pump() {
    let ahead = |alive: bool, age: u64| {
        Some(Ok(Some(vec![PumpRow {
            alive: Some(alive),
            line: format!("v0.6.0@{}", &HOP[..9]),
            step: "wait".into(),
            own: Some(true),
            age,
        }])))
    };
    let mut lab = standby_lab();
    let (mut memory, mut said) = (Memory::default(), String::new());
    lab.holder_pump = ahead(true, 5);
    lab.now += cycle(&mut lab, &mut memory, &mut said);
    assert!(lab.ran.is_empty(), "{:?}", lab.ran);
    assert!(memory.pump_unread.is_some(), "waited out as unknown");
    // Each later look of that pump names the claim's head.
    for _ in 0..480 {
        lab.holder_pump = pump_there(Some(true), true, 30);
        lab.now += cycle(&mut lab, &mut memory, &mut said);
    }
    assert!(lab.ran.is_empty(), "{:?}", lab.ran);
    // NEGATIVE CONTROL: dead past the restart grace, it frees the line at once.
    lab.holder_pump = ahead(false, RESTART_GRACE_SECS + 1);
    cycle(&mut lab, &mut memory, &mut said);
    assert_eq!(lab.ran, [Verb::Adopt]);
}

/// A JOURNAL ANOTHER HOLDER'S RE-BASE RETIRED IS STRANDED, NOT RUN (UB-15 round 4 finding 3). A
/// primary that yielded its line to a standby, which then re-based the line and died, finds main's
/// supersession of its version onto another base: every run of its journal is refused
/// (`line_claim::follow`), and only a session's `--forget` ends it, which the pump never runs. The
/// pump runs nothing of that journal and says so, with what a session does; its fact says the line
/// is not its own, so another standby takes the line from the dead holder; and it asks no other
/// Mac after a pump. An automatic take-back by a primary without `--adopt` is not made: it needs
/// `--forget`, which withdraws the journal's asks, drafts and reservations.
/// NEGATIVE CONTROL: the same journal not retired is prepared again, which the release would refuse
/// at every try.
#[test]
fn a_journal_another_holders_rebase_retired_is_stranded_not_run() {
    let stranded = |retired: bool| {
        let mut lab = Lab::opened();
        lab.line(OPENED).retired = retired;
        // The standby re-based the line onto GREEN and its pump died: its dead child holds the
        // claim.
        lab.main.line = Some(GREEN.into());
        lab.main.superseded = true;
        lab.main.holder = Some("m9 (node 0eda, session s-9, pid 7)".into());
        lab.main.elsewhere = true;
        lab.main.lease_until = Some(lab.now - 1);
        lab.main.lease_ended = true;
        lab.holder_pump = pump_there(Some(false), true, RESTART_GRACE_SECS + 1);
        let (mut memory, mut said) = (Memory::default(), String::new());
        for _ in 0..30 {
            lab.now += cycle(&mut lab, &mut memory, &mut said);
        }
        lab
    };
    let lab = stranded(true);
    assert!(lab.ran.is_empty(), "{:?}", lab.ran);
    let last = lab.said.last().unwrap();
    assert!(
        last.contains("was superseded") && last.contains("--forget") && last.contains("--adopt"),
        "{last}"
    );
    assert!(lab.facts.iter().all(|fact| !fact.own), "{:#?}", lab.facts);
    assert_eq!(lab.holder_pump_asked, 0);
    // NEGATIVE CONTROL: not retired, the journal is prepared again.
    let lab = stranded(false);
    assert_eq!(lab.ran.first(), Some(&Verb::Prepare));
}

/// THE PUMP RENEWS ITS LINE'S LEASE BETWEEN ITS RUNS (UB-15 round 4 finding 5). The claim names
/// the pump's release child, which is not running while the pump holds the line between runs (a
/// red held for a session, a backoff, PREPARED without the direction). A standby that cannot ask
/// the holder's Mac (aterm not running there, the LAN cut off) judges the line by its lease alone,
/// and would take it once the lease ended and the pump had been unreadable for the stale bound.
/// The pump renews the lease of its own child's claim whenever it runs out within
/// `RENEW_AHEAD_SECS`, so for eight hours of a live pump the lease never ends and the standby
/// takes nothing; a claim another process holds is never renewed.
/// NEGATIVE CONTROL: the same standby fed a lease nobody renews (the pump died) takes the line
/// once the lease ends and the stale bound has passed.
#[test]
fn the_pump_renews_its_lines_lease_between_its_runs() {
    let mut lab = Lab::opened();
    lab.line(OPENED);
    lab.exits = vec![1];
    lab.caller = Some("ab12".into());
    let (mut memory, mut said) = (Memory::default(), String::new());
    lab.now += cycle(&mut lab, &mut memory, &mut said);
    assert_eq!(lab.ran, [Verb::Prepare]);
    // The claim names that run's child, with the lease its last write left.
    lab.main.holder_id = Some(child(1));
    lab.main.lease_until = Some(lab.now + LEASE_S);
    let mut standby = standby_lab();
    standby.holder_pump = Some(Err("no route to host".into()));
    let (mut kept, mut words) = (Memory::default(), String::new());
    let mirror = |standby: &mut Lab, now: u64, until: u64| {
        standby.now = now;
        standby.main.lease_until = Some(until);
        standby.main.lease_ended = now > until;
        standby.main.held = (now <= until).then(|| held(until));
    };
    let first = lab.now;
    while lab.now < first + 8 * 60 * 60 {
        lab.now += cycle(&mut lab, &mut memory, &mut said);
        let until = lab.main.lease_until.unwrap();
        assert!(until > lab.now, "the lease never ends");
        mirror(&mut standby, lab.now, until);
        cycle(&mut standby, &mut kept, &mut words);
    }
    assert_eq!(lab.ran, [Verb::Prepare], "the red is held for a session");
    assert!(standby.ran.is_empty(), "{:?}", standby.ran);
    let renewals = lab.renewed.len();
    assert!(
        (1..=8 * 60 * 60 / (LEASE_S - RENEW_AHEAD_SECS) + 1)
            .contains(&u64::try_from(renewals).unwrap()),
        "{renewals}"
    );
    assert!(
        lab.renewed
            .iter()
            .all(|(version, holder)| version == "0.6.0" && *holder == child(1))
    );
    // A claim another process holds is never renewed.
    lab.main.holder_id = Some(Process {
        pid: 7,
        boot: "boot-9".into(),
        start: 1,
    });
    lab.main.lease_until = Some(lab.now + 60);
    for _ in 0..60 {
        lab.now += cycle(&mut lab, &mut memory, &mut said);
    }
    assert_eq!(lab.renewed.len(), renewals);
    // NEGATIVE CONTROL: nobody renews the lease, and the standby takes the line.
    let until = standby.now + 60;
    let last = standby.now;
    while standby.ran.is_empty() {
        let now = standby.now + POLL_SECS;
        mirror(&mut standby, now, until);
        cycle(&mut standby, &mut kept, &mut words);
        assert!(standby.now < last + LEASE_S, "{:#?}", standby.said);
    }
    assert_eq!(standby.ran, [Verb::Adopt]);
    assert!(standby.now > until + PUMP_STALE_SECS);
}

/// A REFUSED RE-RUN OF THE PUMP'S OWN UNDECIDED RE-BASE SPENDS IT (UB-15 round 4 finding 7). A
/// re-base that stopped after it journaled its supersession (exit 130) is run again by the same
/// command, which reads candidate.toml at the replacement's base. That re-run, refused before it
/// records anything (its write proven absent, then a lease refusal), restores the journal to the
/// head the supersession would have retired, where the fleet line still stands. It decided the
/// line's re-base, which it spends: the worker's fix then goes as a hop, however long main moves.
/// NEGATIVE CONTROL: the same re-run refused once another holder moved the fleet line (at neither
/// head) spends nothing.
#[test]
fn a_refused_rerun_of_an_undecided_rebase_spends_it() {
    let refused = |moved: Option<&str>| {
        let mut lab = Lab::opened();
        lab.line(OPENED);
        lab.report("0.6.0", OPENED, "s-1", Report::Fix(FIX.into()));
        lab.greens.insert(OPENED.into(), GREEN.into());
        lab.stops_undecided = true;
        lab.refusal_restores = true;
        lab.exits = vec![130, 1];
        let (mut memory, mut said) = (Memory::default(), String::new());
        cycle(&mut lab, &mut memory, &mut said);
        assert_eq!(memory.rebased, None, "it decided nothing");
        assert!(lab.journal.as_ref().unwrap().undecided.is_some());
        lab.now += POLL_SECS;
        lab.line_moves_during_run = moved.map(str::to_string);
        assert_eq!(cycle(&mut lab, &mut memory, &mut said), 0);
        assert_eq!(lab.ran.len(), 2, "{:?}", lab.ran);
        assert_eq!(lab.ran[0], lab.ran[1], "the same re-base, again");
        let journal = lab.journal.as_ref().unwrap();
        assert_eq!(journal.head, OPENED, "restored");
        assert_eq!(journal.undecided, None);
        (lab, memory, said)
    };
    let (mut lab, mut memory, mut said) = refused(None);
    assert_eq!(
        memory
            .rebased
            .as_ref()
            .map(|kept| (kept.code, kept.from.as_str())),
        Some((1, OPENED))
    );
    for tip in [MOVED, HOP, TIP] {
        lab.main.tip = tip.into();
        lab.now += 10 * REFUSED_CAP_SECS;
        cycle(&mut lab, &mut memory, &mut said);
    }
    let rebases = lab
        .ran
        .iter()
        .filter(|verb| matches!(verb, Verb::Rebase { .. }))
        .count();
    assert_eq!(rebases, 2, "{:?}", lab.ran);
    assert!(lab.ran.contains(&Verb::Fix(FIX.into())), "{:?}", lab.ran);
    // NEGATIVE CONTROL: refused once another holder moved the line, it spends nothing.
    let (_, memory, _) = refused(Some(LATER));
    assert_eq!(memory.rebased, None);
}
