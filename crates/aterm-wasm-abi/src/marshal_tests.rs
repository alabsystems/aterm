// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Each type through a model of the glue: lower a JS value the way the
//! generated glue does (allocate, write), run the Rust conversion, and
//! lift the result the way the glue does (read the slot, copy, free).
use super::*;
use crate::descriptor::{Ty, decode_ty};
use crate::mem;

/// The glue's `passStringToWasm0`/`passArray*ToWasm0`: allocate exactly
/// the bytes at the element's alignment and copy them in.
fn pass<T: Copy>(items: &[T]) -> (usize, usize) {
    let size = std::mem::size_of_val(items);
    let ptr = mem::alloc(size, std::mem::align_of::<T>()).unwrap();
    // SAFETY: `ptr` is a fresh allocation of `size` bytes, aligned for T.
    unsafe { std::ptr::copy_nonoverlapping(items.as_ptr(), ptr as *mut T, items.len()) };
    (ptr, items.len())
}

/// The glue's `getArray*FromWasm0(ptr, len).slice()` then `aterm_free`.
fn lift<T: Copy>(ptr: usize, len: u32) -> Vec<T> {
    let len = len as usize;
    let out = if len == 0 {
        Vec::new()
    } else {
        // SAFETY: the module handed out `len` initialised `T`s at `ptr`.
        unsafe { std::slice::from_raw_parts(ptr as *const T, len) }.to_vec()
    };
    // SAFETY: freed once, with the layout the module made it with.
    unsafe {
        mem::free(
            ptr,
            len * std::mem::size_of::<T>(),
            std::mem::align_of::<T>(),
        )
    };
    out
}

fn ty<T: Describe + ?Sized>() -> Ty {
    decode_ty(T::DESC.as_bytes()).unwrap()
}

/// The trait impls lower each type to the words the generator's table
/// says, so the export the macro writes has the signature the glue
/// calls.
#[test]
fn the_impls_lower_as_the_table_says() {
    fn param<T: FromAbi>() {
        assert_eq!(
            ty::<T>().param_words(),
            Some([T::W0::VALTYPE, T::W1::VALTYPE]),
            "{}",
            ty::<T>()
        );
    }
    fn ref_param<T: RefFromAbi + ?Sized>() {
        assert_eq!(
            ty::<T>().param_words(),
            Some([T::W0::VALTYPE, T::W1::VALTYPE]),
            "{}",
            ty::<T>()
        );
    }
    fn ret<T: IntoAbi>() {
        assert_eq!(
            ty::<T>().ret_word(),
            Some(<T::Ret as RetWord>::RESULT),
            "{}",
            ty::<T>()
        );
    }
    macro_rules! each_scalar {
        ($($t:ty),*) => {$(
            param::<$t>();
            param::<Option<$t>>();
            ret::<$t>();
            ret::<Option<$t>>();
            ret::<Result<$t, String>>();
            assert!(!ty::<$t>().ret_uses_slot());
            assert!(ty::<Option<$t>>().ret_uses_slot());
        )*};
    }
    each_scalar!(
        bool, u8, u16, u32, u64, usize, i8, i16, i32, i64, isize, f32, f64, char
    );
    macro_rules! each_elem {
        ($($t:ty),*) => {$(
            ref_param::<[$t]>();
            param::<Vec<$t>>();
            param::<Option<Vec<$t>>>();
            ret::<Vec<$t>>();
            ret::<Option<Vec<$t>>>();
        )*};
    }
    each_elem!(u8, u16, u32, u64, i8, i16, i32, i64, f32, f64);
    ref_param::<str>();
    param::<String>();
    param::<Option<String>>();
    ret::<String>();
    ret::<Option<String>>();
    ret::<()>();
    ret::<Result<(), String>>();
    ret::<Result<Option<String>, String>>();
    param::<crate::js::JsObject>();
    assert!(!ty::<()>().ret_uses_slot());
}

#[test]
fn scalars_cross_as_wasm_bindgen_casts_them() {
    // SAFETY (each from_abi below): scalars carry no memory.
    unsafe {
        assert_eq!(bool::from_abi(2, 0), Ok(true));
        assert_eq!(bool::from_abi(0, 0), Ok(false));
        assert_eq!(u8::from_abi(256 + 7, 0), Ok(7));
        assert_eq!(u16::from_abi(0x1_0001, 0), Ok(1));
        assert_eq!(i8::from_abi(-129, 0), Ok(127));
        assert_eq!(u64::from_abi(u64::MAX, 0), Ok(u64::MAX));
        assert_eq!(char::from_abi(0x1F600, 0), Ok('😀'));
        assert_eq!(
            char::from_abi(0xD800, 0),
            Err(Thrown::Error(
                "expected a valid Unicode scalar value, found 55296".into()
            ))
        );
    }
    assert_eq!(true.into_abi(), Ok((1, 0)));
    assert_eq!(255u8.into_abi(), Ok((255, 0)));
    assert_eq!((-1i16).into_abi(), Ok((-1, 0)));
    assert_eq!('€'.into_abi(), Ok((0x20AC, 0)));
    assert_eq!(
        f64::NAN.into_abi().map(|(w, s)| (w.is_nan(), s)),
        Ok((true, 0))
    );
}

#[test]
fn options_of_scalars_carry_presence_beside_the_value() {
    // SAFETY: scalars carry no memory.
    unsafe {
        assert_eq!(Option::<u32>::from_abi(0, 99), Ok(None));
        assert_eq!(Option::<u32>::from_abi(1, 99), Ok(Some(99)));
        assert_eq!(Option::<u64>::from_abi(1, 1 << 40), Ok(Some(1 << 40)));
        assert_eq!(Option::<char>::from_abi(1, 0), Ok(Some('\0')));
        assert!(Option::<char>::from_abi(1, 0x11_0000).is_err());
        assert_eq!(Option::<char>::from_abi(0, 0x11_0000), Ok(None));
    }
    assert_eq!(Some(5u16).into_abi(), Ok((5, 1)));
    assert_eq!(None::<u16>.into_abi(), Ok((0, 0)));
    assert_eq!(Some(-2.5f64).into_abi(), Ok((-2.5, 1)));
    assert_eq!(
        Some(false).into_abi(),
        Ok((0, 1)),
        "Some(false) is not None"
    );
}

#[test]
fn strings_round_trip_through_the_allocator() {
    for text in ["", "ascii", "héllo — ünïcödé 😀", "\0nul"] {
        let (ptr, len) = pass(text.as_bytes());
        // SAFETY: `pass` made an allocation of exactly `len` bytes.
        let anchor = unsafe { <str as RefFromAbi>::ref_from_abi(ptr, len) }.unwrap();
        assert_eq!(&*anchor, text);
        let (ptr, len) = anchor.into_abi().unwrap();
        assert_eq!(String::from_utf8(lift::<u8>(ptr, len)).unwrap(), text);
    }
    if cfg!(debug_assertions) {
        let (ptr, len) = pass(&[0xffu8, 0xfe]);
        // SAFETY: as above, but for the bytes, which a debug build checks rather than trusts; ownership is taken even on the error.
        assert_eq!(
            unsafe { String::from_abi(ptr, len) },
            Err(Thrown::Error("a string argument is not UTF-8".into()))
        );
    }
    // SAFETY: a null pointer is `None` without touching memory.
    assert_eq!(unsafe { Option::<String>::from_abi(0, 5) }, Ok(None));
    let (ptr, len) = pass(b"x");
    // SAFETY: as above.
    assert_eq!(
        unsafe { Option::<String>::from_abi(ptr, len) },
        Ok(Some("x".into()))
    );
    assert_eq!(None::<String>.into_abi(), Ok((0, 0)));
    let (ptr, len) = Some(String::from("yes")).into_abi().unwrap();
    assert_ne!(ptr, 0);
    assert_eq!(lift::<u8>(ptr, len), b"yes");
}

#[test]
fn buffers_round_trip_at_their_element_alignment() {
    let words = [0u32, 1, u32::MAX];
    let (ptr, len) = pass(&words);
    // SAFETY: `pass` made `len` u32s at u32's alignment.
    let anchor = unsafe { <[u32] as RefFromAbi>::ref_from_abi(ptr, len) }.unwrap();
    assert_eq!(&*anchor, &words);
    let (ptr, len) = anchor.into_abi().unwrap();
    assert_eq!(lift::<u32>(ptr, len), words);

    let floats = vec![0.5f64, f64::INFINITY, -0.0];
    let (ptr, len) = floats.clone().into_abi().unwrap();
    assert_eq!(ptr % 8, 0);
    assert_eq!(lift::<f64>(ptr, len), floats);

    let empty: Vec<u8> = Vec::new();
    let (ptr, len) = empty.into_abi().unwrap();
    assert_eq!((ptr != 0, len), (true, 0), "an empty buffer is not None");
    assert!(lift::<u8>(ptr, len).is_empty());
    // SAFETY: an empty buffer was never allocated; its pointer is the alignment.
    assert_eq!(unsafe { Vec::<u16>::from_abi(2, 0) }, Ok(Vec::new()));
    assert_eq!(None::<Vec<u8>>.into_abi(), Ok((0, 0)));
    let (ptr, len) = Some(vec![9u8]).into_abi().unwrap();
    assert_eq!(lift::<u8>(ptr, len), [9]);
}

#[test]
fn an_err_is_thrown_as_its_string_and_an_ok_lowers_as_its_value() {
    let err: Result<u32, String> = Err("bad blob".into());
    assert_eq!(err.into_abi(), Err(Thrown::Value("bad blob".into())));
    let ok: Result<Option<u64>, String> = Ok(Some(3));
    assert_eq!(ok.into_abi(), Ok((3, 1)));
    assert_eq!(Ok::<(), String>(()).into_abi(), Ok(((), 0)));
}
