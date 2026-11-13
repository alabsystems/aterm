// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
#[test]
fn exact_environment_drops_inheritance_and_later_exports_overlay_it() {
    let mut command = transparent("/bin/sh");
    command.env("BEFORE", "discard");
    command.env_exactly([("ONLY", "first")]).env("ONLY", "last");
    let image = command.image().unwrap();
    assert_eq!(image.envp.len(), 1);
    assert!(image.envp[0].as_bytes() == b"ONLY=last");
    command.env_exactly(std::iter::empty::<(&str, &str)>());
    assert!(command.image().unwrap().envp.is_empty());
}
#[test]
fn configurable_conversion_preserves_exact_environment_and_overlays() {
    let mut command = transparent("/usr/bin/env");
    command.env("BEFORE", "discard");
    command.env_exactly([("ONLY", "first")]).env("ONLY", "last");
    let output = command.into_command().output().unwrap();
    assert!(output.status.success());
    assert!(output.stdout == b"ONLY=last\n");
}
