//! The in-memory cache the API is served from.
//!
//! Each network has a `Cache` holding everything the HTTP handlers need, so a
//! request never has to touch the database or a node. The node tasks in `main`
//! push changes in through [`update_cache`].

use std::collections::{BTreeMap, BTreeSet, HashSet};

use crate::activity::{ActivityEvent, ActivityEventKind};
use crate::config::{self, BoxedSyncSendNode};
use crate::error;
use crate::headertree;
use crate::types::{
    caches_from, Cache, Caches, ChainTip, Fork, HeaderInfo, HeaderInfoJson, NodeData, NodeDataJson,
    StaleBlockJson, Tree,
};

use corepc_client::client_sync::Error::JsonRpc;
use log::{debug, error, warn};
use petgraph::graph::NodeIndex;
use std::fmt;
use tokio::sync::{broadcast, RwLock};
use tokio::time::{interval, Duration};

pub const VERSION_UNKNOWN: &str = "unknown";
pub const MINER_UNKNOWN: &str = "Unknown";
pub const MAX_FORKS_IN_CACHE: usize = 50;
pub const MAX_STALE_BLOCKS: usize = 50;

/// The initial cache of a network, built from what we loaded from the database.
///
/// Networks come from the configuration and never appear or disappear at
/// runtime, so every network's cache is built once at startup and the map of
/// them is then shared without a lock of its own.
pub async fn build_cache(network: &config::Network, tree: &Tree) -> Cache {
    let forks = headertree::recent_forks(tree, MAX_FORKS_IN_CACHE).await;
    let hij = headertree::strip_tree(
        tree,
        network.max_interesting_heights,
        BTreeSet::new(),
        network.countdown.as_ref().map(|c| c.height),
    )
    .await;
    let stale_blocks = headertree::stale_blocks(tree, MAX_STALE_BLOCKS).await;
    let node_data: NodeData = network
        .nodes
        .iter()
        .cloned()
        .map(|n| {
            (
                n.info().id,
                NodeDataJson::new(
                    n.info(),
                    &vec![],                     // no chain tips knows yet
                    VERSION_UNKNOWN.to_string(), // is updated later, when we know it
                    0,                           // timestamp of last block update
                    true, // assume the node is reachable, if it isn't we set it to false after the first getchaintips RPC call anyway
                ),
            )
        })
        .collect();
    Cache::new(
        hij,
        node_data,
        forks,
        stale_blocks,
        network.countdown.clone(),
    )
}

/// The caches of all configured networks, ready to be shared with the API and
/// the node tasks.
pub async fn build_caches(networks: &[(config::Network, Tree)]) -> Caches {
    let mut caches: Vec<(u32, Cache)> = vec![];
    for (network, tree) in networks.iter() {
        caches.push((network.id, build_cache(network, tree).await));
    }
    caches_from(caches)
}

/// The cache of a network, or a panic if it isn't one we know: the map is built
/// from the configuration at startup, so every id we look up internally is in
/// it.
fn network_cache(caches: &Caches, network_id: u32) -> &RwLock<Cache> {
    caches
        .get(&network_id)
        .expect("this network should be in the caches")
}

// Find out for which heights we have tips for. These are
// interesting to us - we don't want strip them from the tree.
// This includes tips that aren't from a fork, but rather from
// a stale or stuck node (i.e. not an up-to-date view of the
// blocktree).
pub async fn tip_heights(network_id: u32, caches: &Caches) -> BTreeSet<u64> {
    let mut tip_heights: BTreeSet<u64> = BTreeSet::new();
    let cache = network_cache(caches, network_id).read().await;
    for node in cache.node_data.iter() {
        for tip in node.1.tips.iter() {
            tip_heights.insert(tip.height);
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

impl CacheUpdate {
    /// Whether applying this can change what `stale.json` holds.
    ///
    /// Only the header tree carries stale blocks. Everything else replaces node
    /// data or one block's miner, neither of which `stale.json` contains, so
    /// rebuilding it for those would serialize the same bytes again under the
    /// write lock. This lives next to the variants so that adding one makes the
    /// question unavoidable.
    fn changes_stale_blocks(&self) -> bool {
        match self {
            CacheUpdate::HeaderTree { .. } => true,
            CacheUpdate::HeaderMiner { .. }
            | CacheUpdate::NodeTips { .. }
            | CacheUpdate::NodeReachability { .. }
            | CacheUpdate::NodeVersion { .. }
            | CacheUpdate::RemoteNodes { .. } => false,
        }
    }
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
    let cache = network_cache(caches, network_id).read().await;
    let mut heights: BTreeMap<u32, u64> = BTreeMap::new();
    for (node_id, node_data) in cache.node_data.iter() {
        if let Some(tip) = node_data.tips.iter().find(|t| t.status == "active") {
            heights.insert(*node_id, tip.height);
        }
    }
    heights
}

pub async fn is_node_reachable(caches: &Caches, network_id: u32, node_id: u32) -> bool {
    network_cache(caches, network_id)
        .read()
        .await
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
    let changes_stale_blocks = update.changes_stale_blocks();
    {
        let mut cache = network_cache(caches, network_id).write().await;
        match update {
            CacheUpdate::HeaderMiner { header_info } => {
                let hash = header_info.header.block_hash().to_string();
                if let Some(header) = cache.header_infos_json.iter_mut().find(|h| h.hash == hash) {
                    header.update_miner(header_info.miner.clone());
                }

                cache.recent_miners.push((hash, header_info.miner));
                if cache.recent_miners.len() > 5 {
                    cache.recent_miners.remove(0);
                }
            }
            CacheUpdate::HeaderTree {
                mut header_infos_json,
                forks,
                stale_blocks,
            } => {
                // Stripping the tree runs in parallel with identifying miners, so
                // the header list we just got might not have miners we learned
                // about in the meantime. Fill those in, without overwriting a miner
                // the new list already has.
                for header in header_infos_json.iter_mut() {
                    if let Some((_, miner)) = cache
                        .recent_miners
                        .iter()
                        .find(|(hash, _)| *hash == header.hash)
                    {
                        debug!(
                            "During CacheUpdate::HeaderTree, updated miner of block {}: {}",
                            header.hash, miner
                        );
                        header.update_miner(miner.clone());
                    }
                }

                let stale_hashes: HashSet<String> =
                    stale_blocks.iter().map(|b| b.hash.clone()).collect();
                cache.header_infos_json = header_infos_json;
                cache.forks = forks;
                cache.stale_blocks = stale_blocks;
                // Drop cached blocks that are no longer in the stale list, so the
                // block cache stays bounded to the last MAX_STALE_BLOCKS blocks.
                cache
                    .block_cache
                    .retain(|hash, _| stale_hashes.contains(&hash.to_string()));
            }
            CacheUpdate::NodeTips { node_id, tips } => {
                let min_height = match cache.header_infos_json.iter().min_by_key(|h| h.height) {
                    Some(header) => header.height,
                    None => 0,
                };
                let relevant_tips: Vec<ChainTip> = tips
                    .iter()
                    .filter(|t| t.height >= min_height)
                    .cloned()
                    .collect();

                cache
                    .node_data
                    .entry(node_id)
                    .and_modify(|e| e.tips(&relevant_tips));
            }
            CacheUpdate::NodeReachability { node_id, reachable } => {
                cache
                    .node_data
                    .entry(node_id)
                    .and_modify(|e| e.reachable(reachable));
            }
            CacheUpdate::NodeVersion { node_id, version } => {
                cache
                    .node_data
                    .entry(node_id)
                    .and_modify(|e| e.version(version));
            }
            CacheUpdate::RemoteNodes {
                removed_node_ids,
                nodes,
            } => {
                for id in removed_node_ids.iter() {
                    cache.node_data.remove(id);
                }
                for node in nodes.into_iter() {
                    cache.node_data.insert(node.id, node);
                }
            }
        }

        // Every update above changes something `data.json` contains, so it is
        // rebuilt once here rather than once per request. This is the only
        // place that serializes it. `stale.json` only changes with the header
        // tree, and rebuilding it costs as much as serializing it, so the
        // updates that can't affect it don't pay for it.
        cache.rebuild_data_json();
        if changes_stale_blocks {
            cache.rebuild_stale_json();
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
    return VERSION_UNKNOWN.to_string();
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
    use corepc_client::bitcoin::BlockHash;

    async fn get_test_node_reachable(caches: &Caches, net_id: u32, node_id: u32) -> bool {
        caches
            .get(&net_id)
            .expect("network id should be there")
            .read()
            .await
            .node_data
            .get(&node_id)
            .expect("node id should be there")
            .reachable
    }

    #[tokio::test]
    async fn test_node_reachable() {
        let network_id: u32 = 0;
        let (dummy_sender, _) = broadcast::channel(2);
        let node = NodeInfo {
            id: 0,
            name: "".to_string(),
            description: "".to_string(),
            implementation: "".to_string(),
        };
        let mut node_data: NodeData = BTreeMap::new();
        node_data.insert(
            node.id,
            NodeDataJson::new(node.clone(), &vec![], "".to_string(), 0, true),
        );
        let caches = caches_from([(
            network_id,
            Cache::new(vec![], node_data, vec![], vec![], None),
        )]);
        assert_eq!(
            get_test_node_reachable(&caches, network_id, node.id).await,
            true
        );

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
        assert_eq!(
            get_test_node_reachable(&caches, network_id, node.id).await,
            false
        );

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
        assert_eq!(
            get_test_node_reachable(&caches, network_id, node.id).await,
            true
        );
    }

    #[tokio::test]
    async fn remote_nodes_update_inserts_replaces_and_removes() {
        let network_id: u32 = 0;
        let (dummy_sender, _) = broadcast::channel(2);
        let caches = caches_from([(
            network_id,
            Cache::new(vec![], BTreeMap::new(), vec![], vec![], None),
        )]);

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
                    NodeDataJson::new(node_info(1000, "A"), &vec![], "".to_string(), 0, true),
                    NodeDataJson::new(node_info(1001, "B"), &vec![], "".to_string(), 0, true),
                ],
            },
            &dummy_sender,
        )
        .await;
        {
            let cache = caches.get(&network_id).unwrap().read().await;
            let node_data = &cache.node_data;
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
                    &vec![],
                    "".to_string(),
                    0,
                    false,
                )],
            },
            &dummy_sender,
        )
        .await;
        {
            let cache = caches.get(&network_id).unwrap().read().await;
            let node_data = &cache.node_data;
            assert_eq!(node_data.len(), 1);
            let node = node_data.get(&1000).unwrap();
            assert_eq!(node.name, "A renamed");
            assert_eq!(node.reachable, false);
            assert!(!node_data.contains_key(&1001));
        }
    }

    #[tokio::test]
    async fn header_tree_update_prunes_block_cache() {
        use crate::types::StaleBlockJson;
        use std::str::FromStr;

        let network_id: u32 = 0;
        let (dummy_sender, _) = broadcast::channel(2);

        let keep =
            BlockHash::from_str("00000000000000000000000000000000000000000000000000000000000000aa")
                .unwrap();
        let drop_it =
            BlockHash::from_str("00000000000000000000000000000000000000000000000000000000000000bb")
                .unwrap();

        let caches = caches_from([(network_id, {
            let mut cache = Cache::new(vec![], BTreeMap::new(), vec![], vec![], None);
            cache.block_cache.insert(keep, Some(vec![1u8, 2, 3]));
            cache.block_cache.insert(drop_it, Some(vec![4u8, 5, 6]));
            cache
        })]);

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

        let cache = caches.get(&network_id).unwrap().read().await;
        let block_cache = &cache.block_cache;
        assert!(block_cache.contains_key(&keep));
        assert!(!block_cache.contains_key(&drop_it));
    }

    // `stale.json` is only rebuilt by the updates that can change it, so what
    // matters is that those really are the only ones that do.
    #[tokio::test]
    async fn stale_json_follows_the_header_tree_and_nothing_else() {
        use crate::types::StaleBlockJson;

        let network_id: u32 = 0;
        let (dummy_sender, _) = broadcast::channel(2);
        let node = NodeInfo {
            id: 7,
            name: "".to_string(),
            description: "".to_string(),
            implementation: "".to_string(),
        };
        let node_data: NodeData = BTreeMap::from([(
            node.id,
            NodeDataJson::new(node, &vec![], "".to_string(), 0, true),
        )]);
        let caches = caches_from([(
            network_id,
            Cache::new(vec![], node_data, vec![], vec![], None),
        )]);

        let stale_json = |caches: Caches| async move {
            caches
                .get(&network_id)
                .unwrap()
                .read()
                .await
                .stale_json
                .clone()
        };
        let empty = stale_json(caches.clone()).await;

        // A header tree carrying a stale block has to show up.
        update_cache(
            &caches,
            network_id,
            CacheUpdate::HeaderTree {
                header_infos_json: vec![],
                forks: vec![],
                stale_blocks: vec![StaleBlockJson {
                    height: 1,
                    hash: "aa".to_string(),
                    header: "00".repeat(80),
                }],
            },
            &dummy_sender,
        )
        .await;
        let with_stale = stale_json(caches.clone()).await;
        assert_ne!(with_stale, empty);
        assert!(String::from_utf8_lossy(&with_stale).contains("\"aa\""));

        // Updates that don't carry stale blocks must leave it alone - neither
        // clearing it nor letting it go stale.
        for update in [
            CacheUpdate::NodeTips {
                node_id: 7,
                tips: vec![],
            },
            CacheUpdate::NodeReachability {
                node_id: 7,
                reachable: false,
            },
            CacheUpdate::NodeVersion {
                node_id: 7,
                version: "v2".to_string(),
            },
            CacheUpdate::RemoteNodes {
                removed_node_ids: vec![],
                nodes: vec![],
            },
        ] {
            update_cache(&caches, network_id, update, &dummy_sender).await;
            assert_eq!(stale_json(caches.clone()).await, with_stale);
        }
    }

    fn test_header(height: u64, miner: &str) -> HeaderInfoJson {
        HeaderInfoJson {
            id: height as usize,
            prev_id: height.saturating_sub(1) as usize,
            height,
            hash: format!("{:064x}", height),
            version: 1,
            prev_blockhash: format!("{:064x}", height.saturating_sub(1)),
            merkle_root: format!("{:064x}", height),
            time: 0,
            bits: 0,
            difficulty_int: 0,
            nonce: 0,
            miner: miner.to_string(),
        }
    }

    async fn cached_headers(caches: &Caches, network_id: u32) -> Vec<HeaderInfoJson> {
        caches
            .get(&network_id)
            .unwrap()
            .read()
            .await
            .header_infos_json
            .clone()
    }

    #[tokio::test]
    async fn header_tree_update_keeps_the_header_order() {
        let network_id: u32 = 0;
        let (dummy_sender, _) = broadcast::channel(2);
        let caches = caches_from([(
            network_id,
            Cache::new(vec![], BTreeMap::new(), vec![], vec![], None),
        )]);

        let headers: Vec<HeaderInfoJson> = (0..50).map(|h| test_header(h, "Unknown")).collect();

        // The order the header tree was stripped in has to survive an update,
        // and it has to be the same on every update: an unchanged tree must
        // produce an unchanged header list.
        for _ in 0..5 {
            update_cache(
                &caches,
                network_id,
                CacheUpdate::HeaderTree {
                    header_infos_json: headers.clone(),
                    forks: vec![],
                    stale_blocks: vec![],
                },
                &dummy_sender,
            )
            .await;
            assert_eq!(cached_headers(&caches, network_id).await, headers);
        }
    }

    #[tokio::test]
    async fn header_tree_update_fills_in_recently_identified_miners() {
        let network_id: u32 = 0;
        let (dummy_sender, _) = broadcast::channel(2);
        let caches = caches_from([(network_id, {
            let mut cache = Cache::new(vec![], BTreeMap::new(), vec![], vec![], None);
            // a miner identified while the tree was being stripped
            cache.recent_miners = vec![(format!("{:064x}", 2), "Some Pool".to_string())];
            cache
        })]);

        update_cache(
            &caches,
            network_id,
            CacheUpdate::HeaderTree {
                header_infos_json: (0..5).map(|h| test_header(h, "Unknown")).collect(),
                forks: vec![],
                stale_blocks: vec![],
            },
            &dummy_sender,
        )
        .await;

        let cached = cached_headers(&caches, network_id).await;
        assert_eq!(cached[2].miner, "Some Pool");
        assert_eq!(cached[1].miner, "Unknown");
        // and the order is still the one it was given
        assert_eq!(
            cached.iter().map(|h| h.height).collect::<Vec<u64>>(),
            vec![0, 1, 2, 3, 4]
        );
    }
}
