//! Support for using another fork-observer instance as a data source.
//!
//! A remote fork-observer (configured per network with a base URL and the id
//! of a network on the remote instance) is polled via its HTTP API. The
//! headers it knows about are merged into the local header tree and its nodes
//! are shown alongside the locally configured nodes.

use std::collections::BTreeSet;
use std::str::FromStr;

use corepc_client::bitcoin::blockdata::block::{Header, Version};
use corepc_client::bitcoin::{BlockHash, CompactTarget, TxMerkleNode};
use log::{debug, error, info, warn};
use tokio::sync::broadcast;
use tokio::task;
use tokio::time::{interval, Duration, MissedTickBehavior};

use crate::config::{Network, RemoteForkObserver};
use crate::db;
use crate::error::FetchError;
use crate::types::{
    Caches, DataJsonResponse, Db, HeaderInfo, HeaderInfoJson, NetworksJsonResponse, NodeDataJson,
    Tree, TreeInfo,
};

/// The remote's data.json can be considerably larger than a node RPC reply,
/// so allow more time than the 8 seconds used for node requests.
const REMOTE_TIMEOUT_SECS: u64 = 30;

/// GETs `url` and deserializes the JSON response body. `minreq` is blocking,
/// so this runs on the blocking pool.
async fn get_json<T: serde::de::DeserializeOwned + Send + 'static>(
    url: String,
) -> Result<T, FetchError> {
    task::spawn_blocking(move || {
        let response = minreq::get(&url).with_timeout(REMOTE_TIMEOUT_SECS).send()?;
        if response.status_code != 200 {
            return Err(FetchError::DataError(format!(
                "HTTP {} response to GET {}",
                response.status_code, url
            )));
        }
        Ok(response.json::<T>()?)
    })
    .await?
}

/// Rebuilds the exact block header from the remote's JSON representation and
/// verifies that it hashes to the block hash the remote reported.
fn header_info_from_json(hij: &HeaderInfoJson) -> Result<HeaderInfo, FetchError> {
    let prev_blockhash = BlockHash::from_str(&hij.prev_blockhash).map_err(|e| {
        FetchError::DataError(format!(
            "invalid prev_blockhash '{}': {}",
            hij.prev_blockhash, e
        ))
    })?;
    let merkle_root = TxMerkleNode::from_str(&hij.merkle_root).map_err(|e| {
        FetchError::DataError(format!("invalid merkle_root '{}': {}", hij.merkle_root, e))
    })?;
    let header = Header {
        // exact round-trip of HeaderInfoJson::new's `to_consensus() as u32`
        version: Version::from_consensus(hij.version as i32),
        prev_blockhash,
        merkle_root,
        time: hij.time,
        bits: CompactTarget::from_consensus(hij.bits),
        nonce: hij.nonce,
    };
    if header.block_hash().to_string() != hij.hash {
        return Err(FetchError::DataError(format!(
            "header hash mismatch: remote claims {} but the header hashes to {}",
            hij.hash,
            header.block_hash()
        )));
    }
    Ok(HeaderInfo {
        height: hij.height,
        header,
        miner: hij.miner.clone(),
    })
}

/// Converts a whole batch of remote headers. Any hash mismatch fails the whole
/// batch: a mismatch means the remote is buggy or malicious and none of its
/// data should be trusted. Headers below the local min_fork_height are dropped.
fn header_infos_from_json(
    hijs: &[HeaderInfoJson],
    min_fork_height: u64,
) -> Result<Vec<HeaderInfo>, FetchError> {
    hijs.iter()
        .filter(|hij| hij.height >= min_fork_height)
        .map(header_info_from_json)
        .collect()
}

/// Inserts the headers we can hang off a block we already have, lowest first,
/// and returns the ones that ended up in the tree.
///
/// The remote serves its *stripped* header tree, which has gaps by design. It
/// does keep the blocks around every fork though, so a fork block it knows and
/// we don't comes with the block it forked off. Going from the lowest height up
/// means a header's parent is already in the tree by the time we get to it -
/// either it was there all along, or it came from this same batch.
///
/// Headers we can't connect are skipped. Inserting them would leave orphan
/// blocks in the tree, and an orphan breaks the assumption the header fetching
/// relies on: that knowing a block means knowing everything below it. A node
/// walking down the chain stops at the first block it knows, so an orphan makes
/// it stop on top of a hole it then never fills - splitting the tree for good
/// and leaving the blocks above the hole looking like they came out of nowhere.
/// We lose nothing by skipping them: without the blocks in between we can't tell
/// where they belong anyway.
fn insert_connectable_headers(
    tree: &mut TreeInfo,
    header_infos: Vec<HeaderInfo>,
) -> Vec<HeaderInfo> {
    let mut by_height = header_infos;
    by_height.sort_by_key(|h| h.height);

    let mut inserted: Vec<HeaderInfo> = Vec::new();
    for header_info in by_height {
        let hash = header_info.header.block_hash();
        if tree.1.contains_key(&hash) {
            continue; // we already have it
        }
        let idx_prev = match tree.1.get(&header_info.header.prev_blockhash) {
            Some(idx) => *idx,
            None => continue, // nothing to hang it off
        };
        let idx = tree.0.add_node(header_info.clone());
        tree.1.insert(hash, idx);
        tree.0.update_edge(idx_prev, idx, false);
        inserted.push(header_info);
    }
    inserted
}

/// Prepares the remote's nodes to be shown alongside the local nodes: node ids
/// are offset to avoid collisions with local node ids and the remote's name is
/// recorded so the frontend can mark where the node came from. Nodes the remote
/// itself imported from a third instance are dropped, see
/// [`NodeDataJson::remote_source`]. The remote-reported
/// tips, version, reachability, and timestamp are kept as-is (e.g. a node
/// unreachable *on the remote* stays marked unreachable here).
fn prepare_remote_nodes(
    remote: &RemoteForkObserver,
    nodes: Vec<NodeDataJson>,
) -> Vec<NodeDataJson> {
    let mut prepared: Vec<NodeDataJson> = Vec::with_capacity(nodes.len());
    for mut node in nodes.into_iter() {
        if node.remote_source.is_some() {
            debug!(
                "Not importing node '{}' from the remote fork-observer '{}': it was itself imported from another remote instance.",
                node.name, remote.name
            );
            continue;
        }
        // The configuration requires node_id_offset to be larger than every
        // local node id in this network, so an offset id can't collide with a
        // local one. It can still overflow for absurdly large remote ids.
        node.id = match node.id.checked_add(remote.node_id_offset) {
            Some(id) => id,
            None => {
                error!(
                    "The id of node '{}' from the remote fork-observer '{}' overflows when adding the node_id_offset {}. Skipping this node.",
                    node.name, remote.name, remote.node_id_offset
                );
                continue;
            }
        };
        node.remote_source = Some(remote.name.clone());
        prepared.push(node);
    }
    prepared
}

#[derive(Default)]
struct PollState {
    /// The (already offset) node ids we injected into the cache last time.
    last_node_ids: BTreeSet<u32>,
    last_nodes: Vec<NodeDataJson>,
    unreachable: bool,
}

async fn poll_once(
    remote: &RemoteForkObserver,
    network: &Network,
    tree: &Tree,
    db: &Db,
    caches: &Caches,
    cache_changed_tx: &broadcast::Sender<u32>,
    state: &mut PollState,
) -> Result<(), FetchError> {
    let data: DataJsonResponse = get_json(format!(
        "{}/api/{}/data.json",
        remote.url, remote.network_id
    ))
    .await?;
    let header_infos = header_infos_from_json(&data.header_infos, network.min_fork_height)?;

    // Skipping the headers we already have also keeps us from re-writing the
    // (unchanged) remote headers to the database on every poll.
    let new_headers: Vec<HeaderInfo> = {
        let mut tree_locked = tree.lock().await;
        insert_connectable_headers(&mut tree_locked, header_infos)
    };

    let tree_changed = !new_headers.is_empty();
    if tree_changed {
        match db::write_to_db(&new_headers, db.clone(), network.id).await {
            Ok(_) => info!(
                "Written {} headers to database for network '{}' from remote fork-observer '{}'",
                new_headers.len(),
                network.name,
                remote.name
            ),
            Err(e) => error!(
                "Could not write new headers for network '{}' from remote fork-observer '{}' to database: {}",
                network.name, remote.name, e
            ),
        }
    }

    let nodes = prepare_remote_nodes(remote, data.nodes);
    let node_ids: BTreeSet<u32> = nodes.iter().map(|n| n.id).collect();
    let removed_node_ids: Vec<u32> = state.last_node_ids.difference(&node_ids).cloned().collect();

    if state.unreachable || nodes != state.last_nodes || !removed_node_ids.is_empty() {
        crate::update_cache(
            caches,
            network.id,
            crate::CacheUpdate::RemoteNodes {
                removed_node_ids,
                nodes: nodes.clone(),
            },
            cache_changed_tx,
        )
        .await;
    }
    state.last_node_ids = node_ids;
    state.last_nodes = nodes;
    state.unreachable = false;

    if tree_changed {
        // No extra tip heights: the remote's tips are already in the cache, as
        // the RemoteNodes update ran above.
        crate::update_header_tree_cache(
            network,
            tree,
            caches,
            std::iter::empty(),
            cache_changed_tx,
        )
        .await;
    }
    Ok(())
}

/// Best-effort check that the remote actually has the configured network. A
/// mismatch or an unreachable remote only logs: the remote might be down or
/// misconfigured right now and fixed later, so we keep polling either way.
async fn check_remote_network(remote: &RemoteForkObserver) {
    match get_json::<NetworksJsonResponse>(format!("{}/api/networks.json", remote.url)).await {
        Ok(response) => {
            if !response.networks.iter().any(|n| n.id == remote.network_id) {
                error!(
                    "The remote fork-observer '{}' ({}) has no network with id={}. Available networks: {}. Polling anyway - the remote might be reconfigured later.",
                    remote.name,
                    remote.url,
                    remote.network_id,
                    response
                        .networks
                        .iter()
                        .map(|n| format!("'{}' (id={})", n.name, n.id))
                        .collect::<Vec<String>>()
                        .join(", ")
                );
            }
        }
        Err(e) => warn!(
            "Could not fetch networks.json from the remote fork-observer '{}' ({}): {}. Polling anyway.",
            remote.name, remote.url, e
        ),
    }
}

pub async fn run_poller(
    remote: RemoteForkObserver,
    network: Network,
    tree: Tree,
    db: Db,
    caches: Caches,
    cache_changed_tx: broadcast::Sender<u32>,
    query_interval: Duration,
) {
    info!(
        "Polling remote fork-observer '{}' ({}, '{}') for network '{}' (id={})",
        remote.name, remote.url, remote.description, network.name, network.id
    );
    check_remote_network(&remote).await;

    let mut state = PollState::default();
    let mut ticker = interval(query_interval);
    // A poll can take longer than query_interval (the remote's data.json is
    // larger than a node RPC reply and the timeout is 30s). The default
    // behavior would then fire the missed ticks back-to-back, hammering a
    // remote that is already slow. Wait a full interval after each poll instead.
    ticker.set_missed_tick_behavior(MissedTickBehavior::Delay);
    loop {
        // The first tick fires immediately, so we poll right after startup.
        ticker.tick().await;
        if let Err(e) = poll_once(
            &remote,
            &network,
            &tree,
            &db,
            &caches,
            &cache_changed_tx,
            &mut state,
        )
        .await
        {
            error!(
                "Could not fetch data from the remote fork-observer '{}' ({}) for network '{}' (id={}): {}",
                remote.name, remote.url, network.name, network.id, e
            );
            // Keep showing the last known data, but mark the injected nodes as
            // unreachable (once - not on every failed poll).
            if !state.unreachable && !state.last_nodes.is_empty() {
                for node in state.last_nodes.iter_mut() {
                    node.reachable = false;
                }
                crate::update_cache(
                    &caches,
                    network.id,
                    crate::CacheUpdate::RemoteNodes {
                        removed_node_ids: vec![],
                        nodes: state.last_nodes.clone(),
                    },
                    &cache_changed_tx,
                )
                .await;
            }
            state.unreachable = true;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::PoolIdentification;
    use crate::types::{Cache, HeaderInfoJson, NetworkJson, NodeDataJson};
    use corepc_client::bitcoin::hashes::Hash;
    use rusqlite::Connection;
    use std::collections::{BTreeMap, BTreeSet, HashMap};
    use std::sync::Arc;
    use tokio::sync::Mutex;

    // A trivially-easy target, same as used in headertree.rs's tests.
    const EASY_BITS: u32 = 0x207f_ffff;

    fn header(prev: BlockHash, nonce: u32) -> Header {
        Header {
            version: Version::from_consensus(0x2000_0000),
            prev_blockhash: prev,
            merkle_root: TxMerkleNode::all_zeros(),
            time: 1_600_000_000,
            bits: CompactTarget::from_consensus(EASY_BITS),
            nonce,
        }
    }

    #[test]
    fn header_info_json_round_trips_and_verifies_hash() {
        let hi = HeaderInfo {
            height: 1,
            header: header(BlockHash::all_zeros(), 42),
            miner: "Some Pool".to_string(),
        };
        let hij = HeaderInfoJson::new(&hi, 0, usize::MAX);

        let parsed = header_info_from_json(&hij).expect("a valid HeaderInfoJson should parse");
        assert_eq!(parsed, hi);
    }

    #[test]
    fn header_info_json_tampering_is_rejected() {
        let hi = HeaderInfo {
            height: 1,
            header: header(BlockHash::all_zeros(), 42),
            miner: "".to_string(),
        };
        let hij = HeaderInfoJson::new(&hi, 0, usize::MAX);

        // Either changing the claimed hash or changing a header field breaks
        // the hash check.
        let mut claimed_hash = hij.clone();
        claimed_hash.hash = BlockHash::all_zeros().to_string();
        let mut header_field = hij.clone();
        header_field.nonce = hij.nonce.wrapping_add(1);

        for tampered in [claimed_hash, header_field] {
            match header_info_from_json(&tampered) {
                Err(FetchError::DataError(_)) => {}
                other => panic!("expected a DataError, got {:?}", other),
            }
        }
    }

    #[test]
    fn header_infos_from_json_drops_headers_below_min_fork_height() {
        let a = HeaderInfo {
            height: 5,
            header: header(BlockHash::all_zeros(), 1),
            miner: "".to_string(),
        };
        let b = HeaderInfo {
            height: 10,
            header: header(a.header.block_hash(), 2),
            miner: "".to_string(),
        };
        let hijs = vec![
            HeaderInfoJson::new(&a, 0, usize::MAX),
            HeaderInfoJson::new(&b, 1, 0),
        ];

        let kept = header_infos_from_json(&hijs, 10).expect("should parse");
        assert_eq!(kept, vec![b]);
    }

    fn hi(height: u64, header: Header) -> HeaderInfo {
        HeaderInfo {
            height,
            header,
            miner: String::new(),
        }
    }

    // A tree holding the given headers, linked up by their prev_blockhash.
    fn tree_of(headers: &[HeaderInfo]) -> TreeInfo {
        let mut graph: petgraph::graph::DiGraph<HeaderInfo, bool> = petgraph::graph::DiGraph::new();
        let mut index: HashMap<BlockHash, _> = HashMap::new();
        for h in headers {
            index.insert(h.header.block_hash(), graph.add_node(h.clone()));
        }
        for h in headers {
            let current = index[&h.header.block_hash()];
            if let Some(prev) = index.get(&h.header.prev_blockhash) {
                graph.update_edge(*prev, current, false);
            }
        }
        (graph, index)
    }

    // A chain of headers at heights 0..=max_height.
    fn chain(max_height: u64) -> Vec<HeaderInfo> {
        let mut headers = vec![hi(0, header(BlockHash::all_zeros(), 0))];
        for height in 1..=max_height {
            let prev = headers.last().unwrap().header.block_hash();
            headers.push(hi(height, header(prev, height as u32)));
        }
        headers
    }

    // The number of blocks in the tree that have no parent in it. The whole point
    // of the filtering is that this stays at one.
    fn root_count(tree: &TreeInfo) -> usize {
        tree.0.externals(petgraph::Direction::Incoming).count()
    }

    // The case the remote import exists for: it knows a fork we don't. Its
    // stripped tree carries the block the fork forked off, so we can place it.
    #[test]
    fn inserts_a_fork_anchored_in_our_tree() {
        let ours = chain(5);
        let mut tree = tree_of(&ours);
        // The remote sends us blocks around the fork at height 4, including the
        // stale block we've never seen.
        let stale = hi(4, header(ours[3].header.block_hash(), 40));
        let remote = vec![ours[2].clone(), ours[3].clone(), stale.clone()];

        let inserted = insert_connectable_headers(&mut tree, remote);

        assert_eq!(inserted, vec![stale.clone()]);
        assert!(tree.1.contains_key(&stale.header.block_hash()));
        assert_eq!(root_count(&tree), 1);
    }

    // A run of blocks the remote knows, of which we have none: the lowest one
    // anchors on a block we have, so the whole run comes along.
    #[test]
    fn inserts_a_whole_run_lowest_first() {
        let full = chain(8);
        let mut tree = tree_of(&full[0..=5]);
        // Handed to us highest first, to show the order doesn't matter.
        let remote = vec![full[8].clone(), full[7].clone(), full[6].clone()];

        let inserted = insert_connectable_headers(&mut tree, remote);

        assert_eq!(
            inserted,
            vec![full[6].clone(), full[7].clone(), full[8].clone()]
        );
        assert_eq!(root_count(&tree), 1);
    }

    // Headers we can't place are skipped rather than left in the tree as orphans.
    #[test]
    fn skips_what_it_cannot_anchor() {
        let full = chain(10);
        let mut tree = tree_of(&full[0..=3]);
        // Blocks 6..=8 are a run, but block 5 (which would anchor it) is missing.
        let remote = vec![full[6].clone(), full[7].clone(), full[8].clone()];

        let inserted = insert_connectable_headers(&mut tree, remote);

        assert!(
            inserted.is_empty(),
            "expected nothing to be inserted, got heights {:?}",
            inserted.iter().map(|h| h.height).collect::<Vec<u64>>()
        );
        assert_eq!(tree.0.node_count(), 4);
        assert_eq!(root_count(&tree), 1);
    }

    // A gap inside the remote's batch only cuts off what's above the gap.
    #[test]
    fn stops_at_a_gap() {
        let full = chain(10);
        let mut tree = tree_of(&full[0..=3]);
        // 4 and 5 connect, 7 and 8 sit above the missing 6.
        let remote = vec![
            full[4].clone(),
            full[5].clone(),
            full[7].clone(),
            full[8].clone(),
        ];

        let inserted = insert_connectable_headers(&mut tree, remote);

        assert_eq!(inserted, vec![full[4].clone(), full[5].clone()]);
        assert_eq!(root_count(&tree), 1);
    }

    // Headers we already have are neither inserted twice nor reported as new, so
    // an unchanged remote response doesn't rewrite the database every poll.
    #[test]
    fn does_not_reinsert_headers_we_already_have() {
        let full = chain(5);
        let mut tree = tree_of(&full);

        let inserted = insert_connectable_headers(&mut tree, full.clone());

        assert!(inserted.is_empty());
        assert_eq!(tree.0.node_count(), full.len());
        assert_eq!(root_count(&tree), 1);
    }

    fn test_remote(node_id_offset: u32) -> RemoteForkObserver {
        RemoteForkObserver {
            name: "remote".to_string(),
            description: "".to_string(),
            url: "http://127.0.0.1:0".to_string(),
            network_id: 1,
            node_id_offset,
        }
    }

    fn test_node(id: u32, name: &str) -> NodeDataJson {
        NodeDataJson::new(
            crate::backend::NodeInfo {
                id,
                name: name.to_string(),
                description: "".to_string(),
                implementation: "test".to_string(),
            },
            &[],
            "".to_string(),
            0,
            true,
        )
    }

    #[test]
    fn prepare_remote_nodes_offsets_and_labels() {
        let remote = test_remote(1000);
        let nodes = vec![test_node(0, "Node A")];
        let prepared = prepare_remote_nodes(&remote, nodes);

        assert_eq!(prepared.len(), 1);
        assert_eq!(prepared[0].id, 1000);
        // The name is left alone - where the node came from is a separate
        // field, so the frontend can show it as its own label.
        assert_eq!(prepared[0].name, "Node A");
        // Marked so a further hop won't re-import it.
        assert_eq!(prepared[0].remote_source.as_deref(), Some("remote"));
        assert_eq!(prepared[0].display_name(), "Node A via remote");
    }

    #[test]
    fn prepare_remote_nodes_does_not_reimport_already_remote_nodes() {
        // A node the remote itself imported from some other instance must not
        // be re-imported - this is what keeps a cycle between two mutually
        // configured instances from accumulating nodes forever.
        let remote = test_remote(1000);
        let mut already_remote = test_node(0, "Node A");
        already_remote.remote_source = Some("somewhere-else".to_string());
        let nodes = vec![already_remote, test_node(1, "Node B")];

        let prepared = prepare_remote_nodes(&remote, nodes);

        assert_eq!(prepared.len(), 1);
        assert_eq!(prepared[0].name, "Node B");
    }

    #[test]
    fn prepare_remote_nodes_skips_offset_overflow() {
        let remote = test_remote(u32::MAX);
        let nodes = vec![test_node(1, "Node A")];
        let prepared = prepare_remote_nodes(&remote, nodes);
        assert!(prepared.is_empty());
    }

    fn test_network(id: u32) -> Network {
        Network {
            id,
            description: String::new(),
            name: format!("net{}", id),
            slug: format!("net{}", id),
            min_fork_height: 0,
            max_interesting_heights: 100,
            nodes: vec![],
            remote_forkobservers: vec![],
            countdown: None,
            pool_identification: PoolIdentification::default(),
            activity_retention_days: None,
            activity_log_node_ids: BTreeSet::new(),
        }
    }

    async fn memory_db() -> Db {
        let conn = Connection::open_in_memory().expect("in-memory sqlite should open");
        let db: Db = Arc::new(Mutex::new(conn));
        db::setup_db(db.clone())
            .await
            .expect("db setup should succeed");
        db
    }

    async fn empty_caches(network_id: u32) -> Caches {
        let mut map = BTreeMap::new();
        map.insert(
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
        Arc::new(Mutex::new(map))
    }

    /// Serves a fixture `networks.json` + `data.json` on an ephemeral local
    /// port, mimicking a real fork-observer instance, and returns its base URL.
    async fn serve_fixture(
        remote_network_id: u32,
        header_infos: Vec<HeaderInfoJson>,
        nodes: Vec<NodeDataJson>,
    ) -> String {
        use warp::Filter;

        // `warp::Filter::map` requires the closure (and thus what it captures)
        // to be `Clone`; the response structs aren't, so capture them via `Arc`.
        let networks_json = Arc::new(NetworksJsonResponse {
            networks: vec![NetworkJson {
                id: remote_network_id,
                name: "remote network".to_string(),
                slug: "remote-network".to_string(),
                description: "".to_string(),
            }],
        });
        let data_json = Arc::new(DataJsonResponse {
            header_infos,
            nodes,
            countdown: None,
        });

        let networks_route = warp::path!("api" / "networks.json")
            .map(move || warp::reply::json(networks_json.as_ref()));
        let data_route = warp::path!("api" / u32 / "data.json")
            .map(move |_id: u32| warp::reply::json(data_json.as_ref()));
        let routes = networks_route.or(data_route);

        let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
            .await
            .expect("binding an ephemeral local port should succeed");
        let addr = listener
            .local_addr()
            .expect("a bound listener should have a local address");
        tokio::spawn(warp::serve(routes).incoming(listener).run());
        format!("http://{}", addr)
    }

    #[tokio::test]
    async fn poll_once_merges_remote_headers_and_nodes() {
        let remote_network_id = 7;
        let root = HeaderInfo {
            height: 0,
            header: header(BlockHash::all_zeros(), 0),
            miner: "".to_string(),
        };
        let child = HeaderInfo {
            height: 1,
            header: header(root.header.block_hash(), 1),
            miner: "Remote Pool".to_string(),
        };
        let header_infos = vec![
            HeaderInfoJson::new(&root, 0, usize::MAX),
            HeaderInfoJson::new(&child, 1, 0),
        ];
        let remote_nodes = vec![test_node(0, "Remote Node")];

        let base_url = serve_fixture(remote_network_id, header_infos, remote_nodes).await;
        let mut remote = test_remote(1000);
        remote.url = base_url;
        remote.network_id = remote_network_id;

        let network = test_network(1);
        // We already know `root` (a local node fetched it), which is what lets us
        // place `child`. Headers we can't anchor aren't imported.
        let tree: Tree = Arc::new(Mutex::new(tree_of(std::slice::from_ref(&root))));
        let db = memory_db().await;
        let caches = empty_caches(network.id).await;
        let (cache_changed_tx, mut cache_changed_rx) = broadcast::channel(16);
        let mut state = PollState::default();

        poll_once(
            &remote,
            &network,
            &tree,
            &db,
            &caches,
            &cache_changed_tx,
            &mut state,
        )
        .await
        .expect("poll_once should succeed against the fixture server");

        // The remote's header landed in the tree, connected to what we had.
        {
            let tree_locked = tree.lock().await;
            assert!(tree_locked.1.contains_key(&child.header.block_hash()));
            assert_eq!(
                tree_locked
                    .0
                    .externals(petgraph::Direction::Incoming)
                    .count(),
                1,
                "the tree should stay a single connected tree"
            );
        }

        // ... and was persisted under the local network id.
        let restored = db::load_treeinfos(db.clone(), network.id)
            .await
            .expect("tree should reload from the database");
        assert_eq!(restored.1.len(), 1);

        // The remote node shows up with an offset id and its source instance.
        {
            let locked_caches = caches.lock().await;
            let node_data = &locked_caches.get(&network.id).unwrap().node_data;
            assert_eq!(node_data.len(), 1);
            let node = node_data.get(&1000).expect("offset id 1000 should exist");
            assert_eq!(node.name, "Remote Node");
            assert_eq!(node.remote_source.as_deref(), Some("remote"));
        }

        // A cache-changed notification fired.
        assert!(cache_changed_rx.try_recv().is_ok());
        // Drain any further queued notifications from this first poll.
        while cache_changed_rx.try_recv().is_ok() {}

        // Polling again with identical fixture data changes nothing, so no
        // further notification should be sent.
        poll_once(
            &remote,
            &network,
            &tree,
            &db,
            &caches,
            &cache_changed_tx,
            &mut state,
        )
        .await
        .expect("second poll_once should also succeed");
        assert!(cache_changed_rx.try_recv().is_err());
    }
}
