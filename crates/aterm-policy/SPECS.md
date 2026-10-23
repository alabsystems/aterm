<!-- Copyright 2026 Andrew Yates -->
<!-- SPDX-License-Identifier: Apache-2.0 -->

# aterm-policy: specs recovered from its unit tests

This is the crate's migration worklist. Each of the 19 specs below is to be proved through
Trust/Clean about the compiled Rust function, and the tests it subsumes then deleted. None is
proved yet.

The specs restate what the crate's 27 `#[test]`s sample (`src/engine.rs` 3, `src/limits.rs` 12,
`src/selector.rs` 3, `src/tests.rs` 8, `tests/serde_corpus.rs` 1), read against the code they
reach (`src/lib.rs`, `src/profiles.rs`, `src/aliases.rs`). Decisions are consumed in
`crates/aterm-core/src/terminal/policy_bridge.rs`. The claims come from reading the code, not
from running it.

Result: **19 specs**; every test is a sample of one of them.

Gap numbers in the `blocking_gaps` column refer to the table in §7 of
`docs/AUDIT-trust-for-test-migration-2026-10-15.md`. Gap 0 (the installed seal publishes no
Lean-discharged row) blocks every spec, so it is listed on every row.

## 0. Notation used in the statements

- `rank(o)` = `OriginTag::trust_rank(o)`. In declaration order: Host 0, ConfigFile 1, User 2,
  UserTyped 3, Ai 4, PtySafe 5, Pty 6, NetworkUntrusted 7.
- `strict(r)`: the response strictness order Drop 4, Warn 3, Rewrite 2, Ask 1, Execute 0.
  It exists only as the `#[cfg(test)]` `profiles::refinement::response_rank`.
  **Recommendation:** make it a spec-only fn, because `profile-refinement` is stated with it.
- `Sel(s)` = `SequenceSelector::parse(s)`. `M(sel,seq)` = `sel.matches(seq)`.
  `pm(p,a)` = `SelectorParam::matches(p,a)`.
- `E_P` = `PolicyEngine::new(P)`. `Eval_P(seq,o)` = `E_P.evaluate(&seq,o)`. `Eval_X` with
  X in {Perm, Std, Hard} uses `profiles::{permissive,standard,hardened}()`.
- `tier(i)` for a rule whose selector parses to `s_i`: 0 if `s_i.function ≠ Wildcard` and
  `s_i.major = Some(_)`; 1 if `s_i.function ≠ Wildcard` and `s_i.major = None`; 2 if
  `s_i.function = Wildcard`.
- Sequence classes, over `seq = {function, major, suffix, params}`:
  - `S52set(seq)`: Osc, `major = Some(52)`, `|params| ≥ 2`, `params[0] ≠ ""`,
    `params[1] ∉ {"", "?"}`.
  - `S52q(seq)`: as `S52set`, except `params[1] = "?"`.
  - `S4q`, `S4set`: the same two predicates with `major = Some(4)`.
  - `Ct(seq)`: `function = Csi` and `suffix = Some("t")`.
  - `N(seq)`: Osc with `major ∈ {Some(9), Some(99), Some(777)}`.
- Token bucket state `TB = (tokens: f64, last: Option<Instant>)`, with config
  `(C, R, M) = (capacity_bytes, refill_per_second, per_sequence_max)`, widened to u64.
  `from_config(cfg) = (C as f64, None)`. Clocks: every `T: TimeSource`; `now = clock.now()`,
  `Δ = now.saturating_duration_since(last)`.
- `RateLimiterSet` view: `(buckets: Vec<TB>, slots: Map<String, usize>)`. The accessors
  `get`, `len`, `tokens` and `is_declared` are observation views only.
- `ALIAS_TABLE` is the 7-row const in `src/aliases.rs`. `canon(a)` is the `expand_alias` match
  arm for alias `a`.
- `trim`, `splitn`, `split_once` are the `str` methods of the same names.
- `Inv_new(E)`, the bucket invariant `PolicyEngine::new` establishes. `idx ∈ buckets[k]` ⇔
  `compiled[idx] = Some(s)` ∧ `s.bucket_key() = k` ∧ `k.function ≠ Wildcard`.
  `idx ∈ wildcard_bucket` ⇔ `compiled[idx] = Some(s)` ∧ `s.function = Wildcard`. Every bucket
  is in ascending order. `engine-evaluate-first-match` is proved from it.

## 1. Specs (TSV)

Columns: `id`, `kind`, `functions`, `statement`, `subsumes (primary tests; secondary in §2)`,
`difficulty (Rust features the proof must handle)`, `blocking_gaps (audit §7)`.

```tsv
id	kind	functions	statement	subsumes	difficulty	blocking_gaps
origin-rank-table	table-property	OriginTag::trust_rank	∀ o: trust_rank(o) is o's declaration index (Host 0 … NetworkUntrusted 7). Hence trust_rank is a bijection from OriginTag onto {0..7}: injective, image exactly 0..8.	tests.rs::trust_rank_is_a_bijection_onto_0_through_7	EASY: const fn match over an 8-variant #[non_exhaustive] Copy enum returning u8; the quantifier is over a finite enum and is decidable by enumeration.	0,1,3,5
origin-dominates-order	contract	OriginTag::{dominates,trust_rank}	∀ a,b: a.dominates(b) ⇔ rank(a) ≤ rank(b). Corollaries: dominates is reflexive, antisymmetric, transitive and total; it is the chain Host > ConfigFile > User > UserTyped > Ai > PtySafe > Pty > NetworkUntrusted; Host dominates every o and every o dominates NetworkUntrusted; Pty dominates NetworkUntrusted, not conversely.	tests.rs::host_is_the_top_and_networkuntrusted_is_the_bottom	EASY given origin-rank-table: const fn calling a const fn and a u8 comparison; enum carriers; quantifiers with 2 and 3 binders.	0,1,3,4,5
selector-param-matches	contract	SelectorParam::matches	∀ p, a:&str: pm(Wildcard,a) ⇔ a ≠ ""; pm(Literal(l),a) ⇔ l = a (bytes); pm(Query,a) ⇔ a = "?"; pm(NotQuery,a) ⇔ a ≠ "?" ∧ a ≠ "".	selector.rs::not_query_param_rejects_empty_and_question	EASY: &self on an enum with a String payload; &str equality and is_empty.	0,1,2,3
bucket-key-projection	contract	SequenceSelector::{bucket_key,catch_all}	∀ sel: sel.bucket_key() = BucketKey{function: sel.function, major: sel.major}. catch_all() = {Wildcard, None, None, []}, so its key is (Wildcard, None).	-	TRIVIAL: &self field projection; Copy enum and Option<u32>; empty Vec constructor.	0,1,2
selector-matches-denotation	contract	SequenceSelector::matches	∀ sel, seq: M(sel,seq) ⇔ sel.function = Wildcard ∨ (sel.function = seq.function ∧ (sel.major = None ∨ sel.major = seq.major) ∧ (sel.suffix = None ∨ sel.suffix = seq.suffix) ∧ |sel.params| ≤ |seq.params| ∧ ∀ i < |sel.params|: pm(sel.params[i], seq.params[i])). The selector matches a prefix of the dispatched params; a selector longer than the sequence never matches.	selector.rs::selector_matching	MEDIUM: &self; Option<u32>/Option<String> comparison; Vec<SelectorParam> zipped with Vec<String> in a for loop with early return.	0,1,2,3,5,7
selector-parse-denotation	contract	SequenceSelector::parse, parse_compiled, parse_{osc,csi,dcs}_body, split_major, collect_osc_params, push_params, compile_param, parse_major, parse_major_or_wildcard, is_csi_final_byte, non_empty_string	∀ src not an alias (see selector-alias-expansion), t = trim(src): (1) t = "" ⇒ Err(Empty); t = "*" ⇒ Ok(catch_all()). (2) Else f,m,r = the trimmed parts of splitn(3,' ') ("" when absent); f ∉ {OSC,CSI,DCS} ⇒ Err(UnknownFunction(f)). (3) OSC: (ms,lo) = split_once(m,';'); major = None if ms ∈ {"*",""}, else Some(ms.parse::<u32>()) or Err(BadMajor(ms)); params = compile_param over split(lo,';') if lo ≠ "", then over split(trim_start_matches(r,';'),';') if r ≠ ""; suffix = None. (4) compile_param(trim x): Wildcard for * <idx> <selection> <name> <value> <named> <indexed>; Query for ?; NotQuery for <not-question> <base64_non_question>; Literal(x) otherwise. (5) CSI: params = []; (major,suffix) = (None,Some(m)) if m is one char that is ASCII alphabetic or one of @ ` ~ ^ and r = ""; (None,ne(r)) if m = "*"; (Some(0),ne(r)) if m = "?"; (Some(m.parse::<u32>()),ne(r)) otherwise, or Err(BadMajor(m)); ne(r) = None if trim(r) = "" else Some(trim(r)). (6) DCS: always Ok{major None, suffix Some(m ++ r), params []}.	selector.rs::selector_spellings_parse	HARD: &str trim/splitn/split_once/split/trim_start_matches/chars; str::parse::<u32>; format!; String allocation; Vec push in for loops over split iterators; Result and ?; match on string literals.	0,1,3,4,7
selector-alias-expansion	table-property	aliases::lookup, selector::expand_alias, SequenceSelector::parse	lookup(a) = ALIAS_TABLE.iter().find(|e| e.alias = a), exact bytes, so case-sensitive: lookup("osc 52 set") = None and its parse is Err(UnknownFunction("osc")). ∀ s with trim(s) = an alias a: parse(s) = parse(canon(a)), and: OSC 52 set ⇒ {Osc,Some(52),None,[Wildcard,NotQuery]}; OSC 52 query ⇒ {Osc,Some(52),None,[Wildcard,Query]}; OSC 4 query/set ⇒ the same shapes with major 4; OSC 21 set named/indexed ⇒ {Osc,Some(21),None,[Wildcard]}; response any ⇒ catch_all(). Every table alias parses Ok; any other string passes through to selector-parse-denotation.	-	MEDIUM given selector-parse-denotation: Iterator::find with a closure over a &'static [AliasEntry]; match on &str literals with a guard calling lookup.	0,1,3,4,7,13
engine-evaluate-first-match	refinement	PolicyEngine::{new,evaluate,walk_bucket,policy}, Decision::fell_through	∀ P, seq, o, E = PolicyEngine::new(P). Rule i is ok ⇔ Sel(P.rules[i].sequence) = Ok(s_i) ∧ M(s_i,seq) ∧ o.dominates(P.rules[i].origin_min). If some rule is ok, i* = the ok index with least (tier(i), i), and E.evaluate(seq,o) = Decision{P.rules[i*].response, Some(i*), P.rules[i*].rate_limit as Arc<str>}. Else Decision{P.defaults.unmatched, None, None}. So the bucketed engine (exact (function,major) bucket, then (function,None), then wildcard; declared order within each) equals a linear first-match scan by (tier, index). Corollaries: an unparsable selector never matches; an origin-gate failure falls through to later rules; an empty rule list yields the default. Also E.policy() = &P, and evaluate is total (no panic).	engine.rs::{malformed_rule_selector_is_skipped, origin_gate_failure_does_not_short_circuit}, tests.rs::single_matching_rule_decides_for_every_response_and_origin_gate	HARD: HashMap<BucketKey,Vec<usize>> built in an enumerate loop with entry().or_default(); Vec<Option<SequenceSelector>>; Arc<str>; for/continue; let-chains; Option get/and_then/cloned/flatten. Needs the bucket invariant from new: idx ∈ buckets[k] ⇔ compiled[idx] = Some(s) ∧ s.bucket_key() = k ∧ k.function ≠ Wildcard; idx ∈ wildcard bucket ⇔ compiled[idx] = Some(s) ∧ s.function = Wildcard; each bucket ascending.	0,1,2,3,4,5,7
builtin-profile-denotation	contract	profiles::{permissive,standard,hardened}, PolicyEngine::evaluate	∀ seq, o: Eval_Perm(seq,o) = Decision{Execute, Some(0), Some("clipboard")}. Eval_Std(seq,o).response = Ask (rate_limit "clipboard") if S52set ∧ rank(o) ≤ 2; Drop if (S52q ∨ Ct) ∧ o = Host; Warn (rate_limit "notifications") if N ∧ rank(o) ≤ 2; Execute otherwise. Eval_Hard(seq,o).response = Execute if (o = Host ∧ ¬(S52set ∨ S52q ∨ Ct ∨ N)) ∨ (rank(o) ≤ 1 ∧ (S4q ∨ S4set)); Drop otherwise. Corollary (fail-closed): ∀ o with rank(o) ≥ 2, ∀ seq: Hardened answers Drop.	engine.rs::builtin_profile_decisions, tests.rs::hardened_fails_closed_on_unknown_for_untrusted_origins	MEDIUM given engine-evaluate-first-match and selector-alias-expansion: constant fns building Vec<Rule> with Strings; quantified over seq (Vec<String>, Option<String>). Matching splits seq into finitely many classes, so the statement is decidable once the class lemma is proved.	0,1,2,3,4,5,7
profile-refinement	invariant	profiles::{permissive,standard,hardened}, PolicyEngine::evaluate, strict (spec fn; today profiles::refinement::response_rank)	∀ seq, o: strict(Eval_Hard(seq,o).response) ≥ strict(Eval_Std(seq,o).response) ≥ strict(Eval_Perm(seq,o).response). Immediate from builtin-profile-denotation.	tests.rs::refinement_holds_over_all_sequences_and_origins	EASY given builtin-profile-denotation: three evaluate calls composed; quantification over DispatchedSequence and OriginTag; strict must be a spec-level fn.	0,1,3,4,5,7
builtin-profiles-wellformed	table-property	profiles::{permissive,standard,hardened}	For (f,tag) ∈ {(permissive,Permissive),(standard,Standard),(hardened,Hardened)}, p = f(): p.schema_version = SCHEMA_VERSION ∧ p.profile = tag; every rule with rate_limit = Some(id) has id ∈ p.rate_limits[*].id; p.defaults.unmatched = Execute, Warn, Drop respectively (strict monotone H ≥ S ≥ P); shell_integration_require_nonce = false, true, true; ∀ r ∈ hardened().rules: r.origin_min ∈ {Host, ConfigFile, User}.	tests.rs::{every_rate_limit_ref_resolves_in_each_profile, hardened_rules_only_admit_host_configfile_or_user, hardened_requires_shell_integration_nonce}	EASY: constant constructors returning a struct with Vec<Rule>, Vec<RateLimit>, String, Option<String>; a quantifier over vector elements; string membership. Decidable by evaluation once content models exist.	0,1,3,4,5,7
policy-toml-roundtrip	round-trip	Policy::to_toml, aterm_toml::from_str::<Policy>, serde derives on Policy/Defaults/Rule/RateLimit/OriginTag/Profile/Response	∀ p: Policy: p.to_toml() = Ok(s) ∧ aterm_toml::from_str::<Policy>(&s) = Ok(p). This includes empty rules or rate_limits, None rate_limit and prompt_id (skipped on serialize, defaulted on read), and arbitrary Unicode in Strings.	-	HARD: serde proc-macro output (rename_all, deny_unknown_fields, default, skip_serializing_if); the cross-crate aterm_toml serializer and parser; String escaping; Vec loops.	0,1,7,9,12
policy-decode-schema	refinement	aterm_toml::from_str::<Policy>, Policy::to_toml, serde derives (deny_unknown_fields, default, rename_all)	∀ TOML text d: from_str::<Policy>(d) = Ok(p) ⇔ d parses as TOML and its root fits the schema: no key outside the declared fields at any level (root, defaults, rules[i], rate_limits[j]); schema_version, profile, defaults.unmatched present; each rule has sequence, origin_min, response; each rate limit has id, capacity_bytes, refill_per_second; each value has its exact TOML type (integers in 0..=u32::MAX; no float/string/bool for an int); enum strings match rename_all exactly (Response snake_case; Profile and OriginTag PascalCase); rules is an array of tables and defaults a table. Then p takes those values, with shell_integration_require_nonce = false, per_sequence_max = 0, rules = [], rate_limits = [], rate_limit = None, prompt_id = None where absent, and p re-serializes as to_toml(p). Invalid TOML (duplicate key, truncation) is Err; the empty document is Err (required keys missing). Error-message text is not part of the spec.	serde_corpus.rs::policy_documents_answer_as_recorded	HARD: first-party serde derive expansion; the aterm_toml lexer and parser (another crate); integer range checks; Strings, Vecs, loops over tables.	0,1,7,9,12
token-bucket-guards	contract	TokenBucket::{from_config,try_consume}	from_config(cfg) = (C as f64, None). ∀ state, a: u64, clock: if C = 0 ∨ (M > 0 ∧ a > M) ∨ a > C then try_consume(a,clock) = false, the state (tokens, last) is unchanged and the clock is not read. So zero capacity denies every amount including 0, M = 0 disables the per-call cap, and a denied oversized call debits nothing.	limits.rs::{oversized_request_always_denied_without_draining, zero_capacity_is_hard_block, per_sequence_cap_rejects_oversized_call, per_sequence_cap_zero_disables_check}	EASY: &mut self two-state frame; u64 comparisons; u32→u64 widening; u64→f64 cast; a generic T: TimeSource whose now() is not called on these paths.	0,1,2,3,11
token-bucket-refill-debit	contract	TokenBucket::{try_consume,refill}	When no guard fires (0 < C, a ≤ C, M = 0 ∨ a ≤ M), with now = clock.now() and t1 the post-refill balance: last = None ⇒ t1 = tokens, last' = Some(now); Δ = 0 (including now < last) ⇒ t1 = tokens, last' = last; else t1 = min(tokens + (R as f64)·Δ.as_secs_f64(), C as f64), last' = Some(now). Returns t1 ≥ a as f64; tokens' = t1 − a on true, t1 on false. The step preserves 0 ≤ tokens ≤ C as f64, and tokens is never NaN.	limits.rs::refill_replenishes_after_cooldown	MEDIUM: f64 arithmetic, min and casts; Instant/Duration saturating_duration_since and as_secs_f64 (aterm_time, another crate); Option<Instant>; &mut self; generic TimeSource.	0,1,2,3,4,11,12
token-bucket-rate-safety	invariant	TokenBucket::try_consume	∀ bucket from from_config with C > 0, ∀ call trace (a_k, t_k), k = 1..n, with non-decreasing clock readings: Σ{a_k : call k granted} ≤ C + R·(t_n − t_1) seconds. Exact in a real-number model; under f64 it holds up to an explicit rounding bound. In particular with t constant at most ⌊C/a⌋ calls of size a are granted, and idling never builds a balance above C.	limits.rs::{burst_capacity_drains_then_denies, idle_bucket_caps_at_configured_max}	HARD: induction over a trace of &mut self calls using the refill-debit step lemma; f64 against a real model; generic clock.	0,1,2,3,4,5,11
limiter-set-from-policy	contract	RateLimiterSet::from_policy	∀ P, ids = the distinct P.rate_limits[*].id in first-occurrence order: |buckets| = |ids|; slots maps each id_k to k and no other key; buckets[k] = from_config(c) with c the LAST entry of P.rate_limits whose id is id_k. A duplicate id overwrites in place: it adds no slot and moves no other slot.	limits.rs::from_policy_collapses_duplicate_ids_last_wins	MEDIUM: for loop over Vec<RateLimit>; HashMap<String,usize> get/insert; Vec push/get_mut; String clone; a loop invariant over a map and a vector.	0,1,2,3,7
limiter-set-consume-slot	contract	RateLimiterSet::{try_consume_slot,try_consume,slot}	∀ S, slot, amount, clock: slot = UNDECLARED (None) or Some(i) with i ≥ |S.buckets| ⇒ returns true and S is unchanged (a slot from a wider set allows and never aliases a bucket). Some(i) with i < |S.buckets| ⇒ returns buckets[i].try_consume(amount, clock), and only buckets[i] changes (other buckets and slots are framed). The same holds for try_consume(id, …) with slot = slot(id), so an undeclared id is unlimited.	limits.rs::{known_id_enforces_bucket, slot_debits_the_bucket_its_id_names, slot_from_a_wider_set_stays_total}	MEDIUM given the token-bucket specs: &mut self with a frame over Vec elements; Vec get_mut; Option; generic TimeSource.	0,1,2,3,4,11
limiter-slot-resolution	equivalence	RateLimiterSet::{slot,try_consume,try_consume_slot}, RateLimitSlot::default, PolicyEngine::{rate_limit_slot,rate_limit_try_consume,rate_limit_try_consume_slot}	∀ set, id, a, clock: slot(id) = RateLimitSlot(slots.get(id)), declared ⇔ id is a key; try_consume(id,a,c) leaves the same state and answer as try_consume_slot(slot(id),a,c); two sets built from the same P with the same history answer the id-keyed and slot-keyed call sequences identically; RateLimitSlot::default() = UNDECLARED; the PolicyEngine methods delegate to its limiter set unchanged.	limits.rs::undeclared_slot_allows_like_an_unknown_id	EASY given limiter-set-consume-slot: HashMap<String,usize> lookup by &str; derived Default on a newtype over Option<usize>; &mut self delegation; generics.	0,1,2,4,11
```

### Distribution by kind (19)

| kind | count | ids |
|---|---|---|
| contract | 10 | origin-dominates-order, selector-param-matches, bucket-key-projection, selector-matches-denotation, selector-parse-denotation, builtin-profile-denotation, token-bucket-guards, token-bucket-refill-debit, limiter-set-from-policy, limiter-set-consume-slot |
| table-property | 3 | origin-rank-table, selector-alias-expansion, builtin-profiles-wellformed |
| refinement | 2 | engine-evaluate-first-match, policy-decode-schema |
| invariant | 2 | profile-refinement, token-bucket-rate-safety |
| round-trip | 1 | policy-toml-roundtrip |
| equivalence | 1 | limiter-slot-resolution |

### Gap histogram (specs blocked by each audit §7 gap)

| gap | specs | note |
|---|---|---|
| 0 | 19 | seal prerequisite; no spec is blocked by gap 0 alone |
| 1 | 19 | every spec states something about an enum, String, Vec, Option or struct |
| 2 | 11 | `&self`/`&mut self` views, two-state frames |
| 3 | 15 | calls, casts, loops and enum matches in bodies |
| 4 | 11 | composition of callee ensures |
| 5 | 8 | quantified clauses |
| 7 | 10 | loops |
| 9 | 2 | serde proc-macro boundary |
| 11 | 5 | generic `T: TimeSource` |
| 12 | 3 | cross-crate citation (`aterm_toml`, `aterm_time`) |
| 13 | 1 | the closure in `Iterator::find` |

### Proof dependency order

`origin-rank-table` → `origin-dominates-order`; `selector-param-matches` →
`selector-matches-denotation`; `selector-parse-denotation` → `selector-alias-expansion`;
`bucket-key-projection` + the selector specs + `origin-dominates-order` →
`engine-evaluate-first-match` → `builtin-profile-denotation` → `profile-refinement`.
`builtin-profiles-wellformed` is independent. `token-bucket-guards` + `token-bucket-refill-debit`
→ `token-bucket-rate-safety`; the bucket specs + `limiter-set-from-policy` →
`limiter-set-consume-slot` → `limiter-slot-resolution`. The two serde specs are last and
independent; `policy-decode-schema` implies `policy-toml-roundtrip` once `to_toml` is shown to
emit schema-fitting text.

## 2. Test → spec map (TSV)

`primary` is the spec that retires the test. `also` lists the other specs the same test samples.

```tsv
file	test	primary	also
src/engine.rs	malformed_rule_selector_is_skipped	engine-evaluate-first-match	selector-parse-denotation
src/engine.rs	origin_gate_failure_does_not_short_circuit	engine-evaluate-first-match	origin-dominates-order
src/engine.rs	builtin_profile_decisions	builtin-profile-denotation	engine-evaluate-first-match, selector-alias-expansion
src/limits.rs	oversized_request_always_denied_without_draining	token-bucket-guards	
src/limits.rs	zero_capacity_is_hard_block	token-bucket-guards	
src/limits.rs	per_sequence_cap_rejects_oversized_call	token-bucket-guards	
src/limits.rs	per_sequence_cap_zero_disables_check	token-bucket-guards	token-bucket-refill-debit
src/limits.rs	refill_replenishes_after_cooldown	token-bucket-refill-debit	
src/limits.rs	burst_capacity_drains_then_denies	token-bucket-rate-safety	token-bucket-refill-debit
src/limits.rs	idle_bucket_caps_at_configured_max	token-bucket-rate-safety	token-bucket-refill-debit
src/limits.rs	from_policy_collapses_duplicate_ids_last_wins	limiter-set-from-policy	
src/limits.rs	known_id_enforces_bucket	limiter-set-consume-slot	token-bucket-guards, token-bucket-refill-debit
src/limits.rs	slot_debits_the_bucket_its_id_names	limiter-set-consume-slot	limiter-set-from-policy
src/limits.rs	slot_from_a_wider_set_stays_total	limiter-set-consume-slot	
src/limits.rs	undeclared_slot_allows_like_an_unknown_id	limiter-slot-resolution	limiter-set-consume-slot
src/selector.rs	selector_spellings_parse	selector-parse-denotation	selector-alias-expansion
src/selector.rs	selector_matching	selector-matches-denotation	selector-param-matches, selector-parse-denotation
src/selector.rs	not_query_param_rejects_empty_and_question	selector-param-matches	
src/tests.rs	trust_rank_is_a_bijection_onto_0_through_7	origin-rank-table	
src/tests.rs	host_is_the_top_and_networkuntrusted_is_the_bottom	origin-dominates-order	
src/tests.rs	single_matching_rule_decides_for_every_response_and_origin_gate	engine-evaluate-first-match	origin-dominates-order
src/tests.rs	every_rate_limit_ref_resolves_in_each_profile	builtin-profiles-wellformed	
src/tests.rs	hardened_rules_only_admit_host_configfile_or_user	builtin-profiles-wellformed	
src/tests.rs	hardened_requires_shell_integration_nonce	builtin-profiles-wellformed	
src/tests.rs	hardened_fails_closed_on_unknown_for_untrusted_origins	builtin-profile-denotation	
src/tests.rs	refinement_holds_over_all_sequences_and_origins	profile-refinement	builtin-profile-denotation
tests/serde_corpus.rs	policy_documents_answer_as_recorded	policy-decode-schema	policy-toml-roundtrip
```

## 3. Coverage

- All 27 tests have a primary spec. 16 specs retire at least one test; `bucket-key-projection`,
  `selector-alias-expansion` and `policy-toml-roundtrip` retire none.
- The biggest collapses: `token-bucket-guards` (4 primary), `engine-evaluate-first-match` (3),
  `builtin-profiles-wellformed` (3), `limiter-set-consume-slot` (3).
- Proving `engine-evaluate-first-match` makes `builtin-profile-denotation` a finite case split,
  and `profile-refinement` then follows at once.

## 4. Implied specs that look wrong, or disagree with the code

1. **The Standard profile's doc comment contradicts the engine** (`src/profiles.rs`). Its Drop
   rules are gated `origin_min = Host`, and an origin that fails a gate falls through to the
   `response any` rule (`origin_min = NetworkUntrusted`, Execute). So "Clipboard query: Drop",
   "Palette reconfigure … Drop from Pty", "Window ops 1-10: Drop" and "Modal protocols: Host
   only" are false for Pty: Standard **drops** an OSC 52 clipboard query from Host but
   **executes** it from Pty. `builtin_profile_decisions` samples only the Host row of the query,
   so the inverted gate goes unnoticed. The doc's "program output … matches no rule, and so
   defers to the host" is also wrong: the wildcard matches, and `lib.rs` says a matching rule is
   authoritative. Decide which is intended; `builtin-profile-denotation` states today's code.
2. **The Rule doc says first rule top-to-bottom wins** (`src/lib.rs`), but the engine picks by
   (tier, index): exact bucket, then major-wildcard bucket, then the universal wildcard. A
   declared-earlier wildcard rule loses to a later specific one. `engine-evaluate-first-match`
   states the engine's order; the doc should be corrected.
3. **`defaults.unmatched` is dead in Standard and Permissive.** Their `response any` rule
   admits every origin, so evaluate never reaches the default there. The `lib.rs` "refinement
   invariant over the unmatched-default response" is stated over a field evaluate cannot reach;
   `builtin_profile_decisions` says so itself. The meaningful property is `profile-refinement`
   over evaluate; the field clause is kept in `builtin-profiles-wellformed` only as a table fact.
4. **Permissive declares no `"palette"` bucket.** `lib.rs` says the `"palette"` rate limit of
   every built-in profile is authoritative, but `permissive()` declares only `clipboard` and
   `response`. Under Permissive the 16-pair OSC 4/21 per-sequence cap resolves to UNDECLARED,
   which is unlimited. Also every profile's `response any` rule names the `"clipboard"` bucket,
   although permissive's comment says it keeps the response rate limiter.
5. **`PolicyEngine::rate_limit_config` reports the FIRST `RateLimit` with an id**
   (`iter().find`), while `RateLimiterSet::from_policy` enforces the LAST. On a duplicate id the
   config the engine reports is not the bucket it enforces.
   `limits.rs::from_policy_collapses_duplicate_ids_last_wins` pins only the limiter side.
6. **Two sources of truth for aliases.** `ALIAS_TABLE.expansion` and `expand_alias`'s hard-coded
   match disagree: the table's `OSC 21 set named` expansion would compile to a Literal param,
   while `expand_alias` yields a Wildcard. The `aliases::lookup(trimmed).is_some() => "*"` arm is
   dead, because all 7 table aliases are matched explicitly above it. Prefer deriving
   `expand_alias` from the table.
7. **`CSI ? h` parses to major `Some(0)`**, so such a selector matches only CSI sequences with
   major 0, not DEC private modes in general. No test covers it.
8. **Error prose pinned.** `serde_corpus.rs::policy_documents_answer_as_recorded` records
   `Err(e.to_string())`, which pins error-message text on top of the Ok/Err partition and the
   decoded value that `policy-decode-schema` covers. When migrated, record only Ok/Err and the
   decoded value.
9. **`SCHEMA_VERSION` is not checked on read.** The `lib.rs` field doc says "the reader
    rejects any value other than this constant", but `Policy` derives `Deserialize` with a
    plain `u32`, and no caller validates the value, so `from_str` accepts
    `schema_version = 2`. Either add the check (then `policy-decode-schema` gains the clause
    `schema_version = SCHEMA_VERSION`) or fix the doc. The spec states today's behaviour.
10. **The `Profile::Hardened` doc names an origin no rule uses.** It says every sequence requires
    at least `Host | ConfigFile | User`, but every hardened rule is gated at `Host` or
    `ConfigFile`. `hardened_rules_only_admit_host_configfile_or_user` pins the weaker set, and so
    does `builtin-profiles-wellformed`. The exact fact (`{Host, ConfigFile}`) is the stronger spec
    if the doc is corrected.

## 5. Proof order (easiest, high value first)

No spec is blocked by gap 0 alone. Every one also needs gap 1 (non-scalar carriers), because
even `trust_rank` is stated over an enum. For the first step, gap 1 means only finite Copy enums,
which a scalar encoding (the discriminant) may cover before full content models land. So the order prefers the specs whose remaining gaps
are the cheapest (finite enums, scalar fields, no loops, no generics, no other crate).

1. **`origin-rank-table`** then **`origin-dominates-order`**. Const fns over an 8-variant
   enum and u8 comparison; the quantifiers are finite and decidable by enumeration. They retire
   2 tests and are the origin-gate lemma `engine-evaluate-first-match` needs.
2. **`bucket-key-projection`** and **`selector-param-matches`**. Field projection and
   four-arm `&str` equality, no loops. They retire 1 test and are the leaves of the matcher.
3. **`token-bucket-guards`**. u64 comparisons with an unchanged-state frame and an unread
   clock. It retires 4 tests; only gap 11 (the generic clock) stands beyond the shared ones.
4. **`builtin-profiles-wellformed`**. Evaluating three constant constructors; decidable once
   content models exist. Retires 3 tests.
5. `selector-matches-denotation`, `token-bucket-refill-debit`, `limiter-set-from-policy`,
   `limiter-set-consume-slot`, `limiter-slot-resolution` (loops, f64, frames).
6. `selector-parse-denotation` → `selector-alias-expansion` → `engine-evaluate-first-match`
   → `builtin-profile-denotation` → `profile-refinement`. The highest value chain (7 tests),
   but string parsing and the bucket invariant make it the hardest.
7. `token-bucket-rate-safety` (trace induction under f64).
8. `policy-toml-roundtrip`, `policy-decode-schema` last: they wait on gaps 9 and 12.
