// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! The production release host's own decisions, over what `gh` and the download host
//! answer: an HTTP 404 is an ANSWER (not found), a 500 or a dropped connection is not,
//! a name that exists is a refusal; a create is always a prerelease that never takes
//! `latest` (the structural half of the retired prerelease suite); an upload replaces
//! only when asked; a download always overwrites its own partial files; and a write is
//! only ever made as a token file's identity.

use std::ffi::OsString;
use std::path::{Path, PathBuf};

use atpkg_publish::host::gh::{
    GhCli, classify, create_args, download_args, parse_release_line, probe_of, publish_draft_args,
    releases_args, upload_args, view_args,
};
use atpkg_publish::host::{HostError, Probe, ReleaseHost, Replace, Slug};

fn strs(v: &[OsString]) -> Vec<String> {
    v.iter().map(|s| s.to_string_lossy().into_owned()).collect()
}

#[test]
fn a_404_is_an_answer_and_a_500_is_not() {
    for not_found in [
        "release not found\n",
        "gh: Not Found (HTTP 404)\n",
        "no assets match the file pattern\n",
        "GraphQL: Could not resolve to a Repository with the name 'x/y'.\n",
    ] {
        assert_eq!(classify(not_found), HostError::NotFound, "{not_found}");
    }
    for unreachable in [
        "HTTP 500: Internal Server Error (https://api.github.com/...)\n",
        "error connecting to api.github.com\n",
        "gh: Bad credentials (HTTP 401)\n",
        "",
    ] {
        assert!(
            matches!(classify(unreachable), HostError::Unreachable(_)),
            "{unreachable:?} must never read as absent"
        );
    }
    assert!(matches!(
        classify("HTTP 422: Validation Failed\nrelease.tag_name already exists\n"),
        HostError::Refused(_)
    ));
    assert!(matches!(
        classify("a release with the same tag name already exists: v1"),
        HostError::Refused(_)
    ));
}

#[test]
fn the_download_host_status_maps_to_a_probe() {
    assert_eq!(probe_of(302), Probe::Present);
    assert_eq!(probe_of(200), Probe::Present);
    assert_eq!(probe_of(404), Probe::Absent);
    assert!(matches!(probe_of(500), Probe::Unknown(_)));
    assert!(matches!(probe_of(403), Probe::Unknown(_)));
}

#[test]
fn every_create_is_a_prerelease_that_never_takes_latest() {
    let slug = Slug::new("alabsystems", "aterm");
    let args = strs(&create_args(
        &slug,
        "atpkg-index-9",
        "atpkg index 9",
        "notes",
        &[PathBuf::from("/q/index.toml")],
    ));
    assert_eq!(
        &args[..5],
        [
            "release",
            "create",
            "atpkg-index-9",
            "-R",
            "alabsystems/aterm"
        ]
    );
    assert!(args.contains(&"--prerelease".to_string()));
    assert!(args.contains(&"--latest=false".to_string()));
    assert_eq!(args.last().map(String::as_str), Some("/q/index.toml"));
}

#[test]
fn an_upload_replaces_only_when_asked_and_a_download_always_overwrites_its_own() {
    let slug = Slug::new("alabsystems", "ay");
    let files = [PathBuf::from("/s/a.tar.zst")];
    assert!(!strs(&upload_args(&slug, "t", &files, Replace::Never)).contains(&"--clobber".into()));
    assert!(strs(&upload_args(&slug, "t", &files, Replace::Clobber)).contains(&"--clobber".into()));
    let dl = strs(&download_args(
        &slug,
        "t",
        &["pkg-ay-1.toml", "odd[name]*.toml"],
        Path::new("/w"),
    ));
    assert!(dl.contains(&"--clobber".to_string()));
    assert!(dl.contains(&"pkg-ay-1.toml".to_string()));
    assert!(
        dl.contains(&"odd\\[name\\]\\*.toml".to_string()),
        "an asset name is matched literally, never as a glob: {dl:?}"
    );
    let ed = strs(&publish_draft_args(&slug, "t"));
    assert!(ed.contains(&"--draft=false".to_string()) && ed.contains(&"--prerelease".to_string()));
    assert!(
        strs(&releases_args(&slug))
            .iter()
            .any(|a| a == "repos/alabsystems/ay/releases?per_page=100")
    );
    assert!(
        strs(&view_args(&slug, "t")).contains(&"tagName,isDraft,isPrerelease,assets".to_string()),
        "a view reads draftness, so a converge into a draft is never blind"
    );
}

#[test]
fn a_release_line_reads_back_typed_and_a_mangled_one_is_refused() {
    let r = parse_release_line(
        "atpkg-index-68\tfalse\ttrue\taterm-machines.toml/aterm-machines.toml.sig/index.toml/index.toml.sig",
    )
    .expect("parses");
    assert_eq!(r.tag, "atpkg-index-68");
    assert!(!r.draft && r.prerelease);
    assert_eq!(r.assets.len(), 4);
    let empty = parse_release_line("atpkg-ay-1\ttrue\ttrue\t").expect("no assets");
    assert!(empty.draft && empty.assets.is_empty());
    for bad in [
        "",
        "atpkg-ay-1",
        "atpkg-ay-1\tmaybe\ttrue\t",
        "\tfalse\tfalse\t",
        "a\tfalse\tfalse\tx\textra",
    ] {
        assert!(parse_release_line(bad).is_none(), "{bad:?}");
    }
}

/// gh's keychain login never writes: a write with no token file, or an empty one, is
/// refused before gh runs.
#[test]
fn a_write_without_a_token_file_is_refused_before_gh_runs() {
    let slug = Slug::new("alabsystems", "ay");
    let dir = super::scratch("gh-token");
    let empty = dir.path().join("gh_access_token");
    std::fs::write(&empty, "\n").expect("write");
    for (gh, says) in [
        (GhCli::new(None), "never falls back to gh's own login"),
        (GhCli::new(Some(empty)), "is empty"),
    ] {
        for r in [
            gh.create(&slug, "atpkg-ay-1", "t", "n", &[]),
            gh.upload(&slug, "atpkg-ay-1", &[], Replace::Never),
            gh.publish_draft(&slug, "atpkg-ay-1"),
        ] {
            assert!(
                matches!(&r, Err(HostError::Refused(m)) if m.contains(says)),
                "{r:?}"
            );
        }
    }
}

/// Only a real network service is waited on between retries; an in-memory host never is.
#[test]
fn only_the_real_host_pauses_between_retries() {
    assert!(GhCli::new(None).retry_pause() > std::time::Duration::ZERO);
    assert_eq!(
        atpkg_publish::host::fake::FakeHost::new().retry_pause(),
        std::time::Duration::ZERO
    );
}
