<!-- Copyright 2026 Andrew Yates -->
<!-- SPDX-License-Identifier: Apache-2.0 -->

# aterm-http: spec recovery from its 106 unit tests

> Status 2026-10-16, after the merge with the tests-and-gates audit: the rows naming 18 tests it
> deleted and 1 spec whose functions it deleted are gone, and the spec rows no longer cite those
> tests. Since then the strict PEM decoder is deleted, and with it the spec relating it to the
> lossy one, and the 2 tests §4 marked **delete** are deleted with their rows. 33 specs remain and
> §2 maps 88 tests: the result line, the kind table, the gap histogram and §3 count those. The
> per-file census of the 106 tests read was taken before.

> Status 2026-10-16: this is the crate's migration worklist. Each of the 33 specs below is
> proved through Trust/Clean about the compiled Rust fn, and then the tests it subsumes are
> deleted. The 4 tests §4 listed outside any spec are all deleted.

Source: `crates/aterm-http` (read only). I read every `#[test]` (src/client.rs 28, src/pem.rs 11,
src/proxy.rs 16, src/stream.rs 4, src/tls.rs 2, src/uri.rs 10, src/verifier/tests.rs 10,
src/verifier/unix.rs 7, tests/loopback.rs 11, tests/signature_methods.rs 3, tests/tls_stream.rs 2,
tests/trust_config.rs 2 = 106) and the code each one reaches. `tests/http_suite.rs` has no `#[test]`
fn: it only declares modules, and a grep counts 107 because of the `#[test]` in its doc comment.
Nothing was compiled or run. The claims below come from reading the code.

Result: **33 specs** cover **88 tests**. 27 specs are the primary spec of at least one test. Four
are lemmas that the others need and are only secondary specs of tests: `uri-render`,
`proxy-target-debug-redacts`, `http-number-grammar` and `response-accessors`.
`client-config-roots-fail-closed` is now only a secondary spec too, and
`provider-advertises-modern-schemes` has no test left.
**No test outside a spec is left** (§4).

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
  prerequisite for every row, so it is listed on all 35. 1 is non-scalar carriers, 2 is `&self`/`&mut`
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
pem-lossy	contract	pem::decode_certificates_lossy	∀ text: decode_certificates_lossy(text) terminates and never panics (decreases rest.len()), and equals the in-order list of d_i over the blocks of ScanLossy(text) with L_i = "CERTIFICATE" and aterm_codec::base64::decode(W(body_i)) = Ok(d_i), d_i non-empty. Any other label (TRUSTED CERTIFICATE, PRIVATE KEY, …) and any undecodable or empty CERTIFICATE block contributes nothing; an unterminated begin line or a missing end marker ends the scan.	the_lossy_parser_drops_undecodable_blocks_and_keeps_going, the_lossy_parser_terminates_on_every_malformed_input	loop with a decreases measure, let-else, if-let chains, &str slicing, Vec, cross-crate base64 decode	0,1,3,5,7,12
proxy-resolve-selection	contract	proxy::resolve, proxy::env_text	∀ mode, target t, env: (1) resolve(Direct, t, env) = Ok(None) and env.var_os is never called. (2) resolve(Environment, t, env): let np = EnvText(env, NO_PROXY), else EnvText(env, no_proxy); if no_proxy_matches(t, np) then Ok(None). (3) Otherwise take the first n in [HTTPS_PROXY, https_proxy] (t https) or [HTTP_PROXY, http_proxy] (t http), then [ALL_PROXY, all_proxy], with EnvText(env, n) = Some(raw); the result is parse_proxy(n, raw).map(Some). (4) No such n: Ok(None). A set but unusable value (including non-Unicode, read as text with U+FFFD) decides the outcome: it is never skipped for a later variable and never becomes a direct connection.	direct_mode_ignores_every_proxy_variable, scheme_selects_the_variable, uppercase_wins_over_lowercase_and_lowercase_is_a_fallback, all_proxy_is_the_fallback_after_the_schemes_own_variable, absent_configuration_is_a_direct_connection, an_unusable_proxy_setting_is_an_error_naming_the_variable_not_a_direct_connection, a_value_that_is_not_valid_unicode_fails_closed_naming_its_variable	generic E: EnvSource (abstract oracle), OsString::to_string_lossy, trim, into_iter().chain().find_map with closures	0,1,3,4,5,11,13
proxy-parse	contract	proxy::parse_proxy, proxy::ProxyTarget::{uri, basic_auth}	∀ name, raw: parse_proxy(name, raw) = Ok(pt) ⇔ raw.len() ≤ 8192; no char of raw is a control char, ' ' or U+FFFD; raw starts with "http://" (ci), rest = what follows; with auth = rest up to its first '/', '?' or '#' and tail the remainder: if auth contains '@', split at its LAST '@' into userinfo and HP, and userinfo at its FIRST ':' into u and p (p = "" with no ':'), requiring userinfo ≠ "", pct::decode(u) free of ':' and U+FFFD, and pct::decode(p) free of U+FFFD; with no '@', HP = auth; and Uri::parse("http://" + HP + tail) = Some(x). Then pt.uri() = x, and pt.basic_auth() = Some(base64(pct::decode(u) + ":" + pct::decode(p))) when userinfo is present, else None.	userinfo_becomes_a_basic_credential_and_leaves_the_proxy_uri, a_credential_escape_that_is_not_utf8_is_refused_not_altered	split/rsplit_once/split_once, closures, format!, cross-crate aterm_codec pct::decode and base64::encode, composition with Uri::parse ensures	0,1,3,4,5,12
proxy-refusal-names-variable-never-value	contract	proxy::parse_proxy, proxy::resolve	∀ name, raw: parse_proxy(name, raw) = Err(e) ⇒ e = Error::Invalid(m) with m one of exactly two strings: name + " names an https:// proxy, which this client does not support; use an http:// proxy URL", chosen ⇔ raw passes the character screen, does not start with http:// (ci) and does start with https:// (ci); otherwise name + " is set but is not a usable http:// proxy URL". So every refusal from resolve names the deciding variable, and its text depends on raw only through that one bit; no credential, host or other byte of raw is echoed.	a_refusal_never_echoes_the_value, https_proxy_urls_are_refused_not_spoken_to_in_plaintext	String equality to a format! template, enum Error payload, closure `unusable`	0,1,3,5
proxy-target-debug-redacts	contract	<proxy::ProxyTarget as Debug>::fmt	∀ pt: format!("{pt:?}") is a function of pt.uri and pt.basic_auth.is_some() alone: the debug_struct rendering with uri = {pt.uri:?} and basic_auth = None or Some("<redacted>"). No credential byte appears in a Debug print.	(secondary only; see §2)	Formatter/debug_struct (std), Option::map closure, derived Debug of Uri	0,1,2,12,13
no-proxy-match	contract	proxy::no_proxy_matches, proxy::split_no_proxy_entry	∀ t, value: no_proxy_matches(t, None) = false; no_proxy_matches(t, Some(v)) ⇔ some comma-separated entry e of v (trimmed, ASCII-lowercased) is non-empty and either e = "*", or: split_no_proxy_entry(e) = (h, p), where p is a port only if e is bracketed ("[" h "]" with an optional ":" port) or e does not parse as Ipv6Addr and its text after the LAST ':' is non-empty ASCII digits that fit a u16; p = None or p = t.port(); h' = h without leading '.' is non-empty; and with H = lowercase(t.host()): both parse as IpAddr ⇒ same address; exactly one parses ⇒ no match; neither ⇒ H = h' or H ends with "." + h'. No suffix match between IP literals or across a non-label boundary. An entry containing U+FFFD matches no host and does not stop the remaining entries.	no_proxy_star_exempts_everything, no_proxy_matches_on_a_label_boundary_only, no_proxy_honours_a_leading_dot_and_a_port_restriction, no_proxy_is_case_insensitive, no_proxy_matches_ip_literals_as_addresses	for loop over split(','), to_ascii_lowercase Strings, std IpAddr/Ipv6Addr parsing, ends_with, byte indexing, tuple match	0,1,3,5,7
header-validation-grammar	contract	client::{validate_header, valid_header_name, valid_header_value}, client::RequestBuilder::header	∀ name, value: validate_header(name, value).is_ok() ⇔ name ≠ "" ∧ ∀ byte of name: tchar ∧ ∀ byte of value: vchar_ok. RequestBuilder::header(n, v) appends (n, v) ⇔ validate_header(n, v) is Ok; otherwise it stores that error only if no earlier error is stored, and send/send_with_env returns the first stored error before any other work. Hence no stored request header contains CR, LF, NUL or DEL, and no name contains ':' or whitespace.	header_injection_is_rejected, header_names_use_the_same_token_grammar_in_both_directions	bytes().all with closures, matches! on u8, Option::get_or_insert, Vec push, builder by-value self	0,1,3,5,7,13
response-header-fields	contract	client::read_header_fields, client::read_line	read_line reads up to and including the first '\n', strips one trailing '\r' and requires UTF-8; it fails with Protocol on EOF before any byte, on a line over MAX_HEADER_LINE (8192) bytes, or when the shared counter exceeds MAX_HEADER_BYTES (65536); EOF after some bytes ends the line. read_header_fields returns Ok(hs) ⇔ the lines before the first empty line are L_1..L_k with k ≤ 128 and every L_i = n_i ":" v_i (split at the FIRST ':') with valid_header_name(n_i) and valid_header_value(v_i); then hs[i] = (n_i, v_i.trim_matches([' ', '\t'])), so other whitespace (U+00A0) is preserved. Otherwise Err(Protocol). The name grammar is the same tchar grammar as validate_header.	response_headers_preserve_values_and_reject_controls	generic R: Read via &mut, byte-at-a-time loop, &mut usize counter, String::from_utf8, split_once, trim_matches	0,1,2,5,7,11
http-number-grammar	contract	client::parse_http_number	∀ s, r ∈ {10, 16}: parse_http_number(s, r) = Some(n) ⇔ s ≠ "" ∧ every char of s is a radix-r digit (no sign, space, "0x" or other prefix) ∧ val_r(s) ≤ usize::MAX; then n = val_r(s). Otherwise None.	(secondary only; see §2)	chars().all(is_digit(radix)), usize::from_str_radix (std spec needed), overflow as None	0,1,3,5,7
status-line-grammar	contract	client::read_status_line	read_status_line returns Ok(c) ⇔ read_line yields a line whose splitn(3, ' ') pieces give a version starting with "HTTP/1." and a code piece of exactly 3 chars with parse_http_number(code, 10) = Some(c) and 100 ≤ c ≤ 599. Any other line gives Err(Protocol); a read_line failure propagates.	malformed_status_lines_are_rejected, a_status_code_is_exactly_three_digits	splitn, Option filter/and_then chain with closures, u16::try_from, composition with read_line and parse_http_number	0,1,2,4,11,13
read-head-skips-interim	contract	client::read_head	For a reader holding heads H_1, H_2, … (status line, header fields, empty line) read with one shared consumed counter: read_head returns Ok(H_j) (code and headers) for the least j whose code ∉ 100..=199, provided no H_i (i < j) has code 101, total bytes read ≤ MAX_HEADER_BYTES and every H_i parses. A 101 is Err(Protocol); exceeding the budget is Err(Protocol), so an endless stream of 1xx heads ends; EOF mid-head or a malformed head is Err. Interim header fields never appear in the result, and no byte past H_j's empty line is read.	interim_1xx_responses_are_skipped_to_the_final_one, an_unterminated_header_block_does_not_hang	loop with a budget-based decreases measure, match on u16 ranges, generic Read, composition	0,1,2,4,7,11
decide-framing-table	table-property	client::decide_framing	∀ status, hs: CL = values of names ci content-length, TE = values of names ci transfer-encoding (both in order), codings = concat over TE of v.split(',') trimmed, empty dropped. First rule that applies: (1) status ∈ 100..=199 or 204 or 304 → Ok(Empty) whatever the headers; (2) the first x in CL with parse_http_number(x, 10) = None → Err(Protocol), message "Content-Length {x:?} is too large" if x is non-empty ASCII digits, else "Content-Length is not a decimal byte count: {x:?}"; (3) two parsed CL values differ → Err(Protocol); (4) TE empty → Ok(Length(n)) if CL non-empty with common value n, else Ok(ToEof); (5) CL non-empty → Err(Protocol) (TE + CL is refused whatever the coding); (6) codings = [c] with ci(c, "chunked") → Ok(Chunked); (7) otherwise Err(Protocol). Never Ok on conflicting CL; Chunked and Length never both apply.	framing_refuses_every_response_whose_end_is_ambiguous, bodiless_statuses_end_at_their_header_block_whatever_they_declare, a_transfer_coding_this_client_cannot_decode_is_refused, both_framings_together_is_refused_as_smuggling	slice of (String,String), filter/map/flat_map with closures and an impl Trait inner fn, Option<usize> state in a for loop, enum result	0,1,3,5,7,13
response-body-framing	refinement	client::{read_response_from, read_chunked, read_to_limit, read_response}	∀ byte input B (any BufRead), limit: read_response_from(B, limit) = Ok(resp) ⇔ read_head(B) = Ok(H), decide_framing(H.code, H.headers) = Ok(f), and the body read succeeds: Empty → [] and nothing read past the head; Length(n) → n ≤ limit (else Err(TooLarge{limit})) and the next n bytes exist (else Err(Io)), body = them; ToEof → all remaining bytes, |body| ≤ limit (else TooLarge); Chunked → chunk_1 … chunk_m, then "0" plus extensions, a line ending, trailer lines and an empty line, where each size line's text before the first ';' with trailing SP/HTAB trimmed is S_i with parse_http_number(S_i, 16) = Some(s_i) > 0, followed by s_i data bytes and an empty line; a bad size or a non-empty line after the data is Err(Protocol); a running sum that overflows usize or exceeds limit is Err(TooLarge{limit}) before that chunk's data is read; body = the concatenated data. Then resp.status = H.code and resp.headers = H.headers. read_response(stream, limit) = read_response_from(BufReader::new(stream), limit).	the_parser_frames_through_decide_framing, content_length_framing, chunked_framing_reassembles_and_skips_extensions_and_trailers, a_chunk_size_is_hex_digits_and_nothing_else, eof_framing_when_no_length_is_given, a_body_over_the_limit_errors_rather_than_truncating, non_2xx_status_is_reported_not_hidden	generic R: BufRead/Read, &mut reader, nested loops, Vec resize/extend/read_exact, checked_add, composition of read_head + decide_framing, a byte-stream model of Read	0,1,2,4,7,11
response-parse-no-panic	no-panic	client::{read_response_from, read_chunked, read_line, read_to_limit}	∀ finite byte input, ∀ limit (including usize::MAX): read_response_from terminates and never panics; no arithmetic overflows, every slice range is in bounds, and the body Vec never grows past limit. Covers chunk sizes up to usize::MAX (Err(TooLarge{limit}), never a wrap) and header blocks that never end (Err).	loopback::a_chunk_size_at_the_top_of_usize_is_an_error_not_a_panic	overflow and bounds obligations inside loops, Vec resize, slice indexing body[start..], generic Read	0,1,2,7,11
response-accessors	contract	client::Response::{header, is_success, status, body}	∀ r, n: r.header(n) = the value of the FIRST (k, v) in r.headers with ci(k, n), or None; r.is_success() ⇔ 200 ≤ r.status < 300; status() and body() return the stored fields.	(secondary only; see §2)	&self view of Vec<(String,String)>, iterator find with a closure, &str return	0,1,2,5,13
render-request-exact	contract	client::render_request	∀ method, t, via, hs, body_len, with abs = (via is Some ∧ t.scheme = Http): render_request(…) is exactly (1) method + " " + RT + " HTTP/1.1\r\n", RT = t.scheme.as_str() + "://" + t.host_header() + t.path_and_query() if abs, else t.path_and_query(); (2) "Host: " + t.host_header() + "\r\n"; (3) "Proxy-Authorization: Basic " + c + "\r\n" only if abs ∧ via.basic_auth() = Some(c); (4) "Connection: close\r\n" + "Content-Length: " + dec(body_len) + "\r\n" + "Accept: */*\r\n"; (5) for each (n, v) in hs in order whose n is not ci any of host, content-length, transfer-encoding, connection, proxy-authorization: n + ": " + v + "\r\n"; (6) "\r\n". Corollaries: each reserved name appears at most once (Host and Content-Length exactly once); an https request carries no proxy credential and stays in origin form through a proxy.	a_plain_request_is_origin_form_with_host_and_length, plain_http_through_a_proxy_uses_absolute_form, the_proxy_credential_rides_the_absolute_form_and_never_the_tunnel, https_keeps_origin_form_even_through_a_proxy, a_caller_cannot_duplicate_a_generated_header	String building with format!/push_str, for loop over a slice with a nested any() over a const &[&str], Option filter/and_then	0,1,3,4,5,7,13
connect-tunnel-exchange	contract	client::connect_tunnel	∀ s, t, c with A = t.authority(): connect_tunnel writes exactly "CONNECT " + A + " HTTP/1.1\r\nHost: " + A + "\r\nProxy-Connection: keep-alive\r\n" + ("Proxy-Authorization: Basic " + c + "\r\n" if c is Some) + "\r\n", then flushes; it then calls read_head directly on the unbuffered stream, so it consumes no byte past the final head's empty line and skips interim 1xx heads; it returns Ok(()) ⇔ the final code ∈ 200..300, else Err(Protocol("proxy refused CONNECT with status {code}")), which contains neither the credential nor its decoding.	loopback::a_proxy_that_sends_an_interim_response_before_its_connect_answer_still_tunnels, loopback::userinfo_in_the_proxy_url_authenticates_the_connect	&mut Stream (socket effect), Write trait, String building, composition with read_head over a stream model	0,1,2,4,8
send-pipeline-order	refinement	client::RequestBuilder::{send_with_env, send}, client::Client::{open, handshake}	send_with_env runs in order: (1) a stored builder error is returned; (2) Uri::parse(endpoint) = None → Err(Invalid("endpoint is not a usable absolute http(s) URL")), a constant that never echoes the endpoint; (3) proxy::resolve errors are returned; no connector.connect call happens before (1)-(3) pass; (4) connect(via.host or t.host, via.port or t.port); (5) t Https → a TLS handshake against t.host() completes (after connect_tunnel when via is Some) before any request byte is written, and the request goes through the TLS arm, so the first byte the peer sees is a TLS record (0x16) and neither head nor body reaches the socket in cleartext; t Http → plain stream, and no TLS config is requested; (6) the bytes written are render_request("POST", t, via, headers, body.len()) ++ body, then a flush; (7) the result is read_response(stream, limit). send(b) = send_with_env(b, &ProcessEnv).	loopback::a_json_post_puts_the_expected_bytes_on_the_wire_and_parses_the_reply, loopback::an_https_endpoint_puts_a_client_hello_on_the_wire_and_never_the_body, loopback::an_https_proxy_is_refused_before_any_byte_reaches_it, loopback::an_unusable_endpoint_fails_before_any_socket_is_opened	trait objects (Arc<dyn Connect>, Arc<dyn Guard>), sockets, rustls, enum Stream arms, ? propagation, composition of many callee ensures	0,1,2,4,8,13
tls-config-lazy-and-shared	contract	client::TlsConfigCache::get, client::Client::{new, with_shared_trust}, <client::Client as Clone>::clone	∀ c and all its clones: Arc::ptr_eq on their trust and tls_config fields; with_shared_trust(a, ..).trust is ptr_eq to a. Over one TlsConfigCache's lifetime, tls::client_config(trust) is evaluated at most once whatever the number of get calls or threads, and every get returns a clone of that one result (Ok values ptr_eq, Err strings equal). A send to an Http target never calls get, so an unusable trust never fails a plain HTTP request.	repeated_https_requests_and_client_clones_build_one_tls_config, shared_trust_constructor_retains_the_callers_root_allocation, cached_tls_configuration_errors_are_shared_by_clones, plain_http_never_constructs_the_deferred_tls_configuration	Arc, OnceLock::get_or_init with a closure (concurrency), derived Clone, Result<Arc<_>,String> clone	0,2,8,13
error-is-revoked	contract	client::Error::is_revoked, stream::revoked_error, <client::Error as From<io::Error>>::from	∀ e: e.is_revoked() ⇔ e = Io(x) ∧ x.kind() = PermissionDenied; revoked_error().kind() = PermissionDenied; From<io::Error> wraps without changing the kind. So a connect failure (ConnectionRefused) is never reported as revoked, and a guard refusal always is.	loopback::a_refused_port_is_an_error_not_a_panic	enum match with a guard, io::Error kind view	0,1,2
stream-admit-before-every-syscall	invariant	stream::Bounded::{admit, admit_read, admit_write}, <stream::Bounded as Read/Write>, <stream::Stream as Read/Write>, stream::Stream::start_tls, stream::as_deadline_timeout	∀ Stream (plain or TLS), ∀ sequence of read/write/flush calls, including the socket calls rustls makes in complete_io and its record-read loop: each socket read, write or flush in Obs is immediately preceded, on the same thread, by an admit step: guard.is_authorized() false → Err(PermissionDenied), no socket call; deadline.remaining() None → Err(TimedOut), no socket call; otherwise only that direction's timeout is set to the remaining d (read sets the read timeout; write and flush set the write timeout). The same admit gates Stream::read/write/flush before rustls is consulted. A socket WouldBlock or TimedOut is reported as TimedOut. start_tls passes PermissionDenied and TimedOut through and returns UnexpectedEof when complete_io makes no progress mid-handshake. Consequences: once the guard turns false no further request byte reaches the socket, and no Stream operation's last socket call starts after the deadline, so a trickling peer cannot extend the total time.	plaintext_io_sets_only_its_socket_timeout_direction, a_revoked_guard_stops_writes_and_reads_on_an_open_socket, loopback::authority_revoked_before_the_write_stops_the_body_leaving_the_process, loopback::a_silent_server_hits_the_deadline_instead_of_hanging, loopback::a_dribbling_tls_peer_cannot_outlive_the_global_deadline, tls_stream::a_record_dribbled_after_the_handshake_cannot_outlive_the_global_deadline, tls_stream::authority_revoked_during_a_tls_read_stops_it	&mut self over an enum with Box<rustls::StreamOwned>, dyn Guard, atomics read on another thread, sockets and timeouts, rustls internal loops calling back into Bounded	0,2,8,12,13
deadline-remaining	contract	stream::Deadline::{after, remaining, remaining_or_timeout}	In the clock model, for d = Deadline::after(b) taken at t0 and observed at t ≥ t0: d.remaining() = Some(t0 + b − t) if t < t0 + b, else None; never Some(0); Some(r) ⇒ r ≤ b. remaining_or_timeout() = Ok(r) ⇔ remaining() = Some(r), else Err with kind TimedOut. In particular Deadline::after(0).remaining() = None at once.	an_expired_deadline_is_a_timeout_not_a_hang	Instant::now (time effect), checked_duration_since, Duration filter	0,8
init-crypto-installs-provider	contract	tls::{init_crypto, provider}	After any init_crypto() returns, rustls::crypto::CryptoProvider::get_default().is_some(). Every call after the first is a no-op, including calls from other threads and calls after another component installed its own provider.	init_crypto_is_idempotent	std::sync::Once with a closure, rustls process-global state	0,8,12,13
client-config-roots-fail-closed	contract	tls::client_config	∀ ders: client_config(Roots(ders)), first rule that applies: some element is rejected by RootCertStore::add, the first at index i → Err(m) with m starting "CA bundle certificate {i+1} is not usable"; ders empty → Err("CA bundle yielded no usable certificates"); otherwise Ok(config) whose root store holds exactly the anchors from ders, with a webpki verifier and no platform anchors (Roots replaces the platform roots). Every Roots error string contains "CA bundle".	-	for loop over enumerate, rustls RootCertStore and ClientConfig builder, String errors	0,1,7,8,12
verifier-accepts-only-through-arm	contract	<verifier::PlatformVerifier as ServerCertVerifier>::verify_server_cert, verifier::apple::cf_string	∀ inputs: verify_server_cert(ee, ims, name, ocsp, now) = Ok(_) ⇒ name.to_str() ≠ "" ∧ inner.verify(ee, ims, name, name.to_str(), ocsp-or-None, now) = Ok(()). An empty rendered name is Err without calling the arm. On Apple, a hostname cf_string cannot encode is Err, never a NULL hostname. The arm is Ok only on an explicit positive platform verdict, so an empty name or an embedded NUL never reaches an evaluation with name checking off.	verifier::tests::a_degenerate_server_name_fails_closed_instead_of_disabling_name_checking	&self, Cow<str>, trait impl, unsafe FFI to Security.framework in the arm	0,1,2,8
verifier-delegates-signatures	equivalence	<verifier::PlatformVerifier as ServerCertVerifier>::{verify_tls12_signature, verify_tls13_signature, supported_verify_schemes}, verifier::PlatformVerifier::new	∀ v = PlatformVerifier::new(p)?, ∀ inputs: v.verify_tls12_signature(m, c, d) = rustls::crypto::verify_tls12_signature(m, c, d, &p.signature_verification_algorithms); likewise for TLS 1.3; v.supported_verify_schemes() = p.signature_verification_algorithms.supported_schemes(). No narrowing of the scheme set, no stubbed result: a signature rustls rejects is rejected.	verifier::tests::handshake_signature_verification_is_delegated_to_the_provider, signature_methods::tls13_rejects_a_signature_that_verifies_against_nothing, signature_methods::tls12_rejects_a_signature_that_verifies_against_nothing	&self field view (Arc<CryptoProvider>), calls into rustls	0,2,12
provider-advertises-modern-schemes	contract	tls::provider	provider().signature_verification_algorithms.supported_schemes() is non-empty and contains ECDSA_NISTP256_SHA256 and RSA_PSS_SHA256: a fact about ring's default provider, which the one-provider seam pins.	-	rustls/ring provider constant table, Vec contains	0,1,12
platform-verifier-conformance	refinement	<verifier::PlatformVerifier as ServerCertVerifier>::verify_server_cert, verifier::PlatformVerifier::{new, build}, verifier::unix::Verifier::verify, verifier::apple::Verifier::verify, verifier::eku_rejected	∀ chain (ee, ims), name n, OCSP, pinned now: the arm's verdict refines V(anchors, ee, ims, n, now) (Apple additionally requires an explicit EKU), with anchors = platform anchors ∪ extra_roots (extras ADD to the system set) and new() having extra_roots = []. verify is Ok only if V accepts. The error class is one of: BadEncoding, reported before any trust evaluation for non-DER bytes (including empty); NotValidForName; UnknownIssuer; eku_rejected() for any webpki InvalidPurpose; other. Consequences: a chain rooted only in a fixture CA is rejected by new(); a chain a real system anchor validates is accepted for its own name and rejected for any other.	verifier::tests::{the_first_party_verifier_matches_its_recorded_expectations, the_shipped_constructor_trusts_no_fixture_anchor, the_system_trust_store_is_actually_consulted, extra_anchors_add_to_the_system_set_rather_than_replacing_it, a_parse_failure_is_reported_as_one_and_never_as_a_trust_verdict}	FFI (Security.framework, CryptoAPI), trustd deadline, rustls/webpki, fixture DER bytes, an X.509 model on the Lean side	0,1,8,10,12
unix-store-fail-closed	contract	verifier::unix::Verifier::{from_ders, new}	∀ extra, sys, provider: some extra root rejected by RootCertStore::add → from_ders is Err (strict); otherwise S = extra ∪ {d ∈ sys | webpki parses d} (unparseable system roots skipped); S empty → Err(General(m)) with m containing "system trust store"; webpki builder failure → Err(General("system trust store is unusable: ...")); otherwise Ok with a verifier over exactly S. Verifier::new(extra, p) = from_ders(extra, system_root_ders(), p): either Ok or that loud Err, never a verifier over an empty store.	verifier::unix::{an_empty_system_store_is_a_loud_failure_not_a_silent_verifier, unparseable_system_roots_are_skipped_but_do_not_take_the_store_with_them, an_unusable_extra_anchor_is_an_error_rather_than_a_skip, the_shipped_constructor_reads_this_hosts_store}	for loop over Vec, rustls RootCertStore, Result mapping, fs effects in new()	0,1,7,8,12
unix-cert-path-selection	contract	verifier::unix::{bundle_file, bundle_dirs, system_root_ders, collect_ders, read_pem_file}	Against FS: bundle_file(Some(p)) = Some(p) ⇔ FS.exists(p), else None (never a fallback to the built-in list); bundle_file(None) = the first existing path in CERTIFICATE_FILES; bundle_dirs(Some(l)) = [q ∈ split_paths(l) in order | q ≠ "" ∧ FS.exists(q)]; bundle_dirs(None) = the existing CERTIFICATE_DIRS in order; system_root_ders consults NEITHER built-in list when SSL_CERT_FILE is set, or SSL_CERT_DIR is set with a non-empty entry; collect_ders(f, ds) = sort_dedup(lossy(f) ++ concat over d ∈ ds and regular files e in d (symlinks followed; dangling or unreadable skipped) of lossy(e)), where lossy(x) = decode_certificates_lossy(read_to_string(x)), or [] when x is not readable as UTF-8.	verifier::unix::{a_configured_path_replaces_the_built_in_search, a_configured_cert_dir_is_a_list_of_directories, the_built_in_search_finds_this_machines_real_bundle}	PathBuf/OsString, std::fs (exists, read_dir, metadata, read_to_string), env::var_os, split_paths, iterator closures, Vec sort/dedup	0,1,7,8,13
```

Unqualified test names in `subsumes` are in the file that holds the spec's functions (`src/uri.rs`,
`src/pem.rs`, `src/proxy.rs`, `src/client.rs`, `src/stream.rs`, `src/tls.rs`). A `loopback::`,
`trust_config::`, `tls_stream::` or `signature_methods::` prefix is the file under `tests/`;
`verifier::tests::` and `verifier::unix::` are `src/verifier/tests.rs` and `src/verifier/unix.rs`.

### Distribution by kind (33)

| kind | count | ids |
|---|---|---|
| contract | 26 | uri-parse-accept-set, uri-parse-fields, uri-render, pem-lossy, proxy-resolve-selection, proxy-parse, proxy-refusal-names-variable-never-value, proxy-target-debug-redacts, no-proxy-match, header-validation-grammar, response-header-fields, http-number-grammar, status-line-grammar, read-head-skips-interim, response-accessors, render-request-exact, connect-tunnel-exchange, tls-config-lazy-and-shared, error-is-revoked, deadline-remaining, init-crypto-installs-provider, client-config-roots-fail-closed, verifier-accepts-only-through-arm, provider-advertises-modern-schemes, unix-store-fail-closed, unix-cert-path-selection |
| equivalence | 1 | verifier-delegates-signatures |
| refinement | 3 | response-body-framing, send-pipeline-order, platform-verifier-conformance |
| table-property | 1 | decide-framing-table |
| invariant | 1 | stream-admit-before-every-syscall |
| no-panic | 1 | response-parse-no-panic |

### Blocking-gap histogram (specs blocked by each gap)

| gap | specs | gap | specs |
|---|---|---|---|
| 0 seal admission | 33 | 7 loops | 15 |
| 1 non-scalar carriers | 28 | 8 effects/FFI | 11 |
| 2 `&self`/`&mut` views | 16 | 10 model→code refinement | 1 |
| 3 E6 body widening | 12 | 11 generics | 6 |
| 4 callee-ensures composition | 8 | 12 cross-crate citation | 10 |
| 5 quantified clauses | 13 | 13 closures/threads/`OnceLock` | 13 |

No spec is blocked by gap 0 alone. Gap 1 is the crate's bottleneck: only 5 specs avoid it
(`tls-config-lazy-and-shared`, `stream-admit-before-every-syscall`, `deadline-remaining`,
`init-crypto-installs-provider`, `verifier-delegates-signatures`), and every one of those needs
gap 8 or 12 instead.

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
come last; the conformance spec needs the X.509 model `V`.

## 2. Test → spec map (TSV)

`primary` is the spec that retires the test. `also` lists the other specs the same test samples.

```tsv
file	test	primary	also
src/client.rs	a_body_over_the_limit_errors_rather_than_truncating	response-body-framing	
src/client.rs	a_caller_cannot_duplicate_a_generated_header	render-request-exact	
src/client.rs	a_chunk_size_is_hex_digits_and_nothing_else	response-body-framing	http-number-grammar
src/client.rs	a_plain_request_is_origin_form_with_host_and_length	render-request-exact	
src/client.rs	a_status_code_is_exactly_three_digits	status-line-grammar	http-number-grammar
src/client.rs	a_transfer_coding_this_client_cannot_decode_is_refused	decide-framing-table	response-body-framing
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
src/pem.rs	the_lossy_parser_drops_undecodable_blocks_and_keeps_going	pem-lossy	
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
src/stream.rs	a_revoked_guard_stops_writes_and_reads_on_an_open_socket	stream-admit-before-every-syscall	
src/stream.rs	an_expired_deadline_is_a_timeout_not_a_hang	deadline-remaining	
src/stream.rs	plaintext_io_sets_only_its_socket_timeout_direction	stream-admit-before-every-syscall	
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
tests/loopback.rs	a_chunk_size_at_the_top_of_usize_is_an_error_not_a_panic	response-parse-no-panic	response-body-framing
tests/loopback.rs	a_dribbling_tls_peer_cannot_outlive_the_global_deadline	stream-admit-before-every-syscall	
tests/loopback.rs	a_json_post_puts_the_expected_bytes_on_the_wire_and_parses_the_reply	send-pipeline-order	response-accessors, render-request-exact
tests/loopback.rs	a_proxy_that_sends_an_interim_response_before_its_connect_answer_still_tunnels	connect-tunnel-exchange	read-head-skips-interim
tests/loopback.rs	a_refused_port_is_an_error_not_a_panic	error-is-revoked	
tests/loopback.rs	a_silent_server_hits_the_deadline_instead_of_hanging	stream-admit-before-every-syscall	
tests/loopback.rs	an_https_endpoint_puts_a_client_hello_on_the_wire_and_never_the_body	send-pipeline-order	
tests/loopback.rs	an_https_proxy_is_refused_before_any_byte_reaches_it	send-pipeline-order	
tests/loopback.rs	an_unusable_endpoint_fails_before_any_socket_is_opened	send-pipeline-order	
tests/loopback.rs	authority_revoked_before_the_write_stops_the_body_leaving_the_process	stream-admit-before-every-syscall	error-is-revoked
tests/loopback.rs	userinfo_in_the_proxy_url_authenticates_the_connect	connect-tunnel-exchange	proxy-parse
tests/signature_methods.rs	tls12_rejects_a_signature_that_verifies_against_nothing	verifier-delegates-signatures	
tests/signature_methods.rs	tls13_rejects_a_signature_that_verifies_against_nothing	verifier-delegates-signatures	
tests/tls_stream.rs	a_record_dribbled_after_the_handshake_cannot_outlive_the_global_deadline	stream-admit-before-every-syscall	
tests/tls_stream.rs	authority_revoked_during_a_tls_read_stops_it	stream-admit-before-every-syscall	
```

## 3. Coverage

- 106 tests read. 102 mapped to one of the 35 specs and 4 were not spec samples (§4). After the
  deletions since, 88 map to one of the 33 specs and none is outside a spec.
- The biggest collapses: `proxy-resolve-selection` (7 primary), `response-body-framing` (7),
  `stream-admit-before-every-syscall` (7, across three test files), `uri-parse-fields` (6), then
  `render-request-exact`, `no-proxy-match` and `platform-verifier-conformance` (5 each).
- `http-number-grammar`, `uri-render`, `proxy-target-debug-redacts` and `response-accessors` retire
  no test alone, but the larger specs cite them as lemmas, and they are a secondary spec of 10
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
4. **A latency budget that passes vacuously.**
   `loopback::authority_revoked_before_the_write_stops_the_body_leaving_the_process` waits on the server
   with `recv_timeout(5 s).unwrap_or_default()`. That is a latency budget, against the AGENTS.md
   hang-detector rule, and a timeout yields an empty buffer that passes. The guard is also false from
   the start, so the test checks revocation before connect, not while the request is in flight;
   `TcpConnector` opens the socket without consulting the guard. `stream-admit-before-every-syscall`
   states the in-flight property.
5. **An unchecked error kind.** `loopback::a_silent_server_hits_the_deadline_instead_of_hanging`
   discards the error (`let _ = error`) and never checks that it is a `TimedOut` Io error; only the
   elapsed time is bounded. The spec requires `Err(TimedOut)`.

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
