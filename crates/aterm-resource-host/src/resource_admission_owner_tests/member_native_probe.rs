// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
use super::*;

pub(super) fn run(
    client: &Client,
    original: &control::Request,
    lease: &aterm_capacity::build_cpu::Lease,
    query: &str,
    nonce: u8,
    end: Instant,
) {
    let cpu = aterm_uds::kept_out(|| lease.as_fd().try_clone_to_owned()).unwrap();
    let mut request = original.clone();
    request.member_attempt = [nonce; 16];
    request.role = control::Role::MetadataProbe;
    request.command.args = vec![query.as_bytes().to_vec()];
    request.delegations.cpu.as_mut().unwrap().source = cpu.as_raw_fd() as u32;
    request.delegations.gnu = control::GnuTransport::None;
    request.timeout_ns = Some(8_000_000_000);
    let mut child = client
        .member(
            &request,
            MemberIo {
                stdin: std::process::Stdio::null(),
                stdout: std::process::Stdio::piped(),
                stderr: std::process::Stdio::inherit(),
                descriptions: vec![cpu],
            },
            end,
        )
        .unwrap();
    let mut output = child.take_stdout().unwrap();
    let status = loop {
        assert!(
            Instant::now() < end,
            "actual metadata probe did not complete"
        );
        if let Some(status) = child.try_wait(end) {
            break status;
        }
        std::thread::sleep(POLL);
    };
    assert!(
        matches!(
            status,
            control::Status::Finished {
                outcome: control::Outcome::Exit(0),
                cause: control::CompletionCause::Exited,
                ..
            }
        ),
        "{status:?}"
    );
    let mut bytes = String::new();
    output.read_to_string(&mut bytes).unwrap();
    assert_eq!(bytes, format!("{query}\n"));
}
