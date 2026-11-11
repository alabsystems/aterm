// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
#[test]
fn cooperative_ceiling_never_serializes_as_a_fixed_cpu_grant() {
    let manifest = Manifest {
        device: 1,
        inode: 2,
    };
    let mut observed = Inspection {
        session: "s-a".into(),
        attempt: [3; 16],
        host: "darwin:test".into(),
        boot: "ab".repeat(16),
        root_pid: 7,
        root_birth: 8,
        root_birth_kind: BirthKind::MacosWallMicroseconds,
        subject: Subject::Root { pid: 7, birth: 8 },
        cpu: Cpu::RetainedFixed {
            units: 4,
            native_manifest: manifest,
        },
        memory: Memory {
            bytes: 1024,
            namespace: "cd".repeat(16),
            marker: "123-7-0".into(),
            epoch: "ef".repeat(16),
            guard_device: 1,
            guard_inode: 9,
        },
    };
    let fixed = observed.json().unwrap();
    assert_eq!(fixed["cpu"]["mode"].as_str(), Some("retained_fixed"));
    assert_eq!(fixed["cpu"]["units"].as_u64(), Some(4));
    assert!(fixed["cpu"].get("ceiling").is_none());
    observed.cpu = Cpu::CooperativeShared {
        ceiling: 4,
        native_manifest: manifest,
    };
    let cooperative = observed.json().unwrap();
    assert_eq!(
        cooperative["cpu"]["mode"].as_str(),
        Some("cooperative_shared")
    );
    assert_eq!(cooperative["cpu"]["ceiling"].as_u64(), Some(4));
    assert!(cooperative["cpu"].get("units").is_none());
    observed.subject = Subject::RegisteredMember {
        pid: 17,
        birth: 18,
        attempt: [4; 16],
    };
    let member = observed.json().unwrap();
    assert_eq!(member["root_pid"].as_u64(), Some(7));
    assert_eq!(
        member["subject"]["mode"].as_str(),
        Some("registered_member")
    );
    assert_eq!(member["subject"]["pid"].as_u64(), Some(17));
    observed.subject = Subject::Root { pid: 17, birth: 18 };
    assert!(observed.json().is_err());
    observed.subject = Subject::Root { pid: 7, birth: 8 };
    observed.memory.epoch.clear();
    assert!(observed.json().is_err());
}
