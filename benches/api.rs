//! Benchmarks for the API request path.
//!
//! These drive the real routes from [`fork_observer::api::build_routes`] with a
//! synthetic cache, so they measure what a browser hitting the API actually
//! pays: the cache lock, the response building and the serialization.
//!
//! The default cache shape mirrors what fork.observer serves for mainnet
//! (~100 headers, 34 nodes, ~12 chain tips each, which is ~95 KB of JSON, plus
//! a full stale block list at ~13 KB), so the numbers are comparable to
//! production.
//!
//! Run with `cargo bench`, compare against the previous run with
//! `cargo bench -- --baseline <name>`.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use criterion::{criterion_group, criterion_main, BatchSize, Criterion, Throughput};
use fork_observer::api::build_routes;
use fork_observer::cache::{update_cache, CacheUpdate, MAX_STALE_BLOCKS};
use fork_observer::config::{Config, Network, PoolIdentification};
use fork_observer::types::{
    Cache, Caches, ChainTip, ChainTipStatus, HeaderInfoJson, NetworkJson, NodeDataJson,
    StaleBlockJson,
};
use tokio::runtime::Runtime;
use tokio::sync::{broadcast, Mutex};
use warp::Filter;
use warp::Rejection;
use warp::Reply;

/// The cache shape we benchmark with, taken from what fork.observer serves for
/// mainnet.
const HEADERS: usize = 100;
const NODES: usize = 34;
const TIPS_PER_NODE: usize = 12;
/// How many requests the concurrent benchmarks have in flight at once.
const CONCURRENCY: usize = 64;

fn fake_hash(seed: usize) -> String {
    format!("{:064x}", seed)
}

fn fake_headers(count: usize) -> Vec<HeaderInfoJson> {
    (0..count)
        .map(|i| HeaderInfoJson {
            id: i,
            prev_id: i.saturating_sub(1),
            height: 900_000 + i as u64,
            hash: fake_hash(i),
            version: 574447616,
            prev_blockhash: fake_hash(i.saturating_sub(1)),
            merkle_root: fake_hash(i + 1_000_000),
            time: 1776896581 + i as u32 * 600,
            bits: 386012009,
            difficulty_int: 135594876535256,
            nonce: 682214962,
            miner: "MARA Pool".to_string(),
        })
        .collect()
}

fn fake_tips(count: usize) -> Vec<ChainTip> {
    (0..count)
        .map(|i| ChainTip {
            height: 900_000 + i as u64,
            hash: fake_hash(i),
            branchlen: 0,
            status: if i == 0 {
                ChainTipStatus::Active
            } else {
                ChainTipStatus::ValidFork
            },
        })
        .collect()
}

fn fake_nodes(count: usize) -> BTreeMap<u32, NodeDataJson> {
    (0..count)
        .map(|i| {
            let id = i as u32;
            let info = fork_observer::backend::NodeInfo {
                id,
                name: format!("Node {}", id),
                description: "a benchmark node".to_string(),
                implementation: "Bitcoin Core".to_string(),
            };
            (
                id,
                NodeDataJson::new(
                    info,
                    &fake_tips(TIPS_PER_NODE),
                    "/Satoshi:29.0.0/".to_string(),
                    1776896581,
                    true,
                ),
            )
        })
        .collect()
}

fn fake_cache() -> Cache {
    Cache {
        header_infos_json: fake_headers(HEADERS),
        node_data: fake_nodes(NODES),
        forks: vec![],
        // The stale block list runs at its cap on a busy network, and it is
        // a response of its own, so the fixture holds a full one.
        stale_blocks: (0..MAX_STALE_BLOCKS)
            .map(|i| StaleBlockJson {
                height: 900_000 + i as u64,
                hash: fake_hash(i + 5_000_000),
                header: "00".repeat(80),
            })
            .collect(),
        block_cache: HashMap::new(),
        recent_miners: vec![],
    }
}

/// A caches map holding `networks` identically shaped networks (ids 0..n).
fn fake_caches(networks: usize) -> Caches {
    let mut map = BTreeMap::new();
    for id in 0..networks as u32 {
        map.insert(id, fake_cache());
    }
    Arc::new(Mutex::new(map))
}

fn fake_network(id: u32) -> Network {
    Network {
        id,
        description: "a benchmark network".to_string(),
        name: format!("net{}", id),
        slug: format!("net{}", id),
        min_fork_height: 0,
        max_interesting_heights: 100,
        nodes: vec![],
        remote_forkobservers: vec![],
        pool_identification: PoolIdentification::default(),
        countdown: None,
        activity_retention_days: None,
        activity_log_node_ids: BTreeSet::new(),
    }
}

fn fake_config(networks: Vec<Network>) -> Config {
    Config {
        database_path: std::path::PathBuf::new(),
        www_path: std::path::PathBuf::new(),
        query_interval: Duration::from_secs(1),
        address: "127.0.0.1:0".parse().unwrap(),
        networks,
        footer_html: String::new(),
        rss_base_url: "https://example.com".to_string(),
        activity: None,
    }
}

/// The real application routes, built over `caches`.
fn routes(
    caches: &Caches,
    network_count: usize,
) -> impl Filter<Extract = impl Reply, Error = Rejection> + Clone {
    let networks: Vec<Network> = (0..network_count as u32).map(fake_network).collect();
    let network_infos: Vec<NetworkJson> = networks.iter().map(NetworkJson::new).collect();
    let config = fake_config(networks);
    let (cache_changed_tx, _rx) = broadcast::channel(16);
    build_routes(&network_infos, &config, caches, cache_changed_tx, &None)
}

fn multi_thread_runtime() -> Runtime {
    tokio::runtime::Builder::new_multi_thread()
        .worker_threads(4)
        .enable_all()
        .build()
        .unwrap()
}

/// One request against the routes, returning the response body size.
async fn request<F>(route: &F, path: &str) -> usize
where
    F: Filter<Error = Rejection> + 'static,
    F::Extract: Reply + Send,
{
    warp::test::request()
        .path(path)
        .reply(route)
        .await
        .body()
        .len()
}

/// `CONCURRENCY` requests in flight on separate tasks, so they contend for the
/// cache the way parallel clients do.
async fn concurrent_requests<F>(route: &F, path: &str)
where
    F: Filter<Error = Rejection> + Clone + Send + Sync + 'static,
    F::Extract: Reply + Send,
{
    let handles: Vec<_> = (0..CONCURRENCY)
        .map(|_| {
            let route = route.clone();
            let path = path.to_string();
            tokio::spawn(async move { request(&route, &path).await })
        })
        .collect();
    for handle in handles {
        handle.await.unwrap();
    }
}

fn bench_data_json(c: &mut Criterion) {
    let rt = multi_thread_runtime();
    let caches = fake_caches(1);
    let route = routes(&caches, 1);

    // Sanity check (and a useful number to see in the output): the payload size
    // these benchmarks are built around.
    let size = rt.block_on(request(&route, "/api/0/data.json"));

    let mut group = c.benchmark_group("data.json");
    group.throughput(Throughput::Bytes(size as u64));
    group.bench_function("serial", |b| {
        b.to_async(&rt).iter(|| request(&route, "/api/0/data.json"));
    });
    group.finish();
}

fn bench_data_json_concurrent(c: &mut Criterion) {
    let rt = multi_thread_runtime();
    let caches = fake_caches(1);
    let route = routes(&caches, 1);

    let mut group = c.benchmark_group("data.json");
    group.throughput(Throughput::Elements(CONCURRENCY as u64));
    group.bench_function("concurrent", |b| {
        b.to_async(&rt)
            .iter(|| concurrent_requests(&route, "/api/0/data.json"));
    });
    group.finish();
}

/// Readers on network 0 while network 1 is being written to. A cache that is
/// shared across networks makes these block each other; a per-network cache
/// should not.
fn bench_data_json_while_other_network_writes(c: &mut Criterion) {
    let rt = multi_thread_runtime();
    let caches = fake_caches(2);
    let route = routes(&caches, 2);

    let stop = Arc::new(AtomicBool::new(false));
    let writer = {
        let caches = caches.clone();
        let stop = stop.clone();
        rt.spawn(async move {
            let (tx, _rx) = broadcast::channel(16);
            let tips = fake_tips(TIPS_PER_NODE);
            while !stop.load(Ordering::Relaxed) {
                update_cache(
                    &caches,
                    1,
                    CacheUpdate::NodeTips {
                        node_id: 0,
                        tips: tips.clone(),
                    },
                    &tx,
                )
                .await;
                tokio::task::yield_now().await;
            }
        })
    };

    let mut group = c.benchmark_group("data.json");
    group.throughput(Throughput::Elements(CONCURRENCY as u64));
    group.bench_function("concurrent_while_other_network_writes", |b| {
        b.to_async(&rt)
            .iter(|| concurrent_requests(&route, "/api/0/data.json"));
    });
    group.finish();

    stop.store(true, Ordering::Relaxed);
    rt.block_on(async { writer.await.unwrap() });
}

fn bench_other_endpoints(c: &mut Criterion) {
    let rt = multi_thread_runtime();
    let caches = fake_caches(1);
    let route = routes(&caches, 1);

    let mut group = c.benchmark_group("endpoints");
    group.bench_function("stale.json", |b| {
        b.to_async(&rt)
            .iter(|| request(&route, "/api/0/stale.json"));
    });
    group.bench_function("networks.json", |b| {
        b.to_async(&rt)
            .iter(|| request(&route, "/api/networks.json"));
    });
    group.bench_function("rss/forks.xml", |b| {
        b.to_async(&rt).iter(|| request(&route, "/rss/0/forks.xml"));
    });
    group.finish();
}

/// The write path: what a node task pays to push a change into the cache. This
/// runs once per node per new block, so it matters much less than the read
/// path, but it holds the same lock the readers need.
fn bench_update_cache(c: &mut Criterion) {
    let rt = multi_thread_runtime();
    let (tx, _rx) = broadcast::channel(16);

    let mut group = c.benchmark_group("update_cache");
    group.bench_function("node_tips", |b| {
        let caches = fake_caches(1);
        let tips = fake_tips(TIPS_PER_NODE);
        b.to_async(&rt).iter(|| {
            let tips = tips.clone();
            async {
                update_cache(&caches, 0, CacheUpdate::NodeTips { node_id: 0, tips }, &tx).await
            }
        });
    });
    group.bench_function("header_tree", |b| {
        let caches = fake_caches(1);
        b.to_async(&rt).iter_batched(
            || (fake_headers(HEADERS), fake_cache().stale_blocks),
            |(header_infos_json, stale_blocks)| async {
                update_cache(
                    &caches,
                    0,
                    CacheUpdate::HeaderTree {
                        header_infos_json,
                        forks: vec![],
                        stale_blocks,
                    },
                    &tx,
                )
                .await
            },
            BatchSize::SmallInput,
        );
    });
    group.finish();
}

criterion_group!(
    benches,
    bench_data_json,
    bench_data_json_concurrent,
    bench_data_json_while_other_network_writes,
    bench_other_endpoints,
    bench_update_cache,
);
criterion_main!(benches);
