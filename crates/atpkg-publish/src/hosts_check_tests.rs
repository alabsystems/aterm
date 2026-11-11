// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
// Author: Andrew Yates

use super::{HostCheck, render};
use crate::ctx::HostChoice;

#[test]
fn render_is_one_sorted_object() {
    assert_eq!(
        render(
            HostChoice::Agh,
            &[HostCheck {
                name: "config",
                ok: true,
                detail: "d".into(),
            }]
        ),
        r#"{"checks":[{"detail":"d","name":"config","ok":true}],"host":"agh","ok":true}"#
    );
}
