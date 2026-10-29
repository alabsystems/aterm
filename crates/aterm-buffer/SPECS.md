<!-- Copyright 2026 Andrew Yates -->
<!-- SPDX-License-Identifier: Apache-2.0 -->

# aterm-buffer: spec recovery from its 25 unit tests

> Status 2026-10-17, after the merge with the tests-and-gates audit: the rows naming 2 tests it
> deleted are gone, and so is §4's row of a third, the deleted source-compatibility test.
> Counts below are reconciled with current `main` (2026-10-28).

> Audit amendment: The 2026-10-16 code audit corrected the source invariant to use logical log.total() after eviction (§5.1). The dated inventory below records the earlier snapshot; behavior is unchanged.

> Status 2026-10-15: this is the crate's migration worklist. The 16 specs below are proved
> through Trust/Clean, then the tests each subsumes are deleted. All non-spec tests from §4
> have been retired or folded into conformance negative controls. No spec has been proved yet.

Source: `crates/aterm-buffer` (read only). Every `#[test]` is mapped
(src/lib.rs 11, tests/{conformance_cursor 1, conformance_eventlog 2, conformance_evict_full 1,
conformance_kernel 2, conformance_snapshot 1, conformance_subscribe 2, conformance_temporal 3,
conformance_transact 2} = 25) along with the code each one reaches. A raw grep
finds 26 `#[test]` strings. The extra one is in the doc comment of `tests/buffer_suite.rs`
("each file keeps its own `#[test]`s"), and that file has no test fn.

Result: **16 specs**, covering **25 tests**. **0 non-spec tests remain** (§4).

## 0. Notation used in the statements

- `MAX` = `MAX_LOG_EVENTS` = `1 << 16`.
- For an `EventLog` `g`:
  - `R(g)` is the ring as a list of `Event`s, oldest first.
  - `T(g)` = `g.total`.
  - `seqs(g)` = `map (.seq.0) R(g)`.
  - `lo(g)` = `T(g) − |R(g)| + 1`, the oldest live seq.
- `LogReach`: the logs reachable from `EventLog::default()` by `append_at` calls, under the
  precondition `T < u64::MAX`. Surface's private `append` is `append_at(op, Ticks(0))`. At
  `u64::MAX` the `saturating_add` would repeat a seq, but 2^64 appends cannot happen.
- For a `Surface` `s`:
  - `L(s)` is the abstract line list `[(LineId, String)]`, which is the
    `Arc<Vec<(LineId, Arc<String>)>>` with both `Arc`s dereferenced.
  - `N(s)` = `next_line`.
  - `G(s)` = `*s.log`.
- `Reach`: the surfaces reachable from `Surface::new()` by `apply` and `transact`, and by
  `clone`/`snapshot` of a reachable surface.
- `M(L, N, e)`, the model step for `apply`:
  - `AppendLine(t)` gives `(L ++ [(LineId(N), t)], N+1)`.
  - `SetLine(id, t)` replaces the text of the entry whose id is `id` with `t`, if that entry
    exists, and otherwise leaves `L` unchanged. `N` is unchanged.
  - `ClearLine(id)` is `SetLine(id, "")`.
- `op(e)` is `Append(LineId(N))`, `Write(id)` or `Clear(id)`, by the variant of `e`.
- `sorted(L)`: the ids in `L` strictly ascend, and all are `< N`.
- `lb(id)` = `line_lower_bound(id)`.
- **Reads are not events.** `read_text`, `snapshot`, `subscribe`, `poll`, `log` and `seq` take
  `&self` over `Arc` data with no interior mutability. That is a type-level fact, not a spec.
- The kernel-family models in `aterm_spec::derive` (`kernel_model`, `ring_model`,
  `evict_full_model`, `cursor_model`, `subscribe_model`, `snapshot_model`, `transact_model`) are
  scalar projections of these views. The first 15 specs are code-level statements.
  `refines-kernel-family-models` is the model-refinement statement that the conformance tests
  sample today.

## 1. Specs (TSV)

Columns: `id`, `kind`, `functions`, `statement`, `subsumes (primary tests; secondary in §2)`,
`difficulty (Rust features the proof must handle)`, `blocking gaps (audit §7 numbers)`.

Gap 0 (an installed seal that publishes Clean rows) blocks every row, so it is listed once here
and left out of the column. No spec is blocked by gap 0 alone. Every one needs at least gaps
1 and 2 (non-scalar carriers and certified `&self`/`&mut self` views). §6 says how to cut
scalar cores out of the first few, so that they can be proved before those gaps close.

```tsv
id	kind	functions	statement	subsumes	difficulty	blocking gaps
append-at-contract	contract	EventLog::{append_at,total,head}	∀ g∈LogReach, op, ts: let (seq, ev) = g.append_at(op, ts), with g' the log afterwards. Then seq = Seq(T(g)+1), T(g') = T(g)+1 and g'.head() = seq. If |R(g)| < MAX: ev = None and R(g') = R(g) ++ [Event{seq, op, ts}]. If |R(g)| = MAX: ev = Some(R(g)[0]) and R(g') = tail(R(g)) ++ [Event{seq, op, ts}]. The stored event carries exactly the op and ts it was given.	conformance_temporal::append_at_records_seq_tick_and_op, conformance_kernel::real_append_at_spine_conforms_to_kernel_model	MEDIUM: &mut self, VecDeque push_back/pop_front/len/capacity/reserve_exact, Option<Event>, enum Op payloads, tuple return, saturating_add	1, 2, 4
log-window-invariant	invariant	EventLog::{append_at,live,oldest_live,newest_live,head}	∀ g∈LogReach: |R(g)| = min(T(g), MAX); seqs(g) = [lo(g), …, T(g)], ascending and contiguous, so n∈1..=T(g) is live ⇔ lo(g) ≤ n (EvictOldestContiguous); head() = Seq(T(g)); T(g) > 0 ⇒ newest_live().seq = head() ∧ oldest_live().seq = Seq(lo(g)); T(g) = 0 ⇒ both are None; live() yields R(g) in order. Preserved by append_at (by append-at-contract). Corollaries: T ≤ MAX ⇒ lo = 1; T > MAX ⇒ lo = T−MAX+1.	conformance_eventlog::real_eventlog_conforms_to_ring_spec, conformance_eventlog::real_eventlog_eviction_conforms_to_ring_spec, conformance_evict_full::real_eventlog_live_set_is_contiguous_window, conformance_temporal::dropping_the_evicted_event_loses_seqs_control	MEDIUM: a type invariant over a private VecDeque<Event> plus a u64 counter, a quantified clause over ring contents, &self/&mut self views	1, 2, 5
no-silent-loss	round-trip	EventLog::{append_at,live}	∀ n < u64::MAX, ∀ inputs [(op_i, ts_i) | i∈1..=n]: start from EventLog::default(), make the n append_at calls in order, and collect each Some(ev) into spilled. Then spilled ++ R(g) = [Event{seq: Seq(i), op: op_i, ts: ts_i} | i∈1..=n], so order is kept and no seq is lost or duplicated, and |spilled| = n.saturating_sub(MAX). This is NoSilentLoss for a host that tiers every returned event. Induction on n over append-at-contract.	conformance_temporal::append_at_spills_every_evicted_event_no_silent_loss	MEDIUM: induction over a call sequence, Vec/VecDeque content equality, enum payloads	1, 2, 5, 7
ring-capacity-bound	invariant	EventLog::append_at	∀ g∈LogReach: g.ring.len() ≤ MAX ∧ g.ring.capacity() ≤ MAX, so the deque never exceeds MAX elements or reallocates past MAX when it fills (append_at pops the oldest element before pushing when len ≥ MAX).	src/lib.rs::event_log_ring_does_not_double_past_max	EASY: pop_front before push_back keeps len ≤ MAX at every push	1, 2, 11
op-event-slot-sizes	invariant	Op, Event	size_of::<Op>() = 16 ∧ size_of::<Event>() = 32, so a full ring of MAX_LOG_EVENTS (65,536) events occupies 2 MiB of slot storage.	src/lib.rs::op_and_event_keep_their_slot_sizes	TRIVIAL: const evaluation of std::mem::size_of on concrete reprs	1
live-after-is-filter	equivalence	EventLog::{live_after,live}	∀ g satisfying log-window-invariant, ∀ after: Seq (0 and u64::MAX included): live_after(after) yields exactly live().filter(|e| e.seq > after), in the same order. Hence after < lo(g) ⇒ all of R(g), and after ≥ T(g) ⇒ nothing. The start index after+1−lo is clamped to [0, |R|] without overflow or panic.	src/lib.rs::live_after_seek_matches_the_filter_and_the_ends_are_the_extremes	EASY-MEDIUM: returns impl Iterator over VecDeque::range, usize::try_from, saturating arithmetic, min	1, 2, 3, 5
log-forget	contract	EventLog::{forget,append_at,total,head,live}	∀ g∈LogReach: after g.forget(), live() is empty, T and head() are unchanged, and the next append_at(op, ts) returns (Seq(T(g)+1), None) and leaves R = [Event{seq, op, ts}]. So a forgotten log evicts nothing and never reissues a seq.	src/lib.rs::a_forgotten_log_keeps_no_event_and_counts_on_from_its_head	LOW: &mut self, VecDeque replacement	1, 2
ts-monotone-newest-is-max	invariant	EventLog::{append_at,newest_live}	∀ g∈LogReach built by append_at calls whose ts arguments are nondecreasing: the ts values along R(g) are nondecreasing. Hence R(g) ≠ [] ⇒ newest_live().ts = max over live() of e.ts.	(secondary only: src/lib.rs::live_after_seek_matches_the_filter_and_the_ends_are_the_extremes)	MEDIUM: an invariant conditioned on the caller's history (a ghost last-ts), a quantified clause over ring contents	1, 2, 5
apply-refines-line-model	refinement	Surface::{apply,line_index,seq}	∀ s∈Reach (so sorted(L(s))), ∀ e: Edit: let q = s.apply(c, e), with s' the surface afterwards. Then (L(s'), N(s')) = M(L(s), N(s), e) and sorted(L(s')); G(s') follows append-at-contract with op(e) at Ticks(0); q = s'.seq() = Seq(T(G(s))+1). Exactly one event is recorded per apply, also when SetLine/ClearLine name an absent id, which records the event and leaves the lines unchanged. Corollary along any run: s.seq().0 = the number of edits applied (SeqIsCount).	src/lib.rs::apply_read_round_trips	MEDIUM: &mut self, an enum match with String payloads, Arc::make_mut on Arc<Vec<..>> and Arc<EventLog>, Vec push and index assignment, String comparison, calls to line_index and append	1, 2, 3, 4
line-lower-bound	contract	Surface::{line_lower_bound,line_index,line_range_bounds}	∀ s with sorted(L(s)), ∀ id: lb(id) = the least i∈[0, |L|] with i = |L| ∨ L[i].0 ≥ id; line_index(id) = Some(i) ⇔ L[i].0 = id, else None; line_range_bounds(r) = (lb(r.start), lb(r.end)) if r.start < r.end, else (lb(r.start), lb(r.start)). The loop runs at most ⌊log2 |L|⌋+1 iterations (variant: high−low at least halves each iteration), and its get(mid) miss arm is unreachable.	src/lib.rs::narrow_tail_line_operations_do_logarithmic_work	MEDIUM: a while loop with an invariant and a logarithmic decreases bound, slice get, &self over Arc<Vec<(LineId, Arc<String>)>>, Ord on a newtype	1, 2, 3, 7
read-text-contract	contract	Surface::read_text, Snapshot::read_text	∀ s with sorted(L(s)), ∀ r: s.read_text(c, r) = TextWithOrigin{text, origin: OriginTag::Source, seq: s.seq()}, where text = concat [t ++ "\n" | (id, t) ← L(s), r.start ≤ id < r.end]. An empty or inverted range (r.end ≤ r.start) yields "". Snapshot::read_text(c, r) = the inner surface's read_text(c, r).	(secondary only: src/lib.rs::apply_read_round_trips, narrow_tail_line_operations_do_logarithmic_work, transact_is_atomic_and_cc_guarded)	MEDIUM: a for loop over a slice, String push_str/push, a struct return, calls to line_range_bounds	1, 2, 3, 4, 7
snapshot-isolation	contract	Surface::{snapshot,apply,transact}, Snapshot::read_text	∀ s0∈Reach, let snap = s0.snapshot(c). Then snap.at = s0.seq(), and after any later apply/transact calls on s0, or on a clone of s0 or of snap's surface, ∀ r: snap.read_text(c, r) = s0.read_text(c, r) as evaluated at snapshot time. The snapshot's L, N and G are frozen. This is value semantics of Clone combined with Arc::make_mut (a writer never mutates through a shared Arc): SnapshotIsolated, leaked = 0.	conformance_snapshot::real_snapshot_isolated_from_later_writes	MEDIUM-HARD: derived Clone over Arc fields, Arc::make_mut copy-on-write, a two-state statement across later &mut calls on aliased values	1, 2, 4, 5
cow-sharing	contract	Surface::{apply,snapshot}	∀ s∈Reach: snapshot (and clone) share both spines (Arc::ptr_eq on lines and on log). ∀ e, with s' = s after apply(c, e): Arc::ptr_eq(s.lines, s'.lines) whenever the lines Arc was shared and L(s') = L(s), which covers SetLine to equal text, ClearLine of an empty line, and Set/Clear of an absent id. The log Arc is always detached when it was shared, because exactly one event is appended. Other holders of the old Arcs stay pointer-equal among themselves. AppendLine(t), and SetLine(id, t) with changed text, store t by move: the stored Arc<String>'s buffer pointer and capacity equal t's. A replaced line gets a fresh Arc that is not ptr_eq to the snapshot's.	snapshot_shares_the_line_and_log_spines_until_a_line_write, absent_line_edits_append_an_event_without_detaching_the_line_spine, equal_line_edits_advance_the_log_without_detaching_the_line_spine	HARD: Arc pointer identity and refcount (make_mut clones iff strong count > 1), String heap pointer/capacity, std unsafe internals of Arc and String	1, 2, 4, 8
poll-contract	contract	Surface::{subscribe,poll}	∀ s∈Reach: s.subscribe(c) = Cursor{at: s.seq()}. ∀ cursor k with k.at ≤ s.seq(), let h = s.seq() and lo = lo(G(s)). If R ≠ [] ∧ lo > k.at+1: poll(k) = (Gap{resync_to: h}, Cursor{at: h}). Otherwise poll(k) = (Events(clones of live_after(k.at)), Cursor{at: h}), which by log-window-invariant and live-after-is-filter is exactly seqs k.at+1..=h in order (count h − k.at). A caught-up cursor (k.at = h) gets Events([]), never a Gap. The returned cursor never passes the head (CursorBounded). Since seq is monotone (apply-refines-line-model), every cursor from subscribe/poll on s or its later states keeps k.at ≤ seq, so every lost event surfaces as a Gap.	src/lib.rs::subscribe_pulls_new_events_then_drains, conformance_subscribe::real_poll_not_behind_delivers_matching_model, conformance_subscribe::real_poll_behind_gaps_no_silent_loss, conformance_cursor::real_subscription_cursor_conforms_to_cursor_model	MEDIUM: &self, iterator next on VecDeque, a for loop pushing clones into a Vec, an enum with a Vec payload, a private-field Cursor, saturating_add	1, 2, 3, 4, 7
transact-occ	contract	Surface::transact	∀ s∈Reach, base: Seq, body: Vec<Edit>: s.seq() ≠ base ⇒ transact returns Conflict and s is entirely unchanged (L, N and G identical, so no lost update). s.seq() = base ⇒ the result s' = foldl (apply c) s body, and transact returns Committed(s'.seq()) with s'.seq().0 = base.0 + |body| (by apply-refines-line-model).	src/lib.rs::transact_is_atomic_and_cc_guarded, conformance_transact::real_transact_clean_commit_matches_model, conformance_transact::real_transact_conflict_aborts_no_lost_update	EASY given apply-refines-line-model: &mut self, a for loop consuming a Vec<Edit>, callee-ensures composition from apply, an enum return	1, 2, 4, 7
refines-kernel-family-models	refinement	Surface::{apply,transact,poll,snapshot}, EventLog::append_at	Each real transition, projected onto a model's variables, is a step of the matching aterm_spec::derive model, with the model's constants set to the real ones (Cap = MAX, MaxSeq lifted): (seq, count) → kernel_model Emit, SeqIsCount; (seq, lo) → ring_model Push; the live set → evict_full_model EvictOldestContiguous; (seq, cursor) → cursor_model Grow/Deliver, CursorBounded; (seq, lo, cursor, lost) → subscribe_model, PollGap ⇔ lo > cursor+1; (seq, snapped, leaked) → snapshot_model SnapshotIsolated; (seq, tbase, active, lost) → transact_model, CommitClean ⇔ seq = tbase, else Abort. Follows from append-at-contract, log-window-invariant, apply-refines-line-model, snapshot-isolation, poll-contract and transact-occ. The model-side facts (the guards, and Buggy admitting the defect) belong to aterm-spec's Tier-0 checks.	conformance_kernel::real_surface_spine_conforms_to_kernel_model	MEDIUM once the code specs hold: model-to-code refinement over projections of heap state (ty_model! models bound to Rust data), cross-crate model citation	1, 2, 10, 12
```

### Distribution by kind (16)

| kind | count | ids |
|---|---|---|
| contract | 8 | append-at-contract, log-forget, line-lower-bound, read-text-contract, snapshot-isolation, cow-sharing, poll-contract, transact-occ |
| invariant | 4 | log-window-invariant, ring-capacity-bound, op-event-slot-sizes, ts-monotone-newest-is-max |
| refinement | 2 | apply-refines-line-model, refines-kernel-family-models |
| equivalence | 1 | live-after-is-filter |
| round-trip | 1 | no-silent-loss |

### Proof dependency order

`append-at-contract` → `log-window-invariant` → {`no-silent-loss`, `ts-monotone-newest-is-max`,
`live-after-is-filter`, `ring-capacity-bound`}. `line-lower-bound` → {`apply-refines-line-model`,
`read-text-contract`} → `transact-occ`. `log-window-invariant` + `live-after-is-filter` +
`apply-refines-line-model` → `poll-contract`. `apply-refines-line-model` → {`snapshot-isolation`,
`cow-sharing`}. All of the above → `refines-kernel-family-models`.

### Blocking-gap histogram (specs blocked by each audit §7 gap)

| gap | specs |
|---|---|
| 0 (seal) | 16 |
| 1 (non-scalar carriers) | 16 |
| 2 (`&self`/`&mut self` views) | 15 |
| 3 (E6 body widening) | 5 |
| 4 (callee-ensures composition) | 7 |
| 5 (quantified clauses) | 5 |
| 7 (loops) | 5 |
| 8 (effects/unsafe) | 1 |
| 10 (model→code refinement) | 1 |
| 11 (generics) | 1 |
| 12 (cross-crate citation) | 1 |

## 2. Test → spec map (TSV)

`primary` is the spec that retires the test. `also` lists the other specs the same test samples.

```tsv
file	test	primary	also
src/lib.rs	apply_read_round_trips	apply-refines-line-model	read-text-contract
src/lib.rs	snapshot_shares_the_line_and_log_spines_until_a_line_write	cow-sharing	snapshot-isolation
src/lib.rs	absent_line_edits_append_an_event_without_detaching_the_line_spine	cow-sharing	apply-refines-line-model
src/lib.rs	equal_line_edits_advance_the_log_without_detaching_the_line_spine	cow-sharing	apply-refines-line-model
src/lib.rs	narrow_tail_line_operations_do_logarithmic_work	line-lower-bound	read-text-contract (§5.5)
src/lib.rs	subscribe_pulls_new_events_then_drains	poll-contract	
src/lib.rs	event_log_ring_does_not_double_past_max	ring-capacity-bound	(§5.2)
src/lib.rs	op_and_event_keep_their_slot_sizes	op-event-slot-sizes	ring-capacity-bound
src/lib.rs	live_after_seek_matches_the_filter_and_the_ends_are_the_extremes	live-after-is-filter	ts-monotone-newest-is-max
src/lib.rs	a_forgotten_log_keeps_no_event_and_counts_on_from_its_head	log-forget	append-at-contract, live-after-is-filter
src/lib.rs	transact_is_atomic_and_cc_guarded	transact-occ	read-text-contract
tests/conformance_cursor.rs	real_subscription_cursor_conforms_to_cursor_model	poll-contract	refines-kernel-family-models
tests/conformance_eventlog.rs	real_eventlog_conforms_to_ring_spec	log-window-invariant	refines-kernel-family-models
tests/conformance_eventlog.rs	real_eventlog_eviction_conforms_to_ring_spec	log-window-invariant	refines-kernel-family-models
tests/conformance_evict_full.rs	real_eventlog_live_set_is_contiguous_window	log-window-invariant	refines-kernel-family-models
tests/conformance_kernel.rs	real_surface_spine_conforms_to_kernel_model	refines-kernel-family-models	apply-refines-line-model, log-window-invariant, transact-occ
tests/conformance_kernel.rs	real_append_at_spine_conforms_to_kernel_model	append-at-contract	log-window-invariant, refines-kernel-family-models
tests/conformance_snapshot.rs	real_snapshot_isolated_from_later_writes	snapshot-isolation	refines-kernel-family-models (§5.3)
tests/conformance_subscribe.rs	real_poll_not_behind_delivers_matching_model	poll-contract	refines-kernel-family-models (§5.4)
tests/conformance_subscribe.rs	real_poll_behind_gaps_no_silent_loss	poll-contract	refines-kernel-family-models (§5.4)
tests/conformance_temporal.rs	append_at_records_seq_tick_and_op	append-at-contract	
tests/conformance_temporal.rs	append_at_spills_every_evicted_event_no_silent_loss	no-silent-loss	append-at-contract
tests/conformance_temporal.rs	dropping_the_evicted_event_loses_seqs_control	log-window-invariant	no-silent-loss (§5.7)
tests/conformance_transact.rs	real_transact_clean_commit_matches_model	transact-occ	refines-kernel-family-models
tests/conformance_transact.rs	real_transact_conflict_aborts_no_lost_update	transact-occ	refines-kernel-family-models
```

## 3. Coverage

- 25 live tests mapped across the 16 specs (0 non-spec tests remain in §4).
- The biggest collapses are `poll-contract` (4 primary), `log-window-invariant` (4), `cow-sharing` (3)
  and `transact-occ` (3).
- `read-text-contract` and `ts-monotone-newest-is-max` have no primary test. Each is sampled only
  as a side assertion of another spec's test. They are still needed as lemmas: `read-text-contract`
  for `snapshot-isolation`, and `ts-monotone-newest-is-max` for the timestamp half of
  `live_after_seek_…`.
- `refines-kernel-family-models` has 1 primary test, but it is the secondary spec of 10 conformance
  tests. Once the code-level specs are proved, it is what lets those tests' `ty_model!` bindings go.

## 4. Tests that are not spec samples

Status 2026-10-28: 0 standalone non-spec tests remain; `conformance_kernel::a_gapped_append_is_the_buggy_step_the_bind_rejects` was folded into `real_append_at_spine_conforms_to_kernel_model` as its `kernel_model` negative control.

## 5. Implied specs that look wrong, or disagree with the code

1. **(Fixed 2026-10-16.) The crate doc's invariant was stale.** `src/lib.rs` now states the
   invariant in terms of `log.total()`, the logical length across eviction (`apply-refines-line-model`'s
   `SeqIsCount`).
2. **(Fixed 2026-10-28.) `EventLog::append_at` pushed before popping (`len = MAX + 1`) and relied
   on `reserve_exact(1)`.** `append_at` now pops the oldest entry before pushing whenever
   `ring.len() >= MAX_LOG_EVENTS`, so `ring.len()` never exceeds `MAX_LOG_EVENTS`, the 2 MiB
   `65,536 -> 65,537` reallocation is eliminated, and `capacity ≤ MAX` holds directly.
3. **(Fixed 2026-10-28.) `conformance_snapshot` checked a tautology.** `conformance_snapshot` now
   projects `snapped` and `leaked` from `Snapshot::read_text` vs the captured text across `Write`,
   `Snap` and post-snapshot `Write` transitions (`validate_transition_tiered`), with a `Buggy = 1`
   negative control.
4. **(Fixed 2026-10-28.) `conformance_subscribe` projected the test's cursor, not the code's.**
   Both tests now read the before/after cursor positions back through `poll`, derive `lost` from the
   returned `SubUpdate`, validate `PollDeliver`/`PollGap` transitions (`validate_transition_tiered`),
   and include the `Buggy = 1` silent-loss negative control.
5. **(Fixed 2026-10-28.) `narrow_tail_line_operations_do_logarithmic_work` now enforces the exact
   `⌈log2(n+1)⌉` binary-search bound** (`(n + 1).next_power_of_two().ilog2()` per `line_lower_bound`
   call, `2 * bound` for `read_text` and `bound` for `SetLine`) across `n ∈ {1, 2, 3, 7, 8, 15, 16,
   1023, 1024, 4096}`. The `#[cfg(test)]` comparison counter retires with the test once
   `line-lower-bound` is Certified.
6. **(Fixed 2026-10-28.) Three test files restated the cap.** `conformance_eventlog.rs`,
   `conformance_evict_full.rs` and `conformance_subscribe.rs` now import
   `aterm_buffer::MAX_LOG_EVENTS` directly.
7. **A "control" that is a positive sample.** `dropping_the_evicted_event_loses_seqs_control` is
   called a negative control, but it asserts only that, after `MAX + 1000` appends, the live window
   starts at seq 1001 and holds `MAX` events. That is `log-window-invariant`'s corollary
   `lo = T − MAX + 1`. The "loss" it illustrates is the host's, and `no-silent-loss` states the
   positive side of it: the returned `Some(ev)` is the only remaining copy.
8. **The raw test count is 26, and there are 25 live tests.** `tests/buffer_suite.rs` mentions
   `#[test]` only in a doc comment. A naive substring grep over-counts this crate by one.

## 6. The three specs I would prove first (easiest, high value)

Every spec here waits on gaps 0, 1 and 2, because `EventLog` and `Surface` are a `VecDeque`, an
`Arc<Vec<…>>` and `&mut self` throughout. The way to start before those gaps close is to cut
the scalar core out of each spec into a pure `fn` over `u64`/`usize`. Each core is then blocked
only by gap 0 (and gap 3 for `min`/`saturating_*`). The code-level spec is then proved over it
once gaps 1 and 2 land.

1. **`append-at-contract` with `log-window-invariant`.** The scalar core is the step on
   `(T, |R|)`: `T' = T+1` and `|R'| = min(|R|+1, MAX)`, with `ev.is_some() ⇔ |R| = MAX`. Together
   with `lo = T − |R| + 1`, this is linear arithmetic (`omega`). The full statements retire 6
   tests as primary (the 2 temporal/kernel `append_at` tests and the 4 eventlog/evict_full window
   tests), and every other spec builds on them.
2. **`live-after-is-filter`.** The scalar core is the seek-index function
   `min(after.saturating_add(1).saturating_sub(lo), |R|)`, proved in range and equal to the count
   of live seqs `≤ after`. Factor it out of `live_after` as a named `fn`. It retires the
   `live_after_seek_…` test and is the half of `poll-contract` (4 tests) that says which events
   are delivered. `poll-contract`'s other half, the Gap predicate `R ≠ [] ∧ lo > at+1`, is
   another scalar fn of the same kind.
3. **`transact-occ`**, via `apply-refines-line-model`'s seq clause. Its control flow is one
   `seq != base` branch and a fold of `apply`. The scalar core (Conflict leaves `seq` unchanged;
   Committed gives `base + |body|`) is a callee-ensures composition (gap 4) over SeqIsCount. It
   retires 3 tests, including both transact conformance tests.

Runner-up: **`ring-capacity-bound`**. With the §5.2 pop-before-push fix in `record`
(`VecDeque::with_capacity(MAX_LOG_ENTRIES)` and `if len == MAX { pop_front }`), `len ≤ MAX` holds at
every step as a corollary of `log-window-invariant` without depending on std growth policy.
Defer **`cow-sharing`** (gap 8: `Arc`/`String` internals) and **`refines-kernel-family-models`**
(gaps 10 and 12) to the end.
