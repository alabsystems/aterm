<!-- Copyright 2026 Andrew Yates -->
<!-- SPDX-License-Identifier: Apache-2.0 -->

# aterm-net: spec recovery from its 41 unit tests

> Audit amendment: The 2026-10-16 code audit added a presented-certificate check against RemoteEndpoint.fingerprint before AUTH, independently of ClientConfig. The dated test inventory and §5.2 record the earlier snapshot; dial-pin-gate-fail-closed reflects the fix.

> Status 2026-10-16: this is `aterm-net`'s migration worklist. Each of the 24 specs below is
> proved through Trust/Clean about the compiled Rust function, and then the tests it subsumes are
> deleted. The one test in §4 that samples no spec is to be deleted now. No test has been deleted yet.

Source: `crates/aterm-net` (read only). I read every `#[test]` (`src/lib.rs` 4, `src/tls.rs` 8,
`src/drive.rs` 29 = 41), plus the production code each one reaches: all of `lib.rs`, `tls.rs` and
`drive.rs`, `aterm_codec::hex::nibble` and `aterm_session::EdgeToken`. Nothing was compiled or run.
The claims below come from reading the code.

Result: **24 specs**, covering **40 tests**. **1 test is not a spec sample** (§4).

## 0. Notation used in the statements

- `HMAC(k,m)`: HMAC-SHA256 with key `k` and message `m`, as `aterm_digest::HmacSha256` computes it.
  `hex(b)` = `aterm_codec::hex::encode_lower(b)`. `nib(c)` = `aterm_codec::hex::nibble(c)` (it accepts `0-9a-fA-F`).
- `WireTok(s)` := `s ≠ ""` ∧ no byte of `s` is `' '`, `'\n'` or `'\r'` (this is `lib.rs::is_wire_token`).
- `Line(x)` = `x ++ "\n"`.
- Streams: for an `S: Read + Write`, `R(S)` is the byte sequence offered to reads and `W(S)` the bytes
  written so far (flush is unobservable). `Duplex(S)` is a lossless in-order byte channel between two
  endpoints. The handshake functions are generic over `S`, so they can be specified, and proved, against
  a pure in-memory `Vec`-backed `S`, with no socket or thread involved.
- `ReadLine(R, max)` is `lib.rs::read_line`'s result; `read-line-bounded` states it.
- Claim state `st ∈ {RUNNING=0, AUTHED=1, FIRED=2, EVICTED=3}` (the `WD_*` constants in `drive.rs`);
  `Claim ∈ {Authed, Deadline, Evicted}` is what `finish()` reports. "Leaves RUNNING at most once" means
  that over any execution, the values `st` holds are RUNNING followed by at most one other value.
- `Pending(held)` = `{i | held[i].unauthenticated}`. `cnt(x)` = `|{i ∈ Pending | held[i].source = x}|`.
  `argmin_since(I)` = the least index `i ∈ I` minimising `held[i].since` (`Iterator::min_by_key`
  returns the *first* minimum).
- `Dir(a→b)` for `tls::relay` is the plaintext byte stream from endpoint `a` to endpoint `b`. Its EOF is a
  local write-half shutdown or EOF on the control-socket side, or a peer `close_notify` on the TLS side.
- Timing statements quantify over the arming instant `t0` of a `HandshakeWatchdog` and over every
  interleaving of `unpark` and spurious `park_timeout` wakes.
- Models: the refinement specs use the committed `ty_model!` machines in `aterm_spec::derive`
  (`net_dial_after_grant_model` = NetDialAfterGrant, `net_slot_displacement_model` =
  NetSlotDisplacement, `tls_buffered_relay_model`). "Refines M" means every observed step of the code
  projects to a transition M admits, and every reached projected state satisfies M's invariants. Today
  each model is bound to the code by a conformance test with a negative control. Gap 10 is what replaces
  that binding with a proof.
- Prose: error strings are quoted where a test or a caller reads them (`serve`'s
  `"the listener is at capacity"`, `DISPLACED`, `TokenRefused`'s Display). Elsewhere the specs pin the
  error *kind* and which branch fires, and the words are given for reference.
- Crypto and rustls are trusted axioms, never proof obligations: HMAC-SHA256 is a PRF and unforgeable,
  SHA-256 is collision-resistant, and rustls implements TLS 1.3 and RFC 5705 exporters correctly
  (equal on both ends of one session, distinct across sessions with overwhelming probability).
- **Recommended refactors** that expose pure cores (no Rust fn names them today):
  - `pin_refusal(pin: &RemoteEndpoint, observed: Option<&str>) -> Option<String>` (the match at `drive.rs:1313`);
  - `classify_unauth(claim: Claim, r: Result<_, io::ErrorKind>)` (`drive.rs:693` and `:1275`);
  - `parse_roster_head(&str) -> Option<usize>` (`drive.rs:1140`);
  - an integer core for `poll_timeout_millis` over `(as_millis: u128, subsec_nanos: u32)` and for
    `source_of` over `u128`, so that neither needs a `Duration` or `IpAddr` carrier (this drops gap 1).

## 1. Specs (TSV)

Columns: `id`, `kind`, `functions` (paths relative to `crates/aterm-net/src/`), `statement`,
`subsumes` (primary tests, `lib::`/`tls::`/`drive::` = `src/{lib,tls,drive}.rs`; secondary in §2),
`difficulty` (the Rust features the proof must handle), and `blocking gaps` (the numbers of the gap
table in `docs/AUDIT-trust-for-test-migration-2026-10-15.md` §7). Gap 0 (the installed `trust` channel's
seal publishes no Clean row; seal `150efe4a` fixes it but is not admitted to the channel) blocks every row.

```tsv
id	kind	functions	statement	subsumes	difficulty	blocking gaps
channel-bind-verify	contract	lib.rs::{channel_bind,verify_presented}	∀ token t, exporter e, presented p: channel_bind(t,e) = HMAC(t.as_bytes(), e), and verify_presented(t,e,p) ⇔ p = channel_bind(t,e) (so any |p| ≠ 32 is false, never a panic). The .expect on the HMAC key is unreachable (the key takes any length). That a tag bound to e is rejected under e' ≠ e (no cross-channel replay) or under another token (no forgery) is the HMAC axiom, trusted and not proved here (§5.3).	lib::channel_binding_rejects_a_cross_channel_replay	&[u8], [u8;32], &EdgeToken, cross-crate calls into aterm_digest::HmacSha256 (new_from_slice/update/finalize/verify_slice) that need their own ensures (verify_slice(p) ⇔ p = finalize()), an .expect proved unreachable	0, 1, 4, 12
hex-decode-bounded	contract (lemma)	lib.rs::hex_decode, aterm_codec::hex::nibble	∀ s with b = s.as_bytes(), n = |b|: hex_decode(s) = Some(v) ⇔ n ≤ MAX_AUTH_LINE ∧ n even ∧ ∀i<n nib(b[i]) ≠ None, and then |v| = n/2 ∧ ∀k<n/2 v[k] = 16·nib(b[2k]) + nib(b[2k+1]). Round trip: ∀ x with 2|x| ≤ MAX_AUTH_LINE, hex_decode(hex(x)) = Some(x). No index out of bounds, no overflow. Written for the Trust verifier.	(none primary; lemma for auth-line-roundtrip and capability-handshake)	while loop with a two-step index (invariant i even, |out| = i/2), slice indexing, Vec::with_capacity/push, ? on Option, u8 shift/or, a cross-crate const fn	0, 1, 3, 5, 7, 12
auth-line-roundtrip	round-trip	lib.rs::{parse_auth,is_wire_token,hex_decode}	(a) ∀ src, op with WireTok(src) ∧ WireTok(op), ∀ tag:[u8;32]: parse_auth("AUTH "++src++" "++op++" "++hex(tag)) = Some((src, op, tag.to_vec())). (b) parse_auth(l) = Some((s,o,t)) ⇔ l.split(' ') yields exactly four fields f0..f3 with f0 = "AUTH", f1 = s ≠ "", f2 = o ≠ "" and hex_decode(f3) = Some(t); None otherwise. One-directional: upper-case tags parse but are never written (§5.11).	(none primary; lemma for capability-handshake)	str::split iterator, String/&str equality, to_owned, an Option of (String,String,Vec<u8>)	0, 1, 3, 5, 7
read-line-bounded	contract (lemma)	lib.rs::read_line	∀ max, ∀ reader r yielding bytes x (finite or not): let k be the first index with x[k] = '\n'. If k exists, k ≤ max and x[..k] is UTF-8, the result is Ok(x[..k]) having consumed exactly k+1 bytes. EOF before the newline ⇒ Err(UnexpectedEof). No '\n' in the first max+1 bytes ⇒ Err(Other) after consuming exactly max+1 bytes, so the buffer never exceeds max. Non-UTF-8 within the bound ⇒ Err(InvalidData). An Err from r.read propagates. This is the DoS bound present_capability, verify_capability and observe_launch_nonce rely on.	(none primary; lemma for capability-handshake and observe-launch-nonce)	loop over a generic Read with a 1-byte buffer, Vec push with a length guard, String::from_utf8, io::Error construction	0, 1, 7, 8, 11
capability-handshake	contract	lib.rs::{present_capability,verify_capability,is_token_refused,TokenRefused}	Over Duplex(S), ∀ dialer (t_d, e_d, src, op) and listener (e_l, lookup): (1) present_capability returns Err with W(S) unchanged unless WireTok(src) ∧ WireTok(op); otherwise it writes exactly Line("AUTH "++src++" "++op++" "++hex(channel_bind(t_d,e_d))), and the raw token never appears except as that MAC. (2) verify_capability reads l = ReadLine(R(S), MAX_AUTH_LINE). It returns Ok(Granted{src,op}) and writes exactly Line("OK") iff parse_auth(l) = Some((src,op,tag)) ∧ lookup(src,op) = Some(t_l) ∧ verify_presented(t_l,e_l,tag). Otherwise it writes Line("DENIED") (best effort) and returns Err(PermissionDenied) naming the FIRST failing clause: "malformed capability line" | "no capability for the requested op" | "wrong drive token". lookup is called at most once, only after parse_auth succeeds, and only with the parsed (src,op). (3) present_capability then reads the verdict line v: v = "OK" ⇒ Ok(()); any other v ⇒ Err(PermissionDenied) carrying TokenRefused (is_token_refused = true, Display "the remote refused the drive token"); EOF ⇒ Err(UnexpectedEof, "the remote hung up without answering the drive token") with is_token_refused = false. Composed: the listener grants, and the dialer returns Ok, iff lookup(src,op) = Some(t_l) ∧ channel_bind(t_d,e_d) = channel_bind(t_l,e_l), which under the HMAC axiom holds iff t_d = t_l ∧ e_d = e_l.	lib::capability_handshake_grants_valid_and_denies_unknown_replay_and_forgery, lib::a_listener_that_hangs_up_unanswered_is_named_as_such, drive::listener_authenticates_its_local_socket_so_the_token_never_crosses_the_wire	generic S: Read+Write as &mut (modelled as a pure in-memory duplex, so no socket or thread), FnOnce lookup closure, format!, write_all/flush, io::Error with a boxed custom error and downcast (Error::is::<TokenRefused>)	0, 1, 2, 4, 7, 8, 11, 13
endpoint-matches	contract	lib.rs::RemoteEndpoint::matches	∀ ep, f, n: ep.matches(f,n) ⇔ ep.fingerprint = f ∧ ep.nonce = n (string equality).	lib::endpoint_pin_rejects_relaunch_and_host_swap	&self on a struct of Strings, String = &str	0, 1, 2
roster-nonce	contract	drive.rs::roster_nonce	Let parsed(rows) = [(f1, x) | row ∈ rows, row.split(' ') has ≥ 3 fields f0,f1,f2,… with f2 = "nonce="++x], in row order. If pin.sid = "": roster_nonce(pin, rows) = Some(pin.nonce) iff ∃(_,x) ∈ parsed with x = pin.nonce, else None. Otherwise roster_nonce(pin, rows) = Some(x) for the FIRST (s,x) ∈ parsed with s = pin.sid, None if there is none.	drive::the_roster_observer_passes_a_live_pin_and_refuses_a_stale_one	&[String], str::split/strip_prefix, iterator chains (filter_map, any, find) with closures, Option combinators, String clone	0, 1, 3, 5, 7, 13
observe-launch-nonce	contract	drive.rs::observe_launch_nonce, lib.rs::read_line	observe_launch_nonce(pin, T) writes exactly Line("sessions bridge") and reads a head line h (≤ ROSTER_MAX_LINE). If h ≠ "OK "++d with d.trim() parsing as a usize N ≤ ROSTER_MAX_ROWS, it returns Err(Other, "the remote refused `sessions bridge` (" ++ Debug(first 80 chars of h) ++ "), so expect_nonce cannot be checked; nothing was relayed"). Otherwise it reads exactly N further lines, each ≤ ROSTER_MAX_LINE, and returns Ok(roster_nonce(pin, those lines)). It consumes nothing past the N-th row, so none of the roster reaches the dialer's local client. A read error e becomes Err(e.kind(), …): "the remote closed the connection before sending its session roster; nothing was relayed" on EOF, else "reading the remote's session roster: {e}; nothing was relayed".	(none primary; lemma for dial-pin-gate-fail-closed)	TLS stream effects, a read loop with Vec push, str strip_prefix/trim/parse::<usize>, chars().take(80), closures	0, 1, 7, 8
dial-pin-gate-fail-closed	contract	drive.rs::{dial_and_relay_pinned,dial_and_relay_probed,dial_and_relay_pinned_inner,within_wall_clock} (the refusal match at :1313; factor as pin_refusal)	∀ runs with pin = Some(p) whose authenticated phase succeeds: no byte of the probe, of prebuffer, or read from local reaches the transport, and tls::relay is never entered, unless observe_nonce returned Ok(Some(n)) within the wall-clock bound with n = p.nonce (pin_refusal(p, Some(n)) = None). Otherwise the result is Err: TimedOut when the observer outlived handshake_timeout (within_wall_clock's late()), the observer's own error, or Err(Other, refusal) with refusal = sid ≠ "" ∧ observed = Some(_) → "session {sid.escape_debug()} was relaunched: its launch nonce no longer matches expect_nonce; nothing was relayed"; sid ≠ "" ∧ None → "the remote has no session {sid}; nothing was relayed"; sid = "" → "no remote session carries expect_nonce (relaunched or closed); nothing was relayed". With pin = None, observe_nonce is never called. Before capability presentation, SHA-256 of the presented certificate must equal p.fingerprint, decoded as hex after trimming and stripping an optional sha256: prefix; malformed or mismatched fingerprints return PermissionDenied.	drive::a_matching_launch_nonce_pin_passes_and_relays_a_verb, drive::a_mismatching_launch_nonce_pin_is_rejected_before_relay, drive::a_pin_with_no_observable_nonce_fails_closed	FnOnce observer closure over &mut TlsTransport, Option<RemoteEndpoint>, &self matches, early returns, String formatting with escape_debug, a ghost write trace on the transport, a thread watchdog inside within_wall_clock	0, 1, 2, 4, 8, 13
dial-wire-order	contract	drive.rs::{dial_and_relay_pinned_inner,dial_and_relay_pinned,dial_and_relay_probed}	The plaintext the dialer writes to the TLS transport is exactly, in order: the AUTH line of present_capability ++ [Line("sessions bridge") iff pin is Some] ++ [probe.request iff probe is Some] ++ prebuffer ++ (the bytes relayed from local). Each segment is written only if every earlier gate passed: the verdict was OK and the watchdog claim Authed; for pin = Some, dial-pin-gate-fail-closed; for probe = Some, expect_probe_answer returned Ok. Probe and roster replies are consumed by the dialer and never forwarded to local. Connect tries the to_socket_addrs results in order, each with its own handshake_timeout, uses the first that connects, and otherwise returns the last error (InvalidInput "no socket address resolved" when there is none); nothing is written if every connect fails.	drive::driver_dials_listener_presents_capability_and_relays_to_the_local_socket	for loop over generic ToSocketAddrs, Option<RemoteEndpoint>/Option<ConnectProbe>, Result plumbing, write_all/flush, the relay as an opaque continuation	0, 1, 7, 8, 11, 13
listener-dial-after-grant	refinement	drive.rs::{accept_and_relay,accept_and_relay_inner,accept_and_relay_armed}; model aterm_spec::derive::net_dial_after_grant_model	∀ runs: connect_local is called at most once, and only after verify_capability returned Ok (so after Line("OK") was written to the dialer) and watchdog.finish() returned Authed. on_admit(&granted) is called exactly once, before connect_local, iff the capability verified in time. So every run's {Verify, DialLocal} trace is a behaviour of NetDialAfterGrant with Buggy = 0, ending in a DialImpliesGranted state. On any earlier failure (TLS error, malformed, unknown or forged capability, deadline, eviction) connect_local is never called and nothing is relayed. On a grant, Ok(Granted{src,op}) is returned only after tls::relay returns Ok.	drive::accept_and_relay_conforms_to_net_dial_after_grant, drive::a_denied_capability_never_reaches_the_local_socket	FnOnce closures (lookup, connect_local, on_admit), an immediately-invoked closure returning Result, ? propagation, a match on a (Claim, Result) tuple, effects of tls::accept/relay, a ghost event trace	0, 4, 8, 10, 13
unauth-outcome-classification	table-property	drive.rs::accept_and_relay_armed (match at :693), drive.rs::dial_and_relay_pinned_inner (match at :1275), drive.rs::{is_deadline,within_wall_clock}; factor as classify_unauth	A total function of (claim c = finish(), unauthenticated-phase result r). Listener: (Authed, Ok v) ⇒ continue with v; (Authed, Err e) ∧ ¬is_deadline(e) ⇒ Err(e) unchanged; (Evicted, _) ⇒ Err(ConnectionAborted, DISPLACED); every other case ((Deadline, _), or (Authed, Err e) with is_deadline(e)) ⇒ Err(TimedOut, "no handshake within {handshake_timeout:?}"). After proceeding, a connect_local error e becomes Err(e.kind(), "could not reach this aterm's control socket: "++e). Dialer: (Authed, Ok t) ⇒ continue; (Authed, Err e) ∧ ¬is_deadline(e) ⇒ Err(e); otherwise Err(TimedOut, "the remote did not finish the handshake within {handshake_timeout:?}"); a roster exchange past its bound ⇒ Err(TimedOut, "the remote sent no session roster within {handshake_timeout:?}; nothing was relayed"). within_wall_clock(t, bound, exchange, late) = exchange's result if finish() = Authed, else Err(late()). is_deadline(e) ⇔ e.kind() ∈ {WouldBlock, TimedOut}. With watchdog-deadline-safety-and-liveness: a peer that never completes the handshake gets Err(TimedOut), never UnexpectedEof.	drive::a_stalled_pre_auth_peer_is_dropped_by_the_handshake_deadline	enum matching on (Claim, Result) with guards, io::ErrorKind, io::Error::new/kind, format! with Debug of Duration, a map_err closure; gap 8 drops once classify_unauth is factored out	0, 1, 3, 4, 8
watchdog-claim-exactly-once	invariant	drive.rs::HandshakeWatchdog::{arm,finish,evictor}, drive.rs::Evictor::{evict,unauthenticated}, drive.rs::claim_deadline	∀ armed watchdogs: st starts at RUNNING and leaves it at most once (only CAS(RUNNING→x) writes it), to the value of whichever of finish (AUTHED), evict (EVICTED) or claim_deadline (FIRED) wins. finish() = Authed ⇔ its CAS won; = Evicted ⇔ st = EVICTED; = Deadline ⇔ st = FIRED. evict() = true and claim_deadline() = true iff that call's CAS won, and false forever after. unauthenticated() ⇔ st = RUNNING. The watchdog thread shuts the socket clone down iff the final st ∈ {FIRED, EVICTED}, never at AUTHED (that socket still carries bytes). finish() joins the thread before returning, so its socket clone is dropped and no shutdown is pending after it.	drive::an_eviction_cuts_only_a_connection_that_has_not_authenticated	Arc<AtomicU8> compare_exchange (AcqRel/Acquire), std::thread spawn/park_timeout/unpark/join, a cloned TcpStream shutdown, enum Claim, Drop	0, 2, 8, 10, 13
watchdog-deadline-safety-and-liveness	other (temporal)	drive.rs::HandshakeWatchdog::arm (thread body), drive.rs::{claim_deadline,arm_unauthenticated,within_wall_clock}	Safety: a shutdown with st = FIRED happens at a time t with t − t0 ≥ timeout, whatever the unparks or spurious wakes, because remaining is recomputed from t0 and the CAS to FIRED is attempted only at remaining = 0. Liveness: with no claim before t0 + timeout, st becomes FIRED and the socket is shut down after finitely many wakes under fair scheduling (no wake starves it). An EVICTED claim leads to shutdown on the next wake (state is re-read on every wake, and evict unparks the thread). Hence a peer that dribbles bytes (resetting any per-syscall timer) is still cut off at the bound: on the listener the unauthenticated phase (TLS handshake + AUTH line) at handshake_timeout; on the dialer the TLS handshake + verdict and the roster read at handshake_timeout, and the probe at probe.within. arm_unauthenticated also sets the read and write timeouts to handshake_timeout.	drive::a_dribbling_pre_auth_peer_is_cut_off_by_the_wall_clock_deadline, drive::a_dribbling_server_is_cut_off_by_the_dialers_wall_clock_deadline, drive::a_session_roster_dribbled_by_the_remote_is_cut_off_at_the_handshake_bound, drive::spurious_wakes_neither_shorten_nor_starve_the_watchdog_deadline	a loop in a spawned closure with Instant/Duration arithmetic (saturating_sub, elapsed), park_timeout with spurious wakeups, atomics, socket shutdown, FnOnce closures; needs a monotonic-clock and park model and fairness reasoning	0, 7, 8, 10, 13
poll-timeout-ceil	contract	drive.rs::poll_timeout_millis	∀ d: poll_timeout_millis(d) = min(⌈d.as_nanos() / 10^6⌉, i32::MAX), computed as as_millis + [subsec_nanos mod 10^6 ≠ 0] with saturation. Hence result ≥ 0 (never poll's −1 "forever"), result ≥ 1 whenever d > 0 (a positive wait never becomes a zero-timeout busy poll), and result ≥ d in milliseconds unless saturated.	(none primary; lemma for listener-readiness)	u128 saturating_add, bool→u128, is_multiple_of, i32::try_from with unwrap_or, Duration accessors	0, 1, 3
listener-readiness	contract	drive.rs::wait_listener_readable (unix/windows/other arms), drive.rs::{poll_timeout_millis,serve} (accept loop)	wait_listener_readable(l, T) = Ok(true) iff poll/WSAPoll reported an event on l (a queued connection, or an error or hangup the next accept surfaces), which happens as soon as a connection is accept-ready, without waiting for T. Ok(false) only after the whole of T has elapsed since the call: on unix, EINTR recomputes the remaining time from the call's start and never restarts T. Any other poll failure ⇒ Err. The non-native fallback sleeps T and returns Ok(false). In serve, each iteration either accepts or waits at most ACCEPT_POLL (sleeping ACCEPT_POLL on an error), so clearing running is observed within ACCEPT_POLL plus one in-flight accept.	drive::real_listener_readiness_wakes_before_the_shutdown_interval, drive::idle_listener_readiness_wait_is_bounded_without_spinning	unsafe libc::poll / WSAPoll FFI with a pollfd, errno via last_os_error, a loop on EINTR with Instant arithmetic, cfg-gated platform arms	0, 3, 7, 8
pick-displaced-rule	contract	drive.rs::pick_displaced	∀ held, source, now: Pending = ∅ ⇒ None. Otherwise let mine = cnt(source) and M = max_{i∈Pending} cnt(held[i].source). If M − mine ≥ 2 (rule 1), the result is Some(argmin_since{i∈Pending | cnt(held[i].source) = M}). Otherwise (rule 2) it is argmin_since{i∈Pending | cnt(held[i].source) ≥ mine ∧ now.saturating_duration_since(held[i].since) ≥ PREAUTH_GRACE}, or None if that set is empty. Corollaries: Some(i) ⇒ i < |held| ∧ held[i].unauthenticated ∧ cnt(held[i].source) ≥ mine (an admitted relay is never named, and a lighter address's slot never goes to a heavier address's newcomer); with every slot authenticated the result is None for every newcomer and every now.	drive::a_newcomer_takes_only_an_unauthenticated_slot_by_the_rule, drive::idle_sockets_from_one_address_never_turn_away_a_dialer_from_another, drive::a_slow_handshake_is_never_cut_for_a_heavier_address	a slice of Copy structs holding IpAddr (enum) and Instant, iterator chains (enumerate/filter/count/max/min_by_key) with closures including &dyn Fn, usize saturating_sub, Option ?, quantified clauses over the slice	0, 1, 3, 5, 7, 13
source-of-slash64	contract	drive.rs::source_of	∀ ip: source_of(ip) = V4(a) if ip.to_canonical() = V4(a) (an IPv4 address, or an IPv4-mapped IPv6 peer, counts as its IPv4 address); otherwise ip.to_canonical() = V6(v) and source_of(ip) = V6(u128(v) & !(2^64 − 1)). Hence for non-mapped IPv6 x,y: source_of(x) = source_of(y) ⇔ they share their /64; for IPv4 x,y: source_of(x) = source_of(y) ⇔ x = y.	drive::an_address_is_an_ipv4_address_or_an_ipv6_slash_64	std IpAddr/Ipv6Addr enum match with binding, to_canonical (std, cross-crate), u128 conversion and bitwise not/and	0, 1, 3, 12
serve-slots-refine-net-slot-displacement	refinement	drive.rs::Slots::{make_room,take,free}, drive.rs::SlotTenure::drop, drive.rs::Evictor::evict, drive.rs::HandshakeWatchdog::finish, drive.rs::{claim_deadline,serve}; model aterm_spec::derive::net_slot_displacement_model	After each Accept/Authenticate/Evict/Refuse/Deadline/Release/Close, the projection of the real slots (pending/authed/cut counts by claim state, waiting, arrivals, claims, authed_cut) is a step of NetSlotDisplacement with Cap = MAX_INFLIGHT, and its invariants hold. In code terms: (1) |held| ≤ MAX_INFLIGHT always (only the single accept loop calls take, only after make_room returned true, which it does only when a slot is free under the lock, possibly after evicting pick_displaced's choice and waiting ≤ ACCEPT_POLL for it to be freed); (2) make_room evicts only a slot pick_displaced named and whose evict() CAS won, so it is unauthenticated then; (3) each slot's claim leaves pending exactly once (watchdog-claim-exactly-once), and the listener never shuts down an AUTHED socket (authed_cut = 0), so an admitted relay never loses its slot; (4) each slot is freed exactly once, when its SlotTenure drops (panic included). Consequence: MAX_INFLIGHT admitted relays fill the listener, and a newcomer finding every slot held by authenticated relays is refused with Refused{reason = "the listener is at capacity"} and displaces nothing.	drive::serve_slots_conform_to_net_slot_displacement, drive::admitted_relays_still_fill_the_listener_and_are_never_displaced, drive::idle_unauthenticated_sockets_never_lock_a_token_holder_out	Mutex<Vec<Slot>> + Condvar wait_timeout_while, Arc<Self> methods, Vec retain/push, Drop, atomics, thread-per-connection, a loop re-scanning after a lost CAS	0, 2, 8, 10, 13
serve-event-trail	contract	drive.rs::{serve,accept_and_relay_armed}	∀ accepted connections with peer p, serve emits exactly one of: [Refused{p, "the listener is at capacity"}] when make_room is false (nothing spawned); [Refused{p, e.to_string()}] when arming fails; [Refused{p, e.to_string()}] when not admitted (e per unauth-outcome-classification or capability-handshake, e.g. "wrong drive token", DISPLACED, "no handshake within …"); or [Admitted{p, granted}, Closed{p, None | Some(e.to_string())}], where Admitted is emitted when the capability verifies, before the local socket is dialed, and Closed carries None iff the relay returned Ok. A connect_local failure after admission is Closed{p, Some(..)} (this instance's failure), never Refused. No per-connection error stops the loop; it exits only when running is cleared, or emits Stopped(..) and returns if the listener cannot be made non-blocking. No event carries a token.	drive::serve_names_each_peer_and_says_why_one_was_turned_away, drive::an_unreachable_local_socket_is_said_as_this_instances	Fn + Clone + Send closures cloned per thread, Cell<bool>, AtomicBool, thread spawn, enum NetEvent construction, a ghost event trace	0, 4, 8, 13
connect-probe	contract	drive.rs::{expect_probe_answer,probe_not_accepted}, drive.rs::dial_and_relay_pinned_inner (probe step)	expect_probe_answer(T, probe) writes exactly probe.request, then reads one byte at a time up to the first '\n' and consumes nothing past it, so the answer is never relayed. All reads share one deadline t0 + probe.within. Let line = those bytes, lossily decoded, trailing '\r's trimmed. Ok(()) iff the line completes before the deadline ∧ ¬(probe.is_refusal)(line). Refusal ⇒ Err(Other, "the remote answered {line:?}"); deadline passed or a read timed out ⇒ Err(TimedOut, "the remote accepted no connection within {max(probe.within.as_secs(),1)}s") (§5.5); EOF ⇒ Err(UnexpectedEof, "the remote closed the connection before answering"); more than 4096 bytes before the newline ⇒ Err(InvalidData, "runaway probe reply"). EINTR is retried under the same deadline; the read timeout is reset to None on success. In the dial the probe runs under within_wall_clock(probe.within), so a dribbled reply still fails with TimedOut. Composed with dial-wire-order: when the probe fails, the remote's local socket has received the probe request and none of the caller's bytes.	drive::a_probed_dial_into_a_remote_that_serves_nothing_fails_fast, drive::a_probed_dial_consumes_the_answer_and_honours_a_refusal, drive::a_probe_answer_dribbled_by_the_remote_is_cut_off_at_the_probe_bound	a byte loop over a TLS read with an Instant deadline and set_read_timeout, Vec push with a cap, String::from_utf8_lossy/trim_end_matches, a fn-pointer field is_refusal, error-kind matches	0, 1, 7, 8, 13
relay-duplex-fidelity	refinement	tls.rs::{relay,drain_decrypted,is_normal_close,is_would_block}, tls.rs::RelayState::record_err; model aterm_spec::derive::tls_buffered_relay_model	relay(T, L) behaves as two independent ordered byte pipes Dir(local→peer) and Dir(peer→local). In each, at every moment the bytes delivered to the sink are a prefix of the bytes the source produced, and once that direction's source reaches EOF and the sink keeps reading, delivered = produced, in order, nothing lost or duplicated. This holds for any segmentation, any record sizes (including one full 16 KiB record followed by small ones in a single socket read: drained after every process_new_packets, so read_tls never meets a full buffer), any send-side backpressure (writer Ok(0) waits, never drops), and plaintext rustls had already decrypted inside accept/connect (drain_decrypted moves all of it before any new network read, model action DrainBuffered; it returns Ok(true) iff the peer stream ended). No application byte the peer sends after its close_notify is delivered (§5.6). One direction's EOF half-closes only that direction: after the upload, close_notify is sent only once every queued TLS byte was pushed (bounded by RELAY_DRAIN_MAX) and the TCP write half is shut down; after the download, L's write half is shut down; the other direction stays open until its own EOF. If L hangs up entirely after its EOF, relay ends (hangup poll) instead of waiting on the remote. relay returns Ok when both directions ended normally (EOF, close_notify, reset, broken pipe: is_normal_close), otherwise the first unexpected error (record_err keeps only the first). Refines tls_buffered_relay_model (DrainBuffered is taken whenever plaintext is buffered).	tls::end_to_end_tls_capability_handshake_then_relays_a_control_exchange, tls::relay_streams_a_large_payload_under_send_side_backpressure_without_truncating, tls::relay_early_request_close_delivers_guarded_reply_but_not_late_ack, tls::relay_round_trips_guarded_artifact_ack_before_request_half_close, tls::graceful_local_eof_half_closes_without_killing_the_request_direction, tls::relay_ingests_a_full_record_followed_by_small_ones_without_tearing_down	three threads sharing Arc<Mutex<RelayState<C>>> + Condvar, generic C: DerefMut<Target=ConnectionCommon<S>> (rustls bounds), labelled loops, 16 KiB buffers, socket try_clone/shutdown, blocking I/O, error-kind matches; needs a trusted rustls record-layer model and a TCP half-close model	0, 1, 2, 7, 8, 10, 11, 13
pinned-cert-verifier	contract	tls.rs::PinnedServerVerifier::verify_server_cert, tls.rs::{cert_fingerprint,client_config,dial_handshake_error,listen_handshake_error,connect,accept}, tls.rs::PinMismatch::fmt	∀ cert DER c, pin p: cert_fingerprint(c) = SHA-256(c), and verify_server_cert(c, …) = Ok iff cert_fingerprint(c) = p; otherwise Err(InvalidCertificate(Other(PinMismatch{presented: cert_fingerprint(c), pinned: p}))). Intermediates, server name, OCSP and time are ignored. dial_handshake_error(e) = io::Error(InvalidData, "certificate mismatch: the remote presents sha256:"++hex(presented)++"; the connection's fingerprint is sha256:"++hex(pinned)) when e wraps exactly that PinMismatch, else e unchanged. listen_handshake_error(e) = io::Error(PermissionDenied, "it refused this listener's certificate") iff e wraps rustls AlertReceived(CertificateUnknown | AccessDenied), else e unchanged. client_config and server_config offer only TLS 1.3. Composed with rustls (trusted to honour the verifier and to send CertificateUnknown on its rejection): a connect over client_config(p) completes only against a server whose end-entity cert hashes to p and that proves key possession; on a mismatch both ends report the words above.	tls::a_wrong_fingerprint_pin_rejects_the_handshake	a trait impl of rustls ServerCertVerifier (&self), [u8;32] equality, Arc<dyn Error> + downcast_ref chains, enum matching on rustls::Error, format!/Display, a cross-crate Sha256, a handshake loop over complete_io (effects)	0, 1, 2, 4, 8, 11, 12
tls-exporter-agreement	contract	tls.rs::{accept,connect,server_config,client_config,init_crypto}, tls.rs::TlsTransport::exporter	accept and connect each return Ok only after the handshake completed (while is_handshaking { complete_io }) under TLS 1.3 only, and then set exporter = export_keying_material(EXPORTER_LEN bytes, EXPORTER_LABEL, context None) of their own connection. By the RFC 5705 axiom (rustls, trusted) the two ends of one session get equal exporters, and different sessions get different ones with overwhelming probability. That equality is the e_d = e_l precondition of capability-handshake. |exporter()| = EXPORTER_LEN is given by the [u8; EXPORTER_LEN] type and needs no proof.	tls::tls_handshake_exporters_match_and_a_wrong_pin_is_rejected	rustls ServerConnection/ClientConnection calls taken as axioms, Arc configs, a loop over complete_io on a TcpStream, a fixed-size array result, std::sync::Once	0, 7, 8, 12
```

### Distribution by kind (24)

| kind | count | ids |
|---|---|---|
| contract | 17 | channel-bind-verify, hex-decode-bounded, read-line-bounded, capability-handshake, endpoint-matches, roster-nonce, observe-launch-nonce, dial-pin-gate-fail-closed, dial-wire-order, poll-timeout-ceil, listener-readiness, pick-displaced-rule, source-of-slash64, serve-event-trail, connect-probe, pinned-cert-verifier, tls-exporter-agreement |
| refinement | 3 | listener-dial-after-grant, serve-slots-refine-net-slot-displacement, relay-duplex-fidelity |
| round-trip | 1 | auth-line-roundtrip |
| table-property | 1 | unauth-outcome-classification |
| invariant | 1 | watchdog-claim-exactly-once |
| other (temporal) | 1 | watchdog-deadline-safety-and-liveness |

### Gap histogram (specs blocked by each gap)

| gap | specs | |
|---|---|---|
| 0 | 24 | installed seal publishes no Clean row |
| 1 | 17 | non-scalar carriers |
| 2 | 7 | certified &self/&mut self views |
| 3 | 8 | E6 body widening |
| 4 | 7 | callee-ensures composition |
| 5 | 4 | quantified clauses |
| 7 | 13 | loops |
| 8 | 16 | effects |
| 10 | 5 | model→code refinement |
| 11 | 5 | generics |
| 12 | 5 | cross-crate citation |
| 13 | 12 | threads, closures |

No spec is blocked by gap 0 alone. The nearest are `endpoint-matches` (1, 2), `poll-timeout-ceil` (1, 3)
and `source-of-slash64` (1, 3, 12). With the integer-core refactors in §0, `poll-timeout-ceil` and
`source-of-slash64` drop gap 1 and need only gap 3 (`saturating_add`, `try_from`, `&`/`!` on `u128`), plus
gap 12 for citing std's `to_canonical` in the second.

### Proof dependency order

`poll-timeout-ceil`, `source-of-slash64`, `endpoint-matches`, `hex-decode-bounded` → `auth-line-roundtrip`,
`channel-bind-verify`, `read-line-bounded` → `capability-handshake` (over an in-memory duplex) →
`tls-exporter-agreement`, `pinned-cert-verifier` (with trusted rustls/SHA-256 axioms) → `pick-displaced-rule`,
`roster-nonce`, `unauth-outcome-classification` (pure cores) → `observe-launch-nonce` →
`watchdog-claim-exactly-once` → `watchdog-deadline-safety-and-liveness` → `listener-dial-after-grant`,
`dial-pin-gate-fail-closed`, `dial-wire-order`, `connect-probe` → `serve-slots-refine-net-slot-displacement`,
`serve-event-trail`, `listener-readiness` → `relay-duplex-fidelity` (last: three threads plus a rustls
record-layer model).

## 2. Test → spec map (TSV; all 41)

`primary` is the spec that retires the test. `also` lists the other specs the same test samples.

```tsv
file	test	primary	also
src/lib.rs	channel_binding_rejects_a_cross_channel_replay	channel-bind-verify	
src/lib.rs	capability_handshake_grants_valid_and_denies_unknown_replay_and_forgery	capability-handshake	channel-bind-verify, auth-line-roundtrip, hex-decode-bounded, read-line-bounded
src/lib.rs	a_listener_that_hangs_up_unanswered_is_named_as_such	capability-handshake	read-line-bounded
src/lib.rs	endpoint_pin_rejects_relaunch_and_host_swap	endpoint-matches	
src/tls.rs	tls_handshake_exporters_match_and_a_wrong_pin_is_rejected	tls-exporter-agreement	(mislabelled, §5.1)
src/tls.rs	end_to_end_tls_capability_handshake_then_relays_a_control_exchange	relay-duplex-fidelity	capability-handshake, tls-exporter-agreement
src/tls.rs	relay_streams_a_large_payload_under_send_side_backpressure_without_truncating	relay-duplex-fidelity	
src/tls.rs	relay_early_request_close_delivers_guarded_reply_but_not_late_ack	relay-duplex-fidelity	(late-ack half is mostly rustls, §5.6)
src/tls.rs	relay_round_trips_guarded_artifact_ack_before_request_half_close	relay-duplex-fidelity	(its model-only assertions, §5.10)
src/tls.rs	a_wrong_fingerprint_pin_rejects_the_handshake	pinned-cert-verifier	
src/tls.rs	graceful_local_eof_half_closes_without_killing_the_request_direction	relay-duplex-fidelity	
src/tls.rs	relay_ingests_a_full_record_followed_by_small_ones_without_tearing_down	relay-duplex-fidelity	
src/drive.rs	real_listener_readiness_wakes_before_the_shutdown_interval	listener-readiness	poll-timeout-ceil
src/drive.rs	idle_listener_readiness_wait_is_bounded_without_spinning	listener-readiness	poll-timeout-ceil (timing lower bound, §5.8)
src/drive.rs	driver_dials_listener_presents_capability_and_relays_to_the_local_socket	dial-wire-order	capability-handshake, relay-duplex-fidelity
src/drive.rs	a_probed_dial_into_a_remote_that_serves_nothing_fails_fast	connect-probe	unauth-outcome-classification (bound wording, §5.5)
src/drive.rs	a_probed_dial_consumes_the_answer_and_honours_a_refusal	connect-probe	dial-wire-order
src/drive.rs	listener_authenticates_its_local_socket_so_the_token_never_crosses_the_wire	capability-handshake	listener-dial-after-grant
src/drive.rs	a_stalled_pre_auth_peer_is_dropped_by_the_handshake_deadline	unauth-outcome-classification	watchdog-deadline-safety-and-liveness, listener-dial-after-grant
src/drive.rs	a_dribbling_pre_auth_peer_is_cut_off_by_the_wall_clock_deadline	watchdog-deadline-safety-and-liveness	unauth-outcome-classification, listener-dial-after-grant
src/drive.rs	a_dribbling_server_is_cut_off_by_the_dialers_wall_clock_deadline	watchdog-deadline-safety-and-liveness	unauth-outcome-classification
src/drive.rs	a_probe_answer_dribbled_by_the_remote_is_cut_off_at_the_probe_bound	connect-probe	watchdog-deadline-safety-and-liveness
src/drive.rs	a_session_roster_dribbled_by_the_remote_is_cut_off_at_the_handshake_bound	watchdog-deadline-safety-and-liveness	observe-launch-nonce, dial-pin-gate-fail-closed, unauth-outcome-classification
src/drive.rs	finishing_an_early_handshake_does_not_wait_out_a_wake_interval	(none: §4, delete)	watchdog-claim-exactly-once states its structural part
src/drive.rs	spurious_wakes_neither_shorten_nor_starve_the_watchdog_deadline	watchdog-deadline-safety-and-liveness	watchdog-claim-exactly-once (0.8 slack, §5.7)
src/drive.rs	accept_and_relay_conforms_to_net_dial_after_grant	listener-dial-after-grant	capability-handshake
src/drive.rs	a_denied_capability_never_reaches_the_local_socket	listener-dial-after-grant	(duplicate of the forged run above, §5.9)
src/drive.rs	a_matching_launch_nonce_pin_passes_and_relays_a_verb	dial-pin-gate-fail-closed	roster-nonce, observe-launch-nonce, dial-wire-order, endpoint-matches (vacuous fingerprint half, §5.2)
src/drive.rs	a_mismatching_launch_nonce_pin_is_rejected_before_relay	dial-pin-gate-fail-closed	roster-nonce, observe-launch-nonce, dial-wire-order
src/drive.rs	a_pin_with_no_observable_nonce_fails_closed	dial-pin-gate-fail-closed	roster-nonce, observe-launch-nonce, dial-wire-order
src/drive.rs	the_roster_observer_passes_a_live_pin_and_refuses_a_stale_one	roster-nonce	observe-launch-nonce, read-line-bounded
src/drive.rs	serve_names_each_peer_and_says_why_one_was_turned_away	serve-event-trail	serve-slots-refine-net-slot-displacement, unauth-outcome-classification
src/drive.rs	an_unreachable_local_socket_is_said_as_this_instances	serve-event-trail	unauth-outcome-classification
src/drive.rs	idle_unauthenticated_sockets_never_lock_a_token_holder_out	serve-slots-refine-net-slot-displacement	pick-displaced-rule
src/drive.rs	idle_sockets_from_one_address_never_turn_away_a_dialer_from_another	pick-displaced-rule	source-of-slash64, serve-slots-refine-net-slot-displacement (early return without dual-stack loopback, §5.4)
src/drive.rs	a_slow_handshake_is_never_cut_for_a_heavier_address	pick-displaced-rule	serve-slots-refine-net-slot-displacement (early return without dual-stack loopback, §5.4)
src/drive.rs	a_newcomer_takes_only_an_unauthenticated_slot_by_the_rule	pick-displaced-rule	
src/drive.rs	an_address_is_an_ipv4_address_or_an_ipv6_slash_64	source-of-slash64	
src/drive.rs	an_eviction_cuts_only_a_connection_that_has_not_authenticated	watchdog-claim-exactly-once	
src/drive.rs	serve_slots_conform_to_net_slot_displacement	serve-slots-refine-net-slot-displacement	watchdog-claim-exactly-once
src/drive.rs	admitted_relays_still_fill_the_listener_and_are_never_displaced	serve-slots-refine-net-slot-displacement	pick-displaced-rule
```

(`file` is relative to `crates/aterm-net/`.)

## 3. Coverage

- 41 tests read. 40 map to one of the 24 specs. 1 is not a spec sample (§4).
- The biggest collapses (primary tests): `relay-duplex-fidelity` (6), `watchdog-deadline-safety-and-liveness` (4),
  `capability-handshake` (3), `connect-probe` (3), `dial-pin-gate-fail-closed` (3), `pick-displaced-rule` (3),
  `serve-slots-refine-net-slot-displacement` (3).
- `hex-decode-bounded`, `auth-line-roundtrip`, `read-line-bounded`, `observe-launch-nonce` and
  `poll-timeout-ceil` retire no test on their own. They are the lemmas the handshake, roster and readiness
  specs are built on, and the place where the crate's DoS bounds (`MAX_AUTH_LINE`, the 1-byte `read_line`
  loop, `ROSTER_MAX_ROWS`/`ROSTER_MAX_LINE`) and poll rounding are actually stated.

## 4. Tests that are not spec samples

| test | why | action |
|---|---|---|
| drive::finishing_an_early_handshake_does_not_wait_out_a_wake_interval | A latency budget: it asserts that the fastest of 10 `finish()` calls takes under 20 ms. AGENTS.md says a wait is a one-minute hang detector, never a latency budget. Its only non-timing assertion (`finish() == Claim::Authed` when nothing else claimed) is part of `watchdog-claim-exactly-once`. It also reads a `#[cfg(test)]`-only field (`waiting`) that exists for it alone. | **delete** (and the `waiting` field with it if nothing else reads it) |

No test is a pure regression of a removed bug with no general property. Every regression-shaped
test (the dribbling peers, the spurious wakes, the full-record ingest, the guarded-ack half-close) is an
instance of one of the general specs above.

## 5. Implied specs that look wrong, or disagree with the code

1. **Mislabelled test.** `tls::tls_handshake_exporters_match_and_a_wrong_pin_is_rejected` dials only
   with the *correct* pin and checks exporter equality. The wrong-pin half lives in
   `tls::a_wrong_fingerprint_pin_rejects_the_handshake`. Its `len == EXPORTER_LEN` assertion is a type
   tautology (`[u8; 32]`), and `!= [0; 32]` is a probabilistic check of rustls.
2. **The pin gate's fingerprint conjunct is vacuous.** `dial_and_relay_pinned_inner` calls
   `pin.matches(&pin.fingerprint, &nonce)`, comparing the fingerprint with itself.
   `RemoteEndpoint.fingerprint` (a `String`) is never checked against the `[u8;32]` pin inside the
   `ClientConfig` the caller passes, so the two can disagree silently. The docs' claim that the observed
   fingerprint provably equals `pin.fingerprint` holds only if every caller builds both from one source.
   Either state the spec as a nonce-only guard plus a caller precondition (`hex(config pin) = pin.fingerprint`),
   or make the code check it. `endpoint-matches` is still true; only its use here is vacuous.
3. **Crypto claims are not universally true.**
   `lib::channel_binding_rejects_a_cross_channel_replay` asserts that bindings differ across exporters
   and that another token cannot verify. Those are HMAC collision and unforgeability claims: they are
   probabilistic and cannot be proved for all inputs. The provable spec is
   `verify_presented ⇔ p = channel_bind(t,e)`; distinctness is a trusted crypto axiom.
4. **Tests that pass without running.** `drive::idle_sockets_from_one_address_never_turn_away_a_dialer_from_another`
   and `drive::a_slow_handshake_is_never_cut_for_a_heavier_address` return early, and pass after an
   `eprintln!` of NOT RUN, on a host without dual-stack loopback (e.g. Windows). A green result does not
   show the property was checked. `pick-displaced-rule` covers the logic without sockets.
5. **The probe timeout words misstate the bound.** `probe_not_accepted` reports
   `within {max(as_secs, 1)}s`, so a 400 ms bound reads as "1s" and a 1.9 s bound also reads as "1s".
   `drive::a_probed_dial_into_a_remote_that_serves_nothing_fails_fast` checks only the substring
   "accepted no connection", so it does not catch this. `connect-probe` states the words as the code has
   them; fixing the wording (e.g. Debug of the Duration) is a code change.
6. **A relay assertion that is mostly about rustls.** In
   `tls::relay_early_request_close_delivers_guarded_reply_but_not_late_ack`, "the late ACK never reaches
   upstream" depends mostly on rustls refusing or dropping application data written after the client's
   own `send_close_notify`, not on `tls::relay`. The relay half of the spec (no peer bytes after
   close_notify are delivered) needs the rustls record-layer axiom.
7. **A test understates its spec.** `drive::spurious_wakes_neither_shorten_nor_starve_the_watchdog_deadline`
   asserts `closed_after ≥ 0.8·DEADLINE`. The code guarantees `≥ 1.0·DEADLINE` (remaining is measured
   from arming, and the CAS to FIRED happens only at remaining = 0), so an early fire of up to 20% would
   pass. The spec states the 1.0 bound.
8. **A timing lower bound on the OS timer, and platforms that disagree.**
   `drive::idle_listener_readiness_wait_is_bounded_without_spinning` asserts `elapsed ≥ ACCEPT_POLL − 40 ms`,
   a timing tolerance on poll rather than a hang detector. The spec (`Ok(false)` only after the full
   timeout, with `poll_timeout_millis` rounding up) is stronger and needs no tolerance. The windows arm
   does not retry on an EINTR-equivalent error while the unix arm does, so `listener-readiness` holds
   as stated only on unix until the arms agree.
9. **Duplicate test.** `drive::a_denied_capability_never_reaches_the_local_socket` repeats the forged
   run of `drive::accept_and_relay_conforms_to_net_dial_after_grant` (same forged-token path, same
   never-dialed-local assertion).
10. **Model-only assertions inside a code test.** In
    `tls::relay_round_trips_guarded_artifact_ack_before_request_half_close`, the `tls_buffered_relay_model`
    assertions (`action_enabled` on the model and on its Buggy mutant) are facts about the model alone and
    never touch the code. Only the relay run that follows binds the code (DrainBuffered before the first
    block). The model facts belong with the model in `aterm-spec`.
11. **The AUTH line round-trip is one-directional.** `parse_auth` accepts upper-case hex (`nibble` takes
    `A-F`), while `present_capability` always writes lower case. So `parse(format(x)) = x`, but
    `format(parse(l)) ≠ l` for an upper-case tag. This is harmless, and `auth-line-roundtrip` is stated in
    that direction.

## 6. The specs I would prove first (easiest, high value)

No spec here is blocked by gap 0 alone, so the order prefers the fewest and smallest gaps.

1. **`poll-timeout-ceil`**, after factoring the integer core over `(u128, u32)`. Saturating arithmetic,
   one `is_multiple_of`, one `try_from` clamp: an `omega`-class proof needing only gap 3. It retires no
   test alone, but it is the "never a busy zero poll, never an infinite −1" half of `listener-readiness`.
2. **`source-of-slash64`**, on a `u128` core plus the `to_canonical` case split. Bitwise `&`/`!` on
   `u128` and one enum match (gap 3; gap 12 only for citing std's `to_canonical`). It retires
   `drive::an_address_is_an_ipv4_address_or_an_ipv6_slash_64` and is the address lemma under
   `pick-displaced-rule`.
3. **`endpoint-matches`**. One `&self` method, two string equalities (gaps 1 and 2, the two the whole
   workspace needs first). It retires `lib::endpoint_pin_rejects_relaunch_and_host_swap`, and stating it
   surfaces §5.2.
4. **`hex-decode-bounded`** → **`auth-line-roundtrip`** → **`channel-bind-verify`**. `hex_decode` is written
   for the Trust verifier and is already under the crate's floor. Together they are the pure half of
   `capability-handshake`, the crate's central security property (3 tests).
5. **`pick-displaced-rule`**. A pure function over a slice, but it needs iterator chains and closures
   (gaps 1, 3, 5, 7, 13). It retires 3 socket-and-thread tests, two of which silently skip on hosts without
   dual-stack loopback (§5.4).

Runner-up: `unauth-outcome-classification` and `roster-nonce` (with `pin_refusal`), once `classify_unauth`
and `pin_refusal` are factored out as pure functions; each is then a finite case split or a single
iterator scan.
