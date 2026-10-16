// Copyright (c) Erick Tryzelaar, David Tolnay et al. (the serde authors)
// SPDX-License-Identifier: MIT OR Apache-2.0
//
// Derived from serde_core 1.0.228 src/ser/impls.rs and modified by the
// aterm project in 2026.
// See ../../LICENSE-MIT and ../../LICENSE-APACHE for the upstream licenses.

use crate::lib::*;

use crate::ser::{Error, Serialize, SerializeTuple, Serializer};

macro_rules! primitive_impl {
    ($ty:ident, $method:ident $($cast:tt)*) => {
        impl Serialize for $ty {
            #[inline]
            fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
            where
                S: Serializer,
            {
                serializer.$method(*self $($cast)*)
            }
        }
    }
}

primitive_impl!(bool, serialize_bool);
primitive_impl!(isize, serialize_i64 as i64);
primitive_impl!(i8, serialize_i8);
primitive_impl!(i16, serialize_i16);
primitive_impl!(i32, serialize_i32);
primitive_impl!(i64, serialize_i64);
primitive_impl!(i128, serialize_i128);
primitive_impl!(usize, serialize_u64 as u64);
primitive_impl!(u8, serialize_u8);
primitive_impl!(u16, serialize_u16);
primitive_impl!(u32, serialize_u32);
primitive_impl!(u64, serialize_u64);
primitive_impl!(u128, serialize_u128);
primitive_impl!(f32, serialize_f32);
primitive_impl!(f64, serialize_f64);
primitive_impl!(char, serialize_char);

impl Serialize for str {
    #[inline]
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(self)
    }
}

impl Serialize for String {
    #[inline]
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(self)
    }
}

impl<T> Serialize for Option<T>
where
    T: Serialize,
{
    #[inline]
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        match *self {
            Some(ref value) => serializer.serialize_some(value),
            None => serializer.serialize_none(),
        }
    }
}

impl<T> Serialize for [T; 0] {
    #[inline]
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        tri!(serializer.serialize_tuple(0)).end()
    }
}

macro_rules! array_impls {
    ($($len:tt)+) => {
        $(
            impl<T> Serialize for [T; $len]
            where
                T: Serialize,
            {
                #[inline]
                fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
                where
                    S: Serializer,
                {
                    let mut seq = tri!(serializer.serialize_tuple($len));
                    for e in self {
                        tri!(seq.serialize_element(e));
                    }
                    seq.end()
                }
            }
        )+
    }
}

array_impls! {
    1 2 3 4 5 6 7 8 9 10
    11 12 13 14 15 16 17 18 19 20
    21 22 23 24 25 26 27 28 29 30
    31 32
}

impl<T> Serialize for [T]
where
    T: Serialize,
{
    #[inline]
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.collect_seq(self)
    }
}

impl<T> Serialize for Vec<T>
where
    T: Serialize,
{
    #[inline]
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.collect_seq(self)
    }
}

impl Serialize for () {
    #[inline]
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_unit()
    }
}

macro_rules! tuple_impls {
    ($($len:expr => ($($n:tt $name:ident)+))+) => {
        $(
            impl<$($name),+> Serialize for ($($name,)+)
            where
                $($name: Serialize,)+
            {
                tuple_impl_body!($len => ($($n)+));
            }
        )+
    };
}

macro_rules! tuple_impl_body {
    ($len:expr => ($($n:tt)+)) => {
        #[inline]
        fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
        where
            S: Serializer,
        {
            let mut tuple = tri!(serializer.serialize_tuple($len));
            $(
                tri!(tuple.serialize_element(&self.$n));
            )+
            tuple.end()
        }
    };
}

impl<T> Serialize for (T,)
where
    T: Serialize,
{
    tuple_impl_body!(1 => (0));
}

tuple_impls! {
    2 => (0 T0 1 T1)
    3 => (0 T0 1 T1 2 T2)
    4 => (0 T0 1 T1 2 T2 3 T3)
    5 => (0 T0 1 T1 2 T2 3 T3 4 T4)
    6 => (0 T0 1 T1 2 T2 3 T3 4 T4 5 T5)
    7 => (0 T0 1 T1 2 T2 3 T3 4 T4 5 T5 6 T6)
    8 => (0 T0 1 T1 2 T2 3 T3 4 T4 5 T5 6 T6 7 T7)
    9 => (0 T0 1 T1 2 T2 3 T3 4 T4 5 T5 6 T6 7 T7 8 T8)
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
        $ty:ident <K $(: $kbound1:ident $(+ $kbound2:ident)*)*, V $(, $typaram:ident : $bound:ident)*>
    ) => {
        $(#[$attr])*
        impl<K, V $(, $typaram)*> Serialize for $ty<K, V $(, $typaram)*>
        where
            K: Serialize,
            V: Serialize,
        {
            #[inline]
            fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
            where
                S: Serializer,
            {
                serializer.collect_map(self)
            }
        }
    }
}

map_impl! {
    BTreeMap<K: Ord, V>
}

map_impl! {
    HashMap<K: Eq + Hash, V, H: BuildHasher>
}

macro_rules! deref_impl {
    (
        $(#[$attr:meta])*
        <$($desc:tt)+
    ) => {
        $(#[$attr])*
        impl <$($desc)+ {
            #[inline]
            fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
            where
                S: Serializer,
            {
                (**self).serialize(serializer)
            }
        }
    };
}

deref_impl! {
    <'a, T> Serialize for &'a T where T: ?Sized + Serialize
}

deref_impl! {
    <'a, T> Serialize for &'a mut T where T: ?Sized + Serialize
}

deref_impl! {
    <T> Serialize for Box<T> where T: ?Sized + Serialize
}

impl Serialize for Path {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        match self.to_str() {
            Some(s) => s.serialize(serializer),
            None => Err(Error::custom("path contains invalid UTF-8 characters")),
        }
    }
}

impl Serialize for PathBuf {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        self.as_path().serialize(serializer)
    }
}
