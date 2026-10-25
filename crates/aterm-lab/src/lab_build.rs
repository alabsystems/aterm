// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The build the lab's evaluator judges in, which aos-search's bundles and the M6 learner's
//! stores name. A neutral module (design §13.3) so the learner reaches it without naming
//! `aos_search`.

use crate::Fail;

/// The build the lab's evaluator judges in and its bundles name.
pub const LAB_BUILD: &str = concat!(
    "aterm-lab ",
    env!("CARGO_PKG_VERSION"),
    "+g",
    env!("ATERM_LAB_GIT_COMMIT")
);

/// Refuses a build that names no commit.
/// # Errors
/// Exit 1, naming the build.
pub fn check_build(build: &str) -> Result<(), Fail> {
    let commit = build.rsplit_once("+g").map_or("", |(_, commit)| commit);
    let hash = commit.strip_suffix("-dirty").unwrap_or(commit);
    if hash.len() >= 12 && hash.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f')) {
        Ok(())
    } else {
        Err(Fail::die(format!(
            "the build `{build}` names no commit (built without git; pin one with \
             ATERM_BUILD_GIT_COMMIT), so what it writes could not name the interpreter that \
             judged it"
        )))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_lab_build_names_its_commit_and_a_build_that_names_none_is_refused() {
        assert!(
            LAB_BUILD.starts_with(concat!("aterm-lab ", env!("CARGO_PKG_VERSION"), "+g")),
            "{LAB_BUILD}"
        );
        if let Err(f) = check_build(LAB_BUILD) {
            panic!("{LAB_BUILD}: {:?}", f.msg);
        }
        let v = env!("CARGO_PKG_VERSION");
        for commit in [
            "0123456789ab",
            "0123456789ab-dirty",
            "0123456789abcdef0123456789abcdef01234567",
        ] {
            let build = format!("aterm-lab {v}+g{commit}");
            assert!(check_build(&build).is_ok(), "{build}");
        }
        for build in [
            format!("aterm-lab {v}+gunknown"),
            format!("aterm-lab {v}"),
            format!("aterm-lab {v}+g"),
            format!("aterm-lab {v}+g0123456789a"),
            format!("aterm-lab {v}+g0123456789AB"),
            format!("aterm-lab {v}+g0123456789ab-dirty-dirty"),
        ] {
            let f = check_build(&build).expect_err(&build);
            assert_eq!(f.code, 1, "{build}");
            let msg = f.msg.unwrap_or_default();
            assert!(
                msg.contains(&format!("the build `{build}` names no commit")),
                "{msg}"
            );
        }
    }
}
