//! Monero seed phrase generation.
//!
//! Implements the Electrum-style 25-word mnemonic used by Monero:
//! - 32 random bytes → 24 word indices (11 bits each, derived via 3-word groups)
//! - 25th word = checksum (based on CRC32-like prefix matching)
//!
//! New seeds are English; restoring accepts English and French seeds.
//!
//! Reference: https://github.com/monero-project/monero/blob/master/src/mnemonics/

use crate::wordlist_en::MONERO_WORDLIST_EN;
use crate::wordlist_fr::MONERO_WORDLIST_FR;
use zeroize::Zeroizing;

const N: u64 = 1626;

/// A Monero word list: words are identified by their first `prefix_len` characters.
struct WordList {
    words: &'static [&'static str],
    prefix_len: usize,
}

const ENGLISH: WordList = WordList { words: MONERO_WORDLIST_EN, prefix_len: 3 };
const FRENCH: WordList = WordList { words: MONERO_WORDLIST_FR, prefix_len: 4 };
const LANGUAGES: [&WordList; 2] = [&ENGLISH, &FRENCH];

fn prefix(word: &str, len: usize) -> String {
    word.chars().take(len).collect()
}

impl WordList {
    /// Index of `word`, matched on its unique prefix (typed words may be longer or shorter
    /// than the canonical one, as long as the prefix is complete).
    fn find(&self, word: &str) -> Option<u64> {
        let p = prefix(word, self.prefix_len);
        if p.chars().count() < self.prefix_len && !self.words.contains(&word) {
            return None;
        }
        self.words.iter().position(|w| prefix(w, self.prefix_len) == p).map(|i| i as u64)
    }

    fn checksum_index(&self, data_words: &[&str]) -> usize {
        let joined: String = data_words.iter().map(|w| prefix(w, self.prefix_len)).collect();
        (crc32_zlib(joined.as_bytes()) % data_words.len() as u32) as usize
    }
}

/// Generate a fresh 25-word Monero seed phrase.
/// Returns the phrase as a space-separated string, zeroized on drop.
pub fn generate_seed() -> Result<Zeroizing<String>, SeedError> {
    let mut bytes = [0u8; 32];
    getrandom::getrandom(&mut bytes).map_err(|e| SeedError::Rng(e.to_string()))?;
    bytes_to_mnemonic(&bytes)
}

/// Fix the checksum word of a 25-word mnemonic that was generated with the old buggy algorithm.
/// Keeps the first 24 words intact and replaces the 25th word with the correct checksum.
/// Returns None if the mnemonic is already valid or has an unexpected format.
/// English only: the buggy versions only generated English seeds.
pub fn fix_seed_checksum(phrase: &str) -> Option<Zeroizing<String>> {
    let words: Vec<&str> = phrase.split_whitespace().collect();
    if words.len() != 25 { return None; }

    let data_words = &words[..24];
    for w in data_words {
        if !MONERO_WORDLIST_EN.contains(w) { return None; }
    }

    let idx = ENGLISH.checksum_index(data_words);
    if prefix(data_words[idx], 3) == prefix(words[24], 3) {
        return None; // Already valid — no fix needed
    }

    let mut fixed = data_words.to_vec();
    fixed.push(data_words[idx]);
    Some(Zeroizing::new(fixed.join(" ")))
}

/// Convert 32 raw bytes to a 25-word English mnemonic.
/// Algorithm: Monero 3-word encoding over GF(N).
pub fn bytes_to_mnemonic(bytes: &[u8; 32]) -> Result<Zeroizing<String>, SeedError> {
    encode(&ENGLISH, bytes)
}

fn encode(list: &WordList, bytes: &[u8; 32]) -> Result<Zeroizing<String>, SeedError> {
    if list.words.len() as u64 != N {
        return Err(SeedError::WordlistTooShort);
    }
    let mut words: Vec<&str> = Vec::with_capacity(25);
    for chunk in bytes.chunks(4) {
        let b = u32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]) as u64;
        let w1 = b % N;
        let w2 = ((b / N) + w1) % N;
        let w3 = ((b / N / N) + w2) % N;
        words.push(list.words[w1 as usize]);
        words.push(list.words[w2 as usize]);
        words.push(list.words[w3 as usize]);
    }
    // 25th word repeats one of the 24: data_words[crc32(prefixes) % 24]
    let checksum_idx = list.checksum_index(&words);
    words.push(words[checksum_idx]);
    Ok(Zeroizing::new(words.join(" ")))
}

/// The same mnemonic written with the full words of its list (any supported language),
/// after checking every word and the checksum. Input is matched case-insensitively.
pub fn normalize_mnemonic(phrase: &str) -> Result<Zeroizing<String>, SeedError> {
    let (list, indices) = decode_indices(phrase)?;
    let words: Vec<&str> = indices.iter().map(|&i| list.words[i as usize]).collect();
    Ok(Zeroizing::new(words.join(" ")))
}

/// Verify a 25-word mnemonic (English or French) and extract the 32 raw bytes.
pub fn mnemonic_to_bytes(phrase: &str) -> Result<Zeroizing<[u8; 32]>, SeedError> {
    let (_, indices) = decode_indices(phrase)?;
    let mut bytes = [0u8; 32];
    for (i, w) in indices[..24].chunks(3).enumerate() {
        // Reverse the encoding
        let x = w[0] + N * ((N - w[0] + w[1]) % N) + N * N * ((N - w[1] + w[2]) % N);
        bytes[i * 4..i * 4 + 4].copy_from_slice(&(x as u32).to_le_bytes());
    }
    Ok(Zeroizing::new(bytes))
}

fn decode_indices(phrase: &str) -> Result<(&'static WordList, Vec<u64>), SeedError> {
    let lower = Zeroizing::new(phrase.to_lowercase());
    let words: Vec<&str> = lower.split_whitespace().collect();
    if words.len() != 25 {
        return Err(SeedError::InvalidLength(words.len()));
    }
    // A French word can also match an English 3-letter prefix: a list only wins
    // when its checksum holds too.
    let mut unknown = None;
    let mut bad_checksum = false;
    for list in LANGUAGES {
        let indices: Option<Vec<u64>> = words.iter().map(|w| list.find(w)).collect();
        let Some(indices) = indices else {
            if unknown.is_none() {
                unknown = words.iter().find(|w| list.find(w).is_none()).map(|w| w.to_string());
            }
            continue;
        };
        let canonical: Vec<&str> = indices.iter().map(|&i| list.words[i as usize]).collect();
        let idx = list.checksum_index(&canonical[..24]);
        if canonical[idx] != canonical[24] {
            bad_checksum = true;
            continue;
        }
        return Ok((list, indices));
    }
    if bad_checksum {
        return Err(SeedError::BadChecksum);
    }
    Err(SeedError::UnknownWord(unknown.unwrap_or_default()))
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn english_round_trip() {
        let bytes = [7u8; 32];
        let phrase = bytes_to_mnemonic(&bytes).unwrap();
        assert_eq!(*mnemonic_to_bytes(&phrase).unwrap(), bytes);
        assert_eq!(*mnemonic_to_bytes(&phrase.to_uppercase()).unwrap(), bytes);
    }

    #[test]
    fn french_round_trip_and_prefixes() {
        let bytes = [42u8; 32];
        let phrase = encode(&FRENCH, &bytes).unwrap();
        assert_eq!(*mnemonic_to_bytes(&phrase).unwrap(), bytes);
        // Words longer than their 4-letter prefix may be typed in full or extended.
        let short: Vec<String> = phrase.split(' ').map(|w| prefix(w, 4)).collect();
        assert_eq!(*normalize_mnemonic(&short.join(" ")).unwrap(), *phrase);
    }

    #[test]
    fn bad_checksum_and_unknown_word() {
        let phrase = bytes_to_mnemonic(&[1u8; 32]).unwrap();
        let mut words: Vec<&str> = phrase.split(' ').collect();
        let other = if words[24] == "abbey" { "zoom" } else { "abbey" };
        words[24] = other;
        assert!(matches!(mnemonic_to_bytes(&words.join(" ")), Err(SeedError::BadChecksum)));
        words[0] = "qqqq";
        assert!(matches!(mnemonic_to_bytes(&words.join(" ")), Err(SeedError::UnknownWord(_))));
    }
}
