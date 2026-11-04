//! Neighbour weave: weave adjacent chunks into reading order beside each anchor.
//! Mirrors `fetchNeighborChunks` weave in `src/lib/rag/retriever.ts`.

use std::collections::HashSet;

/// Trait implemented by types that can be woven by [`weave_neighbours`].
pub trait WeaveItem: Clone {
    type Id: std::hash::Hash + Eq + Clone;
    type DocId: PartialEq;

    fn weave_id(&self) -> Self::Id;
    fn weave_document_id(&self) -> Self::DocId;
    fn weave_chunk_index(&self) -> u32;
}

/// Weave neighbours into reading order beside each anchor, preserving the
/// anchor's rank priority. Deduplicates so no item is emitted more than once.
#[must_use]
pub fn weave_neighbours<T: WeaveItem>(anchors: &[T], neighbours: &[T], window: u32) -> Vec<T> {
    weave_neighbours_with(
        anchors,
        neighbours,
        window,
        |i| i.weave_id(),
        |i| i.weave_document_id(),
        |i| i.weave_chunk_index(),
    )
}

/// Weave neighbours using closure extractors to avoid orphan trait rule issues.
#[must_use]
pub fn weave_neighbours_with<
    T: Clone,
    Id: std::hash::Hash + Eq,
    DocId: PartialEq,
    FId,
    FDoc,
    FIdx,
>(
    anchors: &[T],
    neighbours: &[T],
    window: u32,
    get_id: FId,
    get_doc_id: FDoc,
    get_chunk_idx: FIdx,
) -> Vec<T>
where
    FId: Fn(&T) -> Id,
    FDoc: Fn(&T) -> DocId,
    FIdx: Fn(&T) -> u32,
{
    if neighbours.is_empty() || window == 0 {
        return anchors.to_vec();
    }

    let mut emitted: HashSet<Id> = HashSet::new();
    let mut ordered: Vec<T> = Vec::new();

    for anchor in anchors {
        let mut group: Vec<T> = Vec::new();
        group.push(anchor.clone());

        for n in neighbours {
            if get_doc_id(n) == get_doc_id(anchor) {
                let diff = (i64::from(get_chunk_idx(n)) - i64::from(get_chunk_idx(anchor))).abs();
                if diff <= i64::from(window) {
                    group.push(n.clone());
                }
            }
        }

        group.sort_by_key(&get_chunk_idx);

        for item in group {
            if emitted.insert(get_id(&item)) {
                ordered.push(item);
            }
        }
    }

    ordered
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Clone, Debug, PartialEq, Eq)]
    struct TestItem {
        id: String,
        doc_id: String,
        chunk_idx: u32,
    }

    impl WeaveItem for TestItem {
        type Id = String;
        type DocId = String;

        fn weave_id(&self) -> Self::Id {
            self.id.clone()
        }

        fn weave_document_id(&self) -> Self::DocId {
            self.doc_id.clone()
        }

        fn weave_chunk_index(&self) -> u32 {
            self.chunk_idx
        }
    }

    #[test]
    fn test_weave_reading_order() {
        let anchors = vec![TestItem {
            id: "a3".into(),
            doc_id: "doc1".into(),
            chunk_idx: 3,
        }];
        let neighbours = vec![
            TestItem {
                id: "n2".into(),
                doc_id: "doc1".into(),
                chunk_idx: 2,
            },
            TestItem {
                id: "n4".into(),
                doc_id: "doc1".into(),
                chunk_idx: 4,
            },
        ];

        let result = weave_neighbours(&anchors, &neighbours, 1);
        assert_eq!(result.len(), 3);
        assert_eq!(result[0].id, "n2");
        assert_eq!(result[1].id, "a3");
        assert_eq!(result[2].id, "n4");
    }

    #[test]
    fn test_weave_deduplication() {
        let anchors = vec![
            TestItem {
                id: "a2".into(),
                doc_id: "doc1".into(),
                chunk_idx: 2,
            },
            TestItem {
                id: "a3".into(),
                doc_id: "doc1".into(),
                chunk_idx: 3,
            },
        ];
        let neighbours = vec![TestItem {
            id: "n3".into(),
            doc_id: "doc1".into(),
            chunk_idx: 3,
        }];

        let result = weave_neighbours(&anchors, &neighbours, 1);
        // "a3" and "n3" have same id/content
        assert_eq!(result.len(), 3);
    }
}
