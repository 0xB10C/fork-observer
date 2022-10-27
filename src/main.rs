use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::SystemTime;
// TODO: remove?


use tokio_stream::wrappers::BroadcastStream;
use tokio::sync::{Mutex, broadcast};
use tokio::task;
use tokio::time;

use warp::Filter;
use futures_util::StreamExt;

use bitcoincore_rpc::json::GetChainTipsResult;
use bitcoincore_rpc::Client;

use rusqlite::Connection;


use petgraph::graph::NodeIndex;
use petgraph::visit::Dfs;

use log::{error, info, warn};

mod api;
mod db;
mod config;
mod error;
mod types;
mod rpc;

use types::{
    DataQuery, HeaderInfo, HeaderInfoJson, NodeInfoJson, Caches, Tree,
    Db, Rpc,
};

const VERSION_UNKNOWN: &str = "unknown";


#[tokio::main]
async fn main() {
    env_logger::init();

    let config: config::Config = match config::load_config() {
        Ok(config) => config,
        Err(e) => panic!("Could not load the configuration: {}", e),
    };

    if config.networks.is_empty() {
        panic!("No networks and nodes defined in the configuration.");
    }

    let connection = match Connection::open(config.database_path.clone()) {
        Ok(db) => db,
        Err(e) => panic!(
            "Could not open the database {:?}: {}",
            config.database_path, e
        ),
    };
    let db: Db = Arc::new(Mutex::new(connection));
    let caches: Caches = Arc::new(Mutex::new(BTreeMap::new()));

    db::setup_db(db.clone()).await;

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
            db::load_treeinfos(db.clone(), network.id).await,
        ));

        let headerinfojson = collapse_tree(&tree, network.max_forks).await;
        {
            let mut locked_caches = caches.lock().await;
            locked_caches.insert(network.id, (headerinfojson, BTreeMap::new()));
        }

        for node in network.nodes.iter().cloned() {
            let rpc: Rpc = Arc::new(Client::new(&node.rpc_url, node.rpc_auth.clone()).unwrap());
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
                    ).await {
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

                            db::write_to_db(&new_headers, db_write, network_cloned.id).await;
                        }

                        let headerinfojson =
                            collapse_tree(&tree_clone, network_cloned.max_forks).await;

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

                        let last_change_timestamp = match SystemTime::now().duration_since(SystemTime::UNIX_EPOCH) {
                            Ok(n) => n.as_secs(),
                            Err(_) => {
                                warn!("SystemTime is before UNIX_EPOCH time. Node last_change_timestamp set to 0.");
                                0u64
                            },
                        };
                        let nodeinfojson = NodeInfoJson::new(node.clone(), &relevant_tips, version_info.clone(), last_change_timestamp);
                        {
                            let mut locked_cache = caches_clone.lock().await;
                            let network = locked_cache
                                .get(&network_cloned.id)
                                .expect("network should already exist in cache");
                            let mut node_infos = network.1.clone();
                            node_infos.insert(node.id, nodeinfojson);
                            locked_cache.insert(network_cloned.id, (headerinfojson, node_infos));
                        }
                        tipchanges_tx_cloned.clone().send(network_cloned.id);
                        has_node_info = true;
                    }
                interval.tick().await;
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

    let change_sse = warp::path!("api" / "changes").and(warp::get()).map(move || {
        let tipchanges_rx = tipchanges_tx.clone().subscribe();
        let broadcast_stream = BroadcastStream::new(tipchanges_rx);
        let event_stream = broadcast_stream.map(move |d| {
            api::data_changed_sse(d.unwrap())
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
}

async fn collapse_tree(tree: &Tree, max_forks: u64) -> Vec<HeaderInfoJson> {
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
