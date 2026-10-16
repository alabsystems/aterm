// Copyright (c) Erick Tryzelaar, David Tolnay et al. (the serde authors)
// SPDX-License-Identifier: MIT OR Apache-2.0
//
// Derived from serde_core 1.0.228 src/ser/impossible.rs and modified by the
// aterm project in 2026.
// See ../../LICENSE-MIT and ../../LICENSE-APACHE for the upstream licenses.

//! This module contains `Impossible` serializer and its implementations.

use crate::lib::*;

use crate::ser::{
    self, Serialize, SerializeMap, SerializeSeq, SerializeStruct, SerializeStructVariant,
    SerializeTuple, SerializeTupleStruct, SerializeTupleVariant,
};

/// Helper type for implementing a `Serializer` that does not support
/// serializing one of the compound types.
///
/// This type cannot be instantiated, but implements every one of the traits
/// corresponding to the [`Serializer`] compound types: [`SerializeSeq`],
/// [`SerializeTuple`], [`SerializeTupleStruct`], [`SerializeTupleVariant`],
/// [`SerializeMap`], [`SerializeStruct`], and [`SerializeStructVariant`].
///
/// [`Serializer`]: crate::Serializer
/// [`SerializeSeq`]: crate::ser::SerializeSeq
/// [`SerializeTuple`]: crate::ser::SerializeTuple
/// [`SerializeTupleStruct`]: crate::ser::SerializeTupleStruct
/// [`SerializeTupleVariant`]: crate::ser::SerializeTupleVariant
/// [`SerializeMap`]: crate::ser::SerializeMap
/// [`SerializeStruct`]: crate::ser::SerializeStruct
/// [`SerializeStructVariant`]: crate::ser::SerializeStructVariant
pub struct Impossible<Ok, Error> {
    void: Void,
    ok: PhantomData<Ok>,
    error: PhantomData<Error>,
}

enum Void {}

impl<Ok, Error> SerializeSeq for Impossible<Ok, Error>
where
    Error: ser::Error,
{
    type Ok = Ok;
    type Error = Error;

    fn serialize_element<T>(&mut self, value: &T) -> Result<(), Error>
    where
        T: ?Sized + Serialize,
    {
        let _ = value;
        match self.void {}
    }

    fn end(self) -> Result<Ok, Error> {
        match self.void {}
    }
}

impl<Ok, Error> SerializeTuple for Impossible<Ok, Error>
where
    Error: ser::Error,
{
    type Ok = Ok;
    type Error = Error;

    fn serialize_element<T>(&mut self, value: &T) -> Result<(), Error>
    where
        T: ?Sized + Serialize,
    {
        let _ = value;
        match self.void {}
    }

    fn end(self) -> Result<Ok, Error> {
        match self.void {}
    }
}

impl<Ok, Error> SerializeTupleStruct for Impossible<Ok, Error>
where
    Error: ser::Error,
{
    type Ok = Ok;
    type Error = Error;

    fn serialize_field<T>(&mut self, value: &T) -> Result<(), Error>
    where
        T: ?Sized + Serialize,
    {
        let _ = value;
        match self.void {}
    }

    fn end(self) -> Result<Ok, Error> {
        match self.void {}
    }
}

impl<Ok, Error> SerializeTupleVariant for Impossible<Ok, Error>
where
    Error: ser::Error,
{
    type Ok = Ok;
    type Error = Error;

    fn serialize_field<T>(&mut self, value: &T) -> Result<(), Error>
    where
        T: ?Sized + Serialize,
    {
        let _ = value;
        match self.void {}
    }

    fn end(self) -> Result<Ok, Error> {
        match self.void {}
    }
}

impl<Ok, Error> SerializeMap for Impossible<Ok, Error>
where
    Error: ser::Error,
{
    type Ok = Ok;
    type Error = Error;

    fn serialize_key<T>(&mut self, key: &T) -> Result<(), Error>
    where
        T: ?Sized + Serialize,
    {
        let _ = key;
        match self.void {}
    }

    fn serialize_value<T>(&mut self, value: &T) -> Result<(), Error>
    where
        T: ?Sized + Serialize,
    {
        let _ = value;
        match self.void {}
    }

    fn end(self) -> Result<Ok, Error> {
        match self.void {}
    }
}

impl<Ok, Error> SerializeStruct for Impossible<Ok, Error>
where
    Error: ser::Error,
{
    type Ok = Ok;
    type Error = Error;

    fn serialize_field<T>(&mut self, key: &'static str, value: &T) -> Result<(), Error>
    where
        T: ?Sized + Serialize,
    {
        let _ = key;
        let _ = value;
        match self.void {}
    }

    fn end(self) -> Result<Ok, Error> {
        match self.void {}
    }
}

impl<Ok, Error> SerializeStructVariant for Impossible<Ok, Error>
where
    Error: ser::Error,
{
    type Ok = Ok;
    type Error = Error;

    fn serialize_field<T>(&mut self, key: &'static str, value: &T) -> Result<(), Error>
    where
        T: ?Sized + Serialize,
    {
        let _ = key;
        let _ = value;
        match self.void {}
    }

    fn end(self) -> Result<Ok, Error> {
        match self.void {}
    }
}
