// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `strip_method_offsets` against every instance-method row in the live runtime.
//!
//! [`aterm_objc::strip_method_offsets`] is the normaliser every encoding check here runs its
//! inputs through (the winit seam census, `objc_live_class_audit`, `toolbar_drive`,
//! `SwizzleSite::install`'s refusal); if it erased a difference, each would accept two encodings
//! that are different types. Bitfield widths, digits in type names, array counts and quoted class
//! names are not offsets, and a rule written as a list of exceptions misses spellings, so the
//! property is computed from the input: stripping may delete offsets and nothing else. The corpus
//! is the runtime's, so it grows when Apple's does.

#![cfg(target_os = "macos")]

use std::ffi::{CStr, c_char, c_void};

use aterm_objc::{ClassPtr, strip_method_offsets};

// AppKit, so the corpus is the one the auditors run against (a system framework, not a crate
// dependency).
//
// SAFETY: the block declares no symbol; its only job is the link line.
#[link(name = "AppKit", kind = "framework")]
unsafe extern "C" {}

unsafe extern "C" {
    fn objc_copyClassList(out_count: *mut u32) -> *mut ClassPtr;
    fn class_copyMethodList(cls: ClassPtr, out_count: *mut u32) -> *mut *const c_void;
    fn method_getTypeEncoding(method: *const c_void) -> *const c_char;
    fn free(ptr: *mut c_void);
}

/// Everything in an encoding that is not an offset, in order: struct, union and template tags,
/// bitfield widths, array counts and quoted class names. A reading independent of the function
/// under test: equal token lists may still differ in offsets; different lists are different types.
fn significant_tokens(enc: &str) -> Vec<String> {
    let b: Vec<char> = enc.chars().collect();
    let mut out = Vec::new();
    let mut i = 0_usize;
    let mut depth = 0_usize;
    let mut in_quotes = false;
    while i < b.len() {
        let c = b[i];
        if c == '"' {
            // A quoted class name, kept whole.
            let start = i;
            i += 1;
            while i < b.len() && b[i] != '"' {
                i += 1;
            }
            out.push(format!(
                "name:{}",
                b[start + 1..i.min(b.len())].iter().collect::<String>()
            ));
            in_quotes = false;
            i += 1;
            continue;
        }
        if in_quotes {
            i += 1;
            continue;
        }
        match c {
            '{' | '(' => {
                // The tag runs to the `=` (or to the close, for `{CGRect}`).
                depth += 1;
                let start = i + 1;
                let mut j = start;
                while j < b.len() && b[j] != '=' && b[j] != '}' && b[j] != ')' {
                    j += 1;
                }
                out.push(format!("tag:{}", b[start..j].iter().collect::<String>()));
                i = j;
                continue;
            }
            '}' | ')' => depth = depth.saturating_sub(1),
            '[' => {
                depth += 1;
                let start = i + 1;
                let mut j = start;
                while j < b.len() && b[j].is_ascii_digit() {
                    j += 1;
                }
                out.push(format!("count:{}", b[start..j].iter().collect::<String>()));
                i = j;
                continue;
            }
            ']' => depth = depth.saturating_sub(1),
            'b' if i + 1 < b.len() && b[i + 1].is_ascii_digit() => {
                let start = i + 1;
                let mut j = start;
                while j < b.len() && b[j].is_ascii_digit() {
                    j += 1;
                }
                out.push(format!("bits:{}", b[start..j].iter().collect::<String>()));
                i = j;
                continue;
            }
            _ => {}
        }
        let _ = depth;
        i += 1;
    }
    out
}

/// Every instance-method type encoding registered in this process.
fn every_registered_encoding() -> Vec<(String, String)> {
    let mut out = Vec::new();
    let mut n_classes: u32 = 0;
    // SAFETY: `objc_copyClassList` fills the count and returns a
    // malloc'd array the caller frees; a null return means zero classes.
    let classes = unsafe { objc_copyClassList(&raw mut n_classes) };
    if classes.is_null() {
        return out;
    }
    for ci in 0..n_classes as usize {
        // SAFETY: `ci` is in range and the array holds live class objects.
        let cls = unsafe { *classes.add(ci) };
        if cls.is_null() {
            continue;
        }
        // SAFETY: `cls` is a live class object.
        let name = unsafe { aterm_objc::class_name(cls) }
            .to_string_lossy()
            .into_owned();
        let mut n: u32 = 0;
        // SAFETY: as above; the list is malloc'd and freed below.
        let list = unsafe { class_copyMethodList(cls, &raw mut n) };
        if list.is_null() {
            continue;
        }
        for i in 0..n as usize {
            // SAFETY: `i` is in range and each element is a live `Method`.
            let types = unsafe { method_getTypeEncoding(*list.add(i)) };
            if types.is_null() {
                continue;
            }
            // SAFETY: a non-null, NUL-terminated, runtime-owned string.
            out.push((
                name.clone(),
                unsafe { CStr::from_ptr(types) }
                    .to_string_lossy()
                    .into_owned(),
            ));
        }
        // SAFETY: `list` came from `class_copyMethodList`, which the caller frees.
        unsafe { free(list.cast()) };
    }
    // SAFETY: `classes` came from `objc_copyClassList`.
    unsafe { free(classes.cast()) };
    out
}

/// Stripping deletes offsets and nothing else, over every row in the process. The corpus floors
/// keep the survey from passing vacuously: a runtime with no row of a kind fails loudly.
#[test]
fn stripping_deletes_offsets_and_nothing_else_over_every_row_in_the_process() {
    let rows = every_registered_encoding();
    let mut with_bits = 0_usize;
    let mut with_tag_digit = 0_usize;
    let mut with_count = 0_usize;
    let mut with_quoted = 0_usize;
    let mut bad: Vec<String> = Vec::new();

    for (cls, raw) in &rows {
        let stripped = strip_method_offsets(raw);
        let before = significant_tokens(raw);
        let after = significant_tokens(&stripped);
        if before.iter().any(|t| t.starts_with("bits:")) {
            with_bits += 1;
        }
        if before
            .iter()
            .any(|t| t.starts_with("tag:") && t.chars().any(|c| c.is_ascii_digit()))
        {
            with_tag_digit += 1;
        }
        if before.iter().any(|t| t.starts_with("count:")) {
            with_count += 1;
        }
        if before.iter().any(|t| t.starts_with("name:")) {
            with_quoted += 1;
        }
        if before != after && bad.len() < 12 {
            bad.push(format!(
                "{cls}\n      raw      {raw}\n      stripped {stripped}\n      lost     {:?}",
                before
                    .iter()
                    .filter(|t| !after.contains(t))
                    .collect::<Vec<_>>()
            ));
        }
    }

    assert!(
        rows.len() >= 20_000,
        "the walk found only {} row(s); it is not reading the runtime, and a \
         survey that reads nothing passes",
        rows.len()
    );
    assert!(
        with_bits >= 1,
        "no row in this process carries a BITFIELD WIDTH, so this test no \
         longer exercises the kind of non-offset digit that found it \
         (`-[NSPrinter _getNodeForKey:inTable:]` had one when it was written)"
    );
    assert!(
        with_tag_digit >= 1,
        "no row in this process carries a DIGIT IN A STRUCT TAG, so this test \
         no longer exercises the second kind (`{{CATransform3D=…}}` had one)"
    );
    assert!(
        with_count >= 1 && with_quoted == with_quoted,
        "no row carries an ARRAY COUNT, the one kind the original rule DID \
         keep — {with_count} found, {with_quoted} quoted class name(s)"
    );
    assert!(
        bad.is_empty(),
        "{} row(s) lost something that is not an offset — a bitfield width, a \
         struct tag, an array count or a quoted class name. Every instrument \
         that normalises through this function would now agree about two \
         encodings that are different types:\n  {}",
        bad.len(),
        bad.join("\n  ")
    );
}
