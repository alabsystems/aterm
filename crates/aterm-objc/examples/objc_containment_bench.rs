// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
//! The cost of exception containment, measured the way the design probe
//! measured it: `objc_msgSend` into a declared method that returns 42,
//! 200M sends, best of three, release profile.
//!
//! Three IMPs on one class: a bare `extern "C-unwind"` Rust function added
//! through `ClassBuilder::add_method` (no trampoline at all), a
//! `declare_class!` method under the default containment (`catch_unwind`
//! outside, `@try` inside), and the same method declared
//! `@abort_on_exception` (the pre-containment shape: `catch_unwind` alone).
//! The difference between the last two is what containment costs per
//! declared-method call; the design probe measured it at +1.0 ns.
//!
//! Run: `targo --unverified run --release -p aterm-objc --example objc_containment_bench`

#![cfg(target_os = "macos")]

use std::hint::black_box;
use std::time::Instant;

use aterm_objc::{Id, MainThread, Sel, msg, sel};

aterm_objc::declare_class! {
    struct ContainmentBench: NSObject {
        const NAME: &str = "ATermContainmentBench";
        type Ivars = ();

        @sel(contained)
        fn contained(&self) -> i64 {
            42
        }

        @sel(uncontained)
        @abort_on_exception
        fn uncontained(&self) -> i64 {
            42
        }
    }
}

/// The trampoline-free IMP, for the floor.
unsafe extern "C-unwind" fn bare(_this: Id, _cmd: Sel) -> i64 {
    42
}

fn bench(label: &str, obj: Id, s: Sel, n: u64) {
    // SAFETY: `s` is `q@:` on `obj`.
    let f: unsafe extern "C-unwind" fn(Id, Sel) -> i64 = unsafe { msg() };
    let mut best = f64::MAX;
    for _ in 0..3 {
        let t = Instant::now();
        let mut acc = 0i64;
        for _ in 0..n {
            // SAFETY: as above.
            acc = acc.wrapping_add(unsafe { f(obj, s) });
        }
        let dt = t.elapsed().as_secs_f64();
        black_box(acc);
        best = best.min(dt * 1e9 / n as f64);
    }
    println!("{label:<58} {best:7.3} ns/call  (n={n}, best of 3)");
}

fn main() {
    let n: u64 = std::env::args()
        .nth(1)
        .and_then(|a| a.parse().ok())
        .unwrap_or(200_000_000);
    // SAFETY: the example's main thread; nothing here is AppKit state.
    let mtm = unsafe { MainThread::new_unchecked() };
    let obj = ContainmentBench::alloc_init(mtm, ()).expect("alloc_init");
    // The bare IMP goes on a second class so the declared one stays as
    // `declare_class!` registered it.
    let bare_obj = {
        let mut b = aterm_objc::begin(c"NSObject", c"ATermContainmentBenchBare");
        b.add_rust_ivar::<()>();
        // SAFETY: `bare` is `(id, SEL) -> long`, matching `q@:`.
        unsafe {
            b.add_method(sel!(answer), bare as *const std::ffi::c_void, "q@:");
        }
        let meta = b.register();
        // SAFETY: `+new` on a registered NSObject subclass.
        unsafe { aterm_objc::send::send_id(meta.class().as_id(), sel!(new)) }
    };
    println!("declared-method call cost by trampoline shape (IMP returns 42):");
    bench(
        "IMP = bare Rust extern \"C-unwind\" fn (no trampoline)",
        bare_obj,
        sel!(answer),
        n,
    );
    bench(
        "IMP = declare_class! @abort_on_exception (catch_unwind only)",
        obj.as_id(),
        sel!(uncontained),
        n,
    );
    bench(
        "IMP = declare_class! default (catch_unwind + @try containment)",
        obj.as_id(),
        sel!(contained),
        n,
    );
    println!("containment count: {}", aterm_objc::contained_count());
}
