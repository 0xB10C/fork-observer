use std::collections::HashMap;

use petgraph::graph::DiGraph;
use petgraph::graph::NodeIndex;

use bitcoincore_rpc::bitcoin;
use bitcoincore_rpc::bitcoin::BlockHash;

use log::{info, warn};

use crate::types::{HeaderInfo, Db, TreeInfo};
use crate::error::DbError;

const SELECT_STMT_HEADER_HEIGHT: &str = "
SELECT
    height, header
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
    PRIMARY KEY (network, hash, header)
)
";

pub async fn setup_db(db: Db) -> Result<(), DbError> {
    db.lock()
        .await
        .execute(CREATE_STMT_TABLE_HEADERS, [])?;
    Ok(())
}

pub async fn write_to_db(new_headers: &Vec<HeaderInfo>, db: Db, network: u32) -> Result<(), DbError> {
    let mut db_locked = db.lock().await;
    let tx = db_locked.transaction()?;
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
        )?;
    }
    tx.commit()?;
    info!(
        "done inserting {} headers from network {} into the database",
        new_headers.len(),
        network
    );
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
            height: row.get(0)?,
            header: header,
        });
    }

    info!(
        "done loading headers for network {}: headers={}",
        network,
        headers.len()
    );

    Ok(headers)
}
