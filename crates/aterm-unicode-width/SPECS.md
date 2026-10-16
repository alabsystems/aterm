<!-- Copyright 2026 Andrew Yates -->
<!-- SPDX-License-Identifier: Apache-2.0 -->

# aterm-unicode-width: spec recovery from its 3 unit tests

> Status 2026-10-16: this is the crate's migration worklist. Each of the 3 specs below is
> proved through Trust/Clean about the compiled Rust function. Then the tests it subsumes
> are deleted. No test in this crate is a non-spec check.

Source: `crates/aterm-unicode-width` (read only). I read every `#[test]` (tests/contract.rs: 3)
and the code each one reaches: `src/lib.rs` and `aterm_grapheme::tables::width::char_width`.
Nothing was compiled or run. The claims below come from reading the code.

Result: **3 specs**, covering **3 tests**. **0 tests are not spec samples** (§4).

The crate (package name `unicode-width`) is a facade with one production fn,
`<char as UnicodeWidthChar>::width`, whose body is
`(!is_control(c)).then(|| aterm_grapheme::char_width(c))`. Its only consumer is
codespan-reporting's renderer (`ch.width().unwrap_or(0)`).

## 0. Notation used in the statements

- `width(c)` = `<char as unicode_width::UnicodeWidthChar>::width(c)`.
- `Ctrl(c)` := c ∈ U+0000..=U+001F ∪ U+007F..=U+009F. This is exactly the private `is_control`.
- `PrintAscii(c)` := c ∈ U+0020..=U+007E.
- `cw(c)` := `aterm_grapheme::tables::width::char_width(c)`.
- `Wref(c)` := the reference width defined in
  [crates/aterm-grapheme/SPECS.md](../aterm-grapheme/SPECS.md) `S5-char-width-table`. It is 0 by the
  generator's zero-width rule, 2 when UCD16 EastAsianWidth is W or F (Ext G/H included), and 1
  otherwise (Ambiguous ↦ 1, U+00AD ↦ 1, plane 14 ↦ 0, beyond the table ↦ 1).
- `UW022(c)` := unicode-width 0.2.2's `UnicodeWidthChar::width`. `D` is the measured disagreement
  set: 56,741 code points, which are U+323B0..U+3FFFD plus 343 others (a lone regional indicator,
  U+00AD, Unicode 17 additions). `D` is described in the `src/lib.rs` doc comment and encoded nowhere.
- Every spec quantifies over every Rust `char`, that is every Unicode scalar value. The `char` type
  excludes surrogates.
- `S5` and `S7` are aterm-grapheme's lemmas (the char_width table property, and range plus no-panic).
  That crate's SPECS.md owns them. They are cited here and not restated.

## 1. Specs (TSV)

Columns: `id`, `kind`, `functions`, `statement`, `subsumes (primary tests; secondary in §2)`,
`difficulty (Rust features the proof must handle)`, `blocking gaps` (numbers from
`AUDIT-trust-for-test-migration-2026-10-15.md` (historical internal audit) §7).

```tsv
id	kind	functions	statement	subsumes	difficulty	blocking_gaps
UW1-facade-refines-grapheme-width	refinement	<char as UnicodeWidthChar>::width, is_control, aterm_grapheme::tables::width::char_width	∀ c: char. width(c) = if Ctrl(c) then None else Some(cw(c)). Equivalently, is_control(c) ⇔ Ctrl(c), and width never calls char_width on a control. This is the facade's defining equation. UW2 and UW3 follow from it plus aterm-grapheme S5/S7.	(none; lemma for UW2, UW3)	EASY: a trait method impl on char; a const fn whose body is a range `matches!`; bool::then with a closure; an Option<usize> result; a cross-crate call to a const fn	0,1,3,5,12,13
UW2-upstream-per-char-contract	contract	<char as UnicodeWidthChar>::width	∀ c: char. (a) Ctrl(c) ⇒ width(c) = None; (b) PrintAscii(c) ⇒ width(c) = Some(1); (c) ¬Ctrl(c) ⇒ width(c) ∈ {Some(0), Some(1), Some(2)}, in particular for every c ≥ U+00A0. Proof: (a) is UW1 alone. (c) is UW1 plus S7 (cw(c) ∈ {0,1,2}, no table read out of bounds, the unreachable! arm is dead). (b) is UW1 plus S5 evaluated on U+0020..U+007E (cw = 1: every one is EAW Na and none is zero-width). Negative control: dropping U+007F from is_control must fail (a), and a table byte that gives U+0041 class 0 must fail (b).	tests/contract.rs::controls_are_none_and_printable_ascii_is_one, tests/contract.rs::every_other_scalar_value_is_zero_one_or_two_columns	EASY-MEDIUM: an Option<usize> carrier; range quantification over char without surrogates; composition with the ensures of a callee in another crate (S5 on ASCII, S7); two-level static array indexing inside that callee	0,1,3,4,5,12,13
UW3-facade-equals-reference-width	table-property	<char as UnicodeWidthChar>::width, aterm_grapheme::tables::width::char_width	∀ c: char. ¬Ctrl(c) ⇒ width(c) = Some(Wref(c)): UW1 composed with S5. The 16 recorded points are instances: U+00A0, U+00B1, U+03B1, U+05D0 ↦ 1; U+0301, U+0E31, U+200B, U+200D ↦ 0; U+2500 ↦ 1; U+4E2D, U+D55C, U+FF21, U+1F600, U+20000 ↦ 2; U+E0001 ↦ 0; U+10FFFD ↦ 1. The test's doc comment implies a stronger reading, ∀ c ∉ D: width(c) = UW022(c). That would need upstream 0.2.2's tables and D as Clean defs. It is an equivalence with frozen third-party code that the crate's own doc already says is false on D, so the provable spec is Wref (§5.2).	tests/contract.rs::the_recorded_characters_answer_as_upstream_did	MEDIUM: an Option<usize>; cross-crate composition with S5 (static [u16;768] and [u8;7104] tables read by shift and mask, a match ending in unreachable!); Wref needs UCD16 EastAsianWidth.txt and the zero-width generator rule as a Clean def	0,1,3,4,5,12,13,14
```

### Distribution by kind (3)

| kind | count | ids |
|---|---|---|
| refinement | 1 | UW1-facade-refines-grapheme-width |
| contract | 1 | UW2-upstream-per-char-contract |
| table-property | 1 | UW3-facade-equals-reference-width |

### Proof dependency order

aterm-grapheme `S7` and `S5` (owned there) → `UW1` → `UW2` (needs S7, and S5 restricted to
U+0020..U+007E) → `UW3` (needs all of S5).

## 2. Test → spec map (TSV; all 3)

`primary` is the spec that retires the test. `also` lists the other specs the same test samples.

```tsv
file	test	primary	also
tests/contract.rs	controls_are_none_and_printable_ascii_is_one	UW2-upstream-per-char-contract	UW1-facade-refines-grapheme-width
tests/contract.rs	every_other_scalar_value_is_zero_one_or_two_columns	UW2-upstream-per-char-contract	UW1-facade-refines-grapheme-width (also aterm-grapheme S7)
tests/contract.rs	the_recorded_characters_answer_as_upstream_did	UW3-facade-equals-reference-width	UW1-facade-refines-grapheme-width (also aterm-grapheme S5)
```

(`tests/contract.rs` is `crates/aterm-unicode-width/tests/contract.rs`, the crate's only test file.)

## 3. Coverage

- 3 original tests read. All 3 map to one of the 3 specs. None is a non-spec check (§4).
- `UW2` retires 2 tests and `UW3` retires 1. `UW1` retires none as primary. It is the lemma both of
  the others are built on.
- Deleting all 3 tests also needs aterm-grapheme's `S5` and `S7` to be Certified. The facade proofs
  only compose with them (gap 12, cross-crate citation).

## 4. Tests that are not spec samples

None. Every test samples UW2 or UW3. One assertion inside a spec test checks the loop and not
`width`. It goes away with its test (§5.1).

| test | why | recommendation |
|---|---|---|
| (none) | | |

## 5. Implied specs that look wrong, or disagree with the code

1. **An assertion that checks the loop, not the function.** In
   `every_other_scalar_value_is_zero_one_or_two_columns`, the trailing
   `assert_eq!(answered, 1_111_904)` checks the size of the char range U+00A0..=U+10FFFF
   (0x110000 − 0xA0 − 2048 surrogates). The `char` type guarantees that count. It adds nothing to UW2.
   Drop it when the test is deleted. Do not carry it into a spec.
2. **A test labelled as upstream agreement that pins aterm's tables.**
   `the_recorded_characters_answer_as_upstream_did` says it checks agreement with unicode-width
   0.2.2. What it pins is aterm-grapheme's tables at 16 points. Upstream agreement was a measurement
   taken on 2026-10-15, and `src/lib.rs` already says it fails on 56,741 code points. The spec this
   test samples is UW3 (facade = Wref), not upstream equivalence. The test becomes redundant only
   after aterm-grapheme S5 is proved.
3. **Prose figures that nothing reproduces.** The 56,741 / 343 disagreement counts in the `src/lib.rs`
   doc comment are not reproduced by any test or spec. They will go stale silently if aterm-grapheme's
   tables move to Unicode 17 or later. Either mark them as dated (they already carry "measured
   2026-10-15") or derive them from a tool. They are not a spec.
4. **A closure that a refactor could remove.** `(!is_control(c)).then(|| …)` puts a closure on the
   proof path (gap 13). `if is_control(c) { None } else { Some(aterm_grapheme::char_width(c)) }`
   has the same meaning and removes gap 13 from all three specs. That change belongs to the proof
   commit, not to this read-only stage.

## 6. Proof order (easiest, high value first)

No spec here is blocked by gap 0 alone. Each one needs at least gaps 1 (an `Option` carrier),
3 (a call in the body) and 5 (∀ over `char`).

Gap histogram (audit §7 gap → specs it blocks): 0 → 3, 1 → 3, 3 → 3, 5 → 3, 12 → 3, 13 → 3
(0 after the §5.4 refactor), 4 → 2 (UW2, UW3), 14 → 1 (UW3).

1. **`UW1-facade-refines-grapheme-width`**. It has the smallest gap set (0,1,3,5,12,13, or 0,1,3,5,12
   after the §5.4 refactor) and the body is one line. Part (a) of UW2 (Ctrl ⇒ None) follows from it with
   no grapheme lemma, so it is the first useful step. Prove `is_control(c) ⇔ Ctrl(c)` first. It is a
   scalar range `matches!` on a `char`, like the `u8` range match the 150efe4a seal already admits.
2. **`UW2-upstream-per-char-contract`**, once aterm-grapheme S7 is Certified. It adds gap 4 (callee
   ensures). It retires 2 of the 3 tests, including the 1,111,904-iteration sweep.
3. **`UW3-facade-equals-reference-width`**, once aterm-grapheme S5 is Certified. It adds gap 14
   because Wref is a Lean-written definition over UCD16 data. It retires the last test.
