use std::collections::BTreeMap;
use std::collections::BTreeSet;

use petgraph::graph::NodeIndex;
use petgraph::visit::{Dfs, EdgeRef};

use crate::types::{Fork, HeaderInfoJson, Tree};

use log::{info, warn};

async fn sorted_interesting_heights(
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
        .rev() // reversing: ascending -> desescending
        .take(max_interesting_heights) // taking the 'last' max_interesting_heights
        .rev() // reversing: desescending -> ascending
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

    // We now have muliple sub header trees. To reconnect them
    // we figure out the starts of these chains (roots) and sort
    // them by height. We can't assume they are sorted when as we
    // added data from mulitple nodes to the tree.

    let mut roots: Vec<NodeIndex> = striped_tree
        .externals(petgraph::Direction::Incoming)
        .collect();

    // We need this to be sorted by height if we use
    // prev_header_to_connect_to to connect to the last header
    // we saw below.
    roots.sort_by_key(|idx| striped_tree[*idx].height);

    let mut prev_header_to_connect_to: Option<NodeIndex> = None;
    for root in roots.iter() {
        // If we have apprev_header_to_connect_to, then connect
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

    info!(
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

    headers
}

// calculate the forks for rss
pub async fn recent_forks(tree: &Tree, how_many: usize) -> Vec<Fork> {
    let tree_locked = tree.lock().await;
    let tree = &tree_locked.0;

    let mut forks: Vec<Fork> = vec![];
    // it could be, that we have mutliple roots. To be safe, do this for all
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
