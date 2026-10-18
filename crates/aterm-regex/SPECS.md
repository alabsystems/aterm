<!-- Copyright 2026 Andrew Yates -->
<!-- SPDX-License-Identifier: Apache-2.0 -->

# aterm-regex: spec recovery from its 86 unit tests

> Status 2026-10-16, after the merge with the tests-and-gates audit: the rows naming 22 tests it
> deleted are gone, and the spec rows no longer cite those tests. Counts elsewhere in this file
> were taken before.

> Status 2026-10-15: this is the crate's migration worklist. Each of the 44 specs below is
> proved through Trust/Clean about the compiled Rust function, and then the tests it
> subsumes are deleted. Nothing has been proved or deleted yet. The 3 tests in §4 that
> sample no spec can be acted on now.

Source: `crates/aterm-regex` (read only). I read every `#[test]`
(src/{compile 12, lib 11, parse 11, pikevm 12, unicode 6}, tests/{bounds 17, differential 16,
fuzz 1} = 86) and the code each one reaches. `tests/regex_suite.rs` has no `#[test]`; the
wave count of 87 counted a doc-comment mention there (§5.1). Nothing was compiled or run. The
claims below come from reading the code.

Result: **44 specs**, covering **83 tests**. **3 tests are not spec samples** (§4).

The `gaps` column cites the numbered rows of the gap table in
the historical development audit `docs/AUDIT-trust-for-test-migration-2026-10-15.md` §7.
Gap 0 (the installed seal publishes no Clean-discharged row) blocks every spec, so it is
listed on every row. No spec is blocked by gap 0 alone: every one needs gap 1
(non-scalar carriers), since the crate works on `&str`, `Vec` and enum trees throughout.

## 0. Notation used in the statements

- `P(p,f)` = `parse::parse(p,f)`. `Prog(ast,L)` = `compile::compile(ast,L)`. `prog` is an `Ok`
  result, with fields `insts`, `classes`, `start`, `first`, `start_anchored`, `class_ascii`.
- `Bnd(t)`: the char boundaries of `t`, a subset of `0..=|t|`. `|t|` is the length in bytes.
- `cons(prog,pc,c)`: `Char{c'}` accepts `c` iff `c = c'`; `Class{k}` accepts `c` iff
  `prog.classes[k].matches(c)`.
- `eps_A(prog,S)`: the pcs reachable from `S` through `Split` (both arms), `Jump` and `Assert`
  edges, where an `Assert` is passable iff `A(kind)`. `A = SAT(t,at)` means
  `satisfied(kind,t,at)`; `A = TOP` makes every assertion passable.
- `Lang_TOP(ast)`: the language of `ast` with every `Assert` read as epsilon.
- `Runs(prog,t,q)`: the pairs `(q,e)` such that some path from `start` at position `q` reaches
  `Match` at `e`, with consuming steps by `cons` and assertions by `SAT(t,pos)`. This is a match
  beginning at `q`.
- `BT(prog,t,s)`: the memoised leftmost-first backtracker over `prog`. It tries starts
  `q ∈ Bnd(t)`, `q ≥ s`, in ascending order, and explores depth first: `Split.a` before
  `Split.b`, `Jump`, `Assert` by `SAT(t,pos)`, consumption by `cons`. It prunes any `(pc,pos)`
  already explored, with one visited set shared across all starts, and returns the first `(q,e)`
  at which `Match` is reached, else `None`. This is the oracle the Pike VM must equal.
  **Recommendation:** add it as a spec-only function. No Rust function implements it today.
- `Ref(ast,t,s)`: the AST-level leftmost-first reference semantics, to be authored with
  `ty_model!`. `Alt` is a left-to-right preference. Greedy repeats try the body first and lazy
  ones try the exit first. The empty-iteration rule for nullable loop bodies is the regex
  crate's. The regex crate is the empirical oracle, and `\w`/`\b` follow the toolchain's
  `Alphabetic`.
- `Unb(prog,t,s,e)` = `pikevm::search(prog, any cache, t, s, e, &mut u64::MAX)`, the
  unbudgeted search (`e` = earliest).
- `C(prog,t,s,e)`: the value of `steps` at the last budget checkpoint (`if *steps > step_limit`)
  that the unbudgeted `simulate` executes. It is deterministic and does not depend on the budget.
- `Iter(f,t)`: `find_iter`'s advance rule over a search function `f`. Start with
  `(le,lm) = (0,None)`. Loop: stop if `le > |t|`. Let `(a,b) = f(le)`; stop on `None`. If `a = b`,
  set `le := b + len_utf8(the next char at b)`, or `b+1` at the end, and skip this yield if
  `lm = Some(b)`. Otherwise set `le := b`. Then set `lm := Some(b)` and yield `(a,b)`.
- `cost(prog)` = `|insts|·INST_COST + Σ_k classes[k].byte_size()`. `INST_COST` =
  `size_of Inst + 2·(size_of Thread + 4)`, which is 64 on 64-bit targets, so 128 KiB admits at
  most 2048 instructions.
- `mark(set,b)` := `(b < 0x80 ∧ set.matches(b as char)) ∨ (b ∈ 0xC2..=0xF4 ∧ (set.negated ∨ perls ≠ ∅ ∨ the last range's hi > 0x7F))`.
- `orbit(c)` = the elements `fold_orbit(c)` yields. `utf8(c)` = the UTF-8 encoding of `c`.

## 1. Specs (TSV)

Columns: `id`, `kind`, `functions`, `statement`, `subsumes (primary tests; secondary in §2)`,
`difficulty (Rust features the proof must handle)`, `gaps (audit §7 rows that block it)`.
Rows are in proof-dependency order, leaves first. Test paths are relative to
`crates/aterm-regex`.

```tsv
id	kind	functions	statement	subsumes	difficulty	gaps
assertion-semantics	contract	pikevm::satisfied, unicode::is_word	∀ t, at ∈ Bnd(t), with wb = (the char before at exists ∧ is_word) and wa = (the char at at exists ∧ is_word): StartText ⇔ at=0; EndText ⇔ at=|t|; StartLine ⇔ at=0 ∨ t[..at] ends with '\n'; EndLine ⇔ at=|t| ∨ t[at..] starts with '\n'; WordBoundary ⇔ wb≠wa; NotWordBoundary ⇔ wb=wa; WordStart ⇔ ¬wb ∧ wa; WordEnd ⇔ wb ∧ ¬wa. The haystack is never sliced at the search start, so ^ and \b at a resume point see the whole text. A combining mark is a word char, so a+U+0301 has no inner boundary.	src/pikevm.rs::assertions_read_the_surrounding_code_points	EASY: &str get(..at)/get(at..), chars().next_back()/next(), char comparison, a call to is_word (tables, via perl-class-tables)	0, 1, 3, 5
classset-byte-size	contract	parse::ClassSet::byte_size	byte_size(s) = size_of::<ClassSet>() + |s.ranges|·size_of::<(char,char)>() + |s.perls|·size_of::<PerlClass>(). So it is ≥ size_of::<ClassSet>() and strictly increasing in |ranges|. This is the class term of cost(prog) in compile-size-ceiling.	src/parse.rs::classes_report_their_size	TRIVIAL: size_of constants, Vec len, usize arithmetic	0, 1, 3
range-tables-well-formed	table-property	unicode::{ND, MARK, PC, FOLD}	For each T ∈ {ND, MARK, PC} and all i: T[i].0 ≤ T[i].1 ≤ 0x10FFFF (the last entry included) and T[i].1 + 1 < T[i+1].0, so the ranges are sorted, disjoint and non-adjacent. The FOLD keys are strictly increasing. This is the precondition of in_ranges and fold_next, and it is decidable by evaluating the static tables.	src/unicode.rs::range_tables_are_well_formed	EASY: evaluation of a static slice (decide); no fn body	0, 1, 5, 7
builder-knobs	contract	RegexBuilder::{new, size_limit, dfa_size_limit, step_limit, build}, Regex::{new, step_limit}	RegexBuilder::new(p) has size_limit = 10 MiB, step_limit = 2^26 and Flags::default(). size_limit(n) and step_limit(n) set exactly that field. dfa_size_limit(n) is the identity on the builder, so build() is independent of it. build() copies step_limit into Regex::step_limit(). Regex::new(p) = RegexBuilder::new(p).build(). The read-back get_step_limit() is #[cfg(test)]-only, so the spec states a field equality instead.	tests/bounds.rs::dfa_size_limit_is_accepted_and_inert, src/lib.rs::the_step_limit_is_the_thing_that_decides (builder half)	EASY: by-value builder methods (move self), a String field, struct update	0, 1, 2, 3
regex-metadata	contract	Regex::{as_str, clone}, <Regex as Display>::fmt, <Regex as Debug>::fmt, <Regex as FromStr>::from_str	∀ p with Regex::new(p) = Ok(re): re.as_str() = p; format!("{re}") = format!("{re:?}") = p; p.parse::<Regex>() = Regex::new(p); re.clone() shares prog and cut_short and agrees with re on every entry point and haystack. Regex: Send + Sync is checked by the type system and needs no proof.	-	EASY: Arc<str>, trait impls (Display/Debug/FromStr), Formatter write_str	0, 1, 3, 11
parse-refusals	contract	parse::Parser::{parse_escape_char, parse_class, parse_flags, open_group}	∀ p whose first failing construct is one of these, P(p,f) = Err(Syntax(msg)), never Ok: \p or \P (msg contains 'Unicode property classes'); &&, -- or ~~ inside a class ('set operations'); a nested '[' that does not start a known POSIX name ('nested character classes'); (?-u) ('byte-oriented'); (?R) ('CRLF mode'); look-around ('look-around'); \digit ('backreferences'). An escaped \-, \& or \~ is always a literal.	tests/differential.rs::refusals_are_refusals	EASY-MEDIUM: peek/peek_at lookahead over Vec<char>, String formatting into Error::Syntax	0, 1, 3, 5
parse-error-render	contract	parse::Parser::{error, err}, <Error as Display>::fmt, <StepLimitExceeded as Display>::fmt	P(p,f) = Err(e) ⇒ e = Syntax("regex parse error:\n    " ++ p ++ "\n    " ++ " "·min(pos,|chars(p)|) ++ "^\nerror: " ++ msg), where pos is the char index the failing branch sets (the opening position for unclosed groups, classes and repetitions) and msg is from a fixed finite nonempty set; so the rendering has length > 20 and contains 'error: '. Display(Syntax(s)) = s; Display(CompiledTooBig(l)) = "Compiled regex exceeds size limit of {l} bytes."; Display(StepLimitExceeded{limit}) = "regex search exceeded its step budget of {limit} units". Samples: ab[cd has its caret at 2, a(b at 1.	src/parse.rs::errors_quote_the_pattern_and_point_at_the_position	EASY-MEDIUM: format!, str repeat, usize min, Formatter write_str/write!; the 'pos' half needs the parser's cursor	0, 1, 3, 5
fail-closed-wrappers	contract	Regex::{is_match, find, try_is_match, try_find}, StepLimitExceeded::new	∀ re, t: is_match(t) = try_is_match(t).unwrap_or(false) and find(t) = try_find(t).unwrap_or(None). Every Err from try_* is StepLimitExceeded::new(re.step_limit()). try_find(t) = Ok(Some(m)) ⇒ m is the unbounded leftmost-first match (step-budget-contract (i)).	src/lib.rs::the_amplifiers_terminate_promptly_instead_of_hanging (fail-closed half)	EASY: Result/Option combinators, map over a Match with lifetimes; composition with step-budget-contract	0, 1, 3, 4, 6
thread-list-generation	invariant	pikevm::ThreadList::{resize, begin, mark}, pikevm::Cache::new	I(list): generation ≥ 1 after any begin(), and every visited[i] ≤ generation. After begin(), within one generation, mark(pc) returns true ⇔ pc ≥ |visited| or pc was already marked in this generation, and it marks pc. begin() leaves every pc < |visited| unmarked even across the u32 wrap (at generation = u32::MAX, visited is refilled with 0 and generation becomes 1). resize(n) with n ≠ |visited| clears and resizes. This is the lemma behind the 'every Cache state' of pikevm-refines-backtracker.	src/pikevm.rs::the_visited_generation_wraps_cleanly	MEDIUM: &mut self, Vec<u32> fill/resize/get_mut, u32 checked_add; a ∀-indexed Vec content invariant	0, 1, 2, 3, 5
nullable-predicate	contract	compile::matches_empty	∀ ast: matches_empty(ast) ⇔ "" ∈ Lang_TOP(ast). Equivalently, by structural recursion: Empty and Assert are nullable; Literal and Class are not; Concat iff all items are; Alt iff some branch is; Repeat iff min=0 or the body is.	-	EASY-MEDIUM: structural recursion over Box/Vec, iterator all/any	0, 1, 3, 5, 7
complement-exact	contract	parse::{complement, char_after, char_before}, parse::ClassSet::add_posix	∀ normalized range lists R over scalar values: complement(R) is normalized, and ∀ scalar c: c ∈ complement(R) ⇔ c ∉ R. char_after and char_before step over the surrogate gap D800..DFFF. Corollary: for all 14 POSIX tables T and both values of ci, add_posix(T,false,ci) and add_posix(T,true,ci) partition the scalar values, including U+D7FF, U+E000 and U+10FFFF.	src/parse.rs::a_negated_posix_item_is_the_complement_of_its_folded_table	MEDIUM: a loop over a slice with an Option<char> cursor, u32 checked_sub, char::from_u32, Vec::with_capacity/push	0, 1, 3, 5, 7
ascii-bitmap-agrees	equivalence	compile::ascii_bitmap, pikevm::class_matches, compile::compile	∀ set: ClassSet, ∀ code < 128: ((ascii_bitmap(set) >> code) & 1 = 1) ⇔ set.matches(char(code)). Hence ∀ prog = Prog(ast,L): |class_ascii| = |classes| and ∀ k < |classes|, ∀ c: class_matches(prog,k,c) = classes[k].matches(c). The bitmap is the whole answer, not a hint.	src/compile.rs::the_ascii_bitmap_agrees_with_the_class_it_stands_in_for	MEDIUM: a for-loop over 0..128, u128 shifts, char::from_u32, ClassSet::matches (binary search plus an iterator any over perls), Vec map/collect, Option is_some_and	0, 1, 3, 5, 7
match-offsets-and-accessors	invariant	Match::{start, end, as_str, range, len, is_empty}, pikevm::simulate, Matches::next	For every Match m on t produced by find, try_find or find_iter: m.start ≤ m.end ≤ |t|, both in Bnd(t); m.as_str() = &t[start..end]; len() = end − start; is_empty() ⇔ start = end; range() = start..end. Every consuming step advances by c.len_utf8() of one whole decoded code point, so '.', \w, [^z] and \W consume exactly one code point. Sample: '.' over e U+0301 U+4F60 U+597D U+1F600 z gives lengths 1,2,3,3,4,1.	tests/bounds.rs::offsets_are_byte_offsets_on_code_point_boundaries	EASY for the accessors (&str get, usize subtraction under start ≤ end); MEDIUM for the loop invariant 'pos ∈ Bnd(t)' through simulate	0, 1, 3, 5, 7
fold-orbit-structure	invariant	unicode::{fold_orbit, fold_next, FOLD}	(table) The FOLD keys are strictly increasing; ∀ (k,v) ∈ FOLD: v is a key and a scalar value, and k ↦ v is a permutation of the keys whose cycles run ascending and wrap (v > k except at a cycle's maximum, where v = the cycle's minimum). (fn) ∀ c: fold_orbit(c) terminates and yields c, then the rest of c's cycle in ascending-wrapping order, each member once; for a c that is not a key it yields [c]. ∀ m ∈ orbit(c): set(orbit(m)) = set(orbit(c)).	src/unicode.rs::fold_orbits_are_symmetric	MEDIUM: the table half is decidable; the fn half is core::iter::from_fn with a captured Option<char> (a closure state machine), binary_search_by_key, char::from_u32	0, 1, 3, 5, 7
literal-fold	contract	parse::Parser::literal, unicode::fold_orbit	Under case_insensitive: literal(c) = Literal(c) if orbit(c) = [c], else Class(S) with S.ranges = the normalized {(x,x) | x ∈ orbit(c)}, no perls, not negated. Without the flag, literal(c) = Literal(c). Hence ∀ cased c, ∀ x: (?i)X matches x ⇔ (?i)[X] matches x ⇔ x ∈ orbit(c), with X = escape(c). Samples: (?i)7 is Literal('7'); (?i)a is a Class.	tests/differential.rs::case_folding_is_pinned	MEDIUM: the from_fn iterator collected to a Vec, sort_unstable, normalize (via classset-normalize, fold-orbit-structure)	0, 1, 3, 5, 7
classset-normalize	contract	parse::ClassSet::{normalize, matches, contains_unnegated}, unicode::in_ranges	normalize(set) leaves ranges sorted with lo_i ≤ hi_i and hi_i + 1 < lo_{i+1} (overlapping and adjacent ranges coalesced); the union of the ranges is unchanged, the perls are unchanged as a set (dedup), and negated is unchanged. For normalized ranges, contains_unnegated(c) ⇔ c ∈ ∪ranges ∨ ∃ perl p: p.matches(c), and matches(c) = (negated ≠ contains_unnegated(c)). The same binary-search lemma gives in_ranges(T,c) ⇔ ∃ (lo,hi) ∈ T: lo ≤ c ≤ hi, for every well-formed T. Sample: [z-za-mn-yA] normalizes to [(A,A),(a,z)].	-	MEDIUM-HARD: sort_unstable on Vec<(char,char)>, last_mut with in-place update, Vec::dedup, binary_search_by with a closure	0, 1, 2, 3, 5, 7
fold-range-closure	contract	unicode::fold_range	∀ lo ≤ hi, out: fold_range(lo,hi,out) appends to out a list whose element set is exactly {(f,f) | f ∉ [lo,hi] ∧ ∃ c ∈ [lo,hi]: f ∈ orbit(c)}, possibly with repeats, and leaves the existing prefix of out unchanged. Visiting only the FOLD keys in [lo,hi] is enough because orbits are symmetric (fold-orbit-structure). Sample: [a-z] pulls in U+212A, U+017F and A..Z, and no lowercase ASCII.	src/unicode.rs::range_folding_reaches_outside_the_range	MEDIUM: partition_point, a while-let over slice get, nested iteration of fold_orbit, &mut Vec push	0, 1, 2, 3, 5, 7
class-denotation	contract	parse::Parser::parse_class, parse::ClassSet::{case_fold, add_posix, matches, any, any_except_newline}, parse::posix_class	For a class [^? items] parsed under flags f: matches(x) = negated XOR (x ∈ U), U = Fold_f(explicit literals and ranges) ∪ (members of the named perl classes) ∪ (the POSIX items). [:n:] denotes Fold_f(T_n) and [:^n:] denotes Complement(Fold_f(T_n)), with T_n = posix_class(n) ASCII-only for the 14 names; an unknown name is the nested-class refusal. Fold_f = closure under fold orbits when f.case_insensitive, else the identity, so folding happens before complementing at both levels (lemma: the complement of a fold-closed set is fold-closed, which makes the class-level re-fold idempotent). Perl classes are not folded. '.' is Complement({'\n'}), or every scalar value under s. Samples: (?i)[^k] rejects k, K, U+212A; (?i)[[:^lower:]] excludes a, Z, U+017F, U+212A but keeps 1 and é; [[:alpha:]] rejects é.	src/parse.rs::posix_classes_are_ascii_and_closed, tests/differential.rs::negated_posix_items_fold_before_they_negate	MEDIUM-HARD: Vec<(char,char)> append/to_vec, a bool negation flag, fold_range calls, a set-theoretic model of a range list; composes classset-normalize, complement-exact, fold-range-closure	0, 1, 3, 4, 5, 7
parse-quantifier	contract	parse::Parser::{apply_repeat, parse_counted, parse_decimal, skip_spaces}	Applied to the last item x of the current branch (no flag directive just before it): * ↦ Repeat{x,0,None}; + ↦ {1,None}; ? ↦ {0,Some(1)}; {n} ↦ {n,Some(n)}; {n,} ↦ {n,None}; {n,m} ↦ {n,Some(m)}. n and m are decimal values that fit u32 (parse_decimal is exact or errs 'too large'), and n ≤ m or 'invalid repetition count range'. Whitespace inside {...} is ignored in every mode. greedy = (¬swap_greed) XOR lazy, where lazy means a '?' immediately follows; for a counted repetition, ignorable x-mode whitespace may precede that '?', for *, + and ? it may not, so (?x)a? ? is (a?)?. Each quantifier adds one depth level.	src/parse.rs::counted_repetition_tolerates_spaces, src/parse.rs::swap_greed_inverts_and_composes	MEDIUM: &mut self cursor arithmetic, u32 checked_mul/checked_add, Vec pop/push of (Ast,usize), Box::new	0, 1, 2, 3, 5, 7
parse-flag-scoping	contract	parse::Parser::{open_group, close_group, parse_flags, apply_repeat, literal}	The flags in force at an item are those set by the nearest enclosing directive. (?F) changes the flags for the rest of the enclosing group (to its ')' or the end); (?F:...) applies F to its body only; ')' restores the flags saved at the matching '('; (?-F) clears. A quantifier right after a directive (?F) has no operand: 'repetition operator missing expression' (a(?i)*, x(?s){2}, a(?i)(?s)*, a(?x) *). An item after the directive, a group included, is repeatable as usual (a(?i)b* on aBB gives (0,3)). Samples: (?i:a) is a Class; (?i)(?-i)a is Literal('a'); in (a(?i)b)c only b is folded.	tests/differential.rs::a_quantifier_after_a_flag_directive_is_refused	MEDIUM: a Copy Flags struct saved and restored on a Vec stack, a bool after_flag_directive state, Vec<char> seen for duplicate flags	0, 1, 2, 3, 5, 7
parse-nesting-cap	contract	parse::Parser::{push, apply_repeat, open_group, close_group}, parse::ast_depth	∀ p: P(p) = Err('... nests more than 250 levels deep') ⇔ some item's recorded depth plus the number of groups open around it exceeds MAX_NESTING_DEPTH = 250, where groups and stacked quantifiers each add one; a group is also refused when it would be the 251st open group. P(p) = Ok(ast) ⇒ ast_depth(ast) ≤ MAX_NESTING_DEPTH + 2. Samples: 249 or 200 nested groups around a are accepted and still match (find on xax gives 1..2); 251, 300, 5,000 and 100,000 are refused, and so are a followed by 251 or 100,000 stars.	tests/bounds.rs::deeply_nested_patterns_are_rejected_rather_than_overflowing	MEDIUM: a depth accumulator over a Vec stack, the iterative ast_depth DFS over Vec<(&Ast,usize)>, usize comparisons	0, 1, 2, 3, 5, 7
compile-inst-count	contract	compile::compile, compile::Compiler::{emit, emit_repeat, push, add_class}, compile::split	∀ ast with Prog(ast,usize::MAX) = Ok(prog): insts[0] = Match; every next/a/b and start is < |insts|; no Jump remains; |insts| = 1 + n(ast), with n(Empty)=0; n(Literal)=n(Class)=n(Assert)=1; n(Concat xs)=Σ n(x); n(Alt bs)=Σ n(b) + |bs|−1; n(Repeat{x,min,Some(max)}) = (max−min)·(n(x)+1) + (n(x)=0 ? 0 : min·n(x)); n(Repeat{x,min,None}) = 1 + n(x) + (min>0 ? (n(x)=0 ? 0 : (min−1)·n(x)) : [matches_empty(x)]). The same recursion counting only Split gives #Split: 1 for x* with non-nullable x, 2 + #Split(x) for nullable x, and 1 for x+, x?, x{2,}. |classes| is the number of Class leaves times their expansion multiplicity.	-	HARD: recursion over a Box/Vec enum tree, loops with early break (the fixed point), &mut self with Vec push and index-assign, u32 saturating_sub, Result ? propagation	0, 1, 2, 3, 5, 7
compile-greed-arm-swap	contract	compile::split, compile::Compiler::emit_repeat	∀ ast, and ast' = ast with one Repeat node's greedy flag flipped: Prog(ast') equals Prog(ast) instruction for instruction, except that every Split emitted through split(body,exit,greedy) for that node has its a and b arms swapped. Greediness affects nothing else. Sample: insts[start] of x* and x*? are Splits with swapped arms.	-	MEDIUM-HARD: a two-run relational property over the compile recursion	0, 1, 3, 5, 7
compile-size-ceiling	contract	compile::compile, compile::Compiler::{push, add_class}, RegexBuilder::{size_limit, build}, parse::ClassSet::byte_size	∀ ast, L: Prog(ast,L) = Ok(prog) ⇔ cost(Prog(ast,usize::MAX)) ≤ L, and then prog = Prog(ast,usize::MAX); otherwise Prog(ast,L) = Err(CompiledTooBig(L)). (cost only grows and is re-checked after every push and add_class.) Hence RegexBuilder::new(p).size_limit(L).build() = Err(CompiledTooBig(L)) ⇔ P(p) is Ok ∧ cost > L. With compile-inst-count this gives the samples: (a{200}){200} (40,001 insts, between 1 MiB and 10 MiB) is accepted by default and refused at 128 KiB; (a{50}){50} fits 1 MiB but not 1 KiB; 1024 literals and 100 alternations fit 128 KiB; the amplifiers (?:x?){2000}z and (?:x|...){400}z exceed 128 KiB.	tests/bounds.rs::{the_amplifier_pattern_is_rejected_under_the_bounded_builder, a_pattern_at_the_length_ceiling_still_compiles, the_size_limit_is_the_thing_that_decides, the_worst_program_under_the_ceiling_still_scans_a_row_promptly}	HARD: usize multiply/compare (overflow of insts.len()·INST_COST), a monotone accumulator across recursion, Result early exit, ClassSet clone	0, 1, 2, 3, 4, 5, 7
compile-terminates-linearly	other (complexity)	compile::Compiler::{emit, emit_repeat}	∀ ast, L: Prog(ast,L) terminates, and the number of emit invocations is ≤ size(ast)·(E+1), where E = the instructions actually pushed, E ≤ L/INST_COST + 1. Proof idea: every iteration of the {n,m} optional loop pushes a Split, and the mandatory-copy loops break at the first copy that emits nothing (copy = entry ⇔ nothing was pushed, since push returns len−1, which exceeds every earlier index), so work beyond what is emitted is independent of the repeat count. Examples: (?:){4294967295}, (?:(?:){N}){N}, (){N}, (?:a{0}){N} and (?:\b{0}){N} emit only Match; a{4000000000}, (?:ab){999999999}, a{1,4000000000} and (?:a?){4294967295} stop at the ceiling after ≤ L/INST_COST pushes.	tests/bounds.rs::an_empty_body_repeat_collapses_instead_of_spinning, tests/bounds.rs::an_unbounded_repetition_count_fails_fast_instead_of_allocating	HARD: loop variants with early break, a recursion measure over the AST, the 'returns its argument iff nothing was pushed' Vec-len lemma	0, 1, 2, 3, 5, 7
start-anchored-exact	contract	compile::start_anchored, pikevm::simulate	For prog = Prog(ast,usize::MAX): start_anchored(insts,start) ⇔ every path from start along eps_TOP edges that reaches a Char, Class or Match passes an Assert{StartText} first, and every reached pc is in range. Soundness: start_anchored ⇒ ∀ t, q, e: (q,e) ∈ Runs(prog,t,q) ⇒ q = 0; so BT(prog,t,s) = None for s > 0, and the simulate early exits (start > 0; pos > 0 with an empty clist) do not change Unb. Anchored: ^a, ^, ^$, \A\Aa, ^a*, ^a|^b, (?i)^ERROR. Not anchored: a|^b, ^a|b, (^|x)y, (?:^)|a, (^a)*, (?:^a)?b, (?m)^a, the empty pattern, a$.	src/compile.rs::start_anchored_is_exact_about_which_patterns_can_only_match_at_zero	MEDIUM-HARD: worklist DFS with a Vec<bool> seen set, slice get_mut, an or-pattern match on enum variants; soundness needs Runs	0, 1, 2, 3, 5, 7
prefilter-sets	contract	compile::{first_set, second_set, consuming_closure, mark_leads}, parse::ClassSet::mark_lead_bytes	For prog = Prog(ast,usize::MAX), K = the Char/Class pcs in eps_TOP(prog,{start}). first = None ⇔ Match ∈ eps_TOP(prog,{start}) ∨ |K| > 16 ∨ K = ∅. Otherwise first = Some(F) with set(F.pcs) = K, and F.lead_bytes[b] ⇔ some pc ∈ K is Char c with b = utf8(c)[0] or is Class k with mark(classes[k],b). mark_lead_bytes(set,out) ORs exactly mark(set,·) into out. Consequences: no byte in 0x80..=0xC1 or 0xF5..=0xFF is marked, and ∀ pc ∈ K, ∀ c with cons(prog,pc,c): lead_bytes[utf8(c)[0]]. With N = {next(pc) | pc ∈ K} and K2 the consuming closure of N: second_bytes = Some(S) ⇔ Match ∉ eps_TOP(prog,N) ∧ 0 < |K2| ≤ 16, with S the same marking over K2; so Some(S) ⇒ every accepting path from start consumes ≥ 2 code points. Samples: ERROR ↦ {R}; (?i)ERROR ↦ {R,r}; ab|cd ↦ {b,d}; E, Ex?, Ex*, E|Ex, E.*, E\b ↦ None; a*, the empty pattern, (?:)|a and \b disarm; 40 alternatives disarm.	src/compile.rs::{the_second_byte_filter_arms_only_when_a_second_code_point_is_certain, the_prefilter_byte_map_marks_lead_bytes_only}	HARD: worklist loop with a Vec<bool> seen set, slice get/get_mut with ?, a [bool;256] array, char::encode_utf8 into [u8;4], Box<[usize]>, Option chaining	0, 1, 2, 3, 5, 7
earliest-preserves-existence	contract	pikevm::simulate, Regex::{try_is_match, is_match}	∀ prog, t, s ∈ Bnd(t): Unb(prog,t,s,true).is_some() ⇔ Unb(prog,t,s,false).is_some(), and a Some((q,e)) from the earliest search is an element of Runs(prog,t,q). Hence, when neither search is exhausted, try_is_match(t) = Ok(x) with x ⇔ try_find(t) = Ok(Some(_)).	-	HARD: a two-run relation over one loop with an early return	0, 1, 2, 3, 5, 7
prefilter-transparent	equivalence	pikevm::{skip_to_candidate, simulate}, pikevm::ThreadList::begin	(a) ∀ prog with first = Some(F), t, pos ∈ Bnd(t): p' = skip_to_candidate(prog,F,t,pos) satisfies pos ≤ p' ≤ |t|, p' ∈ Bnd(t), and no match begins in the skipped range: ∀ boundary q with pos ≤ q < p', Runs(prog,t,q) = ∅. Continuation bytes are never marked, so the walk only stops on a lead byte; the second-byte test excludes only on positive evidence. (b) Hence ∀ t, s, e, caches: Unb(prog,t,s,e) = Unb(prog[first:=None],t,s,e). This includes retiring the visited generation (clist.begin()) after a skip, so marks left by a failing mid-pattern assertion (ab\Bc, x\B\d, \-?\B\d) cannot deduplicate the new start thread.	tests/differential.rs::the_prefilter_never_changes_the_answer	HARD: a while-let byte loop over as_bytes with get, str get(pos..) plus chars().next(), len_utf8, closures over Box<[usize]>, &mut Cache with generation stamps; needs prefilter-sets and thread-list-generation	0, 1, 2, 3, 4, 5, 7
pikevm-refines-backtracker	equivalence	pikevm::{search, simulate, add}, pikevm::ThreadList::{resize, begin, mark}, Regex::search	∀ ast, prog = Prog(ast,usize::MAX), t, s ∈ Bnd(t), and every Cache state (any prior use, any prior program size): search(prog,cache,t,s,false,&mut u64::MAX) = Ok(BT(prog,t,s)). The thread list is ordered, the closure is a depth-first walk taking a before b, the start thread is appended last, and lower-priority threads are cut when one reaches Match; the epsilon closure terminates on cycles because of mark. Regex::search returns the same value whether it uses the thread-local cache or the fresh-Cache fallback, so results never depend on which pattern or haystack last used the cache.	src/pikevm.rs::a_cache_survives_being_resized_between_programs, tests/bounds.rs::the_scratch_cache_is_reused_without_leaking_state	VERY HARD: the main loop with mem::swap of two &mut ThreadLists, an explicit-stack DFS over Vec<usize>, u32 generation stamps, &str slicing and char decode, saturating u64, thread_local! RefCell try_with/try_borrow_mut (the cache fallback)	0, 1, 2, 3, 4, 5, 7, 13
step-budget-contract	contract	pikevm::{search, simulate}, Regex::{try_find, try_is_match, search}	∀ prog, t, s, e, cache, b: let r = search(prog,cache,t,s,e,&mut b) leave b'. (i) r = Ok(x) ⇒ x = Unb(prog,t,s,e): no partial span is ever returned. (ii) r = Err(Exhausted) ⇔ C(prog,t,s,e) > b, so the outcome is monotone in b (Ok at b ⇒ Ok at every b'' ≥ b) and u64::MAX always answers. (iii) b' = b − spent (saturating), with spent ≥ positions visited + bytes skipped by the prefilter + epsilon pcs entered + threads stepped, and spent ≤ C + one position's work. (iv) At the API: re.try_find(t) = Err(StepLimitExceeded::new(re.step_limit())) ⇔ C(prog,t,0,false) > re.step_limit(), else Ok(the unbounded find); likewise try_is_match with e = true. Samples: a+b over 10,000 a's is Ok(None) unbounded and Err at 100 whether or not a b follows; needle over 100,000 x's is Err at 1,000 because skipped bytes are charged; ordinary patterns give identical spans at 2^16, the default and u64::MAX.	src/pikevm.rs::{the_step_budget_stops_the_simulation, the_prefilter_skip_is_charged_by_the_byte}, src/lib.rs::{exhaustion_never_masquerades_as_no_match, the_step_limit_is_the_thing_that_decides, ordinary_patterns_are_unaffected}	HARD: &mut u64 in/out, saturating_add/sub, u64::try_from(usize), Result, a ghost counter relating the budgeted and unbudgeted runs of one loop	0, 1, 2, 3, 4, 5, 7
step-cost-lower-bound	contract	pikevm::{simulate, add}	∀ prog with first = None and ¬start_anchored, ∀ t, s with Unb(prog,t,s,false) = None: C(prog,t,s,false) ≥ Σ_{pos ∈ Bnd(t), pos ≥ s} entered(add(start,pos)) ≥ |Bnd(t) ∩ [s,|t|]|·m, with m = the minimum over pos of the epsilon pcs add(start) enters. Corollary for (?:x?){2000}z (first = None because |K| > 16; m ≥ 4001): on 4,096 x's C > 2^22, so the call-site budget refuses it, as do budgets of 1,000 and 10,000; on 3 MiB of x's C > 2^26, so the default refuses it. The API then reports Err(StepLimitExceeded::new(limit)), and is_match fails closed (fail-closed-wrappers).	src/lib.rs::the_amplifiers_terminate_promptly_instead_of_hanging, src/lib.rs::the_default_budget_stops_the_runaway_scan	HARD: a summation lower bound over loop iterations, the count of add's DFS pops	0, 1, 2, 3, 5, 7
pikevm-linear-cost	other (complexity)	pikevm::{simulate, add, skip_to_candidate}	∀ prog, t, s, e: C(prog,t,s,e) ≤ (|t|−s+2)·(4·|insts|+2) + |t|. Per position: 1 arrival unit, ≤ |insts| threads stepped, ≤ (#add calls + 2|insts|) epsilon pops into each list per generation, because mark admits each pc once; prefilter skips total ≤ |t|. The search terminates for every pattern, including (a|a)*b, (a*)*b, (a+)+b, (a|aa)+b, (?:a?){200}a{200} and (a+)+b+ over a^n b^n. With compile-size-ceiling (≤ 2048 insts at 128 KiB): every program admitted at 128 KiB has C ≤ 4098·8194 + 4096 < 2^26 = DEFAULT_STEP_LIMIT on any haystack of ≤ 4,096 bytes.	tests/bounds.rs::pathological_patterns_finish_promptly, tests/bounds.rs::adversarial_matching_input_is_also_linear, src/lib.rs::the_worst_admitted_program_still_fits_the_default_budget	HARD: a cost model over nested loops, an amortised mark argument over the generation-stamped Vec<u32>, usize/u64 arithmetic	0, 1, 2, 3, 5, 7
find-iter-advance-rule	contract	Matches::next, Regex::find_iter	∀ re, t, when no search is exhausted: the spans of re.find_iter(t) are exactly Iter(λs. Unb(prog,t,s,false), t). Hence: yields are non-overlapping and left to right (a_{i+1} ≥ b_i); no two consecutive yields end at the same offset (so a* on aab gives no empty match at 2 after aa); the empty pattern yields one empty match at every char boundary; the iteration ends after ≤ |t|+1 empty steps plus the non-empty matches; and the iterator is fused. Samples: \b on 'ab cd' gives 0, 2, 3, 5; a(?i)(?:)* on aa gives (0,1), (1,2).	tests/differential.rs::zero_width_matches_are_pinned	MEDIUM-HARD: Iterator::next on &mut self, str get(end..).chars().next(), len_utf8, map_or, a loop with continue, Option<usize> state	0, 1, 2, 3, 5, 7
find-iter-shared-budget	contract	Regex::{find_iter, find_iter_with_limit}, Matches::{next, step_limit_exceeded}	∀ re, t, L, with find_iter(t) = find_iter_with_limit(t, re.step_limit()): the iterator's searches draw on one budget that starts at L and is decremented by each search's spent units. It yields the longest prefix of Iter(Unb) whose searches all return Ok under the remaining budget. At the first Err it returns None, sets Matches::step_limit_exceeded() = true, sets re's sticky flag and sets last_end = |t|+1, so every later next() is None with no further search; if no search errs, step_limit_exceeded() = false at the end. The budget belongs to the iterator: independent of re.step_limit() when L is passed, and of every earlier iterator. Samples: ab|(?:x?){2000}z at 200,000 yields 0..2, 3..5, 6..8, then truncates and stays fused; \w+=|\w over 4,096 word chars at 2^22 truncates (a quadratic iteration) while \w yields all n; needle at the bisected budget L finishes and at L/2 does not, whatever re's own limit is.	src/lib.rs::{find_iter_reports_truncation_and_fuses, a_find_iter_is_held_to_one_budget_not_one_per_match (spec half), find_iter_with_limit_spends_the_budget_it_is_handed}	HARD: a struct with &'r Regex and &'t str lifetimes, a &mut self.budget threaded through calls, FusedIterator, saturating_add; composes step-budget-contract and find-iter-advance-rule	0, 1, 2, 3, 4, 5, 7
cut-short-sticky-shared	invariant	Regex::{search, step_limit_exceeded, clone}, RegexBuilder::build	∀ re: re.step_limit_exceeded() ⇔ some search through any entry point (is_match, find, try_*, find_iter*) on re, or on any clone sharing its Arc<AtomicBool>, has returned Err. It is false right after build(), only goes false → true, and is never reset; clones observe each other's exhaustion. Matches::step_limit_exceeded is per iterator and is not this flag.	src/lib.rs::the_cut_short_flag_is_shared_with_clones	HARD: Arc<AtomicBool> with Relaxed load/store, derive(Clone) sharing an Arc, aliasing across clones, a history property (a ghost log of searches)	0, 1, 2, 4, 5, 8, 13
api-consistency	equivalence	Regex::{find, is_match, find_iter}	∀ re, t, when no search is exhausted: re.find(t) = re.find_iter(t).next(); re.is_match(t) ⇔ re.find(t).is_some() ⇔ find_iter(t) is non-empty. The first find_iter search starts at 0 with last_match = None, so it is never skipped.	(secondary only; see §2)	EASY once its parts are proved: composition of earliest-preserves-existence and find-iter-advance-rule; the Iterator trait	0, 1, 2, 3, 4, 5, 7
escape-literal	round-trip	escape, parse::parse	∀ s: escape(s) is s with '\' inserted before every char in META = \.+*?()|[]{}^$#&-~ and before every whitespace char. ∀ f with case_insensitive = false (ignore_whitespace included): P(escape(s),f) = Ok(lit(s)), where lit is Empty, a single Literal, or a Concat of Literal(c) for c in s. Hence ∀ t: Regex::new(escape(s))?.find(t) is the first occurrence of s in t, and Some(0..0) for s = "". '<' and '>' are not escaped and stay literals.	tests/differential.rs::escape_round_trips	MEDIUM for escape alone (String::with_capacity/push, str::contains(char), char::is_whitespace); HARD for the round trip (parse lemmas for '\' + a non-alphanumeric char, then the end-to-end find)	0, 1, 3, 4, 5, 7
no-panic-total	no-panic	parse::parse, compile::compile, pikevm::search, RegexBuilder::build, Regex::{find, is_match, try_find, try_is_match, find_iter, find_iter_with_limit}, Matches::next, escape, drop_in_place::<Ast>	∀ p, f, L, t, s ∈ Bnd(t), b, caches: these calls terminate and do not panic or index out of bounds, and their Rust stack depth is bounded by a constant independent of p and t: parse, the closures and compile's analyses are iterative; emit recursion and Ast Drop recursion are bounded by ast_depth ≤ 252 (parse-nesting-cap). So a 256 KiB stack survives 249, 250, 251 and 10,000 nested (?: groups.	tests/bounds.rs::nesting_is_bounded_on_a_small_stack	HARD: recursive Drop of Box/Vec trees, the recursion depth of emit (a stack-size model), every slice get, usize +1 arithmetic (pos+1, end+len_utf8), unwrap_or fallbacks	0, 1, 3, 5, 7, 8
perl-class-tables	table-property	unicode::{is_digit, is_space, is_word, in_ranges, ND, MARK, PC}, parse::PerlClass::matches	∀ scalar c: is_digit(c) ⇔ GC(c) = Nd; is_space(c) ⇔ White_Space(c) (= char::is_whitespace); is_word(c) ⇔ Alphabetic(c) ∨ GC(c) ∈ {Mn, Mc, Me, Nd, Pc} ∨ c ∈ {U+200C, U+200D}. The ASCII fast paths agree with the table paths for c < 0x80. PerlClass::matches maps D/S/W to these predicates and their negations. ND, MARK and PC equal the UCD sets, which needs a UCD model because std exposes none of them; Alphabetic is std's char::is_alphabetic and moves with the toolchain.	src/unicode.rs::perl_classes_are_unicode_aware, tests/differential.rs::perl_classes_are_pinned	MEDIUM given a UCD model: static range tables, binary search (classset-normalize's lemma), a loop over 0x110000 values, std char predicates as opaque externs	0, 1, 3, 5, 7, 8
fold-table-derivation	equivalence	unicode::{FOLD, fold_orbit}	FOLD = sort{(c, succ_O(c)) | O an orbit, |O| ≥ 2, c ∈ O}. The orbits are the connected components of the graph on scalar values joining c to single_lower(c) and single_upper(c) (std's single-code-point to_lowercase/to_uppercase), with U+0131 excluded and the edges (U+0390,U+1FD3), (U+03B0,U+1FE3), (U+FB05,U+FB06) added; succ_O(c) is the next larger member of O, wrapping to its minimum. The std-free equivalent is the orbits of Unicode CaseFolding.txt simple folding (C+S, not T). Samples: k ~ K ~ U+212A; s ~ S ~ U+017F; the three sigmas; ß ~ U+1E9E; i ~ I; U+0131 and 7 stand alone.	src/unicode.rs::fold_table_matches_std, src/unicode.rs::named_orbits	HARD: a 2,994-entry static table, a loop over 0x110000 code points, char::to_lowercase/to_uppercase (needs a std/UCD model), union-find over BTreeMap	0, 1, 3, 5, 7, 8
parse-grammar-denotation	refinement	parse::parse, parse::Parser::{run, open_group, close_group, parse_escape, parse_escape_char, parse_hex, parse_class, parse_posix_class, parse_group_name, skip_ignorable}, parse::{take_concat, finish_alts}	P(p,f) is total. P(p,f) = Ok(ast) ⇔ p ∈ L(G), the documented grammar, and then ast = [[p]]_f. G covers literals, '.', classes, the escapes (\a\f\t\n\r\v, \xNN, \x{..}, \u, \U up to U+10FFFF with no surrogates, any non-alphanumeric char), perl and POSIX classes, the assertions ^ $ \A \z \b \B \< \>, groups (capturing; (?:; named <n> or P<n>, names unique, starting with a letter or _ and continuing with alphanumerics or _.[]), the flags imsxU and u, alternation and the quantifiers. x-mode: whitespace and #-to-newline are ignorable, including between '(' and '?' and between '[' and '^'. Otherwise P(p,f) = Err(Syntax(_)), including: unclosed group or class; unopened ')'; {,3}, {}, {b}, a{; a quantifier with no operand; trailing '\'; unknown escape (\q, \Z, \Q); \b inside a class; \1; look-around; unknown or duplicate flags ((?z), (?i-i)); [] and [^]; \x{110000} and \uD800; a shorthand as a range endpoint ([\d-a], [a-\d]); (?P=n); bad or duplicate group names.	tests/differential.rs::malformed_patterns_are_rejected, tests/differential.rs::extended_mode_is_pinned	VERY HARD: a flat loop over Vec<char> with an explicit Vec<Group> stack, &mut self, String building, u32::from_str_radix, char::from_u32, mem::take, many-armed matches with guards; G must be authored as a ty_model! grammar	0, 1, 2, 3, 5, 7, 10
compile-refines-reference	refinement	compile::compile, compile::Compiler::{emit, emit_repeat}, compile::matches_empty	∀ ast produced by parse, t, s ∈ Bnd(t): BT(Prog(ast,usize::MAX),t,s) = Ref(ast,t,s). This covers Alt as a preference (a|ab on xab gives (1,2), ab|a gives (1,3), (?:a|ab)c on abc gives (0,3)); the leftmost start winning over priority; the two star shapes chosen by matches_empty ((|a)* gives the empty match, (a|)* is greedy, ()* and (?:a*)*b terminate, (.*?){2,}\b stops at the first boundary); lazy and greedy counted repeats; and the leftmost start of (.*b)*é-shaped patterns.	tests/differential.rs::{nullable_and_lazy_repetitions_are_pinned, this_engine_finds_the_leftmost_start}	VERY HARD: recursion over the AST against a ty_model! reference, Vec-indexed NFA graphs, a simulation relation between AST positions and pcs	0, 1, 3, 4, 5, 7, 10
regex-reference-semantics	refinement	Regex::{new, find, is_match, find_iter}, parse::parse, compile::compile, pikevm::search	∀ p with Regex::new(p) = Ok(re), ∀ t, when no search is exhausted: [(m.start,m.end) | m ← re.find_iter(t)] = Iter(λs. Ref(P(p,default),t,s), t); re.find(t) is its head, and re.is_match(t) ⇔ it is non-empty. This is the composition of parse-grammar-denotation, compile-refines-reference, pikevm-refines-backtracker, prefilter-transparent, start-anchored-exact and find-iter-advance-rule. The corpora (the builtin selection rules, the syntax corpus, the exotic orbits and the generated fuzz grammar) pin Ref's agreement with the regex crate. That agreement is empirical, so once Ref is a ty_model! these digests become Ref's conformance test with a negative control, not tests to delete (§5.6).	tests/differential.rs::{builtin_selection_patterns_are_pinned, builtin_patterns_select_what_they_are_for, syntax_corpus_is_pinned, exotic_fold_orbits_are_pinned}, tests/fuzz.rs::generated_patterns_are_pinned	FREE once its parts are proved, but the parts include the hardest specs here; Arc<Program>, lifetimes on Match/Matches, the thread_local cache	0, 1, 2, 3, 4, 5, 7, 10, 12, 13
```

### Proof dependency order

Table properties (`range-tables-well-formed`, the table half of `fold-orbit-structure`) and the
leaf contracts (`classset-byte-size`, `builder-knobs`, `assertion-semantics`,
`thread-list-generation`, `nullable-predicate`, `complement-exact`) →
`classset-normalize`, `fold-range-closure`, `literal-fold` → `class-denotation`,
`ascii-bitmap-agrees` → the parser contracts (`parse-quantifier`, `parse-flag-scoping`,
`parse-nesting-cap`, `parse-refusals`, `parse-error-render`) → the compiler contracts
(`compile-inst-count` → `compile-size-ceiling`, `compile-terminates-linearly`,
`compile-greed-arm-swap`, `start-anchored-exact`, `prefilter-sets`) → the VM
(`pikevm-refines-backtracker` with `prefilter-transparent`; then `earliest-preserves-existence`,
`step-budget-contract`, `pikevm-linear-cost`, `step-cost-lower-bound`) → the API
(`fail-closed-wrappers`, `find-iter-advance-rule`, `find-iter-shared-budget`,
`cut-short-sticky-shared`, `match-offsets-and-accessors`, `api-consistency`) → the refinements
(`parse-grammar-denotation`, `compile-refines-reference`) → `regex-reference-semantics`.
`no-panic-total`, `escape-literal`, `perl-class-tables` and `fold-table-derivation` sit beside
this chain: the last two need a UCD model (gap 8), not the other specs.

## 2. Test → spec map (TSV)

`primary` is the spec that retires the test. `also` lists the other specs the same test
samples. `— (§4)` marks a test that samples no spec.

```tsv
file	test	primary	also
src/compile.rs	start_anchored_is_exact_about_which_patterns_can_only_match_at_zero	start-anchored-exact	
src/compile.rs	the_ascii_bitmap_agrees_with_the_class_it_stands_in_for	ascii-bitmap-agrees	
src/compile.rs	the_prefilter_byte_map_marks_lead_bytes_only	prefilter-sets	
src/compile.rs	the_second_byte_filter_arms_only_when_a_second_code_point_is_certain	prefilter-sets	
src/lib.rs	a_find_iter_is_held_to_one_budget_not_one_per_match	find-iter-shared-budget	step-budget-contract
src/lib.rs	every_builtin_selection_rule_stays_far_inside_the_default	— (§4)	
src/lib.rs	exhaustion_never_masquerades_as_no_match	step-budget-contract	fail-closed-wrappers
src/lib.rs	find_iter_reports_truncation_and_fuses	find-iter-shared-budget	cut-short-sticky-shared
src/lib.rs	find_iter_with_limit_spends_the_budget_it_is_handed	find-iter-shared-budget	
src/lib.rs	ordinary_patterns_are_unaffected	step-budget-contract	regex-reference-semantics
src/lib.rs	the_amplifiers_terminate_promptly_instead_of_hanging	step-cost-lower-bound	fail-closed-wrappers, compile-size-ceiling
src/lib.rs	the_cut_short_flag_is_shared_with_clones	cut-short-sticky-shared	
src/lib.rs	the_default_budget_stops_the_runaway_scan	step-cost-lower-bound	
src/lib.rs	the_step_limit_is_the_thing_that_decides	step-budget-contract	builder-knobs
src/lib.rs	the_worst_admitted_program_still_fits_the_default_budget	pikevm-linear-cost	compile-size-ceiling
src/parse.rs	a_negated_posix_item_is_the_complement_of_its_folded_table	complement-exact	class-denotation
src/parse.rs	classes_report_their_size	classset-byte-size	
src/parse.rs	counted_repetition_tolerates_spaces	parse-quantifier	
src/parse.rs	errors_quote_the_pattern_and_point_at_the_position	parse-error-render	
src/parse.rs	posix_classes_are_ascii_and_closed	class-denotation	
src/parse.rs	swap_greed_inverts_and_composes	parse-quantifier	
src/pikevm.rs	a_cache_survives_being_resized_between_programs	pikevm-refines-backtracker	thread-list-generation
src/pikevm.rs	assertions_read_the_surrounding_code_points	assertion-semantics	pikevm-refines-backtracker
src/pikevm.rs	the_prefilter_skip_is_charged_by_the_byte	step-budget-contract	
src/pikevm.rs	the_step_budget_stops_the_simulation	step-budget-contract	
src/pikevm.rs	the_visited_generation_wraps_cleanly	thread-list-generation	
src/unicode.rs	fold_orbits_are_symmetric	fold-orbit-structure	range-tables-well-formed
src/unicode.rs	fold_table_matches_std	fold-table-derivation	
src/unicode.rs	named_orbits	fold-table-derivation	
src/unicode.rs	perl_classes_are_unicode_aware	perl-class-tables	
src/unicode.rs	range_folding_reaches_outside_the_range	fold-range-closure	fold-orbit-structure
src/unicode.rs	range_tables_are_well_formed	range-tables-well-formed	
tests/bounds.rs	a_pattern_at_the_length_ceiling_still_compiles	compile-size-ceiling	compile-inst-count
tests/bounds.rs	adversarial_matching_input_is_also_linear	pikevm-linear-cost	
tests/bounds.rs	an_empty_body_repeat_collapses_instead_of_spinning	compile-terminates-linearly	
tests/bounds.rs	an_unbounded_repetition_count_fails_fast_instead_of_allocating	compile-terminates-linearly	
tests/bounds.rs	deeply_nested_patterns_are_rejected_rather_than_overflowing	parse-nesting-cap	no-panic-total
tests/bounds.rs	dfa_size_limit_is_accepted_and_inert	builder-knobs	
tests/bounds.rs	nesting_is_bounded_on_a_small_stack	no-panic-total	parse-nesting-cap
tests/bounds.rs	offsets_are_byte_offsets_on_code_point_boundaries	match-offsets-and-accessors	perl-class-tables
tests/bounds.rs	pathological_patterns_finish_promptly	pikevm-linear-cost	
tests/bounds.rs	scanning_a_long_haystack_is_linear	— (§4)	
tests/bounds.rs	the_amplifier_pattern_is_rejected_under_the_bounded_builder	compile-size-ceiling	compile-inst-count
tests/bounds.rs	the_scratch_cache_is_reused_without_leaking_state	pikevm-refines-backtracker	thread-list-generation
tests/bounds.rs	the_size_limit_is_the_thing_that_decides	compile-size-ceiling	compile-inst-count, builder-knobs
tests/bounds.rs	the_worst_program_under_the_ceiling_still_scans_a_row_promptly	compile-size-ceiling	pikevm-linear-cost
tests/differential.rs	a_quantifier_after_a_flag_directive_is_refused	parse-flag-scoping	
tests/differential.rs	builtin_patterns_select_what_they_are_for	regex-reference-semantics	
tests/differential.rs	builtin_selection_patterns_are_pinned	regex-reference-semantics	
tests/differential.rs	case_folding_is_pinned	literal-fold	fold-table-derivation
tests/differential.rs	escape_round_trips	escape-literal	parse-grammar-denotation
tests/differential.rs	exotic_fold_orbits_are_pinned	regex-reference-semantics	
tests/differential.rs	extended_mode_is_pinned	parse-grammar-denotation	
tests/differential.rs	malformed_patterns_are_rejected	parse-grammar-denotation	parse-refusals, parse-error-render
tests/differential.rs	negated_posix_items_fold_before_they_negate	class-denotation	complement-exact
tests/differential.rs	nullable_and_lazy_repetitions_are_pinned	compile-refines-reference	
tests/differential.rs	perl_classes_are_pinned	perl-class-tables	
tests/differential.rs	refusals_are_refusals	parse-refusals	parse-error-render
tests/differential.rs	syntax_corpus_is_pinned	regex-reference-semantics	
tests/differential.rs	the_prefilter_never_changes_the_answer	prefilter-transparent	regex-reference-semantics
tests/differential.rs	this_engine_finds_the_leftmost_start	compile-refines-reference	pikevm-refines-backtracker
tests/differential.rs	zero_width_matches_are_pinned	find-iter-advance-rule	assertion-semantics
tests/fuzz.rs	generated_patterns_are_pinned	regex-reference-semantics	api-consistency
```

## 3. Coverage

- 86 tests read. 83 map to one of the 44 specs. 3 are not spec samples (§4).
- The biggest collapses: `compile-size-ceiling` (5 primary), `step-budget-contract` (5),
  `regex-reference-semantics` (5), `prefilter-sets` (4), `pikevm-refines-backtracker` (4),
  `compile-refines-reference` (4).
- `api-consistency` has no primary test. It is the secondary reading of
  `fuzz::generated_patterns_are_pinned`, and it is free once `earliest-preserves-existence` and
  `find-iter-advance-rule` are proved.
- Three tests sample two specs that split their primary role:
  `lib::the_amplifiers_terminate_promptly_instead_of_hanging` (`step-cost-lower-bound`, plus
  `fail-closed-wrappers`), `lib::the_step_limit_is_the_thing_that_decides`
  (`step-budget-contract`, plus `builder-knobs`) and `fuzz::generated_patterns_are_pinned`
  (`regex-reference-semantics`, plus `api-consistency`). Each one is deleted only when both specs
  are Certified.
- The 5 digest tests under `regex-reference-semantics` are not deleted when it is proved. They
  become the `Ref` model's conformance test with a negative control (§5.6).

## 4. Tests that are not spec samples

Status (2026-10-15): `instruction_cost_covers_the_whole_program` (the **delete** row) has been deleted, and its rows here with it.

| test | why | action |
|---|---|---|
| src/lib.rs::every_builtin_selection_rule_stays_far_inside_the_default | Calibration of a tuning constant. It bisects measured unit counts for 11 rule/haystack pairs, prints them, and pins the documented numbers (`per_row ≤ 2^22/8`). That is a margin about chosen inputs, not a property. Its "still matches under the default" half is sampled by `regex-reference-semantics` and `step-budget-contract`. | **move to bench** (`aterm-bench`), then delete |
| tests/bounds.rs::scanning_a_long_haystack_is_linear | A wall-clock hang detector for one pattern. No general spec says `find_iter` is linear: it is quadratic for `\w+=|\w` (see `find-iter-shared-budget`). Only the per-search cost (`pikevm-linear-cost`) is linear. Its count assertion is a point sample of `regex-reference-semantics`. | **move to bench** (`aterm-bench`), then delete |

Totals: move to bench 2. One more test is half calibration
(`lib::a_find_iter_is_held_to_one_budget_not_one_per_match`, §5.7). Its spec half maps to
`find-iter-shared-budget` and its bisection half goes to the bench.

## 5. Implied specs that look wrong, or disagree with the code

1. **The wave count is off by one.** `wave1-chunks.json` lists 87 tests for aterm-regex, but
   there are 86 `#[test]` fns. `tests/regex_suite.rs` has none; only its doc comment mentions
   `#[test]`s.
2. **The table well-formedness test skips entries.** `src/unicode.rs::range_tables_are_well_formed`
   checks `w[0].0 <= w[0].1` over `table.windows(2)`, which is only the first range of each
   window. The last range's `lo ≤ hi` is never checked, a one-entry table is never checked, and
   nothing checks `hi ≤ 0x10FFFF`. `range-tables-well-formed` states all three.
3. **A pinned builtin rule is stale.** The doc of `BUILTIN` in `tests/differential.rs` says
   these are the eleven patterns `aterm-selection`'s builtin_patterns ships. But
   `crates/aterm-selection/src/builtin_patterns.rs:26` ships
   `(?:~|\.{1,2}|[a-zA-Z0-9._-]+)?(?:/[a-zA-Z0-9._-]+)+/?|[A-Za-z]:...` (which the
   `step_budget_tests` in `src/lib.rs` carry), while differential.rs pins an older
   `(?:/(?:[a-zA-Z0-9._-]+/)*...` form. `BUILTIN_DIGEST` therefore pins a rule the product no
   longer ships. Re-derive it from the shipped list when the digest becomes a conformance test.
4. **A doc names a missing feature that exists.** The doc of
   `tests/bounds.rs::the_worst_program_under_the_ceiling_still_scans_a_row_promptly` says bounding
   the scan "needs a step budget in the VM that the call sites opt into". That is stale:
   `RegexBuilder::step_limit` exists and the `step_budget_tests` in `src/lib.rs` cover it.
5. **Half of a prefilter test is vacuous for most of its inputs.** In
   `src/compile.rs::the_prefilter_byte_map_marks_lead_bytes_only`, the half that checks "every
   code point a pattern can start with has its lead byte marked" runs only when
   `re.is_match(one code point)` is true. That is false for 8 of its 10 patterns (`abc`,
   `\bcommit`, `[a-z]+x`, `foo|bar`, `\d+z`, `[^\s]x`, `你好`, `(?i)Hello`), so for them only the
   continuation-byte half is tested. `prefilter-sets` states the lead-byte half over every
   consuming pc in `K`.
6. **Golden digests cannot be retired by a proof alone.** The digests in
   `tests/differential.rs` and `PINNED` in `tests/fuzz.rs` are FNV hashes recorded when the
   outputs agreed with the regex crate. `Ref`'s agreement with the regex crate is empirical, so
   these should become the `Ref` `ty_model!`'s conformance test (with a negative control), per
   AGENTS.md. They should not be deleted.
7. **One test mixes a spec with calibration.**
   `src/lib.rs::a_find_iter_is_held_to_one_budget_not_one_per_match` checks a spec (one shared
   budget truncates the iteration) and a calibration (`one_search * 16 < 2^22`, found by
   bisection). Only the first half is a spec; the second goes with the bench (§4).
8. **The step-limit read-back is test-only.** `get_step_limit()` is `#[cfg(test)]`, so
   `builder-knobs` states the field equality instead. A proof about the shipped build can only
   observe it through `Regex::step_limit()`.

## 6. Proof order (easiest high-value first)

No spec is blocked by gap 0 alone, since all need gap 1. So the order prefers the specs that
need the fewest gaps beyond 0 and 1, and that unblock other specs.

1. **`classset-byte-size`** (gaps 0, 1, 3). This is straight-line `usize` arithmetic over
   `size_of` constants and two `Vec::len` calls: the length-only `Vec` carrier Trust already
   has, plus E6 widening for the calls. It retires its test and is the class term of `cost`,
   which `compile-size-ceiling` (5 tests) is built on.
   **Measured 2026-10-16 on seal `150efe4a`: still blocked, by gaps 1 and 3.** With
   `ensures result == 56 + self.ranges.len() * 8 + self.perls.len()` on `byte_size`, E6 reports
   `admissible=false` (Pure/Total/Deterministic/NoPanic not established: `self` is
   `&ClassSet`, whose `Vec` fields are "outside the v1 scalar fragment"), and the `&self`
   parameter view refuses it ("local `_6` is a reference or raw pointer": the `&self.ranges`
   borrow handed to `Vec::len`). So no `trust_import_aterm_regex__parse__ClassSet__byte_size`
   is minted, and the clause itself is not lowered ("unsupported contract predicate
   expression"). The fn's row: 0/10 proved, verdict `HasViolations`. Even an admitted view would
   be graded below `Certified` (owner ruling R1), and the view vocabulary has no `usize`.
   The code is unchanged and `classes_report_their_size` stays.
2. **`range-tables-well-formed`** (gaps 0, 1, 5, 7). There is no function body: it is decidable
   by evaluating three static range tables and `FOLD`. It retires its test, fixes the gaps that
   §5.2 found in it, and is the precondition of `in_ranges` and `fold_next`, under every
   class, fold and perl spec.
3. **`builder-knobs`** (gaps 0, 1, 2, 3), with **`regex-metadata`** (gaps 0, 1, 3, 11) next to
   it. These are by-value field updates and copies with no loops. They retire 2 tests, and
   they fix the `step_limit` plumbing that `step-budget-contract` and `find-iter-shared-budget`
   (8 tests) depend on.

Next: **`assertion-semantics`** and **`parse-error-render`** (gaps 0, 1, 3, 5; loop-free
`&str` work), then **`thread-list-generation`** (`&mut self` on a `Vec<u32>`). It is the
lemma that lets `pikevm-refines-backtracker` quantify over every cache state.
