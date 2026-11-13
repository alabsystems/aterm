// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Held input copies and bounded evidence capture for disposable native components.

use std::io::Write as _;
use std::path::Path;

use aterm_json::{Value, json};

use crate::Fail;
use crate::mac::{self, Vm};

pub(super) fn fail(error: impl std::fmt::Display) -> Fail {
    Fail::die(error.to_string())
}

/// Publish without clobbering. A failed write remains explicitly partial.
pub(super) fn save(record: &Path, name: &str, bytes: &[u8]) -> Result<(), Fail> {
    let partial = record.join(format!("{name}.partial"));
    let mut file = crate::util::create_new_regular_file(&partial, 0o600).map_err(fail)?;
    file.write_all(bytes).map_err(fail)?;
    drop(file);
    std::fs::hard_link(&partial, record.join(name)).map_err(fail)?;
    std::fs::remove_file(partial).map_err(fail)
}

pub(super) fn json_file(record: &Path, name: &str, value: &Value) -> Result<(), Fail> {
    save(record, name, &aterm_json::to_vec(value).map_err(fail)?)
}

/// Snapshot executable bytes without launching the borrowed host image.
pub(super) fn copy_image(source: &Path, share: &Path, name: &str) -> Result<Value, Fail> {
    let source = source.canonicalize().map_err(fail)?;
    if !std::fs::metadata(&source).map_err(fail)?.is_file() {
        return Err(Fail::usage(format!(
            "{} is not an executable file",
            source.display()
        )));
    }
    let mut input = std::fs::File::open(&source).map_err(fail)?;
    let dest = share.join(name);
    let mut output = crate::util::create_new_regular_file(&dest, 0o700).map_err(fail)?;
    std::io::copy(&mut input, &mut output).map_err(fail)?;
    drop(output);
    Ok(json!({"borrowed_path":source.display().to_string(),
        "bytes":std::fs::metadata(&dest).map_err(fail)?.len(),
        "sha256":crate::windows::sha256_file(&dest)?,"additional_host_launch":false}))
}

pub(super) fn unavailable(why: impl std::fmt::Display) -> Value {
    json!({"complete":false,"unavailable":why.to_string()})
}

pub(super) fn capture(
    vm: &Vm,
    record: &Path,
    name: &str,
    command: &str,
) -> (Value, Option<Vec<u8>>) {
    match vm.exec_complete(command, mac::STEP) {
        Ok(out) if out.status.success() => match save(record, name, &out.stdout) {
            Ok(()) => (
                json!({"complete":true,"file":name,"bytes":out.stdout.len()}),
                Some(out.stdout),
            ),
            Err(why) => (
                json!({"complete":false,"unavailable":{
                    "code":why.code,"message":why.msg,"signal":why.stopped,
                }}),
                None,
            ),
        },
        Ok(out) => (
            unavailable(format!(
                "guest capture exited {}: {}",
                out.status,
                String::from_utf8_lossy(&out.stderr).trim()
            )),
            None,
        ),
        Err(why) => (unavailable(why), None),
    }
}
