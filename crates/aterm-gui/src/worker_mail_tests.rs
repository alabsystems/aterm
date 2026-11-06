// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tests for a worker's mail to its manager ([`super`]).

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use aterm_session::SessionId;
use aterm_types::needs::AttentionKind;

use super::*;
use crate::control::pct_encode;
use crate::fabric::fleet_mail::PeerMail;
use crate::fabric::{FabricFact, FabricSession, SessionFabric};
use crate::harness_host::{NEEDS_ATTENTION_OWNER, store_needs};
use crate::session_store::{SessionHandle, new_store, test_handle};
use crate::task_record::{TaskMark, mark_session_task_within};

const NEED_A: &str = "approve the 0.48.0 release notes";
const NEED_B: &str = "the release credential to sign 0.48.0";
const ENTRY_A: &str = "claude needs you: approve the 0.48.0 release notes \u{2014} carrying on";
const ENTRY_AB: &str =
    "claude needs you: approve the 0.48.0 release notes (+1 more) \u{2014} carrying on";

/// A manager on another Mac, and that Mac.
const BOSS_THERE: &str = "s-0123456789abcdef0123";
const NODE: &str = "n-cccccccccccccccc";

/// Marks `ctx`'s task `state`, started by `from` (`None`: as it was, else by ctl), its result at
/// `result` where it has one.
fn mark_with(
    ctx: &crate::SessionCtx,
    local: u64,
    state: TaskState,
    from: Option<&str>,
    result: Option<&str>,
) {
    let mut mark = TaskMark::new(state);
    mark.from = from.map(str::to_owned);
    mark.result_path = result.map(PathBuf::from);
    mark_session_task_within(ctx, local, &mark, Duration::ZERO).expect("unfenced");
}

/// Marks `ctx`'s task `state`, started by `from`, its result at `/scratch/result.md` once it is
/// no longer working.
fn mark_as(ctx: &crate::SessionCtx, local: u64, state: TaskState, from: Option<&str>) {
    let result = (state != TaskState::Working).then_some("/scratch/result.md");
    mark_with(ctx, local, state, from, result);
}

fn mark(h: &SessionHandle, state: TaskState, from: Option<&str>) {
    mark_as(&h.ctx, h.local_id, state, from);
}

/// Stands `needs` on `ctx`'s session beside the `owner=needs` entry `entry`, as its loop tells
/// them.
fn tell(store: &Store, ctx: &crate::SessionCtx, needs: &[&str], entry: &str) {
    stand(ctx, Some(entry));
    let needs: Vec<String> = needs.iter().map(|need| (*need).to_owned()).collect();
    store_needs(store, ctx.self_id.as_str(), &needs, entry);
}

/// Stands the `owner=needs` entry `entry` on `ctx`'s session (`None`: takes it down), with no
/// needs told beside it.
fn stand(ctx: &crate::SessionCtx, entry: Option<&str>) {
    ctx.meta
        .lock()
        .unwrap()
        .set_attention_kind(
            NEEDS_ATTENTION_OWNER,
            entry.map(str::to_owned),
            entry.map(|_| AttentionKind::Ask),
        )
        .expect("a slot");
}

fn registered(store: &Store, local: u64) -> SessionHandle {
    let h = test_handle(local);
    store.write().unwrap().register(h.clone());
    h
}

/// Each mail's head, in order.
fn subjects(owed: &Owed) -> Vec<&str> {
    owed.mails
        .iter()
        .map(|mail| mail.subject.as_str())
        .collect()
}

/// What `ctx` queued on this Mac's bus: each post's address, kind, key and text.
fn bus(ctx: &crate::SessionCtx) -> Vec<(String, String, Option<String>, String)> {
    ctx.fabric.with_inbox_for_test(|inbox| {
        inbox
            .posts
            .iter()
            .map(|p| (p.to.clone(), p.kind.clone(), p.key.clone(), p.body.clone()))
            .collect()
    })
}

/// The record in Messages of `mail` from worker `sid` that reached `manager` for `why`.
fn unsent_record(
    sid: &str,
    manager: &str,
    mail: &Mail,
    why: &Undelivered,
) -> aterm_messages::Message {
    crate::message_reporters::worker_mail_unsent_record(
        sid,
        manager,
        &mail.subject,
        &why.to_string(),
        &mail.key,
    )
}

/// The worker's session with a fleet drive of its own to one other Mac's mailbox, which keeps
/// each message's key as that mailbox does: the same key with the same bytes is the same message
/// (`dup=1`), with other bytes a refusal (`crate::fabric::fleet_mail::post`).
struct Drives<'a> {
    ctx: &'a crate::SessionCtx,
    /// What the Mac answers every drive with, in place of its mailbox.
    answer: Option<&'a str>,
    drives: Mutex<Vec<(String, String)>>,
    mailbox: Mutex<BTreeMap<String, (u64, String)>>,
}

impl<'a> Drives<'a> {
    fn mailbox(ctx: &'a crate::SessionCtx) -> Self {
        Self {
            ctx,
            answer: None,
            drives: Mutex::new(Vec::new()),
            mailbox: Mutex::new(BTreeMap::new()),
        }
    }

    fn answering(ctx: &'a crate::SessionCtx, answer: &'a str) -> Self {
        Self {
            answer: Some(answer),
            ..Self::mailbox(ctx)
        }
    }
}

impl FabricSession for Drives<'_> {
    fn self_id(&self) -> &SessionId {
        &self.ctx.self_id
    }
    fn fabric(&self) -> &SessionFabric {
        &self.ctx.fabric
    }
    fn record_fact(&self, kind: &'static str, fact: FabricFact<'_>) -> bool {
        self.ctx.record_fact(kind, fact)
    }
    fn fabric_changed(&self) {
        self.ctx.fabric_changed();
    }
    fn holder_token(&self) -> Option<String> {
        None
    }
    fn peer_mail(&self, _node: &str) -> PeerMail {
        PeerMail::Fleet
    }
    fn fleet_drive(&self, node: &str, request: &str) -> Result<String, String> {
        self.drives
            .lock()
            .unwrap()
            .push((node.to_owned(), request.to_owned()));
        if let Some(answer) = self.answer {
            return Ok(answer.to_owned());
        }
        let (head, text) = request.split_once(" -- ").expect("a post's text");
        let key = head
            .split(' ')
            .find_map(|word| word.strip_prefix("key="))
            .expect("a post's key")
            .to_owned();
        let mut mailbox = self.mailbox.lock().unwrap();
        let next = u64::try_from(mailbox.len()).unwrap() + 1;
        Ok(match mailbox.get(&key) {
            Some((id, held)) if held == text => format!("OK f{id} dup=1"),
            Some(_) => "ERR that key already names another message".to_owned(),
            None => {
                mailbox.insert(key, (next, text.to_owned()));
                format!("OK f{next}")
            }
        })
    }
}

#[test]
fn a_local_manager_gets_one_mail_per_need_and_one_for_the_conclusion() {
    let _lane = crate::message_inbox::own_lane_for_test();
    crate::fabric::with_link_reset(|| {
        crate::fabric::note_bridge_supervised();
        let store = new_store();
        let (worker, manager) = (registered(&store, 1), registered(&store, 2));
        let (sid, boss) = (worker.ctx.self_id.as_str(), manager.ctx.self_id.as_str());
        mark(&worker, TaskState::Working, Some(boss));
        assert!(
            owed(&worker.ctx, &store).is_none(),
            "a working task with no need owes nothing"
        );

        tell(&store, &worker.ctx, &[NEED_A], ENTRY_A);
        let first = owed(&worker.ctx, &store).expect("the need is owed");
        assert_eq!(first.manager, Manager::Here(boss.to_owned()));
        assert_eq!(subjects(&first), [format!("NEEDS YOU: {NEED_A}")]);
        assert!(
            owed(&worker.ctx, &store).is_none(),
            "a need a send has in hand is owed nobody else"
        );
        assert_eq!(first.send(), [Ok("OK 1".to_owned())]);

        // The same need told again is owed nobody.
        tell(&store, &worker.ctx, &[NEED_A], ENTRY_A);
        assert!(owed(&worker.ctx, &store).is_none(), "a repeated need");

        // A second need beside the first is owed alone.
        tell(&store, &worker.ctx, &[NEED_A, NEED_B], ENTRY_AB);
        let second = owed(&worker.ctx, &store).expect("the new need is owed");
        assert_eq!(subjects(&second), [format!("NEEDS YOU: {NEED_B}")]);
        assert_eq!(second.send(), [Ok("OK 2".to_owned())]);

        mark(&worker, TaskState::Done, None);
        let end = owed(&worker.ctx, &store).expect("the conclusion is owed");
        assert_eq!(subjects(&end), ["finished"]);
        assert_eq!(end.send(), [Ok("OK 3".to_owned())]);
        assert!(
            owed(&worker.ctx, &store).is_none(),
            "a conclusion is told once"
        );

        let posts = bus(&worker.ctx);
        assert_eq!(posts.len(), 3, "{posts:?}");
        assert!(
            posts
                .iter()
                .all(|(to, kind, ..)| *to == pct_encode(&format!("@{boss}")) && kind == "report"),
            "each mail is a report to the manager: {posts:?}"
        );
        let keys: Vec<&str> = posts.iter().filter_map(|p| p.2.as_deref()).collect();
        assert_eq!(
            keys,
            [&first.mails[0].key, &second.mails[0].key, &end.mails[0].key]
        );
        assert!(
            keys[..2]
                .iter()
                .all(|key| key.starts_with(&format!("{sid}.need.")))
                && keys[2].starts_with(&format!("{sid}.end."))
                && keys[0] != keys[1],
            "{keys:?}"
        );
        assert!(
            posts[0].3.starts_with(&format!(
                "NEEDS YOU: {NEED_A} | sid={sid} state=working conclusion=- why=- result=-"
            )),
            "{}",
            posts[0].3
        );
        assert!(
            posts[2].3.starts_with(&format!(
                "finished | sid={sid} state=done conclusion=finished why=- result={}",
                pct_encode("/scratch/result.md")
            )),
            "{}",
            posts[2].3
        );
        assert!(
            crate::message_inbox::take_queued().is_empty(),
            "a mail the bus took is no record"
        );
    });
}

#[test]
fn a_worker_with_no_manager_mails_nobody() {
    let _lane = crate::message_inbox::own_lane_for_test();
    crate::fabric::with_link_reset(|| {
        crate::fabric::note_bridge_supervised();
        let store = new_store();
        let (alone, managed, manager) = (
            registered(&store, 1),
            registered(&store, 2),
            registered(&store, 3),
        );
        let boss = manager.ctx.self_id.as_str();
        mark(&alone, TaskState::Working, None);
        mark(&managed, TaskState::Working, Some(boss));
        for h in [&alone, &managed] {
            tell(&store, &h.ctx, &[NEED_A], ENTRY_A);
        }
        assert!(owed(&alone.ctx, &store).is_none(), "a ctl task's need");
        // Control: the same need of a worker with a manager is owed.
        assert!(owed(&managed.ctx, &store).is_some());

        mark(&alone, TaskState::Failed, None);
        assert!(
            owed(&alone.ctx, &store).is_none(),
            "a ctl task's conclusion"
        );
        assert!(
            owed_at_close(&alone.ctx, &store).is_none(),
            "a ctl task's close"
        );
        assert!(bus(&alone.ctx).is_empty());
        assert!(crate::message_inbox::take_queued().is_empty());
    });
}

#[test]
fn a_manager_nothing_reaches_is_a_record_in_messages_and_its_mail_is_not_sent_again() {
    let _lane = crate::message_inbox::own_lane_for_test();
    crate::fabric::with_link_reset(|| {
        let store = new_store();
        let (worker, manager) = (registered(&store, 1), registered(&store, 2));
        let (sid, boss) = (worker.ctx.self_id.as_str(), manager.ctx.self_id.as_str());
        mark(&worker, TaskState::Working, Some(boss));
        tell(&store, &worker.ctx, &[NEED_A], ENTRY_A);
        let unsent = owed(&worker.ctx, &store).expect("the need is owed");
        let sent = unsent.send();
        let no_bridge = Undelivered::Unsent(Unsent::NoBridge(crate::fabric::fabric_state().into()));
        assert_eq!(
            sent,
            [Err(no_bridge.clone())],
            "no bridge runs, so the bus carries nothing"
        );
        assert!(bus(&worker.ctx).is_empty(), "nothing is left queued");
        let records = crate::message_inbox::take_queued();
        assert_eq!(
            records
                .iter()
                .map(|queued| &queued.message)
                .collect::<Vec<_>>(),
            [&unsent_record(sid, boss, &unsent.mails[0], &no_bridge)],
            "one record of the one mail"
        );
        assert!(owed(&worker.ctx, &store).is_none(), "never sent again");

        // A manager that is no session here, on no Mac a spawn named, is reached by nothing.
        let orphan = registered(&store, 4);
        mark(&orphan, TaskState::Working, Some(BOSS_THERE));
        tell(&store, &orphan.ctx, &[NEED_A], ENTRY_A);
        let nowhere = owed(&orphan.ctx, &store).expect("the need is owed");
        assert_eq!(nowhere.manager, Manager::Unknown(BOSS_THERE.to_owned()));
        let drives = Drives::answering(&orphan.ctx, "OK f1");
        assert_eq!(
            nowhere.send_as(&drives),
            [Err(Undelivered::NoRoute(BOSS_THERE.to_owned()))]
        );
        assert!(
            drives.drives.lock().unwrap().is_empty(),
            "no Mac was driven"
        );
        assert_eq!(crate::message_inbox::take_queued().len(), 1);
        assert!(owed(&orphan.ctx, &store).is_none(), "never sent again");
    });
}

/// A worker managed from another Mac: its record names the manager and that Mac, as a fleet spawn
/// left it.
fn managed_from_there(h: &SessionHandle) {
    mark(h, TaskState::Working, Some(BOSS_THERE));
    h.ctx
        .timeline
        .lock()
        .unwrap()
        .set_task_manager_node(Some(NODE.to_owned()));
}

#[test]
fn a_remote_managers_mail_is_routed_to_its_mac_by_direct_fleet_mail() {
    let _lane = crate::message_inbox::own_lane_for_test();
    crate::fabric::with_link_reset(|| {
        crate::fabric::note_bridge_supervised();
        let store = new_store();
        let worker = registered(&store, 1);
        let sid = worker.ctx.self_id.as_str();
        managed_from_there(&worker);
        tell(&store, &worker.ctx, &[NEED_A], ENTRY_A);
        let owed = owed(&worker.ctx, &store).expect("the need is owed");
        assert_eq!(
            owed.manager,
            Manager::There {
                sid: BOSS_THERE.to_owned(),
                node: NODE.to_owned()
            }
        );
        let mail = &owed.mails[0];
        let drives = Drives::mailbox(&worker.ctx);
        assert_eq!(
            owed.send_as(&drives),
            [Ok(format!(
                "OK fleet=f1 to=@{BOSS_THERE}@{NODE} key={}",
                mail.key
            ))]
        );
        assert_eq!(
            *drives.drives.lock().unwrap(),
            [(
                NODE.to_owned(),
                format!(
                    "@{BOSS_THERE} post key={} kind=report -- {}",
                    mail.key, mail.text
                )
            )],
            "one drive of the manager's Mac, posting the mail into its mailbox there"
        );
        assert!(
            mail.text
                .starts_with(&format!("NEEDS YOU: {NEED_A} | sid={sid} "))
        );
        assert!(
            bus(&worker.ctx).is_empty(),
            "this Mac's bus carries none of it"
        );

        // Negative control: the Mac refuses it, which is a record and no second drive.
        let refusing = Drives::answering(&worker.ctx, "ERR denied");
        assert_eq!(
            owed.send_as(&refusing),
            [Err(Undelivered::Unsent(Unsent::Answered {
                node: NODE.to_owned(),
                answer: "ERR denied".to_owned()
            }))]
        );
        assert_eq!(refusing.drives.lock().unwrap().len(), 1);
        assert_eq!(crate::message_inbox::take_queued().len(), 1);
    });
}

/// `h`'s task record as a seamless update carries it, and as its successor seeds it: the same
/// session, its record read back from the row.
fn carry(h: &SessionHandle) -> String {
    h.ctx
        .timeline
        .lock()
        .unwrap()
        .task_carry_row()
        .expect("a task to carry")
}

fn seed(h: &SessionHandle, row: &str) {
    crate::task_record::seed_carried(&h.ctx, Some(row));
}

/// A SEAMLESS UPDATE CARRIES WHAT WAS TOLD, AND ONLY THAT: a need and a conclusion whose sends
/// finished are owed nobody after it, while one a send still had in hand when the record was
/// carried (an update's Commit, a quit) is owed again by the successor.
#[test]
fn what_was_told_crosses_a_carry_and_what_a_send_had_in_hand_is_owed_again() {
    let _lane = crate::message_inbox::own_lane_for_test();
    crate::fabric::with_link_reset(|| {
        crate::fabric::note_bridge_supervised();
        let store = new_store();
        let (worker, manager) = (registered(&store, 1), registered(&store, 2));
        let boss = manager.ctx.self_id.as_str();
        mark_with(&worker.ctx, 1, TaskState::Working, Some(boss), None);
        tell(&store, &worker.ctx, &[NEED_A], ENTRY_A);
        assert!(owed(&worker.ctx, &store).expect("A is owed").send()[0].is_ok());
        tell(&store, &worker.ctx, &[NEED_A, NEED_B], ENTRY_AB);
        let in_hand = owed(&worker.ctx, &store).expect("B is owed");
        assert_eq!(subjects(&in_hand), [format!("NEEDS YOU: {NEED_B}")]);
        let row = carry(&worker);
        drop(in_hand);

        seed(&worker, &row);
        let again = owed(&worker.ctx, &store).expect("B is owed again");
        assert_eq!(
            subjects(&again),
            [format!("NEEDS YOU: {NEED_B}")],
            "A was told; B's send was cut short"
        );
        assert!(again.send()[0].is_ok());

        mark_with(&worker.ctx, 1, TaskState::Done, None, None);
        let end = owed(&worker.ctx, &store).expect("the conclusion is owed");
        let row = carry(&worker);
        drop(end);
        seed(&worker, &row);
        let end_again = owed(&worker.ctx, &store).expect("the conclusion is owed again");
        assert_eq!(subjects(&end_again), ["finished"]);

        assert!(end_again.send()[0].is_ok());
        let row = carry(&worker);
        assert!(
            row.contains(" told=") && row.contains(" told_end=1"),
            "{row}"
        );
        seed(&worker, &row);
        assert!(
            owed(&worker.ctx, &store).is_none(),
            "every need and the conclusion were told"
        );
        assert!(owed_at_close(&worker.ctx, &store).is_none());
    });
}

/// A MAIL SENT AGAIN IS ONE MESSAGE WHERE IT LANDS, OR A NEW ONE, NEVER A KEY'S CONFLICT: a
/// successor resends what its predecessor's send had in hand when the record was carried, and the
/// manager's mailbox answers the same line under its key as the same message and a line whose step
/// moved as another, rather than refusing the key. CONTROL: the mailbox does refuse a key that
/// names other bytes.
#[test]
fn a_resent_mail_is_one_message_where_it_lands_and_never_a_keys_conflict() {
    let _lane = crate::message_inbox::own_lane_for_test();
    crate::fabric::with_link_reset(|| {
        let store = new_store();
        let worker = registered(&store, 1);
        managed_from_there(&worker);
        tell(&store, &worker.ctx, &[NEED_A], ENTRY_A);
        let first = owed(&worker.ctx, &store).expect("A is owed");
        // The update carried the record while the send was in hand, and the send then landed.
        let row = carry(&worker);
        let drives = Drives::mailbox(&worker.ctx);
        assert!(first.send_as(&drives)[0].is_ok());

        seed(&worker, &row);
        let same = owed(&worker.ctx, &store).expect("A is owed again");
        assert_eq!(same.mails, first.mails, "the same line under the same key");
        let sent = same.send_as(&drives);
        assert!(
            matches!(sent.as_slice(), [Ok(answer)] if answer.ends_with(" dup=1")),
            "the same message: {sent:?}"
        );

        // Its step moves before the successor sends it again.
        seed(&worker, &row);
        let post =
            crate::fabric::cmd_post(&*worker.ctx, "to=say:build kind=note -- step two", None);
        assert!(post.starts_with("OK "), "{post}");
        let moved = owed(&worker.ctx, &store).expect("A is owed again");
        assert_ne!(moved.mails[0].text, first.mails[0].text, "PRECONDITION");
        assert_ne!(moved.mails[0].key, first.mails[0].key);
        let sent = moved.send_as(&drives);
        assert!(
            matches!(sent.as_slice(), [Ok(answer)] if !answer.ends_with(" dup=1")),
            "another message: {sent:?}"
        );
        assert!(crate::message_inbox::take_queued().is_empty(), "no record");

        // Control: the mailbox refuses a key it holds under other bytes.
        let clash = Owed {
            mails: vec![Mail {
                text: moved.mails[0].text.clone(),
                ..first.mails[0].clone()
            }],
            ..moved
        };
        assert!(
            matches!(
                clash.send_as(&drives).as_slice(),
                [Err(Undelivered::Unsent(Unsent::Answered { .. }))]
            ),
            "the mailbox keeps its keys"
        );
    });
}

/// A NEED THAT WAS RESOLVED AND IS RAISED AGAIN IS NEWS AGAIN, even in the same record after a
/// turn back into its concluded task. CONTROL: an entry no told set describes yet forgets nothing.
#[test]
fn a_need_raised_again_after_it_was_resolved_is_mailed_again() {
    let _lane = crate::message_inbox::own_lane_for_test();
    crate::fabric::with_link_reset(|| {
        crate::fabric::note_bridge_supervised();
        let store = new_store();
        let (worker, manager) = (registered(&store, 1), registered(&store, 2));
        let boss = manager.ctx.self_id.as_str();
        mark(&worker, TaskState::Working, Some(boss));
        tell(&store, &worker.ctx, &[NEED_A], ENTRY_A);
        assert!(owed(&worker.ctx, &store).expect("A").send()[0].is_ok());

        // Control: the entry moves before its set does; A stays told.
        stand(
            &worker.ctx,
            Some("claude needs you: something new \u{2014} carrying on"),
        );
        assert!(owed(&worker.ctx, &store).is_none());
        tell(&store, &worker.ctx, &[NEED_A], ENTRY_A);
        assert!(owed(&worker.ctx, &store).is_none(), "A still stood");

        // A is resolved, the task finishes, and a turn reopens it.
        stand(&worker.ctx, None);
        assert!(owed(&worker.ctx, &store).is_none());
        mark(&worker, TaskState::Done, None);
        assert!(owed(&worker.ctx, &store).expect("the end").send()[0].is_ok());
        mark(&worker, TaskState::Working, None);
        assert!(owed(&worker.ctx, &store).is_none());
        tell(&store, &worker.ctx, &[NEED_A], ENTRY_A);
        let again = owed(&worker.ctx, &store).expect("A is raised again");
        assert_eq!(subjects(&again), [format!("NEEDS YOU: {NEED_A}")]);
        assert!(again.send()[0].is_ok());
        let keys: Vec<Option<String>> = bus(&worker.ctx).into_iter().map(|p| p.2).collect();
        assert_eq!(keys.len(), 3);
        assert_ne!(keys[0], keys[2], "a new message, not the first's resend");
    });
}

/// A WORKER WHOSE TAB CLOSES BEFORE ITS CONCLUSION WAS TOLD TELLS ITS MANAGER ON ANOTHER MAC, by
/// direct fleet mail, as `await task` answers it: `state=closed`, `conclusion=failed`. A manager
/// here, whom only the bus reaches, is a record: the bus carries no word of a session that left.
/// CONTROL: a worker whose conclusion was told owes nothing at its close.
#[test]
fn a_worker_whose_tab_closes_before_its_end_was_told_mails_its_manager_there() {
    let _lane = crate::message_inbox::own_lane_for_test();
    crate::fabric::with_link_reset(|| {
        crate::fabric::note_bridge_supervised();
        let store = new_store();
        let worker = registered(&store, 1);
        let sid = worker.ctx.self_id.as_str();
        managed_from_there(&worker);
        let closed = owed_at_close(&worker.ctx, &store).expect("the close is owed");
        let drives = Drives::mailbox(&worker.ctx);
        assert!(closed.send_as(&drives)[0].is_ok());
        let mail = &closed.mails[0];
        assert!(mail.key.starts_with(&format!("{sid}.end.")), "{}", mail.key);
        assert!(
            mail.text.contains(&format!(
                " | sid={sid} state=closed conclusion=failed why=- result=-"
            )),
            "{}",
            mail.text
        );
        assert_eq!(drives.drives.lock().unwrap().len(), 1);
        assert!(bus(&worker.ctx).is_empty());

        // A manager here.
        let (local, manager) = (registered(&store, 2), registered(&store, 3));
        let boss = manager.ctx.self_id.as_str();
        mark(&local, TaskState::Working, Some(boss));
        let closed = owed_at_close(&local.ctx, &store).expect("the close is owed");
        let leaving = Undelivered::Unsent(Unsent::Leaving);
        assert_eq!(closed.send(), [Err(leaving.clone())]);
        assert!(bus(&local.ctx).is_empty(), "nothing queued on the bus");
        assert_eq!(
            crate::message_inbox::take_queued()
                .iter()
                .map(|queued| &queued.message)
                .collect::<Vec<_>>(),
            [&unsent_record(
                local.ctx.self_id.as_str(),
                boss,
                &closed.mails[0],
                &leaving
            )]
        );

        // Control: a conclusion told, or in a send's hand, is no news at the close.
        mark(&local, TaskState::Done, None);
        let end = owed(&local.ctx, &store).expect("the conclusion");
        assert!(owed_at_close(&local.ctx, &store).is_none(), "in hand");
        assert!(end.send()[0].is_ok());
        assert!(owed_at_close(&local.ctx, &store).is_none(), "told");
    });
}

#[test]
fn a_fleet_spawns_manager_mac_survives_a_carry_and_a_malformed_one_is_dropped() {
    let worker = test_handle(1);
    mark(&worker, TaskState::Working, Some(BOSS_THERE));
    let record = {
        let mut timeline = worker.ctx.timeline.lock().unwrap();
        timeline.set_task_manager_node(Some(NODE.to_owned()));
        timeline.task().cloned().unwrap()
    };
    let now = std::time::Instant::now();
    let row = record.carry_row(now);
    assert!(row.contains(&format!(" manager_node={NODE}")), "{row}");
    let back = |row: &str| {
        crate::task_record::TaskRecord::from_carry_row(row, None, now, record.started_unix_ms)
            .expect("a carried record")
            .manager_node
    };
    assert_eq!(back(&row), Some(NODE.to_owned()));
    let forged = row.replace(&format!("manager_node={NODE}"), "manager_node=beta");
    assert_eq!(back(&forged), None, "no proved drive's id");
}

/// A MANAGER NAMED ANEW BY A MARK RUNS HERE until a spawn says otherwise: the Mac a fleet spawn
/// named for the old one goes with it. CONTROL: a mark that names no other manager keeps it.
#[test]
fn a_mark_that_names_another_manager_forgets_the_old_managers_mac() {
    let store = new_store();
    let (worker, manager) = (registered(&store, 1), registered(&store, 2));
    let boss = manager.ctx.self_id.as_str();
    managed_from_there(&worker);
    let node = || {
        worker
            .ctx
            .timeline
            .lock()
            .unwrap()
            .task()
            .and_then(|task| task.manager_node.clone())
    };
    mark(&worker, TaskState::Stalled, None);
    mark(&worker, TaskState::Working, Some(BOSS_THERE));
    assert_eq!(node(), Some(NODE.to_owned()), "the same manager");
    mark(&worker, TaskState::Working, Some(boss));
    assert_eq!(node(), None);
    let record = worker.ctx.timeline.lock().unwrap().task().cloned().unwrap();
    assert_eq!(Manager::of(&record, &store), Manager::Here(boss.to_owned()));
}

#[test]
fn the_window_mails_a_workers_need_and_its_conclusion_as_they_come() {
    let _lane = crate::message_inbox::own_lane_for_test();
    crate::fabric::with_link_reset(|| {
        crate::fabric::note_bridge_supervised();
        let mut app = crate::App::headless_for_test();
        let ctx = Arc::clone(&app.pool.get(0).expect("session 0").ctx);
        {
            let mut timeline = ctx.timeline.lock().unwrap();
            timeline.note_foreground_group(7);
            timeline.set_program(7, Some("claude".into()));
        }
        let manager = registered(&app.store, 9);
        let (sid, boss) = (
            ctx.self_id.as_str().to_owned(),
            manager.ctx.self_id.as_str().to_owned(),
        );
        let posted = |n: usize| {
            aterm_spec::wait::until_within(aterm_spec::wait::MUST_HAPPEN, "the mail", || {
                let posts = bus(&ctx);
                (posts.len() >= n).then_some(posts)
            })
        };

        mark_as(&ctx, 0, TaskState::Working, Some(&boss));
        app.on_task_changed(0);
        tell(&app.store, &ctx, &[NEED_A], ENTRY_A);
        app.on_meta_changed(0);
        app.apply_harness_fact(&sid, crate::HarnessFact::Needs);
        let posts = posted(1);
        assert!(
            posts[0]
                .3
                .starts_with(&format!("NEEDS YOU: {NEED_A} | sid={sid}"))
        );

        // Every trigger again, with nothing new: the window took the need in hand.
        tell(&app.store, &ctx, &[NEED_A], ENTRY_A);
        app.apply_harness_fact(&sid, crate::HarnessFact::Needs);
        app.on_meta_changed(0);
        app.on_task_changed(0);
        assert!(owed(&ctx, &app.store).is_none(), "nothing more is owed");

        mark_as(&ctx, 0, TaskState::Done, None);
        app.on_task_changed(0);
        let posts = posted(2);
        assert_eq!(posts.len(), 2, "{posts:?}");
        assert!(posts[1].3.starts_with("finished | "), "{}", posts[1].3);
        assert!(
            posts
                .iter()
                .all(|(to, kind, ..)| *to == pct_encode(&format!("@{boss}")) && kind == "report")
        );
    });
}

/// THE WINDOW TELLS A WORKER'S CLOSE AS ITS TAB CLOSES: a manager here, whom the bus alone
/// reaches, is a record of the close. CONTROL: a quit that took the session into its restore
/// manifest sends nothing, since the worker comes back.
#[test]
fn the_window_tells_a_workers_close_unless_a_quit_keeps_it_for_the_next_launch() {
    let _lane = crate::message_inbox::own_lane_for_test();
    crate::fabric::with_link_reset(|| {
        let mut app = crate::App::headless_for_test();
        let wid = crate::WindowId(0);
        app.push_stub_tab(wid, crate::stub_session(1));
        app.push_stub_tab(wid, crate::stub_session(2));
        let ctx = |app: &crate::App, local: u64| Arc::clone(&app.pool.get(local).unwrap().ctx);
        let boss = ctx(&app, 0).self_id.as_str().to_owned();
        let (kept, closing) = (ctx(&app, 1), ctx(&app, 2));
        mark_as(&kept, 1, TaskState::Working, Some(&boss));
        mark_as(&closing, 2, TaskState::Working, Some(&boss));

        app.quit_capture = Some(crate::restore::RestoreManifest::new(Vec::new()));
        assert!(!app.mail_worker_close(&kept), "the quit keeps it");
        app.quit_capture = None;

        app.close_confirm = crate::app_window::CloseConfirm::Programmatic;
        assert!(app.close_session_by_id(2).is_ok());
        let ours =
            crate::message_reporters::host_record_key("worker-mail", closing.self_id.as_str());
        let mut seen = Vec::new();
        let record = aterm_spec::wait::until_within(
            aterm_spec::wait::MUST_HAPPEN,
            "the close's record",
            || {
                seen.extend(crate::message_inbox::take_queued());
                seen.iter()
                    .find(|queued| {
                        queued
                            .message
                            .key
                            .as_deref()
                            .is_some_and(|key| key.starts_with(&ours))
                    })
                    .map(|queued| queued.message.clone())
            },
        );
        assert!(
            record.detail.iter().any(|line| line.contains(&boss)),
            "the close, to its manager: {:?}",
            record.detail
        );
    });
}
