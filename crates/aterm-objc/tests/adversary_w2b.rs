// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! W2 second pass, part two: the claims nobody armed.

#![cfg(target_os = "macos")]
// This file USED to carry `#![allow(clippy::too_many_arguments)]`, because
// `declare_class!` emits each body as an inherent `fn` carrying every declared
// argument and the ten-colon selector below therefore tripped the lint with its
// span inside the expansion — where no `#[allow]` a caller writes can reach.
// The macro now emits the allow itself, alongside the `non_snake_case` and
// `dead_code` it already emitted. The absence of the attribute here IS F9's
// arming: put the ten-colon method back under a bare `cargo clippy` and the
// lint fires again if the macro stops emitting it.

use std::ffi::{CStr, c_char, c_void};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use aterm_objc::{Bool, ClassPtr, ClassType, Id, Sel, class, declare_class, msg, sel};

/// The [`MainThread`](aterm_objc::MainThread) witness every instantiation owes,
/// for a test that is not on the main thread and does not need to be.
///
/// libtest runs every test on a worker (`pthread_main_np()` is 0 there even
/// under `--test-threads=1`), so `MainThread::new()` correctly answers `None`
/// here and the checked constructor is unusable. Every class in this file is an
/// `NSObject`/`NSView` subclass that touches no AppKit state, is instantiated
/// and released on the SAME worker, and whose `Ivars` are therefore dropped on
/// the thread that made them — which is the second form of `new_unchecked`'s
/// obligation, written once instead of at every call site.
fn mtm() -> aterm_objc::MainThread {
    // SAFETY: see this function's doc comment — the class has no main-thread
    // affinity and its ivars are born and dropped on this one worker.
    unsafe { aterm_objc::MainThread::new_unchecked() }
}

/// An ivar that needs 16-byte alignment — `class_addIvar` is told the
/// alignment as a log2, and nothing else in the crate exercises a value > 3.
#[repr(align(16))]
struct AlignedIvars {
    marker: u64,
    drops: Arc<AtomicUsize>,
}
impl Drop for AlignedIvars {
    fn drop(&mut self) {
        self.drops.fetch_add(1, Ordering::SeqCst);
    }
}

declare_class! {
    struct Aligned: NSObject {
        const NAME: &str = "ATermObjcAlignedW2";
        type Ivars = AlignedIvars;

        @sel(marker)
        fn marker(&self) -> u64 {
            let ivars = self.ivars();
            assert_eq!(
                std::ptr::from_ref(ivars).addr() % 16,
                0,
                "the runtime placed a 16-aligned ivar at a misaligned offset"
            );
            ivars.marker
        }
    }
}

declare_class! {
    /// No methods at all, and a zero-sized ivar — the degenerate expansion.
    struct Empty: NSObject {
        const NAME: &str = "ATermObjcEmptyW2";
        type Ivars = ();
    }
}

declare_class! {
    /// TEN colons — the widest selector `msg` can send, because `MsgFn` is
    /// implemented up to twelve PARAMETERS and two of those are `self` and
    /// `_cmd`. Eleven arguments still declares and still registers; it just
    /// cannot be sent from Rust. See the report.
    struct Wide: NSObject {
        const NAME: &str = "ATermObjcWideW2";
        type Ivars = ();

        @sel(a:b:c:d:e:f:g:h:i:j:)
        fn wide(
            &self,
            a: i64, b: i64, c: i64, d: i64, e: i64,
            f: i64, g: i64, h: i64, i: i64, j: i64,
        ) -> i64 {
            a + b * 2 + c * 3 + d * 4 + e * 5
                + f * 6 + g * 7 + h * 8 + i * 9 + j * 10
        }
    }
}

#[test]
fn a_sixteen_byte_aligned_ivar_lands_aligned_and_drops_once() {
    let drops = Arc::new(AtomicUsize::new(0));
    {
        let obj = Aligned::alloc_init(
            mtm(),
            AlignedIvars {
                marker: 0xDEAD_BEEF,
                drops: Arc::clone(&drops),
            },
        )
        .expect("+alloc/-init");
        // SAFETY: `-marker` is `-(unsigned long long)` on a live instance.
        unsafe {
            let f: unsafe extern "C" fn(Id, Sel) -> u64 = msg();
            assert_eq!(f(obj.as_id(), sel!(marker)), 0xDEAD_BEEF);
        }
    }
    assert_eq!(drops.load(Ordering::SeqCst), 1);
}

#[test]
fn a_class_with_no_methods_and_a_zero_sized_ivar_still_registers() {
    let cls = Empty::class();
    assert!(!cls.is_null());
    let obj = Empty::alloc_init(mtm(), ()).expect("+alloc/-init");
    // SAFETY: `-description` is `-(NSString *)` on a live instance; the result
    // is autoreleased and only read inside the pool.
    let d = aterm_objc::autoreleasepool(|_| unsafe {
        let f: unsafe extern "C" fn(Id, Sel) -> Id = msg();
        aterm_objc::ns_string_to_rust(f(obj.as_id(), sel!(description)))
    });
    assert!(d.contains("ATermObjcEmptyW2"), "description was {d:?}");
}

#[test]
fn a_ten_colon_selector_counts_and_dispatches() {
    let obj = Wide::alloc_init(mtm(), ()).expect("+alloc/-init");
    // SAFETY: the exact declared prototype, on a live instance.
    unsafe {
        let f: unsafe extern "C" fn(
            Id,
            Sel,
            i64,
            i64,
            i64,
            i64,
            i64,
            i64,
            i64,
            i64,
            i64,
            i64,
        ) -> i64 = msg();
        // 1+2+...+10 = 55. Anything shifted gives a different number.
        assert_eq!(
            f(
                obj.as_id(),
                sel!(a:b:c:d:e:f:g:h:i:j:),
                1,
                1,
                1,
                1,
                1,
                1,
                1,
                1,
                1,
                1
            ),
            55
        );
        assert_eq!(
            f(
                obj.as_id(),
                sel!(a:b:c:d:e:f:g:h:i:j:),
                1,
                0,
                0,
                0,
                0,
                0,
                0,
                0,
                0,
                0
            ),
            1
        );
        assert_eq!(
            f(
                obj.as_id(),
                sel!(a:b:c:d:e:f:g:h:i:j:),
                0,
                0,
                0,
                0,
                0,
                0,
                0,
                0,
                0,
                1
            ),
            10
        );
    }
    unsafe extern "C" {
        fn class_getInstanceMethod(cls: ClassPtr, sel: Sel) -> *const c_void;
        fn method_getTypeEncoding(m: *const c_void) -> *const c_char;
    }
    // SAFETY: side-effect-free runtime queries on a registered class.
    let enc = unsafe {
        let m = class_getInstanceMethod(
            Wide::class(),
            aterm_objc::sel_uncached(c"a:b:c:d:e:f:g:h:i:j:"),
        );
        assert!(!m.is_null());
        CStr::from_ptr(method_getTypeEncoding(m))
            .to_string_lossy()
            .into_owned()
    };
    assert_eq!(enc, "q@:qqqqqqqqqq");
}

#[test]
fn concurrent_first_registration_produces_one_class() {
    // `class()` is a `OnceLock` around `objc_allocateClassPair` +
    // `objc_registerClassPair`. Two threads racing it must not register two
    // pairs — the second `objc_allocateClassPair` would return nil and `begin`
    // would panic with "already registered".
    const N: usize = 8;
    let barrier = Arc::new(std::sync::Barrier::new(N));
    let handles: Vec<_> = (0..N)
        .map(|_| {
            let b = Arc::clone(&barrier);
            std::thread::spawn(move || {
                b.wait();
                Wide::class().addr()
            })
        })
        .collect();
    let addrs: Vec<usize> = handles.into_iter().map(|h| h.join().unwrap()).collect();
    assert!(addrs.iter().all(|a| *a == addrs[0]), "{addrs:?}");
    assert_eq!(addrs[0], class(c"ATermObjcWideW2").addr());
}

/// Set in the child so it mints an instance the way Objective-C would.
const OBJC_MINTED_CHILD: &str = "ATERM_OBJC_W2_MINTED_CHILD";
const SIGABRT: i32 = 6;

#[test]
fn an_instance_objective_c_minted_is_caught_by_the_ivar_flag() {
    // W2's claim: the initialised flag is checked in RELEASE builds too,
    // because the class is registered under a public name and ObjC code can
    // `+alloc`/`-init` it without ever reaching `alloc_init`. That instance's
    // ivar slot is all zeros; reading it as an `Arc` would be undefined
    // behaviour. The assert turns it into a named abort instead.
    use std::os::unix::process::ExitStatusExt as _;
    if std::env::var_os(OBJC_MINTED_CHILD).is_some() {
        // SAFETY: `+alloc` and `-init` on a registered class, then a message
        // the class declares. This is EXACTLY what a nib or an
        // `NSClassFromString` would do, and it is what the flag guards.
        // Register the class the normal way, then reach it BY NAME through the
        // runtime and build an instance without `alloc_init` — which is what a
        // nib or an `NSClassFromString` does.
        let _ = Aligned::class();
        unsafe {
            let cls = class(c"ATermObjcAlignedW2");
            assert!(!cls.is_null());
            let alloc: unsafe extern "C" fn(ClassPtr, Sel) -> Id = msg();
            let raw = alloc(cls, sel!(alloc));
            let init: unsafe extern "C" fn(Id, Sel) -> Id = msg();
            let obj = init(raw, sel!(init));
            let f: unsafe extern "C" fn(Id, Sel) -> u64 = msg();
            let v = f(obj, sel!(marker));
            println!("SURVIVED-THE-UNWRITTEN-IVAR v={v}");
        }
        return;
    }
    // Force the class to exist in the child by touching it here too.
    let _ = Aligned::class();
    let exe = std::env::current_exe().expect("test binary path");
    let out = std::process::Command::new(exe)
        .args([
            "--exact",
            "--nocapture",
            "an_instance_objective_c_minted_is_caught_by_the_ivar_flag",
        ])
        .env(OBJC_MINTED_CHILD, "1")
        .output()
        .expect("re-run as a child");
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        !stdout.contains("SURVIVED-THE-UNWRITTEN-IVAR"),
        "an unwritten ivar slot was read as a live value\nstdout: {stdout}"
    );
    assert_eq!(
        out.status.signal(),
        Some(SIGABRT),
        "expected the named abort\nstdout: {stdout}\nstderr: {stderr}"
    );
    assert!(
        stderr.contains("ivars read before they were written"),
        "aborted, but not through the ivar flag:\n{stderr}"
    );
}

#[test]
fn a_bool_argument_is_never_materialised_as_a_rust_bool() {
    // D3's value half, on the arch that can execute it. `Bool` is a byte and
    // `as_bool` is `!= 0` on x86_64, so no byte is an invalid bit pattern.
    // Here the only thing arm64 can show is that the round trip is exact.
    assert_eq!(size_of::<Bool>(), 1);
    assert_eq!(align_of::<Bool>(), 1);
    assert!(Bool::YES.as_bool());
    assert!(!Bool::NO.as_bool());
    // And that the crate exposes no `Encode for bool` — checked at compile
    // time by `tests/` failing to build if it did; asserted here in prose.
    assert_eq!(
        <Bool as aterm_objc::Encode>::ENCODING,
        if cfg!(target_arch = "aarch64") {
            "B"
        } else {
            "c"
        }
    );
}
