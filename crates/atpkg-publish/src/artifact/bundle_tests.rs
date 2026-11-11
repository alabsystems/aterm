// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use std::sync::Arc;

use super::*;
use crate::host::Hosts;
use crate::host::fake::FakeHost;

#[test]
fn parse_args_rejects_plus_prefixed_build() {
    let hosts = Hosts {
        staging: Arc::new(FakeHost::default()),
        channel: Arc::new(FakeHost::default()),
    };
    let mut ctx = Ctx::new(Path::new("/tmp/home"), None, None, 1_792_022_400, hosts);
    let args = [
        "trust",
        "--sysroot",
        "/tmp/sysroot",
        "--build",
        "+2026103001",
    ]
    .into_iter()
    .map(String::from)
    .collect::<Vec<_>>();
    assert!(parse_args(&mut ctx, &args).is_err());
}
