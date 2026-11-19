// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::{read_code_directory_hash, read_loaded_code_of, read_process_path};

/// The code a connection's audit token names is the code its process loaded and the path the
/// kernel records for it: this process's own, read by token as by pid. NEGATIVE CONTROLS: a token
/// of another pid version (the one a later exec leaves) and one whose pid is another process's
/// read nothing.
#[test]
fn a_token_reads_the_code_its_process_loaded_and_a_stale_one_reads_none() {
    let (near, _far) = crate::CtlStream::pair().unwrap();
    let token = crate::peer::peer_audit_token(&near).unwrap();
    let me = i32::try_from(std::process::id()).unwrap();
    assert_eq!(token.pid(), std::process::id());
    let (hash, path) = read_loaded_code_of(&token).unwrap();
    assert_eq!(Some(hash), read_code_directory_hash(me));
    assert_eq!(Some(path), read_process_path(me));
    let mut stale = token;
    stale.0[7] = stale.0[7].wrapping_add(1);
    assert_eq!(read_loaded_code_of(&stale), None, "another pid version");
    let mut other = token;
    other.0[5] = 1;
    assert_eq!(read_loaded_code_of(&other), None, "another process");
}
