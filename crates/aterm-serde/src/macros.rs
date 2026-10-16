// Copyright (c) Erick Tryzelaar, David Tolnay et al. (the serde authors)
// SPDX-License-Identifier: MIT OR Apache-2.0
//
// Derived from serde_core 1.0.228 src/macros.rs and modified by the
// aterm project in 2026.
// See ../LICENSE-MIT and ../LICENSE-APACHE for the upstream licenses.

// Super explicit first paragraph because this shows up at the top level and
// trips up people who are just looking for basic Serialize / Deserialize
// documentation.
/// Helper macro when implementing the `Deserializer` part of a new data format
/// for Serde.
///
/// Some [`Deserializer`] implementations for self-describing formats do not
/// care what hint the [`Visitor`] gives them, they just want to blindly call
/// the [`Visitor`] method corresponding to the data they can tell is in the
/// input. This requires repetitive implementations of all the [`Deserializer`]
/// trait methods.
///
/// The `forward_to_deserialize_any!` macro implements these simple forwarding
/// methods so that they forward directly to [`Deserializer::deserialize_any`].
/// You can choose which methods to forward.
///
/// The macro assumes the convention that your `Deserializer` lifetime parameter
/// is called `'de` and that the `Visitor` type parameters on each method are
/// called `V`. A different type parameter and a different lifetime can be
/// specified explicitly if necessary.
///
/// [`Deserializer`]: crate::Deserializer
/// [`Visitor`]: crate::de::Visitor
/// [`Deserializer::deserialize_any`]: crate::Deserializer::deserialize_any
#[macro_export(local_inner_macros)]
macro_rules! forward_to_deserialize_any {
    (<$visitor:ident: Visitor<$lifetime:tt>> $($func:ident)*) => {
        $(forward_to_deserialize_any_helper!{$func<$lifetime, $visitor>})*
    };
    // This case must be after the previous one.
    ($($func:ident)*) => {
        $(forward_to_deserialize_any_helper!{$func<'de, V>})*
    };
}

#[doc(hidden)]
#[macro_export]
macro_rules! forward_to_deserialize_any_method {
    ($func:ident<$l:tt, $v:ident>($($arg:ident : $ty:ty),*)) => {
        #[inline]
        fn $func<$v>(self, $($arg: $ty,)* visitor: $v) -> $crate::__private::Result<$v::Value, <Self as $crate::de::Deserializer<$l>>::Error>
        where
            $v: $crate::de::Visitor<$l>,
        {
            $(
                let _ = $arg;
            )*
            self.deserialize_any(visitor)
        }
    };
}

#[doc(hidden)]
#[macro_export(local_inner_macros)]
macro_rules! forward_to_deserialize_any_helper {
    (bool<$l:tt, $v:ident>) => {
        forward_to_deserialize_any_method!{deserialize_bool<$l, $v>()}
    };
    (i8<$l:tt, $v:ident>) => {
        forward_to_deserialize_any_method!{deserialize_i8<$l, $v>()}
    };
    (i16<$l:tt, $v:ident>) => {
        forward_to_deserialize_any_method!{deserialize_i16<$l, $v>()}
    };
    (i32<$l:tt, $v:ident>) => {
        forward_to_deserialize_any_method!{deserialize_i32<$l, $v>()}
    };
    (i64<$l:tt, $v:ident>) => {
        forward_to_deserialize_any_method!{deserialize_i64<$l, $v>()}
    };
    (i128<$l:tt, $v:ident>) => {
        forward_to_deserialize_any_method!{deserialize_i128<$l, $v>()}
    };
    (u8<$l:tt, $v:ident>) => {
        forward_to_deserialize_any_method!{deserialize_u8<$l, $v>()}
    };
    (u16<$l:tt, $v:ident>) => {
        forward_to_deserialize_any_method!{deserialize_u16<$l, $v>()}
    };
    (u32<$l:tt, $v:ident>) => {
        forward_to_deserialize_any_method!{deserialize_u32<$l, $v>()}
    };
    (u64<$l:tt, $v:ident>) => {
        forward_to_deserialize_any_method!{deserialize_u64<$l, $v>()}
    };
    (u128<$l:tt, $v:ident>) => {
        forward_to_deserialize_any_method!{deserialize_u128<$l, $v>()}
    };
    (f32<$l:tt, $v:ident>) => {
        forward_to_deserialize_any_method!{deserialize_f32<$l, $v>()}
    };
    (f64<$l:tt, $v:ident>) => {
        forward_to_deserialize_any_method!{deserialize_f64<$l, $v>()}
    };
    (char<$l:tt, $v:ident>) => {
        forward_to_deserialize_any_method!{deserialize_char<$l, $v>()}
    };
    (str<$l:tt, $v:ident>) => {
        forward_to_deserialize_any_method!{deserialize_str<$l, $v>()}
    };
    (string<$l:tt, $v:ident>) => {
        forward_to_deserialize_any_method!{deserialize_string<$l, $v>()}
    };
    (bytes<$l:tt, $v:ident>) => {
        forward_to_deserialize_any_method!{deserialize_bytes<$l, $v>()}
    };
    (byte_buf<$l:tt, $v:ident>) => {
        forward_to_deserialize_any_method!{deserialize_byte_buf<$l, $v>()}
    };
    (option<$l:tt, $v:ident>) => {
        forward_to_deserialize_any_method!{deserialize_option<$l, $v>()}
    };
    (unit<$l:tt, $v:ident>) => {
        forward_to_deserialize_any_method!{deserialize_unit<$l, $v>()}
    };
    (unit_struct<$l:tt, $v:ident>) => {
        forward_to_deserialize_any_method!{deserialize_unit_struct<$l, $v>(name: &'static str)}
    };
    (newtype_struct<$l:tt, $v:ident>) => {
        forward_to_deserialize_any_method!{deserialize_newtype_struct<$l, $v>(name: &'static str)}
    };
    (seq<$l:tt, $v:ident>) => {
        forward_to_deserialize_any_method!{deserialize_seq<$l, $v>()}
    };
    (tuple<$l:tt, $v:ident>) => {
        forward_to_deserialize_any_method!{deserialize_tuple<$l, $v>(len: usize)}
    };
    (tuple_struct<$l:tt, $v:ident>) => {
        forward_to_deserialize_any_method!{deserialize_tuple_struct<$l, $v>(name: &'static str, len: usize)}
    };
    (map<$l:tt, $v:ident>) => {
        forward_to_deserialize_any_method!{deserialize_map<$l, $v>()}
    };
    (struct<$l:tt, $v:ident>) => {
        forward_to_deserialize_any_method!{deserialize_struct<$l, $v>(name: &'static str, fields: &'static [&'static str])}
    };
    (enum<$l:tt, $v:ident>) => {
        forward_to_deserialize_any_method!{deserialize_enum<$l, $v>(name: &'static str, variants: &'static [&'static str])}
    };
    (identifier<$l:tt, $v:ident>) => {
        forward_to_deserialize_any_method!{deserialize_identifier<$l, $v>()}
    };
    (ignored_any<$l:tt, $v:ident>) => {
        forward_to_deserialize_any_method!{deserialize_ignored_any<$l, $v>()}
    };
}
