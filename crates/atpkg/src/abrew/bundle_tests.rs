// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

/// A Brewfile that is a link in a dotfiles checkout reads as brew reads it.
#[test]
#[cfg(unix)]
fn a_linked_brewfile_reads_through_its_link() {
    let tmp = aterm_tempfile::tempdir().unwrap();
    let real = tmp.path().join("dotfiles-Brewfile");
    std::fs::write(&real, "brew \"wget\"\n").unwrap();
    let link = tmp.path().join("Brewfile");
    std::os::unix::fs::symlink(&real, &link).unwrap();
    assert_eq!(read_brewfile(Some(&link)).unwrap(), "brew \"wget\"\n");
}

/// `--brewfile=-` names stdin, which `read_brewfile` reads directly: `/dev/stdin` is a link
/// and a pipe, neither of which the file reader takes.
#[test]
fn a_dash_brewfile_is_left_for_the_stdin_reader() {
    assert_eq!(deps_brewfile_path(Some("-")), Path::new("-"));
}

#[test]
#[cfg(unix)]
fn dump_keeps_on_request_and_qualified_names_only() {
    let tmp = aterm_tempfile::tempdir().unwrap();
    let p = Prefix::at(tmp.path());
    p.ensure().unwrap();
    let mk = |name: &str, version: &str, fields: &[(&str, &str)]| {
        let dir = p.keg(name, version);
        std::fs::create_dir_all(&dir).unwrap();
        let mut all: Vec<(&str, &str)> = vec![
            ("installed_by", "abrew"),
            ("name", name),
            ("version", version),
        ];
        all.extend_from_slice(fields);
        p.write_receipt(&dir, &all).unwrap();
    };
    mk("widget", "1.25", &[("installed_on_request", "true")]);
    mk("dep", "2.0", &[("installed_on_request", "false")]);
    mk(
        "taptool",
        "1.0",
        &[
            ("installed_on_request", "true"),
            ("full_name", "someone/tap/taptool"),
        ],
    );
    mk(
        "badreceipt",
        "1.0",
        &[
            ("installed_on_request", "true"),
            ("full_name", "evil\" \ntap \"bad/repo"),
        ],
    );
    mk("legacy", "0.9", &[]);
    let api = crate::abrew::api::Api::read_only(p.clone());
    let text = brewfile_dump(&p, &api, &DumpSelection::all());
    assert!(text.contains("brew \"widget\"\n"), "{text}");
    assert!(text.contains("brew \"someone/tap/taptool\"\n"), "{text}");
    assert!(text.contains("brew \"badreceipt\"\n"), "{text}");
    assert!(!text.contains("evil"), "{text}");
    assert!(text.contains("brew \"legacy\"\n"), "{text}");
    assert!(!text.contains("dep"), "dependency leaked into dump: {text}");

    let (entries, skipped) = parse_brewfile(&text);
    assert_eq!(
        skipped,
        ["brew \"someone/tap/taptool\""],
        "third-party tap formulae remain unsupported rows: {skipped:?}"
    );
    for entry in &entries {
        assert!(
            entry_installed(&p, entry),
            "dumped core entry {entry:?} must be recognized as installed"
        );
    }
    assert!(
        !entry_installed(&p, &BrewfileEntry::Brew("taptool".into())),
        "third-party tap keg must not satisfy a core formula entry"
    );
    assert!(entry_installed(
        &p,
        &BrewfileEntry::Brew("homebrew/core/widget".into())
    ));
}

#[test]
fn cleanup_plan_keeps_entries_and_their_dependencies() {
    let tmp = aterm_tempfile::tempdir().unwrap();
    let p = Prefix::at(tmp.path());
    p.ensure().unwrap();
    let keg = |name: &str, receipt: &str| {
        let dir = p.keg(name, "1.0");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("INSTALL_RECEIPT.json"), receipt).unwrap();
    };
    keg(
        "alpha",
        r#"{"installed_by":"abrew","name":"alpha","version":"1.0","installed_on_request":"true","runtime_dependencies":[{"full_name":"beta"}]}"#,
    );
    keg(
        "beta",
        r#"{"installed_by":"abrew","name":"beta","version":"1.0","installed_on_request":"false"}"#,
    );
    keg(
        "gamma",
        r#"{"installed_by":"abrew","name":"gamma","version":"1.0","installed_on_request":"true"}"#,
    );
    let cask_dir = p.cask_dir("keptcask", "1.0");
    std::fs::create_dir_all(&cask_dir).unwrap();
    std::fs::write(
        cask_dir.join("INSTALL_RECEIPT.json"),
        r#"{"installed_by":"abrew","token":"keptcask","version":"1.0","installed_on_request":"true"}"#,
    )
    .unwrap();
    let cask_dir = p.cask_dir("straycask", "1.0");
    std::fs::create_dir_all(&cask_dir).unwrap();
    std::fs::write(
        cask_dir.join("INSTALL_RECEIPT.json"),
        r#"{"installed_by":"abrew","token":"straycask","version":"1.0","installed_on_request":"true"}"#,
    )
    .unwrap();
    p.rebuild_state().unwrap();
    let api = crate::abrew::api::Api::read_only(p.clone());
    let entries = parse_brewfile_cleanup("brew \"alpha\"\ncask \"keptcask\"\n").unwrap();
    let sel = CleanupSelection {
        formulae: true,
        casks: true,
        taps: true,
        extensions_enabled: false,
    };
    let plan = cleanup_plan(&p, &api, &entries, &sel);
    assert_eq!(plan.formulae, ["gamma"], "beta kept as alpha's dep");
    assert_eq!(plan.casks, ["straycask"]);
    assert!(plan.taps.is_empty());
    let trusted =
        parse_brewfile_cleanup("brew \"alpha\", trusted: true\ntap \"me/tools\", trusted: true\n")
            .unwrap();
    assert!(trusted.iter().all(|e| e.trusted == Some(TrustedOpt::True)));
    assert!(entries.iter().all(|e| e.trusted.is_none()));
}

#[test]
fn brewfile_deps_parse_reads_options_and_ignores_other_entries() {
    let (entries, skipped) = parse_brewfile_deps(
        "tap \"me/tools\"\n\
         brew \"Wget\", link: false\n\
         brew \"homebrew/homebrew/curl\"\n\
         brew \"Someone/Homebrew-Tap/Tool\", args: [\"HEAD\"]\n\
         cask \"homebrew/cask/Firefox\"\n\
         mas \"Xcode\", id: 497799835\n\
         go \"github.com/example/tool@latest\"\n\
         cask_args appdir: \"/Applications\"\n\
         brew \"jq\" if OS.mac?\n\
         whatever \"nope\"\n",
    );
    assert_eq!(
        entries,
        [
            (Kind::Formula, "wget".into()),
            (Kind::Formula, "curl".into()),
            (Kind::Formula, "someone/tap/tool".into()),
            (Kind::Cask, "firefox".into()),
        ]
    );
    assert_eq!(
        skipped,
        ["brew \"jq\" if OS.mac?", "whatever \"nope\""],
        "conditionals and unknown directives stay non-native"
    );
}

#[test]
fn brewfile_parse() {
    let (entries, skipped) = parse_brewfile(
        "tap \"me/tools\"\nbrew \"wget\"\nbrew \"homebrew/homebrew-core/curl\"\ncask \"homebrew/cask/firefox\"\nbrew \"someone/homebrew-tap/taptool\"\nmas \"Xcode\", id: 1\n",
    );
    assert_eq!(
        entries,
        [
            BrewfileEntry::Tap("me/tools".into()),
            BrewfileEntry::Brew("wget".into()),
            BrewfileEntry::Brew("curl".into()),
            BrewfileEntry::Cask("firefox".into()),
        ]
    );
    assert_eq!(skipped.len(), 2);
    let (entries, skipped) = parse_brewfile(
        "brew 'openssl@3' # note\ntap \"me/homebrew-tools\"\nbrew \"wget\", args: [\"HEAD\"]\nbrew \"wget\" if true\nbrew\"wget\"\ncask \"../outside\"\nbrew \"two/parts\"\ncask \"homebrew/core/firefox\"\ntap \"../foo\"\nbrew 'wget\"\n",
    );
    assert_eq!(
        entries,
        [
            BrewfileEntry::Brew("openssl@3".into()),
            BrewfileEntry::Tap("me/tools".into())
        ]
    );
    assert_eq!(
        skipped.len(),
        8,
        "unsupported syntax is never partially executed"
    );
}

#[test]
fn brewfile_list_parse_matches_brew_display_names() {
    let rows = parse_brewfile_list(
        "cask_args appdir: \"/Applications\"\n\
         brew \"homebrew/core/wget\"\n\
         brew \"someone/tools/foo\"\n\
         cask \"homebrew/cask/iterm2\"\n\
         cask \"someone/iterm2\"\n\
         mas \"Xcode\", id: 497799835\n\
         brew(\"sqlite\")\n\
         brew 'UPPER'\n\
         tap \"User/Homebrew-Tools\"\n\
         brew \"homebrew/homebrew/foo\"\n\
         vscode \"ms-python.python\"\n",
    )
    .expect("listable rows");
    assert_eq!(
        rows,
        [
            ListEntry::Brew("homebrew/core/wget".into()),
            ListEntry::Brew("someone/tools/foo".into()),
            ListEntry::Cask("iterm2".into()),
            ListEntry::Cask("someone/iterm2".into()),
            ListEntry::Ext("mas", "Xcode".into()),
            ListEntry::Brew("sqlite".into()),
            ListEntry::Brew("upper".into()),
            ListEntry::Tap("user/tools".into()),
            ListEntry::Brew("foo".into()),
            ListEntry::Ext("vscode", "ms-python.python".into()),
        ]
    );
    assert!(parse_brewfile_list("brew \"x\"\nunknown \"y\"\n").is_none());
    assert!(parse_brewfile_list("brew \"x\" if OS.mac?\n").is_none());
}
