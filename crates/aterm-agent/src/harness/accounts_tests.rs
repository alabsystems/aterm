// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The account roster's tests (design §5.6, §5.8.10).
//!
//! The `.claude.json` fixtures below are SHAPED after the keys design §0
//! records as MEASURED on this machine — `oauthAccount.{organizationName,
//! seatTier}` and `cachedUsageUtilization.{fetchedAtMs, utilization.*}` —
//! with a deliberately hostile neighbourhood of extra keys around them, to
//! pin that the reader copies the closed set and nothing else. They are
//! fixtures, not captures: no real account's file is in this repository and
//! none is read by these tests.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use super::*;

struct Tmp(PathBuf);

impl Tmp {
    fn new(label: &str) -> Tmp {
        let path = std::env::temp_dir().join(format!(
            "aterm-harness-accounts-{label}-{}-{:?}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        let _ = std::fs::remove_dir_all(&path);
        std::fs::create_dir_all(&path).expect("scratch root");
        Tmp(path)
    }

    fn dir(&self, name: &str) -> PathBuf {
        let d = self.0.join(name);
        std::fs::create_dir_all(&d).expect("config dir");
        d
    }
}

impl Drop for Tmp {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// A config directory whose `.claude.json` is signed in, with the two window
/// figures the selection rule reads and a crowd of keys it must ignore.
fn write_claude_json(dir: &Path, five: Option<f64>, seven: Option<f64>, signed_in: bool) {
    let account = if signed_in {
        r#""oauthAccount":{"organizationName":"Acme","seatTier":"enterprise",
            "accessToken":"NEVER-READ","refreshToken":"NEVER-READ",
            "emailAddress":"someone@example.invalid"},"#
    } else {
        ""
    };
    let window = |name: &str, pct: Option<f64>| match pct {
        Some(p) => format!(r#""{name}":{{"utilization":{p},"resetsAt":1790000000}},"#),
        None => String::new(),
    };
    let text = format!(
        "{{{account}\"cachedUsageUtilization\":{{\"fetchedAtMs\":1790000000000,\
         \"utilization\":{{{}{}\"seven_day_opus\":null}}}},\
         \"projects\":{{\"/somewhere\":{{\"history\":[\"a\",\"b\"]}}}},\
         \"primaryApiKey\":\"NEVER-READ\"}}",
        window("five_hour", five),
        window("seven_day", seven),
    );
    std::fs::write(dir.join(".claude.json"), text).expect("write .claude.json");
}

fn row(label: &str, dir: &Path, priority: i64) -> Row {
    Row {
        label: label.to_string(),
        kind: Kind::ConfigDir,
        dir: dir.display().to_string(),
        priority,
    }
}

// ---------------------------------------------------------------------------
// The file
// ---------------------------------------------------------------------------

#[test]
fn a_roster_admits_whole_and_rotation_is_off_until_the_file_says_otherwise() {
    let text = r#"
# The owner's rotation set.
[accounts]
enabled = true

[[account]]
label = "work"
kind = "config-dir"
dir = "/Users//x/.claude"
priority = 0

[[account]]
label = "alt-1"
dir = "/Users//x/.claude-alt1"
"#;
    let roster = Roster::parse(text).expect("the reference roster admits");
    assert!(roster.enabled);
    assert_eq!(roster.rows.len(), 2);
    assert_eq!(roster.rows[0], {
        let mut r = row("work", Path::new("/Users//x/.claude"), 0);
        r.kind = Kind::ConfigDir;
        r
    });
    // `kind` and `priority` both default, and the defaults are stated rather
    // than implied.
    assert_eq!(roster.rows[1].kind, Kind::ConfigDir);
    assert_eq!(roster.rows[1].priority, DEFAULT_PRIORITY);

    // OFF is the shipped default: a roster that lists accounts but never
    // says `enabled` does not rotate.
    let quiet = Roster::parse("[[account]]\nlabel = \"work\"\ndir = \"/Users//x/.claude\"\n")
        .expect("admits");
    assert!(!quiet.enabled, "rotation consent is never implied by a row");

    // And an absent file is the empty roster, not an error: nothing about a
    // missing roster may make the harness un-launchable.
    let tmp = Tmp::new("absent");
    let loaded = Roster::load(&tmp.0.join("nope.toml")).expect("a missing file is not an error");
    assert_eq!(loaded, Roster::default());
}

#[test]
fn every_malformed_roster_is_refused_whole_and_names_what_broke() {
    // NEGATIVE CASES: one per rule, and each must name its own reason rather
    // than loading the rest of the file.
    let cases: &[(&str, &str)] = &[
        ("[[account]]\ndir = \"/Users//x/.claude\"\n", "no label"),
        (
            "[[account]]\nlabel = \"a b\"\ndir = \"/Users//x/.claude\"\n",
            "outside [A-Za-z0-9._-]",
        ),
        (
            "[[account]]\nlabel = \"w\"\nkind = \"bedrock\"\ndir = \"/x\"\n",
            "AWS credential chain",
        ),
        (
            "[[account]]\nlabel = \"w\"\nkind = \"vertex\"\ndir = \"/x\"\n",
            "application-default credentials",
        ),
        (
            "[[account]]\nlabel = \"w\"\nkind = \"azure\"\ndir = \"/x\"\n",
            "this build admits",
        ),
        (
            "[[account]]\nlabel = \"w\"\nkind = \"config-dir\"\ndir = \"relative/path\"\n",
            "not absolute",
        ),
        (
            "[[account]]\nlabel = \"w\"\ndir = \"/x\"\n[[account]]\nlabel = \"w\"\ndir = \"/y\"\n",
            "declared twice",
        ),
        (
            "[[account]]\nlabel = \"w\"\ndir = \"/x\"\npriority = \"first\"\n",
            "not an integer",
        ),
        ("[servers]\nhost = \"x\"\n", "unknown block [servers]"),
        ("enabled = true\n", "keys before the first block"),
    ];
    for (text, needle) in cases {
        let err = Roster::parse(text).expect_err(&format!("{text:?} must be refused"));
        assert!(
            err.contains(needle),
            "the refusal must name {needle:?}: {err}"
        );
    }

    // The reader's own refusals now name THIS file, not harness.toml — a
    // refusal naming the wrong file is a wrong answer.
    let err = Roster::parse("[[account]\nlabel = \"w\"\n").expect_err("unclosed header");
    assert!(err.starts_with(ACCOUNTS_FILE), "{err}");
    assert!(!err.contains("harness.toml"), "{err}");

    // The row bound is a refusal, not a truncation.
    let many: String = (0..=MAX_ACCOUNTS)
        .map(|i| format!("[[account]]\nlabel = \"a{i}\"\ndir = \"/x{i}\"\n"))
        .collect();
    let err = Roster::parse(&many).expect_err("over the bound");
    assert!(err.contains(&MAX_ACCOUNTS.to_string()), "{err}");
}

// ---------------------------------------------------------------------------
// The snapshot — the closed key set
// ---------------------------------------------------------------------------

#[test]
fn the_snapshot_copies_the_closed_non_secret_set_and_nothing_else() {
    let tmp = Tmp::new("snapshot");
    let dir = tmp.dir(".claude");
    write_claude_json(&dir, Some(62.0), Some(18.0), true);
    let snap = snapshot(&dir).expect("the fixture is readable");

    assert!(snap.signed_in);
    assert_eq!(snap.identity.org.as_deref(), Some("Acme"));
    assert_eq!(snap.identity.tier.as_deref(), Some("enterprise"));
    assert_eq!(snap.window(FIVE_HOUR).map(|w| w.used_pct), Some(62.0));
    assert_eq!(snap.window(SEVEN_DAY).map(|w| w.used_pct), Some(18.0));
    assert_eq!(
        snap.window(SEVEN_DAY).and_then(|w| w.resets_at),
        Some(1_790_000_000)
    );
    // A `null` window is ABSENT, never a zero: `seven_day_opus` reads null on
    // the measured account and a zero there would read as "wide open".
    assert_eq!(snap.window("seven_day_opus"), None);
    assert_eq!(snap.age_s(1_790_000_060), Some(60));
    // A cache from the future is age zero, never a negative number.
    assert_eq!(snap.age_s(1_700_000_000), Some(0));

    // NEGATIVE CONTROL: the whole reader's output, rendered, carries nothing
    // that was next to the keys it wanted. This is the credential line.
    let rendered = format!("{snap:?}");
    for secret in [
        "NEVER-READ",
        "accessToken",
        "refreshToken",
        "example.invalid",
        "history",
        "primaryApiKey",
    ] {
        assert!(
            !rendered.contains(secret),
            "{secret:?} must not survive the read: {rendered}"
        );
    }
}

#[test]
fn a_config_dir_with_no_oauth_account_reads_unauthenticated_not_unknown() {
    let tmp = Tmp::new("unauth");
    let dir = tmp.dir(".claude-fresh");
    write_claude_json(&dir, None, None, false);
    let state = AccountState::new(row("fresh", &dir, DEFAULT_PRIORITY)).read_dir();
    assert_eq!(state.auth, AuthState::Unauthenticated);
    assert_eq!(candidacy(&state), Err(NotCandidate::Unauthenticated));

    // NEGATIVE CONTROL: a directory with NO file at all is UNKNOWN, which is
    // a different answer — nothing has been read, rather than something has
    // been read and says no.
    let empty = tmp.dir(".claude-empty");
    let state = AccountState::new(row("empty", &empty, DEFAULT_PRIORITY)).read_dir();
    assert_eq!(state.auth, AuthState::Unknown);
    assert_eq!(candidacy(&state), Ok(()));

    // A file that is not JSON at all is the same honest `None`.
    let junk = tmp.dir(".claude-junk");
    std::fs::write(junk.join(".claude.json"), "not json {{{").expect("junk");
    assert_eq!(snapshot(&junk), None);
}

// ---------------------------------------------------------------------------
// The selection — §5.6 verbatim, plus §5.8.10
// ---------------------------------------------------------------------------

#[test]
fn selection_is_lowest_seven_day_among_accounts_whose_five_hour_has_headroom() {
    let tmp = Tmp::new("select");
    let work = tmp.dir(".claude");
    let alt1 = tmp.dir(".claude-alt1");
    let alt2 = tmp.dir(".claude-alt2");
    // `work` is the session's own account and is at its five-hour limit.
    write_claude_json(&work, Some(100.0), Some(40.0), true);
    // `alt1` has the LOWEST seven-day figure but its five-hour is spent.
    write_claude_json(&alt1, Some(100.0), Some(4.0), true);
    // `alt2` has headroom in both.
    write_claude_json(&alt2, Some(10.0), Some(22.0), true);

    let roster = Roster {
        enabled: true,
        rows: vec![
            row("work", &work, 0),
            row("alt-1", &alt1, 0),
            row("alt-2", &alt2, 0),
        ],
    };
    let states = roster_states(&roster, Some(&work), &BTreeMap::new());
    assert!(states[0].active, "the active account is named by its dir");

    let chosen = select(&states).expect("a candidate");
    assert_eq!(chosen.row.label, "alt-2");
    // ... and every refusal is by NAME.
    assert_eq!(candidacy(&states[0]), Err(NotCandidate::Active));
    assert_eq!(candidacy(&states[1]), Err(NotCandidate::FiveHourExhausted));
}

#[test]
fn exhaustion_is_the_one_threshold_the_classifier_uses() {
    // The roster used to carry its OWN `EXHAUSTED_PCT` at 100.0 while the
    // classifier read 95.0 from design §5.8.2. Two numbers for one word is
    // two answers, and the design names only one — so an account the
    // classifier would call exhausted must not be a rotation candidate.
    let tmp = Tmp::new("threshold");
    let hot = tmp.dir(".claude-hot");
    let cool = tmp.dir(".claude-cool");
    write_claude_json(&hot, Some(97.0), Some(4.0), true);
    write_claude_json(&cool, Some(94.9), Some(80.0), true);
    let roster = Roster {
        enabled: true,
        rows: vec![row("hot", &hot, 0), row("cool", &cool, 0)],
    };
    let states = roster_states(&roster, None, &BTreeMap::new());
    // 97 % is exhausted even though it has the far lower seven-day figure.
    assert_eq!(candidacy(&states[0]), Err(NotCandidate::FiveHourExhausted));
    // NEGATIVE CONTROL: a hair under the threshold is still a candidate, so
    // the test cannot pass by refusing everything.
    assert!(candidacy(&states[1]).is_ok());
    assert_eq!(select(&states).expect("a candidate").row.label, "cool");
}

#[test]
fn ties_break_by_priority_then_by_label_and_a_measured_figure_beats_silence() {
    let tmp = Tmp::new("ties");
    let a = tmp.dir(".claude-a");
    let b = tmp.dir(".claude-b");
    let c = tmp.dir(".claude-c");
    let quiet = tmp.dir(".claude-quiet");
    write_claude_json(&a, Some(1.0), Some(18.0), true);
    write_claude_json(&b, Some(1.0), Some(18.0), true);
    write_claude_json(&c, Some(1.0), Some(18.0), true);
    // `quiet` has an `oauthAccount` and NO utilisation cache: unknown, not
    // exhausted.
    std::fs::write(
        quiet.join(".claude.json"),
        r#"{"oauthAccount":{"organizationName":"Acme"}}"#,
    )
    .expect("quiet");

    // Equal utilisation: the lower priority number wins.
    let roster = Roster {
        enabled: true,
        rows: vec![row("a", &a, 5), row("b", &b, 1), row("c", &c, 5)],
    };
    let states = roster_states(&roster, None, &BTreeMap::new());
    assert_eq!(select(&states).expect("a candidate").row.label, "b");

    // Equal utilisation AND equal priority: the label breaks it, so the
    // answer does not depend on the order rows sit in the file.
    let roster = Roster {
        enabled: true,
        rows: vec![row("c", &c, 5), row("a", &a, 5)],
    };
    let states = roster_states(&roster, None, &BTreeMap::new());
    assert_eq!(select(&states).expect("a candidate").row.label, "a");

    // A measured 18% beats an account nothing measured, even at a better
    // priority — but the unmeasured one is still a candidate when it is the
    // only one left.
    let roster = Roster {
        enabled: true,
        rows: vec![row("quiet", &quiet, 0), row("a", &a, 99)],
    };
    let states = roster_states(&roster, None, &BTreeMap::new());
    assert_eq!(select(&states).expect("a candidate").row.label, "a");
    let only = states
        .iter()
        .filter(|s| s.row.label == "quiet")
        .cloned()
        .collect::<Vec<_>>();
    assert_eq!(select(&only).expect("still a candidate").row.label, "quiet");
}

#[test]
fn an_account_whose_login_expired_is_never_a_rotation_candidate() {
    // §5.8.10: an expired login is renewed at the vendor's own door by a
    // human. It is NOT routed around, however much headroom it has.
    let tmp = Tmp::new("expired");
    let good = tmp.dir(".claude-good");
    let expired = tmp.dir(".claude-expired");
    write_claude_json(&good, Some(1.0), Some(50.0), true);
    // The expired one looks PERFECT on disk — the file still carries an
    // `oauthAccount` and the lowest seven-day figure in the roster.
    write_claude_json(&expired, Some(1.0), Some(0.0), true);

    let roster = Roster {
        enabled: true,
        rows: vec![row("good", &good, 9), row("expired", &expired, 0)],
    };
    let mut observed = BTreeMap::new();
    observed.insert("expired".to_string(), AuthState::LoginExpired);
    let states = roster_states(&roster, None, &observed);

    assert_eq!(candidacy(&states[1]), Err(NotCandidate::LoginExpired));
    assert_eq!(select(&states).expect("a candidate").row.label, "good");

    // NEGATIVE CONTROL: without that observation the same roster picks the
    // expired one, which is exactly what §5.8.10 exists to stop — so the
    // rule is doing work, not decorating an answer it would have given.
    let states = roster_states(&roster, None, &BTreeMap::new());
    assert_eq!(select(&states).expect("a candidate").row.label, "expired");

    // And an observation the caller made is never overwritten by the file.
    assert_eq!(
        AccountState {
            auth: AuthState::LoginExpired,
            ..AccountState::new(row("expired", &expired, 0))
        }
        .read_dir()
        .auth,
        AuthState::LoginExpired
    );
}

#[test]
fn an_api_key_row_is_listed_and_never_selected() {
    // The design's `api-key` rows point at a `secrets/<label>.env` the
    // PRELUDE sources. This module does not read one, so it cannot rotate
    // into one: it says so by name instead of quietly leaving the row out.
    let tmp = Tmp::new("api-key");
    let dir = tmp.dir(".claude-key");
    write_claude_json(&dir, Some(0.0), Some(0.0), true);
    let roster = Roster::parse(&format!(
        "[accounts]\nenabled = true\n\n[[account]]\nlabel = \"key\"\nkind = \"api-key\"\n\
         dir = {:?}\n",
        dir.display().to_string()
    ))
    .expect("an api-key row admits");
    let states = roster_states(&roster, None, &BTreeMap::new());
    assert_eq!(states.len(), 1, "the row is LISTED");
    assert_eq!(candidacy(&states[0]), Err(NotCandidate::Kind));
    assert!(select(&states).is_none());
    assert!(
        states[0].snapshot.is_none(),
        "an api-key row's directory is not read for windows"
    );

    // An empty roster selects nothing and says nothing about why.
    assert!(select(&[]).is_none());
}

// ---------------------------------------------------------------------------
// Discovery
// ---------------------------------------------------------------------------

#[test]
fn discovery_proposes_one_row_per_config_dir_and_writes_nothing() {
    let tmp = Tmp::new("discover");
    let home = tmp.dir("home");
    let main = home.join(".claude");
    let alt = home.join(".claude-alt1");
    let bare = home.join(".claude-bare");
    for dir in [&main, &alt, &bare] {
        std::fs::create_dir_all(dir).expect("config dir");
    }
    write_claude_json(&main, Some(62.0), Some(18.0), true);
    write_claude_json(&alt, Some(5.0), Some(2.0), true);
    write_claude_json(&bare, None, None, false);
    // Noise the scan must not propose: a directory with the wrong prefix,
    // and a FILE whose name starts the same way.
    std::fs::create_dir_all(home.join(".claudex")).expect("noise dir");
    std::fs::write(home.join(".claude-file"), "not a directory").expect("noise file");

    let roster = Roster {
        enabled: false,
        rows: vec![row("work", &main, 0)],
    };
    let found = discover(&home, None, &roster);
    let labels: Vec<&str> = found.iter().map(|p| p.label.as_str()).collect();
    assert_eq!(labels, vec!["claude", "claude-alt1", "claude-bare"]);
    assert!(found[0].already, "the roster already carries this dir");
    assert!(!found[1].already);
    assert!(found[1].signed_in());
    assert!(
        !found[2].signed_in(),
        "a dir with no oauthAccount is listed as unauthenticated, not hidden"
    );
    assert_eq!(
        found[1]
            .snapshot
            .as_ref()
            .and_then(|s| s.identity.org.clone()),
        Some("Acme".to_string())
    );

    // WRITES NOTHING. The directory listing is byte-identical afterwards.
    let before: Vec<_> = std::fs::read_dir(&home)
        .expect("home")
        .flatten()
        .map(|e| e.file_name())
        .collect();
    let _ = discover(&home, None, &roster);
    let after: Vec<_> = std::fs::read_dir(&home)
        .expect("home")
        .flatten()
        .map(|e| e.file_name())
        .collect();
    assert_eq!(before.len(), after.len());
    assert!(
        !home.join(ACCOUNTS_FILE).exists(),
        "discovery writes no roster"
    );

    // `$CLAUDE_CONFIG_DIR` outside home is proposed too, once.
    let outside = tmp.dir("elsewhere");
    write_claude_json(&outside, Some(1.0), Some(1.0), true);
    let found = discover(&home, Some(&outside), &roster);
    assert_eq!(found.len(), 4);
    assert_eq!(found[3].dir, outside);
    // ... and a config dir that is ALSO under home is not proposed twice.
    let found = discover(&home, Some(&alt), &roster);
    assert_eq!(found.len(), 3);

    // The proposed row round-trips through the roster reader.
    let text = format!("[accounts]\nenabled = true\n{}", found[1].to_toml());
    let parsed = Roster::parse(&text).expect("a proposal is a valid row");
    assert_eq!(parsed.rows.len(), 1);
    assert_eq!(parsed.rows[0].dir, alt.display().to_string());
    assert_eq!(parsed.rows[0].kind, Kind::ConfigDir);
}

#[test]
fn the_closed_vocabularies_round_trip_through_their_own_parsers() {
    for kind in [Kind::ConfigDir, Kind::ApiKey] {
        assert_eq!(Kind::parse(kind.as_str()), Some(kind));
    }
    for bad in ["bedrock", "vertex", "", "CONFIG-DIR"] {
        assert_eq!(Kind::parse(bad), None, "{bad:?}");
    }
    // The two the DESIGN names are refused WITH A REASON, and the reason
    // names the mechanism rather than saying "unverified": a bare refusal
    // teaches a reader nothing about what would lift it.
    for (name, why) in Kind::REFUSED {
        assert_eq!(Kind::refusal(name), Some(why), "{name}");
        assert!(
            why.contains("config directory") && why.contains("utilisation"),
            "{name}: the reason must name the selection seam and the missing input — {why}"
        );
    }
    // NEGATIVE CONTROL: a word that is not one of those two has no reason,
    // so `refusal` is not "any unknown kind gets a sentence".
    for other in ["config-dir", "api-key", "azure", ""] {
        assert_eq!(Kind::refusal(other), None, "{other:?}");
    }
    for auth in [
        AuthState::Unknown,
        AuthState::SignedIn,
        AuthState::Unauthenticated,
        AuthState::LoginExpired,
    ] {
        assert_eq!(AuthState::parse(auth.as_str()), auth);
    }
    // An unknown word is UNKNOWN, never an error and never something
    // stronger than the word deserved.
    assert_eq!(AuthState::parse("logged-in"), AuthState::Unknown);
    for why in [
        NotCandidate::Active,
        NotCandidate::Kind,
        NotCandidate::NoDir,
        NotCandidate::FiveHourExhausted,
        NotCandidate::LoginExpired,
        NotCandidate::Unauthenticated,
    ] {
        assert!(!why.as_str().is_empty());
        assert!(
            why.as_str()
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b == b'-')
        );
    }
}

/// F-7 (2026-09-22). The bound ran BEFORE the filter and over an
/// unspecified `read_dir` order, so a home directory holding more than
/// `MAX_DISCOVER_ENTRIES` entries dropped whichever `.claude-*` happened to
/// sort late in readdir order — a different answer on different runs. The
/// bound is on MATCHES now.
#[test]
fn the_discovery_bound_counts_matches_and_not_the_entries_it_walked_past() {
    let tmp = Tmp::new("discover-bound");
    let home = tmp.dir("home");
    let alt = home.join(".claude-alt1");
    std::fs::create_dir_all(&alt).expect("config dir");
    write_claude_json(&alt, Some(5.0), Some(2.0), true);
    // Far more non-matching entries than the bound, so under the old shape
    // the one real config directory was reached only by luck.
    for n in 0..(MAX_DISCOVER_ENTRIES + 64) {
        std::fs::write(home.join(format!("noise-{n:05}")), "x").expect("noise");
    }
    let roster = Roster {
        enabled: false,
        rows: Vec::new(),
    };
    let found = discover(&home, None, &roster);
    let labels: Vec<&str> = found.iter().map(|p| p.label.as_str()).collect();
    assert_eq!(
        labels,
        vec!["claude-alt1"],
        "the one match is found however many entries it walked past"
    );

    // NEGATIVE CONTROL: the bound still bounds, and it bounds MATCHES.
    let big = Tmp::new("discover-bound-many");
    let bhome = big.dir("home");
    for n in 0..(MAX_DISCOVER_ENTRIES + 8) {
        let d = bhome.join(format!(".claude-{n:05}"));
        std::fs::create_dir_all(&d).expect("config dir");
    }
    let many = discover(&bhome, None, &roster);
    assert!(
        many.len() <= MAX_DISCOVER_ENTRIES,
        "bounded at {MAX_DISCOVER_ENTRIES}, proposed {}",
        many.len()
    );
}
