; SPDX-License-Identifier: Apache-2.0
; Copyright 2026 Andrew Yates
;
; int aterm_objc_try(void *data, void (*body)(void *), void (*on_exc)(void *, id))
;
; The `@try [ body(data); return 0; ] @catch (id e) [ on_exc(data, e); return 1; ]`
; (square brackets here because this text is a `global_asm!` template, where a
; brace is a format placeholder)
; wrapper, as the text `clang -S -fobjc-exceptions -O2` emits for it (the probe
; that measured this design: scratchpad/probe-containment/asmsrc.s), with the
; prototype reshaped to the one primitive `exception.rs` needs. Nothing is
; compiled from C or Objective-C: the crate stays dependency-free and
; `build.rs`-free by construction, and this text references exactly five
; symbols outside itself — `___objc_personality_v0`, `_objc_begin_catch`,
; `_objc_end_catch` and `_OBJC_EHTYPE_id` from libobjc, and `__Unwind_Resume`
; from libunwind — all of them already in every aterm process.
;
; A Rust panic passing through is NOT caught here: the personality matches only
; the `id` typeinfo, a foreign (Rust) exception matches no typeinfo but a
; catch-all, so it continues to the `catch_unwind` outside (measured: the
; probe's rust-panic-through-asmtry mode).
	.section	__TEXT,__text,regular,pure_instructions
	.globl	_aterm_objc_try
	.p2align	2
_aterm_objc_try:
LaotTry_begin:
	.cfi_startproc
	.cfi_personality 155, ___objc_personality_v0
	.cfi_lsda 16, LaotTry_lsda
	stp	x20, x19, [sp, #-32]!
	stp	x29, x30, [sp, #16]
	add	x29, sp, #16
	.cfi_def_cfa w29, 16
	.cfi_offset w30, -8
	.cfi_offset w29, -16
	.cfi_offset w19, -24
	.cfi_offset w20, -32
	mov	x19, x2
	mov	x20, x0
LaotTry_call:
	blr	x1
LaotTry_call_end:
	mov	w0, #0
LaotTry_ret:
	ldp	x29, x30, [sp, #16]
	ldp	x20, x19, [sp], #32
	ret
LaotTry_pad:
	cmp	w1, #1
	b.ne	LaotTry_resume
	bl	_objc_begin_catch
	mov	x1, x0
	mov	x0, x20
LaotTry_onexc:
	blr	x19
LaotTry_onexc_end:
	bl	_objc_end_catch
	mov	w0, #1
	b	LaotTry_ret
LaotTry_cleanup:
	mov	x20, x0
	bl	_objc_end_catch
	mov	x0, x20
LaotTry_resume:
	bl	__Unwind_Resume
LaotTry_end:
	.cfi_endproc
	.section	__TEXT,__gcc_except_tab
	.p2align	2, 0x0
LaotTry_lsda:
	.byte	255
	.byte	155
	.uleb128 LaotTry_ttbase-LaotTry_ttbaseref
LaotTry_ttbaseref:
	.byte	1
	.uleb128 LaotTry_cst_end-LaotTry_cst_begin
LaotTry_cst_begin:
	.uleb128 LaotTry_call-LaotTry_begin
	.uleb128 LaotTry_call_end-LaotTry_call
	.uleb128 LaotTry_pad-LaotTry_begin
	.byte	1
	.uleb128 LaotTry_call_end-LaotTry_begin
	.uleb128 LaotTry_onexc-LaotTry_call_end
	.byte	0
	.byte	0
	.uleb128 LaotTry_onexc-LaotTry_begin
	.uleb128 LaotTry_onexc_end-LaotTry_onexc
	.uleb128 LaotTry_cleanup-LaotTry_begin
	.byte	0
	.uleb128 LaotTry_onexc_end-LaotTry_begin
	.uleb128 LaotTry_end-LaotTry_onexc_end
	.byte	0
	.byte	0
LaotTry_cst_end:
	.byte	1
	.byte	0
	.p2align	2, 0x0
LaotTry_tt:
	.long	_OBJC_EHTYPE_id@GOT-LaotTry_tt
LaotTry_ttbase:
	.p2align	2, 0x0
	.section	__TEXT,__text,regular,pure_instructions
