<!-- Copyright 2026 Andrew Yates -->
<!-- SPDX-License-Identifier: Apache-2.0 -->

# aterm-http: spec recovery from its 106 unit tests

> Status 2026-10-16, after the merge with the tests-and-gates audit: the rows naming 18 tests it
> deleted and 1 spec whose functions it deleted are gone, and the spec rows no longer cite those
> tests. Since then the strict PEM decoder is deleted, and with it the spec relating it to the
> lossy one, and the 2 tests §4 marked **delete** are deleted with their rows. 33 specs remain and
> §2 maps 88 tests: the result line, the kind table, the gap histogram and §3 count those. The
> per-file census of the 106 tests read was taken before.

> Status 2026-10-20, agh milestone 1 (WP1): nine specs join for the methods, streamed bodies,
> header iteration, phases, idle deadlines, bounded DNS and secret headers of the agh design §5
> (`method-head-framing`, `bodiless-no-content-length`, `caller-accept`, `header-iteration`,
> `streamed-body-framing`, `phase-classification`, `idle-deadline`, `dns-bounded`,
> `secret-header-scrub`), added with 47 tests (`tests/golden_post.rs`, `tests/methods.rs`,
> `tests/streaming.rs`, one in `tests/tls_stream.rs`, seven in `src/stream.rs` and four in
> `src/client.rs`). All but six map to the nine: three map to `render-request-exact`; two,
> `src/stream.rs`'s `an_answer_sent_before_a_reset_is_read_and_then_the_reset` and
> `tls_stream::an_answer_sent_before_the_peer_closed_over_a_failed_write_is_read`, to
> `stream-admit-before-every-syscall`; and one,
> `streaming::an_answer_sent_while_the_body_was_still_going_out_is_read`, to
> `send-pipeline-order`.
> Five older tests that no row named are mapped too: the three in `src/connect.rs` (to
> `idle-deadline`, which now states `connect::connect_timeout`'s contract),
> `tls_stream::a_dribbled_handshake_cannot_outlive_the_global_deadline` and
> `pem::the_lossy_parser_skips_an_aux_info_block`. **42 specs** now cover **140 tests**, every test
> the crate runs: the result line, the kind table, the gap histogram and §3 count those.

> Status 2026-10-21, main merged into agh: main's GET and PUT builders (2026-10-20) are
> `Client::get` and `Client::put` over agh's `Client::request`, and its two tests join.
> `loopback::a_get_and_a_put_put_their_methods_on_the_wire` maps to `send-pipeline-order`, and
> `a_peer_that_closes_unanswered_is_a_transport_failure` (`src/client.rs`) to
> `read-head-skips-interim`, whose statement now names the EOF before a first byte. Main's
> `a_bodiless_get_names_no_length_and_every_other_request_does` is deleted with its row: it drove
> the old `render_request(…, body_len)` and asserted that a GET handed a body frames it, where agh
> refuses that body before any connection (`bodiless-no-content-length`), and
> `methods::get_and_head_carry_no_content_length` and
> `methods::post_patch_and_put_carry_a_zero_length_when_empty` hold the rest of it. **42 specs**
> cover **142 tests**.

> Status 2026-10-16: this is the crate's migration worklist. Each of the 33 specs of that date
> (42 now) is proved through Trust/Clean about the compiled Rust fn, and then the tests it
> subsumes are deleted. The 4 tests §4 listed outside any spec are all deleted.

Source: `crates/aterm-http` (read only). I read every `#[test]` (src/client.rs 28, src/pem.rs 11,
src/proxy.rs 16, src/stream.rs 4, src/tls.rs 2, src/uri.rs 10, src/verifier/tests.rs 10,
src/verifier/unix.rs 7, tests/loopback.rs 11, tests/signature_methods.rs 3, tests/tls_stream.rs 2,
and 2 in a trust-config test file deleted 2026-10-16 = 106) and the code each one reaches.
`tests/http_suite.rs` has no `#[test]` fn: it only declares modules, and a grep counts 107 because
of the `#[test]` in its doc comment.
Nothing was compiled or run. The claims below come from reading the code.

Result: **42 specs** cover **142 tests**. 36 specs are the primary spec of at least one test. Four
are lemmas that the others need and are only secondary specs of tests: `uri-render`,
`proxy-target-debug-redacts`, `http-number-grammar` and `response-accessors`.
`client-config-roots-fail-closed` is now only a secondary spec too, and
`provider-advertises-modern-schemes` has no test left.
**No test outside a spec is left** (§4): every test the crate runs (2026-10-21: 89 unit tests and
53 in the suite) has a row in §2.

## 0. Notation used in the statements

Function paths are relative to the crate (`uri::Uri::parse` means `aterm_http::uri::Uri::parse`).

- `ci(a,b)`: ASCII case-insensitive equality (`eq_ignore_ascii_case`).
- `tchar(b)`: `b` is ASCII alphanumeric or one of ``!#$%&'*+-.^_`|~`` (the RFC 9110 token character).
- `vchar_ok(b)`: `b == 0x09`, or `0x20 <= b` and `b != 0x7F`. Bytes `0x80..=0xFF` are allowed.
- `val_r(s)`: the value of digit string `s` in radix `r`, as an unbounded natural number.
- `bracket(h)`: `"[" + h + "]"` when `h` contains `':'`, otherwise `h`. `dec(n)`: the decimal rendering of `n`.
- **URL split.** For the text after the scheme prefix (`rest`): `A` = `rest` up to its first `'/'`, `'?'`
  or `'#'` (all of `rest` if there is none), and `tail` = what follows `A`. `F` = `tail` up to its first
  `'#'`. `origin(F)` = `"/"` if `F` is empty, `"/" + F` if `F` starts with `'?'`, otherwise `F`.
- **PEM scan.** `BEGIN = "-----BEGIN "`, `END = "-----END "`, `SUF = "-----"`. `Scan(text)` repeats:
  find the first `BEGIN` in the remaining text; the label `L` runs to the next `SUF`; the body runs to
  the first `END + L + SUF`; resume after that marker. `W(body)` = the body with every Unicode
  whitespace char removed. `ScanLossy` is the same scan, except that it **stops** (yielding nothing
  more) at a block whose begin line has no `SUF` or whose end marker is missing.
- `EnvText(env,n)` = `env.var_os(n)`, converted lossily (each bad sequence becomes U+FFFD) and trimmed;
  `None` if unset or empty after trimming.
- **Clock model.** `Instant::now()` is a monotone abstract clock. `Deadline::after(b)` taken at `t0`
  sets `at = t0 + b`.
- `Obs(stream)`: the sequence of socket syscalls (`connect`, `set_*_timeout`, `read`, `write`, `flush`)
  issued on the `TcpStream`, including those rustls issues through `Bounded`.
- `FS`: an abstract filesystem model (`exists`, `read_dir`, `metadata`, `read_to_string`) for the
  Unix certificate search.
- `V(anchors, ee, ims, n, now)`: an X.509 server-path-validation model (RFC 5280 path building and
  signatures, the validity window, SAN name/IP match, CA basic constraints, serverAuth EKU). It is
  authored on the Lean side and has no Rust counterpart today.
- `blocking_gaps` uses the numbers of the gap table in
  `AUDIT-trust-for-test-migration-2026-10-15.md` (historical internal audit)
  §7. Gap 0 (admitting a seal that publishes Clean rows to the machine's `trust` channel) is a
  prerequisite for every row, so it is listed on all 42. 1 is non-scalar carriers, 2 is `&self`/`&mut`
  views, 3 is E6 body widening, 4 is callee-ensures composition, 5 is quantified clauses, 7 is loops,
  8 is effects/FFI, 10 is model-to-code refinement, 11 is generics, 12 is cross-crate citation
  (`aterm_codec`, rustls, webpki, ring), and 13 is closures, threads and `OnceLock`.

## 1. Specs (TSV)

Columns: `id`, `kind`, `functions`, `statement`, `subsumes (primary tests; secondary in §2)`,
`difficulty (Rust features the proof must handle)`, `blocking_gaps`.

```tsv
id	kind	functions	statement	subsumes	difficulty	blocking_gaps
uri-parse-accept-set	contract	uri::Uri::parse, uri::{strip_prefix_ascii_ci, split_authority, parse_port, parse_port_digits, host_is_plausible}	∀ text: Uri::parse(text).is_some() ⇔ (a) 1 ≤ text.len() ≤ 8192; (b) no char c has c.is_control() or c == ' '; (c) text starts with "https://" or "http://" under ci (https tried first), rest = what follows; (d) A is non-empty and has no '@'; (e) A is either the bracket form "[" h "]" T (h = text before the first ']', non-empty, parses as Ipv6Addr; T empty or ":" P) or the plain form with at most one ':', host H before it non-empty with every byte ASCII alphanumeric or one of - . _ ~ %, and port text P after it; P, when present, is non-empty ASCII digits with 1 ≤ val_10(P) ≤ 65535. Corollaries: userinfo is rejected, never stripped; CR, LF or NUL anywhere, a non-http(s) scheme, an unbracketed IPv6 literal and a port that is 0, empty or out of range are all rejected.	userinfo_is_rejected_rather_than_stripped, control_characters_and_spaces_cannot_smuggle_a_header, only_http_and_https_are_accepted, malformed_authorities_are_rejected	&str slicing, chars()/bytes() with closures, find with a char-array pattern, split(':'), std parse::<Ipv6Addr> and ::<u16>, Option ? chaining	0,1,3,5,7,13
uri-parse-fields	contract	uri::Uri::parse, uri::origin_form, uri::Uri::{scheme, host, port, path_and_query, authority_as_written}	∀ text with Uri::parse(text) = Some(u), using the decomposition of uri-parse-accept-set: u.scheme() = Https ⇔ the https:// prefix matched (ci); u.authority_as_written() = A byte for byte, brackets included; u.host() = h (unbracketed) in the bracket form, H in the plain form; u.port() = val_10(P) if P is present, else u.scheme().default_port(), and explicit_port ⇔ P is present; u.path_and_query() = origin(F) byte for byte (no percent-decoding or re-encoding; the fragment is dropped). Invariant: path_and_query starts with '/', and neither it nor host contains a control char or a space.	parses_a_loopback_and_a_remote_endpoint, missing_path_becomes_root_and_a_bare_query_keeps_it, a_fragment_is_dropped_and_never_reaches_the_request_line, percent_encoding_is_carried_through_verbatim, ipv6_literals_round_trip_through_brackets, the_raw_authority_and_bare_path_are_available_for_agreement_checks	String/&str contents (not just lengths), struct field views, split/starts_with/format!	0,1,2,3,5,7
uri-render	contract	uri::Uri::{host_header, authority, path}, uri::Scheme::{default_port, as_str}	∀ u: u.host_header() = bracket(u.host) + (":" + dec(u.port) if u.explicit_port ∧ u.port ≠ u.scheme.default_port(), else ""); u.authority() = bracket(u.host) + ":" + dec(u.port); u.path() = the prefix of u.path_and_query before its first '?' (all of it if none). default_port(Http) = 80, as_str(Http) = "http"; default_port(Https) = 443, as_str(Https) = "https".	(secondary only; see §2)	&self views of String fields, format!, contains(':'), split('?')	0,1,2,3
pem-lossy	contract	pem::decode_certificates_lossy	∀ text: decode_certificates_lossy(text) terminates and never panics (decreases rest.len()), and equals the in-order list of d_i over the blocks of ScanLossy(text) with L_i = "CERTIFICATE" and aterm_codec::base64::decode(W(body_i)) = Ok(d_i), d_i non-empty. Any other label (TRUSTED CERTIFICATE, PRIVATE KEY, …) and any undecodable or empty CERTIFICATE block contributes nothing; an unterminated begin line or a missing end marker ends the scan.	the_lossy_parser_drops_undecodable_blocks_and_keeps_going, the_lossy_parser_terminates_on_every_malformed_input, the_lossy_parser_skips_an_aux_info_block	loop with a decreases measure, let-else, if-let chains, &str slicing, Vec, cross-crate base64 decode	0,1,3,5,7,12
proxy-resolve-selection	contract	proxy::resolve, proxy::env_text	∀ mode, target t, env: (1) resolve(Direct, t, env) = Ok(None) and env.var_os is never called. (2) resolve(Environment, t, env): let np = EnvText(env, NO_PROXY), else EnvText(env, no_proxy); if no_proxy_matches(t, np) then Ok(None). (3) Otherwise take the first n in [HTTPS_PROXY, https_proxy] (t https) or [HTTP_PROXY, http_proxy] (t http), then [ALL_PROXY, all_proxy], with EnvText(env, n) = Some(raw); the result is parse_proxy(n, raw).map(Some). (4) No such n: Ok(None). A set but unusable value (including non-Unicode, read as text with U+FFFD) decides the outcome: it is never skipped for a later variable and never becomes a direct connection.	direct_mode_ignores_every_proxy_variable, scheme_selects_the_variable, uppercase_wins_over_lowercase_and_lowercase_is_a_fallback, all_proxy_is_the_fallback_after_the_schemes_own_variable, absent_configuration_is_a_direct_connection, an_unusable_proxy_setting_is_an_error_naming_the_variable_not_a_direct_connection, a_value_that_is_not_valid_unicode_fails_closed_naming_its_variable	generic E: EnvSource (abstract oracle), OsString::to_string_lossy, trim, into_iter().chain().find_map with closures	0,1,3,4,5,11,13
proxy-parse	contract	proxy::parse_proxy, proxy::ProxyTarget::{uri, basic_auth}	∀ name, raw: parse_proxy(name, raw) = Ok(pt) ⇔ raw.len() ≤ 8192; no char of raw is a control char, ' ' or U+FFFD; raw starts with "http://" (ci), rest = what follows; with auth = rest up to its first '/', '?' or '#' and tail the remainder: if auth contains '@', split at its LAST '@' into userinfo and HP, and userinfo at its FIRST ':' into u and p (p = "" with no ':'), requiring userinfo ≠ "", pct::decode(u) free of ':' and U+FFFD, and pct::decode(p) free of U+FFFD; with no '@', HP = auth; and Uri::parse("http://" + HP + tail) = Some(x). Then pt.uri() = x, and pt.basic_auth() = Some(base64(pct::decode(u) + ":" + pct::decode(p))) when userinfo is present, else None.	userinfo_becomes_a_basic_credential_and_leaves_the_proxy_uri, a_credential_escape_that_is_not_utf8_is_refused_not_altered	split/rsplit_once/split_once, closures, format!, cross-crate aterm_codec pct::decode and base64::encode, composition with Uri::parse ensures	0,1,3,4,5,12
proxy-refusal-names-variable-never-value	contract	proxy::parse_proxy, proxy::resolve	∀ name, raw: parse_proxy(name, raw) = Err(e) ⇒ e = Error::Invalid(m) with m one of exactly two strings: name + " names an https:// proxy, which this client does not support; use an http:// proxy URL", chosen ⇔ raw passes the character screen, does not start with http:// (ci) and does start with https:// (ci); otherwise name + " is set but is not a usable http:// proxy URL". So every refusal from resolve names the deciding variable, and its text depends on raw only through that one bit; no credential, host or other byte of raw is echoed.	a_refusal_never_echoes_the_value, https_proxy_urls_are_refused_not_spoken_to_in_plaintext	String equality to a format! template, enum Error payload, closure `unusable`	0,1,3,5
proxy-target-debug-redacts	contract	<proxy::ProxyTarget as Debug>::fmt	∀ pt: format!("{pt:?}") is a function of pt.uri and pt.basic_auth.is_some() alone: the debug_struct rendering with uri = {pt.uri:?} and basic_auth = None or Some("<redacted>"). No credential byte appears in a Debug print.	(secondary only; see §2)	Formatter/debug_struct (std), Option::map closure, derived Debug of Uri	0,1,2,12,13
no-proxy-match	contract	proxy::no_proxy_matches, proxy::split_no_proxy_entry	∀ t, value: no_proxy_matches(t, None) = false; no_proxy_matches(t, Some(v)) ⇔ some comma-separated entry e of v (trimmed, ASCII-lowercased) is non-empty and either e = "*", or: split_no_proxy_entry(e) = (h, p), where p is a port only if e is bracketed ("[" h "]" with an optional ":" port) or e does not parse as Ipv6Addr and its text after the LAST ':' is non-empty ASCII digits that fit a u16; p = None or p = t.port(); h' = h without leading '.' is non-empty; and with H = lowercase(t.host()): both parse as IpAddr ⇒ same address; exactly one parses ⇒ no match; neither ⇒ H = h' or H ends with "." + h'. No suffix match between IP literals or across a non-label boundary. An entry containing U+FFFD matches no host and does not stop the remaining entries.	no_proxy_star_exempts_everything, no_proxy_matches_on_a_label_boundary_only, no_proxy_honours_a_leading_dot_and_a_port_restriction, no_proxy_is_case_insensitive, no_proxy_matches_ip_literals_as_addresses	for loop over split(','), to_ascii_lowercase Strings, std IpAddr/Ipv6Addr parsing, ends_with, byte indexing, tuple match	0,1,3,5,7
header-validation-grammar	contract	client::{validate_header, valid_header_name, valid_header_value}, client::RequestBuilder::header	∀ name, value: validate_header(name, value).is_ok() ⇔ name ≠ "" ∧ ∀ byte of name: tchar ∧ ∀ byte of value: vchar_ok. RequestBuilder::header(n, v) appends (n, v) ⇔ validate_header(n, v) is Ok; otherwise it stores that error only if no earlier error is stored, and send/send_with_env returns the first stored error before any other work. Hence no stored request header contains CR, LF, NUL or DEL, and no name contains ':' or whitespace.	header_injection_is_rejected, header_names_use_the_same_token_grammar_in_both_directions	bytes().all with closures, matches! on u8, Option::get_or_insert, Vec push, builder by-value self	0,1,3,5,7,13
response-header-fields	contract	client::read_header_fields, client::read_line	read_line reads up to and including the first '\n', strips one trailing '\r' and requires UTF-8; it fails with Protocol on EOF before any byte, on a line over MAX_HEADER_LINE (8192) bytes, or when the shared counter exceeds MAX_HEADER_BYTES (65536); EOF after some bytes ends the line. read_header_fields returns Ok(hs) ⇔ the lines before the first empty line are L_1..L_k with k ≤ 128 and every L_i = n_i ":" v_i (split at the FIRST ':') with valid_header_name(n_i) and valid_header_value(v_i); then hs[i] = (n_i, v_i.trim_matches([' ', '\t'])), so other whitespace (U+00A0) is preserved. Otherwise Err(Protocol). The name grammar is the same tchar grammar as validate_header.	response_headers_preserve_values_and_reject_controls	generic R: Read via &mut, byte-at-a-time loop, &mut usize counter, String::from_utf8, split_once, trim_matches	0,1,2,5,7,11
http-number-grammar	contract	client::parse_http_number	∀ s, r ∈ {10, 16}: parse_http_number(s, r) = Some(n) ⇔ s ≠ "" ∧ every char of s is a radix-r digit (no sign, space, "0x" or other prefix) ∧ val_r(s) ≤ usize::MAX; then n = val_r(s). Otherwise None.	(secondary only; see §2)	chars().all(is_digit(radix)), usize::from_str_radix (std spec needed), overflow as None	0,1,3,5,7
status-line-grammar	contract	client::read_status_line	read_status_line returns Ok(c) ⇔ read_line yields a line whose splitn(3, ' ') pieces give a version starting with "HTTP/1." and a code piece of exactly 3 chars with parse_http_number(code, 10) = Some(c) and 100 ≤ c ≤ 599. Any other line gives Err(Protocol); a read_line failure propagates.	malformed_status_lines_are_rejected, a_status_code_is_exactly_three_digits	splitn, Option filter/and_then chain with closures, u16::try_from, composition with read_line and parse_http_number	0,1,2,4,11,13
read-head-skips-interim	contract	client::read_head	For a reader holding heads H_1, H_2, … (status line, header fields, empty line) read with one shared consumed counter: read_head returns Ok(H_j) (code and headers) for the least j whose code ∉ 100..=199 or is 101, provided total bytes read ≤ MAX_HEADER_BYTES and every H_i (i ≤ j) parses. A 101 is no interim head (this client never asks to upgrade): it is returned as the final head, and refused by decide_framing or, on a CONNECT, as a non-2xx. Exceeding the budget is Err(Protocol), so an endless stream of 1xx heads ends; EOF before the first byte is Err(Io(UnexpectedEof)) (the peer answered nothing); EOF mid-head or a malformed head is Err(Protocol). Interim header fields never appear in the result, and no byte past H_j's empty line is read.	interim_1xx_responses_are_skipped_to_the_final_one, an_unterminated_header_block_does_not_hang, a_peer_that_closes_unanswered_is_a_transport_failure	loop with a budget-based decreases measure, match on u16 ranges, generic Read, composition	0,1,2,4,7,11
decide-framing-table	table-property	client::decide_framing	∀ method, status, hs: CL = values of names ci content-length, TE = values of names ci transfer-encoding (both in order), codings = concat over TE of v.split(',') trimmed, empty dropped. First rule that applies: (0) status = 101 → Err(Protocol("unrequested 101 Switching Protocols")) whatever the method and headers; (1) method = Head, or status ∈ 100..=199 or 204 or 304 → Ok(Empty) whatever the headers (method-head-framing); (2) the first x in CL with parse_http_number(x, 10) = None → Err(Protocol), message "Content-Length {x:?} is too large" if x is non-empty ASCII digits, else "Content-Length is not a decimal byte count: {x:?}"; (3) two parsed CL values differ → Err(Protocol); (4) TE empty → Ok(Length(n)) if CL non-empty with common value n, else Ok(ToEof); (5) CL non-empty → Err(Protocol) (TE + CL is refused whatever the coding); (6) codings = [c] with ci(c, "chunked") → Ok(Chunked); (7) otherwise Err(Protocol). Never Ok on conflicting CL; Chunked and Length never both apply.	framing_refuses_every_response_whose_end_is_ambiguous, bodiless_statuses_end_at_their_header_block_whatever_they_declare, a_transfer_coding_this_client_cannot_decode_is_refused, both_framings_together_is_refused_as_smuggling	slice of (String,String), filter/map/flat_map with closures and an impl Trait inner fn, Option<usize> state in a for loop, enum result	0,1,3,5,7,13
response-body-framing	refinement	client::{read_final_head, decide_framing, chunk_line, read_to_end}, client::Framed::{new, read, read_chunked, count}, client::read_response_from (the #[cfg(test)] harness)	∀ byte input B (any BufRead), method m, limit: read_to_end(Framed::new(B', f, limit)), where read_final_head(B) = Ok(H) leaves B' and decide_framing(m, H.code, H.headers) = Ok(f), is Ok(body) ⇔ the body read succeeds: Empty → [] and nothing read past the head; Length(n) → n ≤ limit (else Err(TooLarge{limit})) and the next n bytes exist (else Err(Io(UnexpectedEof))), body = them; ToEof → all remaining bytes, |body| ≤ limit (else TooLarge); Chunked → chunk_1 … chunk_m, then "0" plus extensions, a line ending, trailer lines and an empty line, each line read by chunk_line, where each size line's text before the first ';' with trailing SP/HTAB trimmed is S_i with parse_http_number(S_i, 16) = Some(s_i) > 0, followed by s_i data bytes and an empty line; a bad size or a non-empty line after the data is Err(Protocol); a running sum that overflows usize or exceeds limit is Err(TooLarge{limit}) before that chunk's data is read; body = the concatenated data. The #[cfg(test)] harness read_response_from(B, m, limit) composes exactly these (read_final_head, decide_framing, read_to_end) into a Response with status = H.code and headers = H.headers; send_with_env reads its body with the same read_to_end.	the_parser_frames_through_decide_framing, content_length_framing, chunked_framing_reassembles_and_skips_extensions_and_trailers, a_chunk_size_is_hex_digits_and_nothing_else, eof_framing_when_no_length_is_given, a_body_over_the_limit_errors_rather_than_truncating, non_2xx_status_is_reported_not_hidden	generic R: BufRead/Read, &mut reader, nested loops, Vec resize/extend/read_exact, checked_add, composition of read_head + decide_framing, a byte-stream model of Read	0,1,2,4,7,11
response-parse-no-panic	no-panic	client::{read_final_head, read_head, read_line, decide_framing, chunk_line, read_to_end}, client::Framed::{read, read_chunked, count}, client::read_response_from (the #[cfg(test)] harness)	∀ finite byte input, ∀ method, ∀ limit (including usize::MAX): read_final_head, decide_framing and read_to_end over a Framed (as the #[cfg(test)] harness read_response_from composes them) terminate and never panic; no arithmetic overflows, every slice range is in bounds, and the body Vec never grows past limit nor reserves more than min(declared, PREALLOC_CAP) up front. Covers chunk sizes up to usize::MAX (Err(TooLarge{limit}), never a wrap), declared lengths near usize::MAX, and header blocks that never end (Err).	loopback::a_chunk_size_at_the_top_of_usize_is_an_error_not_a_panic	overflow and bounds obligations inside loops, Vec resize, slice indexing body[start..], generic Read	0,1,2,7,11
response-accessors	contract	client::Response::{header, is_success, status, body}	∀ r, n: r.header(n) = the value of the FIRST (k, v) in r.headers with ci(k, n), or None; r.is_success() ⇔ 200 ≤ r.status < 300; status() and body() return the stored fields.	(secondary only; see §2)	&self view of Vec<(String,String)>, iterator find with a closure, &str return	0,1,2,5,13
render-request-exact	contract	client::{render_fields, render_head, is_reserved}, client::render_request (the #[cfg(test)] harness)	∀ method, t, via, hs, S (secret headers), cl: Option<u64>, with abs = (via is Some ∧ t.scheme = Http): render_head(method, t, via, hs, S, cl) is exactly render_fields(method, t, via, hs, cl), then each (n, v) ∈ S in order as n + ": " + v + "\r\n" (secret-header-scrub), then "\r\n"; and render_fields(…) is exactly (1) method.as_str() + " " + RT + " HTTP/1.1\r\n", RT = t.scheme.as_str() + "://" + t.host_header() + t.path_and_query() if abs, else t.path_and_query(); (2) "Host: " + t.host_header() + "\r\n"; (3) "Proxy-Authorization: Basic " + c + "\r\n" only if abs ∧ via.basic_auth() = Some(c); (4) "Connection: close\r\n", then "Content-Length: " + dec(n) + "\r\n" iff cl = Some(n) (bodiless-no-content-length says when exchange_with_env passes Some), then "Accept: */*\r\n" iff no (k, _) ∈ hs has ci(k, "accept") (caller-accept); (5) for each (n, v) in hs in order whose n is not ci any of host, content-length, transfer-encoding, connection, proxy-authorization (is_reserved): n + ": " + v + "\r\n". The #[cfg(test)] harness render_request(m, t, via, hs, cl) = render_fields(m, t, via, hs, cl) + "\r\n", i.e. render_head with S empty. Corollaries: each reserved name appears at most once (Host and Connection exactly once, Content-Length exactly once when cl is Some and never otherwise); an https request carries no proxy credential and stays in origin form through a proxy.	a_plain_request_is_origin_form_with_host_and_length, plain_http_through_a_proxy_uses_absolute_form, the_proxy_credential_rides_the_absolute_form_and_never_the_tunnel, https_keeps_origin_form_even_through_a_proxy, a_caller_cannot_duplicate_a_generated_header, golden_post::post_writes_the_golden_bytes, golden_post::caller_framing_headers_are_dropped_from_the_golden_bytes, methods::request_post_writes_the_same_bytes_as_post	String building with format!/push_str, for loop over a slice with a nested any() over a const &[&str], Option filter/and_then	0,1,3,4,5,7,13
connect-tunnel-exchange	contract	client::connect_tunnel	∀ s, t, c with A = t.authority(): connect_tunnel writes exactly "CONNECT " + A + " HTTP/1.1\r\nHost: " + A + "\r\nProxy-Connection: keep-alive\r\n" + ("Proxy-Authorization: Basic " + c + "\r\n" if c is Some) + "\r\n", then flushes; it then calls read_head directly on the unbuffered stream, so it consumes no byte past the final head's empty line and skips interim 1xx heads; it returns Ok(()) ⇔ the final code ∈ 200..300, else Err(Protocol("proxy refused CONNECT with status {code}")), which contains neither the credential nor its decoding.	loopback::a_proxy_that_sends_an_interim_response_before_its_connect_answer_still_tunnels, loopback::userinfo_in_the_proxy_url_authenticates_the_connect	&mut Stream (socket effect), Write trait, String building, composition with read_head over a stream model	0,1,2,4,8
send-pipeline-order	refinement	client::RequestBuilder::{exchange_with_env, exchange, send_with_env, send}, client::{render_head, write_tracked, send_body, early_answer, read_final_head, decide_framing, read_to_end}, client::Client::{open, handshake}	exchange_with_env runs in order: (1) a stored builder error is returned at NotSent; (2) Uri::parse(endpoint) = None → Err(Invalid("endpoint is not a usable absolute http(s) URL")) at NotSent, a constant that never echoes the endpoint; (3) a non-empty body on a method that takes none is Err(Invalid) at NotSent (bodiless-no-content-length); (4) proxy::resolve errors are returned at NotSent; no connector.connect call happens before (1)-(4) pass; (5) open connects to (via.host or t.host, via.port or t.port) under the connect phase of the deadline (idle-deadline); t Https → a TLS handshake against t.host() completes (after connect_tunnel when via is Some) before any request byte is written, and the request goes through the TLS arm, so the first byte the peer sees is a TLS record (0x16) and neither head nor body reaches the socket in cleartext; t Http → plain stream, and no TLS config is requested; every failure of (5) is NotSent; (6) the stream takes the request's own deadline, and the bytes written are render_head(method, t, via, headers, secrets, cl) (scrubbed after the write whatever its result), then the body through send_body, then a flush; (7) read_final_head, then decide_framing(method, code, headers) at Answered, give the Exchange whose BodyReader frames the rest. A failure in (6) after the first head byte was accepted splits two ways (send_body's Upload): a body reader that ended short or failed is returned at Sent with no read, since the peer still waits for the rest; a write or flush the connection refused (Upload::Wire) with any kind but TimedOut or PermissionDenied (EPIPE, a reset: a server that answered before reading the whole request and closed over the rest, RFC 9112 §9.5) is followed by one read_final_head on the same stream (early_answer, under the request's deadline, the TLS arm reading without retrying the failed write: stream-admit-before-every-syscall), and a head that parses goes on through (7)'s decide_framing at Answered as if the write had finished; otherwise the write's own error is returned at Sent. send_with_env(b, env) = exchange_with_env(Body::Bytes(b), env) with SendError.error as its error, then read_to_end of the BodyReader under the limit; send(b) = send_with_env(b, &ProcessEnv) and exchange(b) = exchange_with_env(b, &ProcessEnv). The builder's method is the one it began with: Client::post, put and get are Client::request with Method::Post, Put and Get.	loopback::a_json_post_puts_the_expected_bytes_on_the_wire_and_parses_the_reply, loopback::an_https_endpoint_puts_a_client_hello_on_the_wire_and_never_the_body, loopback::an_https_proxy_is_refused_before_any_byte_reaches_it, loopback::an_unusable_endpoint_fails_before_any_socket_is_opened, streaming::an_answer_sent_while_the_body_was_still_going_out_is_read, loopback::a_get_and_a_put_put_their_methods_on_the_wire	trait objects (Arc<dyn Connect>, Arc<dyn Guard>), sockets, rustls, enum Stream arms, ? propagation, composition of many callee ensures	0,1,2,4,8,13
tls-config-lazy-and-shared	contract	client::TlsConfigCache::get, client::Client::{new, with_shared_trust}, <client::Client as Clone>::clone	∀ c and all its clones: Arc::ptr_eq on their trust and tls_config fields; with_shared_trust(a, ..).trust is ptr_eq to a. Over one TlsConfigCache's lifetime, tls::client_config(trust) is evaluated at most once whatever the number of get calls or threads, and every get returns a clone of that one result (Ok values ptr_eq, Err strings equal). A send to an Http target never calls get, so an unusable trust never fails a plain HTTP request.	repeated_https_requests_and_client_clones_build_one_tls_config, shared_trust_constructor_retains_the_callers_root_allocation, cached_tls_configuration_errors_are_shared_by_clones, plain_http_never_constructs_the_deferred_tls_configuration	Arc, OnceLock::get_or_init with a closure (concurrency), derived Clone, Result<Arc<_>,String> clone	0,2,8,13
error-is-revoked	contract	client::Error::is_revoked, stream::revoked_error, <client::Error as From<io::Error>>::from	∀ e: e.is_revoked() ⇔ e = Io(x) ∧ x.kind() = PermissionDenied; revoked_error().kind() = PermissionDenied; From<io::Error> wraps without changing the kind. So a connect failure (ConnectionRefused) is never reported as revoked, and a guard refusal always is.	loopback::a_refused_port_is_an_error_not_a_panic	enum match with a guard, io::Error kind view	0,1,2
stream-admit-before-every-syscall	invariant	stream::Bounded::{admit, admit_read, admit_write, timeout_set}, <stream::Bounded as Read/Write>, <stream::Stream as Read/Write>, stream::Stream::{start_tls, wrote}, stream::read_received, stream::Deadline::{per_call, as_timeout, expired}	∀ Stream (plain or TLS), ∀ sequence of read/write/flush calls, including the socket calls rustls makes in complete_io and its record-read loop: each socket read, write or flush in Obs is immediately preceded, on the same thread, by an admit step: guard.is_authorized() false → Err(PermissionDenied), no socket call; Deadline::per_call fails (remaining None → TimedOut; a zero idle → TimedOut) → that error, no socket call; otherwise only that direction's timeout is set to the per-call bound min(remaining, idle) (read sets the read timeout; write and flush set the write timeout), and a setter that fails with InvalidInput on an Apple platform (EINVAL: XNU refuses every setsockopt on a socket shut down both ways, as a peer's reset leaves it, and per_call never yields a zero or out-of-range bound) lets the call go ahead, since such a socket cannot block: a read returns the bytes the peer sent before the reset, then EOF or the reset, so an early answer is read, never lost; any other setter failure is reported as the socket's pending error (take_error), else as the setter's error, with no socket call (Bounded::timeout_set). The same admit gates Stream::read/write/flush before rustls is consulted. Once a Stream write or flush has failed with anything but Interrupted (Stream::wrote), a TLS read takes only what the peer sent: read_received returns what conn.reader() holds, else reads a record through Bounded (read_tls, Interrupted retried) and processes it (a TLS error is InvalidData), and never calls the complete_io that would first retry the write rustls still holds, so an answer the peer sent before it closed over the failed write is read, never lost to that write; a close without close_notify is rustls's UnexpectedEof. A socket WouldBlock or TimedOut is reported through Deadline::as_timeout as the TimedOut expiry of the bound that fired (idle-deadline). start_tls passes PermissionDenied and TimedOut through and returns UnexpectedEof when complete_io makes no progress mid-handshake. Consequences: once the guard turns false no further request byte reaches the socket, and no Stream operation's last socket call starts after the deadline, so a trickling peer cannot extend the total time.	plaintext_io_sets_only_its_socket_timeout_direction, a_revoked_guard_stops_writes_and_reads_on_an_open_socket, an_answer_sent_before_a_reset_is_read_and_then_the_reset, loopback::authority_revoked_before_the_write_stops_the_body_leaving_the_process, loopback::a_silent_server_hits_the_deadline_instead_of_hanging, loopback::a_dribbling_tls_peer_cannot_outlive_the_global_deadline, tls_stream::a_dribbled_handshake_cannot_outlive_the_global_deadline, tls_stream::a_record_dribbled_after_the_handshake_cannot_outlive_the_global_deadline, tls_stream::authority_revoked_during_a_tls_read_stops_it, tls_stream::an_answer_sent_before_the_peer_closed_over_a_failed_write_is_read	&mut self over an enum with Box<rustls::StreamOwned>, dyn Guard, atomics read on another thread, sockets and timeouts, rustls internal loops calling back into Bounded	0,2,8,12,13
deadline-remaining	contract	stream::Deadline::{after, remaining, remaining_or_timeout}	In the clock model, for d = Deadline::after(b) taken at t0 and observed at t ≥ t0: d.remaining() = Some(t0 + b − t) if t < t0 + b, else None; never Some(0); Some(r) ⇒ r ≤ b. remaining_or_timeout() = Ok(r) ⇔ remaining() = Some(r), else Err with kind TimedOut. In particular Deadline::after(0).remaining() = None at once.	an_expired_deadline_is_a_timeout_not_a_hang	Instant::now (time effect), checked_duration_since, Duration filter	0,8
init-crypto-installs-provider	contract	tls::{init_crypto, provider}	After any init_crypto() returns, rustls::crypto::CryptoProvider::get_default().is_some(). Every call after the first is a no-op, including calls from other threads and calls after another component installed its own provider.	init_crypto_is_idempotent	std::sync::Once with a closure, rustls process-global state	0,8,12,13
client-config-roots-fail-closed	contract	tls::client_config	∀ ders: client_config(Roots(ders)), first rule that applies: some element is rejected by RootCertStore::add, the first at index i → Err(m) with m starting "CA bundle certificate {i+1} is not usable"; ders empty → Err("CA bundle yielded no usable certificates"); otherwise Ok(config) whose root store holds exactly the anchors from ders, with a webpki verifier and no platform anchors (Roots replaces the platform roots). Every Roots error string contains "CA bundle".	-	for loop over enumerate, rustls RootCertStore and ClientConfig builder, String errors	0,1,7,8,12
verifier-accepts-only-through-arm	contract	<verifier::PlatformVerifier as ServerCertVerifier>::verify_server_cert, verifier::apple::cf_string	∀ inputs: verify_server_cert(ee, ims, name, ocsp, now) = Ok(_) ⇒ name.to_str() ≠ "" ∧ inner.verify(ee, ims, name, name.to_str(), ocsp-or-None, now) = Ok(()). An empty rendered name is Err without calling the arm. On Apple, a hostname cf_string cannot encode is Err, never a NULL hostname. The arm is Ok only on an explicit positive platform verdict, so an empty name or an embedded NUL never reaches an evaluation with name checking off.	verifier::tests::a_degenerate_server_name_fails_closed_instead_of_disabling_name_checking	&self, Cow<str>, trait impl, unsafe FFI to Security.framework in the arm	0,1,2,8
verifier-delegates-signatures	equivalence	<verifier::PlatformVerifier as ServerCertVerifier>::{verify_tls12_signature, verify_tls13_signature, supported_verify_schemes}, verifier::PlatformVerifier::new	∀ v = PlatformVerifier::new(p)?, ∀ inputs: v.verify_tls12_signature(m, c, d) = rustls::crypto::verify_tls12_signature(m, c, d, &p.signature_verification_algorithms); likewise for TLS 1.3; v.supported_verify_schemes() = p.signature_verification_algorithms.supported_schemes(). No narrowing of the scheme set, no stubbed result: a signature rustls rejects is rejected.	verifier::tests::handshake_signature_verification_is_delegated_to_the_provider, signature_methods::tls13_rejects_a_signature_that_verifies_against_nothing, signature_methods::tls12_rejects_a_signature_that_verifies_against_nothing	&self field view (Arc<CryptoProvider>), calls into rustls	0,2,12
provider-advertises-modern-schemes	contract	tls::provider	provider().signature_verification_algorithms.supported_schemes() is non-empty and contains ECDSA_NISTP256_SHA256 and RSA_PSS_SHA256: a fact about ring's default provider, which the one-provider seam pins.	-	rustls/ring provider constant table, Vec contains	0,1,12
platform-verifier-conformance	refinement	<verifier::PlatformVerifier as ServerCertVerifier>::verify_server_cert, verifier::PlatformVerifier::{new, build}, verifier::unix::Verifier::verify, verifier::apple::Verifier::verify, verifier::eku_rejected	∀ chain (ee, ims), name n, OCSP, pinned now: the arm's verdict refines V(anchors, ee, ims, n, now) (Apple additionally requires an explicit EKU), with anchors = platform anchors ∪ extra_roots (extras ADD to the system set) and new() having extra_roots = []. verify is Ok only if V accepts. The error class is one of: BadEncoding, reported before any trust evaluation for non-DER bytes (including empty); NotValidForName; UnknownIssuer; eku_rejected() for any webpki InvalidPurpose; other. Consequences: a chain rooted only in a fixture CA is rejected by new(); a chain a real system anchor validates is accepted for its own name and rejected for any other.	verifier::tests::{the_first_party_verifier_matches_its_recorded_expectations, the_shipped_constructor_trusts_no_fixture_anchor, the_system_trust_store_is_actually_consulted, extra_anchors_add_to_the_system_set_rather_than_replacing_it, a_parse_failure_is_reported_as_one_and_never_as_a_trust_verdict}	FFI (Security.framework, CryptoAPI), trustd deadline, rustls/webpki, fixture DER bytes, an X.509 model on the Lean side	0,1,8,10,12
unix-store-fail-closed	contract	verifier::unix::Verifier::{from_ders, new}	∀ extra, sys, provider: some extra root rejected by RootCertStore::add → from_ders is Err (strict); otherwise S = extra ∪ {d ∈ sys | webpki parses d} (unparseable system roots skipped); S empty → Err(General(m)) with m containing "system trust store"; webpki builder failure → Err(General("system trust store is unusable: ...")); otherwise Ok with a verifier over exactly S. Verifier::new(extra, p) = from_ders(extra, system_root_ders(), p): either Ok or that loud Err, never a verifier over an empty store.	verifier::unix::{an_empty_system_store_is_a_loud_failure_not_a_silent_verifier, unparseable_system_roots_are_skipped_but_do_not_take_the_store_with_them, an_unusable_extra_anchor_is_an_error_rather_than_a_skip, the_shipped_constructor_reads_this_hosts_store}	for loop over Vec, rustls RootCertStore, Result mapping, fs effects in new()	0,1,7,8,12
unix-cert-path-selection	contract	verifier::unix::{bundle_file, bundle_dirs, system_root_ders, collect_ders, read_pem_file}	Against FS: bundle_file(Some(p)) = Some(p) ⇔ FS.exists(p), else None (never a fallback to the built-in list); bundle_file(None) = the first existing path in CERTIFICATE_FILES; bundle_dirs(Some(l)) = [q ∈ split_paths(l) in order | q ≠ "" ∧ FS.exists(q)]; bundle_dirs(None) = the existing CERTIFICATE_DIRS in order; system_root_ders consults NEITHER built-in list when SSL_CERT_FILE is set, or SSL_CERT_DIR is set with a non-empty entry; collect_ders(f, ds) = sort_dedup(lossy(f) ++ concat over d ∈ ds and regular files e in d (symlinks followed; dangling or unreadable skipped) of lossy(e)), where lossy(x) = decode_certificates_lossy(read_to_string(x)), or [] when x is not readable as UTF-8.	verifier::unix::{a_configured_path_replaces_the_built_in_search, a_configured_cert_dir_is_a_list_of_directories, the_built_in_search_finds_this_machines_real_bundle}	PathBuf/OsString, std::fs (exists, read_dir, metadata, read_to_string), env::var_os, split_paths, iterator closures, Vec sort/dedup	0,1,7,8,13
method-head-framing	table-property	client::decide_framing, client::Framed::{read, declared_len}	∀ method, status, hs with status ≠ 101: decide_framing(method, status, hs) = Ok(Empty) whenever method = Head or status ∈ 100..=199 or status ∈ {204, 304}, whatever framing headers hs carries; a final 101 is refused whatever the method (decide-framing-table rule 0); for every other (method, status) the rules of decide-framing-table apply unchanged. An Empty body yields 0 bytes on every read without reading the stream, and declared_len() = Some(0).	methods::a_head_answer_that_declares_a_length_reads_as_an_empty_body_at_once, methods::no_content_and_not_modified_read_as_empty_whatever_they_declare	enum match on Method, matches! over status ranges, &[(String,String)] view, a reader left untouched	0,1,3,5,7,13
bodiless-no-content-length	contract	client::RequestBuilder::exchange_with_env, client::render_fields, client::Method::{takes_body, always_sends_length}, client::Body::len	∀ method m, body b with n = b.len(): the head carries "Content-Length: " + dec(n) + "\r\n" ⇔ m ∈ {Post, Patch, Put} ∨ (m = Delete ∧ n > 0), and none otherwise; m ∈ {Get, Head} ∧ n > 0 → Err(SendError { phase: NotSent, error: Invalid("a " + m.as_str() + " carries no body") }) before proxy::resolve and before any connector.connect call.	methods::get_and_head_carry_no_content_length, methods::delete_carries_a_length_only_with_a_body, methods::post_patch_and_put_carry_a_zero_length_when_empty, methods::a_body_on_a_get_or_head_is_refused_before_any_connection	enum carriers (Method, Body with a &mut dyn Read), Option<u64>, the connect effect ordered after the check	0,1,3,4,8
caller-accept	contract	client::render_fields, client::RequestBuilder::secret_header	∀ m, t, via, hs, cl: render_fields(m, t, via, hs, cl) writes "Accept: */*\r\n" right after the Content-Length position ⇔ no (k, _) ∈ hs has ci(k, "accept"); a caller Accept is written in its own place among hs, so the default never sits beside a caller's Accept, and with at most one caller Accept the head carries exactly one Accept line. render_head adds no Accept after them: secret_header refuses the name accept (secret-header-scrub).	methods::the_caller_owns_accept, streaming::a_secret_header_with_a_reserved_name_is_refused_before_any_connect	String building, iterator any over a Vec of pairs with a closure	0,1,3,5,7,13
header-iteration	contract	client::{header_pairs, header_values}, client::Response::{headers, header_all, header}, client::Exchange::{headers, header_all, header}	∀ r (Response or Exchange), n: r.headers() yields r's (name, value) pairs in wire order, each name as sent; r.header_all(n) yields the value of every pair (k, v) with ci(k, n), in order (repeated headers each once); r.header(n) = r.header_all(n).next().	methods::headers_keep_their_order_and_repeated_ones_are_all_returned	impl Iterator returns over &self, filter/map closures, two lifetimes	0,1,2,5,13
streamed-body-framing	refinement	client::Framed::{read, read_chunked, count}, client::{chunk_line, read_uninterrupted, body_error, read_to_end, send_body}, client::BodyReader::{read, consumed, declared_len}	∀ framing f, limit L, byte input B: the bytes a BodyReader yields over its reads, concatenated, equal the body response-body-framing decodes from B, and consumed() is their count after every read. Length(n) with n > L, and any read or chunk size (checked with checked_add before its data is read) that would take consumed past L, fail io::Error::other(Error::TooLarge { limit: L }); EOF before the framing's end fails UnexpectedEof; malformed chunk framing fails InvalidData carrying Error::Protocol; each chunk framing line is at most MAX_HEADER_LINE bytes and the trailer section at most MAX_HEADER_BYTES. send() maps a body failure through body_error to the Error inside it, so its values are response-body-framing's; read_to_end reserves at most min(declared, PREALLOC_CAP) bytes up front, through try_reserve_exact, so no declared length panics or aborts on allocation. BodyReader and Exchange are Send + 'static. Uploads: Body::Reader { len, reader } writes exactly len bytes and never asks the reader past len; a reader that ends at k < len fails Io(UnexpectedEof, "the body reader ended at " + dec(k) + " of " + dec(len) + " bytes") at Sent, and a reader whose read fails e (other than Interrupted, which is retried) at k fails Io(Other, "the body reader failed at " + dec(k) + " of " + dec(len) + " bytes: " + e) at Sent, so no reader error reads as a socket's (is_revoked() is false). An Interrupted read inside a chunk framing line is retried there, never returned, so a caller's retry cannot resume a line mid-way.	streaming::a_streamed_upload_arrives_whole_with_its_length, streaming::a_mebibyte_answer_reads_incrementally, streaming::a_chunked_mebibyte_in_small_chunks_reads_whole, streaming::a_body_cut_short_is_unexpected_eof, streaming::bad_chunk_framing_is_invalid_data, streaming::a_body_past_the_limit_is_too_large_inside_the_io_error, streaming::exchange_and_body_reader_are_send, streaming::a_failing_body_reader_is_never_read_as_the_socket_failing, a_declared_length_near_usize_max_is_a_short_body_not_an_allocation, an_interrupted_read_inside_a_chunk_size_line_loses_no_framing	a decoder state machine across calls (&mut self), generic R: BufRead, u64/usize conversions, a boxed error inside io::Error and its downcast	0,1,2,3,4,5,7,8,11
phase-classification	contract	client::RequestBuilder::exchange_with_env, client::{write_tracked, read_final_head, read_head_byte, read_uninterrupted, decide_framing}	∀ request: a SendError's phase is NotSent ⇔ Stream::write accepted no byte of the request head, which covers every builder, endpoint, body-on-GET, proxy, DNS, connect, proxy CONNECT (a refused tunnel is Protocol("proxy refused CONNECT with status " + code)) and TLS-handshake failure; it is Sent once a head byte was accepted and no final head has parsed, and EOF before a status line is then Io(UnexpectedEof, "connection closed before the response"), an UnexpectedEof read error (TLS without close_notify) counting as that EOF and an Interrupted read of the head retried, never returned; it is Answered once the final head parsed, a 101 head included, and so is a final head read after a refused write (send-pipeline-order's early answer). exchange returns Ok exactly when the final head parsed and decide_framing accepted its framing; a refused framing, an unrequested 101 among them, is Err(SendError { phase: Answered, error: Protocol(_) }), and a failure after Ok is a BodyReader read error.	streaming::a_refused_connect_was_not_sent, streaming::a_server_that_reads_the_request_and_closes_leaves_it_sent, streaming::a_server_that_closes_mid_body_had_answered, streaming::a_reader_shorter_than_its_length_fails_after_the_head_was_sent, streaming::a_head_whose_framing_is_refused_had_answered, streaming::an_unrequested_switch_of_protocols_had_answered, streaming::a_proxy_that_refuses_the_tunnel_is_a_protocol_error_unsent, an_unclean_tls_close_in_the_head_reads_as_the_plain_eof, an_interrupted_read_in_the_head_is_waited_out	a phase carried through a &mut out-parameter across a write loop, network effects	0,1,2,4,8
idle-deadline	contract	stream::Deadline::{after, with_idle, idle, connect_phase, per_call, expired, as_timeout}, stream::later, stream::Bounded::{admit, admit_read, admit_write}, stream::TcpConnector::connect, connect::connect_timeout, client::RequestBuilder::{idle, backstop, connect_within}	∀ total T (backstop, else the client timeout), idle I, connect budget C: every socket call (connect, read, write, flush) runs under the timeout min(remaining, I); its expiry is TimedOut with "no bytes for " + dec(I.as_millis()) + "ms" when I was the smaller, else "request deadline exceeded"; until the stream is established (DNS, connect, proxy CONNECT, TLS handshake) remaining counts down to the sooner of start + C and start + T, whose expiry reads "connect deadline exceeded" when C was the sooner, and after it the request's own deadline holds. A T or C too long for an Instant (Duration::MAX) ends FAR_FUTURE from now and never panics. The connect itself is connect::connect_timeout(addr, t), TcpStream::connect_timeout's contract: t = 0 is InvalidInput, a port nothing listens on is ConnectionRefused (on Windows too, after one SYN retry rather than two), and the stream comes back blocking. Every bound here is enforced at a socket call (stream-admit-before-every-syscall), so the platform certificate check inside a TLS handshake, which is no socket call, keeps its own bound (EVALUATION_DEADLINE, 30 s, on Apple platforms; a 10 s URL-retrieval cap on Windows): a check that stalls ends only there, and C or T expiring meanwhile is reported at the next socket call.	streaming::a_silent_server_hits_the_idle_cap, streaming::a_trickling_server_hits_the_total_deadline, streaming::a_tls_handshake_that_never_answers_hits_the_connect_budget_unsent, streaming::the_request_deadline_takes_over_once_the_connect_budget_has_opened_the_stream, an_idle_cap_shorter_than_the_budget_bounds_each_call_and_names_itself, the_connect_phase_takes_the_sooner_end, a_budget_too_long_for_the_clock_is_far_off_rather_than_a_panic, connect::a_listening_port_connects_and_the_stream_blocks, connect::a_closed_port_is_refused, connect::a_zero_budget_is_invalid_input	Instant as a clock effect, Duration min, a per-call cap carried from admit to the error map, closures in map_err	0,1,2,8,13
dns-bounded	contract	stream::resolve_within, stream::TcpConnector::connect	∀ host, port, within, resolver: resolve_within runs resolver(host, port) on a thread named "aterm-http-dns" and returns once it answers or the time within has passed: its addresses collected into a Vec, or Err(TimedOut, "dns: no answer within " + dec(within.as_millis()) + "ms") with the helper left to finish alone (its send fails quietly); a resolver error e becomes Err(e.kind(), "dns: " + host + ": " + e); when no thread can be spawned the resolver runs inline. Every resolution failure TcpConnector::connect reports starts "dns:", on every platform.	a_resolver_that_never_answers_is_a_dns_timeout, a_failed_lookup_keeps_its_kind_and_names_the_host, a_resolved_address_connects	a spawned thread, a sync_channel and recv_timeout, an Arc<Mutex<Option<F>>> hand-over, generics F and I	0,8,11,13
secret-header-scrub	invariant	client::RequestBuilder::{secret_header, exchange_with_env}, client::{render_head, is_reserved}, <client::RequestBuilder as Debug>::fmt, <client::Exchange as Debug>::fmt, aterm_uds::secret::{Secret, scrub}	∀ request with secret headers S: each (n, v) ∈ S is admitted only when n is a token, n is not reserved (is_reserved: ci host, content-length, transfer-encoding, connection, proxy-authorization), n is not ci accept (caller-accept) and every byte of v is vchar_ok (else Err(Invalid) naming n and never v, at NotSent before any connector.connect call); every admitted one is written after the caller's headers as n + ": " + v + "\r\n" into one buffer allocated at its final size (so no reallocation leaves a copy) and scrubbed after the write whatever the write's result; no Debug of RequestBuilder, SendError or Error and no error text contains v (as a contiguous byte string) from the request: the one way v can appear is a Protocol error quoting response bytes, when a peer echoes v into a malformed status line, version or header line. The Debug of a RequestBuilder lists header names, never values, and so does an Exchange's of its response headers, so an Exchange's Debug print carries no response header value (a signed Location); Response keeps its derived Debug, which prints its header values and body.	streaming::a_secret_header_is_on_the_wire_and_in_no_error_or_debug, streaming::a_secret_header_with_a_control_character_is_refused_without_its_value, streaming::a_secret_header_with_a_reserved_name_is_refused_before_any_connect, streaming::an_exchange_debug_names_its_headers_without_their_values	Vec capacity reasoning (no reallocation), volatile scrub writes (aterm_uds), Debug formatting, cross-crate Secret	0,1,2,3,4,8,12
```

Unqualified test names in `subsumes` are in the file that holds the spec's functions (`src/uri.rs`,
`src/pem.rs`, `src/proxy.rs`, `src/client.rs`, `src/stream.rs`, `src/tls.rs`). A `loopback::`,
`tls_stream::`, `signature_methods::`, `golden_post::`, `methods::` or
`streaming::` prefix is the file under `tests/`; `connect::` is `src/connect.rs`, and
`verifier::tests::` and `verifier::unix::` are `src/verifier/tests.rs` and `src/verifier/unix.rs`.

### Distribution by kind (42)

| kind | count | ids |
|---|---|---|
| contract | 32 | uri-parse-accept-set, uri-parse-fields, uri-render, pem-lossy, proxy-resolve-selection, proxy-parse, proxy-refusal-names-variable-never-value, proxy-target-debug-redacts, no-proxy-match, header-validation-grammar, response-header-fields, http-number-grammar, status-line-grammar, read-head-skips-interim, response-accessors, render-request-exact, connect-tunnel-exchange, tls-config-lazy-and-shared, error-is-revoked, deadline-remaining, init-crypto-installs-provider, client-config-roots-fail-closed, verifier-accepts-only-through-arm, provider-advertises-modern-schemes, unix-store-fail-closed, unix-cert-path-selection, bodiless-no-content-length, caller-accept, header-iteration, phase-classification, idle-deadline, dns-bounded |
| equivalence | 1 | verifier-delegates-signatures |
| refinement | 4 | response-body-framing, send-pipeline-order, platform-verifier-conformance, streamed-body-framing |
| table-property | 2 | decide-framing-table, method-head-framing |
| invariant | 2 | stream-admit-before-every-syscall, secret-header-scrub |
| no-panic | 1 | response-parse-no-panic |

### Blocking-gap histogram (specs blocked by each gap)

| gap | specs | gap | specs |
|---|---|---|---|
| 0 seal admission | 42 | 7 loops | 18 |
| 1 non-scalar carriers | 36 | 8 effects/FFI | 17 |
| 2 `&self`/`&mut` views | 21 | 10 model→code refinement | 1 |
| 3 E6 body widening | 17 | 11 generics | 8 |
| 4 callee-ensures composition | 12 | 12 cross-crate citation | 11 |
| 5 quantified clauses | 17 | 13 closures/threads/`OnceLock` | 18 |

No spec is blocked by gap 0 alone. Gap 1 is the crate's bottleneck: only 6 specs avoid it
(`tls-config-lazy-and-shared`, `stream-admit-before-every-syscall`, `deadline-remaining`,
`init-crypto-installs-provider`, `verifier-delegates-signatures`, `dns-bounded`), and every one of
those needs gap 8 or 12 instead.

### Proof dependency order

`http-number-grammar`, `uri-render` and `error-is-revoked` → `uri-parse-accept-set` /
`uri-parse-fields` → `proxy-parse`, `proxy-refusal-names-variable-never-value`, `no-proxy-match` →
`proxy-resolve-selection`. Separately: aterm_codec base64/pct specs → `pem-lossy` →
`client-config-roots-fail-closed`, `unix-cert-path-selection` →
`unix-store-fail-closed`. On the wire: `header-validation-grammar`, `response-header-fields`,
`status-line-grammar` → `read-head-skips-interim` + `decide-framing-table` → `response-body-framing`,
`response-parse-no-panic` → `render-request-exact`, `connect-tunnel-exchange` → `deadline-remaining`,
`stream-admit-before-every-syscall` → `send-pipeline-order`. The verifier specs
(`verifier-delegates-signatures`, `verifier-accepts-only-through-arm`, `platform-verifier-conformance`)
come last; the conformance spec needs the X.509 model `V`. The agh rows (2026-10-20):
`decide-framing-table` → `method-head-framing`; `response-body-framing` → `streamed-body-framing`;
`render-request-exact` → `bodiless-no-content-length`, `caller-accept`; `response-accessors` →
`header-iteration`; `deadline-remaining` → `idle-deadline` and `dns-bounded` →
`phase-classification`; `header-validation-grammar` and aterm-uds's `Secret` →
`secret-header-scrub`.

## 2. Test → spec map (TSV)

`primary` is the spec that retires the test. `also` lists the other specs the same test samples.

```tsv
file	test	primary	also
src/client.rs	a_body_over_the_limit_errors_rather_than_truncating	response-body-framing	
src/client.rs	a_caller_cannot_duplicate_a_generated_header	render-request-exact	
src/client.rs	a_chunk_size_is_hex_digits_and_nothing_else	response-body-framing	http-number-grammar
src/client.rs	a_declared_length_near_usize_max_is_a_short_body_not_an_allocation	streamed-body-framing	response-body-framing
src/client.rs	a_peer_that_closes_unanswered_is_a_transport_failure	read-head-skips-interim	response-header-fields
src/client.rs	a_plain_request_is_origin_form_with_host_and_length	render-request-exact	
src/client.rs	a_status_code_is_exactly_three_digits	status-line-grammar	http-number-grammar
src/client.rs	a_transfer_coding_this_client_cannot_decode_is_refused	decide-framing-table	response-body-framing
src/client.rs	an_interrupted_read_in_the_head_is_waited_out	phase-classification	read-head-skips-interim
src/client.rs	an_interrupted_read_inside_a_chunk_size_line_loses_no_framing	streamed-body-framing	response-body-framing
src/client.rs	an_unclean_tls_close_in_the_head_reads_as_the_plain_eof	phase-classification	read-head-skips-interim
src/client.rs	an_unterminated_header_block_does_not_hang	read-head-skips-interim	response-parse-no-panic
src/client.rs	bodiless_statuses_end_at_their_header_block_whatever_they_declare	decide-framing-table	response-body-framing
src/client.rs	both_framings_together_is_refused_as_smuggling	decide-framing-table	response-body-framing
src/client.rs	cached_tls_configuration_errors_are_shared_by_clones	tls-config-lazy-and-shared	client-config-roots-fail-closed
src/client.rs	chunked_framing_reassembles_and_skips_extensions_and_trailers	response-body-framing	
src/client.rs	content_length_framing	response-body-framing	response-accessors
src/client.rs	eof_framing_when_no_length_is_given	response-body-framing	
src/client.rs	framing_refuses_every_response_whose_end_is_ambiguous	decide-framing-table	http-number-grammar
src/client.rs	header_injection_is_rejected	header-validation-grammar	
src/client.rs	header_names_use_the_same_token_grammar_in_both_directions	header-validation-grammar	response-header-fields
src/client.rs	https_keeps_origin_form_even_through_a_proxy	render-request-exact	
src/client.rs	interim_1xx_responses_are_skipped_to_the_final_one	read-head-skips-interim	
src/client.rs	malformed_status_lines_are_rejected	status-line-grammar	
src/client.rs	non_2xx_status_is_reported_not_hidden	response-body-framing	response-accessors
src/client.rs	plain_http_never_constructs_the_deferred_tls_configuration	tls-config-lazy-and-shared	
src/client.rs	plain_http_through_a_proxy_uses_absolute_form	render-request-exact	
src/client.rs	repeated_https_requests_and_client_clones_build_one_tls_config	tls-config-lazy-and-shared	
src/client.rs	response_headers_preserve_values_and_reject_controls	response-header-fields	header-validation-grammar
src/client.rs	shared_trust_constructor_retains_the_callers_root_allocation	tls-config-lazy-and-shared	
src/client.rs	the_parser_frames_through_decide_framing	response-body-framing	
src/client.rs	the_proxy_credential_rides_the_absolute_form_and_never_the_tunnel	render-request-exact	
src/connect.rs	a_closed_port_is_refused	idle-deadline	
src/connect.rs	a_listening_port_connects_and_the_stream_blocks	idle-deadline	
src/connect.rs	a_zero_budget_is_invalid_input	idle-deadline	
src/pem.rs	the_lossy_parser_drops_undecodable_blocks_and_keeps_going	pem-lossy	
src/pem.rs	the_lossy_parser_skips_an_aux_info_block	pem-lossy	
src/pem.rs	the_lossy_parser_terminates_on_every_malformed_input	pem-lossy	
src/proxy.rs	a_credential_escape_that_is_not_utf8_is_refused_not_altered	proxy-parse	proxy-refusal-names-variable-never-value
src/proxy.rs	a_refusal_never_echoes_the_value	proxy-refusal-names-variable-never-value	
src/proxy.rs	a_value_that_is_not_valid_unicode_fails_closed_naming_its_variable	proxy-resolve-selection	proxy-refusal-names-variable-never-value, no-proxy-match
src/proxy.rs	absent_configuration_is_a_direct_connection	proxy-resolve-selection	
src/proxy.rs	all_proxy_is_the_fallback_after_the_schemes_own_variable	proxy-resolve-selection	no-proxy-match
src/proxy.rs	an_unusable_proxy_setting_is_an_error_naming_the_variable_not_a_direct_connection	proxy-resolve-selection	proxy-parse, proxy-refusal-names-variable-never-value
src/proxy.rs	direct_mode_ignores_every_proxy_variable	proxy-resolve-selection	
src/proxy.rs	https_proxy_urls_are_refused_not_spoken_to_in_plaintext	proxy-refusal-names-variable-never-value	proxy-parse
src/proxy.rs	no_proxy_honours_a_leading_dot_and_a_port_restriction	no-proxy-match	
src/proxy.rs	no_proxy_is_case_insensitive	no-proxy-match	
src/proxy.rs	no_proxy_matches_ip_literals_as_addresses	no-proxy-match	
src/proxy.rs	no_proxy_matches_on_a_label_boundary_only	no-proxy-match	
src/proxy.rs	no_proxy_star_exempts_everything	no-proxy-match	
src/proxy.rs	scheme_selects_the_variable	proxy-resolve-selection	
src/proxy.rs	uppercase_wins_over_lowercase_and_lowercase_is_a_fallback	proxy-resolve-selection	
src/proxy.rs	userinfo_becomes_a_basic_credential_and_leaves_the_proxy_uri	proxy-parse	proxy-target-debug-redacts
src/stream.rs	a_budget_too_long_for_the_clock_is_far_off_rather_than_a_panic	idle-deadline	deadline-remaining
src/stream.rs	a_failed_lookup_keeps_its_kind_and_names_the_host	dns-bounded	
src/stream.rs	a_resolved_address_connects	dns-bounded	
src/stream.rs	a_resolver_that_never_answers_is_a_dns_timeout	dns-bounded	
src/stream.rs	a_revoked_guard_stops_writes_and_reads_on_an_open_socket	stream-admit-before-every-syscall	
src/stream.rs	an_answer_sent_before_a_reset_is_read_and_then_the_reset	stream-admit-before-every-syscall	
src/stream.rs	an_expired_deadline_is_a_timeout_not_a_hang	deadline-remaining	
src/stream.rs	an_idle_cap_shorter_than_the_budget_bounds_each_call_and_names_itself	idle-deadline	deadline-remaining
src/stream.rs	plaintext_io_sets_only_its_socket_timeout_direction	stream-admit-before-every-syscall	
src/stream.rs	the_connect_phase_takes_the_sooner_end	idle-deadline	deadline-remaining
src/tls.rs	init_crypto_is_idempotent	init-crypto-installs-provider	
src/uri.rs	a_fragment_is_dropped_and_never_reaches_the_request_line	uri-parse-fields	
src/uri.rs	control_characters_and_spaces_cannot_smuggle_a_header	uri-parse-accept-set	
src/uri.rs	ipv6_literals_round_trip_through_brackets	uri-parse-fields	uri-render
src/uri.rs	malformed_authorities_are_rejected	uri-parse-accept-set	
src/uri.rs	missing_path_becomes_root_and_a_bare_query_keeps_it	uri-parse-fields	
src/uri.rs	only_http_and_https_are_accepted	uri-parse-accept-set	uri-parse-fields
src/uri.rs	parses_a_loopback_and_a_remote_endpoint	uri-parse-fields	uri-render
src/uri.rs	percent_encoding_is_carried_through_verbatim	uri-parse-fields	
src/uri.rs	the_raw_authority_and_bare_path_are_available_for_agreement_checks	uri-parse-fields	uri-render
src/uri.rs	userinfo_is_rejected_rather_than_stripped	uri-parse-accept-set	
src/verifier/tests.rs	a_degenerate_server_name_fails_closed_instead_of_disabling_name_checking	verifier-accepts-only-through-arm	
src/verifier/tests.rs	a_parse_failure_is_reported_as_one_and_never_as_a_trust_verdict	platform-verifier-conformance	
src/verifier/tests.rs	extra_anchors_add_to_the_system_set_rather_than_replacing_it	platform-verifier-conformance	
src/verifier/tests.rs	handshake_signature_verification_is_delegated_to_the_provider	verifier-delegates-signatures	
src/verifier/tests.rs	the_first_party_verifier_matches_its_recorded_expectations	platform-verifier-conformance	
src/verifier/tests.rs	the_shipped_constructor_trusts_no_fixture_anchor	platform-verifier-conformance	
src/verifier/tests.rs	the_system_trust_store_is_actually_consulted	platform-verifier-conformance	
src/verifier/unix.rs	a_configured_cert_dir_is_a_list_of_directories	unix-cert-path-selection	
src/verifier/unix.rs	a_configured_path_replaces_the_built_in_search	unix-cert-path-selection	
src/verifier/unix.rs	an_empty_system_store_is_a_loud_failure_not_a_silent_verifier	unix-store-fail-closed	
src/verifier/unix.rs	an_unusable_extra_anchor_is_an_error_rather_than_a_skip	unix-store-fail-closed	
src/verifier/unix.rs	the_built_in_search_finds_this_machines_real_bundle	unix-cert-path-selection	platform-verifier-conformance
src/verifier/unix.rs	the_shipped_constructor_reads_this_hosts_store	unix-store-fail-closed	
src/verifier/unix.rs	unparseable_system_roots_are_skipped_but_do_not_take_the_store_with_them	unix-store-fail-closed	
tests/golden_post.rs	caller_framing_headers_are_dropped_from_the_golden_bytes	render-request-exact	send-pipeline-order
tests/golden_post.rs	post_writes_the_golden_bytes	render-request-exact	send-pipeline-order, caller-accept, bodiless-no-content-length
tests/loopback.rs	a_chunk_size_at_the_top_of_usize_is_an_error_not_a_panic	response-parse-no-panic	response-body-framing
tests/loopback.rs	a_dribbling_tls_peer_cannot_outlive_the_global_deadline	stream-admit-before-every-syscall	
tests/loopback.rs	a_get_and_a_put_put_their_methods_on_the_wire	send-pipeline-order	response-accessors, render-request-exact
tests/loopback.rs	a_json_post_puts_the_expected_bytes_on_the_wire_and_parses_the_reply	send-pipeline-order	response-accessors, render-request-exact
tests/loopback.rs	a_proxy_that_sends_an_interim_response_before_its_connect_answer_still_tunnels	connect-tunnel-exchange	read-head-skips-interim
tests/loopback.rs	a_refused_port_is_an_error_not_a_panic	error-is-revoked	
tests/loopback.rs	a_silent_server_hits_the_deadline_instead_of_hanging	stream-admit-before-every-syscall	
tests/loopback.rs	an_https_endpoint_puts_a_client_hello_on_the_wire_and_never_the_body	send-pipeline-order	
tests/loopback.rs	an_https_proxy_is_refused_before_any_byte_reaches_it	send-pipeline-order	
tests/loopback.rs	an_unusable_endpoint_fails_before_any_socket_is_opened	send-pipeline-order	
tests/loopback.rs	authority_revoked_before_the_write_stops_the_body_leaving_the_process	stream-admit-before-every-syscall	error-is-revoked
tests/loopback.rs	userinfo_in_the_proxy_url_authenticates_the_connect	connect-tunnel-exchange	proxy-parse
tests/methods.rs	a_body_on_a_get_or_head_is_refused_before_any_connection	bodiless-no-content-length	phase-classification
tests/methods.rs	a_head_answer_that_declares_a_length_reads_as_an_empty_body_at_once	method-head-framing	streamed-body-framing
tests/methods.rs	delete_carries_a_length_only_with_a_body	bodiless-no-content-length	
tests/methods.rs	get_and_head_carry_no_content_length	bodiless-no-content-length	
tests/methods.rs	headers_keep_their_order_and_repeated_ones_are_all_returned	header-iteration	response-accessors
tests/methods.rs	no_content_and_not_modified_read_as_empty_whatever_they_declare	method-head-framing	decide-framing-table
tests/methods.rs	post_patch_and_put_carry_a_zero_length_when_empty	bodiless-no-content-length	
tests/methods.rs	request_post_writes_the_same_bytes_as_post	render-request-exact	bodiless-no-content-length
tests/methods.rs	the_caller_owns_accept	caller-accept	
tests/signature_methods.rs	tls12_rejects_a_signature_that_verifies_against_nothing	verifier-delegates-signatures	
tests/signature_methods.rs	tls13_rejects_a_signature_that_verifies_against_nothing	verifier-delegates-signatures	
tests/streaming.rs	a_body_cut_short_is_unexpected_eof	streamed-body-framing	
tests/streaming.rs	a_body_past_the_limit_is_too_large_inside_the_io_error	streamed-body-framing	response-body-framing
tests/streaming.rs	a_chunked_mebibyte_in_small_chunks_reads_whole	streamed-body-framing	response-body-framing
tests/streaming.rs	a_failing_body_reader_is_never_read_as_the_socket_failing	streamed-body-framing	phase-classification, error-is-revoked
tests/streaming.rs	a_head_whose_framing_is_refused_had_answered	phase-classification	decide-framing-table
tests/streaming.rs	a_mebibyte_answer_reads_incrementally	streamed-body-framing	
tests/streaming.rs	a_proxy_that_refuses_the_tunnel_is_a_protocol_error_unsent	phase-classification	connect-tunnel-exchange
tests/streaming.rs	a_reader_shorter_than_its_length_fails_after_the_head_was_sent	phase-classification	streamed-body-framing
tests/streaming.rs	a_refused_connect_was_not_sent	phase-classification	error-is-revoked
tests/streaming.rs	a_secret_header_is_on_the_wire_and_in_no_error_or_debug	secret-header-scrub	phase-classification
tests/streaming.rs	a_secret_header_with_a_control_character_is_refused_without_its_value	secret-header-scrub	header-validation-grammar
tests/streaming.rs	a_secret_header_with_a_reserved_name_is_refused_before_any_connect	secret-header-scrub	phase-classification, caller-accept
tests/streaming.rs	a_server_that_closes_mid_body_had_answered	phase-classification	streamed-body-framing
tests/streaming.rs	a_server_that_reads_the_request_and_closes_leaves_it_sent	phase-classification	
tests/streaming.rs	a_silent_server_hits_the_idle_cap	idle-deadline	phase-classification
tests/streaming.rs	a_streamed_upload_arrives_whole_with_its_length	streamed-body-framing	bodiless-no-content-length
tests/streaming.rs	a_tls_handshake_that_never_answers_hits_the_connect_budget_unsent	idle-deadline	phase-classification
tests/streaming.rs	a_trickling_server_hits_the_total_deadline	idle-deadline	deadline-remaining
tests/streaming.rs	an_answer_sent_while_the_body_was_still_going_out_is_read	send-pipeline-order	phase-classification, streamed-body-framing
tests/streaming.rs	an_exchange_debug_names_its_headers_without_their_values	secret-header-scrub	header-iteration
tests/streaming.rs	an_unrequested_switch_of_protocols_had_answered	phase-classification	decide-framing-table, read-head-skips-interim, method-head-framing
tests/streaming.rs	bad_chunk_framing_is_invalid_data	streamed-body-framing	
tests/streaming.rs	exchange_and_body_reader_are_send	streamed-body-framing	
tests/streaming.rs	the_request_deadline_takes_over_once_the_connect_budget_has_opened_the_stream	idle-deadline	phase-classification
tests/tls_stream.rs	a_dribbled_handshake_cannot_outlive_the_global_deadline	stream-admit-before-every-syscall	
tests/tls_stream.rs	a_record_dribbled_after_the_handshake_cannot_outlive_the_global_deadline	stream-admit-before-every-syscall	
tests/tls_stream.rs	an_answer_sent_before_the_peer_closed_over_a_failed_write_is_read	stream-admit-before-every-syscall	send-pipeline-order
tests/tls_stream.rs	authority_revoked_during_a_tls_read_stops_it	stream-admit-before-every-syscall	
```

## 3. Coverage

- 106 tests read. 102 mapped to one of the 35 specs and 4 were not spec samples (§4). After the
  deletions since, 88 map to one of the 33 specs; five more (the three in `src/connect.rs`, one in
  `tests/tls_stream.rs` and one in `src/pem.rs`) had no row until 2026-10-20. The 47 tests agh's
  WP1 added (2026-10-20) map to the 9 specs it added, to `render-request-exact` (3), to
  `stream-admit-before-every-syscall` (2: the Apple-platform reset test in `src/stream.rs` and the
  TLS early answer in `tests/tls_stream.rs`), or to `send-pipeline-order` (1: the plain early
  answer), so with the five, 140 tests map to one of 42 specs. Main's merge (2026-10-21) adds
  two: one to `send-pipeline-order`, one to `read-head-skips-interim`: 142.
- The biggest collapses: `streamed-body-framing` (10), `idle-deadline` (10, 3 of them
  `src/connect.rs`'s), `render-request-exact` (8 primary, 3 of them the golden and method tests),
  `stream-admit-before-every-syscall` (10, across three test files), `phase-classification` (9),
  `proxy-resolve-selection` (7), `response-body-framing` (7), `uri-parse-fields` (6), then
  `no-proxy-match` and `platform-verifier-conformance` (5 each).
- `http-number-grammar`, `uri-render`, `proxy-target-debug-redacts` and `response-accessors` retire
  no test alone, but the larger specs cite them as lemmas, and they are a secondary spec of 12
  tests.
- A test leaves the suite only when **every** spec in its row (primary and `also`) is proved. For
  example, `framing_refuses_every_response_whose_end_is_ambiguous` waits on both
  `decide-framing-table` and `http-number-grammar`.

## 4. Tests that are not spec samples

Status (2026-10-16): none is left. The 2 tests this section marked **delete** and the 2 it marked
**move-to-bench** are deleted from the crate, and their rows are gone from it and from §2.

No test is a pure regression of a removed bug with no general property; every regression-shaped test (header smuggling, the usize chunk-size wrap, the dribbling
TLS peer) is an instance of a spec above.

## 5. Implied specs that look wrong, or disagree with the code

1. **The crate's test count is 106, not 107.** `tests/http_suite.rs` contains no `#[test]` fn; the
   only `#[test]` is in a doc comment, and the file just declares modules. A grep-based count is off by one.
2. **Misleading leftover code.** In
   `verifier::tests::a_parse_failure_is_reported_as_one_and_never_as_a_trust_verdict` the per-iteration
   `Case` is built with chain `&[MALFORMED]` for both inputs, and only its `now` field is used. The
   verified bytes come from the loop variable.
3. **A framing case the shipped parser cannot reach.**
   `framing_refuses_every_response_whose_end_is_ambiguous` includes `("Content-Length", "5 ")`. Through
   the real parser `read_header_fields` trims SP/HTAB, so `Content-Length: 5 ` on the wire is
   **accepted** as 5. The case samples `decide_framing` in isolation, not the end-to-end behaviour its
   comment implies. `decide-framing-table` and `response-header-fields` together state what actually
   happens.
4. **A latency budget that passed vacuously** (fixed 2026-10-20).
   `loopback::authority_revoked_before_the_write_stops_the_body_leaving_the_process` waited on the
   server with `recv_timeout(5 s).unwrap_or_default()`, a latency budget against the AGENTS.md
   hang-detector rule, whose timeout yielded an empty buffer that passed. It now waits under the 60 s
   hang detector and fails if the stub never reports. The guard is also false from
   the start, so the test checks revocation before connect, not while the request is in flight;
   `TcpConnector` opens the socket without consulting the guard. `stream-admit-before-every-syscall`
   states the in-flight property.
5. **An unchecked error kind** (fixed 2026-10-20).
   `loopback::a_silent_server_hits_the_deadline_instead_of_hanging` discarded the error
   (`let _ = error`) and bounded only the elapsed time. It now asserts the `TimedOut` Io error the
   spec requires, and no clock. Its stub holds the connection for the 60 s hang detector, so a
   client that ignored its deadline sees the close a minute later as `UnexpectedEof` and fails.

## 6. Proof order (easiest high-value first)

No spec here is blocked by gap 0 alone, so the order prefers the smallest remaining gap sets.

1. **`error-is-revoked`** (gaps 0, 1, 2). An enum match with a guard on `io::ErrorKind`, plus the
   `From` wrap. It is a small first use of enum carriers and `&self` views, retires
   `a_refused_port_is_an_error_not_a_panic`, and is the error vocabulary that
   `stream-admit-before-every-syscall` and `send-pipeline-order` cite.
2. **`deadline-remaining`** (gaps 0, 8). Pure `Duration` arithmetic once `Instant::now` is a clock
   effect. It is the timeout half of the stream invariant.
3. **`http-number-grammar`** (gaps 0, 1, 3, 5, 7). A single `chars().all` loop and a `from_str_radix`
   std spec over `&str`. It is the lemma under `status-line-grammar`, `decide-framing-table` and the
   chunk sizes of `response-body-framing`, which together hold 13 primary tests.
4. **`uri-render`** and **`proxy-refusal-names-variable-never-value`** (gaps 0, 1, 2, 3 and 0, 1, 3, 5).
   Straight-line string building with no loops. They are the first `&str`/`String` content specs and
   unblock `render-request-exact` and the `proxy` chain.
5. **`decide-framing-table`** (gaps 0, 1, 3, 5, 7, 13). A finite rule table over the header list. It
   retires 4 tests, including the smuggling cases, and with `http-number-grammar` is the core of
   `response-body-framing`.

Runner-up: `verifier-delegates-signatures` (gaps 0, 2, 12). It is three one-line delegations, so it is
almost free once cross-crate citation of rustls lands, and it retires 3 tests.
