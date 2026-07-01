use std::collections::BTreeMap;
use std::collections::BTreeSet;
use std::collections::HashMap;
use std::collections::HashSet;

use crate::types::{Fork, HeaderInfoJson, StaleBlockJson, Tree};

use corepc_client::bitcoin::pow::Work;
use log::{debug, warn};
use petgraph::graph::NodeIndex;
use petgraph::visit::{Dfs, EdgeRef};

pub async fn sorted_interesting_heights(
    tree: &Tree,
    max_interesting_heights: usize,
    tip_heights: BTreeSet<u64>,
) -> Vec<u64> {
    let tree_locked = tree.lock().await;
    if tree_locked.0.node_count() == 0 {
        warn!("tried to collapse an empty tree!");
        return vec![];
    }

    // We are intersted in all heights where we know more than one block
    // (as this indicates a fork).
    let mut height_occurences: BTreeMap<u64, usize> = BTreeMap::new();
    for node in tree_locked.0.raw_nodes() {
        let counter = height_occurences.entry(node.weight.height).or_insert(0);
        *counter += 1;
    }
    let heights_with_multiple_blocks: Vec<u64> = height_occurences
        .iter()
        .filter(|(_, v)| **v > 1)
        .map(|(k, _)| *k)
        .collect();

    // Combine the heights with multiple blocks with the tip_heights.
    let mut interesting_heights_set: BTreeSet<u64> = heights_with_multiple_blocks
        .iter()
        .map(|i| *i)
        .chain(tip_heights)
        .collect();

    // We are also interested in the block with the max height. We should
    // already have that in `tip_heights`, but include it here just to be
    // sure.
    let max_height: u64 = height_occurences
        .iter()
        .map(|(k, _)| *k)
        .max()
        .expect("we should have at least one height here as we have blocks");
    interesting_heights_set.insert(max_height);

    let mut interesting_heights: Vec<u64> = interesting_heights_set.iter().map(|h| *h).collect();
    interesting_heights.sort();

    // As, for example, testnet has a lot of forks we'd return many headers
    // via the API (causing things to slow down), we allow limiting this with
    // max_interesting_heights.
    interesting_heights = interesting_heights_set
        .iter()
        .map(|h| *h)
        .rev() // reversing: ascending -> descending
        .take(max_interesting_heights) // taking the 'last' max_interesting_heights
        .rev() // reversing: descending -> ascending
        .collect();

    // To be sure, sort again.
    interesting_heights.sort();

    interesting_heights
}

// We strip the tree of headers that aren't interesting to us.
pub async fn strip_tree(
    tree: &Tree,
    max_interesting_heights: usize,
    tip_heights: BTreeSet<u64>,
) -> Vec<HeaderInfoJson> {
    let interesting_heights =
        sorted_interesting_heights(tree, max_interesting_heights, tip_heights).await;

    let tree_locked = tree.lock().await;

    // Drop headers from our header tree that aren't 'interesting'.
    let mut striped_tree = tree_locked.0.filter_map(
        |_, header| {
            // Keep some surrounding headers for the headers we find interesting.
            for x in -2i64..=1 {
                if interesting_heights.contains(&((header.height as i64 - x) as u64)) {
                    return Some(header);
                }
            }
            None
        },
        |_, edge| Some(edge),
    );

    // We now have multiple sub header trees. To reconnect them
    // we figure out the starts of these chains (roots) and sort
    // them by height. We can't assume they are sorted when as we
    // added data from multiple nodes to the tree.

    let mut roots: Vec<NodeIndex> = striped_tree
        .externals(petgraph::Direction::Incoming)
        .collect();

    // We need this to be sorted by height if we use
    // prev_header_to_connect_to to connect to the last header
    // we saw below.
    roots.sort_by_key(|idx| striped_tree[*idx].height);

    let mut prev_header_to_connect_to: Option<NodeIndex> = None;
    for root in roots.iter() {
        // If we have a prev_header_to_connect_to, then connect
        // the current root to it.
        if let Some(prev_idx) = prev_header_to_connect_to {
            striped_tree.add_edge(prev_idx, *root, &false);
            prev_header_to_connect_to = None;
        }

        // Find the header with the maximum height in the sub chain
        // with a depth first search. This will be the header we
        // connect the next block to. This works, because:
        // - if we have an older fork, we have a clear winner (connect to this)
        // - if we are in an active fork, we don't need to connect anything
        // - if we are not in a fork, there will only be one header to connect to.
        let mut max_height: u64 = u64::default();
        let mut dfs = Dfs::new(&striped_tree, *root);
        while let Some(idx) = dfs.next(&striped_tree) {
            let height = striped_tree[idx].height;
            if height > max_height {
                max_height = height;
                prev_header_to_connect_to = Some(idx);
            }
        }
    }

    debug!(
        "done collapsing tree: roots={}, tips={}",
        striped_tree
            .externals(petgraph::Direction::Incoming)
            .count(), // root nodes
        striped_tree
            .externals(petgraph::Direction::Outgoing)
            .count(), // tip nodes
    );

    let mut headers: Vec<HeaderInfoJson> = Vec::new();
    for idx in striped_tree.node_indices() {
        let prev_nodes = striped_tree.neighbors_directed(idx, petgraph::Direction::Incoming);
        let prev_node_index: usize;
        match prev_nodes.clone().count() {
            0 => prev_node_index = usize::MAX, // indicates the start in JavaScript
            1 => {
                prev_node_index = prev_nodes
                    .last()
                    .expect("we should have exactly one previous node")
                    .index()
            }
            _ => panic!("got multiple previous nodes. this should not happen."),
        }
        headers.push(HeaderInfoJson::new(
            striped_tree[idx],
            idx.index(),
            prev_node_index,
        ));
    }

    // Sorting the headers by id helps debugging the API response.
    headers.sort_by_key(|h| h.id);

    headers
}

// Collect the stale blocks we know about. A stale block is any block that is
// not part of the active chain. This includes stale tips as well as intermediate
// (non-tip) stale blocks.
//
// The active chain is the chain with the most accumulated proof of work (its tip
// being the block with the highest cumulative work). This mirrors how Bitcoin
// itself picks the best chain and doesn't rely on the (possibly lagging or
// missing) chain-tip status the nodes report. Every block that is not an
// ancestor of - or - the most-work tip is stale.
//
// The `how_many` highest (most recent) stale blocks are returned.
pub async fn stale_blocks(tree: &Tree, how_many: usize) -> Vec<StaleBlockJson> {
    let tree_locked = tree.lock().await;
    let graph = &tree_locked.0;

    if graph.node_count() == 0 {
        return vec![];
    }

    // Compute the cumulative work for every block. Each block links to a single
    // parent (by prev_blockhash) and a parent always has a lower height than its
    // child, so processing blocks in ascending height order guarantees a block's
    // parent is processed before the block itself.
    let mut order: Vec<NodeIndex> = graph.node_indices().collect();
    order.sort_by_key(|idx| graph[*idx].height);

    let mut cumulative_work: HashMap<NodeIndex, Work> = HashMap::new();
    for idx in order.iter() {
        let own_work = graph[*idx].header.work();
        let total = match graph
            .neighbors_directed(*idx, petgraph::Direction::Incoming)
            .next()
        {
            // The parent is present and (being lower) already has its cumulative
            // work computed.
            Some(parent) => match cumulative_work.get(&parent) {
                Some(parent_work) => *parent_work + own_work,
                None => own_work,
            },
            // No parent in the tree (a root / the base of what we track).
            None => own_work,
        };
        cumulative_work.insert(*idx, total);
    }

    // The active chain ends at the block with the most cumulative work.
    let active_tip = order.iter().copied().max_by(|a, b| {
        cumulative_work[a]
            .partial_cmp(&cumulative_work[b])
            .expect("work is always comparable")
    });

    // Walk back from the most-work tip to the root, marking the active chain.
    let mut active: HashSet<NodeIndex> = HashSet::new();
    if let Some(tip) = active_tip {
        let mut current = Some(tip);
        while let Some(idx) = current {
            // If we've already visited this block, its ancestors are visited too.
            if !active.insert(idx) {
                break;
            }
            current = graph
                .neighbors_directed(idx, petgraph::Direction::Incoming)
                .next();
        }
    }

    // Every block that isn't on the active chain is stale.
    let mut stale: Vec<StaleBlockJson> = graph
        .node_indices()
        .filter(|idx| !active.contains(idx))
        .map(|idx| StaleBlockJson::new(&graph[idx]))
        .collect();

    // Return the most recent (highest) stale blocks first, capped at `how_many`.
    stale.sort_by(|a, b| b.height.cmp(&a.height));
    stale.truncate(how_many);
    stale
}

// get recent forks for rss
pub async fn recent_forks(tree: &Tree, how_many: usize) -> Vec<Fork> {
    let tree_locked = tree.lock().await;
    let tree = &tree_locked.0;

    let mut forks: Vec<Fork> = vec![];
    // it could be, that we have multiple roots. To be safe, do this for all
    // roots.
    tree.externals(petgraph::Direction::Incoming)
        .for_each(|root| {
            let mut dfs = Dfs::new(&tree, root);
            while let Some(idx) = dfs.next(&tree) {
                let outgoing_iter = tree.edges_directed(idx, petgraph::Direction::Outgoing);
                if outgoing_iter.clone().count() > 1 {
                    let common = &tree[idx];
                    let fork = Fork {
                        common: common.clone(),
                        children: outgoing_iter
                            .map(|edge| tree[edge.target()].clone())
                            .collect(),
                    };
                    forks.push(fork);
                }
            }
        });

    forks.sort_by_key(|f| f.common.height);
    forks.iter().rev().take(how_many).cloned().collect()
}

#[cfg(test)]
mod tests {
    use super::stale_blocks;
    use crate::types::{HeaderInfo, Tree, TreeInfo};
    use corepc_client::bitcoin::blockdata::block::{Header, Version};
    use corepc_client::bitcoin::hashes::Hash;
    use corepc_client::bitcoin::{BlockHash, CompactTarget, TxMerkleNode};
    use petgraph::graph::DiGraph;
    use std::collections::HashMap;
    use std::sync::Arc;
    use tokio::sync::Mutex;

    // The regtest target - each such block carries very little work.
    const EASY_BITS: u32 = 0x207f_ffff;
    // A much harder target (the mainnet genesis "difficulty 1" target). A single
    // block at this target carries far more work than many EASY_BITS blocks.
    const HARD_BITS: u32 = 0x1d00_ffff;

    // Builds a (structurally valid, PoW-invalid) header. The nonce is used to
    // give headers that share a parent hash distinct block hashes.
    fn header(prev: BlockHash, bits: u32, nonce: u32) -> Header {
        Header {
            version: Version::from_consensus(0x2000_0000),
            prev_blockhash: prev,
            merkle_root: TxMerkleNode::all_zeros(),
            time: 1_600_000_000,
            bits: CompactTarget::from_consensus(bits),
            nonce,
        }
    }

    fn hi(height: u64, header: Header) -> HeaderInfo {
        HeaderInfo {
            height,
            header,
            miner: String::new(),
        }
    }

    fn tree_from(headers: &[HeaderInfo]) -> Tree {
        let mut graph: DiGraph<HeaderInfo, bool> = DiGraph::new();
        let mut index: HashMap<BlockHash, _> = HashMap::new();
        for h in headers {
            let idx = graph.add_node(h.clone());
            index.insert(h.header.block_hash(), idx);
        }
        for h in headers {
            let current = index[&h.header.block_hash()];
            if let Some(prev) = index.get(&h.header.prev_blockhash) {
                graph.update_edge(*prev, current, false);
            }
        }
        let tree_info: TreeInfo = (graph, index);
        Arc::new(Mutex::new(tree_info))
    }

    #[tokio::test]
    async fn most_work_chain_wins_keeps_intermediate_stale() {
        // All blocks share the same (easy) work, so the most-work chain is the
        // longest one.
        //   root(0) -> a(1) -> b(2) -> e(3) -> f(4)   [active, cumulative 5w]
        //                   \-> c(2) -> d(3)          [stale, cumulative 4w]
        let root = hi(0, header(BlockHash::all_zeros(), EASY_BITS, 0));
        let a = hi(1, header(root.header.block_hash(), EASY_BITS, 1));
        let b = hi(2, header(a.header.block_hash(), EASY_BITS, 2));
        let e = hi(3, header(b.header.block_hash(), EASY_BITS, 3));
        let f = hi(4, header(e.header.block_hash(), EASY_BITS, 4));
        let c = hi(2, header(a.header.block_hash(), EASY_BITS, 5));
        let d = hi(3, header(c.header.block_hash(), EASY_BITS, 6));
        let tree = tree_from(&[
            root.clone(),
            a.clone(),
            b.clone(),
            e.clone(),
            f.clone(),
            c.clone(),
            d.clone(),
        ]);

        let stale = stale_blocks(&tree, 50).await;

        // Only the fork blocks c and d are stale, highest first. d is the stale
        // tip and c is a non-tip (intermediate) stale block.
        assert_eq!(stale.len(), 2);
        assert_eq!(stale[0].hash, d.header.block_hash().to_string());
        assert_eq!(stale[0].height, 3);
        assert_eq!(stale[1].hash, c.header.block_hash().to_string());
        assert_eq!(stale[1].height, 2);

        let hashes: Vec<String> = stale.iter().map(|s| s.hash.clone()).collect();
        for on_active_chain in [&root, &a, &b, &e, &f] {
            assert!(!hashes.contains(&on_active_chain.header.block_hash().to_string()));
        }

        // header is the hex-encoded 80-byte block header.
        assert_eq!(stale[0].header.len(), 160);
    }

    #[tokio::test]
    async fn picks_most_work_not_most_blocks() {
        // A single high-work block outweighs a longer chain of low-work blocks.
        //   root(0) -> h(1, HARD)                        [active, most work]
        //           \-> s1(1) -> s2(2) -> s3(3) (EASY)   [stale, higher but weaker]
        let root = hi(0, header(BlockHash::all_zeros(), EASY_BITS, 0));
        let h = hi(1, header(root.header.block_hash(), HARD_BITS, 1));
        let s1 = hi(1, header(root.header.block_hash(), EASY_BITS, 2));
        let s2 = hi(2, header(s1.header.block_hash(), EASY_BITS, 3));
        let s3 = hi(3, header(s2.header.block_hash(), EASY_BITS, 4));
        let tree = tree_from(&[root.clone(), h.clone(), s1.clone(), s2.clone(), s3.clone()]);

        let stale = stale_blocks(&tree, 50).await;

        // The three easy blocks are stale even though s3 (height 3) is higher
        // than the active tip h (height 1).
        let hashes: Vec<String> = stale.iter().map(|s| s.hash.clone()).collect();
        assert_eq!(stale.len(), 3);
        assert!(hashes.contains(&s1.header.block_hash().to_string()));
        assert!(hashes.contains(&s2.header.block_hash().to_string()));
        assert!(hashes.contains(&s3.header.block_hash().to_string()));
        assert!(!hashes.contains(&h.header.block_hash().to_string()));
        assert!(!hashes.contains(&root.header.block_hash().to_string()));
    }

    #[tokio::test]
    async fn respects_the_limit_returning_most_recent() {
        // A long main chain (heights 0..=8, cumulative 9w) and a shorter stale
        // branch of 5 blocks (heights 2..=6, cumulative <= 7w) forking off `a`.
        let root = hi(0, header(BlockHash::all_zeros(), EASY_BITS, 0));
        let a = hi(1, header(root.header.block_hash(), EASY_BITS, 1));
        let mut headers = vec![root, a.clone()];

        // main chain continues from `a` up to height 8.
        let mut prev = a.header.block_hash();
        for height in 2..=8u64 {
            let block = hi(height, header(prev, EASY_BITS, 10 + height as u32));
            prev = block.header.block_hash();
            headers.push(block);
        }

        // stale branch of 5 blocks (heights 2..=6) forking off `a`.
        let mut prev = a.header.block_hash();
        for i in 0..5u32 {
            let block = hi(2 + i as u64, header(prev, EASY_BITS, 100 + i));
            prev = block.header.block_hash();
            headers.push(block);
        }
        let tree = tree_from(&headers);

        let stale = stale_blocks(&tree, 3).await;

        // 5 stale blocks exist, but we cap at 3 and return the highest.
        assert_eq!(stale.len(), 3);
        assert_eq!(stale[0].height, 6);
        assert_eq!(stale[1].height, 5);
        assert_eq!(stale[2].height, 4);
    }
}
