// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Drives a REAL runtime-declared class: registers it, messages it through
//! `objc_msgSend` (not through Rust), and checks the runtime's own view of what
//! was built. A test that only proves the macro expands would prove nothing —
//! the whole risk here is what `libobjc` does with the bytes.

#![cfg(target_os = "macos")]

use std::cell::Cell;
use std::ffi::CStr;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use aterm_objc::{
    Bool, CGPoint, CGRect, CGSize, ClassPtr, ClassType, Encode, Id, ObjcSuper, Sel,
    autoreleasepool, class, class_name, class_of, declare_class, msg, msg_super, sel,
    superclass_of,
};

/// Ivars with a `Drop` that is observable from the test, so "the generated
/// `dealloc` drops the Rust ivars" is a MEASURED fact, not a claim.
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
    /// A test class exercising every shape the seven real sites use: an ivar
    /// with a destructor, a void method, an integer argument, a `BOOL` return,
    /// a struct-by-value argument, and a `[super …]` send.
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

        /// TWO arguments. Before D1 was closed this line alone failed the whole
        /// crate with `error: no rules expected ';'`, because the expansion
        /// emitted `Ret ; A ; B` at an encoding macro that matches one
        /// semicolon and then commas.
        @sel(addFirst:second:)
        fn add_two(&self, a: i64, b: i64) -> i64 {
            a + b
        }

        /// THREE arguments, all floats, so a wrong argument shift shows up as a
        /// wrong number rather than as a crash.
        @sel(blendRed:green:blue:)
        fn blend_three(&self, r: f64, g: f64, b: f64) -> f64 {
            r * 100.0 + g * 10.0 + b
        }

        /// The exact signature of `aterm-gui/src/toolbar.rs:3990`,
        /// `-(BOOL)control:textView:doCommandBySelector:` — three arguments of
        /// three different encodings and a `BOOL` return, which is one of the
        /// two real sites D1 blocked.
        @sel(control:textView:doCommandBySelector:)
        fn control_text_view(&self, _control: Id, _view: Id, command: Sel) -> Bool {
            Bool::new(command == sel!(insertNewline:))
        }

        /// The exact signature of `aterm-gui/src/toolbar.rs:3379`,
        /// `-(NSToolbarItem *)toolbar:itemForItemIdentifier:willBeInsertedIntoToolbar:`:
        /// the other site D1 blocked, and the one that ALSO needs D5. It
        /// returns a brand-new object, so it owes its caller +0 — hence
        /// `autorelease`, not `into_raw` (a leak) and not a borrowed pointer to
        /// something about to drop (a dangling one).
        @sel(toolbar:itemForItemIdentifier:willBeInsertedIntoToolbar:)
        fn toolbar_item(&self, _toolbar: Id, _identifier: Id, inserted: Bool) -> Id {
            let ivars = self.ivars();
            ivars.calls.set(ivars.calls.get() + i64::from(inserted.as_bool()));
            match Probe::alloc_init(Ivars {
                calls: Cell::new(0),
                drops: Arc::clone(&ivars.drops),
            }) {
                Some(child) => child.autorelease(),
                None => Id::NIL,
            }
        }

        /// A 32-byte struct return — the shape that goes through
        /// `objc_msgSend_stret` on the x86_64 compat slice and through plain
        /// `objc_msgSend` with an `x8` result pointer on arm64.
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

        /// Overrides `-hash`, XOR-ing the superclass's answer. If the super
        /// send resolved to THIS method instead of `NSObject`'s, the process
        /// would recurse until the stack died — so a returned value is itself
        /// proof that `objc_msgSendSuper` started lookup above this class.
        @sel(hash)
        fn hash_override(&self) -> usize {
            self.super_hash() ^ 0xABCD
        }

        @sel(superHash)
        fn super_hash(&self) -> usize {
            let sup = self.super_receiver();
            // SAFETY: `-hash` is `-(NSUInteger)hash`, exactly the prototype
            // below, and `sup` names this class's superclass as the place to
            // start lookup.
            unsafe {
                let f: unsafe extern "C" fn(*const ObjcSuper, Sel) -> usize = msg_super();
                f(&raw const sup, sel!(hash))
            }
        }
    }
}

fn probe(drops: &Arc<AtomicUsize>) -> aterm_objc::Retained<Probe> {
    Probe::alloc_init(Ivars {
        calls: Cell::new(0),
        drops: Arc::clone(drops),
    })
    .expect("+alloc/-init produced an instance")
}

#[test]
fn the_class_is_registered_with_the_runtime() {
    let cls = Probe::class();
    assert!(!cls.is_null());
    // Looked up BY NAME through the runtime, not through our own handle: this
    // is the runtime's table answering, not our `OnceLock`.
    let by_name: ClassPtr = class(c"ATermObjcProbe");
    assert_eq!(
        by_name, cls,
        "objc_getClass disagrees with the registered pair"
    );
    // SAFETY: `cls` is the class this crate just registered, so it and its
    // superclass are live, immortal class objects.
    unsafe {
        assert_eq!(class_name(cls), c"ATermObjcProbe");
        assert_eq!(class_name(superclass_of(cls)), c"NSObject");
    }
    assert_eq!(<Probe as ClassType>::NAME, "ATermObjcProbe");
}

#[test]
fn an_instance_is_of_that_class_and_responds_to_its_selectors() {
    let drops = Arc::new(AtomicUsize::new(0));
    let obj = probe(&drops);
    // SAFETY: `obj` is a live instance; `object_getClass` and
    // `respondsToSelector:` are plain side-effect-free runtime queries.
    unsafe {
        assert_eq!(class_of(obj.as_id()), Probe::class());
        let responds: unsafe extern "C" fn(Id, Sel, Sel) -> Bool = msg();
        for name in [
            c"bump",
            c"bumpBy:",
            c"callCount",
            c"isPositive:",
            c"areaOfRect:",
            c"superHash",
            c"hash",
            c"dealloc",
            c"addFirst:second:",
            c"blendRed:green:blue:",
            c"control:textView:doCommandBySelector:",
            c"toolbar:itemForItemIdentifier:willBeInsertedIntoToolbar:",
            c"bigRect",
        ] {
            assert!(
                responds(
                    obj.as_id(),
                    sel!(respondsToSelector:),
                    aterm_objc::sel_uncached(name)
                )
                .as_bool(),
                "instance does not respond to {name:?}"
            );
        }
        // A selector we never declared must NOT be claimed.
        assert!(
            !responds(
                obj.as_id(),
                sel!(respondsToSelector:),
                aterm_objc::sel_uncached(c"neverDeclared")
            )
            .as_bool()
        );
    }
}

#[test]
fn messages_reach_the_rust_bodies_through_objc_msg_send() {
    let drops = Arc::new(AtomicUsize::new(0));
    let obj = probe(&drops);
    // SAFETY: every prototype below is the exact C signature of the selector
    // declared above, and `obj` is a live instance of the class that declares it.
    unsafe {
        let void0: unsafe extern "C" fn(Id, Sel) = msg();
        let void_i64: unsafe extern "C" fn(Id, Sel, i64) = msg();
        let ret_i64: unsafe extern "C" fn(Id, Sel) -> i64 = msg();
        let ret_bool: unsafe extern "C" fn(Id, Sel, i64) -> Bool = msg();
        let ret_f64: unsafe extern "C" fn(Id, Sel, CGRect) -> f64 = msg();

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
        let hash: unsafe extern "C" fn(Id, Sel) -> usize = msg();
        let ours = hash(obj.as_id(), sel!(hash));
        let supers = hash(obj.as_id(), sel!(superHash));
        assert_ne!(supers, 0, "NSObject's -hash returned 0");
        assert_eq!(ours, supers ^ 0xABCD);
        // NSObject's -hash is the instance pointer.
        assert_eq!(supers, obj.as_id().addr());
    }
}

#[test]
fn the_type_encodings_are_what_the_runtime_reports() {
    let cls = Probe::class();
    // SAFETY: `class_getInstanceMethod` + `method_getTypeEncoding` are plain
    // runtime queries on a registered class; the returned string is owned by
    // the runtime and outlives the read.
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
    // The arities D1 blocked, read back from the RUNTIME's own table.
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
fn the_declared_protocol_is_registered() {
    let cls = Probe::class();
    // SAFETY: `conformsToProtocol:` is a side-effect-free class-level query and
    // `NSObject` is a protocol libobjc always defines.
    unsafe {
        let conforms: unsafe extern "C" fn(ClassPtr, Sel, aterm_objc::ProtocolPtr) -> Bool = msg();
        assert!(
            conforms(
                cls,
                sel!(conformsToProtocol:),
                aterm_objc::protocol(c"NSObject")
            )
            .as_bool()
        );
    }
}

#[test]
fn dealloc_drops_the_rust_ivars_exactly_once() {
    let drops = Arc::new(AtomicUsize::new(0));
    {
        let obj = probe(&drops);
        // SAFETY: a live instance; `-bump` is `-(void)`.
        unsafe {
            let void0: unsafe extern "C" fn(Id, Sel) = msg();
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
fn ivars_survive_a_round_trip_through_the_runtime() {
    // The instance is handed to ObjC as a bare `id`, comes back as a bare `id`,
    // and its Rust ivars are still reachable — which is the property the whole
    // ivar-offset derivation exists to provide.
    let drops = Arc::new(AtomicUsize::new(0));
    let obj = probe(&drops);
    // SAFETY: `-self` returns the receiver; the result is the same live
    // instance, so re-adopting it as a borrowed reference is sound.
    unsafe {
        let identity: unsafe extern "C" fn(Id, Sel) -> Id = msg();
        let back = identity(obj.as_id(), sel!(self));
        assert_eq!(back, obj.as_id());
        let void_i64: unsafe extern "C" fn(Id, Sel, i64) = msg();
        void_i64(back, sel!(bumpBy:), 7);
    }
    assert_eq!(obj.ivars().calls.get(), 7);
}

#[test]
fn two_and_three_argument_methods_reach_their_bodies_with_the_arguments_in_order() {
    // D1's arming. The failure this closes was a COMPILE error, so the fact
    // that this file builds is half the proof; the other half is that the
    // arguments arrive in the right registers, which a wrong encoding or a
    // wrong trampoline prototype would scramble rather than reject.
    let drops = Arc::new(AtomicUsize::new(0));
    let obj = probe(&drops);
    // SAFETY: each prototype below is the exact C signature of the selector
    // declared above, on a live instance of the class that declares it.
    unsafe {
        let two: unsafe extern "C" fn(Id, Sel, i64, i64) -> i64 = msg();
        assert_eq!(two(obj.as_id(), sel!(addFirst:second:), 40, 2), 42);
        // Order matters and subtraction would hide it, so the check is
        // asymmetric on purpose.
        assert_eq!(two(obj.as_id(), sel!(addFirst:second:), -1, 100), 99);

        let three: unsafe extern "C" fn(Id, Sel, f64, f64, f64) -> f64 = msg();
        let blended = three(obj.as_id(), sel!(blendRed:green:blue:), 1.0, 2.0, 3.0);
        assert!(
            (blended - 123.0).abs() < f64::EPSILON,
            "three float arguments arrived as {blended}, not 123 — a register shift"
        );

        // The real `toolbar.rs:3990` shape: object, object, selector, BOOL out.
        let cmd: unsafe extern "C" fn(Id, Sel, Id, Id, Sel) -> Bool = msg();
        assert!(
            cmd(
                obj.as_id(),
                sel!(control:textView:doCommandBySelector:),
                Id::NIL,
                Id::NIL,
                sel!(insertNewline:)
            )
            .as_bool()
        );
        assert!(
            !cmd(
                obj.as_id(),
                sel!(control:textView:doCommandBySelector:),
                Id::NIL,
                Id::NIL,
                sel!(insertTab:)
            )
            .as_bool()
        );
    }
}

#[test]
fn an_object_returning_method_hands_back_a_plus_zero_reference() {
    // D5's arming, and the second of the two sites D1 blocked. An
    // `-autorelease`d return must be: alive for the rest of the pool, released
    // exactly once when the pool pops, and never leaked.
    let drops = Arc::new(AtomicUsize::new(0));
    let obj = probe(&drops);

    autoreleasepool(|_| {
        // SAFETY: the prototype is
        // `-(id)toolbar:(id)t itemForItemIdentifier:(id)i willBeInsertedIntoToolbar:(BOOL)b`,
        // exactly as declared, on a live instance.
        let returned = unsafe {
            let f: unsafe extern "C" fn(Id, Sel, Id, Id, Bool) -> Id = msg();
            f(
                obj.as_id(),
                sel!(toolbar:itemForItemIdentifier:willBeInsertedIntoToolbar:),
                Id::NIL,
                Id::NIL,
                Bool::YES,
            )
        };
        assert!(!returned.is_null(), "the method returned nil");
        // The BOOL argument arrived as YES, not as garbage.
        assert_eq!(obj.ivars().calls.get(), 1);
        assert_eq!(
            drops.load(Ordering::SeqCst),
            0,
            "the returned object was freed before the pool popped — the method \
             returned a borrowed pointer to something it had already dropped"
        );
        // Still a live instance of the class, still messageable.
        // SAFETY: `returned` is the live autoreleased instance built above.
        unsafe {
            assert_eq!(class_of(returned), Probe::class());
            let void_i64: unsafe extern "C" fn(Id, Sel, i64) = msg();
            void_i64(returned, sel!(bumpBy:), 5);
            let ret_i64: unsafe extern "C" fn(Id, Sel) -> i64 = msg();
            assert_eq!(ret_i64(returned, sel!(callCount)), 5);
        }
    });

    assert_eq!(
        drops.load(Ordering::SeqCst),
        1,
        "the returned object was NOT released when the pool popped — a +1 leak, \
         which is what `Retained::into_raw` in this position would produce"
    );
}

#[test]
fn a_thirty_two_byte_struct_comes_back_intact() {
    // D4's arming on the half this box can EXECUTE. `CGRect` is 32 bytes, so
    // `returns_indirectly::<CGRect>()` is true on the x86_64 compat slice and
    // the send there must go through `objc_msgSend_stret`; on arm64 the result
    // pointer rides in `x8` and one entry point serves. Either way the value
    // that comes back must be the one the body built — a wrong entry point
    // shifts every argument and returns garbage.
    let drops = Arc::new(AtomicUsize::new(0));
    let obj = probe(&drops);
    // SAFETY: `-bigRect` is `-(NSRect)` with no arguments, on a live instance.
    let r = unsafe {
        let f: unsafe extern "C" fn(Id, Sel) -> CGRect = msg();
        f(obj.as_id(), sel!(bigRect))
    };
    assert_eq!(
        r,
        CGRect {
            origin: CGPoint { x: 1.0, y: 2.0 },
            size: CGSize {
                width: 3.0,
                height: 4.0
            },
        }
    );
}
