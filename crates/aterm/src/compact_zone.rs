// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Compact `mmap`-backed slab and large-block allocator for `aterm` on macOS.
//!
//! macOS 27's `libsystem_malloc` (`xzone`) shards small allocations across CPU clusters and
//! type/size buckets, faulting ~14–15 MiB of empty `Malloc Small` pages across an 18-core Mac
//! even when `aterm`'s entire live Rust heap is ~3 MiB. `CompactZoneAlloc` serves small layouts
//! (`<= 4 KiB`) from lock-free per-thread magazines backed by shared `os_unfair_lock`-protected
//! `16 KiB` slabs that `munmap` emptied slabs back to XNU, and serves larger layouts (`<= 512 KiB`)
//! via an O(1) page-bucketed `mmap` recycle cache drained on [`release_free_memory`].

use std::alloc::{GlobalAlloc, Layout};
use std::cell::UnsafeCell;
use std::ffi::c_void;
use std::ptr::null_mut;
use std::sync::atomic::{AtomicU8, AtomicUsize, Ordering};

const MIN_ALIGN: usize = 16;
const SLAB_BYTES: usize = 16_384;
const HEADER_BYTES: usize = 64;
const USABLE_SLAB_BYTES: usize = SLAB_BYTES - HEADER_BYTES;
const PAGE_ALIGN_FLOOR: usize = if cfg!(target_arch = "aarch64") {
    16_384
} else {
    4_096
};
const MAX_CLASS: usize = 4_096;
const MAX_ALIGNED_SLAB: usize = 4_096;

const NUM_CLASSES: usize = 16;
const CLASSES: [usize; NUM_CLASSES] = [
    16, 32, 48, 64, 96, 128, 192, 256, 384, 512, 768, 1024, 1536, 2048, 3072, 4096,
];
const MAG_CAPS: [u16; NUM_CLASSES] = [64, 64, 64, 64, 48, 48, 32, 32, 24, 24, 16, 16, 12, 12, 8, 8];
const SLAB_CAPS: [u16; NUM_CLASSES] = {
    let mut caps = [0u16; NUM_CLASSES];
    let mut i = 0;
    while i < NUM_CLASSES {
        let cls = CLASSES[i];
        assert!(cls >= MIN_ALIGN);
        assert!(cls.is_multiple_of(MIN_ALIGN));
        let cap = USABLE_SLAB_BYTES / cls;
        assert!(cap >= 3 && cap <= u16::MAX as usize);
        assert!(cap * cls <= USABLE_SLAB_BYTES);
        caps[i] = cap as u16;
        i += 1;
    }
    caps
};

/// Fast index table for `align <= 16` and `1 <= size <= 256` indexed by `(size - 1) >> 4`.
const SMALL_LUT: [u8; 16] = [0, 1, 2, 3, 4, 4, 5, 5, 6, 6, 6, 6, 7, 7, 7, 7];

type PthreadKey = libc::c_ulong;

#[repr(C)]
struct OsUnfairLock(u32);

unsafe extern "C" {
    fn os_unfair_lock_lock(lock: *mut OsUnfairLock);
    fn os_unfair_lock_unlock(lock: *mut OsUnfairLock);
    fn pthread_key_create(
        key: *mut PthreadKey,
        dtor: Option<unsafe extern "C" fn(*mut c_void)>,
    ) -> libc::c_int;
    fn pthread_getspecific(key: PthreadKey) -> *mut c_void;
    fn pthread_setspecific(key: PthreadKey, value: *const c_void) -> libc::c_int;
}

#[repr(C)]
struct FreeNode {
    next: *mut FreeNode,
}

#[repr(C)]
struct SlabHeader {
    prev: *mut SlabHeader,
    next: *mut SlabHeader,
    free_head: *mut FreeNode,
    bump: u16,
    capacity: u16,
    live: u16,
    in_partial: bool,
}

const _: () = assert!(std::mem::size_of::<SlabHeader>() <= HEADER_BYTES);
const _: () = assert!(std::mem::align_of::<SlabHeader>() <= MIN_ALIGN);

struct BinState {
    lock: OsUnfairLock,
    partial_head: *mut SlabHeader,
}

#[repr(align(128))]
struct Bin(UnsafeCell<BinState>);

// SAFETY: Every access to `BinState` fields goes through `os_unfair_lock_lock` /
// `os_unfair_lock_unlock` on `BinState::lock`, serializing concurrent callers across threads.
unsafe impl Sync for Bin {}

impl Bin {
    const fn new() -> Self {
        Self(UnsafeCell::new(BinState {
            lock: OsUnfairLock(0),
            partial_head: null_mut(),
        }))
    }
}

static BINS: [Bin; NUM_CLASSES] = [const { Bin::new() }; NUM_CLASSES];
static CACHED_PAGE_SIZE: AtomicUsize = AtomicUsize::new(0);

#[derive(Clone, Copy)]
#[repr(C)]
struct LocalBin {
    head: *mut FreeNode,
    len: u16,
}

#[repr(C)]
struct ThreadCache {
    bins: [LocalBin; NUM_CLASSES],
}

const TLS_UNINIT: u8 = 0;
const TLS_INITING: u8 = 1;
const TLS_READY: u8 = 2;
const TLS_DISABLED: u8 = 3;
const TLS_SENTINEL: usize = 1;

static TLS_STATE: AtomicU8 = AtomicU8::new(TLS_UNINIT);
static TLS_KEY: AtomicUsize = AtomicUsize::new(0);

#[inline]
#[cfg_attr(trust_verify, trust::skip)]
fn page_size() -> usize {
    let cached = CACHED_PAGE_SIZE.load(Ordering::Relaxed);
    if cached != 0 {
        return cached;
    }
    init_page_size()
}

#[cold]
#[cfg_attr(trust_verify, trust::skip)]
fn init_page_size() -> usize {
    // SAFETY: `sysconf(_SC_PAGESIZE)` has no preconditions and reads a kernel constant.
    let raw = unsafe { libc::sysconf(libc::_SC_PAGESIZE) };
    let ps = usize::try_from(raw)
        .ok()
        .filter(|v| v.is_power_of_two())
        .unwrap_or(PAGE_ALIGN_FLOOR)
        .max(PAGE_ALIGN_FLOOR);
    CACHED_PAGE_SIZE.store(ps, Ordering::Relaxed);
    ps
}

#[inline]
const fn round_up_pow2(n: usize, pow2: usize) -> Option<usize> {
    let base = if n == 0 { 1 } else { n };
    let Some(sum) = base.checked_add(pow2 - 1) else {
        return None;
    };
    Some(sum & !(pow2 - 1))
}

#[inline]
const fn class_index(size: usize, align: usize) -> Option<usize> {
    let s = if size == 0 { 1 } else { size };
    if align <= MIN_ALIGN {
        if s <= 256 {
            return Some(SMALL_LUT[(s - 1) >> 4] as usize);
        }
        if s > MAX_CLASS {
            return None;
        }
        let mut i = 8;
        while i < NUM_CLASSES {
            if CLASSES[i] >= s {
                return Some(i);
            }
            i += 1;
        }
        None
    } else {
        if align > MAX_ALIGNED_SLAB || s > MAX_ALIGNED_SLAB {
            return None;
        }
        let need_raw = if s > align { s } else { align };
        let need = need_raw.next_power_of_two();
        let mut i = 0;
        while i < NUM_CLASSES {
            if CLASSES[i] == need {
                return Some(i);
            }
            i += 1;
        }
        None
    }
}

#[inline]
#[cfg_attr(trust_verify, trust::skip)]
fn mmap_anon(len: usize) -> *mut u8 {
    // SAFETY: Anonymous private mapping with fd `-1` and offset `0` creates a fresh page-aligned,
    // zero-filled virtual memory region of `len` bytes without touching existing mappings.
    let p = unsafe {
        libc::mmap(
            null_mut(),
            len,
            libc::PROT_READ | libc::PROT_WRITE,
            libc::MAP_PRIVATE | libc::MAP_ANON,
            -1,
            0,
        )
    };
    if p == libc::MAP_FAILED {
        null_mut()
    } else {
        p.cast()
    }
}

#[cold]
#[cfg_attr(trust_verify, trust::skip)]
fn mmap_aligned(len: usize, align: usize) -> *mut u8 {
    let Some(total) = len.checked_add(align) else {
        return null_mut();
    };
    let raw = mmap_anon(total);
    if raw.is_null() {
        return null_mut();
    }
    let addr = raw as usize;
    let aligned = (addr + (align - 1)) & !(align - 1);
    let prefix = aligned - addr;
    // SAFETY: `raw` is a fresh page-aligned `mmap` of `total` bytes, and `align` and `len` are
    // both multiples of `page_size()`, so `[addr, aligned)` and `[aligned + len, addr + total)`
    // are exact page-aligned subranges of `raw` leaving `[aligned, aligned + len)` mapped.
    unsafe {
        if prefix > 0 {
            let _ = libc::munmap(raw.cast(), prefix);
        }
        let suffix = total - prefix - len;
        if suffix > 0 {
            let _ = libc::munmap(raw.add(prefix + len).cast::<c_void>(), suffix);
        }
        raw.add(prefix)
    }
}

#[inline]
#[cfg_attr(trust_verify, trust::skip)]
fn mmap_slab() -> *mut u8 {
    let raw = mmap_anon(SLAB_BYTES);
    if raw.is_null() || ((raw as usize) & (SLAB_BYTES - 1)) == 0 {
        return raw;
    }
    // SAFETY: `raw` was just returned by `mmap_anon(SLAB_BYTES)` and has not been exposed.
    unsafe {
        let _ = libc::munmap(raw.cast(), SLAB_BYTES);
    }
    mmap_aligned(SLAB_BYTES, SLAB_BYTES)
}

#[inline]
#[cfg_attr(trust_verify, trust::skip)]
unsafe fn header_from_slab_base(slab_base: *mut u8) -> *mut SlabHeader {
    // SAFETY: `slab_base` owns `[slab_base, slab_base + SLAB_BYTES)` and `USABLE_SLAB_BYTES` is
    // 64-byte aligned, placing `SlabHeader` in `[slab_base + USABLE_SLAB_BYTES, slab_base + SLAB_BYTES)`.
    unsafe { slab_base.add(USABLE_SLAB_BYTES).cast::<SlabHeader>() }
}

#[inline]
#[cfg_attr(trust_verify, trust::skip)]
unsafe fn slab_base_from_header(hdr: *mut SlabHeader) -> *mut u8 {
    // SAFETY: `hdr` was derived from `slab_base.add(USABLE_SLAB_BYTES)`.
    unsafe { hdr.cast::<u8>().sub(USABLE_SLAB_BYTES) }
}

#[inline]
#[cfg_attr(trust_verify, trust::skip)]
unsafe fn push_partial_front(state: &mut BinState, hdr: *mut SlabHeader) {
    // SAFETY: Caller holds `state.lock`, and `hdr` is not currently in `state.partial_head`.
    unsafe {
        let old = state.partial_head;
        (*hdr).prev = null_mut();
        (*hdr).next = old;
        (*hdr).in_partial = true;
        if !old.is_null() {
            (*old).prev = hdr;
        }
        state.partial_head = hdr;
    }
}

#[inline]
#[cfg_attr(trust_verify, trust::skip)]
unsafe fn unlink_partial(state: &mut BinState, hdr: *mut SlabHeader) {
    // SAFETY: Caller holds `state.lock`, and `(*hdr).in_partial` is true.
    unsafe {
        let prev = (*hdr).prev;
        let next = (*hdr).next;
        if !prev.is_null() {
            (*prev).next = next;
        } else {
            state.partial_head = next;
        }
        if !next.is_null() {
            (*next).prev = prev;
        }
        (*hdr).prev = null_mut();
        (*hdr).next = null_mut();
        (*hdr).in_partial = false;
    }
}

#[inline]
#[cfg_attr(trust_verify, trust::skip)]
unsafe fn pop_one_from_partial(state: &mut BinState, slot_bytes: usize) -> *mut u8 {
    // SAFETY: Caller holds `state.lock`.
    unsafe {
        let hdr = state.partial_head;
        if hdr.is_null() {
            return null_mut();
        }
        let slab_base = slab_base_from_header(hdr);
        let ptr = if !(*hdr).free_head.is_null() {
            let node = (*hdr).free_head;
            (*hdr).free_head = (*node).next;
            node.cast::<u8>()
        } else {
            let b = (*hdr).bump as usize;
            (*hdr).bump = (b + 1) as u16;
            slab_base.add(b * slot_bytes)
        };
        (*hdr).live += 1;
        if (*hdr).free_head.is_null() && (*hdr).bump == (*hdr).capacity {
            unlink_partial(state, hdr);
        }
        ptr
    }
}

#[inline]
#[cfg_attr(trust_verify, trust::skip)]
unsafe fn return_one_to_state(state: &mut BinState, ptr: *mut u8) -> *mut u8 {
    let slab_base = ptr.wrapping_sub((ptr as usize) & (SLAB_BYTES - 1));
    // SAFETY: Caller holds `state.lock`, and `ptr` belongs to `slab_base` in this bin.
    unsafe {
        let hdr = header_from_slab_base(slab_base);
        let new_live = (*hdr).live - 1;
        (*hdr).live = new_live;
        if new_live == 0 {
            let has_other_partial = if (*hdr).in_partial {
                !(*hdr).prev.is_null() || !(*hdr).next.is_null()
            } else {
                !state.partial_head.is_null()
            };
            if has_other_partial {
                if (*hdr).in_partial {
                    unlink_partial(state, hdr);
                }
                return slab_base;
            }
            (*hdr).free_head = null_mut();
            (*hdr).bump = 0;
            if !(*hdr).in_partial {
                push_partial_front(state, hdr);
            }
            null_mut()
        } else {
            let node = ptr.cast::<FreeNode>();
            (*node).next = (*hdr).free_head;
            (*hdr).free_head = node;
            let mut unmap = null_mut();
            if !(*hdr).in_partial {
                let old_head = state.partial_head;
                if !old_head.is_null() && (*old_head).live == 0 && (*old_head).next.is_null() {
                    unlink_partial(state, old_head);
                    unmap = slab_base_from_header(old_head);
                }
                push_partial_front(state, hdr);
            }
            unmap
        }
    }
}

#[cold]
#[cfg_attr(trust_verify, trust::skip)]
fn alloc_from_bin_global(idx: usize, req_size: usize, zeroed: bool) -> *mut u8 {
    let slot_bytes = CLASSES[idx];
    let cell = BINS[idx].0.get();
    // SAFETY: `os_unfair_lock` serializes `BinState` access, and fresh slabs are initialized
    // before joining `partial_head`.
    unsafe {
        os_unfair_lock_lock(&raw mut (*cell).lock);
        let ptr = pop_one_from_partial(&mut *cell, slot_bytes);
        os_unfair_lock_unlock(&raw mut (*cell).lock);
        if !ptr.is_null() {
            if zeroed {
                std::ptr::write_bytes(ptr, 0, req_size);
            }
            return ptr;
        }
        let slab_base = mmap_slab();
        if slab_base.is_null() {
            return null_mut();
        }
        let hdr = header_from_slab_base(slab_base);
        hdr.write(SlabHeader {
            prev: null_mut(),
            next: null_mut(),
            free_head: null_mut(),
            bump: 1,
            capacity: SLAB_CAPS[idx],
            live: 1,
            in_partial: false,
        });
        os_unfair_lock_lock(&raw mut (*cell).lock);
        push_partial_front(&mut *cell, hdr);
        os_unfair_lock_unlock(&raw mut (*cell).lock);
        slab_base
    }
}

#[cold]
#[cfg_attr(trust_verify, trust::skip)]
fn dealloc_to_bin_global(idx: usize, ptr: *mut u8) {
    let cell = BINS[idx].0.get();
    // SAFETY: `os_unfair_lock` serializes `BinState` access, and any evicted empty slab is
    // unmapped after releasing the lock.
    unsafe {
        os_unfair_lock_lock(&raw mut (*cell).lock);
        let unmap_base = return_one_to_state(&mut *cell, ptr);
        os_unfair_lock_unlock(&raw mut (*cell).lock);
        if !unmap_base.is_null() {
            let _ = libc::munmap(unmap_base.cast(), SLAB_BYTES);
        }
    }
}

#[cold]
#[cfg_attr(trust_verify, trust::skip)]
unsafe extern "C" fn tls_dtor(raw: *mut c_void) {
    if raw.is_null() || (raw as usize) == TLS_SENTINEL {
        return;
    }
    let key = TLS_KEY.load(Ordering::Relaxed) as PthreadKey;
    // SAFETY: `raw` is this exiting thread's `mmap`-backed `ThreadCache` page; setting `TLS_SENTINEL`
    // prevents re-creating a cache if later destructors allocate, and all cached slots are returned
    // to `BINS` before unmapping the page.
    unsafe {
        let _ = pthread_setspecific(key, TLS_SENTINEL as *const c_void);
        let tc = raw.cast::<ThreadCache>();
        for idx in 0..NUM_CLASSES {
            flush_local_bin(idx, &mut (*tc).bins[idx], 0);
        }
        let _ = libc::munmap(raw, page_size());
    }
}

#[cold]
#[cfg_attr(trust_verify, trust::skip)]
fn init_or_get_tls_cache() -> *mut ThreadCache {
    let mut state = TLS_STATE.load(Ordering::Acquire);
    if state == TLS_UNINIT {
        if TLS_STATE
            .compare_exchange(TLS_UNINIT, TLS_INITING, Ordering::AcqRel, Ordering::Acquire)
            .is_ok()
        {
            let mut key: PthreadKey = 0;
            // SAFETY: `pthread_key_create` initializes a fresh process-wide key with `tls_dtor`.
            let rc = unsafe { pthread_key_create(&raw mut key, Some(tls_dtor)) };
            if rc == 0 {
                TLS_KEY.store(key as usize, Ordering::Release);
                TLS_STATE.store(TLS_READY, Ordering::Release);
                state = TLS_READY;
            } else {
                TLS_STATE.store(TLS_DISABLED, Ordering::Release);
                return null_mut();
            }
        } else {
            return null_mut();
        }
    } else if state != TLS_READY {
        return null_mut();
    }
    debug_assert_eq!(state, TLS_READY);
    let key = TLS_KEY.load(Ordering::Relaxed) as PthreadKey;
    // SAFETY: `key` was created by `pthread_key_create` before `TLS_READY` was published.
    let raw = unsafe { pthread_getspecific(key) };
    if (raw as usize) == TLS_SENTINEL {
        return null_mut();
    }
    if !raw.is_null() {
        return raw.cast::<ThreadCache>();
    }
    let page = mmap_anon(page_size());
    if page.is_null() {
        return null_mut();
    }
    const _: () = assert!(std::mem::size_of::<ThreadCache>() <= PAGE_ALIGN_FLOOR);
    // SAFETY: `page` is a fresh zero-filled `MAP_ANON` page (`head = null`, `len = 0` for all bins);
    // if `pthread_setspecific` fails we unmap `page` immediately.
    unsafe {
        if pthread_setspecific(key, page.cast()) != 0 {
            let _ = libc::munmap(page.cast(), page_size());
            return null_mut();
        }
    }
    page.cast::<ThreadCache>()
}

#[inline]
#[cfg_attr(trust_verify, trust::skip)]
fn current_thread_cache() -> *mut ThreadCache {
    if TLS_STATE.load(Ordering::Acquire) == TLS_READY {
        let key = TLS_KEY.load(Ordering::Relaxed) as PthreadKey;
        // SAFETY: `TLS_READY` guarantees `TLS_KEY` holds a live `PthreadKey`.
        let raw = unsafe { pthread_getspecific(key) };
        if !raw.is_null() && (raw as usize) != TLS_SENTINEL {
            return raw.cast::<ThreadCache>();
        }
        if (raw as usize) == TLS_SENTINEL {
            return null_mut();
        }
    }
    init_or_get_tls_cache()
}

#[cold]
#[cfg_attr(trust_verify, trust::skip)]
unsafe fn refill_local_bin(
    idx: usize,
    local: &mut LocalBin,
    req_size: usize,
    zeroed: bool,
) -> *mut u8 {
    let slot_bytes = CLASSES[idx];
    let want = (MAG_CAPS[idx] / 2).max(1) as usize;
    let cell = BINS[idx].0.get();
    let mut head: *mut FreeNode = null_mut();
    let mut got = 0usize;
    let mut first: *mut u8 = null_mut();
    // SAFETY: `os_unfair_lock` serializes `BinState` access while popping up to `want` slots; if
    // `partial_head` runs dry before `got > 0`, a fresh slab is mapped outside the lock and its
    // remaining slots are published under the lock.
    unsafe {
        os_unfair_lock_lock(&raw mut (*cell).lock);
        while got < want {
            let p = pop_one_from_partial(&mut *cell, slot_bytes);
            if p.is_null() {
                break;
            }
            if got == 0 {
                first = p;
            } else {
                let node = p.cast::<FreeNode>();
                (*node).next = head;
                head = node;
            }
            got += 1;
        }
        os_unfair_lock_unlock(&raw mut (*cell).lock);

        if got > 0 {
            local.head = head;
            local.len = (got - 1) as u16;
            if zeroed {
                std::ptr::write_bytes(first, 0, req_size);
            }
            return first;
        }

        let slab_base = mmap_slab();
        if slab_base.is_null() {
            return null_mut();
        }
        let cap = SLAB_CAPS[idx] as usize;
        let take = want.min(cap);
        for s in 1..take {
            let node = slab_base.add(s * slot_bytes).cast::<FreeNode>();
            (*node).next = head;
            head = node;
        }
        let hdr = header_from_slab_base(slab_base);
        hdr.write(SlabHeader {
            prev: null_mut(),
            next: null_mut(),
            free_head: null_mut(),
            bump: take as u16,
            capacity: SLAB_CAPS[idx],
            live: take as u16,
            in_partial: false,
        });
        if take < cap {
            os_unfair_lock_lock(&raw mut (*cell).lock);
            push_partial_front(&mut *cell, hdr);
            os_unfair_lock_unlock(&raw mut (*cell).lock);
        }
        local.head = head;
        local.len = (take - 1) as u16;
        // Fresh `MAP_ANON` slab: slot 0 is already zero-filled by XNU.
        slab_base
    }
}

#[cold]
#[cfg_attr(trust_verify, trust::skip)]
unsafe fn flush_local_bin(idx: usize, local: &mut LocalBin, keep: u16) {
    if local.len <= keep {
        return;
    }
    let cell = BINS[idx].0.get();
    // SAFETY: Caller owns `local` on the current thread; each batch of up to 32 slots is returned
    // under `BINS[idx].lock` and any emptied slabs are unmapped outside the lock.
    unsafe {
        while local.len > keep {
            let mut unmaps = [null_mut::<u8>(); 32];
            let mut unmap_n = 0usize;
            let batch = ((local.len - keep) as usize).min(32);
            os_unfair_lock_lock(&raw mut (*cell).lock);
            for _ in 0..batch {
                let node = local.head;
                if node.is_null() {
                    local.len = 0;
                    break;
                }
                local.head = (*node).next;
                local.len -= 1;
                let unmap = return_one_to_state(&mut *cell, node.cast::<u8>());
                if !unmap.is_null() {
                    unmaps[unmap_n] = unmap;
                    unmap_n += 1;
                }
            }
            os_unfair_lock_unlock(&raw mut (*cell).lock);
            for &u in &unmaps[..unmap_n] {
                let _ = libc::munmap(u.cast(), SLAB_BYTES);
            }
        }
    }
}

#[inline]
#[cfg_attr(trust_verify, trust::skip)]
fn alloc_from_bin(idx: usize, req_size: usize, zeroed: bool) -> *mut u8 {
    let tc = current_thread_cache();
    if tc.is_null() {
        return alloc_from_bin_global(idx, req_size, zeroed);
    }
    // SAFETY: `tc` belongs exclusively to the calling OS thread.
    unsafe {
        let local = &mut (*tc).bins[idx];
        let node = local.head;
        if !node.is_null() {
            local.head = (*node).next;
            local.len -= 1;
            let ptr = node.cast::<u8>();
            if zeroed {
                std::ptr::write_bytes(ptr, 0, req_size);
            }
            return ptr;
        }
        refill_local_bin(idx, local, req_size, zeroed)
    }
}

#[inline]
#[cfg_attr(trust_verify, trust::skip)]
fn dealloc_to_bin(idx: usize, ptr: *mut u8) {
    let tc = current_thread_cache();
    if tc.is_null() {
        dealloc_to_bin_global(idx, ptr);
        return;
    }
    // SAFETY: `tc` belongs exclusively to the calling OS thread, and `ptr` is a live `>= 16`-byte
    // slot from `BINS[idx]`.
    unsafe {
        let local = &mut (*tc).bins[idx];
        if local.len >= MAG_CAPS[idx] {
            flush_local_bin(idx, local, MAG_CAPS[idx] / 2);
        }
        let node = ptr.cast::<FreeNode>();
        (*node).next = local.head;
        local.head = node;
        local.len += 1;
    }
}

const LARGE_CACHE_MAX_BLOCK: usize = 512 * 1024;
const LARGE_CACHE_BUCKETS: usize = LARGE_CACHE_MAX_BLOCK / 4096;
const LARGE_CACHE_SLOTS: usize = 128;
const LARGE_CACHE_MAX_BYTES: usize = 6 * 1024 * 1024;
const NONE: u16 = u16::MAX;

#[derive(Clone, Copy)]
struct LargeEntry {
    ptr: *mut u8,
    len: usize,
    bucket: u16,
    bucket_prev: u16,
    bucket_next: u16,
    lru_prev: u16,
    lru_next: u16,
}

impl LargeEntry {
    const EMPTY: Self = Self {
        ptr: null_mut(),
        len: 0,
        bucket: NONE,
        bucket_prev: NONE,
        bucket_next: NONE,
        lru_prev: NONE,
        lru_next: NONE,
    };
}

struct LargeCacheState {
    lock: OsUnfairLock,
    initialized: bool,
    free_head: u16,
    lru_head: u16,
    lru_tail: u16,
    count: usize,
    total_bytes: usize,
    buckets: [u16; LARGE_CACHE_BUCKETS],
    entries: [LargeEntry; LARGE_CACHE_SLOTS],
}

struct LargeCache(UnsafeCell<LargeCacheState>);

// SAFETY: Every access to `LargeCacheState` fields goes through `os_unfair_lock_lock` /
// `os_unfair_lock_unlock` on `LargeCacheState::lock`, serializing concurrent callers.
unsafe impl Sync for LargeCache {}

static LARGE_CACHE: LargeCache = LargeCache(UnsafeCell::new(LargeCacheState {
    lock: OsUnfairLock(0),
    initialized: false,
    free_head: NONE,
    lru_head: NONE,
    lru_tail: NONE,
    count: 0,
    total_bytes: 0,
    buckets: [NONE; LARGE_CACHE_BUCKETS],
    entries: [LargeEntry::EMPTY; LARGE_CACHE_SLOTS],
}));

impl LargeCacheState {
    #[inline]
    fn ensure_init(&mut self) {
        if self.initialized {
            return;
        }
        let mut i = 0usize;
        while i < LARGE_CACHE_SLOTS {
            self.entries[i].bucket_next = if i + 1 < LARGE_CACHE_SLOTS {
                (i + 1) as u16
            } else {
                NONE
            };
            i += 1;
        }
        self.free_head = 0;
        self.initialized = true;
    }

    #[inline]
    fn unlink_entry(&mut self, slot: u16) -> (*mut u8, usize) {
        let e = self.entries[slot as usize];
        let b = e.bucket as usize;
        if e.bucket_prev != NONE {
            self.entries[e.bucket_prev as usize].bucket_next = e.bucket_next;
        } else {
            self.buckets[b] = e.bucket_next;
        }
        if e.bucket_next != NONE {
            self.entries[e.bucket_next as usize].bucket_prev = e.bucket_prev;
        }
        if e.lru_prev != NONE {
            self.entries[e.lru_prev as usize].lru_next = e.lru_next;
        } else {
            self.lru_head = e.lru_next;
        }
        if e.lru_next != NONE {
            self.entries[e.lru_next as usize].lru_prev = e.lru_prev;
        } else {
            self.lru_tail = e.lru_prev;
        }
        self.entries[slot as usize] = LargeEntry {
            bucket_next: self.free_head,
            ..LargeEntry::EMPTY
        };
        self.free_head = slot;
        self.count -= 1;
        self.total_bytes -= e.len;
        (e.ptr, e.len)
    }

    #[inline]
    fn push_entry(&mut self, b: usize, ptr: *mut u8, len: usize) {
        let slot = self.free_head;
        debug_assert_ne!(slot, NONE);
        self.free_head = self.entries[slot as usize].bucket_next;

        let old_b_head = self.buckets[b];
        if old_b_head != NONE {
            self.entries[old_b_head as usize].bucket_prev = slot;
        }
        self.buckets[b] = slot;

        let old_tail = self.lru_tail;
        if old_tail != NONE {
            self.entries[old_tail as usize].lru_next = slot;
        } else {
            self.lru_head = slot;
        }
        self.lru_tail = slot;

        self.entries[slot as usize] = LargeEntry {
            ptr,
            len,
            bucket: b as u16,
            bucket_prev: NONE,
            bucket_next: old_b_head,
            lru_prev: old_tail,
            lru_next: NONE,
        };
        self.count += 1;
        self.total_bytes += len;
    }
}

#[inline]
#[cfg_attr(trust_verify, trust::skip)]
fn alloc_large(layout: Layout, zeroed: bool) -> *mut u8 {
    let ps = page_size();
    let Some(len) = round_up_pow2(layout.size(), ps) else {
        return null_mut();
    };
    let align = layout.align();
    if align <= ps && len <= LARGE_CACHE_MAX_BLOCK {
        let b = (len / ps) - 1;
        let cell = LARGE_CACHE.0.get();
        // SAFETY: `os_unfair_lock` serializes `LARGE_CACHE`; any popped block owns a live
        // `ps`-aligned `MAP_ANON` mapping of exact length `len >= layout.size()`.
        unsafe {
            os_unfair_lock_lock(&raw mut (*cell).lock);
            (*cell).ensure_init();
            let head = (*cell).buckets[b];
            if head != NONE {
                let (ptr, _) = (*cell).unlink_entry(head);
                os_unfair_lock_unlock(&raw mut (*cell).lock);
                if zeroed {
                    std::ptr::write_bytes(ptr, 0, layout.size());
                }
                return ptr;
            }
            os_unfair_lock_unlock(&raw mut (*cell).lock);
        }
    }
    if align <= ps {
        return mmap_anon(len);
    }
    mmap_aligned(len, align)
}

#[inline]
#[cfg_attr(trust_verify, trust::skip)]
unsafe fn dealloc_large(ptr: *mut u8, layout: Layout) {
    let ps = page_size();
    let Some(len) = round_up_pow2(layout.size(), ps) else {
        return;
    };
    if layout.align() <= ps && len <= LARGE_CACHE_MAX_BLOCK {
        let b = (len / ps) - 1;
        let mut unmaps = [(null_mut::<u8>(), 0usize); 32];
        let mut unmap_n = 0usize;
        let cell = LARGE_CACHE.0.get();
        // SAFETY: `ptr` owns a `ps`-aligned `MAP_ANON` region of `len` bytes; `os_unfair_lock`
        // serializes O(1) LRU eviction and bucket insertion, and evicted blocks are unmapped
        // outside the lock.
        unsafe {
            os_unfair_lock_lock(&raw mut (*cell).lock);
            (*cell).ensure_init();
            while ((*cell).free_head == NONE || (*cell).total_bytes + len > LARGE_CACHE_MAX_BYTES)
                && (*cell).lru_head != NONE
                && unmap_n < unmaps.len()
            {
                let oldest = (*cell).lru_head;
                unmaps[unmap_n] = (*cell).unlink_entry(oldest);
                unmap_n += 1;
            }
            let cached =
                (*cell).free_head != NONE && (*cell).total_bytes + len <= LARGE_CACHE_MAX_BYTES;
            if cached {
                (*cell).push_entry(b, ptr, len);
            }
            os_unfair_lock_unlock(&raw mut (*cell).lock);
            for &(u_ptr, u_len) in &unmaps[..unmap_n] {
                let _ = libc::munmap(u_ptr.cast(), u_len);
            }
            if !cached {
                let _ = libc::munmap(ptr.cast(), len);
            }
        }
        return;
    }
    // SAFETY: `ptr` was returned by `alloc_large` (or shrunk in `realloc`) with mapped length `len`.
    unsafe {
        let _ = libc::munmap(ptr.cast(), len);
    }
}

/// Flush the calling thread's small-object magazine, drop any empty spare `16 KiB` slabs, and
/// unmap all cached large blocks back to XNU.
#[cfg_attr(trust_verify, trust::skip)]
pub(crate) fn release_free_memory() {
    if TLS_STATE.load(Ordering::Acquire) == TLS_READY {
        let key = TLS_KEY.load(Ordering::Relaxed) as PthreadKey;
        // SAFETY: `TLS_READY` guarantees `key` is valid, and `tc` belongs to the calling thread.
        unsafe {
            let raw = pthread_getspecific(key);
            if !raw.is_null() && (raw as usize) != TLS_SENTINEL {
                let tc = raw.cast::<ThreadCache>();
                for idx in 0..NUM_CLASSES {
                    flush_local_bin(idx, &mut (*tc).bins[idx], 0);
                }
            }
        }
    }
    for bin in &BINS {
        let cell = bin.0.get();
        let mut unmap = null_mut::<u8>();
        // SAFETY: `os_unfair_lock` serializes inspection and unlinking of any empty spare slab.
        unsafe {
            os_unfair_lock_lock(&raw mut (*cell).lock);
            let head = (*cell).partial_head;
            if !head.is_null() && (*head).live == 0 && (*head).next.is_null() {
                unlink_partial(&mut *cell, head);
                unmap = slab_base_from_header(head);
            }
            os_unfair_lock_unlock(&raw mut (*cell).lock);
            if !unmap.is_null() {
                let _ = libc::munmap(unmap.cast(), SLAB_BYTES);
            }
        }
    }
    loop {
        let mut unmaps = [(null_mut::<u8>(), 0usize); 32];
        let mut unmap_n = 0usize;
        let cell = LARGE_CACHE.0.get();
        // SAFETY: `os_unfair_lock` serializes draining `LARGE_CACHE` in batches of 32 entries,
        // unmapping each region outside the lock.
        unsafe {
            os_unfair_lock_lock(&raw mut (*cell).lock);
            if (*cell).initialized {
                while (*cell).lru_head != NONE && unmap_n < unmaps.len() {
                    let oldest = (*cell).lru_head;
                    unmaps[unmap_n] = (*cell).unlink_entry(oldest);
                    unmap_n += 1;
                }
            }
            let more = (*cell).initialized && (*cell).lru_head != NONE;
            os_unfair_lock_unlock(&raw mut (*cell).lock);
            for &(u_ptr, u_len) in &unmaps[..unmap_n] {
                let _ = libc::munmap(u_ptr.cast(), u_len);
            }
            if !more {
                break;
            }
        }
    }
}

pub(crate) struct CompactZoneAlloc;

// SAFETY: `class_index(size, align)` deterministically partitions layouts between fixed-size
// `SLAB_BYTES`-aligned slab bins (each guaranteeing `CLASSES[idx] >= size` and slot alignment
// `>= align`) and page-rounded `mmap` regions (`alloc_large`/`dealloc_large`), and none of the
// code paths unwind.
unsafe impl GlobalAlloc for CompactZoneAlloc {
    #[inline]
    #[cfg_attr(trust_verify, trust::skip)]
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        match class_index(layout.size(), layout.align()) {
            Some(idx) => alloc_from_bin(idx, layout.size(), false),
            None => alloc_large(layout, false),
        }
    }

    #[inline]
    #[cfg_attr(trust_verify, trust::skip)]
    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        match class_index(layout.size(), layout.align()) {
            Some(idx) => alloc_from_bin(idx, layout.size(), true),
            None => alloc_large(layout, true),
        }
    }

    #[inline]
    #[cfg_attr(trust_verify, trust::skip)]
    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        match class_index(layout.size(), layout.align()) {
            Some(idx) => dealloc_to_bin(idx, ptr),
            // SAFETY: `class_index` returned `None`, so `ptr` owns a `round_up_pow2` `mmap` region.
            None => unsafe { dealloc_large(ptr, layout) },
        }
    }

    #[inline]
    #[cfg_attr(trust_verify, trust::skip)]
    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        let old_bin = class_index(layout.size(), layout.align());
        let new_bin = class_index(new_size, layout.align());
        if let (Some(a), Some(b)) = (old_bin, new_bin)
            && a == b
        {
            return ptr;
        }
        if old_bin.is_none() && new_bin.is_none() {
            let ps = page_size();
            if let (Some(old_len), Some(new_len)) = (
                round_up_pow2(layout.size(), ps),
                round_up_pow2(new_size, ps),
            ) && new_len <= old_len
            {
                if new_len < old_len {
                    // SAFETY: `ptr` owns `[ptr, ptr + old_len)` from `alloc_large`, and both
                    // `new_len` and `old_len` are multiples of `ps`, so unmapping the tail
                    // `[ptr + new_len, ptr + old_len)` leaves `[ptr, ptr + new_len)` intact.
                    unsafe {
                        let _ = libc::munmap(ptr.add(new_len).cast(), old_len - new_len);
                    }
                }
                return ptr;
            }
        }
        // SAFETY: `GlobalAlloc::realloc` guarantees `new_size > 0` and `(new_size, layout.align())`
        // is a valid `Layout`; `ptr` is valid for `layout.size()` bytes.
        unsafe {
            let new_layout = Layout::from_size_align_unchecked(new_size, layout.align());
            let new_ptr = self.alloc(new_layout);
            if !new_ptr.is_null() {
                std::ptr::copy_nonoverlapping(ptr, new_ptr, layout.size().min(new_size));
                self.dealloc(ptr, layout);
            }
            new_ptr
        }
    }
}

#[global_allocator]
static GLOBAL: CompactZoneAlloc = CompactZoneAlloc;

#[cfg(test)]
mod tests {
    use std::alloc::{GlobalAlloc, Layout};

    use super::{
        BINS, CLASSES, CompactZoneAlloc, MAX_ALIGNED_SLAB, class_index, os_unfair_lock_lock,
        os_unfair_lock_unlock, release_free_memory,
    };

    fn partial_slab_count(idx: usize) -> usize {
        let cell = BINS[idx].0.get();
        // SAFETY: Test acquires the bin lock before walking `partial_head`.
        unsafe {
            os_unfair_lock_lock(&raw mut (*cell).lock);
            let mut n = 0usize;
            let mut cur = (*cell).partial_head;
            while !cur.is_null() {
                n += 1;
                cur = (*cur).next;
            }
            os_unfair_lock_unlock(&raw mut (*cell).lock);
            n
        }
    }

    #[test]
    fn size_class_lookup_satisfies_every_size_and_alignment() {
        for align in [
            1usize, 2, 4, 8, 16, 32, 64, 128, 256, 512, 1024, 4096, 16384,
        ] {
            for size in 1..=16_500 {
                if let Some(idx) = class_index(size, align) {
                    let slot = CLASSES[idx];
                    assert!(
                        slot >= size,
                        "slot {slot} < size {size} (align {align}, idx {idx})"
                    );
                    if align <= 16 {
                        assert_eq!(slot % 16, 0);
                        if idx > 0 {
                            assert!(CLASSES[idx - 1] < size);
                        }
                    } else {
                        assert!(slot.is_power_of_two());
                        assert!(slot >= align);
                        assert!(MAX_ALIGNED_SLAB >= slot);
                    }
                }
            }
        }
    }

    #[test]
    fn compact_zone_handles_standard_overaligned_and_large_shrink_layouts() {
        for align in [1usize, 8, 16, 32, 64, 256, 4096, 16384, 65536] {
            for size in [
                1usize, 7, 16, 33, 48, 65, 257, 1024, 4096, 8192, 32768, 131072,
            ] {
                let layout = Layout::from_size_align(size, align).expect("valid layout");
                // SAFETY: `layout` has non-zero size (`size >= 1`), and each pointer is grown,
                // shrunk, and freed with its matching current layout.
                unsafe {
                    let p = CompactZoneAlloc.alloc_zeroed(layout);
                    assert!(!p.is_null());
                    assert_eq!((p as usize) & (align - 1), 0);
                    assert!(std::slice::from_raw_parts(p, size).iter().all(|&b| b == 0));
                    std::ptr::write_bytes(p, 0x5A, size);

                    let grown_size = size * 2 + 13;
                    let q = CompactZoneAlloc.realloc(p, layout, grown_size);
                    assert!(!q.is_null());
                    assert_eq!((q as usize) & (align - 1), 0);
                    assert!(
                        std::slice::from_raw_parts(q, size)
                            .iter()
                            .all(|&b| b == 0x5A)
                    );

                    let grown_layout =
                        Layout::from_size_align(grown_size, align).expect("valid grown layout");
                    let mid_size = (size / 2).max(1);
                    let mid = CompactZoneAlloc.realloc(q, grown_layout, mid_size);
                    assert!(!mid.is_null());
                    assert_eq!((mid as usize) & (align - 1), 0);
                    assert!(
                        std::slice::from_raw_parts(mid, mid_size)
                            .iter()
                            .all(|&b| b == 0x5A)
                    );

                    let mid_layout =
                        Layout::from_size_align(mid_size, align).expect("valid mid layout");
                    let shrunk = CompactZoneAlloc.realloc(mid, mid_layout, 1);
                    assert!(!shrunk.is_null());
                    assert_eq!((shrunk as usize) & (align - 1), 0);
                    assert_eq!(*shrunk, 0x5A);

                    let final_layout =
                        Layout::from_size_align(1, align).expect("valid shrunk layout");
                    CompactZoneAlloc.dealloc(shrunk, final_layout);
                }
            }
        }
    }

    #[test]
    fn spike_and_drain_across_threads_flushes_magazines_and_unmaps_slabs() {
        let layout = Layout::from_size_align(3_072, 16).expect("valid 3072 layout");
        let idx = class_index(layout.size(), layout.align()).expect("3072 uses slab bin");
        let (tx, rx) = std::sync::mpsc::channel::<usize>();
        let producer = std::thread::spawn(move || {
            // SAFETY: Pointers are allocated with `layout` and freed with the same `layout` on
            // the consumer thread.
            unsafe {
                for i in 0..96usize {
                    let p = CompactZoneAlloc.alloc(layout);
                    assert!(!p.is_null());
                    std::ptr::write_bytes(p, (i as u8) | 1, layout.size());
                    tx.send(p as usize).expect("send ptr");
                }
            }
        });
        let consumer = std::thread::spawn(move || {
            // SAFETY: Every pointer came from `CompactZoneAlloc.alloc(layout)`.
            unsafe {
                while let Ok(addr) = rx.recv() {
                    CompactZoneAlloc.dealloc(addr as *mut u8, layout);
                }
            }
        });
        producer.join().expect("producer join");
        consumer.join().expect("consumer join");
        release_free_memory();
        assert!(
            partial_slab_count(idx) <= 2,
            "thread-exit magazine flush + release_free_memory must unmap spike slabs"
        );
        // SAFETY: Allocate and free one zeroed slot with `layout`.
        unsafe {
            let z = CompactZoneAlloc.alloc_zeroed(layout);
            assert!(!z.is_null());
            assert!(
                std::slice::from_raw_parts(z, layout.size())
                    .iter()
                    .all(|&b| b == 0),
                "reused slab slots must be zeroed on alloc_zeroed"
            );
            CompactZoneAlloc.dealloc(z, layout);
        }
    }

    /// The global zone's large path: blocks of one bucket come back from the cache zeroed on
    /// `alloc_zeroed`. Which block comes back, and whether the bucket is empty after a relief,
    /// are not asked here: `CompactZoneAlloc` is this test binary's global allocator, so every
    /// other test thread frees into and pops from `LARGE_CACHE`, and one drains it
    /// (`release_free_memory`), concurrently. The cache's own order is
    /// [`large_cache_pops_the_hottest_block_and_drains_whole`]'s, on a cache of its own.
    #[test]
    fn large_blocks_come_back_zeroed_from_the_global_cache() {
        let layout = Layout::from_size_align(262_160, 16).expect("valid 256 KiB Arc<[u8]> layout");
        let mut ptrs = Vec::with_capacity(28);
        // SAFETY: Every pointer is allocated with `layout`, written within bounds, and freed with
        // the exact same `layout`.
        unsafe {
            for i in 0..28usize {
                let p = CompactZoneAlloc.alloc(layout);
                assert!(!p.is_null());
                std::ptr::write_bytes(p, (i as u8) | 1, layout.size());
                ptrs.push(p);
            }
            for p in ptrs.drain(..) {
                CompactZoneAlloc.dealloc(p, layout);
            }

            let reused = CompactZoneAlloc.alloc_zeroed(layout);
            assert!(!reused.is_null());
            assert!(
                std::slice::from_raw_parts(reused, layout.size())
                    .iter()
                    .all(|&b| b == 0),
                "reused large block must be zeroed on alloc_zeroed"
            );
            CompactZoneAlloc.dealloc(reused, layout);
            release_free_memory();
        }
    }

    /// THE LARGE CACHE'S ORDER, on a cache no other thread touches: a bucket pops the block
    /// freed last (O(1) LIFO, the hottest pages), and draining oldest-first, as a relief
    /// (`release_free_memory`) does, empties the bucket and the totals.
    #[test]
    fn large_cache_pops_the_hottest_block_and_drains_whole() {
        let mut cache = Box::new(super::LargeCacheState {
            lock: super::OsUnfairLock(0),
            initialized: false,
            free_head: super::NONE,
            lru_head: super::NONE,
            lru_tail: super::NONE,
            count: 0,
            total_bytes: 0,
            buckets: [super::NONE; super::LARGE_CACHE_BUCKETS],
            entries: [super::LargeEntry::EMPTY; super::LARGE_CACHE_SLOTS],
        });
        cache.ensure_init();
        let ps = super::page_size();
        let len = super::round_up_pow2(262_160, ps).expect("valid rounded len");
        let bucket = (len / ps) - 1;
        // The cache never reads through a block's pointer, so addresses alone stand in for them.
        let blocks: Vec<*mut u8> = (1..=28usize)
            .map(|i| std::ptr::without_provenance_mut(i * len))
            .collect();
        for &block in &blocks {
            cache.push_entry(bucket, block, len);
        }
        assert_eq!((cache.count, cache.total_bytes), (28, 28 * len));

        let head = cache.buckets[bucket];
        assert_ne!(head, super::NONE);
        assert_eq!(
            cache.unlink_entry(head),
            (blocks[27], len),
            "O(1) bucket LIFO pop must reuse the hottest 272 KiB block"
        );

        let mut drained = Vec::new();
        while cache.lru_head != super::NONE {
            let oldest = cache.lru_head;
            drained.push(cache.unlink_entry(oldest).0);
        }
        assert_eq!(drained, blocks[..27], "a relief drains oldest first");
        assert_eq!(cache.buckets[bucket], super::NONE);
        assert_eq!((cache.count, cache.total_bytes), (0, 0));
    }
}
