use crate::activity::{Activity, ActivityJsonResponse, ActivityQuery, EVENTS_PER_PAGE};
use crate::config::{Config, Network};
use crate::rss;
use crate::types::{
    etag, Caches, DataChanged, InfoJsonResponse, NetworkJson, NetworksJsonResponse,
};
use bytes::Bytes;
use corepc_client::bitcoin::BlockHash;
use futures_util::StreamExt;
use log::{error, warn};
use std::convert::Infallible;
use std::str::FromStr;
use std::sync::Arc;
use tokio::sync::broadcast::Sender;
use tokio_stream::wrappers::BroadcastStream;
use warp::http::{header, HeaderValue};
use warp::{sse::Event, Filter, Reply};

pub fn build_routes(
    network_infos: &Vec<NetworkJson>,
    config: &Config,
    caches: &Caches,
    cache_changed_tx_warp: Sender<u32>,
    activity: &Option<Activity>,
) -> impl Filter<Extract = impl warp::Reply, Error = warp::Rejection> + Clone {
    let static_files = StaticFiles::new(&config.www_path);

    // Files we loaded at startup are served from memory; anything else (a file
    // added to `www_path` while we run) still comes off disk through warp.
    let www_dir = warp::get()
        .and(warp::path("static"))
        .and(warp::path::tail())
        .and(with_static_files(static_files.clone()))
        .and(with_if_none_match())
        .and_then(
            |tail: warp::path::Tail, statics: StaticFiles, if_none_match| async move {
                statics
                    .reply(tail.as_str(), if_none_match, STATIC_CACHE_CONTROL)
                    .ok_or_else(warp::reject::not_found)
            },
        )
        .or(warp::get()
            .and(warp::path("static"))
            .and(warp::fs::dir(config.www_path.clone())));

    // The pages themselves are always revalidated: they name the assets, so a
    // stale page keeps a browser on the old ones.
    let page = |name: &'static str, www_path: std::path::PathBuf, statics: StaticFiles| {
        warp::any()
            .and(with_static_files(statics))
            .and(with_if_none_match())
            .and_then(move |statics: StaticFiles, if_none_match| async move {
                statics
                    .reply(name, if_none_match, NO_CACHE)
                    .ok_or_else(warp::reject::not_found)
            })
            .or(warp::fs::file(www_path.join(name)))
    };
    let index_html = warp::get().and(warp::path::end()).and(page(
        "index.html",
        config.www_path.clone(),
        static_files.clone(),
    ));
    let fullscreen_html = warp::get().and(warp::path!("fullscreen")).and(page(
        "fullscreen.html",
        config.www_path.clone(),
        static_files.clone(),
    ));
    let activity_html = warp::get().and(warp::path!("activity")).and(page(
        "activity.html",
        config.www_path.clone(),
        static_files.clone(),
    ));
    let playback_html = warp::get().and(warp::path!("playback")).and(page(
        "playback.html",
        config.www_path.clone(),
        static_files.clone(),
    ));

    // `info.json` and `networks.json` are built entirely out of the
    // configuration, so they're serialized once here instead of on every
    // request.
    let info_json = warp::get()
        .and(warp::path!("api" / "info.json"))
        .and(with_body(serialized(&InfoJsonResponse {
            footer: config.footer_html.clone(),
        })))
        .and(with_if_none_match())
        .and_then(static_json_response);

    let data_json = warp::get()
        .and(warp::path!("api" / u32 / "data.json"))
        .and(with_caches(caches.clone()))
        .and(with_if_none_match())
        .and_then(data_response);

    let stale_json = warp::get()
        .and(warp::path!("api" / u32 / "stale.json"))
        .and(with_caches(caches.clone()))
        .and(with_if_none_match())
        .and_then(stale_blocks_response);

    let activity_json = warp::get()
        .and(warp::path!("api" / u32 / "activity.json"))
        .and(warp::query::<ActivityQuery>())
        .and(with_activity(activity.clone()))
        .and(with_if_none_match())
        .and_then(activity_response);

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
        .and(with_if_none_match())
        .and_then(rss::forks_response);

    let invalid_blocks_rss = warp::get()
        .and(warp::path!("rss" / u32 / "invalid.xml"))
        .and(with_caches(caches.clone()))
        .and(with_networks(network_infos.clone()))
        .and(rss::with_rss_base_url(config.rss_base_url.clone()))
        .and(with_if_none_match())
        .and_then(rss::invalid_blocks_response);

    let lagging_nodes_rss = warp::get()
        .and(warp::path!("rss" / u32 / "lagging.xml"))
        .and(with_caches(caches.clone()))
        .and(with_networks(network_infos.clone()))
        .and(rss::with_rss_base_url(config.rss_base_url.clone()))
        .and(with_if_none_match())
        .and_then(rss::lagging_nodes_response);

    let unreachable_nodes_rss = warp::get()
        .and(warp::path!("rss" / u32 / "unreachable.xml"))
        .and(with_caches(caches.clone()))
        .and(with_networks(network_infos.clone()))
        .and(rss::with_rss_base_url(config.rss_base_url.clone()))
        .and(with_if_none_match())
        .and_then(rss::unreachable_nodes_response);

    let networks_json = warp::get()
        .and(warp::path!("api" / "networks.json"))
        .and(with_body(serialized(&NetworksJsonResponse {
            networks: network_infos.to_vec(),
        })))
        .and(with_if_none_match())
        .and_then(static_json_response);

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
        .or(activity_html)
        .or(playback_html)
        .or(data_json)
        .or(stale_json)
        .or(activity_json)
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

/// How long a browser may reuse a file from `/static` before checking back.
/// Short, because the pages reference the assets by plain name: a deploy has to
/// be picked up without anyone clearing their cache.
const STATIC_CACHE_CONTROL: HeaderValue = HeaderValue::from_static("public, max-age=300");

/// The contents of the `www` directory, read once at startup, each file with
/// the `ETag` of its contents.
///
/// We serve these ourselves rather than through `warp::fs` because `warp::fs`
/// validates with `Last-Modified`, and the modification times we deploy with
/// are meaningless: the Dockerfile copies `www` out of the build stage, which
/// resets every one of them to the epoch. They are still the epoch after the
/// next deploy, so a browser revalidating an asset it cached is told "not
/// modified" and keeps running the old JavaScript against a new backend. An
/// `ETag` over the contents answers the question the timestamp was meant to
/// answer, correctly.
///
/// Serving from memory also means no disk access per request, and the whole
/// directory is well under a megabyte.
///
/// The cost is that a file changed while we run isn't picked up until a
/// restart. Anything not loaded at startup falls through to `warp::fs`, so a
/// newly added file is still served.
#[derive(Clone, Default)]
pub struct StaticFiles {
    /// Keyed by the path relative to `www_path`, e.g. `js/blocktree.js`.
    files: Arc<std::collections::HashMap<String, StaticFile>>,
}

struct StaticFile {
    body: Bytes,
    etag: HeaderValue,
    content_type: HeaderValue,
}

impl StaticFiles {
    pub fn new(www_path: &std::path::Path) -> Self {
        let mut files = std::collections::HashMap::new();
        let mut directories = vec![www_path.to_path_buf()];
        while let Some(directory) = directories.pop() {
            let entries = match std::fs::read_dir(&directory) {
                Ok(entries) => entries,
                Err(e) => {
                    warn!(
                        "Could not read {:?} to serve it from memory: {}",
                        directory, e
                    );
                    continue;
                }
            };
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_dir() {
                    directories.push(path);
                    continue;
                }
                let name = match path.strip_prefix(www_path).ok().and_then(|p| p.to_str()) {
                    Some(name) => name.replace('\\', "/"),
                    None => continue,
                };
                match std::fs::read(&path) {
                    Ok(contents) => {
                        files.insert(
                            name,
                            StaticFile {
                                etag: etag(&contents),
                                content_type: content_type(&path),
                                body: Bytes::from(contents),
                            },
                        );
                    }
                    Err(e) => warn!("Could not read {:?} to serve it from memory: {}", path, e),
                }
            }
        }
        log::info!("Loaded {} files from {:?}", files.len(), www_path);
        StaticFiles {
            files: Arc::new(files),
        }
    }

    /// The file, with its `ETag` and a `Cache-Control`, or a 304 if the client
    /// already has this version. `None` for a path we didn't load, which lets
    /// the caller fall through to serving it off disk.
    fn reply(
        &self,
        name: &str,
        if_none_match: Option<String>,
        cache_control: HeaderValue,
    ) -> Option<warp::reply::Response> {
        let file = self.files.get(name)?;
        let unchanged = etag_matches(if_none_match.as_deref(), &file.etag);

        let response = warp::http::Response::builder()
            .header(header::CACHE_CONTROL, cache_control)
            .header(header::ETAG, file.etag.clone());

        Some(if unchanged {
            response
                .status(warp::http::StatusCode::NOT_MODIFIED)
                .body(Bytes::new())
                .unwrap()
                .into_response()
        } else {
            response
                .header(header::CONTENT_TYPE, file.content_type.clone())
                .body(file.body.clone())
                .unwrap()
                .into_response()
        })
    }
}

/// The content type of a file we serve, by extension. Only the kinds `www`
/// holds; anything else is served as bytes and left to the browser to sniff.
fn content_type(path: &std::path::Path) -> HeaderValue {
    HeaderValue::from_static(match path.extension().and_then(|e| e.to_str()) {
        Some("html") => "text/html; charset=utf-8",
        Some("js") => "text/javascript; charset=utf-8",
        Some("css") => "text/css; charset=utf-8",
        Some("svg") => "image/svg+xml",
        Some("png") => "image/png",
        Some("ico") => "image/x-icon",
        Some("json") => "application/json",
        _ => "application/octet-stream",
    })
}

pub fn with_static_files(
    static_files: StaticFiles,
) -> impl Filter<Extract = (StaticFiles,), Error = Infallible> + Clone {
    warp::any().map(move || static_files.clone())
}

/// Serves a JSON body that was serialized once when the routes were built.
pub async fn static_json_response(
    body: StaticJson,
    if_none_match: Option<String>,
) -> Result<impl warp::Reply, Infallible> {
    Ok(json_response(body.body, body.etag, if_none_match))
}

/// A response body that doesn't change while we run, with its `ETag`.
#[derive(Clone)]
pub struct StaticJson {
    body: Bytes,
    etag: HeaderValue,
}

/// Serializes a response that never changes while we run. A value that can't be
/// serialized would be a bug we'd rather see at startup than per request, so
/// this panics.
fn serialized<T: serde::Serialize>(value: &T) -> StaticJson {
    let body = serde_json::to_vec(value).expect("a static API response should serialize");
    StaticJson {
        etag: etag(&body),
        body: Bytes::from(body),
    }
}

/// A JSON response carrying an `ETag`, or a 304 if the client already has this
/// version.
///
/// `Cache-Control: no-cache` means "you may store this, but check with me
/// before reusing it" - so a client that comes back gets a ~150 byte 304
/// instead of the body, while never being shown data that has since changed.
pub fn cached_response(
    body: Bytes,
    content_type: HeaderValue,
    etag: HeaderValue,
    if_none_match: Option<String>,
) -> warp::http::Response<Bytes> {
    let unchanged = etag_matches(if_none_match.as_deref(), &etag);

    // The header names and the constant values are built once rather than
    // parsed from strings per response.
    let response = warp::http::Response::builder()
        .header(header::CACHE_CONTROL, NO_CACHE)
        .header(header::ETAG, etag);

    if unchanged {
        return response
            .status(warp::http::StatusCode::NOT_MODIFIED)
            .body(Bytes::new())
            .unwrap();
    }
    response
        .header(header::CONTENT_TYPE, content_type)
        .body(body)
        .unwrap()
}

fn json_response(
    body: Bytes,
    etag: HeaderValue,
    if_none_match: Option<String>,
) -> warp::http::Response<Bytes> {
    cached_response(body, APPLICATION_JSON, etag, if_none_match)
}

pub const NO_CACHE: HeaderValue = HeaderValue::from_static("no-cache");
const APPLICATION_JSON: HeaderValue = HeaderValue::from_static("application/json");

/// Whether the `If-None-Match` a client sent covers the version we would serve.
///
/// RFC 9110 §13.1.2 has `If-None-Match` use the *weak* comparison function, so
/// `W/"x"` and `"x"` are the same version, a list matches if any of its tags
/// does, and `*` matches whatever we have.
///
/// The weak form is not a corner case: a proxy that compresses our response
/// rewrites the `ETag` it passes on to `W/"..."`, because what it sends is no
/// longer byte-identical to what we sent. nginx does this, and every browser
/// asks for compression, so the tag that comes back is routinely not the one we
/// handed out. Comparing the two byte for byte answers all of those with the
/// full body, which is the entire cost the `ETag` is here to avoid.
fn etag_matches(if_none_match: Option<&str>, etag: &HeaderValue) -> bool {
    let Some(header) = if_none_match else {
        return false;
    };
    if header.trim() == "*" {
        return true;
    }
    // Splitting on commas is safe for the tags we hand out - they are quoted
    // hex - even though an entity tag may in general contain one.
    let ours = strip_weak(etag.as_bytes());
    header
        .split(',')
        .any(|tag| strip_weak(tag.trim().as_bytes()) == ours)
}

/// An entity tag without its weakness marker, for comparing two of them weakly.
fn strip_weak(tag: &[u8]) -> &[u8] {
    tag.strip_prefix(b"W/").unwrap_or(tag)
}

/// The `If-None-Match` a client sent, if any.
pub fn with_if_none_match(
) -> impl Filter<Extract = (Option<String>,), Error = warp::Rejection> + Clone {
    warp::header::optional::<String>("if-none-match")
}

/// Serves a network's `data.json` straight out of the cache.
///
/// The body was serialized when the cache last changed, so all a request does
/// is bump a reference count on it: it holds the cache lock for a pointer copy
/// instead of for a clone of the whole header list plus a JSON serialization.
/// Under load that's the difference between requests queueing up behind each
/// other and them not noticing each other at all.
pub async fn data_response(
    network: u32,
    caches: Caches,
    if_none_match: Option<String>,
) -> Result<impl warp::Reply, Infallible> {
    let (body, etag) = match caches.get(&network) {
        Some(cache) => {
            let cache = cache.read().await;
            (cache.data_json.clone(), cache.data_json_etag.clone())
        }
        None => (
            Bytes::from_static(EMPTY_DATA_JSON),
            crate::types::etag(EMPTY_DATA_JSON),
        ),
    };
    Ok(json_response(body, etag, if_none_match))
}

/// What we serve for a network that has no cache. Every configured network gets
/// one at startup, so this is only reachable with an unknown network id.
const EMPTY_DATA_JSON: &[u8] = br#"{"header_infos":[],"nodes":[]}"#;

/// Serves a network's `stale.json`, pre-serialized in the cache like
/// `data.json`.
pub async fn stale_blocks_response(
    network: u32,
    caches: Caches,
    if_none_match: Option<String>,
) -> Result<impl warp::Reply, Infallible> {
    let (body, etag) = match caches.get(&network) {
        Some(cache) => {
            let cache = cache.read().await;
            (cache.stale_json.clone(), cache.stale_json_etag.clone())
        }
        None => (
            Bytes::from_static(EMPTY_STALE_JSON),
            crate::types::etag(EMPTY_STALE_JSON),
        ),
    };
    Ok(json_response(body, etag, if_none_match))
}

const EMPTY_STALE_JSON: &[u8] = br#"{"stale_blocks":[]}"#;

/// Serves the [`EVENTS_PER_PAGE`] most recent activity log events of a
/// network, newest first.
///
/// Without a `before` parameter the events come from the in-memory ring
/// buffer; `before=<id>` paginates into the activity database. When the
/// activity log is disabled or the network is unknown, an empty event list
/// is returned (matching how `data.json` degrades).
pub async fn activity_response(
    network_id: u32,
    query: ActivityQuery,
    activity: Option<Activity>,
    if_none_match: Option<String>,
) -> Result<impl warp::Reply, Infallible> {
    let events = match activity {
        Some(activity) => match query.before {
            Some(before) => match activity
                .events_before(network_id, before, EVENTS_PER_PAGE)
                .await
            {
                Ok(events) => events,
                Err(e) => {
                    error!(
                        "Could not query activity events before id {} for network {}: {}",
                        before, network_id, e
                    );
                    vec![]
                }
            },
            None => activity.recent_events(network_id, EVENTS_PER_PAGE).await,
        },
        None => vec![],
    };
    // Unlike the cached responses this one is built per request, so it is
    // serialized and hashed here. Paginating into the past (`before=<id>`)
    // returns events that never change, so those requests get a 304 from the
    // second one onwards.
    let body = match serde_json::to_vec(&ActivityJsonResponse { events }) {
        Ok(body) => Bytes::from(body),
        Err(e) => {
            error!("Could not serialize activity.json: {}", e);
            Bytes::from_static(EMPTY_ACTIVITY_JSON)
        }
    };
    let etag = crate::types::etag(&body);
    Ok(json_response(body, etag, if_none_match))
}

const EMPTY_ACTIVITY_JSON: &[u8] = br#"{"events":[]}"#;

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
    networks: Arc<Vec<Network>>,
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
        match caches.get(&network_id) {
            Some(cache) => {
                let cache = cache.read().await;
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
                if let Some(cache) = caches.get(&network_id) {
                    let mut cache = cache.write().await;
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
    slugs: Arc<Vec<String>>,
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

// These run on every request that reaches the route they're attached to, so
// what they hand the handler has to be cheap to produce. Anything bigger than a
// couple of words is shared behind an `Arc` (or, for a response body, `Bytes`)
// rather than cloned per request.

pub fn with_body(
    body: StaticJson,
) -> impl Filter<Extract = (StaticJson,), Error = Infallible> + Clone {
    warp::any().map(move || body.clone())
}

pub fn with_caches(caches: Caches) -> impl Filter<Extract = (Caches,), Error = Infallible> + Clone {
    warp::any().map(move || caches.clone())
}

pub fn with_activity(
    activity: Option<Activity>,
) -> impl Filter<Extract = (Option<Activity>,), Error = Infallible> + Clone {
    warp::any().map(move || activity.clone())
}

pub fn with_networks(
    networks: Vec<NetworkJson>,
) -> impl Filter<Extract = (Arc<Vec<NetworkJson>>,), Error = Infallible> + Clone {
    let networks = Arc::new(networks);
    warp::any().map(move || networks.clone())
}

pub fn with_config_networks(
    networks: Vec<Network>,
) -> impl Filter<Extract = (Arc<Vec<Network>>,), Error = Infallible> + Clone {
    let networks = Arc::new(networks);
    warp::any().map(move || networks.clone())
}

pub fn with_slugs(
    slugs: Vec<String>,
) -> impl Filter<Extract = (Arc<Vec<String>>,), Error = Infallible> + Clone {
    let slugs = Arc::new(slugs);
    warp::any().map(move || slugs.clone())
}

#[cfg(test)]
mod tests {
    use super::{build_routes, etag_matches, HeaderValue};
    use crate::backend::{BitcoinCoreNode, NodeInfo};
    use crate::config::{BoxedSyncSendNode, Config, Countdown, Network, PoolIdentification};
    use crate::types::{
        caches_from, Cache, Caches, NetworkJson, NodeDataJson, StaleBlockJson, Tree,
    };
    use corepc_client::bitcoin::consensus::deserialize;
    use corepc_client::bitcoin::{Block, BlockHash};
    use corepc_client::client_sync::Auth;
    use petgraph::graph::DiGraph;
    use std::collections::{BTreeMap, HashMap};
    use std::str::FromStr;
    use std::sync::Arc;
    use tokio::sync::Mutex;
    use warp::Filter;

    fn caches_with_stale(network_id: u32, stale_blocks: Vec<StaleBlockJson>) -> Caches {
        caches_for_network(network_id, stale_blocks, None)
    }

    fn caches_for_network(
        network_id: u32,
        stale_blocks: Vec<StaleBlockJson>,
        countdown: Option<Countdown>,
    ) -> Caches {
        caches_from([(
            network_id,
            Cache::new(vec![], BTreeMap::new(), vec![], stale_blocks, countdown),
        )])
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
            remote_forkobservers: vec![],
            pool_identification: PoolIdentification::default(),
            countdown: None,
            activity_retention_days: None,
            activity_log_node_ids: std::collections::BTreeSet::new(),
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

    // Every endpoint that carries an ETag, checked the same way: the tag comes
    // back, and offering it again gets a 304 with no body.
    #[tokio::test]
    async fn endpoints_answer_an_unchanged_etag_with_304() {
        let activity = activity_with_events(0, 5).await;
        let route = routes_with_activity(
            caches_with_stale(
                0,
                vec![StaleBlockJson {
                    height: 1,
                    hash: "aa".to_string(),
                    header: "00".repeat(80),
                }],
            ),
            vec![make_network(0, vec![])],
            Some(activity),
        );

        for path in [
            "/api/0/data.json",
            "/api/0/stale.json",
            "/api/0/activity.json",
            "/api/0/activity.json?before=3",
            "/api/info.json",
            "/api/networks.json",
            "/rss/0/forks.xml",
            "/rss/0/invalid.xml",
            "/rss/0/lagging.xml",
            "/rss/0/unreachable.xml",
        ] {
            let resp = warp::test::request().path(path).reply(&route).await;
            assert_eq!(resp.status(), 200, "{}", path);
            assert_eq!(
                resp.headers().get("cache-control").unwrap(),
                "no-cache",
                "{}",
                path
            );
            let etag = resp
                .headers()
                .get("etag")
                .unwrap_or_else(|| panic!("{} should carry an ETag", path))
                .clone();
            assert!(!resp.body().is_empty(), "{}", path);

            let resp = warp::test::request()
                .path(path)
                .header("if-none-match", etag.clone())
                .reply(&route)
                .await;
            assert_eq!(resp.status(), 304, "{}", path);
            assert!(resp.body().is_empty(), "{}", path);
            assert_eq!(resp.headers().get("etag").unwrap(), &etag, "{}", path);

            // The tag a browser gets back once a proxy compressed the response.
            let weak = format!("W/{}", etag.to_str().unwrap());
            let resp = warp::test::request()
                .path(path)
                .header("if-none-match", weak)
                .reply(&route)
                .await;
            assert_eq!(resp.status(), 304, "{} with a weak tag", path);
            assert!(resp.body().is_empty(), "{} with a weak tag", path);
        }
    }

    #[tokio::test]
    async fn stale_json_etag_changes_with_the_stale_blocks() {
        let caches = caches_with_stale(0, vec![]);
        let route = routes(caches.clone(), vec![make_network(0, vec![])]);
        let before = warp::test::request()
            .path("/api/0/stale.json")
            .reply(&route)
            .await;
        let etag_before = before.headers().get("etag").unwrap().clone();

        {
            let mut cache = caches.get(&0).unwrap().write().await;
            cache.stale_blocks = vec![StaleBlockJson {
                height: 5,
                hash: "cc".to_string(),
                header: "11".repeat(80),
            }];
            cache.rebuild_responses();
        }

        let after = warp::test::request()
            .path("/api/0/stale.json")
            .header("if-none-match", etag_before.clone())
            .reply(&route)
            .await;
        assert_eq!(after.status(), 200);
        assert_ne!(after.headers().get("etag").unwrap(), &etag_before);
        let v: serde_json::Value = serde_json::from_slice(after.body()).unwrap();
        assert_eq!(v["stale_blocks"][0]["hash"], "cc");
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
            activity: None,
        }
    }

    // Builds the real application routes (via `build_routes`) so tests exercise
    // the same route wiring the server uses in production.
    fn routes(
        caches: Caches,
        networks: Vec<Network>,
    ) -> impl Filter<Extract = impl warp::Reply, Error = warp::Rejection> + Clone {
        routes_with_activity(caches, networks, None)
    }

    fn routes_with_activity(
        caches: Caches,
        networks: Vec<Network>,
        activity: Option<crate::activity::Activity>,
    ) -> impl Filter<Extract = impl warp::Reply, Error = warp::Rejection> + Clone {
        let network_infos: Vec<NetworkJson> = networks.iter().map(NetworkJson::new).collect();
        let config = test_config(networks);
        let (cache_changed_tx, _rx) = tokio::sync::broadcast::channel(16);
        build_routes(
            &network_infos,
            &config,
            &caches,
            cache_changed_tx,
            &activity,
        )
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

    // The cache a network gets at startup, so that these tests cover the whole
    // path from the configuration through `build_cache` into the response.
    async fn populated_caches(network: &Network) -> Caches {
        let tree: Tree = Arc::new(Mutex::new((DiGraph::new(), HashMap::new())));
        caches_from([(network.id, crate::cache::build_cache(network, &tree).await)])
    }

    #[tokio::test]
    async fn data_json_exposes_countdown_when_configured() {
        let mut network = make_network(0, vec![]);
        network.countdown = Some(Countdown {
            height: 105,
            label: "Halving".to_string(),
        });
        let caches = populated_caches(&network).await;
        let route = routes(caches, vec![network]);
        let resp = warp::test::request()
            .path("/api/0/data.json")
            .reply(&route)
            .await;
        assert_eq!(resp.status(), 200);
        let v: serde_json::Value = serde_json::from_slice(resp.body()).unwrap();
        assert_eq!(v["countdown"]["height"], 105);
        assert_eq!(v["countdown"]["label"], "Halving");
    }

    #[tokio::test]
    async fn data_json_omits_countdown_when_not_configured() {
        let network = make_network(0, vec![]);
        let caches = populated_caches(&network).await;
        let route = routes(caches, vec![network]);
        let resp = warp::test::request()
            .path("/api/0/data.json")
            .reply(&route)
            .await;
        assert_eq!(resp.status(), 200);
        let v: serde_json::Value = serde_json::from_slice(resp.body()).unwrap();
        assert!(v.get("countdown").is_none());
    }

    #[tokio::test]
    async fn data_json_serves_the_cached_headers_and_nodes() {
        let caches = caches_with_stale(0, vec![]);
        {
            let mut cache = caches.get(&0).unwrap().write().await;
            cache.node_data.insert(
                7,
                NodeDataJson::new(node_info(7, "a node"), &vec![], "v1".to_string(), 42, true),
            );
            cache.rebuild_responses();
        }
        let route = routes(caches, vec![make_network(0, vec![])]);
        let resp = warp::test::request()
            .path("/api/0/data.json")
            .reply(&route)
            .await;
        assert_eq!(resp.status(), 200);
        assert_eq!(
            resp.headers().get("content-type").unwrap(),
            "application/json"
        );
        let v: serde_json::Value = serde_json::from_slice(resp.body()).unwrap();
        assert_eq!(v["nodes"][0]["id"], 7);
        assert_eq!(v["nodes"][0]["name"], "a node");
        assert_eq!(v["header_infos"].as_array().unwrap().len(), 0);
    }

    #[tokio::test]
    async fn data_json_answers_an_unchanged_etag_with_304() {
        let caches = caches_with_stale(0, vec![]);
        let route = routes(caches.clone(), vec![make_network(0, vec![])]);

        let resp = warp::test::request()
            .path("/api/0/data.json")
            .reply(&route)
            .await;
        assert_eq!(resp.status(), 200);
        assert_eq!(resp.headers().get("cache-control").unwrap(), "no-cache");
        let etag = resp.headers().get("etag").unwrap().clone();
        let body = resp.body().clone();
        assert!(!body.is_empty());

        // Coming back with that ETag gets a 304 and no body.
        let resp = warp::test::request()
            .path("/api/0/data.json")
            .header("if-none-match", etag.clone())
            .reply(&route)
            .await;
        assert_eq!(resp.status(), 304);
        assert_eq!(resp.headers().get("etag").unwrap(), &etag);
        assert!(resp.body().is_empty());

        // What a browser sends back once a proxy has compressed the response
        // and weakened the tag on the way out. Same version, so still a 304.
        let resp = warp::test::request()
            .path("/api/0/data.json")
            .header("if-none-match", format!("W/{}", etag.to_str().unwrap()))
            .reply(&route)
            .await;
        assert_eq!(resp.status(), 304);
        assert!(resp.body().is_empty());

        // A different ETag (a client holding an older version) gets the body.
        let resp = warp::test::request()
            .path("/api/0/data.json")
            .header("if-none-match", "\"0000000000000000\"")
            .reply(&route)
            .await;
        assert_eq!(resp.status(), 200);
        assert_eq!(resp.body(), &body);
    }

    #[tokio::test]
    async fn data_json_etag_changes_when_the_cache_changes() {
        let caches = caches_with_stale(0, vec![]);
        let route = routes(caches.clone(), vec![make_network(0, vec![])]);

        let before = warp::test::request()
            .path("/api/0/data.json")
            .reply(&route)
            .await;
        let etag_before = before.headers().get("etag").unwrap().clone();

        {
            let mut cache = caches.get(&0).unwrap().write().await;
            cache.node_data.insert(
                7,
                NodeDataJson::new(node_info(7, "a node"), &vec![], "v1".to_string(), 42, true),
            );
            cache.rebuild_responses();
        }

        // The client's now-stale ETag must not be answered with a 304.
        let after = warp::test::request()
            .path("/api/0/data.json")
            .header("if-none-match", etag_before.clone())
            .reply(&route)
            .await;
        assert_eq!(after.status(), 200);
        assert_ne!(after.headers().get("etag").unwrap(), &etag_before);
    }

    #[test]
    fn etag_matches_compares_weakly() {
        let ours = HeaderValue::from_static("\"abc\"");
        for header in [
            "\"abc\"",
            // what a proxy that compressed the response hands the client
            "W/\"abc\"",
            // more than one stored version, and any position in the list
            "\"old\", \"abc\"",
            "W/\"abc\", \"old\"",
            // "whatever you have"
            "*",
        ] {
            assert!(etag_matches(Some(header), &ours), "{}", header);
        }

        for header in ["\"abcd\"", "W/\"abcd\"", "\"old\", \"older\"", "", "abc"] {
            assert!(!etag_matches(Some(header), &ours), "{}", header);
        }
        assert!(!etag_matches(None, &ours));
    }

    // --- Static files ---

    /// A temporary directory that removes itself when the test ends.
    struct TempDir(std::path::PathBuf);

    impl TempDir {
        fn path(&self) -> &std::path::Path {
            &self.0
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    // A www directory with a page and an asset in a subdirectory.
    fn www_fixture(name: &str) -> TempDir {
        let dir = std::env::temp_dir().join(format!("fork-observer-www-{}", name));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("js")).unwrap();
        std::fs::write(dir.join("index.html"), b"<html>hello</html>").unwrap();
        std::fs::write(dir.join("js/main.js"), b"console.log(1)").unwrap();
        TempDir(dir)
    }

    fn routes_with_www(
        www_path: &std::path::Path,
    ) -> impl Filter<Extract = impl warp::Reply, Error = warp::Rejection> + Clone {
        let networks = vec![make_network(0, vec![])];
        let network_infos: Vec<NetworkJson> = networks.iter().map(NetworkJson::new).collect();
        let mut config = test_config(networks);
        config.www_path = www_path.to_path_buf();
        let (cache_changed_tx, _rx) = tokio::sync::broadcast::channel(16);
        build_routes(
            &network_infos,
            &config,
            &caches_with_stale(0, vec![]),
            cache_changed_tx,
            &None,
        )
    }

    #[tokio::test]
    async fn static_files_are_served_with_an_etag_and_cache_control() {
        let www = www_fixture("etag");
        let route = routes_with_www(www.path());

        let resp = warp::test::request()
            .path("/static/js/main.js")
            .reply(&route)
            .await;
        assert_eq!(resp.status(), 200);
        assert_eq!(resp.body(), "console.log(1)");
        assert_eq!(
            resp.headers().get("content-type").unwrap(),
            "text/javascript; charset=utf-8"
        );
        assert_eq!(
            resp.headers().get("cache-control").unwrap(),
            "public, max-age=300"
        );
        let etag = resp.headers().get("etag").unwrap().clone();

        let resp = warp::test::request()
            .path("/static/js/main.js")
            .header("if-none-match", etag.clone())
            .reply(&route)
            .await;
        assert_eq!(resp.status(), 304);
        assert!(resp.body().is_empty());

        // Assets are the responses a proxy is most likely to compress, and it
        // weakens the tag when it does. Still the same file, so still a 304.
        let resp = warp::test::request()
            .path("/static/js/main.js")
            .header("if-none-match", format!("W/{}", etag.to_str().unwrap()))
            .reply(&route)
            .await;
        assert_eq!(resp.status(), 304);
        assert!(resp.body().is_empty());
    }

    #[tokio::test]
    async fn a_changed_file_gets_a_different_etag() {
        let www = www_fixture("changed");
        let etag_before = {
            let route = routes_with_www(www.path());
            warp::test::request()
                .path("/static/js/main.js")
                .reply(&route)
                .await
                .headers()
                .get("etag")
                .unwrap()
                .clone()
        };

        // A deploy changes the content. Nothing about the file's modification
        // time is consulted, which is the point: the deploy artifact's timestamps
        // are all the epoch.
        std::fs::write(www.path().join("js/main.js"), b"console.log(2)").unwrap();

        // A restart (the files are read at startup) must not answer the old
        // ETag with a 304.
        let route = routes_with_www(www.path());
        let resp = warp::test::request()
            .path("/static/js/main.js")
            .header("if-none-match", etag_before.clone())
            .reply(&route)
            .await;
        assert_eq!(resp.status(), 200);
        assert_eq!(resp.body(), "console.log(2)");
        assert_ne!(resp.headers().get("etag").unwrap(), &etag_before);
    }

    #[tokio::test]
    async fn the_index_page_is_served_but_not_cached() {
        let www = www_fixture("index");
        let route = routes_with_www(www.path());
        let resp = warp::test::request().path("/").reply(&route).await;
        assert_eq!(resp.status(), 200);
        assert_eq!(resp.body(), "<html>hello</html>");
        assert_eq!(resp.headers().get("cache-control").unwrap(), "no-cache");
        assert!(resp.headers().get("etag").is_some());
    }

    #[tokio::test]
    async fn static_paths_cannot_escape_the_www_directory() {
        let www = www_fixture("traversal");
        std::fs::write(www.path().parent().unwrap().join("fo-secret.txt"), b"nope").unwrap();
        let route = routes_with_www(www.path());

        for path in ["/static/../fo-secret.txt", "/static/js/../../fo-secret.txt"] {
            let resp = warp::test::request().path(path).reply(&route).await;
            assert_ne!(resp.status(), 200, "{} should not be served", path);
        }
    }

    #[tokio::test]
    async fn networks_json_answers_an_unchanged_etag_with_304() {
        let route = routes(caches_with_stale(0, vec![]), vec![make_network(0, vec![])]);
        let resp = warp::test::request()
            .path("/api/networks.json")
            .reply(&route)
            .await;
        assert_eq!(resp.status(), 200);
        let etag = resp.headers().get("etag").unwrap().clone();

        let resp = warp::test::request()
            .path("/api/networks.json")
            .header("if-none-match", etag)
            .reply(&route)
            .await;
        assert_eq!(resp.status(), 304);
        assert!(resp.body().is_empty());
    }

    #[tokio::test]
    async fn data_json_unknown_network_is_empty() {
        let route = routes(caches_with_stale(0, vec![]), vec![make_network(0, vec![])]);
        let resp = warp::test::request()
            .path("/api/99/data.json")
            .reply(&route)
            .await;
        assert_eq!(resp.status(), 200);
        let v: serde_json::Value = serde_json::from_slice(resp.body()).unwrap();
        assert_eq!(v["header_infos"].as_array().unwrap().len(), 0);
        assert_eq!(v["nodes"].as_array().unwrap().len(), 0);
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

    // --- Activity API ---

    use crate::activity::{Activity, ActivityEvent, ActivityEventKind, EVENTS_PER_PAGE};

    async fn activity_with_events(network_id: u32, how_many: usize) -> Activity {
        let activity = Activity::new(
            rusqlite::Connection::open_in_memory().expect("in-memory db should open"),
        );
        activity.setup().await.expect("activity setup");
        let events: Vec<ActivityEvent> = (0..how_many)
            .map(|i| {
                ActivityEvent::new(
                    network_id,
                    0,
                    ActivityEventKind::ActiveTipChanged {
                        old_hash: format!("{:02}", i),
                        old_height: i as u64,
                        new_hash: format!("{:02}", i + 1),
                        new_height: i as u64 + 1,
                    },
                )
            })
            .collect();
        crate::activity::write_events(&activity, &events)
            .await
            .expect("writing test events");
        activity
    }

    #[tokio::test]
    async fn activity_json_disabled_returns_empty() {
        let route = routes(caches_with_stale(0, vec![]), vec![make_network(0, vec![])]);
        let resp = warp::test::request()
            .path("/api/0/activity.json")
            .reply(&route)
            .await;
        assert_eq!(resp.status(), 200);
        let v: serde_json::Value = serde_json::from_slice(resp.body()).unwrap();
        assert_eq!(v["events"].as_array().unwrap().len(), 0);
    }

    #[tokio::test]
    async fn activity_json_recent_and_pagination() {
        let activity = activity_with_events(0, 10).await;
        let route = routes_with_activity(
            caches_with_stale(0, vec![]),
            vec![make_network(0, vec![])],
            Some(activity),
        );

        // Recent events (from the ring buffer), newest first.
        let resp = warp::test::request()
            .path("/api/0/activity.json")
            .reply(&route)
            .await;
        assert_eq!(resp.status(), 200);
        let v: serde_json::Value = serde_json::from_slice(resp.body()).unwrap();
        let events = v["events"].as_array().unwrap();
        assert_eq!(events.len(), 10);
        assert_eq!(events[0]["kind"], "active-tip-changed");
        assert_eq!(events[0]["details"]["new_height"], 10);
        assert_eq!(events[2]["details"]["new_height"], 8);
        let third_id = events[2]["id"].as_i64().unwrap();

        // Paginate into the database with `before`.
        let resp = warp::test::request()
            .path(&format!("/api/0/activity.json?before={}", third_id))
            .reply(&route)
            .await;
        let v: serde_json::Value = serde_json::from_slice(resp.body()).unwrap();
        let events = v["events"].as_array().unwrap();
        assert_eq!(events.len(), 7);
        assert_eq!(events[0]["details"]["new_height"], 7);

        // An unknown network has no events.
        let resp = warp::test::request()
            .path("/api/99/activity.json")
            .reply(&route)
            .await;
        let v: serde_json::Value = serde_json::from_slice(resp.body()).unwrap();
        assert_eq!(v["events"].as_array().unwrap().len(), 0);
    }

    #[tokio::test]
    async fn activity_json_serves_a_fixed_page_size() {
        let activity = activity_with_events(0, EVENTS_PER_PAGE + 10).await;
        let route = routes_with_activity(
            caches_with_stale(0, vec![]),
            vec![make_network(0, vec![])],
            Some(activity),
        );

        let resp = warp::test::request()
            .path("/api/0/activity.json")
            .reply(&route)
            .await;
        let v: serde_json::Value = serde_json::from_slice(resp.body()).unwrap();
        assert_eq!(v["events"].as_array().unwrap().len(), EVENTS_PER_PAGE);
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
            let cache = caches.get(&0).unwrap().read().await;
            let cached = cache
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
            let cache = caches.get(&0).unwrap().read().await;
            let entry = cache.block_cache.get(&block_hash).cloned();
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
