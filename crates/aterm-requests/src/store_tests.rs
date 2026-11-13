// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::why::why_of;

/// 2026-11-09 14:11:00 UTC.
const AT: u64 = 1_794_233_460_000;
const NODE: &str = "87be5228c0ffee0087be5228c0ffee00";
/// Another Mac whose node id begins as [`NODE`]'s does, so its drive's actor token is the same.
const TWIN: &str = "87be5228ffffffffffffffffffffffff";
const ASKER: &str = "s-0123456789abcdef0123";
const BY: &str = "fleet:87be5228:s-0123456789abcdef0123";
const TAB_A: &str = "s-aaaaaaaaaaaaaaaaaaaa";
const TAB_B: &str = "s-bbbbbbbbbbbbbbbbbbbb";
const TAB_C: &str = "s-cccccccccccccccccccc";
const TAB_D: &str = "s-dddddddddddddddddddd";
const TAB_E: &str = "s-eeeeeeeeeeeeeeeeeeee";

fn fleet_shell(by: &str, why: &str) -> Intake {
    Intake {
        via: Via::FleetShell,
        form: Form::Shell,
        from: From {
            node: NODE.to_owned(),
            host: "m114".to_owned(),
            by: by.to_owned(),
            program: Some("claude".to_owned()),
            model: Some("opus-5.5".to_owned()),
            account: Some("a…@example.com".to_owned()),
        },
        why: why_of(Via::FleetShell, Some(why), None, None, None),
        brief: None,
        goal: None,
        asked: None,
    }
}

fn journal(dir: &Path) -> Vec<u8> {
    std::fs::read(dir.join(crate::JOURNAL)).unwrap_or_default()
}

fn closed_by(by: &str, reason: &str, code: Option<i32>) -> Left {
    Left {
        reason: reason.to_owned(),
        code,
        task: None,
        by: by.to_owned(),
        node: Some(NODE.to_owned()),
        spent: None,
    }
}

#[test]
fn a_request_survives_the_process_that_made_it() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let id = {
        let mut a = Store::open(dir.path()).unwrap();
        let id = a
            .open_request(AT, fleet_shell(BY, "qualify trust-ir"))
            .unwrap();
        assert_eq!(id, "r-20261109-1411-87be5228-01");
        assert_eq!(a.get(&id).unwrap().state(), State::Open);
        a.tab(AT + 1, &id, TAB_A, BY, false, None).unwrap();
        a.tab(AT + 2, &id, TAB_B, BY, false, None).unwrap();
        assert!(a.attach(AT + 3, TAB_A, Verb::Turn, NODE, BY).unwrap());
        id
    };
    // The process a seamless update starts reads it all back from the journal.
    let mut b = Store::open(dir.path()).unwrap();
    assert_eq!(
        b.owner(TAB_A),
        Some((id.clone(), NODE.to_owned(), BY.to_owned()))
    );
    assert!(b.attach(AT + 4, TAB_A, Verb::Turn, NODE, BY).unwrap());
    assert!(
        String::from_utf8(journal(dir.path()))
            .unwrap()
            .contains(&format!(
                "op=attach req={id} tab={TAB_A} verb=turn n=2 by={BY}"
            ))
    );
    assert_eq!(
        b.left(AT + 5, TAB_A, &closed_by(BY, "ctl-close", None))
            .unwrap(),
        None,
        "one tab is still open"
    );
    assert_eq!(b.get(&id).unwrap().state(), State::Running);
    assert_eq!(
        b.left(AT + 6, TAB_B, &closed_by(BY, "shell-exit", Some(0)))
            .unwrap(),
        Some(Out::Released),
        "one tab done, the other its requester's to release"
    );
    drop(b);
    // And a cold start after a quit reads it concluded, with both tabs and the counts.
    let c = Store::open(dir.path()).unwrap();
    let request = c.get(&id).unwrap();
    assert_eq!(request.state(), State::Concluded(Out::Released));
    assert_eq!(request.end, Some((Out::Released, EndWhy::TabsLeft)));
    assert_eq!(request.why.as_deref(), Some("qualify trust-ir"));
    assert_eq!(request.open.by, BY);
    assert_eq!(request.open.host, "m114");
    assert_eq!(request.open.program.as_deref(), Some("claude"));
    assert_eq!(
        request
            .tabs
            .iter()
            .map(|tab| (
                tab.sid.as_str(),
                tab.verbs.get(&Verb::Turn).copied(),
                tab.left
            ))
            .collect::<Vec<_>>(),
        [
            (TAB_A, Some(2), Some(Out::Released)),
            (TAB_B, None, Some(Out::Done))
        ]
    );
    assert_eq!(c.skipped(), 0);
}

#[test]
fn two_stores_on_one_journal_mint_distinct_ids() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let mut old = Store::open(dir.path()).unwrap();
    let mut new = Store::open(dir.path()).unwrap();
    let first = old.open_request(AT, fleet_shell(BY, "one")).unwrap();
    let second = new
        .open_request(AT + 59_000, fleet_shell(BY, "two"))
        .unwrap();
    assert_eq!(first, "r-20261109-1411-87be5228-01");
    assert_eq!(second, "r-20261109-1411-87be5228-02");
    // Each sees the other's request without reopening.
    new.tab(AT + 59_001, &first, TAB_A, BY, false, None)
        .unwrap();
    assert_eq!(
        old.owner(TAB_A),
        Some((first.clone(), NODE.to_owned(), BY.to_owned()))
    );
    assert!(old.join(&second, NODE, BY).is_ok());
    // The next minute counts from one again.
    let third = old
        .open_request(AT + 60_000, fleet_shell(BY, "three"))
        .unwrap();
    assert_eq!(third, "r-20261109-1412-87be5228-01");
}

#[test]
fn a_tab_attaches_only_its_requesters_verbs() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let mut store = Store::open(dir.path()).unwrap();
    let id = store.open_request(AT, fleet_shell(BY, "drive")).unwrap();
    store.tab(AT, &id, TAB_A, BY, false, None).unwrap();
    let before = journal(dir.path());
    for (node, stranger) in [
        // Another node, the same session id.
        (NODE, "fleet:c0ffee00:s-0123456789abcdef0123"),
        // Another node whose head is the requester's: the same token, a different Mac.
        (TWIN, BY),
        // The same node, another session.
        (NODE, "fleet:87be5228:s-9999999999999999999a"),
        // The same node, a drive that names no session.
        (NODE, "fleet:87be5228:-"),
        // A local tab, a ctl client in no tab, the harness.
        ("", ASKER),
        ("", "ctl"),
        ("", "harness"),
    ] {
        for verb in Verb::ALL {
            assert!(
                !store.attach(AT + 1, TAB_A, verb, node, stranger).unwrap(),
                "{node} {stranger} {verb:?}"
            );
        }
        assert_eq!(journal(dir.path()), before, "{node} {stranger} wrote");
    }
    // A tab no request owns attaches nothing either.
    assert!(!store.attach(AT + 1, TAB_B, Verb::Turn, NODE, BY).unwrap());
    assert_eq!(journal(dir.path()), before);
    // Control: the requester's own verb appends.
    assert!(store.attach(AT + 2, TAB_A, Verb::Key, NODE, BY).unwrap());
    assert!(journal(dir.path()).len() > before.len());
    // A tab that left takes no more.
    store
        .left(AT + 3, TAB_A, &closed_by("human", "ui-close", None))
        .unwrap();
    let after = journal(dir.path());
    assert!(!store.attach(AT + 4, TAB_A, Verb::Key, NODE, BY).unwrap());
    assert_eq!(journal(dir.path()), after);
    // But the requester's close, whose reply comes after its tab's leaving was written, is kept;
    // another's is not.
    assert!(!store.attach(AT + 5, TAB_A, Verb::Close, "", ASKER).unwrap());
    assert!(!store.attach(AT + 5, TAB_A, Verb::Close, TWIN, BY).unwrap());
    assert_eq!(journal(dir.path()), after);
    assert!(store.attach(AT + 5, TAB_A, Verb::Close, NODE, BY).unwrap());
}

#[test]
fn req_joins_only_an_open_request_of_the_same_requester() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let mut store = Store::open(dir.path()).unwrap();
    let mine = store.open_request(AT, fleet_shell(BY, "mine")).unwrap();
    let theirs = store
        .open_request(AT, fleet_shell("fleet:87be5228:-", "theirs"))
        .unwrap();
    let done = store.open_request(AT, fleet_shell(BY, "done")).unwrap();
    store.refused(AT, &done, false).unwrap();
    assert_eq!(
        store.get(&done).unwrap().state(),
        State::Concluded(Out::Failed)
    );
    let refusal = |result: Result<(), Refusal>| result.map_err(|refusal| refusal.to_string());
    assert_eq!(
        refusal(store.join("r-20261109-1411-87be5228-99", NODE, BY)),
        Err("no request of that id on this Mac".to_owned())
    );
    assert_eq!(
        refusal(store.join(&theirs, NODE, BY)),
        Err("another requester's request".to_owned())
    );
    // NEGATIVE CONTROL: the requester's token from another Mac with the same node head.
    assert_eq!(
        refusal(store.join(&mine, TWIN, BY)),
        Err("another requester's request".to_owned())
    );
    assert_eq!(
        refusal(store.join(&done, NODE, BY)),
        Err("the request has concluded".to_owned())
    );
    assert_eq!(refusal(store.join(&mine, NODE, BY)), Ok(()));
    // A joined request's refused spawn leaves it as it was; a concluded one takes no tab.
    store.tab(AT, &mine, TAB_A, BY, false, None).unwrap();
    store.refused(AT, &mine, true).unwrap();
    assert_eq!(store.get(&mine).unwrap().state(), State::Running);
    assert!(store.tab(AT, &done, TAB_B, BY, false, None).is_err());
}

#[test]
fn a_pending_join_keeps_its_request_open_while_its_last_tab_leaves() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let mut store = Store::open(dir.path()).unwrap();
    let id = store.open_request(AT, fleet_shell(BY, "keep")).unwrap();
    store.tab(AT, &id, TAB_A, BY, false, None).unwrap();
    // The requester's `req=` passes while its only tab is closing; the tab leaves first.
    store.join(&id, NODE, BY).unwrap();
    let shell_exit = closed_by("-", "shell-exit", Some(0));
    assert_eq!(store.left(AT + 1, TAB_A, &shell_exit).unwrap(), None);
    assert_eq!(store.get(&id).unwrap().state(), State::Running);
    // So the joined tab lands on an open request, and its leaving concludes it.
    store.tab(AT + 2, &id, TAB_B, BY, true, None).unwrap();
    assert_eq!(
        store.left(AT + 3, TAB_B, &shell_exit).unwrap(),
        Some(Out::Done)
    );
    // A join whose spawn is refused concludes a request whose tabs all left meanwhile.
    let other = store.open_request(AT, fleet_shell(BY, "refused")).unwrap();
    store.tab(AT, &other, TAB_C, BY, false, None).unwrap();
    store.join(&other, NODE, BY).unwrap();
    let failed = closed_by("-", "shell-exit", Some(1));
    assert_eq!(store.left(AT + 1, TAB_C, &failed).unwrap(), None);
    store.refused(AT + 2, &other, true).unwrap();
    assert_eq!(
        store.get(&other).unwrap().end,
        Some((Out::Failed, EndWhy::TabsLeft))
    );
    // Control: with no join pending, the last tab's leaving concludes at once.
    let plain = store.open_request(AT, fleet_shell(BY, "plain")).unwrap();
    store.tab(AT, &plain, TAB_D, BY, false, None).unwrap();
    assert_eq!(
        store.left(AT + 1, TAB_D, &failed).unwrap(),
        Some(Out::Failed)
    );
    // A refused open with no tab concludes failed; one with a join pending waits for it.
    let lone = store.open_request(AT, fleet_shell(BY, "lone")).unwrap();
    store.refused(AT, &lone, false).unwrap();
    assert_eq!(
        store.get(&lone).unwrap().end,
        Some((Out::Failed, EndWhy::SpawnRefused))
    );
    let waits = store.open_request(AT, fleet_shell(BY, "waits")).unwrap();
    store.tab(AT, &waits, TAB_E, BY, false, None).unwrap();
    store.join(&waits, NODE, BY).unwrap();
    store.left(AT + 1, TAB_E, &failed).unwrap();
    assert_eq!(store.get(&waits).unwrap().end, None);
}

#[test]
fn content_tier_text_stays_out_of_the_journal() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let mut store = Store::open(dir.path()).unwrap();
    let goal = "a goal line only the content tier holds";
    let asked = "an asked line only the content tier holds";
    let local = |why: Option<&str>| Intake {
        via: Via::CtlSpawn,
        form: Form::Agent("claude".to_owned()),
        from: From {
            node: String::new(),
            host: "m72".to_owned(),
            by: ASKER.to_owned(),
            program: Some("claude".to_owned()),
            model: None,
            account: None,
        },
        why: why_of(Via::CtlSpawn, why, None, Some(goal), Some(asked)),
        brief: Some(Brief::of(b"the brief")),
        goal: Some(goal.to_owned()),
        asked: Some(asked.to_owned()),
    };
    let fell_back = store.open_request(AT, local(None)).unwrap();
    assert!(fell_back.contains("-local-"), "{fell_back}");
    let given = store
        .open_request(AT, local(Some("a given why the journal holds")))
        .unwrap();
    let journal = String::from_utf8(journal(dir.path())).unwrap();
    let said = String::from_utf8(std::fs::read(dir.path().join(crate::SAID)).unwrap()).unwrap();
    let encoded = |text: &str| aterm_codec::pct::encode(text);
    for content in [goal, asked] {
        assert!(!journal.contains(&encoded(content)), "{journal}");
        assert!(said.contains(&encoded(content)), "{said}");
    }
    assert!(journal.contains(&format!("req={fell_back} via=ctl-spawn")));
    assert!(journal.contains("why_src=goal"));
    assert!(said.contains(&format!(
        "op=why req={fell_back} text={} src=goal",
        encoded(goal)
    )));
    assert!(journal.contains(&format!(
        "op=why req={given} text={}",
        encoded("a given why the journal holds")
    )));
    assert!(!said.contains("a%20given%20why"));
    assert!(journal.contains(&format!("op=brief req={given} sha256=")));
    assert_eq!(
        store.get(&given).unwrap().why.as_deref(),
        Some("a given why the journal holds")
    );
    assert_eq!(store.get(&fell_back).unwrap().why, None);
    // Every line of either file parses as its tier's record.
    assert!(journal.lines().all(|line| Rec::parse(line).is_some()));
    assert!(said.lines().all(|line| Said::parse(line).is_some()));
}

#[test]
fn compaction_keeps_every_count_and_reloads_a_replaced_file() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let mut writer = Store::open(dir.path()).unwrap();
    let mut reader = Store::open(dir.path()).unwrap();
    let id = writer.open_request(AT, fleet_shell(BY, "busy")).unwrap();
    writer.tab(AT, &id, TAB_A, BY, false, None).unwrap();
    writer.tab(AT, &id, TAB_B, BY, false, None).unwrap();
    for n in 0..40 {
        writer.attach(AT + n, TAB_A, Verb::Send, NODE, BY).unwrap();
        writer.attach(AT + n, TAB_B, Verb::Key, NODE, BY).unwrap();
    }
    assert!(
        reader.owner(TAB_A).is_some(),
        "the reader has read the file"
    );
    let before = journal(dir.path()).len();
    let inode = |path: &Path| line_log::identity_of(&std::fs::metadata(path).unwrap());
    let old = inode(&dir.path().join(crate::JOURNAL));
    // The next append passes the bound: the journal is rewritten with each tab's last count.
    writer.compact_at = 1;
    writer.attach(AT + 99, TAB_A, Verb::Send, NODE, BY).unwrap();
    let after = String::from_utf8(journal(dir.path())).unwrap();
    assert!(after.len() < before, "{} !< {before}", after.len());
    assert_eq!(after.matches("op=attach").count(), 2, "{after}");
    let new = inode(&dir.path().join(crate::JOURNAL));
    assert_ne!(
        (old.dev, old.ino),
        (new.dev, new.ino),
        "replaced, not cut in place"
    );
    // A store holding the old file reads the new one whole, counts kept.
    reader.compact_at = u64::MAX;
    assert!(reader.attach(AT + 100, TAB_B, Verb::Key, NODE, BY).unwrap());
    let fresh = Store::open(dir.path()).unwrap();
    for store in [&reader, &fresh] {
        let request = store.get(&id).unwrap();
        let count = |sid: &str, verb| {
            request
                .tabs
                .iter()
                .find(|tab| tab.sid == sid)
                .and_then(|tab| tab.verbs.get(&verb).copied())
        };
        assert_eq!(count(TAB_A, Verb::Send), Some(41));
        assert_eq!(count(TAB_B, Verb::Key), Some(41));
    }
    assert_eq!(fresh.skipped(), 0);
}

#[test]
fn a_journal_of_live_records_is_not_rewritten_for_its_size() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let mut store = Store::open(dir.path()).unwrap();
    store.compact_at = 1;
    let inode = |path: &Path| {
        let id = line_log::identity_of(&std::fs::metadata(path).unwrap());
        (id.dev, id.ino)
    };
    let path = dir.path().join(crate::JOURNAL);
    let id = store.open_request(AT, fleet_shell(BY, "first")).unwrap();
    let first = inode(&path);
    // Every append passes the size bound, and nothing the journal holds is superseded.
    for n in 0..20 {
        store
            .open_request(AT + n, fleet_shell(BY, "another"))
            .unwrap();
    }
    store.tab(AT, &id, TAB_A, BY, false, None).unwrap();
    store.attach(AT, TAB_A, Verb::Turn, NODE, BY).unwrap();
    assert_eq!(inode(&path), first, "rewritten with nothing to drop");
    // Control: once superseded attach rows are half of it, it is rewritten.
    for n in 0..200 {
        store.attach(AT + n, TAB_A, Verb::Turn, NODE, BY).unwrap();
    }
    assert_ne!(inode(&path), first, "never compacted");
    let text = String::from_utf8(journal(dir.path())).unwrap();
    assert!(text.matches("op=attach").count() < 100, "{text}");
    assert_eq!(text.matches("op=open").count(), 21, "{text}");
    let fresh = Store::open(dir.path()).unwrap();
    assert_eq!(
        fresh.get(&id).unwrap().tabs[0].verbs.get(&Verb::Turn),
        Some(&201)
    );
}

#[test]
fn a_tab_comes_out_by_its_task_or_its_exit() {
    let worker = |task| Left {
        reason: "ctl-close".to_owned(),
        code: None,
        task: Some(task),
        by: BY.to_owned(),
        node: Some(NODE.to_owned()),
        spent: None,
    };
    assert_eq!(worker(TaskEnd::Finished).out(NODE, BY), Out::Done);
    assert_eq!(worker(TaskEnd::Failed).out(NODE, BY), Out::Failed);
    assert_eq!(worker(TaskEnd::Working).out(NODE, BY), Out::Abandoned);
    // A plain tab its requester closes is released: it neither did nor failed what it was for.
    assert_eq!(
        closed_by(BY, "ctl-close", None).out(NODE, BY),
        Out::Released
    );
    // The requester's token closing from another Mac, or from a node not known, is not its.
    let mut twin = closed_by(BY, "ctl-close", None);
    twin.node = Some(TWIN.to_owned());
    assert_eq!(twin.out(NODE, BY), Out::Abandoned);
    twin.node = None;
    assert_eq!(twin.out(NODE, BY), Out::Abandoned);
    assert_eq!(
        closed_by(ASKER, "ctl-close", None).out(NODE, BY),
        Out::Abandoned
    );
    assert_eq!(
        closed_by("-", "shell-exit", Some(0)).out(NODE, BY),
        Out::Done
    );
    assert_eq!(
        closed_by("-", "shell-exit", Some(1)).out(NODE, BY),
        Out::Failed
    );
    assert_eq!(
        closed_by("-", "shell-exit", None).out(NODE, BY),
        Out::Abandoned
    );
    for reason in ["ui-close", "window-close", "reaper", "unknown"] {
        assert_eq!(
            closed_by("human", reason, None).out(NODE, BY),
            Out::Abandoned
        );
    }
}

const MIN: u64 = 60 * 1000;

/// A look at `tabs` (each its sid, idle, and its last command's end), with what a read made at
/// `answered`'s instant reached.
fn look(tabs: &[(&str, bool, Option<u64>)], answered: Option<(u64, &[&str])>) -> Look {
    Look {
        idle_after_ms: LEASE_IDLE_MS,
        tabs: tabs
            .iter()
            .map(|(sid, idle, quiet_since_ms)| {
                (
                    (*sid).to_owned(),
                    TabNow {
                        idle: *idle,
                        quiet_since_ms: *quiet_since_ms,
                    },
                )
            })
            .collect(),
        answered: answered.map(|(read_at_ms, nodes)| Answered {
            read_at_ms,
            nodes: nodes.iter().map(|node| (*node).to_owned()).collect(),
        }),
    }
}

/// A leased plain shell concludes abandoned once its quiet passed the lease, its quiet the later
/// of its last drive and its last command's end. NEGATIVE CONTROLS: a busy tab, a worker's and a
/// local request, a tab the look did not find, and a pending join each write nothing.
#[test]
fn an_idle_leased_shell_concludes_abandoned_once_its_quiet_window_passes() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let mut store = Store::open(dir.path()).unwrap();
    let id = store.open_request(AT, fleet_shell(BY, "lease")).unwrap();
    store.tab(AT, &id, TAB_A, BY, false, None).unwrap();
    let driven = AT + 5 * MIN;
    assert!(store.attach(driven, TAB_A, Verb::Turn, NODE, BY).unwrap());
    // A close is no drive.
    store
        .attach(driven + MIN, TAB_A, Verb::Close, NODE, BY)
        .ok();
    let idle = look(&[(TAB_A, true, None)], None);
    let before = journal(dir.path());
    // NEGATIVE CONTROL: one millisecond short of the lease, nothing.
    assert_eq!(store.lease(driven + LEASE_IDLE_MS - 1, &idle).unwrap(), []);
    // NEGATIVE CONTROL: past it, but busy.
    assert_eq!(
        store
            .lease(driven + LEASE_IDLE_MS, &look(&[(TAB_A, false, None)], None))
            .unwrap(),
        []
    );
    // NEGATIVE CONTROL: its last command ended 10 minutes after the drive, which moves the lease.
    let ran = look(&[(TAB_A, true, Some(driven + 10 * MIN))], None);
    assert_eq!(store.lease(driven + LEASE_IDLE_MS, &ran).unwrap(), []);
    // NEGATIVE CONTROL: a look that did not find the tab.
    assert_eq!(
        store
            .lease(driven + LEASE_IDLE_MS, &look(&[(TAB_B, true, None)], None))
            .unwrap(),
        []
    );
    // NEGATIVE CONTROL: a pending join holds it.
    store.join(&id, NODE, BY).unwrap();
    assert_eq!(store.lease(driven + LEASE_IDLE_MS, &idle).unwrap(), []);
    store.refused(driven, &id, true).unwrap();
    assert_eq!(journal(dir.path()), before, "nothing written");
    let ended = store.lease(driven + 40 * MIN, &ran).unwrap();
    assert_eq!(
        ended,
        [Ended {
            id: id.clone(),
            out: Out::Abandoned,
            why: EndWhy::Idle,
            tabs: vec![TAB_A.to_owned()],
        }]
    );
    let request = store.get(&id).unwrap();
    assert_eq!(request.state(), State::Concluded(Out::Abandoned));
    assert_eq!(request.tabs[0].left, None, "its tab stays");
    // Its tab's later leaving writes nothing, and a second look concludes nothing more.
    let after = journal(dir.path());
    assert_eq!(
        store
            .left(
                driven + 41 * MIN,
                TAB_A,
                &closed_by("human", "ui-close", None)
            )
            .unwrap(),
        None
    );
    assert_eq!(store.lease(driven + 50 * MIN, &idle).unwrap(), []);
    assert_eq!(journal(dir.path()), after);
    // NEGATIVE CONTROL: a worker's and a local request, idle as long, are no lease.
    for via in [Via::FleetWorker, Via::CtlSpawn] {
        let other = store
            .open_request(
                AT,
                Intake {
                    via,
                    ..fleet_shell(BY, "not a lease")
                },
            )
            .unwrap();
        store.tab(AT, &other, TAB_C, BY, false, None).unwrap();
        assert_eq!(
            store
                .lease(AT + 2 * LEASE_IDLE_MS, &look(&[(TAB_C, true, None)], None))
                .unwrap(),
            [],
            "{via:?}"
        );
        store
            .left(AT, TAB_C, &closed_by("-", "shell-exit", Some(0)))
            .unwrap();
    }
}

/// A leased shell whose requester's Mac a fresh fleet read made since its last drive no longer
/// reaches concludes once idle, before its lease. NEGATIVE CONTROLS: no read, a read that
/// reached it, one that reached none, one of the other id form, one naming both forms, one made
/// before (or at) its requester's last drive, and a busy tab write nothing.
#[test]
fn a_leased_shell_whose_requester_left_concludes_once_idle() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let mut store = Store::open(dir.path()).unwrap();
    let id = store.open_request(AT, fleet_shell(BY, "left")).unwrap();
    store.tab(AT, &id, TAB_A, BY, false, None).unwrap();
    let driven = AT + 5 * MIN;
    assert!(store.attach(driven, TAB_A, Verb::Turn, NODE, BY).unwrap());
    let other = "c0ffee00c0ffee00c0ffee00c0ffee00";
    let mesh = "n-0123456789abcdef";
    let since = driven + 1;
    let at = driven + MIN;
    let before = journal(dir.path());
    for (idle, answered) in [
        (true, None),
        (true, Some((since, &[NODE, other][..]))),
        (true, Some((since, &[mesh][..]))),
        (true, Some((since, &[other, mesh][..]))),
        (true, Some((since, &[][..]))),
        (true, Some((driven, &[other][..]))),
        (true, Some((driven - MIN, &[other][..]))),
        (false, Some((since, &[other][..]))),
    ] {
        assert_eq!(
            store
                .lease(at, &look(&[(TAB_A, idle, None)], answered))
                .unwrap(),
            [],
            "{idle} {answered:?}"
        );
    }
    // The requester's node in another case is still the requester.
    let upper = NODE.to_ascii_uppercase();
    assert_eq!(
        store
            .lease(
                at,
                &look(&[(TAB_A, true, None)], Some((since, &[&upper, other])))
            )
            .unwrap(),
        []
    );
    assert_eq!(journal(dir.path()), before);
    let ended = store
        .lease(at, &look(&[(TAB_A, true, None)], Some((since, &[other]))))
        .unwrap();
    assert_eq!(ended.len(), 1);
    assert_eq!(
        store.get(&id).unwrap().end,
        Some((Out::Abandoned, EndWhy::RequesterLeft))
    );
}

/// The requester's close of its plain shell releases it. NEGATIVE CONTROL: another Mac's close
/// with the requester's token, and a local tab's, abandon it.
#[test]
fn a_requesters_close_releases_its_shell() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let mut store = Store::open(dir.path()).unwrap();
    let mut twin = closed_by(BY, "ctl-close", None);
    twin.node = Some(TWIN.to_owned());
    for (tab, left, out) in [
        (TAB_A, closed_by(BY, "ctl-close", None), Out::Released),
        (TAB_B, twin, Out::Abandoned),
        (TAB_C, closed_by(ASKER, "ctl-close", None), Out::Abandoned),
    ] {
        let id = store.open_request(AT, fleet_shell(BY, "close")).unwrap();
        store.tab(AT, &id, tab, BY, false, None).unwrap();
        assert_eq!(store.left(AT + 1, tab, &left).unwrap(), Some(out));
        assert_eq!(store.get(&id).unwrap().end, Some((out, EndWhy::TabsLeft)));
    }
}

/// A kept tab is the person's: its request records the keep and concludes released where it
/// was the last open tab, and nothing of the requester attaches to it again. NEGATIVE CONTROLS:
/// a tab no request owns, and one that left, record nothing; a request with another tab open
/// stays running.
#[test]
fn a_kept_tab_is_the_persons_and_leaves_its_request() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let mut store = Store::open(dir.path()).unwrap();
    let id = store.open_request(AT, fleet_shell(BY, "keep")).unwrap();
    store.tab(AT, &id, TAB_A, BY, false, None).unwrap();
    store.tab(AT, &id, TAB_B, BY, false, None).unwrap();
    store
        .left(AT + 1, TAB_B, &closed_by("-", "shell-exit", Some(0)))
        .unwrap();
    let before = journal(dir.path());
    assert_eq!(store.keep(AT + 2, TAB_C, "human").unwrap(), None);
    assert_eq!(store.keep(AT + 2, TAB_B, "human").unwrap(), None);
    assert_eq!(journal(dir.path()), before, "nothing written");
    let ended = store.keep(AT + 3, TAB_A, "human").unwrap();
    assert_eq!(
        ended,
        Some(Ended {
            id: id.clone(),
            out: Out::Released,
            why: EndWhy::Kept,
            tabs: vec![TAB_A.to_owned(), TAB_B.to_owned()],
        })
    );
    assert!(
        String::from_utf8(journal(dir.path()))
            .unwrap()
            .contains(&format!("op=keep req={id} tab={TAB_A} by=human"))
    );
    assert!(!store.attach(AT + 4, TAB_A, Verb::Turn, NODE, BY).unwrap());
    assert!(!store.attach(AT + 4, TAB_A, Verb::Close, NODE, BY).unwrap());
    assert_eq!(
        store
            .lease(AT + 2 * LEASE_IDLE_MS, &look(&[(TAB_A, true, None)], None))
            .unwrap(),
        []
    );
    // A second keep of it records nothing.
    assert_eq!(store.keep(AT + 5, TAB_A, "human").unwrap(), None);
    // With another tab open, keeping one leaves the request running, and the kept one takes no
    // more of its requester's verbs.
    let two = store.open_request(AT, fleet_shell(BY, "two")).unwrap();
    store.tab(AT, &two, TAB_C, BY, false, None).unwrap();
    store.tab(AT, &two, TAB_D, BY, false, None).unwrap();
    assert_eq!(store.keep(AT + 1, TAB_C, "human").unwrap(), None);
    assert_eq!(store.get(&two).unwrap().state(), State::Running);
    assert!(!store.attach(AT + 2, TAB_C, Verb::Turn, NODE, BY).unwrap());
    assert!(store.attach(AT + 2, TAB_D, Verb::Turn, NODE, BY).unwrap());
    // Its kept tab's leaving writes nothing; the other's concludes it, the kept one released.
    assert_eq!(
        store
            .left(AT + 3, TAB_C, &closed_by("human", "ui-close", None))
            .unwrap(),
        None
    );
    assert_eq!(
        store
            .left(AT + 4, TAB_D, &closed_by("-", "shell-exit", Some(0)))
            .unwrap(),
        Some(Out::Released)
    );
    // The keep survives a reload.
    let fresh = Store::open(dir.path()).unwrap();
    assert!(fresh.get(&two).unwrap().tabs[0].kept);
    assert_eq!(fresh.skipped(), 0);
}

/// After a relaunch, a request opened before it whose tabs are gone concludes. NEGATIVE
/// CONTROLS: a request whose tab is hosted, one opened since the process was born, and one with a
/// join pending are untouched, and a second sweep writes nothing.
#[test]
fn the_relaunch_sweep_concludes_only_requests_whose_tabs_are_gone() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let mut store = Store::open(dir.path()).unwrap();
    let born = AT + MIN;
    let hosted_id = store.open_request(AT, fleet_shell(BY, "hosted")).unwrap();
    store.tab(AT, &hosted_id, TAB_A, BY, false, None).unwrap();
    let gone = store.open_request(AT, fleet_shell(BY, "gone")).unwrap();
    store.tab(AT, &gone, TAB_B, BY, false, None).unwrap();
    store.tab(AT, &gone, TAB_E, BY, false, None).unwrap();
    store
        .left(AT, TAB_E, &closed_by("-", "shell-exit", Some(0)))
        .unwrap();
    let tabless = store.open_request(AT, fleet_shell(BY, "tabless")).unwrap();
    let young = store.open_request(born, fleet_shell(BY, "young")).unwrap();
    let joining = store.open_request(AT, fleet_shell(BY, "joining")).unwrap();
    store.tab(AT, &joining, TAB_C, BY, false, None).unwrap();
    store.join(&joining, NODE, BY).unwrap();
    let hosted: BTreeSet<String> = [TAB_A.to_owned()].into();
    let ended = store.sweep(born + 1, born, &hosted).unwrap();
    let ids: Vec<(&str, Out, EndWhy)> = ended
        .iter()
        .map(|end| (end.id.as_str(), end.out, end.why))
        .collect();
    assert_eq!(
        ids,
        [
            (gone.as_str(), Out::Abandoned, EndWhy::Relaunch),
            (tabless.as_str(), Out::Abandoned, EndWhy::Relaunch),
        ]
    );
    let text = String::from_utf8(journal(dir.path())).unwrap();
    assert!(
        text.contains(&format!(
            "op=left req={gone} tab={TAB_B} reason=relaunch code=- task=- out=abandoned"
        )),
        "{text}"
    );
    for (id, state) in [
        (&hosted_id, State::Running),
        (&young, State::Open),
        (&joining, State::Running),
        (&gone, State::Concluded(Out::Abandoned)),
        (&tabless, State::Concluded(Out::Abandoned)),
    ] {
        assert_eq!(store.get(id).unwrap().state(), state, "{id}");
    }
    let before = journal(dir.path());
    assert_eq!(store.sweep(born + 2, born, &hosted).unwrap(), []);
    assert_eq!(journal(dir.path()), before);
    assert_eq!(Store::open(dir.path()).unwrap().skipped(), 0);
}

/// A tab's last drive is folded from its typed verbs' times, and survives a reload and a
/// compaction.
#[test]
fn a_tabs_last_drive_survives_reload_and_compaction() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let mut store = Store::open(dir.path()).unwrap();
    let id = store.open_request(AT, fleet_shell(BY, "drive")).unwrap();
    store.tab(AT, &id, TAB_A, BY, false, None).unwrap();
    assert_eq!(store.get(&id).unwrap().tabs[0].last_ms, AT);
    for n in 1..=30 {
        store.attach(AT + n, TAB_A, Verb::Send, NODE, BY).unwrap();
    }
    store.attach(AT + 40, TAB_A, Verb::Key, NODE, BY).unwrap();
    store.attach(AT + 50, TAB_A, Verb::Close, NODE, BY).unwrap();
    store.compact_at = 1;
    store.attach(AT + 31, TAB_A, Verb::Send, NODE, BY).unwrap();
    let text = String::from_utf8(journal(dir.path())).unwrap();
    assert_eq!(text.matches("op=attach").count(), 3, "compacted: {text}");
    for read in [&store, &Store::open(dir.path()).unwrap()] {
        assert_eq!(read.get(&id).unwrap().tabs[0].last_ms, AT + 40);
    }
}

#[test]
fn an_exhausted_minute_never_reuses_an_id_or_writes_either_log() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let mut store = Store::open(dir.path()).unwrap();
    let first = store.open_request(AT, fleet_shell(BY, "original")).unwrap();
    drop(store);
    let last = crate::id::nth(&crate::id::prefix(AT, &crate::id::head_of(NODE)), u32::MAX);
    let retained = String::from_utf8(journal(dir.path()))
        .unwrap()
        .replace(&first, &last);
    std::fs::write(dir.path().join(crate::JOURNAL), retained).unwrap();
    let mut store = Store::open(dir.path()).unwrap();
    assert!(store.get(&last).is_some());
    assert_eq!(store.skipped(), 0);
    let before = journal(dir.path());
    let said = std::fs::read(dir.path().join(crate::SAID)).unwrap_or_default();
    let mut intake = fleet_shell(BY, "");
    intake.via = Via::FleetWorker;
    intake.form = Form::Agent("claude".into());
    intake.why = why_of(
        Via::FleetWorker,
        None,
        Some(b"new private brief"),
        None,
        None,
    );
    assert!(store.open_request(AT, intake).is_err());
    assert_eq!(journal(dir.path()), before);
    assert_eq!(
        std::fs::read(dir.path().join(crate::SAID)).unwrap_or_default(),
        said
    );
    assert_eq!(store.get(&last).unwrap().why.as_deref(), Some("original"));
    let next = store
        .open_request(AT + 60_000, fleet_shell(BY, "next minute"))
        .unwrap();
    assert_eq!(next, "r-20261109-1412-87be5228-01");
}

#[test]
fn an_interrupted_final_delimiter_cannot_alias_the_next_request() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let mut first = fleet_shell(BY, "");
    first.why = why_of(Via::FleetShell, None, None, None, None);
    let original = {
        let mut store = Store::open(dir.path()).unwrap();
        store.open_request(AT, first).unwrap()
    };
    let mut bytes = journal(dir.path());
    assert_eq!(bytes.pop(), Some(b'\n'));
    std::fs::write(dir.path().join(crate::JOURNAL), bytes).unwrap();
    let mut store = Store::open(dir.path()).unwrap();
    assert!(
        store.get(&original).is_none(),
        "readers wait for a whole line"
    );
    let mut second = fleet_shell(BY, "another request");
    second.from.host = "another-host".into();
    let next = store.open_request(AT, second).unwrap();
    assert_ne!(
        next, original,
        "the append seals the retained open before allocation"
    );
    let reopened = Store::open(dir.path()).unwrap();
    for id in [&original, &next] {
        assert_eq!(store.get(id), reopened.get(id));
    }
    assert_eq!(reopened.get(&original).unwrap().open.host, "m114");
    assert_eq!(reopened.get(&next).unwrap().open.host, "another-host");
    assert_eq!(reopened.skipped(), 0);
}

/// A local agent tab's requester token and this Mac's node.
#[cfg(unix)]
const LOCAL: &str = "s-11111111111111111111";

#[cfg(unix)]
fn named<'a>(id: Option<&'a str>, path: &'a str, node: &'a str, by: &'a str) -> ClaimAsk<'a> {
    ClaimAsk {
        id,
        path,
        node,
        by,
        tab: None,
        born_ms: Some(AT + 10),
        ran: &[],
    }
}

/// A fleet shell request with tab [`TAB_A`] open.
fn running(store: &mut Store) -> String {
    let id = store.open_request(AT, fleet_shell(BY, "work")).unwrap();
    store.tab(AT, &id, TAB_A, BY, false, None).unwrap();
    id
}

#[cfg(unix)]
fn born(id: &str, path: &str, alone: Option<bool>) -> Born {
    Born {
        id: id.to_owned(),
        path: path.to_owned(),
        src: ClaimSrc::Born,
        tab: Some(TAB_A.to_owned()),
        born_ms: AT + 5,
        alone,
    }
}

#[cfg(unix)]
#[test]
fn a_requester_claims_and_the_same_token_on_another_node_is_refused() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let mut store = Store::open(dir.path()).unwrap();
    let id = running(&mut store);
    assert_eq!(
        store
            .claim(AT + 1, named(Some(&id), "/Users//a/x/", NODE, BY))
            .unwrap(),
        Claimed::New(id.clone())
    );
    let held = store.get(&id).unwrap().claim("/Users//a/x").unwrap();
    assert_eq!(held.claim.src, ClaimSrc::Named);
    assert_eq!(
        (held.claim.alone, held.claim.born_ms),
        (None, Some(AT + 10))
    );
    assert_eq!(held.at_ms, AT + 1);
    // NEGATIVE: the same token from another Mac, an unknown id, no id at all.
    assert!(matches!(
        store.claim(AT + 2, named(Some(&id), "/Users//a/y", TWIN, BY)),
        Err(ClaimRefusal::NotYours)
    ));
    assert!(matches!(
        store.claim(
            AT + 2,
            named(Some("r-20261109-1411-87be5228-09"), "/Users//a/y", NODE, BY)
        ),
        Err(ClaimRefusal::Unknown)
    ));
    assert!(matches!(
        store.claim(AT + 2, named(None, "/Users//a/y", NODE, BY)),
        Err(ClaimRefusal::NoRequest)
    ));
    assert!(matches!(
        store.claim(AT + 2, named(Some(&id), "Users/a/y", NODE, BY)),
        Err(ClaimRefusal::Path(_))
    ));
    assert_eq!(store.get(&id).unwrap().claims.len(), 1);
}

#[cfg(unix)]
#[test]
fn a_request_tab_claims_for_its_request_without_req() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let mut store = Store::open(dir.path()).unwrap();
    let id = running(&mut store);
    let other = store.open_request(AT, fleet_shell(BY, "other")).unwrap();
    store.tab(AT, &other, TAB_B, BY, false, None).unwrap();
    let from_tab = |path| ClaimAsk {
        tab: Some(TAB_A),
        ..named(None, path, "", LOCAL)
    };
    assert_eq!(
        store.claim(AT + 1, from_tab("/Users//a/x")).unwrap(),
        Claimed::New(id.clone())
    );
    assert_eq!(
        store.get(&id).unwrap().claims[0].claim.tab.as_deref(),
        Some(TAB_A)
    );
    // NEGATIVE: a tab of another request naming this one is refused, naming its own.
    let wrong = ClaimAsk {
        tab: Some(TAB_B),
        ..named(Some(&id), "/Users//a/y", "", LOCAL)
    };
    assert!(matches!(
        store.claim(AT + 2, wrong),
        Err(ClaimRefusal::OtherTab(own)) if own == other
    ));
}

#[cfg(unix)]
#[test]
fn a_path_another_request_claimed_is_taken_and_a_repeat_is_already() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let mut store = Store::open(dir.path()).unwrap();
    let id = running(&mut store);
    let other = store.open_request(AT, fleet_shell(BY, "other")).unwrap();
    store
        .claim(AT + 1, named(Some(&id), "/Users//a/x", NODE, BY))
        .unwrap();
    assert_eq!(
        store
            .claim(AT + 2, named(Some(&id), "/Users//a/x", NODE, BY))
            .unwrap(),
        Claimed::Already(id.clone())
    );
    for path in ["/Users//a/x", "/Users//a/x/deep"] {
        assert!(matches!(
            store.claim(AT + 3, named(Some(&other), path, NODE, BY)),
            Err(ClaimRefusal::Taken(owner)) if owner == id
        ));
    }
    // CONTROL: a sibling with a shared prefix is free.
    assert_eq!(
        store
            .claim(AT + 4, named(Some(&other), "/Users//a/xy", NODE, BY))
            .unwrap(),
        Claimed::New(other)
    );
}

#[cfg(unix)]
#[test]
fn a_path_inside_a_claim_answers_inside_and_writes_nothing() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let mut store = Store::open(dir.path()).unwrap();
    let id = running(&mut store);
    store
        .claim(AT + 1, named(Some(&id), "/Users//a/x", NODE, BY))
        .unwrap();
    let before = journal(dir.path());
    assert_eq!(
        store
            .claim(AT + 2, named(Some(&id), "/Users//a/x/build", NODE, BY))
            .unwrap(),
        Claimed::Inside {
            id: id.clone(),
            claim: "/Users//a/x".to_owned()
        }
    );
    assert_eq!(journal(dir.path()), before);
}

#[cfg(unix)]
#[test]
fn claims_stop_at_max_claims() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let mut store = Store::open(dir.path()).unwrap();
    let id = running(&mut store);
    let found: Vec<Born> = (0..MAX_CLAIMS + 3)
        .map(|n| born(&id, &format!("/Users//a/b{n}"), Some(true)))
        .collect();
    assert_eq!(store.born(AT + 6, &found).unwrap().len(), MAX_CLAIMS);
    assert!(matches!(
        store.claim(AT + 7, named(Some(&id), "/Users//a/more", NODE, BY)),
        Err(ClaimRefusal::Full)
    ));
    assert_eq!(
        Store::open(dir.path())
            .unwrap()
            .get(&id)
            .unwrap()
            .claims
            .len(),
        MAX_CLAIMS
    );
}

#[cfg(unix)]
#[test]
fn tab_with_a_cwd_records_a_root_once() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let mut store = Store::open(dir.path()).unwrap();
    let id = store.open_request(AT, fleet_shell(BY, "work")).unwrap();
    store
        .tab(AT, &id, TAB_A, BY, false, Some("/Users//a/w"))
        .unwrap();
    store
        .tab(AT, &id, TAB_B, BY, false, Some("/Users//a/w"))
        .unwrap();
    // NEGATIVE: no root, and an unnormal path, record none.
    store.tab(AT, &id, TAB_C, BY, false, None).unwrap();
    store
        .tab(AT, &id, TAB_D, BY, false, Some("/Users//a/v/"))
        .unwrap();
    let text = String::from_utf8(journal(dir.path())).unwrap();
    assert_eq!(text.matches("op=root").count(), 1, "{text}");
    assert!(
        text.contains(&format!("op=root req={id} path=/Users//a/w tab={TAB_A}\n")),
        "{text}"
    );
    assert_eq!(
        Store::open(dir.path()).unwrap().get(&id).unwrap().roots,
        ["/Users//a/w"]
    );
}

#[cfg(unix)]
#[test]
fn born_skips_taken_and_inside_paths_and_keeps_alone() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let mut store = Store::open(dir.path()).unwrap();
    let id = running(&mut store);
    let other = store.open_request(AT, fleet_shell(BY, "other")).unwrap();
    store
        .claim(AT + 1, named(Some(&other), "/Users//a/theirs", NODE, BY))
        .unwrap();
    let found = [
        born(&id, "/Users//a/x", Some(false)),
        born(&id, "/Users//a/x/inner", Some(true)),
        born(&id, "/Users//a/theirs", Some(true)),
        born(&id, "/Users//a/theirs/deep", Some(true)),
        born(&id, "/Users//a/y", Some(true)),
    ];
    let claimed = store.born(AT + 6, &found).unwrap();
    assert_eq!(
        claimed,
        [
            ("/Users//a/x".to_owned(), id.clone()),
            ("/Users//a/y".to_owned(), id.clone())
        ]
    );
    let request = store.get(&id).unwrap();
    let x = &request.claim("/Users//a/x").unwrap().claim;
    assert_eq!(
        (x.src, x.alone, x.born_ms, x.by.as_str(), x.tab.as_deref()),
        (ClaimSrc::Born, Some(false), Some(AT + 5), BY, Some(TAB_A))
    );
    assert_eq!(request.claim("/Users//a/y").unwrap().claim.alone, Some(true));
    // A second look finds the same folders claimed.
    assert!(store.born(AT + 7, &found).unwrap().is_empty());
}

/// No claim holds another: a folder holding another request's claim (or this one's) is refused
/// to a requester and skipped by a scan, whatever order they come in, and a cold reader drops a
/// row that would nest. Negative control: a sibling with a shared prefix is free.
#[cfg(unix)]
#[test]
fn a_folder_holding_a_claim_is_never_claimed() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let mut store = Store::open(dir.path()).unwrap();
    let id = running(&mut store);
    let other = store.open_request(AT, fleet_shell(BY, "other")).unwrap();
    store
        .claim(AT + 1, named(Some(&id), "/Users//a/w/sub", NODE, BY))
        .unwrap();
    for (who, path) in [
        (&other, "/Users//a/w"),
        (&id, "/Users//a/w"),
        (&other, "/Users//a"),
    ] {
        assert!(
            matches!(
                store.claim(AT + 2, named(Some(who), path, NODE, BY)),
                Err(ClaimRefusal::Taken(owner)) if owner == id
            ),
            "{path}"
        );
    }
    assert!(
        store
            .born(AT + 3, &[born(&other, "/Users//a/w", Some(true))])
            .unwrap()
            .is_empty()
    );
    // Within one scan, the inner folder first then the outer: only the inner is claimed.
    let found = [
        born(&other, "/Users//a/v/inner", Some(true)),
        born(&other, "/Users//a/v", Some(true)),
    ];
    assert_eq!(
        store.born(AT + 4, &found).unwrap(),
        [("/Users//a/v/inner".to_owned(), other.clone())]
    );
    // CONTROL: a sibling with a shared prefix.
    assert_eq!(
        store
            .claim(AT + 5, named(Some(&other), "/Users//a/w/su", NODE, BY))
            .unwrap(),
        Claimed::New(other.clone())
    );
    // A row no store writes (a claim holding another) folds into nothing.
    let line = Rec {
        t_ms: AT + 6,
        id: other.clone(),
        body: Body::Claim(Claim {
            path: "/Users//a/w".to_owned(),
            src: ClaimSrc::Named,
            by: BY.to_owned(),
            tab: None,
            born_ms: None,
            alone: None,
        }),
    }
    .line();
    let mut file = std::fs::OpenOptions::new()
        .append(true)
        .open(dir.path().join(crate::JOURNAL))
        .unwrap();
    std::io::Write::write_all(&mut file, format!("{line}\n").as_bytes()).unwrap();
    let mut cold = Store::open(dir.path()).unwrap();
    assert_eq!(cold.skipped(), 1);
    assert!(cold.claims().all(|(path, _)| path != "/Users//a/w"));
}

/// A named claim's birth is judged by the spans handed with it: alone where a bounded command of
/// its own request's tab ran then and no other tab's did, contested where another's ran, and
/// unknown otherwise (no span of its own, one still running, or one whose end was not seen).
#[cfg(unix)]
#[test]
fn a_named_claim_is_alone_only_where_its_own_bounded_span_held_its_birth() {
    use crate::claim::Span;
    let dir = aterm_tempfile::tempdir().unwrap();
    let mut store = Store::open(dir.path()).unwrap();
    let id = running(&mut store);
    let other = store.open_request(AT, fleet_shell(BY, "other")).unwrap();
    store.tab(AT, &other, TAB_B, BY, false, None).unwrap();
    let born_at = AT + 10;
    let ran = |tab, to_ms, whole| Ran {
        tab,
        span: Span {
            from_ms: AT + 5,
            to_ms,
        },
        whole,
    };
    let cases = [
        (
            "/Users//a/n1",
            vec![ran(TAB_A, Some(AT + 20), true)],
            Some(true),
        ),
        (
            "/Users//a/n2",
            vec![ran(TAB_A, Some(AT + 20), true), ran(TAB_B, None, true)],
            Some(false),
        ),
        (
            "/Users//a/n3",
            vec![
                ran(TAB_A, Some(AT + 20), true),
                ran(TAB_E, Some(AT + 20), false),
            ],
            Some(false),
        ),
        ("/Users//a/n4", vec![], None),
        ("/Users//a/n5", vec![ran(TAB_A, None, true)], None),
        ("/Users//a/n6", vec![ran(TAB_A, Some(AT + 20), false)], None),
    ];
    for (path, spans, want) in cases {
        let ask = ClaimAsk {
            born_ms: Some(born_at),
            ran: &spans,
            ..named(Some(&id), path, NODE, BY)
        };
        assert_eq!(store.claim(AT + 30, ask).unwrap(), Claimed::New(id.clone()));
        assert_eq!(
            store.get(&id).unwrap().claim(path).unwrap().claim.alone,
            want,
            "{path}"
        );
    }
}

#[cfg(unix)]
#[test]
fn touched_is_written_once_per_request_and_path() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let mut store = Store::open(dir.path()).unwrap();
    let id = running(&mut store);
    store
        .born(AT + 6, &[born(&id, "/Users//a/x", Some(true))])
        .unwrap();
    let touch = |path: &str, blk| Touch {
        id: id.clone(),
        path: path.to_owned(),
        tab: TAB_B.to_owned(),
        blk,
        why: TouchWhy::PersonTab,
    };
    assert_eq!(
        store
            .touched(AT + 7, &[touch("/Users//a/x", 1), touch("/Users//a/x", 2)])
            .unwrap(),
        1
    );
    assert_eq!(store.touched(AT + 8, &[touch("/Users//a/x", 3)]).unwrap(), 0);
    // NEGATIVE: a folder the request does not claim is no touch of it.
    assert_eq!(store.touched(AT + 8, &[touch("/Users//a/z", 4)]).unwrap(), 0);
    let fresh = Store::open(dir.path()).unwrap();
    assert!(fresh.get(&id).unwrap().touched.contains("/Users//a/x"));
    let text = String::from_utf8(journal(dir.path())).unwrap();
    assert_eq!(text.matches("op=touched").count(), 1, "{text}");
}

#[test]
fn left_with_spent_writes_spent_before_left_and_folds_it() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let mut store = Store::open(dir.path()).unwrap();
    let id = running(&mut store);
    store.tab(AT, &id, TAB_B, BY, false, None).unwrap();
    let spent = Spent {
        cpu_s: Some(12),
        peak_mb: Some(300),
        written_mb: None,
    };
    let left = Left {
        spent: Some(spent),
        ..closed_by(BY, "shell-exit", Some(1))
    };
    store.left(AT + 1, TAB_A, &left).unwrap();
    let text = String::from_utf8(journal(dir.path())).unwrap();
    let spent_at = text.find("op=spent").expect("spent");
    let left_at = text.find("op=left").expect("left");
    assert!(spent_at < left_at, "{text}");
    // A tab of a concluded request still records its spend, once.
    store.left(AT + 2, TAB_B, &left).unwrap();
    store.left(AT + 3, TAB_B, &left).unwrap();
    let fresh = Store::open(dir.path()).unwrap();
    let request = fresh.get(&id).unwrap();
    assert_eq!(request.tabs[0].spent, Some(spent));
    assert_eq!(
        request.spent(),
        Spent {
            cpu_s: Some(24),
            peak_mb: Some(300),
            written_mb: None
        }
    );
    assert_eq!(
        String::from_utf8(journal(dir.path()))
            .unwrap()
            .matches("op=spent")
            .count(),
        2
    );
    // NEGATIVE: an unread spend writes nothing.
    let id2 = store.open_request(AT, fleet_shell(BY, "two")).unwrap();
    store.tab(AT, &id2, TAB_C, BY, false, None).unwrap();
    let unread = Left {
        spent: Some(Spent::default()),
        ..closed_by(BY, "shell-exit", Some(0))
    };
    store.left(AT + 4, TAB_C, &unread).unwrap();
    assert_eq!(store.get(&id2).unwrap().tabs[0].spent, None);
}

#[cfg(unix)]
#[test]
fn claims_survive_reopen_and_compaction() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let mut store = Store::open(dir.path()).unwrap();
    let id = running(&mut store);
    store
        .born(AT + 6, &[born(&id, "/Users//a/x", Some(true))])
        .unwrap();
    store
        .claim(AT + 7, named(Some(&id), "/tmp/named", NODE, BY))
        .unwrap();
    for n in 0..40 {
        store.attach(AT + n, TAB_A, Verb::Send, NODE, BY).unwrap();
    }
    let inode = |path: &Path| {
        let id = line_log::identity_of(&std::fs::metadata(path).unwrap());
        (id.dev, id.ino)
    };
    let path = dir.path().join(crate::JOURNAL);
    let before = inode(&path);
    store.compact_at = 1;
    store.attach(AT + 99, TAB_A, Verb::Send, NODE, BY).unwrap();
    assert_ne!(inode(&path), before, "compacted");
    let mut fresh = Store::open(dir.path()).unwrap();
    let claims: BTreeMap<String, String> = fresh
        .claims()
        .map(|(path, id)| (path.to_owned(), id.to_owned()))
        .collect();
    assert_eq!(
        claims,
        BTreeMap::from([
            ("/Users//a/x".to_owned(), id.clone()),
            ("/tmp/named".to_owned(), id.clone())
        ])
    );
    assert_eq!(fresh.skipped(), 0);
}

#[cfg(unix)]
#[test]
fn a_cold_reader_drops_a_second_claim_of_a_taken_path() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let mut a = Store::open(dir.path()).unwrap();
    let first = running(&mut a);
    let second = a.open_request(AT, fleet_shell(BY, "two")).unwrap();
    a.claim(AT + 1, named(Some(&first), "/Users//a/x", NODE, BY))
        .unwrap();
    // A row no store writes (a second claim of a taken folder) folds into nothing.
    let line = Rec {
        t_ms: AT + 2,
        id: second.clone(),
        body: Body::Claim(Claim {
            path: "/Users//a/x".to_owned(),
            src: ClaimSrc::Named,
            by: BY.to_owned(),
            tab: None,
            born_ms: None,
            alone: None,
        }),
    }
    .line();
    let mut file = std::fs::OpenOptions::new()
        .append(true)
        .open(dir.path().join(crate::JOURNAL))
        .unwrap();
    std::io::Write::write_all(&mut file, format!("{line}\n").as_bytes()).unwrap();
    let mut b = Store::open(dir.path()).unwrap();
    assert_eq!(b.skipped(), 1);
    assert!(b.get(&second).unwrap().claims.is_empty());
    assert_eq!(
        b.claims().collect::<Vec<_>>(),
        [("/Users//a/x", first.as_str())]
    );
}

/// A fleet shell's request with tab [`TAB_A`], concluded where `concluded`.
fn shell_request(store: &mut Store, concluded: bool) -> String {
    let id = store
        .open_request(AT, fleet_shell(BY, "review me"))
        .unwrap();
    store.tab(AT + 1, &id, TAB_A, BY, false, None).unwrap();
    if concluded {
        store
            .left(AT + 2, TAB_A, &closed_by(BY, "shell-exit", Some(3)))
            .unwrap();
    }
    id
}

#[test]
fn review_needs_a_concluded_request_and_folds_by_and_note() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let mut store = Store::open(dir.path()).unwrap();
    let id = shell_request(&mut store, false);
    let before = journal(dir.path());
    // NEGATIVE: a running request is no review's, and nothing is written.
    assert!(matches!(
        store.review(AT + 3, &id, "human", None),
        Err(ActRefusal::Running)
    ));
    assert!(matches!(
        store.review(AT + 3, "r-20261109-1411-87be5228-09", "human", None),
        Err(ActRefusal::Unknown)
    ));
    assert_eq!(journal(dir.path()), before);
    store
        .left(AT + 4, TAB_A, &closed_by(BY, "shell-exit", Some(0)))
        .unwrap();
    assert_eq!(
        store
            .review(AT + 5, &id, "ctl", Some("ran as asked"))
            .unwrap(),
        Reviewed::New
    );
    // A note of more than one line, one with a double quote (which a kept line makes an
    // apostrophe) and `-` (which the record writes for none) are refused, never changed.
    let reviewed = journal(dir.path());
    for note in ["one\ntwo", "it\"s fine", "-", " - "] {
        assert!(
            matches!(
                store.review(AT + 6, &id, "ctl", Some(note)),
                Err(ActRefusal::Shape(_))
            ),
            "{note:?}"
        );
    }
    assert_eq!(journal(dir.path()), reviewed);
    let cold = Store::open(dir.path()).unwrap();
    let review = cold.get(&id).unwrap().review.clone().unwrap();
    assert_eq!(
        review,
        Review {
            by: "ctl".to_owned(),
            at_ms: AT + 5,
            note: Some("ran as asked".to_owned()),
        }
    );
    assert_eq!(cold.get(&id).unwrap().stage(), Stage::Reviewed);
}

#[test]
fn a_second_review_without_a_note_is_already_and_with_one_replaces_the_note() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let mut store = Store::open(dir.path()).unwrap();
    let id = shell_request(&mut store, true);
    assert_eq!(
        store.review(AT + 3, &id, "human", None).unwrap(),
        Reviewed::New
    );
    let once = journal(dir.path());
    assert_eq!(
        store.review(AT + 4, &id, "ctl", None).unwrap(),
        Reviewed::Already
    );
    assert_eq!(journal(dir.path()), once, "an empty repeat writes nothing");
    assert_eq!(
        store
            .review(AT + 5, &id, "ctl", Some("second look"))
            .unwrap(),
        Reviewed::Noted
    );
    assert_eq!(
        store
            .review(AT + 6, &id, "ctl", Some("second look"))
            .unwrap(),
        Reviewed::Already
    );
    let cold = Store::open(dir.path()).unwrap();
    let review = cold.get(&id).unwrap().review.clone().unwrap();
    assert_eq!(
        (review.by.as_str(), review.at_ms, review.note.as_deref()),
        ("human", AT + 3, Some("second look")),
        "the first review says who and when; the newest note stands"
    );
}

#[test]
fn keep_request_returns_open_tabs_and_kept_survives_reopen_and_compaction() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let mut store = Store::open(dir.path()).unwrap();
    let id = shell_request(&mut store, false);
    store.tab(AT + 2, &id, TAB_B, BY, false, None).unwrap();
    store
        .left(AT + 3, TAB_B, &closed_by(BY, "shell-exit", Some(0)))
        .unwrap();
    let kept = store.keep_request(AT + 4, &id, "human").unwrap();
    assert_eq!(
        kept,
        KeptRequest {
            tabs: vec![TAB_A.to_owned()],
            already: false,
        }
    );
    let again = store.keep_request(AT + 5, &id, "ctl").unwrap();
    assert!(again.already);
    assert!(matches!(
        store.keep_request(AT + 5, "r-20261109-1411-87be5228-09", "ctl"),
        Err(ActRefusal::Unknown)
    ));
    // Superseded `attach` rows enough that the journal is mostly stale.
    for n in 0..30 {
        store
            .attach(AT + 6 + n, TAB_A, Verb::Send, NODE, BY)
            .unwrap();
    }
    store
        .left(AT + 40, TAB_A, &closed_by(BY, "shell-exit", Some(0)))
        .unwrap();
    assert_eq!(
        store.review(AT + 41, &id, "ctl", Some("kept it")).unwrap(),
        Reviewed::New
    );
    // The next write compacts: the reply's row is the last before the rewrite.
    store.compact_at = 1;
    store
        .reply(AT + 42, &id, "human", "rq.k1", true, "thanks")
        .unwrap();
    let text = String::from_utf8(journal(dir.path())).unwrap();
    assert_eq!(text.matches("op=attach").count(), 1, "compacted: {text}");
    for read in [&store, &Store::open(dir.path()).unwrap()] {
        let request = read.get(&id).unwrap();
        assert_eq!(request.kept, Some(("human".to_owned(), AT + 4)));
        assert_eq!(
            request.review,
            Some(Review {
                by: "ctl".to_owned(),
                at_ms: AT + 41,
                note: Some("kept it".to_owned()),
            })
        );
        assert_eq!(
            request.replies,
            [Reply {
                by: "human".to_owned(),
                at_ms: AT + 42,
                key: "rq.k1".to_owned(),
                sent: true,
            }]
        );
        assert_eq!(read.skipped(), 0);
    }
}

#[test]
fn left_folds_reason_code_and_task() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let mut store = Store::open(dir.path()).unwrap();
    let id = shell_request(&mut store, true);
    let cold = Store::open(dir.path()).unwrap();
    let tab = &cold.get(&id).unwrap().tabs[0];
    assert_eq!(
        tab.gone,
        Some(Gone {
            at_ms: AT + 2,
            reason: "shell-exit".to_owned(),
            code: Some(3),
            task: None,
        })
    );
    assert_eq!(tab.left, Some(Out::Failed));
    // NEGATIVE: a tab still open has left nothing.
    let mut store = Store::open(dir.path()).unwrap();
    let open = shell_request(&mut store, false);
    assert_eq!(store.get(&open).unwrap().tabs[0].gone, None);
}

#[test]
fn said_fold_reads_fallback_why_goal_asked_and_reply() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let mut store = Store::open(dir.path()).unwrap();
    let local = Intake {
        via: Via::CtlSpawn,
        form: Form::Agent("claude".to_owned()),
        from: From {
            node: String::new(),
            host: "m72".to_owned(),
            by: ASKER.to_owned(),
            program: None,
            model: None,
            account: None,
        },
        why: why_of(Via::CtlSpawn, None, None, Some("the goal"), Some("the ask")),
        brief: None,
        goal: Some("the goal".to_owned()),
        asked: Some("the ask".to_owned()),
    };
    let id = store.open_request(AT, local).unwrap();
    let given = shell_request(&mut store, true);
    // A cold reader on the same folder reads what the writer's said rows hold.
    let mut cold = Store::open(dir.path()).unwrap();
    let said = cold.said(&id).unwrap();
    assert_eq!(said.why, Some(("the goal".to_owned(), WhySrc::Goal)));
    assert_eq!(said.goal.as_deref(), Some("the goal"));
    assert_eq!(said.asked.as_deref(), Some("the ask"));
    // NEGATIVE: a request whose why was given says nothing in the content tier.
    assert_eq!(cold.said(&given), None);
    store
        .reply(AT + 9, &given, "human", "rq.k1", true, "done, thanks")
        .unwrap();
    assert!(matches!(
        store.reply(AT + 9, &given, "human", "bad key!", true, "x"),
        Err(ActRefusal::Shape(_))
    ));
    // The cold reader takes the appended rows without reopening.
    assert_eq!(
        cold.said(&given).unwrap().replies,
        [("rq.k1".to_owned(), "done, thanks".to_owned())]
    );
    let (_, status) = cold.inspect().unwrap();
    assert_eq!(status.skipped_rows, 0);
    let request = cold.get(&given).unwrap();
    assert_eq!(
        request.replies,
        [Reply {
            by: "human".to_owned(),
            at_ms: AT + 9,
            key: "rq.k1".to_owned(),
            sent: true,
        }]
    );
    assert!(
        !String::from_utf8(journal(dir.path()))
            .unwrap()
            .contains("done"),
        "a reply's text stays out of the journal"
    );
}

#[test]
fn stage_derives_open_running_concluded_reviewed() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let mut store = Store::open(dir.path()).unwrap();
    let id = store.open_request(AT, fleet_shell(BY, "stages")).unwrap();
    assert_eq!(store.get(&id).unwrap().stage(), Stage::Open);
    store.tab(AT + 1, &id, TAB_A, BY, false, None).unwrap();
    assert_eq!(store.get(&id).unwrap().stage(), Stage::Running);
    store
        .left(AT + 2, TAB_A, &closed_by(BY, "shell-exit", Some(0)))
        .unwrap();
    assert_eq!(store.get(&id).unwrap().stage(), Stage::Concluded);
    store.review(AT + 3, &id, "human", None).unwrap();
    assert_eq!(store.get(&id).unwrap().stage(), Stage::Reviewed);
    assert_eq!(
        [
            Stage::Open,
            Stage::Running,
            Stage::Concluded,
            Stage::Reviewed
        ]
        .map(Stage::word),
        ["open", "running", "concluded", "reviewed"]
    );
}

#[test]
fn inspection_marks_a_bounded_cold_read_as_partial() {
    use std::io::{Seek as _, Write as _};
    let root = aterm_tempfile::tempdir().unwrap();
    let mut store = Store::open(root.path()).unwrap();
    let id = store
        .open_request(AT, fleet_shell(BY, "retained tail"))
        .unwrap();
    let (_, status) = store.inspect().unwrap();
    assert!(status.complete);
    let retained = journal(root.path());
    drop(store);
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .truncate(true)
        .open(root.path().join(crate::JOURNAL))
        .unwrap();
    file.set_len(READ_MAX).unwrap();
    file.seek(std::io::SeekFrom::End(0)).unwrap();
    file.write_all(b"\n").unwrap();
    file.write_all(&retained).unwrap();
    drop(file);
    let mut reopened = Store::open(root.path()).unwrap();
    let (requests, status) = reopened.inspect().unwrap();
    assert!(!status.complete);
    assert_eq!(
        requests
            .map(|request| request.id.as_str())
            .collect::<Vec<_>>(),
        vec![id.as_str()]
    );
}

#[test]
fn inspection_refuses_a_broken_journal_where_the_cache_would_answer() {
    use std::io::Write as _;
    let root = aterm_tempfile::tempdir().unwrap();
    let mut store = Store::open(root.path()).unwrap();
    let id = store.open_request(AT, fleet_shell(BY, "cached")).unwrap();
    let path = root.path().join(crate::JOURNAL);
    std::fs::OpenOptions::new()
        .append(true)
        .open(&path)
        .unwrap()
        .write_all(b"foreign row\n")
        .unwrap();
    let before = journal(root.path());
    let (_, status) = store.inspect().unwrap();
    assert_eq!(
        status,
        InspectionStatus {
            skipped_rows: 1,
            complete: true,
        }
    );
    assert_eq!(journal(root.path()), before, "an inspection writes nothing");
    std::fs::remove_file(&path).unwrap();
    std::fs::create_dir(&path).unwrap();
    // NEGATIVE: the display cache still answers from what it last read; an inspection refuses.
    assert_eq!(
        store.requests().map(|r| r.id.clone()).collect::<Vec<_>>(),
        [id]
    );
    assert!(store.inspect().is_err());
}
