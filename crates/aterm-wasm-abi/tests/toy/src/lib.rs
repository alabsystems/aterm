// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Toy WebAssembly module exporting every `aterm-wasm-abi` boundary type and shape.

#![cfg(all(target_arch = "wasm32", target_os = "unknown"))]

use std::sync::atomic::{AtomicU32, Ordering};

use aterm_wasm_abi::{export, host, panic_hook};

aterm_wasm_abi::export_runtime!();

static DROPS: AtomicU32 = AtomicU32::new(0);

macro_rules! echo {
    ($($name:ident: $t:ty),* $(,)?) => {$(
        /// Returns its argument.
        #[export]
        pub fn $name(v: $t) -> $t {
            v
        }
    )*};
}

echo! {
    echo_bool: bool,
    echo_u8: u8,
    echo_u16: u16,
    echo_u32: u32,
    echo_u64: u64,
    echo_usize: usize,
    echo_i8: i8,
    echo_i16: i16,
    echo_i32: i32,
    echo_i64: i64,
    echo_isize: isize,
    echo_f32: f32,
    echo_f64: f64,
    echo_char: char,
    echo_string: String,
    echo_opt_bool: Option<bool>,
    echo_opt_u16: Option<u16>,
    echo_opt_u32: Option<u32>,
    echo_opt_i32: Option<i32>,
    echo_opt_u64: Option<u64>,
    echo_opt_f64: Option<f64>,
    echo_opt_char: Option<char>,
    echo_opt_string: Option<String>,
    echo_vec_u8: Vec<u8>,
    echo_opt_vec_u8: Option<Vec<u8>>,
}

/// A borrowed string, returned as a new one.
#[export]
pub fn echo_str(v: &str) -> String {
    v.to_string()
}

/// A borrowed byte slice, returned as a vector.
#[export]
pub fn echo_bytes(v: &[u8]) -> Vec<u8> {
    v.to_vec()
}

/// `&[u32]` in, `Vec<u32>` out.
#[export]
pub fn echo_u32s(v: &[u32]) -> Vec<u32> {
    v.to_vec()
}

/// `&[i64]` in, `Vec<i64>` out (a `BigInt64Array`).
#[export]
pub fn echo_i64s(v: &[i64]) -> Vec<i64> {
    v.to_vec()
}

/// `&[f64]` in, `Vec<f64>` out.
#[export]
pub fn echo_f64s(v: &[f64]) -> Vec<f64> {
    v.to_vec()
}

/// `&[f32]` in, `Vec<f32>` out.
#[export]
pub fn echo_f32s(v: &[f32]) -> Vec<f32> {
    v.to_vec()
}

/// `&[u16]` in, `Vec<u16>` out.
#[export]
pub fn echo_u16s(v: &[u16]) -> Vec<u16> {
    v.to_vec()
}

/// The UTF-8 length of `text`, which crossed as UTF-16 in JS.
#[export]
pub fn utf8_len(text: &str) -> usize {
    text.len()
}

/// Every parameter shape at once, with a non-trailing optional: `a|b|c|d|e`.
#[export]
pub fn mixed(a: u8, b: Option<u64>, c: &str, d: Option<bool>, e: f32) -> String {
    format!("{a}|{b:?}|{c}|{d:?}|{e}")
}

/// Trailing optionals, which JS may leave out: `a + b + len(c)`.
#[export]
pub fn trailing(a: u32, b: Option<u32>, c: Option<String>) -> u32 {
    a + b.unwrap_or(0) + c.map_or(0, |c| c.len() as u32)
}

/// Parameters JS cannot name as written.
#[export]
pub fn reserved(function: u32, this: u32) -> u32 {
    function * 10 + this
}

/// Rust keywords as parameters, written raw: the descriptor records them
/// without the `r#`.
#[export]
pub fn raw_idents(r#type: u32, r#in: u32) -> u32 {
    r#type * 10 + r#in
}

/// `Ok(n * 2)`, or `Err(msg)` thrown as the string.
#[export]
pub fn checked(n: u32, msg: &str) -> Result<u32, String> {
    if msg.is_empty() {
        Ok(n * 2)
    } else {
        Err(msg.to_string())
    }
}

/// `Ok(Some(text))` for a non-empty text, `Ok(None)` for an empty one, an
/// error for `"!"`.
#[export]
pub fn checked_opt(text: &str) -> Result<Option<String>, String> {
    match text {
        "!" => Err("bang".to_string()),
        "" => Ok(None),
        t => Ok(Some(t.to_uppercase())),
    }
}

/// Nothing in, nothing out.
#[export]
pub fn unit() {}

/// `n` bytes of `fill`: enough to grow the memory under the glue's views.
#[export]
pub fn filled(n: u32, fill: u8) -> Vec<u8> {
    vec![fill; n as usize]
}

/// Values dropped so far.
#[export]
pub fn drops() -> u32 {
    DROPS.load(Ordering::Relaxed)
}

/// `Date.now()` through the `aterm_host` import.
#[export]
pub fn date_now() -> f64 {
    host::date_now()
}

/// `performance.now()` through the `aterm_host` import.
#[export]
pub fn performance_now() -> f64 {
    host::performance_now()
}

/// Install the panic hook on the host console, as a module's constructor
/// does.
#[export(js_name = installHook)]
pub fn install_hook() {
    panic_hook::set_once(host::CONSOLE);
}

/// Panic with `msg`: a trap, after the hook (if installed) has logged it.
#[export(js_name = panicWith)]
pub fn panic_with(msg: &str) {
    panic!("{msg}");
}

/// A counter with a log: the class with every member kind.
#[export]
pub struct Counter {
    value: i64,
    notes: Vec<String>,
}

impl Drop for Counter {
    fn drop(&mut self) {
        DROPS.fetch_add(1, Ordering::Relaxed);
    }
}

#[export]
impl Counter {
    /// A counter at `start`; a negative start throws.
    #[export(constructor)]
    pub fn new(start: i64) -> Result<Counter, String> {
        if start < 0 {
            return Err(format!("negative start {start}"));
        }
        Ok(Counter {
            value: start,
            notes: Vec::new(),
        })
    }

    /// A counter at `seed`, through a static method.
    pub fn from_seed(seed: u32) -> Counter {
        Counter {
            value: i64::from(seed),
            notes: Vec::new(),
        }
    }

    /// The value.
    #[export(getter)]
    pub fn value(&self) -> i64 {
        self.value
    }

    /// The notes, joined by `;`.
    #[export(getter)]
    pub fn log(&self) -> String {
        self.notes.join(";")
    }

    /// Add `by`; the new value.
    pub fn add(&mut self, by: i64) -> i64 {
        self.value += by;
        self.value
    }

    /// Append a note; how many there are.
    pub fn note(&mut self, text: &str) -> usize {
        self.notes.push(text.to_string());
        self.notes.len()
    }

    /// A snapshot, returned by value.
    pub fn snapshot(&self) -> Snapshot {
        Snapshot {
            value: self.value,
            notes: self.notes.len() as u32,
        }
    }

    /// A snapshot when a note contains `needle`.
    pub fn find(&self, needle: &str) -> Option<Snapshot> {
        self.notes
            .iter()
            .any(|n| n.contains(needle))
            .then(|| self.snapshot())
    }

    /// Panic mid-change: the value is bumped, then the module traps.
    pub fn boom(&mut self, msg: &str) -> u32 {
        self.value += 1;
        panic!("{msg}");
    }

    /// A renamed method.
    #[export(js_name = describe)]
    pub fn describe_counter(&self) -> String {
        format!("Counter({})", self.value)
    }
}

/// A counter's state at one moment.
#[export]
pub struct Snapshot {
    value: i64,
    notes: u32,
}

impl Drop for Snapshot {
    fn drop(&mut self) {
        DROPS.fetch_add(1, Ordering::Relaxed);
    }
}

#[export]
impl Snapshot {
    /// The value then.
    #[export(getter)]
    pub fn value(&self) -> i64 {
        self.value
    }

    /// The note count then.
    #[export(getter)]
    pub fn notes(&self) -> u32 {
        self.notes
    }
}
