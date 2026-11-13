// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

#[test]
fn the_netbios_name_is_read_from_the_os() {
    let name = super::netbios_host_name_over(None).expect("the OS names this machine");
    assert!(name.chars().count() <= 15, "{name}");
    assert_eq!(
        super::netbios_host_name_over(Some("elsewhere.example".into())).as_deref(),
        Some(name.as_str())
    );
    if let Ok(env) = std::env::var("COMPUTERNAME") {
        assert!(env.trim().eq_ignore_ascii_case(&name), "{env} / {name}");
    }
}
