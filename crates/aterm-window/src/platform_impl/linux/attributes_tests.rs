// Copyright The winit contributors
// SPDX-License-Identifier: Apache-2.0
//
// Derived from winit 0.30.13 src/platform_impl/linux/mod.rs,
// modified by the aterm project in 2026 (see the repository NOTICE).

use super::{ApplicationName, PlatformSpecificWindowAttributes};

#[test]
fn activation_token_debug_reports_presence_without_disclosing_value() {
    let marker = "synthetic-desktop-activation-marker";
    let attributes = PlatformSpecificWindowAttributes {
        name: Some(ApplicationName {
            general: "demo-app".into(),
            instance: "demo-instance".into(),
        }),
        activation_token: Some(marker.into()),
        x11_background: Some(0x123456),
    };
    for shown in [format!("{attributes:?}"), format!("{attributes:#?}")] {
        assert!(!shown.contains(marker));
        assert!(shown.contains("activation_token: true"));
        assert!(shown.contains("demo-app"));
    }
    assert_eq!(attributes.clone().activation_token.as_deref(), Some(marker));
    let absent = format!("{:?}", PlatformSpecificWindowAttributes::default());
    assert!(absent.contains("activation_token: false"));
}
