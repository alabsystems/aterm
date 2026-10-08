// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! SURVIVING AN ATERM SELF-UPDATE: a `subscribe` stream and a blocking read
//! follow the instance that replaced the one they were talking to.
//!
//! An update replaces the server process and its socket (`aterm.sock` is
//! repointed at the successor's `aterm-<pid>.sock`), and the old process
//! `_exit`s at its Commit, closing every connection it held. A relayed EOF
//! must not read as "session over", nor a lost `await` as a plain error.
//!
//! THE RULE, at a hang-up that the stream itself did not announce:
//!
//! 1. every subscribed session already said `EVENT <local> exited` → the
//!    session is over: exit 0;
//! 2. the SAME server process still ANSWERS on the socket we dialled (its pid,
//!    read off the connection with `LOCAL_PEERPID`/`SO_PEERCRED`, and an `OK` to
//!    `version` — [`serving_pid`] says why accepting is not enough), through the
//!    SAME listening socket the stream was accepted through ([`ListenerId`]) →
//!    it ended the stream on purpose (a fixed-target subscription whose
//!    sessions all closed): exit 0 — unless the request is a `dial <name> …`,
//!    which the local server only relays: then the connection to that remote
//!    closed under it, and the call exits [`EXIT_REPLACED`] saying so;
//! 3. otherwise the server is gone. Within [`SUCCESSOR_BOUND`] (the handoff's own
//!    hold bound) ANOTHER live server answering where this call's own
//!    resolution now points — the explicit socket a successor rebinds, the
//!    calling session's graph entry (a call made INSIDE a session follows that
//!    and nothing else), or, outside any session, the `latest` alias — is its
//!    successor: a different process, or the same pid through a fresh
//!    listening socket (a cold update's `exec` keeps the pid and binds the same
//!    path, and one that returned reopens it — [`ListenerId`]):
//!    `subscribe` re-sends its request there and keeps relaying, and a blocking
//!    read is asked once more. No successor: exit [`EXIT_REPLACED`] (75), never 0.
//!
//! BEHIND A FORWARD the server dialled is only a relay: a request naming a
//! session another aterm hosts is forwarded there. When that host goes away
//! the relay says so in place of the EOF
//! ([`aterm_types::control_verbs::host_gone_line`]: the host's pid and socket);
//! rule 2 does not apply, and rule 3 follows the host's successor where it
//! republishes the session ([`Follow::Host`]), never the relay.
//!
//! A RESUMED STREAM MARKS ITS HOLE: the successor's watch starts live, so each
//! target the last leg named gets `GAP <local> replaced` between the legs. A
//! successor that turns the resume away as busy, or closes on it without a
//! word, is asked again within the successor wait.
//!
//! An EXPLICIT `--timeout` bounds that wait too: it is a wall clock over the
//! whole `subscribe` relay and a deadline on a blocking read, and a hang-up
//! does not extend it. The successor wait is the SMALLER of
//! [`SUCCESSOR_BOUND`] and what the caller's deadline has left.
//!
//! Never a DIFFERENT instance the caller did not reach this way: the dead-alias
//! fallback that a fresh flagless call takes ("the newest instance that answers")
//! is deliberately not a successor, and a `--pid` pin names one process for good
//! — it follows that pid back onto a fresh listener at its path while it lives,
//! and nothing else. Nor, for a call made inside a session, the `latest` alias:
//! with two windows up it names the NEWER one, and until a self-updating older
//! one's successor republishes its graph entries, the alias would hand a
//! blocking read to a stranger's active tab.
//!
//! A SELF `subscribe` (`@.`, `@`, no selector) is resumed pinned to the session
//! it followed; a successor that lacks that session moves it to one of its own
//! only when the successor answers on the FIXED socket the call named
//! (`--sock`, not the `latest` alias) and STARTED after the old server went
//! away — a relaunch of the instance configured there, not a stranger or an
//! update's successor that lost the tab ([`after_refusal`]).
//!
//! WHEN THE OLD SERVER WENT AWAY is the kernel's report of its exit where one
//! was watched for ([`HangupAt`]), else when the stream read the hang-up; a
//! successor's start is the kernel's record ([`process_started`], macOS and
//! Linux); and the two are ordered on the boot clock, which a step of the wall
//! clock cannot reorder, wherever this process reads both on it — not under
//! Rosetta, where the wall clock orders them ([`later`]).

use std::io::{self, BufReader, Read as _};
use std::path::PathBuf;
use std::process::ExitCode;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use aterm_uds::CtlStream;
use aterm_uds::exitwatch::ExitMoment;
use aterm_uds::process::BootClock;

/// The exit code of an exchange cut by its server going away with no successor
/// answering (an update that did not come back, a crash, a quit), or that it
/// does not ask again: EX_TEMPFAIL's number — the request was not served, and
/// its note says what to look at before sending it again. Distinct from 0 so a
/// pipeline never reads the loss as "session over", and from 1/124 so it is
/// never read as a refusal or a timeout. The one code every client in this
/// binary ends such an exchange with (`aterm drive`'s persistent prompt too).
pub const EXIT_REPLACED: u8 = 75;

/// How long a successor may take to answer: the update handoff's own hold bound
/// (the outgoing process parks at most this long for its candidate to prove).
pub(crate) const SUCCESSOR_BOUND: Duration = Duration::from_secs(30);

/// Between two looks for a successor.
const SUCCESSOR_POLL: Duration = Duration::from_millis(100);

/// Where a successor of the server this call reached may answer.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Follow {
    /// A socket path the call pinned (`--sock`): a successor rebinds that
    /// same path.
    Path(String),
    /// Flagless: inside a session, the instance hosting it (its graph entry,
    /// `<dir>/graph/<sid>`) and nothing else; outside any session, the
    /// `latest` alias `<dir>/aterm.sock`. A successor republishes both and
    /// binds a new per-instance socket (`aterm-<its pid>.sock`), so the socket
    /// dialled never answers again.
    Flagless {
        /// The rendezvous dir the call resolved against.
        dir: PathBuf,
        /// `$ATERM_PARENT_SESSION_ID`: the session hosting the caller.
        self_sid: Option<String>,
    },
    /// THE HOST BEHIND A FORWARD: the server this call dialled only relayed the
    /// request to the aterm that hosts its session, and that aterm went away
    /// under it, which the relay said on the stream
    /// ([`aterm_types::control_verbs::host_gone_line`]). Its successor answers
    /// at `sock`, the socket the host was reached at, when it rebinds a fixed
    /// one; otherwise where it republishes the session's graph entry, `<dir of
    /// sock>/graph/<sid>` — the entry the relay itself resolved, in its own
    /// socket dir, to forward the request. Followed there directly, with that
    /// instance's own token, as a same-uid caller may: through the relay it
    /// would meet `ERR no such session` until the successor republished, and
    /// its stream would be judged against a relay that never served it.
    Host {
        /// The socket the gone host was reached at.
        sock: String,
        /// The session the request names (`@s-…`), whose graph entry the
        /// host's successor republishes; `None` when the request names none.
        sid: Option<String>,
    },
    /// No successor follows, for the reason given (a `--pid` pin still follows
    /// its own process: [`Unfollowed::PidPin`]).
    Nothing(Unfollowed),
}

/// Why a call follows no successor — the one reason its exit-75 note gives.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Unfollowed {
    /// `--pid <n>` names one process. That process back on a fresh listening
    /// socket (a cold update's `exec` keeps the pid) is followed while it lives
    /// ([`Redial::after_hangup`]), identified by its kernel start record
    /// ([`Redial::new`]).
    PidPin(u32),
    /// The redialled leg of a blocking read: this was already the one retry.
    AskedOnce,
    /// No rendezvous dir (`$HOME` and `$XDG_RUNTIME_DIR` unset): nowhere to
    /// look for a successor.
    NoRendezvousDir,
}

/// The redial policy of one call.
#[derive(Clone, Debug)]
pub(crate) struct Redial {
    follow: Follow,
    /// The start record of the process a `--pid` pin names
    /// ([`process_started`]), read when the call began; `None` otherwise.
    pinned: Option<Stamp>,
    bound: Duration,
    /// When a process started: [`process_started`], or a test's stand-in.
    started: fn(u32) -> Option<Stamp>,
    /// The clock a hang-up is read on: [`Stamp::now`], or a test's stand-in.
    clock: fn() -> Stamp,
    /// Told the pid of each server whose exit this relay watches, so a test
    /// can start a relaunch only once a watched exit is stamped.
    #[cfg(test)]
    on_watch: Option<fn(u32)>,
}

/// What a hang-up turned out to be.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Hangup {
    /// The server we dialled still answers, through the listening socket the
    /// stream was accepted through: it ended the exchange on purpose.
    StillServing,
    /// A live server answers where this call follows, and it is not the one
    /// the stream was on: another process, or the same pid through a fresh
    /// listening socket ([`ListenerId`]).
    Successor { path: String, pid: u32 },
    /// Nothing answered within `waited`: [`SUCCESSOR_BOUND`], or less when the
    /// caller's deadline had less left (`by_deadline`); zero when nothing
    /// follows, or the process a `--pid` pin follows has ended.
    Gone { waited: Duration, by_deadline: bool },
    /// This platform cannot read a peer pid, so a replacement cannot be told
    /// from a deliberate end: the call keeps its old behaviour.
    Unknown,
}

impl Redial {
    /// The policy for a call that follows `follow`. A `--pid` pin is recorded
    /// by its kernel start record: an `exec` keeps it, and a later process
    /// given the same pid has another.
    pub(crate) fn new(follow: Follow) -> Self {
        let pinned = match &follow {
            Follow::Nothing(Unfollowed::PidPin(pid)) => process_started(*pid),
            _ => None,
        };
        Self {
            follow,
            pinned,
            bound: SUCCESSOR_BOUND,
            started: process_started,
            clock: Stamp::now,
            #[cfg(test)]
            on_watch: None,
        }
    }

    /// A policy that never follows — the redialled leg of a blocking read,
    /// which is retried once.
    pub(crate) fn none() -> Self {
        Self::new(Follow::Nothing(Unfollowed::AskedOnce))
    }

    /// Why this policy follows no successor, when it follows none.
    pub(crate) fn unfollowed(&self) -> Option<&Unfollowed> {
        match &self.follow {
            Follow::Nothing(why) => Some(why),
            _ => None,
        }
    }

    /// The policy for what follows a forward's host that went away at `sock`
    /// ([`Follow::Host`]): its successor, for a request naming the session
    /// `sid`. A call that follows nothing still follows nothing — a `--pid`
    /// pin names the process it dialled, which a host behind it is not, and a
    /// retry leg was already the one retry — and the bound, clocks and
    /// start-record reader stay the call's.
    pub(crate) fn host(&self, sock: &str, sid: Option<&str>) -> Self {
        match &self.follow {
            Follow::Nothing(Unfollowed::PidPin(_) | Unfollowed::AskedOnce) => self.clone(),
            _ => Self {
                follow: Follow::Host {
                    sock: sock.to_string(),
                    sid: sid.map(str::to_string),
                },
                pinned: None,
                ..self.clone()
            },
        }
    }

    /// The same policy with a different successor bound (tests).
    #[cfg(test)]
    pub(crate) fn with_bound(mut self, bound: Duration) -> Self {
        self.bound = bound;
        self
    }

    /// The same policy reading a successor's start from `started` (tests: an
    /// in-process fake successor started long before any hang-up it serves).
    #[cfg(test)]
    pub(crate) fn with_started(mut self, started: fn(u32) -> Option<Stamp>) -> Self {
        self.started = started;
        self
    }

    /// The same policy reading every moment it stamps from `clock` (tests: a
    /// stepped or foreign clock, or one that records its readers).
    #[cfg(test)]
    pub(crate) fn with_clock(mut self, clock: fn() -> Stamp) -> Self {
        self.clock = clock;
        self
    }

    /// The same policy telling `note` the pid of each server whose exit it
    /// watches, once the watch is registered.
    #[cfg(test)]
    pub(crate) fn with_watch_note(mut self, note: fn(u32)) -> Self {
        self.on_watch = Some(note);
        self
    }

    /// Stamp the exit of the live server `pid` when the kernel reports it
    /// ([`ExitMoment`]). `None` where it cannot be watched; [`HangupAt::seen`]
    /// alone then orders a successor.
    fn watch_exit(&self, pid: Option<u32>) -> Option<ExitMoment<Stamp>> {
        let pid = pid?;
        let watch = ExitMoment::watch(pid, self.clock)?;
        #[cfg(test)]
        if let Some(note) = self.on_watch {
            note(pid);
        }
        Some(watch)
    }

    /// The [`Arrival`] of the successor `pid` answering at `path`, for a
    /// stream whose server went away at `hung_up` ([`HangupAt::moment`]):
    /// [`Arrival::Shared`] unless `path` is the fixed socket this call named
    /// ([`only_a_relaunch_binds`]), else when it started.
    pub(crate) fn arrival(&self, pid: u32, path: &str, hung_up: Stamp) -> Arrival {
        match &self.follow {
            Follow::Path(fixed) if fixed == path && only_a_relaunch_binds(path) => {
                arrival_of((self.started)(pid), hung_up)
            }
            _ => Arrival::Shared,
        }
    }

    /// Every path a successor of the server reached at `dialed` may answer on,
    /// in order, resolved now. A call made inside a session follows only that
    /// session's graph entry: the dialled path and the alias can name another
    /// live window, and only the entry is republished by the hosting
    /// instance's successor.
    fn candidates(&self, dialed: &str) -> Vec<String> {
        match &self.follow {
            Follow::Path(path) => vec![dialed.to_string(), path.clone()],
            Follow::Flagless {
                dir,
                self_sid: Some(sid),
            } => super::self_instance_sock_in(dir, Some(sid))
                .into_iter()
                .collect(),
            Follow::Flagless {
                dir,
                self_sid: None,
            } => vec![
                dialed.to_string(),
                dir.join(super::SOCK_FILE).to_string_lossy().into_owned(),
            ],
            Follow::Host { sock, sid } => {
                let republished = std::path::Path::new(sock)
                    .parent()
                    .and_then(|dir| super::self_instance_sock_in(dir, sid.as_deref()));
                std::iter::once(sock.clone())
                    .chain(republished.filter(|entry| entry != sock))
                    .collect()
            }
            // Where the pinned process was reached ([`Unfollowed::PidPin`]).
            Follow::Nothing(Unfollowed::PidPin(_)) => vec![dialed.to_string()],
            Follow::Nothing(_) => Vec::new(),
        }
    }

    /// Classify a hang-up from the server `old` reached at `dialed` through
    /// `listener` (the module rule). Waits for a successor up to the bound or
    /// the caller's deadline `until`, whichever is first; returns at once when
    /// the old server still serves or nothing can follow.
    pub(crate) fn after_hangup(
        &self,
        dialed: &str,
        old: Option<u32>,
        listener: Option<ListenerId>,
        until: Option<Instant>,
    ) -> Hangup {
        let Some(old) = old else {
            return Hangup::Unknown;
        };
        let wait = Wait::from_now(self.bound, until);
        if wait
            .probe(dialed)
            .is_some_and(|pid| is_the_same_server(pid, dialed, old, listener))
        {
            return Hangup::StillServing;
        }
        self.successor(dialed, old, listener, &wait, true)
    }

    /// Classify a hang-up a forward said was its host's: `old`, reached at
    /// `sock`, went away under the relay
    /// ([`aterm_types::control_verbs::host_gone_line`]), so there is no
    /// deliberate end to look for — the relay already looked. Waits for a
    /// successor as [`Redial::after_hangup`] does, where this policy follows
    /// ([`Redial::host`]); the host itself answering at `sock` again counts —
    /// it serves once more, so the request is asked of it — and is never read
    /// as [`Hangup::StillServing`], which would end a stream as one ended on
    /// purpose.
    pub(crate) fn after_host_gone(&self, sock: &str, old: u32, until: Option<Instant>) -> Hangup {
        self.successor(sock, old, None, &Wait::from_now(self.bound, until), false)
    }

    /// The successor wait of [`Redial::after_hangup`] and
    /// [`Redial::after_host_gone`]: a live server answering where this policy
    /// follows — never `old` through `listener` again, when `exclude_old`.
    fn successor(
        &self,
        dialed: &str,
        old: u32,
        listener: Option<ListenerId>,
        wait: &Wait,
        exclude_old: bool,
    ) -> Hangup {
        // A `--pid` pin follows that one process alone: back on a fresh
        // listener where it was reached — which only a listener identity tells
        // from the one that hung up — and only while its start record still
        // names it. Not while `kill(pid, 0)` finds the pid: a ZOMBIE (ended,
        // its parent has not reaped it yet) passes that and answers nothing,
        // and it has no start record. Where none was read (another uid, a
        // platform that keeps none) the pin cannot be told from a zombie or a
        // pid given to another process, and follows nothing.
        let pin = match (&self.follow, self.pinned) {
            (Follow::Nothing(Unfollowed::PidPin(pin)), Some(born)) if listener.is_some() => {
                Some((*pin, born))
            }
            (Follow::Nothing(_), _) => {
                return Hangup::Gone {
                    waited: Duration::ZERO,
                    by_deadline: false,
                };
            }
            _ => None,
        };
        let still_pinned = |pid: u32, born: Stamp| process_started(pid) == Some(born);
        loop {
            // The pinned process has ended: it went away, and no wait ran out.
            if let Some((pin, born)) = pin
                && !still_pinned(pin, born)
            {
                return Hangup::Gone {
                    waited: Duration::ZERO,
                    by_deadline: false,
                };
            }
            for path in self.candidates(dialed) {
                if let Some(pid) = wait.probe(&path)
                    && !(exclude_old && is_the_same_server(pid, &path, old, listener))
                    && pin.is_none_or(|(pin, born)| pid == pin && still_pinned(pid, born))
                {
                    return Hangup::Successor { path, pid };
                }
            }
            let now = Instant::now();
            if now >= wait.deadline {
                return Hangup::Gone {
                    waited: now.duration_since(wait.started),
                    by_deadline: wait.by_deadline,
                };
            }
            std::thread::sleep(SUCCESSOR_POLL.min(wait.deadline - now));
        }
    }
}

/// One wait for a successor: from when it began, to the bound or the caller's
/// explicit deadline, whichever comes first (the rule on this module).
struct Wait {
    started: Instant,
    deadline: Instant,
    /// The caller's deadline, not the bound, ends it.
    by_deadline: bool,
}

impl Wait {
    fn from_now(bound: Duration, until: Option<Instant>) -> Self {
        let started = Instant::now();
        let bound = started + bound;
        Self {
            started,
            deadline: until.map_or(bound, |until| until.min(bound)),
            by_deadline: until.is_some_and(|until| until < bound),
        }
    }

    /// The pid answering at `path`, waiting no longer than the wait has left
    /// (never less than [`PROBE_FLOOR`], never more than [`PROBE_TIMEOUT`]).
    fn probe(&self, path: &str) -> Option<u32> {
        let left = self.deadline.saturating_duration_since(Instant::now());
        serving_pid_within(path, left.clamp(PROBE_FLOOR, PROBE_TIMEOUT))
    }
}

/// How long a probe waits for `OK` to its `version` — less when the caller's
/// deadline has less left, never less than [`PROBE_FLOOR`].
const PROBE_TIMEOUT: Duration = Duration::from_secs(2);

/// The shortest probe: a live aterm answers `version` in well under this.
const PROBE_FLOOR: Duration = Duration::from_millis(100);

/// [`serving_pid_within`] with the full probe wait (tests).
#[cfg(test)]
pub(crate) fn serving_pid(path: &str) -> Option<u32> {
    serving_pid_within(path, PROBE_TIMEOUT)
}

/// The pid of the process answering at `path` right now (it accepted a
/// connection and answered `version` with `OK` within `wait`), or `None`
/// (nothing answers, or the platform cannot read a peer pid).
///
/// A bare connect is not enough: a killed server closes its streams before its
/// listening socket, and a connect in between lands in the dying listener's
/// backlog and reads the dead process's pid.
fn serving_pid_within(path: &str, wait: Duration) -> Option<u32> {
    let (pid, line) = version_at(path, wait)?;
    line.starts_with("OK").then_some(pid?)
}

/// What answered `version` at `path` within `wait`: the peer's pid, where it
/// could be read, and its status line — the one it left behind when it closed
/// before the request was written included, as a listener with no lane free
/// does ([`ask_once`]). `None` when nothing connected, or nothing was said.
///
/// The pid is read right after the connect, and can be missing even from a
/// live process: Darwin answers `ENOTCONN` to `LOCAL_PEERPID` (and to every
/// option that carries a pid) once the peer has closed, measured 2026-10-01 on
/// Darwin 25.6 — and a busy listener writes its line and closes at once.
fn version_at(path: &str, wait: Duration) -> Option<(Option<u32>, String)> {
    let (stream, token) = super::connect_then_read(path, super::read_token_for).ok()?;
    let pid = server_pid(&stream);
    let _ = stream.set_read_timeout(Some(wait));
    let _ = stream.set_write_timeout(Some(wait));
    let mut reader = BufReader::new(&stream);
    match ask_once(&stream, &mut reader, token.as_deref(), "version\n") {
        Answer::Line(line) if !line.is_empty() => Some((pid, line)),
        Answer::Line(_) | Answer::Gone => None,
    }
}

/// Whether the server `pid` still answers at `path` through `listener` — with
/// ANY answer to `version`: `OK`, or the busy refusal a listener with every
/// lane at work gives. Either is a live process's accept loop (a dying one, or
/// its backlog, writes nothing), so either proves that a successor which
/// closed on a resume ask without a word is still there to be asked again
/// ([`relay_subscription`]). [`still_serving`] takes `OK` alone; asked of a
/// saturated successor — the very one that closes asks — it read as gone.
///
/// A busy answer often comes with no pid ([`version_at`]): the listener it
/// came through then names the server — the socket file read once a
/// connection to `pid` was up through it ([`ListenerId`]), unchanged after
/// this answer. Without that identity, an answer with no pid proves nothing.
fn answers_at(path: &str, pid: u32, listener: Option<ListenerId>) -> bool {
    let Some((answered, line)) = version_at(path, PROBE_TIMEOUT) else {
        return false;
    };
    (line.starts_with("OK") || is_busy(&line))
        && match answered {
            Some(answered) => is_the_same_server(answered, path, pid, listener),
            None => listener.is_some_and(|then| listener_at(path) == Some(then)),
        }
}

/// The pid of the server at the other end of `stream`.
pub(crate) fn server_pid(stream: &CtlStream) -> Option<u32> {
    aterm_uds::fdpass::peer_pid(stream).ok()
}

/// Whether the server `old` still answers at `dialed` through `listener`:
/// [`Redial::after_hangup`]'s first look alone, with no wait; `false` when it
/// cannot tell.
pub(crate) fn still_serving(dialed: &str, old: Option<u32>, listener: Option<ListenerId>) -> bool {
    old.is_some_and(|old| {
        serving_pid_within(dialed, PROBE_TIMEOUT)
            .is_some_and(|pid| is_the_same_server(pid, dialed, old, listener))
    })
}

/// WHICH LISTENING SOCKET a path names, and why a pid alone does not name a
/// server: [`aterm_uds::listener`], which aterm-gui's forward relay judges its
/// host by too.
pub(crate) use aterm_uds::listener::{ListenerId, listener_at};

/// Whether `pid`, answering at `path`, is the server `old` a stream reached
/// through `listener`: the same process and, where both were read, the same
/// listening socket ([`ListenerId`]).
fn is_the_same_server(pid: u32, path: &str, old: u32, listener: Option<ListenerId>) -> bool {
    pid == old && aterm_uds::listener::still_names(path, listener)
}

/// The saved connection a `dial <name> …` request relays to, `None` for any
/// other request. The local server only relays a dial: when it still serves
/// after a hang-up, the connection that closed is the one to that remote.
pub(crate) fn dial_name<S: AsRef<str>>(request_parts: &[S]) -> Option<&str> {
    match request_parts {
        [dial, name, ..] if dial.as_ref() == "dial" => Some(name.as_ref()),
        _ => None,
    }
}

/// The one session a request names by id (`@s-<hex>`): the first word of a
/// verb's request, the second of a `subscribe` — the only shapes a server
/// forwards to the aterm hosting the session, so the one whose host a hang-up
/// behind a forward follows ([`Follow::Host`]). `None` for anything else:
/// self, a local number, `@*`, a list.
pub(crate) fn target_sid<S: AsRef<str>>(request_parts: &[S]) -> Option<&str> {
    let selector = match request_parts {
        [verb, selector, ..] if verb.as_ref() == "subscribe" => selector.as_ref(),
        [selector, ..] => selector.as_ref(),
        [] => return None,
    };
    let sid = selector.strip_prefix('@')?;
    let hex = sid.strip_prefix("s-")?;
    (!hex.is_empty() && hex.bytes().all(|b| b.is_ascii_hexdigit())).then_some(sid)
}

/// The note for a `dial <name> …` request (`request_parts`, the whole request)
/// whose remote hung up while the local server still serves — before its reply,
/// or (`mid_stream`) in a `subscribe` stream before its sessions exited: the
/// remote [`dial_name`] names, and what the dial relayed to it ([`remote_closed_note`]).
/// `None` for a request that is not a dial. Both call sites take the note from
/// here, so what counts as the relayed request is decided once.
pub(crate) fn remote_closed_note_for<S: AsRef<str>>(
    request_parts: &[S],
    mid_stream: bool,
) -> Option<String> {
    let name = dial_name(request_parts)?;
    Some(remote_closed_note(name, mid_stream, &request_parts[2..]))
}

/// The note for a `dial` to `name` whose remote hung up while the local server
/// still serves. `relayed` is the request past `dial <name>`: a `turn` or
/// `post` may already have taken effect on the remote, and what an armed wait
/// ([`armed_wait`]) waits for may already have happened (a self-updating
/// remote's successor parses a finished command's output before any re-dial
/// can arm), so those notes say to look there before re-sending, as
/// [`not_asked_again_note`] does after an update. Anything else can be
/// dialled again.
fn remote_closed_note<S: AsRef<str>>(name: &str, mid_stream: bool, relayed: &[S]) -> String {
    let when = if mid_stream {
        "before its sessions exited"
    } else {
        "before it answered"
    };
    let then = match (verb_and_args(relayed).0, armed_wait(relayed)) {
        (Some("turn"), _) => {
            "its `turn` may already have been typed, and submitted, on the remote — read the \
             screen there (`text`) before sending it again; never retry it blind"
                .to_owned()
        }
        (Some("post"), _) => {
            "its `post` may already be queued on the remote — never post it again blind: \
             re-post it with the same `key=`, or look for it there first (`outbox`, and the \
             `post`/`post-landed` rows of `timeline`)"
                .to_owned()
        }
        (_, Some(wait)) => format!(
            "its `{wait}` waits for a change after it was armed, which may have happened on the \
             remote; read the session there (`status`, `text`) before waiting again"
        ),
        _ => "the exchange cannot be resumed".to_owned(),
    };
    format!(
        "the connection to the remote '{}' closed {when}; {then} (exit {EXIT_REPLACED})",
        name.escape_debug()
    )
}

/// A moment as this machine's clocks read it: what a hang-up and a
/// successor's start are ordered by ([`later`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Stamp {
    /// The kernel's boot clock ([`BootClock`]): monotonic, so a wall-clock step
    /// cannot reorder two readings. `None` where this process cannot read it
    /// in the kernel's unit: Windows, or a Rosetta-translated process
    /// ([`aterm_uds::process::apple_boot_clock`]).
    pub(crate) boot: Option<BootClock>,
    /// The wall clock: the basis only where one side has no boot reading.
    pub(crate) wall: Option<SystemTime>,
}

impl Stamp {
    /// Now, on both clocks.
    pub(crate) fn now() -> Self {
        Self {
            boot: aterm_uds::process::boot_clock_now(),
            wall: Some(SystemTime::now()),
        }
    }
}

/// Whether `a` is strictly after `b`: on the boot clock where both were read
/// there, else on the wall clock where both were; `None` when they share
/// neither.
///
/// The boot clock first, because a wall-clock step between a hang-up and a
/// successor's start can reorder them either way. Where one side has only the
/// wall clock (an unreadable boot record, or a hang-up read under Rosetta,
/// whose `mach_absolute_time` is in nanoseconds while start records are in
/// kernel ticks), a step can still misjudge. Native macOS and 64-bit Linux
/// read both sides on the boot clock.
pub(crate) fn later(a: Stamp, b: Stamp) -> Option<bool> {
    match (a.boot, b.boot) {
        (Some(a), Some(b)) => Some(a > b),
        _ => Some(a.wall? > b.wall?),
    }
}

/// When the process `pid` started, as the kernel recorded it
/// ([`aterm_uds::process::read_process_start`]: boot clock on macOS and Linux,
/// with macOS's wall record beside it), or on the wall clock alone
/// ([`aterm_uds::process::read_process_birth`]). `None` on another platform,
/// for another uid's process, or one that has ended.
pub(crate) fn process_started(pid: u32) -> Option<Stamp> {
    let pid = i32::try_from(pid).ok()?;
    let wall = |birth: aterm_uds::process::ProcessBirth| {
        UNIX_EPOCH
            .checked_add(Duration::from_secs(birth.seconds))?
            .checked_add(Duration::from_micros(birth.microseconds))
    };
    if let Some(start) = aterm_uds::process::read_process_start(pid) {
        return Some(Stamp {
            boot: Some(start.boot),
            wall: start.wall.and_then(wall),
        });
    }
    let birth = aterm_uds::process::read_process_birth(pid)?;
    Some(Stamp {
        boot: None,
        wall: Some(wall(birth)?),
    })
}

/// Whether the socket at `path` is one only a relaunch of the instance
/// configured on it can bind: a fixed `--control-sock` path. Not the `latest`
/// alias (`aterm.sock` or any link, which names the newest instance and may be
/// republished by another window's update successor), and not an instance's
/// own `aterm-<pid>.sock` (a new process there only reuses the pid). On those
/// a fresh process cannot be told from another aterm's.
fn only_a_relaunch_binds(path: &str) -> bool {
    let path = std::path::Path::new(path);
    let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
        return false;
    };
    name != super::SOCK_FILE
        && aterm_types::control_socket::instance_pid(name).is_none()
        && !std::fs::symlink_metadata(path).is_ok_and(|m| m.file_type().is_symlink())
        && aterm_uds::latest::target_name(path).is_none()
}

/// Whether an I/O error means the peer went away (as opposed to a deadline or a
/// local fault).
pub(crate) fn is_hangup(e: &io::Error) -> bool {
    matches!(
        e.kind(),
        io::ErrorKind::UnexpectedEof
            | io::ErrorKind::ConnectionReset
            | io::ErrorKind::ConnectionAborted
            | io::ErrorKind::BrokenPipe
            | io::ErrorKind::NotConnected
    )
}

/// Whether a request is retried once on a successor after a hang-up before its
/// reply: a blocking read with no instance-local anchor.
///
/// Not retried (they end with [`EXIT_REPLACED`] and a note from
/// [`not_asked_again_note`]):
/// * `await seq <n>` (a per-grid counter a successor restarts), `await inbox
///   since=<id>`, `inbox get <id>` and `inbox since=<id>` (the successor
///   numbers the rows it re-delivers afresh);
/// * waits anchored at the moment they were armed: `await block`, a bare
///   `await seq`, `await consent`. What they wait for may be what a parked
///   producer deferred to the successor, parsed before any retry can arm;
/// * any request naming its session by local number (`@<n>`,
///   [`is_local_selector`]): a successor numbers adopted sessions afresh.
///
/// The bare `inbox` listing has no anchor and is retried.
pub(crate) fn retries_across_replacement(request_parts: &[String]) -> bool {
    let rest = match request_parts.first() {
        Some(sel) if is_local_selector(sel) => return false,
        Some(sel) if sel.starts_with('@') => &request_parts[1..],
        _ => request_parts,
    };
    let args: Vec<&str> = rest.iter().skip(1).map(String::as_str).collect();
    match rest.first().map(String::as_str) {
        Some("text" | "wait" | "ready") => true,
        Some("inbox") => inbox_plain_read(&args),
        Some("await") => match args.first() {
            Some(&("seq" | "block" | "consent")) => false,
            Some(&"inbox") => !args.iter().any(|a| a.starts_with("since=")),
            Some(_) => true,
            None => false,
        },
        _ => false,
    }
}

/// Whether an `inbox` request is a plain read of what is queued now: a
/// count, `--peek` and `--meta` name nothing in the old process, unlike
/// `get <id>`, `since=<id>` or a write such as `seen <id>`.
fn inbox_plain_read(args: &[&str]) -> bool {
    args.iter()
        .all(|a| matches!(*a, "--peek" | "--meta") || a.parse::<u64>().is_ok())
}

/// The part of a blocking read that names something in the old process (a
/// local session number `@3`, a grid counter `seq 40`, an inbox anchor
/// `since=12` / `inbox get 4`); `None` when the request carries none.
pub(crate) fn old_process_anchor(request_parts: &[String]) -> Option<String> {
    let rest = match request_parts.first() {
        Some(sel) if is_local_selector(sel) => return Some(sel.clone()),
        Some(sel) if sel.starts_with('@') => &request_parts[1..],
        _ => request_parts,
    };
    let args: Vec<&str> = rest.iter().skip(1).map(String::as_str).collect();
    match rest.first().map(String::as_str) {
        Some("inbox") if !inbox_plain_read(&args) => Some(rest.join(" ")),
        Some("await") => match args.first() {
            Some(&"seq") if args.len() >= 2 && !args[1].contains('=') => {
                Some(format!("seq {}", args[1]))
            }
            Some(&"inbox") => args
                .iter()
                .find(|a| a.starts_with("since="))
                .map(|a| (*a).to_string()),
            _ => None,
        },
        _ => None,
    }
}

/// The first `subscribe` target named by local number (`@<n>`) that the
/// stream never tied to a session id, which [`resume_request`] cannot follow.
fn untied_local_target(request: &str, sids: &[(String, String)]) -> Option<String> {
    request
        .split_whitespace()
        .filter(|token| token.starts_with('@'))
        .flat_map(|token| token.split(','))
        .find(|piece| {
            is_local_selector(piece) && !sids.iter().any(|(local, _)| *local == piece[1..])
        })
        .map(str::to_string)
}

/// The verb and its arguments, past a leading `@<selector>`.
fn verb_and_args<S: AsRef<str>>(request_parts: &[S]) -> (Option<&str>, Vec<&str>) {
    let rest = match request_parts.first() {
        Some(sel) if sel.as_ref().starts_with('@') => &request_parts[1..],
        _ => request_parts,
    };
    (
        rest.first().map(AsRef::as_ref),
        rest.iter().skip(1).map(AsRef::as_ref).collect(),
    )
}

/// The wait a request is when it waits for a change after the moment it was
/// armed, past a leading `@<selector>`: `await block`, `await consent`, or a
/// bare `await seq` (no counter after `seq`; a `key=value` such as
/// `timeout=0` is not one). `None` otherwise. Its event may have happened while
/// the connection was down, so it is never asked again blind
/// ([`not_asked_again_note`], [`remote_closed_note`]).
fn armed_wait<S: AsRef<str>>(request_parts: &[S]) -> Option<&'static str> {
    let (Some("await"), args) = verb_and_args(request_parts) else {
        return None;
    };
    match args.first().copied() {
        Some("block") => Some("await block"),
        Some("consent") => Some("await consent"),
        Some("seq") if args.get(1).is_none_or(|a| a.contains('=')) => Some("await seq"),
        _ => None,
    }
}

/// What a request cut by a replacement and not asked again
/// ([`retries_across_replacement`]) tells its caller: why, and what to do
/// instead.
pub(crate) fn not_asked_again_note(request_parts: &[String]) -> String {
    match (verb_and_args(request_parts).0, armed_wait(request_parts)) {
        (Some("turn"), _) => {
            "a `turn` TYPES: its text may already have been typed, and submitted, in the \
             session — read the screen (`text`) before sending it again; never retry it blind"
                .to_owned()
        }
        (Some("post"), _) => {
            "the `post` may already be queued — an update carries a queued post under its id \
             and the successor sends it — so never post it again blind: re-post it with the \
             same `key=` (the broker keeps one), or look for it first (`outbox`, and the \
             `post`/`post-landed` rows of `timeline`)"
                .to_owned()
        }
        (_, Some("await seq")) => {
            "a bare `await seq` waits for the next change after it was armed, and the update \
             may have made it — read the session (`status`, `text`) before waiting again"
                .to_owned()
        }
        (_, Some(_)) => {
            "`await block` and `await consent` wait for a change after they were armed, and it \
             may have happened during the update — read the session (`status`, `text`) before \
             waiting again"
                .to_owned()
        }
        // The anchor itself, when the request names one ([`old_process_anchor`]);
        // with none (a bare `await`), no reason is claimed.
        _ => match old_process_anchor(request_parts) {
            Some(anchor) => {
                format!("`{anchor}` names a position in the old process — re-issue it")
            }
            None => "re-issue the request".to_owned(),
        },
    }
}

/// Whether a hang-up before the reply is reported as a replacement (followed
/// to a successor, or [`EXIT_REPLACED`] with a note) rather than a bare
/// connection error: every blocking read ([`is_blocking_read`]), `turn` and
/// `post`.
///
/// A `turn` is a write that blocks for minutes; a retry after a bare error
/// would type the prompt twice. A `post --wait=` (or `--wait-ack`) queued
/// before the park is carried to the successor and sent from there; a caller
/// that retried it would post it twice. Neither is asked again
/// ([`retries_across_replacement`]).
pub(crate) fn reports_replacement(request_parts: &[String]) -> bool {
    is_blocking_read(request_parts)
        || matches!(verb_and_args(request_parts).0, Some("turn" | "post"))
}

/// Whether the request is a blocking read at all (so a hang-up before its reply
/// is reported as a replacement rather than as a bare connection error).
pub(crate) fn is_blocking_read(request_parts: &[String]) -> bool {
    let rest = match request_parts.first() {
        Some(sel) if sel.starts_with('@') => &request_parts[1..],
        _ => request_parts,
    };
    matches!(
        rest.first().map(String::as_str),
        Some("text" | "wait" | "ready" | "await" | "inbox")
    )
}

/// Whether `sel` names a session by the process-local number the server gave
/// it (`@<digits>`, `Selector::Local`), which a successor assigns afresh.
pub(crate) fn is_local_selector(sel: &str) -> bool {
    sel.strip_prefix('@')
        .is_some_and(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()))
}

/// The request a redialled `subscribe` sends.
///
/// `since=` is removed: it is the old process's per-grid content counter,
/// which a successor restarts, so the successor sends each target's current
/// screen fresh.
///
/// The event anchors `since-block=` and `since-turn=` are kept and advanced:
/// the handoff carries shell blocks, their count and turn ids, so they mean
/// the same on the successor. The resumed request asks for everything after the
/// newest `block-complete` and `turn` this relay has seen ([`EventAnchors`], or
/// the original anchor when the stream showed none), so a block completed
/// before the resubscribe is still pushed. A successor whose count is below an
/// anchor seeds at its live high. A multi-target stream carries none (the
/// server takes resume anchors for a single target only).
///
/// A self target (`@.`, `@`, or no selector) is pinned to the sid the stream's
/// `sub` line named, since self is resolved again on the successor; with no
/// sid to pin, the anchors are left off.
///
/// A target named by local number (`@<n>`, [`is_local_selector`]) is rewritten
/// to the sid its `sub <n> <sid>` line tied it to (`sids`,
/// [`FrameTracker::sids`]); `None` when one was never tied, and the relay ends
/// with [`EXIT_REPLACED`].
pub(crate) fn resume_request(
    request: &str,
    sids: &[(String, String)],
    seen: &EventAnchors,
) -> Option<String> {
    resume_request_with(request, sids, seen, true)
}

/// The same request with its self target as the client wrote it and no
/// anchors: the fallback for a pinned resume a fresh successor refused with
/// `ERR no such session` (a fresh instance names its sessions afresh). Asked
/// only of a fresh successor ([`after_refusal`]). `None` when it would equal
/// the pinned request.
pub(crate) fn unpinned_resume_request(
    request: &str,
    sids: &[(String, String)],
    seen: &EventAnchors,
) -> Option<String> {
    let pinned = resume_request_with(request, sids, seen, true)?;
    let unpinned = resume_request_with(request, sids, seen, false)?;
    (unpinned != pinned).then_some(unpinned)
}

/// What one resume leg may ask ([`relay_subscription`]): the last leg's
/// `request` resumed ([`resume_request`]), and the original request's self
/// target as written, with no anchors ([`unpinned_resume_request`]), when that
/// differs; [`after_refusal`] decides whether it is asked. `None` as
/// [`resume_request`].
///
/// The fallback derives from the original, not the last leg: after one pinned
/// resume the leg names a sid, so a fallback derived from it would be `None`.
pub(crate) fn resume_lines(
    original: &str,
    request: &str,
    sids: &[(String, String)],
    seen: &EventAnchors,
) -> Option<(String, Option<String>)> {
    let resumed = resume_request(request, sids, seen)?;
    let fallback = unpinned_resume_request(original, sids, seen).filter(|own| *own != resumed);
    Some((resumed, fallback))
}

fn resume_request_with(
    request: &str,
    sids: &[(String, String)],
    seen: &EventAnchors,
    pin: bool,
) -> Option<String> {
    let mut out: Vec<String> = Vec::new();
    let mut block = seen.block;
    let mut turn = seen.turn;
    let anchor = |v: &str| v.parse::<u64>().ok();
    for token in request.split_whitespace() {
        if token.starts_with("since=") {
            continue;
        }
        if let Some(v) = token.strip_prefix("since-block=") {
            block = block.max(anchor(v));
            continue;
        }
        if let Some(v) = token.strip_prefix("since-turn=") {
            turn = turn.max(anchor(v));
            continue;
        }
        if !token.starts_with('@') {
            out.push(token.to_string());
            continue;
        }
        let mut pieces = Vec::new();
        for piece in token.split(',') {
            if is_local_selector(piece) {
                let (_, sid) = sids.iter().find(|(local, _)| *local == piece[1..])?;
                pieces.push(format!("@{sid}"));
            } else {
                pieces.push(piece.to_string());
            }
        }
        out.push(pieces.join(","));
    }
    let selectors: Vec<&String> = out.iter().filter(|t| t.starts_with('@')).collect();
    let single = match selectors.as_slice() {
        [] => true,
        [one] => !one.contains(',') && one.as_str() != "@*",
        _ => false,
    };
    let self_target = selectors.first().is_none_or(|s| is_self_selector(s));
    // Self is resolved again on the successor (for a client outside every
    // session, the active tab), and the anchors are one session's ids. So self
    // is rewritten to the sid the stream's `sub <n> <sid>` line named; with
    // none, the anchors are not sent and the successor seeds at its live high.
    let pinned = single && (!self_target || (pin && pin_self(&mut out, sids)));
    if pinned {
        if let Some(id) = block {
            out.push(format!("since-block={id}"));
        }
        if let Some(id) = turn {
            out.push(format!("since-turn={id}"));
        }
    }
    let mut line = out.join(" ");
    line.push('\n');
    Some(line)
}

/// Rewrite a self target in `out` to `@<sid>` of the session the stream was
/// tied to ([`self_sid`]), replacing `@.`/`@` or inserted after `subscribe`
/// when no selector was named. `false` when there is no such sid.
fn pin_self(out: &mut Vec<String>, sids: &[(String, String)]) -> bool {
    let Some(sid) = self_sid(sids) else {
        return false;
    };
    let at = format!("@{sid}");
    if let Some(slot) = out.iter_mut().find(|t| is_self_selector(t)) {
        *slot = at;
        return true;
    }
    match out.iter().position(|t| t == "subscribe") {
        Some(verb) => {
            out.insert(verb + 1, at);
            true
        }
        None => false,
    }
}

/// Whether a selector token names the connection's own session (`@.`, or a
/// bare `@`), which the server resolves afresh on every request.
fn is_self_selector(token: &str) -> bool {
    token == "@." || token == "@"
}

/// The one session a single-target stream's `sub` lines tied it to, or `None`
/// when they named none or more than one.
fn self_sid(sids: &[(String, String)]) -> Option<&str> {
    let (_, first) = sids.first()?;
    sids.iter()
        .all(|(_, sid)| sid == first)
        .then_some(first.as_str())
}

/// The newest `block-complete` and `turn` ids a relayed `events` stream has
/// shown, across every leg: what a resumed request continues after
/// ([`resume_request`]).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct EventAnchors {
    pub(crate) block: Option<u64>,
    pub(crate) turn: Option<u64>,
}

/// Whether the subscription's target set is the live `@*` roster, which never
/// ends by its sessions exiting.
fn targets_everything(request: &str) -> bool {
    request.split_whitespace().any(|t| t == "@*")
}

/// Whether `request` names every session it follows by id, as a pinned resume
/// does ([`resume_request`]), rather than `@*` or a self target.
fn names_its_sessions(request: &str) -> bool {
    let mut pieces = request
        .split_whitespace()
        .filter(|token| token.starts_with('@'))
        .flat_map(|token| token.split(','))
        .peekable();
    pieces.peek().is_some() && pieces.all(|piece| piece != "@*" && !is_self_selector(piece))
}

/// Follows the push stream's FRAMING just far enough to know when every
/// subscribed session has said `EVENT <local> exited` — the stream's own
/// end-of-session marker. Header lines are parsed; the bodies that follow some
/// of them (a `screen` DELTA's rows, a `cells` DELTA's bytes, a `BYTES` frame)
/// are skipped by count, so a body that happens to read `EVENT 1 exited` is
/// never mistaken for a frame. Anything it cannot frame makes it `lost`, which
/// only ever means "cannot say the session is over".
///
/// It is also what the stream passes through on its way to the consumer
/// ([`FrameTracker::relay`]): a line is written once it is whole, a body as it
/// comes, and a forward's host-gone line
/// ([`aterm_types::control_verbs::host_gone_line`]) — the relay's own word at
/// a header's place, not a frame — is kept back and remembered.
#[derive(Debug, Default)]
pub(crate) struct FrameTracker {
    line: Vec<u8>,
    skip_lines: u64,
    skip_bytes: u64,
    /// `(local, sid, exited)` per `sub` line seen on the current leg.
    targets: Vec<(String, String, bool)>,
    everything: bool,
    lost: bool,
    /// The newest block and turn ids relayed, kept across legs.
    anchors: EventAnchors,
    /// The host a forward said went away under this leg: its pid and the
    /// socket it was reached at.
    gone: Option<(u32, String)>,
}

/// The longest header line the tracker keeps; a longer one is not a frame
/// header this client knows.
const MAX_HEADER: usize = 4096;

impl FrameTracker {
    /// A tracker for the stream `request` opened.
    pub(crate) fn new(request: &str) -> Self {
        Self {
            everything: targets_everything(request),
            ..Self::default()
        }
    }

    /// A redialled leg starts a new stream: new `sub` lines, new locals. The
    /// event anchors carry over only when the leg relays the same session; on
    /// another ([`LegSession::Other`], a fresh successor's own session) they
    /// are dropped so the next resume cannot pin them to the wrong session.
    pub(crate) fn start_leg(&mut self, session: LegSession) {
        let everything = self.everything;
        let anchors = match session {
            LegSession::Same => self.anchors,
            LegSession::Other => EventAnchors::default(),
        };
        *self = Self {
            everything,
            anchors,
            ..Self::default()
        };
    }

    /// The newest `block-complete` and `turn` ids relayed so far, every leg.
    pub(crate) fn anchors(&self) -> EventAnchors {
        self.anchors
    }

    /// Whether the stream said every one of its sessions exited.
    pub(crate) fn session_over(&self) -> bool {
        !self.lost
            && !self.everything
            && !self.targets.is_empty()
            && self.targets.iter().all(|(_, _, exited)| *exited)
    }

    /// `(local, sid)` per `sub` line of the current leg, so a redial can name
    /// by sid a target the request named by number ([`resume_request`]).
    pub(crate) fn sids(&self) -> Vec<(String, String)> {
        self.targets
            .iter()
            .map(|(local, sid, _)| (local.clone(), sid.clone()))
            .collect()
    }

    /// Account for `bytes` of the stream, relaying nothing (tests).
    #[cfg(test)]
    pub(crate) fn feed(&mut self, bytes: &[u8]) {
        self.relay(bytes, &mut io::sink())
            .expect("a sink takes every write");
    }

    /// Account for `bytes` of the stream and write them on to `out`: a body
    /// as it comes, a line once it is whole — the host-gone line excepted,
    /// which is remembered ([`FrameTracker::take_host_gone`]), never relayed.
    /// What cannot be framed goes on as it came.
    pub(crate) fn relay(&mut self, mut bytes: &[u8], out: &mut impl io::Write) -> io::Result<()> {
        while !bytes.is_empty() {
            if self.lost {
                return out.write_all(bytes);
            }
            if self.skip_bytes > 0 {
                let n = usize::try_from(self.skip_bytes)
                    .unwrap_or(usize::MAX)
                    .min(bytes.len());
                out.write_all(&bytes[..n])?;
                self.skip_bytes -= n as u64;
                bytes = &bytes[n..];
                continue;
            }
            let Some(nl) = bytes.iter().position(|&b| b == b'\n') else {
                self.line.extend_from_slice(bytes);
                if self.line.len() > MAX_HEADER {
                    self.lost = true;
                    out.write_all(&std::mem::take(&mut self.line))?;
                }
                return Ok(());
            };
            self.line.extend_from_slice(&bytes[..=nl]);
            bytes = &bytes[nl + 1..];
            let line = std::mem::take(&mut self.line);
            if self.skip_lines > 0 {
                self.skip_lines -= 1;
                out.write_all(&line)?;
                continue;
            }
            let text = String::from_utf8_lossy(&line[..line.len() - 1]);
            if let Some((pid, sock)) = aterm_types::control_verbs::host_gone(&text) {
                self.gone = Some((pid, sock.to_string()));
                continue;
            }
            out.write_all(&line)?;
            self.header(&text);
        }
        Ok(())
    }

    /// At a leg's end: write on the part of a line the stream ended inside,
    /// as it came.
    pub(crate) fn finish(&mut self, out: &mut impl io::Write) -> io::Result<()> {
        out.write_all(&std::mem::take(&mut self.line))
    }

    /// The host a forward said went away under this leg, once.
    pub(crate) fn take_host_gone(&mut self) -> Option<(u32, String)> {
        self.gone.take()
    }

    fn header(&mut self, line: &str) {
        let tokens: Vec<&str> = line.split_whitespace().collect();
        match tokens.as_slice() {
            ["sub", local, sid, ..] => {
                self.targets
                    .push(((*local).to_string(), (*sid).to_string(), false));
            }
            ["EVENT", local, "exited", ..] => {
                for (known, _, exited) in &mut self.targets {
                    if known == local {
                        *exited = true;
                    }
                }
            }
            ["EVENT", _, "block-complete", id, ..] => {
                if let Ok(id) = id.parse::<u64>() {
                    self.anchors.block = self.anchors.block.max(Some(id));
                }
            }
            ["EVENT", _, "turn", id, ..] => {
                if let Ok(id) = id.parse::<u64>() {
                    self.anchors.turn = self.anchors.turn.max(Some(id));
                }
            }
            ["DELTA", _, _, "screen", rows, ..] => match rows.parse::<u64>() {
                Ok(rows) => self.skip_lines = rows,
                Err(_) => self.lost = true,
            },
            ["DELTA", _, _, "cells", len, ..] | ["BYTES", _, len, ..] => match len.parse::<u64>() {
                // The body, then its trailing newline.
                Ok(len) => self.skip_bytes = len.saturating_add(1),
                Err(_) => self.lost = true,
            },
            _ => {}
        }
    }
}

/// The one refusal a pinned self stream is asked again after: the successor
/// does not hold the session the pin names.
const NO_SUCH_SESSION: &str = "ERR no such session";

/// Which session a redialled leg relays, against the one the stream followed —
/// what [`FrameTracker::start_leg`] keeps across it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum LegSession {
    /// The same one: the successor carried it (an update), so the event
    /// anchors go on.
    Same,
    /// A fresh successor's own session ([`after_refusal`]): the anchors were
    /// the lost session's ids.
    Other,
}

/// One request put to a successor, and the session its leg relays when the
/// successor accepts it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Ask {
    pub(crate) request: String,
    pub(crate) session: LegSession,
}

/// What a successor lacking the stream's session is known to be: where it
/// answers and when it started relative to the hang-up, which tells a
/// relaunch of the old instance from another.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Arrival {
    /// On the fixed socket the call named, started after the hang-up: a
    /// relaunch of the instance configured there (after a crash or quit). It
    /// holds none of the old sessions, or with `[keeper]` on only those the
    /// keeper recovered under their old sids (not the stream's, or it would
    /// have taken the pinned request).
    Fresh,
    /// On the fixed socket and already running when the old server went away
    /// ([`HangupAt::moment`]): the update's own successor, whose copy of the
    /// stream's session has since closed. Or a relaunch within a moment of the
    /// exit (before the watch woke, or before the relay read the hang-up, or on
    /// Linux within the same clock tick). Each miss errs toward ending the
    /// stream.
    Predates,
    /// On the fixed socket; its start could not be read ([`process_started`]:
    /// no start record on this platform, another uid's process, or a zombie),
    /// or cannot be ordered against the hang-up ([`later`]).
    Unknown,
    /// Not on a fixed `--sock` socket ([`only_a_relaunch_binds`]): the
    /// `latest` alias, a link, an instance's own socket, or a session's graph
    /// entry. Another instance can answer there, so when it started proves
    /// nothing.
    Shared,
}

/// The [`Arrival`] of a successor on the fixed socket that started at
/// `started`, for a stream whose server went away at `hung_up`
/// ([`HangupAt::moment`]), ordered by [`later`]. `hung_up` is never earlier
/// than the exit and a start is never read later than it was, so errors lean
/// toward [`Arrival::Predates`], which ends the stream rather than moving it.
/// An unreadable or unorderable start is [`Arrival::Unknown`].
pub(crate) fn arrival_of(started: Option<Stamp>, hung_up: Stamp) -> Arrival {
    match started.and_then(|at| later(at, hung_up)) {
        Some(true) => Arrival::Fresh,
        Some(false) => Arrival::Predates,
        None => Arrival::Unknown,
    }
}

/// When the server a leg was on went away, against which a successor's start
/// is ordered ([`arrival_of`]).
///
/// The kernel's exit report, not the relay's read: a relay blocked writing to
/// a slow reader reads the EOF late, and a supervisor's quick restart would
/// then look like it predates the hang-up. The exit is watched from the moment
/// the relay holds a stream ([`ExitMoment`]: kqueue `NOTE_EXIT` on macOS, a
/// `pidfd` on Linux) on a dedicated thread.
pub(crate) struct HangupAt {
    /// When this relay read the hang-up (an EOF, a reset, a failed send).
    pub(crate) seen: Stamp,
    /// The server's exit, stamped as the kernel reported it — `None` where
    /// it could not be watched.
    pub(crate) exit: Option<ExitMoment<Stamp>>,
}

impl HangupAt {
    /// The earlier of [`HangupAt::seen`] and the stamped exit (both are at or
    /// after the server's end). The read alone when there is no exit stamp or
    /// the two cannot be ordered.
    pub(crate) fn moment(&self) -> Stamp {
        earliest(self.seen, self.exit.as_ref().and_then(ExitMoment::get))
    }
}

/// [`HangupAt::moment`], of its two readings.
pub(crate) fn earliest(seen: Stamp, exited: Option<Stamp>) -> Stamp {
    match exited {
        Some(exited) if later(seen, exited) == Some(true) => exited,
        _ => seen,
    }
}

/// Whether a status line is the control socket's busy refusal
/// ([`aterm_types::control_verbs::CONTROL_BUSY_REPLY`]): nothing was served,
/// and the request may be sent again.
fn is_busy(status: &str) -> bool {
    status.starts_with("ERR control server busy")
}

/// Why a successor's refusal ends the relay with [`EXIT_REPLACED`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Unresumed {
    /// Nothing else to ask: a refusal other than [`NO_SUCH_SESSION`], a stream
    /// that named its sessions, or the self fallback itself refused.
    Refused,
    /// The successor answered busy, or closed on the ask without a word while
    /// it still answered ([`answers_at`]), to every ask while the successor
    /// wait lasted.
    Busy,
    /// The successor lacks the self stream's session and did not start after
    /// the old server's end ([`Arrival::Predates`]).
    Predates,
    /// The successor lacks the self stream's session, and when it started
    /// cannot be read or ordered against that end ([`Arrival::Unknown`]).
    Unknown,
    /// The successor lacks the self stream's session and answers where any
    /// aterm may ([`Arrival::Shared`]).
    Shared,
}

/// What a relay does when a successor refused `asked` with `status`: ask the
/// same successor once more, or end. `fallback` is the original request's self
/// target as written ([`resume_lines`]).
///
/// A pinned self stream is asked again unpinned only by a fresh successor. `ERR
/// no such session` says the successor lacks the session. A relaunch on the
/// fixed socket the call named does not hold it, so the stream continues with
/// that instance's own session. One already running at the hang-up is an
/// update successor that lost the tab, and one on the `latest` alias may be
/// another window's; self there is another tab, so the stream ends with
/// [`EXIT_REPLACED`], as it does when the start cannot be read. A fallback leg
/// relays [`LegSession::Other`], and a refused fallback is never followed by a
/// third ask.
pub(crate) fn after_refusal(
    asked: &Ask,
    status: &str,
    fallback: Option<String>,
    arrival: Arrival,
) -> Result<Ask, Unresumed> {
    if is_busy(status) {
        return Err(Unresumed::Busy);
    }
    if status != NO_SUCH_SESSION || asked.session == LegSession::Other {
        return Err(Unresumed::Refused);
    }
    let Some(request) = fallback.filter(|f| *f != asked.request) else {
        return Err(Unresumed::Refused);
    };
    match arrival {
        Arrival::Fresh => Ok(Ask {
            request,
            session: LegSession::Other,
        }),
        Arrival::Predates => Err(Unresumed::Predates),
        Arrival::Unknown => Err(Unresumed::Unknown),
        Arrival::Shared => Err(Unresumed::Shared),
    }
}

/// The self target of `request` as the caller wrote it, resolved on a fresh
/// successor — never `@.` for a bare `@` or a request that named none.
fn self_resolved(request: &str) -> &'static str {
    match request.split_whitespace().find(|t| t.starts_with('@')) {
        Some("@.") => "`@.` resolved there",
        Some("@") => "`@` resolved there",
        _ => "the request names no session, and it resolved there",
    }
}

/// What one ask of a successor came to ([`ask_once`]).
pub(crate) enum Answer {
    /// The status line: `OK subscribe …` or a refusal (`ERR control server
    /// busy; retry`, `ERR no such session`); empty when the read failed some
    /// other way or the connection closed after the request.
    Line(String),
    /// The request could not be written and the successor left no word behind:
    /// it went away between the probe and the ask.
    Gone,
}

/// Put `request` to the successor `stream` is connected to and read its status
/// line through `reader`.
///
/// A failed write is not the successor going away: a listener with no lane
/// free writes `ERR control server busy; retry` and closes without reading, so
/// the line it left is the answer ([`super::pending_status_line`], as
/// `send_served` reads it for the first request).
pub(crate) fn ask_once(
    stream: &CtlStream,
    reader: &mut BufReader<&CtlStream>,
    token: Option<&str>,
    request: &str,
) -> Answer {
    if super::send_request(stream, token, request).is_err() {
        return match super::pending_status_line(stream) {
            Some(line) => Answer::Line(line),
            None => Answer::Gone,
        };
    }
    let mut status = String::new();
    match super::read_bounded_line(reader, &mut status) {
        Ok(n) if n > 0 => Answer::Line(status.trim_end_matches(['\r', '\n']).to_string()),
        _ => Answer::Line(String::new()),
    }
}

/// The stderr note for a leg a successor accepted: `old` hung up, `pid` at
/// `path` took the stream with `asked`; `original` is the caller's request.
///
/// The successor is described only as far as `arrival` ([`Redial::arrival`])
/// knows it. [`Arrival::Predates`] is the update's own successor.
/// [`Arrival::Fresh`] is a relaunch, which holds the stream's session only if
/// its keeper recovered it, with block and turn counts restarted, so some
/// events in between may not be pushed; a relaunch that took a request naming
/// no session ([`names_its_sessions`]) is just called a relaunch, and its
/// events start from its newest. [`Arrival::Unknown`] and [`Arrival::Shared`]
/// are described only as a replaced aterm.
pub(crate) fn resumed_note(
    old: u32,
    pid: u32,
    path: &str,
    original: &str,
    asked: &Ask,
    arrival: Arrival,
) -> String {
    match asked.session {
        LegSession::Same => {
            let (cause, events) = match arrival {
                Arrival::Predates => (", an update", events_after(&asked.request)),
                Arrival::Fresh if !names_its_sessions(&asked.request) => (
                    ", a relaunch",
                    "its block and turn events start from the relaunch's newest",
                ),
                Arrival::Fresh => (
                    ", a relaunch that recovered the session",
                    "its block and turn events start over with the relaunch's own count, so \
                     some between the two may not have been delivered",
                ),
                Arrival::Unknown | Arrival::Shared => ("", events_after(&asked.request)),
            };
            format!(
                "{}; the stream resumed on {path}, and output, mail and records between the \
                 two were not delivered (`GAP <local> replaced` on each target); {events}",
                replaced_words(old, pid, cause)
            )
        }
        LegSession::Other => format!(
            "aterm was replaced (pid {old} -> {pid}, a relaunch without this stream's session); \
             the stream resumed on {path} with that instance's own session ({}), and output, \
             mail and records between the two were not delivered (`GAP <local> replaced` on \
             each target); its block and turn events start from that session's newest",
            self_resolved(original)
        ),
    }
}

/// What a resumed leg's block and turn events go on from: after the last ones
/// relayed where `request` — the resumed one — carries that anchor
/// ([`resume_request`]); else from the successor's newest, so any that
/// completed between the legs are not pushed (a multi-target stream carries
/// none).
fn events_after(request: &str) -> &'static str {
    let carries = |key: &str| request.split_whitespace().any(|t| t.starts_with(key));
    match (carries("since-block="), carries("since-turn=")) {
        (true, true) => "its block and turn events go on after the last ones relayed",
        (true, false) => {
            "its block events go on after the last one relayed, and its turn events start \
             from the successor's newest"
        }
        (false, true) => {
            "its turn events go on after the last one relayed, and its block events start \
             from the successor's newest"
        }
        (false, false) => "its block and turn events start from the successor's newest",
    }
}

/// How a note names the server `old` giving way to `pid` — `cause` said of a
/// different process. The SAME pid is that process back on a fresh listening
/// socket ([`ListenerId`]): a cold update's `exec` keeps the pid, and an `exec`
/// that returned reopens the socket. The two are not told apart here, so the
/// note says only what happened.
pub(crate) fn replaced_words(old: u32, pid: u32, cause: &str) -> String {
    if pid == old {
        format!("aterm (pid {pid}) reopened its control socket")
    } else {
        format!("aterm was replaced (pid {old} -> {pid}{cause})")
    }
}

/// The stderr note for a successor that did not take the stream over.
pub(crate) fn unresumed_note(old: u32, pid: u32, path: &str, why: Unresumed) -> String {
    match why {
        Unresumed::Refused => format!(
            "{}, and {} did not resume this subscription; re-issue it",
            replaced_words(old, pid, ""),
            if pid == old { "it" } else { "its successor" }
        ),
        Unresumed::Busy => format!(
            "{}, and {} stayed too busy to resume this subscription; re-issue it",
            replaced_words(old, pid, ""),
            if pid == old { "it" } else { "its successor" }
        ),
        Unresumed::Predates | Unresumed::Unknown | Unresumed::Shared if pid == old => format!(
            "aterm (pid {pid}) reopened its control socket without the session this stream \
             followed; re-issue it"
        ),
        Unresumed::Predates | Unresumed::Unknown | Unresumed::Shared => format!(
            "aterm (pid {old}) went away; {path} now answers as pid {pid}, which does not hold \
             the session this stream followed; re-issue it"
        ),
    }
}

/// How one leg of a relayed stream ended.
enum Leg {
    /// The server hung up (EOF, reset), read at this moment ([`HangupAt::seen`]).
    HungUp(Stamp),
    /// The explicit `--timeout` wall clock ran out.
    Timeout,
}

/// Relay one connection's frames to `out` until the server hangs up or the
/// `watch` wall clock (an explicit `--timeout`) runs out. Each frame is flushed
/// as it arrives. A hang-up is read on `clock`.
fn pump(
    stream: &CtlStream,
    reader: &mut BufReader<&CtlStream>,
    out: &mut impl io::Write,
    tracker: &mut FrameTracker,
    watch: Option<(Instant, Duration)>,
    clock: fn() -> Stamp,
) -> io::Result<Leg> {
    let poll = Duration::from_millis(250);
    // Ignored on failure: macOS answers `setsockopt` on a socket whose peer is
    // gone with EINVAL, and such a socket's next read returns the buffered
    // frames and then EOF without waiting.
    let _ = stream.set_read_timeout(watch.map(|_| poll));
    let mut buf = [0u8; 8192];
    loop {
        if let Some((start, limit)) = watch
            && start.elapsed() >= limit
        {
            tracker.finish(out)?;
            out.flush()?;
            return Ok(Leg::Timeout);
        }
        match reader.read(&mut buf) {
            Ok(0) => {
                // Read at once: the flush can block on a slow consumer.
                let at = clock();
                tracker.finish(out)?;
                out.flush()?;
                return Ok(Leg::HungUp(at));
            }
            Ok(n) => {
                tracker.relay(&buf[..n], out)?;
                out.flush()?;
            }
            Err(ref e) if super::is_timeout_error(e) && watch.is_some() => {}
            Err(ref e) if is_hangup(e) => {
                let at = clock();
                tracker.finish(out)?;
                out.flush()?;
                return Ok(Leg::HungUp(at));
            }
            Err(e) => return Err(e),
        }
    }
}

/// The first leg of a `subscribe` relay, already past its `OK subscribe` ack.
pub(crate) struct FirstLeg<'a, 'b> {
    pub(crate) stream: &'b CtlStream,
    pub(crate) reader: &'a mut BufReader<&'b CtlStream>,
    /// The path it was dialled at, resolved.
    pub(crate) dialed: &'a str,
    /// The server's pid, when the platform can say.
    pub(crate) server: Option<u32>,
    /// The listening socket it was accepted through ([`listener_at`], read
    /// once connected).
    pub(crate) listener: Option<ListenerId>,
}

/// Relay a `subscribe` push stream to `out` across server replacements (the
/// module rule). `watch` is the explicit `--timeout` wall clock over the whole
/// relay, redials included.
pub(crate) fn relay_subscription(
    first: FirstLeg<'_, '_>,
    request: &str,
    watch: Option<Duration>,
    redial: &Redial,
    out: &mut impl io::Write,
) -> io::Result<ExitCode> {
    let watch = watch.map(|limit| (Instant::now(), limit));
    // The wall clock the successor wait may not outlast (the rule on this
    // module): a hang-up is inside the watch, never added to it.
    let until = watch.map(|(start, limit)| start + limit);
    // What a hang-up follows: the call's own policy, until a forward says the
    // host behind it went away ([`Redial::host`]).
    let mut redial = redial.clone();
    let mut tracker = FrameTracker::new(request);
    // The server answered this stream's request a moment ago: watch for its
    // exit now, while it can still be registered ([`HangupAt`]).
    let exit = redial.watch_exit(first.server);
    let mut hung_up = match pump(
        first.stream,
        first.reader,
        out,
        &mut tracker,
        watch,
        redial.clock,
    )? {
        Leg::Timeout => return Ok(ExitCode::from(super::EXIT_TIMEOUT)),
        Leg::HungUp(seen) => HangupAt { seen, exit },
    };
    // Until when a successor that turns the resume away as busy, or closes on
    // it without a word, is asked again: the successor wait from the last
    // leg's hang-up.
    let mut ask_until = Wait::from_now(redial.bound, until).deadline;
    // What the next leg asks for: the original, then each resumed form — whose
    // local targets are already sids, so a later leg's new numbers never matter.
    // The original stays at hand: a self stream is one on every leg, and its
    // fallback names self as the caller wrote it ([`after_refusal`]).
    let original = request;
    let mut request = request.to_string();
    let mut dialed = first.dialed.to_string();
    let mut server = first.server;
    let mut listener = first.listener;
    // A live successor that was asked and never took this stream, as `(the
    // server it replaced, it, where it answers)`, while `server` names it: its
    // still answering does not mean it ended the stream on purpose
    // ([`Hangup::StillServing`]).
    let mut never_took: Option<(u32, u32, String)> = None;
    'leg: loop {
        if tracker.session_over() {
            return Ok(ExitCode::SUCCESS);
        }
        // A FORWARD'S HOST THAT WENT AWAY said so as the leg's last line: the
        // server dialled only relayed this stream to the aterm hosting its
        // session, and that aterm hung up. Its still answering is no end on
        // purpose — the relay already looked — and from here the stream follows
        // that host's successor, not the relay.
        let hangup = match tracker.take_host_gone() {
            Some((pid, sock)) => {
                let parts: Vec<&str> = request.split_whitespace().collect();
                redial = redial.host(&sock, target_sid(&parts));
                server = Some(pid);
                listener = None;
                dialed = sock;
                redial.after_host_gone(&dialed, pid, until)
            }
            None => redial.after_hangup(&dialed, server, listener, until),
        };
        // The local server only RELAYS a `dial`: still serving, it did not end
        // the stream — the connection to the remote closed under it, and the
        // stream never said its sessions exited.
        let parts: Vec<&str> = request.split_whitespace().collect();
        if hangup == Hangup::StillServing
            && let Some(note) = remote_closed_note_for(&parts, true)
        {
            super::stderr_line(&note)?;
            return Ok(ExitCode::from(EXIT_REPLACED));
        }
        // A live successor that closed on the ask it was given: the stream
        // is not over, and it did not go on — exit 75 and a note, never 0.
        if hangup == Hangup::StillServing
            && let Some((old, pid, path)) = never_took.take()
        {
            super::stderr_line(&unresumed_note(old, pid, &path, Unresumed::Refused))?;
            return Ok(ExitCode::from(EXIT_REPLACED));
        }
        let (path, pid) = match hangup {
            // An end on purpose, or no peer pid to tell otherwise.
            Hangup::StillServing | Hangup::Unknown => return Ok(ExitCode::SUCCESS),
            Hangup::Gone {
                waited,
                by_deadline,
            } => {
                super::stderr_line(&gone_note(server, waited, by_deadline, redial.unfollowed()))?;
                return Ok(ExitCode::from(EXIT_REPLACED));
            }
            Hangup::Successor { path, pid } => (path, pid),
        };
        let old = server.unwrap_or(0);
        let Some((resumed, mut fallback)) =
            resume_lines(original, &request, &tracker.sids(), &tracker.anchors())
        else {
            let target =
                untied_local_target(&request, &tracker.sids()).unwrap_or_else(|| "@<n>".to_owned());
            super::stderr_line(&format!(
                "{}; `{target}` cannot follow it — re-issue the subscription by sid (exit \
                 {EXIT_REPLACED})",
                replaced_words(old, pid, "")
            ))?;
            return Ok(ExitCode::from(EXIT_REPLACED));
        };
        // The resumed request first — for a self stream, pinned to the session
        // it followed; then, only as [`after_refusal`] allows, self as written.
        let mut ask = Ask {
            request: resumed,
            session: LegSession::Same,
        };
        loop {
            let Ok((stream, token)) = super::connect_then_read(&path, super::read_token_for) else {
                // It answered a moment ago; look again from here.
                dialed = path;
                continue 'leg;
            };
            // Judge the process actually asked: each ask is a new connection to
            // `path`, and another process may have taken it since the probe
            // named `pid` (an alias repointed, a socket rebound). Look again.
            if server_pid(&stream) != Some(pid) {
                continue 'leg;
            }
            let leg_listener = listener_at(&path);
            // The process asked is alive and connected: watch for its exit,
            // which the leg it may open is judged by ([`HangupAt`]).
            let exit = redial.watch_exit(Some(pid));
            // Bounded handshake — by the watch's remainder too; best effort for
            // the same EINVAL reason as `pump`.
            let handshake = until.map_or(SUCCESSOR_BOUND, |until| {
                until
                    .saturating_duration_since(Instant::now())
                    .clamp(PROBE_FLOOR, SUCCESSOR_BOUND)
            });
            let _ = stream.set_write_timeout(Some(handshake));
            let _ = stream.set_read_timeout(Some(handshake));
            let mut reader = BufReader::new(&stream);
            let answer = ask_once(&stream, &mut reader, token.as_deref(), &ask.request);
            // A RETRY SIGNAL IS RETRIED: right after a Commit every client
            // re-dials the successor at once, and a listener with no lane free
            // answers `ERR control server busy; retry` and drops the
            // connection unread — the line can go with the unread request,
            // leaving no word at all. Either, from a successor that still
            // answers, is asked again while the wait lasts. A SILENT CLOSE IS
            // JUDGED BY ANY ANSWER ([`answers_at`]): the same process through
            // the same listener, `OK` or busy — by `OK` alone a saturated
            // successor would read as gone.
            let busy = matches!(&answer, Answer::Line(line) if is_busy(line));
            let silent = matches!(&answer, Answer::Gone)
                || matches!(&answer, Answer::Line(line) if line.is_empty());
            let live = busy || (silent && answers_at(&path, pid, leg_listener));
            if live && Instant::now() < ask_until {
                std::thread::sleep(SUCCESSOR_POLL);
                continue;
            }
            // Turned away until the wait ran out, by a successor still there.
            if silent && live {
                super::stderr_line(&unresumed_note(old, pid, &path, Unresumed::Busy))?;
                return Ok(ExitCode::from(EXIT_REPLACED));
            }
            let status = match answer {
                Answer::Line(line) if !silent => line,
                // Nothing taken, nothing left behind, and nothing answers there
                // now: the successor went away too. The next is judged against
                // ITS hang-up, which is now (or its exit, when the kernel has
                // reported it): judged against the first one, an update
                // successor of this one would read as fresh. It never took this
                // stream: should it still be answering, the relay ends 75
                // ([`never_took`]).
                Answer::Line(_) | Answer::Gone => {
                    never_took = Some((old, pid, path.clone()));
                    dialed = path;
                    server = Some(pid);
                    listener = leg_listener;
                    hung_up = HangupAt {
                        seen: (redial.clock)(),
                        exit,
                    };
                    continue 'leg;
                }
            };
            let status = status.as_str();
            if !status.starts_with("OK") {
                let arrival = redial.arrival(pid, &path, hung_up.moment());
                let why = match after_refusal(&ask, status, fallback.take(), arrival) {
                    Ok(next) => {
                        ask = next;
                        continue;
                    }
                    Err(why) => why,
                };
                // A busy refusal's own "retry" is not echoed: the note's
                // "re-issue it" is the one remedy.
                if !status.is_empty() && why != Unresumed::Busy {
                    super::stderr_line(status)?;
                }
                super::stderr_line(&unresumed_note(old, pid, &path, why))?;
                return Ok(ExitCode::from(EXIT_REPLACED));
            }
            super::stderr_line(status)?;
            let arrival = redial.arrival(pid, &path, hung_up.moment());
            super::stderr_line(&resumed_note(old, pid, &path, original, &ask, arrival))?;
            // THE HOLE BETWEEN THE LEGS IS MARKED ON THE STREAM: the
            // successor's watch starts live, so what came between the two — PTY
            // bytes its readers drained before this resume, mail, timeline
            // records — was never pushed. Each target the last leg named gets
            // the stream's own loss marker, under the number that leg gave it,
            // before any frame of the next.
            for (local, _) in tracker.sids() {
                out.write_all(format!("GAP {local} replaced\n").as_bytes())?;
            }
            out.flush()?;
            never_took = None;
            tracker.start_leg(ask.session);
            request = ask.request;
            // Each leg's successor is judged against that leg's own hang-up, so
            // a stream that rode an update to S1 and then lost S1 does not take
            // S1's update successor for a relaunch.
            hung_up = match pump(&stream, &mut reader, out, &mut tracker, watch, redial.clock)? {
                Leg::Timeout => return Ok(ExitCode::from(super::EXIT_TIMEOUT)),
                Leg::HungUp(seen) => HangupAt { seen, exit },
            };
            ask_until = Wait::from_now(redial.bound, until).deadline;
            dialed = path;
            server = Some(pid);
            listener = leg_listener;
            continue 'leg;
        }
    }
}

/// The note for a hang-up nothing replaced ([`Hangup::Gone`]'s fields, and
/// [`Redial::unfollowed`]: the one reason a call that follows nothing gives).
pub(crate) fn gone_note(
    server: Option<u32>,
    waited: Duration,
    by_deadline: bool,
    unfollowed: Option<&Unfollowed>,
) -> String {
    // A `--pid` call names its process even when no hello said it.
    let pinned = match unfollowed {
        Some(Unfollowed::PidPin(pin)) => Some(*pin),
        _ => None,
    };
    let pid = server.or(pinned).unwrap_or(0);
    let within = if by_deadline {
        format!("the {:.1} s its --timeout left", waited.as_secs_f64())
    } else {
        format!("{:.1} s", waited.as_secs_f64())
    };
    let why = match unfollowed {
        // The pinned process lived on through the wait for it to answer again
        // ([`Redial::after_hangup`]; one that ended is gone with no wait).
        Some(Unfollowed::PidPin(_)) if !waited.is_zero() => {
            format!(
                " and did not answer again within {within}; a --pid call follows no other instance"
            )
        }
        Some(Unfollowed::PidPin(_)) => "; a --pid call follows no other instance".to_owned(),
        Some(Unfollowed::AskedOnce) => "; this was already the one retry".to_owned(),
        Some(Unfollowed::NoRendezvousDir) => {
            "; with $HOME and $XDG_RUNTIME_DIR unset there is nowhere to find a successor"
                .to_owned()
        }
        None => format!(" and nothing replaced it within {within}"),
    };
    format!(
        "aterm (pid {pid}) went away{why}; the exchange cannot be resumed (exit {EXIT_REPLACED})"
    )
}

#[cfg(test)]
#[path = "redial_tests.rs"]
mod tests;
