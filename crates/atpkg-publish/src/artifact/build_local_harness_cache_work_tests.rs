// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::*;
const ID: &str = "20261105T101454208Z-b55136d6eb9578a4";
fn marker(id: &str) -> String {
    format!(
        "{{\"reason\":\"build-analysis-session\",\"schema\":1,\"scope\":\"build-script-inputs-v1\",\"run_id\":\"{id}\"}}\n"
    )
}
#[test]
fn session_selection_is_exact_and_never_a_latest_or_child_fallback() {
    let valid = marker(ID);
    assert_eq!(session(valid.as_bytes()).unwrap(), ID);
    assert_eq!(
        session(b"{\"reason\":\"build-started\",\"run_id\":\"historical\"}\n"),
        Err(UnavailableReason::MissingSession)
    );
    assert_eq!(
        session(format!("{valid}{valid}").as_bytes()),
        Err(UnavailableReason::AmbiguousSession)
    );
    assert_eq!(
        session(marker("--config").as_bytes()),
        Err(UnavailableReason::InvalidReport)
    );
    assert_eq!(
        session(
            valid
                .replace("build-script-inputs-v1", "foreign-inputs")
                .as_bytes()
        ),
        Err(UnavailableReason::InvalidReport)
    );
}

#[test]
fn duplicate_and_malformed_marker_objects_never_select_a_session() {
    let valid = marker(ID);
    for damaged in [
        valid.replace("\"schema\":1", "\"schema\":0,\"schema\":1"),
        valid.replace("\"run_id\":", "\"run_id\":\"foreign\",\"run_id\":"),
        valid.replace("\"reason\":", "\"reason\":\"foreign\",\"reason\":"),
        valid.replace(
            "\"reason\":\"build-analysis-session\"",
            "\"reason\":\"build-analysis-session\",\"reason\":\"foreign\"",
        ),
        valid.replace("\"schema\":1", "\"extra\":true,\"schema\":1"),
        format!("[{}]", valid.trim()),
        valid.trim().trim_end_matches('}').to_owned(),
    ] {
        assert_eq!(
            session(damaged.as_bytes()),
            Err(UnavailableReason::InvalidReport),
            "accepted malformed marker: {damaged}"
        );
    }
    assert_eq!(
        session(b"{\"reason\":\"compiler-artifact\",\"arbitrary\":true}\n"),
        Err(UnavailableReason::MissingSession)
    );
}

/// This is a native transport/parser fixture, not a compiler-work measurement.
#[cfg(unix)]
#[test]
fn large_valid_report_survives_actual_complete_collection_and_persistence() {
    use crate::artifact::tools::HostTools;
    use aterm_json::{Value, json};
    let world = aterm_tempfile::tempdir().unwrap();
    let logs = world.path().join("logs");
    aterm_uds::private_file::create_private_dir(&logs).unwrap();
    let count = 4096;
    let work = json!({"entries":1,"input_bytes":19,"bytes_hashed":17,"metadata_reads":11,
        "resolution_steps":2,"link_reads":0,"observations":3,"revalidations":4});
    let units: Vec<_> = (0..count).map(|index| {
        let measured = index == count-1;
        json!({"index":index,
            "package_id":format!("path+file:///bounded-native-report-fixture/{index}/{}#sample@0.1.0", "p".repeat(80)),
            "target":{"name":format!("target-{index}-{}", "t".repeat(80)),
                "kind":if measured {"build-script"} else {"lib"}},
            "mode":if measured {"run-custom-build"} else {"build"},
            "platform":"native-fixture","features":[],"requested":false,
            "attempts":if measured {1} else {0},"captured_attempts":if measured {1} else {0},
            "uncacheable_attempts":0,
            "phases":if measured {vec![json!({"phase":"freshness","attempts":1,
                "captured_attempts":1,"uncacheable_attempts":0,"work":work.clone()})]} else {vec![]},
            "work":if measured {work.clone()} else {Value::Null}})
    }).collect();
    let fixture = json!({"schema":cache_work::SCHEMA,"run_id":ID,"scope":cache_work::SCOPE,
        "availability":"complete","unavailable_reason":null,"build_outcome":"not-recorded",
        "denominators":{"registered_units":count,"run_script_units":1,"observed_units":1,
            "units_without_attempts":count-1,"attempts_started":1,"attempts_finished":1,
            "captured_attempts":1,"uncacheable_attempts":0},"units":units,"totals":work});
    let bytes = aterm_json::to_vec(&fixture).unwrap();
    assert!(bytes.len() > aterm_uds::deadline::OUTPUT_KEEP);
    assert!(bytes.len() <= cache_work::MAX_REPORT_BYTES);
    let expected = cache_work::parse(&bytes, ID).expect("the large fixture itself is valid");
    let path = world.path().join("report.json");
    std::fs::write(&path, &bytes).unwrap();
    let cat = aterm_uds::deadline::stand_in(std::path::Path::new("/bin/cat"));
    let query = Run::new(cat, [path.as_os_str()]).bounded();
    assert!(
        HostTools
            .run_complete(&query, aterm_uds::deadline::OUTPUT_KEEP)
            .is_err(),
        "the former complete-output bound refuses this otherwise valid report"
    );
    let report = match collect_report(&HostTools, &query, ID, &logs) {
        Collection::Report(report) => report,
        Collection::Unavailable(reason) => {
            panic!("the actual complete collection must retain the large report: {reason:?}");
        }
    };
    assert!(
        report == expected,
        "complete capture preserved every original unit and cost"
    );
    assert_eq!(report.run_id(), ID);
    assert_eq!(report.units().len(), count);
    assert_eq!(report.denominators().observed_units, 1);
    assert_eq!(
        report
            .units()
            .last()
            .unwrap()
            .work()
            .unwrap()
            .metadata_reads,
        11
    );
    assert_eq!(report.totals().unwrap().bytes_hashed, 17);
    assert_eq!(std::fs::read(logs.join("cache-work.json")).unwrap(), bytes);
}
