// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Update watch: whether the mug's origin code was replaced, so it can end with
//! [`crate::launchd::EXIT_RESTART`].

use std::path::{Path, PathBuf};

use crate::code::code_fingerprint;

/// What the origin's code looked like when read ([`code_fingerprint`]).
type Print = Vec<u8>;

/// The print the mug started with, and the rule that says when the code changed.
#[derive(Debug)]
pub(super) struct Watch {
    /// The executable the mug was started as.
    origin: PathBuf,
    baseline: Option<Print>,
}

impl Watch {
    /// Watches the code of `origin` against the code of `running`.
    pub(super) fn start(origin: PathBuf, running: &Path) -> Self {
        let baseline = code_fingerprint(running);
        Self { origin, baseline }
    }

    /// Whether the code was replaced since [`Watch::start`].
    pub(super) fn replaced(&mut self) -> bool {
        let origin = &self.origin;
        replaced(&mut self.baseline, code_fingerprint(origin), || {
            code_fingerprint(origin)
        })
    }
}

/// The rule [`Watch::replaced`] applies to `baseline`, the print `now` and a second read
/// `again`: a print that cannot be read says nothing (an update in the middle of its swap is
/// read at the next tick); with no baseline the first clean read becomes it; a changed print
/// counts only when a second read agrees with it, so a half-written file is not a new build.
fn replaced(
    baseline: &mut Option<Print>,
    now: Option<Print>,
    again: impl FnOnce() -> Option<Print>,
) -> bool {
    let Some(now) = now else {
        return false;
    };
    match baseline {
        None => {
            *baseline = Some(now);
            false
        }
        Some(was) if *was == now => false,
        Some(_) => again().as_ref() == Some(&now),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn plist(text: &str) -> Option<Print> {
        Some(text.as_bytes().to_vec())
    }

    #[test]
    fn a_change_counts_only_when_it_reads_cleanly_twice() {
        let mut base = plist("1");
        assert!(!replaced(&mut base, plist("1"), || panic!(
            "not read again"
        )));
        assert!(!replaced(&mut base, None, || panic!("not read again")));
        assert_eq!(base, plist("1"));
        assert!(!replaced(&mut base, plist("2"), || plist("3")));
        assert!(!replaced(&mut base, plist("2"), || None));
        assert!(replaced(&mut base, plist("2"), || plist("2")));
        assert_eq!(base, plist("1"));
    }

    #[test]
    fn with_no_baseline_the_first_clean_read_is_it() {
        let mut base = None;
        assert!(!replaced(&mut base, None, || panic!("not read again")));
        assert_eq!(base, None);
        assert!(!replaced(&mut base, plist("1"), || panic!(
            "not read again"
        )));
        assert_eq!(base, plist("1"));
        assert!(replaced(&mut base, plist("2"), || plist("2")));
    }

    /// The watch on a bundle's executable: its Info.plist is the print; a bundle with none yet
    /// (the middle of a swap) takes the first one read as its baseline.
    #[test]
    fn a_bundles_new_info_plist_is_a_new_build() {
        let dir = aterm_tempfile::tempdir().expect("tempdir");
        let app = dir.path().join("X.app");
        let exe = app.join("Contents").join("MacOS").join("aterm");
        std::fs::create_dir_all(exe.parent().expect("parent")).expect("mkdir");
        std::fs::write(&exe, b"exe").expect("exe");
        let mut watch = Watch::start(exe.clone(), &exe);
        assert_eq!(watch.baseline, None, "no Info.plist yet");
        let info = app.join("Contents").join("Info.plist");
        std::fs::write(&info, b"<plist>1</plist>").expect("plist");
        assert!(!watch.replaced(), "the first clean read is the baseline");
        assert!(!watch.replaced());
        std::fs::write(&info, b"<plist>2</plist>").expect("plist");
        assert!(watch.replaced());
    }

    /// The baseline is the running copy's code, not the origin's as it reads at the start: an
    /// update landed while the copy was made and first run is a replaced origin. NEGATIVE
    /// CONTROL: a watch taking the origin's print at its start never restarts onto it.
    #[test]
    fn an_update_during_the_copys_first_exec_is_a_replaced_origin() {
        let dir = aterm_tempfile::tempdir().expect("tempdir");
        let bundle = |name: &str, plist: &[u8]| {
            let exe = dir.path().join(name).join("Contents/MacOS/aterm");
            std::fs::create_dir_all(exe.parent().expect("parent")).expect("mkdir");
            std::fs::write(&exe, b"exe").expect("exe");
            std::fs::write(dir.path().join(name).join("Contents/Info.plist"), plist)
                .expect("plist");
            exe
        };
        let copy = bundle("copy/X.app", b"<plist>1</plist>");
        let origin = bundle("X.app", b"<plist>2</plist>");
        assert!(Watch::start(origin.clone(), &copy).replaced());
        assert!(
            !Watch::start(origin.clone(), &origin).replaced(),
            "the control"
        );
    }
}
