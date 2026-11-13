// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::{FLEET_MAIL_ANSWER_MAX, ask_one_line, fleet_mail::PeerMail, peer_mail_in};
use std::io::{Read as _, Write as _};
use std::os::unix::net::UnixStream;

/// The far end of a drive that has answered `answer` (and, `hang_up`, closed its side).
fn peer(answer: &[u8], hang_up: bool) -> (UnixStream, UnixStream) {
    let (ours, mut theirs) = UnixStream::pair().expect("a socket pair");
    // A one-minute hang detector: an unbounded read of a peer that stays open never ends.
    ours.set_read_timeout(Some(aterm_spec::wait::MUST_HAPPEN))
        .expect("a read timeout");
    theirs.write_all(answer).expect("the answer");
    if hang_up {
        theirs.shutdown(std::net::Shutdown::Write).expect("hung up");
    }
    (ours, theirs)
}

fn request_read(mut theirs: UnixStream) -> String {
    theirs
        .set_read_timeout(Some(aterm_spec::wait::MUST_HAPPEN))
        .expect("a read timeout");
    let mut got = [0u8; 256];
    let n = theirs.read(&mut got).expect("the request");
    String::from_utf8_lossy(&got[..n]).into_owned()
}

/// A drive writes its request as one line and takes the one line that answers it; a peer that
/// hangs up first is refused, and one that sends no newline is cut at the bound.
#[test]
fn a_fleet_drive_writes_one_line_and_reads_the_one_line_that_answers_it() {
    let (ours, theirs) = peer(b"OK f7\nAFTER\n", false);
    assert_eq!(
        ask_one_line(&ours, "m9", "@s-87654321 post key=k kind=ask -- HI"),
        Ok("OK f7\n".to_owned()),
        "the first line, nothing after it"
    );
    assert_eq!(
        request_read(theirs),
        "@s-87654321 post key=k kind=ask -- HI\n"
    );

    // Control: a peer that hangs up before it answers gives no answer.
    let (ours, _theirs) = peer(b"", true);
    assert_eq!(
        ask_one_line(&ours, "m9", "@s-87654321 inbox"),
        Err("m9 hung up before it answered".to_owned())
    );

    let (ours, _theirs) = peer(&[b'x'; 5000], false);
    let cut = ask_one_line(&ours, "m9", "@s-87654321 inbox").expect("a cut line");
    assert_eq!(
        u64::try_from(cut.len()).ok(),
        Some(FLEET_MAIL_ANSWER_MAX),
        "an answer with no newline ends at the bound, with the peer still open"
    );
}

/// A Mac reaches a peer by the mesh once its presence row was heard, and its own node always;
/// any other by direct fleet mail.
#[test]
fn a_peer_is_on_the_mesh_once_its_presence_row_was_heard() {
    use aterm_uds::private_file::{ensure_private_dir, write_private};
    let scratch = aterm_tempfile::tempdir().expect("a scratch dir");
    let state = scratch.path().join("state");
    let mesh = state.join(aterm_mesh::logs::MESH_DIR);
    ensure_private_dir(&state).expect("a private state dir");
    ensure_private_dir(&mesh).expect("a private mesh dir");
    let heard = format!("n-{}", "a".repeat(16));
    let unheard = format!("n-{}", "b".repeat(16));
    // Control: before any row was heard, the peer is reached by direct fleet mail.
    assert_eq!(peer_mail_in(&state, &heard), PeerMail::Fleet);
    write_private(
        &mesh,
        aterm_mesh::presence::SEEN_FILE,
        format!("{heard} 1000\n").as_bytes(),
    )
    .expect("the seen ledger");
    assert_eq!(peer_mail_in(&state, &heard), PeerMail::Mesh);
    assert_eq!(peer_mail_in(&state, &unheard), PeerMail::Fleet);
    // Its node as `read_node_id` reads it with MESH on (the key) and off (the legacy id).
    write_private(
        &state,
        aterm_net::fleet_member::NODE_KEY_FILE,
        format!("{}\n", "1".repeat(64)).as_bytes(),
    )
    .expect("a node key");
    write_private(
        &state,
        aterm_net::fleet_member::NODE_FILE,
        format!("{}\n", "2".repeat(32)).as_bytes(),
    )
    .expect("a node id");
    let own = aterm_net::fleet_member::read_node_id(&state)
        .expect("the node key reads")
        .expect("a node id");
    assert_ne!(own, heard);
    assert_eq!(
        peer_mail_in(&state, &own),
        PeerMail::Mesh,
        "this Mac itself"
    );
}
