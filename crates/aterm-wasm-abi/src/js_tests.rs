// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::*;
use crate::descriptor::{Ty, decode_ty, tag};

crate::js_type! {
    /// A test type.
    struct Canvas = "HTMLCanvasElement";
}

/// A declared type describes its TypeScript name and crosses as the
/// object's slot; slot 0, which the glue never hands out, is refused.
#[test]
fn a_declared_type_names_its_ts_type_and_takes_its_slot() {
    assert_eq!(
        decode_ty(<Canvas as Describe>::DESC.as_bytes()),
        Ok(Ty::JsObject("HTMLCanvasElement".into()))
    );
    assert_eq!(<Canvas as Describe>::DESC.as_bytes()[0], tag::JS_OBJECT);
    // SAFETY: a slot index carries no memory.
    let c = unsafe { Canvas::from_abi(7, 0) }.expect("a slot");
    assert_eq!(c.0.index(), 7);
    // SAFETY: as above.
    assert!(unsafe { Canvas::from_abi(0, 0) }.is_err());
}
