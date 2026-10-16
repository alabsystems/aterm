<!-- Copyright 2026 Andrew Yates -->
<!-- SPDX-License-Identifier: Apache-2.0 -->

# aterm-objc: spec recovery from its 201 unit tests

> Status 2026-10-16: this is the crate's migration worklist. Each of the 35 specs below is
> proved through Trust/Clean about the compiled Rust, and then the tests it subsumes are
> deleted. The 36 checks in §4 sample no spec. They are deleted, or re-homed as
> `aterm-census` guards, without waiting for a proof.
>
> **Tried 2026-10-16 on seal `150efe4a`: `bool-semantics` is blocked, not proved (gap 1).**
> `Bool::new` gets no E6 import: an island over `trust_import_aterm_objc__encode__Bool__new` is
> rejected (`AutoBoundHeadOverApplied { name: "trust_import_aterm_objc__encode__Bool__new", .. }`),
> and no clause can name its struct result (`result.0 == value`, `result == Self(value)` and, with
> a named field, `result.value == value` are each "unsupported contract predicate expression").
> `Bool::as_bool` is admitted, with the newtype flattened to its field (`b : Bool`), but its clause
> `result == self.0` is refused: a clause may project only a named field of a parameter, never a
> tuple index. Renamed to a named field, `ensures result == self.value by …` does certify
> (`clean-kernel-citation`, Certified), but that only restates the body; the round trip needs
> `new`. The code is unchanged and both tests stay; each also checks things the spec does not
> (`Bool::ENCODING` against Foundation and the cfg arms, which is `encode-table`).

Source: `crates/aterm-objc` (read only). I read every `#[test]` in `src/{exception,wake_timer}.rs`
and `tests/*.rs`, and the source each one reaches (`src/{lib,encode,exception,wake_timer,runtime,
declare,class_macro,block,sel_cache,swizzle,weak,main_thread_bound,send}.rs`). Nothing was compiled
or run. The claims below come from reading the code.

Count: a grep for `#[test]` finds 202 lines, but two of them are doc-comment text
(`tests/main_thread_bound_drive.rs:23`, `tests/objc_static_suite.rs:9`). There are 200 real `#[test]`
fns. The `harness = false` target `main_thread_bound_drive` answers libtest's argv as one test from
its `fn main`, so it counts as one more: **201 tests**.

Result: **35 specs**, covering **165 tests**. **36 tests are not spec samples** (§4).

Paths in this file are relative to `crates/aterm-objc` unless they name another crate.

## 0. Notation used in the statements

- `Enc(T)` = `<T as Encode>::ENCODING`. `UA(T)` = `<T as Encode>::HAS_UNALIGNED_FIELDS`. `++` is string
  concatenation.
- `MEnc(R; A1..An)` = `Enc(R) ++ "@:" ++ Enc(A1) ++ … ++ Enc(An)`, for `n ≤ 14`.
- `strip` = `encode::strip_method_offsets`. `depth_i` and `inq_i` are the bracket depth and the
  in-quote flag after scanning the prefix `s[0..i]` of the char sequence `s`. The depth is one counter
  for `{([` and `})]`, saturates at 0, and is ignored inside quotes.
- `Arch` ∈ {aarch64, x86_64}. `BOOLENC(Arch)` = `B` on aarch64, `c` on x86_64. `BOOLPTR(Arch)` = `^B` / `^c`.
- `P`: the platform specification of libobjc, Foundation, CoreFoundation, libdispatch and AppKit that a
  Trust FFI assumption channel would hold (audit gap 8). It covers method tables, the weak table,
  autorelease pools, the block runtime, the run loop, `sel_registerName` interning, and `@encode` as
  clang emits it for `Arch`.
- `RC(o)`: an object's retain count. `Alive(o)`: `o` is not deallocated. `Pools`: the thread's
  autorelease-pool stack.
- `CONTAINED` = `exception::CONTAINED`. `Sink`: the function installed by `set_sink`, else `default_sink`.
- `Model` = `aterm_spec::derive::run_loop_waker_model`, with vars `cache, spent, armed, want, delivered,
  steps` and invariants `NoLostWake` and `BoundedCatchUp`.
- Declared site `D`: one `declare_class!` invocation, with `NAME`, superclass `S`, protocols `Ps`, rows
  `(sel_i, fn_i(&self, A_i..) -> R_i, abort_i ∈ {contain, abort_on_exception})`, and ivars type `I`.
- Gaps: numbers in the `gaps` column are rows of the gap table in
  `AUDIT-trust-for-test-migration-2026-10-15.md` (historical internal audit) §7.
  Gap 0 (a seal that publishes Clean rows) is a prerequisite of every row and is not repeated in the
  column. **No spec here is blocked by gap 0 alone.** Six specs (`strip-offsets-filter`,
  `method-encoding-concat`, `encode-table`, `bool-semantics`, `wake-plan-refines-model`,
  `cf-catch-up-arithmetic`) need no platform lane (gap 8).

## 1. Specs (TSV)

Columns: `id`, `kind`, `functions`, `statement`, `subsumes` (every test that samples the spec; §2 names
the one each test is retired by), `difficulty` (Rust features the proof must handle), and `gaps`
(blocking audit §7 gaps besides 0).

```tsv
id	kind	functions	statement	subsumes	difficulty	gaps
strip-offsets-filter	equivalence	encode::strip_method_offsets	∀ s: &str: strip(s) is the subsequence of s.chars() that keeps the char c at index i unless c is an ASCII digit ∧ depth_i = 0 ∧ ¬inq_i. Corollaries ∀ s: (a) strip(strip(s)) = strip(s); (b) every char of strip(s) is a char of s, in order; (c) every non-digit char of s survives; (d) every digit at depth > 0 or inside quotes survives (array counts, bitfield widths, digits in struct/union/template tags, quoted class names), so significant_tokens(strip(s)) = significant_tokens(s); (e) for well-formed types T0..Tn and decimal offsets o0..on, strip(T0 o0 T1 o1 … Tn on) = T0 T1 … Tn.	encode_offsets::stripping_deletes_offsets_and_nothing_else_over_every_row_in_the_process, encode_offsets::the_three_kinds_of_non_offset_digit_survive_by_name	&str → String; a for loop over chars() with a match on char ranges and guards; two mutable locals (usize depth with saturating_sub, bool); String::push	1, 3, 5, 7
method-encoding-concat	equivalence	swizzle::MethodFn::method_encoding (impl_method_fn! arities 0..=14), method_encoding!	∀ Ret: Encode, A1..An: Encode, 0 ≤ n ≤ 14: <unsafe extern "C-unwind" fn(Id, Sel, A1..An) -> Ret as MethodFn>::method_encoding() = method_encoding!(Ret; A1..An) = MEnc(Ret; A1..An). Hence two prototypes encode equally iff their per-position Enc strings concatenate equally: const and mut void* both give ^v, while an extra argument, a narrower return or a struct by value differ.	swizzle::method_fn_derives_the_encoding_from_the_prototype, swizzle::the_encoding_check_is_blind_to_a_disagreement_that_does_not_move_a_register, adversary_w2c::a_context_pointer_and_a_selector_no_longer_encode_the_same, adversary_w2d::an_nsrange_crosses_the_boundary_by_value_and_by_pointer	generic trait impls over a macro-expanded arity family; associated consts of type &'static str; String::push_str in straight-line code	1, 3, 11
encode-table	table-property	encode::Encode impls, swizzle::Imp, block::BlockPtr	Enc is exactly: ()=v; i8=c; u8=C; i16=s; u16=S; i32=i; u32=I; i64,isize=q; u64,usize=Q; f32=f; f64=d; Bool=BOOLENC(Arch); Id=@; ClassPtr=#; ProtocolPtr=@; Sel=:; *mut c_void, *const c_void=^v; *const c_char=*; *mut c_uchar=*; *mut Bool=BOOLPTR(Arch); CGPoint={CGPoint=dd}; CGSize={CGSize=dd}; CGRect={CGRect={CGPoint=dd}{CGSize=dd}}; NSRange={_NSRange=QQ}; *mut NSRange, *const NSRange=^{_NSRange=QQ}; BlockPtr=@?; Imp=^?. UA(T) = false for every row. Layout: Id, ClassPtr, Sel and Imp have the size and align of *const c_void; size_of Option<Imp> = size_of Imp; NSRange is 16 bytes, align 8.	adversary_w2d::each_pointer_shaped_runtime_type_carries_its_own_encoding, adversary_w2c::a_context_pointer_and_a_selector_no_longer_encode_the_same, swizzle::imp_carries_the_encoding_clang_emits, adversary_w2b::a_bool_argument_is_never_materialised_as_a_rust_bool, adversary_w2d::an_nsrange_crosses_the_boundary_by_value_and_by_pointer	associated consts on trait impls, cfg(target_arch) arms, size_of/align_of (decidable by const evaluation once &str consts are carriers)	1, 11
encode-matches-platform	refinement	encode::Encode impls, block::BlockPtr	∀ Encode impl T in the crate, ∀ Arch: Enc(T) = P.@encode(T, Arch), up to the optional const qualifier r. Witnessed through the runtime: the return letter of -[NSObject isEqual:/respondsToSelector:/isProxy] = Enc(Bool); -[NSValue rectValue] starts with Enc(CGRect); argument 3 of -[NSFileManager fileExistsAtPath:isDirectory:] = Enc(*mut Bool); strip(-[NSString enumerateLinesUsingBlock:]) = "v@:" ++ Enc(BlockPtr); NSMethodSignature reads a declared BOOL return as it reads its own.	abi::bool_encoding_matches_what_foundation_itself_emits, abi::the_struct_encodings_match_what_foundation_itself_emits, adversary_w2d::bool_ptr_encodes_what_foundation_compiled_for_its_bool_out_parameter, adversary_w2::nsmethodsignature_agrees_with_foundation_on_what_bool_is, winit_seam::the_block_encoding_is_what_foundation_itself_emits	FFI reads of runtime method tables (class_getInstanceMethod, method_getTypeEncoding, method_copyArgumentType), CStr, String. An axiom about P, checked by these tests as its conformance witness	8
returns-indirectly-and-entry	contract	runtime::returns_indirectly, runtime::msg, runtime::msg_super	∀ R: Encode: returns_indirectly::<R>() = (Arch = x86_64 ∧ (size_of::<R>() > 16 ∨ UA(R))). ∀ F: MsgFn returning R: msg::<F>() as usize = addr(objc_msgSend_stret) if returns_indirectly::<R>(), else addr(objc_msgSend); msg_super likewise with the Super entry points. On aarch64 every return type resolves to one address. Obligation on every unsafe impl Encode (a requires on the impl, author-supplied, not computable from size_of/align_of): UA(T) ⇔ some field anywhere in T's layout tree sits at an offset that is not a multiple of its own alignment; so a wrapper inherits it and [T;1] of a misaligning T misaligns.	abi::the_unaligned_rule_is_transitive_and_a_wrapper_proves_it, abi::the_indirect_return_rule_matches_the_c_abi, abi::the_entry_point_is_chosen_by_the_return_type, adversary_w2::a_thirty_two_byte_send_and_a_sixteen_byte_send_agree_with_the_c_abi, adversary_w2::sixteen_and_seventeen_byte_returns_both_round_trip	const generic fn over size_of plus an associated const; a const-if choosing between two extern symbol addresses; transmute_copy of a fn pointer	3, 8, 11
bool-semantics	round-trip	encode::Bool::{new,as_bool}, From<bool> for Bool, From<Bool> for bool, Debug for Bool	∀ b: bool: Bool::new(b).as_bool() = b ∧ bool::from(Bool::from(b)) = b. Bool::YES.as_bool() ∧ ¬Bool::NO.as_bool(); Bool::default() = Bool::NO. On x86_64, ∀ x: i8: Bool(x).as_bool() = (x ≠ 0), so no bit pattern is invalid. size_of::<Bool>() = align_of::<Bool>() = 1. format!("{:?}", v) = "YES" if v.as_bool(), else "NO".	abi::bool_encoding_matches_what_foundation_itself_emits, adversary_w2b::a_bool_argument_is_never_materialised_as_a_rust_bool	repr(transparent) newtype over bool/i8, const fn, cfg(target_arch) arms, a Debug impl writing to a Formatter (drop the Debug clause or have it delegate to a pure fn if Formatter is not modelled)	1
inert-zero	contract	exception::inert, exception::InertZero impls	∀ R: InertZero: inert::<R>() is the all-zero bit pattern of R and a valid R: 0 for integers, +0.0 for floats, null for Id/ClassPtr/ProtocolPtr/Sel/raw pointers/BlockPtr, Bool::NO, NSRange{0,0}, CGPoint/CGSize/CGRect of zeros, None for Option<NonNull<T>>. Obligation on every unsafe impl InertZero: all-zero bytes are a valid Self.	src/exception.rs::inert_is_the_zero_value	generic fn using MaybeUninit::zeroed().assume_init() (unsafe), a marker trait, a niche-optimised Option<NonNull>	8, 11
objc-try-protocol	contract	exception::{objc_try,run_body,run_on_exc}, Drop for Slot	∀ body: FnOnce() -> R, handler: FnOnce(Id): objc_try(body, handler) has exactly one of three outcomes. (1) body returns v without raising: Ok(v); handler is dropped exactly once, never run; CONTAINED unchanged. (2) body panics: the panic propagates unchanged; handler is never run and dropped exactly once; every Drop in body's frames runs. (3) body raises an Objective-C exception e: Err(Contained{ordinal: 0}); handler runs exactly once with e, between objc_begin_catch and objc_end_catch; body's frames are dropped; out is never read. In every case body and handler are each dropped exactly once.	src/exception.rs::a_body_that_returns_is_ok_and_counts_nothing, src/exception.rs::drops_in_the_body_run_on_the_normal_path, src/exception.rs::a_rust_panic_passes_through_the_wrapper_to_catch_unwind, src/exception.rs::the_unrun_handler_is_dropped_on_both_the_ok_and_the_panic_path	global_asm @try wrapper, foreign unwinding through extern "C-unwind", ManuallyDrop/MaybeUninit, a raw pointer to a stack slot, closures, a Drop impl with a liveness flag	2, 8, 13
containment-trampoline	contract	declare_class! trampoline (__tramp), block::RcBlock::newN invoke, exception::{contain,report}, dispose_helper	∀ declared row (sel_i, fn_i, R_i: InertZero) with abort_i = contain, sent with its exact prototype, and ∀ RcBlock invoke returning R: (1) body raises NSException e ⇒ the send returns inert::<R>(); every Rust Drop on the unwound frames ran; CONTAINED grows by exactly 1 to k; Sink is called exactly once with method = sel_i's name (or "block invoke"), e's class/name/reason/call_stack, and ordinal = k; the receiver stays usable (a later send gets its normal answer, also when Foundation dispatched it via performSelector:). (2) body panics ⇒ the process aborts via abort_on_unwind(fn name) with no containment report. (3) abort_i = abort_on_exception, or the raise is inside -dealloc (an ivar Drop) ⇒ the process aborts. (4) a block capture's Drop panics in dispose ⇒ abort via abort_on_unwind("block dispose").	exception_containment::a_raise_inside_a_declared_method_is_contained_and_every_drop_runs, exception_containment::the_inert_answer_is_the_selectors_nothing_happened_value, exception_containment::a_contained_method_dispatched_by_foundation_still_returns_to_foundation, exception_containment::a_block_contains_a_raise_and_answers_its_inert_zero, exception_containment::a_rust_panic_inside_a_contained_method_still_aborts_with_the_named_message, exception_containment::an_abort_on_exception_method_still_aborts_on_a_raise, exception_containment::dealloc_keeps_the_abort_on_a_raise, adversary_w2::a_panic_inside_a_declared_method_aborts_instead_of_unwinding_into_foundation, adversary_w2c::a_panicking_drop_in_a_block_capture_aborts_by_name	macro-generated extern "C" trampolines, catch_unwind outside and @try inside, an atomic counter, a fn-pointer sink, process abort	1, 8, 13
abort-on-unwind-never-unwinds	no-panic	declare::abort_on_unwind	∀ method: &str, ∀ state of fd 2 (open, closed, a pipe with no reader): abort_on_unwind(method) neither panics nor unwinds, allocates nothing, and ends in process::abort() (SIGABRT). If stderr accepts writes, the bytes written are exactly "aterm-objc: panic escaped Objective-C method `" ++ method ++ "`; aborting\n". Every write error is ignored.	adversary_w2d::the_unwind_guard_does_not_itself_panic_when_stderr_is_gone, adversary_w2::a_panic_inside_a_declared_method_aborts_instead_of_unwinding_into_foundation, adversary_w2c::a_panicking_drop_in_a_block_capture_aborts_by_name	io::stderr().lock() with write_all results discarded, process::abort (diverging)	6, 8
containment-report-format	contract	exception::default_sink, exception::describe (stack formatting, to be factored pure), exception::report	∀ ContainedException e: default_sink(e) writes exactly one line to stderr: "aterm-objc: contained " ++ name ++ " (" ++ class ++ ") in Objective-C method `" ++ method ++ "`: " ++ reason ++ " | stack: " ++ call_stack ++ "\n". From frames f_0..f_{n-1}, call_stack is the first min(n, 48) frames in order, each with every run of spaces collapsed to one, joined by " | ", plus the suffix " | …" iff n > 48. One report call makes exactly one sink call, also when describing raises a second exception (then the fields are the fixed (unknown) strings and the containment still counts). A nil exception gives the (nil) strings.	exception_containment::the_default_sink_writes_one_line_naming_method_and_exception, exception_containment::a_raise_inside_a_declared_method_is_contained_and_every_drop_runs	String building in a loop over chars with a prev_space flag, min, a nested objc_try, stderr writes	1, 7, 8
wake-plan-refines-model	refinement	wake_timer::WakePlan::{new,start,start_at,stop,fired,arm,armed,spent}	WakePlan is a deterministic machine over (armed: Option<WakeTarget>, spent: bool), starting at (None, false). stop(): armed = None ⇒ Keep, state unchanged; else Disarm and (None, false). arm(t): ¬spent ∧ armed = Some(t) ⇒ Keep, state unchanged; else state := (Some(t), false) and return Now if t = Now, At(at) if t = At(at). start() = arm(Now). start_at(now, None) = stop(). start_at(now, Some(at)) = start() if now ≥ at, else arm(At(at)). fired(): spent := spent ∨ armed.is_some(). The projection cache = {None:0, Now:1, At:2}[armed], spent, want = the last request kind, armed_real = (armed.is_some() ∧ ¬spent) maps every WakePlan transition onto exactly one Model action, the Model's successor equals the projected new state, and NoLostWake holds in every reachable state. Negative control: a plan with fired() deleted reaches a state NoLostWake rejects.	src/wake_timer.rs::a_spent_timer_is_re_armed_for_an_unchanged_request, conformance_wake_timer::a_plan_deaf_to_the_callout_is_caught_as_a_lost_wake	&mut self methods, enums with a payload (Instant, opaque Ord), Option, match with guards; ty_model! conformance with a negative control	1, 2, 10
cf-catch-up-arithmetic	contract	wake_timer::{cf_catch_up_steps,interval_ticks}	∀ lag, i: u64: cf_catch_up_steps(lag, 0) = None, and i > 0 ⇒ cf_catch_up_steps(lag, i) = Some(lag / i + 1), which requires lag / i < u64::MAX (today lag = u64::MAX, i = 1 overflows; §5). ∀ secs, rate: f64 with t = secs · rate: interval_ticks = 0 if t is NaN or t ≤ 0; u64::MAX if t ≥ u64::MAX as f64; else trunc(t). Bound: ∀ rate ≥ 1, ∀ lag < interval_ticks(WAKE_TIMER_REPEAT_SECS, rate): cf_catch_up_steps(lag, interval_ticks(WAKE_TIMER_REPEAT_SECS, rate)) = Some(1), so any lateness under a year costs one step.	src/wake_timer.rs::a_week_of_lateness_costs_the_waker_one_catch_up_step	u64 division and addition (overflow check), f64 multiply, NaN and comparison, an f64-to-u64 cast. The u64 half is scalar; the f64 half needs a float model	3, 5
wake-timer-cf-refinement	refinement	wake_timer::WakeTimer::{current_thread,start,start_at,stop,plan,apply,fires_within,next_fire_date,armed_date}	∀ request sequence on a WakeTimer interleaved with CoreFoundation fires, projecting (plan cache, plan spent, fires_within(1 day), want, delivered) onto Model refines it step for step. Every armed timer CF fires is reflected as spent through the callout flag. After a fire L seconds late with L < WAKE_TIMER_REPEAT_SECS, the next fire date is armed_date + 1 · interval (steps = 1 = the Model's). Negative controls: a raw 0.1 µs repeating timer gives steps ≈ L / 1e-7, which the shipped Model rejects and the tiny-interval mutant predicts; a plan deaf to the callout is rejected by NoLostWake.	conformance_wake_timer::the_real_wake_timer_refines_the_run_loop_waker_model, conformance_wake_timer::the_historical_interval_is_caught_by_the_same_projection, conformance_wake_timer::a_plan_deaf_to_the_callout_is_caught_as_a_lost_wake	CFRunLoopTimer FFI, an Rc<Cell<bool>> written from an extern "C" callout, f64 absolute time, Drop invalidating the timer	8, 10
main-thread-bound	contract	main_thread_bound::MainThreadBound::{new,get,get_mut,into_inner,get_on_main}, Drop, Debug	∀ T, v: T, witness mt: get(new(v, mt), mt) borrows v; a write through get_mut is seen by the next get; into_inner(new(v, mt), mt) returns v, runs no destructor of T, and dispatches nothing; moving the container between threads changes nothing in v (e.g. Rc::strong_count). Drop: ¬needs_drop::<T>() ⇒ returns without touching the main queue on any thread; else T's destructor runs exactly once, on the process main thread (in place when dropped there, through run_on_main when dropped elsewhere, including a Retained of a declared class whose -dealloc runs its ivar Drop). format!("{:?}", b) = "MainThreadBound { .. }". Layout: size_of/align_of = T's, which needs #[repr(transparent)] on MainThreadBound (absent today; §5).	main_thread_bound::the_container_costs_nothing_over_the_value, main_thread_bound::get_and_get_mut_reach_the_value_through_a_witness, main_thread_bound::a_container_of_a_drop_less_value_never_reaches_the_main_queue, main_thread_bound::into_inner_hands_the_value_back_and_does_not_run_the_containers_drop, main_thread_bound::the_value_crosses_threads_inside_the_container_and_comes_back_intact, main_thread_bound::debug_is_opaque, main_thread_bound_drive::main_thread_bound_drive	generic container over ManuallyDrop, unsafe impl Send/Sync, needs_drop, dispatch_sync_f to the main queue, &mut self in Drop, closures	2, 8, 11, 13
main-thread-witness	contract	declare::MainThread::new	MainThread::new().is_some() ⇔ +[NSThread isMainThread] answers YES on the calling thread ⇔ the caller is the process main thread. None on every libtest worker and spawned thread, Some on the harness=false main. Never panics.	declared_class::the_main_thread_witness_refuses_a_worker_and_a_spawned_thread, main_thread_bound_drive::main_thread_bound_drive	an AtomicPtr class cache, an FFI send returning Bool	8
sel-cache-coherent	invariant	sel_cache::SelCache::{get,fill}, sel!, runtime::sel, runtime::Sel::name, Debug for Sel	Assume P: sel_registerName(n) is a pure function of n. Then ∀ n, ∀ interleaving of any number of threads calling SelCache::get(n) on one slot: every call returns sel(n). sel!(n) = sel_uncached(n). sel(n).name() = n, also for a selector read back from a class's method table. Sel::NULL.name() = "(null)". Debug of a non-null Sel is "Sel(" ++ name ++ ")", and "Sel()" for the empty name.	runtime::cached_selectors_are_the_uncached_ones, runtime::a_cache_slot_survives_concurrent_first_use, adversary_w2::the_sel_cache_is_correct_under_a_hard_concurrent_first_use, declared_class::an_enumerated_selector_names_itself, adversary_w2c::a_context_pointer_and_a_selector_no_longer_encode_the_same	AtomicPtr Relaxed load/store, racy fill by value, CStr, FFI	8, 13
class-lookup	contract	runtime::{class,class_name,class_of,superclass_of}	∀ n: class(n) ≠ null ⇔ a loaded image registers class n, and then class_name(class(n)) = n. ∀ live o: class_of(o), superclass_of(…), … reaches null after finitely many steps; the last non-null class is a root class (NSObject for Foundation objects); each class on the chain names a real class; a class-clustered NSString's chain contains NSString.	runtime::classes_resolve_and_unknown_names_are_nil, runtime::class_of_and_superclass_of_walk_the_chain	thin FFI wrappers, CStr	8
string-bridge-round-trip	round-trip	runtime::{ns_string,ns_string_to_rust,ns_error_string}, cf::CFString::to_string	∀ s: &str: ns_string(s) = Some(o), -length of o = s.encode_utf16().count(), and ns_string_to_rust(o) = s, including U+0000 and a leading U+FEFF. ∀ u16 sequence u and an NSString/CFString built from u: ns_string_to_rust / CFString::to_string = String::from_utf16_lossy(u) (each unpaired surrogate becomes U+FFFD, the rest is exact). ns_string_to_rust(nil) = "". ns_error_string(nil) = "(nil NSError)". ∀ live NSError e: ns_error_string(e) = ns_string_to_rust(-localizedDescription(e)), or "(no localizedDescription)" when that is nil.	runtime::ns_string_round_trips_every_rust_string_exactly, runtime::an_unpaired_surrogate_reads_as_a_replacement_character, runtime::ns_string_length_is_utf16_code_units, runtime::an_error_description_is_read_out_of_a_real_nserror, runtime::utf8_string_of_a_nil_receiver_is_empty, cf_runtime::the_string_owner_reads_out_the_whole_string	encode_utf16 into Vec<u16>, Vec::with_capacity plus set_len after an FFI fill, from_utf16_lossy, autoreleasepool closure	1, 8
ownership-balance	invariant	runtime::Obj::{from_owned,retain,clone_retained,autorelease,drop}, retained::Retained, cf::CFUUID::{from_create,clone,bytes,drop}, declare_class! object-returning trampoline	Every live Obj, Retained or CF owner holds exactly one +1 on a non-null object. from_owned(nil) = retain(nil) = None (likewise CFUUID::from_create(null)). clone_retained/clone returns the same pointer with RC + 1. Drop does RC − 1 exactly once. autorelease leaves RC unchanged until the innermost pool pops, then RC − 1, so the object is alive and messageable for the rest of the scope; a caller that retains inside the pool keeps it alive past the pop. A declared method returning an object not named new/alloc/copy returns +0: alive until its pool pops and released exactly once there; a +1 (into_raw) return is not released by the pool. CFUUID::bytes() = the 16 creation bytes in CFUUIDBytes order. CFUUID: Send + Sync.	runtime::owned_and_retained_balance_exactly, runtime::from_owned_and_retain_reject_nil, runtime::an_autoreleased_object_lives_exactly_as_long_as_its_pool, cf_runtime::the_uuid_owner_reads_its_bytes_and_balances_its_retains, declared_class::an_extra_retain_defers_the_drop, declared_class::an_object_returning_method_hands_back_a_plus_zero_reference, adversary_w2::an_autoreleased_return_is_at_plus_one_owned_by_the_pool, adversary_w2::a_caller_may_retain_an_autoreleased_return_across_the_pool	owning newtypes with Drop, mem::forget, FFI retain/release/autorelease, the pool stack	2, 8
autoreleasepool-scope	contract	runtime::autoreleasepool, runtime::AutoreleasePool::{new,drop}	∀ f: autoreleasepool(f) pushes exactly one pool p, runs f(&p), and pops p exactly once, both when f returns (the result is f's value) and when f unwinds. So Pools after the call = Pools before it, at any nesting depth. Objects autoreleased inside f stay alive until that pop. A pool pushed after an unwind pops cleanly.	pools::the_scope_form_nests_to_any_depth_and_keeps_objects_alive_to_its_end, pools::a_scope_pops_its_pool_even_when_the_body_unwinds, runtime::a_pool_scope_is_a_pure_nesting	RAII token in a frame, unwind drop, closures, FFI push/pop	8, 13
block-abi	contract	block::RcBlock::{new0,new1,new2,as_ptr,clone_retained,drop}, invoke0/1/2, dispose_helper	∀ closure F: Fn(A..) -> R + 'static with A: Encode, R: Encode + InertZero, align_of::<Blk<F>>() ≤ 16: newN(F) = Some(b), a heap block P's block runtime accepts. ∀ arguments a, invoked through its own invoke slot or by libdispatch/Foundation (dispatch_sync, dispatch_apply, enumerateLinesUsingBlock:, enumerateObjectsUsingBlock:): the invoke returns F(a) and receives arguments in order. Invoking never consumes or drops F. clone_retained returns the same pointer. F's captures are dropped exactly once, when the last reference (Rust's or a framework's) is released, even if the block is never invoked.	blocks::libdispatch_invokes_an_arity_zero_block, blocks::libdispatch_invokes_an_arity_one_block_with_its_argument, blocks::foundation_invokes_an_arity_two_block, blocks::a_returning_block_hands_its_value_back, blocks::the_dispose_helper_drops_the_closure_exactly_once, blocks::a_retained_block_defers_the_dispose, adversary_w2::a_block_capturing_a_drop_type_drops_it_exactly_once_when_the_block_dies, adversary_w2::a_block_that_is_never_invoked_still_drops_its_capture_once, adversary_w2::a_block_handed_to_a_real_framework_api_survives_and_is_released_once, adversary_w2c::a_block_may_return_a_type_that_has_no_default_impl, adversary_w2c::a_panicking_drop_in_a_block_capture_aborts_by_name	repr(C) block header, generic extern "C" invoke and dispose fns, _Block_copy/_Block_release FFI, closures capturing Drop types, a const align assert	8, 11, 13
declared-class-registration	refinement	declare_class!, declare::{begin,ClassBuilder::{add_rust_ivar,add_method,add_protocol,register}}, runtime::{class_methods,class_protocols,method_types}	∀ declared site D, once Self::class() has been called from any number of threads concurrently: exactly one class C exists; objc_getClass(NAME) = C; class_name(C) = NAME; superclass_of(C) = S; ClassType::NAME = NAME. class_methods(C) = {sel_i} ∪ {dealloc}, with no inherited rows. ∀ i: method_types(C, sel_i) = MEnc(R_i; A_i..), and dealloc = "v@:" (covering arity up to 14, BOOL as BOOLENC, Class as #, void* as ^v, BlockPtr as @?, NSRange by value and by pointer, 16/17/32-byte structs). class_methods agrees row for row with method_types. -respondsToSelector:(x) is YES for each sel_i and inherited rows, NO for undeclared selectors. class_protocols(C) ⊇ Ps with no protocol nobody added, and -conformsToProtocol:(p) is YES ∀ p ∈ Ps. A site with no methods and Ivars = () still registers, and its -description names NAME.	declared_class::the_class_is_registered_with_the_runtime, declared_class::an_instance_is_of_that_class_and_responds_to_its_selectors, declared_class::the_type_encodings_are_what_the_runtime_reports, declared_class::the_declared_protocol_is_registered, declared_class::the_whole_registered_table_comes_back_with_its_encodings, declared_class::the_conformance_claim_is_readable_and_is_the_classs_own, declared_class::an_enumerated_selector_names_itself, adversary_w2::the_runtime_reports_the_arity_four_five_and_six_encodings, adversary_w2b::a_class_with_no_methods_and_a_zero_sized_ivar_still_registers, adversary_w2b::a_ten_colon_selector_counts_and_dispatches, adversary_w2b::concurrent_first_registration_produces_one_class, adversary_w2c::a_fourteen_argument_method_is_sendable_from_rust_not_just_declarable, adversary_w2c::a_context_pointer_and_a_selector_no_longer_encode_the_same, adversary_w2d::each_pointer_shaped_runtime_type_carries_its_own_encoding, adversary_w2d::an_nsrange_crosses_the_boundary_by_value_and_by_pointer, stret_declared::the_registered_encodings_are_what_clang_emits_for_the_same_signatures, winit_seam::the_block_encoding_is_what_foundation_itself_emits	macro_rules expansion, OnceLock, FFI class-pair creation, String encodings, Vec of rows	1, 8, 11
declared-dispatch	refinement	declare_class! trampolines (__tramp), runtime::msg, runtime::msg_super, ivars()	∀ declared row i, arguments a: objc_msgSend through msg::<fn(Id, Sel, A_i..) -> R_i>() on an alloc_init instance returns fn_i(&self, a..), arguments in order and unshifted for every return class: register, HFA (CGRect on arm64), indirect (24-byte {qqq}, and > 16 bytes on x86_64), two-register (NSRange). A pointer argument such as *mut NSRange is written through. NSInvocation built from instanceMethodSignatureForSelector: gives the same result, with numberOfArguments = 2 + n, per-index types = Enc(A_k), methodReturnType = Enc(R_i), methodReturnLength = size_of::<R_i>(). An instance round-tripped as a bare id still reaches the same ivars(). A row whose body sends to super starts lookup at S.	declared_class::messages_reach_the_rust_bodies_through_objc_msg_send, declared_class::the_super_send_starts_above_this_class, declared_class::ivars_survive_a_round_trip_through_the_runtime, declared_class::two_and_three_argument_methods_reach_their_bodies_with_the_arguments_in_order, declared_class::a_thirty_two_byte_struct_comes_back_intact, adversary_w2::arity_four_five_and_six_reach_their_bodies_with_arguments_in_order, adversary_w2::sixteen_and_seventeen_byte_returns_both_round_trip, adversary_w2::nsinvocation_reads_our_arity_and_types_and_invokes_correctly, adversary_w2::nsmethodsignature_agrees_with_foundation_on_what_bool_is, adversary_w2b::a_ten_colon_selector_counts_and_dispatches, adversary_w2c::a_fourteen_argument_method_is_sendable_from_rust_not_just_declarable, adversary_w2d::each_pointer_shaped_runtime_type_carries_its_own_encoding, adversary_w2d::an_nsrange_crosses_the_boundary_by_value_and_by_pointer, stret_declared::foundation_computes_the_return_length_from_the_registered_string, stret_declared::a_direct_send_reaches_each_return_class_with_its_arguments_intact, stret_declared::nsinvocation_drives_every_return_class_through_the_registered_encoding	extern "C" trampolines with up to 16 parameters, struct-by-value ABI, transmuted fn pointers, exposed-provenance ivar pointers	8, 11
ivar-slot-contract	contract	declare::IvarSlot::{init,get,dispose}, declare::ClassBuilder::add_rust_ivar, declare_class! ivars()/__dealloc/alloc_init	∀ T with align_of::<IvarSlot<T>>() ≤ 16 (else the const assert rejects it at compile time), ∀ aligned slot whose bytes are zero: get(slot) panics with "ivars read before they were written". After init(slot, v), get(slot) = &v. dispose(slot) drops v exactly once, clears the flag first, and is idempotent. ∀ instance from class_createInstance, base + ivar_offset is a multiple of align_of::<IvarSlot<T>>() (the base is 16-aligned and the offset is a multiple of the alignment). So -dealloc drops the Rust ivars exactly once, and an instance Objective-C minted with -init aborts by name on its first ivar read. Undischarged precondition of the safe ivars() (§5): the receiver is an instance of the declaring class or a subclass, so no substituting initializer can supply a survivor of another class.	declared_class::dealloc_drops_the_rust_ivars_exactly_once, adversary_w2b::a_sixteen_byte_aligned_ivar_lands_aligned_and_drops_once, adversary_w2b::an_instance_objective_c_minted_is_caught_by_the_ivar_flag, adversary_w2d::an_ivar_at_the_sixteen_byte_ceiling_is_aligned_in_every_instance, winit_seam::a_substituting_initializer_defeats_the_initialized_flag	repr(C) generic struct with MaybeUninit, raw *mut Self, drop_in_place, a const assert, an assert that panics	1, 2, 8, 11
swizzle-install-decision	table-property	swizzle::SwizzleSite::{install,original,original_imp,installed_row}	install(cls, sel, new) on a site with claim c (None, or Some{original, owner, sel}) returns the first matching row: cls null ⇒ Err(NullClass); owning_class = None ⇒ Err(NoSuchMethod); c = Some ∧ (owner, sel) ≠ c's row ⇒ Err(WrongRow{claimed, asked}); no registered encoding ⇒ Err(NoEncoding); strip(registered) ≠ F::method_encoding() ⇒ Err(EncodingMismatch{registered, expected}); current IMP = new.imp() ⇒ Ok(AlreadyInstalled{owner, previous: c.original}) if c = Some, else Err(OrphanedInstall); c = Some ⇒ Err(Displaced{expected: new, found: current}); the claim CAS loses ⇒ Err(Contended); otherwise the IMP is swapped and the result is Ok(Replaced{owner, previous: current}) if the displaced IMP = current, else Err(Raced) with the claim repaired to the displaced IMP. Invariants over all concurrent histories: (I1) every Err except Raced leaves the method table and the claim unchanged; (I2) the claim is written at most once, only by the CAS winner, so exactly one of N racing installers gets Replaced; (I3) claim.original is never an IMP installed through this site, so the chain never calls itself; (I4) a KVO subclass of the claimed row's class resolves to the claimed row (AlreadyInstalled).	swizzle::install_refuses_a_prototype_the_registered_encoding_does_not_match, swizzle::install_refuses_a_selector_nothing_implements_and_a_null_class, swizzle::install_replaces_publishes_the_original_and_reports_the_owner, swizzle::install_refuses_once_a_third_party_has_displaced_it, swizzle::install_refuses_when_the_row_is_already_patched_by_somebody_else, swizzle::concurrent_installs_into_one_site_never_build_a_chain_that_calls_itself, swizzle::one_site_asked_about_a_second_row_refuses_instead_of_answering, swizzle::a_kvo_isa_swap_leaves_the_swizzle_and_its_owner_where_they_were	&'static self with an AtomicPtr<Claim> compare_exchange, Box::into_raw leak, enums with String payloads, FFI method_setImplementation. Factor the decision table into a pure fn over (claim, owner, registered, expected, current, new) first; that half needs only gap 1	1, 2, 8, 13
owning-class	contract	swizzle::owning_class	owning_class(cls, sel) = None if cls is null or class_getInstanceMethod(cls, sel) is null. Otherwise, with m that method and C0 = cls, C1 = super(C0), … the superclass chain: the result is Some(Ck) for the largest k with class_getInstanceMethod(Cj, sel) = m ∀ j ≤ k. The loop terminates because the chain is finite (P). So it reports the definer of an inherited row, and for a KVO NSKVONotifying_X subclass with no override it reports X's definer.	swizzle::owning_class_finds_the_definer_not_the_class_asked, swizzle::a_kvo_isa_swap_leaves_the_swizzle_and_its_owner_where_they_were, swizzle::a_swizzle_through_a_subclass_mutates_the_ancestors_row	a loop over FFI calls with pointer equality	7, 8
platform-runtime-axioms	other (platform axioms)	PlatformSpec P: libobjc, Foundation, CoreFoundation, libdispatch, AppKit	Facts the crate's SAFETY comments rely on, stated as Trust FFI assumptions with these tests kept as their conformance witness and negative control (checkable, not provable): method_setImplementation keeps the type encoding byte-identical, returns the displaced IMP and affects only that Method. class_replaceMethod on an existing row ignores types and swaps; on an inherited row it adds to the subclass and returns NULL. class_addMethod refuses an existing row. A patch made through a subclass that does not override mutates the ancestor's row. An out-of-order pool pop aborts with SIGABRT and the runtime's pool diagnostic. Autorelease with no pool leaks silently (it warns only under OBJC_DEBUG_MISSING_POOLS=YES). CFRunLoopPerformBlock takes its own block reference. A heap block's class is an NSBlock kind. A nil send returns zero for register-class returns, and a nil objc_msgSend_stret leaves its buffer untouched (x86_64). Weak table: objc_initWeak registers the address, dealloc writes nil through every registered address, objc_destroyWeak unregisters, objc_moveWeak relocates, and a memcpy does not. The CF run-loop source and observer callouts fire through the declared types. NSBeep is an exported C symbol. NSStandardKeyBindingResponding and NSDraggingDestination rows have their measured encodings.	swizzle::method_set_implementation_leaves_the_encoding_byte_identical, swizzle::class_replace_method_ignores_a_lying_types_on_a_row_the_class_already_has, swizzle::class_add_method_refuses_a_row_the_class_already_has, swizzle::a_swizzle_through_a_subclass_mutates_the_ancestors_row, swizzle::class_replace_method_on_an_inherited_row_adds_and_returns_null, swizzle::the_swizzle_is_confined_to_the_class_pair_it_was_applied_to, pools::an_out_of_order_pop_is_fatal_which_is_why_new_is_unsafe, adversary_w2c::autoreleasing_with_no_pool_leaks_and_that_is_all_it_does, blocks::cfrunloopperformblock_takes_its_own_reference, blocks::the_runtime_agrees_a_block_is_an_objc_object, runtime::libobjc_leaves_a_nil_stret_return_untouched_on_x86_64, runtime::utf8_string_of_a_nil_receiver_is_empty, weak::a_memcpy_moved_weak_slot_reads_a_dangling_pointer_and_the_runtime_nils_the_old_address, weak::objc_move_weak_is_what_a_move_would_have_to_be, weak::destroy_removes_the_registration_and_the_runtime_leaves_the_address_alone, weak::the_wild_write_a_missing_destroy_performs, gui_sent_prototypes::nsbeep_is_a_c_function_not_a_message, winit_seam::the_key_binding_family_is_measured_not_asserted, winit_seam::the_other_two_dragging_rows_are_genuinely_bool, cf_runtime::a_source_and_an_observer_run_through_the_declared_callouts	FFI only; child-process re-exec and signals for the fatal ones. These tests are not deleted when the axioms land: they become the axioms' conformance witnesses	8
weak-obj-model	refinement	weak::WeakObj::{new,from_obj,empty,load,store,clone_weak,is_live,slot_addr,peek,drop}, weak::WeakSlot::{load_retained,load_autoreleased}	Under P's weak-table axioms, WeakObj refines the model w = (addr: a fixed address, target: Option<obj>). slot_addr() is invariant under every Rust move of the handle (Vec growth, swap_remove, by-value round trips). load() = Some(Obj at RC + 1) iff target is Some and Alive, else None; the +1 keeps the target alive past its other owners. load_autoreleased answers the same at +0 inside the open pool. store(o) repoints the same addr (the old target's dealloc no longer affects it); store(nil) empties it. clone_weak registers a second, distinct addr that is nil'd independently. Creating weak references never changes RC. Drop calls objc_destroyWeak before the box is freed, so the runtime never writes through a released address. empty() loads None and can be filled.	weak::a_boxed_weak_survives_every_move_rust_can_perform, weak::the_slot_the_runtime_nils_is_the_boxed_one, weak::load_hands_back_a_plus_one_that_outlives_the_original_owner, weak::the_raw_plus_zero_load_answers_inside_the_pool_that_is_open, weak::store_repoints_the_same_slot_at_a_new_target, weak::clone_weak_registers_a_second_address_and_both_are_nild, weak::an_empty_weak_is_legal_and_can_be_filled, weak::a_weak_reference_does_not_retain, weak::dropping_the_handle_unregisters_before_the_box_is_freed	Box-pinned UnsafeCell slot, &mut self store, Drop, FFI objc_*Weak	2, 8, 10
protocol-or-register	contract	runtime::{protocol_or_register,protocols_registered_by_aterm,protocol}, declare_class! protocols:	∀ n, ∀ concurrent history of calls: protocol_or_register(n) returns protocol(n), which is non-null afterwards. n ∈ protocols_registered_by_aterm() iff no loaded image registered n before the first call, and then exactly once. Repeated calls return the same object. A class whose protocols: list names n answers -conformsToProtocol:(n) = YES, and class_protocols lists n. For a host-registered protocol (e.g. NSObject) the result is the host's object and the registry does not contain n. For NSApplicationDelegate exactly one of host and registry supplies it.	protocol_registration::claiming_a_protocol_no_image_carries_registers_it_and_the_claim_is_true, protocol_registration::a_second_claim_of_the_same_absent_protocol_shares_one_registration, protocol_registration::a_protocol_the_host_registers_is_never_re_registered, protocol_registration::the_application_delegate_claim_builds_on_this_host_and_is_true	a static Mutex<Vec<&CStr>> with double-checked lookup, FFI objc_allocateProtocol/registerProtocol, an assert	1, 8, 13
ffi-decls-match-sdk	refinement	cf (all extern fns, statics, types, repr(C) structs, consts), cg	∀ Arch ∈ {arm64, x86_64}: every declaration in cf.rs and cg.rs matches the SDK header. Each extern fn is type-compatible with the SDK prototype (integer width and signedness, pointer vs value, struct tag, const on callback pointees); each pub type alias is compatible with its typedef; each repr(C) struct has the SDK struct's size, alignment and per-field offset and type; each const equals the SDK value; the declared callback types (the C-unwind CFRunLoopObserverCallBack, the source perform fn) are what CF calls. Negative control: changing one prototype's return type (CFArrayGetCount → int8_t) is rejected on both arches.	cf_cg_sdk::every_cf_and_cg_declaration_matches_the_sdk_on_both_arches, cf_cg_sdk::a_wrong_prototype_fails_the_probe_on_both_arches, cf_runtime::a_source_and_an_observer_run_through_the_declared_callouts	extern blocks, repr(C) structs, cfg(target_arch) aliases; checked today by a generated clang _Static_assert probe. Wants a declaration-level FFI check in Trust's platform lane	8
sdk-constants-equal	table-property	aterm-gui appkit.rs consts, aterm-window platform_impl/macos/aterm_objc_seam.rs consts	∀ const K in aterm-gui's appkit consts module and in aterm-window's seam consts module, ∀ Arch ∈ {arm64, x86_64}: value_Arch(K) = the SDK's value of the spelling K claims, with a compatible type, where value_Arch(K) is the cfg(target_arch) arm selected for Arch and |-composed literals are compared whole. Negative controls: inverting NS_COLOR_RENDERING_INTENT_PERCEPTUAL, NS_EVENT_SUBTYPE_WINDOW_EXPOSED, or either arm of NS_TEXT_ALIGNMENT_CENTER fails on its arch.	gui_appkit_constants::every_gui_constant_equals_the_sdk_on_both_arches, gui_appkit_constants::the_assertion_is_load_bearing_on_both_arches, gui_appkit_constants::the_arch_split_row_is_checked_on_the_arch_it_belongs_to, winit_seam_constants::every_seam_constant_equals_the_sdk_on_both_arches, winit_seam_constants::the_assertion_is_load_bearing_on_both_arches	integer consts with cfg arms in other crates; today a source-text parser plus clang	8, 12
send-site-abi	refinement	send::send_* helpers, aterm-gui and aterm-window send call sites	∀ send site in aterm-gui and aterm-window's macOS backend, with typed helper send_X (or a msg() cast with prototype F), receiver class C (class or instance side, or protocol) and selector s: strip(P.encoding(C, s)) = MEnc of the helper's prototype, under the token map of tests/support/appkit.rs (bool → BOOLENC, f32 → f, i16 → s, ptr → ^v, idptr → r^@, …). Best stated as a requires on each send helper call: method_types(class_of(recv), sel) agrees with the helper's prototype. Coverage of every site then follows from the contract, with no hand table.	gui_sent_prototypes::every_sent_selector_encodes_the_way_its_helper_spells_it, winit_sent_prototypes::every_sent_selector_encodes_the_way_its_helper_spells_it, winit_sent_prototypes::the_undocumented_cursor_accessors_encode_the_way_the_fork_sends_them, winit_sent_prototypes::the_protocol_row_encodes_the_way_its_helper_spells_it	FFI method tables, String encodings, cross-crate call-site contracts	1, 8, 12
declared-row-matches-authority	refinement	aterm-window declare_class! sites (71 rows), aterm-window app.rs SendEvent swizzle	∀ aterm-window declared row r (selector s, Rust signature fn(&self, A..) -> R) with a runtime authority (a protocol method description or the superclass's method): strip(authority(s)) = MEnc(R; A..). No row takes or returns a block (@?) or a function pointer (^?). For the swizzled -[NSApplication sendEvent:]: strip(method_types(NSApplication, sendEvent:)) = <SendEvent as MethodFn>::method_encoding() = "v@:@". draggingEntered: is Q@:@, while prepare/performDragOperation: are BOOLENC@:@.	winit_seam::no_declared_row_disagrees_with_the_runtimes_own_authority, winit_seam::the_ported_signatures_encode_to_the_authority, winit_seam::no_declared_winit_method_takes_a_block, winit_seam::the_swizzled_row_still_encodes_the_way_app_rs_types_it, winit_seam::the_other_two_dragging_rows_are_genuinely_bool	cross-crate declare_class! sites, protocol_getMethodDescription FFI, String	8, 10, 12
send-rect-nil	contract	send::send_rect	∀ sel: send_rect(Id::NIL, sel) = CGRect::default() (all four f64 fields +0.0) without sending, whatever the caller's stack held. For a non-null recv it returns the prototype-exact send's result. The same nil guard is owed by every helper whose return is indirect on some Arch (returns_indirectly).	runtime::a_nil_receiver_returns_a_zero_rect_on_this_arch	an early return on a null check, then an FFI send; a struct return. The nil branch alone is pure (gaps 1, 3); the non-nil branch needs 8	1, 3, 8
```

### Distribution by kind (35)

| kind | count | ids |
|---|---|---|
| contract | 15 | returns-indirectly-and-entry, inert-zero, objc-try-protocol, containment-trampoline, containment-report-format, cf-catch-up-arithmetic, main-thread-bound, main-thread-witness, class-lookup, autoreleasepool-scope, block-abi, ivar-slot-contract, owning-class, protocol-or-register, send-rect-nil |
| refinement | 9 | encode-matches-platform, wake-plan-refines-model, wake-timer-cf-refinement, declared-class-registration, declared-dispatch, weak-obj-model, ffi-decls-match-sdk, send-site-abi, declared-row-matches-authority |
| table-property | 3 | encode-table, swizzle-install-decision, sdk-constants-equal |
| equivalence | 2 | strip-offsets-filter, method-encoding-concat |
| round-trip | 2 | bool-semantics, string-bridge-round-trip |
| invariant | 2 | sel-cache-coherent, ownership-balance |
| no-panic | 1 | abort-on-unwind-never-unwinds |
| other | 1 | platform-runtime-axioms (FFI assumptions with conformance witnesses) |

### Blocking-gap histogram (specs blocked by each audit §7 gap)

| gap | specs | note |
|---|---|---|
| 0 | 35 | prerequisite of every row; no spec is blocked by it alone |
| 1 | 14 | non-scalar carriers (`&str`, `String`, `Vec`, enums, structs) |
| 2 | 7 | `&self`/`&mut self` views, two-state |
| 3 | 5 | E6 body widening (calls, casts, loops, `+` with overflow check) |
| 5 | 2 | quantified clauses |
| 6 | 1 | plain-Rust clause publication |
| 7 | 3 | loops |
| 8 | 29 | effects: platform spec, FFI assumption channel, unsafe |
| 10 | 4 | model-to-code refinement beyond scalars |
| 11 | 9 | generics |
| 12 | 3 | cross-crate theorem citation |
| 13 | 8 | threads, closures, higher-order |

Gap 8 dominates: 29 of 35 specs, because this crate is an FFI binding. Only six specs avoid it
(listed in §0), and those are the start of §6.

## 2. Test → spec map (TSV; all 201)

`primary` is the spec that retires the test. `also` lists the other specs the same test samples.
`NON-SPEC` rows are explained in §4.

```tsv
file	test	primary	also
src/lib.rs	test_zsh_autoload_before_hook_usage	script-const-lints	
src/lib.rs	test_zsh_functions_defined_before_hooks	script-const-lints	
src/tests.rs	test_zsh_shell_d_globs_are_null_glob_guarded	sh-load-completes	
src/tests.rs	test_zsh_survives_shell_d_with_only_zsh_dropins	sh-load-completes	
src/tests.rs	test_reroute_dir_is_moved_to_the_front_of_path_by_each_shell	sh-path-front	
src/tests.rs	test_agents_dir_is_moved_to_the_front_beside_the_reroute_dir_by_each_shell	sh-path-front	
src/tests.rs	test_powershell_moves_the_agents_dir_to_the_front_before_the_reroute_block	sh-path-front	
src/tests.rs	test_bash_urlencode_handles_special_and_unicode_paths	sh-osc7-encoder	
src/tests.rs	test_zsh_urlencode_handles_special_and_unicode_paths	sh-osc7-encoder	
src/tests.rs	test_encoders_answer_in_reply_and_keep_the_users	sh-osc7-encoder	sh-633e-and-frame
src/tests.rs	test_bash_report_cwd_emits_percent_encoded_osc_7	sh-osc7-report	
src/tests.rs	test_bash_on_msys_and_cygwin_reports_the_windows_drive_form	sh-osc7-report	
src/tests.rs	test_bash_preexec_skips_prompt_command_array_siblings	sh-bash-preexec-capture	
src/tests.rs	test_zsh_report_cwd_emits_percent_encoded_osc_7	sh-osc7-report	
src/tests.rs	test_prepare_zsh_prompt_override_starts_without_hook_error	sh-load-completes	
src/tests.rs	test_detect_posix_shells	detect-shell	
src/tests.rs	test_scripts_contain_prompt_override	(none; delete: substring pin)	
src/tests.rs	test_cwd_tab_title_strips_control_chars	sh-633e-and-frame	
src/tests.rs	test_fish_encode_cmd_escapes_all_control_bytes	sh-633e-and-frame	
src/tests.rs	test_fish_encode_cmd_emits_no_raw_control_bytes	sh-633e-and-frame	
src/tests.rs	test_osc_frame_writers_use_printf	sh-633e-and-frame	
src/tests.rs	test_bash_osc_frame_writer_preserves_payload_escapes	sh-633e-and-frame	
src/tests.rs	test_zsh_osc_frame_writer_preserves_payload_escapes	sh-633e-and-frame	
src/tests.rs	test_prepare_writes_scripts	ensure-scripts-writes-set	injection-shape
src/tests.rs	test_prepare_bash_has_argv_override	injection-shape	
src/tests.rs	test_prepare_unknown_returns_none	prepare-into-unknown	
src/tests.rs	test_prepare_cached_skips_rewrite_and_self_heals	ensure-cached-memo	
src/tests.rs	test_prepare_cached_rewrites_on_base_change	ensure-cached-memo	
src/tests.rs	test_prepare_cached_retries_after_write_error	ensure-cached-memo	
src/tests.rs	test_prepare_fish_xdg_data_dirs	injection-shape	
src/tests.rs	test_zsh_wrapper_restores_zdotdir_and_sources_integration	sh-wrappers	
src/tests.rs	test_bash_wrapper_sources_profile_chain_and_bashrc	sh-wrappers	
src/tests.rs	test_bash_133b_embedded_in_custom_ps1	sh-bash-ps1-one-133b	
src/tests.rs	test_bash_default_mode_embeds_133b_in_ps1	(none; delete: spelling pin)	
src/tests.rs	test_bash_prompt_command_guard_prevents_spurious_capture	(none; delete: spelling pin)	
src/tests.rs	test_fish_powerline_sep_uses_separator_color	sh-prompt-render	
src/tests.rs	test_zsh_script_defines_id_suffix_helper	sh-nonce-capture	
src/tests.rs	test_fish_script_defines_id_suffix_helper	sh-nonce-capture	
src/tests.rs	test_bash_mark_functions_invoke_id_suffix	sh-marks	
src/tests.rs	test_zsh_mark_functions_invoke_id_suffix	sh-marks	
src/tests.rs	test_fish_mark_functions_invoke_id_suffix	sh-marks	
src/tests.rs	test_fish_loaded_guard_survives_an_empty_override	sh-fish-empty-is-unset	
src/tests.rs	test_fish_integration_loads_under_an_empty_guard_override	sh-fish-empty-is-unset	
src/tests.rs	test_fish_emits_all_133_marks_without_a_nonce	sh-marks	
src/tests.rs	test_fish_marks_carry_the_nonce_when_set	sh-marks	
src/tests.rs	test_fish_never_tests_definedness_of_an_aterm_variable	script-const-lints	
src/tests.rs	test_fish_empty_prompt_style_falls_through_to_the_user_prompt	sh-fish-empty-is-unset	
src/tests.rs	test_fish_empty_disable_prompt_titles_still_emits_titles	sh-fish-empty-is-unset	
src/tests.rs	test_fish_empty_prompt_colors_fall_back_to_defaults	sh-fish-empty-is-unset	
src/tests.rs	test_spawned_shells_never_read_the_developers_home	(none; delete: tests a test fixture)	
src/tests.rs	test_bash_ps1_mark_b_includes_nonce_when_set	sh-bash-ps1-one-133b	
src/tests.rs	test_bash_id_suffix_emits_hex_when_nonce_set	sh-nonce-capture	
src/tests.rs	test_bash_id_suffix_empty_when_nonce_unset	sh-nonce-capture	
src/tests.rs	test_zsh_id_suffix_emits_hex_when_nonce_set	sh-nonce-capture	
src/tests.rs	test_zsh_id_suffix_empty_when_nonce_unset	sh-nonce-capture	
src/tests.rs	test_fish_id_suffix_emits_hex_when_nonce_set	sh-nonce-capture	
src/tests.rs	test_zsh_script_unsets_shell_nonce_env_var	sh-nonce-capture	
src/tests.rs	test_fish_script_unsets_shell_nonce_env_var	sh-nonce-capture	
src/tests.rs	test_bash_unsets_shell_nonce_env_after_source	sh-nonce-capture	
src/tests.rs	test_bash_children_never_inherit_the_nonce_through_ps1	sh-nonce-capture	
src/tests.rs	test_zsh_unsets_shell_nonce_env_after_source	sh-nonce-capture	
src/tests.rs	test_fish_unsets_shell_nonce_env_after_source	sh-nonce-capture	
src/tests.rs	test_bash_fallback_unnonced_when_env_missing	(none; delete: duplicate)	
src/tests.rs	test_detect_powershell	detect-shell	
src/tests.rs	test_prepare_powershell_dot_sources_script	injection-shape	
src/tests.rs	test_powershell_script_is_ascii	script-const-lints	
src/tests.rs	test_powershell_script_references_shell_nonce_env	(none; delete: subsumed pin)	
src/tests.rs	test_powershell_script_unsets_shell_nonce_env_var	sh-nonce-capture	
src/tests.rs	test_powershell_mark_emissions_include_id_suffix	sh-marks	
src/tests.rs	test_powershell_script_reports_cwd_and_preserves_prompt	sh-pwsh-prompt	
src/tests.rs	test_powershell_sets_utf8_for_native_programs_in_an_aterm_tab	sh-pwsh-utf8	
src/tests.rs	test_powershell_speaks_utf8_only_inside_an_aterm_tab	sh-pwsh-utf8	
src/tests.rs	test_powershell_id_suffix_emits_hex_when_nonce_set	sh-nonce-capture	
src/tests.rs	test_powershell_id_suffix_empty_when_nonce_unset	sh-nonce-capture	
src/tests.rs	test_powershell_unsets_shell_nonce_env_after_source	sh-nonce-capture	
src/tests.rs	test_powershell_prompt_emits_osc7_and_prompt_marks	sh-pwsh-prompt	
src/tests.rs	test_powershell_osc7_path_percent_encodes_windows_paths	sh-osc7-encoder	
src/tests.rs	test_cache_dir_windows_uses_localappdata	cache-dir	
src/tests.rs	test_cache_dir_is_the_per_user_cache_outside_the_containment_write_roots	cache-dir	
src/tests.rs	test_detect_wsl_and_cmd	detect-shell	
src/tests.rs	test_prepare_wsl_env_carries_path_translated_wslenv	injection-shape	path-env-value-lossy
src/tests.rs	test_prepare_wsl_argv_execs_the_bash_rcfile	injection-shape	sh-windows-launchers
src/tests.rs	test_merge_wslenv_is_append_safe_and_idempotent	merge-wslenv	
src/tests.rs	test_wsl_cwd_env_only_crosses_a_posix_path	wsl-cwd-env	
src/tests.rs	test_prepare_cmd_wraps_the_prompt_with_marks_and_cwd	cmd-prompt	
src/tests.rs	test_cmd_prompt_titles_the_tab_with_the_directory	cmd-prompt	
src/tests.rs	test_cmd_prompt_reports_the_cwd_after_the_prompt_start	cmd-prompt	
src/tests.rs	test_cmd_prompt_leaves_a_nested_prompt_untouched	cmd-prompt	
src/tests.rs	test_augment_with_nonce_substitutes_the_placeholder	augment-with-nonce	
src/tests.rs	test_posix_scripts_are_written_lf_only	lf-only	ensure-scripts-writes-set
src/tests.rs	test_cmd_prompt_emits_real_osc_133_marks	sh-windows-launchers	
src/tests.rs	test_wsl_launcher_emits_real_osc_133_marks	sh-windows-launchers	
src/tests.rs	test_fish_never_glues_a_command_substitution	script-const-lints	
src/tests.rs	test_fish_osc7_resolves_the_host_without_a_substitution	sh-osc7-report	
src/tests.rs	test_fish_prompt_never_passes_a_palette_index_to_set_color	sh-prompt-render	
src/tests.rs	test_prompt_color_defaults_agree_across_shells	sh-prompt-render	
src/tests.rs	test_fish_osc7_is_well_formed_for_every_host_value	sh-osc7-report	
src/tests.rs	test_fish_prompt_reasserts_its_own_cwd_before_the_prompt_start	sh-osc7-report	
src/tests.rs	test_fish_fallback_prompt_survives_a_missing_hostname	sh-prompt-render	
src/tests.rs	test_fish_prompt_colors_are_sgr_indices_like_bash	sh-prompt-render	
src/tests.rs	test_scripts_mark_the_multiplexer_boundary_before_the_loader_guard	sh-mux	
src/tests.rs	test_bash_session_shell_stamps_the_multiplexer_base_a_pane_can_be_compared_against	sh-mux	
src/tests.rs	test_bash_marks_a_multiplexer_pane_and_clears_a_stale_marker	sh-mux	
src/tests.rs	test_zsh_marks_a_multiplexer_pane_and_clears_a_stale_marker	sh-mux	
src/tests.rs	test_fish_marks_a_multiplexer_pane_and_clears_a_stale_marker	sh-mux	
src/tests.rs	test_zsh_already_running_session_shell_picks_up_the_managed_dirs_live	sh-live-managed	
src/tests.rs	test_bash_already_running_session_shell_picks_up_the_managed_dirs_live	sh-live-managed	
src/tests.rs	test_bash_live_pickup_holds_under_set_u	sh-live-managed	
src/tests.rs	test_reroute_stubs_survive_a_prompt_time_prepend_with_no_hook_in_bash	sh-live-managed	
src/tests.rs	test_reroute_stubs_survive_a_prompt_time_prepend_with_no_hook_in_zsh	sh-live-managed	
src/tests.rs	test_reroute_stubs_survive_a_prompt_time_prepend_with_no_hook_in_fish	sh-live-managed	
src/tests.rs	test_zsh_twin_landing_in_an_agents_dir_already_leading_path_is_run_live	sh-live-managed	
src/tests.rs	test_zsh_hook_naming_an_agents_dir_that_appears_later_is_fronted_live	sh-live-managed	
src/tests.rs	test_bash_twin_landing_in_an_agents_dir_already_leading_path_is_run_live	sh-live-managed	
src/tests.rs	test_bash_hook_naming_an_agents_dir_that_appears_later_is_fronted_live	sh-live-managed	
src/tests.rs	test_fish_already_running_session_shell_picks_up_the_managed_dirs_live	sh-live-managed	
src/tests.rs	test_live_hot_path_functions_fork_nothing	script-const-lints	
src/tests.rs	test_live_reassert_is_wired_into_every_prompt_and_preexec_hook_and_gated_on_the_session	sh-live-managed	
src/tests.rs	a_live_zsh_takes_a_rekey_at_its_next_prompt	sh-rekey-channel	
src/tests.rs	a_live_bash_takes_a_rekey_at_its_next_prompt	sh-rekey-channel	
src/tests.rs	a_live_bash_prompt_carries_one_133b_however_many_prompts	sh-bash-ps1-one-133b	
src/tests.rs	a_live_fish_takes_a_rekey_at_its_next_prompt	sh-rekey-channel	
src/tests.rs	the_typed_rekey_sets_the_globals_the_marks_read	typed-rekey-text	sh-marks
src/tests.rs	a_live_bash_without_the_channel_takes_a_typed_rekey	sh-typed-rekey	
src/tests.rs	a_live_zsh_without_the_channel_takes_a_typed_rekey	sh-typed-rekey	
src/tests.rs	the_script_set_has_one_stable_sixteen_hex_address	script-set-address	ensure-scripts-writes-set
src/tests.rs	a_foreign_build_cannot_change_what_this_builds_tabs_source	content-address-isolation	
src/tests.rs	a_damaged_content_folder_is_replaced_and_a_matching_one_reused	install-script-set	
src/tests.rs	scratch_suffixes_drawn_at_once_never_collide	scratch-suffix-unique	
src/tests.rs	old_script_folders_are_collected_and_young_or_newest_ones_kept	gc-policy	
src/tests.rs	a_real_zsh_sources_this_builds_scripts_after_a_foreign_write	content-address-isolation	
src/tests_loader.rs	every_posix_script_is_a_loader_around_one_body	body-extract	sh-body-pointer
src/tests_loader.rs	the_script_set_carries_each_body_beside_its_loader	ensure-scripts-writes-set	
src/tests_loader.rs	a_real_zsh_sources_the_body_alone_cleanly	sh-body-pointer	
src/tests_loader.rs	a_live_zsh_takes_a_newer_body_at_its_next_prompt	sh-body-pointer	
src/tests_loader.rs	a_live_bash_takes_a_newer_body_at_its_next_prompt	sh-body-pointer	
src/tests_loader.rs	a_live_zsh_from_before_loaders_is_upgraded_by_the_typed_line	sh-typed-upgrade	
src/tests_loader.rs	a_live_bash_from_before_loaders_is_upgraded_by_the_typed_line	sh-typed-upgrade	
src/tests_loader.rs	the_key_only_line_takes_the_first_line_of_a_three_line_file	sh-typed-rekey	
src/tests_loader.rs	a_nested_zsh_is_not_integrated_by_the_typed_line	sh-typed-upgrade	
src/tests_loader.rs	a_nested_bash_is_not_integrated_by_the_typed_line	sh-typed-upgrade	
src/tests_loader.rs	a_live_fish_takes_a_newer_body_at_its_next_prompt	sh-body-pointer	
src/tests_loader.rs	a_live_fish_from_before_loaders_is_upgraded_by_the_typed_line	sh-typed-upgrade	
src/tests_loader.rs	a_nested_fish_is_not_integrated_by_the_typed_line	sh-typed-upgrade	
src/tests_loader.rs	a_live_zsh_keeps_the_users_key_bindings_across_a_newer_body	sh-body-pointer	
src/tests_loader.rs	a_live_fish_keeps_the_users_key_bindings_across_a_newer_body	sh-body-pointer	
src/tests_loader.rs	a_live_zsh_with_a_line_init_hook_is_upgraded_without_a_widget_loop	sh-typed-upgrade	
src/tests_loader.rs	a_live_fish_keeps_the_users_own_prompt_through_the_typed_upgrade	sh-typed-upgrade	
src/tests_loader.rs	a_live_fish_with_its_own_prompt_takes_a_newer_body	sh-body-pointer	
src/tests_loader.rs	a_live_fish_wrapped_prompt_reads_the_commands_status	sh-fish-user-prompt	
src/tests_loader.rs	a_live_fish_with_a_prompt_style_gives_the_users_prompt_back	sh-fish-user-prompt	
src/tests_loader.rs	a_live_fish_with_its_own_prompt_keeps_its_prompt_marks	sh-fish-user-prompt	
src/tests_tty.rs	an_integrated_zsh_keeps_its_tty_when_a_raw_program_dies	sh-tty-sanity	
src/tests_tty.rs	an_integrated_zsh_whose_rc_broke_the_tty_runs_its_first_command_sane	sh-tty-sanity	
src/tests_tty.rs	an_integrated_zsh_with_its_own_stty_is_repaired_not_frozen	sh-tty-sanity	
src/tests_tty.rs	a_running_zsh_with_a_broken_tty_is_repaired_by_the_body_the_loader_delivers	sh-tty-sanity	
src/tests_tty.rs	an_integrated_bash_repairs_its_tty_after_a_raw_program_fails	sh-tty-sanity	
src/tests_tty.rs	an_integrated_zsh_with_err_return_stays_frozen_after_a_failing_stty	sh-tty-sanity	
src/tests_tty.rs	an_integrated_zsh_refreezes_when_a_thawed_command_is_interrupted	sh-tty-sanity	
src/tests_tty.rs	an_integrated_zsh_marks_every_prompt_whatever_its_rc_does_to_zle_line_init	sh-zsh-line-init-marks	
src/tests_tty.rs	an_integrated_zsh_under_xtrace_traces_the_users_commands_and_not_aterms	sh-user-semantics-preserved	
src/tests_tty.rs	an_integrated_bash_under_xtrace_traces_the_users_commands_and_not_aterms	sh-user-semantics-preserved	
src/tests_tty.rs	an_integrated_bash_keeps_the_users_err_trap_errexit_and_functrace_as_bare_bash_does	sh-user-semantics-preserved	
src/tests_tty.rs	an_integrated_bash_keeps_the_users_last_argument	sh-user-semantics-preserved	
tests/prepare_fish_xdg_default.rs	prepare_fish_xdg_includes_default_fallback	injection-shape	
tests/prepare_zsh_unset_zdotdir.rs	prepare_zsh_sets_unset_zdotdir_when_empty	injection-shape	
```

## 3. Coverage

- 201 tests read (200 `#[test]` fns plus the `main_thread_bound_drive` harness). 165 map to one of the
  35 specs. 36 are not spec samples (§4).
- The biggest collapses: `platform-runtime-axioms` (18 primary), `declared-dispatch` (14),
  `declared-class-registration` (11), `block-abi` (10), `containment-trampoline` (9),
  `weak-obj-model` (9), `ownership-balance` (8), `swizzle-install-decision` (8), `main-thread-bound` (7).
- `platform-runtime-axioms` retires nothing by proof. Its 18 tests turn into the conformance witnesses
  and negative controls of the FFI assumptions it names. They stay as long as those assumptions are
  trusted rather than proved, and are deleted only if the platform lane can check the facts some other
  way.
- 38 tests in `gui_*`, `winit_*` and `cf_cg_sdk` check code in other crates (`aterm-gui`,
  `aterm-window`). Their specs (`sdk-constants-equal`, `send-site-abi`, `declared-row-matches-authority`)
  belong on those crates' functions and need gap 12 to be cited from here.

## 4. Tests that are not spec samples

**Status (2026-10-16):** deleted `main_thread_bound::the_container_is_send_and_sync_over_a_t_that_is_neither`, `adversary_w2c::the_return_types_this_crate_can_send_all_declare_their_abi`, `adversary_w2d::the_three_real_block_shapes_all_satisfy_the_new_encode_bounds`, `gui_appkit_constants::an_unparenthesised_compound_literal_would_be_vacuous` and `winit_seam::the_host_allowance_is_exactly_what_this_host_lacks`. The `gui_sent_prototypes`, `winit_sent_prototypes`, `send_prototype_census` and `objc2_exit_condition` files are no longer in this crate, so their rows were already gone.

Action counts: **delete 30**, **re-home as an `aterm-census` guard (or a `clippy.toml` row), then delete
here 6**.

| test | why | action |
|---|---|---|
| main_thread_bound::the_container_is_send_and_sync_over_a_t_that_is_neither | A compile-time trait assertion. Send/Sync come from unconditional unsafe impls, so the type system already guarantees it. The obligations behind the impls are in `main-thread-bound`. | delete |
| adversary_w2c::the_return_types_this_crate_can_send_all_declare_their_abi | A type-system tautology: the msg() casts compile because of the Encode bound, and compile_fail doctests pin the refusals. Its one runtime line (UTF8String round trip) duplicates `string-bridge-round-trip`. | delete |
| adversary_w2d::the_three_real_block_shapes_all_satisfy_the_new_encode_bounds | A compile-time bound check plus non-null asserts. Its `*mut Bool` row duplicates `encode-table` and `encode-matches-platform`. | delete |
| cf_cg_sdk::the_parser_saw_every_declaration | A vacuity guard for the test-side parser behind the SDK probe (no production caller). Obsolete once `ffi-decls-match-sdk` is a declaration-level contract. | delete |
| support/sdk_consts::the_parser_reads_every_visibility_spelling | Tests `parse_const_decl`, a test-support parser with no production caller. | delete |
| gui_appkit_constants::every_gui_constant_has_a_row_and_every_row_names_a_gui_constant | Coverage of a hand-written table: a registry policed by a test. It goes away once `sdk-constants-equal` quantifies over every const in the module. | delete |
| gui_appkit_constants::only_an_arch_gate_is_read_as_one | Tests the test-side `arch_gate` parser (no production caller). | delete |
| gui_appkit_constants::an_unparenthesised_compound_literal_would_be_vacuous | Checks a C/clang precedence premise the test probe depends on: test infrastructure, not aterm code. | delete |
| winit_seam_constants::every_seam_constant_has_a_row_and_every_row_names_a_seam_constant | Coverage of a hand table, redundant once `sdk-constants-equal` quantifies over the seam module. | delete |
| gui_sent_prototypes::every_sent_selector_in_scope_has_a_row | Coverage of a hand census table, redundant once `send-site-abi` is a contract on every send helper call. | delete |
| gui_sent_prototypes::the_table_names_the_helper_each_site_actually_calls | Keeps the hand table consistent with the source text: a registry policing itself. | delete |
| gui_sent_prototypes::source_walk_follows_extractions_and_rejects_helper_drift_and_dead_rows | Tests the test-side source walker (`selectors_in`, `source_helper_pairs`, `stale_rows`). No production caller. | delete |
| gui_sent_prototypes::the_untyped_block_sends_name_their_sites | Pins prose (site strings), a registry size of 2, and the absence of a `_block(` helper in send.rs (source text). | delete |
| gui_sent_prototypes::the_expected_shapes_are_not_all_the_same | A vacuity floor on the hand table's distinct shapes; meaningless once `send-site-abi` is per call site. | delete |
| winit_sent_prototypes::every_selector_the_fork_sends_has_a_census_row | Coverage of the hand census table; redundant under `send-site-abi`. | delete |
| winit_sent_prototypes::every_class_object_receiver_is_censused_for_its_own_class | Coverage of the hand census table for class receivers; redundant under `send-site-abi`. | delete |
| winit_sent_prototypes::the_expected_shapes_are_not_all_the_same | A vacuity floor on the hand table. | delete |
| winit_sent_prototypes::the_undecidable_rows_are_named_with_a_reason_and_stay_few | Pins a registry size (4) plus host facts about dynamic properties. Any host fact still needed belongs in `platform-runtime-axioms`. | delete |
| winit_seam::every_site_points_at_the_line_that_declares_it | Pins the hand table's file:line strings to the source: prose pinning. | delete |
| winit_seam::the_table_still_covers_every_declared_method | Counts `@sel` sites against the hand table; redundant once `declared-row-matches-authority` is attached to each `declare_class!` site. | delete |
| winit_seam::the_host_allowance_is_exactly_what_this_host_lacks | Polices the `HOST_MAY_LACK` registry against this host: census bookkeeping, not a property of aterm code. | delete |
| winit_seam::the_fork_patches_exactly_the_framework_rows_this_file_checks | A source-text census of `.install(` sites against a hand list; redundant because `SwizzleSite::install` (`swizzle-install-decision`) refuses an encoding mismatch at every site. | delete |
| send_prototype_census::no_send_prototype_is_spelled_extern_c | For msg() casts the type system already refuses a `"C"` prototype (no MsgFn impl; compile_fail doctest). For non-msg sites it is a source-text rule no type names. | re-home (non-msg sites) |
| send_prototype_census::the_census_is_not_vacuous | A vacuity floor (counts of C-unwind spellings) for a source-text census. | delete |
| send_prototype_census::the_entry_points_and_the_msgfn_impl_are_declared_c_unwind | Pins source text of runtime.rs. The MsgFn impl set is a type-system fact already pinned by a compile_fail doctest. | delete |
| send_prototype_census::no_transmuted_prototype_in_the_winit_backend_is_spelled_extern_c | A source-text census of another crate's spellings. | re-home |
| send_prototype_census::the_send_event_override_keeps_stop_app_on_panic_outside_and_contain_inside | Pins the textual order of statements in app.rs. The behaviour it stands for (catch_unwind outside, contain inside) is `containment-trampoline`. | delete |
| send_prototype_census::no_third_party_block_remains_on_a_raise_path | A dependency/source rule (no block2 use). Belongs in aterm-census, with the objc2 exit condition. | re-home |
| send_prototype_census::the_detector_sees_the_old_spelling | Tests the test-side detector (`on_comment_line`, `is_alias_or_binding`). A re-homed guard brings its own negative control. | delete |
| objc2_exit_condition::the_scope_sees_the_macos_gated_platform_extension | Tests the scope derivation of a dependency-policy census (test code); the re-homed guard carries its own. | delete |
| objc2_exit_condition::the_family_file_count_is_what_is_recorded | Pins recorded file counts: registry pinning. | delete |
| objc2_exit_condition::every_family_use_in_the_tree_is_in_scope_or_in_a_declared_slice | A dependency-policy source census over the crates. | re-home |
| objc2_exit_condition::the_code_rule_sees_a_renaming_import | Tests the test-side identifier scanner (`code_idents`). | delete |
| objc2_exit_condition::the_manifest_rule_sees_a_renamed_dependency | Tests the test-side manifest parser (`declared_deps`). | delete |
| objc2_exit_condition::the_manifest_rows_are_live_or_dead_exactly_as_the_file_counts_say | Cross-checks manifest text against recorded counts: dependency policy. | re-home |
| weak::a_pool_parameter_never_mints_a_lifetime | A source-text scan of fn signatures for an API-shape rule (no lifetime out of an `&AutoreleasePool` parameter): a lint or census rule, not a property of a function body. | re-home |

No test here is a pure regression with no general property behind it. Every adversary-wave test
(`adversary_w2*`) maps to one of the specs above.

## 5. Implied specs that look wrong, or disagree with the code

1. **Tautological assertions.**
   - `encode_offsets::stripping_deletes_offsets_and_nothing_else_over_every_row_in_the_process`:
     `with_count >= 1 && with_quoted == with_quoted` has a tautological second conjunct, so the
     quoted-class-name floor its message promises is never enforced.
   - `main_thread_bound::a_container_of_a_drop_less_value_never_reaches_the_main_queue`:
     `assert!(!std::mem::needs_drop::<Rc<u8>>() || true)` is a tautology. Its failure mode is an
     unbounded hang with no one-minute detector, against the AGENTS.md wait rule.
   - `adversary_w2c::a_context_pointer_and_a_selector_no_longer_encode_the_same`: the Observer class's
     method bodies contain `assert!(context.is_null() || !context.is_null())`.
2. **`MainThreadBound<T>` has no `#[repr(transparent)]`** (`src/main_thread_bound.rs:87`); only its
   `ManuallyDrop` field does. `main_thread_bound::the_container_costs_nothing_over_the_value` relies on
   unguaranteed `repr(Rust)` layout, while its message claims a guarantee. Add `#[repr(transparent)]`
   to make the layout clause of `main-thread-bound` true by construction.
3. **`cf_catch_up_steps` can overflow.** `lag / interval_ticks + 1` overflows for `lag = u64::MAX`,
   `interval_ticks = 1` (`src/wake_timer.rs`). The spec needs a precondition, or the code needs
   saturating/checked addition, before it can be proved. The test also reaches the rate through
   `mach_ticks_per_sec` FFI where a pure rate parameter would do; split the pure arithmetic out.
4. **A test pins a soundness hole.** `winit_seam::a_substituting_initializer_defeats_the_initialized_flag`
   asserts that a hole exists: `IvarSlot::get` hands out `&T` over a sibling class's bytes (case c), or
   reads past a smaller survivor's allocation (cases b and d). The safe `ivars()` accessor has an
   undischarged precondition (the receiver is of the declaring class). The spec
   (`ivar-slot-contract`) states that precondition; the code should check it (an `isKindOfClass:`
   guard, or make `ivars()` unsafe) rather than keep a test that documents the defect.
5. **Mislabelled or stale tests.**
   - `adversary_w2::a_thirty_two_byte_send_and_a_sixteen_byte_send_agree_with_the_c_abi` compares
     16-byte and 17-byte returns, not 32-byte ones, pads with `let _ = CGRect::default(); let _ =
     CGSize::default();`, and duplicates `abi::the_entry_point_is_chosen_by_the_return_type`.
   - `adversary_w2b::a_bool_argument_is_never_materialised_as_a_rust_bool` claims a property of argument
     passing, but only checks size, align, YES/NO and the encoding const, duplicating
     `abi::bool_encoding_matches_what_foundation_itself_emits`.
   - `adversary_w2d::bool_ptr_encodes_what_foundation_compiled_for_its_bool_out_parameter` says it was
     written and run on an Intel Mac and "has not been run on arm64". That note is stale on an arm64 host.
6. **Duplicates.** `runtime::a_cache_slot_survives_concurrent_first_use` and
   `adversary_w2::the_sel_cache_is_correct_under_a_hard_concurrent_first_use` are the same check.
   `blocks::the_dispose_helper_drops_the_closure_exactly_once` duplicates
   `adversary_w2::a_block_capturing_a_drop_type_drops_it_exactly_once_when_the_block_dies` and
   `adversary_w2::a_block_that_is_never_invoked_still_drops_its_capture_once`.
7. **Prose pinning in a floor.** `winit_sent_prototypes::every_sent_selector_encodes_the_way_its_helper_spells_it`
   has the floor `checked >= 216` with a long narrative of past counts in its message. The number goes
   stale every time the census changes. Under `send-site-abi` the floor disappears.
8. **A text census re-checks a type-system fact.** `send_prototype_census::no_send_prototype_is_spelled_extern_c`
   re-checks in text what MsgFn's missing `"C"` impl already refuses at compile time. Only the non-msg
   sites are uncovered by the type system, so only those move to aterm-census (§4).
9. **The test count in the wave-2 chunk list is off by one.** It lists 202 tests for aterm-objc, but
   `tests/main_thread_bound_drive.rs` and `tests/objc_static_suite.rs` match `#[test]` only in doc
   comments. There are 200 real `#[test]` fns, plus the harness=false drive counted as one test (201).

## 6. Proof order (easiest high-value first)

No spec is blocked by gap 0 alone, so the order starts with the six specs that need no platform lane,
cheapest first, and then the pure halves that can be factored out of effectful specs.

1. **`cf-catch-up-arithmetic`**, u64 half (gaps 3, 5). Fix the overflow first (§5.3), then this is
   `omega`-class arithmetic over `u64` division and addition, the same shape as the clamp probe the seal
   already publishes. Split `interval_ticks` (f64) into its own row; it waits on a float model.
2. **`bool-semantics`** (gap 1). A one-byte `repr(transparent)` newtype with `const fn` accessors; the
   round trip is a case split on `bool` (and on `i8 ≠ 0` for x86_64). Drop or factor the Debug clause.
3. **`encode-table`** (gaps 1, 11). Decidable by const evaluation of each impl's associated const once
   `&'static str` consts are carriers. It is the base of every encoding spec below.
4. **`strip-offsets-filter`** (gaps 1, 3, 5, 7). One loop over chars with a two-variable state; the loop
   invariant is the filter itself over the prefix. It underlies `swizzle-install-decision`,
   `send-site-abi` and `declared-row-matches-authority`.
5. **`method-encoding-concat`** (gaps 1, 3, 11). Straight-line concatenation per arity; with
   `encode-table` it gives the expected side of every encoding comparison.
6. **`wake-plan-refines-model`** (gaps 1, 2, 10). A small deterministic state machine over an `Option`
   and a `bool`, bound to an existing `ty_model!` model with a negative control already written.
7. **`returns-indirectly-and-entry`**, decision half (gaps 3, 11). The `returns_indirectly` predicate is
   pure; the symbol-address half needs gap 8.
8. **`send-rect-nil`**, nil branch (gaps 1, 3). An early return on a null check; the non-nil branch
   waits on gap 8.
9. **`swizzle-install-decision`**, after factoring the decision table into a pure fn over
   `(claim, owner, registered, expected, current, new)` (gap 1 for that half). It retires 8 tests once the
   CAS/FFI half (gaps 2, 8, 13) lands.
10. **`ivar-slot-contract`**, slot half (`IvarSlot::{init,get,dispose}`, gaps 1, 2, 11): flag protocol and
    exactly-once drop on a `repr(C)` struct, independent of the runtime's offset placement.

Then the specs gated on gap 8, starting with the FFI assumption channel itself: state
`platform-runtime-axioms` and `encode-matches-platform` as assumptions with their tests as conformance
witnesses, then `class-lookup`, `main-thread-witness`, `owning-class`, `sel-cache-coherent`,
`ownership-balance`, `autoreleasepool-scope`, `string-bridge-round-trip`, `weak-obj-model`,
`block-abi`, `objc-try-protocol`, `containment-trampoline` and its report format,
`declared-class-registration` and `declared-dispatch`, `main-thread-bound`, `protocol-or-register`,
`wake-timer-cf-refinement`. The cross-crate ones (`ffi-decls-match-sdk`, `sdk-constants-equal`,
`send-site-abi`, `declared-row-matches-authority`) also need gap 12 and belong with `aterm-gui` and
`aterm-window`.
