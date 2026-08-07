//! A proxy for forkmonitor.info's chaintips API.
//!
//! forkmonitor.info runs its own set of nodes, and `/api/v1/chaintips` reports
//! which block each of them currently considers the tip of the active chain.
//! The frontend can't fetch that itself: the response carries no
//! `Access-Control-Allow-Origin` header (their rack-cors `allow` block never
//! calls `origins`, so no origin ever matches), which makes it unreadable from
//! a browser. So we fetch it here and hand it to the frontend verbatim.
//!
//! This is a temporary measure to see what the forkmonitor nodes consider
//! valid, opt-in in the frontend via `?forkmonitor`. Nothing else in
//! fork-observer reads this: the data is not imported into the tree (it carries
//! no block headers at all, only hashes and heights), and no node data is
//! derived from it.

use log::{debug, warn};
use std::convert::Infallible;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::Mutex;
use warp::Filter;

const URL: &str = "https://forkmonitor.info/api/v1/chaintips";
/// How long a fetched response is served before we fetch a new one. forkmonitor
/// polls its nodes every few seconds, so anything shorter would mostly produce
/// requests that return what we already have.
const CACHE_FOR: Duration = Duration::from_secs(10);
const TIMEOUT_SECONDS: u64 = 8;
/// Set on every response with the age of the proxied data, so the frontend can
/// tell "just fetched" from "this is the last thing we managed to fetch".
const AGE_HEADER: &str = "x-forkmonitor-age";

/// The last response we successfully fetched, and when we fetched it.
pub type ChaintipsCache = Arc<Mutex<Option<(Instant, Vec<u8>)>>>;

pub fn new_cache() -> ChaintipsCache {
    Arc::new(Mutex::new(None))
}

pub fn with_cache(
    cache: ChaintipsCache,
) -> impl Filter<Extract = (ChaintipsCache,), Error = Infallible> + Clone {
    warp::any().map(move || cache.clone())
}

pub async fn chaintips_response(cache: ChaintipsCache) -> Result<impl warp::Reply, Infallible> {
    // The lock is held across the fetch, so requests arriving while one is in
    // flight wait for its result instead of each starting their own.
    let mut cached = cache.lock().await;

    if let Some((fetched_at, body)) = cached.as_ref() {
        let age = fetched_at.elapsed();
        if age < CACHE_FOR {
            return Ok(json_response(body.clone(), age));
        }
    }

    match fetch().await {
        Ok(body) => {
            debug!("fetched {} bytes from {}", body.len(), URL);
            *cached = Some((Instant::now(), body.clone()));
            Ok(json_response(body, Duration::ZERO))
        }
        // Keep serving the last response we did get rather than blanking the
        // view over a single failed request. The age header says how old it is.
        Err(e) => {
            warn!("Could not fetch {}: {}", URL, e);
            match cached.as_ref() {
                Some((fetched_at, body)) => Ok(json_response(body.clone(), fetched_at.elapsed())),
                None => Ok(unavailable(&e)),
            }
        }
    }
}

fn json_response(body: Vec<u8>, age: Duration) -> warp::http::Response<Vec<u8>> {
    warp::http::Response::builder()
        .header("content-type", "application/json")
        .header("cache-control", format!("max-age={}", CACHE_FOR.as_secs()))
        .header(AGE_HEADER, age.as_secs())
        .body(body)
        .unwrap()
}

fn unavailable(error: &str) -> warp::http::Response<Vec<u8>> {
    warp::http::Response::builder()
        .status(502)
        .header("content-type", "text/plain")
        .body(format!("Could not fetch {}: {}", URL, error).into_bytes())
        .unwrap()
}

/// GETs the chaintips JSON. `minreq` blocks, so this runs off the runtime's
/// worker threads.
async fn fetch() -> Result<Vec<u8>, String> {
    tokio::task::spawn_blocking(|| {
        debug!("forkmonitor: GET {}", URL);
        let response = minreq::get(URL)
            .with_header("user-agent", "fork-observer")
            .with_timeout(TIMEOUT_SECONDS)
            .send()
            .map_err(|e| e.to_string())?;

        if response.status_code != 200 {
            return Err(format!(
                "HTTP {} {}",
                response.status_code, response.reason_phrase
            ));
        }

        let body = response.as_bytes().to_vec();
        // We cache what we get for a while and fall back to it when a later
        // fetch fails, so make sure we don't hold on to something that isn't
        // the JSON we asked for (an error page, a captive portal, ..).
        serde_json::from_slice::<serde_json::Value>(&body)
            .map_err(|e| format!("response is not valid JSON: {}", e))?;

        Ok(body)
    })
    .await
    .map_err(|e| format!("fetch task failed: {}", e))?
}
