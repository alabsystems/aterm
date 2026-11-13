// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::*;
use std::path::PathBuf;

/// The file-provider predicate is strictly narrower than its sibling: true for
/// the two sync domains, false for every other protected root.
#[test]
fn only_the_file_provider_domains_are_named_by_the_narrow_predicate() {
    let home = PathBuf::from("/Users//someone");
    let provider = [
        "/Users//someone/Library/Mobile Documents",
        "/Users//someone/Library/Mobile Documents/com~apple~CloudDocs/notes.md",
        "/Users//someone/Library/CloudStorage",
        "/Users//someone/Library/CloudStorage/SomeProvider-someone/notes.md",
    ];
    for p in provider {
        assert!(
            under_file_provider_domain(&home, Path::new(p)),
            "{p} is a file-provider domain"
        );
        assert!(
            under_protected_root(&home, Path::new(p)),
            "{p} is also protected at large"
        );
    }
    let protected_elsewhere = [
        "/Users//someone/Documents/notes.md",
        "/Users//someone/Desktop/notes.md",
        "/Users//someone/Downloads/a/b",
        "/Users//someone/Pictures/x",
        "/Users//someone/Library/Containers/com.apple.Notes/Data",
        "/Users//someone/Library/Group Containers/group.com.apple.notes",
        "/Volumes/External/notes.md",
    ];
    for p in protected_elsewhere {
        assert!(
            !under_file_provider_domain(&home, Path::new(p)),
            "{p} is protected but is not a file-provider domain"
        );
    }
    for p in [
        "/Users//someone/Library",
        "/Users//someone/Library/CloudStoragex/a",
        "/Users//someone/Library/Application Support/aterm",
        "/Users//other/Library/CloudStorage/x",
        "/Users//someone",
    ] {
        assert!(
            !under_file_provider_domain(&home, Path::new(p)),
            "{p} must not match"
        );
    }
}

/// Every protected root answers `true`; shapes that merely resemble one answer `false`.
#[test]
fn the_protected_roots_are_recognised_per_component_and_nothing_else_is() {
    let home = PathBuf::from("/Users//someone");
    let protected = [
        "/Users//someone/Documents",
        "/Users//someone/Documents/dotfiles/zshrc",
        "/Users//someone/Desktop/notes.md",
        "/Users//someone/Downloads/a/b/c",
        "/Users//someone/Pictures/x",
        "/Users//someone/Movies/x",
        "/Users//someone/Music/x",
        "/Users//someone/Library/Mobile Documents",
        "/Users//someone/Library/Mobile Documents/com~apple~CloudDocs/dotfiles/zshrc",
        "/Users//someone/Library/CloudStorage",
        "/Users//someone/Library/CloudStorage/SomeProvider-someone/dotfiles/zshrc",
        "/Users//someone/Library/Containers",
        "/Users//someone/Library/Containers/com.apple.Notes/Data/zshrc",
        "/Users//someone/Library/Group Containers",
        "/Users//someone/Library/Group Containers/group.com.apple.notes/zshrc",
        "/Volumes/External/dotfiles/zshrc",
        "/Volumes",
    ];
    for p in protected {
        assert!(
            under_protected_root(&home, Path::new(p)),
            "{p} is under a protected root"
        );
    }
    let clear = [
        "/Users//someone",
        "/Users//someone/.zshrc",
        "/Users//someone/.config/fish/config.fish",
        "/Users//someone/dotfiles/zshrc",
        "/Users//someone/Documentsx/zshrc",
        "/Users//someone/src/Documents/zshrc",
        "/Users//someone/Library",
        "/Users//someone/Library/Application Support/aterm/pkg",
        "/Users//someone/Library/Caches/aterm",
        "/Users//someone/Library/Preferences/com.apple.Terminal.plist",
        "/Users//someone/Library/Containersx/a",
        "/Users//someone/Library/Group Containersx/a",
        "/Users//someone/Library/Mobile",
        "/Users//someone/Library/CloudStoragex",
        "/Users//other/Documents/zshrc",
        "/private/tmp/Documents/zshrc",
        "/Volumesx/y",
    ];
    for p in clear {
        assert!(
            !under_protected_root(&home, Path::new(p)),
            "{p} is not under a protected root"
        );
    }
}

/// `under_app_data` matches `~/Library/Containers` and
/// `~/Library/Group Containers` per component, implies `under_protected_root`,
/// fails closed on `..` below `home`, and leaves sibling `~/Library` paths and
/// other protected folders alone.
#[test]
fn other_apps_data_is_recognised_per_component_and_nothing_else_is() {
    let home = PathBuf::from("/Users//someone");
    let app_data = [
        "/Users//someone/Library/Containers",
        "/Users//someone/Library/Containers/com.apple.Notes",
        "/Users//someone/Library/Containers/com.apple.Notes/Data/Documents/note.txt",
        "/Users//someone/Library/Group Containers",
        "/Users//someone/Library/Group Containers/group.com.apple.notes/NoteStore.sqlite",
        "/Users//someone/Library/Caches/../Containers/com.apple.Notes",
    ];
    for p in app_data {
        assert!(
            under_app_data(&home, Path::new(p)),
            "{p} is under an App Data root"
        );
        assert!(
            under_protected_root(&home, Path::new(p)),
            "{p} is also protected at large"
        );
    }
    let not_app_data = [
        "/Users//someone",
        "/Users//someone/Documents/notes.md",
        "/Users//someone/Library",
        "/Users//someone/Library/Application Support/aterm",
        "/Users//someone/Library/Caches/com.apple.Notes",
        "/Users//someone/Library/Mobile Documents/com~apple~CloudDocs",
        "/Users//someone/Library/CloudStorage/Provider",
        "/Users//someone/Library/Containersx/com.apple.Notes",
        "/Users//someone/Library/Group Containersx/group.x",
        "/Users//someone/src/Library/Containers/com.apple.Notes",
        "/Users//other/Library/Containers/com.apple.Notes",
        "/Volumes/External",
    ];
    for p in not_app_data {
        assert!(
            !under_app_data(&home, Path::new(p)),
            "{p} must not match under_app_data"
        );
    }
}

/// A guarded folder at the data volume's spelling is guarded: macOS's firmlinks reach
/// `/System/Volumes/Data/Users//x/Documents` as `/Users//x/Documents`, and a home spelled either
/// way guards the same folders. Negative control: an unguarded folder at that spelling, the
/// volume itself, and folders that merely resemble it stay clear.
#[test]
fn a_guarded_folder_at_the_data_volumes_spelling_is_guarded() {
    let home = Path::new("/Users//someone");
    let data = Path::new("/System/Volumes/Data/Users//someone");
    for p in [
        "/System/Volumes/Data/Users//someone/Documents/notes.md",
        "/System/Volumes/Data/Users//someone/Library/CloudStorage/x",
        "/System/Volumes/Data/Users//someone/Library/Containers/com.apple.Notes",
        "/System/Volumes/Data/Volumes/External",
        "/System/Volumes/Data/Users//someone/src/../Desktop",
    ] {
        assert!(under_protected_root(home, Path::new(p)), "{p}");
    }
    assert!(under_protected_root(
        data,
        Path::new("/Users//someone/Desktop/x")
    ));
    assert!(under_file_provider_domain(
        home,
        Path::new("/System/Volumes/Data/Users//someone/Library/Mobile Documents/x")
    ));
    assert!(under_app_data(
        data,
        Path::new("/Users//someone/Library/Group Containers/x")
    ));
    for p in [
        "/System/Volumes/Data",
        "/System/Volumes/Data/Users//someone",
        "/System/Volumes/Data/Users//someone/src/Documents",
        "/System/Volumes/Data/Users//other/Documents",
        "/System/Volumes/Datax/Users//someone/Documents",
        "/System/Volumes/Preboot/Volumes/x",
        "/private/System/Volumes/Data/Volumes/x",
    ] {
        assert!(!under_protected_root(home, Path::new(p)), "{p}");
    }
}

/// A folder is at or under another at any spelling macOS resolves to one folder before a look:
/// case (on macOS), the data volume's firmlinks, and the volume itself holds every folder.
/// Negative control: a sibling, a name that merely shares a prefix, and a folder inside the one
/// asked about.
#[test]
fn at_or_under_reads_every_spelling_of_one_folder() {
    let home = Path::new("/Users//someone");
    let mut holding = vec![
        "/Users//someone",
        "/Users",
        "/System/Volumes/Data/Users//someone",
        "/System/Volumes/Data/Users",
        "/System/Volumes/Data",
    ];
    if cfg!(target_os = "macos") {
        holding.extend(["/USERS", "/users/SomeOne", "/system/volumes/data/users"]);
    }
    for root in holding {
        assert!(at_or_under(Path::new(root), home), "{root}");
    }
    assert!(at_or_under(
        home,
        Path::new("/System/Volumes/Data/Users//someone/src")
    ));
    for root in [
        "/Users//someone/src",
        "/Users//some",
        "/Users//someones",
        "/Users//other",
        "/System/Volumes/Datax",
        "/private",
    ] {
        assert!(!at_or_under(Path::new(root), home), "{root}");
    }
}
