// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `source.toml`'s `mirrors`: one `url` stays the vault's source, each mirror is another address
//! of that vault, and no entry of the list ever costs the read of the file.

use super::super::*;
use super::*;

const URL: &str = "http://vault.local:7484/v1/team";
const M1: &str = "http://192.168.1.40:7484/v1/team";
const M2: &str = "https://mirror.example/v1/team";
const KEY: [u8; 32] = [7; 32];

fn url_config(mirrors: &[&str]) -> Config {
    Config {
        source: Source::Url(URL.to_owned()),
        mirrors: mirrors.iter().map(|url| (*url).to_owned()).collect(),
        name: "team".to_owned(),
        signer: Some(KEY),
        followed: Vec::new(),
        host: "m1".to_owned(),
    }
}

fn home() -> (aterm_tempfile::TempDir, Paths) {
    let root = aterm_tempfile::tempdir().expect("tempdir");
    let paths = Paths::for_home(root.path());
    fsx::ensure_private_dir(&paths.dir).expect("dir");
    (root, paths)
}

/// Today's `source.toml` (no `mirrors`) reads with none and is written back byte for byte, and a
/// list is written right after `url` and read back in its order.
///
/// NEGATIVE CONTROL: leave the `mirrors` line out of `save_config`: the second half fails
/// "the mirrors come back in order".
#[test]
fn source_toml_keeps_mirrors_beside_one_url_and_reads_todays_file_as_before() {
    let (_root, paths) = home();
    save_config(&paths, &url_config(&[])).expect("saved");
    let today = std::fs::read_to_string(paths.source_file()).expect("read");
    assert!(!today.contains("mirrors"), "{today}");
    let read = config(&paths).expect("reads").expect("configured");
    assert!(read.mirrors.is_empty());
    save_config(&paths, &read).expect("saved again");
    assert_eq!(
        std::fs::read_to_string(paths.source_file()).expect("read"),
        today,
        "a file with no mirrors is rewritten as before"
    );

    save_config(&paths, &url_config(&[M2, M1])).expect("saved");
    let text = std::fs::read_to_string(paths.source_file()).expect("read");
    let url_at = text.find("\nurl = ").expect("url");
    let mirrors_at = text.find("\nmirrors = ").expect("mirrors");
    let name_at = text.find("\nname = ").expect("name");
    assert!(url_at < mirrors_at && mirrors_at < name_at, "{text}");
    assert_eq!(
        config(&paths).expect("reads").expect("configured").mirrors,
        [M2, M1],
        "the mirrors come back in order"
    );
}

/// No entry of `mirrors` fails the read of `source.toml`: an entry this build does not keep is
/// left out, and [`mirror_notes`] says which and why.
///
/// NEGATIVE CONTROL: make `config` fail on an entry it does not keep (`sorted(...)` returning an
/// error through `?`): fails "a bad mirrors line never costs the vault".
#[test]
fn a_bad_mirrors_line_costs_the_address_never_the_vault() {
    let (_root, paths) = home();
    save_config(&paths, &url_config(&[])).expect("saved");
    let today = std::fs::read_to_string(paths.source_file()).expect("read");
    let eight: Vec<String> = (1..=8)
        .map(|n| format!("\"http://m{n}.local:7484/v1/team\""))
        .collect();
    let entries = [
        // Kept.
        format!("\"{M1}\""),
        // Not kept, each for its own reason.
        "42".to_owned(),
        "\"http://other.local:7484/v1/elsewhere\"".to_owned(),
        format!("\"{M2}#AAAA\""),
        "\"http://VAULT.local:7484/v1/team\"".to_owned(),
        format!("\"{M1}\""),
        "\"ftp://mirror.example/v1/team\"".to_owned(),
        "{ url = \"http://m.local:7484/v1/team\" }".to_owned(),
        format!("\"http://{}.local:7484/v1/team\"", "a".repeat(600)),
    ]
    .into_iter()
    .chain(eight)
    .collect::<Vec<_>>();
    let text = today.replacen(
        "\nname = ",
        &format!("\nmirrors = [{}]\nname = ", entries.join(", ")),
        1,
    );
    std::fs::write(paths.source_file(), &text).expect("written");
    let read = config(&paths)
        .expect("a bad mirrors line never costs the vault")
        .expect("configured");
    assert_eq!(read.source, Source::Url(URL.to_owned()));
    let mut kept = vec![M1.to_owned()];
    kept.extend((1..=6).map(|n| format!("http://m{n}.local:7484/v1/team")));
    assert_eq!(read.mirrors, kept, "the first seven valid entries");
    let notes = mirror_notes(&paths);
    assert_eq!(notes.len(), entries.len() - kept.len(), "{notes:#?}");
    for why in [
        "not a URL string",
        "names the vault elsewhere",
        "no #pin",
        "the vault's url",
        "listed already",
        "only the first 7",
        "longer than",
    ] {
        assert!(
            notes.iter().any(|note| note.contains(why)),
            "{why}: {notes:#?}"
        );
    }

    for odd in ["mirrors = \"http://m.local:7484/v1/team\"", "mirrors = 3"] {
        let text = today.replacen("\nname = ", &format!("\n{odd}\nname = "), 1);
        std::fs::write(paths.source_file(), &text).expect("written");
        let read = config(&paths).expect("reads").expect("configured");
        assert!(read.mirrors.is_empty(), "{odd}");
        assert_eq!(mirror_notes(&paths).len(), 1, "{odd}");
    }

    let path = Config {
        source: Source::Path(PathBuf::from("/Volumes/Keys/team.vault")),
        mirrors: Vec::new(),
        name: "team".to_owned(),
        signer: Some(KEY),
        followed: Vec::new(),
        host: "m1".to_owned(),
    };
    save_config(&path_paths(&paths), &path).expect("saved");
    let text = std::fs::read_to_string(path_paths(&paths).source_file()).expect("read");
    let text = text.replacen("\nname = ", &format!("\nmirrors = [\"{M1}\"]\nname = "), 1);
    std::fs::write(path_paths(&paths).source_file(), &text).expect("written");
    let read = config(&path_paths(&paths))
        .expect("reads")
        .expect("configured");
    assert!(read.mirrors.is_empty(), "a vault at a path has no mirrors");
    assert!(
        mirror_notes(&path_paths(&paths))[0].contains("at a path"),
        "{:?}",
        mirror_notes(&path_paths(&paths))
    );
}

fn path_paths(paths: &Paths) -> Paths {
    let other = Paths::for_home(&paths.home.join("path-vault"));
    fsx::ensure_private_dir(&other.dir).expect("dir");
    other
}

/// A mirror a person types is a URL of this vault: a `#<key>` it carries must be the pin.
#[test]
fn a_typed_mirror_is_a_url_of_this_vault_under_its_pin() {
    let config = url_config(&[]);
    let pin = crate::b64::encode_padded(&KEY);
    assert_eq!(mirror_typed(&config, M1).expect("plain"), M1);
    assert_eq!(
        mirror_typed(&config, &format!("{M1}#{pin}")).expect("the pin"),
        M1
    );
    let other = crate::b64::encode_padded(&[8; 32]);
    let refused = mirror_typed(&config, &format!("{M1}#{other}")).expect_err("another key");
    assert!(
        refused.to_string().contains("not this vault's pin"),
        "{refused}"
    );
    for typed in [
        URL,
        "http://m.local:7484/v1/elsewhere",
        "/Volumes/Keys/team.vault",
    ] {
        assert!(mirror_typed(&config, typed).is_err(), "{typed}");
    }
}

/// [`set_mirrors`] saves over the configuration the caller read and nothing else.
///
/// NEGATIVE CONTROL: drop the comparison with `expected`: fails "a changed vault is refused".
#[test]
fn set_mirrors_saves_only_over_the_configuration_read() {
    let (_root, paths) = home();
    let read = url_config(&[]);
    save_config(&paths, &read).expect("saved");
    let saved = set_mirrors(&paths, &read, &[M1.to_owned()]).expect("set");
    assert_eq!(saved.mirrors, [M1]);
    assert_eq!(config(&paths).expect("reads"), Some(saved.clone()));
    assert!(
        set_mirrors(&paths, &read, &[M2.to_owned()]).is_err(),
        "a changed vault is refused"
    );
    assert!(
        set_mirrors(&paths, &saved, &[URL.to_owned()]).is_err(),
        "the url"
    );
    assert_eq!(config(&paths).expect("reads"), Some(saved));
}

/// A mirror nothing else names a server of the vault is read and never written to; one the
/// caller's own peers also name is a server, and is written to as one.
///
/// NEGATIVE CONTROL: have `ReadOnly::servers` keep every peer: fails "a mirror is no server".
#[test]
fn a_mirror_is_written_to_only_where_something_else_names_it_a_server() {
    let (_root, paths) = home();
    let config = url_config(&[M1, M2]);
    let peers = vec![
        M1.to_owned(),
        M2.to_owned(),
        "http://p.local:7484/v1/team".to_owned(),
    ];
    let read_only = ReadOnly::of(&paths, &config, &[]);
    assert!(read_only.holds("HTTP://192.168.1.40:7484/v1/team"));
    assert_eq!(
        read_only.servers(URL, &peers),
        [URL, "http://p.local:7484/v1/team"],
        "a mirror is no server"
    );
    let named = ReadOnly::of(&paths, &config, &[format!("{M2}#ignored")]);
    assert!(!named.holds(M2));
    assert_eq!(
        named.servers(URL, &peers),
        [URL, M2, "http://p.local:7484/v1/team"]
    );
    let none = ReadOnly::of(&paths, &url_config(&[]), &[]);
    assert_eq!(none.servers(URL, &peers).len(), 4);
}

#[test]
fn rejected_mirror_diagnostics_omit_url_credentials() {
    let credential = "synthetic-mirror-credential";
    let config = url_config(&[]);
    for url in [
        format!("https://mirror.example/v1/elsewhere?token={credential}"),
        format!("https://mirror.example/v1/team?token={credential}#bad-pin"),
        format!("https://user:{credential}@mirror.example/v1/team"),
    ] {
        let entries = aterm_toml::Value::Array(vec![aterm_toml::Value::String(url.clone())]);
        let read = sorted(&config.source, &config.name, Some(&entries));
        assert!(read.kept.is_empty());
        assert_eq!(read.dropped.len(), 1);
        assert!(!format!("{read:?}").contains(credential));
        let error = mirror_typed(&config, &url).expect_err("invalid mirror");
        assert!(!error.to_string().contains(credential));
    }
    assert_eq!(shown(M2), M2, "a usable endpoint stays recognizable");
}
