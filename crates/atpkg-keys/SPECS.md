<!-- Copyright 2026 Andrew Yates -->
<!-- SPDX-License-Identifier: Apache-2.0 -->

# atpkg-keys: spec recovery from its 100 unit tests

> Status 2026-10-16, after the merge with the tests-and-gates audit: the rows naming 3 tests it
> deleted are gone, and the spec rows no longer cite those tests. Counts elsewhere in this file
> were taken before.

> Status 2026-10-16: this is the crate's migration worklist. Each spec below is proved
> through Trust/Clean about the compiled Rust function, and then the tests it subsumes are
> deleted. The 2 tests in §4 that sample no spec are handled as that table says. Nothing
> has been deleted yet.

Source: `crates/atpkg-keys` (read only). I read every `#[test]` in src/{lib 4, fsio 5, master 13,
pins_edit 14, provision 27, roster_ops 8} and tests/{owner_to_client 8, paper_master_to_client 2,
provision_cli 14, roster_redo_model 5} (= 100), plus the code each one reaches, including the parts
of `src/main.rs` that the CLI tests drive (`vet_args`, `flag`, `provision`, `confirm_transcription`,
`machine_revoke`). `tests/keys_suite.rs` has no test; its module doc mentions the literal `#[test]`,
which is why a raw grep counts 101. Nothing was compiled or run. The claims below come from reading
the code.

Result: **43 specs**, covering **98 tests**. **2 tests are not spec samples** (§4). Three of the
specs (`client-*`) are properties of `atpkg` and `aterm-update-core` that this crate's integration
tests happen to sample. They are recorded here so the tests are accounted for, and they should
move to those crates' `SPECS.md` (§5.12).

## 0. Notation used in the statements

- `ALPH` = `b"0123456789abcdefghjkmnpqrstvwxyz"` (32 symbols). `val(c)` = the index of `c` in `ALPH`,
  that is, `master::b32_val` restricted to `ALPH`. Table fact: `∀ i<32: b32_val(ALPH[i]) = i`.
- `norm(s)` = `map(fold, filter(|c| ¬c.is_ascii_whitespace() ∧ c ≠ b'-', s.bytes()))`, where
  `fold(c)` = `match c.to_ascii_lowercase() { b'o' => b'0', b'i' | b'l' => b'1', x => x }`.
- `B32dec(cs: [u8;52]) : [u8;32]` = the first 256 bits of the big-endian concatenation of the 5-bit
  groups `val(cs[j])`; the low 4 bits of `val(cs[51])` are dropped. `B32enc(raw: [u8;32]) : [u8;52]`
  = the 5-bit regrouping of raw's 256 bits followed by 4 zero bits, so the last char is
  `ALPH[(raw[31] & 1) << 4]`.
- `Id(seed)` = the Ed25519 public key of `from_seed_unchecked(seed)`. `b64` =
  `aterm_codec::base64::encode`. `FP(pk)` = `hex(SHA256(pk)[0..2]) ++ "-" ++ hex(SHA256(pk)[2..4])`,
  lowercase. `dec(n)` = the decimal rendering of `n`.
- **CRYPTO** (assumed, never proved): EUF-CMA for Ed25519 (no valid signature on `m` under `pk`
  without the secret key), and collision resistance of SHA-256 and of `Id`. Every "a different X
  gives a different Y" and every "a tampered message or a foreign key is rejected" assertion rests on
  these. Such assertions are cited as axioms, not proof obligations. `ring`'s Ed25519, SHA-256 and
  `SystemRandom` enter as assumed (FFI) specs.
- **Anchor grammar** for `pins_edit::read_anchor(src, name)`. `L` = `lines_with_spans(src)`: each line
  keeps its newline in its span, and its text has the trailing `\n` and `\r` stripped. `D` = the lines
  whose text `starts_with("pub const NAME:")`. `OPEN` = `pub const NAME: &[&str] = &[`. `EMPTY` =
  `OPEN ++ "];"`. For an entry line `t' = t.trim()`: **Blank** if `t' = ""`; **Comment** if `t'` starts
  with `//`; **Member(x)** if `t' = "\"" x "\","` and `x` contains no `"` and no `\`; otherwise
  unrecognised.
- `FS`: the filesystem projection `Path → Option<Bytes>`, plus mode bits. `FS'` is the state after the
  call. `entries(dir)` is the set of names in `dir`. A **run** is `preflight`, then `plan`, then
  `write_pins`, then `write_rest`.
- `RosterPairRedo`: the `ty_model!` model `aterm_models_release::roster_pair_redo_model`. Its projection
  is the one in `tests/roster_redo_model.rs::project`: body and signature are each classified 0 =
  predecessor-or-absent, 1 = target, 2 = foreign; plus redo-marker presence, `writer_lock`, and outcome
  counters.
- "Contains K" in an error statement means the error's text contains the literal `K`. The spec fixes the
  error variant and the facts it names (counts, positions, paths), not the surrounding prose.

## 1. Specs (TSV)

Columns: `id`, `kind`, `functions`, `statement`, `subsumes (primary tests; secondary in §2)`,
`difficulty (Rust features the proof must handle)`, `blocking gaps` (audit §7 numbers). Gap 0, the
seal that publishes Clean rows, blocks every row and is not repeated per row (§6).

```tsv
id	kind	functions	statement	subsumes	difficulty	blocking gaps
master-seed-decode	contract	master::MasterPhrase::seed, master::b32_val	∀ phrase p with every p.chars[i] ∈ ALPH (the invariant parse_master and generate_master establish): seed(p).bytes = B32dec(p.chars), and the loop's out index reaches exactly 32. Hence seed depends only on the first 256 bits: two phrases that differ only in the low 4 bits of val(chars[51]) give equal seeds, so equal Id, pubkey_b64 and fingerprint. Phrases that differ in any of the first 256 bits give different seeds (pure fact); the step to a different Id is CRYPTO.	master::the_padding_bits_of_the_last_character_are_masked	EASY-MEDIUM: for-loop over [u8;52] with a u32 shift/or accumulator, a conditional emit into [u8;32] (loop invariant out = ⌊5j/8⌋), range match on u8, the unsafe scrub on exit	3, 5, 7
master-base32-roundtrip	round-trip	master::generate_master, master::MasterPhrase::seed	Factor out the CSPRNG draw raw: [u8;32]; then generate_master's chars = B32enc(raw). ∀ raw: (a) every chars[i] ∈ ALPH (so the phrase is lowercase); (b) B32dec(B32enc(raw)) = raw, so seed(generate) = raw; (c) ∀ cs ∈ ALPH^52 with the low 4 bits of val(cs[51]) = 0: B32enc(B32dec(cs)) = cs; (d) parse_master(str(chars)) = Ok with the same chars. Freshness (two calls differ) is a property of SystemRandom, not of this code, and is not part of the spec.	master::generate_produces_a_fresh_fifty_two_character_phrase	MEDIUM: nested while loop with a shift accumulator, array index by out (invariant 5·out ≤ 8·k), u32 shifts; the RNG call must be split out as an opaque input	3, 5, 7, 8
master-parse-contract	contract	master::parse_master	∀ input: &str, t = norm(input). If ∃ i: t[i] ∉ ALPH, then parse_master(input) = Err(BadChar{position: i0+1}) for the least such i0, whatever |t| is (the character check precedes the length check). Else if |t| ≠ 52: Err(Length{got: |t|}). Else Ok(p) with p.chars = t. No write goes outside chars[0..52] for any |t| (no panic). Corollaries: parse_master(s) = parse_master(s.to_ascii_uppercase()); o/i/l parse as 0/1/1; separators do not count toward positions.	master::exactly_fifty_two_characters_is_accepted_and_case_folds, master::o_i_and_l_are_read_as_the_characters_they_resemble, master::fifty_one_and_fifty_three_characters_are_refused_with_the_count, master::a_bad_character_is_refused_by_position_and_never_echoed, master::a_bad_character_is_reported_even_when_the_length_is_also_wrong, master::separators_are_stripped_and_positions_count_significant_characters	EASY-MEDIUM: loop over str::bytes, a fixed [u8;52] with a guarded index, early return, u8 ascii helpers, contains over a const array, an enum result; scrub (unsafe write_volatile) on the error paths	1, 3, 5, 7
phrase-error-message	contract	master::PhraseError::message	∀ p: BadChar{position: p}.message() = "character " ++ dec(p) ++ K1. ∀ g: Length{got: g}.message() = K2 ++ dec(g), with K1, K2 the fixed literals. The message is a function of the PhraseError value alone; since PhraseError holds no input byte, no character of the phrase can appear in it (non-disclosure by construction).	(secondary only: master::fifty_one_and_fifty_three_characters_are_refused_with_the_count, master::a_bad_character_is_refused_by_position_and_never_echoed)	EASY: String push_str, usize::to_string (needs a decimal-rendering model), match on an enum	1, 3
fingerprint-format	contract	master::fingerprint	∀ pk: &[u8], d = SHA256(pk): fingerprint(pk) = hex2(d[0]) ++ hex2(d[1]) ++ "-" ++ hex2(d[2]) ++ hex2(d[3]). Hence len = 9, byte 4 = '-', every other byte ∈ [0-9a-f].	(secondary only: master::the_fingerprint_matches_on_a_correct_transcription_and_differs_otherwise)	EASY: iterator take/enumerate, String push of chars, a nibble lookup in a const table; SHA-256 opaque	1, 3, 7, 8
master-identity-function	equivalence	master::MasterSeed::{keypair,pubkey_b64,fingerprint,sign}, master::fingerprint	∀ seed s: pubkey_b64(s) = b64(Id(s)); fingerprint(s) = FP(Id(s)); sign(s, m) = the Ed25519 signature of m under s. Each is a function of s.bytes alone (the same phrase gives the same identity), and |base64_decode(pubkey_b64(s))| = 32. The tests' inequalities (a different phrase, a one-character slip, fingerprint(seed bytes) ≠ fingerprint(pubkey)) are CRYPTO applied to B32dec-distinct seeds.	master::the_fingerprint_is_taken_over_the_public_key, master::the_fingerprint_matches_on_a_correct_transcription_and_differs_otherwise	MEDIUM: ring Ed25519/SHA-256 (asm/FFI, assumed spec), &self on a struct with Drop, String result	1, 2, 4, 8, 12
scrub-zeroes	contract	master::scrub, master::scrub_u32	∀ buf: &mut [u8]: after scrub(buf), |buf'| = |buf| and ∀ i: buf'[i] = 0. After scrub_u32(v), v' = 0. (That the optimizer keeps the volatile store is a compiler guarantee, outside the spec.)	master::scrub_zeroes_the_buffer	EASY in logic, blocked on tooling: &mut slice, iter_mut loop, unsafe ptr::write_volatile, compiler_fence	1, 2, 7, 8
secret-debug-redaction	contract	master::{MasterSeed,MasterPhrase}::fmt, provision::{Preflight,Planned}::fmt	Debug(MasterSeed) = "MasterSeed(<redacted>)" and Debug(MasterPhrase) = "MasterPhrase(<redacted>)", constants independent of every field. Debug(Preflight{verb,id,..}) = "Preflight(" ++ verb.name() ++ ", " ++ id ++ ")". Debug(Planned{verb,id,..}) = "Planned(" ++ verb.name() ++ ", " ++ id ++ ", secret=<redacted>)". None depends on seed, chars, machine_pkcs8 or pins_src (noninterference of the secret fields into the formatter output).	master::debug_never_prints_secret_material, provision::debug_renderings_carry_no_secret_material	EASY in logic: trait impl fmt::Debug, Formatter write_str (needs a sink model), &self	1, 2, 11
keygen-pubkey-consistency	contract	generate, pubkey_b64, sign	∀ (k, pk) = generate()?: pubkey_b64(k) = Ok(pk) and |base64_decode(pk)| = 32. ∀ b not a valid Ed25519 pkcs8 document: sign(b, m) = Err and pubkey_b64(b) = Err (fail closed, no panic).	lib::pubkey_b64_recovers_the_published_key, lib::invalid_key_fails_closed	MEDIUM: ring pkcs8 parse and RNG (FFI), Vec<u8>/String results, map_err	1, 4, 8, 12
sign-verify-roundtrip	round-trip	sign, aterm_update_core::roster::Roster::authorize_appcast	∀ generated (k, pk), msg, and roster R listing a live machine {id, pubkey: pk} at now: R.authorize_appcast(msg, sign(k, msg), now) = Ok(who) with who.machine_id = id and who.pubkey_b64 = pk. Rejection of a tampered msg or another key's signature (Err(Verify)) is CRYPTO applied to authorize_appcast's own verify spec.	lib::produces_signatures_the_client_verifier_accepts	MEDIUM: cross-crate call (Vec of structs, String compare), ring verify (FFI)	1, 4, 8, 12
verify-signed-is-roster-chain	equivalence	verify_signed	∀ inputs: verify_signed(mp, roster, rsig, file, sig, now) = verify_roster(mp, roster, rsig) >>= Roster::parse >>= admit(0, now) >>= authorize_appcast(file, sig, now), projected to Signed{machine_id, roster_seq}; the first Err in that order is returned unchanged. The test's negative cases (tamper, stranger key, forged roster, revoked, stale) are then the specs of those four aterm_update_core functions plus CRYPTO.	lib::verify_signed_accepts_a_rostered_signature_and_nothing_else	EASY given the callee specs: ? over a Result chain, struct construction from a callee's String/u64; callee-ensures composition across crates	1, 4, 12
owner-client-roundtrip	round-trip (composite)	master::{parse_master,MasterPhrase::seed,MasterSeed::{sign,pubkey_b64}}, roster_ops::{empty,add}, generate, sign	∀ valid phrase P, id accepted by add, machine key (k, pk) = generate(), appcast A carrying machine_id = id and roster_seq = s: let R = add(empty(now), id, pk, now), B = R.to_toml(), mpk = pubkey_b64(seed(P)). Then verify_roster([mpk], B, seed(P).sign(B)), parse, admit(0, now), authorize_appcast(A, sign(k, A), now) and bind(Some(id), Some(R.roster_seq)) all succeed, with machine_id = id and pubkey = pk. Composition of master-identity-function, roster-add-contract and sign-verify-roundtrip.	paper_master_to_client::a_master_phrase_mints_a_machine_whose_release_the_client_accepts	FREE given its parts plus a TOML emit/parse round trip in aterm_update_core (String-heavy)	1, 4, 8, 12
roster-add-contract	contract	roster_ops::add, roster_ops::empty	∀ r, id, pk, now: add(r, id, pk, now) = Err ⇔ id = "" ∨ id ∈ ids(r.machines) ∨ id ∈ r.revoked ∨ pk ∈ pubkeys(r.machines) ∨ |r.machines| ≥ MAX_MACHINES. Otherwise Ok(r') with r'.machines = r.machines ++ [Machine{id, pubkey: pk, added_at: format_rfc3339(now), not_after: None}], r'.revoked = r.revoked, r'.roster_seq = r.roster_seq + 1, r'.valid_until = VALID_UNTIL_FOREVER, r'.schema = r.schema. empty(_) = Roster{SUPPORTED_SCHEMA, 0, FOREVER, [], []}.	roster_ops::adding_a_machine_lists_it_and_advances_the_sequence, roster_ops::reusing_a_machine_id_is_refused_rather_than_overwriting_the_key, roster_ops::reusing_a_key_under_a_second_id_is_refused_because_revocation_names_ids, roster_ops::a_revoked_id_can_never_be_re_added, roster_ops::the_roster_has_a_hard_ceiling, roster_ops::every_edit_stamps_the_forever_window	MEDIUM: Vec<Machine> iter().any/find over String fields, push, by-value struct move, u64 increment (requires roster_seq < u64::MAX), format_rfc3339 (cross-crate)	1, 3, 4, 5, 7, 12
roster-revoke-contract	contract	roster_ops::revoke	∀ r, id: revoke(r, id, _) = Err ⇔ id ∉ ids(r.machines) ∧ id ∉ r.revoked. Otherwise Ok(r') with r'.machines = filter(m.id ≠ id, r.machines) (order kept); r'.revoked = r.revoked ++ [id] if id ∉ r.revoked, else r.revoked; roster_seq + 1; valid_until = FOREVER. (States the code as written; see §5.9.)	roster_ops::revoking_removes_the_machine_and_names_it_in_the_deny_list, roster_ops::revoking_an_unknown_id_is_an_error_not_a_silent_no_op	MEDIUM: Vec::retain with a closure, any, push, is_revoked (cross-crate &self)	1, 2, 4, 5, 7, 12, 13
roster-edits-preserve-validity	invariant	roster_ops::{empty,add,revoke}, aterm_update_core::roster::Roster::validate	validate(empty(t)) = Ok. ∀ r with validate(r) = Ok: add(r, ..) = Ok(r') ⇒ validate(r') = Ok, and revoke(r, ..) = Ok(r') ⇒ validate(r') = Ok. Uses validate's own definition: unique ids, unique keys, revoked ids not listed, |machines| ≤ MAX_MACHINES.	(secondary only: the add/revoke tests in roster_ops)	MEDIUM given validate's spec: quantified uniqueness over a Vec	1, 4, 5, 7, 12
forever-admits	table-property	roster_ops::VALID_UNTIL_FOREVER, aterm_update_core::roster::Roster::admit	∀ r with r.valid_until = VALID_UNTIL_FOREVER and r.roster_seq ≥ f, ∀ now < parse_rfc3339("9999-12-31T00:00:00Z"): r.admit(f, now) = Ok. The constant parses under the client's strict parser, which is decidable by evaluating the parser once.	(secondary only: roster_ops::every_edit_stamps_the_forever_window)	EASY by evaluation once the cross-crate RFC 3339 parser (loops, digit arithmetic over &str) is admitted; i64 compare	1, 3, 4, 7, 12
vet-key-contract	contract	pins_edit::vet_key	∀ key: &str: vet_key(key) = Ok ⇔ |key| = 44 ∧ key ends with '=' ∧ every byte ∈ [A-Za-z0-9+/=]. The checks run in that order; the length error contains dec(|key|) and "exactly 44", the alphabet error "non-base64". append_member returns the same Err before reading src, so no edit is planned for a malformed key.	pins_edit::a_malformed_key_is_refused_before_any_edit_is_planned	EASY: &str len/ends_with, bytes().all with a closure, concat of &str parts	1, 3, 5, 7
leading-ws-no-panic	no-panic/bounds	pins_edit::leading_ws, pins_edit::lines_with_spans	∀ text: &str: leading_ws(text) does not panic and returns the maximal prefix of text made of Unicode whitespace; text.len() − text.trim_start().len() is always a char boundary. lines_with_spans(src) partitions [0, |src|) into consecutive spans whose texts are the lines minus their trailing \n and \r. Trailing whitespace on a line never affects the indent.	pins_edit::a_multibyte_comment_with_trailing_whitespace_does_not_panic	MEDIUM: str slicing at computed offsets (char-boundary obligation), trim_start/trim_end_matches, find, a while loop building Vec<(&str, Span)> with lifetimes	1, 3, 5, 7
anchor-read-contract	contract	pins_edit::read_anchor, pins_edit::classify	∀ src, name: read_anchor(src, name) = Ok(a) ⇔ |D| = 1 and either (EmptyInline) d.trim_end() = EMPTY, giving a.members = [], indent = four spaces, newline = "\r\n" if d's span ends with \r\n else "\n"; or (Open) d.trim_end() = OPEN, a first later line has text exactly "];", and every line strictly between classifies as Blank, Comment or Member, giving a.members = the in-order Member payloads, indent = leading_ws of the first Comment or Member line (default four spaces), terminator = the span of that "];" line. Otherwise Err, classified: |D| = 0 contains "will not guess"; |D| > 1 contains dec(|D|) ++ " declarations"; another declaration spelling contains "refusing to guess"; an unrecognised entry contains "does not recognise" and that line's text; no terminator contains "never closed". Never panics.	pins_edit::both_declaration_shapes_are_read_and_the_head_is_index_zero, pins_edit::unrecognised_shapes_are_refused_not_guessed_at	MEDIUM: Vec<(&str, Span)>, enumerate/skip loops with early return, starts_with/strip_prefix/strip_suffix/contains, Option<String>, String building	1, 3, 5, 7
anchor-append-contract	contract	pins_edit::append_member	∀ src, name, key, comment, max with vet_key(key) = Ok and read_anchor(src, name) = Ok(a): (1) ∃ i: a.members[i] = "" ⇒ Err ("empty string", "brick"); (2) else key ∈ a.members ⇒ AlreadyPresent{members: a.members}; (3) else |a.members| + 1 > max ⇒ Err (contains "ceiling is " ++ dec(max)); (4) else Changed{text, members: a.members ++ [key]}, where E = concat(a.indent ++ "// " ++ c ++ a.newline for c ∈ comment) ++ a.indent ++ "\"" ++ key ++ "\"," ++ a.newline, and text = src[..t.start] ++ E ++ src[t.start..] (Open) or src[..d.start] ++ OPEN ++ a.newline ++ E ++ "];" ++ nl ++ src[d.end..] with nl = a.newline if d had a newline else "" (EmptyInline). Consequences: src's lines are an in-order subsequence of text's with exactly |comment| + 1 lines added; read_anchor(text, name).members = a.members ++ [key] (the head stays at index 0); ∀ other n with read_anchor(src, n) = Ok, its members are unchanged; every line of E ends with a.newline (CRLF stays CRLF); append_member(text, name, key, ..) = AlreadyPresent. A plan that does not read back is Err, never Changed.	pins_edit::appending_the_same_key_twice_produces_one_entry, pins_edit::the_existing_keys_survive_and_the_head_stays_at_index_zero, pins_edit::nothing_outside_the_edited_block_is_disturbed, pins_edit::the_empty_form_opens_into_a_one_member_list_without_touching_its_doc, pins_edit::trailing_whitespace_on_a_neighbouring_line_does_not_poison_the_indent, pins_edit::a_crlf_file_keeps_its_line_endings, pins_edit::an_anchor_holding_an_empty_member_is_refused, pins_edit::the_keyset_ceiling_is_enforced_by_the_writer, provision::a_second_pass_over_an_already_listed_key_adds_no_second_entry	MEDIUM-HARD: String building by slicing src at spans, Vec<String> clone/push, enum result with owned fields, a self-call to verify_members/read_anchor (callee-ensures), line-subsequence reasoning	1, 3, 4, 5, 7
verify-members-contract	contract	pins_edit::verify_members	∀ src, name, expected: verify_members(src, name, expected) = Ok ⇔ read_anchor(src, name) = Ok(a) ∧ a.members = expected (element by element, in order). Errors: a read failure contains "could not be read back"; a length mismatch contains "holds " ++ dec(|a.members|) ++ " keys but " ++ dec(|expected|) ++ " were intended"; otherwise the first differing index i gives "[" ++ dec(i) ++ "]" .. "was intended".	pins_edit::verification_catches_a_write_that_did_not_land	EASY given anchor-read-contract: zip/enumerate over Vec<String>, String eq, concat	1, 3, 4, 5, 7
shipped-pins-readable	table-property	pins_edit::read_anchor, aterm_update_core::pins::PAPER_MASTER_PUBKEYS	read_anchor(include_str!("../../aterm-update-core/src/pins.rs"), MASTER_ANCHOR) = Ok(a) with a.members = PAPER_MASTER_PUBKEYS.map(to_string). A closed ground fact, decidable by evaluating read_anchor on the committed source; best kept as a compile-time const assertion or build check so it is re-proved whenever pins.rs changes.	pins_edit::the_shipped_pins_file_is_a_shape_this_writer_understands	EASY by evaluation once read_anchor is admitted (const-eval or a kernel evaluator over String code); cross-crate const	1, 3, 7, 12
fsio-atomic-replace	contract	fsio::write_bytes_atomic, fsio::write_bytes_atomic_with_sync, fsio::stage_sibling_temp_with_mode	For each run without an external edit, with E = entries(dir(path)) before: Ok ⇒ FS'(path) = bytes, entries(dir) = E ∪ {basename(path)}, and no staged temp survives. A stage or rename Err leaves the destination unchanged by this run; temporary cleanup is attempted. A parent-sync Err follows a successful rename, so the new bytes already reached the path but directory durability is unconfirmed. The shared internal error records renamed = false before a successful rename and true afterward, independently of a later readback or external restore. The public io::Result adapter preserves the original io error. The staging name is path ++ ".atpkg-keys." ++ pid ++ "." ++ seq ++ ".tmp" with seq fresh from TEMP_SEQUENCE; an AlreadyExists collision retries with the next seq. Files under unrelated names (including legacy ".atpkg-keys.tmp") are not read or removed.	fsio::an_atomic_write_replaces_the_file_and_leaves_nothing_behind, fsio::a_failed_atomic_write_leaves_the_original_intact, fsio::a_crash_left_temp_never_wedges_the_next_atomic_write	HARD (effects): OpenOptions create_new/mode, write_all, sync_all, rename, remove_file, an AtomicU64 counter, a loop matching io::ErrorKind	1, 7, 8, 13
fsio-owner-create-new	contract	fsio::write_owner_file_create_new	∀ path, bytes: FS(path) = None ∧ Ok ⇒ FS'(path) = bytes (complete) with mode & 0o777 = 0o600, requires a umask that keeps the owner rw bits (§5.11). FS(path) ≠ None ⇒ Err ∧ FS'(path) = FS(path) ∧ entries(dir) unchanged (the staged temp is removed). The final name never exposes a partial inode: hard_link comes after sync_all.	fsio::an_owner_file_is_complete_private_and_never_replaced	HARD (effects): hard_link, remove_file, parent-dir sync, mode bits, umask as an environment assumption	1, 8
fsio-ensure-parent-dir	contract	fsio::ensure_parent_dir	∀ path: parent(path) is None or empty ⇒ Ok with no effect. Otherwise Ok ⇒ parent(path) is a directory afterwards and each newly created ancestor's entry was synced. Idempotent: a second call on the same path is Ok with no further creation.	fsio::ensure_parent_dir_creates_a_missing_chain_and_is_idempotent	MEDIUM (effects): Path::parent loop into Vec<PathBuf>, symlink_metadata, create_dir_all, reverse iteration, to_str Option	1, 7, 8
provision-preflight-table	contract	provision::preflight, provision::vet_machine_id	preflight(verb, id, paths) is decided in this order; the first failing row is the Err: vet_machine_id(id); pins is readable UTF-8; read_anchor(pins, MASTER_ANCHOR) = Ok(m). Setup: m.members = [] (else contains "ALREADY committed", "join", "strand"); ¬(pins discovered ∧ compiled PAPER_MASTER_PUBKEYS ≠ []); FS(roster) = None (else "roster already exists"). Join: m.members ≠ [] (else "no paper master is committed", "setup"); FS(roster) ≠ None (else "no roster at", "COPY", "will not start a second roster"). Then the parents of key, machine_pub and roster are created; then FS(key) = None (else "already exists", "never overwrite a key the roster still names"). Ok ⇒ Preflight{verb, id, paths, pins_src, master_members: m.members}. Frame: no regular file is created or modified, only directories.	provision::setup_refuses_when_a_master_is_already_committed, provision::join_refuses_a_missing_roster_instead_of_forking_a_second_one, provision::join_refuses_when_no_master_is_committed, provision::an_existing_machine_key_is_refused_before_anything_is_generated, provision::setup_refuses_to_start_over_an_existing_roster, provision::an_unrecognised_pins_file_stops_the_run_at_preflight, provision::a_run_creates_the_directories_its_key_and_roster_need	MEDIUM: fs metadata/read (effects), String::from_utf8, read_anchor callee-ensures, enum match, struct building with clones	1, 4, 8, 12
provision-verify-master	contract	provision::verify_master	∀ pre, seed: verify_master(pre, seed) = Ok ⇔ b64(Id(seed)) ∈ pre.master_members. The Err contains "is NOT the master committed" and "Nothing has been written". No effects.	provision::join_refuses_a_phrase_that_does_not_match_the_committed_anchor	EASY given master-identity-function: Vec<String>::contains, &self borrow of a non-Copy struct	1, 2, 4, 8
provision-load-roster-table	contract	provision::load_roster, provision::refuse_pending_roster_transaction	load_roster(path, mpk, now, expect) is decided in this order. path ++ ".atpkg-keys.txn" exists ⇒ Err (contains "interrupted roster transaction", "is not what is wrong"), never the verify error. Else FS(path) = None ⇒ (FS(path.sig) ≠ None ⇒ Err, half-published); (expect = MustExist ⇒ Err, contains "no roster at", "second roster"); else Ok((empty(now), None)). Else (raw, sig) = (FS(path), FS(path.sig)) and the result is Ok((parse(verify_roster([mpk], raw, sig)), Some(RosterSnapshot{raw, sig}))) when both steps succeed; a verify failure contains "does not verify under the master you typed". No writes.	provision::a_pending_roster_transaction_is_named_instead_of_a_mistyped_phrase	MEDIUM: fs metadata/read effects, cross-crate verify/parse, Option<struct> in a tuple result	1, 4, 8, 12
provision-plan-contract	contract	provision::plan	∀ pre, seed, now. pre.verb = Join ∧ verify_master(pre, seed) = Err ⇒ that Err, whatever the roster says. Otherwise, with (R, snap) = load_roster(pre.paths.roster, b64(Id(seed)), now, Join ? MustExist : MayCreateFresh)? and R' = add(R, pre.id, mpk_machine, now)?, the result is Planned with: pins_text = Some(text) ⇔ verb = Setup ∧ append_member(pre.pins_src, MASTER_ANCHOR, mpk, comment, 2) = Changed{text, ..}; master_after = that edit's members (pre.master_members for Join); roster_bytes = R'.to_toml(); roster_sig = sign(seed, roster_bytes); roster_snapshot = snap; roster_was_fresh = (snap = None); machine_pubkey ≠ mpk. Frame: no FS write. A roster that vanished after preflight is Err for Join, never a fresh mint.	provision::join_refuses_a_roster_that_vanished_between_preflight_and_plan, provision::plan_reproves_the_anchor_even_when_the_orchestrator_skips_verify_master	MEDIUM given callees: composition of five callee contracts, by-value moves, array map to String, generate as an opaque input, fs reads via load_roster	1, 2, 4, 8, 12
provision-write-pins-cas	contract	provision::write_pins	∀ planned. pins_text = Some(t): pins unreadable ⇒ Err (landed = false) ∧ FS' = FS; FS(pins) ≠ pins_src.as_bytes() ⇒ Err (contains "CHANGED ON DISK", "NOTHING HAS BEEN WRITTEN"; landed = false) ∧ FS' = FS; else the shared atomic replacement runs, and an Err from it has landed ⇔ this run successfully renamed its staged file over pins, even if a later external writer restores pins_src. After that write, or with pins_text = None: Ok ⇔ FS'(pins) reads back ∧ is UTF-8 ∧ (pins_text = Some(t) ⇒ FS'(pins) = t) ∧ verify_members(FS'(pins), MASTER_ANCHOR, master_after) = Ok; each Err has landed ⇔ pins_text = Some, and the last wraps verify_members' error.	provision::write_pins_refuses_a_file_that_changed_under_it, provision::write_pins_says_whether_a_failed_write_reached_the_anchor, provision::write_pins_reports_a_write_that_did_not_land	MEDIUM: fs read/atomic-write effects, Vec<u8> vs &[u8] compare, a closure for the error map, a let-chain	1, 4, 8
provision-write-rest-order	contract	provision::write_rest, provision::RosterLock::assert_snapshot	write_rest(planned) runs S1 lock_roster; S2 assert_snapshot(roster_snapshot); S3 write_owner_file_create_new(key, pkcs8); S4 write_bytes_atomic(machine_pub, record); S5 publish_roster_locked; the first failing step fixes the post-state. S2 fails ⇒ FS(key), FS(machine_pub) and the roster pair unchanged; message contains "CHANGED ON DISK" or "APPEARED", then " NOTHING HAS BEEN WRITTEN" (pins_text = None) or "The anchor is armed but nothing else was written: `git checkout -- " (Some). S4 fails ⇒ the key exists, machine_pub absent, the pair unchanged; message contains machine_pub, "Nothing authorizes this machine yet" and "rm ", followed by "`." (Join) or the setup start-over text (Setup). S5 fails before the redo commit ⇒ the key exists, the pair unchanged, same message shape (Setup adds "git checkout --"); after the commit ⇒ the message says KEEP the key. Ok ⇒ FS'(key) is a pkcs8 with pubkey_b64 = machine_pubkey and mode 0600, FS'(pair) = (roster_bytes, roster_sig), no redo marker. Key-before-roster: no reachable state has a published pair naming machine_pubkey while FS(key) = None.	provision::write_rest_refuses_a_roster_that_changed_since_the_plan_read_it, provision::the_key_is_written_before_the_roster_that_names_it, provision::a_record_that_cannot_be_written_fails_the_run_before_the_roster_is_published, provision::a_failure_that_authorized_nothing_undoes_only_what_its_verb_did	HARD (effects): a sequenced fs protocol, flock (FileLock), closures capturing a bool, string-suffix reasoning, moves of a large struct	1, 4, 8
provision-setup-join-e2e	refinement (composite)	provision::{preflight,plan,write_pins,write_rest}	From an unarmed pins fixture and no roster, a Setup run (id a, seed S) followed by Join runs (ids b1..bn, the same S, distinct key paths) leaves: master anchor members = [b64(Id(S))]; no machine pubkey in pins; the roster pair verifies under that anchor and parses to machines [a, b1..bn] in order with roster_seq = n + 1; each key file is 0600 with pubkey_b64 = its roster entry; reports have roster_was_fresh = true for Setup and false for each Join. Each Join keeps every machine already listed. Composition of the preflight, plan, write-pins and write-rest specs with roster-add-contract.	provision::setup_arms_the_anchor_mints_the_machine_and_signs_the_roster, provision::join_proves_the_master_and_adds_a_second_machine, paper_master_to_client::setup_then_join_produce_an_anchor_and_a_roster_the_client_accepts	FREE given its parts; otherwise whole-pipeline fs state, crypto callees, a TOML round trip	1, 2, 4, 8, 12
master-secret-noninterference	other (information flow)	provision::{plan,write_pins,write_rest}, fsio::*, main::provision	∀ seeds S1, S2 with Id(S1) = Id(S2) (same RNG draws and fs state): every byte a run writes to FS, stdout or stderr is identical under S1 and S2. Equivalently, the only S-dependent values reaching any sink are b64(Id(S)), FP(Id(S)) and sign(S, roster_bytes). Corollaries the test samples: no written file or captured stream contains the phrase, any 16-character window of it, or the 32 raw seed bytes. MasterSeed has no byte accessor, so the flow argument closes at keypair/sign; ring's internal copy is outside the claim.	provision::no_file_written_by_a_run_contains_the_phrase_or_its_seed	VERY HARD: relational (2-safety) reasoning across fs and stdout effects; private fields as the confinement boundary	1, 2, 4, 8, 16
render-report-contract	contract	provision::render_report	∀ report r: render_report(r) = ["", H, A, K, Ro, "", "=== NEXT ===", steps..]. H = "=== DONE (working tree only — a commit makes it durable) ===" if r.pins_changed, else "=== DONE ===". A = the anchor line (master_pubkey and fingerprint for Setup, fingerprint only otherwise). K = "  key     " ++ paths.key ++ .. ++ machine_pubkey ++ ")". Ro = the roster line with seq and roster_machines.join(", "), appending "  valid until " ++ v ⇔ v ≠ FOREVER, and the "the ONLY roster this master signs" clause ⇔ roster_was_fresh. Steps are numbered 1.. in order: [review git diff ⇐ pins_changed] [delete-empty-anchor test ⇐ Setup] [commit ⇐ pins_changed] [cut .. '" ++ id ++ "'] [copy roster]. Every field is drawn from Report, which holds no secret.	-	MEDIUM: Vec<String> building, a FnMut closure with a captured counter, join, usize to_string	1, 3, 7, 13
roster-redo-refines-model	refinement	provision::{lock_roster,lock_roster_read_only,RosterLock::assert_snapshot,commit_roster_pair,CommittedRoster::complete,publish_roster_locked,begin_roster_transaction,complete_roster_transaction,transaction_half_is_known,retire_roster_transaction}	Under the projection of tests/roster_redo_model.rs, every execution of these functions, interleaved with any external writes to the two canonical halves and with crash cuts between any two fs effects, maps to a behaviour of RosterPairRedo. Obligations: (a) transaction_half_is_known(c, p, t) ⇔ c = p ∨ c = Some(t). (b) complete_roster_transaction overwrites the pair only when both halves are known, then installs (body, sig) exactly, re-reads them and retires the fixed name; otherwise Err (contains "newer or unrelated") with FS unchanged and the marker kept (ForeignBytesAreNeverOverwritten). (c) lock_roster replays any marker before returning, so no RosterLock holder observes a torn pair. (d) lock_roster_read_only never creates the .lock file and never replays: an absent lock is Err "without creating it", a marker is Err "never replays". (e) assert_snapshot(Some(s)) = Ok ⇔ FS(body) = s.raw ∧ FS(sig) = s.sig; assert_snapshot(None) = Ok ⇔ both halves absent; on a stale premise commit_roster_pair commits no marker. (f) Every canonical half equal to the target is preceded by a durable marker (TargetHalfHasRedoAuthority).	provision::a_committed_roster_transaction_recovers_forward_after_a_body_only_crash, provision::redo_recovery_never_overwrites_a_newer_or_unrelated_pair, provision::check_only_lock_never_creates_or_recovers_roster_state, roster_redo_model::snapshot_cas_and_end_to_end_pair_publish_refine_the_model, roster_redo_model::every_durable_crash_cut_recovers_forward_through_real_lock_acquisition, roster_redo_model::read_only_and_writer_recovery_preserve_an_unrelated_half, roster_redo_model::stale_snapshot_and_clean_read_only_decisions_refine_the_model, roster_redo_model::the_pre_redo_torn_pair_is_the_model_s_mutant_and_nothing_can_replay_it	VERY HARD: ty_model! refinement beyond scalars, fs rename/dir effects with crash cuts, flock, Option<&[u8]> compare, a #[must_use] struct whose drop models a crash. (a) alone is EASY.	1, 4, 8, 10, 13
cli-vet-args	contract	main::vet_args, main::flag	∀ verb, argv, allowed, k: vet_args scans left to right and returns the error for the first violation, else Ok. Violations: a bare token beyond the k-th ("unexpected argument '" tok "'"); "--n=v" (the "--name=value" / "two tokens" error); "--n" with n ∈ MASTER_FLAGS (names tok, contains "COMPROMISED" and "/dev/tty", never the following value); "--n" with n ∉ allowed ("unknown flag '" tok "'" plus "--a" for each a ∈ allowed); "--n" with no next token ("needs a value"); "--n" whose next token starts with "--" ("as its value, which is another flag"). flag(argv, n) = the value after the first "--n". main runs vet_args before any flag() read for the verb, so a spelling vet_args refuses never reaches a default (no discovered pins.rs is armed).	provision_cli::a_flag_that_would_carry_the_master_is_refused_and_calls_it_compromised, provision_cli::an_unread_flag_spelling_is_refused_rather_than_silently_replaced_by_a_default, provision_cli::unknown_and_malformed_arguments_are_refused_by_name	EASY-MEDIUM once in a library: a while loop over [Option<String>; MAX_ARGS], &str get/find/starts_with, contains over a const slice, String building; lives in the binary crate (§5.13)	1, 3, 5, 7
cli-master-source-is-tty	other (provenance / information flow)	main::provision, main::confirm_transcription, master::prompt_master_attempt, master::Tty::open	In the atpkg-keys binary every MasterPhrase comes from generate_master (Setup) or from parse_master over bytes read from a File opened at "/dev/tty" that passed isatty. No env var, argv token, fd 0 or other file flows into a MasterPhrase. Setup calls Tty::open before generate_master, so with no controlling terminal Setup is Err containing "/dev/tty" and SETUP_ARMED_NOTHING, with no generate call and no fs write after preflight. Join with no tty is Err containing "/dev/tty" with FS unchanged and no key minted. Phrase bytes reach only the Tty handle, never stdout or stderr.	provision_cli::join_refuses_the_phrase_from_env_stdin_or_a_file, provision_cli::setup_without_a_terminal_refuses_and_writes_nothing	VERY HARD: information flow over process I/O (env, argv, fds), libc isatty/termios FFI, opening a device path	1, 4, 8, 16
cli-refusals-precede-prompt	contract (effect order)	main::provision, main::machine_revoke, provision::preflight	For setup, join and machine-revoke: every refusal decidable from fs state (the preflight table; machine-revoke's absent roster) is returned before Tty::open or prompt_master_attempt is reached. So the Err contains the preflight message and not "/dev/tty", and FS is unchanged. machine-revoke with an absent roster creates no lock file and no directory.	provision_cli::machine_revoke_with_no_roster_refuses_before_asking_for_the_phrase, provision_cli::join_without_a_roster_refuses_instead_of_forking_one, provision_cli::join_on_an_unarmed_tree_refuses_before_the_prompt, provision_cli::setup_refuses_on_an_armed_tree	MEDIUM: effect ordering in the binary's control flow, fs metadata, the ? chain	4, 8
cli-setup-ceremony	contract (effect order)	main::provision, main::show_phrase, main::confirm_transcription, master::Tty::write_phrase	Setup runs preflight, Tty::open, generate_master, plan, show_phrase, confirm_transcription, write_pins, write_rest, in that order. write_phrase emits exactly 13 groups of 4 chars separated by single spaces, then \n (65 bytes), only to the tty. confirm_transcription loops: a typed phrase whose seed's pubkey = want is Ok ("paper proven"); a mismatch prints "NO MATCH" and re-shows the phrase; a malformed typo re-prompts; Length{got: 0} is Err. No anchor write happens before it returns Ok, so the anchor stays unarmed through every NO MATCH. If write_pins fails after the phrase was shown, roster and key are unchanged, and the Err ends with SETUP_PAPER_ARMS_NOTHING when its landed is false (pins unchanged), else with `git checkout -- <pins>`, destroy the paper and run setup again.	provision_cli::setup_over_a_real_terminal_shows_the_phrase_there_and_never_on_stdout, provision_cli::a_mistyped_retype_reshows_the_phrase_and_a_correct_one_arms, provision_cli::a_setup_that_cannot_write_the_anchor_leaves_it_unarmed_and_says_so	HARD: a loop with tty I/O, fixed arrays with index arithmetic (i % 4), libc termios, effect ordering. The write_phrase layout alone is EASY.	1, 4, 7, 8
cli-join-ceremony	contract (effect order)	main::provision, master::prompt_master_attempt, master::TtyEcho	Join runs preflight; TtyEcho::disable (ECHO cleared before the prompt is written, the saved termios restored on Drop on every non-abort path); a read of at most PROMPT_CAP bytes up to the first \n; parse_master; verify_master; plan; write_pins; write_rest. A wrong phrase is Err "is NOT the master committed" with FS unchanged. Typed bytes are never echoed. On success the roster is extended (provision-setup-join-e2e) and the report omits the "ONLY roster" clause.	provision_cli::join_over_a_real_terminal_reads_the_phrase_and_extends_the_roster	HARD: a Drop guard around FFI tcsetattr, a bounded read loop into [u8;256], from_utf8 without allocation	1, 2, 7, 8
client-install-accepts-iff-authorized	contract (belongs to atpkg)	atpkg::install, atpkg::Anchor, aterm_update_core::roster::{verify_roster,Roster::admit,Roster::authorize_appcast,Roster::machine}	∀ registries, anchors A = Anchor::of(keys, f), now: install selects an index candidate only if the roster verifies under some key ∈ keys, roster_seq ≥ f, the index is signed by a live machine on that roster (listed, not revoked, not expired), and its machine_id/roster_seq claim binds to that machine. No candidate ⇒ Err(NoIndex) and nothing installed. keys = [] ⇒ the anchor is unarmed, the result is always Err(NoIndex), and which() = None. A revoked id leaves live(now) before any signature check. A genuine signature under a false machine_id claim is refused.	owner_to_client::one_master_and_one_roster_authorize_both_a_release_and_a_toolchain_index, owner_to_client::an_unpinned_client_installs_nothing_although_every_byte_is_genuine, owner_to_client::a_client_pinned_to_a_different_master_installs_nothing, owner_to_client::revoking_one_machine_stops_the_release_and_the_toolchain_at_once, owner_to_client::a_pre_revocation_roster_cannot_be_replayed_past_the_ratchet, owner_to_client::an_index_cannot_wear_another_machines_name	VERY HARD: trait object Fetcher, HashMap, archive extraction and fs install effects, TOML parsing, crypto FFI, cross-crate	1, 2, 4, 5, 7, 8, 11, 12
client-selects-by-generation	contract (belongs to atpkg)	atpkg::resolve_verified_index	Among the acceptable candidates, resolve_verified_index chooses one with maximal roster_seq, ties broken by index_build. A higher index_build on an older generation never beats a newer generation.	owner_to_client::a_revoked_machine_cannot_outbid_the_generation_that_revokes_it	MEDIUM in isolation: iteration over Vec<Candidate>, lexicographic max, cross-crate	1, 4, 5, 7, 12
client-floor-ratchets-on-observation	invariant (belongs to atpkg)	atpkg::install, atpkg::resolve_verified_index, atpkg::Floor	After any install or resolve call that admits a roster at sequence s, the durable floor ≥ s, whether or not anything installed. Later candidates with roster_seq below the floor are never selected. When the served set selects nothing, the store falls back to its cached last-trusted index.	owner_to_client::observing_a_revocation_is_durable_even_when_nothing_installs	HARD: fs-backed floor state, a two-state property across calls, cross-crate	1, 2, 4, 8, 12
```

### Distribution by kind (43)

| kind | count | ids |
|---|---|---|
| contract | 29 | master-seed-decode, master-parse-contract, phrase-error-message, fingerprint-format, scrub-zeroes, secret-debug-redaction, keygen-pubkey-consistency, roster-add-contract, roster-revoke-contract, vet-key-contract, anchor-read-contract, anchor-append-contract, verify-members-contract, fsio-atomic-replace, fsio-owner-create-new, fsio-ensure-parent-dir, provision-preflight-table, provision-verify-master, provision-load-roster-table, provision-plan-contract, provision-write-pins-cas, provision-write-rest-order, render-report-contract, cli-vet-args, cli-refusals-precede-prompt, cli-setup-ceremony, cli-join-ceremony, client-install-accepts-iff-authorized, client-selects-by-generation |
| round-trip | 3 | master-base32-roundtrip, sign-verify-roundtrip, owner-client-roundtrip (composite) |
| equivalence | 2 | master-identity-function, verify-signed-is-roster-chain |
| invariant | 2 | roster-edits-preserve-validity, client-floor-ratchets-on-observation |
| table-property | 2 | forever-admits, shipped-pins-readable |
| refinement | 2 | roster-redo-refines-model, provision-setup-join-e2e (composite) |
| no-panic/bounds | 1 | leading-ws-no-panic |
| other | 2 | master-secret-noninterference (information flow), cli-master-source-is-tty (provenance) |

### Blocking-gap histogram (43 specs)

| gap | specs blocked |
|---|---|
| 0 (seal) | 43 (every spec) |
| 1 non-scalar carriers | 40 |
| 4 callee-ensures composition | 26 |
| 8 effects | 25 |
| 7 loops | 23 |
| 12 cross-crate citation | 17 |
| 3 E6 body widening | 15 |
| 5 quantified clauses | 14 |
| 2 `&self`/`&mut self` views | 11 |
| 13 threads/closures | 4 |
| 11 generics | 2 |
| 16 soundness (relational) | 2 |
| 10 model refinement | 1 |

Only `master-seed-decode` (gaps 3, 5, 7), `master-base32-roundtrip` (3, 5, 7, 8) and
`cli-refusals-precede-prompt` (4, 8) are free of gap 1. No spec is blocked by gap 0 alone.

### Proof dependency order

`master-seed-decode`, `master-base32-roundtrip` (pure core), `scrub-zeroes`, `phrase-error-message`,
`vet-key-contract`, `fingerprint-format` → `master-parse-contract` → `master-identity-function`
(assumed crypto) → `keygen-pubkey-consistency`, `sign-verify-roundtrip`, `verify-signed-is-roster-chain`
→ `roster-add-contract`, `roster-revoke-contract` → `roster-edits-preserve-validity`, `forever-admits`;
`leading-ws-no-panic` → `anchor-read-contract` → `verify-members-contract` → `anchor-append-contract`
→ `shipped-pins-readable`; `fsio-*` (effects) → `provision-preflight-table`, `provision-verify-master`,
`provision-load-roster-table` → `provision-plan-contract` → `provision-write-pins-cas`,
`roster-redo-refines-model` → `provision-write-rest-order` → free composites (`provision-setup-join-e2e`,
`owner-client-roundtrip`) → `render-report-contract`, `cli-*` → `master-secret-noninterference`.

## 2. Test → spec map (TSV)

`primary` is the spec that retires the test. `also` lists the other specs the same test samples.

```tsv
file	test	primary	also
src/lib.rs	verify_signed_accepts_a_rostered_signature_and_nothing_else	verify-signed-is-roster-chain	sign-verify-roundtrip
src/lib.rs	produces_signatures_the_client_verifier_accepts	sign-verify-roundtrip	keygen-pubkey-consistency
src/lib.rs	pubkey_b64_recovers_the_published_key	keygen-pubkey-consistency	
src/lib.rs	invalid_key_fails_closed	keygen-pubkey-consistency	
src/fsio.rs	an_atomic_write_replaces_the_file_and_leaves_nothing_behind	fsio-atomic-replace	
src/fsio.rs	a_failed_atomic_write_leaves_the_original_intact	fsio-atomic-replace	
src/fsio.rs	a_crash_left_temp_never_wedges_the_next_atomic_write	fsio-atomic-replace	(covers only the legacy-name case, §5.10)
src/fsio.rs	an_owner_file_is_complete_private_and_never_replaced	fsio-owner-create-new	
src/fsio.rs	ensure_parent_dir_creates_a_missing_chain_and_is_idempotent	fsio-ensure-parent-dir	
src/master.rs	exactly_fifty_two_characters_is_accepted_and_case_folds	master-parse-contract	
src/master.rs	o_i_and_l_are_read_as_the_characters_they_resemble	master-parse-contract	
src/master.rs	fifty_one_and_fifty_three_characters_are_refused_with_the_count	master-parse-contract	phrase-error-message
src/master.rs	a_bad_character_is_refused_by_position_and_never_echoed	master-parse-contract	phrase-error-message
src/master.rs	a_bad_character_is_reported_even_when_the_length_is_also_wrong	master-parse-contract	
src/master.rs	separators_are_stripped_and_positions_count_significant_characters	master-parse-contract	
src/master.rs	generate_produces_a_fresh_fifty_two_character_phrase	master-base32-roundtrip	(its freshness assert samples the RNG, §5.2)
src/master.rs	the_padding_bits_of_the_last_character_are_masked	master-seed-decode	master-identity-function
src/master.rs	the_fingerprint_matches_on_a_correct_transcription_and_differs_otherwise	master-identity-function	fingerprint-format
src/master.rs	the_fingerprint_is_taken_over_the_public_key	master-identity-function	fingerprint-format (negative control is wrong, §5.1)
src/master.rs	debug_never_prints_secret_material	secret-debug-redaction	
src/master.rs	scrub_zeroes_the_buffer	scrub-zeroes	
src/pins_edit.rs	both_declaration_shapes_are_read_and_the_head_is_index_zero	anchor-read-contract	
src/pins_edit.rs	appending_the_same_key_twice_produces_one_entry	anchor-append-contract	
src/pins_edit.rs	the_existing_keys_survive_and_the_head_stays_at_index_zero	anchor-append-contract	
src/pins_edit.rs	nothing_outside_the_edited_block_is_disturbed	anchor-append-contract	
src/pins_edit.rs	the_empty_form_opens_into_a_one_member_list_without_touching_its_doc	anchor-append-contract	anchor-read-contract
src/pins_edit.rs	trailing_whitespace_on_a_neighbouring_line_does_not_poison_the_indent	anchor-append-contract	leading-ws-no-panic
src/pins_edit.rs	a_multibyte_comment_with_trailing_whitespace_does_not_panic	leading-ws-no-panic	anchor-read-contract
src/pins_edit.rs	a_crlf_file_keeps_its_line_endings	anchor-append-contract	
src/pins_edit.rs	unrecognised_shapes_are_refused_not_guessed_at	anchor-read-contract	
src/pins_edit.rs	an_anchor_holding_an_empty_member_is_refused	anchor-append-contract	
src/pins_edit.rs	a_malformed_key_is_refused_before_any_edit_is_planned	vet-key-contract	
src/pins_edit.rs	the_keyset_ceiling_is_enforced_by_the_writer	anchor-append-contract	
src/pins_edit.rs	verification_catches_a_write_that_did_not_land	verify-members-contract	
src/pins_edit.rs	the_shipped_pins_file_is_a_shape_this_writer_understands	shipped-pins-readable	
src/provision.rs	setup_arms_the_anchor_mints_the_machine_and_signs_the_roster	provision-setup-join-e2e	
src/provision.rs	setup_refuses_when_a_master_is_already_committed	provision-preflight-table	
src/provision.rs	join_proves_the_master_and_adds_a_second_machine	provision-setup-join-e2e	
src/provision.rs	join_refuses_a_missing_roster_instead_of_forking_a_second_one	provision-preflight-table	
src/provision.rs	join_refuses_a_roster_that_vanished_between_preflight_and_plan	provision-plan-contract	provision-load-roster-table
src/provision.rs	write_rest_refuses_a_roster_that_changed_since_the_plan_read_it	provision-write-rest-order	roster-redo-refines-model
src/provision.rs	a_broken_signature_target_leaves_the_roster_document_unchanged	NON-SPEC (becomes-spec-after-fix)	roster-redo-refines-model
src/provision.rs	a_committed_roster_transaction_recovers_forward_after_a_body_only_crash	roster-redo-refines-model	
src/provision.rs	redo_recovery_never_overwrites_a_newer_or_unrelated_pair	roster-redo-refines-model	
src/provision.rs	check_only_lock_never_creates_or_recovers_roster_state	roster-redo-refines-model	
src/provision.rs	a_pending_roster_transaction_is_named_instead_of_a_mistyped_phrase	provision-load-roster-table	
src/provision.rs	plan_reproves_the_anchor_even_when_the_orchestrator_skips_verify_master	provision-plan-contract	
src/provision.rs	a_run_creates_the_directories_its_key_and_roster_need	provision-preflight-table	provision-setup-join-e2e
src/provision.rs	the_key_is_written_before_the_roster_that_names_it	provision-write-rest-order	
src/provision.rs	a_record_that_cannot_be_written_fails_the_run_before_the_roster_is_published	provision-write-rest-order	
src/provision.rs	a_failure_that_authorized_nothing_undoes_only_what_its_verb_did	provision-write-rest-order	
src/provision.rs	write_pins_refuses_a_file_that_changed_under_it	provision-write-pins-cas	
src/provision.rs	write_pins_says_whether_a_failed_write_reached_the_anchor	provision-write-pins-cas	
src/provision.rs	join_refuses_a_phrase_that_does_not_match_the_committed_anchor	provision-verify-master	provision-load-roster-table, provision-plan-contract
src/provision.rs	join_refuses_when_no_master_is_committed	provision-preflight-table	
src/provision.rs	a_second_pass_over_an_already_listed_key_adds_no_second_entry	anchor-append-contract	
src/provision.rs	an_existing_machine_key_is_refused_before_anything_is_generated	provision-preflight-table	(mislabelled, §5.4)
src/provision.rs	setup_refuses_to_start_over_an_existing_roster	provision-preflight-table	
src/provision.rs	an_unrecognised_pins_file_stops_the_run_at_preflight	provision-preflight-table	anchor-read-contract
src/provision.rs	write_pins_reports_a_write_that_did_not_land	provision-write-pins-cas	verify-members-contract
src/provision.rs	debug_renderings_carry_no_secret_material	secret-debug-redaction	
src/provision.rs	no_file_written_by_a_run_contains_the_phrase_or_its_seed	master-secret-noninterference	
src/roster_ops.rs	adding_a_machine_lists_it_and_advances_the_sequence	roster-add-contract	roster-edits-preserve-validity
src/roster_ops.rs	reusing_a_machine_id_is_refused_rather_than_overwriting_the_key	roster-add-contract	
src/roster_ops.rs	reusing_a_key_under_a_second_id_is_refused_because_revocation_names_ids	roster-add-contract	roster-edits-preserve-validity
src/roster_ops.rs	revoking_removes_the_machine_and_names_it_in_the_deny_list	roster-revoke-contract	roster-edits-preserve-validity
src/roster_ops.rs	a_revoked_id_can_never_be_re_added	roster-add-contract	roster-revoke-contract
src/roster_ops.rs	revoking_an_unknown_id_is_an_error_not_a_silent_no_op	roster-revoke-contract	
src/roster_ops.rs	the_roster_has_a_hard_ceiling	roster-add-contract	
src/roster_ops.rs	every_edit_stamps_the_forever_window	roster-add-contract	forever-admits
tests/owner_to_client.rs	one_master_and_one_roster_authorize_both_a_release_and_a_toolchain_index	client-install-accepts-iff-authorized	
tests/owner_to_client.rs	an_unpinned_client_installs_nothing_although_every_byte_is_genuine	client-install-accepts-iff-authorized	(last two asserts pin repo state, §5.7)
tests/owner_to_client.rs	a_client_pinned_to_a_different_master_installs_nothing	client-install-accepts-iff-authorized	
tests/owner_to_client.rs	revoking_one_machine_stops_the_release_and_the_toolchain_at_once	client-install-accepts-iff-authorized	roster-revoke-contract
tests/owner_to_client.rs	a_pre_revocation_roster_cannot_be_replayed_past_the_ratchet	client-install-accepts-iff-authorized	client-floor-ratchets-on-observation
tests/owner_to_client.rs	an_index_cannot_wear_another_machines_name	client-install-accepts-iff-authorized	
tests/owner_to_client.rs	a_revoked_machine_cannot_outbid_the_generation_that_revokes_it	client-selects-by-generation	
tests/owner_to_client.rs	observing_a_revocation_is_durable_even_when_nothing_installs	client-floor-ratchets-on-observation	
tests/paper_master_to_client.rs	a_master_phrase_mints_a_machine_whose_release_the_client_accepts	owner-client-roundtrip	
tests/paper_master_to_client.rs	setup_then_join_produce_an_anchor_and_a_roster_the_client_accepts	provision-setup-join-e2e	
tests/provision_cli.rs	join_refuses_the_phrase_from_env_stdin_or_a_file	cli-master-source-is-tty	
tests/provision_cli.rs	a_flag_that_would_carry_the_master_is_refused_and_calls_it_compromised	cli-vet-args	
tests/provision_cli.rs	setup_without_a_terminal_refuses_and_writes_nothing	cli-master-source-is-tty	
tests/provision_cli.rs	an_unread_flag_spelling_is_refused_rather_than_silently_replaced_by_a_default	cli-vet-args	
tests/provision_cli.rs	unknown_and_malformed_arguments_are_refused_by_name	cli-vet-args	
tests/provision_cli.rs	machine_revoke_with_no_roster_refuses_before_asking_for_the_phrase	cli-refusals-precede-prompt	
tests/provision_cli.rs	join_without_a_roster_refuses_instead_of_forking_one	cli-refusals-precede-prompt	provision-preflight-table
tests/provision_cli.rs	setup_over_a_real_terminal_shows_the_phrase_there_and_never_on_stdout	cli-setup-ceremony	cli-master-source-is-tty
tests/provision_cli.rs	a_mistyped_retype_reshows_the_phrase_and_a_correct_one_arms	cli-setup-ceremony	
tests/provision_cli.rs	join_over_a_real_terminal_reads_the_phrase_and_extends_the_roster	cli-join-ceremony	provision-setup-join-e2e
tests/provision_cli.rs	a_setup_that_cannot_write_the_anchor_leaves_it_unarmed_and_says_so	cli-setup-ceremony	provision-write-pins-cas
tests/provision_cli.rs	join_on_an_unarmed_tree_refuses_before_the_prompt	cli-refusals-precede-prompt	provision-preflight-table
tests/provision_cli.rs	setup_refuses_on_an_armed_tree	cli-refusals-precede-prompt	provision-preflight-table
tests/roster_redo_model.rs	snapshot_cas_and_end_to_end_pair_publish_refine_the_model	roster-redo-refines-model	
tests/roster_redo_model.rs	every_durable_crash_cut_recovers_forward_through_real_lock_acquisition	roster-redo-refines-model	
tests/roster_redo_model.rs	read_only_and_writer_recovery_preserve_an_unrelated_half	roster-redo-refines-model	
tests/roster_redo_model.rs	stale_snapshot_and_clean_read_only_decisions_refine_the_model	roster-redo-refines-model	
tests/roster_redo_model.rs	the_pre_redo_torn_pair_is_the_model_s_mutant_and_nothing_can_replay_it	roster-redo-refines-model	
```

(Paths in the map are relative to `crates/atpkg-keys`.)

## 3. Coverage

- 100 tests read. 98 map to one of the 43 specs. 2 are not spec samples (§4).
- The biggest collapses: `anchor-append-contract` (9 primary), `roster-redo-refines-model` (8),
  `provision-preflight-table` (7), `master-parse-contract`, `roster-add-contract` and
  `client-install-accepts-iff-authorized` (6 each).
- Four specs have no primary test, only secondary ones: `phrase-error-message`, `fingerprint-format`,
  `roster-edits-preserve-validity`, `forever-admits`. They are the general statements behind
  assertions inside other tests; proving them is what lets those assertions go.
- Two specs are free composites once their parts are proved: `provision-setup-join-e2e` (3 tests) and
  `owner-client-roundtrip` (1).
- 9 tests (`client-*`) sample `atpkg`/`aterm-update-core` properties; they retire when those crates'
  specs are proved, and atpkg-keys keeps only the fixtures.

## 4. Tests that are not spec samples

| test | why | action |
|---|---|---|
| provision::a_broken_signature_target_leaves_the_roster_document_unchanged | The refusal it asserts ("missing one half or unreadable") comes from the `#[cfg(test)]`-only `provision::publish_roster` wrapper, which has no production caller. Its MUTATION note refers to that test-only path. The production equivalent is `assert_snapshot` refusing an unreadable half (`roster-redo-refines-model` (e), via `read_bytes` rejecting a directory). | **becomes-spec-after-fix**: retarget it at `write_rest`/`commit_roster_pair`, fold it into `roster-redo-refines-model`, and delete the test-only `publish_roster`. |

Action counts: becomes-spec-after-fix 1.

## 5. Implied specs that look wrong, or disagree with the code

1. **`master::the_fingerprint_is_taken_over_the_public_key` tests the wrong negative control.** Its comment
   says "the SEED's own bytes produce a different value", but it fingerprints the constant `[0x01;32]`,
   which is not SYNTHETIC's seed (`B32dec(SYNTHETIC)` is a different array). Either hash `B32dec(SYNTHETIC)`
   or drop the claim. Under `master-identity-function`, `fingerprint(s) = FP(Id(s))` is a definitional
   equality.
2. **`master::generate_produces_a_fresh_fifty_two_character_phrase` asserts `a ≠ b`.** That samples the
   CSPRNG, not code. It cannot be proved and fails with probability 2⁻²⁵⁶. Drop that assertion when
   `master-base32-roundtrip` lands.
3. **`provision::write_pins_reports_a_write_that_did_not_land` accepts a disjunction.** It accepts "holds 0
   keys but 1" or "holds 1 keys but 2". `planned2` is planned against the restored unarmed fixture, so
   `master_after = [master]` and only the first branch can occur; the disjunction hides which state is
   tested. The first `write_pins(&planned)` plus restore is dead setup.
4. **`provision::an_existing_machine_key_is_refused_before_anything_is_generated` is mislabelled.** Its doc
   says "a re-run of `setup` with the SAME id", but it calls `preflight(Verb::Join, "m3", ..)`. It reaches
   the key-exists row only because the anchor is armed, so Join passes the verb checks.
5. **`provision::a_broken_signature_target_leaves_the_roster_document_unchanged` describes a mechanism it
   never exercises.** The doc says a directory on the sig path lets "its staging sibling write fine and the
   rename fail". The test-only `publish_roster` refuses at its premise read before any staging (§4).
6. **`owner_to_client::one_master_and_one_roster_authorize_both_a_release_and_a_toolchain_index` does not
   assert the equality it names.** The final block says "ONE ROOT, stated as an equality rather than a
   story" but asserts only `anchor.is_armed()`, true by construction from `Anchor::of(vec![master_pub])`.
   The intended equality (the atpkg anchor key = the release roster key) is not asserted.
7. **`owner_to_client::an_unpinned_client_installs_nothing_although_every_byte_is_genuine` pins repo
   state.** Its last two asserts (`!PKG_TRUST_ANCHORS.is_empty()`, `manager_enabled_with(..)`) check the
   repository's current shipped pins, not a function property. They flip whenever the anchor is
   legitimately emptied and belong in atpkg's own pins check.
8. **`paper_master_to_client.rs` prose is stale.** The PAPER doc says "Sixty-four characters of a visible
   repeating pattern"; the constant is 52 characters (`MASTER_PHRASE_LEN`).
9. **`revoke` accepts a re-revocation.** `roster_ops::revoking_an_unknown_id_is_an_error_not_a_silent_no_op`
   implies any no-op revocation is refused. `revoke()` actually accepts an id that is already revoked and
   not listed, leaves `machines` and `revoked` unchanged, and bumps `roster_seq`: a content no-op that is
   signed and published. Decide: refuse re-revocation, or document the seq-only bump.
   `roster-revoke-contract` states the code as written.
10. **`fsio::a_crash_left_temp_never_wedges_the_next_atomic_write` kills only one mutation.** The planted
    litter uses the legacy fixed name `.atpkg-keys.tmp`, which the pid+seq scheme can never produce. The
    retry-on-`AlreadyExists` loop in `stage_sibling_temp_with_mode`, the real collision case, is untested.
    `fsio-atomic-replace` covers it as a quantified obligation.
11. **Mode-0600 assertions assume a umask.** `fsio::an_owner_file_is_complete_private_and_never_replaced`
    and the provision mode checks assume a umask that keeps the owner rw bits; `mode(0o600)` under umask
    `0o200` gives `0400`. `fsio-owner-create-new` carries that as an environment precondition.
12. **The `client-*` specs live in the wrong crate.** `owner_to_client.rs` tests `atpkg::install`,
    `atpkg::resolve_verified_index` and `aterm_update_core::roster`. Move these three specs to
    `crates/atpkg/SPECS.md` (and `crates/aterm-update-core/SPECS.md` for the roster parts); atpkg-keys
    supplies only the fixtures.
13. **The CLI specs are on binary-only code.** `vet_args`, `flag`, `confirm_transcription` and the verb
    orchestration live in `src/main.rs`. Factor the pure parts (`vet_args`, `flag`, `write_phrase`'s
    layout) into the library so they can carry Trust clauses; the effect-order specs need the binary
    verified as a crate.

## 6. The specs I would prove first (easiest, high value)

No spec here is blocked by gap 0 alone: 40 of 43 need gap 1 (non-scalar carriers). Gap 0 itself is
closed locally by seal `150efe4a` (PLAN §"Seal 150efe4a"), but that seal is not admitted to the
machine's `trust` channel. The order below starts with the specs whose gaps are smallest and
whose code is closest to scalar.

1. **`master-seed-decode`**, after factoring the decode loop into a pure
   `b32_decode(&[u8;52]) -> [u8;32]`. With spec 2 it is one of the two gap-1-free specs over pure code: fixed arrays,
   shifts, a range `match` on `u8` (already admitted for E6 import by seal `150efe4a`), and one loop with
   an obvious invariant (`out = ⌊5j/8⌋`). Gaps 3, 5 and 7 are needed. It retires the padding test and
   underlies every identity claim.
2. **`master-base32-roundtrip`**, with the RNG draw factored out as an argument
   (`b32_encode(&[u8;32]) -> [u8;52]`). This removes gap 8 from the pure core, leaving 3, 5 and 7. It proves
   `B32dec ∘ B32enc = id`, so with spec 1 the whole phrase codec is closed.
3. **`vet-key-contract`** and **`phrase-error-message`**. Small `&str` predicates and message builders
   (gaps 1 and 3, plus 5 and 7 for the byte scan). They are the cheapest gap-1 probes in this crate and
   retire a test and two secondary asserts. `master-parse-contract` (6 tests) follows directly once gap 1
   covers `&str` input and a fixed output array.

Runner-up: `roster-add-contract` / `roster-revoke-contract` (8 tests together, pure `Vec<Machine>`
logic), then `anchor-read-contract` → `anchor-append-contract` (12 tests together, pure string code).
Then the effectful specs (`fsio-*`, `provision-*`, `roster-redo-refines-model`, `cli-*`), which wait on
gap 8. Their pure cores (`transaction_half_is_known`, the preflight decision table over an abstract FS,
`vet_args`) can be factored out and proved earlier. Last: `master-secret-noninterference` and
`cli-master-source-is-tty`, which need relational information-flow reasoning (gap 16).
