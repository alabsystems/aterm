<!-- Copyright 2026 Andrew Yates -->
<!-- SPDX-License-Identifier: Apache-2.0 -->

# aterm-spec: spec recovery from its 723 unit tests

> Status 2026-10-16: this is the crate's migration worklist. Each spec below is proved
> (Tier 0 by ty plus the interpreter for the model specs once the model-side fix M1 lands,
> then through Trust/Clean about the compiled Rust), and the tests it subsumes are deleted
> in the same commit. The 127 tests in §4 sample no spec and can go now.

Source: `crates/aterm-spec` (read only). Three recovery passes read every `#[test]` fn
(the chunk manifests said 726; counting `#[test]` attributes that are followed by a fn gives
**723**: `tests/spec_suite.rs` has none, its only `#[test]` is in a doc comment) and the
code each one reaches. Nothing was compiled or run; the claims come from reading the code.
`tests/derived_ring_ty.rs` (237) and `tests/derived_supervise.rs` (55) are the two big files;
`src/verify_tests.rs` (51) is the `#[cfg(test)]` module of `src/verify.rs`.

Result: **56 specs**, covering **596 tests**. **127 tests are not spec samples** (§4).

Most of this crate's tests (about 80%) test authored `ty_model!` models, not production code.
For those the spec is a property of the model, checked today at Tier 0 by the registry sweep
(`tests/non_vacuity.rs`, `spec-check models`). Binding a model to the Rust it describes is a
separate refinement obligation in the crate that owns the code (audit gap 10). The rest test
the checker itself (`derive`, `interp`, `verify`, `check`, `xref`, `ir`, `tla_check`) and its
tool-discovery shell: those specs are about compiled `aterm-spec` functions.

## 0. Notation used in the statements

Model side:

- `M` is a `derive::Model` value `{name, consts, vars, fn_vars, actions: [{name, guard?, updates}],
  invariants: [{name, kind ∈ {Claim, Bound, Uncaught}, expr}]}` returned by a `derive::*_model()`
  constructor. Every model named here is registered (`ty_model!`, `#[aterm_spec::model]`, or named
  by `#[aterm_spec::liveness(f)]`), so it is in `xref::model_registry()` and `xref::model_configurations()`.
- A state `s` maps variable names to `i64`. `env(M,s)` = `Model::eval_env`: `s` overlaid on the
  consts (the state shadows a const; of duplicate consts the last wins). `[[e]]_env` = `Expr::eval`.
- `Enabled(M,a,s)` ⇔ the guard of `a` is absent or true at `env(M,s)`. `succ_a(s)` = `Succ(M,a,s)`
  = the successors of `a` at `s`: updates evaluated in the **pre-state** (TLA+ primed semantics),
  `InRange` picks ascending, unwritten variables kept. `En(a,s)` ⇔ `succ_a(s) ≠ ∅`. `s·a` = the first
  successor (`Model::fire`).
- `Reach(M)` = the states reachable from `M.init_state()` under every action. `Good(M)` = those
  reached through states where every invariant holds.
- `M[B=d]` = `interp::with_buggy(M,d)`; `M[k=v]` = `interp::with_consts(M,&[(k,v)])` (only consts M
  declares change). `dials(M)` = `Model::mutant_dials` (the `Buggy` values M compares against).
  `Iso(M,x)` = `verify::buggy_baseline_with(M,x)`: the B=1 baseline keeping only the actions fired at
  B=0 plus mutant `x`. `only(M,I)` = M with only invariant `I` (`tests/common/walk.rs`).
- `Configs(M)` = `[M] ++ [M with each declared `scenario` override]`, in declaration order.
- `Holds(M)` ⇔ every state of `Reach(M)` satisfies every invariant ⇔ `interp::bmc(M) = Ok(|Reach(M)|)`.
  `Breaks(M,I)` ⇔ `bmc(only(M,I))` is `Err`. `Viol(M)` = `interp::violated_invariants(M)`.
- `Dead(M)` = `Acts(M) \ interp::fired_actions(M[B=0])`.
- `Sound(M)` ⇔ `verify::sweep_faults(Configs(M)) = []` (spec `registered-model-sound`).
  `Strict(M)` ⇔ `Dead(M) = ∅`. `Catch(M,T)`: each defect of table `T` alone breaks its named law
  (spec `mutant-table-catch`). `Live(M,L)` ⇔ `verify::audit_liveness(M,L) = Ok`
  (spec `liveness-obligations-sound`).
- `Law(M): φ` means `φ(s, En, s·a)` holds for every `s ∈ Reach(M[B=0])`. A scripted walk in a test
  is one sample point of such a law; a walk under B=1 that ends violating `I` is a witness row of `Catch`.

**Model-side gap M1** (in `aterm-spec` itself, not a row of the audit's §7 table): `Expr` has no
`ENABLED`, no primed or step predicate and no `reach` (EF) obligation, and `Model` declares no
catch table, no named mutant or assumption dials besides `Buggy`, no `strict` flag and no terminal
predicate. So the Law, Catch, Strict and dial clauses cannot yet be written into `ty_model!` and
checked by the sweep, and the scripted tests stand in for them. Specs that need it carry `M1` in
their blocking-gaps column. Every model spec also lists gap 10: a Clean proof (rather than the
Tier-0 enumeration) needs the model imported as a kernel term and bound to the code it describes.

Code side: `state` in the checker specs is the interpreter's `BTreeMap<&'static str, i64>`.
Tool-discovery specs quantify over file-system states and `PATH` values (effects, gap 8).
A panic named in a statement is a contract (an audited `trust::contract_panic` where it applies).

## 1. Specs (TSV)

Columns: `id`, `kind`, `functions`, `statement`, `subsumes` (primary tests; secondary ones in §2),
`blocking_gaps` (audit §7 numbers; `M1` is the model-side gap above), `rust_features` (what the
proof must handle).

```tsv
id	kind	functions	statement	subsumes	blocking_gaps	rust_features
registered-model-sound	invariant (registry)	verify::sweep_faults, xref::model_configurations, interp::violated_invariants, interp::violated_within_budget, verify::audit_dead_negative_controls, Model::mutant_dials, every registered derive::*_model	∀ registered M: Sound(M). Spelled out: ∀ C ∈ Configs(M): Holds(C[B=0]); ∀ Claim invariant I: ∃ C ∈ Configs(M), d ∈ dials(M): Breaks(C[B=d], I) (each claim broken by some mutant on its own, i.e. uncaught_invariants(M) = ∅, not only the first invariant a whole-model check stops at); no Bound/Uncaught invariant is broken; every action dead at B=0 in every config is an audited negative control; and where ty is installed its verdict on to_tla/to_cfg at B=0 is clean with exactly |Reach| states and refuted at B=1. tests/non_vacuity.rs (interpreter tier) and `spec-check models` (ty tier) already check this for every registered model, so every test that only re-checks it for one model is a duplicate (§4). Precondition for deleting a test that also drives a config the model does not declare: declare it (see declared-dials-and-scenarios).	55 primary tests (§2), e.g. tests/derived_ring_ty.rs::derived_closed_recovery_ledgers_prove_and_catch_double_record_or_loss, tests/derived_ring_ty.rs::derived_composed_sync_hold_requires_every_pane_to_release, tests/derived_ring_ty.rs::derived_composite_accessibility_route_proves_and_catches_wrong_or_stale_owner	10	ty_model! Model data (Vec<Action>, Vec<Invariant>, Box<Expr>, BTreeMap<&str,i64> states); the sweep driver uses thread::scope + catch_unwind. Tier 0 today is ty plus the interpreter; a Clean proof needs the model imported as a kernel term and the checker specs below.
model-strict	table-property (registry)	interp::fired_actions, verify::audit_dead_negative_controls, tests/common/walk.rs::committed_actions	∀ M that declares its negative controls N(M) (empty for a `strict` model): Dead(M) = Acts(M) \ fired_actions(M[B=0]) = N(M); M[B=1] with N(M) removed holds; each d ∈ N(M) fires in M[B=1] with only d added back and is then caught; so audit_dead_negative_controls([M]) = Ok(|N(M)|). Every audit-count pin (Ok(0), Ok(3), Ok(4), Ok(8), Ok(11)) and every "has no dead action" test is one instance. M1: add a model-level `strict` / negative-control declaration so the sweep states this and the count pins go.	28 primary tests (§2), e.g. src/derive.rs::ascii_grapheme_fast_path_model_proves_and_catches_mutant, src/derive.rs::gpu_frame_ring_model_proves_and_catches_mutant, src/derive.rs::metal_atlas_residency_model_proves_and_catches_mutant	10; M1	ty_model! Model data; BTreeSet of action names; BFS
model-claim-breaks-first	invariant (registry)	tests/derived_ring_ty.rs::assert_every_invariant_breaks_first (test helper), interp::Resolved::{successors,holds}	For the listed models (ClaudeFooterModel, GpuLossRecovery, RecoveryRedraw, HdrReconfigureRetag, VideoRecordingLifecycle, ExactInstanceRetention, AnchoredArtifactTransaction, ArtifactReplyPublication, ArtifactHandoffCapacity, ArtifactReaderLease, PresentedFrameTap, VideoTapSlot, SemanticPrewarmGeneration, ClaudeModeReturn, SemanticPrewarmHandshake, SemanticPrewarmRequestSwap, HarnessModelSwitch with PersonFlag ≠ 2) and every claim I: ∃ s ∈ Good(M[B=1]) (states reached through states where every invariant holds), an action a and s' ∈ succ_a(s) with {J | ¬J(s')} = {I}. Good(M[B=1]) is finite (< 100000 states). Stronger than Sound's per-claim catch: each claim is the first and only law a step breaks. Recommended: make it a model flag the sweep checks, then the helper goes.	16 primary tests (§2), e.g. tests/derived_ring_ty.rs::derived_anchored_artifact_transaction_proves_handle_and_reply_identity, tests/derived_ring_ty.rs::derived_artifact_handoff_capacity_refuses_overbooking, tests/derived_ring_ty.rs::derived_artifact_reader_lease_sweeps_only_after_final_release	10; M1	test-local BFS over Vec<i64> resolved states
mutant-table-catch	table-property (registry)	verify::buggy_baseline_with, interp::{bmc,violated_invariants,fired_actions,with_consts}	∀ M with a declared defect table T (MUTANTS, *_DEFECTS, Ride*, knob tables): Catch(M,T): for every row (x, L_x), with only defect x live (Iso(M,x), or the knob x = 1 on only(M,L_x)) x fires and L_x is broken; in the bmc form L_x is the first violation; in the 'exactly' form Viol = {L_x} (plus a declared second reader) and the knob at 0 holds; with no defect live nothing is violated. Every "each mutant alone breaks the invariant it was written for", "the defects are caught on every invariant" and per-path counterexample test is a row of such a table. M1: ty_model! has no catch-table syntax today, so the tables live in tests.	45 primary tests (§2), e.g. tests/derived_atpkg_full_pass.rs::the_full_pass_rule_proves_and_catches_the_session_lanes_misread, tests/derived_atpkg_index_probe.rs::cross_process_probe_cooldown_proves_and_catches_duplicate_ranges, tests/derived_atpkg_pass_stamps.rs::the_pass_record_proves_and_catches_both_historical_defects	10; M1	ty_model! Model data; Vec::retain over actions; whole-space BFS; BTreeSet comparison
declared-dials-and-scenarios	table-property (registry)	interp::with_consts, interp::bmc, Model::mutant_dials, refutes/at helpers (tests/derived_supervise.rs)	∀ M and every configuration the tests drive: (a) every shipped setting c holds: Viol(M[B=0][c]) = ∅ (StreamingSearch Wrap=0, SupervisorQuestionAnswer MaxRetry ∈ {1,3}, TrailAudioReopenLadder Budget=6, the SupervisorQuestionText Multi/Tabs/Review shapes, ...); (b) every secondary mutant dial D (UpgradeSettleTurnEnd Carry/Every, UpgradeRefusedTurnEnd Renew/Shared/Stale, HarnessUpgradeWorkEnd Nag/Hold, Fault/Early/Blanket/BypassReuse/Display/... knobs) breaks its named invariant at D=1 and holds at D=0; (c) every load-bearing assumption dial (PersonFence=0, TakenBeforeStill=0, OwnTextRemembered=0, EnterWhereTyped=1, Every=1, Stretch=1, Hold, MaxRetry) breaks its law: L ∈ Viol(only(M[d:=v], L)); (d) legal-by-design corners hold (FlashLimiter Buggy=1, Overlap=0). Fix: declare (a) as `scenario` rows and (b),(c) as named dials so the registry sweep states them. Model::mutant_dials reads only Buggy today, so no general check sweeps (b) or (c).	27 primary tests (§2), e.g. tests/derived_ring_ty.rs::derived_flash_limiter_proves_and_catches_overlap_blindness, tests/derived_streaming_search_ty.rs::derived_streaming_search_proves_and_catches_with_wrap_disabled, tests/derived_streaming_search_ty.rs::streaming_search_interpreter_walks_the_engine_lifecycle	10; M1	ty_model! syntax for named dials (proc macro); constant overrides; BFS
model-deadlock-free	invariant (registry)	interp::find_deadlock, verify::deadlock_free_and_catches_tiered, assert_operator_model_shape (test helper)	∀ listed M with its terminal predicate final_M: ∀ s ∈ Reach(M[B=0]): ¬final_M(s) ⇒ ∃ a: succ_a(s) ≠ ∅ (find_deadlock(M[B=0], final_M) = None). For operator-shaped models also fired_actions(M[B=1]) = Acts(M) and the audit is Ok(0). For the harness::instances Liveness rows M[B=1] has a reachable non-final state with no successor (the wedge is caught). ty CHECK_DEADLOCK agrees where installed. Recommended: declare final_M in the model so the sweep checks it.	15 primary tests (§2), e.g. tests/derived_ring_ty.rs::derived_exact_profanity_completion_rejects_predictive_fuc, tests/derived_ring_ty.rs::derived_harness_model_ladder_lands_every_due_move_and_catches_the_cold_only_rule, tests/derived_ring_ty.rs::derived_harness_model_priority_proves_and_catches_every_writer_and_reader_law	10, 13; M1	BFS; closure predicates for terminal states; ty spawned as a process
liveness-obligations-sound	other (fair liveness, registry)	verify::audit_liveness, verify::liveness_proves_and_catches_tiered, interp::nonprogress_under, xref::liveness_registry, derive::*_liveness	∀ (M, L) ∈ liveness_registry(), L = (goal G, weak set W, strong set S, mutants): Live(M,L): every behaviour of M[B=0] fair under W and S satisfies □◇G (nonprogress_under(M[B=0], L) = None); dropping any one fairness assumption admits a reachable non-progress cycle (each is load-bearing); each named mutant alone admits a fair non-progress cycle. The per-model lasso shapes the tests pin (entry state, moves) are witnesses of the last clause. Readings the tests add (AccountRotation's s2 twin, the non-property conjunction, the canary hold under a fair clock) should become registered obligations.	16 primary tests (§2), e.g. tests/derived_chrome_regrow.rs::a_fall_the_floor_holds_is_applied_and_the_restart_mutant_starves_it, tests/derived_exceptions_digest.rs::an_unnamed_entry_ends_by_its_own_clock_and_the_mutants_are_caught, tests/derived_harness_restart.rs::the_restart_ends_its_program_under_fairness_and_term_only_does_not	5, 10	fair-cycle (SCC) search over a state graph; link-section registry
prove-catch-multiply	table-property	verify::prove_catch_and_multiply_scalar, interp::with_consts	For M ∈ {FlashLimiterWindow, AltArchivePool, SessionIdClaim}, with declared corners C and catch corner k: M declares Buggy, Local and Instances committed at 0, 0, ≥ 2; ∀ c ∈ C: Viol(M[B=0][c]) = ∅; Viol(M[B=1][k]) ≠ ∅; ∀ c ∈ C: Viol(M[Local=1][c]) ≠ ∅; Viol(M[Local=1, Instances=1]) = ∅. For SessionIdClaim the adopt → exit → adopt trace also ends with exactly one holder.	tests/derived_ring_ty.rs::derived_alt_archive_pool_proves_catches_and_multiplies, tests/derived_ring_ty.rs::derived_flash_limiter_window_proves_catches_and_multiplies, tests/derived_ring_ty.rs::derived_session_id_claim_proves_catches_and_multiplies	10	constant overrides; BFS; ty spawned as a process
guard-conjunct-audit	table-property	tests/common/conjunct_audit.rs::audit_guard_conjuncts, derive::{trail_sound_seam_model,trail_audio_reopen_ladder_model,release_publish_once_model}	For M ∈ {TrailSoundSeam, TrailAudioReopenLadder, ReleasePublishOnce}: auditing every top-level conjunct of every committed action guard (drop it alone, check each invariant) equals the declared role table exactly: Decision ⇒ carries = ∅ ∧ changes behaviour; Stutter ⇒ carries = ∅ ∧ only self-loops change; Environment(S) ⇒ carries = S ∧ changes behaviour. TrailSoundSeam has no guards; in TrailAudioReopenLadder only HealthyWindowElapses#0 carries {RecoveryIsBounded, ReopensLeftIsTruthful, ResetOnlyOnPlayedWindow}; ReleasePublishOnce's table names, per conjunct, the invariants dropping it breaks. Recommended: move the audit into aterm-spec and the role tables into the models.	tests/derived_release_publish_once.rs::publish_once_guard_conjuncts_carry_the_order_they_claim, tests/derived_trail_sound_ty.rs::trail_audio_reopen_ladder_proves_bounded_recovery_and_catches_every_replay, tests/derived_trail_sound_ty.rs::trail_sound_seam_proves_the_rulings_and_catches_d1	10; M1	Expr tree rewriting (dropping conjuncts); per-invariant BFS; transition-set comparison
sound-step-edits-refute	table-property	interp::{bmc,violated_invariants}, account_rotation_edited (test helper)	For M ∈ {TrailAudioReopenLadder, SupervisorAccountRotation, SupervisorLimitReset} and each edit e in the test's table (a dropped guard conjunct or a rewritten update of a SOUND action at B=0): L_e ∈ Viol(only(M_e, L_e)) at every listed constant setting, with no defect action firing; with nothing dropped the rebuilt step reproduces Reach(M[B=0]) state for state and Viol = ∅. So each law is stated over the sound step, not over a flag only a mutant writes.	tests/derived_supervise.rs::the_account_rotation_laws_break_when_the_sound_move_follows_a_low_active, tests/derived_supervise.rs::the_account_rotation_laws_break_when_the_sound_move_picks_shared_or_low_first, tests/derived_supervise.rs::the_account_rotation_laws_break_when_the_sound_move_reads_stale_or_follows_the_herd, tests/derived_supervise.rs::the_account_rotation_liveness_breaks_when_the_sound_move_never_takes_the_canary_s_place, tests/derived_supervise.rs::the_limit_reset_laws_break_when_a_sound_step_drops_a_guard, tests/derived_trail_sound_ty.rs::trail_audio_reopen_ladder_refutes_edits_to_its_own_reset_law	10; M1	Expr tree rewriting via &mut on Model actions/updates; BFS
test-only-laws-lifted	invariant	derive::{harness_model_switch_model,supervisor_question_text_model,supervisor_account_rotation_model}	Laws asserted today only inside test walks, which must become named model invariants or action properties (then registered-model-sound and mutant-table-catch subsume them): (i) HarnessModelSwitch, ∀ (DefaultFable, PersonFlag, Build) ∈ MODEL_SWITCH_CONFIGS: no s ∈ Reach(M[B=0]) with fresh=1 ∧ phase>0 ∧ ann=1 has fl=1 (gap a), live=2 ∧ cmd=1 (gap b) or live=4 (gap c); each Ride* mutant alone reaches exactly its gaps (none when PersonFlag=2); PersonFlag=2 ⇒ due=mto=set=flag=0 everywhere. (ii) SupervisorQuestionText, Multi ∈ {0,1}: ∀ s ∈ Reach: fresh=1 ∧ seen_fallback=1 ⇒ ¬En(HarnessType,s). (iii) SupervisorAccountRotation: every step that changes session n's account sets (mv, pa, ph)' = (n, act, hold). The models prove less than their tests claim until this lands.	10 primary tests (§2), e.g. tests/derived_ring_ty.rs::derived_harness_model_switch_ride_keeps_the_move_laws_at_build_0, tests/derived_ring_ty.rs::derived_harness_model_switch_ride_keeps_the_move_laws_at_build_1, tests/derived_supervise.rs::each_account_rotation_defect_breaks_its_own_law_slice_0_of_5	10; M1	test-local BFS with per-state predicates, to move into the model
model-laws	other (state, step and path laws, registry)	Model::{fire,action_enabled,successors}, walk/run/step test helpers, every model in §1.1	∀ M: Law(M) for M's catalogue entry (§1.1 for the chunk-1/3 models; the R13 test walks for the rest). Each scripted walk w = (a1..ak) samples three general properties checked over all of Reach(M[B=0]) instead of at one point: (a) reachability of intended behaviour, an EF obligation (`reach` in the model, not a trace); (b) guard laws □(P_A ⇒ ¬En(A)) for every assert!(!action_enabled(A, s)), with P_A the observable condition the assertion message names; (c) step and state laws: □[A ⇒ v' = f(v)] for every assert_eq on a post-state, and plain invariants for state facts (e.g. held + dropped + failed = total). A walk on M[B=1] that ends violating I is a witness row of mutant-table-catch. M1: Expr has no ENABLED and no primed/step predicate and Model has no `reach` obligation, so these laws cannot be written into ty_model! yet; once they can, Tier 0 (ty + interpreter) checks them and the walk tests go.	237 primary tests (§2), e.g. tests/derived_atpkg_contention_release.rs::a_contended_lane_retries_on_the_first_slice_after_release, tests/derived_atpkg_flip_quiet.rs::a_busy_toolchain_holds_its_flip_up_to_the_ceiling_and_a_revoked_one_never_waits, tests/derived_atpkg_head_watch_hosts.rs::a_window_or_one_session_watches_never_both	5, 10; M1	ENABLED predicates, primed action properties and EF obligations over ty_model! Model data; BTreeMap state indexing
text-zero-projection	equivalence	derive::{supervisor_question_answer_model,supervisor_question_text_model}, interp::reachable_states	∀ shape ∈ {committed, (Tabs=1, Review=0), (Tabs=1, Review=1)}: π(Reach(QuestionText[Text=0][shape])) = Reach(QuestionAnswer[shape]), where π drops {own, seen_own, astray, fallback, seen_fallback, wrote, twice, exact, seen_exact}; each of those is 0 in every reachable state; no text action (PersonTypesPolicyText, TextLands, TextPartly, FitLostOnIt, FitLostOffIt, HarnessType) is enabled anywhere at B ∈ {0,1}. The pinned counts 12989/3197/6295 are a change detector standing in for this.	tests/derived_supervise.rs::at_text_zero_the_question_model_is_todays	1, 10	two models' reachable sets compared under a projection; BFS
interpreter-semantics	equivalence	Model::{init_state,eval_env,action_enabled,successors,successors_in,fire,check_invariant}, Expr::eval	∀ m, a, s: action_enabled(m,a,s) = Enabled(m,a,s) ⇔ a is declared, its guard (true if None) evaluates true at env(m,s), and every InRange pick in its updates is nonempty ⇔ successors(m,a,s) ≠ []; successors(m,a,s) = Succ(m,a,s) as a list (updates in declaration order evaluated in the pre-state, ranges ascending, a deterministic action has exactly one successor); fire(m,a,s) is false and leaves s unchanged iff Succ = ∅, else sets s to the first successor; check_invariant(m,I,s) ⇔ [[I]]_env(m,s); init_state maps each var to its init; Expr::eval is the integer semantics of +, -, =<, >, ==, If (nested), with state shadowing consts. These coincide with the TLA+ meaning of the action formulas in to_tla(m) (the executable-twin claim). An unknown action or invariant name panics (audited contract panic).	src/derive.rs::action_enabled_reflects_guards, tests/macro_ring.rs::macro_built_interpreter_runs, tests/macro_ty_model.rs::macro_nested_if_else_interpreter_tiers, tests/macro_ty_model.rs::macro_paren_and_gt_in_guard, tests/macro_ty_model.rs::macro_unguarded_action_is_always_enabled_in_interpreter	1, 2, 3, 7	BTreeMap<&'static str,i64> clone/insert, recursive Box<Expr> eval with contract panics, cartesian-product loops, iterator find with closures
value-accessor-fail-closed	contract	derive::Value::{as_int,as_bool}, derive::Expr::eval	∀ v: as_int(Int(i)) = i, as_bool(Bool(b)) = b; as_int(Bool(_)) panics with 'expected Int, got Bool', as_bool(Int(_)) with 'expected Bool, got Int'. Hence eval of an Expr with a boolean in an integer position (Add/Sub/Gt/Le operands, InRange/Forall bounds) or an integer in a boolean position (And/Or/Iff/If-cond) panics with the matching message and never yields a default.	src/derive.rs::ill_typed_bool_position_fails_closed, src/derive.rs::ill_typed_int_position_fails_closed	1, 3	enum match, panic as a contract (trust::contract_panic), recursive eval over Box<Expr>
bounds-are-ranges	contract	derive::not_range_bounds, derive::Model::malformed_bounds	∀ e, consts: not_range_bounds(e,consts) = [] ⇔ e is a conjunction of atoms x <= c, x > c or c > x, with x a state variable and c an integer literal, a constant other than Buggy, or such a constant ± a literal, and for every x with an upper bound the combined lower extent (max of lower bounds, from 0) is strictly below the combined upper bound (min of upper bounds). Otherwise the result names each non-range atom (relations between variables, atoms over Buggy) and each pinned variable.	src/derive.rs::a_bound_must_be_a_range	1, 3, 7	recursive conjunct split, BTreeMap fold, String messages
expr-tla-agrees-with-eval	equivalence	derive::Expr::{to_tla,eval}	∀ well-typed Expr e and env binding its free names: the TLA+ value of e.to_tla(), read with TLA+ precedence and left-associative + and -, equals e.eval(env). In particular Sub(a, b) parenthesizes b exactly when b is Add or Sub, and a left-nested Sub renders bare ('5 - 2 - 1').	src/derive.rs::sub_with_additive_rhs_parenthesizes_to_match_the_interpreter	1, 3, 5, 14	recursive String building over Box<Expr>; needs a Lean model of TLA+ expression semantics
model-tla-rendering	round-trip	derive::Model::{to_tla,to_cfg,to_cfg_with,to_cfg_deadlock_with,transition_spec,transition_cfg,render}, Expr::to_tla, tla_check::TlaSpec::parse_str	∀ m: TlaSpec::parse_str(m.to_tla()) = Ok(spec) with module_name = m.name and actions ⊇ every action and invariant name; CONSTANT iff consts ≠ [] and VARIABLES over vars ++ fn_vars; Init is the conjunction of v = init (function vars as [n \in 1..K |-> e]); each action renders 'A == [G /\ ]v1' = e1 /\ ... /\ UNCHANGED << vars not written >>' (no guard conjunct iff guard = None; a disjunctive guard parenthesized); Next is the disjunction of the actions in order; Spec == Init /\ [][Next]_vars; each invariant renders Name == to_tla(expr); Le renders =<, If renders (IF c THEN a ELSE b) with nesting kept and source parentheses dropped. transition_spec() = to_tla() with Init == TRUE. transition_cfg(ov) = one 'CONSTANT k = v' line per const in order (ov applied, Buggy defaulting to its committed value), then SPECIFICATION Spec and CHECK_DEADLOCK FALSE; to_cfg_deadlock_with(ov) = to_cfg_with(ov) with exactly CHECK_DEADLOCK TRUE. Preferred proof target: parse_tla(to_tla(m)) reconstructs m up to Expr equality, and ty's state space for to_tla(m) equals Reach(m) (which verify::assert_same_space_explored checks per model).	11 primary tests (§2), e.g. src/derive.rs::cursor_model_emits_unchanged_and_disjunctive_next, src/derive.rs::deadlock_cfg_flips_the_line, src/derive.rs::evict_full_emits_function_valued_tla	1, 3, 5, 7, 11, 14	String/format!/join over Vec iterators, str::replace, recursive Expr rendering; a TLA+ semantics (and parser) in Lean
ty-model-desugar	refinement	aterm-spec-macros::ty_model (proc macro), derive::{add,sub,le,gt,cst,var,int,if_}	∀ well-formed ty_model! input T: expand(T) is the Model built compositionally from T: consts in declaration order; vars with inits; actions in order with guard = Some(tr(c)) iff `when (c)` is present; updates in order; invariants with kind Claim for `invariant`, Bound for `bound`, Uncaught for `uncaught`; fn_vars = []; tr is a homomorphism (+, -, <= → le, > → gt, ==, if/else with else-if nesting, tr((e)) = tr(e)); an identifier maps to cst(x) iff x is a declared const, else var(x); a literal to int(n). Corollary: to_tla, to_cfg and transition_spec of expand(T) equal those of the hand-built tree.	tests/macro_ring.rs::macro_surface_equals_hand_built_model, tests/macro_ty_model.rs::macro_nested_if_else_matches_hand_built, tests/macro_ty_model.rs::macro_unguarded_action_matches_hand_built_tla	1, 3, 7, 9	syn/quote token-stream transformation; structural equality over Model (no PartialEq today; the tests compare rendered strings)
model-registry	contract	xref::{model_registry,model_configurations,liveness_registry}, registry::submit!, #[aterm_spec::model], #[aterm_spec::liveness], ty_model! (registration half)	In any binary: model_registry() holds every model written with ty_model! or #[aterm_spec::model], or named by an #[aterm_spec::liveness(f)] obligation, exactly once, each equal (by to_tla) to what its constructor returns, in name order; model_configurations() yields per M the list [M] ++ [with_consts(M, sc) | sc ∈ M's `scenario` overrides, in declaration order] (e.g. RegisteredWithScenario → [[Buggy=0,Cap=0],[Buggy=0,Cap=2]]); liveness_registry() pairs each obligation with f()'s model, sorted by (model name, obligation name).	tests/macro_ty_model.rs::writing_a_model_registers_it	1, 8, 9, 11	link-section static registration (unsafe load-time constructors), proc-macro attributes, BTreeMap by name, fn pointers
registry-intrusive-list	invariant	registry::{Registry::submit,Registry::iter,iter,Node::new}	∀ sequence of submit calls on a static Registry: each node is linked at most once (resubmitting is a no-op); iter() yields every submitted node exactly once, newest first submission first; registry::iter::<T>() yields every node the load-time constructors submitted for T.	src/registry.rs::a_node_submitted_twice_is_linked_once, src/registry.rs::the_load_time_constructors_collected_both_submits	2, 8, 11, 13	&'static intrusive linked list with atomics/unsafe, load-time (ctor) registration, generics
bmc-reach-semantics	contract	interp::{bmc,fired_actions,violated_invariants,violated_within_budget,reachable_states,find_deadlock,with_buggy,with_consts}	∀ scalar m with |Reach(m)| < MAX_STATES: bmc(m) = Ok(|Reach(m)|) ⇔ Holds(m); otherwise bmc(m) = Err((s,I)) with s ∈ Reach(m), ¬I(s), s the first violating state in BFS order and I the first violated invariant in declaration order; fired_actions(m) = {a | ∃ s ∈ Reach(m): succ_a(s) ≠ ∅} (stutters included); violated_invariants(m) = {I | ∃ s ∈ Reach(m): ¬I(s)} = the union of per-invariant bmc results; reachable_states(m) = Reach(m); find_deadlock(m, final) = some reachable non-final state with no non-stutter successor iff one exists; with_consts/with_buggy change exactly the named constants m declares; bmc panics iff the space reaches MAX_STATES. Every model spec above rests on this checker; no test exercises it directly beyond the one sample (Buggy-guarded actions of config_catalog_snapshot_model are dead at B=0 and fire at B=1).	src/interp.rs::fired_actions_separates_committed_dead_mutants_from_buggy_live_ones	1, 3, 5, 7, 11, 13	BFS over VecDeque/BTreeSet/BTreeMap, closures, nondeterministic InRange fan-out, generic Walker trait, panicking budget assert
admits-iff-some-action	contract	interp::admits	∀ m, states p, n: admits(m,p,n) = Some(a) where a is the first action in declaration order with n ∈ successors_in(a, eval_env(p), p); None iff no action produces n from p. p need not be reachable.	src/interp.rs::admits_is_a_real_conformance_check	1, 7	Iterator::find over Vec<Action>, Vec::contains on BTreeMap states
compiled-walk-equals-by-name	equivalence	interp::{Compiled::new,Compiled::successors,Compiled::holds,Resolved::new,Resolved::slot,ByName,bmc_on,fired_actions_on,violated_invariants_on,find_deadlock_on,explore_on,reachable_on}	∀ m: Compiled::new(m) = None iff some update writes a name the initial state does not bind (every walk then uses ByName(m)). When Some(c), every walk W ∈ {bmc_on, fired_actions_on, violated_invariants_on, find_deadlock_on(final), explore_on (states, steps, parents, fired), reachable_on} satisfies W(m,c) = W(m,ByName(m)); at every reachable state the compact successors map back exactly to successors_in, and c.holds(i,s) = check_invariant_in. Resolved::slot(name) = the name's index in State key order; constants shadowed by variables resolve to the variable, the last duplicate constant wins, nested Forall indices shadow outward. An unbound identifier panics with 'unbound identifier `x` in model evaluation' only when that sub-expression is evaluated.	src/interp.rs::a_model_writing_an_undeclared_variable_is_walked_as_written, src/interp.rs::compiled_walks_are_the_by_name_walks_on_committed_models, src/interp.rs::compiled_walks_are_the_by_name_walks_on_the_resolution_corners, src/interp.rs::the_compiled_form_raises_an_unbound_identifier_when_it_reads_it	1, 7, 11, 13	two interpreters (Vec<i64> vs BTreeMap states), trait Walker, generic walks, panics as contract
scc-components	contract	interp::StateGraph::components	∀ graph g and node subset N: g.components(N) is a partition of N in which a, b share a block iff each reaches the other along steps whose targets are in N; each block is listed least node first.	src/interp.rs::components_are_exactly_the_mutual_reachability_classes	1, 7	iterative Tarjan over Vec<Vec<(usize,usize)>>, explicit stack, BTreeSet
nonprogress-cycle-fair-lasso	contract	interp::{find_nonprogress_cycle,nonprogress_under,find_deadlock,Space::of,Space::find_nonprogress_cycle,Space::nonprogress_under,Space::states,Space::fired,StateGraph::witness}	∀ m, goal g, fairness F (weak W, strong S over declared actions): find_nonprogress_cycle(m,g,F) = None iff every F-fair behaviour of m from Init reaches a g-state. A step that leaves the state unchanged is a stutter: never progress and never 'enabled' for fairness. When Some(c), c is a lasso: c.entry() ∈ Reach(m) is not a goal; c.cycle is a (possibly empty, meaning a stall) cycle of non-goal states back to the entry; c.moves are the actions on it; the behaviour satisfies W and S (S checked Emerson-Lei style: states where an S action is enabled are removed and the rest recomputed, never the whole component); the nearest component is reported first. Naming an undeclared action panics ('which the model does not declare'), as does naming one both weak and strong. Space::of(m) answers exactly as the one-shot fns: the same cycles, nonprogress_under(L) = interp::nonprogress_under(m,L), states() = bmc of m without invariants, fired() = fired_actions(m).	src/interp.rs::a_lane_that_always_lands_is_clean_and_its_fairness_is_what_makes_it_so, src/interp.rs::a_retry_loop_that_never_lands_passes_every_older_check_and_is_found, src/interp.rs::a_space_answers_what_the_one_shot_walks_answer, src/interp.rs::a_step_that_changes_nothing_is_a_stutter_not_progress, src/interp.rs::a_wedge_short_of_the_goal_is_reported_as_a_stall, src/interp.rs::fairness_excludes_starvation_only_cycles, src/interp.rs::fairness_naming_an_undeclared_action_panics, src/interp.rs::strong_fairness_keeps_a_loop_that_never_passes_the_gate	1, 5, 7, 13, 14	graph algorithms over Vec/BTreeMap/BTreeSet, impl Fn goal closures, priority queue; quantifies over infinite behaviours (needs fair-path semantics in Lean)
one-walk-ghost-sweep	equivalence	verify::{uncaught_invariants,uncaught_invariants_one_walk_per_invariant}	∀ m: uncaught_invariants(m) (one walk) = uncaught_invariants_one_walk_per_invariant(m), same names in declaration order: the invariants never violated in Reach(m[B=1]). Both panic iff some evaluation reaches a function-valued expression. With no invariants the result is [] and no walk happens.	src/verify_tests.rs::a_model_without_invariants_has_no_ghosts_and_is_not_walked, src/verify_tests.rs::the_one_walk_ghost_sweep_matches_the_per_invariant_oracle	1, 7, 13	BFS with an early-exit live set; panic equivalence (catch_unwind in the oracle)
dead-negative-control-audit	contract	verify::{audit_dead_negative_controls,dead_actions_agree,isolate_mutant,buggy_baseline_with}	∀ config list C = [m, scenarios...], D = the actions dead at B=0 in every config of C: audit_dead_negative_controls(C) = Ok(|D|) iff (a) D = ∅ or m commits Buggy = 0; (b) every a ∈ D fires under some mutant dial; (c) for every a ∈ D, Iso(m,a) reaches a violation whose violating step is a itself, taken from a state where every invariant held; (d) an action fired in some declared scenario is not in D. Otherwise Err naming the first failing clause ('no committed `Buggy = 0` dial', 'fires under no mutant dial', 'caught by nothing', 'independently caught by nothing', 'another branch', "no `scenario` declares"). dead_actions_agree(m, ty_dead) = Ok(D) iff ty_dead = D as sets; Err('TIER DISAGREEMENT') when they differ (an empty ty report included); Err('does not declare') for a name m lacks.	9 primary tests (§2), e.g. src/verify_tests.rs::a_scenario_keeps_its_own_actions_alive, src/verify_tests.rs::audit_accepts_config_catalog_snapshot_mutants, src/verify_tests.rs::audit_accepts_strictly_nonvacuous_model	1, 5, 7	BFS walks, BTreeSet algebra, Result<usize,String> with message contracts
ty-transcript-parsers	contract	verify::{ty_states_explored,ty_evidence_header,stopped_early}, check::{violated_law,dead_actions}	∀ transcript t: ty_states_explored(t) = Some(n) iff t has a 'Statistics:' block with a line 'States found: n' (decimal), else None (including 'States explored', non-numeric counts, no block). ∀ path p, ty_evidence_header(p) names p and ends in '\n', and ∀ body b the parsers read header ++ b as b. stopped_early(out) ⇔ exit code 5 or a stdout line starting 'Model checking stopped:', at any exit code. violated_law(t) = Some(X) for the first line containing 'Invariant X is violated' with X nonempty and space-free, else None. dead_actions(t) = the comma-split, trimmed, nonempty names after the first 'dead action(s) (never fired): ' (plain or allow-vacuous form), else [].	src/check.rs::the_dead_action_report_is_read_in_both_forms, src/check.rs::the_violated_law_is_read_off_the_transcript, src/verify_tests.rs::a_stopped_search_is_never_a_verdict, src/verify_tests.rs::the_ty_evidence_header_is_invisible_to_the_transcript_parsers, src/verify_tests.rs::ty_state_count_is_parsed_or_refused	1, 3, 7	&str line iteration, find/strip_prefix/split, parse::<u64>; std::process::Output for stopped_early
explored-space-guard	contract	verify::{assert_same_space_explored,assert_ty_saw_whole_space}	∀ m, interpreter count n, evidence e: assert_same_space_explored(m,n,e) returns iff ty_states_explored(e) = Some(n); it panics 'TIER DISAGREEMENT ON THE EXPLORED SPACE' for Some(k), k ≠ n, and 'no evidence it explored anything' for None. assert_ty_saw_whole_space(m,e) returns iff bmc(m[B=0]) = Ok(n) and e reports n states, so it panics on a clean verdict over a violated committed config and on a POR-reduced count.	src/verify_tests.rs::a_clean_ty_verdict_over_a_violated_model_is_a_false_proof_not_a_coverage_gap, src/verify_tests.rs::a_clean_verdict_without_a_state_count_credits_nothing, src/verify_tests.rs::a_dead_set_from_a_reduced_run_is_refused_by_the_space_guard, src/verify_tests.rs::a_ty_verdict_from_a_larger_space_is_not_a_proof_either, src/verify_tests.rs::a_ty_verdict_from_a_smaller_space_is_not_a_proof	1, 4, 7	panic as a contract; composition with ty-transcript-parsers and bmc
scalar-tier-always-runs	contract	verify::{check_model_tiered,prove_and_catch_scalar}	∀ m with fn_vars = []: check_model_tiered(m,label) never returns Err(NotRun); it returns Ok(Interpreter) or Ok(InterpreterAndTy), or panics on a violation or tier disagreement. So prove_and_catch_scalar's unreachable! arm is unreachable whatever tools are installed.	src/verify_tests.rs::a_scalar_model_can_never_report_not_run	4, 8	Result/enum; a call into find_ty (fs probing) whose outcome must not matter
prove-and-catch-contract	contract	verify::{prove_and_catch_scalar,prove_and_catch_tiered}, interp::{prove_and_catch,bmc}	∀ m: prove_and_catch_scalar(m, label) returns (does not panic) ⇔ m.fn_vars = [] ∧ Holds(m[B=0]) ∧ ∃ s ∈ Reach(m[B=1]), I: ¬I(s), and, when a ty checker is found, ty's verdicts on to_tla(m)/to_cfg(m) and to_cfg_with(Buggy=1) agree with the interpreter's over the same committed state count. prove_and_catch_tiered returns Err(NotRun) exactly when fn_vars ≠ [] and no ty is installed (so a caller that discards the Result proves nothing; see §5). The interpreter half is provable; the ty half is an effect.	tests/rfc_worked_examples_ty.rs::rfc_atlas_pack_proves_and_catches_overflow, tests/rfc_worked_examples_ty.rs::rfc_glyph_fidelity_proves_and_catches_mac_roman_substitution, tests/rfc_worked_examples_ty.rs::rfc_mark_row_origin_proves_and_catches_y_divergence, tests/rfc_worked_examples_ty.rs::rfc_reload_frame_proves_and_catches_font_shrink	1, 2, 7, 8, 10	panicking asserts as the contract channel; Option<PathBuf> from find_ty; a child process for the ty tier
tool-discovery-order	contract	verify::{locate_ty,locate_trust_ir,locate_ay,discover,find_trust_bin,resolve_store_shim,exec_stub_target,classify_store_shim,is_pending_stub,atpkg_store_bin_dir,account_home,home_dirs,path_search_in}	∀ fs state and PATH value P: the tool found for bin is (1) the store tier, if <account store>/bin/<bin> is a live shim (a symlink resolving to an existing file, or an atpkg exec stub whose first `exec '<abs path>'` line, sh single-quote unescaped, names an existing absolute file), resolved to that file; else (2) the first P entry holding <bin> that is not a pending-program stub; else None. A developer tree ($HOME/trust/.../target, stage dirs, ~/ay) is never a candidate. resolve_store_shim is None for absent, dangling, tombstone (refusal script), pending-stub, relative-target and reclaimed-target shims, and classify_store_shim returns Absent or Refusal accordingly. is_pending_stub(p) reads at most PENDING_STUB_HEAD bytes and is true iff that head carries the atpkg pending-stub marker (false for an unreadable path). The store root comes from the account's home (getpwuid_r), whatever $HOME says.	src/verify_tests.rs::a_pending_stub_is_not_the_tool, src/verify_tests.rs::discovery_order_is_store_then_path_and_no_developer_tree, src/verify_tests.rs::exec_stub_target_reads_atpkg_shims_and_rejects_refusals, src/verify_tests.rs::store_shim_resolution_is_fail_closed, src/verify_tests.rs::the_pending_stub_check_reads_only_the_head, src/verify_tests.rs::the_store_is_the_accounts_whatever_home_says	1, 8	fs (read_link, canonicalize, bounded File::read, metadata), libc getpwuid_r FFI, env vars
shell-literal-word	contract	verify::shell_literal_word	∀ s: shell_literal_word(s) = Some((w, rest)) iff s starts with a literal word followed by end-of-string or a blank (rest = the remainder from that blank on): a single-quoted '...' (w = the verbatim contents, '' allowed, $ not expanded); a double-quoted "..." whose contents contain none of $ ` \; or a bare word with no whitespace and none of $`\"'*?[]{}~;&|<>(). Otherwise None: empty input, an unclosed quote, a quote followed directly by text, an expansion, a glob, a ';', or a leading ~.	src/verify_tests.rs::a_shell_literal_word_is_read_only_when_nothing_expands	1, 3, 7	&str slicing, char scanning loop, Option<(&str,&str)> borrowing the input
binary-identity-chain	contract	verify::{binary_identity,script_exec_target,literal_exec_target,is_shell_option_line,exec_stub_target}	∀ fs state, path p: binary_identity(p) = None iff p cannot be stat'ed. Otherwise links[0] = (p, FileIdentity(len, mtime, dev, ino, ctime)) and the chain follows a hop for an atpkg shim (atpkg header, optional exports and route guards, final exec of the store file) giving AtpkgShim, or a literal forwarder (shebang, then only blank lines, comments and `set -e...` lines, then exactly one final `exec <literal word> ["$@"]`) giving LiteralExec. Any other script ends as OpaqueScript with its own identity; a non-script as Binary; a missing target as Dangling(target); a loop or depth past the bound as TooDeep. So replacing only the target changes the identity while links[0] is unchanged.	src/verify_tests.rs::a_binary_identity_follows_a_wrapper_to_the_binary_it_execs	1, 7, 8	fs metadata (dev/ino/ctime), bounded reads, depth-bounded loop, Vec<ChainLink>, enum ChainEnd
provenance-claim-state	contract	verify::{claim_report,change_note,tilde}	∀ slot last, key now: claim_report(last, now) = First if unclaimed, Unchanged if *last = now, else Changed(old); always *last := now. Slots are per tool; keys compare the Located path and origin plus the full BinaryIdentity, so a rollback or same-path rebuild is Changed. change_note(prev, now, home) = ' — changed mid-process (was X)', X = 'NOT FOUND' for None prev, SAME_PATH_CHANGED when the paths are equal, else prev's path with the home prefix shown as ~.	src/verify_tests.rs::provenance_is_claimed_again_only_when_the_resolution_changes	1, 2	&mut Option<_>, enum Claim, PartialEq over nested structs/Vec, String formatting
provenance-line-rendering	contract	verify::{render_provenance,provenance_line,describe_exec_chain,unreached_store_note,store_copies_in,report_provenance_on_change,ty_build_stamp,utc_timestamp}	∀ discovery outcome: the line is 'VERIFY ESCALATION TIER: <bin> = NOT FOUND (probed the atpkg store shim, PATH)' plus a clause naming the managed store's copy and why discovery missed it, when the store holds one; else '... = <path> [<origin>; <version>; built <UTC ISO date of the end binary>; <bytes of the end binary> bytes]' with origin one of 'atpkg managed store', 'PATH', 'PATH (atpkg shim → T)', 'PATH (wrapper → T)', the opaque-script note (built/bytes then the script's own) or '... (missing)' for a dangling target. Other store copies are appended as ' — the managed store also holds ...', never the chosen copy; caps is appended for ty. A line is printed exactly when claim_report ≠ Unchanged, and a repeated line ends with change_note. ty_build_stamp(p) = 'mtime N (unix)' plus ' of T' for a wrapper whose end binary is T.	src/verify_tests.rs::a_path_exec_stub_names_its_target, src/verify_tests.rs::provenance_is_reported_again_whenever_the_resolved_binary_changes, src/verify_tests.rs::provenance_nothing_installed_is_a_plain_not_found, src/verify_tests.rs::provenance_store_present_names_the_unreached_copy, src/verify_tests.rs::the_provenance_line_names_the_build_behind_a_wrapper	1, 4, 8	fs walks of the store, SystemTime, String formatting; composes binary-identity-chain and civil-date
provenance-report-per-tool-serial	invariant	verify::{report_on_change,LastReported}	∀ set of concurrent report_on_change calls: (a) a call for tool t blocks only on calls for the same t; a probe in flight for t never delays a report for t' ≠ t; (b) per t, lines are written in the order their claims were made, so the last line written for t names the key in t's slot; (c) a call whose claim is Unchanged writes nothing.	src/verify_tests.rs::a_slow_provenance_probe_holds_up_only_its_own_tool, src/verify_tests.rs::a_tools_last_line_names_what_runs_when_its_changes_race	2, 13	Mutex per tool, closures (probe, sink), threads; a linearizability argument
civil-date	contract	verify::{civil_from_unix_days,build_date}	∀ d (i64 days from 1970-01-01, in the algorithm's range): civil_from_unix_days(d) = the proleptic-Gregorian (year, month 1..=12, day 1..=31) of 1970-01-01 + d, e.g. civil_from_unix_days(-1) = (1969,12,31). ∀ readable p: build_date(p) = Some('YYYY-MM-DD') of p's mtime in UTC.	src/verify_tests.rs::build_dates_are_utc_civil_dates	3, 8	i64 div/rem arithmetic (Hinnant days_from_civil inverse), casts; fs mtime for build_date only
ty-capabilities-parse	contract	verify::{caps_from,caps_clause}	∀ (code, stdout): caps_from(code, stdout) = the 'capabilities' array of strings, in order, iff code = Some(0) and stdout is a JSON object with that field, else [] (exit 2, a killed probe, a plain listing). caps_clause([]) = '; caps=none'; caps_clause(xs) = '; caps=' ++ join(xs, ',').	src/verify_tests.rs::ty_capabilities_are_the_json_list_and_an_older_ty_has_none	1, 9, 12	aterm_json parsing (another crate), Vec<String>, join
ty-capabilities-cache-and-probe	contract	verify::{caps_cached,ty_caps,probe_version,probe_output_within,probe_output}	caps_cached(cache, ty, probe) calls probe exactly once per (path, binary_identity(ty)) and serves later calls from the cache; a rebuild behind a wrapper or another path is a new key. ty_caps of a ty that exits 2 is []. probe_version(b) = the first nonempty stdout line of `b --version`, None if b cannot run. probe_output_within(b, args, d) = Some(output) if the child exits within d, else the child is killed and the result is None. PROBE_HANG is a hang bound, not a latency budget.	src/verify_tests.rs::a_slow_probe_is_answered_and_a_wedged_one_is_killed_at_its_bound, src/verify_tests.rs::an_older_ty_probes_as_no_capabilities, src/verify_tests.rs::the_version_probe_returns_the_first_line, src/verify_tests.rs::ty_capabilities_are_asked_again_for_another_build	8, 13	Mutex<BTreeMap> cache, closures, child processes (spawnfd), kill, timeouts
ty-cases-pool	contract	verify::{ty_trace_verdict,ty_trace_per_process}	∀ sequence of ty_trace_verdict calls against one binary and (spec, cfg): requests go to a pooled `ty --cases` co-process keyed by (binary identity, spec, cfg); a reply line with the request's id is the verdict; a verdict marked `end` retires its child without spending a retry; class 'error' panics 'ty COULD NOT EVALUATE'; a request with no matching verdict line (EOF, another id, non-JSON) is answered per process, never as an accept; a child that dies before its header is answered per process, the pair gets at most one respawn, then no co-process for the rest of the process; a binary re-laid at the same path gets a fresh child and the old build's memo and co-process are never asked again; find_ty and the verdicts follow a store shim repointed mid-process.	src/verify_tests.rs::a_case_with_no_verdict_line_is_never_an_accept, src/verify_tests.rs::a_dead_cases_child_falls_back_per_process_and_respawns_once, src/verify_tests.rs::an_evaluation_error_is_not_a_refutation, src/verify_tests.rs::find_ty_and_its_verdicts_follow_a_store_update_mid_process, src/verify_tests.rs::the_cases_pool_and_the_memo_answer_only_for_the_build_in_use, src/verify_tests.rs::the_cases_pool_answers_and_an_end_verdict_retires_its_child	1, 8, 13	long-lived child processes with piped stdio, JSON-lines protocol, process-global Mutex state, SIGKILL handling
ty-verdict-memo	contract	verify::memoized_verdict	∀ ty path and question q = (spec, cfg, trace): memoized_verdict(ty, q, run) returns a stored result iff an earlier call asked q and binary_identity(ty) equals that call's, before and after its run, end binary's ctime included; otherwise it calls run(checker, q) once. checker = Some(TyChecker(path, chain)) when ty can be stat'ed, else None, and then nothing is stored; a result whose run saw the identity change is never stored.	src/verify_tests.rs::a_ty_verdict_behind_a_wrapper_is_asked_again_when_only_its_target_changes, src/verify_tests.rs::a_ty_verdict_is_replayed_only_for_the_same_binary_and_files	1, 8, 13	global Mutex<BTreeMap> memo, closures, fs stat
check-main-dispatch	contract	check::main	∀ args: main(args) = 2 when args[0] names no check (usage error); 0 for ['--list'], which prints the check names; a named check returns its verdict code.	src/check.rs::an_unknown_check_is_a_usage_error	1, 8	&[String] matching, stdout printing
model-under-ty-interp-failure	contract	check::model_under_ty, derive::Model::mutant_dials	∀ models and ty path: if interpreting m at a dial d ∈ {0} ∪ mutant_dials(m) panics, the result has a finding containing 'Buggy=d' and the panic text for each such d; m is not counted in `compared` and no external checker runs for it. mutant_dials(m) = {1 if m mentions Buggy} ∪ {k | Buggy = k or Buggy # k appears in m}.	src/check.rs::an_interpreter_panic_is_a_failed_model_check_at_every_dial	1, 7, 13	catch_unwind, recursive Expr walk into BTreeSet
flip-buggy	contract	check::flip_buggy	∀ cfg: flip_buggy(cfg) = Some(cfg with its first 'Buggy = 0' replaced by 'Buggy = 1') if cfg contains it, else Some(first 'Buggy = FALSE' → 'Buggy = TRUE') if it contains that, else None. Intended (and what the test name claims): only a declared `CONSTANT Buggy = ...` line is flipped; the code does not ensure it (§5).	src/check.rs::a_buggy_constant_is_flipped_once_and_only_when_declared	1	&str contains/replacen, Option<String>
ay-obligation-judging	contract	check::ay::{declared,verdict,stamp,judge}	declared(text) = Ok((want, minutes)) iff text has a '(set-info :status unsat|sat)' line, minutes ⇔ '(set-info :aterm-cost minutes)' present; any other :status or :aterm-cost value, or no status, is Err. verdict(stdout) = the lowercased first trimmed line equal (ignoring case) to sat, unsat or unknown, else ''. stamp(stderr) = the nonempty value of the first 'build.stamp=' word, else None. judge(f, want, a) = Ok iff a.stamp is Some and a.verdict = want's word.	src/check/ay.rs::a_verdict_counts_only_when_its_build_is_named, src/check/ay.rs::an_obligation_declares_its_verdict_and_its_cost, src/check/ay.rs::the_verdict_is_the_first_verdict_line	1, 3, 7	&str lines/strip_prefix/split_whitespace, slice patterns, Result<_,String>
ay-discharge-needs-success	contract	check::ay::{solve,obligations}	∀ solver run: solve(ay, ob) = Ok(Answer{verdict(stdout), stamp(stderr)}) iff the process exits with success; a nonzero exit or signal death is Err whatever it printed. obligations(root) returns a nonempty fault list when the walk finds no obligation, so an empty proof directory is never a discharge.	src/check/ay.rs::an_empty_requested_proof_directory_is_not_a_discharge, src/check/ay.rs::an_unsuccessful_solver_cannot_supply_an_attributed_verdict	8	child process (spawnfd), ExitStatus, fs directory walk
spec-link-classification	contract	check::spec_link::{classify_spec_link,report_has_exact_full_coverage,parse_spec_link_header,parse_spec_link_coverage_row,parse_canonical_usize,report_has_spec_link_failure}	∀ (exit, report): classify_spec_link = Some(Certifying) iff exit = Some(0) and the report is exactly one 'spec-link: N machine(s) in <path>' header (N canonical decimal), N well-formed coverage rows 'machine "X" [embedded|external(<nonempty>)] coverage a/b = p% (k anchored, w waived) [linked]' with unique names, a = b = k + w, p the canonical 1-decimal percentage (100.0 for 0/0), exactly one 'spec-link-status: certifying', and no noncert, numbered-violation, 'error:' or harness-manifest line. = Some(ExplicitDesignOnly) iff exit = Some(1), the same grammar with every row [design-only] (partial coverage allowed), status 'non-certifying', and exactly one matching 'spec-link-noncert: code=design-only detail=...' line per row. Everything else is None (fail closed). report_has_exact_full_coverage(r, X, n, d) ⇔ r classifies as d, has n rows, and X's row has a = b. The live registry's design-only report classifies as ExplicitDesignOnly.	src/check/spec_link.rs::spec_link_report_classification_accepts_only_honest_zero_violation_states	1, 3, 5, 7	line-grammar parsing over &str, Vec<CoverageRow<'_>> of borrowed slices, BTreeSet uniqueness, integer percentage formatting
trust-ir-lowering	contract	ir::{lower_to_ir,canonical_fallback_anchors,lower_module_block,emit_embedded_vars,q}	∀ (name, modules, anchors, waivers): the output starts '; TrustIr text format v1\nmodule "<name>"\n'; each module gives a 'spec_module "<Name>" {' block with origin, enforcement, 'var "v" : "Int"' per variable, 'action "A"' per action and 'invariant "I" : "<to_tla>"' per invariant. An anchor or waiver whose machine names a module case-insensitively is emitted under the module's canonical name; one naming no module, or with an empty machine, is dropped. Anchors equal on (machine, action, rust path) collapse to one, preferring a nonempty projection, then the least span, so the output does not depend on input order. An empty projection omits the 'project' clause.	src/ir.rs::anchor_without_project_omits_project_clause, src/ir.rs::embedded_block_emits_vars_actions_invariants_and_canonicalizes_machine, src/ir.rs::fallback_anchor_identity_is_deduplicated_deterministically, src/ir.rs::header_is_v1_canonical, src/ir.rs::unresolved_or_bare_records_are_dropped	1, 7	String building, sort/dedup over Vec of borrowed records, case-insensitive matching
tla-definitions-parse	contract	tla_check::{TlaSpec::parse_str,extract_module_name,update_comment_depth,extract_definitions,extract_definition_name,is_tla_keyword,extract_next_disjuncts,leading_identifier}, xref::SpecModule::{action_names,invariant_names}	∀ TLA+ text c, path p: parse_str(c,p) = Err iff c has no '---- MODULE X ----' line. Otherwise module_name = X, file_path = p, actions = the names N with a top-level 'N ==', 'N(args) ==' or 'THEOREM N ==' outside block comments, which nest to any depth; TLA keywords and the '====' marker are excluded. extract_next_disjuncts(c) = the action names each disjunct of Next calls (single- or multi-line, plain or after '\E ... :', a single action included). For an External module action_names() = the Next disjuncts and invariant_names() = all top-level definitions.	12 primary tests (§2), e.g. src/tla_check.rs::next_disjuncts_plain_and_quantified, src/tla_check.rs::next_disjuncts_single_action, src/tla_check.rs::test_deeply_nested_block_comments	1, 3, 7	line scanning over &str with a u32 depth counter, BTreeSet<String>, Result
step-audit-definitions	contract	xref::{StepLedger::audit,StepAudit::is_complete,StepAudit::widest_window}	∀ anchor table A (action, fn, site id) of one machine and step windows W (action, set of entered site ids): unwitnessed = actions none of whose windows entered one of their own sites; blind_steps = windows that entered none of their action's sites; witnessed_steps = those that did; dark_sites = anchored sites no window entered; entered_under[f] = actions whose windows entered a site of anchored fn f; interchangeable[a] = anchored fns entered in a's windows; indiscriminate = actions whose anchored fn another action's window also enters; stray[f] = actions whose windows enter f though f is not anchored for them; widest_window = max |interchangeable[a]|; is_complete ⇔ unwitnessed, blind_steps and stray are empty. So moving an anchor to a fn whose entry pattern equals the claimed action set, or swapping two actions on one fn, changes nothing observable.	src/xref.rs::a_move_inside_one_window_is_caught_exactly_when_the_new_site_is_wider, src/xref.rs::a_second_anchor_site_that_never_runs_is_named_rather_than_carried, src/xref.rs::a_window_that_misses_its_own_action_is_named_even_when_a_sibling_window_saw_it, src/xref.rs::entered_under_fires_where_the_old_ubiquitous_intersection_could_not, src/xref.rs::interchangeable_counts_what_a_window_leaves_ambiguous	1, 5, 7	BTreeMap/BTreeSet set algebra over &'static str
entered-anchor-window	invariant	xref::{reset_entered_anchors,note_entered,disarm_entered_anchors,take_entered_anchors,merge_entered_anchors,entered_anchor_ids,entered_anchor_count,window_is_armed}	Per thread the window is (armed, record). reset makes it (true, ∅) and returns true; note_entered(id) inserts iff armed; disarm closes it and keeps the record; take returns Some(record) and makes it (false, ∅) iff armed, else None and no change; merge(ids) unions into an armed window and returns true, and returns false recording nothing for a closed one; count and is_armed read the state. Nothing written on one thread reaches another thread's window except through take then merge.	src/xref.rs::a_workers_window_reaches_its_owner_only_by_take_and_merge	2, 8, 13	thread_local! RefCell state machine, BTreeSet, threads
xorshift64	contract	rng::XorShift::{new,next_u64,below,index,range,pick}	∀ seed s: new(s) has state s, or 0x9E3779B97F4A7C15 when s = 0; next_u64 maps x ↦ x' with x1 = x ^ (x<<13), x2 = x1 ^ (x1>>7), x' = x2 ^ (x2<<17), stores and returns x' (so the sequence is a function of the seed; new(1).next_u64() = 1082269761). ∀ n ≥ 1: index(n) < n; ∀ lo < hi: range(lo,hi) ∈ lo..hi; ∀ nonempty xs: pick(xs) ∈ xs.	src/rng.rs::bounded_draws_stay_in_bounds, src/rng.rs::the_sequence_is_xorshift64_and_fixed_by_its_seed	2, 3	u64 shifts/xor, %, try_from casts, &mut self, slice index
wait-within	contract	wait::{within,until_within}	∀ budget b, interval i, probe: within(b,i,probe) calls probe at least once and returns the first Some(v) it yields, or None once probe has yielded None and b has elapsed since the call, sleeping i between probes. until_within(b, what, probe) = v when within(...) = Some(v), else it panics 'timed out after {b:?} waiting for: {what}'.	src/wait.rs::until_within_names_what_it_waited_for, src/wait.rs::within_answers_the_first_some_and_none_after_the_budget	7, 8, 13	FnMut closure, Instant/Duration, thread::sleep, loop
first-exec-once-scratch	contract	wait::{first_exec,run_once,file_id,scratch_dir}	∀ path p, args: first_exec(p, args) runs p at most once per file identity (dev, ino; a rename-replaced file is a new key) per process, and runs nothing when p does not exist (file_id(p) = None). The run sees only PATH, HOME = TMPDIR = a fresh scratch dir under temp_dir (not temp_dir itself) and the shell's own variables, and that dir is removed after the run.	src/wait.rs::first_exec_runs_a_file_once_in_a_scratch_environment	8, 13	child process with env_clear, fs metadata, process-global memo set
```

### 1.1 Distribution by kind (56)

| kind | count | ids |
|---|---|---|
| contract | 34 | value-accessor-fail-closed, bounds-are-ranges, model-registry, bmc-reach-semantics, admits-iff-some-action, scc-components, nonprogress-cycle-fair-lasso, dead-negative-control-audit, ty-transcript-parsers, explored-space-guard, scalar-tier-always-runs, prove-and-catch-contract, tool-discovery-order, shell-literal-word, binary-identity-chain, provenance-claim-state, provenance-line-rendering, civil-date, ty-capabilities-parse, ty-capabilities-cache-and-probe, ty-cases-pool, ty-verdict-memo, check-main-dispatch, model-under-ty-interp-failure, flip-buggy, ay-obligation-judging, ay-discharge-needs-success, spec-link-classification, trust-ir-lowering, tla-definitions-parse, step-audit-definitions, xorshift64, wait-within, first-exec-once-scratch |
| invariant | 7 | registered-model-sound, model-claim-breaks-first, model-deadlock-free, test-only-laws-lifted, registry-intrusive-list, provenance-report-per-tool-serial, entered-anchor-window |
| table-property | 6 | model-strict, mutant-table-catch, declared-dials-and-scenarios, prove-catch-multiply, guard-conjunct-audit, sound-step-edits-refute |
| equivalence | 5 | text-zero-projection, interpreter-semantics, expr-tla-agrees-with-eval, compiled-walk-equals-by-name, one-walk-ghost-sweep |
| other | 2 | liveness-obligations-sound, model-laws |
| round-trip | 1 | model-tla-rendering |
| refinement | 1 | ty-model-desugar |

### 1.2 Per-model law catalogue

`model-laws`, `mutant-table-catch`, `model-strict`, `declared-dials-and-scenarios` and
`liveness-obligations-sound` are registry-level obligations. What each one asks of a given model is
the content of its tests. For the models below the recovery passes stated that content per model;
§2's `also` column names the entry as `law:<entry>`. For the models of `tests/derived_ring_ty.rs`,
`tests/derived_supervise.rs` and the other files of the second pass the content is recovered per
test (the R13 instances: guard laws, step laws and reach obligations) and is to be lifted from each
test when its model gains the M1 surface.

| entry | models (`derive::*`) | Law(M) and catch/live clauses the tests sample |
|---|---|---|
| atpkg-contention-release | atpkg_contention_release_park_model | Check while the writer lock is held ⇒ decision' = 0; Check after Release with nothing offered ⇒ 3 (retry on the first slice after release); OfferBump ⇒ 1, OfferVendor ⇒ 2, Disable ⇒ 0. Catch(B=1): early Check breaks HeldWriterKeepsParked; Release;Check breaks ReleasedWriterWakesOnCheck. |
| atpkg-flip-quiet | atpkg_flip_quiet_model | Hold is enabled only while the toolchain is in use, not revoked and under the 3-look ceiling; at the ceiling Flip is enabled and keeps NoFlipUnderABuild; a revoked build never enables Hold. Catch(B=1): Unsure;Flip, Use;Revoke;Hold, four holds break NoFlipUnderABuild, RevokedNeverWaits, WaitIsBounded. |
| atpkg-full-pass | atpkg_full_pass_rule_model | A SessionLook after an old-enough success and a later OtherWrite queues the owed pass (queued = 1, verdict = 0); a child taking the lock behind a failed pass stands down. Catch(B=1): the misread breaks NoOwedPassHeldBack and NoSuccessReadAsFailure; back-to-back passes break NoPassBackToBack. Scenario negative control: bmc(M[Interval=5]) = Err(NoOwedPassHeldBack). |
| atpkg-head-watch-hosts | atpkg_head_watch_hosts_model | WindowAdmits during a session's round leaves the window refused; after the round the window watches and the session stands by; WindowCloses lets the session watch again; a second session stands by and takes the seat on the first's exit. Catch(B=1): S1RoundsOnStaleProbe, S2RoundsWithoutSeat, S1ExitsKeepingSeat break NeverBothKinds, OneSessionWatcher, NoOrphanedSeat. |
| atpkg-index-pending | atpkg_index_pending_park_model | Audit Ok(0). A ready index beats a ready vendor (ChooseVendor disabled while an index result is harvestable); a ready vendor survives index priority and full-pass preemption and is retired once delivered; a covered near hint is never chosen, a newer one is (chosen = 3); SpawnExtra never yields a second owned worker; a stale answer is never applied; VendorSpawnFailed schedules a retry, never an inline get. Catch(B=1): a witness for each of its six invariants. |
| atpkg-index-probe-family | atpkg_index_{shared_handoff,probe_completion_cadence,probe_cooldown,successor_selection,wake_highwater}_model | SharedHandoff: once the shared stamp expires the suppressed host rechecks (requests = 1). CompletionCadence: the next probe is timed from the shared stamp's completion. Cooldown: after ProbeMissing;Release;Acquire SkipFresh is enabled and ProbeMissing is not. SuccessorSelection: Choose picks the highest available hint, never a false missing. WakeHighwater: a failed mark never downgrades and an older hint is never replayed. Catch(B=1): each model's named invariants. |
| atpkg-index-publish | atpkg_index_publish_walk_model | Strict. A writer targets the successor of the newest PUBLISHED index, never of a draft (after a killed publish only RefuseB is enabled); A's re-run converges its draft and the walk lands it (floor = 2); of two publishers on one baseline the first create wins. Catch(B=1): skipped number, building on a draft, converging onto A's tag break OneRun/WalkLandsNewest/NoOverwrite. |
| atpkg-pass-stamps | atpkg_pass_stamps_model | Catch(B=1), two isolated defects: PK-3 breaks HealedOnlyByAReachedPass and StandDownOnlyBehindAReachedPass; PK-4 breaks FailedMeansTheLastPassFailed and SpacingCountsOnlyFullPasses but not HealedOnlyByAReachedPass. The same traces keep every invariant at B=0. |
| atpkg-pending-wait | atpkg_pending_wait_model | A pass that sets the program up and ends between the stub's two reads enables DecideRun, not DecideFail; a pass that died before setup leads to DecideFail and NoFalseFailure holds. Catch(B=1): the race breaks NoFalseFailure; a dead pass taken for an install breaks RunsOnlyInstalled. |
| atpkg-published-spacing | atpkg_published_spacing_model | A newer published index escapes spacing for the GUI and the queued child (1,1) after an old ok or failed end; a same-target failure, an unknown end, a stale witness or a live holder makes both wait (2,2). Catch: B=1 breaks five invariants on their schedules; B=2 breaks SameTargetWaits. |
| atpkg-session-index | atpkg_session_index_{eligibility,handoff}_model | Eligibility: an empty local look does not delay the first eligible probe. Handoff: a near answer requests once, its launch waits out an open window, a far replay never launches again, a verified floor launches nothing. Catch(B=1): NoArtificialNetworkWarmup; OneNetworkWorker, NoWindowOverlap, OnePassPerAnswer. |
| atpkg-session-index-retry | atpkg_session_index_retry_model | A same-build retry is admitted only after its back-off (5 then 15 min): EarlySame never increments passes, RetryFirst/RetrySecond do; NewerHint resets to (1, 1, 0). Catch(B=1): First;EarlySame breaks NoPrematureSameBuildPass. |
| atpkg-vendor-pending | atpkg_vendor_pending_check_model | At most two checks are active; results are harvested in either order, each exactly once; a failed head gets a retry. Catch(B=1): SpawnExtra, CompleteTwice, OfferTwice, DropFirstReady, LoseFirstRetry each break one of its five invariants. |
| broadcast-cursor | broadcast_cursor_checkpoint_model, broadcast_head_subscription_model | Checkpoint: across crashes an irrelevant cursor may lag but durable = delivered and reoffered = 0. Head: a head subscriber added after a record reached the broker starts past it. Catch(B=1): NoDeliveredReplay; HeadSkipsEarlierRecord. |
| budgeted-search-resume | budgeted_search_resume_model | Audit Ok(0). Start opens a reset-marked stream with a fresh id; Resume keeps cursor and id and clears reset; each restart resets progress to 1, mints exactly one id and sets prior_id; Cancel zeroes public state without reusing ids; only DrainComplete retires the cursor; Drain has no stutter successor. Catch(B=1): a witness for each of its five laws. |
| chrome-regrow | chrome_regrow_floor_model, chrome_regrow_floor_liveness | Audit Ok(3). No fall is enabled within the floor after another source's rise (after a person's read too); a want back re-grids nothing; a rise is never delayed; Cramp takes a waiting fall at once. Catch(B=1): NoQuickRegrow on three schedules; Restart breaks TheAnchorIsTheLastRise. Live(regrow-floor); the Restart lasso moves Restart and Tick and no fall. |
| console-pet-z-order | console_pet_z_order_model | Strict, audit Ok(0), every action fires at B=1. Ink sets under = 1 on the same frame; Clear keeps under = 1 until ClearDwelt; ClearDwelt is disabled with no run; Hide releases the hold. Catch(B=1): ClearNeverSinks; SurfacingWaitsTheDwell. |
| crash-journal | crash_journal_claim_model | Every committed action fires except the mutant-only FinishWrite. Claim is enabled only for an unclean end of a window that wrote; a claim takes the image once; SIGKILL after a write is reopened; a hand-off or a quit cut short after its layout is not reopened; a crash inside 90 s is skipped; a first stop inside 90 s is reopened once; SuccessorDies is disabled until Adopt; nothing is lost. Catch(B=1): a counterexample for each of six invariants. |
| debugger-access | debugger_access_change_model, debugger_access_change_liveness | Launch only after Press, Consent and a locked Recheck over a fresh read; no Press or PeerLaunch while our child holds the lock; a child's exit claims nothing until Notice;Probe; Cancel/Decline retry nothing; a rule moved after render aborts; a peer's lock refuses us until PeerExit; Adopt only an ended peer change, Follow a running one. Catch: each of 8 mutants alone breaks exactly its invariant (Iso form). Named residual: NoClobber is violated at B=0 by Customize;ChildClobbers. Live; ResultWakeLost's lasso sits at phase 5. |
| desktop-alert-debt | desktop_alert_debt_model | Holds at Fault ∈ {0..3}. Disable cancels pending debt and notices, keeps seen = label; observations while disabled create no work; Enable replays neither. Catch: Fault=1 breaks only DisabledOwnsNoDebt; 2 breaks DisabledOwnsNoDebt and OnlyEnabledNotices; 3 breaks only SeenTracksLabel. |
| driver-geometry | driver_geometry_model, driver_geometry_liveness | Audit Ok(4). No Regrid while a lease's hand may type; the fence refuses on program output or a pre-lease re-grid and never counts it against the hand; Commit is disabled after the lease moved since the Sense until a re-Sense; a peer's unseen lease is caught up only by Catch;Commit. Catch: four mutants break their named invariants only. Live; DropNoWake leaves the goal false with neither Sense nor Commit enabled. |
| editor-carry | native_update_editor_carry_model | Strict. A typed, journaled draft rides the update; typing during the overlap disables Commit; Rollback then Reseat returns the journal with the latest edit and Plan is disabled while the reseat is owed; an append on the worker disables Start; a failed restore still allows Commit; a clean document carries nothing. Catch: B=1 breaks NoDraftLost and JournalOwnedOutsideHandoff; B=2,3 break only NoDraftLost. |
| exceptions-digest | exceptions_digest_model, exceptions_digest_ends_model, exceptions_digest_ends_liveness | Digest: a Later never seats a row; a dismissed fact is not posted again inside its span; at the cap a post is filed, never dropped. Catch(B=1): four mutants break exactly four invariants. Ends: Live(AnUnnamedEntryEnds) under Tick and Settle; a flicker clears the clock; two mutants give lassos, two break only their invariant. |
| exceptions-once | exceptions_once_model | Strict. After Assert;Log;Show;Dismiss;Restart, Assert is disabled for the same cause and re-enabled by Drift; a dismissal filed between Handoff and Commit keeps the cause quiet in the successor. Catch (knobs Reopen, Early, Volatile, StaleSuccessor, NoFlush): each gives bmc = Err with its stated first invariant. |
| fabric-lanes | fabric_outbox_wake_model, fabric_reconnect_backoff_model, fabric_refill_retry_model | Outbox: Queue enables PromptDrain; a lost event is delivered by the second BackstopTick. Reconnect: Events before the Deadline leave result = 0, Deadline sets 1, Closed sets 2 and disables Deadline. Refill: a failed fetch keeps old mail owed and delivered before new mail; DeliverNew is disabled until the refill completes. Catch(B=1): the named invariants of each. |
| foreground-handback | foreground_handback_model, foreground_handback_ownership_model | Strict, audit Ok(0). On the INCIDENT, ADOPTED, STOP_FG, PARKED and ONE_SHOT schedules the prompt is handed back with the shell's own modes; a stopped job keeps its modes until it dies; a one-shot keeps its display modes. Catch: B=1 and the knobs Split, Carry, Alive, Display break their stated invariants. Ownership: a job that armed in its own bytes is handed back; B=1 breaks ObservedArmIsHandedBack. |
| handoff-address-owner | handoff_address_owner_model | While this process owns the session SuccessorPublishes is disabled; its supervisor's request is served; after Commit it publishes; a rejected attempt names nothing; a stranger is refused. Catch: B=1, Early=1 (M[Early=1] minus that invariant holds) and Blanket=1 break their named invariants. |
| harness-capture-and-daemon | harness_capture_worker_lifecycle_model, harness_codex_daemon_update_model | Capture: Return is enabled only after the reader joins. Daemon: Update is disabled while a detached turn or background terminal runs, under the owner's hold, under an unseen client and over a vendor-ahead build, and re-enabled as each is released. Catch(B=1): the named invariants. |
| harness-host | harness_worker_lifecycle_model, harness_upgrade_look_model, harness_handoff_claim_model, harness_leave_model | One supervisor per session; the budget is the session's (survives Leave/Arrive); Off stops the worker and starts nothing until TurnOn; a refused or unwritten look owes a later one; Grace > Renew and a carried lease or grace blocks HostClaims; an exit is handed only for an exit and the upgrade's own restart is carried on. Catch(B=1): each model's named invariants; HandoffClaim's mutants are disabled at B=0. |
| harness-login-wall | harness_login_wall_model | Strict; each invariant load-bearing alone; Catch(KNOBS). At the wall nothing is typed and no clock runs; once LoginBack the notice is retyped as the same ask; an inherited give-up is retaken and given up only after MaxAsks; a renewable login asks to renew, then Relaunch and Continue. The incident run at B=1 is refused by admits(M) at Continue and breaks every INCIDENT invariant. |
| harness-relaunch | harness_relaunch_on_exit_model, harness_exit_record_model, harness_codex_exit_witness_model | Badged after exactly Badge misses and still tried; back-off saturates at Steps and Healthy resets it; a person's exit leaves the tab to them; the harness's own Restarted is never taken off. A crash is read at its exit and kept through a Sweep; a graceful exit is looked at only after its own removal. A Codex exit is relaunched exactly for a told crash. Catch(B=1): the named invariants. |
| harness-restart-stamp | harness_restart_stamp_model, harness_restart_stamp_liveness | The act's end is read as the act's through TERM, escalated KILL, or Commit plus the carrier's finish; a joined second act sends nothing; one watch per process; an act that wrote its stamp and sent nothing takes it back; while another hand or claim stands five actions are disabled; nothing is decided while the program runs. Catch: 10 mutants, each alone. Live; the TermOnly lasso keeps alive = stamped = 1. |
| harness-restored-carry | harness_restored_carry_model, harness_commit_owed_model | A step the park caught mid-run is carried and relaunched; a step resolved or an agent run during the park refuses Commit; no StepStart while parked. An owed relaunch through a failed Commit is never lost (queued by the rollback, handed by the host, carried by the next park). Catch(B=1): NoOwedRelaunchLost on 18 historical schedules, and the other named invariants. |
| harness-state-collection | harness_upgrade_state_collection_model, harness_upgrade_state_collection_liveness | Prune ⇔ the tab is closed ∧ the stage is settled ∧ the record is a week old. Catch: four mutants alone. Live(collects); the historical lane (no Prune) holds every invariant but fails the liveness. |
| harness-upgrade-age-drain | harness_upgrade_age_model, harness_upgrade_drain_bound_model | An age belongs to one process (after HandRestart and NewBuild the record is fresh, Judge disabled). A held answer is voided at the bound and never ends the agent; the agent's own work is waited on up to the bound; a late READY gets a whole bound; a lagging status is re-asked and given up. Catch: B=1 and knobs LagEnds, LagHolds. |
| harness-upgrade-goal-pause | harness_upgrade_goal_pause_model, harness_upgrade_goal_pause_liveness | Audit Ok(8). No GoalPause before Advance or under an open switch; LadderMove waits for the last turn; LetGo waits for the one owed GoalResume; a person's resume or pause makes it theirs. Catch(B=1): three mutants. Live(lands) with three stated lassos. |
| harness-upgrade-ladder | harness_upgrade_ladder_model, harness_upgrade_ladder_liveness | Audit Ok(11). Move is never enabled within the keys gap, over a draft or box, or mid this client's turn; a person near the tab holds until KeysOnly; another session's turn holds until two looks place it; a hidden or subagent turn is never placed. Catch(B=1): eleven mutants. Four livelock lassos with stated entries. |
| harness-upgrade-limit-queue | harness_upgrade_limit_queue_model | Strict; each invariant load-bearing; Catch(KNOBS). ∀ s: En(Look,s) ⇔ (phase > 0 ∧ limited = shown = held = stuck = 0 ∧ no STEP enabled); behind a queued notice no STEP or NowAsks is enabled; untaken ≤ TOP; phase = 2 ⇒ took = MaxAsks. Path law: once the limit ends the upgrade asks again or gives up by its own steps (NoBound, ForGood violate it). The owner's report run is the caught negative control. |
| harness-upgrade-never-strands | harness_upgrade_never_strands_model | Strict; three invariants load-bearing; Catch(KNOBS). In the quiet phases En(Look) ⇔ none of Restart/BuildHold/Release/DropRelease/Rearm is enabled; En(DropRelease) ⇒ holding = 0; no reachable state is a permanent wait; a limited session is asked after its reset; a failed build strands no agent. The incident and the owner's stall replay as negative controls. |
| harness-upgrade-notice-owner | harness_upgrade_notice_owner_model | Strict; five invariants load-bearing; Catch(KNOBS) exact. A READY from another tab, a duplicate owner or a partial scan disables Terminate; with the notice's process gone and a live holder Reask is enabled and only the new READY enables Terminate; ∀ s: En(Look) ⇔ ¬En(Reask) ∧ ¬En(Terminate). |
| harness-upgrade-progress | harness_upgrade_progress_model, _liveness, _truth | Audit Ok(4); |Reach| > 500. ∀ s where Watch fires: (s·Watch).cause = seen = T(s) and Watch never moves phase. Edited-step negative controls (see sound-step-edits-refute). On main_today nonprogress is None; the settled-break re-decision differs from the old yardstick only at posture = 1. |
| harness-usage-tap | harness_usage_tap_model | Strict; invariants load-bearing; Catch(KNOBS). ∀ s: figs ≥ noted ∧ (tapped = 0 ∨ pid > 0); an idle repeat read after a served turn records nothing stale; NoFresh breaks NeverNotesStale. |
| input-unread | input_unread_gate_model | With a published, established stall DriverKey is disabled and HumanKey stays enabled; a merely slow program gets DriverKey (warned in the warn band) and is never Published; a stopped job refuses at any age; canonical type-ahead is written without warning; a read after Publish enables DriverKey and Withdraw. Catch(B=1), the screen fence and the static cap. |
| keeper-custody | pty_keeper_custody_model | Every committed action fires; uncaught_invariants = [RelaunchSpace]. After a crash both masters are offered and adopted; a crash before REGISTER loses only that shell; a quit disables Relaunch; a closed tab is never offered; an offer returns to orphan when its link drops; a release from one of two claimants keeps the copy. Catch(B=1): all nine design invariants. |
| keeper-brake-and-death | keeper_relaunch_brake_model, keeper_death_judgement_model | Brake: a boot trial gets three relaunches then a hold only ManualLaunch lifts; Healthy resets the streak; CleanQuit grants nothing. Death: a lost-BYE quit with the marker gone is a quit; SIGKILL and a panic exit are orphaned; the P3-window lost BYE is a crash; an orphan whose shell dies is pruned. Catch(B=1): the stated counterexamples. |
| launched-dial-wait | native_update_launched_dial_wait_model | Catch: ∀ (x, I) ∈ MUTANTS: bmc(Iso(M,x)) = Err(I), and M[B=1] minus the mutants holds. A successor dead before its dial ends the wait on AnswerExited or on the kernel's word, budget unspent; a named pid alone never ends it; a live successor never enables EndOnExit. |
| main-stall-refusal | main_thread_stall_refusal_model | Probe refuses ⇔ the main thread is stalled (refused = stalled on every listed path); a dialog is never a stall; a hop counts until Taken, not until its Reply. Catch(B=1): three mutants each break only one invariant. |
| render-small-lanes | metal_drawable_acquire_model, native_leaf_cache_lifetime_model, net_slot_displacement_model | MetalDrawableAcquire (function-valued: ty tier only): each historical failure has an independent witness at B=1. NativeLeafCache: Sound; BindSplit;Paint;Bind* breaks OnlyVisibleLeavesRetainRasters. NetSlotDisplacement: uncaught = []; at capacity Accept is disabled; Evict frees one pending slot and the newcomer waits until Release; no relay is evicted. |
| output-echo | output_echo_receipt_publication_model | Strict (exactly the five healthy actions fire). PublishOlderEcho is disabled before acceptance; a delayed older completion never replaces the newer boundary; Sample reads one published version. Catch(B=1): each defect seen only by its own laws. |
| paste-order | queued_key_kernel_delivery_model, paste_order_sink_isolation_model | PostWake is disabled after AcceptSpill, even after a later DrainSuccess; a paste in A never orders a key in B; a partly drained A gives decision = 1. Catch(B=1): WakeRequiresDirectReceipt; DecisionReadsOnlySelectedSink. (The tests discard prove_and_catch_tiered's Result; §5.) |
| pet-observation-and-seam | console_observation_admission_model, pet_surface_seam_model | Admission: an incoherent observation freezes motion (TickAdvance/Retire/Launch disabled), a coherent one recovers it; the launch and advance mutants are separate. Seam: audit Ok(0), every action fires at B=0 and B=1; a regrid keeps the body; a replacement crossfades; lost glass spends no ghost; every ghost leaves at full presence. Catch: B=1 and five knobs. |
| precommit-input | native_update_precommit_input_model | A reordered or lossy stream is repaired, and only such a stream; Drop is disabled on an empty queue; after Commit no queue action is enabled. Catch(B=1): RepairOnlyForACause, ReorderedInputIsRepaired. |
| relaunch-prompt-read | relaunch_prompt_read_model, relaunch_prompt_read_liveness | With the mark elsewhere, a stamp, a trailing blank and nobody keyed, Read is enabled and types; a person's Key disables Read; with no mark and no blank Read and WaitTyping are disabled. Live (a prompt nobody keyed is typed at). Catch: four mutants, one with a lasso. |
| release-publish-once | release_publish_once_model | Audit Ok(11); uncaught = []; without invariants deadlock-free to phase ∈ {5,6} and caught at B=1; without ReassertHead it wedges. The head is reached only through Ratchet;Bind;UploadSig;UploadToml;ProveAssets;Ratchet;MakeHead;ProveHead; a newer head is refused; Abandon is disabled over a head. Catch: 11 mutants. Guard-conjunct audit (see guard-conjunct-audit). |
| resize-render | resize_render_model | uncaught = [Bounded]. A quiet flap is an identity; a displaced net-zero run drawn on is a risk until a changed-size repaint heals it; a net-changed run drawn on is unverified, never ok and never a risk; a trim is no risk; an earlier restored flap excuses no later one; an epoch judges the residual debt. Catch: B=1 and multi-valued dials 2..8 break exactly their stated sets. |
| rendezvous-admission | native_update_rendezvous_admission_model | Catch(B=1) exact: four paths each break exactly one invariant. After TrustdSilent neither Grant nor Refuse is enabled; TrustdAnswers enables Grant; DeadlineWhileHeld;FileByCause ends the hold. |
| repaint-nudge | repaint_nudge_model, repaint_nudge_liveness | Live(TheNudgeEnds). The healthy run heals; a silent app is given up within budget after one try, holding the PTY and never the engine; a lasting resize is superseded, never a give-up; a standing halt disables Hold; an orphaned hold is released by the deadline. Catch(B=1): five mutants. |
| resize-row-reuse | resize_row_reuse_model | Sound at BypassReuse ∈ {0,1}. Repeated Shrink;Output;Grow keeps (live, free, allocated) = (2,0,2); Rebuild resets. Catch: B=1 breaks OwnedExactlyOnce and BoundedByHighWater; B=1 with BypassReuse breaks only BoundedByHighWater. |
| ribbon | ribbon_follow_twin_model, ribbon_arrival_evidence_model, ribbon_release_restoration_model, ribbon_row_hold_model | An arrived block with one standing twin names the whole run; arrival evidence dials 1-3 each break one invariant alone; TryRestore is the identity until ObserveFull after a Release, then restores once; Abandon/Retire/Replace revoke custody; returning to an earlier row is permitted with the owner keeping its lease. |
| window-show | native_update_window_show_model | No negative controls (fired = all actions). Three update schedules: in aterm → keyed = fs = fsfront = 1; elsewhere EnterFullScreen is disabled until Activate; typing before/after the proof leaves keyed = 0, typed = 1 with every invariant holding; an untyped Steal is undone at Commit; Stack/Commit/EnterFullScreen disabled before Reveal/Prove/Commit. |
| rainbow-composer-newline | rainbow_composer_newline_gate_model | PlainModifiedKey and CancelBeforeHome leave ObserveHome/FirstEcho disabled; TypeAheadBeforeHome leaves spent = 0 until ObserveHome, then FirstEcho sets spent = 1. Catch(B=1): three one-step paths. |
| rainbow-short-wrap | rainbow_short_wrap_park_model | FollowOther keeps CancelSameEnd enabled and FlushLicensed leaves armed = 0; FollowExact disables CancelSameEnd; FlushLicensed arms, Replay relays and disarms. Catch(B=1): three invariants. |
| upgrade-bounded-ownership | upgrade_settle_turn_end_model, upgrade_refused_turn_end_model, harness_upgrade_work_end_model | ∀ s: owns(s) = 1 ⇒ ¬En(LoopContinues); after more than Bound owning looks owns = 0, LoopContinues is enabled and leaves over = 0; the count resets on Types and OwnerWord, not on PointComes; HostAsks is enabled only after work was seen since the notice; HostHolds owns the point only inside the re-ask window; with asks spent the round gives up. Secondary dials: see declared-dials-and-scenarios. |

### 1.3 Proof dependency order

`interpreter-semantics`, `value-accessor-fail-closed` → `bmc-reach-semantics` →
`compiled-walk-equals-by-name`, `scc-components` → `nonprogress-cycle-fair-lasso`,
`one-walk-ghost-sweep`, `dead-negative-control-audit` → `prove-and-catch-contract`,
`explored-space-guard` (with `ty-transcript-parsers`) → the registry specs. The registry specs
are checked at Tier 0 as soon as M1 gives each model the surface to state them; they are only as
trustworthy as the checker chain, which is the compiled-Rust proof target. `ty-model-desugar` and
`model-registry` make "the registered model is what the source says" a theorem rather than an
assumption. `model-tla-rendering` with `expr-tla-agrees-with-eval` is what makes the ty tier an
independent check of the same model.

## 2. Test → spec map (TSV; all 723)

`primary` is the spec that retires the test. `also` lists the other specs the same test samples,
and `law:<entry>` names its §1.2 catalogue entry. Paths are relative to `crates/aterm-spec`.
For the first-pass model files (the `law:` rows) the primary was assigned by what the test
asserts: a defect table → `mutant-table-catch`, an audit count or dead-set pin → `model-strict`,
a fairness lasso → `liveness-obligations-sound`, otherwise the walk → `model-laws`.

```tsv
file	test	primary	also
src/check.rs	a_buggy_constant_is_flipped_once_and_only_when_declared	flip-buggy	
src/check.rs	an_interpreter_panic_is_a_failed_model_check_at_every_dial	model-under-ty-interp-failure	
src/check.rs	an_unknown_check_is_a_usage_error	check-main-dispatch	
src/check.rs	the_dead_action_report_is_read_in_both_forms	ty-transcript-parsers	
src/check.rs	the_violated_law_is_read_off_the_transcript	ty-transcript-parsers	
src/check/ay.rs	a_verdict_counts_only_when_its_build_is_named	ay-obligation-judging	
src/check/ay.rs	an_empty_requested_proof_directory_is_not_a_discharge	ay-discharge-needs-success	
src/check/ay.rs	an_obligation_declares_its_verdict_and_its_cost	ay-obligation-judging	
src/check/ay.rs	an_unsuccessful_solver_cannot_supply_an_attributed_verdict	ay-discharge-needs-success	
src/check/ay.rs	the_verdict_is_the_first_verdict_line	ay-obligation-judging	
src/check/spec_link.rs	spec_link_report_classification_accepts_only_honest_zero_violation_states	spec-link-classification	
src/derive.rs	a_bound_must_be_a_range	bounds-are-ranges	
src/derive.rs	action_enabled_reflects_guards	interpreter-semantics	
src/derive.rs	ascii_grapheme_fast_path_model_proves_and_catches_mutant	model-strict	registered-model-sound
src/derive.rs	cursor_model_emits_unchanged_and_disjunctive_next	model-tla-rendering	
src/derive.rs	deadlock_cfg_flips_the_line	model-tla-rendering	
src/derive.rs	evict_full_emits_function_valued_tla	model-tla-rendering	
src/derive.rs	generated_tla_parses_and_exposes_its_defs	model-tla-rendering	
src/derive.rs	gpu_frame_ring_model_proves_and_catches_mutant	model-strict	registered-model-sound
src/derive.rs	ill_typed_bool_position_fails_closed	value-accessor-fail-closed	interpreter-semantics
src/derive.rs	ill_typed_int_position_fails_closed	value-accessor-fail-closed	interpreter-semantics
src/derive.rs	kernel_and_snapshot_generate_expected_tla	model-tla-rendering	
src/derive.rs	metal_atlas_residency_model_proves_and_catches_mutant	model-strict	registered-model-sound
src/derive.rs	ring_generates_expected_tla	model-tla-rendering	
src/derive.rs	rle_local_compact_model_proves_and_catches_mutant	model-strict	registered-model-sound
src/derive.rs	single_hold_prediction_reconcile_model_proves_and_catches_mutant	model-strict	registered-model-sound
src/derive.rs	sub_with_additive_rhs_parenthesizes_to_match_the_interpreter	expr-tla-agrees-with-eval	model-tla-rendering
src/derive.rs	subscribe_emits_parenthesized_disjunction_guard_and_eq	model-tla-rendering	
src/derive.rs	transact_emits_conjunctive_guards	model-tla-rendering	
src/derive.rs	transition_spec_opens_init_but_shares_the_action	model-tla-rendering	
src/derive/models_console_life.rs	derived_cursor_home_proves_and_catches_ink_disappearance_and_stranded_residency	NON-SPEC (delete)	
src/interp.rs	a_lane_that_always_lands_is_clean_and_its_fairness_is_what_makes_it_so	nonprogress-cycle-fair-lasso	
src/interp.rs	a_model_writing_an_undeclared_variable_is_walked_as_written	compiled-walk-equals-by-name	
src/interp.rs	a_retry_loop_that_never_lands_passes_every_older_check_and_is_found	nonprogress-cycle-fair-lasso	
src/interp.rs	a_space_answers_what_the_one_shot_walks_answer	nonprogress-cycle-fair-lasso	
src/interp.rs	a_step_that_changes_nothing_is_a_stutter_not_progress	nonprogress-cycle-fair-lasso	
src/interp.rs	a_wedge_short_of_the_goal_is_reported_as_a_stall	nonprogress-cycle-fair-lasso	
src/interp.rs	admits_is_a_real_conformance_check	admits-iff-some-action	
src/interp.rs	compiled_walks_are_the_by_name_walks_on_committed_models	compiled-walk-equals-by-name	
src/interp.rs	compiled_walks_are_the_by_name_walks_on_the_resolution_corners	compiled-walk-equals-by-name	
src/interp.rs	components_are_exactly_the_mutual_reachability_classes	scc-components	
src/interp.rs	fairness_excludes_starvation_only_cycles	nonprogress-cycle-fair-lasso	
src/interp.rs	fairness_naming_an_undeclared_action_panics	nonprogress-cycle-fair-lasso	
src/interp.rs	fired_actions_separates_committed_dead_mutants_from_buggy_live_ones	bmc-reach-semantics	
src/interp.rs	promoted_bmc_proves_and_catches	NON-SPEC (delete)	
src/interp.rs	strong_fairness_keeps_a_loop_that_never_passes_the_gate	nonprogress-cycle-fair-lasso	
src/interp.rs	the_compiled_form_raises_an_unbound_identifier_when_it_reads_it	compiled-walk-equals-by-name	
src/ir.rs	anchor_without_project_omits_project_clause	trust-ir-lowering	
src/ir.rs	embedded_block_emits_vars_actions_invariants_and_canonicalizes_machine	trust-ir-lowering	
src/ir.rs	fallback_anchor_identity_is_deduplicated_deterministically	trust-ir-lowering	
src/ir.rs	header_is_v1_canonical	trust-ir-lowering	
src/ir.rs	unresolved_or_bare_records_are_dropped	trust-ir-lowering	
src/registry.rs	a_node_submitted_twice_is_linked_once	registry-intrusive-list	
src/registry.rs	the_load_time_constructors_collected_both_submits	registry-intrusive-list	
src/rng.rs	bounded_draws_stay_in_bounds	xorshift64	
src/rng.rs	the_sequence_is_xorshift64_and_fixed_by_its_seed	xorshift64	
src/tla_check.rs	next_disjuncts_plain_and_quantified	tla-definitions-parse	
src/tla_check.rs	next_disjuncts_single_action	tla-definitions-parse	
src/tla_check.rs	test_deeply_nested_block_comments	tla-definitions-parse	
src/tla_check.rs	test_end_marker_not_parsed_as_definition	tla-definitions-parse	
src/tla_check.rs	test_nested_block_comments_parse_correctly	tla-definitions-parse	
src/tla_check.rs	test_no_module_returns_error	tla-definitions-parse	
src/tla_check.rs	test_parse_parametric_definition	tla-definitions-parse	
src/tla_check.rs	test_parse_simple_module	tla-definitions-parse	
src/tla_check.rs	test_parse_theorem_definitions	tla-definitions-parse	
src/tla_check.rs	test_single_line_block_comment	tla-definitions-parse	
src/tla_check.rs	test_skips_keywords	tla-definitions-parse	
src/verify_tests.rs	a_binary_identity_follows_a_wrapper_to_the_binary_it_execs	binary-identity-chain	
src/verify_tests.rs	a_case_with_no_verdict_line_is_never_an_accept	ty-cases-pool	
src/verify_tests.rs	a_clean_ty_verdict_over_a_violated_model_is_a_false_proof_not_a_coverage_gap	explored-space-guard	
src/verify_tests.rs	a_clean_verdict_without_a_state_count_credits_nothing	explored-space-guard	
src/verify_tests.rs	a_dead_cases_child_falls_back_per_process_and_respawns_once	ty-cases-pool	
src/verify_tests.rs	a_dead_set_from_a_reduced_run_is_refused_by_the_space_guard	explored-space-guard	
src/verify_tests.rs	a_model_without_invariants_has_no_ghosts_and_is_not_walked	one-walk-ghost-sweep	
src/verify_tests.rs	a_path_exec_stub_names_its_target	provenance-line-rendering	
src/verify_tests.rs	a_pending_stub_is_not_the_tool	tool-discovery-order	
src/verify_tests.rs	a_scalar_model_can_never_report_not_run	scalar-tier-always-runs	
src/verify_tests.rs	a_scenario_keeps_its_own_actions_alive	dead-negative-control-audit	
src/verify_tests.rs	a_shell_literal_word_is_read_only_when_nothing_expands	shell-literal-word	
src/verify_tests.rs	a_slow_probe_is_answered_and_a_wedged_one_is_killed_at_its_bound	ty-capabilities-cache-and-probe	
src/verify_tests.rs	a_slow_provenance_probe_holds_up_only_its_own_tool	provenance-report-per-tool-serial	
src/verify_tests.rs	a_stopped_search_is_never_a_verdict	ty-transcript-parsers	
src/verify_tests.rs	a_tools_last_line_names_what_runs_when_its_changes_race	provenance-report-per-tool-serial	
src/verify_tests.rs	a_ty_verdict_behind_a_wrapper_is_asked_again_when_only_its_target_changes	ty-verdict-memo	
src/verify_tests.rs	a_ty_verdict_from_a_larger_space_is_not_a_proof_either	explored-space-guard	
src/verify_tests.rs	a_ty_verdict_from_a_smaller_space_is_not_a_proof	explored-space-guard	
src/verify_tests.rs	a_ty_verdict_is_replayed_only_for_the_same_binary_and_files	ty-verdict-memo	
src/verify_tests.rs	an_evaluation_error_is_not_a_refutation	ty-cases-pool	
src/verify_tests.rs	an_older_ty_probes_as_no_capabilities	ty-capabilities-cache-and-probe	
src/verify_tests.rs	audit_accepts_config_catalog_snapshot_mutants	dead-negative-control-audit	
src/verify_tests.rs	audit_accepts_strictly_nonvacuous_model	dead-negative-control-audit	
src/verify_tests.rs	audit_accepts_transact_buggy_commit	dead-negative-control-audit	
src/verify_tests.rs	audit_credits_a_violation_step_and_not_a_step_after_one	dead-negative-control-audit	
src/verify_tests.rs	audit_rejects_dead_actions_without_committed_buggy_dial	dead-negative-control-audit	
src/verify_tests.rs	audit_rejects_never_firing_and_uncaught_mutants	dead-negative-control-audit	
src/verify_tests.rs	audit_rejects_two_mutants_when_only_one_is_independently_caught	dead-negative-control-audit	
src/verify_tests.rs	build_dates_are_utc_civil_dates	civil-date	
src/verify_tests.rs	dead_sets_must_agree_across_tiers	dead-negative-control-audit	
src/verify_tests.rs	discovery_order_is_store_then_path_and_no_developer_tree	tool-discovery-order	
src/verify_tests.rs	exec_stub_target_reads_atpkg_shims_and_rejects_refusals	tool-discovery-order	
src/verify_tests.rs	find_ty_and_its_verdicts_follow_a_store_update_mid_process	ty-cases-pool	
src/verify_tests.rs	provenance_is_claimed_again_only_when_the_resolution_changes	provenance-claim-state	
src/verify_tests.rs	provenance_is_reported_again_whenever_the_resolved_binary_changes	provenance-line-rendering	
src/verify_tests.rs	provenance_nothing_installed_is_a_plain_not_found	provenance-line-rendering	
src/verify_tests.rs	provenance_store_present_names_the_unreached_copy	provenance-line-rendering	
src/verify_tests.rs	store_bin_dir_mirrors_the_atpkg_default_prefix	NON-SPEC (becomes-spec-after-fix)	
src/verify_tests.rs	store_shim_resolution_is_fail_closed	tool-discovery-order	
src/verify_tests.rs	the_cases_pool_and_the_memo_answer_only_for_the_build_in_use	ty-cases-pool	
src/verify_tests.rs	the_cases_pool_answers_and_an_end_verdict_retires_its_child	ty-cases-pool	
src/verify_tests.rs	the_one_walk_ghost_sweep_matches_the_per_invariant_oracle	one-walk-ghost-sweep	
src/verify_tests.rs	the_pending_stub_check_reads_only_the_head	tool-discovery-order	
src/verify_tests.rs	the_provenance_line_names_the_build_behind_a_wrapper	provenance-line-rendering	
src/verify_tests.rs	the_store_is_the_accounts_whatever_home_says	tool-discovery-order	
src/verify_tests.rs	the_ty_evidence_header_is_invisible_to_the_transcript_parsers	ty-transcript-parsers	
src/verify_tests.rs	the_version_probe_returns_the_first_line	ty-capabilities-cache-and-probe	
src/verify_tests.rs	ty_capabilities_are_asked_again_for_another_build	ty-capabilities-cache-and-probe	
src/verify_tests.rs	ty_capabilities_are_the_json_list_and_an_older_ty_has_none	ty-capabilities-parse	
src/verify_tests.rs	ty_state_count_is_parsed_or_refused	ty-transcript-parsers	
src/wait.rs	first_exec_runs_a_file_once_in_a_scratch_environment	first-exec-once-scratch	
src/wait.rs	until_within_names_what_it_waited_for	wait-within	
src/wait.rs	within_answers_the_first_some_and_none_after_the_budget	wait-within	
src/xref.rs	a_move_inside_one_window_is_caught_exactly_when_the_new_site_is_wider	step-audit-definitions	
src/xref.rs	a_second_anchor_site_that_never_runs_is_named_rather_than_carried	step-audit-definitions	
src/xref.rs	a_window_that_misses_its_own_action_is_named_even_when_a_sibling_window_saw_it	step-audit-definitions	
src/xref.rs	a_workers_window_reaches_its_owner_only_by_take_and_merge	entered-anchor-window	
src/xref.rs	entered_under_fires_where_the_old_ubiquitous_intersection_could_not	step-audit-definitions	
src/xref.rs	external_action_names_is_next_only_invariant_names_is_full_set	tla-definitions-parse	
src/xref.rs	interchangeable_counts_what_a_window_leaves_ambiguous	step-audit-definitions	
tests/conformance_fabric_enable.rs	capability_publication_proves_and_catches_rejected_mint	NON-SPEC (delete)	
tests/conformance_fabric_enable.rs	tier_1_runs_the_real_command_through_this_model	NON-SPEC (delete)	
tests/derived_atpkg_contention_release.rs	a_contended_lane_retries_on_the_first_slice_after_release	model-laws	registered-model-sound, mutant-table-catch, law:atpkg-contention-release
tests/derived_atpkg_flip_quiet.rs	a_busy_toolchain_holds_its_flip_up_to_the_ceiling_and_a_revoked_one_never_waits	model-laws	registered-model-sound, mutant-table-catch, law:atpkg-flip-quiet
tests/derived_atpkg_full_pass.rs	the_full_pass_rule_proves_and_catches_the_session_lanes_misread	mutant-table-catch	registered-model-sound, model-laws, declared-dials-and-scenarios, law:atpkg-full-pass
tests/derived_atpkg_head_watch_hosts.rs	a_window_or_one_session_watches_never_both	model-laws	registered-model-sound, mutant-table-catch, law:atpkg-head-watch-hosts
tests/derived_atpkg_index_pending.rs	one_index_probe_can_overlap_a_vendor_hint_without_losing_ready_priority	model-laws	registered-model-sound, model-strict, mutant-table-catch, law:atpkg-index-pending
tests/derived_atpkg_index_probe.rs	a_suppressed_host_rechecks_when_the_shared_stamp_expires	model-laws	registered-model-sound, mutant-table-catch, law:atpkg-index-probe-family
tests/derived_atpkg_index_probe.rs	cross_process_probe_cooldown_proves_and_catches_duplicate_ranges	mutant-table-catch	registered-model-sound, model-laws, law:atpkg-index-probe-family
tests/derived_atpkg_index_probe.rs	failed_index_highwater_proves_no_older_replay	model-laws	registered-model-sound, mutant-table-catch, law:atpkg-index-probe-family
tests/derived_atpkg_index_probe.rs	independent_hints_prove_highest_and_no_false_missing	model-laws	registered-model-sound, mutant-table-catch, law:atpkg-index-probe-family
tests/derived_atpkg_index_probe.rs	the_next_probe_uses_the_shared_stamps_completion_clock	model-laws	registered-model-sound, mutant-table-catch, law:atpkg-index-probe-family
tests/derived_atpkg_index_publish.rs	the_index_channel_is_one_run_the_walk_lands_and_nothing_is_overwritten	model-laws	registered-model-sound, model-strict, mutant-table-catch, law:atpkg-index-publish
tests/derived_atpkg_pass_stamps.rs	the_pass_record_proves_and_catches_both_historical_defects	mutant-table-catch	registered-model-sound, model-laws, law:atpkg-pass-stamps
tests/derived_atpkg_pending_wait.rs	a_waiting_stub_reads_the_pass_before_the_shim	model-laws	registered-model-sound, mutant-table-catch, law:atpkg-pending-wait
tests/derived_atpkg_published_spacing.rs	a_newer_index_escapes_spacing_but_same_failed_index_and_unknowns_wait	model-laws	registered-model-sound, declared-dials-and-scenarios, mutant-table-catch, law:atpkg-published-spacing
tests/derived_atpkg_session_index.rs	a_near_index_answer_obeys_the_seat_window_and_verified_floor	model-laws	registered-model-sound, mutant-table-catch, law:atpkg-session-index
tests/derived_atpkg_session_index.rs	an_empty_local_look_does_not_delay_the_first_eligible_index_probe	model-laws	registered-model-sound, mutant-table-catch, law:atpkg-session-index
tests/derived_atpkg_session_index_retry.rs	same_build_retries_wait_longer_but_a_newer_build_bypasses_them	model-laws	registered-model-sound, mutant-table-catch, law:atpkg-session-index-retry
tests/derived_atpkg_vendor_pending.rs	independent_vendor_results_are_bounded_and_harvested_once	model-laws	registered-model-sound, mutant-table-catch, law:atpkg-vendor-pending
tests/derived_broadcast_checkpoint.rs	irrelevant_cursor_checkpoints_may_lag_but_delivered_records_may_not	model-laws	registered-model-sound, mutant-table-catch, law:broadcast-cursor
tests/derived_broadcast_head.rs	a_head_subscriber_never_takes_a_record_already_on_the_broker	model-laws	registered-model-sound, mutant-table-catch, law:broadcast-cursor
tests/derived_budgeted_search_resume_ty.rs	budgeted_search_resume_proves_and_catches_stale_continuation	NON-SPEC (delete)	
tests/derived_budgeted_search_resume_ty.rs	delivery_backlog_cannot_complete_or_stutter_early	model-laws	registered-model-sound, model-strict, mutant-table-catch, law:budgeted-search-resume
tests/derived_budgeted_search_resume_ty.rs	each_lifecycle_law_has_its_own_slip	mutant-table-catch	registered-model-sound, model-laws, model-strict, law:budgeted-search-resume
tests/derived_budgeted_search_resume_ty.rs	executable_model_walks_every_public_lifecycle_branch	model-laws	registered-model-sound, model-strict, mutant-table-catch, law:budgeted-search-resume
tests/derived_budgeted_search_resume_ty.rs	stale_resume_mutant_is_observably_rejected	model-laws	registered-model-sound, model-strict, mutant-table-catch, law:budgeted-search-resume
tests/derived_chrome_regrow.rs	a_fall_the_floor_holds_is_applied_and_the_restart_mutant_starves_it	liveness-obligations-sound	registered-model-sound, model-laws, model-strict, mutant-table-catch, law:chrome-regrow
tests/derived_chrome_regrow.rs	no_source_falls_inside_the_settle_of_another_sources_rise	model-laws	registered-model-sound, liveness-obligations-sound, model-strict, mutant-table-catch, law:chrome-regrow
tests/derived_console_pet_z_order.rs	pet_z_order_goes_under_at_once_and_surfaces_only_after_the_dwell	model-laws	registered-model-sound, model-strict, mutant-table-catch, law:console-pet-z-order
tests/derived_console_pet_z_order.rs	pet_z_order_proves_and_catches_every_mutant	model-strict	registered-model-sound, model-laws, mutant-table-catch, law:console-pet-z-order
tests/derived_control_bind_retry.rs	handoff_bind_retry_proves_and_catches_foreign_path_reuse	NON-SPEC (delete)	
tests/derived_control_preparation.rs	control_preparation_proves_and_catches_proof_before_workers	NON-SPEC (delete)	
tests/derived_control_socket_handoff.rs	fixed_socket_handoff_proves_and_catches_early_takeover	NON-SPEC (delete)	
tests/derived_crash_journal.rs	crash_journal_claim_proves_and_catches	model-strict	registered-model-sound, model-laws, mutant-table-catch, law:crash-journal
tests/derived_crash_journal.rs	crash_journal_claim_the_defects_are_caught_on_every_invariant	mutant-table-catch	registered-model-sound, model-laws, model-strict, law:crash-journal
tests/derived_crash_journal.rs	crash_journal_claim_walks_the_ends_the_way_a_person_meets_them	model-laws	registered-model-sound, model-strict, mutant-table-catch, law:crash-journal
tests/derived_debugger_access.rs	a_records_re_read_adopts_only_an_ended_change	model-laws	registered-model-sound, liveness-obligations-sound, mutant-table-catch, law:debugger-access
tests/derived_debugger_access.rs	an_exited_change_is_always_re_read_and_a_lost_wake_is_caught	model-laws	registered-model-sound, liveness-obligations-sound, mutant-table-catch, law:debugger-access
tests/derived_debugger_access.rs	the_change_runs_only_on_consent_over_a_fresh_read_one_child_at_a_time	model-laws	registered-model-sound, liveness-obligations-sound, mutant-table-catch, law:debugger-access
tests/derived_debugger_access.rs	the_race_between_the_re_read_and_the_tools_write_is_named	model-laws	registered-model-sound, liveness-obligations-sound, mutant-table-catch, law:debugger-access
tests/derived_desktop_alert_debt.rs	desktop_alert_debt_is_cancelled_without_forgetting_seen_escalations	model-laws	registered-model-sound, declared-dials-and-scenarios, mutant-table-catch, law:desktop-alert-debt
tests/derived_desktop_alert_debt.rs	each_mutant_exposes_its_own_failure_and_forgotten_state_replays	mutant-table-catch	registered-model-sound, model-laws, declared-dials-and-scenarios, law:desktop-alert-debt
tests/derived_desktop_alert_debt.rs	enable_replays_neither_cancelled_debt_nor_observations_made_while_disabled	model-laws	registered-model-sound, declared-dials-and-scenarios, mutant-table-catch, law:desktop-alert-debt
tests/derived_driver_geometry.rs	a_drivers_own_lease_never_moves_the_screen_its_fence_judged	model-laws	registered-model-sound, liveness-obligations-sound, model-strict, mutant-table-catch, law:driver-geometry
tests/derived_driver_geometry.rs	a_lease_that_moved_between_the_sense_and_the_commit_raises_no_row	model-laws	registered-model-sound, liveness-obligations-sound, model-strict, mutant-table-catch, law:driver-geometry
tests/derived_driver_geometry.rs	a_row_a_moved_lease_holds_catches_up_once_the_wake_is_handled	model-laws	registered-model-sound, liveness-obligations-sound, model-strict, mutant-table-catch, law:driver-geometry
tests/derived_driver_geometry.rs	a_row_a_peers_unseen_lease_held_is_caught_up_by_the_timer	model-laws	registered-model-sound, liveness-obligations-sound, model-strict, mutant-table-catch, law:driver-geometry
tests/derived_editor_carry.rs	editor_carry_proves_catches_and_has_no_dead_action	model-strict	registered-model-sound, model-laws, declared-dials-and-scenarios, mutant-table-catch, law:editor-carry
tests/derived_editor_carry.rs	editor_carry_the_defects_are_caught_on_every_invariant	mutant-table-catch	registered-model-sound, model-laws, model-strict, declared-dials-and-scenarios, law:editor-carry
tests/derived_editor_carry.rs	editor_carry_walks_the_update_the_way_the_person_meets_it	model-laws	registered-model-sound, model-strict, declared-dials-and-scenarios, mutant-table-catch, law:editor-carry
tests/derived_exceptions_digest.rs	a_later_never_wants_a_row_and_a_dismissal_holds	model-laws	registered-model-sound, liveness-obligations-sound, mutant-table-catch, law:exceptions-digest
tests/derived_exceptions_digest.rs	an_unnamed_entry_ends_by_its_own_clock_and_the_mutants_are_caught	liveness-obligations-sound	registered-model-sound, model-laws, mutant-table-catch, law:exceptions-digest
tests/derived_exceptions_once.rs	exceptions_once_every_defect_is_caught_on_its_own	model-laws	registered-model-sound, model-strict, declared-dials-and-scenarios, mutant-table-catch, law:exceptions-once
tests/derived_exceptions_once.rs	exceptions_once_proves_and_catches	model-strict	registered-model-sound, model-laws, declared-dials-and-scenarios, mutant-table-catch, law:exceptions-once
tests/derived_exceptions_once.rs	exceptions_once_walks_a_dismissal_made_during_the_update	model-laws	registered-model-sound, model-strict, declared-dials-and-scenarios, mutant-table-catch, law:exceptions-once
tests/derived_exceptions_once.rs	exceptions_once_walks_the_full_disk_access_history	model-laws	registered-model-sound, model-strict, declared-dials-and-scenarios, mutant-table-catch, law:exceptions-once
tests/derived_fabric_outbox_wake.rs	prompt_outbox_wake_and_lost_event_backstop_prove_and_catch	mutant-table-catch	registered-model-sound, model-laws, law:fabric-lanes
tests/derived_fabric_reconnect_backoff.rs	fabric_retry_wait_ignores_events_until_deadline_but_closes_on_aterm_loss	model-laws	registered-model-sound, mutant-table-catch, law:fabric-lanes
tests/derived_fabric_refill_retry.rs	failed_fetch_keeps_old_mail_owed_before_a_group_subscription	model-laws	registered-model-sound, mutant-table-catch, law:fabric-lanes
tests/derived_fd_lifecycle_ty.rs	derived_fd_lifecycle_proves_and_catches_use_after_close	NON-SPEC (delete)	
tests/derived_foreground_handback.rs	foreground_handback_a_one_shot_keeps_its_display_modes	model-laws	registered-model-sound, model-strict, declared-dials-and-scenarios, mutant-table-catch, law:foreground-handback
tests/derived_foreground_handback.rs	foreground_handback_a_restarted_reader_without_the_carried_holder_is_caught	model-laws	registered-model-sound, model-strict, declared-dials-and-scenarios, mutant-table-catch, law:foreground-handback
tests/derived_foreground_handback.rs	foreground_handback_a_stopped_job_keeps_its_modes_until_it_dies	model-laws	registered-model-sound, model-strict, declared-dials-and-scenarios, mutant-table-catch, law:foreground-handback
tests/derived_foreground_handback.rs	foreground_handback_at_every_foreground_change_is_caught	model-laws	registered-model-sound, model-strict, declared-dials-and-scenarios, mutant-table-catch, law:foreground-handback
tests/derived_foreground_handback.rs	foreground_handback_batch_end_only_sampling_is_caught	model-laws	registered-model-sound, model-strict, declared-dials-and-scenarios, mutant-table-catch, law:foreground-handback
tests/derived_foreground_handback.rs	foreground_handback_incident_and_adopted_schedules_committed_vs_buggy	model-laws	registered-model-sound, model-strict, declared-dials-and-scenarios, mutant-table-catch, law:foreground-handback
tests/derived_foreground_handback.rs	foreground_handback_ownership_a_missed_job_loses_only_its_own_handback	model-laws	registered-model-sound, model-strict, declared-dials-and-scenarios, mutant-table-catch, law:foreground-handback
tests/derived_foreground_handback.rs	foreground_handback_ownership_proves_catches_and_has_no_dead_action	model-strict	registered-model-sound, model-laws, declared-dials-and-scenarios, mutant-table-catch, law:foreground-handback
tests/derived_foreground_handback.rs	foreground_handback_proves_catches_and_has_no_dead_action	model-strict	registered-model-sound, model-laws, declared-dials-and-scenarios, mutant-table-catch, law:foreground-handback
tests/derived_foreground_handback.rs	foreground_handback_the_ui_sweep_design_is_caught_on_every_invariant	mutant-table-catch	registered-model-sound, model-laws, model-strict, declared-dials-and-scenarios, law:foreground-handback
tests/derived_handoff_address_owner.rs	the_predecessor_serves_its_own_candidate_and_refuses_a_stranger	model-laws	registered-model-sound, declared-dials-and-scenarios, mutant-table-catch, law:handoff-address-owner
tests/derived_harness_capture_worker.rs	capture_must_join_its_reader_before_return	model-laws	registered-model-sound, mutant-table-catch, law:harness-capture-and-daemon
tests/derived_harness_codex_daemon_update.rs	the_daemon_moves_only_between_turns_and_never_backwards	model-laws	registered-model-sound, mutant-table-catch, law:harness-capture-and-daemon
tests/derived_harness_host.rs	an_exit_is_handed_only_for_an_exit_and_the_upgrades_own_is_carried_on	model-laws	registered-model-sound, mutant-table-catch, law:harness-host
tests/derived_harness_host.rs	an_external_supervisor_keeps_its_session_across_the_handoff	model-laws	registered-model-sound, mutant-table-catch, law:harness-host
tests/derived_harness_host.rs	an_upgrade_is_never_let_go_for_a_look_that_could_not_read	model-laws	registered-model-sound, mutant-table-catch, law:harness-host
tests/derived_harness_host.rs	autopilot_off_stops_the_worker_and_lets_nothing_go	model-laws	registered-model-sound, mutant-table-catch, law:harness-host
tests/derived_harness_host.rs	one_supervisor_per_session_across_restarts_and_reloads	model-laws	registered-model-sound, mutant-table-catch, law:harness-host
tests/derived_harness_login_wall.rs	a_renewable_login_is_waited_at_asked_renewed_and_followed	model-laws	registered-model-sound, model-strict, declared-dials-and-scenarios, mutant-table-catch, law:harness-login-wall
tests/derived_harness_login_wall.rs	each_defect_is_caught_on_its_own	mutant-table-catch	registered-model-sound, model-laws, model-strict, declared-dials-and-scenarios, law:harness-login-wall
tests/derived_harness_login_wall.rs	the_incident_as_it_ran_is_the_caught_negative_control	model-laws	registered-model-sound, model-strict, declared-dials-and-scenarios, mutant-table-catch, law:harness-login-wall
tests/derived_harness_login_wall.rs	the_incident_now_is_waited_out_and_nothing_is_spent	model-laws	registered-model-sound, model-strict, declared-dials-and-scenarios, mutant-table-catch, law:harness-login-wall
tests/derived_harness_login_wall.rs	the_login_wall_model_proves_and_catches	model-strict	registered-model-sound, model-laws, declared-dials-and-scenarios, mutant-table-catch, law:harness-login-wall
tests/derived_harness_policy_one_writer.rs	harness_policy_one_writer_proves_and_catches_the_second_writer	NON-SPEC (delete)	
tests/derived_harness_policy_one_writer.rs	the_window_catches_up_with_the_file	NON-SPEC (delete)	
tests/derived_harness_relaunch.rs	a_codex_exit_is_decided_on_its_shells_word_alone	model-laws	registered-model-sound, mutant-table-catch, law:harness-relaunch
tests/derived_harness_relaunch.rs	a_crash_is_read_at_its_exit_and_kept_through_the_back_off	model-laws	registered-model-sound, mutant-table-catch, law:harness-relaunch
tests/derived_harness_relaunch.rs	a_person_wins_and_no_exit_is_dropped_silently	model-laws	registered-model-sound, mutant-table-catch, law:harness-relaunch
tests/derived_harness_restart.rs	the_restart_act_is_read_by_its_stamp_and_ends_its_program	model-laws	registered-model-sound, liveness-obligations-sound, mutant-table-catch, law:harness-restart-stamp
tests/derived_harness_restart.rs	the_restart_ends_its_program_under_fairness_and_term_only_does_not	liveness-obligations-sound	registered-model-sound, model-laws, mutant-table-catch, law:harness-restart-stamp
tests/derived_harness_restore_queue.rs	a_relaunch_owed_through_a_failed_commit_is_never_lost	model-laws	registered-model-sound, mutant-table-catch, law:harness-restored-carry
tests/derived_harness_restore_queue.rs	harness_work_in_flight_at_a_commit_is_carried_never_dropped	model-laws	registered-model-sound, mutant-table-catch, law:harness-restored-carry
tests/derived_harness_restore_queue.rs	network_probe_shutdown_cannot_notify_before_the_waiter_parks	NON-SPEC (delete)	
tests/derived_harness_restore_queue.rs	restored_tabs_get_first_attempts_before_a_retry	NON-SPEC (delete)	
tests/derived_harness_state_collection.rs	a_closed_tabs_settled_record_goes_and_the_historical_lane_kept_it_for_ever	model-laws	registered-model-sound, liveness-obligations-sound, mutant-table-catch, law:harness-state-collection
tests/derived_harness_state_collection.rs	only_a_closed_tabs_settled_week_old_record_is_pruned	model-laws	registered-model-sound, liveness-obligations-sound, mutant-table-catch, law:harness-state-collection
tests/derived_harness_upgrade_age.rs	an_age_is_never_carried_onto_another_process	model-laws	registered-model-sound, declared-dials-and-scenarios, mutant-table-catch, law:harness-upgrade-age-drain
tests/derived_harness_upgrade_drain_bound.rs	the_drain_never_ends_the_agent_on_an_answer_a_person_held_past_its_bound	model-laws	registered-model-sound, declared-dials-and-scenarios, mutant-table-catch, law:harness-upgrade-age-drain
tests/derived_harness_upgrade_goal_pause.rs	the_goal_pause_is_resumed_once_and_never_cuts_a_tool_off	model-laws	registered-model-sound, liveness-obligations-sound, model-strict, mutant-table-catch, law:harness-upgrade-goal-pause
tests/derived_harness_upgrade_goal_pause.rs	the_upgrade_lands_under_a_goal_and_leaves_it_pursued	model-laws	registered-model-sound, liveness-obligations-sound, model-strict, mutant-table-catch, law:harness-upgrade-goal-pause
tests/derived_harness_upgrade_ladder.rs	each_livelock_is_the_cycle_its_mutant_names	liveness-obligations-sound	registered-model-sound, model-laws, model-strict, mutant-table-catch, law:harness-upgrade-ladder
tests/derived_harness_upgrade_ladder.rs	the_ladder_keeps_every_floor_and_counts_a_quiet_looking_pause	model-laws	registered-model-sound, liveness-obligations-sound, model-strict, mutant-table-catch, law:harness-upgrade-ladder
tests/derived_harness_upgrade_ladder.rs	the_upgrade_lands_under_fairness_and_catches_the_livelocks	NON-SPEC (delete)	
tests/derived_harness_upgrade_limit_queue.rs	a_limit_that_lasts_for_days_adds_a_handful_of_copies_then_one_a_day	model-laws	registered-model-sound, model-strict, declared-dials-and-scenarios, mutant-table-catch, law:harness-upgrade-limit-queue
tests/derived_harness_upgrade_limit_queue.rs	each_defect_is_caught_on_its_own	mutant-table-catch	registered-model-sound, model-laws, model-strict, declared-dials-and-scenarios, law:harness-upgrade-limit-queue
tests/derived_harness_upgrade_limit_queue.rs	once_the_limit_ends_the_upgrade_asks_again_by_itself	model-laws	registered-model-sound, model-strict, declared-dials-and-scenarios, mutant-table-catch, law:harness-upgrade-limit-queue
tests/derived_harness_upgrade_limit_queue.rs	the_limit_queue_model_proves_and_catches	model-strict	registered-model-sound, model-laws, declared-dials-and-scenarios, mutant-table-catch, law:harness-upgrade-limit-queue
tests/derived_harness_upgrade_limit_queue.rs	the_owners_report_as_it_ran_is_the_caught_negative_control	model-laws	registered-model-sound, model-strict, declared-dials-and-scenarios, mutant-table-catch, law:harness-upgrade-limit-queue
tests/derived_harness_upgrade_limit_queue.rs	the_quiet_word_is_derived_from_the_upgrades_guards	model-laws	registered-model-sound, model-strict, declared-dials-and-scenarios, mutant-table-catch, law:harness-upgrade-limit-queue
tests/derived_harness_upgrade_limit_queue.rs	the_report_now_waits_types_again_rests_and_types_once_more	model-laws	registered-model-sound, model-strict, declared-dials-and-scenarios, mutant-table-catch, law:harness-upgrade-limit-queue
tests/derived_harness_upgrade_never_strands.rs	a_break_that_never_ends_still_releases_the_agent_it_asked	model-laws	registered-model-sound, model-strict, declared-dials-and-scenarios, mutant-table-catch, law:harness-upgrade-never-strands
tests/derived_harness_upgrade_never_strands.rs	a_build_that_fails_to_start_strands_no_agent_it_holds	model-laws	registered-model-sound, model-strict, declared-dials-and-scenarios, mutant-table-catch, law:harness-upgrade-never-strands
tests/derived_harness_upgrade_never_strands.rs	a_limited_session_is_asked_after_its_reset_and_a_tired_one_is_released	model-laws	registered-model-sound, model-strict, declared-dials-and-scenarios, mutant-table-catch, law:harness-upgrade-never-strands
tests/derived_harness_upgrade_never_strands.rs	a_release_is_dropped_only_for_an_agent_that_took_other_work_up	model-laws	registered-model-sound, model-strict, declared-dials-and-scenarios, mutant-table-catch, law:harness-upgrade-never-strands
tests/derived_harness_upgrade_never_strands.rs	each_defect_is_caught_on_its_own	mutant-table-catch	registered-model-sound, model-laws, model-strict, declared-dials-and-scenarios, law:harness-upgrade-never-strands
tests/derived_harness_upgrade_never_strands.rs	no_reachable_state_is_a_permanent_wait	model-laws	registered-model-sound, model-strict, declared-dials-and-scenarios, mutant-table-catch, law:harness-upgrade-never-strands
tests/derived_harness_upgrade_never_strands.rs	the_incident_as_it_ran_is_the_caught_negative_control	model-laws	registered-model-sound, model-strict, declared-dials-and-scenarios, mutant-table-catch, law:harness-upgrade-never-strands
tests/derived_harness_upgrade_never_strands.rs	the_last_word_is_derived_from_the_reducers_guards	model-laws	registered-model-sound, model-strict, declared-dials-and-scenarios, mutant-table-catch, law:harness-upgrade-never-strands
tests/derived_harness_upgrade_never_strands.rs	the_owners_stall_rearms_and_the_terminal_reducer_is_caught	model-laws	registered-model-sound, model-strict, declared-dials-and-scenarios, mutant-table-catch, law:harness-upgrade-never-strands
tests/derived_harness_upgrade_never_strands.rs	the_upgrade_never_strands_the_agent_it_asked	model-laws	registered-model-sound, model-strict, declared-dials-and-scenarios, mutant-table-catch, law:harness-upgrade-never-strands
tests/derived_harness_upgrade_notice_owner.rs	a_notice_whose_process_is_gone_is_asked_afresh_in_a_new_round	model-laws	registered-model-sound, model-strict, declared-dials-and-scenarios, mutant-table-catch, law:harness-upgrade-notice-owner
tests/derived_harness_upgrade_notice_owner.rs	each_notice_owner_defect_is_caught_on_its_own	model-laws	registered-model-sound, model-strict, declared-dials-and-scenarios, mutant-table-catch, law:harness-upgrade-notice-owner
tests/derived_harness_upgrade_notice_owner.rs	ready_from_tab_a_never_authorizes_tab_b_or_duplicate_owners	model-laws	registered-model-sound, model-strict, declared-dials-and-scenarios, mutant-table-catch, law:harness-upgrade-notice-owner
tests/derived_harness_upgrade_notice_owner.rs	the_notice_owners_wait_is_derived_from_its_guards	model-laws	registered-model-sound, model-strict, declared-dials-and-scenarios, mutant-table-catch, law:harness-upgrade-notice-owner
tests/derived_harness_upgrade_progress.rs	derived_harness_upgrade_progress_break_is_a_redecision	model-laws	registered-model-sound, liveness-obligations-sound, model-strict, mutant-table-catch, law:harness-upgrade-progress
tests/derived_harness_upgrade_progress.rs	derived_harness_upgrade_progress_gaps_are_closed_on_main	model-laws	registered-model-sound, liveness-obligations-sound, model-strict, mutant-table-catch, law:harness-upgrade-progress
tests/derived_harness_upgrade_progress.rs	each_livelock_is_the_cycle_its_mutant_names	liveness-obligations-sound	registered-model-sound, model-laws, model-strict, mutant-table-catch, law:harness-upgrade-progress
tests/derived_harness_upgrade_progress.rs	the_upgrade_moves_or_its_record_names_what_holds_it_under_fairness	NON-SPEC (delete)	
tests/derived_harness_upgrade_progress.rs	the_upgrade_progress_keeps_every_safety_law	model-laws	registered-model-sound, liveness-obligations-sound, model-strict, mutant-table-catch, law:harness-upgrade-progress
tests/derived_harness_usage_tap.rs	each_defect_is_caught_on_its_own	mutant-table-catch	registered-model-sound, model-laws, model-strict, declared-dials-and-scenarios, law:harness-usage-tap
tests/derived_harness_usage_tap.rs	the_always_fresh_read_records_an_idle_repeat	model-laws	registered-model-sound, model-strict, declared-dials-and-scenarios, mutant-table-catch, law:harness-usage-tap
tests/derived_harness_usage_tap.rs	the_usage_tap_model_proves_and_catches	model-strict	registered-model-sound, model-laws, declared-dials-and-scenarios, mutant-table-catch, law:harness-usage-tap
tests/derived_input_unread.rs	a_slow_program_is_written_to_with_a_warning_and_never_published	model-laws	registered-model-sound, mutant-table-catch, law:input-unread
tests/derived_input_unread.rs	a_stopped_job_refuses_typeahead_passes_and_a_read_lifts_the_stall	model-laws	registered-model-sound, mutant-table-catch, law:input-unread
tests/derived_input_unread.rs	input_unread_gate_proves_and_catches_every_invariant	NON-SPEC (delete)	
tests/derived_input_unread.rs	the_incident_is_refused_once_published_and_admitted_by_the_screen_fence	model-laws	registered-model-sound, mutant-table-catch, law:input-unread
tests/derived_keeper.rs	keeper_death_judgement_proves_and_catches	model-strict	registered-model-sound, model-laws, mutant-table-catch, law:keeper-brake-and-death
tests/derived_keeper.rs	keeper_death_judgement_the_defects_are_caught_on_every_invariant	mutant-table-catch	registered-model-sound, model-laws, model-strict, law:keeper-brake-and-death
tests/derived_keeper.rs	keeper_death_judgement_walks_the_ends	model-laws	registered-model-sound, model-strict, mutant-table-catch, law:keeper-brake-and-death
tests/derived_keeper.rs	keeper_relaunch_brake_proves_and_catches	model-strict	registered-model-sound, model-laws, mutant-table-catch, law:keeper-brake-and-death
tests/derived_keeper.rs	keeper_relaunch_brake_the_defects_are_caught_on_every_invariant	mutant-table-catch	registered-model-sound, model-laws, model-strict, law:keeper-brake-and-death
tests/derived_keeper.rs	keeper_relaunch_brake_walks_the_lifecycles	model-laws	registered-model-sound, model-strict, mutant-table-catch, law:keeper-brake-and-death
tests/derived_keeper.rs	pty_keeper_custody_a_release_from_one_of_two_claimants_keeps_the_copy	model-laws	registered-model-sound, model-strict, mutant-table-catch, law:keeper-custody
tests/derived_keeper.rs	pty_keeper_custody_an_offer_returns_when_its_link_drops	model-laws	registered-model-sound, model-strict, mutant-table-catch, law:keeper-custody
tests/derived_keeper.rs	pty_keeper_custody_proves_and_catches	model-strict	registered-model-sound, model-laws, mutant-table-catch, law:keeper-custody
tests/derived_keeper.rs	pty_keeper_custody_the_defects_are_caught_on_every_invariant	mutant-table-catch	registered-model-sound, model-laws, model-strict, law:keeper-custody
tests/derived_keeper.rs	pty_keeper_custody_walks_the_lifecycles	model-laws	registered-model-sound, model-strict, mutant-table-catch, law:keeper-custody
tests/derived_keeper.rs	the_keeper_spaces_fit_the_interpreter	NON-SPEC (delete)	
tests/derived_launched_dial_wait.rs	a_dead_successor_ends_the_wait_and_a_live_one_is_accepted	model-laws	registered-model-sound, mutant-table-catch, law:launched-dial-wait
tests/derived_launched_dial_wait.rs	a_dead_successor_ends_the_wait_without_the_dial_budget	NON-SPEC (delete)	
tests/derived_launched_dial_wait.rs	each_mutant_alone_breaks_the_invariant_it_was_written_for	mutant-table-catch	registered-model-sound, model-laws, law:launched-dial-wait
tests/derived_launched_dial_wait.rs	launched_dial_wait_proves_and_catches	NON-SPEC (delete)	
tests/derived_main_stall_refusal.rs	a_dialog_is_never_read_as_a_stall	model-laws	registered-model-sound, mutant-table-catch, law:main-stall-refusal
tests/derived_main_stall_refusal.rs	a_hop_whose_reply_is_queued_stops_counting_when_it_is_taken	model-laws	registered-model-sound, mutant-table-catch, law:main-stall-refusal
tests/derived_main_stall_refusal.rs	a_reset_under_a_stall_and_a_hop_after_idleness_are_caught	model-laws	registered-model-sound, mutant-table-catch, law:main-stall-refusal
tests/derived_main_stall_refusal.rs	main_stall_refusal_proves_and_catches_every_defect	NON-SPEC (delete)	
tests/derived_metal_drawable_acquire_ty.rs	each_historical_failure_has_an_independent_derived_counterexample	mutant-table-catch	registered-model-sound, model-laws, law:render-small-lanes
tests/derived_metal_drawable_acquire_ty.rs	metal_drawable_acquire_proves_bounds_and_catches_blocking_stale_and_retirement_mutants	NON-SPEC (delete)	
tests/derived_native_leaf_cache.rs	native_leaf_cache_retires_hidden_rasters_on_every_route	model-laws	registered-model-sound, mutant-table-catch, law:render-small-lanes
tests/derived_net_slot_displacement.rs	a_newcomer_waits_for_the_slot_it_freed_and_never_takes_a_relays	model-laws	registered-model-sound, mutant-table-catch, law:render-small-lanes
tests/derived_net_slot_displacement.rs	each_mutant_breaks_its_own_invariant	mutant-table-catch	registered-model-sound, model-laws, law:render-small-lanes
tests/derived_net_slot_displacement.rs	net_slot_displacement_proves_and_catches_the_cut_relay	mutant-table-catch	registered-model-sound, model-laws, law:render-small-lanes
tests/derived_output_echo_ty.rs	an_echo_the_sink_has_not_accepted_is_never_published	model-laws	registered-model-sound, model-strict, mutant-table-catch, law:output-echo
tests/derived_output_echo_ty.rs	delayed_older_completion_cannot_replace_the_newer_boundary	model-laws	registered-model-sound, model-strict, mutant-table-catch, law:output-echo
tests/derived_output_echo_ty.rs	output_echo_receipt_publication_proves_and_catches	model-strict	registered-model-sound, model-laws, mutant-table-catch, law:output-echo
tests/derived_output_echo_ty.rs	sample_cannot_mix_the_boundary_order_with_older_echo_timestamps	model-laws	registered-model-sound, model-strict, mutant-table-catch, law:output-echo
tests/derived_output_retention.rs	tiny_output_is_bounded_and_every_eviction_is_disclosed	NON-SPEC (delete)	
tests/derived_paste_order_ty.rs	a_paste_in_a_cannot_order_a_key_in_b	model-laws	registered-model-sound, mutant-table-catch, law:paste-order
tests/derived_paste_order_ty.rs	a_spill_receipt_cannot_grant_queued_key_present_priority	model-laws	registered-model-sound, mutant-table-catch, law:paste-order
tests/derived_paste_order_ty.rs	ordered_input_retention_is_bounded_with_room_for_keys	NON-SPEC (delete)	
tests/derived_pet_observation_admission.rs	pet_observation_admission_preserves_unfed_motion_and_recovers_existing_flight	model-laws	registered-model-sound, model-strict, declared-dials-and-scenarios, mutant-table-catch, law:pet-observation-and-seam
tests/derived_pet_observation_admission.rs	pet_observation_admission_proves_and_catches_incoherent_motion	NON-SPEC (delete)	
tests/derived_pet_observation_admission.rs	pet_observation_admission_rejects_launch_and_advance_as_separate_mutants	mutant-table-catch	registered-model-sound, model-laws, model-strict, declared-dials-and-scenarios, law:pet-observation-and-seam
tests/derived_pet_surface_seam.rs	a_breed_leaver_crosses_a_boundary_and_leaves_at_full_presence	model-laws	registered-model-sound, model-strict, declared-dials-and-scenarios, mutant-table-catch, law:pet-observation-and-seam
tests/derived_pet_surface_seam.rs	a_quick_return_continues_one_body_and_every_ghost_leaves_at_full_presence	model-laws	registered-model-sound, model-strict, declared-dials-and-scenarios, mutant-table-catch, law:pet-observation-and-seam
tests/derived_pet_surface_seam.rs	a_regrid_keeps_the_body_and_a_replacement_crossfades_in_place	model-laws	registered-model-sound, model-strict, declared-dials-and-scenarios, mutant-table-catch, law:pet-observation-and-seam
tests/derived_pet_surface_seam.rs	pet_surface_seam_proves_and_catches_every_mutant	model-strict	registered-model-sound, model-laws, declared-dials-and-scenarios, mutant-table-catch, law:pet-observation-and-seam
tests/derived_precommit_input.rs	precommit_input_proves_and_catches	mutant-table-catch	registered-model-sound, model-laws, law:precommit-input
tests/derived_precommit_input.rs	precommit_input_the_defects_are_caught_on_every_invariant	mutant-table-catch	registered-model-sound, model-laws, law:precommit-input
tests/derived_precommit_input.rs	precommit_input_walks_the_handoff_the_way_the_owner_meets_it	model-laws	registered-model-sound, mutant-table-catch, law:precommit-input
tests/derived_relaunch_prompt_read.rs	relaunch_prompt_read_never_types_on_the_stamp_alone	model-laws	registered-model-sound, liveness-obligations-sound, mutant-table-catch, law:relaunch-prompt-read
tests/derived_relaunch_prompt_read.rs	relaunch_prompt_read_never_types_onto_wrapped_typing	model-laws	registered-model-sound, liveness-obligations-sound, mutant-table-catch, law:relaunch-prompt-read
tests/derived_relaunch_prompt_read.rs	relaunch_prompt_read_proves_and_catches_the_invariant_and_the_liveness	liveness-obligations-sound	registered-model-sound, model-laws, mutant-table-catch, law:relaunch-prompt-read
tests/derived_relaunch_prompt_read.rs	relaunch_prompt_read_types_at_tab_ones_prompt_and_tab_ones_rule_never_does	model-laws	registered-model-sound, liveness-obligations-sound, mutant-table-catch, law:relaunch-prompt-read
tests/derived_release_publish_once.rs	publish_once_guard_conjuncts_carry_the_order_they_claim	guard-conjunct-audit	registered-model-sound, model-laws, model-strict, mutant-table-catch, law:release-publish-once
tests/derived_release_publish_once.rs	publish_once_proves_and_catches_every_defect_class	model-strict	registered-model-sound, model-laws, mutant-table-catch, law:release-publish-once
tests/derived_render.rs	a_displaced_net_zero_run_is_still_a_risk_until_repainted	model-laws	registered-model-sound, declared-dials-and-scenarios, mutant-table-catch, law:resize-render
tests/derived_render.rs	a_flap_a_spinner_tick_splits_is_judged_and_the_draw_closing_judge_is_caught	model-laws	registered-model-sound, declared-dials-and-scenarios, mutant-table-catch, law:resize-render
tests/derived_render.rs	a_flap_an_app_frame_splits_is_judged_as_one_epoch	model-laws	registered-model-sound, declared-dials-and-scenarios, mutant-table-catch, law:resize-render
tests/derived_render.rs	a_net_changed_run_is_never_a_risk_and_never_ok_once_drawn_on	model-laws	registered-model-sound, declared-dials-and-scenarios, mutant-table-catch, law:resize-render
tests/derived_render.rs	a_net_zero_risk_is_never_downgraded_to_unverified	model-laws	registered-model-sound, declared-dials-and-scenarios, mutant-table-catch, law:resize-render
tests/derived_render.rs	a_quiet_flap_is_an_identity_and_the_historical_engine_is_caught	model-laws	registered-model-sound, declared-dials-and-scenarios, mutant-table-catch, law:resize-render
tests/derived_render.rs	a_quiet_flap_owes_nothing_and_an_unknown_program_is_never_accused	model-laws	registered-model-sound, declared-dials-and-scenarios, mutant-table-catch, law:resize-render
tests/derived_render.rs	a_restored_flap_split_by_time_is_ok_and_the_time_closing_judge_is_caught	model-laws	registered-model-sound, declared-dials-and-scenarios, mutant-table-catch, law:resize-render
tests/derived_render.rs	a_tick_on_a_changed_size_is_unverified_where_the_assumed_repaint_said_ok	model-laws	registered-model-sound, declared-dials-and-scenarios, mutant-table-catch, law:resize-render
tests/derived_render.rs	a_trim_is_no_risk_and_a_changed_size_repaint_heals	model-laws	registered-model-sound, declared-dials-and-scenarios, mutant-table-catch, law:resize-render
tests/derived_render.rs	an_earlier_restored_flap_excuses_no_later_one	model-laws	registered-model-sound, declared-dials-and-scenarios, mutant-table-catch, law:resize-render
tests/derived_render.rs	an_undrawn_residual_is_judged_after_the_risk_wait	model-laws	registered-model-sound, declared-dials-and-scenarios, mutant-table-catch, law:resize-render
tests/derived_render.rs	each_named_mutant_breaks_exactly_its_claims_the_epoch_judge_s	mutant-table-catch	registered-model-sound, model-laws, declared-dials-and-scenarios, law:resize-render
tests/derived_render.rs	each_named_mutant_breaks_exactly_its_claims_the_risk_s	mutant-table-catch	registered-model-sound, model-laws, declared-dials-and-scenarios, law:resize-render
tests/derived_render.rs	each_named_mutant_breaks_exactly_its_claims_the_verdict_s	mutant-table-catch	registered-model-sound, model-laws, declared-dials-and-scenarios, law:resize-render
tests/derived_render.rs	the_render_invariants_are_each_caught	mutant-table-catch	registered-model-sound, model-laws, declared-dials-and-scenarios, law:resize-render
tests/derived_rendezvous_admission.rs	an_unanswered_check_holds_until_it_is_answered_or_the_deadline	model-laws	registered-model-sound, mutant-table-catch, law:rendezvous-admission
tests/derived_rendezvous_admission.rs	each_mutant_breaks_its_own_invariant	mutant-table-catch	registered-model-sound, model-laws, law:rendezvous-admission
tests/derived_rendezvous_admission.rs	rendezvous_admission_proves_and_catches_the_secret_only_gate	NON-SPEC (delete)	
tests/derived_rendezvous_claim.rs	rendezvous_claim_proves_and_catches_the_uid_only_gate	NON-SPEC (delete)	
tests/derived_rendezvous_grant.rs	rendezvous_grant_proves_and_catches_a_chunked_grant_an_old_successor_cannot_take	NON-SPEC (delete)	
tests/derived_repaint_nudge.rs	the_repaint_nudge_never_strands_a_tab_and_always_ends	liveness-obligations-sound	registered-model-sound, model-laws, mutant-table-catch, law:repaint-nudge
tests/derived_resize_row_reuse.rs	historical_discard_and_independent_bypass_are_both_caught	mutant-table-catch	registered-model-sound, model-laws, declared-dials-and-scenarios, law:resize-row-reuse
tests/derived_resize_row_reuse.rs	resize_slices_are_conserved_and_reused_before_allocating	model-laws	registered-model-sound, declared-dials-and-scenarios, mutant-table-catch, law:resize-row-reuse
tests/derived_ribbon_follow.rs	a_copy_that_lands_beside_the_line_and_stands_forty_ms_is_no_arrival	model-laws	registered-model-sound, mutant-table-catch, law:ribbon
tests/derived_ribbon_follow.rs	a_twin_inside_an_arrived_block_follows_and_the_veto_melts_it	model-laws	registered-model-sound, mutant-table-catch, law:ribbon
tests/derived_ribbon_follow.rs	ribbon_arrival_evidence_proves_and_each_old_writer_is_caught_by_its_own_law	mutant-table-catch	registered-model-sound, model-laws, law:ribbon
tests/derived_ribbon_follow.rs	ribbon_follow_twin_proves_and_catches_the_twin_veto	NON-SPEC (delete)	
tests/derived_ribbon_release_restoration.rs	failed_observations_retain_custody_until_a_complete_return	model-laws	registered-model-sound, mutant-table-catch, law:ribbon
tests/derived_ribbon_release_restoration.rs	independent_abandonment_and_identity_changes_revoke_restoration	model-laws	registered-model-sound, mutant-table-catch, law:ribbon
tests/derived_ribbon_release_restoration.rs	ribbon_release_restoration_proves_and_catches_partial_revival	NON-SPEC (delete)	
tests/derived_ribbon_row_hold.rs	ribbon_row_hold_permits_returning_to_the_earlier_row	model-laws	registered-model-sound, mutant-table-catch, law:ribbon
tests/derived_ribbon_row_hold.rs	ribbon_row_hold_proves_and_catches_global_renewal	NON-SPEC (delete)	
tests/derived_ring_ty.rs	derived_aa_edge_hardening_proves_and_catches_soft_seam	NON-SPEC (delete)	
tests/derived_ring_ty.rs	derived_active_handle_proves_and_catches_stale_handle	NON-SPEC (delete)	
tests/derived_ring_ty.rs	derived_alt_archive_pool_proves_catches_and_multiplies	prove-catch-multiply	registered-model-sound
tests/derived_ring_ty.rs	derived_alt_selection_park_never_leaves_a_selection_parked_off_alt	model-laws	
tests/derived_ring_ty.rs	derived_anchored_artifact_transaction_proves_handle_and_reply_identity	model-claim-breaks-first	registered-model-sound
tests/derived_ring_ty.rs	derived_artifact_handoff_capacity_refuses_overbooking	model-claim-breaks-first	model-strict, registered-model-sound
tests/derived_ring_ty.rs	derived_artifact_reader_lease_sweeps_only_after_final_release	model-claim-breaks-first	model-laws, model-strict, registered-model-sound
tests/derived_ring_ty.rs	derived_artifact_reply_publication_requires_ack_or_quarantine_expiry	model-claim-breaks-first	model-laws, model-strict, registered-model-sound
tests/derived_ring_ty.rs	derived_asymmetric_pad_layout_proves_cover_bounds_and_cache_invalidation	model-laws	registered-model-sound
tests/derived_ring_ty.rs	derived_capture_after_present_proves_and_catches_stale_pixels_and_late_retry	model-strict	registered-model-sound
tests/derived_ring_ty.rs	derived_channel_bind_proves_and_catches_replay	NON-SPEC (delete)	
tests/derived_ring_ty.rs	derived_chrome_face_gate_proves_and_catches_dejavu_hardcode	NON-SPEC (delete)	
tests/derived_ring_ty.rs	derived_claude_footer_model_names_the_running_model_at_once	model-claim-breaks-first	model-laws
tests/derived_ring_ty.rs	derived_claude_footer_watch_retires_and_keeps_a_replacement	model-laws	
tests/derived_ring_ty.rs	derived_claude_is_idle_only_at_its_composer	model-laws	
tests/derived_ring_ty.rs	derived_claude_mode_return_proves_and_catches_overshoot_box_and_silent_stop	model-claim-breaks-first	model-laws, registered-model-sound
tests/derived_ring_ty.rs	derived_clipboard_mailbox_proves_one_latest_pending_write	model-laws	registered-model-sound
tests/derived_ring_ty.rs	derived_closed_recovery_ledgers_prove_and_catch_double_record_or_loss	registered-model-sound	
tests/derived_ring_ty.rs	derived_coalesce_proves_and_catches_lane_divergence	NON-SPEC (delete)	
tests/derived_ring_ty.rs	derived_companion_tenure_flicker_proves_and_catches_a_flapping_cat	model-laws	registered-model-sound
tests/derived_ring_ty.rs	derived_composed_sync_hold_requires_every_pane_to_release	registered-model-sound	
tests/derived_ring_ty.rs	derived_composed_witness_generation_preserves_captured_rows_and_fences_fresh_reads	model-laws	
tests/derived_ring_ty.rs	derived_composite_accessibility_route_proves_and_catches_wrong_or_stale_owner	registered-model-sound	
tests/derived_ring_ty.rs	derived_config_catalog_snapshot_proves_and_catches_split_generation	registered-model-sound	
tests/derived_ring_ty.rs	derived_config_file_commit_cas_proves_and_catches_dual_lane_loss	registered-model-sound	
tests/derived_ring_ty.rs	derived_console_life_episodes_prove_and_catch_replay_and_stale_ownership	NON-SPEC (delete)	
tests/derived_ring_ty.rs	derived_console_resident_handoff_proves_and_catches_stranded_or_perpetual_wakes	NON-SPEC (delete)	
tests/derived_ring_ty.rs	derived_contrast_floor_proves_and_catches_weak_pole	NON-SPEC (delete)	
tests/derived_ring_ty.rs	derived_control_connection_admission_proves_and_catches_overflow	registered-model-sound	
tests/derived_ring_ty.rs	derived_control_lane_tenure_proves_and_catches_the_held_lane	registered-model-sound	
tests/derived_ring_ty.rs	derived_ct_frac_bearing_proves_and_catches_rounded_pin	NON-SPEC (delete)	
tests/derived_ring_ty.rs	derived_cursor_cat_curse_wince_rejects_fuc_and_catches_preview_mutant	model-laws	registered-model-sound
tests/derived_ring_ty.rs	derived_cursor_cat_earn_floor_proves_and_catches_v056_threshold	NON-SPEC (delete)	
tests/derived_ring_ty.rs	derived_cursor_cat_fold_proves_and_catches_direct_edge_teleport	model-laws	registered-model-sound
tests/derived_ring_ty.rs	derived_cursor_cat_long_gap_settles_directly_and_preserves_hidden_pause	model-laws	
tests/derived_ring_ty.rs	derived_cursor_cat_motion_pulse_routing_proves_and_catches_composed_strand	model-laws	registered-model-sound
tests/derived_ring_ty.rs	derived_cursor_cat_proves_and_catches_hidden_expiry	NON-SPEC (delete)	
tests/derived_ring_ty.rs	derived_cursor_cat_trail_master_blocks_ordinary_but_not_hello	model-laws	
tests/derived_ring_ty.rs	derived_cursor_companion_owner_lifecycle_proves_and_catches_stale_coordinates	registered-model-sound	
tests/derived_ring_ty.rs	derived_cursor_cutout_clip_proves_and_catches_unclipped_bleed	NON-SPEC (delete)	
tests/derived_ring_ty.rs	derived_cursor_effect_scroll_proves_and_catches_stranded_geometry	NON-SPEC (delete)	
tests/derived_ring_ty.rs	derived_cursor_hint_license_proves_and_catches_cold_light	mutant-table-catch	model-laws, registered-model-sound
tests/derived_ring_ty.rs	derived_cursor_proves_and_catches_overshoot	NON-SPEC (delete)	
tests/derived_ring_ty.rs	derived_cursor_scroll_signal_proves_and_catches_capped_history_stranding	NON-SPEC (delete)	
tests/derived_ring_ty.rs	derived_cursor_viewport_lifecycle_proves_and_catches_history_overlays	registered-model-sound	
tests/derived_ring_ty.rs	derived_damage_to_present_proves_and_catches_starvation	model-laws	registered-model-sound
tests/derived_ring_ty.rs	derived_deco_band_containment_proves_and_catches_spill	NON-SPEC (delete)	
tests/derived_ring_ty.rs	derived_deco_phase_proves_and_catches_seam_reset	NON-SPEC (delete)	
tests/derived_ring_ty.rs	derived_done_mark_lru_proves_and_catches_full_map_selection	NON-SPEC (delete)	
tests/derived_ring_ty.rs	derived_dsu_quiescence_proves_and_catches_midflight_tear	NON-SPEC (delete)	
tests/derived_ring_ty.rs	derived_echo_ledger_bridge_proves_and_catches_the_unexplained_hole	mutant-table-catch	model-laws, registered-model-sound
tests/derived_ring_ty.rs	derived_effect_phase_lock_proves_and_catches_cadence_slide	NON-SPEC (delete)	
tests/derived_ring_ty.rs	derived_effect_present_rebase_proves_and_catches_frame_doublets	NON-SPEC (delete)	
tests/derived_ring_ty.rs	derived_effect_presentability_settle_proves_and_catches_stranded_pixels	model-laws	registered-model-sound
tests/derived_ring_ty.rs	derived_emacs_search_navigation_proves_and_catches_each_regression	NON-SPEC (delete)	
tests/derived_ring_ty.rs	derived_exact_instance_retention_covers_the_lease_pid_matrix	model-claim-breaks-first	
tests/derived_ring_ty.rs	derived_exact_profanity_completion_rejects_predictive_fuc	model-deadlock-free	model-laws
tests/derived_ring_ty.rs	derived_fallback_band_clip_proves_and_catches_unclipped_blit	NON-SPEC (delete)	
tests/derived_ring_ty.rs	derived_fallback_precedence_proves_and_catches_discovery_over_config	NON-SPEC (delete)	
tests/derived_ring_ty.rs	derived_fallback_scale_clamp_proves_and_catches_unclamped_ratio	NON-SPEC (delete)	
tests/derived_ring_ty.rs	derived_fd_handoff_no_leak_proves_and_catches_dangling_fd	NON-SPEC (delete)	
tests/derived_ring_ty.rs	derived_flash_limiter_proves_and_catches_overlap_blindness	declared-dials-and-scenarios	registered-model-sound
tests/derived_ring_ty.rs	derived_flash_limiter_window_proves_catches_and_multiplies	prove-catch-multiply	registered-model-sound
tests/derived_ring_ty.rs	derived_focus_modifier_cache_resets_only_at_focus_loss	model-laws	
tests/derived_ring_ty.rs	derived_font_catalog_generation_rejects_stale_completion	model-laws	
tests/derived_ring_ty.rs	derived_font_theme_generation_reprepares_overtaken_config	model-laws	
tests/derived_ring_ty.rs	derived_gpu_loss_recovery_schedules_once_and_stops_recording	model-claim-breaks-first	model-laws, model-strict, registered-model-sound
tests/derived_ring_ty.rs	derived_gpu_loss_routes_to_fallback_and_catches_retry_on_dead_device	registered-model-sound	
tests/derived_ring_ty.rs	derived_grid_translate_proves_and_catches_chrome_band_leak	NON-SPEC (delete)	
tests/derived_ring_ty.rs	derived_handoff_roundtrip_proves_and_catches_dropped_session	NON-SPEC (delete)	
tests/derived_ring_ty.rs	derived_harness_model_ladder_lands_every_due_move_and_catches_the_cold_only_rule	model-deadlock-free	registered-model-sound
tests/derived_ring_ty.rs	derived_harness_model_priority_proves_and_catches_every_writer_and_reader_law	model-deadlock-free	model-strict, registered-model-sound
tests/derived_ring_ty.rs	derived_harness_model_switch_proves_and_catches_every_law	mutant-table-catch	model-deadlock-free, model-claim-breaks-first, declared-dials-and-scenarios, model-strict, registered-model-sound
tests/derived_ring_ty.rs	derived_harness_model_switch_ride_keeps_the_move_laws_at_build_0	test-only-laws-lifted	mutant-table-catch, declared-dials-and-scenarios
tests/derived_ring_ty.rs	derived_harness_model_switch_ride_keeps_the_move_laws_at_build_1	test-only-laws-lifted	mutant-table-catch, declared-dials-and-scenarios
tests/derived_ring_ty.rs	derived_hdr_present_gate_proves_and_catches_hdr_without_optin	NON-SPEC (delete)	
tests/derived_ring_ty.rs	derived_hdr_reconfigure_retag_proves_and_catches_ignored_failure	model-claim-breaks-first	model-strict, registered-model-sound
tests/derived_ring_ty.rs	derived_historical_recovery_converges_without_republication	model-laws	model-strict, registered-model-sound
tests/derived_ring_ty.rs	derived_hyperlink_scheme_cap_proves_and_catches_never_allow_admission	NON-SPEC (delete)	
tests/derived_ring_ty.rs	derived_idle_deadline_proves_and_catches_missed_earliest	NON-SPEC (delete)	
tests/derived_ring_ty.rs	derived_ignition_reservation_lifecycle_proves_and_catches_stale_future_work	NON-SPEC (delete)	
tests/derived_ring_ty.rs	derived_ignition_reservation_rekey_proves_and_catches_overlap	NON-SPEC (delete)	
tests/derived_ring_ty.rs	derived_inject_floor_proves_and_catches_overdraft	NON-SPEC (delete)	
tests/derived_ring_ty.rs	derived_input_release_pairing_prevents_orphan_csi_u_bytes	model-laws	
tests/derived_ring_ty.rs	derived_kernel_proves_and_catches_gap	NON-SPEC (delete)	
tests/derived_ring_ty.rs	derived_key_injectivity_proves_and_catches_size_collision	NON-SPEC (delete)	
tests/derived_ring_ty.rs	derived_kitty_collectibles_proves_and_catches_duplicate_growth	NON-SPEC (delete)	
tests/derived_ring_ty.rs	derived_kitty_flush_worker_proves_finite_exit_ownership	model-laws	registered-model-sound
tests/derived_ring_ty.rs	derived_kitty_pin_merge_proves_and_catches_a_lost_pin	model-laws	registered-model-sound
tests/derived_ring_ty.rs	derived_kitty_sidecar_proves_bidirectional_reconcile_and_catches_rollback	NON-SPEC (delete)	
tests/derived_ring_ty.rs	derived_kitty_sing_detector_proves_and_catches_eight_press_arm	model-laws	model-strict, registered-model-sound
tests/derived_ring_ty.rs	derived_layout_coordinate_reset_proves_and_catches_stale_trail	registered-model-sound	
tests/derived_ring_ty.rs	derived_ligature_gate_proves_and_catches_unflagged_collapse	NON-SPEC (delete)	
tests/derived_ring_ty.rs	derived_log_rotation_proves_and_catches_lost_lines_and_unbounded_growth	model-laws	registered-model-sound
tests/derived_ring_ty.rs	derived_manual_config_completion_proves_keyboard_window_and_context_lifecycle	model-laws	registered-model-sound
tests/derived_ring_ty.rs	derived_manual_config_handoff_proves_path_reuse_and_exact_target_handling	registered-model-sound	
tests/derived_ring_ty.rs	derived_manual_diagnostics_lane_proves_latest_revision_and_stale_rejection	model-laws	registered-model-sound
tests/derived_ring_ty.rs	derived_manual_problem_navigation_proves_exact_reveal_and_full_semantics	model-laws	registered-model-sound
tests/derived_ring_ty.rs	derived_mint_reachability_proves_and_catches_untrusted_mint	NON-SPEC (delete)	
tests/derived_ring_ty.rs	derived_momentum_wait_read_proves_and_catches_expired_reparking	NON-SPEC (delete)	
tests/derived_ring_ty.rs	derived_motion_policy_proves_and_catches_ignored_reduce_flag	NON-SPEC (delete)	
tests/derived_ring_ty.rs	derived_native_async_delivery_proves_and_catches_focus_routing	registered-model-sound	
tests/derived_ring_ty.rs	derived_native_capture_source_proves_and_catches_stale_compositor_pixels	registered-model-sound	
tests/derived_ring_ty.rs	derived_native_close_plan_proves_and_catches_partial_detach	registered-model-sound	
tests/derived_ring_ty.rs	derived_native_config_observation_handoff_proves_and_catches_loss	registered-model-sound	
tests/derived_ring_ty.rs	derived_native_config_transaction_proves_and_catches_stale_overwrite	registered-model-sound	
tests/derived_ring_ty.rs	derived_native_control_routing_proves_and_catches_hidden_terminal_fallback	registered-model-sound	
tests/derived_ring_ty.rs	derived_native_document_publication_proves_and_catches_partial_publish	registered-model-sound	
tests/derived_ring_ty.rs	derived_native_document_queue_proves_bounded_nonblocking_admission	model-laws	registered-model-sound
tests/derived_ring_ty.rs	derived_native_draft_journal_proves_and_catches_stale_or_unsafe_prune	registered-model-sound	
tests/derived_ring_ty.rs	derived_native_editor_command_palette_proves_selection_and_exact_submit	registered-model-sound	
tests/derived_ring_ty.rs	derived_native_editor_modal_proves_and_catches_anchor_or_input_leak	registered-model-sound	
tests/derived_ring_ty.rs	derived_native_editor_viewport_proves_and_catches_fixed_desktop_capacity	registered-model-sound	
tests/derived_ring_ty.rs	derived_native_file_watch_proves_and_catches_priority_inversion	registered-model-sound	
tests/derived_ring_ty.rs	derived_native_markdown_history_proves_and_catches_unbounded_or_untrimmed_visit	registered-model-sound	
tests/derived_ring_ty.rs	derived_native_markdown_viewport_proves_and_catches_block_only_scroll	registered-model-sound	
tests/derived_ring_ty.rs	derived_native_packages_worker_proves_matching_completion_and_result_truth	model-laws	registered-model-sound
tests/derived_ring_ty.rs	derived_native_preview_font_convergence_proves_and_catches_lost_ready_frame	NON-SPEC (delete)	
tests/derived_ring_ty.rs	derived_native_recovery_interaction_proves_and_catches_unsafe_lifecycle	registered-model-sound	
tests/derived_ring_ty.rs	derived_native_reopen_ledger_proves_and_catches_loss_or_identity_reuse	registered-model-sound	
tests/derived_ring_ty.rs	derived_native_save_intent_latch_proves_and_catches_dropped_completion_pump	registered-model-sound	
tests/derived_ring_ty.rs	derived_native_settings_draft_close_proves_and_catches_loss	registered-model-sound	
tests/derived_ring_ty.rs	derived_native_settings_singleton_proves_and_catches_duplicate_activation	registered-model-sound	
tests/derived_ring_ty.rs	derived_native_tab_identity_proves_and_catches_reuse_or_dangling_focus	registered-model-sound	
tests/derived_ring_ty.rs	derived_native_update_admission_proves_and_catches_foreground_blocker	model-strict	registered-model-sound
tests/derived_ring_ty.rs	derived_native_update_apply_ladder_lands_a_busy_terminal_and_catches_the_stand_down	mutant-table-catch	model-deadlock-free, registered-model-sound
tests/derived_ring_ty.rs	derived_native_update_apply_ladder_lands_under_fairness_and_catches_the_livelocks	mutant-table-catch	liveness-obligations-sound, model-deadlock-free, registered-model-sound
tests/derived_ring_ty.rs	derived_native_update_attempt_identity_proves_and_catches_stale_abort	registered-model-sound	
tests/derived_ring_ty.rs	derived_native_update_auto_intent_proves_and_catches_lost_stage_wake	model-strict	registered-model-sound
tests/derived_ring_ty.rs	derived_native_update_disk_transaction_proves_and_catches_identity_or_early_gc	model-deadlock-free	registered-model-sound
tests/derived_ring_ty.rs	derived_native_update_failed_mark_suppression_proves_quarantine_and_backoff	registered-model-sound	
tests/derived_ring_ty.rs	derived_native_update_hidden_output_quiet_proves_liveness_and_future_retry	model-laws	model-strict, registered-model-sound
tests/derived_ring_ty.rs	derived_native_update_menu_activation_is_independent_of_active_tab_kind	model-laws	
tests/derived_ring_ty.rs	derived_native_update_overlap_handoff_proves_and_catches_ownership_regressions	model-deadlock-free	model-laws, model-strict, registered-model-sound
tests/derived_ring_ty.rs	derived_native_update_status_reconciliation_proves_caller_and_ready_authority	registered-model-sound	
tests/derived_ring_ty.rs	derived_native_update_worker_queue_proves_and_catches_lost_latch_or_drain	model-deadlock-free	model-laws, registered-model-sound
tests/derived_ring_ty.rs	derived_native_updater_proves_and_catches_stale_or_double_apply	registered-model-sound	
tests/derived_ring_ty.rs	derived_net_capability_grant_proves_and_catches_dropped_binding	NON-SPEC (delete)	
tests/derived_ring_ty.rs	derived_net_dial_after_grant_proves_and_catches_premature_dial	NON-SPEC (delete)	
tests/derived_ring_ty.rs	derived_notify_follow_checkpoint_proves_and_catches_premature_cursor	NON-SPEC (delete)	
tests/derived_ring_ty.rs	derived_nova_phase_proves_and_catches_reflash	NON-SPEC (delete)	
tests/derived_ring_ty.rs	derived_one_shot_peek_proves_and_catches_repeek	NON-SPEC (delete)	
tests/derived_ring_ty.rs	derived_operator_event_delivery_proves_and_catches_stale_claim	model-deadlock-free	model-strict, registered-model-sound
tests/derived_ring_ty.rs	derived_operator_fleet_fault_proves_and_catches_blocked_egress	model-deadlock-free	model-strict, registered-model-sound
tests/derived_ring_ty.rs	derived_operator_leadership_proves_and_catches_split_brain	model-deadlock-free	model-strict, registered-model-sound
tests/derived_ring_ty.rs	derived_operator_resync_cursor_proves_and_catches_silent_loss	model-deadlock-free	model-strict, registered-model-sound
tests/derived_ring_ty.rs	derived_operator_wal_actuator_proves_and_catches_interjection_or_replay	model-deadlock-free	model-strict, registered-model-sound
tests/derived_ring_ty.rs	derived_output_streak_attribution_proves_and_catches_unrelated_output	NON-SPEC (delete)	
tests/derived_ring_ty.rs	derived_output_streak_delivery_proves_and_catches_lost_episode_edges	model-laws	model-strict, registered-model-sound
tests/derived_ring_ty.rs	derived_pad_absorption_proves_and_catches_lopsided_split	NON-SPEC (delete)	
tests/derived_ring_ty.rs	derived_pane_tree_proves_and_catches_dangling_focus	model-laws	registered-model-sound
tests/derived_ring_ty.rs	derived_path_feed_snapshot_proves_same_read_binding_and_catches_reread	model-laws	registered-model-sound
tests/derived_ring_ty.rs	derived_per_window_metrics_proves_and_catches_shared_backend_clobber	NON-SPEC (delete)	
tests/derived_ring_ty.rs	derived_predictive_echo_proves_no_flash_and_catches_immediate_display	registered-model-sound	
tests/derived_ring_ty.rs	derived_present_retry_proves_future_bounded_recovery_and_catches_unbounded_train	model-laws	registered-model-sound
tests/derived_ring_ty.rs	derived_presentation_gate_proves_and_catches_text_colored_as_emoji	NON-SPEC (delete)	
tests/derived_ring_ty.rs	derived_presented_frame_tap_proves_and_catches_fail_open_map	model-claim-breaks-first	model-strict, registered-model-sound
tests/derived_ring_ty.rs	derived_press_custody_keeps_the_viewport_and_selection_off_inert_presses	model-laws	
tests/derived_ring_ty.rs	derived_program_resolution_retries_a_static_miss_and_stops_after_a_name	model-laws	
tests/derived_ring_ty.rs	derived_program_resolver_coalesces_replacements_and_recovers_a_crash	model-laws	
tests/derived_ring_ty.rs	derived_proxy_forward_proves_and_catches_forward_cycle	NON-SPEC (delete)	
tests/derived_ring_ty.rs	derived_published_identity_accepts_symbolic_history_and_rejects_drift	model-laws	
tests/derived_ring_ty.rs	derived_rain_band_containment_proves_and_catches_stale_glyph_ghost	NON-SPEC (delete)	
tests/derived_ring_ty.rs	derived_rain_ignition_proves_and_catches_dropped_flash_floor	NON-SPEC (delete)	
tests/derived_ring_ty.rs	derived_rain_lifecycle_proves_and_catches_phantom_relight	NON-SPEC (delete)	
tests/derived_ring_ty.rs	derived_rainbow_exit_sampling_proves_and_catches_sparse_restart	model-laws	registered-model-sound
tests/derived_ring_ty.rs	derived_rainbow_idle_twinkle_proves_and_catches_idle_wakes	NON-SPEC (delete)	
tests/derived_ring_ty.rs	derived_rainbow_landing_pool_keeps_the_latest_arrivals	model-laws	
tests/derived_ring_ty.rs	derived_rainbow_typed_continuity_rejoins_only_exact_same_row_text	model-laws	
tests/derived_ring_ty.rs	derived_read_image_seq_proves_and_catches_torn_and_run_ahead_stamps	registered-model-sound	
tests/derived_ring_ty.rs	derived_recovery_stimulus_requests_redraw_and_catches_silent_reset	model-claim-breaks-first	model-laws, model-strict, registered-model-sound
tests/derived_ring_ty.rs	derived_reduced_motion_companion_handoff_proves_and_catches_blackout	model-laws	registered-model-sound
tests/derived_ring_ty.rs	derived_release_channel_floor_proves_carry_forward_and_late_guard	model-laws	registered-model-sound
tests/derived_ring_ty.rs	derived_release_claim_landing_proves_the_writer_reader_contract	model-deadlock-free	model-laws, model-strict, registered-model-sound
tests/derived_ring_ty.rs	derived_release_journal_requires_exact_prefix_and_ordered_resume	model-laws	
tests/derived_ring_ty.rs	derived_release_post_intents_are_durable_and_one_shot	model-laws	
tests/derived_ring_ty.rs	derived_release_publisher_fence_proves_unique_mutation_session	model-laws	registered-model-sound
tests/derived_ring_ty.rs	derived_release_yank_is_successor_first_and_crash_convergent	model-laws	
tests/derived_ring_ty.rs	derived_restore_manifest_claim_is_durable_single_use_and_unique	registered-model-sound	
tests/derived_ring_ty.rs	derived_ring_spec_proves_and_catches_late_eviction	model-laws	registered-model-sound
tests/derived_ring_ty.rs	derived_roster_pair_redo_proves_crash_recovery_and_foreign_preservation	model-laws	registered-model-sound
tests/derived_ring_ty.rs	derived_same_caret_typed_echo_requires_exact_oldest_key_and_fresh_probe	model-laws	
tests/derived_ring_ty.rs	derived_scroll_glide_proves_and_catches_unsettled_reduced_edge	NON-SPEC (delete)	
tests/derived_ring_ty.rs	derived_scrollback_maintenance_lane_proves_output_isolation	model-laws	registered-model-sound
tests/derived_ring_ty.rs	derived_seamless_handoff_ownership_proves_and_catches_orphan_and_double_reader	model-deadlock-free	registered-model-sound
tests/derived_ring_ty.rs	derived_seamless_nonce_proves_and_catches_replay	NON-SPEC (delete)	
tests/derived_ring_ty.rs	derived_selection_custody_spares_damage_that_missed_the_selection	model-laws	
tests/derived_ring_ty.rs	derived_self_governor_proves_and_catches_breaker_bypass	NON-SPEC (delete)	
tests/derived_ring_ty.rs	derived_semantic_prewarm_generation_proves_and_catches_stale_worker	model-claim-breaks-first	model-strict, registered-model-sound
tests/derived_ring_ty.rs	derived_semantic_prewarm_handshake_proves_and_catches_dropped_or_mixed_candidate	model-claim-breaks-first	model-strict, registered-model-sound
tests/derived_ring_ty.rs	derived_semantic_prewarm_request_swap_proves_and_catches_mixed_active_paint	model-claim-breaks-first	registered-model-sound
tests/derived_ring_ty.rs	derived_serious_mode_intent_queue_proves_and_catches_stale_third_toggle	registered-model-sound	
tests/derived_ring_ty.rs	derived_serious_mode_proves_and_catches_effect_leak	NON-SPEC (delete)	
tests/derived_ring_ty.rs	derived_session_chrome_expiry_is_waking_bounded_and_self_disarming	registered-model-sound	
tests/derived_ring_ty.rs	derived_session_id_claim_proves_catches_and_multiplies	prove-catch-multiply	declared-dials-and-scenarios, model-laws, registered-model-sound
tests/derived_ring_ty.rs	derived_session_pool_proves_and_catches_premature_close	NON-SPEC (delete)	
tests/derived_ring_ty.rs	derived_settings_page_scroll_proves_and_catches_overscroll	registered-model-sound	
tests/derived_ring_ty.rs	derived_shade_phase_proves_and_catches_doubled_seam_line	NON-SPEC (delete)	
tests/derived_ring_ty.rs	derived_shared_budget_proves_and_catches_global_overrun	NON-SPEC (delete)	
tests/derived_ring_ty.rs	derived_snapshot_generation_commit_proves_and_catches_stale_publish	registered-model-sound	
tests/derived_ring_ty.rs	derived_snapshot_proves_and_catches_leak	NON-SPEC (delete)	
tests/derived_ring_ty.rs	derived_sparkle_identity_proves_and_catches_amnesia_and_rekey	model-laws	registered-model-sound
tests/derived_ring_ty.rs	derived_sparkle_persist_capacity_proves_and_catches_grace_overflow	model-laws	registered-model-sound
tests/derived_ring_ty.rs	derived_sparkle_reflow_cardinality_proves_and_catches_context_gate	model-laws	registered-model-sound
tests/derived_ring_ty.rs	derived_sparkle_retype_rearm_proves_and_catches_second_inert_birth	model-laws	registered-model-sound
tests/derived_ring_ty.rs	derived_spawn_locale_proves_and_catches_non_utf8_child	NON-SPEC (delete)	
tests/derived_ring_ty.rs	derived_startup_phase_publication_requires_all_eight_intervals	model-laws	
tests/derived_ring_ty.rs	derived_stream_fade_gate_proves_and_catches_fading_keystroke_echo	NON-SPEC (delete)	
tests/derived_ring_ty.rs	derived_strike_selection_proves_and_catches_largest_strike_bias	NON-SPEC (delete)	
tests/derived_ring_ty.rs	derived_styled_run_face_proves_and_catches_dilated_bold_run	NON-SPEC (delete)	
tests/derived_ring_ty.rs	derived_subscribe_proves_and_catches_silent_loss	NON-SPEC (delete)	
tests/derived_ring_ty.rs	derived_supernova_burst_mutex_keeps_the_nova_share_funded	model-laws	
tests/derived_ring_ty.rs	derived_surface_coverage_proves_full_live_bands_and_catches_frame_viewport	registered-model-sound	
tests/derived_ring_ty.rs	derived_sync_reopen_visibility_holds_dirty_new_episode_until_close	model-laws	model-strict, registered-model-sound
tests/derived_ring_ty.rs	derived_tab_nav_proves_and_catches_out_of_range_active	NON-SPEC (delete)	
tests/derived_ring_ty.rs	derived_tab_stop_handoff_proves_and_catches_narrow_truncation	model-laws	registered-model-sound
tests/derived_ring_ty.rs	derived_tab_strip_proves_and_catches_strip_desync	NON-SPEC (delete)	
tests/derived_ring_ty.rs	derived_terminal_modes_proves_and_catches_stuck_sync_hold	NON-SPEC (delete)	
tests/derived_ring_ty.rs	derived_text_blend_gate_proves_and_catches_degenerate_divide	NON-SPEC (delete)	
tests/derived_ring_ty.rs	derived_top_anchored_scroll_proves_history_retention	model-laws	registered-model-sound
tests/derived_ring_ty.rs	derived_trail_audio_lifecycle_proves_nonblocking_reset_and_idle_pause	model-laws	registered-model-sound
tests/derived_ring_ty.rs	derived_trail_audio_start_latency_proves_one_buffer_and_safe_reclaim	model-laws	registered-model-sound
tests/derived_ring_ty.rs	derived_transact_proves_and_catches_lost_update	NON-SPEC (delete)	
tests/derived_ring_ty.rs	derived_unknown_insert_orphan_keys_are_exact_ordered_and_invalidated	model-laws	
tests/derived_ring_ty.rs	derived_vf_axis_clamp_proves_and_catches_unclamped_coord	NON-SPEC (delete)	
tests/derived_ring_ty.rs	derived_vf_nudge_gate_proves_and_catches_ungated_nudge	NON-SPEC (delete)	
tests/derived_ring_ty.rs	derived_vibrancy_contrast_proves_and_catches_dropped_floor	NON-SPEC (delete)	
tests/derived_ring_ty.rs	derived_video_batch_publication_requires_current_directory_barrier	model-laws	model-strict, registered-model-sound
tests/derived_ring_ty.rs	derived_video_recording_lifecycle_proves_cleanup_and_serialization	model-claim-breaks-first	model-laws, model-strict, registered-model-sound
tests/derived_ring_ty.rs	derived_video_tap_slot_proves_and_catches_lifecycle_and_ordering_mutants	model-claim-breaks-first	model-strict, registered-model-sound
tests/derived_ring_ty.rs	derived_visible_pad_crop_proves_base_bottom_and_catches_raw_exposure	registered-model-sound	
tests/derived_ring_ty.rs	derived_wake_heal_evidence_proves_and_catches_false_heal	model-laws	registered-model-sound
tests/derived_ring_ty.rs	derived_watcher_failure_recovery_proves_and_catches_duplicate_or_hidden_failure	registered-model-sound	
tests/derived_ring_ty.rs	derived_watcher_latch_proves_and_catches_silent_loss	NON-SPEC (delete)	
tests/derived_ring_ty.rs	derived_wide_center_proves_and_catches_left_bias	NON-SPEC (delete)	
tests/derived_ring_ty.rs	derived_window_routing_proves_and_catches_each_seam_slip	registered-model-sound	
tests/derived_ring_ty.rs	property_classes_prove_and_catch_under_ty	model-deadlock-free	registered-model-sound
tests/derived_run_loop_waker.rs	run_loop_waker_proves_and_catches_the_spin_and_the_lost_wake	NON-SPEC (delete)	
tests/derived_say_replay_budget.rs	bridge_wide_replay_pages_rotate_after_control_work	model-laws	
tests/derived_screen_generation.rs	the_change_test_sees_an_equal_seq_re_entry	model-laws	
tests/derived_settings_draft_carry.rs	settings_draft_carry_every_defect_loses_the_draft	mutant-table-catch	
tests/derived_settings_draft_carry.rs	settings_draft_carry_proves_catches_and_has_no_dead_action	model-strict	registered-model-sound
tests/derived_settings_draft_carry.rs	settings_draft_carry_walks_the_update_the_way_the_person_meets_it	model-laws	
tests/derived_streaming_search_ty.rs	derived_streaming_search_proves_and_catches_unclamped_index	NON-SPEC (delete)	
tests/derived_streaming_search_ty.rs	derived_streaming_search_proves_and_catches_with_wrap_disabled	declared-dials-and-scenarios	registered-model-sound
tests/derived_streaming_search_ty.rs	streaming_search_each_law_has_its_own_slip	model-laws	
tests/derived_streaming_search_ty.rs	streaming_search_interpreter_walks_the_engine_lifecycle	declared-dials-and-scenarios	model-laws
tests/derived_subscribe_announcement.rs	a_session_is_never_announced_before_its_watch_or_its_exit	model-laws	model-strict, registered-model-sound
tests/derived_successor_warm_before_claim.rs	each_mutant_alone_breaks_the_invariant_it_was_written_for	mutant-table-catch	
tests/derived_successor_warm_before_claim.rs	every_warm_successor_proves_or_exits	liveness-obligations-sound	registered-model-sound
tests/derived_successor_warm_before_claim.rs	successor_warm_before_claim_proves_and_catches	NON-SPEC (delete)	
tests/derived_successor_warm_before_claim.rs	the_warm_precedes_the_dial_except_where_the_intake_changes_directory	model-laws	
tests/derived_supervise.rs	a_harness_turn_answered_with_real_work_ends_the_streak	model-laws	
tests/derived_supervise.rs	a_tab_that_opens_off_the_choice_and_a_lone_question_are_answered_under_the_same_proof	declared-dials-and-scenarios	model-laws
tests/derived_supervise.rs	a_taken_act_never_withholds_a_break_and_an_untaken_one_is_never_stacked_on	declared-dials-and-scenarios	model-laws
tests/derived_supervise.rs	an_account_rotation_step_that_moves_a_session_without_its_record_is_caught	NON-SPEC (becomes-spec-after-fix)	test-only-laws-lifted
tests/derived_supervise.rs	at_text_zero_the_question_model_is_todays	text-zero-projection	declared-dials-and-scenarios
tests/derived_supervise.rs	each_account_rotation_defect_breaks_its_own_law_slice_0_of_5	test-only-laws-lifted	mutant-table-catch
tests/derived_supervise.rs	each_account_rotation_defect_breaks_its_own_law_slice_1_of_5	test-only-laws-lifted	mutant-table-catch
tests/derived_supervise.rs	each_account_rotation_defect_breaks_its_own_law_slice_2_of_5	test-only-laws-lifted	mutant-table-catch
tests/derived_supervise.rs	each_account_rotation_defect_breaks_its_own_law_slice_3_of_5	test-only-laws-lifted	mutant-table-catch
tests/derived_supervise.rs	each_account_rotation_defect_breaks_its_own_law_slice_4_of_5	test-only-laws-lifted	mutant-table-catch
tests/derived_supervise.rs	each_limit_reset_defect_breaks_its_own_law_over_its_whole_space	mutant-table-catch	
tests/derived_supervise.rs	each_question_instance_holds_only_laws_its_constants_can_break	declared-dials-and-scenarios	
tests/derived_supervise.rs	each_usage_probe_defect_breaks_its_own_law_and_no_other_over_its_whole_space	mutant-table-catch	
tests/derived_supervise.rs	text_mode_answers_a_dialog_of_typed_and_fallback_tabs_on_a_multi_select	test-only-laws-lifted	declared-dials-and-scenarios
tests/derived_supervise.rs	text_mode_answers_a_dialog_of_typed_and_fallback_tabs_on_a_single_select	test-only-laws-lifted	declared-dials-and-scenarios
tests/derived_supervise.rs	text_mode_answers_every_tab_and_never_types_twice	declared-dials-and-scenarios	model-laws
tests/derived_supervise.rs	text_mode_hands_a_persons_text_over	declared-dials-and-scenarios	model-laws
tests/derived_supervise.rs	text_mode_proves_and_the_fence_alone_refutes_each_invariant_on_a_multi_select	declared-dials-and-scenarios	registered-model-sound
tests/derived_supervise.rs	text_mode_proves_and_the_fence_alone_refutes_each_invariant_on_a_single_select	declared-dials-and-scenarios	registered-model-sound
tests/derived_supervise.rs	text_mode_rests_on_the_person_fence_and_its_assumptions	declared-dials-and-scenarios	
tests/derived_supervise.rs	text_mode_writes_nothing_after_a_persons_key_until_it_reads_again	declared-dials-and-scenarios	model-laws
tests/derived_supervise.rs	text_mode_writes_one_text_per_tab_whatever_changes_after_it	declared-dials-and-scenarios	model-laws
tests/derived_supervise.rs	the_account_rotation_laws_break_when_the_sound_move_follows_a_low_active	sound-step-edits-refute	test-only-laws-lifted, mutant-table-catch
tests/derived_supervise.rs	the_account_rotation_laws_break_when_the_sound_move_picks_shared_or_low_first	sound-step-edits-refute	test-only-laws-lifted, mutant-table-catch
tests/derived_supervise.rs	the_account_rotation_laws_break_when_the_sound_move_reads_stale_or_follows_the_herd	sound-step-edits-refute	test-only-laws-lifted, mutant-table-catch
tests/derived_supervise.rs	the_account_rotation_liveness_breaks_when_the_sound_move_never_takes_the_canary_s_place	sound-step-edits-refute	test-only-laws-lifted, mutant-table-catch, liveness-obligations-sound
tests/derived_supervise.rs	the_account_rotation_moves_a_session_at_its_wall_once_an_account_is_usable	liveness-obligations-sound	
tests/derived_supervise.rs	the_account_rotation_moves_one_session_alone_and_the_rest_once_it_is_served	registered-model-sound	
tests/derived_supervise.rs	the_account_rotation_s_liveness_readings_hold_only_under_their_fairness	liveness-obligations-sound	
tests/derived_supervise.rs	the_account_rotation_s_sound_space_breaks_nothing_and_only_its_defects_are_dead	test-only-laws-lifted	mutant-table-catch
tests/derived_supervise.rs	the_box_ask_proves_and_catches_a_reask_on_a_redraw	declared-dials-and-scenarios	model-laws, registered-model-sound
tests/derived_supervise.rs	the_codex_rate_nudge_saves_the_work_and_comes_back	liveness-obligations-sound	model-laws
tests/derived_supervise.rs	the_decline_is_carried_out_and_a_keystroke_is_never_written_twice	model-laws	
tests/derived_supervise.rs	the_decline_proves_and_catches_a_keystroke_that_meets_a_state_it_was_not_decided_on	NON-SPEC (delete)	
tests/derived_supervise.rs	the_decline_rests_on_the_person_fence_at_the_write	declared-dials-and-scenarios	
tests/derived_supervise.rs	the_done_check_proves_and_catches_nudging_a_finished_task	model-laws	registered-model-sound
tests/derived_supervise.rs	the_fence_alone_refutes_every_decline_invariant	declared-dials-and-scenarios	
tests/derived_supervise.rs	the_fence_alone_refutes_every_question_invariant	declared-dials-and-scenarios	
tests/derived_supervise.rs	the_focus_choice_proves_and_catches_an_enter_on_an_unconfirmed_focus	NON-SPEC (delete)	
tests/derived_supervise.rs	the_host_turn_end_proves_and_catches_a_point_left_to_nobody	model-laws	registered-model-sound
tests/derived_supervise.rs	the_limit_reset_laws_break_when_a_sound_step_drops_a_guard	sound-step-edits-refute	mutant-table-catch
tests/derived_supervise.rs	the_limit_reset_takes_one_row_a_wall_and_enters_only_its_own_exact_confirmation	mutant-table-catch	liveness-obligations-sound, declared-dials-and-scenarios, model-laws, model-strict, registered-model-sound
tests/derived_supervise.rs	the_memory_dial_is_refuted_only_through_a_persons_copy_of_the_text	declared-dials-and-scenarios	
tests/derived_supervise.rs	the_model_switch_makes_the_fallback_and_returns_only_at_a_named_reset	liveness-obligations-sound	model-laws
tests/derived_supervise.rs	the_network_wall_proves_and_catches_typing_into_a_measured_outage	declared-dials-and-scenarios	model-laws, registered-model-sound
tests/derived_supervise.rs	the_question_answer_answers_every_tab_and_submits	declared-dials-and-scenarios	model-laws
tests/derived_supervise.rs	the_question_answer_proves_and_catches_a_key_that_meets_a_state_it_was_not_decided_on	declared-dials-and-scenarios	registered-model-sound
tests/derived_supervise.rs	the_question_answer_rests_on_the_person_fence_and_one_assumption	declared-dials-and-scenarios	
tests/derived_supervise.rs	the_rating_cards_0_is_written_only_where_it_lands_nowhere_else	declared-dials-and-scenarios	model-laws
tests/derived_supervise.rs	the_stall_remedy_proves_and_catches_a_kill_a_blind_term_and_a_second_one	model-laws	registered-model-sound
tests/derived_supervise.rs	the_supervisor_claim_proves_and_catches_a_press_on_a_stale_view	NON-SPEC (delete)	
tests/derived_supervise.rs	the_turn_end_policy_backs_off_for_no_turn_of_the_harness	model-laws	
tests/derived_supervise.rs	the_turn_end_policy_proves_and_catches_typing_over_a_person_and_escalating_done	NON-SPEC (delete)	
tests/derived_supervise.rs	the_usage_probe_writes_only_onto_its_own_panel_and_never_leaves_it_open	mutant-table-catch	liveness-obligations-sound, declared-dials-and-scenarios, model-laws, model-strict, registered-model-sound
tests/derived_supervise.rs	the_usage_wall_ends_and_never_types_before_its_reset	liveness-obligations-sound	declared-dials-and-scenarios, model-laws, model-strict, registered-model-sound
tests/derived_text_selection_lifecycle.rs	text_selection_calls_act_only_in_their_phase	model-laws	
tests/derived_trail_sound_ty.rs	trail_audio_reopen_ladder_proves_bounded_recovery_and_catches_every_replay	guard-conjunct-audit	declared-dials-and-scenarios, registered-model-sound
tests/derived_trail_sound_ty.rs	trail_audio_reopen_ladder_refutes_edits_to_its_own_reset_law	sound-step-edits-refute	declared-dials-and-scenarios
tests/derived_trail_sound_ty.rs	trail_audio_reopen_ladder_witnesses_recovery_exhaustion_and_the_replays	model-laws	
tests/derived_trail_sound_ty.rs	trail_sound_seam_proves_the_rulings_and_catches_d1	guard-conjunct-audit	registered-model-sound
tests/derived_trail_sound_ty.rs	trail_sound_seam_witnesses_every_ruling_and_the_d1_replay	model-laws	
tests/derived_trust_facts.rs	a_floors_only_change_is_judged_again_from_the_same_facts	model-laws	
tests/derived_trust_facts.rs	a_shortfall_the_clock_may_have_caused_is_measured_again_not_excused	model-laws	
tests/derived_trust_facts.rs	another_provers_floor_is_held_and_lowered_only_with_a_reason	model-laws	registered-model-sound
tests/derived_trust_facts.rs	trust_facts_prove_and_catch_every_defect_class	model-strict	registered-model-sound
tests/derived_typed_rekey.rs	typed_rekey_heals_a_read_key_and_takes_back_an_unread_one	model-laws	
tests/derived_typed_rekey.rs	typed_rekey_proves_catches_and_has_no_dead_action	model-strict	registered-model-sound
tests/derived_typed_rekey.rs	typed_rekey_the_no_way_back_design_is_caught_on_every_invariant	model-laws	registered-model-sound
tests/derived_typed_rekey.rs	typed_rekey_upgrades_a_healthy_shell_without_moving_its_key	model-laws	
tests/derived_update_carried_health.rs	a_carried_warning_heals_on_its_own_proof_and_the_next_episode_speaks	model-laws	
tests/derived_update_carried_health.rs	carried_health_proves_and_catches	NON-SPEC (delete)	
tests/derived_update_carried_health.rs	each_mutant_alone_breaks_the_invariant_it_was_written_for	mutant-table-catch	
tests/derived_update_check_coordination.rs	completed_checks_joins_and_lock_release_prove_and_catch	NON-SPEC (delete)	
tests/derived_update_history_carry.rs	a_pane_that_clears_before_the_import_holds_nothing_and_loses_nothing	model-laws	
tests/derived_update_history_carry.rs	a_quiet_handoff_carries_everything_and_counts_nothing	model-laws	
tests/derived_update_history_carry.rs	every_fallback_is_counted	model-laws	
tests/derived_update_history_carry.rs	the_history_carry_proves_catches_and_has_no_dead_action	model-strict	registered-model-sound
tests/derived_update_history_carry.rs	the_three_defects_are_caught_each_on_its_own_path	model-laws	registered-model-sound
tests/derived_update_parallel_attach.rs	the_handoff_rebuilds_in_the_attach_pass_and_proves_only_when_every_window_painted	model-deadlock-free	model-laws, registered-model-sound
tests/derived_update_parallel_attach.rs	the_successor_proves_under_fairness_and_an_unkicked_window_never_does	liveness-obligations-sound	registered-model-sound
tests/derived_update_parent_diet.rs	each_mutant_alone_breaks_the_invariant_it_was_written_for	mutant-table-catch	
tests/derived_update_parent_diet.rs	every_attempt_is_granted_or_retired	liveness-obligations-sound	
tests/derived_update_parent_diet.rs	speculative_sidecar_proves_and_catches	NON-SPEC (delete)	
tests/derived_update_parent_diet.rs	the_park_keeps_rewrites_or_writes_and_the_successor_reads_its_bytes	model-laws	
tests/derived_update_roster_freshness.rs	a_stale_stage_waits_for_the_check_which_retires_or_confirms_it	model-laws	
tests/derived_update_roster_freshness.rs	each_mutant_alone_breaks_the_invariant_it_was_written_for	mutant-table-catch	
tests/derived_update_roster_freshness.rs	roster_freshness_proves_and_catches	NON-SPEC (delete)	
tests/derived_update_structural_latch.rs	a_structural_latch_is_resampled_once_and_yields_to_a_newer_release	model-laws	
tests/derived_update_web_cache.rs	web_cache_proves_and_catches_lost_stage_and_rebound_authority	NON-SPEC (delete)	
tests/derived_upgrade_attended.rs	a_person_s_grace_over_a_ready_ends_in_the_restart	declared-dials-and-scenarios	model-laws
tests/derived_upgrade_settle.rs	a_ready_after_notices_at_breaks_owns_its_settle	declared-dials-and-scenarios	model-laws, law:upgrade-bounded-ownership
tests/derived_upgrade_settle.rs	a_refused_notice_owns_its_point_for_its_looks_and_no_turn_renews_them	declared-dials-and-scenarios	model-laws, law:upgrade-bounded-ownership
tests/derived_upgrade_settle.rs	the_end_of_the_agents_own_work_is_the_upgrades_point	declared-dials-and-scenarios	model-laws, law:upgrade-bounded-ownership
tests/derived_window_show.rs	window_show_proves_catches_and_has_no_dead_action	model-strict	registered-model-sound
tests/derived_window_show.rs	window_show_the_defects_are_caught_on_every_invariant	mutant-table-catch	registered-model-sound, bmc-reach-semantics
tests/derived_window_show.rs	window_show_walks_the_update_the_way_the_user_meets_it	model-laws	law:window-show
tests/foreign_facts_are_observed.rs	each_half_of_the_rule_is_load_bearing	NON-SPEC (move-to-guard)	
tests/foreign_facts_are_observed.rs	no_parameter_freezes_another_agents_state	NON-SPEC (move-to-guard)	
tests/foreign_facts_are_observed.rs	the_rule_still_catches_the_shape_it_was_written_for	NON-SPEC (move-to-guard)	
tests/macro_ring.rs	macro_built_interpreter_runs	interpreter-semantics	ty-model-desugar
tests/macro_ring.rs	macro_surface_equals_hand_built_model	ty-model-desugar	model-tla-rendering
tests/macro_ty_model.rs	macro_nested_if_else_interpreter_tiers	interpreter-semantics	ty-model-desugar
tests/macro_ty_model.rs	macro_nested_if_else_matches_hand_built	ty-model-desugar	model-tla-rendering
tests/macro_ty_model.rs	macro_nested_if_else_renders_nested_tla	model-tla-rendering	ty-model-desugar
tests/macro_ty_model.rs	macro_paren_and_gt_in_guard	interpreter-semantics	model-tla-rendering, ty-model-desugar
tests/macro_ty_model.rs	macro_unguarded_action_emits_no_guard_conjunct	model-tla-rendering	ty-model-desugar
tests/macro_ty_model.rs	macro_unguarded_action_is_always_enabled_in_interpreter	interpreter-semantics	ty-model-desugar
tests/macro_ty_model.rs	macro_unguarded_action_matches_hand_built_tla	ty-model-desugar	model-tla-rendering
tests/macro_ty_model.rs	writing_a_model_registers_it	model-registry	
tests/non_vacuity.rs	every_liveness_obligation_is_proven_minimal_and_caught	liveness-obligations-sound	
tests/non_vacuity.rs	every_registered_model_holds_and_each_law_is_what_it_declares	registered-model-sound	dead-negative-control-audit
tests/pet_stroke_ty.rs	pet_stroke_model_proves_and_catches_stationary_and_cooldown_bypasses	registered-model-sound	prove-and-catch-contract
tests/rainbow_composer_newline_ty.rs	composer_gate_waits_for_home_and_catches_the_early_spend	model-laws	registered-model-sound, law:rainbow-composer-newline
tests/rainbow_short_wrap_ty.rs	short_wrap_park_proves_exact_follow_and_catches_both_old_failures	model-laws	registered-model-sound, law:rainbow-short-wrap
tests/rfc_worked_examples_ty.rs	rfc_atlas_pack_proves_and_catches_overflow	prove-and-catch-contract	
tests/rfc_worked_examples_ty.rs	rfc_glyph_fidelity_proves_and_catches_mac_roman_substitution	prove-and-catch-contract	
tests/rfc_worked_examples_ty.rs	rfc_mark_row_origin_proves_and_catches_y_divergence	prove-and-catch-contract	
tests/rfc_worked_examples_ty.rs	rfc_reload_frame_proves_and_catches_font_shrink	prove-and-catch-contract	
tests/update_activation_observation.rs	activation_observation_proves_and_catches_unknown_retirement	registered-model-sound	prove-and-catch-contract
tests/update_activation_observation.rs	environment_repair_proves_and_catches_permanent_refusal	registered-model-sound	prove-and-catch-contract
tests/update_activation_observation.rs	failure_target_proves_and_catches_replacement_misattribution	registered-model-sound	prove-and-catch-contract
tests/update_activation_observation.rs	retired_intent_proves_and_catches_obsolete_latch	registered-model-sound	prove-and-catch-contract
tests/window_routing_twin.rs	window_routing_twin_holds_invariants_over_all_reachable_states	registered-model-sound	bmc-reach-semantics
```

## 3. Coverage

- 723 tests read. 596 map to one of the 56 specs; 127 are not spec samples (§4).
- The biggest collapses (primary tests): `model-laws` (237), `registered-model-sound` (55), `mutant-table-catch` (45), `model-strict` (28), `declared-dials-and-scenarios` (27), `model-claim-breaks-first` (16), `liveness-obligations-sound` (16), `model-deadlock-free` (15).
- 391 of the covered tests retire at Tier 0 without any Trust change, once M1 gives the
  models the surface to declare what the tests now script: `model-laws`, `mutant-table-catch`,
  `model-strict`, `declared-dials-and-scenarios`, `model-claim-breaks-first`,
  `model-deadlock-free`, `test-only-laws-lifted`, `sound-step-edits-refute`,
  `guard-conjunct-audit`, `prove-catch-multiply` and `text-zero-projection`. A Clean proof of the
  same statements waits on gap 10.
- The checker and tool code (`src/`) holds 125 of the tests; their specs are about the
  compiled `aterm-spec` fns and wait on gaps 1, 3 and 7 for the pure ones and gap 8 for the effectful ones.

Gap histogram (gap number → number of specs blocked by it, from the `blocking_gaps` column):

| gap | specs |
|---|---|
| 1 | 34 |
| 2 | 7 |
| 3 | 14 |
| 4 | 3 |
| 5 | 9 |
| 7 | 23 |
| 8 | 16 |
| 9 | 3 |
| 10 | 14 |
| 11 | 5 |
| 12 | 1 |
| 13 | 14 |
| 14 | 3 |
| M1 (model-side, not in §7) | 9 |

No spec is blocked only by gap 0: every row also needs a non-scalar carrier (1), body widening (3),
`&mut self` (2), loops (7), effects (8) or the model→code surface (10).

## 4. Tests that are not spec samples

> Status 2026-10-16: the 122 **delete** rows below are deleted (121 removed; `tier_1_runs_the_real_command_through_this_model` was already gone), with the 12 test files they emptied. The move and fix rows remain.

Each one is marked `NON-SPEC (<action>)` in §2.

| tests | count | why | action |
|---|---|---|---|
| `tests/derived_ring_ty.rs`: 80 `derived_*_proves_and_catches_*` fns that only call `prove_and_catch_scalar` on a registered model at its committed constants (aa_edge_hardening … wide_center) | 80 | Duplicate `tests/non_vacuity.rs::every_registered_model_holds_and_each_law_is_what_it_declares` (interpreter tier) and `spec-check models` (ty tier), which state Sound(M) for every registered model and declared scenario. The only difference: these demand the catch at Buggy=1, while the sweep accepts any declared mutant dial. | **delete** |
| The same shape in `derived_run_loop_waker`, `derived_streaming_search_ty` (`…_unclamped_index`), `derived_successor_warm_before_claim`, `derived_supervise` (4: decline, focus_choice, supervisor_claim, turn_end_policy; its `Budget=2` is a declared `scenario`), `derived_update_carried_health`, `derived_update_check_coordination` (6 models in a loop), `derived_update_parent_diet`, `derived_update_roster_freshness`, `derived_update_web_cache` | 12 | As above. | **delete** |
| `src/derive/models_console_life.rs`, `src/interp.rs::promoted_bmc_proves_and_catches` (its doc points at a deleted test), `tests/conformance_fabric_enable.rs::capability_publication_…`, and one `*_proves_and_catches_*` each in `derived_control_bind_retry`, `derived_control_preparation`, `derived_control_socket_handoff`, `derived_fd_lifecycle_ty`, `derived_harness_restore_queue` (2), `derived_input_unread`, `derived_launched_dial_wait`, `derived_main_stall_refusal`, `derived_output_retention`, `derived_paste_order_ty`, `derived_pet_observation_admission`, `derived_rendezvous_{admission,claim,grant}`, `derived_ribbon_{follow,release_restoration,row_hold}`, `derived_budgeted_search_resume_ty` | 22 | Bare `prove_and_catch` on a registered model: a duplicate of the registry sweep. | **delete** |
| `derived_harness_policy_one_writer::harness_policy_one_writer_proves_and_catches_the_second_writer` | 1 | The sweep already requires the audit to be Ok; the `Ok(3)` only pins the current mutant list. | **delete** |
| `derived_metal_drawable_acquire_ty::metal_drawable_acquire_proves_bounds_and_catches_…` | 1 | `prove_and_catch_tiered` with its Result discarded: a duplicate of the ty-tier sweep that passes on `NotRun`. | **delete** |
| `derived_launched_dial_wait::a_dead_successor_ends_the_wait_without_the_dial_budget`, `derived_harness_policy_one_writer::the_window_catches_up_with_the_file`, `derived_harness_upgrade_ladder::the_upgrade_lands_under_fairness_and_catches_the_livelocks`, `derived_harness_upgrade_progress::the_upgrade_moves_or_its_record_names_what_holds_it_under_fairness` | 4 | Bare `liveness_proves_and_catches_tiered` on a registered obligation: duplicates `non_vacuity.rs::every_liveness_obligation_is_proven_minimal_and_caught` and the ty tier. | **delete** |
| `tests/conformance_fabric_enable.rs::tier_1_runs_the_real_command_through_this_model` | 1 | Prose-pinning: `include_str!` of another crate's test file plus substring checks (`'.1 = 8;'`). The spec it guards is aterm-link's own Tier-1 conformance (gap 10). | **delete** |
| `tests/derived_keeper.rs::the_keeper_spaces_fit_the_interpreter` | 1 | Asserts only termination and `eprintln!`s the state counts; its doc promises to catch a space explosion but no bound is asserted, and the sweep walks these spaces. | **delete** |
| `tests/foreign_facts_are_observed.rs::no_parameter_freezes_another_agents_state` | 1 | A text scan over the workspace enforcing a coding rule (no `bool` cross-agent parameter that is a literal at every call site). It states nothing about a compiled fn. Per AGENTS.md a rule no lint can name is an `aterm-census` guard (`verify guards`). | **move to an aterm-census guard** |
| `tests/foreign_facts_are_observed.rs::the_rule_still_catches_the_shape_it_was_written_for`, `::each_half_of_the_rule_is_load_bearing` | 2 | Test the scanner code that exists only in this test file; they are the guard's negative control and move with it. | **move with the guard** |
| `src/verify_tests.rs::store_bin_dir_mirrors_the_atpkg_default_prefix` | 1 | Pins a hand-mirrored constant path: the "registry plus a test that polices it" pattern AGENTS.md forbids. The spec is `unix_store_bin_dir(h) = atpkg::platform::default_prefix(h)/bin`. | **becomes a spec after the fix**: share the definition with atpkg, or state the cross-crate equality (gap 12) |
| `tests/derived_supervise.rs::an_account_rotation_step_that_moves_a_session_without_its_record_is_caught` | 1 | `#[should_panic]` control for an assertion inside the test helper `account_rotation_space`: the law is stated only in test code. | **becomes a spec after the fix**: lift the law into `supervisor_account_rotation_model` (`test-only-laws-lifted`), then it is an ordinary mutant row |

Totals: delete 122, move to an aterm-census guard 3, becomes a spec after a fix 2.

Not deletable yet, although they look like the duplicates above: the tests that drive a configuration
the model does not declare (StreamingSearch `Wrap=0`, SupervisorQuestionAnswer `MaxRetry ∈ {1,3}`,
TrailAudioReopenLadder `Budget=6`, the SupervisorQuestionText `Multi/Tabs/Review` shapes) and the
tests of secondary dials (`derived_upgrade_settle.rs`). The sweep never checks those, so deleting the
tests would silently lose the proofs. They retire with `declared-dials-and-scenarios`.

## 5. Implied specs that look wrong, or disagree with the code

1. **`check::flip_buggy` does not do what its test's name claims.** It replaces the first substring
   `Buggy = 0` (or `Buggy = FALSE`) anywhere in the cfg, not a declared constant line:
   `NotBuggy = 0` is flipped and `Buggy = 05` becomes `Buggy = 15`
   (`src/check.rs::a_buggy_constant_is_flipped_once_and_only_when_declared`). Fix the code (match a
   `CONSTANT Buggy = …` line) before proving `flip-buggy`.
2. **Discarded `prove_and_catch_tiered` results.** `derived_metal_drawable_acquire_ty` and both
   `derived_paste_order_ty` walk tests do `let _ = prove_and_catch_tiered(..)`. `derive.rs`'s own
   workflow doc says `Err(NotRun)` is never a pass, yet these pass on any machine without ty.
3. **Specs held only by tests.** Configurations the models do not declare as `scenario` (§4), the
   secondary mutant dials (`Model::mutant_dials` reads only `Buggy`), and laws stated only in test
   walks (`test-only-laws-lifted`: the HarnessModelSwitch ride gaps (a)-(c) and the PersonFlag=2
   walk, the QuestionText "no HarnessType on a fallback tab" loop, the AccountRotation "every move
   leaves a true record" helper assertion). The models prove less than their tests claim.
4. **Change detectors standing in for specs.**
   - `derived_supervise.rs::at_text_zero_the_question_model_is_todays` pins reachable-state counts
     (12989, 3197, 6295), committed constants and action-name lists; the spec is `text-zero-projection`.
   - Many derived tests pin exact `audit_dead_negative_controls` counts (`Ok(3)`, `Ok(4)`, `Ok(8)`,
     `Ok(11)`) and exact mutant lists. Under Sound(M) these are model-shape pins; they break on any
     added negative control without a defect. `model-strict` states them as a declared set instead.
   - `derived_window_show.rs::window_show_the_defects_are_caught_on_every_invariant` pins field values
     of the first BFS counterexample that depend on BFS and declaration order.
   - `derived_trail_sound_ty.rs::trail_audio_reopen_ladder_witnesses_recovery_exhaustion_and_the_replays`
     asserts `Budget == 2`; its facts should hold for every Budget.
   - `derived_harness_usage_tap.rs::the_usage_tap_model_proves_and_catches` asserts the model is in
     `xref::model_registry()`, which `ty_model!` guarantees by construction.
   - Two AccountRotation liveness tests compare `live.goal.to_tla()` with a test helper's rebuilt
     formula: a consistency check of test code, not a property.
   - `src/verify_tests.rs::a_slow_probe_is_answered_and_a_wedged_one_is_killed_at_its_bound` mixes an
     elapsed-time assertion and a const pin (`PROBE_HANG >= 30 s`) with the real contract.
5. **Timing-dependent coverage.** `src/verify_tests.rs::a_tools_last_line_names_what_runs_when_its_changes_race`
   sleeps 500 ms to "give the change back time to reach its claim"; if the second thread has not
   claimed by then it still passes, so the race it claims to cover is not guaranteed to run. The
   spec `provenance-report-per-tool-serial` replaces it.
6. **A twin that never compares with the TLA+.** `tests/window_routing_twin.rs` says that if the
   interpreter disagreed with the derived spec "the twin would diverge here", but it never reads
   `to_tla` or runs ty; it runs the interpreter BFS, which the sweep already does, and it hard-codes
   4 action and 3 invariant names, so a later action goes unexplored silently. The claim it makes is
   `interpreter-semantics` (the executable-twin clause) plus `model-tla-rendering`.
7. **Toy models presented as catches of real bugs.** `tests/rfc_worked_examples_ty.rs` says "three
   shipped fixes" and "all six configs" but has four models (eight configs), and the models are
   tautological (Reload sets `font := Target`, the invariant is `font == Target`). No conformance
   test binds them to the renderer or reload code they name. They only exercise the
   prove-and-catch driver, which is why they map to `prove-and-catch-contract`.
8. **Near-duplicates kept for the ty tier.** `tests/update_activation_observation.rs` (4) and
   `tests/pet_stroke_ty.rs` (1) are registered models that the sweep already holds; they add only the
   optional ty tier, which `spec-check models` also covers. They retire with `registered-model-sound`.
9. **Invariants evaluated on another config's states.** Tests in `derived_debugger_access`,
   `derived_pet_surface_seam` and `derived_harness_upgrade_progress` evaluate the committed model's
   invariants on states reached by a differently configured model. That is sound only because
   invariant expressions are shared across configs, which nothing states.
10. **Helpers that bypass the shared API.** `derived_flash_limiter_proves_and_catches_overlap_blindness`
    re-implements `interp::with_consts`; two cursor-cat tests set `Buggy` by mutating consts in place
    instead of `interp::with_buggy`. Harmless, but duplicate code.
11. **A stale doc in a guard.** In `tests/foreign_facts_are_observed.rs` a leftover doc comment now
    attaches to `type CallSite`. Its literal check accepts only `true`/`false` and its parser is
    textual, so the guard can miss a named or const-propagated argument; fix when it moves to aterm-census.
12. **Counts in the manifests.** The manifests said 343, 349 and 34 tests (726); the files hold 348,
    342 and 33 (723). The second pass's "93 bare duplicates" is 92 by its own list.

## 6. The specs I would prove first (easiest, high value)

No spec here is blocked only by gap 0, so the order trades the smallest Trust widening for the most tests:

0. **Delete the 122 duplicates in §4 now and move the scanner to aterm-census.** No proof is
   needed: `registered-model-sound` and `liveness-obligations-sound` are already checked for every
   registered model by `tests/non_vacuity.rs` and `spec-check models`. Keep the configuration and
   secondary-dial tests §4 names until their scenarios and dials are declared.
1. **`xorshift64`** (gaps 2, 3). `next_u64` is three straight-line `u64` shift/xor steps and `new`
   one branch. Factor the step as a free `fn xorshift_step(x: u64) -> u64` and it needs only E6
   shifts and bitwise ops (gap 3); `below`/`index`/`range`/`pick` are bounds with `%` (omega-class).
   It retires both `src/rng.rs` tests and is a clean first probe for gap 3.
2. **`civil-date`** on `civil_from_unix_days` alone (gap 3; `build_date` is the effectful shell,
   gap 8). Pure `i64` div/rem arithmetic against a proleptic-Gregorian reference; retires
   `build_dates_are_utc_civil_dates` and anchors `provenance-line-rendering`.
3. **M1 in `aterm-spec`** (no Trust gap, Tier 0): add catch tables, named dials and scenarios, a
   `strict`/negative-control declaration, a terminal predicate, and `ENABLED`/primed/`reach`
   expressions to `ty_model!`, with the sweep checking each. This is not a Clean proof, but it turns
   391 scripted tests into declared, registry-checked specs (`model-laws` alone has
   237 primary tests), which is by far the largest collapse in the crate. It also fixes §5.3.
4. **The pure `&str` parsers** (gaps 1, 3, 7): `ty-transcript-parsers`, `shell-literal-word`,
   `ay-obligation-judging`, `flip-buggy` (after the §5.1 fix), then `tla-definitions-parse` (12 tests)
   and `spec-link-classification`. They are total functions over text with fail-closed contracts and
   share the `&str` carrier work of gap 1 with the parser crate.
5. **The checker chain** (§1.3): `interpreter-semantics` then `bmc-reach-semantics`. These are the
   root of trust for every Tier-0 model check, and they need gaps 1, 3 and 7 (BTreeMap states,
   recursive `Expr`, BFS loops) at XL size, so they come after the cheaper wins but before any claim
   that the model specs are Certified.

Effects last: `tool-discovery-order`, `binary-identity-chain`, the provenance specs, `ty-cases-pool`,
`ty-verdict-memo`, `ty-capabilities-cache-and-probe`, `wait-within`, `first-exec-once-scratch` and
`ay-discharge-needs-success` get their pure cores factored out first (`shell-literal-word`,
`exec_stub_target` on bytes, `claim_report`, `caps_from`); the shells wait on gap 8 (and 13 for
the threaded ones).
