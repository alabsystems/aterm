<!-- Copyright 2026 Andrew Yates -->
<!-- SPDX-License-Identifier: Apache-2.0 -->

# aterm-parser: spec recovery from its 172 unit tests

> Status 2026-10-16, after the merge with the tests-and-gates audit: the rows naming 2 tests it
> deleted and 1 spec whose functions it deleted are gone, and the spec rows no longer cite those
> tests. Counts elsewhere in this file were taken before.

> Status 2026-10-15: the 13 checks in §4 that sample no spec and the test-only
> `advance_batch` path were deleted in `a1d10c265`. Its shared UTF-8/control case is
> retained on production `advance_fast`.
> Status 2026-10-16: 64 more were deleted as duplicates of the exhaustive and differential
> tests (the small `simd`/`utf8_simd` scan cases, the `assert_invariants_after_*` samples,
> the `utf8_errors` cases `ill_formed_utf8_is_replaced_per_maximal_subpart` subsumes,
> `refinement.rs` and `table_validity.rs`); their rows are gone from §2, while §1's `subsumes`
> column still names them as the recovery found them.
> Status 2026-10-16 (later): `Parser::advance` and its byte path (`process_byte`, Latin-1 at
> Ground) were deleted, since only tests called them; `advance_fast` is the one entry point. The
> specs `advance-is-byte-fold`, `advance-fast-agrees-with-advance` and `byte-path-ground-high-bytes`
> went with them. The 39 specs below are the crate's migration worklist: each is proved through
> Trust/Clean, then the tests it subsumes are deleted.

Source: `crates/aterm-parser` (read only). I read every `#[test]`
(simd.rs 15, utf8_simd.rs 11, tests/{basic 17, csi 18, subparams 8, empty_params 2, c1 19,
string_cancel 4, batch 17, carry 18, invariants 12, utf8_errors 13, utf8_parity 7, refinement 3,
table_validity 3, performance 3}, tests/fuzz_parser.rs 2 = 172) and the code each one reaches.
Nothing was compiled or run. The claims below come from reading the code.

Result: **42 specs**, covering **159 tests**. **13 tests are not spec samples** (§4).

## 0. Notation used in the statements

- `Reach`: the parsers reachable from `Parser::new()` (C1 off) or `Parser::with_c1_controls()` (C1 on)
  by any sequence of `advance_fast`, `reset` and accepted `restore_carry` calls.
- `Obs(run)`: the *flattened* trace of `ActionSink` calls a run makes. A bulk call is expanded to the
  per-element calls its default implementation makes (`print_ascii_bulk`/`print_unicode_bulk` become
  `print`*, `dcs_put_bulk` becomes `dcs_put`*, `apc_put_bulk` becomes `apc_put`*), and
  `osc_dispatch_with_terminator` becomes `osc_dispatch`. `RecordingSink` already observes this
  flattening. Without it, "same output" is false between a whole read and one-byte reads, because
  they group calls differently.
- `step_u(p,b)` = the UTF-8-aware single-byte step that `advance_fast(p,&[b])` performs:
  Ground with `utf8_len>0` -> `process_utf8_byte`; Ground with `is_utf8_lead(b)` -> `start_utf8`;
  Ground with `b>=0x80` -> `process_ground_special_byte`; otherwise `process_byte_inner`.
  **Recommendation:** add this as a spec-only function. Specs 10-12 are stated against it, and no Rust
  function names it today.
- `View(p)` = `(state, carry_unhooking(), dcs_active, apc_active, osc_discard, c1_controls_enabled)`.
  "Same final parser" means equal `View`, never field equality. The CSI fast parser and the per-byte
  state machine leave *different* stale `params`/`current_param`/`param_started`/`last_was_colon` at Ground (§5.3). Those
  differences are unobservable, but field equality would make the equivalences false.
- `Inv(p)`: the conjunction in `src/invariants.rs::assert_invariants`, strengthened as §5.2 proposes.

## 1. Specs (TSV)

Columns: `id`, `kind`, `functions`, `statement`, `subsumes (primary tests; secondary in §2)`,
`difficulty (Rust features the proof must handle)`.

```tsv
id	kind	functions	statement	subsumes	difficulty
scan-non-printable	equivalence	simd::find_non_printable_simd, x86_simd::find_non_printable_{sse2,avx2}, arm_simd::find_non_printable_neon, find_non_printable_scalar	∀ input: each arm returns input.iter().position(|b| !(0x20..=0x7E).contains(b)) (AVX2 arm: requires has_avx2()). Equivalently: Some(i) ⇒ i<len ∧ ¬P(input[i]) ∧ ∀j<i P(input[j]); None ⇒ ∀j P(input[j]).	simd::test_tiers_exhaustive_bytes_and_positions, simd::test_tiers_random_buffers (np part)	HARD for x86: unsafe std::arch intrinsics (loadu, cmpgt, movemask, trailing_zeros), target_feature, OnceLock has_avx2; each needs an intrinsic model. MEDIUM for NEON (portable safe Rust, 16-byte chunk loop plus scalar tail). EASY for scalar (Iterator::position).
scan-c0-control	equivalence	simd::find_c0_control{,_simd,_scalar}, *_sse2/_avx2/_neon	∀ input: each arm returns input.iter().position(|b| b < 0x20).	simd::test_find_c0_control_{empty,pure_data,at_start,embedded,boundary,varying_sizes}, tiers tests (c0 part)	same as scan-non-printable
scan-any-of	equivalence	simd::find_any_of<N>{,_simd,_scalar}, *_sse2/_avx2/_neon	∀ N, needles:[u8;N], input: each arm returns input.iter().position(|b| needles.contains(b)).	simd::test_find_any_of_{empty,pure_data,at_start,embedded,scalar_equivalence}, tiers tests (any part)	same, plus a const generic N and an OR-reduction loop over needles
take-printable-split	contract	simd::count_printable, simd::take_printable	n = count_printable(x) is the length of the maximal prefix of x in 0x20..=0x7E; take_printable(x) = (x[..n], x[n..]). Corollary: the first half is valid UTF-8, which discharges advance_batch's from_utf8_unchecked.	simd::test_count_printable, simd::test_take_printable	EASY given scan-non-printable (split_at_checked, unwrap_or)
utf8prefix-after-fold	equivalence	utf8::Utf8Prefix::{step,after}	∀ p with from_bytes(p.as_slice())=Some(p), ∀ run: p.after(run) = run.iter().fold(p, step). Key lemma: ∀ q, ∀ r with |r|>=3: q.after(r) = NONE.after(last3(r)) (a prefix is ≤3 bytes and a lead resets).	utf8_errors::a_prefix_after_a_run_is_the_prefix_after_each_byte	MEDIUM: Copy struct with [u8;3], slice get, fold. The state space is finite (well-formed prefixes × 256 bytes), so the step lemma is decidable by evaluation; `after` then needs induction on run length.
utf8prefix-from-bytes	contract	utf8::Utf8Prefix::{from_bytes,expected,as_slice}, utf8::{utf8_sequence_len,utf8_continues,utf8_prefix_well_formed}	from_bytes(b)=Some(q) ⇔ b=[] ∨ (seq_len(b[0])=Some(L) ∧ |b|<L ∧ every b[i] (i≥1) satisfies utf8_continues(b[0],i,b[i])), and then q.as_slice()=b, q.expected()=L. ∀ reachable q: from_bytes(q.as_slice())=Some(q) ∧ |q.as_slice()| < max(q.expected(),1). utf8_sequence_len/utf8_continues equal Unicode Table 3-7 (a table property decidable over 256×3×256).	(as above, second half); restore-carry-validation's Utf8Tail rows	EASY-MEDIUM: loops over a slice, saturating u8, const fns
decode-multibyte-char	contract	dispatch::Parser::decode_multibyte_char	requires is_utf8_lead(lead) (or lead ∈ 0xC0..=0xF7; see §5.5). Result Some((c,k)) ⇔ k = seq_len(lead)-1 ≤ |rem| ∧ [lead]++rem[..k] is a well-formed UTF-8 sequence encoding c; None otherwise.	(retires the duplicate test oracle utf8_simd::tests::scalar_one); utf8_simd::bulk_first_sequence_matches_scalar_for_every_lead_and_continuation_class (with next spec)	EASY: slice patterns, bit ops, char::from_u32 (needs a char model)
bulk-decode-run-refines-scalar	equivalence	utf8_simd::{bulk_decode_run,bulk_2byte,bulk_3byte,bulk_4byte}	∀ bytes, out with |out| ≤ RUN_CAP: bulk_decode_run(bytes,out) appends exactly the chars, and returns exactly the byte count, of iterating decode_multibyte_char from bytes[0] while (a) the lead has the same length class as bytes[0], (b) the decode is Some and (c) out is not full. Hence: returns 0 and leaves out unchanged when bytes is empty, does not start on a lead, or starts on an invalid or incomplete sequence; |out| ≤ RUN_CAP always; the result ≤ |bytes|.	utf8_simd::bulk_matches_scalar_homogeneous_cjk, bulk_matches_scalar_two_and_four_byte, bulk_respects_the_run_cap, bulk_first_sequence_matches_scalar_for_every_lead_and_continuation_class, bulk_matches_scalar_on_random_buffers	MEDIUM: while loops with slice-pattern get(i..i+k), ArrayVec<char,256> push/is_full, char::from_u32. Safe code with no intrinsics (written for Trust). Loop invariant: i = 3·|appended| (resp. 2, 4).
advance-fast-refines-utf8-step-fold	equivalence (refinement)	dispatch::Parser::{advance_fast,advance_simd_loop,advance_string_states_bulk,decode_multibyte_run}, csi::try_parse_csi_fast and helpers	∀ p∈Reach, x: Obs(advance_fast(p,x)) = Obs(foldl(step_u,p,x)) and View(advance_fast(p,x)) = View(foldl(step_u,p,x)). This is the master simulation. The bulk printable run, the CSI fast parser (65-byte window, fallbacks), the OSC/DCS/APC bulk copies and the multibyte run decoder each refine a run of step_u.	utf8_errors::fast_empty_input_produces_no_actions (the x=[] case); others via corollaries 11 and 12	VERY HARD: one loop with three FnMut closure params and bool mode flags, slice split_first/get, SIMD scanner calls (use the scan-* contracts), try_parse_csi_fast (three nested scan loops with early return), ArrayVec params, Vec osc_data extend, generic sink, &mut self. Needs the osc-bulk-copy-len and bulk-decode-run lemmas.
advance-fast-chunk-invariance	equivalence	Parser::advance_fast	∀ p∈Reach, x, y: Obs(advance_fast(p,x++y)) = Obs(advance_fast(p,x)) ++ Obs(advance_fast(p',y)) where p' = advance_fast(p,x), and the final Views are equal. (A corollary of the previous spec plus List.foldl_append.)	utf8_parity::incomplete_trailing_lead_after_complete_char_matches_bytewise	FREE once advance-fast-refines-utf8-step-fold is proved; otherwise as hard as it
ground-utf8-lossy	equivalence (to a reference decoder)	step_u at Ground: utf8::{process_utf8_byte,start_utf8,decode_utf8_validated}, dispatch::{process_ground_special_byte,decode_multibyte_run}	∀ p∈Reach with state=Ground, utf8_len=0, C1 off; ∀ b ∈ ({0x20..0x7E}∪{0x80..0xFF})*, z ∈ 0x20..0x7E: Obs(advance_fast(p,b++[z])) = [print(c) | c ← String::from_utf8_lossy(b++[z]).chars()] (one U+FFFD per maximal subpart, Unicode §3.9), and it ends at Ground with utf8_len=0. Without z, the prints cover the lossy decoding of b minus its trailing well-formed incomplete prefix, which stays buffered (utf8_buffer[..utf8_len]).	bulk_path_matches_continuation_path_for_sampled_codepoints, fuzz_random_valid_utf8_has_exact_parity, ill_formed_utf8_is_replaced_per_maximal_subpart; utf8_errors::fast_invalid_continuation_emits_replacement_then_reprints_ascii	MEDIUM-HARD: needs a Clean model of the Table 3-7 maximal-subpart decoder as the reference (std's from_utf8_lossy is not modeled); state over [u8;4]+u8+u8; prove on foldl step_u, then lift with the master spec
osc-bulk-copy-len	contract (arithmetic)	dispatch::{osc_bulk_copy_len,osc_put_keeps}	∀ len ≤ MAX_OSC_DATA, n: (a) len + osc_bulk_copy_len(len,n) = min(len+n, MAX_OSC_DATA); (b) ∀ i<n: osc_put_keeps(min(len+i,MAX)) ⇔ i < osc_bulk_copy_len(len,n), and min(len+i+1,MAX) = min(len+i,MAX) + [osc_put_keeps(min(len+i,MAX))]. So the bulk copy keeps exactly what n OscPut steps keep.	batch::osc_bulk_copy_agrees_with_per_byte_puts_at_every_breakpoint	TRIVIAL: usize min/saturating_sub, linear arithmetic (omega)
param-accumulator	contract	dispatch::{add_param_digit,finalize_param,clear}, csi::{push_current_param,close_params}	add_param_digit(d∈'0'..='9'): current' = current.saturating_mul(10).saturating_add(d−'0') ∧ started' ∧ params unchanged. add_param_digit(';'|':'): if |params|<MAX_PARAMS then params' = params++[min(current,65535) as u16] and mask' = mask | (old last_was_colon ? 1<<|params| : 0), else params and mask unchanged; current'=0, started'=false, last_was_colon' = (byte==':'). close_params(c) pushes iff started ∨ (params≠[] ∧ ¬c). All four preserve |params| ≤ MAX_PARAMS and mask>>|params| = 0.	csi::add_param_digit_saturates_and_finalize_param_clamps, parameter_digits_saturate_and_finalize_clamps, parse_csi_many_params	EASY: &mut self, saturating u32, ArrayVec<u16,24> push with a capacity check, u32 shifts (index < 24)
csi-dispatch-denotation	contract (sequence semantics)	Parser::advance_fast on CSI input; csi::try_parse_csi_fast; table CsiEntry/CsiParam/CsiIntermediate rows	∀ p∈Reach at Ground with utf8_len=0, either C1 mode, whole or one byte per read: for s = ESC '[' M P I F with M ∈ {ε}∪{0x3C..0x3F}, P ∈ [0-9;:]*, I ∈ [0x20..0x2F]*, F ∈ 0x40..0x7E, the trace is exactly one call and the run ends at Ground. Split P on ';'/':' into segments s0..sk with separators sep1..sepk. Segment i is a parameter iff i<k ∨ sk≠ε ∨ (k>0 ∧ sepk=';'). params = take(MAX_PARAMS, [min(dec(si),65535)]) with dec(ε)=0 (decimal saturating at u32::MAX). mask bit i ⇔ i≥1 ∧ sepi=':'. ims = take(MAX_INTERMEDIATES, M++I). The call is csi_dispatch(params,ims,F) if mask=0, else csi_dispatch_with_subparams(params,ims,F,mask).	parse_csi_kakoune_sgr, csi_fast_path_multiple_sequences, csi_fast_path_interleaved_with_text; subparams::parse_csi_colon_subparams, parse_csi_mixed_colon_semicolon, parse_csi_no_colon_no_subparams, parse_csi_subparam_flag_survives_past_index_16, parse_csi_subparam_flag_at_last_param_index; empty_params::an_empty_parameter_reaches_the_handler_as_the_default	MEDIUM on the per-byte state machine (induction over P with the param-accumulator lemmas; table lookups as evaluated facts). The CSI fast parser comes free via advance-fast-refines-utf8-step-fold.
dcs-header-denotation	contract	advance_fast on DCS headers; table DcsEntry/DcsParam/DcsIntermediate rows; close_params	For ESC 'P' M P I F with P ∈ [0-9;]* (a ':' sends the string to DcsIgnore), F ∈ 0x40..0x7E: exactly one dcs_hook(params,ims,F), with params computed by the CSI rule (empty trailing parameter after ';' → 0), and the run ends in DcsPassthrough with dcs_active.	empty_params::a_dcs_header_keeps_its_empty_last_parameter	MEDIUM (same shape as CSI)
vt-ground-and-esc-basics	contract	process_byte_inner, process_utf8_byte + TRANSITIONS Ground/Escape/EscapeIntermediate rows	With an incomplete UTF-8 prefix at Ground, advance_fast emits one U+FFFD then replays an interrupting ESC normally. At Ground: b∈0x20..=0x7E ⇒ [print(b as char)]; b a C0 other than ESC ⇒ [execute(b)] (CAN/SUB included). ESC I* F (I∈0x20..0x2F, F∈0x30..0x7E minus the introducers [ ] P X ^ _) ⇒ [esc_dispatch(take(4,I),F)], ending at Ground.	basic::parse_plain_text, parse_control_character, parse_esc_sequence, parse_esc_with_intermediate; utf8_errors::fast_invalid_utf8_prefix_replays_escape_across_chunks	EASY: evaluated table facts plus a match on ActionType
introducers-terminate	contract	advance_fast	∀ C1 mode: ESC '[' d F (d digit, F∈0x40..0x7E), ESC ']' D* BEL (D∈0x20..0x7E) and ESC 'P' 'q' ESC '\\' each end at Ground from Parser::new(). (A corollary of the three denotations.)	basic::every_introducer_terminates	FREE as a corollary; EASY directly
osc-denotation	contract	process_byte_impl OscString arm, dispatch_osc, parse_and_dispatch_osc, parse_osc_from_third_param	From Ground, s = INTRO D T, where INTRO ∈ {ESC ']'} ∪ (C1 on ? {0x9D} : ∅), D is any bytes the OscString row maps to OscPut (with C1 on, minus 0x9C), and T ∈ {BEL, ESC '\\'} ∪ (C1 on ? {0x9C} : ∅): exactly one osc_dispatch(split16(take(MAX_OSC_DATA, D))), followed by esc_dispatch([],'\\') when T = ESC '\\', ending at Ground. split16 splits on ';' into at most 16 slots, and the last slot carries the remainder verbatim. With C1 off, 0x9C (and all of 0x80..0x9F) is data.	basic::parse_osc_with_bel, parse_osc_with_esc_backslash; c1::parse_c1_osc, osc_payload_preserves_non_st_c1_bytes, osc_string_c1_st_byte_is_data_not_terminator, parse_c1_osc_with_c1_st, parse_c1_osc_with_c1_st_advance_fast; string_cancel::a_terminated_osc_still_dispatches	MEDIUM: Vec<u8> push with a cap, ArrayVec<&[u8],16> of borrowed subslices (lifetimes), position scans; the existing island proof seeded_from_third_matches_full_split covers one lemma
dcs-passthrough-denotation	contract	process_byte_impl DcsPassthrough arm (DcsHook/DcsPut), advance_string_states_bulk DCS branch	ESC 'P' hdr F payload T with T ∈ {ESC '\\', a 0x9C that is ST (see string-st-0x9c-utf8-aware)} and payload free of CAN/SUB/ESC/ST: exactly [dcs_hook(..)] ++ [dcs_put(b) | b ← payload, b ≠ 0x7F] ++ [dcs_unhook(false)], then, for T = ESC '\\' only, esc_dispatch([],'\\') (Escape row 0x5C is EscDispatch), ending at Ground. Every high byte except an ST 0x9C is delivered.	parse_dcs_passthrough; c1::dcs_passthrough_preserves_non_st_high_bytes, dcs_passthrough_st_c1_byte_terminates_sequence, dcs_passthrough_st_c1_byte_terminates_sequence_advance_fast	MEDIUM
apc-sos-pm-denotation	contract	process_byte_impl SosPmApcString arm, APC bulk branch	ESC '_' payload T ⇒ [apc_start] ++ [apc_put(b) | b ← payload] (DEL included) ++ [apc_end(false)] (++ esc_dispatch([],'\\') when T = ESC '\\'). ESC 'X' / ESC '^' (SOS/PM) payload T ⇒ no string-consumer calls. Payload and T as for DCS. Ends at Ground.	c1::apc_payload_preserves_non_st_c1_bytes, apc_st_c1_byte_terminates_sequence	MEDIUM
can-sub-cancel	contract (per-step)	process_byte_impl	∀ p∈Reach, b∈{0x18,0x1A}: the step ends at Ground and its trace is [string-end(canceled=true)] ++ [execute(b)] (a pending UTF-8 prefix first emits one U+FFFD), where string-end = dcs_unhook(true) if dcs_active or dcs_st_pending, apc_end(true) if apc_active, nothing for OSC (osc_discard set, no osc_dispatch), nothing otherwise. No CSI/ESC/OSC dispatch for the interrupted sequence.	basic::cancel_aborts_sequence; utf8_errors::sub_aborts_csi_sequence; string_cancel::can_and_sub_cancel_an_osc_string, can_and_sub_cancel_an_apc_string, a_carried_osc_is_canceled_by_can_in_the_tail	EASY per step once the table facts for 0x18/0x1A are evaluated
esc-interrupts	contract (per-step)	process_byte_impl	∀ p∈Reach in a non-string state, ESC goes to Escape with params/intermediates cleared and no dispatch. In OscString, ESC ends the OSC with osc_dispatch (unless osc_discard). In SosPmApcString it sends apc_end(false). In DcsPassthrough it sends nothing and sets dcs_st_pending; the next byte b (whatever the state machine then does with it in Escape) is preceded by dcs_unhook(b ≠ '\\'): ST completes the string, anything else cancels it.	basic::esc_interrupts_sequence; string_cancel::an_escape_that_does_not_begin_st_cancels_a_dcs, st_still_ends_a_dcs_complete; carry::a_dcs_at_the_escape_that_may_begin_its_st_is_still_hooked	EASY
c1-guard	contract (per-step)	process_byte_impl guard, process_ground_special_byte	C1 off: ∀ state ∉ {Ground, DcsPassthrough, DcsIgnore, OscString, SosPmApcString}, b∈0x80..=0x9F: the step is the identity (empty trace, p unchanged). At Ground it is [print(U+FFFD)] with the state unchanged. C1 on: 0x9B/0x9D/0x90 enter CsiEntry/OscString/DcsEntry from every non-string state (with Clear).	c1::parse_c1_csi, c1_disabled_by_default, c1_disabled_blocks_injection_in_csi_param_state, c1_disabled_blocks_injection_in_escape_state, c1_disabled_blocks_dcs_osc_injection_mid_sequence	EASY
string-st-0x9c-utf8-aware	contract + invariant	process_byte_impl read_as logic, Utf8Prefix, string bulk branches	Invariant: in DcsPassthrough/DcsIgnore/SosPmApcString, string_utf8 = from_bytes(carried head).after(payload bytes read in this string), and NONE in every other state. Contract: with C1 off, 0x9C read in those states ends the string iff ¬string_utf8.continues(0x9C), and is read as 0x80 otherwise. With C1 on, 0x9C always ends it.	c1::a_0x9c_inside_a_utf8_character_does_not_end_the_string	MEDIUM: relies on utf8prefix-after-fold for the bulk paths
dcs-ignore-bisimilar-passthrough	table-property + equivalence	TRANSITIONS[DcsIgnore], TRANSITIONS[DcsPassthrough]	(table) ∀ b: next(DcsIgnore,b)=DcsIgnore ⇔ next(DcsPassthrough,b)=DcsPassthrough, and when both leave they go to the same state. (parsers) The relation R = {(p in DcsPassthrough, q in DcsIgnore) equal except dcs_active/state} is a bisimulation for step_u on every observation except dcs_put/dcs_unhook, in both C1 modes.	c1::an_ignored_dcs_reads_every_byte_as_a_hooked_one	EASY (table: evaluate 256 entries); MEDIUM (bisimulation: per-step case split)
dcs-header-gr-ignored	table-property	TRANSITIONS rows DcsEntry, DcsParam, DcsIntermediate	∀ s∈{DcsEntry,DcsParam,DcsIntermediate}: ∀ b∈0xA0..=0xFF, TRANSITIONS[s][b] = (s, Ignore); ∀ b∈0x80..=0x9F, TRANSITIONS[s][b] = TRANSITIONS[Escape][b]. Corollary with dcs-header-denotation: GR bytes in a header are dropped and the string still hooks.	c1::a_gr_byte_in_a_dcs_header_keeps_the_string	TRIVIAL (const evaluation of generate_table)
type-invariant	invariant	Parser::new, reset, process_byte_inner, process_utf8_byte, advance_fast, restore_carry	Inv(Parser::new()), Inv(with_c1_controls()), Inv(p) ⇒ Inv(reset(p)); Inv(p) ⇒ Inv(step_u(p,b)) ∀b; hence Inv is preserved by advance_fast for all x; restore_carry(c)=Ok ⇒ Inv. (Inv as strengthened in §5.2.)	every_state_every_byte_keeps_the_type_invariant	HARD but central: process_byte_impl! macro (a match over 16 ActionTypes), facts about TRANSITIONS (e.g. DcsHook only enters DcsPassthrough), ArrayVec capacity, u8 arithmetic, generic sink (the trait call must be framed as unable to touch self). The fast path needs the master simulation, or the invariant re-proved over advance_simd_loop.
reset-is-fresh	contract	Parser::reset	View(reset(p)) = View(new()) with p's c1_controls_enabled kept, and every scratch field equal to new()'s (osc_data capacity aside).	basic::reset_clears_state	TRIVIAL: field assignments
no-panic-total	no-panic/bounds	advance_fast, reset, carry, carry_unhooking, restore_carry (and every callee)	∀ p∈Reach, x, c: these calls neither panic nor index out of bounds, and they terminate. (Trust's strict lane may already discharge this as panic-freedom; the code is written with get/split_first/saturating_* for that purpose.)	fuzz_parser::fuzz_advance_never_panics, fuzz_escape_shapes_never_panic; utf8_parity::fuzz_arbitrary_bytes_never_panic	MEDIUM: bounds on every slice/array access, ArrayVec push overflow, u8/u32 arithmetic, the unsafe SIMD calls (needs the AVX2 precondition)
linear-progress	other (complexity: loop variant)	advance_simd_loop, advance_string_states_bulk, try_parse_csi_fast, decode_multibyte_run	Each iteration of the advance_simd_loop while-loop strictly decreases remaining.len(), so iterations ≤ |input|. Every iteration does O(1 + bytes consumed) work (bulk scans and copies are linear in what they consume; CSI scan ≤ 65 bytes; a multibyte run ≤ 256 chars).	performance::parser_handles_pathological_input, parser_handles_large_osc	EASY-MEDIUM: a variant on a slice length through split_first/get(n..); the cost bound needs a cost model
carry-restore-continues	equivalence	Parser::carry, restore_carry, advance_fast	∀ p∈Reach with carry(p)=Some(c) ∧ ¬carry_swallows_sequence(p), ∀ fresh q with p's C1 mode: restore_carry(q,c)=Ok, and ∀ tail, Obs(feed(q',tail)) = Obs(feed(p,tail)) and View(feed(q',tail)) = View(feed(p,tail)). Equivalently: feed(head); carry; restore; feed(tail) ≡ feed(head++tail).	carry::a_carry_continues_every_split_exactly, a_dcs_header_carry_reads_every_tail_as_the_live_parser, every_split_of_a_dcs_stream_continues_exactly, can_sub_and_esc_still_reset_a_restored_state	HARD: a simulation relation between p and restore(carry(p)) (they differ only in the dead scratch fields, the osc_data capacity and osc_discard for an OSC over the cap); then reuse step lemmas. Vec↔ArrayVec conversions.
carry-projection	contract	Parser::carry, carry_unhooking, restore_carry	carry(p)=None ⇔ in_hooked_dcs(p) (p.state=DcsPassthrough, or p.state=Escape at the ESC that may begin a hooked string's ST: dcs_st_pending). At Ground, carry(p) = {utf8_tail = utf8_buffer[..utf8_len], utf8_expected}, else default. carry_unhooking(p) = {state: Escape} (nothing held) at that pending ESC; else carry(p) if Some, else {state: SosPmApcString, string_utf8_tail}. restore_carry(q,c)=Ok ⇒ carry(q')=Some(c) (read-back). ∀ p∈Reach: restore accepts carry(p) and carry_unhooking(p).	carry::a_ground_parser_carries_nothing_but_a_split_glyph, a_hooked_dcs_is_not_carried	MEDIUM: Vec clone/to_vec, a match over 14 states
carry-lossy-swallow	contract	carry, restore_carry, osc_discard, SosPmApcString/DcsIgnore without consumer	If carry(p) is OscString with osc_overflowed, or DcsIgnore, or SosPmApcString (no consumer is restored), then after restore and any tail: the successor emits no osc_dispatch for that OSC (a CAN/SUB also discards it), no dcs_*/apc_* calls for that string, and from the string's terminator on its trace equals the live parser's trace from its terminator on.	carry::string_payloads_are_swallowed_to_their_terminator, an_over_cap_osc_is_discarded_at_its_terminator, a_string_carry_continues_a_split_utf8_character	MEDIUM: a per-step invariant "inside the string ⇒ only swallow"
restore-carry-validation	contract	Parser::restore_carry, validate_carry	restore_carry(c) = Err(r) ⇒ the parser is unchanged, with r the first failing clause of validate_carry (a total, decidable predicate over the fields). Ok ⇒ Safe(q'): Inv ∧ state≠DcsPassthrough ∧ ¬dcs_active ∧ ¬apc_active ∧ mask>>|params| = 0 ∧ (utf8_len=0 ∨ (Ground ∧ utf8_len<utf8_expected)) ∧ (string_utf8=NONE ∨ state∈{DcsIgnore,SosPmApcString}). Acceptance is independent of the C1 mode.	carry::restore_carry_refuses_every_invalid_carry, restore_carry_accepts_the_edge_of_every_bound, restore_carry_installs_only_safe_carries_whoever_wrote_them	MEDIUM: Result, Vec iteration, u8::try_from, from_bytes; "unchanged on Err" is structural (validate before reset)
hooked-string-carry-bisimilar	table-property + equivalence	TRANSITIONS[SosPmApcString] vs [DcsPassthrough], carry_unhooking	(table) ∀ b: next(SosPmApcString,b)=SosPmApcString ⇔ next(DcsPassthrough,b)=DcsPassthrough, and where they leave, (next, action) are equal. (parsers) A hooked live parser and restore(carry_unhooking) of it agree on every observation except dcs_*/apc_* for every tail, in both C1 modes; while the live parser is in DcsPassthrough the successor is in SosPmApcString, and after leaving their Views are equal.	carry::the_hooked_string_carry_reads_every_byte_as_the_hooked_string, a_hooked_dcs_carried_unhooking_is_swallowed_to_its_terminator	EASY (table) / MEDIUM (bisimulation)
carry-for-older-builds	contract	ParserCarry::for_older_builds	for_older_builds(c) = Some({state: DcsIgnore, ..default}) if c.state ∈ {DcsEntry, DcsParam, DcsIntermediate}, else None.	carry::a_header_carry_has_the_older_builds_form_beside_it	TRIVIAL
carry-swallows-predicate	contract	carry_swallows_sequence, carry_swallows_picture	(definitional) swallows ⇔ (OscString ∧ ¬osc_discard ∧ |osc_data| > OSC_CARRY_CAP) ∨ (SosPmApcString ∧ apc_active); picture ⇔ swallows ∧ (SosPmApcString ∨ osc_data starts with "1337;"). (semantic, recommended) ∀ p∈Reach with carry(p)=Some: carry-restore-continues holds for p ⇔ ¬swallows(p).	carry::carry_swallows_sequence_names_the_lossy_carries_only	TRIVIAL (definitional) / HARD (semantic: the converse needs a witness tail per case)
state-name-roundtrip	table-property	State::{ALL,name,from_name}, derive(Debug)	∀ i<COUNT: ALL[i] as usize = i; ∀ s: from_name(name(s)) = Some(s); from_name(x)=None when x is not a name; name(s) = format!("{s:?}").	carry::state_names_round_trip	TRIVIAL for the first three (14 cases); the Debug clause needs the derive modeled, so have Debug delegate to name() or drop the clause
```

### Distribution by kind (39)

| kind | count | ids |
|---|---|---|
| contract | 22 | take-printable-split, utf8prefix-from-bytes, decode-multibyte-char, osc-bulk-copy-len, param-accumulator, csi-dispatch-denotation, dcs-header-denotation, vt-ground-and-esc-basics, introducers-terminate, osc-denotation, dcs-passthrough-denotation, apc-sos-pm-denotation, can-sub-cancel, esc-interrupts, c1-guard, string-st-0x9c-utf8-aware, reset-is-fresh, carry-projection, carry-lossy-swallow, restore-carry-validation, carry-for-older-builds, carry-swallows-predicate |
| equivalence | 9 | scan-non-printable, scan-c0-control, scan-any-of, utf8prefix-after-fold, bulk-decode-run-refines-scalar, advance-fast-refines-utf8-step-fold, advance-fast-chunk-invariance, ground-utf8-lossy, carry-restore-continues |
| table-property | 4 | dcs-ignore-bisimilar-passthrough, dcs-header-gr-ignored, hooked-string-carry-bisimilar, state-name-roundtrip |
| invariant | 1 | type-invariant |
| no-panic/bounds | 1 | no-panic-total |
| other | 2 | linear-progress (loop variant), refinement-to-parser-model |

### Proof dependency order

`osc-bulk-copy-len`, `param-accumulator`, the `scan-*` scalar/NEON contracts, `decode-multibyte-char`,
the table properties, `utf8prefix-*` → `bulk-decode-run-refines-scalar` → per-step contracts (`c1-guard`,
`can-sub-cancel`, `esc-interrupts`, denotations on the per-byte state machine) → `type-invariant` →
master simulation `advance-fast-refines-utf8-step-fold` → free corollaries (chunk invariance, the
fast-path halves of the denotations) → carry specs.

## 2. Test → spec map (TSV; all 172)

`primary` is the spec that retires the test. `also` lists the other specs the same test samples.

```tsv
file	test	primary	also
src/simd.rs	test_count_printable	take-printable-split	scan-non-printable
src/simd.rs	test_take_printable	take-printable-split	
src/simd.rs	test_find_c0_control_boundary	scan-c0-control	
src/simd.rs	test_find_any_of_pure_data	scan-any-of	
src/simd.rs	test_find_any_of_at_start	scan-any-of	
src/simd.rs	test_tiers_exhaustive_bytes_and_positions	scan-non-printable	scan-c0-control, scan-any-of
src/simd.rs	test_tiers_random_buffers	scan-non-printable	scan-c0-control, scan-any-of
src/utf8_simd.rs	bulk_matches_scalar_homogeneous_cjk	bulk-decode-run-refines-scalar	
src/utf8_simd.rs	bulk_matches_scalar_two_and_four_byte	bulk-decode-run-refines-scalar	
src/utf8_simd.rs	bulk_respects_the_run_cap	bulk-decode-run-refines-scalar	
src/utf8_simd.rs	bulk_first_sequence_matches_scalar_for_every_lead_and_continuation_class	bulk-decode-run-refines-scalar	decode-multibyte-char
src/utf8_simd.rs	bulk_matches_scalar_on_random_buffers	bulk-decode-run-refines-scalar	
src/tests/basic.rs	parse_plain_text	vt-ground-and-esc-basics	
src/tests/basic.rs	parse_control_character	vt-ground-and-esc-basics	
src/tests/basic.rs	parse_esc_sequence	vt-ground-and-esc-basics	
src/tests/basic.rs	parse_esc_with_intermediate	vt-ground-and-esc-basics	
src/tests/basic.rs	parse_osc_with_bel	osc-denotation	
src/tests/basic.rs	parse_osc_with_esc_backslash	osc-denotation	
src/tests/basic.rs	parse_dcs_passthrough	dcs-passthrough-denotation	
src/tests/basic.rs	cancel_aborts_sequence	can-sub-cancel	
src/tests/basic.rs	esc_interrupts_sequence	esc-interrupts	
src/tests/basic.rs	reset_clears_state	reset-is-fresh	
src/tests/basic.rs	every_introducer_terminates	introducers-terminate	
src/tests/csi.rs	parse_csi_many_params	param-accumulator	csi-dispatch-denotation
src/tests/csi.rs	parse_csi_kakoune_sgr	csi-dispatch-denotation	
src/tests/csi.rs	csi_fast_path_multiple_sequences	csi-dispatch-denotation	
src/tests/csi.rs	csi_fast_path_interleaved_with_text	csi-dispatch-denotation	
src/tests/csi.rs	csi_fast_path_falls_back_when_sequence_exceeds_64_bytes	advance-fast-chunk-invariance	
src/tests/csi.rs	csi_fast_path_falls_back_on_unknown_byte_in_param_scan	advance-fast-chunk-invariance	
src/tests/csi.rs	csi_fast_path_falls_back_on_unexpected_byte_after_intermediate	advance-fast-chunk-invariance	
src/tests/csi.rs	csi_fast_path_trailing_empty_colon_subparam_matches_per_byte	advance-fast-chunk-invariance	csi-dispatch-denotation
src/tests/csi.rs	parameter_digits_saturate_and_finalize_clamps	param-accumulator	
src/tests/csi.rs	add_param_digit_saturates_and_finalize_param_clamps	param-accumulator	
src/tests/subparams.rs	parse_csi_colon_subparams	csi-dispatch-denotation	
src/tests/subparams.rs	parse_csi_mixed_colon_semicolon	csi-dispatch-denotation	
src/tests/subparams.rs	parse_csi_no_colon_no_subparams	csi-dispatch-denotation	
src/tests/subparams.rs	parse_csi_subparam_flag_survives_past_index_16	csi-dispatch-denotation	
src/tests/subparams.rs	parse_csi_subparam_flag_at_last_param_index	csi-dispatch-denotation	
src/tests/empty_params.rs	an_empty_parameter_reaches_the_handler_as_the_default	csi-dispatch-denotation	advance-fast-chunk-invariance, carry-restore-continues
src/tests/empty_params.rs	a_dcs_header_keeps_its_empty_last_parameter	dcs-header-denotation	advance-fast-chunk-invariance
src/tests/c1.rs	parse_c1_csi	c1-guard	csi-dispatch-denotation
src/tests/c1.rs	parse_c1_osc	osc-denotation	
src/tests/c1.rs	osc_payload_preserves_non_st_c1_bytes	osc-denotation	
src/tests/c1.rs	osc_string_c1_st_byte_is_data_not_terminator	osc-denotation	
src/tests/c1.rs	c1_disabled_by_default	c1-guard	
src/tests/c1.rs	dcs_passthrough_preserves_non_st_high_bytes	dcs-passthrough-denotation	
src/tests/c1.rs	dcs_passthrough_st_c1_byte_terminates_sequence	dcs-passthrough-denotation	string-st-0x9c-utf8-aware
src/tests/c1.rs	dcs_passthrough_st_c1_byte_terminates_sequence_advance_fast	dcs-passthrough-denotation	
src/tests/c1.rs	apc_payload_preserves_non_st_c1_bytes	apc-sos-pm-denotation	
src/tests/c1.rs	apc_st_c1_byte_terminates_sequence	apc-sos-pm-denotation	string-st-0x9c-utf8-aware
src/tests/c1.rs	parse_c1_osc_with_c1_st	osc-denotation	
src/tests/c1.rs	c1_disabled_blocks_injection_in_csi_param_state	c1-guard	
src/tests/c1.rs	c1_disabled_blocks_injection_in_escape_state	c1-guard	
src/tests/c1.rs	c1_disabled_blocks_dcs_osc_injection_mid_sequence	c1-guard	
src/tests/c1.rs	an_ignored_dcs_reads_every_byte_as_a_hooked_one	dcs-ignore-bisimilar-passthrough	
src/tests/c1.rs	a_0x9c_inside_a_utf8_character_does_not_end_the_string	string-st-0x9c-utf8-aware	advance-fast-chunk-invariance
src/tests/c1.rs	a_gr_byte_in_a_dcs_header_keeps_the_string	dcs-header-gr-ignored	dcs-header-denotation, advance-fast-chunk-invariance
src/tests/string_cancel.rs	can_and_sub_cancel_an_osc_string	can-sub-cancel	advance-fast-chunk-invariance
src/tests/string_cancel.rs	a_terminated_osc_still_dispatches	osc-denotation	advance-fast-chunk-invariance
src/tests/string_cancel.rs	can_and_sub_cancel_an_apc_string	can-sub-cancel	apc-sos-pm-denotation
src/tests/string_cancel.rs	a_carried_osc_is_canceled_by_can_in_the_tail	can-sub-cancel	carry-lossy-swallow
src/tests/utf8_errors.rs	fast_invalid_utf8_prefix_replays_escape_across_chunks	vt-ground-and-esc-basics	advance-fast-chunk-invariance (migrated from the shared batch UTF-8/control case)
src/tests/string_bulk.rs	osc_bulk_copy_agrees_with_per_byte_puts_at_every_breakpoint	osc-bulk-copy-len	
src/tests/string_bulk.rs	dcs_passthrough_del_parity_whole_vs_per_byte	advance-fast-chunk-invariance	dcs-passthrough-denotation
src/tests/carry.rs	a_carry_continues_every_split_exactly	carry-restore-continues	carry-projection
src/tests/carry.rs	a_ground_parser_carries_nothing_but_a_split_glyph	carry-projection	
src/tests/carry.rs	a_hooked_dcs_is_not_carried	carry-projection	
src/tests/carry.rs	a_hooked_dcs_carried_unhooking_is_swallowed_to_its_terminator	hooked-string-carry-bisimilar	carry-lossy-swallow
src/tests/carry.rs	the_hooked_string_carry_reads_every_byte_as_the_hooked_string	hooked-string-carry-bisimilar	
src/tests/carry.rs	string_payloads_are_swallowed_to_their_terminator	carry-lossy-swallow	
src/tests/carry.rs	an_over_cap_osc_is_discarded_at_its_terminator	carry-lossy-swallow	
src/tests/carry.rs	can_sub_and_esc_still_reset_a_restored_state	carry-restore-continues	can-sub-cancel, esc-interrupts
src/tests/carry.rs	restore_carry_refuses_every_invalid_carry	restore-carry-validation	utf8prefix-from-bytes
src/tests/carry.rs	restore_carry_accepts_the_edge_of_every_bound	restore-carry-validation	carry-projection
src/tests/carry.rs	restore_carry_installs_only_safe_carries_whoever_wrote_them	restore-carry-validation	type-invariant
src/tests/carry.rs	state_names_round_trip	state-name-roundtrip	
src/tests/carry.rs	carry_swallows_sequence_names_the_lossy_carries_only	carry-swallows-predicate	
src/tests/carry.rs	a_string_carry_continues_a_split_utf8_character	carry-lossy-swallow	string-st-0x9c-utf8-aware
src/tests/carry.rs	a_dcs_header_carry_reads_every_tail_as_the_live_parser	carry-restore-continues	
src/tests/carry.rs	every_split_of_a_dcs_stream_continues_exactly	carry-restore-continues	
src/tests/carry.rs	a_header_carry_has_the_older_builds_form_beside_it	carry-for-older-builds	carry-lossy-swallow
src/tests/invariants.rs	every_state_every_byte_keeps_the_type_invariant	type-invariant	can-sub-cancel, c1-guard
src/tests/utf8_errors.rs	a_prefix_after_a_run_is_the_prefix_after_each_byte	utf8prefix-after-fold	utf8prefix-from-bytes
src/tests/utf8_parity.rs	bulk_path_matches_continuation_path_for_sampled_codepoints	ground-utf8-lossy	advance-fast-chunk-invariance
src/tests/utf8_parity.rs	fuzz_random_valid_utf8_has_exact_parity	ground-utf8-lossy	advance-fast-chunk-invariance
src/tests/utf8_parity.rs	incomplete_trailing_lead_after_complete_char_matches_bytewise	advance-fast-chunk-invariance	ground-utf8-lossy
src/tests/utf8_parity.rs	ill_formed_utf8_is_replaced_per_maximal_subpart	ground-utf8-lossy	advance-fast-chunk-invariance
src/tests/performance.rs	parser_handles_pathological_input	linear-progress	
src/tests/performance.rs	parser_handles_large_osc	linear-progress	
tests/fuzz_parser.rs	fuzz_advance_never_panics	no-panic-total	
tests/fuzz_parser.rs	fuzz_escape_shapes_never_panic	no-panic-total	
```

(`tests/*.rs` in the map means `src/tests/*.rs`, except `tests/fuzz_parser.rs`, which is the
integration test file.)

## 3. Coverage

- 172 original tests read. 159 map to one of the 42 specs (the shared batch UTF-8/control case now runs on advance_fast). 13 are not spec samples (§4).
- The biggest collapses: `csi-dispatch-denotation` (17 primary), `ground-utf8-lossy` (13),
  `type-invariant` (12), `bulk-decode-run-refines-scalar` (11), `scan-c0-control` (6 primary, plus 2
  shared).
- `advance-fast-refines-utf8-step-fold` has only 1 primary test, but proving it makes the primary
  tests of `advance-fast-chunk-invariance` and the fast-path halves of about 25 others free.

## 4. Tests that are not spec samples

| test | why | recommendation |
|---|---|---|
| table_validity::transition_table_all_entries_valid | `Transition.action: ActionType` and `next_state: State` are enums, so "action ≤ 15, state < 14" holds by typing. The test can never fail. | **delete** |
| table_validity::transition_table_matches_generated | `pub static TRANSITIONS = generate_table();` (table/mod.rs:386). This compares one const evaluation of a pure function with another. | **delete** |
| performance::parser_linear_scaling | A floating-point ratio over two input sizes. It is weaker than the loop variant and duplicates the other two tests. | **delete** (`linear-progress` replaces it) |
| performance::parser_handles_pathological_input, parser_handles_large_osc | They sample a real property: iterations ≤ input length. | **become the spec** `linear-progress`. Then delete the `#[cfg(test)]` `PARSER_LOOP_ITERATIONS` instrumentation in lib.rs and dispatch.rs. Keep throughput in `aterm-bench` (already the case). |
| batch::advance_batch_* (10 tests) | `advance_batch`, `process_byte_batch` and `BatchActionSink` are `#[cfg(test)]` and have **no production caller**. These ten cases duplicate production coverage through that test-only entry. The eleventh, `advance_batch_invalid_utf8_then_escape_replays_escape`, uniquely checked shared `process_utf8_byte` control resynchronization and is retained as `utf8_errors::fast_invalid_utf8_prefix_replays_escape_across_chunks` on `advance_fast`. | **delete the ten duplicate cases with the batch API; retain the shared UTF-8/control regression on the production path**, including chunk splits. |

No test is a pure regression of a removed bug with no general property. Every regression test I found
(#7556 C1 injection, THRU-4a straddle, the DEL-in-DCS leak, R15-PARSER-P1/P2, the trailing-colon
fast/slow split) is an instance of one of the general specs above.

## 5. Implied specs that look wrong, or disagree with the code

1. **Mislabelled or stale tests**
   - `c1::c1_disabled_blocks_injection_in_escape_state` says "EscapeIntermediate" in its comment but
     drives `Escape`.
2. **The TypeInvariant's TLA text contradicts the code.** The comment in `src/invariants.rs` says
   `currentParam ∈ 0..65535`. In the code `current_param: u32` saturates at `u32::MAX`, and the tests
   reach that state (`reachable_starts` has `ESC [ 99999999999`). The clamp happens only when a parameter
   is finalized. `assert_invariants` checks nothing for this field. The correct conjuncts are the ones
   `validate_carry` already enforces, so `Inv` should be strengthened with:
   - `¬param_started ⇒ current_param = 0`. This holds on every path I read: digits set it, and
     push/clear/`parse_csi_after_first_param`/`parse_csi_general` reset it.
   - `subparam_mask >> params.len() = 0`. This replaces the weaker `SubparamMaskValid`, which ignores
     bit 0 and is skipped when `len ≥ 32`. `assert_restored_state_is_safe` already uses the strong form.
3. **The CSI fast parser and the per-byte state machine leave different scratch state.** After
   `ESC [ 3 1 CAN` followed by `ESC [ 5 m`:
   - the fast path's `set_single` branch never resets `current_param`/`param_started`, so the parser ends
     at Ground with `current_param = 31, param_started = true`;
   - the per-byte state machine (the same input one byte per read) ends with `0/false`.

   This cannot be observed (the next Clear or general parse resets them, and `carry()` at Ground ignores
   them). But it means that (a) any equivalence must compare `View`, not fields, and (b) the
   `ParserModel` projection (`param_count`, `current_param`) **differs between whole and one-byte
   reads** of the same input. A refinement stated over that projection would be false for `advance_fast`.
4. **`refinement.rs` is not a refinement.** It defines `ParserModel` and `project`, but there is no
   abstract `Init`/`Next`, no `ty_model!`, and no conformance test with a negative control (contrary to
   AGENTS.md). The three tests only sample the projection. `test_ground_after_complete_sequence` pins
   that finalized params *remain* after dispatch at Ground. That is an implementation leak (item 3), not
   a property a model should promise. Recommendation: author `ParserModel` with `ty_model!` so that
   Ground has no parameters, project via `carry()` (which already hides the leak), and delete the three
   tests once the refinement is proved.
5. **`decode_multibyte_char` has an unstated precondition.** For leads ≥ 0xF8 it computes
   `lead & 0x07` and can return `Some`. For example, `(0xF8, [0x90,0x80,0x80])` gives `U+10000` and
   `(0xFC, [0x80,0x80,0x80])` gives `U+100000`. No caller passes such a lead (every caller is gated by
   `is_utf8_lead`, which is C2..=F4), and the exhaustive test restricts itself to `0xC0..=0xF7`. The
   spec must carry `requires is_utf8_lead(lead)`, or the function should reject `lead > 0xF4`.
6. **The test oracle is a copy.** `utf8_simd::tests::scalar_one` is a hand-kept "byte-for-byte copy" of
   `decode_multibyte_char`, and `ref_run` and the five sample tests compare against the copy, not the
   function. The spec `bulk-decode-run-refines-scalar` should reference `decode_multibyte_char` itself.
   The one exhaustive test already does.
7. **Resolved 2026-10-16: the two public entry points decoded Ground high bytes differently**
   (`advance` as Latin-1, `advance_fast` as UTF-8). `advance` is deleted; the reference for the one
   entry point is `foldl step_u`, and no function names that step today (§0).
8. **`restore_carry` accepts states the live parser cannot reach.** It accepts `subparam_mask` bit 0
   with ≥1 param (e.g. `params=[1], mask=1`), but the live parser never sets bit 0, because the first
   parameter cannot follow a colon. Restore guarantees *Safe*, not *Reachable*. That is fine for the
   tests as written, but `carry-restore-continues` can only be stated for `carry(p)` with `p∈Reach`, not
   for every accepted carry.
9. **The string terminator `ESC \\` is two sink calls, not one.** Escape row 0x5C is `EscDispatch`, so a
   string ended by `ESC \\` also emits `esc_dispatch([], '\\')`. No test asserts that, and several
   docs call `ESC \\` simply "ST". The OSC/DCS/APC denotations above state it, so a sink that
   treats `ESC \\` as an ESC sequence must ignore final `\\`.
10. **A doc cites a test for something the test does not check.** The NOTE in `invariants.rs` says
   `assert_invariants_after_subparams` "reaches Ground with `last_was_colon == true`". The claim is true
   (`ESC[4:3m`), but the test does not assert it.

## 6. The three specs I would prove first (easiest, high value)

1. **`osc-bulk-copy-len`**. This is pure linear `usize` arithmetic (`min`, `saturating_sub`) with no
   loops, so it is an `omega`-class proof. It retires the breakpoint-enumeration test and is the
   arithmetic core of the OSC half of the master simulation (the 8 MiB cap parity tests).
2. **`param-accumulator`** (`add_param_digit`/`finalize_param`/`push_current_param`/`close_params`).
   This is `&mut self` field updates, saturating `u32` and one bounded `ArrayVec<u16,24>` push. It
   retires 5 tests outright (including a 70,000-value loop) and is the lemma `csi-dispatch-denotation`
   (17 tests) and the strengthened `type-invariant` are built on.
3. **`bulk-decode-run-refines-scalar`** together with **`decode-multibyte-char`**. This is safe code
   written for Trust: no intrinsics, slice patterns, `ArrayVec<char,256>`, and a fixed-stride loop with an
   obvious invariant (`i = k·|appended|`). It retires all 11 utf8_simd tests, including the
   256×256×10×10 exhaustive one, removes the duplicated oracle (§5.6), and forces the missing
   `requires is_utf8_lead(lead)` (§5.5).

Runner-up: the four **table properties** (`dcs-header-gr-ignored`, and the table halves of
`dcs-ignore-bisimilar-passthrough` and `hooked-string-carry-bisimilar`, plus `state-name-roundtrip`).
They are decidable by evaluating the `const fn generate_table()`, so they are almost free, but they only
retire their tests fully once the bisimulation halves are proved.
