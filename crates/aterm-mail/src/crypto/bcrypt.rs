// Copyright 2026 Andrew Yates
// Derived from RustCrypto blowfish 0.9.1; Copyright RustCrypto Developers.
// Adapted bcrypt 0.17.1 hashing with local Blowfish and aterm secret wiping.
// SPDX-License-Identifier: Apache-2.0 OR MIT

use zeroize::Zeroize;

struct Blowfish {
    s: [[u32; 256]; 4],
    p: [u32; 18],
}
fn next_u32_wrap(buf: &[u8], offset: &mut usize) -> u32 {
    let mut v = 0;
    for _ in 0..4 {
        if *offset >= buf.len() {
            *offset = 0;
        }
        v = (v << 8) | buf[*offset] as u32;
        *offset += 1;
    }
    v
}

impl Blowfish {
    fn init_state() -> Blowfish {
        Blowfish {
            p: super::blowfish_constants::P,
            s: super::blowfish_constants::S,
        }
    }

    fn expand_key(&mut self, key: &[u8]) {
        let mut key_pos = 0;
        for word in &mut self.p {
            *word ^= next_u32_wrap(key, &mut key_pos);
        }
        let mut lr = [0u32; 2];
        for i in 0..9 {
            lr = self.encrypt(lr);
            self.p[2 * i] = lr[0];
            self.p[2 * i + 1] = lr[1];
        }
        for i in 0..4 {
            for j in 0..128 {
                lr = self.encrypt(lr);
                self.s[i][2 * j] = lr[0];
                self.s[i][2 * j + 1] = lr[1];
            }
        }
    }

    #[allow(
        clippy::many_single_char_names,
        reason = "Keep the reference Blowfish round notation"
    )]
    fn round_function(&self, x: u32) -> u32 {
        let a = self.s[0][(x >> 24) as usize];
        let b = self.s[1][((x >> 16) & 0xff) as usize];
        let c = self.s[2][((x >> 8) & 0xff) as usize];
        let d = self.s[3][(x & 0xff) as usize];
        (a.wrapping_add(b) ^ c).wrapping_add(d)
    }

    fn encrypt(&self, [mut l, mut r]: [u32; 2]) -> [u32; 2] {
        for i in 0..8 {
            l ^= self.p[2 * i];
            r ^= self.round_function(l);
            r ^= self.p[2 * i + 1];
            l ^= self.round_function(r);
        }
        l ^= self.p[16];
        r ^= self.p[17];
        [r, l]
    }
}

impl Blowfish {
    pub fn salted_expand_key(&mut self, salt: &[u8], key: &[u8]) {
        let mut key_pos = 0;
        for word in &mut self.p {
            *word ^= next_u32_wrap(key, &mut key_pos);
        }
        let mut lr = [0u32; 2];
        let mut salt_pos = 0;
        for i in 0..9 {
            lr[0] ^= next_u32_wrap(salt, &mut salt_pos);
            lr[1] ^= next_u32_wrap(salt, &mut salt_pos);
            lr = self.encrypt(lr);

            self.p[2 * i] = lr[0];
            self.p[2 * i + 1] = lr[1];
        }
        for i in 0..4 {
            for j in 0..64 {
                lr[0] ^= next_u32_wrap(salt, &mut salt_pos);
                lr[1] ^= next_u32_wrap(salt, &mut salt_pos);
                lr = self.encrypt(lr);

                self.s[i][4 * j] = lr[0];
                self.s[i][4 * j + 1] = lr[1];

                lr[0] ^= next_u32_wrap(salt, &mut salt_pos);
                lr[1] ^= next_u32_wrap(salt, &mut salt_pos);
                lr = self.encrypt(lr);

                self.s[i][4 * j + 2] = lr[0];
                self.s[i][4 * j + 3] = lr[1];
            }
        }
    }
}

impl Drop for Blowfish {
    fn drop(&mut self) {
        self.s.zeroize();
        self.p.zeroize();
    }
}

/// Proton's fixed-cost bcrypt representation, including the $2y$ prefix.
pub(super) fn hash(password: &[u8], salt: &[u8; 16]) -> String {
    let mut key = zeroize::Zeroizing::new(password[..password.len().min(72)].to_vec());
    if key.len() < 72 {
        key.push(0);
    }
    let mut state = Blowfish::init_state();
    state.salted_expand_key(salt, &key);
    for _ in 0..1024 {
        state.expand_key(&key);
        state.expand_key(salt);
    }
    let mut words = [
        0x4f727068, 0x65616e42, 0x65686f6c, 0x64657253, 0x63727944, 0x6f756274,
    ];
    for pair in words.as_chunks_mut::<2>().0.iter_mut() {
        for _ in 0..64 {
            let block = state.encrypt([pair[0], pair[1]]);
            pair.copy_from_slice(&block);
        }
    }
    let mut result = [0u8; 24];
    for (word, bytes) in words.iter().zip(result.as_chunks_mut::<4>().0.iter_mut()) {
        bytes.copy_from_slice(&word.to_be_bytes());
    }
    let encoded = format!("$2y$10${}{}", encode(salt), encode(&result[..23]));
    words.zeroize();
    result.zeroize();
    encoded
}

fn encode(input: &[u8]) -> String {
    const ALPHABET: &[u8] = b"./ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789";
    let mut output = String::new();
    let mut bits = 0u32;
    let mut count = 0;
    for byte in input {
        bits = (bits << 8) | u32::from(*byte);
        count += 8;
        while count >= 6 {
            count -= 6;
            output.push(char::from(ALPHABET[((bits >> count) & 63) as usize]));
        }
    }
    if count > 0 {
        output.push(char::from(ALPHABET[((bits << (6 - count)) & 63) as usize]));
    }
    output
}
