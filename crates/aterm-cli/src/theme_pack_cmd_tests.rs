// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

use super::*;
use crate::theme_cmd::run;
use crate::theme_cmd::tests::NEVER_LISTS;

fn scratch(tag: &str) -> aterm_tempfile::TempDir {
    aterm_tempfile::Builder::new()
        .prefix(&format!("aterm-cli-pack-{tag}-"))
        .tempdir()
        .expect("owned scratch dir")
}

fn theme(argv: &[&str], dir: Option<&Path>) -> Outcome {
    let args: Vec<OsString> = argv.iter().map(OsString::from).collect();
    run(&args, dir, &NEVER_LISTS)
}

fn files(dir: &Path) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(dir)
        .map(|entries| {
            entries
                .filter_map(Result::ok)
                .map(|e| e.file_name().to_string_lossy().into_owned())
                .collect()
        })
        .unwrap_or_default();
    names.sort();
    names
}

#[test]
fn pack_lists_every_theme_as_tsv() {
    let out = theme(&["pack", "--format", "tsv"], None);
    assert_eq!(out.code, 0, "{out:?}");
    let lines: Vec<&str> = out.stdout.lines().collect();
    let count = aterm_theme_pack::themes().len();
    assert_eq!(lines.len(), count + 2, "{}", out.stdout);
    for line in &lines[..=count] {
        assert_eq!(line.split('\t').count(), 8, "{line}");
    }
    let trailer: Vec<&str> = lines[count + 1].split('\t').collect();
    assert_eq!(trailer[0], "#collection");
    assert_eq!(trailer[2], aterm_theme_pack::collection().commit);

    let light = theme(&["pack", "--appearance=light", "--format=tsv"], None);
    let rows: Vec<&str> = light
        .stdout
        .lines()
        .skip(1)
        .filter(|l| !l.starts_with('#'))
        .collect();
    assert!(!rows.is_empty() && rows.len() < count);
    assert!(
        rows.iter()
            .all(|row| row.split('\t').nth(1) == Some("light"))
    );
    let text = theme(&["pack"], None);
    assert_eq!(text.code, 0);
    assert!(text.stdout.lines().count() > count);
}

#[test]
fn pack_prints_a_theme_and_the_licences() {
    let kanagawa = aterm_theme_pack::find("Kanagawa Wave").expect("in the pack");
    for id in ["Kanagawa Wave", "kanagawa wave"] {
        let out = theme(&["pack", id], None);
        assert_eq!(out.code, 0);
        assert_eq!(out.stdout, kanagawa.text);
    }
    for argv in [
        vec!["pack", "Nope"],
        vec!["pack", "a", "b"],
        vec!["pack", "--licences", "Ayu"],
        vec!["pack", "--licences", "--format", "tsv"],
        vec!["pack", "Kanagawa Wave", "--format", "tsv"],
        vec!["pack", "Kanagawa Wave", "--appearance", "light"],
        vec!["pack", "--appearance", "dark", "--appearance", "light"],
        vec!["pack", "--format", "text", "--format", "tsv"],
        vec!["pack", "--help=extra"],
        vec!["pack", "--appearance", "blue"],
        vec!["pack", "--bogus"],
    ] {
        let out = theme(&argv, None);
        assert_eq!(out.code, 2, "{argv:?}: {out:?}");
        assert!(out.stdout.is_empty(), "{argv:?}");
    }
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStringExt as _;
        let bad = OsString::from_vec(vec![0xff]);
        let out = run(
            &[OsString::from("pack"), OsString::from("--appearance"), bad],
            None,
            &NEVER_LISTS,
        );
        assert_eq!(out.code, 2);
        assert!(out.stderr.contains("is not UTF-8"), "{}", out.stderr);
    }
    for flag in ["--licences", "--licenses"] {
        let out = theme(&["pack", flag], None);
        assert_eq!(out.code, 0);
        for (file, text) in aterm_theme_pack::licences() {
            let first = text.lines().find(|l| !l.trim().is_empty()).unwrap();
            assert!(out.stdout.contains(first.trim_end()), "{file}");
        }
    }
}

#[test]
fn install_writes_keeps_and_replaces() {
    let owned = scratch("install");
    let dir = owned.path().to_path_buf();
    let iceberg = aterm_theme_pack::find("Iceberg Dark").unwrap();
    let dest = dir.join("Iceberg Dark.conf");
    let out = theme(&["install", "Iceberg Dark"], Some(&dir));
    assert_eq!(out.code, 0, "{out:?}");
    assert_eq!(std::fs::read_to_string(&dest).unwrap(), iceberg.text);
    assert!(
        out.stdout
            .contains("theme = \"dark:Iceberg Dark,light:Iceberg Light\""),
        "{}",
        out.stdout
    );
    assert_eq!(files(&dir), ["Iceberg Dark.conf"]);
    let again = theme(&["install", "iceberg dark"], Some(&dir));
    assert_eq!(again.code, 0);
    assert_eq!(std::fs::read_to_string(&dest).unwrap(), iceberg.text);

    std::fs::write(&dest, "mine").unwrap();
    let kept = theme(&["install", "Iceberg Dark"], Some(&dir));
    assert_eq!(kept.code, 0, "{kept:?}");
    assert_eq!(std::fs::read_to_string(&dest).unwrap(), "mine");
    let replaced = theme(&["install", "Iceberg Dark", "--replace"], Some(&dir));
    assert_eq!(replaced.code, 0);
    assert_eq!(std::fs::read_to_string(&dest).unwrap(), iceberg.text);
}

#[test]
fn install_takes_the_shown_name_and_keeps_an_earlier_copy() {
    let owned = scratch("names");
    let dir = owned.path().to_path_buf();
    let renamed = aterm_theme_pack::themes()
        .iter()
        .find(|theme| theme.name != theme.id)
        .expect("premise: a theme shown under another name");
    let dest = dir.join(format!("{}.conf", renamed.id));
    let out = theme(&["install", renamed.name], Some(&dir));
    assert_eq!(out.code, 0, "{out:?}");
    assert_eq!(files(&dir), [format!("{}.conf", renamed.id)]);
    assert_eq!(std::fs::read_to_string(&dest).unwrap(), renamed.text);
    let earlier = renamed.text.replacen(
        &format!("\nname = {}\n", renamed.name),
        &format!("\nname = {}\n", renamed.id),
        1,
    );
    assert_ne!(earlier, renamed.text, "premise: the name line differs");
    std::fs::write(&dest, &earlier).unwrap();
    let listed = theme(&["pack", "--format", "tsv"], Some(&dir));
    let row = listed
        .stdout
        .lines()
        .find(|line| line.split('\t').next() == Some(renamed.id))
        .expect("its row");
    assert_eq!(row.rsplit('\t').next(), Some("installed"), "{row}");
    for argv in [vec!["install", renamed.id], vec!["install", "--all"]] {
        let out = theme(&argv, Some(&dir));
        assert_eq!(out.code, 0, "{argv:?}: {out:?}");
        assert_eq!(std::fs::read_to_string(&dest).unwrap(), earlier, "{argv:?}");
    }
    assert_eq!(
        files(&dir)
            .iter()
            .filter(|file| file.starts_with(&format!("{}.", renamed.id))
                || file.starts_with(&format!("{}.", renamed.name)))
            .count(),
        1,
        "{:?}",
        files(&dir)
    );

    let gone = theme(&["install", "GitHub Light Default"], Some(&dir));
    assert_eq!(gone.code, 2, "{gone:?}");
    let standing = aterm_theme_pack::withdrawn("GitHub Light Default").expect("withdrawn");
    assert!(
        gone.stderr.contains(&format!("{standing:?}")),
        "{}",
        gone.stderr
    );
    let unknown = theme(&["install", "Nope"], Some(&dir));
    assert_eq!(unknown.code, 2);
    assert!(
        !unknown.stderr.contains(&format!("{standing:?}")),
        "{}",
        unknown.stderr
    );
}

#[test]
fn install_all_and_refusals() {
    let owned = scratch("all");
    let dir = owned.path().to_path_buf();
    for argv in [
        vec!["install", "Nope", "Iceberg Dark"],
        vec!["install", "--all", "Ayu"],
        vec!["install"],
        vec!["install", "--bogus"],
    ] {
        assert_eq!(theme(&argv, Some(&dir)).code, 2, "{argv:?}");
    }
    assert!(files(&dir).is_empty(), "a refusal wrote {:?}", files(&dir));
    let out = theme(&["install", "--all"], Some(&dir));
    assert_eq!(out.code, 0, "{out:?}");
    let mut want: Vec<String> = aterm_theme_pack::themes()
        .iter()
        .map(|t| format!("{}.conf", t.id))
        .collect();
    want.sort();
    assert_eq!(files(&dir), want);
}

#[test]
fn install_fails_without_a_folder_and_warns_past_the_cap() {
    assert_eq!(theme(&["install", "Ayu"], None).code, 1);
    let owned = scratch("cap");
    let dir = owned.path().to_path_buf();
    for n in 0..MAX_USER_THEME_FILES - 1 {
        std::fs::write(dir.join(format!("f{n}.conf")), "").unwrap();
    }
    let under = theme(&["install", "Ayu"], Some(&dir));
    assert_eq!(under.code, 0);
    assert!(under.stderr.is_empty(), "{}", under.stderr);
    let over = theme(&["install", "Snazzy"], Some(&dir));
    assert_eq!(over.code, 0);
    assert!(
        over.stderr
            .contains(&format!("{}", MAX_USER_THEME_FILES + 1)),
        "{}",
        over.stderr
    );
}
