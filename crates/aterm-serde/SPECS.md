<!-- Copyright 2026 Andrew Yates -->
<!-- SPDX-License-Identifier: Apache-2.0 -->

# aterm-serde: spec recovery from its unit tests

> Status 2026-10-19: this is the crate's migration worklist. Each of the 22 specs below is
> proved through Trust/Clean about the compiled Rust, and then the tests it subsumes are
> deleted. No test has been deleted yet. None of the 190 mapped tests is a non-spec check, so
> nothing can be deleted ahead of a proof (§4).

Source: `crates/aterm-serde` (read only), plus the derive it tests (`crates/aterm-serde-derive`,
`crates/aterm-macro-support`). The tests are `tests/{de 47, de_error 36, annotations 23, ser 22,
enum_untagged 17, enum_internally_tagged 16, derive_input 13, attrs 8, untrusted 4, value 2,
text_keys 2, ignored_any 1}` = 191, all run by `tests/serde_suite.rs`. `src/` has no `#[test]`.
`derive_input::self_inside_a_type_macro_remains_the_macros_input` landed after this map and is
not mapped yet; the other 190 are.
`tests/support/` is the serde_test token harness. It is test-only code that the tests use to
observe the crate. It is not itself tested, and no spec is about it.
The first recovery pass read 176 tests. The merge read the 12 in `derive_input.rs` and added
`derive-input-parse` for them. Nothing was compiled or run. The claims below come from reading
the code. `buffered-text-key` and its 2 `text_keys` tests came with ATERM-KI-1015-7's fix, and
were run.

Result: **22 specs**, covering **190 of the 191 tests**. **0 tests are non-spec samples** (§4).

## 0. Notation used in the statements

- `Tok`: the serde data-model event alphabet that `support::Token` spells: `Bool`, `I8..I64`,
  `U8..U64`, `F32`, `F64`, `Char`, `Str|BorrowedStr|String` (the three *string carriers*),
  `Bytes|ByteBuf`, `None`, `Some`, `Unit`, `UnitStruct{name}`, `UnitVariant{name,variant}`,
  `NewtypeStruct{name}`, `NewtypeVariant`, `Seq{len:Option}`/`SeqEnd`, `Tuple{len}`/`TupleEnd`,
  `TupleStruct`/`TupleStructEnd`, `TupleVariant`/`TupleVariantEnd`, `Map{len:Option}`/`MapEnd`,
  `Struct{name,len}`/`StructEnd`, `StructVariant`/`StructVariantEnd`, `Enum{name}`.
- `Ser(v)`: the sequence of `Serializer`/`SerializeSeq`/`SerializeMap`/`SerializeStruct`/… method
  calls that `<T as Serialize>::serialize(&v, s)` makes on an arbitrary `Serializer` `s`, written as
  `Tok`. A recording serializer sees exactly this trace. `SerErr(v)` is the `Err` the impl returns,
  together with the trace up to that point.
- A *self-describing source* for `ts`: any `Deserializer` whose `deserialize_*` calls drive the
  `Visitor` with the `visit_*` calls that `ts` encodes. `support::Deserializer` is one model of it.
  `De_T(ts)` is the result of `T::deserialize` over that source, with `ts` consumed exactly.
  `Accepts_T(v) = {ts | De_T(ts) = Ok(v)}`.
- `InPlace_T(ts, p)`: `T::deserialize_in_place` over the same source, starting from place `p`.
- `msg(e) = e.to_string()` for the source's error type, which is built only through
  `de::Error::custom`/`ser::Error::custom`. So every message is fixed by the default constructors in
  `src/de/mod.rs` (`declare_error_trait!`) and `src/ser/mod.rs`.
- `Exp(V)`: the text `V::expecting` writes. `Unexp(x)`: `Unexpected::fmt(x)`. `Range(T)`: the
  mathematical integer interval `[T::MIN, T::MAX]`.
- `Shape(T)`: the derive-free data-model encoding the std impls give `T` (`src/ser/impls.rs`,
  `src/de/impls.rs`). `Derive(T, attrs)`: what `aterm-serde-derive` generates for `T`, with the
  attribute set in `docs/measured/third-party-2026-10-15/serde-attrs.tsv` (`rename`, `rename_all`,
  `alias`, `skip`, `skip_serializing_if`, `default`, `default = path`, `deserialize_with`,
  `deny_unknown_fields`, `flatten`, `tag`, `untagged`, `transparent`).
- `key(f)`: a field's or variant's wire name: `rename(f)` if present, else the container's
  `rename_all` rule applied to the ident (`derive-rename-rules`), else the ident.
- `Buf(ts)`: the `Content` tree that `ContentVisitor` builds from `ts` (`src/private/de.rs`).
  `CRef(c) = ContentRefDeserializer::new(&c)` and `CDe(c) = ContentDeserializer::new(c)`.
- `FlatBuf`: the `Vec<Option<(Content, Content)>>` that a struct with `flatten` fields gathers from
  its unrecognised entries, in input order.
- **Generic code.** Most specs quantify over `T`, `Serializer` and `Deserializer`. Until gap 11
  closes, every one of them is proved per instantiation: the types and the harness's
  serializer/deserializer the tests use today. The harness is how we observe the code, not what we
  prove.
- **Gap 0** (the installed seal publishes no Clean row) applies to every spec, so it is left out of
  the column. The local seal `150efe4a` closes it (PLAN, "Seal `150efe4a`"). No spec is blocked by
  gap 0 alone: every row also needs at least one of gaps 1–13.

## 1. Specs (TSV)

Columns: `id`, `kind`, `functions`, `statement`, `subsumes` (primary tests; the full map is in §2),
`difficulty` (the Rust features the proof must handle), and `blocking_gaps` (audit §7 numbers).

```tsv
id	kind	functions	statement	subsumes	difficulty	blocking_gaps
size-hint-cautious	contract	private::size_hint::{cautious,from_bounds,helper}	∀ Element, hint: Option<usize>: cautious::<Element>(hint) = 0 if size_of::<Element>() = 0, else min(hint.unwrap_or(0), 1048576 / size_of::<Element>()). Corollary: cautious::<E>(h) · size_of::<E>() ≤ 1 MiB ∀ h, including Some(usize::MAX). helper((l, Some(u))) = Some(u) iff l = u, and helper(b) = None otherwise. Composed: for every source and len hint, the initial capacity of each buffer at de/impls.rs:569/606 (Vec<T>), de/impls.rs:962 (HashMap<K,V>) and private/de.rs:343/356/726 (the Content seq/map and the tagged-content Vec) is cautious::<Elem>(hint). So no hint makes preallocation exceed 1 MiB, and the decoded value equals the elements actually read.	untrusted::a_huge_seq_hint_preallocates_at_most_the_cap, a_huge_map_hint_does_not_size_the_table, a_huge_hint_into_the_untagged_buffer_preallocates_at_most_the_cap	EASY core: mem::size_of, unwrap_or, min, division by a nonzero constant (omega-class once instantiated per Element). The composed half needs with_capacity and callee-ensures at six call sites.	1,3,4,11
de-int-convert	table-property	de::impls::impl_deserialize_num! for {i8..i64,isize,u8..u64,usize,i128,u128} via num_self!, num_as_self!, int_to_int!, int_to_uint!, uint_to_self!, num_128!; de::Error::invalid_value; de::Unexpected::fmt	∀ target T ∈ {i8,i16,i32,i64,isize,u8,u16,u32,u64,usize,i128,u128}, source width S ∈ {i8..i64,u8..u64} (plus the 128-bit crossover: i128 from u128, u128 from i128), v: S: visit_S(v) = Ok(v as T) when the value of v lies in Range(T). Otherwise it is Err(invalid_value(Signed(v as i64) if S is signed else Unsigned(v as u64), &visitor)), so msg = "invalid value: integer `{v}`, expected {T}". The 128-bit crossover reports Unexpected::Other("u128"|"i128"). isize/usize request deserialize_i64/deserialize_u64. The result depends on the value only, not on the source's token kind.	de::test_{i8,i16,i32,i64,i128,isize,u8,u16,u32,u64,u128,usize}; de_error::test_{i8,i16,i32,i64,u8,u16,u32,u64,u128,usize}	EASY-MEDIUM core (in range ⇔ try_from succeeds and preserves the value; casts and MIN/MAX comparisons). Then macro-expanded methods over generic E: de::Error, map_err with a closure, and the format_args error path.	1,3,11,13
de-float-convert	contract	de::impls::impl_deserialize_num! for {f32,f64} via num_self!, num_as_self!, num_as_copysign_self!	∀ x: f32: f64::deserialize from visit_f32(x) = Ok((x as f64).copysign(x)), which is exact widening. ∀ y: f64: f32::deserialize from visit_f64(y) = Ok((y as f32).copysign(y)). Same-width visits are the identity, and integer visits give Ok(v as F). Hence is_sign_positive(result) = is_sign_positive(input) for every input, NaN included.	de::test_f32, test_f64, test_nan	MEDIUM: f32/f64 casts, copysign, is_sign_positive, generic E: Error. Floats are outside every Trust lane today; they fall under gap 3 (casts) and need a float model.	3,11
de-error-messages	contract	de::Error::{invalid_type,invalid_value,invalid_length,unknown_variant,unknown_field,missing_field,duplicate_field} (default methods, declare_error_trait!); de::OneOf::fmt; de::Unexpected::fmt; de::WithDecimalPoint::fmt; dyn Expected::fmt	∀ E: de::Error, each default constructor equals E::custom of exactly: invalid_type(u,e) = "invalid type: {Unexp(u)}, expected {e}"; invalid_value(u,e) = "invalid value: {Unexp(u)}, expected {e}"; invalid_length(n,e) = "invalid length {n}, expected {e}"; missing_field(f) = "missing field `{f}`"; duplicate_field(f) = "duplicate field `{f}`"; unknown_field(f,xs) = "unknown field `{f}`, there are no fields" if xs = [], else "unknown field `{f}`, expected {OneOf(xs)}". unknown_variant is the same with variant/variants. OneOf([a]) = "`a`", OneOf([a,b]) = "`a` or `b`", and for n ≥ 3, OneOf(xs) = "one of " ++ join(", ", ["`x`" | x ← xs]). Unexp follows the table at de/mod.rs:305: Str(s) is "string {s:?}", Unit is "unit value", Seq is "sequence", and Float(f) is "floating point `{WithDecimalPoint(f)}`". WithDecimalPoint(f) = Display(f) ++ (".0" if f is finite and Display(f) has no '.'), and Display(f) when f is not finite ("NaN", "inf").	de_error::test_integer_from_float, test_nan_no_decimal_point, test_number_from_string, test_bool_from_string	MEDIUM: trait default methods over generic Self, fmt::Formatter, a write! state machine (LookForDecimalPoint implements fmt::Write over &mut), an enumerate loop, &'static [&'static str], f64 Display.	1,2,3,7,11,13
de-std-invalid-type	table-property	de::Visitor default visit_* methods (de/mod.rs); de::impls visitors UnitVisitor, BoolVisitor, StringVisitor, VecVisitor, ArrayVisitor<[T;0]>, map_impl! (BTreeMap, HashMap), PrimitiveVisitor	∀ std visitor V and every visit_X that V does not override: visit_X(x) = Err(invalid_type(Unexp_of(X,x), &V)). Unexp_of maps visit_unit (which a unit_struct source also drives) to Unit, visit_seq to Seq, visit_str to Str(s) and visit_f32/f64 to Float. Exp(V) is fixed per type: () "unit", bool "a boolean", String "a string", Vec "a sequence", [T;0] "an empty array", maps "a map", ints by type name, and a derived unit struct X "unit struct X". So msg = "invalid type: {Unexp}, expected {Exp(V)}" for every such pair. [T;0] accepts an empty seq and rejects unit. () rejects every seq, including an empty one with or without a length.	de_error::test_unit_from_empty_seq, test_unit_from_empty_seq_without_len, test_unit_from_tuple_struct, test_string_from_unit, test_vec_from_unit, test_vec_from_unit_struct, test_zero_array_from_unit, test_zero_array_from_unit_struct, test_btreemap_from_unit, test_btreemap_from_unit_struct, test_hashmap_from_unit, test_hashmap_from_unit_struct, test_unit_struct_from_seq	EASY-MEDIUM given de-error-messages: trait default methods, generic Visitor/E, &self as a dyn Expected, format_args.	1,4,11,13
de-std-shape	refinement	de::impls Deserialize for bool, char, String, Option<T>, (), Vec<T>, [T;N], tuples (tuple_impls!), BTreeMap/HashMap (map_impl!), Box<T> (forwarded_impl!), PathBuf (PathBufVisitor)	∀ std T, Accepts_T(v) is exactly the following, compositionally in the element types. bool: Bool(v). char: Char(v), or a string carrier holding exactly one char. String: any string carrier with content v, or Char(c) with v = c.to_string(). Option<T>: None, Unit, or Some ++ Accepts_T(x). (): Unit. Vec<T>: (Seq{len:_} | TupleStruct{_,_}) ++ concat(map(Accepts_T, v)) ++ the matching end, where the len hint does not affect the result. [T;N]: Seq or Tuple of exactly N elements; fewer fail with invalid_length(k, "an array of length N"). Tuples (T1..Tn): Seq or Tuple of exactly n elements; fewer fail with invalid_length(k, "a tuple of size n"). BTreeMap/HashMap: (Map{len:_} | Struct{_,_}) of key/value pairs, where a later duplicate key overwrites an earlier one. Box<T>: Accepts_T. PathBuf: any string carrier, or Bytes/ByteBuf whose UTF-8 content equals the path. Every accepted ts is consumed exactly.	de::test_bool, test_char, test_string, test_option, test_unit, test_vec, test_array, test_tuple, test_btreemap, test_box, test_boxed_slice, test_path_buf; de_error::test_short_tuple, test_short_array	MEDIUM-HARD: generic impls over Deserializer/Visitor/SeqAccess/MapAccess (&mut), while-let loops pushing into Vec/HashMap/BTreeMap, const-generic arrays, String/PathBuf/OsString, str::from_utf8, char counting.	1,2,4,5,7,11,13
de-in-place-agrees	equivalence	Deserialize::deserialize_in_place (the default, and the overrides for String, Vec<T>, [T;N], tuples and derived structs)	∀ T, ts, p: if De_T(ts) = Ok(v), then InPlace_T(ts, p) = Ok(()), leaves p = v, and consumes ts exactly. If De_T(ts) is Err, InPlace_T(ts, p) is Err as well.	(no primary test. Every assert_de_tokens call checks this as its second half, so every de test is a secondary sample)	MEDIUM: &mut place updates (Vec truncate/extend, String clear/push_str, per-element in-place recursion), generics, loops.	1,2,7,11,13
ignored-any-skips-one-value	contract	de::ignored_any::IgnoredAny::{deserialize, visit_*}; Deserializer::deserialize_ignored_any default forwarding	∀ well-formed single data-model value ts (any scalar, string, bytes, option, unit, seq/tuple/map/struct with balanced ends, or an enum with any variant payload): IgnoredAny::deserialize over ts = Ok(IgnoredAny) and consumes exactly ts. Corollary: for a derived struct S without deny_unknown_fields and any unknown key k, De_S(prefix ++ [k] ++ ts ++ suffix) = De_S(prefix ++ suffix).	de::test_ignored_any; ignored_any::test_deserialize_enum. Every de.rs test that calls `test` is a secondary sample through assert_de_tokens_ignore.	MEDIUM: recursion through visit_seq/visit_map/visit_enum (trait-generic SeqAccess/MapAccess/EnumAccess), while-let loops.	1,2,7,11,13
ser-std-shape	refinement	ser::impls Serialize for (), bool, i8..i64, u8..u64, f32, f64, char, str, String, Option<T>, [T], Vec<T>, [T;N], tuples, BTreeMap<K,V>, Box<T>, Path/PathBuf	∀ v of these types, Ser(v) = Shape(v), defined recursively: () → Unit. Each scalar → its own token at its own width (i8 → I8, …, f64 → F64). char → Char. str/String → Str. None → None. Some(x) → Some ++ Ser(x). Slices and Vec → Seq{len:Some(n)} ++ concat(Ser(xi)) ++ SeqEnd. [T;N] → Tuple{len:N} ++ … ++ TupleEnd, including N = 0. A tuple of arity n → Tuple{len:n} ++ … ++ TupleEnd. BTreeMap → Map{len:Some(n)} ++ the (Ser(k) ++ Ser(v)) pairs in key order ++ MapEnd. Box<T> → Ser(*b). Path/PathBuf p: if p.to_str() = Some(s), then Ser(p) = Str(s). Otherwise serialize returns Err(custom("path contains invalid UTF-8 characters")) and makes no serializer call.	ser::test_unit, test_bool, test_isizes, test_usizes, test_floats, test_char, test_str, test_option, test_slice, test_array, test_vec, test_tuple, test_btreemap, test_box, test_boxed_slice, test_path_buf, test_cannot_serialize_paths	MEDIUM: generic impls over Serializer and its compound traits (&mut), iterator loops (collect_seq/collect_map), const-generic arrays, BTreeMap iteration order, OsStr::to_str.	1,2,4,7,11,13
ser-128-default-unsupported	contract	ser::Serializer::serialize_i128 / serialize_u128 (default methods)	∀ Serializer S that does not override them, ∀ v: i128 (resp. u128): S.serialize_i128(v) = Err(S::Error::custom("i128 is not supported")) (resp. "u128 is not supported"), and the call makes no other serializer call. Since i128/u128 serialize through these methods, Ser(v) is that error.	ser::test_integer128	EASY: a trait default method over generic Self, a 128-bit scalar argument, Error::custom.	1,11
derive-struct-ser	refinement	aterm-serde-derive ser.rs (generated Serialize for named, tuple, newtype, unit and transparent structs); ser::SerializeStruct	∀ struct S deriving Serialize, ∀ s. Named: Ser(s) = Struct{name, len:|W|} ++ concat over f ∈ W, in declaration order, of (Str(key(f)) ++ Ser(s.f)) ++ StructEnd, where W = the fields that are not #[serde(skip)] and have no skip_serializing_if(pred) with pred(&s.f) true. So the announced len counts only the written fields. Tuple: TupleStruct{name,n} ++ fields ++ TupleStructEnd. Newtype: NewtypeStruct{name} ++ Ser(inner). Unit: UnitStruct{name}. #[serde(transparent)]: Ser(s) = Ser(the single field), with no wrapper, and De_S = De_inner wrapped.	ser::test_unit_struct, test_tuple_struct, test_struct; attrs::skip_serializing_if_drops_the_field_and_its_length_slot, transparent_is_the_inner_value	MEDIUM per type: proc-macro-generated code (one instantiation per derived type), a generic Serializer, &mut compound serializers, predicate calls through paths.	1,2,4,9,11,12
derive-struct-de	refinement	aterm-serde-derive de/struct_.rs, de/identifier.rs, de/unit.rs, de/tuple.rs (generated Deserialize and field-identifier visitor); private::de::missing_field	∀ derived named struct S, let R be its non-skipped fields in order, N(f) = {key(f)} ∪ aliases(f), and All = the concatenation of the N(f) in order. Accepts_S is exactly: (a) Map{_} or Struct{S,_} whose keys are string carriers, Bytes, or integers i (i < |R| selects R[i]). A key in N(f) sets f, and a second occurrence fails with duplicate_field(key(f)). Any other key, a skipped field's name included: with deny_unknown_fields it fails with unknown_field(k, All), which reads "there are no fields" when R = []; without it, its value is skipped as IgnoredAny. When the map ends, each unset f takes, in order of preference, its deserialize_with/default/default = path value, the container #[serde(default)] instance's field, or None if f's type is Option (private::missing_field). Otherwise the input fails with missing_field(key(f)). Skipped fields take Default::default() or the container default's field. (b) Seq{_} of exactly |R| elements read positionally; fewer fail with invalid_length(k, "struct S with |R| elements"). deserialize_with(fn) reads f through fn, and an Err from fn propagates unchanged. Tuple structs accept Seq/Tuple/TupleStruct of exactly n elements. Newtype structs accept NewtypeStruct ++ inner. Unit structs (any const generics) accept Unit or UnitStruct{_}, and any other input fails with invalid_type(…, "unit struct S").	de::test_unit_struct, test_generic_unit_struct, test_newtype_struct, test_tuple_struct, test_struct, test_struct_borrowed_keys, test_struct_owned_keys, test_struct_with_skip, test_struct_skip_all, test_struct_skip_default, test_struct_skip_all_deny_unknown, test_struct_default; de_error::test_unknown_field, test_skipped_field_is_unknown, test_skip_all_deny_unknown, test_duplicate_field_struct; attrs::field_default_bare_and_path_fill_missing_keys, container_default_fills_from_the_type_and_unknown_keys_still_fail, a_missing_field_without_default_names_the_field, a_struct_reads_positionally_from_a_sequence, deserialize_with_runs_the_named_function; annotations::test_unknown_field_rename_struct, test_skip_struct	HARD per type: proc-macro output, generic Deserializer/MapAccess/SeqAccess (&mut), Option<T> accumulator locals, a while-let loop over keys, a match on string literals and integer indices, Default::default calls, user fn calls (deserialize_with).	1,2,4,5,7,9,11,12,13
derive-rename-rules	contract	aterm-serde-derive case::RenameRule::{from_str,apply_to_variant,apply_to_field}	from_str(s) = Some(r) iff s ∈ {"lowercase","UPPERCASE","PascalCase","snake_case","kebab-case"}, with the matching rule. apply_to_variant(v): None and PascalCase give v; LowerCase gives ascii_lower(v); UpperCase gives ascii_upper(v); SnakeCase gives the concatenation over (i,c) of (("_" if i > 0 ∧ c is uppercase) ++ ascii_lower(c)); KebabCase gives SnakeCase(v) with '_' replaced by '-'. apply_to_field(f): None, LowerCase and SnakeCase give f; UpperCase gives ascii_upper(f); PascalCase drops each '_' and upper-cases the first char and each char after an '_'; KebabCase replaces '_' with '-'. An explicit #[serde(rename)] on a variant or field overrides the container rule. Both directions use the result as the key, and an unknown spelling fails with unknown_variant(s, the renamed names).	attrs::rename_all_cases_aterm_uses	EASY-MEDIUM: &str/String building, chars/char_indices loops, char::is_uppercase, ASCII case mapping, str::replace. It is plain Rust that happens to live in a proc-macro crate; moving case.rs into a library crate drops gaps 9 and 12.	1,3,7,9,12
derive-enum-external	refinement	aterm-serde-derive de/enum_externally.rs, de/enum_.rs, de/identifier.rs, ser.rs (externally tagged enums); de::value::{U32Deserializer, StrDeserializer, MapAccessDeserializer}::{variant_seed, deserialize_enum}	∀ externally tagged derived enum E with variants V0..V(n-1) named key(Vi). Ser: a unit variant gives UnitVariant{E, name}. Newtype(x) gives NewtypeVariant ++ Ser(x). A tuple variant gives TupleVariant{len} ++ fields ++ TupleVariantEnd. A struct variant gives StructVariant{len = written fields} ++ the keyed fields ++ StructVariantEnd. A variant whose only field is #[serde(skip)] serializes as a unit variant. De: the variant identifier may be a string carrier, Bytes, or an integer i, through Enum{E} or any variant token. An integer i ≥ n fails with invalid_value(Unsigned(i), "variant index 0 <= i < n"). An unknown name fails with unknown_variant(s, [key(V0), …]). The payload is read per variant kind, and struct variants follow derive-struct-de (alias, deny_unknown_fields and duplicate_field included). The value deserializers obey the same rules: u32.into_deserializer() selects variant index v, and a MapAccessDeserializer over a one-entry map {k: payload} reads variant k with payload.	de::test_enum_unit, test_enum_simple, test_enum_simple_with_skipped, test_enum_seq, test_enum_map, test_enum_unit_usize, test_enum_unit_bytes; de_error::test_unknown_variant, test_duplicate_field_enum, test_enum_out_of_range; ser::test_enum; annotations::test_unknown_field_rename_enum; value::test_u32_to_enum, test_map_access_to_enum	HARD per type: proc-macro output, EnumAccess/VariantAccess (generic, by value and &mut), matching on string/bytes/integer identifiers, nested derived payloads.	1,2,4,9,11,12,13
derive-enum-internal	refinement	aterm-serde-derive de/enum_internally.rs and ser.rs (tag = …); private::de::content::{TaggedContentVisitor, TagOrContentVisitor, ContentDeserializer, InternallyTaggedUnitVisitor, VariantDeserializer}; private::ser::{serialize_tagged_newtype, TaggedSerializer}	∀ enum E with #[serde(tag = t)]. Ser: a unit variant V gives Struct{E,1} ++ [Str(t), Str(key(V))] ++ StructEnd. A struct variant gives Struct{E, 1+|W|} with the tag entry first, then the fields. A newtype over a struct/map/unit/newtype/enum emits the inner value's map or struct form with (t, key(V)) inserted first; a unit payload gives Map{Some(1)} [t, V] MapEnd. A struct variant whose only field is skipped gives Struct{E,1}[t,V]. A flatten field inside a variant gives Map{None}, tag first. De: (a) map/struct input: the entries before and after the tag key are buffered as Content in input order, wherever the tag sits and whichever string carrier spells it. No tag fails with missing_field(t). A second tag fails with duplicate_field(t); it never silently overrides the first. An unknown tag value fails with unknown_variant(s, all names). The variant then reads from CDe(Map(rest)), and a unit variant ignores the remaining entries. (b) seq input: the first element is the tag and the rest is the payload, read positionally. A unit variant then requires no further elements (invalid_length(k, "0 elements in sequence")), and a newtype-of-map from seq fails with invalid_type(Seq, "a map"). Round trip: ∀ v, De_E(Ser(v)) = Ok(v).	enum_internally_tagged::unit, newtype_unit, newtype_unit_struct, newtype_newtype, newtype_map, newtype_struct, newtype_enum::{unit,newtype,tuple,struct_}, struct_, struct_enum::unit, wrong_tag, with_skipped_field, containing_flatten, unit_variant_with_unknown_fields; untrusted::a_second_tag_key_is_a_duplicate_field	HARD: the Content enum tree (Box/Vec recursion), while-let over MapAccess with an Option tag accumulator, ContentDeserializer consumption, TaggedSerializer wrapping a generic Serializer, proc-macro output.	1,2,4,5,7,9,11,12,13
content-buffer-transparent	equivalence	private::de::content::{ContentVisitor, ContentRefDeserializer, ContentDeserializer, SeqRefDeserializer, MapRefDeserializer, EnumRefDeserializer, VariantRefDeserializer, VariantDeserializer}	∀ T: Deserialize (std or derived), ts, v: if De_T(ts) = Ok(v), then T::deserialize(CRef(Buf(ts))) = Ok(v) and T::deserialize(CDe(Buf(ts))) = Ok(v). Buf preserves the event tree: integers keep their width, every string carrier becomes Str/String, Seq/Map keep their element order, NewtypeStruct keeps its wrapper, and an enum source becomes a one-entry map {variant: payload}. Replaying it drives the same visitor calls. Documented leniency: CRef/CDe deserialize_option reads None or Unit as None, Some(x) as Some, and any other content c as Some(c). A newtype struct accepts NewtypeStruct(c) or a bare c. A struct variant's payload accepts Map or Seq content, empty ones included. A failing CRef read leaves the buffer unchanged, so the next untagged candidate sees the same content.	enum_untagged::newtype_struct, newtype_enum::{unit,newtype,tuple0,tuple2,struct_from_map,struct_from_seq,empty_struct_from_map,empty_struct_from_seq}, with_optional_field::{some,some_without_marker,none,unit}	HARD: a recursive Content enum (Box, Vec, Vec of pairs), by-ref and by-value deserializers, slice iterators with &mut state, generic Visitor dispatch, lifetimes 'a/'de.	1,2,4,5,7,11,13
buffered-text-key	contract	de::value::{TextKeyDeserializer, KeyText} (de/text_key.rs, aterm's own); private::de::content::{key_deserializer, key_ref_deserializer, content_text} and the map-key sites of MapDeserializer, MapRefDeserializer, FlatMapAccess, FlatStructAccess	∀ D: Deserializer + KeyText, ∀ visitor. Let t = D.key_text(). An integer request (i8..i128, u8..u128) with t = Some(s) where s is Display's spelling of an integer n (an optional `-`, then `0` alone or digits with no leading zero, never `-0`) calls exactly one of visit_u64/visit_i64/visit_u128/visit_i128(n), the first of the four whose range holds n. A bool request with t = Some("true"|"false") calls visit_bool. An option request with t = Some(_) calls visit_some(self), and a newtype request visit_newtype_struct(self). Every other request, and every request with t = None or another spelling, is D's own. ∀ Content key k: key_text(CDe(k)) = key_text(CRef(k)) = Some(s) iff k ∈ {Str(s), String(s)}. Every map key a buffered map (visit_content_map, visit_content_map_ref, a flattened map or struct) hands a seed goes through TextKeyDeserializer. Hence ∀ K ∈ {integers, bool, newtype or Option over one}: a map keyed by K that a string-keyed format (JSON, TOML) wrote reads back through #[serde(flatten)], untagged and internally tagged enums as it does directly; a key Content holds as an integer reads as before.	text_keys::a_buffered_text_key_reads_back_as_what_it_spells, a_buffered_text_key_reads_back_only_from_its_one_spelling	MEDIUM: byte-slice pattern match and str::parse for four widths (an integer-parse model), Option chaining with or_else, a generic wrapper forwarding ~25 trait methods, Content matching by reference.	1,2,3,4,11,13
derive-enum-untagged	refinement	aterm-serde-derive de/enum_untagged.rs and ser.rs (untagged); private::de::content::{ContentVisitor, ContentRefDeserializer, UntaggedUnitVisitor}	∀ #[serde(untagged)] enum E with variants V0..V(n-1). Ser(Vi(payload)) = the payload's serialization alone, with no variant marker: a unit variant gives Unit, a newtype gives Ser(x), a tuple gives Tuple{len}, and a struct gives Struct{E,len} with the keyed fields. De_E(ts) = Vi(x) for the first i, in declaration order, with Vi::deserialize(CRef(Buf(ts))) = Ok(x). A unit variant accepts Unit or None. If no i succeeds, wrong arity included, it fails with custom("data did not match any variant of untagged enum E"). Round trip: De_E(Ser(v)) = Ok(v) whenever no earlier variant also accepts Ser(v). That side condition is part of the spec, not a defect: an empty map deserializes as an earlier map-shaped variant.	enum_untagged::complex, newtype_unit_and_empty_map, contains_flatten, contains_flatten_with_integer_key; annotations::test_untagged_enum_holding_a_tagged_enum	HARD: proc-macro output, the Content buffer, an ordered fallback over variants (a sequence of Result matches), a generic Deserializer.	1,4,5,7,9,11,12,13
flat-map-take-entry	contract	private::de::flat_map_take_entry; private::de::content::content_as_str	∀ entry: &mut Option<(Content, Content)>, recognized: &[&str]: let hit = (entry = Some((k,_)) ∧ content_as_str(k) = Some(s) ∧ s ∈ recognized). If hit, the result is the old *entry and *entry' = None. Otherwise the result is None and *entry' = *entry. content_as_str(c) = Some(s) iff c is Str(s), String(s), or Bytes/ByteBuf holding UTF-8 s.	(no primary test of its own. It is the lemma that flatten::map_twice and flatten::alias sample through flatten-de-partition)	EASY-MEDIUM: &mut Option of a tuple of a non-scalar enum, Option::take, slice::contains over &str (a loop), str::from_utf8. No generics, no proc macro.	1,2,3,5,7
flatten-de-partition	refinement	private::de::{FlatMapDeserializer, FlatMapAccess, FlatStructAccess, flat_map_take_entry}; aterm-serde-derive de/struct_.rs flatten path	∀ derived struct S with direct fields D and flatten fields F1..Fk in declaration order, ∀ Map/Struct input. The keys of D are read as in derive-struct-de, and every other entry is appended to FlatBuf in input order. Then each Fi is deserialized from FlatMapDeserializer(&mut FlatBuf), in order. A struct, or a struct-like enum (internally tagged or untagged), claims and removes exactly the live entries whose keys it recognises (flat_map_take_entry, aliases included). A map, an IgnoredAny or a user deserialize_any reads every live entry and removes none. An externally tagged enum takes the first live entry whose key is one of its variant names, and if there is none it fails with custom("no variant of enum E found in flattened data"). () and unit structs read nothing. Option<T> is Some iff T succeeds on the remaining buffer. Any other type (scalars, String, seq, tuple) fails with custom("can only flatten structs and maps"). With deny_unknown_fields on S, an entry still live after every Fi has run fails with unknown_field(key), with no expected list. So the result does not depend on how entries interleave between groups, for example extra keys before the variant key.	annotations::test_collect_other, flatten::{complex, map_twice, unsupported_type, unknown_field, non_string_keys, flatten_any_after_flatten_struct, enum_tuple_and_struct, option, ignored_any, alias, unit::unit, unit::unit_struct, enum_::externally_tagged::{straightforward,unit,newtype}, enum_::internally_tagged::{structs,unit_enum_with_unknown_fields}}	VERY HARD: a &mut Vec<Option<(Content,Content)>> shared across sequential deserializers, slice Iter/IterMut with pending state (&mut self), proc-macro output, generic visitors, __private_visit_untagged_option.	1,2,4,5,7,9,11,12,13
flatten-ser-inline	refinement	private::ser::{FlatMapSerializer, FlatMapSerializeMap, FlatMapSerializeStruct, FlatMapSerializeStructVariantAsMapValue, Unsupported::fmt}; aterm-serde-derive ser.rs flatten path	∀ derived struct S with a flatten field: Ser(s) = Map{len:None} ++ the following for each field in declaration order ++ MapEnd. A direct field gives Str(key) ++ Ser(f). A flattened map or struct gives its entries inline, with keys of any type (integers included). A flattened externally tagged enum gives Str(variant) ++ payload (Unit for a unit variant, the inner value for a newtype, a nested Map/Seq for tuple/struct variants). A flattened internally tagged enum gives (tag, name) and then its fields. A flattened () or unit struct gives nothing. A flattened Option gives nothing for None and the inline entries of x for Some(x). A flattened value of any other kind fails with custom("can only flatten structs and maps (got {Unsupported(kind)})") after the preceding entries have been emitted. A direct field and a flattened struct's field reach the map through SerializeMap::serialize_field_entry (aterm's own; its default is serialize_entry, so the token trace above is unchanged), and so does such a field buffered as Content inside a tuple or struct variant (Content::Field replays it); a flattened map's entries and a flattened enum's payload reach it through serialize_entry. Unsupported renders as "a boolean", "an integer", "a float", "a char", "a string", "a byte array", "an optional", "a sequence", "a tuple" or "a tuple struct". Round trip with flatten-de-partition: De_S(Ser(s)) = Ok(s).	annotations::flatten::enum_::untagged::struct_. Every flatten test in flatten-de-partition is a secondary sample, as are enum_internally_tagged::containing_flatten and enum_untagged::contains_flatten{,_with_integer_key}.	HARD: a Serializer adaptor wrapping &mut M: SerializeMap, the Unsupported enum's Display, private ser Content buffering for tuple/struct variants, proc-macro output.	1,2,4,9,11,12,13
derive-input-parse	refinement	aterm-macro-support {item.rs, ty.rs, lit.rs, cursor.rs, meta.rs} as aterm-serde-derive's input parser	∀ item I that the derive accepts, let Plain(I) be the same data with plain syntax: no generic bounds, defaults or where clauses; no raw `r#` prefixes; string literals unescaped; no doc attributes or visibilities; None-delimited macro groups flattened; discriminants dropped. Then the parsed item has the same container name, the same fields or variants in order, the same field types, and the same serde attributes as Plain(I), so Derive(I) is Derive(Plain(I)) with I's generics carried through as bounds. In particular: a raw identifier's key is the ident without `r#`, and a rename string literal is its unescaped value (raw strings, \u{..}, \t and \x.. escapes included). `pub (A, B)` in a tuple field is a visibility followed by a tuple type. A `$ty` None-group starts a type, not a visibility. `->`, nested `<…>`, associated types and `Self` stay inside the field type. A discriminant expression ends only at a comma outside generic arguments: `<` and `<<` compare or shift unless they open a turbofish, a qualified path or a cast's type. Deprecated variants derive without warnings. A #[repr(packed)] struct reads each field by copy.	derive_input::generics_with_bounds_defaults_and_a_where_clause, where_clauses_after_a_tuple_and_before_an_enum_body, raw_identifiers_and_strings_name_the_data, doc_attributes_and_visibilities, macro_fragments_read_through_their_none_groups, nested_generics_and_an_associated_type_are_bounded, arrows_in_types_and_higher_ranked_bounds, self_in_a_field_type_is_the_type_itself, discriminants_and_deprecated_variants, shifts_and_comparisons_in_a_discriminant_end_at_its_comma, a_packed_struct_is_read_by_copy, lifetime_parameters_with_bounds	HARD: a token-cursor parser over proc_macro::TokenStream (which exists only inside the compiler), recursive descent with depth counting for `<`/`>`, string-literal unescaping. The "compiles at all" half is a compile-time fact that serde_suite already asserts by building. Recovered at merge time from tests/derive_input.rs.	1,2,3,7,9,12
```

### Distribution by kind (22)

| kind | count | ids |
|---|---|---|
| refinement | 10 | de-std-shape, ser-std-shape, derive-struct-ser, derive-struct-de, derive-enum-external, derive-enum-internal, derive-enum-untagged, flatten-de-partition, flatten-ser-inline, derive-input-parse |
| contract | 8 | size-hint-cautious, de-float-convert, de-error-messages, ignored-any-skips-one-value, ser-128-default-unsupported, derive-rename-rules, flat-map-take-entry, buffered-text-key |
| table-property | 2 | de-int-convert, de-std-invalid-type |
| equivalence | 2 | de-in-place-agrees, content-buffer-transparent |

### Proof dependency order

`de-error-messages` (every `de` error string is built here) → `de-int-convert`, `de-float-convert`,
`de-std-invalid-type` → `ignored-any-skips-one-value` → `de-std-shape` → `de-in-place-agrees`.
In parallel, `ser-128-default-unsupported` → `ser-std-shape`.
`derive-rename-rules` and `derive-input-parse` (key(f) and the parsed item) → `derive-struct-ser`,
`derive-struct-de` → `derive-enum-external` → `content-buffer-transparent` → `derive-enum-internal`,
`derive-enum-untagged` → `flat-map-take-entry` → `flatten-de-partition` + `flatten-ser-inline`
(their round trip is the last proof). `size-hint-cautious` has no dependency: its core stands
alone, and its composed half sits under `de-std-shape` and `content-buffer-transparent`.

## 2. Test → spec map (TSV)

`primary` is the spec that retires the test. `also` lists the other specs the same test samples.
A test is deleted only when its primary spec and every spec under `also` are Certified.
Two secondary samplings are too broad to list per row. Every `assert_de_tokens` call samples
`de-in-place-agrees`. Every `de.rs` test that goes through its local `test` helper samples
`ignored-any-skips-one-value` (via `assert_de_tokens_ignore`). Every `assert_de_tokens_error`
message also samples `de-error-messages`.

```tsv
file	test	primary	also
tests/annotations.rs	test_unknown_field_rename_struct	derive-struct-de	
tests/annotations.rs	test_unknown_field_rename_enum	derive-enum-external	
tests/annotations.rs	test_skip_struct	derive-struct-de	derive-struct-ser
tests/annotations.rs	test_collect_other	flatten-de-partition	flatten-ser-inline
tests/annotations.rs	test_untagged_enum_holding_a_tagged_enum	derive-enum-untagged	
tests/annotations.rs	flatten::complex	flatten-de-partition	flatten-ser-inline
tests/annotations.rs	flatten::map_twice	flatten-de-partition	flat-map-take-entry
tests/annotations.rs	flatten::unsupported_type	flatten-de-partition	flatten-ser-inline
tests/annotations.rs	flatten::unknown_field	flatten-de-partition	
tests/annotations.rs	flatten::non_string_keys	flatten-de-partition	flatten-ser-inline
tests/annotations.rs	flatten::flatten_any_after_flatten_struct	flatten-de-partition	
tests/annotations.rs	flatten::enum_tuple_and_struct	flatten-de-partition	flatten-ser-inline
tests/annotations.rs	flatten::option	flatten-de-partition	flatten-ser-inline
tests/annotations.rs	flatten::ignored_any	flatten-de-partition	
tests/annotations.rs	flatten::alias	flatten-de-partition	flat-map-take-entry
tests/annotations.rs	flatten::unit::unit	flatten-de-partition	flatten-ser-inline
tests/annotations.rs	flatten::unit::unit_struct	flatten-de-partition	flatten-ser-inline
tests/annotations.rs	flatten::enum_::externally_tagged::straightforward	flatten-de-partition	flatten-ser-inline
tests/annotations.rs	flatten::enum_::externally_tagged::unit	flatten-de-partition	flatten-ser-inline
tests/annotations.rs	flatten::enum_::externally_tagged::newtype	flatten-de-partition	flatten-ser-inline
tests/annotations.rs	flatten::enum_::internally_tagged::structs	flatten-de-partition	flatten-ser-inline
tests/annotations.rs	flatten::enum_::internally_tagged::unit_enum_with_unknown_fields	flatten-de-partition	flatten-ser-inline
tests/annotations.rs	flatten::enum_::untagged::struct_	flatten-ser-inline	derive-enum-untagged
tests/attrs.rs	field_default_bare_and_path_fill_missing_keys	derive-struct-de	
tests/attrs.rs	skip_serializing_if_drops_the_field_and_its_length_slot	derive-struct-ser	
tests/attrs.rs	container_default_fills_from_the_type_and_unknown_keys_still_fail	derive-struct-de	
tests/attrs.rs	a_missing_field_without_default_names_the_field	derive-struct-de	
tests/attrs.rs	a_struct_reads_positionally_from_a_sequence	derive-struct-de	
tests/attrs.rs	rename_all_cases_aterm_uses	derive-rename-rules	
tests/attrs.rs	transparent_is_the_inner_value	derive-struct-ser	
tests/attrs.rs	deserialize_with_runs_the_named_function	derive-struct-de	
tests/de.rs	test_bool	de-std-shape	
tests/de.rs	test_i8	de-int-convert	
tests/de.rs	test_i16	de-int-convert	
tests/de.rs	test_i32	de-int-convert	
tests/de.rs	test_i64	de-int-convert	
tests/de.rs	test_i128	de-int-convert	
tests/de.rs	test_isize	de-int-convert	
tests/de.rs	test_u8	de-int-convert	
tests/de.rs	test_u16	de-int-convert	
tests/de.rs	test_u32	de-int-convert	
tests/de.rs	test_u64	de-int-convert	
tests/de.rs	test_u128	de-int-convert	
tests/de.rs	test_usize	de-int-convert	
tests/de.rs	test_f32	de-float-convert	
tests/de.rs	test_f64	de-float-convert	
tests/de.rs	test_nan	de-float-convert	(sign preservation via copysign, §5.5)
tests/de.rs	test_char	de-std-shape	
tests/de.rs	test_string	de-std-shape	
tests/de.rs	test_option	de-std-shape	
tests/de.rs	test_unit	de-std-shape	
tests/de.rs	test_unit_struct	derive-struct-de	
tests/de.rs	test_generic_unit_struct	derive-struct-de	(runtime duplicate of test_unit_struct, §4)
tests/de.rs	test_newtype_struct	derive-struct-de	
tests/de.rs	test_tuple_struct	derive-struct-de	
tests/de.rs	test_vec	de-std-shape	
tests/de.rs	test_array	de-std-shape	
tests/de.rs	test_tuple	de-std-shape	
tests/de.rs	test_btreemap	de-std-shape	
tests/de.rs	test_struct	derive-struct-de	
tests/de.rs	test_struct_borrowed_keys	derive-struct-de	
tests/de.rs	test_struct_owned_keys	derive-struct-de	
tests/de.rs	test_struct_with_skip	derive-struct-de	
tests/de.rs	test_struct_skip_all	derive-struct-de	
tests/de.rs	test_struct_skip_default	derive-struct-de	
tests/de.rs	test_struct_skip_all_deny_unknown	derive-struct-de	
tests/de.rs	test_struct_default	derive-struct-de	
tests/de.rs	test_enum_unit	derive-enum-external	
tests/de.rs	test_enum_simple	derive-enum-external	
tests/de.rs	test_enum_simple_with_skipped	derive-enum-external	
tests/de.rs	test_enum_seq	derive-enum-external	
tests/de.rs	test_enum_map	derive-enum-external	
tests/de.rs	test_enum_unit_usize	derive-enum-external	
tests/de.rs	test_enum_unit_bytes	derive-enum-external	
tests/de.rs	test_box	de-std-shape	
tests/de.rs	test_boxed_array	de-std-shape	
tests/de.rs	test_path_buf	de-std-shape	
tests/de.rs	test_ignored_any	ignored-any-skips-one-value	
tests/de_error.rs	test_i8	de-int-convert	
tests/de_error.rs	test_i16	de-int-convert	
tests/de_error.rs	test_i32	de-int-convert	
tests/de_error.rs	test_i64	de-int-convert	
tests/de_error.rs	test_u8	de-int-convert	
tests/de_error.rs	test_u16	de-int-convert	
tests/de_error.rs	test_u32	de-int-convert	
tests/de_error.rs	test_u64	de-int-convert	
tests/de_error.rs	test_u128	de-int-convert	
tests/de_error.rs	test_usize	de-int-convert	
tests/de_error.rs	test_unknown_field	derive-struct-de	
tests/de_error.rs	test_skipped_field_is_unknown	derive-struct-de	(§5.6)
tests/de_error.rs	test_skip_all_deny_unknown	derive-struct-de	
tests/de_error.rs	test_unknown_variant	derive-enum-external	
tests/de_error.rs	test_duplicate_field_struct	derive-struct-de	
tests/de_error.rs	test_duplicate_field_enum	derive-enum-external	
tests/de_error.rs	test_enum_out_of_range	derive-enum-external	
tests/de_error.rs	test_short_tuple	de-std-shape	
tests/de_error.rs	test_short_array	de-std-shape	
tests/de_error.rs	test_unit_from_empty_seq	de-std-invalid-type	
tests/de_error.rs	test_unit_from_empty_seq_without_len	de-std-invalid-type	
tests/de_error.rs	test_unit_from_tuple_struct	de-std-invalid-type	
tests/de_error.rs	test_string_from_unit	de-std-invalid-type	
tests/de_error.rs	test_vec_from_unit	de-std-invalid-type	
tests/de_error.rs	test_vec_from_unit_struct	de-std-invalid-type	
tests/de_error.rs	test_zero_array_from_unit	de-std-invalid-type	
tests/de_error.rs	test_zero_array_from_unit_struct	de-std-invalid-type	
tests/de_error.rs	test_btreemap_from_unit	de-std-invalid-type	
tests/de_error.rs	test_btreemap_from_unit_struct	de-std-invalid-type	
tests/de_error.rs	test_hashmap_from_unit	de-std-invalid-type	
tests/de_error.rs	test_hashmap_from_unit_struct	de-std-invalid-type	
tests/de_error.rs	test_bool_from_string	de-error-messages	
tests/de_error.rs	test_number_from_string	de-error-messages	
tests/de_error.rs	test_integer_from_float	de-error-messages	
tests/de_error.rs	test_nan_no_decimal_point	de-error-messages	
tests/de_error.rs	test_unit_struct_from_seq	de-std-invalid-type	
tests/derive_input.rs	generics_with_bounds_defaults_and_a_where_clause	derive-input-parse	
tests/derive_input.rs	where_clauses_after_a_tuple_and_before_an_enum_body	derive-input-parse	
tests/derive_input.rs	raw_identifiers_and_strings_name_the_data	derive-input-parse	derive-rename-rules
tests/derive_input.rs	doc_attributes_and_visibilities	derive-input-parse	
tests/derive_input.rs	macro_fragments_read_through_their_none_groups	derive-input-parse	
tests/derive_input.rs	nested_generics_and_an_associated_type_are_bounded	derive-input-parse	
tests/derive_input.rs	arrows_in_types_and_higher_ranked_bounds	derive-input-parse	
tests/derive_input.rs	self_in_a_field_type_is_the_type_itself	derive-input-parse	
tests/derive_input.rs	discriminants_and_deprecated_variants	derive-input-parse	de-error-messages, derive-enum-external
tests/derive_input.rs	shifts_and_comparisons_in_a_discriminant_end_at_its_comma	derive-input-parse	derive-enum-external
tests/derive_input.rs	a_packed_struct_is_read_by_copy	derive-input-parse	
tests/derive_input.rs	lifetime_parameters_with_bounds	derive-input-parse	
tests/derive_input.rs	self_inside_a_type_macro_remains_the_macros_input	derive-input-parse	
tests/enum_internally_tagged.rs	unit	derive-enum-internal	
tests/enum_internally_tagged.rs	newtype_unit	derive-enum-internal	
tests/enum_internally_tagged.rs	newtype_unit_struct	derive-enum-internal	
tests/enum_internally_tagged.rs	newtype_newtype	derive-enum-internal	
tests/enum_internally_tagged.rs	newtype_map	derive-enum-internal	
tests/enum_internally_tagged.rs	newtype_struct	derive-enum-internal	
tests/enum_internally_tagged.rs	newtype_enum::unit	derive-enum-internal	
tests/enum_internally_tagged.rs	newtype_enum::newtype	derive-enum-internal	
tests/enum_internally_tagged.rs	newtype_enum::tuple	derive-enum-internal	
tests/enum_internally_tagged.rs	newtype_enum::struct_	derive-enum-internal	
tests/enum_internally_tagged.rs	struct_	derive-enum-internal	
tests/enum_internally_tagged.rs	struct_enum::unit	derive-enum-internal	
tests/enum_internally_tagged.rs	wrong_tag	derive-enum-internal	
tests/enum_internally_tagged.rs	with_skipped_field	derive-enum-internal	
tests/enum_internally_tagged.rs	containing_flatten	derive-enum-internal	flatten-ser-inline
tests/enum_internally_tagged.rs	unit_variant_with_unknown_fields	derive-enum-internal	
tests/enum_untagged.rs	complex	derive-enum-untagged	
tests/enum_untagged.rs	newtype_unit_and_empty_map	derive-enum-untagged	(first-match side condition, §5.4)
tests/enum_untagged.rs	newtype_struct	content-buffer-transparent	
tests/enum_untagged.rs	newtype_enum::unit	content-buffer-transparent	
tests/enum_untagged.rs	newtype_enum::newtype	content-buffer-transparent	
tests/enum_untagged.rs	newtype_enum::tuple0	content-buffer-transparent	
tests/enum_untagged.rs	newtype_enum::tuple2	content-buffer-transparent	
tests/enum_untagged.rs	newtype_enum::struct_from_map	content-buffer-transparent	
tests/enum_untagged.rs	newtype_enum::struct_from_seq	content-buffer-transparent	
tests/enum_untagged.rs	newtype_enum::empty_struct_from_map	content-buffer-transparent	
tests/enum_untagged.rs	newtype_enum::empty_struct_from_seq	content-buffer-transparent	
tests/enum_untagged.rs	with_optional_field::some	content-buffer-transparent	
tests/enum_untagged.rs	with_optional_field::some_without_marker	content-buffer-transparent	
tests/enum_untagged.rs	with_optional_field::none	content-buffer-transparent	
tests/enum_untagged.rs	with_optional_field::unit	content-buffer-transparent	
tests/enum_untagged.rs	contains_flatten	derive-enum-untagged	flatten-ser-inline
tests/enum_untagged.rs	contains_flatten_with_integer_key	derive-enum-untagged	flatten-ser-inline
tests/ignored_any.rs	test_deserialize_enum	ignored-any-skips-one-value	derive-enum-external (first half tests test-only code, §4)
tests/ser.rs	test_unit	ser-std-shape	
tests/ser.rs	test_bool	ser-std-shape	
tests/ser.rs	test_isizes	ser-std-shape	
tests/ser.rs	test_usizes	ser-std-shape	
tests/ser.rs	test_floats	ser-std-shape	
tests/ser.rs	test_char	ser-std-shape	
tests/ser.rs	test_str	ser-std-shape	
tests/ser.rs	test_option	ser-std-shape	
tests/ser.rs	test_slice	ser-std-shape	
tests/ser.rs	test_array	ser-std-shape	
tests/ser.rs	test_vec	ser-std-shape	
tests/ser.rs	test_tuple	ser-std-shape	
tests/ser.rs	test_btreemap	ser-std-shape	
tests/ser.rs	test_unit_struct	derive-struct-ser	
tests/ser.rs	test_tuple_struct	derive-struct-ser	
tests/ser.rs	test_struct	derive-struct-ser	
tests/ser.rs	test_enum	derive-enum-external	
tests/ser.rs	test_box	ser-std-shape	
tests/ser.rs	test_boxed_array	ser-std-shape	
tests/ser.rs	test_path_buf	ser-std-shape	
tests/ser.rs	test_cannot_serialize_paths	ser-std-shape	
tests/ser.rs	test_integer128	ser-128-default-unsupported	
tests/text_keys.rs	a_buffered_text_key_reads_back_as_what_it_spells	buffered-text-key	flatten-de-partition, derive-enum-untagged, derive-enum-internal
tests/text_keys.rs	a_buffered_text_key_reads_back_only_from_its_one_spelling	buffered-text-key	de-int-convert, de-error-messages
tests/untrusted.rs	a_second_tag_key_is_a_duplicate_field	derive-enum-internal	
tests/untrusted.rs	a_huge_seq_hint_preallocates_at_most_the_cap	size-hint-cautious	
tests/untrusted.rs	a_huge_map_hint_does_not_size_the_table	size-hint-cautious	
tests/untrusted.rs	a_huge_hint_into_the_untagged_buffer_preallocates_at_most_the_cap	size-hint-cautious	
tests/value.rs	test_u32_to_enum	derive-enum-external	
tests/value.rs	test_map_access_to_enum	derive-enum-external	
```

## 3. Coverage

- All 191 tests mapped to one of the 22 specs, and none is a non-spec sample.
- The biggest collapses: `derive-struct-de` (23 primary), `de-int-convert` (22),
  `flatten-de-partition` (18), `ser-std-shape` (17), `derive-enum-internal` (17),
  `de-std-shape` (14), `derive-enum-external` (14), `de-std-invalid-type` (13),
  `content-buffer-transparent` (13), `derive-input-parse` (13).
- `de-in-place-agrees` and `flat-map-take-entry` have no primary test. They are lemmas that every
  `de` test (respectively the two flatten alias/map_twice tests) samples. `flatten-ser-inline` has a
  single primary, but 16 other tests sample it.
- 9 of the 22 specs are about code that `aterm-serde-derive` generates, which puts them behind
  gap 9 (the proc-macro boundary) and gap 12 (citing a theorem across crates). Their primary tests number 97 of
  the 191. Gap 9 is the single change that unblocks the most tests in this crate.

## 4. Tests that are not spec samples

None. Every test checks production code (`src/` or the derive) at sample points of a spec in §1.

| test | why it still needs a change | action |
|---|---|---|
| `tests/support/*` (harness, not tests) | Test-only code: a recording `Serializer` and a replaying `Deserializer`. | **delete with the last test that uses it**. No spec is stated about it. |
| `ignored_any::test_deserialize_enum` | Exercises `Target` dispatch and `IgnoredAny::visit_enum` over a custom `EnumAccess`. | **delete when `derive-enum-external` and `ignored-any-skips-one-value` are Certified**. |
| `de::test_boxed_array`, `ser::test_boxed_array` | Samples `Box<[i32; 3]>` (`Box<T>` + `[T; N]`). | **delete when `de-std-shape`/`ser-std-shape` are Certified**. |
| `de::test_generic_unit_struct` | At runtime it duplicates `test_unit_struct`. The const-generic compile check is already asserted by building `serde_suite`. | **delete with `derive-struct-de`**. |

Non-spec count by action: 0 to delete now, 0 to become a spec. The rows above are trims to spec
samples, not non-spec tests.

## 5. Implied specs that look wrong, or disagree with the code

1. **`test_boxed_slice` renamed to `test_boxed_array` (resolved 2026-10-27).** `Box::new([0, 1, 2])`
   is `Box<[i32; 3]>`, and both `de.rs` and `ser.rs` now name it `test_boxed_array`.
2. **`de::test_tuple_struct` duplicate replaced with `Tuple` carrier (resolved 2026-10-27).** Its
   repeated `TupleStruct` call now exercises `Token::Tuple { len: 3 } … Token::TupleEnd` as stated
   by `derive-struct-de`.
3. **`untrusted.rs` capacity assertions added (resolved 2026-10-27).**
   `a_huge_map_hint_does_not_size_the_table` now bounds `m.capacity()`, and
   `a_huge_hint_into_the_untagged_buffer_preallocates_at_most_the_cap` bounds the decoded `Vec`'s
   capacity; `size-hint-cautious` also covers the intermediate `Content` Vec's `with_capacity` call.
4. **Untagged round trip is conditional.** `enum_untagged::newtype_unit_and_empty_map` depends on
   `Message::Unit(Unit)` rejecting an empty map so that the `Map` variant wins, and never asserts
   the reverse. `derive-enum-untagged` states the first-match side condition explicitly. An
   unconditional round trip for untagged enums is false.
5. **`de-float-convert` relies on `copysign`, not on `as`.** NaN sign preservation (`test_nan`)
   holds only because of `num_as_copysign_self!`. A plain `as` cast between float widths does not
   promise to keep a NaN's sign. The spec states the `copysign` form, so a refactor to `as` alone
   would break it.
6. **A skipped field's name is unknown, even under `deny_unknown_fields`.**
   `de_error::test_skipped_field_is_unknown` pins that `#[serde(skip)]` removes the name from the
   expected list as well as from the input. That matches upstream serde. The spec keeps it, so a
   reader should not read "skip" as "ignore if present".

## 6. Proof order (easiest high-value first)

No spec is blocked by gap 0 alone (§0). Each proof needs the local seal `150efe4a` plus the gaps
in its row. The order below takes the smallest gap sets first and weighs them by the tests they
retire.

1. **`size-hint-cautious`**, scalar core first. Instantiate per `Element` (sidesteps gap 11).
   The body is `size_of`, `unwrap_or`, `min` and a division by a nonzero constant, so it is
   omega-class once gap 3 admits calls to std `min` and the division. This is the crate's
   untrusted-input guarantee: no length hint can make it allocate more than 1 MiB up front.
   The 3 `untrusted.rs` tests retire once the composed half (gaps 1 and 4) lands.
2. **`ser-128-default-unsupported`.** One default trait method that returns `Error::custom` of a
   fixed string. It needs only a concrete serializer instantiation (gap 11) and a `&str` carrier
   (gap 1). It retires 1 test and is the smallest end-to-end exercise of the trait default path.
3. **`de-int-convert`**, scalar core. The heart of it is "in range ⇔ the conversion preserves the
   value", over 12 targets × 8 source widths. That is a table property over casts and MIN/MAX
   comparisons (gap 3), instantiated per `E` (gap 11). It retires **22 tests**, the most of any spec
   outside the derive. The error-message half follows `de-error-messages`.
4. **`derive-rename-rules`.** Move `case.rs` out of the proc-macro crate into a plain library, or
   prove it in place once gap 9 is closed. Then it is non-generic string code (gaps 1, 3 and 7), with
   no trait dispatch. It fixes `key(f)` for every derive spec, so it is high leverage despite
   retiring 1 test directly.
5. **`flat-map-take-entry`.** Non-generic, no proc macro: one `Option::take` guarded by a
   `contains` over `&[&str]` (gaps 1, 2, 3, 5 and 7). It is the lemma `flatten-de-partition` rests on.

Next: `de-error-messages` (it unlocks every error-text clause), then `de-std-invalid-type` and
`de-float-convert` (once floats have a model). The derive specs and `content-buffer-transparent`
follow, and the flatten pair comes last. They wait on gap 9 (proc-macro boundary), gap 12
(cross-crate citation) and gap 13 (closures in `map_err`/visitors), on top of gaps 1, 2, 4 and 11.

## 7. Gap histogram

Number of specs (of 21) whose row names each audit §7 gap. Gap 0 applies to all 21 and is
excluded (§0).

| gap | specs blocked |
|---|---|
| 1 | 20 |
| 2 | 14 |
| 3 | 7 |
| 4 | 12 |
| 5 | 7 |
| 7 | 13 |
| 9 | 9 |
| 11 | 18 |
| 12 | 9 |
| 13 | 14 |
