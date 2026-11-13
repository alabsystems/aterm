// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

use super::super::tests::{run, spots};
use super::*;

/// 0.161.0's reap hold: a flag the guard's Drop clears.
const FLAG: &str = "#[cfg(any(test, debug_assertions))]\n\
     static HOLD: AtomicBool = AtomicBool::new(false);\n\
     #[cfg(any(test, debug_assertions))]\n\
     pub fn hold() -> Hold {\n    \
         HOLD.store(true, Ordering::Release);\n    \
         Hold(())\n\
     }\n\
     #[cfg(any(test, debug_assertions))]\n\
     pub struct Hold(());\n\
     #[cfg(any(test, debug_assertions))]\n\
     impl Drop for Hold {\n    \
         fn drop(&mut self) {\n        \
             HOLD.store(false, Ordering::Release);\n    \
         }\n\
     }\n";

#[test]
fn a_flag_a_test_guard_resets_is_found_and_its_count_is_not() {
    let count = FLAG
        .replace("AtomicBool::new(false)", "AtomicUsize::new(0)")
        .replace("AtomicBool", "AtomicUsize")
        .replace("HOLD.store(true, ", "HOLD.fetch_add(1, ")
        .replace("HOLD.store(false, ", "HOLD.fetch_sub(1, ");
    let shipped = FLAG.replace("#[cfg(any(test, debug_assertions))]\n", "");
    let found = run(
        &SEAM_RESET,
        &[
            ("crates/x/src/flag.rs", FLAG),
            ("crates/x/src/count.rs", &count),
            ("crates/x/src/shipped.rs", &shipped),
        ],
    );
    assert_eq!(spots(&found), ["crates/x/src/flag.rs:13"]);
}

#[test]
fn a_restore_in_a_test_file_and_a_wrapped_swap_are_found() {
    let restore = "struct Restore(u8);\n\
         impl Drop for Restore {\n    \
             fn drop(&mut self) {\n        \
                 crate::mode::MODE\n            \
                     .store(self.0, Ordering::SeqCst);\n        \
                 LAST.swap(0, Ordering::SeqCst);\n    \
             }\n\
         }\n";
    let found = run(&SEAM_RESET, &[("crates/x/tests/restore.rs", restore)]);
    assert_eq!(
        spots(&found),
        ["crates/x/tests/restore.rs:5", "crates/x/tests/restore.rs:6"]
    );
}

#[test]
fn a_slot_emptied_behind_its_lock_is_found_and_a_keyed_removal_is_not() {
    let slot = "#[cfg(test)]\n\
         mod tests {\n    \
             struct Armed;\n    \
             impl Drop for Armed {\n        \
                 fn drop(&mut self) {\n            \
                     *HOOK.lock().unwrap() = None;\n            \
                     SEEN.lock().unwrap().clear();\n            \
                     let _ = NEXT.lock().unwrap().take();\n            \
                     ARMED.lock().unwrap().remove(&self.0);\n            \
                     let held = HELD.lock().unwrap().len() == 0;\n            \
                     self.flag.store(false, Ordering::SeqCst);\n            \
                     OWN.with(|own| own.set(None));\n        \
                 }\n    \
             }\n\
         }\n";
    let found = run(&SEAM_RESET, &[("crates/x/src/slot.rs", slot)]);
    assert_eq!(
        spots(&found),
        [
            "crates/x/src/slot.rs:6",
            "crates/x/src/slot.rs:7",
            "crates/x/src/slot.rs:8",
        ]
    );
}

#[test]
fn a_reset_no_other_holder_can_be_inside_of_passes() {
    let serialised = "#[cfg(test)]\n\
         struct Override {\n    \
             _serial: std::sync::MutexGuard<'static, ()>,\n\
         }\n\
         #[cfg(test)]\n\
         impl Drop for Override {\n    \
             fn drop(&mut self) {\n        \
                 *DIR.write().unwrap() = None;\n    \
             }\n\
         }\n";
    let shared = serialised.replace("MutexGuard<", "RwLockReadGuard<");
    let exclusive = "static ACTIVE: AtomicBool = AtomicBool::new(false);\n\
         struct Measuring;\n\
         impl Drop for Measuring {\n    \
             fn drop(&mut self) {\n        \
                 ACTIVE.store(false, Ordering::SeqCst);\n    \
             }\n\
         }\n\
         fn measure() {\n    \
             assert!(!ACTIVE\n        .swap(true, Ordering::SeqCst));\n    \
             let _m = Measuring;\n\
         }\n";
    let unasserted = exclusive.replace("assert!(!ACTIVE\n        .swap(true", "ACTIVE.store(true");
    let last = "#[cfg(test)]\n\
         impl Drop for OwnLane {\n    \
             fn drop(&mut self) {\n        \
                 if let Ok(mut q) = INBOX.lock() {\n            \
                     q.retain(|(at, _)| *at != self.lane);\n            \
                     if q.is_empty() {\n                \
                         PENDING.store(false, Ordering::Relaxed);\n            \
                     }\n            \
                     if self.lane > 0 {\n                \
                         LAST.store(0, Ordering::Relaxed);\n            \
                     }\n        \
                 }\n        \
                 if HOLDS.fetch_sub(1, Ordering::AcqRel) == 1 {\n            \
                     HELD.store(false, Ordering::Release);\n        \
                 }\n    \
             }\n\
         }\n";
    let found = run(
        &SEAM_RESET,
        &[
            ("crates/x/src/serialised.rs", serialised),
            ("crates/x/src/shared.rs", &shared),
            ("crates/x/tests/exclusive.rs", exclusive),
            ("crates/x/tests/unasserted.rs", &unasserted),
            ("crates/x/src/last.rs", last),
        ],
    );
    assert_eq!(
        spots(&found),
        [
            "crates/x/src/last.rs:10",
            "crates/x/src/shared.rs:8",
            "crates/x/tests/unasserted.rs:5",
        ]
    );
}

#[test]
fn a_binding_a_comparison_or_a_read_through_a_lock_is_no_reset() {
    let reads = "#[cfg(test)]\n\
         impl Drop for Rig {\n    \
             fn drop(&mut self) {\n        \
                 if let Some(m) = WATCHES.lock().unwrap().as_mut() {\n            \
                     m.remove(&self.pid);\n        \
                 }\n        \
                 *self.seen = Some((ACTIVITY.lock().unwrap().subjects.is_empty(), 1));\n        \
                 let idle = QUEUE.lock().unwrap().next.is_none() == true;\n        \
                 assert!(LINK.try_lock().is_err());\n    \
             }\n\
         }\n";
    assert!(run(&SEAM_RESET, &[("crates/x/src/reads.rs", reads)]).is_empty());
    assert!(assigns_through("\n    *", "HOOK.lock().unwrap() = None"));
    assert!(assigns_through("", "SLOT.lock().unwrap().field = 1"));
    assert!(!assigns_through("", "HELD.lock().unwrap().len() == 0"));
    assert!(!assigns_through("let g = ", "HELD.lock()"));
    assert!(!assigns_through("if let Ok(q) = ", "HELD.lock() {"));
    assert!(!assigns_through("match ", "HELD.lock() { Ok(g) => g }"));
    assert!(!assigns_through("", "HELD.lock().map(|g| g.len() >= 1)"));
    assert!(!assigns_through(
        "*self.0 = Some((",
        "A.lock().unwrap().x, 1))"
    ));
    assert!(!assigns_through("\n    *", "HELD.lock().unwrap() -= 1"));
}

#[test]
fn statics_are_screaming() {
    assert!(is_static_name("HOLD_REAP_KILLS"));
    assert!(is_static_name("A1"));
    assert!(!is_static_name("hold"));
    assert!(!is_static_name("Hold"));
    assert!(!is_static_name("_"));
}
