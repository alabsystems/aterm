// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! `#[export]`'s expansion, natively: the exports it writes are linked into
//! this test binary under their wasm names, so the test calls them as the
//! glue does, with words, and reads results through the return slot and
//! the allocator. A throw is a panic carrying the [`Thrown`] here.
//!
//! What only a wasm module shows (the descriptor section, the glue, a trap
//! poisoning an object) is the node toy's: `verify wasm-glue`.

use aterm_wasm_abi::__rt::Thrown;
use aterm_wasm_abi::{export, mem};

/// A counter with a name.
#[export]
pub struct Counter {
    n: u32,
    name: String,
}

#[export]
impl Counter {
    /// Start at `start`; a name is required.
    #[export(constructor)]
    pub fn new(start: u32, name: &str) -> Result<Counter, String> {
        if name.is_empty() {
            return Err("a counter needs a name".into());
        }
        Ok(Counter {
            n: start,
            name: name.to_string(),
        })
    }

    /// The count.
    #[export(getter)]
    pub fn n(&self) -> u32 {
        self.n
    }

    /// Add `by` (1 when absent) and return the count.
    pub fn add(&mut self, by: Option<u32>) -> u32 {
        self.n += by.unwrap_or(1);
        self.n
    }

    /// `name=n`.
    pub fn label(&self) -> String {
        format!("{}={}", self.name, self.n)
    }

    /// The count as an [`Item`], `None` at zero.
    pub fn item(&self) -> Option<Item> {
        (self.n != 0).then_some(Item {
            v: u64::from(self.n) << 32,
        })
    }

    /// Halve an even count; an odd one is an error.
    pub fn halve(&mut self) -> Result<(), String> {
        if self.n % 2 == 1 {
            return Err(format!("{} is odd", self.n));
        }
        self.n /= 2;
        Ok(())
    }

    /// A static method.
    pub fn zero() -> u32 {
        0
    }

    /// Left alone: not `pub`.
    fn bump(&mut self) {
        self.n += 1;
    }

    /// Configured like aterm-wasm's getters: the option arrives through
    /// `cfg_attr`, which rustc expands before the method's marker runs.
    #[cfg_attr(all(), export(getter))]
    pub fn name_len(&mut self) -> usize {
        self.bump();
        self.name.len()
    }
}

/// A value returned by value.
#[export]
pub struct Item {
    v: u64,
}

#[export]
impl Item {
    /// The value.
    #[export(getter)]
    pub fn v(&self) -> u64 {
        self.v
    }
}

/// The sum of `bytes`.
#[export(js_name = sumBytes)]
pub fn sum_bytes(bytes: &[u8]) -> u32 {
    bytes.iter().map(|&b| u32::from(b)).sum()
}

// The exports, as the glue sees them: every parameter is two words.
unsafe extern "C-unwind" {
    fn counter_new(start: u32, _: u32, name: usize, len: usize) -> usize;
    fn counter_n(this: usize) -> u32;
    fn counter_add(this: usize, some: u32, by: u32) -> u32;
    fn counter_label(this: usize) -> usize;
    fn counter_item(this: usize) -> usize;
    fn counter_halve(this: usize);
    fn counter_zero() -> u32;
    fn counter_name_len(this: usize) -> usize;
    fn __aterm_free_counter(ptr: usize, from_finalizer: u32);
    fn item_v(this: usize) -> u64;
    fn __aterm_free_item(ptr: usize, from_finalizer: u32);
    #[link_name = "sumBytes"]
    fn sum_bytes_export(ptr: usize, len: usize) -> u32;
}

/// The glue's `passStringToWasm0`.
fn pass(bytes: &[u8]) -> (usize, usize) {
    let ptr = mem::alloc(bytes.len(), 1).unwrap();
    // SAFETY: a fresh allocation of `bytes.len()` bytes.
    unsafe { std::ptr::copy_nonoverlapping(bytes.as_ptr(), ptr as *mut u8, bytes.len()) };
    (ptr, bytes.len())
}

/// The glue's string return: the length from the slot, copy, free.
fn take_string(ptr: usize) -> String {
    let len = mem::ret() as usize;
    // SAFETY: the export handed out `len` bytes of UTF-8 at `ptr`.
    let s =
        String::from_utf8(unsafe { std::slice::from_raw_parts(ptr as *const u8, len) }.to_vec())
            .unwrap();
    // SAFETY: freed once, with the layout it was made with.
    unsafe { mem::free(ptr, len, 1) };
    s
}

/// What a call threw, or `None` when it returned: the glue's check after
/// every call. A call that threw returned a zero word.
fn thrown<R: Default + PartialEq + std::fmt::Debug>(f: impl FnOnce() -> R) -> Option<Thrown> {
    let word = f();
    let thrown = mem::take_thrown();
    if thrown.is_some() {
        assert_eq!(word, R::default(), "a call that threw returns a zero word");
    }
    thrown
}

fn error(msg: &str) -> Option<Thrown> {
    Some(Thrown::Error(msg.to_string()))
}

#[test]
fn a_class_is_built_called_and_freed_through_its_exports() {
    let (name, len) = pass(b"clicks");
    // SAFETY (this test): words as the glue lowers them; each handle is used
    // only while live.
    unsafe {
        let c = counter_new(4, 0, name, len);
        assert_ne!(c, 0);
        assert_eq!(counter_n(c), 4);
        assert_eq!(counter_add(c, 0, 99), 5, "None adds 1");
        assert_eq!(counter_add(c, 1, 5), 10);
        assert_eq!(take_string(counter_label(c)), "clicks=10");
        assert_eq!(counter_zero(), 0);
        assert_eq!(counter_name_len(c), 6);
        assert_eq!(counter_n(c), 11, "the cfg_attr getter ran its body");

        assert_eq!(
            thrown(|| counter_halve(c)),
            Some(Thrown::Value("11 is odd".into()))
        );
        assert_eq!(counter_n(c), 11, "a thrown Err left the object usable");
        counter_add(c, 1, 1);
        assert_eq!(thrown(|| counter_halve(c)), None);
        assert_eq!(counter_n(c), 6);

        let item = counter_item(c);
        assert_ne!(item, 0);
        assert_eq!(item_v(item), 6 << 32);
        __aterm_free_item(item, 0);

        __aterm_free_counter(c, 0);
    }
}

#[test]
fn constructor_errors_and_option_classes_cross_as_values() {
    let (name, len) = pass(b"");
    // SAFETY: as above.
    unsafe {
        assert_eq!(
            thrown(|| counter_new(0, 0, name, len)),
            Some(Thrown::Value("a counter needs a name".into()))
        );
        let (name, len) = pass(b"z");
        let c = counter_new(0, 0, name, len);
        assert_eq!(counter_item(c), 0, "None is a null handle");
        __aterm_free_counter(c, 1);
    }
}

#[test]
fn misuse_throws_the_errors_wasm_bindgen_throws() {
    let (name, len) = pass(b"x");
    // SAFETY: 0 and live handles only.
    unsafe {
        assert_eq!(
            thrown(|| counter_n(0)),
            error("null pointer passed to rust")
        );
        assert_eq!(
            thrown(|| __aterm_free_counter(0, 0)),
            error("null pointer passed to rust")
        );
        assert_eq!(
            thrown(|| __aterm_free_counter(0, 1)),
            None,
            "the finalizer's free reports nothing: no JS frame would catch it"
        );
        let c = counter_new(2, 0, name, len);
        let item = counter_item(c);
        assert_eq!(
            thrown(|| counter_n(item)),
            error("an object of another class was passed to rust")
        );
        __aterm_free_item(item, 1);
        __aterm_free_counter(c, 0);
    }
}

#[test]
fn a_free_function_takes_a_borrowed_buffer() {
    let (ptr, len) = pass(&[1, 2, 250]);
    // SAFETY: a buffer as the glue passes it; the export takes and frees it.
    assert_eq!(unsafe { sum_bytes_export(ptr, len) }, 253);
    // SAFETY: an empty buffer is its alignment and never allocated.
    assert_eq!(unsafe { sum_bytes_export(1, 0) }, 0);
}
