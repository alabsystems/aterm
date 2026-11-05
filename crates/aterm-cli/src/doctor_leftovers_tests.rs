// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Doctor's `leftovers:` row over the running instance's leftover view as it answers it, and
//! over a reading of the table that knows no tab.

use super::*;
use crate::Mark;

/// A `sys left scope=aterm --json` body of a whole reading: two processes a closed tab and a
/// dead run left (the oldest has run 3 h 20 m), an orphan of a live tab, and `links` dangling
/// links.
fn body(entry: bool, links: usize, more: u64) -> String {
    reading_body(entry, links, more, true, false)
}

/// [`body`] of a reading `complete` (the sweep listed every process) or not, and `bounded` (it
/// stopped at its bound of new candidates) or not.
fn reading_body(entry: bool, links: usize, more: u64, complete: bool, bounded: bool) -> String {
    let link = r#"{"state":"dangling","root":1,"kind":"target","target":"gone","seen":"disk"}"#;
    let links: Vec<&str> = std::iter::repeat_n(link, links).collect();
    format!(
        r#"{{"view":"left","left":{{"complete":{complete},"bounded":{bounded},"program_check":0}},"procs":[{{"pid":2606,"state":"left","started_ms":12000000,"tier":"offer","reason":"owner-gone","entry":{entry},"end":"2606@1:2"}},{{"pid":4300,"state":"left","started_ms":600000,"tier":"auto","reason":"launcher-dead","entry":false,"end":"4300@1:3"}},{{"pid":2700,"state":"orphaned","started_ms":99000000,"tier":"offer","reason":"orphaned","entry":false,"end":"2700@1:4"}}],"more":{more},"runs":[],"links":[{}]}}"#,
        links.join(",")
    )
}

/// THE ROW COUNTS ROWS AND LINKS WITH ITS POINTER: of the instance's view, the processes that
/// outlived their tab or run (not a live tab's orphan), the longest any of them has run, and the
/// links, then where to look; a note while none is one the Entry names. A view with none says
/// so, ok.
#[test]
fn leftovers_row_counts_rows_and_links_with_its_pointer() {
    let left = from_json(&body(false, 1, 0)).expect("the leftover view's body");
    assert_eq!(
        left,
        Leftovers {
            from_instance: true,
            rows: 2,
            more: false,
            oldest_ms: Some(12_000_000),
            eligible: false,
            links: Some(1),
        }
    );
    assert_eq!(
        row(&Ok(left)),
        (
            Mark::Note,
            "2 processes outlived a closed tab or a dead verify run, the oldest 3 h; 1 dangling \
             link (aterm ctl sys left scope=aterm)"
                .to_owned()
        )
    );
    let more = from_json(&body(false, 3, 4)).expect("a capped reply");
    assert!(row(&Ok(more)).1.starts_with("at least 2 processes"));
    let young = Leftovers {
        oldest_ms: Some(130_000),
        ..from_json(&body(false, 0, 0)).expect("the leftover view's body")
    };
    assert!(
        row(&Ok(young)).1.contains(", the oldest 2 m; "),
        "under an hour, in minutes"
    );
    let none = r#"{"view":"left","left":{"complete":true,"bounded":false},"procs":[],"more":0,"runs":[],"links":[]}"#;
    assert_eq!(
        row(&Ok(from_json(none).expect("an empty view"))),
        (
            Mark::Ok,
            "no process outlived a closed tab or a dead verify run; no dangling links".to_owned()
        )
    );
    assert_eq!(from_json("OK 1 sys v=1 view=left"), None, "no body");
}

/// THE ROW WARNS ONCE A ROW IS ONE THE ENTRY NAMES: a leftover whose owner has been gone an hour
/// and that costs something (or a dead run's) is a step for the person.
#[test]
fn leftovers_row_warns_when_a_row_is_eligible() {
    let left = from_json(&body(true, 0, 0)).expect("the leftover view's body");
    assert!(left.eligible);
    let (mark, words) = row(&Ok(left));
    assert_eq!(mark, Mark::Warn, "{words}");
    assert!(
        words.ends_with("no dangling links (aterm ctl sys left scope=aterm)"),
        "{words}"
    );
}

/// A row of the reading with no tab known: `why`, its source, how long it has run, and its tier.
fn judged_row(
    pid: u32,
    why: aterm_vitals::left::Why,
    from: aterm_vitals::left::Source,
    started_ms: u64,
) -> aterm_vitals::left::LeftRow {
    use aterm_vitals::left::{LeftRow, State, Tier};
    LeftRow {
        pid,
        state: State::Left,
        peer: None,
        check_ms: None,
        started_ms: Some(started_ms),
        cpu_ms: Some(0),
        exe_judged: Some("/w/wt/target/debug/aterm-link".into()),
        tier: Tier::Offer,
        why,
        proof: Vec::new(),
        listen: false,
        from,
        teller: None,
        token: Some(format!("{pid}@1:2")),
        foreground: false,
        after_agent: false,
    }
}

/// THE ROW WITHOUT AN INSTANCE COUNTS DEAD RUNS ONLY: the table and this boot's records know no
/// tab, so only a dead verify run's leftovers are counted (and named once its launcher has been
/// dead an hour, though they cost nothing), and no link is read.
#[test]
fn leftovers_row_without_an_instance_counts_dead_runs_only() {
    use aterm_vitals::left::{ENTRY_AFTER_MS, LeftView, RunRow, Source, Why};
    let run = "41983-1792163800331520";
    let view = |dead_ms: u64| LeftView {
        rows: vec![
            judged_row(4300, Why::LauncherDead, Source::Run(run.into()), 7_200_000),
            judged_row(2700, Why::Orphaned, Source::Unknown, 90_000_000),
        ],
        runs: vec![RunRow {
            id: run.into(),
            dead: true,
            dead_ms: Some(dead_ms),
            ..RunRow::default()
        }],
        complete: true,
        ..LeftView::default()
    };
    let fresh = from_rows(&view(60_000));
    assert_eq!(
        fresh,
        Leftovers {
            from_instance: false,
            rows: 1,
            more: false,
            oldest_ms: Some(7_200_000),
            eligible: false,
            links: None,
        }
    );
    assert_eq!(
        row(&Ok(fresh)),
        (
            Mark::Note,
            "1 process outlived a dead verify run, the oldest 2 h; links not read with no \
             instance running (aterm ctl sys left scope=aterm)"
                .to_owned()
        )
    );
    assert!(from_rows(&view(ENTRY_AFTER_MS)).eligible, "an hour dead");
    let empty = from_rows(&LeftView {
        complete: true,
        ..LeftView::default()
    });
    assert_eq!(
        row(&Ok(empty)).1,
        "no process outlived a dead verify run; links not read with no instance running"
    );
}

/// THE ROW COUNTS ONLY ROWS THAT OUTLIVED THEIR OWNER: of the instance's view, a row a live verify
/// run still holds (`reason=live`, tier never) and a daemon (`reason=daemon`) outlived a closed
/// tab by their `state` but are listed, never counted; of a reading with no tab known, the same
/// rule leaves out a dead run's daemon and its held program. NEGATIVE CONTROL: count every
/// `state=left` row (or every dead run's row), and they are counted.
#[test]
fn leftovers_row_counts_only_rows_that_outlived_their_owner() {
    use aterm_vitals::left::{LeftView, RunRow, Source, State, Tier, Why};
    let extra = r#"{"pid":2800,"state":"left","started_ms":50000000,"tier":"never","reason":"live","entry":false,"end":"-"},{"pid":2900,"state":"left","started_ms":60000000,"tier":"name","reason":"daemon","entry":false,"end":"-"},"#;
    let shown = body(false, 0, 0).replacen(r#""procs":["#, &format!(r#""procs":[{extra}"#), 1);
    let left = from_json(&shown).expect("the leftover view's body");
    assert_eq!(
        (left.rows, left.oldest_ms),
        (2, Some(12_000_000)),
        "the live run's row and the daemon are not counted"
    );
    let run = "41983-1792163800331520";
    let row = |pid, state, why| aterm_vitals::left::LeftRow {
        state,
        ..judged_row(pid, why, Source::Run(run.into()), 7_200_000)
    };
    let view = LeftView {
        rows: vec![
            row(4300, State::Left, Why::LauncherDead),
            row(4301, State::Left, Why::Daemon),
            aterm_vitals::left::LeftRow {
                tier: Tier::Name,
                token: None,
                ..row(4302, State::ProgramCheck, Why::LauncherDead)
            },
        ],
        runs: vec![RunRow {
            id: run.into(),
            dead: true,
            dead_ms: Some(60_000),
            ..RunRow::default()
        }],
        complete: true,
        ..LeftView::default()
    };
    assert_eq!(
        from_rows(&view).rows,
        1,
        "the daemon and the held program are not"
    );
}

/// THE ROW IS A FLOOR WHEN THE READING IS PARTIAL: a view whose reading stopped at its bound of
/// new candidates, or whose sweep hit its cap, proves no absence, so a view of none is no `ok`
/// and its count is "at least"; so is a table read whose reading was bounded or cut. NEGATIVE
/// CONTROL: read only the reply's own `more`, and a bounded view of none reads `ok`.
#[test]
fn leftovers_row_is_a_floor_when_the_reading_is_partial() {
    let none = |complete: bool, bounded: bool| {
        format!(
            r#"{{"view":"left","left":{{"complete":{complete},"bounded":{bounded}}},"procs":[],"more":0,"runs":[],"links":[]}}"#
        )
    };
    for (complete, bounded) in [(true, true), (false, false)] {
        let left = from_json(&none(complete, bounded)).expect("an empty view");
        assert!(left.more, "complete={complete} bounded={bounded}");
        let (mark, words) = row(&Ok(left));
        assert_eq!(mark, Mark::Note, "{words}");
        assert!(
            words.starts_with("at least 0 processes outlived"),
            "{words}"
        );
    }
    let bounded = from_json(&reading_body(false, 0, 0, true, true)).expect("a bounded view");
    assert!(row(&Ok(bounded)).1.starts_with("at least 2 processes"));
    let cut = from_rows(&aterm_vitals::left::LeftView::default());
    assert!(cut.more, "a table read whose sweep was cut");
}

/// THE ROW IS NOT MEASURED WHEN AN INSTANCE RUNS BUT DOES NOT ANSWER: its closed tabs and links
/// are read nowhere else, so the table is not read in its place; with no instance running the
/// table is the row, and a reply that is not the view is not measured. NEGATIVE CONTROL: read
/// the table whenever no view answered, and a silent instance's row reads `ok`.
#[test]
fn leftovers_row_is_not_measured_when_the_instance_does_not_answer() {
    let table = || Ok(Leftovers::default());
    let silent = fact_of(
        Asked::NoView("the running instance did not answer its leftover view within 2 s".into()),
        || panic!("the table is not read while an instance runs"),
    );
    assert_eq!(
        row(&silent),
        (
            Mark::Note,
            "not measured (the running instance did not answer its leftover view within 2 s)"
                .to_owned()
        )
    );
    assert_eq!(fact_of(Asked::NoInstance, table), Ok(Leftovers::default()));
    assert!(
        fact_of(
            Asked::View(r#"{"view":"left","left":{"why":"not_sampled"}}"#.into()),
            || panic!("the table is not read while an instance runs")
        )
        .is_err(),
        "a view the instance has not read yet"
    );
}

/// The table read's world: this process (pid 100 under pid 90), and processes launched by version
/// 50 (never launchd's, 1), each with the run id its environment carries.
#[derive(Default)]
struct World {
    procs: std::sync::Mutex<
        std::collections::BTreeMap<i32, (aterm_uds::process::Lineage, Option<String>)>,
    >,
}

/// The uid every process of [`World`] runs as.
const UID: u32 = 501;
/// The pid doctor runs as in [`World`].
const DOCTOR: u32 = 100;

impl World {
    fn new() -> Self {
        let world = Self::default();
        world.add(90, 1, 10, None);
        world.add(i32::try_from(DOCTOR).unwrap(), 90, 20, None);
        world
    }

    fn add(&self, pid: i32, parent: u32, uniqueid: u64, run: Option<&str>) {
        use aterm_uds::process::{Birth, Identity, Lineage};
        self.procs.lock().unwrap().insert(
            pid,
            (
                Lineage {
                    identity: Identity {
                        pid,
                        birth: Birth(1_792_000_000_000_000 + u64::from(pid.unsigned_abs())),
                        idversion: Some(pid),
                    },
                    uniqueid,
                    parent,
                    pgid: pid.unsigned_abs(),
                    launcher_version: Some(50),
                    tty: None,
                },
                run.map(str::to_owned),
            ),
        );
    }

    /// The sweep a table read takes, each process born `born_ms` (unix ms).
    fn sweep(&self, born_ms: i64) -> Vec<aterm_sysprobe::procs::ProcessReading> {
        self.procs
            .lock()
            .unwrap()
            .iter()
            .map(|(pid, (lin, _))| aterm_sysprobe::procs::ProcessReading {
                pid: pid.unsigned_abs(),
                ppid: lin.parent,
                pgid: lin.pgid,
                uid: Some(UID),
                start: Some(u64::from(pid.unsigned_abs())),
                cpu_ns: Some(0),
                birth_ms: Some(born_ms),
                ..aterm_sysprobe::procs::ProcessReading::default()
            })
            .collect()
    }
}

impl aterm_uds::runs::Kernel for World {
    fn birth(&self, pid: i32) -> Option<aterm_uds::process::Birth> {
        self.lineage(pid).map(|l| l.identity.birth)
    }
    fn presence(&self, pid: i32) -> aterm_uds::process::ProcessPresence {
        if self.procs.lock().unwrap().contains_key(&pid) {
            aterm_uds::process::ProcessPresence::Unknown
        } else {
            aterm_uds::process::ProcessPresence::Absent
        }
    }
    fn lineage(&self, pid: i32) -> Option<aterm_uds::process::Lineage> {
        self.procs.lock().unwrap().get(&pid).map(|(l, _)| *l)
    }
    fn members(&self, pgid: u32) -> Option<aterm_uds::process::GroupMembers> {
        Some(aterm_uds::process::GroupMembers {
            pids: self
                .procs
                .lock()
                .unwrap()
                .iter()
                .filter(|(_, (l, _))| l.pgid == pgid)
                .map(|(pid, _)| *pid)
                .collect(),
            complete: true,
        })
    }
    fn exe(&self, pid: i32) -> Option<PathBuf> {
        self.procs
            .lock()
            .unwrap()
            .contains_key(&pid)
            .then(|| PathBuf::from("/wt/target/debug/ugrep"))
    }
    fn markers(&self, pid: i32) -> Option<aterm_uds::process::Markers> {
        self.procs
            .lock()
            .unwrap()
            .get(&pid)
            .map(|(_, run)| aterm_uds::process::Markers {
                verify_run: run.clone(),
                ..aterm_uds::process::Markers::default()
            })
    }
    fn held(&self, pid: i32) -> Option<bool> {
        self.procs
            .lock()
            .unwrap()
            .contains_key(&pid)
            .then_some(false)
    }
    fn held_now(&self) -> Option<aterm_uds::process::HeldNow> {
        None
    }
    fn launchd_version(&self) -> Option<i32> {
        Some(1)
    }
    fn tty_master_held(&self, _: u32) -> Option<bool> {
        Some(false)
    }
    fn uid_pids(&self) -> Option<Vec<i32>> {
        Some(self.procs.lock().unwrap().keys().copied().collect())
    }
    fn now_ms(&self) -> u64 {
        0
    }
    fn end(
        &self,
        _: aterm_uds::process::Identity,
        _: &dyn Fn(&aterm_uds::process::Identity) -> bool,
    ) -> Result<aterm_uds::process::Ended, aterm_uds::process::EndRefusal> {
        Err(aterm_uds::process::EndRefusal::NotProven)
    }
}

impl aterm_uds::left::LeftKernel for World {
    fn listeners(&self) -> Option<aterm_uds::holders::HolderScan> {
        Some(aterm_uds::holders::HolderScan::default())
    }
    fn sandboxed(&self, _: i32) -> Option<bool> {
        Some(false)
    }
    fn coalition(&self, _: i32) -> Option<u64> {
        None
    }
    fn argv(&self, _: i32) -> Option<Vec<std::ffi::OsString>> {
        None
    }
    fn writer(&self, _: i32) -> bool {
        false
    }
}

/// A verify run's record whose launcher (pid 77) is gone, rooted at `/wt`.
const DEAD_RUN: &str =
    "run v=1 boot=b id=77-1792000000000077 uniqueid=900 start=1 sid=-\nroot path=/wt\n";

/// The table read of `world` at `now_ms`, over the records in `dir`, every process born `born_ms`.
fn table_at(world: &World, dir: &Path, now_ms: u64, born_ms: i64) -> LeftoversFact {
    let sweep = world.sweep(born_ms);
    table_fact(&TableReads {
        kernel: world,
        sweep: &sweep,
        complete: true,
        records: Some(dir),
        boot: Some("b"),
        me: DOCTOR,
        now_ms,
    })
}

/// THE ROW WITHOUT AN INSTANCE APPENDS A DEAD RUN'S `dead_seen` AND WARNS AN HOUR ON: the first
/// table read that finds a run's launcher dead writes the record's one `dead_seen` line, as every
/// reader of the records does, and reads again by the bound it recorded, so it names the run's
/// leftover at once; a later read an hour on counts the hour from that line and names the
/// leftover as the Entry's; each leftover's age is the sweep's birth of it. NEGATIVE CONTROLS:
/// leave the record without its line, and the read an hour on never warns; judge only the reading
/// that recorded the bound, and the first read names none.
#[test]
fn leftovers_row_without_an_instance_appends_dead_seen_and_warns_an_hour_on() {
    use aterm_vitals::left::ENTRY_AFTER_MS;
    let dir = aterm_tempfile::tempdir().expect("scratch rendezvous");
    let record = dir.path().join("77-1792000000000077.run");
    std::fs::write(&record, DEAD_RUN).expect("a record");
    let world = World::new();
    world.add(4300, 1, 1500, Some("77-1792000000000077"));
    let now = 1_792_100_000_000;
    let born = i64::try_from(now - 7_200_000).unwrap();
    let first = table_at(&world, dir.path(), now, born).expect("read");
    assert_eq!(
        (first.rows, first.oldest_ms, first.eligible),
        (1, Some(7_200_000), false),
        "named, its age the sweep's, just found dead"
    );
    let text = std::fs::read_to_string(&record).expect("the record");
    let seen: Vec<&str> = text
        .lines()
        .filter(|l| l.starts_with("dead_seen "))
        .collect();
    assert_eq!(seen.len(), 1, "{text}");
    let later = table_at(&world, dir.path(), now + ENTRY_AFTER_MS, born).expect("read");
    assert!(
        later.eligible,
        "an hour after its launcher was first found dead: {later:?}"
    );
    assert_eq!(row(&Ok(later)).0, Mark::Warn);
    let text = std::fs::read_to_string(&record).expect("the record");
    assert_eq!(
        text.lines().filter(|l| l.starts_with("dead_seen ")).count(),
        1,
        "the record keeps its first line: {text}"
    );
}

/// THE ROW WITHOUT AN INSTANCE IS NOT MEASURED WHEN A RECORD DOES NOT READ: a record that is cut
/// or not a run record hides a dead run whose leftovers would be named, so the row says why
/// instead of `ok`; a folder with no records yet is this boot's none. NEGATIVE CONTROL: read a
/// failed folder as empty, and the row reads `ok`.
#[test]
fn leftovers_row_without_an_instance_is_not_measured_when_a_record_does_not_read() {
    let dir = aterm_tempfile::tempdir().expect("scratch rendezvous");
    let world = World::new();
    let now = 1_792_100_000_000;
    let missing = dir.path().join("verify-runs");
    let none = table_at(&world, &missing, now, 1).expect("no records yet");
    assert_eq!(row(&Ok(none)).0, Mark::Ok);
    std::fs::write(
        dir.path().join("77-1792000000000077.run"),
        "root path=/wt\n",
    )
    .expect("a cut record");
    let why = table_at(&world, dir.path(), now, 1).expect_err("not measured");
    assert!(
        why.starts_with("this boot's verify run records did not read"),
        "{why}"
    );
}
