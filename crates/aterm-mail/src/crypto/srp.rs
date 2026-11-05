// Copyright 2026 Andrew Yates
// Derived from Proton AG proton-srp at c468e282625206eda9b48d539949e6183b1da32a.
// Adapted to aterm hashes, randomness, encoding and fixed-width arithmetic.
// SPDX-License-Identifier: MIT

//! Proton's SRP client, with fixed-width arithmetic and the server modulus pin.

use aterm_mail_bigint::modular::runtime_mod::{DynResidue, DynResidueParams};
use aterm_mail_bigint::{Encoding, NonZero, U2048, Zero};
use zeroize::Zeroizing;

#[cfg(test)]
#[path = "srp_vectors.rs"]
mod vectors;

/// Authentication evidence. The shared secret and password are not retained.
pub struct Proof {
    pub client_ephemeral: String,
    pub client_proof: String,
    expected_server_proof: Zeroizing<[u8; 256]>,
}

impl Proof {
    pub fn verify_server(&self, encoded: &str) -> Result<(), String> {
        let proof =
            aterm_codec::base64::decode(encoded).map_err(|_| "Malformed SRP server proof")?;
        if aterm_digest::ct_eq(&proof, self.expected_server_proof.as_ref()) {
            Ok(())
        } else {
            Err("Proton server proof did not verify".into())
        }
    }
}

pub fn authenticate(
    password: &str,
    version: u64,
    salt: &str,
    signed_modulus: &str,
    server_ephemeral: &str,
) -> Result<Proof, String> {
    let modulus = super::modulus::verify(signed_modulus)?;
    calculate(password, version, salt, &modulus, server_ephemeral, None)
}

fn expand(slices: &[&[u8]]) -> Zeroizing<[u8; 256]> {
    let mut result = Zeroizing::new([0u8; 256]);
    for (id, chunk) in result.as_chunks_mut::<64>().0.iter_mut().enumerate() {
        let mut hash = ring::digest::Context::new(&ring::digest::SHA512);
        for slice in slices {
            hash.update(slice);
        }
        hash.update(&[id as u8]);
        chunk.copy_from_slice(hash.finish().as_ref());
    }
    result
}

fn calculate(
    password: &str,
    version: u64,
    salt: &str,
    modulus: &[u8],
    server_ephemeral: &str,
    fixed_secret: Option<&[u8]>,
) -> Result<Proof, String> {
    if !matches!(version, 3 | 4) {
        return Err("Unsupported legacy Proton authentication version".into());
    }
    if modulus.len() != 256 || modulus[0] & 7 != 3 || modulus[255] & 128 == 0 {
        return Err("Invalid Proton SRP modulus".into());
    }
    let n = NonZero::new(U2048::from_le_slice(modulus))
        .into_option()
        .ok_or("Zero SRP modulus")?;
    let n_minus_one = NonZero::new(n.wrapping_sub(&U2048::ONE))
        .into_option()
        .ok_or("Invalid SRP modulus")?;
    let server =
        aterm_codec::base64::decode(server_ephemeral).map_err(|_| "Invalid server ephemeral")?;
    if server.len() != 256 {
        return Err("Invalid server ephemeral length".into());
    }
    let server = U2048::from_le_slice(&server);
    if server <= U2048::ONE || server >= *n_minus_one {
        return Err("Invalid server ephemeral range".into());
    }
    let salt = aterm_codec::base64::decode(salt).map_err(|_| "Invalid SRP salt")?;
    if salt.len() != 10 {
        return Err("Invalid SRP salt length".into());
    }
    let mut full_salt = [0u8; 16];
    full_salt[..10].copy_from_slice(&salt);
    full_salt[10..].copy_from_slice(b"proton");
    let hashed = Zeroizing::new(super::bcrypt::hash(password.as_bytes(), &full_salt));
    let x = Zeroizing::new(U2048::from_le_slice(
        expand(&[hashed.as_bytes(), modulus]).as_ref(),
    ));
    let params = DynResidueParams::new(&n);
    let generator = U2048::from_u32(2);
    let g = DynResidue::new(&generator, params);
    let generator_bytes = generator.to_le_bytes();
    let k = U2048::from_le_slice(expand(&[&generator_bytes, modulus]).as_ref()).rem(&n);
    if k <= U2048::ONE || k >= *n_minus_one {
        return Err("Invalid SRP multiplier".into());
    }
    let mut secret = Zeroizing::new([0u8; 256]);
    if let Some(fixed) = fixed_secret {
        if fixed.len() > 256 {
            return Err("Invalid fixed SRP secret".into());
        }
        secret[..fixed.len()].copy_from_slice(fixed);
    } else {
        let mut found = false;
        for _ in 0..64 {
            aterm_uds::rand::fill(secret.as_mut()).map_err(|e| e.to_string())?;
            let number = U2048::from_le_slice(secret.as_ref());
            if number > U2048::from_u32(4096) && number < *n_minus_one {
                found = true;
                break;
            }
        }
        if !found {
            return Err("Could not generate SRP secret".into());
        }
    }
    let a = Zeroizing::new(U2048::from_le_slice(secret.as_ref()));
    let public = g.pow(&a).retrieve();
    let public_bytes = public.to_le_bytes();
    let server_bytes = server.to_le_bytes();
    let u = U2048::from_le_slice(expand(&[&public_bytes, &server_bytes]).as_ref());
    if bool::from(u.rem(&n_minus_one).is_zero()) {
        return Err("Invalid SRP scramble".into());
    }
    let base = DynResidue::new(&server, params).sub(&g.pow(&x).mul(&DynResidue::new(&k, params)));
    let (ux, _) = U2048::const_rem_wide(x.mul_wide(&u), &n_minus_one);
    let exponent = Zeroizing::new(a.add_mod(&ux, &n_minus_one));
    let shared = Zeroizing::new(base.pow(&exponent).retrieve());
    let shared_bytes = Zeroizing::new(shared.to_le_bytes());
    let client_proof = expand(&[&public_bytes, &server_bytes, shared_bytes.as_ref()]);
    Ok(Proof {
        client_ephemeral: aterm_codec::base64::encode(&public_bytes).map_err(|e| e.to_string())?,
        client_proof: aterm_codec::base64::encode(client_proof.as_ref())
            .map_err(|e| e.to_string())?,
        expected_server_proof: expand(&[
            &public_bytes,
            client_proof.as_ref(),
            shared_bytes.as_ref(),
        ]),
    })
}
