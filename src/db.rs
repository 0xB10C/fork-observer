use std::collections::HashMap;

use petgraph::graph::DiGraph;
use petgraph::graph::NodeIndex;

use corepc_client::bitcoin::BlockHash;

use log::{debug, info, warn};

use crate::error::DbError;
use crate::types::{Db, HeaderInfo, TreeInfo};

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

// Filtering on the network as well lets SQLite use the primary key index
// (network, hash, header) instead of scanning the whole table.
const UPDATE_STMT_HEADER_MINER: &str = "
UPDATE
    headers
SET
    miner = ?1
WHERE
    network = ?2 AND hash = ?3;
";

pub async fn setup_db(db: Db) -> Result<(), DbError> {
    db.lock().await.execute(CREATE_STMT_TABLE_HEADERS, [])?;
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
            [
                &info.height.to_string(),
                &network.to_string(),
                &info.header.block_hash().to_string(),
                &corepc_client::bitcoin::consensus::encode::serialize_hex(&info.header),
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

pub async fn update_miner(
    db: Db,
    network: u32,
    hash: &BlockHash,
    miner: String,
) -> Result<(), DbError> {
    let mut db_locked = db.lock().await;
    let tx = db_locked.transaction()?;

    tx.execute(
        UPDATE_STMT_HEADER_MINER,
        [miner, network.to_string(), hash.to_string()],
    )?;
    tx.commit()?;
    Ok(())
}

// Loads header and tip information for a specified network from the DB and
// builds a header-tree from it.
pub async fn load_treeinfos(db: Db, network: u32) -> Result<TreeInfo, DbError> {
    let header_infos = load_header_infos(db, network).await?;

    let mut tree: DiGraph<HeaderInfo, bool> =
        DiGraph::with_capacity(header_infos.len(), header_infos.len());
    let mut hash_index_map: HashMap<BlockHash, NodeIndex> =
        HashMap::with_capacity(header_infos.len());
    info!("building header tree for network {}..", network);
    // add headers as nodes, remembering each header's parent hash so the
    // edges can be added once all nodes are there
    let mut parents: Vec<(NodeIndex, BlockHash)> = Vec::with_capacity(header_infos.len());
    for h in header_infos {
        let hash = h.header.block_hash();
        let prev_hash = h.header.prev_blockhash;
        let idx = tree.add_node(h);
        hash_index_map.insert(hash, idx);
        parents.push((idx, prev_hash));
    }
    info!(".. added headers from network {}", network);
    // add prev-current block relationships as edges
    for (idx_current, prev_hash) in parents {
        if let Some(idx_prev) = hash_index_map.get(&prev_hash) {
            tree.add_edge(*idx_prev, idx_current, false);
        }
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

    let mut headers: Vec<HeaderInfo> = Vec::new();

    let mut rows = stmt.query([network.to_string()])?;
    while let Some(row) = rows.next()? {
        let header_bytes = hex::decode(row.get_ref(1)?.as_str().map_err(rusqlite::Error::from)?)?;
        let header = corepc_client::bitcoin::consensus::deserialize(&header_bytes)?;
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
