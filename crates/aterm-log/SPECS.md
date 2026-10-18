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
level-severity-order	table-property	Level (derived PartialOrd/Ord), LevelFilter (derived PartialOrd/Ord)	∀ a,b ∈ Level: a < b ⇔ (a as usize) < (b as usize), with Error=1 < Warn=2 < Info=3 < Debug=4 < Trace=5. The same holds for LevelFilter, with Off=0 below every other variant. ∀ l ∈ Level: (l as usize) = (lf(l) as usize). This correspondence is what makes __log's `level as usize > MAX_LEVEL` compare like with like. The domain is finite (5×5 and 6×6), so the property is decidable by evaluation.	-	EASY: fieldless #[repr(usize)] enums, derived PartialOrd (discriminant_value), `as usize` casts	1, 3
level-display-names	table-property	<Level as Display>::fmt, LevelFilter::parse	∀ l ∈ Level: l.to_string() = ascii_uppercase(NAMES[lf(l)]), i.e. "ERROR","WARN","INFO","DEBUG","TRACE". fmt writes exactly that string with one write_str and returns that call's result. Round trip: ∀ l: LevelFilter::parse(&l.to_string()) = Some(lf(l)).	test_level_display	MEDIUM: &mut fmt::Formatter, a Display trait impl, &str literals, String from to_string; the round trip composes with parse	1, 2, 3, 4
level-filter-parse	contract	LevelFilter::parse	∀ s: &str, ∀ f ∈ LevelFilter: parse(s) = Some(f) ⇔ s.trim().eq_ignore_ascii_case(NAMES[f]); parse(s) = None ⇔ no f satisfies that. The names are pairwise distinct ignoring ASCII case, so the result is well defined. trim strips Unicode White_Space at both ends (§5.4); the match is ASCII-case-insensitive only.	-	MEDIUM: &str, str::trim (a char loop over Unicode whitespace), eq_ignore_ascii_case (a byte loop), an if-chain, Option<enum>	1, 3, 5, 7
should-rotate-threshold	contract	should_rotate	∀ len: u64: should_rotate(len) ⇔ len > MAX_LOG_BYTES = 4·1024·1024 = 4194304.	(none; its test went when the spec was proved)	TRIVIAL: one u64 comparison against a const; a straight-line admissible body	0
record-cap-by-level	contract	record_cap	∀ l ∈ Level: record_cap(l) = if l ≤ Level::Warn then MAX_ALERT_RECORD_BYTES (1024) else MAX_RECORD_BYTES (512). Hence record_cap(l) ≤ MAX_ALERT_RECORD_BYTES, which is sanitize_capped's precondition.	(shared: test_warn_and_error_keep_the_longer_cap)	EASY: an enum match with or-patterns, usize consts	1, 3
sanitize-record	contract	sanitize_capped, sanitize_record, sanitize_record_for	∀ msg: &str, ∀ cap ≤ MAX_ALERT_RECORD_BYTES, let r = sanitize_capped(msg, cap). (a) Value: r.as_ref() = San(msg, cap). (b) Borrowing: r is Cow::Borrowed ⇔ (∀ c ∈ msg.chars(): ¬ctl(c)) ∧ msg.len() ≤ cap, and then r = Borrowed(msg), the same pointer. (c) Consequences of (a), stated for the proof: r has no control chars; r.len() ≤ cap + 3; if r ends with an added '…' then M(msg).len() > cap and r.len() > cap − 3; every char of r except an added final '…' is repl of the char at the same index of msg, so no code point is split; for Owned, out never outgrows its reservation: len ≤ OUT_CAPACITY = 1028 and capacity is unchanged. Wrappers: sanitize_record(m) = sanitize_capped(m, MAX_RECORD_BYTES); sanitize_record_for(l, m) = sanitize_capped(m, record_cap(l)).	test_warn_and_error_keep_the_longer_cap, test_sanitize_record_replaces_control_characters, test_sanitize_record_cap_respects_char_boundaries, test_sanitize_record_control_char_at_boundary_stays_within_reservation	MEDIUM-HARD: &str and Cow<'_, str> carriers; a chars() loop with break and the invariant out = Prefix of M over the chars consumed so far ∧ out.len() ≤ cap; String::with_capacity/push/len; char::is_control/len_utf8; saturating_add; usize::min; a call to record_cap (composition); a quantifier over chars	1, 3, 4, 5, 7
record-accessors	contract	Record::{level,target,args,file,line,metadata}, Metadata::level	∀ r = Record{level,target,args,file,line}: r.level() = level, r.target() = target (the same &'a str), r.file() = file, r.line() = line, r.args() points to r.args, and r.metadata().level() = level.	-	EASY once views are certified: &self views on a struct with &'a str, Option<&str>, Option<u32> and fmt::Arguments fields; field projection	1, 2
log-gate	contract	set_max_level, __log	After set_max_level(f), with no intervening set_max_level: MAX_LEVEL holds f as usize, and __log(l, t, a, fi, li) calls logger.log(&Record{l,t,a,fi,li}) exactly once ⇔ Gate(l, f) ∧ a logger L was installed by set_logger ∧ L.enabled(&Metadata{level: l}). Otherwise it calls neither log nor anything else, except that enabled is called when Gate holds and a logger is installed. No current test samples the gate itself.	(none; §4)	HARD: AtomicUsize store/load (Relaxed), OnceLock get, a dyn Log trait-object call; __log carries trust::skip today	2, 8, 13
env-mutators	contract	env::{take,unset,set,env_lock,remove_locked,set_locked}	Sequential semantics on Env E, ∀ k, v: take(k) returns E(k) and leaves E' = E\k, so a second take(k) returns None (one-shot). unset(k) leaves E' = E\k, whether or not k was set. set(k,v) leaves E' = E[k:=v]. All are linearizable with respect to each other and to scoped/scoped_unset, because each holds ENV_LOCK for its whole effect. A poisoned lock is recovered, not propagated: none of them panics on a poisoned lock.	take_reads_and_removes_in_one_step	HARD: process environment (unsafe set_var/remove_var, var_os), a static Mutex and MutexGuard, poison recovery, impl AsRef<OsStr> generics, OsStr/OsString	1, 8, 11, 13
env-scoped-restore	contract	env::{scoped,scoped_unset,scoped_opt}, Restore::drop	∀ k, v: Option<OsStr>, body: let E0 be the environment on entry. body runs under E0[k:=v], or under E0\k when v = None. If body returns x, scoped_opt returns x. If body panics, the panic propagates unchanged. In both cases, on exit E'(k) = E0(k): a present value is set back, and an absent key is removed, never set to an empty value. Keys other than k are left as body left them. ENV_LOCK is held from before the override until after the restore, so no other env:: call can observe the override.	scoped_restores_the_previous_value_and_the_previous_absence, scoped_unset_hides_a_set_key_for_exactly_the_body, scoped_restores_even_when_the_body_panics	VERY HARD: an FnOnce closure (a higher-order spec), unwinding with Drop of a local guard struct (Option::take), a MutexGuard moved into a struct, generics T and impl AsRef<OsStr>, unsafe process-environment effects	1, 8, 11, 13
```

## 2. Test → spec map (TSV; all 23)

`primary` is the spec that retires the test. `also` lists the other specs the same test samples.

```tsv
file	test	primary	also
src/lib.rs	test_level_display	level-display-names	
src/lib.rs	test_warn_and_error_keep_the_longer_cap	sanitize-record	record-cap-by-level
src/lib.rs	test_sanitize_record_replaces_control_characters	sanitize-record	
src/lib.rs	test_sanitize_record_cap_respects_char_boundaries	sanitize-record	
src/lib.rs	test_sanitize_record_control_char_at_boundary_stays_within_reservation	sanitize-record	
src/lib.rs	take_reads_and_removes_in_one_step	env-mutators	
src/lib.rs	scoped_restores_the_previous_value_and_the_previous_absence	env-scoped-restore	env-mutators
src/lib.rs	scoped_unset_hides_a_set_key_for_exactly_the_body	env-scoped-restore	
src/lib.rs	scoped_restores_even_when_the_body_panics	env-scoped-restore	
```

The first 5 rows are `mod tests`; the last 4 are `mod env_tests`.

## 3. Coverage

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

1. **The "stays within reservation" test does not check the reservation.**
   `test_sanitize_record_control_char_at_boundary_stays_within_reservation` (a table over both caps) is
   named for the `String` reservation (`OUT_CAPACITY = 1028`, no realloc), but it only asserts
   `out.len() ≤ cap + 3`. A variant that measured the original char's width instead of the 3-byte
   U+FFFD would still pass, because its result is at most `cap + 2`. `sanitize-record` (c) states
   `len ≤ OUT_CAPACITY` and that the capacity is unchanged.
2. **`test_warn_and_error_keep_the_longer_cap` mixes two specs.** It checks `record_cap`'s table and
   capping by level together, and it asserts `Cow::Borrowed` for a WARN body between the two caps. That
   is right only because the body has no control chars. The borrowing condition is "no control chars
   and `len ≤ cap`", not just `len ≤ cap`.
3. **`sanitize_capped`'s doc understates when the marker is added.** It says the result is "capped near
   MAX_RECORD_BYTES (with a … marker)". The exact rule is that `'…'` is added only when the mapped
   string `M(msg)` exceeds `cap`. Input that has control chars but still fits after U+FFFD expansion is
   `Owned` with no marker. No test covers that branch; `sanitize-record` (a) does.
4. **`LevelFilter::parse` trims Unicode whitespace, not ASCII.** The doc says it ignores "surrounding
   whitespace". `str::trim` strips Unicode White_Space (for example U+3000). `level-filter-parse` states
   the Unicode behaviour. If ASCII-only was intended, the code should use `trim_ascii`.
5. **The env tests read back through test-only code.** They read the environment through `env::read`,
   which is `#[cfg(test)]`-only. The env specs are stated against `std::env::var_os`, so they do not
   depend on that helper.

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
