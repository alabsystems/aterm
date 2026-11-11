// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use std::cell::Cell;

/// A `.crate`: a ustar archive of `files` under `top/`, gzip'd in stored blocks.
pub(crate) fn crate_file(top: &str, files: &[(&str, &[u8])]) -> Vec<u8> {
    let mut tar = Vec::new();
    for (rel, body) in files {
        tar.extend_from_slice(&header(&format!("{top}/{rel}"), body.len(), b'0'));
        tar.extend_from_slice(body);
        tar.resize(tar.len().div_ceil(BLOCK) * BLOCK, 0);
    }
    tar.resize(tar.len() + 2 * BLOCK, 0);
    gzip_stored(&tar)
}

/// A ustar header for `path`.
pub(crate) fn header(path: &str, size: usize, kind: u8) -> [u8; BLOCK] {
    let mut h = [0u8; BLOCK];
    h[..path.len()].copy_from_slice(path.as_bytes());
    h[100..108].copy_from_slice(b"0000644\0");
    h[124..136].copy_from_slice(format!("{size:011o}\0").as_bytes());
    h[156] = kind;
    h[257..263].copy_from_slice(b"ustar\0");
    h[263..265].copy_from_slice(b"00");
    h[148..156].copy_from_slice(b"        ");
    let sum: u32 = h.iter().map(|b| u32::from(*b)).sum();
    h[148..156].copy_from_slice(format!("{sum:06o}\0 ").as_bytes());
    h
}

/// `data` as one gzip member of stored DEFLATE blocks.
pub(crate) fn gzip_stored(data: &[u8]) -> Vec<u8> {
    let mut out = vec![0x1f, 0x8b, 8, 0, 0, 0, 0, 0, 0, 0xff];
    let mut chunks = data.chunks(0xffff).peekable();
    if chunks.peek().is_none() {
        out.extend_from_slice(&[1, 0, 0, 0xff, 0xff]);
    }
    while let Some(chunk) = chunks.next() {
        out.push(u8::from(chunks.peek().is_none()));
        let len = u16::try_from(chunk.len()).unwrap();
        out.extend_from_slice(&len.to_le_bytes());
        out.extend_from_slice(&(!len).to_le_bytes());
        out.extend_from_slice(chunk);
    }
    out.extend_from_slice(&aterm_codec::crc32::Crc32::of(data).to_le_bytes());
    out.extend_from_slice(&u32::try_from(data.len()).unwrap().to_le_bytes());
    out
}

/// crates.io as a test's own bytes: an index row naming `sha256` for `name` `version`, and
/// `archive`, counting what was asked. `None` is offline.
pub(crate) struct Served {
    pub(crate) name: String,
    pub(crate) version: String,
    pub(crate) sha256: String,
    pub(crate) archive: Option<Vec<u8>>,
    pub(crate) asked: std::rc::Rc<Cell<u32>>,
}

impl Served {
    /// `archive`, with the sha256 it really has.
    pub(crate) fn honest(name: &str, version: &str, archive: Vec<u8>) -> Self {
        Self {
            name: name.to_string(),
            version: version.to_string(),
            sha256: aterm_codec::hex::encode_lower(&aterm_digest::Sha256::digest(&archive)),
            archive: Some(archive),
            asked: std::rc::Rc::default(),
        }
    }

    /// No network.
    pub(crate) fn offline() -> Self {
        Self {
            name: String::new(),
            version: String::new(),
            sha256: String::new(),
            archive: None,
            asked: std::rc::Rc::default(),
        }
    }
}

impl Registry for Served {
    fn index(&self, name: &str) -> Result<Vec<u8>, String> {
        self.asked.set(self.asked.get() + 1);
        if self.archive.is_none() {
            return Err(format!(
                "GET {} failed: failed to lookup address information",
                index_url(name)
            ));
        }
        Ok(format!(
            "{{\"name\":\"{}\",\"vers\":\"0.0.1\",\"cksum\":\"{}\"}}\n\
             {{\"name\":\"{}\",\"vers\":\"{}\",\"cksum\":\"{}\",\"yanked\":false}}\n",
            self.name,
            "0".repeat(64),
            self.name,
            self.version,
            self.sha256
        )
        .into_bytes())
    }

    fn archive(&self, name: &str, version: &str) -> Result<Vec<u8>, String> {
        self.asked.set(self.asked.get() + 1);
        self.archive
            .clone()
            .ok_or_else(|| format!("GET {} failed", crate_url(name, version)))
    }
}

fn pristine(cache: &Path, served: Served) -> Pristine {
    Pristine::at(Some(cache.to_path_buf()), Box::new(served))
}

fn upstream() -> Vec<u8> {
    crate_file(
        "upstream-1.2.3",
        &[
            ("Cargo.toml", b"[package]\nname = \"upstream\"\n"),
            ("src/lib.rs", b"pub fn f() {}\n"),
            ("src/deep/mod.rs", b"// deep\n"),
        ],
    )
}

#[test]
fn a_published_crate_is_fetched_held_to_the_index_and_cached() {
    let cache = aterm_tempfile::tempdir().unwrap();
    let served = Served::honest("upstream", "1.2.3", upstream());
    let asked = std::rc::Rc::clone(&served.asked);
    let sha = served.sha256.clone();
    let source = pristine(cache.path(), served);
    std::fs::create_dir_all(cache.path().join("crates")).unwrap();
    std::fs::write(cache.path().join("crates/upstream-1.2.3"), b"stale file").unwrap();

    let copy = source.copy("upstream", "1.2.3").expect("a held copy");
    assert!(copy.fetched && copy.sha256 == sha, "{}", copy.provenance());
    assert_eq!(
        std::fs::read(copy.dir.join("src/deep/mod.rs")).unwrap(),
        b"// deep\n"
    );
    assert_eq!(asked.get(), 2, "the index, then the archive");

    let again = source.copy("upstream", "1.2.3").expect("the cached copy");
    assert!(!again.fetched && again.dir == copy.dir && again.sha256 == sha);
    assert_eq!(asked.get(), 2, "a cached copy asks crates.io nothing");
}

/// A download the index does not vouch for is refused before a byte of it is unpacked, and
/// leaves nothing in the cache.
#[test]
fn a_tampered_archive_or_a_wrong_sha_is_refused() {
    let mut tampered = Served::honest("upstream", "1.2.3", upstream());
    if let Some(archive) = tampered.archive.as_mut() {
        let mid = archive.len() / 2;
        archive[mid] ^= 1;
    }
    let mut wrong = Served::honest("upstream", "1.2.3", upstream());
    wrong.sha256 = "ab".repeat(32);
    for (case, served) in [("tampered", tampered), ("wrong sha", wrong)] {
        let cache = aterm_tempfile::tempdir().unwrap();
        let Err(why) = pristine(cache.path(), served).copy("upstream", "1.2.3") else {
            panic!("{case}: a copy the index does not vouch for was used");
        };
        assert!(
            why.contains("hashes to") && why.contains("refused"),
            "{case}: {why}"
        );
        assert!(
            !cache.path().join("crates/upstream-1.2.3").exists(),
            "{case}: a refused download left a cache entry"
        );
    }
}

/// An archive the index vouches for is still refused whole when a member would land
/// outside its folder or is a link, and nothing is laid outside the staging folder.
#[test]
fn a_member_outside_the_package_or_a_link_refuses_the_unpack() {
    let escape = |path: &str, kind: u8| {
        let mut tar = Vec::new();
        tar.extend_from_slice(&header("upstream-1.2.3/ok.rs", 3, b'0'));
        tar.extend_from_slice(b"ok\n");
        tar.resize(2 * BLOCK, 0);
        tar.extend_from_slice(&header(path, 0, kind));
        tar.resize(tar.len() + 2 * BLOCK, 0);
        gzip_stored(&tar)
    };
    for (path, kind) in [
        ("upstream-1.2.3/../../escape.rs", b'0'),
        ("/tmp/escape.rs", b'0'),
        ("other-9.9.9/escape.rs", b'0'),
        ("upstream-1.2.3/link", b'2'),
    ] {
        let cache = aterm_tempfile::tempdir().unwrap();
        let served = Served::honest("upstream", "1.2.3", escape(path, kind));
        let Err(why) = pristine(cache.path(), served).copy("upstream", "1.2.3") else {
            panic!("`{path}` unpacked");
        };
        assert!(why.contains("did not unpack"), "{path}: {why}");
        let left: Vec<_> = std::fs::read_dir(cache.path().join("crates"))
            .unwrap()
            .flatten()
            .map(|e| e.file_name())
            .collect();
        assert!(left.is_empty(), "{path}: left {left:?}");
    }

    let pax_record = b"23 path=/tmp/escape.rs\n";
    let mut global_pax = Vec::new();
    global_pax.extend_from_slice(&header("pax_global_header", pax_record.len(), b'g'));
    global_pax.extend_from_slice(pax_record);
    global_pax.resize(2 * BLOCK, 0);
    global_pax.extend_from_slice(&header("upstream-1.2.3/ok.rs", 3, b'0'));
    global_pax.extend_from_slice(b"ok\n");
    global_pax.resize(global_pax.len().div_ceil(BLOCK) * BLOCK + 2 * BLOCK, 0);
    let cache = aterm_tempfile::tempdir().unwrap();
    let served = Served::honest("upstream", "1.2.3", gzip_stored(&global_pax));
    let Err(why) = pristine(cache.path(), served).copy("upstream", "1.2.3") else {
        panic!("a global PAX path override unpacked");
    };
    assert!(why.contains("global PAX header"), "{why}");

    let sparse_pax_record = b"22 GNU.sparse.major=1\n";
    let mut sparse_pax = Vec::new();
    sparse_pax.extend_from_slice(&header("PaxHeader", sparse_pax_record.len(), b'x'));
    sparse_pax.extend_from_slice(sparse_pax_record);
    sparse_pax.resize(2 * BLOCK, 0);
    sparse_pax.extend_from_slice(&header("upstream-1.2.3/ok.rs", 3, b'0'));
    sparse_pax.extend_from_slice(b"ok\n");
    sparse_pax.resize(sparse_pax.len().div_ceil(BLOCK) * BLOCK + 2 * BLOCK, 0);
    let cache = aterm_tempfile::tempdir().unwrap();
    let served = Served::honest("upstream", "1.2.3", gzip_stored(&sparse_pax));
    let Err(why) = pristine(cache.path(), served).copy("upstream", "1.2.3") else {
        panic!("a PAX sparse record unpacked");
    };
    assert!(why.contains("PAX sparse record"), "{why}");

    let mut dangling_ext = Vec::new();
    dangling_ext.extend_from_slice(&header("././@LongLink", 18, b'L'));
    dangling_ext.extend_from_slice(b"upstream-1.2.3/ok\0");
    dangling_ext.resize(4 * BLOCK, 0);
    let cache = aterm_tempfile::tempdir().unwrap();
    let served = Served::honest("upstream", "1.2.3", gzip_stored(&dangling_ext));
    let Err(why) = pristine(cache.path(), served).copy("upstream", "1.2.3") else {
        panic!("a dangling extension header unpacked");
    };
    assert!(
        why.contains("extension header has no member after it"),
        "{why}"
    );
}

#[test]
fn offline_there_is_no_copy_and_the_reason_says_why() {
    let cache = aterm_tempfile::tempdir().unwrap();
    let Err(why) = pristine(cache.path(), Served::offline()).copy("winit", "0.30.13") else {
        panic!("an offline run found a copy");
    };
    assert!(why.contains("https://index.crates.io/wi/ni/winit"), "{why}");
}

#[test]
fn the_index_shards_as_cargo_does() {
    for (name, path) in [
        ("a", "1/a"),
        ("ab", "2/ab"),
        ("abc", "3/a/abc"),
        ("Winit", "wi/ni/winit"),
    ] {
        assert_eq!(index_url(name), format!("{INDEX}/{path}"));
    }
}

/// NEGATIVE CONTROL: symlinked cache entries/roots, conflicting per-member extension
/// headers, and non-zero bytes smuggled after an end-of-archive zero block are refused.
#[test]
fn pristine_refuses_symlinked_cache_conflicting_headers_and_smuggled_trailer() {
    #[cfg(unix)]
    {
        let root = aterm_tempfile::tempdir().unwrap();
        let forged = root.path().join("forged");
        std::fs::create_dir_all(forged.join(PACKAGE)).unwrap();
        std::fs::write(forged.join(STAMP), format!("{}\n", "a".repeat(64))).unwrap();
        std::fs::write(forged.join(PACKAGE).join("lib.rs"), b"forged\n").unwrap();

        let linked_cache = root.path().join("linked-cache");
        std::os::unix::fs::symlink(&forged, &linked_cache).unwrap();
        let Err(why) = pristine(
            &linked_cache,
            Served::honest("upstream", "1.2.3", upstream()),
        )
        .copy("upstream", "1.2.3") else {
            panic!("symlinked cache root was accepted");
        };
        assert!(why.contains("not a cache directory"), "{why}");

        let cache = root.path().join("cache");
        std::fs::create_dir_all(cache.join("crates")).unwrap();
        let linked_entry = cache.join("crates/upstream-1.2.3");
        std::os::unix::fs::symlink(&forged, &linked_entry).unwrap();
        assert!(
            cached(&linked_entry, "upstream-1.2.3").is_none(),
            "symlinked cache entry must not be trusted"
        );
        let copy = pristine(&cache, Served::honest("upstream", "1.2.3", upstream()))
            .copy("upstream", "1.2.3")
            .expect("symlinked entry unlinked and replaced by verified copy");
        assert!(copy.fetched);
        assert_eq!(
            std::fs::read(forged.join(PACKAGE).join("lib.rs")).unwrap(),
            b"forged\n",
            "forged target outside cache left untouched"
        );
    }

    // Duplicate `L` + `x` path overrides on one member are refused.
    let pax_path = b"28 path=upstream-1.2.3/b.rs\n";
    let mut dup_path = Vec::new();
    dup_path.extend_from_slice(&header("././@LongLink", 20, b'L'));
    dup_path.extend_from_slice(b"upstream-1.2.3/a.rs\0");
    dup_path.resize(2 * BLOCK, 0);
    dup_path.extend_from_slice(&header("PaxHeader", pax_path.len(), b'x'));
    dup_path.extend_from_slice(pax_path);
    dup_path.resize(4 * BLOCK, 0);
    dup_path.extend_from_slice(&header("upstream-1.2.3/c.rs", 3, b'0'));
    dup_path.extend_from_slice(b"ok\n");
    dup_path.resize(dup_path.len().div_ceil(BLOCK) * BLOCK + 2 * BLOCK, 0);
    let cache = aterm_tempfile::tempdir().unwrap();
    let Err(why) = pristine(
        cache.path(),
        Served::honest("upstream", "1.2.3", gzip_stored(&dup_path)),
    )
    .copy("upstream", "1.2.3") else {
        panic!("conflicting L + x path headers unpacked");
    };
    assert!(why.contains("duplicate path override"), "{why}");

    // Duplicate `size` PAX records in one header body are refused.
    let dup_size_records = b"10 size=3\n10 size=4\n";
    let mut dup_size = Vec::new();
    dup_size.extend_from_slice(&header("PaxHeader", dup_size_records.len(), b'x'));
    dup_size.extend_from_slice(dup_size_records);
    dup_size.resize(2 * BLOCK, 0);
    dup_size.extend_from_slice(&header("upstream-1.2.3/ok.rs", 3, b'0'));
    dup_size.extend_from_slice(b"ok\n");
    dup_size.resize(dup_size.len().div_ceil(BLOCK) * BLOCK + 2 * BLOCK, 0);
    let cache = aterm_tempfile::tempdir().unwrap();
    let Err(why) = pristine(
        cache.path(),
        Served::honest("upstream", "1.2.3", gzip_stored(&dup_size)),
    )
    .copy("upstream", "1.2.3") else {
        panic!("duplicate PAX size records unpacked");
    };
    assert!(why.contains("duplicate size override"), "{why}");

    // Non-zero bytes smuggled after the first 512-byte zero block are refused.
    let mut smuggled = Vec::new();
    smuggled.extend_from_slice(&header("upstream-1.2.3/ok.rs", 3, b'0'));
    smuggled.extend_from_slice(b"ok\n");
    smuggled.resize(2 * BLOCK, 0);
    smuggled.resize(3 * BLOCK, 0); // first 512-byte zero block ends archive
    smuggled.extend_from_slice(&header("upstream-1.2.3/hidden.rs", 4, b'0'));
    smuggled.extend_from_slice(b"bad\n");
    smuggled.resize(smuggled.len().div_ceil(BLOCK) * BLOCK + 2 * BLOCK, 0);
    let cache = aterm_tempfile::tempdir().unwrap();
    let Err(why) = pristine(
        cache.path(),
        Served::honest("upstream", "1.2.3", gzip_stored(&smuggled)),
    )
    .copy("upstream", "1.2.3") else {
        panic!("smuggled member past zero block unpacked");
    };
    assert!(why.contains("non-zero bytes trail"), "{why}");
}
