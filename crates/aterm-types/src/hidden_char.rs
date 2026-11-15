// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! The one table of characters no surface prints and no free text keeps, alab-policy's
//! (`alab_policy::text`): the chrome's sanitizer (`aterm_messages::text`), the session log's free
//! text (`aterm_slog::BoundedText`), a request's `why=` line
//! ([`crate::control_verbs::request_why`]) and the turn-end readers' sanitizer
//! (`alab_policy::needs`) all read it.

/// A character no chrome surface prints ([`alab_policy::text::is_hidden_char`]).
pub use alab_policy::text::is_hidden_char;
