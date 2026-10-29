<!-- Copyright 2026 Andrew Yates -->
<!-- SPDX-License-Identifier: Apache-2.0 -->

# aterm-time: spec recovery from its 8 unit tests

> Status 2026-10-28: the 2 tests deleted by the tests-and-gates audit and the 4 non-spec tests
> formerly in §4 are gone from the crate and from §2, leaving 8 live `#[test]` fns.

> Status 2026-10-16: this is the crate's migration worklist. Each spec in §1 is to be proved
> through Trust/Clean about the compiled Rust function, and then the tests it subsumes are
> deleted.

Source: `crates/aterm-time` (read only). Every live `#[test]` (`src/lib.rs` 1 and
`src/shim.rs` 7, for 8 in all) and the code each one reaches are mapped below.

Result: **9 specs**, covering **8 tests**. **0 remaining non-spec tests** (§4).

## 0. Notation used in the statements

- `D` = `core::time::Duration`. It is modelled as the natural number of nanoseconds
  `‖d‖ = secs·10^9 + subsec_nanos`, with `‖D::MAX‖ = (2^64−1)·10^9 + 999_999_999`.
  `‖D::from_millis(m)‖ = m·10^6` and `‖D::from_nanos(n)‖ = n`. `D`'s order and `+`/`−` are those of `‖·‖`.
- `Ishim(x)` and `Sshim(x)` are the shim newtypes `shim::Instant(x)` and `shim::SystemTime(x)` over
  `x: D`. `.0` is the field. `UNIX_EPOCH = Sshim(D::ZERO)`.
- `conv(ms)` = `shim::duration_from_millis_f64(ms)` for `ms: f64` (IEEE-754 binary64).
  `trunc` is `f64::trunc`. `fl(a·b)` is the round-to-nearest-even product. `⌊x⌋` is the `as u64` /
  `as u32` cast of a non-negative finite float (truncation, saturating).
- `from_js_millis` = `Instant::from_js_millis` / `SystemTime::from_js_millis`. These are private
  seams. Their production caller is the wasm-only `now()`.
- `now_w` = one read of the platform wall clock: `std::time::SystemTime::now` natively, and `Sshim`
  over `Date.now()` on `wasm32-unknown-unknown`. Effect model: each call of `since_epoch` performs
  exactly one `now_w` read, and the accessors are stated per read.
- **Gap labels.** Numbers are rows of the gap table in
  `AUDIT-trust-for-test-migration-2026-10-15.md` (historical internal audit) §7.
  Gap 0 (a seal that publishes Clean rows) blocks every spec and is not repeated per row. Two
  blockers are not rows of §7:
  - `F`: **an IEEE binary64 model in Trust/Clean.** That means `is_finite`, NaN/−0.0 comparison,
    `trunc`, `−`, `×` with rounding, and the saturating float→int `as` casts. The audit lists floats
    as refused (PLAN "Admissible bodies") but has no row for them. It should become a §7 row.
  - `cfg` (**resolved 2026-10-27**): `mod shim` in `src/lib.rs` is now gated
    `#[cfg(any(all(target_arch = "wasm32", target_os = "unknown"), trust_verify, test))]`, so
    native Trust verify lanes compile `shim.rs` alongside `wasm32-unknown-unknown` and `test`.

## 1. Specs (TSV)

Columns: `id`, `kind`, `functions`, `statement`, `subsumes (primary tests; secondary in §2)`,
`difficulty (Rust features the proof must handle)`, `blocking gaps`.

```tsv
id	kind	functions	statement	subsumes	difficulty	blocking gaps
time-conv-total-floor	no-panic + contract	shim::duration_from_millis_f64	∀ ms: f64 (NaN, ±∞, ±0, subnormals and f64::MAX included): conv(ms) does not panic, and (¬ms.is_finite() ∨ ms ≤ 0.0) ⇒ conv(ms) = D::ZERO.	shim::millisecond_conversion_is_total	EASY once floats exist: two f64 predicates (is_finite, `<=` with NaN and −0.0), an early return, a Duration struct return; no loops. The no-panic half also needs time-conv-exact's bound that the cast operands are in range.	1, 3, 6, F, cfg
time-conv-exact	contract	shim::duration_from_millis_f64	∀ finite ms > 0.0: let w = trunc(ms) and f = ms − w (exact in IEEE, 0 ≤ f < 1). Then ‖conv(ms)‖ = M·10^6 + ⌊fl(f·10^6)⌋, where M = u64::MAX if w ≥ 2^64 else ⌊w⌋. Also ⌊fl(f·10^6)⌋ < 10^6, so the final saturating_add never saturates. Lemmas: (a) ms = k integral with k < 2^64 ⇒ conv(ms) = D::from_millis(k); (b) ms ≥ 2^64 ⇒ conv(ms) = D::from_millis(u64::MAX) (then f = 0); (c) ms = k + f with k integral and f·10^6 exactly representable ⇒ ‖conv(ms)‖ = k·10^6 + f·10^6.	shim::millisecond_conversion_keeps_the_fraction	MEDIUM: f64 trunc/sub/mul with rounding, f64→u64 and f64→u32 saturating casts, the `u64::MAX as f64` comparison (it rounds to 2^64), and core calls Duration::from_millis/from_nanos/saturating_add. The `< 10^6` bound is a real rounding fact: for f = 1 − 2^-53, fl(f·10^6) is the float just below 10^6, not 10^6.	1, 3, 4, 6, 12, F
time-conv-monotone-resolution	contract (quantified)	shim::duration_from_millis_f64	(monotone) ∀ finite ms1 ≤ ms2: conv(ms1) ≤ conv(ms2). (accuracy) ∀ finite 0 < ms < 2^64: |‖conv(ms)‖ − ms·10^6| < 1 ns, where ms is read as its exact real value. So the error does not grow with the integer part. (resolution) ∀ finite 0 < a < b < 2^64: ‖conv(b)‖ − ‖conv(a)‖ > (b − a)·10^6 − 2, with b − a the exact real difference. At a = 3.6e6 and b = fl(a + 0.1), this gives > 99_997 ns.	shim::resolution_survives_an_aged_time_origin	MEDIUM: two binders and real-valued reasoning over time-conv-exact's formula. Monotone across trunc boundaries uses ⌊fl(f·10^6)⌋ < 10^6. It is FALSE at +∞, so `finite` is part of the spec (see §5.1).	1, 3, 5, 6, F
time-from-js-millis	refinement	shim::Instant::from_js_millis, shim::SystemTime::from_js_millis	∀ ms: f64: Instant::from_js_millis(ms) = Ishim(conv(ms)) and SystemTime::from_js_millis(ms) = Sshim(conv(ms)). So equal readings give equal values, and every concrete reading in the shim tests reduces to time-conv-exact.	-	TRIVIAL: tuple-struct construction around one call to an admitted fn.	1, 3, 4, cfg
time-instant-difference	contract	shim::Instant::{duration_since,checked_duration_since,saturating_duration_since}, <shim::Instant as Sub<Instant>>::sub, derived PartialEq/Eq/PartialOrd/Ord	∀ a, b: Instant: a.checked_duration_since(b) = (a.0 ≥ b.0 ? Some(a.0 − b.0) : None). a.saturating_duration_since(b) = a.duration_since(b) = a − b = (a.0 ≥ b.0 ? a.0 − b.0 : D::ZERO). None of them panics. a < b ⇔ a.0 < b.0 and a = b ⇔ a.0 = b.0 (the derived order is the field order). Corollary: a = b ⇒ b.duration_since(a) = D::ZERO.	shim::instant_differences_saturate_backwards	EASY: &self methods, an Option return, core Duration::checked_sub/saturating_sub, an operator-trait impl, derives on a newtype.	1, 2, 3, 4, 6, 12, cfg
time-instant-shift	contract	shim::Instant::{checked_add,checked_sub}, <shim::Instant as Add<Duration>>::add, <… as Sub<Duration>>::sub, <… as AddAssign<Duration>>::add_assign, <… as SubAssign<Duration>>::sub_assign	∀ a: Instant, d: D: a.checked_add(d) = (‖a.0‖ + ‖d‖ ≤ ‖D::MAX‖ ? Some(Ishim(a.0 + d)) : None). a.checked_sub(d) = (d ≤ a.0 ? Some(Ishim(a.0 − d)) : None). a + d panics ⇔ a.checked_add(d) = None, and otherwise a + d = a.checked_add(d).unwrap(); likewise a − d with checked_sub. `a += d` leaves a' = a + d and `a -= d` leaves a' = a − d, with the same panic condition. Round trip: a.checked_add(d) = Some(b) ⇒ b − d = a ∧ b − a = d ∧ (d > 0 ⇒ b > a).	shim::instant_arithmetic_round_trips_and_checks	EASY-MEDIUM: &mut self two-state a', Option::map, `expect` (panic as a precondition), operator traits, core Duration::checked_add/checked_sub. Same proof as time-systime-shift.	1, 2, 3, 4, 6, 12, cfg
time-systime-duration-since	contract	shim::SystemTime::duration_since, shim::SystemTimeError::duration, <shim::SystemTimeError as Display>::fmt, shim::UNIX_EPOCH, shim::SystemTime::UNIX_EPOCH	∀ s, t: SystemTime: s.duration_since(t) = (s.0 ≥ t.0 ? Ok(s.0 − t.0) : Err(e) with e.duration() = t.0 − s.0 > 0), and it never panics. UNIX_EPOCH.0 = D::ZERO and SystemTime::UNIX_EPOCH = UNIX_EPOCH, so s.duration_since(UNIX_EPOCH) = Ok(s.0). ∀ e: SystemTimeError: e.to_string() = "second time provided was later than self" (std's message, the parity contract that callers may log). Corollary: s = t ⇒ t.duration_since(s) = Ok(D::ZERO).	shim::system_time_reports_backwards_steps	EASY-MEDIUM: a Result return built with an ok_or_else closure, &self, consts, and a Display impl through write! into a Formatter (the String content needs gap 1).	1, 2, 3, 4, 6, 12, 13, cfg
time-systime-shift	contract	shim::SystemTime::{checked_add,checked_sub}, <shim::SystemTime as Add/Sub/AddAssign/SubAssign<Duration>>	time-instant-shift with Sshim for Ishim: ∀ s: SystemTime, d: D: checked_add is Some(Sshim(s.0 + d)) iff the Duration does not overflow, and None otherwise. checked_sub is Some(Sshim(s.0 − d)) iff d ≤ s.0, and None otherwise, so UNIX_EPOCH.checked_sub(d) = None for d > 0. `+`/`−` equal the unwrapped checked forms and panic exactly when those are None. `+=`/`−=` equal `+`/`−`. s.checked_add(d) = Some(u) ⇒ u − d = s.	shim::system_time_arithmetic_round_trips	as time-instant-shift	1, 2, 3, 4, 6, 12, cfg
time-epoch-accessors	contract (effectful)	since_epoch, since_epoch_at, unix_secs, unix_secs_at, unix_millis, unix_millis_at	Pure per-reading core: since_epoch_at(w) = (w ≥ UNIX_EPOCH ? Some(w − UNIX_EPOCH) : None); unix_secs_at(w) = since_epoch_at(w).map(as_secs); unix_millis_at(w) = since_epoch_at(w).map(|d| min(d.as_millis(), u64::MAX) as u64). Effect model: each of since_epoch(), unix_secs(), unix_millis() does one now_w read with value w and returns the corresponding *_at(w). None of them panics. So for reads w0 ≤ w ≤ w1, each accessor lies in [w0 − E, w1 − E] in its unit.	lib::the_epoch_accessors_read_the_wall_clock	MEDIUM: a platform clock effect with two arms (std re-export natively, the shim on wasm), Result::ok, Option::map with closures, and u128→u64 try_from().unwrap_or saturation. The pure *_at(w) helpers can be proved first without effects.	1, 3, 4, 8, 12, 13
```

### Distribution by kind (9)

| kind | count | ids |
|---|---|---|
| contract | 7 | time-conv-exact, time-conv-monotone-resolution, time-instant-difference, time-instant-shift, time-systime-duration-since, time-systime-shift, time-epoch-accessors |
| no-panic + contract | 1 | time-conv-total-floor |
| refinement | 1 | time-from-js-millis |

### Proof dependency order

`cfg` fix → `time-instant-difference`, `time-instant-shift`, `time-systime-shift`,
`time-systime-duration-since` (Duration arithmetic only, no floats) → the float model `F` →
`time-conv-total-floor` → `time-conv-exact` → `time-conv-monotone-resolution` and
`time-from-js-millis` (they compose on conv's ensures, gap 4) → `time-epoch-accessors` (effects, gap 8).

## 2. Test → spec map (TSV)

`primary` is the spec that retires the test. `also` lists the other specs the same test samples. The
tests of §4 are deleted and have no row.

```tsv
file	test	primary	also
src/shim.rs	millisecond_conversion_is_total	time-conv-total-floor	time-conv-exact
src/shim.rs	millisecond_conversion_keeps_the_fraction	time-conv-exact	
src/shim.rs	resolution_survives_an_aged_time_origin	time-conv-monotone-resolution	time-conv-exact
src/shim.rs	instant_differences_saturate_backwards	time-instant-difference	time-from-js-millis
src/shim.rs	instant_arithmetic_round_trips_and_checks	time-instant-shift	time-instant-difference, time-from-js-millis
src/shim.rs	system_time_reports_backwards_steps	time-systime-duration-since	time-from-js-millis, time-conv-exact
src/shim.rs	system_time_arithmetic_round_trips	time-systime-shift	time-from-js-millis
src/lib.rs	the_epoch_accessors_read_the_wall_clock	time-epoch-accessors	
```

## 3. Coverage

- 8 live tests; all 8 map to one of the 9 specs, and the 4 non-spec tests formerly in §4 are deleted.
- Every spec but `time-from-js-millis` retires one primary test. `millisecond_conversion_is_total`
  also checks that a reading beyond `u64::MAX` milliseconds saturates, a clause of `time-conv-exact`.
  `time-from-js-millis` and `time-conv-exact` are also lemmas that the arithmetic tests use to build
  their inputs.
- When the tests that exercise them are gone, the shim specs remain the only statement of the
  wasm clock's contract. That is the point: on native Trust the shim runs nowhere except tests.

## 4. Tests that are not spec samples

> Status 2026-10-16: all 4 tests below are deleted from `src/lib.rs`.
>
> Status 2026-10-17: their table and their rows in §2 are gone. They exercised std's `Instant` and
> `SystemTime` (plain re-exports on native), one of them by timing a sleep, and one checked that the
> host wall clock reads between 2020 and 2200; none sampled a property of aterm code.

## 5. Implied specs that look wrong, or disagree with the code

1. **`+∞` floors to zero, but `f64::MAX` saturates.** `conv(f64::MAX) = D::from_millis(u64::MAX)`,
   while `conv(+∞) = D::ZERO`. So conv is not monotone over all of `f64` (`conv(1.0) > conv(+∞)`), and
   a clock that overflows to `+∞` reads as the time origin. The doc comment ("Everything unusable
   floors to zero") makes this deliberate. `time-conv-monotone-resolution` therefore quantifies over
   finite inputs only. If saturation is the intent for huge readings, `+∞` should saturate too.
   That is an owner call, and the spec is written for the code as it is.
2. **Resolution test tightened (resolved 2026-10-27).** `resolution_survives_an_aged_time_origin`
   now asserts `b - a >= Duration::from_nanos(99_998)` for a `0.1 ms` step at `3.6e6 ms`, matching
   `time-conv-monotone-resolution`'s `< 2 ns` two-reading bound.
3. **Shim compiled under `trust_verify` (resolved 2026-10-27).** `mod shim` in `src/lib.rs`
   includes `trust_verify` in its `cfg` gate, so native Trust check/verify lanes build `shim.rs`
   even without `cfg(test)`.
4. **Per-read epoch accessors factored out of wall-clock bracketing (resolved 2026-10-28).**
   `since_epoch_at`, `unix_secs_at` and `unix_millis_at` expose the pure per-reading conversion of
   `time-epoch-accessors`, and `the_epoch_accessors_read_the_wall_clock` tests them on pre-epoch,
   epoch, post-epoch and saturating inputs plus one captured `SystemTime::now()` without bracketing
   separate syscalls against NTP steps.
5. **Tautological `Box<dyn Error>` message check removed (resolved 2026-10-27).**
   `system_time_reports_backwards_steps` checks `err.to_string()` directly and checks the
   `core::error::Error` impl via a `const fn` trait bound instead of `!boxed.to_string().is_empty()`.
6. **An unstated rounding fact.** The comment "`frac` is in `[0, 1)`, so the product is in
   `[0, 1e6)`" is true only because of rounding. For `f = 1 − 2^-53` the real product rounds to the
   float just below `10^6`, not to `10^6`, because `10^6` is not a power of two and its ulp is `2^-33`.
   The bound is part of `time-conv-exact` and must be proved, not assumed.

## 6. Proof order (easiest high-value first)

No spec here is blocked only by gap 0. Every spec needs at least gap 1 (newtype/`Option`/`Result`
carriers) and gap 3 (calls into core `Duration`). The order below takes the specs with no floats and
no effects first:

1. **`time-instant-difference`**. A newtype over `Duration`, `checked_sub`/`saturating_sub` and derived
   order. Once Duration is modelled as `‖·‖ ∈ ℕ`, it is linear arithmetic. It is the template for the
   other three arithmetic specs. Blocked by 1, 2, 3, 4, 6 and 12.
2. **`time-instant-shift`** and **`time-systime-shift`** together. They have the same proof, and they
   add the `&mut self` two-state form (gap 2) and the panic-as-precondition form of `+`/`−`.
3. **`time-systime-duration-since`**. The same arithmetic, plus a `Result` and the `Display`
   message (gaps 1 and 13).
4. **`time-from-js-millis`**. Trivial once conv has a contract to compose on.
5. **`time-conv-total-floor`**, then **`time-conv-exact`**, then **`time-conv-monotone-resolution`**.
   All three wait on the float model `F`. Proposing `F` as a new §7 row is the highest-leverage Trust
   change this crate surfaces, since 3 of its 9 specs depend on it.
6. **`time-epoch-accessors`** last. It waits on effects (gap 8). Its pure map from `w` to the three
   values can be factored out and proved earlier.
