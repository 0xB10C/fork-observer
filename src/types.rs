use bitcoincore_rpc::bitcoin::hash_types::BlockHash;
use bitcoincore_rpc::bitcoin::BlockHeader;
use bitcoincore_rpc::json::{GetChainTipsResultStatus, GetChainTipsResultTip};
use serde::{Deserialize, Serialize};

use crate::config::Network;

#[derive(Debug, Eq, PartialEq, Clone)]
pub struct HeaderInfo {
    pub height: u64,
    pub header: BlockHeader,
}

#[derive(Deserialize)]
pub struct DataQuery {
    pub network: u32,
}

#[derive(Serialize)]
pub struct NetworkJson {
    pub id: u32,
    pub name: String,
    pub description: String,
}

impl NetworkJson {
    pub fn new(network: &Network) -> Self {
        NetworkJson {
            id: network.id,
            name: network.name.clone(),
            description: network.description.clone(),
        }
    }
}

#[derive(Serialize)]
pub struct NetworksJsonResponse {
    pub networks: Vec<NetworkJson>,
}

#[derive(Debug, Eq, PartialEq, Clone, Serialize)]
pub struct HeaderInfoJson {
    pub id: usize,
    pub prev_id: usize,
    pub height: u64,
    pub hash: String,
    pub version: u32,
    pub prev_blockhash: String,
    pub merkle_root: String,
    pub time: u32,
    pub bits: u32,
    pub nonce: u32,
}

impl HeaderInfoJson {
    pub fn new(hi: &HeaderInfo, id: usize, prev_id: usize) -> Self {
        HeaderInfoJson {
            id: id,
            prev_id: prev_id,
            height: hi.height,
            hash: hi.header.block_hash().to_string(),
            version: hi.header.version as u32,
            prev_blockhash: hi.header.prev_blockhash.to_string(),
            merkle_root: hi.header.merkle_root.to_string(),
            time: hi.header.time,
            bits: hi.header.bits,
            nonce: hi.header.nonce,
        }
    }
}

#[derive(Serialize)]
pub struct DataJsonResponse {
    pub block_infos: Vec<HeaderInfoJson>,
    pub tip_infos: Vec<TipInfoJson>,
    pub nodes: Vec<NodeInfoJson>,
}

#[derive(Serialize)]
pub struct TipInfoJson {

}

#[derive(Serialize)]
pub struct NodeInfoJson {

}
