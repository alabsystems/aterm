// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
// Author: Andrew Yates

use super::IntentId;

/// Ids order by step: from step 10 on, text order would put `10-…` before `9-…`.
#[test]
fn intent_ids_order_by_step() {
    let id = |text: &str| IntentId::parse(text).expect("an id");
    let mut ids = [
        id("10-0123456789ab"),
        id("9-0123456789ab"),
        id("0-ffffffffffff"),
    ];
    ids.sort();
    let steps: Vec<u64> = ids.iter().map(IntentId::step).collect();
    assert_eq!(steps, [0, 9, 10]);
    assert!(id("10-0123456789ab") > id("9-ffffffffffff"));
    assert!(id("9-0123456789aa") < id("9-0123456789ab"));
}
