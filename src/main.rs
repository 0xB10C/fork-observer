use std::cmp::max;
use std::collections::BTreeMap;
use std::collections::HashMap;
use std::sync::Arc;
// TODO: remove?
use std::convert::Infallible;

use tokio::sync::Mutex;
use tokio::task;
use tokio::time;

use warp::Filter;

use bitcoincore_rpc::bitcoin;
use bitcoincore_rpc::bitcoin::BlockHash;
use bitcoincore_rpc::json::{GetChainTipsResult, GetChainTipsResultStatus};
use bitcoincore_rpc::{Client, RpcApi};

use rusqlite::Connection;

use petgraph::graph::DiGraph;
use petgraph::graph::NodeIndex;
use petgraph::visit::Dfs;

use log::info;

mod config;
mod types;

use types::{DataQuery, HeaderInfo, HeaderInfoJson, NodeInfoJson, NetworksJsonResponse, NetworkJson, DataJsonResponse};

use config::Network;

type NodeInfo = BTreeMap<u8, NodeInfoJson>;
type Cache = (Vec<HeaderInfoJson>, NodeInfo);
type Caches = Arc<Mutex<BTreeMap<u32, Cache>>>;
type TreeInfo = (DiGraph<HeaderInfo, bool>, HashMap<BlockHash, NodeIndex>);
type Tree = Arc<Mutex<TreeInfo>>;
type Db = Arc<Mutex<Connection>>;
type Rpc = Arc<Client>;

// Maximum number of tips to send via a data.json response. Fewer tips mean
// less work during collapsing.
const MAX_TIPS: usize = 100;

async fn get_new_active_tips(
    tips: &GetChainTipsResult,
    rest_url: String,
    tree: &Tree,
    rpc: Rpc,
    min_fork_height: u64,
) -> Vec<HeaderInfo> {
    let mut new_headers: Vec<HeaderInfo> = Vec::new();
    let first_fork_tip = tips
        .iter()
        .filter(|tip| tip.height - tip.branch_length as u64 > min_fork_height)
        .min_by_key(|tip| tip.height - tip.branch_length as u64)
        .unwrap();
    let min_height = first_fork_tip.height - first_fork_tip.branch_length as u64;
    let scan_start_height = max(min_height as u64 - 5, 0);

    let current_height: u64;
    {
        let locked_tree = tree.lock().await;
        if locked_tree.0.node_count() == 0 {
            current_height = scan_start_height;
        } else {
            let max_tip_idx = locked_tree
                .0
                .externals(petgraph::Direction::Outgoing)
                .max_by_key(|idx| locked_tree.0[*idx].height)
                .unwrap();
            current_height = locked_tree.0[max_tip_idx].height;
        }
    }

    let active_tip = tips
        .iter()
        .filter(|tip| tip.status == GetChainTipsResultStatus::Active)
        .last()
        .unwrap();

    const STEP_SIZE: usize = 2000;
    for query_height in (current_height + 1..=active_tip.height).step_by(STEP_SIZE) {
        let header_hash = rpc.get_block_hash(query_height).unwrap();
        {
            let locked_tree = tree.lock().await;
            if locked_tree.1.contains_key(&header_hash) {
                continue;
            }
        }
        let headers = get_active_chain_headers(rest_url.clone(), STEP_SIZE, header_hash).await;
        for height_header_pair in (query_height..(query_height + headers.len() as u64)).zip(headers)
        {
            new_headers.push(HeaderInfo {
                height: height_header_pair.0,
                header: height_header_pair.1,
            });
        }
    }

    return new_headers;
}

async fn get_new_nonactive_tips(
    tips: &GetChainTipsResult,
    tree: &Tree,
    rpc: Rpc,
    min_fork_height: u64,
) -> Vec<HeaderInfo> {
    let mut new_headers: Vec<HeaderInfo> = Vec::new();

    for inactive_tip in tips
        .iter()
        .filter(|tip| tip.height - tip.branch_length as u64 > min_fork_height)
        .filter(|tip| tip.status != GetChainTipsResultStatus::Active)
    {
        let mut next_header = inactive_tip.hash;
        for i in 0..inactive_tip.branch_length {
            {
                let tree_locked = tree.lock().await;
                if tree_locked.1.contains_key(&inactive_tip.hash) {
                    break;
                }
            }

            let height = inactive_tip.height - i as u64;
            info!(
                "loading non-active-chain header: hash={}, height={}",
                next_header.to_string(),
                height
            );
            let header = rpc.get_block_header(&next_header).unwrap();

            new_headers.push(HeaderInfo {
                height: height,
                header: header,
            });
            next_header = header.prev_blockhash;
        }
    }

    return new_headers;
}

async fn get_new_tips(
    tips: &GetChainTipsResult,
    tree: &Tree,
    rpc: Rpc,
    rest_url: String,
    min_fork_height: u64,
) -> Vec<HeaderInfo> {
    let mut new_headers: Vec<HeaderInfo> = Vec::new();
    let mut active_new_headers: Vec<HeaderInfo> =
        get_new_active_tips(tips, rest_url.clone(), tree, rpc.clone(), min_fork_height).await;
    new_headers.append(&mut active_new_headers);
    let mut nonactive_new_headers: Vec<HeaderInfo> =
        get_new_nonactive_tips(tips, tree, rpc.clone(), min_fork_height).await;
    new_headers.append(&mut nonactive_new_headers);
    return new_headers;
}

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

    setup_db(db.clone()).await;

    for network in config.networks.iter().cloned() {
        info!(
            "network '{}' (id={}) has {} nodes",
            network.name,
            network.id,
            network.nodes.len()
        );

        let tree: Tree = Arc::new(Mutex::new(load_treeinfos_from_db(db.clone(), network.id).await));

        let headerinfojson = collapse_tree(&tree).await;
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
            let mut has_node_info = false;
            task::spawn(async move {
                loop {
                    let db_write = db_write.clone();
                    let tips = get_tips(rpc.clone()).await;
                    let new_headers: Vec<HeaderInfo> = get_new_tips(
                        &tips,
                        &tree_clone,
                        rpc.clone(),
                        rest_url.clone(),
                        network_cloned.min_fork_height,
                    )
                    .await;
                    if !new_headers.is_empty() || !has_node_info {
                        {
                            let mut tree_locked = tree_clone.lock().await;
                            for h in new_headers.clone() {
                                if !tree_locked.1.contains_key(&h.header.block_hash()) {
                                    let idx = tree_locked.0.add_node(h.clone());
                                    tree_locked.1.insert(h.header.block_hash(), idx);
                                }
                            }
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
                                        None => continue, // the tree root as no prev block, skip it
                                    }
                                }
                                tree_locked.0.update_edge(idx_prev, idx_current, false);
                            }
                        }

                        write_to_db(&new_headers, db_write, network_cloned.id).await;

                        let headerinfojson = collapse_tree(&tree_clone).await;
                        let nodeinfojson = NodeInfoJson::new(node.clone(), &tips);
                        {
                            let mut locked_cache = caches_clone.lock().await;
                            let entry = locked_cache.get(&network_cloned.id).expect("network should already exist in cache");
                            let mut node_infos = entry.1.clone();
                            node_infos.insert(node.id, nodeinfojson);
                            locked_cache.insert(network_cloned.id, (headerinfojson, node_infos));
                        }
                        has_node_info = true;
                    }
                    interval.tick().await;
                }
            });
        }
    }

    let index_html = warp::get()
        .and(warp::path::end())
        .and(warp::fs::file(config.www_path.join("index.html")));

    let style_css = warp::get()
        .and(warp::path!("css" / "style.css"))
        .and(warp::fs::file(config.www_path.join("css/style.css")));

    let blocktree_js = warp::get()
        .and(warp::path!("js" / "blocktree.js"))
        .and(warp::fs::file(config.www_path.join("js/blocktree.js")));

    let logo_png = warp::get()
        .and(warp::path!("img" / "logo.png"))
        .and(warp::fs::file(config.www_path.join("img/logo.png")));

    let d3_js = warp::get()
        .and(warp::path!("js" / "d3.v7.min.js"))
        .and(warp::fs::file(config.www_path.join("js/d3.v7.min.js")));

    let data_json = warp::get()
        .and(warp::path("data.json"))
        .and(with_caches(caches.clone()))
        .and(warp::query::<DataQuery>())
        .and_then(data_response);

    let networks_json = warp::get()
        .and(warp::path("networks.json"))
        .and(with_networks(config.networks.clone()))
        .and_then(networks_response);

    let routes = index_html
        .or(blocktree_js)
        .or(logo_png)
        .or(style_css)
        .or(data_json)
        .or(networks_json)
        .or(d3_js);

    warp::serve(routes).run(config.address).await;
}

async fn collapse_tree(tree: &Tree) -> Vec<HeaderInfoJson> {
    let tree_locked = tree.lock().await;

    let mut height_occurences: BTreeMap<u64, usize> = BTreeMap::new();
    for node in tree_locked.0.raw_nodes() {
        let counter = height_occurences.entry(node.weight.height).or_insert(0);
        *counter += 1;
    }
    let active_tip_height: u64 = height_occurences
        .iter()
        .map(|(k, _)| *k)
        .max()
        .expect("we should have at least one header in the tree here");
    let mut relevant_heights: Vec<u64> = height_occurences
        .iter()
        .filter(|(_, v)| **v > 1)
        .map(|(k, _)| *k)
        .collect();
    relevant_heights.push(active_tip_height);
    relevant_heights.sort();
    relevant_heights = relevant_heights.iter().rev().take(MAX_TIPS).cloned().collect();

    let mut collapsed_tree = tree_locked.0.filter_map(
        |_, node| {
            let height = node.height;
            for x in -3i64..=3 {
                if relevant_heights.contains(&((height as i64-x) as u64)) {
                    return Some(node);
                }
            }
            return None;
        },
        |_, edge| Some(edge),
    );

    let mut prev_header_to_connect_to: Option<NodeIndex> = None;
    let root_indicies: Vec<NodeIndex> = collapsed_tree.externals(petgraph::Direction::Incoming).collect();
    // Assumes root_indicies is sorted..
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
        collapsed_tree.externals(petgraph::Direction::Incoming).count(), // root nodes
        collapsed_tree.externals(petgraph::Direction::Outgoing).count(), // tip nodes
    );

    let mut headers: Vec<HeaderInfoJson> = Vec::new();
    for idx in collapsed_tree.node_indices() {
        let prev_nodes = collapsed_tree.neighbors_directed(idx, petgraph::Direction::Incoming);
        let prev_node_index: usize;
        match prev_nodes.clone().count() {
            0 => prev_node_index = usize::MAX,
            1 => prev_node_index = prev_nodes.last().expect("we should have exactly one previous node").index(),
            _ => panic!("got multiple previous nodes. this should not happen.")
        }
        headers.push(
            HeaderInfoJson::new(collapsed_tree[idx], idx.index(), prev_node_index)
        );
    };

    return headers;
}

fn with_caches(
    caches: Caches,
) -> impl Filter<Extract = (Caches,), Error = std::convert::Infallible> + Clone {
    warp::any().map(move || caches.clone())
}

fn with_networks(
    networks: Vec<Network>,
) -> impl Filter<Extract = (Vec<Network>,), Error = std::convert::Infallible> + Clone {
    warp::any().map(move || networks.clone())
}

// Loads header and tip information for a specified network from the DB and
// builds a header-tree from it.
async fn load_treeinfos_from_db(db: Db, network: u32) -> TreeInfo {
    let header_infos = load_header_infos_from_db(db, network).await;

    let mut tree: DiGraph<HeaderInfo, bool> = DiGraph::new();
    let mut hash_index_map: HashMap<BlockHash, NodeIndex> = HashMap::new();
    info!("building header tree for network {}..", network);
    // add headers as nodes
    for h in header_infos.clone() {
        let idx = tree.add_node(h.clone());
        hash_index_map.insert(h.header.block_hash(), idx);
    }
    info!(".. added headers from network {}", network);
    // add prev-current block relationships as edges
    for current in header_infos {
        let idx_current = hash_index_map
            .get(&current.header.block_hash())
            .expect("current header should be in the map as we just inserted it");
        match hash_index_map.get(&current.header.prev_blockhash) {
            Some(idx_prev) => tree.update_edge(*idx_prev, *idx_current, false),
            None => continue,
        };
    }
    info!(
        ".. added relationships between headers from network {}",
        network
    );
    info!(
        "done building header tree for network {}: roots={}, tips={}",
        network,
        tree.externals(petgraph::Direction::Incoming).count(), // root nodes
        tree.externals(petgraph::Direction::Outgoing).count(), // tip nodes
    );
    return (tree, hash_index_map);
}

async fn load_header_infos_from_db(db: Db, network: u32) -> Vec<HeaderInfo> {
    info!("loading headers for network {} from database..", network);
    let db_locked = db.lock().await;

    let mut stmt = db_locked
        .prepare(
            "SELECT
            height, header
        FROM
            headers
        WHERE
            network = ?1
        ORDER BY
            height
            ASC
        ",
        )
        .unwrap();
    let headers: Vec<HeaderInfo> = stmt
        .query_map([network.to_string()], |row| {
            let header_hex: String = row.get(1).unwrap();
            let header_bytes = hex::decode(&header_hex).unwrap();
            let header = bitcoin::consensus::deserialize(&header_bytes).unwrap();

            Ok(HeaderInfo {
                height: row.get(0).unwrap(),
                header: header,
            })
        })
        .unwrap()
        .map(|h| h.unwrap())
        .collect();
    info!(
        "done loading headers for network {}: headers={}",
        network,
        headers.len()
    );

    return headers;
}

async fn data_response(caches: Caches, query: DataQuery) -> Result<impl warp::Reply, Infallible> {
    let network: u32 = query.network;

    let caches_locked = caches.lock().await;
    let (header_info_json, node_infos) = caches_locked.get(&network).unwrap().clone();

    Ok(warp::reply::json(&DataJsonResponse{
        header_infos: header_info_json,
        nodes: node_infos.values().cloned().collect(),
    }))
}


async fn networks_response(networks: Vec<Network>) -> Result<impl warp::Reply, Infallible> {
    let network_infos: Vec<NetworkJson> = networks.iter().map(|n| NetworkJson::new(n)).collect();

    Ok(warp::reply::json(&NetworksJsonResponse {
        networks: network_infos,
    }))
}


async fn setup_db(db: Db) {
    db.lock()
        .await
        .execute(
            "CREATE TABLE IF NOT EXISTS headers (
             height     INT,
             network    INT,
             hash       BLOB,
             header     BLOB,
             PRIMARY KEY (network, hash, header)
        )",
            [],
        )
        .unwrap();
}

async fn write_to_db(new_headers: &Vec<HeaderInfo>, db: Db, network: u32) {
    let mut db_locked = db.lock().await;
    let tx = db_locked.transaction().unwrap();
    info!(
        "inserting {} headers from network {} into the database..",
        new_headers.len(),
        network
    );
    for info in new_headers {
        tx.execute(
            "INSERT OR IGNORE INTO headers
                   (height, network, hash, header)
                   values (?1, ?2, ?3, ?4)",
            &[
                &info.height.to_string(),
                &network.to_string(),
                &info.header.block_hash().to_string(),
                &bitcoin::consensus::encode::serialize_hex(&info.header),
            ],
        )
        .unwrap();
    }
    tx.commit().unwrap();
    info!(
        "done inserting {} headers from network {} into the database",
        new_headers.len(),
        network
    );
}

async fn get_tips(rpc: Rpc) -> GetChainTipsResult {
    let res = task::spawn_blocking(move || {
        return rpc.get_chain_tips().unwrap();
    })
    .await;
    res.unwrap()
}

async fn get_active_chain_headers(
    rest_url: String,
    count: usize,
    start: BlockHash,
) -> Vec<bitcoin::BlockHeader> {
    info!(
        "loading active-chain headers starting from {}",
        start.to_string()
    );
    let res = reqwest::get(format!(
        "http://{}/rest/headers/{}/{}.bin",
        rest_url,
        count,
        start.to_string()
    ))
    .await
    .unwrap();

    let headers: Vec<bitcoin::BlockHeader> = res.bytes()
        .await
        .unwrap()
        .chunks(80)
        .map(|hbytes| bitcoin::consensus::deserialize(&hbytes).unwrap())
        .collect();

    info!(
        "loaded {} active-chain headers starting from {}",
        headers.len(),
        start.to_string()
    );

    return headers;

}
