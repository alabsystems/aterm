<!-- Copyright 2026 Andrew Yates -->
<!-- SPDX-License-Identifier: Apache-2.0 -->

# aterm-aos-pane: specs

> 2026-11-01, after the Rewind release audit: the crate's first rows are
> `pane-refines-model`, the pane task's refinement of
> `aterm_models_aos::aos_pane_faithful_replay_model` (`AosPaneFaithfulReplay`), and
> `pane-model-laws`, the model's own laws. They were written with the code they state, and the
> tests named in them ran. The crate's other tests (src/tests.rs) are not mapped here yet.

Of record: the doc comment of `aos_pane_faithful_replay_model`
(crates/aterm-models-aos/src/models_pane.rs), and the code where they differ.
A *chain* is one `trace::PaneRecording` that holds only a start state and the slices, resizes and
live witnesses recorded after it, one reply effect per slice. *The check* of a chain is
`PaneRecording::check(PaneCode::ThisBuild, build)`, the replay and verdict that `rewind check`
and `aos check` run.

## 1. Specs (TSV)

Columns: `id`, `kind`, `functions`, `statement`, `primary_tests` (how many tests it retires as
primary; the list is in §2).

```tsv
id	kind	functions	statement	primary_tests
pane-refines-model	refinement	trace::{PaneRecording::{new,bytes,resize,witness,check},run}; PrebuiltPane::step; check_step; finish_verdict; PaneCounts::from_inputs_and_matched; screen_digest; aterm_models_aos::aos_pane_faithful_replay_model	The pane check refines AosPaneFaithfulReplay (MaxIn = 4). The projection reads the inputs (n), the check's counts of resizes, witnesses and matched witnesses (z, w, m), and its verdict and cursor (pass, fail:WitnessMatches, fail:RepliesMatch, fail:ClockRecorded). It also reads PaneCode::ThisBuild stepped through the chain, each reply shadowed by its recorded one: the first fault that stepped replay meets and its cursor (a witness whose replayed screen is not the live one, a diverged reply, a slice with no clock), and whether its screen differs from the live engine's. Every chain of the model's actions up to four inputs is recorded from a live engine and checked after every input. Slice, Resize and Witness are what a faithful recorder writes. SkewedSlice records a prompt mark an hour past the clock the live engine parsed it at, AlteredReply records a DA query with another reply, DroppedClock records a slice with no clock, and ForgedWitness records a digest of no screen. After every input the projection is the model's successor for the action named, every law holds on it, and the stepped replay's state digest is the check's. Every transition of the model's 493-state space is taken this way, and every verdict occurs. Each fault is refused by name at its cursor. For each Buggy dial 1-4, the mutant's successor differs from the real step and breaks the dial's law. A chain with no fault passes, including one no witness checked. The debug-only DemoClockSkew fails a faithful recording of a mark and its witness under WitnessMatches at the witness, which is the model's verdict for SkewedSlice then Witness; ThisBuild passes it.	6
pane-model-laws	invariant	aterm_models_aos::aos_pane_faithful_replay_model	In every reachable state of AosPaneFaithfulReplay (493 at MaxIn = 4, the same count under the interpreter and ty), the first fault names the verdict. A witness that differs fails WitnessMatches, a diverged reply fails RepliesMatch, and a slice with no clock fails ClockRecorded, each at the fault's cursor. With no fault, the verdict is pass, with or without a witness. A law that fails does not stop the replay. Every action fires. Each Buggy dial 1-4 breaks exactly its own law (WitnessMatches, RepliesMatch, ClockRecorded, FaithfulPasses), and the fault sweep finds nothing.	0
```

`pane-model-laws` retires no test of this crate. Its four tests are in
crates/aterm-models-aos/tests/derived_pane.rs (`the_pane_contract_holds_and_each_law_has_its_own_mutant`,
`a_faithful_chain_passes_with_or_without_a_witness`, `each_fault_is_named_by_its_law_at_its_own_input`,
`the_first_fault_decides_and_a_failed_law_does_not_stop_the_replay`). crates/aterm-models/tests/non_vacuity.rs
(`every_aos_model_is_registered`) holds the model in the registry that `verify spec` checks.

Both rows cover chains whose every step the engine admits. Outside them, the check also fails
ClockRecorded for a host operation with no clock, WitnessMatches for a refused step, a short run
or an observation of another kind than its input, and RepliesMatch for recorded replies the replay
never reached; cell-size and host inputs are in no count. A chain with no fault passes without a
witness, and no work budget refuses a step.

## 2. Test → spec map (TSV)

Paths are relative to `crates/aterm-aos-pane/`. `primary` is the spec that retires the test;
`also` lists other specs the same test samples.

```tsv
file	test	primary	also
src/model_conformance_tests.rs	every_chain_of_the_pane_model_is_a_real_check_in_lock_step	pane-refines-model	pane-model-laws
src/model_conformance_tests.rs	negative_control_skewed_clock_the_witness_after_it_fails_witness_matches	pane-refines-model	
src/model_conformance_tests.rs	negative_control_witness_digest_mismatch_fails_witness_matches	pane-refines-model	
src/model_conformance_tests.rs	negative_control_altered_da_reply_fails_replies_match	pane-refines-model	
src/model_conformance_tests.rs	negative_control_dropped_clock_fails_clock_recorded	pane-refines-model	
src/model_conformance_tests.rs	negative_control_a_faithful_chain_passes	pane-refines-model	
```
