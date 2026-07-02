use crate::config::{Config, Network};
use crate::rss;
use crate::types::{
    Caches, DataChanged, DataJsonResponse, InfoJsonResponse, NetworkJson, NetworksJsonResponse,
    StaleBlocksJsonResponse,
};
use corepc_client::bitcoin::BlockHash;
use futures_util::StreamExt;
use log::{error, warn};
use std::convert::Infallible;
use std::str::FromStr;
use tokio::sync::broadcast::Sender;
use tokio_stream::wrappers::BroadcastStream;
use warp::{sse::Event, Filter};

pub fn build_routes(
    network_infos: &Vec<NetworkJson>,
    config: &Config,
    caches: &Caches,
    cache_changed_tx_warp: Sender<u32>,
) -> impl Filter<Extract = impl warp::Reply, Error = warp::Rejection> + Clone {
    let www_dir = warp::get()
        .and(warp::path("static"))
        .and(warp::fs::dir(config.www_path.clone()));
    let index_html = warp::get()
        .and(warp::path::end())
        .and(warp::fs::file(config.www_path.join("index.html")));
    let fullscreen_html = warp::get()
        .and(warp::path!("fullscreen"))
        .and(warp::fs::file(config.www_path.join("fullscreen.html")));

    let info_json = warp::get()
        .and(warp::path!("api" / "info.json"))
        .and(with_footer(config.footer_html.clone()))
        .and_then(info_response);

    let data_json = warp::get()
        .and(warp::path!("api" / u32 / "data.json"))
        .and(with_caches(caches.clone()))
        .and_then(data_response);

    let stale_json = warp::get()
        .and(warp::path!("api" / u32 / "stale.json"))
        .and(with_caches(caches.clone()))
        .and_then(stale_blocks_response);

    let block_hex = warp::get()
        .and(warp::path!("api" / u32 / "block" / String / "hex"))
        .and(with_caches(caches.clone()))
        .and(with_config_networks(config.networks.clone()))
        .and_then(|network_id, hash, caches, networks| {
            block_response(network_id, hash, true, caches, networks)
        });

    let block_bin = warp::get()
        .and(warp::path!("api" / u32 / "block" / String / "bin"))
        .and(with_caches(caches.clone()))
        .and(with_config_networks(config.networks.clone()))
        .and_then(|network_id, hash, caches, networks| {
            block_response(network_id, hash, false, caches, networks)
        });

    let forks_rss = warp::get()
        .and(warp::path!("rss" / u32 / "forks.xml"))
        .and(with_caches(caches.clone()))
        .and(with_networks(network_infos.clone()))
        .and(rss::with_rss_base_url(config.rss_base_url.clone()))
        .and_then(rss::forks_response);

    let invalid_blocks_rss = warp::get()
        .and(warp::path!("rss" / u32 / "invalid.xml"))
        .and(with_caches(caches.clone()))
        .and(with_networks(network_infos.clone()))
        .and(rss::with_rss_base_url(config.rss_base_url.clone()))
        .and_then(rss::invalid_blocks_response);

    let lagging_nodes_rss = warp::get()
        .and(warp::path!("rss" / u32 / "lagging.xml"))
        .and(with_caches(caches.clone()))
        .and(with_networks(network_infos.clone()))
        .and(rss::with_rss_base_url(config.rss_base_url.clone()))
        .and_then(rss::lagging_nodes_response);

    let unreachable_nodes_rss = warp::get()
        .and(warp::path!("rss" / u32 / "unreachable.xml"))
        .and(with_caches(caches.clone()))
        .and(with_networks(network_infos.clone()))
        .and(rss::with_rss_base_url(config.rss_base_url.clone()))
        .and_then(rss::unreachable_nodes_response);

    let networks_json = warp::get()
        .and(warp::path!("api" / "networks.json"))
        .and(with_networks(network_infos.to_vec()))
        .and_then(networks_response);

    // Friendly network URLs: `/testnet4` redirects to `/?network=testnet4`,
    // which the frontend then resolves. Unknown slugs are rejected so they fall
    // through to a 404.
    let slug_redirect = warp::get()
        .and(warp::path::param::<String>())
        .and(warp::path::end())
        .and(with_slugs(
            network_infos.iter().map(|n| n.slug.clone()).collect(),
        ))
        .and_then(slug_redirect_response);

    let change_sse = warp::path!("api" / "changes")
        .and(warp::get())
        .map(move || {
            let changes_tx = cache_changed_tx_warp.subscribe();
            let broadcast_stream = BroadcastStream::new(changes_tx);
            let event_stream = broadcast_stream.map(move |d| match d {
                Ok(d) => data_changed_sse(d),
                Err(e) => {
                    error!("Could not SSE notify about tip changed event: {}", e);
                    data_changed_sse(u32::MAX)
                }
            });
            let stream = warp::sse::keep_alive().stream(event_stream);
            warp::sse::reply(stream)
        });

    www_dir
        .or(index_html)
        .or(fullscreen_html)
        .or(data_json)
        .or(stale_json)
        .or(block_hex)
        .or(block_bin)
        .or(info_json)
        .or(networks_json)
        .or(change_sse)
        .or(forks_rss)
        .or(lagging_nodes_rss)
        .or(unreachable_nodes_rss)
        .or(invalid_blocks_rss)
        .or(slug_redirect)
}

pub async fn info_response(footer: String) -> Result<impl warp::Reply, Infallible> {
    Ok(warp::reply::json(&InfoJsonResponse { footer }))
}

pub async fn data_response(network: u32, caches: Caches) -> Result<impl warp::Reply, Infallible> {
    let caches_locked = caches.lock().await;
    match caches_locked.get(&network) {
        Some(cache) => Ok(warp::reply::json(&DataJsonResponse {
            header_infos: cache.header_infos_json.clone(),
            nodes: cache.node_data.values().cloned().collect(),
        })),
        None => Ok(warp::reply::json(&DataJsonResponse {
            header_infos: vec![],
            nodes: vec![],
        })),
    }
}

pub async fn stale_blocks_response(
    network: u32,
    caches: Caches,
) -> Result<impl warp::Reply, Infallible> {
    let caches_locked = caches.lock().await;
    let stale_blocks = match caches_locked.get(&network) {
        Some(cache) => cache.stale_blocks.clone(),
        None => vec![],
    };
    Ok(warp::reply::json(&StaleBlocksJsonResponse { stale_blocks }))
}

/// Serves a full block by its hash as hex (`as_hex = true`) or raw binary.
/// We try every node we are connected to until one returns the block. We cache
/// this response for a while.
///
/// Only blocks that this instance currently considers stale (i.e. present in the
/// cached stale-blocks list) are served; any other hash returns a 404. This
/// keeps the endpoint from acting as a general-purpose block proxy.
pub async fn block_response(
    network_id: u32,
    hash: String,
    as_hex: bool,
    caches: Caches,
    networks: Vec<Network>,
) -> Result<impl warp::Reply, Infallible> {
    let block_hash = match BlockHash::from_str(&hash) {
        Ok(h) => h,
        Err(e) => {
            return Ok(warp::http::Response::builder()
                .status(400)
                .header("content-type", "text/plain")
                .body(format!("Invalid block hash '{}': {}", hash, e).into_bytes())
                .unwrap());
        }
    };

    // We only serve blocks the instance considers stale. While holding the lock
    // we also read any cached entry:
    //   Some(Some(bytes)) - the block, already fetched and cached
    //   Some(None)        - we already asked every node and none had it
    //   None              - not yet fetched
    let cached = {
        let caches_locked = caches.lock().await;
        match caches_locked.get(&network_id) {
            Some(cache) => {
                if !cache
                    .stale_blocks
                    .iter()
                    .any(|b| b.hash == block_hash.to_string())
                {
                    return Ok(not_a_stale_block(block_hash, network_id));
                }
                cache.block_cache.get(&block_hash).cloned()
            }
            None => return Ok(not_a_stale_block(block_hash, network_id)),
        }
    };

    let bytes = match cached {
        // Cached hit.
        Some(Some(bytes)) => bytes,
        // We already tried every node and none had it. Don't retry.
        Some(None) => return Ok(block_not_available(block_hash)),
        // Not fetched yet: try every node until one returns the block, then
        // cache the outcome (the bytes, or `None` if no node had it).
        None => {
            let network = match networks.iter().find(|n| n.id == network_id) {
                Some(n) => n,
                None => return Ok(block_not_available(block_hash)),
            };

            let mut fetched: Option<Vec<u8>> = None;
            for node in network.nodes.iter() {
                match node.block(&block_hash).await {
                    Ok(bytes) => {
                        fetched = Some(bytes);
                        break;
                    }
                    Err(e) => {
                        warn!(
                            "Could not fetch block {} from node {} on network {}: {}",
                            block_hash,
                            node.info(),
                            network_id,
                            e
                        );
                    }
                }
            }

            // Cache the result (only while the block is still stale, so we don't
            // reintroduce an entry that was concurrently pruned).
            {
                let mut caches_locked = caches.lock().await;
                if let Some(cache) = caches_locked.get_mut(&network_id) {
                    if cache
                        .stale_blocks
                        .iter()
                        .any(|b| b.hash == block_hash.to_string())
                    {
                        cache.block_cache.insert(block_hash, fetched.clone());
                    }
                }
            }

            match fetched {
                Some(bytes) => bytes,
                None => return Ok(block_not_available(block_hash)),
            }
        }
    };

    if as_hex {
        return Ok(warp::http::Response::builder()
            .header("content-type", "text/plain")
            .body(hex::encode(&bytes).into_bytes())
            .unwrap());
    }
    Ok(warp::http::Response::builder()
        .header("content-type", "application/octet-stream")
        .body(bytes)
        .unwrap())
}

fn not_a_stale_block(block_hash: BlockHash, network_id: u32) -> warp::http::Response<Vec<u8>> {
    warp::http::Response::builder()
        .status(404)
        .header("content-type", "text/plain")
        .body(
            format!(
                "Block {} is not a known stale block on network {}.",
                block_hash, network_id
            )
            .into_bytes(),
        )
        .unwrap()
}

fn block_not_available(block_hash: BlockHash) -> warp::http::Response<Vec<u8>> {
    warp::http::Response::builder()
        .status(404)
        .header("content-type", "text/plain")
        .body(format!("Could not fetch block {} from any node.", block_hash).into_bytes())
        .unwrap()
}

/// Redirects a friendly network URL (`/<slug>`) to the query-parameter form the
/// frontend understands (`?network=<slug>`). Unknown slugs are rejected so warp
/// continues matching and eventually returns a 404.
///
/// The `Location` is a relative reference (`./?network=<slug>`) so it resolves
/// against the request's directory. This keeps the redirect correct both when
/// the app is served from the site root and when it is mounted under a subpath
/// (e.g. `example.com/forks/`), matching the relative URLs the frontend uses.
pub async fn slug_redirect_response(
    slug: String,
    slugs: Vec<String>,
) -> Result<impl warp::Reply, warp::Rejection> {
    if slugs.iter().any(|s| *s == slug) {
        Ok(warp::http::Response::builder()
            .status(warp::http::StatusCode::FOUND)
            .header("location", format!("./?network={}", slug))
            .body(Vec::new())
            .unwrap())
    } else {
        Err(warp::reject::not_found())
    }
}

pub async fn networks_response(
    network_infos: Vec<NetworkJson>,
) -> Result<impl warp::Reply, Infallible> {
    Ok(warp::reply::json(&NetworksJsonResponse {
        networks: network_infos,
    }))
}

pub fn data_changed_sse(network_id: u32) -> Result<Event, serde_json::Error> {
    warp::sse::Event::default()
        .event("cache_changed")
        .json_data(DataChanged { network_id })
}

pub fn with_footer(footer: String) -> impl Filter<Extract = (String,), Error = Infallible> + Clone {
    warp::any().map(move || footer.clone())
}

pub fn with_caches(caches: Caches) -> impl Filter<Extract = (Caches,), Error = Infallible> + Clone {
    warp::any().map(move || caches.clone())
}

pub fn with_networks(
    networks: Vec<NetworkJson>,
) -> impl Filter<Extract = (Vec<NetworkJson>,), Error = Infallible> + Clone {
    warp::any().map(move || networks.clone())
}

pub fn with_config_networks(
    networks: Vec<Network>,
) -> impl Filter<Extract = (Vec<Network>,), Error = Infallible> + Clone {
    warp::any().map(move || networks.clone())
}

pub fn with_slugs(
    slugs: Vec<String>,
) -> impl Filter<Extract = (Vec<String>,), Error = Infallible> + Clone {
    warp::any().map(move || slugs.clone())
}

#[cfg(test)]
mod tests {
    use super::build_routes;
    use crate::config::{BoxedSyncSendNode, Config, Network, PoolIdentification};
    use crate::node::{BitcoinCoreNode, NodeInfo};
    use crate::types::{Cache, Caches, NetworkJson, StaleBlockJson};
    use corepc_client::bitcoin::consensus::deserialize;
    use corepc_client::bitcoin::{Block, BlockHash};
    use corepc_client::client_sync::Auth;
    use std::collections::{BTreeMap, HashMap};
    use std::str::FromStr;
    use std::sync::Arc;
    use tokio::sync::Mutex;
    use warp::Filter;

    fn caches_with_stale(network_id: u32, stale_blocks: Vec<StaleBlockJson>) -> Caches {
        let mut map = BTreeMap::new();
        map.insert(
            network_id,
            Cache {
                header_infos_json: vec![],
                node_data: BTreeMap::new(),
                forks: vec![],
                stale_blocks,
                block_cache: HashMap::new(),
                recent_miners: vec![],
            },
        );
        Arc::new(Mutex::new(map))
    }

    fn make_network(id: u32, nodes: Vec<BoxedSyncSendNode>) -> Network {
        Network {
            id,
            description: String::new(),
            name: format!("net{}", id),
            slug: format!("net{}", id),
            min_fork_height: 0,
            max_interesting_heights: 100,
            nodes,
            pool_identification: PoolIdentification::default(),
        }
    }

    fn node_info(id: u32, name: &str) -> NodeInfo {
        NodeInfo {
            id,
            name: name.to_string(),
            description: String::new(),
            implementation: "Bitcoin Core".to_string(),
        }
    }

    // A node whose RPC/REST endpoint is not listening, so every request fails.
    fn broken_core_node(id: u32) -> BoxedSyncSendNode {
        Arc::new(BitcoinCoreNode::new(
            node_info(id, "broken"),
            "http://127.0.0.1:1".to_string(),
            Auth::UserPass("user".to_string(), "pass".to_string()),
            false, // use_rest
            false, // use_waitfornewblock
        ))
    }

    #[tokio::test]
    async fn stale_json_returns_cached_blocks_in_order() {
        let caches = caches_with_stale(
            0,
            vec![
                StaleBlockJson {
                    height: 10,
                    hash: "aa".to_string(),
                    header: "00".repeat(80),
                },
                StaleBlockJson {
                    height: 9,
                    hash: "bb".to_string(),
                    header: "11".repeat(80),
                },
            ],
        );
        let route = routes(caches, vec![make_network(0, vec![])]);

        let resp = warp::test::request()
            .path("/api/0/stale.json")
            .reply(&route)
            .await;

        assert_eq!(resp.status(), 200);
        let v: serde_json::Value = serde_json::from_slice(resp.body()).unwrap();
        let arr = v["stale_blocks"].as_array().unwrap();
        assert_eq!(arr.len(), 2);
        assert_eq!(arr[0]["hash"], "aa");
        assert_eq!(arr[0]["height"], 10);
        assert_eq!(arr[1]["hash"], "bb");
    }

    #[tokio::test]
    async fn stale_json_unknown_network_is_empty() {
        let caches = caches_with_stale(
            0,
            vec![StaleBlockJson {
                height: 1,
                hash: "aa".to_string(),
                header: "00".repeat(80),
            }],
        );
        let route = routes(caches, vec![make_network(0, vec![])]);

        let resp = warp::test::request()
            .path("/api/99/stale.json")
            .reply(&route)
            .await;

        assert_eq!(resp.status(), 200);
        let v: serde_json::Value = serde_json::from_slice(resp.body()).unwrap();
        assert_eq!(v["stale_blocks"].as_array().unwrap().len(), 0);
    }

    // Builds a caches map in which `hash` is a known stale block on `network_id`.
    fn caches_with_stale_hash(network_id: u32, hash: &str) -> Caches {
        caches_with_stale(
            network_id,
            vec![StaleBlockJson {
                height: 1,
                hash: hash.to_string(),
                header: "00".repeat(80),
            }],
        )
    }

    fn test_config(networks: Vec<Network>) -> Config {
        Config {
            database_path: std::path::PathBuf::new(),
            www_path: std::path::PathBuf::new(),
            query_interval: std::time::Duration::from_secs(1),
            address: "127.0.0.1:0".parse().unwrap(),
            networks,
            footer_html: String::new(),
            rss_base_url: String::new(),
        }
    }

    // Builds the real application routes (via `build_routes`) so tests exercise
    // the same route wiring the server uses in production.
    fn routes(
        caches: Caches,
        networks: Vec<Network>,
    ) -> impl Filter<Extract = impl warp::Reply, Error = warp::Rejection> + Clone {
        let network_infos: Vec<NetworkJson> = networks.iter().map(NetworkJson::new).collect();
        let config = test_config(networks);
        let (cache_changed_tx, _rx) = tokio::sync::broadcast::channel(16);
        build_routes(&network_infos, &config, &caches, cache_changed_tx)
    }

    #[tokio::test]
    async fn known_slug_redirects_to_query_param() {
        // make_network(0, ..) has slug "net0".
        let route = routes(caches_with_stale(0, vec![]), vec![make_network(0, vec![])]);
        let resp = warp::test::request().path("/net0").reply(&route).await;
        assert_eq!(resp.status(), 302);
        assert_eq!(resp.headers().get("location").unwrap(), "./?network=net0");
    }

    #[tokio::test]
    async fn unknown_slug_returns_404() {
        let route = routes(caches_with_stale(0, vec![]), vec![make_network(0, vec![])]);
        let resp = warp::test::request()
            .path("/does-not-exist")
            .reply(&route)
            .await;
        assert_eq!(resp.status(), 404);
    }

    #[tokio::test]
    async fn networks_json_exposes_slug() {
        let route = routes(caches_with_stale(0, vec![]), vec![make_network(0, vec![])]);
        let resp = warp::test::request()
            .path("/api/networks.json")
            .reply(&route)
            .await;
        assert_eq!(resp.status(), 200);
        let v: serde_json::Value = serde_json::from_slice(resp.body()).unwrap();
        assert_eq!(v["networks"][0]["slug"], "net0");
    }

    #[tokio::test]
    async fn block_invalid_hash_returns_400() {
        let route = routes(
            caches_with_stale(0, vec![]),
            vec![make_network(0, vec![broken_core_node(0)])],
        );
        let resp = warp::test::request()
            .path("/api/0/block/not-a-hash/hex")
            .reply(&route)
            .await;
        assert_eq!(resp.status(), 400);
    }

    #[tokio::test]
    async fn block_unknown_network_returns_404() {
        let hash = "0".repeat(64);
        let route = routes(
            caches_with_stale_hash(0, &hash),
            vec![make_network(0, vec![broken_core_node(0)])],
        );
        let resp = warp::test::request()
            .path(&format!("/api/99/block/{}/hex", hash))
            .reply(&route)
            .await;
        assert_eq!(resp.status(), 404);
    }

    #[tokio::test]
    async fn block_non_stale_hash_returns_404() {
        // The network exists, but the requested block isn't a known stale block.
        let route = routes(
            caches_with_stale(0, vec![]),
            vec![make_network(0, vec![broken_core_node(0)])],
        );
        let hash = "0".repeat(64);
        let resp = warp::test::request()
            .path(&format!("/api/0/block/{}/hex", hash))
            .reply(&route)
            .await;
        assert_eq!(resp.status(), 404);
    }

    #[tokio::test]
    async fn block_all_nodes_failing_returns_502() {
        // The block is a known stale block, so we proceed to (unsuccessfully)
        // query the nodes.
        let hash = "0".repeat(64);
        let route = routes(
            caches_with_stale_hash(0, &hash),
            vec![make_network(0, vec![broken_core_node(0)])],
        );
        let resp = warp::test::request()
            .path(&format!("/api/0/block/{}/hex", hash))
            .reply(&route)
            .await;
        assert_eq!(resp.status(), 404);
    }

    // --- Tests that spin up a real regtest bitcoind ---

    use corepc_node::Node as CoreNode;

    fn start_bitcoind() -> CoreNode {
        let exe = corepc_node::exe_path()
            .expect("a bitcoind binary via BITCOIND_EXE or PATH (see shell.nix)");
        CoreNode::new(exe).expect("failed to launch bitcoind")
    }

    fn core_node(id: u32, core: &CoreNode) -> BoxedSyncSendNode {
        Arc::new(BitcoinCoreNode::new(
            node_info(id, "core"),
            core.rpc_url(),
            Auth::CookieFile(core.params.cookie_file.clone()),
            false, // use_rest (avoid needing bitcoind's REST interface enabled)
            true,  // use_waitfornewblock
        ))
    }

    #[tokio::test]
    async fn block_endpoints_return_the_full_block() {
        let core = start_bitcoind();
        let address = core.client.new_address().expect("new_address failed");
        core.client
            .generate_to_address(3, &address)
            .expect("generate_to_address failed");

        let node = core_node(0, &core);
        let hash = node.block_hash(2).await.expect("block_hash failed");
        let network = make_network(0, vec![node.clone()]);
        // Mark the block as stale so the endpoint serves it.
        let caches = caches_with_stale_hash(0, &hash.to_string());

        let route = routes(caches, vec![network]);

        // hex endpoint
        let resp = warp::test::request()
            .path(&format!("/api/0/block/{}/hex", hash))
            .reply(&route)
            .await;
        assert_eq!(resp.status(), 200);
        let hex = std::str::from_utf8(resp.body()).expect("hex body should be utf8");
        let bytes = hex::decode(hex).expect("body should be valid hex");
        let block: Block = deserialize(&bytes).expect("should deserialize to a block");
        assert_eq!(block.block_hash(), hash);

        // bin endpoint
        let resp = warp::test::request()
            .path(&format!("/api/0/block/{}/bin", hash))
            .reply(&route)
            .await;
        assert_eq!(resp.status(), 200);
        let block: Block = deserialize(resp.body()).expect("should deserialize to a block");
        assert_eq!(block.block_hash(), hash);
    }

    #[tokio::test]
    async fn block_endpoint_tries_all_nodes_until_one_succeeds() {
        let core = start_bitcoind();
        let address = core.client.new_address().expect("new_address failed");
        core.client
            .generate_to_address(2, &address)
            .expect("generate_to_address failed");

        let good = core_node(1, &core);
        let hash = good.block_hash(1).await.expect("block_hash failed");

        // A broken node comes first; the handler must fall through to the good one.
        let network = make_network(0, vec![broken_core_node(0), good.clone()]);
        let caches = caches_with_stale_hash(0, &hash.to_string());
        let route = routes(caches, vec![network]);

        let resp = warp::test::request()
            .path(&format!("/api/0/block/{}/hex", hash))
            .reply(&route)
            .await;

        assert_eq!(resp.status(), 200);
        let hex = std::str::from_utf8(resp.body()).expect("hex body should be utf8");
        let bytes = hex::decode(hex).expect("body should be valid hex");
        let block: Block = deserialize(&bytes).expect("should deserialize to a block");
        assert_eq!(block.block_hash(), hash);
    }

    #[tokio::test]
    async fn block_is_served_from_cache_after_first_fetch() {
        let core = start_bitcoind();
        let address = core.client.new_address().expect("new_address failed");
        core.client
            .generate_to_address(2, &address)
            .expect("generate_to_address failed");

        let good = core_node(0, &core);
        let hash = good.block_hash(1).await.expect("block_hash failed");
        let caches = caches_with_stale_hash(0, &hash.to_string());

        // First fetch via a working node populates the cache.
        let route = routes(caches.clone(), vec![make_network(0, vec![good.clone()])]);
        let resp = warp::test::request()
            .path(&format!("/api/0/block/{}/hex", hash))
            .reply(&route)
            .await;
        assert_eq!(resp.status(), 200);

        // The cache now holds the raw block bytes.
        {
            let locked = caches.lock().await;
            let cached = locked
                .get(&0)
                .unwrap()
                .block_cache
                .get(&hash)
                .cloned()
                .expect("cache entry present")
                .expect("cached bytes present");
            let block: Block = deserialize(&cached).expect("cached bytes deserialize");
            assert_eq!(block.block_hash(), hash);
        }

        // A second request whose only node is broken still succeeds: it is served
        // from the cache and the node is never consulted.
        let route2 = routes(
            caches.clone(),
            vec![make_network(0, vec![broken_core_node(1)])],
        );
        let resp = warp::test::request()
            .path(&format!("/api/0/block/{}/bin", hash))
            .reply(&route2)
            .await;
        assert_eq!(resp.status(), 200);
        let block: Block = deserialize(resp.body()).expect("should deserialize to a block");
        assert_eq!(block.block_hash(), hash);
    }

    #[tokio::test]
    async fn missing_block_is_cached_as_none_and_not_retried() {
        let hash = "0".repeat(64);
        let block_hash = BlockHash::from_str(&hash).unwrap();
        let caches = caches_with_stale_hash(0, &hash);
        let route = routes(
            caches.clone(),
            vec![make_network(0, vec![broken_core_node(0)])],
        );

        let resp = warp::test::request()
            .path(&format!("/api/0/block/{}/hex", hash))
            .reply(&route)
            .await;
        assert_eq!(resp.status(), 404);

        // The failure is remembered as `None` so we don't retry the nodes.
        {
            let locked = caches.lock().await;
            let entry = locked
                .get(&0)
                .unwrap()
                .block_cache
                .get(&block_hash)
                .cloned();
            assert_eq!(entry, Some(None));
        }

        // A second request is still 404, now served from the cached `None`.
        let resp = warp::test::request()
            .path(&format!("/api/0/block/{}/hex", hash))
            .reply(&route)
            .await;
        assert_eq!(resp.status(), 404);
    }
}
