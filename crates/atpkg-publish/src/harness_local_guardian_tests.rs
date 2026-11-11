// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::*;

/// A launch directory whose owner record names `owner` and whose acknowledgement is `ack`.
fn launch(owner: &Identity, ack: &Ack) -> (aterm_tempfile::TempDir, PathBuf) {
    let tmp = aterm_tempfile::Builder::new()
        .prefix("guardian-look-")
        .tempdir()
        .expect("private temp");
    let dir = tmp
        .path()
        .canonicalize()
        .expect("canonical launch")
        .join("launch");
    private_file::create_private_dir(&dir).expect("private launch");
    write(&dir, "owner.json", &owner.token()).expect("owner record");
    write(&dir, "ack.json", ack).expect("acknowledgement");
    (tmp, dir)
}

fn armed() -> Ack {
    Ack {
        done: false,
        success: true,
        reason: "local update owner armed".into(),
    }
}

fn finished() -> Ack {
    Ack {
        done: true,
        success: true,
        reason: "native ownership fixture completed".into(),
    }
}

/// This process with another birth: a recorded owner that no longer runs.
fn ended_owner() -> Identity {
    let me = identity().expect("own identity");
    Identity {
        birth: process::Birth(me.birth.0.wrapping_add(1)),
        ..me
    }
}

/// The order `look` replaced, kept here as the negative control: acknowledgement first.
fn look_acknowledgement_first(dir: &Path, between: impl FnOnce()) -> Option<Result<()>> {
    let ack = fs::read::<Ack>(dir, "ack.json").unwrap().expect("armed");
    if ack.done {
        return Some(Ok(()));
    }
    between();
    let owner = fs::read::<String>(dir, "owner.json")
        .unwrap()
        .expect("owner");
    (!live(&owner)).then(|| Err(Error::refused("ended without a final acknowledgement")))
}

#[test]
fn an_owner_that_acknowledges_and_ends_between_the_reads_is_not_refused() {
    // 0.193.0's release tree (m3, 2026-11-10): the replacement owner wrote
    // `{"done":true,"success":true}` and ended between the client's two reads, and the
    // client refused it as "ended without a final acknowledgement".
    let (_tmp, dir) = launch(&ended_owner(), &armed());
    let mut was_armed = false;
    let verdict = look(&dir, &mut was_armed, || {
        write(&dir, "ack.json", &finished()).expect("final acknowledgement");
    })
    .expect("look");
    assert!(matches!(verdict, Some(Ok(()))), "{verdict:?}");

    // Negative control: the same moment, read in the replaced order, is refused.
    let (_tmp, dir) = launch(&ended_owner(), &armed());
    let old = look_acknowledgement_first(&dir, || {
        write(&dir, "ack.json", &finished()).expect("final acknowledgement");
    });
    assert!(matches!(old, Some(Err(_))), "{old:?}");
}

#[test]
fn an_owner_that_ends_unacknowledged_is_still_refused() {
    let (_tmp, dir) = launch(&ended_owner(), &armed());
    let mut was_armed = false;
    let verdict = look(&dir, &mut was_armed, || {}).expect("look");
    let Some(Err(error)) = verdict else {
        panic!("an unacknowledged end must refuse: {verdict:?}");
    };
    assert!(
        error
            .to_string()
            .contains("ended without a final acknowledgement"),
        "{error}"
    );
    assert!(was_armed);
}

#[test]
fn a_live_owner_still_working_has_no_verdict() {
    let (_tmp, dir) = launch(&identity().expect("own identity"), &armed());
    let mut was_armed = false;
    let verdict = look(&dir, &mut was_armed, || {}).expect("look");
    assert!(verdict.is_none(), "{verdict:?}");
    assert!(was_armed);
}
