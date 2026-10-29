// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Offline Monitor inputs for a private Lab window. This never changes its fleet scope or
//! discovers peers: the real census and cached-Hello projections consume only the named file.
//!
//! Pass `--env ATERM_DEBUG_MONITOR_FIXTURE=<scratch-home>/monitor.json` to `lab instance`,
//! then copy `crates/aterm-gui/fixtures/monitor.json` there after the instance has made HOME.
//! Open `/monitor` through its private control socket. The JSON wraps a normal `census`,
//! optional `now_s` and `local_label`, and `configured` rows containing `uuid`, `name`,
//! optional `display_name`, `address`, and the ordinary cached machine `state` (`hello`
//! remains a JSON string). Only the address and raw `name` may match a census identity.

use std::io::Read as _;
use std::path::Path;

use super::{TabComputer, TabSpreadsheetRow};

const MAX_BYTES: u64 = 1024 * 1024;

/// Read only in an isolated process with a scratch HOME. The collector retains its ordinary
/// isolation banner and never reaches sibling sockets, fleet credentials, or network discovery.
pub(super) fn read() -> Option<(Vec<TabComputer>, Vec<TabSpreadsheetRow>)> {
    let path = aterm_types::dev_seam!("ATERM_DEBUG_MONITOR_FIXTURE")?;
    let home = aterm_types::dirs::home_dir()?.canonicalize().ok()?;
    let account = aterm_uds::account::home()?.canonicalize().ok()?;
    if home == account {
        return None;
    }
    read_in(Path::new(&path), &home)
}

fn read_in(path: &Path, home: &Path) -> Option<(Vec<TabComputer>, Vec<TabSpreadsheetRow>)> {
    if !path.is_absolute() {
        return None;
    }
    let path = path.canonicalize().ok()?;
    let home = home.canonicalize().ok()?;
    if !path.starts_with(home) {
        return None;
    }
    let meta = path.metadata().ok()?;
    if !meta.is_file() || meta.len() > MAX_BYTES {
        return None;
    }
    let file = std::fs::File::open(path).ok()?;
    let mut raw = String::new();
    file.take(MAX_BYTES + 1).read_to_string(&mut raw).ok()?;
    if u64::try_from(raw.len()).ok()? > MAX_BYTES {
        return None;
    }
    project(&raw)
}

fn project(raw: &str) -> Option<(Vec<TabComputer>, Vec<TabSpreadsheetRow>)> {
    let root = aterm_json::from_str::<aterm_json::Value>(raw).ok()?;
    let census = aterm_agent::fleet_census::SwarmCensus::from_json(
        &aterm_json::to_string(root.get("census")?).ok()?,
    )
    .ok()?;
    let now_s = root
        .get("now_s")
        .and_then(aterm_json::Value::as_u64)
        .unwrap_or(census.generated_at);
    let local = root.get("local_label").and_then(aterm_json::Value::as_str);
    let (mut computers, known) = super::swarm_census_to_tab_computers(&census, local, now_s);
    let mut tabs = Vec::new();
    if let Some(configured) = root.get("configured") {
        for row in configured.as_array()? {
            let text = |key| row.get(key).and_then(aterm_json::Value::as_str);
            let (cached, host_tabs) = super::cached_configured_computer(
                text("uuid")?.to_owned(),
                text("display_name").or_else(|| text("name"))?.to_owned(),
                text("address")?.to_owned(),
                row.get("state"),
            );
            super::merge_cached_computer(
                &mut computers,
                &known,
                text("name")?,
                cached,
                host_tabs,
                &mut tabs,
            );
        }
    }
    Some((computers, tabs))
}

#[cfg(test)]
mod tests {
    use super::*;

    const EXAMPLE: &str = include_str!("../fixtures/monitor.json");

    #[test]
    fn private_monitor_fixture_uses_census_and_cached_facts_without_reading_outside_home() {
        let root = aterm_tempfile::tempdir().expect("owned fixture world");
        let home = root.path().join("home");
        std::fs::create_dir(&home).expect("scratch HOME");
        let fixture = home.join("monitor.json");
        std::fs::write(&fixture, EXAMPLE).expect("fixture");
        let (computers, tabs) = read_in(&fixture, &home).expect("offline projection");
        assert_eq!(computers.len(), 4);
        let enriched = computers.iter().find(|row| row.name == "m100").unwrap();
        assert_eq!(enriched.os_version.as_deref(), Some("macOS 27.0"));
        assert_eq!(enriched.instances, Some(2));
        assert_eq!(enriched.tabs, Some(2));
        assert_eq!(enriched.ai_clis, Some(2));
        assert_eq!(enriched.active_ai, Some(1));
        assert!(enriched.vitals.contains("CPU 12%"));
        assert_eq!(
            tabs.len(),
            2,
            "distinct sessions remain while the repeated alias observation shares one row"
        );
        assert_eq!(tabs[0].computer, "m100");
        let unknown = computers.iter().find(|row| row.name == "m101").unwrap();
        assert_eq!((unknown.tabs, unknown.ai_clis), (None, None));
        let zero = computers.iter().find(|row| row.name == "m102").unwrap();
        assert_eq!((zero.tabs, zero.ai_clis), (Some(0), Some(0)));
        let outside = root.path().join("outside.json");
        std::fs::write(&outside, EXAMPLE).expect("outside fixture");
        assert!(read_in(&outside, &home).is_none());
        #[cfg(unix)]
        {
            let link = home.join("outside.json");
            std::os::unix::fs::symlink(&outside, &link).expect("escaping link");
            assert!(read_in(&link, &home).is_none());
        }
        assert!(project("{}").is_none());
    }
}
