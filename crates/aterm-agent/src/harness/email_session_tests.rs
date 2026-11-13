// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::*;
use std::io::Cursor;
use std::sync::{Arc, Mutex};

struct Fake {
    calls: Arc<Mutex<Vec<&'static str>>>,
    messages: Vec<Message>,
    fail: bool,
}
impl Backend for Fake {
    fn folder(&self) -> &str {
        "INBOX"
    }
    fn folders(&mut self, _: Instant) -> Result<Vec<Folder>, Failure> {
        Ok(Vec::new())
    }
    fn list(&mut self, _: usize, _: Instant) -> Result<Vec<Message>, Failure> {
        self.calls.lock().unwrap().push("list");
        if self.fail {
            return Err(transient("offline"));
        }
        Ok(self.messages.clone())
    }
    fn observe(&mut self, _: usize, _: &[Message], _: Instant) -> Result<Vec<Message>, Failure> {
        self.calls.lock().unwrap().push("observe");
        if self.fail {
            return Err(transient("offline"));
        }
        Ok(self.messages.clone())
    }
    fn read(&mut self, _: &str, _: Instant) -> Result<Message, Failure> {
        self.calls.lock().unwrap().push("read");
        Ok(message("one"))
    }
    fn select(&mut self, _: &str, _: Instant) -> Result<(), Failure> {
        Ok(())
    }
    fn wait(&mut self, _: Duration, _: Instant) -> Result<bool, Failure> {
        self.calls.lock().unwrap().push("wait");
        self.messages.push(message("new"));
        Ok(true)
    }
}
fn message(id: &str) -> Message {
    Message {
        id: id.into(),
        from: String::new(),
        to: String::new(),
        subject: id.into(),
        date: String::new(),
        unread: true,
        text: "body".into(),
    }
}
fn options() -> Options {
    Options {
        command: Command::Session,
        account: "a@example.test".into(),
        folder: None,
        limit: 10,
        timeout: Duration::from_secs(60),
    }
}
fn rows(bytes: &[u8]) -> Vec<Value> {
    bytes
        .split(|b| *b == b'\n')
        .filter(|b| !b.is_empty())
        .map(|b| aterm_json::from_slice(b).unwrap())
        .collect()
}

#[test]
fn one_connection_serves_correlated_reads_and_waits() {
    let calls = Arc::new(Mutex::new(Vec::new()));
    let fake = Fake {
        calls: calls.clone(),
        messages: vec![message("one")],
        fail: false,
    };
    let mut output = Vec::new();
    let requests = b"{\"id\":1,\"command\":\"list\"}\n{\"id\":2,\"command\":\"wait\",\"timeout\":1}\n{\"id\":3,\"command\":\"read\",\"message_id\":\"one\"}\n{\"id\":4,\"command\":\"quit\"}\n";
    assert!(
        drive(
            fake,
            |_, _| panic!("healthy connection must be reused"),
            options(),
            Instant::now() + Duration::from_secs(60),
            Cursor::new(requests.to_vec()),
            &mut output
        )
        .is_ok()
    );
    let rows = rows(&output);
    assert_eq!(
        rows.iter()
            .filter_map(|v| v["type"].as_str())
            .collect::<Vec<_>>(),
        ["ready", "snapshot", "update", "message", "goodbye"]
    );
    assert_eq!(rows[2]["id"].as_u64(), Some(2));
    assert_eq!(rows[2]["changed"].as_bool(), Some(true));
    assert_eq!(rows[2]["messages"].as_array().unwrap().len(), 2);
    assert_eq!(
        calls
            .lock()
            .unwrap()
            .iter()
            .filter(|call| **call == "wait")
            .count(),
        1
    );
    assert_eq!(
        calls
            .lock()
            .unwrap()
            .iter()
            .filter(|call| **call == "list")
            .count(),
        1
    );
    assert_eq!(
        calls
            .lock()
            .unwrap()
            .iter()
            .filter(|call| **call == "observe")
            .count(),
        2
    );
}
#[test]
fn a_failed_read_reconnects_on_the_next_request() {
    let calls = Arc::new(Mutex::new(Vec::new()));
    let fake = Fake {
        calls: calls.clone(),
        messages: Vec::new(),
        fail: true,
    };
    let mut opens = 0;
    let mut output = Vec::new();
    let requests = b"{\"id\":1,\"command\":\"list\"}\n{\"id\":2,\"command\":\"list\"}\n";
    assert!(
        drive(
            fake,
            |_, _| {
                opens += 1;
                Ok(Fake {
                    calls: calls.clone(),
                    messages: Vec::new(),
                    fail: false,
                })
            },
            options(),
            Instant::now() + Duration::from_secs(60),
            Cursor::new(requests.to_vec()),
            &mut output
        )
        .is_ok()
    );
    assert_eq!(opens, 1);
    let rows = rows(&output);
    assert_eq!(rows[1]["retryable"].as_bool(), Some(true));
    assert_eq!(rows[2]["id"].as_u64(), Some(2));
    assert_eq!(rows.last().unwrap()["reason"].as_str(), Some("eof"));
}
#[test]
fn rejected_reconnection_is_not_repeated_for_later_requests() {
    let calls = Arc::new(Mutex::new(Vec::new()));
    let fake = Fake {
        calls,
        messages: Vec::new(),
        fail: true,
    };
    let mut opens = 0;
    let mut output = Vec::new();
    let requests = b"{\"command\":\"list\"}\n{\"command\":\"list\"}\n{\"command\":\"list\"}\n";
    assert!(
        drive(
            fake,
            |_, _| {
                opens += 1;
                Err("authorization refused".into())
            },
            options(),
            Instant::now() + Duration::from_secs(60),
            Cursor::new(requests.to_vec()),
            &mut output
        )
        .is_ok()
    );
    assert_eq!(opens, 1);
    let rows = rows(&output);
    assert_eq!(rows[2]["retryable"].as_bool(), Some(false));
    assert_eq!(rows[3]["retryable"].as_bool(), Some(false));
}

#[test]
fn a_session_preserves_the_retry_deadline_across_connection_replacement() {
    let calls = Arc::new(Mutex::new(Vec::new()));
    let fake = Fake {
        calls: Arc::clone(&calls),
        messages: Vec::new(),
        fail: true,
    };
    let mut opens = 0;
    let mut output = Vec::new();
    drive(
        fake,
        |_, _| {
            opens += 1;
            Err(Failure {
                message: "owned provider wait".into(),
                retry: true,
                retry_at: Some(Instant::now() + Duration::from_secs(3600)),
            })
        },
        options(),
        Instant::now() + Duration::from_secs(60),
        Cursor::new(b"{\"id\":1,\"command\":\"list\"}\n{\"id\":2,\"command\":\"list\"}\n{\"id\":3,\"command\":\"list\"}\n{\"id\":4,\"command\":\"ping\"}\n".to_vec()),
        &mut output,
    ).unwrap();
    assert_eq!(
        opens, 1,
        "the next request cannot reconnect before the provider deadline"
    );
    assert_eq!(*calls.lock().unwrap(), ["list"]);
    let rows = rows(&output);
    assert_eq!(rows[2]["message"].as_str(), Some("owned provider wait"));
    assert_eq!(rows[3]["message"].as_str(), Some("owned provider wait"));
    assert_eq!(rows[3]["retryable"].as_bool(), Some(true));
    assert_eq!(rows[4]["type"].as_str(), Some("pong"));
}

#[test]
fn malformed_requests_do_not_execute_and_oversized_input_stops() {
    let calls = Arc::new(Mutex::new(Vec::new()));
    let fake = Fake {
        calls: calls.clone(),
        messages: Vec::new(),
        fail: false,
    };
    let mut output = Vec::new();
    assert!(
        drive(
            fake,
            |_, _| panic!("no reconnect"),
            options(),
            Instant::now() + Duration::from_secs(60),
            Cursor::new(
                b"not json\n{\"id\":1,\"command\":\"send\"}\n{\"id\":2,\"command\":\"ping\"}\n"
                    .to_vec()
            ),
            &mut output
        )
        .is_ok()
    );
    assert!(calls.lock().unwrap().is_empty());
    let rows = rows(&output);
    assert_eq!(rows[3]["type"].as_str(), Some("pong"));
    let receive = input(Cursor::new(vec![b'x'; MAX_REQUEST + 1]))
        .ok()
        .unwrap();
    assert!(
        receive
            .recv_timeout(Duration::from_secs(60))
            .unwrap()
            .is_err()
    );
}
#[test]
fn request_limits_and_fields_are_validated() {
    for value in [
        json!({"command":"read","message_id":""}),
        json!({"command":"list","limit":101}),
        json!({"command":"wait","timeout":31}),
        json!({"command":"ping","password":"private"}),
        json!({"command":"list","id":{}}),
    ] {
        assert!(parse(&value, 10).is_err());
    }
}

#[test]
fn wait_observes_after_full_poll_slice_and_skips_quiet_idle_timeout() {
    struct PollSliceBackend {
        observes: usize,
        waits: usize,
        notified: bool,
        messages: Vec<Message>,
    }
    impl Backend for PollSliceBackend {
        fn folder(&self) -> &str {
            "INBOX"
        }
        fn folders(&mut self, _: Instant) -> Result<Vec<Folder>, Failure> {
            Ok(Vec::new())
        }
        fn list(&mut self, _: usize, _: Instant) -> Result<Vec<Message>, Failure> {
            Ok(self.messages.clone())
        }
        fn observe(
            &mut self,
            _: usize,
            _: &[Message],
            _: Instant,
        ) -> Result<Vec<Message>, Failure> {
            self.observes += 1;
            Ok(self.messages.clone())
        }
        fn read(&mut self, _: &str, _: Instant) -> Result<Message, Failure> {
            Ok(message("one"))
        }
        fn select(&mut self, _: &str, _: Instant) -> Result<(), Failure> {
            Ok(())
        }
        fn wait(&mut self, within: Duration, _: Instant) -> Result<bool, Failure> {
            self.waits += 1;
            std::thread::sleep(within + Duration::from_millis(2));
            if self.notified {
                self.messages.push(message("arrived"));
            }
            Ok(self.notified)
        }
    }

    let mut opts = options();
    let req = Request {
        id: json!(1),
        action: Action::Wait,
        limit: 10,
        within: Duration::from_millis(500),
    };

    // 1. When `wait` consumes the full `within` slice (`Instant::now() >= wait_end`) and
    // returns `Ok(true)` (Proton / non-IDLE poll or late IDLE notification), `execute` still
    // runs the post-wait `observe` within the 5s `end` slack and reports the new message.
    let mut notified_backend = PollSliceBackend {
        observes: 0,
        waits: 0,
        notified: true,
        messages: vec![message("one")],
    };
    let mut previous = Some((10, vec![message("one")]));
    let reply = execute(
        &mut notified_backend,
        &req,
        &mut opts,
        &mut previous,
        Instant::now() + Duration::from_secs(60),
    )
    .unwrap();
    assert_eq!(notified_backend.waits, 1);
    assert_eq!(reply["changed"].as_bool(), Some(true));
    assert_eq!(notified_backend.observes, 2);

    // 2. Negative control: when an IDLE wait expires quietly (`Ok(false)`), no redundant
    // post-wait `observe` is issued.
    let mut quiet_backend = PollSliceBackend {
        observes: 0,
        waits: 0,
        notified: false,
        messages: vec![message("one")],
    };
    let mut quiet_prev = Some((10, vec![message("one")]));
    let quiet_reply = execute(
        &mut quiet_backend,
        &req,
        &mut opts,
        &mut quiet_prev,
        Instant::now() + Duration::from_secs(60),
    )
    .unwrap();
    assert_eq!(quiet_backend.waits, 1);
    assert_eq!(quiet_reply["changed"].as_bool(), Some(false));
    assert_eq!(quiet_backend.observes, 1);
}
