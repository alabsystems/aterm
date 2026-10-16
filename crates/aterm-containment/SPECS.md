<!-- Copyright 2026 Andrew Yates -->
<!-- SPDX-License-Identifier: Apache-2.0 -->

# aterm-containment: spec recovery from its 195 unit tests

> Status 2026-10-16: nothing has been proved or deleted yet. The 57 specs below are the crate's
> migration worklist: each is proved through Trust/Clean about the compiled Rust function, then the
> tests it subsumes are deleted. §4 lists the 33 tests that are not spec samples and what to do with each.
>
> **Measured on Trust seal `150efe4a` (2026-10-16): the ten `{0,1,3}` / `{0,3}` specs are all blocked by
> gap 1, so nothing is proved and no test is deleted.** The strict lane admits the crate: no proc-macro
> (PM-1) refusal. A temporary build put an island over the kernel import of each fn, plus contracts on
> `ContainmentMode::level` and `ProbeLabel::refused`. The source was restored byte for byte afterwards.
> Every island was rejected `AutoBoundHeadOverApplied { name: "trust_import_aterm_containment__…", remaining_args: n }`.
> That means E6 mints no kernel import for any of these fns, because each one takes or returns an enum, an
> `Option`/`Result`, a struct with enum fields, a `PathBuf` or a `Vec<String>`. The rejections were
> `mode::ContainmentMode::level` (mode-order), `policy::ContainmentPolicy::network` (policy-monotone-minimal),
> `consent::fence_exe_from` (fence-exe), `consent::ProbeLabel::refused` (refusal-labels), `consent::FdaState::as_str`
> (wire-tokens), `consent::responsible_answer` (responsible-classification), `consent::tccutil_reset_command`,
> `consent::ConsentPosture::join` (posture-join), `consent::classify_membership` and `consent::classify_cs_status`
> (debugger-pure-classifiers), and `consent::devtool_grant`. The island for `classify_responsible` failed earlier,
> on Clean's `Result` binder (`AutoBoundHeadOverApplied { name: "Result" }`). A scratch probe of the same shape,
> scalar `i32` arguments returning a fieldless enum, also gets no import. A clause that names an enum
> receiver is refused `missing supported type for clause variable \`self\``, and its predicate is not lowered
> (`unsupported contract predicate expression`). The only clause the lane accepts on these fns names `result`
> alone. `ensures result <= 3` on `level` was proved natively (`trust-full-verifier`, not `clean-kernel-citation`)
> and then withheld at publication, and it does not state the mode table. So `mode-order` and
> `debugger-pure-classifiers` need gap 1 (enum/`Option` carriers in E6 imports and clauses) as well as gap 3.
> A scalar control in the same probe set, `fn g(a: u8) -> bool` imported as `trust_import_<crate>__T__g`, does get an import.

Source: `crates/aterm-containment` (read only). I read every `#[test]`
(src/lib.rs 2, src/mode.rs 6, src/capability.rs 4, src/policy.rs 2, src/actuator.rs 26, src/sbpl.rs 22,
src/consent.rs 115, tests/{audit_denial_logged 5, claimant_census 4, image_anchor_live 3,
init_from_flag_default 1, init_from_flag_rejects_unparsable 1, init_from_flag_with_value 1,
init_mode_is_write_once 1, uninitialized_mode 2, containment_suite 0} = 195) and the code each one
reaches (lib.rs, mode.rs, capability.rs, policy.rs, audit.rs, actuator.rs, sbpl.rs, consent.rs including
`mod imp` and `mod json`). A plain `grep '#\[test\]'` reports 196, because it also matches the text
`#[test]` in a doc comment at `tests/containment_suite.rs:31`; that file holds no `#[test]` fn.
Nothing was compiled or run. The claims below come from reading the code.

Result: **57 specs**, covering **162 tests**. **33 tests are not spec samples** (§4).

## 0. Notation used in the statements

- `LV(m) = m as u8`, so `Containment=0`, `Safety=1`, `User=2`, `Master=3`. `NAME(m) = ContainmentMode::name(m)`.
- `CAPS(m) = ContainmentPolicy::capabilities(m)`, given by the table
  `Master=(Full,Full,Full)`; `User=(Full,HomeReadWrite,Full)`; `Safety=(Full,ProjectReadWrite,Restricted)`;
  `Containment=(None,TmpOnly,NoFork)` as (network, fs, process). Each capability enum orders by its derived
  `Ord` (declaration order).
- `G`: the global `MODE: OnceLock<ContainmentMode>`, modelled as an `Option<Mode>` cell that starts as
  `None` in every fresh process.
- `AUD(call)`: the sequence of `aterm_log` records the call emits, each a `(target, level, message)` triple.
  `REC_D(s,o,m,r) = ("containment_audit", Warn, "DENIED: "++s++"::"++o++" in "++NAME(m)++" mode — "++r)`.
  `REC_P(s,o,m,r)` is the same with `Info` and the prefix `"CONTAINMENT: "`.
- `FS`: an abstract filesystem snapshot, held fixed for one call: `canonicalize`, `symlink_metadata`,
  `metadata` (dev, ino, is_file, is_dir, nlink), `read_dir`. Every spec over `sbpl::profile_scoped` or over
  the consent census is stated relative to `FS`. That is the pure core; the effect shell waits on gap 8.
- `CH(h) = canonicalize(h)` when `h` is `Some` and non-empty, else `None`.
- `ESC(s)` doubles `'\\'` and `'"'` with a leading backslash and leaves every other char unchanged.
  `SUB(p) = " (subpath \"" ++ ESC(lossy(p)) ++ "\")"` and `LIT(p) = " (literal \"" ++ ESC(lossy(p)) ++ "\")"`.
- `PROF(h,t,fs) = sbpl::profile_scoped(h, t, fs)`, a pair `(profile String, home_scoped bool)`.
- An **injected-seam probe** is one of `probe_fda_with`, `probe_debugger_with`, `probe_devtool_with`,
  `probe_devtool_list_with`. `Refused(L)` means the result equals `X::refused(L)`: every state field
  `Unknown`, every count `None`, and no injected closure called. "Called exactly once / not called" is a
  call-trace obligation on a `FnOnce` parameter (gaps 11 and 13).
- `ToolRun` is the run-outcome sum `Exited{code,stdout,stderr} | Absent | SpawnFailed | TimedOut | Unreadable`.
- **Blocking gaps** are the row numbers of the gap table in
  `docs/AUDIT-trust-for-test-migration-2026-10-15.md` §7. Gap 0 (the installed seal publishes no
  Lean-discharged row) blocks every spec and is listed on every row. Gaps 6, 14, 16 and 17 apply to every
  proof on their lane and are not listed per row. Gap 8 (effects) covers every spec over fs, process,
  FFI or the logger; until it closes, such a spec is proved over `FS`/a process model or a factored pure core.

## 1. Specs (TSV)

Columns: `id`, `kind`, `functions`, `statement`, `subsumes` (primary tests; file paths relative to the crate),
`difficulty` (Rust features the proof must handle), `blocking_gaps` (audit §7 numbers).

```tsv
id	kind	functions	statement	subsumes	difficulty	blocking_gaps
mode-order	contract	mode::ContainmentMode::level, <ContainmentMode as Ord>::cmp, <ContainmentMode as PartialOrd>::partial_cmp	∀ a,b: level(a) = LV(a) with the table Containment=0, Safety=1, User=2, Master=3; cmp(a,b) = LV(a).cmp(LV(b)); partial_cmp(a,b) = Some(cmp(a,b)). So Ord is total, Containment < Safety < User < Master, and cmp(a,b)=Equal ⇔ a=b (consistent with derived Eq).	src/mode.rs::test_ordering	EASY: repr(u8) cast, hand-written Ord delegating to u8::cmp, Option	0,3
policy-monotone-minimal	table-property	policy::ContainmentPolicy::{capabilities,network,fs,process}	∀ m: CAPS(m) = (network(m), fs(m), process(m)) and equals the CAPS table. Monotone: ∀ a ≤ b (mode-order), each component CAPS(a).x ≤ CAPS(b).x. Minimal: CAPS(Containment) is the least value of every axis (None, TmpOnly, NoFork). network(m) = None ⇔ m = Containment. All four are total const fns.	src/policy.rs::test_monotonic_capabilities, src/policy.rs::test_numeric_policy_table	EASY: const fn, enum match, struct construction, derived Ord; finite (4 modes), decidable by evaluation	0,1,3
parse-mode	contract	<ContainmentMode as FromStr>::from_str, mode::ContainmentMode::from_str_loose, <ContainmentMode as Display>::fmt, mode::ContainmentMode::name	∀ s: from_str(s) = Ok(m) ⇔ s.to_ascii_lowercase() = lowercase(NAME(m)), i.e. s is master/user/safety/containment in some ASCII case with no other byte (no whitespace, NUL, digit, prefix or suffix). Otherwise Err(ParseModeError(s.to_string())), carrying s verbatim. Display(m) writes exactly NAME(m), and ∀ m: from_str(Display(m)) = Ok(m).	src/mode.rs::test_parse, src/mode.rs::test_parse_rejects_bypass_attempts, src/mode.rs::test_display, src/mode.rs::test_display_parse_roundtrip	MEDIUM: &str, String allocation (to_ascii_lowercase, to_string), match on string literals, Result, Formatter::write_str	0,1,2,3
error-messages-name-their-inputs	contract	<InitError as Display>::fmt, <InitModeFromFlagError as Display>::fmt, <ParseModeError as Display>::fmt	Display(AlreadyInitialized{existing:e, attempted:a}) = "containment mode already set to "++NAME(e)++", cannot change to "++NAME(a). Display(ParseModeError(s)) = "invalid containment mode: "++Debug(s)++" (expected: master, user, safety, containment)". Display(InitModeFromFlagError::Parse(p)) = "invalid containment mode: "++Display(p); Display(Init(i)) = Display(i). source() is Some(inner) for both wrapper variants and None for the leaves.	src/lib.rs::test_init_error_message_includes_modes, src/lib.rs::test_init_mode_from_flag_error_variants, src/mode.rs::test_parse_error_includes_input	MEDIUM: fmt::Formatter (&mut, trait-object writer), str Debug escaping, ? on fmt::Result, dyn Error	0,1,2,8
mode-global-write-once	refinement	init_mode, try_current_mode, mode_or_containment, init_mode_from_flag	Over the cell G: init_mode(m) sets G:=Some(m) and returns Ok(()) if G=None, else returns Err(AlreadyInitialized{existing:G, attempted:m}) with G unchanged. try_current_mode() = G. mode_or_containment() = G.unwrap_or(Containment), so G=None gives the minimal CAPS. init_mode_from_flag(None, d) behaves as init_mode(d) and returns d; with Some(v) it returns Err(Parse) and leaves G unchanged when parse-mode rejects v, else behaves as init_mode(m) and returns Ok(m) on success. No call changes a G that is Some (NonEscalation / ModeImmutableAfterInit).	tests/init_from_flag_default.rs::init_from_flag_uses_default_when_no_flag, tests/init_from_flag_rejects_unparsable.rs::init_from_flag_rejects_unparsable_values, tests/init_from_flag_with_value.rs::init_from_flag_uses_the_flag_over_default, tests/init_mode_is_write_once.rs::init_mode_is_write_once, tests/uninitialized_mode.rs::mode_or_containment_defaults_to_containment_when_uninit, tests/uninitialized_mode.rs::uninitialized_mode_gets_most_restrictive_capabilities	MEDIUM-HARD: static OnceLock (unsafe internals, process-global state), closures in map_err, expect, Result	0,1,2,8,13
audit-line-format	contract	audit::{log_denial,log_posture,forward,call1}	∀ s,o,r,m: AUD(log_denial(s,o,m,r)) = [REC_D(s,o,m,r)] and AUD(log_posture(s,o,m,r)) = [REC_P(s,o,m,r)], provided the logger's max level admits the record. Corollary: a posture message never begins with "DENIED:", and the target is always "containment_audit".	tests/audit_denial_logged.rs::log_denial_emits_audit_event, tests/audit_denial_logged.rs::log_denial_includes_all_modes, tests/audit_denial_logged.rs::log_posture_is_neutral_not_denial	MEDIUM: String push_str, FnOnce closure through a generic dispatcher, format_args! (unsafe fmt::Arguments), process-global logger effect	0,1,2,8,13
decide-on-decision	contract	actuator::{decide_on,decide,network_sandbox_actuated,os_sandbox_actuated}, SpawnDecision::is_permitted	Let D = (network(m) = None), A = sandbox_available. If D ∧ ¬A: decide_on(m,A,home) = Deny{mode:m, reason:NO_OS_SANDBOX_REASON}. Otherwise Permit{mode:m, os_sandbox: D∧A, sbpl, home_scoped} with (sbpl, home_scoped) = (Some(PROF(home,temp_dir,fs(m)).0), PROF(…).1) when D∧A and (None, false) otherwise. Hence sbpl.is_some() = os_sandbox, home_scoped ⇒ os_sandbox, an os_sandbox sbpl starts with BASE_PROFILE, home_scoped = CH(home).is_some(), and Master/User/Safety are always permitted. decide(m) = decide_on(m, os_sandbox_actuated(), sbpl::home_dir()). network_sandbox_actuated(m) = os_sandbox_actuated() ∧ m = Containment. is_permitted ⇔ Permit.	src/actuator.rs::network_sandbox_in_force_only_for_containment_on_macos, src/actuator.rs::every_mode_permits_the_initial_shell_where_its_sandbox_exists, src/actuator.rs::containment_fails_closed_without_an_os_sandbox, src/actuator.rs::permit_carries_sbpl_iff_network_sandbox_actuated, src/actuator.rs::non_containment_spawn_is_never_os_sandboxed, src/actuator.rs::home_scoped_follows_the_home_not_the_sandbox	MEDIUM: enum with String payload, Option<(String,bool)> map/is_some_and closures, debug_assert, cfg!, env read (home_dir), callee contract of profile_scoped	0,1,3,4,8,13
decide-on-audit	contract	actuator::{decide_on,applied_posture}	Every decide_on(m,A,home) emits exactly one record. Deny: [REC_D("spawn","spawn initial shell",m,NO_OS_SANDBOX_REASON)]. Permit with os_sandbox: [REC_P("spawn","os-network-sandbox",m,applied_posture(home_scoped))]. Permit without: [REC_P("spawn","os-network-sandbox",m,R(m))], where R(m) contains "OS sandbox not applied"; it contains "resource limits inherited from aterm's own process" ⇔ m ∈ {Master,User}, and "resource limits + process-cap gate only" ⇔ m = Safety. applied_posture(h) claims the shell-history allowance and the credential/private-data denies ⇔ h, and claims "no debugger attach" and "no mach service lookup but user and group queries" for both h. So no posture record claims a rule the generated profile lacks.	tests/audit_denial_logged.rs::decide_permit_is_not_logged_as_denied, tests/audit_denial_logged.rs::posture_claims_resource_limits_only_for_the_mode_that_has_them, src/actuator.rs::applied_posture_claims_home_denies_only_when_emitted	MEDIUM: &'static str constants, contains/ends_with on constants (decidable), composition with audit-line-format	0,1,4,8
sbpl-escape	round-trip	sbpl::{push_sbpl_escaped,push_subpath_clause,push_literal_clause}	∀ out,p: out' = out ++ ESC(lossy(p)). ESC is injective; dropping each backslash and keeping the next char inverts it; ESC(x) has no '"' not preceded by an odd run of backslashes. So SUB(p) and LIT(p) are each one well-formed SBPL string literal, and no home path can end the literal early.	src/sbpl.rs::sbpl_escaping_quotes_and_backslashes	MEDIUM: &mut String push/push_str, chars() loop, Path::to_string_lossy (Cow)	0,1,2,5,7
profile-selection	contract	sbpl::{profile_for,profile_for_scoped,home_dir}	∀ caps,home: profile_for_scoped(caps, home) = Some(PROF(home, Some(temp_dir()), caps.fs)) if caps.network = None and None if caps.network = Full; no other field of caps is read. profile_for(&caps) = profile_for_scoped(caps, home_dir()).map(fst). home_dir() = Some(h) ⇔ $HOME is set and non-empty. With policy-monotone-minimal: profile_for(CAPS(m)).is_some() ⇔ m = Containment, and that profile starts with BASE_PROFILE.	src/sbpl.rs::non_containment_modes_get_no_sandbox, src/sbpl.rs::profile_selection_keys_only_on_network_capability_none, src/sbpl.rs::containment_caps_yield_a_network_plus_secret_profile	EASY-MEDIUM: Copy struct field read, enum match, Option map, env read	0,1,3,8
profile-structure	contract	sbpl::{profile_scoped,profile_for_home}	∀ home,t,fs,FS with T = (fs = TmpOnly), H = CH(home): PROF(home,t,fs) = (BASE_PROFILE ++ (T ? W(t,H) : "") ++ (H = Some(h) ? "(deny file-read* file-write*" ++ C(h,fs) ++ ")" ++ ANC(h,fs) : ""), H.is_some()). W is write-confinement; C(h,fs) is, in list order, RES(h,e,SUB) for e ∈ SECRET_SUBDIRS, RES(h,e,LIT) for e ∈ SECRET_LITERAL_FILES, then RES(h,e,SUB) for e ∈ PRIVATE_SUBDIRS when T; RES is resolved-deny-clauses and ANC is ancestor-write-denies. profile_for_home = fst ∘ profile_scoped. Corollaries: H=None gives BASE_PROFILE (¬T) or BASE_PROFILE++W(t,None) (T); ¬T gives no "(allow file-write*", no "(deny file-write*)" and no PRIVATE entry; h or h/Library is never a subpath or literal deny unless an entry resolves there.	src/sbpl.rs::home_unset_yields_no_home_scoped_rule, src/sbpl.rs::nonexistent_home_falls_back_to_the_base_profile, src/sbpl.rs::broader_fs_tiers_get_no_write_confinement, src/sbpl.rs::full_profile_starts_with_the_base_profile_and_denies_each_secret, src/sbpl.rs::private_data_set_present_only_when_included, src/sbpl.rs::full_profile_does_not_deny_the_whole_home, src/sbpl.rs::private_data_is_gated_on_the_fs_capability_not_network	MEDIUM-HARD: String building over &mut, loops over const slices, Option filter/and_then, canonicalize (FS), Vec<&str> chain/extend	0,1,2,7,8
write-confinement	contract	sbpl::{push_write_confinement,push_history_allows,plain_or_absent,admitted_user_temp}	W(t,H) = PREFERENCE_WRITE_DENY ++ "(deny file-write*)(allow file-write*" ++ SUB(r) for r ∈ WRITABLE_ROOTS ++ (t.and_then(admitted_user_temp) = Some(c) ? SUB(c) : "") ++ (H=Some(h) ? HA(h) : "") ++ ")". HA(h): for each row R of HISTORY_FILES with plain_or_absent(h/R[0].name), and each (n,v) ∈ R in order, " (require-all"++LIT(h/n)++" (vnode-type "++v++"))"; a row whose first file is not plain is dropped whole. plain_or_absent(p) ⇔ symlink_metadata(p) is NotFound ∨ (is_file ∧ nlink=1); other errors give false. admitted_user_temp(t) = Some(c) ⇔ c = canonicalize(t) ∧ file_name(c) = "T" ∧ parent³(c) = USER_TEMP_PARENT. Corollaries: the allow clause holds no "(prefix ", never names h, h/Library, a non-plain row's history name, USER_TEMP_PARENT or a sibling of T.	src/sbpl.rs::tmp_only_confines_writes_to_the_writable_roots, src/sbpl.rs::a_history_file_that_is_not_a_plain_file_gets_no_allowance, src/sbpl.rs::only_the_users_own_tmpdir_joins_the_write_roots	MEDIUM-HARD: &mut String, nested const slice loops, Path parent/file_name chains, symlink_metadata + nlink (FS), cfg(unix)	0,1,2,7,8
resolved-deny-clauses	contract	sbpl::{push_resolved_clause,resolve_existing_prefix}	For canonical h, entry e, push ∈ {SUB, LIT}, L = h.join(e): if canonicalize(L) = Ok(c), RES(h,e,push) = push(c) ++ (c ≠ L ? push(L) : ""); else RES = push(L) ++ (R = resolve_existing_prefix(L) ∧ R ≠ L ? push(R) : ""). resolve_existing_prefix(L) = canonicalize(a).join(L.strip_prefix(a)) for the deepest strict ancestor a of L that canonicalizes, or None. Corollary: a store or ancestor reached through a symlink is denied at the kernel-resolved target and at its lexical path; a non-canonical home spelling (e.g. /tmp) never appears.	src/sbpl.rs::secret_paths_are_canonicalized_not_raw, src/sbpl.rs::symlinked_secret_store_is_denied_at_its_canonical_target, src/sbpl.rs::symlinked_ancestor_with_absent_leaf_is_denied_at_resolved_target	MEDIUM-HARD: fn-pointer parameter, Path::ancestors loop, strip_prefix, PathBuf join, canonicalize (FS)	0,1,7,8,13
ancestor-write-denies	contract	sbpl::{push_existing_ancestor_write_denies,push_ancestor_literals,profile_scoped}	ANC(h,fs) = (S = "" ? "" : "(deny file-write*" ++ S ++ ")"). S lists LIT(n) once each in first-emission order: entries in nested order (SECRET_SUBDIRS, SECRET_LITERAL_FILES, then PRIVATE_SUBDIRS when fs = TmpOnly); per entry the lexical path L then its resolved R when R ≠ L; per path the nodes shallow-first. The nodes are the contiguous run of strict ancestors with h ⊊ n ⊊ leaf, n.starts_with(h), symlink_metadata(n) existing. h, nodes outside h and the leaf are never emitted; no node is emitted twice.	src/sbpl.rs::nested_secret_ancestor_nodes_are_write_denied_to_block_rename_relocation, src/sbpl.rs::nonexistent_nested_ancestor_is_not_write_denied, src/sbpl.rs::symlinked_ancestor_store_is_write_denied_at_its_resolved_nodes, src/sbpl.rs::ancestor_write_deny_dedups_shared_parents	HARD: &mut Vec<PathBuf> dedup set, ancestors().skip().take_while() closure, Vec reverse, linear search, FS	0,1,2,7,8,13
sbpl-profile-denotation	refinement	sbpl::{profile_scoped,BASE_PROFILE,MACH_LOOKUP_ALLOWLIST}	Under a ty_model!-authored SBPL semantics (last matching rule wins), ∀ canonical h, FS: PROF(Some(h),t,TmpOnly).0 denies every network*; denies mach-priv-task-port except target self; denies mach-lookup of every global name not in MACH_LOOKUP_ALLOWLIST and allows that list; denies user-preference-write; denies file-write* outside WRITABLE_ROOTS, admitted_user_temp(t) and the plain rows' history literals; denies file-read* and file-write* under every resolved secret/private entry and file-write* on every existing nested ancestor node; allows every other read. The live enforcement tests in src/actuator.rs stay as the model's conformance tests against the macOS kernel (§4).	(none; it is what the kept conformance tests in §4 bind)	VERY HARD: String output only, but needs an SBPL model plus a model-to-string refinement	0,1,7,8,10
app-bundle-root	contract	consent::{bundle_root_where,app_bundle_root,path_is_in_app_bundle,bundle_layout_root,cache_bundle_for}	Let c = exe.components(), n = |c|. bundle_root_where(exe, P) = Some(collect(c[..=i])) for the greatest i with i+3 < n ∧ P(c[i]) ∧ c[i+1] = "Contents" ∧ c[i+2] = "MacOS", else None (including n < 4). app_bundle_root: P(d) = Path(d).extension() eq_ignore_ascii_case "app" (a bare ".app" has no extension). bundle_layout_root: P(d) = d is Normal. path_is_in_app_bundle = app_bundle_root(exe).is_some(). Purely lexical (no FS; relative paths allowed). cache_bundle_for(exe) = app_bundle_root(exe).unwrap_or(exe).	src/consent.rs::out_of_bundle_paths_are_not_in_a_bundle, src/consent.rs::in_bundle_paths_resolve_to_their_bundle_root, src/consent.rs::the_bundle_layout_root_ignores_the_directory_name, src/consent.rs::cache_bundle_prefers_the_bundle_root	MEDIUM: Path::components into Vec<Component>, reverse range find_map, impl Fn closures, OsStr compare, PathBuf collect	0,1,7,11,13
fence-exe	contract	consent::{fence_exe_from,fence_exe}	∀ exec,image: fence_exe_from(exec, image) = exec if exec = Some(e) with path_is_in_app_bundle(e); else Some(p) if image = At(p) with path_is_in_app_bundle(p); else exec (possibly None). fence_exe() = fence_exe_from(current_exe().ok(), self_image()). Corollary: the result is in a bundle ⇔ the exec path or the kernel's image path is.	src/consent.rs::the_fence_judges_the_bundled_image_behind_a_launch_symlink	EASY: Option<PathBuf> moves, enum match with guards	0,1,3
image-anchor	table-property	consent::{classify_image_anchor,image_anchor}, ImageAnchor::grant_unverifiable	classify_image_anchor(exec, At(p)) = NotBundled if bundle_layout_root(p) = None, else Live if app_bundle_root(p) ≠ None, else Displaced. With NoPath: Deleted if exec = Some(e) with bundle_layout_root(e) ≠ None, NotBundled if exec = Some(e) otherwise, Unknown if exec = None. With Unreadable: Unknown. grant_unverifiable(a) ⇔ a ∈ {Displaced, Deleted}. Off macOS, image_anchor() = NotBundled.	src/consent.rs::the_image_anchor_names_every_way_the_code_can_go_missing	EASY: enum match with guards over &SelfImage, composition with app-bundle-root	0,1,3,4
fda-probe-gate	contract	consent::{probe_fda_with,classify_probe,path_has_interior_nul}	probe_fda_with returns the first that applies: ¬gate.permits() (¬(enabled ∧ check)) → Refused(RefusedDisabled); exe=None → Refused(RefusedNoExe); ¬path_is_in_app_bundle(exe) → Refused(RefusedOutOfBundle); home=None → Refused(RefusedNoHome); db = home/TCC_DB_RELATIVE containing a 0 byte → Refused(RefusedBadPath); otherwise classify_probe(syscall(db)) with syscall called exactly once on db, and never on a refusal path. classify_probe: Ok → (Granted, OpenOk); Errno(1) → (Denied, OpenEperm); Errno(e≠1) → (Unknown, OpenErrno(e)). So Denied ⇔ EPERM was observed.	src/consent.rs::probe_refuses_out_of_bundle_without_touching_the_os, src/consent.rs::probe_refuses_when_disabled_or_unresolvable_without_touching_the_os, src/consent.rs::probe_reaches_the_syscall_exactly_once_from_inside_a_bundle, src/consent.rs::probe_classification_is_a_table	MEDIUM: generic FnOnce parameter (call-trace spec), PathBuf join, byte search	0,1,3,5,11,13
refusal-labels	table-property	{ProbeLabel,DebuggerProbeLabel,DevToolProbeLabel,DevToolListLabel}::refused, {FdaProbe,DebuggerProbe}::{refused,pending}, {DevToolProbe,DevToolList}::refused, consent::unsupported_{probe,debugger_probe,devtool_probe,devtool_list}	For each label enum, refused(l) ⇔ l is a Refused* variant or UnsupportedPlatform; Pending, every completed outcome, and NoFullDiskAccess/OpenFailed are not refused. Every X::refused(l) and X::pending() has every state field Unknown and every count/time field None; pending's label is Pending. Each unsupported_*() = X::refused(UnsupportedPlatform).	src/consent.rs::refusal_labels_all_report_no_syscall, src/consent.rs::the_non_macos_answer_is_unknown	EASY: const fn, matches!, Copy structs; finite, decidable by evaluation	0,1,3
wire-tokens	table-property	{FdaState,FdaScope,ProbeLabel,ImageAnchor,DrClass,Enumeration,TrashOffer,DebuggerMode,DebuggerMember,SelfHardening,DebuggerProbeLabel,DevToolState,DevToolProbeLabel,DevToolListing,DevToolListLabel,DevToolGrant,Attribution,FsConsent}::as_str, Responsible::{token,pid}	For each enum, as_str is injective and equals the documented token table (the privacy-row and doctor wire schema); ProbeLabel::OpenErrno(_) ↦ "open_errno" for every errno. Each Default is the not-observed variant (Unknown; Unavailable for Enumeration). Responsible::token(r) = None ⇔ r = Other(_); Responsible::pid(r) = Some(p) ⇔ r = Other(p).	src/consent.rs::tokens_render_for_every_state, src/consent.rs::responsible_tokens_render, src/consent.rs::the_debugger_tokens_are_the_documented_spellings, src/consent.rs::the_devtool_tokens_are_the_documented_spellings, src/consent.rs::the_list_and_grant_tokens_are_the_documented_spellings	EASY: const fn match returning &'static str, derived Default; finite, decidable	0,1,3
responsible-classification	contract	consent::{responsible_answer,classify_responsible}	responsible_answer(ret, e) = Ok(ret) if ret ≥ 0, else Err(NotOurs) if e = 1, Err(Gone) if e = 3, Err(Errno(e)) otherwise. requires self_pid > 0 (see §5.4): classify_responsible(self_pid, a) = SelfProcess if a = Ok(self_pid); Other(p) if a = Ok(p), p > 0, p ≠ self_pid; Exited if a = Err(Gone); Unknown otherwise (including Ok(p ≤ 0) and every other Err). Corollary: no Err answer and no non-positive pid is ever SelfProcess or Other.	src/consent.rs::responsible_answers_are_a_table, src/consent.rs::minus_one_never_becomes_self	EASY: const fn, i32 compare, Result match with guards	0,1,3
protected-roots	contract	consent::{protected_roots_in,is_under_protected_root}	With overrides = []: protected_roots_in(home, []) = [h.join(s) for s in PRIVATE_SUBDIRS] in order if home = Some(h), else []. Otherwise, in override order, for each o with t = o.trim() ≠ "": home.join(t[2..]) if t starts with "~/" and home is Some; home if t = "~" and home is Some; t if t is absolute; all others dropped. is_under_protected_root(p, R) ⇔ ∃ r ∈ R: p.starts_with(r) (whole components).	src/consent.rs::empty_overrides_resolve_to_private_subdirs, src/consent.rs::overrides_replace_the_default_and_are_expanded, src/consent.rs::no_home_means_no_default_roots_and_absolute_overrides_only, src/consent.rs::containment_is_a_prefix_test_over_whole_components	MEDIUM: Vec<PathBuf> build loop, &[String] iteration, trim/strip_prefix, is_absolute/starts_with, iterator any	0,1,5,7
plist-string-and-display-name	contract	consent::{plist_string,display_name,readable_info_plist}	plist_string(text, key) looks only at the first '<key>'…'</key>' block (left to right) whose trimmed inner text equals key. If the text after it, leading whitespace trimmed, starts with '<string>' and a '</string>' follows, the result is Some(v) with v the trimmed content and v ≠ ""; otherwise None, and later blocks with the same key are not consulted. No matching block → None. display_name(exe, t) = first Some of plist_string(t, CFBundleDisplayName), plist_string(t, CFBundleName); else exe.file_name() lossily; else "unknown". readable_info_plist(exe, R) = app_bundle_root(exe).filter(¬is_under_protected_root(·,R)).map(r ↦ r/Contents/Info.plist).	src/consent.rs::the_plist_reader_is_exact_and_xml_only, src/consent.rs::display_name_prefers_display_then_name_then_basename, src/consent.rs::display_name_does_not_bind_a_prefix_key, src/consent.rs::info_plist_under_a_protected_root_is_refused_and_falls_back_to_the_basename	MEDIUM: &str slicing by find offsets, loop with early return, lifetimes on returned &str	0,1,5,7
classify-dr	contract	consent::{classify_dr,without_quoted_strings,has_cdhash_clause}, DrClass::grant_stable	Let x = requirement.trim(), lo = ascii_lowercase(x), q = without_quoted_strings(lo) (empties the inside of every closed '"…"' string, honouring backslash escapes, never crossing a line, keeping an unclosed quote as written). First that applies: x = "" → Unknown; lo contains "not signed at all" or "code object is not signed" → Unsigned; q has a 'cdhash' not preceded by [A-Za-z0-9_.-] and followed, after whitespace, by 'h"' → Cdhash; q contains ('identifier ' or 'identifier"') and one of ('anchor apple', 'certificate leaf', 'certificate root', 'subject.ou') → Identity; else Unknown. grant_stable(c) ⇔ c = Identity.	src/consent.rs::dr_classification_is_a_table, src/consent.rs::only_an_identity_requirement_is_grant_stable	MEDIUM-HARD: split_inclusive/find/char_indices, stateful find_map closure, match_indices, contains	0,1,5,7,13
codesign-report-readers	contract	consent::{designated_requirement,team_identifier,classify_signing,identity_from_report,same_identity}	designated_requirement(r) = the clause of the first line whose trim, after stripping one leading '#' and leading whitespace, starts with 'designated =>', else None. team_identifier(r) = the first 'TeamIdentifier=' value, trimmed, non-empty and ≠ 'not set'. classify_signing(r) = 'unsigned' iff r has exactly one non-blank line and it ends with ': code object is not signed at all'; else 'adhoc' iff some trimmed line = 'Signature=adhoc'; else 'developer-id' iff team_identifier(r) is Some; else 'unknown'. identity_from_report(rep, id): dr_text = rep.and_then(designated_requirement).unwrap_or(""); signing = rep.map_or("unknown", classify_signing); dr = Unsigned if signing='unsigned', Unknown if classify_dr(dr_text) = Unsigned, else classify_dr(dr_text); team = rep.and_then(team_identifier). Hence identity_from_report(None,_).dr = Unknown and a quoted or path occurrence of the unsigned phrase never yields Unsigned. same_identity(f, c) ⇔ f.dr ≠ Unknown ∧ f.dr = c.dr ∧ f.signing = c.signing ∧ f.dr_text = c.dr_text.	src/consent.rs::the_signing_readers_fail_toward_the_weaker_claim, src/consent.rs::unsigned_is_the_whole_report_not_a_phrase_in_it, src/consent.rs::the_identity_recheck_tells_an_unsigned_copy_from_a_failed_read	MEDIUM: lines() with find_map/any/filter, Option<&str> combinators, struct construction	0,1,4,5,7,13
claimant-offer-rule	contract	Claimant::conflicts_with, TrashOffer::of, Claimants::{conflicting,running_stable,retirable,listed,offered_for_trash,sole_claimant}	conflicts_with(a,b) ⇔ trim(a.dr_text) = "" ∨ trim(b.dr_text) = "" ∨ trim(a.dr_text) ≠ trim(b.dr_text). With R the first running claimant: conflicting(C) = [c ∈ C.found in order | ¬c.running ∧ conflicts_with(c, R)], or [] with no R. stable(C) ⇔ ∃ running c with c.dr = Identity. of(c, s): if (c.signing, c.dr) ∈ {(adhoc, Cdhash), (unsigned, Unsigned)}, Offered when c.links = [] ∧ s, Linked when links ≠ [], RunningUnstable when links = [] ∧ ¬s; else Requirement if signing = adhoc; else Signature. retirable = [c ∈ conflicting | of(c, stable) = Offered]. listed(tool) = first MAX_CLAIMANT_ROWS of conflicting paired with of(c, stable), Offered ↦ NoTool when ¬tool. offered_for_trash(tool) = [c | (c, Offered) ∈ listed(tool)] ⊆ retirable ∩ take(MAX_CLAIMANT_ROWS). sole_claimant ⇔ enumeration = Complete ∧ ∀ c ∈ found: c.running.	src/consent.rs::an_unreadable_requirement_conflicts_rather_than_matching, src/consent.rs::nothing_is_retirable_beside_an_unstable_running_copy, src/consent.rs::a_developer_id_copy_is_never_retirable, src/consent.rs::only_listed_copies_are_offered_for_the_trash, src/consent.rs::an_unsigned_copy_is_unsigned_and_offered_an_unreadable_one_is_not	MEDIUM: Vec<&Claimant> filter/take/map/collect closures, tuple match on (&str, enum), &self views	0,1,2,5,7,13
classify-claimants	contract	consent::{classify_claimants,in_trash,locate_candidate}	Candidates in order: the looks_like_bundle entries of each root listing that is Entries (looked += 1); a Missing listing of a non-required root counts as looked; every other listing outcome is a hole; then registered paths not in_trash (looked += 1), or one hole when registered = None; then running. key(s) = File(locate(s).file) when locate succeeds, else Spelled(s). Candidates group by key in first-seen order, one claimant per bundle at most. A bundle's path is the first non-link spelling, else the first link's resolved path; its links are the distinct link spellings in order. read is called once per bundle path: NotOurs dropped, Hole a hole, Ours(id) a Claimant with running ⇔ key = key(running). found is sorted by (running desc, path asc). enumeration = Unavailable if looked = 0 ∧ found = []; Partial if holes > 0 ∨ looked = 0; else Complete. in_trash(p) ⇔ some Normal component is '.Trash' or '.Trashes'. locate_candidate(s) = None if s cannot be stat'd or is a link whose canonical target fails looks_like_bundle; else Some{file: (dev,ino) of the target, path: s or its canonical target when a link, link: s is a link}.	src/consent.rs::the_census_finds_every_claimant_including_rollback_and_registered_copies, src/consent.rs::every_hole_downgrades_the_census_and_a_complete_lone_install_is_sole, tests/claimant_census.rs::a_link_is_the_bundle_it_names	HARD: three impl Fn closure params, Vec with &mut index updates, sort_by with then_with, PathBuf ordering, FS for locate_candidate	0,1,2,5,7,8,11,13
retire-claimants	contract	consent::retire_claimants	Result = [(p, o(p)) for p in plan], in plan order. With c = the first retirable() claimant with c.path = p: c absent or ¬unchanged(c) → NotOffered; else in_use(p) ≠ Some(false) → InUse; else Failed(e) if trash(p) = Err(e), Moved if Ok. trash is called exactly for the p reaching the last branch, in plan order, and never for a path outside plan or not retirable.	src/consent.rs::only_a_shown_still_offered_idle_copy_is_moved_to_the_trash	MEDIUM: three impl Fn closures with a call-trace obligation, Vec<(PathBuf, Retired)>	0,1,4,5,7,13
census-roots-and-names	contract	consent::{looks_like_bundle,claimant_search_roots}	looks_like_bundle(p) ⇔ n = lowercase(file_name(p)) exists ∧ n does not start with '.' ∧ (extension(n) = 'app' ∨ n.rsplit_once('.app.') = (head, tail) with head ≠ "" ∧ tail ≠ ""). claimant_search_roots(running, home) = [(parent(running), true)] when that parent exists, then (/Applications, false) and (home/Applications, false) when home is Some, each skipped if already present. No duplicate path; only the running bundle's parent is required.	src/consent.rs::bundle_names_include_the_rollback_suffix, src/consent.rs::the_search_roots_start_where_this_process_runs_from	EASY-MEDIUM: String lowercase, rsplit_once, Path extension, Vec dedup-by-scan	0,1,7
census-fence	contract	consent::{census_may_read,list_unless_protected,list_root,read_candidate,bundle_reaches_protected,bundle_identity,bundle_id_of,census_in,still_claims}	Over FS with device ids: census_may_read(p, R) ⇔ ¬under(p, R) ∧ ¬(canonicalize(p) = Ok(c) ∧ under(c, R)). list_unless_protected(d, R) = list_root(d) if census_may_read(d, R), else Unreadable; list_root gives Entries / Missing (NotFound) / Unreadable. read_candidate(root, id, R), first that applies: NotOurs if root is not a dir (followed); Hole unless census_may_read on root and root/Contents/Info.plist; NotOurs if the plist is NotFound or not a file; Hole on another stat error or if read_info_plist is None; NotOurs if the plist's identifier ≠ id; Hole if bundle_reaches_protected(root, R); else Ours(identity_from_report(codesign(root), Some(id))). bundle_reaches_protected(root, R) holds iff: ¬census_may_read(root/Contents); or root does not canonicalize or root/its canonical form holds a control char; or canonical B fails looks_like_bundle; or some r ∈ R or canonical(r) starts with B; or Contents does not canonicalize inside B; or B's top level holds anything but Contents and plain files; or the walk of Contents (≤4096 entries, depth ≤16, every dir on B's device) meets an unreadable entry, a symlink not resolving inside canonical(Contents), or a depth-0 'info*.plist' other than 'Info.plist'; or an existing Info.plist fails plist_names_plain_executables. bundle_identity and bundle_id_of are None whenever the fence refuses. census_in = classify_claimants with list_unless_protected, locate_candidate, read_candidate. still_claims(c, id) ⇔ read_candidate(c.path, id, protected_roots([])) = Ours(f) ∧ same_identity(f, c).	src/consent.rs::the_census_does_not_look_inside_a_protected_root, src/consent.rs::the_walk_stays_inside_the_bundles_own_contents, src/consent.rs::the_walk_does_not_cross_into_another_volume, src/consent.rs::the_spelled_protected_arm_refuses_on_its_own, src/consent.rs::the_census_lists_through_the_protected_fence, src/consent.rs::the_candidate_reader_tells_ours_from_not_a_bundle_from_a_hole, src/consent.rs::the_bundle_id_is_read_off_the_bundle_and_never_under_a_protected_root	HARD: read_dir/metadata/symlink_metadata/canonicalize, worklist stack loop with counters, MetadataExt dev, early returns, codesign spawn	0,1,2,7,8
info-plist-cf-reading	refinement	consent::{imp::parse_info_plist,read_info_plist,plist_names_plain_executables,plain_file_name,read_plist_bytes,read_plist_text}	Relative to a model of CFPropertyListCreateWithData (partial map bytes → CF value; XML in every encoding, binary and old-style text accepted, JSON rejected). read_plist_bytes(p) = None if p is not a file, is over 256 KiB, or cannot be read; read_plist_text adds UTF-8 decoding. On macOS, read_info_plist(p) = None iff read_plist_bytes(p) = None; it is {bundle_id: None, plain_executables: true} when CF rejects the bytes or the root is not a dictionary. For a dictionary D, over string keys of ≤128 UTF-16 units only: bundle_id = text(D['CFBundleIdentifier-macos']) if present, else text(D['CFBundleIdentifier']), where text(v) is Some only for a CFString of ≤1024 units without interior NUL. plain_executables ⇔ every key whose lowercase contains 'executable' maps to a text satisfying plain_file_name. plain_file_name(n) ⇔ n has exactly one component, it is Normal, and it has no control char.	src/consent.rs::the_walk_reads_the_plist_as_codesign_does, src/consent.rs::a_binary_info_plist_answers_whose_bundle_it_is, src/consent.rs::only_plain_file_names_are_plain	HARD: unsafe CoreFoundation FFI, raw pointer buffers, Vec<CfRef>, UTF-16 length check. plain_file_name alone is EASY	0,1,7,8
bounded-run	contract	consent::{run_bounded_exit,run_bounded}	Over a process model, run_bounded_exit(program, args, c, b) returns: Absent iff spawn reports NotFound; SpawnFailed iff spawn fails otherwise or a reader thread cannot start; TimedOut iff the child has not exited by start+c (it is then killed and reaped) or exited but both readers have not reached EOF by start+c; Unreadable iff the exit cannot be read or either stream exceeds b bytes or is not UTF-8; else Exited{code, stdout, stderr} with both streams whole. stdin is /dev/null. Liveness: returns within c + O(polling period) regardless of descendants holding the pipes. run_bounded(p, a, c) = Some((out, err)) exactly for Exited with code = Some(0) at cap TOOL_OUTPUT_MAX_BYTES, None otherwise.	src/consent.rs::a_tool_report_read_in_part_is_no_report, src/consent.rs::a_pipe_held_open_past_the_tool_cannot_outlast_the_ceiling, src/consent.rs::a_bounded_run_keeps_its_outcome_apart	VERY HARD: spawn/try_wait/kill, threads with JoinHandle, Read::take, Instant deadlines, polling loop	0,7,8,13
fda-open-probe	contract	consent::imp::{probe_descriptor,open_probe}	∀ db without interior NUL: probe_descriptor(db) = open(db, O_RDONLY|O_CLOEXEC) as an OwnedFd, or Err(errno); the descriptor carries FD_CLOEXEC from creation. open_probe(db) is Ok after dropping the fd having read nothing, or Errno(e); a missing file gives Errno(ENOENT).	src/consent.rs::the_fda_probe_descriptor_is_close_on_exec_from_its_open	MEDIUM: unsafe libc::open FFI, CString, OwnedFd::from_raw_fd, errno read	0,8
folder-table	table-property	Folder::{as_str,tcc_service,parse,path,label,noun,absent_means_allowed,ALL,WARMUP_DEFAULT}, consent::folder_paths_in	Folder has four variants; every per-variant function equals the documented table; as_str and label are each injective. ALL = [Documents, Desktop, Downloads, AppData]; WARMUP_DEFAULT = ALL[..3] (no AppData). absent_means_allowed(f) ⇔ f = AppData. path(f, h) = h/Documents, h/Desktop, h/Downloads, and h/Library/Containers/com.apple.Notes for AppData. ∀ s: parse(s) = Some(f) ⇔ lowercase(trim(s)) ∈ spellings(f), with spellings(AppData) = {app-data, app_data, appdata} and {as_str(f)} for the others; so parse(as_str(f)) = Some(f) in any case. folder_paths_in(None, _) = []; otherwise [(f, path(f, h)) for name in names if parse(name) = Some(f)], in input order.	src/consent.rs::folder_names_services_and_paths, src/consent.rs::the_warmup_default_is_the_folders_without_app_data, src/consent.rs::the_app_data_item_answers_to_every_spelling_of_its_name, src/consent.rs::every_item_has_its_own_label_and_noun, src/consent.rs::only_the_app_data_probe_reads_a_missing_path_as_permission, src/consent.rs::folder_paths_are_resolved_data_for_the_warmup	EASY-MEDIUM: const fn matches, &'static [Self] consts, lowercase/trim, PathBuf joins, filter_map collect	0,1,3,7
tccutil-reset-command	contract	consent::tccutil_reset_command	∀ id,f: tccutil_reset_command(id, f) = [TCCUTIL_PATH, "reset", tcc_service(f), id] exactly. Its length is 4 for every id, including "" and flag-like ids, so the subject-less every-app form `tccutil reset <service>` is never produced by this builder.	src/consent.rs::the_reset_command_is_built_from_the_running_bundle_id, src/consent.rs::the_reset_command_can_never_take_the_every_app_form	EASY: vec! of String::from; needs a Vec<String> content model	0,1
tccutil-presence-and-subject	contract	consent::{classify_tccutil,bundle_id_is_usable}, TccutilPresence::can_run	classify_tccutil(is_file, x) = Missing if ¬is_file, Executable if is_file ∧ x, NotExecutable if is_file ∧ ¬x. can_run(p) ⇔ p = Executable; Default = Unknown, which cannot run. bundle_id_is_usable(s) ⇔ t = trim(s) ≠ "" ∧ ¬t.starts_with('-') ∧ no char of t is whitespace or a control char.	src/consent.rs::tccutil_presence_is_a_table_and_only_executable_may_run, src/consent.rs::a_usable_bundle_id_is_non_blank_flagless_and_whitespace_free	EASY-MEDIUM: const fn tuple match, trim/starts_with/chars().any	0,1,5,7
reset-offer	contract	consent::reset_offer, ResetOffer::shows_button	reset_offer(dr, tool, id) = ExplainRebuild if dr ∈ {Cdhash, Unsigned}, regardless of tool and id; else HideNoTool if ¬tool.can_run(); else Offer if id = Some(s) with bundle_id_is_usable(s); else HideNoBundleId. shows_button(o) ⇔ o = Offer. So the Default input gives HideNoTool.	src/consent.rs::the_repair_offer_is_a_table, src/consent.rs::a_rebuild_scoped_identity_suppresses_the_button_entirely	EASY: nested enum/Option match, composition with tccutil-presence-and-subject	0,1,3,4
reset-plan	contract	ResetPlan::{new,for_offer,commands,bundle_id,folders}	new(id, fs) = None iff ¬bundle_id_is_usable(id) ∨ fs = []. Otherwise Some(P) with P.bundle_id = trim(id) and P.folders = fs deduplicated keeping first occurrences, in order (type invariant: distinct, non-empty). P.commands() = [(f, tccutil_reset_command(P.bundle_id, f)) for f in P.folders]: one command per distinct folder, all naming the same subject. for_offer(inputs, fs) = Some(P) ⇒ reset_offer(inputs).shows_button() ∧ P = new(inputs.bundle_id.unwrap(), fs); it is None whenever the button is hidden.	src/consent.rs::a_plan_refuses_an_unusable_subject_or_an_empty_folder_set, src/consent.rs::a_plan_is_one_command_per_folder_in_plan_order_from_one_subject, src/consent.rs::a_repeated_folder_never_becomes_a_second_invocation, src/consent.rs::only_an_offered_repair_can_be_built	MEDIUM: Vec<Folder> dedup loop with contains, private struct invariant, trim/to_owned, map/collect	0,1,4,5,7
reset-fold	contract	ResetStatus::{from_exit_status,is_reset}, ResetAttempt::{from_exit_status,tool_absent}, consent::{reset_outcome,folders_reset,folders_declined}	from_exit_status(c) = Reset if c = Some(0), else Declined{code: c} (so None ↦ Declined{None}). is_reset ⇔ Reset. With n = |A| and k = #{a ∈ A | is_reset(a)}: reset_outcome(A) = NotAttempted if n = 0; AllReset if k = n; Partial if 0 < k < n; ToolAbsent if k = 0 ∧ every attempt is ToolAbsent; NoneReset otherwise. folders_reset(A) = [a.folder | is_reset(a)] in order. folders_declined(A) = [(a.folder, code) | a.status = Declined{code}] in order, so it never holds a ToolAbsent attempt.	src/consent.rs::one_invocations_status_is_read_from_its_exit_alone, src/consent.rs::the_reset_outcome_is_a_table, src/consent.rs::a_partial_success_is_never_reported_as_every_folder, src/consent.rs::an_absent_tool_is_never_reported_as_a_refusal	MEDIUM: slice filter/count/all/filter_map, enum with payload	0,1,5,7
posture-join	contract	ConsentPosture::join	∀ I: join(I).attribution = I.adoption, .responsible = I.responsible, .fda = I.fda, .fda_scope = I.evidence.fda_scope. fs_consent = Unknown if I.adoption = Adopted ∧ ¬handoff_attribution_measured; else Denied if I.observed_eperm; else Covered if fda = Granted ∧ adoption = Live ∧ fda_coverage_measured; else Unknown. prompt_possible = ¬(fda = Granted ∧ fda_coverage_measured). Corollaries: under SpikeEvidence::UNMEASURED, fs_consent ≠ Covered and prompt_possible; Denied ⇒ observed_eperm; SpikeEvidence::default() = UNMEASURED and UNMEASURED.fda_scope = Unknown.	src/consent.rs::posture_join_is_a_table_under_todays_evidence, src/consent.rs::an_observed_eperm_is_the_only_route_to_denied, src/consent.rs::an_adopted_session_is_never_denied_before_s2, src/consent.rs::covered_requires_grant_plus_live_plus_measured_coverage, src/consent.rs::attribution_comes_from_the_adoption_record_not_the_spi, src/consent.rs::fda_scope_is_carried_from_the_evidence_and_is_unknown_today	EASY: const fn over Copy structs of bools and enums; finite, decidable	0,1,3
debugger-change-order	contract	DebuggerChange::{command,precondition,target,as_str,from_token}, consent::{devtools_change_with,devtools_status_command}, DevtoolsChangeRun::spawned	status = [DEVTOOLSSECURITY_PATH, "-status"], On.command = [P, "-enable"], Off.command = [P, "-disable"], with P absolute; no flagless argv exists, and DEBUGGER_ACCESS_REVERT = join(Off.command, " "). precondition(On) = Disabled, precondition(Off) = Enabled; target is the opposite mode; precondition is never Unrecognized or Unknown. from_token(t) = Some(c) ⇔ t = as_str(c). devtools_change_with calls recheck exactly once; if that read's mode ≠ precondition(change) the result is NotDue{observed: that read} and spawn is not called; otherwise the result is spawn(change.command()), called once. So a refused read (mode Unknown) spawns nothing. spawned(r) ⇔ r ∈ {Exited, WaitFailed}.	src/consent.rs::the_change_builders_always_carry_their_flags, src/consent.rs::a_change_spawns_only_over_a_fresh_read_of_its_precondition, src/consent.rs::only_a_started_child_counts_as_spawned	MEDIUM: const [&str;2] arrays, generic FnOnce params with call-trace obligation	0,1,3,11,13
change-runner	contract	consent::run_to_exit_keeping_heads	Over a process model: argv = [] → SpawnFailed. Spawn NotFound → Absent; other spawn errors → SpawnFailed; in both, on_spawn is not called. Otherwise on_spawn(pid) is called exactly once, before the wait. The child gets stdin /dev/null, is never killed, and the wait has no deadline. A failed wait → WaitFailed. Otherwise Exited{code, stdout_head, stderr_head}, each head the lossy first min(len, 4096) bytes of its stream if its reader finished within the grace polls, else ""; the reader drains past the head.	src/consent.rs::the_change_runner_keeps_the_exit_and_a_head_of_each_stream, src/consent.rs::the_change_runner_waits_and_names_a_missing_program	HARD: spawn/wait, threads, read loop into a fixed buffer, bounded polling	0,7,8,13
devtools-child-running	contract	DevtoolsChild::is_running, consent::{process_birth,imp::process_birth}	is_running(c) ⇔ c.birth = Some(b) ∧ process_birth(c.pid) = Some(b). process_birth(pid) = None for pid ≤ 1, for a pid that does not fit pid_t, for a short proc_pidinfo record, and for a record whose pbi_pid ≠ pid; else Some{start sec, usec}; always None off macOS. Corollary: a record with birth None never runs, and a different birth at the same pid is another process.	src/consent.rs::a_recorded_child_runs_only_while_its_birth_matches	MEDIUM: unsafe proc_pidinfo FFI, MaybeUninit, integer conversions	0,8
devtools-status-classifier	contract	consent::{devtools_status_line,classify_devtools_status}	devtools_status_line(l) with x = trim(l): Unrecognized if x = "Developer mode rights are"++m++" in an unknown state."; Enabled if x = "Developer mode is"++m++" enabled."; Disabled with " disabled."; in every case trim(m) ∈ {"", currently, already, now}; every other line → None. classify_devtools_status maps Absent, SpawnFailed, TimedOut, Unreadable to Unknown with their own labels. For Exited{code, out, err}, with S = the Some values of devtools_status_line over out's lines then err's: S = [] or not all equal → (Unknown, StatusUnrecognized); S[0] = Unrecognized → (Unrecognized, StatusOk) for any code; S[0] ∈ {Enabled, Disabled} ∧ code = Some(0) → (S[0], StatusOk); else (Unknown, StatusUnrecognized). So no unrecognised output ever reads Disabled.	src/consent.rs::the_status_classifier_reads_only_what_the_tool_says	MEDIUM: strip_prefix/strip_suffix/trim, const array contains, chained line iterators	0,1,5,7,13
debugger-pure-classifiers	table-property	consent::{classify_membership,classify_cs_status}	classify_membership(admin, dev) = Admin if admin = Some(true) (any dev); with admin = Some(false): Developer if dev = Some(true), NotMember if Some(false), Unknown if None; with admin = None: Unknown. classify_cs_status(None) = Unknown; for Some(f): Hardened ⇔ f & CS_RUNTIME ≠ 0 ∧ f & CS_GET_TASK_ALLOW = 0, else NotHardened. Constants: CS_RUNTIME = 0x10000, CS_GET_TASK_ALLOW = 4, CS_OPS_STATUS = 0.	src/consent.rs::membership_names_the_first_rule_that_matches, src/consent.rs::hardening_is_the_runtime_without_get_task_allow	EASY: const fn match on Option tuples, u32 bitwise and	0,3
debugger-probe-gate	contract	consent::probe_debugger_with	First that applies: ¬permits → Refused(RefusedDisabled); exe = None → Refused(RefusedNoExe); ¬path_is_in_app_bundle(exe) → Refused(RefusedOutOfBundle); in these three neither closure is called. Otherwise spawn is called exactly once with devtools_status_command() and membership exactly once; with (m, l) = classify_devtools_status(run), the result is {mode: m, member: membership(), label: l}. A failed run still carries the membership it read.	src/consent.rs::the_debugger_probe_refuses_without_spawning, src/consent.rs::the_debugger_probe_spawns_only_the_status_argv	MEDIUM: two generic FnOnce params with call-trace obligations	0,1,4,11,13
real-probes-refuse-outside-bundle	contract	consent::{probe_fda,probe_debugger,probe_devtool,probe_devtool_list,responsible_pid,responsible_pid_detailed,responsible_app}	On macOS, for a process with fence_exe() not in a bundle (neither current_exe nor proc_pidpath is) and an open gate: probe_fda(gate) = Refused(RefusedOutOfBundle); probe_debugger(gate) = Refused(RefusedOutOfBundle) with mode and member Unknown; probe_devtool(gate, s) and probe_devtool_list(gate, s) = Refused(RefusedOutOfBundle) for usable s; no syscall, open or spawn happens past the fence. Off macOS each wrapper returns its unsupported_*() value, responsible_pid* gives None or Err(Unsupported), responsible_app gives None. Proof: composition of fence-exe with each *-probe-gate instantiated at the live arms.	src/consent.rs::the_real_probe_refuses_from_a_test_binary, src/consent.rs::the_real_debugger_probe_refuses_from_a_test_binary, src/consent.rs::the_real_devtool_probe_refuses_from_a_test_binary, src/consent.rs::the_real_list_probe_refuses_from_a_test_binary, src/consent.rs::off_macos_every_os_wrapper_answers_unknown	MEDIUM once the gates are proved: cfg arms, current_exe/proc_pidpath effects, callee-ensures composition	0,4,8
devtool-log-classifier	contract	consent::{classify_devtool_log,devtool_answer,tccd_record,devtool_trailer}	Exited with code ≠ Some(0) → Refused(LogFailed); Absent, SpawnFailed → Refused(SpawnFailed); TimedOut → Refused(TimedOut); Unreadable → Refused(OutputUnreadable). For Exited{Some(0), out}: Refused(FormatUnrecognized) unless every line is a json::object_line; exactly the last line is the trailer {count:n, finished:1} with exactly those keys and n = the number of preceding records; every other line is a tccd_record (eventType=logEvent, processImagePath = senderImagePath = TCCD_IMAGE_PATH, subsystem com.apple.TCC, category access, formatString = TCCD_ACCESS_FORMAT, userID = 0, string eventMessage and timestamp); devtool_answer(msg, subject) is Ok for each record; devtool_timestamp_ms(ts) is Some for each Ok(Some) record. devtool_answer(msg, S) = Err unless msg starts with DEVTOOL_MESSAGE_PREFIX; with sub = text before the first '}', sub ≠ S → Ok(None); else Ok(Some(state)) iff the text after that '}' starts with 'Resp:{', msg has no '<decode:' and no '<…>', exactly one '}Resp:{' and one 'ReqResult(', and its suffix from '}, ReqResult(Auth Right: ' parses as v ' (' r '), promptType: ' digits+ ',DB Action:' a [', UpdateVerifierData'] ')' to the end with r ∈ TCCD_REASONS, a ∈ TCCD_DB_ACTIONS and (v, r) ∈ {(Allowed,_)↦Allowed, (Denied,_)↦Denied, (Unknown,None)↦Unlisted}; every other case Err. With valid records: no Ok(Some) → {Unknown, NoRecentRequest, 0, 0, None}; else state of the record with maximal timestamp (later line wins ties), label Observed, requests = #Ok(Some), not_allowed = #(state ≠ Allowed), at = that timestamp. The subject match is exact.	src/consent.rs::the_devtool_classifier_reads_only_what_tccd_says	HARD: line loop with early returns, Vec<(String, json::Value)> lookups, split_once/strip_suffix/matches().count(), saturating counters, Option fold	0,1,5,7,13
devtool-log-no-forged-allow	invariant	consent::{classify_devtool_log,devtool_answer}	Corollary of devtool-log-classifier. ∀ usable S, every binary-path string b and timestamp t, with R the record carrying tccd_message(S, b, "Unknown (None)") under ndjson escaping: classify_devtool_log(log_of([R]), S).state ≠ Allowed, and state = Unlisted ∨ label = FormatUnrecognized. Generally, state = Allowed ⇒ the newest subject record's message ends, in tccd's own suffix, with 'Auth Right: Allowed (…)' and holds exactly one 'ReqResult(' and one '}Resp:{', which program-controlled text cannot produce.	src/consent.rs::no_planted_path_reads_allowed	HARD: ∀ over strings; String content model and substring-count lemmas	0,1,5,7
devtool-timestamp	contract	consent::{devtool_timestamp_ms,days_from_civil,days_in_month,digits,devtool_answer_age_ms}	devtool_timestamp_ms(s) = Some(ms) iff s = D ' ' C O with D = YYYY-MM-DD fixed-width, 1 ≤ MM ≤ 12, 1 ≤ DD ≤ days_in_month(Y, M) (Gregorian leap rule); C = HH:MM:SS[.f], HH ≤ 23, MM ≤ 59, SS ≤ 59, f of 1–9 digits when '.' is present; O = ±hhmm, hh ≤ 23, mm ≤ 59. Then ms = (days_from_civil(Y,M,D)·86400 + HH·3600 + MM·60 + SS − sign·(hh·3600 + mm·60))·1000 + the first 3 digits of f, zero-padded. days_from_civil(y,m,d) = days from 1970-01-01 to that proleptic-Gregorian date, a bijection on valid dates. devtool_answer_age_ms(a, n) = max(0, n.saturating_sub(a)) as u64.	src/consent.rs::the_devtool_answer_age_is_utc_arithmetic	MEDIUM-HARD: i64 division/modulo (overflow obligations), split/get, fold over bytes, saturating_sub, unsigned_abs	0,1,3,5,7
ndjson-reader	contract	consent::json::{object_line,str,uint,Reader}	object_line(l) = Some(fields) iff l, JSON-whitespace trimmed, is exactly one RFC 8259 value that is an object with pairwise-distinct top-level keys and nesting depth ≤ 16 (no raw control char in a string, paired surrogates, no leading-zero numbers, no empty fraction or exponent, no trailing comma, no bare key); fields holds the pairs in order, strings decoded, numbers as source text. str(f, k) = the first pair with key k when its value is a string. uint(f, k) = the first pair with key k when it is a Number whose text is all digits and either "0" or has no leading '0', parsed as u64; else None.	src/consent.rs::the_ndjson_reader_is_strict	HARD: recursive-descent parser over &[u8] with &mut cursor, recursion depth bound, char::from_u32, String building	0,1,2,3,7
devtool-probe-gate	contract	consent::{probe_devtool_with,devtool_subject_is_usable,DEVTOOL_LOG_ARGS}	First that applies: ¬permits → Refused(RefusedDisabled); exe = None → Refused(RefusedNoExe); out of bundle → Refused(RefusedOutOfBundle); ¬devtool_subject_is_usable(S) → Refused(RefusedNoSubject); in these four neither store_readable nor spawn is called. Then store_readable() is called exactly once; false → Refused(LogUnavailable) with no spawn. Otherwise classify_devtool_log(spawn(LOG_TOOL, DEVTOOL_LOG_ARGS), S), spawn called exactly once. devtool_subject_is_usable(S) ⇔ bundle_id_is_usable(S) ∧ S ∈ [A-Za-z0-9.-]+. The argv is constant: the subject never appears in it, the style is ndjson, LOG_TOOL = /usr/bin/log, and the '--last' value equals DEVTOOL_WINDOW_S/60 'm'.	src/consent.rs::the_devtool_probe_refuses_without_spawning, src/consent.rs::the_devtool_probe_spawns_only_the_log_argv	MEDIUM: generic FnOnce params with call-trace obligations, const [&str;10]	0,1,4,11,13
devtool-db-classifier	contract	consent::{classify_devtool_db,devtool_db_row}	devtool_db_row(l) = Some((client, kind, auth, mod)) iff l splits on '|' into exactly 4 fields: field 1 an even-length run of [0-9A-F] whose decoded bytes, lossily UTF-8, are client; fields 2–4 decimal runs of 1–18 digits with kind and auth fitting u32 and mod an i64. classify_devtool_db(run, S): Exited with code ≠ Some(0) → Refused(ToolFailed); Absent, SpawnFailed → Refused(SpawnFailed); TimedOut → Refused(TimedOut); Unreadable → Refused(OutputUnreadable). For Exited{Some(0), out}: any non-row line → Refused(FormatUnrecognized). With rows = the line count and O = rows with kind = 0 ∧ client = S: |O| > 1 → FormatUnrecognized; |O| = 0 → {Absent, Read, None, Some(rows)}; O = [(auth, m)]: auth = 2 → {On, Read, Some(m), Some(rows)}, auth = 0 → {Off, Read, Some(m), Some(rows)}, other auth → FormatUnrecognized. Corollary: planted text can force Unknown but never On.	src/consent.rs::the_list_classifier_reads_only_the_subjects_bundle_row	MEDIUM-HARD: split iterator, chunks(2) hex decode into Option<Vec<u8>>, parse::<i64>, u32::try_from, Option::replace	0,1,5,7,13
devtool-list-probe-gate	contract	consent::{probe_devtool_list_with,DEVTOOL_DB_ARGS}	First that applies: ¬permits → Refused(RefusedDisabled); exe = None → Refused(RefusedNoExe); out of bundle → Refused(RefusedOutOfBundle); unusable subject → Refused(RefusedNoSubject); in these four neither closure is called. Then open_db is called exactly once with SYSTEM_TCC_DB: Errno(1) → Refused(NoFullDiskAccess), other Errno → Refused(OpenFailed), neither spawns; Ok → classify_devtool_db(spawn(SQLITE_TOOL, DEVTOOL_DB_ARGS), S), spawn called exactly once. The argv is constant: -readonly, -safe, -nofollow, -bail, then '-init' '/dev/null', and a query selecting only Developer Tools rows with hex(client); the subject is in no argument.	src/consent.rs::the_list_probe_refuses_without_opening_or_spawning, src/consent.rs::the_list_probe_opens_once_and_spawns_only_the_sqlite_argv	MEDIUM: generic FnOnce params with call-trace obligations, const [&str;13]	0,1,4,11,13
devtool-grant-join	contract	consent::devtool_grant	T (tccd answer) = (answered_at, Granted) if log.label = Observed with state Allowed, (answered_at, NotGranted) if Observed with Denied or Unlisted, else None. W (switch) = Granted if list.label = Read with On, NotGranted if Read with Off or Absent, else None. devtool_grant(list, log) = NotGranted if W is Some and list.listing = Absent; if W and T are both Some, T's verdict when answered_at = Some(a), modified_at_s = Some(s) and a > s·1000 (saturating), else W; W if only W; T's verdict if only T; Unknown if neither.	src/consent.rs::the_grant_joins_the_switch_and_tccds_answer	EASY: tuple matches over Copy structs, saturating_mul	0,1,3
platform-ffi-wrappers-total	no-panic	consent::{imp::in_group,imp::group_gid,debugger_membership,responsible_pid_detailed,imp::responsible_app_with_roots,responsible_app,ERRNO_EPERM,ERRNO_ESRCH}	Each wrapper terminates and never panics. in_group(g) = None whenever group_gid(g) = None (non-existent group, a name with NUL, buffer bound exceeded). responsible_pid_detailed never returns Ok(p) with p < 0 (by responsible-classification). responsible_app(pid) = Some(app) ⇒ app.path is the absolute proc_pidpath of app.pid and app.display_name ≠ "". ERRNO_EPERM = libc::EPERM and ERRNO_ESRCH = libc::ESRCH on macOS.	src/consent.rs::the_membership_query_is_total, src/consent.rs::the_responsibility_spi_is_total_for_our_own_process, src/consent.rs::responsible_app_is_total_and_names_something_real, src/consent.rs::errno_constants_match_the_platform	HARD: unsafe FFI (getgrnam_r ERANGE doubling loop, mbr_*, dlsym+transmute, proc_pidpath), thread-local errno, CString. The errno pins are EASY	0,7,8
```

Merge note: the chunk's specs had no duplicates across files. The one cross-file property, "an uninitialised
process gets the minimal capabilities", is a corollary of `mode-global-write-once` plus `policy-monotone-minimal`;
it is stated once, in the former.

## 2. Test → spec map (TSV; all 195)

`primary` is the spec that retires the test. `also` lists other specs the same test samples. A primary of
`§4` means the test is not a spec sample; its action is in §4.

```tsv
file	test	primary	also
src/lib.rs	test_init_error_message_includes_modes	error-messages-name-their-inputs	
src/lib.rs	test_init_mode_from_flag_error_variants	error-messages-name-their-inputs	parse-mode
src/mode.rs	test_ordering	mode-order	
src/mode.rs	test_parse	parse-mode	
src/mode.rs	test_parse_rejects_bypass_attempts	parse-mode	
src/mode.rs	test_display	parse-mode	
src/mode.rs	test_display_parse_roundtrip	parse-mode	
src/mode.rs	test_parse_error_includes_input	error-messages-name-their-inputs	parse-mode
src/capability.rs	test_network_ordering	§4	policy-monotone-minimal
src/capability.rs	test_fs_ordering	§4	policy-monotone-minimal
src/capability.rs	test_process_ordering	§4	policy-monotone-minimal
src/capability.rs	test_repr_encoding	§4	policy-monotone-minimal
src/policy.rs	test_monotonic_capabilities	policy-monotone-minimal	mode-order
src/policy.rs	test_numeric_policy_table	policy-monotone-minimal	
src/actuator.rs	network_sandbox_in_force_only_for_containment_on_macos	decide-on-decision	
src/actuator.rs	every_mode_permits_the_initial_shell_where_its_sandbox_exists	decide-on-decision	policy-monotone-minimal
src/actuator.rs	containment_fails_closed_without_an_os_sandbox	decide-on-decision	
src/actuator.rs	permit_carries_sbpl_iff_network_sandbox_actuated	decide-on-decision	profile-structure
src/actuator.rs	non_containment_spawn_is_never_os_sandboxed	decide-on-decision	
src/actuator.rs	home_scoped_follows_the_home_not_the_sandbox	decide-on-decision	profile-structure
src/actuator.rs	applied_posture_claims_home_denies_only_when_emitted	decide-on-audit	profile-structure
src/actuator.rs	os_sandbox_actuation_matches_platform	§4	
src/actuator.rs	decision_is_permitted_helper_matches_variant	§4	decide-on-decision
src/actuator.rs	enforcement_proof_network_deny_blocks_loopback_connect	§4	sbpl-profile-denotation
src/actuator.rs	enforcement_proof_file_read_deny_yields_operation_not_permitted	§4	
src/actuator.rs	shell_compat_full_containment_profile_keeps_a_normal_shell_working	§4	sbpl-profile-denotation
src/actuator.rs	secret_deny_full_containment_profile_blocks_secret_but_allows_nonsecret	§4	sbpl-profile-denotation
src/actuator.rs	enforcement_proof_containment_confines_writes	§4	sbpl-profile-denotation, write-confinement
src/actuator.rs	enforcement_proof_history_allowance_widens_nothing	§4	sbpl-profile-denotation, write-confinement
src/actuator.rs	shell_compat_login_shell_runs_under_containment	§4	sbpl-profile-denotation
src/actuator.rs	task_port_probe_child	§4	
src/actuator.rs	enforcement_proof_task_ports_stay_outside	§4	sbpl-profile-denotation
src/actuator.rs	mach_lookup_probe_child	§4	
src/actuator.rs	enforcement_proof_launch_brokers_denied	§4	sbpl-profile-denotation
src/actuator.rs	enforcement_proof_mach_lookup_denied_by_default	§4	sbpl-profile-denotation
src/actuator.rs	preference_write_probe_child	§4	
src/actuator.rs	enforcement_proof_preference_writes_denied	§4	sbpl-profile-denotation
src/actuator.rs	shell_compat_tool_set_runs_under_containment	§4	sbpl-profile-denotation
src/actuator.rs	enforcement_proof_open_and_osascript_stay_refused	§4	sbpl-profile-denotation
src/actuator.rs	sandbox_exec_wrapper_is_present_on_this_macos	§4	
src/sbpl.rs	sbpl_escaping_quotes_and_backslashes	sbpl-escape	
src/sbpl.rs	non_containment_modes_get_no_sandbox	profile-selection	policy-monotone-minimal
src/sbpl.rs	profile_selection_keys_only_on_network_capability_none	profile-selection	
src/sbpl.rs	containment_caps_yield_a_network_plus_secret_profile	profile-selection	profile-structure
src/sbpl.rs	home_unset_yields_no_home_scoped_rule	profile-structure	
src/sbpl.rs	nonexistent_home_falls_back_to_the_base_profile	profile-structure	
src/sbpl.rs	broader_fs_tiers_get_no_write_confinement	profile-structure	write-confinement
src/sbpl.rs	full_profile_starts_with_the_base_profile_and_denies_each_secret	profile-structure	resolved-deny-clauses
src/sbpl.rs	private_data_set_present_only_when_included	profile-structure	
src/sbpl.rs	full_profile_does_not_deny_the_whole_home	profile-structure	
src/sbpl.rs	private_data_is_gated_on_the_fs_capability_not_network	profile-structure	profile-selection
src/sbpl.rs	tmp_only_confines_writes_to_the_writable_roots	write-confinement	profile-structure
src/sbpl.rs	a_history_file_that_is_not_a_plain_file_gets_no_allowance	write-confinement	
src/sbpl.rs	only_the_users_own_tmpdir_joins_the_write_roots	write-confinement	
src/sbpl.rs	secret_paths_are_canonicalized_not_raw	resolved-deny-clauses	
src/sbpl.rs	symlinked_secret_store_is_denied_at_its_canonical_target	resolved-deny-clauses	
src/sbpl.rs	symlinked_ancestor_with_absent_leaf_is_denied_at_resolved_target	resolved-deny-clauses	
src/sbpl.rs	nested_secret_ancestor_nodes_are_write_denied_to_block_rename_relocation	ancestor-write-denies	
src/sbpl.rs	nonexistent_nested_ancestor_is_not_write_denied	ancestor-write-denies	
src/sbpl.rs	symlinked_ancestor_store_is_write_denied_at_its_resolved_nodes	ancestor-write-denies	resolved-deny-clauses
src/sbpl.rs	ancestor_write_deny_dedups_shared_parents	ancestor-write-denies	
src/sbpl.rs	sandbox_exec_path_is_the_fixed_absolute_wrapper	§4	
src/consent.rs	out_of_bundle_paths_are_not_in_a_bundle	app-bundle-root	
src/consent.rs	in_bundle_paths_resolve_to_their_bundle_root	app-bundle-root	
src/consent.rs	the_bundle_layout_root_ignores_the_directory_name	app-bundle-root	
src/consent.rs	cache_bundle_prefers_the_bundle_root	app-bundle-root	
src/consent.rs	the_fence_judges_the_bundled_image_behind_a_launch_symlink	fence-exe	app-bundle-root
src/consent.rs	the_image_anchor_names_every_way_the_code_can_go_missing	image-anchor	app-bundle-root
src/consent.rs	probe_refuses_out_of_bundle_without_touching_the_os	fda-probe-gate	refusal-labels
src/consent.rs	probe_refuses_when_disabled_or_unresolvable_without_touching_the_os	fda-probe-gate	refusal-labels
src/consent.rs	probe_reaches_the_syscall_exactly_once_from_inside_a_bundle	fda-probe-gate	
src/consent.rs	probe_classification_is_a_table	fda-probe-gate	
src/consent.rs	refusal_labels_all_report_no_syscall	refusal-labels	
src/consent.rs	the_non_macos_answer_is_unknown	refusal-labels	
src/consent.rs	tokens_render_for_every_state	wire-tokens	
src/consent.rs	responsible_tokens_render	wire-tokens	
src/consent.rs	the_debugger_tokens_are_the_documented_spellings	wire-tokens	
src/consent.rs	the_devtool_tokens_are_the_documented_spellings	wire-tokens	
src/consent.rs	the_list_and_grant_tokens_are_the_documented_spellings	wire-tokens	
src/consent.rs	responsible_answers_are_a_table	responsible-classification	
src/consent.rs	minus_one_never_becomes_self	responsible-classification	
src/consent.rs	empty_overrides_resolve_to_private_subdirs	protected-roots	
src/consent.rs	overrides_replace_the_default_and_are_expanded	protected-roots	
src/consent.rs	no_home_means_no_default_roots_and_absolute_overrides_only	protected-roots	
src/consent.rs	containment_is_a_prefix_test_over_whole_components	protected-roots	
src/consent.rs	the_plist_reader_is_exact_and_xml_only	plist-string-and-display-name	
src/consent.rs	display_name_prefers_display_then_name_then_basename	plist-string-and-display-name	
src/consent.rs	display_name_does_not_bind_a_prefix_key	plist-string-and-display-name	
src/consent.rs	info_plist_under_a_protected_root_is_refused_and_falls_back_to_the_basename	plist-string-and-display-name	protected-roots
src/consent.rs	dr_classification_is_a_table	classify-dr	
src/consent.rs	only_an_identity_requirement_is_grant_stable	classify-dr	
src/consent.rs	the_signing_readers_fail_toward_the_weaker_claim	codesign-report-readers	
src/consent.rs	unsigned_is_the_whole_report_not_a_phrase_in_it	codesign-report-readers	classify-dr
src/consent.rs	the_identity_recheck_tells_an_unsigned_copy_from_a_failed_read	codesign-report-readers	
src/consent.rs	an_unreadable_requirement_conflicts_rather_than_matching	claimant-offer-rule	
src/consent.rs	nothing_is_retirable_beside_an_unstable_running_copy	claimant-offer-rule	
src/consent.rs	a_developer_id_copy_is_never_retirable	claimant-offer-rule	
src/consent.rs	only_listed_copies_are_offered_for_the_trash	claimant-offer-rule	
src/consent.rs	an_unsigned_copy_is_unsigned_and_offered_an_unreadable_one_is_not	claimant-offer-rule	codesign-report-readers
src/consent.rs	the_census_finds_every_claimant_including_rollback_and_registered_copies	classify-claimants	census-roots-and-names
src/consent.rs	every_hole_downgrades_the_census_and_a_complete_lone_install_is_sole	classify-claimants	claimant-offer-rule
src/consent.rs	only_a_shown_still_offered_idle_copy_is_moved_to_the_trash	retire-claimants	claimant-offer-rule
src/consent.rs	bundle_names_include_the_rollback_suffix	census-roots-and-names	
src/consent.rs	the_search_roots_start_where_this_process_runs_from	census-roots-and-names	
src/consent.rs	the_census_does_not_look_inside_a_protected_root	census-fence	protected-roots
src/consent.rs	the_walk_stays_inside_the_bundles_own_contents	census-fence	
src/consent.rs	the_walk_does_not_cross_into_another_volume	census-fence	
src/consent.rs	the_spelled_protected_arm_refuses_on_its_own	census-fence	
src/consent.rs	the_census_lists_through_the_protected_fence	census-fence	classify-claimants
src/consent.rs	the_candidate_reader_tells_ours_from_not_a_bundle_from_a_hole	census-fence	info-plist-cf-reading
src/consent.rs	the_bundle_id_is_read_off_the_bundle_and_never_under_a_protected_root	census-fence	
src/consent.rs	the_walk_reads_the_plist_as_codesign_does	info-plist-cf-reading	census-fence
src/consent.rs	a_binary_info_plist_answers_whose_bundle_it_is	info-plist-cf-reading	
src/consent.rs	only_plain_file_names_are_plain	info-plist-cf-reading	
src/consent.rs	a_tool_report_read_in_part_is_no_report	bounded-run	
src/consent.rs	a_pipe_held_open_past_the_tool_cannot_outlast_the_ceiling	bounded-run	
src/consent.rs	a_bounded_run_keeps_its_outcome_apart	bounded-run	
src/consent.rs	the_fda_probe_descriptor_is_close_on_exec_from_its_open	fda-open-probe	
src/consent.rs	folder_names_services_and_paths	folder-table	
src/consent.rs	the_warmup_default_is_the_folders_without_app_data	folder-table	
src/consent.rs	the_app_data_item_answers_to_every_spelling_of_its_name	folder-table	
src/consent.rs	every_item_has_its_own_label_and_noun	folder-table	
src/consent.rs	only_the_app_data_probe_reads_a_missing_path_as_permission	folder-table	
src/consent.rs	folder_paths_are_resolved_data_for_the_warmup	folder-table	
src/consent.rs	the_reset_command_is_built_from_the_running_bundle_id	tccutil-reset-command	
src/consent.rs	the_reset_command_can_never_take_the_every_app_form	tccutil-reset-command	
src/consent.rs	tccutil_presence_is_a_table_and_only_executable_may_run	tccutil-presence-and-subject	
src/consent.rs	a_usable_bundle_id_is_non_blank_flagless_and_whitespace_free	tccutil-presence-and-subject	
src/consent.rs	the_repair_offer_is_a_table	reset-offer	
src/consent.rs	a_rebuild_scoped_identity_suppresses_the_button_entirely	reset-offer	
src/consent.rs	a_plan_refuses_an_unusable_subject_or_an_empty_folder_set	reset-plan	
src/consent.rs	a_plan_is_one_command_per_folder_in_plan_order_from_one_subject	reset-plan	tccutil-reset-command
src/consent.rs	a_repeated_folder_never_becomes_a_second_invocation	reset-plan	
src/consent.rs	only_an_offered_repair_can_be_built	reset-plan	reset-offer
src/consent.rs	one_invocations_status_is_read_from_its_exit_alone	reset-fold	
src/consent.rs	the_reset_outcome_is_a_table	reset-fold	
src/consent.rs	a_partial_success_is_never_reported_as_every_folder	reset-fold	
src/consent.rs	an_absent_tool_is_never_reported_as_a_refusal	reset-fold	
src/consent.rs	posture_join_is_a_table_under_todays_evidence	posture-join	
src/consent.rs	an_observed_eperm_is_the_only_route_to_denied	posture-join	
src/consent.rs	an_adopted_session_is_never_denied_before_s2	posture-join	
src/consent.rs	covered_requires_grant_plus_live_plus_measured_coverage	posture-join	
src/consent.rs	attribution_comes_from_the_adoption_record_not_the_spi	posture-join	
src/consent.rs	fda_scope_is_carried_from_the_evidence_and_is_unknown_today	posture-join	
src/consent.rs	the_change_builders_always_carry_their_flags	debugger-change-order	
src/consent.rs	a_change_spawns_only_over_a_fresh_read_of_its_precondition	debugger-change-order	
src/consent.rs	only_a_started_child_counts_as_spawned	debugger-change-order	
src/consent.rs	the_change_runner_keeps_the_exit_and_a_head_of_each_stream	change-runner	
src/consent.rs	the_change_runner_waits_and_names_a_missing_program	change-runner	
src/consent.rs	a_recorded_child_runs_only_while_its_birth_matches	devtools-child-running	
src/consent.rs	the_status_classifier_reads_only_what_the_tool_says	devtools-status-classifier	
src/consent.rs	membership_names_the_first_rule_that_matches	debugger-pure-classifiers	
src/consent.rs	hardening_is_the_runtime_without_get_task_allow	debugger-pure-classifiers	
src/consent.rs	the_debugger_probe_refuses_without_spawning	debugger-probe-gate	refusal-labels
src/consent.rs	the_debugger_probe_spawns_only_the_status_argv	debugger-probe-gate	devtools-status-classifier
src/consent.rs	the_real_probe_refuses_from_a_test_binary	real-probes-refuse-outside-bundle	fence-exe, fda-probe-gate
src/consent.rs	the_real_debugger_probe_refuses_from_a_test_binary	real-probes-refuse-outside-bundle	fence-exe, debugger-probe-gate
src/consent.rs	the_real_devtool_probe_refuses_from_a_test_binary	real-probes-refuse-outside-bundle	fence-exe, devtool-probe-gate
src/consent.rs	the_real_list_probe_refuses_from_a_test_binary	real-probes-refuse-outside-bundle	fence-exe, devtool-list-probe-gate
src/consent.rs	off_macos_every_os_wrapper_answers_unknown	real-probes-refuse-outside-bundle	refusal-labels
src/consent.rs	the_devtool_classifier_reads_only_what_tccd_says	devtool-log-classifier	ndjson-reader, devtool-timestamp
src/consent.rs	no_planted_path_reads_allowed	devtool-log-no-forged-allow	devtool-log-classifier
src/consent.rs	the_devtool_answer_age_is_utc_arithmetic	devtool-timestamp	
src/consent.rs	the_ndjson_reader_is_strict	ndjson-reader	
src/consent.rs	the_devtool_probe_refuses_without_spawning	devtool-probe-gate	refusal-labels
src/consent.rs	the_devtool_probe_spawns_only_the_log_argv	devtool-probe-gate	devtool-log-classifier
src/consent.rs	the_list_classifier_reads_only_the_subjects_bundle_row	devtool-db-classifier	
src/consent.rs	the_list_probe_refuses_without_opening_or_spawning	devtool-list-probe-gate	refusal-labels
src/consent.rs	the_list_probe_opens_once_and_spawns_only_the_sqlite_argv	devtool-list-probe-gate	devtool-db-classifier
src/consent.rs	the_grant_joins_the_switch_and_tccds_answer	devtool-grant-join	
src/consent.rs	the_membership_query_is_total	platform-ffi-wrappers-total	debugger-pure-classifiers
src/consent.rs	the_responsibility_spi_is_total_for_our_own_process	platform-ffi-wrappers-total	responsible-classification
src/consent.rs	responsible_app_is_total_and_names_something_real	platform-ffi-wrappers-total	
src/consent.rs	errno_constants_match_the_platform	platform-ffi-wrappers-total	
src/consent.rs	no_bundle_id_literal_lives_outside_the_tests	§4	
src/consent.rs	a_test_binary_is_not_hardened	§4	debugger-pure-classifiers
src/consent.rs	the_real_tccutil_presence_reads_the_mode_of_a_real_file	§4	tccutil-presence-and-subject
tests/audit_denial_logged.rs	log_denial_emits_audit_event	audit-line-format	
tests/audit_denial_logged.rs	log_denial_includes_all_modes	audit-line-format	
tests/audit_denial_logged.rs	log_posture_is_neutral_not_denial	audit-line-format	
tests/audit_denial_logged.rs	decide_permit_is_not_logged_as_denied	decide-on-audit	audit-line-format
tests/audit_denial_logged.rs	posture_claims_resource_limits_only_for_the_mode_that_has_them	decide-on-audit	
tests/claimant_census.rs	a_link_is_the_bundle_it_names	classify-claimants	census-fence
tests/claimant_census.rs	the_real_census_tells_a_rollback_and_a_backup_from_the_running_copy	§4	codesign-report-readers, classify-claimants
tests/claimant_census.rs	still_claims_follows_the_bundle_on_disk	§4	census-fence
tests/claimant_census.rs	still_claims_tells_an_unsigned_copy_from_a_failed_read	§4	codesign-report-readers
tests/image_anchor_live.rs	the_anchor_follows_a_bundle_renamed_and_then_deleted_under_a_live_process	§4	image-anchor
tests/image_anchor_live.rs	a_retargeted_launch_symlink_is_not_a_displacement	§4	image-anchor, fence-exe
tests/image_anchor_live.rs	anchor_probe_child	§4	
tests/init_from_flag_default.rs	init_from_flag_uses_default_when_no_flag	mode-global-write-once	
tests/init_from_flag_rejects_unparsable.rs	init_from_flag_rejects_unparsable_values	mode-global-write-once	parse-mode
tests/init_from_flag_with_value.rs	init_from_flag_uses_the_flag_over_default	mode-global-write-once	
tests/init_mode_is_write_once.rs	init_mode_is_write_once	mode-global-write-once	error-messages-name-their-inputs
tests/uninitialized_mode.rs	mode_or_containment_defaults_to_containment_when_uninit	mode-global-write-once	
tests/uninitialized_mode.rs	uninitialized_mode_gets_most_restrictive_capabilities	mode-global-write-once	policy-monotone-minimal
```

## 3. Coverage

- 195 tests read. 162 map to one of the 57 specs as primary. 33 are not spec samples (§4).
- The biggest collapses: `profile-structure` (7), `census-fence` (7), `decide-on-decision` (6),
  `mode-global-write-once` (6), `folder-table` (6), `posture-join` (6), `claimant-offer-rule` (5),
  `wire-tokens` (5), `real-probes-refuse-outside-bundle` (5).
- `sbpl-profile-denotation` has no primary test. It is the model that the 16 kept kernel-conformance tests
  in `src/actuator.rs` bind to macOS; proving it makes those tests the model's conformance lane rather than
  its only evidence.
- The six `tests/*` integration binaries for the global mode each exist only to get a fresh process
  (`OnceLock`). Once `mode-global-write-once` is proved, all six files go, together with
  `tests/containment_suite.rs` if nothing else uses its `in_fresh_process` helper.

## 4. Tests that are not spec samples

Actions: **delete** (it samples nothing the code could get wrong); **keep as conformance** (a live check that
binds a model or a platform assumption to the real kernel or tool; it leaves the unit suite for the
conformance lane, with its control); **move-to-bench** (an observation of this machine, not of aterm code);
**move to a guard** (a source-text rule that becomes an `aterm-census` guard under `verify guards`).

Status (2026-10-16): the 7 **delete** rows below are deleted from the crate.

| test | why | action |
|---|---|---|
| src/capability.rs::test_network_ordering | Tautology of `derive(PartialOrd)` (declaration order). The meaning of the order is in `policy-monotone-minimal`. | **delete** |
| src/capability.rs::test_fs_ordering | Same as above. | **delete** |
| src/capability.rs::test_process_ordering | Same as above. | **delete** |
| src/capability.rs::test_repr_encoding | Re-asserts the explicit `= n` discriminants of the declaration; duplicates `test_numeric_policy_table`'s encoding check. | **delete** |
| src/actuator.rs::os_sandbox_actuation_matches_platform | The body of `os_sandbox_actuated` is literally `cfg!(target_os = "macos")`. | **delete** |
| src/actuator.rs::decision_is_permitted_helper_matches_variant | Restates the `matches!` that defines `is_permitted` on two hand-built values; `decide-on-decision` uses `is_permitted ⇔ Permit` directly. | **delete** |
| src/sbpl.rs::sandbox_exec_path_is_the_fixed_absolute_wrapper | Pins a const string literal against itself. | **delete** |
| src/actuator.rs::enforcement_proof_network_deny_blocks_loopback_connect | Live Seatbelt check of `deny network*`, with a control. Binds `sbpl-profile-denotation`. | **keep as conformance** |
| src/actuator.rs::enforcement_proof_file_read_deny_yields_operation_not_permitted | Runs a hand-written profile through the test-only `sandbox_wrap`; tests Seatbelt itself, not anything aterm generates. | **keep as conformance** (platform) |
| src/actuator.rs::shell_compat_full_containment_profile_keeps_a_normal_shell_working | Live check that the generated profile does not break `/bin/sh` (over-breadth). | **keep as conformance** |
| src/actuator.rs::secret_deny_full_containment_profile_blocks_secret_but_allows_nonsecret | Live secret-deny check against the real `$HOME/.aws` (§5.7). | **keep as conformance**, on a scratch HOME |
| src/actuator.rs::enforcement_proof_containment_confines_writes | Live write-confinement check over the real `$HOME` and user cache; profile content is `write-confinement`. | **keep as conformance**, on a scratch HOME |
| src/actuator.rs::enforcement_proof_history_allowance_widens_nothing | Live zsh/Seatbelt check; its one profile-content assertion is also `write-confinement`. | **keep as conformance** |
| src/actuator.rs::shell_compat_login_shell_runs_under_containment | Live check that the user's login shell runs under the profile. | **keep as conformance** |
| src/actuator.rs::task_port_probe_child | `#[ignore]`d probe child of `enforcement_proof_task_ports_stay_outside`. | **keep as conformance** (with its parent) |
| src/actuator.rs::enforcement_proof_task_ports_stay_outside | Live `mach-priv-task-port` check, with a control. | **keep as conformance** |
| src/actuator.rs::mach_lookup_probe_child | `#[ignore]`d probe child of the two mach-lookup proofs. | **keep as conformance** (with its parents) |
| src/actuator.rs::enforcement_proof_launch_brokers_denied | Live mach-lookup refusal, with a control. | **keep as conformance** |
| src/actuator.rs::enforcement_proof_mach_lookup_denied_by_default | Live deny-by-default plus allowlist, with a control. | **keep as conformance** |
| src/actuator.rs::preference_write_probe_child | `#[ignore]`d probe child of the preference-write proof. | **keep as conformance** (with its parent) |
| src/actuator.rs::enforcement_proof_preference_writes_denied | Live `sandbox_check` of `user-preference-write`, with a control. | **keep as conformance** |
| src/actuator.rs::shell_compat_tool_set_runs_under_containment | Live tool-set check; skips tools the machine lacks. | **keep as conformance** |
| src/actuator.rs::enforcement_proof_open_and_osascript_stay_refused | Live run of `open` and `osascript` under the profile. | **keep as conformance** |
| tests/image_anchor_live.rs::the_anchor_follows_a_bundle_renamed_and_then_deleted_under_a_live_process | Checks the kernel assumption behind `image-anchor` (`proc_pidpath` follows a rename, ENOENT after unlink). | **keep as conformance** |
| tests/image_anchor_live.rs::a_retargeted_launch_symlink_is_not_a_displacement | Checks the kernel resolves the image, not the launch symlink. | **keep as conformance** |
| tests/image_anchor_live.rs::anchor_probe_child | `#[ignore]`d probe child of the two live-anchor tests. | **keep as conformance** (with its parents) |
| tests/claimant_census.rs::the_real_census_tells_a_rollback_and_a_backup_from_the_running_copy | Checks real `codesign -d` output (including the commented `# designated =>` line of an ad-hoc signature) that `codesign-report-readers` assumes. | **keep as conformance** |
| tests/claimant_census.rs::still_claims_follows_the_bundle_on_disk | Checks real codesign gives a new cdhash after a re-sign; the logic is `census-fence` + `codesign-report-readers`. | **keep as conformance** |
| tests/claimant_census.rs::still_claims_tells_an_unsigned_copy_from_a_failed_read | Checks real codesign report shapes; its `same_identity` logic duplicates `the_identity_recheck_tells_an_unsigned_copy_from_a_failed_read`. | **keep as conformance** (drop the duplicated logic assertions) |
| src/actuator.rs::sandbox_exec_wrapper_is_present_on_this_macos | An environment precondition (`/usr/bin/sandbox-exec` exists). | **move-to-bench** |
| src/consent.rs::a_test_binary_is_not_hardened | Observes the test binary's linker ad-hoc signature; `classify_cs_status` is `debugger-pure-classifiers`. | **move-to-bench** |
| src/consent.rs::the_real_tccutil_presence_reads_the_mode_of_a_real_file | Asserts `/usr/bin/tccutil` exists and is executable; `classify_tccutil` is `tccutil-presence-and-subject`. | **move-to-bench** |
| src/consent.rs::no_bundle_id_literal_lives_outside_the_tests | A source-text rule (no `com.aterm` in the shipping half, via `include_str!`). No lint or function proof can say it. | **move to a guard** (`aterm-census`, `verify guards`) |

Totals: delete 7, keep as conformance 22, move-to-bench 3, move to a guard 1.

## 5. Implied specs that look wrong, or disagree with the code

1. **Dead Deny arm in `decide_on`** (`src/actuator.rs:235`). The `#[allow(unreachable_patterns)] _ =>` arm is
   dead: `#[non_exhaustive]` does not apply inside the defining crate, so the match on `ProcessCapability` is
   already exhaustive. Its doc promise (a future capability below `NoFork` gets Deny) cannot be tested, and the
   arm would log a posture line and then a denial, two records for one decision, which contradicts
   `decide-on-audit`'s "exactly one record". Delete the arm, or make it the only record.
2. **Stale isolation comment** in `src/lib.rs` tests: it says `OnceLock` init isolation is not testable in
   one binary, but `tests/containment_suite.rs` `in_fresh_process` re-executes the binary to do exactly that.
3. **Mislabelled property** in the `src/lib.rs` crate doc: "MonotonicCapabilities: capabilities only decrease
   over time", said to be pinned by `policy.rs`/`mode.rs` tests. No test checks anything over time, and the
   mode cannot change after init (`mode-global-write-once`). Only per-pair monotonicity
   (`policy-monotone-minimal`) is pinned.
4. **`classify_responsible` needs `requires self_pid > 0`** (`src/consent.rs:750`). `Ok(pid) if pid == self_pid`
   is matched before the `pid > 0` guard, so `classify_responsible(-1, Ok(-1)) = SelfProcess`.
   `minus_one_never_becomes_self` and the doc's "-1 maps to Unknown — never to self" hold only under that
   unstated precondition. Either add the guard to the first arm or state the `requires`.
5. **`responsible_app_is_total_and_names_something_real` asserts `app.pid > 0`**, which the code does not
   guarantee: `responsible_answer` maps `ret = 0` to `Ok(0)` and `responsible_app_with_roots` does not filter
   0. The assertion relies on `proc_pidpath(0)` failing. `platform-ffi-wrappers-total` does not claim it.
6. **The enforcement tests bind a copy of the wrap.** `sandbox_wrap` is a test-only copy that "mirrors the wrap
   done in aterm-pty::spawn_shell", so the conformance tests bind a duplicate of the production wrap, and the
   two can drift. The kept conformance tests should call the production wrap.
7. **Tests that touch the owner's real `$HOME`.** `secret_deny_full_containment_profile_blocks_secret_but_allows_nonsecret`
   and `enforcement_proof_containment_confines_writes` probe the real `$HOME`; one may create `~/.aws`, and a
   failed deny would create files under `$HOME` or append to `~/.zshrc`. Both also return early (vacuous pass)
   when `$HOME` is unset. Likewise `sbpl::private_data_is_gated_on_the_fs_capability_not_network` and
   `containment_caps_yield_a_network_plus_secret_profile` read the real `$HOME`, and the first silently passes
   when `HOME` is unset or does not canonicalize; the property belongs on `profile_scoped` with an injected home.
8. **Mixed test.** `the_fence_judges_the_bundled_image_behind_a_launch_symlink` ends with an environment
   assertion on the real `fence_exe()` (the test binary is not bundled), mixing a pure table with an
   observation of the running process. That half is `real-probes-refuse-outside-bundle`.
9. **Two constants kept in sync only by a test.** `DEVTOOL_WINDOW_S` (`src/consent.rs:3608`) and the literal
   `"10m"` in `DEVTOOL_LOG_ARGS` (`:3644`). Derive the argv from the window, or state the coupling as a const
   assertion; `devtool-probe-gate` then states it.
10. **Docs that claim proof.** `tests/image_anchor_live.rs` says "The table test proves the classification"; a
    test samples it and nothing proves it yet. `src/capability.rs` says the discriminants are "pinned by the
    tests below"; those tests are tautologies listed for deletion, so the doc should point at
    `policy-monotone-minimal`.

## 6. Proof order (easiest high-value first)

No spec here is blocked by gap 0 alone: every one also needs at least one of gaps 1, 3, 7 or 8. So the
first proofs are the ones whose only other gap is 3 (E6 body widening: enum matches, casts, bit ops), then
`{1,3}` finite tables decidable by evaluation once enum carriers land.

1. **`mode-order`** (gaps 0, 3). One cast and a `u8::cmp`. It is the base lemma for `policy-monotone-minimal`,
   `mode-global-write-once` and every "Master/User/Safety are permitted" claim.
2. **`debugger-pure-classifiers`** (gaps 0, 3). `const fn` over `Option` pairs and a `u32` mask.
3. **`policy-monotone-minimal`** (gaps 0, 1, 3). The crate's central security table (4 modes, finite).
   With `mode-order` it retires all of `policy.rs` and lets the four tautologies in `capability.rs` go.
4. **`posture-join`** (gaps 0, 1, 3). A `const fn` over `Copy` structs that retires 6 tests.
5. The other `{0,1,3}` tables: **`wire-tokens`** (5 tests), **`refusal-labels`**, **`responsible-classification`**
   (with the §5.4 `requires`), **`fence-exe`**, **`devtool-grant-join`**, then **`tccutil-reset-command`**
   (gaps 0, 1: needs only a `Vec<String>` content model).
6. Gap 4 composition on top of those: **`image-anchor`**, **`reset-offer`**, then **`folder-table`** and
   **`parse-mode`** (gap 3 plus string content).
7. String and slice contracts on gaps 1, 5, 7: **`protected-roots`**, **`tccutil-presence-and-subject`**,
   **`reset-fold`**, **`reset-plan`**, **`census-roots-and-names`**, **`plist-string-and-display-name`**,
   **`sbpl-escape`**, then the parsers **`classify-dr`**, **`codesign-report-readers`**,
   **`devtools-status-classifier`**, **`devtool-timestamp`**, **`ndjson-reader`**, **`devtool-db-classifier`**,
   **`devtool-log-classifier`** and its corollary **`devtool-log-no-forged-allow`**.
8. Higher-order gates (gaps 11, 13 call-trace specs): **`fda-probe-gate`**, **`debugger-probe-gate`**,
   **`devtool-probe-gate`**, **`devtool-list-probe-gate`**, **`debugger-change-order`**, **`app-bundle-root`**,
   **`claimant-offer-rule`**, **`retire-claimants`**.
9. Over the `FS` model (gap 8): the sbpl chain **`profile-selection`** → **`resolved-deny-clauses`** →
   **`ancestor-write-denies`** → **`write-confinement`** → **`profile-structure`** → **`decide-on-decision`** →
   **`audit-line-format`** → **`decide-on-audit`**; then **`mode-global-write-once`** and
   **`error-messages-name-their-inputs`**; then the census **`classify-claimants`**, **`census-fence`**,
   **`info-plist-cf-reading`**, and the composed **`real-probes-refuse-outside-bundle`**.
10. Process and FFI effects: **`fda-open-probe`**, **`devtools-child-running`**, **`platform-ffi-wrappers-total`**,
    **`change-runner`**, **`bounded-run`**.
11. Last: **`sbpl-profile-denotation`** (gap 10). It needs a `ty_model!` SBPL semantics and a model-to-string
    refinement, and it turns the 16 kept kernel-conformance tests in `src/actuator.rs` into the model's conformance lane.

Gap histogram (number of the 57 specs each gap blocks): 0 → 57, 1 → 49, 2 → 12, 3 → 19, 4 → 11, 5 → 17,
7 → 30, 8 → 20, 10 → 1, 11 → 7, 13 → 21.
