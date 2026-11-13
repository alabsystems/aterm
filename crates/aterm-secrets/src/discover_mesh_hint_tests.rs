// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

#[test]
fn mesh_txt_is_only_a_well_formed_dial_hint() {
    let mut advert = Advert {
        instance: "opaque".into(),
        host: "peer.local".into(),
        port: 7100,
        txt: vec![
            ("v".into(), "2".into()),
            ("id".into(), "n-0123456789abcdef".into()),
            ("port".into(), "7100".into()),
        ],
    };
    assert_eq!(advert.mesh_node(), Some("n-0123456789abcdef"));
    advert.txt.push(("id".into(), "n-fedcba9876543210".into()));
    assert_eq!(advert.mesh_node(), None);
    advert.txt.pop();
    advert.port = 7101;
    assert_eq!(advert.mesh_node(), None);
    advert.port = 7100;
    advert.txt[0].1 = "1".into();
    assert_eq!(advert.mesh_node(), None);
    advert.txt[0].1 = "2".into();
    advert.txt[1].1 = "arbitrary-host".into();
    assert_eq!(advert.mesh_node(), None);
}
