<!-- Copyright 2026 Andrew Yates -->
<!-- SPDX-License-Identifier: Apache-2.0 -->

# aterm-secrets: specs behind the credentials vault's tests

> Status: spec recovery (docs/PLAN-tests-to-specs-2026-10-15.md, step 2) for `aterm secrets`
> (design `docs/DESIGN-secrets-2026-10-20.md`). Each spec below is held by the tests named in its
> `subsumes` column; none is proved through Trust/Clean yet. The crate is on the Trust lane with a
> floor in `crates/aterm-release/trust/floors.tsv`.

Source: `crates/aterm-secrets/src/` and the two integration tests. The codecs (`b64`, `bech32`,
`x25519`, `age`), the signed envelope (`envelope`), the vault text (`text`) and the join proof (`join`)
are pure apart from randomness; `transport`, `fsx`, `materialize`, `server`, `client`, `service` and
`cli` touch files, sockets and processes.

## 0. Notation used in the statements

- File keys: `L` = `src/lib.rs`, `B64` = `src/b64.rs`, `BE` = `src/bech32.rs`, `X` = `src/x25519.rs`,
  `A` = `src/age.rs`, `SB` = `src/secret.rs`, `JN` = `src/join.rs`, `EN` = `src/envelope.rs`,
  `T` = `src/text.rs`, `TR` = `src/transport.rs`, `FS` = `src/fsx.rs`, `M` = `src/materialize.rs`,
  `SV` = `src/server.rs`, `CL` = `src/client.rs`, `SE` = `src/service.rs`, `CLI` = `src/cli.rs`,
  `F` = `tests/flows.rs`, `CC` = `tests/cctv.rs`.
- `p` = 2^255 − 19. The low-order u-coordinates are 0, 1, p − 1, the two points of order 8, and p and
  p + 1 (0 and 1 unreduced), each also with bit 255 set: 14 spellings.
- A version is an envelope; its seq, digest (SHA-256 of its bytes), signer and handoff chain (the
  `previous-signer`/`endorsement` pairs, oldest first) are the envelope's.
- "The vault wrote f" = the manifest names f with the digest of what is in it now.

## 1. Specs (TSV)

Columns: `id`, `kind`, `functions`, `statement`, `subsumes`, `difficulty` (the Rust features a proof must
handle), `blocking_gaps` (rows of `docs/AUDIT-trust-for-test-migration-2026-10-15.md` §7).

```tsv
id	kind	functions	statement	subsumes	difficulty	blocking_gaps
ALAB-PINS	table-property	ALAB_SIGNER, ALAB_RETIRED, envelope::Retired::compiled	ALAB_SIGNER is canonical padded base64 of a 32-byte key whose fingerprint is fed4-97da-a265-0615-9f56; every ALAB_RETIRED entry is canonical padded base64 of 32 bytes and none is ALAB_SIGNER; Retired::compiled() holds every retired key and current = ALAB_SIGNER.	L::the_alab_signer_is_the_ed25519_key_of_the_vault_m22_made; L::every_retired_alab_key_is_a_key_and_none_is_the_signer	const strings, base64, SHA-256	1,12
ALAB-NAME	table-property	alab_advertised, ALAB_DEFAULT_URL, DEFAULT_PORT	alab_advertised() is the label of ALAB_DEFAULT_URL's `<label>.local` host, `alab-secrets`, a valid vault name, and that URL's port is DEFAULT_PORT: a server with no flags answers the URL every new Mac's setup joins.	L::the_server_advertises_the_name_and_port_the_default_url_reaches	&str splitting	1
B64-CANON	round-trip	b64::encode_padded, decode_padded, encode_nopad, decode_nopad	∀ bytes b: decode_padded(encode_padded(b)) = Some(b) and decode_nopad(encode_nopad(b)) = Some(b); encode_padded is RFC 4648 §4 (its §10 vectors) and encode_nopad that text without `=`. Each decoder accepts only canonical text: decode_X(t) = Some(b) ⇒ t = encode_X(b), so padding in the unpadded form, missing padding in the padded one, a length ≡ 1 mod 4, non-zero trailing bits, whitespace and a newline are refused.	B64::rfc4648_vectors_round_trip_in_both_spellings; B64::unpadded_refuses_padding_bad_length_and_trailing_bits; B64::padded_refuses_missing_padding_and_trailing_bits	&str/Vec<u8>, loops	1,5,7
BECH-CHECKSUM	contract	bech32::decode_groups, encode_groups, decode	decode_groups accepts exactly BIP 173's valid Bech32 strings (checksum constant 1) without its 90-character limit: one case throughout, an HRP and a data part from the charset, a checksum over the lowercased text that every character affects. encode_groups is lowercase; decode_groups(encode_groups(h, g)) = (h, g, false), and the uppercased text decodes the same with uppercase = true. BIP 173's invalid strings, mixed case and characters outside the charset are refused by decode_groups and decode.	BE::bip173_valid_strings_decode_and_re_encode; BE::bip173_invalid_strings_are_refused; BE::no_length_limit; BE::mixed_case_is_refused; BE::characters_outside_the_charset_are_refused	bit arithmetic over Vec<u8>, loops	1,3,5,7
BECH-REGROUP	round-trip	bech32::to_groups, from_groups, encode, decode	from_groups(to_groups(b)) = Some(b) for all b; from_groups refuses a padding of 5 or more bits and non-zero padding bits; decode(encode(h, d)) = (h, d, lowercase) for every HRP and length (0 to 200 bytes tested), and the uppercase text decodes to the same with uppercase set; BIP 173's segwit programs regroup to their scriptPubKey exactly.	BE::bip173_valid_strings_decode_to_bytes; BE::bip173_segwit_programs_regroup_exactly; BE::padding_must_be_short_and_zero; BE::age_length_round_trips	bit accumulator loops	1,3,5,7
X-RFC7748	equivalence	x25519::x25519, x25519_base	∀ 32-byte k, u: x25519(k, u) = RFC 7748 §5's X25519: k clamped, u's bit 255 masked, u reduced mod p (so u + p for u < 19 equals u), the scalar multiple of u on Curve25519; x25519_base(k) = x25519(k, 9). Samples: §5.2's single evaluations and its iterations (1, 1,000; 1,000,000 ignored by default), §6.1's Diffie–Hellman, and agreement with ring's X25519 on random keys in both directions. The C it transcribes has a Coq proof of exactly this (ePrint 2021/428); the Rust has none.	X::rfc7748_5_2_single_evaluations; X::rfc7748_5_2_iterated_once_and_a_thousand_times; X::rfc7748_5_2_iterated_a_million_times; X::rfc7748_6_1_diffie_hellman; X::non_canonical_u_is_reduced; X::agrees_with_ring_on_random_keys	i64 limb arithmetic, 255-step ladder, masked swaps	1,3,5,7
X-LOW-ORDER	contract	x25519::x25519	∀ k and each of the 14 low-order spellings u: x25519(k, u) = 0³² (what age's all-zero refusal relies on).	X::low_order_inputs_give_all_zero_output	as X-RFC7748	1,3,5,7
AGE-ROUNDTRIP	round-trip	age::encrypt, decrypt, armor	∀ non-empty recipient lists R with no low-order key, plaintext p ≤ MAX_FILE and identity i whose recipient is in R: decrypt([i], encrypt(R, p)) = p and decrypt([i], armor(encrypt(R, p))) = p (sizes around every chunk boundary, 1, 2 and 5 recipients); generated identities differ and each opens only files made to it.	A::round_trips_every_size_to_one_two_and_five_recipients; A::generated_identities_differ_and_open_their_files	ChaCha20-Poly1305, HKDF, X25519, Vec	1,7,8,12
AGE-FORMAT	contract	age::encrypt	encrypt writes age v1: a header with one `X25519` stanza per recipient (one 43-character share argument, a 32-byte body), lines of at most 64 columns, and a MAC; each call draws a fresh ephemeral share and file key (two encryptions share no stanza argument and no MAC); the payload is STREAM in 64 KiB chunks, the last flagged final, with the chunk count age.md gives for each size.	A::chunking_matches_the_spec; A::header_is_well_formed	header text, chunk loop	1,7,8
AGE-NOMATCH	contract	age::decrypt	decrypt with no identity, or with identities none of which opens a stanza, is NoMatch; identities are tried in order and the first that opens a stanza decrypts.	A::wrong_identity_is_no_match_and_no_identity_too	slice of identities, loops	1,7
AGE-ARMOR	contract	age::armor, dearmor	armor(b) = `-----BEGIN AGE ENCRYPTED FILE-----\n`, padded base64 of b in 64-column lines, `-----END AGE ENCRYPTED FILE-----\n`; dearmor(armor(b)) = b, also with CRLF line ends and the whitespace the reference reader allows around it; empty input, more whitespace than that, and text sharing a marker's line are Armor.	A::armor_round_trips; A::armor_whitespace_bounds	&[u8] scanning	1,5,7
AGE-KEYS	round-trip	age::Identity::parse, to_bech32, Recipient::parse, to_bech32	An identity is `AGE-SECRET-KEY-1…` Bech32 in uppercase of 32 bytes, a recipient `age1…` in lowercase, 62 characters, of 32 bytes; parse ∘ to_bech32 is the identity on both; lowercase identities, a wrong HRP (an identity as a recipient and the reverse), a wrong length and the empty string are Key; age.md's example identity (32 × 0x42) spells as the spec shows; Identity's Debug shows only its recipient.	A::identity_and_recipient_spellings; A::spec_example_identity_spelling	Bech32, String	1,5
AGE-LOW-ORDER	contract	age::Recipient::parse, encrypt, decrypt	A recipient whose key is any of the 14 low-order spellings is refused by Recipient::parse (Key) and by encrypt, alone or beside a good one; encrypt refuses an empty list; real keys and small coordinates of large order still parse. A stanza whose share gives the all-zero shared secret is Header, before the MAC is looked at, for its recipient and anyone else.	A::encrypt_refuses_no_recipient_and_a_low_order_one; A::a_low_order_recipient_is_refused_by_parse_and_by_encrypt; A::a_stanza_whose_share_is_low_order_is_a_header_failure	X25519, comparisons over [u8; 32]	1,3,7
AGE-HEADER	contract	age::decrypt	An `scrypt` stanza beside any other stanza is Header; a stanza of another type is skipped (so a header changed by one fails its MAC: Hmac); a header with no stanza, or one past MAX_HEADER, is Header.	A::scrypt_beside_another_stanza_is_a_header_failure; A::a_header_without_stanzas_or_past_the_bound_is_refused	header parser	1,7
AGE-NO-PARTIAL	invariant	age::decrypt	No plaintext is released unless every chunk authenticated, and no input panics: every truncation and every bit flip of the header, the nonce and a stride of the payload, of a binary and an armored file, is an error; only a flip of armor whitespace into whitespace still opens; bytes before an armored file are Armor and bytes after a binary one Payload.	A::truncations_and_bit_flips_only_fail	whole decoder	1,7,8
AGE-CCTV	table-property	age::decrypt, armor	Every applicable C2SP CCTV vector fails with exactly its class (armor, header, no match, HMAC, payload) or decrypts to its plaintext hash; the subset kept covers every class; every non-armored success vector round-trips through this armor.	CC::every_applicable_cctv_vector_has_its_exact_class; CC::the_subset_covers_every_class; CC::success_vectors_round_trip_through_our_armor	fixtures, zlib	1,7,8
AGE-ERRORS	invariant	age::AgeError (Display)	Every AgeError's text begins `age: ` and names its class, never a value.	A::errors_name_no_value	format!	1
SEC-REDACT	invariant	SecretBytes (Debug), join::Key (Debug)	A SecretBytes prints its length (`7 bytes`) and never its bytes; a join key prints `join::Key(<redacted>)`.	SB::debug_never_shows_the_bytes; JN::debug_never_shows_the_key	fmt::Debug	1
SEC-WIPE-READ	contract	secret::read_to_end_wiping_on_error	Ok(n) appends the reader's n bytes to the buffer; an error after any bytes leaves the buffer empty (wiped), whatever it held before; an empty successful read leaves it unchanged.	SB::a_partial_read_error_wipes_the_accumulated_bytes	io::Read, Vec	1,8
JOIN-PROOF	contract	join::Key::derive, prove, verify	verify(derive(v, w), v', r', h', prove(v, r, h)) holds exactly when (v', r', h') = (v, r, h) and the password is w up to surrounding blanks (derive trims them); a proof that is empty or not base64, or a v', r' or h' holding a LF (the message's separator), never verifies.	JN::a_proof_binds_the_vault_the_key_and_the_host; JN::blanks_around_a_password_do_not_count; JN::a_line_break_inside_a_field_never_moves_a_proof_to_another_request	PBKDF2/HMAC (ring), &str	1,8,12
JOIN-STRENGTH	contract	join::generate, strength	generate() gives 23-character passwords in four `-`-joined groups that strength accepts, distinct across calls; strength refuses fewer than MIN_LEN (12) characters after trimming, more than MAX_LEN (256), a line break and the empty password, and its reason never quotes the password.	JN::generated_passwords_are_strong_distinct_and_grouped; JN::weak_passwords_are_refused_without_echoing_them	&[u8] predicates, randomness	1,8
ENV-GRAMMAR	contract	envelope::sign, parse	parse(sign(u, seed)) holds exactly the bytes the module doc spells and gives u back; the grammar has one spelling of every envelope (each header once and in order, one space after `:`, decimals without leading zeros, keys and signatures in canonical padded base64 of 32 and 64 bytes, LF only, locker labels `admin` or a decimal from 1 each at most once, armored blocks between the exact markers, `signature:` last with one LF), and every other spelling is refused naming the line; sign refuses what parse would; a vault with no handoff and one with one handoff are spelled byte for byte as before chains.	EN::a_signed_envelope_renders_exactly_and_parses_back; EN::the_grammar_has_one_spelling; EN::keys_and_signatures_must_be_canonical_and_sized; EN::sign_refuses_what_parse_would; EN::no_handoff_and_one_handoff_are_spelled_byte_for_byte_as_before_chains	line parser, Ed25519 (ring)	1,7,8,12
ENV-BOUNDS	contract	envelope::parse, sign	An envelope past MAX_ENVELOPE (16 MiB), with more than MAX_LOCKERS (64) lockers or more than MAX_HANDOFF (64) handoff pairs is refused by parse and by sign.	EN::sizes_are_bounded; EN::the_handoff_chain_is_capped	length checks	1,7
ENV-SIGNATURE	contract	envelope::verify_for_reader, verify_signature	Every single-bit flip of a signed envelope, every truncation and every extension is refused, by the grammar or by the Ed25519 signature over every byte before `signature:`.	EN::every_single_bit_flip_of_a_signed_envelope_is_refused; EN::a_truncated_or_extended_envelope_is_refused	Ed25519 (ring)	1,8,12
ENV-PIN	contract	envelope::Envelope::check_pin, verify_for_reader	A reader takes a version only under its expected name and when its signer is the pin (Same) or is reached from the pin through the version's handoff chain with every endorsement from the pin on verifying for this name (Rotated(signer), re-pinned): a chain of rotations is followed from every key the vault had; a broken link refuses it from every key before the break; an endorsement made for another vault or by another key is refused; a wrong pin or name is refused naming both fingerprints.	EN::the_wrong_pin_or_the_wrong_name_is_refused; EN::an_endorsed_rotation_is_accepted_and_re_pins; EN::an_endorsement_for_another_vault_or_by_another_key_is_refused; EN::a_chain_of_rotations_is_followed_from_every_key_the_vault_had; EN::a_chain_with_a_broken_link_is_refused_from_every_key_before_the_break	chain walk, Ed25519	1,7,8,12
ENV-RETIRED	contract	envelope::Retired::gives_way, Envelope::check_pin_with, verify_for_reader_with	A retired key signs nothing a reader takes. A pin that is retired, or was reached through a retired key's endorsement of a key the build does not know, gives way to the current key: the chain is followed from it, the result is Replaced(signer), and the floor held under the old pin judges nothing.	EN::a_retired_key_signs_nothing_and_a_pin_to_it_follows_the_key_that_replaced_it; EN::a_pin_the_removed_admin_led_a_reader_to_gives_way_and_what_it_held_floors_nothing	slices of keys, chain walk	1,7
ENV-FLOOR	contract	envelope::verify_for_reader, Floor, Envelope::check_extends, chain_carried_on	Against the held Floor: a higher seq that carries on the held chain (every held link, in order, right before the held signer's own) is New; the same seq and digest is Unchanged when the floor was applied and New when held below; the same seq with other bytes, or a lower seq, is refused (a rollback or a fork); a version that drops a held link is refused, and an admin's next version after one that only dropped links from the front carries them again (chain_carried_on).	EN::rollback_and_fork_are_refused_and_identical_bytes_are_unchanged; EN::a_newer_version_carries_on_the_handoff_chain_the_reader_holds; EN::an_admin_puts_back_the_links_a_rotation_by_a_build_before_chains_dropped	Vec<Link> comparison	1,7
ENV-NAMES	contract	envelope::valid_name, valid_label, fingerprint	valid_name(n) ⇔ n matches [a-z0-9][a-z0-9-]{0,62}; valid_label(l) ⇔ l = admin or l is a decimal from 1 without leading zeros (bounded digits); fingerprint(k) is the first 10 bytes (80 bits) of SHA-256(k) as five `-`-joined groups of four lowercase hex digits.	EN::a_fingerprint_is_five_groups_of_hex; EN::names_and_labels	byte predicates, SHA-256	1,3,5,7
TXT-ROUNDTRIP	round-trip	text::Document::parse, render, machines, admins, entries	render(parse(t)) = t byte for byte for every valid t, its comments and blank lines kept (a `#` inside a heredoc or after a value is text); machines, admins and entries read in file order; an empty heredoc and one holding a single empty line read back as written.	T::an_unedited_document_renders_byte_for_byte; T::machines_admins_and_entries_are_read; T::heredocs_empty_and_one_empty_line	line model over String	1,7
TXT-HOSTS	contract	text::Audience::header, Document::parse	A host section's audience is its sorted, deduplicated host set, and its header spells it so.	T::host_headers_are_sorted_and_deduplicated	sorting, dedup	1,7
TXT-ENTRIES-FOR	contract	text::merge_for	merge_for(h, entries) is `[everyone]`'s entries overridden by name by those of sections naming h, sorted by name (over a document's entries or a machine's opened lockers), and two host-audience entries of one name that both include h, or two `[everyone]` entries of one name, are an error naming the entry, never the value.	T::entries_for_a_host_override_everyone_by_name; T::merge_for_reports_a_clash_between_two_host_audiences	maps by name	1,7
TXT-LOCKERS	contract	text::Document::lockers, parse_locker	lockers() gives one locker per distinct audience with at least one machine: `[everyone]` first (present with no entries, and sections of one audience accumulate), then host audiences sorted; each locker's text reads back with parse_locker to its audience's entries; parse_locker refuses `[machines]`, `[admins]` and `[join]` and checks no host against a machine list.	T::lockers_are_everyone_first_then_sorted_audiences_and_read_back; T::the_everyone_locker_exists_without_entries_and_sections_accumulate; T::a_locker_refuses_machines_and_admins_and_does_not_check_hosts	grouping, String building	1,7
TXT-VALIDATE	contract	text::Document::parse	parse accepts exactly the texts its doc lists (known sections, hosts declared in `[machines]`, at least one admin in a whole vault but none needed in a locker, valid `age1…` recipients, no duplicate machine, recipient, admin or entry within an audience, no name set by two host sections that share a host, terminated heredocs, canonical base64 heredocs, values within MAX_VALUE, the text within MAX_TEXT, and at most MAX_HOST_AUDIENCES (62) distinct sets of hosts, so a vault's lockers with the admins' fill at most MAX_LOCKERS; an edit past it is refused and changes nothing); every error carries its 1-based line, and no error or Debug output holds a value.	T::structural_errors_name_their_line; T::machine_lines_are_checked; T::a_vault_needs_an_admin_but_a_locker_does_not; T::sizes_are_bounded; T::no_error_or_debug_output_ever_holds_a_value; T::a_vault_names_as_many_sets_of_hosts_as_one_version_seals	parser over lines	1,7
TXT-NAMES	contract	text::valid_entry_name, valid_machine_name	valid_entry_name(n): a file name, or `~/` and a home path of valid components within the length bound that reaches neither into `~/.secrets` nor into `~/.config/aterm/secrets` (compared without case); valid_machine_name(n) ⇔ n matches [a-z0-9][a-z0-9-]{0,62} and is not `machines`, `admins`, `everyone` or `join`.	T::entry_and_machine_names; T::no_machine_may_be_named_join	byte predicates, split	1,3,5,7
TXT-SET	contract	text::Document::set_entry, set_entry_with, unset_entry	set_entry replaces an existing entry's lines in place (comments kept) or appends to the first section of the audience, making one at the end if none; it spells `name=value` for one line of UTF-8 without CR, LF or NUL, a text heredoc for UTF-8 ending in LF without CR or NUL, else a base64 heredoc of 64-column lines, with a tag (`END`, `END1`, …) no value line equals, so parse reads back the same value; whatever would break the document's rules is refused and nothing changes; unset_entry removes every line of the entry.	T::set_entry_replaces_in_place_and_keeps_comments; T::set_entry_appends_to_the_first_section_or_makes_one; T::set_entry_spells_each_kind_of_value_so_it_reads_back; T::set_entry_refuses_what_would_break_the_rules_and_changes_nothing; T::unset_entry_removes_every_line_of_it	line edits over Vec<String>	1,2,7
TXT-MACHINES	contract	text::Document::add_machine, remove_machine, add_admin, remove_admin	add_machine appends `name = recipient` and refuses a duplicate name or recipient; remove_machine drops its `[machines]` and `[admins]` lines and its name from multi-host headers, refusing (unchanged) when a section names only it and holds entries unless told to drop those sections and their entries, and dropping such a section with no entry left (an `unset` of its last) either way; the last admin is never removed.	T::add_machine_appends_and_refuses_duplicates; T::remove_machine_edits_headers_and_refuses_single_host_sections; T::remove_machine_can_drop_its_own_sections; T::a_section_its_entries_left_goes_with_the_machine; T::the_last_admin_is_never_removed	line edits	1,2,7
TXT-NEW	contract	text::Document::new_vault	new_vault(m, r) is a commented text with `[machines]` holding m = r, `[admins]` holding m and an empty `[everyone]`, which parses; with a join password and if-absent entries added it still parses and reads them back.	T::a_new_vault_explains_itself_and_parses; T::a_new_vault_takes_a_join_password_and_if_absent_entries	String building	1,7
TXT-JOIN	contract	text::Document::join_password, set_join_password, open, set_open, code_only, add_code_only, add_machine, parse_locker	A join password is set, rendered, parsed, replaced and removed in `[join]` (blanks around it dropped); a weak one is refused without being quoted; a vault holds at most one. An `open = yes` line is set, rendered, parsed and removed in the same place; a vault holds a password or an open line, never both, and setting one replaces the other in its line; any other `open` spelling is refused naming its line. A `code-only = age1…` line names a key an open vault leaves to approval by code: added once however often, beside a password or an open line, kept by opening, closing and setting a password, refused when its key is invalid or named twice (both lines named), and removed when `add_machine` lets that key in. `[join]` exists only in a whole vault, never in a locker.	T::a_join_password_is_set_rendered_parsed_replaced_and_removed; T::weak_join_passwords_are_refused_without_quoting_them; T::a_vault_holds_one_join_password_at_most; T::the_join_password_is_a_whole_vaults_only; T::an_open_vault_is_set_rendered_parsed_and_closed; T::an_open_line_is_checked_and_never_beside_a_password; T::a_code_only_key_is_named_once_and_dropped_when_it_is_let_in	line edits	1,2,7
TXT-IF-ABSENT	round-trip	text::Document::parse, render, set_entry_with, lockers	` if-absent` after a heredoc's tag (text or base64) round-trips through parse and render, is set and cleared in place, reaches the lockers, and is accepted in that one place only.	T::if_absent_entries_round_trip_in_every_spelling; T::if_absent_is_set_and_cleared_in_place_and_reaches_the_lockers; T::the_if_absent_modifier_has_one_place	line parser	1,7
TXT-ANCHOR	contract	text::Document::set_entry_with, parse, anchored, merge_for	` if-absent=<anchor>` is spelled, parsed and carried into the lockers; an anchor must be another ` if-absent` entry that every host receiving the anchored one also receives, else it is refused; anchored() says whether any entry has one.	T::anchored_entries_are_spelled_parsed_and_reach_the_lockers; T::an_anchor_is_another_if_absent_entry_its_receivers_all_get	set inclusion over audiences	1,5,7
TR-SOURCE	contract	transport::parse_source, vault_name, describe	A URL parses with or without a `#<signer>` pin in padded or unpadded, standard or URL-safe base64; an absolute or `~/` path parses (expanded against home); anything else is refused with a hint; the vault name is a URL's last segment or a path's stem and must be a valid name; query (`?after=`) and join (`/join`) URLs compose on it, and describe names scheme, host and path only.	TR::a_url_parses_with_and_without_a_pin_in_every_base64_spelling; TR::paths_parse_and_anything_else_is_refused_with_a_hint; TR::the_vault_name_is_the_last_segment_or_the_stem; TR::query_and_join_urls_compose	&str parsing	1,5,7
TR-REASON	invariant	transport::reason, fetch, publish	What a server says reaches a message only as one line of at most 200 printable ASCII characters (anything else `?`): its reason is its body's first line so cut, a refusal names the status and that reason, and a malformed answer is quoted the same way.	TR::a_reason_is_one_short_printable_line; TR::a_refusal_names_the_status_and_a_sanitized_reason; TR::a_malformed_answer_is_quoted_as_one_short_printable_line	char filtering	1,7
TR-PATH	contract	transport::fetch, publish	At a path, fetch reads the envelope and publish writes it atomically at mode 0644, taking only a version newer than the one of the vault's name already there (refused like a server's 409 otherwise), from the read to the rename under `<path>.lock` (an exclusive create, removed when done), so a publish while another holds it is refused, naming the lock and its holder, and changes nothing.	TR::a_path_is_fetched_and_published_atomically_and_world_readably; TR::a_path_takes_only_a_newer_version_of_its_vault; TR::a_publish_to_a_path_another_publisher_holds_is_refused	fs	1,8
TR-LONGPOLL	contract	transport::fetch	A fetch with `after = n` asks `GET <url>?after=n`; 204 is NoNewer with the seq its `ETag: "<seq>"` names (none without one, or with one that is not a quoted seq), 404 Missing, 200 the body.	TR::a_long_poll_asks_after_the_seq_and_reads_no_newer	HTTP	1,8
TR-JOIN-LINE	contract	transport::line, listed, parse_join_line, valid_code	A join line is `age1… <host> <code>`, optionally with the queue time, optionally ending ` proof=<44 chars of padded base64>`, single spaces; the recipient must be valid, the host a machine name, the code four `-`-joined groups of four `[a-z2-7]`; anything else is refused.	TR::join_lines_are_validated	&str parsing	1,5,7
FS-PRIVATE-DIR	contract	fsx::ensure_private_dir	ensure_private_dir makes the directory and its missing parents, narrows the directory itself to 0700, and refuses a symlink there, a non-directory, or a directory another user owns.	FS::a_private_dir_is_made_and_narrowed_to_0700; FS::a_symlinked_dir_is_refused	fs, modes	1,8
FS-READ	contract	fsx::read_private, read_owned, read_public	Every read opens with O_NOFOLLOW and O_NONBLOCK and judges what it opened: read_private refuses group or other bits and a symlink; every read refuses more than its bound; a FIFO is refused at once, never blocking. A read fills one buffer sized from the file's length (a byte over, to see the end), never grown, so no block it outgrew is freed holding the bytes unwiped.	FS::private_reads_refuse_group_or_other_bits_and_symlinks; FS::reads_are_bounded_and_a_fifo_never_blocks; FS::a_read_fills_one_buffer_sized_from_the_files_length	fs, fstat	1,8
FS-NOREPLACE	contract	fsx::rename_noreplace, write_new	rename_noreplace never replaces an existing target; when the unlink of the source fails it undoes the link, leaving the names as it found them; write_new fills only an empty name and reports Ok(false) otherwise, removing its temp either way.	FS::rename_noreplace_refuses_an_existing_target; FS::a_rename_whose_unlink_fails_undoes_its_link; FS::write_new_fills_only_an_empty_name	fs, link/unlink	1,8
FS-FILE-ID	contract	fsx::file_id	file_id(a) = file_id(b) exactly when the two paths name one file (device, inode), however spelled, without following a final symlink.	FS::file_ids_tell_one_file_from_two	fs	1,8
FS-WIPE	contract	fsx::wipe_and_remove, wipe_if	wipe_if removes a regular file only while what the descriptor it opened holds passes its check and the path still names that file (one renamed over it since is neither zeroed nor removed), removing the name before it zeroes the file through that descriptor (so every other hard link reads zeros, except of a file its owner may not write, which loses its name but keeps its bytes) and syncs; wipe_and_remove does the same whatever the file holds; a symlink is removed without touching what it names; a missing file is fine.	FS::wipe_removes_files_and_only_the_link_of_a_link; FS::wipe_zeroes_the_bytes_under_every_name; FS::a_judged_wipe_never_touches_a_file_renamed_over_the_one_it_judged	fs	1,8
MAT-PATHS	contract	materialize::target_path, digest_hex, aside_name	target_path(home, n) is home/.secrets/n for a file name and home/p for `~/p`, and refuses every invalid name (empty, `..`, a slash in a file name, absolute, into `~/.secrets`, a dot file, `~/` alone); digest_hex is lowercase hex SHA-256; a moved-aside name is the flattened name and `.<secs>`, with `.1`, `.2`, … where taken, cut to fit NAME_MAX with a hash that keeps two long names apart.	M::target_paths; M::digests_are_lowercase_hex_sha256; M::moved_aside_names_stay_within_name_max_and_apart; M::a_taken_aside_name_gets_a_numbered_suffix	path building, hashing	1,7
MAT-WRITE	contract	materialize::apply	A first sync writes every entry atomically at 0600 and narrows `~/.secrets` to 0700; a home path's missing parents are made 0700; a target already equal to the value is adopted, its mode narrowed to 0600; one the vault did not write stays out of the manifest, never announced as a write, and is adopted again on every pass.	M::a_first_sync_writes_0600_files_and_narrows_the_directory; M::a_home_path_target_gets_private_parents; M::an_equal_file_is_adopted_and_narrowed; M::an_adoptable_file_is_never_announced_as_a_write	fs	1,8
MAT-ASIDE	invariant	materialize::apply, Manifest	A file the vault did not write, or one edited since it did, is moved to `.replaced/` with its content before it is overwritten; what the vault wrote is overwritten in place; a dropped entry's file is removed only while the vault wrote it (judged on the descriptor that is zeroed, FS-WIPE), and one whose unlink is refused keeps its bytes and its digest, so the next pass removes it; a dropped name whose directory is now a file is forgotten, as a missing one is; a file the vault never wrote is never removed, an adopted one included, and a new value moves an adopted file aside first.	M::a_pre_existing_different_file_is_moved_aside_with_its_content; M::a_change_overwrites_what_the_vault_wrote_but_saves_a_local_edit; M::a_dropped_entry_is_removed_only_while_unchanged; M::a_dropped_file_whose_unlink_is_refused_keeps_its_bytes_for_the_next_pass; M::a_dropped_name_whose_directory_is_now_a_file_is_forgotten; M::a_file_the_vault_never_wrote_is_never_removed; M::dropping_an_adopted_entry_leaves_the_machines_own_file; M::a_new_value_moves_an_adopted_file_aside_first	fs, manifest map	1,8
MAT-SYMLINK	invariant	materialize::apply	No write follows a symlink: a link at a target is moved aside itself and a regular file takes its place, and an adopted file's mode is narrowed through a descriptor that refuses a link; an entry behind a symlinked directory (a `~/a/b` parent, or `~/.secrets` itself), or behind aterm's own key directory reached by another spelling (by device and inode), fails naming it before anything is looked at behind it, and the other entries proceed.	M::a_symlink_at_the_target_is_replaced_never_followed; M::a_symlinked_directory_on_the_way_is_refused_never_followed; M::a_symlinked_secrets_dir_fails_its_entries_and_spares_the_rest; M::an_entry_behind_a_symlinked_directory_is_refused_before_any_look; M::narrowing_never_follows_a_link_swapped_in_after_the_look; M::aterms_own_directory_by_another_spelling_is_refused	fs, lstat	1,8
MAT-ISOLATE	invariant	materialize::apply	An entry that cannot be placed (a directory at its target, an invalid name) fails alone, with a reason naming its path, and every other entry is placed.	M::a_directory_in_the_way_fails_that_entry_only; M::an_invalid_name_fails_that_entry_only	fs	1,8
MAT-ONE-FILE	invariant	materialize::apply, fsx::file_id	Two names are one file by (device, inode), never by spelling: the second name to land on a file another entry of the run placed is refused whatever its spelling, a dropped name for a file another entry holds (or a refused entry names) is never wiped, and an entry respelled by case on a case-insensitive volume keeps what the vault wrote.	M::two_names_for_one_file_are_refused_where_the_volume_makes_them_one; M::a_second_name_for_a_placed_file_is_refused_whatever_its_spelling; M::a_dropped_name_for_another_entrys_file_is_never_wiped; M::an_entry_respelled_by_case_keeps_what_the_vault_wrote; M::a_dropped_name_for_a_file_a_refused_entry_names_is_kept	fs, maps keyed by file id	1,8
MAT-IF-ABSENT	invariant	materialize::apply	An ` if-absent` entry fills only a gap (by a link that refuses a taken name): a file it finds that the vault never wrote stays as it is and out of the manifest; a file the vault filled takes new values while the vault wrote it and is the machine's own, left alone, once anything changes it; it never claims a file another entry placed.	M::an_if_absent_entry_fills_a_gap_but_never_touches_a_machines_own_file; M::an_if_absent_file_the_vault_wrote_is_the_machines_own_once_changed; M::an_if_absent_entry_cannot_claim_a_file_another_entry_placed	fs	1,8
MAT-ANCHOR	invariant	materialize::apply, standings	An anchored entry is the vault's on a machine only while its anchor is (the vault wrote the anchor's file, or this pass fills its gap): a set lands whole on a machine with none of it; beside the machine's own anchor its gaps stay empty and what the vault filled before is taken back while unchanged; an anchor the machine cannot judge fills nothing; a file beside an anchor that cannot be looked at stays the vault's; a part that becomes the machine's own while the pass writes (a gap taken, or the vault's file changed) fails that pass, so the next one takes the rest of the set back.	M::a_gap_taken_while_the_pass_writes_fails_so_the_next_pass_takes_the_set_back; M::a_file_the_vault_wrote_changed_while_the_pass_writes_fails_so_the_next_pass_takes_the_set_back; M::a_machines_own_machine_toml_keeps_the_vaults_key_and_profile_away; M::an_anchored_set_fills_a_machine_that_has_none_of_it_whole; M::a_profile_the_vault_filled_beside_a_machines_own_key_is_taken_back; M::a_key_and_machine_toml_the_vault_filled_beside_a_machines_own_profile_are_taken_back; M::a_file_the_vault_wrote_beside_an_anchor_it_cannot_look_at_stays_the_vaults; M::a_machines_own_key_keeps_the_vaults_profile_and_machine_toml_away; M::a_machines_own_profile_keeps_the_vaults_key_away; M::an_anchor_the_machine_cannot_judge_fills_nothing	fs, a fixed point over anchors	1,7,8
MAT-RESUME	contract	materialize::apply_resuming	A pass cut short after its writes keeps the anchored set it wrote the vault's: the next pass finds those files as written and treats them as the vault's. A temp a pass cut short left beside a target it was writing, holding that value, is wiped by the next (one that is still a second name of a file, the target's, another entry's or a target renamed away since, loses only that name); another writer's temp is left.	M::a_pass_cut_short_after_its_writes_keeps_the_anchored_set; M::a_temp_a_pass_cut_short_left_holding_its_value_is_wiped; M::a_cut_temp_that_names_another_file_loses_only_its_name	fs, manifest	1,8
MAT-REPORT	invariant	materialize::Report	A report names entries and paths, never a value.	M::reports_carry_names_never_values	strings	1
SRV-TOFU	contract	server::serve, this_machine, unheld_note	A name the store holds nothing for takes its first envelope under the configured `--pin`, else trusts it on first use only from this machine (loopback or one of its own addresses), refusing another Mac's with a 403 whose reason names the server's own Mac and `aterm secrets serve --pin` as what lets it in, and serves it with `ETag: "<seq>"`. A server whose store holds no vault says so at start: a joining machine finds none until a Mac in the vault puts its copy there, which another Mac can do only for a name it pins (named); holding one, it says nothing.	SV::the_first_envelope_is_trusted_on_first_use_and_served_with_an_etag; SV::a_configured_pin_holds_for_a_vault_not_yet_held; SV::a_first_version_is_taken_only_from_this_machine_or_under_a_pin; SV::this_machine_is_loopback_and_its_own_addresses; SV::a_store_that_holds_no_vault_says_a_mac_in_the_vault_puts_its_copy_here	sockets, store	1,8,13
SRV-PUT	contract	server (PUT)	A PUT is taken only when it verifies under the name's pin with a higher seq and carries on the held handoff chain; every refusal names why and leaves the store unchanged; the same seq with other bytes is refused as not newer; an endorsed handoff moves the pin, also across several rotations from the first key; a retired key signs nothing the server takes, and a store a retired key's holder fed takes the vault once the build retires it.	SV::refused_puts_name_why_and_leave_the_store_unchanged; SV::a_same_seq_with_other_bytes_is_refused_as_not_newer; SV::an_endorsed_handoff_moves_the_pin; SV::a_fresh_store_pinned_to_the_first_key_takes_a_vault_three_rotations_on; SV::a_rotation_that_drops_a_link_of_the_held_chain_is_refused; SV::a_store_the_removed_admin_fed_takes_the_vault_once_its_build_retires_the_key; SV::a_retired_key_signs_nothing_the_server_takes	HTTP, envelope rules	1,8,13
SRV-LONGPOLL	contract	server (GET ?after)	A long-poll answers at once when the held seq is newer, else wakes on the PUT of a newer one, else ends 204 after the hold or at stop; a hold too long to add to the clock ends with the newer version, never an overflow.	SV::a_long_poll_answers_at_once_when_newer_and_wakes_on_a_put; SV::a_long_poll_ends_with_204_after_the_hold_or_at_stop; SV::a_hold_too_long_to_add_ends_with_a_newer_version	condvar, threads, clock	1,8,13
SRV-JOINS	invariant	server (join queue and store)	Join requests are validated, their code the key's, and queued by key and address: the same request again, from any address, keeps its place and the address it was queued from and moves its time on at most hourly, so one still waiting never ages out, its address changed or not; the change gap reads when its own address last queued or changed it, which no re-post moves, so a stranger's re-post of a listed line holds off no change; a new proof or host from the same address replaces the old; another address's is queued beside it, so none replaces or hides another's request or holds its changes off. At most 256, and 4 from one address; once the address or the queue holds its cap, the longest queued request asked before the vault's current version gives way (a publish answered it; one still waiting counts again when it asks again), so joins asked under a key that gave way give way too. A request lives 7 days either side of the time it was last asked; a key changes its request at most every 10 s, but a time ahead of the clock (stepped back since) holds no change off (the log notes changes hourly); the store keeps each address and the listing never shows it; a restart reads the store back and expires the old.	SV::join_requests_queue_deduplicate_and_validate; SV::the_join_queue_is_bounded; SV::one_address_queues_at_most_4_joins_and_the_listing_never_names_it; SV::a_join_asked_again_keeps_its_place_moves_its_time_hourly_and_a_new_proof_replaces_it; SV::a_publish_answers_the_queued_joins_and_one_still_waiting_counts_when_it_asks_again; SV::a_key_changes_its_request_at_most_every_10_s_and_the_log_notes_it_hourly; SV::joins_asked_under_a_key_that_gave_way_give_way_to_new_ones; SV::the_join_store_keeps_each_address_where_the_listing_never_shows_it; SV::the_server_restarts_from_its_store_and_expires_old_joins; SV::expiry_keeps_a_week_either_side_of_the_clock; SV::another_address_can_neither_replace_nor_hide_a_queued_join_request; SV::a_clock_stepped_back_holds_off_no_change_and_drops_no_request	maps, clock, store file	1,8,13
SRV-HTTP	contract	server (request parser)	Heads over 16 KiB and bodies over MAX_ENVELOPE are refused; a PUT with `Expect: 100-continue` is answered 100 then 204; a bad path, method or query is refused with its status; a request that carries `Origin` or any `Sec-Fetch-*` header (browser request metadata; plain HTTP reads without either header remain admissible, so this filter is not client authentication) is refused 403 before its body is processed or anything is queued.	SV::oversize_heads_and_bodies_are_refused; SV::an_expect_continue_put_is_answered_100_then_204; SV::bad_paths_methods_and_queries_are_refused; SV::a_browsers_request_is_refused_before_anything_is_read_or_queued	HTTP/1.1 parsing	1,7,8
SRV-LIMITS	invariant	server (admission)	At most 16 connections from one address (the next reads 503); at most 4 PUT bodies read at once, 1 from one address of another machine, the last slot kept for this machine; a join waiting on its store write holds up no read or publish; a head not finished within its deadline, and an answer read too slowly, are cut; a PUT body that stalls (10 s) or outruns its deadline (120 s) is answered 408 naming which, and its slot freed.	SV::one_address_holds_at_most_16_connections_and_the_next_reads_503; SV::at_most_4_put_bodies_are_read_at_once; SV::another_machine_reads_one_put_at_a_time_and_the_last_slot_is_this_machines; SV::a_join_waiting_on_its_store_write_holds_up_no_read_or_publish; SV::a_client_that_never_finishes_its_head_is_cut_at_the_deadline; SV::an_answer_read_too_slowly_is_cut_at_its_deadline; SV::a_put_body_that_stalls_or_outruns_its_deadline_is_cut_with_408_and_frees_its_slot	threads, atomics, deadlines	1,8,13
SRV-ARGV	contract	server::dns_sd_args, caffeinate_args	dns_sd_args = `-P <name> _alab-vault._tcp local <port> <name>.local <ip> path=/v1`; caffeinate_args = `-i -w <pid>`.	SV::the_children_argv_is_exact	Vec<String>	1
SRV-REPLACED	contract	server::supervise, Children	When the program is replaced on disk, or the stop flag serve's signal thread raises on SIGTERM, SIGINT or SIGHUP is up, the supervisor stops the server and ends cleanly; the children serve holds (dns-sd, which has no lifeline of its own, and caffeinate) end when it lets them go. That serve's signal thread raises the flag is not tested: a test process cannot take a stop signal, since the harness's threads leave it unblocked.	SV::a_replaced_program_stops_the_server_and_ends_it_cleanly; SV::a_raised_stop_flag_stops_the_server_and_ends_it_cleanly; SV::letting_the_servers_children_go_ends_them	fs, threads	8,13
CL-KEY-RACE	contract	client::machine_identity	Two makers of the machine key at once: one key wins, written by a link that refuses a taken name, and no existing bytes are replaced.	CL::a_concurrent_machine_key_wins_without_replacing_any_existing_bytes	fs, threads	8,13
CL-ALAB	contract	client (source and pin)	The ALab vault is recognised by its keys (ALAB_SIGNER and the retired ones), whatever URL reaches it.	CL::the_alab_vault_is_known_by_its_keys_whatever_url_reaches_it	key comparison	1
CL-RETIRED	contract	client::sync, admin_open	A reader the removed admin led off follows the vault once its build retires that key; an admin gives the vault back to a store the removed admin fed, and gives a path its copy back over a version it does not verify, however high its seq.	CL::a_reader_the_removed_admin_led_off_follows_the_vault_once_its_build_retires_the_key; CL::an_admin_gives_the_vault_back_to_a_store_the_removed_admin_fed; CL::an_admin_gives_a_path_its_copy_back_over_a_version_it_does_not_verify	whole flow over a scratch home	8
CL-CUT-PASS	invariant	client::sync, materialize::apply_resuming, state.toml	A pass is done only when its writes are saved: what a cut pass pended and wrote is the vault's on the next pass (taken back with its anchor, removed when dropped), and it writes nothing it did not pend first; a gap it left, or a file the person changed since, is never pending; the same version is applied again while writes are pending; a target that changed since the look is not written unannounced; a membership whose pin gave way floors on its cached version unapplied, refusing older versions, another at its seq and a next one that drops its chain.	CL::a_pass_cut_short_after_its_writes_leaves_its_files_the_vaults; CL::a_pass_cut_short_leaves_no_entry_it_keeps_away_pending; CL::a_cut_pass_resumes_as_the_vaults_and_writes_only_what_it_pended; CL::a_pass_cut_short_is_applied_again_at_the_same_version; CL::a_target_that_changed_since_the_look_is_not_written_unannounced; CL::a_membership_with_no_digest_floors_on_its_cache_unapplied	state across passes	1,8,10
CL-STATE	contract	client::sync, join, ask_again_if_due, load_state, save_state	A request for a vault this machine left changes nothing of the state a later join made for the vault it went to; a join to another vault forgets the join-password proof made for the one it left and restarts the hourly re-ask, so the subscriber never sends that proof to the new vault. state.toml reads back whatever save_state writes: a manifest past 4 MiB reads, and a state past MAX_STATE is refused before it is written, so the one held still reads.	CL::a_request_for_a_vault_this_machine_left_leaves_its_state_alone; CL::a_join_to_another_vault_forgets_the_proof_made_for_the_last; CL::a_large_manifest_reads_back_and_one_past_the_bound_is_never_written	state files	8
CL-CHAIN	contract	client::admin_open, admin_publish	An admin's next version puts back the handoff links a rotation by a build before chains dropped.	CL::an_admin_puts_back_the_link_a_rotation_by_a_build_before_chains_dropped	envelope rules	1,8
CL-SEAL	contract	client::seal (admin_publish), envelope::sign	A vault whose envelope would come to more than MAX_ENVELOPE is refused before it is signed, saying it is too large to publish and how much entry text fits (each entry is sealed for the admins and again for its audience); five values of MAX_VALUE in `[everyone]` seal, a sixth does not.	CL::a_vault_too_large_to_seal_is_refused_saying_why	length sums over Vec<Locker>	1,8
CL-EDIT	contract	client (edit directory)	An unpublished edit and files the editor made that nobody claimed survive later edits; a draft is discarded only while it still holds what was published.	CL::an_unpublished_edit_and_unclaimed_files_survive_later_edits	fs	8
CL-RETRY	contract	client::pause_after, pass_line, retry_after, nap_unless_moved, subscriber_loop	After a pass that applied a version with an entry left failed (which leaves the next pass no long-poll), the subscriber waits 5 s, doubling to 5 min while entries keep failing, and logs a retry that ends the same way once (so for a copy put back where the source held none, CL-LOST); a pass that applied cleanly long-polls at once, and any pass but a failing one starts the retries again at 5 s; a path source is read once a minute. A pass that errs is tried again after 5 s, doubling to 1 min however long it keeps erring (a server back after any absence is asked again within a minute), and after 5 s again once this machine's network changed during the wait.	CL::a_pass_that_leaves_an_entry_failed_backs_off_and_a_clean_one_starts_again; CL::a_pass_that_errs_is_tried_again_within_a_minute; CL::a_wait_after_an_error_ends_once_the_network_changes	Duration	1
CL-REPAIR	contract	client::repair_due, subscriber_pass, files_intact	While a machine that is in, with nothing failed or pending, holds a file the manifest says the vault wrote that no longer holds it (deleted, edited, or an if-absent file a program rewrote), the subscriber's next pass fetches and applies the version it holds instead of long-polling, once per version, so readiness returns: the deleted file is written again and the rewritten if-absent file is left the machine's own; a newer version may be repaired again. A repair counts only once its pass succeeds: one that cannot reach the server leaves it due, and the next pass that reaches it writes the file again.	CL::the_subscriber_repairs_a_changed_file_once_per_version_so_readiness_returns	fs, HTTP	8
CL-PIN	contract	client::pin_admitted, config, sync, join, envelope::Envelope::reached_from	A requested pin admits the configured vault when it is the signer or a key `followed` names, and `followed`, as this build records it (at a re-pin in sync or join) or reads it off the cached version the pin signs (when source.toml has no list), holds only the keys of the chain from which every endorsement on to the signer verifies: a key the chain names before a link that does not verify is no predecessor. A list an older build (v0.110.0 and before) wrote names every key of that chain and is read as written until the next re-pin rewrites it. A retired requested pin is read as the key that replaced it, and a configured pin that gives way admits nothing.	CL::pin_admission_follows_verified_rotation_and_compiled_replacement; CL::a_key_the_chain_names_before_a_link_that_does_not_verify_is_no_predecessor	chain walk, Ed25519, state files	1,8
CL-APPROVER	contract	client::Settled, auto_approve	The approver remembers what it judged only while the queue lists it, so a host posting new proofs grows nothing; a key two addresses list is let in once, as the first line asked.	CL::the_approver_remembers_only_what_is_still_listed; SV::a_key_two_addresses_ask_for_is_let_in_once	BTreeSet, the whole system	8,13
CL-REASK	contract	client::join, request_join, ask_again_if_due, subscriber_pass, change_request	A join cut off before its request is queued (its own sync fails once the person trusted the key, or its request fails while the person was asked for the join password) leaves the machine waiting with its re-ask due at once, with the proof the person gave: the subscriber's next pass that reaches the server queues the request, rather than an hour on; so does a waiting machine's pass that fails (a server that lost its store lost the queue with it), also when another machine in the vault puts it back before the next pass, which then finds nothing new; failures one after another write the state once. The re-ask reads its state under the lock with source.toml, and only while that names the vault it was called for: a pass begun on a vault the machine left sends that vault nothing, never a proof made for another; it asks under the host source.toml names then, the one its proof was made for: a join of the same vault under another name drops the old name's proof as it saves the new name, and the re-ask counts from then. It records only when it asked, never a proof, and only while the state still holds the proof it read: a join's newer proof stands.	CL::a_join_cut_off_before_its_request_is_queued_is_asked_for_by_the_next_pass; CL::a_reask_begun_on_the_vault_left_never_sends_the_new_ones_proof_there; CL::a_request_the_server_lost_with_its_store_is_asked_again_once_the_vault_is_back; CL::a_reask_asks_under_the_name_source_toml_holds_now; CL::a_join_under_another_name_drops_the_old_names_proof; CL::a_reask_never_puts_back_a_proof_a_join_replaced	fs, HTTP	8
CL-LOST	contract	client::sync, held_copy, pause_after, pass_line, subscriber_loop, admin_open	A signed URL vault's source that holds no vault of that name (a lost store, or a server moved to a Mac with an empty one) gets back the cached copy of the first machine in the vault whose sync finds it missing (its subscriber's next pass, or `aterm secrets sync`), admin or not and whatever the vault approves (a vault closed to joins too), and the pass says so (PutBack, no seq served): only the version state.toml holds (an apply that left an entry unwritten keeps it), only one that let the machine in, and only once it verifies under a pin that does not give way at that seq and digest. The next pass is 5 s later; a source that holds none again at each next pass (a GET that reaches no vault server) is given the copy 5 s, doubling to 5 min, apart, and the log says so once until a pass ends otherwise. A machine waiting for approval, or whose cached copy is not that version or does not verify, publishes nothing, and its pass is the missing error alone. A server that refuses the copy (one that takes no first version of that name from another Mac, as for a name it does not pin) makes the pass the missing error with the refusal in it, the same text at each pass, so the subscriber logs it once and its backoff paces the next try. The first machine to put it back may hold an older version than another (one that slept through the versions since): a machine holding a newer one puts that back at its next long-poll (CL-NEWER), and an admin command on the older copy before then publishes on it. A path that holds no vault is left to an admin command, which puts its copy back as ever (admin_open): a machine in the vault writes nothing there.	CL::a_machine_in_the_vault_puts_back_a_vault_its_server_lost; CL::a_put_back_the_source_keeps_no_more_backs_off_and_is_logged_once; CL::a_server_that_does_not_pin_the_vault_refuses_its_copy_and_the_pass_errs; CL::a_machine_kept_out_or_without_a_verified_copy_puts_nothing_back; CL::a_machine_in_the_vault_writes_nothing_to_a_path_that_holds_no_vault; CL::an_admin_of_a_vault_closed_to_joins_puts_back_a_vault_its_server_lost	HTTP, fs	8
CL-NEWER	contract	client::sync, held_copy, subscriber_loop, transport::fetch	A long-poll the source ends with nothing newer, its ETag naming a seq below the one this machine holds (a server that lost its store took an older copy back), has a machine that version let in put its cached copy back and say so (PutBack), whatever the vault approves and whether or not the machine is an admin, over whatever was published on the older copy meanwhile: only the version state.toml holds, unchanged since the long-poll asked after it, and only once it verifies under the pin at that seq and digest; a cached copy that is not that version, or does not verify, is never published, nor is the copy of a machine it keeps out (a reader the removed admin led off, whose fork a server holding a version of the key that signed or endorsed it would take, with a seq no real version passes; a fork that carried a locker for the machine is not told apart). The ETag names a seq alone: a stale line that reaches this machine's seq before its long-poll ends is not seen, and its next version is taken as new. A refusal is the pass's error, so the subscriber's backoff, after the server's hold, paces the next try.	CL::a_machine_puts_its_newer_copy_back_over_an_older_one_a_server_took; CL::a_reader_the_removed_admin_led_off_follows_the_vault_once_its_build_retires_the_key	HTTP, fs	8
SVC-OWN-HOME	contract	service::own_home, install, state	A job is set up, removed or read only for the login's own home; a home that is not the login's has no job and asks launchd nothing.	SE::a_job_is_set_up_only_for_the_logins_own_home; SE::a_home_that_is_not_the_logins_has_no_job	paths	1,8
SVC-PATHS	contract	service::label, plist_path, unit_path, log_path	Labels are systems.alab.aterm-secrets and systems.alab.aterm-secrets-server; the plist is ~/Library/LaunchAgents/<label>.plist, the unit ~/.config/systemd/user/<label>.service, the log ~/.config/aterm/secrets/<label>.log.	SE::labels_and_paths_sit_where_launchd_systemd_and_the_design_say	path building	1
SVC-PLIST	contract	service::plist_text, write_plist	The plist runs the program at load, keeps it alive as a Background process at Nice 5, appends stdout and stderr to the log, sets HOME only when the log is the default one, and XML-escapes every string (the five entities); plutil lints it; written, it is 0644 beside a private log directory.	SE::the_plist_names_the_job_keeps_it_alive_and_escapes_every_string; SE::a_log_elsewhere_writes_no_environment; SE::xml_escaping_covers_the_five_entities; SE::the_plist_is_well_formed_by_plutil; SE::writing_the_plist_makes_a_0644_file_and_a_private_log_directory	String building, a plutil run	1,7,8
SVC-SYSTEMD	contract	service::systemd_word, systemd_unit, write_unit, systemctl_path	systemd_word(w) reads back through ExecStart's quoting as w (C escapes, `%%`, `$$`); the unit runs the quoted argv with Restart=always and RestartSec=5; a unit for what it cannot name is refused; systemctl is found by absolute path or refused by name.	SE::a_systemd_word_reads_back_as_itself; SE::the_unit_runs_the_quoted_argv_and_always_restarts; SE::a_unit_file_is_refused_for_what_it_cannot_name; SE::systemctl_is_found_by_absolute_path_or_refused_by_name	string escaping	1,7
SVC-ARGV	contract	service::launchctl_argv, systemctl_argv, print_pid, main_pid	launchctl and systemctl argv are plain words in gui/<uid> and --user; the pid is launchctl print's own `pid = ` line one tab deep (a nested one is not it) and MainPID's value, None for 0 or anything unreadable.	SE::the_launchctl_and_systemctl_argv_are_plain; SE::the_pid_is_the_services_own_line	text scanning	1,7
SVC-PROGRAM	contract	service::in_build_dir, transient, service_program	A job never runs a build: a path in a cargo target directory (a `target` beside Cargo.toml, a profile directory, a `.noindex` target, or under a CACHEDIR.TAG) is refused, the test binary itself included; nor a copy that goes away: a path under /Volumes/, an App Translocation copy, or under the temporary directory, /tmp or /var/tmp (as spelled or resolved) is refused naming --program; an executable inside an `.app` runs from its outermost bundle's Contents/MacOS.	SE::a_build_in_a_target_directory_is_never_a_services_program; SE::a_bundled_executable_runs_from_its_bundle_and_a_build_is_refused; SE::the_test_binary_itself_is_refused_as_a_build; SE::a_tagged_cache_directory_is_a_build_whatever_its_name; SE::a_copy_that_goes_away_is_never_a_services_program	path components	1,7
CLI-PARSE	contract	cli::parse	Arguments are parsed before any probe, prompt or network call: a bare verb is status; join takes one source and its switches, and create one source, `--import`, `--name` and `--no-agent`; approve takes join codes (any case), never host names, and a key's `--name` only when it is a machine name not shaped like a join code; password actions parse, with `--no-agent`; serve flags are checked first and `server on` passes them through (a relative `--store` is under the home directory, however the server is run); on macOS serve advertises the default vault's name unless `--advertise` names another or `--no-advertise`, and only on every address at the default port (a server on loopback or another port claims no name); an unknown command or option is a usage error.	CLI::bare_verb_is_status; CLI::join_takes_one_source_and_its_switches; CLI::create_takes_one_source_and_its_switches; CLI::approve_takes_codes_never_host_names; CLI::a_key_is_never_approved_under_a_name_it_was_not_given; CLI::password_actions; CLI::serve_flags_are_checked_before_anything_runs; CLI::serve_advertises_the_default_vaults_name_unless_told; CLI::a_relative_store_is_under_the_home_directory; CLI::server_on_passes_serve_flags_through; CLI::unknown_commands_and_options_are_usage_errors	argv matching	1,7
CLI-APPROVE	contract	cli::from_queue	approve takes one (key, host) per key from the queue: the request each code names, or every one with --all; a code no machine asks with is refused; a key listed under more than one host is refused both ways, naming the hosts and `approve <key> --name`, since anyone on the LAN can queue a key it read under a name of its choosing beside the machine's own line; one host asked from two addresses is one machine.	CLI::approve_takes_no_key_listed_under_two_names	slices, strings	1,7
CLI-SET	contract	cli::parse, set_in, trim_piped_line	set never takes a value on the command line; a value piped in loses its final LF (and a CR before it) only when what is left is one line of text (UTF-8 without CR, LF or NUL), so a binary value keeps every byte; `--with` names the anchor of an if-absent entry; setting an entry again keeps how it fills unless told; a vault's first anchor needs `--fleet-reads-anchors`, and later ones do not.	CLI::set_never_takes_a_value_on_the_command_line; CLI::a_piped_value_loses_its_line_end_only_when_it_is_one_line_of_text; CLI::set_with_names_the_anchor_of_an_if_absent_entry; CLI::setting_an_entry_again_keeps_how_it_fills_unless_told; CLI::the_vaults_first_anchor_waits_for_the_fleet	document edits	1,7
CLI-WORDS	contract	cli (messages, machine operands)	A rotation of the ALab vault prints both lines the next release changes; a waiting machine whose request went with a join password is told how a mistyped one is fixed, beside what an admin runs (one that asked by code alone is told only what an admin runs); revoke and admins remove name every entry the machine could read (an admin's, every entry; another's, `[everyone]`'s and its own sections', an overridden value too); status names a machine of a signed vault by its code and the name it asked for as asked; a join code typed for a machine (`revoke`, `set|unset --for`, `admins`) reaches the machine whose key has it, and is refused, naming both, when another machine is named like it.	CLI::a_rotation_of_the_alab_vault_names_both_lines_the_next_release_changes; CLI::a_waiting_machine_is_told_how_a_mistyped_join_password_is_fixed; CL::only_a_request_with_a_proof_was_asked_with_a_password; CLI::letting_a_machine_go_names_everything_it_could_read; CLI::status_names_a_machine_of_a_signed_vault_by_its_code; CLI::a_code_typed_for_a_machine_reaches_the_key_that_has_it	format!	1
CLI-EDIT	contract	cli::edit_with	An editor that fails or cannot start leaves no draft while the draft still holds the published text, and keeps one it changed; once a save lands on a version that moved, the draft reopened unchanged publishes only when the person answers yes (then over that version, keeping who it let in) and otherwise publishes nothing and is kept.	CLI::an_editor_that_fails_on_the_published_text_leaves_no_draft; CLI::a_draft_reopened_unchanged_on_a_vault_that_moved_publishes_only_when_the_person_says_so	fs, closures for the editor and the terminal	8
CLI-APPROVER	contract	cli::create, password, put_in_approver, approver_words, approver_line, client::approving_here	Only an admin's subscriber runs the approver, so what promises that machines are let in puts it in, for a vault with a join queue (a URL's; one at a path is joined by key, and reads or installs no subscriber, and where password would say whom its `[join]` lets in, open or with a password, it says instead that a machine joins it only when an admin approves its key): create installs this Mac's subscriber unless `--no-agent`, and password set, new and open (open sampled, also on a vault already open) install it where it does not run (not installed, or installed with no process) unless `--no-agent`; a running one is left be, a state that cannot be read installs nothing, and off reads no subscriber. `--no-agent` (create as password) installs nothing but reads the subscriber, so one already running is said to run. Each says the install line, or that no approver runs here (naming `--no-agent`, the failed install or the unreadable state) and that `aterm secrets agent on` starts one; a failed install leaves the vault made (exit 0), and so does a failure of this Mac's own sync after create's publish (client::create keeps it as `local`), which still puts the approver in and says the copy is not synced yet. status says whether the approver runs only on an admin of a URL vault that approves by itself, read off the version it holds: a path's, a stranger's and a closed vault's have none to report. No test installs a job: the subscriber is read and installed through an injected seam.	CLI::create_puts_in_the_approver_unless_told_not_to; CLI::a_create_whose_own_sync_fails_still_puts_the_approver_in; CLI::the_approver_is_put_in_only_where_it_does_not_run; CLI::a_command_whose_approver_does_not_run_says_what_starts_one; CLI::a_vault_at_a_path_says_a_machine_joins_it_by_key; CLI::a_vault_that_lets_machines_in_gets_an_approver_and_status_says_whether_it_runs	closures for the service manager; the whole system over scratch homes and a loopback server	8
FLOW-LIFECYCLE	refinement	client, server, transport	A LAN vault runs from create through join, approve, sync, edit and revoke; a signed vault at a path needs no server; a plain text file at a path is read as is.	F::a_lan_vault_from_create_to_revoke; F::a_signed_vault_at_a_path_needs_no_server; F::a_plain_text_file_at_a_path_is_read_as_is	the whole system	8,13
FLOW-ROTATION	refinement	client, server, envelope	The server and every reader refuse a key the vault never endorsed; removing an admin (by any publish) rotates the key and every machine re-pins; every later version carries the whole handoff chain, so a machine pinned to the first key still follows; an admin puts back a vault its server lost.	F::the_server_and_every_reader_refuse_a_key_the_vault_never_endorsed; F::removing_an_admin_rotates_the_key_and_every_machine_re_pins; F::removing_an_admin_by_any_publish_rotates_and_later_versions_carry_the_handoff; F::every_version_carries_the_whole_chain_so_the_first_key_still_admits; F::an_admin_puts_back_a_vault_its_server_lost	the whole system	8,13
FLOW-JOIN-PASSWORD	refinement	client::join, watch, auto_approve, admin_publish	A machine that proves the join password is let in by an admin's subscriber; letting a machine go, or removing an admin, renews the password it read so it is not let in again; a join whose subscriber applied the approval first is done; the approver judges a corrected request at once and rests once all are judged; a revoked machine that joins again waits and asks again; a join beside another vault's leftover state starts afresh; a join with the password says how to fix a mistyped one before it waits.	F::a_machine_that_knows_the_join_password_is_let_in_by_the_admins_subscriber; F::letting_a_machine_go_renews_the_join_password_so_it_is_not_let_in_again; F::a_join_whose_subscriber_applied_the_approval_first_is_done; F::removing_an_admin_renews_the_join_password_it_read; F::the_approver_judges_a_corrected_request_at_once_and_rests_once_all_are_judged; F::a_revoked_machine_that_joins_again_waits_and_asks_again; F::a_join_beside_another_vaults_leftover_state_starts_afresh; F::a_join_with_the_password_says_how_to_fix_a_mistyped_one_before_it_waits	the whole system	8,13
FLOW-PROBE	contract	client::probe	A probe names a vault only under its signer (or a key it endorsed); something that is no vault server answering at the address is Foreign, not a failure to reach it, while what a vault server can say stays a failure a later try may get past.	F::a_probe_names_a_vault_only_under_its_signer; F::a_probe_answered_by_something_else_is_not_the_vault	HTTP	1,8
FLOW-OPEN	refinement	client::auto_approve, code_only_for, keep_admitted, Settled, admin_publish	On an open vault an admin's subscriber lets in, in one publish, every queued request from a key not in `[machines]` and on no `code-only` line, with or without a proof, and every member then syncs; the approver rests once the members' requests are judged. A version that opens the vault names every request already waiting on a `code-only` line (one may be a machine let go while it was closed, its approved request still queued) and reports them; a version that lets a machine go from an open vault names its key, so neither the request it joined with nor one it asks again makes it a member, after closing and opening too, and opening at a terminal never offers it among the requests to let in; opening at a terminal offers a machine that asked from two addresses once, and never a key the queue lists under two names, which joins only as the person names it; a version that lets nobody go leaves the lines be, and a closed vault never gets one. Approving a key by code lets it in and drops its line; an edit saved over a newer version keeps the lines that version added and leaves out the machines it let go. A closed vault (no password, not open) approves nothing.	F::an_open_vault_lets_in_every_machine_that_asks_with_no_password; F::letting_a_machine_go_renews_the_join_password_so_it_is_not_let_in_again; F::an_edit_saved_over_an_open_vault_that_let_a_machine_go_keeps_it_out; CLI::opening_never_offers_a_machine_let_go	the whole system	8,13
FLOW-EDIT	refinement	client::free_name, machine_named, admin_open, keep_admitted, admin_publish	free_name finds a free name for a taken one of 62 or 63 characters within 63, and never gives one shaped like a join code (a machine that asks for another's code as its name is let in as `machine`); a machine admitted under a free name is named by its code where it speaks of itself (admin_open's refusal names `admins add <code>`), and machine_named reads a code as the machine whose key has it, refuses one that is also another machine's name (naming both), and refuses one no key has, also when a machine is so named (it would reach a stranger an older approver let in, typed for a machine not yet in), naming that machine's own code; an edit saved over a newer version keeps what that version changed of who is in where the saved text left it as the edit began: a machine admitted meanwhile is kept (and its password), also when it lands after the approver, and at a path after another admin; a machine let go stays gone with its own sections, also after a revocation that renewed the join password, which the save keeps; an admin added or removed stays so; a change the person made themselves stands; a name given another key is refused, naming the key, until the text gives it.	F::a_taken_name_of_62_or_63_characters_still_gets_a_free_one; F::a_machine_admitted_under_another_name_is_named_by_its_code; F::a_machine_asking_for_another_machines_code_as_its_name_is_let_in_under_another; F::an_edit_saved_over_a_machine_admitted_meanwhile_keeps_it; F::an_edit_that_lands_after_the_approver_keeps_the_machine_and_the_password; F::an_edit_at_a_path_that_lands_after_another_admin_keeps_the_machine_it_admitted; F::an_edit_saved_over_another_admins_changes_to_who_is_in_keeps_them; F::an_edit_that_lands_after_a_revocation_keeps_the_machine_out	the whole system	1,8,13
```

## 2. Test → spec map (TSV; every test)

```tsv
file	test	primary	also
L	the_alab_signer_is_the_ed25519_key_of_the_vault_m22_made	ALAB-PINS	ENV-NAMES
L	every_retired_alab_key_is_a_key_and_none_is_the_signer	ALAB-PINS	B64-CANON
L	the_server_advertises_the_name_and_port_the_default_url_reaches	ALAB-NAME	
B64	rfc4648_vectors_round_trip_in_both_spellings	B64-CANON	
B64	unpadded_refuses_padding_bad_length_and_trailing_bits	B64-CANON	
B64	padded_refuses_missing_padding_and_trailing_bits	B64-CANON	
BE	bip173_valid_strings_decode_and_re_encode	BECH-CHECKSUM	
BE	bip173_valid_strings_decode_to_bytes	BECH-REGROUP	
BE	bip173_segwit_programs_regroup_exactly	BECH-REGROUP	
BE	bip173_invalid_strings_are_refused	BECH-CHECKSUM	
BE	no_length_limit	BECH-CHECKSUM	BECH-REGROUP
BE	padding_must_be_short_and_zero	BECH-REGROUP	
BE	age_length_round_trips	BECH-REGROUP	
BE	mixed_case_is_refused	BECH-CHECKSUM	
BE	characters_outside_the_charset_are_refused	BECH-CHECKSUM	
X	rfc7748_5_2_single_evaluations	X-RFC7748	
X	rfc7748_5_2_iterated_once_and_a_thousand_times	X-RFC7748	
X	rfc7748_5_2_iterated_a_million_times	X-RFC7748	
X	rfc7748_6_1_diffie_hellman	X-RFC7748	
X	low_order_inputs_give_all_zero_output	X-LOW-ORDER	
X	non_canonical_u_is_reduced	X-RFC7748	
X	agrees_with_ring_on_random_keys	X-RFC7748	
A	round_trips_every_size_to_one_two_and_five_recipients	AGE-ROUNDTRIP	AGE-FORMAT
A	chunking_matches_the_spec	AGE-FORMAT	
A	header_is_well_formed	AGE-FORMAT	
A	wrong_identity_is_no_match_and_no_identity_too	AGE-NOMATCH	
A	armor_round_trips	AGE-ARMOR	AGE-ROUNDTRIP
A	armor_whitespace_bounds	AGE-ARMOR	
A	identity_and_recipient_spellings	AGE-KEYS	SEC-REDACT
A	spec_example_identity_spelling	AGE-KEYS	
A	generated_identities_differ_and_open_their_files	AGE-ROUNDTRIP	AGE-NOMATCH
A	encrypt_refuses_no_recipient_and_a_low_order_one	AGE-LOW-ORDER	
A	a_low_order_recipient_is_refused_by_parse_and_by_encrypt	AGE-LOW-ORDER	X-LOW-ORDER
A	a_stanza_whose_share_is_low_order_is_a_header_failure	AGE-LOW-ORDER	AGE-HEADER
A	scrypt_beside_another_stanza_is_a_header_failure	AGE-HEADER	
A	a_header_without_stanzas_or_past_the_bound_is_refused	AGE-HEADER	
A	truncations_and_bit_flips_only_fail	AGE-NO-PARTIAL	AGE-ARMOR
A	errors_name_no_value	AGE-ERRORS	
SB	debug_never_shows_the_bytes	SEC-REDACT	
SB	a_partial_read_error_wipes_the_accumulated_bytes	SEC-WIPE-READ	
JN	a_proof_binds_the_vault_the_key_and_the_host	JOIN-PROOF	
JN	blanks_around_a_password_do_not_count	JOIN-PROOF	
JN	a_line_break_inside_a_field_never_moves_a_proof_to_another_request	JOIN-PROOF	
JN	generated_passwords_are_strong_distinct_and_grouped	JOIN-STRENGTH	
JN	weak_passwords_are_refused_without_echoing_them	JOIN-STRENGTH	
JN	debug_never_shows_the_key	SEC-REDACT	
EN	a_signed_envelope_renders_exactly_and_parses_back	ENV-GRAMMAR	
EN	every_single_bit_flip_of_a_signed_envelope_is_refused	ENV-SIGNATURE	
EN	a_truncated_or_extended_envelope_is_refused	ENV-SIGNATURE	ENV-GRAMMAR
EN	the_wrong_pin_or_the_wrong_name_is_refused	ENV-PIN	
EN	an_endorsed_rotation_is_accepted_and_re_pins	ENV-PIN	
EN	an_endorsement_for_another_vault_or_by_another_key_is_refused	ENV-PIN	
EN	no_handoff_and_one_handoff_are_spelled_byte_for_byte_as_before_chains	ENV-GRAMMAR	
EN	a_chain_of_rotations_is_followed_from_every_key_the_vault_had	ENV-PIN	
EN	a_chain_with_a_broken_link_is_refused_from_every_key_before_the_break	ENV-PIN	
EN	the_handoff_chain_is_capped	ENV-BOUNDS	
EN	a_retired_key_signs_nothing_and_a_pin_to_it_follows_the_key_that_replaced_it	ENV-RETIRED	ENV-PIN
EN	a_pin_the_removed_admin_led_a_reader_to_gives_way_and_what_it_held_floors_nothing	ENV-RETIRED	ENV-FLOOR
EN	a_newer_version_carries_on_the_handoff_chain_the_reader_holds	ENV-FLOOR	
EN	an_admin_puts_back_the_links_a_rotation_by_a_build_before_chains_dropped	ENV-FLOOR	
EN	rollback_and_fork_are_refused_and_identical_bytes_are_unchanged	ENV-FLOOR	
EN	the_grammar_has_one_spelling	ENV-GRAMMAR	
EN	keys_and_signatures_must_be_canonical_and_sized	ENV-GRAMMAR	B64-CANON
EN	sizes_are_bounded	ENV-BOUNDS	
EN	sign_refuses_what_parse_would	ENV-GRAMMAR	
EN	a_fingerprint_is_five_groups_of_hex	ENV-NAMES	
EN	names_and_labels	ENV-NAMES	
T	an_unedited_document_renders_byte_for_byte	TXT-ROUNDTRIP	
T	machines_admins_and_entries_are_read	TXT-ROUNDTRIP	
T	host_headers_are_sorted_and_deduplicated	TXT-HOSTS	
T	heredocs_empty_and_one_empty_line	TXT-ROUNDTRIP	
T	entries_for_a_host_override_everyone_by_name	TXT-ENTRIES-FOR	
T	lockers_are_everyone_first_then_sorted_audiences_and_read_back	TXT-LOCKERS	
T	the_everyone_locker_exists_without_entries_and_sections_accumulate	TXT-LOCKERS	
T	a_locker_refuses_machines_and_admins_and_does_not_check_hosts	TXT-LOCKERS	
T	merge_for_reports_a_clash_between_two_host_audiences	TXT-ENTRIES-FOR	
T	structural_errors_name_their_line	TXT-VALIDATE	
T	machine_lines_are_checked	TXT-VALIDATE	
T	a_vault_needs_an_admin_but_a_locker_does_not	TXT-VALIDATE	
T	sizes_are_bounded	TXT-VALIDATE	
T	a_vault_names_as_many_sets_of_hosts_as_one_version_seals	TXT-VALIDATE	TXT-LOCKERS
T	entry_and_machine_names	TXT-NAMES	
T	set_entry_replaces_in_place_and_keeps_comments	TXT-SET	
T	set_entry_appends_to_the_first_section_or_makes_one	TXT-SET	
T	set_entry_spells_each_kind_of_value_so_it_reads_back	TXT-SET	
T	set_entry_refuses_what_would_break_the_rules_and_changes_nothing	TXT-SET	
T	unset_entry_removes_every_line_of_it	TXT-SET	
T	add_machine_appends_and_refuses_duplicates	TXT-MACHINES	
T	remove_machine_edits_headers_and_refuses_single_host_sections	TXT-MACHINES	
T	remove_machine_can_drop_its_own_sections	TXT-MACHINES	
T	a_section_its_entries_left_goes_with_the_machine	TXT-MACHINES	TXT-SET
T	the_last_admin_is_never_removed	TXT-MACHINES	
T	a_new_vault_explains_itself_and_parses	TXT-NEW	
T	no_error_or_debug_output_ever_holds_a_value	TXT-VALIDATE	SEC-REDACT
T	a_join_password_is_set_rendered_parsed_replaced_and_removed	TXT-JOIN	
T	weak_join_passwords_are_refused_without_quoting_them	TXT-JOIN	JOIN-STRENGTH
T	a_vault_holds_one_join_password_at_most	TXT-JOIN	
T	the_join_password_is_a_whole_vaults_only	TXT-JOIN	TXT-LOCKERS
T	an_open_vault_is_set_rendered_parsed_and_closed	TXT-JOIN	
T	an_open_line_is_checked_and_never_beside_a_password	TXT-JOIN	TXT-LOCKERS
T	a_code_only_key_is_named_once_and_dropped_when_it_is_let_in	TXT-JOIN	TXT-LOCKERS
T	no_machine_may_be_named_join	TXT-NAMES	
T	if_absent_entries_round_trip_in_every_spelling	TXT-IF-ABSENT	
T	if_absent_is_set_and_cleared_in_place_and_reaches_the_lockers	TXT-IF-ABSENT	TXT-LOCKERS
T	the_if_absent_modifier_has_one_place	TXT-IF-ABSENT	
T	a_new_vault_takes_a_join_password_and_if_absent_entries	TXT-NEW	TXT-JOIN, TXT-IF-ABSENT
T	anchored_entries_are_spelled_parsed_and_reach_the_lockers	TXT-ANCHOR	
T	an_anchor_is_another_if_absent_entry_its_receivers_all_get	TXT-ANCHOR	
TR	a_url_parses_with_and_without_a_pin_in_every_base64_spelling	TR-SOURCE	
TR	paths_parse_and_anything_else_is_refused_with_a_hint	TR-SOURCE	
TR	the_vault_name_is_the_last_segment_or_the_stem	TR-SOURCE	ENV-NAMES
TR	query_and_join_urls_compose	TR-SOURCE	
TR	a_reason_is_one_short_printable_line	TR-REASON	
TR	a_path_is_fetched_and_published_atomically_and_world_readably	TR-PATH	
TR	a_path_takes_only_a_newer_version_of_its_vault	TR-PATH	ENV-FLOOR
TR	a_publish_to_a_path_another_publisher_holds_is_refused	TR-PATH	
TR	a_refusal_names_the_status_and_a_sanitized_reason	TR-REASON	
TR	a_malformed_answer_is_quoted_as_one_short_printable_line	TR-REASON	
TR	a_long_poll_asks_after_the_seq_and_reads_no_newer	TR-LONGPOLL	
TR	join_lines_are_validated	TR-JOIN-LINE	
FS	a_private_dir_is_made_and_narrowed_to_0700	FS-PRIVATE-DIR	
FS	a_symlinked_dir_is_refused	FS-PRIVATE-DIR	
FS	private_reads_refuse_group_or_other_bits_and_symlinks	FS-READ	
FS	a_read_fills_one_buffer_sized_from_the_files_length	FS-READ	
FS	reads_are_bounded_and_a_fifo_never_blocks	FS-READ	
FS	rename_noreplace_refuses_an_existing_target	FS-NOREPLACE	
FS	a_rename_whose_unlink_fails_undoes_its_link	FS-NOREPLACE	
FS	file_ids_tell_one_file_from_two	FS-FILE-ID	
FS	wipe_removes_files_and_only_the_link_of_a_link	FS-WIPE	
FS	wipe_zeroes_the_bytes_under_every_name	FS-WIPE	
FS	a_judged_wipe_never_touches_a_file_renamed_over_the_one_it_judged	FS-WIPE	
FS	write_new_fills_only_an_empty_name	FS-NOREPLACE	
M	target_paths	MAT-PATHS	TXT-NAMES
M	digests_are_lowercase_hex_sha256	MAT-PATHS	
M	a_first_sync_writes_0600_files_and_narrows_the_directory	MAT-WRITE	
M	a_pre_existing_different_file_is_moved_aside_with_its_content	MAT-ASIDE	
M	an_equal_file_is_adopted_and_narrowed	MAT-WRITE	
M	narrowing_never_follows_a_link_swapped_in_after_the_look	MAT-SYMLINK	MAT-WRITE
M	dropping_an_adopted_entry_leaves_the_machines_own_file	MAT-ASIDE	
M	a_new_value_moves_an_adopted_file_aside_first	MAT-ASIDE	
M	an_adoptable_file_is_never_announced_as_a_write	MAT-WRITE	
M	a_change_overwrites_what_the_vault_wrote_but_saves_a_local_edit	MAT-ASIDE	
M	a_taken_aside_name_gets_a_numbered_suffix	MAT-PATHS	MAT-ASIDE
M	a_dropped_entry_is_removed_only_while_unchanged	MAT-ASIDE	
M	a_dropped_file_whose_unlink_is_refused_keeps_its_bytes_for_the_next_pass	MAT-ASIDE	FS-WIPE
M	a_file_the_vault_never_wrote_is_never_removed	MAT-ASIDE	
M	a_home_path_target_gets_private_parents	MAT-WRITE	
M	a_symlink_at_the_target_is_replaced_never_followed	MAT-SYMLINK	
M	a_directory_in_the_way_fails_that_entry_only	MAT-ISOLATE	
M	a_dropped_name_whose_directory_is_now_a_file_is_forgotten	MAT-ASIDE	
M	two_names_for_one_file_are_refused_where_the_volume_makes_them_one	MAT-ONE-FILE	FS-FILE-ID
M	a_second_name_for_a_placed_file_is_refused_whatever_its_spelling	MAT-ONE-FILE	
M	a_dropped_name_for_another_entrys_file_is_never_wiped	MAT-ONE-FILE	
M	an_entry_respelled_by_case_keeps_what_the_vault_wrote	MAT-ONE-FILE	
M	an_if_absent_entry_fills_a_gap_but_never_touches_a_machines_own_file	MAT-IF-ABSENT	
M	an_if_absent_file_the_vault_wrote_is_the_machines_own_once_changed	MAT-IF-ABSENT	
M	an_if_absent_entry_cannot_claim_a_file_another_entry_placed	MAT-IF-ABSENT	MAT-ONE-FILE
M	a_machines_own_machine_toml_keeps_the_vaults_key_and_profile_away	MAT-ANCHOR	
M	a_pass_cut_short_after_its_writes_keeps_the_anchored_set	MAT-RESUME	MAT-ANCHOR
M	a_temp_a_pass_cut_short_left_holding_its_value_is_wiped	MAT-RESUME	FS-WIPE
M	a_cut_temp_that_names_another_file_loses_only_its_name	MAT-RESUME	FS-WIPE
M	a_gap_taken_while_the_pass_writes_fails_so_the_next_pass_takes_the_set_back	MAT-ANCHOR	MAT-IF-ABSENT
M	a_file_the_vault_wrote_changed_while_the_pass_writes_fails_so_the_next_pass_takes_the_set_back	MAT-ANCHOR	MAT-IF-ABSENT
M	an_anchored_set_fills_a_machine_that_has_none_of_it_whole	MAT-ANCHOR	
M	a_profile_the_vault_filled_beside_a_machines_own_key_is_taken_back	MAT-ANCHOR	
M	a_key_and_machine_toml_the_vault_filled_beside_a_machines_own_profile_are_taken_back	MAT-ANCHOR	
M	a_file_the_vault_wrote_beside_an_anchor_it_cannot_look_at_stays_the_vaults	MAT-ANCHOR	
M	a_machines_own_key_keeps_the_vaults_profile_and_machine_toml_away	MAT-ANCHOR	
M	a_machines_own_profile_keeps_the_vaults_key_away	MAT-ANCHOR	
M	an_anchor_the_machine_cannot_judge_fills_nothing	MAT-ANCHOR	
M	a_symlinked_directory_on_the_way_is_refused_never_followed	MAT-SYMLINK	
M	aterms_own_directory_by_another_spelling_is_refused	MAT-SYMLINK	TXT-NAMES
M	a_symlinked_secrets_dir_fails_its_entries_and_spares_the_rest	MAT-SYMLINK	MAT-ISOLATE
M	an_entry_behind_a_symlinked_directory_is_refused_before_any_look	MAT-SYMLINK	
M	a_dropped_name_for_a_file_a_refused_entry_names_is_kept	MAT-ONE-FILE	
M	an_invalid_name_fails_that_entry_only	MAT-ISOLATE	
M	moved_aside_names_stay_within_name_max_and_apart	MAT-PATHS	
M	reports_carry_names_never_values	MAT-REPORT	
SV	the_first_envelope_is_trusted_on_first_use_and_served_with_an_etag	SRV-TOFU	
SV	a_store_that_holds_no_vault_says_a_mac_in_the_vault_puts_its_copy_here	SRV-TOFU	
SV	refused_puts_name_why_and_leave_the_store_unchanged	SRV-PUT	
SV	a_same_seq_with_other_bytes_is_refused_as_not_newer	SRV-PUT	ENV-FLOOR
SV	an_endorsed_handoff_moves_the_pin	SRV-PUT	ENV-PIN
SV	a_configured_pin_holds_for_a_vault_not_yet_held	SRV-TOFU	
SV	a_fresh_store_pinned_to_the_first_key_takes_a_vault_three_rotations_on	SRV-PUT	ENV-PIN
SV	a_rotation_that_drops_a_link_of_the_held_chain_is_refused	SRV-PUT	ENV-FLOOR
SV	a_store_the_removed_admin_fed_takes_the_vault_once_its_build_retires_the_key	SRV-PUT	ENV-RETIRED
SV	joins_asked_under_a_key_that_gave_way_give_way_to_new_ones	SRV-JOINS	ENV-RETIRED
SV	a_retired_key_signs_nothing_the_server_takes	SRV-PUT	ENV-RETIRED
SV	a_long_poll_answers_at_once_when_newer_and_wakes_on_a_put	SRV-LONGPOLL	
SV	a_long_poll_ends_with_204_after_the_hold_or_at_stop	SRV-LONGPOLL	
SV	join_requests_queue_deduplicate_and_validate	SRV-JOINS	TR-JOIN-LINE
SV	the_join_queue_is_bounded	SRV-JOINS	
SV	the_server_restarts_from_its_store_and_expires_old_joins	SRV-JOINS	
SV	oversize_heads_and_bodies_are_refused	SRV-HTTP	
SV	an_expect_continue_put_is_answered_100_then_204	SRV-HTTP	
SV	bad_paths_methods_and_queries_are_refused	SRV-HTTP	
SV	a_browsers_request_is_refused_before_anything_is_read_or_queued	SRV-HTTP	SRV-JOINS
SV	the_children_argv_is_exact	SRV-ARGV	
SV	expiry_keeps_a_week_either_side_of_the_clock	SRV-JOINS	
SV	one_address_holds_at_most_16_connections_and_the_next_reads_503	SRV-LIMITS	
SV	at_most_4_put_bodies_are_read_at_once	SRV-LIMITS	
SV	another_machine_reads_one_put_at_a_time_and_the_last_slot_is_this_machines	SRV-LIMITS	
SV	a_join_asked_again_keeps_its_place_moves_its_time_hourly_and_a_new_proof_replaces_it	SRV-JOINS	
SV	a_put_body_that_stalls_or_outruns_its_deadline_is_cut_with_408_and_frees_its_slot	SRV-LIMITS	
SV	another_address_can_neither_replace_nor_hide_a_queued_join_request	SRV-JOINS	TR-JOIN-LINE
SV	a_key_two_addresses_ask_for_is_let_in_once	CL-APPROVER	FLOW-OPEN
SV	one_address_queues_at_most_4_joins_and_the_listing_never_names_it	SRV-JOINS	
SV	a_publish_answers_the_queued_joins_and_one_still_waiting_counts_when_it_asks_again	SRV-JOINS	
SV	a_key_changes_its_request_at_most_every_10_s_and_the_log_notes_it_hourly	SRV-JOINS	
SV	a_clock_stepped_back_holds_off_no_change_and_drops_no_request	SRV-JOINS	
SV	a_join_waiting_on_its_store_write_holds_up_no_read_or_publish	SRV-LIMITS	
SV	the_join_store_keeps_each_address_where_the_listing_never_shows_it	SRV-JOINS	
SV	a_first_version_is_taken_only_from_this_machine_or_under_a_pin	SRV-TOFU	
SV	this_machine_is_loopback_and_its_own_addresses	SRV-TOFU	
SV	a_hold_too_long_to_add_ends_with_a_newer_version	SRV-LONGPOLL	
SV	a_replaced_program_stops_the_server_and_ends_it_cleanly	SRV-REPLACED	
SV	a_raised_stop_flag_stops_the_server_and_ends_it_cleanly	SRV-REPLACED	
SV	letting_the_servers_children_go_ends_them	SRV-REPLACED	
SV	a_client_that_never_finishes_its_head_is_cut_at_the_deadline	SRV-LIMITS	
SV	an_answer_read_too_slowly_is_cut_at_its_deadline	SRV-LIMITS	
CL	a_concurrent_machine_key_wins_without_replacing_any_existing_bytes	CL-KEY-RACE	FS-NOREPLACE
CL	the_alab_vault_is_known_by_its_keys_whatever_url_reaches_it	CL-ALAB	ALAB-PINS
CL	a_reader_the_removed_admin_led_off_follows_the_vault_once_its_build_retires_the_key	CL-RETIRED	ENV-RETIRED, CL-NEWER
CL	an_admin_gives_the_vault_back_to_a_store_the_removed_admin_fed	CL-RETIRED	SRV-PUT
CL	a_pass_cut_short_after_its_writes_leaves_its_files_the_vaults	CL-CUT-PASS	MAT-RESUME
CL	a_pass_cut_short_leaves_no_entry_it_keeps_away_pending	CL-CUT-PASS	MAT-ANCHOR
CL	a_cut_pass_resumes_as_the_vaults_and_writes_only_what_it_pended	CL-CUT-PASS	MAT-SYMLINK
CL	a_pass_cut_short_is_applied_again_at_the_same_version	CL-CUT-PASS	
CL	a_target_that_changed_since_the_look_is_not_written_unannounced	CL-CUT-PASS	
CL	a_membership_with_no_digest_floors_on_its_cache_unapplied	CL-CUT-PASS	ENV-FLOOR
CL	a_request_for_a_vault_this_machine_left_leaves_its_state_alone	CL-STATE	
CL	a_large_manifest_reads_back_and_one_past_the_bound_is_never_written	CL-STATE	
CL	a_join_to_another_vault_forgets_the_proof_made_for_the_last	CL-STATE	JOIN-PROOF
CL	an_admin_gives_a_path_its_copy_back_over_a_version_it_does_not_verify	CL-RETIRED	TR-PATH
CL	an_admin_puts_back_the_link_a_rotation_by_a_build_before_chains_dropped	CL-CHAIN	ENV-FLOOR
CL	an_unpublished_edit_and_unclaimed_files_survive_later_edits	CL-EDIT	
CL	a_vault_too_large_to_seal_is_refused_saying_why	CL-SEAL	ENV-BOUNDS
CL	a_pass_that_leaves_an_entry_failed_backs_off_and_a_clean_one_starts_again	CL-RETRY	
CL	a_pass_that_errs_is_tried_again_within_a_minute	CL-RETRY	CL-LOST
CL	a_wait_after_an_error_ends_once_the_network_changes	CL-RETRY	
CL	the_subscriber_repairs_a_changed_file_once_per_version_so_readiness_returns	CL-REPAIR	
CL	pin_admission_follows_verified_rotation_and_compiled_replacement	CL-PIN	ENV-RETIRED
CL	a_key_the_chain_names_before_a_link_that_does_not_verify_is_no_predecessor	CL-PIN	ENV-PIN
CL	the_approver_remembers_only_what_is_still_listed	CL-APPROVER	FLOW-JOIN-PASSWORD
CL	a_join_cut_off_before_its_request_is_queued_is_asked_for_by_the_next_pass	CL-REASK	CL-STATE
CL	a_reask_begun_on_the_vault_left_never_sends_the_new_ones_proof_there	CL-REASK	CL-STATE
CL	a_reask_asks_under_the_name_source_toml_holds_now	CL-REASK	JOIN-PROOF
CL	a_join_under_another_name_drops_the_old_names_proof	CL-REASK	JOIN-PROOF
CL	a_reask_never_puts_back_a_proof_a_join_replaced	CL-REASK	CL-STATE
CL	a_request_the_server_lost_with_its_store_is_asked_again_once_the_vault_is_back	CL-REASK	CL-LOST
CL	a_machine_in_the_vault_puts_back_a_vault_its_server_lost	CL-LOST	SRV-TOFU
CL	a_put_back_the_source_keeps_no_more_backs_off_and_is_logged_once	CL-LOST	CL-RETRY
CL	a_server_that_does_not_pin_the_vault_refuses_its_copy_and_the_pass_errs	CL-LOST	SRV-TOFU
CL	a_machine_kept_out_or_without_a_verified_copy_puts_nothing_back	CL-LOST	CL-REASK
CL	a_machine_in_the_vault_writes_nothing_to_a_path_that_holds_no_vault	CL-LOST	TR-PATH
CL	an_admin_of_a_vault_closed_to_joins_puts_back_a_vault_its_server_lost	CL-LOST	CLI-APPROVER
CL	a_machine_puts_its_newer_copy_back_over_an_older_one_a_server_took	CL-NEWER	TR-LONGPOLL
SE	a_job_is_set_up_only_for_the_logins_own_home	SVC-OWN-HOME	
SE	a_home_that_is_not_the_logins_has_no_job	SVC-OWN-HOME	
SE	labels_and_paths_sit_where_launchd_systemd_and_the_design_say	SVC-PATHS	
SE	the_plist_names_the_job_keeps_it_alive_and_escapes_every_string	SVC-PLIST	
SE	a_log_elsewhere_writes_no_environment	SVC-PLIST	
SE	xml_escaping_covers_the_five_entities	SVC-PLIST	
SE	the_plist_is_well_formed_by_plutil	SVC-PLIST	
SE	a_systemd_word_reads_back_as_itself	SVC-SYSTEMD	
SE	the_unit_runs_the_quoted_argv_and_always_restarts	SVC-SYSTEMD	
SE	the_launchctl_and_systemctl_argv_are_plain	SVC-ARGV	
SE	the_pid_is_the_services_own_line	SVC-ARGV	
SE	systemctl_is_found_by_absolute_path_or_refused_by_name	SVC-SYSTEMD	
SE	a_build_in_a_target_directory_is_never_a_services_program	SVC-PROGRAM	
SE	a_bundled_executable_runs_from_its_bundle_and_a_build_is_refused	SVC-PROGRAM	
SE	the_test_binary_itself_is_refused_as_a_build	SVC-PROGRAM	
SE	a_tagged_cache_directory_is_a_build_whatever_its_name	SVC-PROGRAM	
SE	a_copy_that_goes_away_is_never_a_services_program	SVC-PROGRAM	
SE	writing_the_plist_makes_a_0644_file_and_a_private_log_directory	SVC-PLIST	
SE	a_unit_file_is_refused_for_what_it_cannot_name	SVC-SYSTEMD	
CLI	bare_verb_is_status	CLI-PARSE	
CLI	join_takes_one_source_and_its_switches	CLI-PARSE	
CLI	create_takes_one_source_and_its_switches	CLI-PARSE	
CLI	set_never_takes_a_value_on_the_command_line	CLI-SET	
CLI	a_piped_value_loses_its_line_end_only_when_it_is_one_line_of_text	CLI-SET	TXT-SET
CLI	set_with_names_the_anchor_of_an_if_absent_entry	CLI-SET	TXT-ANCHOR
CLI	setting_an_entry_again_keeps_how_it_fills_unless_told	CLI-SET	
CLI	the_vaults_first_anchor_waits_for_the_fleet	CLI-SET	
CLI	approve_takes_codes_never_host_names	CLI-PARSE	TR-JOIN-LINE
CLI	a_key_is_never_approved_under_a_name_it_was_not_given	CLI-PARSE	TXT-NAMES
CLI	approve_takes_no_key_listed_under_two_names	CLI-APPROVE	SRV-JOINS
CLI	password_actions	CLI-PARSE	
CLI	serve_flags_are_checked_before_anything_runs	CLI-PARSE	
CLI	serve_advertises_the_default_vaults_name_unless_told	CLI-PARSE	ALAB-NAME
CLI	a_relative_store_is_under_the_home_directory	CLI-PARSE	
CLI	a_rotation_of_the_alab_vault_names_both_lines_the_next_release_changes	CLI-WORDS	ALAB-PINS
CLI	server_on_passes_serve_flags_through	CLI-PARSE	
CLI	unknown_commands_and_options_are_usage_errors	CLI-PARSE	
CLI	a_waiting_machine_is_told_how_a_mistyped_join_password_is_fixed	CLI-WORDS	
CL	only_a_request_with_a_proof_was_asked_with_a_password	CLI-WORDS	
CLI	opening_never_offers_a_machine_let_go	FLOW-OPEN	TXT-JOIN
CLI	an_editor_that_fails_on_the_published_text_leaves_no_draft	CLI-EDIT	CL-EDIT
CLI	a_draft_reopened_unchanged_on_a_vault_that_moved_publishes_only_when_the_person_says_so	CLI-EDIT	FLOW-EDIT
CLI	letting_a_machine_go_names_everything_it_could_read	CLI-WORDS	TXT-LOCKERS
CLI	status_names_a_machine_of_a_signed_vault_by_its_code	CLI-WORDS	
CLI	the_approver_is_put_in_only_where_it_does_not_run	CLI-APPROVER	
CLI	a_command_whose_approver_does_not_run_says_what_starts_one	CLI-APPROVER	CLI-WORDS
CLI	a_vault_at_a_path_says_a_machine_joins_it_by_key	CLI-APPROVER	CLI-WORDS
CLI	create_puts_in_the_approver_unless_told_not_to	CLI-APPROVER	
CLI	a_create_whose_own_sync_fails_still_puts_the_approver_in	CLI-APPROVER	
CLI	a_vault_that_lets_machines_in_gets_an_approver_and_status_says_whether_it_runs	CLI-APPROVER	FLOW-OPEN
CLI	a_code_typed_for_a_machine_reaches_the_key_that_has_it	CLI-WORDS	FLOW-EDIT
F	a_lan_vault_from_create_to_revoke	FLOW-LIFECYCLE	
F	the_server_and_every_reader_refuse_a_key_the_vault_never_endorsed	FLOW-ROTATION	ENV-PIN, SRV-PUT
F	removing_an_admin_rotates_the_key_and_every_machine_re_pins	FLOW-ROTATION	
F	a_signed_vault_at_a_path_needs_no_server	FLOW-LIFECYCLE	TR-PATH
F	a_plain_text_file_at_a_path_is_read_as_is	FLOW-LIFECYCLE	
F	a_machine_that_knows_the_join_password_is_let_in_by_the_admins_subscriber	FLOW-JOIN-PASSWORD	JOIN-PROOF
F	an_open_vault_lets_in_every_machine_that_asks_with_no_password	FLOW-OPEN	
F	an_edit_saved_over_an_open_vault_that_let_a_machine_go_keeps_it_out	FLOW-OPEN	FLOW-EDIT
F	letting_a_machine_go_renews_the_join_password_so_it_is_not_let_in_again	FLOW-JOIN-PASSWORD	FLOW-OPEN
F	a_join_whose_subscriber_applied_the_approval_first_is_done	FLOW-JOIN-PASSWORD	
F	removing_an_admin_by_any_publish_rotates_and_later_versions_carry_the_handoff	FLOW-ROTATION	
F	every_version_carries_the_whole_chain_so_the_first_key_still_admits	FLOW-ROTATION	ENV-PIN
F	an_admin_puts_back_a_vault_its_server_lost	FLOW-ROTATION	
F	a_probe_names_a_vault_only_under_its_signer	FLOW-PROBE	
F	removing_an_admin_renews_the_join_password_it_read	FLOW-JOIN-PASSWORD	
F	the_approver_judges_a_corrected_request_at_once_and_rests_once_all_are_judged	FLOW-JOIN-PASSWORD	
F	a_taken_name_of_62_or_63_characters_still_gets_a_free_one	FLOW-EDIT	TXT-NAMES
F	a_machine_admitted_under_another_name_is_named_by_its_code	FLOW-EDIT	FLOW-JOIN-PASSWORD
F	a_machine_asking_for_another_machines_code_as_its_name_is_let_in_under_another	FLOW-EDIT	FLOW-OPEN
F	an_edit_saved_over_a_machine_admitted_meanwhile_keeps_it	FLOW-EDIT	
F	an_edit_that_lands_after_the_approver_keeps_the_machine_and_the_password	FLOW-EDIT	
F	a_revoked_machine_that_joins_again_waits_and_asks_again	FLOW-JOIN-PASSWORD	
F	a_join_beside_another_vaults_leftover_state_starts_afresh	FLOW-JOIN-PASSWORD	
F	a_join_with_the_password_says_how_to_fix_a_mistyped_one_before_it_waits	FLOW-JOIN-PASSWORD	CLI-WORDS
F	an_edit_at_a_path_that_lands_after_another_admin_keeps_the_machine_it_admitted	FLOW-EDIT	
F	an_edit_saved_over_another_admins_changes_to_who_is_in_keeps_them	FLOW-EDIT	TXT-MACHINES
F	an_edit_that_lands_after_a_revocation_keeps_the_machine_out	FLOW-EDIT	FLOW-JOIN-PASSWORD
F	a_probe_answered_by_something_else_is_not_the_vault	FLOW-PROBE	
CC	every_applicable_cctv_vector_has_its_exact_class	AGE-CCTV	
CC	the_subset_covers_every_class	AGE-CCTV	
CC	success_vectors_round_trip_through_our_armor	AGE-CCTV	AGE-ARMOR
```

## 3. Coverage

- Every test in the crate is in §2's map.
- `X::rfc7748_5_2_iterated_a_million_times` is `#[ignore]`d by default (it is the RFC's longest
  vector); the others run in every test pass.
- The flows (`F`) and the client's tests run the real code against a scratch home, an in-process
  server and a path; no test reaches launchd, systemd, mDNS or the network beyond loopback.

## 4. Tests that are not spec samples

None. The text module's test-only Bech32 helper has no test of its own: every fixture it builds
passes through `Document::parse`, which checks each recipient, so a wrong helper fails those tests.

## 5. Implied specs that look wrong, or disagree with the design

- X-RFC7748 is proved for the C the module transcribes (ePrint 2021/428), not for this Rust: the
  transcription, Rust's semantics and the compiler are held only by vectors and the differential
  suite, and constant time is a property of the code's shape that no tool here checks.
- TR-LONGPOLL's `wait` bounds the request, but not the name lookup before it: aterm-http resolves a host
  with `to_socket_addrs`, so a slow `.local` lookup can hold a request past `wait` (the module says so).
- ALAB-PINS: a release that retires a key must also move `ALAB_SIGNER`; until a build carries both, a
  new Mac trusts what the removed admin signs with the old key (the constant's doc says so).
- MAT-ASIDE holds for removal (FS-WIPE judges and zeroes one descriptor) but not quite for an
  overwrite: the file is judged, then a temp is synced and renamed over it, so a tool's own rename
  (git's `config.lock`) that lands in between is replaced without a copy in `.replaced/`. Closing it
  needs an atomic exchange (`renamex_np` with `RENAME_SWAP`, `renameat2` with `RENAME_EXCHANGE`) of
  the synced temp with the target, then a judgment of whatever lands at the temp's name (moved to
  `.replaced/` unless it is the vault's). `forbid(unsafe_code)` does not rule that out: a safe
  wrapper could live in aterm-uds, as aterm-update's `sys::rename_swap` and atpkg's lock swap do in
  their crates. It stays open on purpose, to avoid that code: a swap primitive with a fallback
  where the volume has no exchange, and a judgment of every kind that can land there (a file, a
  link, a directory). The window is one sync, and only a file the vault wrote and is changing (a
  plain entry's, or an ` if-absent` one it still owns) is exposed.

## 6. Proof order (easiest, high value first)

1. ENV-NAMES, TXT-NAMES, TR-JOIN-LINE, B64-CANON: byte predicates and canonical codecs; the names reach
   file system paths, so these guard the path boundary.
2. BECH-CHECKSUM, BECH-REGROUP, ENV-GRAMMAR, ENV-BOUNDS: parsers with one spelling.
3. X-RFC7748, X-LOW-ORDER: the ladder over i64 limbs, the one place a proof about the code (not the C)
   is worth most; it needs loops (gap 7) and wide arithmetic.
4. ENV-PIN, ENV-RETIRED, ENV-FLOOR: the trust decisions, a chain walk over key slices once the
   signature check is an assumed primitive.
5. TXT-*, AGE-* over the codec primitives.
6. FS-*, MAT-*, SRV-*, CL-*, SVC-*, CLI-*, FLOW-*: effectful and concurrent (gaps 8, 13); they stay
   tests, and MAT-ANCHOR and CL-CUT-PASS are the first candidates for a `ty_model!` of the manifest across
   passes.
