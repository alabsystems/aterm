// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use std::path::PathBuf;
use std::sync::Arc;

use aterm_agent::harness::footer::{FactsOwner, FooterFacts};
use atpkg::image::{ImageBuild, ImageSource, ProgramTag};

use super::{RegistryDrift, SessionTimeline};
use crate::holder_image::{EdgeKind, HolderImage};

fn build(version: &str) -> u64 {
    atpkg::vendor_direct::Version::parse(version)
        .expect("a vendor build")
        .build_id()
}

fn image(
    tl: &mut SessionTimeline,
    pgid: i32,
    exe: &str,
    source: ImageSource,
    version: Option<&str>,
) {
    tl.offer_holder_image(
        Arc::new(HolderImage::of(
            pgid,
            Some(5),
            Some(PathBuf::from(exe)),
            ImageBuild {
                program: ProgramTag::new("claude"),
                build: version.map_or(0, build),
                source,
            },
            1,
        )),
        EdgeKind::Exec,
    );
}

fn row(tl: &mut SessionTimeline, pgid: i32, id: &str, version: &str) {
    tl.set_claude_footer(
        pgid,
        None,
        Some(FooterFacts {
            version: Some(version.into()),
            owner: Some(FactsOwner {
                floor: None,
                session_id: id.into(),
            }),
            ..FooterFacts::default()
        }),
    );
}

const STORE_304: &str = "/p/pkg/store/claude/1000002000001000304/bin/claude";

#[test]
fn a_row_that_disagrees_with_its_drawer_image_is_said_once() {
    let mut tl = SessionTimeline::default();
    tl.note_foreground_group(42);
    image(&mut tl, 42, STORE_304, ImageSource::Store, Some("2.1.304"));
    assert_eq!(tl.take_registry_drift(), None, "no row yet");
    row(&mut tl, 42, "a", "2.1.303");
    assert_eq!(
        tl.take_registry_drift(),
        Some(RegistryDrift {
            pgid: 42,
            image: "2.1.304".into(),
            source: ImageSource::Store,
            reported: "2.1.303".into(),
        })
    );
    assert_eq!(tl.take_registry_drift(), None, "said once");
    row(&mut tl, 42, "a", "2.1.302");
    assert_eq!(
        tl.take_registry_drift().map(|drift| drift.reported),
        Some("2.1.302".into()),
        "another reported build is said"
    );
    row(&mut tl, 42, "a", "2.1.304");
    assert_eq!(tl.take_registry_drift(), None, "agreeing");
    let mut native = SessionTimeline::default();
    native.note_foreground_group(43);
    image(
        &mut native,
        43,
        "/h/.local/share/claude/versions/2.1.305",
        ImageSource::Native,
        Some("2.1.305"),
    );
    row(&mut native, 43, "b", "2.1.303");
    assert_eq!(
        native.take_registry_drift().map(|drift| drift.source),
        Some(ImageSource::Native)
    );
}

#[test]
fn a_stale_row_an_unknown_spelling_or_no_drawer_is_no_drift() {
    let mut tl = SessionTimeline::default();
    tl.note_foreground_group(42);
    row(&mut tl, 42, "a", "2.1.303");
    image(&mut tl, 42, STORE_304, ImageSource::Store, Some("2.1.304"));
    assert_eq!(tl.take_registry_drift(), None, "the replaced image's row");
    row(&mut tl, 42, "b", "2.1.303");
    assert!(tl.take_registry_drift().is_some(), "the new image's row");
    let mut odd = SessionTimeline::default();
    odd.note_foreground_group(42);
    image(&mut odd, 42, STORE_304, ImageSource::Store, Some("2.1.304"));
    row(&mut odd, 42, "a", "nightly");
    assert_eq!(odd.take_registry_drift(), None, "an unknown spelling");
    for (exe, source, build) in [
        ("/p/agents/claude", ImageSource::Launcher, None),
        ("/opt/elsewhere/claude", ImageSource::Unknown, None),
        (STORE_304, ImageSource::StoreRecord, Some("2.1.304")),
    ] {
        let mut tl = SessionTimeline::default();
        tl.note_foreground_group(42);
        image(&mut tl, 42, exe, source, build);
        row(&mut tl, 42, "a", "2.1.303");
        assert_eq!(tl.take_registry_drift(), None, "{source:?}");
    }
}
