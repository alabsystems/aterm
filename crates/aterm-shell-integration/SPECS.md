<!-- Copyright 2026 Andrew Yates -->
<!-- SPDX-License-Identifier: Apache-2.0 -->

# aterm-shell-integration: spec recovery from its 166 unit tests

> Status 2026-10-16: this is the crate's migration worklist. Each spec below is proved through
> Trust/Clean, and then the tests it subsumes are deleted. The 6 checks in §4 sample no spec and can
> be deleted now. Nothing has been deleted yet.

Source: `crates/aterm-shell-integration` (read only). The `#[test]` fns are src/lib.rs 2,
src/tests.rs 129, src/tests_loader.rs 21, src/tests_tty.rs 12, tests/prepare_fish_xdg_default.rs 1
and tests/prepare_zsh_unset_zdotdir.rs 1, for 166 in all. `grep -c '#\[test\]'` reports 167 because
`tests/shell_suite.rs` mentions `#[test]` in a doc comment but defines no test. Nothing was compiled or run.
The claims below come from reading the code.

Result: **45 specs** cover **160 tests**. **6 tests are not spec samples** (§4). 20 specs are about
compiled Rust in `src/lib.rs` and retire 40 tests as their primary spec. 25 specs (`sh-*`) are about the
embedded shell programs and retire 120 tests.

## 0. Notation used in the statements

**Two layers.** Specs whose id does not start with `sh-` are about compiled Rust in `src/lib.rs`, and
Trust/Clean can prove them about the compiled fn. Specs whose id starts with `sh-` are about the
**embedded shell programs**: `scripts::{ZSH,BASH,FISH,POWERSHELL}`, `ZSH_WRAPPER`, `BASH_WRAPPER` and
`WSL_LAUNCH_SH`. Each one says what a real zsh, bash, fish, pwsh or cmd does when it runs those bytes.
No Rust fn body implements these properties, so a Trust proof about a compiled fn cannot discharge them.
They need a shell-language semantics: a Clean model of the POSIX-sh/zsh/bash/fish/PowerShell subset the
scripts use, plus a refinement from that model to the real interpreters. Audit §7 has no row for this.
This file calls it gap **SH** and proposes it as a new §7 row. The nearest existing rows are 8 (effects:
process, pty, fs, env) and 10 (model→code refinement), so every `sh-*` row lists `SH,8,10`. Until SH
exists, the `sh-*` specs stay written down here and their live tests stay.

Gap 0 (a seal that publishes Clean rows) is a prerequisite for every row, so the blocking-gaps column
leaves it out. No spec here is blocked by gap 0 alone, because every Rust fn in the crate handles at least
`&str`, `String`, `Vec`, `Option` or the filesystem.

- `last_seg(p)`: the suffix of p after its last `/` or `\` (all of p if it has neither).
- `lc`: ASCII lowercase. `strip1(x,s)`: s with one trailing x removed, if present.
- `PH` = `NONCE_PLACEHOLDER` = `"@ATERM_SHELL_NONCE@"`. `TITLE` = `CMD_PROMPT_TITLE` = `"$e]0;$P$e\"`.
- `OURS` = `WSLENV_ENTRIES` = `["ATERM_SHELL_INTEGRATION_DIR/p","ATERM_SHELL_NONCE","ATERM_WSL_CWD"]`.
  `name(e)` is the prefix of e before its first `/`, and `names(OURS)` = {name(o) | o ∈ OURS}.
- `pev(p)` = `path_env_value(p)` = `String::from_utf8_lossy(p.as_os_str().as_encoded_bytes())`.
- `SET` = `script_set()`: the 10 (relative path, bytes) pairs. `ADDR` = `script_set_address()`.
- `Match(d)` ⇔ ∀(r,b)∈SET: `fs::read(d/r) = Ok(b)`.
- `pct(s)`: the OSC 7 percent encoding. Bytes in `[A-Za-z0-9_.~/-]` are kept. Every other byte becomes
  `%XX` in uppercase hex.
- `enc633(s)`: the OSC 633;E encoding. `\` becomes `\\`, `;` becomes `\x3b`, a space becomes `\x20`, and
  each byte in 0x00..=0x1F or 0x7F becomes `\xNN` in lowercase hex. Every other byte is kept.
- `Frame(p)` = ESC `]` p BEL. `suffix(n)` = `";id="++n` if n≠"" else "".
- `Session(env)` ⇔ `ATERM_CHILD` or `ATERM_SESSION_ID` is non-empty.
- `Bare(sh)`: the same interpreter run with no integration sourced. `Obs\OSC`: a shell's stdout/stderr
  with every OSC frame removed.

**Refactors that shrink the effect surface (gap 8).** Each one turns an effectful spec into a pure one
that the gap-1/3/5 lanes can prove:
- Factor `prepare_zsh`, `prepare_fish`, `prepare_wsl` and `prepare_cmd` into pure functions of the
  inherited value, as `cmd_prompt` already is: `zsh_env(base, Option<zdotdir>)`, `fish_env(base,
  Option<xdg>)`, `wsl_env(base, existing)`. This moves `injection-shape` and `cmd-prompt` off gap 8.
- Factor `collect_old_script_sets` into a pure selector over `[(name, is_dir, mtime)]` plus a delete loop.
  This moves the selection half of `gc-policy` off gap 8.
- Make `prepare_into` skip `ensure_scripts` for `Cmd`, which reads no file (see §5.1).

## 1. Specs (TSV)

Columns: `id`, `kind`, `functions`, `statement`, `subsumes` (primary and secondary tests; the primary
choice is in §2), `rust_features` (what the proof must handle), and `blocking_gaps` (audit §7 row
numbers, with `SH` for the proposed shell-semantics row).

```tsv
op-wire-roundtrip	round-trip	edge::Op::{as_str,parse}	∀ op: Op::parse(op.as_str()) = Some(op). ∀ s: parse(s) = Some(op) ⇒ s = op.as_str(), so as_str is injective. Every s outside {as_str(o)} (including "" and "write-keys") parses to None: the parser fails closed.	edge::op_wire_tokens_roundtrip_and_unknown_fails_closed	&'static str return, match on &str literals, Option<enum>, quantifier over all &str	1,3,5
connection-kind-ops	table-property	edge::ConnectionKind::ops	ops(Pull) = [ReadScreen]; ops(Push) = [WriteInput, Signal]; ops(Both) = [ReadScreen, WriteInput, Signal]. Corollary: ∀ k, ∀ op ∈ {ConfigWrite, ClipboardWrite, DeriveLoop}: op ∉ ops(k).	edge::connection_kind_ops_are_the_closed_human_fidelity_mapping	&'static [Op] slice return, enum match, slice contains	1,3
hex-codec-and-ct-eq	round-trip	lib::{hex_digit,hex,hex_nibble,from_hex,ct_eq}, id::LaunchNonce::{to_hex,from_hex,ct_eq}, edge::EdgeToken::{to_hex,from_hex,ct_eq}	(a) hex_nibble(c) = Some(v) ⇔ c ∈ 0-9 ∪ a-f ∪ A-F, v its digit value; hex_digit(n) is the (n & 15)-th char of "0123456789abcdef". (b) ∀ b: |hex(b)| = 2|b|, every char ∈ [0-9a-f], hex(b)[2i..2i+2] = hex_digit(b[i]>>4), hex_digit(b[i]&15). (c) from_hex(s,out) = Some(()) ⇔ |s| = 2|out| ∧ every byte of s is a hex digit, and then out[i] = 16·nib(s[2i]) + nib(s[2i+1]); else None (never zero-filled). (d) ∀ b: from_hex(hex(b),out) = Some ∧ out = b; hence LaunchNonce::from_hex(n.to_hex()) = Some(n), likewise EdgeToken. (e) ct_eq(a,b) ⇔ |a| = |b| ∧ ∀i a[i] = b[i], with no data-dependent early exit in the loop.	id::launch_nonce_hex_roundtrips_and_ct_eq	String push/with_capacity, &str bytes, as_chunks::<2>, zip loop with early ? return, &mut [u8] out-param, u8 shifts/masks, fold over zip, char	1,2,3,5,7
session-id-format	contract	id::SessionId::{generate,as_str,new}	∃ b:[u8;10] (what fill_random returned) with generate().as_str() = "s-" ++ hex(b): it starts with "s-", has length 22 and matches s-[0-9a-f]{20}. as_str(new(s)) = s. Distinctness of two generate() calls is the CSPRNG axiom, not a theorem.	id::session_id_generate_is_prefixed_and_distinct	String::from/push_str, [u8;10] out-param through a trust::skip FFI callee	1,4,8
decide-edge-denotation	contract	edge::{decide_edge,EdgeTable::authorize,EdgeDecision::is_permitted}, LaunchNonce::ct_eq	decide_edge(t,tok,d,op,n) = Permit ⇔ ∃ e = Rows(t)[tok] with e.op = op ∧ e.dst = d ∧ e.nonce = n; otherwise Deny, including every tok with Fresh(tok,t). authorize(t,tok,d,n) = Some(e.op) if Rows(t)[tok] = e ∧ e.dst = d ∧ e.nonce = n, else None; so decide_edge = Permit ⇔ authorize = Some(op). is_permitted(x) ⇔ x = Permit. Corollaries: (i) no reachability: the answer depends only on Rows(t)[tok], so a reverse edge, a cycle or a self-loop never permits; (ii) AuthorizeSoundness refinement: of the 16 guard combinations (token known, dst, op, nonce) only all-true permits (PermitImpliesAllGuards). The Buggy=1 model that waives dst rejects the real gate's denial at [true,false,true,true]; that rejection is the negative control.	edge::decide_edge_permits_only_exact_match, edge::authorize_returns_op_for_a_live_edge_else_none, edge::authority_does_not_flow_along_a_cycle_or_self_loop, conformance_fabric::decide_edge_conforms_to_fail_closed_gate_with_negative_control, conformance_authorize::the_real_authorization_gate_conforms_to_authorize_soundness, conformance_authorize::the_dropped_destination_mutant_is_told_apart_from_the_real_gate	&self over HashMap get (finite-map content model), SessionId String equality, [u8;16] ct_eq loop, match with guard, Option<Op>, ty_model! refinement	1,2,3,4,5,10
edge-table-ops	contract	edge::EdgeTable::{new,grant,insert,revoke,revoke_src,revoke_src_ops,src_of,edge_of,edges,len,is_empty}	new(): Rows = ∅. grant(s,d,op,n) returns tok with Rows' = Rows[tok ↦ (s,d,op,n)]; under the CSPRNG axiom Fresh(tok,t), so no row is overwritten. insert(tok,…) = false with Rows unchanged if tok ∈ dom Rows, else true with Rows' = Rows[tok ↦ …]. revoke(tok) = (tok ∈ dom Rows), Rows' = Rows ∖ {tok}. revoke_src_ops(s,ops): Rows' = {(k,e) ∈ Rows | e.src ≠ s ∨ (ops = Some(os) ∧ e.op ∉ os)}, returns |Rows| − |Rows'|. revoke_src(s) = revoke_src_ops(s,None). src_of(tok) = Rows[tok].map(src); edge_of(tok) = Rows[tok].map(Proj). edges() is a permutation of [Proj(e) | e ∈ values Rows]. len() = |Rows|; is_empty() ⇔ len() = 0. (Edge has no token field, so edges() cannot leak a capability, by typing.)	edge::revoke_makes_the_edge_deny, edge::edges_enumerates_rows_without_leaking_tokens, edge::edge_of_reads_the_row_identity_without_the_token, edge::revoke_src_sweeps_exactly_the_matching_source, edge::revoke_src_ops_filters_by_op	&mut self, HashMap insert/remove/retain with a closure, values().map().collect() (multiset spec), SessionId clone; grant/insert/revoke/revoke_src_ops are #[trust::skip] today and must be un-skipped	1,2,3,5,8,13
grant-connection	contract	edge::EdgeTable::grant_connection	src = dst ⇒ result [] and Rows' = Rows. src ≠ dst ⇒ result [(op_i,tok_i)] with [op_i] = ops(kind) in order, and Rows' = Rows[tok_i ↦ (src,dst,op_i,nonce)] for each i, so |Rows'| = |Rows| + |ops(kind)| under freshness. With decide-edge-denotation and edge-table-ops: each tok_i permits exactly (dst,op_i,nonce), and revoke_src(src) afterwards removes all of them.	edge::grant_connection_mints_exactly_the_kinds_ops, edge::grant_connection_refuses_a_self_loop, edge::grant_connection_tokens_authorize_and_revoke_src_kills_them	&mut self, iterator map with a closure capturing &mut self, collect into Vec<(Op,EdgeToken)>, calls into the skipped grant	1,2,4,5,7,13
input-ledger-run-invariant	invariant	input_backlog::InputLedger::{new,begin,end,merge_cheapest,mark_discard}	Inv(new(t)). Inv(L) ⇒ Inv(L') for begin, mark_discard and end, with nondecreasing stamps. end(n,at): inflight' = 0. n = 0 ⇒ ring and accepted unchanged. n > 0 ⇒ accepted' = accepted +sat n; if the ring is non-empty and at − back.last < COALESCE then |ring'| = |ring|, back'.end = accepted', back'.last = max(at, back.last); otherwise, if |ring| = CAPACITY, merge_cheapest first folds one run i into run i+1 (next.first := run.first, keeping the partition), then Run{end:accepted', first:at, last:at} is pushed. So |ring'| = min(|ring|+1, CAPACITY), and the newest write always keeps its own run.	input_backlog::ledger_coalescing_and_merging_stay_lower_bounds, input_backlog::ledger_a_refused_write_records_nothing	&mut self, VecDeque push_back/back_mut/remove/get_mut, Instant/Duration (opaque clock carrier), u64 saturating adds, usize->u64 try_from	1,2,3,7
ledger-oldest-unread-lower-bound	contract	input_backlog::InputLedger::oldest_unread_at	Let top = accepted + inflight, idx = top − u. oldest_unread_at(u) = None if u = 0; Some(born) if u > top; None if idx ≥ accepted (all u bytes may be in flight); else Some(r.last) for the first run r with r.end > idx (exists by Inv). Soundness: for every history of begin/end with nondecreasing stamps and every 0 < u ≤ accepted (inflight = 0), oldest_unread_at(u) = Some(a) with a ≥ Time(accepted − u), so now − a never exceeds the true wait. With inflight > 0 the answer is None or the date of an offset no older than the true one. The newest run is never merged, so oldest_unread_at(1) = the last write's stamp.	input_backlog::ledger_dates_the_oldest_unread_byte, input_backlog::ledger_in_flight_bytes_move_the_answer_newer, input_backlog::ledger_a_bracket_held_across_a_park_would_blind_the_probe	&self, VecDeque iter().find with a closure, checked_sub, Option<Instant>, history-quantified invariant (ghost Time map)	1,2,5,7,13
ledger-merge-relative-accuracy	other	input_backlog::InputLedger::{merge_cheapest,end,oldest_unread_at}	(a) merge_cheapest(now) removes the run i minimising cost_i = (ring[i+1].last − ring[i].first) / max(now − ring[i].first, 1), compared cross-multiplied in saturating u128, ties to the smallest i, and sets ring[i+1].first := ring[i].first. (b) A run j whose cost_j exceeds min_{i≠j} cost_i at every later push is never merged, so its bytes keep their date (an isolated Enter followed only by much later, closely spaced writes is a checkable instance). (c) For n ≤ 16·CAPACITY one-byte writes at spacing Δ ≥ COALESCE and any now ≥ the last stamp, ∀ u ∈ 1..=n: oldest_unread_at(u) ≥ Time(n−u) and now − oldest_unread_at(u) ≥ 0.85·(now − Time(n−u)).	input_backlog::ledger_a_full_ring_keeps_the_date_of_the_stalled_byte, input_backlog::ledger_a_ring_full_of_history_keeps_the_date_of_the_stalled_byte, input_backlog::ledger_a_stream_longer_than_the_ring_is_dated_within_a_fraction_of_its_age	zip(iter, iter.skip(1)).enumerate loop with an is_none_or closure, u128 saturating mul, VecDeque remove/get_mut, rational reasoning for the 0.85 bound, induction over a parametric stream	1,2,3,5,7,13
ledger-discard-mark	contract	input_backlog::InputLedger::{mark_discard,accepted_since_discard}	mark_discard(e): discard_mark' = Some((e, accepted + inflight)), every other field unchanged. accepted_since_discard(e') = None if discard_mark = None or its epoch ≠ e'; else Some(accepted ∸ at), at the marked offset. Corollary: a write in flight at the mark counts as before it, and a short write leaves at > accepted, so later bytes are under-counted (by at − accepted), never over-counted.	input_backlog::ledger_the_discard_mark_counts_what_was_accepted_after_it	&mut self, Option<(u64,u64)>, bool::then closure, saturating arithmetic	1,2,3
contention-arith	contract	input_backlog::Contention::{of_load,per_cpu_milli,factor_milli,stretch,fair_share_ns}	of_load(load,cpus) = default (per_cpu_milli = 0) if cpus = 0 ∨ ¬finite(load) ∨ load ≤ 0; else per_cpu_milli = u32::MAX if load/cpus·1000 ≥ 2^32−1, else trunc(load/cpus·1000) (f64 semantics). factor = clamp(per_cpu_milli, 1000, 10000). stretch(b) = b.saturating_mul(factor)/1000. fair_share_ns(w) = min(⌊w·1000/factor⌋, u64::MAX). Corollaries: b ≤ stretch(b) ≤ 10·b absent saturation; stretch is monotone in per_cpu_milli; fair_share_ns(w) ∈ [w/10, w].	input_backlog::contention_stretches_the_stall_thresholds_with_the_load_per_cpu	f64 division/compare/is_finite, f64->u32 as cast, u32 clamp, Duration saturating_mul/div (nanos normalisation), u128 arithmetic, u64::try_from	1,3
classify-table	contract	input_backlog::{classify,InputBacklog::unread}	classify(None,_,_) = Unmeasured. For Some(b): unread(b) = 0 ⇒ Clear; else stopped ⇒ Stopped; else b.canonical ⇒ Typeahead; else Stalled iff (liveness = Published ∧ b.wait ≥ S) ∨ (liveness = Watched{drew,spinning,c} ∧ b.output_backlog = Some(0) ∧ ¬drew ∧ b.wait ≥ c.stretch(spinning ? S : Q)); otherwise Pending. So Unobserved never yields Stalled, and a program seen drawing is never entered.	input_backlog::classify_decides_in_order, input_backlog::classify_enters_a_stall_only_on_evidence_the_program_is_stuck, input_backlog::classify_stretches_entry_by_the_machines_contention	Option<&struct>, struct-like enum variant match, Duration comparison, call to Contention::stretch	1,3,4
gate-table	contract	input_backlog::{gate,lagging}	lagging(b) ⇔ b.queued > 0 ∧ ¬b.canonical ∧ b.wait ≥ W. gate(b,stopped,pub) = Open if b.queued = 0 (even when spilled > 0 or pub); else Refuse if stopped ∨ (pub ∧ ¬b.canonical ∧ b.wait ≥ S); else Warn if lagging(b); else Open. Corollaries: ¬stopped ∧ ¬pub ⇒ gate ≠ Refuse at every age; canonical ∧ ¬stopped ⇒ Open.	input_backlog::the_gate_warns_on_age_and_refuses_only_a_published_stall_or_a_stopped_job	&struct with Duration fields, bool logic, Duration compare	1,3
backlog-unread-and-signal	contract	input_backlog::InputBacklog::{unread,signals_on_write}, aterm_pty::TtySignals::signals	unread(b) = b.queued +sat b.spilled.unwrap_or(0). signals_on_write(bytes) ⇔ bytes = [x] ∧ b.signals = Some(s) ∧ s.signals(x) ∧ b.spilled = Some(0) ∧ b.queued < ROOM, where s.signals(x) ⇔ x ∈ {intr, quit, susp} ∩ Some (an aterm-pty contract cited across crates).	input_backlog::a_lone_signal_character_under_isig_is_not_input, input_backlog::unread_is_kernel_plus_spill	slice pattern let [byte] = bytes, Option is_some_and with a closure, cross-crate callee contract	1,3,4,12
input-word-wire	table-property	input_backlog::InputWord::as_str	as_str maps Unmeasured, Clear, Pending, Typeahead, Stalled, Stopped to "-", "clear", "pending", "typeahead", "stalled", "stopped". The map is injective and "frozen" is not in its image.	input_backlog::input_words_are_the_wire_words	const fn match returning &'static str	1
gate-classify-refine-input-unread-model	refinement	input_backlog::{gate,classify}, aterm_spec::derive::input_unread_gate_model	proj(b,nunread) = (raw = ¬b.canonical, q = nunread, a1 = #{B ∈ [W,S] | b.wait ≥ B}). For every reachable InputUnreadGate state st with (raw,q,a1) = proj(b) and both values of stopped: gate(b,stopped,st.published) = Refuse ⇔ DriverRefused is enabled. When q < 2: gate = Refuse ⇔ ¬enabled(DriverKey), and if gate ≠ Refuse then gate = Warn ⇔ DriverKey leaves warned = 1. When ¬stopped ∧ ¬published: classify(Some(b), false, Watched{drew: reading, spinning: ¬reading, default}) = Stalled ⇔ enabled(Publish). The pure half is decidable over the model's finite space plus gate-table and classify-table; the real-pty half needs input-backlog-reading. Under Buggy=1 (static cap, screen-only fence) the model admits a step the real gate refuses: the negative control.	conformance_input_unread::gate_and_classify_are_the_models_guards_at_every_step, conformance_input_unread::a_driver_key_into_a_fresh_queue_is_written_without_a_warning, conformance_input_unread::aged_canonical_typeahead_is_written_without_a_warning, conformance_input_unread::a_slow_read_is_warned_and_written_and_the_static_cap_is_caught, conformance_input_unread::the_incident_replays_and_the_gate_withholds_the_key	ty_model! model as a Lean predicate, quantification over model states, composition with gate-table/classify-table; the projection's real side is effectful (pty FIONREAD)	1,4,5,8,10,12
bulk-stop-cut	contract	sink::bulk_stop_cut	Let L = |x|. off = 0 ⇒ (0, L). Otherwise K = the least k ≥ min(off,L) with k = L ∨ ¬Cont(x[k]); B = Bracketed(x); keep = B ? max(K,6) : K; tail = max(B ? L−6 : L, keep). Properties: min(off,L) ≤ keep ≤ tail ≤ L; x valid UTF-8 ⇒ x[..keep] ++ x[tail..] valid UTF-8; B ∧ off > 0 ⇒ x[..keep] ++ x[tail..] starts with OPEN and ends with CLOSE.	sink::a_stop_cut_keeps_the_frame_well_formed	&[u8] get/starts_with/ends_with, while loop with is_some_and closure, saturating usize ops, a UTF-8 well-formedness predicate	1,3,5,7
write-metered	contract	sink::write_metered, sink::Shared::write_stamped_through	∀ x, meter, and every FnMut w returning Ok(n ≤ |slice|), Ok(0) or Err: let off_s be the offset at the first loop entry where meter.stop_requested() holds, if any, and (keep,tail) = bulk_stop_cut(x,off_s). The slices passed to w are each ≤ METERED_SLICE and consecutive; their accepted prefixes concatenate to x with no stop, and to a prefix of x[..keep] ++ x[tail..] otherwise. meter.sent = accepted after every Ok(n). Ok(0) returns MeteredEnd{accepted, owed}, with owed = x[off..max(keep,off)] ++ x[max(tail,off)..] if a stop is requested (recomputing the cut at off if none was taken), else []. Err(e) returns (e, accepted). Running to the end returns {accepted, owed: []}. Always accepted = Σ n over the Ok(n). Windows twin: with w = write_stamped_through over a sink that accepts each slice whole, the received stream is x with no stop and x[..keep] ++ x[tail..] after one, with owed = [].	sink::the_windows_twin_finishes_a_stopped_paste, sink::the_windows_twin_writes_an_unstopped_frame_whole, sink::a_give_up_hands_back_what_the_cut_owes	generic impl FnMut parameter (higher-order spec), while loop with continue, Option<(usize,usize)>, slice get, Vec concat, BulkMeter atomics, io::Error	1,2,4,5,7,11,13
receipt-constructors	contract	sink::WriteReceipt::{new,completed,accepted,order,is_direct}, sink::WriteReceiptError::{new,after_write,refused,no_drainer,order,into_error,is_refused}, From<io::Error> for WriteReceiptError	new(a,o) requires a = 0 ⇔ o = None and yields {a, o, direct: false}. completed(a,i,o) = {accepted: a, order: a > 0 ? Some(o) : None, direct: a > 0 ∧ a = i}. after_write(e,a,o) = {error: e (the same value), receipt: new(a, a > 0 ? Some(o) : None), refused: false}. From(e) = {e, new(0,None), false}. is_refused() is true for refused(..) and false for new/after_write/no_drainer. into_error returns the stored io::Error unchanged (raw_os_error preserved). Every receipt: order() = None ⇔ accepted() = 0.	sink::receipt_error_preserves_prefix_order_and_original_io_error	struct construction, Option, bool::then_some, opaque io::Error carried by identity	1,3
reply-budget	invariant	sink::ReplyBudget::try_reserve, sink::ReplyPermit::drop, sink::SinkWriter::{reply_budget,sever_input}	Let held = reply_reserved. try_reserve(n) = None, held unchanged, if severed. Otherwise Some(permit{n}) with held' = held + n iff held + n does not overflow ∧ (held = 0 ∨ held + n ≤ REPLY_BUDGET_BYTES); else None, held unchanged. Dropping a permit: held' = held − permit.bytes. Across any interleaving: held = Σ bytes of live permits ∧ held ≤ max(REPLY_BUDGET_BYTES, the largest live permit). A lone over-budget permit is admitted only into an empty budget.	sink::reply_permits_bound_the_budget_and_release_on_drop	AtomicUsize::try_update with a closure (CAS loop), Arc<Shared>, Drop impl, concurrent interleavings	1,2,4,13
owned-master-lifecycle	invariant	sink::SinkWriter::{new,new_owned,master,project_fd_state}, sink::CountedMaster::{new,drop}, sink::owned_masters_open	OWNED_MASTERS = the number of live CountedMaster values in the process. new_owned increments it; new(fd) does not and never closes fd. For a sink from new_owned(fd): fd stays open while any Arc<SinkWriter> clone is alive, so project_fd_state() = (true,true) and writes succeed. fd is closed exactly when the last clone drops (fd_lifecycle_model NoUseAfterClose), and OWNED_MASTERS decrements after that close, never before.	owned_master_census::the_census_counts_an_owned_master_until_its_last_clone_closes_it, sink::owned_fd_stays_open_until_last_clone_drops	Arc strong-count reasoning, Drop order (field drop then fetch_sub), global static AtomicUsize, OwnedFd close (fd effect), fd_lifecycle ty_model refinement	2,8,10,13
sink-ordered-egress	refinement	sink::SinkWriter::{write_frame,write_frame_with_receipt,write_frame_nonparking,write_frame_nonparking_with_receipt,write_frame_interactive_with_receipt,write_reply,write_frame_after_reserve,write_frame_body_locked,write_frame_locked,write_frame_unparked,spill_tail_locked}, sink::Shared::{next_accepted_order,direct_order_if_spill_empty,spill_append,spill_prepend,drain_loop}	Over every interleaving of writers and the drainer, with no discard or sever and a peer that eventually reads: (1) whole-frame atomicity: K ++ (Spill minus its chunk_written prefix) is a concatenation of the Acc(x) of all frames, each contiguous, with owed bytes immediately after their own frame's accepted part; (2) total order: frames appear in strictly increasing AcceptedOrder, minted at the direct-lane or spill linearisation point (which can differ from input_epoch attempt order); (3) FIFO: while Spill ≠ ∅ no frame is written directly to the kernel; (4) no silent loss: Ok(receipt) with accepted() = a has Acc(x) = x[..a], and those bytes reach K in order unless the peer closes; a < |x| only on a peer close (Ok(0)) or an error, and an Err carries the accepted prefix and its order; (5) order = None ⇔ a = 0, and is_direct ⇔ the whole frame reached the kernel on the direct lane in this call; (6) an empty frame is Ok(0) with no order; (7) fast path: nothing spilled and kernel room ⇒ the frame lands with Spill unchanged.	sink::write_frame_is_whole_frame_atomic_across_threads, sink::write_frame_reports_full_count, sink::receipt_twins_report_actual_acceptance_in_increasing_order, sink::accepted_order_follows_forced_spill_linearization_not_attempt_epoch, sink::degraded_locked_fallback_rechecks_a_racing_spill, sink::small_write_below_spill_cap_returns_promptly_and_preserves_order, sink::declared_nonblocking_master_writes_whole_frames_in_order, sink::nonparking_fast_path_writes_inline, sink::a_whole_frame_on_the_direct_lane_is_direct_and_a_spilled_one_is_not	Mutex/Condvar/try_lock with poison recovery, AtomicU64 packed CAS (order_state), VecDeque<u8> spill, a detached drainer thread (Arc, dup(2), spawn), write/poll syscalls via aterm_pty, &self, linearizability	1,2,4,5,7,8,10,13
input-epoch-accounting	contract	sink::SinkWriter::{input_epoch,driven_input,reserve_attempt,reserve_input_attempt} and every public write entry point	Each public write call with x ≠ [] (write_frame*, write_frame_nonparking*, write_frame_interactive*, write_frame_metered* when not stopped at entry, try_write_frame_immediate*, write_reply) increments epoch by exactly 1 before it can wait or refuse, including internal delegation to the blocking body; at u64::MAX the epoch stays there and conditional writes fail closed. driven is incremented by exactly 1 for each such call except write_reply, which leaves it unchanged. An empty x changes neither counter. Corollary: a foreign write between snapshot e and a conditional immediate write makes its previous ≠ e, so it returns ConflictZero with zero bytes.	sink::public_writes_reserve_one_epoch_per_nonempty_attempt	AtomicU64 fetch_add and try_update with checked_add closure, &self, call-graph composition over all entry points	2,4,5,13
immediate-write	contract	sink::SinkWriter::{try_write_frame_immediate,try_write_frame_immediate_with_receipt,try_write_frame_immediate_if_epoch,try_write_frame_immediate_if_epoch_with_receipt,try_write_frame_immediate_reserved}	x = [] ⇒ (Full, None), no reservation. Otherwise: BusyZero if |x| > IMMEDIATE_FRAME_MAX, severed, not declared nonblocking, the fd lock or spill mutex is contended (try_lock fails), Spill ≠ ∅, or the kernel answers EAGAIN, closed or fatal; ConflictZero (conditional only) if the epoch moved since expected; Full if the kernel took x whole; PartialInDoubt{a}, 0 < a < |x|, if it took x[..a]. BusyZero/ConflictZero: no byte of x enters K or Spill, now or later, and Spill is unchanged. Full: x is appended to K contiguously. PartialInDoubt{a}: exactly x[..a] is appended and no later byte of x ever reaches K. order is Some iff accepted > 0 (a minted but unaccepted order is not returned). The call never parks: no blocking lock or syscall.	sink::peer_closed_immediate_write_has_no_accepted_order, sink::immediate_write_refuses_contention_without_delayed_injection, sink::immediate_write_refuses_spill_without_delayed_injection, immediate_egress::initial_eagain_is_busy_zero_and_never_appears_later, immediate_egress::conditional_submit_rejects_intervening_foreign_input_without_enter, immediate_egress::short_write_is_partial_in_doubt_and_tail_is_never_injected	try_lock on two mutexes with poison recovery, nonblocking write syscall result enum, atomics, &self	1,2,4,8,13
nonparking-progress	contract	sink::SinkWriter::{write_frame_nonparking_with_receipt,write_frame_interactive_with_receipt,write_frame_unparked,spill_or_when_full}	Interactive egress never parks: from every state it returns after ≤ TRY_LOCK_SPINS spin hints plus a bounded number of non-blocking syscalls, never waiting on the fd lock, kernel room or spill room. |Spill| ≥ SPILL_CAP at entry ⇒ Err with is_refused() = true, accepted = 0, order = None, nothing appended. A frame of any size below the cap is written directly or spilled, so the spill can pass the cap by at most one frame. No drainer arrangeable ⇒ Err with is_refused() = false. Non-parking egress (|x| ≤ NONPARK_MAX): while |Spill| < SPILL_CAP and a drainer exists or can be arranged, returns Ok(|x|) after bounded steps, conceding a contended lock to the spill; otherwise it delegates to the blocking body, which parks (intended backpressure). A dead fd (−1) fails with is_refused() = false.	sink::contended_nonparking_write_concedes_within_a_bounded_spin, sink::the_interactive_egress_refuses_at_the_spill_cap_and_never_parks, sink::a_large_interactive_frame_spills_instead_of_parking	bounded spin loop over try_lock, progress argument under concurrency, syscalls, Mutex, inline thread spawn	2,5,7,8,13
spill-bound	invariant	sink::Shared::spill_append, sink::SinkWriter::{write_frame_unparked,write_frame_after_reserve,spill_tail_locked}	An append under Room::Wait happens only when |Spill| ≤ SPILL_CAP (or no drainer is live). An append under Room::NoWait happens only after its caller read |Spill| < SPILL_CAP. A prepend adds at most one frame's tail plus ≤ 12 owed bytes. Hence |Spill| ≤ SPILL_CAP + P·F, P the number of concurrent non-parking producers and F the largest frame they append. One non-parking producer of frames ≤ NONPARK_MAX: |Spill| ≤ SPILL_CAP + NONPARK_MAX + 12, so retained heap is O(SPILL_CAP).	spill_bound::nonparking_spill_is_bounded_against_a_wedged_pty	VecDeque length invariant under Mutex, racy pre-check of an AtomicUsize cache, condvar wait loop, concurrency	1,2,7,13
drainer-arrangement	contract	sink::Shared::{spill_append,arrange_drainer}, sink::SinkWriter::{install_spill_arranger,arrange_pending_drainer}	Spill.draining ⇔ a drainer thread is live ∨ arrange_pending. A NoWait append (a concession) that finds ¬draining with an arranger installed (first install wins) sets draining ∧ arrange_pending, commits the bytes, spawns no thread and calls the arranger once after releasing the mutex; each later conceding append while arrange_pending holds calls it once again. With no arranger the append arranges inline (dup + spawn), leaving draining ∧ ¬arrange_pending, or returns NoDrainer on failure. arrange_pending_drainer() is a no-op unless arrange_pending; then it clears the flag and spawns, rolling draining back on failure. A Room::Wait waiter that sees arrange_pending arranges the drainer itself. So no waiter waits on a pending drainer that does not exist, and the spill eventually drains when the peer reads.	sink::a_concession_with_an_arranger_defers_the_spawn_to_the_worker, sink::without_an_arranger_the_concession_arranges_inline_as_before, sink::a_waiter_for_spill_room_arranges_a_pending_drainer_itself	OnceLock<Box<dyn Fn>> (higher-order), thread spawn and dup(2) failure paths, Mutex-guarded flags, &mut Spill, Arc<Self> receiver	1,2,8,13
egress-observers-and-fence	contract	sink::SinkWriter::{egress_drained_to_kernel,try_egress_drained_to_kernel,try_fence_egress,spilled_bytes}, sink::EgressFence	egress_drained_to_kernel() ⇔ Spill = ∅ (read through the cached atomic). try_egress_drained_to_kernel() = None iff another thread holds the spill mutex; otherwise Some(Spill = ∅), including when the mutex is poisoned; it never blocks. try_fence_egress() = Some(fence) iff the fd lock is free (a poisoned lock counts as acquired) ∧ Spill = ∅; else None, without blocking. While a fence lives no byte is appended to K by any writer or the drainer; non-parking frames queue whole in Spill and proceed in order when the fence drops.	sink::try_egress_drained_is_nonblocking_and_recovers_poison, sink::egress_drained_predicate_follows_the_spill, sink::the_egress_fence_keeps_every_frame_out_of_the_kernel_until_it_lifts	try_lock with TryLockError::Poisoned/WouldBlock, MutexGuard lifetime held in a returned struct, atomics, concurrency	1,2,13
completion-fence	contract	sink::SinkWriter::{wait_egress_drained_to_kernel,wait_egress_drained_to_kernel_for}, sink::Shared::drain_loop	wait_egress_drained_to_kernel() blocks while draining ∧ Spill ≠ ∅ and returns Spill = ∅ ∧ ¬failed. failed is sticky: set when the drainer discards bytes after a closed peer or a hard error, and when sever drops a non-empty spill; never cleared. So after a loss every later call returns false while egress_drained_to_kernel() is true. _for(limit) returns None if the bytes are still process-local at the deadline, else the same answer. Liveness: if the peer reads, a waiter returns true once the spill has drained.	sink::egress_completion_fails_closed_when_the_spill_peer_dies	Condvar wait/wait_timeout loop (trust::paired), Instant deadline arithmetic, drainer behaviour on peer close (fd effect)	2,7,8,13
metered-frame	contract	sink::SinkWriter::{write_frame_metered_with_receipt,deliver_owed_locked}, sink::BulkMeter::{new,request_stop,stop_requested,progress,abandon,settle_unmetered}, sink::Shared::{write_stamped_blocking,prepend_owed}	meter.total := |x|. x = [] or a stop requested at entry ⇒ state Stopped, receipt (0,None), nothing written, sent = 0. Otherwise the state is Writing during the frame; after return it is Failed on Err, Delivered if accepted = |x|, Stopped if a stop was requested or a discard/sever happened since entry, Failed otherwise. Always sent = accepted. With no stop the frame's projection on K is x (sink-ordered-egress); with a stop at off > 0 it is x[..keep] ++ x[tail..] (bulk-stop-cut), so a bracketed paste stays closed and no character is half-sent. Owed bytes go to the front of Spill, ahead of every later frame, unless a discard or sever drops them. A frame waiting for spill room when the stop lands appends nothing and ends Stopped with sent = 0. A spilled-whole frame counts as fully sent. abandon() ⇒ Stopped; settle_unmetered(d) ⇒ Delivered if d, else Failed.	sink::a_frame_stopped_before_it_begins_writes_nothing, sink::a_metered_frame_reports_progress_and_a_stop_drops_the_unsent_middle, sink::an_unstopped_metered_frame_is_the_plain_frame	AtomicU8 state encoding, composition with write-metered and bulk-stop-cut, poll/write syscalls, spill prepend under the fd lock, concurrency	1,2,4,7,8,13
sink-waiters-escape	other (liveness)	sink::Shared::{write_stamped_blocking,spill_append,drain_loop}, sink::SinkWriter::{write_frame_metered_with_receipt,discard_unread_input,sever_input}	Every parked party (a blocking writer in poll(POLLOUT) holding the fd lock, a Room::Wait waiter for spill room, the drainer) returns within a bounded number of PARK_RECHECK periods after any of: (a) a stop on its own meter, for a metered writer; (b) a discard (the discard epoch moves), after which it writes no more of its frame; (c) a sever; (d) the peer making room. After a discard the parked writer returns short (accepted = what the kernel took before it), and the next frame reaches the peer alone. Only (d) completes the frame. A pending drainer never strands a waiter (drainer-arrangement).	sink::a_stop_reaches_a_paste_parked_on_a_tty_that_is_not_draining, sink::a_stop_ends_a_paste_waiting_for_spill_room, input_backlog_pty::a_discard_ends_a_writer_parked_mid_frame	liveness under a fair scheduler, timed poll and condvar wait_timeout loops, atomics, a detached thread, fd effects (poll, tcflush)	2,5,7,8,13
sever-terminal	contract	sink::SinkWriter::{sever_input,is_severed}, sink::Shared::{is_severed,discarded_since,severed_error}, sink::ReplyBudget::try_reserve	sever_input() sets severed = true for good. Under the spill mutex it sets failed if Spill ≠ ∅, clears Spill, chunk_written and the spill-active bit, then wakes every room waiter. It leaves discards and the kernel queue unchanged and is idempotent. Afterwards: write_frame*, write_frame_nonparking* and write_frame_interactive* fail at entry with ErrorKind::BrokenPipe and accept nothing; try_write_frame_immediate returns BusyZero; reply_budget().try_reserve(_) = None; wait_egress_drained_to_kernel() = false if the spill was dropped; frames in flight are dropped as a discard drops them.	sink::a_sever_unblocks_every_waiter	Mutex, AtomicBool/AtomicU64, Condvar notify_all, io::ErrorKind, composition over every entry point	1,2,4,13
input-hook-arming	contract	sink::SinkWriter::{install_input_hook,rearm_input_hook,wake_input_hook}, sink::Shared::{fire_input_hook,ledger_end}	install_input_hook(h) installs h iff none is installed (first install wins) and runs nothing; installing does not set armed. fire_input_hook() = false, running nothing, if no hook is installed or armed is set; otherwise it sets armed, runs the hook once and returns true. rearm sets armed = false and runs nothing. wake_input_hook() = fire_input_hook(). Every write path that puts accepted > 0 bytes into the kernel calls fire_input_hook once after the write; an empty frame or a 0-byte write never calls it. So the hook runs at most once per arming, and never on an idle sink.	sink::input_hook_fires_once_until_rearmed, sink::an_explicit_wake_shares_the_input_hooks_arming	OnceLock<Box<dyn Fn>> (higher-order call), AtomicBool swap, call-site composition across all write paths	2,4,13
sink-ledger-complete	invariant	sink::Shared::{ledger_begin,ledger_end,write_stamped_blocking,write_stamped_count_blocking,drain_loop}, sink::SinkWriter::{try_write_frame_immediate_reserved,write_frame_unparked}	At every quiescent point ledger.accepted = |K| (minus bytes queued before the sink existed). Every aterm_pty write_some* call on the master is bracketed by ledger_begin(len) … ledger_end(n) under the fd lock, n the kernel's accepted count and the stamp taken after the syscall returns. Writers park (poll) outside any bracket, so ledger.inflight > 0 only while a write(2) is in progress. This includes the metered paste path, whose bytes are therefore dated by their own write, not by the sink's birth.	sink::every_production_master_write_is_ledgered, input_backlog_pty::a_metered_paste_is_dated_by_its_own_write	whole-module call-site property (every syscall site), Mutex<InputLedger>, Instant::now effect, threads	2,4,8,13
input-backlog-reading	contract	sink::SinkWriter::{input_backlog,input_backlog_at,tty_echo}, sink::Shared::try_spill_len, aterm_pty::{tty_echo,input_queue_len,output_queue_len}	Master not a tty, or platform not macOS ⇒ tty_echo() = None and input_backlog_at(now) = None. Otherwise Some(b) with: b.queued = FIONREAD (complete lines only in canonical mode); b.spilled = None if the spill mutex is busy, else Some(|Spill| − chunk_written), so each unread byte is counted once in unread(b); b.wait = now ∸ oldest_unread_at(queued), or 0 if that is None; b.canonical and b.signals from the same tcgetattr; b.output_backlog = TIOCOUTQ. Lower bound: if now is read before FIONREAD, b.wait ≤ the true wait of the oldest unread byte (needs sink-ledger-complete, ledger-oldest-unread-lower-bound and the kernel FIFO-queue axiom). The probe takes neither the fd lock nor a blocking spill lock.	sink::tty_echo_is_none_off_a_tty, sink::input_backlog_is_none_off_a_tty, input_backlog_pty::raw_unread_input_is_counted_and_dated, input_backlog_pty::canonical_partial_line_reads_zero, input_backlog_pty::a_writer_parked_on_a_full_queue_does_not_blind_the_probe, input_backlog_pty::a_drainer_parked_mid_chunk_counts_each_unread_byte_once	ioctl/tcgetattr effects via cross-crate aterm_pty contracts (Option results), Mutex lock with poison recovery, try_lock, Instant/Duration, a kernel FIFO axiom	1,2,4,8,12
discard-and-read-evidence	contract	sink::SinkWriter::{discard_unread_input,read_since_discard,discards,restart_discards}, input_backlog::InputLedger::{mark_discard,accepted_since_discard}, aterm_pty::{flush_input_queue,tty_passes_every_byte}	discard_unread_input(why) = None, changing nothing (no counter, Spill untouched), when input_queue_len(master) = None (off a tty or off macOS). Otherwise, in one spill critical section: discards += 1 (wrapping), restarts += [why = Restart], unsent = |Spill| − chunk_written, Spill cleared; then it wakes waiters, flushes the kernel queue (f bytes), calls ledger.mark_discard(new epoch) and returns Some(f + unsent). read_since_discard() ⇔ tty_passes_every_byte = Some(true) ∧ after = accepted_since_discard(epoch) > 0 ∧ input_queue_len < after ∧ the epoch did not move during the call ∧ ¬severed. So it is false before any discard, with no write since, and while written bytes are still queued; true once some post-discard byte was read, staying true while later bytes queue; reset at the next discard; false in canonical mode and in cbreak mode (where the line discipline eats ^C/^O/^S/^Q), because tty_passes_every_byte ≠ Some(true).	input_backlog_pty::a_discard_drops_a_parked_spill_and_the_next_key_still_arrives, input_backlog_pty::a_discard_off_a_tty_drops_nothing, input_backlog_pty::a_flush_moves_the_discard_epoch_and_not_the_restart_count, input_backlog_pty::a_read_after_a_discard_is_told_from_the_discard, input_backlog_pty::a_control_key_a_cbreak_driver_eats_is_not_a_read	Mutex critical section, atomics, Condvar notify, tcflush/FIONREAD/termios effects through cross-crate aterm_pty contracts, composition with ledger-discard-mark	1,2,4,8,12,13
```

### Distribution by kind (45)

| kind | count | ids |
|---|---|---|
| contract | 25 | wsl-cwd-env, typed-rekey-text, lf-only, merge-wslenv, cmd-prompt, augment-with-nonce, body-extract, script-set-address, injection-shape, prepare-into-unknown, ensure-scripts-writes-set, ensure-cached-memo, install-script-set, gc-policy, cache-dir, sh-osc7-report, sh-633e-and-frame, sh-marks, sh-bash-preexec-capture, sh-mux, sh-rekey-channel, sh-typed-rekey, sh-typed-upgrade, sh-pwsh-prompt, sh-pwsh-utf8 |
| equivalence | 6 | path-env-value-lossy, sh-osc7-encoder, sh-fish-empty-is-unset, sh-prompt-render, sh-fish-user-prompt, sh-user-semantics-preserved |
| invariant | 6 | content-address-isolation, scratch-suffix-unique, sh-nonce-capture, sh-bash-ps1-one-133b, sh-tty-sanity, sh-zsh-line-init-marks |
| table-property | 2 | detect-shell, script-const-lints |
| refinement | 2 | sh-body-pointer, sh-windows-launchers |
| other | 4 | sh-load-completes, sh-wrappers, sh-path-front, sh-live-managed |

### Blocking-gap histogram (specs blocked by each gap)

| gap | specs | which |
|---|---|---|
| 1 (non-scalar carriers) | 15 | detect-shell, wsl-cwd-env, typed-rekey-text, lf-only, merge-wslenv, cmd-prompt, augment-with-nonce, body-extract, script-const-lints, path-env-value-lossy, script-set-address, injection-shape, ensure-scripts-writes-set, gc-policy, sh-body-pointer |
| 2 (certified &self/&mut views) | 2 | augment-with-nonce, ensure-cached-memo |
| 3 (E6 body widening) | 11 | detect-shell, wsl-cwd-env, typed-rekey-text, lf-only, merge-wslenv, cmd-prompt, body-extract, path-env-value-lossy, script-set-address, injection-shape, gc-policy |
| 5 (quantified clauses) | 7 | detect-shell, wsl-cwd-env, merge-wslenv, cmd-prompt, augment-with-nonce, body-extract, script-const-lints |
| 7 (loops) | 8 | lf-only, merge-wslenv, augment-with-nonce, script-const-lints, path-env-value-lossy, script-set-address, ensure-scripts-writes-set, gc-policy |
| 8 (effects) | 35 | every sh-* spec (25), plus cmd-prompt, injection-shape, prepare-into-unknown, ensure-scripts-writes-set, ensure-cached-memo, install-script-set, content-address-isolation, gc-policy, scratch-suffix-unique, cache-dir |
| 10 (model→code refinement) | 25 | every sh-* spec (25) |
| 12 (cross-crate citation) | 2 | script-set-address, cache-dir |
| 13 (threads, closures) | 6 | script-set-address, ensure-cached-memo, install-script-set, content-address-isolation, scratch-suffix-unique, sh-tty-sanity |
| SH (shell-language semantics, proposed) | 25 | every sh-* spec (25) |

Gaps 0, 4, 6, 9, 11 and 14–17 appear in no row. 4 (callee-ensures composition) and 14 (Clean elaborator) will still be met while the proofs are written.

## 2. Test → spec map (TSV; all 166)

`primary` is the spec that retires the test. `also` lists the other specs the same test samples. A test in §4 shows `(none; …)`.

```tsv
src/edge.rs	op_wire_tokens_roundtrip_and_unknown_fails_closed	op-wire-roundtrip	
src/edge.rs	decide_edge_permits_only_exact_match	decide-edge-denotation	
src/edge.rs	revoke_makes_the_edge_deny	edge-table-ops	decide-edge-denotation
src/edge.rs	authorize_returns_op_for_a_live_edge_else_none	decide-edge-denotation	
src/edge.rs	edges_enumerates_rows_without_leaking_tokens	edge-table-ops	
src/edge.rs	edge_of_reads_the_row_identity_without_the_token	edge-table-ops	
src/edge.rs	authority_does_not_flow_along_a_cycle_or_self_loop	decide-edge-denotation	
src/edge.rs	revoke_src_sweeps_exactly_the_matching_source	edge-table-ops	
src/edge.rs	revoke_src_ops_filters_by_op	edge-table-ops	
src/edge.rs	connection_kind_ops_are_the_closed_human_fidelity_mapping	connection-kind-ops	
src/edge.rs	grant_connection_mints_exactly_the_kinds_ops	grant-connection	connection-kind-ops
src/edge.rs	grant_connection_refuses_a_self_loop	grant-connection	
src/edge.rs	grant_connection_tokens_authorize_and_revoke_src_kills_them	grant-connection	decide-edge-denotation, edge-table-ops
src/id.rs	session_id_generate_is_prefixed_and_distinct	session-id-format	
src/id.rs	launch_nonce_hex_roundtrips_and_ct_eq	hex-codec-and-ct-eq	
src/input_backlog.rs	ledger_dates_the_oldest_unread_byte	ledger-oldest-unread-lower-bound	
src/input_backlog.rs	ledger_in_flight_bytes_move_the_answer_newer	ledger-oldest-unread-lower-bound	
src/input_backlog.rs	ledger_a_bracket_held_across_a_park_would_blind_the_probe	ledger-oldest-unread-lower-bound	
src/input_backlog.rs	ledger_coalescing_and_merging_stay_lower_bounds	input-ledger-run-invariant	ledger-oldest-unread-lower-bound
src/input_backlog.rs	ledger_a_full_ring_keeps_the_date_of_the_stalled_byte	ledger-merge-relative-accuracy	
src/input_backlog.rs	ledger_a_ring_full_of_history_keeps_the_date_of_the_stalled_byte	ledger-merge-relative-accuracy	
src/input_backlog.rs	ledger_a_stream_longer_than_the_ring_is_dated_within_a_fraction_of_its_age	ledger-merge-relative-accuracy	
src/input_backlog.rs	ledger_a_refused_write_records_nothing	input-ledger-run-invariant	
src/input_backlog.rs	ledger_the_discard_mark_counts_what_was_accepted_after_it	ledger-discard-mark	
src/input_backlog.rs	the_gate_warns_on_age_and_refuses_only_a_published_stall_or_a_stopped_job	gate-table	
src/input_backlog.rs	contention_stretches_the_stall_thresholds_with_the_load_per_cpu	contention-arith	
src/input_backlog.rs	classify_decides_in_order	classify-table	
src/input_backlog.rs	classify_enters_a_stall_only_on_evidence_the_program_is_stuck	classify-table	gate-table
src/input_backlog.rs	classify_stretches_entry_by_the_machines_contention	classify-table	contention-arith
src/input_backlog.rs	input_words_are_the_wire_words	input-word-wire	
src/input_backlog.rs	a_lone_signal_character_under_isig_is_not_input	backlog-unread-and-signal	gate-table
src/input_backlog.rs	unread_is_kernel_plus_spill	backlog-unread-and-signal	
src/sink.rs	receipt_error_preserves_prefix_order_and_original_io_error	receipt-constructors	
src/sink.rs	tty_echo_is_none_off_a_tty	input-backlog-reading	
src/sink.rs	input_backlog_is_none_off_a_tty	input-backlog-reading	
src/sink.rs	input_hook_fires_once_until_rearmed	input-hook-arming	
src/sink.rs	an_explicit_wake_shares_the_input_hooks_arming	input-hook-arming	
src/sink.rs	every_production_master_write_is_ledgered	sink-ledger-complete	
src/sink.rs	write_frame_is_whole_frame_atomic_across_threads	sink-ordered-egress	
src/sink.rs	write_frame_reports_full_count	sink-ordered-egress	
src/sink.rs	receipt_twins_report_actual_acceptance_in_increasing_order	sink-ordered-egress	receipt-constructors
src/sink.rs	peer_closed_immediate_write_has_no_accepted_order	immediate-write	
src/sink.rs	accepted_order_follows_forced_spill_linearization_not_attempt_epoch	sink-ordered-egress	
src/sink.rs	degraded_locked_fallback_rechecks_a_racing_spill	sink-ordered-egress	
src/sink.rs	try_egress_drained_is_nonblocking_and_recovers_poison	egress-observers-and-fence	
src/sink.rs	public_writes_reserve_one_epoch_per_nonempty_attempt	input-epoch-accounting	
src/sink.rs	immediate_write_refuses_contention_without_delayed_injection	immediate-write	
src/sink.rs	immediate_write_refuses_spill_without_delayed_injection	immediate-write	
src/sink.rs	owned_fd_stays_open_until_last_clone_drops	owned-master-lifecycle	
src/sink.rs	small_write_below_spill_cap_returns_promptly_and_preserves_order	sink-ordered-egress	nonparking-progress, completion-fence
src/sink.rs	egress_completion_fails_closed_when_the_spill_peer_dies	completion-fence	egress-observers-and-fence
src/sink.rs	egress_drained_predicate_follows_the_spill	egress-observers-and-fence	
src/sink.rs	declared_nonblocking_master_writes_whole_frames_in_order	sink-ordered-egress	
src/sink.rs	contended_nonparking_write_concedes_within_a_bounded_spin	nonparking-progress	sink-ordered-egress
src/sink.rs	nonparking_fast_path_writes_inline	sink-ordered-egress	
src/sink.rs	a_whole_frame_on_the_direct_lane_is_direct_and_a_spilled_one_is_not	sink-ordered-egress	
src/sink.rs	the_egress_fence_keeps_every_frame_out_of_the_kernel_until_it_lifts	egress-observers-and-fence	
src/sink.rs	a_concession_with_an_arranger_defers_the_spawn_to_the_worker	drainer-arrangement	
src/sink.rs	without_an_arranger_the_concession_arranges_inline_as_before	drainer-arrangement	
src/sink.rs	a_stop_cut_keeps_the_frame_well_formed	bulk-stop-cut	
src/sink.rs	a_frame_stopped_before_it_begins_writes_nothing	metered-frame	
src/sink.rs	a_metered_frame_reports_progress_and_a_stop_drops_the_unsent_middle	metered-frame	bulk-stop-cut
src/sink.rs	an_unstopped_metered_frame_is_the_plain_frame	metered-frame	sink-ordered-egress
src/sink.rs	the_windows_twin_finishes_a_stopped_paste	write-metered	bulk-stop-cut
src/sink.rs	the_windows_twin_writes_an_unstopped_frame_whole	write-metered	
src/sink.rs	a_give_up_hands_back_what_the_cut_owes	write-metered	bulk-stop-cut
src/sink.rs	a_stop_reaches_a_paste_parked_on_a_tty_that_is_not_draining	sink-waiters-escape	metered-frame
src/sink.rs	a_stop_ends_a_paste_waiting_for_spill_room	sink-waiters-escape	metered-frame
src/sink.rs	the_interactive_egress_refuses_at_the_spill_cap_and_never_parks	nonparking-progress	sink-ordered-egress
src/sink.rs	a_large_interactive_frame_spills_instead_of_parking	nonparking-progress	sink-ordered-egress
src/sink.rs	a_sever_unblocks_every_waiter	sever-terminal	sink-waiters-escape, reply-budget, completion-fence
src/sink.rs	a_waiter_for_spill_room_arranges_a_pending_drainer_itself	drainer-arrangement	sink-waiters-escape
src/sink.rs	reply_permits_bound_the_budget_and_release_on_drop	reply-budget	
tests/conformance_authorize.rs	the_real_authorization_gate_conforms_to_authorize_soundness	decide-edge-denotation	
tests/conformance_authorize.rs	the_dropped_destination_mutant_is_told_apart_from_the_real_gate	decide-edge-denotation	
tests/conformance_fabric.rs	decide_edge_conforms_to_fail_closed_gate_with_negative_control	decide-edge-denotation	
tests/conformance_fabric.rs	edge_gate_renders_tla_and_proves_and_catches	-	
tests/conformance_fabric.rs	edge_gate_spec_proves_and_catches_over_its_whole_space	-	
tests/conformance_fabric.rs	sink_no_loss_proves_and_buggy_tail_drop_is_caught	-	
tests/conformance_fabric.rs	sink_no_loss_spec_proves_and_catches_over_its_whole_space	-	
tests/conformance_input_unread.rs	gate_and_classify_are_the_models_guards_at_every_step	gate-classify-refine-input-unread-model	input-backlog-reading, gate-table, classify-table
tests/conformance_input_unread.rs	a_driver_key_into_a_fresh_queue_is_written_without_a_warning	gate-classify-refine-input-unread-model	gate-table
tests/conformance_input_unread.rs	aged_canonical_typeahead_is_written_without_a_warning	gate-classify-refine-input-unread-model	gate-table
tests/conformance_input_unread.rs	a_slow_read_is_warned_and_written_and_the_static_cap_is_caught	gate-classify-refine-input-unread-model	gate-table
tests/conformance_input_unread.rs	the_incident_replays_and_the_gate_withholds_the_key	gate-classify-refine-input-unread-model	gate-table, classify-table
tests/immediate_egress.rs	initial_eagain_is_busy_zero_and_never_appears_later	immediate-write	
tests/immediate_egress.rs	conditional_submit_rejects_intervening_foreign_input_without_enter	immediate-write	input-epoch-accounting
tests/immediate_egress.rs	short_write_is_partial_in_doubt_and_tail_is_never_injected	immediate-write	
tests/input_backlog_pty.rs	raw_unread_input_is_counted_and_dated	input-backlog-reading	
tests/input_backlog_pty.rs	canonical_partial_line_reads_zero	input-backlog-reading	
tests/input_backlog_pty.rs	a_writer_parked_on_a_full_queue_does_not_blind_the_probe	input-backlog-reading	sink-ledger-complete
tests/input_backlog_pty.rs	a_drainer_parked_mid_chunk_counts_each_unread_byte_once	input-backlog-reading	
tests/input_backlog_pty.rs	a_metered_paste_is_dated_by_its_own_write	sink-ledger-complete	input-backlog-reading
tests/input_backlog_pty.rs	a_discard_ends_a_writer_parked_mid_frame	sink-waiters-escape	discard-and-read-evidence
tests/input_backlog_pty.rs	a_discard_drops_a_parked_spill_and_the_next_key_still_arrives	discard-and-read-evidence	sink-waiters-escape
tests/input_backlog_pty.rs	a_discard_off_a_tty_drops_nothing	discard-and-read-evidence	
tests/input_backlog_pty.rs	a_flush_moves_the_discard_epoch_and_not_the_restart_count	discard-and-read-evidence	
tests/input_backlog_pty.rs	a_read_after_a_discard_is_told_from_the_discard	discard-and-read-evidence	
tests/input_backlog_pty.rs	a_control_key_a_cbreak_driver_eats_is_not_a_read	discard-and-read-evidence	
tests/owned_master_census.rs	the_census_counts_an_owned_master_until_its_last_clone_closes_it	owned-master-lifecycle	
tests/spill_bound.rs	nonparking_spill_is_bounded_against_a_wedged_pty	spill-bound	
```

## 3. Coverage

- The biggest collapses are `sh-nonce-capture` (17 primary), `sh-live-managed` (12), `sh-typed-upgrade` (8),
  `injection-shape` (7), `sh-body-pointer` (7), `sh-tty-sanity` (7), `script-const-lints` (6) and
  `sh-osc7-report` (6).
- Only `path-env-value-lossy` has no primary test. Its one sample,
  `test_prepare_wsl_env_carries_path_translated_wslenv`, is retired by `injection-shape`, which
  calls `pev`. The spec is kept because `injection-shape` is stated in terms of `pev`.
- A test listed under both layers in §2 (such as `the_typed_rekey_sets_the_globals_the_marks_read`,
  `test_prepare_wsl_argv_execs_the_bash_rcfile` or `every_posix_script_is_a_loader_around_one_body`)
  is deleted only after **both** its Rust spec and its `sh-*` spec are proved. Until then, split it, so
  that its Rust half goes with the Rust proof and its live-shell half stays.
- Of the 160 covered tests, 40 can be retired by Rust proofs alone once their gaps close. The other 120
  wait on SH.

## 4. Tests that are not spec samples

Status 2026-10-16: deleted `test_scripts_contain_prompt_override`, `test_spawned_shells_never_read_the_developers_home`, `test_bash_fallback_unnonced_when_env_missing`, `test_powershell_script_references_shell_nonce_env` and `test_bash_prompt_command_guard_prevents_spurious_capture`. `test_bash_default_mode_embeds_133b_in_ps1` was not in this deletion batch and remains.

| test | why | action |
|---|---|---|
| src/tests.rs::test_scripts_contain_prompt_override | It checks only that the substring `ATERM_PROMPT_STYLE` appears in each script, which the scripts' comments alone would satisfy. `test_fish_empty_prompt_style_falls_through_to_the_user_prompt` and `test_fish_prompt_colors_are_sgr_indices_like_bash` sample the behaviour (`sh-fish-empty-is-unset`, `sh-prompt-render`). | **delete** |
| src/tests.rs::test_spawned_shells_never_read_the_developers_home | It tests the `#[cfg(test)]` fixture `shell_command` (hermetic HOME/XDG), which has no production caller. This is a property of the test harness, not of the crate. | **delete** |
| src/tests.rs::test_bash_fallback_unnonced_when_env_missing | It duplicates `test_bash_id_suffix_empty_when_nonce_unset`: the same script, cleanup, `env_remove`, `command -v` guard and empty-suffix assertion, with only a bracketed print added. | **delete** |
| src/tests.rs::test_powershell_script_references_shell_nonce_env | `test_powershell_script_unsets_shell_nonce_env_var`, whose pinned capture line contains `ATERM_SHELL_NONCE`, subsumes it, and so do the Windows live nonce tests. | **delete** |
| src/tests.rs::test_bash_default_mode_embeds_133b_in_ps1 | It pins one spelling of bash's default-mode 133;B embed. bash is on every unix host, so the live twins always run: `a_live_bash_prompt_carries_one_133b_however_many_prompts` checks for exactly one 133;B, and `test_bash_children_never_inherit_the_nonce_through_ps1` requires PS1 to carry `;id=<nonce>`. The file's own header says bash needs no static pin for this reason. | **delete** |
| src/tests.rs::test_bash_prompt_command_guard_prevents_spurious_capture | It pins the substrings of the in-prompt guard. Its live twin `test_bash_preexec_skips_prompt_command_array_siblings` runs on every unix host and checks both that nothing is captured during the prompt cycle and that the real command still is. | **delete** |

Actions: 6 delete, 0 become-spec, 0 keep.

## 5. Implied specs that look wrong, or disagree with the code

1. **`test_prepare_cmd_wraps_the_prompt_with_marks_and_cwd` depends on the test process's `PROMPT`.**
   `prepare_cmd` reads `std::env::var("PROMPT")`. If the inherited PROMPT is non-default, or already
   carries `]133;A` (as it does when the suite runs inside an aterm cmd tab, where the nonce has already
   been substituted), the `$P$G` and `133;A;id={nonce}` assertions fail. `prepare_into(Cmd)` also writes
   the whole script set to disk, although cmd reads none of it. The implied spec belongs on the pure
   `cmd_prompt` and `augment_with_nonce` (`cmd-prompt`, `augment-with-nonce`).
2. **Two memo tests compare against the wrong bytes.** `test_prepare_cached_skips_rewrite_and_self_heals`
   and `test_prepare_cached_rewrites_on_base_change` compare the bytes on disk with `scripts::ZSH`, but
   the bytes written are `lf_only(scripts::ZSH)`. On a CRLF checkout, the case `lf_only` exists for, they
   fail. `a_foreign_build_cannot_change_what_this_builds_tabs_source` makes the comparison correctly,
   with `lf_only`.
3. **`lf_only` does not remove every CR.** `test_posix_scripts_are_written_lf_only` says no CR reaches
   disk, but `lf_only` replaces only `"\r\n"`, so a lone `\r` survives. The property holds only because
   the constants contain no lone CR. `lf-only` states that as a separate const fact.
4. **The content-address isolation claim is too strong.**
   `a_foreign_build_cannot_change_what_this_builds_tabs_source` says this build's tabs are safe
   "whatever a foreign build wrote". With a warm memo, `ensure_cached` only stats
   `root/<ADDR>/aterm_shell_integration.zsh`, so a write *into* `root/<own address>` is served without
   being re-verified. The true spec (`content-address-isolation`) needs the precondition that foreign
   writes stay outside `root/<ADDR>`. A content-addressed foreign build satisfies it; a person editing
   the folder does not. Only a process's first prepare (the cold path) replaces a damaged folder.
5. **The cache-dir containment check often passes vacuously.**
   `test_cache_dir_is_the_per_user_cache_outside_the_containment_write_roots` returns early when the
   base cannot be canonicalized or already lies under a Containment write root (HOME or XDG_CACHE_HOME
   under /tmp, which is common on CI), so the containment half is often never checked.
6. **A stale `cfg(windows)` gate.** `test_detect_powershell` gates its Windows-path case on
   `cfg(windows)`, but `ShellType::detect` splits on both `/` and `\` on every host
   (`test_detect_wsl_and_cmd` checks backslash paths everywhere). The case could run on every host.
7. **Substring pins sample behaviour weakly.** `test_prepare_wsl_argv_execs_the_bash_rcfile` and the
   static pins of POWERSHELL, FISH and ZSH are substring checks on embedded code, so a comment that
   contains the needle satisfies them. The fish tests had to work around this by filtering out comment
   lines. `script-const-lints` restates the meaningful ones as decidable predicates over non-comment
   lines.
8. **Skip equals pass.** The live tests in `src/tests_loader.rs` and `src/tests_tty.rs` skip silently
   (eprintln and return) where zsh, fish or `script` is missing, and several zsh lanes run only on
   macOS. On such hosts a green suite says nothing about the `sh-*` specs. `resolve_test_shell`'s own
   comment documents this hazard.

## 6. Proof order (easiest high-value first)

No spec is blocked by gap 0 alone (§0). The order below starts with pure `&str`/`Option` functions
that need only gap 1 (string carriers) and gap 3 (calls, enum matches), so the first work on Trust here
is the non-scalar carrier.

1. **`wsl-cwd-env`** (1,3,5). This is an enum compare, `Option ?` and two `starts_with` calls, with no
   loop and no allocation beyond `to_string`. It is the smallest test of the `&str`/`Option<(String,String)>`
   carrier, and it retires 1 test.
2. **`typed-rekey-text`** (1,3). Each arm is a single `format!` with one interpolated `&str`, and the
   statement is equality with a literal template. It retires the Rust half of
   `the_typed_rekey_sets_the_globals_the_marks_read`, and it is the Rust half of `sh-typed-rekey` and
   `sh-typed-upgrade` (16 tests downstream).
3. **`detect-shell`** (1,3,5). It is a finite string table after `last_seg`/`lc`/`strip1`. It retires 3
   tests and makes the stale cfg gate of §5.6 moot.
4. **`script-set-address`, the `is_address`/`nibble_to_hex` half** (`nibble_to_hex` is a `u8` range
   match, which seal `150efe4a` already admits, plus an `as char` cast, which is gap 3). Do this before
   the SHA-256 half, which waits on gaps 12 and 13.
5. **`cmd-prompt` pure half** (1,3,5) and **`augment-with-nonce`** (1,2,5,7). Together they retire 4 cmd
   tests and the augment test, and they fix §5.1 once `prepare_cmd` takes PROMPT as a parameter.
6. **`lf-only`**, **`body-extract`** and **`script-const-lints`** (1,3/5,7). These are const evaluation
   over the `include_str!` constants. They retire 8 tests and give the static halves of five `sh-*`
   specs. They need efficient kernel string reduction over tens of KB.
7. **`merge-wslenv`** (1,3,5,7). This is a split/filter/join loop with an idempotence corollary.
8. **`injection-shape`** after the gap-8 refactor in §0 (then 1,3 only). It retires 7 tests.
9. Effectful Rust: `prepare-into-unknown`, `ensure-scripts-writes-set`, `gc-policy` (pure selector
   first), `cache-dir`, then `ensure-cached-memo`, `install-script-set`, `scratch-suffix-unique` and
   `content-address-isolation` (8, 13).
10. `sh-*` specs once gap SH exists. Start with the pure encoders (`sh-osc7-encoder`, `sh-633e-and-frame`,
    and `sh-marks`), which are byte functions in shell, then `sh-nonce-capture` (17 tests),
    `sh-wrappers`, `sh-path-front`, and last the temporal, pty-bound specs (`sh-live-managed`,
    `sh-body-pointer`, `sh-typed-upgrade`, `sh-tty-sanity`, `sh-zsh-line-init-marks`,
    `sh-user-semantics-preserved`).
