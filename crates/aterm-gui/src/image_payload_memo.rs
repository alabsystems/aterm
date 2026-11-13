// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Bounded process-wide memo for base64-encoded inline image wire payloads.

use std::collections::HashMap;
use std::sync::{Arc, LazyLock, Mutex, Weak};

use aterm_core::grid::extra::{ImageData, ImageFormat};
use aterm_types::MutexExt as _;

use crate::control::control_media::MAX_IMAGE_PAYLOAD_BYTES;

const MEMO_BYTES: usize = 16 * 1024 * 1024;

const ENTRY_BYTES: usize = std::mem::size_of::<(usize, Entry)>();

static MEMO: LazyLock<PayloadMemo> = LazyLock::new(|| PayloadMemo::with_budget(MEMO_BYTES));

/// Returns the wire payloads for `images` in order, served from or added to the process memo.
pub(crate) fn image_payloads(images: &[&Arc<ImageData>]) -> Vec<ImagePayload> {
    MEMO.payloads_of(images)
}

/// Wire payload for an inline image: format name and optional base64 data.
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
        match aterm_codec::base64::encode(&img.bytes) {
            Ok(b64) => ImagePayload {
                format,
                b64: Some(Arc::new(b64)),
            },
            Err(_) => Self::TRUNCATED,
        }
    }

    /// `png`, `rgba`, `unknown`, or `truncated` past the F4 cap.
    pub(crate) const fn format(&self) -> &'static str {
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

struct Batch {
    /// The batch's number: [`Entries::batches`] when it began.
    number: u64,
    /// One payload per image, `TRUNCATED` at each miss until it is encoded.
    out: Vec<ImagePayload>,
    /// The indices of the images the memo had no entry for.
    misses: Vec<usize>,
}

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

    fn payloads_of(&self, images: &[&Arc<ImageData>]) -> Vec<ImagePayload> {
        let batch = self.begin(images);
        self.finish(images, batch)
    }

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
        drop(freed);
        Batch {
            number,
            out,
            misses,
        }
    }

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
#[path = "image_payload_memo_tests.rs"]
mod tests;
