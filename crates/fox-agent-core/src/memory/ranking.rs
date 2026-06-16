use std::cmp::Reverse;
use std::collections::BinaryHeap;

struct ScoredItem<T> {
    score: f32,
    ordinal: usize,
    value: T,
}

impl<T> PartialEq for ScoredItem<T> {
    fn eq(&self, other: &Self) -> bool {
        self.score.to_bits() == other.score.to_bits() && self.ordinal == other.ordinal
    }
}
impl<T> Eq for ScoredItem<T> {}
impl<T> PartialOrd for ScoredItem<T> {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}
impl<T> Ord for ScoredItem<T> {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.score.total_cmp(&other.score).then_with(|| self.ordinal.cmp(&other.ordinal))
    }
}

/// Keep the top `limit` items by descending `f32` score.
/// Returns items in descending score order.
pub fn top_k_by_score<T, I>(items: I, limit: usize) -> Vec<(T, f32)>
where
    I: IntoIterator<Item = (T, f32)>,
{
    if limit == 0 {
        return Vec::new();
    }
    let mut heap: BinaryHeap<Reverse<ScoredItem<T>>> = BinaryHeap::new();
    for (ordinal, (value, score)) in items.into_iter().enumerate() {
        let cand = Reverse(ScoredItem { score, ordinal, value });
        if heap.len() < limit {
            heap.push(cand);
        } else if heap.peek().map(|s| score > s.0.score).unwrap_or(false) {
            heap.pop();
            heap.push(cand);
        }
    }
    let mut results: Vec<_> = heap.into_iter().map(|Reverse(ScoredItem { value, score, ordinal })| (value, score, ordinal)).collect();
    results.sort_by(|a, b| b.1.total_cmp(&a.1).then_with(|| a.2.cmp(&b.2)));
    results.into_iter().map(|(v, s, _)| (v, s)).collect()
}

struct OrdItem<T, K: Ord> {
    key: K,
    ordinal: usize,
    value: T,
}
impl<T, K: Ord> PartialEq for OrdItem<T, K> {
    fn eq(&self, other: &Self) -> bool { self.key == other.key && self.ordinal == other.ordinal }
}
impl<T, K: Ord> Eq for OrdItem<T, K> {}
impl<T, K: Ord> PartialOrd for OrdItem<T, K> {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> { Some(self.cmp(other)) }
}
impl<T, K: Ord> Ord for OrdItem<T, K> {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.key.cmp(&other.key).then_with(|| self.ordinal.cmp(&other.ordinal))
    }
}

/// Keep the top `limit` items by descending `K` key.
pub fn top_k_by_ord<T, K: Ord, I>(items: I, limit: usize) -> Vec<(T, K)>
where
    I: IntoIterator<Item = (T, K)>,
{
    if limit == 0 {
        return Vec::new();
    }
    let mut heap: BinaryHeap<Reverse<OrdItem<T, K>>> = BinaryHeap::new();
    for (ordinal, (value, key)) in items.into_iter().enumerate() {
        let cand = Reverse(OrdItem { key, ordinal, value });
        if heap.len() < limit {
            heap.push(cand);
        } else if heap.peek().map(|s| cand.0.key > s.0.key).unwrap_or(false) {
            heap.pop();
            heap.push(cand);
        }
    }
    let mut results: Vec<_> = heap.into_iter().map(|Reverse(OrdItem { value, key, ordinal })| (value, key, ordinal)).collect();
    results.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.2.cmp(&b.2)));
    results.into_iter().map(|(v, k, _)| (v, k)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_top_k_by_score_basic() {
        let ranked = top_k_by_score([("a", 1.0), ("b", 3.0), ("c", 2.0)], 2);
        assert_eq!(ranked, vec![("b", 3.0), ("c", 2.0)]);
    }

    #[test]
    fn test_top_k_by_score_zero_limit() {
        assert!(top_k_by_score([("a", 1.0)], 0).is_empty());
    }

    #[test]
    fn test_top_k_by_ord_basic() {
        let ranked = top_k_by_ord([("a", 1), ("b", 3), ("c", 2)], 2);
        assert_eq!(ranked, vec![("b", 3), ("c", 2)]);
    }
}
