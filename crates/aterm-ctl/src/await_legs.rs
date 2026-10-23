// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Multi-leg `await inbox|agent|edit` client loop ([`await_task::DEFAULT_TOTAL`]).

use std::io::{self, BufReader};
use std::process::ExitCode;
use std::time::{Duration, Instant};

use super::{
    EXIT_TIMEOUT, TargetOrigin, answer_replacement, await_task, connect_served, drain_refusal_tail,
    read_status_line, redial, replaced_before_reply, reply_is_timeout, request_line, send_served,
    stderr_line, validate_request_line_len,
};

/// A re-armed `[@<sel>] await inbox|agent|edit …`.
#[derive(Debug, PartialEq, Eq)]
struct Wait {
    parts: Vec<String>,
    timeout: Option<Duration>,
}

fn parse(parts: &[String]) -> Option<Wait> {
    let at = usize::from(parts.first().is_some_and(|p| p.starts_with('@')));
    if parts.get(at).map(String::as_str) != Some("await")
        || !matches!(
            parts.get(at + 1).map(String::as_str),
            Some("inbox" | "agent" | "edit")
        )
    {
        return None;
    }
    let mut kept = parts[..at + 2].to_vec();
    let mut timeout = None;
    let mut words = parts[at + 2..].iter();
    while let Some(word) = words.next() {
        let value = match word.strip_prefix("timeout=") {
            Some(value) => value,
            None if word == "timeout" => match words.next() {
                Some(value) => value.as_str(),
                None => {
                    kept.push(word.clone());
                    continue;
                }
            },
            None => {
                kept.push(word.clone());
                continue;
            }
        };
        if timeout.is_some() {
            return None;
        }
        timeout = Some(Duration::from_millis(value.parse::<u64>().ok()?));
    }
    Some(Wait {
        parts: kept,
        timeout,
    })
}

/// Whether `parts` is an `await inbox|agent|edit` this client re-arms ([`parse`]).
pub(crate) fn is_rearmed_await(parts: &[String]) -> bool {
    parse(parts).is_some()
}

/// How one leg ended.
#[derive(Debug)]
enum Leg {
    /// `OK timeout`: the leg's own deadline, nothing latched.
    TimedOut,
    /// Any other `OK` line: the answer.
    Answered(String),
    /// The server went away before it answered.
    HungUp {
        server: Option<u32>,
        listener: Option<redial::ListenerId>,
        error: Option<io::Error>,
    },
    /// The hosting `aterm` exited while relaying the leg.
    HostGone { pid: u32, sock: String },
    /// An exit already explained on stderr: a refusal, a connect phase.
    Code(ExitCode),
    /// An error this client cannot read past.
    Failed(io::Error),
}

type LegFn<'a> = dyn FnMut(&str, TargetOrigin, &str, Duration) -> Leg + 'a;

fn socket_leg(sock: &str, origin: TargetOrigin, request: &str, wait: Duration) -> Leg {
    let stream = match connect_served(sock, origin, Some(wait + await_task::LEG_SLACK)) {
        Ok(Ok(stream)) => stream,
        Ok(Err(code)) => return Leg::Code(code),
        Err(e) => return Leg::Failed(e),
    };
    let server = redial::server_pid(&stream);
    let listener = redial::listener_at(sock);
    match send_served(&stream, request.as_bytes()) {
        Ok(None) => {}
        Ok(Some(code)) => return Leg::Code(code),
        Err(e) if redial::is_hangup(&e) => {
            return Leg::HungUp {
                server,
                listener,
                error: Some(e),
            };
        }
        Err(e) => return Leg::Failed(e),
    }
    let mut reader = BufReader::new(&stream);
    let mut status = String::new();
    match read_status_line(&mut reader, &mut status) {
        Ok(0) => {
            return Leg::HungUp {
                server,
                listener,
                error: None,
            };
        }
        Ok(_) => {}
        Err(e) if redial::is_hangup(&e) => {
            return Leg::HungUp {
                server,
                listener,
                error: Some(e),
            };
        }
        Err(e) => return Leg::Failed(e),
    }
    let status = status.trim_end_matches(['\r', '\n']);
    if let Some((pid, sock)) = aterm_types::control_verbs::host_gone(status) {
        return Leg::HostGone {
            pid,
            sock: sock.to_string(),
        };
    }
    if reply_is_timeout(status) {
        return Leg::TimedOut;
    }
    if status.split_whitespace().next() == Some("OK") {
        return Leg::Answered(status.to_string());
    }
    match stderr_line(status).and_then(|()| drain_refusal_tail(&mut reader)) {
        Ok(()) => Leg::Code(ExitCode::FAILURE),
        Err(e) => Leg::Failed(e),
    }
}

fn leg_request(parts: &[String], wait: Duration) -> io::Result<String> {
    let mut parts = parts.to_vec();
    parts.push(format!("timeout={}", wait.as_millis()));
    let mut line = request_line(&parts)?;
    validate_request_line_len(&line, Some("await"))?;
    line.push('\n');
    Ok(line)
}

/// Runs the legs of `wait` against `sock` until `until`.
#[allow(
    clippy::too_many_arguments,
    reason = "the wait, where it is asked and how far it follows, its deadline, and the two \
              seams a test scripts; a wrapper struct would relocate the list, not simplify it"
)]
fn wait_legs(
    wait: &Wait,
    sock: &str,
    origin: TargetOrigin,
    until: Option<Instant>,
    follow: &redial::Redial,
    leg: &mut LegFn<'_>,
    out: &mut dyn FnMut(&str) -> io::Result<()>,
) -> io::Result<ExitCode> {
    loop {
        let left = until.map_or(await_task::LEG_MAX, |until| {
            until
                .saturating_duration_since(Instant::now())
                .min(await_task::LEG_MAX)
        });
        let request = leg_request(&wait.parts, left)?;
        match leg(sock, origin, &request, left) {
            Leg::TimedOut if until.is_none_or(|until| Instant::now() < until) => {}
            Leg::TimedOut => {
                out("OK timeout")?;
                return Ok(ExitCode::from(EXIT_TIMEOUT));
            }
            Leg::Answered(line) => {
                out(&line)?;
                return Ok(ExitCode::SUCCESS);
            }
            Leg::Code(code) => return Ok(code),
            Leg::Failed(e) => return Err(e),
            Leg::HungUp {
                server,
                listener,
                error,
            } => {
                let again = |next: &str| {
                    wait_legs(
                        wait,
                        next,
                        TargetOrigin::Pinned,
                        until,
                        &redial::Redial::none(),
                        &mut *leg,
                        &mut *out,
                    )
                };
                return replaced_before_reply(
                    sock,
                    server,
                    listener,
                    &wait.parts,
                    follow,
                    until,
                    again,
                    error,
                );
            }
            Leg::HostGone { pid, sock: host } => {
                let joined = wait.parts.join(" ");
                let sid = redial::target_sid(&joined);
                let policy = follow.host(&host, sid.map(|sid| sid.as_str()));
                let again = |next: &str| {
                    wait_legs(
                        wait,
                        next,
                        TargetOrigin::Pinned,
                        until,
                        &redial::Redial::none(),
                        &mut *leg,
                        &mut *out,
                    )
                };
                return answer_replacement(
                    policy.after_host_gone(&host, pid, until),
                    Some(pid),
                    &wait.parts,
                    &policy,
                    again,
                    None,
                );
            }
        }
    }
}

/// `[@<sel>] await inbox|agent|edit …`: the whole re-armed wait ([the module](self)).
pub(crate) fn run_rearmed_await(
    parts: &[String],
    sock: &str,
    origin: TargetOrigin,
    follow: &redial::Redial,
    total: Option<Option<Duration>>,
) -> io::Result<ExitCode> {
    let wait = parse(parts).ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "not an `await inbox|agent|edit` request",
        )
    })?;
    let total = match (wait.timeout, total) {
        (Some(verb), _) => Some(verb),
        (None, Some(flag)) => flag,
        (None, None) => Some(await_task::DEFAULT_TOTAL),
    };
    let until = total.map(|t| Instant::now() + t);
    wait_legs(
        &wait,
        sock,
        origin,
        until,
        follow,
        &mut socket_leg,
        &mut super::print_stdout_line,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parts(line: &str) -> Vec<String> {
        line.split_whitespace().map(String::from).collect()
    }

    #[test]
    fn the_inbox_and_agent_waits_are_parsed_with_their_timeout_taken_out() {
        assert_eq!(
            parse(&parts("@s-ab await inbox since=4 topic=lab timeout=900000")),
            Some(Wait {
                parts: parts("@s-ab await inbox since=4 topic=lab"),
                timeout: Some(Duration::from_secs(900)),
            })
        );
        assert_eq!(
            parse(&parts("await inbox timeout 5000 re=7")),
            Some(Wait {
                parts: parts("await inbox re=7"),
                timeout: Some(Duration::from_secs(5)),
            })
        );
        assert_eq!(
            parse(&parts("await inbox since=4")),
            Some(Wait {
                parts: parts("await inbox since=4"),
                timeout: None,
            })
        );
        assert_eq!(
            parse(&parts(
                "@s-ab await agent idle,prompt since=7 timeout=900000"
            )),
            Some(Wait {
                parts: parts("@s-ab await agent idle,prompt since=7"),
                timeout: Some(Duration::from_secs(900)),
            })
        );
        assert_eq!(
            parse(&parts("@s-ab await edit ticket=7 timeout=900000")),
            Some(Wait {
                parts: parts("@s-ab await edit ticket=7"),
                timeout: Some(Duration::from_secs(900)),
            })
        );
        assert_eq!(
            parse(&parts("await edit uri=file:///tmp/x")),
            Some(Wait {
                parts: parts("await edit uri=file:///tmp/x"),
                timeout: None,
            })
        );
        for refused in [
            "await idle 500",
            "await task @s-a",
            "await gone ready",
            "inbox since=4",
            "agent idle",
            "await inbox since=4 timeout=soon",
            "await inbox since=4 timeout=1 timeout=2",
        ] {
            assert_eq!(parse(&parts(refused)), None, "{refused}");
        }
    }

    struct Run {
        sent: Vec<(String, Duration)>,
        printed: Vec<String>,
        code: ExitCode,
    }

    fn run(wait: &Wait, until: Option<Instant>, script: Vec<Leg>) -> Run {
        let mut script = script.into_iter();
        let mut sent = Vec::new();
        let mut printed = Vec::new();
        let code = {
            let mut leg = |_: &str, _: TargetOrigin, request: &str, asked: Duration| {
                sent.push((request.to_string(), asked));
                let next = script.next().unwrap_or(Leg::TimedOut);
                if matches!(next, Leg::TimedOut) {
                    std::thread::sleep(asked);
                }
                next
            };
            let mut out = |line: &str| {
                printed.push(line.to_string());
                Ok(())
            };
            wait_legs(
                wait,
                "/no/such.sock",
                TargetOrigin::Pinned,
                until,
                &redial::Redial::none(),
                &mut leg,
                &mut out,
            )
            .expect("no transport error")
        };
        Run {
            sent,
            printed,
            code,
        }
    }

    #[test]
    fn a_wait_is_re_armed_with_the_same_since_until_a_leg_answers() {
        let wait = parse(&parts("@s-ab await inbox since=4 topic=lab")).expect("re-armed");
        let until = Some(Instant::now() + Duration::from_secs(3600));
        let mut sent = Vec::new();
        let mut printed = Vec::new();
        let mut script = vec![
            Leg::TimedOut,
            Leg::TimedOut,
            Leg::Answered("OK inbox 9".into()),
        ]
        .into_iter();
        let code = wait_legs(
            &wait,
            "/no/such.sock",
            TargetOrigin::Pinned,
            until,
            &redial::Redial::none(),
            &mut |_: &str, _: TargetOrigin, request: &str, asked: Duration| {
                sent.push((request.to_string(), asked));
                script.next().expect("no leg past the answer")
            },
            &mut |line: &str| {
                printed.push(line.to_string());
                Ok(())
            },
        )
        .expect("no transport error");
        assert_eq!(code, ExitCode::SUCCESS);
        assert_eq!(printed, ["OK inbox 9"]);
        assert_eq!(sent.len(), 3);
        for (request, asked) in &sent {
            assert_eq!(
                request,
                "@s-ab await inbox since=4 topic=lab timeout=600000\n"
            );
            assert_eq!(*asked, await_task::LEG_MAX);
        }
    }

    #[test]
    fn the_deadline_ends_the_wait_and_a_refusal_ends_it_at_once() {
        let wait = parse(&parts("await inbox since=4")).expect("re-armed");
        let short = Duration::from_millis(50);
        let got = run(&wait, Some(Instant::now() + short), Vec::new());
        assert_eq!(got.code, ExitCode::from(EXIT_TIMEOUT));
        assert_eq!(got.printed, ["OK timeout"]);
        assert_eq!(got.sent.len(), 1, "{:?}", got.sent);
        assert!(got.sent[0].1 <= short, "{:?}", got.sent);

        let got = run(&wait, None, vec![Leg::Code(ExitCode::FAILURE)]);
        assert_eq!((got.code, got.sent.len()), (ExitCode::FAILURE, 1));
        assert!(got.printed.is_empty());

        let mut leg = |_: &str, _: TargetOrigin, _: &str, _: Duration| Leg::HungUp {
            server: None,
            listener: None,
            error: None,
        };
        let cut = wait_legs(
            &wait,
            "/no/such.sock",
            TargetOrigin::Pinned,
            None,
            &redial::Redial::none(),
            &mut leg,
            &mut |_: &str| Ok(()),
        );
        assert_eq!(cut.map_err(|e| e.kind()), Err(io::ErrorKind::UnexpectedEof));
    }
}
