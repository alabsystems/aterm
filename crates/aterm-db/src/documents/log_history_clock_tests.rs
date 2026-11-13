// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::Hlc;

#[test]
fn every_representable_child_strictly_advances_its_parent() {
    for ms in [0, 1, u64::MAX - 1, u64::MAX] {
        for counter in [0, 1, u64::MAX - 1, u64::MAX] {
            let parent = Hlc { ms, counter };
            for claim in [0, 1, u64::MAX - 1, u64::MAX] {
                match Hlc::after(claim, Some(parent)) {
                    Ok(child) => {
                        assert!(child > parent);
                        assert!(child.ms >= claim);
                        assert_eq!(child.ms, ms.max(claim));
                    }
                    Err(_) => {
                        assert_eq!(counter, u64::MAX);
                        assert!(claim <= ms);
                    }
                }
            }
        }
    }
    assert_eq!(Hlc::after(0, None).unwrap(), Hlc { ms: 0, counter: 0 });
    assert_eq!(
        Hlc::after(
            u64::MAX,
            Some(Hlc {
                ms: 0,
                counter: u64::MAX
            })
        )
        .unwrap(),
        Hlc {
            ms: u64::MAX,
            counter: 0
        }
    );
}
