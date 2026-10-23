// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Runtime registration, `objc_msgSend` dispatch, and reflection tests for declared classes.

#![cfg(target_os = "macos")]
#![expect(
    clippy::disallowed_methods,
    reason = "aterm-objc's own runtime tests drive the bare `msg` cast; everything else uses checked_msg"
)]

use std::cell::Cell;
use std::ffi::CStr;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use aterm_objc::{
    Bool, CGPoint, CGRect, CGSize, ClassPtr, ClassType, Encode, Id, ObjcSuper, Sel, declare_class,
    msg, msg_super, sel,
};

/// The [`MainThread`](aterm_objc::MainThread) witness every instantiation owes, minted unchecked.
fn mtm() -> aterm_objc::MainThread {
    // SAFETY: see the doc comment above.
    unsafe { aterm_objc::MainThread::new_unchecked() }
}

/// Ivars with an observable `Drop`, so the tests count what the generated `dealloc` drops.
struct Ivars {
    calls: Cell<i64>,
    drops: Arc<AtomicUsize>,
}

impl Drop for Ivars {
    fn drop(&mut self) {
        self.drops.fetch_add(1, Ordering::SeqCst);
    }
}

declare_class! {
    /// A test class with every shape the real sites use.
    struct Probe: NSObject {
        const NAME: &str = "ATermObjcProbe";
        type Ivars = Ivars;
        protocols: [NSObject];

        @sel(bump)
        fn bump(&self) {
            let ivars = self.ivars();
            ivars.calls.set(ivars.calls.get() + 1);
        }

        @sel(bumpBy:)
        fn bump_by(&self, n: i64) {
            let ivars = self.ivars();
            ivars.calls.set(ivars.calls.get() + n);
        }

        @sel(callCount)
        fn call_count(&self) -> i64 {
            self.ivars().calls.get()
        }

        @sel(isPositive:)
        fn is_positive(&self, n: i64) -> Bool {
            Bool::new(n > 0)
        }

        @sel(addFirst:second:)
        fn add_two(&self, a: i64, b: i64) -> i64 {
            a + b
        }

        /// Three float arguments.
        @sel(blendRed:green:blue:)
        fn blend_three(&self, r: f64, g: f64, b: f64) -> f64 {
            r * 100.0 + g * 10.0 + b
        }

        /// The signature of `aterm-gui/src/toolbar.rs`'s
        /// `-(BOOL)control:textView:doCommandBySelector:` override.
        @sel(control:textView:doCommandBySelector:)
        fn control_text_view(&self, _control: Id, _view: Id, command: Sel) -> Bool {
            Bool::new(command == sel!(insertNewline:))
        }

        /// The signature of `aterm-gui/src/toolbar.rs`'s `-(NSToolbarItem
        /// *)toolbar:itemForItemIdentifier:willBeInsertedIntoToolbar:`.
        @sel(toolbar:itemForItemIdentifier:willBeInsertedIntoToolbar:)
        fn toolbar_item(&self, _toolbar: Id, _identifier: Id, inserted: Bool) -> Id {
            let ivars = self.ivars();
            ivars.calls.set(ivars.calls.get() + i64::from(inserted.as_bool()));
            match Probe::alloc_init(mtm(), Ivars {
                calls: Cell::new(0),
                drops: Arc::clone(&ivars.drops),
            }) {
                Some(child) => child.autorelease(),
                None => Id::NIL,
            }
        }

        /// A 32-byte struct return.
        @sel(bigRect)
        fn big_rect(&self) -> CGRect {
            CGRect {
                origin: CGPoint { x: 1.0, y: 2.0 },
                size: CGSize {
                    width: 3.0,
                    height: 4.0,
                },
            }
        }

        @sel(areaOfRect:)
        fn area_of_rect(&self, r: CGRect) -> f64 {
            r.size.width * r.size.height
        }

        /// Overrides `-hash`, XOR-ing the superclass's answer.
        @sel(hash)
        fn hash_override(&self) -> usize {
            self.super_hash() ^ 0xABCD
        }

        @sel(superHash)
        fn super_hash(&self) -> usize {
            let sup = self.super_receiver();
            // SAFETY: `-hash` is `-(NSUInteger)hash`.
            unsafe {
                let f: unsafe extern "C-unwind" fn(*const ObjcSuper, Sel) -> usize = msg_super();
                f(&raw const sup, sel!(hash))
            }
        }
    }
}

fn probe(drops: &Arc<AtomicUsize>) -> aterm_objc::Retained<Probe> {
    Probe::alloc_init(
        mtm(),
        Ivars {
            calls: Cell::new(0),
            drops: Arc::clone(drops),
        },
    )
    .expect("+alloc/-init produced an instance")
}

#[test]
fn messages_reach_the_rust_bodies_through_objc_msg_send() {
    let drops = Arc::new(AtomicUsize::new(0));
    let obj = probe(&drops);
    // SAFETY: every prototype below is the exact C signature of the selector declared above.
    unsafe {
        let void0: unsafe extern "C-unwind" fn(Id, Sel) = msg();
        let void_i64: unsafe extern "C-unwind" fn(Id, Sel, i64) = msg();
        let ret_i64: unsafe extern "C-unwind" fn(Id, Sel) -> i64 = msg();
        let ret_bool: unsafe extern "C-unwind" fn(Id, Sel, i64) -> Bool = msg();
        let ret_f64: unsafe extern "C-unwind" fn(Id, Sel, CGRect) -> f64 = msg();

        void0(obj.as_id(), sel!(bump));
        void0(obj.as_id(), sel!(bump));
        void_i64(obj.as_id(), sel!(bumpBy:), 40);
        assert_eq!(ret_i64(obj.as_id(), sel!(callCount)), 42);

        assert!(ret_bool(obj.as_id(), sel!(isPositive:), 1).as_bool());
        assert!(!ret_bool(obj.as_id(), sel!(isPositive:), -1).as_bool());

        let r = CGRect {
            origin: CGPoint { x: 1.0, y: 2.0 },
            size: CGSize {
                width: 3.0,
                height: 4.0,
            },
        };
        assert!((ret_f64(obj.as_id(), sel!(areaOfRect:), r) - 12.0).abs() < f64::EPSILON);
    }
}

#[test]
fn the_super_send_starts_above_this_class() {
    let drops = Arc::new(AtomicUsize::new(0));
    let obj = probe(&drops);
    // SAFETY: `-hash` and `-superHash` are both `-(NSUInteger)`; `obj` is live.
    unsafe {
        let hash: unsafe extern "C-unwind" fn(Id, Sel) -> usize = msg();
        let ours = hash(obj.as_id(), sel!(hash));
        let supers = hash(obj.as_id(), sel!(superHash));
        assert_ne!(supers, 0, "NSObject's -hash returned 0");
        assert_eq!(ours, supers ^ 0xABCD);
        assert_eq!(supers, obj.as_id().addr());
    }
}

#[test]
fn the_type_encodings_are_what_the_runtime_reports() {
    let cls = Probe::class();
    // SAFETY: runtime method/encoding queries on a registered class.
    let encoding = |name: &'static CStr| -> String {
        unsafe extern "C" {
            fn class_getInstanceMethod(cls: ClassPtr, sel: Sel) -> *const std::ffi::c_void;
            fn method_getTypeEncoding(m: *const std::ffi::c_void) -> *const std::ffi::c_char;
        }
        unsafe {
            let m = class_getInstanceMethod(cls, aterm_objc::sel_uncached(name));
            assert!(!m.is_null(), "no method {name:?}");
            CStr::from_ptr(method_getTypeEncoding(m))
                .to_string_lossy()
                .into_owned()
        }
    };
    assert_eq!(encoding(c"bump"), "v@:");
    assert_eq!(encoding(c"bumpBy:"), "v@:q");
    assert_eq!(encoding(c"callCount"), "q@:");
    assert_eq!(
        encoding(c"isPositive:"),
        format!("{}@:q", Bool::ENCODING),
        "BOOL is \"B\" on arm64 and \"c\" on the x86_64 compat slice"
    );
    assert_eq!(encoding(c"addFirst:second:"), "q@:qq");
    assert_eq!(encoding(c"blendRed:green:blue:"), "d@:ddd");
    assert_eq!(
        encoding(c"control:textView:doCommandBySelector:"),
        format!("{}@:@@:", Bool::ENCODING)
    );
    assert_eq!(
        encoding(c"toolbar:itemForItemIdentifier:willBeInsertedIntoToolbar:"),
        format!("@@:@@{}", Bool::ENCODING)
    );
    assert_eq!(encoding(c"bigRect"), "{CGRect={CGPoint=dd}{CGSize=dd}}@:");
    assert_eq!(
        encoding(c"areaOfRect:"),
        "d@:{CGRect={CGPoint=dd}{CGSize=dd}}"
    );
    assert_eq!(encoding(c"dealloc"), "v@:");
}

#[test]
fn dealloc_drops_the_rust_ivars_exactly_once() {
    let drops = Arc::new(AtomicUsize::new(0));
    {
        let obj = probe(&drops);
        // SAFETY: a live instance; `-bump` is `-(void)`.
        unsafe {
            let void0: unsafe extern "C-unwind" fn(Id, Sel) = msg();
            void0(obj.as_id(), sel!(bump));
        }
        assert_eq!(drops.load(Ordering::SeqCst), 0, "dropped while still alive");
    }
    assert_eq!(
        drops.load(Ordering::SeqCst),
        1,
        "the generated -dealloc did not drop the ivars exactly once"
    );
}

#[test]
fn an_extra_retain_defers_the_drop() {
    let drops = Arc::new(AtomicUsize::new(0));
    let obj = probe(&drops);
    let second = obj.clone_retained();
    drop(obj);
    assert_eq!(drops.load(Ordering::SeqCst), 0, "released at +2");
    drop(second);
    assert_eq!(drops.load(Ordering::SeqCst), 1);
}

#[test]
fn the_main_thread_witness_refuses_a_worker_and_a_spawned_thread() {
    assert!(
        aterm_objc::MainThread::new().is_none(),
        "libtest workers are not the process main thread; a witness minted here \
         would be a rubber stamp"
    );

    let on_spawned = std::thread::spawn(|| aterm_objc::MainThread::new().is_none())
        .join()
        .expect("the probe thread does not panic");
    assert!(on_spawned, "a spawned thread is never the main thread");

    assert!(!Probe::class().is_null());
}

#[test]
fn the_whole_registered_table_comes_back_with_its_encodings() {
    let cls = Probe::class();
    // SAFETY: `Probe::class()` is a live, registered class object.
    let rows = unsafe { aterm_objc::class_methods(cls) };

    for (selector, encoding) in &rows {
        // SAFETY: `cls` is live and `selector` came out of its own table.
        let one_at_a_time = unsafe { aterm_objc::method_types(cls, *selector) };
        assert_eq!(
            encoding.as_deref(),
            one_at_a_time.as_deref(),
            "{:?}: the table and method_types disagree",
            selector
        );
    }

    let names: Vec<String> = rows
        .iter()
        .map(|(s, _)| s.name().to_string_lossy().into_owned())
        .collect();
    for declared in [
        "bump",
        "bumpBy:",
        "callCount",
        "isPositive:",
        "addFirst:second:",
        "blendRed:green:blue:",
        "control:textView:doCommandBySelector:",
        "toolbar:itemForItemIdentifier:willBeInsertedIntoToolbar:",
        "bigRect",
        "areaOfRect:",
        "hash",
        "superHash",
        "dealloc",
    ] {
        assert!(
            names.iter().any(|n| n == declared),
            "{declared} was declared but is not in the registered table: {names:?}"
        );
    }

    let is_positive = rows
        .iter()
        .find(|(s, _)| s.name() == c"isPositive:")
        .and_then(|(_, e)| e.clone())
        .expect("isPositive: is registered with types");
    assert_eq!(
        aterm_objc::strip_method_offsets(&is_positive),
        format!("{}@:q", Bool::ENCODING)
    );

    assert!(
        !names.iter().any(|n| n == "description"),
        "class_copyMethodList must report only the class's OWN methods"
    );
    // SAFETY: `cls` is live; `sel!` interns the selector.
    assert!(unsafe { aterm_objc::method_types(cls, sel!(description)) }.is_some());
}

#[test]
fn the_conformance_claim_is_readable_and_is_the_classs_own() {
    let cls = Probe::class();
    // SAFETY: a live, registered class object.
    let claimed = unsafe { aterm_objc::class_protocols(cls) };
    assert!(
        claimed.iter().any(|p| p == "NSObject"),
        "Probe's `protocols:` list names NSObject; the runtime says {claimed:?}"
    );

    assert!(
        !claimed.iter().any(|p| p == "NSDraggingDestination"),
        "a protocol nobody added must not appear: {claimed:?}"
    );

    let drops = Arc::new(AtomicUsize::new(0));
    let obj = probe(&drops);
    // SAFETY: `-conformsToProtocol:` is `B@:@`.
    let conforms: Bool = unsafe {
        let f: unsafe extern "C-unwind" fn(Id, Sel, aterm_objc::ProtocolPtr) -> Bool = msg();
        f(
            obj.as_id(),
            sel!(conformsToProtocol:),
            aterm_objc::protocol(c"NSObject"),
        )
    };
    assert!(conforms.as_bool());
}
