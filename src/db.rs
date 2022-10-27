use crate::types::{HeaderInfo,Db};

use bitcoincore_rpc::bitcoin;

use log::info;

pub async fn setup_db(db: Db) {
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

pub async fn write_to_db(new_headers: &Vec<HeaderInfo>, db: Db, network: u32) {
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
