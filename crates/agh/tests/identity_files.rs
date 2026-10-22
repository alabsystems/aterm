// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
// Author: Andrew Yates

//! Token files and env sources under the strict load (design §6.1, §6.6): shape, kind,
//! fingerprint, file identity, the loose, symlink and missing cases and their remedies, and
//! the ambient report. Unix only: every test reads a private token file or its remedy.

use std::ffi::OsString;
use std::os::unix::ffi::OsStringExt;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};

use agh::identity::{self, LoadError, MapEnv, Profile, Source, TokenKind};
use agh::policy::{Presence, Tier};

use crate::common;

/// `bytes` as `name` in a fresh private directory `dir` under `root`.
fn write_raw(root: &Path, dir: &str, name: &str, bytes: &[u8]) -> PathBuf {
    let dir = root.join(dir);
    aterm_uds::private_file::create_private_dir(&dir).expect("a private directory");
    aterm_uds::private_file::write_private(&dir, name, bytes).expect("a private file");
    dir.join(name)
}

fn file_profile(path: &Path) -> Profile {
    Profile::new("dev", Source::File(path.to_path_buf()), Tier::Dev)
}

fn load(path: &Path) -> Result<identity::Credential, LoadError> {
    identity::load_for_tests(&file_profile(path), &common::env())
}

#[test]
fn a_private_file_loads_with_its_kind_fingerprint_and_identity() {
    let root = common::scratch("id-load");
    let token = agh_fake::token("load");
    let path = common::write_token(&root.path().join("tokens"), "dev.token", &token);
    let cred = load(&path).expect("the token loads");
    assert_eq!(cred.profile(), "dev");
    assert_eq!(cred.tier(), Tier::Dev);
    assert_eq!(cred.kind(), TokenKind::Classic);
    assert_eq!(cred.fingerprint(), identity::fingerprint(token.as_bytes()));
    let meta = std::fs::metadata(&path).expect("metadata");
    assert_eq!(cred.file_id(), Some((meta.dev(), meta.ino())));
    let fine = agh_fake::fine_grained_token("load");
    let path = write_raw(root.path(), "fine", "dev.token", fine.as_bytes());
    assert_eq!(load(&path).expect("loads").kind(), TokenKind::FineGrained);
}

#[test]
fn one_trailing_newline_is_trimmed() {
    let root = common::scratch("id-trim");
    let token = agh_fake::token("trim");
    for (dir, ending) in [("lf", "\n"), ("crlf", "\r\n"), ("bare", "")] {
        let path = write_raw(
            root.path(),
            dir,
            "t.token",
            format!("{token}{ending}").as_bytes(),
        );
        let cred = load(&path).expect("the token loads");
        assert_eq!(
            cred.fingerprint(),
            identity::fingerprint(token.as_bytes()),
            "{dir}"
        );
    }
}

#[test]
fn a_token_of_the_wrong_shape_is_malformed() {
    let root = common::scratch("id-shape");
    let token = agh_fake::token("shape");
    let cases: [(&str, Vec<u8>); 5] = [
        ("two-newlines", format!("{token}\n\n").into_bytes()),
        ("nineteen", token.as_bytes()[..19].to_vec()),
        (
            "space",
            format!("{} {}", &token[..20], &token[20..]).into_bytes(),
        ),
        ("long", vec![b'a'; 257]),
        ("too-long-unterminated", vec![b'a'; 256]),
    ];
    for (dir, bytes) in cases {
        let path = write_raw(root.path(), dir, "t.token", &bytes);
        let error = load(&path).expect_err(dir);
        assert_eq!(error.slug(), "malformed", "{dir}: {error}");
        assert_eq!(error.presence(), Presence::Unusable("malformed"), "{dir}");
    }
    let edge = write_raw(root.path(), "edge", "t.token", &[b'a'; 255]);
    assert_eq!(
        load(&edge).expect("255 bytes load").kind(),
        TokenKind::Unknown
    );
    let twenty = write_raw(root.path(), "twenty", "t.token", &token.as_bytes()[..20]);
    assert!(load(&twenty).is_ok());
}

#[test]
fn a_refresh_token_is_unsupported() {
    let root = common::scratch("id-refresh");
    let refresh = format!("ghr_{}", &agh_fake::token("refresh")[4..]);
    let path = write_raw(root.path(), "tokens", "r.token", refresh.as_bytes());
    let error = load(&path).expect_err("a refresh token");
    assert_eq!(error.slug(), "unsupported");
    assert_eq!(error.presence(), Presence::Unusable("unsupported"));
}

#[test]
fn a_loose_file_or_directory_is_loose() {
    let root = common::scratch("id-loose");
    let token = agh_fake::token("loose");
    let file = common::write_token(&root.path().join("file"), "t.token", &token);
    common::chmod(&file, 0o644);
    assert!(matches!(load(&file), Err(LoadError::Loose(_))));
    let dir_case = common::write_token(&root.path().join("dir"), "t.token", &token);
    common::chmod(&root.path().join("dir"), 0o755);
    let error = load(&dir_case).expect_err("a loose directory");
    assert!(matches!(error, LoadError::Loose(_)), "{error:?}");
    assert_eq!(error.presence(), Presence::Unusable("loose"));
}

#[test]
fn a_symlink_is_refused_and_a_missing_file_is_absent() {
    let root = common::scratch("id-link");
    let real = common::write_token(
        &root.path().join("tokens"),
        "real.token",
        &agh_fake::token("l"),
    );
    let link = root.path().join("tokens").join("link.token");
    std::os::unix::fs::symlink(&real, &link).expect("a symlink");
    let error = load(&link).expect_err("a symlink");
    assert_eq!(error, LoadError::Symlink);
    assert_eq!(error.presence(), Presence::Unusable("symlink"));
    let missing = load(&root.path().join("tokens").join("none.token")).expect_err("missing");
    assert_eq!(missing, LoadError::Missing);
    assert_eq!(missing.presence(), Presence::Absent);
    let profile = file_profile(&root.path().join("tokens").join("none.token"));
    assert_eq!(
        identity::presence(&profile, &common::env()),
        Presence::Absent
    );
}

#[test]
fn the_loose_remedy_names_only_the_loose_parts() {
    let root = common::scratch("id-remedy");
    let home = root.path();
    let token = agh_fake::token("remedy");
    let remedy_for = |dir: &str, dir_mode: u32, file_mode: u32| {
        let path = common::write_token(&home.join(dir), "dev.token", &token);
        common::chmod(&path, file_mode);
        common::chmod(&home.join(dir), dir_mode);
        let error = load(&path).expect_err("loose");
        assert!(matches!(error, LoadError::Loose(_)), "{error:?}");
        error
            .remedy(&file_profile(&path), home)
            .expect("a loose file has a remedy")
    };
    let file_only = remedy_for("f", 0o700, 0o644);
    assert!(file_only.contains("chmod 600 ~/f/dev.token"), "{file_only}");
    assert!(file_only.contains("chmod 644 ~/f/dev.token"), "{file_only}");
    assert!(!file_only.contains("chmod 700"), "{file_only}");
    assert!(
        file_only.contains("agh auth add dev --tier dev --stdin < ~/f/dev.token"),
        "{file_only}"
    );
    let dir_only = remedy_for("d", 0o755, 0o600);
    assert!(dir_only.contains("chmod 700 ~/d"), "{dir_only}");
    assert!(dir_only.contains("chmod 755 ~/d"), "{dir_only}");
    assert!(!dir_only.contains("chmod 600"), "{dir_only}");
    let both = remedy_for("b", 0o750, 0o640);
    for part in [
        "chmod 700 ~/b",
        "chmod 600 ~/b/dev.token",
        "chmod 750 ~/b",
        "chmod 640 ~/b/dev.token",
    ] {
        assert!(both.contains(part), "{part}: {both}");
    }
    let symlink = LoadError::Symlink.remedy(&file_profile(&home.join("x")), home);
    assert!(symlink.is_some());
    assert_eq!(
        LoadError::Missing.remedy(&file_profile(&home.join("x")), home),
        None
    );
}

/// A token file directly in the home directory, which is open to the group (0750, as on
/// macOS): the remedy never narrows `~` (design §6.2), and names no chmod of the file either,
/// which cannot lift a refusal the directory's mode makes. Only provisioning remains; with
/// `~` private, the file's own chmod is offered.
#[test]
fn the_loose_remedy_never_narrows_home() {
    let root = common::scratch("id-home");
    let home = root.path();
    let token = agh_fake::token("home");
    aterm_uds::private_file::write_private(home, "dev.token", token.as_bytes())
        .expect("a token file");
    let path = home.join("dev.token");
    common::chmod(home, 0o750);
    let remedy_for = |file_mode: u32| {
        common::chmod(&path, file_mode);
        let error = load(&path).expect_err("loose");
        assert!(matches!(error, LoadError::Loose(_)), "{error:?}");
        error
            .remedy(&file_profile(&path), home)
            .expect("a loose file has a remedy")
    };
    for remedy in [remedy_for(0o644), remedy_for(0o600)] {
        assert!(!remedy.contains("chmod"), "{remedy}");
        assert!(remedy.contains("--stdin < ~/dev.token"), "{remedy}");
        assert!(!remedy.contains("''"), "{remedy}");
    }
    common::chmod(home, 0o700);
    let file_only = remedy_for(0o644);
    assert!(file_only.contains("chmod 600 ~/dev.token"), "{file_only}");
    assert!(!file_only.contains("chmod 700"), "{file_only}");
}

/// `~/.config` and `~/.config/aterm` are never narrowed either (design §6.2), and each is
/// judged by identity: a `..` spelling of `~/.config` is still `~/.config`.
#[test]
fn the_loose_remedy_never_narrows_the_config_directories() {
    let root = common::scratch("id-dotconfig");
    let home = root.path();
    let token = agh_fake::token("dotconfig");
    let config = home.join(".config");
    let aterm = config.join("aterm");
    let in_config = common::write_token(&config, "dev.token", &token);
    let in_aterm = common::write_token(&aterm, "dev.token", &token);
    let spelled = home.join(".config/aterm/../dev.token");
    common::chmod(&aterm, 0o755);
    common::chmod(&config, 0o755);
    for path in [&in_config, &in_aterm, &spelled] {
        let error = load(path).expect_err("a loose directory");
        assert!(matches!(error, LoadError::Loose(_)), "{error:?}");
        let remedy = error
            .remedy(&file_profile(path), home)
            .expect("a loose directory has a remedy");
        assert!(!remedy.contains("chmod 700"), "{remedy}");
        assert!(remedy.contains("--stdin < ~/.config/"), "{remedy}");
    }
}

/// A token file that is a link, or a directory, in a loose directory: `read_private`
/// refuses the directory first. The remedy names no chmod of the file, which would follow
/// the link (and the link's mode is no inverse of the target's); a directory gets the
/// not-a-file remedy, with no chmod and no provisioning from it.
#[test]
fn the_loose_remedy_names_no_chmod_of_a_link_or_a_directory() {
    let root = common::scratch("id-loose-link");
    let home = root.path();
    let real = common::write_token(&home.join("real"), "dev.token", &agh_fake::token("ll"));
    common::chmod(&real, 0o644);
    let dir = home.join("d");
    std::fs::create_dir(&dir).expect("a directory");
    let link = dir.join("dev.token");
    std::os::unix::fs::symlink(&real, &link).expect("a file symlink");
    common::chmod(&dir, 0o755);
    let error = load(&link).expect_err("a loose directory");
    assert!(matches!(error, LoadError::Loose(_)), "{error:?}");
    let remedy = error.remedy(&file_profile(&link), home).expect("a remedy");
    assert!(remedy.contains("chmod 700 ~/d"), "{remedy}");
    for args in remedy.split("chmod ").skip(1) {
        let target = args.split_whitespace().nth(1).unwrap_or_default();
        assert!(!target.starts_with("~/d/"), "a chmod of the link: {remedy}");
    }
    assert!(remedy.contains("~/d/dev.token"), "{remedy}");

    let sub = home.join("e").join("dev.token");
    std::fs::create_dir_all(&sub).expect("a directory where the file would be");
    common::chmod(&home.join("e"), 0o755);
    let error = load(&sub).expect_err("a loose directory");
    assert!(matches!(error, LoadError::Loose(_)), "{error:?}");
    let remedy = error.remedy(&file_profile(&sub), home).expect("a remedy");
    assert!(!remedy.contains("chmod"), "{remedy}");
    assert!(!remedy.contains("--stdin"), "{remedy}");
    assert_eq!(
        Some(remedy),
        LoadError::Malformed("x").remedy(&file_profile(&sub), home)
    );
}

/// A directory that is another user's (`/`, root's): `read_private` refuses it for its
/// owner, which no chmod of its mode changes, so the remedy holds no chmod of it.
#[test]
fn the_loose_remedy_names_no_chmod_of_another_users_directory() {
    if aterm_uds::peer::our_uid() == 0 {
        return;
    }
    let root = common::scratch("id-owner");
    let remedy = LoadError::Loose("x".into())
        .remedy(&file_profile(Path::new("/agh-no-such-token")), root.path())
        .expect("the provisioning remedy");
    assert!(!remedy.contains("chmod 700 /"), "{remedy}");
    assert!(remedy.contains("--stdin < /agh-no-such-token"), "{remedy}");
}

/// A token file whose directory is a link: the remedy names the directory, the link to
/// fix; a file that is itself the link gets the other remedy.
#[test]
fn the_symlink_remedy_names_a_linked_directory() {
    let root = common::scratch("id-linked");
    let home = root.path();
    let real = common::write_token(&home.join("real"), "dev.token", &agh_fake::token("ld"));
    std::os::unix::fs::symlink(home.join("real"), home.join("linked")).expect("a dir symlink");
    let through = home.join("linked").join("dev.token");
    let error = load(&through).expect_err("a linked directory");
    assert_eq!(error, LoadError::Symlink);
    let linked = error
        .remedy(&file_profile(&through), home)
        .expect("a remedy");
    assert!(linked.contains("~/linked"), "{linked}");
    let link = home.join("real").join("link.token");
    std::os::unix::fs::symlink(&real, &link).expect("a file symlink");
    let file_link = LoadError::Symlink
        .remedy(&file_profile(&link), home)
        .expect("a remedy");
    assert!(!file_link.contains("~/real"), "{file_link}");
    assert_ne!(linked, file_link);
}

/// A system directory that is a link of another user's (root's `/tmp` on macOS), or `None`
/// where there is none, or this is root.
pub(crate) fn foreign_dir_link() -> Option<(PathBuf, PathBuf)> {
    if aterm_uds::peer::our_uid() == 0 {
        return None;
    }
    ["/tmp", "/var", "/etc"].into_iter().find_map(|dir| {
        let meta = std::fs::symlink_metadata(dir).ok()?;
        let foreign = meta.file_type().is_symlink() && meta.uid() != aterm_uds::peer::our_uid();
        let real = std::fs::canonicalize(dir).ok()?;
        foreign.then(|| (PathBuf::from(dir), real))
    })
}

/// A token file under a directory link that is another user's: the person cannot make it a
/// real directory, so the remedy names the real path and the token_file key, and never the
/// link's change. Skipped where no such link exists (no system directory is a link).
#[test]
fn the_symlink_remedy_of_another_users_link_names_only_the_real_path() {
    let Some((link, real)) = foreign_dir_link() else {
        return;
    };
    let root = common::scratch("id-foreign-link");
    let profile = file_profile(&link.join("agh-no-such.token"));
    let remedy = LoadError::Symlink
        .remedy(&profile, root.path())
        .expect("a remedy");
    assert!(remedy.contains(&real.display().to_string()), "{remedy}");
    assert!(remedy.contains("token_file"), "{remedy}");
    assert_eq!(
        path_mentions(&remedy, &link.display().to_string()),
        1,
        "the link is named only as where the file is: {remedy}"
    );
    // The control: a link of this user's is named twice, as the place and as the fix.
    std::os::unix::fs::symlink(root.path(), root.path().join("mine")).expect("a dir symlink");
    let mine = LoadError::Symlink
        .remedy(
            &file_profile(&root.path().join("mine/x.token")),
            root.path(),
        )
        .expect("a remedy");
    assert_eq!(path_mentions(&mine, "~/mine"), 2, "{mine}");
}

/// How many words of `text` are `path`, with trailing punctuation dropped.
pub(crate) fn path_mentions(text: &str, path: &str) -> usize {
    text.split_whitespace()
        .filter(|word| word.trim_end_matches([':', ',', ';', ')']) == path)
        .count()
}

/// The Malformed remedy follows the rule broken: the token's shape (which carries the
/// 20-255 bounds) for a token of the wrong shape or size, and another for a token_file that
/// names a directory, and for a variable.
#[test]
fn the_malformed_remedy_follows_the_rule_broken() {
    let root = common::scratch("id-malformed");
    let home = root.path();
    let short = write_raw(home, "short", "t.token", b"too short");
    let shape = load(&short).expect_err("a short token");
    let shape_remedy = shape.remedy(&file_profile(&short), home).expect("a remedy");
    assert!(shape_remedy.contains("20-255"), "{shape_remedy}");
    let long = write_raw(home, "long", "t.token", &[b'a'; 300]);
    let size = load(&long).expect_err("a long file");
    assert_eq!(size.slug(), "malformed");
    assert_eq!(
        size.remedy(&file_profile(&long), home),
        Some(shape_remedy.clone())
    );
    std::fs::create_dir(home.join("short").join("sub")).expect("a directory");
    let dir = home.join("short").join("sub");
    let not_file = load(&dir).expect_err("a directory");
    assert_eq!(not_file.slug(), "malformed");
    let not_file_remedy = not_file
        .remedy(&file_profile(&dir), home)
        .expect("a remedy");
    assert!(!not_file_remedy.contains("20-255"), "{not_file_remedy}");
    let env_profile = Profile::new("ext", Source::Env("AGH_TEST_TOKEN".into()), Tier::External);
    let bytes = OsString::from_vec(vec![b'g', b'h', b'p', b'_', 0xff, 0xfe]);
    let not_utf8 = identity::load_for_tests(&env_profile, &RawEnv(bytes)).expect_err("not UTF-8");
    let env_remedy = not_utf8.remedy(&env_profile, home).expect("a remedy");
    assert_ne!(env_remedy, shape_remedy);
    assert_ne!(env_remedy, not_file_remedy);
}

/// An environment whose every variable is one raw value, which need not be UTF-8: the case
/// `MapEnv::with` cannot spell.
struct RawEnv(OsString);

impl identity::EnvSource for RawEnv {
    fn var_os(&self, _: &str) -> Option<OsString> {
        Some(self.0.clone())
    }
}

#[test]
fn an_env_source_loads_and_judges_its_variable() {
    let token = agh_fake::token("env");
    let profile = Profile::new("ext", Source::Env("AGH_TEST_TOKEN".into()), Tier::External);
    let set = MapEnv::new().with("AGH_TEST_TOKEN", &token);
    let cred = identity::load_for_tests(&profile, &set).expect("the variable loads");
    assert_eq!(cred.file_id(), None);
    assert_eq!(cred.fingerprint(), identity::fingerprint(token.as_bytes()));
    assert_eq!(cred.tier(), Tier::External);
    let unset = identity::load_for_tests(&profile, &MapEnv::new()).expect_err("unset");
    assert_eq!(unset, LoadError::Missing);
    let empty = MapEnv::new().with("AGH_TEST_TOKEN", "");
    assert_eq!(
        identity::load_for_tests(&profile, &empty).expect_err("empty"),
        LoadError::Missing
    );
    let bytes = OsString::from_vec(vec![b'g', b'h', b'p', b'_', 0xff, 0xfe]);
    let error = identity::load_for_tests(&profile, &RawEnv(bytes)).expect_err("not UTF-8");
    assert_eq!(error.slug(), "malformed");
    assert_eq!(identity::presence(&profile, &set), Presence::Present);
}

#[test]
fn ambient_tokens_are_reported_and_an_empty_one_is_ignored() {
    let gh = agh_fake::token("gh");
    let github = agh_fake::token("github");
    let env = MapEnv::new()
        .with("GH_TOKEN", &gh)
        .with("GITHUB_TOKEN", &github);
    let found = identity::ambient(&env);
    let vars: Vec<&str> = found.iter().map(|a| a.var).collect();
    assert_eq!(vars, ["GH_TOKEN", "GITHUB_TOKEN"]);
    assert_eq!(found[0].fingerprint, identity::fingerprint(gh.as_bytes()));
    assert_eq!(found[1].kind, Some(TokenKind::Classic));
    let empty = MapEnv::new()
        .with("GH_TOKEN", "")
        .with("GITHUB_TOKEN", &github);
    let vars: Vec<&str> = identity::ambient(&empty).iter().map(|a| a.var).collect();
    assert_eq!(vars, ["GITHUB_TOKEN"]);
}

#[test]
fn the_fingerprint_is_fixed() {
    // sha256("agh/fp/v1\0agh fingerprint fixture"), computed once outside agh.
    assert_eq!(
        identity::fingerprint(b"agh fingerprint fixture"),
        "8b4416e21b8a"
    );
}

#[test]
fn a_credential_debug_holds_no_token_byte() {
    let root = common::scratch("id-debug");
    let token = agh_fake::token("debug");
    let path = common::write_token(&root.path().join("tokens"), "dev.token", &token);
    let cred = load(&path).expect("loads");
    let shown = format!("{cred:?}");
    assert!(!shown.contains(&token), "{shown}");
    assert!(!shown.contains(&token[4..]), "{shown}");
    assert!(shown.contains(cred.fingerprint()), "{shown}");
}
