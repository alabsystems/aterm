//! Declaration/codec controls create no actual qualification or publication.
use super::*;

fn fixture() -> Plan {
    let mut plan = super::super::tests::plan();
    plan.nodes.truncate(6);
    plan.inputs.cwd = b"/repo".to_vec();
    for (index, source) in plan.sources.iter_mut().enumerate() {
        source.root = format!("/repo/package{index}").into_bytes();
        source.manifest.path = format!("/repo/package{index}/Cargo.toml").into_bytes();
    }
    let checks = [0, 3, 5]
        .into_iter()
        .map(|index| RequiredCheck {
            id: plan.nodes[index].name.clone(),
            node: NodeId(index as u32),
        })
        .collect::<Vec<_>>();
    plan.nodes.push(Node {
        name: "publish-source".into(),
        dependencies: checks
            .iter()
            .map(|check| Dependency {
                node: check.node,
                condition: EdgeCondition::Succeeded,
            })
            .collect(),
        timeout_ns: 1_000_000,
        kind: Kind::PublishSource {
            policy: SourcePolicy {
                target: Target::WholeRepository {
                    root: b"/repo".to_vec(),
                },
                candidate_commit: "b".repeat(40),
                expected_main_commit: "a".repeat(40),
                required_checks: checks,
            },
        },
    });
    plan.validate().unwrap();
    plan
}
fn policy(plan: &mut Plan) -> &mut SourcePolicy {
    match &mut plan.nodes.last_mut().unwrap().kind {
        Kind::PublishSource { policy } => policy,
        _ => panic!("fixture publication node"),
    }
}
fn encode_policy(value: &SourcePolicy) -> Vec<u8> {
    let mut encoder = Encoder::new(106);
    value.write(&mut encoder).unwrap();
    encoder.finish().unwrap()
}
fn decode_policy(bytes: &[u8]) -> io::Result<SourcePolicy> {
    let mut decoder = header(bytes, 106)?;
    let result = SourcePolicy::read(&mut decoder)?;
    decoder.done()?;
    Ok(result)
}

#[test]
fn ordinary_source_publication_round_trips_exact_sha1_and_sha256_without_comparison() {
    assert_eq!(MAIN_REF, "refs/heads/main");
    for width in [40, 64] {
        let mut plan = fixture();
        let value = policy(&mut plan);
        value.candidate_commit = "b".repeat(width);
        value.expected_main_commit = "a".repeat(width);
        let expected = value.clone();
        let bytes = encode_policy(&expected);
        assert_eq!(decode_policy(&bytes).unwrap(), expected);
        for end in 0..bytes.len() {
            assert!(decode_policy(&bytes[..end]).is_err(), "policy prefix {end}");
        }
        let mut trailing = bytes;
        trailing.push(0);
        assert!(decode_policy(&trailing).is_err());
        let bytes = plan.encode().unwrap();
        assert_eq!(Plan::decode(&bytes).unwrap(), plan);
        assert!(
            !plan
                .nodes
                .iter()
                .any(|node| matches!(node.kind, Kind::Compare { .. } | Kind::Land { .. }))
        );
    }
}

#[test]
fn malformed_or_equal_commit_ids_refuse_before_publication() {
    let malformed = [
        "".into(),
        "a".repeat(39),
        "a".repeat(41),
        "a".repeat(63),
        "a".repeat(65),
        "A".repeat(40),
        "g".repeat(40),
        "0".repeat(40),
        "0".repeat(64),
        format!("{}\0", "a".repeat(39)),
        "é".repeat(20),
        "refs/heads/main".into(),
    ];
    for candidate in [true, false] {
        for value in &malformed {
            let mut plan = fixture();
            let selected = policy(&mut plan);
            if candidate {
                selected.candidate_commit = value.clone();
            } else {
                selected.expected_main_commit = value.clone();
            }
            assert!(
                plan.validate().is_err(),
                "candidate={candidate} id={value:?}"
            );
            assert!(plan.encode().is_err());
        }
    }
    let mut plan = fixture();
    policy(&mut plan).candidate_commit = "a".repeat(40);
    assert!(
        plan.validate().is_err(),
        "already-current is not a source publication"
    );
    let mut plan = fixture();
    policy(&mut plan).candidate_commit = "b".repeat(64);
    assert!(
        plan.validate().is_err(),
        "mixed Git object formats are not one repository"
    );
}

#[test]
fn whole_repository_root_is_explicit_canonical_and_contains_selected_sources() {
    for root in [
        b"".as_slice(),
        b"repo",
        b"/",
        b"/repo/",
        b"/repo//child",
        b"/repo/./child",
        b"/repo/../child",
        b"/repo\0",
        b"/other",
        b"/repo/package0",
        b"/repo2",
    ] {
        let mut plan = fixture();
        policy(&mut plan).target = Target::WholeRepository {
            root: root.to_vec(),
        };
        assert!(plan.validate().is_err(), "root={root:?}");
    }
    let mut plan = fixture();
    // A repository may contain its bootstrap SDK and build outputs. The
    // selected package and output populations keep existing non-overlap rules.
    plan.inputs.sdk_root = b"/repo/sdk".to_vec();
    plan.inputs.program.path = b"/repo/sdk/bin/targo".to_vec();
    plan.inputs.compiler.path = b"/repo/sdk/bin/trustc".to_vec();
    plan.inputs.output_root = b"/repo/build".to_vec();
    for (key, value) in &mut plan.inputs.environment {
        if key == b"CARGO" {
            *value = plan.inputs.program.path.clone();
        }
        if key == b"RUSTC" {
            *value = plan.inputs.compiler.path.clone();
        }
    }
    for output in &mut plan.outputs {
        output.root = format!("/repo/build/{}", output.name).into_bytes();
    }
    plan.validate().unwrap();
    assert_eq!(Plan::decode(&plan.encode().unwrap()).unwrap(), plan);
    plan.outputs[0].root = plan.sources[0].root.clone();
    assert!(
        plan.validate().is_err(),
        "actual package/output overlap remains invalid"
    );
}

#[test]
fn required_check_population_rejects_empty_duplicate_and_malformed_identifiers() {
    let mut plan = fixture();
    policy(&mut plan).required_checks.clear();
    plan.nodes.last_mut().unwrap().dependencies.clear();
    assert!(plan.validate().is_err());
    for duplicate_id in [true, false] {
        let mut plan = fixture();
        let selected = policy(&mut plan);
        if duplicate_id {
            selected.required_checks[1].id = selected.required_checks[0].id.clone();
        } else {
            selected.required_checks[1].node = selected.required_checks[0].node;
        }
        assert!(plan.validate().is_err());
    }
    for id in [
        "",
        "_check",
        ".check",
        "-check",
        "check/path",
        "check name",
        "check\nname",
        "check\0name",
        "café",
        "check:scope",
    ] {
        let mut plan = fixture();
        policy(&mut plan).required_checks[0].id = id.into();
        assert!(plan.validate().is_err(), "id={id:?}");
    }
    let mut plan = fixture();
    policy(&mut plan).required_checks[0].id = "a".repeat(257);
    assert!(plan.validate().is_err());
    let mut plan = fixture();
    policy(&mut plan).required_checks[0].id = "Check.v1_source-0".into();
    plan.validate().unwrap();
}

#[test]
fn required_checks_are_exact_successful_edges_to_check_roles() {
    for node in [NodeId(6), NodeId(7), NodeId(u32::MAX)] {
        let mut plan = fixture();
        policy(&mut plan).required_checks[0].node = node;
        assert!(plan.validate().is_err(), "node={node:?}");
    }
    let mut plan = fixture();
    plan.nodes.last_mut().unwrap().dependencies[0].condition = EdgeCondition::Retired;
    assert!(
        plan.validate().is_err(),
        "retirement is not successful qualification"
    );
    let mut plan = fixture();
    plan.nodes.last_mut().unwrap().dependencies.remove(0);
    assert!(plan.validate().is_err(), "omitted edge");
    let mut plan = fixture();
    plan.nodes
        .last_mut()
        .unwrap()
        .dependencies
        .push(Dependency {
            node: NodeId(1),
            condition: EdgeCondition::Succeeded,
        });
    assert!(plan.validate().is_err(), "undeclared extra prerequisite");
    for wrong in super::super::tests::plan()
        .nodes
        .into_iter()
        .filter_map(|node| {
            matches!(
                node.kind,
                Kind::Run { .. } | Kind::Compare { .. } | Kind::Land { .. }
            )
            .then_some(node.kind)
        })
    {
        let mut plan = fixture();
        let selected = policy(&mut plan).clone();
        plan.nodes[0].kind = wrong;
        assert!(
            selected.validate(&plan, NodeId(6)).is_err(),
            "wrong check role"
        );
    }
    let mut plan = fixture();
    let selected = policy(&mut plan).clone();
    plan.nodes[0].kind = Kind::PublishSource {
        policy: selected.clone(),
    };
    assert!(
        selected.validate(&plan, NodeId(6)).is_err(),
        "publication cannot qualify publication"
    );
    assert!(
        selected.validate(&fixture(), NodeId(0)).is_err(),
        "wrong publication role"
    );
    assert!(selected.validate(&fixture(), NodeId(u32::MAX)).is_err());
}

#[test]
fn unknown_policy_versions_target_tags_and_node_tags_refuse() {
    for (version, target) in [(0, 0), (2, 0), (1, 1), (1, u64::MAX)] {
        let mut encoder = Encoder::new(106);
        encoder.number(version);
        encoder.number(target);
        assert!(decode_policy(&encoder.finish().unwrap()).is_err());
    }
    for tag in [7, u64::MAX] {
        let mut encoder = Encoder::new(106);
        encoder.number(tag);
        let bytes = encoder.finish().unwrap();
        let mut decoder = header(&bytes, 106).unwrap();
        assert!(Kind::read(&mut decoder).is_err(), "unknown node tag {tag}");
    }
}
