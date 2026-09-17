//! Benchmarks for the database functions in [`fork_observer::db`].
//!
//! They run against an in-memory SQLite database filled with a synthetic
//! headers table of [`ROWS`] rows, which is what a fork-observer instance
//! following mainnet since genesis has.
//!
//! - `update_miner` runs once for every block whose miner was identified. On
//!   startup, a job queues every unidentified block among the interesting
//!   heights, so this can run thousands of times in a row.
//! - `load_treeinfos` runs once per network on startup and builds the header
//!   tree from the database.
//!
//! Run with `cargo bench --bench db`.

use std::sync::Arc;

use corepc_client::bitcoin::blockdata::block::{Header, Version};
use corepc_client::bitcoin::consensus::encode::serialize_hex;
use corepc_client::bitcoin::hashes::Hash;
use corepc_client::bitcoin::{BlockHash, CompactTarget, TxMerkleNode};
use criterion::{criterion_group, criterion_main, BenchmarkId, Criterion, Throughput};
use fork_observer::db::{load_treeinfos, setup_db, update_miner};
use fork_observer::types::Db;
use rusqlite::{params, Connection};
use tokio::runtime::Runtime;
use tokio::sync::Mutex;

const ROWS: usize = 1_000_000;
const NETWORK: u32 = 1;
const SAMPLE_SIZE: usize = 10;

fn header(prev: BlockHash, nonce: u32) -> Header {
    Header {
        version: Version::from_consensus(0x2000_0000),
        prev_blockhash: prev,
        merkle_root: TxMerkleNode::all_zeros(),
        time: 1_600_000_000,
        bits: CompactTarget::from_consensus(0x1d00_ffff),
        nonce,
    }
}

/// Fills an in-memory database with a chain of `rows` headers, and returns it
/// together with the hash of a header in the middle of the chain.
fn synthetic_db(rt: &Runtime, rows: usize) -> (Db, BlockHash) {
    let conn = Connection::open_in_memory().expect("in-memory database");
    let db: Db = Arc::new(Mutex::new(conn));
    rt.block_on(setup_db(db.clone())).expect("setup_db");

    let mut conn = rt.block_on(db.lock());
    let tx = conn.transaction().expect("transaction");
    let mut middle = BlockHash::all_zeros();
    {
        let mut stmt = tx
            .prepare(
                "INSERT INTO headers (height, network, hash, header, miner)
                 VALUES (?1, ?2, ?3, ?4, ?5)",
            )
            .expect("prepare");
        let mut prev = BlockHash::all_zeros();
        for height in 0..rows {
            let h = header(prev, 0);
            let hash = h.block_hash();
            if height == rows / 2 {
                middle = hash;
            }
            stmt.execute(params![
                height as i64,
                NETWORK,
                hash.to_string(),
                serialize_hex(&h),
                "",
            ])
            .expect("insert");
            prev = hash;
        }
    }
    tx.commit().expect("commit");
    drop(conn);
    (db, middle)
}

fn bench_update_miner(c: &mut Criterion) {
    let rt = Runtime::new().unwrap();
    let (db, hash) = synthetic_db(&rt, ROWS);
    let mut group = c.benchmark_group("update_miner");
    group.sample_size(SAMPLE_SIZE);
    group.bench_with_input(BenchmarkId::from_parameter(ROWS), &ROWS, |b, _| {
        b.to_async(&rt).iter(|| async {
            update_miner(db.clone(), NETWORK, &hash, "Some Pool".to_string()).await
        })
    });
    group.finish();
}

fn bench_load_treeinfos(c: &mut Criterion) {
    let rt = Runtime::new().unwrap();
    let (db, _) = synthetic_db(&rt, ROWS);
    let mut group = c.benchmark_group("load_treeinfos");
    group.sample_size(SAMPLE_SIZE);
    group.throughput(Throughput::Elements(ROWS as u64));
    group.bench_with_input(BenchmarkId::from_parameter(ROWS), &ROWS, |b, _| {
        b.to_async(&rt)
            .iter(|| async { load_treeinfos(db.clone(), NETWORK).await })
    });
    group.finish();
}

criterion_group!(benches, bench_update_miner, bench_load_treeinfos);
criterion_main!(benches);
