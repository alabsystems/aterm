// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Ancillary exact-session collection after original retained output is complete.

use super::{HarnessBuild, Plan, Run, Tools, write_new};
use aterm_capacity::cache_work::{self, Collection, UnavailableReason};
use std::path::PathBuf;

/// Original checked build context retained for exact later warm-output diagnostics.
pub struct HarnessCacheWork {
    plan: Plan,
    recipe: aterm_capacity::epoch_recipe::RootRecipe,
    manifest: PathBuf,
    target: PathBuf,
    _lease: std::sync::Arc<crate::artifact::toolchain::Held>,
}

impl HarnessBuild {
    /// Retain the original immutable driver and recipe for later warm output.
    /// # Errors
    /// The actual prepared plan cannot become a supported typed recipe.
    pub fn cache_work_collector(&self, root: u32) -> crate::error::Result<HarnessCacheWork> {
        Ok(HarnessCacheWork {
            plan: self.plan.clone(),
            recipe: self.recipe(root)?,
            manifest: self.manifest(),
            target: self.target.clone(),
            _lease: std::sync::Arc::clone(&self._lease),
        })
    }
}

impl HarnessCacheWork {
    /// Collect only from this original recipe's fully settled output bytes.
    /// Every failure is ancillary unavailable data, never artifact acceptance.
    #[must_use]
    pub fn collect(
        &self,
        tools: &dyn Tools,
        output: &aterm_capacity::epoch_recipe::RootOutputBytes,
        logs: &std::path::Path,
    ) -> Collection {
        if output.recipe != self.recipe
            || output.manifest != self.manifest
            || output.target != self.target
        {
            return Collection::Unavailable(UnavailableReason::InvalidReport);
        }
        collect(
            &self.plan,
            self.recipe
                .analysis
                .as_ref()
                .map(|analysis| &analysis.cargo_home),
            &self.manifest,
            logs,
            tools,
            &output.stdout,
        )
    }
}

pub(super) fn original_cargo_home(plan: &Plan) -> Option<PathBuf> {
    if !plan.analysis_session {
        return None;
    }
    let value = plan
        .command
        .env_of("CARGO_HOME")
        .cloned()
        .or_else(|| std::env::var_os("CARGO_HOME"))
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".cargo")))?;
    let value = if value.is_absolute() {
        value
    } else {
        plan.command.cwd.as_ref()?.join(value)
    };
    if value
        .components()
        .any(|part| matches!(part, std::path::Component::ParentDir))
    {
        return None;
    }
    Some(value.canonicalize().unwrap_or(value))
}

fn run_id(value: &str) -> bool {
    let bytes = value.as_bytes();
    bytes.len() == 36
        && bytes[8] == b'T'
        && bytes[18] == b'Z'
        && bytes[19] == b'-'
        && bytes[..8]
            .iter()
            .chain(&bytes[9..18])
            .all(u8::is_ascii_digit)
        && bytes[20..]
            .iter()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(b))
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct SessionMarker {
    reason: String,
    schema: u32,
    scope: String,
    run_id: String,
}

// Examine every reason key before typed marker decoding. An untyped map would
// erase duplicate keys, including a marker followed by a different reason.
struct MarkerCandidate(bool);
impl<'de> serde::Deserialize<'de> for MarkerCandidate {
    fn deserialize<D: serde::Deserializer<'de>>(
        deserializer: D,
    ) -> std::result::Result<Self, D::Error> {
        struct Object;
        impl<'de> serde::de::Visitor<'de> for Object {
            type Value = MarkerCandidate;
            fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str("a compiler JSON object")
            }
            fn visit_map<M: serde::de::MapAccess<'de>>(
                self,
                mut map: M,
            ) -> std::result::Result<Self::Value, M::Error> {
                let mut marker = false;
                while let Some(key) = map.next_key::<String>()? {
                    if key == "reason" {
                        let reason = map.next_value::<aterm_json::Value>()?;
                        marker |= reason.as_str() == Some("build-analysis-session");
                    } else {
                        map.next_value::<serde::de::IgnoredAny>()?;
                    }
                }
                Ok(MarkerCandidate(marker))
            }
        }
        deserializer.deserialize_map(Object)
    }
}

fn session(stdout: &[u8]) -> std::result::Result<String, UnavailableReason> {
    let text = std::str::from_utf8(stdout).map_err(|_| UnavailableReason::InvalidReport)?;
    let mut session = None;
    for line in text.lines() {
        let candidate = match aterm_json::from_str::<MarkerCandidate>(line) {
            Ok(candidate) => candidate,
            Err(_) if line.trim_start().starts_with(['{', '[']) => {
                return Err(UnavailableReason::InvalidReport);
            }
            Err(_) => continue,
        };
        if !candidate.0 {
            continue;
        }
        if session.is_some() {
            return Err(UnavailableReason::AmbiguousSession);
        }
        let marker = aterm_json::from_str::<SessionMarker>(line)
            .map_err(|_| UnavailableReason::InvalidReport)?;
        if marker.reason != "build-analysis-session"
            || marker.schema != 1
            || marker.scope != cache_work::SCOPE
            || !run_id(&marker.run_id)
        {
            return Err(UnavailableReason::InvalidReport);
        }
        session = Some(marker.run_id);
    }
    session.ok_or(UnavailableReason::MissingSession)
}

fn collect(
    plan: &Plan,
    home: Option<&PathBuf>,
    manifest: &std::path::Path,
    logs: &std::path::Path,
    tools: &dyn Tools,
    stdout: &[u8],
) -> Collection {
    let unavailable = Collection::Unavailable;
    // An intermediate/old driver cannot acquire this capability from child JSON.
    if !plan.analysis_session {
        return unavailable(UnavailableReason::UnsupportedDriver);
    }
    let Some(home) = home else {
        return unavailable(UnavailableReason::NotRecorded);
    };
    let id = match session(stdout) {
        Ok(id) => id,
        Err(reason) => return unavailable(reason),
    };
    let mut query = Run::new(
        &plan.command.program,
        [
            "-Zbuild-analysis",
            "report",
            "cache-work",
            "--id",
            &id,
            "--json",
            "--manifest-path",
        ],
    )
    .bounded();
    query.args.push(manifest.into());
    query.cwd = plan.command.cwd.clone();
    query.env = plan.command.env.clone();
    query.env.push(("CARGO_HOME".into(), home.clone().into()));
    query.env_remove = plan.command.env_remove.clone();
    query
        .env_remove
        .extend(["CARGO_MAKEFLAGS", "MAKEFLAGS", "MFLAGS", "CARGO_LOG"].map(str::to_owned));
    collect_report(tools, &query, &id, logs)
}

fn collect_report(tools: &dyn Tools, query: &Run, id: &str, logs: &std::path::Path) -> Collection {
    let unavailable = Collection::Unavailable;
    let output = match tools.run_complete(query, cache_work::MAX_REPORT_BYTES) {
        Ok(output) => output,
        _ => return unavailable(UnavailableReason::QueryFailed),
    };
    if output.stdout.len() > cache_work::MAX_REPORT_BYTES {
        return unavailable(UnavailableReason::InvalidReport);
    }
    if write_new(&logs.join("cache-work.json"), &output.stdout).is_err() {
        return unavailable(UnavailableReason::PersistenceFailed);
    }
    if !output.success() {
        return unavailable(UnavailableReason::QueryFailed);
    }
    let report = match cache_work::parse(&output.stdout, id) {
        Ok(report) => report,
        Err(_) => return unavailable(UnavailableReason::InvalidReport),
    };
    Collection::Report(report)
}

#[cfg(test)]
mod tests {
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
}
