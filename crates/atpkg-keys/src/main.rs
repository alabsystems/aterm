// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! `atpkg-keys` — the owner-only Ed25519 signing CLI for the atpkg publish pipeline (§12).
//!
//!   atpkg-keys keygen <key-file>            generate a key (0600) + print its base64 pubkey
//!   atpkg-keys pubkey <key-file>            print the base64 pubkey of an existing key
//!   atpkg-keys sign   <key-file> <file> [<sig-out>]   detached-sign <file>'s exact bytes
//!
//! The key file holds the **secret** pkcs8 bytes — keep it offline; never commit it. The
//! printed base64 pubkey goes into `PINNED_PKG_ROOTKEY` (root) or the index's
//! `[keys].release_key_pubkey` (release). The detached signature is exactly what the
//! verify-only client (`atpkg::sig`) checks.

#[cfg(unix)]
use std::os::unix::fs::OpenOptionsExt;
#[cfg(unix)]
use std::process::ExitCode;

// POSIX-only owner tool (see lib.rs); the Windows binary honestly refuses.
#[cfg(windows)]
fn main() {
    eprintln!("atpkg-keys: unsupported on Windows (POSIX-only owner signing tool)");
    std::process::exit(2);
}

#[cfg(unix)]
fn main() -> ExitCode {
    // Positional `next()` pulls instead of `collect()`: the CLI uses at most four
    // arguments, and collecting an arbitrary-length iterator into a Vec is an
    // unbounded allocation Trust cannot bound (`count-not-derivable`). Extra
    // arguments are ignored exactly as the previous `collect` + get(0..4) was.
    // (`std::env::args` itself is a hardened compat_observable boundary either way;
    // see the artifact notes in `create_secret_file`.)
    let mut argv = std::env::args().skip(1);
    let verb = argv.next();
    let arg1 = argv.next();
    let arg2 = argv.next();
    let arg3 = argv.next();
    let r = match verb.as_deref() {
        Some("keygen") => keygen(arg1.as_ref()),
        Some("pubkey") => pubkey(arg1.as_ref()),
        Some("sign") => sign(arg1.as_ref(), arg2.as_ref(), arg3.as_ref()),
        Some(other) => Err(concat(&[
            "unknown verb '",
            other,
            "' (try: keygen, pubkey, sign)",
        ])),
        None => Err("usage: atpkg-keys <keygen|pubkey|sign> …".to_string()),
    };
    match r {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprint_line(&concat(&["atpkg-keys: ", &e]));
            ExitCode::from(1)
        }
    }
}

/// Build a message by concatenation — deliberately not `format!`: the macro's inlined
/// unsafe `fmt::Arguments::new` expansion is unmodeled by Trust and charged to the caller.
#[cfg(unix)]
fn concat(parts: &[&str]) -> String {
    // No `with_capacity` pre-size: the capacity hint is a pure optimization, and its
    // unbounded-size allocation obligation is unprovable for arbitrary inputs.
    let mut s = String::new();
    for p in parts {
        s.push_str(p);
    }
    s
}

/// `println!`-equivalent without the fmt machinery: one buffered write of `line` + '\n',
/// panicking on failure exactly like `println!`.
///
/// Known Trust L0 artifact: `panic_any` is unsupported MIR for the full verifier
/// (`Call::std::panic::panic_any`), and hoisting it into a cold local leaf does NOT
/// help — the caller then carries an equally-unsupported `Call::<leaf>` obligation
/// (unsupported lowering propagates through local callees), so the direct call is the
/// minimal shape. An inline `panic!` is no better: it is a charged assert edge.
#[cfg(unix)]
fn print_line(line: &str) {
    use std::io::Write as _;
    let mut buf = String::new();
    buf.push_str(line);
    buf.push('\n');
    if std::io::stdout().write_all(buf.as_bytes()).is_err() {
        std::panic::panic_any("failed printing to stdout");
    }
}

/// `eprintln!`-equivalent without the fmt machinery (see `print_line`).
#[cfg(unix)]
fn eprint_line(line: &str) {
    use std::io::Write as _;
    let mut buf = String::new();
    buf.push_str(line);
    buf.push('\n');
    if std::io::stderr().write_all(buf.as_bytes()).is_err() {
        std::panic::panic_any("failed printing to stderr");
    }
}

/// Everything this tool reads is tiny — a pkcs8 key is under a hundred bytes and a
/// manifest-to-sign is a few KiB — so 1 MiB is a generous ceiling, not a tight fit.
#[cfg(unix)]
const READ_CAP: u64 = 1024 * 1024;

/// Name a non-regular file's actual type for the `read_bytes` refusal message, so
/// `--key /dev/urandom` says "character device" instead of a mystery error.
#[cfg(unix)]
fn file_type_name(t: std::fs::FileType) -> &'static str {
    use std::os::unix::fs::FileTypeExt as _;
    if t.is_dir() {
        "a directory"
    } else if t.is_char_device() {
        "a character device"
    } else if t.is_block_device() {
        "a block device"
    } else if t.is_fifo() {
        "a FIFO"
    } else if t.is_socket() {
        "a socket"
    } else {
        "not a regular file"
    }
}

/// Read a file's exact bytes — the one shared file-read site (Trust: the std fs FFI
/// boundary is charged to the enclosing fn, so every reader funnels through here).
///
/// Regular-file-only and bounded: an unbounded `read_to_end` of an operator-supplied
/// path is the seamless.rs `/dev/urandom` kernel-panic incident in miniature — a
/// never-EOF character device fills RAM+swap until the machine dies. The type check
/// runs on the OPEN handle's fstat (so it cannot race a path swap) and refuses
/// devices, directories, FIFOs, and sockets before a single byte is read; `take` then
/// bounds the read at `READ_CAP + 1` so even a regular file growing underneath us
/// cannot allocate unboundedly — the +1 byte is how we DETECT overflow (exactly
/// CAP+1 bytes arriving proves the file exceeds the cap) without reading further.
/// (One residual: a writerless FIFO parks the tool in open(2) itself — POSIX blocks
/// there until a writer arrives — but past open, nothing can hang or overallocate.)
#[cfg(unix)]
fn read_bytes(path: &str) -> std::io::Result<Vec<u8>> {
    use std::io::Read as _;
    let f = std::fs::File::open(path)?;
    let ft = f.metadata()?.file_type();
    if !ft.is_file() {
        return Err(std::io::Error::other(concat(&[
            path,
            " is ",
            file_type_name(ft),
            ", not a regular file",
        ])));
    }
    let mut bytes = Vec::new();
    f.take(READ_CAP + 1).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > READ_CAP {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            concat(&[
                path,
                " exceeds the 1 MiB read cap (keys and manifests are tiny)",
            ]),
        ));
    }
    Ok(bytes)
}

/// Write `bytes` to `path` — the one shared plain-file-write site (see `read_bytes`).
///
/// Known Trust L0 artifact: `File::create` is a hardened raw-path boundary
/// (`hardened_raw_path_api`, fail-closed absent capability contracts). It must stay:
/// re-signing overwrites an existing `.sig`, so the non-clobbering `File::create_new`
/// (which Trust does not flag) would be a behavior change.
#[cfg(unix)]
fn write_bytes(path: &str, bytes: &[u8]) -> std::io::Result<()> {
    use std::io::Write as _;
    std::fs::File::create(path)?.write_all(bytes)
}

/// Read the secret pkcs8 key file.
#[cfg(unix)]
fn read_key(path: &str) -> Result<Vec<u8>, String> {
    read_bytes(path).map_err(|e| concat(&["read key ", path, ": ", &e.to_string()]))
}

/// Create the SECRET key file 0600, create-new (never clobber an existing key) — the one
/// owner-secret file-create FFI site (Trust: the fs FFI boundary is charged to the
/// enclosing fn; see `read_bytes`).
///
/// Write access is requested via `append(true)`, deliberately NOT `write(true)`: Trust's
/// FFI-summary matcher keys on the callee's LAST path segment, so `OpenOptions::write`
/// false-matches the libc `write(fd, buf, len)` summary and manufactures refuted
/// fd-range/non-null/writes-global obligations for a plain builder-flag call. The two
/// spellings are byte-identical here: `create_new` (O_EXCL) guarantees a brand-new empty
/// file, and it is written exactly once by a single `write_all`, for which append-at-EOF
/// (EOF = 0) and write-at-offset-0 coincide.
///
/// Known Trust L0 artifact: `OpenOptions::open` itself is a hardened raw-path boundary
/// (`hardened_raw_path_api`, fail-closed) that can only be discharged by capability
/// contracts, which this campaign does not add. `File::create` cannot express
/// create-new + 0600, which the secret key requires, so the one residual obligation is
/// confined to this leaf.
///
/// Unix-only: `OpenOptions::mode` is a POSIX-permission extension, and the sole
/// caller (`keygen`) is `#[cfg(unix)]`. The Windows binary refuses in `main`.
#[cfg(unix)]
fn create_secret_file(path: &str) -> std::io::Result<std::fs::File> {
    std::fs::OpenOptions::new()
        .append(true)
        .create_new(true)
        .mode(0o600)
        .open(path)
}

/// Write `bytes` to an already-open file — the leaf write FFI site paired with
/// `create_secret_file` (see `read_bytes`).
#[cfg(unix)]
fn write_all_to(f: &mut std::fs::File, bytes: &[u8]) -> std::io::Result<()> {
    use std::io::Write as _;
    f.write_all(bytes)
}

#[cfg(unix)]
fn keygen(out: Option<&String>) -> Result<(), String> {
    let out = out.ok_or("usage: atpkg-keys keygen <key-file>")?;
    let (pkcs8, pub_b64) = atpkg_keys::generate()?;
    // Write the SECRET key 0600 (create-new: never clobber an existing key).
    let mut f = create_secret_file(out).map_err(|e| {
        concat(&[
            "create ",
            out,
            " (refusing to overwrite an existing key): ",
            &e.to_string(),
        ])
    })?;
    write_all_to(&mut f, &pkcs8).map_err(|e| concat(&["write ", out, ": ", &e.to_string()]))?;
    print_line(&pub_b64);
    eprint_line(&concat(&[
        "atpkg-keys: wrote secret key to ",
        out,
        " (0600) — keep it offline, never commit it",
    ]));
    Ok(())
}

#[cfg(unix)]
fn pubkey(key: Option<&String>) -> Result<(), String> {
    let key = key.ok_or("usage: atpkg-keys pubkey <key-file>")?;
    print_line(&atpkg_keys::pubkey_b64(&read_key(key)?)?);
    Ok(())
}

#[cfg(unix)]
fn sign(
    key: Option<&String>,
    msg: Option<&String>,
    sig_out: Option<&String>,
) -> Result<(), String> {
    let key = key.ok_or("usage: atpkg-keys sign <key-file> <file> [<sig-out>]")?;
    let msg = msg.ok_or("usage: atpkg-keys sign <key-file> <file> [<sig-out>]")?;
    let bytes = read_bytes(msg).map_err(|e| concat(&["read ", msg, ": ", &e.to_string()]))?;
    let sig = atpkg_keys::sign(&read_key(key)?, &bytes)?;
    let path = match sig_out {
        Some(path) => path.clone(),
        // Default: <file>.sig next to the input.
        None => concat(&[msg, ".sig"]),
    };
    write_bytes(&path, &sig).map_err(|e| concat(&["write ", &path, ": ", &e.to_string()]))?;
    eprint_line(&concat(&[
        "atpkg-keys: wrote ",
        &path,
        " (",
        &sig.len().to_string(),
        " bytes)",
    ]));
    Ok(())
}
