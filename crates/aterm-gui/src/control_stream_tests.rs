// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The record stream, driven over real control socket pairs through the shipping lanes: the
//! fleet's verb gate and the verb's own gate in the order the request loop runs them
//! ([`super::super::fleet_refusal`], [`super::super::stream_verb`]), the flip to a stream lane,
//! and the reader ([`super::serve`]). The service here answers a `claim` line by placing a
//! fixture claim on the connection that sent it, serves a `claim` record's line through the
//! shipping `meta set supervisor` with stand-in kernel proofs, and leaves every stream decision
//! to the shipping code. The request loop itself (`serve_request_line`) needs a window's event loop and
//! runs in no headless test.

use std::io::{BufRead, BufReader, Write};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use alab_wire::record::{Body, LINE_MAX, RATE_BURST, Record, State, StreamReply};
use aterm_agent::harness::contract::{self, Scope as Duty};
use aterm_uds::CtlStream;

use super::super::control_lanes::{LaneLimits, LaneReader, LaneService, Lanes, Outbox};
use super::super::{Principal, Request, Scope, ServeDisposition};
use crate::harness_claim::{Proven, Scoped};
use crate::session_store::{SessionHandle, Store};

const SETTLE: Duration = aterm_spec::wait::MUST_HAPPEN;

/// The two sessions the tests host.
const ONE: &str = "t0000000001";
const TWO: &str = "t0000000002";
/// A claim's child pid the stand-in kernel reads keep nothing of: the claim stands without it.
const UNKEPT: u32 = 4999;
/// A session no test hosts.
const ELSEWHERE: &str = "t00000000zz";

fn session(local: u64, sid: &str) -> SessionHandle {
    crate::session_store::test_handle_identified(
        local,
        aterm_session::SessionId::parse(sid).expect(sid),
        aterm_session::LaunchNonce::generate(),
        Arc::new(crate::session_log::Epochs::new()),
        aterm_slog::Stamp::new(1_792_000_000_000, 1_000),
    )
}

/// The claimant every `claim` line proves, unless it says `other`.
fn running() -> Proven {
    crate::self_claim::stand_in::running()
}

/// Another claimant: the same pid under another kernel start.
fn other() -> Proven {
    Proven {
        start: "Thu Jan  1 00:00:00 1970".to_string(),
        ..running()
    }
}

/// Place a claim on `handle`'s session, bound to connection `conn`: contract 2 is codex-alab's
/// proven self-claim, 3 and 4 a registered harness's scoped claim of `duties` proven as `proven`,
/// under the registry row `row`.
fn place(
    handle: &SessionHandle,
    contract_no: u32,
    duties: &[Duty],
    conn: u64,
    proven: Proven,
    row: &str,
) -> Result<(), String> {
    place_as(handle, contract_no, duties, conn, proven, row, None)
}

/// [`place`] for a claim that states `login=<login>`, the tab's own account.
fn place_as(
    handle: &SessionHandle,
    contract_no: u32,
    duties: &[Duty],
    conn: u64,
    proven: Proven,
    row: &str,
    login: Option<&str>,
) -> Result<(), String> {
    let now = crate::metrics::now_us();
    if contract_no == 2 {
        let holder = format!("codex-alab@{}", std::process::id());
        return crate::session_timeline::claim_supervisor_as(
            &handle.ctx,
            &holder,
            Some(conn),
            None,
            Some(proven),
            now,
            aterm_slog::Actor::Ctl,
        )
        .map(drop);
    }
    let holder = format!("{row}@{}", proven.pid);
    let contract::Words::Scoped(mut claim) =
        contract::read(&holder, &["contract=3", "scope=upgrade", "version=0.3.0"])
    else {
        panic!("the roster admits {holder}");
    };
    claim.contract = contract_no;
    claim.login = login.map(str::to_string);
    claim.scopes = contract::Scopes::for_test(duties);
    if !duties.is_empty() {
        // What it asked for is what it was granted; a grant of no duty keeps the read's ask.
        claim.claimed = claim.scopes.words();
    }
    crate::session_timeline::claim_supervisor_scoped(
        &handle.ctx,
        &holder,
        Some(conn),
        Scoped {
            proven,
            claim,
            child: None,
        },
        now,
        aterm_slog::Actor::Ctl,
    )
    .map(drop)
}

/// What a stream lane had entered for its thread when it called the service.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Entered {
    conn: Option<u64>,
    principal: Principal,
    /// The connection's caller memo is the thread's.
    caller: bool,
    /// The hang-up mark names the stream's own descriptor.
    descriptor: bool,
    direct: bool,
}

/// The lanes' service: the shipping gates and reader over a real store, a frozen clock the test
/// moves, the connections whose claims the lanes released, and what each stream lane entered.
/// It is the world aterm's harness watchdog reads, too ([`crate::harness_watchdog::World`]).
struct Service {
    store: Store,
    clock: AtomicU64,
    /// The awake clock at the machine's last wake, plus one (`0`: none).
    woke: AtomicU64,
    /// What the kernel reports of the claimant's stop state.
    stopped: Mutex<Option<bool>>,
    /// The watchdog's `quiet=`, in milliseconds.
    quiet_ms: AtomicU64,
    /// The policy's `stall_term_after_s`, in milliseconds (`0`: never), which the progress watch
    /// reads: 600 s, the policy's default, until a test says otherwise.
    stall_after_ms: AtomicU64,
    /// The policy's `stall_term`: on, the policy's default, until a test says otherwise.
    stall_term: std::sync::atomic::AtomicBool,
    /// What each watchdog act waited out, in order.
    waited: Mutex<Vec<(crate::harness_watchdog::Act, crate::harness_watchdog::Hand)>>,
    released: Mutex<Vec<u64>>,
    entered: Mutex<Vec<Entered>>,
    /// What the stand-in proof answers a `claim` record's line: the claimant and the version of
    /// the build it was admitted from; `None` proves nothing.
    proof: Mutex<Option<crate::harness_claim::ClaimProof>>,
    /// Every claim line a `claim` record had served, as it was built.
    lines: Mutex<Vec<String>>,
    /// What the land check reads of the claimant's repository and of `sys left`: unread and
    /// unchecked, so nothing agrees, until a test says otherwise.
    repo: Mutex<aterm_agent::harness::repo_work::RepoRead>,
    left: Mutex<aterm_agent::harness::leftover::LeftRead>,
    /// Each relaunch's back-off, in order: waited out by nobody, as the awake clock is the test's.
    paused: Mutex<Vec<Duration>>,
    /// What happens on the tab while a relaunch waits its back-off, once per pause: a test's
    /// stand-in for a person or another ending acting meanwhile.
    meanwhile: Mutex<Option<Box<dyn FnMut() + Send>>>,
    /// What the kept child's snapshot reads as the watchdog ends its harness
    /// ([`crate::harness_watchdog::World::snapshot`]): `None`, nothing.
    snapshot: Mutex<Option<crate::session_timeline::Resume>>,
    /// Each relaunch's words, as the typer was handed them.
    typed: Mutex<Vec<Vec<String>>>,
    /// Each relaunch's directory, as the typer was handed it.
    typed_cwd: Mutex<Vec<Option<String>>>,
    /// What the typer answers, one per line handed to it: past them, it types the line.
    typer: Mutex<std::collections::VecDeque<aterm_agent::harness::relaunch_type::RunLineRefusal>>,
}

/// The version the stand-in proof admits: no claim and no record states it.
const ADMITTED: &str = "9.1.0";

impl Service {
    /// Serve a `claim` record's line as the shipping `meta set supervisor` on the connection
    /// the stream lane entered, with the kernel's proof answered by [`Self::proof`] and every
    /// named child kept as named.
    fn claim_line(&self, handle: &SessionHandle, rest: &str) -> String {
        self.lines
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(rest.to_string());
        self.serve_meta(handle, rest)
    }

    /// `meta <sid> <rest>`, on the connection that sent it: the shipping `meta` verb for that
    /// session, with the same stand-in proofs a `claim` record's line is served under.
    fn meta(&self, line: &str) -> String {
        let (sid, rest) = line["meta ".len()..]
            .split_once(' ')
            .expect("meta <sid> <rest>");
        self.serve_meta(&self.handle(sid), rest)
            .trim_end()
            .to_string()
    }

    fn serve_meta(&self, handle: &SessionHandle, rest: &str) -> String {
        let proof = self
            .proof
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone();
        super::super::meta_wakes_admitting_for_test(
            &self.store,
            &handle.term,
            handle.local_id,
            &handle.ctx,
            rest,
            super::super::serving_connection(),
            &|_, _| proof.clone(),
            &|_, word| {
                (word.pid != UNKEPT).then(|| crate::harness_claim::KeptChild {
                    pid: word.pid,
                    claimed_us: word.start_us,
                    birth: aterm_uds::process::Birth(word.start_us),
                })
            },
        )
        .0
    }

    fn lines(&self) -> Vec<String> {
        self.lines
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    fn handle(&self, sid: &str) -> SessionHandle {
        let sid = super::Sid::parse(sid).expect("a session id");
        super::hosted(&self.store, sid).expect("a hosted session")
    }

    fn released(&self, id: u64) -> bool {
        self.released
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .contains(&id)
    }

    /// `claim <sid> <contract> <duty,…|-> [other] [login=<label>] [<row id>]`, on the connection
    /// that sent it: `other` proves another claimant, `login=` states the tab's account, and a
    /// row id claims under that registry row and not claude-code-alab's.
    fn claim(&self, line: &str) -> String {
        let words: Vec<&str> = line.split(' ').collect();
        let handle = self.handle(words[1]);
        let contract_no: u32 = words[2].parse().expect("a contract number");
        let duties: Vec<Duty> = words[3]
            .split(',')
            .filter(|word| *word != "-")
            .map(|word| Duty::from_word(word).expect("a scope word"))
            .collect();
        let rest = words.get(4..).unwrap_or_default();
        let proven = if rest.contains(&"other") {
            other()
        } else {
            running()
        };
        let login = rest.iter().find_map(|word| word.strip_prefix("login="));
        let row = rest
            .iter()
            .find(|word| **word != "other" && !word.starts_with("login="))
            .copied()
            .unwrap_or("claude-code-alab");
        let conn = super::super::serving_connection().expect("serving a connection");
        super::super::note_connection_claim();
        match place_as(&handle, contract_no, &duties, conn, proven, row, login) {
            Ok(()) => "OK claim".to_string(),
            Err(refusal) => format!("ERR {}", refusal.trim_end()),
        }
    }
}

impl crate::harness_watchdog::World for Service {
    fn awake_ms(&self) -> Option<u64> {
        Some(self.clock.load(Ordering::Acquire))
    }

    fn woke_ms(&self) -> Option<u64> {
        self.woke.load(Ordering::Acquire).checked_sub(1)
    }

    fn stopped(&self, _pid: u32) -> Option<bool> {
        *self.stopped.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// The claimant is the shared stand-in every test proves: the watchdog reads it as gone,
    /// so it never signals it.
    fn alive(&self, _proven: &Proven) -> bool {
        false
    }

    fn quiet(&self) -> Duration {
        Duration::from_millis(self.quiet_ms.load(Ordering::Acquire))
    }

    fn look_again(&self) -> Duration {
        Duration::from_millis(5)
    }

    fn stall_after(&self) -> Option<Duration> {
        match self.stall_after_ms.load(Ordering::Acquire) {
            0 => None,
            ms => Some(Duration::from_millis(ms)),
        }
    }

    fn stall_term(&self) -> bool {
        self.stall_term.load(Ordering::Acquire)
    }

    fn waits(
        &self,
        _sid: &str,
        act: crate::harness_watchdog::Act,
        hand: crate::harness_watchdog::Hand,
    ) {
        self.waited
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push((act, hand));
    }

    fn snapshot(&self, _sid: &str, _pid: u32) -> Option<crate::session_timeline::Resume> {
        self.snapshot
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    fn pause(&self, _sid: &str, pause: Duration, stands: &dyn Fn() -> bool) -> bool {
        self.paused
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(pause);
        if let Some(meanwhile) = self
            .meanwhile
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .as_mut()
        {
            meanwhile();
        }
        stands()
    }

    fn type_run(
        &self,
        _sid: &str,
        words: &[String],
        cwd: Option<&str>,
    ) -> Result<(), aterm_agent::harness::relaunch_type::RunLineRefusal> {
        self.typed
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(words.to_vec());
        self.typed_cwd
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(cwd.map(ToString::to_string));
        match self
            .typer
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .pop_front()
        {
            Some(refusal) => Err(refusal),
            None => Ok(()),
        }
    }
}

impl LaneService for Service {
    fn authenticate(
        &self,
        stream: &CtlStream,
        reader: &mut LaneReader<'_>,
    ) -> Option<(Scope, Principal, Option<String>)> {
        match super::super::read_request_line(reader)?.as_str() {
            "AUTH owner" => Some((Scope::Owner, Principal::Instance, None)),
            "AUTH harness" => Some((Scope::Owner, Principal::Harness, None)),
            "AUTH edge" => Some((
                Scope::Edge(aterm_session::EdgeToken::generate()),
                Principal::Instance,
                None,
            )),
            "AUTH member" => Some((
                Scope::Fleet {
                    content: true,
                    drive: true,
                },
                Principal::Instance,
                None,
            )),
            _ => {
                let mut writer = stream;
                let _ = writer.write_all(b"ERR auth\n");
                None
            }
        }
    }

    fn serve_line(
        &self,
        request: Request,
        scope: Scope,
        _stream: &CtlStream,
        _reader: &mut LaneReader<'_>,
        out: &mut Outbox,
    ) -> Option<ServeDisposition> {
        // As the request loop orders them: the fleet's verb gate, which closes, then the verb.
        if let Some(refusal) = super::super::fleet_refusal(&request, scope) {
            out.extend_from_slice(refusal.as_bytes());
            return Some(ServeDisposition::Close);
        }
        if request.head().verb == "stream" {
            return super::super::stream_verb(request, scope, &self.store, out);
        }
        let reply = match request.line() {
            "who" => format!(
                "OK {}",
                super::super::serving_connection().expect("serving a connection")
            ),
            line if line.starts_with("claim ") => self.claim(line),
            line if line.starts_with("meta ") => self.meta(line),
            line => format!("OK {line}"),
        };
        out.extend_from_slice(format!("{reply}\n").as_bytes());
        None
    }

    fn is_wait(&self, request: &Request, scope: Scope) -> bool {
        super::super::is_wait_request(request, scope, &self.store)
    }

    fn subscribe(&self, _request: Request, _scope: Scope, _stream: CtlStream) {}

    fn stream(&self, request: Request, stream: &CtlStream, buffered: Vec<u8>) {
        use std::os::fd::AsRawFd as _;
        self.entered
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(Entered {
                conn: super::super::serving_connection(),
                principal: super::super::serving_principal(),
                caller: super::super::SERVING_CALLER.with(|caller| caller.borrow().is_some()),
                descriptor: aterm_control::hangup::serving_fd() == Some(stream.as_raw_fd()),
                direct: super::super::SERVING_DIRECT.with(std::cell::Cell::get),
            });
        // The land check runs in line over the service's reads, where the window runs it on a
        // thread of its own, so its mark is made before the record's acknowledgement.
        let repo = |_: &str, _: &Proven| {
            self.repo
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .clone()
        };
        let left = |_: &str| {
            self.left
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .clone()
        };
        super::serve(
            &request,
            &self.store,
            stream,
            buffered,
            &super::Services {
                world: self,
                claims: &|handle, rest| self.claim_line(handle, rest),
                post: &|_| {},
                landed: &|handle, proven| {
                    let posture = self
                        .store
                        .read()
                        .unwrap_or_else(PoisonError::into_inner)
                        .harness_posture();
                    super::finish_landed(
                        handle,
                        proven,
                        &super::LandReads {
                            repo: &repo,
                            left: &left,
                            posture,
                        },
                    );
                },
            },
        );
    }

    fn release(&self, id: u64) {
        super::super::release_connection_claims(&self.store, None, id);
        self.released
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(id);
    }

    fn has_live_claim(&self, id: u64) -> bool {
        super::super::connection_has_live_claim(&self.store, id, crate::metrics::now_us())
    }
}

struct Client {
    stream: CtlStream,
    reader: BufReader<CtlStream>,
    /// The records aterm sent as the stream opened: its `hand`, its `want` where the tab's Out
    /// ring held none of the claimant's conversation, and its `policy`.
    opening: Vec<Record>,
    /// aterm's own records heard since, oldest first, while the client waited for something
    /// else ([`Self::ack`]) or for one of them ([`Self::own`]).
    heard: std::collections::VecDeque<Record>,
}

impl Client {
    fn send(&mut self, text: &str) {
        self.stream
            .write_all(text.as_bytes())
            .expect("client write");
    }

    fn line(&mut self) -> String {
        let mut reply = String::new();
        match self.reader.read_line(&mut reply) {
            Ok(0) => "<EOF>".to_string(),
            Ok(_) => reply.trim_end().to_string(),
            Err(error) => format!("<{error}>"),
        }
    }

    fn request(&mut self, line: &str) -> String {
        self.send(&format!("{line}\n"));
        self.line()
    }

    /// This connection's serial.
    fn id(&mut self) -> u64 {
        let reply = self.request("who");
        reply
            .strip_prefix("OK ")
            .and_then(|id| id.parse().ok())
            .unwrap_or_else(|| panic!("who answered {reply}"))
    }

    /// Open `sid`'s stream, which must open: the reply's `(epoch, acked, in)`. The `hand`, the
    /// `want` and the `policy` aterm sends as a stream opens are read too, numbered after `in`,
    /// and kept in [`Self::opening`].
    fn open(&mut self, sid: &str) -> StreamReply {
        let reply = self.request(&format!("stream @{sid} v=1"));
        let opened = StreamReply::parse(&reply).unwrap_or_else(|error| panic!("{reply}: {error}"));
        self.opened(&opened);
        opened
    }

    /// Read the records aterm sends as a stream opens, after the `OK` that said `opened`: its
    /// `hand`, then a `want` where the tab's Out ring holds none of the claimant's conversation,
    /// then its `policy`, which always comes and ends the opening.
    fn opened(&mut self, opened: &StreamReply) {
        let hand = self.own();
        assert!(
            matches!(hand.body, Body::Hand { .. }) && hand.seq == opened.sent + 1,
            "the opening hand: {hand:?}"
        );
        let mut opening = vec![hand];
        let mut next = self.own();
        if next.body == Body::Want {
            assert_eq!(next.seq, opened.sent + 2, "the opening want");
            opening.push(next);
            next = self.own();
        }
        let seq = opened.sent + 1 + opening.len() as u64;
        assert_eq!(
            (next.seq, next.body.clone()),
            (seq, Body::Policy),
            "the opening policy"
        );
        opening.push(next);
        self.opening = opening;
    }

    /// Whether aterm asked for the conversation as the stream opened (`want`).
    fn wanted(&self) -> bool {
        self.opening.iter().any(|record| record.body == Body::Want)
    }

    /// The last record of the opening: its `policy`.
    fn opening_end(&self) -> u64 {
        self.opening.last().map_or(0, |record| record.seq)
    }

    /// The next line as a record of aterm's: `Ok` for one of its own, `Err` with the line for
    /// anything else (an acknowledgement, the end, a line that is no record).
    fn next_record(&mut self) -> Result<Record, String> {
        let line = self.line();
        match Record::read(line.as_bytes()) {
            Ok(decoded) if !matches!(decoded.record.body, Body::Ack { .. }) => Ok(decoded.record),
            _ => Err(line),
        }
    }

    /// aterm's next own record: the oldest heard, else the next line, which must be one (a
    /// read that times out is the hang detector).
    fn own(&mut self) -> Record {
        if let Some(record) = self.heard.pop_front() {
            return record;
        }
        self.next_record()
            .unwrap_or_else(|line| panic!("{line} is no record of aterm's own"))
    }

    /// The acknowledgement that comes next, aterm's own records before it kept in
    /// [`Self::heard`]: the `seq` acknowledged and its `dropped`.
    fn ack(&mut self) -> (u64, Option<String>) {
        loop {
            match self.next_record() {
                Ok(own) => self.heard.push_back(own),
                Err(reply) => {
                    let decoded = Record::read(reply.as_bytes())
                        .unwrap_or_else(|error| panic!("{reply} is no record: {error}"));
                    let Body::Ack { dropped } = decoded.record.body else {
                        panic!("{reply} is no acknowledgement");
                    };
                    return (decoded.record.seq, dropped.map(|why| why.word()));
                }
            }
        }
    }

    /// What the stream ends on, aterm's own records before it kept in [`Self::heard`]: `<EOF>`
    /// for a closed stream.
    fn end(&mut self) -> String {
        loop {
            match self.next_record() {
                Ok(own) => self.heard.push_back(own),
                Err(line) => return line,
            }
        }
    }

    /// Send record `seq` and read its acknowledgement: the `seq` acknowledged and its `dropped`.
    fn record(&mut self, seq: u64, body: Body) -> (u64, Option<String>) {
        let line = Record { seq, ms: 0, body }.line().expect("a record line");
        self.raw(&line)
    }

    /// Send `line` as it is and read its acknowledgement.
    fn raw(&mut self, line: &str) -> (u64, Option<String>) {
        self.send(line);
        self.send("\n");
        self.ack()
    }
}

struct Rig {
    lanes: Arc<Lanes>,
    service: Arc<Service>,
}

impl Rig {
    /// Lanes with `streams` stream lanes over a store hosting [`ONE`] and [`TWO`].
    fn new(streams: usize) -> Self {
        let store = crate::session_store::new_store();
        for (local, sid) in [(1, ONE), (2, TWO)] {
            store
                .write()
                .unwrap_or_else(PoisonError::into_inner)
                .register(session(local, sid));
        }
        let lanes = Lanes::new(LaneLimits {
            rpc: 2,
            wait: 1,
            inline: 1,
            open: 8,
            subscriptions: streams,
        });
        let started = lanes.start();
        assert_eq!(started.rpc, 2, "every request lane started");
        let service = Arc::new(Service {
            store,
            clock: AtomicU64::new(1_000),
            woke: AtomicU64::new(0),
            stopped: Mutex::new(None),
            quiet_ms: AtomicU64::new(0),
            stall_after_ms: AtomicU64::new(600_000),
            stall_term: std::sync::atomic::AtomicBool::new(true),
            waited: Mutex::new(Vec::new()),
            released: Mutex::new(Vec::new()),
            entered: Mutex::new(Vec::new()),
            proof: Mutex::new(Some(crate::harness_claim::ClaimProof {
                proven: running(),
                version: Some(ADMITTED.to_string()),
            })),
            lines: Mutex::new(Vec::new()),
            repo: Mutex::new(aterm_agent::harness::repo_work::RepoRead::Unread),
            left: Mutex::new(aterm_agent::harness::leftover::LeftRead::Unchecked),
            paused: Mutex::new(Vec::new()),
            meanwhile: Mutex::new(None),
            snapshot: Mutex::new(None),
            typed: Mutex::new(Vec::new()),
            typed_cwd: Mutex::new(Vec::new()),
            typer: Mutex::new(std::collections::VecDeque::new()),
        });
        lanes.publish(Arc::clone(&service) as Arc<dyn LaneService>);
        started.activate();
        Self { lanes, service }
    }

    fn dial(&self, auth: &str) -> Client {
        let (client, server) = CtlStream::pair().expect("real control socket pair");
        client
            .set_read_timeout(Some(SETTLE))
            .expect("bound client reads");
        assert!(self.lanes.admit(server).is_ok(), "a lane admits the client");
        let reader = BufReader::new(client.try_clone().expect("clone the client end"));
        let mut client = Client {
            stream: client,
            reader,
            opening: Vec::new(),
            heard: std::collections::VecDeque::new(),
        };
        client.send(&format!("AUTH {auth}\n"));
        // A fleet peer is served only the fleet's verbs; any other line closes its connection.
        let hello = if auth == "member" {
            aterm_types::control_verbs::VERBS
                .iter()
                .find(|spec| spec.fleet)
                .expect("a fleet verb")
                .name
        } else {
            "ping"
        };
        assert_eq!(client.request(hello), format!("OK {hello}"));
        client
    }

    /// An owner connection holding `sid`'s contract 4 claim of `duties`.
    fn claimant(&self, sid: &str, duties: &str) -> Client {
        let mut client = self.dial("owner");
        assert_eq!(
            client.request(&format!("claim {sid} 4 {duties}")),
            "OK claim"
        );
        client
    }

    /// Wait until no stream lane serves a connection: a closed stream's lane is free only once
    /// its service has returned, which is after its connection's release.
    fn lanes_free(&self) {
        let deadline = Instant::now() + SETTLE;
        while self.lanes.census().streams != 0 {
            assert!(Instant::now() < deadline, "a stream lane never came free");
            std::thread::sleep(Duration::from_millis(2));
        }
    }

    /// Wait until the lanes released connection `id`'s claims: it ended.
    fn ended(&self, id: u64) {
        let deadline = Instant::now() + SETTLE;
        while !self.service.released(id) {
            assert!(
                Instant::now() < deadline,
                "connection {id} never ended and released its claims"
            );
            std::thread::sleep(Duration::from_millis(2));
        }
    }

    fn state(&self, sid: &str) -> crate::session_timeline::StreamState {
        self.service
            .handle(sid)
            .ctx
            .meta
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .stream
            .clone()
            .expect("the session keeps a stream's state")
    }

    fn meta(&self, sid: &str) -> String {
        let handle = self.service.handle(sid);
        super::super::meta_line_for_test(
            &self.service.store,
            &handle.term,
            handle.local_id,
            &handle.ctx,
            "",
        )
    }
}

fn title() -> Body {
    Body::Title {
        title: Some("the stream split".to_string()),
        summary: None,
    }
}

/// The state survives the connection, keyed on the claimant: a reopened stream answers the kept
/// `acked`, a record at or below it is acknowledged `dropped=applied` and changes nothing, and
/// the record after it is taken. Another claimant starts from nothing, and a new epoch starts
/// `in` again while it keeps `acked`.
#[test]
fn a_stream_resends_from_acked_and_drops_what_was_applied() {
    let rig = Rig::new(2);
    let mut first = rig.claimant(ONE, "state");
    let id = first.id();
    let opened = first.open(ONE);
    assert_eq!(opened.epoch, crate::control_auth::process_instance_id());
    assert_eq!((opened.acked, opened.sent), (0, 0));
    assert_eq!(first.record(1, title()), (1, None));
    assert_eq!(first.record(2, Body::Beat), (2, None));
    assert_eq!(first.record(3, title()), (3, None));
    let first_opening = first.opening.len() as u64;
    drop(first);
    rig.ended(id);

    // The same claimant on a new connection: aterm says where it stands.
    let mut again = rig.claimant(ONE, "state");
    let id = again.id();
    let reopened = again.open(ONE);
    assert_eq!(reopened.epoch, opened.epoch);
    assert_eq!(reopened.acked, 3, "the kept acked, not a fresh stream's");
    assert_eq!(
        reopened.sent, first_opening,
        "aterm's opening records of the first stream, in the same epoch"
    );
    let before = rig.state(ONE);
    assert_eq!(
        again.record(2, Body::Beat),
        (2, Some("applied".to_string()))
    );
    assert_eq!(again.record(3, title()), (3, Some("applied".to_string())));
    let resent = rig.state(ONE);
    assert_eq!(
        (resent.acked, resent.titles, resent.unapplied),
        (before.acked, before.titles, before.unapplied),
        "a resent record is applied once"
    );
    assert_eq!(resent.dropped, before.dropped + 2);
    assert_eq!(again.record(4, title()), (4, None), "acked+1 is taken");
    assert_eq!(rig.state(ONE).acked, 4);

    // A new epoch (another aterm instance) starts `in` at 0 and keeps what was acknowledged;
    // the same epoch keeps both.
    {
        let handle = rig.service.handle(ONE);
        let mut meta = handle
            .ctx
            .meta
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        meta.stream.as_mut().expect("the stream's state").sent = 7;
        let same = meta.open_stream(&running(), &opened.epoch, id);
        assert_eq!((same.acked, same.sent), (4, 7));
        let next = meta.open_stream(&running(), "p1-another", id);
        assert_eq!((next.acked, next.sent), (4, 0));
        assert_eq!(next.epoch, "p1-another");
    }
    drop(again);
    rig.ended(id);

    // Another claimant's stream is not this one's: it starts from nothing.
    let mut stranger = rig.dial("owner");
    assert_eq!(
        stranger.request(&format!("claim {ONE} 4 state other")),
        "OK claim"
    );
    let fresh = stranger.open(ONE);
    assert_eq!((fresh.acked, fresh.sent), (0, 0));
    assert_eq!(
        stranger.record(1, title()),
        (1, None),
        "its first record is not a resend"
    );
}

/// The verb opens for the session whose live contract 4 claim is bound to the connection that
/// asks, and for nothing else; every refusal is one `ERR` line that leaves the connection
/// serving requests.
#[test]
fn a_stream_opens_only_for_a_contract_4_claim_on_its_own_connection() {
    let rig = Rig::new(2);
    let open = format!("stream @{ONE} v=1");
    const NO_CLAIM: &str =
        "ERR usage: stream opens only for a contract 4 claim placed on this connection";

    // A caller that is not owner-class, whatever it names: the connection is kept.
    let mut edge = rig.dial("edge");
    assert_eq!(edge.request(&open), "ERR denied: stream is owner-only");
    assert_eq!(
        edge.request(&format!("@{ONE} stream v=1")),
        "ERR denied: stream is owner-only"
    );
    assert_eq!(edge.request("ping"), "OK ping");
    // A fleet peer never reaches the verb: `stream` is no fleet verb, so the fleet's own gate
    // answers and closes, as it does for any other.
    assert!(
        aterm_types::control_verbs::spec("stream").is_some_and(|spec| !spec.fleet),
        "stream is a verb, and not the fleet's"
    );
    for line in [open.clone(), format!("@{ONE} stream v=1")] {
        let mut member = rig.dial("member");
        let refusal = member.request(&line);
        assert!(
            refusal.starts_with("ERR denied: the fleet serves "),
            "{line}: {refusal}"
        );
        assert_eq!(member.line(), "<EOF>", "{line}: the fleet's refusal closes");
    }

    // A session this instance does not host, and the spellings the verb does not have.
    let mut owner = rig.dial("owner");
    assert_eq!(
        owner.request(&format!("stream @{ELSEWHERE} v=1")),
        "ERR no such session"
    );
    for spelling in [
        "stream",
        "stream @self v=1",
        &format!("stream @{ONE}"),
        &format!("stream @{ONE} v=2"),
        &format!("stream @{ONE} v=1 since=3"),
        // Verb first only: after a selector it is the same usage, from the same gate.
        &format!("@{ONE} stream v=1"),
        &format!("@{ONE} stream @{ONE} v=1"),
    ] {
        assert_eq!(
            owner.request(spelling),
            "ERR usage: stream @<sid> v=1",
            "{spelling}"
        );
    }

    // No claim at all.
    assert_eq!(owner.request(&open), NO_CLAIM);

    // A contract 4 claim, but on another connection: only its own connection opens the stream.
    let mut holder = rig.claimant(ONE, "state");
    let holder_id = holder.id();
    assert_eq!(owner.request(&open), NO_CLAIM);
    // And it names its own session: the claim on ONE opens no stream of TWO.
    assert_eq!(holder.request(&format!("stream @{TWO} v=1")), NO_CLAIM);
    assert_eq!(holder.id(), holder_id, "a refusal keeps the connection");
    assert!(rig.service.has_live_claim(holder_id), "and its claim");
    drop(holder);
    rig.ended(holder_id);

    // A claim on this connection under contract 3, then under contract 2.
    for contract_no in [3, 2] {
        let mut older = rig.dial("owner");
        let id = older.id();
        assert_eq!(
            older.request(&format!("claim {ONE} {contract_no} upgrade")),
            "OK claim"
        );
        assert!(rig.service.has_live_claim(id));
        assert_eq!(older.request(&open), NO_CLAIM, "contract {contract_no}");
        assert_eq!(older.id(), id);
        drop(older);
        rig.ended(id);
    }

    // The one that opens: a contract 4 claim on the connection that asks, a grant of no duty
    // included. The selector spelling opened nothing for it first.
    let mut claimant = rig.claimant(ONE, "-");
    assert_eq!(
        claimant.request(&format!("@{ONE} stream v=1")),
        "ERR usage: stream @<sid> v=1"
    );
    let opened = claimant.open(ONE);
    assert_eq!(opened.epoch, crate::control_auth::process_instance_id());
    assert_eq!(claimant.record(1, Body::Beat), (1, None));
}

/// The claim is read again on the stream lane. Where it left the connection between the verb
/// and the `OK`, the connection ends with no line: it serves no request any more, so nothing
/// that reads as a kept refusal is written.
#[test]
fn a_claim_gone_before_the_ok_closes_with_no_line() {
    let store = crate::session_store::new_store();
    store
        .write()
        .unwrap_or_else(PoisonError::into_inner)
        .register(session(1, ONE));
    let (client, server) = CtlStream::pair().expect("real control socket pair");
    client
        .set_read_timeout(Some(SETTLE))
        .expect("bound client reads");
    let request = Request::parse(format!("stream @{ONE} v=1"));
    // The lane's serial for a connection that holds no claim, as `Serving::enter` sets it.
    super::super::SERVING_CONNECTION.with(|serving| serving.set(Some(u64::MAX - 3)));
    super::serve(
        &request,
        &store,
        &server,
        Vec::new(),
        &super::Services {
            world: &crate::harness_watchdog::Live::POLICY,
            claims: &|_, _| String::new(),
            post: &|_| {},
            landed: &|_, _| {},
        },
    );
    super::super::SERVING_CONNECTION.with(|serving| serving.set(None));
    drop(server);
    let mut reply = String::new();
    let read = BufReader::new(client).read_line(&mut reply);
    assert_eq!((read.ok(), reply.as_str()), (Some(0), ""));
    let sid = super::Sid::parse(ONE).expect("a session id");
    let handle = super::hosted(&store, sid).expect("a hosted session");
    let meta = handle
        .ctx
        .meta
        .lock()
        .unwrap_or_else(PoisonError::into_inner);
    assert!(meta.stream.is_none(), "nothing opened");
}

/// A `beat` line of exactly `len` bytes: its padding is a field the codec does not know.
fn padded(seq: u64, len: usize) -> String {
    let head = format!("{{\"k\":\"beat\",\"seq\":{seq},\"ms\":0,\"pad\":\"");
    let line = format!("{head}{}\"}}", "x".repeat(len - head.len() - 2));
    assert_eq!(line.len(), len);
    line
}

/// A line of exactly 256 KiB is a record; one byte more is acknowledged as the next record,
/// unparsed, however far past the bound it runs, and the line after it still reads; a record
/// the claim's grant does not cover is dropped for scope; past the burst a record is dropped
/// for rate while its `beat` still counts, the dropped records cost the budget nothing, and a
/// resend costs nothing either.
#[test]
fn records_over_the_bound_the_budget_or_the_scope_are_acked_dropped() {
    let rig = Rig::new(2);
    let mut client = rig.claimant(ONE, "state");
    client.open(ONE);

    // The bound: a line of LINE_MAX bytes is taken, one byte over is dropped whole though it
    // would parse, and so is one many reads long; a record follows each.
    assert_eq!(client.raw(&padded(1, LINE_MAX)), (1, None));
    assert_eq!(
        client.raw(&padded(2, LINE_MAX + 1)),
        (2, Some("bound".to_string())),
        "its own seq=2 was never read: it is acked+1"
    );
    assert_eq!(client.record(3, Body::Beat), (3, None));
    assert_eq!(
        client.raw(&"x".repeat(5 * LINE_MAX)),
        (4, Some("bound".to_string()))
    );
    assert_eq!(client.record(5, Body::Beat), (5, None));

    // The scope: `state` is granted, `walls` is not.
    let wall = Body::Wall {
        kind: "usage".to_string(),
        resets_ms: None,
    };
    assert_eq!(client.record(6, wall), (6, Some("scope".to_string())));
    assert_eq!(client.record(7, title()), (7, None));
    assert_eq!(rig.state(ONE).titles, 1, "the granted record was taken");

    // The budget: the burst is 500 at one instant. The two lines over the bound spent none of
    // it, and neither does a resend; five records did.
    assert_eq!(client.record(7, title()), (7, Some("applied".to_string())));
    let burst = u64::from(RATE_BURST);
    for seq in 8..=burst + 2 {
        assert_eq!(client.record(seq, Body::Beat), (seq, None));
    }
    assert_eq!(rig.state(ONE).last_beat_ms, Some(1_000));
    rig.service.clock.store(1_001, Ordering::Release);
    let over = burst + 3;
    assert_eq!(
        client.record(over, Body::Beat),
        (over, Some("rate".to_string()))
    );
    let state = rig.state(ONE);
    assert_eq!(
        state.last_beat_ms,
        Some(1_001),
        "a beat over the budget still counts as liveness"
    );
    assert_eq!(
        state.acked, over,
        "a dropped record is acknowledged for good"
    );
    assert_eq!(state.dropped, 5, "two bound, scope, applied and rate");
    // One token comes back every 20 ms (50 a second), counted from when the burst ran out: a
    // record dropped in between took nothing of what had come back.
    rig.service.clock.store(1_019, Ordering::Release);
    assert_eq!(
        client.record(over + 1, title()),
        (over + 1, Some("rate".to_string()))
    );
    rig.service.clock.store(1_020, Ordering::Release);
    assert_eq!(client.record(over + 2, title()), (over + 2, None));
    assert_eq!(
        client.record(over + 3, title()),
        (over + 3, Some("rate".to_string()))
    );
    // A sender a little over the rate (a record every 19 ms) keeps nearly all of them.
    let mut taken = 0;
    for step in 1..=40 {
        rig.service
            .clock
            .store(1_020 + step * 19, Ordering::Release);
        let seq = over + 3 + step;
        match client.record(seq, title()) {
            (acked, None) if acked == seq => taken += 1,
            (acked, Some(why)) if acked == seq && why == "rate" => {}
            other => panic!("record {seq}: {other:?}"),
        }
    }
    assert_eq!(taken, 38, "760 ms give 38 tokens to 40 records");
}

/// The line reader holds one line being gathered and no more: a line many times the bound,
/// with no newline for as long as it runs, is discarded as it arrives, and the line after it
/// is whole.
#[test]
fn a_line_past_the_bound_is_discarded_as_it_arrives() {
    let (client, server) = CtlStream::pair().expect("real control socket pair");
    let revoked = aterm_uds::stop::Stop::new();
    let bell = revoked.bell().expect("the claim's stop descriptor");
    const RUN: usize = 16 * LINE_MAX;
    let writer = std::thread::spawn(move || {
        let mut client = client;
        client.write_all(&vec![b'x'; RUN]).expect("the long line");
        client.write_all(b"\nnext\n").expect("the line after it");
    });
    let owed = aterm_uds::ready::Doorbell::new().expect("the stream's doorbell");
    let mut lines = super::Lines::new(Vec::new());
    let line = |lines: &mut super::Lines| lines.next(&server, &revoked, bell, owed.fd(), None);
    assert!(matches!(line(&mut lines), super::Line::Over));
    assert!(
        lines.buffer.capacity() <= 2 * (LINE_MAX + super::CHUNK),
        "{} bytes held for a line of {RUN}",
        lines.buffer.capacity()
    );
    match line(&mut lines) {
        super::Line::Whole(line) => assert_eq!(line, b"next"),
        _ => panic!("the line after the long one is whole"),
    }
    writer.join().expect("the writer");
    assert!(matches!(line(&mut lines), super::Line::Ended));
}

#[test]
fn a_move_of_the_same_claim_ends_a_stream_with_an_unfinished_line() {
    let rig = Rig::new(1);
    let mut client = rig.claimant(ONE, "state");
    let id = client.id();
    client.open(ONE);
    client
        .stream
        .write_all(b"unfinished")
        .expect("partial line");

    let handle = rig.service.handle(ONE);
    let successor = u64::MAX - 9;
    place(
        &handle,
        4,
        &[Duty::State],
        successor,
        running(),
        "claude-code-alab",
    )
    .expect("a claim placed elsewhere");
    rig.ended(id);
    assert!(rig.service.has_live_claim(successor));
    rig.lanes_free();
}

#[test]
fn a_revoked_stream_leaves_a_complete_buffered_line_unread() {
    let (_client, server) = CtlStream::pair().expect("real control socket pair");
    let revoked = aterm_uds::stop::Stop::new();
    let bell = revoked.bell().unwrap();
    assert!(revoked.set());
    let owed = aterm_uds::ready::Doorbell::new().expect("the stream's doorbell");
    owed.ring();
    let mut lines = super::Lines::new(b"buffered\n".to_vec());
    assert!(matches!(
        lines.next(&server, &revoked, bell, owed.fd(), None),
        super::Line::Revoked
    ));
    assert_eq!(lines.buffer, b"buffered\n");
}

/// A `stream` verb is served or refused here, never forwarded to a sibling and never a wait;
/// and with every stream lane taken it is answered `ERR busy streams` before any `OK`, on a
/// connection that keeps serving requests and keeps its claim, and opens once a lane is free.
#[test]
fn a_stream_verb_is_never_relayed_and_a_full_pool_keeps_the_claim() {
    let rig = Rig::new(1);
    let store = &rig.service.store;
    let request = |line: String| Request::parse(line);
    for line in [
        format!("stream @{ELSEWHERE} v=1"),
        format!("@{ELSEWHERE} stream v=1"),
    ] {
        let stream = request(line.clone());
        assert!(
            super::super::relay_candidate(&stream, Scope::Owner).is_none(),
            "{line} names no session to forward to"
        );
        assert!(
            !super::super::is_wait_request(&stream, Scope::Owner, store),
            "{line} is no wait"
        );
    }
    // The verbs that are forwarded for a session hosted elsewhere, so the stream's refusal is
    // its own and not the sid's.
    for line in [
        format!("subscribe @{ELSEWHERE} events"),
        format!("@{ELSEWHERE} status"),
    ] {
        let relayed = request(line.clone());
        assert_eq!(
            super::super::relay_candidate(&relayed, Scope::Owner).map(|sid| sid.as_str()),
            Some(ELSEWHERE),
            "{line}"
        );
    }
    assert!(super::super::is_wait_request(
        &request(format!("@{ELSEWHERE} status")),
        Scope::Owner,
        store
    ));

    // One stream lane, taken by the first claimant.
    let mut first = rig.claimant(ONE, "state");
    let first_id = first.id();
    first.open(ONE);
    assert_eq!(first.record(1, Body::Beat), (1, None));

    let mut second = rig.dial("harness");
    assert_eq!(second.request(&format!("claim {TWO} 4 state")), "OK claim");
    let second_id = second.id();
    assert_eq!(
        second.request(&format!("stream @{TWO} v=1")),
        "ERR busy streams",
        "refused before any OK"
    );
    assert_eq!(
        second.id(),
        second_id,
        "still in request mode, the same tenure"
    );
    assert!(
        rig.service.has_live_claim(second_id),
        "the refusal kept the claim"
    );
    assert!(!rig.service.released(second_id));
    assert!(
        rig.service
            .handle(TWO)
            .ctx
            .meta
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .stream
            .is_none(),
        "a refused stream opened nothing"
    );

    // The lane comes free: the same connection, with the same claim, now opens its stream.
    drop(first);
    rig.ended(first_id);
    rig.lanes_free();
    // The verb and its first record in one write: what the request lane read past the verb
    // goes to the stream lane with the connection.
    let record = Record {
        seq: 1,
        ms: 0,
        body: Body::Beat,
    }
    .line()
    .expect("a record line");
    second.send(&format!("stream @{TWO} v=1\n{record}\n"));
    let opened = second.line();
    let opened = StreamReply::parse(&opened).unwrap_or_else(|error| panic!("{opened}: {error}"));
    assert_eq!(opened.acked, 0);
    // aterm's opening records come before the acknowledgement: they are written before any
    // line is read.
    second.opened(&opened);
    assert_eq!(
        second.ack(),
        (1, None),
        "the record sent with the verb was read"
    );
    assert!(
        !rig.service.released(second_id),
        "the flip moved the tenure"
    );
    // The stream lane serves as the connection: its serial, principal, caller, descriptor and
    // direct peer are the thread's for the stream's life.
    let entered = rig
        .service
        .entered
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .clone();
    let as_conn = |conn, principal| Entered {
        conn: Some(conn),
        principal,
        caller: true,
        descriptor: true,
        direct: true,
    };
    assert_eq!(
        entered,
        [
            as_conn(first_id, Principal::Instance),
            as_conn(second_id, Principal::Harness)
        ]
    );
}

/// A `seq` past `acked+1` closes the stream with nothing acknowledged, where the next `seq`
/// does not. A line within the bound that is no record does not close it: it is record
/// `acked+1`, acknowledged `dropped=refused:usage`, so the harness's resend from `acked` never
/// brings it back and the record behind it arrives. A claim placed on another connection closes
/// the stream, where a renewal on its own connection does not; the new claim outlives the closed
/// connection.
#[test]
fn a_seq_gap_or_a_claim_on_another_connection_closes_the_stream() {
    let rig = Rig::new(2);
    let mut gapped = rig.claimant(ONE, "state");
    let id = gapped.id();
    gapped.open(ONE);
    assert_eq!(gapped.record(1, title()), (1, None));
    assert_eq!(
        gapped.record(2, title()),
        (2, None),
        "acked+1 keeps it open"
    );
    gapped.send(
        &Record {
            seq: 4,
            ms: 0,
            body: title(),
        }
        .line()
        .expect("a record line"),
    );
    gapped.send("\n");
    assert_eq!(gapped.end(), "<EOF>", "a gap is closed, not acknowledged");
    rig.ended(id);
    assert_eq!(rig.state(ONE).acked, 2, "the gapped record was not taken");

    // A line that is no record takes the next number and is dropped, on a stream that stays
    // open: the records sent behind it are taken, and a reopened stream stands past it.
    let mut stray = rig.claimant(ONE, "state");
    let id = stray.id();
    assert_eq!(stray.open(ONE).acked, 2);
    let usage = Some("refused:usage".to_string());
    for (seq, line) in [
        (3, ""),
        (4, "not a record"),
        // A known kind without its required field: the codec reads no record, whatever `seq`
        // the line carries.
        (5, r#"{"k":"state","seq":9,"ms":0}"#),
    ] {
        assert_eq!(stray.raw(line), (seq, usage.clone()), "{line:?}");
    }
    assert_eq!(
        stray.record(6, title()),
        (6, None),
        "the record behind them"
    );
    let state = rig.state(ONE);
    assert_eq!(
        (state.acked, state.dropped, state.titles, state.unapplied),
        (6, 3, 3, 0)
    );
    drop(stray);
    rig.ended(id);
    // The harness's resend: it keeps what is above the `acked` it is told, so the unread lines
    // are not sent again; were one sent again it would be a new record, not a closed stream.
    let mut back = rig.claimant(ONE, "state");
    let id = back.id();
    assert_eq!(
        back.open(ONE).acked,
        6,
        "past the lines that were no record"
    );
    // A gap still closes, with the unread lines counted in what it is a gap from.
    back.send(r#"{"k":"beat","seq":8,"ms":0}"#);
    back.send("\n");
    assert_eq!(back.end(), "<EOF>");
    rig.ended(id);
    assert_eq!(rig.state(ONE).acked, 6);

    let mut moved = rig.claimant(ONE, "state");
    let id = moved.id();
    assert_eq!(moved.open(ONE).acked, 6);
    // A renewal in place, on the stream's own connection, changes nothing.
    let handle = rig.service.handle(ONE);
    place(
        &handle,
        4,
        &[Duty::State],
        id,
        running(),
        "claude-code-alab",
    )
    .expect("a renewal in place");
    assert_eq!(moved.record(7, title()), (7, None));
    // The claim placed on another connection: the stream ends at its next look.
    let elsewhere = u64::MAX - 7;
    place(
        &handle,
        4,
        &[Duty::State],
        elsewhere,
        running(),
        "claude-code-alab",
    )
    .expect("a claim placed elsewhere");
    assert_eq!(moved.end(), "<EOF>");
    rig.ended(id);
    let now = crate::metrics::now_us();
    let meta = handle
        .ctx
        .meta
        .lock()
        .unwrap_or_else(PoisonError::into_inner);
    assert_eq!(
        meta.live_claim(now).and_then(|claim| claim.conn),
        Some(elsewhere),
        "the closed stream's connection released only its own claim"
    );
    assert_eq!(meta.stream.as_ref().map(|state| state.acked), Some(7));
}

/// `title` and an unknown kind are each acknowledged with no `dropped`, counted where `meta`
/// prints them, and left out of the applied kinds; `beat`, which is applied, is in them. Every
/// known kind a harness sends is applied since HX-15f (`session`) and HX-15i1 (`ask`, `asked`,
/// `wall`), so `stream_unapplied` counts only a record the codec read and cannot write back,
/// and stays 0 here.
#[test]
fn title_and_unknown_records_are_acked_and_counted_in_meta() {
    let rig = Rig::new(2);
    let mut client = rig.claimant(ONE, "state");
    let quiet = "stream_epoch=- stream_acked=0 stream_in=0 stream_dropped=0 stream_unknown=0 \
                 stream_unapplied=0 stream_titles=0 stream_took=0 stream_refused=0 questions=";
    assert!(rig.meta(ONE).contains(quiet), "{}", rig.meta(ONE));
    let epoch = client.open(ONE).epoch;
    let opening = client.opening.len();

    assert_eq!(client.record(1, title()), (1, None));
    assert_eq!(
        client.raw(r#"{"k":"hx99-later","seq":2,"ms":0,"run":"rm -rf /"}"#),
        (2, None)
    );
    assert_eq!(client.record(3, title()), (3, None));
    assert_eq!(client.record(4, title()), (4, None));
    assert_eq!(client.record(5, Body::Beat), (5, None));

    let meta = rig.meta(ONE);
    // `in` counts aterm's opening records.
    let counted = format!(
        " supervisor_proven=1 stream_epoch={epoch} stream_acked=5 stream_in={opening} stream_dropped=0 \
         stream_unknown=1 stream_unapplied=0 stream_titles=3 stream_took=0 stream_refused=0 \
         questions="
    );
    assert!(meta.contains(&counted), "{meta}");
    let state = rig.state(ONE);
    assert_eq!(
        state.applied.iter().copied().collect::<Vec<_>>(),
        ["beat"],
        "a kind acknowledged but not applied stays the built-in source's"
    );
    // The record's text was data: the session's own fields are as they were.
    assert!(meta.contains(" user_title=- "), "{meta}");
    // The other session counted nothing.
    assert!(rig.meta(TWO).contains(quiet), "{}", rig.meta(TWO));
}

/// The `state` record and the current-state record it feeds.
#[path = "control_stream_state_tests.rs"]
mod state;

/// The `claim` record and the re-exec grace of a stream's claim.
#[path = "control_stream_claim_tests.rs"]
mod claim;

/// aterm's own records on the stream (`hand`, `policy`, `restart`) and the harness's answers.
#[path = "control_stream_own_tests.rs"]
mod own;

#[cfg(test)]
#[path = "control_stream_kinds_tests.rs"]
mod kinds;

/// The conversation records (`turn`, `tool`, `subagent`, `msg`, `diff`), the Out ring and
/// `want` (HX-15h).
#[path = "control_stream_conversation_tests.rs"]
mod conversation;

#[path = "control_stream_watchdog_tests.rs"]
mod watchdog;

/// The watchdog's relaunch with `<tool> run` and the `session` record (HX-15f).
#[path = "control_stream_relaunch_tests.rs"]
mod relaunch;

/// The `act`, `need`, `clear`, `land`, `model` and `upgrade` records.
#[path = "control_stream_acts_tests.rs"]
mod acts;

/// The memory ceiling's `restart` record to a harness holding `relaunch` (HX-15m).
#[path = "control_stream_memory_tests.rs"]
mod memory;

/// The `ask`, `asked` and `wall` records and aterm's progress watch (HX-15i1).
#[path = "control_stream_asks_tests.rs"]
mod asks;
