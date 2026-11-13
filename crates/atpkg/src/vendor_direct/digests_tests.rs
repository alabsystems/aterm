// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::*;

const T: &str = "aarch64-apple-darwin";

fn layout(label: &str) -> crate::testdir::ScratchLayout {
    crate::testdir::layout(&format!("atpkg-vendor-digests-{label}-"))
}

fn v(text: &str) -> Version {
    Version::parse(text).unwrap()
}

fn sha(c: char) -> String {
    c.to_string().repeat(64)
}

fn d256(hex: &str) -> AuthenticatedDigest {
    AuthenticatedDigest::authenticated(DigestAlg::Sha256, hex).unwrap()
}

fn d512(c: char) -> AuthenticatedDigest {
    AuthenticatedDigest::authenticated(DigestAlg::Sha512, &c.to_string().repeat(128)).unwrap()
}

fn rec(
    l: &Layout,
    program: &str,
    version: Version,
    triple: &str,
    sha256: &str,
) -> io::Result<DigestSeen> {
    record_or_conflict(l, program, version, triple, &d256(sha256), true)
}

fn rec512(l: &Layout, program: &str, version: &str, c: char) -> DigestSeen {
    record_or_conflict(l, program, v(version), T, &d512(c), true).unwrap()
}

#[test]
fn first_seen_wins_and_a_different_digest_conflicts() {
    let l = layout("first");
    let seen = |p, ver, t, s: &str| rec(&l, p, v(ver), t, s).unwrap();
    assert_eq!(seen("claude", "2.1.280", T, &sha('a')), DigestSeen::New);
    assert_eq!(seen("claude", "2.1.280", T, &sha('a')), DigestSeen::Same);
    assert_eq!(
        seen("claude", "2.1.280", T, &sha('A')),
        DigestSeen::Same,
        "hex case is not a different digest"
    );
    assert_eq!(
        seen("claude", "2.1.280", T, &sha('b')),
        DigestSeen::Conflict {
            first: d256(&sha('a'))
        }
    );
    assert_eq!(
        seen("claude", "2.1.280", T, &sha('b')),
        DigestSeen::Conflict {
            first: d256(&sha('a'))
        }
    );
    assert_eq!(
        seen("claude", "2.1.280", "x86_64-apple-darwin", &sha('b')),
        DigestSeen::New
    );
    assert_eq!(seen("claude", "2.1.281", T, &sha('b')), DigestSeen::New);
    assert_eq!(seen("codex", "2.1.280", T, &sha('b')), DigestSeen::New);
    let check =
        |ver, s: &str| record_or_conflict(&l, "claude", v(ver), T, &d256(s), false).unwrap();
    assert_eq!(check("2.1.290", &sha('a')), DigestSeen::New);
    assert_eq!(seen("claude", "2.1.290", T, &sha('b')), DigestSeen::New);
    assert_eq!(
        check("2.1.280", &sha('b')),
        DigestSeen::Conflict {
            first: d256(&sha('a'))
        }
    );
}

#[test]
fn malformed_arguments_are_refused_and_write_nothing() {
    let l = layout("args");
    for (p, t) in [
        ("trust", T),
        ("claude", "darwin"),
        ("claude", "../x-y"),
        ("claude", "Aarch64-apple-darwin"),
    ] {
        assert!(rec(&l, p, v("1.0.0"), t, &sha('a')).is_err(), "{p} {t}");
    }
    assert!(!vendor_dir(&l).exists());
}

#[test]
fn only_a_digest_of_its_algorithm_is_an_authenticated_digest() {
    const AGY_SHA512: &str = "473a34b685d0a09d48790e8b86bc3e545dfab889fe0d8fc81cb7e1fdfc336fe7\
                              8ce57022e3b438c754c93c9cffeb13fdbd177e50fdedc087d52732fa5475183f";
    for (alg, bad) in [
        (DigestAlg::Sha256, String::from("abc")),
        (DigestAlg::Sha256, sha('g')),
        (DigestAlg::Sha256, String::new()),
        (DigestAlg::Sha256, format!("{}0", sha('a'))),
        (DigestAlg::Sha256, AGY_SHA512.to_string()),
        (DigestAlg::Sha512, sha('a')),
        (DigestAlg::Sha512, format!("{AGY_SHA512}0")),
    ] {
        assert_eq!(
            AuthenticatedDigest::authenticated(alg, &bad),
            None,
            "{alg} {bad}"
        );
    }
    let d = d256(&sha('A'));
    assert_eq!(
        (d.alg(), d.as_str()),
        (DigestAlg::Sha256, sha('a').as_str()),
        "held lowercase"
    );
    let d =
        AuthenticatedDigest::authenticated(DigestAlg::Sha512, &AGY_SHA512.to_uppercase()).unwrap();
    assert_eq!((d.alg(), d.as_str()), (DigestAlg::Sha512, AGY_SHA512));
    assert_eq!(d.keyed(), (String::new(), AGY_SHA512.to_string()));
    assert_eq!(AuthenticatedDigest::recorded("", AGY_SHA512), Some(d));
    for (sha256, sha512) in [
        (sha('a'), AGY_SHA512.to_string()),
        (String::new(), String::new()),
        (sha('A'), String::new()),
        (String::new(), AGY_SHA512.to_uppercase()),
        (AGY_SHA512.to_string(), String::new()),
    ] {
        assert_eq!(
            AuthenticatedDigest::recorded(&sha256, &sha512),
            None,
            "{sha256} {sha512}"
        );
    }
}

#[test]
fn each_digest_is_logged_under_its_algorithm() {
    let l = layout("alg");
    let shared = digests_path(&l, "claude");
    std::fs::create_dir_all(shared.parent().unwrap()).unwrap();
    let earlier = format!(
        "[[seen]]\nprogram = \"claude\"\nversion = \"2.1.280\"\ntriple = \"{T}\"\nsha256 = \"{a}\"\n",
        a = sha('a')
    );
    std::fs::write(&shared, format!("schema = 1\n\n{earlier}")).unwrap();
    assert_eq!(rec512(&l, "agy", "1.3.6", '5'), DigestSeen::New);
    assert_eq!(
        rec(&l, "codex", v("0.157.0"), T, &sha('b')).unwrap(),
        DigestSeen::New
    );
    let text = std::fs::read_to_string(&shared).unwrap();
    assert!(
        text.contains(&earlier),
        "the earlier entry is rewritten as it was: {text}"
    );
    #[derive(Deserialize)]
    struct EarlierEntry {
        program: String,
        version: String,
        triple: String,
        sha256: String,
    }
    #[derive(Deserialize)]
    struct EarlierLog {
        seen: Vec<EarlierEntry>,
    }
    let earlier_reads: EarlierLog = aterm_toml::from_str(&text).unwrap();
    let rows: Vec<[String; 4]> = earlier_reads
        .seen
        .into_iter()
        .map(|e| [e.program, e.version, e.triple, e.sha256])
        .collect();
    assert_eq!(
        rows,
        [
            ["claude", "2.1.280", T, &sha('a')].map(String::from),
            ["codex", "0.157.0", T, &sha('b')].map(String::from),
        ],
        "{text}"
    );
    let agy = digests_path(&l, "agy");
    let text = std::fs::read_to_string(&agy).unwrap();
    assert!(
        text.contains(&format!("sha512 = \"{}\"\n", "5".repeat(128))) && !text.contains("sha256"),
        "{text}"
    );
    assert_eq!(rec512(&l, "agy", "1.3.6", '5'), DigestSeen::Same);
    assert_eq!(
        rec512(&l, "agy", "1.3.6", '6'),
        DigestSeen::Conflict { first: d512('5') }
    );
    assert_eq!(
        rec(&l, "agy", v("1.3.6"), T, &sha('5')).unwrap(),
        DigestSeen::New,
        "a sha256 is first seen apart from the sha512 seen for the same build"
    );
    assert_eq!(
        rec(&l, "agy", v("1.3.6"), T, &sha('6')).unwrap(),
        DigestSeen::Conflict {
            first: d256(&sha('5'))
        },
        "and then conflicts as any first sighting does"
    );
    assert_eq!(rec512(&l, "agy", "1.3.6", '5'), DigestSeen::Same);
    let both = format!(
        "[[seen]]\nprogram = \"agy\"\nversion = \"1.3.7\"\ntriple = \"{T}\"\n\
         sha256 = \"{a}\"\nsha512 = \"{b}\"\n\
         [[seen]]\nprogram = \"agy\"\nversion = \"1.3.8\"\ntriple = \"{T}\"\n",
        a = sha('a'),
        b = "b".repeat(128)
    );
    std::fs::write(&agy, format!("{text}{both}")).unwrap();
    assert_eq!(
        read_log(&agy).unwrap().len(),
        1,
        "the two-digest and no-digest entries dropped alone"
    );
    assert_eq!(
        rec(&l, "agy", v("1.3.7"), T, &sha('c')).unwrap(),
        DigestSeen::New
    );
}

#[test]
fn a_later_programs_sightings_outlive_an_older_clients_rewrite() {
    let l = layout("older");
    assert_eq!(rec512(&l, "agy", "1.3.6", '5'), DigestSeen::New);
    let muse = || v("1.4.9-R6297.1");
    assert_eq!(
        rec(&l, "muse", muse(), T, &sha('a')).unwrap(),
        DigestSeen::New
    );
    assert_eq!(
        rec(&l, "codex", v("0.157.0"), T, &sha('a')).unwrap(),
        DigestSeen::New
    );
    std::fs::write(
        digests_path(&l, "codex"),
        format!(
            "schema = 1\n[[seen]]\nprogram = \"codex\"\nversion = \"0.163.0\"\n\
             triple = \"{T}\"\nsha256 = \"{b}\"\n",
            b = sha('b')
        ),
    )
    .unwrap();
    assert_eq!(
        rec512(&l, "agy", "1.3.6", '6'),
        DigestSeen::Conflict { first: d512('5') }
    );
    assert_eq!(
        rec(&l, "muse", muse(), T, &sha('b')).unwrap(),
        DigestSeen::Conflict {
            first: d256(&sha('a'))
        }
    );
}

#[test]
fn the_log_keeps_the_newest_versions_per_program() {
    let l = layout("bound");
    let total = KEEP_PER_PROGRAM + 6;
    for patch in 0..total {
        let ver = Version::new(1, 0, u32::try_from(patch).unwrap()).unwrap();
        rec(&l, "codex", ver, T, &sha('a')).unwrap();
    }
    rec(&l, "claude", v("1.0.0"), T, &sha('c')).unwrap();
    let log = read_log(&digests_path(&l, "codex")).unwrap();
    let codex: Vec<Version> = log.keys().filter(|k| k.0 == "codex").map(|k| k.1).collect();
    assert_eq!(codex.len(), KEEP_PER_PROGRAM);
    assert_eq!(codex[0], v("1.0.6"), "the six lowest were dropped");
    assert!(
        log.keys().any(|k| k.0 == "claude"),
        "one program's bound never evicts another's"
    );
    assert_eq!(
        rec(&l, "codex", v("1.0.0"), T, &sha('b')).unwrap(),
        DigestSeen::New
    );
}

#[test]
fn an_unreadable_log_is_refused_and_a_bad_entry_drops_alone() {
    let l = layout("malformed");
    rec(&l, "claude", v("2.1.280"), T, &sha('a')).unwrap();
    let path = digests_path(&l, "claude");
    std::fs::write(&path, "not toml {{{").unwrap();
    assert!(read_log(&path).is_err());
    let err = rec(&l, "claude", v("2.1.280"), T, &sha('b')).unwrap_err();
    assert!(err.to_string().contains("does not read"), "{err}");
    assert_eq!(
        std::fs::read_to_string(&path).unwrap(),
        "not toml {{{",
        "the unreadable log is left for a person to look at"
    );
    std::fs::remove_file(&path).unwrap();
    assert_eq!(
        rec(&l, "claude", v("2.1.280"), T, &sha('b')).unwrap(),
        DigestSeen::New,
        "no log at all starts a new one"
    );
    std::fs::write(
        &path,
        format!(
            "schema = 1\n\
             [[seen]]\nprogram = \"claude\"\nversion = \"2.1.08\"\ntriple = \"{T}\"\nsha256 = \"{a}\"\n\
             [[seen]]\nprogram = \"claude\"\nversion = \"2.1.281\"\ntriple = \"{T}\"\nsha256 = \"{a}\"\n",
            a = sha('a')
        ),
    )
    .unwrap();
    let log = read_log(&path).unwrap();
    assert_eq!(log.len(), 1);
    assert_eq!(
        rec(&l, "claude", v("2.1.281"), T, &sha('b')).unwrap(),
        DigestSeen::Conflict {
            first: d256(&sha('a'))
        }
    );
    std::fs::write(&path, "schema = 9\n").unwrap();
    assert!(read_log(&path).is_err(), "an unknown schema is not read");
    assert!(
        rec(&l, "claude", v("2.1.282"), T, &sha('a')).is_err(),
        "…nor overwritten"
    );
}
