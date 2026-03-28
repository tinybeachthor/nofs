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
