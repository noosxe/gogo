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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plan_basics() {
        let specs = plan(0);
        assert!(specs.is_empty());

        let specs = plan(CHUNK_SIZE);
        assert_eq!(specs.len(), 1);
        assert_eq!(
            specs[0],
            ChunkSpec {
                index: 0,
                start: 0,
                end: CHUNK_SIZE - 1
            }
        );

        let specs = plan(CHUNK_SIZE + 1);
        assert_eq!(specs.len(), 2);
        assert_eq!(
            specs[1].end, CHUNK_SIZE,
            "one byte tail: second chunk covers exactly one byte"
        );
    }

    #[test]
    fn bitmap_counts_and_completes() {
        let mut bm = CompletionBitmap::new(4);
        assert!(!bm.is_complete());
        bm.mark_done(2);
        assert!(bm.is_done(2));
        assert_eq!(bm.remaining().collect::<Vec<_>>(), vec![0, 1, 3]);
        bm.mark_done(0);
        bm.mark_done(1);
        bm.mark_done(3);
        assert!(bm.is_complete());
    }

    use proptest::prelude::*;

    proptest::proptest! {
        /// 08 §4 planner invariants: coverage, contiguity, span bounds.
        #[test]
        fn plan_covers_exactly(total_size in 0u64..(5 * CHUNK_SIZE)) {
            let specs = plan(total_size);
            let expected_count = total_size.div_ceil(CHUNK_SIZE) as usize;
            prop_assert_eq!(specs.len(), expected_count);
            let covered: u64 = specs.iter().map(|s| s.end - s.start + 1).sum();
            prop_assert_eq!(covered, total_size);
            for pair in specs.windows(2) {
                prop_assert_eq!(pair[1].start, pair[0].end + 1);
            }
            for spec in &specs {
                prop_assert!(spec.end - spec.start < CHUNK_SIZE || (spec.end - spec.start + 1) == CHUNK_SIZE);
            }
            if let Some(last) = specs.last() {
                prop_assert_eq!(last.end + 1, total_size);
            }
        }
    }
}
