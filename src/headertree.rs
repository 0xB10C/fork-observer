use std::collections::BTreeMap;

use petgraph::visit::Dfs;
use petgraph::graph::NodeIndex;

use crate::types::{HeaderInfoJson, Tree};

use log::{warn, info};

pub async fn collapse_tree(tree: &Tree, max_forks: u64) -> Vec<HeaderInfoJson> {
    let tree_locked = tree.lock().await;
    if tree_locked.0.node_count() == 0 {
        warn!("tried to collapse an empty tree!");
        return vec![];
    }

    let mut height_occurences: BTreeMap<u64, usize> = BTreeMap::new();
    for node in tree_locked.0.raw_nodes() {
        let counter = height_occurences.entry(node.weight.height).or_insert(0);
        *counter += 1;
    }
    let active_tip_height: u64 = height_occurences
        .iter()
        .map(|(k, _)| *k)
        .max()
        .expect("we should have at least one height here as we have blocks");

    let mut relevant_heights: Vec<u64> = height_occurences
        .iter()
        .filter(|(_, v)| **v > 1)
        .map(|(k, _)| *k)
        .collect();
    relevant_heights.push(active_tip_height);
    relevant_heights.sort();
    relevant_heights = relevant_heights
        .iter()
        .rev()
        .take(max_forks as usize)
        .rev()
        .cloned()
        .collect();

    // filter out unrelevant (no forks) heights from the header tree
    let mut collapsed_tree = tree_locked.0.filter_map(
        |_, node| {
            let height = node.height;
            for x in -2i64..=1 {
                if relevant_heights.contains(&((height as i64 - x) as u64)) {
                    return Some(node);
                }
            }
            return None;
        },
        |_, edge| Some(edge),
    );

    // in the new collapsed_tree, connect headers that previously a
    // linear chain of headers between them.
    let mut root_indicies: Vec<NodeIndex> = collapsed_tree
        .externals(petgraph::Direction::Incoming)
        .collect();
    // We need this to be sorted by height if we use
    // prev_header_to_connect_to to connect to the last header
    // we saw. We can't assume it's sorted when we add data from
    // mulitple nodes on the same network to the tree.
    root_indicies.sort_by_key(|idx| collapsed_tree[*idx].height);

    let mut prev_header_to_connect_to: Option<NodeIndex> = None;
    for root in root_indicies.iter() {
        if let Some(prev_idx) = prev_header_to_connect_to {
            collapsed_tree.add_edge(prev_idx, *root, &false);
        }
        let mut max_height: u64 = u64::default();
        let mut dfs = Dfs::new(&collapsed_tree, *root);
        while let Some(idx) = dfs.next(&collapsed_tree) {
            let height = collapsed_tree[idx].height;
            if height > max_height {
                max_height = height;
                prev_header_to_connect_to = Some(idx);
            }
        }
    }

    info!(
        "done collapsing tree: roots={}, tips={}",
        collapsed_tree
            .externals(petgraph::Direction::Incoming)
            .count(), // root nodes
        collapsed_tree
            .externals(petgraph::Direction::Outgoing)
            .count(), // tip nodes
    );

    let mut headers: Vec<HeaderInfoJson> = Vec::new();
    for idx in collapsed_tree.node_indices() {
        let prev_nodes = collapsed_tree.neighbors_directed(idx, petgraph::Direction::Incoming);
        let prev_node_index: usize;
        match prev_nodes.clone().count() {
            0 => prev_node_index = usize::MAX,
            1 => {
                prev_node_index = prev_nodes
                    .last()
                    .expect("we should have exactly one previous node")
                    .index()
            }
            _ => panic!("got multiple previous nodes. this should not happen."),
        }
        headers.push(HeaderInfoJson::new(
            collapsed_tree[idx],
            idx.index(),
            prev_node_index,
        ));
    }

    return headers;
}
