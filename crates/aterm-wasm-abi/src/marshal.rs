// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! How each boundary type crosses: the lowering both sides agree on.

use crate::descriptor::{TypeDesc, ValType, tag};

/// A JS exception an export raises: left in the return slot as the export returns ([`crate::mem::set_thrown`]), and thrown by the glue.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Thrown {
    /// `throw new Error(msg)`: the boundary was misused (a freed or busy handle, a bad argument), as wasm-bindgen's `throw_str` reports it.
    Error(String),
    /// `throw msg`: the export returned `Err(msg)`; JS catches the string.
    Value(String),
}

/// A wasm value a word of the boundary is carried in.
pub trait Word: Copy + Default {
    /// Its wasm type on `wasm32` (a `usize` is an `i32` there).
    const VALTYPE: ValType;
}

macro_rules! words {
    ($($t:ty => $v:ident),* $(,)?) => {
        $(impl Word for $t {
            const VALTYPE: ValType = ValType::$v;
        })*
    };
}
words!(u32 => I32, i32 => I32, usize => I32, isize => I32, u64 => I64, i64 => I64, f32 => F32, f64 => F64);

/// What an export returns as its wasm result: a [`Word`], or nothing.
pub trait RetWord: Copy + Default {
    /// Its wasm type on `wasm32`, `None` for `()`.
    const RESULT: Option<ValType>;
}

impl<W: Word> RetWord for W {
    const RESULT: Option<ValType> = Some(W::VALTYPE);
}

impl RetWord for () {
    const RESULT: Option<ValType> = None;
}

/// A type's entry in the descriptor.
pub trait Describe {
    /// Its description ([`crate::descriptor::tag`]).
    const DESC: TypeDesc;
}

/// An owned parameter type.
pub trait FromAbi: Sized + Describe {
    /// The first word.
    type W0: Word;
    /// The second word.
    type W1: Word;

    /// The value the glue lowered into `w0`, `w1`.
    /// # Safety
    /// The words are this type's lowering as the glue writes it
    /// ([`crate::descriptor::Ty::param_words`]): a pointer and length name a live allocation of
    /// exactly that many elements, made by [`crate::mem::alloc`] at the element's alignment, which
    /// this call takes ownership of. A string's bytes are UTF-8, as the glue's `TextEncoder` writes
    /// them; a release build does not check that.
    /// # Errors
    /// [`Thrown::Error`] when the words are no value of the type (an invalid
    /// `char`; in a debug build, a string that is not UTF-8). Ownership was
    /// still taken.
    unsafe fn from_abi(w0: Self::W0, w1: Self::W1) -> Result<Self, Thrown>;
}

/// A borrowed parameter type (`str`, `[T]`): the call borrows an owned anchor the conversion builds, dropped when the call returns.
pub trait RefFromAbi: Describe {
    /// What the borrow is taken from.
    type Anchor: core::ops::Deref<Target = Self>;
    /// The first word.
    type W0: Word;
    /// The second word.
    type W1: Word;

    /// The anchor for the words the glue lowered.
    /// # Safety
    /// As [`FromAbi::from_abi`].
    /// # Errors
    /// As [`FromAbi::from_abi`].
    unsafe fn ref_from_abi(w0: Self::W0, w1: Self::W1) -> Result<Self::Anchor, Thrown>;
}

/// A return type.
pub trait IntoAbi: Describe {
    /// The wasm result.
    type Ret: RetWord;

    /// The result word and the value for the return slot (0 when the type does not use it).
    /// # Errors
    /// [`Thrown::Value`] for `Err` of a `Result`.
    fn into_abi(self) -> Result<(Self::Ret, u32), Thrown>;
}

/// A type `Option<T>` can take as a parameter.
pub trait OptFromAbi: Sized + Describe {
    /// The first word: the presence tag, or a pointer that is 0 for `None`.
    type W0: Word;
    /// The second word.
    type W1: Word;

    /// `None` or the value for the words of `Option<Self>`.
    /// # Safety
    /// As [`FromAbi::from_abi`].
    /// # Errors
    /// As [`FromAbi::from_abi`].
    unsafe fn opt_from_abi(w0: Self::W0, w1: Self::W1) -> Result<Option<Self>, Thrown>;
}

/// A type `Option<T>` can return.
pub trait OptIntoAbi: Sized + Describe {
    /// The wasm result.
    type Ret: RetWord;

    /// The result word and slot value for `Option<Self>`.
    /// # Errors
    /// None today; the signature matches [`IntoAbi::into_abi`].
    fn opt_into_abi(v: Option<Self>) -> Result<(Self::Ret, u32), Thrown>;
}

/// An element type of `&[T]` and `Vec<T>`: a number with a JS typed array.
pub trait Elem: Copy + Describe {}

impl Describe for () {
    const DESC: TypeDesc = TypeDesc::tag(tag::UNIT);
}

impl IntoAbi for () {
    type Ret = ();
    #[inline]
    fn into_abi(self) -> Result<((), u32), Thrown> {
        Ok(((), 0))
    }
}

/// The message JS's `_assertChar` (and wasm-bindgen's) gives a code point
/// that is no `char`.
fn bad_char(c: u32) -> Thrown {
    Thrown::Error(format!("expected a valid Unicode scalar value, found {c}"))
}

macro_rules! scalars {
    ($($t:ty, $tag:ident, $w:ty, |$f:ident| $from:expr, |$v:ident| $into:expr;)*) => {$(
        impl Describe for $t {
            const DESC: TypeDesc = TypeDesc::tag(tag::$tag);
        }
        impl FromAbi for $t {
            type W0 = $w;
            type W1 = u32;
            #[inline]
            unsafe fn from_abi($f: $w, _: u32) -> Result<Self, Thrown> {
                $from
            }
        }
        impl IntoAbi for $t {
            type Ret = $w;
            #[inline]
            fn into_abi(self) -> Result<($w, u32), Thrown> {
                let $v = self;
                Ok(($into, 0))
            }
        }
        impl OptFromAbi for $t {
            type W0 = u32;
            type W1 = $w;
            #[inline]
            unsafe fn opt_from_abi(w0: u32, $f: $w) -> Result<Option<Self>, Thrown> {
                if w0 == 0 { Ok(None) } else { ($from).map(Some) }
            }
        }
        impl OptIntoAbi for $t {
            type Ret = $w;
            #[inline]
            fn opt_into_abi(v: Option<Self>) -> Result<($w, u32), Thrown> {
                Ok(match v {
                    Some($v) => ($into, 1),
                    None => (<$w>::default(), 0),
                })
            }
        }
    )*};
}

scalars! {
    bool, BOOL, u32, |w| Ok(w != 0), |v| u32::from(v);
    u8, U8, u32, |w| Ok(w as u8), |v| u32::from(v);
    u16, U16, u32, |w| Ok(w as u16), |v| u32::from(v);
    u32, U32, u32, |w| Ok(w), |v| v;
    usize, USIZE, usize, |w| Ok(w), |v| v;
    i8, I8, i32, |w| Ok(w as i8), |v| i32::from(v);
    i16, I16, i32, |w| Ok(w as i16), |v| i32::from(v);
    i32, I32, i32, |w| Ok(w), |v| v;
    isize, ISIZE, isize, |w| Ok(w), |v| v;
    u64, U64, u64, |w| Ok(w), |v| v;
    i64, I64, i64, |w| Ok(w), |v| v;
    f32, F32, f32, |w| Ok(w), |v| v;
    f64, F64, f64, |w| Ok(w), |v| v;
    char, CHAR, u32, |w| char::from_u32(w).ok_or_else(|| bad_char(w)), |v| u32::from(v);
}

macro_rules! elems {
    ($($t:ty),*) => {$(impl Elem for $t {})*};
}
elems!(u8, u16, u32, u64, i8, i16, i32, i64, f32, f64);

/// Take ownership of the `len`-element buffer the glue wrote at `ptr`.
/// # Safety
/// `FromAbi::from_abi`'s contract for a buffer of `T`.
#[inline]
unsafe fn take_vec<T>(ptr: usize, len: usize) -> Vec<T> {
    if len == 0 {
        // Nothing was allocated; `ptr` is the alignment.
        return Vec::new();
    }
    // SAFETY: the caller's contract: a live allocation of exactly `len` `T`s at `T`'s alignment from the global allocator, now owned here.
    unsafe { Vec::from_raw_parts(ptr as *mut T, len, len) }
}

/// Hand a buffer to the glue: its pointer and element count. The glue frees
/// it with `len * size_of::<T>()` bytes at `align_of::<T>()`.
#[inline]
fn give_vec<T>(v: Vec<T>) -> (usize, u32) {
    let b = v.into_boxed_slice();
    let len = b.len();
    let ptr = Box::into_raw(b).cast::<T>() as usize;
    (ptr, len as u32)
}

/// A string argument's bytes as a `String`.
/// # Safety
/// `bytes` is UTF-8 ([`FromAbi::from_abi`]'s contract for a string).
#[inline]
unsafe fn utf8(bytes: Vec<u8>) -> Result<String, Thrown> {
    if cfg!(debug_assertions) {
        return String::from_utf8(bytes)
            .map_err(|_| Thrown::Error("a string argument is not UTF-8".into()));
    }
    // SAFETY: the caller's contract.
    Ok(unsafe { String::from_utf8_unchecked(bytes) })
}

impl Describe for str {
    const DESC: TypeDesc = TypeDesc::tag(tag::STR);
}

impl RefFromAbi for str {
    type Anchor = String;
    type W0 = usize;
    type W1 = usize;
    unsafe fn ref_from_abi(ptr: usize, len: usize) -> Result<String, Thrown> {
        // SAFETY: the caller's contract.
        unsafe { String::from_abi(ptr, len) }
    }
}

impl Describe for String {
    const DESC: TypeDesc = TypeDesc::tag(tag::STRING);
}

impl FromAbi for String {
    type W0 = usize;
    type W1 = usize;
    unsafe fn from_abi(ptr: usize, len: usize) -> Result<Self, Thrown> {
        // SAFETY: the caller's contract: a buffer of UTF-8 bytes.
        unsafe { utf8(take_vec(ptr, len)) }
    }
}

impl IntoAbi for String {
    type Ret = usize;
    fn into_abi(self) -> Result<(usize, u32), Thrown> {
        Ok(give_vec(self.into_bytes()))
    }
}

impl OptFromAbi for String {
    type W0 = usize;
    type W1 = usize;
    unsafe fn opt_from_abi(ptr: usize, len: usize) -> Result<Option<Self>, Thrown> {
        if ptr == 0 {
            return Ok(None);
        }
        // SAFETY: the caller's contract.
        unsafe { Self::from_abi(ptr, len) }.map(Some)
    }
}

impl OptIntoAbi for String {
    type Ret = usize;
    fn opt_into_abi(v: Option<Self>) -> Result<(usize, u32), Thrown> {
        Ok(v.map_or((0, 0), |s| give_vec(s.into_bytes())))
    }
}

impl<T: Elem> Describe for [T] {
    const DESC: TypeDesc = TypeDesc::wrap(tag::SLICE, T::DESC);
}

impl<T: Elem> RefFromAbi for [T] {
    type Anchor = Vec<T>;
    type W0 = usize;
    type W1 = usize;
    unsafe fn ref_from_abi(ptr: usize, len: usize) -> Result<Vec<T>, Thrown> {
        // SAFETY: the caller's contract.
        Ok(unsafe { take_vec(ptr, len) })
    }
}

impl<T: Elem> Describe for Vec<T> {
    const DESC: TypeDesc = TypeDesc::wrap(tag::VEC, T::DESC);
}

impl<T: Elem> FromAbi for Vec<T> {
    type W0 = usize;
    type W1 = usize;
    unsafe fn from_abi(ptr: usize, len: usize) -> Result<Self, Thrown> {
        // SAFETY: the caller's contract.
        Ok(unsafe { take_vec(ptr, len) })
    }
}

impl<T: Elem> IntoAbi for Vec<T> {
    type Ret = usize;
    fn into_abi(self) -> Result<(usize, u32), Thrown> {
        Ok(give_vec(self))
    }
}

impl<T: Elem> OptFromAbi for Vec<T> {
    type W0 = usize;
    type W1 = usize;
    unsafe fn opt_from_abi(ptr: usize, len: usize) -> Result<Option<Self>, Thrown> {
        if ptr == 0 {
            return Ok(None);
        }
        // SAFETY: the caller's contract.
        Ok(Some(unsafe { take_vec(ptr, len) }))
    }
}

impl<T: Elem> OptIntoAbi for Vec<T> {
    type Ret = usize;
    fn opt_into_abi(v: Option<Self>) -> Result<(usize, u32), Thrown> {
        Ok(v.map_or((0, 0), give_vec))
    }
}

impl<T: Describe> Describe for Option<T> {
    const DESC: TypeDesc = TypeDesc::wrap(tag::OPTION, T::DESC);
}

impl<T: OptFromAbi> FromAbi for Option<T> {
    type W0 = T::W0;
    type W1 = T::W1;
    unsafe fn from_abi(w0: T::W0, w1: T::W1) -> Result<Self, Thrown> {
        // SAFETY: the caller's contract.
        unsafe { T::opt_from_abi(w0, w1) }
    }
}

impl<T: OptIntoAbi> IntoAbi for Option<T> {
    type Ret = T::Ret;
    fn into_abi(self) -> Result<(T::Ret, u32), Thrown> {
        T::opt_into_abi(self)
    }
}

/// An error type a `Result` export throws: as the JS string itself.
pub trait ThrowAbi: Describe {
    /// The thrown string.
    fn into_thrown(self) -> String;
}

impl ThrowAbi for String {
    fn into_thrown(self) -> String {
        self
    }
}

impl<T: Describe, E: Describe> Describe for Result<T, E> {
    const DESC: TypeDesc = TypeDesc::pair(tag::RESULT, T::DESC, E::DESC);
}

impl<T: IntoAbi, E: ThrowAbi> IntoAbi for Result<T, E> {
    type Ret = T::Ret;
    fn into_abi(self) -> Result<(T::Ret, u32), Thrown> {
        match self {
            Ok(v) => v.into_abi(),
            Err(e) => Err(Thrown::Value(e.into_thrown())),
        }
    }
}

#[cfg(test)]
mod tests {

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
}
