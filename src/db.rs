use std::collections::HashMap;

use petgraph::graph::DiGraph;
use petgraph::graph::NodeIndex;

use bitcoincore_rpc::bitcoin;
use bitcoincore_rpc::bitcoin::BlockHash;

use log::{debug, info, warn};

use crate::error::DbError;
use crate::types::{ActivityJson, ActivityType, Db, HeaderInfo, TreeInfo};

const SELECT_STMT_HEADER_HEIGHT: &str = "
SELECT
    height, header, miner
FROM
    headers
WHERE
    network = ?1
ORDER BY
    height
    ASC
";

const CREATE_STMT_TABLE_HEADERS: &str = "
CREATE TABLE IF NOT EXISTS headers (
    height     INT,
    network    INT,
    hash       BLOB,
    header     BLOB,
    miner      TEXT,
    PRIMARY KEY (network, hash, header)
)
";

const UPDATE_STMT_HEADER_MINER: &str = "
UPDATE
    headers
SET
    miner = ?1
WHERE
    hash = ?2;
";

const CREATE_STMT_TABLE_ACTIVITY_LOG: &str = "
CREATE TABLE IF NOT EXISTS activity_log (
    id INTEGER PRIMARY KEY,
    timestamp DATETIME DEFAULT CURRENT_TIMESTAMP,
    network INT,
    node_id INT,
    activity_type INT,
    specific_activity_id INT
)
";

const CREATE_STMT_TABLE_ACTIVITY_TIP_CHANGED: &str = "
CREATE TABLE IF NOT EXISTS activity_tip_changed (
    id INTEGER PRIMARY KEY,
    activity_data TEXT
)
";

const CREATE_STMT_TABLE_ACTIVITY_REACHABILITY: &str = "
CREATE TABLE IF NOT EXISTS activity_reachability (
    id INTEGER PRIMARY KEY,
    is_reachable BOOLEAN
)
";

const INSERT_STMT_ACTIVITY_TIP_CHANGED: &str = "
INSERT INTO activity_tip_changed (activity_data) VALUES (?1)
";

const INSERT_STMT_ACTIVITY_REACHABILITY: &str = "
INSERT INTO activity_reachability (is_reachable) VALUES (?1)
";

const INSERT_STMT_ACTIVITY_LOG: &str = "
INSERT INTO activity_log
    (network, node_id, activity_type, specific_activity_id)
    VALUES (?1, ?2, ?3, ?4)
";

const SELECT_STMT_ACTIVITY_LOG: &str = "
SELECT
    a.timestamp,
    a.network,
    a.node_id,
    a.activity_type,
    t.activity_data as tip_data,
    r.is_reachable as reachability_data
FROM
    activity_log a
LEFT JOIN activity_tip_changed t ON a.activity_type = 0 AND a.specific_activity_id = t.id
LEFT JOIN activity_reachability r ON a.activity_type IN (1, 2) AND a.specific_activity_id = r.id
WHERE
    a.network = ?1
ORDER BY
    a.timestamp DESC
LIMIT 100
";

pub async fn setup_db(db: Db) -> Result<(), DbError> {
    db.lock().await.execute(CREATE_STMT_TABLE_HEADERS, [])?;
    let db_locked = db.lock().await;
    db_locked.execute(CREATE_STMT_TABLE_ACTIVITY_LOG, [])?;
    db_locked.execute(CREATE_STMT_TABLE_ACTIVITY_TIP_CHANGED, [])?;
    db_locked.execute(CREATE_STMT_TABLE_ACTIVITY_REACHABILITY, [])?;
    Ok(())
}

pub async fn write_to_db(
    new_headers: &Vec<HeaderInfo>,
    db: Db,
    network: u32,
) -> Result<(), DbError> {
    let mut db_locked = db.lock().await;
    let tx = db_locked.transaction()?;
    debug!(
        "inserting {} headers from network {} into the database..",
        new_headers.len(),
        network
    );
    for info in new_headers {
        tx.execute(
            "INSERT OR IGNORE INTO headers
                   (height, network, hash, header, miner)
                   values (?1, ?2, ?3, ?4, ?5)",
            &[
                &info.height.to_string(),
                &network.to_string(),
                &info.header.block_hash().to_string(),
                &bitcoin::consensus::encode::serialize_hex(&info.header),
                &info.miner,
            ],
        )?;
    }
    tx.commit()?;
    debug!(
        "done inserting {} headers from network {} into the database",
        new_headers.len(),
        network
    );
    Ok(())
}

pub async fn update_miner(db: Db, hash: &BlockHash, miner: String) -> Result<(), DbError> {
    let mut db_locked = db.lock().await;
    let tx = db_locked.transaction()?;

    tx.execute(UPDATE_STMT_HEADER_MINER, [miner, hash.to_string()])?;
    tx.commit()?;
    Ok(())
}

// Loads header and tip information for a specified network from the DB and
// builds a header-tree from it.
pub async fn load_treeinfos(db: Db, network: u32) -> Result<TreeInfo, DbError> {
    let header_infos = load_header_infos(db, network).await?;

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
    Ok((tree, hash_index_map))
}

async fn load_header_infos(db: Db, network: u32) -> Result<Vec<HeaderInfo>, DbError> {
    info!("loading headers for network {} from database..", network);
    let db_locked = db.lock().await;

    let mut stmt = db_locked.prepare(SELECT_STMT_HEADER_HEIGHT)?;

    let mut headers: Vec<HeaderInfo> = vec![];

    let mut rows = stmt.query([network.to_string()])?;
    while let Some(row) = rows.next()? {
        let header_hex: String = row.get(1)?;
        let header_bytes = hex::decode(&header_hex)?;
        let header = bitcoin::consensus::deserialize(&header_bytes)?;
        headers.push(HeaderInfo {
            height: row.get::<_, i64>(0)? as u64,
            header,
            miner: row.get(2)?,
        });
    }

    info!(
        "done loading headers for network {}: headers={}",
        network,
        headers.len()
    );

    Ok(headers)
}

pub async fn log_activity(
    db: Db,
    network: u32,
    node_id: u32,
    activity_type: ActivityType,
    activity_data: Option<String>,
) -> Result<(), DbError> {
    let mut db_locked = db.lock().await;
    let tx = db_locked.transaction()?;

    let specific_id = match activity_type {
        ActivityType::TipChanged => {
            if let Some(data) = activity_data {
                tx.execute(INSERT_STMT_ACTIVITY_TIP_CHANGED, &[&data])?;
                tx.last_insert_rowid()
            } else {
                return Err(DbError::from(rusqlite::Error::InvalidQuery)); // Should have data
            }
        }
        ActivityType::NodeReachable => {
            tx.execute(INSERT_STMT_ACTIVITY_REACHABILITY, [true])?;
            tx.last_insert_rowid()
        }
        ActivityType::NodeUnreachable => {
            tx.execute(INSERT_STMT_ACTIVITY_REACHABILITY, [false])?;
            tx.last_insert_rowid()
        }
    };

    tx.execute(
        INSERT_STMT_ACTIVITY_LOG,
        &[
            &network.to_string(),
            &node_id.to_string(),
            &(activity_type as u32).to_string(),
            &specific_id.to_string(),
        ],
    )?;
    tx.commit()?;
    Ok(())
}

pub async fn get_activities(db: Db, network: u32) -> Result<Vec<ActivityJson>, DbError> {
    let db_locked = db.lock().await;
    let mut stmt = db_locked.prepare(SELECT_STMT_ACTIVITY_LOG)?;

    let mut activities: Vec<ActivityJson> = vec![];

    let mut rows = stmt.query([network.to_string()])?;
    while let Some(row) = rows.next()? {
        let act_type_int: u32 = row.get(3)?;
        let activity_type = match act_type_int {
            0 => ActivityType::TipChanged,
            1 => ActivityType::NodeReachable,
            2 => ActivityType::NodeUnreachable,
            _ => continue, // Invalid unknown type in db
        };

        let activity_data = match activity_type {
            ActivityType::TipChanged => row.get::<_, Option<String>>(4)?, // tip_data
            ActivityType::NodeReachable | ActivityType::NodeUnreachable => {
                let reachable: Option<bool> = row.get(5)?; // reachability_data
                reachable.map(|r| r.to_string())
            }
        };

        activities.push(ActivityJson {
            timestamp: row.get(0)?,
            network: row.get(1)?,
            node_id: row.get(2)?,
            activity_type,
            activity_data,
        });
    }

    Ok(activities)
}
