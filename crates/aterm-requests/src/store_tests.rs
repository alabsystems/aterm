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
const BY: &str = "fleet:87be5228:tqn5r6e0148";
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
fn a_historical_fleet_request_survives_reopening_and_archiving() {
    let dir = aterm_tempfile::tempdir().unwrap();
    drop(Store::open(dir.path()).unwrap());
    let id = "r-20261109-1411-87be5228-01";
    let by = "fleet:87be5228:s-0123456789abcdef0123";
    // These are rows an earlier build wrote, independent of today's actor constructors and
    // record encoder. Reading them must preserve authorship and every attached verb.
    let rows = [
        format!(
            "t={AT} op=open req={id} via=fleet-shell form=shell node={NODE} host=m114 \
             by={by} program=- model=- account=- why_src=none"
        ),
        format!("t={} op=tab req={id} tab={TAB_A} by={by}", AT + 1),
        format!(
            "t={} op=attach req={id} tab={TAB_A} verb=turn n=2 by={by}",
            AT + 2
        ),
    ];
    std::fs::write(
        dir.path().join(crate::JOURNAL),
        format!("{}\n", rows.join("\n")),
    )
    .unwrap();

    let mut reopened = Store::open(dir.path()).unwrap();
    let request = reopened.get(id).expect("the historical open is retained");
    assert_eq!(request.open.by, by);
    assert_eq!(request.open.node, NODE);
    assert_eq!(request.state(), State::Running);
    assert_eq!(request.tabs.len(), 1);
    assert_eq!(request.tabs[0].sid, TAB_A);
    assert_eq!(request.tabs[0].verbs.get(&Verb::Turn), Some(&2));
    assert_eq!(request.tabs[0].last_ms, AT + 2);
    assert_eq!(
        reopened.owner(TAB_A),
        Some((id.to_owned(), NODE.to_owned(), by.to_owned()))
    );
    assert_eq!(reopened.skipped(), 0);
    let (archived, said) = lines_of(dir.path(), id).unwrap();
    assert_eq!(archived, rows);
    assert!(said.is_empty());
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
    assert!(old.join(&second, NODE, BY, Via::FleetShell).is_ok());
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
        (NODE, "fleet:c0ffee00:tqn5r6e0148"),
        // Another node whose head is the requester's: the same token, a different Mac.
        (TWIN, BY),
        // The same node, another session.
        (NODE, "fleet:87be5228:t0123456789"),
        (NODE, "fleet:87be5228:tqn5r6e0149"),
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
        refusal(store.join("r-20261109-1411-87be5228-99", NODE, BY, Via::FleetShell)),
        Err("no request of that id on this Mac".to_owned())
    );
    assert_eq!(
        refusal(store.join(&theirs, NODE, BY, Via::FleetShell)),
        Err("another requester's request".to_owned())
    );
    // NEGATIVE CONTROL: the requester's token from another Mac with the same node head.
    assert_eq!(
        refusal(store.join(&mine, TWIN, BY, Via::FleetShell)),
        Err("another requester's request".to_owned())
    );
    assert_eq!(
        refusal(store.join(&done, NODE, BY, Via::FleetShell)),
        Err("the request has concluded".to_owned())
    );
    assert_eq!(
        refusal(store.join(&mine, NODE, BY, Via::FleetShell)),
        Ok(())
    );
    // A plain fleet shell joins only a plain fleet shell's request, which a lease ends: joined
    // to a worker's request it would be leased by none. Nor does a worker join a shell's.
    let worker = store
        .open_request(
            AT,
            Intake {
                via: Via::FleetWorker,
                form: Form::Agent("claude".to_owned()),
                ..fleet_shell(BY, "a worker")
            },
        )
        .unwrap();
    let shell_into_worker = refusal(store.join(&worker, NODE, BY, Via::FleetShell));
    assert!(
        shell_into_worker.as_ref().is_err_and(|why| why
            .starts_with("the request came in as fleet-worker and this spawn as fleet-shell")
            && why.contains("spawn it without req= to open its own")),
        "{shell_into_worker:?}"
    );
    assert!(refusal(store.join(&mine, NODE, BY, Via::FleetWorker)).is_err());
    assert_eq!(
        refusal(store.join(&worker, NODE, BY, Via::FleetWorker)),
        Ok(())
    );
    store.refused(AT, &worker, true).unwrap();
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
    store.join(&id, NODE, BY, Via::FleetShell).unwrap();
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
    store.join(&other, NODE, BY, Via::FleetShell).unwrap();
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
    store.join(&waits, NODE, BY, Via::FleetShell).unwrap();
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
    let inode = |path: &Path| line_log::identity_at(path).unwrap();
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
        let id = line_log::identity_at(path).unwrap();
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

/// Append `lines` to the journal as another build writes them, byte for byte.
fn append_raw(dir: &Path, lines: &[Vec<u8>]) {
    use std::io::Write as _;
    let mut file = std::fs::OpenOptions::new()
        .append(true)
        .open(dir.join(crate::JOURNAL))
        .unwrap();
    for line in lines {
        file.write_all(line).unwrap();
        file.write_all(b"\n").unwrap();
    }
}

/// Two builds share a state home (a seamless update's overlap, an update reverted): an older
/// build's compaction keeps every line it cannot read, byte for byte and in its place, so a
/// newer build's records (a kind, a word or a key the older one lacks, a value that is no
/// UTF-8, an `attach` of a later shape) survive it. NEGATIVE CONTROL: the superseded `attach`es
/// it reads still go, the last with the count.
#[test]
fn compaction_keeps_every_row_a_newer_build_wrote_as_written() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let mut store = Store::open(dir.path()).unwrap();
    store.compact_at = u64::MAX;
    let id = store.open_request(AT, fleet_shell(BY, "shared")).unwrap();
    store.tab(AT, &id, TAB_A, BY, false, None).unwrap();
    let newer: Vec<Vec<u8>> = vec![
        format!("t={} op=verdict req={id} by=human note=-", AT + 1).into_bytes(),
        format!("t={} op=end req={id} out=released why=superseded", AT + 2).into_bytes(),
        format!("t={} op=keep req={id} tab={TAB_A} by=human seat=1", AT + 3).into_bytes(),
        [
            format!("t={} op=review req={id} note=", AT + 4).as_bytes(),
            b"\xff\xfe by=human",
        ]
        .concat(),
        format!(
            "t={} op=attach req={id} tab={TAB_A} verb=turn n=1 by={BY} seat=1",
            AT + 5
        )
        .into_bytes(),
    ];
    for (n, line) in (0..).zip(&newer) {
        for k in 0..10 {
            store
                .attach(AT + 10 + n * 10 + k, TAB_A, Verb::Turn, NODE, BY)
                .unwrap();
        }
        append_raw(dir.path(), std::slice::from_ref(line));
    }
    let before = journal(dir.path());
    let is_attach = |line: &[u8]| {
        std::str::from_utf8(line)
            .ok()
            .and_then(Rec::parse)
            .is_some_and(|rec| matches!(rec.body, Body::Attach { .. }))
    };
    let inode = |path: &Path| {
        let id = line_log::identity_at(path).unwrap();
        (id.dev, id.ino)
    };
    let old = inode(&dir.path().join(crate::JOURNAL));
    // The next append passes the bound: the older build (this one) compacts.
    store.compact_at = 1;
    assert!(store.attach(AT + 99, TAB_A, Verb::Turn, NODE, BY).unwrap());
    assert_ne!(
        inode(&dir.path().join(crate::JOURNAL)),
        old,
        "never compacted"
    );
    let after = journal(dir.path());
    let last = format!(
        "t={} op=attach req={id} tab={TAB_A} verb=turn n=51 by={BY}\n",
        AT + 99
    );
    let mut want: Vec<u8> = before
        .split_inclusive(|b| *b == b'\n')
        .filter(|line| !is_attach(line.strip_suffix(b"\n").unwrap_or(line)))
        .flatten()
        .copied()
        .collect();
    want.extend_from_slice(last.as_bytes());
    assert_eq!(
        String::from_utf8_lossy(&after),
        String::from_utf8_lossy(&want)
    );
    assert_eq!(after, want, "byte for byte");
    for line in &newer {
        assert_eq!(
            after
                .split(|b| *b == b'\n')
                .filter(|kept| kept == line)
                .count(),
            1,
            "{}",
            String::from_utf8_lossy(line)
        );
    }
    // Every store reads it as before: the count kept, the newer rows skipped, never applied.
    let fresh = Store::open(dir.path()).unwrap();
    let request = fresh.get(&id).unwrap();
    assert_eq!(request.tabs[0].verbs.get(&Verb::Turn), Some(&51));
    assert_eq!(request.state(), State::Running);
    assert_eq!(fresh.skipped(), 5);
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
    store.join(&id, NODE, BY, Via::FleetShell).unwrap();
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
    store.join(&joining, NODE, BY, Via::FleetShell).unwrap();
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
        let id = line_log::identity_at(path).unwrap();
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

/// A local agent's spawn whose why is `why` (else its goal's), its tab's goal and asked lines
/// `goal` and `asked`.
fn local_agent(why: Option<&str>, goal: Option<&str>, asked: Option<&str>) -> Intake {
    Intake {
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
        why: why_of(Via::CtlSpawn, why, None, goal, asked),
        brief: None,
        goal: goal.map(str::to_owned),
        asked: asked.map(str::to_owned),
    }
}

/// The content rows a spawn wrote at `t_ms` under `id` before its `open` was lost (a crash, a
/// failed append between the two writes): its goal's why, its goal and its asked line.
fn lost_intake(dir: &Path, t_ms: u64, id: &str) {
    use std::io::Write as _;
    let rows = [
        SaidBody::Why {
            text: "migrate the billing DB".to_owned(),
            src: WhySrc::Goal,
        },
        SaidBody::Goal {
            text: "migrate the billing DB".to_owned(),
        },
        SaidBody::Asked {
            text: "drop the old table?".to_owned(),
        },
    ];
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(dir.join(crate::SAID))
        .unwrap();
    for body in rows {
        let line = Said {
            t_ms,
            id: id.to_owned(),
            body,
        }
        .line();
        writeln!(file, "{line}").unwrap();
    }
}

/// A spawn's content rows whose `open` never reached the journal are no later request's: the
/// next request of that minute and head is minted past their id, and says only what it said.
/// NEGATIVE CONTROL: a request whose `open` was written reads its own rows.
#[test]
fn content_rows_whose_open_was_lost_lend_nothing_to_a_later_request() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let mut store = Store::open(dir.path()).unwrap();
    let lost = "r-20261109-1411-local-01";
    lost_intake(dir.path(), AT, lost);
    // A spawn of the same minute gives its why, and its tab has no goal or asked line yet.
    let id = store
        .open_request(AT + 35_000, local_agent(Some("run the tests"), None, None))
        .unwrap();
    assert_eq!(id, "r-20261109-1411-local-02", "minted past the lost id");
    assert_eq!(store.said(&id), None);
    assert_eq!(Store::open(dir.path()).unwrap().said(&id), None);
    assert_eq!(store.said(lost), None, "no request's");
    let own = store
        .open_request(
            AT + 36_000,
            local_agent(None, Some("its own goal"), Some("its own ask")),
        )
        .unwrap();
    assert_eq!(own, "r-20261109-1411-local-03");
    let said = Store::open(dir.path()).unwrap().said(&own).unwrap();
    assert_eq!(said.goal.as_deref(), Some("its own goal"));
    assert_eq!(said.asked.as_deref(), Some("its own ask"));
}

/// Where a build that mints from the journal alone opened a lost spawn's id again, the lost
/// content rows are still not its: a request's own are those written with its `open`.
/// NEGATIVE CONTROL: its own row, at its `open`'s time, is read.
#[test]
fn a_reopened_id_reads_only_the_rows_written_with_its_open() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let lost = "r-20261109-1411-local-01";
    lost_intake(dir.path(), AT, lost);
    let opened = AT + 35_000;
    let intake = local_agent(Some("run the tests"), None, None);
    let rows = [
        Body::Open(Open {
            via: intake.via,
            form: intake.form,
            node: String::new(),
            host: intake.from.host,
            by: intake.from.by,
            program: None,
            model: None,
            account: None,
            why_src: WhySrc::Given,
        }),
        Body::Why {
            text: "run the tests".to_owned(),
        },
    ]
    .map(|body| {
        Rec {
            t_ms: opened,
            id: lost.to_owned(),
            body,
        }
        .line()
        .into_bytes()
    });
    std::fs::write(dir.path().join(crate::JOURNAL), b"").unwrap();
    append_raw(dir.path(), &rows);
    let mut store = Store::open(dir.path()).unwrap();
    assert_eq!(
        store.get(lost).map(|request| request.opened_ms),
        Some(opened)
    );
    assert_eq!(store.said(lost), None);
    let mut file = std::fs::OpenOptions::new()
        .append(true)
        .open(dir.path().join(crate::SAID))
        .unwrap();
    let asked = Said {
        t_ms: opened,
        id: lost.to_owned(),
        body: SaidBody::Asked {
            text: "its own ask".to_owned(),
        },
    }
    .line();
    std::io::Write::write_all(&mut file, format!("{asked}\n").as_bytes()).unwrap();
    let said = store.said(lost).unwrap();
    assert_eq!(said.asked.as_deref(), Some("its own ask"));
    assert_eq!((said.why, said.goal), (None, None));
}

/// A spawn's content row cut short under its id (a short write on a full disk, a power loss,
/// before its `open`) keeps that id from being minted again: the read made to mint finishes the
/// row, as the next append would, so no later request's rows are written under its id.
/// NEGATIVE CONTROL: the finished row is still no request's, and the new request reads its own.
#[test]
fn a_content_row_cut_short_keeps_its_id_from_being_minted_again() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let lost = "r-20261109-1411-local-01";
    let row = Said {
        t_ms: AT,
        id: lost.to_owned(),
        body: SaidBody::Asked {
            text: "drop the old table".to_owned(),
        },
    }
    .line();
    // Cut inside its text, where what was written still reads as a row.
    let cut = &row[..row.len() - 3];
    std::fs::write(dir.path().join(crate::SAID), cut).unwrap();
    let mut store = Store::open(dir.path()).unwrap();
    let id = store
        .open_request(AT + 35_000, local_agent(None, Some("its own goal"), None))
        .unwrap();
    assert_eq!(
        id, "r-20261109-1411-local-02",
        "minted past the cut row's id"
    );
    let said = std::fs::read_to_string(dir.path().join(crate::SAID)).unwrap();
    assert!(
        said.starts_with(&format!("{cut}\n")) && !said[cut.len() + 1..].contains(lost),
        "the cut row is finished where it was, and nothing more is written under its id: {said}"
    );
    assert_eq!(store.said(lost), None, "no request's");
    let own = Store::open(dir.path()).unwrap().said(&id).unwrap();
    assert_eq!(own.goal.as_deref(), Some("its own goal"));
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

fn every_tab(_: &str) -> bool {
    true
}

fn plan_of(start: CleanStart) -> CleanPlan {
    match start {
        CleanStart::Plan(plan) => *plan,
        CleanStart::Already => panic!("a plan"),
    }
}

#[test]
fn begin_clean_refuses_running_without_stop_kept_and_cleaned_and_writes_an_implicit_review() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let mut store = Store::open(dir.path()).unwrap();
    let running = shell_request(&mut store, false);
    let before = journal(dir.path());
    // A running request is no cleanup's without stop=1, and nothing is written.
    assert!(matches!(
        store.begin_clean(AT + 10, &running, "human", false, &every_tab),
        Err(ActRefusal::Live)
    ));
    // A standing tab another aterm hosts is that aterm's to close.
    assert!(matches!(
        store.begin_clean(AT + 10, &running, "human", true, &|_| false),
        Err(ActRefusal::Elsewhere(sid)) if sid == TAB_A
    ));
    assert!(matches!(
        store.begin_clean(
            AT + 10,
            "r-20261109-1411-87be5228-09",
            "human",
            true,
            &every_tab
        ),
        Err(ActRefusal::Unknown)
    ));
    assert_eq!(journal(dir.path()), before);
    // NEGATIVE: with stop=1 it begins, and the person's clean is their review.
    let plan = plan_of(
        store
            .begin_clean(AT + 11, &running, "human", true, &every_tab)
            .unwrap(),
    );
    assert!(!plan.resumed);
    assert_eq!(plan.at_ms, AT + 11);
    assert_eq!(plan.request.stage(), Stage::Cleaning);
    let review = plan.request.review.clone().unwrap();
    assert_eq!((review.by.as_str(), review.at_ms), ("human", AT + 11));
    assert_eq!(
        plan.request.clean,
        Some(CleanBegan {
            by: "human".to_owned(),
            at_ms: AT + 11,
            stop: true
        })
    );
    // A concluded, reviewed request writes no second review.
    let concluded = {
        let id = store
            .open_request(AT + 20, fleet_shell(BY, "second"))
            .unwrap();
        store.tab(AT + 21, &id, TAB_B, BY, false, None).unwrap();
        store
            .left(AT + 22, TAB_B, &closed_by(BY, "shell-exit", Some(0)))
            .unwrap();
        store.review(AT + 23, &id, "ctl", Some("fine")).unwrap();
        id
    };
    let plan = plan_of(
        store
            .begin_clean(AT + 24, &concluded, "human", false, &every_tab)
            .unwrap(),
    );
    assert_eq!(plan.request.review.as_ref().unwrap().by, "ctl");
    let text = String::from_utf8(journal(dir.path())).unwrap();
    assert_eq!(
        text.matches(&format!("op=review req={concluded}")).count(),
        1
    );
    store
        .end_clean(
            AT + 25,
            &concluded,
            CleanEnd::Cleaned {
                tabs: 0,
                deleted: 0,
                kept: 0,
                failed: 0,
                by: "human".to_owned(),
            },
        )
        .unwrap();
    assert_eq!(store.get(&concluded).unwrap().stage(), Stage::Cleaned);
    let before = journal(dir.path());
    assert_eq!(
        store
            .begin_clean(AT + 26, &concluded, "human", false, &every_tab)
            .unwrap(),
        CleanStart::Already
    );
    assert!(matches!(
        store.keep_request(AT + 27, &concluded, "human"),
        Err(ActRefusal::Cleaned)
    ));
    assert_eq!(journal(dir.path()), before, "neither wrote");
    // A kept request is never cleaned.
    let kept = {
        let id = store
            .open_request(AT + 30, fleet_shell(BY, "third"))
            .unwrap();
        store.tab(AT + 31, &id, TAB_C, BY, false, None).unwrap();
        store
            .left(AT + 32, TAB_C, &closed_by(BY, "shell-exit", Some(0)))
            .unwrap();
        store.keep_request(AT + 33, &id, "human").unwrap();
        id
    };
    assert!(matches!(
        store.begin_clean(AT + 34, &kept, "human", true, &every_tab),
        Err(ActRefusal::Kept)
    ));
}

// Its claims are Unix paths.
#[cfg(unix)]
#[test]
fn gone_and_cleaned_fold_and_a_cut_clean_reads_cleaning_and_resumes_skipping_gone_paths() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let mut store = Store::open(dir.path()).unwrap();
    let id = shell_request(&mut store, true);
    store
        .born(
            AT + 3,
            &[
                born(&id, "/Users//a/x", Some(true)),
                born(&id, "/Users//a/y", Some(true)),
            ],
        )
        .unwrap();
    // NEGATIVE: no cleanup began, so no cleanup row is taken.
    let gone_x = GoneClaim {
        at_ms: AT + 12,
        path: "/Users//a/x".to_owned(),
        bytes: Some(10),
        why: None,
        did: Did::Deleted,
    };
    assert_eq!(
        store.gone(&id, &gone_x).unwrap_err().kind(),
        io::ErrorKind::InvalidInput
    );
    plan_of(
        store
            .begin_clean(AT + 10, &id, "human", false, &every_tab)
            .unwrap(),
    );
    let archive = Archive {
        at_ms: AT + 11,
        dir: id.clone(),
        bytes: 100,
        files: 3,
        listed: 2,
        whole: true,
    };
    store.archived(&id, &archive).unwrap();
    store.gone(&id, &gone_x).unwrap();
    // NEGATIVE: a folder the request does not claim takes no row.
    let stray = GoneClaim {
        path: "/Users//a/z".to_owned(),
        ..gone_x.clone()
    };
    assert_eq!(
        store.gone(&id, &stray).unwrap_err().kind(),
        io::ErrorKind::InvalidInput
    );
    // A cut: the process ends here. A cold reader reads it cleaning, with what it did.
    drop(store);
    let mut store = Store::open(dir.path()).unwrap();
    let request = store.get(&id).unwrap().clone();
    assert_eq!(request.stage(), Stage::Cleaning);
    assert_eq!(request.archives, vec![archive.clone()]);
    assert_eq!(request.gone, vec![gone_x.clone()]);
    // Cleaning it again resumes it: the folder deleted stays deleted.
    let plan = plan_of(
        store
            .begin_clean(AT + 20, &id, "ctl", false, &every_tab)
            .unwrap(),
    );
    assert!(plan.resumed);
    assert_eq!(
        plan.request.gone_of("/Users//a/x").map(|g| g.did),
        Some(Did::Deleted)
    );
    assert_eq!(plan.request.gone_of("/Users//a/y"), None);
    let kept_y = GoneClaim {
        at_ms: AT + 21,
        path: "/Users//a/y".to_owned(),
        bytes: None,
        why: Some(Keep::Unpushed),
        did: Did::Kept,
    };
    store.gone(&id, &kept_y).unwrap();
    // The resumed cleanup's own archive is recorded beside the first, which still holds what the
    // cut cleanup listed of the folder it deleted.
    let again = Archive {
        at_ms: AT + 21,
        dir: format!("{id}-2"),
        bytes: 10,
        files: 0,
        listed: 1,
        whole: true,
    };
    store.archived(&id, &again).unwrap();
    store
        .end_clean(
            AT + 22,
            &id,
            CleanEnd::Cleaned {
                tabs: 0,
                deleted: 1,
                kept: 1,
                failed: 0,
                by: "ctl".to_owned(),
            },
        )
        .unwrap();
    // NEGATIVE: a finished cleanup takes no more rows.
    assert_eq!(
        store
            .end_clean(AT + 23, &id, CleanEnd::Uncleaned(Unclean::Store))
            .unwrap_err()
            .kind(),
        io::ErrorKind::InvalidInput
    );
    let fresh = Store::open(dir.path()).unwrap();
    let request = fresh.get(&id).unwrap();
    assert_eq!(request.stage(), Stage::Cleaned);
    assert_eq!(request.archives, vec![archive, again]);
    assert_eq!(request.gone, vec![gone_x, kept_y]);
    assert_eq!(
        request
            .cleaned
            .as_ref()
            .map(|c| (c.deleted, c.kept, c.by.as_str())),
        Some((1, 1, "ctl"))
    );
    assert_eq!(fresh.skipped(), 0);
    // The record rows of the request are read back whole, its said rows apart.
    let (rows, said) = lines_of(dir.path(), &id).unwrap();
    assert!(rows.iter().all(|row| row.contains(&format!(" req={id} "))));
    assert!(rows.iter().any(|row| row.contains(" op=cleaned ")));
    assert!(said.is_empty());
    assert_eq!(
        rows.len(),
        String::from_utf8(journal(dir.path()))
            .unwrap()
            .lines()
            .count()
    );
    // NEGATIVE: another request's rows are not its.
    assert!(
        lines_of(dir.path(), "r-20261109-1411-87be5228-09")
            .unwrap()
            .0
            .is_empty()
    );
}

#[test]
fn uncleaned_returns_the_stage_to_reviewed() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let mut store = Store::open(dir.path()).unwrap();
    let id = shell_request(&mut store, true);
    assert_eq!(store.get(&id).unwrap().stage(), Stage::Concluded);
    plan_of(
        store
            .begin_clean(AT + 10, &id, "human", false, &every_tab)
            .unwrap(),
    );
    assert_eq!(store.get(&id).unwrap().stage(), Stage::Cleaning);
    store
        .end_clean(AT + 11, &id, CleanEnd::Uncleaned(Unclean::Busy))
        .unwrap();
    let request = Store::open(dir.path()).unwrap().get(&id).unwrap().clone();
    assert_eq!(request.stage(), Stage::Reviewed);
    assert_eq!(request.uncleaned, Some((Unclean::Busy, AT + 11)));
    // A later cleanup begins afresh, not resumed.
    let plan = plan_of(
        store
            .begin_clean(AT + 12, &id, "human", false, &every_tab)
            .unwrap(),
    );
    assert!(!plan.resumed);
    assert_eq!(plan.request.stage(), Stage::Cleaning);
}

#[test]
fn a_request_kept_while_its_cleanup_was_cut_leaves_cleaning() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let mut store = Store::open(dir.path()).unwrap();
    let id = shell_request(&mut store, true);
    plan_of(
        store
            .begin_clean(AT + 10, &id, "human", false, &every_tab)
            .unwrap(),
    );
    // A quit cuts the cleanup here.
    assert_eq!(store.get(&id).unwrap().stage(), Stage::Cleaning);
    store.keep_request(AT + 11, &id, "human").unwrap();
    let request = Store::open(dir.path()).unwrap().get(&id).unwrap().clone();
    assert_eq!(request.stage(), Stage::Reviewed, "the person's now");
    // NEGATIVE: no cleanup resumes it.
    assert!(matches!(
        store.begin_clean(AT + 12, &id, "human", false, &every_tab),
        Err(ActRefusal::Kept)
    ));
}

/// A tab opened before the `t` form, which keeps its `s-` ID until aterm restores or closes it.
const LEGACY_TAB: &str = "s-0123456789abcdef0123";
/// Another such tab: an agent's, which asks for requests of its own.
const LEGACY_ASKER: &str = "s-fedcba98765432100123";
/// A fleet drive whose requester is a legacy tab on the driving Mac.
const LEGACY_BY: &str = "fleet:87be5228:s-fedcba98765432100123";

/// The journal a build before the `t` form left, each row the bytes that build wrote (no row
/// goes through this build's encoder, and each is held to be the one line its record has): a
/// fleet drive from a legacy tab opened a request and was given a legacy tab, and a local legacy
/// tab opened one of its own.
fn earlier_journal(dir: &Path) -> (String, String) {
    let fleet = "r-20261109-1411-87be5228-01".to_owned();
    let local = "r-20261109-1411-local-01".to_owned();
    let lines = [
        format!(
            "t={AT} op=open req={fleet} via=fleet-shell form=shell node={NODE} host=m114 \
             by={LEGACY_BY} program=claude model=opus-5.5 account=- why_src=none"
        ),
        format!(
            "t={} op=tab req={fleet} tab={LEGACY_TAB} by={LEGACY_BY}",
            AT + 1
        ),
        format!(
            "t={} op=attach req={fleet} tab={LEGACY_TAB} verb=turn n=1 by={LEGACY_BY}",
            AT + 2
        ),
        format!(
            "t={AT} op=open req={local} via=ctl-spawn form=agent:claude node=- host=m72 \
             by={LEGACY_ASKER} program=- model=- account=- why_src=none"
        ),
        format!(
            "t={} op=tab req={local} tab={TAB_A} by={LEGACY_ASKER}",
            AT + 3
        ),
    ];
    for line in &lines {
        let rec = Rec::parse(line).unwrap_or_else(|| panic!("no record: {line}"));
        assert_eq!(&rec.line(), line, "not the line its record has");
    }
    std::fs::write(dir.join(crate::JOURNAL), lines.join("\n") + "\n").unwrap();
    (fleet, local)
}

/// A Mac updated over tabs opened before the `t` form: the journal names them by their `s-` IDs,
/// as a tab (`tab=`), as a local requester (`by=`) and inside a fleet drive's token
/// (`by=fleet:<head>:`). Every such row is read, so each request is still its requester's and
/// each tab still its request's: the requester's next verb counts on from the stored one, and
/// the tab's leaving concludes the request. NEGATIVE CONTROL: a row whose `s-` word is loose is
/// skipped and counted, and names no tab.
#[test]
fn a_journal_an_earlier_build_wrote_for_legacy_tabs_is_folded_whole() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let (fleet, local) = earlier_journal(dir.path());
    let mut store = Store::open(dir.path()).unwrap();
    assert_eq!(store.skipped(), 0, "every row an earlier build wrote reads");
    assert_eq!(store.get(&fleet).unwrap().open.by, LEGACY_BY);
    assert_eq!(store.get(&fleet).unwrap().state(), State::Running);
    assert_eq!(store.get(&local).unwrap().open.by, LEGACY_ASKER);
    assert_eq!(
        store.owner(LEGACY_TAB),
        Some((fleet.clone(), NODE.to_owned(), LEGACY_BY.to_owned()))
    );
    assert_eq!(
        store.owner(TAB_A),
        Some((local.clone(), String::new(), LEGACY_ASKER.to_owned()))
    );
    // The requester's next verb is its own, and counts on from the stored one.
    assert!(
        store
            .attach(AT + 4, LEGACY_TAB, Verb::Turn, NODE, LEGACY_BY)
            .unwrap()
    );
    assert!(
        String::from_utf8(journal(dir.path()))
            .unwrap()
            .contains(&format!(
                "op=attach req={fleet} tab={LEGACY_TAB} verb=turn n=2 by={LEGACY_BY}"
            ))
    );
    // NEGATIVE CONTROL: another legacy tab of the same Mac is not that requester.
    assert!(
        !store
            .attach(
                AT + 5,
                LEGACY_TAB,
                Verb::Turn,
                NODE,
                &format!("fleet:87be5228:{LEGACY_TAB}")
            )
            .unwrap()
    );
    assert_eq!(
        store
            .left(
                AT + 6,
                LEGACY_TAB,
                &closed_by(LEGACY_BY, "shell-exit", Some(0))
            )
            .unwrap(),
        Some(Out::Done)
    );
    // NEGATIVE CONTROL: loose `s-` words are no tab and no requester.
    append_raw(
        dir.path(),
        &[
            format!("t={} op=tab req={local} tab=s-1 by={LEGACY_ASKER}", AT + 7).into_bytes(),
            format!(
                "t={} op=tab req={local} tab={TAB_B} by=s-0123456789ABCDEF0123",
                AT + 7
            )
            .into_bytes(),
            format!(
                "t={} op=tab req={local} tab={TAB_C} by=fleet:87be5228:s-0123456789abcdef01234",
                AT + 7
            )
            .into_bytes(),
        ],
    );
    let mut fresh = Store::open(dir.path()).unwrap();
    assert_eq!(fresh.skipped(), 3);
    assert_eq!(fresh.owner("s-1"), None);
    assert_eq!(fresh.owner(TAB_B), None);
    assert_eq!(fresh.owner(TAB_C), None);
    assert_eq!(
        fresh.get(&fleet).unwrap().state(),
        State::Concluded(Out::Done)
    );
    assert_eq!(
        fresh.get(&fleet).unwrap().tabs[0]
            .verbs
            .get(&Verb::Turn)
            .copied(),
        Some(2)
    );
    assert_eq!(fresh.get(&local).unwrap().tabs.len(), 1);
}

/// This build compacts a journal an earlier build wrote for legacy tabs. It reads a legacy
/// tab's `attach` as one, so the superseded ones go and the last carries the count, and every
/// other row naming a legacy tab stays as it was written, in its place. NEGATIVE CONTROL: the
/// journal is not rewritten before the superseded rows are half of it.
#[test]
fn a_compaction_keeps_a_legacy_tabs_rows_and_drops_its_superseded_attaches() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let (fleet, _) = earlier_journal(dir.path());
    let before = String::from_utf8(journal(dir.path())).unwrap();
    let mut store = Store::open(dir.path()).unwrap();
    store.compact_at = 1;
    let path = dir.path().join(crate::JOURNAL);
    let inode = |path: &Path| {
        let id = line_log::identity_at(path).unwrap();
        (id.dev, id.ino)
    };
    let first = inode(&path);
    assert!(
        store
            .attach(AT + 10, LEGACY_TAB, Verb::Turn, NODE, LEGACY_BY)
            .unwrap()
    );
    assert_eq!(inode(&path), first, "CONTROL: rewritten for one stale row");
    for n in 1..40 {
        assert!(
            store
                .attach(AT + 10 + n, LEGACY_TAB, Verb::Turn, NODE, LEGACY_BY)
                .unwrap()
        );
    }
    assert_ne!(inode(&path), first, "never compacted");
    let text = String::from_utf8(journal(dir.path())).unwrap();
    let rest = |text: &str| -> Vec<String> {
        text.lines()
            .filter(|line| !line.contains(" op=attach "))
            .map(str::to_owned)
            .collect()
    };
    assert_eq!(
        rest(&text),
        rest(&before),
        "a stored row was lost or changed"
    );
    let attach = format!("op=attach req={fleet} tab={LEGACY_TAB} verb=turn n=");
    assert_eq!(
        text.matches(" op=attach ").count(),
        text.matches(&attach).count()
    );
    assert!(text.matches(&attach).count() < 40, "{text}");
    assert!(!text.contains(&format!("{attach}1 ")), "{text}");
    assert!(
        text.contains(&format!("{attach}41 by={LEGACY_BY}")),
        "{text}"
    );
    let mut fresh = Store::open(dir.path()).unwrap();
    assert_eq!(fresh.skipped(), 0);
    assert_eq!(
        fresh.owner(LEGACY_TAB),
        Some((fleet.clone(), NODE.to_owned(), LEGACY_BY.to_owned()))
    );
    assert_eq!(
        fresh.get(&fleet).unwrap().tabs[0].verbs.get(&Verb::Turn),
        Some(&41)
    );
}

/// A shell still running in a tab opened before the `t` form asks under its `s-` ID. Its
/// request is opened under that ID, so its own `req=` joins it and no other tab's does, and the
/// tab its spawn made is recorded as that requester's. NEGATIVE CONTROL: a loose `s-` word is
/// no requester: its request is opened by `-`, which nothing joins.
#[test]
fn a_live_legacy_shell_opens_a_request_and_joins_it_by_req() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let mut store = Store::open(dir.path()).unwrap();
    let from = |by: &str| {
        let mut intake = local_agent(Some("build it"), None, None);
        intake.from.by = by.to_owned();
        intake
    };
    let refusal = |result: Result<(), Refusal>| result.map_err(|refusal| refusal.to_string());
    let id = store.open_request(AT, from(LEGACY_ASKER)).unwrap();
    assert_eq!(store.get(&id).unwrap().open.by, LEGACY_ASKER);
    assert_eq!(
        refusal(store.join(&id, "", LEGACY_TAB, Via::CtlSpawn)),
        Err("another requester's request".to_owned()),
        "CONTROL: another legacy tab"
    );
    assert_eq!(
        refusal(store.join(&id, "", LEGACY_ASKER, Via::CtlSpawn)),
        Ok(())
    );
    store
        .tab(AT + 1, &id, TAB_A, LEGACY_ASKER, true, None)
        .unwrap();
    // A fleet drive from a legacy tab is its own requester too.
    let driven = store
        .open_request(AT, fleet_shell(LEGACY_BY, "driven"))
        .unwrap();
    assert_eq!(store.get(&driven).unwrap().open.by, LEGACY_BY);
    assert_eq!(
        refusal(store.join(
            &driven,
            NODE,
            &format!("fleet:87be5228:{LEGACY_TAB}"),
            Via::FleetShell
        )),
        Err("another requester's request".to_owned()),
        "CONTROL: another legacy tab of the driving Mac"
    );
    assert_eq!(
        refusal(store.join(&driven, NODE, LEGACY_BY, Via::FleetShell)),
        Ok(())
    );
    store.refused(AT + 1, &driven, true).unwrap();
    for loose in [
        "s-1",
        "s-0123456789abcdef01234",
        "s-0123456789ABCDEF0123",
        "fleet:87be5228:s-1",
    ] {
        let none = store.open_request(AT + 2, from(loose)).unwrap();
        assert_eq!(store.get(&none).unwrap().open.by, "-", "CONTROL: {loose}");
        assert_eq!(
            refusal(store.join(&none, "", loose, Via::CtlSpawn)),
            Err("another requester's request".to_owned()),
            "CONTROL: {loose}"
        );
        assert!(
            store
                .tab(AT + 3, &id, loose, LEGACY_ASKER, false, None)
                .is_err(),
            "CONTROL: {loose} is no tab"
        );
    }
    // The next process reads all of it back.
    let mut fresh = Store::open(dir.path()).unwrap();
    assert_eq!(fresh.skipped(), 0);
    assert_eq!(
        fresh.owner(TAB_A),
        Some((id.clone(), String::new(), LEGACY_ASKER.to_owned()))
    );
    let text = String::from_utf8(journal(dir.path())).unwrap();
    assert!(
        text.contains(&format!("op=tab req={id} tab={TAB_A} by={LEGACY_ASKER}")),
        "{text}"
    );
}

/// A legacy tab's footprint is its request's: the folder it claims names it, a folder born under
/// it names it, and a command a person ran in a legacy tab of their own counts as a touch.
/// NEGATIVE CONTROL: a loose `s-` word names no tab in a claim and is no touch.
#[cfg(unix)]
#[test]
fn a_legacy_tab_claims_and_touches_as_any_tab_does() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let (fleet, _) = earlier_journal(dir.path());
    let mut store = Store::open(dir.path()).unwrap();
    let from_tab = |tab, path| ClaimAsk {
        tab: Some(tab),
        ..named(None, path, "", LOCAL)
    };
    assert_eq!(
        store
            .claim(AT + 10, from_tab(LEGACY_TAB, "/Users//a/x"))
            .unwrap(),
        Claimed::New(fleet.clone())
    );
    store
        .born(
            AT + 11,
            &[Born {
                tab: Some(LEGACY_TAB.to_owned()),
                ..born(&fleet, "/Users//a/y", Some(true))
            }],
        )
        .unwrap();
    let touch = |path: &str, tab: &str| Touch {
        id: fleet.clone(),
        path: path.to_owned(),
        tab: tab.to_owned(),
        blk: 1,
        why: TouchWhy::PersonTab,
    };
    // NEGATIVE CONTROL: a folder born under a loose word is claimed, and names no tab.
    store
        .born(
            AT + 11,
            &[Born {
                tab: Some("s-1".to_owned()),
                ..born(&fleet, "/Users//a/z", Some(true))
            }],
        )
        .unwrap();
    assert_eq!(
        store
            .touched(AT + 12, &[touch("/Users//a/x", "s-1")])
            .unwrap(),
        0,
        "CONTROL: a loose word is no tab"
    );
    assert_eq!(
        store
            .touched(AT + 12, &[touch("/Users//a/x", LEGACY_ASKER)])
            .unwrap(),
        1
    );
    let fresh = Store::open(dir.path()).unwrap();
    assert_eq!(fresh.skipped(), 0);
    let request = fresh.get(&fleet).unwrap();
    assert_eq!(
        request.claim("/Users//a/x").unwrap().claim.tab.as_deref(),
        Some(LEGACY_TAB)
    );
    assert_eq!(
        request.claim("/Users//a/y").unwrap().claim.tab.as_deref(),
        Some(LEGACY_TAB)
    );
    assert_eq!(
        request.claim("/Users//a/z").unwrap().claim.tab,
        None,
        "CONTROL: a loose word names no tab"
    );
    assert!(request.touched.contains("/Users//a/x"));
}
