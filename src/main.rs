use std::cmp::max;
use std::collections::BTreeMap;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::SystemTime;
// TODO: remove?
use std::convert::Infallible;

use tokio_stream::wrappers::BroadcastStream;
use tokio::sync::{Mutex, broadcast};
use tokio::task;
use tokio::time;

use warp::{sse::Event, Filter};
use futures_util::StreamExt;

use bitcoincore_rpc::bitcoin;
use bitcoincore_rpc::bitcoin::BlockHash;
use bitcoincore_rpc::json::{GetChainTipsResult, GetChainTipsResultStatus};
use bitcoincore_rpc::{Client, RpcApi};

use rusqlite::Connection;

use petgraph::graph::DiGraph;
use petgraph::graph::NodeIndex;
use petgraph::visit::Dfs;

use log::{error, info, warn};

mod config;
mod types;

use types::{
    DataJsonResponse, DataQuery, HeaderInfo, HeaderInfoJson, NetworkJson, NetworksJsonResponse,
    NodeInfoJson, DataChanged, InfoJsonResponse,
};

use config::Network;

type NodeInfo = BTreeMap<u8, NodeInfoJson>;
type Cache = (Vec<HeaderInfoJson>, NodeInfo);
type Caches = Arc<Mutex<BTreeMap<u32, Cache>>>;
type TreeInfo = (DiGraph<HeaderInfo, bool>, HashMap<BlockHash, NodeIndex>);
type Tree = Arc<Mutex<TreeInfo>>;
type Db = Arc<Mutex<Connection>>;
type Rpc = Arc<Client>;

async fn get_new_active_headers(
    tips: &GetChainTipsResult,
    rest_url: String,
    tree: &Tree,
    rpc: Rpc,
    min_fork_height: u64,
) -> Vec<HeaderInfo> {
    let mut new_headers: Vec<HeaderInfo> = Vec::new();
    let first_fork_tip = match tips
        .iter()
        .filter(|tip| tip.height - tip.branch_length as u64 > min_fork_height)
        .min_by_key(|tip| tip.height - tip.branch_length as u64)
    {
        Some(tip) => tip,
        None => {
            warn!("No tip qualifies as first_fork_tip. Is min_fork_height={} reasonable for this network?", min_fork_height);
            return new_headers;
        }
    };
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

async fn get_new_nonactive_headers(
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
        for i in 0..=inactive_tip.branch_length {
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

async fn get_new_headers(
    tips: &GetChainTipsResult,
    tree: &Tree,
    rpc: Rpc,
    rest_url: String,
    min_fork_height: u64,
) -> Vec<HeaderInfo> {
    let mut new_headers: Vec<HeaderInfo> = Vec::new();
    let mut active_new_headers: Vec<HeaderInfo> =
        get_new_active_headers(tips, rest_url.clone(), tree, rpc.clone(), min_fork_height).await;
    new_headers.append(&mut active_new_headers);
    let mut nonactive_new_headers: Vec<HeaderInfo> =
        get_new_nonactive_headers(tips, tree, rpc.clone(), min_fork_height).await;
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
            load_treeinfos_from_db(db.clone(), network.id).await,
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
            task::spawn(async move {
                loop {
                    interval.tick().await;
                    if version_info == String::default() {
                        version_info = match get_version_info(rpc.clone()).await {
                            Ok(version) => version,
                            Err(e) => {
                                error!("Could not fetch getnetworkinfo from node '{}' (id={}) on network '{}' (id={}): {:?}", node.name, node.id, network_cloned.name, network_cloned.id, e);
                                continue;
                            }
                        };
                    };
                    let tips = match get_tips(rpc.clone()).await {
                        Ok(tips) => tips,
                        Err(e) => {
                            error!("Could not fetch chaintips from node '{}' (id={}) on network '{}' (id={}): {:?}", node.name, node.id, network_cloned.name, network_cloned.id, e);
                            continue;
                        }
                    };
                    let new_headers: Vec<HeaderInfo> = get_new_headers(
                        &tips,
                        &tree_clone,
                        rpc.clone(),
                        rest_url.clone(),
                        network_cloned.min_fork_height,
                    )
                    .await;
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

                        write_to_db(&new_headers, db_write, network_cloned.id).await;

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

    /*


    let style_css = warp::get()
        .and(warp::path!("css" / "style.css"))
        .and(warp::fs::file(config.www_path.join("css/style.css")));

    let bootstrap_css = warp::get()
        .and(warp::path!("css" / "bootstrap.min.css"))
        .and(warp::fs::file(
            config.www_path.join("css/bootstrap.min.css"),
        ));

    let blocktree_js = warp::get()
        .and(warp::path!("js" / "blocktree.js"))
        .and(warp::fs::file(config.www_path.join("js/blocktree.js")));

    let logo_png = warp::get()
        .and(warp::path!("img" / "logo.png"))
        .and(warp::fs::file(config.www_path.join("img/logo.png")));
    let node_svg = warp::get()
        .and(warp::path!("img" / "node.svg"))
        .and(warp::fs::file(config.www_path.join("img/node.svg")));

    let d3_js = warp::get()
        .and(warp::path!("js" / "d3.v7.min.js"))
        .and(warp::fs::file(config.www_path.join("js/d3.v7.min.js")));

    */

    let info_json = warp::get()
        .and(warp::path!("api" / "info.json"))
        .and(with_footer(config.footer_html.clone()))
        .and_then(info_response);

    let data_json = warp::get()
        .and(warp::path!("api" / "data.json"))
        .and(with_caches(caches.clone()))
        .and(warp::query::<DataQuery>())
        .and_then(data_response);

    let networks_json = warp::get()
        .and(warp::path!("api" / "networks.json"))
        .and(with_networks(config.networks.clone()))
        .and_then(networks_response);

    let change_sse = warp::path!("api" / "changes").and(warp::get()).map(move || {
        let tipchanges_rx = tipchanges_tx.clone().subscribe();
        let broadcast_stream = BroadcastStream::new(tipchanges_rx);
        let event_stream = broadcast_stream.map(move |d| {
            data_changed_sse(d.unwrap())
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

    /*
        index_html
        .or(blocktree_js)
        .or(logo_png)
        .or(node_svg)
        .or(style_css)
        .or(bootstrap_css)
        .or(d3_js);
*/
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

fn data_changed_sse(network_id: u32) -> Result<Event, Infallible> {
    Ok(warp::sse::Event::default().event("tip_changed").json_data(DataChanged {network_id}).unwrap())
}

fn with_footer(
    footer: String,
) -> impl Filter<Extract = (String,), Error = std::convert::Infallible> + Clone {
    warp::any().map(move || footer.clone())
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
    let root_nodes = tree.externals(petgraph::Direction::Incoming).count();
    info!(
        "done building header tree for network {}: roots={}, tips={}",
        network,
        root_nodes,                                            // root nodes
        tree.externals(petgraph::Direction::Outgoing).count(), // tip nodes
    );
    if root_nodes > 1 {
        warn!(
            "header-tree for network {} has more than one ({}) root!",
            network, root_nodes
        );
    }
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

async fn info_response(footer: String) -> Result<impl warp::Reply, Infallible> {
    Ok(warp::reply::json(&InfoJsonResponse {
        footer: footer,
    }))
}

async fn data_response(caches: Caches, query: DataQuery) -> Result<impl warp::Reply, Infallible> {
    let network: u32 = query.network;

    let caches_locked = caches.lock().await;
    let (header_info_json, node_infos) = caches_locked.get(&network).unwrap().clone();

    Ok(warp::reply::json(&DataJsonResponse {
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

#[derive(Debug)]
enum FetchError {
    TokioJoinError(tokio::task::JoinError),
    BitcoinCoreRPCError(bitcoincore_rpc::Error),
}

impl From<tokio::task::JoinError> for FetchError {
    fn from(e: tokio::task::JoinError) -> Self {
        FetchError::TokioJoinError(e)
    }
}

impl From<bitcoincore_rpc::Error> for FetchError {
    fn from(e: bitcoincore_rpc::Error) -> Self {
        FetchError::BitcoinCoreRPCError(e)
    }
}

async fn get_tips(rpc: Rpc) -> Result<GetChainTipsResult, FetchError> {
    match task::spawn_blocking(move || rpc.get_chain_tips()).await {
        Ok(tips_result) => match tips_result.into() {
            Ok(tips) => Ok(tips),
            Err(e) => Err(e.into()),
        },
        Err(e) => Err(e.into()),
    }
}

async fn get_version_info(rpc: Rpc) -> Result<String, FetchError> {
    match task::spawn_blocking(move || rpc.get_network_info()).await {
        Ok(result) => match result.into() {
            Ok(result) => Ok(result.subversion),
            Err(e) => Err(e.into()),
        },
        Err(e) => Err(e.into()),
    }
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

    let headers: Vec<bitcoin::BlockHeader> = res
        .bytes()
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
