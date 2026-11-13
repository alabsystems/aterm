// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::site::releases::{LinuxDownload, LinuxKind, MacDownload};
use crate::site::toolchain::Coverage;
use std::collections::BTreeMap;

fn page(name: &str) -> String {
    let dir = aterm_tempfile::manifest_dir().join("tests/fixtures/site/public");
    std::fs::read_to_string(dir.join(name)).unwrap()
}

const DMG63: &str =
    "https://github.com/alabsystems/aterm/releases/download/v0.63.0/aterm-0.63.0.dmg";

fn facts(size: Option<u64>) -> Facts {
    Facts {
        slug: "alabsystems/aterm".into(),
        mac: MacDownload {
            tag: "v0.63.0".into(),
            version: "0.63.0".into(),
            label: "aterm-0.63.0.dmg".into(),
            url: DMG63.into(),
            size_bytes: size,
        },
        head: "v0.63.0".into(),
        intel_proof: None,
        linux: [None, None],
    }
}

fn tar(v: &str) -> LinuxDownload {
    let label = format!("aterm-{v}-linux-x86_64.tar.gz");
    LinuxDownload {
        tag: format!("v{v}"),
        version: v.into(),
        url: format!("https://github.com/alabsystems/aterm/releases/download/v{v}/{label}"),
        label,
        kind: LinuxKind::LegacyTar,
    }
}

#[test]
fn the_buttons_and_the_note_name_the_dmg_and_its_true_size() {
    let f = facts(Some(1_067_044_990));
    for name in ["index.html", "aterm.html"] {
        let out = rewrite(name, &page(name), &f, None).unwrap();
        assert_eq!(hrefs(name, &out), [DMG63], "{name}");
        assert_eq!(
            note_cells(&out),
            ["<code>aterm-0.63.0.dmg</code> &middot; <code>1.07&nbsp;GB</code>"],
            "{name}"
        );
    }
    let out = rewrite("terminal.html", &page("terminal.html"), &f, None).unwrap();
    assert_eq!(hrefs("terminal.html", &out), [DMG63]);
    assert_eq!(title_versions(&out), ["0.63.0"]);
    assert_eq!(
        quoted(&out, "<a class=\"get register\" href=\"", "\"").len(),
        1
    );
}

#[test]
fn an_unknown_size_leaves_the_name_alone_never_a_guess() {
    let out = rewrite("index.html", &page("index.html"), &facts(None), None).unwrap();
    assert_eq!(note_cells(&out), ["<code>aterm-0.63.0.dmg</code>"]);
}

#[test]
fn the_release_notes_link_points_at_releases() {
    let out = rewrite("aterm.html", &page("aterm.html"), &facts(None), None).unwrap();
    let links = quoted(&out, "<a href=\"", "\">release notes</a>");
    assert_eq!(&out[links[0].inner.0..links[0].inner.1], "/releases");
}

#[test]
fn the_platform_attributes_follow_the_facts_and_never_reach_the_homepage() {
    let mut f = facts(Some(1));
    f.linux[0] = Some(tar("0.63.0"));
    f.intel_proof = Some("https://example.invalid/probe.txt".into());
    let levels = [Coverage::Partial, Coverage::Full];
    for name in ["aterm.html", "terminal.html"] {
        let out = rewrite(name, &page(name), &f, Some(&levels)).unwrap();
        let one = |attr| attribute_values(&out, attr);
        assert_eq!(one("data-href-linux-x86_64"), [tar("0.63.0").url], "{name}");
        assert_eq!(
            one("data-href-linux-aarch64"),
            Vec::<String>::new(),
            "{name}"
        );
        assert_eq!(
            one("data-intel-proof"),
            ["https://example.invalid/probe.txt"]
        );
        assert_eq!(one("data-toolchain-x86_64"), ["partial"]);
        assert_eq!(one("data-toolchain-aarch64"), ["full"]);
        assert_eq!(
            rewrite(name, &out, &f, Some(&levels)).unwrap(),
            out,
            "{name}"
        );
        let bare = rewrite(name, &out, &facts(Some(1)), None).unwrap();
        for (attr, _) in platform_attributes(&f, Some(&levels)) {
            assert!(attribute_values(&bare, attr).is_empty(), "{name} {attr}");
        }
    }
    let home = rewrite("index.html", &page("index.html"), &f, Some(&levels)).unwrap();
    for (attr, _) in platform_attributes(&f, Some(&levels)) {
        assert!(attribute_values(&home, attr).is_empty(), "{attr}");
    }
}

fn signed(v: &str, arch: &str) -> LinuxDownload {
    let label = format!("aterm-{v}-linux-{arch}");
    LinuxDownload {
        tag: format!("v{v}"),
        version: v.into(),
        url: format!("https://github.com/alabsystems/aterm/releases/download/v{v}/{label}"),
        label,
        kind: LinuxKind::SignedRaw {
            sha256: "b".repeat(64),
            size: 9000,
        },
    }
}

#[test]
fn the_install_command_is_written_only_for_a_signed_executable() {
    let mut f = facts(Some(1));
    f.linux = [Some(tar("0.61.0")), Some(signed("0.63.0", "aarch64"))];
    let plain = "curl -fsSL https://alab.systems/install.sh | bash";
    assert_eq!(linux_install_command(&f, "x86_64"), None);
    assert_eq!(linux_install_command(&f, "aarch64").as_deref(), Some(plain));
    for name in ["aterm.html", "terminal.html"] {
        let out = rewrite(name, &page(name), &f, None).unwrap();
        assert_eq!(
            attribute_values(&out, "data-install-linux-aarch64"),
            [plain],
            "{name}"
        );
        assert!(
            attribute_values(&out, "data-install-linux-x86_64").is_empty(),
            "{name}"
        );
        assert_eq!(
            attribute_values(&out, "data-href-linux-aarch64"),
            [signed("0.63.0", "aarch64").url],
            "{name}"
        );
    }
    f.linux[0] = Some(signed("0.62.0", "x86_64"));
    assert_eq!(
        linux_install_command(&f, "x86_64").as_deref(),
        Some("curl -fsSL https://alab.systems/install.sh | bash -s -- --version 0.62.0")
    );
    let home = rewrite("index.html", &page("index.html"), &f, None).unwrap();
    assert!(attribute_values(&home, "data-install-linux-aarch64").is_empty());
}

#[test]
fn older_note_shapes_converge_on_name_and_size_and_a_rewrite_is_idempotent() {
    let f = facts(Some(1_067_044_990));
    let old = page("aterm.html").replace(
        "<code>aterm-0.61.0.dmg</code> &middot; <code>31&nbsp;MB</code>",
        "<code>aterm-0.61.0-mac.zip</code> &middot; <code>0.61.0</code> &middot; \
         <code>~27&nbsp;MB</code>",
    );
    let name_only = page("aterm.html").replace(" &middot; <code>31&nbsp;MB</code>", "");
    assert_ne!(old, page("aterm.html"));
    assert_ne!(name_only, page("aterm.html"));
    let a = rewrite("aterm.html", &old, &f, None).unwrap();
    let b = rewrite("aterm.html", &name_only, &f, None).unwrap();
    assert_eq!(a, b);
    assert_eq!(rewrite("aterm.html", &a, &f, None).unwrap(), a);
    assert!(!a.contains("~27") && !a.contains("mac.zip"));
}

#[test]
fn a_page_that_lost_a_rules_shape_is_refused_by_name() {
    let f = facts(Some(1));
    let renamed = page("index.html").replace("class=\"download\"", "class=\"dl\"");
    assert_eq!(
        rewrite("index.html", &renamed, &f, None).unwrap_err(),
        PageError::Shape {
            page: "index.html".into(),
            rule: Rule::DownloadAnchor,
            found: 0
        }
    );
    let devtitle = page("terminal.html").replace("aterm v0.61.0&nbsp;", "aterm (dev) ");
    assert_eq!(
        rewrite("terminal.html", &devtitle, &f, None).unwrap_err(),
        PageError::Shape {
            page: "terminal.html".into(),
            rule: Rule::HeaderTitle,
            found: 0
        }
    );
    let twice = page("aterm.html").replace(
        "data-href-linux-x86_64=",
        "data-intel-proof=\"a\" data-intel-proof=\"b\" data-href-linux-x86_64=",
    );
    assert_eq!(
        rewrite("aterm.html", &twice, &f, None).unwrap_err(),
        PageError::Duplicate {
            page: "aterm.html".into(),
            attr: "data-intel-proof"
        }
    );
}

#[test]
fn the_live_comparison_names_each_difference() {
    let mut f = facts(Some(1));
    f.linux[0] = Some(tar("0.63.0"));
    let levels = [Coverage::Partial, Coverage::Full];
    let mut local = BTreeMap::new();
    for name in PAGES {
        local.insert(name, rewrite(name, &page(name), &f, Some(&levels)).unwrap());
    }
    local.insert("engine.js", "engine\n".into());
    let mut live = local.clone();
    let lookup = |m: &BTreeMap<&'static str, String>| {
        let m = m.clone();
        move |k: &str| m.get(k).cloned()
    };
    let (l, v) = (lookup(&local), lookup(&live));
    assert_eq!(compare(&f, &Copies { get: &l }, &Copies { get: &v }), []);
    local.insert("install.sh", "#!/usr/bin/env bash\n".into());
    let l = lookup(&local);
    assert_eq!(
        compare(&f, &Copies { get: &l }, &Copies { get: &v }),
        [Difference::InstallerNotFetched]
    );
    live.insert("install.sh", "#!/bin/sh\n".into());
    let v = lookup(&live);
    assert_eq!(
        compare(&f, &Copies { get: &l }, &Copies { get: &v }),
        [Difference::InstallerDiffers]
    );
    live.insert("install.sh", local["install.sh"].clone());
    live.insert(
        "aterm.html",
        local["aterm.html"].replace(
            "data-toolchain-x86_64=\"partial\"",
            "data-toolchain-x86_64=\"full\"",
        ),
    );
    live.insert("engine.js", "stale\n".into());
    live.remove("terminal.html");
    let v = lookup(&live);
    assert_eq!(
        compare(&f, &Copies { get: &l }, &Copies { get: &v }),
        [
            Difference::EngineDiffers,
            Difference::Attribute {
                page: "aterm.html",
                attr: "data-toolchain-x86_64",
                live: vec!["full".into()],
                want: vec!["partial".into()]
            },
            Difference::PageNotFetched("terminal.html"),
        ]
    );
}
