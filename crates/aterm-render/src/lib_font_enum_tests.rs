// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::face_info;

#[test]
fn face_info_matches_live_renderer_geometry() {
    let file = aterm_tempfile::manifest_dir().join("tests/fixtures/jetbrains-mono.ttf");
    let path = file.to_str().expect("a UTF-8 package directory");
    let bytes = std::fs::read(path).expect("committed JetBrains Mono fixture");
    let live =
        super::Renderer::from_bytes(&bytes, super::FaceInfo::PROBE_PX, super::Theme::default())
            .expect("fixture builds a renderer");
    let info = face_info(path).expect("face_info resolves explicit fixture path");
    assert_eq!((info.cell_width, info.cell_height), live.cell_size());
    assert_eq!(info.baseline, live.baseline());
    assert_eq!((info.cell_width, info.cell_height), (10, 21));
}

#[cfg(unix)]
#[test]
fn the_xdg_font_roots_are_searched_and_never_twice() {
    use super::font_search_dirs_in;
    use std::path::{Path, PathBuf};
    let home = Path::new("/home/u");
    let dirs = font_search_dirs_in(
        Some(home),
        Some(PathBuf::from("/data/u/fonts")),
        vec![
            PathBuf::from("/nix/profile/share/fonts"),
            PathBuf::from("/usr/share/fonts/"),
        ],
    );
    let at = |dir: &str| {
        dirs.iter()
            .position(|held| held == Path::new(dir))
            .unwrap_or_else(|| panic!("{dir} is not searched: {dirs:?}"))
    };
    assert!(at("/home/u/.local/share/fonts") < at("/data/u/fonts"));
    assert!(at("/data/u/fonts") < at("/usr/share/fonts"));
    assert!(at("/usr/share/fonts") < at("/nix/profile/share/fonts"));
    for (i, outer) in dirs.iter().enumerate() {
        for (j, inner) in dirs.iter().enumerate() {
            assert!(i == j || !inner.starts_with(outer), "{dirs:?}");
        }
    }
    let defaults = font_search_dirs_in(
        Some(home),
        Some(home.join(".local/share/fonts")),
        vec![
            PathBuf::from("/usr/local/share/fonts"),
            PathBuf::from("/usr/share/fonts/"),
        ],
    );
    assert_eq!(defaults, font_search_dirs_in(Some(home), None, Vec::new()));
    #[cfg(not(target_os = "macos"))]
    assert!(
        !defaults.iter().any(|dir| dir.ends_with("Library/Fonts")),
        "no macOS root is searched off macOS: {defaults:?}"
    );
}

#[cfg(windows)]
#[test]
fn windows_searches_its_own_font_directories_and_no_posix_root() {
    use std::path::{Path, PathBuf};
    let home = Path::new("C:\\Users\\u");
    let dirs = super::font_search_dirs_in(Some(home), None, Vec::new());
    assert_eq!(
        dirs,
        [
            home.join(".fonts"),
            home.join(".local/share/fonts"),
            home.join("AppData/Local/Microsoft/Windows/Fonts"),
            super::windows_fonts_dir(),
        ]
    );
    for dir in &dirs {
        assert!(
            dir.is_absolute(),
            "{dir:?} would be read off the current drive"
        );
    }
    let from = |windir: Option<&str>, root: Option<&str>| {
        super::windows_fonts_dir_from(windir.map(Into::into), root.map(Into::into))
    };
    assert_eq!(
        from(Some("D:\\WINNT"), Some("C:\\Windows")),
        PathBuf::from("D:\\WINNT\\Fonts")
    );
    assert_eq!(
        from(None, Some("E:\\Windows")),
        PathBuf::from("E:\\Windows\\Fonts")
    );
    assert_eq!(
        from(Some("Windows"), Some("\\Windows")),
        PathBuf::from("C:\\Windows\\Fonts")
    );
    assert_eq!(from(None, None), PathBuf::from("C:\\Windows\\Fonts"));
}

#[cfg(windows)]
#[test]
fn a_built_in_windows_row_follows_the_machines_font_directory() {
    use std::path::Path;
    let moved = |declared: &str, dir: &str| {
        super::relocate_windows_builtin_font_path(declared, Path::new(dir))
    };
    let row = "C:\\Windows\\Fonts\\msyh.ttc";
    assert_eq!(
        moved(row, "D:\\WINNT\\Fonts").as_deref(),
        Some("D:\\WINNT\\Fonts\\msyh.ttc")
    );
    assert_eq!(moved(row, "C:\\Windows\\Fonts"), None);
    assert_eq!(
        moved(row, "C:\\WINDOWS\\Fonts"),
        None,
        "the same directory, as %WINDIR% often spells it"
    );
    assert_eq!(
        moved("C:\\Windows\\Fonts\\sub\\msyh.ttc", "D:\\WINNT\\Fonts"),
        None
    );
    assert_eq!(
        moved("C:\\Windows\\FontsX\\msyh.ttc", "D:\\WINNT\\Fonts"),
        None
    );
    assert_eq!(
        moved("C:\\Users\\u\\msyh.ttc", "D:\\WINNT\\Fonts"),
        None,
        "a user's own path"
    );
    for table in [
        super::FONT_CANDIDATES,
        super::FALLBACK_CANDIDATES,
        super::SYMBOL_FALLBACK_CANDIDATES,
        super::COLOR_EMOJI_CANDIDATES,
    ] {
        let rows: Vec<&str> = table
            .iter()
            .copied()
            .filter(|row| super::builtin_candidate_is_native(row))
            .collect();
        assert!(!rows.is_empty());
        for row in rows {
            assert!(
                moved(row, "D:\\WINNT\\Fonts").is_some(),
                "{row} is not spelled under the built-in Windows root"
            );
        }
    }
    for paths in [
        super::primary_candidate_paths(),
        super::fallback_discovery_paths(),
        super::symbol_discovery_paths(),
        super::color_emoji_discovery_paths(),
    ] {
        for path in paths {
            assert!(!path.starts_with('/'), "{path} names no file on Windows");
        }
    }
}

#[cfg(windows)]
#[test]
fn han_reads_chinese_first_unless_the_user_reads_japanese_first() {
    let table: Vec<String> = super::FALLBACK_CANDIDATES
        .iter()
        .filter(|row| super::builtin_candidate_is_native(row))
        .map(|row| (*row).to_string())
        .collect();
    let at = |paths: &[String], row: &str| paths.iter().position(|p| p == row).unwrap();
    let (ja, zh) = (super::WINDOWS_JAPANESE_ROW, super::WINDOWS_CHINESE_ROW);
    let chinese = super::han_reading_order(table.clone(), false);
    assert_eq!(chinese, table, "no Japanese preference: the table's order");
    assert!(at(&chinese, zh) < at(&chinese, ja));
    let japanese = super::han_reading_order(table.clone(), true);
    assert_eq!(at(&japanese, ja) + 1, at(&japanese, zh));
    assert_eq!(at(&japanese, ja), at(&table, zh), "nothing else moved");
    let mut sorted = (japanese.clone(), table.clone());
    sorted.0.sort();
    sorted.1.sort();
    assert_eq!(sorted.0, sorted.1);
    assert_eq!(super::han_reading_order(japanese.clone(), true), japanese);
    // The chain a renderer is built on is the table's unless this user reads Japanese first.
    let live = super::fallback_discovery_paths();
    assert_eq!(
        at(&live, ja) < at(&live, zh),
        super::win_locale::user_reads_japanese_first()
    );
    for (row, _) in super::WEIGHT_SIBLING_FAMILIES.iter().copied().flatten() {
        assert!(
            super::NATIVE_SCRIPT_FALLBACK_CANDIDATES.contains(row),
            "{row}: a weight sibling that would end the chain where it stands"
        );
    }
}

#[test]
fn the_generic_monospace_name_resolves_to_the_default_face() {
    let default = super::default_primary_identity()
        .expect("a default primary loads (the bundled stack, or the host's)");
    for generic in ["monospace", "Monospace", "MONOSPACE", "mono", " mono "] {
        assert!(super::is_generic_monospace_family(generic), "{generic:?}");
        assert_eq!(
            super::resolve_family_or_bundled(generic, super::FaceTarget::REGULAR).as_ref(),
            Some(&default),
            "{generic:?}"
        );
        assert_eq!(
            super::resolve_config_font(generic, super::FaceTarget::REGULAR).as_ref(),
            Ok(&default),
            "{generic:?}"
        );
        assert_eq!(
            face_info(generic).map(|info| info.path).as_deref(),
            Some(default.path.as_str())
        );
    }
    let batch = crate::font_catalog::resolve_and_admit(&[(
        "monospace".to_string(),
        super::FaceTarget::REGULAR,
    )]);
    let admitted = batch.get(0).expect("one entry").as_ref().expect("admits");
    assert_eq!(
        (admitted.path.as_str(), admitted.index),
        (default.path.as_str(), default.index)
    );
    for family in ["Monoid", "JetBrains Mono", "monospaced", "Ubuntu Mono"] {
        assert!(!super::is_generic_monospace_family(family), "{family:?}");
    }
}
