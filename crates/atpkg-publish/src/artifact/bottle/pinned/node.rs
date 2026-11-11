// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! The relocated standalone Node must execute the integer WebAssembly oracle itself.

use std::path::Path;

use super::Runner;
use crate::error::{Error, Result};

const PROBE: &str = r#"
'use strict';
const assert = require('node:assert/strict');
const fs = require('node:fs');
assert.equal(fs.realpathSync(process.execPath), fs.realpathSync(process.argv[1]));
assert.ok(process.versions.v8 && process.versions.openssl);
function add(type, opcode) {
  const bytes = Uint8Array.from([
    0,97,115,109,1,0,0,0, 1,7,1,96,2,type,type,1,type,
    3,2,1,0, 7,7,1,3,97,100,100,0,0,
    10,9,1,7,0,32,0,32,1,opcode,11
  ]);
  return new WebAssembly.Instance(new WebAssembly.Module(bytes)).exports.add;
}
const i32 = add(127, 106);
assert.equal(i32(-7, 8), 1);
assert.equal(i32(2147483647, 1), -2147483648);
const i64 = add(126, 124);
assert.equal(i64(-7n, 8n), 1n);
assert.equal(i64(9223372036854775807n, 1n), -9223372036854775808n);
assert.throws(() => new WebAssembly.Module(Uint8Array.from([0,97,115,109,1,0,0,0,255])), WebAssembly.CompileError);
fs.writeFileSync('UTF8 probe with spaces.txt', 'Node \u03bb\n');
assert.equal(fs.readFileSync('UTF8 probe with spaces.txt', 'utf8'), 'Node \u03bb\n');
process.stdout.write('atpkg Node runtime qualified\n');
"#;

pub(super) fn qualify(program: &Path, version: &str, work: &Path, run: &Runner<'_>) -> Result<()> {
    let answer = run(program, &["--version"], work)?;
    if !answer.status.success()
        || String::from_utf8_lossy(&answer.stdout).trim() != format!("v{version}")
    {
        return Err(Error::refused("Node version differs from the package"));
    }
    let executable = program
        .to_str()
        .ok_or_else(|| Error::refused("Node package path is not UTF-8"))?;
    let answer = run(program, &["--eval", PROBE, executable], work)?;
    if !answer.status.success()
        || String::from_utf8_lossy(&answer.stdout) != "atpkg Node runtime qualified\n"
    {
        return Err(Error::refused(format!(
            "relocated Node failed its JavaScript/WebAssembly reference controls: {}{}",
            String::from_utf8_lossy(&answer.stdout),
            String::from_utf8_lossy(&answer.stderr)
        )));
    }
    Ok(())
}

#[cfg(all(test, unix))]
#[path = "node_tests.rs"]
mod tests;
