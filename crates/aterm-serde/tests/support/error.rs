// Copyright (c) Erick Tryzelaar, David Tolnay et al. (the serde authors)
// SPDX-License-Identifier: MIT OR Apache-2.0
//
// Derived from serde_test 1.0.177 src/error.rs (github.com/serde-rs/test) and
// modified by the aterm project in 2026.
// See ../../LICENSE-MIT and ../../LICENSE-APACHE for the upstream licenses.

use aterm_serde::{de, ser};
use std::error;
use std::fmt::{self, Display};

#[derive(Clone, Debug)]
pub struct Error {
    msg: String,
}

impl ser::Error for Error {
    fn custom<T: Display>(msg: T) -> Self {
        Error {
            msg: msg.to_string(),
        }
    }
}

impl de::Error for Error {
    fn custom<T: Display>(msg: T) -> Self {
        Error {
            msg: msg.to_string(),
        }
    }
}

impl fmt::Display for Error {
    fn fmt(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
        formatter.write_str(&self.msg)
    }
}

impl error::Error for Error {}

impl PartialEq<str> for Error {
    fn eq(&self, other: &str) -> bool {
        self.msg == other
    }
}
