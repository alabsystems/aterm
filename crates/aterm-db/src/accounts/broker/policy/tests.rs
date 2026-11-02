// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

fn grant() -> Grant {
    Grant {
        id: [1; 16],
        producer: [2; 32],
        actor: [3; 48],
        lifetime: Duration::from_secs(60),
        binding: RequestBinding {
            principal: [4; 32],
            account: [5; 32],
            revision: [6; 32],
            credential: [6; 32],
            policy: [7; 32],
            adapter: "example-v1".into(),
        },
    }
}

#[test]
fn broker_policy_round_trips_and_rejects_ambiguous_or_extra_fields() {
    let g = grant();
    let text = Policy::new(vec![g.clone()]).unwrap().text();
    assert_eq!(
        Policy::parse(text.as_bytes()).unwrap().grant(g.id).unwrap(),
        &g
    );
    assert!(Policy::new(vec![g.clone(), g]).is_err());
    let a = grant();
    let mut b = a.clone();
    b.id = [9; 16];
    assert!(Policy::new(vec![a, b]).is_err());
    assert!(Policy::parse((text.clone() + "secret = 'no'\n").as_bytes()).is_err());
    assert!(Policy::parse(text.replace("producer =", "client_selected =").as_bytes()).is_err());
    assert!(Policy::parse(text.replace("version = 1", "version = 2").as_bytes()).is_err());
    assert!(Policy::parse(&vec![b' '; MAX_BYTES + 1]).is_err());
}

#[test]
fn broker_policy_bounds_ids_lifetimes_and_count() {
    for index in 0..6 {
        let mut g = grant();
        match index {
            0 => g.id = [0; 16],
            1 => g.producer = [0; 32],
            2 => g.actor = [0; 48],
            3 => g.lifetime = Duration::ZERO,
            4 => g.lifetime = MAX_LEASE + Duration::from_millis(1),
            _ => g.lifetime = Duration::from_nanos(1),
        }
        assert!(Policy::new(vec![g]).is_err());
    }
    assert!(Policy::new(vec![grant(); MAX_SESSIONS + 1]).is_err());
}
