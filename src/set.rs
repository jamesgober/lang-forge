//! Token sets: fixed-width bitsets over a language's token kinds, stored
//! back to back in one allocation.
//!
//! The parser asks one question millions of times — can this token start that
//! piece of grammar? — so every FIRST, FOLLOW, and stop set is a row of `u64`
//! words in a single flat vector, and membership is a shift and a mask on
//! memory that is usually already in cache.

use alloc::vec::Vec;

/// The identifier of one set within a [`Sets`] arena.
pub(crate) type SetId = u32;

/// Marks "no set" where a set identifier is optional.
pub(crate) const NO_SET: SetId = SetId::MAX;

/// An arena of equally sized bitsets.
#[derive(Clone, Debug, Default)]
pub(crate) struct Sets {
    /// Words per set.
    width: usize,
    words: Vec<u64>,
}

impl Sets {
    /// An empty arena whose sets hold bits `0..bits`.
    pub(crate) fn new(bits: usize) -> Self {
        Self {
            width: bits.div_ceil(64).max(1),
            words: Vec::new(),
        }
    }

    /// Adds an empty set and returns its identifier.
    pub(crate) fn alloc(&mut self) -> SetId {
        let id = self.words.len() / self.width;
        self.words.resize(self.words.len() + self.width, 0);
        id as SetId
    }

    #[inline]
    fn row(&self, id: SetId) -> &[u64] {
        let start = id as usize * self.width;
        &self.words[start..start + self.width]
    }

    #[inline]
    fn row_mut(&mut self, id: SetId) -> &mut [u64] {
        let start = id as usize * self.width;
        &mut self.words[start..start + self.width]
    }

    /// Whether `bit` is in set `id`.
    #[inline]
    pub(crate) fn contains(&self, id: SetId, bit: usize) -> bool {
        let word = self.words[id as usize * self.width + (bit >> 6)];
        (word >> (bit & 63)) & 1 != 0
    }

    /// Adds `bit` to set `id`; returns whether the set changed.
    pub(crate) fn insert(&mut self, id: SetId, bit: usize) -> bool {
        let word = &mut self.row_mut(id)[bit >> 6];
        let mask = 1u64 << (bit & 63);
        let changed = *word & mask == 0;
        *word |= mask;
        changed
    }

    /// Adds every member of `src` to `dst`; returns whether `dst` changed.
    pub(crate) fn union(&mut self, dst: SetId, src: SetId) -> bool {
        if dst == src {
            return false;
        }
        let width = self.width;
        let (d, s) = (dst as usize * width, src as usize * width);
        let mut changed = false;
        for i in 0..width {
            let merged = self.words[d + i] | self.words[s + i];
            changed |= merged != self.words[d + i];
            self.words[d + i] = merged;
        }
        changed
    }

    /// Like [`union`](Self::union), but a no-op when either side is
    /// [`NO_SET`].
    pub(crate) fn join(&mut self, dst: SetId, src: SetId) -> bool {
        dst != NO_SET && src != NO_SET && self.union(dst, src)
    }

    /// A zeroed scratch row, for accumulating a set without allocating one
    /// in the arena.
    pub(crate) fn scratch(&self) -> Vec<u64> {
        alloc::vec![0; self.width]
    }

    /// Adds every member of set `id` to the scratch row `buf`.
    pub(crate) fn or_into(&self, buf: &mut [u64], id: SetId) {
        for (word, add) in buf.iter_mut().zip(self.row(id)) {
            *word |= add;
        }
    }

    /// Adds a set holding the members of the scratch row `buf`.
    pub(crate) fn alloc_from(&mut self, buf: &[u64]) -> SetId {
        let id = self.alloc();
        self.row_mut(id).copy_from_slice(buf);
        id
    }

    /// The members of set `id`, in increasing order.
    pub(crate) fn members(&self, id: SetId) -> impl Iterator<Item = usize> + '_ {
        self.row(id).iter().enumerate().flat_map(|(i, &word)| {
            let mut rest = word;
            core::iter::from_fn(move || {
                if rest == 0 {
                    return None;
                }
                let bit = rest.trailing_zeros() as usize;
                rest &= rest - 1;
                Some(i * 64 + bit)
            })
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_sets_insert_and_contains_across_words() {
        let mut sets = Sets::new(130);
        let a = sets.alloc();
        assert_eq!(sets.members(a).count(), 0);
        assert!(sets.insert(a, 0));
        assert!(sets.insert(a, 64));
        assert!(sets.insert(a, 129));
        assert!(!sets.insert(a, 129));
        assert!(sets.contains(a, 64));
        assert!(!sets.contains(a, 63));
        assert_eq!(sets.members(a).collect::<Vec<_>>(), [0, 64, 129]);
    }

    #[test]
    fn test_sets_union_reports_change() {
        let mut sets = Sets::new(10);
        let a = sets.alloc();
        let b = sets.alloc();
        let _ = sets.insert(b, 3);
        assert!(sets.union(a, b));
        assert!(!sets.union(a, b));
        assert!(!sets.union(a, a));
        assert!(sets.contains(a, 3));
    }

    #[test]
    fn test_sets_scratch_rows_and_join() {
        let mut sets = Sets::new(70);
        let a = sets.alloc();
        let _ = sets.insert(a, 69);
        let mut buf = sets.scratch();
        sets.or_into(&mut buf, a);
        let b = sets.alloc_from(&buf);
        assert!(sets.contains(b, 69));
        assert!(!sets.join(NO_SET, a));
        assert!(!sets.join(a, NO_SET));
        let c = sets.alloc();
        assert!(sets.join(c, b));
    }

    #[test]
    fn test_sets_are_independent() {
        let mut sets = Sets::new(1);
        let a = sets.alloc();
        let b = sets.alloc();
        let _ = sets.insert(a, 0);
        assert!(!sets.contains(b, 0));
        assert!(sets.contains(a, 0));
    }
}
