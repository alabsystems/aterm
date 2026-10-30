// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The private release keychain is shared with the installed credential subscriber.
#![cfg(unix)]
pub use aterm_secrets::release_keychain::*;
