#![cfg_attr(feature = "strict", deny(warnings))]

use bitcoin_pool_identification::{default_data, PoolIdentification};
use bitcoincore_rpc::bitcoin::{BlockHash, Network};
use bitcoincore_rpc::Error::JsonRpc;
use env_logger::Env;
use futures_util::StreamExt;
use log::{debug, error, info, warn};
use petgraph::graph::NodeIndex;
use rusqlite::Connection;
use std::cmp::max;
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::fmt;
use std::sync::Arc;
use tokio::sync::mpsc::unbounded_channel;
use tokio::sync::{broadcast, Mutex};
use tokio::task;
use tokio::time::{interval, interval_at, sleep, Duration, Instant};
use tokio_stream::wrappers::BroadcastStream;
use warp::Filter;

mod api;
mod config;
mod db;
mod error;
mod headertree;
mod jsonrpc;
mod node;
mod rss;
mod types;

use crate::config::BoxedSyncSendNode;
use crate::error::{DbError, MainError};
use types::{
    Cache, Caches, ChainTip, Db, Fork, HeaderInfo, HeaderInfoJson, NetworkJson, NodeData,
    NodeDataJson, Tree,
};

const VERSION_UNKNOWN: &str = "unknown";
const MINER_UNKNOWN: &str = "Unknown";
const MAX_FORKS_IN_CACHE: usize = 50;

async fn startup() -> Result<(config::Config, Db, Caches), MainError> {
    let config: config::Config = match config::load_config() {
        Ok(config) => {
            info!("Configuration loaded");
            config
        }
        Err(e) => {
            error!("Could not load the configuration: {}", e);
            return Err(e.into());
        }
    };

    let connection = match Connection::open(config.database_path.clone()) {
        Ok(db) => {
            info!("Opened database: {:?}", config.database_path);
            db
        }
        Err(e) => {
            error!(
                "Could not open the database {:?}: {}",
                config.database_path, e
            );
            return Err(DbError::from(e).into());
        }
    };

    let db: Db = Arc::new(Mutex::new(connection));
    let caches: Caches = Arc::new(Mutex::new(BTreeMap::new()));

    match db::setup_db(db.clone()).await {
        Ok(_) => info!("Database setup successful"),
        Err(e) => {
            error!(
                "Could not setup the database {:?}: {}",
                config.database_path, e
            );
            return Err(e.into());
        }
    };
    Ok((config, db, caches))
}

async fn populate_cache(network: &config::Network, tree: &Tree, caches: &Caches) {
    let forks = headertree::recent_forks(&tree, MAX_FORKS_IN_CACHE).await;
    let hij = headertree::strip_tree(&tree, network.max_interesting_heights, BTreeSet::new()).await;
    {
        let mut locked_caches = caches.lock().await;
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
        locked_caches.insert(
            network.id,
            Cache {
                header_infos_json: hij.clone(),
                node_data,
                forks,
                recent_miners: vec![],
            },
        );
    }
}

#[tokio::main]
async fn main() -> Result<(), MainError> {
    env_logger::Builder::from_env(Env::default().default_filter_or("info")).init();
    let (config, db, caches) = startup().await?;

    // A channel to notify about tip changes via ServerSentEvents to clients.
    let (tipchanges_tx, _) = broadcast::channel(16);
    let network_infos: Vec<NetworkJson> = config.networks.iter().map(NetworkJson::new).collect();
    let db_clone = db.clone();

    for network in config.networks.iter().cloned() {
        let network = network.clone();
        let (pool_id_tx, mut pool_id_rx) = unbounded_channel::<BlockHash>();

        info!(
            "network '{}' (id={}) has {} nodes",
            network.name,
            network.id,
            network.nodes.len()
        );

        let tree: Tree = Arc::new(Mutex::new(
            match db::load_treeinfos(db_clone.clone(), network.id).await {
                Ok(tree) => tree,
                Err(e) => {
                    error!(
                        "Could not load tree_infos (headers) from the database {:?}: {}",
                        config.database_path, e
                    );
                    return Err(e.into());
                }
            },
        ));

        populate_cache(&network, &tree, &caches).await;

        for node in network.nodes.iter().cloned() {
            let network = network.clone();
            // Spread query times equally apart to even out network/CPU load
            let mut interval = interval_at(
                Instant::now()
                    + Duration::from_millis(
                        (config.query_interval.as_millis() / network.nodes.len() as u128) as u64,
                    )
                    + Duration::from_secs((network.id % 10) as u64),
                config.query_interval,
            );
            let db_write = db.clone();
            let tree_clone = tree.clone();
            let caches_clone = caches.clone();
            let tipchanges_tx_cloned = tipchanges_tx.clone();
            let pool_id_tx_clone = pool_id_tx.clone();

            let mut last_tips: Vec<ChainTip> = vec![];
            task::spawn(async move {
                // Try to load the node version an update the cache with it.
                update_cache(
                    &caches_clone,
                    network.id,
                    CacheUpdate::NodeVersion {
                        node_id: node.info().id,
                        version: load_node_version(node.clone(), &network.name).await,
                    },
                )
                .await;

                loop {
                    // We specifically wait at the beginning of the loop, as we
                    // are using 'continue' on errors. If we would wait at the end,
                    // we might skip the waiting.
                    interval.tick().await;
                    let tips = match node.tips().await {
                        Ok(tips) => {
                            if !is_node_reachable(&caches_clone, network.id, node.info().id).await {
                                update_cache(
                                    &caches_clone,
                                    network.id,
                                    CacheUpdate::NodeReachability {
                                        node_id: node.info().id,
                                        reachable: true,
                                    },
                                )
                                .await;
                            }
                            tips
                        }
                        Err(e) => {
                            error!(
                                "Could not fetch chaintips from {} on network '{}' (id={}): {:?}",
                                node.info(),
                                network.name,
                                network.id,
                                e
                            );
                            if is_node_reachable(&caches_clone, network.id, node.info().id).await {
                                update_cache(
                                    &caches_clone,
                                    network.id,
                                    CacheUpdate::NodeReachability {
                                        node_id: node.info().id,
                                        reachable: false,
                                    },
                                )
                                .await;
                            }
                            continue;
                        }
                    };

                    if last_tips != tips {
                        let (new_headers, miners_needed): (Vec<HeaderInfo>, Vec<BlockHash>) =
                            match node
                                .new_headers(&tips, &tree_clone, network.min_fork_height)
                                .await
                            {
                                Ok(headers) => headers,
                                Err(e) => {
                                    error!(
                                    "Could not fetch headers from {} on network '{}' (id={}): {}",
                                    node.info(),
                                    network.name,
                                    network.id,
                                    e
                                );
                                    continue;
                                }
                            };

                        // Identify the miner of the new header(s)
                        for hash in miners_needed.iter() {
                            if let Err(e) = pool_id_tx_clone.send(*hash) {
                                error!(
                                    "Could not send a block hash into the pool identification channel: {}",
                                    e
                                );
                            }
                        }

                        last_tips = tips.clone();
                        let db_write = db_write.clone();
                        // We want to avoid stripping the tree (strip_tree()) if it didn't change.
                        // Keeping tracking of changes:
                        let mut tree_changed = false;
                        if !new_headers.is_empty() {
                            tree_changed =
                                insert_new_headers_into_tree(&tree_clone, &new_headers).await;

                            match db::write_to_db(&new_headers, db_write, network.id).await {
                                Ok(_) => info!(
                                    "Written {} headers to database for network '{}' by node {}",
                                    new_headers.len(),
                                    network.name,
                                    node.info()
                                ),
                                Err(e) => {
                                    error!("Could not write new headers for network '{}' by node {} to database: {}", network.name, node.info(), e);
                                    return MainError::Db(e);
                                }
                            }
                        }

                        // Update node tips in cache
                        update_cache(
                            &caches_clone,
                            network.id,
                            CacheUpdate::NodeTips {
                                node_id: node.info().id,
                                tips: tips.clone(),
                            },
                        )
                        .await;

                        if tree_changed {
                            let mut tip_heights: BTreeSet<u64> =
                                tip_heights(network.id, &caches_clone).await;
                            for tip in tips.iter() {
                                tip_heights.insert(tip.height);
                            }
                            let header_infos_json = headertree::strip_tree(
                                &tree_clone,
                                network.max_interesting_heights,
                                tip_heights,
                            )
                            .await;
                            let forks =
                                headertree::recent_forks(&tree_clone, MAX_FORKS_IN_CACHE).await;

                            update_cache(
                                &caches_clone,
                                network.id,
                                CacheUpdate::HeaderTree {
                                    header_infos_json,
                                    forks,
                                },
                            )
                            .await;

                            match tipchanges_tx_cloned.clone().send(network.id) {
                                Ok(_) => debug!("Sent a tip_changed notification."),
                                Err(e) => {
                                    debug!(
                                        "Could not send tip_changed update into the channel: {}",
                                        e
                                    )
                                }
                            };
                        }
                    }
                }
            });
        }

        // A one-shot thread trying to identify all unidentified miners. This
        // runs once after startup (with a 5 minutes delay to be sure nodes
        // are ready and the headertree is loaded).
        let tree_clone = tree.clone();
        let caches_clone = caches.clone();
        let network_clone = network.clone();
        let pool_id_tx_clone = pool_id_tx.clone();
        task::spawn(async move {
            sleep(Duration::from_secs(5 * 60)).await;

            let tip_heights: BTreeSet<u64> = tip_heights(network_clone.id, &caches_clone).await;
            let interesting_heights = headertree::sorted_interesting_heights(
                &tree_clone,
                network_clone.max_interesting_heights,
                tip_heights,
            )
            .await;

            let tree_locked = tree_clone.lock().await;

            for header_info in tree_locked
                .0
                .raw_nodes()
                .iter()
                .filter(|node| node.weight.miner == "" || node.weight.miner == MINER_UNKNOWN)
                .filter(|node| {
                    let h = node.weight.height;
                    interesting_heights.contains(&h)
                        || interesting_heights.contains(&(h + 1))
                        || interesting_heights.contains(&(h + 2))
                        || interesting_heights.contains(&(max(h, 1) - 1))
                })
                .map(|node| node.weight.clone())
            {
                if let Err(e) = pool_id_tx_clone.send(header_info.header.block_hash()) {
                    error!(
                        "Could not send block hash into the pool identification channel: {}",
                        e
                    );
                }
            }
        });

        // A thread that identifies miners for each header send into the pool
        // id channel
        let tree_clone = tree.clone();
        let db_clone2 = db_clone.clone();
        let caches_clone = caches.clone();
        let network_clone = network.clone();
        task::spawn(async move {
            let pool_identification_network = match network.pool_identification.network {
                Some(ref network) => network.to_network(),
                None => Network::Regtest,
            };
            let pool_identification_data = default_data(pool_identification_network);

            let limit = 100;
            let mut buffer: Vec<BlockHash> = Vec::with_capacity(limit);
            loop {
                buffer.clear();
                pool_id_rx.recv_many(&mut buffer, limit).await;
                for hash in buffer.iter() {
                    if !network_clone.pool_identification.enable {
                        continue;
                    }

                    let idx: NodeIndex = {
                        let tree_locked = tree_clone.lock().await;
                        match tree_locked.1.get(hash) {
                            Some(idx) => *idx,
                            None => {
                                error!("Block hash {} not (yet) present in tree for network: {}. Skipping identification...", hash.to_string(), network_clone.name);
                                continue;
                            }
                        }
                    };

                    let mut header_info = {
                        let tree_locked = tree_clone.lock().await;
                        tree_locked.0[idx].clone()
                    };

                    // skip miner identification if we previously identified a miner
                    if !(header_info.miner == MINER_UNKNOWN.to_string() || header_info.miner == "")
                    {
                        continue;
                    }

                    let mut miner = MINER_UNKNOWN.to_string();
                    for node in network_clone.nodes.iter().cloned() {
                        match node.coinbase(&header_info.header.block_hash()).await {
                            Ok(coinbase) => {
                                miner = match coinbase.identify_pool(
                                    pool_identification_network,
                                    &pool_identification_data,
                                ) {
                                    Some(result) => result.pool.name,
                                    None => MINER_UNKNOWN.to_string(),
                                };
                            }
                            Err(e) => {
                                warn!(
                                    "Could not get coinbase for block {} from node {}: {}",
                                    header_info.header.block_hash().to_string(),
                                    node.info().name,
                                    e
                                );
                            }
                        }
                        if miner != MINER_UNKNOWN.to_string() {
                            info!(
                                "Updated miner for block {} from node {}: {}",
                                header_info.height,
                                node.info().name,
                                miner
                            );
                            break;
                        }
                    }
                    header_info.update_miner(miner);

                    // update in-memory graph
                    {
                        let mut tree_locked = tree_clone.lock().await;
                        tree_locked.0[idx] = header_info.clone();
                    }
                    // write to db
                    if let Err(e) = db::update_miner(
                        db_clone2.clone(),
                        &header_info.header.block_hash(),
                        header_info.miner.clone(),
                    )
                    .await
                    {
                        warn!(
                            "Could not update miner to {} for block {}: {}",
                            header_info.miner.clone(),
                            &header_info.header.block_hash(),
                            e
                        );
                    }
                    // update cache
                    update_cache(
                        &caches_clone,
                        network.id,
                        CacheUpdate::HeaderMiner { header_info },
                    )
                    .await;
                }
            }
        });
    }

    let www_dir = warp::get()
        .and(warp::path("static"))
        .and(warp::fs::dir(config.www_path.clone()));
    let index_html = warp::get()
        .and(warp::path::end())
        .and(warp::fs::file(config.www_path.join("index.html")));
    let fullscreen_html = warp::get()
        .and(warp::path!("fullscreen"))
        .and(warp::fs::file(config.www_path.join("fullscreen.html")));
    let playground_html = warp::get()
        .and(warp::path!("playground"))
        .and(warp::fs::file(config.www_path.join("playground.html")));

    let info_json = warp::get()
        .and(warp::path!("api" / "info.json"))
        .and(api::with_footer(config.footer_html.clone()))
        .and_then(api::info_response);

    let data_json = warp::get()
        .and(warp::path!("api" / u32 / "data.json"))
        .and(api::with_caches(caches.clone()))
        .and_then(api::data_response);

    let forks_rss = warp::get()
        .and(warp::path!("rss" / u32 / "forks.xml"))
        .and(api::with_caches(caches.clone()))
        .and(api::with_networks(network_infos.clone()))
        .and(rss::with_rss_base_url(config.rss_base_url.clone()))
        .and_then(rss::forks_response);

    let invalid_blocks_rss = warp::get()
        .and(warp::path!("rss" / u32 / "invalid.xml"))
        .and(api::with_caches(caches.clone()))
        .and(api::with_networks(network_infos.clone()))
        .and(rss::with_rss_base_url(config.rss_base_url.clone()))
        .and_then(rss::invalid_blocks_response);

    let lagging_nodes_rss = warp::get()
        .and(warp::path!("rss" / u32 / "lagging.xml"))
        .and(api::with_caches(caches.clone()))
        .and(api::with_networks(network_infos.clone()))
        .and(rss::with_rss_base_url(config.rss_base_url.clone()))
        .and_then(rss::lagging_nodes_response);

    let unreachable_nodes_rss = warp::get()
        .and(warp::path!("rss" / u32 / "unreachable.xml"))
        .and(api::with_caches(caches.clone()))
        .and(api::with_networks(network_infos.clone()))
        .and(rss::with_rss_base_url(config.rss_base_url.clone()))
        .and_then(rss::unreachable_nodes_response);

    let networks_json = warp::get()
        .and(warp::path!("api" / "networks.json"))
        .and(api::with_networks(network_infos))
        .and_then(api::networks_response);

    let change_sse = warp::path!("api" / "changes")
        .and(warp::get())
        .map(move || {
            let tipchanges_rx = tipchanges_tx.clone().subscribe();
            let broadcast_stream = BroadcastStream::new(tipchanges_rx);
            let event_stream = broadcast_stream.map(move |d| match d {
                Ok(d) => api::data_changed_sse(d),
                Err(e) => {
                    error!("Could not SSE notify about tip changed event: {}", e);
                    api::data_changed_sse(u32::MAX)
                }
            });
            let stream = warp::sse::keep_alive().stream(event_stream);
            warp::sse::reply(stream)
        });

    let routes = www_dir
        .or(index_html)
        .or(fullscreen_html)
        .or(playground_html)
        .or(data_json)
        .or(info_json)
        .or(networks_json)
        .or(change_sse)
        .or(forks_rss)
        .or(lagging_nodes_rss)
        .or(unreachable_nodes_rss)
        .or(invalid_blocks_rss);

    warp::serve(routes).run(config.address).await;
    Ok(())
}

// Find out for which heights we have tips for. These are
// interesting to us - we don't want strip them from the tree.
// This includes tips that aren't from a fork, but rather from
// a stale or stuck node (i.e. not an up-to-date view of the
// blocktree).
async fn tip_heights(network_id: u32, caches: &Caches) -> BTreeSet<u64> {
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

#[derive(Debug)]
enum CacheUpdate {
    HeaderMiner {
        header_info: HeaderInfo,
    },
    HeaderTree {
        header_infos_json: Vec<HeaderInfoJson>,
        forks: Vec<Fork>,
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
        }
    }
}

async fn is_node_reachable(caches: &Caches, network_id: u32, node_id: u32) -> bool {
    let locked_cache = caches.lock().await;
    locked_cache
        .get(&network_id)
        .expect("this network should be in the caches")
        .node_data
        .get(&node_id)
        .expect("this node should be in the network cache")
        .reachable
}

async fn update_cache(caches: &Caches, network_id: u32, update: CacheUpdate) {
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

            locked_cache.entry(network_id).and_modify(|e| {
                e.header_infos_json = new_header_infos_map
                    .iter()
                    .map(|(_, header)| header.clone())
                    .collect();
                e.forks = forks;
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
    }
}

async fn load_node_version(node: BoxedSyncSendNode, network: &str) -> String {
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

async fn insert_new_headers_into_tree(tree: &Tree, new_headers: &[HeaderInfo]) -> bool {
    let mut tree_changed: bool = false;
    let mut tree_locked = tree.lock().await;
    // insert headers to tree
    for h in new_headers {
        if !tree_locked.1.contains_key(&h.header.block_hash()) {
            let idx = tree_locked.0.add_node(h.clone());
            tree_locked.1.insert(h.header.block_hash(), idx);
            tree_changed = true;
        }
    }
    // connect nodes with edges
    for current in new_headers {
        let idx_current: NodeIndex;
        let idx_prev: NodeIndex;
        {
            idx_current = *tree_locked
                    .1
                    .get(&current.header.block_hash())
                    .expect(
                    "current header should be in the map as we just inserted it or it was already present",
                );
            match tree_locked.1.get(&current.header.prev_blockhash) {
                Some(idx) => idx_prev = *idx,
                None => {
                    continue; // the tree's root has no previous block, skip it
                }
            }
        }
        tree_locked.0.update_edge(idx_prev, idx_current, false);
    }
    tree_changed
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::node::NodeInfo;

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
                NodeDataJson::new(node.clone(), &vec![], "".to_string(), 0, true),
            );
            locked_caches.insert(
                network_id,
                Cache {
                    header_infos_json: vec![],
                    node_data,
                    forks: vec![],
                    recent_miners: vec![],
                },
            );
        }
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
        )
        .await;
        assert_eq!(
            get_test_node_reachable(&caches, network_id, node.id).await,
            true
        );
    }
}
