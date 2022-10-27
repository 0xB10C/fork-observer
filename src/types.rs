use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;

use crate::config::{Network, Node};

use bitcoincore_rpc::bitcoin::{BlockHash, BlockHeader};
use bitcoincore_rpc::json::{GetChainTipsResult, GetChainTipsResultStatus, GetChainTipsResultTip};
use bitcoincore_rpc::Client;

use serde::{Deserialize, Serialize};

use petgraph::graph::DiGraph;
use petgraph::graph::NodeIndex;

use rusqlite::Connection;

use tokio::sync::Mutex;

pub type NodeInfo = BTreeMap<u8, NodeInfoJson>;
pub type Cache = (Vec<HeaderInfoJson>, NodeInfo);
pub type Caches = Arc<Mutex<BTreeMap<u32, Cache>>>;
pub type TreeInfo = (DiGraph<HeaderInfo, bool>, HashMap<BlockHash, NodeIndex>);
pub type Tree = Arc<Mutex<TreeInfo>>;
pub type Db = Arc<Mutex<Connection>>;
pub type Rpc = Arc<Client>;

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
            id,
            prev_id,
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
pub struct InfoJsonResponse {
    pub footer: String,
}

#[derive(Serialize)]
pub struct DataJsonResponse {
    pub header_infos: Vec<HeaderInfoJson>,
    pub nodes: Vec<NodeInfoJson>,
}

#[derive(Serialize, Clone)]
pub struct TipInfoJson {
    pub hash: String,
    pub status: String,
    pub height: u64,
}

impl TipInfoJson {
    pub fn new(tip: &GetChainTipsResultTip) -> Self {
        TipInfoJson {
            hash: tip.hash.to_string(),
            status: tip_status_string(tip.status),
            height: tip.height,
        }
    }
}

fn tip_status_string(status: GetChainTipsResultStatus) -> String {
    match status {
        GetChainTipsResultStatus::Active => String::from("active"),
        GetChainTipsResultStatus::Invalid => String::from("invalid"),
        GetChainTipsResultStatus::HeadersOnly => String::from("headers-only"),
        GetChainTipsResultStatus::ValidHeaders => String::from("valid-headers"),
        GetChainTipsResultStatus::ValidFork => String::from("valid-fork"),
    }
}

#[derive(Serialize, Clone)]
pub struct NodeInfoJson {
    pub id: u8,
    pub name: String,
    pub description: String,
    pub tips: Vec<TipInfoJson>,
    /// UTC timestamp when the tip information of the node was last changed.
    pub last_changed_timestamp: u64,
    /// The node subversion as advertised by the node on the network.
    pub version: String,
}

impl NodeInfoJson {
    pub fn new(
        node: Node,
        tips: &GetChainTipsResult,
        version: String,
        last_changed_timestamp: u64,
    ) -> Self {
        NodeInfoJson {
            id: node.id,
            name: node.name,
            description: node.description,
            tips: tips.iter().map(TipInfoJson::new).collect(),
            last_changed_timestamp,
            version,
        }
    }
}

#[derive(Serialize, Clone)]
pub struct DataChanged {
    pub network_id: u32,
}
