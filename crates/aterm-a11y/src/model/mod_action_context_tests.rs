// Copyright 2021 The AccessKit Authors.
// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
//
// Derived from accesskit 0.24.1 (MIT OR Apache-2.0, used under Apache-2.0)
// src/lib.rs, and cut by the aterm project in 2026 to what aterm-gui builds
// and its publishers read: the roles and properties aterm sets (plus a
// link's URL and a value's step, which the AT-SPI publisher serves), stored
// as plain fields; one tree per window (a tree id names only the root tree);
// no serialization.
//
// Derived from Chromium's accessibility abstraction.
// Copyright 2018 The Chromium Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE-CHROMIUM file.

use super::ActionContext;

#[test]
fn native_review_identity_preserves_lifecycle_and_never_aliases_other_contexts() {
    let review = ActionContext::native_review(u64::MAX, u64::MAX, 9);
    let narrow = u64::from(u32::MAX);
    assert_eq!(review, ActionContext::native_review(u64::MAX, u64::MAX, 9));
    assert_ne!(review, ActionContext::native_review(narrow, u64::MAX, 9));
    assert_ne!(review, ActionContext::native_review(u64::MAX, narrow, 9));
    assert_ne!(review, ActionContext::new(u64::MAX, u64::MAX));
    assert_ne!(review, ActionContext::native_review(u64::MAX, u64::MAX, 10));
    assert_ne!(review, ActionContext::native_review(u64::MAX, 1, 9));
    assert_ne!(review, ActionContext::tab(u64::MAX));
}
