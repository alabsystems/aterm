// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE ADAPTER — the one unwritten seam `mod.rs` named, written.
//!
//! [`watch::Watcher`] decides and [`observe::Observer`] reads, both behind
//! traits, and until now nothing in the tree implemented those traits against
//! a real aterm: the loop, its `Wire` and `Watcher::pump` were authored and
//! unreached. [`CtlWire`] is the implementation. It holds TWO control
//! connections to one instance and nothing else:
//!
//! * the STREAM connection is parked on `subscribe @<sid> events` for its
//!   whole life, read by one thread that forwards every pushed line into a
//!   channel. That is the ingress, and it is pushed — the loop learns a turn
//!   began because the server said so, not because a clock came round;
//! * the REQUEST connection carries `status`, the revision-gated grid read,
//!   the guard reads and every act. One line, one reply.
//!
//! **There is no sleep and no cadence in this file.** A deadline is an
//! `await … timeout=<ms>` ([`watch::Arm::line`], already clamped to the
//! server's own [`watch::AWAIT_CLAMP_MS`]) issued on a THIRD, short-lived
//! connection, so the park returns on whichever of the two answers first.
//! When the stream wins, the armed connection is SHUT DOWN — the wait is
//! cancelled, not abandoned — and the thread joined before the next park
//! arms again. `park` blocks in `Receiver::recv`, which is not a poll: it
//! wakes when a sender sends and never on a timer.
//!
//! The one place a clock is read at all is [`Wire::now_ms`] / [`Wire::now_unix`],
//! which is what the trait is FOR: every decision above this file takes its
//! instants as arguments.
//!
//! GENERATION. aterm publishes no per-session generation counter, so this
//! module derives one from the fact that does identify a launch: the public
//! launch nonce on the session's `sessions` roster row (`nonce=<hex32>`,
//! MEASURED — the same field `turn id=<epoch>:…` is keyed by). The counter
//! starts at 1 and steps when a re-read shows a DIFFERENT nonce, which is a
//! relaunch and nothing else. A nonce that cannot be read leaves the
//! generation where it was and leaves [`watch::WatchConfig::nonce`] empty, so
//! every typed act refuses `unresolved` rather than printing a line the
//! server rejects at parse.
//!
//! CREDENTIALS. The only secret this file touches is the instance's own
//! control token, read from beside the socket by `aterm-ctl`'s own reader and
//! written to the socket and nowhere else. No vendor credential, keychain
//! item or account token is read, here or anywhere under `harness`.

use std::path::PathBuf;
use std::sync::mpsc::{Receiver, Sender, channel};
use std::thread::JoinHandle;

use aterm_uds::CtlStream;

use super::observe;
use super::watch::{self, Arm, HostGuards, Timer, Wake};
use crate::RelayClient;
use crate::supervise::CtlReply;

/// The streams the parked connection subscribes to. `events` is the
/// per-target digest — `EVENT <local> turn|block-complete|meta|title|bell …`
/// — which is exactly [`watch::Frame`]'s vocabulary. `screen` is NOT
/// subscribed: a DELTA per frame would push a read on every spinner tick,
/// and the revision gate on `status` is what makes the grid read cheap.
pub const STREAMS: &str = "events";

/// What the reader thread and the await workers put on the one channel.
enum Msg {
    /// A line the server pushed on the stream connection.
    Frame(String),
    /// The stream connection ended (the instance went away, or the session
    /// was retired). Sent exactly once.
    StreamEnded,
    /// An armed `await` answered. `epoch` is the arm generation, so a reply
    /// from a wait that was already cancelled is DROPPED rather than folded
    /// in late.
    Armed {
        epoch: u64,
        timer: Timer,
        reply: Result<String, String>,
    },
}

/// One armed wait in flight.
struct Armed {
    epoch: u64,
    arm: Arm,
    /// A second handle on the armed connection, held so the wait can be
    /// CANCELLED (`shutdown`) when the stream answers first.
    cancel: CtlStream,
    join: JoinHandle<()>,
}

/// Everything the wire needs from outside itself, injected.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WireConfig {
    /// The control socket path.
    pub sock: String,
    /// The instance's control token.
    pub token: String,
    /// The session to watch. EMPTY means the connection's own session, and
    /// then no `@selector` is sent at all.
    pub sid: String,
    /// The harness state directory the ledgers live in.
    pub state: PathBuf,
    /// The mark is not `bypassed` — the master switch and `$ATERM_NO_HARNESS`
    /// as the caller read them. This file re-reads neither.
    pub engaged: bool,
}

/// A live connection pair to one aterm instance, implementing the harness's
/// two seams.
pub struct CtlWire {
    cfg: WireConfig,
    req: RelayClient<CtlStream>,
    rx: Receiver<Msg>,
    tx: Sender<Msg>,
    stream: Option<JoinHandle<()>>,
    stream_alive: bool,
    armed: Option<Armed>,
    epoch: u64,
    /// The derived generation (see the module docs).
    generation: u64,
    /// The launch nonce the generation is derived from; EMPTY when the
    /// roster could not be read.
    nonce: String,
    /// The first hard error, kept so a caller can say WHY the loop stopped.
    failure: Option<String>,
}

impl CtlWire {
    /// Open both connections and park the stream one.
    ///
    /// # Errors
    ///
    /// The socket could not be reached, the token was refused, or the
    /// `subscribe` was not accepted. All three are reported rather than
    /// retried: a harness that cannot watch says so instead of looping.
    pub fn connect(cfg: WireConfig) -> Result<CtlWire, String> {
        let mut req = RelayClient::connect_local(&cfg.sock, &cfg.token)
            .map_err(|e| format!("{}: {e}", cfg.sock))?;
        // Prove the connection before anything is parked on a second one: an
        // auth failure is silent on this protocol until the first verb.
        let probe = req
            .request_line("version")
            .map_err(|e| format!("{}: {e}", cfg.sock))?;
        if !probe.starts_with("OK") {
            return Err(format!("{}: {probe}", cfg.sock));
        }
        let (tx, rx) = channel();
        let mut wire = CtlWire {
            req,
            rx,
            tx,
            stream: None,
            stream_alive: false,
            armed: None,
            epoch: 0,
            generation: 1,
            nonce: String::new(),
            failure: None,
            cfg,
        };
        wire.start_stream()?;
        wire.nonce = wire.read_nonce();
        Ok(wire)
    }

    /// The launch nonce the generation is keyed by, EMPTY when unread.
    #[must_use]
    pub fn nonce(&self) -> &str {
        &self.nonce
    }

    /// The first hard failure, where one happened.
    #[must_use]
    pub fn failure(&self) -> Option<&str> {
        self.failure.as_deref()
    }

    /// Whether the pushed stream is still open.
    #[must_use]
    pub fn stream_alive(&self) -> bool {
        self.stream_alive
    }

    /// `@<sid> ` when a session was named, else nothing.
    fn selector(&self) -> String {
        if self.cfg.sid.is_empty() {
            String::new()
        } else {
            format!("@{} ", self.cfg.sid)
        }
    }

    /// Park a second connection on `subscribe … events`, read by one thread.
    fn start_stream(&mut self) -> Result<(), String> {
        let sel = if self.cfg.sid.is_empty() {
            "@.".to_string()
        } else {
            format!("@{}", self.cfg.sid)
        };
        let mut stream = RelayClient::connect_local(&self.cfg.sock, &self.cfg.token)
            .map_err(|e| format!("{}: {e}", self.cfg.sock))?;
        let ack = stream
            .request_line(&format!("subscribe {sel} {STREAMS}"))
            .map_err(|e| format!("{}: {e}", self.cfg.sock))?;
        if !ack.starts_with("OK") {
            return Err(format!("subscribe {sel} {STREAMS}: {ack}"));
        }
        let tx = self.tx.clone();
        self.stream_alive = true;
        self.stream = Some(std::thread::spawn(move || {
            loop {
                match stream.next_pushed() {
                    Ok(line) => {
                        if tx.send(Msg::Frame(line)).is_err() {
                            return;
                        }
                    }
                    Err(_) => {
                        let _ = tx.send(Msg::StreamEnded);
                        return;
                    }
                }
            }
        }));
        Ok(())
    }

    /// Arm one bounded `await` on its own connection.
    fn arm(&mut self, arm: &Arm) {
        let mut client = match RelayClient::connect_local(&self.cfg.sock, &self.cfg.token) {
            Ok(client) => client,
            Err(e) => {
                // Nothing is armed. The park then waits on the stream alone,
                // which is a degradation (this deadline will not fire) and is
                // reported as such rather than replaced with a sleep.
                self.note(format!(
                    "{}: could not arm {}: {e}",
                    self.cfg.sock,
                    arm.line()
                ));
                return;
            }
        };
        // The cancel handle is a SECOND descriptor on the same socket, so a
        // `shutdown` through it unblocks the read the worker is parked in.
        let cancel = match client.transport().try_clone() {
            Ok(cancel) => cancel,
            Err(e) => {
                self.note(format!(
                    "the armed connection could not be cloned, so the wait would not be                      cancellable: {e}"
                ));
                return;
            }
        };
        self.epoch = self.epoch.saturating_add(1);
        let epoch = self.epoch;
        let timer = arm.timer;
        let line = format!("{}{}", self.selector(), arm.line());
        let tx = self.tx.clone();
        let join = std::thread::spawn(move || {
            let reply = client
                .request_line(&line)
                .map_err(|e| e.to_string())
                .and_then(|r| if r.starts_with("OK") { Ok(r) } else { Err(r) });
            let _ = tx.send(Msg::Armed {
                epoch,
                timer,
                reply,
            });
        });
        self.armed = Some(Armed {
            epoch,
            arm: arm.clone(),
            cancel,
            join,
        });
    }

    /// Cancel the armed wait, if any: shut its socket down so the blocked
    /// read returns, then JOIN the thread. The reply, if one was already on
    /// the channel, is dropped by its epoch on the next pass.
    fn cancel_armed(&mut self) {
        let Some(armed) = self.armed.take() else {
            return;
        };
        let _ = armed.cancel.shutdown(std::net::Shutdown::Both);
        let _ = armed.join.join();
    }

    fn note(&mut self, why: String) {
        if self.failure.is_none() {
            self.failure = Some(why);
        }
    }

    /// One request on the REQUEST connection, as a [`CtlReply`].
    fn call(&mut self, verb: &str) -> CtlReply {
        let line = format!("{}{verb}", self.selector());
        match self.req.request_line(&line) {
            Ok(reply) if reply.starts_with("OK") => CtlReply {
                code: 0,
                stdout: reply,
                stderr: String::new(),
            },
            Ok(reply) => CtlReply {
                code: 1,
                stdout: String::new(),
                stderr: reply,
            },
            Err(e) => {
                self.note(format!("{line}: {e}"));
                CtlReply {
                    code: 1,
                    stdout: String::new(),
                    stderr: format!("ERR {e}"),
                }
            }
        }
    }

    /// One COUNTED request (`Lines` framing) on the request connection.
    fn call_lines(&mut self, verb: &str) -> CtlReply {
        let line = format!("{}{verb}", self.selector());
        match self.req.request_counted(&line) {
            Ok((header, body)) if header.starts_with("OK") => CtlReply {
                code: 0,
                stdout: body,
                stderr: String::new(),
            },
            Ok((header, _)) => CtlReply {
                code: 1,
                stdout: String::new(),
                stderr: header,
            },
            Err(e) => {
                self.note(format!("{line}: {e}"));
                CtlReply {
                    code: 1,
                    stdout: String::new(),
                    stderr: format!("ERR {e}"),
                }
            }
        }
    }

    /// This session's `nonce=<hex32>` off the `sessions` roster, or EMPTY.
    ///
    /// `sessions` is Owner-only and instance-wide, so it is asked WITHOUT a
    /// selector and the row is matched by sid. With no sid named, the
    /// connection's own session is asked for by `whoami` first.
    fn read_nonce(&mut self) -> String {
        let sid = if self.cfg.sid.is_empty() {
            let who = self.req.request_line("whoami").unwrap_or_default();
            field_of(&who, "sid").unwrap_or_default()
        } else {
            self.cfg.sid.clone()
        };
        if sid.is_empty() {
            return String::new();
        }
        let Ok((header, body)) = self.req.request_counted("sessions") else {
            return String::new();
        };
        if !header.starts_with("OK") {
            return String::new();
        }
        for row in body.lines() {
            if row.split_whitespace().any(|t| t == sid)
                && let Some(nonce) = field_of(row, "nonce")
                && watch::valid_turn_nonce(&nonce)
            {
                return nonce;
            }
        }
        String::new()
    }

    /// Re-read the launch nonce and step the generation when it MOVED.
    ///
    /// A nonce that reads empty (the roster could not be asked) leaves both
    /// where they were: "I could not look" is not "it changed", and treating
    /// it as a change would drop every pending decision on a transient read
    /// failure.
    fn refresh_generation(&mut self) {
        let now = self.read_nonce();
        if now.is_empty() || now == self.nonce {
            return;
        }
        if !self.nonce.is_empty() {
            self.generation = self.generation.saturating_add(1);
        }
        self.nonce = now;
    }
}

impl Drop for CtlWire {
    fn drop(&mut self) {
        self.cancel_armed();
        // The reader thread is woken by the request connection's peer going
        // away when the process exits; it is detached here on purpose rather
        // than joined, because a `subscribe` that is quiet has no deadline of
        // its own and joining would block a Drop.
        self.stream = None;
    }
}

/// Does a `custody` reply say a PERSON owns this screen right now?
///
/// READING **OR SELECTING**, which is what the fence has always meant and was
/// only half of what it read. `owner=user` is `cmd_custody`'s
/// `display_offset > 0` — scrolled back — and the same reply carries
/// `selection=yes|no`; a person holding a live selection AT THE TAIL is as
/// much in custody of the keyboard as one who scrolled away, and that half
/// was read by nothing, so an L3 typed act could land under a live selection.
///
/// The `changed=` conjunct that used to ride beside `owner=` is GONE.
/// `cmd_custody` always prints `changed=<transition>` or the literal
/// `changed=none`, and [`field_of`] filters only `-` and the empty string, so
/// the test could never be false when the first held: false precision reading
/// as a second fence is worse than one fence stated plainly.
fn custody_is_the_person(reply: &str) -> bool {
    field_of(reply, "owner").as_deref() == Some("user")
        || field_of(reply, "selection").as_deref() == Some("yes")
}

/// `key=value` out of a control reply line.
fn field_of(line: &str, key: &str) -> Option<String> {
    line.split_whitespace()
        .find_map(|t| t.strip_prefix(key)?.strip_prefix('='))
        .filter(|v| *v != "-" && !v.is_empty())
        .map(str::to_owned)
}

impl observe::Introspect for CtlWire {
    fn status(&mut self) -> CtlReply {
        self.call("status")
    }

    fn text_json_tail(&mut self, rows: usize) -> CtlReply {
        self.call_lines(&format!("text --json trim tail={rows}"))
    }

    fn offscreen(&mut self, since: u64, max: usize) -> CtlReply {
        self.call_lines(&format!("offscreen since={since} max={max}"))
    }

    fn search(&mut self, pattern: &str) -> CtlReply {
        self.call_lines(&format!("search {pattern}"))
    }
}

impl watch::Wire for CtlWire {
    fn park(&mut self, arm: Option<&Arm>) -> Wake {
        match arm {
            Some(want) => {
                if self.armed.as_ref().map(|a| &a.arm) != Some(want) {
                    self.cancel_armed();
                    self.arm(want);
                }
            }
            None => self.cancel_armed(),
        }
        loop {
            if !self.stream_alive && self.armed.is_none() {
                // Nothing can ever send again; blocking would hang the loop.
                return Wake::Closed;
            }
            let Ok(msg) = self.rx.recv() else {
                return Wake::Closed;
            };
            match msg {
                Msg::StreamEnded => {
                    self.stream_alive = false;
                    self.cancel_armed();
                    return Wake::Closed;
                }
                Msg::Frame(line) => {
                    let Some(frame) = watch::parse_frame(&line) else {
                        // `BYTES`, a body row, a blank line: not a frame this
                        // loop acts on. Keep parking rather than waking the
                        // decision pass on noise.
                        continue;
                    };
                    self.cancel_armed();
                    return Wake::Frame(frame);
                }
                Msg::Armed {
                    epoch,
                    timer,
                    reply,
                } => {
                    if self.armed.as_ref().map(|a| a.epoch) != Some(epoch) {
                        // A cancelled wait answering late.
                        continue;
                    }
                    self.armed = None;
                    return match reply {
                        // `OK timeout` is the deadline ARRIVING; for a stall
                        // wait that IS the verdict (design §5.4).
                        Ok(r) if r.contains("timeout") => Wake::Deadline(timer),
                        Ok(_) => Wake::Latched(timer),
                        Err(e) => {
                            // A REFUSED await is not a deadline: answering
                            // `Deadline` would re-plan, re-arm the same
                            // condition and spin. The loop ends and says why.
                            self.note(format!("await {}: {e}", timer.as_str()));
                            Wake::Closed
                        }
                    };
                }
            }
        }
    }

    fn send(&mut self, line: &str) -> Result<String, String> {
        let reply = self.call(line);
        if reply.code == 0 {
            Ok(reply.stdout)
        } else {
            Err(reply.stderr)
        }
    }

    fn journal(&mut self, ring: &str, row: &str) -> u64 {
        super::cli::append_to(&self.cfg.state, ring, row).unwrap_or(0)
    }

    fn guards(&mut self) -> HostGuards {
        // Read FRESH, never cached across a park (the trait's own rule).
        let status = self.call("status");
        let hold = observe::StatusSample::parse(&status).is_ok_and(|s| s.hold);
        let who = self.call_lines("who");
        let sid = &self.cfg.sid;
        let busy = who.code == 0
            && who.stdout.lines().any(|row| {
                (sid.is_empty() || row.split_whitespace().any(|t| t == sid))
                    && field_of(row, "driving").is_some()
            });
        let custody = self.call("custody");
        let custody_user = custody.code == 0 && custody_is_the_person(&custody.stdout);
        self.refresh_generation();
        HostGuards {
            hold,
            busy,
            custody_user,
            generation: self.generation,
            engaged: self.cfg.engaged,
        }
    }

    fn now_ms(&mut self) -> u64 {
        u64::try_from(
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_millis())
                .unwrap_or(0),
        )
        .unwrap_or(u64::MAX)
    }

    fn now_unix(&mut self) -> i64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| i64::try_from(d.as_secs()).unwrap_or(i64::MAX))
            .unwrap_or(0)
    }
}

#[path = "wire_tests.rs"]
#[cfg(test)]
mod tests;
