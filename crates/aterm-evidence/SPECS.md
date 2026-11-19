<!-- Copyright 2026 Andrew Yates -->
<!-- SPDX-License-Identifier: Apache-2.0 -->

# aterm-evidence: spec recovery from its unit tests

Each row states a property its tests sample, so that a Trust/Clean proof against the compiled Rust
function can retire them; none is proved yet. The code is `src/obs.rs` (`CredentialPrint`,
`WitnessId`, `Source`, `Subject`, `Obs`, `Fresh`), `src/ledger.rs` (`Ledger`, `Observed`),
`src/hold.rs` (`HoldBound`, `PersonHold`, `WorkHold`) and `src/episode.rs` (`Episode`,
`EpisodeClock`, `CLEAR_GAP_S`). Paths are relative to `crates/aterm-evidence`.

## 0. Notation used in the statements

- An observation `o` is `(value, at, witness, source, subject)`; `at` is seconds since the Unix
  epoch of the evidence itself. `o ≻ p` := `(o.at, o.value, o.witness) > (p.at, p.value,
  p.witness)` lexicographically (the value's `Ord` breaks a tie of dates, the `WitnessId`'s
  derived `Ord` a tie of date and value).
- `L` is a `Ledger<K>`. `First(L)` is the map witness ↦ the first observation `L.observe` was given
  with that witness. `R(L, s)` = `{ o ∈ First(L) : o.subject = s }`; `N(L, s)` = the `≻`-greatest
  element of `R(L, s)`, which is unique because `First(L)` holds one observation per witness, so
  `≻` is total on it (None when `R(L, s)` is empty).
- `P(n, r, g)` = `CredentialPrint::new(n, r, g)`; `Cred(p)` = `Subject::Credential(p)`.
- `C` is an `EpisodeClock`; `E(C)` = `C.current()`; `G` = `CLEAR_GAP_S` = 300.
- The LoginEvidence model is `aterm_models_harness::login_evidence_model`, with the projection
  `src/ledger_conformance_tests.rs::project`.

## 1. Specs (TSV)

Columns: `id`, `kind`, `functions`, `statement`, `subsumes`, `difficulty` (Rust features the proof must
handle), `blocking gaps` (rows of `docs/TRUST-LANE.md` (Blocking gaps); gap 0,
the installed seal, blocks every row).

```tsv
id	kind	functions	statement	subsumes	difficulty	blocking gaps
print-digest	contract	obs::CredentialPrint::new, obs::CredentialPrint::bytes	P(n, r, g).bytes() = SHA-256(D ++ be64(|n|) ++ n ++ be64(|r|) ++ r ++ be64(g)), D = "aterm-evidence credential print v1\0". Hence P is injective in (n, r, g) up to SHA-256 collisions (each part is length-framed, so no two triples share an input), a renewal (g+1) is another print, and no credential byte is an input. Debug shows the first 8 bytes as 16 hex digits.	src/obs_tests.rs::a_print_is_namespace_row_and_generation_and_nothing_else	EASY: byte framing, u64 big-endian, a cross-crate SHA-256 as an axiom	0, 1, 12
fresh-window	contract	obs::Obs::fresh, obs::Fresh::{obs,value,until,into_obs}	o.fresh(now, h) = Some(f) ⇔ o.at ≤ now ∧ now − o.at ≤ h.as_secs(); then f.obs() = o, f.until() = o.at ⊕ h.as_secs() (saturating). Fresh has private fields and no other constructor, so every Fresh value is the result of such a check.	src/obs_tests.rs::an_observation_is_fresh_only_within_its_horizon_and_never_from_the_future	EASY: checked_sub, Option, a struct with private fields	0, 1
ledger-newest-decides	invariant	ledger::Ledger::{observe,verdict,newest_of,readings_of}, obs::WitnessId	For every sequence of observe calls: L.newest_of(s) = N(L, s) and L.verdict(p) = N(L, Cred(p)). observe(o) returns Reread iff o.witness ∈ First(L) before the call (and changes nothing); else it records o and returns Decides iff N(L, o.subject) was None or o ≻ N(L, o.subject) before the call, Older otherwise. Hence the verdict, the whole observation (value, at, witness and source), is a function of the set R(L, Cred(p)) alone: arrival order never decides; a witness read again, whatever its date, value or subject this time, never renews and replaces no stored value; and an observation of any subject other than Cred(p), an earlier generation of the same row included, never decides for p, so a renewal starts with no verdict. A WitnessId::ScreenLine is (sid, hash, row_epoch), so the same line at the same row epoch in two tabs is two witnesses, and each decides for its own subject whichever is read first. Refines LoginEvidence Observe: NewestObservationDecides, ArrivalOrderNeverDecides, ForeignPrintSaysNothing, SameWitnessNeverRenews.	src/ledger_tests.rs::the_newest_observation_of_the_current_print_decides, src/ledger_tests.rs::every_arrival_order_gives_one_verdict, src/ledger_tests.rs::one_witness_read_twice_is_one_observation, src/ledger_tests.rs::the_same_line_in_two_tabs_is_two_witnesses, src/ledger_tests.rs::an_older_prints_observation_says_nothing, src/ledger_conformance_tests.rs::login_evidence_conforms, src/ledger_conformance_tests.rs::a_restored_sticky_word_is_refused_and_is_the_sticky_word_mutant	MEDIUM: two BTreeMaps kept consistent (a cached deciding witness per subject), generic K: Ord, lexicographic tuple order over references, a derived Ord on an enum with String fields	0, 1, 2, 4
hold-horizon	contract	hold::HoldBound::horizon, hold::PersonHold::words, hold::WorkHold::words	horizon(Within(d)) = Some(d), horizon(Program{backstop: b}) = Some(b), horizon(PersonNow(_)) = horizon(WorkNow(_)) = None. Every PersonHold's and WorkHold's words say the hold lasts "only while" its state or progress is true.	src/hold_tests.rs::only_a_persons_state_and_progressing_work_have_no_horizon	EASY: fieldless and one-field enum matches, &'static str	0, 1
episode-monotone	invariant	episode::EpisodeClock::{held,clear,progress,current}	C.held(t, h): if E(C) = Some(e) and no clear look began at c with t − c ≥ G, the result is (e.since, min(e.horizon_at, e.since ⊕ h)); otherwise (t, t ⊕ h); either way the pending clear is forgotten. So without progress or a clear of G, since never moves and horizon_at never grows, whatever word or process holds. C.clear(t) records the first clear look c while E(C) is Some, and ends the episode once t − c ≥ G. C.progress() ends it. A clear look with no episode starts nothing.	src/episode_tests.rs::a_change_of_word_or_process_keeps_the_episode_and_never_extends_its_horizon, src/episode_tests.rs::a_gap_shorter_than_the_clear_gap_keeps_the_episode	EASY: Option<Copy struct>, saturating u64, min	0, 1
```

## 2. Test → spec map (TSV)

`primary` is the spec that retires the test. `also` lists the other specs the same test samples.

```tsv
file	test	primary	also
src/obs_tests.rs	a_print_is_namespace_row_and_generation_and_nothing_else	print-digest	
src/obs_tests.rs	an_observation_is_fresh_only_within_its_horizon_and_never_from_the_future	fresh-window	
src/ledger_tests.rs	the_newest_observation_of_the_current_print_decides	ledger-newest-decides	
src/ledger_tests.rs	every_arrival_order_gives_one_verdict	ledger-newest-decides	
src/ledger_tests.rs	one_witness_read_twice_is_one_observation	ledger-newest-decides	
src/ledger_tests.rs	the_same_line_in_two_tabs_is_two_witnesses	ledger-newest-decides	
src/ledger_tests.rs	an_older_prints_observation_says_nothing	ledger-newest-decides	print-digest (a renewal is another print)
src/ledger_conformance_tests.rs	login_evidence_conforms	ledger-newest-decides	
src/ledger_conformance_tests.rs	a_restored_sticky_word_is_refused_and_is_the_sticky_word_mutant	ledger-newest-decides	
src/hold_tests.rs	only_a_persons_state_and_progressing_work_have_no_horizon	hold-horizon	
src/episode_tests.rs	a_change_of_word_or_process_keeps_the_episode_and_never_extends_its_horizon	episode-monotone	
src/episode_tests.rs	a_gap_shorter_than_the_clear_gap_keeps_the_episode	episode-monotone	
```

## 4. Tests that are not spec samples

None: every test maps to a spec in §2. The negative control drives the `#[cfg(test)]` seam
`ledger::restore_sticky_word`, which no build without tests compiles.

## 5. Implied specs that look wrong, or disagree with the code

1. **The ledger keeps every first reading.** Nothing is pruned: a reading that can no longer
   decide is still the record that a later re-read of its witness is a re-read. A caller that keeps
   a ledger for a long-lived subject bounds it by the subject's own lifetime (a renewal is a new
   print).
2. **A witness belongs to the subject of its first reading.** The same witness offered later for
   another subject is a re-read; a caller that attributes one witness to two subjects is wrong, and
   the ledger keeps the first. That rule needs each id to name one observation wherever it was
   made: a screen line names its tab's sid, because its text hash and row epoch repeat across
   tabs; a transcript row's and a reading's id must be unique as their minter makes them.
