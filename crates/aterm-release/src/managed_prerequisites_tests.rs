// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use std::os::unix::fs::PermissionsExt as _;

#[test]
fn publication_pin_contract_is_required_without_a_path_interpreter_probe() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let script = dir.path().join("pub");
    let helper = script.with_file_name("pub-python");
    for body in [
        "#!/bin/sh\nexec python3.14 \"$@\"\n",
        "#!/bin/sh\nprintf old-contract\\n\n",
        "#!/bin/sh\nprintf '%s\\n' publication-pinned-python-v1\nexit 1\n",
    ] {
        std::fs::write(&helper, body).unwrap();
        std::fs::set_permissions(&helper, std::fs::Permissions::from_mode(0o755)).unwrap();
        assert!(require_engine_pin(&script).is_err());
    }
    std::fs::write(&helper, "#!/bin/sh\n[ -z \"$PATH\" ] || exit 1\n[ \"$#:$1\" = '1:--capabilities' ] || exit 1\nprintf '%s\\n' publication-pinned-python-v1\n").unwrap();
    require_engine_pin(&script).unwrap();
    std::fs::remove_file(&helper).unwrap();
    assert!(require_engine_pin(&script).is_err());
}
