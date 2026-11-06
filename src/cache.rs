//! The in-memory cache the API is served from.
//!
//! Each network has a `Cache` holding everything the HTTP handlers need, so a
//! request never has to touch the database or a node. The node tasks in `main`
//! push changes in through [`update_cache`].

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

use crate::activity::{ActivityEvent, ActivityEventKind};
use crate::config::{self, BoxedSyncSendNode};
use crate::error;
use crate::headertree;
use crate::types::{
    Cache, Caches, ChainTip, Fork, HeaderInfo, HeaderInfoJson, NodeData, NodeDataJson,
    StaleBlockJson, Tree,
};

use corepc_client::Error::JsonRpc;
use log::{debug, error, warn};
use petgraph::graph::NodeIndex;
use std::fmt;
use tokio::sync::broadcast;
use tokio::time::{interval, Duration};

pub const VERSION_UNKNOWN: &str = "unknown";
pub const MINER_UNKNOWN: &str = "Unknown";
pub const MAX_FORKS_IN_CACHE: usize = 50;
pub const MAX_STALE_BLOCKS: usize = 50;

pub async fn populate_cache(network: &config::Network, tree: &Tree, caches: &Caches) {
    let forks = headertree::recent_forks(tree, MAX_FORKS_IN_CACHE).await;
    let hij = headertree::strip_tree(
        tree,
        network.max_interesting_heights,
        BTreeSet::new(),
        network.countdown.as_ref().map(|c| c.height),
    )
    .await;
    let stale_blocks = headertree::stale_blocks(tree, MAX_STALE_BLOCKS).await;
    {
        let mut locked_caches = caches.lock().await;
        let node_data: NodeData = network
            .nodes
            .iter()
            .map(|n| {
                (
                    n.info().id,
                    NodeDataJson::new(
                        n.info(),
                        &[],                         // no chain tips knows yet
                        VERSION_UNKNOWN.to_string(), // is updated later, when we know it
                        0,                           // timestamp of last block update
                        true, // assume the node is reachable, if it isn't we set it to false after the first getchaintips RPC call anyway
                    ),
                )
            })
            .collect();
        locked_caches.insert(
            network.id,
            Cache {
                header_infos_json: hij.clone(),
                node_data,
                forks,
                stale_blocks,
                block_cache: HashMap::new(),
                recent_miners: vec![],
            },
        );
    }
}

// Find out for which heights we have tips for. These are
// interesting to us - we don't want strip them from the tree.
// This includes tips that aren't from a fork, but rather from
// a stale or stuck node (i.e. not an up-to-date view of the
// blocktree).
pub async fn tip_heights(network_id: u32, caches: &Caches) -> BTreeSet<u64> {
    let mut tip_heights: BTreeSet<u64> = BTreeSet::new();
    {
        let locked_cache = caches.lock().await;
        let this_network = locked_cache
            .get(&network_id)
            .expect("network should already exist in cache");
        let node_infos: NodeData = this_network.node_data.clone();
        for node in node_infos.iter() {
            for tip in node.1.tips.iter() {
                tip_heights.insert(tip.height);
            }
        }
    }
    tip_heights
}

/// Recomputes the stripped header tree, the recent forks and the stale blocks
/// of a network and pushes them into the cache. Call this after new headers
/// were inserted into the tree. `extra_tip_heights` are heights to keep in
/// addition to the tips already in the cache (a caller that just learned about
/// new tips might not have them in the cache yet).
pub async fn update_header_tree_cache(
    network: &config::Network,
    tree: &Tree,
    caches: &Caches,
    extra_tip_heights: impl IntoIterator<Item = u64>,
    cache_changed_tx: &broadcast::Sender<u32>,
) {
    let mut tip_heights: BTreeSet<u64> = tip_heights(network.id, caches).await;
    tip_heights.extend(extra_tip_heights);
    let header_infos_json = headertree::strip_tree(
        tree,
        network.max_interesting_heights,
        tip_heights,
        network.countdown.as_ref().map(|c| c.height),
    )
    .await;
    let forks = headertree::recent_forks(tree, MAX_FORKS_IN_CACHE).await;
    let stale_blocks = headertree::stale_blocks(tree, MAX_STALE_BLOCKS).await;
    update_cache(
        caches,
        network.id,
        CacheUpdate::HeaderTree {
            header_infos_json,
            forks,
            stale_blocks,
        },
        cache_changed_tx,
    )
    .await;
}

#[derive(Debug)]
pub enum CacheUpdate {
    HeaderMiner {
        header_info: HeaderInfo,
    },
    HeaderTree {
        header_infos_json: Vec<HeaderInfoJson>,
        forks: Vec<Fork>,
        stale_blocks: Vec<StaleBlockJson>,
    },
    NodeTips {
        node_id: u32,
        tips: Vec<ChainTip>,
    },
    NodeReachability {
        node_id: u32,
        reachable: bool,
    },
    NodeVersion {
        node_id: u32,
        version: String,
    },
    /// Replaces the node entries injected by a remote fork-observer source:
    /// `removed_node_ids` are dropped from the cache and `nodes` are inserted
    /// (or updated) by id. The poller owns which ids are "its", so it computes
    /// the delta each poll.
    RemoteNodes {
        removed_node_ids: Vec<u32>,
        nodes: Vec<NodeDataJson>,
    },
}

impl fmt::Display for CacheUpdate {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            CacheUpdate::HeaderMiner { header_info } => {
                write!(
                    f,
                    "Setting miner of block {} to miner={}",
                    header_info.header.block_hash(),
                    header_info.miner
                )
            }
            CacheUpdate::HeaderTree {
                header_infos_json, ..
            } => match header_infos_json.last() {
                Some(last) => {
                    write!(
                        f,
                        "Updating headertree with last header hash={} and miner={}",
                        last.hash, last.miner
                    )
                }
                None => {
                    write!(f, "Updating headertree with empty header list")
                }
            },
            CacheUpdate::NodeTips { node_id, .. } => {
                write!(f, "Update tips of node={}", node_id,)
            }
            CacheUpdate::NodeVersion { node_id, version } => {
                write!(f, "Update node={} version={}", node_id, version)
            }
            CacheUpdate::NodeReachability { node_id, reachable } => {
                write!(f, "Setting node {} to reachable={}", node_id, reachable)
            }
            CacheUpdate::RemoteNodes {
                removed_node_ids,
                nodes,
            } => {
                write!(
                    f,
                    "Updating {} remote node(s), removing {} remote node(s)",
                    nodes.len(),
                    removed_node_ids.len()
                )
            }
        }
    }
}

// Sends an activity event to the writer task. This only fails when the writer
// is gone, in which case events are lost anyway - so just log it.
pub fn send_activity(
    activity_tx: &tokio::sync::mpsc::UnboundedSender<ActivityEvent>,
    network_id: u32,
    node_id: u32,
    kind: ActivityEventKind,
) {
    if let Err(e) = activity_tx.send(ActivityEvent::new(network_id, node_id, kind)) {
        warn!("Could not send an activity event into the channel: {}", e);
    }
}

// The active tip height of each of a network's nodes, from the cached node
// data. Nodes that haven't reported any tips yet are absent.
pub async fn active_tip_heights(network_id: u32, caches: &Caches) -> BTreeMap<u32, u64> {
    let locked_cache = caches.lock().await;
    let mut heights: BTreeMap<u32, u64> = BTreeMap::new();
    if let Some(network) = locked_cache.get(&network_id) {
        for (node_id, node_data) in network.node_data.iter() {
            if let Some(tip) = node_data.tips.iter().find(|t| t.status == "active") {
                heights.insert(*node_id, tip.height);
            }
        }
    }
    heights
}

pub async fn is_node_reachable(caches: &Caches, network_id: u32, node_id: u32) -> bool {
    let locked_cache = caches.lock().await;
    locked_cache
        .get(&network_id)
        .expect("this network should be in the caches")
        .node_data
        .get(&node_id)
        .expect("this node should be in the network cache")
        .reachable
}

pub async fn update_cache(
    caches: &Caches,
    network_id: u32,
    update: CacheUpdate,
    cache_changed_tx: &tokio::sync::broadcast::Sender<u32>,
) {
    debug!("updating cache with: {}", update);
    let mut locked_cache = caches.lock().await;
    let network = locked_cache
        .get(&network_id)
        .expect("this network should be in the caches");
    match update {
        CacheUpdate::HeaderMiner { header_info } => {
            let mut old = network.header_infos_json.clone();
            if let Some(index) = old
                .iter()
                .position(|h| h.hash == header_info.header.block_hash().to_string())
            {
                old[index].update_miner(header_info.miner.clone());
            }

            locked_cache.entry(network_id).and_modify(|cache| {
                cache.header_infos_json = old;

                cache.recent_miners.push((
                    header_info.header.block_hash().to_string(),
                    header_info.miner,
                ));
                if cache.recent_miners.len() > 5 {
                    cache.recent_miners.remove(0);
                }
            });
        }
        CacheUpdate::HeaderTree {
            header_infos_json,
            forks,
            stale_blocks,
        } => {
            let mut new_header_infos_map: HashMap<String, HeaderInfoJson> = header_infos_json
                .iter()
                .map(|h| (h.hash.clone(), h.clone()))
                .collect();
            // we might have new miner infos. Make sure to not overwrite headers
            // that already have a miner.
            for (hash, miner) in network.recent_miners.iter() {
                new_header_infos_map.entry(hash.clone()).and_modify(|new| {
                    new.update_miner(miner.clone());
                    debug!(
                        "During CacheUpdate::HeaderTree, updated miner of block {}: {}",
                        hash, miner
                    );
                });
            }

            let stale_hashes: HashSet<String> =
                stale_blocks.iter().map(|b| b.hash.clone()).collect();
            locked_cache.entry(network_id).and_modify(|e| {
                e.header_infos_json = new_header_infos_map.values().cloned().collect();
                e.forks = forks;
                e.stale_blocks = stale_blocks;
                // Drop cached blocks that are no longer in the stale list, so the
                // block cache stays bounded to the last MAX_STALE_BLOCKS blocks.
                e.block_cache
                    .retain(|hash, _| stale_hashes.contains(&hash.to_string()));
            });
        }
        CacheUpdate::NodeTips { node_id, tips } => {
            let min_height = match network.header_infos_json.iter().min_by_key(|h| h.height) {
                Some(header) => header.height,
                None => 0,
            };
            let relevant_tips: Vec<ChainTip> = tips
                .iter()
                .filter(|t| t.height >= min_height)
                .cloned()
                .collect();

            locked_cache.entry(network_id).and_modify(|network| {
                network
                    .node_data
                    .entry(node_id)
                    .and_modify(|e| e.tips(&relevant_tips));
            });
        }
        CacheUpdate::NodeReachability { node_id, reachable } => {
            locked_cache.entry(network_id).and_modify(|network| {
                network
                    .node_data
                    .entry(node_id)
                    .and_modify(|e| e.reachable(reachable));
            });
        }
        CacheUpdate::NodeVersion { node_id, version } => {
            locked_cache.entry(network_id).and_modify(|network| {
                network
                    .node_data
                    .entry(node_id)
                    .and_modify(|e| e.version(version));
            });
        }
        CacheUpdate::RemoteNodes {
            removed_node_ids,
            nodes,
        } => {
            locked_cache.entry(network_id).and_modify(|network| {
                for id in removed_node_ids.iter() {
                    network.node_data.remove(id);
                }
                for node in nodes.into_iter() {
                    network.node_data.insert(node.id, node);
                }
            });
        }
    }

    match cache_changed_tx.send(network_id) {
        Ok(_) => debug!(
            "Sent a cache_changed notification for network={}.",
            network_id,
        ),
        Err(e) => {
            debug!(
                "Could not send cache_changed into the channel for network={}: {}",
                network_id, e
            )
        }
    };
}

pub async fn load_node_version(node: BoxedSyncSendNode, network: &str) -> String {
    // The Bitcoin Core version is requested via the getnetworkinfo RPC. This
    // RPC exposes sensitive information to the caller, so it might not be
    // allowed on the whitelist. We set the version to VERSION_UNKNOWN if we
    // can't request it. As RPC interface might not be up yet, we
    // try to request the version multiple times.
    let mut interval = interval(Duration::from_secs(10));
    for _ in 0..5 {
        match node.version().await {
            Ok(version) => {
                return version;
            }
            Err(e) => match e {
                error::FetchError::BitcoinCoreRPC(JsonRpc(msg)) => {
                    warn!("Could not fetch getnetworkinfo from node='{}' on network '{}': {:?}. Retrying...", node.info().name, network, msg);
                }
                _ => {
                    error!(
                        "Could not load version from node='{}' on network='{}': {:?}",
                        node.info().name,
                        network,
                        e
                    );
                    return VERSION_UNKNOWN.to_string();
                }
            },
        };
        interval.tick().await;
    }
    warn!(
        "Could not load version from node='{}' on network='{}'. Using '{}' as version.",
        node.info().name,
        network,
        VERSION_UNKNOWN
    );
    VERSION_UNKNOWN.to_string()
}

pub async fn insert_new_headers_into_tree(tree: &Tree, new_headers: &[HeaderInfo]) -> bool {
    let mut tree_changed: bool = false;
    let mut tree_locked = tree.lock().await;
    // insert new headers to tree. We first insert all headers we know about
    // and only connect them to parent headers afterwards (see below).
    for h in new_headers {
        if !tree_locked.1.contains_key(&h.header.block_hash()) {
            let idx = tree_locked.0.add_node(h.clone());
            tree_locked.1.insert(h.header.block_hash(), idx);
            tree_changed = true;
        }
    }
    // connect a header with it's parent header by the prev_hash
    for new in new_headers {
        let idx_new: NodeIndex;
        let idx_prev: NodeIndex;
        {
            idx_new = *tree_locked
                    .1
                    .get(&new.header.block_hash())
                    .expect(
                    "the new header should be in the map as we just inserted it or it was already present",
                );
            match tree_locked.1.get(&new.header.prev_blockhash) {
                Some(idx) => idx_prev = *idx,
                None => {
                    continue; // the tree's root has no previous block, skip it
                }
            }
        }
        tree_locked.0.update_edge(idx_prev, idx_new, false);
    }
    tree_changed
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::NodeInfo;
    use crate::types::ChainTipStatus;
    use corepc_client::bitcoin::blockdata::block::{Header, Version};
    use corepc_client::bitcoin::hashes::Hash;
    use corepc_client::bitcoin::{BlockHash, CompactTarget, TxMerkleNode};
    use petgraph::graph::DiGraph;
    use std::sync::Arc;
    use tokio::sync::Mutex;

    async fn get_test_node_reachable(caches: &Caches, net_id: u32, node_id: u32) -> bool {
        let locked_caches = caches.lock().await;
        locked_caches
            .get(&net_id)
            .expect("network id should be there")
            .node_data
            .get(&node_id)
            .expect("node id should be there")
            .reachable
    }

    #[tokio::test]
    async fn test_node_reachable() {
        let network_id: u32 = 0;
        let (dummy_sender, _) = broadcast::channel(2);
        let caches: Caches = Arc::new(Mutex::new(BTreeMap::new()));
        let node = NodeInfo {
            id: 0,
            name: "".to_string(),
            description: "".to_string(),
            implementation: "".to_string(),
        };
        {
            // populate data
            let mut locked_caches = caches.lock().await;
            let mut node_data: NodeData = BTreeMap::new();
            node_data.insert(
                node.id,
                NodeDataJson::new(node.clone(), &[], "".to_string(), 0, true),
            );
            locked_caches.insert(
                network_id,
                Cache {
                    header_infos_json: vec![],
                    node_data,
                    forks: vec![],
                    stale_blocks: vec![],
                    block_cache: HashMap::new(),
                    recent_miners: vec![],
                },
            );
        }
        assert!(get_test_node_reachable(&caches, network_id, node.id).await);

        update_cache(
            &caches,
            network_id,
            CacheUpdate::NodeReachability {
                node_id: node.id,
                reachable: false,
            },
            &dummy_sender,
        )
        .await;
        assert!(!get_test_node_reachable(&caches, network_id, node.id).await);

        update_cache(
            &caches,
            network_id,
            CacheUpdate::NodeReachability {
                node_id: node.id,
                reachable: true,
            },
            &dummy_sender,
        )
        .await;
        assert!(get_test_node_reachable(&caches, network_id, node.id).await);
    }

    #[tokio::test]
    async fn remote_nodes_update_inserts_replaces_and_removes() {
        let network_id: u32 = 0;
        let (dummy_sender, _) = broadcast::channel(2);
        let caches: Caches = Arc::new(Mutex::new(BTreeMap::new()));
        {
            let mut locked_caches = caches.lock().await;
            locked_caches.insert(
                network_id,
                Cache {
                    header_infos_json: vec![],
                    node_data: BTreeMap::new(),
                    forks: vec![],
                    stale_blocks: vec![],
                    block_cache: HashMap::new(),
                    recent_miners: vec![],
                },
            );
        }

        let node_info = |id: u32, name: &str| NodeInfo {
            id,
            name: name.to_string(),
            description: "".to_string(),
            implementation: "".to_string(),
        };

        // Insert two remote nodes.
        update_cache(
            &caches,
            network_id,
            CacheUpdate::RemoteNodes {
                removed_node_ids: vec![],
                nodes: vec![
                    NodeDataJson::new(node_info(1000, "A"), &[], "".to_string(), 0, true),
                    NodeDataJson::new(node_info(1001, "B"), &[], "".to_string(), 0, true),
                ],
            },
            &dummy_sender,
        )
        .await;
        {
            let locked = caches.lock().await;
            let node_data = &locked.get(&network_id).unwrap().node_data;
            assert_eq!(node_data.len(), 2);
            assert!(node_data.contains_key(&1000));
            assert!(node_data.contains_key(&1001));
        }

        // Replace node 1000's data and remove node 1001.
        update_cache(
            &caches,
            network_id,
            CacheUpdate::RemoteNodes {
                removed_node_ids: vec![1001],
                nodes: vec![NodeDataJson::new(
                    node_info(1000, "A renamed"),
                    &[],
                    "".to_string(),
                    0,
                    false,
                )],
            },
            &dummy_sender,
        )
        .await;
        {
            let locked = caches.lock().await;
            let node_data = &locked.get(&network_id).unwrap().node_data;
            assert_eq!(node_data.len(), 1);
            let node = node_data.get(&1000).unwrap();
            assert_eq!(node.name, "A renamed");
            assert!(!node.reachable);
            assert!(!node_data.contains_key(&1001));
        }
    }

    #[tokio::test]
    async fn header_tree_update_prunes_block_cache() {
        use crate::types::StaleBlockJson;
        use std::str::FromStr;

        let network_id: u32 = 0;
        let (dummy_sender, _) = broadcast::channel(2);
        let caches: Caches = Arc::new(Mutex::new(BTreeMap::new()));

        let keep =
            BlockHash::from_str("00000000000000000000000000000000000000000000000000000000000000aa")
                .unwrap();
        let drop_it =
            BlockHash::from_str("00000000000000000000000000000000000000000000000000000000000000bb")
                .unwrap();

        {
            let mut locked = caches.lock().await;
            let mut block_cache = HashMap::new();
            block_cache.insert(keep, Some(vec![1u8, 2, 3]));
            block_cache.insert(drop_it, Some(vec![4u8, 5, 6]));
            locked.insert(
                network_id,
                Cache {
                    header_infos_json: vec![],
                    node_data: BTreeMap::new(),
                    forks: vec![],
                    stale_blocks: vec![],
                    block_cache,
                    recent_miners: vec![],
                },
            );
        }

        // After a header-tree update whose stale list only contains `keep`, the
        // cached block for `drop_it` must be pruned.
        update_cache(
            &caches,
            network_id,
            CacheUpdate::HeaderTree {
                header_infos_json: vec![],
                forks: vec![],
                stale_blocks: vec![StaleBlockJson {
                    height: 1,
                    hash: keep.to_string(),
                    header: "00".repeat(80),
                }],
            },
            &dummy_sender,
        )
        .await;

        let locked = caches.lock().await;
        let block_cache = &locked.get(&network_id).unwrap().block_cache;
        assert!(block_cache.contains_key(&keep));
        assert!(!block_cache.contains_key(&drop_it));
    }

    fn node_info(id: u32, name: &str) -> NodeInfo {
        NodeInfo {
            id,
            name: name.to_string(),
            description: "".to_string(),
            implementation: "".to_string(),
        }
    }

    fn cache_with(header_infos_json: Vec<HeaderInfoJson>, node_data: NodeData) -> Cache {
        Cache {
            header_infos_json,
            node_data,
            forks: vec![],
            stale_blocks: vec![],
            block_cache: HashMap::new(),
            recent_miners: vec![],
        }
    }

    // A structurally valid (PoW-invalid) header. The nonce gives the headers
    // distinct block hashes.
    fn test_header(nonce: u32) -> Header {
        Header {
            version: Version::from_consensus(0x2000_0000),
            prev_blockhash: BlockHash::all_zeros(),
            merkle_root: TxMerkleNode::all_zeros(),
            time: 1_600_000_000,
            bits: CompactTarget::from_consensus(0x207f_ffff),
            nonce,
        }
    }

    fn header_info(height: u64, nonce: u32) -> HeaderInfo {
        HeaderInfo {
            height,
            header: test_header(nonce),
            miner: MINER_UNKNOWN.to_string(),
        }
    }

    fn chain_tip(height: u64, status: ChainTipStatus) -> ChainTip {
        ChainTip {
            height,
            hash: format!("tip at height {}", height),
            branchlen: 0,
            status,
        }
    }

    async fn cached_network(caches: &Caches, network_id: u32, cache: Cache) {
        caches.lock().await.insert(network_id, cache);
    }

    #[tokio::test]
    async fn active_tip_heights_only_reports_the_active_tip() {
        let network_id: u32 = 0;
        let caches: Caches = Arc::new(Mutex::new(BTreeMap::new()));
        let tips = vec![
            chain_tip(100, ChainTipStatus::Active),
            chain_tip(105, ChainTipStatus::ValidFork),
        ];
        let mut node_data: NodeData = BTreeMap::new();
        node_data.insert(
            0,
            NodeDataJson::new(node_info(0, "with tips"), &tips, "".to_string(), 0, true),
        );
        node_data.insert(
            1,
            NodeDataJson::new(node_info(1, "without tips"), &[], "".to_string(), 0, true),
        );
        cached_network(&caches, network_id, cache_with(vec![], node_data)).await;

        let heights = active_tip_heights(network_id, &caches).await;

        // The higher valid-fork tip isn't the node's active tip, and the node
        // without tips isn't in the map at all.
        assert_eq!(heights.len(), 1);
        assert_eq!(heights.get(&0), Some(&100));
    }

    #[tokio::test]
    async fn a_miner_update_only_touches_the_header_it_is_for() {
        let network_id: u32 = 0;
        let (dummy_sender, _) = broadcast::channel(2);
        let caches: Caches = Arc::new(Mutex::new(BTreeMap::new()));
        let mined = header_info(1, 1);
        let other = header_info(1, 2);
        let headers = vec![
            HeaderInfoJson::new(&mined, 0, usize::MAX),
            HeaderInfoJson::new(&other, 1, 0),
        ];
        cached_network(&caches, network_id, cache_with(headers, BTreeMap::new())).await;

        update_cache(
            &caches,
            network_id,
            CacheUpdate::HeaderMiner {
                header_info: HeaderInfo {
                    miner: "Miner A".to_string(),
                    ..mined
                },
            },
            &dummy_sender,
        )
        .await;

        let locked = caches.lock().await;
        let cached = &locked.get(&network_id).unwrap().header_infos_json;
        assert_eq!(cached[0].miner, "Miner A");
        assert_eq!(cached[1].miner, MINER_UNKNOWN);
    }

    #[tokio::test]
    async fn the_cache_remembers_the_last_five_miners() {
        let network_id: u32 = 0;
        let (dummy_sender, _) = broadcast::channel(2);
        let caches: Caches = Arc::new(Mutex::new(BTreeMap::new()));
        cached_network(&caches, network_id, cache_with(vec![], BTreeMap::new())).await;

        for i in 0..7u32 {
            update_cache(
                &caches,
                network_id,
                CacheUpdate::HeaderMiner {
                    header_info: HeaderInfo {
                        miner: format!("Miner {}", i),
                        ..header_info(i as u64, i)
                    },
                },
                &dummy_sender,
            )
            .await;
        }

        let locked = caches.lock().await;
        let miners: Vec<String> = locked
            .get(&network_id)
            .unwrap()
            .recent_miners
            .iter()
            .map(|(_, miner)| miner.clone())
            .collect();
        assert_eq!(
            miners,
            ["Miner 2", "Miner 3", "Miner 4", "Miner 5", "Miner 6"]
        );
    }

    #[tokio::test]
    async fn node_tips_below_the_oldest_cached_header_are_dropped() {
        let network_id: u32 = 0;
        let (dummy_sender, _) = broadcast::channel(2);
        let caches: Caches = Arc::new(Mutex::new(BTreeMap::new()));
        // The oldest header the cache knows about is at height 100, so a tip
        // below that isn't something the header tree could show.
        let oldest = header_info(100, 1);
        let mut node_data: NodeData = BTreeMap::new();
        node_data.insert(
            0,
            NodeDataJson::new(node_info(0, "node"), &[], "".to_string(), 0, true),
        );
        cached_network(
            &caches,
            network_id,
            cache_with(vec![HeaderInfoJson::new(&oldest, 0, usize::MAX)], node_data),
        )
        .await;

        update_cache(
            &caches,
            network_id,
            CacheUpdate::NodeTips {
                node_id: 0,
                tips: vec![
                    chain_tip(99, ChainTipStatus::ValidFork),
                    chain_tip(100, ChainTipStatus::Active),
                ],
            },
            &dummy_sender,
        )
        .await;

        let locked = caches.lock().await;
        let tips = &locked.get(&network_id).unwrap().node_data[&0].tips;
        assert_eq!(tips.len(), 1);
        assert_eq!(tips[0].height, 100);
    }

    #[tokio::test]
    async fn headers_are_inserted_once_and_connected_to_their_parent() {
        let tree: Tree = Arc::new(Mutex::new((DiGraph::new(), HashMap::new())));
        let root = header_info(0, 1);
        let child = HeaderInfo {
            header: Header {
                prev_blockhash: root.header.block_hash(),
                ..test_header(2)
            },
            ..header_info(1, 2)
        };
        let headers = [root, child];

        assert!(insert_new_headers_into_tree(&tree, &headers).await);
        // The same headers again change nothing.
        assert!(!insert_new_headers_into_tree(&tree, &headers).await);

        let locked = tree.lock().await;
        assert_eq!(locked.0.node_count(), 2);
        assert_eq!(locked.0.edge_count(), 1);
    }
}
