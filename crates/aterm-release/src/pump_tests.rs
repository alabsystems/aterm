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

/// A scripted world: what it reads is its fields, and running a verb applies that verb's effect
/// to them, as the release would to origin and this clone, then ends with the scripted code.
#[derive(Default)]
struct Lab {
    now: u64,
    main: Main,
    journal: Option<Journal>,
    holder: Option<String>,
    publishes: bool,
    adopt: bool,
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
    /// The exit of each run, in order; a run past the script succeeds.
    exits: Vec<i32>,
    /// A read of main that fails, once.
    main_down: bool,
    /// What a red run does to the repair record.
    opens: Option<(String, Report)>,
    ran: Vec<Verb>,
    said: Vec<String>,
    waited: Vec<u64>,
    remembered: Option<Memory>,
    /// How many waits the loop is given before it is told to stop.
    patience: usize,
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
        observe(self).unwrap()
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
        if std::mem::take(&mut self.main_down) {
            return Err("origin did not answer".into());
        }
        Ok(self.main.clone())
    }
    fn journal(&mut self) -> Result<Option<Journal>, String> {
        Ok(self.journal.clone())
    }
    fn holder(&mut self) -> Option<String> {
        self.holder.clone()
    }
    fn publishes(&mut self) -> bool {
        self.publishes
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
    fn run(&mut self, verb: &Verb) -> Ran {
        self.ran.push(verb.clone());
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
        unread: None,
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

    // Exit 4, another live holder: wait; adoption is tried again later, never a second line.
    let mut lab = Lab::opened();
    lab.adopt = true;
    lab.main.line = Some(OPENED.into());
    let memory = attempt(&mut lab, 4, 0);
    lab.now += HOLDER_SECS - 1;
    assert!(waits(&decide(&lab.seen(), &memory)).contains("no second line is started"));
    lab.now += 1;
    assert_eq!(runs(&decide(&lab.seen(), &memory)), Some(&Verb::Adopt));

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

/// The stopped line names the supersede candidate, or says there is none; the pump runs neither.
#[test]
fn a_stopped_line_names_the_newest_judged_pass_past_its_head_as_a_candidate() {
    let mut lab = Lab::opened();
    lab.line(OPENED);
    lab.report(
        "0.6.0",
        OPENED,
        "s-9",
        Report::NoFix("could not finish".into()),
    );
    let none = decide(&lab.seen(), &Memory::default());
    assert!(
        waits(&none).contains("there is no supersede candidate"),
        "{none:?}"
    );
    lab.green = Some(GREEN.into());
    let named = decide(&lab.seen(), &Memory::default());
    assert!(
        waits(&named).contains(&format!("--commit {GREEN} --supersede")),
        "{named:?}"
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
    assert_eq!(spelled(Verb::Prepare), "release --prepare");
    assert_eq!(spelled(Verb::Finish), "release");
    assert_eq!(
        spelled(Verb::Fix(FIX.into())),
        format!("release --prepare --fix {FIX}")
    );
    for verb in [
        Verb::OpenNext,
        Verb::Start(OPENED.into()),
        Verb::Adopt,
        Verb::Prepare,
        Verb::Finish,
        Verb::Fix(FIX.into()),
    ] {
        let args = verb.args(&linux);
        for never in ["--forget", "--bump", "--supersede", "--abandon"] {
            assert!(
                !args.iter().any(|arg| arg == never),
                "{verb:?} spells {never}"
            );
        }
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
/// verdict at first; unread at every look for half an hour, the line is said to be stopped,
/// with what a session does and the supersede candidate. A report that reads again resets it.
#[test]
fn a_report_unread_for_half_an_hour_stops_the_line_for_a_session() {
    let mut lab = Lab::opened();
    lab.line(OPENED);
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
    /// Its processes work (compilers, tests): the tree is never idle.
    busy: bool,
    /// The second its tree last made progress; `None` where the tree is not read.
    idle_since: Option<u64>,
    stop_at: Option<u64>,
    interrupts: Vec<u64>,
    said: Vec<String>,
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
                p95_s: 0,
                boundary: "qualify — test aterm-gui".into(),
            }),
            advances: 0,
            busy: false,
            idle_since: Some(T0),
            stop_at: None,
            interrupts: Vec::new(),
            said: Vec::new(),
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

/// A LONG ROW IS NOT A STALL. A run whose processes work is never interrupted however long it
/// completes no row (a slow Mac's test stage: 108 minutes of 0.199.0), nor is one that keeps
/// completing rows while idle between them, one that records no row at all (it holds no fleet
/// line), or one whose tree is not read. A stop asked of the pump is passed on once.
/// NEGATIVE CONTROL: judged by rows alone, the working run is interrupted at its bound, and
/// again on every resume.
#[test]
fn a_run_whose_processes_work_or_that_holds_no_line_is_only_waited_on() {
    let mut working = Child::silent();
    working.busy = true;
    working.ends = Some((T0 + 108 * 60, 0));
    assert_eq!(attend(&mut working, true), 0);
    assert!(working.interrupts.is_empty() && working.said.is_empty());

    let mut rows = Child::silent();
    rows.advances = STALL_FLOOR_SECS - 60;
    rows.ends = Some((T0 + 40 * STALL_FLOOR_SECS, 0));
    assert_eq!(attend(&mut rows, true), 0);
    assert!(rows.interrupts.is_empty() && rows.said.is_empty());

    let mut lineless = Child::silent();
    lineless.row = None;
    lineless.ends = Some((T0 + 40 * STALL_FLOOR_SECS, 1));
    assert_eq!(attend(&mut lineless, true), 1);
    assert!(lineless.interrupts.is_empty() && lineless.said.is_empty());

    let mut unread = Child::silent();
    unread.idle_since = None;
    unread.ends = Some((T0 + 40 * STALL_FLOOR_SECS, 1));
    assert_eq!(attend(&mut unread, true), 1);
    assert!(unread.interrupts.is_empty() && unread.said.is_empty());

    let mut asked = Child::silent();
    asked.stop_at = Some(T0 + 30);
    assert_eq!(attend(&mut asked, true), 130);
    assert_eq!(asked.interrupts, [T0 + 30]);
    assert!(asked.said.is_empty(), "a stop is no stall");
}
