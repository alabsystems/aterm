// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use std::io::BufRead as _;

#[test]
fn private_metadata_consumes_its_json_line_and_refuses_oversized_observations() {
    for size in [0, 8192] {
        let directory = crate::test_scratch_dir("resource-metadata");
        let path = directory.path().join("socket");
        let listener = aterm_uds::CtlListener::bind(&path).unwrap();
        let body =
            aterm_json::json!({"version":3,"ready":false,"reason":"x".repeat(size)}).to_string();
        let expected = body.clone();
        let server = std::thread::spawn(move || {
            let (mut stream, _) = aterm_uds::accept(
                &listener,
                aterm_uds::AcceptWait::Forever,
                aterm_uds::AcceptedMode::Blocking,
            )
            .unwrap();
            stream
                .set_read_timeout(Some(std::time::Duration::from_secs(3)))
                .unwrap();
            let mut reader = std::io::BufReader::new(&stream);
            let mut request = String::new();
            reader.read_line(&mut request).unwrap();
            if request.starts_with("AUTH ") {
                request.clear();
                reader.read_line(&mut request).unwrap();
            }
            assert_eq!(request, "resource-admission-v3 capabilities\n");
            writeln!(stream, "OK resource-admission-v3 capabilities\n{body}").unwrap();
        });
        let wire = ConnWire {
            path: path.to_string_lossy().into_owned(),
            origin: crate::TargetOrigin::Pinned,
        };
        let result = wire.request(&["resource-admission-v3".into(), "capabilities".into()]);
        if size == 0 {
            assert_eq!(result.unwrap().lines, vec![expected]);
        } else {
            assert!(result.is_err());
        }
        server.join().unwrap();
    }
}
