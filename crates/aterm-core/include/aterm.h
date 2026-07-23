// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
//
// Author: Andrew Yates
//
// Public C ABI for the aterm engine (ATERM_DESIGN WS-D).
//
// This header declares exactly the symbols the aterm cdylib/staticlib
// (`libaterm_ffi`) actually exports — no more, no less. It is self-contained:
// a C consumer needs only `#include "aterm.h"`.
//
// The whole point of aterm is reliable screen reading; this exposes that to any
// language with a C FFI as the canonical "feed bytes -> read the screen" loop,
// over an opaque engine handle:
//
//     AtermEngine* e = aterm_engine_new(24, 80);
//     aterm_engine_feed(e, input, input_len);
//     char* screen = aterm_engine_visible_content(e);  // NUL-terminated UTF-8
//     // ... use screen ...
//     aterm_string_free(screen);
//     aterm_engine_free(e);
//
// Ownership contract:
//   - Strings returned by this library are owned by the caller and MUST be
//     released with `aterm_string_free`.
//   - Engine handles MUST be released with `aterm_engine_free`.
//   - Every function null-checks its handle; null in == safe no-op / NULL out.

#ifndef ATERM_H
#define ATERM_H

#include <stddef.h>  // size_t
#include <stdint.h>  // uint16_t

#ifdef __cplusplus
extern "C" {
#endif  // __cplusplus

// Opaque engine handle. The Rust side backs this with its `Terminal` type; the
// C side only ever holds a pointer to it.
typedef struct AtermEngine AtermEngine;

// Create a new engine of `rows` x `cols`. Free with `aterm_engine_free`.
AtermEngine* aterm_engine_new(uint16_t rows, uint16_t cols);

// Feed `len` VT bytes at `ptr` to the engine. No-op on a null handle or pointer.
//
// `engine` must be a live handle from `aterm_engine_new`; `ptr` must point to at
// least `len` readable bytes.
void aterm_engine_feed(AtermEngine* engine, const uint8_t* ptr, size_t len);

// Resize the engine grid. No-op on a null handle.
//
// `engine` must be a live handle from `aterm_engine_new`.
void aterm_engine_resize(AtermEngine* engine, uint16_t rows, uint16_t cols);

// The visible screen as a newly-allocated NUL-terminated UTF-8 C string. Free
// with `aterm_string_free`. Returns NULL on a null handle.
//
// `engine` must be a live handle from `aterm_engine_new`.
char* aterm_engine_visible_content(const AtermEngine* engine);

// Row `row`'s text as a newly-allocated NUL-terminated UTF-8 C string. Free with
// `aterm_string_free`. Returns NULL on a null handle or an out-of-range row.
//
// `engine` must be a live handle from `aterm_engine_new`.
char* aterm_engine_row_text(const AtermEngine* engine, size_t row);

// Free a string previously returned by this library. No-op on NULL.
//
// `s` must be a pointer returned by one of this library's string functions and
// not already freed.
void aterm_string_free(char* s);

// Free an engine handle. No-op on NULL.
//
// `engine` must be a handle from `aterm_engine_new` and not already freed.
void aterm_engine_free(AtermEngine* engine);

#ifdef __cplusplus
}  // extern "C"
#endif  // __cplusplus

#endif /* ATERM_H */
