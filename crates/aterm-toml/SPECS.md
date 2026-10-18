<!-- Copyright 2026 Andrew Yates -->
<!-- SPDX-License-Identifier: Apache-2.0 -->

# aterm-toml: spec recovery from its 47 unit tests

> Status 2026-10-16, after the merge with the tests-and-gates audit: the rows naming 2 tests it
> deleted are gone, and the spec rows no longer cite those tests. Counts elsewhere in this file
> were taken before.

> Status 2026-10-15: this is the crate's migration worklist. The 27 specs below are each
> proved through Trust/Clean about the compiled Rust function, and then the tests each one
> subsumes are deleted. The one test that samples no spec (§4) moves to a bench. No spec
> is proved yet, and no test has been deleted.

Source: `crates/aterm-toml` (read only). I read every `#[test]` in `tests/`
(cost.rs 1, depth.rs 4, edit.rs 13, fuzz.rs 5, roundtrip.rs 2, spec.rs 9, typed.rs 13 = 47)
and the code each one reaches. `src/` has no `#[test]` and no `#[cfg(test)]`. The chunk
manifest said 48: the 48th `#[test]` grep hit is the doc comment "keeps its own `#[test]`s" in
`tests/toml_suite.rs`, which is not a test. Nothing was compiled or run. The claims below come
from reading the code.

Result: **27 specs**, covering **46 tests**. **1 test is not a spec sample** (§4).

## 0. Notation used in the statements

- `P(s)` = `edit::parse::parse_document(s, ParseLimits::default())`, with `max_depth = 128`. It is
  the one parser. `DocumentMut::from_str` = `P`.
  `de::from_str::<T>(s)` = `P(s) >>= T::deserialize(Deserializer::new(Some(s), Node::from_item(root)))`.
  `aterm_toml::Value::from_str` = `Table::from_str` = `from_str`. `edit::Value::from_str` =
  `parse::parse_value`. `Key::parse` = `parse::parse_key_path`.
- `print(d)` = `<DocumentMut as Display>` (`encode::encode_document`).
- `L` = the TOML 1.0.0 documents (the ABNF plus the spec's key-uniqueness and table-redefinition
  rules), plus crate policy: TOML 1.1 additions are refused (`\e`, `\x`, newlines in inline tables,
  times without seconds), a finite float literal that overflows binary64 is refused in both signs,
  and DEL is refused in comments. `L_val` = the single-value production `val`, with no surrounding
  trivia.
- `depth(s)` = the depth of the parsed tree below the root. Every header segment, dotted-key
  segment, array and inline table adds one level, and levels from different constructs add up.
- `[[s]]` = the TOML 1.0 meaning of `s` as an `aterm_toml::Value` table. Escapes are decoded. The
  multi-line delimiter-run rule applies (1 or 2 delimiters before the close are content). The
  first newline after an opening multi-line delimiter is trimmed. A line-ending backslash folds.
  CRLF inside a multi-line string becomes LF. Integers keep radix and underscores out of the
  value. Floats round to nearest (underflow gives ±0.0). Dotted keys build nested tables.
  Implicit super-tables merge with a later explicit header. `[[h]]` gives an array of tables.
- `CharAligned(s,r)` := `r.start ≤ r.end ≤ s.len() ∧ s.is_char_boundary(r.start) ∧ s.is_char_boundary(r.end)`.
- `Entries` := {`P`, `from_str::<T>`, `aterm_toml::Value::from_str`, `Table::from_str`,
  `edit::Value::from_str`, `Key::parse`}.
- `M(v)` = the serde data model of a `Value`. A Table becomes a map whose unknown keys are passed
  through; an Integer becomes `i64`; a Float `f64`; a String `str`; an Array a seq with a length;
  a Datetime the private `DATETIME_STRUCT`/`DATETIME_FIELD` struct.
- Gap numbers are the rows of the table in
  the historical development audit `docs/AUDIT-trust-for-test-migration-2026-10-15.md`
  §7. **Gap 0 (a seal that publishes Clean rows) blocks every spec**, so the column lists only the
  gaps beyond it. No spec here is blocked by gap 0 alone: every one needs at least gap 1
  (non-scalar carriers), because even the scalar cores take `&str`, `Option` or `Result`.

## 1. Specs (TSV)

Columns: `id`, `kind`, `functions`, `statement`, `subsumes (primary tests; secondary in §2)`,
`difficulty (Rust features the proof must handle)`, `blocking gaps (audit §7, beyond gap 0)`.

```tsv
id	kind	functions	statement	subsumes	difficulty	blocking_gaps
roundtrip	round-trip	edit::parse::parse_document, edit::encode::encode_document, <DocumentMut as Display>::fmt, <DocumentMut as FromStr>::from_str	∀ s: &str, P(s) = Ok(d) ⇒ print(d) == s, byte for byte. This includes comments, blank lines, CRLF, a BOM, a missing final newline, every key, string and number spelling, interleaved dotted keys in authored order, and a super-table header that comes after its child.	roundtrip::every_toml_file_in_the_repository_round_trips_byte_for_byte, roundtrip::formatting_details_survive, fuzz::generated_documents_parse_and_round_trip, fuzz::single_character_mutations_round_trip_or_refuse_cleanly	HARD: loops over bytes, &str/String building, Vec and an ordered map tree, &mut self parser state, recursion, fmt::Formatter/Display	1,2,3,5,7,14
accept-domain	refinement	edit::parse::parse_document, Parser::{document,table_header,open_table,keyval,value,basic_string,literal_string,multiline_string,escape}, insert_path	∀ s: &str, P(s).is_ok() ⇔ (s ∈ L ∧ depth(s) ≤ 128). ⇐ covers every refusal: a duplicate key under any spelling, a header reopening a table or a dotted-key table, a table over an array of tables or the reverse, appending to a static array, extending an inline table, a bad escape or surrogate, the number-grammar violations, impossible dates and times, a time without seconds, bare strings, trailing tokens, malformed headers. ⇒ covers every spec-allowed construct, including dotted-key tables extended by sub-table headers, empty quoted keys and a BOM.	spec::everything_the_spec_allows_parses, spec::everything_the_spec_forbids_is_refused	VERY HARD: a grammar model (Lean inductive for L) bound to a &str parser, loops, recursion, Vec/map tree, &mut self	1,3,5,7,10,14
entry-points-agree	equivalence	de::from_str, <aterm_toml::Value as FromStr>::from_str, <aterm_toml::Table as FromStr>::from_str, <DocumentMut as FromStr>::from_str, <aterm_toml::Value as Deserialize>::deserialize, de::Deserializer::deserialize_any	∀ s: (a) from_str::<Value>(s).is_ok() ⇔ P(s).is_ok(), so deserializing any P-accepted root into Value is total. (b) Value::from_str(s) == from_str::<Value>(s) and Table::from_str(s) == from_str::<Table>(s). (c) P(s) = Err ⇒ every E ∈ Entries fed s (or, for the value and key entries, a token containing the same malformed fragment) returns Err.	(none primary; shared by the fuzz, depth, roundtrip, typed and spec tests in §2)	MEDIUM: serde Deserializer/Visitor traits, generic T, recursion over the Item tree; (b) is definitional	1,3,4,5,9,11
depth-bound	contract	Parser::{check_depth,value,array,inline_table,keyval,table_header}	∀ s: depth(s) > 128 ⇒ P(s) = Err(e) ∧ ∀ T, from_str::<T>(s) = Err(e), with e.message_text() containing "limit", whichever constructs make up the depth (arrays, inline tables, dotted keys, headers, or any mix such as dotted keys inside nested inline tables, or a header plus a dotted key plus arrays). depth(s) ≤ 128 ⇒ neither check_depth nor the depth guard in value(depth) returns Err.	depth::deeply_nested_input_returns_an_error_instead_of_overflowing_the_stack, depth::ordinary_nesting_is_untouched, depth::constructs_that_each_fit_the_limit_cannot_stack_past_it	MEDIUM: recursion with a depth parameter, &mut self, usize arithmetic (path.len() + segs.len()), Vec	1,2,3,5,7
no-panic-bounded-stack	no-panic/bounds	edit::parse::{parse_document,parse_value,parse_key_path}, de::from_str, <Item as Drop>, edit::encode::encode_document	∀ s: &str, ∀ E ∈ Entries: E(s) returns (Ok or Err) without a panic. Neither a successful parse nor the rejection path (including dropping a partially built tree) recurses deeper than c·128 frames, for a constant c independent of s. In particular nothing slices &str off a char boundary, so no unicode-escape window, error span or snippet panics.	depth::the_rejection_path_itself_is_safe, fuzz::punctuation_noise_never_panics	HARD: panic freedom over &str slicing; a recursion-depth (stack) bound including Drop glue, which no §7 row covers (see §5.9)	1,3,7,16
span-aligned	invariant	Parser::{err,here,next_boundary,unicode_escape}, Error::{at,with_span,span}	∀ s, ∀ E ∈ Entries: E(s) = Err(e) ⇒ (a) if e is a syntax error, e.span() = Some(r); (b) every Some(r) satisfies CharAligned(s, r). For a parse_value refusal this includes r.end ≤ s.len().	spec::parse_errors_carry_a_span_inside_the_source, spec::token_spans_underline_the_token	MEDIUM: Range<usize>, &str char boundaries, loops, &self parser views, Option, every error construction site; built on char-boundary	1,2,3,4,5,7
char-boundary	contract	Parser::next_boundary, error::floor_boundary	Lemma for span-aligned and error-display. (a) ∀ src, at ≤ src.len(): next_boundary(at) = the least j with at < j ≤ len ∧ src.is_char_boundary(j), or len if there is none; the result is ≥ at and a char boundary. (b) ∀ source, i: floor_boundary(source, i) = the greatest j ≤ min(i, len) with source.is_char_boundary(j); 0 is always a candidate.	(none; lemma)	EASY: while loops over usize, &str::is_char_boundary, min/max	1,3,7
unicode-escape	contract	Parser::{unicode_escape,escape}	∀ parser states with pos just past \u (digits = 4) or \U (digits = 8) whose backslash is at start: unicode_escape(out, digits, start) = Ok ⇔ pos + digits ≤ b.len() ∧ every byte of b[pos..pos+digits] is an ASCII hex digit ∧ code is a Unicode scalar value (≤ 0x10FFFF, ∉ 0xD800..=0xDFFF). On Ok it pushes char::from_u32(code) onto out and advances pos by digits. Otherwise it returns Err with a CharAligned span starting at start, leaves pos unchanged and does not panic. A sign, a space or a multi-byte character anywhere in the window gives Err.	fuzz::a_multi_byte_character_inside_a_unicode_escape_is_an_error_not_an_abort, fuzz::a_signed_unicode_escape_is_not_four_hex_digits	EASY-MEDIUM: &mut self, slice window, an all/fold loop, u32 arithmetic, char::from_u32, String push	1,2,3,7
eof-empty-span	contract	Parser::{here,next_boundary,array,value}, error::locate	∀ s where P(s) fails because the input ended inside an open construct (pos == s.len() with an array, value, string or header still open): the error's span is Some(s.len()..s.len()), and its Display begins "TOML parse error at line L, column C" with (L, C) = line_col(s, s.len()). L is the line of the last authored character: a final newline does not start a new line.	spec::end_of_input_errors_point_at_the_last_authored_line	MEDIUM: &mut self parser, Range, &str scanning loops; Display half from error-display	1,2,3,7
error-display	contract	<Error as Display>::fmt, error::{locate,line_col,snippet,decimal_width,floor_boundary}	∀ source, offset: locate(source, offset) = (line, col, ls), where o' = floor_boundary(source, min(offset, len)); scan = floor_boundary(source, o' − 1) if o' == len ∧ o' > 0, else o'; line = 1 + #'\n' in source[..scan]; ls = 1 + the position of the last '\n' in source[..scan], or 0; col = 1 + #chars in source[ls..o']. ∀ e = Error::at(source, r, msg) (or attached by with_span): e.to_string() == "TOML parse error at line {L}, column {C}\n{pad} |\n{L} | {text}\n{pad} | {sp}{carets}\n{msg}\n", where (L, C) = locate at r.start, pad = decimal_width(L) spaces, text = the line containing r.start with a trailing \r trimmed, sp = C−1 spaces, carets = max(1, #chars in source[start..min(r.end, line end)]) '^'. decimal_width(n) = #decimal digits of n, 1 for n = 0. An error with no line_col prints msg alone.	spec::a_schema_error_renders_the_offending_line	MEDIUM: &str slicing and counting loops, String, fmt::Formatter with width parameters (trait), ilog10	1,3,7,11
value-semantics	refinement	de::from_str, <aterm_toml::Value as Deserialize>::deserialize, Parser::{multiline_string,escape}, edit::parse::parse_number, insert_path	∀ s ∈ dom(P): from_str::<Value>(s) == Ok([[s]]). This includes the multi-line delimiter runs ("""a"""" means a" and """a""""" means a""), CRLF inside a multi-line string read as LF, an underflowing float reading as 0.0, a super-table declared after its child keeping both entries, a document parsed as a Table with one entry per distinct top-level key, and arrays of tables, dotted keys and inline tables nesting as [[·]] says.	spec::an_implicit_super_table_may_be_declared_later, spec::multiline_strings_handle_delimiter_runs	VERY HARD: a Lean model of TOML semantics bound to a recursive Value enum, String, Vec, map, f64 parsing, loops	1,3,5,7,10,14
number-literal	contract	edit::parse::{parse_number,strip_underscores,validate_float}, Parser::number_or_datetime	∀ raw: &str, parse_number(raw) = Some(n) ⇔ raw matches TOML 1.0's integer or float production and is in range: no leading zero; every underscore flanked by digits; an unsigned 0x/0o/0b prefix with ≥ 1 digit; an exponent with ≥ 1 digit; a float with integer-part digits and a nonempty fraction; an optional sign on inf and nan. An integer fits in i64. A finite float literal rounds to a finite f64 (1e400, -1e309, 92E3372036854775807 give None; 1e-400 gives 0.0). n is the literal's exact value, for a float its IEEE round-to-nearest.	spec::overflowing_float_literals_are_refused_in_both_signs	MEDIUM-HARD: &str find/slicing, String building, loops, i64 radix parse, f64 parse (needs a float-parse model), Option	1,3,7
days-in-month	table-property	datetime::{days_in_month,parse_datetime,parse_time,digits}	(a) ∀ year: u16, month: u8: days_in_month(year, month) = 31 for months {1,3,5,7,8,10,12}, 30 for {4,6,9,11}, 29 for month 2 in a Gregorian leap year ((y%4 = 0 ∧ y%100 ≠ 0) ∨ y%400 = 0), 28 for month 2 otherwise, and 0 for any other month. (b) parse_datetime(b) = Some((dt, n)) ⇒ b[..n] is a TOML 1.0 date-time in one of the four shapes, with month ∈ 1..=12, day ∈ 1..=days_in_month(year, month), hour ≤ 23, minute ≤ 59, second ≤ 60, seconds present, offset hour ≤ 23 and minute ≤ 59; dt's fields are the literal's digits, fractional seconds as nanoseconds (.999 gives 999_000_000).	(none primary; samples in spec::everything_the_spec_forbids_is_refused and typed::datetimes_round_trip_through_serde)	(a) TRIVIAL: a match on integer literals and is_multiple_of; (b) MEDIUM: byte slices, loops, Option, structs	1,3,7
key-parse-spelling	round-trip	edit::Key::{parse,new}, edit::parse::parse_key_path, Parser::{key_path,key_segment}, <Key as Display>::fmt, edit::encode::{encode_key,encode_basic_string}	(a) ∀ s: Key::parse(s) = Ok(segs) ⇔ s = ws* k1 ws* ('.' ws* ki ws*)* with each ki a bare key ([A-Za-z0-9_-]+), a basic-string key or a literal-string key; then segs[i].get() = ki decoded. A dot inside quotes belongs to the name. An empty key, or one with an empty, unterminated or space-separated segment, is refused. (b) ∀ raw: Key::new(raw).to_string() == encode_key(raw) = raw if raw is nonempty and all bare-key bytes, else encode_basic_string(raw). (c) ∀ raw: Key::parse(&Key::new(raw).to_string()) == Ok([raw]).	edit::key_parsing_and_canonical_spelling	MEDIUM: &str/String, Vec<Key>, loops, &mut self parser, Display	1,3,5,7
standalone-value	round-trip	<edit::Value as FromStr>::from_str, edit::parse::parse_value, <edit::Value as Display>::fmt	∀ s: edit::Value::from_str(s) = Ok(v) ⇔ s ∈ L_val with no leading or trailing whitespace, newline or comment and nothing after the value; then v.to_string() == s. Otherwise Err(e) with e.span() = Some(r) ∧ CharAligned(s, r).	edit::a_standalone_value_is_the_whole_input	MEDIUM: &str parsing loops, recursion (arrays, inline tables), String, Display	1,3,5,7
parsed-spans	invariant	edit::parse::parse_document, edit::value::parsed, Item::span, edit::Value::span, Table::span, Array::span	∀ s with P(s) = Ok(d): every parsed value node v (scalar, array, inline table) has span() = Some(r) with s[r] == v's authored token, delimiters and brackets included; every table opened by an explicit [h] header has span Some(r) with s[r] == the header text from '[' to ']'; every parsed key's span covers its authored spelling; all these r satisfy CharAligned(s, r).	edit::a_dotted_lookup_finds_the_value_and_keeps_its_span	MEDIUM-HARD: Range<usize> stored in tree nodes, &self views over enum/struct trees, Vec, loops	1,2,3,5,7
parsed-tree-shape	contract	edit::parse::parse_document, Parser::open_table, DocumentMut::{iter,get}, <DocumentMut as Index<&str>>::index, Table::is_implicit	∀ s with P(s) = Ok(d): (a) d.iter() yields each top-level key once, in order of first appearance in s. (b) k = val gives Item::Value, [k] gives Item::Table, [[k]] gives Item::ArrayOfTables with one element per occurrence, in order. (c) A table created only as a prefix of a deeper header has is_implicit() = true; an explicitly headed table has false. (d) ∀ k ∉ d: d.get(k) = None, &d[k] is Item::None, and d is unchanged.	edit::the_walking_surface	MEDIUM: ordered map, enum Item, &self Index returning a static, iterators	1,2,3,5,7
item-kind-predicates	table-property	Item::{is_none,is_value,is_str,is_integer,is_float,is_bool,is_datetime,is_array,is_table,is_table_like,is_array_of_tables,as_value,as_table,as_table_like,as_array_of_tables,as_str,as_integer,as_float,as_bool,as_array,get}, edit::Value::is_str	∀ item: Item: is_none ⇔ Item::None; is_value ⇔ Item::Value(_); is_str, is_integer, is_float, is_bool, is_datetime, is_array ⇔ Item::Value of the matching variant; is_table ⇔ Item::Table; is_array_of_tables ⇔ Item::ArrayOfTables; is_table_like ⇔ Item::Table ∨ Item::Value(InlineTable). ∀ kind X: as_X(item).is_some() ⇔ is_X(item), and as_X returns the payload. Item::get(k) = as_table_like()?.get(k).	edit::item_kind_predicates	EASY: an enum match on nested enums, &self, Option<&T>; &dyn TableLike (trait object) for as_table_like/get	1,2,3
edit-semantics	refinement	<DocumentMut as IndexMut<&str>>::index_mut, TableLike::insert, Table::insert, InlineTable::insert, edit::value::{inline_item,table_to_inline}, ArrayOfTables::push, DocumentMut::{remove,remove_path}, edit::encode::encode_document	∀ s with P(s) = Ok(d) (and for d = DocumentMut::new()), ∀ finite edit sequences ops (set by IndexMut, TableLike::insert into a Table or InlineTable, remove, remove_path, ArrayOfTables::push) whose semantic result is a legal TOML tree, with d' = ops(d): P(print(d')) is Ok and from_str::<Value>(print(d')) == Ok(sem_apply(ops, [[s]])). A Table or ArrayOfTables inserted into an InlineTable keeps its whole subtree's meaning, because inline_item converts it.	edit::a_nested_write_through_an_inline_table_lands_in_it	VERY HARD: &mut self through trait objects, recursion over the tree, String encoding, Vec, a two-state (before/after) spec; needs value-semantics	1,2,3,4,5,7,10
edit-byte-locality	contract	DocumentMut::remove_path, edit::document::line_owner_depth, <DocumentMut as IndexMut<&str>>::index_mut, TableLike::insert, edit::Value::decor_mut, Decor::{set_prefix,set_suffix}, ArrayOfTables::push, edit::encode::{encode_document,encode_table,end_line}	∀ s with P(s) = Ok(d): (a) Replacing an existing key's value with one that adopts the old value's decor prints s with only the old token's bytes replaced by the new value's repr. (b) remove_path(p) of a key-value line prints s with exactly that line's own bytes deleted (key, value, same-line comment, terminator); comment and blank lines above it stay where they were, and if it was the last line of its table they stay after that table's last line. (c) Inserting a new key into an existing table T, a new table, or a new array-of-tables element prints s with new text inserted only at the end of T's printed region (after the root region for a root key), with a '\n' first when the preceding line had no terminator; every byte of s appears unchanged and in order; when T's region ends the file, print(d') starts with s. (d) s starts with U+FEFF ⇒ print(d') starts with U+FEFF after any edit sequence.	edit::a_non_destructive_edit_keeps_every_other_byte, edit::a_removed_line_keeps_the_comment_lines_above_it, edit::a_byte_order_mark_stays_first, edit::interleaved_dotted_keys_keep_their_authored_order, edit::a_key_added_after_an_unterminated_final_line_starts_its_own_line, edit::a_table_added_after_an_unterminated_final_line_starts_its_own_line	HARD: &mut self, String decor (RawString), a two-state relation between source and printed text, loops, recursion; reuses roundtrip	1,2,3,5,7
canonical-encoding	contract	ser::{to_string,value_to_item,insert_unless_none}, ItemSerializer, StructSerializer::serialize_field, edit::encode::{encode_document,encode_key,encode_basic_string,encode_float}, <Datetime as Display>::fmt	∀ DocumentMut d built from new() by inserting only never-parsed items (no repr, default decor): print(d) == canonical(d), where canonical(d) = the root key-values, one 'encode_key(k) = repr\n' line each in insertion order, then each table as '\n[path]\n' followed by its key-value lines, recursively; strings use encode_basic_string, floats encode_float, arrays '[a, b]', datetimes Datetime::to_string; and canonical(d) ∈ L. ∀ x: Serialize whose serde model has a table root and no top-level None: to_string(&x) = Ok(canonical(value_to_item(M⁻¹(x)))). A field or map entry whose value serializes to none is omitted; no null spelling is ever written.	edit::a_document_built_by_hand_prints_legal_toml	HARD: serde Serializer trait impls, generic T, String building, recursion, f64 formatting	1,2,3,5,7,9,11
serde-roundtrip	round-trip	ser::to_string, de::from_str, <Datetime as Serialize>::serialize, <Datetime as Deserialize>::deserialize	∀ T: Serialize + DeserializeOwned + PartialEq with serde-derived impls (structs; unit, newtype and struct enums; Option; Vec; HashMap<String, _>; fixed arrays and tuples; aterm_toml::Datetime), ∀ x: T containing no NaN: to_string(&x) = Ok(t) ⇒ from_str::<T>(&t) = Ok(y) ∧ y == x. A None field is absent from t and reads back as None.	typed::a_typed_value_survives_serialize_then_deserialize, typed::absent_keys_and_none_values, typed::datetimes_round_trip_through_serde	HARD: generic T over serde traits, proc-macro derives, composition of callee contracts (canonical-encoding, deser-refines-model), recursion	1,4,5,9,11
deser-refines-model	refinement	de::from_str, de::Deserializer::{deserialize_any,deserialize_option,deserialize_struct,deserialize_enum}, de::MapAccess::{next_key_seed,next_value_seed}	∀ s ∈ dom(P), ∀ T: DeserializeOwned: from_str::<T>(s) is extensionally T::deserialize run on M([[s]]). Every table key, unknown ones included, is offered to the visitor, so deny_unknown_fields refuses a typo. An Integer i goes to visit_i64, so an iN/uN target accepts it ⇔ i is in range, and an f32/f64 target accepts every integer. A one-character String reads as char. An absent key reads as None (or the serde default). An enum reads from a bare string (unit variant) or a one-key table. Datetime reads through the private struct protocol.	typed::a_realistic_config_deserializes, typed::unknown_fields_are_refused_where_the_model_asks, typed::scalar_coercions	HARD: serde Deserializer/Visitor/MapAccess/SeqAccess/EnumAccess traits, generic T, recursion over the Item tree, derives	1,4,5,9,11
seq-drained	contract	de::drained, de::SeqAccess::next_element_seed, de::Deserializer::deserialize_any	(a) ∀ left, total: usize: drained(left, total) = Ok(()) ⇔ left = 0; otherwise Err(invalid_length(total, took = total.saturating_sub(left))). (b) ∀ s ∈ dom(P), ∀ fixed-length targets ([U; N], an N-tuple, a 1-tuple of structs) read from an array or array of tables of length m: the read succeeds only if m = N; when m ≠ N the error's span is the array node's span (the array token, or for an array of tables the key that names it). A Vec target accepts every m.	typed::a_fixed_length_target_refuses_a_longer_array	(a) EASY: a scalar comparison, saturating_sub and a serde error constructor (a call); (b) MEDIUM: the SeqAccess trait and generic visitors	1,3,4,11
unit-variant-empty	contract	<VariantValue as VariantAccess>::unit_variant, <UnitVariant as VariantAccess>::unit_variant	∀ VariantValue{item, key, source}: unit_variant() = Ok(()) ⇔ item is an empty Item::Table, an empty Value::InlineTable or an empty Value::Array; otherwise Err, with a message naming key and item.type_name() and the span of the variant body or key attached by with_span. UnitVariant (a bare-string variant) always returns Ok. Hence a unit-variant target reads Ok from a bare string, from '{ v = {} }', '{ v = [] }' or an entry-less '[t.v]', and Err from any non-empty body.	typed::a_unit_variant_in_table_form_carries_nothing	EASY: a nested enum match, len() on map/Vec, self by value, String formatting for the error	1,2,3
schema-error-location	invariant	de::Deserializer::{locate,mismatch}, Error::with_span, de::Node::span, de::MapAccess::next_value_seed, de::EnumAccess::variant_seed	∀ s ∈ dom(P), ∀ T: from_str::<T>(s) = Err(e) from deserialization (not syntax) ⇒ e.span() = Some(r) ∧ CharAligned(s, r). r is the span of the innermost node being deserialized when the error was raised, if it has one (a value token, or the [header] of an explicit table); otherwise the span of the key that names the node (a dotted or implied table, an array of tables via its first header's key, or a variant key in any of its three spellings). Display then follows error-display at r.start.	typed::schema_errors_carry_a_source_span, typed::a_schema_error_in_a_spanless_node_is_located_on_its_key	HARD: serde trait impls, generic T, Option<Range>, &self views, callee-error composition	1,2,4,5,9,11
value-index	contract	<aterm_toml::Value as Index<I>>::index, aterm_toml::Value::{get,as_str,as_integer,as_float,as_table}, <str as value::Index>::index_into, <usize as value::Index>::index_into, <Map<String, Value> as Index<&str>>::index	∀ v: Value, i: I with I: value::Index: v.get(i) == i.index_into(v). index_into by str/String = Some(t[k]) if v = Table(t) ∧ k ∈ t, else None. index_into by usize = Some(a[n]) if v = Array(a) ∧ n < len, else None. v[i] = i.index_into(v).unwrap() when Some, and panics with a message containing "index not found" ⇔ it is None. Each as_X returns Some exactly on its variant.	typed::the_untyped_table_and_index_surface	EASY-MEDIUM: a sealed generic trait, enum match, BTreeMap lookup, Vec indexing, panic as a specified outcome (#[should_panic] becomes a panics-iff clause)	1,2,3,11
```

### Distribution by kind (27)

| kind | count | ids |
|---|---|---|
| contract | 12 | depth-bound, char-boundary, unicode-escape, eof-empty-span, error-display, number-literal, parsed-tree-shape, edit-byte-locality, canonical-encoding, seq-drained, unit-variant-empty, value-index |
| round-trip | 4 | roundtrip, key-parse-spelling, standalone-value, serde-roundtrip |
| refinement | 4 | accept-domain, value-semantics, edit-semantics, deser-refines-model |
| invariant | 3 | span-aligned, parsed-spans, schema-error-location |
| table-property | 2 | days-in-month, item-kind-predicates |
| equivalence | 1 | entry-points-agree |
| no-panic/bounds | 1 | no-panic-bounded-stack |

### Proof dependency order

`char-boundary` → `span-aligned`, `error-display` → `eof-empty-span`, `schema-error-location`.
`days-in-month`, `number-literal`, `unicode-escape`, `key-parse-spelling` are lemmas of
`accept-domain` and `value-semantics`. `roundtrip` → `edit-byte-locality`.
`value-semantics` → `edit-semantics`. `canonical-encoding` + `deser-refines-model` →
`serde-roundtrip`. `entry-points-agree` (b) is definitional; (a) needs `deser-refines-model` on the
`Value` instance.

## 2. Test → spec map (TSV; all 47)

`primary` is the spec that retires the test. `also` lists the other specs the same test samples.

```tsv
file	test	primary	also
tests/cost.rs	many_lines_under_a_long_header_cost_one_pass	NON-SPEC (move-to-bench)	roundtrip, value-semantics
tests/depth.rs	deeply_nested_input_returns_an_error_instead_of_overflowing_the_stack	depth-bound	no-panic-bounded-stack, entry-points-agree
tests/depth.rs	ordinary_nesting_is_untouched	depth-bound	roundtrip
tests/depth.rs	constructs_that_each_fit_the_limit_cannot_stack_past_it	depth-bound	no-panic-bounded-stack, entry-points-agree
tests/depth.rs	the_rejection_path_itself_is_safe	no-panic-bounded-stack	
tests/edit.rs	a_non_destructive_edit_keeps_every_other_byte	edit-byte-locality	edit-semantics
tests/edit.rs	a_removed_line_keeps_the_comment_lines_above_it	edit-byte-locality	edit-semantics
tests/edit.rs	a_byte_order_mark_stays_first	edit-byte-locality	edit-semantics (pinned placement questioned, §5.6)
tests/edit.rs	a_dotted_lookup_finds_the_value_and_keeps_its_span	parsed-spans	
tests/edit.rs	key_parsing_and_canonical_spelling	key-parse-spelling	
tests/edit.rs	the_walking_surface	parsed-tree-shape	parsed-spans, item-kind-predicates
tests/edit.rs	interleaved_dotted_keys_keep_their_authored_order	edit-byte-locality	roundtrip
tests/edit.rs	a_document_built_by_hand_prints_legal_toml	canonical-encoding	edit-semantics
tests/edit.rs	a_standalone_value_is_the_whole_input	standalone-value	span-aligned
tests/edit.rs	item_kind_predicates	item-kind-predicates	parsed-tree-shape
tests/edit.rs	a_key_added_after_an_unterminated_final_line_starts_its_own_line	edit-byte-locality	edit-semantics, roundtrip
tests/edit.rs	a_table_added_after_an_unterminated_final_line_starts_its_own_line	edit-byte-locality	edit-semantics
tests/edit.rs	a_nested_write_through_an_inline_table_lands_in_it	edit-semantics	edit-byte-locality
tests/fuzz.rs	generated_documents_parse_and_round_trip	roundtrip	accept-domain, entry-points-agree, span-aligned
tests/fuzz.rs	single_character_mutations_round_trip_or_refuse_cleanly	roundtrip	entry-points-agree, no-panic-bounded-stack, span-aligned
tests/fuzz.rs	punctuation_noise_never_panics	no-panic-bounded-stack	span-aligned
tests/fuzz.rs	a_multi_byte_character_inside_a_unicode_escape_is_an_error_not_an_abort	unicode-escape	no-panic-bounded-stack, span-aligned, entry-points-agree, key-parse-spelling
tests/fuzz.rs	a_signed_unicode_escape_is_not_four_hex_digits	unicode-escape	accept-domain, span-aligned
tests/roundtrip.rs	every_toml_file_in_the_repository_round_trips_byte_for_byte	roundtrip	entry-points-agree (repository-content half moves to a census guard, §4)
tests/roundtrip.rs	formatting_details_survive	roundtrip	
tests/spec.rs	everything_the_spec_allows_parses	accept-domain	roundtrip, entry-points-agree
tests/spec.rs	everything_the_spec_forbids_is_refused	accept-domain	number-literal, days-in-month
tests/spec.rs	parse_errors_carry_a_span_inside_the_source	span-aligned	
tests/spec.rs	an_implicit_super_table_may_be_declared_later	value-semantics	accept-domain, roundtrip
tests/spec.rs	multiline_strings_handle_delimiter_runs	value-semantics	
tests/spec.rs	overflowing_float_literals_are_refused_in_both_signs	number-literal	value-semantics
tests/spec.rs	a_schema_error_renders_the_offending_line	error-display	schema-error-location
tests/spec.rs	end_of_input_errors_point_at_the_last_authored_line	eof-empty-span	error-display
tests/spec.rs	token_spans_underline_the_token	span-aligned	(name claims more than it checks, §5.3)
tests/typed.rs	a_realistic_config_deserializes	deser-refines-model	
tests/typed.rs	a_typed_value_survives_serialize_then_deserialize	serde-roundtrip	
tests/typed.rs	absent_keys_and_none_values	serde-roundtrip	canonical-encoding
tests/typed.rs	unknown_fields_are_refused_where_the_model_asks	deser-refines-model	
tests/typed.rs	schema_errors_carry_a_source_span	schema-error-location	span-aligned
tests/typed.rs	scalar_coercions	deser-refines-model	
tests/typed.rs	a_fixed_length_target_refuses_a_longer_array	seq-drained	schema-error-location
tests/typed.rs	a_unit_variant_in_table_form_carries_nothing	unit-variant-empty	
tests/typed.rs	datetimes_round_trip_through_serde	serde-roundtrip	days-in-month, canonical-encoding
tests/typed.rs	the_untyped_table_and_index_surface	value-index	entry-points-agree, value-semantics, span-aligned
tests/typed.rs	a_schema_error_in_a_spanless_node_is_located_on_its_key	schema-error-location	
```

## 3. Coverage

- 47 tests read. 46 map to one of the 27 specs. 1 is not a spec sample (§4).
- The biggest collapses: `edit-byte-locality` (6 primary), `roundtrip` (4 primary, plus 6
  shared), `depth-bound`, `value-semantics`, `serde-roundtrip` and `deser-refines-model` (3 each).
- Three specs have no primary test: `entry-points-agree` (sampled by 9 tests as a side check),
  `char-boundary` (a lemma no test isolates) and `days-in-month` (sampled inside the grammar and
  datetime tests). Each is still needed: the first is how one parser proof covers six entry points,
  the other two are the scalar cores the bigger proofs are built on.

## 4. Tests that are not spec samples

| test | why | action |
|---|---|---|
| cost::many_lines_under_a_long_header_cost_one_pass | It checks that parsing is linear in input size (`current_table` resolves by position, not by name), and detects a regression only by never finishing, so it is a hang or timing check in disguise. Trust cannot state a complexity bound today. Its functional assertions (table length = `LINES`, byte round-trip, the serde half agrees) are samples of `roundtrip` and `value-semantics`. | **move-to-bench** (`aterm-bench`), then delete it here |
| roundtrip::every_toml_file_in_the_repository_round_trips_byte_for_byte (repository-content half only) | Half of this test is a guard over the repository's content: every tracked `.toml` parses (handoff fixtures excluded). No crate spec can subsume that. It reads the filesystem, silently `continue`s on unreadable files, and `corpus()` asserts more than 100 files. The test's crate half is a sample of `roundtrip`. | **move the content half to an `aterm-census` guard** once `roundtrip` and `entry-points-agree` are proved; the test then goes |

## 5. Implied specs that look wrong, or disagree with the code

1. **A tautological count.** `fuzz::a_multi_byte_character_inside_a_unicode_escape_is_an_error_not_an_abort`
   says "2 widths x 12 offsets x 3 intruders" (72) in its assertion message, but the grid is
   (4 + 8) lead offsets × 3 intruders = 36. The final `checked == 36` counts only its own loop
   iterations, so it can never fail. `unicode-escape` states the real property.
2. **Stale prose from the two-implementation era.** In `spec::everything_the_spec_forbids_is_refused`,
   the INVALID doc ("Both implementations must REFUSE these") and the module text date from when the
   crate was diffed against `toml`/`toml_edit`; only one implementation is exercised now. The module
   doc says "509 deliberately-invalid .toml files" while the measured line says
   "invalid: 492 pass / 0 fail".
3. **A name claims more than the test checks.** `spec::token_spans_underline_the_token` claims every
   span underlines the offending token, but its loop checks only bounds and char alignment, which
   duplicates `parse_errors_carry_a_span_inside_the_source` on the `DocumentMut` entry. Token
   underlining is pinned at one point (`int = 010` → `6..9`). No general token-span rule for errors
   is stated or tested. `span-aligned` is what the test actually samples. If token underlining is
   wanted, it is a new spec (the error-side analogue of `parsed-spans`).
4. **A repository guard sits in the crate's tests** (`roundtrip::every_toml_file_in_the_repository_round_trips_byte_for_byte`; see §4).
5. **A message describes the wrong operator.** In `edit::the_walking_surface`, "indexing an absent key
   must vivify to the vacant item" describes `IndexMut`. `&doc["missing"]` goes through `Index`, which
   returns a static `VACANT` and inserts nothing. `parsed-tree-shape` (d) states the `Index` behaviour.
6. **A pinned output may contradict the edit contract.** `edit::a_byte_order_mark_stays_first` pins a
   newly set root key placed above the user's leading comment
   (`"\u{feff}font_px = 14.0\n# my aterm settings\n..."`), so the comment that opened the file moves
   below the new key. That cuts against the "comment lines stay where they were written" contract
   stated for `remove_path`. Confirm this placement is intended before it becomes part of
   `edit-byte-locality` (c), which as stated puts a root key at the end of the root region.
7. **The depth bound's value is never pinned.** `depth::ordinary_nesting_is_untouched` and
   `deeply_nested_input_returns_an_error_instead_of_overflowing_the_stack` probe 32 and 10,000+
   levels while the limit is 128. The boundary (128 accepted, 129 refused) is never sampled.
   `depth-bound` states it exactly. The message checks ("nesting" or "limit" in one test, "limit" in
   another) pin prose; the spec keeps only "limit".
8. **Inconsistent call-site counts in prose.** The `typed.rs` module and test docs say "the path 43
   call sites" and "the other half of the 168 call sites". No test checks either number.
9. **A spec with no gap row.** `no-panic-bounded-stack` needs a stack-depth (recursion and `Drop`
   glue) bound. No row of audit §7 covers stack-depth reasoning; the gaps listed (1, 3, 7, 16) are
   necessary but not sufficient. A new §7 row is needed, or the parser and `Drop` must be made
   iterative so that the bound is a loop variant.

## 6. Proof order (easiest high-value first)

No spec is blocked by gap 0 alone: gap 1 (non-scalar carriers) is the floor for all 27. So the order
below starts with specs whose only other blocker is gap 3 (calls, matches, loops in reflected bodies)
and gap 7 (loops), on scalar or near-scalar cores.

1. **`seq-drained` (a)**, **`days-in-month` (a)**, **`char-boundary`**. Scalar or near-scalar:
   one comparison and `saturating_sub`; a match on integer literals with `is_multiple_of`; two
   `usize` while-loops over `is_char_boundary`. Gaps 1, 3 (and 7 for `char-boundary`). They retire
   no test alone, but they are the lemmas under `span-aligned`, `error-display`, `accept-domain` and
   the fixed-length half of `seq-drained`.
2. **`item-kind-predicates`**, **`unit-variant-empty`**, **`unicode-escape`**. Enum matches and
   `&self`/`&mut self` views (gaps 1, 2, 3, plus 7 for the escape window). They retire 4 tests
   outright and are the first users of gap 2.
3. **`error-display`**, **`number-literal`**, **`key-parse-spelling`**. String-scanning contracts
   (gaps 1, 3, 7; plus 11 for the `Formatter`, 5 for keys). `number-literal` needs a float-parse
   model for its round-to-nearest clause; its range half (None on overflow) can land first.
4. **`span-aligned`**, **`eof-empty-span`**, **`depth-bound`**, **`value-index`**. Invariants over
   every error site and the depth guard (gaps 1, 2, 3, 5, 7). `span-aligned` retires 2 tests and is
   shared by 8 more.
5. The whole-parser refinements: **`roundtrip`**, **`accept-domain`**, **`value-semantics`**,
   **`entry-points-agree`**, plus **`standalone-value`**, **`parsed-spans`**, **`parsed-tree-shape`**
   and **`no-panic-bounded-stack`**. These need a Lean model of TOML 1.0 (gaps 10, 14) and the
   stack-depth gap of §5.9. They retire the fuzz, spec and roundtrip files.
6. The edit specs (**`edit-byte-locality`**, **`edit-semantics`**), then the serde specs
   (**`canonical-encoding`**, **`deser-refines-model`**, **`schema-error-location`**,
   **`seq-drained`** (b), **`serde-roundtrip`**). The serde specs need gap 9 (serde derives across
   the proc-macro boundary) and gap 11 (generic `T`).
