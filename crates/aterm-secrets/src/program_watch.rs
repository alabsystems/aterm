// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A service's immutable executable identity, retained from startup through its last round.

use aterm_uds::process::LoadedImage;
use std::path::{Path, PathBuf};

/// A running service and the path its supervisor will start next. Neither is re-sampled as
/// a baseline: the path may already hold a replacement before this watch is constructed.
pub(crate) struct ProgramWatch {
    path: Option<PathBuf>,
    image: Option<LoadedImage>,
    started: Option<Stamp>,
}

/// An executable as it is on disk. The loaded identity also catches a replacement that
/// happened before the first disk read; these fields retain detection of in-place rewrites.
type Stamp = (u64, u64, u64, i64);

fn stamp(meta: &std::fs::Metadata) -> Stamp {
    use std::os::unix::fs::MetadataExt as _;
    (meta.dev(), meta.ino(), meta.size(), meta.mtime())
}

fn metadata(path: &Path) -> Option<std::fs::Metadata> {
    std::fs::metadata(path)
        .ok()
        .filter(std::fs::Metadata::is_file)
}

fn initial_stamp(now: Option<Stamp>, matches: Option<bool>) -> Option<Stamp> {
    // A replacement observed before startup is not the running image's baseline. If the
    // old image later returns to the path, it must not be mistaken for another replacement.
    (matches != Some(false)).then_some(now).flatten()
}

impl ProgramWatch {
    pub(crate) fn current() -> Self {
        let image = LoadedImage::read(std::process::id().try_into().unwrap_or(0));
        let path = std::env::current_exe().ok();
        // On macOS keep the original dyld launch path: a kernel image path may now name
        // the moved-aside bundle. Linux's verified kernel path removes its deleted marker
        // and names a translated program rather than the interpreter, when proven.
        #[cfg(any(target_os = "linux", target_os = "android"))]
        let path = image
            .as_ref()
            .map(|image| image.launch_path().to_path_buf())
            .or(path);
        let current = path.as_deref().and_then(metadata);
        let matches = image
            .as_ref()
            .zip(current.as_ref())
            .map(|(image, meta)| image.matches_metadata(meta));
        let started = initial_stamp(current.as_ref().map(stamp), matches);
        Self {
            path,
            image,
            started,
        }
    }

    pub(crate) fn path(&self) -> Option<&Path> {
        self.path.as_deref()
    }

    /// A positive mismatch asks a supervised process to retire cleanly. A missing path means
    /// no successor is there to start; an unreadable image never means a replacement.
    pub(crate) fn replaced(&self) -> bool {
        let current = self.path().and_then(metadata);
        let matches = self
            .image
            .as_ref()
            .zip(current.as_ref())
            .map(|(image, meta)| image.matches_metadata(meta));
        replaced(self.started, current.as_ref().map(stamp), matches)
    }
}

fn replaced(started: Option<Stamp>, now: Option<Stamp>, matches: Option<bool>) -> bool {
    now.is_some() && (matches == Some(false) || started.zip(now).is_some_and(|(a, b)| a != b))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read as _, Write as _};

    #[test]
    fn a_loaded_image_outlives_the_first_path_sample_without_guessing_unknown_reads() {
        let old = Some((1, 2, 3, 4));
        let new = Some((1, 5, 3, 4));
        assert!(
            replaced(new, new, Some(false)),
            "replaced before first sample"
        );
        assert!(replaced(old, new, Some(true)), "later in-place rewrite");
        assert!(!replaced(new, new, Some(true)));
        assert!(!replaced(new, new, None), "unreadable loaded image");
        assert!(!replaced(None, new, None), "no usable initial evidence");
        assert!(
            !replaced(old, None, Some(false)),
            "no successor at the path"
        );
        assert!(
            replaced(old, new, None),
            "a known later disk change still counts"
        );
        let started = initial_stamp(new, Some(false));
        assert_eq!(started, None, "never adopt a replacement as the baseline");
        assert!(
            !replaced(started, old, Some(true)),
            "the old image returned"
        );
        assert_eq!(initial_stamp(old, None), old, "unavailable-image fallback");
    }

    #[test]
    fn later_path_replacement_is_seen_but_a_missing_successor_is_not() {
        let root = aterm_tempfile::tempdir().unwrap();
        let path = root.path().join("program");
        std::fs::write(&path, b"old").unwrap();
        let watch = ProgramWatch {
            started: metadata(&path).as_ref().map(stamp),
            path: Some(path.clone()),
            image: None,
        };
        assert!(!watch.replaced());
        let next = root.path().join("next");
        std::fs::write(&next, b"new").unwrap();
        std::fs::rename(&next, &path).unwrap();
        assert!(watch.replaced());
        std::fs::rename(&path, root.path().join("moved")).unwrap();
        assert!(!watch.replaced());
    }

    /// The parent waits for proof of exec before replacing the file. In the first arm this
    /// process then takes its *first* path sample, after replacement, while executing the old
    /// image. A stamp-only watch cannot detect it. The other arm checks a later replacement.
    #[test]
    fn executable_watch_child() {
        let Some(mode) = std::env::var_os("ATERM_TEST_PROGRAM_WATCH") else {
            return;
        };
        let before = (mode == "after-watch").then(ProgramWatch::current);
        println!("\nprogram-watch-ready");
        std::io::stdout().flush().unwrap();
        let mut go = [0];
        std::io::stdin().read_exact(&mut go).unwrap();
        let watch = before.unwrap_or_else(ProgramWatch::current);
        assert!(
            watch.replaced(),
            "the process still maps the replaced executable"
        );
    }

    #[test]
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    fn an_exec_replaced_before_the_first_watch_is_not_adopted_as_its_baseline() {
        use aterm_uds::deadline::{KeptUntilRan, ad_hoc_copy, prints};
        use aterm_uds::spawnfd::{Child, Stdio, command};
        const CHILD: &str = "program_watch::tests::executable_watch_child";
        struct Reap(Child);
        impl Drop for Reap {
            fn drop(&mut self) {
                let _ = self.0.kill();
                let _ = self.0.wait();
            }
        }
        for mode in ["before-watch", "after-watch"] {
            let root = aterm_tempfile::tempdir().unwrap();
            let mut kept = KeptUntilRan::new(root.keep());
            let program = kept.path().join("watch-child");
            let next = kept.path().join("watch-next");
            let exe = std::env::current_exe().unwrap();
            ad_hoc_copy(&exe, &program);
            ad_hoc_copy(&exe, &next);
            let mut child = Reap(
                command(&program)
                    .args(["--exact", CHILD, "--nocapture"])
                    .env("ATERM_TEST_PROGRAM_WATCH", mode)
                    .stdin(Stdio::piped())
                    .stdout(Stdio::piped())
                    .spawn()
                    .unwrap(),
            );
            prints(
                &mut child.0,
                "program-watch-ready",
                aterm_spec::wait::MUST_HAPPEN,
            );
            kept.ran();
            std::fs::rename(&next, &program).unwrap();
            child.0.stdin.take().unwrap().write_all(b"x").unwrap();
            let status = aterm_spec::wait::until_within(
                aterm_spec::wait::MUST_HAPPEN,
                "the replaced executable's watch",
                || child.0.try_wait().unwrap(),
            );
            assert!(status.success(), "{mode}: {status}");
        }
    }
}
