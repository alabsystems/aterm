// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The pages the fleet views print (design §4.14 "Fleet views are computed locally"): `aterm
//! fleet metrics` (tokens per hour by Mac and in all, and the accounts near a wall) and the
//! fleet columns of `aterm harness accounts`, rendered from [`crate::merge`]'s view model.
//!
//! Empty until Q5, which writes the pages beside the readers that call them. Everything a
//! page shows is already in the view model: each Mac's row with its age, the merged figures
//! with the Mac and time they are credited to, "reset at T, not read since", and a quiet Mac's
//! latest window.
