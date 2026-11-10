// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

#![cfg(target_os = "macos")]

#[path = "support/font_bridge.rs"]
mod probe;

#[test]
fn registered_appkit_bridges_coretext_fonts_in_a_fresh_process() {
    const CHILD: &str = "ATERM_TEST_FONT_BRIDGE_CHILD";
    if std::env::var_os(CHILD).is_some() {
        let observed = probe::inspect(true);
        assert!(observed.is_nsfont, "{observed:?}");
        assert!(observed.system_class.is_ok(), "{observed:?}");
        println!("font bridge ready: {}", observed.created_class);
        return;
    }
    let mut command = aterm_uds::spawnfd::command(std::env::current_exe().unwrap());
    command
        .args([
            "--exact",
            "registered_appkit_bridges_coretext_fonts_in_a_fresh_process",
            "--nocapture",
        ])
        .env(CHILD, "1");
    let output =
        aterm_uds::deadline::output_within(&mut command, std::time::Duration::from_secs(60))
            .expect("the isolated font query must complete before the hang detector");
    assert!(
        output.status.success(),
        "isolated font query failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        String::from_utf8_lossy(&output.stdout).contains("font bridge ready: "),
        "the child must prove it reached the native font query"
    );
}
