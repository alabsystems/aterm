//! Codec/selection DATA controls; no decoded plan creates a native issuer.
use super::super::{Kind, Node};
use super::*;
use crate::wire::header;

fn fixture() -> Plan {
    let mut plan = super::super::tests::plan();
    plan.nodes = vec![Node {
        name: "seed-inputs".into(),
        dependencies: vec![],
        timeout_ns: 1_000_000,
        kind: Kind::SeedInputs {
            selection: Selection {
                source: vec![Provider {
                    source: 0,
                    odb: 0,
                    relative: vec![],
                    commit: "a".repeat(40),
                }],
                orchestration: vec![Provider {
                    source: 1,
                    odb: 1,
                    relative: vec![],
                    commit: "b".repeat(40),
                }],
            },
        },
    }];
    plan.validate().unwrap();
    plan
}
fn selection(plan: &mut Plan) -> &mut Selection {
    let Kind::SeedInputs { selection } = &mut plan.nodes[0].kind else {
        panic!("seed fixture")
    };
    selection
}

#[test]
fn seed_node_uses_its_distinct_tag_seven_and_round_trips_both_git_algorithms() {
    for width in [40, 64] {
        let mut plan = fixture();
        let selected = selection(&mut plan);
        selected.source[0].commit = "a".repeat(width);
        selected.orchestration[0].commit = "b".repeat(width);
        let mut encoder = Encoder::new(106);
        plan.nodes[0].kind.write(&mut encoder).unwrap();
        let bytes = encoder.finish().unwrap();
        let mut decoder = header(&bytes, 106).unwrap();
        assert_eq!(decoder.number().unwrap(), 7);
        let bytes = plan.encode().unwrap();
        assert_eq!(Plan::decode(&bytes).unwrap(), plan);
        for length in 0..bytes.len() {
            assert!(Plan::decode(&bytes[..length]).is_err());
        }
        let mut extra = bytes;
        extra.push(0);
        assert!(Plan::decode(&extra).is_err());
    }
}
#[test]
fn complete_families_and_exact_provider_paths_are_required() {
    let original = fixture();
    for change in 0..8 {
        let mut plan = original.clone();
        match change {
            0 => selection(&mut plan).source.clear(),
            1 => selection(&mut plan).orchestration.clear(),
            2 => selection(&mut plan).source[0].source = u32::MAX,
            3 => selection(&mut plan).source[0].odb = u32::MAX,
            4 => selection(&mut plan).source[0].relative = b"outside".to_vec(),
            5 => selection(&mut plan).source[0].commit = "A".repeat(40),
            6 => selection(&mut plan).source[0].commit = "a".repeat(41),
            7 => {
                let repeated = selection(&mut plan).source[0].clone();
                selection(&mut plan).source.push(repeated);
            }
            _ => unreachable!(),
        }
        assert!(plan.validate().is_err(), "corrupt seed selector {change}");
    }
}
#[test]
fn separate_source_and_orchestration_provider_populations_bind_the_whole_plan() {
    let plan = fixture();
    let original = plan.binding_preimage().unwrap();
    let mut changed = plan.clone();
    selection(&mut changed).orchestration[0].commit = "c".repeat(40);
    assert_ne!(changed.binding_preimage().unwrap(), original);
    let mut changed = plan;
    selection(&mut changed).source[0].odb = 1;
    assert_ne!(changed.binding_preimage().unwrap(), original);
}
#[test]
fn provider_paths_cannot_escape_or_erase_an_unbound_component() {
    for relative in [
        b"/escape".as_slice(),
        b"../escape",
        b"missing/../source",
        b"a//b",
        b"a\0b",
    ] {
        let mut plan = fixture();
        let mut child = selection(&mut plan).source[0].clone();
        child.relative = relative.to_vec();
        selection(&mut plan).source.push(child);
        assert!(plan.validate().is_err());
    }
}
