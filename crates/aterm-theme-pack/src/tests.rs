// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Theme pack generation, notice, manifest, and installation round-trip tests.

use super::*;

fn pack_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../contrib/themes/pack/")
}

fn upstream_of(theme: &PackTheme) -> String {
    let path = pack_dir().join("upstream").join(theme.upstream);
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()))
}

fn licences_of(theme: &PackTheme) -> Vec<&'static str> {
    theme
        .licence_files
        .iter()
        .map(|file| licence_text(file).unwrap_or_else(|| panic!("{file} is embedded")))
        .collect()
}

fn collection_licence() -> &'static str {
    licence_text(collection().licence_file).expect("the collection's licence is embedded")
}

fn derived(theme: &PackTheme, upstream: &str) -> String {
    derive(theme, upstream, &licences_of(theme), collection_licence())
        .unwrap_or_else(|why| panic!("{why}"))
}

fn scratch(tag: &str) -> aterm_tempfile::TempDir {
    aterm_tempfile::Builder::new()
        .prefix(&format!("aterm-theme-pack-{tag}-"))
        .tempdir()
        .expect("owned scratch dir")
}

#[test]
fn pack_files_are_derived_from_their_upstream() {
    assert!(!themes().is_empty());
    let bless = std::env::var_os("ATERM_BLESS_GOLDEN").is_some();
    let attribution = pack_dir().join("ATTRIBUTION.md");
    for theme in themes() {
        let want = derived(theme, &upstream_of(theme));
        if bless {
            std::fs::write(pack_dir().join(format!("{}.conf", theme.id)), &want).unwrap();
        } else {
            assert!(
                theme.text == want,
                "{}.conf is not what its upstream derives; regenerate it with \
                 ATERM_BLESS_GOLDEN=1 targo --unverified test -p aterm-theme-pack",
                theme.id
            );
        }
    }
    if bless {
        std::fs::write(&attribution, notices()).unwrap();
    } else {
        assert!(
            std::fs::read_to_string(&attribution).ok() == Some(notices()),
            "ATTRIBUTION.md is not notices(); regenerate it with ATERM_BLESS_GOLDEN=1"
        );
    }

    let first = &themes()[0];
    let upstream = upstream_of(first);
    let changed: String = upstream
        .lines()
        .map(|line| {
            if line.replace(' ', "").starts_with("palette=1=") {
                "palette = 1=#123456".to_string()
            } else {
                line.to_string()
            }
        })
        .collect::<Vec<_>>()
        .join("\n");
    assert_ne!(changed, upstream, "the control changed a line");
    assert_ne!(derived(first, &changed), derived(first, &upstream));
}

#[test]
fn every_pack_file_carries_its_notices() {
    for theme in themes() {
        for licence in licences_of(theme) {
            assert!(
                carries_notice(theme.text, theme.licence, licence),
                "{}: missing its {} notice",
                theme.id,
                theme.licence
            );
        }
        assert!(
            carries_notice(theme.text, "MIT", collection_licence()),
            "{}: missing the collection's notice",
            theme.id
        );
    }
    let theme = &themes()[0];
    let licence = licences_of(theme)[0];
    let line = notice_lines(theme.licence, licence)
        .into_iter()
        .find(|line| !line.trim().is_empty())
        .expect("a notice line");
    let cut = theme
        .text
        .replacen(&format!("# {}\n", line.trim_end()), "", 1);
    assert_ne!(cut, theme.text, "the control removed a line");
    assert!(!carries_notice(&cut, theme.licence, licence));
}

#[test]
fn pairs_are_symmetric_and_cross_appearance() {
    let mut pairs = 0;
    for theme in themes() {
        let Some(pair) = theme.pair else {
            continue;
        };
        pairs += 1;
        let other = match find(pair) {
            Some(other) => {
                assert_eq!(other.pair, Some(theme.id), "{} <-> {pair}", theme.id);
                other.appearance
            }
            None => {
                scheme::builtin_theme(pair)
                    .unwrap_or_else(|| panic!("{}: pair {pair:?} names nothing", theme.id))
                    .appearance
            }
        };
        assert_ne!(other, theme.appearance, "{} <-> {pair}", theme.id);
    }
    assert!(pairs > 0);
}

#[test]
fn pack_ids_are_free_and_files_declare_their_row() {
    for theme in themes() {
        assert!(scheme::builtin_theme(theme.id).is_none(), "{}", theme.id);
        assert!(
            scheme::builtin_theme(theme.name).is_none(),
            "{}",
            theme.name
        );
        let parsed = scheme::parse_theme_str(theme.text).expect("it parses");
        assert!(parsed.appearance_declared, "{}", theme.id);
        assert_eq!(parsed.scheme.appearance, theme.appearance, "{}", theme.id);
        assert_eq!(parsed.author.as_deref(), Some(theme.author), "{}", theme.id);
        assert_eq!(parsed.scheme.name, theme.name, "{}", theme.id);
    }
}

#[test]
fn find_answers_the_id_then_the_name_shown() {
    let renamed = themes()
        .iter()
        .find(|theme| theme.name != theme.id)
        .expect("premise: a theme shown under another name");
    for spelling in [
        renamed.id.to_string(),
        renamed.name.to_string(),
        renamed.name.to_ascii_lowercase(),
    ] {
        assert_eq!(
            find(&spelling).map(|theme| theme.id),
            Some(renamed.id),
            "{spelling}"
        );
    }
    assert_eq!(find("Tokyo Night"), None);
    assert!(scheme::builtin_theme("Tokyo Night").is_some(), "premise");
}

#[test]
fn install_round_trips_in_a_scratch_folder() {
    let owned = scratch("install");
    let dir = owned.path().to_path_buf();
    let theme = find("Iceberg Dark").expect("a pack theme");
    let dest = dir.join("Iceberg Dark.conf");
    assert_eq!(state_in(&dir, theme), PackState::Absent);
    assert_eq!(
        install(theme, Some(&dir), false),
        Ok(PackInstall::Wrote(dest.clone()))
    );
    assert_eq!(std::fs::read(&dest).unwrap(), theme.text.as_bytes());
    assert_eq!(state_in(&dir, theme), PackState::Installed);
    assert_eq!(
        install(theme, Some(&dir), false),
        Ok(PackInstall::Unchanged(dest.clone()))
    );

    std::fs::write(&dest, format!("{}# mine\n", theme.text)).unwrap();
    assert_eq!(state_in(&dir, theme), PackState::Modified);
    assert_eq!(
        install(theme, Some(&dir), false),
        Err(InstallError::Exists(dest.clone()))
    );
    assert_eq!(
        install(theme, Some(&dir), true),
        Ok(PackInstall::Wrote(dest.clone()))
    );
    assert_eq!(std::fs::read(&dest).unwrap(), theme.text.as_bytes());

    let other = find("Iceberg Light").expect("a pack theme");
    let variant = dir.join(format!("{}.conf", other.id.to_ascii_lowercase()));
    std::fs::write(&variant, "background = #000000\nforeground = #ffffff\n").unwrap();
    assert_eq!(
        state_in(&dir, other),
        PackState::CaseVariant(variant.clone())
    );
    assert_eq!(
        install(other, Some(&dir), true),
        Err(InstallError::Failed(
            scheme_install::case_collision_message(&variant, other.id)
        ))
    );
    assert!(matches!(
        install(theme, None, false),
        Err(InstallError::Failed(_))
    ));

    let builtin = PackTheme {
        id: "nord",
        ..*theme
    };
    assert!(matches!(
        install(&builtin, Some(&dir), false),
        Err(InstallError::Refused(_))
    ));
    assert!(!dir.join("nord.conf").exists());
}

#[test]
fn an_earlier_copy_reads_installed_and_an_edit_does_not() {
    let owned = scratch("earlier");
    let dir = owned.path().to_path_buf();
    let theme = themes()
        .iter()
        .find(|theme| theme.name != theme.id)
        .expect("premise: a theme shown under another name");
    let dest = dir.join(format!("{}.conf", theme.id));
    let earlier = theme.text.replacen(
        &format!("\nname = {}\n", theme.name),
        &format!("\nname = {}\n", theme.id),
        1,
    );
    assert_ne!(earlier, theme.text, "premise: the name line differs");
    std::fs::write(&dest, &earlier).unwrap();
    assert_eq!(state_in(&dir, theme), PackState::Installed);
    assert_eq!(
        install(theme, Some(&dir), false),
        Ok(PackInstall::Unchanged(dest.clone()))
    );
    assert_eq!(std::fs::read_to_string(&dest).unwrap(), earlier);
    assert_eq!(
        install(theme, Some(&dir), true),
        Ok(PackInstall::Wrote(dest.clone()))
    );
    assert_eq!(std::fs::read(&dest).unwrap(), theme.text.as_bytes());

    let renamed = theme.text.replacen(
        &format!("\nname = {}\n", theme.name),
        "\nname = My own storm\n",
        1,
    );
    assert_ne!(renamed, theme.text, "premise: the name line differs");
    std::fs::write(&dest, &renamed).unwrap();
    assert_eq!(state_in(&dir, theme), PackState::Modified);
    assert_eq!(
        install(theme, Some(&dir), false),
        Err(InstallError::Exists(dest.clone()))
    );
    assert_eq!(std::fs::read_to_string(&dest).unwrap(), renamed);

    let background = scheme::parse_theme_str(theme.text)
        .expect("it parses")
        .scheme
        .background;
    let line = format!(
        "\nbackground = #{:02x}{:02x}{:02x}\n",
        background.r, background.g, background.b
    );
    let edited = earlier.replacen(&line, "\nbackground = #123456\n", 1);
    assert_ne!(edited, earlier, "premise: the control changed a color");
    std::fs::write(&dest, &edited).unwrap();
    assert_eq!(state_in(&dir, theme), PackState::Modified);
    assert_eq!(
        install(theme, Some(&dir), false),
        Err(InstallError::Exists(dest.clone()))
    );
    assert_eq!(std::fs::read_to_string(&dest).unwrap(), edited);
}

#[test]
fn install_refuses_an_id_that_leaves_the_requested_folder() {
    let owned = scratch("id");
    let dir = owned.path().join("themes");
    std::fs::create_dir(&dir).unwrap();
    match std::fs::create_dir(dir.join("...")) {
        Err(e) if cfg!(windows) && e.kind() == std::io::ErrorKind::AlreadyExists => {}
        made => made.unwrap(),
    }
    let ordinary = find("Iceberg Dark").expect("a pack theme");
    assert!(matches!(
        install(ordinary, Some(&dir), false),
        Ok(PackInstall::Wrote(_))
    ));
    assert_eq!(
        std::fs::read(dir.join("Iceberg Dark.conf")).unwrap(),
        ordinary.text.as_bytes()
    );

    let outside = owned.path().join("outside.conf");
    std::fs::write(&outside, b"keep outside").unwrap();
    let invalid = PackTheme {
        id: "../outside",
        ..*ordinary
    };
    let result = install(&invalid, Some(&dir), true);
    let kept = std::fs::read(&outside).unwrap() == b"keep outside";
    assert!(
        matches!(result, Err(InstallError::Refused(_))) && kept,
        "invalid id: result={result:?}, kept outside={kept}"
    );
}
