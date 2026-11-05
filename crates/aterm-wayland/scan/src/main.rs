// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! wayland-scan: the dev-only generator of aterm-wayland's protocol modules.

mod generate;
mod xml;

fn write_fresh(path: &std::path::Path, bytes: impl AsRef<[u8]>) -> std::io::Result<()> {
    use std::io::Write as _;
    let _ = std::fs::remove_file(path);
    std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)?
        .write_all(bytes.as_ref())
}

fn main() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("..");
    let root = root.as_path();
    let files = match generate::generate(root) {
        Ok(files) => files,
        Err(e) => {
            eprintln!("wayland-scan: {e}");
            std::process::exit(1);
        }
    };
    for (rel, text) in files {
        let path = root.join(&rel);
        if let Err(e) = write_fresh(&path, text) {
            eprintln!("wayland-scan: {}: {e}", path.display());
            std::process::exit(1);
        }
        println!("wrote {rel}");
    }
}
