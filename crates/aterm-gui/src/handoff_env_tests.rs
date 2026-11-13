// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::*;

/// The accessors mirror the env calls: `string` refuses non-UTF-8 as `var` does, `take` is
/// one-shot, `set` keeps the capture order `exec_env` reports in.
#[test]
fn accessors_mirror_the_env_calls_they_replace() {
    use crate::seamless::{ENV_COMMIT_FD, ENV_FDS, ENV_MANIFEST};
    use std::os::unix::ffi::OsStringExt as _;
    let mut env = HandoffEnv::default();
    env.set(ENV_COMMIT_FD, "9");
    env.set(ENV_FDS, OsString::from_vec(vec![0xff, b'1']));
    env.set(ENV_MANIFEST, "/m");
    assert!(env.present(ENV_FDS));
    assert_eq!(env.string(ENV_FDS), None, "not UTF-8");
    assert!(env.os(ENV_FDS).is_some());
    assert_eq!(env.held(), [ENV_MANIFEST, ENV_FDS, ENV_COMMIT_FD]);
    assert_eq!(env.take(ENV_COMMIT_FD), Some("9".into()));
    assert_eq!(env.take(ENV_COMMIT_FD), None, "one-shot");
    env.remove(ENV_FDS);
    assert_eq!(
        env.exec_env(),
        [(
            OsString::from("ATERM_SEAMLESS_MANIFEST"),
            OsString::from("/m")
        )]
    );
}
