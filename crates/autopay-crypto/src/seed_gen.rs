//! Monero seed phrase generation.
//!
//! Implements the Electrum-style 25-word mnemonic used by Monero:
//! - 32 random bytes → 24 word indices (11 bits each, derived via 3-word groups)
//! - 25th word = checksum (based on CRC32-like prefix matching)
//!
//! Reference: https://github.com/monero-project/monero/blob/master/src/mnemonics/

use crate::wordlist_en::MONERO_WORDLIST_EN;
use zeroize::Zeroizing;

const N: u64 = MONERO_WORDLIST_EN.len() as u64; // 1626

/// Generate a fresh 25-word Monero seed phrase.
/// Returns the phrase as a space-separated string, zeroized on drop.
pub fn generate_seed() -> Result<Zeroizing<String>, SeedError> {
    let mut bytes = [0u8; 32];
    getrandom::getrandom(&mut bytes).map_err(|e| SeedError::Rng(e.to_string()))?;
    bytes_to_mnemonic(&bytes)
}

/// Convert 32 raw bytes to a 25-word mnemonic.
/// Algorithm: Monero 3-word encoding over GF(N).
pub fn bytes_to_mnemonic(bytes: &[u8; 32]) -> Result<Zeroizing<String>, SeedError> {
    if MONERO_WORDLIST_EN.len() < 1626 {
        return Err(SeedError::WordlistTooShort);
    }

    let mut words: Vec<&str> = Vec::with_capacity(25);

    // Split 32 bytes into 8 groups of 4 bytes → 8 triplets of words
    for chunk in bytes.chunks(4) {
        let b = u32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]);
        let w1 = (b as u64) % N;
        let w2 = ((b as u64 / N) + w1) % N;
        let w3 = ((b as u64 / N / N) + w2) % N;
        words.push(MONERO_WORDLIST_EN[w1 as usize]);
        words.push(MONERO_WORDLIST_EN[w2 as usize]);
        words.push(MONERO_WORDLIST_EN[w3 as usize]);
    }

    // 25th word: checksum — Monero algorithm:
    // idx = crc32(first_3_chars_of_each_data_word) % 24
    // checksum_word = data_words[idx]  (repeats one of the 24 words)
    let checksum_idx = compute_checksum_index(&words) as usize;
    words.push(words[checksum_idx]);

    Ok(Zeroizing::new(words.join(" ")))
}

/// Verify a 25-word mnemonic and extract the 32 raw bytes.
/// Returns Err if the checksum word is wrong.
pub fn mnemonic_to_bytes(phrase: &str) -> Result<Zeroizing<[u8; 32]>, SeedError> {
    let words: Vec<&str> = phrase.split_whitespace().collect();
    if words.len() != 25 {
        return Err(SeedError::InvalidLength(words.len()));
    }

    // Verify checksum: the 25th word must match data_words[crc32(prefixes) % 24]
    let idx = compute_checksum_index(&words[..24]) as usize;
    let expected_prefix: String = words[idx].chars().take(3).collect();
    let actual_prefix: String = words[24].chars().take(3).collect();
    if expected_prefix != actual_prefix {
        return Err(SeedError::BadChecksum);
    }

    let mut bytes = [0u8; 32];
    for (i, chunk) in words[..24].chunks(3).enumerate() {
        let w1 = find_word(chunk[0])?;
        let w2 = find_word(chunk[1])?;
        let w3 = find_word(chunk[2])?;

        // Reverse the encoding
        let x = w1 + N * ((N - w1 + w2) % N) + N * N * ((N - w2 + w3) % N);
        let b = (x as u32).to_le_bytes();
        bytes[i * 4..i * 4 + 4].copy_from_slice(&b);
    }

    Ok(Zeroizing::new(bytes))
}

fn find_word(word: &str) -> Result<u64, SeedError> {
    // Match by first 3 characters (Monero uses prefix matching)
    let prefix = &word[..word.len().min(3)];
    MONERO_WORDLIST_EN
        .iter()
        .position(|&w| w.starts_with(prefix))
        .map(|i| i as u64)
        .ok_or_else(|| SeedError::UnknownWord(word.to_string()))
}

/// Compute the checksum index.
/// Exact Monero algorithm (from monero/src/mnemonics/electrum-words.cpp):
///   prefix_string = concat first-3-chars of each of the 24 data words
///   index = crc32(prefix_string) % num_data_words   ← index INTO THE SEED, not the wordlist!
///   checksum word = data_words[index]  (one of the 24 words is repeated as the 25th)
fn compute_checksum_index(data_words: &[&str]) -> u32 {
    let mut prefix = String::with_capacity(data_words.len() * 3);
    for word in data_words.iter() {
        let p: String = word.chars().take(3).collect();
        prefix.push_str(&p);
    }
    let crc = crc32_zlib(prefix.as_bytes());
    crc % data_words.len() as u32
}

/// CRC32 using the zlib/ISO-3309 polynomial (same as libz crc32_z).
/// This is what Monero calls internally via zlib's crc32().
fn crc32_zlib(data: &[u8]) -> u32 {
    let mut crc: u32 = 0xFFFF_FFFF;
    for &byte in data {
        crc ^= byte as u32;
        for _ in 0..8 {
            if crc & 1 != 0 {
                crc = (crc >> 1) ^ 0xEDB8_8320; // reflected poly
            } else {
                crc >>= 1;
            }
        }
    }
    !crc
}

#[derive(Debug, thiserror::Error)]
pub enum SeedError {
    #[error("RNG error: {0}")]
    Rng(String),
    #[error("Invalid mnemonic length: expected 25, got {0}")]
    InvalidLength(usize),
    #[error("Bad checksum word")]
    BadChecksum,
    #[error("Unknown word: {0}")]
    UnknownWord(String),
    #[error("Wordlist too short")]
    WordlistTooShort,
}
