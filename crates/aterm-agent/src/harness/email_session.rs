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
    let mut retry: Option<Failure> = None;
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
            if let Some(failure) = &retry
                && failure.retry_at.is_some_and(|at| Instant::now() < at)
            {
                error(output, request.id, failure)?;
                continue;
            }
            retry = None;
            let connect_end = end.min(Instant::now() + request.within);
            match reopen(&options, connect_end) {
                Ok(connected) => backend = Some(connected),
                Err(failure) => {
                    error(output, request.id, &failure)?;
                    if !failure.retry {
                        permanent = Some(failure);
                    } else {
                        retry = Some(failure);
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
                    retry = Some(failure.clone());
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
#[path = "email_session_tests.rs"]
mod tests;
