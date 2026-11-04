<!-- Copyright 2026 Andrew Yates -->
<!-- SPDX-License-Identifier: Apache-2.0 -->

# amacs: specs proved on the Trust lane, and the ones still blocked

What the Trust lane proves about amacs: contracts on its pure scalar core, each
cited to a Clean theorem the kernel checks against the compiled body, with the
mutation that shows the theorem depends on that body; and the properties that
cannot be proved yet, each with the refusal that blocks it. §2 to §4 cover the
editor core, the protocol and the rope, §5 the language table, and §6
`fill-paragraph` and TAB. E6 is Trust's kernel import of a compiled body: its
carriers are `bool`, `uN` and `iN`, and it refuses calls. "Gap 1" (non-scalar
carriers get no E6 import) and "gap 3" (E6 refuses shapes such as a
checked-arithmetic assert) are the gaps of `docs/PLAN-tests-to-specs-2026-10-15.md`,
"What blocks the rest".

## 1. The lane

```sh
targo --unverified ship trust run -p amacs --obligations   # rows in target-trust.noindex/logs/amacs@<ver>.log{,.oblig.tsv}
targo --unverified ship trust run -p amacs --update-ratchet
targo --unverified verify test -p aterm-contract-probe     # before changing a contract
```

The lane runs `targo trust check -p amacs --lib --allow-l0-gaps` and holds the
crate to its row in `crates/aterm-release/trust/floors.tsv`, which names the
prover the floor was measured under; this host keeps that prover at
`~/toolchains/trust-41d79979`, so `--prover ~/toolchains/trust-41d79979/bin`
re-judges the floor.

The prover marks every amacs run `not-reproducible`: some rows of the
reducer's one large command match end on the solver's clock
(`TIMEOUT (trust-full-verifier, 120000ms)`), so a loaded host times out more
of them. The proved counts repeat from run to run, so the floor is
ratcheted anyway. A run below the floor with more timeouts is the clock, not
the code: rerun it on a quieter host before reading it as a regression.

A rejected Clean island taints the crate's Clean environment: every citation
in the crate then reports `cannot be validated because an earlier Clean
island was rejected`, and the crate's Trust compile fails.

## 2. Proved specs

Each contract is written through the crate's `trust_contract_fn!`
(`src/lib.rs`), which emits the native `ensures` under `cfg(trust_verify)` and
a plain fn on every other compiler. Each clause cites a theorem in a
`verify_island! { clean { … } }` block, stated over the kernel import
`trust_import_amacs__<module>__<fn>`: a statement about the compiled body, not
a restated copy. Every contract in this file is `[VERIFIED]`: each obligation
of its fn is proved, each postcondition is `PROVED (clean-kernel-citation,
CONSTRUCTIVE (certified; validation=kernel-rechecked, …))`, and the lane
prints that the citation kernel-discharges the postcondition with a clean
certification audit.

The negative control column names the mutation that broke the theorem. Each
control changes one constant or comparison in one body, leaves the clause and
the theorem alone, runs the lane, and restores the file
(`git checkout -- <file>`, then an empty `git diff`). A control holds when
the theorem's island is rejected and the mutated fn's row is no longer
`[VERIFIED]` (`[VIOLATIONS]` with `verified_counterexample = true`, or
`[INCONCLUSIVE]`). Neither the lane's exit code nor another contract's row is
the control: the exit code also moves with timeouts and the floor, and the
taint fails every citation (§1) while the unmutated rows stay `[VERIFIED]`.

| id | fn | statement (for every input) | theorem | negative control |
| --- | --- | --- | --- | --- |
| `utf8-char-start` | `text::utf8_char_start(byte: u8) -> bool` | `result ⇔ byte < 0x80 ∨ byte ≥ 0xC0`: a char starts at an ASCII byte or a lead byte, never at a continuation byte (0x80..=0xBF) | `utf8_char_start_skips_continuations` | `0xC0` → `0xBF` |
| `taller-child-covers-both` | `text::taller_child(left: u8, right: u8) -> u8` | `result ≥ left ∧ result ≥ right` | `taller_child_covers_both` | branches swapped (the body becomes `min`) |
| `taller-child-is-a-child` | `text::taller_child` | `result = left ∨ result = right` | `taller_child_is_a_child` | (same mutation) |
| `repeat-count-bounds` | `editor::command::repeat_count(value: i64) -> i64` | `1 ≤ result ≤ 10000` | `repeat_count_within_bounds` | the cap `10_000` → `10_001` |
| `repeat-count-identity` | `editor::command::repeat_count` | `value < 1 ∨ value > 10000 ∨ result = value` | `repeat_count_keeps_an_in_range_value` | (same mutation) |
| `viewport-capacity-bounds` | `editor::view::visible_line_capacity(visible_lines: usize) -> usize` | `1 ≤ result ≤ 256` | `visible_line_capacity_within_bounds` | the cap `256` → `257` |
| `viewport-capacity-identity` | `editor::view::visible_line_capacity` | `visible_lines < 1 ∨ visible_lines > 256 ∨ result = visible_lines` | `visible_line_capacity_keeps_an_in_range_value` | (same mutation) |
| `projected-column-limit-bounds` | `editor::projection::projected_column_limit(requested_columns: usize) -> usize` | `4 ≤ result ≤ 4096` | `projected_column_limit_within_bounds` | the cap `4_096` → `4_097` |
| `projected-column-limit-identity` | `editor::projection::projected_column_limit` | `requested_columns < 4 ∨ requested_columns > 4096 ∨ result = requested_columns` | `projected_column_limit_keeps_an_in_range_value` | (same mutation) |
| `file-uri-unescaped-set` | `protocol::uri::file_uri_keeps(byte: u8) -> bool` | `result ⇔ byte ∈ [0-9A-Za-z] ∪ {/ - _ . ~ :}`; every other byte, space and `%` included, is percent-encoded by `path_to_file_uri` | `file_uri_keeps_exactly_the_unescaped_set` | `b'~'` → `b' '` |
| `h6-fits` | `protocol::anchor::fits_h6(value: u32) -> bool` | `result ⇔ value ≤ 0xff_ffff` (24 bits); `Hash6::from_value` admits exactly these values | `fits_h6_is_24_bits` | body `value < H6_MAX` |
| `h6-hex-digit` | `protocol::anchor::is_hex_digit(byte: u8) -> bool` | `result ⇔ byte ∈ 0x30..=0x39 ∪ 0x61..=0x66 ∪ 0x41..=0x46` (0-9, a-f, A-F); `Hash6::from_str` reads 6 such bytes and nothing else | `is_hex_digit_is_the_class` | `byte < b'f'` for the lower-case range |
| `for-word-byte-set` | `protocol::edit::for_word_byte(byte: u8) -> bool` | `result ⇔ byte ∈ [0-9A-Za-z] ∪ {. _ -}`; neither space nor `=` is in the set, so a `for=` word stays one `key=value` field | `for_word_byte_is_exactly_the_word_set` | `b'-'` → `b'='` |
| `doc-disk-same` | `protocol::doc::disk_same(now_exists: bool, base_exists: bool, same_len: bool, same_content: bool) -> bool` | `result ⇔ now_exists ∧ base_exists ∧ same_len ∧ same_content`: `doc ls` prints `disk=same` only when the file is there, the buffer's version was a file, and both length and content agree; never of a missing file, and never of a rewrite of the same length | `disk_same_is_all_four` | body `now_exists && base_exists && (same_len \|\| same_content)` |

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
- `fits_h6` and `is_hex_digit` read a `line:h6` anchor back. An anchor is
  `<n>:<h6>`: a 1-based line number and the top 24 bits of FNV-1a/64
  (`aterm_hash::fnv1a_64`) of the line's bytes, without its end-of-line, as 6
  hex digits, and reading
  one must accept exactly what a read prints. `protocol::anchor::tests` check
  that composition (a printed anchor parses back; shapes a read never prints
  are refused), which the two contracts do not state.
- `disk_same` is the `same` row of `DiskState::by_content`, the table
  aterm-gui's `doc ls` lane reads `disk=` from (`control_doc::disk_state`).
  `by_content` asks `!now_exists` first; the theorem's `now = false` rows
  show the order does not matter.
  `protocol::doc::tests::disk_state_by_content_is_its_table` checks the
  wrapper's arms over all 16 inputs.

How the theorems are written:

- The one-byte predicates and `fits_h6` close with `simp only [<import>]`: the
  clause and the compiled `bool` tree elaborate to the same `decide` term.
- `disk_same_is_all_four` is `simp only [<import>]`, then `cases` on each
  `Bool` argument and `rfl`. A theorem `simp only` already closes must stop
  there: a `cases` after it fails with `NoGoals`, and the rejected island
  taints every citation (§1).
- The selects are term-mode `@Decidable.casesOn` proofs over the comparison
  the body makes, with an explicit motive.
- Trust reads an `i64` as a `BitVec 64`; the `repeat_count` proofs order it
  through `i64_order_key v = v.toNat xor 2^63`, the key the elaborated clause
  compares.
- Trust reads a `usize` as a `UInt64` on this target, so `usize` theorems are
  stated over `UInt64` (Clean's `decide` does not reduce `USize.ofNat`
  literal comparisons).

These tests check each contract against the std form it replaced, over every
byte, every `u8` pair or the clamp's edges. No Clean theorem can state those
forms (`u8::is_ascii_alphanumeric`, `u8::max`, `str::is_char_boundary`,
`Ord::clamp`): they are calls, and E6 refuses calls.

- `text::tests::contracted_scalar_core_matches_the_std_forms_it_replaced`
- `editor::command::tests::repeat_count_is_the_clamp_it_replaced`
- `editor::view::tests::visible_line_capacity_is_the_clamp_it_replaced`
- `editor::projection::tests::projected_column_limit_is_the_clamp_it_replaced`
- `protocol::uri::tests::file_uri_keeps_the_set_it_replaced`
- `protocol::edit::tests::for_word_byte_is_the_set_it_replaced`

## 3. Rows that stay unproved, and why

None is a reachable hazard in amacs's own code: each is a fact the solver
cannot see, or code Trust has no model for. By status and kind:

- **Failed, `unbounded_allocation`:** `with_capacity` sized by the document
  (`TextRope::from`, `TextRope::to_flat_string`). A document
  has no static size bound, and a `requires` would only move the obligation
  to every caller. The same holds for the unknown `collect`/`extend` rows.
  `project_viewport_with`'s and `concat`'s capacities are bounded in fact (a
  clamp, `CHUNK_MERGE_MAX`), but the solver does not derive the bound through
  `clamp`, `min` and `len`.
- **Failed, `expect` panic-freedom** (and its hardened twins, unknown):
  static chord literals (`Keymap::emacs`, `handle_chord`, `reduce_key`),
  `NonZeroU64::new(raw)` after `raw = next_id.max(1)` (`DocumentStore::open`),
  and a second pass over keys the first validated (`commit_detach_batch`).
  Trust does not model `expect`'s panic-freedom.
- **Failed, `instance-hypothesis:discharge`:** `repeat_line_motion`'s call
  through its `impl Fn(usize) -> usize` step (§4). Unknown rows of the same
  kind: its open generic dispatch and drop glue, and
  `aterm_grapheme::word_break_before::<fn(char) -> bool>` in
  `next_word_boundary`.
- **Failed, `unreachable`** in the derived `Ord::cmp` of `AwaitKey`: generated
  code.
- **Timeout:** rows of the reducer's command match (§1). Arithmetic there
  that cannot overflow (`-delta`, with `delta ≥ 1`) stays a runtime check
  because its queries time out.
- **Unknown, `unsupported_mir`:** allocating bodies (`String`, `Vec`, `Arc`;
  Gap 1); unmodeled char boundaries (`&text[a..b]`, `String::truncate`) and
  slice ranges indexed by a cloned `Range`; termination of the rope's
  recursions (`split`, `is_boundary`, `chunk_count`, `append_to`), whose
  measure, the tree height, needs an enum carrier (Gap 1); `str::repeat`'s
  capacity; and verifier bookkeeping.
- **Unknown, `native-verification-gap`:** the native verifier's own rows,
  with no source site.
- **Unknown, `hardened_error_discard` and `hardened_utf8_reject`:** deliberate
  defaults (`unwrap_or_default` of an empty tail or a missing prefix label,
  `parse::<u32>().ok()` of a line field `parse_line` then reports as
  `BadLine`) and `percent_decode`'s refusal of non-UTF-8 bytes
  (`UriError::InvalidEncoding`).
- **Unknown, `non_termination`:** the keyboard-macro cycle through
  `play_macro` (§4), and `clamp_to_char_boundary`'s loop, which decreases
  `position` by one each turn and stops at 0.
- **Runtime-checked:** calls to absent callees (`dyn`, `alloc`, `core`,
  `aterm_*`), whole-body panic freedom, drop glue, bundled std members,
  `format!` dispatch, and `Ord::clamp`'s `min > max` at
  `window_start.clamp(start, end)`: the callee's body is not in the bundle.
- **`protocol::{anchor,read,script,doc}`:** unknown and runtime-checked rows
  of the same kinds, not itemised: allocating bodies, `collect` counts the
  solver cannot derive, and `doc`'s `str::parse` error discard behind a
  digits-only check. `doc`'s helpers `digits` and `column` read a `&str`, so
  Gap 1 gives them no clause type.

## 4. Properties that cannot be proved today

Each quoted refusal is from `targo trust check --lib --allow-l0-gaps`: on
amacs itself, or, for a scalar shape amacs does not have, on a scratch crate
holding the body it would need. A "not attempted" row was never stated as a
clause. `flat` is `to_flat_string`.

| property | where | shape | refusal (quoted) |
| --- | --- | --- | --- |
| tab stop: `width - column % width ∈ 1..=width` | `editor/edit.rs` `tab_stop_columns` | `%` and `-` on `usize` | island rejected: `AutoBoundHeadOverApplied` (gap 3: no import for a body holding the remainder's divide-by-zero or the subtraction's overflow assert; §6) |
| hex digit value `≤ 15` | `protocol/uri.rs` `nibble` | returns `Option<u8>` (Gap 1); a `u8` core, probed as a select over `wrapping_sub` | SHAPE gate: "the construct outside the E6 fragment is bb0 has no immediate post-dominator" |
| prefix argument digits: `value * 10 + digit ≥ value` for `value ≥ 0` | `editor/command.rs` `after_digit` | `saturating_mul` + `saturating_add` | FACET gate: "call to `core::num::<impl i64>::saturating_mul`, which is not itself structurally certified" |
| `C-u` multiplies by four, monotone | `editor/command.rs` `after_universal` | `saturating_mul(4)` | the same |
| AVL balance factor `\|h(l) - h(r)\| ≤ 1` after `concat` | `text.rs` `balance` | `i16::from(l) - i16::from(r)` | FACET gate: "call to `<i16 as core::convert::From<u8>>::from`, which is not itself structurally certified"; without the widening (`left > right + 1`), SHAPE gate on `u8::wrapping_add` |
| leaf merge stays under the chunk cap | `text.rs` `concat` | `left.saturating_add(right) <= CAP` | FACET gate: "call to `core::num::<impl usize>::saturating_add`, which is not itself structurally certified" |
| the `line:h6` anchor hash step (FNV-1a; probed before S6 wrote the code, which now calls `aterm_hash::fnv1a_64` in another crate) | `protocol/anchor.rs` | `(h ^ byte).wrapping_mul(P)` | SHAPE gate: "the construct outside the E6 fragment is the bit operation `^` (`BinOp::BitXor`) on `u64`" |
| an h6 is the top 24 bits of the hash (`h6_of_hash(hash) = hash >> 40`): `ensures result == (hash / 1099511627776)` | `protocol/anchor.rs` `h6_of_hash` | a division in the clause | *(lane)* "contract predicate … (hash / 1099511627776) uses arithmetic whose domain is not preserved" fragment: `protocol::anchor::h6_of_hash` F1 (VF5 first target, docs/PLAN-tests-to-specs-2026-10-15.md "F1-ready specs"). |
| the same h6 as a bound that does not divide: `ensures result <= 16777215 by h6_of_hash_fits` | `protocol/anchor.rs` `h6_of_hash` | a shift body | *(lane, `--obligations`)* `Clean island declaration rejected: AutoBoundHeadOverApplied { name: "trust_import_amacs__protocol__anchor__h6_of_hash", …, remaining_args: 1 }`: the shift body gets no import, because E6 admits neither a shift nor a division (Trust's `crates/trust-types/src/e6_expr.rs`). The rejected island tainted the crate's Clean environment, so the other citations "cannot be validated" and were withheld (361/1233 instead of 362/1233); the attempt was removed. `h6_of_hash` stays a plain fn, `[VERIFIED]` 2/2 on safety alone, and `anchor::tests::h6_matches_fnv1a_64_prefix_and_is_stable` pins the property against `fnv1a_64_to_hex16`. |
| a `count`-line motion stays on lines `0..=last` and stops at the first or last line | `editor/view.rs` `repeat_line_motion`, `previous_line_position`, `next_line_position` | no line arithmetic to contract: the motion walks `count` steps of an `impl Fn(usize) -> usize` step, and each step finds the neighbouring line with `str::rfind`/`str::find` over the text. The arithmetic form the plan names (`line.saturating_sub(count)`, `line.saturating_add(count).min(last)`) is not in the code, and would meet the `saturating_*` FACET refusal above. | *(lane)* the two failed rows at `view.rs:475`: "unsupported MIR `instance-hypothesis:discharge`: instance discharge: site `<impl Fn(usize) -> usize as core::ops::function::Fn<(usize,)>>::call` of `amacs::editor::view::repeat_line_motion::<{closure@crates/amacs/src/editor/workspace.rs:369:78: 369:82}>` (named at crates/amacs/src/editor/workspace.rs:369) resolves to `<{closure@…} as core::ops::function::Fn<(usize,)>>::call`", and in the step "call to absent callee `core::str::<impl str>::rfind::<char>` (body not in the lowered bundle) may panic" |
| vertical motion's goal column: the landing byte is the last cluster boundary at or before `desired_column` cells on the target line, never inside a cluster | `editor/projection.rs` `byte_at_editor_display_column` | a grapheme walk over `&text[start..end]` | *(lane)* "unsupported MIR `str-range-char-boundary-unmodeled`: str range index panics when an endpoint is not a UTF-8 char boundary; char boundaries are not modeled …" (`projection.rs:44`) and "call to absent callee `<str as aterm_grapheme::grapheme_iter::GraphemeClusters>::grapheme_indices` (body not in the lowered bundle) may panic"; and Gap 1 for `&str`. The scalar column clamp next to it, the projection's column limit, is proved (`projected-column-limit-*`). |
| rope height of a `Node` | `text.rs` `height` | `match` on the `Node` enum | FACET gate: "local `node` has type Adt { name: …Node … } … outside the v1 scalar fragment" (Gap 1: no enum carrier) |
| rope laws: `to_flat_string(replace(r, a..b, s)) = flat(r)[..a] ++ s ++ flat(r)[b..]`, `is_char_boundary` over the whole rope, balance after every edit | `text.rs` | `Arc<Node>`, `&str`, `String`, recursion | *(not attempted)* Gap 1: non-scalar carriers get no E6 import. |
| reducer laws: undo restores the text, a selection stays on a char boundary, `C-g` always cancels | `editor/*.rs` | `&mut self` over `Vec<Selection>`, `String`, `Option` | *(not attempted)* Gap 1, and `&mut` is outside E6. |
| keyboard-macro playback terminates (`play_macro` refuses to nest through `playing_macro`) | `editor/macros.rs`, `editor/workspace.rs` `execute` | a recursion cycle `execute` → `play_macro` → `execute` that a runtime flag breaks | *(lane)* "non-termination: recursion measure `none (`workspace::EditorWorkspace::execute` closes a recursion cycle; every member needs a `decreases` measure)` may not decrease": a flag in `&mut self` is not a measure Trust can read |
| `usize` literal facts in Clean (`decide` over `USize`) | any `usize` spec | `USize.ofNat` literals | "decide: failed to reduce `Decidable (Nat.le (USize.toNat (USize.ofNat 4)) (USize.toNat (USize.ofNat 4096)))` to `isTrue` or `isFalse`". Worked around: on this target the import uses `UInt64`, so the `visible_line_capacity` theorems are stated over `UInt64`. |

## 5. The language table

The contracts of `language` and its submodules, stated over
`trust_import_amacs__language__<module>__<fn>`, verified and controlled as §2
says.

| id | fn | statement (for every input) | theorem | negative control |
| --- | --- | --- | --- | --- |
| `line-space-set` | `language::line_space(byte: u8) -> bool` | `result ⇔ byte ∈ {0x20, 0x09, 0x0D}`: the blanks inside a line every lexer skips, a CRLF's `\r` among them | `line_space_is_exactly_the_blanks` | `b'\r'` → `b'\n'` |
| `tab-stop-nonzero` | `language::tab_stop(width: usize) -> usize` | `result ≥ 1`, so a column can always be divided by the stop | `tab_stop_is_never_zero` | `{ 1 }` → `{ 0 }` |
| `tab-stop-identity` | `language::tab_stop` | `width < 1 ∨ result = width`: a width of 1 or more is the stop | `tab_stop_keeps_a_nonzero_width` | (same mutation) |
| `indent-cap-bound` | `language::indent_columns_cap(columns: usize) -> usize` | `result ≤ 1024` (`MAX_INDENT_COLUMNS`), the most indentation `IndentStyle::render` writes | `indent_columns_cap_is_at_most_the_cap` | `> MAX_INDENT_COLUMNS` → `> 2048` |
| `indent-cap-identity` | `language::indent_columns_cap` | `columns > 1024 ∨ result = columns`: indentation within the cap is kept | `indent_columns_cap_keeps_a_value_within_it` | (same mutation) |
| `decimal-room-bound` | `language::decimal_room(value: u32) -> bool` | `result ⇔ value ≤ 429,496,728`, the largest value whose every next digit fits in a `u32`; the island's two closed facts are the bound's arithmetic, `429496728 · 10 + 9 ≤ 2³² − 1 < 429496729 · 10 + 9` | `decimal_room_is_the_bound`, `decimal_room_bound_takes_any_digit`, `decimal_room_bound_is_the_largest` | `429_496_728` → `429_496_729` |
| `decimal-append-step` | `language::decimal_append(value: u32, digit: u32) -> u32` | `result = value · 10 + digit` in `UInt32` arithmetic | `decimal_append_is_the_step` | `wrapping_mul(10)` → `wrapping_mul(16)` |
| `rust-ident-byte-set` | `language::rust::ident_byte(byte: u8) -> bool` | `result ⇔ byte ∈ [0-9A-Za-z_] ∪ [0x80, 0xFF]`; every byte of a non-ASCII character is in, so a span that ends where the set ends never splits a character | `ident_byte_is_exactly_the_word_set` | `b'_'` → `b'-'` |
| `utf8-lead-width` | `language::rust::utf8_width(byte: u8) -> u8` | `result` is 1 below 0x80, 0 for a continuation byte (0x80 to 0xBF), 2 below 0xE0, 3 below 0xF0 and 4 above (RFC 3629 §3) | `utf8_width_is_the_table` | `byte < 0xC0` → `byte < 0xC1` |
| `toml-bare-key-byte-set` | `language::toml::bare_key_byte(byte: u8) -> bool` | `result ⇔ byte ∈ [0-9A-Za-z_-]`, TOML's bare key set | `bare_key_byte_is_exactly_the_key_set` | `b'-'` → `b'.'` |
| `toml-value-byte-set` | `language::toml::value_byte(byte: u8) -> bool` | `result ⇔ byte ∈ [0-9A-Za-z_+.:-]`: the bytes of a bare value (a number, a date, a time, `true`) | `value_byte_is_exactly_the_value_set` | `b':'` → `b';'` |
| `shell-metachar-set` | `language::shell::metachar(byte: u8) -> bool` | `result ⇔ byte ∈ {; & \| ( ) < >}`, the operators that end a word, after which `#` starts a comment | `metachar_is_exactly_the_operators` | `b'>'` → `b'='` |
| `heredoc-hash-step` | `language::shell::delimiter_hash_step(hash: u32, byte: u32) -> u32` | `result = hash · 16777619 + byte` in `UInt32` arithmetic | `delimiter_hash_step_is_the_polynomial` | `16_777_619` → `16_777_618` |
| `commit-summary-limit` | `language::git::summary_too_long(chars: usize, limit: usize) -> bool` | `result ⇔ chars > limit`: a 51-character summary is flagged at the default 50, a 50-character one is not | `summary_too_long_is_past_the_limit` | `chars > limit` → `chars >= limit` |
| `diff-hunk-open` | `language::diff::hunk_open(old: u32, new: u32) -> bool` | `result ⇔ old > 0 ∨ new > 0`: a hunk ends only when both of its sides are used up | `hunk_open_until_both_sides_end` | `old > 0 \|\| new > 0` → `old > 0 && new > 0` |

Where each spec is used:

- `line_space` is every lexer's blank, and the cursor's.
- `tab_stop` is the divisor of `IndentStyle::render` and
  `IndentStyle::next_stop`. Both call it directly, outside any loop: through
  a method or inside a loop the prover does not carry its `result ≥ 1` to the
  division and remainders.
- `indent_columns_cap` caps what `IndentStyle::render` repeats.
- `decimal_room` and `decimal_append` make `decimal_push`, which reads the
  diff lexer's hunk counts and Markdown's list numbers: below the bound it
  appends the digit, and past it the result is `u32::MAX`, so a long run of
  digits saturates and never wraps. `decimal_push` is `[VERIFIED]` on safety
  alone, with no postcondition.
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
  table and the two wrapping chains: the clause and the compiled body
  elaborate to the same `decide` tree, `ite` chain, or `UInt32` `*` and `+`,
  which are Clean's wrapping operations.
- A term-mode case split, `@Decidable.casesOn (LT.lt x k) …
  (instDecidableUInt64Lt x k)`, for the two clamps, as in §2.
- `decide`, for the two closed `Nat` facts about the decimal bound.

The bool clauses nest `||` to the right, the way Clean elaborates them, and
the bodies are written with the same nesting.

`language::tests::contracted_scalar_core_matches_the_std_forms` checks each
contracted fn against the std form it stands for, which no Clean theorem can
state because it is a call:

- the byte sets, against `u8::is_ascii_alphanumeric` and a byte string's
  `contains`, over all 256 bytes;
- `utf8_width`, against `char::len_utf8` for the lead byte of every `char`,
  and 0 for every continuation byte;
- `tab_stop`, against `usize::max(1)`, and `indent_columns_cap`, against
  `usize::min(1024)`, at their edges;
- `decimal_push`, against `u64` arithmetic, for 0 to 30, the values around
  the bound and the two largest, with every digit: the exact
  `value · 10 + digit` while that fits, and `u32::MAX` after. That no-wrap
  law is tested, not proved (see the refused forms).

`summary_too_long` and `hunk_open` need no such loop: their theorems state
exactly what one would check.

### Refused forms

Each was tried in a probe crate, and is why a contract has the shape it has.

| form | what Trust said | what the code does instead |
| --- | --- | --- |
| one saturating step, `if value > 429_496_728 { u32::MAX } else { value.wrapping_mul(10).wrapping_add(digit) }`, with or without the step `let`-bound | island rejected: `AutoBoundHeadOverApplied` (no kernel import) | the guard `decimal_room` and the step `decimal_append` are two contracts, and `decimal_push` composes them |
| an FNV-1a step, `(hash ^ byte).wrapping_mul(16_777_619)` | island rejected: `AutoBoundHeadOverApplied`: E6 imports no `^` | a multiply-then-add polynomial step over the same prime |
| the no-wrap lemma, `value.toNat ≤ 429496728 → digit.toNat ≤ 9 → (value * 10 + digit).toNat = value.toNat * 10 + digit.toNat` over `UInt32` | `omega`: `trustc` was killed (exit 137) after 79 s; `bv_omega`: "unknown tactic"; `simp [UInt32.toNat_add, UInt32.toNat_mul]`: "no progress" | the two closed `Nat` facts are proved with `decide`, and `decimal_push`'s no-wrap behaviour is tested against `u64` arithmetic |
| `width.max(1)`, `columns.min(1024)`, `(b'0'..=b'9').contains(&byte)` | calls, which E6 refuses to import | selects and comparison trees, each with a test against its std form |
| a bit of a bracket set, `if depth < 32 { 1u32 << depth } else { 0 }` (`rust::level_bit`; `toml::Nesting` has the same shift), and `1u32.wrapping_shl(u32::from(depth))` | island rejected: `AutoBoundHeadOverApplied`, for both: E6 imports no shift | no contract: the lane proves the shift and subtract rows of `level_bit`, `levels_below` and `Nesting` (L0 safety), and the goldens and `indent_rules` test the depths they track fragment: `language::rust::level_bit` F1 (VF5 first target, docs/PLAN-tests-to-specs-2026-10-15.md "F1-ready specs"). |

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

No language row fails. What stays unproved is the lane's crate-wide policy for
allocating bodies; std's internals (`str::repeat`'s capacity in
`IndentStyle::render` and `markdown::hanging_indent`, `Iterator::count` over a
`take_while`, `Enumerate<Split>::next`, `Vec::splice`'s range); calls the lane
does not follow (absent callees, and `Cursor::eat_while`'s open
`impl FnMut(u8) -> bool`); small `&str` readers the native verifier takes no
input for; compiler claims without attachable proof evidence (`diff::digits`,
`diff::range_count`, `markdown::ListItem::next_marker`), which stay runtime
checks; and laws over `&str` and `Vec`. `LexCache::splice` clamps `first` and
`removed` before its `Vec::splice`, so that range is in bounds.

### What is tested rather than proved

The lexers take a `&str` and return a `Vec<Span>`, and `LexCache` holds a
`Vec` of lines. Those carriers get no E6 import (Gap 1), so their laws are
property tests in `language::tests`:

- `incremental_lex_equals_full_lex`: after random edits, `LexCache::splice`
  equals a full lex, and every span is non-empty, in order, inside its line
  and on char boundaries. The edits are whole-line inserts (`removed = 0`),
  whole-line deletes (`inserted = 0`), and replacements of a char range that
  may cross lines, drawn from fragments that open and close every
  multi-line state (strings, block comments, fences, here-documents).
- `lexing_reads_each_byte_a_bounded_number_of_times`: from every state a line
  can start in, the cursor makes at most `8 · len + 16` byte reads of a line,
  on lines built to make a lexer rescan.
- `indent_rules`: re-indenting rustfmt's output with the bracket-depth rule
  changes no line, and `Language::line_break` keeps a string's text, trims
  the blanks around the break, and continues Markdown list items and quotes.

## 6. `fill-paragraph` and TAB

### `word-fits` (`src/editor/fill.rs`)

`word_fits(room, word_width)` decides whether a word `word_width` columns
wide joins a line that has `room` columns left before the fill column, after
the one space that separates it from the word before. The word needs
`word_width + 1` columns, so

    ensures result == (word_width < room) by word_fits_is_narrower_than_the_room

where the theorem states the same comparison in Clean
(`decide (word_width < room)`). The caller passes
`room(line_width, fill_column) = fill_column.saturating_sub(line_width)`, so
with `line_width <= fill_column` the decision is
`line_width + 1 + word_width <= fill_column` made without the addition, which
could overflow, and a line already at or past the column has no room.
`fill-paragraph` (M-q) asks it for every word after a line's first, so a
filled line is never wider than the fill column unless its one word is.

The contract takes the room rather than the line and the column because E6
mints no kernel import for a body holding a checked subtraction's overflow
assert (gap 3): the form `word_fits(line_width, word_width, fill_column)`
with `line_width < fill_column - word_width` had its island rejected
(`AutoBoundHeadOverApplied`).

`[VERIFIED]`. Negative control: `word_width <= room`. Tests:
`editor::command::tests::word_fits_is_room_for_a_space_and_the_word` checks
it, through `saturating_sub`, against `line + 1 + word <= column` in `u128`
over every `0..12` triple and at the `u64::MAX` edges;
`editor::command::tests::fill_paragraph_fills_to_the_fill_column` samples it
through the command.

### `tab-stop-columns` (`src/editor/edit.rs`): blocked by gap 3

`tab_stop_columns(column, tab_width)` is how far TAB moves from `column`: to
the next multiple of the tab width, a whole stop from a column already on
one, with a zero width read as one column. The spec it should carry is

    ensures result == (if tab_width == 0 { 1 } else { tab_width - column % tab_width })

Written with the `.max(1)` call replaced by that `if`, so the body held only
a branch, a remainder and a subtraction, it was refused: the island was
rejected (`AutoBoundHeadOverApplied`), because E6 mints no kernel import for
a body holding the remainder's divide-by-zero assert or the subtraction's
overflow assert, and the clause's `if` is an "unsupported contract predicate
expression". A tab stop is a remainder and the distance to it a subtraction,
so the computation has no assert-free form: the fn stays plain, with its
safety obligations proved, until E6 takes checked arithmetic.
`editor::command::tests::tab_reads_the_indent_options` checks a column
between stops, one on a stop, and the zero width.
