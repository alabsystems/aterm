<!-- Copyright 2026 Andrew Yates -->
<!-- SPDX-License-Identifier: Apache-2.0 -->

# aterm-winsign: spec recovery from its unit tests

> Each of the 17 specs below is proved through Trust/Clean about the compiled Rust function, and
> then the tests it subsumes are deleted. The one test that sampled no spec (§4) is deleted.

Source: `crates/aterm-winsign/src/lib.rs`, which holds all 21 `#[test]` functions in `mod tests`.

Result: **17 specs**, covering **24 tests**. **No test is outside a spec** (§4).

Gap numbers are the rows of the table in
`docs/TRUST-LANE.md` (Blocking gaps).
Gap 0 (a seal that publishes Clean rows) is a prerequisite for every row, so the
`blocking_gaps` column leaves it out. Seal `150efe4a` closes it for local work
(`docs/TRUST-LANE.md`), but
it has not been admitted to the machine's `trust` channel. **No spec here is blocked by gap 0
alone.** Every one needs gap 1 (non-scalar carriers), because the crate works on `&str`,
`String`, `Vec`, `PathBuf`, `Option` and enums throughout.

## 0. Notation used in the statements

- `text(ran)` = `ran.stdout ++ "\n" ++ ran.stderr`. `s ⊑ t`: `s` is a substring of `t`.
- `Chain(ran)`: the certs `(depth, to, by)` that `parse_verify_output` collects over the
  `str::lines` of `text(ran)` (which strips `"\n"` and `"\r\n"`). For a line `L`, `trim(L)` =
  `str::trim` and `depth(L)` = the byte index of its first non-whitespace char, or `|L|`.
  - Collection is on inside the first `"Signing Certificate Chain:"` block; a later header turns it
    on again only while the list is empty.
  - It turns off after a line whose `trim` starts with `"The signature is timestamped"`,
    `"Timestamp Verified by:"`, `"File has page hashes"` or `"Successfully verified"`.
  - `"Issued to: v"` pushes `(depth(L), trim(v), None)`. `"Issued by: v"` sets `by = trim(v)` on
    the last cert when it has the same depth and `by = None`.
- `Leaf(ran)` = `last(Chain)` when `Chain ≠ []` and every earlier cert is strictly shallower;
  otherwise `⊥`. A one-cert chain is its own leaf.
- `Active(p)` = `Tier::Active { publisher: p }`.
- `WJ(b, ps)` = `win_join` on encoded bytes: a `foldl` over `ps` from `bytes(b)` of
  `(acc, p) ↦ acc ++ (if last(acc) ∉ {⊥, '\\', '/', ':'} then "\\" else "") ++ p`.
- `Vers(ns)` = `versions_newest_first(ns)`.
- **Host model `H`.** `H` is an abstract (`&self`) host:
  - `env: String ⇀ String`, `is_file: Path → bool`, `list_dir: Path → [String]`,
    `home: Option<Path>`, `temp_dir`, `pid: u32`, and `sdk_arch: &'static str`.
  - `write`, `remove_file`, and `run` append `(path, text)`, `path`, and `(program, argv, env)` to
    observable traces on `H`, and `run` returns a reply.
  - The finder specs treat `H.is_file` and `H.list_dir` as opaque predicates. The test-only
    `win_path` canonicaliser therefore plays no part in any spec.
- `Cands_signtool(H)`, the `cfg = None` search list, in order:
  1. The PATH candidates from `find_on_path`: for each non-empty trimmed, unquoted dir in
     `H.env(PATH).split(';')`, try `WJ(dir, [prog])`, `WJ(dir, [prog.exe])`, and `WJ(dir, [prog.cmd])`.
  2. Then, for each `root_var` in `[ProgramFiles(x86), ProgramFiles]` with
     `H.env(root_var) = Some(r)`, and each `v` in `Vers(H.list_dir(WJ(r, [Windows Kits, 10, bin])))`:
     `WJ(bin, [v, sdk_arch, signtool.exe])`.
- `Cands_dlib(H)`:
  - `cache` = `H.env(NUGET_PACKAGES)`, else `WJ(H.home, [.nuget, packages])`.
  - Then, for each `pkg` in `DLIB_PACKAGES` (artifactsigning first) and each `v` in
    `Vers(list_dir(WJ(cache, [pkg])))`: `WJ(pkg_dir, [v, bin, sdk_arch, Azure.CodeSigning.Dlib.dll])`.
- `T`, `ℓ`: an explicit `Tier` and lane passed to `run_with(args, H, &T, ℓ)` (`run` passes `pinned_lane()` and `tier()` =
  `tier_from(pins::WINDOWS_SIGNING_PUBLISHER)`). Every run-level statement is parametric in `T`.

## 1. Specs (TSV)

`subsumes` names the primary tests (secondary ones are in §2), `rust_features` what the proof must
handle, and `blocking_gaps` the audit §7 gaps, gap 0 omitted.

```tsv
id	kind	functions	statement	subsumes	rust_features	blocking_gaps
winsign-tier-from	contract	tier_from, aterm_update_core::pins::anchor_active	∀ a:&str: tier_from(a) = Inactive if a = "", else Active(a.to_string()). The result depends only on a.	the_tier_is_the_anchor_and_nothing_else	&str emptiness, String in an enum variant, cross-crate const fn call	1, 3, 12
winsign-verify-flags	contract	parse_verify_output	∀ ran, with r = parse_verify_output(&ran) and t = text(ran): r.verified ⇔ ran.code = Some(0) ∧ "Successfully verified" ⊑ t; r.signed ⇔ ¬("No signature found" ⊑ t); r.untrusted_root ⇔ "not trusted by the trust provider" ⊑ t ∨ "certificate chain could not be built" ⊑ t; r.timestamped ⇔ "The signature is timestamped" ⊑ t. parse_verify_output never panics.	a_self_signed_chain_is_signed_but_not_verified, an_unsigned_file_reads_as_unsigned	String format!/contains, Option<i32> compare, struct update syntax	1, 3
winsign-verify-leaf	contract	parse_verify_output	∀ ran: (r.issued_to, r.issued_by) = (Some(Leaf(ran).to), Leaf(ran).by) if Leaf(ran) ≠ ⊥, else (None, None), with Chain and Leaf as in §0 (first chain block only; the leaf is the last cert and is strictly deeper than every cert before it). Corollary (line-ending invariance): replacing every "\n" by "\r\n" in stdout and stderr leaves parse_verify_output unchanged.	a_verified_chain_is_read_back_as_its_leaf_not_its_root	loop over str::lines, local struct Vec<Cert> push/last_mut, &mut through Option::filter, strip_prefix/trim/find with a char closure, split_last + all over a slice, a bool state flag	1, 2, 3, 5, 7, 13
winsign-judge	table-property	judge_reason, judge	∀ r: judge_reason(Inactive, r) = Ok(()). ∀ p, r: judge_reason(Active(p), r) = Ok(()) ⇔ r.signed ∧ r.verified ∧ r.issued_to = Some(p) ∧ r.timestamped. On Err, RefusalReason names the FIRST failing check in order: (1) ¬signed ⇒ Unsigned; (2) ¬verified ⇒ UntrustedRoot when r.untrusted_root, else UnverifiedPolicy; (3) issued_to = Some(g) with g ≠ p ⇒ PublisherMismatch { got: g, publisher: p }, and issued_to = None ⇒ MissingChain; (4) ¬timestamped ⇒ NotTimestamped. judge(t, r) = judge_reason(t, r).map_err(to_string).	the_active_tier_refuses_everything_but_the_anchored_trusted_timestamped_leaf	enum match, Option<String> as_deref compare, structured RefusalReason enum, Display	1, 3
winsign-pin-is-leaf	other (composition)	judge, parse_verify_output, tier_from	∀ p ≠ "", ran: judge(tier_from(p), parse_verify_output(&ran)) = Ok ⇒ Leaf(ran) ≠ ⊥ ∧ Leaf(ran).to = p ∧ ran.code = Some(0). In particular, a pin equal to the name of a non-leaf cert c ∈ init(Chain(ran)) with c.to ≠ Leaf(ran).to is refused, and a block whose shape names no leaf (e.g. two certs at one depth) is refused for every p.	the_pin_names_one_leaf_and_never_the_root_it_chains_to	caller-side composition of winsign-tier-from, winsign-verify-flags, winsign-verify-leaf and winsign-judge	1, 4, 5
winsign-sign-verify-args	contract	sign_args, verify_args	∀ lane, ts, dlib, meta, files: sign_args(lane, ts, dlib, meta, files) = [sign, /v, /fd, SHA256, /tr, ts, /td, SHA256] ++ M ++ map(display, files), where M = [/dlib, dlib.display or "", /dmdf, meta.display or ""] for TrustedSigning and M = [/sha1, t] for Thumbprint(t). Hence /fd SHA256 and /tr ts /td SHA256 are always present, and no /f or /p token appears outside the caller-supplied ts, t, dlib, meta and file strings. ∀ f: verify_args(f) = [verify, /pa, /v, f.display()].	sign_args_per_lane_carry_a_sha256_digest_and_a_timestamp	Vec<String> literal + push + extend over an iterator map, Path::display to_string, enum match, Option map/unwrap_or_default	1, 3, 7
winsign-default-timestamp	table-property	default_timestamp	default_timestamp(TrustedSigning{..}) = "http://timestamp.acs.microsoft.com" and default_timestamp(Thumbprint(_)) = "http://timestamp.digicert.com".	(secondary only: sign_args_per_lane_carry_a_sha256_digest_and_a_timestamp)	enum match returning &'static str	1
winsign-metadata-json-roundtrip	round-trip	trusted_signing_metadata	∀ e, a, p: &str: trusted_signing_metadata(e, a, p) = "{\n  \"Endpoint\": \"" ++ esc(e) ++ "\",\n  \"CodeSigningAccountName\": \"" ++ esc(a) ++ "\",\n  \"CertificateProfileName\": \"" ++ esc(p) ++ "\",\n  \"ExcludeCredentials\": [\"InteractiveBrowserCredential\"]\n}\n". esc(s) is a valid JSON string body: no unescaped '"', no unescaped '\\', no char < 0x20; and json_unescape(esc(s)) = s. Hence a JSON parse of the output yields exactly {Endpoint: e, CodeSigningAccountName: a, CertificateProfileName: p, ExcludeCredentials: ["InteractiveBrowserCredential"]}: the plug-in never opens a sign-in.	metadata_json_escapes_what_a_name_could_carry, metadata_json_excludes_the_interactive_browser_credential	nested fn, loop over chars with match guards, String push/push_str, format! with {:04x}, a char-to-u32 cast; needs a Lean JSON-string unescape model	1, 3, 5, 7, 14
winsign-pinned-lane	table-property	lane_from, pinned_lane	∀ [e, a, p], t: the first matching row decides. (a) some but not all of e, a, p are "": Err. (b) e, a, p all ≠ "" ∧ t ≠ "": Err. (c) e, a, p all ≠ "": Ok(Some(TrustedSigning{e, a, p})). (d) t = "": Ok(None). (e) |t| ≠ 40 or some char of t is not ASCII hex: Err. (f) otherwise Ok(Some(Thumbprint(uppercase(t)))). pinned_lane() = lane_from(TRUSTED_SIGNING, THUMBPRINT).	the_lane_is_the_committed_identity_whole_or_none	array destructuring of [&str; 3], iter().any, chars all(is_ascii_hexdigit), to_ascii_uppercase	1, 3, 5, 7
winsign-win-join	contract	win_join	∀ base, parts: win_join(base, parts).as_os_str().as_encoded_bytes() = WJ(base, parts), byte for byte, on every build host. Corollaries: empty parts gives bytes(base); a separator is never doubled after a trailing '\\' or '/'; no separator follows a trailing ':' (C: + x = C:x); separators the caller wrote are left untouched. This byte equality is stronger than the component-wise PathBuf equality the test asserts.	windows_paths_are_joined_with_backslashes_on_every_host	generic impl Into<PathBuf> + IntoIterator<Item=&str>, OsString push, as_encoded_bytes last, loop	1, 3, 7, 11
winsign-versions-newest-first	contract	versions_newest_first	∀ names: let K(n) = n.split('.').map(parse::<u64>) when every piece parses, else ⊥. Vers(names) is the sub-list of names with K(n) ≠ ⊥, sorted by K descending in lexicographic Vec<u64> order. The sort is stable, so names with equal K (e.g. "1.0" and "01.0") keep their input order. Hence numeric, not string, order (1.0.52 > 1.0.10 > 1.0.9), and every non-numeric name, "" included, is dropped.	versions_sort_numerically	generic IntoIterator, split/parse collect into Result<Vec<u64>>, filter_map with ?, Vec sort_by with a closure (stable-sort model), lexicographic Vec Ord	1, 3, 5, 7, 11, 13
winsign-find-signtool	contract	find_signtool, find_on_path, win_join, versions_newest_first	∀ H, cfg. If cfg = Some(p): Ok(p) when H.is_file(p), else Err; nothing else is consulted. If cfg = None: Ok(c) for the FIRST c in Cands_signtool(H) with H.is_file(c), else Err(msg), where msg contains "signtool on PATH" and, for each present root r, every SDK candidate tried under r, or WJ(r, [Windows Kits, 10, bin, <10.x.y.z>]) when Vers(list_dir) = []. The search is pure over H: no H.run and no writes.	signtool_is_found_newest_sdk_first_and_every_miss_is_named, find_signtool_resolves_windows_semicolon_path_on_every_host	&dyn Host trait-object calls, nested for loops with early return, Vec<String> push, join, format!, str::split(';')	1, 3, 5, 7, 13
winsign-find-dlib	contract	find_dlib, win_join, versions_newest_first	∀ H, cfg. If cfg = Some(p): Ok(p) when H.is_file(p), else Err. If cfg = None: Err (no NuGet cache) when H.env(NUGET_PACKAGES) = None ∧ H.home = None; otherwise Ok(c) for the FIRST c in Cands_dlib(H) with H.is_file(c), with packages in DLIB_PACKAGES order (artifactsigning before trusted.signing) and versions newest first inside each package; else Err naming, for every package, each tried path or WJ(pkg_dir, [<version>]). Corollary: any present dll of the newer lineage beats any of the older lineage, whatever their versions.	the_dlib_is_found_under_the_nuget_cache_newest_version_first, the_renamed_package_lineage_wins_over_the_unlisted_one	&dyn Host calls, Option or_else chain, nested loops with early return, Vec<String>, format!	1, 3, 5, 7, 13
winsign-find-dotnet-runtime	contract	find_dotnet_runtime, dotnet_version_under, versions_newest_first	∀ H: the candidate roots are, in order, (H.env(DOTNET_ROOT), false), (WJ(PF, [dotnet]), false) when ProgramFiles = Some(PF), and (WJ(LA, [Microsoft, dotnet]), true) when LOCALAPPDATA = Some(LA); each is present only when its variable is set. Let ver(root) = the first v in Vers(H.list_dir(WJ(root, [shared, Microsoft.NETCore.App]))) whose first dotted piece parses to a u64 ≥ 8. Result = Ok{root, version: ver(root), needs_root_env: flag} for the FIRST candidate with ver ≠ ⊥, else Err containing WJ(root, [shared, Microsoft.NETCore.App, <8+>]) for every candidate. Hence needs_root_env ⇔ the chosen root is the user-local one.	the_dotnet_runtime_is_found_global_first_and_user_local_needs_dotnet_root	&dyn Host calls, Vec<(PathBuf,bool)>, loop with early return, iterator find with a parse closure, is_some_and	1, 3, 5, 7, 13
winsign-run-exit-classes	contract	run, run_inner, parse_flags, report_file	∀ args, H, T: run(args, H) ∈ {0, 1, 2}. 0 when the verb is help (no verb, `help`, or --help), and then Runs(H) stays empty. 2 ⇔ run_inner returns Err: a parse_flags error (unknown --opt, missing value), an unknown verb, no files, a sign file with ¬H.is_file, sign with ℓ = None or an invalid pin (help and verify never read the pin), sign on TrustedSigning with no Azure principal (the three AZURE_* service-principal variables, else `az` on PATH: checked before any H.write or launch), a find_signtool/find_dlib/find_dotnet_runtime Err, an H.write Err, or an H.run Err; in every one of these except the H.run Err itself, no signtool sign is in Runs(H). 1 ⇔ signtool sign ran and replied code ≠ Some(0), or some file's verdict was rejected (winsign-run-verify-verdict). Exits 1 and 2 have disjoint causes: a recorded, refused sign never yields 2.	sign_without_a_lane_refuses_by_name_and_help_exits_zero, a_signtool_that_ran_and_refused_is_exit_one_not_two, an_invalid_pin_fails_only_the_verbs_that_sign, a_trusted_signing_sign_without_an_azure_principal_refuses_before_signtool	&dyn Host, Result/? plumbing across many callees, String matching on verb, loops over files, println!/eprintln! effects, std::fs::remove_file and std::process::id outside the Host seam, a committed const read via tier()	1, 3, 4, 7, 8, 13
winsign-run-sign-protocol	contract	run, run_inner, report_file, sign_args, verify_args, default_timestamp, trusted_signing_metadata	∀ H, files ≠ [] all is_file, with ℓ = Some(lane), st = find_signtool(H, the --signtool path) Ok (and, for TrustedSigning, dlib d and runtime rt Ok): Runs(H) after run([sign] ++ files) = [(st, sign_args(lane, default_timestamp(lane), d?, meta?, files), E)] ++ (if that reply's code = Some(0) then [(st, verify_args(f), []) for f in files] else []). For TrustedSigning, meta = H.temp_dir()/aterm-winsign-<pid>.json, and H.write(meta, trusted_signing_metadata(endpoint, account, profile)) happens before the run. E = [(DOTNET_ROOT, rt.root.display())] when lane is TrustedSigning ∧ rt.needs_root_env, else []. The verify launches always carry an empty env.	sign_with_a_thumbprint_runs_signtool_then_reads_the_signature_back, a_trusted_signing_sign_passes_dotnet_root_for_a_user_local_runtime	trace of &dyn Host::run calls (abstract host with an append-only run log), Vec<(String,String)>, loops, the fs::remove_file side effect, process::id	1, 3, 4, 7, 8, 13
winsign-run-verify-verdict	contract	run, run_inner, report_file, judge, parse_verify_output	∀ T, H, files ≠ [] with signtool found: for verify (and for sign after a successful signtool sign), file f is accepted ⇔ judge(T, r_f) = Ok ∧ r_f.verified, where r_f = parse_verify_output(reply to (st, verify_args(f), [])). The exit code is 0 ⇔ every file is accepted, else 1. In particular, under T = Inactive an unsigned or untrusted-chain exe still exits 1 and a verified one exits 0. Every file is reported; there is no early stop.	verify_reports_an_untrusted_chain_with_exit_one_even_when_inactive	&dyn Host::run, a loop accumulating a code, println!/eprint! effects, composition with judge and parse_verify_output	1, 3, 4, 7, 8, 13
```

## 2. Test → spec map (TSV)

`primary` is the spec that retires the test. `also` lists the other specs the same test samples.

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
src/lib.rs	metadata_json_excludes_the_interactive_browser_credential	winsign-metadata-json-roundtrip	
src/lib.rs	the_lane_is_the_committed_identity_whole_or_none	winsign-pinned-lane	
src/lib.rs	signtool_is_found_newest_sdk_first_and_every_miss_is_named	winsign-find-signtool	winsign-win-join, winsign-versions-newest-first
src/lib.rs	find_signtool_resolves_windows_semicolon_path_on_every_host	winsign-find-signtool	winsign-win-join
src/lib.rs	the_dlib_is_found_under_the_nuget_cache_newest_version_first	winsign-find-dlib	winsign-versions-newest-first
src/lib.rs	windows_paths_are_joined_with_backslashes_on_every_host	winsign-win-join	
src/lib.rs	the_renamed_package_lineage_wins_over_the_unlisted_one	winsign-find-dlib	
src/lib.rs	versions_sort_numerically	winsign-versions-newest-first	
src/lib.rs	sign_without_a_lane_refuses_by_name_and_help_exits_zero	winsign-run-exit-classes	
src/lib.rs	sign_with_a_thumbprint_runs_signtool_then_reads_the_signature_back	winsign-run-sign-protocol	winsign-run-verify-verdict, winsign-sign-verify-args
src/lib.rs	the_dotnet_runtime_is_found_global_first_and_user_local_needs_dotnet_root	winsign-find-dotnet-runtime	
src/lib.rs	a_signtool_that_ran_and_refused_is_exit_one_not_two	winsign-run-exit-classes	winsign-run-sign-protocol
src/lib.rs	a_trusted_signing_sign_passes_dotnet_root_for_a_user_local_runtime	winsign-run-sign-protocol	winsign-run-exit-classes, winsign-find-dotnet-runtime
src/lib.rs	verify_reports_an_untrusted_chain_with_exit_one_even_when_inactive	winsign-run-verify-verdict	winsign-run-exit-classes, winsign-verify-flags
src/lib.rs	a_trusted_signing_sign_without_an_azure_principal_refuses_before_signtool	winsign-run-exit-classes	
src/lib.rs	an_invalid_pin_fails_only_the_verbs_that_sign	winsign-run-exit-classes	
```

## 4. Tests that are not spec samples

None left. `the_fake_spells_paths_the_way_the_windows_machine_it_models_would` tested `win_path`, a
`#[cfg(test)]`-only path canonicaliser inside the `Fake` host with no production caller, and was
deleted.

Count by action: delete 0.

## 5. Implied specs that look wrong, or disagree with the code

1. **Fixed (2026-10-28): `run_with` parameterizes `run_inner` over `tier: &Tier`.** `run` passes
   `&tier()`, while run-level tests pass `&Tier::Inactive` and `&tier_from(...)` explicitly so neither
   active nor inactive coverage depends on `pins::WINDOWS_SIGNING_PUBLISHER`.
2. **Fixed (2026-10-28): `VerifyReport::signed` and `parse_verify_output` docs state the negative
   `"No signature found"` marker rule explicitly.**
3. **Fixed (2026-10-28): dead `(!parts.is_empty())` guard removed from `versions_newest_first`.**
4. **Fixed (2026-10-28): `find_on_path` splits Windows `PATH` on `';'` explicitly and joins with
   `win_join`.** Covered on every host by `find_signtool_resolves_windows_semicolon_path_on_every_host`.
5. **Fixed (2026-10-28): `judge_reason` returns a structured `RefusalReason` enum.** `judge` delegates
   `judge_reason(tier, report).map_err(|r| r.to_string())`, and `the_pin_names_one_leaf_…` plus
   `the_active_tier_refuses_…` assert exact `RefusalReason` variants rather than error prose substrings.
6. **Fixed (2026-10-28): metadata file removal and PID are routed through `Host` (`Host::remove_file`,
   `Host::pid`).** `run_with` performs zero filesystem or process queries outside `&dyn Host`.
