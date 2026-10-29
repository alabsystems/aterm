<!-- Copyright 2026 Andrew Yates -->
<!-- SPDX-License-Identifier: Apache-2.0 -->

# aterm-log: spec recovery from its 23 unit tests

> Status 2026-10-16, after the merge with the tests-and-gates audit: the rows naming 11 tests it
> deleted are gone, and the spec rows no longer cite those tests. Counts elsewhere in this file
> were taken before.

> Status 2026-10-16: this is the crate's migration worklist. The 10 specs below are each proved
> through Trust/Clean, and then the tests each one subsumes are deleted. The 2 tests in §4 sample no
> spec and can be deleted now.
>
> **Proved: `should-rotate-threshold`** (2026-10-16). `should_rotate` carries
> `ensures result == (len > 4194304) by should_rotate_is_threshold`, a Clean theorem over
> `trust_import_aterm_log__should_rotate`. On Trust seal `150efe4a` the row is Certified
> (`clean-kernel-citation`, kernel-rechecked), and mutating the body to `>=` fails the theorem.
> `test_should_rotate_threshold` is deleted. The machine's installed seal (321aaeda7) withholds
> Clean rows at publication, so the row counts toward the floor only once a newer seal is installed.

Source: `crates/aterm-log/src/lib.rs` (read only). I read every `#[test]` (`mod tests` 18,
`mod env_tests` 5 = 23) and the code each one reaches. Nothing was compiled or run. The claims below
come from reading the code.

Result: **10 specs**, covering **21 tests**. **2 tests are not spec samples** (§4). One spec,
`log-gate`, has no test today. It replaces a test of `#[cfg(test)]`-only code.

## 0. Notation used in the statements

- `NAMES` = `[Off→"off", Error→"error", Warn→"warn", Info→"info", Debug→"debug", Trace→"trace"]`.
- `lf(l)` = the `LevelFilter` variant with the same name as `Level` `l`, so `lf(l) as usize = l as usize`.
- `ctl(c)` = `char::is_control(c)`, the Unicode Cc category: U+0000..U+001F, U+007F, U+0080..U+009F.
- `repl(c)` = `if ctl(c) then U+FFFD else c`. `M(msg)` = the string `msg.chars().map(repl)`.
- `Prefix_cap(s)` = the longest char-boundary prefix of `s` whose UTF-8 length is ≤ `cap`.
- `San(msg, cap)` = `Prefix_cap(M(msg)) ++ (if Prefix_cap(M(msg)) ≠ M(msg) then "…" else "")`. This is
  the value the loop in `sanitize_capped` builds. It pushes `repl(c)` until the next push would exceed
  `cap`, then pushes `'…'` and stops.
- `Gate(l, f)` := `(l as usize) ≤ (f as usize)`. This is the check in `__log` against `MAX_LEVEL`.
- `Env`: the process environment as a partial map `E : OsString ⇀ OsString`, read through
  `std::env::var_os`. `E[k:=v]` updates `k`; `E\k` removes `k`.
- **Linearizable** means that calls into `aterm_log::env` from different threads take effect atomically
  in some total order, because every one of them holds `ENV_LOCK`. It says nothing about `getenv` calls
  made outside the module.
- `blocking_gaps` are the numbers in the gap table of
  the historical development audit `docs/AUDIT-trust-for-test-migration-2026-10-15.md`
  §7. Gap 0 (a seal that publishes Clean rows) is a prerequisite for every spec. It is listed only
  where it is the *only* blocker.

## 1. Specs (TSV)

Columns: `id`, `kind`, `functions`, `statement`, `subsumes (primary tests; secondary in §2)`,
`difficulty (Rust features the proof must handle)`, `blocking_gaps`.

```tsv
id	kind	functions	statement	subsumes	difficulty	blocking_gaps
level-severity-order	table-property	Level (derived PartialOrd/Ord), LevelFilter (derived PartialOrd/Ord)	∀ a,b ∈ Level: a < b ⇔ (a as usize) < (b as usize), with Error=1 < Warn=2 < Info=3 < Debug=4 < Trace=5. The same holds for LevelFilter, with Off=0 below every other variant. ∀ l ∈ Level: (l as usize) = (lf(l) as usize). This correspondence is what makes __log's `level as usize > MAX_LEVEL` compare like with like. The domain is finite (5×5 and 6×6), so the property is decidable by evaluation.	levels_and_filters_order_by_verbosity	EASY: fieldless #[repr(usize)] enums, derived PartialOrd (discriminant_value), `as usize` casts	1, 3
level-display-names	table-property	<Level as Display>::fmt, LevelFilter::parse	∀ l ∈ Level: l.to_string() = ascii_uppercase(NAMES[lf(l)]), i.e. "ERROR","WARN","INFO","DEBUG","TRACE". fmt writes exactly that string with one write_str and returns that call's result. Round trip: ∀ l: LevelFilter::parse(&l.to_string()) = Some(lf(l)).	test_level_display	MEDIUM: &mut fmt::Formatter, a Display trait impl, &str literals, String from to_string; the round trip composes with parse	1, 2, 3, 4
level-filter-parse	contract	LevelFilter::parse	∀ s: &str, ∀ f ∈ LevelFilter: parse(s) = Some(f) ⇔ s.trim().eq_ignore_ascii_case(NAMES[f]); parse(s) = None ⇔ no f satisfies that. The names are pairwise distinct ignoring ASCII case, so the result is well defined. trim strips Unicode White_Space at both ends (§5.4); the match is ASCII-case-insensitive only.	parse_level_filter_takes_names_in_any_case_and_refuses_junk	MEDIUM: &str, str::trim (a char loop over Unicode whitespace), eq_ignore_ascii_case (a byte loop), an if-chain, Option<enum>	1, 3, 5, 7
should-rotate-threshold	contract	should_rotate	∀ len: u64: should_rotate(len) ⇔ len > MAX_LOG_BYTES = 4·1024·1024 = 4194304.	(none; its test went when the spec was proved)	TRIVIAL: one u64 comparison against a const; a straight-line admissible body	0
record-cap-by-level	contract	record_cap	∀ l ∈ Level: record_cap(l) = if l ≤ Level::Warn then MAX_ALERT_RECORD_BYTES (1024) else MAX_RECORD_BYTES (512). Hence record_cap(l) ≤ MAX_ALERT_RECORD_BYTES, which is sanitize_capped's precondition.	record_cap_matches_alert_and_standard_thresholds	EASY: an enum match with or-patterns, usize consts	1, 3; fragment: `record_cap` F1+D2b (VF5 first target, docs/PLAN-tests-to-specs-2026-10-15.md "F1-ready specs").
sanitize-record	contract	sanitize_capped, sanitize_record, sanitize_record_for	∀ msg: &str, ∀ cap ≤ MAX_ALERT_RECORD_BYTES, let r = sanitize_capped(msg, cap). (a) Value: r.as_ref() = San(msg, cap). (b) Borrowing: r is Cow::Borrowed ⇔ (∀ c ∈ msg.chars(): ¬ctl(c)) ∧ msg.len() ≤ cap, and then r = Borrowed(msg), the same pointer. (c) Consequences of (a), stated for the proof: r has no control chars; r.len() ≤ cap + 3; if r ends with an added '…' then M(msg).len() > cap and r.len() > cap − 3; every char of r except an added final '…' is repl of the char at the same index of msg, so no code point is split; for Owned, out never outgrows its reservation: len ≤ OUT_CAPACITY = 1028 and capacity is unchanged. Wrappers: sanitize_record(m) = sanitize_capped(m, MAX_RECORD_BYTES); sanitize_record_for(l, m) = sanitize_capped(m, record_cap(l)).	test_warn_and_error_keep_the_longer_cap, test_sanitize_record_replaces_control_characters, test_sanitize_record_cap_respects_char_boundaries, test_sanitize_record_control_char_at_boundary_stays_within_reservation	MEDIUM-HARD: &str and Cow<'_, str> carriers; a chars() loop with break and the invariant out = Prefix of M over the chars consumed so far ∧ out.len() ≤ cap; String::with_capacity/push/len; char::is_control/len_utf8; saturating_add; usize::min; a call to record_cap (composition); a quantifier over chars	1, 3, 4, 5, 7
record-accessors	contract	Record::{level,target,args,file,line,metadata}, Metadata::level	∀ r = Record{level,target,args,file,line}: r.level() = level, r.target() = target (the same &'a str), r.file() = file, r.line() = line, r.args() points to r.args, and r.metadata().level() = level.	(shared: tests/log_gate.rs::log_gate_and_record_accessors_hold_across_filters_and_logger_decisions)	EASY once views are certified: &self views on a struct with &'a str, Option<&str>, Option<u32> and fmt::Arguments fields; field projection	1, 2
log-gate	contract	set_max_level, set_logger, settle_logger, wait_for_logger, takes_records, __log	After set_max_level(f), with no intervening set_max_level: MAX_LEVEL holds f as usize, and __log(l, t, a, fi, li) calls logger.log(&Record{l,t,a,fi,li}) exactly once ⇔ Gate(l, f) ∧ a logger L was installed by set_logger ∧ L.enabled(&Metadata{level: l}). Otherwise it calls neither log nor anything else, except that enabled is called when Gate holds and a logger is installed. wait_for_logger(within) returns true as soon as LOGGER.get().is_some() ∧ MAX_LEVEL > Off (woken by set_logger and set_max_level), and returns false without waiting further once settle_logger() has run while ¬takes_records() or once within elapses.	tests/log_gate.rs::log_gate_and_record_accessors_hold_across_filters_and_logger_decisions, a_waiter_wakes_once_a_logger_takes_records	HARD: AtomicUsize/AtomicBool, OnceLock, Mutex+Condvar wakeups, a dyn Log trait-object call; __log and wait_for_logger carry trust::skip today	2, 8, 13
env-mutators	contract	env::{take,unset,set,env_lock,remove_locked,set_locked}	Sequential semantics on Env E, ∀ k, v: take(k) returns E(k) and leaves E' = E\k, so a second take(k) returns None (one-shot). unset(k) leaves E' = E\k, whether or not k was set. set(k,v) leaves E' = E[k:=v]. All are linearizable with respect to each other and to scoped/scoped_unset, because each holds ENV_LOCK for its whole effect. A poisoned lock is recovered, not propagated: none of them panics on a poisoned lock.	take_reads_and_removes_in_one_step	HARD: process environment (unsafe set_var/remove_var, var_os), a static Mutex and MutexGuard, poison recovery, impl AsRef<OsStr> generics, OsStr/OsString	1, 8, 11, 13
env-scoped-restore	contract	env::{scoped,scoped_unset,scoped_opt}, Restore::drop	∀ k, v: Option<OsStr>, body: let E0 be the environment on entry. body runs under E0[k:=v], or under E0\k when v = None. If body returns x, scoped_opt returns x. If body panics, the panic propagates unchanged. In both cases, on exit E'(k) = E0(k): a present value is set back, and an absent key is removed, never set to an empty value. Keys other than k are left as body left them. ENV_LOCK is held from before the override until after the restore, so no other env:: call can observe the override.	scoped_restores_the_previous_value_and_the_previous_absence, scoped_unset_hides_a_set_key_for_exactly_the_body, scoped_restores_even_when_the_body_panics	VERY HARD: an FnOnce closure (a higher-order spec), unwinding with Drop of a local guard struct (Option::take), a MutexGuard moved into a struct, generics T and impl AsRef<OsStr>, unsafe process-environment effects	1, 8, 11, 13
log-words	contract	words::{Len,word,WordOrLen,Panic,Panic::of,Panic::of_text,Panic::kind,PanicKind::of,PanicKind::word}	What aterm.log may keep of a text (DESIGN-session-log §3.6, unit E7). ∀ s: Len(s) displays as decimal(s.len()) + "B"; word(s) = s when 0 < s.len() ≤ WORD_MAX (64) and every byte of s is in [A-Za-z0-9@._:=-], else "?"; WordOrLen(s) displays word(s) when that is s, else Len(s). ∀ message m: Panic::of_text(m) displays as PanicKind::of(m).word() + " panic (" + Len(m) + ")", where PanicKind::of reads only m's opening (a closed list: str-slice, index, unwrap-none, unwrap-err, overflow, assert, else other); Panic::of(payload) is Panic::of_text of a &'static str or String payload, and "other panic (0B)" for any other. Law: each Display depends on its text only through its length and that closed kind, so two texts of one length and one kind display alike.	src/words.rs::tests::panic_words_name_the_kind_and_never_the_text	EASY: &str byte loops, str::starts_with over a const table, fmt::Display impls, dyn Any downcast_ref	1, 3
```

## 2. Test → spec map (TSV)

`primary` is the spec that retires the test. `also` lists the other specs the same test samples.

```tsv
file	test	primary	also
src/lib.rs	a_waiter_wakes_once_a_logger_takes_records	log-gate	
src/lib.rs	levels_and_filters_order_by_verbosity	level-severity-order	
src/lib.rs	test_level_display	level-display-names	
src/lib.rs	parse_level_filter_takes_names_in_any_case_and_refuses_junk	level-filter-parse	level-display-names
src/lib.rs	record_cap_matches_alert_and_standard_thresholds	record-cap-by-level	
src/lib.rs	test_warn_and_error_keep_the_longer_cap	sanitize-record	record-cap-by-level
src/lib.rs	test_sanitize_record_replaces_control_characters	sanitize-record	
src/lib.rs	test_sanitize_record_cap_respects_char_boundaries	sanitize-record	
src/lib.rs	test_sanitize_record_control_char_at_boundary_stays_within_reservation	sanitize-record	
src/lib.rs	take_reads_and_removes_in_one_step	env-mutators	
src/lib.rs	scoped_restores_the_previous_value_and_the_previous_absence	env-scoped-restore	env-mutators
src/lib.rs	scoped_unset_hides_a_set_key_for_exactly_the_body	env-scoped-restore	
src/lib.rs	scoped_restores_even_when_the_body_panics	env-scoped-restore	
src/words.rs	panic_words_name_the_kind_and_never_the_text	log-words	
tests/log_gate.rs	log_gate_and_record_accessors_hold_across_filters_and_logger_decisions	log-gate	record-accessors
```

The first 5 rows are `mod tests`; the last 4 are `mod env_tests`.

## 3. Coverage

> Status 2026-10-28 (unit E7): `log-words` (the `words` module) joins with its one test,
> `panic_words_name_the_kind_and_never_the_text`; the counts below are the 2026-10-15 reading.

- 23 tests read. 21 map to one of the 10 specs. 2 are not spec samples (§4).
- The biggest collapse is `sanitize-record` (7 primary). Then come `level-filter-parse` and
  `env-scoped-restore` (3 each).
- `record-cap-by-level` has no primary test. It is a lemma of `sanitize-record`, and two tests sample it
  through `sanitize_record_for`. `log-gate` has no test at all (§4).

## 4. Tests that are not spec samples

> Status 2026-10-15: both tests this section listed (`test_max_level_round_trips` and
> `test_set_logger_error_display`) are deleted, with the `#[cfg(test)]`-only `max_level()` and
> `LEVEL_TEST_LOCK` they alone used. The first read the level back through `max_level()` (the property
> that matters, that `set_max_level` drives `__log`'s gate, is `log-gate`); the second pinned the prose
> of a constant error message. Every test left samples a spec.

## 5. Implied specs that look wrong, or disagree with the code

1. **Reservation check tightened (resolved 2026-10-27).**
   `test_sanitize_record_control_char_at_boundary_stays_within_reservation` now asserts both
   `len ≤ cap + 3` and `capacity == MAX_ALERT_RECORD_BYTES + 4` (`OUT_CAPACITY`, no realloc),
   matching `sanitize-record` (c).
2. **Split `record_cap` and control-char borrowing checks (resolved 2026-10-28).**
   `record_cap_matches_alert_and_standard_thresholds` tests `record-cap-by-level` directly, while
   `test_warn_and_error_keep_the_longer_cap` asserts pointer-equal `Cow::Borrowed` for clean input
   within `MAX_ALERT_RECORD_BYTES` alongside `Cow::Owned` (with no `'…'`) when a control character
   is present within the cap, and `tests/log_gate.rs` directly tests `log-gate` and `record-accessors`.
3. **Marker condition documented and sampled (resolved 2026-10-27).** `sanitize_record`'s doc now
   states that `'…'` is appended only when the sanitized body exceeds the cap, and
   `test_sanitize_record_control_char_at_boundary_stays_within_reservation` covers control-char
   input whose U+FFFD expansion fits `cap` (`Cow::Owned` with no `'…'`).
4. **Unicode whitespace trim documented and sampled (resolved 2026-10-28).** `LevelFilter::parse`
   documents that surrounding Unicode `White_Space` is stripped via `str::trim`, matching
   `level-filter-parse`, and `parse_level_filter_takes_names_in_any_case_and_refuses_junk` covers
   U+3000 padding.
5. **Env tests retargeted onto `std::env::var_os` (resolved 2026-10-28).** Deleted `#[cfg(test)]`
   `env::read` and retargeted all four `env_tests` onto `std::env::var_os`.

## 6. Proof order (easiest high-value first)

1. **`should-rotate-threshold`**. It is the only spec blocked by gap 0 alone: one `u64` comparison
   against a const, a straight-line admissible body. It is the smoke test that a newer seal publishes a
   Certified row for this crate, and it retires 1 test.
2. **`record-cap-by-level`** and **`level-severity-order`** (gaps 1, 3). Both are finite enum tables
   decidable by evaluation once E6 admits enum matches and `as usize` casts. They retire 2 tests, and
   `record-cap-by-level` is the lemma `sanitize-record` needs for `sanitize_record_for`.
3. **`sanitize-record`** (gaps 1, 3, 4, 5, 7). This is the highest-value spec: it retires 7 tests and
   guards every log record against control-char injection and unbounded size. It needs `&str`/`Cow`
   carriers, a `chars()` loop with an invariant, and composition with `record_cap`.
4. **`level-filter-parse`**, then **`level-display-names`** (it composes with parse for the round trip),
   then **`record-accessors`** (gap 2, certified `&self` views). Together they retire 6 tests.
5. **Effects last**: `env-mutators`, `env-scoped-restore` and `log-gate` wait on gaps 8, 11 and 13
   (platform lane, generics, closures and threads). Their pure cores are small. A spec over an abstract
   `Env` map could be stated now, but proving the real bodies needs the platform lane.

Gap histogram (gap → specs blocked by it): 0 → 1 explicitly (all 10 implicitly), 1 → 8, 2 → 3,
3 → 5, 4 → 2, 5 → 2, 7 → 2, 8 → 3, 11 → 2, 13 → 3.
