// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `aterm ctl [@<sel>] await inbox|agent|edit …`: the inbox, agent and edit
//! waits, RE-ARMED past the server's bound
//! (`docs/DESIGN-sessions-as-subagents-2026-10-15.md` §4; `await edit`,
//! `docs/DESIGN-amacs-2026-10-19.md` §6.4).
//!
//! The server parks one `await` for its `timeout=` — 30 s when none is given,
//! at most 600 s — and answers `OK timeout` at its end. A session that listens
//! on a channel, or waits out a peer's turn, runs the wait as a background
//! command whose EXIT wakes it, so a wait that ended at the server's bound
//! would wake it for no news. This client sends the request in LEGS, every
//! other word as given (`await inbox`'s `since=`, `re=`, `kinds=`, `topic=`;
//! `await agent`'s words and `since=`: each predicate latches only on what is
//! past `since`, else on what stands; `await edit`'s `ticket=` or `uri=`
//! latches on the edit's one outcome, so each leg waits for the same answer),
//! each asking for what the whole wait has left, up to the server's bound,
//! until a leg answers or the whole wait's deadline passes: the verb's
//! `timeout=<ms>` (or `timeout <ms>`), else `--timeout`, else an hour
//! ([`await_task::DEFAULT_TOTAL`]), as `await task` does. The answering leg's
//! line is printed as one exchange prints it (`OK inbox <id>`, `OK inbox
//! hold=<0|1>`, `OK agent <word> rev=<n>`, `OK edit done …`) on stdout, exit
//! 0; an `ERR …` on
//! stderr, exit 1; `OK timeout` on stdout, exit 124, at the deadline. A leg
//! whose server hung up before it answered is judged as any blocking read's
//! ([`replaced_before_reply`]): a successor is asked for the rest of the wait
//! only where the request names nothing of the old process.

use std::io::{self, BufReader};
use std::process::ExitCode;
use std::time::{Duration, Instant};

use super::{
    EXIT_TIMEOUT, TargetOrigin, answer_replacement, await_task, connect_served, drain_refusal_tail,
    read_status_line, redial, replaced_before_reply, reply_is_timeout, request_line, send_served,
    stderr_line, validate_request_line_len,
};

/// A re-armed `[@<sel>] await inbox|agent|edit …`: the request every leg sends,
/// its timeout words taken out, and the whole wait the verb's own timeout
/// names.
#[derive(Debug, PartialEq, Eq)]
struct Wait {
    parts: Vec<String>,
    timeout: Option<Duration>,
}

/// `parts` (`@self` expanded) as a re-armed wait; `None` for any other
/// request, and for one whose timeout word names no milliseconds or comes
/// twice — the server's to refuse, in one exchange.
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

/// Whether `parts` is an `await inbox|agent|edit` this client re-arms
/// ([`parse`]).
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
    /// The aterm hosting the session went away under the forward that relayed
    /// the leg (its relay said so in place of a reply).
    HostGone { pid: u32, sock: String },
    /// An exit already explained on stderr: a refusal, a connect phase.
    Code(ExitCode),
    /// An error this client cannot read past.
    Failed(io::Error),
}

/// One leg's round trip: `(sock, origin, request line, the wait it asks for)`.
type LegFn<'a> = dyn FnMut(&str, TargetOrigin, &str, Duration) -> Leg + 'a;

/// Send one leg to `sock` and read its line; an `ERR` is printed as one
/// exchange prints it (the line and its tail on stderr).
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

/// The request line one leg sends: the wait's words and the leg's own
/// `timeout=`.
fn leg_request(parts: &[String], wait: Duration) -> io::Result<String> {
    let mut parts = parts.to_vec();
    parts.push(format!("timeout={}", wait.as_millis()));
    let mut line = request_line(&parts)?;
    validate_request_line_len(&line, Some("await"))?;
    line.push('\n');
    Ok(line)
}

/// The legs of `wait` against `sock`, to `until` (`None`: no deadline): the
/// answering leg's line, or `OK timeout` at the deadline, written through
/// `out`; a hang-up judged as a blocking read's, its successor (where it is
/// asked) waited on for what is left, following nothing further.
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

/// `[@<sel>] await inbox|agent|edit …`: the whole re-armed wait ([the
/// module](self)). `sock` is the socket the call resolves to and `origin`
/// how, `total` the explicit `--timeout` when one was given (`Some(None)`:
/// `--timeout 0`, no deadline).
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

    /// The re-armed forms: `await inbox`, `await agent` and `await edit`, a leading
    /// selector or none, the timeout as `timeout=<ms>` or `timeout <ms>`
    /// taken out of the words every leg sends, the rest kept as given.
    /// NEGATIVE CONTROLS: another `await`, a timeout that is no number, and
    /// one given twice are not re-armed (one exchange, the server's to
    /// answer).
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
        // An edit's wait, by ticket or by file (the `amacs` client runs its
        // own legs, with no total limit: `crate::edit`).
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

    /// What a scripted run sent and printed.
    struct Run {
        sent: Vec<(String, Duration)>,
        printed: Vec<String>,
        code: ExitCode,
    }

    /// `wait` run to `until` against scripted legs: each answers the next of
    /// `script` after sleeping the wait it asked for where it times out (as
    /// the server parks), and times out once the script is spent.
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

    /// RE-ARMED UNTIL NEWS: two legs time out at the server's bound and the
    /// third answers — every leg the same words, `since=` unchanged, each
    /// asking for the server's bound while the hour lasts; the answer is
    /// printed once, exit 0, and no `OK timeout` is ever printed before it.
    #[test]
    fn a_wait_is_re_armed_with_the_same_since_until_a_leg_answers() {
        let wait = parse(&parts("@s-ab await inbox since=4 topic=lab")).expect("re-armed");
        let until = Some(Instant::now() + Duration::from_secs(3600));
        // These legs time out at once: one sleeping the server's bound would
        // hold the test 20 minutes.
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

    /// AT THE WHOLE WAIT'S DEADLINE, `OK timeout` and exit 124: a wait of
    /// 50 ms asks its one leg for no more than that, and prints the timeout
    /// once. NEGATIVE CONTROLS: a refusal ends the run at its leg, with no
    /// line printed here (the leg said it on stderr); a hang-up with no
    /// successor to follow is the original error.
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
