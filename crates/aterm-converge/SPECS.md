<!-- Copyright 2026 Andrew Yates -->
<!-- SPDX-License-Identifier: Apache-2.0 -->

# aterm-converge: specs behind the converge engine's tests

> Status: spec recovery (docs/PLAN-tests-to-specs-2026-10-15.md, step 2) for the engine every
> provisioning surface runs (design `docs/DESIGN-init-system-2026-10-20.md` §3). Each spec below is
> held by the tests named in its `subsumes` column; none is proved through Trust/Clean yet. The crate
> is on the Trust lane with a floor in `crates/aterm-release/trust/floors.tsv`.

Source: `crates/aterm-converge/src/` — `model.rs` (the vocabulary and the report rows), `engine.rs`
(order, probe, plan, apply, read-back), `scope.rs` (what a surface lets a run do, `--only`, and the
wrappers that hold changes back) and `journal.rs` (`FileJournal`). Every test is a unit test in the
file it covers.

## 0. Notation used in the statements

- `M` = `src/model.rs`, `E` = `src/engine.rs`, `S` = `src/scope.rs`, `J` = `src/journal.rs`.
- `run(items, mode, scope, journal)` = `run_scoped`; `run` and `run_with` are it at `Scope::EVERY`.
- `ok?(r)` = `r.outcome ∈ {Ok, Changed}`.
- `names(w, id)` = `id = w`, or `w = p*` and `id` starts with `p`.
- The probe sequence of an item is what its successive `probe()` calls return; `apply`, `refresh`,
  `stands_aside` and `stood_aside` are the item's own answers.

## 1. Specs (TSV)

Columns: `id`, `kind`, `functions`, `statement`, `subsumes`, `difficulty` (the Rust features a proof must
handle), `blocking_gaps` (rows of `docs/AUDIT-trust-for-test-migration-2026-10-15.md` §7).

```tsv
id	kind	functions	statement	subsumes	difficulty	blocking_gaps
CV-FAILURE	table-property	Outcome::is_failure	is_failure(o) ⇔ o ∈ {Failed, Unknown}: a person step, a block, a plan, not-applicable and deferred never fail a run.	M::a_failed_or_unmeasured_item_fails_the_run_and_a_person_step_does_not	const fn, enum match	1,3
CV-FAILS-SURFACE	table-property	ItemReport::fails_surface	fails_surface(r) ⇔ r.outcome = Failed ∨ (r.essential ∧ r.outcome ∈ {Planned, Person, Blocked, Unknown}); Ok, Changed, NotApplicable and Deferred never fail a surface. A row carries its item's essential(), false by default.	M::only_an_essential_row_left_wrong_fails_its_surface; E::a_row_says_whether_its_item_is_essential	&self on a struct, enum match	1,2,3
CV-ASIDE-EXIT	table-property	StoodAside::exit_status, StoodAside::why	exit_status is 75 for Lock and Waiting, 69 for Offline, 77 for Permission; why() is the line each kind carries.	M::standing_aside_ends_with_the_reason_specific_exit_status	enum with String payloads	1,2,3
CV-JSON	contract	ItemReport, Report (serde)	∀ r: ItemReport, from_str(to_string(r)) = r, for every Remedy and StoodAside variant; ∀ Report likewise, its text starting `{"schema":1,"mode":<kebab mode>,"complete":<bool>,"items":[`, with `"essential":true` once per essential row.	M::a_row_round_trips_through_json_with_every_new_field; M::a_report_round_trips_in_the_one_schema	serde derive, aterm_json, String	1,7,9,12
CV-JSON-COMPAT	contract	ItemReport (serde)	A row written without layer, class, explain, stood_aside or essential reads with None, None, None, None, false; a row whose optional fields are None and essential false writes none of their keys.	M::a_row_written_before_the_new_fields_still_reads	serde defaults and skip_serializing_if	1,7,9,12
CV-NAMES	contract	names, named_by	named_by(w, me, ids) yields, in index order, every i with names(w, ids[i]), except i = me when w ends in `*`; an exact edge may name its own item.	E::an_after_that_names_no_item_is_refused_and_a_prefix_must_match_another	&str prefix tests, iterator filter	1,5,7
CV-ORDER	contract	order	order(items) = Ok(π) iff every edge names some item (CV-NAMES) and the edges are acyclic; π places each item after every item its edges name and is otherwise the composition order (each step takes the lowest-index item whose prerequisites are placed), so an ordered composition keeps the identity. Err(Dangling{id, want}) names the first item, in composition order, with an edge naming no item, and that edge; Err(Cycle{ids}) lists, in composition order, every item no order can place (the cycle and everything behind it); an item that names itself exactly is a cycle. On Err, run probes nothing.	E::an_out_of_order_composition_runs_each_prerequisite_first; E::a_cycle_is_refused_before_anything_is_probed; E::an_after_that_names_no_item_is_refused_and_a_prefix_must_match_another	nested loops over Vec<Vec<usize>>, trait objects	1,3,5,7
CV-STEP	table-property	run_one	With no hold, an item's first probe decides its row outside a change: Ok → Ok with to = now (CV-REFRESH covers a refresh); Person{step, how} → Person (to = step, why = how); Blocked → Blocked; NotApplicable → NotApplicable; Deferred → Deferred; Unusable{w} → Unknown with why = w (never Ok, whoever made the reading), unless the item's read_aside() is Some(a) right after it: then Blocked with why = a.why() and stood_aside = a, never applied, explained as CV-STOOD-ASIDE explains a, and what waits on it in an apply stands aside with a (CV-WAITS); Change in a plan → Planned with from, to, revert and effect on the row. None of these calls apply.	E::a_plan_run_never_applies; E::an_item_already_as_wanted_is_not_applied; E::an_unmeasurable_item_is_unknown_never_ok; E::a_reading_a_user_could_not_make_stays_unknown_with_its_own_reason; E::not_applicable_and_deferred_are_their_own_outcomes; E::an_unreadable_probe_that_lacked_the_network_stands_aside_offline	enum match over Probe	1,2,3
CV-READBACK	contract	run_one	In an apply, a Change is journaled (CV-JOURNAL-FIRST), then applied once: apply Err(w) → Failed with why = w; apply Ok with no stood_aside → the item is probed again and only Ok counts (Changed); Change{from} → Failed "applied, but it still reads {from}"; Unusable → Failed "applied, but the read-back did not answer: …"; any other → Failed "applied, but the read-back says: …".	E::only_a_matching_read_back_counts_as_changed; E::an_apply_error_is_a_failure_with_its_reason	enum match, format!	1,2,3,4
CV-REFRESH	contract	run_one	refresh() is called only in an apply, with no hold, after an Ok probe; Some(Ok) stays Ok only when a second probe is Ok (else Failed), Some(Err(w)) is Failed with why = w. When refresh_changes(), the line (id, now, now, revert) is journaled before the refresh, and a refused line leaves it unrun and the row Failed "not refreshed: the journal refused the record (…)"; an activation (refresh_changes false) writes no line.	E::opt_in_refresh_is_apply_only_and_requires_a_matching_second_probe; S::a_refresh_that_changes_the_machine_is_journaled_before_it_runs	Option<Result>, a second probe	1,2,3,4
CV-JOURNAL-FIRST	invariant	run_one, Journal, ReadOnly	before_change(id, from, to, revert) returns before the item's apply (or changing refresh) starts, once per change; when it errs, apply is never called and the row is Failed "not applied: the journal refused the record (…)". No line is asked for an Ok item without refresh_changes, a plan, a held or not-own-account change, or one that stands aside. ReadOnly refuses every record, so under it a plan plans and an apply fails without applying.	E::a_change_the_journal_cannot_record_is_not_applied; E::a_read_only_journal_refuses_every_change	a call order across a trait object	1,2,4,8
CV-WAITS	contract	run_scoped, unmet_prerequisite	In an apply, an item one of whose edges names an earlier row that is not ok? is not probed: its row is Blocked "waits for {id}" for the first such row (edges in order, rows in run order); when that row stood aside, the row is Blocked "waits for {id}: {line}" with the same stood_aside. In a plan the item is probed as usual.	E::an_item_waits_for_a_prerequisite_that_did_not_land; S::a_dependant_of_a_planned_item_waits_for_it	nested iteration over rows	1,3,7
CV-PLAN-WAITS	contract	run_one	In a plan, Waits{on, why} reads Planned ("read after {on}, which this plan changes first ({why})") exactly when an earlier row with id = on is Planned, and Blocked with why otherwise; a Blocked probe stays Blocked and an Unusable one Unknown whatever is planned before it, and what waits on them is Blocked.	E::a_plan_judges_an_item_waiting_on_a_planned_prerequisite_as_planned; E::a_plan_keeps_a_block_its_prerequisite_does_not_lift; E::a_plan_keeps_an_unreadable_item_unknown_behind_a_planned_prerequisite	slice search, enum match	1,3,7
CV-STOOD-ASIDE	contract	run_one, explain_aside	An apply that returns Ok while the item reports stood_aside(a) is Blocked with why = a.why() and stood_aside = a, and is not probed again; its explanation's why is a.why() and its remedy Network for Offline, the item's Deferred remedy for Lock and Waiting, and its Blocked remedy for Permission.	E::a_stood_aside_apply_is_blocked_with_its_kind_and_is_not_read_back	enum match, trait calls	1,2,3,4
CV-EXPLAIN	invariant	run_one, run_scoped, blank	Every row carries its item's layer, class, effect and essential; exactly the rows not ok? carry an explanation, item.explain(p) of the probe that judged the row: the read-back after a failed one, Unusable{w} after a failed refresh, Blocked{why} after a refused record, explain_aside after a stand-aside.	E::every_row_carries_its_layer_and_class_and_only_unfinished_rows_an_explanation; E::a_failed_refresh_and_a_refused_record_are_explained_by_what_failed	struct construction, enum match	1,2,3
CV-SCOPE-CLASS	contract	Scope::applies, run_scoped	In an apply, an item whose class the scope does not apply runs as in a plan (probed, never applied, Planned on a change), so what waits on it is Blocked (CV-WAITS). REPAIR applies Relay and Asked; a Relay-only scope plans Asked.	S::an_apply_run_plans_the_classes_its_surface_does_not_apply; S::a_dependant_of_a_planned_item_waits_for_it	slice contains	1,3
CV-SCOPE-ADMITS	contract	Scope::admits, run_scoped, not_own_account	A Console item where the scope does not admit Console (not at the console, or not the account's own shell) is never probed; its row is Blocked with NOT_AT_CONSOLE, or not_own_account(why) when the shell is not the account's own. With not_own_account = Some(w), every Account item is probed in a plan and an apply alike: Ok reads Ok and is not refreshed; Change reads Blocked with not_own_account(w), from and to named, no stood_aside, and nothing applied, refreshed or journaled. In the account's own shell the same items apply, refresh and journal.	S::outside_the_accounts_own_shell_every_item_is_read_and_no_change_is_made; S::away_from_the_console_an_account_item_applies_and_a_console_item_is_blocked	Option<&str>, enum match	1,2,3
CV-SELECTS	contract	selects, Layer::name	selects(t, id, family, layer) ⇔ t ≠ "" ∧ (t = family ∨ layer.map(name) = Some(t) ∨ id = t ∨ (t holds no `@` ∧ id starts with t followed by `.` or `@`)); a layer word selects only through the row's layer.	S::a_token_names_a_family_a_layer_an_id_or_an_id_it_prefixes	&str prefix tests	1,3,5
CV-SELECT	contract	select	select(items, []) = (items, []) unchanged. Otherwise the kept items are those some token selects, in composition order; each kept item's after() is its edges that name some kept item; the second value is the tokens that select no item, in order; and the kept run orders (CV-ORDER: no edge dangles).	S::select_keeps_what_it_names_and_narrows_edges_to_the_run	Vec of trait objects, wrappers	1,3,5,7
CV-WRAPPERS	refinement	standing_aside, held_back, left_as_is, left_to_another	standing_aside(items, a): each item probes as itself; Ok reads Ok; a Change in an apply is Blocked with a and never journaled or applied; what waits on it is Blocked with a, unprobed; select keeps it standing aside. held_back(items, w): a Change reads Blocked "{from}; not changed: {w}" with no stood_aside (its surface counts it undone), never applied or journaled; Ok reads Ok. left_as_is(items, w): a Change reads NotApplicable "{from}; left as it is: {w}" with no stood_aside, never applied, refreshed or journaled; Ok reads Ok and is not refreshed. left_to_another(items, w): as left_as_is, and it waits on no item (its after is empty), so a successor of one left the same way is read, not Blocked; a probe that Waits on {on} reads NotApplicable "waits for {on}; left as it is: {w}".	S::a_standing_aside_item_is_probed_never_applied_and_blocked_on_the_lock; S::a_held_back_item_is_read_and_blocked_and_never_applied; S::an_item_left_as_is_is_read_and_not_applicable_and_never_applied; S::an_item_another_run_lays_waits_on_nothing	trait-object delegation	1,2,3
CV-ACTIVATION-CHECK	contract	run_scoped, settle, read_back_as	An initially Ok item in an admitted Apply class sharing an activation waits for its group's final callback. It is probed again only if another item caused activate() to be invoked, including an activation that returns Err or partial failure. This passive check never calls its apply or refresh or asks for a journal line, preserves Ok with no from or revert when the probe remains Ok, and reports Failed only when its own new probe does not establish the desired state. Activation-wide failure is not itself a failure of an unchanged peer. With no activation invoked, or for an unrelated item, a Plan run, or a class read as a plan, no passive probe is added. Each row is delivered once, after its final check.	E::a_shared_activation_rechecks_initially_correct_items_without_reapplying_them; E::a_class_read_as_a_plan_is_not_rechecked_by_an_applied_shared_activation; E::an_activation_that_does_not_run_fails_every_item_it_was_to_activate; E::an_activation_runs_only_when_an_applied_item_awaits_it_and_something_is_pending	trait objects, grouped completion, enum match	1,2,3,4,7
FJ-BEFORE	contract	FileJournal::before_change	When an item's apply starts, journal.ndjson already holds its line {time > 0, id, from, to, revert}; a failed apply keeps its line; one line per change, in run order.	J::the_line_is_on_disk_before_the_apply_runs_and_a_failed_apply_keeps_it	fs, serde, the clock	1,8,9
FJ-LAZY	contract	FileJournal::under, run	Nothing under <state>/converge exists before the first change: an item already as wanted and a plan create no folder and no file.	J::an_item_already_as_wanted_and_a_plan_write_nothing	fs	1,8
FJ-BOUND	invariant	FileJournal::append	After every append journal.ndjson and journal.1.ndjson each hold at most the bound; a line that would carry a non-empty journal past it first renames the journal over journal.1.ndjson, so the rotation's last line directly precedes the journal's first; a line longer than the bound is refused and its change not made; the folder holds only the journal, its rotation and its lock.	J::the_journal_rotates_within_its_bound_and_keeps_one_rotation; J::a_line_over_the_bound_is_refused_and_its_change_not_made	u64 arithmetic, fs metadata and rename	1,8
FJ-REGULAR	contract	FileJournal::append	A journal path that is not a regular file refuses every line ("not a regular file"), and so the change it would record is not made.	J::a_journal_that_is_not_a_regular_file_refuses_and_nothing_is_applied	fs metadata	1,8
FJ-DURABLE	contract	FileJournal::append	Each line's data is flushed durable (aterm_durable::durable_data) before before_change returns; the folder is flushed too whenever the line starts a file (the first line, and the first after a rotation), and only then.	J::each_line_is_flushed_durable_before_its_change_and_a_new_entry_with_its_folder	fs, a recording seam	1,8
FJ-LOCK	invariant	Held::take, FileJournal::append	Appends are serialized by journal.lock (flock): a writer waits at most `wait` for another holder, then refuses ("another run held the journal for more than …s") and its change is not made; once released the next line takes it. Writers that cross the bound together each rotate only a journal one more line would carry past the bound: no line is torn or lost and none is refused.	J::a_line_waits_for_another_writers_lock_and_a_hold_past_the_wait_refuses_its_change; J::writers_that_cross_the_bound_together_rotate_once_and_none_is_refused	flock, threads	1,8,13
```

## 2. Test → spec map (TSV)

```tsv
file	test	primary	also
M	a_failed_or_unmeasured_item_fails_the_run_and_a_person_step_does_not	CV-FAILURE	
M	a_row_round_trips_through_json_with_every_new_field	CV-JSON	
M	a_row_written_before_the_new_fields_still_reads	CV-JSON-COMPAT	
M	only_an_essential_row_left_wrong_fails_its_surface	CV-FAILS-SURFACE	
M	a_report_round_trips_in_the_one_schema	CV-JSON	CV-FAILS-SURFACE
M	standing_aside_ends_with_the_reason_specific_exit_status	CV-ASIDE-EXIT	
S	outside_the_accounts_own_shell_every_item_is_read_and_no_change_is_made	CV-SCOPE-ADMITS	CV-JOURNAL-FIRST, CV-REFRESH
S	a_refresh_that_changes_the_machine_is_journaled_before_it_runs	CV-REFRESH	CV-JOURNAL-FIRST
S	a_held_back_item_is_read_and_blocked_and_never_applied	CV-WRAPPERS	CV-JOURNAL-FIRST
S	an_item_left_as_is_is_read_and_not_applicable_and_never_applied	CV-WRAPPERS	CV-JOURNAL-FIRST, CV-REFRESH
S	an_item_another_run_lays_waits_on_nothing	CV-WRAPPERS	CV-WAITS
S	away_from_the_console_an_account_item_applies_and_a_console_item_is_blocked	CV-SCOPE-ADMITS	CV-EXPLAIN
S	an_apply_run_plans_the_classes_its_surface_does_not_apply	CV-SCOPE-CLASS	
S	a_dependant_of_a_planned_item_waits_for_it	CV-SCOPE-CLASS	CV-WAITS
S	a_token_names_a_family_a_layer_an_id_or_an_id_it_prefixes	CV-SELECTS	
S	select_keeps_what_it_names_and_narrows_edges_to_the_run	CV-SELECT	CV-ORDER
S	a_standing_aside_item_is_probed_never_applied_and_blocked_on_the_lock	CV-WRAPPERS	CV-WAITS, CV-ASIDE-EXIT, CV-SELECT
E	a_row_says_whether_its_item_is_essential	CV-FAILS-SURFACE	CV-EXPLAIN
E	a_read_only_journal_refuses_every_change	CV-JOURNAL-FIRST	
E	a_plan_run_never_applies	CV-STEP	
E	only_a_matching_read_back_counts_as_changed	CV-READBACK	
E	an_apply_error_is_a_failure_with_its_reason	CV-READBACK	
E	an_item_already_as_wanted_is_not_applied	CV-STEP	
E	opt_in_refresh_is_apply_only_and_requires_a_matching_second_probe	CV-REFRESH	
E	an_unmeasurable_item_is_unknown_never_ok	CV-STEP	CV-FAILURE
E	a_reading_a_user_could_not_make_stays_unknown_with_its_own_reason	CV-STEP	
E	an_item_waits_for_a_prerequisite_that_did_not_land	CV-WAITS	CV-NAMES
E	a_plan_judges_an_item_waiting_on_a_planned_prerequisite_as_planned	CV-PLAN-WAITS	
E	a_plan_keeps_a_block_its_prerequisite_does_not_lift	CV-PLAN-WAITS	
E	a_plan_keeps_an_unreadable_item_unknown_behind_a_planned_prerequisite	CV-PLAN-WAITS	CV-FAILURE
E	not_applicable_and_deferred_are_their_own_outcomes	CV-STEP	
E	a_change_the_journal_cannot_record_is_not_applied	CV-JOURNAL-FIRST	
E	an_out_of_order_composition_runs_each_prerequisite_first	CV-ORDER	CV-WAITS, CV-NAMES
E	a_cycle_is_refused_before_anything_is_probed	CV-ORDER	
E	an_after_that_names_no_item_is_refused_and_a_prefix_must_match_another	CV-ORDER	CV-NAMES
E	a_stood_aside_apply_is_blocked_with_its_kind_and_is_not_read_back	CV-STOOD-ASIDE	CV-ASIDE-EXIT
E	an_unreadable_probe_that_lacked_the_network_stands_aside_offline	CV-STEP	CV-STOOD-ASIDE, CV-WAITS, CV-ASIDE-EXIT
E	a_failed_refresh_and_a_refused_record_are_explained_by_what_failed	CV-EXPLAIN	CV-REFRESH, CV-JOURNAL-FIRST
E	every_row_carries_its_layer_and_class_and_only_unfinished_rows_an_explanation	CV-EXPLAIN	CV-READBACK
E	a_shared_activation_rechecks_initially_correct_items_without_reapplying_them	CV-ACTIVATION-CHECK	CV-JOURNAL-FIRST, CV-EXPLAIN
E	a_class_read_as_a_plan_is_not_rechecked_by_an_applied_shared_activation	CV-ACTIVATION-CHECK	CV-SCOPE-CLASS
E	an_activation_that_does_not_run_fails_every_item_it_was_to_activate	CV-ACTIVATION-CHECK	CV-EXPLAIN
E	an_activation_runs_only_when_an_applied_item_awaits_it_and_something_is_pending	CV-ACTIVATION-CHECK	CV-JOURNAL-FIRST
J	the_line_is_on_disk_before_the_apply_runs_and_a_failed_apply_keeps_it	FJ-BEFORE	CV-JOURNAL-FIRST
J	an_item_already_as_wanted_and_a_plan_write_nothing	FJ-LAZY	CV-JOURNAL-FIRST
J	the_journal_rotates_within_its_bound_and_keeps_one_rotation	FJ-BOUND	
J	a_line_over_the_bound_is_refused_and_its_change_not_made	FJ-BOUND	CV-JOURNAL-FIRST
J	a_journal_that_is_not_a_regular_file_refuses_and_nothing_is_applied	FJ-REGULAR	CV-JOURNAL-FIRST
J	each_line_is_flushed_durable_before_its_change_and_a_new_entry_with_its_folder	FJ-DURABLE	FJ-BOUND
J	a_line_waits_for_another_writers_lock_and_a_hold_past_the_wait_refuses_its_change	FJ-LOCK	CV-JOURNAL-FIRST
J	writers_that_cross_the_bound_together_rotate_once_and_none_is_refused	FJ-LOCK	FJ-BOUND
```

## 3. Coverage

- Every test in the crate is in §2's map.
- These tests hold the engine alone; the machines families that run on it keep their own tests in
  `aterm-machines`.

## 4. Tests that are not spec samples

None.

## 5. Implied specs that look wrong, or disagree with the design

- CV-WAITS blocks an item behind any prerequisite that did not end `Ok` or `Changed`, so a prerequisite
  that is `NotApplicable` on this host (an Intel Mac, an older macOS) blocks what runs after it in every
  apply, although `NotApplicable` is "nothing to do, and nothing wrong" and never fails a surface
  (CV-FAILS-SURFACE). A composition with such an edge needs the rule to admit `NotApplicable`, or the
  dependant reads `Blocked` on every such host.
- CV-ORDER is quadratic in the items per placement (each step scans every item for the lowest ready
  one): fine for a run's few hundred items, and the property it states (the lowest-index ready item
  first) is what keeps an ordered composition in its order.

## 6. Proof order (easiest, high value first)

1. CV-FAILURE, CV-ASIDE-EXIT, CV-FAILS-SURFACE: closed tables over enums (gap 1's enum carriers, then
   gap 3's enum matches).
2. CV-SELECTS, CV-NAMES: string-prefix predicates, the `--only` and edge rules every surface shares.
3. CV-ORDER, CV-SELECT: the scheduler, a loop over index vectors (gap 7).
4. CV-STEP, CV-READBACK, CV-REFRESH, CV-PLAN-WAITS, CV-STOOD-ASIDE, CV-EXPLAIN: `run_one` as a table from
   the probe sequence to the row; they need trait-object calls modelled as the item's answers.
5. CV-JOURNAL-FIRST, CV-WAITS, CV-SCOPE-*, CV-WRAPPERS: properties of whole runs; with the probe
   sequences as inputs they are state-machine properties a `ty_model!` of the run could carry first.
6. FJ-*: the file journal is effectful (gap 8) and FJ-LOCK concurrent (gap 13); they stay tests.
