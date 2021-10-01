use std::cmp::max;
use std::collections::HashSet;
use std::convert::TryInto;
use std::sync::Arc;
// TODO: remove?
use std::convert::Infallible;

use tokio::sync::Mutex;
use tokio::task;
use tokio::time;

use warp::Filter;

use zerocopy::{byteorder::U64, AsBytes, LayoutVerified};

use bitcoincore_rpc::bitcoin::hash_types::BlockHash;
use bitcoincore_rpc::json::{GetChainTipsResult, GetChainTipsResultStatus};
use bitcoincore_rpc::{Client, RpcApi};

mod config;
mod types;

use types::{
    BlockInfo, BlockInfoJson, BlockInfoKey, DataJsonResponse, DataQuery, NetworkInfo,
    NetworkInfoKey, NetworkJson, NetworksJsonResponse, NodeInfo, NodeInfoKey, NodeJson, TipInfo,
    TipInfoJson, TipInfoKey, ValueError,
};

use config::{Network, Node};

async fn get_tips(rpc: Rpc) -> GetChainTipsResult {
    let res = task::spawn_blocking(move || {
        return rpc.get_chain_tips().unwrap();
    })
    .await;
    res.unwrap()
}

async fn write_to_db(
    db_adds: &HashSet<(BlockInfoKey, BlockInfo)>,
    tips: &GetChainTipsResult,
    db: Db,
    network_id: u32,
    node_id: u8,
) {
    let mut batch = sled::Batch::default();

    println!(
        "DEBUG: write to db: db_adds={}, tips={}",
        db_adds.len(),
        tips.len()
    );

    // insert blocks
    for (k, v) in db_adds.iter() {
        println!(
            "DEBUG: inserting block for net={} at height={}",
            k.network, k.height
        );
        batch.insert(k.as_bytes(), v.as_bytes());
    }

    // remove the tip data for this node from the db
    for kv_option in db.lock().await.range(
        TipInfoKey::new(network_id, &BlockHash::default()).as_bytes()
            ..TipInfoKey::new(network_id, &types::max_block_hash()).as_bytes(),
    ) {
        let (k, _) = kv_option.unwrap();
        println!("DEBUG: removing tip");
        batch.remove(k);
    }

    // insert tips
    for tip in tips.iter() {
        batch.insert(
            TipInfoKey::new(network_id, &tip.hash).as_bytes(),
            TipInfo::new(&tip, node_id).as_bytes(),
        );
    }

    db.lock().await.apply_batch(batch).unwrap();
}

async fn replace_networks_and_nodes_in_db(
    networks: Vec<Network>,
    db: &Db,
) -> Result<(), ValueError> {
    let mut batch = sled::Batch::default();

    // remove all networks from the db
    for kv_option in db.lock().await.range(NetworkInfoKey::new(0).as_bytes()..) {
        let (k, _) = kv_option.unwrap();
        batch.remove(k);
    }

    // remove all nodes from the db
    for kv_option in db.lock().await.range(
        NodeInfoKey::new(u32::MIN, u8::MIN).as_bytes()
            ..NodeInfoKey::new(u32::MAX, u8::MAX).as_bytes(),
    ) {
        let (k, _) = kv_option.unwrap();
        batch.remove(k);
    }

    for network in networks.iter() {
        batch.insert(
            NetworkInfoKey::new(network.id).as_bytes(),
            NetworkInfo::new(network.id, &network.name, &network.description)?.as_bytes(),
        );
        for node in network.nodes.iter() {
            batch.insert(
                NodeInfoKey::new(network.id, node.id).as_bytes(),
                NodeInfo::new(node.id, &node.name, &node.description)?.as_bytes(),
            );
        }
    }

    db.lock().await.apply_batch(batch).unwrap();
    Ok(())
}

async fn process_tips(
    tips: &GetChainTipsResult,
    known_tips: &HashSet<BlockHash>,
    rpc: Arc<Client>,
    network_id: u32,
) -> HashSet<(BlockInfoKey, BlockInfo)> {
    let mut db_adds: HashSet<(BlockInfoKey, BlockInfo)> = HashSet::new();

    let min_height = tips.iter().min_by_key(|tip| tip.height).unwrap().height;
    let scan_start_height = max(min_height as i64 - 5, 0);

    let active_tip = tips
        .iter()
        .filter(|tip| tip.status == GetChainTipsResultStatus::Active)
        .last()
        .unwrap();
    if !known_tips.contains(&active_tip.hash) {
        let mut next_header = active_tip.hash;
        for i in 0..=(active_tip.height as i64 - scan_start_height) {
            if known_tips.contains(&next_header) {
                break;
            }

            let header = rpc.get_block_header(&next_header).unwrap();
            println!("DEBUG: got block header {}", next_header);
            let key = BlockInfoKey::new(
                active_tip.height - i as u64,
                &header.block_hash(),
                network_id,
            );
            let header_bytes = bitcoincore_rpc::bitcoin::consensus::serialize(&header);
            let value = BlockInfo {
                height: U64::new(active_tip.height - i as u64),
                header: header_bytes[0..80].try_into().unwrap(),
            };

            db_adds.insert((key, value));
            next_header = header.prev_blockhash;
        }
    }

    for inactiv_tip in tips
        .iter()
        .filter(|tip| tip.status != GetChainTipsResultStatus::Active)
    {
        if !known_tips.contains(&inactiv_tip.hash) {
            let mut next_header = inactiv_tip.hash;
            for i in 0..=inactiv_tip.branch_length + 1 {
                if known_tips.contains(&next_header) {
                    break;
                }

                let header = rpc.get_block_header(&next_header).unwrap();
                println!("DEBUG: got block header {}", next_header);
                let key = BlockInfoKey::new(
                    inactiv_tip.height - i as u64,
                    &header.block_hash(),
                    network_id,
                );
                let header_bytes = bitcoincore_rpc::bitcoin::consensus::serialize(&header);
                let value = BlockInfo {
                    height: U64::new(inactiv_tip.height - i as u64),
                    header: header_bytes[0..80].try_into().unwrap(),
                };

                db_adds.insert((key, value));
                next_header = header.prev_blockhash;
            }
        }
    }

    return db_adds;
}

type Db = Arc<Mutex<sled::Db>>;
type Rpc = Arc<Client>;

#[tokio::main]
async fn main() {
    let config: config::Config = match config::load_config() {
        Ok(config) => config,
        Err(e) => panic!("Could not load the configuration: {}", e),
    };

    if config.networks.is_empty() {
        panic!("No networks and nodes defined in the configuration.");
    }

    let sled_db: sled::Db = match sled::open(config.database_path.clone()) {
        Ok(db) => db,
        Err(e) => panic!(
            "Could not open the database {:?}: {}",
            config.database_path, e
        ),
    };
    let db: Db = Arc::new(Mutex::new(sled_db));

    if let Err(e) = replace_networks_and_nodes_in_db(config.networks.clone(), &db).await {
        panic!(
            "Could not update the information about networks in the db: {}",
            e
        )
    }

    for network in config.networks.iter().cloned() {
        println!(
            "Network {} with {} nodes",
            network.name,
            network.nodes.len()
        );

        for node in network.nodes.iter().cloned() {
            let rpc: Rpc = Arc::new(Client::new(&node.rpc_url, node.rpc_auth.clone()).unwrap());
            let mut interval = time::interval(config.query_interval);
            let db_write = db.clone();
            let network_cloned = network.clone();
            task::spawn(async move {
                println!("DEBUG: Task for node {} spawned", node.name);
                let mut known_tips: HashSet<BlockHash> = HashSet::new();
                loop {
                    println!("DEBUG: in loop for node {}", node.name);
                    let db_write = db_write.clone();
                    println!("DEBUG: pre gettips {}", node.name);
                    let tips = get_tips(rpc.clone()).await;
                    println!("DEBUG: pre process_tips {}", node.name);
                    let db_adds =
                        process_tips(&tips, &known_tips, rpc.clone(), network_cloned.id).await;
                    println!(
                        "DEBUG: pre-write-to-db {}, dbadds={}",
                        node.name,
                        db_adds.len()
                    );
                    write_to_db(&db_adds, &tips, db_write, network_cloned.id, node.id).await;
                    println!("DEBUG: post-write-to-db {}", node.name);
                    known_tips = tips.iter().map(|tip| tip.hash).collect();
                    println!(
                        "Node {} on network {} has {} tips",
                        node.name,
                        network_cloned.name,
                        tips.len()
                    );
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

    let data_json = warp::get()
        .and(warp::path("data.json"))
        .and(with_db(db.clone()))
        .and(warp::query::<DataQuery>())
        .and_then(data_response);

    let networks_json = warp::get()
        .and(warp::path("networks.json"))
        .and(with_db(db.clone()))
        .and_then(networks_response);

    let routes = index_html
        .or(blocktree_js)
        .or(style_css)
        .or(data_json)
        .or(networks_json);

    warp::serve(routes).run(config.address).await;
}

fn with_db(db: Db) -> impl Filter<Extract = (Db,), Error = std::convert::Infallible> + Clone {
    warp::any().map(move || db.clone())
}

async fn data_response(db: Db, query: DataQuery) -> Result<impl warp::Reply, Infallible> {
    let network: u32 = query.network;

    let start_key = BlockInfoKey::new(u64::MIN, &BlockHash::default(), network);
    let end_key = BlockInfoKey::new(u64::MAX, &types::max_block_hash(), network);

    println!("DEBUG: Preparing DATA response for network {}", network);

    let mut block_infos = vec![];

    for kv_option in db
        .lock()
        .await
        .range(start_key.as_bytes()..end_key.as_bytes())
    {
        let (_, v_bytes) = kv_option.unwrap();
        let layout: LayoutVerified<&[u8], BlockInfo> =
            LayoutVerified::new_unaligned(&*v_bytes).expect("bytes do not fit schema");
        let block_info: &BlockInfo = layout.into_ref();
        block_infos.push(BlockInfoJson::new(block_info));
        println!("BlockInfo pushed {}", block_info.height);
    }

    let mut tip_infos = vec![];

    for kv_option in db.lock().await.range(
        TipInfoKey::new(network, &BlockHash::default()).as_bytes()
            ..TipInfoKey::new(network, &types::max_block_hash()).as_bytes(),
    ) {
        let (k_bytes, v_bytes) = kv_option.unwrap();
        let keylayout: LayoutVerified<&[u8], TipInfoKey> =
            LayoutVerified::new_unaligned(&*k_bytes).expect("bytes do not fit schema");
        let layout: LayoutVerified<&[u8], TipInfo> =
            LayoutVerified::new_unaligned(&*v_bytes).expect("bytes do not fit schema");
        let tip_info: &TipInfo = layout.into_ref();
        let tip_infokey: &TipInfoKey = keylayout.into_ref();
        println!("Network {}", tip_infokey.network);
        tip_infos.push(TipInfoJson::new(tip_info));
    }

    let nodes: Vec<NodeJson> = db
        .lock()
        .await
        .range(
            NodeInfoKey::new(network, u8::MIN).as_bytes()
                ..NodeInfoKey::new(network, u8::MAX).as_bytes(),
        )
        .map(|kv_option| {
            let (_, v_bytes) = kv_option.unwrap();
            let layout: LayoutVerified<&[u8], NodeInfo> =
                LayoutVerified::new_unaligned(&*v_bytes).expect("bytes do not fit schema");
            let node_info: &NodeInfo = layout.into_ref();
            NodeJson::new(node_info)
        })
        .collect();

    Ok(warp::reply::json(&DataJsonResponse {
        tip_infos: tip_infos,
        block_infos: block_infos,
        nodes,
    }))
}

async fn networks_response(db: Db) -> Result<impl warp::Reply, Infallible> {
    let start_key = NetworkInfoKey::new(u32::MIN);
    let end_key = NetworkInfoKey::new(u32::MAX);

    let mut network_infos = vec![];

    for kv_option in db
        .lock()
        .await
        .range(start_key.as_bytes()..end_key.as_bytes())
    {
        let (_, v_bytes) = kv_option.unwrap();
        println!("v bytes len {}", v_bytes.len());
        let layout: LayoutVerified<&[u8], NetworkInfo> =
            LayoutVerified::new_unaligned(&*v_bytes).expect("bytes do not fit schema");
        let network_info: &NetworkInfo = layout.into_ref();
        network_infos.push(NetworkJson::new(network_info));
    }

    Ok(warp::reply::json(&NetworksJsonResponse {
        networks: network_infos,
    }))
}
