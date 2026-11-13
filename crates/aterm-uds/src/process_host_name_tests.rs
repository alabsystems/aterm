// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

#[test]
fn the_hostname_override_wins_and_a_blank_one_falls_to_the_os() {
    assert_eq!(
        super::host_name_over(Some(" my-override.local ".into())).as_deref(),
        Some("my-override.local")
    );
    assert_eq!(
        super::host_name_over(Some("  ".into())),
        super::os_host_name()
    );
    assert_eq!(super::host_name_over(None), super::os_host_name());
}

#[test]
fn the_lan_name_is_a_dotted_host_name_else_bonjour_else_the_kernel() {
    let some = |name: &'static str| move || Some(name.to_owned());
    let none = || None::<String>;
    let unasked = || -> Option<String> { panic!("asked after an earlier reader answered") };
    assert_eq!(
        super::lan_name(some("m8.lab.example"), unasked, unasked).as_deref(),
        Some("m8.lab.example")
    );
    assert_eq!(
        super::lan_name(some("m8"), some("m8"), unasked).as_deref(),
        Some("m8.local"),
        "a bare HostName yields to the name the LAN resolves"
    );
    assert_eq!(
        super::lan_name(none, some("m8"), unasked).as_deref(),
        Some("m8.local")
    );
    assert_eq!(
        super::lan_name(some("m8"), none, some("m8")).as_deref(),
        Some("m8")
    );
    assert_eq!(super::lan_name(none, none, none), None);
}

#[cfg(target_os = "macos")]
#[test]
fn the_lan_name_reads_what_scutil_reads() {
    let scutil = |key: &str| {
        let out = crate::spawnfd::command("/usr/sbin/scutil")
            .args(["--get", key])
            .output()
            .expect("run scutil");
        let name = String::from_utf8_lossy(&out.stdout).trim().to_owned();
        (out.status.success() && !name.is_empty()).then_some(name)
    };
    let configured = scutil("HostName");
    assert_eq!(super::configured_host_name(), configured);
    // SAFETY: a CFString constant SystemConfiguration exports.
    let computer = super::setup_system(unsafe { super::sc::kSCPropSystemComputerName });
    assert!(computer.is_some(), "the read gives the ComputerName");
    assert_eq!(computer, scutil("ComputerName"));
    let want = configured
        .filter(|name| name.contains('.'))
        .or_else(|| scutil("LocalHostName").map(|local| format!("{local}.local")))
        .or_else(super::os_host_name);
    assert_eq!(super::lan_host_name(), want);
}

#[test]
fn the_host_name_is_the_kernels_node_name() {
    let ours = super::os_host_name().expect("gethostname names this machine");
    let uname = crate::spawnfd::command("/usr/bin/uname")
        .arg("-n")
        .output()
        .or_else(|_| crate::spawnfd::command("/bin/uname").arg("-n").output())
        .expect("run uname -n");
    assert_eq!(ours, String::from_utf8_lossy(&uname.stdout).trim());
}
