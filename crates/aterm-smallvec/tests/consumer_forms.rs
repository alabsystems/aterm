// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! The forms the third-party consumers write, transcribed against this crate.

#![allow(dead_code)]

use smallvec::{SmallVec, smallvec};

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
    let mut scripts = SmallVec::new();
    scripts.push(1u32);
    let mut tag = SmallVec::<[u8; 4]>::new();
    tag.push(b'a');
    let three: SmallVec<[u32; 3]> = scripts;
    assert_eq!(three.left(), 2);
    assert!(!tag.is_full());
    let mut match_positions: SmallVec<[usize; 4]> = smallvec::SmallVec::from_elem(0, 4);
    match_positions[3] = 9;
    match_positions.resize(6, 0);
    assert_eq!(&match_positions[..], &[0, 0, 0, 9, 0, 0]);
}

#[test]
fn parking_lot_core_forms() {
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
            $crate::consumer_forms::wayland_backend::Message {
                opcode: $opcode,
                args: $crate::consumer_forms::wayland_backend::smallvec::smallvec![$($args),*],
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
    let args = [Ok(Argument::Int(1)), Ok(Argument::Int(2))]
        .into_iter()
        .collect::<Result<SmallVec<_>, ()>>()
        .map(|args| Message { opcode: 0, args });
    assert_eq!(args.map(|m| m.args.len()), Ok(2));
    let mut msg_args = SmallVec::with_capacity(5);
    msg_args.push(Argument::Int(0));
    let msg = Message {
        opcode: 3,
        args: msg_args,
    };
    let many: SmallVec<[Argument; INLINE_ARGS]> =
        smallvec::SmallVec::from_vec(vec![Argument::Int(1); 6]);
    let mut arg_iter = msg.args.into_iter();
    assert_eq!(arg_iter.next(), Some(Argument::Int(0)));
    assert!(many.spilled());
}

struct PointerData {
    pending: wayland_backend::smallvec::SmallVec<[u32; 3]>,
}

#[test]
fn wgpu_forms() {
    let buffers = [1u64, 2, 3, 4, 5].into_iter().collect::<SmallVec<[_; 4]>>();
    assert!(buffers.spilled());
    let mut closures: SmallVec<[Box<dyn FnOnce() + Send>; 1]> = SmallVec::new();
    closures.push(Box::new(|| {}));
    let taken: SmallVec<_> = closures.drain(..).collect();
    let taken: SmallVec<[Box<dyn FnOnce() + Send>; 1]> = taken;
    assert_eq!((closures.len(), taken.len()), (0, 1));
    let attachments = [Some(1u8), None];
    let color: SmallVec<[Option<u8>; 1]> = SmallVec::from(attachments.as_slice());
    assert_eq!(color.len(), 2);
    let mut clear_views: SmallVec<[std::mem::ManuallyDrop<Box<dyn std::fmt::Debug>>; 1]> =
        SmallVec::new();
    clear_views.push(std::mem::ManuallyDrop::new(Box::new(1u8)));
    for view in clear_views.drain(..) {
        drop(std::mem::ManuallyDrop::into_inner(view));
    }
    let mut ranges: SmallVec<[(std::ops::Range<u32>, u8); 1]> =
        std::iter::once((0..4, 1)).collect();
    ranges.insert(0, (0..0, 2));
    ranges.retain(|pair| pair.0.start != pair.0.end);
    ranges.iter_mut().for_each(|pair| pair.1 += 1);
    assert_eq!(&ranges[..], &[(0..4, 2)]);
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
