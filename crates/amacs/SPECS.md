<!-- Copyright 2026 Andrew Yates -->
<!-- SPDX-License-Identifier: Apache-2.0 -->

# amacs: specs proved on the Trust lane, and the ones still blocked

amacs is Rust on the Trust toolchain, and the owner wants it verified as well
as built. This file lists what the lane proves about the crate today:

- the contracts on its pure scalar core, each with a Clean theorem that the
  kernel checks against the compiled body;
- the negative control that shows each theorem depends on that body;
- the hazards that were made provable;
- the properties that cannot be proved yet, each with the exact refusal that
  blocks it.

Sections 1 to 5 cover the editor core, the protocol and the rope. Section 6
covers the language table (`language` and its submodules, slice L1) on its
own: its contracts, negative controls, refused forms and unproved rows.
Section 7 covers the scalar decisions of slice S5's P0 commands
(`fill-paragraph`'s word fit and TAB's distance to the next stop).

The recipe is `docs/PLAN-tests-to-specs-2026-10-15.md` (an internal plan).
All of it was measured on 2026-10-20 with the prover the lane uses on this host:
`targo 1.99.0-dev (41d799797 2026-10-16) (targo 0.10.0+g41d799797)`, at
`~/toolchains/trust-41d79979`.

## 1. The lane, before and after

`targo --unverified ship trust run -p amacs --obligations` runs `targo trust check
-p amacs --lib --allow-l0-gaps` and writes one row per obligation.

| tree | proved / total | failed | timeout | unknown | runtime-checked |
| --- | --- | --- | --- | --- | --- |
| `origin/main` 3eda2d3d76 (baseline) | 269 / 929 | 45 | 7 | 328 | 280 |
| f0610e8635 (the six contracts and the overflow fixes) | 310 / 939 | 18 | 7 | 326 | 278 |
| e1191a9811 (plus the `keyed` and `pointer_select` index fixes) | 311 / 935 | 13 | 7 | 325 | 279 |
| 964b6b8a8e (plus the clippy `#[expect]`s) | 311 / 935 | 13 | 7 | 325 | 279 |
| 3bf329aea4 (review round: the completion-wrap guard and `push_percent_escape`) | 318 / 935 | 13 | 7 | 317 | 280 |
| 3a9c7b2a0c (plus `projected_column_limit`) | 325 / 942 | 13 | 7 | 317 | 280 |
| S6's b8810716dc (the edit script, `line:h6` anchors and reads, before this table's contracts; `&dyn BaseMap`) | 360 / 3594 | not recorded | not recorded | not recorded | not recorded |
| S6's c3834102aa (`PlanOptions::base_map` a concrete `&LineMap`) | 362 / 1233 | not recorded | not recorded | not recorded | not recorded |
| S6 merged with 3a9c7b2a0c's line (origin/main ec4864b065) | 418 / 1246 | 13 | 7 | 414 | 393 |
| L1's language table (§6) merged with that line (`amacs/L1` f7e865eee3, origin/main 90f10d9fc6) | 639 / 1717 | 13 | 7 | 520 | 537 |
| that tree merged with origin/main 2cf0ec1c81, whose f31b5d99fb rejects misplaced patch EOF markers in `protocol::script` (`amacs/L1` 978fa5518f) | 639 / 1727 | 13 | 7 | 527 | 540 |
| S5's P0 commands (§7) on their own branch from origin/main 623a1a840f (`amacs/S5` 5ac0f6237c) | 336 / 1144 | not recorded | not recorded | not recorded | not recorded |
| S5 merged with that line (origin/main ce903e0d35), and `record_kill` without `insert_str` (§7; the floor in `floors.tsv`) | **704 / 1943** | 13 | 4 | 614 | 606 |

The floor committed before this work was `amacs 191 691`, measured under
`edbb3f300`. `--update-ratchet` re-bases the row onto `41d799797` at the
measured count (`crates/aterm-release/trust/floors.tsv`).

The S6 rows are the edit-script slice (`protocol::{anchor,read,script}`),
which grew on its own branch and was then merged with the rows above. Its
first measurement, 360/3594, had a `&dyn BaseMap` in the planner. That `dyn`
call was an open principal: the lane named it as the reason 2374 of the
crate's 2801 unknown obligations stayed unknown ("pools unclosed because:
open principal `dyn protocol::script::BaseMap` @ protocol::script::map_base"),
and `map_base` carried the one failed obligation of the slice's files. With
`PlanOptions::base_map` a concrete `&LineMap` the crate measured 362/1233:
fewer obligations and more of them proved. The merged tree measured
`TOTAL: 418/1246 proved` (`--update-ratchet`, the floor now), with the gate
line `418 proved, 13 failed, 393 runtime-checked, 328 assumed, 1 mandated, 0
contract-panic, 93 unknown / 1246 total`. None of its failed or timed-out rows
is in S6's files.

The language table of slice L1 (§6) grew on its own branch from 269/929 to
490/1400 and was then merged with that tree. The merged tree measured
`TOTAL: 639/1717 proved` (`--update-ratchet`, the floor now), with the gate
line `639 proved, 13 failed, 537 runtime-checked, 423 assumed, 1 mandated, 0
contract-panic, 104 unknown / 1717 total`. The two lines add up exactly: the
language fns contribute 471 rows and 221 proofs, as they did on L1's branch
(1400 − 929 and 490 − 269): 470 rows under `crates/amacs/src/language*` and
one sourceless row of `toml::lex`. The other 1246 rows have the same 418
proofs, 13 failures and 7 timeouts as the run above. All 22 contracts of §2 and §6 are `[VERIFIED]` with the counts in
their tables, and all 28 citations kernel-discharge (13 here, 15 in §6).
Merged once more with main (978fa5518f), whose f31b5d99fb changed
`protocol::script`, the crate measured `TOTAL: 639/1727 proved`: the same
proofs, and ten more rows, all in `protocol/script.rs` (7 unknown, 3
runtime-checked). The language fns' rows and every contract's verdict were
unchanged.

The prover marks every amacs run `not-reproducible`. Its Stop causes line
says "NOT REPRODUCIBLE: a clock, an RSS reading, a solver fault or an
unexplained stop ended at least one obligation, or work the verdicts depend
on, so these counts are not a floor measurement". On amacs the clock is the
solver's: six rows of `execute` end `TIMEOUT (trust-full-verifier,
120000ms)` and one `native-verification-gap` row ends `TIMEOUT (trust-native,
0ms)` (§4), and the Stop causes line counts `wall-backstop=17`, so a more
loaded host can time out more rows. The floor is ratcheted from these runs
anyway, because the counts repeat: 311/935 was measured on e1191a9811, on
964b6b8a8e and in the review's independent rerun with every row's status
identical, and 325/942 was measured by the ratchet run on 3a9c7b2a0c, again
on c5fe22995f (the same code) and on 91b8e26999 (that tree merged with
main), all 942 rows identical each time. S6's 362/1233 repeated on two
merges of its branch with main, and the merged 418/1246 and 639/1717 runs
were `not-reproducible` the same way (`wall-backstop=17`). A later run that
lands below the floor with more timeouts is the clock, not the code; rerun
it on a quieter host before reading it as a regression.

## 2. Proved specs

Each contract is written through `trust_contract_fn!`. It emits the native
`ensures` under `cfg(trust_verify)` and a plain fn on every other compiler,
the foreign cells included. Each clause cites a theorem in a
`verify_island! { clean { … } }` block. That theorem is stated over the kernel
import `trust_import_amacs__<module>__<fn>`, so it is a statement about the
compiled body, not about a restated copy. A measured verdict below means three
things:

- the fn's row is `[VERIFIED]` with every obligation proved;
- each postcondition is `PROVED (clean-kernel-citation, CONSTRUCTIVE (certified;
  validation=kernel-rechecked, …))`;
- the lane printed `citation `<theorem>` kernel-discharges this postcondition
  statement … with a clean certification audit`.

| id | fn | statement (for every input) | theorem | measured verdict | negative control |
| --- | --- | --- | --- | --- | --- |
| `utf8-char-start` | `text::utf8_char_start(byte: u8) -> bool` | `result ⇔ byte < 0x80 ∨ byte ≥ 0xC0`: a char starts at an ASCII byte or a lead byte, never at a continuation byte (0x80..=0xBF) | `utf8_char_start_skips_continuations` | `[VERIFIED]`, 3 of 3 obligations proved | `0xC0` → `0xBF`. The island is rejected: `InTactic { tactic: "simp", … UnsolvedGoals … (UInt8.ofNat 191 ≤ fvar#0) … = … (UInt8.ofNat 192) …`. The fn row is `[VIOLATIONS] (2 obligations: 1 proved, 1 failed)`, with `postcondition FAILED (trust-full-verifier) counterexample: verified_counterexample = true`. |
| `taller-child-covers-both` | `text::taller_child(left: u8, right: u8) -> u8` | `result ≥ left ∧ result ≥ right` | `taller_child_covers_both` | `[VERIFIED]`, 6 of 6 obligations proved | Branches swapped (the body becomes `min`). Both theorems are rejected: `TypeMismatch { expected: "Nat.le (UInt8.toNat fvar#0) (UInt8.toNat (trust_import_amacs__text__taller_child fvar#0 fvar#1)) ∧ …", actual: "… ite UInt8 … x fvar#1 fvar#0 …" }`. The fn row is `[VIOLATIONS] (6 obligations: 2 proved, 2 failed, 2 unknown)`, and both covers-both rows show `counterexample: verified_counterexample = true`. The solver still proves is-a-child, because a `min` is also one of its arguments. |
| `taller-child-is-a-child` | `text::taller_child` | `result = left ∨ result = right` | `taller_child_is_a_child` | (same row) | (same run) |
| `repeat-count-bounds` | `editor::command::repeat_count(value: i64) -> i64` | `1 ≤ result ≤ 10000` | `repeat_count_within_bounds` | `[VERIFIED]`, 8 of 8 obligations proved | The cap branch's `10_000` → `10_001`. Both theorems are rejected: `TypeMismatch { expected: "Nat.le (i64_order_key (BitVec.ofNat 64 1)) (i64_order_key (trust_import_amacs__editor__command__repeat_count fvar#0)) ∧ …", actual: "… ite (BitVec 64) …" }`. The fn row is `[VIOLATIONS] (7 obligations: 5 proved, 1 failed, 1 unknown)`, with one `verified_counterexample = true`. |
| `repeat-count-identity` | `editor::command::repeat_count` | `value < 1 ∨ value > 10000 ∨ result = value` | `repeat_count_keeps_an_in_range_value` | (same row) | (same run) |
| `viewport-capacity-bounds` | `editor::view::visible_line_capacity(visible_lines: usize) -> usize` | `1 ≤ result ≤ 256` | `visible_line_capacity_within_bounds` | `[VERIFIED]`, 8 of 8 obligations proved | The cap branch's `256` → `257`. Both theorems are rejected: `TypeMismatch { expected: "Nat.le (UInt64.toNat (UInt64.ofNat 1)) (UInt64.toNat (trust_import_amacs__editor__view__visible_line_capacity fvar#0)) ∧ …", … }`. The fn row is `[VIOLATIONS] (7 obligations: 5 proved, 1 failed, 1 unknown)`, with one `verified_counterexample = true`. |
| `viewport-capacity-identity` | `editor::view::visible_line_capacity` | `visible_lines < 1 ∨ visible_lines > 256 ∨ result = visible_lines` | `visible_line_capacity_keeps_an_in_range_value` | (same row) | (same run) |
| `projected-column-limit-bounds` | `editor::projection::projected_column_limit(requested_columns: usize) -> usize` | `4 ≤ result ≤ 4096` | `projected_column_limit_within_bounds` | `[VERIFIED] (8 obligations: 8 proved …)` on 3a9c7b2a0c | The cap branch's `4_096` → `4_097` (run from 3a9c7b2a0c). Both theorems are rejected: `TypeMismatch { expected: "Nat.le (UInt64.toNat (UInt64.ofNat 4)) (UInt64.toNat (trust_import_amacs__editor__projection__projected_column_limit fvar#0)) ∧ …", actual: "(fun (x : Decidable (fvar#0 < UInt64.ofNat 4)) => …" }`. The fn row is `[VIOLATIONS] (7 obligations: 5 proved, 0 runtime-checked, 1 failed, 1 unknown …)`, with one `verified_counterexample = true`. |
| `projected-column-limit-identity` | `editor::projection::projected_column_limit` | `requested_columns < 4 ∨ requested_columns > 4096 ∨ result = requested_columns` | `projected_column_limit_keeps_an_in_range_value` | (same row) | (same run) |
| `file-uri-unescaped-set` | `protocol::uri::file_uri_keeps(byte: u8) -> bool` | `result ⇔ byte ∈ [0-9A-Za-z] ∪ {/ - _ . ~ :}`; every other byte, space and `%` included, is percent-encoded by `path_to_file_uri` | `file_uri_keeps_exactly_the_unescaped_set` | `[VERIFIED]`, 10 of 10 obligations proved | `b'~'` → `b' '`. The island is rejected: `InTactic { tactic: "simp", … UnsolvedGoals …`. The fn row is `[VIOLATIONS] (10 obligations: 7 proved, 1 failed, 2 unknown)`, with one `verified_counterexample = true`. |
| `h6-fits` | `protocol::anchor::fits_h6(value: u32) -> bool` | `result ⇔ value ≤ 0xff_ffff` (24 bits); `Hash6::from_value` admits exactly these values | `fits_h6_is_24_bits` | `[VERIFIED]`, 2 of 2 obligations proved | Body `value < H6_MAX`. The island is rejected (`simp` leaves `decide (value < 16777215) = decide (value <= 16777215)` unsolved), the citation fails, and one postcondition is `FAILED … verified_counterexample = true`. |
| `h6-hex-digit` | `protocol::anchor::is_hex_digit(byte: u8) -> bool` | `result ⇔ byte ∈ 0x30..=0x39 ∪ 0x61..=0x66 ∪ 0x41..=0x46` (0-9, a-f, A-F); `Hash6::from_str` reads 6 such bytes and nothing else | `is_hex_digit_is_the_class` | `[VERIFIED]`, 12 of 12 obligations proved | Body `byte < b'f'` for the lower-case range. The island is rejected (`simp` leaves the goal unsolved), the citation fails, and one postcondition is `FAILED … verified_counterexample = true`. |
| `for-word-byte-set` | `protocol::edit::for_word_byte(byte: u8) -> bool` | `result ⇔ byte ∈ [0-9A-Za-z] ∪ {. _ -}`; neither space nor `=` is in the set, so a `for=` word stays one `key=value` field | `for_word_byte_is_exactly_the_word_set` | `[VERIFIED]`, 10 of 10 obligations proved | `b'-'` → `b'='`. The island is rejected: `InTactic { tactic: "simp", … UnsolvedGoals …`. The fn row is `[VIOLATIONS] (10 obligations: 6 proved, 2 failed, 2 unknown)`, with two `verified_counterexample = true`. |

On 3a9c7b2a0c the seven contracts other than the two `h6-*` ones were
`[VERIFIED]` again, with the counts in the table, and all eleven of their
citations kernel-discharged. On the merged tree (S6 with origin/main
ec4864b065) all nine contracts are `[VERIFIED]` with the counts in the table,
all thirteen citations kernel-discharge, and no island is rejected.

Where each spec is used:

- `utf8_char_start` decides `TextRope::is_char_boundary` inside a leaf.
- `taller_child` is the height of every rope branch.
- `repeat_count` is `PrefixArgument::count`, which every counting command uses.
  The kill ring's `count.saturating_sub(1)` relies on its `result ≥ 1`.
- `visible_line_capacity` is the viewport capacity that
  `reconcile_viewport_with` and `ensure_primary_visible_with` install.
- `projected_column_limit` is the column span every projected line is cut to
  in `project_viewport_with`, so it bounds what each frame paints.
- `file_uri_keeps` is the escape decision in `path_to_file_uri`.
- `for_word_byte` is the byte test in `is_for_word`.
- `fits_h6` is `Hash6::from_value`'s admission.
- `is_hex_digit` is the byte test in `Hash6::from_str`, which reads 6 such
  bytes and nothing else.

How the theorems are written:

- The two `u8` set specs close with `simp only [<import>]`, because the clause
  and the compiled `bool` tree elaborate to the same `decide` disjunction.
- The selects are term-mode `@Decidable.casesOn` proofs over the comparison
  the body makes, with an explicit motive.
- Trust reads an `i64` as a `BitVec 64`. The `repeat_count` proofs order it
  through `i64_order_key v = v.toNat xor 2^63`, which is the key the elaborated
  clause compares.
- Trust reads a `usize` as a `UInt64` on this target, so the
  `visible_line_capacity` proofs are over `UInt64`.

Negative control procedure, run on 2026-10-20 from e1191a9811 for the first
six contracts and from 3a9c7b2a0c for `projected_column_limit`:

1. Mutate one constant or one comparison in one body. Leave the clause and the
   theorem alone.
2. Run `targo --unverified ship trust run -p amacs`.
3. Restore the file with `git checkout -- <file>` and confirm that `git diff`
   is empty.

Each run mutated one contract. The first six runs ended the same way:

- `rc=2`;
- `BUDGET LAPSED: amacs: 298–300 proved, floor 310`;
- the crate check ended with `could not compile `amacs` (lib)`: 10 previous
  errors for one rejected theorem plus nine refused citations, and 11 where
  both theorems of a two-clause contract were rejected.

The seventh (`projected_column_limit`) ended `rc=2`, `BUDGET LAPSED: amacs:
311 proved, floor 325; 17 obligation(s) ran out of time`, and `could not
compile `amacs` (lib) due to 13 previous errors`: two rejected theorems plus
the crate's eleven citations.

A rejected island taints the crate's Clean environment. So in each run every
citation (nine, then eleven) reported `cannot be validated because an
earlier Clean island was rejected and the crate-local environment is
tainted`. Only the mutated fn got a counterexample; the solver lane proved
the other contracts' clauses instead.

### The `line:h6` anchor (`protocol::anchor`, slice S6)

An anchor is `<n>:<h6>`: a line number and the first 24 bits of FNV-1a/64
(`aterm_hash::fnv1a_64`) over the line's bytes, written as 6 hex digits
(design §7b-§7c). An agent copies anchors from a read into an edit script, so
reading one back must accept exactly what a read prints. The two predicates
that reading goes through are contracted:

- `fits_h6` is `Hash6::from_value`'s admission;
- `is_hex_digit` is the byte test in `Hash6::from_str`, which reads 6 such
  bytes and nothing else.

The tests in `protocol::anchor::tests` stay: they check the composition (an
anchor token parses back to what a read printed, and the shapes a read never
prints are refused), which these two contracts do not state.

The theorems close with `simp only [<import>]`, like the `u8` set specs
above. Their negative controls were run on the S6 branch before it was merged
with this file's other contracts, by
`targo --unverified ship trust run -p amacs --obligations`: the crate printed
`TOTAL: 360/3594 proved` with both contracts in, `358/3594` with the first
mutant and `357/3592` with the second (the totals of the `&dyn BaseMap` tree,
§1). Each file was restored byte for byte.

### Tests kept beside the specs

These tests check that each contract gives the same answer as the std form it
replaced. They check every byte, every `u8` pair, or the clamp's edges. They are
not subsumed by the specs, because no Clean theorem can state the old forms:
`u8::is_ascii_alphanumeric`, `u8::max`, `str::is_char_boundary` and
`Ord::clamp` are calls, and E6 refuses calls.

- `text::tests::contracted_scalar_core_matches_the_std_forms_it_replaced`
- `editor::command::tests::repeat_count_is_the_clamp_it_replaced`
- `editor::view::tests::visible_line_capacity_is_the_clamp_it_replaced`
- `editor::projection::tests::projected_column_limit_is_the_clamp_it_replaced`
- `protocol::uri::tests::file_uri_keeps_the_set_it_replaced`
- `protocol::edit::tests::for_word_byte_is_the_set_it_replaced`

## 3. Hazards made provable

Each change below leaves the result the same for every reachable state.
Most of them move an obligation the lane could not prove onto an operation
that cannot panic, or make a guard the solver can carry. Four of them route
existing code through a contract above.

| site | was | now | why the result is unchanged |
| --- | --- | --- | --- |
| `text.rs` `TextRope::from` chunk end (`start + relative`) | `+` | `saturating_add` | `start + relative ≤ text.len()` |
| `text.rs` `branch` height | `max(..).saturating_add(1)` | `taller_child(..).saturating_add(1)` | `taller_child` is `u8::max` on every pair (test above) |
| `text.rs` `is_boundary` leaf | `str::is_char_boundary` | `as_bytes().get(offset).map_or(offset == len, utf8_char_start)` | the same answer for every offset (test above) |
| `editor/projection.rs` `byte_of_line`, `line_starts` | `index + 1` | `saturating_add(1)` | `index < text.len()` |
| `editor/projection.rs` `EditorLines::total` | `count() + 1` | `saturating_add(1)` | a count of bytes |
| `editor/search.rs` match range | `start + offset .. + len` | `saturating_add` | inside the text |
| `editor/kill.rs` `C-u n C-y` index | `count - 1` | `saturating_sub(1)` | `count ≥ 1` (`repeat-count-bounds`) |
| `editor/kill.rs` `M-y` index | `ring_index + 1` | `saturating_add(1)` | `ring_index < ring.len()` |
| `editor/kill.rs`, `editor/history.rs`, `store.rs` growth | `insert as isize - removed as isize` | `saturating_sub` | two non-negative `isize`s |
| `editor/minibuffer.rs` landed line | `count() + 1` | `saturating_add(1)` | a count of bytes |
| `editor/command.rs` command ranking | `first + needle.len() - 1` | `saturating_add`/`saturating_sub` | `needle` is non-empty and `first ≤ index` |
| `protocol/edit.rs` `keyed` | `values[slot]` over `fields.iter().zip(keys).enumerate()` | write through `values.iter_mut().zip(fields.iter().zip(keys))` | `slot < N`: the zip has `N` keys |
| `editor/view.rs` `pointer_select` (extend) | `selections.len() - 1`, `selections[primary]` | `saturating_sub(1)`, `get_mut(primary)` | guarded by `!selections.is_empty()` |
| `editor/view.rs` `primary_selection` | `selections[i]` | `selections.get(i).unwrap_or(&caret(0))` | **real**: `selections` is `pub`, so a host can leave it empty, and this panicked. It now answers a caret at 0; `view::tests::primary_selection_of_an_emptied_view_is_a_caret_at_zero` pins it. |
| `editor/minibuffer.rs` `command_completion_action` | `candidates.is_empty()` guard, then `% candidates.len()`, `candidates.len() - 1`, `candidates[chosen]` | guard on a local `count == 0`, then `% count`, `last = count - 1`, `candidates.get(selected.min(last))` | the same guard and arithmetic. Trust does not carry `Vec::is_empty` into `len()`, so the remainder, the subtraction and their hardened twins were unknown; a test of the local discharges them, and the index is gone. |
| `protocol/uri.rs` `path_to_file_uri` | `HEX[usize::from(byte >> 4)]`, `HEX[usize::from(byte & 0x0f)]` in the escape branch | the same two indexes in `push_percent_escape(uri, byte)` | the same bytes. In the loop the solver withheld the bound: "query depends on the unmodeled return of same-crate `amacs::protocol::uri::file_uri_keeps`". In their own fn both indexes and their hardened twins are proved below 16. |
| `editor/projection.rs` `project_viewport_with` column limit | `requested_columns.clamp(4, 4_096)` | `projected_column_limit(requested_columns)` | the contract `projected-column-limit-*` above; `projection::tests::projected_column_limit_is_the_clamp_it_replaced` checks it against the clamp. |

## 4. Rows that stay unproved, and why

On 3a9c7b2a0c the lane left 617 of 942 obligations unproved: 13 failed, 7
timeouts, 317 unknown and 280 runtime-checked. Every row is listed below by
status and kind, with its site where the rows are few. The merged tree leaves
828 of 1246 unproved: the same 13 failed and 7 timeout rows in the same fns
(some have moved down a few lines, `AwaitKey`'s derive to `edit.rs:296`), plus the unknown and runtime-checked rows of S6's edit-script planner
(`protocol::{anchor,read,script}`), which are not itemised here. None of them
was found to be a reachable hazard in amacs's own code: each is a fact the
solver cannot see or an obligation on code Trust has no model for. With the
language table merged the crate leaves 1078 of 1717 unproved; the language
rows add no failed and no timed-out row, and §6 lists them.

| status | rows | kind and site | why it stays |
| --- | --- | --- | --- |
| failed | 3 | `unbounded_allocation`: `Vec::with_capacity` in `TextRope::from` (`text.rs:73`), `String::with_capacity` in `TextRope::to_flat_string` (`text.rs:122`), `Vec::with_capacity` in `line_starts` (`editor/projection.rs:86`) | Each capacity is sized by the document, and a document has no static size bound. `#[requires(n < BOUND)]` would only move the obligation onto every caller. |
| failed | 3 + 4 | `expect` panic-freedom (3 `assertion`, 4 `unsupported_mir`): `Keymap::emacs` (`editor/keymap.rs:220`, `224`), `handle_chord` (`editor/workspace.rs:208`, `214`), `reduce_key` (`input.rs:367`), `DocumentStore::open` (`store.rs:179`), `commit_detach_batch` (`store.rs:591`) | These are `expect`s on facts the lane cannot see: static chord literals, built-in keymap tables, `NonZeroU64::new(raw)` after `raw = next_id.max(1)`, and a second pass over keys the first pass validated. Trust does not model `expect`'s panic-freedom ("match / `if let` to prove it"). Rewriting them would touch reducer files that parallel slices are editing, and would change no reachable behaviour. |
| failed | 2 | `instance-hypothesis:discharge` at `repeat_line_motion`'s `step(at)` (`editor/view.rs:475`), reached from `execute` | A call through an `impl Fn(usize) -> usize` parameter. Trust cannot discharge the instance at that site (§5 quotes it). |
| failed | 1 | `unreachable` in the derived `Ord::cmp` of `AwaitKey` (`protocol/edit.rs:280`) | The derive's discriminant comparison; the code is generated, not written. |
| timeout | 7 | in `EditorWorkspace::execute`: the hardened twins of `-delta` (`workspace.rs:408`, `delta ≥ 1`) and of `TAB_STOP - column % TAB_STOP` and `column % TAB_STOP` (`workspace.rs:523`), `Vec::with_capacity(order.len())` (`workspace.rs:509`), an `unreachable` (`workspace.rs:282`), the macro recursion (`workspace.rs:614`) and one `native-verification-gap` | `execute` is the reducer's one large match; six of these end `TIMEOUT (trust-full-verifier, 120000ms)` and the gap row `TIMEOUT (trust-native, 0ms)` on this host. |
| unknown | 228 | `unsupported_mir`: bodies that allocate ("allocation(s) and/or allocation-owning drop(s) in this body rely on `GlobalAlloc::{alloc,dealloc}` never panicking") | Heap code (`String`, `Vec`, `Arc`): Gap 1. |
| unknown | 25 + 4 + 1 | `unsupported_mir`: `str-range-char-boundary-unmodeled` (`&text[a..b]`), `slice-range-bounds-unresolved`, and `string-char-boundary-unmodeled` (`String::truncate` in the minibuffer) | Char boundaries are not modeled ("reported Unknown, never silently verified"); every such endpoint comes from a boundary walk or a `line_start`/`line_end` call the solver cannot see into. The four slice rows index with a cloned `Range` (`history.rs:186`, `kill.rs:60`, `projection.rs:354`, `:617`) whose "bound does not resolve to a single value at the call". |
| unknown | 8 | `unsupported_mir`: termination of the rope's recursions `split`, `is_boundary`, `chunk_count`, `append_to` (`text.rs`), two rows each ("this self call binds no `decreases`") | Each recursion descends into a child `Node`; a measure over the tree height is Gap 1 (no enum carrier). |
| unknown | 6 | `unsupported_mir`: `UnboundedAllocation` on `collect`/`extend` ("count-not-derivable", "bounded-source-destination-may-overflow") | The same document-sized allocation as the failed `with_capacity` rows. |
| unknown | 3 + 1 | `unsupported_mir`: `repeat_line_motion`'s open generic dispatch (`[trust-instance-hypothesis]`), its drop glue for the generic parameter and its instance inventory; and the instance discharge of `aterm_grapheme::word::word_break_before::<fn(char) -> bool {is_word_char}>` in `next_word_boundary` | Calls through a generic `Fn`; the obligation moves to each instance, which fails or is unknown there. |
| unknown | 4 | `unsupported_mir`: `str-repeat-capacity-overflow` (the indent's `" ".repeat(..)`), one `termination` tag disagreement, one `fresh-vc-rekey-integrity` | Library internals and verifier bookkeeping, not amacs code. |
| unknown | 21 | `assertion`: `native-verification-gap`, with no source site | The native verifier's own gap rows. |
| unknown | 7 | `hardened_panic_boundary`: the twins of the seven failed `expect` rows above | As for those rows. |
| unknown | 4 | `hardened_error_discard`: `text.get(from..).unwrap_or_default()` (`editor/view.rs:382`), two `.unwrap_or_default()` in `handle_chord` (`workspace.rs:236`, `258`), `text.parse::<u32>().ok()` (`protocol/edit.rs:361`) | Deliberate defaults: an empty tail, no prefix-argument label, and a non-number line field that `parse_line` then reports as `BadLine`. |
| unknown | 3 | `non_termination`: the keyboard-macro cycle `play_macro` → `execute` (`editor/macros.rs:47`, `55`) and `clamp_to_char_boundary`'s loop (`editor/projection.rs:30`: "loop measure `position` may not decrease") | No loop or recursion measures (§5). The loop decreases `position` by one on each turn and stops at 0. |
| unknown | 2 | `unbounded_allocation`: `Vec::with_capacity(line_limit.min(..))` in `project_viewport_with` (`editor/projection.rs:313`) and `String::with_capacity(a.len() + b.len())` in `concat` (`text.rs:248`) | Bounded in fact (`line_limit ≤ 512` from its clamp; `concat` joins two leaves only under `CHUNK_MERGE_MAX`), but the solver does not derive the bound through the `clamp`, `min` and `len` calls. |
| unknown | 1 | `hardened_utf8_reject`: `String::from_utf8(decoded)` in `percent_decode` (`protocol/uri.rs:117`) | Deliberate: a URI that decodes to bytes that are not UTF-8 is refused with `UriError::InvalidEncoding`. |
| runtime-checked | 271 | `assertion`: 203 `[trust-absent-callee-assumption] call to absent callee …` (`dyn`, `alloc`, `core`, `aterm_*`), 41 whole-body "panic freedom: no assertion, `unreachable!`, or panic is reachable", 5 drop glue, 16 bundled-std members (`Sum::sum`, `Enumerate`, `Layout::array`, `has_physical_root`), 4 extern `format!` dispatches, `Ord::clamp: min > max` at `window_start.clamp(start, end)` (`editor/projection.rs:608`), and the `panic_fmt` of the `expect` in `commit_detach_batch` | Kept as runtime checks, not proofs: the callee's body is not in the bundle. |
| runtime-checked | 8 + 1 | in `execute`: `TAB_STOP - column % TAB_STOP` (`workspace.rs:523`), seven `arithmetic_overflow_sub` inside `vec!`/`format!` expansions (`alloc/src/macros.rs:59`), and `-delta` (`workspace.rs:408`, `negation_overflow`) | Cannot overflow (`column % TAB_STOP < TAB_STOP`, `delta ≥ 1`), but `execute`'s queries time out (above), so the lane keeps them as runtime checks. |

The reviewer round (3bf329aea4) discharged the nine unknown rows that the
earlier version of this table lumped under "assertions and hardened
boundaries": the two `HEX` indexes at `protocol/uri.rs:161`/`162`, the
remainder and the subtraction at `editor/minibuffer.rs:207`/`215`, each
with its hardened twin, and the index at `editor/minibuffer.rs:221` (§3).
The review counted 13; the last lane log before this round (311/935) holds
these nine.

## 5. Properties that cannot be proved today

Each row is a property of amacs and the refusal that blocks it, from the same
prover. Where the text comes from:

- The scalar shapes were probed in a scratch crate (`targo trust check --lib
  --allow-l0-gaps`), each with the body amacs has or would need. The quoted
  text is that check's E6 admission verdict, which does not depend on the
  theorem.
- Rows marked *(lane)* quote amacs's own lane runs on e1191a9811, 964b6b8a8e
  and 3a9c7b2a0c.
- Rows marked *(not attempted)* were not stated as clauses here. They are
  blocked by the Gap 1 refusal that the `Node` row measures, and by the
  recipe's record for `&str`, `String`, `Vec` and `Option`
  (`docs/PLAN-tests-to-specs-2026-10-15.md`,
  "What blocks the rest").

| property | where | shape | refusal (quoted) |
| --- | --- | --- | --- |
| tab stop: `TAB_STOP - column % TAB_STOP ∈ 1..=TAB_STOP` | `editor/workspace.rs` `execute` (indent); since S5, `editor/edit.rs` `tab_stop_columns` (§7) | `%` on `u64`/`usize` | FACET gate: "NoPanic (not established: reachable assert; structural NoPanic requires an assert-free body …); the construct outside the E6 fragment is the division `%` (`BinOp::Rem`) on `u64`". *(lane)* In the crate itself the two rows at `workspace.rs:523` time out. S5's lane run of `tab_stop_columns` refuses the same shape (§7). |
| hex digit value `≤ 15` | `protocol/uri.rs` `nibble` (percent decoding) | `nibble` returns `Option<u8>` (Gap 1); its `u8` core, a select whose leaves are `wrapping_sub` arithmetic, was probed | SHAPE gate: "all four E6 facets hold, but no E6 reader reads its body …; the construct outside the E6 fragment is bb0 has no immediate post-dominator" |
| prefix argument digits: `value * 10 + digit ≥ value` for `value ≥ 0` | `editor/command.rs` `after_digit` | `saturating_mul` + `saturating_add` | FACET gate: "call to `core::num::<impl i64>::saturating_mul`, which is not itself structurally certified (facets are closed over callees by exact def-path identity)" |
| `C-u` multiplies by four, monotone | `editor/command.rs` `after_universal` | `saturating_mul(4)` | the same FACET refusal for `core::num::<impl i64>::saturating_mul` |
| AVL balance factor `\|h(l) - h(r)\| ≤ 1` after `concat` | `text.rs` `balance` | `i16::from(l) - i16::from(r)` | FACET gate: "call to `<i16 as core::convert::From<u8>>::from`, which is not itself structurally certified" |
| the same left-heavy decision without the `i16` widening (`left > right + 1`) | `text.rs` `balance` | `wrapping_add` inside a comparison | SHAPE gate: "the construct outside the E6 fragment is the call to `@trust-rustc-total-primitive-method::core::num::<impl u8>::wrapping_add`" |
| leaf merge stays under the chunk cap | `text.rs` `concat` | `left.saturating_add(right) <= CAP` | FACET gate: "call to `core::num::<impl usize>::saturating_add`, which is not itself structurally certified" |
| the `line:h6` anchor hash step (FNV-1a; probed before S6 wrote the code, which now calls `aterm_hash::fnv1a_64` in another crate) | `protocol/anchor.rs` | `(h ^ byte).wrapping_mul(P)` | SHAPE gate: "the construct outside the E6 fragment is the bit operation `^` (`BinOp::BitXor`) on `u64`" |
| an h6 is the top 24 bits of the hash (`h6_of_hash(hash) = hash >> 40`): `ensures result == (hash / 1099511627776)` | `protocol/anchor.rs` `h6_of_hash` | a division in the clause | *(lane)* "contract predicate … (hash / 1099511627776) uses arithmetic whose domain is not preserved" |
| the same h6 as a bound that does not divide: `ensures result <= 16777215 by h6_of_hash_fits` | `protocol/anchor.rs` `h6_of_hash` | a shift body | *(lane, `--obligations`)* `Clean island declaration rejected: AutoBoundHeadOverApplied { name: "trust_import_amacs__protocol__anchor__h6_of_hash", …, remaining_args: 1 }`: the shift body gets no import, because E6 admits neither a shift nor a division (Trust's `crates/trust-types/src/e6_expr.rs`). The rejected island tainted the crate's Clean environment, so the other citations "cannot be validated" and were withheld (361/1233 instead of 362/1233); the attempt was removed. `h6_of_hash` stays a plain fn, `[VERIFIED]` 2/2 on safety alone, and `anchor::tests::h6_matches_fnv1a_64_prefix_and_is_stable` pins the property against `fnv1a_64_to_hex16`. |
| a `count`-line motion stays on lines `0..=last` and stops at the first or last line | `editor/view.rs` `repeat_line_motion`, `previous_line_position`, `next_line_position` | no line arithmetic to contract: the motion walks `count` steps of an `impl Fn(usize) -> usize` step, and each step finds the neighbouring line with `str::rfind`/`str::find` over the text. The arithmetic form the plan names (`line.saturating_sub(count)`, `line.saturating_add(count).min(last)`) is not in the code, and would meet the `saturating_*` FACET refusal above. | *(lane)* the two failed rows at `view.rs:475`: "unsupported MIR `instance-hypothesis:discharge`: instance discharge: site `<impl Fn(usize) -> usize as core::ops::function::Fn<(usize,)>>::call` of `amacs::editor::view::repeat_line_motion::<{closure@crates/amacs/src/editor/workspace.rs:369:78: 369:82}>` (named at crates/amacs/src/editor/workspace.rs:369) resolves to `<{closure@…} as core::ops::function::Fn<(usize,)>>::call`", and in the step "call to absent callee `core::str::<impl str>::rfind::<char>` (body not in the lowered bundle) may panic" |
| vertical motion's goal column: the landing byte is the last cluster boundary at or before `desired_column` cells on the target line, never inside a cluster | `editor/projection.rs` `byte_at_editor_display_column` | a grapheme walk over `&text[start..end]` | *(lane)* "unsupported MIR `str-range-char-boundary-unmodeled`: str range index panics when an endpoint is not a UTF-8 char boundary; char boundaries are not modeled …" (`projection.rs:44`) and "call to absent callee `<str as aterm_grapheme::grapheme_iter::GraphemeClusters>::grapheme_indices` (body not in the lowered bundle) may panic"; and Gap 1 for `&str`. The scalar column clamp next to it, the projection's column limit, is proved (`projected-column-limit-*`). |
| rope height of a `Node` | `text.rs` `height` | `match` on the `Node` enum | FACET gate: "local `node` has type Adt { name: …Node … } … outside the v1 scalar fragment" (Gap 1: no enum carrier) |
| rope laws: `to_flat_string(replace(r, a..b, s)) = flat(r)[..a] ++ s ++ flat(r)[b..]`, `is_char_boundary` over the whole rope, balance after every edit | `text.rs` | `Arc<Node>`, `&str`, `String`, recursion | *(not attempted)* Gap 1: non-scalar carriers get no E6 import. |
| reducer laws: undo restores the text, a selection stays on a char boundary, `C-g` always cancels | `editor/*.rs` | `&mut self` over `Vec<Selection>`, `String`, `Option` | *(not attempted)* Gap 1, and `&mut` is outside E6. |
| keyboard-macro playback terminates (`play_macro` refuses to nest through `playing_macro`) | `editor/macros.rs`, `editor/workspace.rs` `execute` | a recursion cycle `execute` → `play_macro` → `execute` that a runtime flag breaks | *(lane)* "non-termination: recursion measure `none (`workspace::EditorWorkspace::execute` closes a recursion cycle; every member needs a `decreases` measure)` may not decrease": a flag in `&mut self` is not a measure Trust can read |
| `usize` literal facts in Clean (`decide` over `USize`) | any `usize` spec | `USize.ofNat` literals | "decide: failed to reduce `Decidable (Nat.le (USize.toNat (USize.ofNat 4)) (USize.toNat (USize.ofNat 4096)))` to `isTrue` or `isFalse`". Worked around: on this target the import uses `UInt64`, so the `visible_line_capacity` theorems are stated over `UInt64`. |

## 6. The language table (slice L1)

Measured on 2026-10-20 with the same prover as the sections above. The
contracts use the crate's `trust_contract_fn!` and `verify_island!` from
lib.rs, and each theorem is stated over
`trust_import_amacs__language__<module>__<fn>`.

### The lane, before and after

`targo --unverified ship trust run -p amacs` runs `targo trust check -p amacs
--lib --allow-l0-gaps`.

| tree | proved / total |
| --- | --- |
| `origin/main` 1ac166b26c, before L1 | 269 / 929 |
| `amacs/L1` bd1835894b (origin/main 82aee5d2b1 merged), the first six contracts | 417 / 1317 |
| `amacs/L1` 72962054af, the scalars review asked for contracted | 486 / 1400 |
| `amacs/L1` d5e0856491, `tab_stop` called directly at its divisors | 488 / 1400 |
| `amacs/L1` 01c99738e0, the tab advance out of `columns_through`'s loop | 490 / 1400 |
| `amacs/L1` f7e865eee3, merged with origin/main 90f10d9fc6 (TR2 and S6, §1) | 639 / 1717 |

### Proved specs

A measured verdict means three things, all read off the lane's log:

- the fn's row is `[VERIFIED]` with every obligation proved;
- every postcondition row is `PROVED (clean-kernel-citation, CONSTRUCTIVE
  (certified; validation=kernel-rechecked, …))`;
- the lane printed "citation `<theorem>` kernel-discharges this postcondition
  statement … with a clean certification audit".

The verdicts are the 01c99738e0 run's. The merged tree's run (f7e865eee3)
printed the same verdict and the same counts for all thirteen fns, and
`decimal_push` and `IndentStyle::next_stop` stayed `[VERIFIED]` too.

| id | fn | statement (for every input) | theorem | measured verdict | negative control |
| --- | --- | --- | --- | --- | --- |
| `line-space-set` | `language::line_space(byte: u8) -> bool` | `result ⇔ byte ∈ {0x20, 0x09, 0x0D}`: the blanks inside a line every lexer skips, a CRLF's `\r` among them | `line_space_is_exactly_the_blanks` | Certified: `[VERIFIED]`, 4 of 4 obligations proved | refused, round 1 |
| `tab-stop-nonzero` | `language::tab_stop(width: usize) -> usize` | `result ≥ 1`, so a column can always be divided by the stop | `tab_stop_is_never_zero` | Certified: `[VERIFIED]`, 6 of 6 obligations proved | refused, round 2 |
| `tab-stop-identity` | `language::tab_stop` | `width < 1 ∨ result = width`: a width of 1 or more is the stop | `tab_stop_keeps_a_nonzero_width` | (the same row) | refused, round 2 |
| `indent-cap-bound` | `language::indent_columns_cap(columns: usize) -> usize` | `result ≤ 1024` (`MAX_INDENT_COLUMNS`), the most indentation `IndentStyle::render` writes | `indent_columns_cap_is_at_most_the_cap` | Certified: `[VERIFIED]`, 6 of 6 obligations proved | refused, round 2 |
| `indent-cap-identity` | `language::indent_columns_cap` | `columns > 1024 ∨ result = columns`: indentation within the cap is kept | `indent_columns_cap_keeps_a_value_within_it` | (the same row) | refused, round 2 |
| `decimal-room-bound` | `language::decimal_room(value: u32) -> bool` | `result ⇔ value ≤ 429,496,728`, the largest value whose every next digit fits in a `u32`; the island's two closed facts are the bound's arithmetic, `429496728 · 10 + 9 ≤ 2³² − 1 < 429496729 · 10 + 9` | `decimal_room_is_the_bound`, `decimal_room_bound_takes_any_digit`, `decimal_room_bound_is_the_largest` | Certified: `[VERIFIED]`, 2 of 2 obligations proved | refused, round 2 |
| `decimal-append-step` | `language::decimal_append(value: u32, digit: u32) -> u32` | `result = value · 10 + digit` in `UInt32` arithmetic | `decimal_append_is_the_step` | Certified: `[VERIFIED]`, 2 of 2 obligations proved | refused, round 2 |
| `rust-ident-byte-set` | `language::rust::ident_byte(byte: u8) -> bool` | `result ⇔ byte ∈ [0-9A-Za-z_] ∪ [0x80, 0xFF]`; every byte of a non-ASCII character is in, so a span that ends where the set ends never splits a character | `ident_byte_is_exactly_the_word_set` | Certified: `[VERIFIED]`, 24 of 24 obligations proved | refused, round 1 |
| `utf8-lead-width` | `language::rust::utf8_width(byte: u8) -> u8` | `result` is 1 below 0x80, 0 for a continuation byte (0x80 to 0xBF), 2 below 0xE0, 3 below 0xF0 and 4 above (RFC 3629 §3) | `utf8_width_is_the_table` | Certified: `[VERIFIED]`, 2 of 2 obligations proved | refused, round 2 |
| `toml-bare-key-byte-set` | `language::toml::bare_key_byte(byte: u8) -> bool` | `result ⇔ byte ∈ [0-9A-Za-z_-]`, TOML's bare key set | `bare_key_byte_is_exactly_the_key_set` | Certified: `[VERIFIED]`, 24 of 24 obligations proved | refused, round 1 |
| `toml-value-byte-set` | `language::toml::value_byte(byte: u8) -> bool` | `result ⇔ byte ∈ [0-9A-Za-z_+.:-]`: the bytes of a bare value (a number, a date, a time, `true`) | `value_byte_is_exactly_the_value_set` | Certified: `[VERIFIED]`, 48 of 48 obligations proved | refused, round 2 |
| `shell-metachar-set` | `language::shell::metachar(byte: u8) -> bool` | `result ⇔ byte ∈ {; & \| ( ) < >}`, the operators that end a word, after which `#` starts a comment | `metachar_is_exactly_the_operators` | Certified: `[VERIFIED]`, 8 of 8 obligations proved | refused, round 1 |
| `heredoc-hash-step` | `language::shell::delimiter_hash_step(hash: u32, byte: u32) -> u32` | `result = hash · 16777619 + byte` in `UInt32` arithmetic | `delimiter_hash_step_is_the_polynomial` | Certified: `[VERIFIED]`, 2 of 2 obligations proved | refused, round 2 |
| `commit-summary-limit` | `language::git::summary_too_long(chars: usize, limit: usize) -> bool` | `result ⇔ chars > limit`: a 51-character summary is flagged at the default 50, a 50-character one is not | `summary_too_long_is_past_the_limit` | Certified: `[VERIFIED]`, 2 of 2 obligations proved | refused, round 1 |
| `diff-hunk-open` | `language::diff::hunk_open(old: u32, new: u32) -> bool` | `result ⇔ old > 0 ∨ new > 0`: a hunk ends only when both of its sides are used up | `hunk_open_until_both_sides_end` | Certified: `[VERIFIED]`, 3 of 3 obligations proved | refused, round 1 |

Where each spec is used:

- `line_space` is every lexer's blank, and the cursor's.
- `tab_stop` is the divisor of `IndentStyle::render` and
  `IndentStyle::next_stop`. Both call it directly, in straight-line code.
  Through the uncontracted method that used to wrap it, the lane refuted the
  remainder (a probe crate got a counterexample with the divisor 0), and
  with the call inside `columns_through`'s loop it left the remainder
  unknown ("depends on the unmodeled return of same-crate `tab_stop`"). As
  called now, the division and both remainders are proved.
- `indent_columns_cap` caps what `IndentStyle::render` repeats.
- `decimal_room` and `decimal_append` are `decimal_push`, which reads the
  diff lexer's hunk counts and Markdown's list numbers: below the bound it
  appends the digit, and past it the value stays `u32::MAX`, so a long run of
  digits saturates and never wraps. `decimal_push` itself is the glue of the
  two contracts: its own row is `[VERIFIED]`, 1 of 1 obligation proved (L0
  safety), with no postcondition.
- `ident_byte` decides where a Rust word, number suffix, lifetime or
  attribute name ends.
- `utf8_width` decides whether `'…'` after a quote is a character literal or
  a lifetime: the byte after the quote leads a character of that many bytes.
- `bare_key_byte` decides where a TOML bare key ends, and `value_byte` where
  a bare value does.
- `metachar` ends a shell word and makes the next `#` a comment.
- `delimiter_hash_step` hashes a here-document's word and each body line, so
  the lexer can tell the line that ends the document in the 4 bytes a
  `LexState` has room for. A body line whose hash collides with the word's
  ends the document early; the chance is about one in 2³² per line.
- `summary_too_long` is the commit summary rule of `Language::diagnostics`.
- `hunk_open` decides when the diff lexer leaves a hunk.

Each theorem is one of three proof shapes:

- `simp only [<import>, <spec def>]`, for the byte sets, the `utf8_width`
  table and the two wrapping chains. The clause and the compiled body
  elaborate to the same `decide` tree, `ite` chain, or `UInt32` `*` and `+`,
  which are Clean's wrapping operations.
- A term-mode case split, `@Decidable.casesOn (LT.lt x k) …
  (instDecidableUInt64Lt x k)`, for the two clamps: one arm per branch of
  the select, the pattern of TR2's `projected_column_limit` proofs.
- `decide`, for the two closed `Nat` facts about the decimal bound.

On this target Trust reads a `usize` as a `UInt64`, so the `usize`
theorems are stated over `UInt64`. The bool clauses nest `||` to the right,
the way Clean elaborates them, and the bodies are written with the same
nesting.

`language::tests::contracted_scalar_core_matches_the_std_forms` checks each
contracted fn against the std form it stands for, a form no Clean theorem can
state because it is a call, which E6 refuses:

- the byte sets, against `u8::is_ascii_alphanumeric` and a byte string's
  `contains`, over all 256 bytes;
- `utf8_width`, against `char::len_utf8` for the lead byte of every `char`,
  and 0 for every continuation byte;
- `tab_stop`, against `usize::max(1)`, and `indent_columns_cap`, against
  `usize::min(1024)`, at their edges;
- `decimal_push`, against `u64` arithmetic: for 0 to 30, the values around
  the bound and the two largest, with every digit, the result is the exact
  `value · 10 + digit` while that fits, and `u32::MAX` after. That no-wrap fact is the one law of the pair that is
  tested and not proved (see the refused forms).

`summary_too_long` and `hunk_open` have no such loop: their theorems state
exactly what a loop would check.

### Negative controls

Each mutation changed one comparison or one constant in one body and left
the clause and the theorem alone.

Round 1, run on 2026-10-20 from 0560e211c2. Each mutant was its own lane run,
`targo --unverified ship trust run -p amacs`, restored with
`git checkout -- <file>`, and `git diff` was confirmed empty after.

| fn | mutation | lane total | what the log said |
| --- | --- | --- | --- |
| `line_space` | `b'\r'` → `b'\n'` | 414 / 1316 | island rejected (`simp`, 1 unsolved goal); `[VIOLATIONS] (3 obligations: 2 proved, 0 runtime-checked, 1 failed …)` |
| `ident_byte` | `b'_'` → `b'-'` | 409 / 1311 | island rejected (`simp`, 1 unsolved goal); `[VIOLATIONS] (23 obligations: 21 proved, 0 runtime-checked, 2 failed …)` |
| `bare_key_byte` | `b'-'` → `b'.'` | 410 / 1311 | island rejected (`simp`, 1 unsolved goal); `[VIOLATIONS] (23 obligations: 22 proved, 0 runtime-checked, 1 failed …)` |
| `metachar` | `b'>'` → `b'='` | 410 / 1312 | island rejected (`simp`, 1 unsolved goal); `[VIOLATIONS] (7 obligations: 6 proved, 0 runtime-checked, 1 failed …)`; the instance discharge of `shell::word`'s closure, which calls `metachar`, FAILED as well |
| `summary_too_long` | `chars > limit` → `chars >= limit` | 410 / 1312 | island rejected (`simp`, 1 unsolved goal); `[VIOLATIONS] (2 obligations: 0 proved, 0 runtime-checked, 1 failed, 1 unknown …)` |
| `hunk_open` | `old > 0 \|\| new > 0` → `old > 0 && new > 0` | 409 / 1312 | island rejected (`simp`, 1 unsolved goal); `[VIOLATIONS] (3 obligations: 0 proved, 0 runtime-checked, 2 failed, 1 unknown …)` |

Every round 1 mutant was refused in the same three ways:

- the theorem's island was rejected: "Clean island declaration rejected:
  InTactic { tactic: "simp", … UnsolvedGoals { count: 1, … } }";
- that tainted the crate's Clean environment, so every citation failed
  ("cannot be validated because an earlier Clean island was rejected and the
  crate-local environment is tainted"), and the lane printed "could not
  compile `amacs` (lib) due to 7 previous errors";
- the mutated fn's row read `[VIOLATIONS]`, with "postcondition FAILED
  (trust-full-verifier) counterexample: verified_counterexample = true".

The lane still exited 0 for each round 1 mutant, because the committed floor
then was 191 and every mutant stayed above it. The exit code alone is
therefore not the control; the rejection and the row are.

Round 2, run on 2026-10-20 for the seven contracts review asked for, in two
steps.

First, one at a time in a probe crate: the seven contracts and their
islands, copied into one file with the kernel imports renamed for the probe
crate, compiled with the lane's `trustc` (`--crate-name nc
-Ztrust-verify=on`), once unmutated (all six islands kernel-checked, all nine
citations kernel-discharged, exit 0) and once per mutant. Each mutant left
the other five islands kernel-checked and exited 1:

| fn | mutation | what `trustc` said |
| --- | --- | --- |
| `utf8_width` | `byte < 0xC0` → `byte < 0xC1` | `utf8_width_is_the_table` rejected: `simp`, 1 unsolved goal |
| `value_byte` | `b':'` → `b';'` | `value_byte_is_exactly_the_value_set` rejected: `simp`, 1 unsolved goal |
| `tab_stop` | `{ 1 }` → `{ 0 }` | `tab_stop_is_never_zero` and `tab_stop_keeps_a_nonzero_width` rejected: `TypeMismatch`, the case split no longer fits the body |
| `indent_columns_cap` | `> MAX_INDENT_COLUMNS` → `> 2048` | `indent_columns_cap_is_at_most_the_cap` and `indent_columns_cap_keeps_a_value_within_it` rejected: `TypeMismatch` |
| `decimal_room` | `429_496_728` → `429_496_729` | `decimal_room_is_the_bound` rejected: `simp`, 1 unsolved goal |
| `decimal_append` | `wrapping_mul(10)` → `wrapping_mul(16)` | `decimal_append_is_the_step` rejected: `simp`, 1 unsolved goal |
| `delimiter_hash_step` | `16_777_619` → `16_777_618` | `delimiter_hash_step_is_the_polynomial` rejected: `simp`, 1 unsolved goal |

Then all seven mutations at once in the crate itself, applied to 01c99738e0
and run through `targo --unverified ship trust run -p amacs` against the floor
01c99738e0 ratchets to, 490:

- the lane printed `TOTAL: 464/1393 proved` and `VERDICT: FAIL — amacs
  RATCHET-REGRESSION(<490) 464/1393`, and exited 1 (15 min 50 s);
- all nine theorems of the seven mutated contracts were rejected:
  `tab_stop_is_never_zero`, `tab_stop_keeps_a_nonzero_width`,
  `indent_columns_cap_is_at_most_the_cap` and
  `indent_columns_cap_keeps_a_value_within_it` with `TypeMismatch` (the case
  split no longer fits the body), and `decimal_room_is_the_bound`,
  `decimal_append_is_the_step`, `utf8_width_is_the_table`,
  `delimiter_hash_step_is_the_polynomial` and
  `value_byte_is_exactly_the_value_set` with `simp`'s 1 unsolved goal. The six
  unmutated islands were still kernel-checked;
- the rejections tainted the crate's Clean environment, so all 15 citations
  "cannot be validated", and the lane printed "could not compile `amacs`
  (lib) due to 24 previous errors";
- the mutated fns' rows: `tab_stop`, `indent_columns_cap`, `decimal_room` and
  `value_byte` read `[VIOLATIONS]` (1 failed obligation each, a
  counterexample), and `decimal_append`, `utf8_width` and
  `delimiter_hash_step` read `[INCONCLUSIVE]` (2 unknown each, 0 proved);
- `IndentStyle::next_stop`'s remainder went from proved to unknown, and
  `IndentStyle::render` went from 4 proved to 2: with `tab_stop` mutated to
  return 0, its `result >= 1` no longer holds, and the divisions that rest on
  it are no longer proved.

The unmutated contracts' rows stayed `[VERIFIED]`, each with one obligation
fewer than the clean run (`line_space` 3 of 3, against 4 of 4). A contract's
row alone therefore does not show a broken island elsewhere in the crate; the
rejections, the "could not compile" and the total do. The four files were
restored with `git checkout --`, and `git diff` then showed only this file
and the trust tables.

### Refused forms

Each of these was tried in the probe crate first and is why a contract has
the shape it has.

| form | what Trust said | what the code does instead |
| --- | --- | --- |
| one saturating step, `if value > 429_496_728 { u32::MAX } else { value.wrapping_mul(10).wrapping_add(digit) }`, and the same with the step `let`-bound | island rejected: `AutoBoundHeadOverApplied` (no kernel import): "all four public E6 structural facet findings … positive, but they are diagnostic only; no sealed, item-bound kernel admission exists" | the guard `decimal_room` and the step `decimal_append` are two contracts, and `decimal_push` composes them |
| an FNV-1a step, `(hash ^ byte).wrapping_mul(16_777_619)` | island rejected: `AutoBoundHeadOverApplied`: E6 imports no `^` | a multiply-then-add polynomial step over the same prime |
| the no-wrap lemma, `value.toNat ≤ 429496728 → digit.toNat ≤ 9 → (value * 10 + digit).toNat = value.toNat * 10 + digit.toNat` over `UInt32` | `omega`: `trustc` was killed (exit 137) after 79 s; `bv_omega`: "unknown tactic"; `simp [UInt32.toNat_add, UInt32.toNat_mul]`: "no progress" | the two closed `Nat` facts are proved with `decide`, and `decimal_push`'s no-wrap behaviour is tested against `u64` arithmetic |
| `width.max(1)`, `columns.min(1024)`, `(b'0'..=b'9').contains(&byte)` | calls, which E6 refuses to import | selects and comparison trees, each with a test against its std form |
| a bit of a bracket set, `if depth < 32 { 1u32 << depth } else { 0 }` (`rust::level_bit`; `toml::Nesting` has the same shift), and `1u32.wrapping_shl(u32::from(depth))` | island rejected: `AutoBoundHeadOverApplied`, for both: E6 imports no shift | no contract: the lane proves the shift and subtract rows of `level_bit`, `levels_below` and `Nesting` (L0 safety), and the goldens and `indent_rules` test the depths they track |

### Hazards made provable

The lane's first run on the new code left nine arithmetic rows failed and
five unknown. None could be reached: each operand is bounded by a line's
length or by a clamp just above it. Each was still a MIR overflow assert, so
each became saturating, and its row became provable. No lexer output changed.
The last row is from the review round's runs: a divisor whose bound the
prover did not see where it was used.

| site | was | now | why it was never reached |
| --- | --- | --- | --- |
| `language.rs` `IndentStyle::columns_through` (now `next_stop`) | `width - columns % width` | `saturating_sub` | `columns % width < width` (`width ≥ 1`) |
| `language.rs` `hash_comment_leader` | `blanks + hashes + …` | `saturating_add` | three lengths within one line |
| `language.rs` `Cursor::eat_while` | `pos - start` | `saturating_sub` | the cursor only moves forward |
| `language.rs` `LexCache::splice` | `len - first`, `first + removed` | `saturating_sub`, `saturating_add` | `first ≤ len`, `removed ≤ len - first` |
| `language/rust.rs` `quote`, `word` | `width + 1`, `width + 2`, `prefix + 1`, `start + 1` | `saturating_add` | `width ≤ 4`, `prefix ≤ 2`, `start < len` |
| `language/shell.rs` `word` | `start + name.len()` | `saturating_add` | a prefix of the word at `start` |
| `language/git.rs` `trailer` | `key + 1` | the cursor's position after the colon | the same byte |
| `language/diff.rs` `lex` | called itself to re-read a line that ends a hunk early | `hunk_line` answers `None` instead | one level deep; the lane could not see the self-call terminate |
| `language.rs` `IndentStyle::render`, `columns_through` | divided by `self.stop()`, a method around `tab_stop`, the remainder inside `columns_through`'s loop | `render` calls `tab_stop` directly, and the loop calls `next_stop`, which does | `tab_stop`'s result is at least 1; through the method, or inside the loop, the prover did not see it |

### Rows that stay unproved, and why

Read off the 01c99738e0 run's obligation table
(`target-trust/logs/amacs@0.107.0.log.oblig.tsv`, `--obligations`; a run
now writes it under `target-trust.noindex/logs/`): the
rows under `crates/amacs/src/language*` are 470 obligations, 221 proved, 144
runtime-checked, 105 unknown and 0 failed: no row reports a counterexample.
The merged tree's run (f7e865eee3) gave the same counts for those 470 rows,
plus one unknown `native-verification-gap` row of `toml::lex` with no
source site, which the file filter above does not count.

| rows | status | what the lane said | why it stays |
| --- | --- | --- | --- |
| 75 | unknown | "allocation(s) … rely on `GlobalAlloc::{alloc,dealloc}` never panicking; DISCHARGED vacuously …" (a conditional policy row, no proof credit) | the lane's policy for every allocating body, crate-wide; nothing in a body changes it |
| 24 | unknown | "[trust-instance-hypothesis] … the panic-freedom of this open generic dispatch is assumed for every instantiation": 23 at the `Cursor::eat_while(…)` calls with a closure or fn item, 1 at the `FnMut` call inside `eat_while` | the cursor takes `impl FnMut(u8) -> bool`, and the lane does not discharge a call through that open generic |
| 1 | unknown | `eat_while`'s drop glue for its generic parameter ("needs monomorphization") | the same generic parameter |
| 4 | unknown | "`str::repeat` computes its result capacity (len * n) inside the library impl and can overflow" in `IndentStyle::render` (3) and `markdown::hanging_indent` (1) | `render` caps `n` at 1024 (`indent-cap-bound`) and `hanging_indent` repeats a list marker's width; the multiply is inside std, where no contract reaches |
| 1 | unknown | `LexCache::splice`: "unsupported MIR `vec-range-method-bounds-unmodeled`: Vec::splice/extract_if panics when the range start exceeds its end or the end exceeds the length; the call RESIZES the Vec …" | the range is `first..first + removed`, and `splice` clamps both before the call (`first ≤ len`, `removed ≤ len − first`), so it is in bounds; the lane does not model `Vec::splice`'s range. `incremental_lex_equals_full_lex` drives it with random edits, `removed = 0` and `inserted = 0` among them |
| 72 | runtime-checked | "[trust-absent-callee-assumption] call to absent callee …": the module's own functions called across a function boundary (36, `Spans::push` 20 of them), `dyn fmt::Write::write_str` (17, the derived `Debug`s), derived `Hash` (9), `Vec` `clone`, `eq` and `push` (7), `str::repeat` (2) and `str::starts_with` (1) | each callee's body is not in the bundle the lane lowers for its caller; the module's own callees have their rows where they are defined |
| 35 | runtime-checked | "bundled std member … `Iterator::count` … carries an internal Assert that no row of this function discharges", over a `take_while` | the counter is inside std (a `usize` count of a line's bytes), where no row of the caller discharges it |
| 4 | runtime-checked | the same, for std's `Enumerate<Split>::next` | a line splitter inside std |
| 30 | runtime-checked | the whole body's panic freedom, with no native evidence: "invalid native solve input … uses Alloca without an internally bound extent" (22) or "… executes Undef … trust-mc has no typed CHC input for this obligation" (8) | the native verifier takes no input for those bodies (small `&str` readers such as `leading_blanks`, `quote_leader`, `by_extension`) |
| 3 | runtime-checked | the same, "trust-assert-refute: compiler transport claimed a favorable outcome without attachable native or clean-kernel proof evidence; published as runtime-checked under policy B", in `diff::digits`, `diff::range_count` and `markdown::ListItem::next_marker` | the compiler's own claim is not counted as proof; rustc's check stays in the binary |

No row above is a scalar law left uncontracted for want of trying: the
thirteen contracts state fifteen specs, and the forms E6 refused are listed
under "Refused forms". What stays unproved is the lane's crate-wide policy
(allocation), std's own internals (`str::repeat`, `Iterator::count`), calls
the lane does not follow (absent callees, the open `FnMut`), or a law over
`&str` and `Vec`, which the property tests below cover.

### What is tested rather than proved

The lexers take a `&str` and return a `Vec<Span>`, and `LexCache` holds a
`Vec` of lines. Those are non-scalar carriers, which get no E6 import (Gap 1
in docs/PLAN-tests-to-specs-2026-10-15.md), so their laws are property tests:

- `incremental_lex_equals_full_lex`: after random edits, `LexCache::splice`
  equals a full lex, and every span is non-empty, in order, inside its line
  and on char boundaries. The edits are whole-line inserts (`removed = 0`),
  whole-line deletes (`inserted = 0`), and replacements of a char range that
  may cross lines, drawn from fragments that open and close every
  multi-line state (strings, block comments, fences, here-documents).
- `lexing_reads_each_byte_a_bounded_number_of_times`: the cursor's byte reads
  stay at most 8 per byte plus 16 on lines built to make a lexer rescan, from
  every state a line can start in. A quadratic mutation (rescanning to the end
  of the line on each escape) read 4,002,002 bytes for a 4,000-byte line and
  failed it.
- `indent_rules`: re-indenting rustfmt's output with the bracket-depth rule
  changes no line, and `Language::line_break` keeps a string's text, trims
  the blanks around the break, and continues Markdown list items and quotes.

## 7. The P0 commands (slice S5)

Slice S5's two scalar decisions, `fill-paragraph`'s word fit and TAB's
distance to the next stop. The two subsections were measured on S5's branch
before it was merged with the sections above; the last one is the merged
tree. The macros are the crate's (`src/lib.rs`).

### `word-fits` (`src/editor/fill.rs`, slice S5)

`word_fits(room, word_width)` decides whether a word `word_width` columns
wide joins a line that has `room` columns left before the fill column, after
the one space that separates it from the word before. The word needs
`word_width + 1` columns, so

    ensures result == (word_width < room) by word_fits_is_narrower_than_the_room

where the theorem states, over `trust_import_amacs__editor__fill__word_fits`,
the same comparison in Clean (`decide (word_width < room)`). The caller passes
`room = fill_column.saturating_sub(line_width)` (`room`), so with
`line_width <= fill_column` the decision is
`line_width + 1 + word_width <= fill_column`, made without the addition,
which could overflow, and a line already at or past the column has no room.

The contract takes the room rather than the line and the column because E6
mints no kernel import for a body holding a checked subtraction's overflow
assert (gap 3 of `docs/PLAN-tests-to-specs-2026-10-15.md`). The first form,
`word_fits(line_width, word_width, fill_column)` with
`word_width < fill_column && line_width < fill_column - word_width`, had its
postcondition proved by the solver, but its island was rejected
(`AutoBoundHeadOverApplied` on the import name), which fails the crate's
Trust compile.

`fill-paragraph` (M-q) asks it for every word after a line's first, so a
filled line is never wider than the fill column unless its one word is.

**Proved** (2026-10-20, `targo --unverified ship trust run -p amacs` on
targo 1.99.0-dev 41d799797): `amacs::editor::fill::word_fits [VERIFIED]`,
2 of 2 obligations, both postconditions `clean-kernel-citation`,
`CONSTRUCTIVE (certified; validation=kernel-rechecked)`. Negative control:
the body mutated to `word_width <= room` leaves the theorem's `simp` with
an unsolved goal, the island is rejected and the postcondition is UNKNOWN;
the file was then restored byte for byte.

Test: `editor::command::tests::word_fits_is_room_for_a_space_and_the_word`
checks it, through `saturating_sub`, against `line + 1 + word <= column` in
`u128` over every `0..12` triple and at the `u64::MAX` edges;
`editor::command::tests::fill_paragraph_fills_to_the_fill_column` samples it
through the command.

### `tab-stop-columns` (`src/editor/edit.rs`, slice S5): blocked by gap 3

`tab_stop_columns(column, tab_width)` is how far TAB moves from `column`: to
the next multiple of the tab width, a whole stop from a column already on
one, with a zero width read as one column. The spec it should carry is

    ensures result == (if tab_width == 0 { 1 } else { tab_width - column % tab_width })

**Refused** (2026-10-20, `targo --unverified ship trust run -p amacs` on
targo 1.99.0-dev 41d799797), written over `u64` with the `.max(1)` call
replaced by that `if`, so its body held only a branch, a remainder and a
subtraction:

- the island was rejected: `AutoBoundHeadOverApplied` on
  `trust_import_amacs__editor__edit__tab_stop_columns`, because E6 mints no
  kernel import for a body that holds the remainder's divide-by-zero assert
  or the subtraction's overflow assert (gap 3);
- the clause was not lowered: "unsupported contract predicate expression"
  for the `if` in `result == (if …)`;
- and the rejected island taints the crate's Clean environment, so
  `word-fits`' citation could not be validated in the same run either.

The computation has no assert-free form (a tab stop is a remainder, and the
distance to it a subtraction), so the fn stays plain: its three safety
obligations are proved (the remainder's divisor is at least one, and the
remainder is below it). It is contracted when E6 takes checked arithmetic or
the proven-core program gives it a carrier.

Test: `editor::command::tests::tab_reads_the_indent_options` checks a column
between stops, one on a stop, and the zero width.

### The merged tree

S5 merged with origin/main ce903e0d35 (the 639/1727 line of §1) measured
`TOTAL: 704/1944 proved` with the gate line `704 proved, 14 failed, 606
runtime-checked, 470 assumed, 2 mandated, 0 contract-panic, 148 unknown /
1944 total`. The one failed row more than the line's 13 was S5's
`record_kill`: a backward kill (`M-backspace` twice) joined the newest
entry with `insert_str(0, ..)`, whose panic-freedom Trust does not model
(`Call::insert_str::panic-freedom-unverified`). Index 0 is always a char
boundary, so it never panicked; it now builds the new text with the entry
pushed after it, which has no index, and
`kill::tests::kill_append_joins_consecutive_kills` still reads
`["beta gamma"]`. The re-run measured `TOTAL: 704/1943 proved`, gate line
`704 proved, 13 failed, 606 runtime-checked, 470 assumed, 2 mandated, 0
contract-panic, 148 unknown / 1943 total` (`--update-ratchet`, the floor
now). The 13 failed rows are §4's: `run`'s two are `repeat_line_motion`'s
instance discharges, which moved there from `execute`. Four rows time out,
down from §4's seven, because TAB's remainder and subtraction left
`execute` for `tab_stop_columns`, whose three obligations are proved.
Every contracted fn of §2, §6 and this section (22 fns, `word_fits`
included) is `[VERIFIED]` with the counts in its table, and 29 citations
kernel-discharge (§2's 13, §6's 15 and `word_fits`' one). Both runs are
`not-reproducible`, as §1 says; the re-run's Stop causes line counts
`wall-backstop=17`.

## 8. Re-measuring

```sh
targo --unverified ship trust run -p amacs --obligations   # rows in target-trust.noindex/logs/amacs@<ver>.log{,.oblig.tsv}
targo --unverified ship trust run -p amacs --update-ratchet
targo --unverified verify test -p aterm-contract-probe     # before changing a contract
```
