<!-- Copyright 2026 Andrew Yates -->
<!-- SPDX-License-Identifier: Apache-2.0 -->

# aterm-winit-keymap: spec recovery from its 15 unit tests

> Status 2026-10-16, after the merge with the tests-and-gates audit: the rows naming 5 tests it
> deleted are gone, and the spec rows no longer cite those tests. Counts elsewhere in this file
> were taken before.

> Status 2026-10-16: the 7 specs below are the crate's migration worklist. Each is proved through
> Trust/Clean about the compiled Rust function, then the tests it subsumes are deleted. Nothing has been
> proved or deleted yet.

Source: `crates/aterm-winit-keymap/src/lib.rs` (read only). All 15 `#[test]` fns are in the in-file
`mod tests`. Nothing was compiled or run; the claims below come from reading the code. Production
callers: `crates/aterm-gui/src/keymap.rs` (`map_logical_key`, `map_numpad_key`, `base_layout_key_for`)
and the GUI key path (`sided_modifier`, `physical_modifier_twin`).

Result: **7 specs**, covering **14 tests**. **1 test is not a spec sample** (§4).

## 0. Notation used in the statements

- `WN` = `aterm_window::keyboard::NamedKey` (foreign, `#[non_exhaustive]`); `KC` = `aterm_window::keyboard::KeyCode`
  (foreign, `#[non_exhaustive]`, ~200 variants); `WK` = `aterm_window::keyboard::Key<SmolStr>`
  (`Character(s) | Named(n) | Dead(_) | Unidentified(_)`); `PK` = `PhysicalKey`
  (`Code(kc) | Unidentified(_)`); `Loc` = `KeyLocation {Standard, Left, Right, Numpad}`.
- `Key`, `NamedKey` = `aterm_types::keyboard::{Key, NamedKey}` (the engine's types).
- `single(s)` = `Some(c)` when `s.chars()` yields exactly one char `c`, otherwise `None` (covers both the
  empty and the multi-codepoint string).
- `MODS` = {Shift, Control, Alt, Super, Hyper, Meta}; `Left(m)` / `Right(m)` are the engine's sided
  variants (`ShiftLeft`, `ShiftRight`, …).
- `T_named : WN -> Option<NamedKey>`, the explicit table at `lib.rs` (`map_named_key`): the same-named
  variant for the 8 navigation keys, 7 editing keys (Backspace, Delete, Insert, Enter, Tab, Escape,
  Space), 6 lock/system keys (CapsLock, NumLock, ScrollLock, PrintScreen, Pause, ContextMenu), F1..F35 and
  the 12 media/audio keys; each `m ∈ MODS` goes to `Left(m)`; every other variant gives `None`.
- `T_kp_char : char -> Option<NamedKey>`: `'0'..'9'` → `Numpad0..Numpad9`, `'.'` → `NumpadDecimal`,
  `','` → `NumpadSeparator`, `'/'` → `NumpadDivide`, `'*'` → `NumpadMultiply`, `'-'` → `NumpadSubtract`,
  `'+'` → `NumpadAdd`, `'='` → `NumpadEqual`; `None` otherwise.
- `T_kp_named : WN -> Option<NamedKey>`: `Enter` → `NumpadEnter`; ArrowUp/Down/Left/Right, Home, End,
  PageUp, PageDown, Insert, Delete → their `Numpad*` twins; `None` otherwise.
- `PAIRS` = {(ShiftLeft,ShiftRight), (ControlLeft,ControlRight), (AltLeft,AltRight),
  (SuperLeft,SuperRight)} ⊂ `KC × KC`.
- `T_qwerty : KC -> Option<char>`: KeyA..KeyZ → `'a'..'z'`, Digit0..Digit9 → `'0'..'9'`, and Backquote,
  Minus, Equal, BracketLeft, BracketRight, Backslash, Semicolon, Quote, Comma, Period, Slash → the
  matching unshifted US glyph; `None` for every other code.

All tables are finite, so every table property is decidable by enumerating variants once the carriers
(foreign enums, `SmolStr`, `char`, `Option`) can be stated in a clause.

## 1. Specs (TSV)

Columns: `id`, `kind`, `functions`, `statement`, `subsumes (primary tests; secondary in §2)`,
`difficulty (Rust features the proof must handle)`, `blocking gaps` (numbers from
`docs/AUDIT-trust-for-test-migration-2026-10-15.md` §7; gap 0, the stale installed seal, blocks every
row and is listed once here rather than in each row).

```tsv
id	kind	functions	statement	subsumes	difficulty	blocking gaps
named-key-table	table-property	map_named_key	∀ n:WN: map_named_key(n) = T_named(n). Corollaries: (a) injective on its Some-domain; (b) no result is a *Right modifier or a Numpad* key; (c) ∀ m∈MODS: map_named_key(m) = Some(Left(m)).	-	EASY once stateable: ~70-arm match on a foreign #[non_exhaustive] fieldless enum with a wildcard arm into Option<enum>; no loops, no heap. The clause quantifies over (or enumerates) a foreign enum's variants.	1, 3, 5, 12
logical-key-contract	contract	map_logical_key, map_named_key	∀ k:WK: k = Character(s) ⇒ result = single(s).map(Key::Character); k = Named(n) ⇒ result = T_named(n).map(Key::Named) (cites named-key-table); k = Dead(_) ∨ Unidentified(_) ⇒ result = None. In particular ∀ m∈MODS: map_logical_key(Named(m)) = Some(Key::Named(Left(m))).	character_logical_key_maps_through	MEDIUM: match on an enum carrying SmolStr; two str::chars().next() calls need a UTF-8 decode model of a str/SmolStr carrier; `?` on Option; composes with map_named_key through its ensures.	1, 3, 4, 12
numpad-location-gate	contract	map_numpad_key	∀ p:PK, k:WK, loc:Loc with loc ≠ Numpad: map_numpad_key(p,k,loc) = None. Only the location decides that a key is off the keypad; neither the scancode (e.g. Code(Numpad5)) nor the logical key matters.	standard_location_is_untouched	TRIVIAL once a foreign enum and a &WK reference parameter are carriers: one early return on enum inequality.	1, 12
numpad-key-table	contract	map_numpad_key, map_logical_key	∀ p:PK, k:WK, loc = Numpad: (1) k = Character(s): single(s) = None ⇒ None; single(s) = Some(c) ⇒ T_kp_char(c).map(Key::Named), whatever p is (a Character never reaches the centre-key rescue). (2) k = Named(n) ∧ T_kp_named(n) = Some(t) ⇒ Some(Named(t)). (3) Otherwise (Named with no keypad twin, Dead, Unidentified): Some(Named(NumpadBegin)) iff p = Code(KC::Numpad5) ∧ map_logical_key(k) = None, else None. Corollary: every Some result is Key::Named(x) with x a Numpad* key; p affects the result only through clause (3).	numpad_location_promotes_digit_to_numpad_key, numpad_enter_is_not_main_enter, numlock_off_navigation_gets_numpad_twin, keypad_identity_is_the_glyph_not_the_scancode, keypad_key_without_keypad_identity_takes_the_logical_route	MEDIUM: nested match on a SmolStr-carrying enum, char-literal match, a match guard that calls map_logical_key (composition through its ensures), str::chars iteration, early returns, PhysicalKey equality including the NativeKeyCode payload.	1, 3, 4, 12
sided-modifier-contract	contract	sided_modifier	∀ key:Key, loc:Loc: loc ≠ Right ⇒ sided_modifier(key,loc) = key. loc = Right: key = Named(Left(m)) for m∈MODS ⇒ Named(Right(m)); otherwise key. Corollaries: idempotent at each location; with logical-key-contract, sided_modifier(map_logical_key(Named(m)).unwrap(), Right) = Named(Right(m)).	a_right_side_modifier_press_maps_to_its_right_variant	EASY: Key by value (non-Copy enum holding char and NamedKey), a match with a binding fall-through arm, equality on a foreign KeyLocation.	1, 3, 12
physical-twin-involution	table-property	physical_modifier_twin	∀ (l,r)∈PAIRS: twin(Code(l)) = Some(Code(r)) ∧ twin(Code(r)) = Some(Code(l)); for every other p (every PK::Unidentified, KC::Meta, KC::Hyper, KC::Fn, …) twin(p) = None. Hence an involution without fixed points on its domain: twin(p) = Some(q) ⇒ q ≠ p ∧ twin(q) = Some(p). Optional cross-crate clause: under the embedding of the eight sided codes into NamedKey, twin agrees with NamedKey::modifier_twin.	a_sided_modifier_has_its_other_side_as_twin	EASY once stateable: let-else on a foreign enum, a match on the ~200-variant #[non_exhaustive] KeyCode with a wildcard arm, Option<PhysicalKey>. The cross-crate clause needs theorem citation across crates.	1, 3, 5, 12
base-layout-key-table	table-property	base_layout_key_for	∀ p:PK: p = Code(kc) ⇒ base_layout_key_for(p) = T_qwerty(kc); p = Unidentified(_) ⇒ None. Corollaries: every Some(c) has c ∈ 0x21..=0x7E (printable, non-space ASCII), never an uppercase letter or a shifted symbol; injective (47 codes to 47 distinct chars).	base_layout_key_is_us_qwerty	EASY once stateable: let-else and a ~47-arm match on a foreign #[non_exhaustive] enum into Option<char>; needs a char carrier.	1, 3, 5, 12
```

### Distribution by kind (7)

| kind | count | ids |
|---|---|---|
| contract | 4 | logical-key-contract, numpad-location-gate, numpad-key-table, sided-modifier-contract |
| table-property | 3 | named-key-table, physical-twin-involution, base-layout-key-table |

### Blocking-gap histogram (audit §7 number → specs it blocks)

| gap | specs |
|---|---|
| 0 (installed seal) | 7 (prerequisite for all) |
| 1 (non-scalar carriers: foreign enums, SmolStr, char, Option) | 7 |
| 12 (cross-crate citation: `Key`/`NamedKey` live in aterm-types, carriers in winit) | 7 |
| 3 (E6 body widening: enum matches, calls) | 6 |
| 5 (quantified clauses) | 3 |
| 4 (callee-ensures composition) | 2 |

No spec is blocked by gap 0 alone: every row needs gap 1 (foreign enum carriers) and gap 12.

### Proof dependency order

`named-key-table` → `logical-key-contract` → `numpad-key-table` (its clause 3 guard calls
`map_logical_key`). `sided-modifier-contract`'s composition corollary cites `logical-key-contract`.
`numpad-location-gate`, `physical-twin-involution` and `base-layout-key-table` are independent.

## 2. Test → spec map (TSV)

`primary` is the spec that retires the test. `also` lists the other specs the same test samples.

```tsv
file	test	primary	also
src/lib.rs	character_logical_key_maps_through	logical-key-contract	
src/lib.rs	standard_location_is_untouched	numpad-location-gate	
src/lib.rs	numpad_location_promotes_digit_to_numpad_key	numpad-key-table	
src/lib.rs	numpad_enter_is_not_main_enter	numpad-key-table	
src/lib.rs	numlock_off_navigation_gets_numpad_twin	numpad-key-table	logical-key-contract
src/lib.rs	keypad_identity_is_the_glyph_not_the_scancode	numpad-key-table	
src/lib.rs	keypad_key_without_keypad_identity_takes_the_logical_route	numpad-key-table	logical-key-contract
src/lib.rs	a_right_side_modifier_press_maps_to_its_right_variant	sided-modifier-contract	logical-key-contract
src/lib.rs	a_sided_modifier_has_its_other_side_as_twin	physical-twin-involution	
src/lib.rs	base_layout_key_is_us_qwerty	base-layout-key-table	
```

## 3. Coverage

- 15 tests read. 14 map to one of the 7 specs; 1 is not a spec sample (§4).
- The biggest collapse is `numpad-key-table` (5 primary). `named-key-table` retires 3 regression samples
  whose names pin the history of the removed GUI inline match (§5.3).
- `a_right_side_modifier_press_maps_to_its_right_variant` is retired only when both
  `sided-modifier-contract` and `logical-key-contract` are proved (it asserts one half of each).

## 4. Tests that are not spec samples

> Status 2026-10-17: none left. The one test here, which read `crates/aterm-types/Cargo.toml` and
> checked that no non-comment line names `winit`, is deleted as source scraping; no `aterm-census`
> guard took its place.

## 5. Implied specs that look wrong, or disagree with the code

1. **The centre-key rescue is unreachable for a Character key** (`numlock_off_navigation_gets_numpad_twin`).
   The doc comment says the `Numpad5` → `NumpadBegin` rescue applies when "the engine has no name for
   what winit reported", but `Character("")` and `Character(c)` with `c` outside `T_kp_char` return
   `None` from the inner `?` / `_ => return None` before the
   `physical == Numpad5 && map_logical_key(logical).is_none()` guard runs. The tests sample only
   `Unidentified` and `Named(Clear)`, so they agree with the code. `numpad-key-table` clause (1) pins
   the code's behaviour, not the comment's wording. Decide which is intended before proving it.
2. **No physical twin for Hyper and Meta** (`a_sided_modifier_has_its_other_side_as_twin`).
   `physical_modifier_twin` covers Shift, Control, Alt and Super only, and the test asserts that
   `KeyCode::Meta` has no twin, while `sided_modifier` and `NamedKey::modifier_twin` also produce and
   pair `HyperRight`/`MetaRight`. winit's `KeyCode` has no sided Meta or Hyper, so the asymmetry is
   forced by the platform, but a right-side Meta or Hyper release cannot have its twin's held state
   checked through `physical_modifier_twin`. The doc comment states this only implicitly ("Control, Alt
   and Super likewise"); `physical-twin-involution` makes it explicit.
3. **A history-named regression sample** (`maps_function_keys_past_f12_and_media_keys`). Its name
   and comment pin prose about removed code ("stopped at F12 and dropped the media keys"). The real
   property is the whole `T_named` table; delete it once `named-key-table` is proved rather than
   porting it.

## 6. Proof order (easiest high-value first)

No spec here is blocked by gap 0 alone; the cheapest are those needing only gaps 1 and 12 (plus 3).

1. **`numpad-location-gate`** (gaps 1, 12). One early return on enum inequality; no gap 3, 4 or 5.
   It retires 1 test and is the first clause of `map_numpad_key`, so it exercises the foreign-enum and
   `&WK` carriers every other spec needs.
2. **`sided-modifier-contract`** (gaps 1, 3, 12). A six-arm match on the engine's own `Key`; no
   quantifier over a foreign enum and no `SmolStr`. Retires half of
   `a_right_side_modifier_press_maps_to_its_right_variant`.
3. **`base-layout-key-table`** and **`physical-twin-involution`** (gaps 1, 3, 5, 12). Pure finite
   tables, decidable by enumeration once a foreign `#[non_exhaustive]` enum can be quantified over.
4. **`named-key-table`** (gaps 1, 3, 5, 12). Same shape, larger table; retires 3 tests and is the lemma
   the two composed specs cite.
5. **`logical-key-contract`**, then **`numpad-key-table`** (gaps 1, 3, 4, 12). They need a `SmolStr`
   UTF-8 model and callee-ensures composition; `numpad-key-table` retires the most tests (5) but comes
   last because it cites both earlier specs. Settle §5.1 before proving it.
