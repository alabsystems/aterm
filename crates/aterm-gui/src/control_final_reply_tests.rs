// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::FinalReplies;
use aterm_types::control_request::head;
use std::time::Duration;

#[test]
fn only_a_request_that_can_end_the_process_is_owed() {
    static REPLIES: FinalReplies = FinalReplies::new();
    for line in [
        "close",
        "@s-1 close",
        "tab close",
        "tab close 1",
        "invoke Quit",
        "confirm yes",
    ] {
        let guard = REPLIES.enter(&head(line));
        assert!(guard.is_some(), "{line:?} can end the process");
        assert_eq!(REPLIES.owed(), 1, "{line:?}");
        drop(guard);
        assert_eq!(REPLIES.owed(), 0, "{line:?} delivered");
    }
    let close = REPLIES.enter(&head("close")).expect("close is owed");
    assert_eq!(REPLIES.owed(), 1);
    for line in [
        "version",
        "ls",
        "tab new",
        "tab 1",
        "text",
        "closed",
        "await seq 3",
    ] {
        assert!(REPLIES.enter(&head(line)).is_none(), "{line:?}");
        assert_eq!(REPLIES.owed(), 1, "{line:?} cannot deliver close's reply");
    }
    drop(close);
    assert_eq!(REPLIES.owed(), 0, "only close's guard delivers its reply");
}

#[test]
fn the_exit_waits_for_an_owed_reply_and_only_to_its_bound() {
    static REPLIES: FinalReplies = FinalReplies::new();
    assert!(REPLIES.settle(Duration::ZERO), "nothing owed: no wait");
    let guard = REPLIES.enter(&head("close")).expect("close is owed");
    assert!(
        !REPLIES.settle(Duration::from_millis(20)),
        "owed: the wait ends at its bound"
    );
    let writer = std::thread::spawn(move || drop(guard));
    assert!(
        REPLIES.settle(aterm_spec::wait::MUST_HAPPEN),
        "delivered: the wait ends"
    );
    writer.join().expect("writer thread");
}

#[test]
fn an_input_verbs_reply_is_owed_until_delivered_in_its_registry() {
    let replies = super::InputReplies::default();
    let other = super::InputReplies::default();
    for line in [
        "send ls",
        "@s-1 key enter",
        "hwkey a",
        "ctrl c",
        "paste hi",
        "paste-bin 3",
        "feed 6869",
        "@s-1 feed-bin 3",
        "mouse click left 1 1",
    ] {
        let guard = replies.enter(line);
        assert!(guard.is_some(), "{line:?} writes input");
        assert!(replies.owed(), "{line:?}");
        assert!(
            replies.try_fence().is_none(),
            "{line:?}: its reply precedes Commit"
        );
        assert!(!other.owed(), "{line:?}: another registry owes nothing");
        drop(guard);
        assert!(!replies.owed(), "{line:?} delivered");
    }
    for line in [
        "text",
        "turn hi",
        "close",
        "tab close",
        "ls",
        "keys",
        "native-focus",
    ] {
        assert!(replies.enter(line).is_none(), "{line:?}");
    }
    assert!(!replies.owed());

    let fence = replies.try_fence().expect("nothing owed: admission closes");
    assert!(
        replies.try_fence().is_none(),
        "one exclusive Commit admission"
    );
    assert!(
        replies.enter("text").is_none(),
        "a read takes no input admission"
    );
    assert!(
        replies.enter("native-focus").is_none(),
        "native focus remains readable under the exclusive input fence"
    );
    let elsewhere = other.enter("send hi").expect("another registry stays open");
    drop(elsewhere);
    drop(fence); // A failed Commit lets a later request start.
    let later = replies.enter("send hi").expect("admission reopened");
    assert!(
        replies.try_fence().is_none(),
        "the later request now precedes Commit"
    );
    drop(later);
    assert!(
        replies.try_fence().is_some(),
        "delivered: Commit may fence again"
    );
}

#[test]
#[cfg(unix)]
fn a_lane_owes_an_input_reply_until_it_is_delivered() {
    use super::control_lanes::{LaneLimits, LaneReader, LaneService, Lanes, Outbox};
    use std::io::{BufRead as _, Write as _};
    use std::sync::{Arc, Mutex};

    #[derive(Default)]
    struct Service {
        replies: super::InputReplies,
        seen: Mutex<Vec<(String, bool)>>,
    }
    impl LaneService for Service {
        fn owed_input_reply(&self, request: &super::Request) -> Option<super::FinalReply> {
            self.replies.enter_head(&request.head())
        }
        fn authenticate(
            &self,
            _stream: &aterm_uds::CtlStream,
            reader: &mut LaneReader<'_>,
        ) -> Option<(super::Scope, super::Principal, Option<String>)> {
            (super::read_request_line(reader)? == "AUTH ok").then_some((
                super::Scope::Owner,
                super::Principal::Instance,
                None,
            ))
        }
        fn serve_line(
            &self,
            request: super::Request,
            _scope: super::Scope,
            _stream: &aterm_uds::CtlStream,
            _reader: &mut LaneReader<'_>,
            out: &mut Outbox,
        ) -> Option<super::ServeDisposition> {
            let line = request.line().to_string();
            out.extend_from_slice(format!("OK {line}\n").as_bytes());
            let owed = self.replies.owed();
            self.seen
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .push((line, owed));
            None
        }
        fn is_wait(&self, _request: &super::Request, _scope: super::Scope) -> bool {
            false
        }
        fn subscribe(
            &self,
            _request: super::Request,
            _scope: super::Scope,
            _stream: aterm_uds::CtlStream,
        ) {
        }
        fn release(&self, _id: u64) {}
        fn has_live_claim(&self, _id: u64) -> bool {
            false
        }
    }

    let lanes = Lanes::new(LaneLimits {
        rpc: 1,
        wait: 1,
        inline: 1,
        open: 4,
        subscriptions: 1,
    });
    let started = lanes.start();
    let service = Arc::new(Service::default());
    lanes.publish(service.clone());
    started.activate();
    let (client, server) = aterm_uds::CtlStream::pair().expect("socket pair");
    assert!(lanes.admit(server).is_ok(), "admitted");
    client
        .set_read_timeout(Some(aterm_spec::wait::MUST_HAPPEN))
        .expect("hang detector");
    (&client)
        .write_all(b"AUTH ok\nsend ls\ntext\n")
        .expect("the requests");
    let mut reader = std::io::BufReader::new(&client);
    for expected in ["OK send ls\n", "OK text\n"] {
        let mut reply = String::new();
        reader.read_line(&mut reply).expect("a reply");
        assert_eq!(reply, expected);
    }
    assert!(
        service.replies.settle(aterm_spec::wait::MUST_HAPPEN),
        "delivered: nothing is owed"
    );
    assert_eq!(
        *service
            .seen
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner),
        [("send ls".to_string(), true), ("text".to_string(), false)]
    );
}

#[test]
fn a_relayed_input_request_owes_no_reply_here_and_a_read_takes_no_lock() {
    use std::sync::{Arc, mpsc};
    let store = crate::session_store::new_store();
    let hosted = crate::session_store::test_handle(5);
    let hosted_sid = hosted.sid.clone();
    store
        .write()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .register(hosted);
    let replies = super::input_replies_of(&store);
    let elsewhere = aterm_session::SessionId::generate();
    for (line, counted) in [
        ("send ls".to_string(), true),
        ("@. key enter".to_string(), true),
        (format!("@{} send ls", hosted_sid.as_str()), true),
        (format!("@{} send ls", elsewhere.as_str()), false),
        (format!("@{} paste hi", elsewhere.as_str()), false),
    ] {
        let guard = super::owed_input_reply(&store, &super::Request::parse(line.clone()));
        assert_eq!(guard.is_some(), counted, "{line:?}");
        assert_eq!(replies.owed(), counted, "{line:?}");
        drop(guard);
        assert!(!replies.owed(), "{line:?} delivered");
    }

    let (held_tx, held_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel::<()>();
    let holder = {
        let store = Arc::clone(&store);
        std::thread::spawn(move || {
            let _write = store
                .write()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let _ = held_tx.send(());
            let _ = release_rx.recv();
        })
    };
    held_rx
        .recv_timeout(aterm_spec::wait::MUST_HAPPEN)
        .expect("the store's write lock is held");
    let (answer_tx, answer_rx) = mpsc::channel();
    let asker = {
        let store = Arc::clone(&store);
        std::thread::spawn(move || {
            let version = super::Request::parse("version".to_string());
            let _ = answer_tx.send(super::owed_input_reply(&store, &version).is_none());
        })
    };
    let answered = answer_rx.recv_timeout(aterm_spec::wait::MUST_HAPPEN);
    let _ = release_tx.send(());
    holder.join().expect("holder thread");
    asker.join().expect("asker thread");
    assert_eq!(
        answered,
        Ok(true),
        "`version` was answered while the store's write lock was held"
    );
}
