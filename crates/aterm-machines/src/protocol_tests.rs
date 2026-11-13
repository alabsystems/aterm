// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

/// A hello an older agent wrote parses whole: its vitals' dropped keys (a CPU busy share,
/// GPU temperature and power, `df`'s used and available) are ignored, the keys it never
/// wrote (`mem_available_bytes`, `disk_free_bytes`) are absent, and its sessions still read.
#[test]
fn an_old_hello_with_dropped_vitals_keys_still_parses() {
    let old = r#"{"schema":1,"agent_version":"0.146.0","families":["power"],"uuid":"U","serial_number":"S","computer_name":"m32","local_host_name":"m32","os_version":"27.0","os_build":"25A1","arch":"arm64","floors":[],"cached":[],"sessions":[{"instance_pid":71,"local_id":2,"sid":"s-1","state":"alive","title":"t"}],"vitals":{"cpu_cores":18,"cpu_busy_pct":12,"gpu_temp_c":45,"gpu_power_deciwatts":34,"mem_total_bytes":17179869184,"disk_total_bytes":1024000,"disk_used_bytes":614400,"disk_avail_bytes":204800}}"#;
    let hello: Hello = aterm_json::from_str(old).expect("an old hello parses");
    assert_eq!(hello.sessions.rows.len(), 1);
    let vitals = hello.vitals.clone().expect("its vitals");
    assert_eq!(vitals.cpu_cores, Some(18));
    assert_eq!(vitals.mem_total_bytes, Some(16 << 30));
    assert_eq!(vitals.disk_total_bytes, Some(1_024_000));
    assert_eq!(vitals.mem_available_bytes, None);
    assert_eq!(
        vitals.disk_free_bytes, None,
        "no shim reads disk_avail_bytes"
    );
    let again: Hello = aterm_json::from_str(&aterm_json::to_string(&hello).unwrap()).unwrap();
    assert_eq!(again, hello);
}
