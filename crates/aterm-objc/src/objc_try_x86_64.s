## SPDX-License-Identifier: Apache-2.0
## Copyright 2026 Andrew Yates
##
## int aterm_objc_try(void *data, void (*body)(void *), void (*on_exc)(void *, id))
##
## The x86_64 counterpart of objc_try_aarch64.s, for the compat slice of the
## universal binary; from the probe's asmsrc-x86.s (`clang -arch x86_64 -S`).
## Same five external symbols, same LSDA shape, same Rust-panic pass-through.
## Assembled by a cross `cargo build --target x86_64-apple-darwin` (a `check`
## never assembles it) with `options(att_syntax)`, since clang emits AT&T and
## `global_asm!` defaults to Intel on x86; the development box cannot execute
## this slice.
	.section	__TEXT,__text,regular,pure_instructions
	.globl	_aterm_objc_try
	.p2align	4
_aterm_objc_try:
LaotTry_begin:
	.cfi_startproc
	.cfi_personality 155, ___objc_personality_v0
	.cfi_lsda 16, LaotTry_lsda
	pushq	%rbp
	.cfi_def_cfa_offset 16
	.cfi_offset %rbp, -16
	movq	%rsp, %rbp
	.cfi_def_cfa_register %rbp
	pushq	%r14
	pushq	%rbx
	.cfi_offset %rbx, -32
	.cfi_offset %r14, -24
	movq	%rdx, %rbx
	movq	%rdi, %r14
LaotTry_call:
	callq	*%rsi
LaotTry_call_end:
	xorl	%eax, %eax
LaotTry_ret:
	popq	%rbx
	popq	%r14
	popq	%rbp
	retq
LaotTry_pad:
	cmpl	$1, %edx
	jne	LaotTry_resume_rax
	movq	%rax, %rdi
	callq	_objc_begin_catch
	movq	%rax, %rsi
	movq	%r14, %rdi
LaotTry_onexc:
	callq	*%rbx
LaotTry_onexc_end:
	callq	_objc_end_catch
	movl	$1, %eax
	jmp	LaotTry_ret
LaotTry_cleanup:
	movq	%rax, %r14
	callq	_objc_end_catch
	movq	%r14, %rax
LaotTry_resume_rax:
	movq	%rax, %rdi
	callq	__Unwind_Resume
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
	.long	_OBJC_EHTYPE_id@GOTPCREL+4
LaotTry_ttbase:
	.p2align	2, 0x0
	.section	__TEXT,__text,regular,pure_instructions
