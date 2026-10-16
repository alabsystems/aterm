// Copyright (c) Erick Tryzelaar, David Tolnay et al. (the serde authors)
// SPDX-License-Identifier: MIT OR Apache-2.0
//
// Derived from serde_core 1.0.228 src/de/impls.rs and modified by the
// aterm project in 2026.
// See ../../LICENSE-MIT and ../../LICENSE-APACHE for the upstream licenses.

use crate::lib::*;

use crate::de::{Deserialize, Deserializer, Error, MapAccess, SeqAccess, Unexpected, Visitor};
use crate::private::InPlaceSeed;

use crate::private::size_hint;

struct UnitVisitor;

impl<'de> Visitor<'de> for UnitVisitor {
    type Value = ();

    fn expecting(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
        formatter.write_str("unit")
    }

    fn visit_unit<E>(self) -> Result<Self::Value, E>
    where
        E: Error,
    {
        Ok(())
    }
}

impl<'de> Deserialize<'de> for () {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        deserializer.deserialize_unit(UnitVisitor)
    }
}

struct BoolVisitor;

impl<'de> Visitor<'de> for BoolVisitor {
    type Value = bool;

    fn expecting(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
        formatter.write_str("a boolean")
    }

    fn visit_bool<E>(self, v: bool) -> Result<Self::Value, E>
    where
        E: Error,
    {
        Ok(v)
    }
}

impl<'de> Deserialize<'de> for bool {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        deserializer.deserialize_bool(BoolVisitor)
    }
}

macro_rules! impl_deserialize_num {
    ($primitive:ident, $deserialize:ident $($method:ident!($($val:ident : $visit:ident)*);)*) => {
        impl<'de> Deserialize<'de> for $primitive {
            #[inline]
            fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
            where
                D: Deserializer<'de>,
            {
                struct PrimitiveVisitor;

                impl<'de> Visitor<'de> for PrimitiveVisitor {
                    type Value = $primitive;

                    fn expecting(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
                        formatter.write_str(stringify!($primitive))
                    }

                    $($($method!($val : $visit);)*)*
                }

                deserializer.$deserialize(PrimitiveVisitor)
            }
        }
    };
}

macro_rules! num_self {
    ($ty:ident : $visit:ident) => {
        #[inline]
        fn $visit<E>(self, v: $ty) -> Result<Self::Value, E>
        where
            E: Error,
        {
            Ok(v)
        }
    };
}

macro_rules! num_as_self {
    ($ty:ident : $visit:ident) => {
        #[inline]
        fn $visit<E>(self, v: $ty) -> Result<Self::Value, E>
        where
            E: Error,
        {
            Ok(v as Self::Value)
        }
    };
}

macro_rules! num_as_copysign_self {
    ($ty:ident : $visit:ident) => {
        #[inline]
        fn $visit<E>(self, v: $ty) -> Result<Self::Value, E>
        where
            E: Error,
        {
            {
                // Preserve sign of NaN. The `as` produces a nondeterministic sign.
                let sign = if v.is_sign_positive() { 1.0 } else { -1.0 };
                Ok((v as Self::Value).copysign(sign))
            }
        }
    };
}

macro_rules! int_to_int {
    ($ty:ident : $visit:ident) => {
        #[inline]
        fn $visit<E>(self, v: $ty) -> Result<Self::Value, E>
        where
            E: Error,
        {
            Self::Value::try_from(v as i64)
                .map_err(|_| Error::invalid_value(Unexpected::Signed(v as i64), &self))
        }
    };
}

macro_rules! int_to_uint {
    ($ty:ident : $visit:ident) => {
        #[inline]
        fn $visit<E>(self, v: $ty) -> Result<Self::Value, E>
        where
            E: Error,
        {
            if 0 <= v {
                #[allow(irrefutable_let_patterns)]
                if let Ok(v) = Self::Value::try_from(v as u64) {
                    return Ok(v as Self::Value);
                }
            }
            Err(Error::invalid_value(Unexpected::Signed(v as i64), &self))
        }
    };
}

macro_rules! uint_to_self {
    ($ty:ident : $visit:ident) => {
        #[inline]
        fn $visit<E>(self, v: $ty) -> Result<Self::Value, E>
        where
            E: Error,
        {
            Self::Value::try_from(v as u64)
                .map_err(|_| Error::invalid_value(Unexpected::Unsigned(v as u64), &self))
        }
    };
}

impl_deserialize_num! {
    i8, deserialize_i8
    num_self!(i8:visit_i8);
    int_to_int!(i16:visit_i16 i32:visit_i32 i64:visit_i64);
    uint_to_self!(u8:visit_u8 u16:visit_u16 u32:visit_u32 u64:visit_u64);
}

impl_deserialize_num! {
    i16, deserialize_i16
    num_self!(i16:visit_i16);
    num_as_self!(i8:visit_i8);
    int_to_int!(i32:visit_i32 i64:visit_i64);
    uint_to_self!(u8:visit_u8 u16:visit_u16 u32:visit_u32 u64:visit_u64);
}

impl_deserialize_num! {
    i32, deserialize_i32
    num_self!(i32:visit_i32);
    num_as_self!(i8:visit_i8 i16:visit_i16);
    int_to_int!(i64:visit_i64);
    uint_to_self!(u8:visit_u8 u16:visit_u16 u32:visit_u32 u64:visit_u64);
}

impl_deserialize_num! {
    i64, deserialize_i64
    num_self!(i64:visit_i64);
    num_as_self!(i8:visit_i8 i16:visit_i16 i32:visit_i32);
    uint_to_self!(u8:visit_u8 u16:visit_u16 u32:visit_u32 u64:visit_u64);
}

impl_deserialize_num! {
    isize, deserialize_i64
    num_as_self!(i8:visit_i8 i16:visit_i16);
    int_to_int!(i32:visit_i32 i64:visit_i64);
    uint_to_self!(u8:visit_u8 u16:visit_u16 u32:visit_u32 u64:visit_u64);
}

impl_deserialize_num! {
    u8, deserialize_u8
    num_self!(u8:visit_u8);
    int_to_uint!(i8:visit_i8 i16:visit_i16 i32:visit_i32 i64:visit_i64);
    uint_to_self!(u16:visit_u16 u32:visit_u32 u64:visit_u64);
}

impl_deserialize_num! {
    u16, deserialize_u16
    num_self!(u16:visit_u16);
    num_as_self!(u8:visit_u8);
    int_to_uint!(i8:visit_i8 i16:visit_i16 i32:visit_i32 i64:visit_i64);
    uint_to_self!(u32:visit_u32 u64:visit_u64);
}

impl_deserialize_num! {
    u32, deserialize_u32
    num_self!(u32:visit_u32);
    num_as_self!(u8:visit_u8 u16:visit_u16);
    int_to_uint!(i8:visit_i8 i16:visit_i16 i32:visit_i32 i64:visit_i64);
    uint_to_self!(u64:visit_u64);
}

impl_deserialize_num! {
    u64, deserialize_u64
    num_self!(u64:visit_u64);
    num_as_self!(u8:visit_u8 u16:visit_u16 u32:visit_u32);
    int_to_uint!(i8:visit_i8 i16:visit_i16 i32:visit_i32 i64:visit_i64);
}

impl_deserialize_num! {
    usize, deserialize_u64
    num_as_self!(u8:visit_u8 u16:visit_u16);
    int_to_uint!(i8:visit_i8 i16:visit_i16 i32:visit_i32 i64:visit_i64);
    uint_to_self!(u32:visit_u32 u64:visit_u64);
}

impl_deserialize_num! {
    f32, deserialize_f32
    num_self!(f32:visit_f32);
    num_as_copysign_self!(f64:visit_f64);
    num_as_self!(i8:visit_i8 i16:visit_i16 i32:visit_i32 i64:visit_i64);
    num_as_self!(u8:visit_u8 u16:visit_u16 u32:visit_u32 u64:visit_u64);
}

impl_deserialize_num! {
    f64, deserialize_f64
    num_self!(f64:visit_f64);
    num_as_copysign_self!(f32:visit_f32);
    num_as_self!(i8:visit_i8 i16:visit_i16 i32:visit_i32 i64:visit_i64);
    num_as_self!(u8:visit_u8 u16:visit_u16 u32:visit_u32 u64:visit_u64);
}

macro_rules! num_128 {
    ($ty:ident : $visit:ident) => {
        fn $visit<E>(self, v: $ty) -> Result<Self::Value, E>
        where
            E: Error,
        {
            if v as i128 >= Self::Value::MIN as i128 && v as u128 <= Self::Value::MAX as u128 {
                Ok(v as Self::Value)
            } else {
                Err(Error::invalid_value(
                    Unexpected::Other(stringify!($ty)),
                    &self,
                ))
            }
        }
    };
}

impl_deserialize_num! {
    i128, deserialize_i128
    num_self!(i128:visit_i128);
    num_as_self!(i8:visit_i8 i16:visit_i16 i32:visit_i32 i64:visit_i64);
    num_as_self!(u8:visit_u8 u16:visit_u16 u32:visit_u32 u64:visit_u64);
    num_128!(u128:visit_u128);
}

impl_deserialize_num! {
    u128, deserialize_u128
    num_self!(u128:visit_u128);
    num_as_self!(u8:visit_u8 u16:visit_u16 u32:visit_u32 u64:visit_u64);
    int_to_uint!(i8:visit_i8 i16:visit_i16 i32:visit_i32 i64:visit_i64);
    num_128!(i128:visit_i128);
}

struct CharVisitor;

impl<'de> Visitor<'de> for CharVisitor {
    type Value = char;

    fn expecting(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
        formatter.write_str("a character")
    }

    #[inline]
    fn visit_char<E>(self, v: char) -> Result<Self::Value, E>
    where
        E: Error,
    {
        Ok(v)
    }

    #[inline]
    fn visit_str<E>(self, v: &str) -> Result<Self::Value, E>
    where
        E: Error,
    {
        let mut iter = v.chars();
        match (iter.next(), iter.next()) {
            (Some(c), None) => Ok(c),
            _ => Err(Error::invalid_value(Unexpected::Str(v), &self)),
        }
    }
}

impl<'de> Deserialize<'de> for char {
    #[inline]
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        deserializer.deserialize_char(CharVisitor)
    }
}

struct StringVisitor;
struct StringInPlaceVisitor<'a>(&'a mut String);

impl<'de> Visitor<'de> for StringVisitor {
    type Value = String;

    fn expecting(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
        formatter.write_str("a string")
    }

    fn visit_str<E>(self, v: &str) -> Result<Self::Value, E>
    where
        E: Error,
    {
        Ok(v.to_owned())
    }

    fn visit_string<E>(self, v: String) -> Result<Self::Value, E>
    where
        E: Error,
    {
        Ok(v)
    }

    fn visit_bytes<E>(self, v: &[u8]) -> Result<Self::Value, E>
    where
        E: Error,
    {
        match str::from_utf8(v) {
            Ok(s) => Ok(s.to_owned()),
            Err(_) => Err(Error::invalid_value(Unexpected::Bytes(v), &self)),
        }
    }

    fn visit_byte_buf<E>(self, v: Vec<u8>) -> Result<Self::Value, E>
    where
        E: Error,
    {
        match String::from_utf8(v) {
            Ok(s) => Ok(s),
            Err(e) => Err(Error::invalid_value(
                Unexpected::Bytes(&e.into_bytes()),
                &self,
            )),
        }
    }
}

impl<'a, 'de> Visitor<'de> for StringInPlaceVisitor<'a> {
    type Value = ();

    fn expecting(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
        formatter.write_str("a string")
    }

    fn visit_str<E>(self, v: &str) -> Result<Self::Value, E>
    where
        E: Error,
    {
        self.0.clear();
        self.0.push_str(v);
        Ok(())
    }

    fn visit_string<E>(self, v: String) -> Result<Self::Value, E>
    where
        E: Error,
    {
        *self.0 = v;
        Ok(())
    }

    fn visit_bytes<E>(self, v: &[u8]) -> Result<Self::Value, E>
    where
        E: Error,
    {
        match str::from_utf8(v) {
            Ok(s) => {
                self.0.clear();
                self.0.push_str(s);
                Ok(())
            }
            Err(_) => Err(Error::invalid_value(Unexpected::Bytes(v), &self)),
        }
    }

    fn visit_byte_buf<E>(self, v: Vec<u8>) -> Result<Self::Value, E>
    where
        E: Error,
    {
        match String::from_utf8(v) {
            Ok(s) => {
                *self.0 = s;
                Ok(())
            }
            Err(e) => Err(Error::invalid_value(
                Unexpected::Bytes(&e.into_bytes()),
                &self,
            )),
        }
    }
}

impl<'de> Deserialize<'de> for String {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        deserializer.deserialize_string(StringVisitor)
    }

    fn deserialize_in_place<D>(deserializer: D, place: &mut Self) -> Result<(), D::Error>
    where
        D: Deserializer<'de>,
    {
        deserializer.deserialize_string(StringInPlaceVisitor(place))
    }
}

macro_rules! forwarded_impl {
    (
        $(#[$attr:meta])*
        ($($id:ident),*), $ty:ty, $func:expr
    ) => {
        $(#[$attr])*
        impl<'de $(, $id : Deserialize<'de>,)*> Deserialize<'de> for $ty {
            fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
            where
                D: Deserializer<'de>,
            {
                Deserialize::deserialize(deserializer).map($func)
            }
        }
    }
}

struct OptionVisitor<T> {
    marker: PhantomData<T>,
}

impl<'de, T> Visitor<'de> for OptionVisitor<T>
where
    T: Deserialize<'de>,
{
    type Value = Option<T>;

    fn expecting(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
        formatter.write_str("option")
    }

    #[inline]
    fn visit_unit<E>(self) -> Result<Self::Value, E>
    where
        E: Error,
    {
        Ok(None)
    }

    #[inline]
    fn visit_none<E>(self) -> Result<Self::Value, E>
    where
        E: Error,
    {
        Ok(None)
    }

    #[inline]
    fn visit_some<D>(self, deserializer: D) -> Result<Self::Value, D::Error>
    where
        D: Deserializer<'de>,
    {
        T::deserialize(deserializer).map(Some)
    }

    fn __private_visit_untagged_option<D>(self, deserializer: D) -> Result<Self::Value, ()>
    where
        D: Deserializer<'de>,
    {
        Ok(T::deserialize(deserializer).ok())
    }
}

impl<'de, T> Deserialize<'de> for Option<T>
where
    T: Deserialize<'de>,
{
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        deserializer.deserialize_option(OptionVisitor {
            marker: PhantomData,
        })
    }

    // The Some variant's repr is opaque, so we can't play cute tricks with its
    // tag to have deserialize_in_place build the content in place unconditionally.
    //
    // FIXME: investigate whether branching on the old value being Some to
    // deserialize_in_place the value is profitable (probably data-dependent?)
}

impl<'de, T> Deserialize<'de> for Vec<T>
where
    T: Deserialize<'de>,
{
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        struct VecVisitor<T> {
            marker: PhantomData<T>,
        }

        impl<'de, T> Visitor<'de> for VecVisitor<T>
        where
            T: Deserialize<'de>,
        {
            type Value = Vec<T>;

            fn expecting(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
                formatter.write_str("a sequence")
            }

            fn visit_seq<A>(self, mut seq: A) -> Result<Self::Value, A::Error>
            where
                A: SeqAccess<'de>,
            {
                let capacity = size_hint::cautious::<T>(seq.size_hint());
                let mut values = Vec::<T>::with_capacity(capacity);

                while let Some(value) = tri!(seq.next_element()) {
                    values.push(value);
                }

                Ok(values)
            }
        }

        let visitor = VecVisitor {
            marker: PhantomData,
        };
        deserializer.deserialize_seq(visitor)
    }

    fn deserialize_in_place<D>(deserializer: D, place: &mut Self) -> Result<(), D::Error>
    where
        D: Deserializer<'de>,
    {
        struct VecInPlaceVisitor<'a, T: 'a>(&'a mut Vec<T>);

        impl<'a, 'de, T> Visitor<'de> for VecInPlaceVisitor<'a, T>
        where
            T: Deserialize<'de>,
        {
            type Value = ();

            fn expecting(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
                formatter.write_str("a sequence")
            }

            fn visit_seq<A>(self, mut seq: A) -> Result<Self::Value, A::Error>
            where
                A: SeqAccess<'de>,
            {
                let hint = size_hint::cautious::<T>(seq.size_hint());
                if let Some(additional) = hint.checked_sub(self.0.len()) {
                    self.0.reserve(additional);
                }

                for i in 0..self.0.len() {
                    let next = {
                        let next_place = InPlaceSeed(&mut self.0[i]);
                        tri!(seq.next_element_seed(next_place))
                    };
                    if next.is_none() {
                        self.0.truncate(i);
                        return Ok(());
                    }
                }

                while let Some(value) = tri!(seq.next_element()) {
                    self.0.push(value);
                }

                Ok(())
            }
        }

        deserializer.deserialize_seq(VecInPlaceVisitor(place))
    }
}

struct ArrayVisitor<A> {
    marker: PhantomData<A>,
}
struct ArrayInPlaceVisitor<'a, A: 'a>(&'a mut A);

impl<A> ArrayVisitor<A> {
    fn new() -> Self {
        ArrayVisitor {
            marker: PhantomData,
        }
    }
}

impl<'de, T> Visitor<'de> for ArrayVisitor<[T; 0]> {
    type Value = [T; 0];

    fn expecting(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
        formatter.write_str("an empty array")
    }

    #[inline]
    fn visit_seq<A>(self, _: A) -> Result<Self::Value, A::Error>
    where
        A: SeqAccess<'de>,
    {
        Ok([])
    }
}

// Does not require T: Deserialize<'de>.
impl<'de, T> Deserialize<'de> for [T; 0] {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        deserializer.deserialize_tuple(0, ArrayVisitor::<[T; 0]>::new())
    }
}

macro_rules! array_impls {
    ($($len:expr => ($($n:tt)+))+) => {
        $(
            impl<'de, T> Visitor<'de> for ArrayVisitor<[T; $len]>
            where
                T: Deserialize<'de>,
            {
                type Value = [T; $len];

                fn expecting(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
                    formatter.write_str(concat!("an array of length ", $len))
                }

                #[inline]
                fn visit_seq<A>(self, mut seq: A) -> Result<Self::Value, A::Error>
                where
                    A: SeqAccess<'de>,
                {
                    Ok([$(
                        match tri!(seq.next_element()) {
                            Some(val) => val,
                            None => return Err(Error::invalid_length($n, &self)),
                        }
                    ),+])
                }
            }

            impl<'a, 'de, T> Visitor<'de> for ArrayInPlaceVisitor<'a, [T; $len]>
            where
                T: Deserialize<'de>,
            {
                type Value = ();

                fn expecting(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
                    formatter.write_str(concat!("an array of length ", $len))
                }

                #[inline]
                fn visit_seq<A>(self, mut seq: A) -> Result<Self::Value, A::Error>
                where
                    A: SeqAccess<'de>,
                {
                    let mut fail_idx = None;
                    for (idx, dest) in self.0[..].iter_mut().enumerate() {
                        if tri!(seq.next_element_seed(InPlaceSeed(dest))).is_none() {
                            fail_idx = Some(idx);
                            break;
                        }
                    }
                    if let Some(idx) = fail_idx {
                        return Err(Error::invalid_length(idx, &self));
                    }
                    Ok(())
                }
            }

            impl<'de, T> Deserialize<'de> for [T; $len]
            where
                T: Deserialize<'de>,
            {
                fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
                where
                    D: Deserializer<'de>,
                {
                    deserializer.deserialize_tuple($len, ArrayVisitor::<[T; $len]>::new())
                }

                fn deserialize_in_place<D>(deserializer: D, place: &mut Self) -> Result<(), D::Error>
                where
                    D: Deserializer<'de>,
                {
                    deserializer.deserialize_tuple($len, ArrayInPlaceVisitor(place))
                }
            }
        )+
    }
}

array_impls! {
    1 => (0)
    2 => (0 1)
    3 => (0 1 2)
    4 => (0 1 2 3)
    5 => (0 1 2 3 4)
    6 => (0 1 2 3 4 5)
    7 => (0 1 2 3 4 5 6)
    8 => (0 1 2 3 4 5 6 7)
    9 => (0 1 2 3 4 5 6 7 8)
    10 => (0 1 2 3 4 5 6 7 8 9)
    11 => (0 1 2 3 4 5 6 7 8 9 10)
    12 => (0 1 2 3 4 5 6 7 8 9 10 11)
    13 => (0 1 2 3 4 5 6 7 8 9 10 11 12)
    14 => (0 1 2 3 4 5 6 7 8 9 10 11 12 13)
    15 => (0 1 2 3 4 5 6 7 8 9 10 11 12 13 14)
    16 => (0 1 2 3 4 5 6 7 8 9 10 11 12 13 14 15)
    17 => (0 1 2 3 4 5 6 7 8 9 10 11 12 13 14 15 16)
    18 => (0 1 2 3 4 5 6 7 8 9 10 11 12 13 14 15 16 17)
    19 => (0 1 2 3 4 5 6 7 8 9 10 11 12 13 14 15 16 17 18)
    20 => (0 1 2 3 4 5 6 7 8 9 10 11 12 13 14 15 16 17 18 19)
    21 => (0 1 2 3 4 5 6 7 8 9 10 11 12 13 14 15 16 17 18 19 20)
    22 => (0 1 2 3 4 5 6 7 8 9 10 11 12 13 14 15 16 17 18 19 20 21)
    23 => (0 1 2 3 4 5 6 7 8 9 10 11 12 13 14 15 16 17 18 19 20 21 22)
    24 => (0 1 2 3 4 5 6 7 8 9 10 11 12 13 14 15 16 17 18 19 20 21 22 23)
    25 => (0 1 2 3 4 5 6 7 8 9 10 11 12 13 14 15 16 17 18 19 20 21 22 23 24)
    26 => (0 1 2 3 4 5 6 7 8 9 10 11 12 13 14 15 16 17 18 19 20 21 22 23 24 25)
    27 => (0 1 2 3 4 5 6 7 8 9 10 11 12 13 14 15 16 17 18 19 20 21 22 23 24 25 26)
    28 => (0 1 2 3 4 5 6 7 8 9 10 11 12 13 14 15 16 17 18 19 20 21 22 23 24 25 26 27)
    29 => (0 1 2 3 4 5 6 7 8 9 10 11 12 13 14 15 16 17 18 19 20 21 22 23 24 25 26 27 28)
    30 => (0 1 2 3 4 5 6 7 8 9 10 11 12 13 14 15 16 17 18 19 20 21 22 23 24 25 26 27 28 29)
    31 => (0 1 2 3 4 5 6 7 8 9 10 11 12 13 14 15 16 17 18 19 20 21 22 23 24 25 26 27 28 29 30)
    32 => (0 1 2 3 4 5 6 7 8 9 10 11 12 13 14 15 16 17 18 19 20 21 22 23 24 25 26 27 28 29 30 31)
}

macro_rules! tuple_impls {
    ($($len:tt => ($($n:tt $name:ident)+))+) => {
        $(
            impl<'de, $($name),+> Deserialize<'de> for ($($name,)+)
            where
                $($name: Deserialize<'de>,)+
            {
                tuple_impl_body!($len => ($($n $name)+));
            }
        )+
    };
}

macro_rules! tuple_impl_body {
    ($len:tt => ($($n:tt $name:ident)+)) => {
        #[inline]
        fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
        where
            D: Deserializer<'de>,
        {
            struct TupleVisitor<$($name,)+> {
                marker: PhantomData<($($name,)+)>,
            }

            impl<'de, $($name: Deserialize<'de>),+> Visitor<'de> for TupleVisitor<$($name,)+> {
                type Value = ($($name,)+);

                fn expecting(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
                    formatter.write_str(concat!("a tuple of size ", $len))
                }

                #[inline]
                #[allow(non_snake_case)]
                fn visit_seq<A>(self, mut seq: A) -> Result<Self::Value, A::Error>
                where
                    A: SeqAccess<'de>,
                {
                    $(
                        let $name = match tri!(seq.next_element()) {
                            Some(value) => value,
                            None => return Err(Error::invalid_length($n, &self)),
                        };
                    )+

                    Ok(($($name,)+))
                }
            }

            deserializer.deserialize_tuple($len, TupleVisitor { marker: PhantomData })
        }

        #[inline]
        fn deserialize_in_place<D>(deserializer: D, place: &mut Self) -> Result<(), D::Error>
        where
            D: Deserializer<'de>,
        {
            struct TupleInPlaceVisitor<'a, $($name: 'a,)+>(&'a mut ($($name,)+));

            impl<'a, 'de, $($name: Deserialize<'de>),+> Visitor<'de> for TupleInPlaceVisitor<'a, $($name,)+> {
                type Value = ();

                fn expecting(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
                    formatter.write_str(concat!("a tuple of size ", $len))
                }

                #[inline]
                #[allow(non_snake_case)]
                fn visit_seq<A>(self, mut seq: A) -> Result<Self::Value, A::Error>
                where
                    A: SeqAccess<'de>,
                {
                    $(
                        if tri!(seq.next_element_seed(InPlaceSeed(&mut (self.0).$n))).is_none() {
                            return Err(Error::invalid_length($n, &self));
                        }
                    )+

                    Ok(())
                }
            }

            deserializer.deserialize_tuple($len, TupleInPlaceVisitor(place))
        }
    };
}

impl<'de, T> Deserialize<'de> for (T,)
where
    T: Deserialize<'de>,
{
    tuple_impl_body!(1 => (0 T));
}

tuple_impls! {
    2  => (0 T0 1 T1)
    3  => (0 T0 1 T1 2 T2)
    4  => (0 T0 1 T1 2 T2 3 T3)
    5  => (0 T0 1 T1 2 T2 3 T3 4 T4)
    6  => (0 T0 1 T1 2 T2 3 T3 4 T4 5 T5)
    7  => (0 T0 1 T1 2 T2 3 T3 4 T4 5 T5 6 T6)
    8  => (0 T0 1 T1 2 T2 3 T3 4 T4 5 T5 6 T6 7 T7)
    9  => (0 T0 1 T1 2 T2 3 T3 4 T4 5 T5 6 T6 7 T7 8 T8)
    10 => (0 T0 1 T1 2 T2 3 T3 4 T4 5 T5 6 T6 7 T7 8 T8 9 T9)
    11 => (0 T0 1 T1 2 T2 3 T3 4 T4 5 T5 6 T6 7 T7 8 T8 9 T9 10 T10)
    12 => (0 T0 1 T1 2 T2 3 T3 4 T4 5 T5 6 T6 7 T7 8 T8 9 T9 10 T10 11 T11)
    13 => (0 T0 1 T1 2 T2 3 T3 4 T4 5 T5 6 T6 7 T7 8 T8 9 T9 10 T10 11 T11 12 T12)
    14 => (0 T0 1 T1 2 T2 3 T3 4 T4 5 T5 6 T6 7 T7 8 T8 9 T9 10 T10 11 T11 12 T12 13 T13)
    15 => (0 T0 1 T1 2 T2 3 T3 4 T4 5 T5 6 T6 7 T7 8 T8 9 T9 10 T10 11 T11 12 T12 13 T13 14 T14)
    16 => (0 T0 1 T1 2 T2 3 T3 4 T4 5 T5 6 T6 7 T7 8 T8 9 T9 10 T10 11 T11 12 T12 13 T13 14 T14 15 T15)
}

macro_rules! map_impl {
    (
        $(#[$attr:meta])*
        $ty:ident <K $(: $kbound1:ident $(+ $kbound2:ident)*)*, V $(, $typaram:ident : $bound1:ident $(+ $bound2:ident)*)*>,
        $access:ident,
        $with_capacity:expr,
    ) => {
        $(#[$attr])*
        impl<'de, K, V $(, $typaram)*> Deserialize<'de> for $ty<K, V $(, $typaram)*>
        where
            K: Deserialize<'de> $(+ $kbound1 $(+ $kbound2)*)*,
            V: Deserialize<'de>,
            $($typaram: $bound1 $(+ $bound2)*),*
        {
            fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
            where
                D: Deserializer<'de>,
            {
                struct MapVisitor<K, V $(, $typaram)*> {
                    marker: PhantomData<$ty<K, V $(, $typaram)*>>,
                }

                impl<'de, K, V $(, $typaram)*> Visitor<'de> for MapVisitor<K, V $(, $typaram)*>
                where
                    K: Deserialize<'de> $(+ $kbound1 $(+ $kbound2)*)*,
                    V: Deserialize<'de>,
                    $($typaram: $bound1 $(+ $bound2)*),*
                {
                    type Value = $ty<K, V $(, $typaram)*>;

                    fn expecting(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
                        formatter.write_str("a map")
                    }

                    #[inline]
                    fn visit_map<A>(self, mut $access: A) -> Result<Self::Value, A::Error>
                    where
                        A: MapAccess<'de>,
                    {
                        let mut values = $with_capacity;

                        while let Some((key, value)) = tri!($access.next_entry()) {
                            values.insert(key, value);
                        }

                        Ok(values)
                    }
                }

                let visitor = MapVisitor { marker: PhantomData };
                deserializer.deserialize_map(visitor)
            }
        }
    }
}

map_impl! {
    BTreeMap<K: Ord, V>,
    map,
    BTreeMap::new(),
}

map_impl! {
    HashMap<K: Eq + Hash, V, S: BuildHasher + Default>,
    map,
    HashMap::with_capacity_and_hasher(size_hint::cautious::<(K, V)>(map.size_hint()), S::default()),
}

struct PathBufVisitor;

impl<'de> Visitor<'de> for PathBufVisitor {
    type Value = PathBuf;

    fn expecting(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
        formatter.write_str("path string")
    }

    fn visit_str<E>(self, v: &str) -> Result<Self::Value, E>
    where
        E: Error,
    {
        Ok(From::from(v))
    }

    fn visit_string<E>(self, v: String) -> Result<Self::Value, E>
    where
        E: Error,
    {
        Ok(From::from(v))
    }

    fn visit_bytes<E>(self, v: &[u8]) -> Result<Self::Value, E>
    where
        E: Error,
    {
        str::from_utf8(v)
            .map(From::from)
            .map_err(|_| Error::invalid_value(Unexpected::Bytes(v), &self))
    }

    fn visit_byte_buf<E>(self, v: Vec<u8>) -> Result<Self::Value, E>
    where
        E: Error,
    {
        String::from_utf8(v)
            .map(From::from)
            .map_err(|e| Error::invalid_value(Unexpected::Bytes(&e.into_bytes()), &self))
    }
}

impl<'de> Deserialize<'de> for PathBuf {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        deserializer.deserialize_string(PathBufVisitor)
    }
}

forwarded_impl! {
    (T), Box<T>, Box::new
}
