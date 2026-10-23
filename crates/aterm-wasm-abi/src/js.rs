// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! JS objects the host hands a module: a `<canvas>`, an `OffscreenCanvas`.
//!
//! The module never sees the object, only its index in the glue's object
//! table: the glue stores an argument there (`addHeapObject`) and passes the
//! index, and an import the module calls with that index reads the object
//! back (`getObject`). A [`JsObject`] owns its slot and releases it on drop
//! (`aterm_host.object_drop`), so the table holds an object exactly as long
//! as the module does. Index 0 is never handed out.
//!
//! [`js_type!`](crate::js_type) declares a typed parameter, a newtype over
//! [`JsObject`] whose descriptor names the TypeScript type the `.d.ts` shows
//! (`init(canvas: HTMLCanvasElement)`). It crosses as a parameter only: a
//! module that returns a JS object has no use in aterm yet.
//!
//! The glue does not check the object's type, as wasm-bindgen's glue does
//! not for an imported type: the import that uses it does (a `getContext`
//! that a non-canvas lacks answers as a failed context, never a throw
//! through the module).

use crate::descriptor::TypeDesc;
use crate::marshal::{Describe, FromAbi, Thrown};

/// A slot in the glue's object table, released when dropped.
#[derive(Debug, PartialEq, Eq)]
pub struct JsObject {
    index: u32,
}

impl JsObject {
    /// The object's index in the glue's table, for an import to name it by.
    #[inline]
    #[must_use]
    pub const fn index(&self) -> u32 {
        self.index
    }
}

impl Drop for JsObject {
    fn drop(&mut self) {
        #[cfg(all(target_arch = "wasm32", target_os = "unknown"))]
        crate::host::object_drop(self.index);
    }
}

impl Describe for JsObject {
    const DESC: TypeDesc = TypeDesc::js_object("unknown");
}

impl FromAbi for JsObject {
    type W0 = u32;
    type W1 = u32;

    /// # Safety
    /// `index` is a slot the glue filled for this call ([`FromAbi`]'s
    /// contract: the words are this type's lowering), now owned here.
    #[inline]
    unsafe fn from_abi(index: u32, _: u32) -> Result<Self, Thrown> {
        if index == 0 {
            return Err(Thrown::Error(
                "a JS object argument has no table slot".into(),
            ));
        }
        Ok(Self { index })
    }
}

/// Declare a JS type a module's exports take: a newtype over [`JsObject`]
/// whose descriptor names `$ts`, the TypeScript type its `.d.ts` shows.
///
/// ```
/// aterm_wasm_abi::js_type! {
///     /// A `<canvas>` element.
///     pub struct HtmlCanvasElement = "HTMLCanvasElement";
/// }
/// ```
/// aterm_wasm_abi::js_type!
#[macro_export]
macro_rules! js_type {
    ($(#[$meta:meta])* $vis:vis struct $name:ident = $ts:literal;) => {
        $(#[$meta])*
        #[derive(Debug)]
        $vis struct $name(pub $crate::js::JsObject);

        impl $crate::marshal::Describe for $name {
            const DESC: $crate::descriptor::TypeDesc = $crate::descriptor::TypeDesc::js_object($ts);
        }

        impl $crate::marshal::FromAbi for $name {
            type W0 = u32;
            type W1 = u32;
            unsafe fn from_abi(w0: u32, w1: u32) -> Result<Self, $crate::marshal::Thrown> {
                // SAFETY: the caller's contract, passed on.
                unsafe { <$crate::js::JsObject as $crate::marshal::FromAbi>::from_abi(w0, w1) }
                    .map(Self)
            }
        }
    };
}

#[cfg(test)]
mod tests {
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
}
