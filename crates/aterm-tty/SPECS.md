<!-- Copyright 2026 Andrew Yates -->
<!-- SPDX-License-Identifier: Apache-2.0 -->

# aterm-tty: Trust specs

> Status 2026-10-20 (amacs slice TR1). aterm-tty is on the Trust lane with a committed floor
> (`crates/aterm-release/trust/floors.tsv`: `aterm-tty 208 696`). Ten contracts on the decoder's
> byte classes are proved, each by a Clean theorem about the compiled function, and each one's
> negative control was run. §3 lists the properties that are not provable today, with the exact
> refusal for each, and §4 lists every obligation the lane leaves open in the crate's own code.

Source: `crates/aterm-tty/src/decode.rs` holds the contracts and islands, and the macros are in
`src/lib.rs`. Every number and quoted refusal below was measured, never inferred. The prover is
the lane's, `/Users//example/toolchains/trust-41d79979/bin`, which prints itself as
`targo 1.99.0-dev (41d799797 2026-10-16) (targo 0.10.0+g41d799797)`. The texts come from three
kinds of run:

- **Lane runs.** §1, §2, §4 and the §3 rows that name a lane obligation come from lane runs. Re-measure
  them with `targo --unverified ship trust run -p aterm-tty --obligations`.
- **Standalone copies.** The E6, clause and tactic refusals in §3 come from compiling a standalone
  copy of each candidate (`blk::…`, `t::…`, `e2`) with the same prover's `trustc` under
  verification. Every such candidate was refused, so the crate carries no contract in its refused
  form (`utf8_len` returning `usize` became the `u8` contract of §1).
- **The dev seal.** The `dev-seal-theorem` row was measured by compiling a standalone copy of
  three contracts as crate `aterm_tty` with the seal on `PATH` (`321aaeda7`), and the same copy
  with the lane's `trustc`.

## 0. How a contract is written

- **The contract.** A contracted fn goes through `trust_contract_fn!`, the idiom of `aterm-log`.
  Under Trust's verifying session (`cfg(trust_verify)`), the fn carries
  `ensures result == SPEC by THEOREM`. Every other compiler gets the plain fn: stock rustc in the
  foreign cells, and Trust with verification off.
- **The specs.** The specs are Clean `def`s in a `spec_island!`, which compiles under
  `cfg(clean_islands)`. `build.rs` sets that cfg whenever the compiler is Trust, so the specs are
  kernel-checked at every Trust build. They are transcriptions of the standards' tables and name
  no import. The lemma `ite_and_split`, which `utf8_len`'s proof uses, is in the same island.
- **The theorems.** The theorems are in `verify_island!`s, which compile under `cfg(trust_verify)`
  only. Each is stated over `trust_import_aterm_tty__decode__<fn>`, the kernel definition Trust
  mints from the compiled body, and Trust mints it only while it verifies. There is one island per
  theorem, so a refusal names its own theorem.
- **Why the lemma's proof reduces instances.** `ite_and_split` is proved by
  `cases dp <;> cases dq <;> rfl`, which the seal on `PATH` (`321aaeda7`) and the lane's prover
  (`41d79979`) both check. The seal refused the earlier proof,
  `by_cases hp : P <;> by_cases hq : Q <;> simp only […]`, with
  `` `ite_and_split` did not elaborate: Tactic failed: simp: no progress made ``, which kept the
  lemma out of the spec island until this proof replaced it.
- **The `u8` return.** `utf8_len` returns `u8` rather than `usize` so that its clause types at
  the parameter's carrier (§3, `usize-result-carrier`). The callers widen the result with
  `usize::from`.

## 1. Proved specs (TSV)

Columns:

- `id`
- `statement`, the property proved for every input
- `fn`, the contracted function in `src/decode.rs`
- `theorem`, the cited Clean theorem
- `verdict`, as the lane printed it
- `prover`
- `negative control`, the mutation, then the lane's refusal with the file restored byte for byte
  afterwards (§2)

```tsv
id	statement	fn	theorem	verdict	prover	negative control
utf8-lead-length	∀ lead: u8, utf8_len(lead) = 2 if C2 ≤ lead ≤ DF, 3 if E0 ≤ lead ≤ EF, 4 if F0 ≤ lead ≤ F4, else 1 (Unicode Table 3-7, first column)	utf8_len	utf8_len_is_table_3_7 (with ite_and_split)	[VERIFIED] (2 obligations: 2 proved, 0 runtime-checked, 0 failed, 0 unknown); postcondition PROVED (clean-kernel-citation, CONSTRUCTIVE (certified; validation=kernel-rechecked, closure=empty, coverage=unbounded, execution=unmonitored, reflection=unrecorded))	41d79979	0xC2..=0xDF => 2 → 0xC1..=0xDF => 2: `utf8_len_is_table_3_7` rejected (UnsolvedGoals after `simp`), utf8_len [INCONCLUSIVE]
utf8-second-min	∀ lead: u8, utf8_second_min(lead) = A0 if lead = E0, 90 if lead = F0, else 80 (Table 3-7, second column, lower bound)	utf8_second_min	utf8_second_min_is_table_3_7	[VERIFIED] (2 obligations: 2 proved, 0 runtime-checked, 0 failed, 0 unknown); postcondition PROVED (clean-kernel-citation, CONSTRUCTIVE (certified; validation=kernel-rechecked, closure=empty, coverage=unbounded, execution=unmonitored, reflection=unrecorded))	41d79979	0xE0 => 0xA0 → 0xE0 => 0x9F: `utf8_second_min_is_table_3_7` rejected (UnsolvedGoals after `simp`), utf8_second_min [INCONCLUSIVE]
utf8-second-max	∀ lead: u8, utf8_second_max(lead) = 9F if lead = ED, 8F if lead = F4, else BF (Table 3-7, second column, upper bound)	utf8_second_max	utf8_second_max_is_table_3_7	[VERIFIED] (2 obligations: 2 proved, 0 runtime-checked, 0 failed, 0 unknown); postcondition PROVED (clean-kernel-citation, CONSTRUCTIVE (certified; validation=kernel-rechecked, closure=empty, coverage=unbounded, execution=unmonitored, reflection=unrecorded))	41d79979	0xED => 0x9F → 0xED => 0xA0: `utf8_second_max_is_table_3_7` rejected (UnsolvedGoals after `simp`), utf8_second_max [INCONCLUSIVE]
utf8-continuation	∀ b: u8, is_utf8_continuation(b) ⇔ 80 ≤ b ≤ BF (Table 3-7, trailing bytes)	is_utf8_continuation	utf8_continuation_is_table_3_7	[VERIFIED] (4 obligations: 4 proved, 0 runtime-checked, 0 failed, 0 unknown); postcondition PROVED (clean-kernel-citation, CONSTRUCTIVE (certified; validation=kernel-rechecked, closure=empty, coverage=unbounded, execution=unmonitored, reflection=unrecorded))	41d79979	0x80..=0xBF → 0x80..=0xC0: `utf8_continuation_is_table_3_7` rejected (UnsolvedGoals after `simp`), is_utf8_continuation [VIOLATIONS]
csi-body	∀ b: u8, is_csi_body(b) ⇔ 20 ≤ b ≤ 3F (ECMA-48 §5.4: parameter bytes 03/00-03/15 and intermediate bytes 02/00-02/15)	is_csi_body	csi_body_is_ecma_48	[VERIFIED] (4 obligations: 4 proved, 0 runtime-checked, 0 failed, 0 unknown); postcondition PROVED (clean-kernel-citation, CONSTRUCTIVE (certified; validation=kernel-rechecked, closure=empty, coverage=unbounded, execution=unmonitored, reflection=unrecorded))	41d79979	0x20..=0x3F → 0x20..=0x40: `csi_body_is_ecma_48` rejected (UnsolvedGoals after `simp`), is_csi_body [VIOLATIONS]
csi-parameter	∀ b: u8, is_csi_parameter(b) ⇔ 30 ≤ b ≤ 3B (ECMA-48 §5.4.1: the bytes of a parameter string's sub-strings, 03/00-03/11: the digits, `:` and `;`)	is_csi_parameter	csi_parameter_is_ecma_48	[VERIFIED] (4 obligations: 4 proved, 0 runtime-checked, 0 failed, 0 unknown); postcondition PROVED (clean-kernel-citation, CONSTRUCTIVE (certified; validation=kernel-rechecked, closure=empty, coverage=unbounded, execution=unmonitored, reflection=unrecorded))	41d79979	0x30..=0x3B → 0x30..=0x3C: `csi_parameter_is_ecma_48` rejected (UnsolvedGoals after `simp`), is_csi_parameter [VIOLATIONS]
csi-private-marker	∀ b: u8, is_csi_private_marker(b) ⇔ 3C ≤ b ≤ 3F (ECMA-48 §5.4.1: a private parameter string's first byte, 03/12-03/15: `<`, `=`, `>`, `?`)	is_csi_private_marker	csi_private_marker_is_ecma_48	[VERIFIED] (4 obligations: 4 proved, 0 runtime-checked, 0 failed, 0 unknown); postcondition PROVED (clean-kernel-citation, CONSTRUCTIVE (certified; validation=kernel-rechecked, closure=empty, coverage=unbounded, execution=unmonitored, reflection=unrecorded))	41d79979	0x3C..=0x3F → 0x3B..=0x3F: `csi_private_marker_is_ecma_48` rejected (UnsolvedGoals after `simp`), is_csi_private_marker [VIOLATIONS]
csi-intermediate	∀ b: u8, is_csi_intermediate(b) ⇔ 20 ≤ b ≤ 2F (ECMA-48 §5.4: 02/00-02/15)	is_csi_intermediate	csi_intermediate_is_ecma_48	[VERIFIED] (4 obligations: 4 proved, 0 runtime-checked, 0 failed, 0 unknown); postcondition PROVED (clean-kernel-citation, CONSTRUCTIVE (certified; validation=kernel-rechecked, closure=empty, coverage=unbounded, execution=unmonitored, reflection=unrecorded))	41d79979	0x20..=0x2F → 0x20..=0x30: `csi_intermediate_is_ecma_48` rejected (UnsolvedGoals after `simp`), is_csi_intermediate [VIOLATIONS]
csi-final	∀ b: u8, is_csi_final(b) ⇔ 40 ≤ b ≤ 7E (ECMA-48 §5.4: 04/00-07/14)	is_csi_final	csi_final_is_ecma_48	[VERIFIED] (4 obligations: 4 proved, 0 runtime-checked, 0 failed, 0 unknown); postcondition PROVED (clean-kernel-citation, CONSTRUCTIVE (certified; validation=kernel-rechecked, closure=empty, coverage=unbounded, execution=unmonitored, reflection=unrecorded))	41d79979	0x40..=0x7E → 0x40..=0x7F: `csi_final_is_ecma_48` rejected (UnsolvedGoals after `simp`), is_csi_final [VIOLATIONS]
string-control	∀ b: u8, is_string_control(b) ⇔ b ≤ 1F ∨ b = 7F (C0 or DEL, which inside a DCS/OSC string means the string was typed keys)	is_string_control	string_control_is_c0_or_del	[VERIFIED] (6 obligations: 6 proved, 0 runtime-checked, 0 failed, 0 unknown); postcondition PROVED (clean-kernel-citation, CONSTRUCTIVE (certified; validation=kernel-rechecked, closure=empty, coverage=unbounded, execution=unmonitored, reflection=unrecorded))	41d79979	0x00..=0x1F | 0x7F → 0x00..=0x1F: `string_control_is_c0_or_del` rejected (UnsolvedGoals after `simp`), is_string_control [VIOLATIONS]
```

Lane totals on prover 41d79979, as the lane printed them:

- **Baseline** (origin/main `3eda2d3d76`, before this slice): `aterm-tty NO-FLOOR 174/751`.
- **First floor** (`57fe49ee50`, eight contracts): `aterm-tty FLOOR-RECORDED 199/686`.
- **Final** (`02d4360577`, `--obligations --update-ratchet`): `aterm-tty open 208 696` and
  `TOTAL: 208/696 proved`, exit 0. The floor row is `aterm-tty 208 696`.

From the baseline to the first floor the total fell, because the checked and saturating rewrites
(§4 lists what stays open) remove obligations outright. A `split_at` becomes a
`split_at_checked`, and an `a + b` becomes a `saturating_add`, so neither has a panic left to
prove. From the first floor to the final count:

- The two parameter classes add 8 proved postconditions.
- `Screen::new`'s `unbounded_allocation` obligation is now proved. At `57fe49ee50` it failed with
  `count = 268435456`, because the solver loses the product of the two clamped sides. `Screen::new`
  now checks the product against `MAX_CELLS` (`MAX_SIDE` squared, which the product never
  exceeds) before it allocates, so the bound stands where the obligation is checked.

The decoder reads the two standards through these classes at these sites:

- `utf8_lead` takes its sequence length and its second-byte bounds from `utf8_len`,
  `utf8_second_min` and `utf8_second_max`, where before it held a second copy of Table 3-7.
- The byte after a lead must fall in that range (the `Utf8` state). Every byte after it must pass
  `is_utf8_continuation` (the `Utf8Tail` state), where before the state held 80 and BF as
  literals.
- `trim_partial_char` uses `is_utf8_continuation` and `utf8_len`, and `first_char` uses
  `utf8_len`.
- `csi` splits a body into its private marker, parameters and intermediates with
  `is_csi_private_marker`, `is_csi_parameter` and `is_csi_intermediate`.
- `step` uses `is_csi_body` and `is_csi_final` in the CSI, SS3 and DCS states, and
  `is_string_control` inside DCS and OSC strings.

At those sites a proof about a class is a proof about the byte the decoder tests. Three byte sets
are still literals, because no class of the two standards is theirs:

- SS3's parameters, `0`-`9` and `;`. They are an xterm extension, and ECMA-48 gives SS3 no
  parameters.
- An OSC number: `is_ascii_digit`, then `;`.
- The letters that follow ESC: `[`, `O`, `P` and `]`.

## 2. Negative controls

For each contract, one lane run (`targo --unverified ship trust run -p aterm-tty --obligations`)
was made on a `decode.rs` with that one mutation. The file was then copied back, and `cmp` against
the saved original and `git diff --quiet HEAD` both passed before the next run. The table quotes
each run's refusal of the theorem and the verdict line of the fn.

- **One rejection per run.** In every run, the mutated theorem's island was the only island
  rejected. The fill script asserted this by counting the `Clean island declaration rejected`
  lines. The rejection taints the crate's Clean environment, so each run then reports every
  citation as `cannot be validated … tainted`. That is why each run's proved count is 11 to 14
  below the unmutated 199 on `a6bb4b6a2c`, and 13 to 14 below the unmutated 208 on `02d4360577`.
- **The bool classes fail twice.** For the five bool classes, the full verifier also refutes the
  postcondition on its own, with a verified counterexample.
- **The trees.** Eight runs were made on `a6bb4b6a2c`, before the floor was recorded, so their
  lane verdicts read `NO-FLOOR`. Their fns and theorems are unchanged since then, and the line
  numbers they quote are that tree's. Three runs were made on `02d4360577`, after the floor of
  199 was recorded, so theirs read `RATCHET-REGRESSION(<199)`. Those three are the two new
  contracts and `utf8_len`, whose lemma's proof changed.

```tsv
fn	tree	mutation	theorem refusal	fn verdict	lane verdict
utf8_len	a6bb4b6a2c	0xC2..=0xDF => 2 → 0xC1..=0xDF => 2	`utf8_len_is_table_3_7` rejected: Clean island declaration rejected: InTactic { tactic: "simp", … TacticFailed(UnsolvedGoals { count: 1, detail: … }) } at decode.rs:914; then citation `utf8_len_is_table_3_7` cannot be validated because an earlier Clean island was rejected and the crate-local environment is tainted	utf8_len [INCONCLUSIVE] (2 obligations: 0 proved, 0 runtime-checked, 0 failed, 2 unknown)	FAIL — aterm-tty NO-FLOOR 188/681
utf8_len	02d4360577	0xC2..=0xDF => 2 → 0xC1..=0xDF => 2	`utf8_len_is_table_3_7` rejected: Clean island declaration rejected: InTactic { tactic: "simp", … TacticFailed(UnsolvedGoals { count: 1, detail: … }) } at decode.rs:940; then citation `utf8_len_is_table_3_7` cannot be validated because an earlier Clean island was rejected and the crate-local environment is tainted	utf8_len [INCONCLUSIVE] (2 obligations: 0 proved, 0 runtime-checked, 0 failed, 2 unknown)	FAIL — aterm-tty RATCHET-REGRESSION(<199) 195/689
utf8_second_min	a6bb4b6a2c	0xE0 => 0xA0 → 0xE0 => 0x9F	`utf8_second_min_is_table_3_7` rejected: Clean island declaration rejected: InTactic { tactic: "simp", … TacticFailed(UnsolvedGoals { count: 1, detail: … }) } at decode.rs:923; then citation `utf8_second_min_is_table_3_7` cannot be validated because an earlier Clean island was rejected and the crate-local environment is tainted	utf8_second_min [INCONCLUSIVE] (2 obligations: 0 proved, 0 runtime-checked, 0 failed, 2 unknown)	FAIL — aterm-tty NO-FLOOR 188/681
utf8_second_max	a6bb4b6a2c	0xED => 0x9F → 0xED => 0xA0	`utf8_second_max_is_table_3_7` rejected: Clean island declaration rejected: InTactic { tactic: "simp", … TacticFailed(UnsolvedGoals { count: 1, detail: … }) } at decode.rs:932; then citation `utf8_second_max_is_table_3_7` cannot be validated because an earlier Clean island was rejected and the crate-local environment is tainted	utf8_second_max [INCONCLUSIVE] (2 obligations: 0 proved, 0 runtime-checked, 0 failed, 2 unknown)	FAIL — aterm-tty NO-FLOOR 188/681
is_utf8_continuation	a6bb4b6a2c	0x80..=0xBF → 0x80..=0xC0	`utf8_continuation_is_table_3_7` rejected: Clean island declaration rejected: InTactic { tactic: "simp", … TacticFailed(UnsolvedGoals { count: 1, detail: … }) } at decode.rs:941; then citation `utf8_continuation_is_table_3_7` cannot be validated because an earlier Clean island was rejected and the crate-local environment is tainted	is_utf8_continuation [VIOLATIONS] (3 obligations: 2 proved, 0 runtime-checked, 1 failed, 0 unknown); postcondition FAILED (trust-full-verifier) counterexample: verified_counterexample = true	FAIL — aterm-tty NO-FLOOR 188/682
is_csi_body	a6bb4b6a2c	0x20..=0x3F → 0x20..=0x40	`csi_body_is_ecma_48` rejected: Clean island declaration rejected: InTactic { tactic: "simp", … TacticFailed(UnsolvedGoals { count: 1, detail: … }) } at decode.rs:950; then citation `csi_body_is_ecma_48` cannot be validated because an earlier Clean island was rejected and the crate-local environment is tainted	is_csi_body [VIOLATIONS] (3 obligations: 2 proved, 0 runtime-checked, 1 failed, 0 unknown); postcondition FAILED (trust-full-verifier) counterexample: verified_counterexample = true	FAIL — aterm-tty NO-FLOOR 187/681
is_csi_parameter	02d4360577	0x30..=0x3B → 0x30..=0x3C	`csi_parameter_is_ecma_48` rejected: Clean island declaration rejected: InTactic { tactic: "simp", … TacticFailed(UnsolvedGoals { count: 1, detail: … }) } at decode.rs:985; then citation `utf8_len_is_table_3_7` cannot be validated because an earlier Clean island was rejected and the crate-local environment is tainted	is_csi_parameter [VIOLATIONS] (3 obligations: 2 proved, 0 runtime-checked, 1 failed, 0 unknown); postcondition FAILED (trust-full-verifier) counterexample: verified_counterexample = true	FAIL — aterm-tty RATCHET-REGRESSION(<199) 195/690
is_csi_private_marker	02d4360577	0x3C..=0x3F → 0x3B..=0x3F	`csi_private_marker_is_ecma_48` rejected: Clean island declaration rejected: InTactic { tactic: "simp", … TacticFailed(UnsolvedGoals { count: 1, detail: … }) } at decode.rs:994; then citation `utf8_len_is_table_3_7` cannot be validated because an earlier Clean island was rejected and the crate-local environment is tainted	is_csi_private_marker [VIOLATIONS] (3 obligations: 2 proved, 0 runtime-checked, 1 failed, 0 unknown); postcondition FAILED (trust-full-verifier) counterexample: verified_counterexample = true	FAIL — aterm-tty RATCHET-REGRESSION(<199) 194/689
is_csi_intermediate	a6bb4b6a2c	0x20..=0x2F → 0x20..=0x30	`csi_intermediate_is_ecma_48` rejected: Clean island declaration rejected: InTactic { tactic: "simp", … TacticFailed(UnsolvedGoals { count: 1, detail: … }) } at decode.rs:959; then citation `csi_intermediate_is_ecma_48` cannot be validated because an earlier Clean island was rejected and the crate-local environment is tainted	is_csi_intermediate [VIOLATIONS] (3 obligations: 2 proved, 0 runtime-checked, 1 failed, 0 unknown); postcondition FAILED (trust-full-verifier) counterexample: verified_counterexample = true	FAIL — aterm-tty NO-FLOOR 188/682
is_csi_final	a6bb4b6a2c	0x40..=0x7E → 0x40..=0x7F	`csi_final_is_ecma_48` rejected: Clean island declaration rejected: InTactic { tactic: "simp", … TacticFailed(UnsolvedGoals { count: 1, detail: … }) } at decode.rs:968; then citation `csi_final_is_ecma_48` cannot be validated because an earlier Clean island was rejected and the crate-local environment is tainted	is_csi_final [VIOLATIONS] (3 obligations: 2 proved, 0 runtime-checked, 1 failed, 0 unknown); postcondition FAILED (trust-full-verifier) counterexample: verified_counterexample = true	FAIL — aterm-tty NO-FLOOR 187/681
is_string_control	a6bb4b6a2c	0x00..=0x1F | 0x7F → 0x00..=0x1F	`string_control_is_c0_or_del` rejected: Clean island declaration rejected: InTactic { tactic: "simp", … TacticFailed(UnsolvedGoals { count: 1, detail: … }) } at decode.rs:977; then citation `string_control_is_c0_or_del` cannot be validated because an earlier Clean island was rejected and the crate-local environment is tainted	is_string_control [VIOLATIONS] (4 obligations: 2 proved, 0 runtime-checked, 1 failed, 1 unknown); postcondition FAILED (trust-full-verifier) counterexample: verified_counterexample = true	FAIL — aterm-tty NO-FLOOR 185/680
```

## 3. Not provable today (TSV)

Columns: `id`, `property`, `where`, `refusal or gap (exact text, prover 41d79979 unless noted)`
and `what would make it provable`.

```tsv
id	property	where	refusal or gap	what would make it provable
number-digit-step	∀ n, d: number's fold step n·10 + (d − '0') saturates at u32::MAX instead of wrapping	decode.rs number	E6 admission of `blk::digit_step` was refused by the FACET gate (Pure, Total, Deterministic and NoPanic must all be established): Pure (not established: call to `core::num::<impl u32>::saturating_mul`, which is not itself structurally certified (facets are closed over callees by exact def-path identity)), … The wrapping form with a guard is refused by the SHAPE gate: "the construct outside the E6 fragment is the call to `@trust-rustc-total-primitive-method::core::num::<impl u32>::wrapping_mul`" (a wrapping `*` inside a decision arm).	E6 admitting `saturating_*` (or a wrapping `*` under a guard) as a structurally certified primitive
modifiers-lock-drop	∀ bits: modifiers drops Caps Lock and Num Lock (bits & 0x3F after the − 1)	decode.rs modifiers	E6 admission of `blk::chord_bits` was refused by the SHAPE gate: all four E6 facets hold, but no E6 reader reads its body (…); the construct outside the E6 fragment is the bit operation `&` (`BinOp::BitAnd`) on `u8`.	E6 reading `&`/`|` on unsigned integers (today only on `bool`)
mouse-cell-clamp	∀ col: a mouse report's 1-based column becomes col − 1, clamped into u16	decode.rs mouse (its `cell` closure)	E6 admission of `blk::cell` was refused by the FACET gate (Pure, Total, Deterministic and NoPanic must all be established): Pure (not established: local _2 has type Adt { name: "core::result::Result", … TryFromIntError … })	E6 reading `Result`/`Option` locals (`u16::try_from(..).unwrap_or(..)`)
screen-index	∀ row < rows, col < cols: index(row, col) = row·cols + col < rows·cols	screen.rs Screen::index	E6 admission of `blk::index` was refused by the FACET gate (…): Pure (not established: call to `<usize as core::convert::From<u16>>::from`, which is not itself structurally certified (facets are closed over callees by exact def-path identity)), …	E6 treating widening `From` as a cast; and the bound below needs a type invariant
screen-at-get-bounds	Screen::at and Screen::get index inside `cells`	screen.rs:397 (at), screen.rs:401 (get)	slice_bounds_check failed: `[index]` in aterm_tty::screen::Screen::at and `[self.index(row, col)]` in aterm_tty::screen::Screen::get ("slice bounds check"). Each holds only under the struct invariant cells.len() = rows·cols together with the callers' row < rows ∧ col < cols. The lane has no type invariant to carry it. Both bodies index by design, since every caller stays inside the frame, and the public `cell()` uses `get` behind its own guard.	A Trust type invariant on `Screen` (cells.len() = rows·cols), and `requires` on private fns
utf8-semantic	The Table 3-7 bounds reject exactly the overlong forms, the surrogates and the code points past U+10FFFF (e.g. lead E0 ⇒ second ≥ A0 ⇔ the code point ≥ U+0800)	decode.rs utf8_lead	Clean island declaration rejected: TacticFailed(ArithmeticFailed { tactic: "omega", reason: "(linarith fallback) could not extract linear constraints and ay_lra failed" }). omega does not reason over UInt8, and the Nat statements it is given do not close. `decide` over ∀ UInt8 does not elaborate either.	omega or decide over fixed-width integers (UInt8 ↔ Nat bridges)
usize-result-carrier	utf8_len returning usize with its u8 spec	decode.rs utf8_len	citation `utf8_len_table` failed the strict Clean statement/certification audit: the postcondition goal could not be built (the elaborated goal does not typecheck (TypeMismatch { expected: … "UInt8" …, inferred: … "UInt64" … }); a call in the clause most likely applies an island definition at a different carrier than its declared domain …)	Worked around: utf8_len returns u8 and its callers widen it with usize::from
clause-if	A non-bool spec written inline in the clause (`ensures result == if … then … else …`)	decode.rs	"unsupported contract predicate expression". The clause language has no `if`.	Worked around: the spec is an island `def` that the clause names
dev-seal-theorem	The theorems checked by the seal on PATH (321aaeda7), which the `targo --unverified` builds use	decode.rs theorem islands	Clean island declaration rejected: AutoBoundHeadOverApplied { name: "trust_import_aterm_tty__decode__utf8_second_min", func_type: "fvar#9223372036854775808", remaining_args: 1 }, one per theorem (a standalone copy of three contracts, compiled as crate aterm_tty by the seal's trustc). The seal mints no `trust_import_*` definition, so it can check no citation whatever the lemma. It refused the earlier `simp` proof of `ite_and_split` (`ite_and_split` did not elaborate: Tactic failed: simp: no progress made) and accepts the `cases` proof now in the spec island (Clean island kernel-checked: 4 declaration(s) registered (utf8_len_spec, utf8_second_min_spec, utf8_second_max_spec, ite_and_split)).	A seal at or past 41d79979 on PATH. Until then the theorems live in verify islands, which only the lane compiles; the specs and the lemma are kernel-checked by every Trust build.
probe-deadline	probe's `tty.now() + PROBE_TIMEOUT` never overflows	probe.rs:94	[FAILED: unproved, no counterexample; failed closed] assertion: [trust-absent-callee-assumption] call to absent callee … (body not in the lowered bundle) may panic. The lane keeps one such row for the fn, and the callee it names changed between runs on an unchanged probe.rs: `<std::time::Instant as core::ops::arith::Add<core::time::Duration>>::add` at 57fe49ee50, `<T as aterm_tty::tty::Tty>::read` at 02d4360577.	The std `Instant` arithmetic bodies in the lowered bundle (a monotonic clock is nowhere near Instant's limit), and the `Tty` instances' bodies
ffi-rows	raw-mode termios, signal and fd calls are sound	raw.rs, signals.rs, tty.rs (DevTty::read, size_of_fd)	hardened_unsafe_operation failed and hardened_ffi_boundary/hardened_panic_boundary unknown on every `unsafe` libc call (tcgetattr/tcsetattr, sigaction, pipe, read/write, ioctl, kill, errno)	Trust contracts for the libc functions themselves; nothing in this crate can discharge them
```

`Screen::new`'s allocation bound, a row here at `57fe49ee50`, is proved since `02d4360577` (§1).

## 4. Every obligation left open in the crate's own code (TSV)

This section is the worklist for the next prover. It covers all 488 obligations of the final lane
run (`02d4360577`) that are not proved: 696 total less 208 proved, which is 234 unknown, 194
runtime-checked, 52 failed and 8 design requirements. They are grouped by the lane's reason, and
each group gives:

- the count, split by status and kind;
- up to two distinct texts, exactly as the lane printed them;
- where the rows are: `file:line`, or the count per file for a group of more than 16 sites;
- what the rows are.

The full per-row list is `target-trust.noindex/logs/aterm-tty@<version>.log.oblig.tsv` after
`targo --unverified ship trust run -p aterm-tty --obligations`.

```tsv
id	rows	status and kind	reason (exact lane text)	where	reading
alloc-vacuous	162	162 unknown unsupported_mir	allocation(s) and/or allocation-owning drop(s) in this body rely on `GlobalAlloc::{alloc,dealloc}` never panicking; DISCHARGED vacuously: no `#[global_allocator]` is registered in the crate graph, so every allocation and dealloc is `Global`'s std chain (`__rust_alloc` / `__rust_dealloc` → the std default allocator), which has no panic path; conditional on the final link registering no other allocator	decode.rs 31, probe.rs 7, raw.rs 19, screen.rs 40, signals.rs 17, tty.rs 48 (rows per file)	No hazard: the lane discharges it vacuously (no `#[global_allocator]` in the graph) and still counts the row open. Nothing in this crate can close it.
absent-callee-dyn	23	23 runtime_checked assertion	assertion: [trust-absent-callee-assumption] call to absent callee `<dyn core::fmt::Write as core::fmt::Write>::write_str` (dynamic dispatch on receiver `dyn core::fmt::Write`: the concrete impl is chosen at runtime by whichever value was unsized into the trait object in a caller, so there is no single body to lower here and Trust will not assume one) may panic	decode.rs 5, probe.rs 1, raw.rs 1, screen.rs 7, signals.rs 2, tty.rs 7 (rows per file)	A `dyn` call (Debug/Display formatting, `dyn Write`): there is no single body to lower. rustc's own checks stand.
absent-callee-reach	19	19 runtime_checked assertion	assertion: [trust-absent-callee-assumption] call to absent callee `<aterm_types::keyboard::key_types::Key as core::clone::Clone>::clone` (CALL-REACH-1: its instance reaches `<aterm_types::keyboard::key_types::Key as core::clone::Clone>::clone`, a body outside the sysroot (crate `aterm_types`)) may panic / assertion: [trust-absent-callee-assumption] call to absent callee `<&aterm_types::keyboard::key_types::Key as core::cmp::PartialEq>::eq` (CALL-REACH-1: its instance reaches `<aterm_types::keyboard::key_types::Key as core::cmp::PartialEq>::eq`, a body outside the sysroot (crate `aterm_types`)) may panic (and 10 more of the same shape)	decode.rs 9, screen.rs 8, signals.rs 1, tty.rs 1 (rows per file)	A call whose instance reaches another crate (aterm-types, aterm-grapheme) or does not resolve. That crate's own rows carry it.
absent-callee-failed	1	1 failed assertion	assertion: [trust-absent-callee-assumption] call to absent callee `<T as aterm_tty::tty::Tty>::read` (body not in the lowered bundle) may panic	probe.rs:94	The probe fn's absent-callee row, failed closed (§3 probe-deadline). The callee it names changes between runs on unchanged probe.rs: `<std::time::Instant as core::ops::arith::Add<core::time::Duration>>::add` at 57fe49ee50, `<T as aterm_tty::tty::Tty>::read` at 02d4360577.
absent-callee	110	110 runtime_checked assertion	assertion: [trust-absent-callee-assumption] call to absent callee `aterm_tty::decode::Decoder::new` (body not in the lowered bundle) may panic / assertion: [trust-absent-callee-assumption] call to absent callee `core::iter::adapters::zip::zip::<&[&str], &[&dyn core::fmt::Debug]>` (body not in the lowered bundle) may panic (and 63 more of the same shape)	decode.rs 22, probe.rs 3, raw.rs 17, screen.rs 19, signals.rs 9, tty.rs 40 (rows per file)	A call whose body is not in the lowered bundle (std, or one of this crate's own fns). rustc's own checks stand. Needs those bodies in the bundle.
panic-freedom	34	34 runtime_checked assertion	assertion: panic freedom: no assertion, `unreachable!`, or panic is reachable	decode.rs 15, probe.rs 1, raw.rs 3, screen.rs 10, signals.rs 3, tty.rs 2 (rows per file)	The fn-level panic-freedom row. It is published runtime-checked because one of the fn's callee rows is.
std-internal-assert	5	5 runtime_checked assertion	assertion: panic freedom: bundled std member `<core::str::lossy::Utf8Chunks<'_> as core::iter::traits::iterator::Iterator>::next` carries an internal Assert that no row of this function discharges (an Assert sits off its unguarded entry prefix, so no unconditional caller-side charge exists for it); it is left to rustc's runtime check	decode.rs:371, decode.rs:551, decode.rs:583, decode.rs:677, decode.rs:751	An assert inside std (`Utf8Chunks::next`, …) that rustc keeps. Nothing here to change.
debug-assert	1	1 runtime_checked assertion	assertion: panic call: core::panicking::assert_failed::<usize, usize>	put (@sysroot/core/src/macros/mod.rs)	`Screen::put`'s `debug_assert_eq!` on the column walk (screen.rs:469), reported at its macro. A debug-only consistency check, by design.
null-ref	1	1 runtime_checked assertion	assertion: null reference constructed	raw.rs:76	rustc's check on `&mut *raw.as_mut_ptr()` in `Termios::raw`. The pointer is a live local `MaybeUninit<termios>`.
native-gap	3	3 unknown assertion	assertion: native-verification-gap	diff (@none), paint (@none), repaint (@none)	No source span: `Painter::paint`, `diff` and `repaint`. The lane gives no further reason.
index	1	1 runtime_checked index_out_of_bounds	index out of bounds	raw.rs:81	`t.c_cc[libc::VTIME]` in `Termios::raw`: a constant index into the fixed `c_cc` array, below `NCCS`.
unsafe-safety-comment	28	28 failed hardened_unsafe_operation	assertion: [unsafe] missing SAFETY comment on unsafe block	raw.rs 10, signals.rs 15, tty.rs 3 (rows per file)	Each of these sites has a `// SAFETY:` comment within the six lines above it (checked by reading each one). The lane's check does not recognise the comment where it stands, and moving it does not help: in a standalone probe of an FFI call with the comment above the block, inside it, above the `let` and trailing it, every placement left the call's hardened_unsafe_operation row FAILED (structural-unsafe-demand-finding) under 41d79979. A libc model, as for the ffi rows, is what would close these.
unsafe-zeroed	5	5 failed hardened_unsafe_operation	assertion: [unsafe:zeroed] `mem::zeroed::<libc::darwin_aarch64::sigaction>()` is undefined behaviour or a guaranteed abort: `libc::darwin_aarch64::sigaction` is a struct/union; the legacy type lowering erases pattern-type validity invariants (`NonZero`, `NonNull`, …), so no field list is trusted / assertion: [unsafe:zeroed] `mem::zeroed::<libc::darwin_aarch64::winsize>()` is undefined behaviour or a guaranteed abort: `libc::darwin_aarch64::winsize` is a struct/union; the legacy type lowering erases pattern-type validity invariants (`NonZero`, `NonNull`, …), so no field list is trusted	raw.rs:179, signals.rs:145, signals.rs:271, signals.rs:274, tty.rs:458	Zero-initialised libc structs (`sigaction`, `winsize`). All-zero is a valid value for each, but Trust trusts no libc field list.
unsafe-addr-of	5	5 failed hardened_unsafe_operation	assertion: [unsafe:sep:addr_of] &raw const on `_10` (source liveness unverified) / assertion: [unsafe:sep:addr_of] &raw const on `_9` (source liveness unverified) (and 3 more of the same shape)	raw.rs:182, raw.rs:393, signals.rs:240, signals.rs:282, signals.rs:294	Out-parameters handed to libc.
unsafe-deref	10	10 failed hardened_unsafe_operation	assertion: [unsafe:sep:deref] null check for *_1.* / assertion: [unsafe:sep:deref] allocation validity for *_1.* (and 5 more of the same shape)	signals.rs:78, signals.rs:96, signals.rs:112	The errno slot (`*libc::__error()`), read and written from the signal handler.
unsafe-reborrow	1	1 failed hardened_unsafe_operation	assertion: [unsafe:reborrow] `&mut *ptr` reborrows a raw pointer into a reference; Trust has no model of the reference's validity (non-null, aligned, dereferenceable, initialized, unaliased for its whole lifetime), so the site is unproved (fail-closed; a `// SAFETY:` comment documents it but does not discharge it)	raw.rs:76	The termios edit in `Termios::raw`.
ffi-summary	17	7 design_requirement hardened_ffi_boundary, 10 unknown hardened_ffi_boundary	FFI boundary violation in `sigaction`: unmodeled FFI call: no summary, cannot prove safe / FFI boundary violation in `tcsetattr`: unmodeled FFI call: no summary, cannot prove safe (and 8 more of the same shape)	raw.rs 8, signals.rs 9 (rows per file)	A libc call with no Trust summary (`tcgetattr`, `tcsetattr`, `sigaction`, `pipe`, `ioctl`, `kill`, `__error`, …). Trust contracts for libc itself would close these; nothing in this crate can.
ffi-nonnull	5	5 unknown hardened_ffi_boundary	FFI boundary violation in `read`: FFI `read`: parameter 1 must be non-null at read / FFI boundary violation in `write`: FFI `write`: parameter 1 must be non-null at write (and 3 more of the same shape)	raw.rs:130, raw.rs:374, signals.rs:109, signals.rs:249, tty.rs:405	`read`/`write` from a live slice. Its pointer is non-null by construction, but Trust has no libc summary to check it against.
panic-boundary	9	9 unknown hardened_panic_boundary	hardened boundary (panic_boundary): mir_assert::MisalignedPointerDereference: MIR runtime assert can panic; hardened code needs a proved precondition / hardened boundary (panic_boundary): mir_assert::Custom("null reference constructed"): MIR assert can panic at runtime; hardened code needs this precondition proved: null reference constructed (and 2 more of the same shape)	raw.rs:76, raw.rs:81, signals.rs:78, signals.rs:96, signals.rs:112	rustc's runtime asserts on the raw-pointer derefs and the `c_cc` index of the rows above.
unmodeled-safety-assert	7	7 unknown unsupported_mir	unsupported MIR `UnmodeledSafetyAssert(MisalignedPointerDereference)`: BlockId(3): safety assert not modeled by vcgen: MisalignedPointerDereference / unsupported MIR `UnmodeledSafetyAssert(MisalignedPointerDereference)`: BlockId(1): safety assert not modeled by vcgen: MisalignedPointerDereference (and 4 more of the same shape)	raw.rs:76, signals.rs:78, signals.rs:96, signals.rs:112	The same pointer derefs, which vcgen does not model.
self-ref	1	1 unknown unsupported_mir	unsupported MIR `SelfReferentialDefinition`: `_7*.0#s7_10` is defined in terms of itself: Eq(Var("_7*.0#s7_10", Int), BvToInt(BvAnd(IntToBv(Var("_7*.0#s7_10", Int), 64), IntToBv(Var("_9", Int), 64), 64), 64, false))	raw.rs:80	The `&=` flag edits on the termios fields.
addr-of-field	2	2 unknown unsupported_mir	unverified assumption: MIR shape the verifier cannot lower yet (addr-of-field)	raw.rs:195, signals.rs:154	`&raw mut` on a field of a local `sigaction`.
alloc-count	7	7 unknown unsupported_mir	unsupported MIR `UnboundedAllocation::<[T]>::resize::count-not-derivable`: bulk allocation `<[T]>::resize` recognized but element count not derivable from optimized MIR (size operand absent at index 1); cannot prove the allocation is bounded / unsupported MIR `UnboundedAllocation::Iterator::collect::count-not-derivable`: bulk allocation recognized (collect/from_iter) but element count not derivable from optimized MIR; cannot prove the allocation is bounded (and 1 more of the same shape)	decode.rs:774, decode.rs:1103, decode.rs:1136, decode.rs:1138, raw.rs:274, raw.rs:343, tty.rs:231	Each is bounded by its input: `fields` splits a held CSI body (at most `MAX_SEQUENCE_BYTES`), `typed_keys` extends by the characters of a held run, `RawGuard::enter` collects at most three signals, and `reassert` and `reset_bytes` walk constant mode tables. The solver cannot derive the count.
instance-hypothesis	5	5 unknown unsupported_mir	unverified assumption: [trust-instance-hypothesis] call to `<T as aterm_tty::tty::Tty>::write_all` in `aterm_tty::probe::probe`: the panic-freedom of this open generic dispatch is assumed for every instantiation; the obligation moves to each instance / unverified assumption: [trust-instance-hypothesis] call to `<T as aterm_tty::tty::Tty>::now` in `aterm_tty::probe::probe`: the panic-freedom of this open generic dispatch is assumed for every instantiation; the obligation moves to each instance (and 1 more of the same shape)	probe.rs:95, probe.rs:96, probe.rs:105, probe.rs:112, probe.rs:115	`probe` is generic over `T: Tty`, so each `T::…` call's panic-freedom moves to the instance.
instance-inventory	1	1 unknown unsupported_mir	unverified assumption: instance inventory incomplete: discharge rows exist only for instances named in the bodies this compilation verified or created by the vtables and drop glue those bodies build; an instance created inside a dependency's generic code (a vtable it builds from a type this crate hands it) or in an unwalked body has none	probe (@none)	No source span (`probe`).
termination	1	1 unknown unsupported_mir	unverified assumption: termination of `aterm_tty::decode::Decoder::step`: this self call binds no `decreases` measure, and the direct-self termination lane made no claim for it	decode.rs:472	`step` calls itself once, after an aborted string ESC has reset the state to `Ground`, so the depth is at most two.
callee-return	1	1 unknown unsupported_mir	unsupported MIR `UnmodeledCalleeReturn`: the obligation's verdict is the return value of an unmodelled call (`run`), which nothing in this body constrains; a solver model over it is not an execution, so the row is Unknown rather than refuted	decode.rs:773	`rest.split_at(run)` in `typed_keys`, where `run` is a `position` in `rest` or `rest.len()`, so it is never out of range.
slice-range	2	2 unknown unsupported_mir	unsupported MIR `slice-range-len-unresolved`: slice range index whose receiver length is not modeled at the call (no slice length, no recoverable owned-container length) — the bounds cannot be established; reported Unknown, never refuted on an arbitrary model and never silently verified	screen.rs:666	`Painter::paint`'s per-row comparison of `prev` and `next`. `diff` calls it only on frames of equal size, so this needs the same type invariant as §3 screen-at-get-bounds.
owned-vec-slice	1	1 unknown unsupported_mir	unsupported MIR `SliceBoundsCheck`: slice length is a non-derivable owned-Vec abstract placeholder; fail-honest to Unknown rather than false-REFUTE a provably-safe slice	tty.rs:655	`MemTty::read` (the test double) copies `n = min(len, buf.len())` bytes.
time-add	1	1 unknown unsupported_mir	unsupported MIR `time-point-duration-overflow`: Instant/SystemTime + or - a Duration panics when the result is not representable; only a small literal Duration added to or subtracted from a fresh `Instant::now()`/`SystemTime::now()` reading is bounded — here the Duration is not such a literal, or the time point is caller-supplied, loop-carried or a constant — refused, never silently verified (use `checked_add`/`checked_sub`)	tty.rs:570	`MemPeer::schedule` (the test double's scripted clock) adds a script delay to its last instant.
slice-bounds	2	2 failed slice_bounds_check	slice bounds check	screen.rs:397, screen.rs:401	§3 screen-at-get-bounds.
error-discard	13	13 unknown hardened_error_discard	hardened boundary (error_discard): core::option::Option::<&[u8]>::unwrap_or_default: unwrap_or_default can erase an error into a successful default value / hardened boundary (error_discard): core::result::Result::<&str, core::str::error::Utf8Error>::ok: Result::ok discards the error channel; hardened code needs a proof-bearing allow or propagation (and 3 more of the same shape)	decode.rs:311, decode.rs:520, decode.rs:557, decode.rs:585, decode.rs:645, decode.rs:683, decode.rs:763, decode.rs:783, decode.rs:1166, decode.rs:1273, decode.rs:1328, raw.rs:132, raw.rs:382	Each one is a deliberate fallback. A `get` that misses means there is nothing to deliver, a reply that is not UTF-8 or is out of range is malformed and delivered `Unknown`, and decode.rs:683 is `lossy_paste`'s `get(..end).unwrap_or_default()` from this slice. Needs a proof-bearing allow.
byte-loss	2	2 unknown hardened_byte_loss	hardened boundary (byte_loss): alloc::string::String::from_utf8_lossy: lossy UTF-8 conversion can corrupt byte-exact input/output	decode.rs:615, decode.rs:774	Intended: typed bytes and an XTVERSION name become text, with U+FFFD where they are malformed, as documented.
utf8-reject	1	1 design_requirement hardened_utf8_reject	hardened boundary (utf8_reject): alloc::string::String::from_utf8: strict UTF-8 conversion can reject valid Unix filenames or byte streams	decode.rs:661	`finish_paste` tries strict UTF-8 first and falls back to `lossy_paste`, so nothing is rejected.
raw-path	1	1 unknown hardened_raw_path_api	hardened boundary (raw_path_api): std::fs::OpenOptions::open::<&str>: OpenOptions::open on a path needs explicit no-follow/create/mode and trust-domain contracts	tty.rs:325	`DevTty::open` opens `/dev/tty`, the controlling terminal. A no-follow flag means nothing for it.
```

Two groups index on an invariant the lane cannot state (`cells.len() = rows·cols`): `slice-bounds`
(§3 `screen-at-get-bounds`) and `slice-range`. `callee-return`, `owned-vec-slice` and `index` are
in range by their own code. The rest are `unsafe` and FFI sites Trust has no model for, calls whose
bodies are not in the lowered bundle, std's and rustc's own checks, and deliberate fallbacks.

## 5. Tests

No test is deleted. The proved classes are each a few bytes of a table, and no existing test
samples a class on its own. The decoder's tests (`tests/decode_differential.rs`) are
whole-decoder round trips through aterm's encoders, the writer's (`tests/screen_oracle.rs`) are
differential checks against aterm's grid, and the proofs do not subsume them. Two tests were
added:

- `screen::tests::a_frame_is_clamped_to_the_grid_size_limit` pins the `MAX_SIDE` clamp, the one
  behaviour this slice changed (frames past 4096 rows or columns).
- `decode_differential::a_broken_character_reads_as_from_utf8_lossy_reads_it` pins the
  `Utf8`/`Utf8Tail` split. A second byte outside its lead's range, and a third or fourth byte
  that is not a trailing byte, decode exactly as `String::from_utf8_lossy` reads them.
