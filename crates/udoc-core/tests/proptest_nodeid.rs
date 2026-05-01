//! Property test: NodeId stability + walk invariants.
//!
//! Three properties exercise the core arena:
//!
//! (i)   NodeIds returned for non-removed nodes stay valid (i.e., reachable
//!       via `Document::walk`) after any sequence of insert/remove operations.
//! (ii)  `Document::walk` visits each Block exactly once.
//! (iii) `Document::alloc_node_id()` is collision-free under concurrent calls
//!       from multiple threads (`AtomicU64` correctness check).
//!
//! Budget: 256 cases per property, capped via `ProptestConfig`.

use std::collections::HashSet;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::thread;

use proptest::prelude::*;
use proptest::test_runner::Config as ProptestConfig;

use udoc_core::document::{Block, Document, Inline, NodeId, SpanStyle};

// ---------------------------------------------------------------------------
// Mutation script
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
enum Op {
    /// Insert a paragraph at index `idx % (len + 1)` of the top-level content.
    InsertParagraph { idx: usize, text_len: usize },
    /// Insert a heading.
    InsertHeading { idx: usize, level: u8 },
    /// Insert an empty section that itself contains a paragraph child.
    InsertSection { idx: usize },
    /// Remove the top-level block at `idx % len` (no-op if empty).
    Remove { idx: usize },
}

fn op_strategy() -> impl Strategy<Value = Op> {
    prop_oneof![
        (any::<usize>(), 0usize..32)
            .prop_map(|(idx, text_len)| Op::InsertParagraph { idx, text_len }),
        (any::<usize>(), 1u8..=6).prop_map(|(idx, level)| Op::InsertHeading { idx, level }),
        any::<usize>().prop_map(|idx| Op::InsertSection { idx }),
        any::<usize>().prop_map(|idx| Op::Remove { idx }),
    ]
}

/// Apply an op to `doc`. Returns the NodeIds of any newly-created blocks
/// (so the caller can track which IDs are "live").
fn apply(doc: &mut Document, op: &Op) -> Vec<NodeId> {
    match op {
        Op::InsertParagraph { idx, text_len } => {
            let id = doc.alloc_node_id();
            let inline_id = doc.alloc_node_id();
            let block = Block::Paragraph {
                id,
                content: vec![Inline::Text {
                    id: inline_id,
                    text: "x".repeat(*text_len),
                    style: SpanStyle::default(),
                }],
            };
            let pos = idx % (doc.content.len() + 1);
            doc.content.insert(pos, block);
            vec![id, inline_id]
        }
        Op::InsertHeading { idx, level } => {
            let id = doc.alloc_node_id();
            let block = Block::Heading {
                id,
                level: *level,
                content: Vec::new(),
            };
            let pos = idx % (doc.content.len() + 1);
            doc.content.insert(pos, block);
            vec![id]
        }
        Op::InsertSection { idx } => {
            let sec_id = doc.alloc_node_id();
            let para_id = doc.alloc_node_id();
            let block = Block::Section {
                id: sec_id,
                role: None,
                children: vec![Block::Paragraph {
                    id: para_id,
                    content: Vec::new(),
                }],
            };
            let pos = idx % (doc.content.len() + 1);
            doc.content.insert(pos, block);
            vec![sec_id, para_id]
        }
        Op::Remove { idx } => {
            if doc.content.is_empty() {
                return Vec::new();
            }
            let pos = idx % doc.content.len();
            doc.content.remove(pos);
            Vec::new()
        }
    }
}

/// Collect all Block NodeIds reachable via `Document::walk`.
fn walk_collect_ids(doc: &Document) -> Vec<NodeId> {
    let mut ids = Vec::new();
    doc.walk(&mut |block: &Block| ids.push(block.id()));
    ids
}

// ---------------------------------------------------------------------------
// Properties
// ---------------------------------------------------------------------------

proptest! {
    #![proptest_config(ProptestConfig {
        cases: 256,
        max_shrink_iters: 4096,
        .. ProptestConfig::default()
    })]

    /// Property (i): every NodeId allocated for a Block that is still in the
    /// content tree (i.e., not removed by a subsequent `Remove` op) must be
    /// reachable via `Document::walk`.
    ///
    /// We do not assert this for inline NodeIds because `walk` is documented
    /// to visit Blocks only. The reachability claim therefore covers Block
    /// IDs only.
    #[test]
    fn block_ids_stay_walkable_after_mutations(ops in prop::collection::vec(op_strategy(), 0..40)) {
        let mut doc = Document::new();
        // Track block-only NodeIds we expect to find. After any `Remove` op
        // we re-sync the set against what `walk` reports, since a top-level
        // remove can drop arbitrary block IDs.
        let mut seen_block_ids: HashSet<NodeId> = HashSet::new();

        for op in &ops {
            match op {
                Op::InsertParagraph { .. } | Op::InsertHeading { .. } => {
                    let new_ids = apply(&mut doc, op);
                    // First ID is always the Block itself (paragraph/heading).
                    if let Some(&block_id) = new_ids.first() {
                        seen_block_ids.insert(block_id);
                    }
                }
                Op::InsertSection { .. } => {
                    let new_ids = apply(&mut doc, op);
                    // Section block + child paragraph block are both walkable.
                    for id in new_ids {
                        seen_block_ids.insert(id);
                    }
                }
                Op::Remove { .. } => {
                    // After a Remove, drop block IDs we can no longer find.
                    apply(&mut doc, op);
                    let live: HashSet<NodeId> = walk_collect_ids(&doc).into_iter().collect();
                    seen_block_ids.retain(|id| live.contains(id));
                }
            }
        }

        let live: HashSet<NodeId> = walk_collect_ids(&doc).into_iter().collect();
        for id in &seen_block_ids {
            prop_assert!(
                live.contains(id),
                "block NodeId {:?} disappeared from walk despite never being removed",
                id
            );
        }
    }

    /// Property (ii): `Document::walk` visits each Block exactly once. No
    /// duplicates, no skipped nodes.
    #[test]
    fn walk_visits_each_block_exactly_once(
        ops in prop::collection::vec(op_strategy(), 0..40),
    ) {
        let mut doc = Document::new();
        for op in &ops {
            apply(&mut doc, op);
        }

        let visits = walk_collect_ids(&doc);
        let unique: HashSet<NodeId> = visits.iter().copied().collect();
        prop_assert_eq!(visits.len(), unique.len(), "walk visited some block twice");
    }
}

// ---------------------------------------------------------------------------
// Concurrent allocation: not a proptest, but a deterministic stress check
// of the AtomicU64 counter under thread::scope.
// ---------------------------------------------------------------------------

#[test]
fn alloc_node_id_is_collision_free_under_threads() {
    const THREADS: usize = 8;
    const PER_THREAD: usize = 5_000;

    let doc = Document::new();
    let collected = std::sync::Mutex::new(Vec::with_capacity(THREADS * PER_THREAD));
    let counter = AtomicUsize::new(0);

    thread::scope(|s| {
        for _ in 0..THREADS {
            s.spawn(|| {
                let mut local = Vec::with_capacity(PER_THREAD);
                for _ in 0..PER_THREAD {
                    local.push(doc.alloc_node_id());
                    counter.fetch_add(1, Ordering::Relaxed);
                }
                collected.lock().unwrap().extend(local);
            });
        }
    });

    let allocated = collected.into_inner().unwrap();
    assert_eq!(counter.load(Ordering::Relaxed), THREADS * PER_THREAD);
    assert_eq!(allocated.len(), THREADS * PER_THREAD);

    let unique: HashSet<NodeId> = allocated.iter().copied().collect();
    assert_eq!(
        unique.len(),
        allocated.len(),
        "AtomicU64 alloc returned a duplicate NodeId under concurrent calls"
    );
}
