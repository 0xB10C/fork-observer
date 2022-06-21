use std::cmp::max;
use std::collections::BTreeMap;
use std::collections::HashMap;
use std::collections::HashSet;
use std::convert::TryInto;
use std::sync::Arc;
// TODO: remove?
use std::convert::Infallible;

use tokio::sync::Mutex;
use tokio::task;
use tokio::time;

use warp::Filter;

use bitcoincore_rpc::bitcoin;
use bitcoincore_rpc::bitcoin::hash_types::BlockHash;
use bitcoincore_rpc::json::{GetChainTipsResult, GetChainTipsResultStatus};
use bitcoincore_rpc::{Client, RpcApi};

use rusqlite::Connection;

use petgraph::graph::DiGraph;
use petgraph::graph::NodeIndex;
use petgraph::dot::{Dot, Config};
use petgraph::visit::Dfs;

use log::info;

mod config;
mod types;

use types::{DataQuery, HeaderInfo};

use config::Network;

type TreeInfo = (DiGraph<HeaderInfo, bool>, HashMap<BlockHash, NodeIndex>);
type Trees = Arc<Mutex<BTreeMap<u32, TreeInfo>>>;
type Db = Arc<Mutex<Connection>>;
type Rpc = Arc<Client>;

async fn get_new_active_tips(
    tips: &GetChainTipsResult,
    rest_url: String,
    trees: &Trees,
    network: u32,
    rpc: Rpc,
) -> Vec<HeaderInfo> {
    let mut new_headers: Vec<HeaderInfo> = Vec::new();
    let first_fork_tip = tips
        .iter()
        .min_by_key(|tip| tip.height - tip.branch_length as u64)
        .unwrap();
    let min_height = first_fork_tip.height - first_fork_tip.branch_length as u64;
    let scan_start_height = max(min_height as u64 - 5, 0);

    let current_height: u64;
    {
        let locked_tree = trees.lock().await;
        let tree_info = locked_tree.get(&network).unwrap();
        if tree_info.0.node_count() == 0 {
            current_height = scan_start_height;
        } else {
            let max_tip_idx = tree_info
                .0
                .externals(petgraph::Direction::Outgoing)
                .max_by_key(|idx| tree_info.0[*idx].height)
                .unwrap();
            current_height = tree_info.0[max_tip_idx].height;
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
            let locked_tree = trees.lock().await;
            let tree_info = locked_tree.get(&network).unwrap();
            if tree_info.1.contains_key(&header_hash) {
                panic!("active header already in tree");
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
    rest_url: String,
    trees: &Trees,
    network: u32,
    rpc: Rpc,
) -> Vec<HeaderInfo> {
    let mut new_headers: Vec<HeaderInfo> = Vec::new();

    for inactive_tip in tips
        .iter()
        .filter(|tip| tip.status != GetChainTipsResultStatus::Active)
    {
        let mut next_header = inactive_tip.hash;
        for i in 0..inactive_tip.branch_length {
            {
                let locked_tree = trees.lock().await;
                let tree_info = locked_tree.get(&network).unwrap();
                if tree_info.1.contains_key(&inactive_tip.hash) {
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
    trees: &Trees,
    rpc: Rpc,
    rest_url: String,
    network: u32,
) -> Vec<HeaderInfo> {
    let mut new_headers: Vec<HeaderInfo> = Vec::new();
    let mut active_new_headers: Vec<HeaderInfo> =
        get_new_active_tips(tips, rest_url.clone(), trees, network, rpc.clone()).await;
    println!("new active: {}", active_new_headers.len());
    new_headers.append(&mut active_new_headers);
    let mut nonactive_new_headers: Vec<HeaderInfo> =
        get_new_nonactive_tips(tips, rest_url, trees, network, rpc.clone()).await;
    println!("new non-active: {}", nonactive_new_headers.len());
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
    let trees: Trees = Arc::new(Mutex::new(BTreeMap::new()));

    setup_db(db.clone()).await;

    for network in config.networks.iter().cloned() {
        info!(
            "network '{}' (id={}) has {} nodes",
            network.name,
            network.id,
            network.nodes.len()
        );

        let tree_info = load_treeinfos_from_db(db.clone(), network.id).await;
        {
            trees.lock().await.insert(network.id, tree_info);
        }

        for node in network.nodes.iter().cloned() {
            let rpc: Rpc = Arc::new(Client::new(&node.rpc_url, node.rpc_auth.clone()).unwrap());
            let rest_url = node.rpc_url.clone();
            let mut interval = time::interval(config.query_interval);
            let db_write = db.clone();
            let trees_clone = trees.clone();
            let network_cloned = network.clone();
            task::spawn(async move {
                loop {
                    let db_write = db_write.clone();
                    let tips = get_tips(rpc.clone()).await;
                    info!(
                        "node '{}' on network '{}' has {} tips",
                        node.name,
                        network_cloned.name,
                        tips.len()
                    );
                    let new_headers: Vec<HeaderInfo> = get_new_tips(
                        &tips,
                        &trees_clone,
                        rpc.clone(),
                        rest_url.clone(),
                        network_cloned.id,
                    )
                    .await;
                    if !new_headers.is_empty() {
                        {
                            let mut trees_locked = trees_clone.lock().await;
                            let tree_info = trees_locked.get_mut(&network_cloned.id).unwrap();
                            for h in new_headers.clone() {
                                let idx = tree_info.0.add_node(h.clone());
                                tree_info.1.insert(h.header.block_hash(), idx);
                            }
                            for current in new_headers.clone() {
                                let idx_current = tree_info
                                    .1
                                    .get(&current.header.block_hash())
                                    .expect(
                                    "current header should be in the map as we just inserted it",
                                );
                                match tree_info.1.get(&current.header.prev_blockhash) {
                                    Some(idx_prev) => {
                                        tree_info.0.update_edge(*idx_prev, *idx_current, false)
                                    }
                                    None => continue,
                                };
                            }
                        }
                        write_to_db(&new_headers, db_write, network_cloned.id).await;
                        collapse_tree(&trees_clone, network_cloned.id).await;
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
        .and(with_trees(trees.clone()))
        .and(warp::query::<DataQuery>())
        .and_then(data_response);

    let data_json = warp::get()
        .and(warp::path("data.json"))
        .and(with_trees(trees.clone()))
        .and(warp::query::<DataQuery>())
        .and_then(data_response);

    /*
    let networks_json = warp::get()
        .and(warp::path("networks.json"))
        .and(with_db(db.clone()))
        .and_then(networks_response);
    */

    let routes = index_html
        .or(blocktree_js)
        .or(logo_png)
        .or(style_css)
        .or(data_json)
        //.or(networks_json)
        .or(d3_js);

    warp::serve(routes).run(config.address).await;
}

async fn collapse_tree(trees: &Trees, network: u32) {
    let trees_locked = trees.lock().await;
    let tree_info = trees_locked.get(&network).unwrap();
    let mut collapsed_tree = tree_info.0.clone();

    let mut height_occurences: BTreeMap<u64, usize> = BTreeMap::new();
    for node in collapsed_tree.raw_nodes() {
        let counter = height_occurences.entry(node.weight.height).or_insert(0);
        *counter += 1;
    }
    let relevant_heights: Vec<u64> = height_occurences
        .iter()
        .filter(|(_, v)| **v > 1)
        .map(|(k, _)| *k)
        .collect();
    println!("relevant_heights: {:?}", relevant_heights);

    let mut a = collapsed_tree.filter_map(
        |_, node| {
            let height = node.height;
            for x in -2i64..=2 {
                if relevant_heights.contains(&((height as i64-x) as u64)) {
                    return Some(node);
                }
            }
            return None;
        },
        |_, edge| Some(edge),
    );

    info!(
        "done collapsing tree: roots={}, tips={}",
        a.externals(petgraph::Direction::Incoming).count(), // root nodes
        a.externals(petgraph::Direction::Outgoing).count(), // tip nodes
    );

    let mut prev_header_to_connect_to: Option<NodeIndex> = None;
    let root_indicies: Vec<NodeIndex> = a.externals(petgraph::Direction::Incoming).collect();
    // Assumes root_indicies is sorted..
    for root in root_indicies.iter() {
        if let Some(prev_idx) = prev_header_to_connect_to {
            a.add_edge(prev_idx, *root, &false);
        }
        let mut max_height: u64 = u64::default();
        let mut dfs = Dfs::new(&a, *root);
        while let Some(idx) = dfs.next(&a) {
            let height = a[idx].height;
            if height > max_height {
                max_height = height;
                prev_header_to_connect_to = Some(idx);
            }
        }
    }

    println!("{:?}", Dot::with_config(&a, &[Config::EdgeNoLabel]));

    //let j = serde_json::to_string(&address)?;
}

fn with_trees(
    trees: Trees,
) -> impl Filter<Extract = (Trees,), Error = std::convert::Infallible> + Clone {
    warp::any().map(move || trees.clone())
}

// Loads header and tip information for a specified network from the DB and
// builds a header-tree from it.
async fn load_treeinfos_from_db(db: Db, network: u32) -> TreeInfo {
    let header_infos = load_header_infos_from_db(db, network).await;

    let mut tree: DiGraph<HeaderInfo, bool> = DiGraph::new();
    let mut hash_index_map: HashMap<BlockHash, NodeIndex> = HashMap::new();
    info!("buidling header tree for network {}..", network);
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

async fn data_response(trees: Trees, query: DataQuery) -> Result<impl warp::Reply, Infallible> {
    let network: u32 = query.network;
    println!("AAAAAA");

    let trees_locked = trees.lock().await;
    let tree_info = trees_locked.get(&network).unwrap();

    Ok(warp::reply::json(&tree_info.0))
}

/*
async fn networks_response(db: Db) -> Result<impl warp::Reply, Infallible> {
    let mut network_infos = vec![];

    Ok(warp::reply::json(&NetworksJsonResponse {
        networks: network_infos,
    }))
}
*/

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
            "INSERT INTO headers
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
        "loading up to {} active-chain headers starting from {}",
        count,
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

    res.bytes()
        .await
        .unwrap()
        .chunks(80)
        .map(|hbytes| bitcoin::consensus::deserialize(&hbytes).unwrap())
        .collect()
}
