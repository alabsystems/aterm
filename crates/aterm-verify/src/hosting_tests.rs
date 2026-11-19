// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The join's wire: what a run posts, what an answer names, and that a pass posts one end.

use super::*;
use std::sync::Mutex;

/// A host that keeps what it was posted and answers every start with `placed`.
#[derive(Debug, Default)]
pub(crate) struct Recording {
    pub(crate) placed: Hosted,
    pub(crate) posted: Mutex<Vec<String>>,
}

impl Recording {
    pub(crate) fn posted(&self) -> Vec<String> {
        self.posted.lock().expect("posted").clone()
    }
}

impl Host for Recording {
    fn start(&self, kind: &str) -> Hosted {
        self.posted.lock().expect("posted").push(start_line(kind));
        self.placed.clone()
    }
    fn end(&self, kind: &str, exit: Option<i32>) {
        self.posted
            .lock()
            .expect("posted")
            .push(end_line(kind, exit));
    }
}

const MINTED: &str = "tqn5r6e0148";
const LEGACY: &str = "s-f56dadda2d012b94f138";

/// An answer names the session a run is credited to, in either id form, and its worker; every
/// other answer names nothing: an instance that does not know the verb, a refusal, a word that
/// is no session id, a missing or a stray word, a worker with no session to credit, and a
/// worker that is the credited session. NEGATIVE CONTROL: the same answers with ids that read.
#[test]
fn an_answer_names_a_session_only_in_its_own_shape() {
    let named = |hosted: &str, worker: Option<&str>| Hosted {
        hosted: Some(hosted.to_owned()),
        worker: worker.map(str::to_owned),
    };
    assert_eq!(
        Hosted::of_reply(&format!("OK hosted={MINTED} worker=-\n")),
        named(MINTED, None)
    );
    assert_eq!(
        Hosted::of_reply(&format!("OK hosted={LEGACY} worker={MINTED}\n")),
        named(LEGACY, Some(MINTED))
    );
    assert_eq!(
        Hosted::of_reply("OK hosted=- worker=-\n"),
        Hosted::default()
    );
    for reply in [
        String::new(),
        "ERR unknown verb: hosted\n".to_owned(),
        "ERR denied: hosted is owner-only\n".to_owned(),
        "OK recorded\n".to_owned(),
        format!("OK hosted={MINTED}\n"),
        format!("OK worker=- hosted={MINTED}\n"),
        format!("OK hosted={MINTED} worker=- by=env\n"),
        "OK hosted=tab-1 worker=-\n".to_owned(),
        format!("OK hosted={} worker=-\n", MINTED.to_uppercase()),
        format!("OK hosted=- worker={MINTED}\n"),
        format!("OK hosted={MINTED} worker={MINTED}\n"),
        format!("OK hosted={MINTED} worker=tab-2\n"),
    ] {
        assert_eq!(Hosted::of_reply(&reply), Hosted::default(), "{reply:?}");
    }
}

/// A run's posts are one line each, naming the phase, the kind and, for an end, the exit code;
/// a kind is one lower-case word, so no kind can carry a second word into the request.
#[test]
fn a_post_is_one_line_of_phase_kind_and_exit() {
    assert_eq!(start_line(LAND), "hosted start kind=land");
    assert_eq!(end_line(LAND, Some(4)), "hosted end kind=land exit=4");
    assert_eq!(end_line(VERIFY, None), "hosted end kind=verify");
    for kind in [LAND, VERIFY, "build", "gate-2"] {
        assert!(is_kind(kind), "{kind}");
    }
    for kind in [
        "",
        "Land",
        "2nd",
        "-x",
        "land session=t",
        "a\nb",
        &"k".repeat(33),
    ] {
        assert!(!is_kind(kind), "{kind:?}");
    }
}

/// A pass posts its start as it is made and its end once: with the exit code where the pass
/// gave one, with none where it returned early, never twice.
#[test]
fn a_pass_posts_its_start_and_one_end() {
    let host = Recording::default();
    Pass::start(&host, VERIFY).end(Some(1));
    assert_eq!(
        host.posted(),
        ["hosted start kind=verify", "hosted end kind=verify exit=1"]
    );
    let early = Recording::default();
    drop(Pass::start(&early, VERIFY));
    assert_eq!(
        early.posted(),
        ["hosted start kind=verify", "hosted end kind=verify"]
    );
}

/// A host with no session to find its instance by, or none hosting it, asks nobody and names
/// nothing, at once.
#[test]
fn a_run_with_no_instance_to_ask_names_nothing() {
    let rendezvous = aterm_tempfile::tempdir().expect("tempdir");
    for host in [
        ToInstance::default(),
        ToInstance {
            sid: Some(MINTED.to_owned()),
            dir: None,
        },
        ToInstance {
            sid: Some(MINTED.to_owned()),
            dir: Some(rendezvous.path().to_path_buf()),
        },
    ] {
        assert_eq!(host.start(LAND), Hosted::default(), "{host:?}");
        host.end(LAND, Some(0));
    }
    assert_eq!(Unhosted.start(LAND), Hosted::default());
}
