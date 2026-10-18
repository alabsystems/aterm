<!-- Copyright 2026 Andrew Yates -->
<!-- SPDX-License-Identifier: Apache-2.0 -->

# aterm-tempfile: spec recovery from its 7 unit tests

> Status 2026-10-16, after the merge with the tests-and-gates audit: the rows naming 1 test it
> deleted are gone, and the spec rows no longer cite those tests. Counts elsewhere in this file
> were taken before.

> Audit amendment: The 2026-10-16 code audit corrected unique_name's cross-process uniqueness claim (§5.1). The dated inventory below records the earlier snapshot; atomic-create collision retry remains the guarantee.

> Status 2026-10-16: this is the crate's migration worklist. Each of the 4 specs below is
> to be proved through Trust/Clean, and then the tests it subsumes are deleted. No test has
> been deleted yet. No spec is blocked by gap 0 alone (§1 histogram). Only
> `unique-name-format-and-injective` can retire its test without the effects lane (gap 8).

Source: `crates/aterm-tempfile/src/lib.rs` (read only). Every path below is repo-relative. It is the crate's only source file.
All 7 `#[test]` fns are in its `mod tests`. One is `cfg(unix)` and one is `cfg(windows)`. I read
every test and the code it reaches. Nothing was compiled or run. The claims below come from
reading the code.

Result: **4 specs**, covering **6 tests**. **1 test is not a spec sample** (§4).

## 0. Notation used in the statements

- `Sfx(pid,r,c) = dec(pid) ++ "_" ++ hex16(r) ++ "_" ++ dec(c)`. `dec` is `u32`/`u64`
  `to_string`. `hex16(r)` is the 16-char, zero-padded, lowercase hex of `r`, most significant
  nibble first. Every char of `Sfx` is in `[0-9a-f_]`, and `Sfx` is never empty.
- `Name(p,pid,r,c) = p ++ Sfx(pid,r,c)`. This is the equality with
  `format!("{prefix}{pid}_{rand:016x}_{count}")` that the code comment in `unique_name` claims.
- `C` is the static `COUNTER: AtomicU64`. One call of `unique_name` reads `c = C` and leaves
  `C = c+1` (`fetch_add`, which wraps only after 2^64 calls).
- `r` is the value `os_random_u64` returns. It is `u64::from_ne_bytes(buf)` when `read_os_random`
  succeeds and `no_csprng_fallback()` otherwise. Every spec leaves `r` universally quantified, so
  that callee can be an opaque, havocked call.
- `FsEnv` is an abstract filesystem model: a map from path to `{Dir(mode, uid), File(bytes, mode,
  ino, uid)}`, plus `temp_dir() = std::env::temp_dir()`. Every fs fact below is stated over
  `FsEnv` and needs gap 8 (effects).
- `SingleNormal(n)` means `Path::new(n).components()` yields exactly `[Normal(n)]`. On unix,
  `SingleNormal(p ++ Sfx) ⇔ '/' ∉ p`. `Sfx` is nonempty and has no separator, so the last
  component is never `.` or `..`. A leading `/` or `./`, or an inner `/`, is exactly what breaks
  it. On windows, a `\`, a drive prefix or a verbatim prefix in `p` also breaks it.
- `Fresh(d, parent)` means all of the following hold:
  - `d.path() = parent.join(n)` for some `n` with `SingleNormal(n)`;
  - that path did not exist before the call;
  - after the call it is a `Dir`, with mode `0o700` on unix;
  - nothing else under `parent` changed.
- `SI(path, body)` means `path` is a regular file holding exactly `body` with
  `mode & 0o7777 = 0o555`.
- `Cache(body) = temp_dir()/"aterm-stand-ins"/hex16(fnv1a(body))`.
- `fnv1a` is the fold that starts at `0xcbf29ce484222325` and maps
  `h ↦ (h ^ b) · 0x100000001b3 mod 2^64` for each byte `b`.
- **`trust::skip` prerequisite (an aterm-side change, not a Trust gap).** `unique_name`,
  `read_os_random`, `Drop::drop` and `Builder::prefix` carry
  `#[cfg_attr(trust_verify, trust::skip)]`. A proof about their compiled bodies first needs that
  skip lifted. Until then the spec can only be an assumed contract on the skipped fn, which is
  not a migration. The skip comments give the reasons: allocator panic obligations exhaust the
  budget, and `remove_dir_all` is a syscall wrapper.

## 1. Specs (TSV)

Columns: `id`, `kind`, `functions`, `statement`, `subsumes`, `difficulty (Rust features the proof
must handle)`, `blocking_gaps` (audit §7 numbers).

```tsv
id	kind	functions	statement	subsumes	difficulty	blocking_gaps
unique-name-format-and-injective	contract	unique_name, hex_digit, os_random_u64 (opaque r), no_csprng_fallback (opaque)	∀ prefix:&str, if C = c before the call, then unique_name(prefix) = Name(prefix, std::process::id(), r, c) and C = c+1 after it. Table lemma: ∀ n<16, hex_digit(n) = "0123456789abcdef"[n], and ∀ n≥16, hex_digit(n) = 'f'. Loop lemma: after the 16 iterations of slot := hex_digit(v mod 16); v := v/16 over hex.iter_mut().rev(), hex = hex16(r). Corollary (injectivity within one process): two calls read different counter values c1 ≠ c2 (any two calls do, before 2^64 calls), the suffix after the last '_' is dec(c), and dec is injective. So any two calls return distinct strings, even when their prefixes are equal and their r values are equal.	-	EASY table lemma: a 16-arm match on u64, decidable by evaluation. MEDIUM rest: String::with_capacity and push/push_str contents, u32/u64 to_string (a std Display model), a [char;16] iter_mut().rev() loop and a for-in over the array, u64 div/mod by 16, and the AtomicU64 fetch_add on a static (a sequential atomic model is enough). Lift trust::skip first.	0, 1, 3, 4, 5, 7, 13
tempdir-creates-fresh-private-dir-under-parent	contract	TempDir::{with_prefix_in,new_in,new}, tempdir, Builder::{new,prefix,prefix_str,tempdir,tempdir_in}, call0	(prefix_str) Builder::new().prefix_str() = ".tmp", and b.prefix(p).prefix_str() = p, with the last prefix call winning. (with_prefix_in) ∀ prefix, dir, in FsEnv: (a) if ¬SingleNormal(prefix ++ Sfx) (on unix exactly '/' ∈ prefix), the result is Err(InvalidInput) on the first attempt and FsEnv is unchanged. Validity depends only on prefix, because Sfx has no separator. (b) Otherwise, on success it returns Ok(d) with Fresh(d, dir), and d.path().file_name() = Name(prefix,pid,r,c), which starts with prefix. A create that fails with AlreadyExists is retried under a new name, up to 5 attempts in all, and then the result is Err(AlreadyExists). Any other io error is returned unchanged. (instances) tempdir() = TempDir::new() = new_in(temp_dir()) = with_prefix_in(".tmp", temp_dir()). For a Builder b, b.tempdir() = with_prefix_in(b.prefix_str(), temp_dir()) and b.tempdir_in(dir) = with_prefix_in(b.prefix_str(), dir). So the prefixes "", ".", ".." and "ordinary_" are accepted, with parent = dir. "../x", "a/b" and an absolute path are refused and nothing is created. On windows "..\x" and "C:x" are refused too.	crates/aterm-tempfile/src/lib.rs::builder_prefix, crates/aterm-tempfile/src/lib.rs::a_prefix_cannot_escape_the_requested_parent (also the creation half of tempdir_creates_and_cleans_up)	MEDIUM pure core: the SingleNormal lemma needs a model of std's unix (and windows) Path component parser, and the OsStr==str compare inside matches!. The rest: an Option<String> field match, generic impl AsRef<Path> and call0, Path::join, a bounded for 0..5 loop with continue and early returns, io::Error construction and kind(). Effects: DirBuilder::mode(0o700).create (unix) or fs::create_dir, and std::env::temp_dir. Builder::prefix is trust::skip.	0, 1, 3, 4, 5, 7, 8, 11
tempdir-drop-removes-keep-transfers	contract	Drop for TempDir::drop, TempDir::{keep,path}, AsRef<Path> for TempDir::as_ref	∀ d:TempDir with d.path = q: path(&d) = as_ref(&d) = q. keep(d) returns q and leaves d.path = PathBuf::new() before d is dropped, so that drop does no fs operation and the tree at q, contents included, is unchanged. Dropping a TempDir with a non-empty path calls remove_dir_all(q) and ignores its result. In FsEnv, when remove_dir_all succeeds, q and everything under it no longer exist. Composed with tempdir-creates-fresh-private-dir-under-parent: after a tempdir() guard drops, its path does not exist. After keep the caller alone owns q, and once the caller removes q the parent holds exactly what it held before the child was created.	crates/aterm-tempfile/src/lib.rs::tempdir_creates_and_cleans_up, crates/aterm-tempfile/src/lib.rs::keeping_a_directory_transfers_cleanup_to_the_caller	EASY pure half: mem::take on a struct field (a two-state x'), the consuming self, then the drop-glue order, OsStr::is_empty. The cleanup half is fs::remove_dir_all, an effect. Drop is trust::skip, so until gap 8 that half can only be an assumed platform contract.	0, 1, 2, 4, 8
stand-in-read-only-shared-inode	contract	stand_in, link_shared, is_stand_in, write_read_only, fnv1a, unique_name	(This is feature stand-in or cfg(test) code, unix only.) ∀ path, body in FsEnv: if path.file_name() is None, stand_in returns Err(InvalidInput) and changes nothing. Let D be path's parent, or "." when that parent is empty. On Ok, SI(path, body) holds, a write through path fails (mode 0555), and D gains no entry other than path: every branch renames or removes the .stand-in.* temp. Calling it again with the same (path, body) is Ok and leaves the same state. Off macOS, each call lays a fresh inode. On macOS, after Ok, ino(path) = ino(Cache(body)) when is_stand_in(Cache(body), cache, body) held or was re-established, and the hard link passed is_stand_in(tmp, D, body). So two stand-ins of one body share one inode, and different bodies get different inodes. A collision of fnv1a(body) only costs a rewrite, because is_stand_in compares the bytes. A cached copy is replaced by a fresh 0555 copy before linking if its mode ≠ 0555, or its bytes ≠ body, or its owner ≠ the cache dir's owner, so a tampered copy is never linked. If the link into D fails (for example, D is read-only), stand_in returns Err and Cache(body) keeps its inode. fnv1a(bytes) is the pure fold in §0.	crates/aterm-tempfile/src/lib.rs::stand_ins_share_one_read_only_inode_per_body	EASY fnv1a (a slice fold with wrapping_mul). HARD rest: Path/&[u8]/String, Vec<u8> compare, format! hex naming, fs symlink_metadata/metadata/read/rename/hard_link/remove_file, OpenOptions create_new+mode, set_permissions, MetadataExt ino/uid/mode, inspect_err closures, cfg(target_os = "macos") branches. Every claim except fnv1a is a filesystem effect. The race freedom the code argues (only an inode's owner can change it) needs a concurrent FS model.	0, 1, 3, 4, 7, 8, 13
```

### Distribution by kind (4)

| kind | count | ids |
|---|---|---|
| contract | 4 | unique-name-format-and-injective, tempdir-creates-fresh-private-dir-under-parent, tempdir-drop-removes-keep-transfers, stand-in-read-only-shared-inode |

### Gap histogram (gap → specs blocked)

| gap | specs | ids |
|---|---|---|
| 0 | 4 | all |
| 1 | 4 | all |
| 2 | 1 | tempdir-drop-removes-keep-transfers |
| 3 | 3 | unique-name-format-and-injective, tempdir-creates-fresh-private-dir-under-parent, stand-in-read-only-shared-inode |
| 4 | 4 | all (each spec composes callee contracts: hex_digit, unique_name, with_prefix_in, write_read_only/is_stand_in, keep→drop) |
| 5 | 2 | unique-name-format-and-injective, tempdir-creates-fresh-private-dir-under-parent |
| 7 | 3 | unique-name-format-and-injective, tempdir-creates-fresh-private-dir-under-parent, stand-in-read-only-shared-inode |
| 8 | 3 | tempdir-creates-fresh-private-dir-under-parent, tempdir-drop-removes-keep-transfers, stand-in-read-only-shared-inode |
| 11 | 1 | tempdir-creates-fresh-private-dir-under-parent |
| 13 | 2 | unique-name-format-and-injective (static AtomicU64), stand-in-read-only-shared-inode (concurrent FS) |

### Proof dependency order

`hex_digit` table lemma → hex loop lemma → `unique-name-format-and-injective` →
the naming half of `tempdir-creates-fresh-private-dir-under-parent` (plus `prefix_str` and the
`SingleNormal` lemma) → the pure `keep`/`path` half of `tempdir-drop-removes-keep-transfers` →
the fs halves of both (gap 8) → `fnv1a` → `stand-in-read-only-shared-inode`.

## 2. Test → spec map (TSV; all 7)

`primary` is the spec that retires the test. `also` lists the other specs the same test samples.

```tsv
file	test	primary	also
crates/aterm-tempfile/src/lib.rs	tempdir_creates_and_cleans_up	tempdir-drop-removes-keep-transfers	tempdir-creates-fresh-private-dir-under-parent
crates/aterm-tempfile/src/lib.rs	keeping_a_directory_transfers_cleanup_to_the_caller	tempdir-drop-removes-keep-transfers	tempdir-creates-fresh-private-dir-under-parent
crates/aterm-tempfile/src/lib.rs	builder_prefix	tempdir-creates-fresh-private-dir-under-parent	
crates/aterm-tempfile/src/lib.rs	a_prefix_cannot_escape_the_requested_parent	tempdir-creates-fresh-private-dir-under-parent	
crates/aterm-tempfile/src/lib.rs	stand_ins_share_one_read_only_inode_per_body	stand-in-read-only-shared-inode	unique-name-format-and-injective, tempdir-creates-fresh-private-dir-under-parent
crates/aterm-tempfile/src/lib.rs	windows_os_random_succeeds_and_differs	(none; §4)	
```

## 3. Coverage

- 7 tests read. 6 map to one of the 4 specs, and 1 is not a spec sample (§4).
- `tempdir-creates-fresh-private-dir-under-parent` has 2 primary tests and 3 secondary.
  `tempdir-drop-removes-keep-transfers` has 2 primary tests. The other two specs have 1 each.
- No test asserts the `0o700` mode, the 5-attempt `AlreadyExists` retry, or the
  pass-through of other io errors. The specs state them because the code does them. Proving them
  adds coverage that the tests never had.

## 4. Tests that are not spec samples

| test | why | recommendation |
|---|---|---|
| crates/aterm-tempfile/src/lib.rs::windows_os_random_succeeds_and_differs | It is a `cfg(windows)` smoke check of the BCryptGenRandom FFI. The "differs" assertion is probabilistic: it fails with probability 2^-64, which is not a property of the code. The "succeeds" half is an assumption about the OS. It belongs in the FFI assumption channel as an axiom on `read_os_random` (gap 8), and the Rust body cannot prove it. The only provable fact nearby is that `os_random_u64 = from_ne_bytes(buf)` when `read_os_random(buf)`, and `no_csprng_fallback()` otherwise. That is a trivial branch, and no test samples it. | **move-to-bench**: move it to a Windows environment check, outside the unit-test suite, and record the OS-RNG success assumption as the FFI contract of `read_os_random`. |

## 5. Implied specs that look wrong, or disagree with the code

1. **The doc on `unique_name` overclaims.** It says names "do not collide across processes".
   The test only samples distinctness within one process. Across processes, uniqueness rests on
   the pid plus OS randomness, or a nanosecond wall clock when no CSPRNG is available. That is
   probabilistic, not provable. The provable spec is injectivity within one process, through
   the counter suffix. The doc should say so. The `AlreadyExists` retry is what actually handles a
   cross-process collision.
2. **The windows test uses a probabilistic oracle.** `assert_ne!` on two random 8-byte reads
   can fail by chance, and the test pins an OS service rather than crate behaviour (§4).
3. **`tempdir_creates_and_cleans_up` samples a skipped body.** Its cleanup half exercises
   `Drop::drop`, which is `trust::skip`. The spec it implies can only be assumed, not proved
   about the compiled body, until the skip is lifted and gap 8 lands.
4. **`stand_in` is test-support code with a real contract.** It is
   `cfg(any(test, feature = "stand-in"))` and never in a shipped build. But other crates' tests
   reach it through the feature at 117 call sites, so it keeps a real contract and is not
   test-only code to delete. Its macOS-only branches (the shared inode, the tamper replacement,
   the failed link that keeps the copy) are unchecked on Linux, where `shared = false` always.
5. **"The directory of path" is `.` for a bare file name.** When `path.parent()` is empty,
   `stand_in` lays its temp in `.`, which is the directory a bare relative name resolves in, so
   the code is correct. The spec still has to name `D` explicitly, as §1 does, because
   `path.parent()` alone is the empty path there.
6. **The `0o700` part of `Fresh` is unix-only.** On non-unix, `with_prefix_in` uses
   `fs::create_dir` with the default permissions. The spec therefore carries the mode only under
   `cfg(unix)`.

## 6. Proof order (easiest high-value first)

No spec is blocked by gap 0 alone. The pure stepping stones come first because they need the
least beyond gap 0.

1. **`unique-name-format-and-injective`.** This is the only spec that needs no effects lane: with
   `r` havocked, it touches no filesystem. Start with the `hex_digit` table lemma: a 16-arm
   literal `match` on `u64`, decidable by evaluation. It needs
   gap 0 plus the literal-match admission the 150efe4a seal already has for `u8`. Then prove the
   16-iteration hex loop (gap 7) and the `String` contents (gap 1). Lift `trust::skip` on
   `unique_name` first. It supplies the naming lemma for both tempdir specs and for `stand_in`.
2. **The pure half of `tempdir-creates-fresh-private-dir-under-parent`:** the `prefix_str`
   match (gaps 1, 2) and the `SingleNormal(prefix ++ Sfx) ⇔ '/' ∉ prefix` lemma over a model of
   std's `Path::components`. This is the security-relevant core: a prefix can never escape the
   requested parent. It retires `a_prefix_cannot_escape_the_requested_parent` and
   `builder_prefix` once the `DirBuilder::create` effect is specified (gap 8).
3. **The pure half of `tempdir-drop-removes-keep-transfers`:** `keep` leaves an empty path, so
   the following drop is a no-op (gap 2, `mem::take`). The `remove_dir_all` half and both
   tests wait on gap 8 and on lifting the `Drop` skip.
4. **`stand-in-read-only-shared-inode`, last.** `fnv1a` is an easy pure fold that can be
   proved early, but the spec is almost entirely macOS filesystem effects with a concurrency
   argument (gaps 8, 13). It is test-support code, so it has the lowest value per unit of effort.
