//! sha-256, hmac, and pbkdf2, written out here to keep the crate free of
//! dependencies. the end-to-end tests check the result against python's
//! hashlib.

use std::collections::hash_map::RandomState;
use std::hash::{BuildHasher, Hasher};

const BLOCK_BYTES: usize = 64;
pub const DIGEST_BYTES: usize = 32;
const INNER_PAD: u8 = 0x36;
const OUTER_PAD: u8 = 0x5c;

const INITIAL: [u32; 8] =
    [0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab, 0x5be0cd19];
const ROUND: [u32; 64] = [
    0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4, 0xab1c5ed5,
    0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe, 0x9bdc06a7, 0xc19bf174,
    0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f, 0x4a7484aa, 0x5cb0a9dc, 0x76f988da,
    0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7, 0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967,
    0x27b70a85, 0x2e1b2138, 0x4d2c6dfc, 0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85,
    0xa2bfe8a1, 0xa81a664b, 0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070,
    0x19a4c116, 0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
    0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7, 0xc67178f2,
];

/// message schedule of one block.
fn schedule(block: &[u8]) -> [u32; 64] {
    let mut words = [0u32; 64];
    for (word, bytes) in words.iter_mut().zip(block.chunks_exact(4)) {
        *word = u32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]);
    }
    for i in 16..64 {
        let s0 = words[i - 15].rotate_right(7) ^ words[i - 15].rotate_right(18) ^ (words[i - 15] >> 3);
        let s1 = words[i - 2].rotate_right(17) ^ words[i - 2].rotate_right(19) ^ (words[i - 2] >> 10);
        words[i] = words[i - 16].wrapping_add(s0).wrapping_add(words[i - 7]).wrapping_add(s1);
    }
    words
}

fn compress(state: &mut [u32; 8], block: &[u8]) {
    let words = schedule(block);
    let [mut a, mut b, mut c, mut d, mut e, mut f, mut g, mut h] = *state;
    for i in 0..64 {
        let s1 = e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25);
        let choice = (e & f) ^ (!e & g);
        let t1 = h.wrapping_add(s1).wrapping_add(choice).wrapping_add(ROUND[i]).wrapping_add(words[i]);
        let s0 = a.rotate_right(2) ^ a.rotate_right(13) ^ a.rotate_right(22);
        let majority = (a & b) ^ (a & c) ^ (b & c);
        (h, g, f, e) = (g, f, e, d.wrapping_add(t1));
        (d, c, b, a) = (c, b, a, t1.wrapping_add(s0).wrapping_add(majority));
    }
    for (value, add) in state.iter_mut().zip([a, b, c, d, e, f, g, h]) {
        *value = value.wrapping_add(add);
    }
}

pub fn sha256(data: &[u8]) -> [u8; DIGEST_BYTES] {
    let mut padded = data.to_vec();
    padded.push(0x80);
    let zeros = (BLOCK_BYTES + 56 - padded.len() % BLOCK_BYTES) % BLOCK_BYTES;
    padded.resize(padded.len() + zeros, 0);
    padded.extend_from_slice(&(data.len() as u64 * 8).to_be_bytes());
    let mut state = INITIAL;
    padded.chunks_exact(BLOCK_BYTES).for_each(|block| compress(&mut state, block));
    let mut digest = [0u8; DIGEST_BYTES];
    for (bytes, word) in digest.chunks_exact_mut(4).zip(state) {
        bytes.copy_from_slice(&word.to_be_bytes());
    }
    digest
}

pub fn hmac(key: &[u8], message: &[u8]) -> [u8; DIGEST_BYTES] {
    let mut block = [0u8; BLOCK_BYTES];
    match key.len() > BLOCK_BYTES {
        true => block[..DIGEST_BYTES].copy_from_slice(&sha256(key)),
        false => block[..key.len()].copy_from_slice(key),
    }
    let padded = |pad: u8, rest: &[u8]| -> Vec<u8> {
        block.iter().map(|b| b ^ pad).chain(rest.iter().copied()).collect()
    };
    sha256(&padded(OUTER_PAD, &sha256(&padded(INNER_PAD, message))))
}

/// pbkdf2-hmac-sha256 with a 32 byte result.
pub fn pbkdf2(password: &[u8], salt: &[u8], rounds: u32) -> [u8; DIGEST_BYTES] {
    let mut last = hmac(password, &[salt, &1u32.to_be_bytes()].concat());
    let mut key = last;
    for _ in 1..rounds {
        last = hmac(password, &last);
        key.iter_mut().zip(last).for_each(|(byte, mix)| *byte ^= mix);
    }
    key
}

pub fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

/// 64 random bits. std seeds its hash maps from the system random number
/// generator, which is the only one available without dependencies.
pub fn random() -> u64 {
    RandomState::new().build_hasher().finish()
}
