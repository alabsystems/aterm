// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

fn words(w: &[&str]) -> Vec<String> {
    w.iter().map(ToString::to_string).collect()
}

const INSTANCE: i32 = 4242;
const CLAUDE: i32 = 4250;

/// What the fake saw, in order.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Ev {
    Signal(i32, Sig),
    Offset(i64),
    Hold(Duration),
}

/// A lab's processes on a clock of their own: nothing is signalled and
/// nothing sleeps (a sleep moves the clock).
struct Fake {
    ours: bool,
    /// The instance's pid stops being the lab's at this time.
    ours_until: Option<i64>,
    clock: i64,
    offset: i64,
    /// The offset write fails.
    write_fails: bool,
    /// A sleep this long or longer is interrupted.
    stop_at: Option<Duration>,
    /// The journal: when each line is written.
    journal: Vec<(i64, String)>,
    /// How far the journal was read (`None`: never looked at).
    journal_read: Option<usize>,
    /// Each process's record: the offset it took, from this time on.
    took: Vec<Proc>,
    /// The instance takes a new offset this long after its continue (never: `None`).
    instance_takes_after: Option<i64>,
    /// The instance ends at this time (a crash a driver made in the hold).
    ends_at: Option<i64>,
    stopped: bool,
    ev: Vec<Ev>,
}

/// A process of the fake lab: its pid, executable, and each offset it
/// took with the time it took it.
type Proc = (i32, &'static str, Vec<(i64, i64)>);

fn fake() -> Fake {
    Fake {
        ours: true,
        ours_until: None,
        clock: 1_000_000,
        offset: 0,
        write_fails: false,
        stop_at: None,
        journal: Vec::new(),
        journal_read: None,
        took: vec![
            (INSTANCE, "/b/aterm", vec![(0, 0)]),
            (CLAUDE, "/r/clock/claude", vec![(0, 0)]),
        ],
        instance_takes_after: Some(40),
        ends_at: None,
        stopped: false,
        ev: Vec::new(),
    }
}

fn lab() -> Run {
    Run {
        tag: "z1".into(),
        instance_pid: INSTANCE,
        clock: "1".into(),
        ..Run::default()
    }
}

fn line(t: i64, kind: &str, rule: &str) -> (i64, String) {
    (
        t,
        format!(
            r#"{{"t":{t},"sid":"s-1","kind":"{kind}","phase":"{rule}","seq":7,"line":"{} seq=7 rule={rule} keep going"}}"#,
            kind.to_uppercase()
        ),
    )
}

impl Ops for Fake {
    fn names_lab(&mut self, run: &Run, pid: i32) -> bool {
        self.ours
            && pid == run.instance_pid
            && self.alive(pid)
            && self.ours_until.is_none_or(|t| self.clock < t)
    }
    fn records(&mut self, _run: &Run) -> Vec<Record> {
        let now = self.clock;
        self.took
            .iter()
            .map(|(pid, exe, steps)| Record {
                pid: *pid,
                ppid: 1,
                exe: (*exe).into(),
                offset_ns: steps
                    .iter()
                    .rfind(|(t, _)| *t <= now)
                    .map_or(0, |(_, s)| i128::from(*s) * 1_000_000_000),
            })
            .collect()
    }
    fn alive(&mut self, pid: i32) -> bool {
        pid != INSTANCE || self.ends_at.is_none_or(|t| self.clock < t)
    }
    fn read_offset(&mut self, _run: &Run) -> Result<i64, String> {
        Ok(self.offset)
    }
    fn write_offset(&mut self, _run: &Run, secs: i64) -> Result<(), String> {
        if self.write_fails {
            return Err("a full disk".into());
        }
        self.offset = secs;
        self.ev.push(Ev::Offset(secs));
        let now = self.clock;
        for (pid, _, steps) in &mut self.took {
            if *pid == CLAUDE {
                steps.push((now + 5, secs));
            }
        }
        Ok(())
    }
    fn journal_lines(&mut self, _run: &Run) -> Vec<String> {
        let now = self.clock;
        let upto = self.journal.iter().filter(|(t, _)| *t <= now).count();
        let from = self.journal_read.replace(upto);
        from.map_or_else(Vec::new, |from| {
            self.journal[from..upto]
                .iter()
                .map(|(_, l)| l.clone())
                .collect()
        })
    }
    fn signal(&mut self, pid: i32, sig: Sig) -> Result<(), String> {
        self.ev.push(Ev::Signal(pid, sig));
        if !self.alive(pid) {
            return Err("No such process (os error 3)".into());
        }
        match sig {
            Sig::Stop => self.stopped = true,
            Sig::Cont => {
                self.stopped = false;
                let (now, off) = (self.clock, self.offset);
                if let Some(after) = self.instance_takes_after {
                    for (p, _, steps) in &mut self.took {
                        if *p == INSTANCE {
                            steps.push((now + after, off));
                        }
                    }
                }
            }
        }
        Ok(())
    }
    fn now_ms(&mut self) -> i64 {
        self.clock
    }
    fn sleep(&mut self, d: Duration) -> Result<(), Fail> {
        if self.stop_at.is_some_and(|s| d >= s) {
            return Err(Fail::from(stop::Stopped(libc::SIGINT)));
        }
        self.clock += i64::try_from(d.as_millis()).unwrap();
        if self.stopped {
            self.ev.push(Ev::Hold(d));
        }
        Ok(())
    }
}

fn asked(w: &[&str]) -> Asked {
    parse_args(&words(w)).unwrap().unwrap()
}

/// THE SLEEP: the instance alone is stopped, the offset stepped while it
/// is, held, and continued; nothing else is signalled.
#[test]
fn the_offset_steps_between_a_stop_and_a_continue_of_the_instance_alone() {
    let mut f = fake();
    let s = sleep(&lab(), &asked(&["--for", "1800", "--hold", "3"]), &mut f).unwrap();
    assert_eq!(
        f.ev,
        [
            Ev::Signal(INSTANCE, Sig::Stop),
            Ev::Offset(1800),
            Ev::Hold(Duration::from_secs(3)),
            Ev::Signal(INSTANCE, Sig::Cont),
        ]
    );
    assert_eq!((s.old, s.new, s.t_cont - s.t_stop), (0, 1800, 3000));
    assert_eq!(
        s.took,
        [
            (INSTANCE, "aterm".to_string()),
            (CLAUDE, "claude".to_string())
        ]
    );
    assert!(s.not_yet.is_empty() && s.act.is_none());
    let s = sleep(&lab(), &asked(&["--for", "60"]), &mut f).unwrap();
    assert_eq!((s.old, s.new), (1800, 1860));
}

/// AT AN ACT: the instance is stopped the moment the harness journals a
/// `CONTINUED` line written after the verb began (of the rule named),
/// never at one written before, nor at another kind or rule.
#[test]
fn at_an_act_it_stops_at_the_first_act_journaled_after_it_began() {
    let mut f = fake();
    let t0 = f.clock;
    f.journal = vec![
        line(t0 - 10, "continued", "usage-resume@v1"),
        line(t0 + 1000, "waiting", "usage-resume@v1"),
        line(t0 + 2000, "continued", "keep-going@v1"),
        line(t0 + 1_800_000, "continued", "usage-resume@v1"),
    ];
    let a = asked(&[
        "--for",
        "1800",
        "--hold",
        "3",
        "--at-act",
        "--rule",
        "usage-resume@v1",
    ]);
    let s = sleep(&lab(), &a, &mut f).unwrap();
    let (act, read) = s.act.clone().unwrap();
    assert_eq!(
        (act.t, act.rule.as_str()),
        (t0 + 1_800_000, "usage-resume@v1")
    );
    assert_eq!(act.line, "CONTINUED seq=7 rule=usage-resume@v1 keep going");
    assert!(read - act.t <= 2, "read {} ms after it", read - act.t);
    assert_eq!(s.t_stop, read);
    assert_eq!(f.ev[0], Ev::Signal(INSTANCE, Sig::Stop));
    let mut f = fake();
    let t0 = f.clock;
    f.journal = vec![
        line(t0 - 10, "continued", "usage-resume@v1"),
        line(t0 + 2000, "continued", "keep-going@v1"),
    ];
    let s = sleep(&lab(), &asked(&["--for", "5", "--at-act"]), &mut f).unwrap();
    assert_eq!(s.act.unwrap().0.rule, "keep-going@v1");
}

/// AN INTERRUPT IN THE HOLD, OR A STEP THAT COULD NOT BE WRITTEN, STILL
/// CONTINUES THE CONFIRMED INSTANCE.
#[test]
fn an_interrupt_or_a_failed_step_still_continues_the_instance() {
    let mut f = fake();
    f.stop_at = Some(Duration::from_secs(3));
    let e = sleep(&lab(), &asked(&["--for", "1800", "--hold", "3"]), &mut f).unwrap_err();
    assert_eq!(e.stopped, Some(libc::SIGINT));
    assert_eq!(
        f.ev,
        [
            Ev::Signal(INSTANCE, Sig::Stop),
            Ev::Offset(1800),
            Ev::Signal(INSTANCE, Sig::Cont),
        ]
    );
    let mut f = fake();
    f.write_fails = true;
    let e = sleep(&lab(), &asked(&["--for", "1800", "--hold", "3"]), &mut f).unwrap_err();
    assert!(e.msg.unwrap().contains("not stepped"));
    assert_eq!(
        f.ev,
        [
            Ev::Signal(INSTANCE, Sig::Stop),
            Ev::Signal(INSTANCE, Sig::Cont)
        ]
    );
    assert_eq!(f.offset, 0);

    let mut f = fake();
    f.ours_until = Some(f.clock + 1000);
    let e = sleep(&lab(), &asked(&["--for", "1800", "--hold", "3"]), &mut f).unwrap_err();
    assert!(e.msg.unwrap().contains("no longer confirmed"));
    assert_eq!(
        f.ev,
        [
            Ev::Signal(INSTANCE, Sig::Stop),
            Ev::Offset(1800),
            Ev::Hold(Duration::from_secs(3)),
        ]
    );
    assert!(f.alive(INSTANCE));
}

/// AN INSTANCE THAT ENDS WHILE IT IS STOPPED (a crash a driver makes in
/// the hold: `crash --keep-tree`, the next instance up at the stepped
/// offset) is said to have ended, never to be left stopped: there is
/// nothing to continue.
#[test]
fn an_instance_that_ends_in_the_hold_is_said_gone_not_left_stopped() {
    let mut f = fake();
    f.ends_at = Some(f.clock + 1000);
    let e = sleep(&lab(), &asked(&["--for", "1800", "--hold", "3"]), &mut f).unwrap_err();
    let msg = e.msg.unwrap();
    assert_eq!(e.code, 1);
    assert!(msg.contains("ended while it was stopped"), "{msg}");
    assert!(!msg.contains("left stopped"), "{msg}");
    assert_eq!(
        f.ev,
        [
            Ev::Signal(INSTANCE, Sig::Stop),
            Ev::Offset(1800),
            Ev::Hold(Duration::from_secs(3)),
        ]
    );
    assert_eq!(f.offset, 1800);
}

/// THE STEP IS AWAITED IN THE INSTANCE'S OWN RECORD: one that never
/// records the new offset fails the verb (the instance continued).
#[test]
fn the_step_is_awaited_in_the_instance_record() {
    let mut f = fake();
    f.instance_takes_after = Some(59_000);
    assert!(sleep(&lab(), &asked(&["--for", "10"]), &mut f).is_ok());
    let mut f = fake();
    f.instance_takes_after = None;
    let e = sleep(&lab(), &asked(&["--for", "10"]), &mut f).unwrap_err();
    assert!(e.msg.unwrap().contains("did not record the step"));
    assert_eq!(f.ev.last(), Some(&Ev::Signal(INSTANCE, Sig::Cont)));
}

/// REFUSED, NOTHING STOPPED AND THE OFFSET AS IT WAS: a lab up without
/// its clock, a pid that is no longer the lab's, an instance with no
/// clock record, an act that never came.
#[test]
fn a_refusal_stops_nothing_and_steps_nothing() {
    let a = asked(&["--for", "1800"]);
    let mut no_clock = lab();
    no_clock.clock = "0".into();
    let mut f = fake();
    let e = sleep(&no_clock, &a, &mut f).unwrap_err();
    assert!(e.msg.unwrap().contains("without its clock"));
    let mut f = fake();
    f.ours = false;
    assert!(sleep(&lab(), &a, &mut f).is_err());
    assert!(f.ev.is_empty());
    let mut f = fake();
    f.took.retain(|(p, _, _)| *p != INSTANCE);
    let e = sleep(&lab(), &a, &mut f).unwrap_err();
    assert!(e.msg.unwrap().contains("no clock record"));
    assert!(f.ev.is_empty());
    let mut f = fake();
    let t0 = f.clock;
    f.journal = vec![line(t0 - 10, "continued", "usage-resume@v1")];
    let e = sleep(
        &lab(),
        &asked(&["--for", "1800", "--at-act", "--within", "5"]),
        &mut f,
    )
    .unwrap_err();
    assert!(e.msg.unwrap().contains("no act journaled within 5 s"));
    assert!(f.ev.is_empty() && f.offset == 0);
    let mut f = fake();
    let t0 = f.clock;
    f.journal = vec![line(t0 + 600_000, "continued", "usage-resume@v1")];
    f.ours_until = Some(t0 + 60_000);
    let e = sleep(&lab(), &asked(&["--for", "1800", "--at-act"]), &mut f).unwrap_err();
    assert!(e.msg.unwrap().contains("no process of the lab any more"));
    assert!(f.ev.is_empty() && f.offset == 0);
    assert!(f.clock - t0 <= 61_000, "refused {} ms on", f.clock - t0);

    let mut f = fake();
    let t0 = f.clock;
    f.journal = vec![line(t0 + 2, "continued", "usage-resume@v1")];
    f.ours_until = Some(t0 + 2);
    let e = sleep(&lab(), &asked(&["--for", "1800", "--at-act"]), &mut f).unwrap_err();
    assert!(e.msg.unwrap().contains("no process of the lab now"));
    assert!(f.ev.is_empty() && f.offset == 0);
    assert_eq!(f.clock, t0 + 2);

    let mut f = fake();
    f.offset = i64::MAX;
    let e = sleep(&lab(), &asked(&["--for", "1"]), &mut f).unwrap_err();
    assert!(e.msg.unwrap().contains("offset step is out of range"));
    assert!(f.ev.is_empty());
    assert_eq!(f.offset, i64::MAX);

    let mut f = fake();
    f.offset = -1;
    let got = sleep(&lab(), &asked(&["--for", "1"]), &mut f).unwrap();
    assert_eq!((got.old, got.new, f.offset), (-1, 0, 0));
    assert_eq!(f.ev.first(), Some(&Ev::Signal(INSTANCE, Sig::Stop)));
    assert_eq!(f.ev.last(), Some(&Ev::Signal(INSTANCE, Sig::Cont)));
}

/// THE JOURNALS' NEW LINES ONLY: the first look reads nothing (what was
/// written before the verb began is never an act of it), a later one
/// what was appended since, a journal born since from its start, and a
/// line still being written only once it ends.
#[test]
fn the_journals_are_read_from_where_they_ended_at_the_first_look() {
    let mut files: HashMap<PathBuf, Vec<u8>> = HashMap::new();
    let a = PathBuf::from("/d/s-1.journal.jsonl");
    let b = PathBuf::from("/d/s-2.journal.jsonl");
    files.insert(a.clone(), b"old 1\nold 2\n".to_vec());
    let mut tail = Tail::default();
    let mut look = |files: &HashMap<PathBuf, Vec<u8>>| {
        let lens: Vec<(PathBuf, u64)> = files
            .iter()
            .map(|(p, b)| (p.clone(), b.len() as u64))
            .collect();
        tail.lines(&lens, |p, from| {
            Some(files[p][usize::try_from(from).unwrap()..].to_vec())
        })
    };
    assert!(look(&files).is_empty());
    assert!(look(&files).is_empty());
    files.get_mut(&a).unwrap().extend(b"new 1\nnew 2 half");
    files.insert(b.clone(), b"born 1\n".to_vec());
    let mut got = look(&files);
    got.sort();
    assert_eq!(got, ["born 1", "new 1"]);
    files.get_mut(&a).unwrap().extend(b" done\n");
    assert_eq!(look(&files), ["new 2 half done"]);
    assert!(look(&files).is_empty());
}

#[test]
fn its_words_parse() {
    assert_eq!(
        asked(&[
            "--for", "1800", "--hold", "0.25", "--at-act", "--rule", "r@v1", "--within", "60"
        ]),
        Asked {
            secs: 1800,
            hold_ms: 250,
            at_act: true,
            rule: Some("r@v1".into()),
            within: 60,
        }
    );
    let plain = asked(&["--for", "1"]);
    assert_eq!(
        (plain.hold_ms, plain.within, plain.at_act),
        (0, WITHIN_DEFAULT, false)
    );
    for bad in [
        &[][..],
        &["--for", "0"],
        &["--for", "-5"],
        &["--for", "2592001"],
        &["--for", "10", "--hold", "1.2345"],
        &["--for", "10", "--hold", ".5"],
        &["--for", "10", "--rule", "r@v1"],
        &["--for", "10", "--within", "5"],
        &["--for", "10", "--bogus"],
        &["--for"],
    ] {
        assert_eq!(parse_args(&words(bad)).unwrap_err().code, 2, "{bad:?}");
    }
    assert!(parse_args(&words(&["--help"])).unwrap().is_none());
    assert_eq!(parse_ms("3"), Some(3000));
    assert_eq!(parse_ms("0.2"), Some(200));
}
