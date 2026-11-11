// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::{Driver, DrivingAs, FabricFact, SessionFabric};
use aterm_session::SessionId;

#[test]
fn mesh_mail_rewrite_retains_unknown_nodes_and_is_idempotent() {
    let old = "a".repeat(32);
    let unknown = "b".repeat(32);
    let mut boxed = Mailbox {
        next: 2,
        rows: [old.clone(), unknown.clone()]
            .into_iter()
            .enumerate()
            .map(|(i, node)| Row {
                id: i as u64 + 1,
                node,
                key: "unique".into(),
                kind: "ask".into(),
                text: "retained".into(),
                listed: false,
                reply: None,
                reply_seen: false,
            })
            .collect(),
    };
    let aliases = vec![(old, "n-0123456789abcdef".into())];
    assert_eq!(rewrite_nodes(&mut boxed, &aliases), 1);
    assert_eq!(boxed.rows[0].node, aliases[0].1);
    assert_eq!(boxed.rows[1].node, unknown);
    assert_eq!(boxed.rows[0].text, "retained");
    assert!(validated(&boxed));
    assert_eq!(rewrite_nodes(&mut boxed, &aliases), 0);
    assert!(!valid_node("0123456789abcdef"));
}

/// How a test serves a fleet drive: the Mac and the request line in, the answer out.
pub(super) type Drive = Box<dyn Fn(&str, &str) -> Result<String, String> + Send + Sync>;

pub(super) struct Session {
    pub(super) sid: SessionId,
    pub(super) fabric: SessionFabric,
    pub(super) path: std::path::PathBuf,
    pub(super) facts: std::sync::Mutex<Vec<String>>,
    pub(super) wakes: std::sync::atomic::AtomicU64,
    pub(super) ready: std::sync::atomic::AtomicBool,
    /// The Macs whose presence row this session's Mac heard (§11 C6).
    pub(super) heard: Vec<String>,
    pub(super) drive: Option<Drive>,
    /// Every fleet drive asked for: the Mac and the request line.
    pub(super) drives: std::sync::Mutex<Vec<(String, String)>>,
}

impl FabricSession for Session {
    fn self_id(&self) -> &SessionId {
        &self.sid
    }
    fn fabric(&self) -> &SessionFabric {
        &self.fabric
    }
    fn record_fact(&self, _: &'static str, fact: FabricFact<'_>) -> bool {
        self.facts
            .lock()
            .unwrap()
            .push(crate::test_support::said(fact));
        false
    }
    fn fabric_changed(&self) {
        self.wakes
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    }
    fn holder_token(&self) -> Option<String> {
        None
    }
    fn fleet_mail_path(&self) -> Option<std::path::PathBuf> {
        Some(self.path.clone())
    }
    fn fleet_mail_ready(&self) -> bool {
        self.ready.load(std::sync::atomic::Ordering::SeqCst)
    }
    fn peer_mail(&self, node: &str) -> PeerMail {
        if self.heard.iter().any(|heard| heard == node) {
            PeerMail::Mesh
        } else {
            PeerMail::Fleet
        }
    }
    fn fleet_drive(&self, node: &str, request: &str) -> Result<String, String> {
        self.drives
            .lock()
            .unwrap()
            .push((node.to_owned(), request.to_owned()));
        self.drive
            .as_ref()
            .map_or_else(|| Err("no drive".to_owned()), |drive| drive(node, request))
    }
}

pub(super) fn session(path: &std::path::Path) -> Session {
    Session {
        sid: SessionId::new("s-12345678"),
        fabric: SessionFabric::default(),
        path: path.to_owned(),
        facts: Default::default(),
        wakes: Default::default(),
        ready: std::sync::atomic::AtomicBool::new(true),
        heard: Vec::new(),
        drive: None,
        drives: Default::default(),
    }
}

/// A session of a process whose mesh switch is on: an R3 aterm.
fn meshed(path: &std::path::Path) -> Session {
    Session {
        fabric: SessionFabric {
            mesh: MeshPolicy::on(),
            ..SessionFabric::default()
        },
        ..session(path)
    }
}

pub(super) fn as_node<T>(byte: &str, f: impl FnOnce() -> T) -> T {
    let _driver = DrivingAs::enter(Driver {
        node: byte.repeat(32),
        host: "peer.local".to_owned(),
    });
    f()
}

/// A message a pre-R3 Mac left unread in `path`'s mailbox, taken by an earlier build (the
/// mesh off); then R3's first start on it, whose inbox lists it; then a restart's. What each
/// of the two inboxes said.
fn first_start_then_restart(path: &std::path::Path) -> (String, String) {
    let earlier = session(path);
    assert_eq!(
        as_node("a", || post(&earlier, "key=held kind=ask -- RETAINED")),
        "OK f1\n"
    );
    drop(earlier);
    let first = meshed(path);
    assert!(restore(&first), "the first start's transaction runs");
    let listed = crate::cmd_inbox(&first, "");
    drop(first);
    let again = meshed(path);
    assert!(restore(&again), "the restart's transaction runs");
    (listed, crate::cmd_inbox(&again, ""))
}

#[test]
fn a_retained_unread_message_enters_the_fabric_inbox_once_and_not_again_after_a_restart() {
    let scratch = aterm_tempfile::tempdir().unwrap();
    let path = scratch.path().join("private/inbox.toml");
    let (listed, restarted) = first_start_then_restart(&path);
    assert_eq!(listed.matches("RETAINED").count(), 1, "{listed}");
    let row = listed
        .lines()
        .find(|line| line.contains("RETAINED"))
        .unwrap();
    assert!(
        row.starts_with("msg 1 off=- ")
            && row.contains(&format!(" from=fleet:{} ", "a".repeat(32)))
            && row.contains(" kind=ask trust=agent fleet=f1 "),
        "an ordinary ring row, answerable by its mailbox id: {row}"
    );
    assert!(
        !listed.contains("msg f1"),
        "no page of the mailbox follows the ring: {listed}"
    );
    assert!(
        !restarted.contains("RETAINED"),
        "imported once, not again after a restart: {restarted}"
    );
    // Kept for its sender: the answer waits there for the pre-R3 Mac's next drive.
    let after = meshed(&path);
    assert_eq!(
        post(&after, "re=f1 key=r kind=answer -- ANSWERED"),
        "OK f1 reply=1\n"
    );
    let read = as_node("a", || inbox(&after, "", false));
    assert!(read.contains("reply_text=ANSWERED"), "{read}");

    // Negative control: with no listing mark, the restart takes the message again.
    let scratch = aterm_tempfile::tempdir().unwrap();
    UNMARKED.with(|unmarked| unmarked.set(true));
    let (listed, restarted) = first_start_then_restart(&scratch.path().join("private/inbox.toml"));
    UNMARKED.with(|unmarked| unmarked.set(false));
    assert_eq!(listed.matches("RETAINED").count(), 1, "{listed}");
    assert_eq!(
        restarted.matches("RETAINED").count(),
        1,
        "the control imports it again: {restarted}"
    );
}

#[test]
fn an_arrival_with_the_mesh_on_is_one_ring_row_that_wakes_the_inbox_wait_and_counts_once() {
    let scratch = aterm_tempfile::tempdir().unwrap();
    let receiver = meshed(&scratch.path().join("private/inbox.toml"));
    assert_eq!(
        as_node("a", || post(&receiver, "key=one kind=task -- ARRIVED")),
        "OK f1\n"
    );
    assert_eq!(
        crate::cmd_await_inbox(&receiver, &["since=0"], 0),
        "OK inbox 1\n",
        "the ring row latches the wait, with no mailbox form"
    );
    let facts = receiver.fabric.mail_facts();
    assert_eq!(facts.unread, 1, "counted in the ring alone");
    assert_eq!(
        facts.last.unwrap().from,
        format!("fleet:{}", "a".repeat(32))
    );
    assert_eq!(
        receiver
            .fabric
            .mail_since(0, &crate::MailFilter::default())
            .rows
            .len(),
        0,
        "a subscriber is told of it by its mailbox id alone"
    );
    assert_eq!(
        receiver
            .fabric
            .direct_mail_since(0, &crate::MailFilter::default())
            .1
            .len(),
        1
    );
    assert!(
        restore(&receiver),
        "a refresh's transaction takes no second row"
    );
    assert_eq!(receiver.fabric.last_msg_id(), 1);
    assert_eq!(crate::cmd_inbox_seen(&receiver, "1 handled"), "OK seen=1\n");
    assert!(
        receiver.fabric.lock().receipts.is_empty(),
        "no bus receipt is owed to direct fleet mail"
    );
    let successor = meshed(&receiver.path);
    assert!(restore(&successor));
    assert_eq!(
        successor.fabric.last_msg_id(),
        0,
        "seen in the ring, so a successor takes it no more"
    );
}

/// The pre-R3 Mac `n-…bb`'s session `s-87654321`, served as a fleet drive serves it (its
/// mesh off), and an R3 session whose Mac heard `n-…aa`'s presence row and drives `n-…bb`.
fn pre_r3_pair(scratch: &std::path::Path) -> (std::sync::Arc<Session>, Session) {
    let there = std::sync::Arc::new(session(&scratch.join("there/s-87654321.toml")));
    let served = std::sync::Arc::clone(&there);
    let here = Session {
        heard: vec![format!("n-{}", "a".repeat(16))],
        drive: Some(Box::new(move |node, request| {
            assert_eq!(node, format!("n-{}", "b".repeat(16)));
            let rest = request
                .strip_prefix("@s-87654321 post ")
                .ok_or_else(|| format!("not a post to the session: {request}"))?;
            Ok(as_node("c", || crate::cmd_post(&*served, rest, None)))
        })),
        ..meshed(&scratch.join("here/inbox.toml"))
    };
    (there, here)
}

#[test]
fn a_post_to_a_pre_r3_mac_goes_by_direct_fleet_mail() {
    let scratch = aterm_tempfile::tempdir().unwrap();
    let (there, here) = pre_r3_pair(scratch.path());
    let to = format!("to=@s-87654321@n-{}", "b".repeat(16));
    assert_eq!(
        crate::cmd_post(&here, &format!("{to} kind=ask key=q1 -- ACROSS"), None),
        format!("OK fleet=f1 to=@s-87654321@n-{} key=q1\n", "b".repeat(16))
    );
    assert_eq!(
        crate::cmd_post(&here, &format!("{to} kind=ask key=q1 -- ACROSS"), None),
        format!(
            "OK fleet=f1 to=@s-87654321@n-{} key=q1 dup=1\n",
            "b".repeat(16)
        ),
        "the key holds there: posted again, it lands once"
    );
    let keyless = crate::cmd_post(&here, &format!("{to} kind=note -- KEYLESS"), None);
    assert!(
        keyless.starts_with(&format!(
            "OK fleet=f2 to=@s-87654321@n-{} key=s-12345678.",
            "b".repeat(16)
        )),
        "a key of this session's is made and named: {keyless}"
    );
    let facts = here.facts.lock().unwrap().clone();
    assert!(
        facts.len() == 2
            && facts[0].starts_with("f1 to=")
            && facts[0].ends_with(" kind=ask key=q1")
            && facts[1].starts_with("f2 to=")
            && facts[1].contains(" kind=note key=s-12345678."),
        "each message carried is one fact on this session's log, a duplicate none: {facts:?}"
    );
    let page = crate::cmd_inbox(&*there, "");
    assert!(
        page.contains(&format!("msg f1 from=fleet:{} kind=ask", "c".repeat(32)))
            && page.contains("text=ACROSS")
            && page.contains("text=KEYLESS"),
        "it is in the pre-R3 Mac's direct mailbox, from this Mac: {page}"
    );
    assert!(
        here.fabric.lock().posts.is_empty(),
        "nothing waits for a bridge here"
    );
    for refused in [
        format!("{to} kind=ack re=7 -- receipt"),
        format!("{to} kind=note re=7 -- answer by offset"),
        format!("{to} kind=note dl=5 -- deadline"),
    ] {
        let reply = crate::cmd_post(&here, &refused, None);
        assert!(reply.starts_with("ERR fleet-mail: "), "{refused}: {reply}");
    }
    assert!(
        crate::cmd_post(
            &here,
            &format!("{to} kind=note len=4"),
            Some(b"BODY".to_vec())
        )
        .starts_with("ERR fleet-mail: "),
        "direct fleet mail takes inline text"
    );
    assert_eq!(
        here.drives.lock().unwrap().len(),
        3,
        "a refusal drives nothing"
    );

    // Control: a Mac whose presence row was heard takes the post on the bus, undriven.
    let heard = format!("to=@s-87654321@n-{}", "a".repeat(16));
    assert_eq!(
        crate::cmd_post(&here, &format!("{heard} kind=note -- ON-THE-BUS"), None),
        "OK 1\n"
    );
    assert_eq!(here.fabric.lock().posts.len(), 1);
    assert_eq!(here.drives.lock().unwrap().len(), 3);

    // Control: with the mesh off, the unheard Mac is posted on the bus as before.
    let off = Session {
        drive: None,
        ..session(&scratch.path().join("off/inbox.toml"))
    };
    assert_eq!(
        crate::cmd_post(&off, &format!("{to} kind=note -- MESH-OFF"), None),
        "OK 1\n"
    );
    assert_eq!(off.fabric.lock().posts.len(), 1);
    assert!(off.drives.lock().unwrap().is_empty());
}

#[test]
fn a_report_to_a_session_on_another_mac_is_routed_to_that_mac_by_direct_fleet_mail() {
    use crate::report::{To, post as report};
    let scratch = aterm_tempfile::tempdir().unwrap();
    let there = std::sync::Arc::new(Session {
        sid: SessionId::new("s-87654321"),
        ..session(&scratch.path().join("there/s-87654321.toml"))
    });
    let served = std::sync::Arc::clone(&there);
    let node = format!("n-{}", "b".repeat(16));
    let wanted = node.clone();
    // This Mac's mesh off, as every aterm's is until R3: the bus is one Mac's alone.
    let here = Session {
        drive: Some(Box::new(move |at, request| {
            assert_eq!(at, wanted, "the drive goes to the manager's Mac");
            let rest = request
                .strip_prefix("@s-87654321 post ")
                .ok_or_else(|| format!("not a post to the manager: {request}"))?;
            Ok(as_node("c", || crate::cmd_post(&*served, rest, None)))
        })),
        ..session(&scratch.path().join("here/inbox.toml"))
    };
    let to = To::There {
        sid: "s-87654321",
        node: &node,
    };
    let line = "NEEDS YOU: a sign-in | sid=s-12345678 state=working";
    assert_eq!(
        report(&here, to, "s-12345678.need.1", line),
        Ok(format!(
            "OK fleet=f1 to=@s-87654321@{node} key=s-12345678.need.1"
        ))
    );
    assert_eq!(
        report(&here, to, "s-12345678.need.1", line),
        Ok(format!(
            "OK fleet=f1 to=@s-87654321@{node} key=s-12345678.need.1 dup=1"
        )),
        "sent again under its key, it lands once there"
    );
    let page = crate::cmd_inbox(&*there, "");
    assert!(
        page.contains(&format!("msg f1 from=fleet:{} kind=report", "c".repeat(32)))
            && page.contains("NEEDS"),
        "in the manager's inbox on its Mac, from this one: {page}"
    );
    assert!(
        here.fabric.lock().posts.is_empty(),
        "nothing is queued on this Mac's bus"
    );
    assert_eq!(
        here.facts.lock().unwrap().as_slice(),
        [format!(
            "f1 to={} kind=report key=s-12345678.need.1",
            crate::pct_encode(&format!("@s-87654321@{node}"))
        )],
        "the mail carried is one fact on the worker's log"
    );

    // Negative control: a manager's Mac that refuses the drive's post takes nothing, and the
    // refusal is the sender's to record.
    let refused = Session {
        drive: Some(Box::new(|_, _| {
            Ok("ERR denied: no drive grant\n".to_owned())
        })),
        ..session(&scratch.path().join("refused/inbox.toml"))
    };
    assert_eq!(
        report(&refused, to, "s-12345678.need.1", line),
        Err(crate::report::Unsent::Answered {
            node: node.clone(),
            answer: "ERR denied: no drive grant".to_owned()
        })
    );
    assert!(refused.facts.lock().unwrap().is_empty());
    assert_eq!(refused.drives.lock().unwrap().len(), 1);
}

/// A REPORT FROM A SESSION LEAVING THE ROSTER GOES BY DIRECT FLEET MAIL EVEN TO A MAC THE MESH
/// CARRIES A LIVE SENDER'S TO: the bus drains no outbox of a session that left. CONTROL: the
/// same report from a session that stays goes on the bus, undriven.
#[test]
fn a_leaving_sessions_report_is_driven_to_a_mac_the_mesh_would_carry_it_to() {
    use crate::report::{To, post as report, post_leaving};
    crate::with_link_reset(|| {
        crate::note_bridge_supervised();
        let scratch = aterm_tempfile::tempdir().unwrap();
        let there = std::sync::Arc::new(Session {
            sid: SessionId::new("s-87654321"),
            ..session(&scratch.path().join("there/s-87654321.toml"))
        });
        let served = std::sync::Arc::clone(&there);
        let node = format!("n-{}", "a".repeat(16));
        let here = Session {
            heard: vec![node.clone()],
            drive: Some(Box::new(move |_, request| {
                let rest = request
                    .strip_prefix("@s-87654321 post ")
                    .ok_or_else(|| format!("not a post to the manager: {request}"))?;
                Ok(as_node("c", || crate::cmd_post(&*served, rest, None)))
            })),
            ..meshed(&scratch.path().join("here/inbox.toml"))
        };
        let to = To::There {
            sid: "s-87654321",
            node: &node,
        };
        let closed = "s-12345678.closed.1";
        let line = "finished, then its tab closed | sid=s-12345678 state=closed";

        assert_eq!(
            report(&here, to, closed, line),
            Ok("OK 1".to_owned()),
            "a live sender's report rides the mesh"
        );
        assert!(here.drives.lock().unwrap().is_empty());

        assert_eq!(
            post_leaving(&here, to, closed, line),
            Ok(format!("OK fleet=f1 to=@s-87654321@{node} key={closed}"))
        );
        assert_eq!(here.fabric.lock().posts.len(), 1, "nothing more is queued");
        assert_eq!(here.drives.lock().unwrap().len(), 1);
        assert!(
            crate::cmd_inbox(&*there, "").contains("state=closed"),
            "it is in the manager's mailbox on its Mac"
        );
    });
}

#[test]
fn direct_arrival_discovers_once_without_reading_storage_on_presence_or_subscription() {
    let scratch = aterm_tempfile::tempdir().unwrap();
    let path = scratch.path().join("private/inbox.toml");
    let receiver = session(&path);
    assert_eq!(
        as_node("a", || post(
            &receiver,
            "key=notice kind=ask -- PRIVATE_BODY"
        )),
        "OK f1\n"
    );
    assert_eq!(receiver.fabric.mail_facts().unread, 1);
    assert_eq!(receiver.fabric.mail_facts().head, 1);
    assert_eq!(
        receiver.fabric.mail_facts().last.unwrap().from,
        format!("fleet:{}", "a".repeat(32))
    );
    let (high, rows) = receiver
        .fabric
        .direct_mail_since(0, &crate::MailFilter::default());
    assert_eq!(high, 1);
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].id, 1);
    assert_eq!(receiver.facts.lock().unwrap().len(), 1);
    assert!(!receiver.facts.lock().unwrap()[0].contains("PRIVATE_BODY"));
    assert_eq!(receiver.wakes.load(std::sync::atomic::Ordering::Relaxed), 1);
    assert_eq!(
        as_node("a", || post(
            &receiver,
            "key=notice kind=ask -- PRIVATE_BODY"
        )),
        "OK f1 dup=1\n"
    );
    assert_eq!(receiver.facts.lock().unwrap().len(), 1);
    assert_eq!(receiver.wakes.load(std::sync::atomic::Ordering::Relaxed), 1);
    assert!(
        receiver
            .fabric
            .direct_mail_since(1, &crate::MailFilter::default())
            .1
            .is_empty()
    );
    for filter in [
        crate::MailFilter {
            topic: Some("build".to_owned()),
            ..Default::default()
        },
        crate::MailFilter {
            from: Some("human".to_owned()),
            ..Default::default()
        },
        crate::MailFilter {
            kinds: vec!["note".to_owned()],
            ..Default::default()
        },
    ] {
        assert!(receiver.fabric.direct_mail_since(0, &filter).1.is_empty());
    }
    let successor = session(&path);
    restore(&successor);
    assert_eq!(successor.fabric.mail_facts().unread, 1);
    assert_eq!(
        successor.wakes.load(std::sync::atomic::Ordering::Relaxed),
        1
    );
    assert!(
        successor.facts.lock().unwrap().is_empty(),
        "restoring discovery must not repeat carried timeline marks"
    );
    assert_eq!(
        as_node("b", || post(
            &receiver,
            "key=later kind=note -- handoff arrival"
        )),
        "OK f2\n"
    );
    assert_eq!(
        successor.fabric.mail_facts().unread,
        1,
        "the other process has its own cache"
    );
    assert!(restore(&successor));
    assert_eq!(
        successor.fabric.mail_facts().unread,
        2,
        "a write after adoption is discovered on refresh"
    );
    assert_eq!(
        successor
            .fabric
            .direct_mail_since(1, &crate::MailFilter::default())
            .1[0]
            .id,
        2
    );
    std::fs::remove_file(&path).unwrap();
    assert_eq!(
        successor.fabric.mail_facts().unread,
        2,
        "GUI reads only cached metadata"
    );
    assert_eq!(
        successor
            .fabric
            .direct_mail_since(0, &crate::MailFilter::default())
            .1
            .len(),
        2
    );
}

#[test]
fn answering_or_retiring_direct_mail_clears_cached_pending_without_reusing_bus_cursors() {
    let scratch = aterm_tempfile::tempdir().unwrap();
    let receiver = session(&scratch.path().join("private/inbox.toml"));
    assert_eq!(
        as_node("a", || post(&receiver, "key=one kind=ask -- question")),
        "OK f1\n"
    );
    let _ = crate::cmd_inbox(&receiver, "");
    assert_eq!(receiver.fabric.mail_facts().pending, 1);
    assert_eq!(
        post(&receiver, "re=f1 key=r kind=answer -- answer"),
        "OK f1 reply=1\n"
    );
    assert_eq!(receiver.fabric.mail_facts().unread, 0);
    assert_eq!(receiver.fabric.mail_facts().pending, 0);
    assert_eq!(receiver.fabric.last_msg_id(), 0);
    assert_eq!(
        as_node("a", || post(&receiver, "key=two kind=note -- note")),
        "OK f2\n"
    );
    let _ = crate::cmd_inbox(&receiver, "");
    assert_eq!(forget(&receiver, "f2"), "OK forgot=f2\n");
    assert_eq!(receiver.fabric.mail_facts().unread, 0);
    assert_eq!(receiver.fabric.last_direct_mail_id(), 2);
    assert_eq!(
        receiver
            .facts
            .lock()
            .unwrap()
            .iter()
            .filter(|f| f.contains("moved=1"))
            .count(),
        2
    );
}

#[test]
fn a_failed_refresh_is_retryable_and_keeps_the_last_successful_snapshot() {
    use aterm_uds::private_file::PrivateLock;
    let scratch = aterm_tempfile::tempdir().unwrap();
    let path = scratch.path().join("private/inbox.toml");
    let receiver = session(&path);
    assert_eq!(
        as_node("a", || post(&receiver, "key=one kind=ask -- question")),
        "OK f1\n"
    );
    let successor = session(&path);
    let held = PrivateLock::take(
        path.parent().unwrap(),
        "inbox.toml.lock",
        std::time::Duration::ZERO,
        "test lock",
    )
    .unwrap();
    assert!(
        !restore(&successor),
        "a busy observation must not commit its stamp"
    );
    assert_eq!(successor.fabric.last_direct_mail_id(), 0);
    drop(held);
    assert!(restore(&successor));
    assert_eq!(successor.fabric.last_direct_mail_id(), 1);
    std::fs::write(&path, "not valid toml").unwrap();
    assert!(
        !restore(&successor),
        "malformed observations must not commit their stamp"
    );
    assert_eq!(successor.fabric.mail_facts().unread, 1);
}

#[test]
fn a_peer_posts_and_reads_its_reply_without_reading_another_peers_mail() {
    let scratch = aterm_tempfile::tempdir().unwrap();
    let receiver = session(&scratch.path().join("private/inbox.toml"));
    assert_eq!(
        as_node("a", || crate::cmd_post(
            &receiver,
            "key=one kind=ask -- build status?",
            None
        )),
        "OK f1\n"
    );
    assert_eq!(
        as_node("b", || crate::cmd_post(
            &receiver,
            "key=two kind=note -- unrelated",
            None
        )),
        "OK f2\n"
    );
    let read = as_node("a", || crate::cmd_inbox_verb(&receiver, ""));
    assert!(read.contains("build%20status?"), "{read}");
    assert!(!read.contains("unrelated"), "{read}");
    let local = crate::cmd_inbox(&receiver, "");
    assert!(
        local.contains("msg f1 ") && local.contains("msg f2 "),
        "{local}"
    );
    assert_eq!(
        crate::cmd_post(&receiver, "re=f1 key=answer kind=answer -- complete", None),
        "OK f1 reply=1\n"
    );
    let reply = as_node("a", || crate::cmd_inbox_verb(&receiver, ""));
    assert!(reply.contains("reply_text=complete"), "{reply}");
    assert!(!as_node("b", || crate::cmd_inbox_verb(&receiver, "")).contains("complete"));
}

#[test]
fn durable_keys_and_replies_survive_a_successor_using_the_same_session_path() {
    let scratch = aterm_tempfile::tempdir().unwrap();
    let path = scratch.path().join("private/inbox.toml");
    let before = session(&path);
    assert_eq!(
        as_node("a", || post(&before, "key=k kind=ask -- question")),
        "OK f1\n"
    );
    let successor = session(&path);
    assert_eq!(
        as_node("a", || post(&successor, "key=k kind=ask -- question")),
        "OK f1 dup=1\n"
    );
    assert!(as_node("a", || post(&successor, "key=k kind=ask -- changed")).starts_with("ERR "));
    assert_eq!(
        post(&successor, "re=f1 key=r kind=answer -- answer"),
        "OK f1 reply=1\n"
    );
    let again = session(&path);
    assert_eq!(
        post(&again, "re=f1 key=r kind=answer -- answer"),
        "OK f1 reply=1 dup=1\n"
    );
    assert!(as_node("a", || inbox(&again, "", false)).contains("reply_text=answer"));
}

#[test]
fn no_local_sender_forges_a_peer_and_no_peer_replies_or_retires_another_conversation() {
    let scratch = aterm_tempfile::tempdir().unwrap();
    let receiver = session(&scratch.path().join("private/inbox.toml"));
    assert!(post(&receiver, "key=k kind=ask -- forged").starts_with("ERR "));
    assert!(
        as_node("a", || post(
            &receiver,
            "from=other key=k kind=ask -- forged"
        ))
        .starts_with("ERR ")
    );
    assert_eq!(
        as_node("a", || post(&receiver, "key=k kind=ask -- question")),
        "OK f1\n"
    );
    assert!(
        as_node("b", || post(&receiver, "re=f1 key=r kind=answer -- forged")).starts_with("ERR ")
    );
    assert!(as_node("b", || forget(&receiver, "f1")).starts_with("ERR denied"));
    assert!(forget(&receiver, "f1").starts_with("ERR "));
}

#[test]
fn bounded_mailbox_refuses_without_losing_waiting_messages_and_wakes_normal_inbox_waits() {
    let scratch = aterm_tempfile::tempdir().unwrap();
    let receiver = session(&scratch.path().join("private/inbox.toml"));
    for n in 0..PER_NODE {
        assert!(
            as_node("a", || post(
                &receiver,
                &format!("key=k{n} kind=ask -- question")
            ))
            .starts_with("OK ")
        );
    }
    assert!(
        as_node("a", || post(&receiver, "key=overflow kind=ask -- question")).starts_with("ERR ")
    );
    assert_eq!(
        crate::cmd_await_inbox(&receiver, &["since=0"], 0),
        "OK inbox 0 fleet=f16\n"
    );
    assert_eq!(
        crate::cmd_await_inbox(&receiver, &["since=0", "fleet_since=f16"], 0),
        "OK timeout\n"
    );
    assert_eq!(
        crate::cmd_await_inbox(&receiver, &["since=f16"], 0),
        "OK timeout\n"
    );
    assert!(crate::cmd_inbox(&receiver, "").contains("msg f16 "));
    assert_eq!(
        crate::cmd_await_inbox(&receiver, &["since=0"], 0),
        "OK timeout\n"
    );
    assert_eq!(
        post(&receiver, "re=f1 key=r kind=answer -- answer"),
        "OK f1 reply=1\n"
    );
    assert!(as_node("b", || acknowledge(&receiver, "f1")).starts_with("ERR "));
    assert_eq!(
        as_node("a", || acknowledge(&receiver, "f1")),
        "OK seen=f1\n"
    );
    assert_eq!(forget(&receiver, "f1"), "OK forgot=f1\n");
    assert_eq!(
        as_node("a", || post(&receiver, "key=after kind=note -- after")),
        "OK f17\n"
    );
}

#[test]
fn damaged_durable_state_refuses_and_is_not_replaced() {
    let scratch = aterm_tempfile::tempdir().unwrap();
    let receiver = session(&scratch.path().join("private/inbox.toml"));
    as_node("a", || post(&receiver, "key=k kind=ask -- question"));
    assert_eq!(
        crate::deliver_row(
            &receiver,
            &[
                "off=8",
                "from=s-a@n-a",
                "kind=ask",
                "trust=agent",
                "text=bus"
            ],
            &|| {}
        ),
        "OK 1\n"
    );
    std::fs::write(&receiver.path, b"other-peer-private-message = [").unwrap();
    assert_eq!(
        crate::cmd_await_inbox(&receiver, &["since=0"], 0),
        "OK inbox 1\n"
    );
    assert!(crate::cmd_await_inbox(&receiver, &["since=f0"], 0).starts_with("ERR "));
    let page = crate::cmd_inbox(&receiver, "");
    assert!(
        page.starts_with("OK 1 ") && page.contains("fleet_error=unavailable"),
        "{page}"
    );
    assert!(page.contains("text=bus"), "{page}");
    assert!(
        receiver.fabric.lock().rows[0].listed,
        "unrelated damaged direct storage does not block bus delivery"
    );
    let refused = as_node("a", || post(&receiver, "key=k2 kind=ask -- question"));
    assert!(
        refused.starts_with("ERR ") && !refused.contains("other-peer-private-message"),
        "{refused}"
    );
    assert_eq!(
        std::fs::read(&receiver.path).unwrap(),
        b"other-peer-private-message = ["
    );
}

#[test]
fn bus_and_direct_cursors_can_be_rearmed_independently() {
    let scratch = aterm_tempfile::tempdir().unwrap();
    let receiver = session(&scratch.path().join("private/inbox.toml"));
    assert_eq!(
        crate::deliver_row(
            &receiver,
            &[
                "off=8",
                "from=s-a@n-a",
                "kind=ask",
                "trust=agent",
                "text=bus"
            ],
            &|| {}
        ),
        "OK 1\n"
    );
    assert_eq!(
        as_node("a", || post(&receiver, "key=k kind=ask -- direct")),
        "OK f1\n"
    );
    assert_eq!(
        crate::cmd_await_inbox(&receiver, &["since=0"], 0),
        "OK inbox 1\n"
    );
    assert_eq!(
        crate::cmd_await_inbox(&receiver, &["since=1"], 0),
        "OK inbox 1 fleet=f1\n"
    );
    assert_eq!(
        crate::cmd_await_inbox(&receiver, &["since=1", "fleet_since=f1"], 0),
        "OK timeout\n"
    );
    assert_eq!(
        crate::cmd_await_inbox(&receiver, &["since=f1"], 0),
        "OK timeout\n"
    );
    for bad in [
        &["since=f1", "fleet_since=f1"][..],
        &["fleet_since=f1", "since=f1"][..],
        &["since=f1", "since=1"][..],
        &["since=1", "since=f1"][..],
        &["re=8", "fleet_since=f1"][..],
        &["since=0", "topic=lab", "fleet_since=f1"][..],
    ] {
        assert!(
            crate::cmd_await_inbox(&receiver, bad, 0).starts_with("ERR usage"),
            "{bad:?}"
        );
    }
    assert_eq!(
        crate::deliver_row(
            &receiver,
            &[
                "off=9",
                "from=s-a@n-a",
                "kind=ask",
                "trust=agent",
                "text=next"
            ],
            &|| {}
        ),
        "OK 2\n"
    );
    assert_eq!(
        crate::cmd_await_inbox(&receiver, &["since=1", "fleet_since=f1"], 0),
        "OK inbox 2\n"
    );
}

#[test]
fn a_small_inbox_page_keeps_unreturned_bus_and_direct_rows_pending() {
    let scratch = aterm_tempfile::tempdir().unwrap();
    let receiver = session(&scratch.path().join("private/inbox.toml"));
    crate::deliver_row(
        &receiver,
        &[
            "off=8",
            "from=s-a@n-a",
            "kind=ask",
            "trust=agent",
            "text=bus",
        ],
        &|| {},
    );
    as_node("a", || post(&receiver, "key=one kind=ask -- direct-one"));
    as_node("a", || post(&receiver, "key=two kind=ask -- direct-two"));
    let empty = crate::cmd_inbox(&receiver, "0");
    assert!(
        empty.starts_with("OK 0 ") && empty.contains("pending=3"),
        "{empty}"
    );
    let page = crate::cmd_inbox(&receiver, "1");
    assert!(
        page.starts_with("OK 1 ") && page.contains("pending=2"),
        "{page}"
    );
    assert!(
        page.contains("msg f2 ") && !page.contains("msg 1 "),
        "{page}"
    );
    assert!(!receiver.fabric.lock().rows[0].listed);
    assert!(crate::cmd_inbox(&receiver, "1").contains("msg f1 "));
    let bus = crate::cmd_inbox(&receiver, "1");
    assert!(
        bus.contains("msg 1 "),
        "retained listed direct rows cannot starve bus mail: {bus}"
    );
}

#[test]
fn a_busy_durable_mailbox_waits_for_the_writer_without_losing_its_transaction() {
    let scratch = aterm_tempfile::tempdir().unwrap();
    let receiver = session(&scratch.path().join("private/inbox.toml"));
    let dir = receiver.path.parent().unwrap();
    aterm_uds::private_file::create_private_dir(dir).unwrap();
    let held = aterm_uds::private_file::PrivateLock::take(
        dir,
        "inbox.toml.lock",
        std::time::Duration::ZERO,
        "busy",
    )
    .unwrap();
    std::thread::scope(|scope| {
        let (tx, rx) = std::sync::mpsc::channel();
        let (waiting_tx, waiting_rx) = std::sync::mpsc::channel();
        let receiver = &receiver;
        scope.spawn(move || {
            let mut waiting_tx = Some(waiting_tx);
            let result = transaction_waiting(
                receiver,
                aterm_spec::wait::MUST_HAPPEN,
                |boxed| {
                    boxed.next += 1;
                    Ok((boxed.next, true))
                },
                || {
                    if let Some(tx) = waiting_tx.take() {
                        tx.send(()).unwrap();
                    }
                },
            );
            tx.send(result).unwrap()
        });
        waiting_rx
            .recv_timeout(aterm_spec::wait::MUST_HAPPEN)
            .unwrap();
        assert!(rx.try_recv().is_err(), "a held mailbox cannot be edited");
        assert_eq!(
            crate::cmd_await_inbox(receiver, &["since=0"], 0),
            "OK timeout\n"
        );
        assert_eq!(
            crate::cmd_await_inbox(receiver, &["since=f0"], 0),
            "OK timeout\n"
        );
        drop(held);
        assert_eq!(
            rx.recv_timeout(aterm_spec::wait::MUST_HAPPEN)
                .unwrap()
                .unwrap(),
            (1, true)
        );
    });
    assert_eq!(
        as_node("a", || post(&receiver, "key=k kind=ask -- question")),
        "OK f2\n"
    );
}

#[test]
fn a_recipient_can_retire_read_mail_even_if_the_sender_never_returns() {
    let scratch = aterm_tempfile::tempdir().unwrap();
    let receiver = session(&scratch.path().join("private/inbox.toml"));
    as_node("a", || post(&receiver, "key=k kind=note -- received"));
    assert!(forget(&receiver, "f1").starts_with("ERR "));
    inbox(&receiver, "--peek", false);
    assert!(forget(&receiver, "f1").starts_with("ERR "));
    inbox(&receiver, "", false);
    assert_eq!(forget(&receiver, "f1"), "OK forgot=f1\n");
    assert_eq!(
        as_node("a", || post(&receiver, "key=next kind=note -- next")),
        "OK f2\n"
    );
}

#[test]
fn a_lost_inbox_response_can_be_recovered_from_the_retained_fleet_inbox() {
    let scratch = aterm_tempfile::tempdir().unwrap();
    let receiver = session(&scratch.path().join("private/inbox.toml"));
    as_node("a", || post(&receiver, "key=k kind=ask -- retained"));
    assert!(crate::cmd_inbox(&receiver, "").contains("msg f1 "));
    assert!(crate::cmd_inbox_verb(&receiver, "fleet").contains("msg f1 "));
    assert_eq!(
        crate::cmd_await_inbox(&receiver, &["since=0"], 0),
        "OK timeout\n"
    );
}

#[test]
fn read_mail_releases_capacity_but_peeks_and_unacknowledged_replies_do_not() {
    for mesh in [false, true] {
        for nodes in [1, ROWS / PER_NODE] {
            let scratch = aterm_tempfile::tempdir().unwrap();
            let path = scratch.path().join("private/inbox.toml");
            let receiver = if mesh { meshed(&path) } else { session(&path) };
            for node in 0..nodes {
                for n in 0..PER_NODE {
                    assert!(
                        as_node(["a", "b", "c", "d"][node], || post(
                            &receiver,
                            &format!("key=k{n} kind=note -- progress")
                        ))
                        .starts_with("OK ")
                    );
                }
            }
            let next = || as_node("a", || post(&receiver, "key=next kind=note -- next"));
            crate::cmd_inbox(&receiver, "--peek");
            assert!(next().starts_with("ERR "), "peeking frees no capacity");
            crate::cmd_inbox(&receiver, "");
            post(&receiver, "re=f1 key=answer kind=answer -- retained reply");
            assert_eq!(next(), format!("OK f{}\n", nodes * PER_NODE + 1));
            assert!(
                as_node("a", || inbox(&receiver, "", false))
                    .contains("reply_text=retained%20reply")
            );
            assert_eq!(next(), format!("OK f{} dup=1\n", nodes * PER_NODE + 1));
        }
    }
}

#[test]
fn a_full_mailbox_reclaims_acknowledged_replies_without_a_manual_cleanup() {
    let scratch = aterm_tempfile::tempdir().unwrap();
    let receiver = session(&scratch.path().join("private/inbox.toml"));
    for n in 0..PER_NODE {
        as_node("a", || {
            post(&receiver, &format!("key=k{n} kind=ask -- question"))
        });
    }
    post(&receiver, "re=f1 key=r kind=answer -- complete");
    as_node("a", || acknowledge(&receiver, "f1"));
    assert_eq!(
        as_node("a", || post(&receiver, "key=next kind=note -- next")),
        "OK f17\n"
    );
    let page = inbox(&receiver, "", false);
    assert!(!page.contains("msg f1 "), "{page}");
    assert!(
        page.contains("msg f2 ") && page.contains("msg f17 "),
        "{page}"
    );
}

fn transactions() -> u64 {
    TRANSACTIONS.with(std::cell::Cell::get)
}

fn timed() -> u64 {
    TIMED.with(std::cell::Cell::get)
}

#[test]
fn the_tally_times_a_verbs_transaction_and_never_the_watchers_refresh() {
    let scratch = aterm_tempfile::tempdir().unwrap();
    let path = scratch.path().join("private/inbox.toml");
    let receiver = session(&path);
    let peer = session(&path);
    assert_eq!(
        as_node("a", || post(&peer, "key=one kind=ask -- first")),
        "OK f1\n"
    );
    let (all, verbs) = (transactions(), timed());
    assert!(restore(&receiver), "the watcher's refresh");
    assert_eq!(
        (transactions(), timed()),
        (all + 1, verbs),
        "a transaction no verb asked for"
    );
    assert_eq!(
        as_node("a", || post(&peer, "key=two kind=ask -- second")),
        "OK f2\n"
    );
    let (all, verbs) = (transactions(), timed());
    assert_eq!(
        crate::cmd_await_inbox(&receiver, &["since=0"], 0),
        "OK inbox 0 fleet=f2\n"
    );
    assert!(crate::cmd_inbox(&receiver, "").contains("msg f2 "));
    assert_eq!(
        (transactions(), timed()),
        (all + 2, verbs + 2),
        "`await inbox` and `inbox` each asked for theirs"
    );
}

#[test]
fn a_warm_cache_refuses_a_mailbox_that_loses_private_file_permissions() {
    check_warm_cache_access(false);
}

#[test]
fn a_warm_cache_refuses_a_mailbox_directory_replaced_by_a_symlink() {
    check_warm_cache_access(true);
}

fn check_warm_cache_access(directory_link: bool) {
    use std::os::unix::fs::{PermissionsExt as _, symlink};
    let scratch = aterm_tempfile::tempdir().unwrap();
    let path = scratch.path().join("private/inbox.toml");
    let receiver = session(&path);
    assert_eq!(
        as_node("a", || post(&receiver, "key=one kind=ask -- private")),
        "OK f1\n"
    );
    assert_eq!(pending(&receiver, 0, |_| true).unwrap(), Some(1));
    if directory_link {
        let moved = scratch.path().join("moved");
        std::fs::rename(path.parent().unwrap(), &moved).unwrap();
        symlink(moved, path.parent().unwrap()).unwrap();
    } else {
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
    }
    assert!(
        pending(&receiver, 0, |_| true).is_err(),
        "a cached notification cannot bypass the private mailbox read"
    );
}

#[test]
fn no_mailbox_in_a_directory_a_transaction_refuses_is_refused_not_read_as_empty() {
    use std::os::unix::fs::PermissionsExt as _;
    let scratch = aterm_tempfile::tempdir().unwrap();
    let path = scratch.path().join("private/inbox.toml");
    let receiver = session(&path);
    let dir = path.parent().unwrap();
    let mode = |bits| std::fs::set_permissions(dir, std::fs::Permissions::from_mode(bits));
    std::fs::create_dir(dir).unwrap();
    mode(0o700).unwrap();
    // The control: a private directory with no mailbox in it is an empty mailbox.
    let page = crate::cmd_inbox(&receiver, "");
    assert!(
        page.starts_with("OK 0 ") && !page.contains("fleet_error"),
        "{page}"
    );
    assert_eq!(
        crate::cmd_await_inbox(&receiver, &["since=f0"], 0),
        "OK timeout\n"
    );
    // Open to others, the directory no transaction would use: still no file in it, and no
    // reader may call that an empty mailbox.
    mode(0o755).unwrap();
    assert!(
        crate::cmd_inbox_verb(&receiver, "fleet").starts_with("ERR fleet-mail: "),
        "the transaction refuses it"
    );
    let page = crate::cmd_inbox(&receiver, "");
    assert!(page.contains(" fleet_error=unavailable"), "{page}");
    let waited = crate::cmd_await_inbox(&receiver, &["since=f0"], 0);
    assert!(waited.starts_with("ERR fleet-mail: "), "{waited}");
    assert!(pending(&receiver, 0, |_| true).is_err());
    assert!(
        !dir.join("inbox.toml.lock").exists(),
        "refused before the lock"
    );
}

#[test]
fn a_session_with_no_mailbox_lists_and_waits_without_making_its_directory_or_lock() {
    let scratch = aterm_tempfile::tempdir().unwrap();
    let path = scratch.path().join("private/inbox.toml");
    let receiver = session(&path);
    let before = transactions();
    for _ in 0..8 {
        let page = crate::cmd_inbox(&receiver, "");
        assert!(
            page.starts_with("OK 0 ") && !page.contains("fleet_error"),
            "{page}"
        );
        for args in [
            &["since=0"][..],
            &["since=f0"],
            &["since=0", "fleet_since=f0"],
        ] {
            assert_eq!(crate::cmd_await_inbox(&receiver, args, 0), "OK timeout\n");
        }
        assert_eq!(
            crate::cmd_await_inbox(&receiver, &["since=0"], 1),
            "OK timeout\n",
            "a wait that parks reads no mailbox either"
        );
    }
    assert_eq!(transactions(), before);
    let dir = path.parent().unwrap();
    assert!(!dir.exists(), "no directory, so no inbox.toml.lock");
    // The negative control: the transaction those readers no longer take makes both.
    assert!(crate::cmd_inbox_verb(&receiver, "fleet").starts_with("OK 0 fleet=1 "));
    assert_eq!(transactions(), before + 1);
    assert!(dir.join("inbox.toml.lock").exists());
    assert!(
        !dir.join(KEY).exists(),
        "a transaction that writes nothing mints no key"
    );
}

/// The mailbox at `path` as the writer before sealing left it: its TOML, in plaintext.
fn plaintext_writer(path: &std::path::Path, boxed: &Mailbox) {
    aterm_uds::private_file::write_private(
        path.parent().unwrap(),
        path.file_name().unwrap().to_str().unwrap(),
        aterm_toml::to_string(boxed).unwrap().as_bytes(),
    )
    .unwrap();
}

/// The mailbox at `path`, opened as a transaction opens it.
fn opened(path: &std::path::Path) -> Result<Mailbox, String> {
    let dir = path.parent().unwrap();
    let name = path.file_name().unwrap().to_str().unwrap();
    let bytes = aterm_uds::private_file::read_private(dir, name, SEALED_BYTES)
        .map_err(|e| e.to_string())?;
    open_mailbox(dir, name, bytes, &mut None).map(|(boxed, _)| boxed)
}

/// Whether the file at `path` holds `text`'s bytes anywhere.
fn holds(path: &std::path::Path, text: &str) -> bool {
    std::fs::read(path)
        .unwrap()
        .windows(text.len())
        .any(|window| window == text.as_bytes())
}

fn legacy_mailbox(path: &std::path::Path) -> Vec<u8> {
    aterm_uds::private_file::create_private_dir(path.parent().unwrap()).unwrap();
    plaintext_writer(
        path,
        &Mailbox {
            next: 1,
            rows: vec![Row {
                id: 1,
                node: "a".repeat(32),
                key: "earlier-post".into(),
                kind: "ask".into(),
                text: "earlier question".into(),
                listed: false,
                reply: Some(Reply {
                    key: "earlier-reply".into(),
                    kind: "answer".into(),
                    text: "earlier answer".into(),
                }),
                reply_seen: false,
            }],
        },
    );
    std::fs::read(path).unwrap()
}

/// The pre-sealing reader opens the file anew for every transaction, then requires UTF-8
/// and TOML. Keeping its session idle holds no transaction lock.
fn earlier_reader(path: &std::path::Path) -> Result<String, String> {
    let bytes = aterm_uds::private_file::read_private(
        path.parent().unwrap(),
        path.file_name().unwrap().to_str().unwrap(),
        FILE_BYTES,
    )
    .map_err(|e| e.to_string())?;
    let text = std::str::from_utf8(&bytes).map_err(|e| e.to_string())?;
    let mailbox: Mailbox = aterm_toml::from_str(text).map_err(|e| e.to_string())?;
    aterm_toml::to_string(&mailbox).map_err(|e| e.to_string())
}

#[test]
fn migration_keeps_an_idle_earlier_readers_mailbox_unchanged() {
    let scratch = aterm_tempfile::tempdir().unwrap();
    let path = scratch.path().join("private/s-aaaaaaaa.toml");
    let before = legacy_mailbox(&path);
    let expected = earlier_reader(&path).unwrap();
    let dir = path.parent().unwrap();
    // A new owner refreshes its own mailbox, without adopting the older owner's file.
    let current = session(&dir.join("s-bbbbbbbb.toml"));
    let first = restore(&current);
    let first_read = earlier_reader(&path);
    let second = restore(&current);
    let second_read = earlier_reader(&path);
    assert_eq!(first_read, Ok(expected.clone()), "startup {first:?}");
    assert_eq!(second_read, Ok(expected), "startup {second:?}");
    assert_eq!(std::fs::read(&path).unwrap(), before);
    assert!(!dir.join(KEY).exists(), "a foreign mailbox mints no key");
}

#[test]
fn a_storage_endpoint_alone_does_not_admit_mailbox_io() {
    // This adapter exposes a valid endpoint but makes no ownership declaration.
    struct Endpoint<'a>(&'a Session);
    impl FabricSession for Endpoint<'_> {
        fn self_id(&self) -> &SessionId {
            self.0.self_id()
        }
        fn fabric(&self) -> &SessionFabric {
            self.0.fabric()
        }
        fn record_fact(&self, who: &'static str, fact: FabricFact<'_>) -> bool {
            self.0.record_fact(who, fact)
        }
        fn fabric_changed(&self) {
            self.0.fabric_changed();
        }
        fn holder_token(&self) -> Option<String> {
            self.0.holder_token()
        }
        fn fleet_mail_path(&self) -> Option<std::path::PathBuf> {
            self.0.fleet_mail_path()
        }
    }

    let scratch = aterm_tempfile::tempdir().unwrap();
    let path = scratch.path().join("private/s-aaaaaaaa.toml");
    let before = legacy_mailbox(&path);
    let owner = session(&path);
    let endpoint = Endpoint(&owner);
    assert!(!restore(&endpoint));
    let posted = as_node("b", || post(&endpoint, "key=unowned kind=ask -- unowned"));
    assert!(posted.starts_with("ERR "), "{posted}");
    assert_eq!(std::fs::read(&path).unwrap(), before);
    assert!(!path.parent().unwrap().join(KEY).exists());
    assert!(owner.fabric.lock().direct_mail.rows.is_empty());
    assert!(owner.facts.lock().unwrap().is_empty());
    assert_eq!(owner.wakes.load(std::sync::atomic::Ordering::Relaxed), 0);

    // The same endpoint is usable once its actual owner declares readiness.
    assert!(restore(&owner));
    assert!(std::fs::read(&path).unwrap().starts_with(SEALED));
}

#[test]
fn migration_waits_for_committed_ownership_before_reading_or_writing() {
    use std::sync::atomic::Ordering;
    let scratch = aterm_tempfile::tempdir().unwrap();
    let missing = scratch.path().join("not-created/inbox.toml");
    let pending = session(&missing);
    pending.ready.store(false, Ordering::SeqCst);
    assert!(!restore(&pending));
    assert!(as_node("b", || post(&pending, "key=new kind=ask -- new")).starts_with("ERR "));
    assert!(
        !missing.parent().unwrap().exists(),
        "no directory before Commit"
    );
    let path = scratch.path().join("private/s-aaaaaaaa.toml");
    let before = legacy_mailbox(&path);
    let owner = std::sync::Arc::new(session(&path));
    owner.ready.store(false, Ordering::SeqCst);
    // Use the Arc forwarding implementation too: a wrapper must not grant readiness.
    let refreshed = restore(&owner);
    let posted = as_node("b", || post(&owner, "key=pending kind=ask -- pending"));
    assert!(!refreshed, "an uncommitted candidate restored mail");
    assert!(posted.starts_with("ERR "), "{posted}");
    assert_eq!(std::fs::read(&path).unwrap(), before);
    assert!(!path.parent().unwrap().join(KEY).exists());
    assert!(owner.fabric.lock().direct_mail.rows.is_empty());
    assert!(owner.facts.lock().unwrap().is_empty());
    assert_eq!(owner.wakes.load(Ordering::SeqCst), 0);

    owner.ready.store(true, Ordering::SeqCst);
    assert!(restore(&owner), "Commit activates the same owner");
    assert!(std::fs::read(&path).unwrap().starts_with(SEALED));
    let page = inbox(&owner, "", false);
    assert!(
        page.contains("text=earlier%20question") && page.contains("reply_text=earlier%20answer"),
        "{page}"
    );
    assert_eq!(
        as_node("b", || post(&owner, "key=committed kind=ask -- later")),
        "OK f2\n"
    );
    owner.ready.store(false, Ordering::SeqCst);
    let before = std::fs::read(&path).unwrap();
    let page = inbox(&owner, "", true);
    assert!(
        page.starts_with("ERR "),
        "a warm cache bypassed ownership: {page}"
    );
    assert_eq!(std::fs::read(&path).unwrap(), before);
}

#[test]
fn no_row_text_reaches_the_disk_and_an_earlier_builds_plaintext_is_sealed_by_its_next_transaction()
{
    let scratch = aterm_tempfile::tempdir().unwrap();
    let path = scratch.path().join("private/s-12345678.toml");
    let receiver = session(&path);
    assert_eq!(
        as_node("a", || post(
            &receiver,
            "key=one kind=ask -- PRIVATE_ROW_TEXT"
        )),
        "OK f1\n"
    );
    assert_eq!(
        post(&receiver, "re=f1 key=r kind=answer -- PRIVATE_REPLY_TEXT"),
        "OK f1 reply=1\n"
    );
    let texts = ["PRIVATE_ROW_TEXT", "PRIVATE_REPLY_TEXT", "[[rows]]"];
    let sealed = |why: &str| {
        assert!(std::fs::read(&path).unwrap().starts_with(SEALED), "{why}");
        for text in texts {
            assert!(!holds(&path, text), "{why}: {text} on disk");
        }
    };
    sealed("after a post and a reply");
    // The negative control: the writer before sealing leaves every row's text readable.
    let boxed = opened(&path).unwrap();
    plaintext_writer(&path, &boxed);
    for text in texts {
        assert!(holds(&path, text), "the old writer's file holds {text}");
    }
    // Its next transaction, a remote read that edits nothing, seals it and loses no row.
    let successor = session(&path);
    let page = as_node("a", || inbox(&successor, "", false));
    assert!(
        page.contains("text=PRIVATE_ROW_TEXT") && page.contains("reply_text=PRIVATE_REPLY_TEXT"),
        "{page}"
    );
    sealed("after a read");
    // As does one whose edit refuses: the seal comes first.
    plaintext_writer(&path, &boxed);
    assert!(forget(&successor, "f9").starts_with("ERR "));
    sealed("after a refused verb");
    assert_eq!(opened(&path).unwrap().rows.len(), 1);
}

#[test]
fn a_mailbox_moved_to_another_sessions_file_does_not_open_there_and_is_not_replaced() {
    let scratch = aterm_tempfile::tempdir().unwrap();
    let mine = scratch.path().join("private/s-11111111.toml");
    let theirs = mine.with_file_name("s-22222222.toml");
    assert_eq!(
        as_node("a", || post(
            &session(&mine),
            "key=k kind=ask -- MOVED_TEXT"
        )),
        "OK f1\n"
    );
    std::fs::rename(&mine, &theirs).unwrap();
    let moved = std::fs::read(&theirs).unwrap();
    let other = session(&theirs);
    let refused = crate::cmd_inbox_verb(&other, "fleet");
    assert!(
        refused.starts_with("ERR fleet-mail: ") && refused.contains("does not open"),
        "{refused}"
    );
    assert!(as_node("a", || post(&other, "key=k2 kind=note -- next")).starts_with("ERR "));
    assert_eq!(std::fs::read(&theirs).unwrap(), moved, "left as it is");
    // The control: under its own name, the same bytes open.
    std::fs::rename(&theirs, &mine).unwrap();
    let page = crate::cmd_inbox_verb(&session(&mine), "fleet");
    assert!(page.contains("text=MOVED_TEXT"), "{page}");
}

#[test]
fn a_missing_key_refuses_direct_mail_mints_none_and_leaves_bus_mail_listed() {
    let scratch = aterm_tempfile::tempdir().unwrap();
    let path = scratch.path().join("private/inbox.toml");
    let dir = path.parent().unwrap();
    let key = dir.join(KEY);
    let receiver = session(&path);
    assert_eq!(
        as_node("a", || post(&receiver, "key=k kind=ask -- direct")),
        "OK f1\n"
    );
    assert_eq!(
        crate::deliver_row(
            &receiver,
            &[
                "off=8",
                "from=s-a@n-a",
                "kind=ask",
                "trust=agent",
                "text=bus"
            ],
            &|| {}
        ),
        "OK 1\n"
    );
    let held = std::fs::read(&key).unwrap();
    let sealed = std::fs::read(&path).unwrap();
    std::fs::remove_file(&key).unwrap();
    let refused = crate::cmd_inbox_verb(&receiver, "fleet");
    assert!(
        refused.starts_with("ERR fleet-mail: ") && refused.contains("key beside it is missing"),
        "{refused}"
    );
    let page = crate::cmd_inbox(&receiver, "");
    assert!(
        page.starts_with("OK 1 ")
            && page.contains("fleet_error=unavailable")
            && page.contains("text=bus"),
        "{page}"
    );
    assert!(
        crate::cmd_await_inbox(&session(&path), &["since=f0"], 0).starts_with("ERR fleet-mail: "),
        "a wait with no cache reads the mailbox"
    );
    assert!(as_node("a", || post(&receiver, "key=k2 kind=ask -- next")).starts_with("ERR "));
    assert!(!key.exists(), "no key is minted beside a sealed mailbox");
    assert_eq!(std::fs::read(&path).unwrap(), sealed, "left as it is");
    // The control: the key put back opens it, and is kept out of backups again.
    aterm_uds::private_file::write_private(dir, KEY, &held).unwrap();
    let page = crate::cmd_inbox_verb(&receiver, "fleet");
    assert!(
        page.contains("msg f1 ") && page.contains("text=direct"),
        "{page}"
    );
    #[cfg(target_os = "macos")]
    assert!(BACKUPS.excluded(&std::fs::File::open(&key).unwrap()));
}

#[test]
fn the_key_is_owner_only_and_left_out_of_backups() {
    use std::os::unix::fs::PermissionsExt as _;
    let scratch = aterm_tempfile::tempdir().unwrap();
    let path = scratch.path().join("private/inbox.toml");
    let key = path.with_file_name(KEY);
    let receiver = session(&path);
    assert_eq!(
        as_node("a", || post(&receiver, "key=k kind=ask -- question")),
        "OK f1\n"
    );
    let meta = std::fs::metadata(&key).unwrap();
    assert_eq!((meta.permissions().mode() & 0o777, meta.len()), (0o600, 32));
    // Only macOS has a mark; elsewhere nothing is left out of any backup, and the mint says so.
    #[cfg(target_os = "macos")]
    {
        let marked = |path: &std::path::Path| BACKUPS.excluded(&std::fs::File::open(path).unwrap());
        assert!(marked(&key), "the key is left out of backups");
        assert!(
            !marked(&path),
            "the control: the sealed mailbox beside it is not"
        );
    }
    let mode = |bits| std::fs::set_permissions(&key, std::fs::Permissions::from_mode(bits));
    mode(0o644).unwrap();
    let refused = crate::cmd_inbox_verb(&session(&path), "fleet");
    assert!(
        refused.starts_with("ERR fleet-mail: ") && refused.contains("key does not read"),
        "{refused}"
    );
    assert!(as_node("b", || post(&receiver, "key=k2 kind=note -- next")).starts_with("ERR "));
    // The control: back at 0600, it reads.
    mode(0o600).unwrap();
    assert!(crate::cmd_inbox_verb(&session(&path), "fleet").contains("msg f1 "));
}

#[test]
fn a_key_of_another_length_is_refused_by_the_length_it_has() {
    let scratch = aterm_tempfile::tempdir().unwrap();
    let path = scratch.path().join("private/inbox.toml");
    let dir = path.parent().unwrap();
    let receiver = session(&path);
    assert_eq!(
        as_node("a", || post(&receiver, "key=k kind=ask -- question")),
        "OK f1\n"
    );
    let held = std::fs::read(dir.join(KEY)).unwrap();
    for len in [64, 31] {
        aterm_uds::private_file::write_private(dir, KEY, &vec![7; len]).unwrap();
        let refused = crate::cmd_inbox_verb(&session(&path), "fleet");
        assert!(
            refused.contains(&format!("it holds {len} bytes, not 32")),
            "{refused}"
        );
    }
    // The control: the key it was sealed under reads.
    aterm_uds::private_file::write_private(dir, KEY, &held).unwrap();
    assert!(crate::cmd_inbox_verb(&session(&path), "fleet").contains("msg f1 "));
}

#[test]
fn no_key_is_minted_while_another_mailbox_beside_it_is_sealed_so_the_key_put_back_opens_all() {
    let scratch = aterm_tempfile::tempdir().unwrap();
    let first = scratch.path().join("private/s-aaaaaaaa.toml");
    let second = first.with_file_name("s-bbbbbbbb.toml");
    let dir = first.parent().unwrap();
    let key = dir.join(KEY);
    assert_eq!(
        as_node("a", || post(&session(&first), "key=k kind=ask -- FIRST")),
        "OK f1\n"
    );
    let held = std::fs::read(&key).unwrap();
    std::fs::remove_file(&key).unwrap();
    // The second session has no mailbox: a key its first write minted would leave the first
    // session's mail unopened under any key file.
    let refused = as_node("a", || post(&session(&second), "key=k kind=ask -- SECOND"));
    assert!(
        refused.starts_with("ERR fleet-mail: ")
            && refused.contains("sealed")
            && refused.contains("put the key back"),
        "{refused}"
    );
    assert!(!key.exists(), "no key is minted beside a sealed mailbox");
    assert!(!second.exists());
    // Put back, the one key opens both: the first session's mail, and the second's after it.
    aterm_uds::private_file::write_private(dir, KEY, &held).unwrap();
    assert_eq!(
        as_node("a", || post(&session(&second), "key=k kind=ask -- SECOND")),
        "OK f1\n"
    );
    let page = |path: &std::path::Path| crate::cmd_inbox_verb(&session(path), "fleet");
    assert!(page(&first).contains("text=FIRST"), "{}", page(&first));
    assert!(page(&second).contains("text=SECOND"), "{}", page(&second));
    // The control: with every sealed mailbox moved out of the folder, a lost key is replaced.
    std::fs::remove_file(&key).unwrap();
    for path in [&first, &second] {
        std::fs::rename(path, scratch.path().join(path.file_name().unwrap())).unwrap();
    }
    let third = first.with_file_name("s-cccccccc.toml");
    assert_eq!(
        as_node("a", || post(&session(&third), "key=k kind=ask -- THIRD")),
        "OK f1\n"
    );
    assert!(key.exists());
}

#[test]
fn a_changed_stamp_takes_the_transaction_and_an_unchanged_one_reads_the_cache() {
    let scratch = aterm_tempfile::tempdir().unwrap();
    let path = scratch.path().join("private/inbox.toml");
    let receiver = session(&path);
    let peer = session(&path);
    assert_eq!(
        as_node("a", || post(&receiver, "key=one kind=ask -- question")),
        "OK f1\n"
    );
    let before = transactions();
    assert_eq!(
        crate::cmd_await_inbox(&receiver, &["since=0"], 0),
        "OK inbox 0 fleet=f1\n"
    );
    assert_eq!(transactions(), before, "the post's own write is cached");

    assert_eq!(
        as_node("b", || post(&peer, "key=two kind=ask -- from the peer")),
        "OK f2\n"
    );
    let before = transactions();
    let after_f1 = ["since=0", "fleet_since=f1"];
    assert_eq!(
        crate::cmd_await_inbox(&receiver, &after_f1, 0),
        "OK inbox 0 fleet=f2\n"
    );
    assert_eq!(
        transactions(),
        before + 1,
        "the peer's write moved the stamp"
    );
    assert_eq!(
        crate::cmd_await_inbox(&receiver, &after_f1, 0),
        "OK inbox 0 fleet=f2\n"
    );
    assert_eq!(
        transactions(),
        before + 1,
        "and that read cached the new one"
    );

    let page = crate::cmd_inbox(&receiver, "");
    assert!(
        page.contains("msg f1 ") && page.contains("msg f2 "),
        "{page}"
    );
    assert_eq!(
        transactions(),
        before + 2,
        "rows to mark take the transaction"
    );
    let page = crate::cmd_inbox(&receiver, "");
    assert!(page.starts_with("OK 0 "), "{page}");
    assert_eq!(
        crate::cmd_await_inbox(&receiver, &["since=0"], 0),
        "OK timeout\n"
    );
    assert_eq!(transactions(), before + 2, "a page with nothing to list");

    assert_eq!(
        as_node("b", || post(&peer, "key=three kind=ask -- unread")),
        "OK f3\n"
    );
    crate::deliver_row(
        &receiver,
        &[
            "off=8",
            "from=s-a@n-a",
            "kind=ask",
            "trust=agent",
            "text=bus",
        ],
        &|| {},
    );
    let before = transactions();
    assert_eq!(
        crate::cmd_await_inbox(&receiver, &["since=0"], 0),
        "OK inbox 1\n"
    );
    assert_eq!(
        transactions(),
        before,
        "a latching bus row reads no mailbox"
    );
}

#[test]
fn a_handoff_peers_write_is_listed_by_the_next_inbox_and_its_refresh_wakes_a_parked_wait() {
    use std::sync::atomic::Ordering;
    let scratch = aterm_tempfile::tempdir().unwrap();
    let path = scratch.path().join("private/inbox.toml");
    let receiver = session(&path);
    // Another process's handle on the same mailbox: its writes ring only its own waiters.
    let peer = session(&path);
    assert_eq!(
        as_node("a", || post(&receiver, "key=one kind=ask -- first")),
        "OK f1\n"
    );
    assert!(crate::cmd_inbox(&receiver, "").contains("msg f1 "));
    assert_eq!(
        as_node("b", || post(&peer, "key=two kind=ask -- second")),
        "OK f2\n"
    );
    assert_eq!(
        receiver.fabric.last_direct_mail_id(),
        1,
        "nothing refreshed"
    );
    let page = crate::cmd_inbox(&receiver, "");
    assert!(
        page.contains("msg f2 ") && !page.contains("msg f1 "),
        "{page}"
    );

    let parks = || receiver.fabric.inbox_parks.load(Ordering::SeqCst);
    // What rings the receiver's parked waits: the epoch and the session's watchers move
    // together, once a ring.
    let rings = || {
        (
            receiver.fabric.direct_epoch.load(Ordering::SeqCst),
            receiver.wakes.load(Ordering::SeqCst),
        )
    };
    // Past the hang detector below, so an answer that comes before it is a ring's and never
    // the wait's deadline: a wait that times out reads the mailbox as it answers.
    let timeout = 2 * aterm_spec::wait::MUST_HAPPEN;
    // A wait parked on the receiver while the peer writes `key`, and then the watcher's
    // refresh, unrung when `unrung`; an unrung refresh is followed by a bus row's ring.
    // Its answer, how often it parked, and how far the refresh moved the rings.
    let parked_through = |key: &str, unrung: bool| {
        /// A failed assertion ends the wait at once, not at its deadline.
        struct Release<'a>(&'a SessionFabric);
        impl Drop for Release<'_> {
            fn drop(&mut self) {
                if std::thread::panicking() {
                    self.0.close();
                }
            }
        }
        std::thread::scope(|scope| {
            let (tx, rx) = std::sync::mpsc::channel();
            let before = parks();
            let receiver = &receiver;
            let armed = std::time::Instant::now();
            scope.spawn(move || {
                let timeout = u64::try_from(timeout.as_millis()).unwrap();
                // Unheard once a failed assertion has gone.
                let _ = tx.send(crate::cmd_await_inbox(receiver, &["since=0"], timeout));
            });
            let _release = Release(&receiver.fabric);
            aterm_spec::wait::within(
                aterm_spec::wait::MUST_HAPPEN,
                std::time::Duration::from_millis(1),
                || (parks() > before).then_some(()),
            )
            .expect("the wait parks");
            let bound = receiver
                .fabric
                .inbox_park_until
                .lock()
                .unwrap()
                .expect("a park's bound");
            assert!(
                bound >= armed + timeout,
                "no recheck ends the park before the wait's own deadline"
            );
            assert!(
                as_node("b", || post(&peer, &format!("key={key} kind=ask -- later")))
                    .starts_with("OK f")
            );
            let (epoch, wakes) = rings();
            UNRUNG.with(|u| u.set(unrung));
            let refreshed = restore(receiver);
            UNRUNG.with(|u| u.set(false));
            assert!(refreshed, "the watcher's refresh");
            let (rung_epoch, rung_wakes) = rings();
            if unrung {
                crate::deliver_row(
                    receiver,
                    &[
                        "off=8",
                        "from=s-a@n-a",
                        "kind=ask",
                        "trust=agent",
                        "text=bus",
                    ],
                    &|| {},
                );
            }
            let answer = rx.recv_timeout(aterm_spec::wait::MUST_HAPPEN);
            if answer.is_err() {
                receiver.fabric.wake_waiters_for_test();
            }
            (
                answer.expect("the parked wait answers"),
                parks() - before,
                (rung_epoch - epoch, rung_wakes - wakes),
            )
        })
    };
    assert_eq!(
        parked_through("three", false),
        ("OK inbox 0 fleet=f3\n".to_owned(), 1, (1, 1)),
        "the refresh rang the one park awake"
    );
    assert!(crate::cmd_inbox(&receiver, "").contains("msg f3 "));
    // The negative control. Unrung, the refresh moves no ring, and the park's only other
    // bound is the wait's own deadline (both asserted above, neither by a race), so the
    // wait sleeps through f4 until the bus row's ring, which latches before the mailbox is
    // read.
    assert_eq!(
        parked_through("four", true),
        ("OK inbox 1\n".to_owned(), 1, (0, 0))
    );
    assert_eq!(
        receiver.fabric.last_direct_mail_id(),
        4,
        "refreshed, unrung"
    );
}

#[test]
fn inbox_verb_and_direct_mail_parsers_reject_signed_numbers_and_stuck_fleet_prefixes() {
    let scratch = aterm_tempfile::tempdir().unwrap();
    let receiver = session(&scratch.path().join("private/inbox.toml"));
    assert_eq!(
        as_node("a", || post(&receiver, "key=k kind=ask -- question")),
        "OK f1\n"
    );
    assert_eq!(
        as_node("a", || crate::cmd_inbox_verb(&receiver, "seen")),
        "ERR usage: inbox seen f<id>\n"
    );
    assert!(as_node("a", || crate::cmd_inbox_verb(&receiver, "fleet--peek")).starts_with("ERR "));
    assert!(as_node("a", || crate::cmd_inbox_verb(&receiver, "fleet1")).starts_with("ERR "));
    assert!(as_node("a", || inbox(&receiver, "since=f+1", false)).starts_with("ERR "));
    assert!(as_node("a", || inbox(&receiver, "+1", false)).starts_with("ERR "));
    assert_eq!(
        post(&receiver, "re=f+1 key=r kind=answer -- answer"),
        "ERR fleet-mail: invalid direct-mail option\n"
    );
    let _ = crate::cmd_inbox(&receiver, "");
    assert_eq!(
        crate::cmd_inbox_verb(&receiver, "seen   f+1 forget"),
        "ERR usage: inbox seen f<id> forget\n"
    );
    assert_eq!(
        crate::cmd_inbox_verb(&receiver, "seen   f1 forget"),
        "OK forgot=f1\n"
    );
}
