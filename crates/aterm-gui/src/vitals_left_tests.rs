// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The leftover pass over a fake kernel and fake readings: a tab's close, its shell's unique id
//! and the horizon, and a dead run's `dead_seen`. Nothing here reads the live table or a real
//! rendezvous.

use super::*;
use aterm_uds::process::{
    Birth, EndRefusal, Ended, GroupMembers, HeldNow, Identity, Lineage, Markers, ProcessPresence,
};
use aterm_vitals::left::{State, Why};
use aterm_vitals::model::TabRoot;
use std::sync::Mutex;

/// The uid every process here runs as.
const ME: u32 = 501;
/// This instance's pid.
const SELF: u32 = 100;

/// One process as the fake lays it out: launched by version 50, never launchd's (1).
#[derive(Clone, Debug)]
struct Proc {
    lin: Lineage,
    markers: Markers,
}

#[derive(Default)]
struct Fake {
    procs: Mutex<BTreeMap<i32, Proc>>,
    /// Pids whose one thread has not run: the kernel's `held` reads them so.
    unran: Mutex<std::collections::BTreeSet<i32>>,
}

impl Fake {
    fn add(&self, pid: i32, parent: u32, uniqueid: u64, sid: Option<&str>) {
        self.procs.lock().unwrap().insert(
            pid,
            Proc {
                lin: Lineage {
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
                markers: Markers {
                    parent_session: sid.map(str::to_owned),
                    ..Markers::default()
                },
            },
        );
    }

    /// Puts `pid` in `pgid`'s group: a pipeline's member, as M12's `ugrep` behind its `tail`.
    fn group(&self, pid: i32, pgid: u32) {
        if let Some(p) = self.procs.lock().unwrap().get_mut(&pid) {
            p.lin.pgid = pgid;
        }
    }

    fn remove(&self, pid: i32) {
        self.procs.lock().unwrap().remove(&pid);
    }

    /// Gives `pid` the parent `parent`: an orphan's adoption once its parent exits.
    fn reparent(&self, pid: i32, parent: u32) {
        if let Some(p) = self.procs.lock().unwrap().get_mut(&pid) {
            p.lin.parent = parent;
        }
    }

    /// The sweep a reading would list.
    fn sweep(&self) -> Vec<ProcessReading> {
        self.procs
            .lock()
            .unwrap()
            .iter()
            .map(|(pid, p)| ProcessReading {
                pid: pid.unsigned_abs(),
                ppid: p.lin.parent,
                pgid: p.lin.pgid,
                uid: Some(ME),
                start: Some(u64::from(pid.unsigned_abs())),
                ..ProcessReading::default()
            })
            .collect()
    }
}

impl aterm_uds::runs::Kernel for Fake {
    fn birth(&self, pid: i32) -> Option<Birth> {
        self.procs
            .lock()
            .unwrap()
            .get(&pid)
            .map(|p| p.lin.identity.birth)
    }
    fn presence(&self, pid: i32) -> ProcessPresence {
        if self.procs.lock().unwrap().contains_key(&pid) {
            ProcessPresence::Unknown
        } else {
            ProcessPresence::Absent
        }
    }
    fn lineage(&self, pid: i32) -> Option<Lineage> {
        self.procs.lock().unwrap().get(&pid).map(|p| p.lin)
    }
    fn members(&self, pgid: u32) -> Option<GroupMembers> {
        Some(GroupMembers {
            pids: self
                .procs
                .lock()
                .unwrap()
                .iter()
                .filter(|(_, p)| p.lin.pgid == pgid)
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
            .then(|| PathBuf::from("/usr/bin/sleep"))
    }
    fn markers(&self, pid: i32) -> Option<Markers> {
        self.procs
            .lock()
            .unwrap()
            .get(&pid)
            .map(|p| p.markers.clone())
    }
    fn held(&self, pid: i32) -> Option<bool> {
        self.procs
            .lock()
            .unwrap()
            .contains_key(&pid)
            .then(|| self.unran.lock().unwrap().contains(&pid))
    }
    fn held_now(&self) -> Option<HeldNow> {
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
    fn end(&self, _: Identity, _: &dyn Fn(&Identity) -> bool) -> Result<Ended, EndRefusal> {
        Err(EndRefusal::NotProven)
    }
}

impl LeftKernel for Fake {
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

/// A pass over `fake`, reading records from `records`.
fn pass(fake: &Arc<Fake>, records: Option<PathBuf>) -> LeftPass {
    LeftPass::with(LeftReaders {
        kernel: Arc::clone(fake) as Arc<dyn LeftKernel + Send + Sync>,
        records,
        boot: || Some("b".to_owned()),
        ..LeftReaders::inert()
    })
}

/// This instance (pid 100 under pid 90) and tab 7's shell (pid 200, unique id 1000).
fn world() -> Arc<Fake> {
    world_as("s-7")
}

/// [`world`], tab 7's session `sid`.
fn world_as(sid: &str) -> Arc<Fake> {
    let fake = Arc::new(Fake::default());
    fake.add(90, 1, 10, None);
    fake.add(i32::try_from(SELF).unwrap(), 90, 20, None);
    fake.add(200, SELF, 1000, Some(sid));
    fake
}

/// A request naming tab 7 (session `s-7`, its shell pid 200), or no tab.
fn req(tab: bool) -> Request {
    led(tab, None)
}

/// [`req`], tab 7 naming `leader` as its agent's group leader.
fn led(tab: bool, leader: Option<u32>) -> Request {
    led_as("s-7", tab, leader)
}

/// [`led`], tab 7's session `sid`.
fn led_as(sid: &str, tab: bool, leader: Option<u32>) -> Request {
    Request {
        self_pid: SELF,
        tabs: tab
            .then(|| TabRoot {
                key: 7,
                tab: 7,
                tab_id: Some(7),
                shell_pid: 200,
                label: "Tab 2".into(),
                program: "zsh".into(),
                agent_leader: leader,
            })
            .into_iter()
            .collect(),
        layout: tab
            .then_some(crate::sys_host::PaneFact {
                key: 7,
                window: 1,
                tab: 2,
            })
            .into_iter()
            .collect(),
        session_ids: tab.then(|| (7, sid.to_owned())).into_iter().collect(),
        ..Request::default()
    }
}

fn tick(pass: &mut LeftPass, fake: &Fake, at_ms: u64, tab: bool) -> Arc<LeftView> {
    let sweep = fake.sweep();
    pass.tick(at_ms, Some(&sweep), true, &req(tab), None, 0)
        .expect("a view")
}

/// A closed tab's count is of the rows `end` takes: a process it left that leads its own group
/// with no terminal, outside every run's folders (daemon-shaped, as a `trustd` is), is listed and
/// named with no token, and not counted. NEGATIVE CONTROL: count every row of the tab, and the
/// daemon is counted.
#[test]
fn a_closed_tabs_count_leaves_out_the_rows_it_offers_no_token() {
    let fake = world();
    let mut pass = pass(&fake, None);
    fake.add(2606, 1, 1500, Some("s-7"));
    fake.group(2606, 2600);
    fake.add(2610, 1, 1400, Some("s-7"));
    tick(&mut pass, &fake, 1_000, true);
    fake.remove(200);
    let closed = tick(&mut pass, &fake, 2_000, false);
    let daemon = closed.rows.iter().find(|r| r.pid == 2610).expect("listed");
    assert_eq!(
        (
            daemon.why,
            daemon.tier,
            daemon.from.clone(),
            daemon.token.as_deref()
        ),
        (Why::Daemon, Tier::Name, Source::Closed(7), None)
    );
    assert_eq!(closed.closed[0].left, 1, "{:?}", closed.rows);
}

/// `end`'s re-proof reads the one pid again on fresh state and judges it against the view's live
/// and closed tabs: a closed tab's survivor still offers the token the view offered, from that
/// closed tab; this instance and a pid gone since read as none, and records that do not read or a
/// table that does not list read as unread, not as gone. NEGATIVE CONTROL: judge it
/// against no closed tab, and the survivor offers nothing; read unreadable records as none, and
/// the survivor reads.
#[test]
fn the_reread_judges_one_pid_again_against_the_views_tabs() {
    let fake = world();
    let mut pass = pass(&fake, None);
    fake.add(2606, 1, 1500, Some("s-7"));
    fake.group(2606, 2600);
    tick(&mut pass, &fake, 1_000, true);
    fake.remove(200);
    let view = tick(&mut pass, &fake, 2_000, false);
    let shown = view
        .rows
        .iter()
        .find(|r| r.pid == 2606)
        .expect("listed")
        .clone();
    assert!(shown.token.is_some(), "{shown:?}");
    let readers = LeftReaders {
        kernel: Arc::clone(&fake) as Arc<dyn LeftKernel + Send + Sync>,
        records: None,
        boot: || Some("b".to_owned()),
        ..LeftReaders::inert()
    };
    let again = reread(&readers, &view.live, &view.closed, SELF, 2606, 3_000)
        .view()
        .expect("read again");
    let again = &again.rows[0];
    assert_eq!(
        (again.tier, again.from.clone(), again.token.clone()),
        (Tier::Offer, Source::Closed(7), shown.token.clone())
    );
    assert_eq!(
        reread(&readers, &view.live, &view.closed, SELF, SELF, 3_000),
        Reread::Read(None),
        "this instance"
    );
    // A rendezvous whose records do not read (a file where the folder should be), and a table
    // that does not list (this instance's own lineage lost), say nothing of the survivor.
    let unreadable = LeftReaders {
        kernel: Arc::clone(&fake) as Arc<dyn LeftKernel + Send + Sync>,
        records: Some(std::env::current_exe().expect("this test binary")),
        boot: || Some("b".to_owned()),
        ..LeftReaders::inert()
    };
    assert_eq!(
        reread(&unreadable, &view.live, &view.closed, SELF, 2606, 3_000),
        Reread::Unread,
        "records that do not read"
    );
    fake.remove(i32::try_from(SELF).expect("a pid"));
    assert_eq!(
        reread(&readers, &view.live, &view.closed, SELF, 2606, 3_000),
        Reread::Unread,
        "a table that does not list"
    );
    fake.add(i32::try_from(SELF).expect("a pid"), 90, 20, None);
    fake.remove(2606);
    assert_eq!(
        reread(&readers, &view.live, &view.closed, SELF, 2606, 3_000),
        Reread::Read(None),
        "gone since"
    );
}

/// Tab 7's survivor (a pipeline's member, its leader gone), born at 1500 while the tab lived, is
/// the closed tab's once the
/// reading that misses the tab records its close: its shell's unique id (1000) opens the window
/// and that reading's horizon (1500, the highest of the table) closes it. A process born after
/// that reading is not the tab's. NEGATIVE CONTROL: take the horizon from the shell's own unique
/// id, and the survivor is not the closed tab's.
#[test]
fn a_closed_tab_keeps_its_shell_and_horizon() {
    let fake = world();
    let mut pass = pass(&fake, None);
    fake.add(2606, 1, 1500, Some("s-7"));
    fake.group(2606, 2600);
    let open = tick(&mut pass, &fake, 1_000, true);
    let row = open.rows.iter().find(|r| r.pid == 2606).expect("listed");
    assert_eq!(
        (row.state, row.why, row.from.clone()),
        (State::Orphaned, Why::Orphaned, Source::Tab(7)),
        "its tab lives"
    );
    fake.remove(200);
    let closed = tick(&mut pass, &fake, 2_000, false);
    assert_eq!(closed.closed.len(), 1, "{:?}", closed.closed);
    let tab = &closed.closed[0];
    assert_eq!(
        (
            tab.key,
            tab.sid.as_str(),
            tab.number,
            tab.shell_uniqueid,
            tab.horizon
        ),
        (7, "s-7", Some(2), 1000, 1500)
    );
    assert_eq!((tab.closed_ms, tab.left), (2_000, 1));
    let row = closed.rows.iter().find(|r| r.pid == 2606).expect("listed");
    assert_eq!(
        (row.state, row.why, row.tier, row.from.clone()),
        (State::Left, Why::OwnerGone, Tier::Offer, Source::Closed(7))
    );
    fake.add(2700, 1, 1600, Some("s-7"));
    fake.group(2700, 2600);
    let later = tick(&mut pass, &fake, 3_000, false);
    let row = later.rows.iter().find(|r| r.pid == 2700).expect("listed");
    assert_eq!(
        (row.why, row.tier, row.from.clone()),
        (Why::Orphaned, Tier::Name, Source::Unknown),
        "born after the reading that missed the tab"
    );
}

/// A tab whose shell was never read keeps no record of its close: with no unique id to open
/// its window, nothing could be proven its.
#[test]
fn a_tab_closed_before_its_shell_was_read_keeps_no_record() {
    let fake = world();
    fake.remove(200);
    let mut pass = pass(&fake, None);
    tick(&mut pass, &fake, 1_000, true);
    let closed = tick(&mut pass, &fake, 2_000, false);
    assert!(closed.closed.is_empty(), "{:?}", closed.closed);
}

/// The first reading that finds a run's launcher dead appends one `dead_seen` line, its
/// horizon; the readings after it append none. NEGATIVE CONTROL: append on every reading that
/// finds the launcher dead, and the record holds two.
#[test]
fn a_dead_record_gets_one_dead_seen_line() {
    let dir = aterm_tempfile::tempdir().expect("scratch rendezvous");
    let path = dir.path().join("77-1792000000000077.run");
    std::fs::write(
        &path,
        "run v=1 boot=b id=77-1792000000000077 uniqueid=900 start=1 sid=-\nroot path=/wt\n",
    )
    .expect("a record");
    let fake = world();
    let mut pass = pass(&fake, Some(dir.path().to_path_buf()));
    for at in [1_000, 2_000, 3_000] {
        let view = tick(&mut pass, &fake, at, true);
        assert_eq!(view.runs.len(), 1, "{:?}", view.runs);
        assert!(view.runs[0].dead, "pid 77 is gone");
    }
    let text = std::fs::read_to_string(&path).expect("the record");
    let seen: Vec<&str> = text
        .lines()
        .filter(|l| l.starts_with("dead_seen "))
        .collect();
    assert_eq!(seen, ["dead_seen uniqueid=1000 at=1000"], "{text}");
}

#[test]
fn a_failed_dead_seen_write_stays_pending_until_it_is_recorded() {
    let dir = aterm_tempfile::tempdir().expect("scratch rendezvous");
    let path = dir.path().join("77-1792000000000077.run");
    let header =
        "run v=1 boot=b id=77-1792000000000077 uniqueid=900 start=1 sid=-\nroot path=/usr/bin\n";
    std::fs::write(&path, header).unwrap();
    let fake = world();
    fake.add(2606, 1, 950, None);
    fake.procs
        .lock()
        .unwrap()
        .get_mut(&2606)
        .unwrap()
        .markers
        .verify_run = Some("77-1792000000000077".to_owned());
    let mut pass = pass(&fake, Some(dir.path().to_path_buf()));
    pass.refresh_records(900);
    assert_eq!(pass.records.list.len(), 1);

    // A failed refresh retains the last admitted record, whose pending horizon must not be
    // marked recorded when the subsequent write is refused too.
    std::fs::remove_file(&path).unwrap();
    std::fs::create_dir(&path).unwrap();
    let failed = tick(&mut pass, &fake, 1_000, true);
    assert_eq!(pass.records.list[0].1.dead_seen, None);
    assert_eq!(pass.records.list[0].1.dead_seen_ms, None);
    assert!(
        failed
            .rows
            .iter()
            .find(|row| row.pid == 2606)
            .unwrap()
            .token
            .is_none()
    );

    std::fs::remove_dir(&path).unwrap();
    std::fs::write(&path, header).unwrap();
    fake.add(2607, 1, 1600, None);
    let recorded = tick(&mut pass, &fake, 2_000, true);
    assert_eq!(pass.records.list[0].1.dead_seen, Some(1600));
    assert_eq!(pass.records.list[0].1.dead_seen_ms, Some(2_000));
    let records = aterm_uds::runs::read_records(dir.path()).unwrap();
    assert_eq!(records[0].1.dead_seen, Some(1600));
    assert_eq!(records[0].1.dead_seen_ms, Some(2_000));
    assert!(
        recorded
            .rows
            .iter()
            .find(|row| row.pid == 2606)
            .unwrap()
            .token
            .is_none()
    );
    let fresh = tick(&mut pass, &fake, 3_000, true);
    assert!(
        fresh
            .rows
            .iter()
            .find(|row| row.pid == 2606)
            .unwrap()
            .token
            .is_some()
    );
}

#[test]
fn a_stale_dead_seen_cache_adopts_the_first_recorded_horizon() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let path = dir.path().join("77-1792000000000077.run");
    std::fs::write(
        &path,
        "run v=1 boot=b id=77-1792000000000077 uniqueid=900 start=1 sid=-\nroot path=/wt\n",
    )
    .unwrap();
    let fake = world();
    let mut pass = pass(&fake, Some(dir.path().to_path_buf()));
    pass.refresh_records(900);
    let stale = aterm_uds::left::RunRead {
        path: path.clone(),
        record: pass.records.list[0].1.clone(),
        dead: true,
    };
    let first = aterm_uds::runs::append_dead_seen(&path, &stale.record, 1200, 1500).unwrap();
    assert_eq!((first.horizon, first.at_ms), (1200, 1500));

    fake.add(2606, 1, 1600, None);
    pass.append_dead_seen(&*fake, SELF, &mut [stale], 2000);
    assert_eq!(pass.records.list[0].1.dead_seen, Some(1200));
    assert_eq!(pass.records.list[0].1.dead_seen_ms, Some(1500));
    let records = aterm_uds::runs::read_records(dir.path()).unwrap();
    assert_eq!(records[0].1.dead_seen, Some(1200));
    assert_eq!(records[0].1.dead_seen_ms, Some(1500));
    assert_eq!(
        std::fs::read_to_string(&path)
            .unwrap()
            .lines()
            .filter(|line| line.starts_with("dead_seen "))
            .count(),
        1
    );
}

/// A held program outside every job is tied to the live tab its group-mate's environment names
/// (its own is never read), for the jobs pass to name it by, and the tie says it was not born
/// after an agent started there (none was seen); its row gets no token.
#[test]
fn a_held_programs_tie_is_handed_to_the_jobs_pass() {
    use aterm_vitals::jobs::{Findings, Named};
    let fake = world();
    fake.add(2606, 1, 1500, None);
    fake.add(2607, 1, 1501, Some("s-7"));
    fake.group(2607, 2606);
    let jobs = JobsView {
        findings: Findings {
            program_check: vec![Named {
                pid: 2606,
                exec_version: None,
                name: "gh".into(),
                tab: None,
                job: None,
                for_ms: 30_000,
            }],
            ..Findings::default()
        },
        ..JobsView::default()
    };
    let mut pass = pass(&fake, None);
    let sweep = fake.sweep();
    let view = pass
        .tick(1_000, Some(&sweep), true, &req(true), Some(&jobs), 0)
        .expect("a view");
    assert_eq!(
        pass.ties(),
        BTreeMap::from([(
            2606,
            left::Tie {
                key: 7,
                closed: false,
                after_agent: false,
            }
        )]),
        "its group-mate's environment names tab 7's session"
    );
    let row = view.rows.iter().find(|r| r.pid == 2606).expect("listed");
    assert_eq!(
        (row.state, row.tier, row.token.as_deref()),
        (State::ProgramCheck, Tier::Name, None)
    );
}

/// A reading the engine did not run on carries the last pass's jobs view, so a held row's
/// `check_ms` is that pass's `for_ms` told on by the view's age at this reading, as `status
/// program_check=` tells it. NEGATIVE CONTROL: copy `for_ms` as the view has it, and the row reads
/// the pass's 30 s while the reading is 25 s past it.
#[test]
fn a_held_rows_age_is_told_on_by_the_carried_jobs_views_age() {
    use aterm_vitals::jobs::{Findings, Named};
    let fake = world();
    fake.add(2606, 1, 1500, None);
    let jobs = JobsView {
        findings: Findings {
            program_check: vec![Named {
                pid: 2606,
                exec_version: None,
                name: "gh".into(),
                tab: None,
                job: None,
                for_ms: 30_000,
            }],
            ..Findings::default()
        },
        ..JobsView::default()
    };
    let check_ms = |age_ms| {
        let mut pass = pass(&fake, None);
        let view = pass
            .tick(
                1_000,
                Some(&fake.sweep()),
                true,
                &req(true),
                Some(&jobs),
                age_ms,
            )
            .expect("a view");
        let row = view.rows.iter().find(|r| r.pid == 2606).expect("listed");
        (row.state, row.check_ms)
    };
    assert_eq!(
        check_ms(0),
        (State::ProgramCheck, Some(30_000)),
        "its own pass"
    );
    assert_eq!(
        check_ms(25_000),
        (State::ProgramCheck, Some(55_000)),
        "a pass 25 s before the reading"
    );
}

/// A program outside every job that has not started for another reason than macOS's program
/// check (translating, on a volume not proven local, or stopped or traced) is tied to the live tab
/// its group-mate's environment names, as a held one is, for the jobs pass to name it by; one the
/// jobs view does not name as unstarted is tied to none. NEGATIVE CONTROL: tie only the rows the
/// view names held, and none is tied.
#[test]
fn an_unstarted_programs_tie_is_handed_to_the_jobs_pass_whatever_keeps_it() {
    use aterm_vitals::jobs::{Findings, Named, StoppedUnstarted};
    let fake = world();
    let named = |pid| Named {
        pid,
        exec_version: None,
        name: "p".into(),
        tab: None,
        job: None,
        for_ms: 30_000,
    };
    for (pid, mate, born) in [(2606, 2616, 1500), (2607, 2617, 1502), (2608, 2618, 1504)] {
        fake.add(pid, 1, born, None);
        fake.add(mate, 1, born + 1, Some("s-7"));
        fake.group(mate, pid.unsigned_abs());
        fake.unran.lock().unwrap().insert(pid);
    }
    fake.add(2609, 1, 1506, None);
    fake.add(2619, 1, 1507, Some("s-7"));
    fake.group(2619, 2609);
    fake.unran.lock().unwrap().insert(2609);
    let jobs = JobsView {
        findings: Findings {
            translating: vec![named(2606)],
            not_started: vec![named(2607)],
            stopped_unstarted: vec![StoppedUnstarted {
                named: named(2608),
                stopped: true,
                traced: true,
            }],
            ..Findings::default()
        },
        ..JobsView::default()
    };
    let mut pass = pass(&fake, None);
    let sweep = fake.sweep();
    pass.tick(1_000, Some(&sweep), true, &req(true), Some(&jobs), 0)
        .expect("a view");
    let tab_7 = left::Tie {
        key: 7,
        closed: false,
        after_agent: false,
    };
    assert_eq!(
        pass.ties(),
        BTreeMap::from([(2606, tab_7), (2607, tab_7), (2608, tab_7)]),
        "each group-mate's environment names tab 7's session; 2609, which the view does not \
         name, is tied to none"
    );
}

/// A dead run's `dead_seen` is the highest unique id of a table of this uid read after the
/// reading found its launcher dead, never of the sweep the reading began with: a process the
/// run started between the two is inside its window. NEGATIVE CONTROL: write the sweep's
/// horizon, and that process is outside it.
#[test]
fn a_dead_runs_horizon_is_read_after_its_launcher_is_found_dead() {
    let dir = aterm_tempfile::tempdir().expect("scratch rendezvous");
    let path = dir.path().join("77-1792000000000077.run");
    std::fs::write(
        &path,
        "run v=1 boot=b id=77-1792000000000077 uniqueid=900 start=1 sid=-\nroot path=/wt\n",
    )
    .expect("a record");
    let fake = world();
    let mut pass = pass(&fake, Some(dir.path().to_path_buf()));
    let sweep = fake.sweep();
    fake.add(2606, 1, 1600, None);
    pass.tick(1_000, Some(&sweep), true, &req(true), None, 0)
        .expect("a view");
    let text = std::fs::read_to_string(&path).expect("the record");
    let seen: Vec<&str> = text
        .lines()
        .filter(|l| l.starts_with("dead_seen "))
        .collect();
    assert_eq!(seen, ["dead_seen uniqueid=1600 at=1000"], "{text}");
}

/// Tab 7 whose agent (pid 300, unique id 1200, the shell's child) the host names, with what it
/// left: a `nohup` its short call started at 1500, which no jobs pass saw as an agent call, and the
/// person's own from 1100, before the agent, each left in the group of the shell that started it;
/// read at [`AT_MS`].
fn agents_tab(leader: Option<u32>) -> (Arc<Fake>, LeftPass, Arc<LeftView>) {
    let fake = world();
    fake.add(300, 200, 1200, None);
    fake.add(2606, 1, 1500, Some("s-7"));
    fake.group(2606, 2600);
    fake.add(2607, 1, 1100, Some("s-7"));
    fake.group(2607, 2500);
    fake.add(2608, 1, 1600, None);
    fake.group(2608, 2600);
    let mut pass = pass(&fake, None);
    let sweep = fake.sweep();
    let view = pass
        .tick(AT_MS, Some(&sweep), true, &led(true, leader), None, 0)
        .expect("a view");
    (fake, pass, view)
}

/// When [`agents_tab`] reads, unix ms: a minute after the fake's births.
const AT_MS: u64 = 1_792_000_060_000;

/// Each row's tier, source and whether it was born while an agent ran in its tab.
fn after(view: &LeftView, pid: u32) -> (Tier, Source, bool) {
    let row = view.rows.iter().find(|r| r.pid == pid).expect("listed");
    (row.tier, row.from.clone(), row.after_agent)
}

/// An agent stretch still open, from `since`.
const fn open_span(since: u64) -> left::AgentSpan {
    left::AgentSpan { since, until: None }
}

/// THE AGENT'S OWN START OPENS ITS WINDOW: with no agent call seen in tab 7, the agent's group
/// leader the host names dates the tab's agent start, so the `nohup` its short call left reads as
/// born after the agent and the person's older one does not; the tab's close keeps the start.
/// NEGATIVE CONTROL: fold no named leader into the tab's agent start, and the agent's `nohup`
/// reads as born before it.
#[test]
fn the_agents_own_start_opens_its_window() {
    let (fake, mut pass, view) = agents_tab(Some(300));
    assert_eq!(after(&view, 2606), (Tier::Offer, Source::Tab(7), true));
    assert_eq!(after(&view, 2607), (Tier::Offer, Source::Tab(7), false));
    assert_eq!(view.live[0].agent_spans, [open_span(1200)]);
    fake.remove(200);
    let closed = tick(&mut pass, &fake, AT_MS + 1_000, false);
    assert_eq!(closed.closed[0].agent_spans, [open_span(1200)]);
}

/// AN AGENT GONE FROM ITS TAB CLOSES ITS WINDOW: tab 7's agent (unique id 1200) leaves its
/// `nohup` (1500) and exits; the next reading names no agent there, so its stretch closes at that
/// reading's horizon. The server the person then starts by hand (1600) is born outside every
/// stretch, so when a second agent (2000) starts in the same shell the server reads as the
/// person's: not the agent's left row, while the first agent's `nohup` still is. NEGATIVE CONTROL:
/// keep the stretch open when a reading names no agent (the earliest agent start alone), and the
/// person's server reads as born after the agent and counts in its `left=`.
#[test]
fn an_agent_gone_from_its_tab_closes_its_window() {
    let fake = world();
    fake.add(300, 200, 1200, None);
    fake.add(2606, 1, 1500, Some("s-7"));
    fake.group(2606, 2600);
    let mut pass = pass(&fake, None);
    let read = |pass: &mut LeftPass, at_ms, leader| {
        let sweep = fake.sweep();
        pass.tick(at_ms, Some(&sweep), true, &led(true, leader), None, 0)
            .expect("a view")
    };
    read(&mut pass, AT_MS, Some(300));
    fake.remove(300);
    let gone = read(&mut pass, AT_MS + 1_000, None);
    assert_eq!(
        gone.live[0].agent_spans,
        [left::AgentSpan {
            since: 1200,
            until: Some(1500)
        }],
        "closed at the horizon of the reading that named no agent"
    );
    fake.add(2608, 1, 1600, Some("s-7"));
    fake.group(2608, 2500);
    fake.add(400, 200, 2000, None);
    let view = read(&mut pass, AT_MS + 2_000, Some(400));
    assert_eq!(after(&view, 2606), (Tier::Offer, Source::Tab(7), true));
    assert_eq!(after(&view, 2608), (Tier::Offer, Source::Tab(7), false));
    assert_eq!(
        aterm_vitals::left::agents_left(&view, "s-7")
            .iter()
            .map(|r| r.pid)
            .collect::<Vec<_>>(),
        [2606],
        "the person's server is not the agent's"
    );
    assert!(
        crate::sys_host::status_left(Some(&view), None, true, Some("s-7"), AT_MS + 2_000)
            .starts_with("left=1:")
    );
}

/// [`agents_tab`]'s view with the agent's `nohup` aged 60 s at the reading, whatever the kernel
/// here reads of ages.
fn aged(view: &LeftView) -> LeftView {
    let mut aged = view.clone();
    for row in aged.rows.iter_mut().filter(|r| r.pid == 2606) {
        row.started_ms = Some(60_000);
    }
    aged
}

/// Every left row proves alive at the ask.
fn alive(_: &str) -> bool {
    true
}

/// ONE VIEW COUNTS THE AGENT'S LEFT ROWS ALIKE for `status left=` and the leftover fact: tab 7's
/// view holds the agent's `nohup`, the person's older one and a process nothing ties (named),
/// and both read one row, aged alike to now, the fact's sentence naming no path or executable; a
/// view of the person's `nohup` and the untied process alone reads none in both, and no view reads
/// `left=-`. NEGATIVE CONTROL: have `status` count the tab's offered rows, and it reads two while
/// the fact reads one; leave the view's age out of `status`, and its age is the reading's.
#[test]
fn one_view_counts_left_alike_for_status_and_the_fact() {
    let (_fake, _pass, view) = agents_tab(Some(300));
    let view = aged(&view);
    let now = AT_MS + 5_000;
    let status = crate::sys_host::status_left(Some(&view), None, true, Some("s-7"), now);
    let fact = crate::vitals_jobs::leftover_reading(&view, "s-7", now.try_into().unwrap(), alive)
        .expect("a whole view");
    assert_eq!(status, "left=1:65000");
    assert_eq!((fact.count, fact.oldest_ms), (1, Some(65_000)), "{fact:?}");
    let told = fact.told();
    assert!(!told.contains('/') && !told.contains("sleep"), "{told}");

    let person = LeftView {
        rows: view
            .rows
            .iter()
            .filter(|r| r.pid != 2606)
            .cloned()
            .collect(),
        ..view.clone()
    };
    assert_eq!(person.rows.len(), 2, "{:?}", person.rows);
    assert_eq!(
        crate::sys_host::status_left(Some(&person), None, true, Some("s-7"), AT_MS),
        "left=0:-"
    );
    assert_eq!(
        crate::vitals_jobs::leftover_reading(&person, "s-7", AT_MS.try_into().unwrap(), alive)
            .map(|f| f.count),
        Some(0)
    );
    assert_eq!(
        crate::sys_host::status_left(None, None, true, Some("s-7"), AT_MS),
        "left=-"
    );
}

/// A PARTIAL VIEW IS UNREAD ALIKE for `status left=` and the leftover fact: the same view whose
/// sweep stopped at its cap, or whose reading stopped at its bound of new candidates, reads
/// `left=-` and no fact part, whether it names the agent's row or none, so it neither says none is
/// left nor closes an episode. NEGATIVE CONTROL: count a partial view, and its `0` and `1` read as
/// whole.
#[test]
fn a_partial_view_is_unread_alike_for_status_and_the_fact() {
    let (_fake, _pass, view) = agents_tab(Some(300));
    let none = LeftView {
        rows: view
            .rows
            .iter()
            .filter(|r| r.pid != 2606)
            .cloned()
            .collect(),
        ..(*view).clone()
    };
    let ms: i64 = AT_MS.try_into().unwrap();
    for whole in [(*view).clone(), none] {
        for partial in [
            LeftView {
                complete: false,
                ..whole.clone()
            },
            LeftView {
                bounded: true,
                ..whole.clone()
            },
        ] {
            assert_eq!(
                crate::sys_host::status_left(Some(&partial), None, true, Some("s-7"), AT_MS),
                "left=-",
                "{partial:?}"
            );
            assert_eq!(
                crate::vitals_jobs::leftover_reading(&partial, "s-7", ms, alive),
                None
            );
        }
        assert_ne!(
            crate::sys_host::status_left(Some(&whole), None, true, Some("s-7"), AT_MS),
            "left=-",
            "the same view whole is read"
        );
    }
}

/// A VIEW NAMING AN AGENT'S LEFT ROW SPEAKS FOR A MINUTE: it is read again on the slow cadence
/// ([`aterm_vitals::left::holds_an_offer`]), so a minute past its reading its reader has stopped
/// delivering and `status` reads `left=-` rather than a count that may name a process gone since;
/// a view naming none, which no timer reads again, speaks on while its reader runs and not once it
/// stops. NEGATIVE CONTROL: gate `left=` by nothing, and the minute-old count is printed.
#[test]
fn a_stale_left_view_reads_unread() {
    let (_fake, _pass, view) = agents_tab(Some(300));
    assert!(aterm_vitals::left::holds_an_offer(&view));
    let fresh = AT_MS + u64::try_from(crate::vitals_jobs::JOBS_FRESH.as_millis()).unwrap();
    let status = |v: &LeftView, reader, now| {
        crate::sys_host::status_left(Some(v), None, reader, Some("s-7"), now)
    };
    assert!(status(&view, true, fresh).starts_with("left=1:"));
    assert_eq!(status(&view, true, fresh + 1), "left=-");
    let none = LeftView {
        rows: view
            .rows
            .iter()
            .filter(|r| r.pid != 2606)
            .cloned()
            .collect(),
        ..(*view).clone()
    };
    assert!(!aterm_vitals::left::holds_an_offer(&none));
    assert_eq!(status(&none, true, fresh + 600_000), "left=0:-");
    assert_eq!(status(&none, false, fresh + 1), "left=-");
}

/// THE FACT'S LEFT PART IS AS OF THE ASK: each of the agent's rows is re-proved by its token at the
/// ask, so a view 20 s old gives a part stamped now with the oldest's age told on to now, which a
/// turn end may tell; a row the re-proof finds gone (ended, or its pid another process's) is not
/// counted, and none proved is a part unread, never one with none. NEGATIVE CONTROL: stamp the
/// part with the view's own time, and a 20 s old view's part is past the tell gate.
#[test]
fn the_facts_left_part_is_as_of_the_ask() {
    let (_fake, _pass, view) = agents_tab(Some(300));
    let view = aged(&view);
    let now: i64 = (AT_MS + 20_000).try_into().unwrap();
    let part = crate::vitals_jobs::leftover_reading(&view, "s-7", now, alive).expect("proved");
    assert_eq!(
        (part.as_of_ms, part.count, part.oldest_ms),
        (now, 1, Some(80_000))
    );
    assert!(part.tells_at(now));
    fn gone(_: &str) -> bool {
        false
    }
    assert_eq!(
        crate::vitals_jobs::leftover_reading(&view, "s-7", now, gone),
        None
    );
}

/// Tab 7's session id in the closed-tab tests, of the shape aterm mints: the record keeps no
/// other.
const SID: &str = "s-0000000000000000a007";

/// When the closed-tab tests first read, unix ms.
const T0: u64 = 1_792_000_060_000;

/// A pass over `fake` in boot `b` whose closed-tab record is `file`: one aterm's run on a state
/// home.
fn kept_pass(fake: &Arc<Fake>, file: &std::path::Path) -> LeftPass {
    LeftPass::with(LeftReaders {
        kernel: Arc::clone(fake) as Arc<dyn LeftKernel + Send + Sync>,
        boot: || Some("b".to_owned()),
        closed: Some(file.to_path_buf()),
        ..LeftReaders::inert()
    })
}

/// A scratch state home and its closed-tab record's path.
fn state_home() -> (aterm_tempfile::TempDir, PathBuf) {
    let dir = aterm_tempfile::tempdir().expect("a scratch state home");
    let file = dir.path().join(crate::vitals_left_closed::FILE);
    (dir, file)
}

/// A reading of `fake` with tab 7 (session [`SID`]) open where `tab`.
fn tick_as(pass: &mut LeftPass, fake: &Fake, at_ms: u64, tab: bool) -> Arc<LeftView> {
    let sweep = fake.sweep();
    pass.tick(at_ms, Some(&sweep), true, &led_as(SID, tab, None), None, 0)
        .expect("a view")
}

/// `pid`'s row as `sys left` judges it, its source aside: its state, why, tier, proof words and
/// token.
fn judged(view: &LeftView, pid: u32) -> (State, Why, Tier, Vec<left::Proof>, Option<String>) {
    let row = view.rows.iter().find(|r| r.pid == pid).expect("listed");
    (
        row.state,
        row.why,
        row.tier,
        row.proof.clone(),
        row.token.clone(),
    )
}

/// `pid`'s row's tier, source and token.
fn offered(view: &LeftView, pid: u32) -> (Tier, Source, Option<String>) {
    let row = view.rows.iter().find(|r| r.pid == pid).expect("listed");
    (row.tier, row.from.clone(), row.token.clone())
}

/// A CLOSED TAB'S ROW READS ALIKE AFTER A RESTART: tab 7 closes and leaves a pipeline's member
/// (born at 1500, its leader gone); the aterm that hosted it quits, and one that starts again in
/// the same boot on the same state home gives the row the same state, why, tier, proof words and
/// token, from the same close (its session, label, number, window and time), and `end`'s re-read
/// judges it alike. NEGATIVE CONTROL: read no record at the start, and the row is named with no
/// token.
#[test]
fn a_closed_tabs_row_reads_alike_after_a_restart() {
    let (_home, file) = state_home();
    let fake = world_as(SID);
    fake.add(2606, 1, 1500, Some(SID));
    fake.group(2606, 2600);
    let mut first = kept_pass(&fake, &file);
    tick_as(&mut first, &fake, T0, true);
    fake.remove(200);
    let before = tick_as(&mut first, &fake, T0 + 1_000, false);
    assert_eq!(offered(&before, 2606).0, Tier::Offer, "{:?}", before.rows);
    first.at_stop(&[], T0 + 2_000);
    drop(first);

    let mut again = kept_pass(&fake, &file);
    let after = tick_as(&mut again, &fake, T0 + 5_000, false);
    assert_eq!(judged(&after, 2606), judged(&before, 2606));
    assert_eq!(
        offered(&after, 2606).1,
        Source::Closed(RESTORED_KEYS),
        "a key no session of this process has"
    );
    let (was, now) = (&before.closed[0], &after.closed[0]);
    assert_eq!(
        (
            now.sid.as_str(),
            now.label.as_str(),
            now.number,
            now.shell_uniqueid,
            now.horizon,
            now.closed_ms,
            now.left
        ),
        (
            was.sid.as_str(),
            was.label.as_str(),
            was.number,
            was.shell_uniqueid,
            was.horizon,
            was.closed_ms,
            was.left
        )
    );
    assert!(now.restored && !was.restored);
    let readers = LeftReaders {
        kernel: Arc::clone(&fake) as Arc<dyn LeftKernel + Send + Sync>,
        boot: || Some("b".to_owned()),
        ..LeftReaders::inert()
    };
    let fresh = reread(&readers, &after.live, &after.closed, SELF, 2606, T0 + 6_000)
        .view()
        .expect("read again");
    assert_eq!(offered(&fresh, 2606), offered(&after, 2606));
}

/// A TAB OPEN AT THE QUIT IS CLOSED AT THE QUIT: tab 7 holds an orphan its session started (born
/// at 1500, offered from the live tab) when the probe thread stops; the stop records the tab
/// closed then, at the high-water unique id of the table it reads then (1700, past the last
/// reading's 1500), and the shell goes with the aterm. The aterm that starts again reads the
/// orphan as that tab's leftover, owner gone, with the same token; a process the session's marker
/// names that was born after the quit's horizon is not the tab's. NEGATIVE CONTROL: close no live
/// tab at the stop, and the orphan is named with no token.
#[test]
fn a_tab_open_at_the_quit_is_closed_at_the_quit() {
    let (_home, file) = state_home();
    let fake = world_as(SID);
    fake.add(2606, 1, 1500, Some(SID));
    fake.group(2606, 2600);
    let mut first = kept_pass(&fake, &file);
    let open = tick_as(&mut first, &fake, T0, true);
    let (state, why, tier, proof, token) = judged(&open, 2606);
    assert_eq!(
        (state, why, tier, offered(&open, 2606).1),
        (State::Orphaned, Why::Orphaned, Tier::Offer, Source::Tab(7))
    );
    fake.add(2700, 90, 1700, None);
    first.at_stop(&[], T0 + 1_000);
    drop(first);
    fake.remove(200);
    fake.add(2800, 1, 1800, Some(SID));
    fake.group(2800, 2600);

    let mut again = kept_pass(&fake, &file);
    let after = tick_as(&mut again, &fake, T0 + 2_000, false);
    assert_eq!(
        judged(&after, 2606),
        (State::Left, Why::OwnerGone, Tier::Offer, proof, token)
    );
    assert_eq!(offered(&after, 2606).1, Source::Closed(RESTORED_KEYS));
    let tab = &after.closed[0];
    assert_eq!(
        (
            tab.sid.as_str(),
            tab.closed_ms,
            tab.shell_uniqueid,
            tab.horizon
        ),
        (SID, T0 + 1_000, 1000, 1700)
    );
    assert_eq!(
        offered(&after, 2800),
        (Tier::Name, Source::Unknown, None),
        "born after the quit"
    );
}

/// A QUIT'S RECORD WAITS WHILE ITS SHELL RUNS: tab 7's shell outlives the aterm that recorded the
/// tab closed at its quit (it survived the quit's hangup), so the session it names is not gone. An
/// aterm that starts while that shell runs does not take the record, and keeps it in the file when
/// it writes a close of its own; one that starts once the shell is gone takes it. NEGATIVE
/// CONTROL: take a quit's record whatever its shell reads, and the running tab's orphan reads as a
/// closed tab's leftover.
#[test]
fn a_quits_record_waits_while_its_shell_runs() {
    let (_home, file) = state_home();
    let fake = world_as(SID);
    fake.add(2606, 1, 1500, Some(SID));
    fake.group(2606, 2600);
    let mut first = kept_pass(&fake, &file);
    tick_as(&mut first, &fake, T0, true);
    first.at_stop(&[], T0 + 1_000);
    drop(first);

    let other = "s-0000000000000000a00c";
    fake.add(201, SELF, 1700, Some(other));
    let mut during = kept_pass(&fake, &file);
    let sweep = fake.sweep();
    let view = during
        .tick(T0 + 2_000, Some(&sweep), true, &tab_8(other), None, 0)
        .expect("a view");
    assert!(view.closed.is_empty(), "{:?}", view.closed);
    assert_eq!(offered(&view, 2606), (Tier::Name, Source::Unknown, None));
    during.at_stop(&[], T0 + 2_500);
    drop(during);

    fake.remove(200);
    fake.remove(201);
    let mut later = kept_pass(&fake, &file);
    let view = tick_as(&mut later, &fake, T0 + 3_000, false);
    let sids: Vec<&str> = view.closed.iter().map(|t| t.sid.as_str()).collect();
    assert_eq!(sids, [SID, other], "both quits, oldest first");
    assert_eq!(
        offered(&view, 2606).1,
        Source::Closed(RESTORED_KEYS),
        "{:?}",
        view.rows
    );
}

/// A MARKER NAMING A SESSION NO RECORDED ATERM OF THIS USER HOSTED STAYS NAMED: an orphan whose
/// marker names a session no record holds, and one naming a session recorded only in another
/// boot's file (whose unique ids say nothing of this boot's), each born inside that record's
/// window, read `name` with no token, tied to nothing. NEGATIVE CONTROL: read a record of another
/// boot, and the second offers a token.
#[test]
fn a_marker_naming_no_recorded_session_stays_named() {
    let (_home, file) = state_home();
    let other = "s-0000000000000000a008";
    let elsewhere = Kept {
        tab: ClosedTab {
            sid: other.to_owned(),
            label: "Tab 3".into(),
            number: Some(3),
            shell_uniqueid: 1000,
            horizon: 2000,
            closed_ms: T0 - 1_000,
            ..ClosedTab::default()
        },
        shell_pid: 300,
        ..Kept::default()
    };
    crate::vitals_left_closed::write(&file, "a", [&elsewhere], T0).expect("another boot's");
    let fake = world_as(SID);
    fake.add(2606, 1, 1500, Some(other));
    fake.group(2606, 2600);
    fake.add(2607, 1, 1600, Some("s-0000000000000000a009"));
    fake.group(2607, 2600);
    let mut pass = kept_pass(&fake, &file);
    let view = tick_as(&mut pass, &fake, T0, false);
    assert!(view.closed.is_empty(), "{:?}", view.closed);
    for pid in [2606, 2607] {
        assert_eq!(
            offered(&view, pid),
            (Tier::Name, Source::Unknown, None),
            "pid {pid}"
        );
    }
}

/// A CLOSE PAST THE BOUND IS DROPPED, in memory as on disk: 30 days and a millisecond after tab 7
/// closed, the reading drops its close, so its survivor is named with no token, and an aterm that
/// starts again then reads no close either. NEGATIVE CONTROL: keep closes in memory whatever their
/// age, and the survivor still offers a token.
#[test]
fn a_close_past_the_bound_is_dropped() {
    let (_home, file) = state_home();
    let fake = world_as(SID);
    fake.add(2606, 1, 1500, Some(SID));
    fake.group(2606, 2600);
    let mut pass = kept_pass(&fake, &file);
    tick_as(&mut pass, &fake, T0, true);
    fake.remove(200);
    let closed = tick_as(&mut pass, &fake, T0 + 1_000, false);
    assert_eq!(offered(&closed, 2606).0, Tier::Offer);
    let past = T0 + 1_000 + crate::vitals_left_closed::CLOSED_KEPT_MS + 1;
    let late = tick_as(&mut pass, &fake, past, false);
    assert!(late.closed.is_empty(), "{:?}", late.closed);
    assert_eq!(offered(&late, 2606), (Tier::Name, Source::Unknown, None));
    let mut again = kept_pass(&fake, &file);
    let view = tick_as(&mut again, &fake, past, false);
    assert!(view.closed.is_empty(), "{:?}", view.closed);
}

/// A CLOSE TOLD BEFORE A RESTART IS NOT TOLD AGAIN: the reading that records tab 7's close counts
/// the process it left, its host tells of it, and the stop takes that telling, so the record says
/// the close was told and the aterm that starts again makes no second `proc.left` record of it; a
/// tab that closed at the quit was never told, and is. NEGATIVE CONTROL: tell every close a view
/// holds with processes left, and the first close is told again.
#[test]
fn a_close_told_before_a_restart_is_not_told_again() {
    let (_home, file) = state_home();
    let fake = world_as(SID);
    fake.add(2606, 1, 1500, Some(SID));
    fake.group(2606, 2600);
    let mut first = kept_pass(&fake, &file);
    tick_as(&mut first, &fake, T0, true);
    fake.remove(200);
    let closed = tick_as(&mut first, &fake, T0 + 1_000, false);
    assert_eq!(
        told_now(&closed),
        [SID],
        "told by the aterm that saw the close"
    );
    first.at_stop(&[closed.closed[0].key], T0 + 1_500);
    drop(first);
    let other = "s-0000000000000000a00b";
    fake.add(201, SELF, 1700, Some(other));
    fake.add(2607, 1, 1800, Some(other));
    fake.group(2607, 2601);
    let mut second = kept_pass(&fake, &file);
    let sweep = fake.sweep();
    second
        .tick(T0 + 2_000, Some(&sweep), true, &tab_8(other), None, 0)
        .expect("a view");
    second.at_stop(&[], T0 + 3_000);
    drop(second);
    fake.remove(201);

    let mut third = kept_pass(&fake, &file);
    let view = tick_as(&mut third, &fake, T0 + 4_000, false);
    assert_eq!(told_now(&view), [other], "{:?}", view.closed);
}

/// A reading's request with tab 8 open, session `sid`, its shell pid 201, and no tab 7.
fn tab_8(sid: &str) -> Request {
    Request {
        tabs: vec![TabRoot {
            key: 8,
            tab: 8,
            tab_id: Some(8),
            shell_pid: 201,
            label: "Tab 4".into(),
            program: "zsh".into(),
            agent_leader: None,
        }],
        session_ids: vec![(8, sid.to_owned())],
        ..led_as(SID, false, None)
    }
}

/// The sessions of the closes `view` holds that the host tells of now, as a host that never told
/// one would ([`crate::message_reporters::newly_left`]).
fn told_now(view: &LeftView) -> Vec<String> {
    let mut told = std::collections::BTreeSet::new();
    crate::message_reporters::newly_left(&mut told, view)
        .iter()
        .map(|t| t.sid.clone())
        .collect()
}

/// A CLOSE WITH AN EMPTY WINDOW SPOILS NO RECORD: tab 7 closes and leaves an orphan (born at
/// 1500); tab 8 (its shell's unique id 3000) then closes with no process newer than 1500 alive, so
/// the reading that misses it reads a horizon below its shell and its window holds nothing. The
/// aterm that starts again still offers tab 7's survivor from its close. NEGATIVE CONTROL: write
/// every close the table holds, and the record reads as none, so the survivor is named with no
/// token.
#[test]
fn a_close_with_an_empty_window_spoils_no_record() {
    let (_home, file) = state_home();
    let fake = world_as(SID);
    fake.add(2606, 1, 1500, Some(SID));
    fake.group(2606, 2600);
    let mut first = kept_pass(&fake, &file);
    tick_as(&mut first, &fake, T0, true);
    fake.remove(200);
    let closed = tick_as(&mut first, &fake, T0 + 1_000, false);
    assert_eq!(offered(&closed, 2606).0, Tier::Offer, "{:?}", closed.rows);
    let other = "s-0000000000000000a00d";
    fake.add(201, SELF, 3000, Some(other));
    let sweep = fake.sweep();
    first
        .tick(T0 + 2_000, Some(&sweep), true, &tab_8(other), None, 0)
        .expect("a view");
    fake.remove(201);
    let view = tick_as(&mut first, &fake, T0 + 3_000, false);
    let empty = view.closed.iter().find(|t| t.sid == other).expect("kept");
    assert!(empty.horizon < empty.shell_uniqueid, "{empty:?}");
    first.at_stop(&[], T0 + 4_000);
    drop(first);

    let mut again = kept_pass(&fake, &file);
    let after = tick_as(&mut again, &fake, T0 + 5_000, false);
    assert_eq!(
        offered(&after, 2606).1,
        Source::Closed(RESTORED_KEYS),
        "{:?}",
        after.rows
    );
}

/// A CLOSE ITS HOST NEVER TOLD IS TOLD BY THE NEXT ATERM: the reading that records tab 7's close
/// counts the process it left, but the aterm quits before its host adopts that reading, so no
/// `proc.left` record told of it; the aterm that starts again tells of it. NEGATIVE CONTROL: write
/// a close told once a reading counted what it left, and the next aterm tells none.
#[test]
fn a_close_its_host_never_told_is_told_by_the_next_aterm() {
    let (_home, file) = state_home();
    let fake = world_as(SID);
    fake.add(2606, 1, 1500, Some(SID));
    fake.group(2606, 2600);
    let mut first = kept_pass(&fake, &file);
    tick_as(&mut first, &fake, T0, true);
    fake.remove(200);
    let closed = tick_as(&mut first, &fake, T0 + 1_000, false);
    assert_eq!(closed.closed[0].left, 1, "counted at the close");
    first.at_stop(&[], T0 + 1_500);
    drop(first);

    let mut again = kept_pass(&fake, &file);
    let view = tick_as(&mut again, &fake, T0 + 2_000, false);
    assert_eq!(told_now(&view), [SID], "{:?}", view.closed);
}

/// A CLOSE ITS HOST TOLD IS NOT TOLD AGAIN AFTER AN EXIT WITH NO STOP: tab 7's survivor (born at
/// 1500) is still its pipeline's child when the close is recorded, so that reading counts nothing
/// left; the next finds it orphaned and counts it, its host tells of it and says so to the pass,
/// and the aterm exits as an update's predecessor does at its Commit, with no stop. The aterm that
/// starts again tells none. NEGATIVE CONTROL: say nothing to the pass of the host's telling, and
/// the close is told again.
#[test]
fn a_close_its_host_told_is_not_told_again_after_an_exit() {
    let (_home, file) = state_home();
    let fake = world_as(SID);
    fake.add(2605, 200, 1400, Some(SID));
    fake.group(2605, 2600);
    fake.add(2606, 2605, 1500, Some(SID));
    fake.group(2606, 2600);
    let mut first = kept_pass(&fake, &file);
    tick_as(&mut first, &fake, T0, true);
    fake.remove(200);
    let closed = tick_as(&mut first, &fake, T0 + 1_000, false);
    assert_eq!(closed.closed[0].left, 0, "{:?}", closed.rows);
    fake.remove(2605);
    fake.reparent(2606, 1);
    let left = tick_as(&mut first, &fake, T0 + 2_000, false);
    assert_eq!(told_now(&left), [SID], "{:?}", left.rows);
    first.keep_told(&[left.closed[0].key], T0 + 2_500);
    drop(first);

    let mut again = kept_pass(&fake, &file);
    let view = tick_as(&mut again, &fake, T0 + 3_000, false);
    assert_eq!(offered(&view, 2606).1, Source::Closed(RESTORED_KEYS));
    assert!(told_now(&view).is_empty(), "{:?}", view.closed);
}

/// A QUIT'S RECORD IS TAKEN ONCE ITS SHELL'S PID NAMES ANOTHER PROCESS: tab 7 is open at the
/// quit, its shell goes, and its pid is taken again by a process born later (unique id 1900); the
/// aterm that starts then takes the quit's record. NEGATIVE CONTROL: read a pid that names any
/// process as the shell, whatever its unique id, and the record is held back.
#[test]
fn a_quits_record_is_taken_once_its_shells_pid_is_reused() {
    let (_home, file) = state_home();
    let fake = world_as(SID);
    fake.add(2606, 1, 1500, Some(SID));
    fake.group(2606, 2600);
    let mut first = kept_pass(&fake, &file);
    tick_as(&mut first, &fake, T0, true);
    first.at_stop(&[], T0 + 1_000);
    drop(first);
    fake.remove(200);
    fake.add(200, SELF, 1900, None);

    let mut again = kept_pass(&fake, &file);
    let view = tick_as(&mut again, &fake, T0 + 2_000, false);
    assert_eq!(
        offered(&view, 2606).1,
        Source::Closed(RESTORED_KEYS),
        "{:?}",
        view.closed
    );
}
