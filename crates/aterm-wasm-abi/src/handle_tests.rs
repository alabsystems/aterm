// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::*;
use std::rc::Rc;

struct Probe(Rc<StdCell<u32>>, u32);

impl Class for Probe {
    const NAME: &'static str = "Probe";
}

impl Drop for Probe {
    fn drop(&mut self) {
        self.0.set(self.0.get() + 1);
    }
}

struct Other;

impl Class for Other {
    const NAME: &'static str = "Other";
}

fn err(msg: &str) -> Thrown {
    Thrown::Error(msg.to_string())
}

fn probe() -> (usize, Rc<StdCell<u32>>) {
    let drops = Rc::new(StdCell::new(0));
    (into_raw(Probe(drops.clone(), 7)), drops)
}

#[test]
fn calls_see_the_value_and_free_drops_it_once() {
    let (h, drops) = probe();
    assert_ne!(h, 0);
    // SAFETY (this test): `h` is live until the `free` that returns Ok.
    unsafe {
        assert_eq!(borrow::<Probe>(h).unwrap().1, 7);
        borrow_mut::<Probe>(h).unwrap().1 = 8;
        {
            let a = borrow::<Probe>(h).unwrap();
            let b = borrow::<Probe>(h).unwrap();
            assert_eq!(a.1 + b.1, 16, "shared borrows coexist");
        }
        assert_eq!(free::<Probe>(h, false), Ok(()));
    }
    assert_eq!(drops.get(), 1);
}

#[test]
fn null_and_another_class_are_refused_before_any_access() {
    let other = into_raw(Other);
    // SAFETY: 0 and live handles only.
    unsafe {
        assert_eq!(borrow::<Probe>(0).err(), Some(err(NULL)));
        assert_eq!(borrow_mut::<Probe>(0).err(), Some(err(NULL)));
        assert_eq!(free::<Probe>(0, false), Err(err(NULL)));
        assert_eq!(borrow::<Probe>(other).err(), Some(err(WRONG_CLASS)));
        assert_eq!(free::<Probe>(other, false), Err(err(WRONG_CLASS)));
        assert_eq!(free::<Other>(other, false), Ok(()));
    }
}

#[test]
fn matching_class_hashes_do_not_authorize_another_rust_type() {
    #[repr(transparent)]
    struct CUzpSYPuOo(u32);
    #[repr(transparent)]
    struct CPmqmZnSBt(u32);
    impl Class for CUzpSYPuOo {
        const NAME: &'static str = "CUzpSYPuOo";
    }
    impl Class for CPmqmZnSBt {
        const NAME: &'static str = "CPmqmZnSBt";
    }
    assert_eq!(CUzpSYPuOo::TAG, CPmqmZnSBt::TAG, "a genuine collision");
    let ptr = into_raw(CUzpSYPuOo(7));
    // SAFETY: ptr stays a live handle, both types have the same valid
    // representation, and no wrongly admitted value is dereferenced.
    let (shared, exclusive) = unsafe {
        let shared = borrow::<CPmqmZnSBt>(ptr).err();
        let exclusive = borrow_mut::<CPmqmZnSBt>(ptr).err();
        assert_eq!(borrow::<CUzpSYPuOo>(ptr).unwrap().0, 7);
        assert_eq!(free::<CUzpSYPuOo>(ptr, false), Ok(()));
        (shared, exclusive)
    };
    assert_eq!(shared, Some(err(WRONG_CLASS)));
    assert_eq!(exclusive, Some(err(WRONG_CLASS)));
}

#[test]
fn aliasing_borrows_are_refused() {
    let (h, _drops) = probe();
    // SAFETY: `h` is live throughout.
    unsafe {
        let shared = borrow::<Probe>(h).unwrap();
        assert_eq!(borrow_mut::<Probe>(h).err(), Some(err(ALIASED)));
        drop(shared);
        let exclusive = borrow_mut::<Probe>(h).unwrap();
        assert_eq!(borrow::<Probe>(h).err(), Some(err(ALIASED)));
        assert_eq!(borrow_mut::<Probe>(h).err(), Some(err(ALIASED)));
        drop(exclusive);
        assert!(borrow_mut::<Probe>(h).is_ok());
        assert_eq!(free::<Probe>(h, false), Ok(()));
    }
}

#[test]
fn a_trap_mid_call_poisons_only_that_object() {
    let (h, drops) = probe();
    let (sibling, sibling_drops) = probe();
    // SAFETY: both handles stay live (the poisoned one is leaked).
    unsafe {
        std::mem::forget(borrow_mut::<Probe>(h).unwrap());
        assert_eq!(borrow::<Probe>(h).err(), Some(err(ALIASED)));
        assert_eq!(borrow_mut::<Probe>(h).err(), Some(err(ALIASED)));
        assert_eq!(free::<Probe>(h, false), Err(err(BORROWED)));
        assert_eq!(free::<Probe>(h, true), Ok(()));
        assert_eq!(drops.get(), 0, "a poisoned value is never dropped");

        assert_eq!(borrow::<Probe>(sibling).unwrap().1, 7);
        assert_eq!(free::<Probe>(sibling, true), Ok(()));
    }
    assert_eq!(sibling_drops.get(), 1);
}
