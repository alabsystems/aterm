<!-- Copyright 2026 Andrew Yates -->
<!-- SPDX-License-Identifier: Apache-2.0 -->

# aterm-observe: spec recovery from its 11 unit tests

> Status 2026-10-16, after the merge with the tests-and-gates audit: the rows naming 3 tests it
> deleted are gone, and the spec rows no longer cite those tests. Counts elsewhere in this file
> were taken before.

> Status 2026-10-16: this file is the crate's migration worklist. Each of the 5 specs below is to be
> proved through Trust/Clean about the compiled Rust function, and then the tests it subsumes are
> deleted. No test has been deleted yet. No spec here is Certified, and every one is blocked by more
> than gap 0 (§7 of `docs/AUDIT-trust-for-test-migration-2026-10-15.md`).

Source: `crates/aterm-observe/src/lib.rs` (read only). It is the crate's only file, and it has 11
`#[test]` fns, all in `mod tests`. I also read the code they reach: `RowMatch`, `RowVerdict`,
`RowRange` and the `WatcherSet` scans in `crates/aterm-core/src/terminal/observe.rs`, and the
aterm-regex builder and Pike VM. Nothing was compiled or run. The claims below come from reading the code.

Result: **5 specs**, covering **10 tests**. **1 test is not a spec sample** (§4).

Ownership. The `observe-*` specs are about aterm-observe code. `regex-step-budget-exact` is an
obligation on aterm-regex that this crate cites. Its proof belongs to `crates/aterm-regex/SPECS.md`
(`step-budget-contract`, `step-cost-lower-bound`, `pikevm-linear-cost`, `cut-short-sticky-shared`).
The `watch-*` specs are about aterm-core code that these integration tests reach. They compose
`crates/aterm-core/SPECS.md` (`obs-observe-latch-step`, `obs-terminal-watch-arm-eval`) with
`observe-verdict-trichotomy`. They also depend on the grid and parser specs that fix `row_text(i)`
after `process_at`.

## 0. Notation used in the statements

- Constants (from `src/lib.rs`): `MAX_REGEX_PATTERN_LEN = 1024`, `REGEX_SIZE_LIMIT = 131072` (128 KiB),
  `REGEX_DFA_SIZE_LIMIT = 2^20` (inert, §5.4), `L = REGEX_STEP_LIMIT = 2^22`.
- `insts(p)`: the instruction count of the compiled program of `p` (aterm-regex `compile-inst-count`).
  `progsize(p) = 64·insts(p)` is the per-instruction charge against `size_limit`.
- `Parses(p)`: the aterm-regex parser accepts `p`.
- `Adm(p)` := `|p| <= 1024` ∧ `Parses(p)` ∧ `progsize(p) <= 131072`.
- `Lang(p,row)`: the semantic unanchored is-match of the compiled program of `p` on `row`. This is the
  Lean regex-language model owned by aterm-regex.
- `work(p,row)`: the step units that the aterm-regex Pike VM `search(text, 0, true, &mut budget)` uses
  up to decide `row`. This is `C(prog,row,0,true)` in aterm-regex's notation. It is deterministic and
  does not depend on the budget.
- `V(m,row)` = `RegexRowMatch::verdict(&m,row)` ∈ {`Match`, `NoMatch`, `Undecided`}. Through the default
  methods of the `RowMatch` trait, `matches(row)` ⇔ `V = Match` and `clears(row)` ⇔ `V = NoMatch`.
- `Eval(t)`: the evaluation points of a watcher armed on Terminal `t`. These are the arm call itself
  (`fresh = true`), plus each later `process_at` after which `WatcherSet::is_advance(content_seq, alt,
  epoch)` holds. At a point `e`, `Rows_e` is the `Vec<Option<String>>` of visible-row texts gathered
  there: `[]` when no range the watchers scanning there read meets the grid
  (`WatcherSet::row_scan_hull` = `None`); otherwise one entry per visible row, the text of every row
  in that hull (so every in-grid row of `r`) and `None` outside it. `Hit` and `Clear` read the same in
  both shapes: an unlatched watcher scans at each of its points, so `r` is one of those ranges; in the
  `[]` case it meets none of the grid's `n` rows, so `r.covers_any(0)` and `r.covers_any(n)` are both
  false and no in-range row has text. `cseq_e` is the `content_seq` there.
- `Hit(m,r,e)` := ∃ i < |Rows_e|: `r.contains(i)` ∧ `Rows_e[i] = Some(s)` ∧ `m.matches(s)`. This is
  `any_row_matches`.
- `Clear(m,r,e)` := `r.covers_any(|Rows_e|)` ∧ ∀ i: (`r.contains(i)` ∧ `Rows_e[i] = Some(s)`) ⇒ `m.clears(s)`.
  This is `covers_any` ∧ `every_row_clear`.

## 1. Specs (TSV)

Columns: `id`, `kind`, `functions`, `statement`, `subsumes` (primary tests; the secondary ones are in
§2), `difficulty` (the Rust features the proof must handle), `blocking_gaps` (audit §7 numbers).

Gap 0 (the installed seal publishes no Lean-discharged row) blocks every row, so every row lists it.

```tsv
id	kind	functions	statement	subsumes	difficulty	blocking_gaps
observe-row-matcher-admission	contract	aterm_observe::{row_matcher, bounded_regex}; aterm_regex::RegexBuilder::{new, size_limit, dfa_size_limit, step_limit, build}	∀ p: &str: row_matcher(p).is_ok() ⇔ Adm(p), and it never panics. (a) Length gate: |p| > 1024 ⇒ Err(aterm_regex::Error::Syntax(_)), and RegexBuilder is never reached, because the gate comes before parsing. (b) |p| <= 1024 ⇒ the result is RegexBuilder::new(p).size_limit(131072).dfa_size_limit(2^20).step_limit(2^22).build(), wrapped in Arc<RegexRowMatch> as Arc<dyn RowMatch>. Corollaries: (c) a pattern outside the grammar, e.g. "(unclosed", is Err and does not panic; (d) a pure literal of n <= 1024 bytes compiles to n+1 instructions, 64·(n+1) <= 65600 <= 131072, so it is admitted (the at-cap 1024-byte literal is Ok); (e) a short pattern with progsize > 131072, e.g. (a{200}){200}, is Err even though the 10 MiB default accepts it. Negative controls: drop the .size_limit call, or raise MAX_REGEX_PATTERN_LEN, and the spec fails.	row_matcher_rejects_overlong_pattern, row_matcher_bounds_compiled_size	MEDIUM: &str len (clause (a) observes only the length), an enum error carrying a format!-built String, the ? operator, a by-value builder chain into another crate, Arc<dyn RowMatch> unsizing. It cites aterm-regex builder-knobs, parse-refusals, compile-inst-count and compile-size-ceiling, whose parser and compiler loops build a Vec program.	0, 1, 3, 4, 5, 7, 12
observe-verdict-trichotomy	refinement	aterm_observe::RegexRowMatch::verdict; aterm_core::terminal::RowMatch::{matches, clears}; RowVerdict::from(bool); aterm_regex::Regex::try_is_match	∀ m: RegexRowMatch, ∀ row: &str: V(m,row) = Match ⇔ m.re.try_is_match(row) = Ok(true); V = NoMatch ⇔ it is Ok(false); V = Undecided ⇔ it is Err(StepLimitExceeded). With the RowMatch defaults, matches(row) ⇔ V = Match and clears(row) ⇔ V = NoMatch. Hence ¬(matches ∧ clears) for every row, and a row the budget refused is neither a match nor clear: fail closed, so a refusal is never read as a match or as a non-match. The budget refuses the input, not the pattern: the same m still answers a row whose work fits.	-	MEDIUM (this crate's part): a &self trait-impl method, a match on Result, From<bool> for an enum, default trait methods compared by enum PartialEq, dyn dispatch through Arc. It composes with the aterm-regex try_is_match contract (regex-step-budget-exact).	0, 1, 2, 3, 4, 11, 12
regex-step-budget-exact	contract	aterm_regex::Regex::{try_is_match, search, step_limit_exceeded, clone}	∀ r: Regex built with step_limit L, ∀ text: r.try_is_match(text) = Err(_) ⇔ work(prog,text) > L, and Ok(b) ⇒ b = Lang(prog,text). After an Err, r.step_limit_exceeded() is true, and so is the flag on every clone sharing r's cut_short (sticky and shared). Upper bound: work(prog,text) <= (|text|+2)·(4·insts+2) + |text| (aterm-regex pikevm-linear-cost). Corollary for observe: ∀ p with Adm(p), ∀ row with (|row|+2)·(4·insts(p)+2) + |row| <= 2^22, V(row_matcher(p), row) ∈ {Match, NoMatch} and equals Lang. That is the ordinary-pattern case (the four sample patterns over the 4,200-byte row are instances). Witness: (?:x?){1020}z is 13 bytes, passes the length and size gates, and over 4,096 'x' has work > 2^22 (a checked evaluation of the VM, aterm-regex step-cost-lower-bound), so it gives Undecided and sets the flag; over "xxxz" it gives Match.	ordinary_patterns_never_reach_the_scan_budget	HARD: Pike VM loops over &str bytes and the instruction Vec, a &mut u64 budget threaded through the loop, Option/Result, a shared Arc<AtomicBool> sticky flag across Clone, and a Lean inductive regex-language model. Owned by aterm-regex; this crate cites it.	0, 1, 2, 3, 5, 7, 12, 13
watch-rows-match-latch	refinement	aterm_core::terminal::Terminal::{watch_rows, watch_poll, process_at}; observe::WatcherSet::{observe_screen, is_advance}; observe::any_row_matches; RowRange::contains	∀ t: Terminal, m, r: RowRange, arm time: after id = t.watch_rows(m,r,now) and any sequence of process_at calls, t.watch_poll(id) = Some(sat) ⇔ ∃ e ∈ Eval(t) with Hit(m,r,e). Then sat.seq = cseq_e for the first such e, and once latched it stays latched with the same seq. Arm is an evaluation point, so a surface that already matches latches at arm (level-triggered). Output that advances content without a Hit does not latch. With observe-verdict-trichotomy, a row the budget refused never makes a Hit. Any e reached by processing a byte that changed content has cseq_e >= 1.	row_matcher_latches_on_real_engine_output, row_matcher_latches_immediately_if_already_matching	VERY HARD: &mut self on Terminal and WatcherSet (two-state), a Vec of watchers with a fresh flag, Vec<Option<String>> row texts, iterator any/enumerate loops, Arc<dyn RowMatch> dispatch, the content_seq/alt/epoch clock, and the parser and grid specs that fix row_text after process_at (cross-crate). Instant/ClockReading arguments are opaque.	0, 1, 2, 3, 4, 5, 7, 10, 11, 12
watch-rows-gone-latch	refinement	aterm_core::terminal::Terminal::{watch_rows_gone, watch_poll, process_at}; observe::WatcherSet::{observe_screen, is_advance}; observe::every_row_clear; RowRange::{contains, covers_any}	∀ t, m, r: after id = t.watch_rows_gone(m,r,now) and any sequence of process_at and watch_expire calls, t.watch_poll(id) = Some(sat) ⇔ ∃ e ∈ Eval(t) with Clear(m,r,e) that is a closed frame, or a run of evaluation points all Clear lasting GONE_SETTLE. Then sat.seq = cseq_e for the first such e, and the latch is sticky. Consequences: (a) a surface with no matching row latches at arm when it is a DEC 2026 frame the program closed with nothing drawn since, or has not changed for GONE_SETTLE; (b) while some visible in-range row matches, or is Undecided because the budget refused it, every evaluation point fails Clear, so the watcher stays pending across repaints; (c) the first advancing paint after which every in-range row with text clears latches at that paint on a closed frame, and otherwise at GONE_SETTLE past it once every scan since has read Clear (a scan that fails Clear cancels it; the host's expiry latches at that computed deadline); (d) an empty range (¬covers_any) never latches.	row_gone_latches_immediately_if_nothing_matches, row_gone_waits_for_the_matching_row_to_leave, row_gone_waits_while_a_visible_row_exhausts_the_scan_budget	VERY HARD: as watch-rows-match-latch, plus a universal over the row Vec (iterator all), Option::is_none_or, and covers_any on an enum match. The Undecided case composes observe-verdict-trichotomy with regex-step-budget-exact.	0, 1, 2, 3, 4, 5, 7, 10, 11, 12
```

### Distribution by kind (5)

| kind | count | ids |
|---|---|---|
| contract | 2 | observe-row-matcher-admission, regex-step-budget-exact |
| refinement | 3 | observe-verdict-trichotomy, watch-rows-match-latch, watch-rows-gone-latch |

### Proof dependency order

aterm-regex `builder-knobs`, `compile-inst-count`, `compile-size-ceiling` → `observe-row-matcher-admission`.
aterm-regex `step-budget-contract`, `pikevm-linear-cost`, `step-cost-lower-bound`, `cut-short-sticky-shared`
→ `regex-step-budget-exact` → `observe-verdict-trichotomy`. aterm-core `obs-observe-latch-step`,
`obs-terminal-watch-arm-eval` and the grid model, together with `observe-verdict-trichotomy`, then give
`watch-rows-match-latch` and `watch-rows-gone-latch`.

## 2. Test → spec map (TSV)

`primary` is the spec that retires the test. `also` lists the other specs the same test samples. All
tests are in `crates/aterm-observe/src/lib.rs`, `mod tests`.

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
```

## 3. Coverage

- 11 tests read. 10 map to one of the 5 specs. 1 is not a spec sample (§4).
- The latch specs retire the most tests: `watch-rows-gone-latch` has 3 primary and `watch-rows-match-latch`
  has 2. They are also the hardest, and most of their proof is aterm-core's (`obs-observe-latch-step`).
  This crate owns only the composition with `observe-verdict-trichotomy`.
- `observe-row-matcher-admission` retires 3 tests. Its clause (a), the length gate, needs no regex model
  and can be proved on its own first.
- `regex-step-budget-exact` retires `ordinary_patterns_never_reach_the_scan_budget` through its work-bound
  corollary. Its proof is aterm-regex's. Until that crate's specs are Certified and citable across crates
  (gap 12), every spec here except clause (a) of admission waits on it.

## 4. Tests that are not spec samples

None. The one test this section listed, a workspace-manifest guard, is deleted.

## 5. Implied specs that look wrong, or disagree with the code

1. **`ordinary_patterns_never_reach_the_scan_budget` checks less than its doc says.** The doc says
   ordinary patterns "never touch the budget", but the test checks only `V = Match`. A match found before
   the budget runs out returns early, so the test cannot tell "never reached the budget" from "reached it
   and still matched". It never checks that `step_limit_exceeded()` is false. The four patterns are
   illustrative, not aterm's shipped vocabulary. The general spec is the work-bound corollary of
   `regex-step-budget-exact`, not this list.
2. **The budget witness rests on unstated numbers.** `row_gone_waits_while_a_visible_row_exhausts_the_scan_budget`
   depends on `REGEX_STEP_LIMIT = 2^22` and about 16.7M units for `(?:x?){1020}z` over a 4,096-column
   row. Those numbers come from comments ("measured on the m21 box"), not from a stated work function. A
   VM change in how work is charged could silently make the witness decidable. The spec defines `work()`
   in aterm-regex and makes the witness a checked evaluation.
3. **Part of a test is about another crate.** The "sanity" assertion in `row_matcher_bounds_compiled_size`,
   that the 10 MiB default accepts `(a{200}){200}`, tests `aterm_regex::RegexBuilder`, not aterm-observe
   code. It belongs to aterm-regex `compile-size-ceiling`.
4. **`REGEX_DFA_SIZE_LIMIT` is inert, but the doc counts it.** `src/lib.rs` documents it as having no
   effect (aterm-regex has no lazy DFA), yet `row_matcher`'s doc says the compile is "held to
   REGEX_SIZE_LIMIT/REGEX_DFA_SIZE_LIMIT". No test or spec can observe it (aterm-regex `builder-knobs`:
   `dfa_size_limit` is the identity). Either delete the constant and the call, or fix the doc.

## 6. Proof order (easiest high-value first)

No spec here is blocked only by gap 0. Each needs at least gaps 0, 1 and 3.

1. **Clause (a) of `observe-row-matcher-admission`, the length gate** (gaps 0, 1, 3). One length
   comparison, then an early return of an error variant. Only `&str` length is observed, so it needs the
   narrowest form of gap 1 (a length-only carrier, which already exists for Vec). It needs no regex model
   and no cross-crate citation. Corollary (d), `64·1025 = 65600 <= 131072`, is constant arithmetic and can
   be a stand-alone lemma at the same time.
2. **The rest of `observe-row-matcher-admission`** (gaps 0, 1, 3, 4, 5, 7, 12). It retires all 3 admission
   tests. It needs the aterm-regex `build` contract cited across crates (gap 12) and assumed at the call
   site (gap 4).
3. **`observe-verdict-trichotomy`** (gaps 0, 1, 2, 3, 4, 11, 12). This crate's body is one `Result` match,
   so once aterm-regex's `try_is_match` contract is citable the obligation here is small. It retires 1 test
   and is the lemma both latch specs depend on.
4. **`regex-step-budget-exact`** (gaps 0, 1, 2, 3, 5, 7, 12, 13). Prove it in aterm-regex (its Pike VM loops
   are where gap 7 bites, and the shared flag is where gap 13 bites). This crate then cites it to retire
   `ordinary_patterns_never_reach_the_scan_budget`.
5. **`watch-rows-gone-latch`, then `watch-rows-match-latch`** (gaps 0, 1, 2, 3, 4, 5, 7, 10, 11, 12). They
   retire 5 tests, but they rest on aterm-core's watcher-latch spec, the grid model under `process_at`, and
   specs 3 and 4. Do them last. Until then, keep the 5 tests, tagged "spec deferred: needs gaps 2, 5, 10, 12".
