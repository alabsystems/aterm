// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Objective-C `NSException` containment and Rust panic / `-dealloc` abort tests.

#![cfg(target_os = "macos")]
#![expect(
    clippy::disallowed_methods,
    reason = "aterm-objc's own runtime tests drive the bare `msg` cast; everything else uses checked_msg"
)]

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

use aterm_objc::{
    Bool, ContainedException, Id, MainThread, NSRange, Sel, autoreleasepool, class,
    contained_count, msg, ns_string, sel,
};

/// One containment as the sink saw it.
#[derive(Debug, Clone)]
struct Seen {
    method: String,
    class: String,
    name: String,
    reason: String,
    call_stack: String,
    ordinal: u64,
}

fn seen() -> &'static Mutex<Vec<Seen>> {
    static SEEN: OnceLock<Mutex<Vec<Seen>>> = OnceLock::new();
    SEEN.get_or_init(|| Mutex::new(Vec::new()))
}

fn record(e: &ContainedException<'_>) {
    seen().lock().unwrap().push(Seen {
        method: e.method.to_owned(),
        class: e.class.to_owned(),
        name: e.name.to_owned(),
        reason: e.reason.to_owned(),
        call_stack: e.call_stack.to_owned(),
        ordinal: e.ordinal,
    });
    aterm_objc::exception::default_sink(e);
}

fn install_sink() {
    static ONCE: OnceLock<()> = OnceLock::new();
    ONCE.get_or_init(|| aterm_objc::exception::set_sink(record));
}

fn mtm() -> MainThread {
    // SAFETY: every test here builds and drops its objects on its own thread
    // and the class carries no main-thread AppKit state.
    unsafe { MainThread::new_unchecked() }
}

struct Tell(Arc<AtomicUsize>);
impl Drop for Tell {
    fn drop(&mut self) {
        self.0.fetch_add(1, Ordering::SeqCst);
    }
}

fn raise(name: &str, reason: &str) -> ! {
    let n = ns_string(name).expect("NSString");
    let r = ns_string(reason).expect("NSString");
    // SAFETY: `+exceptionWithName:reason:userInfo:` is `@@:@@@` on
    // `NSException` (autoreleased result); `-raise` is `v@:` and does not
    // return.
    unsafe {
        let make: unsafe extern "C-unwind" fn(Id, Sel, Id, Id, Id) -> Id = msg();
        let e = make(
            class(c"NSException").as_id(),
            sel!(exceptionWithName:reason:userInfo:),
            n.id(),
            r.id(),
            Id::NIL,
        );
        assert!(!e.is_null(), "NSException factory answered nil");
        aterm_objc::send::send_v(e, sel!(raise));
    }
    unreachable!("-[NSException raise] returned")
}

fn deep(drops: &Arc<AtomicUsize>, name: &str, reason: &str) -> i64 {
    let _outer = Tell(Arc::clone(drops));
    deeper(drops, name, reason)
}
fn deeper(drops: &Arc<AtomicUsize>, name: &str, reason: &str) -> i64 {
    let _inner = Tell(Arc::clone(drops));
    raise(name, reason)
}

struct Ivars {
    drops: Arc<AtomicUsize>,
}

aterm_objc::declare_class! {
    struct ContainmentProbe: NSObject {
        const NAME: &str = "ATermContainmentProbe";
        type Ivars = Ivars;

        @sel(raiseInteger)
        fn raise_integer(&self) -> i64 {
            let _local = Tell(Arc::clone(&self.ivars().drops));
            deep(&self.ivars().drops, "ATermProbeException", "raised on purpose") + 1
        }

        @sel(raiseBool)
        fn raise_bool(&self) -> Bool {
            let _local = Tell(Arc::clone(&self.ivars().drops));
            raise("ATermProbeBoolException", "the BOOL row raised");
        }

        @sel(raiseObject)
        fn raise_object(&self) -> Id {
            raise("ATermProbeObjectException", "the object row raised");
        }

        @sel(raiseRange)
        fn raise_range(&self) -> NSRange {
            raise("ATermProbeRangeException", "the range row raised");
        }

        @sel(fortyTwo)
        fn forty_two(&self) -> i64 {
            42
        }

        @sel(rustPanic)
        fn rust_panic(&self) {
            let _local = Tell(Arc::clone(&self.ivars().drops));
            panic!("a Rust panic inside a contained method");
        }

        @sel(raiseUncontained)
        @abort_on_exception
        fn raise_uncontained(&self) -> i64 {
            raise("ATermProbeUncontained", "the opted-out row raised");
        }
    }
}

fn probe(drops: &Arc<AtomicUsize>) -> aterm_objc::Retained<ContainmentProbe> {
    ContainmentProbe::alloc_init(
        mtm(),
        Ivars {
            drops: Arc::clone(drops),
        },
    )
    .expect("alloc_init")
}

unsafe fn send_i64(obj: Id, s: Sel) -> i64 {
    // SAFETY: the caller pins the prototype.
    unsafe {
        let f: unsafe extern "C-unwind" fn(Id, Sel) -> i64 = msg();
        f(obj, s)
    }
}

#[test]
fn a_raise_inside_a_declared_method_is_contained_and_every_drop_runs() {
    install_sink();
    let drops = Arc::new(AtomicUsize::new(0));
    let obj = probe(&drops);
    let before = contained_count();
    let answer = autoreleasepool(|_| {
        // SAFETY: `-raiseInteger` is `q@:` on the live instance.
        unsafe { send_i64(obj.as_id(), sel!(raiseInteger)) }
    });
    assert_eq!(
        answer, 0,
        "the contained method answers the inert zero, not `+ 1`"
    );
    assert_eq!(
        drops.load(Ordering::SeqCst),
        3,
        "the method's local and both deeper frames' locals were dropped by the unwind"
    );
    assert!(contained_count() > before, "the containment was counted");

    // SAFETY: `-fortyTwo` is `q@:`.
    let later = unsafe { send_i64(obj.as_id(), sel!(fortyTwo)) };
    assert_eq!(later, 42, "a later send on the same object works");

    let records = seen().lock().unwrap();
    let mine = records
        .iter()
        .find(|s| s.method == "raiseInteger")
        .unwrap_or_else(|| panic!("no record for raiseInteger in {records:?}"));
    assert_eq!(mine.class, "NSException");
    assert_eq!(mine.name, "ATermProbeException");
    assert_eq!(mine.reason, "raised on purpose");
    assert!(
        mine.call_stack.contains("Foundation") || mine.call_stack.contains("CoreFoundation"),
        "the call stack names the framework that threw: {:?}",
        mine.call_stack
    );
    assert!(
        mine.call_stack.contains(" | "),
        "the call stack is ONE line with frames joined: {:?}",
        mine.call_stack
    );
    assert!(
        mine.ordinal > before,
        "the record carries the count after this containment"
    );
}

#[test]
fn the_inert_answer_is_the_selectors_nothing_happened_value() {
    install_sink();
    let drops = Arc::new(AtomicUsize::new(0));
    let obj = probe(&drops);
    autoreleasepool(|_| {
        // SAFETY: each selector's prototype is the declared one.
        unsafe {
            let b = aterm_objc::send::send_bool(obj.as_id(), sel!(raiseBool));
            assert!(!b, "a contained BOOL row answers NO");
            let o = aterm_objc::send::send_id(obj.as_id(), sel!(raiseObject));
            assert!(o.is_null(), "a contained object row answers nil");
            let r: unsafe extern "C-unwind" fn(Id, Sel) -> NSRange = msg();
            let range = r(obj.as_id(), sel!(raiseRange));
            assert_eq!(
                (range.location, range.length),
                (0, 0),
                "a contained NSRange row answers {{0, 0}}"
            );
        }
    });
    assert_eq!(
        drops.load(Ordering::SeqCst),
        1,
        "the BOOL row's local was dropped"
    );
    let records = seen().lock().unwrap();
    for m in ["raiseBool", "raiseObject", "raiseRange"] {
        assert!(
            records.iter().any(|s| s.method == m),
            "no record for {m} in {records:?}"
        );
    }
}

#[test]
fn a_contained_method_dispatched_by_foundation_still_returns_to_foundation() {
    install_sink();
    let drops = Arc::new(AtomicUsize::new(0));
    let obj = probe(&drops);
    // SAFETY: `-performSelector:` is `@@::`; `-raiseObject` answers an object.
    let answer = autoreleasepool(|_| unsafe {
        let perform: unsafe extern "C-unwind" fn(Id, Sel, Sel) -> Id = msg();
        perform(obj.as_id(), sel!(performSelector:), sel!(raiseObject))
    });
    assert!(answer.is_null(), "nil came back through Foundation's frame");
    // SAFETY: `-fortyTwo` is `q@:`.
    assert_eq!(unsafe { send_i64(obj.as_id(), sel!(fortyTwo)) }, 42);
}

#[test]
fn a_block_contains_a_raise_and_answers_its_inert_zero() {
    use std::ffi::c_void;
    install_sink();
    let drops = Arc::new(AtomicUsize::new(0));
    let tell = Tell(Arc::clone(&drops));
    // SAFETY: an `NSInteger (^)(NSInteger)` block.
    let block = unsafe {
        aterm_objc::RcBlock::new1(move |n: i64| -> i64 {
            let _ = &tell;
            let _local = Tell(Arc::clone(&drops));
            if n == 7 {
                raise("ATermProbeBlockException", "the block raised");
            }
            n * 2
        })
    }
    .expect("heap block");
    #[repr(C)]
    struct HeaderPrefix {
        isa: *const c_void,
        flags: i32,
        reserved: i32,
        invoke: unsafe extern "C" fn(*mut c_void, i64) -> i64,
    }
    // SAFETY: the first four fields of every block are fixed ABI, and `block`
    // is a live heap block this crate built with that invoke prototype.
    let (ok, contained, again) = unsafe {
        let hdr = &*block.as_ptr().cast::<HeaderPrefix>();
        (
            (hdr.invoke)(block.as_ptr(), 3),
            autoreleasepool(|_| (hdr.invoke)(block.as_ptr(), 7)),
            (hdr.invoke)(block.as_ptr(), 4),
        )
    };
    assert_eq!(ok, 6);
    assert_eq!(contained, 0, "the contained block answers 0");
    assert_eq!(again, 8, "the block still works after the containment");
    let records = seen().lock().unwrap();
    let mine = records
        .iter()
        .find(|s| s.name == "ATermProbeBlockException")
        .unwrap_or_else(|| panic!("no record for the block in {records:?}"));
    assert_eq!(mine.method, "block invoke");
}

const CHILD: &str = "ATERM_OBJC_CONTAINMENT_CHILD";

fn child(test: &str, mode: &str) -> (Option<i32>, String, String) {
    let out = crate::reexec::rerun(module_path!(), test, &[(CHILD, mode)]);
    (
        out.status.code(),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

#[test]
fn a_rust_panic_inside_a_contained_method_still_aborts_with_the_named_message() {
    if crate::reexec::is_child(CHILD) {
        let drops = Arc::new(AtomicUsize::new(0));
        let obj = probe(&drops);
        // SAFETY: `-rustPanic` is `v@:`.
        unsafe { aterm_objc::send::send_v(obj.as_id(), sel!(rustPanic)) };
        println!("SURVIVED-THE-PANIC");
        return;
    }
    let (code, stdout, stderr) = child(
        "a_rust_panic_inside_a_contained_method_still_aborts_with_the_named_message",
        "panic",
    );
    assert!(
        !stdout.contains("SURVIVED-THE-PANIC"),
        "@catch(id) swallowed a Rust panic\nstdout: {stdout}\nstderr: {stderr}"
    );
    assert_eq!(
        code,
        Some(crate::reexec::ABORTED),
        "expected an abort\nstdout: {stdout}\nstderr: {stderr}"
    );
    assert!(
        stderr.contains("panic escaped Objective-C method `rust_panic`"),
        "the abort came through the unwind guard, naming the method\nstderr: {stderr}"
    );
    assert!(
        !stderr.contains("aterm-objc: contained "),
        "a Rust panic must not be reported as a containment\nstderr: {stderr}"
    );
}

#[test]
fn an_abort_on_exception_method_still_aborts_on_a_raise() {
    if crate::reexec::is_child(CHILD) {
        let drops = Arc::new(AtomicUsize::new(0));
        let obj = probe(&drops);
        // SAFETY: `-raiseUncontained` is `q@:`.
        let v = unsafe { send_i64(obj.as_id(), sel!(raiseUncontained)) };
        println!("SURVIVED-THE-RAISE {v}");
        return;
    }
    let (code, stdout, stderr) = child(
        "an_abort_on_exception_method_still_aborts_on_a_raise",
        "raise",
    );
    assert!(
        !stdout.contains("SURVIVED-THE-RAISE"),
        "the opted-out method was contained\nstdout: {stdout}\nstderr: {stderr}"
    );
    assert_eq!(
        code,
        Some(crate::reexec::ABORTED),
        "expected an abort\nstdout: {stdout}\nstderr: {stderr}"
    );
}

struct RaiseOnDrop;
impl Drop for RaiseOnDrop {
    fn drop(&mut self) {
        raise(
            "ATermProbeDeallocException",
            "raised from the ivar drop inside -dealloc",
        );
    }
}

aterm_objc::declare_class! {
    struct DeallocProbe: NSObject {
        const NAME: &str = "ATermContainmentDeallocProbe";
        type Ivars = RaiseOnDrop;

        @sel(ping)
        fn ping(&self) {}
    }
}

#[test]
fn dealloc_keeps_the_abort_on_a_raise() {
    if crate::reexec::is_child(CHILD) {
        let obj = DeallocProbe::alloc_init(mtm(), RaiseOnDrop).expect("alloc_init");
        drop(obj);
        println!("SURVIVED-DEALLOC");
        return;
    }
    let (code, stdout, stderr) = child("dealloc_keeps_the_abort_on_a_raise", "dealloc");
    assert!(
        !stdout.contains("SURVIVED-DEALLOC"),
        "-dealloc contained a raise, which is a half-deallocated object to continue from\nstdout: {stdout}\nstderr: {stderr}"
    );
    assert_eq!(
        code,
        Some(crate::reexec::ABORTED),
        "expected an abort\nstdout: {stdout}\nstderr: {stderr}"
    );
}

#[test]
fn the_default_sink_writes_one_line_naming_method_and_exception() {
    if crate::reexec::is_child(CHILD) {
        let drops = Arc::new(AtomicUsize::new(0));
        let obj = probe(&drops);
        // SAFETY: `-raiseInteger` is `q@:`.
        let v = autoreleasepool(|_| unsafe { send_i64(obj.as_id(), sel!(raiseInteger)) });
        println!("ANSWERED {v}");
        return;
    }
    let (code, stdout, stderr) = child(
        "the_default_sink_writes_one_line_naming_method_and_exception",
        "default-sink",
    );
    assert_eq!(code, Some(0), "the child must not die\nstderr: {stderr}");
    assert!(stdout.contains("ANSWERED 0"), "stdout: {stdout}");
    let lines: Vec<&str> = stderr
        .lines()
        .filter(|l| l.starts_with("aterm-objc: contained "))
        .collect();
    assert_eq!(
        lines.len(),
        1,
        "exactly ONE containment line for one raise (a second raise while describing the \
         first, or a duplicated sink, would print two):\n{stderr}"
    );
    let line = lines[0];
    assert!(
        line.contains("ATermProbeException") && line.contains("raiseInteger"),
        "the line names the exception and the method: {line}"
    );
}
