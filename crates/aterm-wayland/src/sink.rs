// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Where the events this crate's seats and selections decode go.

use std::sync::Arc;

/// A caller's sink: called once per event, in dispatch order, inside the dispatch.
pub type Sink<T> = Arc<dyn Fn(T) + Send + Sync>;
