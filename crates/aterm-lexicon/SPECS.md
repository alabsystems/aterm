<!-- Copyright 2026 Andrew Yates -->
<!-- SPDX-License-Identifier: Apache-2.0 -->

# aterm-lexicon: spec recovery from its 131 unit tests

> Status 2026-10-16, after the merge with the tests-and-gates audit: the rows naming 19 tests it
> deleted are gone, and the spec rows no longer cite those tests. Counts elsewhere in this file
> were taken before.

> Status 2026-10-17: the rows of the 2 tests §4 deleted are gone too, and the spec rows no longer
> cite the audit's other deleted tests. The corpus table test `listed_surfaces_reach_their_class`
> stands in for the five tests it replaced.

> Status 2026-10-27: the embedded tricks vocabulary is decoded at build time too
> (builtin-build-time-decode now covers `TrickLexicon::with_languages` and `all_rows`), and a
> resource spec, builtin-tricks-compile-peak, bounds the heap that compile holds at once. Three
> tests were added: one per spec and the allocator harness's peak self-check (§4).

> Status 2026-10-16: this file is the crate's migration worklist. Each of the 40 specs
> below is proved through Trust/Clean about the compiled Rust function, and then the tests
> it subsumes are deleted. The 4 tests in §4 sample no spec and can be deleted now. No spec
> is proved yet.

Source: `crates/aterm-lexicon` (read only). Every `#[test]` was read, along with the code it
reaches: `src/fold.rs` 7, `src/lib.rs` 10, `src/tricks.rs` 0,
`tests/{animal_multilingual 8, animal_species 4, corpus 51, english_fold_collisions 4,
lexicon_alloc 1, profanity_reaches_major_languages 3, scan_scratch_alloc 1, serde_corpus 1,
tricks 40, tricks_alloc 1}` = 131. `tests/lexicon_suite.rs` only declares modules and has no
tests. A naive grep counts 132 because its line-5 doc comment contains the text `#[test]`.
Nothing was compiled or run. The claims below come from reading the code. A second, independent
recovery pass over the same tests (2026-10-16) re-derived the grouping and agreed with all 40
specs and all 4 non-spec calls; its refinements are merged here.

Result: **40 specs**, covering **127 tests**. **4 tests are not spec samples** (§4): 2 are deleted
and the other 2 are to be deleted.

## 0. Notation used in the statements

Characters and fold (`src/fold.rs`):

- `C(s)` = `s.chars()` as a list. `lower(c)` = `char::to_lowercase(c)`, std's Unicode
  lowercase table. The proofs need a model of that table.
- `Strip(c)`, `Kept(c)`, `NoSpace(c)`: `c as u32` lies in the range lists of
  `is_strippable_mark`, `is_kept_mark`, `is_no_space_script`.
- `P16` = `{ / \ : = @ # + ~ $ % | < > * & ^ }`, the set of `is_code_adjacent_punct`.
- `J` = `{ ' , U+2019, - }`, the set of `is_interior_joiner`.
- `Tok(c)` = `is_token_char(c)` = `¬NoSpace(c) ∧ (alnum(c) ∨ c='_' ∨ Strip(c) ∨ Kept(c))`.
- `base(c)` = `fold::base_letter(c)`, and `dom(base)` is its table of accented letters.
- The per-char fold `f(c)`:
  - `[]` if `Strip(c)`;
  - `"ss"` if `c ∈ {ß, ẞ}`;
  - `[σ]` if `c = ς`;
  - otherwise `[ base(l).unwrap_or(l) | l ← lower(c), ¬Strip(l) ]`.
- `Fold(s)` = `concat(map f C(s))`.
- `Marked(s)` = `has_foldable_marks(s)` ⇔ `∃ c ∈ s: ¬ascii(c) ∧ (Strip(c) ∨ ∃ l ∈ lower(c): Strip(l) ∨ base(l) ≠ None)`.
- `fnv(s)` = FNV-1a 64 over the UTF-8 bytes of `s`: offset `0xcbf29ce484222325`, and each
  step is `h ↦ (h xor b)·0x100000001b3 mod 2^64`.

Lexicon views (`lx: Lexicon`):

- `Sp(lx)`: folded key → `Hit{class, langs, form_id, ambiguous, species, marks_required}` (the field `spaced`).
- `Cj(lx)`: raw no-space key → `Hit` (`cjk_forms`). `Ex(lx)`: `cjk_exceptions`.
- `Heads(lx)`: `cjk_heads`. `maxcjk(lx)`: `max_cjk`. `U(lx)`: `user_surfaces`.
- `Conf(lx)`: `conflicts`. `Langs(lx)`: `langs`. `Species(lx)`: `species`.
- `Builtin(L)` = `Lexicon::with_languages(L)`, and `Lexicon::builtin()` = `Builtin(["en"])`.

Build input:

- `E` = the builtin entries followed by the override entries (flagged user), in TOML order.
- `rank`: Profanity 5 > Feline 4 > Canine 3 > Animal 1 > Emphasis 0.
- `Loaded_L(e)` holds iff all of these hold:
  - `class(e)` is one of the six known strings and is not `"orca"`;
  - `class ≠ animal ∨ trim(species) ≠ ""`;
  - `¬e.ambiguous ∨ "all" ∈ L ∨ e.lang ∈ L`.
- `Surf(e)` = `(mode = "suffix" ? [st ++ sf | st ← stems, sf ← (suffixes, or [""] if empty)] : []) ++ forms`.
  Each part is kept only within the global cap `MAX_TOTAL_SURFACES`.
- `Key(e, s)` = `trim(s)` if `e.cjk`, else `Fold(trim(s))`.
- `Claims(k)` = the loaded `(e, s)` with `Key(e, s) = k` that pass validation, in TOML order.
  Each claim carries:
  - `class`;
  - `langs = {intern(e.lang)}`;
  - `ambiguous`;
  - `species`;
  - `marks_required = ¬e.cjk ∧ ascii(k) ∧ Marked(trim s)`.

Scan model:

- `Seg(chars)` is the left-to-right partition that `scan_into_with_scratch` makes. Each part
  is one of:
  - a maximal `NoSpace` run `[i, k)`;
  - a spaced token `[i, token_end(chars, i))` when `Tok(chars[i])`;
  - a skipped char.
- `token_end(chars, i)` = the largest `j` with `chars[i..j] ∈ Tok+ (J Tok+)*`.
- `Lsup(i)` ⇔ `i > 0 ∧ (P16(chars[i-1]) ∨ chars[i-1] = '-' ∨ (chars[i-1] = '.' ∧ i ≥ 2 ∧ alnum(chars[i-2])))`.
- `Rsup(j)` ⇔ `j < n ∧ (P16(chars[j]) ∨ (chars[j] ∈ {'.', '-'} ∧ j+1 < n ∧ alnum(chars[j+1])))`.
- `Admit(h, t)` ⇔ `¬h.marks_required ∨ Marked(t)`.

Trick views (`tl: TrickLexicon`, `src/tricks.rs`):

- `T(tl)`: key → `TrickEntry{role, marks_required, single_cjk}`.
- `eff(e)` = `effective_role(e)`: `needs_vocative` is forced to true when `single_cjk`.
- `Rows(doc)` = `TrickLexicon::rows_from_source(doc)`. `Compile_L(doc)` = `TrickLexicon::from_source(doc, L)`.
- `collapse(cs, k)` replaces every maximal run of ≥3 identical chars with `k` copies and
  leaves shorter runs alone. `elongated(cs)` ⇔ some run has length ≥ 3.

Blocking gaps are the row numbers of the gap table in
`AUDIT-trust-for-test-migration-2026-10-15.md` (historical internal audit) §7.
**Gap 0 (a seal that publishes Clean rows) blocks every spec**, so the column leaves it out.
Seal `150efe4a` closes it locally (see the plan), and admitting that seal to the machine
channel is owner-gated.

## 1. Specs (TSV)

Columns:

- `id`
- `kind`
- `functions`
- `statement`
- `subsumes` (primary tests; §2 lists every test)
- `rust_features` (what the proof must handle)
- `blocking_gaps` (audit §7 numbers)

Test paths in `subsumes` are relative to `crates/aterm-lexicon`.

```tsv
id	kind	functions	statement	subsumes	rust_features	blocking_gaps
fold-denotation	contract	fold::fold, fold::fold_into, fold::base_letter, fold::is_strippable_mark	∀ s:&str, ∀ out:String (any prior contents): after fold_into(s,&mut out), out = Fold(s), and fold(s) = Fold(s). Corollaries: ascii(s) ⇒ Fold(s) = s.to_ascii_lowercase(); ß/ẞ ↦ "ss"; ς ↦ σ; ∀ c with Kept(c) ∧ lower(c)=[c] ∧ base(c)=None: f(c)=[c] (Indic matras survive: fold("बिल्ली") = "बिल्ली").	fold_greek_final_sigma, keeps_indic_marks	String push/push_str/clear/reserve on &mut String; for-loop over Chars; nested loop over char::ToLowercase (needs a model of std's lowercase table); match on char alternatives; Option::unwrap_or; u32 range matches	1, 2, 3, 5, 7
fold-idempotent	invariant	fold::fold, fold::fold_into	∀ s: Fold(Fold(s)) = Fold(s). Proof route: the per-char lemma ∀ scalar c, ∀ d ∈ f(c): f(d) = [d]. It is decidable by evaluation once lower() is modelled, and it lifts through concat. Instances include Turkish İ (lower = i + U+0307, and the combining dot is stripped).	src/fold.rs::fold_is_idempotent	as fold-denotation, plus a finite exhaustive lemma over 1.1M scalars (kernel evaluation or a range-compressed lowercase table)	1, 3, 5, 7, 14
fold-decomposition-invariant	table-property	fold::fold, fold::base_letter, fold::is_strippable_mark	∀ precomposed c with lower(c)=[l], base(l)=Some(b), NFD(c) = B·m1…mk (k≥1), every mi ∈ Strip and lower(B)=[b]: Fold([c]) = Fold(B·m1…mk) = [b]. Hence Fold(s) = Fold(NFD(s)) for every s over such letters. This is a finite table over dom(base), about 150 chars. The domain restriction is part of the spec: the statement is FALSE outside dom(base) (§5.2).	tests/corpus.rs::decomposed_diacritic_matches	a spec-side NFD decomposition table (no std function); otherwise as fold-denotation	1, 3, 5, 14
char-class-tables	table-property	fold::is_no_space_script, fold::is_token_char, fold::is_code_adjacent_punct, is_interior_joiner, is_no_space_surface	∀ c: is_no_space_script(c) ⇔ NoSpace(c); is_token_char(c) ⇔ Tok(c); is_code_adjacent_punct(c) ⇔ c ∈ P16, with |P16| = 16 and P16 ∩ ({'.', ',', '!', '?', '\'', '"', '(', ')', ' '} ∪ alnum) = ∅; is_interior_joiner(c) ⇔ c ∈ J. ∀ s: is_no_space_surface(s) ⇔ s ≠ "" ∧ ∀ c ∈ C(s): NoSpace(c).	src/fold.rs::cjk_detected_as_no_space	matches! over u32 ranges; char::is_alphanumeric (Unicode table model); Iterator::all over Chars	1, 3, 5
ascii-early-out-equivalence	equivalence	fold::is_strippable_mark, is_kept_mark, is_no_space_script, is_token_char, base_letter, has_foldable_marks	∀ c with ascii(c), each early-out answer equals the body without the early-out: c ∉ R_strip ∪ R_kept ∪ R_ns ∪ dom(base), and (c.is_ascii_alphanumeric() ∨ c='_') = (¬NoSpace(c) ∧ (c.is_alphanumeric() ∨ c='_' ∨ Strip(c) ∨ Kept(c))). So each function equals its early-out-free version on every char. It is decidable over 128 values. The table half is a compile-time `const` evaluation of the three tables (without their early-outs) over all 128 ASCII scalars in fold.rs.	(const evaluation in src/fold.rs)	128-case evaluation; u32 range matches; char::is_alphanumeric restricted to ASCII	3, 5
tokenization-whole-word	contract	Lexicon::scan, Lexicon::scan_into_with_scratch, token_end, is_matchable_token	∀ lx, text, opts: every spaced Match m has [m.start,m.end) equal to a spaced segment of Seg(C(text)). Its hit is a lookup of the segment's WHOLE text (classify-token-fallbacks), never of a substring or prefix. So a key matches only where some segment classifies to it: a key inside a longer token (scatter, been, monkeywrench, megathinker, cat5, my_cat, concat_str) or split across tokens (mega think) never matches. ∀ s: is_matchable_token(s) ⇔ C(s) ∈ Tok+ (J Tok+)*.	tests/corpus.rs::scunthorpe_problem, emphasis_negatives; tests/animal_species.rs::animal_words_are_whole_word_only	while loops over Vec<char> with index i; nested loop in token_end; Vec<char> collect; &mut Vec<Match> push; &self	1, 2, 3, 5, 7
code-context-guard	contract	left_suppresses, right_suppresses, Lexicon::try_spaced_token, Lexicon::scan_into_with_scratch, fold::is_code_adjacent_punct	∀ chars, ∀ segment [i,j) of Seg(chars) (a spaced token or a maximal NoSpace run): Lsup(i) ∨ Rsup(j) ⇒ the scan emits no Match within [i,j). left_suppresses(chars,i) = Lsup(i) and right_suppresses(chars,j) = Rsup(j) for all i, j ≤ n. Corollary: for one ASCII punctuation c ≠ '_' and a matching word w, scan(c ++ w) = [] ⇔ c ∈ P16 ∨ c = '-'. This is the same P16 the tricks module exports.	tests/corpus.rs::code_and_path_context_suppressed, code_and_path_context_suppressed_in_no_space_scripts_too; tests/tricks.rs::the_exported_code_context_set_is_the_one_the_scanner_uses	slice get with Option; index arithmetic i-1/i-2/j+1; char::is_alphanumeric; matches!	1, 2, 3, 5, 7
classify-token-fallbacks	contract	Lexicon::classify_token_into, Lexicon::classify_token, strip_possessive, FormId::with_variant	∀ lx, token, scratch: classify_token_into(token, folded, cand) = the first Some of four lookups. (a) h = Sp[Fold(token)] with Admit(h,token), variant 0. (b) The FIRST suffix suf of ["'s", "’s", "'", "’"] with token = stem ++ suf and stem ≠ "": h = Sp[Fold(stem)] with Admit(h,token), variant = index+1; no later suffix is tried. (c) token = "ال" ++ rest with |C(rest)| ≥ 2: h = Sp[Fold(rest)] with Admit(h,token), variant 5. (d) The first char x ∈ A = [و, ف, ب, ك, ل] ∪ H = [ה, ו, ב, כ, ל, מ, ש] with |C(token)| ≥ 3: h = Sp[Fold(token minus x)] with Admit(h,token), variant 6 + (index in A, or 5 + index in H). The result has form_id = h.form_id with its low byte set to the variant. On return, folded = Fold(token) whatever the outcome. classify_token = map(.class).	tests/corpus.rs::arabic_hebrew_clitics_are_stripped, possessive_and_punctuation	&str strip_suffix/strip_prefix; byte slicing at len_utf8; FxHashMap<String,_> get by &str; closures (admits); Option chains; array position; u8 arithmetic; expect on a guarded Option; two &mut String scratch params	1, 2, 3, 4, 5, 7, 13
scan-policy-guards	contract	Lexicon::try_spaced_token, Lexicon::scan	∀ lx, opts, ∀ spaced segment [i,j) with token t, ¬(Lsup(i) ∨ Rsup(j)) and h = classify_token_into(t) = Some: a Match is emitted iff all three hold: (1) ¬(opts.ignore ∋ Fold(t)), keyed on the FULL token's fold and not a stripped stem; (2) ¬(h.class = Feline ∧ ¬opts.allow_bare_cat ∧ |C(Fold(t))| ≤ 3 ∧ ¬h.marks_required); (3) ¬(h.class = Emphasis ∧ |C(Fold(t))| ≤ 3 ∧ Fold(t) ∉ U(lx)). The emitted Match is {start:i, end:j, class, langs, form_id, ambiguous, species} of h with form_hash = fnv(Fold(t)). No other spaced Match is emitted.	tests/corpus.rs::bare_cat_is_opt_in, ignore_set_suppresses_matches, a_marks_required_short_feline_is_not_the_bare_cat_command, user_short_emphasis_surface_matches, non_user_short_emphasis_surface_stays_suppressed; src/lib.rs::ignore_guard_keys_on_full_token_fold_not_stripped_stem	std HashSet<String>::contains through Option<&HashSet>; chars().count(); FxHashSet contains; Vec push of a Copy struct; &self with &mut scratch	1, 2, 3, 4, 5
marks-required-gate	contract	Lexicon::build, insert_class, Lexicon::classify_token_into, Lexicon::surface_requires_marks, fold::has_foldable_marks	∀ sources, L, ∀ k ∈ dom(Sp): Sp[k].marks_required = ⋀ over ALL Claims(k), of every class, of (ascii(k) ∧ Marked(trim s)). surface_requires_marks(k) = Sp[k].marks_required, and false for k ∉ dom(Sp). ∀ token t with ¬Marked(t): classify_token_into(t) never returns a hit with marks_required. Corollary: the bare-ASCII skeleton of a lossy-folded surface matches only when some claimant lists that bare spelling itself (consent), e.g. a user override forms=["dm"].	tests/corpus.rs::lossy_folded_surfaces_do_not_match_their_bare_skeleton, user_override_opts_a_bare_skeleton_in	FxHashMap get_mut/insert; bool &=; str::is_ascii; Iterator::any over Chars and ToLowercase	1, 2, 3, 5, 7
cjk-maximal-munch	contract	Lexicon::scan_cjk_run, Lexicon::scan_into_with_scratch	∀ lx, opts, ∀ maximal NoSpace run [s,k) with ¬(Lsup(s) ∨ Rsup(k)): the matches in [s,k) are MM(s), where MM(k) = [] and, for p < k, l* = max{ l ∈ 1..min(maxcjk, k-p) : chars[p..p+l] ∈ Ex ∪ dom(Cj) } (Ex tested before Cj at each length). No such l ⇒ MM(p) = MM(p+1). w = chars[p..p+l*] ∈ Ex ⇒ MM(p) = MM(p+l*). Else l* = 1 ∧ ¬opts.cjk_single_char ⇒ MM(p) = MM(p+1). Else MM(p) = (ignore ∋ w ? [] : [Match{p, p+l*, Cj[w] fields, form_hash: fnv(w)}]) ++ MM(p+l*). So the longest match wins, exceptions suppress, and runs on one line are independent.	tests/corpus.rs::cjk_feline_compounds_match, cjk_single_char_is_opt_in, cjk_incidental_compounds_suppressed; src/lib.rs::cjk_run_scratch_reuse_preserves_matches_and_ignore; tests/animal_multilingual.rs::single_char_animal_forms_ride_the_cjk_single_char_gate, panda_compounds_are_pandas_not_suppressions_and_never_cats, maximal_munch_and_particles_behave	three nested while loops; &mut String window plus &mut Vec<usize> bounds; String::get(..n) at char boundaries; FxHashMap/FxHashSet lookup by &str; saturating_add; enum CjkHit match; Option<&HashSet>	1, 2, 3, 5, 7
cjk-prefilter-sound	invariant	Lexicon::build, note_cjk_head, Lexicon::scan_cjk_run	∀ sources, L, lx = build(...): (i) ∀ w ∈ dom(Cj) ∪ Ex: w ≠ "" ∧ Heads[C(w)[0]] ≥ |C(w)|; (ii) dom(Heads) = { C(w)[0] | w ∈ dom(Cj) ∪ Ex }, and Heads[c] = the max of |C(w)| over w with head c; (iii) ∀ c: Heads[c] ≤ maxcjk ≤ MAX_CJK_SURFACE_CHARS = 32. Consequence (equivalence): scan_cjk_run with the head prefilter and clamp returns the same matches as MM with window min(maxcjk, k-p) and no prefilter.	src/lib.rs::cjk_prefilter_admits_every_surface_it_gates	FxHashMap<char,usize> entry().or_insert; iteration over map keys and set members; Chars::count; loop invariant over the build loop	1, 3, 5, 7
build-surface-bounds	contract	Lexicon::build, push_overlong_surface_conflict	∀ lexicon/exception sources and L: (a) the running total of materialized surfaces never exceeds MAX_TOTAL_SURFACES = 2^20, so |dom(Sp)| + |dom(Cj)| ≤ 2^20; (b) an entry whose stems·max(1,|suffixes|) would push the total past the cap materializes none of its suffix surfaces and appends one conflict containing "surface cap", and likewise for forms; entries within the cap expand to exactly Surf(e); (c) every key of Cj and Ex has 1..=32 chars, a longer cjk surface or exception is dropped with a conflict naming its char count, and maxcjk ≤ 32. All arithmetic saturates, so the build never panics or overflows.	src/lib.rs::suffix_expansion_is_globally_bounded, no_space_surface_length_is_bounded	saturating_mul/add on usize; Vec<String> push in nested loops; String::with_capacity; to_string of usize; loop invariant on `total`	1, 3, 5, 7, 9
build-key-set	contract	Lexicon::build, Lexicon::from_sources, Lexicon::with_languages_and_override, is_matchable_token	∀ sources, L: dom(Sp) = { Fold(trim s) | e ∈ E, Loaded_L(e), ¬e.cjk, s ∈ Surf(e), Fold(trim s) ≠ "" ∧ is_matchable_token(Fold(trim s)) }; dom(Cj) = { trim s | Loaded_L(e), e.cjk, s ∈ Surf(e), 1 ≤ |C(trim s)| ≤ 32 }; Ex = { trim w | 1 ≤ |C(trim w)| ≤ 32 }. with_languages_and_override(L, Some(t)) uses E = builtin ++ entries(t), with every entry of t flagged user, and U = the spaced keys of user entries. mode = "forms" contributes only forms. A suffix-mode entry contributes stems × suffixes ++ forms. No derived (inflected) surface is ever added.	tests/corpus.rs::user_override_extends_coverage, emphasis_positive_forms	Vec<RawEntry> into_iter with a map closure; String trim/to_string; FxHashMap/FxHashSet inserts; the serde-decoded structs	1, 3, 5, 7, 9, 13
insert-class-merge	contract	insert_class, class_rank, Lexicon::build	∀ key k with Claims(k) = c1..cn (n ≥ 1), in Sp or Cj: class = W, the class of maximal rank among the ci; langs = ⋃{ ci.langs | ci.class = W }; ambiguous = ⋀{ ci.ambiguous | ci.class = W }; marks_required = ⋀ over ALL ci (marks-required-gate); species = the species of the first ci with class W and Some species. A different later species on a W-claimant appends the conflict "claimed by two animal species". class, langs, ambiguous and marks_required are invariant under any permutation of the claims. Entries of the retired class "orca" contribute no claim.	tests/corpus.rs::class_precedence_total_order, cross_class_homograph_resolves_to_profanity, cross_class_winner_keeps_only_its_own_langs, a_bare_claimant_keeps_its_key_matchable_across_a_precedence_replacement, cjk_homograph_neko_unions_ja_zh	FxHashMap::get_mut with guarded match arms; *prev = new on &mut; u64 bitset union; Option<SpeciesId> match; a fold over insertions (loop invariant) for order independence	1, 2, 3, 5, 7; fragment: `class_rank` F1+D2b (VF5 first target, docs/PLAN-tests-to-specs-2026-10-15.md "F1-ready specs").
species-iff-animal	invariant	Lexicon::build, insert_class, intern_species, Lexicon::species_code	∀ sources, L, ∀ hit ∈ rng(Sp) ∪ rng(Cj): hit.species.is_some() ⇔ hit.class = Animal. For an Animal hit, species_code(hit.species) is the trimmed species string of the first winning claimant. Species ids are assigned in first-appearance order over animal entries with a species, before ambiguous gating. species_code(id) = "unknown" for any id never issued.	tests/animal_species.rs::non_animal_matches_carry_no_species	tuple match on (Class, &str); Vec<String> position/push; usize::min; u16 casts	1, 3, 5, 7
ambiguous-gating	contract	Lexicon::build, Langs::enabled, Langs::new	∀ sources, L: an entry with e.ambiguous contributes claims iff "all" ∈ L ∨ trim(e.lang) ∈ L. A non-ambiguous entry contributes regardless of L. Every Match carries ambiguous = the merged bit of insert-class-merge. Hence a surface claimed only by gated entries is absent when its languages are not configured. A surface claimed by both a gated and an ungated entry of the winning class is present, carries only the loaded claimants' langs, and is ambiguous iff all loaded winning-class claimants are.	src/lib.rs::romanian_fut_forms_are_ambiguous_in_ro_and_all_but_not_default_english, padded_entry_lang_trims_consistently_for_interning_gating_and_known_check	FxHashSet<String> contains; slice contains on &[&str]; early continue in the build loop	1, 3, 5, 7
lang-interning	contract	intern_lang, Lexicon::lang_code, primary_lang, Lexicon::build	∀ sources, L: let D = the distinct codes trim(e.lang), excluding "" and "unknown", in first-appearance order over entries with a known non-orca class, counted BEFORE the species check and ambiguous gating. LangId(i) is assigned to D[i] for i < 63. Every code past the 63rd, and ""/"unknown", gets UNKNOWN_LANG = LangId(63). Langs(lx) = D[..min(63,|D|)], so ids do not depend on L. lang_code(id) = Langs[id] if id < |Langs|, else "unknown". Corollary: under equal sources, the langs of a match built with L1 ⊆ those built with L2 whenever the loaded claims of L1 ⊆ those of L2.	tests/corpus.rs::primary_lang_is_first_toml_appearance, empty_lang_interns_as_unknown, ambiguous_gating_keeps_semantics_and_ids_stay_config_independent; src/lib.rs::lang_interning_overflow_collapses_to_unknown	Vec<String> position with a closure; trim; u16 casts; Vec::get with map_or	1, 3, 5, 7, 13
langset-finite-set	refinement	LangSet::with, union, is_empty, len, iter, primary_lang	Let α(LangSet(b)) = { i < 64 | bit i of b is set }. Then α(EMPTY) = ∅; α(s.with(id)) = α(s) ∪ {id.0 mod 64}; α(s.union(t)) = α(s) ∪ α(t); s.is_empty() ⇔ α(s) = ∅; s.len() = |α(s)|; s.iter() yields exactly α(s) in strictly ascending order and terminates; primary_lang(s) = LangId(min α(s)) if α(s) ≠ ∅, else UNKNOWN_LANG. LangSet::contains is #[cfg(test)]-only, so membership is stated through α.	src/lib.rs::langset_bitset_mechanics	u64 shifts, |, &, count_ones, trailing_zeros, wrapping_sub; iter::from_fn closure with captured mut state; u32 % 64	3, 5, 7, 13
form-identity	contract	fnv1a64, form_hash, finalize_hits, FormId::exact, FormId::with_variant, Lexicon::try_spaced_token, Lexicon::scan_cjk_run	∀ s: form_hash(s) = fnv(s). ∀ Match m from scan: m.form_hash = fnv(Fold(full token)) on the spaced arm (the full possessive or clitic token, not the stem), or fnv(raw window) on the CJK arm. finalize_hits assigns pairwise-distinct base ids 0..|Sp|+|Cj|-1 over dom(Sp) ⊎ dom(Cj), each table sorted by key, and form_id = base << 8 | variant (variant 0 exact, 1..4 possessives, 5 article, 6..17 clitics). Hence m1.form_id = m2.form_id ⇔ same table, same key and same variant. form_id and form_hash do not depend on match position or case. FormId::UNKNOWN is never issued when |Sp|+|Cj| < 2^56.	tests/corpus.rs::form_hash_populated_and_position_independent, form_id_distinguishes_supported_morphology_from_its_stem, form_hash_populated_on_cjk_path, form_hash_helpers_match_scan_semantics	byte loop with wrapping_mul and xor; Vec sort_unstable_by with a closure; checked_add with expect; u64 shift/or; FxHashMap rebuild	1, 3, 5, 7, 13
scan-scratch-stateless	equivalence	Lexicon::scan_into_with_scratch, Lexicon::scan	∀ lx, text, opts, ∀ prior values of chars:Vec<char>, out:Vec<Match> and scratch:ScanScratch: after scan_into_with_scratch(text, opts, chars, out, scratch), out = scan(lx, text, opts) and chars = C(text). The scratch carries no result state, so repeated calls with the same buffers give the same out every time.	src/lib.rs::resident_scan_scratch_matches_compatibility_and_stops_growing	five &mut buffer params; Vec clear/extend; destructuring of &mut ScanScratch; frame reasoning that each buffer is cleared before it is read	1, 2, 3, 7
builtin-build-time-decode	equivalence	Lexicon::with_languages, Lexicon::with_languages_and_override, builtin_rows, TrickLexicon::with_languages, TrickLexicon::all_rows, builtin_tricks, build.rs	∀ L ∈ {[], ["en"], ["all"]} ∪ {[x] | x a data lang}: with_languages(L) = from_sources(data/lexicon.toml, data/exceptions.toml, L) = with_languages_and_override(L, None), equal as values (every key and hit, language and species table, CJK table, exception and conflict, in order). The build script decodes both files through the run-time shapes (src/raw.rs) and emits their rows in file order (BUILTIN_ENTRIES, BUILTIN_EXCEPTION_WORDS); builtin_rows rebuilds RawLexicon/RawExceptions from them with notes/lang/meaning empty (never read) and user = false, so build sees the rows a run-time decode would give it. Likewise for data/tricks.toml: ∀ L in the same set (over the file's languages), TrickLexicon::with_languages(L) = Compile_L(data/tricks.toml), equal as values (Debug), and all_rows() = Rows(data/tricks.toml); the build script emits the trick, vocative and filler rows in file order (BUILTIN_TRICK_ROWS, BUILTIN_VOCATIVE_ROWS, BUILTIN_FILLER_ROWS), and builtin_tricks rebuilds RawTricks from them with notes empty (never read). A document that does not decode fails the build.	tests/corpus.rs::the_builtin_decoded_at_build_time_is_the_lexicon_of_the_data_files; tests/tricks.rs::the_builtin_decoded_at_build_time_is_the_vocabulary_of_the_data_file	build script codegen (include! of OUT_DIR source); &'static [&'static str] statics; Vec<String> rebuilt by map/collect	9
warm-path-allocation-free	other (resource)	Lexicon::scan_into_with_scratch, TrickLexicon::classify, TrickLexicon::classify_elongated, fold::fold_into	(a) ∀ lx, text, opts: if chars, out and every ScanScratch buffer already have capacity ≥ the high-water demand of scanning text (e.g. after one earlier scan of the same text), scan_into_with_scratch makes zero global-allocator calls and leaves every capacity unchanged. (b) ∀ tl, token: if scratch.capacity() ≥ the byte length of the longest fold or collapse the call writes (≤ |Fold(token)|), classify(token, scratch) makes zero allocator calls on every lane (hit, marked, raw no-space, elongation retries, miss).	tests/scan_scratch_alloc.rs::warmed_scan_scratch_is_allocation_free_across_every_scanner_lane; tests/tricks_alloc.rs::a_warmed_classify_is_allocation_free_on_every_lane	a heap/allocator effect model (capacity vs len for String/Vec push/extend/reserve); fixed [char;64] window; FxHashMap lookups proven non-allocating	1, 2, 7, 8
builtin-tricks-compile-peak	other (resource)	TrickLexicon::with_languages, builtin_tricks, tricks::compile	∀ L ∈ {["en"], ["all"]}: the most bytes the calling thread holds at once from the global allocator, from the call to with_languages(L) through dropping its result, is ≤ 2 MiB. (Decoding data/tricks.toml at run time held 7.8 MB at its peak and left that much small-allocator memory dirty in a launch: docs/measured/idle-memory-2026-10-27.md.)	tests/tricks_alloc.rs::compiling_the_embedded_vocabulary_peaks_under_2_mib	a heap/allocator effect model with a live-byte high-water mark; FxHashMap growth; Vec<String> rebuilt by map/collect	1, 7, 8
every-key-round-trips	contract	Lexicon::scan, Lexicon::build, Lexicon::iter_spaced, Lexicon::iter_cjk, Lexicon::surface_requires_marks	∀ sources, L, with lx = build and O = {allow_bare_cat, cjk_single_char, ignore: None}: (a) ∀ (k,h) ∈ Sp with ¬(h.class = Emphasis ∧ |C(k)| ≤ 3 ∧ k ∉ U): scan(lx, W(k), O) contains a Match of class h.class with langs ≠ ∅. W(k) = k when ¬h.marks_required, else marked_witness(k) (k's first char, then U+0301, then the rest). When h.marks_required, scan(lx, k, O) has no Match with form_id h.form_id. (b) ∀ (w,h) ∈ Cj with w ∉ Ex and no exception that overlaps w inside the run shadowing it: scan(lx, w, O) contains a Match of h.class with langs ≠ ∅. Lemma: every hit has langs ≠ ∅, since each claim's set is {intern(lang)}.	tests/corpus.rs::every_surface_round_trips	quantified over map contents; composes fold-idempotent, tokenization, classify and munch	1, 2, 3, 4, 5, 7
build-conflicts-channel	contract	Lexicon::build, Langs::note_unknown, push_overlong_surface_conflict	∀ sources, L: Conf(lx) is exactly the ordered list of these load-law violations, one message each naming its kind and the lang/surface: unknown class (not one of the six strings, "orca" excepted); animal entry without species; species on a non-animal entry; mode ∉ {"suffix","forms"} ("unknown mode"); suffix or forms expansion over the cap ("surface cap"); user single-char cjk surface ("requires cjk_single_char = true"; the surface is still inserted); user spaced surface whose fold is empty or not matchable ("dropped"); overlong no-space surface or exception; an entry with ≥1 offered surface and 0 inserted ("NO scannable surface"); two species on one key; each configured code in L that is ≠ "all" and is not trim(e.lang) of any entry, entries of unknown or orca class included ("configured language …"). Conf = [] iff none occurs. Builtin (non-user) multi-word and single-char cjk surfaces are not violations.	tests/corpus.rs::a_configured_language_the_data_never_names_is_reported_not_swallowed, an_unknown_entry_mode_is_reported_and_its_stems_are_not_lost_in_silence, an_entry_whose_every_surface_is_unmatchable_is_reported, user_unscannable_surfaces_surface_as_conflicts	String building with push_str and Debug-through-Display (DebugText); Vec<String> push; FxHashSet of data langs	1, 3, 5, 7, 9
toml-schema-decoding	refinement	Lexicon::from_sources, Lexicon::with_languages_and_override, TrickLexicon::from_source, TrickLexicon::rows_from_source, Deserialize of RawLexicon/RawEntry/RawExceptions/RawException/RawTricks/RawTrickRow/RawWordRow	For each document type, decode(doc) is Ok iff doc is valid TOML whose root holds only the declared arrays of tables (entry; exception; trick, vocative, filler), whose rows hold only declared keys of the declared types, and whose rows have their required keys (class; word; id and lang; lang). On Ok it equals the serde-data-model decoding, with mode defaulting to "suffix" and every other optional field to empty/false. `notes` (and the exception's `lang`/`meaning`) are accepted and discarded. `user`, `_notes`, a table given for an array, and a wrong scalar type (including NaN/-0 for a bool) are rejected. The observable answer (Lexicon key set/class/species/conflicts, or Rows Debug) equals the recorded registry-serde answer for every corpus document.	tests/serde_corpus.rs::lexicon_documents_answer_as_recorded; tests/corpus.rs::a_typod_key_in_lexicon_data_is_a_parse_error_not_an_empty_entry	serde derive (deny_unknown_fields, default, default = fn, rename, skip) through the aterm_toml cross-crate deserializer; Result/Err mapping	1, 7, 9, 12
builtin-lexicon-wellformed	table-property	Lexicon::with_languages, Lexicon::builtin, Lexicon::build	For the embedded data/lexicon.toml + exceptions.toml, ∀ L ∈ {[], ["en"], ["all"]} ∪ {[x] | x a data lang} ∪ {["en",x]}: the embedded documents decode (at build time, builtin-build-time-decode) and Conf = []; no Emphasis key is in dom(Sp) ∪ dom(Cj); dom(Cj) ∩ Ex = ∅; |Sp(Builtin(["all"]))| > 50. Proof: evaluate the reflected build over the embedded constants.	tests/animal_multilingual.rs::expanded_builtin_stays_conflict_free	rows decoded at build time (builtin-build-time-decode); whole-build evaluation (hash maps); OnceLock for builtin()	1, 3, 7, 9, 13
builtin-lexicon-coverage	table-property	Lexicon::with_languages, Lexicon::scan, Lexicon::species_code, Lexicon::lang_code	A pinned finite coverage relation Cov of rows (L, text, opts, expected [(class, species?, ambiguous?)]) holds for Builtin(L). Rows include: en profanity and feline forms; fr/de/es/it/pl/fi profanity; es/it/fr/de/sv/fi/id feline; Scheiße/SCHEISSE; species tags for the English animal roster (fox, bee and owl included; orca matches nothing; kitty stays Feline); zh/ja/ko/th multi-char animal compounds in running text; id guguk = Canine while anjing = Profanity; an f-bomb in running text for each of 20 major languages under ["en"]. Each row is a decidable instance of the scan specs over the evaluated builtin tables. Recommended: move Cov into a data file beside lexicon.toml so the spec quantifies over its rows.	tests/corpus.rs::listed_surfaces_reach_their_class; tests/animal_species.rs::animal_surfaces_scan_with_their_species, animal_class_never_steals_existing_families; tests/animal_multilingual.rs::turkey_speaks_chinese_japanese_and_korean, big_animals_match_whole_word_in_mixed_cjk_text; tests/profanity_reaches_major_languages.rs::major_languages_have_a_working_f_bomb_by_default	evaluation of build + scan over embedded data; finite table	1, 3, 7, 9
builtin-lexicon-false-positive-avoidance	table-property	Lexicon::with_languages, Lexicon::scan	A pinned finite negative relation Neg of rows (L, text, opts) holds: scan(Builtin(L), text, opts) has no match, or no match of the named class. Positive controls sit beside it. Rows include: the ja/ko/th prefix-bomb exception compounds under cjk_single_char; ja くま, ko 판다 and id kalkun under ["en"]; 我操作/我操心/我操控 (while 我操这… stays Profanity); "fan" and "future" in English prose; every prefix of dmg/dmesg/dmarc/dmz/dma/DM/dm's, while đm/ĐM/d̃m stay immediate Profanity; every prefix of pick/picture/picnic. Each row is a decidable instance of the exception, gating and marks-required specs over the evaluated builtin tables.	tests/animal_multilingual.rs::prefix_bomb_exceptions_stay_silent_and_animals_survive, homograph_bombs_are_language_gated; tests/profanity_reaches_major_languages.rs::ordinary_chinese_does_not_detonate, genuinely_ambiguous_surfaces_stay_gated; tests/english_fold_collisions.rs::typing_dmg_is_silent_and_the_marked_form_still_fires, typing_pick_is_silent_all_the_way_through	evaluation of build + scan over embedded data; finite table	1, 3, 7, 9
english-dictionary-collision-gates	table-property	Lexicon::builtin, Lexicon::scan, Lexicon::iter_spaced, TrickLexicon::with_languages, TrickLexicon::classify, TrickLexicon::all_rows	Over a VENDORED English word list D (today the tests read the host's /usr/share/dict/web2 and end COULD NOT RUN where it is absent, §5.4): (a) ∀ w ∈ D, ascii, 2 ≤ |w| ≤ 8, w ∉ INTENTIONAL: no Match m in scan(Builtin(["en"]), w) has m = [0,|w|) ∧ ¬m.ambiguous. (b) Let Live = { k ∈ dom(Sp) | Sp[k].class = Profanity ∧ scan(k) has an unambiguous Profanity match }; then ∀ ascii w ∈ D, ∀ proper prefix p of w with p ∉ INTENTIONAL_PREFIXES: p ∉ Live. (c) ∀ ascii w ∈ D not folded from an en tricks row, w ∉ DICTIONARY_EXCUSED: classify(with_languages(["en"]), w) ≠ Some(Trick{needs_vocative:false}).	tests/english_fold_collisions.rs::no_unambiguous_surface_is_an_ordinary_english_word, no_english_word_transits_an_unambiguous_profanity_prefix; tests/tricks.rs::no_ungated_non_english_surface_is_an_english_dictionary_word	fs::read_to_string of an external file (must become an embedded constant first); ~235k-word evaluation	1, 3, 7, 8, 9
trick-code-bijection	table-property	Trick::code, Trick::from_code, Trick::ALL	∀ i < 17: ALL[i] as usize = i. ∀ t: from_code(code(t)) = Some(t). code is injective, and code(t) is non-empty [a-z]+. ∀ s ∉ {code(t)}: from_code(s) = None (exact match only, no fold or trim). code(ALL[0]) = "sit" and code(ALL[16]) = "treat".	tests/tricks.rs::trick_codes_round_trip_and_all_is_in_declaration_order	enum match returning &'static str; array into_iter().find with a closure; &str equality	1, 3, 7, 13
trick-blurb-table	table-property	Trick::blurb	∀ t ∈ ALL, with b = blurb(t): b ≠ ""; every char of b ∈ [a-z ' ' '-']; b has no leading, trailing or double space; |b| ≤ 48 bytes. A 17-case evaluation.	-	&'static str predicates over chars; finite evaluation	1, 3, 5, 7
row-line-format	contract	tricks::row_line, push_surface_list	∀ row: row_line(row) = head ++ "lang=" ++ row.lang ++ " gated=" ++ (row.gated ? "1" : "0") ++ " words=" ++ list(row.words) ++ tail. head = "command trick=" ++ code(t) ++ " " for Trick(t), else "vocative " or "filler ". tail = " address=" ++ list(row.address) ++ " does=" ++ map(' '↦'_', blurb(t)) for Trick(t), else "". list([]) = "-" and list(xs) = join(",", xs). Corollary: if the lang and every surface avoid ' ', '=', ',' and '\n', the line splits on ' ' into the head word and key=value fields with keys [trick, lang, gated, words, address, does] (or [lang, gated, words]). With trick-blurb-table and tricks-embedded-wellformed this holds for every embedded row, and every trick is listed.	tests/tricks.rs::row_line_is_stable, every_listing_line_of_the_embedded_file_is_one_line_of_key_value_fields	String push/push_str; enumerate loop over &[String]; char map loop	1, 3, 7
trick-row-order	contract	ordered_rows, stage_row, TrickLexicon::rows_from_source, TrickLexicon::all_rows	∀ doc that decodes: Rows(doc) = stable_sort_by(first-appearance index of lang, Staged), where Staged = [trick rows with Trick::from_code(trim id) = Some, in file order] ++ [vocative rows] ++ [filler rows]. Each staged row has its lang and every surface trimmed, and empty surfaces dropped. Rows whose trimmed lang is "" are removed, each recorded as the conflict "row without lang (<kind>)". An unknown id is recorded as "unknown trick id \"<id>\"" and its row is skipped. Hence each language is one contiguous run in first-appearance order, and within a run tricks precede vocatives, which precede fillers, each in file order. Compile_L consumes exactly these rows in this order.	tests/tricks.rs::listing_rows_are_grouped_by_language_with_tricks_then_vocatives_then_fillers, a_row_without_a_language_is_a_conflict_and_a_typo_in_a_key_is_a_parse_error, an_unknown_trick_id_is_a_conflict_and_its_row_is_skipped	Vec<TrickRow> push; retain with a closure; iter_mut; stable sort_by_key with a position closure; Vec<String> contains	1, 3, 5, 7, 9, 13
trick-compile-laws	contract	tricks::compile, tricks::claim_surface, tricks::effective_role, TrickLexicon::from_source, TrickLexicon::with_languages	∀ rows = Rows(doc), L, tl = compile(rows, L): a row loads iff ¬gated ∨ lang = "en" ∨ "all" ∈ L ∨ lang ∈ L, and a gated en row loads and records "English is never gated". For each loaded row, list (words with needs_vocative false, address with true) and surface s: |C(s)| > 32 ⇒ dropped with "longer than the typed-token window". A cjk row: ¬is_no_space_surface(s) ⇒ dropped with "no-space-script surfaces only"; otherwise key = s, single_cjk = (|C(s)| = 1), marks_required = false, unglued = (s ∈ row.unglued). A spaced row: row.unglued ≠ [] records a conflict; key = Fold(s); key = "" ∨ ¬is_matchable_token(key) ⇒ dropped with "not a single whole-word token"; otherwise marks_required = ascii(key) ∧ Marked(s), unglued = false. An entry in row.unglued naming no surface of its row records a conflict. Then, in listing order: a new key is inserted; a key already present with the same eff merges silently with marks_required ANDed and unglued ORed (one claimant keeping a surface out of the glued reading keeps it out); a key with a different eff keeps the first entry and records "already claimed under another role: kept the first". dom(T(tl)) is exactly the keys of loaded, validated surfaces, and the conflicts are exactly these, after the trick-row-order conflicts.	tests/tricks.rs::a_surface_claimed_under_two_roles_keeps_the_first_and_says_so, a_cross_language_claim_is_won_by_the_language_that_opens_the_file, the_same_role_claimed_again_merges_silently, a_surface_that_is_not_one_token_is_dropped_and_named, a_cjk_row_takes_no_space_script_surfaces_only_and_matches_the_whole_run, a_single_character_no_space_surface_always_needs_the_vocative, a_marked_surface_needs_marked_typing_unless_the_bare_spelling_is_listed, gated_rows_stay_silent_until_their_language_is_listed_and_english_is_never_gated, a_surface_longer_than_the_token_window_is_dropped_and_named, gated_rows_of_the_embedded_file_are_silent_by_default, no_lone_ideograph_in_the_embedded_file_fires_unaddressed, an_unglued_entry_that_names_nothing_in_its_row_is_a_conflict, one_claimant_keeping_a_surface_out_of_the_glued_reading_keeps_it_out	FxHashMap get_mut/insert with a guarded match; enum equality on TrickRole; array of (&[String], bool) tuples; nested loops; String conflict building	1, 2, 3, 5, 7
trick-classify-denotation	contract	TrickLexicon::classify, TrickLexicon::classify_glued_half, TrickLexicon::shared_en, TrickLexicon::default	∀ tl, token, scratch (any prior contents): classify(token, scratch) = let e = (is_no_space_surface(token) ? T[token] : T[Fold(token)] orElse elongated_lookup(Fold(token))) in (e = None ∨ (e.marks_required ∧ ¬Marked(token)) ? None : Some(eff(e))), independent of scratch; classify_glued_half(token, scratch) = None when e.unglued, else classify(token, scratch). Consequences: case and strippable-mark insensitivity; whole-token only; raw whole-run equality on no-space tokens (no maximal munch); an unglued no-space surface matches whole via classify and never as half of a glued run via classify_glued_half; a Trick result for a lone no-space token has needs_vocative = true. shared_en() is extensionally equal to with_languages(["en"]) (equal surface_count and classify on every token). TrickLexicon::default() has T = ∅ and no conflicts.	tests/tricks.rs::classify_folds_case_and_strippable_marks, the_cached_english_table_is_the_default_configuration, an_unglued_surface_is_a_word_whole_and_no_half_of_a_run	&self + &mut String; ? on Option; FxHashMap get; OnceLock get_or_init (static); uses fold-denotation	1, 2, 3, 5, 7, 13
trick-elongation-fold	contract	TrickLexicon::classify_elongated, run_len, has_elongated_run, push_collapsed	∀ folded f: classify_elongated(f) = None if |C(f)| > 64 or ¬elongated(C(f)); otherwise the first Some of T[collapse(C(f),2)] and T[collapse(C(f),1)]. collapse leaves runs of length ≤ 2 alone, so a doubled letter is never changed and a typo stays a miss. Lemmas: run_len(cs,at) = the length of the maximal run of cs[at] from at (0 if at ≥ |cs|); has_elongated_run(cs) ⇔ some maximal run has length ≥ 3; push_collapsed appends collapse(cs,k); all three terminate, never panic, and |collapse| ≤ |cs|.	tests/tricks.rs::a_drawn_out_word_is_still_the_word	fixed [char;64] array with get_mut ?; while loops with saturating_add; slice get comparisons; String push in a counted for-loop	1, 3, 5, 7
trick-authored-round-trip	contract	TrickLexicon::from_source, TrickLexicon::classify, TrickLexicon::rows_from_source	∀ doc, L, tl = Compile_L(doc): ∀ loaded row r ∈ Rows(doc), ∀ authored surface s of r that passed validation and whose key's first claimant has eff = ExpectedRole(r,s) (the list's role, with needs_vocative forced for a lone cjk surface): classify(tl, s) = Some(ExpectedRole(r,s)). s is its own marked witness. Instance: the embedded file is conflict-free (tricks-embedded-wellformed), so this holds for every embedded surface under ["all"] and under [r.lang].	tests/tricks.rs::every_authored_surface_round_trips_to_its_own_role	composes trick-compile-laws and trick-classify-denotation with fold-idempotent	1, 2, 3, 4, 5, 7
tricks-embedded-wellformed	table-property	TrickLexicon::with_languages, TrickLexicon::all_rows	For the embedded data/tricks.toml, ∀ L ∈ {[], ["en"], ["all"]} ∪ {[x], ["en",x] | x a file lang}: conflicts = [] and surface_count > 100; the first lang of all_rows() is "en"; every row has a trimmed lang and ≥ 1 surface, and only trick rows have an address list; every authored surface s has s = s.to_lowercase(), |C(s)| ≤ 32, and is unique per (lang, s); no surface contains ' ’ " `, and no surface is z{3,}.	tests/tricks.rs::the_embedded_vocabulary_has_no_conflicts_under_any_language_configuration, authored_surfaces_are_lowercase_unique_within_a_language_and_fit_the_token_window, no_surface_would_hurt_someone_who_presses_enter	evaluation of decode + compile over the embedded constant	1, 3, 7, 9
tricks-embedded-coverage	table-property	TrickLexicon::with_languages, TrickLexicon::shared_en, TrickLexicon::classify, TrickLexicon::all_rows	For the embedded file: ∀ t ∈ ALL, some en words-surface classifies under ["en"] to Trick{t, needs_vocative:false}; ∀ L ∈ {["en"], [], ["ja"]}, en reaches every t; ∀ file lang x, ∀ t, some (t,x) surface classifies to t under [x] (CORE_GAPS = ∅); no plain en word folds into LINE_OPENERS \ PLAIN_ON_PURPOSE, and every PLAIN_ON_PURPOSE word is both an opener and a plain en word; the pinned role table holds (iconic plain verbs, address openers, praise/scold, cat-speak, vocatives, fillers, natural-phrase words, purity-ending words); the pinned inflections and lookalikes classify None under ["all"]. Recommended: move the pinned word lists into a data file the spec quantifies over.	tests/tricks.rs::every_trick_has_a_plain_english_word, the_iconic_bare_verbs_answer_unaddressed_and_the_everyday_openers_do_not, praise_and_scolding_never_fire_without_the_pet_being_named, the_cat_speak_the_internet_knows_is_understood, the_natural_phrases_are_made_of_vocabulary_words_only, inflections_and_lookalikes_never_classify, every_language_in_the_file_reaches_every_trick, no_plain_english_word_opens_ordinary_command_lines_or_chat_messages	evaluation over the embedded constant plus pinned lists	1, 3, 7, 9
tricks-lexicon-disjoint	table-property	TrickLexicon::all_rows, Lexicon::with_languages, Lexicon::classify_token, Lexicon::scan	Let S(s) = classify_token(Builtin(["all"]), s) orElse the class of the first match of scan(Builtin(["all"]), s, all opts on). For every embedded trick row r: if r.kind ≠ Vocative, ∀ surface s of r: S(s) = None, unless r.kind = Trick(Speak) ∧ S(s) = Feline ∧ Fold(s) ∈ {meow, mew, miaow}; and ∀ r, ∀ s: S(s) ≠ Profanity. Non-vacuity: S("meow") = Feline, S("bat") = Animal, S("fuck") = Profanity, and each of meow/mew/miaow classifies as plain Speak.	tests/tricks.rs::no_trick_address_or_filler_surface_is_a_shared_sparkle_lexicon_surface, nothing_in_the_vocabulary_is_profanity_in_any_language	evaluation of both builds over embedded constants	1, 3, 7, 9
```

### Distribution by kind (42)

| kind | count | ids |
|---|---|---|
| contract | 21 | fold-denotation, tokenization-whole-word, code-context-guard, classify-token-fallbacks, scan-policy-guards, marks-required-gate, cjk-maximal-munch, build-surface-bounds, build-key-set, insert-class-merge, ambiguous-gating, lang-interning, form-identity, every-key-round-trips, build-conflicts-channel, row-line-format, trick-row-order, trick-compile-laws, trick-classify-denotation, trick-elongation-fold, trick-authored-round-trip |
| table-property | 11 | fold-decomposition-invariant, char-class-tables, builtin-lexicon-wellformed, builtin-lexicon-coverage, builtin-lexicon-false-positive-avoidance, english-dictionary-collision-gates, trick-code-bijection, trick-blurb-table, tricks-embedded-wellformed, tricks-embedded-coverage, tricks-lexicon-disjoint |
| invariant | 3 | fold-idempotent, cjk-prefilter-sound, species-iff-animal |
| equivalence | 3 | ascii-early-out-equivalence, scan-scratch-stateless, builtin-build-time-decode |
| refinement | 2 | langset-finite-set, toml-schema-decoding |
| other | 2 | warm-path-allocation-free, builtin-tricks-compile-peak (allocator resource contracts) |


### Proof dependency order

`char-class-tables`, `ascii-early-out-equivalence`, `langset-finite-set`, `trick-code-bijection`,
`trick-blurb-table` → `fold-denotation` → `fold-idempotent`, `fold-decomposition-invariant` →
`tokenization-whole-word`, `code-context-guard`, `classify-token-fallbacks`, `trick-elongation-fold` →
the build specs (`build-surface-bounds`, `build-key-set`, `insert-class-merge`, `marks-required-gate`,
`ambiguous-gating`, `lang-interning`, `species-iff-animal`, `cjk-prefilter-sound`, `form-identity`,
`build-conflicts-channel`) → the scan specs (`scan-policy-guards`, `cjk-maximal-munch`,
`scan-scratch-stateless`) → `every-key-round-trips`. The tricks specs follow the same pattern:
`trick-row-order` → `trick-compile-laws` → `trick-classify-denotation` → `trick-authored-round-trip`.
The embedded-data table properties (`builtin-lexicon-*`, `tricks-embedded-*`, `tricks-lexicon-disjoint`,
`english-dictionary-collision-gates`) come last. They are evaluations of the proved build and scan
over the embedded constants, and they need `toml-schema-decoding` (gap 9) for the decode step.
`warm-path-allocation-free` waits on the effects lane (gap 8).

## 2. Test → spec map (TSV)

`primary` is the spec that retires the test. `also` lists the other specs the same test samples.
Paths are relative to `crates/aterm-lexicon`.

```tsv
file	test	primary	also
src/fold.rs	fold_greek_final_sigma	fold-denotation	
src/fold.rs	fold_is_idempotent	fold-idempotent	
src/fold.rs	keeps_indic_marks	fold-denotation	
src/fold.rs	cjk_detected_as_no_space	char-class-tables	
src/lib.rs	cjk_prefilter_admits_every_surface_it_gates	cjk-prefilter-sound	
src/lib.rs	romanian_fut_forms_are_ambiguous_in_ro_and_all_but_not_default_english	ambiguous-gating	insert-class-merge
src/lib.rs	padded_entry_lang_trims_consistently_for_interning_gating_and_known_check	ambiguous-gating	lang-interning, build-conflicts-channel
src/lib.rs	resident_scan_scratch_matches_compatibility_and_stops_growing	scan-scratch-stateless	warm-path-allocation-free
src/lib.rs	cjk_run_scratch_reuse_preserves_matches_and_ignore	cjk-maximal-munch	scan-scratch-stateless
src/lib.rs	ignore_guard_keys_on_full_token_fold_not_stripped_stem	scan-policy-guards	classify-token-fallbacks
src/lib.rs	suffix_expansion_is_globally_bounded	build-surface-bounds	build-conflicts-channel
src/lib.rs	no_space_surface_length_is_bounded	build-surface-bounds	build-conflicts-channel
src/lib.rs	langset_bitset_mechanics	langset-finite-set	
src/lib.rs	lang_interning_overflow_collapses_to_unknown	lang-interning	
tests/animal_multilingual.rs	turkey_speaks_chinese_japanese_and_korean	builtin-lexicon-coverage	cjk-maximal-munch
tests/animal_multilingual.rs	big_animals_match_whole_word_in_mixed_cjk_text	builtin-lexicon-coverage	tokenization-whole-word, cjk-maximal-munch
tests/animal_multilingual.rs	single_char_animal_forms_ride_the_cjk_single_char_gate	cjk-maximal-munch	
tests/animal_multilingual.rs	panda_compounds_are_pandas_not_suppressions_and_never_cats	cjk-maximal-munch	insert-class-merge
tests/animal_multilingual.rs	prefix_bomb_exceptions_stay_silent_and_animals_survive	builtin-lexicon-false-positive-avoidance	cjk-maximal-munch
tests/animal_multilingual.rs	homograph_bombs_are_language_gated	builtin-lexicon-false-positive-avoidance	ambiguous-gating
tests/animal_multilingual.rs	maximal_munch_and_particles_behave	cjk-maximal-munch	
tests/animal_multilingual.rs	expanded_builtin_stays_conflict_free	builtin-lexicon-wellformed	build-conflicts-channel
tests/animal_species.rs	animal_surfaces_scan_with_their_species	builtin-lexicon-coverage	species-iff-animal
tests/animal_species.rs	animal_class_never_steals_existing_families	builtin-lexicon-coverage	insert-class-merge
tests/animal_species.rs	non_animal_matches_carry_no_species	species-iff-animal	
tests/animal_species.rs	animal_words_are_whole_word_only	tokenization-whole-word	
tests/corpus.rs	listed_surfaces_reach_their_class	builtin-lexicon-coverage	fold-denotation
tests/corpus.rs	bare_cat_is_opt_in	scan-policy-guards	
tests/corpus.rs	possessive_and_punctuation	classify-token-fallbacks	code-context-guard (gap in coverage, §5.3)
tests/corpus.rs	decomposed_diacritic_matches	fold-decomposition-invariant	
tests/corpus.rs	scunthorpe_problem	tokenization-whole-word	
tests/corpus.rs	code_and_path_context_suppressed	code-context-guard	
tests/corpus.rs	code_and_path_context_suppressed_in_no_space_scripts_too	code-context-guard	cjk-maximal-munch
tests/corpus.rs	a_marks_required_short_feline_is_not_the_bare_cat_command	scan-policy-guards	marks-required-gate
tests/corpus.rs	a_bare_claimant_keeps_its_key_matchable_across_a_precedence_replacement	insert-class-merge	marks-required-gate
tests/corpus.rs	cjk_feline_compounds_match	cjk-maximal-munch	
tests/corpus.rs	cjk_single_char_is_opt_in	cjk-maximal-munch	
tests/corpus.rs	cjk_incidental_compounds_suppressed	cjk-maximal-munch	
tests/corpus.rs	every_surface_round_trips	every-key-round-trips	
tests/corpus.rs	cross_class_homograph_resolves_to_profanity	insert-class-merge	
tests/corpus.rs	arabic_hebrew_clitics_are_stripped	classify-token-fallbacks	
tests/corpus.rs	ignore_set_suppresses_matches	scan-policy-guards	cjk-maximal-munch
tests/corpus.rs	user_override_extends_coverage	build-key-set	
tests/corpus.rs	user_unscannable_surfaces_surface_as_conflicts	build-conflicts-channel	build-key-set
tests/corpus.rs	lossy_folded_surfaces_do_not_match_their_bare_skeleton	marks-required-gate	
tests/corpus.rs	user_override_opts_a_bare_skeleton_in	marks-required-gate	build-key-set
tests/corpus.rs	emphasis_positive_forms	build-key-set	scan-policy-guards
tests/corpus.rs	emphasis_negatives	tokenization-whole-word	
tests/corpus.rs	class_precedence_total_order	insert-class-merge	
tests/corpus.rs	form_hash_populated_and_position_independent	form-identity	
tests/corpus.rs	form_id_distinguishes_supported_morphology_from_its_stem	form-identity	classify-token-fallbacks
tests/corpus.rs	cjk_homograph_neko_unions_ja_zh	insert-class-merge	
tests/corpus.rs	primary_lang_is_first_toml_appearance	lang-interning	langset-finite-set
tests/corpus.rs	ambiguous_gating_keeps_semantics_and_ids_stay_config_independent	lang-interning	ambiguous-gating
tests/corpus.rs	cross_class_winner_keeps_only_its_own_langs	insert-class-merge	
tests/corpus.rs	empty_lang_interns_as_unknown	lang-interning	
tests/corpus.rs	form_hash_populated_on_cjk_path	form-identity	
tests/corpus.rs	user_short_emphasis_surface_matches	scan-policy-guards	build-key-set
tests/corpus.rs	non_user_short_emphasis_surface_stays_suppressed	scan-policy-guards	
tests/corpus.rs	form_hash_helpers_match_scan_semantics	form-identity	
tests/corpus.rs	a_configured_language_the_data_never_names_is_reported_not_swallowed	build-conflicts-channel	
tests/corpus.rs	an_unknown_entry_mode_is_reported_and_its_stems_are_not_lost_in_silence	build-conflicts-channel	build-key-set
tests/corpus.rs	a_typod_key_in_lexicon_data_is_a_parse_error_not_an_empty_entry	toml-schema-decoding	
tests/corpus.rs	the_builtin_decoded_at_build_time_is_the_lexicon_of_the_data_files	builtin-build-time-decode	toml-schema-decoding
tests/corpus.rs	an_entry_whose_every_surface_is_unmatchable_is_reported	build-conflicts-channel	
tests/english_fold_collisions.rs	no_unambiguous_surface_is_an_ordinary_english_word	english-dictionary-collision-gates	(COULD NOT RUN without the host's web2, §5.4)
tests/english_fold_collisions.rs	no_english_word_transits_an_unambiguous_profanity_prefix	english-dictionary-collision-gates	(COULD NOT RUN without the host's web2, §5.4)
tests/english_fold_collisions.rs	typing_dmg_is_silent_and_the_marked_form_still_fires	builtin-lexicon-false-positive-avoidance	marks-required-gate
tests/english_fold_collisions.rs	typing_pick_is_silent_all_the_way_through	builtin-lexicon-false-positive-avoidance	
tests/lexicon_alloc.rs	the_counter_sees_this_threads_allocations_and_no_other_threads	NON-SPEC (test-only code)	
tests/lexicon_alloc.rs	the_peak_is_the_most_this_thread_held_at_once	NON-SPEC (test-only code)	
tests/profanity_reaches_major_languages.rs	major_languages_have_a_working_f_bomb_by_default	builtin-lexicon-coverage	
tests/profanity_reaches_major_languages.rs	genuinely_ambiguous_surfaces_stay_gated	builtin-lexicon-false-positive-avoidance	ambiguous-gating
tests/profanity_reaches_major_languages.rs	ordinary_chinese_does_not_detonate	builtin-lexicon-false-positive-avoidance	cjk-maximal-munch
tests/scan_scratch_alloc.rs	warmed_scan_scratch_is_allocation_free_across_every_scanner_lane	warm-path-allocation-free	
tests/serde_corpus.rs	lexicon_documents_answer_as_recorded	toml-schema-decoding	(golden answer also pins message text and Debug, §5.12)
tests/tricks.rs	the_embedded_vocabulary_has_no_conflicts_under_any_language_configuration	tricks-embedded-wellformed	
tests/tricks.rs	every_trick_has_a_plain_english_word	tricks-embedded-coverage	
tests/tricks.rs	every_authored_surface_round_trips_to_its_own_role	trick-authored-round-trip	
tests/tricks.rs	authored_surfaces_are_lowercase_unique_within_a_language_and_fit_the_token_window	tricks-embedded-wellformed	
tests/tricks.rs	listing_rows_are_grouped_by_language_with_tricks_then_vocatives_then_fillers	trick-row-order	
tests/tricks.rs	no_plain_english_word_opens_ordinary_command_lines_or_chat_messages	tricks-embedded-coverage	
tests/tricks.rs	the_iconic_bare_verbs_answer_unaddressed_and_the_everyday_openers_do_not	tricks-embedded-coverage	
tests/tricks.rs	no_surface_would_hurt_someone_who_presses_enter	tricks-embedded-wellformed	
tests/tricks.rs	a_drawn_out_word_is_still_the_word	trick-elongation-fold	
tests/tricks.rs	praise_and_scolding_never_fire_without_the_pet_being_named	tricks-embedded-coverage	
tests/tricks.rs	the_cat_speak_the_internet_knows_is_understood	tricks-embedded-coverage	
tests/tricks.rs	the_natural_phrases_are_made_of_vocabulary_words_only	tricks-embedded-coverage	
tests/tricks.rs	inflections_and_lookalikes_never_classify	tricks-embedded-coverage	trick-elongation-fold
tests/tricks.rs	classify_folds_case_and_strippable_marks	trick-classify-denotation	fold-denotation
tests/tricks.rs	the_cached_english_table_is_the_default_configuration	trick-classify-denotation	
tests/tricks.rs	no_trick_address_or_filler_surface_is_a_shared_sparkle_lexicon_surface	tricks-lexicon-disjoint	
tests/tricks.rs	nothing_in_the_vocabulary_is_profanity_in_any_language	tricks-lexicon-disjoint	
tests/tricks.rs	no_ungated_non_english_surface_is_an_english_dictionary_word	english-dictionary-collision-gates	(COULD NOT RUN without the host's web2, §5.4)
tests/tricks.rs	every_language_in_the_file_reaches_every_trick	tricks-embedded-coverage	
tests/tricks.rs	trick_codes_round_trip_and_all_is_in_declaration_order	trick-code-bijection	
tests/tricks.rs	row_line_is_stable	row-line-format	
tests/tricks.rs	every_listing_line_of_the_embedded_file_is_one_line_of_key_value_fields	row-line-format	tricks-embedded-wellformed, trick-blurb-table
tests/tricks.rs	an_unknown_trick_id_is_a_conflict_and_its_row_is_skipped	trick-row-order	
tests/tricks.rs	a_surface_claimed_under_two_roles_keeps_the_first_and_says_so	trick-compile-laws	
tests/tricks.rs	a_cross_language_claim_is_won_by_the_language_that_opens_the_file	trick-compile-laws	trick-row-order
tests/tricks.rs	the_same_role_claimed_again_merges_silently	trick-compile-laws	
tests/tricks.rs	a_surface_that_is_not_one_token_is_dropped_and_named	trick-compile-laws	
tests/tricks.rs	a_cjk_row_takes_no_space_script_surfaces_only_and_matches_the_whole_run	trick-compile-laws	trick-classify-denotation
tests/tricks.rs	a_single_character_no_space_surface_always_needs_the_vocative	trick-compile-laws	trick-classify-denotation
tests/tricks.rs	no_lone_ideograph_in_the_embedded_file_fires_unaddressed	trick-compile-laws	tricks-embedded-wellformed
tests/tricks.rs	a_marked_surface_needs_marked_typing_unless_the_bare_spelling_is_listed	trick-compile-laws	trick-classify-denotation
tests/tricks.rs	gated_rows_stay_silent_until_their_language_is_listed_and_english_is_never_gated	trick-compile-laws	
tests/tricks.rs	gated_rows_of_the_embedded_file_are_silent_by_default	trick-compile-laws	tricks-embedded-wellformed
tests/tricks.rs	a_surface_longer_than_the_token_window_is_dropped_and_named	trick-compile-laws	
tests/tricks.rs	a_row_without_a_language_is_a_conflict_and_a_typo_in_a_key_is_a_parse_error	trick-row-order	toml-schema-decoding
tests/tricks.rs	the_exported_code_context_set_is_the_one_the_scanner_uses	code-context-guard	char-class-tables
tests/tricks.rs	the_builtin_decoded_at_build_time_is_the_vocabulary_of_the_data_file	builtin-build-time-decode	trick-row-order
tests/tricks.rs	an_unglued_surface_is_a_word_whole_and_no_half_of_a_run	trick-classify-denotation	trick-compile-laws, tricks-embedded-coverage
tests/tricks.rs	an_unglued_entry_that_names_nothing_in_its_row_is_a_conflict	trick-compile-laws	trick-classify-denotation
tests/tricks.rs	one_claimant_keeping_a_surface_out_of_the_glued_reading_keeps_it_out	trick-compile-laws	trick-classify-denotation
tests/tricks_alloc.rs	a_warmed_classify_is_allocation_free_on_every_lane	warm-path-allocation-free	
tests/tricks_alloc.rs	compiling_the_embedded_vocabulary_peaks_under_2_mib	builtin-tricks-compile-peak	
```

## 3. Coverage

- 131 tests read. 128 map to one of the 41 specs, and 3 are not spec samples (§4).
- The biggest collapses are:
  - `trick-compile-laws`: 11 primary;
  - `builtin-lexicon-coverage`: 11;
  - `tricks-embedded-coverage`: 9;
  - `cjk-maximal-munch`: 7;
  - `scan-policy-guards`: 7;
  - `insert-class-merge`: 7;
  - `builtin-lexicon-false-positive-avoidance`: 6.
- Each of the five embedded-data table properties (`builtin-lexicon-*`, `tricks-embedded-*`,
  `tricks-lexicon-disjoint`) is an evaluation over the shipped data, not a theorem about
  arbitrary input. That holds 31 tests together. They are worth proving only after the build
  and scan contracts are proved, because then the evaluation runs on the reflected code
  rather than on a copy.

## 4. Tests that are not spec samples

Status 2026-10-16: deleted `tests/corpus.rs::cat_command_not_decorated_by_default` and `src/lib.rs::embedded_lexicon_parses_and_has_no_conflicts`.

Status 2026-10-17: those two duplicates' rows are gone from this table and from §2.

Status 2026-10-28: deleted `tests/tricks.rs::the_dictionary_gate_catches_an_ungated_foreign_english_word_and_clears_a_gated_one` (and its §2 row).

| test | why | action |
|---|---|---|
| tests/lexicon_alloc.rs::the_counter_sees_this_threads_allocations_and_no_other_threads | It tests the test binary's own `CountingAllocator` and thread-local counter, which is test-only code with no production caller. It is a harness self-check that matters only while the allocation pins exist. | **delete** (with the allocator harness, once `warm-path-allocation-free` is a proof) |
| tests/lexicon_alloc.rs::the_peak_is_the_most_this_thread_held_at_once | The same harness's live-byte high-water mark, checked against one known allocation. Test-only code, no production caller. | **delete** (with the allocator harness, once `builtin-tricks-compile-peak` is a proof) |

Action counts: delete 2, keep 0, rewrite 0.

## 5. Implied specs that look wrong, or disagree with the code

1. **Resolved: `a_marked_surface_needs_marked_typing_unless_the_bare_spelling_is_listed` now exercises stroked-letter ASCII folding alongside non-ASCII vowel hooks.**
   - Previously, an older comment claimed Vietnamese `nhảy` folded to ASCII `nhay` (in fact `ả` is outside `fold::base_letter`, so `fold("nhảy") == "nhảy"`).
   - The false comment is gone, and the test now also checks Polish `łapa` (`fold("łapa") == "lapa"`, an ASCII `marks_required` key) alongside `couché` and `nhảy`.
2. **Resolved: `src/fold.rs` no longer makes an unqualified NFD claim, and `fold-decomposition-invariant` carries the `dom(base)` domain restriction.**
   - `fold::base_letter` maps lowercase precomposed accented Latin and monotonic Greek letters in its table; precomposed Vietnamese letters outside `dom(base_letter)` fold to themselves, while decomposed base+mark sequences drop strippable marks.
   - `src/fold.rs`'s module doc no longer claims full NFD equivalence, and `fold-decomposition-invariant` states the `dom(base_letter)` domain explicitly.
3. **A possessive escapes the short-token guards.**
   - `try_spaced_token`'s bare-cat and emphasis guards measure `|Fold(full token)|`.
   - So `cat's`, `cat’s` and `mew's` decorate as Feline with `allow_bare_cat = false`, because the folded full token has 5 chars.
   - `possessive_and_punctuation` only probes `the cat's toy` with `allow_bare_cat` on, so this is untested and may be unintended.
   - Decide whether the guard should measure the stem. The spec `scan-policy-guards` states the code as it is.
4. **The dictionary tests depend on the host's word list.**
   - These read the host's Webster's Second (`/usr/share/dict/web2`, which macOS's `/usr/share/dict/words` names), the list their allowlists are curated against:
     - `english_fold_collisions::no_unambiguous_surface_is_an_ordinary_english_word`;
     - `english_fold_collisions::no_english_word_transits_an_unambiguous_profanity_prefix`;
     - `tricks::no_ungated_non_english_surface_is_an_english_dictionary_word`.
   - On a host without it, which includes most Linux machines (Debian's `words` is `wamerican`, with possessives and inflections web2 lacks), they end COULD NOT RUN. Until 2026-10-16 they read `/usr/share/dict/words`, passed silently without one and failed against Debian's list. They become a spec only once the word list is vendored.
5. **`ascii-early-out-equivalence` is checked at compile time.**
   - It is a 128-value evaluation. A `const` check in `fold.rs` evaluates the tables over every ASCII scalar, so a table that names one fails the build.
   - No scan of `fold.rs` for `0x` literals stands in for it; such a scan counts hex inside comments and breaks on a rename or a reformat.
6. **Resolved: `fold.rs::keeps_indic_marks` asserts exact equality.** `keeps_indic_marks` now checks
   `assert_eq!(fold("बिल्ली"), "बिल्ली")` instead of a weak disjunction.
7. **Resolved: `Lexicon::build` trims `e.lang` once at ingest.**
   - `Lexicon::build` now binds `let lang = e.lang.trim()` at the top of the entry loop and passes `lang` to `data_langs`, `intern_lang`, `langs.enabled(lang)`, and conflict diagnostics, matching `TrickLexicon`'s `stage_row`.
   - `src/lib.rs::padded_entry_lang_trims_consistently_for_interning_gating_and_known_check` pins agreement across interning, ambiguous gating, and unknown-language conflict reporting.
8. **Resolved: `form_hash` tests assert exact `aterm_lexicon::form_hash(key)` equality.**
   - `form_hash_populated_and_position_independent` and `form_hash_populated_on_cjk_path` now assert `form_hash == aterm_lexicon::form_hash(key)` directly instead of unprovable `form_hash != 0`.
9. **Resolved: data-size range assertions removed from `cjk_prefilter_admits_every_surface_it_gates`.**
   - `cjk_prefilter_admits_every_surface_it_gates` now asserts only `cjk-prefilter-sound` (every no-space surface's head char appears in `cjk_heads` with a bound `>=` its char count and `<= max_cjk`), without the `50 < |cjk_heads| < 5000` selectivity heuristic.
10. **Resolved: tautological clauses removed from `tricks.rs`.**
    - `the_cached_english_table_is_the_default_configuration` no longer checks `OnceLock`'s `std::ptr::eq`, and `a_surface_longer_than_the_token_window_is_dropped_and_named` no longer pins `MAX_SURFACE_CHARS == 32`.
11. **Resolved: token-boundary decoys moved from `code_and_path_context_suppressed` to `scunthorpe_problem`.**
    - `cat_food`, `my_cat`, `concat_str`, and `cat5` (where `_` and digits extend the token under `tokenization-whole-word`) now live in `corpus.rs::scunthorpe_problem`, leaving `code_and_path_context_suppressed` sampling `code-context-guard` alone.
12. **A golden snapshot also pins prose.**
    - `serde_corpus.rs::lexicon_documents_answer_as_recorded` compares against a recorded file (`crates/aterm-json/test_support/serde_corpus.rs`).
    - Each recorded answer embeds conflict-message text and the `Debug` output of `TrickRow`, so rewording a message fails it with no decode change.
    - `toml-schema-decoding` is the law; the recorded equality is only its instance on the corpus.

## 6. Proof order (easiest high-value first)

No spec here is blocked only by gap 0. Every one needs at least non-scalar carriers (gap 1), E6 body
widening (gap 3), quantified clauses (gap 5), or build-script codegen (gap 9). So the order goes by
the smallest set of gaps beyond 0.

1. **`ascii-early-out-equivalence`** (gaps 3, 5).
   - It is a 128-case evaluation over `u32` range matches, with no strings and no loops.
   - It retires the source-text-scanning test (§5.5).
   - It makes every later fold proof safe to state without the early-outs.
2. **`langset-finite-set`** (gaps 3, 5, 7, 13).
   - It is pure `u64` bit arithmetic (`|`, `&`, shifts, `count_ones`, `trailing_zeros`), which is `bv_decide`/`omega`-class work.
   - Only `iter()` needs the loop/closure lanes. The other six clauses are blocked by gaps 3 and 5 alone, so prove those first and add `iter` later.
3. **`trick-code-bijection`** and **`trick-blurb-table`** (gaps 1, 3, 7).
   - Each is a 17-case evaluation of an enum `match` returning `&'static str`.
   - They are the first specs that need `&str` carriers (gap 1), but they need only constant strings.
   - This makes them a good first customer for gap 1.
4. **`char-class-tables`** (gaps 1, 3, 5).
   - It is range-table predicates on `char`. The `is_no_space_surface` clause brings in `Iterator::all`.
   - It is the base fact under `tokenization-whole-word` and `code-context-guard`.
5. **`trick-elongation-fold`** (gaps 1, 3, 5, 7).
   - It works on a fixed `[char;64]` window with `saturating_add` loops, and has obvious variants.
   - It is self-contained and retires its test outright.
6. **`fold-denotation`** → **`fold-idempotent`**. These are the highest-value core: the next 20+ specs are stated through `Fold`.
   - They need a Clean model of std's lowercase table (gap 1).
   - The idempotence lemma is an exhaustive scalar evaluation (gap 14 for elaborator performance).
7. **`cjk-prefilter-sound`** and **`build-surface-bounds`**. These are loop invariants over the build with saturating `usize` arithmetic.
   - They are invariant-shaped (gap 7) rather than content-shaped.
   - They retire 3 tests, including the 2^20 cap regression.

The scan-level contracts (`tokenization-whole-word`, `classify-token-fallbacks`, `scan-policy-guards`,
`cjk-maximal-munch`) come after these. They need `&self`/`&mut` scratch views (gap 2) and the
`Vec<char>` content model (gap 1).

### Gap histogram (specs blocked by each audit §7 gap)

| gap | specs |
|---|---|
| 0 | 42 (prerequisite for all; closed locally by seal `150efe4a`, owner-gated) |
| 1 | 39 |
| 2 | 14 |
| 3 | 38 |
| 4 | 4 |
| 5 | 28 |
| 7 | 37 |
| 8 | 3 |
| 9 | 13 |
| 12 | 1 |
| 13 | 9 |
| 14 | 2 |
