// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::{Object, encode, push_be};

/// `objects` as a `bplist00`, the top object `top`; every reference and offset 2 bytes wide.
pub(crate) fn list(objects: &[Object], top: usize) -> Vec<u8> {
    let mut out = b"bplist00".to_vec();
    let mut offsets = Vec::new();
    for object in objects {
        offsets.push(out.len());
        encode(&mut out, object, 2).unwrap();
    }
    let table = out.len();
    for at in offsets {
        push_be(&mut out, u64::from(u16::try_from(at).unwrap()), 2);
    }
    out.extend([0, 0, 0, 0, 0, 0, 2, 2]);
    for n in [objects.len(), top, table] {
        push_be(&mut out, n as u64, 8);
    }
    out
}
