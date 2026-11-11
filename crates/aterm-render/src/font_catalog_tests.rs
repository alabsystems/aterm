// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

#[test]
fn search_dirs_are_never_nested_so_the_walk_visits_a_file_once() {
    let dirs = crate::font_search_dirs();
    for (i, outer) in dirs.iter().enumerate() {
        for (j, inner) in dirs.iter().enumerate() {
            assert!(
                i == j || !inner.starts_with(outer),
                "font search dir {} lies inside {} — the recursive walk already                      visits it, and listing it as a root walks every file under it twice",
                inner.display(),
                outer.display()
            );
        }
    }
}

const JETBRAINS: &[u8] = include_bytes!("../tests/fixtures/jetbrains-mono.ttf");

fn with_weight(weight: u16) -> Vec<u8> {
    let mut bytes = JETBRAINS.to_vec();
    let tables = usize::from(u16::from_be_bytes([bytes[4], bytes[5]]));
    let record = (0..tables)
        .map(|index| 12 + 16 * index)
        .find(|record| &bytes[*record..*record + 4] == b"OS/2")
        .expect("the fixture has an OS/2 table");
    let offset = u32::from_be_bytes(bytes[record + 8..record + 12].try_into().unwrap()) as usize;
    bytes[offset + 4..offset + 6].copy_from_slice(&weight.to_be_bytes());
    bytes
}

#[test]
fn monospace_families_list_what_the_resolvers_find() {
    let root = fixture("families");
    std::fs::write(root.join("JetBrainsMono-Regular.ttf"), JETBRAINS).unwrap();
    std::fs::write(root.join("JetBrainsMono-Bold.ttf"), with_weight(700)).unwrap();
    std::fs::write(root.join("NotoColorEmoji.ttf"), b"not read").unwrap();
    std::fs::write(root.join("notes.txt"), b"not a font").unwrap();
    let files = Catalog::scan(std::slice::from_ref(&*root), Limits::default()).files;
    let regular = root.join("JetBrainsMono-Regular.ttf");
    let families = monospace_families_in(&files, &[regular.to_string_lossy().into_owned()]);
    assert!(
        families
            .installed
            .iter()
            .any(|family| family == "JetBrains Mono"),
        "{families:?}"
    );
    assert_eq!(families.automatic.as_deref(), Some("JetBrains Mono"));
    assert!(families.automatic_has_ligatures);
    assert_eq!(families.emoji, ["NotoColorEmoji"]);
    assert_eq!(families.bold.len(), 1, "{families:?}");
}

fn with_family(name: &str) -> Vec<u8> {
    const FROM: &str = "JetBrains Mono";
    let padded = format!("{name:<14}");
    assert_eq!(padded.chars().count(), FROM.chars().count(), "{name:?}");
    let utf16 =
        |text: &str| -> Vec<u8> { text.encode_utf16().flat_map(u16::to_be_bytes).collect() };
    let mut bytes = JETBRAINS.to_vec();
    for (from, to) in [
        (utf16(FROM), utf16(&padded)),
        (FROM.as_bytes().to_vec(), padded.as_bytes().to_vec()),
    ] {
        let mut at = 0;
        while at + from.len() <= bytes.len() {
            if bytes[at..at + from.len()] == from[..] {
                bytes[at..at + from.len()].copy_from_slice(&to);
                at += from.len();
            } else {
                at += 1;
            }
        }
    }
    bytes
}

#[test]
fn a_hidden_system_face_is_listed_only_under_a_public_family() {
    let root = fixture("hidden");
    std::fs::write(root.join("Placeholder.ttf"), with_family(".LastResort")).unwrap();
    std::fs::write(root.join("SFNSMono.ttf"), with_family(".SF NS Mono")).unwrap();
    let files = Catalog::scan(std::slice::from_ref(&*root), Limits::default()).files;
    let families = monospace_families_in(&files, &[]);
    assert_eq!(families.installed, ["SF Mono"], "{families:?}");
    assert!(crate::is_hidden_face_name(".LastResort"));
    assert!(!crate::is_hidden_face_name(".SF NS Mono"));
    assert_eq!(crate::public_face_name(".SF NS Mono"), "SF Mono");
    assert_eq!(crate::public_face_name(".LastResort"), ".LastResort");
}

#[test]
fn a_family_resolves_to_its_regular_face_not_the_first_file() {
    let root = fixture("regular-face");
    std::fs::write(root.join("JetBrainsMono-Bold.ttf"), with_weight(700)).unwrap();
    std::fs::write(root.join("JetBrainsMono-Regular.ttf"), JETBRAINS).unwrap();
    let batch = resolve_and_admit_in_dirs(
        &[("JetBrains Mono".to_string(), FaceTarget::REGULAR)],
        std::slice::from_ref(&*root),
        Limits::default(),
    );
    let admitted = batch.get(0).unwrap().as_ref().expect("the family resolves");
    assert!(
        admitted.path.ends_with("JetBrainsMono-Regular.ttf"),
        "resolved {}",
        admitted.path
    );
}

/// A family installed as a variable face plus links that name its weights
/// (Ubuntu's `fonts-ubuntu`: `UbuntuMono-B.ttf` and `-R.ttf` link to
/// `UbuntuMono[wght].ttf`) resolves to the file itself: every one of them
/// is the regular default instance, and the link that sorts first named a
/// Bold file as the regular face.
#[cfg(unix)]
#[test]
fn a_family_resolves_to_its_file_not_a_link_that_sorts_first() {
    let root = fixture("linked-weights");
    let file = root.join("JetBrainsMono[wght].ttf");
    std::fs::write(&file, JETBRAINS).unwrap();
    for alias in ["JetBrainsMono-B.ttf", "JetBrainsMono-R.ttf"] {
        std::os::unix::fs::symlink("JetBrainsMono[wght].ttf", root.join(alias)).unwrap();
    }
    let request = [("JetBrains Mono".to_string(), FaceTarget::REGULAR)];
    let batch =
        resolve_and_admit_in_dirs(&request, std::slice::from_ref(&*root), Limits::default());
    let admitted = batch.get(0).unwrap().as_ref().expect("the family resolves");
    assert_eq!(admitted.path, file.to_string_lossy());
    assert_eq!(
        super::super::resolve_by_name_table_in(
            Catalog::scan(std::slice::from_ref(&*root), Limits::default()).files,
            "jetbrainsmono",
            FaceTarget::REGULAR,
        )
        .map(|face| face.path)
        .as_deref(),
        Some(&*file.to_string_lossy()),
        "the by-name resolver `show-face` reports agrees"
    );

    std::fs::rename(&file, root.join("elsewhere.bin")).unwrap();
    for alias in ["JetBrainsMono-B.ttf", "JetBrainsMono-R.ttf"] {
        std::fs::remove_file(root.join(alias)).unwrap();
        std::os::unix::fs::symlink("elsewhere.bin", root.join(alias)).unwrap();
    }
    let batch =
        resolve_and_admit_in_dirs(&request, std::slice::from_ref(&*root), Limits::default());
    let admitted = batch
        .get(0)
        .unwrap()
        .as_ref()
        .expect("a link still resolves");
    assert!(
        admitted.path.ends_with("JetBrainsMono-B.ttf"),
        "{}",
        admitted.path
    );
    drop(root);

    let store = fixture("linked-weights-store");
    let (bold, regular) = (
        store.join("JetBrainsMono-Bold.ttf"),
        store.join("JetBrainsMono-Regular.ttf"),
    );
    std::fs::write(&bold, with_weight(700)).unwrap();
    std::fs::write(&regular, JETBRAINS).unwrap();
    let profile = fixture("linked-weights-profile");
    for link in ["a.ttf", "c.ttf", "d.ttf", "e.ttf"] {
        std::os::unix::fs::symlink(&bold, profile.join(link)).unwrap();
    }
    let link = profile.join("b.ttf");
    let stow = Path::new("..")
        .join(store.file_name().unwrap())
        .join("JetBrainsMono-Regular.ttf");
    for target in [regular.clone(), stow] {
        let _ = std::fs::remove_file(&link);
        std::os::unix::fs::symlink(&target, &link).unwrap();
        let batch =
            resolve_and_admit_in_dirs(&request, std::slice::from_ref(&*profile), Limits::default());
        let admitted = batch.get(0).unwrap().as_ref().expect("the family resolves");
        assert_eq!(admitted.path, link.to_string_lossy(), "{target:?}");
        assert_eq!(
            batch.stats.name_files, 2,
            "a link to {target:?} is the regular face's only path here, so it \
             ends the search; the fonts after it were read"
        );
        assert_eq!(
            super::super::resolve_by_name_table_in(
                Catalog::scan(std::slice::from_ref(&*profile), Limits::default()).files,
                "jetbrainsmono",
                FaceTarget::REGULAR,
            )
            .map(|face| face.path)
            .as_deref(),
            Some(&*link.to_string_lossy()),
            "{target:?}"
        );
    }
}

#[test]
fn a_regular_face_after_a_nested_directory_still_wins_its_directory() {
    let user = fixture("regular-interleaved-user");
    let system = fixture("regular-interleaved-system");
    let bold = user.join("A-Bold.ttf");
    let regular = user.join("Z-Regular.ttf");
    let nested = user.join("M-vendor");
    std::fs::create_dir(&nested).unwrap();
    std::fs::write(&bold, with_weight(700)).unwrap();
    std::fs::write(nested.join("Regular.ttf"), JETBRAINS).unwrap();
    std::fs::write(&regular, JETBRAINS).unwrap();
    std::fs::write(system.join("Regular.ttf"), JETBRAINS).unwrap();
    let roots = [user.to_path_buf(), system.to_path_buf()];
    let request = [("JetBrains Mono".to_string(), FaceTarget::REGULAR)];
    let winner = |batch: &Batch| batch.get(0).unwrap().as_ref().unwrap().path.clone();
    let batch = resolve_and_admit_in_dirs(&request, &roots, Limits::default());
    assert_eq!(
        winner(&batch),
        regular.to_string_lossy().into_owned(),
        "descending into another directory must not finalize the first bold face"
    );

    std::fs::remove_file(&regular).unwrap();
    let batch = resolve_and_admit_in_dirs(&request, &roots, Limits::default());
    assert_eq!(winner(&batch), bold.to_string_lossy().into_owned());
}

#[cfg(unix)]
#[test]
fn the_family_memo_observes_an_atomic_replacement_at_the_same_path() {
    let root = fixture("family-replacement");
    let path = root.join("Regular.ttf");
    let replacement = root.join(".replacement");
    std::fs::write(&path, JETBRAINS).unwrap();
    std::fs::write(&replacement, with_weight(700)).unwrap();
    std::fs::File::open(&*root)
        .unwrap()
        .set_times(
            std::fs::FileTimes::new()
                .set_modified(std::time::UNIX_EPOCH + std::time::Duration::from_secs(946_684_800)),
        )
        .unwrap();
    let before_mtime = dir_mtime(&root).unwrap();
    let roots = [root.to_path_buf()];
    let automatic = [path.to_string_lossy().into_owned()];
    let memo = std::sync::Mutex::new(None);
    let first = memoized_monospace_families(&roots, &automatic, &memo);
    assert_eq!(first.automatic.as_deref(), Some("JetBrains Mono"));
    assert!(first.bold.is_empty());
    let again = memoized_monospace_families(&roots, &automatic, &memo);
    assert_eq!(
        *again, *first,
        "an unchanged inventory preserves its answer"
    );
    let before_files = memoized_font_files(&roots);

    std::fs::rename(replacement, &path).unwrap();
    assert_ne!(dir_mtime(&root).unwrap(), before_mtime);
    let (after_files, walked) = memoized_font_files_walked(&roots);
    assert!(walked, "the existing directory freshness path re-walks");
    assert_eq!(before_files, after_files, "no font pathname changed");
    let refreshed = memoized_monospace_families(&roots, &automatic, &memo);
    assert_eq!(
        refreshed.bold.len(),
        1,
        "the replacement is now a bold face despite the identical file list"
    );
}

const JB_REGULAR: &[u8] = include_bytes!("../assets/bundled/JetBrainsMono-Regular.ttf");
const JB_BOLD: &[u8] = include_bytes!("../assets/bundled/JetBrainsMono-Bold.ttf");
const JB_ITALIC: &[u8] = include_bytes!("../assets/bundled/JetBrainsMono-Italic.ttf");
const JB_BOLD_ITALIC: &[u8] = include_bytes!("../assets/bundled/JetBrainsMono-BoldItalic.ttf");

fn resolved_face(roots: &[PathBuf], request: &str, target: FaceTarget) -> (String, u32) {
    let batch =
        resolve_and_admit_in_dirs(&[(request.to_string(), target)], roots, Limits::default());
    let admitted = batch
        .get(0)
        .unwrap()
        .as_ref()
        .unwrap_or_else(|error| panic!("{request}: {error}"));
    let name = Path::new(&admitted.path)
        .file_name()
        .unwrap()
        .to_string_lossy()
        .into_owned();
    (name, admitted.index)
}

#[test]
fn a_collection_resolves_each_request_to_its_own_face() {
    let collection = ttc_of(&[JB_BOLD, JB_REGULAR]);
    let by_name = fixture("collection-by-name");
    std::fs::write(by_name.join("Mono.ttc"), &collection).unwrap();
    let by_stem = fixture("collection-by-stem");
    std::fs::write(by_stem.join("JetBrainsMono.ttc"), &collection).unwrap();
    for root in [&*by_name, &*by_stem] {
        let roots = std::slice::from_ref(root);
        assert_eq!(
            resolved_face(roots, "JetBrains Mono", FaceTarget::REGULAR).1,
            1,
            "the family is its Regular face ({})",
            root.display()
        );
        assert_eq!(
            resolved_face(roots, "JetBrains Mono", FaceTarget::BOLD).1,
            0,
            "the bold slot takes the family's Bold"
        );
        assert_eq!(
            resolved_face(roots, "JetBrains Mono Bold", FaceTarget::REGULAR).1,
            0,
            "a face's full name is that face, whatever the slot"
        );
        assert_eq!(
            resolved_face(roots, "JetBrainsMono-Regular", FaceTarget::BOLD).1,
            1,
            "a face's PostScript name is that face"
        );
    }
    let (_, index) = resolved_face(
        std::slice::from_ref(&*by_name),
        "JetBrains Mono",
        FaceTarget::REGULAR,
    );
    let face = aterm_font::Face::parse(&collection, index).unwrap();
    assert_eq!(face.weight().to_number(), 400);
}

#[test]
fn every_face_of_a_collection_is_listed_under_its_style_and_resolves_back() {
    let root = fixture("collection-listing");
    std::fs::write(
        root.join("Mono.ttc"),
        ttc_of(&[JB_REGULAR, JB_BOLD, JB_ITALIC, JB_BOLD_ITALIC]),
    )
    .unwrap();
    let files = Catalog::scan(std::slice::from_ref(&*root), Limits::default()).files;
    let families = monospace_families_with_bundled(&files, &[], &[]);
    assert_eq!(families.installed, ["JetBrains Mono"], "{families:?}");
    assert_eq!(families.bold, ["JetBrains Mono Bold"], "{families:?}");
    assert_eq!(families.italic, ["JetBrains Mono Italic"], "{families:?}");
    assert_eq!(
        families.bold_italic,
        ["JetBrains Mono Bold Italic"],
        "{families:?}"
    );
    let roots = std::slice::from_ref(&*root);
    for (slot, (name, index)) in [
        (&families.bold[0], 1),
        (&families.italic[0], 2),
        (&families.bold_italic[0], 3),
    ]
    .into_iter()
    .enumerate()
    {
        assert_eq!(
            resolved_face(roots, name, FaceTarget::for_slot(slot)),
            ("Mono.ttc".to_string(), index),
            "{name}"
        );
        assert_eq!(
            resolved_face(roots, "JetBrains Mono", FaceTarget::for_slot(slot)).1,
            index
        );
    }

    let bold_first = fixture("collection-bold-first");
    std::fs::write(bold_first.join("Mono.ttc"), ttc_of(&[JB_BOLD, JB_REGULAR])).unwrap();
    let files = Catalog::scan(std::slice::from_ref(&*bold_first), Limits::default()).files;
    let families = monospace_families_with_bundled(&files, &[], &[]);
    assert_eq!(families.installed, ["JetBrains Mono"], "{families:?}");
    assert_eq!(families.bold, ["JetBrains Mono Bold"], "{families:?}");
}

fn without_gsub(bytes: &[u8]) -> Vec<u8> {
    let mut bytes = bytes.to_vec();
    let tables = usize::from(u16::from_be_bytes([bytes[4], bytes[5]]));
    let record = (0..tables)
        .map(|index| 12 + 16 * index)
        .find(|record| &bytes[*record..*record + 4] == b"GSUB")
        .expect("the fixture has a GSUB table");
    bytes[record + 3] = b'c';
    bytes
}

#[test]
fn every_listed_family_records_its_ligatures() {
    let with = fixture("ligatures-with");
    std::fs::write(with.join("Regular.ttf"), JETBRAINS).unwrap();
    let without = fixture("ligatures-without");
    std::fs::write(without.join("Regular.ttf"), without_gsub(JETBRAINS)).unwrap();
    let families_in = |root: &PathBuf, bundled: &[String]| {
        let files = Catalog::scan(std::slice::from_ref(root), Limits::default()).files;
        monospace_families_with_bundled(&files, &[], bundled)
    };
    let has = families_in(&with, &[]);
    assert_eq!(has.has_ligatures("JetBrains Mono"), Some(true));
    let lacks = families_in(&without, &[]);
    assert_eq!(lacks.has_ligatures("jetbrains-mono"), Some(false));
    assert_eq!(lacks.has_ligatures("Not A Listed Family"), None);
    if crate::bundled::face_for_family(crate::bundled::IOSEVKA_FAMILY).is_some() {
        let bundled = families_in(&without, &[crate::bundled::IOSEVKA_FAMILY.to_string()]);
        assert_eq!(
            bundled.has_ligatures(crate::bundled::IOSEVKA_FAMILY),
            Some(true)
        );
    }
}

#[test]
fn a_name_beyond_the_remaining_budget_says_the_budget_ran_out() {
    let root = fixture("name-budget");
    let first = without_gsub(JETBRAINS);
    let second = with_family("Budget Mono");
    assert!(crate::bundled::face_for_family("Budget Mono").is_none());
    std::fs::write(root.join("A.ttf"), &first).unwrap();
    std::fs::write(root.join("B.ttf"), &second).unwrap();
    let resolve = |max_name_bytes| {
        resolve_and_admit_in_dirs(
            &[("Budget Mono".to_string(), FaceTarget::REGULAR)],
            std::slice::from_ref(&*root),
            Limits {
                max_name_bytes,
                ..Limits::default()
            },
        )
    };
    let enough = resolve(first.len() + second.len());
    let found = enough.get(0).unwrap().as_ref().unwrap();
    assert!(found.path.ends_with("B.ttf"), "{}", found.path);
    let batch = resolve(first.len() + second.len() - 1);
    let error = batch.get(0).unwrap().as_ref().unwrap_err();
    assert!(error.contains("exhausted"), "{error}");
    assert!(
        batch.stats.exhausted.contains("name bytes"),
        "{:?}",
        batch.stats
    );
}

#[test]
fn an_installed_family_remains_installed_when_it_also_is_bundled() {
    let root = fixture("installed-and-bundled");
    std::fs::write(root.join("Regular.ttf"), JETBRAINS).unwrap();
    let files = Catalog::scan(std::slice::from_ref(&*root), Limits::default()).files;
    let families = monospace_families_with_bundled(
        &files,
        &[],
        &[
            "JetBrains Mono".to_string(),
            "Other Bundled Face".to_string(),
        ],
    );
    assert!(families.installed.iter().any(|f| f == "JetBrains Mono"));
    assert_eq!(families.bundled, ["Other Bundled Face"]);
}

struct Fixture {
    _dir: aterm_tempfile::TempDir,
    path: PathBuf,
}

impl std::ops::Deref for Fixture {
    type Target = PathBuf;

    fn deref(&self) -> &Self::Target {
        &self.path
    }
}

fn fixture(label: &str) -> Fixture {
    let dir = aterm_tempfile::Builder::new()
        .prefix(&format!("aterm-font-catalog-{label}-"))
        .suffix(".noindex")
        .tempdir()
        .expect("create font_catalog fixture tempdir");
    let path = dir.path().to_path_buf();
    Fixture { _dir: dir, path }
}

#[test]
fn the_font_files_memo_is_revalidated_by_the_visited_directories_mtimes() {
    let root = fixture("memo");
    let vendor = root.join("truetype").join("vendor");
    std::fs::create_dir_all(&vendor).unwrap();
    std::fs::write(vendor.join("a.ttf"), b"a").unwrap();
    let absent_root = fixture("memo-later");
    std::fs::remove_dir_all(&*absent_root).unwrap();
    let roots = vec![root.to_path_buf(), absent_root.to_path_buf()];
    // Pin the directory's mtime so the next entry moves it at any clock granularity.
    #[cfg(unix)]
    let vendor_dir = std::fs::File::open(&vendor);
    #[cfg(windows)]
    let vendor_dir = {
        use std::os::windows::fs::OpenOptionsExt as _;
        const FILE_WRITE_ATTRIBUTES: u32 = 0x0100;
        const FILE_FLAG_BACKUP_SEMANTICS: u32 = 0x0200_0000;
        std::fs::OpenOptions::new()
            .access_mode(FILE_WRITE_ATTRIBUTES)
            .custom_flags(FILE_FLAG_BACKUP_SEMANTICS)
            .open(&vendor)
    };
    vendor_dir
        .and_then(|d| {
            d.set_modified(std::time::UNIX_EPOCH + std::time::Duration::from_secs(946_684_800))
        })
        .unwrap();

    let (first, walked) = memoized_font_files_walked(&roots);
    assert_eq!(first, vec![vendor.join("a.ttf")]);
    assert!(walked, "a cold ask walks");

    let (again, walked) = memoized_font_files_walked(&roots);
    assert_eq!(again, first);
    assert!(!walked, "an unchanged tree is answered from the memo");

    std::fs::write(vendor.join("b.ttf"), b"b").unwrap();
    let (grown, walked) = memoized_font_files_walked(&roots);
    assert_eq!(grown, vec![vendor.join("a.ttf"), vendor.join("b.ttf")]);
    assert!(walked, "an entry added deep in the tree is a miss");

    std::fs::create_dir_all(&*absent_root).unwrap();
    std::fs::write(absent_root.join("c.otf"), b"c").unwrap();
    let (with_root, walked) = memoized_font_files_walked(&roots);
    assert_eq!(
        with_root,
        vec![
            vendor.join("a.ttf"),
            vendor.join("b.ttf"),
            absent_root.join("c.otf")
        ]
    );
    assert!(walked, "a root appearing is a miss");

    let (remembered, walked) = memoized_font_files_walked(&roots);
    assert_eq!(remembered, with_root);
    assert!(!walked, "and the new state is remembered");
}

#[test]
fn concurrent_asks_for_one_tree_walk_it_once() {
    let root = fixture("single-flight");
    for i in 0..64 {
        let dir = root.join(format!("d{i:02}"));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("f.ttf"), b"f").unwrap();
    }
    let roots = vec![root.to_path_buf()];
    let start = std::sync::Barrier::new(8);
    let answers: Vec<(Vec<PathBuf>, bool)> = std::thread::scope(|s| {
        let handles: Vec<_> = (0..8)
            .map(|_| {
                s.spawn(|| {
                    start.wait();
                    memoized_font_files_walked(&roots)
                })
            })
            .collect();
        handles.into_iter().map(|h| h.join().unwrap()).collect()
    });
    let walks = answers.iter().filter(|(_, walked)| *walked).count();
    assert_eq!(walks, 1, "{walks} concurrent callers walked the same tree");
    assert!(answers.iter().all(|(files, _)| files.len() == 64));
    assert!(answers.windows(2).all(|w| w[0].0 == w[1].0));
}

#[test]
fn one_scan_resolves_duplicates_and_shares_admitted_bytes() {
    let root = fixture("batch");
    let path = root.join("Example-Mono.ttf");
    std::fs::write(&path, b"font bytes").unwrap();
    let requests = vec![
        ("Example Mono".to_string(), FaceTarget::REGULAR),
        ("ExampleMono".to_string(), FaceTarget::REGULAR),
    ];
    let batch =
        resolve_and_admit_in_dirs(&requests, std::slice::from_ref(&*root), Limits::default());
    let first = batch.get(0).unwrap().as_ref().unwrap();
    let second = batch.get(1).unwrap().as_ref().unwrap();
    assert_eq!(first.path, path.to_string_lossy());
    assert!(Arc::ptr_eq(&first.bytes, &second.bytes));
    assert_eq!(batch.stats.dirs, 1);
}

#[test]
fn an_installed_face_wins_a_bundled_family_name_and_the_identity_never_yields() {
    if !crate::bundled::ACTIVE {
        return;
    }
    let root = fixture("bundled-name");
    let installed = root.join("JetBrainsMono-Regular.ttf");
    std::fs::write(&installed, b"installed jetbrains mono").unwrap();
    let requests = vec![
        ("JetBrains Mono".to_string(), FaceTarget::REGULAR),
        (
            crate::bundled::JETBRAINS_ID.to_string(),
            FaceTarget::REGULAR,
        ),
    ];
    let batch =
        resolve_and_admit_in_dirs(&requests, std::slice::from_ref(&*root), Limits::default());
    let by_name = batch.get(0).unwrap().as_ref().unwrap();
    assert_eq!(by_name.path, installed.to_string_lossy(), "installed wins");
    assert_eq!(&by_name.bytes[..], b"installed jetbrains mono");
    let by_id = batch.get(1).unwrap().as_ref().unwrap();
    assert_eq!(by_id.path, crate::bundled::JETBRAINS_ID);

    std::fs::remove_file(&installed).unwrap();
    let batch =
        resolve_and_admit_in_dirs(&requests, std::slice::from_ref(&*root), Limits::default());
    for entry in &batch.entries {
        let admitted = entry.result.as_ref().unwrap();
        assert_eq!(
            admitted.path,
            crate::bundled::JETBRAINS_ID,
            "{}",
            entry.requested
        );
        assert_eq!(
            &admitted.bytes[..],
            crate::bundled::face_for_id(crate::bundled::JETBRAINS_ID).unwrap()
        );
    }
}

#[test]
fn a_bundled_default_is_named_as_it_is_selected() {
    if crate::bundled::IOSEVKA_SEMIBOLD_ACTIVE {
        let id = crate::bundled::IOSEVKA_SEMIBOLD_ID;
        let face = aterm_font::Face::parse(crate::bundled::face_for_id(id).unwrap(), 0).unwrap();
        assert_eq!(
            face_name(&face, aterm_font::name_id::TYPOGRAPHIC_FAMILY).as_deref(),
            Some(crate::bundled::IOSEVKA_FAMILY),
            "CONTROL: the face's own typographic family"
        );
        assert_eq!(
            automatic_face(id),
            Some((crate::bundled::IOSEVKA_SEMIBOLD_FAMILY.to_string(), false))
        );
    }
    if crate::bundled::ACTIVE {
        assert_eq!(
            automatic_face(crate::bundled::JETBRAINS_ID),
            Some((crate::bundled::JETBRAINS_FAMILY.to_string(), true))
        );
    }
}

#[test]
fn many_files_and_missing_families_stop_at_every_scan_cap() {
    let root = fixture("caps");
    for index in 0..40 {
        std::fs::write(root.join(format!("Face{index:03}.ttf")), b"not a font").unwrap();
    }
    let limits = Limits {
        max_dirs: 1,
        max_entries: 7,
        max_font_files: 5,
        max_requests: 3,
        max_work: 7,
        max_name_bytes: 8,
        max_aggregate_bytes: 16,
        max_depth: 0,
    };
    let requests = (0..12)
        .map(|i| (format!("Missing {i}"), FaceTarget::REGULAR))
        .collect::<Vec<_>>();
    let batch = resolve_and_admit_in_dirs(&requests, std::slice::from_ref(&*root), limits);
    assert!(batch.stats.dirs <= limits.max_dirs);
    assert!(batch.stats.entries <= limits.max_entries);
    assert!(batch.stats.font_files <= limits.max_font_files);
    assert!(batch.stats.work <= limits.max_work);
    assert!(batch.stats.name_bytes <= limits.max_name_bytes);
    assert!(batch.stats.aggregate_bytes <= limits.max_aggregate_bytes);
    assert!(batch.stats.truncated());
    assert!(batch.entries.iter().all(|entry| entry.result.is_err()));
    assert!(
        batch.entries[3]
            .result
            .as_ref()
            .unwrap_err()
            .contains("request limit")
    );
}

#[test]
fn aggregate_byte_budget_is_shared_across_explicit_assets() {
    let root = fixture("bytes");
    let a = root.join("a.ttf");
    let b = root.join("b.ttf");
    std::fs::write(&a, [1; 10]).unwrap();
    std::fs::write(&b, [2; 10]).unwrap();
    let requests = vec![
        (a.to_string_lossy().into_owned(), FaceTarget::REGULAR),
        (b.to_string_lossy().into_owned(), FaceTarget::REGULAR),
    ];
    let limits = Limits {
        max_aggregate_bytes: 15,
        ..Limits::default()
    };
    let batch = resolve_and_admit_in_dirs(&requests, &[], limits);
    assert!(batch.entries[0].result.is_ok());
    assert!(
        batch.entries[1]
            .result
            .as_ref()
            .unwrap_err()
            .contains("aggregate-byte")
    );
    assert!(batch.stats.aggregate_bytes <= limits.max_aggregate_bytes);
}
