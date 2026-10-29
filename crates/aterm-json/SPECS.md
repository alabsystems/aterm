<!-- Copyright 2026 Andrew Yates -->
<!-- SPDX-License-Identifier: Apache-2.0 -->

# aterm-json: spec recovery from its 24 unit tests

> Status 2026-10-16, after the merge with the tests-and-gates audit: the rows naming 1 test it
> deleted are gone, and the spec rows no longer cite those tests. Counts elsewhere in this file
> were taken before.

> Audit amendment: The 2026-10-16 code audit corrected the source docs identified in §5.1/2/7. The dated inventory and those observations below record the earlier snapshot; parser behavior is unchanged.

> Status 2026-10-16: this is the crate's migration worklist. Each of the 19 specs below is proved
> through Trust/Clean about the compiled Rust function, and then the tests it subsumes are deleted.
> No test has been deleted yet. Every test samples at least one spec (§4), so none goes before its
> spec is Certified.

Source: `crates/aterm-json` (read only). All 24 tests are in `tests/json.rs`. The library has no
unit tests in `src/` (`[lib] test = false`). I read every test and all of
`src/{de,ser,value,lib,macros,error}.rs`. Nothing was compiled or run. The claims below come from
reading the code.

Result: **19 specs**, covering **24 tests**. **0 tests are not spec samples** (§4). Most tests
are digest-pinned sweeps: they hash their verdicts and output bytes into one FNV-1a value. A spec
replaces the sweep, not the digest (§5.3).

This revision corrects `typed-integer-targets`: `i128` accepts `-0` as 0 (§5.7). The other 18 specs
were re-checked against every test and the source and are unchanged in substance.

Gap numbers refer to the table in
`AUDIT-trust-for-test-migration-2026-10-15.md` §7 (historical internal audit).
Gap 0 (a seal that publishes Clean rows) is a prerequisite for every row, so every row lists it.

## 0. Notation used in the statements

- `JSONa`: the input language aterm-json accepts, as a set of byte strings. It is `ws value ws`
  with `ws = [ \t\n\r]*` and nothing else (no form feed, no BOM, no comments).
  - Numbers follow RFC 8259: `-? (0 | [1-9][0-9]*) (. [0-9]+)? ([eE][+-]?[0-9]+)?`.
  - A string is `"` (an unescaped byte ≥ 0x20 other than `"` and `\` | `\` one of `" \ / b f n r t`
    | `\uXXXX`)* `"`, with three restrictions. (a) The raw content bytes are valid UTF-8. (b) A `\u`
    high surrogate D800..DBFF must be followed at once by `\u` with a low surrogate DC00..DFFF, and
    a lone low surrogate is refused. (c) No unescaped C0 control.
  - Arrays and objects allow no trailing comma. Object keys are strings.
  - Container nesting depth is ≤ 127 (`depth` below).
  - Every number that becomes a float has a finite nearest double. Overflow to infinity is refused;
    underflow to 0 is accepted.
- `nearest(t)`: the correctly rounded IEEE binary64 value of the decimal token `t` (Rust
  `str::parse::<f64>`). `dec(t)`: the integer a decimal token denotes.
- `cls(t)` classifies a number token `t`:
  - no `.` and no `e`, `t` non-negative and fitting u64 → `Unsigned`;
  - no `.` and no `e`, `t` negative, fitting i64 and ≠ 0 → `Signed`;
  - `-0` → `Float(-0.0)` (`Num::NegZero`);
  - any other integer → `Float(nearest(t))` in lenient mode, `Err` in exact mode
    (`Deserializer::exact`, which `to_value` uses);
  - a fraction or exponent → `Float(nearest(t))`.
- `Den(b)`: the `Value` denoted by `b ∈ JSONa`. Literals map to `Null`/`Bool`, a number to
  `Number(cls(t))`, and a string to its decoded scalar values. An array maps to its elements in
  order. An object maps to `foldl BTreeMap::insert` over its members in document order, so duplicate
  keys are last-wins.
- `depth(v)`: the maximum number of nested `Array`/`Object` constructors. `Deserializer.depth`
  counts entered containers, and `enter` fails when the count after the increment is ≥
  `RECURSION_LIMIT` (128). So depth 127 parses and depth 128 does not.
- `Q(s)` = `"` ++ concatMap esc (bytes s) ++ `"`, where esc(`"`) = `\"`, esc(`\`) = `\\`,
  esc(08) = `\b`, esc(0C) = `\f`, esc(0A) = `\n`, esc(0D) = `\r`, esc(09) = `\t`. Any other byte
  c < 0x20 becomes `\u00` plus two lowercase hex digits. Every other byte (DEL, non-ASCII, `/`,
  U+2028, U+2029) is copied verbatim.
- `F64(x)`: `null` when x is not finite, and `0.0` or `-0.0` when x is ±0. Otherwise it is
  `layout(sign, D, e, -4..=16)`.
  - D is the decimal digit string of minimal length n such that 0.D×10^(e+1) rounds to x. n is
    the digit count of Rust `{:e}`. Among the decimals of length n that round to x, D is the one
    closest to x.
  - `layout` sets kk = e+1. For kk in the range it uses fixed notation: `0.` then -kk zeros then
    D when kk ≤ 0; D, then kk-n zeros, then `.0` when kk ≥ n; otherwise D split after kk digits.
  - For kk outside the range it writes `d[.ddd]e(+|-)|e|`, and the exponent sign is always present.
- `F32(y)`: `F64` at binary32 width, with the fixed range -5..=13.
- `Enc(v)`: the compact writer on `Value`. `Null` is `null`. `Bool` is `true`/`false`.
  `Unsigned`/`Signed` are decimal. `Float` is `F64(x)`. `String` is `Q(s)`. An `Array` is `[` the
  elements joined by `,` `]`. An `Object` is `{` the `Q(k):Enc(v)` pairs in BTreeMap (sorted byte)
  key order, joined by `,` `}`.
- `SerModel`: the serde data-model denotation of `ser::Serializer`.
  - A struct is `{` `Q(field):ser(v)` in call (declaration) order `}`. Its fields are not sorted,
    and skipped fields are absent.
  - Seq, tuple and tuple struct are `[`…`]`.
  - Unit, unit struct and none are `null`. some(x) and newtype(x) are ser(x).
  - A unit variant is `Q(name)`. Newtype, tuple and struct variants are `{Q(name):x}`,
    `{Q(name):[..]}` and `{Q(name):{..}}`.
  - Bytes are an array of decimals. A map writes each key with `KeySer`.
- `KeySer`: str, char and unit variant are written as `Q(text)`, a bool as `Q("true")` or
  `Q("false")`, an integer of any width as a quoted decimal (`Display`'s spelling), and newtype(x)
  as `KeySer(x)`. Every other kind (float, bytes, option, unit, seq, tuple, map, struct, variants
  with a payload) is `Err("JSON object keys must be strings")`.
- `Faithful(b)`: the protocol `&mut Deserializer` offers any serde Visitor, for `b ∈ JSONa`.
  - `deserialize_any` calls exactly the `visit_*` that matches `Den(b)`.
  - `SeqAccess` yields the elements in order and never consumes `]`. After the visitor returns,
    `end_container` requires the next non-ws byte to be `]`, otherwise Err. So a visitor that stops
    early with elements left fails.
  - `MapAccess` yields every (key, value) pair in document order, duplicates included.
  - `deserialize_option` gives none for `null` and some otherwise.
  - `deserialize_enum` takes a string (unit variant) or a single-entry object `{"V":payload}`.
  - `deserialize_struct` also accepts an array of the fields in order.
  - A key reaches the visitor through `Key` wrapped in serde's `TextKeyDeserializer`, as a string, except: an integer request
    takes a key spelled as `Display` writes an integer (optional `-`, no leading zero, never `-0`)
    as that integer (`u64`/`i64`, wider `u128`/`i128`); a bool request takes `true`/`false`; an
    option or newtype request sees the key itself. Any other spelling reaches the visitor as a
    string, so the visitor's type error refuses it.

## 1. Specs (TSV)

Columns: `id`, `kind`, `functions`, `statement`, `subsumes`, `difficulty (Rust features the proof
must handle)`, `blocking_gaps` (audit §7 numbers). `subsumes` lists every test the spec samples;
§2 names the one that retires each test. Function paths are relative to `crates/aterm-json/src`.

```tsv
id	kind	functions	statement	subsumes	difficulty	blocking_gaps
json-accepts-exactly-grammar	contract	de::{from_slice,from_str}, de::Deserializer::{skip_ws,peek_value,literal,expect,end,end_container,parse_string_body,push_chunk,parse_escape,parse_hex4,parse_number,visit_number,dispatch}, <&mut de::Deserializer as serde::Deserializer>::deserialize_any, de::SeqAccess::next_element_seed, de::MapAccess::{next_key_seed,next_value_seed}, de::Key::deserialize_any, value::ValueVisitor	∀ b: &[u8]: from_slice::<Value>(b) = Ok(v) ⇔ b ∈ JSONa, and then v = Den(b). ∀ s: &str: from_str::<Value>(s) = from_slice::<Value>(s.as_bytes()). This covers: exactly one value then only RFC whitespace (trailing data, empty input, whitespace-only input and a leading BOM refused); leading zeros, bare `-`, `+1`, `.5`, `1.`, `1e`, `1e+`, hex, `_`, NaN, Infinity, `--1` and `1..2` refused; literals lowercase and complete; the surrogate-pair rules; unescaped C0 bytes refused; invalid UTF-8 refused; trailing commas, missing separators and non-string keys refused; duplicate keys last-wins; a float literal past f64::MAX refused, underflow gives ±0.	json::generated_documents_parse_and_reserialize, mutated_documents_get_their_pinned_verdicts, arbitrary_bytes_get_their_pinned_verdicts, named_edge_cases_get_their_pinned_verdicts	HARD: &mut self cursor over &[u8] (pos, depth); while loops with early return and `?` propagation; core::str::from_utf8 (needs a UTF-8 validity model); Cow<str>; String push/push_str; char::from_u32; str::parse::<u64/i64/f64> (needs an integer-parse model and a correctly rounded dec2flt model, which no gap row names, §5.6); generic Visitor trait calls; mutual recursion deserialize_any ↔ SeqAccess/MapAccess; BTreeMap insert. The grammar side needs an inductive language over byte lists.	0,1,2,3,4,5,7,11,14
recursion-limit	invariant	de::Deserializer::{enter,leave,skip_value}, <&mut de::Deserializer as serde::Deserializer>::{deserialize_any,deserialize_enum}	∀ d ≥ 1, both container kinds (d nested `[`…`]`, or d nested `{"k":`…`}`, around a scalar): from_str::<Value> succeeds ⇔ d ≤ 127, and so does from_slice::<IgnoredAny> (through skip_value). Invariant: on every successful path, Deserializer.depth = the number of containers entered and not yet left, and depth ≤ RECURSION_LIMIT (128) whenever a container body is parsed. So the parser's recursion depth is bounded for every input; 5000-deep input is Err, not a stack overflow.	json::the_recursion_limit	MEDIUM: &mut self u32 counter with saturating_add/sub; an invariant across mutual recursion and trait-generic visitor calls; loops in skip_value; error paths that return before leave (they abort the parse)	0,1,2,3,5,7,11
skip-equals-materialize	equivalence	de::Deserializer::{skip_value,skip_string_body,parse_string_body,push_chunk,parse_escape}, <&mut de::Deserializer as serde::Deserializer>::{deserialize_ignored_any,deserialize_any}	∀ b: &[u8]: from_slice::<IgnoredAny>(b).is_ok() ⇔ from_slice::<Value>(b).is_ok(). Key lemma: ∀ input, ∀ pos just after an opening quote, skip_string_body succeeds ⇔ parse_string_body succeeds, and both leave pos at the same byte (0x22 and 0x5C never occur inside a multi-byte UTF-8 sequence, so validating the chunks between escapes equals validating the whole run). For literals, numbers and containers, skip_value and deserialize_any consume the same bytes and fail on the same inputs, depth included, in value, nested-array and object-key positions.	json::ignored_strings_validate_the_same_bytes_as_materialized_values	MEDIUM-HARD: two loops over the same &[u8] with different chunking; from_utf8 over sub-slices (lemma: valid(a++b) ⇔ valid(a) ∧ valid(b) when the split is at an ASCII byte); recursion; Cow vs unit result	0,1,2,3,5,7,11
value-encoding	contract	ser::{to_vec,to_string,write_json_string,write_u128,write_i128,write_f64}, ser::Serializer, ser::Compound::{comma,finish,entry}, ser::KeySerializer::serialize_str, <Value as Serialize>::serialize, <Value as Display>::fmt	∀ v: Value: to_string(&v) = Ok(Enc(v)): compact, object keys in sorted BTreeMap order, a non-finite float written as null. format!("{v}") = Enc(v). Corollary: to_vec never fails on a Value, because every key is a String.	json::value_display_is_compact, the_json_macro	MEDIUM: &mut Vec<u8> extend/push; recursion over the Value enum (Vec, BTreeMap); serde collect_seq/collect_map over a BTreeMap iterator; fmt::Formatter::write_str; String::from_utf8 (lemma: the output is valid UTF-8). Floats come from float-f64-shortest-roundtrip.	0,1,2,3,5,7,11
number-normal-form	invariant	value::Number::{from(u64),from(i64),from(f64),as_u64,as_i64,as_f64}, value::ValueVisitor::{visit_i64,visit_u64,visit_f64}	Type invariant on Number: Signed(v) ⇒ v < 0. Every constructor keeps it: the field is private and From<i64> maps v ≥ 0 to Unsigned. So a non-negative integer has exactly one representation. as_u64 = Some(v) ⇔ Unsigned(v); as_i64 is Some for Signed and for Unsigned ≤ i64::MAX; as_f64 is always Some (an `as` cast). A lemma of value-roundtrip: without it, Signed(5) would read back as Unsigned(5).	(none directly; lemma)	EASY: private-field enum invariant; i64::unsigned_abs; i64::try_from(u64); `as` cast to f64	0,1,3
value-roundtrip	round-trip	ser::to_string, de::from_str, <Value as Serialize>::serialize, value::ValueVisitor, de::Deserializer::parse_number	∀ v: Value with every float finite and depth(v) ≤ 127: from_str::<Value>(&to_string(&v)?) = Ok(v′) with v′ structurally identical to v, floats compared by to_bits (so -0.0 keeps its sign). Uses number-normal-form, string-escape-roundtrip and float-f64-shortest-roundtrip. Corollary: ∀ b ∈ JSONa, from_str(to_string(Den(b))) = Den(b) bitwise, the lossless check the test helper `parse` makes on every accepted document.	generated_documents_parse_and_reserialize, mutated_documents_get_their_pinned_verdicts, named_edge_cases_get_their_pinned_verdicts	FREE given value-encoding, json-accepts-exactly-grammar and the three lemmas: composition over a recursive enum with Vec and BTreeMap; float bit equality	0,1,2,3,4,5,7,11
integer-format	contract	ser::{write_u128,write_i128}, ser::Serializer::serialize_{u8,u16,u32,u64,u128,i8,i16,i32,i64,i128}, ser::KeySerializer::{quoted_number,serialize_<int>}	∀ integer n of every width (u8..u128, i8..i128): to_string(&n) = Ok(n.to_string()): `-` iff n < 0, no leading zeros, `0` for zero, i128::MIN exact through unsigned_abs. write_u128(out, x) appends exactly the decimal digits of x (1 to 39 of them) and nothing else. As a map key, an integer is that decimal in quotes.	json::integer_formatting, wide_integers_round_trip_or_fail	EASY-MEDIUM: a while loop over a [u8;39] buffer with % and / by 10 on u128; slice get_mut/get; &mut Vec<u8>; loop invariant: buf[i..] encodes value / 10^k	0,1,2,3,5,7
string-escape-roundtrip	round-trip	ser::{write_json_string,hex_nibble}, ser::Serializer::{serialize_str,serialize_char}, de::Deserializer::{parse_string_body,parse_escape,parse_hex4}	∀ s: &str: to_string(&s) = Ok(Q(s)): only C0 controls, `"` and `\` are escaped, in Q's forms; every other byte (DEL, non-ASCII, `/`, U+2028/2029) is copied verbatim. from_str::<String>(&Q(s)) = Ok(s.to_owned()). ∀ c: char: to_string(&c) = Ok(Q(c.to_string())).	json::string_escaping_is_pinned_and_round_trips	MEDIUM: for loop with enumerate over bytes plus a chunk index; slice get(chunk..i); extend_from_slice; hex_nibble lookup via get().unwrap_or; char::encode_utf8. The writer half is EASY alone; the inverse needs the parse_string_body model.	0,1,2,3,4,5,7
float-f64-shortest-roundtrip	contract	ser::{write_f64,write_special,shortest_f64,split_exp,write_layout}, ser::Serializer::serialize_f64, de::from_str::<f64>	∀ x: f64: to_string(&x) = Ok(F64(x)). Not finite → `null`; ±0 → `0.0`/`-0.0`. For finite x ≠ 0 the text t is in the JSON number grammar and contains `.` or `e` (it never reads back as an integer); t.parse::<f64>() and from_str::<f64>(t) both have the bits of x; t's significant-digit count is the minimum over all decimals that round to x, and among those the digits are the closest to x; kk = e+1 ∈ -4..=16 gives fixed notation, anything else d.ddde±X.	json::float_formatting_is_pinned_and_round_trips, a_power_of_two_gets_the_shortest_digits	VERY HARD: format!("{:e}") and format!("{:.p$e}") (std float formatting; format_args uses an unsafe constructor the Trust model refuses); str::parse::<f64>; String filtering by chars; i32 arithmetic; loops pushing zeros; split_at. Needs a Clean model of binary64 rounding intervals and the shortest-digits theorem (Ryu/Grisu). See §5.5 for an unproved step.	0,1,3,5,7,8,14
float-f32-shortest	contract	ser::{write_f32,shortest_f32,write_layout,write_special}, ser::Serializer::serialize_f32	∀ y: f32: to_string(&y) = Ok(F32(y)). The f64 statement at binary32: the digits are the shortest that round to y as an f32 (not to the f64 it widens to), closest among them; t.parse::<f32>() has the bits of y; fixed notation iff kk ∈ -5..=13.	json::f32_formatting_is_pinned, a_power_of_two_gets_the_shortest_digits	VERY HARD: as float-f64-shortest-roundtrip, at binary32	0,1,3,5,7,8,14
float-parse-correctly-rounded	contract	de::Deserializer::{parse_number,visit_number,dispatch}, <&mut de::Deserializer as serde::Deserializer>::deserialize_f64 (forwarded to deserialize_any), serde f64 primitive visitor	∀ t in the JSON number grammar (after ws): from_str::<f64>(t) = Ok(nearest(t)) bitwise when nearest(t) is finite, Err otherwise. Integer tokens included: a token classified Unsigned/Signed goes through visit_u64/visit_i64 and `as f64`, which equals nearest(t); a token wider than 64 bits goes straight to nearest. `-0` gives -0.0; underflow gives ±0.	json::our_float_parsing_is_correctly_rounded	HARD: str::parse::<f64> (dec2flt model); the u64/i64 `as f64` rounding lemma; serde's primitive visitor (cross-crate)	0,1,3,5,11,12
typed-integer-targets	contract	de::Deserializer::{deserialize_integer,deserialize_wide,visit_number,dispatch,parse_number}, <&mut de::Deserializer as serde::Deserializer>::deserialize_{u8,i8,u16,i16,u32,i32,u64,i64,u128,i128}	∀ W ∈ {u8,i8,u16,i16,u32,i32,u64,i64,usize,isize}, ∀ t: from_str::<W>(t) = Ok(n) ⇔ t (after ws) is an integer token with no `.`/`e`, t ≠ `-0`, and dec(t) ∈ range(W); then n = dec(t). Otherwise Err: a fraction or exponent (`1.5`, `1e3`, `0e0`, `-0.0`); `-0`; out of range, including past 64 bits; a non-number, with the visitor's type error. For W = u128 the same holds with range u128. For W = i128 it holds with range i128 EXCEPT that `-0` is accepted as 0: deserialize_wide parses the text as i128 before the NegZero dispatch (serde_json does the same; §5.7). For W = f64 see float-parse-correctly-rounded; for W = Value the result is Den(t).	json::scalar_integer_targets_are_pinned, wide_integers_round_trip_or_fail, typed_deserialization_verdicts_and_values_are_pinned	MEDIUM-HARD: generic Visitor dispatch; serde's primitive visitors with their range checks (external crate); str::parse::<u64/i64/u128/i128>	0,1,2,3,5,11,12
to-value-exact	contract	lib::{to_value,from_value}, de::Deserializer::{exact,parse_number,end}	∀ T: Serialize, x: T: to_value(x) = Ok(v) ⇔ to_vec(&x) = Ok(b) ∧ every integer literal in b fits u64 or i64; then v = Den(b). Otherwise Err: the serializer's error, or `number out of range` for an integer past 64 bits. Floats are never silently rounded. from_value::<T>(v) = from_slice::<T>(&to_vec(&v)?). Corollary: to_value(u128::MAX) and to_value(i128::MIN) are Err; to_value(u64::MAX) and to_value(i64::MIN) are Ok.	json::to_value_and_from_value_round_trip, wide_integers_round_trip_or_fail	MEDIUM given the serializer and parser contracts: generic T: Serialize; composition; the exact_integers flag	0,1,2,3,4,11,12
key-serializer-refuses	contract	ser::KeySerializer (all serialize_* methods), ser::key_type_error, ser::KeySerializer::quoted_number, <ser::Compound as SerializeMap>::serialize_key, ser::{to_vec,to_string}, lib::to_value, macros::json_build ($other:expr arm)	∀ map key k: serialize_key(k) writes KeySer(k). ∀ key kind that is not string-like it returns Err("JSON object keys must be strings"). The Err propagates: to_vec, to_string and to_value of any value containing such a key are Err, never a document with the entry replaced by null. json!(e) for a non-literal expression e is to_value(&e).expect(..), so it panics exactly when to_value(&e) is Err.	json::a_value_that_cannot_serialize_is_refused	MEDIUM: serde::ser::Impossible; an FnOnce closure parameter (quoted_number); a panic via expect, as a specified panic; macro expansion	0,1,3,4,11,13
serializer-data-model	contract	ser::Serializer (all serialize_* methods), ser::Compound::{comma,finish,entry}, ser::Close, <ser::Compound as SerializeSeq/Tuple/TupleStruct/TupleVariant/Map/Struct/StructVariant>	∀ sequence of serde data-model calls a Serialize impl makes, the bytes appended are SerModel of that call tree: struct fields in call order, unsorted; None is null; a unit variant is a string; seqs, tuples and fixed arrays bracketed; a newtype transparent; a variant with a payload a single-entry object. Corollary: for derived types, to_string follows declaration order, and a field with skip_serializing_if is absent when skipped.	json::typed_models_serialize_to_pinned_bytes_and_read_back, to_value_and_from_value_round_trip	MEDIUM-HARD: serde Serializer trait with associated types; &mut Vec<u8> threaded through Compound; generic T: ?Sized + Serialize; the corollary needs the serde_derive expansion	0,1,2,3,4,9,11,12
deserializer-faithful	refinement	<&mut de::Deserializer as serde::Deserializer>::{deserialize_any,deserialize_option,deserialize_enum,deserialize_struct,deserialize_newtype_struct,deserialize_unit}, de::Deserializer::end_container, de::SeqAccess, de::MapAccess, de::{Deserializer::key,Key}, serde::de::value::TextKeyDeserializer (aterm-serde's buffered-text-key), de::EnumAccess (EnumAccess + VariantAccess)	∀ b ∈ JSONa, ∀ visitor: the calls the Deserializer makes follow Faithful(b). Consequences: (a) a visitor that takes exactly N elements ([T;N], a tuple, a newtype over [T;N]) succeeds only on arrays of exactly N elements (longer is Err at end_container, shorter is the visitor's Err); (b) every key in the document, unknown or duplicate, reaches the visitor, so deny_unknown_fields rejects unknown keys and derive's duplicate-field check rejects repeated keys (the Deserializer itself decides neither); (c) ∀ T whose derived Serialize and Deserialize are mutually inverse over the serde data model: from_str::<T>(&to_string(&x)?) = Ok(x) and from_value::<T>(to_value(&x)?) = Ok(x); (d) in particular every map whose keys KeySer writes reads back as itself, whatever its key type (integer of any width, bool, char, unit variant, newtype or String), whether read directly or through serde's buffer (#[serde(flatten)], untagged, internally tagged), and an integer or bool key is read from KeySer's spelling and no other.	json::typed_deserialization_verdicts_and_values_are_pinned, deny_unknown_fields_denies, typed_models_serialize_to_pinned_bytes_and_read_back, to_value_and_from_value_round_trip, every_map_the_writer_accepts_reads_back, a_key_reads_back_only_in_the_spelling_the_writer_uses	VERY HARD: serde Deserializer/SeqAccess/MapAccess/EnumAccess/VariantAccess impls generic over the visitor; &mut borrows nested in access structs; serde_derive impls; a higher-order spec quantified over an arbitrary visitor	0,1,2,3,4,5,7,9,11,12,13
json-macro-denotation	other (macro denotation)	macros::{json,json_build}	∀ json! invocation: json!(null)/json!(true)/json!(false) are Null/Bool(true)/Bool(false); json!([e1,…,en]) = Array([J(e1),…,J(en)]); json!({k1: e1, …}) = Object(foldl insert (ki.into(), J(ei)) from an empty Map), so a repeated key is last-wins; [] and {} give empty containers; a trailing comma is allowed; any other expression e gives to_value(&e).expect(..). J is json! applied recursively to nested [] and {} token trees. With value-encoding, to_string(json!(…)) is the sorted compact document.	json::the_json_macro	MEDIUM: a macro_rules token muncher. Trust sees only the expansion at each call site, so the proof is per expansion arm; there is no surface yet for a theorem about the macro itself.	0,1,4,11
json-pointer-rfc6901	contract	value::Value::pointer, value::array_index	∀ v: Value, p: &str: pointer("") = Some(v); p not starting with `/` gives None; otherwise fold over p.split('/').skip(1), each token unescaped `~1`→`/` first, then `~0`→`~`: an Object steps to map.get(token), an Array to items.get(array_index(token)?), anything else or any miss is None. array_index(t) = Some(n) ⇔ t ∈ (0 | [1-9][0-9]*) ∧ n = dec(t) ≤ usize::MAX; so `00`, `+0`, `-` and non-digits give None.	json::json_pointer	MEDIUM: str::split iterator, str::replace (allocation), starts_with, str::parse::<usize> (which accepts a leading `+`, hence the explicit guard), a loop with `?` on Option, BTreeMap::get, Vec::get. array_index alone is EASY.	0,1,3,5,7
index-total	no-panic	<Value as Index<&str>>::index, <Value as Index<usize>>::index, value::Value::{get,get_index,is_null,as_u64}	∀ v: Value, k: &str, i: usize: the index sugar never panics. v[k] is the value under k if v is an object holding k, else &Null. v[i] is items[i] if v is an array with i < len, else &Null. Indexing a scalar always gives Null.	json::index_sugar_is_total	EASY: enum match, BTreeMap::get, slice get, unwrap_or on a reference to a static	0,1,3
```

### Distribution by kind (19)

| kind | count | ids |
|---|---|---|
| contract | 11 | json-accepts-exactly-grammar, value-encoding, integer-format, float-f64-shortest-roundtrip, float-f32-shortest, float-parse-correctly-rounded, typed-integer-targets, to-value-exact, key-serializer-refuses, serializer-data-model, json-pointer-rfc6901 |
| invariant | 2 | recursion-limit, number-normal-form |
| round-trip | 2 | value-roundtrip, string-escape-roundtrip |
| equivalence | 1 | skip-equals-materialize |
| refinement | 1 | deserializer-faithful |
| no-panic | 1 | index-total |
| other | 1 | json-macro-denotation |

### Blocking-gap histogram (gap → specs it blocks)

| gap | specs | gap | specs |
|---|---|---|---|
| 0 | 19 | 7 | 11 |
| 1 | 19 | 8 | 2 |
| 2 | 11 | 9 | 2 |
| 3 | 18 | 11 | 12 |
| 4 | 8 | 12 | 5 |
| 5 | 13 | 13 | 2 |
| | | 14 | 3 |

No spec is blocked by gap 0 alone. Every spec needs gap 1 (`&str`, `&[u8]`, `Vec`, `String` and
`Value` as content-modelled carriers), and every spec except the macro needs gap 3 (E6 body widening).
None needs gaps 6, 10, 15, 16 or 17 specifically. Those gaps (and the std numeric parse/format models
in §5.6) apply to the whole migration.

### Proof dependency order

`number-normal-form`, `integer-format`, `array_index` → the writer half of `string-escape-roundtrip`
→ `value-encoding` (floats assumed) → `recursion-limit` → `skip-equals-materialize` lemma
(`skip_string_body` ≡ `parse_string_body`) → `json-accepts-exactly-grammar` → `value-roundtrip`
(needs the float specs for its float case) → `typed-integer-targets`, `to-value-exact`,
`key-serializer-refuses`, `serializer-data-model` → `deserializer-faithful`. The float specs
(`float-parse-correctly-rounded`, `float-f64-shortest-roundtrip`, `float-f32-shortest`) form a
separate chain that needs an IEEE model first.

## 2. Test → spec map (TSV)

`primary` is the spec that retires the test. `also` lists the other specs the same test samples.
Every file is `crates/aterm-json/tests/json.rs`.

```tsv
file	test	primary	also
crates/aterm-json/tests/json.rs	ignored_strings_validate_the_same_bytes_as_materialized_values	skip-equals-materialize	
crates/aterm-json/tests/json.rs	generated_documents_parse_and_reserialize	json-accepts-exactly-grammar	value-roundtrip
crates/aterm-json/tests/json.rs	mutated_documents_get_their_pinned_verdicts	json-accepts-exactly-grammar	value-roundtrip
crates/aterm-json/tests/json.rs	arbitrary_bytes_get_their_pinned_verdicts	json-accepts-exactly-grammar	
crates/aterm-json/tests/json.rs	named_edge_cases_get_their_pinned_verdicts	json-accepts-exactly-grammar	value-roundtrip
crates/aterm-json/tests/json.rs	the_recursion_limit	recursion-limit	(bound in the doc is off by one, §5.2)
crates/aterm-json/tests/json.rs	float_formatting_is_pinned_and_round_trips	float-f64-shortest-roundtrip	
crates/aterm-json/tests/json.rs	f32_formatting_is_pinned	float-f32-shortest	
crates/aterm-json/tests/json.rs	a_power_of_two_gets_the_shortest_digits	float-f64-shortest-roundtrip	float-f32-shortest
crates/aterm-json/tests/json.rs	integer_formatting	integer-format	
crates/aterm-json/tests/json.rs	string_escaping_is_pinned_and_round_trips	string-escape-roundtrip	
crates/aterm-json/tests/json.rs	typed_models_serialize_to_pinned_bytes_and_read_back	serializer-data-model	deserializer-faithful
crates/aterm-json/tests/json.rs	typed_deserialization_verdicts_and_values_are_pinned	deserializer-faithful	typed-integer-targets
crates/aterm-json/tests/json.rs	scalar_integer_targets_are_pinned	typed-integer-targets	(pins the i128 `-0` case, §5.7)
crates/aterm-json/tests/json.rs	wide_integers_round_trip_or_fail	typed-integer-targets	integer-format, to-value-exact
crates/aterm-json/tests/json.rs	a_value_that_cannot_serialize_is_refused	key-serializer-refuses	
crates/aterm-json/tests/json.rs	every_map_the_writer_accepts_reads_back	deserializer-faithful	key-serializer-refuses, integer-format
crates/aterm-json/tests/json.rs	a_key_reads_back_only_in_the_spelling_the_writer_uses	deserializer-faithful	key-serializer-refuses
crates/aterm-json/tests/json.rs	deny_unknown_fields_denies	deserializer-faithful	(one row is mislabelled, §5.1)
crates/aterm-json/tests/json.rs	the_json_macro	json-macro-denotation	value-encoding
crates/aterm-json/tests/json.rs	to_value_and_from_value_round_trip	to-value-exact	serializer-data-model, deserializer-faithful
crates/aterm-json/tests/json.rs	value_display_is_compact	value-encoding	
crates/aterm-json/tests/json.rs	our_float_parsing_is_correctly_rounded	float-parse-correctly-rounded	
crates/aterm-json/tests/json.rs	json_pointer	json-pointer-rfc6901	
crates/aterm-json/tests/json.rs	index_sugar_is_total	index-total	
crates/aterm-json/src/lib.rs	json_const_helpers_round_trip	recursion-limit	json-accepts-exactly-grammar
```

## 3. Coverage

- 25 tests read. All 25 map to one of the 19 specs. 18 specs retire at least one test as primary;
  `number-normal-form` retires none and is a lemma of `value-roundtrip`.
- The biggest collapse: `json-accepts-exactly-grammar` retires the four input sweeps (generated,
  mutated, arbitrary and named), which together pin several thousand verdicts.
- `deserializer-faithful` retires 4 tests as primary and is also sampled by 2 more. It is the hardest
  spec (gaps 9 and 13). `to_value_and_from_value_round_trip` and
  `typed_models_serialize_to_pinned_bytes_and_read_back` both need it before they can go.
- A test goes only when all its specs (primary and `also`) are Certified. Example:
  `wide_integers_round_trip_or_fail` needs `typed-integer-targets`, `integer-format` and
  `to-value-exact`.

## 4. Tests that are not spec samples

None. Every test samples at least one general property. Two pieces of test-only code go with the tests:

| item | why | action |
|---|---|---|
| `Golden` (FNV-1a digest) and the digest constants `GENERATED`, `MUTATED`, `ARBITRARY`, `NAMED`, `RECURSION`, `FLOATS`, `F32S`, `STRINGS`, `TYPED`, `TYPED_READS`, `SCALARS` in `crates/aterm-json/tests/json.rs` | These are golden pins, not specs (§5.3). They exist only to compress the sweeps. | **delete** each constant in the commit that deletes the last sweep using it, and `Golden` with the last constant |
| the `parse` helper (lossless re-serialization check) in `crates/aterm-json/tests/json.rs` | It samples `value-roundtrip` with a weaker equality (§5.4). | **delete** with the last test that calls it |

(`test_support/serde_corpus.rs` is shared by other crates through `#[path]` and is not part of this
crate's migration.)

## 5. Implied specs that look wrong, or disagree with the code

1. **Duplicate-key labelling clarified (resolved 2026-10-27).**
   - `deny_unknown_fields_denies` documents that `{"a":1,"b":"x","a":2}` fails through the derived
     struct visitor's duplicate-field guard rather than an unknown field.
   - `deserializer-faithful` (b) states the real rule: the Deserializer shows every key, and the
     visitor decides (`Value`/`Map` is last-wins; derived structs reject duplicates).
2. **Recursion bound prose aligned with `127` (resolved 2026-10-27).** `de::RECURSION_LIMIT` (`128`)
   and `lib::recursion_limit()` (`127`) both state that at most `127` nested containers are accepted
   and `128` is refused, matching `recursion-limit`.
3. **The digest sweeps pin agreement with a reference that is gone.** Each digest was "recorded
   when every verdict and every byte matched serde_json", but serde_json is no longer in the tree.
   - The spec those tests imply, agreement with serde_json, has no reference to state it against.
   - A failure gives no diagnostic, and re-recording a digest silently accepts any change.
   - `json-accepts-exactly-grammar`, the float writers, `typed-integer-targets` and
     `string-escape-roundtrip` restate that agreement as grammar and denotation properties. The
     digests are not specs.
4. **Lossless check now preserves `-0.0` sign bits (resolved 2026-10-27).** The `parse` helper in
   `tests/json.rs` now recursively compares number classifications and `f64::to_bits` alongside
   `PartialEq`, catching any loss of `-0.0` (`Num::NegZero`) as stated by `value-roundtrip`.
5. **The shortest-digits sweep is close to a tautology, and one doc claim is unproved.**
   - In `a_power_of_two_gets_the_shortest_digits`, the digit-count assertion follows from the
     construction. `shortest_f64`/`shortest_f32` take their precision from `{:e}` and use the closest
     decimal of that length only if it reads back, so they cannot have fewer digits without
     contradicting `{:e}`'s minimality. Only the four pinned literals and the read-back checks carry
     information.
   - The `ser.rs` doc on `shortest_f64` claims that when the closest decimal does not read back,
     `{:e}`'s digits are "the only decimal of that length the interval holds". This is unproved, and
     `float-f64-shortest-roundtrip` depends on it. It needs a proof or a counterexample before the spec
     is attempted.
6. **No audit gap covers std numeric parsing and formatting.** `json-accepts-exactly-grammar`,
   `float-parse-correctly-rounded`, `typed-integer-targets`, `json-pointer-rfc6901` and both float
   writers rest on `str::parse::<u64/i64/u128/i128/usize/f64>` (dec2flt) and the `{:e}` float
   formatter. That needs Clean models of integer parsing, a correctly rounded dec2flt and shortest
   float formatting. Add a row to the audit §7 table (std numeric parse and format models), or factor
   the parse onto first-party code that Trust can import.
7. **`-0` `i128` path documented in `de.rs` and `typed-integer-targets` (resolved 2026-10-27).**
   `deserialize_wide` (`src/de.rs`) parses `-0` as `i128` `0` before the `NegZero` dispatch (matching
   `serde_json`), while `u128` and narrower integer targets reject `-0`. Both `visit_number`'s doc
   and `typed-integer-targets` state the `i128` path explicitly.

## 6. The specs I would prove first (easiest, high value)

No spec is blocked by gap 0 alone. Every one needs at least gap 1, so these are ordered by the smallest
gap set and by what each unlocks.

1. **`index-total`** (gaps 0,1,3). An enum match, `BTreeMap::get`, slice `get` and a static `Null`.
   There are no loops. It retires `index_sugar_is_total` and is the cleanest first use of a `Value`
   carrier, the gap-1 work every other spec needs.
2. **`number-normal-form`** (gaps 0,1,3). A private-field enum invariant plus three accessors. It retires
   no test alone, but it is the lemma that makes `value-roundtrip` true. It is also the smallest real
   type invariant in the crate.
3. **`integer-format`** (gaps 0,1,2,3,5,7). One digit loop over a `[u8;39]` buffer on `u128`, with an
   obvious invariant. It retires `integer_formatting` and is a third of `wide_integers_round_trip_or_fail`.
   It is also the lemma behind integer keys in `key-serializer-refuses`.

Runner-up: `array_index` (canonical decimal, the core of `json-pointer-rfc6901`) and the writer half of
`string-escape-roundtrip` (`Q(s)`, one byte loop). After them, `recursion-limit` is the first parser spec
and the cheapest one with a security property: bounded stack on any input. The float specs and
`deserializer-faithful` go last, since they need an IEEE/shortest-digits model (gaps 8, 14) and
higher-order serde specs (gaps 9, 13).
