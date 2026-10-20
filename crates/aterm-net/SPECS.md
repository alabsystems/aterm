<!-- Copyright 2026 Andrew Yates -->
<!-- SPDX-License-Identifier: Apache-2.0 -->

# aterm-net: spec recovery from its unit tests

> Status 2026-10-16: this is `aterm-net`'s migration worklist. Each of the 12 specs below is
> proved through Trust/Clean about the compiled Rust function, and then the tests it subsumes are
> deleted. The one test in §4 that samples no spec is to be deleted now. No test has been deleted yet.
>
> Since the recovery pass (2026-10-16, the merge of the sessions-as-subagents work): the network
> drive left this crate (`docs/DESIGN-sessions-as-subagents-2026-10-15.md` §7) — `drive.rs` with
> its dial, launch-nonce pin, roster and probe, and `lib.rs`'s capability handshake
> (`channel_bind`, `present_capability`, `verify_capability`, `RemoteEndpoint`). Thirteen specs
> went with them (`channel-bind-verify`, `auth-line-roundtrip`, `read-line-bounded`,
> `capability-handshake`, `endpoint-matches`, `roster-nonce`, `observe-launch-nonce`,
> `dial-pin-gate-fail-closed`, `dial-wire-order`, `listener-dial-after-grant`,
> `unauth-outcome-classification`, `serve-event-trail`, `connect-probe`), and their tests with
> them. The listener slots and the handshake watchdog moved to `slots.rs`, shared by the fleet
> listener (`fleet.rs::serve`), and their tests to `slots.rs` and `fleet.rs`; the specs below
> name them there. The rest of `fleet.rs` and `fleet_member.rs` is newer than the pass and has no
> specs here yet, but for the issuer's book (`fleet-book`, added with it: S11 of the owner-surfaces
> design, 2026-10-16), whose six tests are in §2. The drive's half-close relay, `tls::relay`, went
> after it, with its one test of its own
> (`graceful_local_eof_half_closes_without_killing_the_request_direction`): the fleet's
> `tls::relay_answers` is the one relay, and `relay-duplex-fidelity` is restated for it, its other
> tests moved onto it.

Source: `crates/aterm-net` (read only). The pass read every `#[test]` (`src/lib.rs` 4, `src/tls.rs` 8,
`src/drive.rs` 29 = 41), plus the production code each one reaches. Nothing was compiled or run.
The claims below come from reading the code.

Result: **12 specs**, covering **25 tests** (one fewer since `tls::relay` went). **1 test is not a spec sample** (§4).

## 0. Notation used in the statements

- `hex(b)` = `aterm_codec::hex::encode_lower(b)`. `nib(c)` = `aterm_codec::hex::nibble(c)` (it accepts `0-9a-fA-F`).
- Claim state `st ∈ {RUNNING=0, AUTHED=1, FIRED=2, EVICTED=3}` (the `WD_*` constants in `slots.rs`);
  `Claim ∈ {Authed, Deadline, Evicted}` is what `finish()` reports. "Leaves RUNNING at most once" means
  that over any execution, the values `st` holds are RUNNING followed by at most one other value.
- `Pending(held)` = `{i | held[i].unauthenticated}`. `cnt(x)` = `|{i ∈ Pending | held[i].source = x}|`.
  `argmin_since(I)` = the least index `i ∈ I` minimising `held[i].since` (`Iterator::min_by_key`
  returns the *first* minimum).
- `Dir(a→b)` for `tls::relay_answers` is the plaintext byte stream from endpoint `a` to endpoint `b`. Its
  EOF is a local write-half shutdown or EOF on the control-socket side, or a peer `close_notify` on the
  TLS side.
- Timing statements quantify over the arming instant `t0` of a `HandshakeWatchdog` and over every
  interleaving of `unpark` and spurious `park_timeout` wakes.
- Models: the refinement specs use the committed `ty_model!` machines
  `aterm_models_core::net_slot_displacement_model` (NetSlotDisplacement) and
  `aterm_models_session::tls_buffered_relay_model` (TlsBufferedRelay). "Refines M" means
  every observed step of the code projects to a transition M admits, and every reached projected state
  satisfies M's invariants. Today each model is bound to the code by a conformance test with a negative
  control. Gap 10 is what replaces that binding with a proof.
- Prose: error strings are quoted where a test or a caller reads them (`slots::DISPLACED`,
  `fleet::AT_CAPACITY`). Elsewhere the specs pin the error *kind* and which branch fires.
- Crypto and rustls are trusted axioms, never proof obligations: SHA-256 is collision-resistant, and
  rustls implements TLS 1.3 and RFC 5705 exporters correctly (equal on both ends of one session,
  distinct across sessions with overwhelming probability).
- **Recommended refactor** that exposes pure cores (no Rust fn names them today): an integer core for
  `poll_timeout_millis` over `(as_millis: u128, subsec_nanos: u32)` and for `source_of` over `u128`, so
  that neither needs a `Duration` or `IpAddr` carrier (this drops gap 1).

## 1. Specs (TSV)

Columns: `id`, `kind`, `functions` (paths relative to `crates/aterm-net/src/`), `statement`,
`subsumes` (primary tests, `tls::`/`slots::`/`fleet::` = `src/{tls,slots,fleet}.rs`; secondary in §2),
`difficulty` (the Rust features the proof must handle), and `blocking gaps` (the numbers of the gap
table in `docs/AUDIT-trust-for-test-migration-2026-10-15.md` §7). Gap 0 (the installed `trust` channel's
seal publishes no Clean row; seal `150efe4a` fixes it but is not admitted to the channel) blocks every row.

```tsv
id	kind	functions	statement	subsumes	difficulty	blocking gaps
hex-decode-bounded	contract (lemma)	lib.rs::hex_decode, aterm_codec::hex::nibble	∀ s with b = s.as_bytes(), n = |b|: hex_decode(s) = Some(v) ⇔ n ≤ MAX_HEX ∧ n even ∧ ∀i<n nib(b[i]) ≠ None, and then |v| = n/2 ∧ ∀k<n/2 v[k] = 16·nib(b[2k]) + nib(b[2k+1]). Round trip: ∀ x with 2|x| ≤ MAX_HEX, hex_decode(hex(x)) = Some(x). No index out of bounds, no overflow. Written for the Trust verifier.	(none primary; lemma for the fleet's proof-tag and key-file reads)	while loop with a two-step index (invariant i even, |out| = i/2), slice indexing, Vec::with_capacity/push, ? on Option, u8 shift/or, a cross-crate const fn	0, 1, 3, 5, 7, 12
watchdog-claim-exactly-once	invariant	slots.rs::HandshakeWatchdog::{arm,finish,evictor}, slots.rs::Evictor::{evict,unauthenticated}, slots.rs::claim_deadline	∀ armed watchdogs: st starts at RUNNING and leaves it at most once (only CAS(RUNNING→x) writes it), to the value of whichever of finish (AUTHED), evict (EVICTED) or claim_deadline (FIRED) wins. finish() = Authed ⇔ its CAS won; = Evicted ⇔ st = EVICTED; = Deadline ⇔ st = FIRED. evict() = true and claim_deadline() = true iff that call's CAS won, and false forever after. unauthenticated() ⇔ st = RUNNING. The watchdog thread shuts the socket clone down iff the final st ∈ {FIRED, EVICTED}, never at AUTHED (that socket still carries bytes). finish() joins the thread before returning, so its socket clone is dropped and no shutdown is pending after it.	slots::an_eviction_cuts_only_a_connection_that_has_not_authenticated	Arc<AtomicU8> compare_exchange (AcqRel/Acquire), std::thread spawn/park_timeout/unpark/join, a cloned TcpStream shutdown, enum Claim, Drop	0, 2, 8, 10, 13
watchdog-deadline-safety-and-liveness	other (temporal)	slots.rs::HandshakeWatchdog::arm (thread body), slots.rs::{claim_deadline,arm_unauthenticated}	Safety: a shutdown with st = FIRED happens at a time t with t − t0 ≥ timeout, whatever the unparks or spurious wakes, because remaining is recomputed from t0 and the CAS to FIRED is attempted only at remaining = 0. Liveness: with no claim before t0 + timeout, st becomes FIRED and the socket is shut down after finitely many wakes under fair scheduling (no wake starves it). An EVICTED claim leads to shutdown on the next wake (state is re-read on every wake, and evict unparks the thread). Hence a peer that dribbles bytes (resetting any per-syscall timer) is still cut off at the bound: the fleet handshake's unauthenticated phase (TLS handshake and proof exchange, on either end) at handshake_timeout. arm_unauthenticated also sets the read and write timeouts to handshake_timeout.	slots::spurious_wakes_neither_shorten_nor_starve_the_watchdog_deadline, fleet::a_dialer_that_proves_nothing_in_time_is_timed_out	a loop in a spawned closure with Instant/Duration arithmetic (saturating_sub, elapsed), park_timeout with spurious wakeups, atomics, socket shutdown, FnOnce closures; needs a monotonic-clock and park model and fairness reasoning	0, 7, 8, 10, 13
poll-timeout-ceil	contract	slots.rs::poll_timeout_millis	∀ d: poll_timeout_millis(d) = min(⌈d.as_nanos() / 10^6⌉, i32::MAX), computed as as_millis + [subsec_nanos mod 10^6 ≠ 0] with saturation. Hence result ≥ 0 (never poll's −1 "forever"), result ≥ 1 whenever d > 0 (a positive wait never becomes a zero-timeout busy poll), and result ≥ d in milliseconds unless saturated.	(none primary; lemma for listener-readiness)	u128 saturating_add, bool→u128, is_multiple_of, i32::try_from with unwrap_or, Duration accessors	0, 1, 3
listener-readiness	contract	slots.rs::wait_listener_readable (unix/windows/other arms), slots.rs::poll_timeout_millis, fleet.rs::serve (accept loop)	wait_listener_readable(l, T) = Ok(true) iff poll/WSAPoll reported an event on l (a queued connection, or an error or hangup the next accept surfaces), which happens as soon as a connection is accept-ready, without waiting for T. Ok(false) only after the whole of T has elapsed since the call: on unix, EINTR recomputes the remaining time from the call's start and never restarts T. Any other poll failure ⇒ Err. The non-native fallback sleeps T and returns Ok(false). In serve, each iteration either accepts or waits at most ACCEPT_POLL (sleeping ACCEPT_POLL on an error), so clearing running is observed within ACCEPT_POLL plus one in-flight accept.	slots::real_listener_readiness_wakes_before_the_shutdown_interval, slots::idle_listener_readiness_wait_is_bounded_without_spinning	unsafe libc::poll / WSAPoll FFI with a pollfd, errno via last_os_error, a loop on EINTR with Instant arithmetic, cfg-gated platform arms	0, 3, 7, 8
pick-displaced-rule	contract	slots.rs::pick_displaced	∀ held, source, now, grace (the listener's, fleet::PREAUTH_GRACE): Pending = ∅ ⇒ None. Otherwise let mine = cnt(source) and M = max_{i∈Pending} cnt(held[i].source). If M − mine ≥ 2 (rule 1), the result is Some(argmin_since{i∈Pending | cnt(held[i].source) = M}). Otherwise (rule 2) it is argmin_since{i∈Pending | cnt(held[i].source) ≥ mine ∧ now.saturating_duration_since(held[i].since) ≥ grace}, or None if that set is empty. Corollaries: Some(i) ⇒ i < |held| ∧ held[i].unauthenticated ∧ cnt(held[i].source) ≥ mine (an authenticated connection is never named, and a lighter address's slot never goes to a heavier address's newcomer); with every slot authenticated the result is None for every newcomer and every now.	slots::a_newcomer_takes_only_an_unauthenticated_slot_by_the_rule, fleet::idle_sockets_from_one_address_never_turn_away_a_member_from_another, fleet::a_slow_handshake_is_never_cut_for_a_heavier_address	a slice of Copy structs holding IpAddr (enum) and Instant, iterator chains (enumerate/filter/count/max/min_by_key) with closures including &dyn Fn, usize saturating_sub, Option ?, quantified clauses over the slice	0, 1, 3, 5, 7, 13
source-of-slash64	contract	slots.rs::source_of	∀ ip: source_of(ip) = V4(a) if ip.to_canonical() = V4(a) (an IPv4 address, or an IPv4-mapped IPv6 peer, counts as its IPv4 address); otherwise ip.to_canonical() = V6(v) and source_of(ip) = V6(u128(v) & !(2^64 − 1)). Hence for non-mapped IPv6 x,y: source_of(x) = source_of(y) ⇔ they share their /64; for IPv4 x,y: source_of(x) = source_of(y) ⇔ x = y.	slots::an_address_is_an_ipv4_address_or_an_ipv6_slash_64	std IpAddr/Ipv6Addr enum match with binding, to_canonical (std, cross-crate), u128 conversion and bitwise not/and	0, 1, 3, 12
serve-slots-refine-net-slot-displacement	refinement	slots.rs::Slots::{make_room,take,free}, slots.rs::SlotTenure::drop, slots.rs::Evictor::evict, slots.rs::HandshakeWatchdog::finish, slots.rs::claim_deadline, fleet.rs::serve; model aterm_models_core::net_slot_displacement_model	After each Accept/Authenticate/Evict/Refuse/Deadline/Release/Close, the projection of the real slots (pending/authed/cut counts by claim state, waiting, arrivals, claims, authed_cut) is a step of NetSlotDisplacement with Cap = the listener's slot count (Slots::new's capacity), and its invariants hold. In code terms: (1) |held| ≤ Cap always (only the single accept loop calls take, only after make_room returned true, which it does only when a slot is free under the lock, possibly after evicting pick_displaced's choice and waiting ≤ ACCEPT_POLL for it to be freed); (2) make_room evicts only a slot pick_displaced named and whose evict() CAS won, so it is unauthenticated then; (3) each slot's claim leaves pending exactly once (watchdog-claim-exactly-once), and the listener never shuts down an AUTHED socket (authed_cut = 0), so a proved connection never loses its slot; (4) each slot is freed exactly once, when its SlotTenure drops (panic included). Consequence: Cap proved members fill the listener, and a newcomer finding every slot held by authenticated connections is refused at capacity (fleet::AT_CAPACITY) and displaces nothing.	slots::slots_conform_to_net_slot_displacement, fleet::proved_members_still_fill_the_listener_and_are_never_displaced, fleet::idle_sockets_never_lock_a_member_of_their_own_address_out	Mutex<Vec<Slot>> + Condvar wait_timeout_while, Arc<Self> methods, Vec retain/push, Drop, atomics, thread-per-connection, a loop re-scanning after a lost CAS	0, 2, 8, 10, 13
relay-duplex-fidelity	refinement	tls.rs::{relay_answers,drain_decrypted,is_normal_close,is_would_block}, tls.rs::RelayState::record_err; model aterm_models_session::tls_buffered_relay_model	relay_answers(T, L, g) carries two ordered byte pipes Dir(local→peer) and Dir(peer→local). In each, at every moment the bytes delivered to the sink are a prefix of the bytes the source produced, and once that direction's source reaches EOF and the sink keeps reading, delivered = produced, in order, nothing lost or duplicated. This holds for any segmentation, any record sizes (including one full 16 KiB record followed by small ones in a single socket read: drained after every process_new_packets, so read_tls never meets a full buffer), any send-side backpressure (writer Ok(0) waits, never drops), and plaintext rustls had already decrypted inside accept/connect (drain_decrypted moves all of it before any new network read, model action DrainBuffered; it returns Ok(true) iff the peer stream ended). No application byte the peer sends after its close_notify is delivered (§5.6). L's EOF ends the exchange both ways: every queued TLS byte is pushed first (bounded by RELAY_DRAIN_MAX), then L is hung up, g is dropped, and only then does the peer get a close_notify (as far as the socket takes it at once) and the FIN. The peer's end (close_notify, FIN, reset or a fatal error) reaches L as a shutdown of L's write half after every decrypted byte was flushed; L may still answer, and the exchange ends once it does or hangs up. relay_answers returns Ok when the exchange ended normally (EOF, close_notify, reset, broken pipe: is_normal_close), otherwise the first unexpected error (record_err keeps only the first). Refines tls_buffered_relay_model (DrainBuffered is taken whenever plaintext is buffered).	tls::a_pinned_tls_stream_relays_a_control_exchange_end_to_end, tls::relay_streams_a_large_payload_under_send_side_backpressure_without_truncating, tls::relay_early_request_close_delivers_guarded_reply_but_not_late_ack, tls::relay_round_trips_guarded_artifact_ack_before_request_half_close, tls::relay_ingests_a_full_record_followed_by_small_ones_without_tearing_down	three threads sharing Arc<Mutex<RelayState<C>>> + Condvar, generic C: DerefMut<Target=ConnectionCommon<S>> (rustls bounds), labelled loops, 16 KiB buffers, socket try_clone/shutdown, blocking I/O, error-kind matches; needs a trusted rustls record-layer model and a TCP half-close model	0, 1, 2, 7, 8, 10, 11, 13
pinned-cert-verifier	contract	tls.rs::PinnedServerVerifier::verify_server_cert, tls.rs::{cert_fingerprint,client_config,dial_handshake_error,listen_handshake_error,connect,accept}, tls.rs::PinMismatch::fmt	∀ cert DER c, pin p: cert_fingerprint(c) = SHA-256(c), and verify_server_cert(c, …) = Ok iff cert_fingerprint(c) = p; otherwise Err(InvalidCertificate(Other(PinMismatch{presented: cert_fingerprint(c), pinned: p}))). Intermediates, server name, OCSP and time are ignored. dial_handshake_error(e) = io::Error(InvalidData, "certificate mismatch: the remote presents sha256:"++hex(presented)++"; the connection's fingerprint is sha256:"++hex(pinned)) when e wraps exactly that PinMismatch, else e unchanged. listen_handshake_error(e) = io::Error(PermissionDenied, "it refused this listener's certificate") iff e wraps rustls AlertReceived(CertificateUnknown | AccessDenied), else e unchanged. client_config and server_config offer only TLS 1.3. Composed with rustls (trusted to honour the verifier and to send CertificateUnknown on its rejection): a connect over client_config(p) completes only against a server whose end-entity cert hashes to p and that proves key possession; on a mismatch both ends report the words above.	tls::a_wrong_fingerprint_pin_rejects_the_handshake	a trait impl of rustls ServerCertVerifier (&self), [u8;32] equality, Arc<dyn Error> + downcast_ref chains, enum matching on rustls::Error, format!/Display, a cross-crate Sha256, a handshake loop over complete_io (effects)	0, 1, 2, 4, 8, 11, 12
fleet-book	contract	fleet_member.rs::{serve_book,fetch_book,judge,copies_of,with_copies,merge_book,read_issuer_book,holds_issuer_book,write_issuer_book,read_peers}	serve_book(s, dir, node) writes one answer: when dir holds CODE_FILE, node() = Ok(n) with is_node_id(n) and read_peers(dir) = Ok(b), "OK k node=n" then the k = min(|F|, PEERS_MAX) first entries F of b with via ∈ {Joined, Manual}, one Peer::line each, in b's order; otherwise "ERR w [node=n ]why" with w = BOOK_NONE iff dir holds no CODE_FILE, else BOOK_NOT_NOW, node= present iff node() gave a node id. fetch_book(t, …) = Ok(book) only for an OK head whose node= is t.node when t.node is Some, book.peers being the first-hand lines that parse; an ERR head gives NoBook (word BOOK_NONE) or NotNow (any other word), each with the answering address and the node it named; a NO op handshake gives Older(address). judge(r, found, want, may_ask): Refused(x) iff r = Err(x), x ∈ {Older, NoBook} and x is the issuer's (its node = want, or it named none and came from found); Kept for NotNow from the issuer, OtherNode, Unreached, and any refusal not the issuer's unless may_ask ∧ found = Some(a) ∧ a ≠ its address, which gives AskAt(a); Book(b) iff r = Ok(b). copies_of(book, own): every element has via = IssuerBook, none has node own or book.node, pairwise distinct by node and by Peer::same_place, at most PEERS_MAX, in book.peers' order. with_copies(b, c): b is a prefix of the result, each appended element is from c in order and shares no node or place with an element before it, and the result is at most max(|b|, PEERS_MAX) long. merge_book(b, f, own) = with_copies(b without IssuerBook entries, copies_of(f, own)). read_issuer_book(dir, n) after write_issuer_book(dir, n, c) = the IssuerBook elements of c, at most PEERS_MAX; = [] when the file's head names another node, a later line is no IssuerBook peer, or the file is missing or not private. read_peers never returns an IssuerBook entry. holds_issuer_book(dir, n) ⇔ dir holds ISSUER_BOOK_FILE, private, whose first line is the head naming n, however few copies follow (an empty copy is a book that came).	fleet_member::the_issuer_serves_its_book_and_any_other_machine_refuses, fleet_member::a_merge_replaces_the_copy_whole_dedupes_and_is_bounded, fleet_member::a_copy_of_the_issuers_book_yields_to_the_machines_own_entries, fleet_member::a_copy_is_read_only_beside_the_issuer_it_came_from, fleet_member::a_machine_at_the_issuers_old_address_never_speaks_for_the_issuer (its judge half), fleet_member::an_issuer_that_cannot_share_its_book_now_leaves_the_copy (its judge half)	Vec/String/Option/enum carriers (Peer, Book, BookUnread), iterator chains with closures, an io::Write sink, std::fs through read_private/write_private (effects); the judge half of the two read_whole tests stands on a TLS dial and the issuer seat's threads, which stay tests	0, 1, 3, 5, 7, 8, 13
tls-exporter-agreement	contract	tls.rs::{accept,connect,server_config,client_config,init_crypto}, tls.rs::TlsTransport::exporter	accept and connect each return Ok only after the handshake completed (while is_handshaking { complete_io }) under TLS 1.3 only, and then set exporter = export_keying_material(EXPORTER_LEN bytes, EXPORTER_LABEL, context None) of their own connection. By the RFC 5705 axiom (rustls, trusted) the two ends of one session get equal exporters, and different sessions get different ones with overwhelming probability. |exporter()| = EXPORTER_LEN is given by the [u8; EXPORTER_LEN] type and needs no proof.	tls::tls_handshake_exporters_match_and_a_wrong_pin_is_rejected	rustls ServerConnection/ClientConnection calls taken as axioms, Arc configs, a loop over complete_io on a TcpStream, a fixed-size array result, std::sync::Once	0, 7, 8, 12
```

### Distribution by kind (12)

| kind | count | ids |
|---|---|---|
| contract | 8 | hex-decode-bounded, poll-timeout-ceil, listener-readiness, pick-displaced-rule, source-of-slash64, pinned-cert-verifier, fleet-book, tls-exporter-agreement |
| refinement | 2 | serve-slots-refine-net-slot-displacement, relay-duplex-fidelity |
| invariant | 1 | watchdog-claim-exactly-once |
| other (temporal) | 1 | watchdog-deadline-safety-and-liveness |

### Gap histogram (specs blocked by each gap)

| gap | specs | |
|---|---|---|
| 0 | 12 | installed seal publishes no Clean row |
| 1 | 7 | non-scalar carriers |
| 2 | 4 | certified &self/&mut self views |
| 3 | 6 | E6 body widening |
| 4 | 1 | callee-ensures composition |
| 5 | 3 | quantified clauses |
| 7 | 7 | loops |
| 8 | 8 | effects |
| 10 | 4 | model→code refinement |
| 11 | 2 | generics |
| 12 | 4 | cross-crate citation |
| 13 | 6 | threads, closures |

No spec is blocked by gap 0 alone. The nearest are `poll-timeout-ceil` (1, 3) and `source-of-slash64`
(1, 3, 12). With the integer-core refactor in §0 both drop gap 1 and need only gap 3 (`saturating_add`,
`try_from`, `&`/`!` on `u128`), plus gap 12 for citing std's `to_canonical` in the second.

### Proof dependency order

`poll-timeout-ceil`, `source-of-slash64`, `hex-decode-bounded` → `tls-exporter-agreement`,
`pinned-cert-verifier` (with trusted rustls/SHA-256 axioms) → `pick-displaced-rule` (pure core) →
`watchdog-claim-exactly-once` → `watchdog-deadline-safety-and-liveness` →
`serve-slots-refine-net-slot-displacement`, `listener-readiness` → `relay-duplex-fidelity` (last: three
threads plus a rustls record-layer model). `fleet-book` stands apart: its pure halves (`copies_of`,
`with_copies`, `judge`) first, then `serve_book` and the copy's file with the effects gap.

## 2. Test → spec map (TSV; all 26)

`primary` is the spec that retires the test. `also` lists the other specs the same test samples.

```tsv
file	test	primary	also
src/tls.rs	tls_handshake_exporters_match_and_a_wrong_pin_is_rejected	tls-exporter-agreement	(mislabelled, §5.1)
src/tls.rs	a_pinned_tls_stream_relays_a_control_exchange_end_to_end	relay-duplex-fidelity	tls-exporter-agreement
src/tls.rs	relay_streams_a_large_payload_under_send_side_backpressure_without_truncating	relay-duplex-fidelity	
src/tls.rs	relay_early_request_close_delivers_guarded_reply_but_not_late_ack	relay-duplex-fidelity	(late-ack half is mostly rustls, §5.3)
src/tls.rs	relay_round_trips_guarded_artifact_ack_before_request_half_close	relay-duplex-fidelity	(its model-only assertions, §5.6)
src/tls.rs	a_wrong_fingerprint_pin_rejects_the_handshake	pinned-cert-verifier	
src/tls.rs	relay_ingests_a_full_record_followed_by_small_ones_without_tearing_down	relay-duplex-fidelity	
src/slots.rs	real_listener_readiness_wakes_before_the_shutdown_interval	listener-readiness	poll-timeout-ceil
src/slots.rs	idle_listener_readiness_wait_is_bounded_without_spinning	listener-readiness	poll-timeout-ceil (timing lower bound, §5.5)
src/slots.rs	finishing_an_early_handshake_does_not_wait_out_a_wake_interval	(none: §4, delete)	watchdog-claim-exactly-once states its structural part
src/slots.rs	spurious_wakes_neither_shorten_nor_starve_the_watchdog_deadline	watchdog-deadline-safety-and-liveness	watchdog-claim-exactly-once (0.8 slack, §5.4)
src/slots.rs	a_newcomer_takes_only_an_unauthenticated_slot_by_the_rule	pick-displaced-rule	
src/slots.rs	an_address_is_an_ipv4_address_or_an_ipv6_slash_64	source-of-slash64	
src/slots.rs	an_eviction_cuts_only_a_connection_that_has_not_authenticated	watchdog-claim-exactly-once	
src/slots.rs	slots_conform_to_net_slot_displacement	serve-slots-refine-net-slot-displacement	watchdog-claim-exactly-once
src/fleet.rs	a_dialer_that_proves_nothing_in_time_is_timed_out	watchdog-deadline-safety-and-liveness	
src/fleet.rs	idle_sockets_from_one_address_never_turn_away_a_member_from_another	pick-displaced-rule	source-of-slash64, serve-slots-refine-net-slot-displacement (early return without dual-stack loopback, §5.2)
src/fleet.rs	a_slow_handshake_is_never_cut_for_a_heavier_address	pick-displaced-rule	serve-slots-refine-net-slot-displacement (early return without dual-stack loopback, §5.2)
src/fleet.rs	idle_sockets_never_lock_a_member_of_their_own_address_out	serve-slots-refine-net-slot-displacement	pick-displaced-rule
src/fleet.rs	proved_members_still_fill_the_listener_and_are_never_displaced	serve-slots-refine-net-slot-displacement	pick-displaced-rule
src/fleet_member_tests.rs	the_issuer_serves_its_book_and_any_other_machine_refuses	fleet-book	
src/fleet_member_tests.rs	a_merge_replaces_the_copy_whole_dedupes_and_is_bounded	fleet-book	
src/fleet_member_tests.rs	a_copy_of_the_issuers_book_yields_to_the_machines_own_entries	fleet-book	
src/fleet_member_tests.rs	a_copy_is_read_only_beside_the_issuer_it_came_from	fleet-book	
src/fleet_member_tests.rs	a_machine_at_the_issuers_old_address_never_speaks_for_the_issuer	fleet-book	(its read_whole half: the issuer's survey finding it at its name stays a test)
src/fleet_member_tests.rs	an_issuer_that_cannot_share_its_book_now_leaves_the_copy	fleet-book	(its read_whole half stays a test)
```

(`file` is relative to `crates/aterm-net/`.)

## 3. Coverage

- 26 tests: the pass's `tls.rs` and `drive.rs` tests that survived, under their names and files now,
  and the six `fleet-book` tests. 25 map to one of the 12 specs. 1 is not a spec sample (§4).
- The biggest collapses (primary tests): `relay-duplex-fidelity` (5), `pick-displaced-rule` (3),
  `serve-slots-refine-net-slot-displacement` (3).
- `hex-decode-bounded` and `poll-timeout-ceil` retire no test on their own. They are the lemmas the
  fleet's reads and the readiness spec are built on, and the place where `MAX_HEX` and poll rounding are
  actually stated.

## 4. Tests that are not spec samples

| test | why | action |
|---|---|---|
| slots::finishing_an_early_handshake_does_not_wait_out_a_wake_interval | A latency budget: it asserts that the fastest of 10 `finish()` calls takes under 20 ms. AGENTS.md says a wait is a one-minute hang detector, never a latency budget. Its only non-timing assertion (`finish() == Claim::Authed` when nothing else claimed) is part of `watchdog-claim-exactly-once`. It also reads a `#[cfg(test)]`-only field (`waiting`) that exists for it alone. | **delete** (and the `waiting` field with it if nothing else reads it) |

No test is a pure regression of a removed bug with no general property. Every regression-shaped
test (the silent dialer, the spurious wakes, the full-record ingest, the guarded-ack half-close) is an
instance of one of the general specs above.

## 5. Implied specs that look wrong, or disagree with the code

1. **Mislabelled test.** `tls::tls_handshake_exporters_match_and_a_wrong_pin_is_rejected` dials only
   with the *correct* pin and checks exporter equality. The wrong-pin half lives in
   `tls::a_wrong_fingerprint_pin_rejects_the_handshake`. Its `len == EXPORTER_LEN` assertion is a type
   tautology (`[u8; 32]`), and `!= [0; 32]` is a probabilistic check of rustls.
2. **Tests that pass without running.** `fleet::idle_sockets_from_one_address_never_turn_away_a_member_from_another`
   and `fleet::a_slow_handshake_is_never_cut_for_a_heavier_address` return early, and pass after an
   `eprintln!` of NOT RUN, on a host without dual-stack loopback (e.g. Windows). A green result does not
   show the property was checked. `pick-displaced-rule` covers the logic without sockets.
3. **A relay assertion that is mostly about rustls.** In
   `tls::relay_early_request_close_delivers_guarded_reply_but_not_late_ack`, "the late ACK never reaches
   upstream" depends mostly on rustls refusing or dropping application data written after the client's
   own `send_close_notify`, not on `tls::relay_answers`. The relay half of the spec (no peer bytes after
   close_notify are delivered) needs the rustls record-layer axiom.
4. **A test understates its spec.** `slots::spurious_wakes_neither_shorten_nor_starve_the_watchdog_deadline`
   asserts `closed_after ≥ 0.8·DEADLINE`. The code guarantees `≥ 1.0·DEADLINE` (remaining is measured
   from arming, and the CAS to FIRED happens only at remaining = 0), so an early fire of up to 20% would
   pass. The spec states the 1.0 bound.
5. **A timing lower bound on the OS timer, and platforms that disagree.**
   `slots::idle_listener_readiness_wait_is_bounded_without_spinning` asserts `elapsed ≥ ACCEPT_POLL − 40 ms`,
   a timing tolerance on poll rather than a hang detector. The spec (`Ok(false)` only after the full
   timeout, with `poll_timeout_millis` rounding up) is stronger and needs no tolerance. The windows arm
   does not retry on an EINTR-equivalent error while the unix arm does, so `listener-readiness` holds
   as stated only on unix until the arms agree.
6. **Model-only assertions inside a code test.** In
   `tls::relay_round_trips_guarded_artifact_ack_before_request_half_close`, the `tls_buffered_relay_model`
   assertions (`action_enabled` on the model and on its Buggy mutant) are facts about the model alone and
   never touch the code. Only the relay run that follows binds the code (DrainBuffered before the first
   block). The model facts belong with the model in `aterm-models-session`.

## 6. The specs I would prove first (easiest, high value)

No spec here is blocked by gap 0 alone, so the order prefers the fewest and smallest gaps.

1. **`poll-timeout-ceil`**, after factoring the integer core over `(u128, u32)`. Saturating arithmetic,
   one `is_multiple_of`, one `try_from` clamp: an `omega`-class proof needing only gap 3. It retires no
   test alone, but it is the "never a busy zero poll, never an infinite −1" half of `listener-readiness`.
2. **`source-of-slash64`**, on a `u128` core plus the `to_canonical` case split. Bitwise `&`/`!` on
   `u128` and one enum match (gap 3; gap 12 only for citing std's `to_canonical`). It retires
   `slots::an_address_is_an_ipv4_address_or_an_ipv6_slash_64` and is the address lemma under
   `pick-displaced-rule`.
3. **`hex-decode-bounded`**. `hex_decode` is written for the Trust verifier and is already under the
   crate's floor; it is the lemma under the fleet's proof-tag and key-file reads.
4. **`pick-displaced-rule`**. A pure function over a slice, but it needs iterator chains and closures
   (gaps 1, 3, 5, 7, 13). It retires 3 tests, two of them socket-and-thread tests that silently skip on
   hosts without dual-stack loopback (§5.2).
