// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The installer's channel grammar against the updater's: tag classification,
//! release order, the evergreen head pointer and the repository slug, each on a
//! generated corpus, so a disagreement that would strand or misdirect a fresh
//! install fails here. The named cases below them are the installer's own
//! contract where the updater has no twin (the asset allowlist, the manifest
//! identity bind, the Linux architecture map).

use aterm_update_core::pointer::{self, canonical_app_tag};
use aterm_update_core::tag::{TagKind, parse_release_tag};
use aterm_update_core::{HeadAnswer, is_valid_slug};

use crate::support::{Lib, Rng, Scratch, q, s};

/// The locales a `curl … | bash` meets: none set, and a person's UTF-8 shell.
const LOCALES: [&str; 2] = ["C", "en_US.UTF-8"];

/// Spellings a tag component can take, canonical and not.
const COMPONENTS: &[&str] = &[
    "0",
    "1",
    "2",
    "9",
    "10",
    "99",
    "100",
    "01",
    "00",
    "007",
    "",
    "x",
    "1a",
    "+1",
    "-1",
    " 1",
    "1 ",
    "4294967296",
    "18446744073709551615",
    "18446744073709551616",
    "99999999999999999999",
    "018446744073709551615",
    "\u{663}",
    "\u{ff12}",
    "1e3",
    "0x1",
];

fn tag_corpus() -> Vec<String> {
    let mut tags: Vec<String> = [
        "v0.2.0",
        "v0.2.1",
        "v10.20.30",
        "v0.21.2607041853",
        "v0.0.0",
        "v0.61",
        "v0.25",
        "v1.0",
        "v1",
        "v0.2.0.1",
        "0.2.0",
        "V0.2.0",
        "v",
        "",
        "v0.legacy.1",
        "v0.02.0",
        "v01.2.3",
        "v0.2.",
        "v0..2",
        "v.10.0",
        "v0.10.0-rc1",
        "v1.2.99999999999999999999",
        "v0.1.18446744073709551615",
        "v0.1.18446744073709551616",
        "v18446744073709551616.0",
        "atpkg-index-12",
    ]
    .iter()
    .map(|t| (*t).to_string())
    .collect();
    let small = &COMPONENTS[..10];
    for a in small {
        tags.push(format!("v{a}"));
        for b in small {
            tags.push(format!("v{a}.{b}"));
            for c in small {
                tags.push(format!("v{a}.{b}.{c}"));
            }
        }
    }
    let mut rng = Rng::new(0x7a67_5f67_7261_6d6d);
    for _ in 0..1500 {
        let prefix = *rng.pick(&["v", "v", "v", "", "V", "vv", "v "]);
        let arity = rng.below(6);
        let parts: Vec<&str> = (0..arity).map(|_| *rng.pick(COMPONENTS)).collect();
        let suffix = *rng.pick(&["", "", "", "-rc1", ".", "+build", "\t"]);
        tags.push(format!("{prefix}{}{suffix}", parts.join(".")));
    }
    tags.retain(|t| !t.contains('\n'));
    tags.sort();
    tags.dedup();
    tags
}

/// What the updater makes of `tag`: its kind, and whether it may be the
/// channel's elected release.
fn updater_tag(tag: &str) -> (&'static str, bool) {
    let kind = match parse_release_tag(tag) {
        Ok(TagKind::Candidate(_)) => "candidate",
        Ok(TagKind::Legacy) => "legacy",
        Err(_) => "refused",
    };
    (kind, canonical_app_tag(tag))
}

/// The tags whose classification differs between the installer and the
/// updater, with `redefine` (shell code run after sourcing) applied first.
fn tag_disagreements(scratch: &Scratch, redefine: &str, locale: &str) -> Vec<String> {
    let corpus = tag_corpus();
    let file = scratch.write("tags", corpus.join("\n") + "\n");
    let out = scratch.join("tag-verdicts");
    let mut lib = Lib::new();
    lib.env("LC_ALL", locale).env("LANG", locale);
    lib.step(redefine);
    lib.step(format!(
        "while IFS= read -r tag; do
			if parse_release_tag \"$tag\"; then k=\"$TAG_KIND_RESULT\"; else k=refused; fi
			if canonical_authority_tag \"$tag\"; then a=1; else a=0; fi
			printf '%s %s\\n' \"$k\" \"$a\"
		done <{} >{}",
        q(s(&file)),
        q(s(&out))
    ));
    lib.run(scratch);
    let verdicts = crate::support::read(&out);
    let lines: Vec<&str> = verdicts.lines().collect();
    assert_eq!(lines.len(), corpus.len(), "one verdict per tag");
    corpus
        .iter()
        .zip(lines)
        .filter_map(|(tag, line)| {
            let (kind, authority) = updater_tag(tag);
            let want = format!("{kind} {}", u8::from(authority));
            (line != want).then(|| format!("{tag:?}: install.sh {line}, updater {want}"))
        })
        .collect()
}

#[test]
fn tag_grammar_agrees_with_the_updater_on_a_generated_corpus() {
    let scratch = Scratch::new();
    for locale in LOCALES {
        let wrong = tag_disagreements(&scratch, ":", locale);
        assert!(wrong.is_empty(), "LC_ALL={locale}:\n{}", wrong.join("\n"));
    }
}

/// Negative control: the corpus reaches the u64 bound, so an installer that
/// drops it is caught.
#[test]
fn the_tag_differential_catches_a_dropped_u64_bound() {
    let scratch = Scratch::new();
    let wrong = tag_disagreements(
        &scratch,
        "numeric_component_in_u64() { [[ \"$1\" =~ ^(0|[1-9][0-9]*)$ ]]; }",
        "C",
    );
    assert!(
        wrong
            .iter()
            .any(|w| w.starts_with("\"v0.1.18446744073709551616\"")),
        "{wrong:?}"
    );
}

fn candidate(rng: &mut Rng) -> String {
    let edge = [
        0,
        1,
        9,
        10,
        99,
        100,
        u64::from(u32::MAX),
        10_000_000_000_000_000_000,
        9_999_999_999_999_999_999,
        u64::MAX - 1,
        u64::MAX,
    ];
    let part = |rng: &mut Rng| {
        if rng.below(3) == 0 {
            rng.next()
        } else if rng.below(2) == 0 {
            *rng.pick(&edge)
        } else {
            rng.next() % 12
        }
    };
    let (a, b, c) = (part(rng), part(rng), part(rng));
    format!("v{a}.{b}.{c}")
}

#[test]
fn tag_order_agrees_with_the_updaters_release_order() {
    let scratch = Scratch::new();
    let mut rng = Rng::new(0x6f_7264_6572);
    let mut pairs: Vec<(String, String)> = Vec::new();
    for _ in 0..600 {
        let a = candidate(&mut rng);
        let b = if rng.below(4) == 0 {
            a.clone()
        } else {
            candidate(&mut rng)
        };
        pairs.push((a, b));
    }
    // A noncanonical operand is refused, never ordered. (Two components are
    // the min_os advisory's macOS versions, ordered below.)
    for bad in ["v0.02.0", "v1.2.18446744073709551616", "x"] {
        pairs.push(("v0.2.0".into(), bad.into()));
        pairs.push((bad.into(), "v0.2.0".into()));
    }
    let mut lib = Lib::new();
    let calls: Vec<usize> = pairs
        .iter()
        .map(|(a, b)| {
            lib.call(
                format!(
                    "TAG_COMPARE_RESULT=; compare_numeric_tags {} {}",
                    q(a),
                    q(b)
                ),
                &["TAG_COMPARE_RESULT"],
            )
        })
        .collect();
    let recs = lib.run(&scratch);
    for ((a, b), i) in pairs.iter().zip(calls) {
        let rec = &recs[i];
        match (parse_release_tag(a), parse_release_tag(b)) {
            (Ok(left @ TagKind::Candidate(_)), Ok(right @ TagKind::Candidate(_))) => {
                let want = match left.cmp(&right) {
                    std::cmp::Ordering::Less => "-1",
                    std::cmp::Ordering::Equal => "0",
                    std::cmp::Ordering::Greater => "1",
                };
                assert_eq!(
                    (rec.rc, rec.var("TAG_COMPARE_RESULT")),
                    (0, want),
                    "{a} vs {b}"
                );
            }
            _ => assert_eq!(rec.rc, 2, "{a} vs {b} must be refused"),
        }
    }
}

/// The min_os advisory compares macOS product versions, which have two or
/// three components: a shared prefix orders the shorter operand first.
#[test]
fn a_shared_prefix_orders_the_shorter_operand_first() {
    let scratch = Scratch::new();
    let mut lib = Lib::new();
    let rows = [
        ("v15.5", "v15.5.1", "-1"),
        ("v15.5.1", "v15.5", "1"),
        ("v14.0", "v15.5", "-1"),
        ("v0.9.0", "v0.10.0", "-1"),
        ("v0.99.0", "v0.100.0", "-1"),
        ("v0.2.100", "v0.2.99", "1"),
        ("v0.1.18446744073709551615", "v0.2.0", "-1"),
    ];
    let calls: Vec<usize> = rows
        .iter()
        .map(|(a, b, _)| {
            lib.call(
                format!("compare_numeric_tags {a} {b}"),
                &["TAG_COMPARE_RESULT"],
            )
        })
        .collect();
    let recs = lib.run(&scratch);
    for ((a, b, want), i) in rows.iter().zip(calls) {
        assert_eq!(
            (recs[i].rc, recs[i].var("TAG_COMPARE_RESULT")),
            (0, *want),
            "{a} vs {b}"
        );
    }
}

const OWNER: &str = "example";
const REPO: &str = "aterm";
const APPCAST: &str = "aterm-appcast.toml";

fn location_corpus() -> Vec<(u16, Option<String>)> {
    let hosts = [
        "https://github.com",
        "http://github.com",
        "https://evil.example",
        "https://github.com.evil.example",
        "https://GITHUB.com",
    ];
    let owners = ["example", "Example", "other"];
    let repos = ["aterm", "other", "aterm.git"];
    let middles = [
        "/releases/download/",
        "/releases/latest/download/",
        "/releases/download//",
    ];
    let tags = [
        "v0.92.0",
        "v0.92.0",
        "v0.100.1",
        "v18446744073709551615.0.0",
        "v0.61",
        "v0.09.0",
        "atpkg-index-12",
        "v0.92.0/x",
        "",
        "..",
        "v0.92.0%2F",
        "v0.92.0?x",
        "v0.92.0#x",
    ];
    let assets = [
        "aterm-appcast.toml",
        "aterm-appcast.toml",
        "aterm-appcast.toml.sig",
        "aterm-appcast.toml?x=1",
        "x/aterm-appcast.toml",
        "aterm-machines.toml",
    ];
    let codes = [302, 404, 429, 500, 503, 200, 403];
    let parts: [&[&str]; 6] = [&hosts, &owners, &repos, &middles, &tags, &assets];
    let location = |pick: [&str; 6]| {
        format!(
            "{}/{}/{}{}{}/{}",
            pick[0], pick[1], pick[2], pick[3], pick[4], pick[5]
        )
    };
    let mut locations = Vec::new();
    // Every variant of one part with the rest canonical (each list's first
    // entry), then random mixtures.
    for (at, variants) in parts.iter().enumerate() {
        for variant in *variants {
            let mut pick = parts.map(|p| p[0]);
            pick[at] = variant;
            locations.push(location(pick));
        }
    }
    let mut rng = Rng::new(0x6865_6164);
    for _ in 0..300 {
        locations.push(location(parts.map(|p| *rng.pick(p))));
    }
    let mut corpus = vec![(302, None)];
    for location in locations {
        for code in codes {
            corpus.push((code, Some(location.clone())));
        }
    }
    corpus
}

/// The answers on which the installer's `channel_head` elects a different tag
/// than the updater's pointer, with `redefine` run after sourcing.
fn head_disagreements(scratch: &Scratch, redefine: &str) -> Vec<String> {
    let corpus = location_corpus();
    let mut lib = Lib::new();
    lib.step(format!(
        "REPO_SLUG={OWNER}/{REPO}; web_curl() {{ printf '%s' \"$STUB_ANSWER\"; }}"
    ));
    lib.step(redefine);
    let calls: Vec<usize> = corpus
        .iter()
        .map(|(code, location)| {
            let answer = format!("{code} {}", location.as_deref().unwrap_or(""));
            lib.call(
                format!("STUB_ANSWER={}; channel_head", q(&answer)),
                &["HEAD_TAG"],
            )
        })
        .collect();
    let recs = lib.run(scratch);
    let url = pointer::latest_download_url(OWNER, REPO, APPCAST).expect("a safe source");
    let mut wrong = Vec::new();
    for ((code, location), i) in corpus.iter().zip(calls) {
        let answer = HeadAnswer {
            code: *code,
            location: location.clone(),
        };
        let updater = pointer::classify(OWNER, REPO, APPCAST, &url, Ok(answer), &canonical_app_tag)
            .ok()
            .map(|p| p.tag);
        let installer = (recs[i].rc == 0).then(|| recs[i].var("HEAD_TAG").to_string());
        if updater != installer {
            wrong.push(format!(
                "{code} {location:?}: install.sh {installer:?}, updater {updater:?}"
            ));
        }
    }
    wrong
}

/// Every answer elects the same tag through the installer's `channel_head` as
/// through the updater's pointer.
#[test]
fn the_head_pointer_elects_what_the_updater_elects() {
    let scratch = Scratch::new();
    let wrong = head_disagreements(&scratch, ":");
    assert!(wrong.is_empty(), "{}", wrong.join("\n"));
}

/// Negative control: an installer that elects a retired two-component head is
/// caught.
#[test]
fn the_head_differential_catches_an_elected_legacy_tag() {
    let scratch = Scratch::new();
    let wrong = head_disagreements(
        &scratch,
        "canonical_authority_tag() { parse_release_tag \"$1\"; }",
    );
    assert!(wrong.iter().any(|w| w.contains("/v0.61/")), "{wrong:?}");
}

/// `channel_head`'s own classes: elected (0), a skip with its reason (1), a
/// refusal to interpret the redirect (2) — and the one request it makes.
#[test]
fn the_head_pointer_classifies_each_answer() {
    let scratch = Scratch::new();
    let base = "https://github.com/example/aterm/releases/download";
    let rows: &[(&str, i32, &str)] = &[
        ("302 B/v0.92.0/aterm-appcast.toml", 0, "v0.92.0"),
        (
            "302 https://evil.example/example/aterm/releases/download/v0.92.0/aterm-appcast.toml",
            2,
            "",
        ),
        (
            "302 https://github.com/example/other/releases/download/v0.92.0/aterm-appcast.toml",
            2,
            "",
        ),
        ("302 B/v0.92.0/aterm-appcast.toml.sig", 2, ""),
        ("302 B/v0.92.0/aterm-appcast.toml?x=1", 2, ""),
        ("302 B/v0.92.0/x/aterm-appcast.toml", 2, ""),
        (
            "302 https://github.com/Example/aterm/releases/download/v0.92.0/aterm-appcast.toml",
            2,
            "",
        ),
        ("302 ", 2, ""),
        ("302 B/v0.61/aterm-appcast.toml", 1, ""),
        ("302 B/atpkg-index-12/aterm-appcast.toml", 1, ""),
        ("302 B/v0.09.0/aterm-appcast.toml", 1, ""),
        ("301 B/v0.92.0/aterm-appcast.toml", 1, ""),
        ("404 ", 1, ""),
        ("429 ", 1, ""),
        ("503 ", 1, ""),
        ("200 ", 1, ""),
    ];
    let mut lib = Lib::new();
    let args = scratch.join("stub-args");
    lib.step(format!(
        "REPO_SLUG=example/aterm
		web_curl() {{ printf '%s' \"$*\" >{}; printf '%s' \"$STUB_ANSWER\"; return \"$STUB_STATUS\"; }}",
        q(s(&args))
    ));
    let mut calls: Vec<usize> = rows
        .iter()
        .map(|(answer, _, _)| {
            lib.call(
                format!(
                    "STUB_ANSWER={} STUB_STATUS=0; channel_head",
                    q(&answer.replace('B', base))
                ),
                &["HEAD_TAG", "HEAD_WHY"],
            )
        })
        .collect();
    // curl itself failing is no answer: a skip.
    calls.push(lib.call(
        "STUB_ANSWER='000 ' STUB_STATUS=6; channel_head",
        &["HEAD_TAG", "HEAD_WHY"],
    ));
    let recs = lib.run(&scratch);
    for ((answer, status, tag), i) in rows.iter().chain([&("curl exit 6", 1, "")]).zip(calls) {
        let rec = &recs[i];
        assert_eq!((rec.rc, rec.var("HEAD_TAG")), (*status, *tag), "{answer}");
        if *status != 0 {
            assert!(
                !rec.var("HEAD_WHY").is_empty(),
                "{answer}: a non-answer says why"
            );
        }
    }
    // The last request: ONE redirect-refusing HEAD of the latest pointer.
    let argv = crate::support::read(&args);
    let argv: Vec<&str> = argv.split(' ').collect();
    assert!(argv.contains(&"-I"), "{argv:?}");
    assert!(!crate::support::follows_redirects(&argv), "{argv:?}");
    assert_eq!(
        argv.last(),
        Some(&"https://github.com/example/aterm/releases/latest/download/aterm-appcast.toml")
    );
}

fn slug_corpus() -> Vec<String> {
    let alphabet = [
        "a", "Z", "0", "-", "_", ".", "/", " ", "?", "#", "@", "%", ":", "\u{e9}", "\u{ff21}",
    ];
    let mut slugs: Vec<String> = [
        "a/b",
        "chan-org/aterm",
        "chan-org",
        "chan-org/aterm/extra",
        "/aterm",
        "aterm/",
        "chan org/aterm",
        "chan-org/..",
        "./aterm",
        "https://github.com/chan-org/aterm",
        "repo.name/x.y",
    ]
    .iter()
    .map(|s| (*s).to_string())
    .collect();
    slugs.push(format!("{}/b", "a".repeat(100)));
    slugs.push(format!("{}/b", "a".repeat(101)));
    slugs.push(format!("a/{}", "b".repeat(100)));
    slugs.push(format!("a/{}", "b".repeat(101)));
    let mut rng = Rng::new(0x736c_7567);
    for _ in 0..800 {
        let len = rng.below(9);
        slugs.push((0..len).map(|_| *rng.pick(&alphabet)).collect());
    }
    slugs.sort();
    slugs.dedup();
    slugs
}

#[test]
fn slug_grammar_agrees_with_the_updater() {
    let scratch = Scratch::new();
    for locale in LOCALES {
        let corpus = slug_corpus();
        let mut lib = Lib::new();
        lib.env("LC_ALL", locale).env("LANG", locale);
        let calls: Vec<usize> = corpus
            .iter()
            .map(|slug| lib.call(format!("valid_slug {}", q(slug)), &[]))
            .collect();
        let recs = lib.run(&scratch);
        let wrong: Vec<String> = corpus
            .iter()
            .zip(calls)
            .filter_map(|(slug, i)| {
                let updater = slug
                    .split_once('/')
                    .is_some_and(|(owner, repo)| is_valid_slug(owner) && is_valid_slug(repo));
                let installer = recs[i].rc == 0;
                (updater != installer)
                    .then(|| format!("{slug:?}: install.sh {installer}, updater {updater}"))
            })
            .collect();
        assert!(wrong.is_empty(), "LC_ALL={locale}:\n{}", wrong.join("\n"));
    }
}

/// `release_asset_url` decides which names are installable at all: the signed
/// proofs, the two macOS containers and the signed Linux executables, each by
/// its tag-specific URL — and nothing else of the release.
#[test]
fn the_asset_allowlist_addresses_only_canonical_names() {
    let scratch = Scratch::new();
    let good = [
        "aterm-appcast.toml",
        "aterm-appcast.toml.sig",
        "aterm-machines.toml",
        "aterm-machines.toml.sig",
        "aterm-appcast-v0.2.0.toml",
        "aterm-appcast-v0.2.0.toml.sig",
        "aterm-0.2.0.dmg",
        "aterm-0.23.0-mac.zip",
        "aterm-1.10.3.dmg",
        "aterm-0.100.0-linux-x86_64",
        "aterm-0.100.0-linux-aarch64",
        "aterm-0.44.0-linux-x86_64.tar.gz",
        "aterm-0.44.0-linux-x86_64.tar.gz.sha256",
    ];
    let bad = [
        "aterm-0.2.0-mac.zip.evil",
        "aterm-evil-mac.zip",
        "../../etc/passwd",
        "aterm-.dmg",
        "aterm-0.2.0.zip",
        "aterm-0.100.0-linux-riscv64",
        "aterm-0.100.0-linux-aarch64.evil",
        "aterm-0.100.0-linux-x86_64.sha256",
        "aterm-0.44.0-linux-x86_64.tar.gz.sha256.evil",
        "aterm-linux-x86_64.tar.gz",
        "aterm-0.44.0-linux-aarch64.tar.gz",
        "aterm-0.44.0-linux-x86_64.tar",
        "aterm-0.2.0-arm64.dmg",
        "aterm-x86_64.dmg",
        "aterm-0.2.0-x86_64.dmg.evil",
        "aterm-0.2.0-x86_64.dmg",
        "aterm-0.47.0-x86_64.dmg",
        "aterm-0.2.0-lite.dmg",
        "aterm-offline.dmg",
        "aterm-appcast.toml.sig.evil",
        "aterm-machines.toml.bak",
        "aterm-appcast-v0.3.0.toml",
        "aterm-appcast-v0.2.0.toml.sig.evil",
    ];
    let mut lib = Lib::new();
    lib.step("REPO_SLUG=example/aterm");
    let good_calls: Vec<usize> = good
        .iter()
        .map(|name| lib.call(format!("release_asset_url v0.2.0 {}", q(name)), &[]))
        .collect();
    let bad_calls: Vec<usize> = bad
        .iter()
        .map(|name| lib.call(format!("release_asset_url v0.2.0 {}", q(name)), &[]))
        .collect();
    let malformed_tag = lib.call("release_asset_url v0.2.0.1 aterm-appcast.toml", &[]);
    let recs = lib.run(&scratch);
    for (name, i) in good.iter().zip(good_calls) {
        assert_eq!(
            (recs[i].rc, recs[i].out.as_str()),
            (
                0,
                format!("https://github.com/example/aterm/releases/download/v0.2.0/{name}")
                    .as_str()
            ),
            "{name}"
        );
    }
    for (name, i) in bad.iter().zip(bad_calls) {
        assert_eq!(
            (recs[i].rc, recs[i].out.as_str()),
            (2, ""),
            "{name} must be refused"
        );
    }
    assert_eq!(
        recs[malformed_tag].rc, 2,
        "an asset of a malformed tag is never addressed"
    );
}

/// The tag, the manifest's version and `aterm-<version>.dmg` must agree
/// exactly, under a three-component tag, with a 64-hex digest.
#[test]
fn manifest_identity_binds_tag_version_dmg_and_digest() {
    let scratch = Scratch::new();
    let sha = "a".repeat(64);
    let rows: &[(&str, &str, &str, &str, i32)] = &[
        ("v0.2.0", "0.2.0", "aterm-0.2.0.dmg", &sha, 0),
        ("v0.61", "0.61", "aterm-0.61.dmg", &sha, 2),
        ("v0.2.0", "0.2.1", "aterm-0.2.1.dmg", &sha, 2),
        ("v0.2.0", "0.2", "aterm-0.2.dmg", &sha, 2),
        ("v0.02.0", "0.02.0", "aterm-0.02.0.dmg", &sha, 2),
        ("v0.2.0", "0.2.0", "../aterm-0.2.0.dmg", &sha, 2),
        ("v0.2.0", "0.2.0", "another.dmg", &sha, 2),
        ("v0.2.0", "0.2.0", "aterm-0.2.0.dmg", "abc123", 2),
    ];
    let mut lib = Lib::new();
    let calls: Vec<usize> = rows
        .iter()
        .map(|(tag, version, dmg, digest, _)| {
            lib.call(
                format!(
                    "validate_manifest_identity {tag} {version} {} {digest}",
                    q(dmg)
                ),
                &[],
            )
        })
        .collect();
    let recs = lib.run(&scratch);
    for (row, i) in rows.iter().zip(calls) {
        assert_eq!(recs[i].rc, row.4, "{row:?}");
    }
}

/// Only the two supported Linux architectures are named, never guessed.
#[test]
fn linux_architectures_are_named_never_guessed() {
    let scratch = Scratch::new();
    let rows = [
        ("Linux x86_64", 0, "x86_64"),
        ("Linux aarch64", 0, "aarch64"),
        ("Linux arm64", 0, "aarch64"),
        ("Linux riscv64", 1, ""),
        ("Darwin arm64", 1, ""),
    ];
    let mut lib = Lib::new();
    let calls: Vec<usize> = rows
        .iter()
        .map(|(args, _, _)| lib.call(format!("linux_release_arch {args}"), &[]))
        .collect();
    let recs = lib.run(&scratch);
    for ((args, rc, arch), i) in rows.iter().zip(calls) {
        assert_eq!((recs[i].rc, recs[i].out.as_str()), (*rc, *arch), "{args}");
    }
}
