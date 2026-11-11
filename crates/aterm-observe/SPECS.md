<!-- Copyright 2026 Andrew Yates -->
<!-- SPDX-License-Identifier: Apache-2.0 -->

# aterm-observe: spec recovery from its 9 unit tests

Source: `crates/aterm-observe/src/lib.rs` (read only). It is the crate's only file, and it has 9
`#[test]` fns, all in `mod tests`. I also read the code they reach: `RowMatch`, `RowVerdict`,
`RowRange` and the `WatcherSet` scans in `crates/aterm-core/src/terminal/observe.rs`, and the
aterm-regex builder and Pike VM.

Result: **6 specs**, covering **9 tests**. **0 remaining non-spec tests** (§4).

Ownership. The `observe-*` specs are about aterm-observe code. `regex-step-budget-exact` is an
obligation on aterm-regex that this crate cites. Its proof belongs to `crates/aterm-regex/SPECS.md`
(`step-budget-contract`, `step-cost-lower-bound`, `pikevm-linear-cost`, `cut-short-sticky-shared`).
The `watch-*` specs are about aterm-core code these tests reach: they compose
`crates/aterm-core/SPECS.md` (`obs-observe-latch-step`, `obs-terminal-watch-arm-eval`) with
`observe-verdict-trichotomy`, and depend on the grid and parser specs that fix `row_text(i)` after
`process_at`.

## 0. Notation used in the statements

- Constants (from `src/lib.rs`): `MAX_REGEX_PATTERN_LEN = 1024`, `REGEX_SIZE_LIMIT = 131072` (128 KiB),
  `L = REGEX_STEP_LIMIT = 2^22`.
- `insts(p)`: the instruction count of the compiled program of `p` (aterm-regex `compile-inst-count`).
  `progsize(p) = 64·insts(p)` is the per-instruction charge against `size_limit`.
- `Parses(p)`: the aterm-regex parser accepts `p`.
- `Adm(p)` := `|p| <= 1024` ∧ `Parses(p)` ∧ `progsize(p) <= 131072`.
- `Lang(p,row)`: the semantic unanchored is-match of the compiled program of `p` on `row`, from the
  Lean regex-language model aterm-regex owns.
- `work(p,row)`: the step units the aterm-regex Pike VM `search(text, 0, true, &mut budget)` spends
  to decide `row` (`C(prog,row,0,true)` in aterm-regex's notation). It is deterministic and does not
  depend on the budget.
- `V(m,row)` = `RegexRowMatch::verdict(&m,row)` ∈ {`Match`, `NoMatch`, `Undecided`}. The `RowMatch`
  defaults give `matches(row)` ⇔ `V = Match` and `clears(row)` ⇔ `V = NoMatch`.
- `Eval(t)`: the points where a row watcher armed on Terminal `t` scans: the arm call, and each
  later observation (after a `process_at`, or a frame-hang expiry in `watch_expire`) that follows a
  surface advance (`WatcherSet::is_advance(content_seq, alt, epoch)`). A scan that falls due inside
  an unfinished frame waits for the next whole-frame observation. At a point `e`, `Rows_e` is the
  `Vec<Option<String>>` of visible-row texts: the text of every row in `WatcherSet::row_scan_hull`
  and `None` outside it, or `[]` when the hull is `None`. In the `[]` case the watcher's range `r`
  meets no grid row, so `Hit` and `Clear` are false in both shapes. `cseq_e` is the `content_seq`
  at `e`.
- `Hit(m,r,e)` := ∃ i < |Rows_e|: `r.contains(i)` ∧ `Rows_e[i] = Some(s)` ∧ `m.matches(s)`
  (`any_row_matches`).
- `Clear(m,r,e)` := `r.covers_any(|Rows_e|)` ∧ ∀ i: (`r.contains(i)` ∧ `Rows_e[i] = Some(s)`) ⇒
  `m.clears(s)` (`covers_any` ∧ `every_row_clear`).

## 1. Specs (TSV)

Columns: `id`, `kind`, `functions`, `statement`, `subsumes` (primary tests; secondary ones are in
§2), `difficulty` (the Rust features the proof must handle), `blocking_gaps` (rows of §7 of
`docs/TRUST-LANE.md` (Blocking gaps)). Gap 0 (an installed seal that publishes
Lean-discharged rows) blocks every row.

```tsv
id	kind	functions	statement	subsumes	difficulty	blocking_gaps
observe-row-matcher-admission	contract	aterm_observe::{row_matcher, bounded_regex}; aterm_regex::RegexBuilder::{new, size_limit, step_limit, build}	∀ p: &str: row_matcher(p).is_ok() ⇔ Adm(p), and it never panics. (a) Length gate: |p| > 1024 ⇒ Err(aterm_regex::Error::Syntax(_)), and RegexBuilder is never reached, because the gate comes before parsing. (b) |p| <= 1024 ⇒ the result is RegexBuilder::new(p).size_limit(131072).step_limit(2^22).build(), wrapped in Arc<RegexRowMatch> as Arc<dyn RowMatch>. Corollaries: (c) a pattern outside the grammar, e.g. "(unclosed", is Err and does not panic; (d) a pure literal of n <= 1024 bytes compiles to n+1 instructions, 64·(n+1) <= 65600 <= 131072, so it is admitted (the at-cap 1024-byte literal is Ok); (e) a short pattern with progsize > 131072, e.g. (a{200}){200}, is Err(CompiledTooBig(131072)). Negative controls: drop the .size_limit call, or raise MAX_REGEX_PATTERN_LEN, and the spec fails.	row_matcher_rejects_overlong_pattern, row_matcher_bounds_compiled_size	MEDIUM: &str len (clause (a) observes only the length), an enum error carrying a format!-built String, the ? operator, a by-value builder chain into another crate, Arc<dyn RowMatch> unsizing. It cites aterm-regex builder-knobs, parse-refusals, compile-inst-count and compile-size-ceiling, whose parser and compiler loops build a Vec program.	0, 1, 3, 4, 5, 7, 12
observe-verdict-trichotomy	refinement	aterm_observe::RegexRowMatch::verdict; aterm_core::terminal::RowMatch::{matches, clears}; RowVerdict::from(bool); aterm_regex::Regex::try_is_match	∀ m: RegexRowMatch, ∀ row: &str: V(m,row) = Match ⇔ m.re.try_is_match(row) = Ok(true); V = NoMatch ⇔ it is Ok(false); V = Undecided ⇔ it is Err(StepLimitExceeded). With the RowMatch defaults, matches(row) ⇔ V = Match and clears(row) ⇔ V = NoMatch. Hence ¬(matches ∧ clears) for every row, and a row the budget refused is neither a match nor clear: fail closed, so a refusal is never read as a match or as a non-match. The budget refuses the input, not the pattern: the same m still answers a row whose work fits.	-	MEDIUM (this crate's part): a &self trait-impl method, a match on Result, From<bool> for an enum, default trait methods compared by enum PartialEq, dyn dispatch through Arc. It composes with the aterm-regex try_is_match contract (regex-step-budget-exact).	0, 1, 2, 3, 4, 11, 12
regex-step-budget-exact	contract	aterm_regex::Regex::{try_is_match, search, step_limit_exceeded, clone}	∀ r: Regex built with step_limit L, ∀ text: r.try_is_match(text) = Err(_) ⇔ work(prog,text) > L, and Ok(b) ⇒ b = Lang(prog,text). After an Err, r.step_limit_exceeded() is true, and so is the flag on every clone sharing r's cut_short (sticky and shared). Upper bound: work(prog,text) <= (|text|+2)·(4·insts+2) + |text| (aterm-regex pikevm-linear-cost). Corollary for observe: ∀ p with Adm(p), ∀ row with (|row|+2)·(4·insts(p)+2) + |row| <= 2^22, V(row_matcher(p), row) ∈ {Match, NoMatch} and equals Lang. That is the ordinary-pattern case (the four sample patterns over the 4,200-byte row are instances). Witness: (?:x?){1020}z is 13 bytes, passes the length and size gates, and over 4,096 'x' has work > 2^22 (a checked evaluation of the VM, aterm-regex step-cost-lower-bound), so it gives Undecided and sets the flag; over "xxxz" it gives Match.	ordinary_patterns_never_reach_the_scan_budget	HARD: Pike VM loops over &str bytes and the instruction Vec, a &mut u64 budget threaded through the loop, Option/Result, a shared Arc<AtomicBool> sticky flag across Clone, and a Lean inductive regex-language model. Owned by aterm-regex; this crate cites it.	0, 1, 2, 3, 5, 7, 12, 13
watch-rows-match-latch	refinement	aterm_core::terminal::Terminal::{watch_rows, watch_poll, process_at}; observe::WatcherSet::{observe_screen, is_advance}; observe::any_row_matches; RowRange::contains	∀ t: Terminal, m, r: RowRange, arm time: after id = t.watch_rows(m,r,now) and any sequence of process_at calls, t.watch_poll(id) = Some(sat) ⇔ ∃ e ∈ Eval(t) with Hit(m,r,e). Then sat.seq = cseq_e for the first such e, and once latched it stays latched with the same seq. Arm is an evaluation point, so a surface that already matches latches at arm (level-triggered). Output that advances content without a Hit does not latch. With observe-verdict-trichotomy, a row the budget refused never makes a Hit. Any e reached by processing a byte that changed content has cseq_e >= 1.	row_matcher_latches_on_real_engine_output, row_matcher_latches_immediately_if_already_matching	VERY HARD: &mut self on Terminal and WatcherSet (two-state), a Vec of watchers with a fresh flag, Vec<Option<String>> row texts, iterator any/enumerate loops, Arc<dyn RowMatch> dispatch, the content_seq/alt/epoch clock, and the parser and grid specs that fix row_text after process_at (cross-crate). Instant/ClockReading arguments are opaque.	0, 1, 2, 3, 4, 5, 7, 10, 11, 12
watch-rows-gone-latch	refinement	aterm_core::terminal::Terminal::{watch_rows_gone, watch_poll, process_at}; observe::WatcherSet::{observe_screen, is_advance}; observe::every_row_clear; RowRange::{contains, covers_any}	∀ t, m, r: after id = t.watch_rows_gone(m,r,now) and any sequence of process_at and watch_expire calls, t.watch_poll(id) = Some(sat) ⇔ ∃ e ∈ Eval(t) with Clear(m,r,e) that is a closed frame, or a run of evaluation points all Clear lasting GONE_SETTLE. Then sat.seq = cseq_e for the first such e, and the latch is sticky. Consequences: (a) a surface with no matching row latches at arm when it is a DEC 2026 frame the program closed with nothing drawn since, or has not changed for GONE_SETTLE; (b) while some visible in-range row matches, or is Undecided because the budget refused it, every evaluation point fails Clear, so the watcher stays pending across repaints; (c) the first advancing paint after which every in-range row with text clears latches at that paint on a closed frame, and otherwise at GONE_SETTLE past it once every scan since has read Clear (a scan that fails Clear cancels it; the host's expiry latches at that computed deadline); (d) an empty range (¬covers_any) never latches.	row_gone_latches_immediately_if_nothing_matches, row_gone_waits_for_the_matching_row_to_leave, row_gone_waits_while_a_visible_row_exhausts_the_scan_budget	VERY HARD: as watch-rows-match-latch, plus a universal over the row Vec (iterator all), Option::is_none_or, and covers_any on an enum match. The Undecided case composes observe-verdict-trichotomy with regex-step-budget-exact.	0, 1, 2, 3, 4, 5, 7, 10, 11, 12
observe-wire-roundtrip	round-trip	aterm_observe::{encode_watcher_budget_wire, decode_watcher_budget_wire, encode_row_verdict_wire, decode_row_verdict_wire}	∀ (mp, sz, st): decode_watcher_budget_wire(&encode_watcher_budget_wire(mp, sz, st)) = Some((mp, sz, st)) ⇔ 1 ≤ mp ≤ MAX_REGEX_PATTERN_LEN ∧ 1 ≤ sz ≤ REGEX_SIZE_LIMIT ∧ 1 ≤ st ≤ REGEX_STEP_LIMIT, and None on wrong-length frames, non-"OBSB" magic or out-of-range caps. ∀ (v, row, plen, seq): decode_row_verdict_wire(&encode_row_verdict_wire(v, row, plen, seq)) = Some((v, row, plen, seq)), and decode_row_verdict_wire returns None on wrong-length frames, non-"OBSV" magic or verdict tag ∉ {0,1,2}.	watcher_budget_and_row_verdict_wire_roundtrip	EASY: fixed [u8; 20]/[u8; 16] frames, const fn shift/or LE helpers, enum discriminant match	0, 1, 3
```

## 2. Test → spec map (TSV)

`primary` is the spec that retires the test; `also` lists the other specs it samples.

```tsv
file	test	primary	also
crates/aterm-observe/src/lib.rs	row_matcher_latches_on_real_engine_output	watch-rows-match-latch	observe-verdict-trichotomy
crates/aterm-observe/src/lib.rs	row_matcher_latches_immediately_if_already_matching	watch-rows-match-latch	observe-verdict-trichotomy
crates/aterm-observe/src/lib.rs	row_gone_latches_immediately_if_nothing_matches	watch-rows-gone-latch	observe-verdict-trichotomy
crates/aterm-observe/src/lib.rs	row_gone_waits_for_the_matching_row_to_leave	watch-rows-gone-latch	observe-verdict-trichotomy
crates/aterm-observe/src/lib.rs	row_gone_waits_while_a_visible_row_exhausts_the_scan_budget	watch-rows-gone-latch	watch-rows-match-latch, observe-verdict-trichotomy, regex-step-budget-exact
crates/aterm-observe/src/lib.rs	row_matcher_rejects_overlong_pattern	observe-row-matcher-admission	
crates/aterm-observe/src/lib.rs	row_matcher_bounds_compiled_size	observe-row-matcher-admission	
crates/aterm-observe/src/lib.rs	ordinary_patterns_never_reach_the_scan_budget	regex-step-budget-exact	observe-verdict-trichotomy
crates/aterm-observe/src/lib.rs	watcher_budget_and_row_verdict_wire_roundtrip	observe-wire-roundtrip	
```

## 4. Tests that are not spec samples

None.

## 5. Implied specs that look wrong, or disagree with the code

2. **The budget witness rests on unstated numbers.** `row_gone_waits_while_a_visible_row_exhausts_the_scan_budget`
   depends on `REGEX_STEP_LIMIT = 2^22` and about 16.7M units for `(?:x?){1020}z` over a 4,096-column
   row. Those numbers come from comments ("measured on the m21 box"), not from a stated work function. A
   VM change in how work is charged could silently make the witness decidable. The spec defines `work()`
   in aterm-regex and makes the witness a checked evaluation.
