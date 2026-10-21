// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE IMAGE PAYLOAD MEMO: the base64 of an inline image is encoded once per
//! image, not once per frame that shows it.
//!
//! The styled frame (`screen`, `subscribe … cells`) and `image read` carry
//! every on-screen image's bytes as base64, up to about 5.6 MB an image under
//! the F4 cap ([`MAX_IMAGE_PAYLOAD_BYTES`]). An [`ImageData`] is immutable once
//! placed and shared by `Arc`, so its encoding is a function of its identity.
//! A frame change that left a picture in place used to encode it again.
//!
//! KEYED BY IDENTITY, PINNED BY A `Weak`. An entry is keyed by
//! [`Arc::as_ptr`] and holds a `Weak` of the image. The `Weak` keeps the
//! allocation, and so the address, from being reused by another image while
//! the entry lives, without keeping the bytes alive. A reader holds the image
//! it asks about, so a key match is that image (the renderer's `ImageCache`
//! pins the same way). An image that has been freed leaves the memo at the
//! next batch.
//!
//! RETENTION. The memo is bounded by bytes ([`MEMO_BYTES`]). A batch is one
//! frame's images, or one `image read`'s. The entries a batch uses are never
//! evicted while it runs: room for a new entry comes from entries no batch has
//! used since this one began, least recently used first, and a payload that
//! would not fit even with all of those gone is served without being kept,
//! evicting nothing. A frame visits its images in one order every time, so a
//! frame with more images than fit keeps the ones it reaches first and encodes
//! only the rest. Evicting least-recently-used entries regardless would evict
//! each image before the next frame reused it, and the memo would never hit.
//! A payload over the F4 cap is never kept: it is `truncated` and costs
//! nothing to produce. Dropped entries are freed with the memo's lock
//! released.
//!
//! NOT A FIELD OF `ImageData`. A `OnceLock` there would keep the base64 for
//! the image's whole life, scrollback included, outside every image budget,
//! and would put this control-wire encoding into the vocabulary crate.

use std::collections::HashMap;
use std::sync::{Arc, LazyLock, Mutex, Weak};

use aterm_core::grid::extra::{ImageData, ImageFormat};
use aterm_types::MutexExt as _;

use crate::control::control_media::MAX_IMAGE_PAYLOAD_BYTES;

/// The process memo's byte budget: about three images at the F4 cap, or a
/// screen of ordinary thumbnails.
const MEMO_BYTES: usize = 16 * 1024 * 1024;

/// What an entry charges against the budget besides its base64: its slot in
/// the map. A screen of tiny images is bounded too.
const ENTRY_BYTES: usize = std::mem::size_of::<(usize, Entry)>();

static MEMO: LazyLock<PayloadMemo> = LazyLock::new(|| PayloadMemo::with_budget(MEMO_BYTES));

/// The wire payload of each of `images`, in order, through the process's
/// memo. `images` are distinct, as `distinct_images` and `image read` collect
/// them.
pub(crate) fn image_payloads(images: &[&Arc<ImageData>]) -> Vec<ImagePayload> {
    MEMO.payloads_of(images)
}

/// One image's wire payload: its format word and the base64 of its bytes, or
/// `truncated` and no payload past the F4 cap. A clone shares the base64.
#[derive(Clone, Debug)]
pub(crate) struct ImagePayload {
    format: &'static str,
    /// `Arc<String>`, not `Arc<str>`: the encoder's `String` moves in, where
    /// an `Arc<str>` would copy up to 5.6 MB once more.
    b64: Option<Arc<String>>,
}

impl ImagePayload {
    pub(crate) const TRUNCATED: ImagePayload = ImagePayload {
        format: "truncated",
        b64: None,
    };

    /// `img` encoded under the F4 cap: an oversized image is `truncated`, not
    /// encoded.
    fn encode(img: &ImageData) -> ImagePayload {
        let format = match img.format {
            ImageFormat::Png => "png",
            ImageFormat::RawRgba8 { .. } => "rgba",
            _ => "unknown",
        };
        if img.bytes.len() > MAX_IMAGE_PAYLOAD_BYTES {
            return Self::TRUNCATED;
        }
        #[cfg(test)]
        crate::work_counts::image_encoded();
        // `base64::encode` fails only above `aterm_codec::MAX_INPUT_LEN` (64
        // MiB), past the cap checked above. Were it to fail, the image is
        // reported like the cap path rather than panicking.
        match aterm_codec::base64::encode(&img.bytes) {
            Ok(b64) => ImagePayload {
                format,
                b64: Some(Arc::new(b64)),
            },
            Err(_) => Self::TRUNCATED,
        }
    }

    /// `png`, `rgba`, `unknown`, or `truncated` past the F4 cap.
    pub(crate) fn format(&self) -> &'static str {
        self.format
    }

    /// The base64 of the image's bytes; empty when `truncated`.
    pub(crate) fn b64(&self) -> &str {
        self.b64.as_deref().map_or("", String::as_str)
    }

    fn charge(&self) -> usize {
        ENTRY_BYTES + self.b64().len()
    }
}

/// Encoded payloads keyed by image identity, bounded by bytes. See the module
/// doc for the eviction rule.
struct PayloadMemo {
    budget: usize,
    /// A STRICT LEAF: held to look up or store entries, never across an
    /// encode or a free, and nothing is locked under it.
    entries: Mutex<Entries>,
}

#[derive(Default)]
struct Entries {
    by_image: HashMap<usize, Entry>,
    /// The sum of the entries' charges.
    bytes: usize,
    /// The batches begun so far: each batch is numbered by this count as it
    /// begins.
    batches: u64,
}

struct Entry {
    /// Keeps the image's address from being reused while the entry lives;
    /// read only to drop the entry once the image is freed.
    pin: Weak<ImageData>,
    payload: ImagePayload,
    /// The last batch that used this entry.
    used: u64,
}

/// A batch between its two holds of the lock: what the lookup served, and the
/// images it left to encode.
struct Batch {
    /// The batch's number: [`Entries::batches`] when it began.
    number: u64,
    /// One payload per image, `TRUNCATED` at each miss until it is encoded.
    out: Vec<ImagePayload>,
    /// The indices of the images the memo had no entry for.
    misses: Vec<usize>,
}

/// The entries a batch may evict to keep what it encoded: those no batch used
/// since it began.
struct Evictable {
    /// Most recently used first, so `pop` takes the least recent.
    order: Vec<(u64, usize)>,
    /// The charges of the entries still in `order`.
    bytes: usize,
}

impl PayloadMemo {
    fn with_budget(budget: usize) -> PayloadMemo {
        PayloadMemo {
            budget,
            entries: Mutex::new(Entries::default()),
        }
    }

    /// The payload of each of `images`, in order: the memo's entry when it has
    /// one, else encoded here with the lock released and kept while it fits.
    fn payloads_of(&self, images: &[&Arc<ImageData>]) -> Vec<ImagePayload> {
        let batch = self.begin(images);
        self.finish(images, batch)
    }

    /// Begin a batch over `images`: number it, drop freed images' entries, and
    /// serve every image the memo holds, marking its entry used by the batch.
    fn begin(&self, images: &[&Arc<ImageData>]) -> Batch {
        let mut out = Vec::with_capacity(images.len());
        let mut misses = Vec::new();
        let mut freed = Vec::new();
        let number = {
            let mut entries = self.entries.lock_or_recover();
            entries.batches += 1;
            let number = entries.batches;
            entries.drop_freed(&mut freed);
            for (at, img) in images.iter().enumerate() {
                match entries.by_image.get_mut(&key(img)) {
                    Some(entry) => {
                        entry.used = number;
                        out.push(entry.payload.clone());
                    }
                    None => {
                        out.push(ImagePayload::TRUNCATED);
                        misses.push(at);
                    }
                }
            }
            number
        };
        // Dropped entries' base64 is freed with the lock released.
        drop(freed);
        Batch {
            number,
            out,
            misses,
        }
    }

    /// Finish `batch` over the same `images`: encode its misses with the lock
    /// released, keep each while it fits, and return its payloads. Other
    /// batches may begin and finish between this batch's two holds.
    fn finish(&self, images: &[&Arc<ImageData>], batch: Batch) -> Vec<ImagePayload> {
        let Batch {
            number,
            mut out,
            mut misses,
        } = batch;
        for &at in &misses {
            out[at] = ImagePayload::encode(images[at]);
        }
        misses.retain(|&at| out[at].b64.is_some());
        let mut freed = Vec::new();
        if !misses.is_empty() {
            let mut entries = self.entries.lock_or_recover();
            let mut evictable = None;
            for at in misses {
                entries.keep(
                    self.budget,
                    images[at],
                    &out[at],
                    number,
                    &mut evictable,
                    &mut freed,
                );
            }
        }
        drop(freed);
        out
    }

    /// The bytes the memo charges now.
    #[cfg(test)]
    fn charged_bytes(&self) -> usize {
        self.entries.lock_or_recover().bytes
    }

    /// Whether the memo holds an entry for `img`.
    #[cfg(test)]
    fn holds_entry(&self, img: &Arc<ImageData>) -> bool {
        self.entries
            .lock_or_recover()
            .by_image
            .contains_key(&key(img))
    }

    /// How many entries the memo holds.
    #[cfg(test)]
    fn entry_count(&self) -> usize {
        self.entries.lock_or_recover().by_image.len()
    }
}

fn key(img: &Arc<ImageData>) -> usize {
    Arc::as_ptr(img) as usize
}

impl Entries {
    /// Move the entries of images that have been freed into `freed`.
    fn drop_freed(&mut self, freed: &mut Vec<Entry>) {
        let before = freed.len();
        freed.extend(
            self.by_image
                .extract_if(|_, entry| entry.pin.strong_count() == 0)
                .map(|(_, entry)| entry),
        );
        for entry in &freed[before..] {
            self.bytes -= entry.payload.charge();
        }
    }

    /// Keep `payload` for `img`, encoded by `batch`, if evicting entries no
    /// batch has used since `batch` began, least recently used first, can make
    /// room for it; otherwise leave the memo as it is. `evictable` is the
    /// batch's eviction order, built on first need and kept to the entries the
    /// batch does not use; evicted entries move into `freed`.
    fn keep(
        &mut self,
        budget: usize,
        img: &Arc<ImageData>,
        payload: &ImagePayload,
        batch: u64,
        evictable: &mut Option<Evictable>,
        freed: &mut Vec<Entry>,
    ) {
        let key = key(img);
        if let Some(entry) = self.by_image.get_mut(&key) {
            // A concurrent batch encoded it first. An older batch's entry may
            // be in this batch's eviction order; this batch uses it now, so it
            // leaves that order and its charge leaves the order's bytes.
            if entry.used < batch
                && let Some(evictable) = evictable
                && let Some(at) = evictable.order.iter().position(|&(_, k)| k == key)
            {
                evictable.order.remove(at);
                evictable.bytes -= entry.payload.charge();
            }
            entry.used = entry.used.max(batch);
            return;
        }
        let charge = payload.charge();
        if self.bytes + charge > budget {
            let evictable = evictable.get_or_insert_with(|| self.evictable_by(batch));
            if self.bytes.saturating_sub(evictable.bytes) + charge > budget {
                // Evicting every older entry would still leave no room.
                return;
            }
            while self.bytes + charge > budget {
                let Some((_, victim)) = evictable.order.pop() else {
                    return;
                };
                if let Some(gone) = self.by_image.remove(&victim) {
                    self.bytes -= gone.payload.charge();
                    evictable.bytes -= gone.payload.charge();
                    freed.push(gone);
                }
            }
        }
        self.bytes += charge;
        self.by_image.insert(
            key,
            Entry {
                pin: Arc::downgrade(img),
                payload: payload.clone(),
                used: batch,
            },
        );
    }

    /// The entries no batch has used since `batch` began, least recent last.
    fn evictable_by(&self, batch: u64) -> Evictable {
        let mut order: Vec<(u64, usize)> = self
            .by_image
            .iter()
            .filter(|(_, entry)| entry.used < batch)
            .map(|(&key, entry)| (entry.used, key))
            .collect();
        order.sort_unstable_by(|a, b| b.cmp(a));
        let bytes = order
            .iter()
            .map(|(_, key)| self.by_image[key].payload.charge())
            .sum();
        Evictable { order, bytes }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use aterm_core::grid::extra::{ImageData, ImageFormat, ImageScaling};

    use super::{ENTRY_BYTES, PayloadMemo};
    use crate::control::control_media::MAX_IMAGE_PAYLOAD_BYTES;
    use crate::work_counts::take_images_encoded;

    fn image(fill: u8, len: usize) -> Arc<ImageData> {
        Arc::new(ImageData {
            bytes: vec![fill; len],
            format: ImageFormat::Png,
            cols: 1,
            rows: 1,
            z_index: 0,
            band_lift_px: 0,
            cell_offset_px: (0, 0),
            scaling: ImageScaling::Fit,
            source_rect: None,
        })
    }

    fn b64_of(img: &ImageData) -> String {
        aterm_codec::base64::encode(&img.bytes).expect("encode")
    }

    /// What `img`'s payload charges against a memo's budget.
    fn charge_of(img: &ImageData) -> usize {
        ENTRY_BYTES + b64_of(img).len()
    }

    /// The case the retired two-slot shape lost: three images, in one order
    /// frame after frame, are each encoded once, and every later frame
    /// shares the first encoding. The negative control is a fresh memo,
    /// which encodes all three again.
    #[test]
    fn three_images_are_encoded_once_across_frames() {
        let memo = PayloadMemo::with_budget(1 << 20);
        let images = [image(1, 30), image(2, 31), image(3, 32)];
        let refs: Vec<&Arc<ImageData>> = images.iter().collect();
        take_images_encoded();

        let first = memo.payloads_of(&refs);
        assert_eq!(
            take_images_encoded(),
            3,
            "the first frame encodes each image"
        );
        for (payload, img) in first.iter().zip(&images) {
            assert_eq!(payload.format(), "png");
            assert_eq!(payload.b64(), b64_of(img), "the payload is the image's own");
        }
        for _ in 0..3 {
            let again = memo.payloads_of(&refs);
            assert_eq!(take_images_encoded(), 0, "a later frame encodes nothing");
            for (a, b) in again.iter().zip(&first) {
                assert!(
                    std::ptr::eq(a.b64().as_ptr(), b.b64().as_ptr()),
                    "and shares the first encoding"
                );
            }
        }

        let fresh = PayloadMemo::with_budget(1 << 20);
        fresh.payloads_of(&refs);
        assert_eq!(take_images_encoded(), 3, "the negative control re-encodes");
    }

    /// A full memo keeps what the frame reaches first and encodes only the
    /// rest, frame after frame. The negative control is the thrash the
    /// eviction rule forbids: evicting the frame's own entries would encode
    /// all three every frame.
    #[test]
    fn a_full_memo_keeps_what_the_frame_uses_and_encodes_only_the_rest() {
        let len = 300;
        let images = [image(1, len), image(2, len), image(3, len)];
        let budget = 2 * charge_of(&images[0]);
        let memo = PayloadMemo::with_budget(budget);
        let refs: Vec<&Arc<ImageData>> = images.iter().collect();
        take_images_encoded();

        memo.payloads_of(&refs);
        assert_eq!(take_images_encoded(), 3);
        assert_eq!(memo.entry_count(), 2, "two fit");
        assert!(memo.holds_entry(&images[0]) && memo.holds_entry(&images[1]));
        for _ in 0..3 {
            let payloads = memo.payloads_of(&refs);
            assert_eq!(take_images_encoded(), 1, "only the image that does not fit");
            assert_eq!(payloads[2].b64(), b64_of(&images[2]));
            assert!(memo.charged_bytes() <= budget, "within the budget");
        }

        // A payload that cannot fit even with every older entry gone evicts
        // nothing.
        let big = image(7, 2 * len);
        memo.payloads_of(&[&images[0], &big]);
        assert_eq!(take_images_encoded(), 1);
        assert!(memo.holds_entry(&images[1]), "the older entry stays");
        assert!(!memo.holds_entry(&big), "and the big payload is not kept");

        // A frame of new images takes the room the old frame no longer uses.
        let next = [image(4, len), image(5, len)];
        let next_refs: Vec<&Arc<ImageData>> = next.iter().collect();
        memo.payloads_of(&next_refs);
        assert_eq!(take_images_encoded(), 2);
        assert!(
            memo.holds_entry(&next[0]) && memo.holds_entry(&next[1]),
            "both kept"
        );
        assert!(!memo.holds_entry(&images[0]) && !memo.holds_entry(&images[1]));
        assert!(memo.charged_bytes() <= budget);
    }

    /// Two batches interleave over one image X: B begins, then A, then B
    /// keeps X while A is encoding, so A's keep finds an entry an older batch
    /// left evictable. A uses it from then on: W's miss evicts only the
    /// entry no batch uses, and Y, which would need X's room, is served
    /// without being kept. Were X left in A's eviction order, Y's miss would
    /// evict it and keep Y.
    #[test]
    fn a_batch_never_evicts_an_entry_an_older_batch_kept_for_it() {
        let len = 300;
        let [old, w, x, y] = [image(1, len), image(2, len), image(3, len), image(4, len)];
        let budget = 2 * charge_of(&old);
        let memo = PayloadMemo::with_budget(budget);
        memo.payloads_of(&[&old]);

        let b = memo.begin(&[&x]);
        let a_images = [&w, &x, &y];
        let a = memo.begin(&a_images);
        assert_eq!(a.misses, [0, 1, 2], "A finds none of its images");
        memo.finish(&[&x], b);
        assert!(
            memo.holds_entry(&x),
            "B keeps X, last used by B, which is older than A"
        );
        take_images_encoded();

        let payloads = memo.finish(&a_images, a);
        assert_eq!(take_images_encoded(), 3, "A encodes its misses");
        for (payload, img) in payloads.iter().zip(a_images) {
            assert_eq!(payload.b64(), b64_of(img), "the payload is the image's own");
        }
        assert!(memo.holds_entry(&x), "A does not evict X, which it uses");
        assert!(memo.holds_entry(&w), "W takes the unused entry's room");
        assert!(!memo.holds_entry(&old) && !memo.holds_entry(&y));
        assert!(memo.charged_bytes() <= budget);
    }

    /// A freed image's entry pins its address until the next batch drops the
    /// entry, so no other image can be served the freed one's payload. The
    /// negative control is an image kept alive: its entry stays.
    #[test]
    fn a_freed_image_leaves_the_memo_and_its_address_serves_no_stale_payload() {
        let memo = PayloadMemo::with_budget(1 << 20);
        let kept = image(9, 40);
        let freed = image(1, 40);
        memo.payloads_of(&[&kept, &freed]);
        assert_eq!(memo.entry_count(), 2);
        let freed_at = Arc::as_ptr(&freed) as usize;
        drop(freed);

        let other = image(2, 40);
        assert_ne!(
            Arc::as_ptr(&other) as usize,
            freed_at,
            "the entry's Weak holds the freed image's allocation"
        );
        let payloads = memo.payloads_of(&[&other]);
        assert_eq!(
            payloads[0].b64(),
            b64_of(&other),
            "the new image's own bytes"
        );
        assert!(memo.holds_entry(&kept), "a live image keeps its entry");
        assert_eq!(memo.entry_count(), 2, "the freed image's entry is gone");
        assert_eq!(memo.charged_bytes(), charge_of(&kept) + charge_of(&other));
    }

    /// F4: a payload over the cap is `truncated` with no base64, and is never
    /// kept.
    #[test]
    fn an_oversized_image_is_truncated_and_not_kept() {
        let memo = PayloadMemo::with_budget(1 << 30);
        let big = image(0, MAX_IMAGE_PAYLOAD_BYTES + 1);
        take_images_encoded();
        let payloads = memo.payloads_of(&[&big]);
        assert_eq!(payloads[0].format(), "truncated");
        assert_eq!(payloads[0].b64(), "");
        assert_eq!(take_images_encoded(), 0, "nothing is encoded");
        assert_eq!(memo.entry_count(), 0, "nothing is kept");
    }
}
