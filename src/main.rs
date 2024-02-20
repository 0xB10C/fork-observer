#![cfg_attr(feature = "strict", deny(warnings))]

use bitcoin_pool_identification::{default_data, PoolIdentification};
use bitcoincore_rpc::bitcoin::{BlockHash, Network};
use bitcoincore_rpc::Error::JsonRpc;
use env_logger::Env;
use futures_util::StreamExt;
use log::{error, info, warn};
use petgraph::graph::NodeIndex;
use rusqlite::Connection;
use std::cmp::max;
use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;
use std::time::SystemTime;
use tokio::sync::mpsc::unbounded_channel;
use tokio::sync::{broadcast, Mutex};
use tokio::task;
use tokio::time::{interval, sleep, Duration};
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

use types::{Cache, Caches, ChainTip, Db, HeaderInfo, NetworkJson, NodeData, NodeDataJson, Tree};

use crate::error::{DbError, MainError};

const VERSION_UNKNOWN: &str = "unknown";
const MINER_UNKNOWN: &str = "Unknown";
const MAX_FORKS_IN_CACHE: usize = 50;

#[tokio::main]
async fn main() -> Result<(), MainError> {
    env_logger::Builder::from_env(Env::default().default_filter_or("info")).init();

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

        let forks = headertree::recent_forks(&tree, MAX_FORKS_IN_CACHE).await;
        let header_infos_json =
            headertree::strip_tree(&tree, network.max_interesting_heights, BTreeSet::new()).await;
        {
            let mut locked_caches = caches.lock().await;
            locked_caches.insert(
                network.id,
                Cache {
                    header_infos_json,
                    node_data: BTreeMap::new(),
                    forks,
                },
            );
        }

        for node in network.nodes.iter().cloned() {
            let network = network.clone();
            let mut interval = interval(config.query_interval);
            let db_write = db.clone();
            let tree_clone = tree.clone();
            let caches_clone = caches.clone();
            let tipchanges_tx_cloned = tipchanges_tx.clone();
            let pool_id_tx_clone = pool_id_tx.clone();
            let mut has_node_info = false;
            let mut version_info: String = String::default();
            let mut version_request_tries: u8 = 0;
            let mut last_tips: Vec<ChainTip> = vec![];
            task::spawn(async move {
                loop {
                    // We specifically wait at the beginning of the loop, as we
                    // are using 'continue' on errors. If we would wait at the end,
                    // we might skip the waiting.
                    interval.tick().await;
                    if version_info == String::default() {
                        // The Bitcoin Core version is requested via the getnetworkinfo RPC. This
                        // RPC exposes sensitive information to the caller, so it might not be
                        // allowed on the whitelist. We set the version to VERSION_UNKNOWN if we
                        // can't request it. As Bitcoin Core RPC interface might not be up yet, we
                        // try to request the version multiple times.
                        loop {
                            match node.version().await {
                                Ok(v) => {
                                    version_info = v;
                                    break;
                                }
                                Err(e) => match e {
                                    error::FetchError::BitcoinCoreRPC(JsonRpc(msg)) => {
                                        if version_request_tries > 5 {
                                            warn!("Could not fetch getnetworkinfo from {} on network '{}' (id={}): {:?}. Using '{}' as version.", node.info(), network.name, network.id, msg, VERSION_UNKNOWN);
                                            version_info = VERSION_UNKNOWN.to_string();
                                            break;
                                        } else {
                                            warn!("Could not fetch getnetworkinfo from {} on network '{}' (id={}): {:?}. Retrying...", node.info(), network.name, network.id, msg);
                                            version_request_tries += 1;
                                            interval.tick().await;
                                        }
                                    }
                                    _ => {
                                        error!("Could not fetch getnetworkinfo from {} on network '{}' (id={}): {:?}.", node.info(), network.name, network.id, e);
                                        version_info = VERSION_UNKNOWN.to_string();
                                        break;
                                    }
                                },
                            };
                        }
                        continue;
                    };
                    let tips = match node.tips().await {
                        Ok(tips) => tips,
                        Err(e) => {
                            error!(
                                "Could not fetch chaintips from {} on network '{}' (id={}): {:?}",
                                node.info(),
                                network.name,
                                network.id,
                                e
                            );
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

                        if !new_headers.is_empty() || !has_node_info {
                            {
                                let mut tree_locked = tree_clone.lock().await;
                                // insert headers to tree
                                for h in new_headers.clone() {
                                    if !tree_locked.1.contains_key(&h.header.block_hash()) {
                                        let idx = tree_locked.0.add_node(h.clone());
                                        tree_locked.1.insert(h.header.block_hash(), idx);
                                    }
                                }
                                // connect nodes with edges
                                for current in new_headers.clone() {
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
                            }
                            if !new_headers.is_empty() {
                                match db::write_to_db(&new_headers, db_write, network.id).await {
                                    Ok(_) => info!("Written {} new headers to database for network '{}' by node {}", new_headers.len(), network.name, node.info()),
                                    Err(e) => {
                                        error!("Could not write new headers for network '{}' by node {} to database: {}", network.name, node.info(), e);
                                        return MainError::Db(e);
                                    },
                                }
                            }
                        }

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

                        // only put tips that we also keep headers for in the cache
                        let min_height = header_infos_json
                            .iter()
                            .min_by_key(|h| h.height)
                            .expect("we should have atleast one header in here")
                            .height;
                        let relevant_tips = tips
                            .iter()
                            .filter(|t| t.height >= min_height)
                            .cloned()
                            .collect();

                        let last_change_timestamp = match SystemTime::now()
                            .duration_since(SystemTime::UNIX_EPOCH)
                        {
                            Ok(n) => n.as_secs(),
                            Err(_) => {
                                warn!("SystemTime is before UNIX_EPOCH time. Node last_change_timestamp set to 0.");
                                0u64
                            }
                        };
                        let node_info_json = NodeDataJson::new(
                            node.info(),
                            &relevant_tips,
                            version_info.clone(),
                            last_change_timestamp,
                            true, // here, the node was reachable
                        );

                        let forks = headertree::recent_forks(&tree_clone, MAX_FORKS_IN_CACHE).await;
                        {
                            let mut locked_cache = caches_clone.lock().await;
                            let this_network = locked_cache
                                .get(&network.id)
                                .expect("network should already exist in cache");
                            let mut node_infos = this_network.node_data.clone();
                            node_infos.insert(node.info().id, node_info_json);
                            locked_cache.insert(
                                network.id,
                                Cache {
                                    header_infos_json,
                                    node_data: node_infos,
                                    forks,
                                },
                            );
                        }

                        match tipchanges_tx_cloned.clone().send(network.id) {
                            Ok(_) => (),
                            Err(e) => {
                                warn!("Could not send tip_changed update into the channel: {}", e)
                            }
                        };

                        has_node_info = true;
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
                        "Could not block hash into the pool identification channel: {}",
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

                    // write to db and cache
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
                    {
                        let mut tree_locked = tree_clone.lock().await;
                        tree_locked.0[idx] = header_info;
                    }
                }

                // update the cache
                {
                    let tip_heights: BTreeSet<u64> = tip_heights(network.id, &caches_clone).await;

                    let header_infos_json = headertree::strip_tree(
                        &tree_clone,
                        network.max_interesting_heights,
                        tip_heights,
                    )
                    .await;
                    let mut locked_cache = caches_clone.lock().await;
                    let mut cached = locked_cache
                        .get(&network.id)
                        .expect("network should already exist in cache")
                        .clone();
                    cached.header_infos_json = header_infos_json;
                    locked_cache.insert(network.id, cached);
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
        .or(data_json)
        .or(info_json)
        .or(networks_json)
        .or(change_sse)
        .or(forks_rss)
        .or(lagging_nodes_rss)
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
