//! A map from `u32` slices to dense ids, for subset construction.
//!
//! Ported from grammar-lang 1.0 (`util.rs`).

use alloc::{vec, vec::Vec};

/// Numbers distinct `u32` slices in insertion order.
///
/// Subset construction and the LR(0) automaton both discover states as sorted
/// sets of smaller things — NFA states, LR items — and must recognise a set
/// they have seen before. The keys live back to back in one buffer; an
/// open-addressed index of `id + 1` entries (0 marks a free slot) finds them by
/// hash, so a lookup is one hash, a probe or two, and a slice compare.
#[derive(Debug)]
pub(crate) struct SliceMap {
    data: Vec<u32>,
    ends: Vec<usize>,
    hashes: Vec<u64>,
    index: Vec<u32>,
}

impl SliceMap {
    /// An empty map.
    pub(crate) fn new() -> Self {
        Self {
            data: Vec::new(),
            ends: Vec::new(),
            hashes: Vec::new(),
            index: vec![0; 64],
        }
    }

    /// The number of distinct slices stored.
    #[inline]
    pub(crate) fn len(&self) -> usize {
        self.ends.len()
    }

    /// The total length of every stored slice.
    #[inline]
    pub(crate) fn words(&self) -> usize {
        self.data.len()
    }

    /// The slice numbered `id`.
    #[inline]
    pub(crate) fn get(&self, id: usize) -> &[u32] {
        let start = if id == 0 { 0 } else { self.ends[id - 1] };
        &self.data[start..self.ends[id]]
    }

    /// The number of `key`, inserting it first if it is new. The flag is true
    /// when the key was inserted.
    pub(crate) fn insert(&mut self, key: &[u32]) -> (u32, bool) {
        let hash = hash(key);
        let mask = self.index.len() - 1;
        let mut slot = (hash as usize) & mask;
        loop {
            match self.index[slot] {
                0 => break,
                entry => {
                    let id = (entry - 1) as usize;
                    if self.hashes[id] == hash && self.get(id) == key {
                        return (entry - 1, false);
                    }
                }
            }
            slot = (slot + 1) & mask;
        }
        let id = self.ends.len() as u32;
        self.data.extend_from_slice(key);
        self.ends.push(self.data.len());
        self.hashes.push(hash);
        self.index[slot] = id + 1;
        if self.ends.len() * 2 > self.index.len() {
            self.grow();
        }
        (id, true)
    }

    /// Doubles the index and re-places every entry.
    fn grow(&mut self) {
        let size = self.index.len() * 2;
        let mask = size - 1;
        let mut index = vec![0u32; size];
        for (id, &hash) in self.hashes.iter().enumerate() {
            let mut slot = (hash as usize) & mask;
            while index[slot] != 0 {
                slot = (slot + 1) & mask;
            }
            index[slot] = id as u32 + 1;
        }
        self.index = index;
    }
}

/// A fast, non-cryptographic hash of a `u32` slice (the Fx multiply-rotate
/// scheme), finished with a fold so the low bits used for slot selection see
/// every input word.
#[inline]
fn hash(key: &[u32]) -> u64 {
    const K: u64 = 0x517c_c1b7_2722_0a95;
    let mut h = key.len() as u64;
    for &word in key {
        h = (h.rotate_left(5) ^ u64::from(word)).wrapping_mul(K);
    }
    h ^ (h >> 32)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slice_map_numbers_distinct_slices() {
        let mut map = SliceMap::new();
        assert_eq!(map.insert(&[1, 2, 3]), (0, true));
        assert_eq!(map.insert(&[]), (1, true));
        assert_eq!(map.insert(&[1, 2, 3]), (0, false));
        assert_eq!(map.insert(&[]), (1, false));
        assert_eq!(map.get(0), &[1, 2, 3]);
        assert_eq!(map.get(1), &[] as &[u32]);
        for i in 0..1000u32 {
            let (id, new) = map.insert(&[i, i + 1]);
            assert!(new);
            assert_eq!(id, i + 2);
        }
        for i in 0..1000u32 {
            assert_eq!(map.insert(&[i, i + 1]), (i + 2, false));
        }
        assert_eq!(map.len(), 1002);
    }
}
