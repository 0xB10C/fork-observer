use std::convert::Infallible;

use warp::{sse::Event, Filter};

use crate::types::{
    Caches, DataChanged, DataJsonResponse, InfoJsonResponse, NetworkJson, NetworksJsonResponse,
};

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

pub async fn networks_response(
    network_infos: Vec<NetworkJson>,
) -> Result<impl warp::Reply, Infallible> {
    Ok(warp::reply::json(&NetworksJsonResponse {
        networks: network_infos,
    }))
}

pub fn data_changed_sse(
    network_id: u32,
) -> Result<Event, bitcoincore_rpc::jsonrpc::serde_json::Error> {
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
