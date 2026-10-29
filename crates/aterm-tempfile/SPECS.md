<!-- Copyright 2026 Andrew Yates -->
<!-- SPDX-License-Identifier: Apache-2.0 -->

# aterm-tempfile: spec recovery from its 16 unit tests

> Status 2026-10-16, after the merge with the tests-and-gates audit: the rows naming 1 test it
> deleted are gone, and the spec rows no longer cite those tests. Counts below are reconciled with
> current `main` (2026-10-29).

> Audit amendment: The 2026-10-16 code audit corrected unique_name's cross-process uniqueness claim (§5.1). The dated inventory below records the earlier snapshot; atomic-create collision retry remains the guarantee.

> Status 2026-10-16: this is the crate's migration worklist. Each of the 4 specs below is
> to be proved through Trust/Clean, and then the tests it subsumes are deleted. No test has
> been deleted yet. No spec is blocked by gap 0 alone (§1 histogram). Only
> `unique-name-format-and-injective` can retire its test without the effects lane (gap 8).

Source: `crates/aterm-tempfile/src/lib.rs` (read only). Every path below is repo-relative. It is the crate's only source file.
All 16 `#[test]` fns are in its `mod tests` (6 unconditional, 7 `cfg(unix)`, 2 `cfg(target_os = "macos")`, and 1 `cfg(windows)`). I read
every test and the code it reaches. Nothing was compiled or run. The claims below come from
reading the code.

Result: **4 specs**, covering **15 tests**. **1 test is not a spec sample** (§4).

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
- `Cache(me, tmp) = quiet_in(tmp)/("atv-standins-" ++ dec(me))`, user `me`'s stand-in cache,
  and `Slot(text, k) = Cache(me, tmp)/(hex16(fnv1a(text)) ++ "-" ++ dec(k))` for `k < 4`
  (2026-10-28: this replaced the hard-linked `temp_dir()/"aterm-stand-ins"/hex16(fnv1a(body))`).
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
tempdir-creates-fresh-private-dir-under-parent	contract	TempDir::{with_prefix_in,new_in,new,made_in}, tempdir, Builder::{new,heard,prefix,prefix_str,tempdir,tempdir_in}, is_quiet, quiet_in, quiet_place, outside_quiet, placed, is_shared_root, made_quiet, call0	(prefix_str) Builder::new().prefix_str() = ".tmp", Builder::new().heard = false, b.heard() sets heard = true, and b.prefix(p).prefix_str() = p, with the last prefix call winning. (with_prefix_in) ∀ prefix, dir, in FsEnv: (a) if ¬SingleNormal(prefix ++ Sfx) (on unix exactly '/' ∈ prefix), the result is Err(InvalidInput) on the first attempt and FsEnv is unchanged. Validity depends only on prefix, because Sfx has no separator. (b) Otherwise, on success it returns Ok(d) with Fresh(d, dir), and d.path().file_name() = Name(prefix,pid,r,c), which starts with prefix. A create that fails with AlreadyExists is retried under a new name, up to 5 attempts in all, and then the result is Err(AlreadyExists). Any other io error is returned unchanged. (instances) tempdir() = TempDir::new() = new_in(temp_dir()) = with_prefix_in(".tmp", temp_dir()). For a Builder b, b.tempdir() = b.tempdir_in(temp_dir()); when ¬b.heard, b.tempdir_in(dir) = with_prefix_in(b.prefix_str(), dir); when b.heard, b.tempdir_in(dir) = made_in(b.prefix_str(), &outside_quiet(dir)), cutting dir at its first .ubd component so the directory is made outside every quiet folder. So the prefixes "", ".", ".." and "ordinary_" are accepted, with parent = dir. (placement, 2026-10-26) On macOS, when dir is a shared temporary root (temp_dir() as named or as it resolves, /tmp, /private/tmp), "dir" in with_prefix_in reads P = quiet_in(dir): dir/.ubd when made_quiet makes or finds it usable (a real directory owned by this user and closed unless sticky, or root-owned, sticky and open to all), else dir; a dir already holding a .ubd component, any other dir, and every dir off macOS give P = dir. "../x", "a/b" and an absolute path are refused and nothing is created. On windows "..\x" and "C:x" are refused too.	crates/aterm-tempfile/src/lib.rs::builder_prefix, crates/aterm-tempfile/src/lib.rs::a_prefix_cannot_escape_the_requested_parent, crates/aterm-tempfile/src/lib.rs::made_in_sets_private_mode_retries_collisions_and_propagates_errors, crates/aterm-tempfile/src/lib.rs::a_shared_roots_directories_go_in_its_quiet_folder, crates/aterm-tempfile/src/lib.rs::tempdirs_in_shared_roots_land_where_fseventsd_drops_their_events, crates/aterm-tempfile/src/lib.rs::a_heard_directory_is_made_outside_every_quiet_folder, crates/aterm-tempfile/src/lib.rs::a_quiet_folder_is_used_only_while_it_is_safe (also the creation half of tempdir_creates_and_cleans_up)	MEDIUM pure core: the SingleNormal lemma needs a model of std's unix (and windows) Path component parser, and the OsStr==str compare inside matches!. The rest: an Option<String> field match, generic impl AsRef<Path> and call0, Path::join, a bounded for 0..5 loop with continue and early returns, io::Error construction and kind(). Effects: DirBuilder::mode(0o700).create (unix) or fs::create_dir, and std::env::temp_dir. Builder::prefix is trust::skip.	0, 1, 3, 4, 5, 7, 8, 11
tempdir-drop-removes-keep-transfers	contract	Drop for TempDir::drop, TempDir::{keep,path}, AsRef<Path> for TempDir::as_ref	∀ d:TempDir with d.path = q: path(&d) = as_ref(&d) = q. keep(d) returns q and leaves d.path = PathBuf::new() before d is dropped, so that drop does no fs operation and the tree at q, contents included, is unchanged. Dropping a TempDir with a non-empty path calls remove_dir_all(q) and ignores its result. In FsEnv, when remove_dir_all succeeds, q and everything under it no longer exist. Composed with tempdir-creates-fresh-private-dir-under-parent: after a tempdir() guard drops, its path does not exist. After keep the caller alone owns q, and once the caller removes q the parent holds exactly what it held before the child was created.	crates/aterm-tempfile/src/lib.rs::tempdir_creates_and_cleans_up, crates/aterm-tempfile/src/lib.rs::keeping_a_directory_transfers_cleanup_to_the_caller	EASY pure half: mem::take on a struct field (a two-state x'), the consuming self, then the drop-glue order, OsStr::is_empty. The cleanup half is fs::remove_dir_all, an effect. Drop is trust::skip, so until gap 8 that half can only be an assumed platform contract.	0, 1, 2, 4, 8
stand-in-read-only-shared-inode	contract	stand_in, shared_stand_in, shared_in, holds, cached_in, write_read_only, fnv1a, euid, unique_name	(stand_in is unix test-support code: cfg(test), debug_assertions or feature test-support. shared_stand_in (doc-hidden), shared_in, holds and cached_in are unix in every profile, for aterm-verify's standin.) holds(me, F, text): F is a regular file (not a link) owned by me, mode 0555, holding exactly text. cached_in(me, tmp, text): with C = Cache(me, tmp), if C cannot be made, or is not a real directory owned by me with no group or other bits (a symlink, another user's, one others may write), the result is None and nothing is made in C. Otherwise for the least k < 4 at which Slot(text, k) is absent, or holds(me, Slot(text, k), text), the result is Some(Slot(text, k)): an absent slot is created (create_new, 0600), written, closed, and only then made 0555, and a write or chmod that fails gives None. A slot that is not that file is passed over and left as it is; no slot is ever renamed, linked or removed. None when all four are passed over. shared_stand_in(text) = shared_in(M, euid, temp_dir(), text) over the process's memo M: M's file F for text while holds(euid, F, text), checked again on every call; else (none yet, or F made writable and written through, or removed) cached_in(euid, temp_dir(), text), which M names from then on, and None, text unnamed in M, when that is None. So racing first writers in several processes can run up to four slots of one text, one per process, and a later process finds the first whole one. stand_in(path, body): if path.file_name() is None, Err(InvalidInput) and nothing changes. Let D be path's parent, or "." when that parent is empty. On macOS when shared_stand_in(body) = Some(F) and a symlink to F can be made at D/.stand-in.<unique_name>, that link is renamed over path; otherwise (and always off macOS) a fresh file there holding body, mode 0555, is renamed over path. On Ok, reading or running path reads or runs body, metadata(path) is mode 0555 with link count 1, a write through path fails, and D gains no entry other than path: a failed rename removes the temp. On macOS two stand-ins of one body one process lays resolve to one F outside every fixture, link count 1, so a fixture going removes no name of the file another fixture's stand-in runs. A collision of fnv1a(text) only costs a slot, because a slot is used only when it holds the bytes. If the temp cannot be made in D (D is read-only), stand_in returns Err and F is unchanged.	crates/aterm-tempfile/src/lib.rs::stand_ins_of_one_body_run_one_file_of_one_name, crates/aterm-tempfile/src/lib.rs::a_stand_in_laid_again_with_another_body_runs_that_body, crates/aterm-tempfile/src/lib.rs::a_stand_in_file_that_no_longer_holds_its_text_is_passed_over, crates/aterm-tempfile/src/lib.rs::the_stand_in_cache_holds_one_read_only_file_per_text, crates/aterm-tempfile/src/lib.rs::a_slot_still_being_written_or_holding_another_text_is_passed_over, crates/aterm-tempfile/src/lib.rs::a_cache_folder_not_the_users_alone_is_refused	EASY fnv1a (a slice fold with wrapping_mul). HARD rest: Path/&[u8]/String, Vec<u8> compare, format! hex naming, a static Mutex<BTreeMap> memo, fs symlink_metadata/metadata/read/rename/symlink/remove_file, DirBuilder and OpenOptions create_new+mode, set_permissions, MetadataExt uid/mode, cfg(target_os = "macos") branches. Every claim except fnv1a is a filesystem effect. The race freedom the code argues (create_new, a slot used only once whole and 0555, no slot ever removed) needs a concurrent FS model.	0, 1, 3, 4, 7, 8, 13
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
| 4 | 4 | all (each spec composes callee contracts: hex_digit, unique_name, with_prefix_in, write_read_only/cached_in, keep→drop) |
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

## 2. Test → spec map (TSV)

`primary` is the spec that retires the test. `also` lists the other specs the same test samples.

```tsv
file	test	primary	also
crates/aterm-tempfile/src/lib.rs	tempdir_creates_and_cleans_up	tempdir-drop-removes-keep-transfers	tempdir-creates-fresh-private-dir-under-parent
crates/aterm-tempfile/src/lib.rs	keeping_a_directory_transfers_cleanup_to_the_caller	tempdir-drop-removes-keep-transfers	tempdir-creates-fresh-private-dir-under-parent
crates/aterm-tempfile/src/lib.rs	builder_prefix	tempdir-creates-fresh-private-dir-under-parent	
crates/aterm-tempfile/src/lib.rs	a_prefix_cannot_escape_the_requested_parent	tempdir-creates-fresh-private-dir-under-parent	
crates/aterm-tempfile/src/lib.rs	a_shared_roots_directories_go_in_its_quiet_folder	tempdir-creates-fresh-private-dir-under-parent	
crates/aterm-tempfile/src/lib.rs	tempdirs_in_shared_roots_land_where_fseventsd_drops_their_events	tempdir-creates-fresh-private-dir-under-parent	tempdir-drop-removes-keep-transfers
crates/aterm-tempfile/src/lib.rs	a_heard_directory_is_made_outside_every_quiet_folder	tempdir-creates-fresh-private-dir-under-parent	
crates/aterm-tempfile/src/lib.rs	a_quiet_folder_is_used_only_while_it_is_safe	tempdir-creates-fresh-private-dir-under-parent	
crates/aterm-tempfile/src/lib.rs	made_in_sets_private_mode_retries_collisions_and_propagates_errors	tempdir-creates-fresh-private-dir-under-parent	
crates/aterm-tempfile/src/lib.rs	stand_ins_of_one_body_run_one_file_of_one_name	stand-in-read-only-shared-inode	unique-name-format-and-injective, tempdir-creates-fresh-private-dir-under-parent, tempdir-drop-removes-keep-transfers
crates/aterm-tempfile/src/lib.rs	a_stand_in_laid_again_with_another_body_runs_that_body	stand-in-read-only-shared-inode	tempdir-creates-fresh-private-dir-under-parent
crates/aterm-tempfile/src/lib.rs	a_stand_in_file_that_no_longer_holds_its_text_is_passed_over	stand-in-read-only-shared-inode	
crates/aterm-tempfile/src/lib.rs	the_stand_in_cache_holds_one_read_only_file_per_text	stand-in-read-only-shared-inode	
crates/aterm-tempfile/src/lib.rs	a_slot_still_being_written_or_holding_another_text_is_passed_over	stand-in-read-only-shared-inode	
crates/aterm-tempfile/src/lib.rs	a_cache_folder_not_the_users_alone_is_refused	stand-in-read-only-shared-inode	
crates/aterm-tempfile/src/lib.rs	windows_os_random_succeeds	(none; §4)	
```

## 3. Coverage

- 16 `#[test]` functions in `src/lib.rs` (including the `.ubd` quiet-folder suite and the stand-in suite of
  2026-10-28, plus the non-`#[test]` child helper `stand_in_cache_child_if_asked`). `tempdir-creates-fresh-private-dir-under-parent`
  now directly tests Unix `0o700` mode, the 5-attempt `AlreadyExists` collision retry and exhaustion, and immediate
  propagation of other `io::Error` kinds (`made_in_sets_private_mode_retries_collisions_and_propagates_errors`).

## 4. Tests that are not spec samples

| test | why | recommendation |
|---|---|---|
| crates/aterm-tempfile/src/lib.rs::windows_os_random_succeeds | It is a `cfg(windows)` smoke check of the BCryptGenRandom FFI (without the former probabilistic `assert_ne!`). It belongs in the FFI assumption channel as an axiom on `read_os_random` (gap 8). | **move-to-bench**: move it to a Windows environment check, outside the unit-test suite, and record the OS-RNG success assumption as the FFI contract of `read_os_random`. |

## 5. Implied specs that look wrong, or disagree with the code

1. **Resolved — `unique_name` doc.** Updated `unique_name`'s doc to state intra-process injectivity via the
   atomic counter and cross-process collision retry in `TempDir::made_in`.
2. **Resolved — probabilistic Windows RNG assertion.** Renamed to `windows_os_random_succeeds` and dropped
   the `2^-64` `assert_ne!` oracle; it now checks deterministic FFI call success on empty and 8-byte slices (§4).
3. **`tempdir_creates_and_cleans_up` samples a skipped body.** Its cleanup half exercises
   `Drop::drop`, which is `trust::skip`. The spec it implies can only be assumed, not proved
   about the compiled body, until the skip is lifted and gap 8 lands.
4. **`stand_in` is test-support code with a real contract.** It is
   `cfg(all(unix, any(test, debug_assertions, feature = "test-support")))`, so no release build
   of a crate that leaves the feature off compiles it. Other crates' tests reach it at 14 call
   sites, one of them install.sh's suite's `Scratch::stand_in` (aterm-update's
   `tests/install_sh`, which aterm-bootstrap-verify's includes), so it keeps a real contract
   and is not test-only code to delete. The cache under it (`shared_stand_in`, `shared_in`,
   `holds`, `cached_in`, `fnv1a`, `STAND_IN_CACHE`) compiles in every unix profile, the
   aterm-tempfile the shipped `aterm` links included, because aterm-verify's `standin` uses it
   in every profile aterm-verify builds in; `shared_stand_in` is `#[doc(hidden)]`, as
   `standin` is, and no shipped code calls it. Its macOS-only branch (the symlink to the shared file) is
   unchecked on Linux, where `shared = false` always; the cache itself (`cached_in`, the slot
   and folder rules) is checked on every unix, since aterm-verify's `standin` uses it there.
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
