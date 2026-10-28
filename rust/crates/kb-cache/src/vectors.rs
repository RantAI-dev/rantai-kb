//! The on-disk form of a vector: little-endian f32, nothing else.

pub(crate) fn encode(v: &[f32]) -> Vec<u8> {
    v.iter().flat_map(|x| x.to_le_bytes()).collect()
}

/// `None` for an entry of the wrong size, so a truncated file is a miss.
pub(crate) fn decode(bytes: &[u8], dim: u32) -> Option<Vec<f32>> {
    if bytes.len() != dim as usize * 4 {
        return None;
    }
    Some(
        bytes
            .chunks_exact(4)
            .map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]]))
            .collect(),
    )
}
