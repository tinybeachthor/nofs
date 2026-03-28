use reed_solomon_erasure::galois_8::ReedSolomon;

// 10 data shards + 4 parity shards — tolerates up to 4 corrupt shards per blob
pub const DATA_SHARDS: usize = 10;
pub const PARITY_SHARDS: usize = 4;

/// Encode raw blob data with Reed-Solomon error correction.
///
/// Storage format:
///   [8 bytes: original data length (u64 LE)]
///   [8 bytes: shard size (u64 LE)]
///   For each of (DATA_SHARDS + PARITY_SHARDS) shards:
///     [4 bytes: CRC32 of shard (u32 LE)]
///     [shard_size bytes: shard data]
pub fn encode(data: &[u8]) -> Vec<u8> {
    let original_len = data.len();
    let shard_size = ((original_len + DATA_SHARDS - 1) / DATA_SHARDS).max(1);

    // Build data shards, zero-padding the last one if needed
    let mut shards: Vec<Vec<u8>> = (0..DATA_SHARDS)
        .map(|i| {
            let start = i * shard_size;
            let end = ((i + 1) * shard_size).min(original_len);
            let mut shard = if start < original_len {
                data[start..end].to_vec()
            } else {
                Vec::new()
            };
            shard.resize(shard_size, 0u8);
            shard
        })
        .collect();

    // Append empty parity shards to be filled by the encoder
    for _ in 0..PARITY_SHARDS {
        shards.push(vec![0u8; shard_size]);
    }

    let rs = ReedSolomon::new(DATA_SHARDS, PARITY_SHARDS).expect("Invalid RS parameters");
    rs.encode(&mut shards).expect("RS encode failed");

    // Serialize header + shards with per-shard CRC32
    let total_shards = DATA_SHARDS + PARITY_SHARDS;
    let mut out = Vec::with_capacity(16 + total_shards * (4 + shard_size));
    out.extend_from_slice(&(original_len as u64).to_le_bytes());
    out.extend_from_slice(&(shard_size as u64).to_le_bytes());
    for shard in &shards {
        let crc = crc32fast::hash(shard);
        out.extend_from_slice(&crc.to_le_bytes());
        out.extend_from_slice(shard);
    }
    out
}

/// Decode Reed-Solomon encoded blob data, recovering from up to PARITY_SHARDS corrupt shards.
///
/// Falls back to returning the raw bytes unchanged if the format is unrecognised (e.g. legacy blobs).
/// Shards with a failing CRC32 are treated as missing; up to PARITY_SHARDS such shards can be
/// reconstructed automatically.
pub fn decode(encoded: &[u8]) -> Vec<u8> {
    if encoded.len() < 16 {
        return encoded.to_vec();
    }

    let original_len = u64::from_le_bytes(encoded[..8].try_into().unwrap()) as usize;
    let shard_size   = u64::from_le_bytes(encoded[8..16].try_into().unwrap()) as usize;

    let total_shards = DATA_SHARDS + PARITY_SHARDS;
    let expected_len = 16 + total_shards * (4 + shard_size);

    if shard_size == 0 || encoded.len() != expected_len {
        // Not in RS format — return as-is (handles legacy / empty blobs)
        return encoded.to_vec();
    }

    // Parse shards, marking those with bad CRC as missing (None)
    let mut shards: Vec<Option<Vec<u8>>> = Vec::with_capacity(total_shards);
    let mut pos = 16usize;
    for _ in 0..total_shards {
        let crc_stored = u32::from_le_bytes(encoded[pos..pos + 4].try_into().unwrap());
        let shard_data = encoded[pos + 4..pos + 4 + shard_size].to_vec();
        let crc_actual = crc32fast::hash(&shard_data);
        shards.push(if crc_stored == crc_actual { Some(shard_data) } else { None });
        pos += 4 + shard_size;
    }

    let missing = shards.iter().filter(|s| s.is_none()).count();
    if missing > 0 {
        let rs = ReedSolomon::new(DATA_SHARDS, PARITY_SHARDS).expect("Invalid RS parameters");
        if rs.reconstruct(&mut shards).is_err() {
            log::warn!("RS reconstruction failed ({} shards missing)", missing);
        }
    }

    // Reassemble original data from data shards
    let mut data = Vec::with_capacity(DATA_SHARDS * shard_size);
    for shard in shards.into_iter().take(DATA_SHARDS) {
        match shard {
            Some(s) => data.extend_from_slice(&s),
            None    => data.extend_from_slice(&vec![0u8; shard_size]),
        }
    }
    data.truncate(original_len);
    data
}

#[cfg(test)]
mod tests {
    use super::*;

    // Flip the first byte of shard `idx`'s data region so its CRC check fails.
    fn corrupt_shard(encoded: &mut Vec<u8>, shard_idx: usize) {
        let shard_size = u64::from_le_bytes(encoded[8..16].try_into().unwrap()) as usize;
        let data_offset = 16 + shard_idx * (4 + shard_size) + 4;
        encoded[data_offset] ^= 0xFF;
    }

    #[test]
    fn roundtrip_small() {
        let data = b"hello, world!";
        assert_eq!(decode(&encode(data)), data);
    }

    #[test]
    fn roundtrip_empty() {
        assert_eq!(decode(&encode(b"")), b"");
    }

    #[test]
    fn roundtrip_exactly_one_shard() {
        // DATA_SHARDS bytes: each shard is exactly 1 byte
        let data: Vec<u8> = (0..DATA_SHARDS as u8).collect();
        assert_eq!(decode(&encode(&data)), data);
    }

    #[test]
    fn roundtrip_large() {
        // Several MB so shard_size > 1
        let data: Vec<u8> = (0..200_000).map(|i| (i % 251) as u8).collect();
        assert_eq!(decode(&encode(&data)), data);
    }

    #[test]
    fn recover_one_corrupt_shard() {
        let data = b"shard recovery test data";
        let mut encoded = encode(data);
        corrupt_shard(&mut encoded, 3);
        assert_eq!(decode(&encoded), data);
    }

    #[test]
    fn recover_max_corrupt_shards() {
        let data = b"max corruption tolerance test";
        let mut encoded = encode(data);
        // Corrupt exactly PARITY_SHARDS shards — the maximum recoverable
        for i in 0..PARITY_SHARDS {
            corrupt_shard(&mut encoded, i * 2); // spread across data shards
        }
        assert_eq!(decode(&encoded), data);
    }

    #[test]
    fn recover_corrupt_parity_shards() {
        let data = b"corrupt parity only";
        let mut encoded = encode(data);
        // Corrupt all parity shards — data shards are intact so recovery is trivial
        for i in DATA_SHARDS..DATA_SHARDS + PARITY_SHARDS {
            corrupt_shard(&mut encoded, i);
        }
        assert_eq!(decode(&encoded), data);
    }

    #[test]
    fn legacy_passthrough_short() {
        // Bytes that don't match the RS format header are returned unchanged
        let raw = b"not encoded data";
        assert_eq!(decode(raw), raw);
    }

    #[test]
    fn legacy_passthrough_wrong_length() {
        // Encode, then truncate so length doesn't match expected — should pass through
        let mut encoded = encode(b"some data");
        encoded.pop();
        let original = encoded.clone();
        assert_eq!(decode(&encoded), original);
    }
}
