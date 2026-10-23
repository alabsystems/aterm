// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
// Author: Andrew Yates

//! agh.toml's rules (design §6.2–§6.3).

use std::path::Path;

use agh::config::{self, ConfigFault};
use agh::identity::{Credential, Source};
use agh::policy::{ChainStep, Tier};

use crate::common;

/// Two profiles with logins and token files under `{root}`: the rows every fixture adds to.
const BASE: &str = r#"
[profiles.dev]
token_file = "{root}/tokens/dev.token"
tier = "dev"
login = "fixture-dev-bot"

[profiles.release]
token_file = "{root}/secrets/release.token"
tier = "release"
login = "fixture-release-bot"
"#;

/// `text` with `{root}` filled, escaped for a TOML basic string as `common` escapes it.
fn fill(text: &str, root: &Path) -> String {
    let root = root
        .display()
        .to_string()
        .replace('\\', "\\\\")
        .replace('"', "\\\"");
    text.replace("{root}", &root)
}

/// The faults of `BASE` + `extra` under `root`, with `file_id` judging files.
fn faults_with(
    extra: &str,
    root: &Path,
    file_id: &dyn Fn(&Path) -> Option<(u64, u64)>,
) -> Vec<ConfigFault> {
    let text = fill(&format!("{BASE}\n{extra}"), root);
    let parsed = match config::parse(&text) {
        Ok(parsed) => parsed,
        Err(fault) => return vec![fault],
    };
    match config::validate(&parsed, root, file_id) {
        Ok(_) => Vec::new(),
        Err(faults) => faults,
    }
}

/// The faults of `BASE` + `extra` in a fresh root.
fn faults(extra: &str) -> Vec<ConfigFault> {
    let root = common::scratch("rules");
    faults_with(extra, root.path(), &config::file_id)
}

fn rules(faults: &[ConfigFault]) -> Vec<&'static str> {
    faults.iter().map(|f| f.rule).collect()
}

/// One fault, of `rule`, whose text carries every one of `names`.
fn assert_one(extra: &str, rule: &str, names: &[&str]) {
    let found = faults(extra);
    assert_eq!(rules(&found), [rule], "{extra}: {found:?}");
    for name in names {
        assert!(
            found[0].what.contains(name),
            "{name} in {:?}",
            found[0].what
        );
    }
}

#[test]
fn the_canonical_config_validates() {
    let root = common::scratch("canonical");
    let text = common::canonical_config(root.path());
    let parsed = config::parse(&text).expect("parses");
    let validated = config::validate(&parsed, root.path(), &config::file_id).expect("validates");
    let names: Vec<&String> = validated.profiles().keys().collect();
    assert_eq!(names, ["dev", "release"]);
    assert_eq!(
        validated.profiles()["dev"].source,
        Source::File(root.path().join("tokens").join("dev.token"))
    );
    assert_eq!(validated.profiles()["release"].tier, Tier::Release);
    let owners: Vec<&String> = validated.owners().keys().collect();
    assert_eq!(owners, ["alabsystems", "fixture-dev"]);
    assert_eq!(
        validated.owners()["alabsystems"].write.as_deref(),
        Some("release")
    );
    assert_eq!(
        validated.owners()["alabsystems"].read,
        [ChainStep::Profile("dev".into()), ChainStep::Anonymous]
    );
    assert_eq!(
        validated.default_read(),
        [ChainStep::Profile("dev".into()), ChainStep::Anonymous]
    );
    assert_eq!(validated.targets()["channel"].key(), "alabsystems/aterm");
    assert_eq!(faults(""), Vec::new());
}

#[test]
fn l1_the_toml_and_its_schema() {
    for extra in [
        "[profiles.x\n",
        "[profiles.x]\ntoken_file = \"{root}/x.token\"\ntier = \"dev\"\nlegacy_loose = true\n",
        "[[writers]]\ncaller = \"atpkg-publish\"\nrepos = [\"alabsystems/aterm\"]\n",
    ] {
        let found = faults(extra);
        assert_eq!(rules(&found), ["L1"], "{extra}: {found:?}");
    }
    let unknown = faults("[profiles.x]\ntier = \"dev\"\nlegacy_loose = true\n");
    assert!(unknown[0].what.contains("legacy_loose"), "{unknown:?}");
}

/// A token pasted into agh.toml, each case to add to `BASE`. The first three fail the parse
/// (an unknown key's value, a value of the wrong type, unquoted), whose L1 fault names the
/// line and the parser's message. The rest are well typed, so `validate` judges them and
/// its fault names the row: an owner's `write`, `read`, `tier` and name, `[default] read`,
/// a `[targets]` value and a profile's name. No fault carries the token.
fn pasted(token: &str) -> [String; 10] {
    let owner = "[[owners]]\nname = \"fixture-ext\"\ntier = \"external\"\n";
    [
        format!("[profiles.x]\ntier = \"dev\"\ntoken = \"{token}\"\n"),
        format!("{owner}read = \"{token}\"\n"),
        format!("[profiles.x]\ntier = \"dev\"\ntoken_file = {token}\n"),
        format!("{owner}write = \"{token}\"\n"),
        format!("{owner}read = [\"{token}\", \"anonymous\"]\n"),
        format!("[[owners]]\nname = \"fixture-ext\"\ntier = \"{token}\"\n"),
        format!("[[owners]]\nname = \"{token}\"\ntier = \"external\"\nread = [\"ghost\"]\n"),
        format!("[default]\nread = [\"{token}\"]\n"),
        format!("[targets]\nt = \"{token}\"\n"),
        format!("[profiles.{token}]\ntier = \"dev\"\nenv = \"X_TOKEN\"\n"),
    ]
}

/// Neither the token nor its body after the prefix is in `text`.
fn assert_unleaked(text: &str, token: &str) {
    assert!(!text.contains(token), "the token in {text:?}");
    assert!(!text.contains(&token[4..]), "the token's body in {text:?}");
}

#[test]
fn l1_never_quotes_a_pasted_token() {
    let token = agh_fake::token("leak");
    for pasted_value in [token.as_str(), &token[4..]] {
        for (i, extra) in pasted(pasted_value).iter().enumerate() {
            let found = faults(extra);
            if i < 3 {
                assert_eq!(rules(&found), ["L1"], "{found:?}");
            } else {
                assert!(!found.is_empty(), "{extra}");
            }
            for fault in &found {
                assert_unleaked(&fault.what, pasted_value);
            }
        }
    }
    let unknown = faults("[profiles.x]\ntier = \"dev\"\nlegacy_loose = true\n");
    assert!(unknown[0].what.contains("`legacy_loose`"), "{unknown:?}");
}

#[test]
fn l1_env_must_name_a_variable() {
    let legacy = agh_fake::token("env")[4..].to_string();
    let long = "A".repeat(65);
    for value in [legacy.as_str(), "GH TOKEN", "", "1X", long.as_str()] {
        let extra = format!("[profiles.x]\ntier = \"dev\"\nenv = \"{value}\"\n");
        let found = faults(&extra);
        assert_eq!(rules(&found), ["L1"], "{value:?}: {found:?}");
        assert!(found[0].what.contains("profile x"), "{found:?}");
        assert!(found[0].what.contains("env"), "{found:?}");
        if !value.is_empty() {
            assert!(!found[0].what.contains(value), "{found:?}");
        }
    }
    for value in ["GH_TOKEN", "_X", "x_token_2"] {
        let extra = format!("[profiles.x]\ntier = \"dev\"\nenv = \"{value}\"\n");
        assert_eq!(faults(&extra), Vec::new(), "{value:?}");
    }
}

#[test]
fn l1_puts_end_of_input_on_the_last_line() {
    let found = faults("[profiles.x]\ntier = \"dev\"\nlogin = [\n");
    assert_eq!(rules(&found), ["L1"], "{found:?}");
    let lines = 1 + format!("{BASE}\n[profiles.x]\ntier = \"dev\"\nlogin = [\n")
        .trim_end_matches('\n')
        .matches('\n')
        .count();
    assert!(
        found[0].what.contains(&format!("line {lines}: ")),
        "line {lines} in {:?}",
        found[0].what
    );
}

#[test]
fn l1_references_and_values() {
    assert_one(
        "[profiles.x]\ntoken_file = \"{root}/x.token\"\ntier = \"prod\"\n",
        "L1",
        &["profile x", "prod"],
    );
    assert_one(
        "[[owners]]\nname = \"fixture-ext\"\ntier = \"prod\"\n",
        "L1",
        &["owner fixture-ext", "prod"],
    );
    assert_one(
        "[profiles.x]\ntoken_file = \"{root}/x.token\"\nenv = \"X_TOKEN\"\ntier = \"dev\"\n",
        "L1",
        &["profile x", "token_file", "env"],
    );
    assert_one(
        "[profiles.x]\ntier = \"dev\"\n",
        "L1",
        &["profile x", "token_file", "env"],
    );
    assert_one(
        "[profiles.x]\ntoken_file = \"tokens/x.token\"\ntier = \"dev\"\n",
        "L1",
        &["profile x", "token_file", "~/"],
    );
    assert_one(
        "[profiles.x]\ntoken_file = \"{root}/x.token\"\ntier = \"dev\"\ndrafts_fp = \"ABCDEF012345\"\n",
        "L1",
        &["profile x", "drafts_fp"],
    );
    assert_one(
        "[[owners]]\nname = \"fixture-ext\"\ntier = \"external\"\nread = [\"ghost\"]\n",
        "L1",
        &["owner fixture-ext", "read", "ghost"],
    );
    assert_one(
        "[[owners]]\nname = \"fixture-ext\"\ntier = \"external\"\nwrite = \"ghost\"\n",
        "L1",
        &["owner fixture-ext", "write", "ghost"],
    );
    assert_one(
        "[[owners]]\nname = \"fixture-dev\"\ntier = \"dev\"\nread = [\"dev\", \"dev\"]\n",
        "L1",
        &["fixture-dev", "dev"],
    );
    assert_one(
        "[default]\nread = [\"ghost\", \"anonymous\"]\n",
        "L1",
        &["[default]", "ghost"],
    );
}

#[test]
fn l2_the_release_floor() {
    assert_one(
        "[[owners]]\nname = \"ALABSYSTEMS\"\ntier = \"dev\"\nread = [\"dev\"]\n",
        "L2",
        &["owner ALABSYSTEMS"],
    );
    let found = faults(
        "[[owners]]\nname = \"alabsystems\"\ntier = \"release\"\nread = [\"dev\"]\nwrite = \"dev\"\n",
    );
    assert_eq!(rules(&found), ["L2", "L6"], "{found:?}");
    assert!(found[0].what.contains("owner alabsystems"), "{found:?}");
    assert!(found[0].what.contains("profile dev"), "{found:?}");
}

#[test]
fn l3_owner_names_and_rows() {
    assert_one(
        "[[owners]]\nname = \"-bad\"\ntier = \"external\"\n",
        "L3",
        &["-bad"],
    );
    assert_one(
        "[[owners]]\nname = \"fixture-dev\"\ntier = \"dev\"\nread = [\"dev\"]\n\n[[owners]]\nname = \"FIXTURE-DEV\"\ntier = \"dev\"\n",
        "L3",
        &["owner FIXTURE-DEV"],
    );
}

/// A name of 20 or more characters with no `-` that passes its own rule (L3's 39 for an
/// owner, L10's 32 for a profile or target) is carried whole, not masked as a token.
#[test]
fn a_long_name_is_carried_whole() {
    let owner = "fixtureextwithalongname";
    assert_one(
        &format!("[[owners]]\nname = \"{owner}\"\ntier = \"prod\"\n"),
        "L1",
        &[owner],
    );
    let profile = "fixtureprofilewithalongname";
    assert_one(
        &format!("[profiles.{profile}]\nenv = \"X_TOKEN\"\ntier = \"prod\"\n"),
        "L1",
        &[profile],
    );
    let target = "fixturetargetwithalongname";
    assert_one(
        &format!("[targets]\n{target} = \"nope\"\n"),
        "L9",
        &[target],
    );
}

#[test]
fn l4_one_writer() {
    assert_one(
        "[[owners]]\nname = \"fixture-dev\"\ntier = \"dev\"\nwrite = [\"dev\", \"dev\"]\n",
        "L4",
        &["owner fixture-dev", "2"],
    );
    assert_one(
        "[default]\nread = [\"anonymous\"]\nwrite = \"dev\"\n",
        "L4",
        &["[default]"],
    );
}

#[test]
fn l5_release_reads_a_file() {
    assert_one(
        "[profiles.x]\nenv = \"X_TOKEN\"\ntier = \"release\"\nlogin = \"fixture-reader\"\n",
        "L5",
        &["profile x"],
    );
}

#[test]
fn l6_the_tier_match() {
    let found = faults("[[owners]]\nname = \"fixture-dev\"\ntier = \"dev\"\nwrite = \"release\"\n");
    assert_eq!(rules(&found), ["L6", "L6"], "{found:?}");
    for fault in &found {
        for name in ["fixture-dev", "release"] {
            assert!(fault.what.contains(name), "{name} in {found:?}");
        }
    }
    let found =
        faults("[[owners]]\nname = \"fixture-ext\"\ntier = \"prod\"\nwrite = \"release\"\n");
    assert_eq!(rules(&found), ["L1", "L6"], "{found:?}");
    for name in ["fixture-ext", "write", "release"] {
        assert!(found[1].what.contains(name), "{name} in {found:?}");
    }
    assert_one(
        "[[owners]]\nname = \"fixture-dev\"\ntier = \"dev\"\nread = [\"release\"]\n",
        "L6",
        &["owner fixture-dev", "read", "profile release"],
    );
    assert_one(
        "[[owners]]\nname = \"fixture-ext\"\ntier = \"external\"\nwrite = \"dev\"\n",
        "L6",
        &["owner fixture-ext", "external", "profile dev"],
    );
    assert_one(
        "[default]\nread = [\"release\", \"anonymous\"]\n",
        "L6",
        &["[default]", "profile release"],
    );
}

#[test]
fn l7_disjoint_files_and_logins() {
    let root = common::scratch("rules-id");
    let same = |_: &Path| Some((1, 1));
    let found = faults_with("", root.path(), &same);
    assert_eq!(rules(&found), ["L7"], "{found:?}");
    for name in ["dev", "release"] {
        assert!(found[0].what.contains(name), "{name} in {found:?}");
    }
    assert_one(
        "[profiles.y]\ntoken_file = \"{root}/y.token\"\ntier = \"dev\"\nlogin = \"Fixture-Release-Bot\"\n",
        "L7",
        &["y", "dev", "release", "Fixture-Release-Bot"],
    );
}

/// L7 by canonical path: a second profile reaches the dev token file through a directory
/// symlink.
#[cfg(unix)]
#[test]
fn l7_the_same_file_through_a_directory_symlink() {
    let root = common::scratch("rules-link");
    common::write_token(
        &root.path().join("tokens"),
        "dev.token",
        &agh_fake::token("l7"),
    );
    std::os::unix::fs::symlink(root.path().join("tokens"), root.path().join("link"))
        .expect("a directory symlink");
    let extra = "[profiles.y]\ntoken_file = \"{root}/link/dev.token\"\ntier = \"dev\"\n";
    let found = faults_with(extra, root.path(), &|_| None);
    assert_eq!(rules(&found), ["L7"], "{found:?}");
    for name in ["dev", "y"] {
        assert!(found[0].what.contains(name), "{name} in {found:?}");
    }
}

#[test]
fn l8_logins() {
    assert_one(
        "[profiles.x]\ntoken_file = \"{root}/x.token\"\ntier = \"external\"\n\n[[owners]]\nname = \"fixture-ext\"\ntier = \"external\"\nwrite = \"x\"\n",
        "L8",
        &["profile x", "fixture-ext"],
    );
    assert_one(
        "[profiles.x]\ntoken_file = \"{root}/x.token\"\ntier = \"release\"\n",
        "L8",
        &["profile x"],
    );
}

#[test]
fn l9_targets_are_literal() {
    assert_one("[targets]\nbad = \"nope\"\n", "L9", &["target bad", "nope"]);
    assert_one(
        "[targets]\nbad = \"@channel\"\n",
        "L9",
        &["target bad", "@channel"],
    );
}

#[test]
fn l10_names() {
    assert_one(
        "[profiles.Dev2]\ntoken_file = \"{root}/d.token\"\ntier = \"dev\"\n",
        "L10",
        &["Dev2"],
    );
    assert_one(
        "[targets]\nChan = \"alabsystems/aterm\"\n",
        "L10",
        &["Chan"],
    );
    assert_one(
        "[profiles.anonymous]\ntoken_file = \"{root}/a.token\"\ntier = \"dev\"\n",
        "L10",
        &["anonymous"],
    );
}

#[test]
fn every_fault_at_once_in_rule_order() {
    let found = faults(
        "[profiles.Zed]\ntoken_file = \"{root}/z.token\"\ntier = \"dev\"\n\n\
         [[owners]]\nname = \"fixture-dev\"\ntier = \"prod\"\nwrite = [\"dev\", \"dev\"]\n\n\
         [[owners]]\nname = \"fixture-ext\"\ntier = \"external\"\nread = [\"ghost\"]\n\n\
         [targets]\nbad = \"nope\"\n",
    );
    assert_eq!(rules(&found), ["L1", "L1", "L4", "L9", "L10"], "{found:?}");
    assert!(found[0].what.contains("fixture-dev"), "{found:?}");
    assert!(found[1].what.contains("ghost"), "{found:?}");
}

#[test]
fn drafts_fp_reaches_the_profile_and_vouches_only_its_token() {
    let root = common::scratch("rules-drafts");
    let fine = agh_fake::fine_grained_token("drafts");
    let fp = agh::identity::fingerprint(fine.as_bytes());
    let text = fill(
        &format!(
            "{BASE}\n[profiles.writer]\ntoken_file = \"{{root}}/w.token\"\ntier = \"dev\"\nlogin = \"fixture-ext-bot\"\ndrafts_fp = \"{fp}\"\n\n\
             [profiles.plain]\ntoken_file = \"{{root}}/p.token\"\ntier = \"dev\"\nlogin = \"fixture-reader\"\n"
        ),
        root.path(),
    );
    let parsed = config::parse(&text).expect("parses");
    let validated = config::validate(&parsed, root.path(), &config::file_id).expect("validates");
    let writer = &validated.profiles()["writer"];
    let plain = &validated.profiles()["plain"];
    assert_eq!(writer.drafts_fp.as_deref(), Some(fp.as_str()));
    let classic = Credential::for_tests("plain", Tier::Dev, agh_fake::token("drafts").as_bytes());
    assert!(plain.drafts_vouched(&classic));
    let vouched = Credential::for_tests("writer", Tier::Dev, fine.as_bytes());
    assert!(writer.drafts_vouched(&vouched));
    let unvouched = Credential::for_tests("plain", Tier::Dev, fine.as_bytes());
    assert!(!plain.drafts_vouched(&unvouched));
    let rotated = agh_fake::fine_grained_token("rotated");
    let other = Credential::for_tests("writer", Tier::Dev, rotated.as_bytes());
    assert!(!writer.drafts_vouched(&other));
}

#[test]
fn a_home_path_expands() {
    let root = common::scratch("rules-home");
    let text = fill(
        &format!("{BASE}\n[profiles.x]\ntoken_file = \"~/tokens/x.token\"\ntier = \"dev\"\n"),
        root.path(),
    );
    let parsed = config::parse(&text).expect("parses");
    let validated = config::validate(&parsed, root.path(), &config::file_id).expect("validates");
    assert_eq!(
        validated.profiles()["x"].source,
        Source::File(root.path().join("tokens").join("x.token"))
    );
    assert_eq!(
        config::expand("~/a/b", root.path()),
        Some(root.path().join("a/b"))
    );
    assert_eq!(config::expand("a/b", root.path()), None);
    assert_eq!(config::expand("~a/b", root.path()), None);
}

/// `config::load` and `agh policy check`: unix, because each writes a private agh.toml.
#[cfg(unix)]
mod files {
    use agh::config::{self, Dirs};
    use agh::error::{Kind, Sub};
    use agh::policy::RefusalCode;
    use agh_fake::FakeGithub;

    use crate::common;

    #[test]
    fn load_answers_none_for_no_file_and_refuses_a_bad_one() {
        let root = common::scratch("load");
        let dirs = Dirs::under(root.path());
        assert_eq!(config::load(&dirs).expect("no file is no config"), None);
        common::write_config(&dirs, &common::canonical_config(root.path()));
        assert!(config::load(&dirs).expect("loads").is_some());

        common::chmod(&dirs.config_file(), 0o644);
        let loose = config::load(&dirs).expect_err("a loose agh.toml");
        assert_eq!(loose.kind, Kind::Refused);
        assert_eq!(loose.sub, Sub::Policy(RefusalCode::ConfigInvalid));
        assert!(loose.next.contains("chmod 600"), "{}", loose.next);
        assert!(loose.next.contains("chmod 644"), "{}", loose.next);
        common::chmod(&dirs.config_file(), 0o600);

        aterm_uds::private_file::write_private(&dirs.config, "agh.toml", &[0xff, 0xfe, b'\n'])
            .expect("agh.toml");
        let binary = config::load(&dirs).expect_err("not UTF-8");
        assert_eq!(binary.kind, Kind::Refused);
        assert_eq!(binary.sub, Sub::Policy(RefusalCode::ConfigInvalid));

        common::write_config(&dirs, "[targets]\nbad = \"nope\"\n");
        let faulty = config::load(&dirs).expect_err("a fault");
        assert_eq!(faulty.kind, Kind::Refused);
        assert_eq!(faulty.sub, Sub::Policy(RefusalCode::ConfigInvalid));
        assert!(faulty.why.contains("L9"), "{}", faulty.why);
        assert_eq!(faulty.next, "agh policy check");
    }

    /// `agh policy check` in a context over `root`, with `args` after the verb.
    fn check(root: &std::path::Path, args: &[&str]) -> (u8, String, String) {
        let fake = FakeGithub::start();
        let (mut ctx, captured) = common::ctx(root, &fake, common::env());
        let mut line = vec!["policy", "check"];
        line.extend_from_slice(args);
        let exit = common::run_cli(&mut ctx, &line);
        (exit, captured.stdout_text(), captured.stderr_text())
    }

    #[test]
    fn policy_check_reports_a_valid_file() {
        let root = common::scratch("check-ok");
        let dirs = Dirs::under(root.path());
        common::write_config(&dirs, &common::canonical_config(root.path()));
        let (exit, stdout, stderr) = check(root.path(), &[]);
        assert_eq!(exit, 0, "{stderr}");
        let expected = format!(
            "{{\"config\":\"{}\",\"counts\":{{\"owners\":2,\"profiles\":2,\"targets\":1}},\"faults\":[],\"ok\":true,\"present\":true}}\n",
            dirs.config_file().display()
        );
        assert_eq!(stdout, expected);
        assert_eq!(stderr, "");
    }

    #[test]
    fn policy_check_of_no_file_is_ok() {
        let root = common::scratch("check-none");
        let (exit, stdout, _) = check(root.path(), &[]);
        assert_eq!(exit, 0);
        let value: aterm_json::Value = aterm_json::from_str(&stdout).expect("JSON");
        assert_eq!(value["present"], aterm_json::Value::from(false));
        assert_eq!(value["ok"], aterm_json::Value::from(true));
    }

    #[test]
    fn policy_check_reports_every_fault_and_exits_invalid() {
        let root = common::scratch("check-bad");
        let dirs = Dirs::under(root.path());
        let text = format!(
            "{}\n[profiles.Zed]\nenv = \"Z\"\ntier = \"dev\"\n",
            common::canonical_config(root.path())
                .replace("channel = \"alabsystems/aterm\"", "channel = \"nope\"")
        );
        common::write_config(&dirs, &text);
        let (exit, stdout, stderr) = check(root.path(), &[]);
        assert_eq!(exit, 10);
        let value: aterm_json::Value = aterm_json::from_str(&stdout).expect("JSON");
        assert_eq!(value["ok"], aterm_json::Value::from(false));
        let faults = value["faults"].as_array().expect("faults");
        let rules: Vec<&str> = faults
            .iter()
            .map(|f| f["rule"].as_str().expect("rule"))
            .collect();
        assert_eq!(rules, ["L9", "L10"]);
        let lines: Vec<&str> = stderr.lines().collect();
        assert_eq!(lines.len(), 2, "{stderr}");
        assert!(
            lines[0].starts_with("agh: invalid exit=10 verb=policy.check"),
            "{stderr}"
        );
        let error: aterm_json::Value = aterm_json::from_str(lines[1]).expect("JSON");
        assert_eq!(error["error"]["kind"], aterm_json::Value::from("invalid"));
    }

    #[test]
    fn policy_check_reads_the_named_file() {
        let root = common::scratch("check-path");
        let elsewhere = root.path().join("elsewhere");
        aterm_uds::private_file::create_private_dir(&elsewhere).expect("a directory");
        aterm_uds::private_file::write_private(
            &elsewhere,
            "other.toml",
            common::canonical_config(root.path()).as_bytes(),
        )
        .expect("the file");
        let path = elsewhere.join("other.toml");
        let path_text = path.display().to_string();
        let (exit, stdout, stderr) = check(root.path(), &["--config", &path_text]);
        assert_eq!(exit, 0, "{stderr}");
        let value: aterm_json::Value = aterm_json::from_str(&stdout).expect("JSON");
        assert_eq!(value["config"], aterm_json::Value::from(path_text.as_str()));
        assert_eq!(value["present"], aterm_json::Value::from(true));
        assert_eq!(value["counts"]["owners"], aterm_json::Value::from(2u64));
    }

    /// A token pasted into agh.toml reaches neither `policy check`'s stdout or stderr nor
    /// `config::load`'s refusal.
    #[test]
    fn a_pasted_token_is_never_echoed() {
        let token = agh_fake::token("leak");
        for pasted_value in [token.as_str(), &token[4..]] {
            for extra in super::pasted(pasted_value) {
                let root = common::scratch("check-leak");
                let dirs = Dirs::under(root.path());
                let text = format!("{}\n{extra}", super::fill(super::BASE, root.path()));
                common::write_config(&dirs, &text);
                let (exit, stdout, stderr) = check(root.path(), &[]);
                assert_eq!(exit, 10, "{stderr}");
                super::assert_unleaked(&stdout, pasted_value);
                super::assert_unleaked(&stderr, pasted_value);
                let refused = config::load(&dirs).expect_err("an L1 fault");
                assert_eq!(refused.kind, Kind::Refused);
                super::assert_unleaked(&refused.why, pasted_value);
                super::assert_unleaked(&refused.next, pasted_value);
            }
        }
    }

    /// A `--config` naming no file follows load's rule (plan §7 body 8): `present: false`,
    /// `ok: true`, exit 0, and the report names the path it looked at.
    #[test]
    fn policy_check_reports_a_named_file_that_is_not_there_as_absent() {
        let root = common::scratch("check-nofile");
        let path = root.path().join("does-not-exist.toml");
        let path_text = path.display().to_string();
        let (exit, stdout, stderr) = check(root.path(), &["--config", &path_text]);
        assert_eq!(exit, 0, "{stderr}");
        assert_eq!(stderr, "");
        let value: aterm_json::Value = aterm_json::from_str(&stdout).expect("JSON");
        assert_eq!(value["config"], aterm_json::Value::from(path_text.as_str()));
        assert_eq!(value["present"], aterm_json::Value::from(false));
        assert_eq!(value["ok"], aterm_json::Value::from(true));
        assert_eq!(value["faults"], aterm_json::Value::Array(Vec::new()));
    }

    #[test]
    fn policy_check_names_a_loose_file() {
        let root = common::scratch("check-loose");
        let dirs = Dirs::under(root.path());
        common::write_config(&dirs, &common::canonical_config(root.path()));
        common::chmod(&dirs.config_file(), 0o640);
        let (exit, stdout, _) = check(root.path(), &[]);
        assert_eq!(exit, 10);
        let value: aterm_json::Value = aterm_json::from_str(&stdout).expect("JSON");
        let fault = &value["faults"][0];
        assert_eq!(fault["rule"], aterm_json::Value::from("file"));
        let what = fault["what"].as_str().expect("what");
        assert!(what.contains("chmod 600"), "{what}");
        assert!(what.contains("chmod 640"), "{what}");
    }

    /// A `--config` whose directory is a link: the remedy names that directory, the link
    /// to fix, rather than the file, which is no link.
    #[test]
    fn policy_check_names_a_linked_directory() {
        let root = common::scratch("check-linked");
        let real = root.path().join("real");
        aterm_uds::private_file::create_private_dir(&real).expect("a private directory");
        aterm_uds::private_file::write_private(
            &real,
            "agh.toml",
            common::canonical_config(root.path()).as_bytes(),
        )
        .expect("agh.toml");
        let linked = root.path().join("linked");
        std::os::unix::fs::symlink(&real, &linked).expect("a directory symlink");
        let path_text = linked.join("agh.toml").display().to_string();
        let (exit, stdout, _) = check(root.path(), &["--config", &path_text]);
        assert_eq!(exit, 10);
        let value: aterm_json::Value = aterm_json::from_str(&stdout).expect("JSON");
        let fault = &value["faults"][0];
        assert_eq!(fault["rule"], aterm_json::Value::from("file"));
        let what = fault["what"].as_str().expect("what");
        let remedy = what
            .split_once("remedy: ")
            .map(|(_, r)| r)
            .expect("a remedy");
        assert!(remedy.contains(&linked.display().to_string()), "{what}");
    }

    /// The `what` of `policy check --config {path}`'s one fault, which must be a file fault.
    fn file_fault(root: &std::path::Path, path: &std::path::Path) -> String {
        let path_text = path.display().to_string();
        let (exit, stdout, _) = check(root, &["--config", &path_text]);
        assert_eq!(exit, 10, "{stdout}");
        let value: aterm_json::Value = aterm_json::from_str(&stdout).expect("JSON");
        assert_eq!(value["faults"][0]["rule"], aterm_json::Value::from("file"));
        value["faults"][0]["what"]
            .as_str()
            .expect("what")
            .to_string()
    }

    /// `text` as agh.toml in `dir` (created private), then `dir` and the file chmodded.
    fn loose_config(root: &std::path::Path, dir: &std::path::Path, dir_mode: u32) {
        std::fs::create_dir_all(dir).expect("the directory");
        common::chmod(dir, 0o700);
        aterm_uds::private_file::write_private(
            dir,
            "agh.toml",
            common::canonical_config(root).as_bytes(),
        )
        .expect("agh.toml");
        common::chmod(&dir.join("agh.toml"), 0o640);
        common::chmod(dir, dir_mode);
    }

    /// agh.toml directly in the home directory, which is open to the group (0750, as on
    /// macOS): no remedy asks the person to narrow `~`, by any spelling of it, or names a
    /// chmod of the file, which cannot lift the directory's refusal; it names `--config`
    /// and the agh config directory instead.
    #[test]
    fn policy_check_never_narrows_home() {
        let root = common::scratch("check-home");
        let dirs = Dirs::under(root.path());
        loose_config(root.path(), &dirs.home, 0o750);
        let spelled = dirs.home.join("../home/agh.toml");
        for path in [dirs.home.join("agh.toml"), spelled] {
            let what = file_fault(root.path(), &path);
            assert!(!what.contains("chmod"), "{what}");
            assert!(!what.contains("''"), "{what}");
            assert!(what.contains("--config"), "{what}");
            assert!(what.contains(&dirs.config.display().to_string()), "{what}");
        }
        common::chmod(&dirs.home, 0o700);
        let what = file_fault(root.path(), &dirs.home.join("agh.toml"));
        assert!(what.contains("chmod 600 ~/agh.toml"), "{what}");
        assert!(!what.contains("chmod 700"), "{what}");
    }

    /// `~/.config`, `~/.config/aterm` and their XDG-relocated equivalents (the agh config
    /// directory's parent and grandparent) are never narrowed either (design §6.2).
    #[test]
    fn policy_check_never_narrows_the_config_directories() {
        let root = common::scratch("check-dotconfig");
        let dirs = Dirs::under(root.path());
        let aterm = dirs.config.parent().expect("the aterm config directory");
        let xdg = aterm.parent().expect("the XDG config directory");
        let dot = dirs.home.join(".config");
        for dir in [
            dot.join("aterm"),
            dot,
            aterm.to_path_buf(),
            xdg.to_path_buf(),
        ] {
            loose_config(root.path(), &dir, 0o755);
            let what = file_fault(root.path(), &dir.join("agh.toml"));
            assert!(!what.contains("chmod 700"), "{}: {what}", dir.display());
            assert!(what.contains("--config"), "{}: {what}", dir.display());
            common::chmod(&dir, 0o700);
        }
    }

    /// agh.toml that is a link (as a dotfile manager makes) or a directory, in a loose
    /// directory: `read_private` refuses the directory first, and the remedy names no chmod
    /// of the file, which would follow the link and whose "inverse" would widen the target.
    #[test]
    fn policy_check_names_no_chmod_of_a_linked_or_directory_file() {
        let root = common::scratch("check-loose-link");
        let dotfiles = root.path().join("dotfiles");
        loose_config(root.path(), &dotfiles, 0o700);
        let linked = root.path().join("d");
        std::fs::create_dir(&linked).expect("a directory");
        std::os::unix::fs::symlink(dotfiles.join("agh.toml"), linked.join("agh.toml"))
            .expect("a file symlink");
        common::chmod(&linked, 0o755);
        let link_text = linked.join("agh.toml").display().to_string();
        let what = file_fault(root.path(), &linked.join("agh.toml"));
        assert!(
            what.contains(&format!("chmod 700 {}", linked.display())),
            "{what}"
        );
        for args in what.split("chmod ").skip(1) {
            let target = args.split_whitespace().nth(1).unwrap_or_default();
            assert!(
                !target.starts_with(&link_text),
                "a chmod of the link: {what}"
            );
        }
        assert!(what.contains(&link_text), "{what}");

        let holder = root.path().join("e");
        std::fs::create_dir_all(holder.join("agh.toml")).expect("a directory named agh.toml");
        common::chmod(&holder, 0o755);
        let what = file_fault(root.path(), &holder.join("agh.toml"));
        assert!(!what.contains("chmod"), "{what}");
    }

    /// A `--config` under a directory link that is another user's (root's `/tmp` on macOS):
    /// the remedy names the link only as the place and offers `--config`, never the link's
    /// change. Skipped where no system directory is such a link.
    #[test]
    fn policy_check_of_another_users_link_names_no_change_to_it() {
        let Some((link, real)) = crate::identity_files::foreign_dir_link() else {
            return;
        };
        let root = common::scratch("check-foreign-link");
        let what = file_fault(root.path(), &link.join("agh-no-such-config.toml"));
        let remedy = what
            .split_once("remedy: ")
            .map(|(_, r)| r)
            .expect("a remedy");
        assert!(remedy.contains(&real.display().to_string()), "{what}");
        assert!(remedy.contains("--config"), "{what}");
        assert_eq!(
            crate::identity_files::path_mentions(remedy, &link.display().to_string()),
            1,
            "{what}"
        );
    }

    /// The agh directory is one agh never narrows (here it is the home directory, at 0755):
    /// load's refusal, the `next` of every routed verb, names no `--config`, which no routed
    /// verb takes and which changes no file that routes, and no chmod of it.
    #[test]
    fn load_of_a_never_narrowed_agh_directory_names_no_config_flag() {
        let root = common::scratch("load-home-agh");
        let agh_dir = root.path().join("home");
        let dirs = Dirs {
            config: agh_dir.clone(),
            state: root.path().join("state"),
            home: agh_dir.clone(),
        };
        common::write_config(&dirs, &common::canonical_config(root.path()));
        common::chmod(&agh_dir, 0o755);
        let refused = config::load(&dirs).expect_err("a loose agh directory");
        assert_eq!(refused.kind, Kind::Refused);
        assert_eq!(refused.sub, Sub::Policy(RefusalCode::ConfigInvalid));
        assert!(!refused.next.contains("--config"), "{}", refused.next);
        assert!(!refused.next.contains("chmod"), "{}", refused.next);
        assert!(refused.next.contains("agh.toml"), "{}", refused.next);
    }

    /// agh.toml with a second hard link: `read_private` refuses it for the link, which no
    /// chmod lifts, so the remedy is a private copy put in its place, never the mode's
    /// chmod and its inverse.
    #[test]
    fn a_hard_linked_config_gets_a_private_copy_not_a_chmod() {
        let root = common::scratch("check-hardlink");
        let dir = root.path().join("c");
        loose_config(root.path(), &dir, 0o700);
        let file = dir.join("agh.toml");
        let other = root.path().join("other");
        aterm_uds::private_file::create_private_dir(&other).expect("a private directory");
        std::fs::hard_link(&file, other.join("agh.toml")).expect("a hard link");
        let what = file_fault(root.path(), &file);
        let remedy = what
            .split_once("remedy: ")
            .map(|(_, r)| r)
            .expect("a remedy");
        let file_text = file.display().to_string();
        assert!(remedy.contains(&format!("{file_text}.new")), "{what}");
        let bare = remedy.replace(&root.path().display().to_string(), "<root>");
        assert!(!bare.contains("640"), "{what}");
        assert!(
            !remedy.contains(&format!("chmod 600 {file_text} ")),
            "{what}"
        );
    }

    /// `--config ~/x/agh.toml` reads the home directory's file (agh's path rule, token_file's),
    /// never `./~/x/agh.toml`.
    #[test]
    fn policy_check_expands_a_home_config_path() {
        let root = common::scratch("check-tilde");
        let dirs = Dirs::under(root.path());
        let dir = dirs.home.join("x");
        std::fs::create_dir_all(&dirs.home).expect("home");
        aterm_uds::private_file::create_private_dir(&dir).expect("a private directory");
        let text = format!(
            "{}\n[profiles.Zed]\nenv = \"Z\"\ntier = \"dev\"\n",
            common::canonical_config(root.path())
        );
        aterm_uds::private_file::write_private(&dir, "agh.toml", text.as_bytes())
            .expect("agh.toml");
        let (exit, stdout, _) = check(root.path(), &["--config", "~/x/agh.toml"]);
        assert_eq!(exit, 10, "{stdout}");
        let value: aterm_json::Value = aterm_json::from_str(&stdout).expect("JSON");
        assert_eq!(value["present"], aterm_json::Value::from(true));
        assert_eq!(value["config"], aterm_json::Value::from("~/x/agh.toml"));
    }

    /// A relative `--config` is reported as `./…`, so it never reads as a `~/` path.
    #[test]
    fn policy_check_shows_a_relative_config_path_as_relative() {
        let root = common::scratch("check-relative");
        let (exit, stdout, _) = check(root.path(), &["--config", "agh-no-such-dir/agh.toml"]);
        assert_eq!(exit, 0, "{stdout}");
        let value: aterm_json::Value = aterm_json::from_str(&stdout).expect("JSON");
        assert_eq!(value["present"], aterm_json::Value::from(false));
        assert_eq!(
            value["config"],
            aterm_json::Value::from("./agh-no-such-dir/agh.toml")
        );
    }
}
