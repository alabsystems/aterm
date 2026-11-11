// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::*;
use crate::triple::{AARCH64_UNKNOWN_LINUX_GNU, X86_64_UNKNOWN_LINUX_GNU};
use std::cell::Cell;
use std::sync::Mutex;

struct Fixture {
    formulas: BTreeMap<String, Formula>,
    source_required: Vec<String>,
    corrupt: bool,
    downloads: Mutex<Vec<String>>,
}

impl Source for Fixture {
    fn formula(&self, name: &str) -> Result<Record> {
        self.formulas
            .get(name)
            .cloned()
            .map(|formula| Record {
                formula,
                source_dependencies: if name == "git" {
                    self.source_required.clone()
                } else {
                    Vec::new()
                },
            })
            .ok_or_else(|| Error::refused("unknown fixture formula"))
    }
    fn download(&self, bottle: &Bottle, path: &Path) -> Result<()> {
        self.downloads.lock().unwrap().push(bottle.url.clone());
        let bytes = if self.corrupt {
            b"corrupt".as_slice()
        } else {
            bottle.url.as_bytes()
        };
        std::fs::write(path, bytes).map_err(|error| Error::io("write fixture bottle", &error))
    }
}

#[test]
fn official_input_graph_is_targeted_version_bound_deduplicated_and_digest_checked() {
    let mut source = Fixture {
        formulas: BTreeMap::new(),
        source_required: vec!["expat".into()],
        corrupt: false,
        downloads: Mutex::new(Vec::new()),
    };
    for (name, dependencies) in [
        ("git", vec!["curl", "pcre2"]),
        ("curl", vec!["pcre2"]),
        ("pcre2", vec![]),
        ("expat", vec![]),
    ] {
        let url = format!("fixture-{name}");
        let bottles = [("arm64_linux", "arm64"), ("x86_64_linux", "x86_64")]
            .into_iter()
            .map(|(tag, arch)| {
                let url = format!("{url}-{arch}");
                Bottle {
                    tag: tag.into(),
                    sha256: fetch::sha256_bytes(url.as_bytes()),
                    url,
                    cellar: "/home/linuxbrew/.linuxbrew/Cellar".into(),
                }
            })
            .collect();
        source.formulas.insert(
            name.into(),
            Formula {
                name: name.into(),
                version: "2.56.0".into(),
                stable_url: url.clone(),
                stable_sha256: fetch::sha256_bytes(url.as_bytes()),
                dependencies: dependencies.into_iter().map(str::to_string).collect(),
                bottle_files: bottles,
                ..Formula::default()
            },
        );
    }
    let tmp = aterm_tempfile::tempdir().unwrap();
    let mut request = Request {
        program: "git".into(),
        version: "2.56.0".into(),
        build: 1,
        triple: AARCH64_UNKNOWN_LINUX_GNU,
        merge_from: None,
        out: tmp.path().join("out"),
    };
    let inputs = collect(&request, &source, &tmp.path().join("positive")).unwrap();
    assert_eq!(inputs.donors.len(), 3);
    assert_eq!(std::fs::read(inputs.archive).unwrap(), b"fixture-git");
    assert!(
        source
            .downloads
            .lock()
            .unwrap()
            .iter()
            .skip(1)
            .all(|url| url.ends_with("-arm64"))
    );
    assert_eq!(
        source.downloads.lock().unwrap().len(),
        4,
        "a shared runtime dependency is fetched once"
    );
    source.downloads.lock().unwrap().clear();
    let mut intel = request.clone();
    intel.triple = X86_64_UNKNOWN_LINUX_GNU;
    let intel_inputs = collect(&intel, &source, &tmp.path().join("intel")).unwrap();
    assert_eq!(intel_inputs.donors.len(), 3);
    assert!(
        source
            .downloads
            .lock()
            .unwrap()
            .iter()
            .skip(1)
            .all(|url| url.ends_with("-x86_64"))
    );
    source.downloads.lock().unwrap().clear();
    request.version = "2.55.0".into();
    assert!(collect(&request, &source, &tmp.path().join("version-mismatch")).is_err());
    assert!(source.downloads.lock().unwrap().is_empty());
    request.version = "2.56.0".into();
    source.corrupt = true;
    assert!(collect(&request, &source, &tmp.path().join("digest-mismatch")).is_err());
    source.corrupt = false;
    source
        .formulas
        .get_mut("pcre2")
        .unwrap()
        .bottle_files
        .clear();
    assert!(collect(&request, &source, &tmp.path().join("missing-target")).is_err());
}

#[test]
fn gnu_source_build_adds_runtime_mac_system_dependencies_without_build_only_tools() {
    let doc = aterm_json::json!({"uses_from_macos": ["expat", {"perl": ["build"]}, {"zlib": []}]});
    assert_eq!(source_dependencies(&doc).unwrap(), ["expat", "zlib"]);
    assert!(source_dependencies(&aterm_json::json!({})).is_err());
    assert!(source_dependencies(&aterm_json::json!({"uses_from_macos": [false]})).is_err());
}

fn public_git_status(status: u16) -> aterm_update_core::git_https::Failure {
    aterm_update_core::git_https::Failure::diagnostic(format!(
        "fatal: unable to access 'https://github.com/git/git.git/': The requested URL returned error: {status}"
    ))
}

/// NEGATIVE CONTROLS: the qualification's public `ls-remote` takes the shared anonymous
/// transport, so transient service failures recover but an absent repository is final.
#[test]
fn public_git_qualification_retries_two_504s_but_never_a_404() {
    let calls = Cell::new(0_u32);
    let mut retries = Vec::new();
    let answer = anonymous_git_qualification(
        |attempt, _| {
            calls.set(calls.get() + 1);
            if attempt < 3 {
                Err(public_git_status(504))
            } else {
                Ok(200)
            }
        },
        |retry| retries.push(retry),
    )
    .expect("the third public read answers");
    assert_eq!(answer, 200);
    assert_eq!(calls.get(), 3);
    assert_eq!(retries.len(), 2);

    calls.set(0);
    let absent = anonymous_git_qualification(
        |_, _| {
            calls.set(calls.get() + 1);
            Err::<(), _>(public_git_status(404))
        },
        |_| panic!("404 must be final"),
    )
    .expect_err("404 is an immediate refusal");
    assert_eq!(calls.get(), 1);
    assert!(!absent.to_string().contains("19 attempts"), "{absent}");
}

#[test]
fn public_git_qualification_names_its_bound_and_recovers_curl_56() {
    let calls = Cell::new(0_u32);
    let exhausted = anonymous_git_qualification(
        |_, _| {
            calls.set(calls.get() + 1);
            Err::<(), _>(public_git_status(503))
        },
        |_| {},
    )
    .expect_err("an endless outage is bounded");
    assert_eq!(calls.get(), 19);
    assert!(exhausted.to_string().contains("19 attempts / 200s"));

    calls.set(0);
    let answer = anonymous_git_qualification(
        |attempt, _| {
            calls.set(calls.get() + 1);
            if attempt == 1 {
                Err(aterm_update_core::git_https::Failure::diagnostic(
                    "error: RPC failed; curl 56 Recv failure: Connection reset by peer",
                ))
            } else {
                Ok(200)
            }
        },
        |_| {},
    )
    .expect("curl exit 56 is transient");
    assert_eq!(answer, 200);
    assert_eq!(calls.get(), 2);
}
