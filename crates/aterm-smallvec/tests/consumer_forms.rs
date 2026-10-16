// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! The forms the third-party consumers write, transcribed against this crate.
//!
//! A macOS build compiles rustybuzz against the facade, and the `wgpu-oracle`
//! test graph compiles wgpu, wgpu-core, wgpu-hal (Metal) and parking_lot_core.
//! wayland-backend and its dependants, and wgpu-hal's Vulkan backend, compile
//! only on Linux. Each block below copies a consumer's shape, with the crate,
//! file and line it came from and local stand-ins for its types, so the
//! surface is checked against all of them on any host. The `#[test]`s run
//! the forms too.

// Consumers' shapes are copied whole, including fields this file never reads.
#![allow(dead_code)]

use smallvec::{SmallVec, smallvec};

// rustybuzz-0.20.1 src/hb/tag.rs:10-22: generic over `A: smallvec::Array`.
trait SmallVecExt {
    fn left(&self) -> usize;
    fn is_full(&self) -> bool;
}

impl<A: smallvec::Array> SmallVecExt for SmallVec<A> {
    fn left(&self) -> usize {
        self.inline_size() - self.len()
    }
    fn is_full(&self) -> bool {
        self.len() == self.inline_size()
    }
}

#[test]
fn rustybuzz_forms() {
    // tag.rs:31-32, 109: element type inferred from later use.
    let mut scripts = SmallVec::new();
    scripts.push(1u32);
    let mut tag = SmallVec::<[u8; 4]>::new();
    tag.push(b'a');
    let three: SmallVec<[u32; 3]> = scripts;
    assert_eq!(three.left(), 2);
    assert!(!tag.is_full());
    // ot_layout_gsubgpos.rs:462: `from_elem` then indexed writes.
    let mut match_positions: SmallVec<[usize; 4]> = smallvec::SmallVec::from_elem(0, 4);
    match_positions[3] = 9;
    match_positions.resize(6, 0);
    assert_eq!(&match_positions[..], &[0, 0, 0, 9, 0, 0]);
}

#[test]
fn parking_lot_core_forms() {
    // parking_lot.rs:821: `[_; 8]` inferred from what is pushed.
    let mut threads = SmallVec::<[_; 8]>::new();
    threads.push((1u32, "handle"));
    for (n, _) in threads.into_iter() {
        assert_eq!(n, 1);
    }
}

/// wayland-backend-0.3.15 src/protocol.rs:232: the public field that puts the
/// type in wayland-backend's API, and lib.rs:54-65's re-export plus the
/// `message!` macro that reaches `smallvec!` through it.
mod wayland_backend {
    pub extern crate smallvec;
    pub const INLINE_ARGS: usize = 4;
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub enum Argument {
        Int(i32),
        NewId(u32),
    }
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub struct Message {
        pub opcode: u16,
        pub args: smallvec::SmallVec<[Argument; INLINE_ARGS]>,
    }
    #[macro_export]
    macro_rules! message {
        ($opcode:expr, [$($args:expr),* $(,)?] $(,)?) => {
            $crate::wayland_backend::Message {
                opcode: $opcode,
                args: $crate::wayland_backend::smallvec::smallvec![$($args),*],
            }
        }
    }
}

use wayland_backend::{Argument, INLINE_ARGS, Message};

#[test]
fn wayland_forms() {
    let msg = crate::message!(1, [Argument::Int(3), Argument::NewId(2)]);
    assert_eq!(msg.args.len(), 2);
    let msg = crate::message!(2, []);
    assert!(msg.args.is_empty());
    // rs/wire.rs:236: collect into a `Result` of a `SmallVec<_>`.
    let args = [Ok(Argument::Int(1)), Ok(Argument::Int(2))]
        .into_iter()
        .collect::<Result<SmallVec<_>, ()>>()
        .map(|args| Message { opcode: 0, args });
    assert_eq!(args.map(|m| m.args.len()), Ok(2));
    // rs/client_impl/mod.rs:451: `with_capacity` inferred from the field.
    let mut msg_args = SmallVec::with_capacity(5);
    msg_args.push(Argument::Int(0));
    let msg = Message {
        opcode: 3,
        args: msg_args,
    };
    // wayland-scanner-0.31.10 common.rs:583-600: the generated forms, and the
    // generated parser's `msg.args.into_iter()`.
    let many: SmallVec<[Argument; INLINE_ARGS]> =
        smallvec::SmallVec::from_vec(vec![Argument::Int(1); 6]);
    let mut arg_iter = msg.args.into_iter();
    assert_eq!(arg_iter.next(), Some(Argument::Int(0)));
    assert!(many.spilled());
}

// smithay-client-toolkit-0.19.2 src/seat/pointer/mod.rs:7, 213: the type named
// through wayland-backend's re-export.
struct PointerData {
    pending: wayland_backend::smallvec::SmallVec<[u32; 3]>,
}

#[test]
fn wgpu_forms() {
    // wgpu-29.0.3 src/backend/wgpu_core.rs:2090: collect with an inferred item.
    let buffers = [1u64, 2, 3, 4, 5].into_iter().collect::<SmallVec<[_; 4]>>();
    assert!(buffers.spilled());
    // wgpu-core-29.0.3 device/life.rs:312: `drain(..)` into an inferred vec.
    let mut closures: SmallVec<[Box<dyn FnOnce() + Send>; 1]> = SmallVec::new();
    closures.push(Box::new(|| {}));
    let taken: SmallVec<_> = closures.drain(..).collect();
    let taken: SmallVec<[Box<dyn FnOnce() + Send>; 1]> = taken;
    assert_eq!((closures.len(), taken.len()), (0, 1));
    // command/render.rs:1852: `SmallVec::from` a slice.
    let attachments = [Some(1u8), None];
    let color: SmallVec<[Option<u8>; 1]> = SmallVec::from(attachments.as_slice());
    assert_eq!(color.len(), 2);
    // resource.rs:1270: elements with no `Clone`, behind `ManuallyDrop`.
    let mut clear_views: SmallVec<[std::mem::ManuallyDrop<Box<dyn std::fmt::Debug>>; 1]> =
        SmallVec::new();
    clear_views.push(std::mem::ManuallyDrop::new(Box::new(1u8)));
    for view in clear_views.drain(..) {
        drop(std::mem::ManuallyDrop::into_inner(view));
    }
    // track/range.rs: insert, retain and `iter_mut` on ranged states.
    let mut ranges: SmallVec<[(std::ops::Range<u32>, u8); 1]> =
        std::iter::once((0..4, 1)).collect();
    ranges.insert(0, (0..0, 2));
    ranges.retain(|pair| pair.0.start != pair.0.end);
    ranges.iter_mut().for_each(|pair| pair.1 += 1);
    assert_eq!(&ranges[..], &[(0..4, 2)]);
    // wgpu-hal-29.0.3 vulkan/command.rs:559-596: nested vectors, `with_capacity`
    // and a tuple of `smallvec!`s.
    const CAPACITY_OUTER: usize = 8;
    const CAPACITY_INNER: usize = 1;
    let mut storage =
        SmallVec::<[SmallVec<[u32; CAPACITY_INNER]>; CAPACITY_OUTER]>::with_capacity(2);
    let (geometries, ranges): (
        SmallVec<[u32; CAPACITY_INNER]>,
        SmallVec<[u8; CAPACITY_INNER]>,
    ) = (smallvec![7], smallvec![8]);
    storage.push(geometries);
    assert_eq!((storage[0][0], ranges[0]), (7, 8));
    // vulkan/device.rs:346: `from_iter` by turbofish, borrowed as a slice.
    let sets = &smallvec::SmallVec::<[u64; 32]>::from_iter([1, 2, 3]);
    assert_eq!(sets.as_slice(), &[1, 2, 3]);
}

/// The auto traits the consumers rely on (wgpu-core's queues hold
/// `SmallVec`s across threads).
#[test]
fn send_and_sync_follow_the_item() {
    fn send_sync<T: Send + Sync>() {}
    send_sync::<SmallVec<[u32; 4]>>();
    send_sync::<SmallVec<[std::sync::Arc<u32>; 1]>>();
}
