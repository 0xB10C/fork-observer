//! Benchmarks for the header tree functions in [`fork_observer::headertree`].
//!
//! Every time a node reports a new block, `update_header_tree_cache` runs
//! `strip_tree`, `recent_forks` and `stale_blocks` over the *whole* header
//! tree of the network. On mainnet that tree holds close to a million headers,
//! so these functions decide how long a new block takes to show up. These
//! benchmarks measure them on synthetic trees of increasing size, so a change
//! to them can be compared before and after.
//!
//! The synthetic tree is one long chain with a one-block stale fork every
//! [`FORK_EVERY`] blocks. That gives `size / FORK_EVERY` heights with more than
//! one block - the "interesting" heights `strip_tree` keeps - which is enough
//! for both a mainnet-like `max_interesting_heights = 100` and the much larger
//! limits used by downstream deployments to actually take effect.
//!
//! Run all of them with `cargo bench`, or a subset by filtering on the
//! benchmark name, for example `cargo bench -- 'strip_tree/.*/100000'`. Store
//! a run with `cargo bench -- --save-baseline before` and compare a change
//! against it with `cargo bench -- --baseline before`.

use std::collections::{BTreeSet, HashMap};
use std::sync::Arc;

use corepc_client::bitcoin::blockdata::block::{Header, Version};
use corepc_client::bitcoin::hashes::Hash;
use corepc_client::bitcoin::{BlockHash, CompactTarget, TxMerkleNode};
use criterion::{criterion_group, criterion_main, BenchmarkId, Criterion, Throughput};
use fork_observer::cache::{MAX_FORKS_IN_CACHE, MAX_STALE_BLOCKS};
use fork_observer::headertree::{
    recent_forks, sorted_interesting_heights, stale_blocks, strip_tree,
};
use fork_observer::types::{HeaderInfo, Tree, TreeInfo};
use petgraph::graph::{DiGraph, NodeIndex};
use tokio::runtime::Runtime;
use tokio::sync::Mutex;

/// Header tree sizes to benchmark with. The largest is what a fork-observer
/// instance following mainnet since genesis holds.
const TREE_SIZES: &[usize] = &[10_000, 100_000, 1_000_000];
/// A stale block is added every this many heights.
const FORK_EVERY: u64 = 100;
/// The `max_interesting_heights` settings to benchmark `strip_tree` with: the
/// value in the example config, and the much larger limit used by some
/// downstream deployments.
const MAX_INTERESTING_HEIGHTS: &[usize] = &[100, 5_000];
/// Benchmarks on trees at or above this size take seconds per iteration, so
/// they run with the smallest sample criterion allows.
const LARGE_TREE: usize = 1_000_000;
const SMALL_SAMPLE_SIZE: usize = 10;

// The mainnet "difficulty 1" target. The work of a header only matters for
// picking the most-work chain in `stale_blocks`, so all headers share it.
const BITS: u32 = 0x1d00_ffff;

// Builds a (structurally valid, PoW-invalid) header. The nonce gives headers
// that share a parent distinct block hashes.
fn header(prev: BlockHash, nonce: u32) -> Header {
    Header {
        version: Version::from_consensus(0x2000_0000),
        prev_blockhash: prev,
        merkle_root: TxMerkleNode::all_zeros(),
        time: 1_600_000_000,
        bits: CompactTarget::from_consensus(BITS),
        nonce,
    }
}

// Adds a header to the tree, connected to its parent when it has one.
fn add(
    graph: &mut DiGraph<HeaderInfo, bool>,
    index: &mut HashMap<BlockHash, NodeIndex>,
    height: u64,
    header: Header,
    prev: Option<NodeIndex>,
) -> NodeIndex {
    let hash = header.block_hash();
    let idx = graph.add_node(HeaderInfo {
        height,
        header,
        miner: String::new(),
    });
    index.insert(hash, idx);
    if let Some(prev) = prev {
        graph.add_edge(prev, idx, false);
    }
    idx
}

/// Builds a header tree of `size` headers: a single chain from height 0 with
/// a one-block stale fork branching off every [`FORK_EVERY`] heights. The
/// stale blocks count towards `size`.
fn synthetic_tree(size: usize) -> Tree {
    let mut graph: DiGraph<HeaderInfo, bool> = DiGraph::with_capacity(size, size);
    let mut index: HashMap<BlockHash, NodeIndex> = HashMap::with_capacity(size);

    let genesis = header(BlockHash::all_zeros(), 0);
    let mut prev_hash = genesis.block_hash();
    let mut prev_idx = add(&mut graph, &mut index, 0, genesis, None);
    let mut height: u64 = 1;
    while graph.node_count() < size {
        if height.is_multiple_of(FORK_EVERY) && graph.node_count() + 1 < size {
            // A stale sibling of the block we are about to add. Its nonce
            // differs from the main chain block's, so the hashes differ.
            add(
                &mut graph,
                &mut index,
                height,
                header(prev_hash, 1),
                Some(prev_idx),
            );
        }
        let h = header(prev_hash, 0);
        prev_hash = h.block_hash();
        prev_idx = add(&mut graph, &mut index, height, h, Some(prev_idx));
        height += 1;
    }

    let tree_info: TreeInfo = (graph, index);
    Arc::new(Mutex::new(tree_info))
}

/// The tip heights a network's nodes would report for the synthetic tree: the
/// chain tip, which is the highest header.
fn tip_heights(size: usize) -> BTreeSet<u64> {
    let mut tips = BTreeSet::new();
    tips.insert(size as u64 - size as u64 / FORK_EVERY - 1);
    tips
}

fn sample_size_for(size: usize) -> usize {
    if size >= LARGE_TREE {
        SMALL_SAMPLE_SIZE
    } else {
        100
    }
}

fn bench_sorted_interesting_heights(c: &mut Criterion) {
    let rt = Runtime::new().unwrap();
    let mut group = c.benchmark_group("sorted_interesting_heights");
    for &size in TREE_SIZES {
        let tree = synthetic_tree(size);
        group.throughput(Throughput::Elements(size as u64));
        group.sample_size(sample_size_for(size));
        group.bench_with_input(BenchmarkId::from_parameter(size), &size, |b, &size| {
            b.to_async(&rt).iter(|| async {
                sorted_interesting_heights(&tree, usize::MAX, tip_heights(size)).await
            })
        });
    }
    group.finish();
}

fn bench_strip_tree(c: &mut Criterion) {
    let rt = Runtime::new().unwrap();
    let mut group = c.benchmark_group("strip_tree");
    for &size in TREE_SIZES {
        let tree = synthetic_tree(size);
        for &max_interesting_heights in MAX_INTERESTING_HEIGHTS {
            group.throughput(Throughput::Elements(size as u64));
            group.sample_size(sample_size_for(size));
            group.bench_with_input(
                BenchmarkId::new(
                    format!("max_interesting_heights_{}", max_interesting_heights),
                    size,
                ),
                &size,
                |b, &size| {
                    b.to_async(&rt).iter(|| async {
                        strip_tree(&tree, max_interesting_heights, tip_heights(size), None).await
                    })
                },
            );
        }
    }
    group.finish();
}

fn bench_stale_blocks(c: &mut Criterion) {
    let rt = Runtime::new().unwrap();
    let mut group = c.benchmark_group("stale_blocks");
    for &size in TREE_SIZES {
        let tree = synthetic_tree(size);
        group.throughput(Throughput::Elements(size as u64));
        group.sample_size(sample_size_for(size));
        group.bench_with_input(BenchmarkId::from_parameter(size), &size, |b, _| {
            b.to_async(&rt)
                .iter(|| async { stale_blocks(&tree, MAX_STALE_BLOCKS).await })
        });
    }
    group.finish();
}

fn bench_recent_forks(c: &mut Criterion) {
    let rt = Runtime::new().unwrap();
    let mut group = c.benchmark_group("recent_forks");
    for &size in TREE_SIZES {
        let tree = synthetic_tree(size);
        group.throughput(Throughput::Elements(size as u64));
        group.sample_size(sample_size_for(size));
        group.bench_with_input(BenchmarkId::from_parameter(size), &size, |b, _| {
            b.to_async(&rt)
                .iter(|| async { recent_forks(&tree, MAX_FORKS_IN_CACHE).await })
        });
    }
    group.finish();
}

criterion_group!(
    benches,
    bench_sorted_interesting_heights,
    bench_strip_tree,
    bench_stale_blocks,
    bench_recent_forks
);
criterion_main!(benches);
