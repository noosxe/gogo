//! Chunk plan, Range math, completion bitmap (`docs/design/04` §4).

/// Live-verified chunk size (docs/04: 10 MiB, last chunk partial).
pub const CHUNK_SIZE: u64 = crate::gog::checksum::CHUNK_SIZE;

/// Inclusive byte range of one chunk — maps 1:1 to the checksum XML's
/// `<chunk id from to>` entries.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChunkSpec {
    pub index: u32,
    pub start: u64,
    /// Inclusive end offset (matches XML `to`).
    pub end: u64,
}

/// Plan `count` chunks covering `total_size` bytes (authoritative XML total).
pub fn plan(total_size: u64) -> Vec<ChunkSpec> {
    let count = total_size.div_ceil(CHUNK_SIZE);
    (0..count)
        .map(|i| {
            let start = i * CHUNK_SIZE;
            ChunkSpec {
                index: i as u32,
                start,
                end: (start + CHUNK_SIZE).min(total_size) - 1,
            }
        })
        .collect()
}

/// Which chunks are done; workers mark, resume reads (`docs/design/04` §6).
#[derive(Debug, Clone)]
pub struct CompletionBitmap {
    done: Vec<bool>,
}

impl CompletionBitmap {
    pub fn new(len: usize) -> Self {
        Self {
            done: vec![false; len],
        }
    }

    pub fn mark_done(&mut self, index: u32) {
        self.done[index as usize] = true;
    }

    pub fn is_done(&self, index: u32) -> bool {
        self.done[index as usize]
    }

    pub fn is_complete(&self) -> bool {
        self.done.iter().all(|&d| d)
    }

    pub fn remaining(&self) -> impl Iterator<Item = u32> + '_ {
        (0..self.done.len()).filter_map(|i| (!self.done[i]).then_some(i as u32))
    }
}
