use std::collections::HashSet;
use std::convert::TryInto;
use std::sync::Arc;
use std::cmp::max;

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

mod types;
mod config;

use types::{
    BlockInfo, BlockInfoJson, BlockInfoKey, JsonResponse, TipInfo, TipInfoJson, TipInfoKey,
};

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
) {
    let mut batch = sled::Batch::default();
    for (k, v) in db_adds.iter() {
        batch.insert(k.as_bytes(), v.as_bytes());
    }

    // remove data about all tips from the db
    for kv_option in db
        .lock()
        .await
        .range(TipInfoKey::new(&BlockHash::default()).as_bytes()..)
    {
        let (k, _) = kv_option.unwrap();
        batch.remove(k);
    }

    for tip in tips.iter() {
        batch.insert(
            TipInfoKey::new(&tip.hash).as_bytes(),
            TipInfo::new(&tip).as_bytes(),
        );
    }

    db.lock().await.apply_batch(batch).unwrap();
}

async fn process_tips(
    tips: &GetChainTipsResult,
    known_tips: &HashSet<BlockHash>,
    rpc: Arc<Client>,
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
            let key = BlockInfoKey::new(active_tip.height - i as u64, &header.block_hash());
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
                let key = BlockInfoKey::new(inactiv_tip.height - i as u64, &header.block_hash());
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
    let config = match config::load_config() {
        Ok(config) => config,
        Err(e) => panic!("Could not load the configuration: {}", e),
    };
    let sled_db: sled::Db = match sled::open(config.database_path.clone()) {
        Ok(db) => db,
        Err(e) => panic!("Could not open the database {:?}: {}", config.database_path, e),
    };
    let db: Db = Arc::new(Mutex::new(sled_db));

    let rpc: Rpc = Arc::new(
        Client::new(
            &config.rpc_url,
            config.rpc_auth,
        )
        .unwrap(),
    );

    let mut interval = time::interval(config.query_interval);

    let db_write = db.clone();
    task::spawn(async move {
        let mut known_tips: HashSet<BlockHash> = HashSet::new();
        loop {
            let db_write = db_write.clone();
            let tips = get_tips(rpc.clone()).await;
            let db_adds = process_tips(&tips, &known_tips, rpc.clone()).await;
            write_to_db(&db_adds, &tips, db_write).await;
            known_tips = tips.iter().map(|tip| tip.hash).collect();
            interval.tick().await;
        }
    });

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
        .and(with_db(db))
        .and_then(block_and_tip_info_response);

    let routes = index_html.or(blocktree_js).or(style_css).or(data_json);

    warp::serve(routes).run(config.address).await;
}

fn with_db(db: Db) -> impl Filter<Extract = (Db,), Error = std::convert::Infallible> + Clone {
    warp::any().map(move || db.clone())
}

async fn block_and_tip_info_response(db: Db) -> Result<impl warp::Reply, Infallible> {
    let start_key = BlockInfoKey::new(0, &BlockHash::default());
    let end_key = BlockInfoKey::new(1000, &BlockHash::default());

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
    }

    let mut tip_infos = vec![];

    for kv_option in db
        .lock()
        .await
        .range(TipInfoKey::new(&BlockHash::default()).as_bytes()..)
    {
        let (_, v_bytes) = kv_option.unwrap();
        let layout: LayoutVerified<&[u8], TipInfo> =
            LayoutVerified::new_unaligned(&*v_bytes).expect("bytes do not fit schema");
        let tip_info: &TipInfo = layout.into_ref();
        tip_infos.push(TipInfoJson::new(tip_info));
    }

    Ok(warp::reply::json(&JsonResponse {
        tip_infos: tip_infos,
        block_infos: block_infos,
    }))
}
