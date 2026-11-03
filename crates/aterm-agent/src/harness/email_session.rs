// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
//! Bounded JSON Lines requests over one authenticated mail connection.

use super::*;
use std::io::{BufRead, BufReader, Read};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};

const MAX_REQUEST: usize = 64 * 1024;

trait Backend {
    fn folder(&self) -> &str;
    fn folders(&mut self, end: Instant) -> Result<Vec<Folder>, Failure>;
    fn list(&mut self, limit: usize, end: Instant) -> Result<Vec<Message>, Failure>;
    fn observe(
        &mut self,
        limit: usize,
        previous: &[Message],
        end: Instant,
    ) -> Result<Vec<Message>, Failure>;
    fn read(&mut self, id: &str, end: Instant) -> Result<Message, Failure>;
    fn select(&mut self, folder: &str, end: Instant) -> Result<(), Failure>;
    fn wait(&mut self, within: Duration, end: Instant) -> Result<bool, Failure>;
}
impl Backend for Connection {
    fn folder(&self) -> &str {
        &self.folder
    }
    fn folders(&mut self, end: Instant) -> Result<Vec<Folder>, Failure> {
        Connection::folders(self, end)
    }
    fn list(&mut self, limit: usize, end: Instant) -> Result<Vec<Message>, Failure> {
        Connection::messages(self, limit, end)
    }
    fn observe(
        &mut self,
        limit: usize,
        previous: &[Message],
        end: Instant,
    ) -> Result<Vec<Message>, Failure> {
        Connection::observe(self, limit, previous, end)
    }
    fn read(&mut self, id: &str, end: Instant) -> Result<Message, Failure> {
        Connection::read(self, id, end)
    }
    fn select(&mut self, folder: &str, end: Instant) -> Result<(), Failure> {
        Connection::select(self, folder, end)
    }
    fn wait(&mut self, within: Duration, end: Instant) -> Result<bool, Failure> {
        Connection::wait_for(self, within, end)
    }
}

#[derive(Debug, PartialEq, Eq)]
enum Action {
    Folders,
    List,
    Read(String),
    Select(String),
    Wait,
    Ping,
    Quit,
}
struct Request {
    id: Value,
    action: Action,
    limit: usize,
    within: Duration,
}

fn id(value: &Value) -> Result<Value, Failure> {
    match value.get("id") {
        None | Some(Value::Null) => Ok(Value::Null),
        Some(Value::String(text)) if text.len() <= 128 && !text.chars().any(char::is_control) => {
            Ok(Value::String(text.clone()))
        }
        Some(number) if number.as_u64().is_some() => Ok(number.clone()),
        _ => Err("Request id must be an unsigned integer or a short string".into()),
    }
}

fn parse(value: &Value, default_limit: usize) -> Result<Request, Failure> {
    let object = value
        .as_object()
        .ok_or("A session request must be an object")?;
    let id = id(value)?;
    let command = value["command"]
        .as_str()
        .ok_or("A session request needs a command")?;
    let text = |name: &str| -> Result<String, Failure> {
        value[name]
            .as_str()
            .filter(|text| {
                !text.is_empty() && text.len() <= 8192 && !text.chars().any(char::is_control)
            })
            .map(str::to_owned)
            .ok_or_else(|| Failure::from("Invalid message or folder id"))
    };
    let action = match command {
        "folders" => Action::Folders,
        "list" => Action::List,
        "read" => Action::Read(text("message_id")?),
        "select" => Action::Select(text("folder")?),
        "wait" => Action::Wait,
        "ping" => Action::Ping,
        "quit" => Action::Quit,
        _ => return Err("Unknown read-only mail session command".into()),
    };
    for key in object.keys() {
        let allowed = matches!(key.as_str(), "id" | "command" | "timeout")
            || key == "limit" && matches!(action, Action::List | Action::Wait)
            || key == "message_id" && matches!(action, Action::Read(_))
            || key == "folder" && matches!(action, Action::Select(_));
        if !allowed {
            return Err("Unexpected session request field".into());
        }
    }
    let integer = |key: &str, default: u64, max: u64| -> Result<u64, Failure> {
        match value.get(key) {
            None => Ok(default),
            Some(value) => value
                .as_u64()
                .filter(|n| (1..=max).contains(n))
                .ok_or_else(|| Failure::from("Session request limit or timeout is out of range")),
        }
    };
    Ok(Request {
        id,
        action,
        limit: integer("limit", default_limit as u64, 100)? as usize,
        within: Duration::from_secs(integer("timeout", 30, 30)?),
    })
}

fn input(
    input: impl Read + Send + 'static,
) -> Result<Receiver<Result<Vec<u8>, &'static str>>, Failure> {
    let (sender, receiver) = mpsc::sync_channel(8);
    let _input_worker = std::thread::Builder::new()
        .name("aterm-email-input".into())
        .spawn(move || {
            let mut reader = BufReader::new(input);
            loop {
                let mut line = Vec::new();
                match reader
                    .by_ref()
                    .take((MAX_REQUEST + 1) as u64)
                    .read_until(b'\n', &mut line)
                {
                    Ok(0) => break,
                    Ok(_) if line.len() > MAX_REQUEST => {
                        let _ = sender.send(Err("Session request exceeds 64 KiB"));
                        break;
                    }
                    Ok(_) => {
                        if sender.send(Ok(line)).is_err() {
                            break;
                        }
                    }
                    Err(_) => {
                        let _ = sender.send(Err("Could not read mail session input"));
                        break;
                    }
                }
            }
        })
        .map_err(|_| Failure::from("Could not start mail session input"))?;
    Ok(receiver)
}

fn error(output: &mut impl Write, id: Value, failure: &Failure) -> Result<(), Failure> {
    emit(
        output,
        json!({"schema":1,"type":"error","id":id,"message":failure.message,"retryable":failure.retry}),
    )
}
fn headers(messages: &[Message]) -> Vec<Value> {
    messages
        .iter()
        .map(|message| message_value(message, false))
        .collect()
}

fn execute(
    backend: &mut impl Backend,
    request: &Request,
    options: &mut Options,
    previous: &mut Option<(usize, Vec<Message>)>,
    overall: Instant,
) -> Result<Value, Failure> {
    let wait = request.action == Action::Wait;
    let end = overall.min(
        Instant::now()
            + request.within
            + if wait {
                Duration::from_secs(5)
            } else {
                Duration::ZERO
            },
    );
    remaining(end)?;
    match &request.action {
        Action::Folders => {
            let values: Vec<_> = backend
                .folders(end)?
                .into_iter()
                .map(|folder| json!({"id":folder.id,"name":folder.name}))
                .collect();
            Ok(json!({"schema":1,"type":"folders","id":request.id,"folders":values}))
        }
        Action::List => {
            let messages = backend.list(request.limit, end)?;
            let result = json!({"schema":1,"type":"snapshot","id":request.id,"folder":backend.folder(),"messages":headers(&messages)});
            *previous = Some((request.limit, messages));
            Ok(result)
        }
        Action::Read(id) => {
            let message = backend.read(id, end)?;
            Ok(
                json!({"schema":1,"type":"message","id":request.id,"folder":backend.folder(),"message":message_value(&message,true)}),
            )
        }
        Action::Select(folder) => {
            backend.select(folder, end)?;
            options.folder = Some(folder.clone());
            *previous = None;
            Ok(json!({"schema":1,"type":"selected","id":request.id,"folder":backend.folder()}))
        }
        Action::Wait => {
            let wait_end = overall.min(Instant::now() + request.within);
            let last = previous
                .as_ref()
                .filter(|(limit, _)| *limit == request.limit)
                .map_or(&[][..], |(_, messages)| messages.as_slice());
            let mut messages = backend.observe(request.limit, last, end)?;
            let mut changed = previous
                .as_ref()
                .is_none_or(|(limit, old)| *limit != request.limit || old != &messages);
            while !changed && Instant::now() < wait_end {
                let should_observe =
                    backend.wait(wait_end.saturating_duration_since(Instant::now()), end)?;
                if !should_observe || Instant::now() >= end {
                    if Instant::now() >= wait_end {
                        break;
                    }
                    continue;
                }
                messages = backend.observe(request.limit, &messages, end)?;
                changed = previous.as_ref().is_none_or(|(_, old)| old != &messages);
            }
            let result = json!({"schema":1,"type":"update","id":request.id,"folder":backend.folder(),"changed":changed,"messages":headers(&messages)});
            *previous = Some((request.limit, messages));
            Ok(result)
        }
        Action::Ping | Action::Quit => unreachable!(),
    }
}

fn drive<B: Backend>(
    initial: B,
    mut reopen: impl FnMut(&Options, Instant) -> Result<B, Failure>,
    mut options: Options,
    end: Instant,
    input_stream: impl Read + Send + 'static,
    output: &mut impl Write,
) -> Result<(), Failure> {
    let requests = input(input_stream)?;
    emit(
        output,
        json!({"schema":1,"type":"ready","account":options.account,"folder":initial.folder()}),
    )?;
    let mut backend = Some(initial);
    let mut previous = None;
    let mut permanent = None;
    loop {
        let left = end.saturating_duration_since(Instant::now());
        let line = if left.is_zero() {
            None
        } else {
            match requests.recv_timeout(left) {
                Ok(Ok(line)) => Some(line),
                Ok(Err(message)) => return Err(message.into()),
                Err(RecvTimeoutError::Disconnected) => {
                    emit(output, json!({"schema":1,"type":"complete","reason":"eof"}))?;
                    return Ok(());
                }
                Err(RecvTimeoutError::Timeout) => None,
            }
        };
        let Some(line) = line else {
            emit(
                output,
                json!({"schema":1,"type":"complete","reason":"timeout"}),
            )?;
            return Ok(());
        };
        let value: Value = match aterm_json::from_slice(&line) {
            Ok(value) => value,
            Err(_) => {
                error(output, Value::Null, &Failure::from("Invalid session JSON"))?;
                continue;
            }
        };
        let request = match parse(&value, options.limit) {
            Ok(request) => request,
            Err(failure) => {
                error(output, id(&value).unwrap_or(Value::Null), &failure)?;
                continue;
            }
        };
        if request.action == Action::Quit {
            emit(output, json!({"schema":1,"type":"goodbye","id":request.id}))?;
            return Ok(());
        }
        if request.action == Action::Ping {
            emit(output, json!({"schema":1,"type":"pong","id":request.id}))?;
            continue;
        }
        if let Some(failure) = &permanent {
            error(output, request.id, failure)?;
            continue;
        }
        if backend.is_none() {
            let connect_end = end.min(Instant::now() + request.within);
            match reopen(&options, connect_end) {
                Ok(connected) => backend = Some(connected),
                Err(failure) => {
                    error(output, request.id, &failure)?;
                    if !failure.retry {
                        permanent = Some(failure);
                    }
                    continue;
                }
            }
        }
        let result = execute(
            backend.as_mut().ok_or("No mail session connection")?,
            &request,
            &mut options,
            &mut previous,
            end,
        );
        match result {
            Ok(result) => emit(output, result)?,
            Err(failure) => {
                if failure.retry {
                    backend = None;
                    previous = None;
                }
                error(output, request.id, &failure)?;
            }
        }
    }
}

pub(super) fn run(
    initial: Connection,
    options: Options,
    home: &Path,
    end: Instant,
    input_stream: impl Read + Send + 'static,
    output: &mut impl Write,
) -> Result<(), Failure> {
    drive(
        initial,
        |options, end| Connection::open(home, options, end),
        options,
        end,
        input_stream,
        output,
    )
}

#[cfg(test)]
mod tests {
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
        fn observe(
            &mut self,
            _: usize,
            _: &[Message],
            _: Instant,
        ) -> Result<Vec<Message>, Failure> {
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
}
