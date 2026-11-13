// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
#[test]
fn owned_environment_rejects_duplicate_names_without_disclosing_values() {
    assert_eq!(local_environment(&[]).unwrap(), Vec::<String>::new());
    let raw = vec![(OsString::from("PROBE"), OsString::from("first"))];
    assert_eq!(local_environment(&raw).unwrap(), ["PROBE=first"]);
    let mut duplicate = raw;
    duplicate.push((
        OsString::from("PROBE"),
        OsString::from("different-private-value"),
    ));
    let refusal = local_environment(&duplicate).unwrap_err().to_string();
    assert!(!refusal.contains("different-private-value"));
    assert!(local_environment(&[(OsString::from("bad=name"), OsString::new())]).is_err());
}
