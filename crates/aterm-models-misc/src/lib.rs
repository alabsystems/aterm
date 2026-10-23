// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

#![cfg_attr(trust_verify, feature(register_tool))]
#![cfg_attr(trust_verify, register_tool(trust))]

//! aterm's models of search, scrollback and the other families.

mod models_diskwalk;
mod models_misc;
mod models_vitals;

pub use models_diskwalk::*;
pub use models_misc::*;
pub use models_vitals::*;
