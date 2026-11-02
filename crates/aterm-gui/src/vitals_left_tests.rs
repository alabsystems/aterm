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
    })
}

/// This instance (pid 100 under pid 90) and tab 7's shell (pid 200, unique id 1000).
fn world() -> Arc<Fake> {
    let fake = Arc::new(Fake::default());
    fake.add(90, 1, 10, None);
    fake.add(i32::try_from(SELF).unwrap(), 90, 20, None);
    fake.add(200, SELF, 1000, Some("s-7"));
    fake
}

/// A request naming tab 7 (session `s-7`, its shell pid 200), or no tab.
fn req(tab: bool) -> Request {
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
        session_ids: tab.then(|| (7, "s-7".to_owned())).into_iter().collect(),
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
/// closed tab; this instance and a pid gone since read as none. NEGATIVE CONTROL: judge it
/// against no closed tab, and the survivor offers nothing.
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
    };
    let again = reread(&readers, &view.live, &view.closed, SELF, 2606, 3_000).expect("read again");
    let again = &again.rows[0];
    assert_eq!(
        (again.tier, again.from.clone(), again.token.clone()),
        (Tier::Offer, Source::Closed(7), shown.token.clone())
    );
    assert!(
        reread(&readers, &view.live, &view.closed, SELF, SELF, 3_000).is_none(),
        "this instance"
    );
    fake.remove(2606);
    assert!(
        reread(&readers, &view.live, &view.closed, SELF, 2606, 3_000).is_none(),
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

/// A held program outside every job is tied to the live tab its group-mate's environment names
/// (its own is never read), for the jobs pass to name it by; its row gets no token.
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
        BTreeMap::from([(2606, 7)]),
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
    assert_eq!(
        pass.ties(),
        BTreeMap::from([(2606, 7), (2607, 7), (2608, 7)]),
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
