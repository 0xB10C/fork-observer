use std::convert::Infallible;

use warp::{sse::Event, Filter};

use crate::types::{
    DataJsonResponse, DataQuery, NetworkJson, NetworksJsonResponse,
    DataChanged, InfoJsonResponse, Caches
};
use crate::config::Network;

pub async fn info_response(footer: String) -> Result<impl warp::Reply, Infallible> {
    Ok(warp::reply::json(&InfoJsonResponse {
        footer: footer,
    }))
}

pub async fn data_response(caches: Caches, query: DataQuery) -> Result<impl warp::Reply, Infallible> {
    let network: u32 = query.network;

    let caches_locked = caches.lock().await;
    let (header_info_json, node_infos) = caches_locked.get(&network).unwrap().clone();

    Ok(warp::reply::json(&DataJsonResponse {
        header_infos: header_info_json,
        nodes: node_infos.values().cloned().collect(),
    }))
}

pub async fn networks_response(networks: Vec<Network>) -> Result<impl warp::Reply, Infallible> {
    let network_infos: Vec<NetworkJson> = networks.iter().map(|n| NetworkJson::new(n)).collect();

    Ok(warp::reply::json(&NetworksJsonResponse {
        networks: network_infos,
    }))
}

pub fn data_changed_sse(network_id: u32) -> Result<Event, Infallible> {
    Ok(warp::sse::Event::default().event("tip_changed").json_data(DataChanged {network_id}).unwrap())
}

pub fn with_footer(
    footer: String,
) -> impl Filter<Extract = (String,), Error = std::convert::Infallible> + Clone {
    warp::any().map(move || footer.clone())
}

pub fn with_caches(
    caches: Caches,
) -> impl Filter<Extract = (Caches,), Error = std::convert::Infallible> + Clone {
    warp::any().map(move || caches.clone())
}

pub fn with_networks(
    networks: Vec<Network>,
) -> impl Filter<Extract = (Vec<Network>,), Error = std::convert::Infallible> + Clone {
    warp::any().map(move || networks.clone())
}
