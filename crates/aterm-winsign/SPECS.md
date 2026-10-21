<!-- Copyright 2026 Andrew Yates -->
<!-- SPDX-License-Identifier: Apache-2.0 -->

# aterm-winsign: spec recovery from its 22 unit tests

> Each of the 19 specs below is proved through Trust/Clean about the compiled Rust function, and
> then the tests it subsumes are deleted. The one test that samples no spec (§4) is deleted with
> the test-only `Fake` host once the host-level tests are replaced.

Source: `crates/aterm-winsign/src/lib.rs`, which holds all 22 `#[test]` functions in `mod tests`.

Result: **19 specs**, covering **21 tests**. **1 test is not a spec sample** (§4).

Gap numbers are the rows of the table in
`AUDIT-trust-for-test-migration-2026-10-15.md` (historical internal audit) §7.
Gap 0 (a seal that publishes Clean rows) is a prerequisite for every row, so the
`blocking_gaps` column leaves it out. Seal `150efe4a` closes it for local work
(`PLAN-tests-to-specs-2026-10-15.md` (historical internal plan)), but
it has not been admitted to the machine's `trust` channel. **No spec here is blocked by gap 0
alone.** Every one needs gap 1 (non-scalar carriers), because the crate works on `&str`,
`String`, `Vec`, `PathBuf`, `Option` and enums throughout.

## 0. Notation used in the statements

- `text(ran)` = `ran.stdout ++ "\n" ++ ran.stderr`. `Lines(text)` = `str::lines`, which strips
  `"\n"` and `"\r\n"`. For a line `L`, `trim(L)` = `str::trim`, and `depth(L)` = the byte index of
  the first non-whitespace char of `L`, or `|L|` when there is none.
- `s ⊑ t`: `s` is a substring of `t`.
- `Chain(ran)`: the list of certs `(depth, to, by)` that `parse_verify_output`'s line loop collects.
  - Collection is on only inside the FIRST `"Signing Certificate Chain:"` block. A later header
    turns it on again only while the list is still empty.
  - It turns off after a line whose `trim` starts with `"The signature is timestamped"`,
    `"Timestamp Verified by:"`, `"File has page hashes"` or `"Successfully verified"`.
  - Each `"Issued to: v"` line pushes `(depth(L), trim(v), None)`.
  - An `"Issued by: v"` line sets `by = trim(v)` on the last cert, but only when that cert has the
    same depth and its `by` is still `None`.
- `Leaf(ran)` = `last(Chain)` when `Chain ≠ []` and `∀ c ∈ init(Chain): c.depth < last.depth`.
  Otherwise `Leaf(ran) = ⊥`. A one-cert chain is its own leaf.
- `Lane`, `Tier`, `VerifyReport` and `Config` are the crate's types. `Tier::Active { publisher }`
  is written `Active(p)`.
- `WJ(b, ps)` = `win_join` on encoded bytes. It is a `foldl` over `ps` of
  `(acc, p) ↦ acc ++ (if last(acc) ∉ {⊥, '\\', '/', ':'} then "\\" else "") ++ p`, starting
  from `bytes(b)`.
- `Vers(ns)` = `versions_newest_first(ns)`.
- `get(k)`: the value of the first source `s` in `resolve_config`'s source order with `k ∈ dom(s)`,
  or `None`.
- **Host model `H`.** `H` is an abstract, read-only (`&self`) host:
  - `env: String ⇀ String`, `is_file: Path → bool`, `list_dir: Path → [String]`,
    `home: Option<Path>`, `temp_dir`, and `sdk_arch: &'static str`.
  - `run` appends `(program, argv, env)` to an observable trace `Runs(H)` and returns a reply.
  - The finder specs treat `H.is_file` and `H.list_dir` as opaque predicates. The test-only
    `win_path` canonicaliser therefore plays no part in any spec.
- `Cands_signtool(H)`, the `cfg = None` search list, in order:
  1. The PATH candidates from `find_on_path`: for each dir in `split_paths(H.env(PATH))`, try
     `prog`, `prog.exe` and `prog.cmd`.
  2. Then, for each `root_var` in `[ProgramFiles(x86), ProgramFiles]` with
     `H.env(root_var) = Some(r)`, and each `v` in `Vers(H.list_dir(WJ(r, [Windows Kits, 10, bin])))`:
     `WJ(bin, [v, sdk_arch, signtool.exe])`.
- `Cands_dlib(H)`:
  - `cache` = `H.env(NUGET_PACKAGES)`, else `WJ(H.home, [.nuget, packages])`.
  - Then, for each `pkg` in `DLIB_PACKAGES` (artifactsigning first) and each `v` in
    `Vers(list_dir(WJ(cache, [pkg])))`: `WJ(pkg_dir, [v, bin, sdk_arch, Azure.CodeSigning.Dlib.dll])`.
- `T` = `tier()` = `tier_from(pins::WINDOWS_SIGNING_PUBLISHER)`. `run` reads this constant, so every
  run-level statement is parametric in `T`. Today `T = Inactive` (see §5.1).

## 1. Specs (TSV)

Columns: `id`, `kind`, `functions`, `statement`, `subsumes` (primary tests; secondary ones are in
§2), `rust_features` (what the proof must handle), and `blocking_gaps` (audit §7 numbers, gap 0
omitted).

```tsv
id	kind	functions	statement	subsumes	rust_features	blocking_gaps
winsign-tier-from	contract	tier_from, aterm_update_core::pins::anchor_active	∀ a:&str: tier_from(a) = Inactive if a = "", else Active(a.to_string()). The result depends only on a.	the_tier_is_the_anchor_and_nothing_else	&str emptiness, String in an enum variant, cross-crate const fn call	1, 3, 12
winsign-verify-flags	contract	parse_verify_output	∀ ran, with r = parse_verify_output(&ran) and t = text(ran): r.verified ⇔ ran.code = Some(0) ∧ "Successfully verified" ⊑ t; r.signed ⇔ ¬("No signature found" ⊑ t); r.untrusted_root ⇔ "not trusted by the trust provider" ⊑ t ∨ "certificate chain could not be built" ⊑ t; r.timestamped ⇔ "The signature is timestamped" ⊑ t. parse_verify_output never panics.	a_self_signed_chain_is_signed_but_not_verified, an_unsigned_file_reads_as_unsigned	String format!/contains, Option<i32> compare, struct update syntax	1, 3
winsign-verify-leaf	contract	parse_verify_output	∀ ran: (r.issued_to, r.issued_by) = (Some(Leaf(ran).to), Leaf(ran).by) if Leaf(ran) ≠ ⊥, else (None, None), with Chain and Leaf as in §0 (first chain block only; the leaf is the last cert and is strictly deeper than every cert before it). Corollary (line-ending invariance): replacing every "\n" by "\r\n" in stdout and stderr leaves parse_verify_output unchanged.	a_verified_chain_is_read_back_as_its_leaf_not_its_root	loop over str::lines, local struct Vec<Cert> push/last_mut, &mut through Option::filter, strip_prefix/trim/find with a char closure, split_last + all over a slice, a bool state flag	1, 2, 3, 5, 7, 13
winsign-judge	table-property	judge	∀ r: judge(Inactive, r) = Ok(()). ∀ p, r: judge(Active(p), r) = Ok(()) ⇔ r.signed ∧ r.verified ∧ r.issued_to = Some(p) ∧ r.timestamped. On Err the message names the FIRST failing check, in this order: (1) ¬signed gives the no-signature message; (2) ¬verified gives the untrusted-root message when r.untrusted_root, else the policy message; (3) issued_to = Some(g) with g ≠ p gives a message containing "signed by `g`" and "is `p`", and issued_to = None gives the no-chain message; (4) ¬timestamped gives the not-timestamped message.	the_active_tier_refuses_everything_but_the_anchored_trusted_timestamped_leaf	enum match, Option<String> as_deref compare, String/format! in Err, early return	1, 3
winsign-pin-is-leaf	other (composition)	judge, parse_verify_output, tier_from	∀ p ≠ "", ran: judge(tier_from(p), parse_verify_output(&ran)) = Ok ⇒ Leaf(ran) ≠ ⊥ ∧ Leaf(ran).to = p ∧ ran.code = Some(0). In particular, a pin equal to the name of a non-leaf cert c ∈ init(Chain(ran)) with c.to ≠ Leaf(ran).to is refused, and a block whose shape names no leaf (e.g. two certs at one depth) is refused for every p.	the_pin_names_one_leaf_and_never_the_root_it_chains_to	caller-side composition of winsign-tier-from, winsign-verify-flags, winsign-verify-leaf and winsign-judge	1, 4, 5
winsign-sign-verify-args	contract	sign_args, verify_args	∀ lane, ts, dlib, meta, files: sign_args(lane, ts, dlib, meta, files) = [sign, /v, /fd, SHA256, /tr, ts, /td, SHA256] ++ M ++ map(display, files), where M = [/dlib, dlib.display or "", /dmdf, meta.display or ""] for TrustedSigning and M = [/sha1, t] for Thumbprint(t). Hence /fd SHA256 and /tr ts /td SHA256 are always present, and no /f or /p token appears outside the caller-supplied ts, t, dlib, meta and file strings. ∀ f: verify_args(f) = [verify, /pa, /v, f.display()].	sign_args_per_lane_carry_a_sha256_digest_and_a_timestamp	Vec<String> literal + push + extend over an iterator map, Path::display to_string, enum match, Option map/unwrap_or_default	1, 3, 7
winsign-default-timestamp	table-property	default_timestamp	default_timestamp(TrustedSigning{..}) = "http://timestamp.acs.microsoft.com" and default_timestamp(Thumbprint(_)) = "http://timestamp.digicert.com".	(secondary only: sign_args_per_lane_carry_a_sha256_digest_and_a_timestamp)	enum match returning &'static str	1
winsign-metadata-json-roundtrip	round-trip	trusted_signing_metadata	∀ e, a, p: &str: trusted_signing_metadata(e, a, p) = "{\n  \"Endpoint\": \"" ++ esc(e) ++ "\",\n  \"CodeSigningAccountName\": \"" ++ esc(a) ++ "\",\n  \"CertificateProfileName\": \"" ++ esc(p) ++ "\"\n}\n". esc(s) is a valid JSON string body: no unescaped '"', no unescaped '\\', no char < 0x20; and json_unescape(esc(s)) = s. Hence a JSON parse of the output yields exactly {Endpoint: e, CodeSigningAccountName: a, CertificateProfileName: p}.	metadata_json_escapes_what_a_name_could_carry	nested fn, loop over chars with match guards, String push/push_str, format! with {:04x}, a char-to-u32 cast; needs a Lean JSON-string unescape model	1, 3, 5, 7, 14
winsign-profile-grammar	contract	values_from_profile	∀ text: each line is trimmed. Blank lines and lines starting with '#' are skipped. Every other line must contain '=', else Err; it is split at the first '='. A line whose trim(key) lacks the prefix "winsign_" is ignored. Otherwise bare = key minus the prefix, and bare ∉ KEYS gives Err. v = trim(value) must be '"' ++ w ++ '"' (else Err) with w ≠ "" (else Err). Then out[bare] = w verbatim, with no escape processing, and a later line with the same bare key overwrites. Ok(out) ⇔ no line errs, and then dom(out) ⊆ KEYS.	the_profile_grammar_refuses_what_the_release_cutter_refuses	loop over lines with early return, split_once/strip_prefix/strip_suffix, slice contains over &[&str], BTreeMap insert, ? on Option→Result	1, 3, 5, 7
winsign-config-precedence	contract	resolve_config	∀ sources, when resolve_config(sources) = Ok(c): c.timestamp = get(timestamp), c.signtool = get(signtool).map(PathBuf::from), and every lane field equals get of its key (dlib = get(dlib).map(PathBuf::from)). Merging is PER KEY across sources. A later source never overrides an earlier one, and the result depends on sources only through get.	precedence_is_flag_then_env_then_profile_and_the_lane_is_inferred	closure over &[&Values] with iter().find_map, BTreeMap get/cloned, Option<String> plumbing	1, 3, 5, 7, 13
winsign-config-lane	table-property	resolve_config	Let L = get(lane), TS = get(endpoint) ≠ None ∨ get(account) ≠ None ∨ get(profile) ≠ None, and TH = get(thumbprint). The first matching row decides. (a) L ∈ {Some(trusted-signing), None} ∧ TS: if L = None ∧ TH ≠ None, Err mentioning --lane; elif any of the three is missing, Err naming endpoint/account/profile; else Ok(TrustedSigning{endpoint, account, profile, dlib}). (b) L = Some(trusted-signing) ∧ ¬TS: Err. (c) L ∈ {Some(thumbprint), None} ∧ TH = Some(t): with k = t minus whitespace, Err containing "40 hex" if |k| ≠ 40 or some char of k is not ASCII hex, else Ok(Thumbprint(uppercase(k))). (d) L = Some(thumbprint) ∧ TH = None: Err. (e) L = Some(other): Err (unknown lane). (f) otherwise (L = None, ¬TS, TH = None): Ok with lane = None.	(secondary only: precedence_is_flag_then_env_then_profile_and_the_lane_is_inferred)	match on Option<&str> with or-patterns under a guard, let-else on a tuple of Options, chars filter/collect, all(is_ascii_hexdigit), to_ascii_uppercase	1, 3, 5, 7, 13
winsign-win-join	contract	win_join	∀ base, parts: win_join(base, parts).as_os_str().as_encoded_bytes() = WJ(base, parts), byte for byte, on every build host. Corollaries: empty parts gives bytes(base); a separator is never doubled after a trailing '\\' or '/'; no separator follows a trailing ':' (C: + x = C:x); separators the caller wrote are left untouched. This byte equality is stronger than the component-wise PathBuf equality the test asserts.	windows_paths_are_joined_with_backslashes_on_every_host	generic impl Into<PathBuf> + IntoIterator<Item=&str>, OsString push, as_encoded_bytes last, loop	1, 3, 7, 11
winsign-versions-newest-first	contract	versions_newest_first	∀ names: let K(n) = n.split('.').map(parse::<u64>) when every piece parses, else ⊥. Vers(names) is the sub-list of names with K(n) ≠ ⊥, sorted by K descending in lexicographic Vec<u64> order. The sort is stable, so names with equal K (e.g. "1.0" and "01.0") keep their input order. Hence numeric, not string, order (1.0.52 > 1.0.10 > 1.0.9), and every non-numeric name, "" included, is dropped.	versions_sort_numerically	generic IntoIterator, split/parse collect into Result<Vec<u64>>, filter_map with ?, Vec sort_by with a closure (stable-sort model), lexicographic Vec Ord	1, 3, 5, 7, 11, 13
winsign-find-signtool	contract	find_signtool, find_on_path, win_join, versions_newest_first	∀ H, cfg. If cfg = Some(p): Ok(p) when H.is_file(p), else Err; nothing else is consulted. If cfg = None: Ok(c) for the FIRST c in Cands_signtool(H) with H.is_file(c), else Err(msg), where msg contains "signtool on PATH" and, for each present root r, every SDK candidate tried under r, or WJ(r, [Windows Kits, 10, bin, <10.x.y.z>]) when Vers(list_dir) = []. The search is pure over H: no H.run and no writes.	signtool_is_found_newest_sdk_first_and_every_miss_is_named	&dyn Host trait-object calls, nested for loops with early return, Vec<String> push, join, format!, std::env::split_paths	1, 3, 5, 7, 13
winsign-find-dlib	contract	find_dlib, win_join, versions_newest_first	∀ H, cfg. If cfg = Some(p): Ok(p) when H.is_file(p), else Err. If cfg = None: Err (no NuGet cache) when H.env(NUGET_PACKAGES) = None ∧ H.home = None; otherwise Ok(c) for the FIRST c in Cands_dlib(H) with H.is_file(c), with packages in DLIB_PACKAGES order (artifactsigning before trusted.signing) and versions newest first inside each package; else Err naming, for every package, each tried path or WJ(pkg_dir, [<version>]). Corollary: any present dll of the newer lineage beats any of the older lineage, whatever their versions.	the_dlib_is_found_under_the_nuget_cache_newest_version_first, the_renamed_package_lineage_wins_over_the_unlisted_one	&dyn Host calls, Option or_else chain, nested loops with early return, Vec<String>, format!	1, 3, 5, 7, 13
winsign-find-dotnet-runtime	contract	find_dotnet_runtime, dotnet_version_under, versions_newest_first	∀ H: the candidate roots are, in order, (H.env(DOTNET_ROOT), false), (WJ(PF, [dotnet]), false) when ProgramFiles = Some(PF), and (WJ(LA, [Microsoft, dotnet]), true) when LOCALAPPDATA = Some(LA); each is present only when its variable is set. Let ver(root) = the first v in Vers(H.list_dir(WJ(root, [shared, Microsoft.NETCore.App]))) whose first dotted piece parses to a u64 ≥ 8. Result = Ok{root, version: ver(root), needs_root_env: flag} for the FIRST candidate with ver ≠ ⊥, else Err containing WJ(root, [shared, Microsoft.NETCore.App, <8+>]) for every candidate. Hence needs_root_env ⇔ the chosen root is the user-local one.	the_dotnet_runtime_is_found_global_first_and_user_local_needs_dotnet_root	&dyn Host calls, Vec<(PathBuf,bool)>, loop with early return, iterator find with a parse closure, is_some_and	1, 3, 5, 7, 13
winsign-run-exit-classes	contract	run, run_inner, parse_flags, report_file	∀ args, H, T: run(args, H) ∈ {0, 1, 2}. 0 when the verb is help (no verb, `help`, or --help), and then Runs(H) stays empty. 2 ⇔ run_inner returns Err: a parse_flags error (unknown --opt, missing value), an unknown verb, an unreadable or ungrammatical credentials file, a resolve_config Err, no files, a sign file with ¬H.is_file, sign with lane = None, a find_signtool/find_dlib/find_dotnet_runtime Err, an H.write Err, or an H.run Err; in every one of these except the H.run Err itself, no signtool sign is in Runs(H). 1 ⇔ signtool sign ran and replied code ≠ Some(0), or some file's verdict was rejected (winsign-run-verify-verdict). Exits 1 and 2 have disjoint causes: a recorded, refused sign never yields 2.	sign_without_a_lane_refuses_by_name_and_help_exits_zero, a_signtool_that_ran_and_refused_is_exit_one_not_two	&dyn Host, Result/? plumbing across many callees, String matching on verb, loops over files, println!/eprintln! effects, std::fs::remove_file and std::process::id outside the Host seam, a committed const read via tier()	1, 3, 4, 7, 8, 13
winsign-run-sign-protocol	contract	run, run_inner, report_file, sign_args, verify_args, default_timestamp, trusted_signing_metadata	∀ H, files ≠ [] all is_file, with resolve_config = Ok(c), c.lane = Some(lane), st = find_signtool(H, c.signtool) Ok (and, for TrustedSigning, dlib d and runtime rt Ok): Runs(H) after run([sign] ++ files) = [(st, sign_args(lane, c.timestamp or default_timestamp(lane), d?, meta?, files), E)] ++ (if that reply's code = Some(0) then [(st, verify_args(f), []) for f in files] else []). For TrustedSigning, meta = H.temp_dir()/aterm-winsign-<pid>.json, and H.write(meta, trusted_signing_metadata(endpoint, account, profile)) happens before the run. E = [(DOTNET_ROOT, rt.root.display())] when lane is TrustedSigning ∧ rt.needs_root_env, else []. The verify launches always carry an empty env.	sign_with_a_thumbprint_runs_signtool_then_reads_the_signature_back, a_trusted_signing_sign_passes_dotnet_root_for_a_user_local_runtime	trace of &dyn Host::run calls (abstract host with an append-only run log), Vec<(String,String)>, loops, the fs::remove_file side effect, process::id	1, 3, 4, 7, 8, 13
winsign-run-verify-verdict	contract	run, run_inner, report_file, judge, parse_verify_output	∀ T, H, files ≠ [] with signtool found: for verify (and for sign after a successful signtool sign), file f is accepted ⇔ judge(T, r_f) = Ok ∧ r_f.verified, where r_f = parse_verify_output(reply to (st, verify_args(f), [])). The exit code is 0 ⇔ every file is accepted, else 1. In particular, under T = Inactive an unsigned or untrusted-chain exe still exits 1 and a verified one exits 0. Every file is reported; there is no early stop.	verify_reports_an_untrusted_chain_with_exit_one_even_when_inactive	&dyn Host::run, a loop accumulating a code, println!/eprint! effects, composition with judge and parse_verify_output	1, 3, 4, 7, 8, 13
```

### Distribution by kind (19)

| kind | count | ids |
|---|---|---|
| contract | 14 | winsign-tier-from, winsign-verify-flags, winsign-verify-leaf, winsign-sign-verify-args, winsign-profile-grammar, winsign-config-precedence, winsign-win-join, winsign-versions-newest-first, winsign-find-signtool, winsign-find-dlib, winsign-find-dotnet-runtime, winsign-run-exit-classes, winsign-run-sign-protocol, winsign-run-verify-verdict |
| table-property | 3 | winsign-judge, winsign-default-timestamp, winsign-config-lane |
| round-trip | 1 | winsign-metadata-json-roundtrip |
| other | 1 | winsign-pin-is-leaf (composition) |

### Blocking gaps (audit §7), specs blocked by each

| gap | specs | note |
|---|---|---|
| 0 | 19 | prerequisite for all; omitted from the column |
| 1 | 19 | non-scalar carriers; the binding gap for this crate |
| 3 | 17 | E6 body widening (every spec but default-timestamp and pin-is-leaf) |
| 7 | 14 | loops |
| 5 | 10 | quantified clauses |
| 13 | 10 | closures (iterator adaptors, sort_by, find_map) |
| 4 | 4 | callee-ensures composition (pin-is-leaf and the three run specs) |
| 8 | 3 | effects (the three run specs) |
| 11 | 2 | generics (win-join, versions-newest-first) |
| 2 | 1 | `&mut` through `last_mut`/`Option::filter` (verify-leaf) |
| 12 | 1 | cross-crate citation of `pins::anchor_active` (tier-from) |
| 14 | 1 | Clean elaborator, for the JSON unescape model (metadata round-trip) |

### Proof dependency order

`winsign-default-timestamp`, `winsign-tier-from`, `winsign-judge`, `winsign-verify-flags`,
`winsign-sign-verify-args` → `winsign-verify-leaf` → `winsign-pin-is-leaf` (composition).
Independently: `winsign-win-join`, `winsign-versions-newest-first` → `winsign-find-signtool`,
`winsign-find-dlib`, `winsign-find-dotnet-runtime`. Also independent: `winsign-profile-grammar`,
`winsign-config-precedence`, `winsign-config-lane`, and `winsign-metadata-json-roundtrip`. The
three `winsign-run-*` specs come last; they compose every spec above under the host trace.

## 2. Test → spec map (TSV; all 22)

`primary` is the spec that retires the test. `also` lists the other specs the same test samples.
All tests are in `crates/aterm-winsign/src/lib.rs` `mod tests`.

```tsv
file	test	primary	also
src/lib.rs	the_tier_is_the_anchor_and_nothing_else	winsign-tier-from	
src/lib.rs	a_verified_chain_is_read_back_as_its_leaf_not_its_root	winsign-verify-leaf	winsign-verify-flags
src/lib.rs	the_pin_names_one_leaf_and_never_the_root_it_chains_to	winsign-pin-is-leaf	winsign-verify-leaf, winsign-judge
src/lib.rs	a_self_signed_chain_is_signed_but_not_verified	winsign-verify-flags	winsign-verify-leaf
src/lib.rs	an_unsigned_file_reads_as_unsigned	winsign-verify-flags	winsign-verify-leaf
src/lib.rs	the_active_tier_refuses_everything_but_the_anchored_trusted_timestamped_leaf	winsign-judge	
src/lib.rs	sign_args_per_lane_carry_a_sha256_digest_and_a_timestamp	winsign-sign-verify-args	winsign-default-timestamp
src/lib.rs	metadata_json_escapes_what_a_name_could_carry	winsign-metadata-json-roundtrip	
src/lib.rs	precedence_is_flag_then_env_then_profile_and_the_lane_is_inferred	winsign-config-precedence	winsign-config-lane, winsign-profile-grammar
src/lib.rs	the_profile_grammar_refuses_what_the_release_cutter_refuses	winsign-profile-grammar	
src/lib.rs	the_fake_spells_paths_the_way_the_windows_machine_it_models_would	(none; §4)	
src/lib.rs	signtool_is_found_newest_sdk_first_and_every_miss_is_named	winsign-find-signtool	winsign-win-join, winsign-versions-newest-first
src/lib.rs	the_dlib_is_found_under_the_nuget_cache_newest_version_first	winsign-find-dlib	winsign-versions-newest-first
src/lib.rs	windows_paths_are_joined_with_backslashes_on_every_host	winsign-win-join	
src/lib.rs	the_renamed_package_lineage_wins_over_the_unlisted_one	winsign-find-dlib	
src/lib.rs	versions_sort_numerically	winsign-versions-newest-first	
src/lib.rs	sign_without_a_lane_refuses_by_name_and_help_exits_zero	winsign-run-exit-classes	winsign-config-lane
src/lib.rs	sign_with_a_thumbprint_runs_signtool_then_reads_the_signature_back	winsign-run-sign-protocol	winsign-run-verify-verdict, winsign-sign-verify-args
src/lib.rs	the_dotnet_runtime_is_found_global_first_and_user_local_needs_dotnet_root	winsign-find-dotnet-runtime	
src/lib.rs	a_signtool_that_ran_and_refused_is_exit_one_not_two	winsign-run-exit-classes	winsign-run-sign-protocol
src/lib.rs	a_trusted_signing_sign_passes_dotnet_root_for_a_user_local_runtime	winsign-run-sign-protocol	winsign-run-exit-classes, winsign-find-dotnet-runtime
src/lib.rs	verify_reports_an_untrusted_chain_with_exit_one_even_when_inactive	winsign-run-verify-verdict	winsign-run-exit-classes, winsign-verify-flags
```

## 3. Coverage

- 22 tests read: 21 map to one of the 19 specs, and 1 samples no spec (§4).
- The biggest collapses are `winsign-verify-leaf` (1 primary test, 4 in all) and
  `winsign-verify-flags` (2 primary, 4 in all). The run specs share 5 tests among 3 specs.
- `winsign-default-timestamp` and `winsign-config-lane` have no primary test. Each is sampled only
  as a side assertion of another test, but both are needed before that test can be deleted:
  `sign_args_per_lane_…` checks the default URLs, and `precedence_…` checks lane inference.
- Once `winsign-run-*` are proved, the test-only `Fake` host and its `win_path` canonicaliser
  have no remaining user and are deleted too.

## 4. Tests that are not spec samples

| test | why | action |
|---|---|---|
| the_fake_spells_paths_the_way_the_windows_machine_it_models_would | It tests `win_path`, a `#[cfg(test)]`-only path canonicaliser inside the `Fake` host, which has no production caller. The finder specs treat `H.is_file`/`H.list_dir` as opaque predicates, so the canonicaliser plays no part in any spec. | **delete**, together with the `Fake`, once the host-level tests are replaced |

Count by action: delete 1.

## 5. Implied specs that look wrong, or disagree with the code

1. **The run-level tests depend on a committed constant.** Every run-level test
   (`sign_without_a_lane_…`, `sign_with_a_thumbprint_…`, `a_signtool_that_ran_…`,
   `a_trusted_signing_sign_…`, `verify_reports_an_untrusted_chain_…_even_when_inactive`) silently
   depends on `pins::WINDOWS_SIGNING_PUBLISHER` being `""`, because `run_inner` reads `tier()`
   itself.
   - If the anchor is set to any name other than "Andrew Yates", the VERIFIED case of
     `verify_reports_…` flips from exit 0 to exit 1.
   - The Active tier is never exercised through `run`.

   **Recommendation:** make `run_inner` take the `Tier` as a parameter (`run` passes `tier()`), and
   state the run specs over every `T`, as §1 already does.
2. **One doc principle does not hold for `signed`.** `parse_verify_output`'s doc says "absence of a
   marker reads as absence of the property". `signed` is the inverse:
   `signed = !contains("No signature found")`, so empty or unrecognised output reads as signed.
   This does not change any verdict, because `verified` is still false, and both `judge(Active)` and
   `report_file` reject. But the stated principle is false for this field. `winsign-verify-flags`
   states the code as written. Either fix the doc or make `signed` positive-evidence.
3. **Dead check in `versions_newest_first`.** The `(!parts.is_empty())` guard is dead: `str::split`
   always yields at least one piece, and a piece that does not parse already drops the name. Also,
   `str::parse::<u64>` accepts a leading `+`, so `"+1.0"` is kept as version `[1, 0]`. The spec's
   `K` inherits this through `parse`; it is harmless for SDK and NuGet directory names.
4. **The PATH stage is host-native.** `find_on_path` applies host-native `std::env::split_paths` to
   the Host's PATH value. On a non-Windows build host, a Windows PATH like `C:\a;C:\b` is split on
   `':'`. So the PATH stage of `find_signtool` cannot model the Windows machine the way `win_join`
   does for the SDK stage. No test reaches this, because no `Fake` sets PATH.
   **Recommendation:** split on `';'` explicitly, as `win_join` does for joining. `Cands_signtool`
   can then be stated host-independently.
5. **Two tests pin error prose.** `the_pin_names_one_leaf_…` and `the_active_tier_refuses_…` check
   error text with substrings ("root", "timestamped", "no signature"). `winsign-judge` keeps only
   which check failed first, plus the got/publisher names, and not the wording.
6. **`run` has effects outside the Host seam.** `std::fs::remove_file` of the metadata file and
   `std::process::id()` in its name are reached directly, not through `H`. The run specs can only
   be stated over `Runs(H)` if these move behind the Host, or if the spec abstracts them. Moving
   them is the cleaner fix, and it matches AGENTS.md's injected-field rule.

## 6. Proof order (easiest high-value first)

No spec is blocked by gap 0 alone. Every one needs gap 1, so the order is chosen by what else
each one needs.

1. **`winsign-default-timestamp`** (gap 1 only). An enum match returning a `&'static str`. It is
   the smallest gap-1 carrier probe in the repo (an enum in, a string literal out), and it frees
   half of `sign_args_per_lane_…`.
2. **`winsign-judge`** (gaps 1, 3). A pure function over a 4-bool + `Option<String>` record, with
   an enum match and early returns. It is the security core of Tier WINDOWS: it retires
   `the_active_tier_refuses_…` and is the last link of `winsign-pin-is-leaf`.
3. **`winsign-tier-from`** (gaps 1, 3, 12). Trivial once `&str` emptiness is a carrier fact. Cite
   `anchor_active` across crates, or inline its `a.is_empty()` test to drop gap 12.
4. **`winsign-verify-flags`** (gaps 1, 3). Four substring facts and one `Option<i32>` compare, with
   no loops. Retires two tests and is a lemma of `winsign-pin-is-leaf`.
5. **`winsign-sign-verify-args`** (gaps 1, 3, 7). Straight-line `Vec<String>` construction plus
   one `extend` over a map. It pins the SHA-256 and timestamp flags that Code Integrity requires.

Then `winsign-verify-leaf` → `winsign-pin-is-leaf`; this pair is the highest-value property in the
crate, since it ensures a pin can only ever match the leaf. Then `winsign-win-join` and
`winsign-versions-newest-first`, the finders, and the config specs. The three `winsign-run-*`
specs come last. They wait on gap 8 and on the §5.1 and §5.6 refactors, which should land first,
because they make the specs statable at all.
