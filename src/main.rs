#![cfg_attr(feature = "strict", deny(warnings))]

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::SystemTime;

use tokio::sync::{broadcast, Mutex};
use tokio::task;
use tokio::time;
use tokio_stream::wrappers::BroadcastStream;

use futures_util::StreamExt;
use warp::Filter;

use bitcoincore_rpc::json::GetChainTipsResult;
use bitcoincore_rpc::Client;

use rusqlite::Connection;

use petgraph::graph::NodeIndex;

use log::{error, info, warn};

mod api;
mod config;
mod db;
mod error;
mod headertree;
mod rpc;
mod types;

use types::{Caches, DataQuery, Db, HeaderInfo, NodeInfoJson, Rpc, Tree};

use crate::error::{DbError, FetchError, MainError};

const VERSION_UNKNOWN: &str = "unknown";

#[tokio::main]
async fn main() -> Result<(), MainError> {
    env_logger::init();

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

    for network in config.networks.iter().cloned() {
        info!(
            "network '{}' (id={}) has {} nodes",
            network.name,
            network.id,
            network.nodes.len()
        );

        let tree: Tree = Arc::new(Mutex::new(
            match db::load_treeinfos(db.clone(), network.id).await {
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

        let headerinfojson = headertree::collapse_tree(&tree, network.max_forks).await;
        {
            let mut locked_caches = caches.lock().await;
            locked_caches.insert(network.id, (headerinfojson, BTreeMap::new()));
        }

        for node in network.nodes.iter().cloned() {
            let client = match Client::new(&node.rpc_url, node.rpc_auth.clone()) {
                Ok(c) => c,
                Err(e) => {
                    error!(
                        "Could not create a RPC client for node '{}' on network '{}': {:?}",
                        node.name, network.name, e
                    );
                    return Err(FetchError::from(e).into());
                }
            };
            let rpc: Rpc = Arc::new(client);
            let rest_url = node.rpc_url.clone();
            let mut interval = time::interval(config.query_interval);
            let db_write = db.clone();
            let tree_clone = tree.clone();
            let caches_clone = caches.clone();
            let network_cloned = network.clone();
            let tipchanges_tx_cloned = tipchanges_tx.clone();
            let mut has_node_info = false;
            let mut version_info: String = String::default();
            let mut last_tips: GetChainTipsResult = vec![];
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
                        // can't request it.
                        version_info = match rpc::get_version_info(rpc.clone()).await {
                            Ok(version) => version,
                            Err(e) => {
                                error!("Could not fetch getnetworkinfo from node '{}' (id={}) on network '{}' (id={}): {:?}", node.name, node.id, network_cloned.name, network_cloned.id, e);
                                version_info = VERSION_UNKNOWN.to_string();
                                continue;
                            }
                        };
                    };
                    let tips = match rpc::get_tips(rpc.clone()).await {
                        Ok(tips) => tips,
                        Err(e) => {
                            error!("Could not fetch chaintips from node '{}' (id={}) on network '{}' (id={}): {:?}", node.name, node.id, network_cloned.name, network_cloned.id, e);
                            continue;
                        }
                    };

                    let new_headers: Vec<HeaderInfo> = match rpc::get_new_headers(
                        &tips,
                        &tree_clone,
                        rpc.clone(),
                        rest_url.clone(),
                        node.use_rest,
                        network_cloned.min_fork_height,
                    )
                    .await
                    {
                        Ok(headers) => headers,
                        Err(e) => {
                            error!("Could not fetch headers from node '{}' (id={}) on network '{}' (id={}): {}", node.name, node.id, network_cloned.name, network_cloned.id, e);
                            continue;
                        }
                    };

                    if last_tips != tips {
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
                                match db::write_to_db(&new_headers, db_write, network_cloned.id).await {
                                    Ok(_) => info!("Written {} new heders to database for network '{}' by node '{}'", new_headers.len(), network_cloned.name, node.name),
                                    Err(e) => {
                                        error!("Could not write new headers for network '{}' by node '{}' to database: {}", network_cloned.name, node.name, e);
                                        return MainError::Db(e);
                                    },
                                }
                            }
                        }

                        let headerinfojson =
                            headertree::collapse_tree(&tree_clone, network_cloned.max_forks).await;

                        // only put tips that we also have headers for in the cache
                        let min_height = headerinfojson
                            .iter()
                            .min_by_key(|h| h.height)
                            .expect("we should have atleast on header in here")
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
                        let nodeinfojson = NodeInfoJson::new(
                            node.clone(),
                            &relevant_tips,
                            version_info.clone(),
                            last_change_timestamp,
                        );
                        {
                            let mut locked_cache = caches_clone.lock().await;
                            let network = locked_cache
                                .get(&network_cloned.id)
                                .expect("network should already exist in cache");
                            let mut node_infos = network.1.clone();
                            node_infos.insert(node.id, nodeinfojson);
                            locked_cache.insert(network_cloned.id, (headerinfojson, node_infos));
                        }
                        if !new_headers.is_empty() {
                            match tipchanges_tx_cloned.clone().send(network_cloned.id) {
                                Ok(_) => (),
                                Err(e) => warn!(
                                    "Could not send tip_changed update into the channel: {}",
                                    e
                                ),
                            };
                        }
                        has_node_info = true;
                    }
                }
            });
        }
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
        .and(warp::path!("api" / "data.json"))
        .and(api::with_caches(caches.clone()))
        .and(warp::query::<DataQuery>())
        .and_then(api::data_response);

    let networks_json = warp::get()
        .and(warp::path!("api" / "networks.json"))
        .and(api::with_networks(config.networks.clone()))
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
        .or(change_sse);

    warp::serve(routes).run(config.address).await;
    Ok(())
}
