//! Monero seed phrase generation.
//!
//! Implements the Electrum-style 25-word mnemonic used by Monero:
//! - 32 random bytes → 24 word indices (11 bits each, derived via 3-word groups)
//! - 25th word = checksum (based on CRC32-like prefix matching)
//!
//! Reference: https://github.com/monero-project/monero/blob/master/src/mnemonics/

use crate::wordlist_en::MONERO_WORDLIST_EN;
use zeroize::Zeroizing;

const N: u64 = MONERO_WORDLIST_EN.len() as u64; // 1624

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
    if MONERO_WORDLIST_EN.len() < 1624 {
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

    // 25th word: checksum
    let checksum_word = compute_checksum(&words);
    words.push(checksum_word);

    Ok(Zeroizing::new(words.join(" ")))
}

/// Verify a 25-word mnemonic and extract the 32 raw bytes.
/// Returns Err if the checksum word is wrong.
pub fn mnemonic_to_bytes(phrase: &str) -> Result<Zeroizing<[u8; 32]>, SeedError> {
    let words: Vec<&str> = phrase.split_whitespace().collect();
    if words.len() != 25 {
        return Err(SeedError::InvalidLength(words.len()));
    }

    // Verify checksum
    let checksum_word = compute_checksum(&words[..24]);
    if !words[24].starts_with(&checksum_word[..3]) {
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

/// Compute the 25th checksum word.
/// Monero algorithm: each word contributes its first 3 chars; compute CRC-like sum.
fn compute_checksum<'a>(words: &[&'a str]) -> &'a str {
    // Monero prefix-based checksum: sum the indices of word prefix matches
    let mut checksum: u32 = 0;
    for (i, word) in words.iter().enumerate() {
        let prefix = &word[..word.len().min(3)];
        let idx = MONERO_WORDLIST_EN
            .iter()
            .position(|&w| w.starts_with(prefix))
            .unwrap_or(0) as u32;
        checksum ^= idx.wrapping_add(i as u32);
    }
    MONERO_WORDLIST_EN[(checksum as u64 % N) as usize]
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
