// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use std::os::unix::fs::{MetadataExt as _, symlink};

fn auxv(descriptor: Option<usize>) -> Vec<u8> {
    descriptor
        .into_iter()
        .map(|fd| (2_usize, fd))
        .chain([(0, 0)])
        .flat_map(|(kind, value)| [kind.to_ne_bytes(), value.to_ne_bytes()])
        .flatten()
        .collect()
}

#[test]
fn matching_inodes_on_different_devices_are_not_the_same_image() {
    let root = aterm_tempfile::tempdir().unwrap();
    let path = root.path().join("program");
    std::fs::write(&path, b"image").unwrap();
    let meta = std::fs::metadata(&path).unwrap();
    let mut image = FileIdentity::of(&meta);
    assert!(image.matches(&meta));
    image.device ^= 1;
    assert!(
        !image.matches(&meta),
        "the inode alone supplies no file identity"
    );
}

#[test]
fn a_candidate_observation_is_bracketed_by_the_same_loaded_image() {
    let root = aterm_tempfile::tempdir().unwrap();
    let path = root.path().join("program");
    let next = root.path().join("next");
    std::fs::write(&path, b"old").unwrap();
    std::fs::write(&next, b"next").unwrap();
    let old = FileIdentity::of(&std::fs::metadata(&path).unwrap());
    let new = FileIdentity::of(&std::fs::metadata(&next).unwrap());
    let mut reads = 0;
    assert_eq!(
        observed_match(&path, || {
            reads += 1;
            if reads == 1 {
                std::fs::rename(&next, &path).unwrap();
            }
            Some(old)
        }),
        Some(false),
        "the candidate is observed after the first loaded-image read"
    );
    assert_eq!(reads, 2);
    let mut images = [Some(old), Some(new)].into_iter();
    assert_eq!(observed_match(&path, || images.next().unwrap()), None);
    assert_eq!(observed_match(&path, || Some(new)), Some(true));
    assert_eq!(observed_match(&path, || None), None);
}

#[test]
fn exec_descriptors_use_the_kernels_native_word_width() {
    for word in [4, 8] {
        let encode = |pairs: &[(u64, u64)]| -> Vec<u8> {
            pairs
                .iter()
                .flat_map(|&(kind, value)| [kind, value])
                .flat_map(|value| match word {
                    4 => (value as u32).to_ne_bytes().to_vec(),
                    _ => value.to_ne_bytes().to_vec(),
                })
                .collect()
        };
        assert_eq!(
            exec_descriptor_words(&encode(&[(6, 4096), (2, 3), (0, 0)]), word),
            Some(Some(3))
        );
        assert_eq!(
            exec_descriptor_words(&encode(&[(6, 4096), (0, 0), (2, 3)]), word),
            Some(None)
        );
        assert_eq!(
            exec_descriptor_words(&encode(&[(2, i32::MAX as u64 + 1), (0, 0)]), word),
            None
        );
        assert_eq!(exec_descriptor_words(&encode(&[(6, 4096)]), word), None);
    }
    assert_eq!(exec_descriptor_words(&[0; 7], 4), None);
    assert_eq!(exec_descriptor_words(&[0; 16], 3), None);
}

#[test]
fn a_loaded_image_keeps_its_identity_when_replaced_before_observation() {
    let root = aterm_tempfile::tempdir().unwrap();
    let proc = root.path().join("proc/123");
    std::fs::create_dir_all(&proc).unwrap();
    let program = root.path().join("aterm");
    let next = root.path().join("next");
    std::fs::write(&program, b"old image").unwrap();
    symlink(&program, proc.join("exe")).unwrap();
    let dir = super::super::ProcDir::of(&root.path().join("proc"), 123).unwrap();
    // /proc's magic link keeps the exec file open even after its dentry is deleted.
    let held = dir.open(c"exe").unwrap();
    std::fs::write(&next, b"new image").unwrap();
    std::fs::rename(&next, &program).unwrap();
    let deleted = PathBuf::from(format!("{} (deleted)", program.display()));
    let loaded = LoadedImage::from_opened(deleted, &held.metadata().unwrap()).unwrap();
    assert_eq!(loaded.launch_path(), program);
    assert_eq!(loaded.matches_file(&program), Some(false));
    assert!(!loaded.matches_metadata(&std::fs::metadata(&program).unwrap()));
    std::fs::remove_file(&program).unwrap();
    assert_eq!(loaded.matches_file(&program), None);
}

#[test]
fn a_rename_between_kernel_path_observations_retains_the_original_launch_path() {
    for (name, deletion) in ["program", "program (deleted)"]
        .into_iter()
        .flat_map(|name| (0..3).map(move |deletion| (name, deletion)))
    {
        let root = aterm_tempfile::tempdir().unwrap();
        let proc = root.path().join("proc/123");
        std::fs::create_dir_all(&proc).unwrap();
        std::fs::write(proc.join("auxv"), auxv(None)).unwrap();
        let program = root.path().join(name);
        let next = root.path().join("next");
        std::fs::write(&program, b"old").unwrap();
        std::fs::write(&next, b"new").unwrap();
        let held = std::fs::File::open(&program).unwrap();
        let mut reads = 0;
        let loaded = LoadedImage::read_linux_with(&root.path().join("proc"), 123, &mut |_, _| {
            reads += 1;
            if reads == 2 && deletion == 0 {
                std::fs::rename(&next, &program).unwrap();
            }
            let meta = held.metadata().unwrap();
            if reads == 2 && deletion == 1 {
                std::fs::rename(&next, &program).unwrap();
            }
            let path = if reads == 1 || (reads == 2 && deletion == 2) {
                program.clone()
            } else {
                PathBuf::from(format!("{} (deleted)", program.display()))
            };
            if reads == 2 && deletion == 2 {
                std::fs::rename(&next, &program).unwrap();
            }
            Some(ImageRead { meta, path })
        })
        .expect("the same loaded inode survived the path's replacement");
        assert_eq!(loaded.launch_path(), program);
        assert_eq!(loaded.matches_file(&program), Some(false));
        assert_eq!(reads, if deletion == 1 { 3 } else { 2 });
    }
}

#[test]
fn a_path_change_without_a_proven_unlink_is_not_a_replacement_path() {
    for last in [
        "elsewhere",
        "program (deleted)",
        "program (deleted) (deleted)",
    ] {
        let root = aterm_tempfile::tempdir().unwrap();
        let proc = root.path().join("proc/123");
        std::fs::create_dir_all(&proc).unwrap();
        std::fs::write(proc.join("auxv"), auxv(None)).unwrap();
        let program = root.path().join("program");
        std::fs::write(&program, b"image").unwrap();
        let held = std::fs::File::open(&program).unwrap();
        let mut reads = 0;
        assert!(
            LoadedImage::read_linux_with(&root.path().join("proc"), 123, &mut |_, _| {
                reads += 1;
                Some(ImageRead {
                    meta: held.metadata().unwrap(),
                    path: if reads == 1 {
                        program.clone()
                    } else {
                        root.path().join(last)
                    },
                })
            },)
            .is_none()
        );
        assert!(reads <= 3, "one bounded confirmation at most");
    }
}

#[test]
fn a_translated_image_can_be_unlinked_while_its_maps_are_read() {
    for marked_map in [false, true] {
        let root = aterm_tempfile::tempdir().unwrap();
        let program = translated_fixture(root.path(), true, true);
        let held = std::fs::File::open(&program).unwrap();
        let next = root.path().join("next");
        std::fs::write(&next, b"new").unwrap();
        let mut reads = 0;
        let loaded = LoadedImage::read_linux_with(&root.path().join("proc"), 123, &mut |_, _| {
            reads += 1;
            if reads == 2 {
                std::fs::rename(&next, &program).unwrap();
                if marked_map {
                    let maps = root.path().join("proc/123/maps");
                    let text = std::fs::read_to_string(&maps).unwrap();
                    std::fs::write(&maps, text.trim_end().to_owned() + " (deleted)\n").unwrap();
                }
            }
            Some(ImageRead {
                meta: held.metadata().unwrap(),
                path: if reads == 1 {
                    program.clone()
                } else {
                    PathBuf::from(format!("{} (deleted)", program.display()))
                },
            })
        })
        .expect("the translated executable still maps the same held file");
        assert_eq!(loaded.launch_path(), program);
        assert_eq!(loaded.matches_file(&program), Some(false));
    }
}

#[test]
fn a_live_filename_ending_deleted_is_not_normalized() {
    let root = aterm_tempfile::tempdir().unwrap();
    let program = root.path().join("aterm (deleted)");
    std::fs::write(&program, b"image").unwrap();
    let loaded =
        LoadedImage::from_opened(program.clone(), &std::fs::metadata(&program).unwrap()).unwrap();
    assert_eq!(loaded.launch_path(), program);
    assert_eq!(loaded.matches_file(&program), Some(true));
}

#[test]
fn a_native_image_is_read_from_its_proc_descriptor() {
    let root = aterm_tempfile::tempdir().unwrap();
    let dir = root.path().join("proc/123");
    std::fs::create_dir_all(&dir).unwrap();
    let program = root.path().join("program");
    std::fs::write(&program, b"image").unwrap();
    symlink(&program, dir.join("exe")).unwrap();
    std::fs::write(dir.join("auxv"), auxv(None)).unwrap();
    let loaded = LoadedImage::read_linux(&root.path().join("proc"), 123).unwrap();
    assert_eq!(loaded.launch_path(), program);
    assert_eq!(loaded.matches_file(&program), Some(true));
    assert!(LoadedImage::read_linux(&root.path().join("proc"), 999).is_none());
}

#[test]
fn an_ambiguous_deleted_suffix_is_not_guessed() {
    let root = aterm_tempfile::tempdir().unwrap();
    let program = root.path().join("program");
    std::fs::write(&program, b"image").unwrap();
    let meta = std::fs::metadata(&program).unwrap();
    let marked = PathBuf::from(format!("{} (deleted)", program.display()));
    assert!(LoadedImage::from_opened(marked, &meta).is_none());
}

fn translated_fixture(root: &Path, executable: bool, exec_mark: bool) -> PathBuf {
    let dir = root.join("proc/123");
    std::fs::create_dir_all(dir.join("fd")).unwrap();
    std::fs::create_dir_all(dir.join("fdinfo")).unwrap();
    let program = root.join("program");
    let translator = root.join("translator");
    std::fs::write(&program, b"program").unwrap();
    std::fs::write(&translator, b"translator").unwrap();
    symlink(&translator, dir.join("exe")).unwrap();
    symlink(&program, dir.join("fd/3")).unwrap();
    std::fs::write(dir.join("auxv"), auxv(Some(3))).unwrap();
    let meta = std::fs::metadata(&program).unwrap();
    std::fs::write(
        dir.join("fdinfo/3"),
        format!(
            "flags:\t{}\nino:\t{}\n",
            if exec_mark { "0400040" } else { "0400000" },
            meta.ino()
        ),
    )
    .unwrap();
    let dev = meta.dev() as libc::dev_t;
    std::fs::write(
        dir.join("maps"),
        format!(
            "1000-2000 {} 0 {:x}:{:x} {} {}\n",
            if executable { "r-xp" } else { "r--p" },
            libc::major(dev),
            libc::minor(dev),
            meta.ino(),
            program.display()
        ),
    )
    .unwrap();
    program
}

#[test]
fn a_translated_image_requires_the_kernel_exec_descriptor_and_its_mapping() {
    for (executable, exec_mark) in [(true, true), (false, true), (true, false)] {
        let root = aterm_tempfile::tempdir().unwrap();
        let program = translated_fixture(root.path(), executable, exec_mark);
        let loaded = LoadedImage::read_linux(&root.path().join("proc"), 123);
        assert_eq!(loaded.is_some(), executable && exec_mark);
        if let Some(loaded) = loaded {
            assert_eq!(loaded.launch_path(), program);
            assert_eq!(loaded.matches_file(&program), Some(true));
            let next = root.path().join("next");
            std::fs::write(&next, b"next").unwrap();
            std::fs::rename(next, &program).unwrap();
            assert_eq!(loaded.matches_file(&program), Some(false));
        }
    }
}

#[test]
fn a_reused_exec_descriptor_cannot_borrow_the_old_path_mapping() {
    let root = aterm_tempfile::tempdir().unwrap();
    let program = translated_fixture(root.path(), true, true);
    let next = root.path().join("next");
    std::fs::write(&next, b"next").unwrap();
    std::fs::rename(next, &program).unwrap();
    assert!(LoadedImage::read_linux(&root.path().join("proc"), 123).is_none());
    // Even a descriptor marked for the new inode cannot borrow the old image's map.
    std::fs::write(
        root.path().join("proc/123/fdinfo/3"),
        format!(
            "flags:\t0400040\nino:\t{}\n",
            std::fs::metadata(&program).unwrap().ino()
        ),
    )
    .unwrap();
    assert!(LoadedImage::read_linux(&root.path().join("proc"), 123).is_none());
}

#[test]
fn missing_or_malformed_translator_evidence_never_falls_back_to_its_exe() {
    let root = aterm_tempfile::tempdir().unwrap();
    translated_fixture(root.path(), true, true);
    let dir = root.path().join("proc/123");
    std::fs::remove_file(dir.join("fd/3")).unwrap();
    assert!(LoadedImage::read_linux(&root.path().join("proc"), 123).is_none());
    std::fs::write(dir.join("auxv"), [0_u8; 7]).unwrap();
    assert!(LoadedImage::read_linux(&root.path().join("proc"), 123).is_none());
    std::fs::remove_file(dir.join("auxv")).unwrap();
    assert!(LoadedImage::read_linux(&root.path().join("proc"), 123).is_none());
}
