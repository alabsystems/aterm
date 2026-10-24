// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Integration test: `init_mode_from_flag` takes the flag's mode over the default.

use aterm_containment::ContainmentMode;

#[test]
fn init_from_flag_uses_the_flag_over_default() {
    crate::in_fresh_process(
        module_path!(),
        "init_from_flag_uses_the_flag_over_default",
        || {
            let mode =
                aterm_containment::init_mode_from_flag(Some("safety"), ContainmentMode::User)
                    .expect("init_mode_from_flag should succeed");

            assert_eq!(mode, ContainmentMode::Safety);

            assert_eq!(
                aterm_containment::try_current_mode(),
                Some(ContainmentMode::Safety)
            );

            let caps = aterm_containment::ContainmentPolicy::capabilities(mode);
            assert_eq!(caps.network, aterm_containment::NetworkCapability::Full);
            assert_eq!(
                caps.process,
                aterm_containment::ProcessCapability::Restricted
            );
            assert_eq!(caps.fs, aterm_containment::FsCapability::ProjectReadWrite);
        },
    );
}
