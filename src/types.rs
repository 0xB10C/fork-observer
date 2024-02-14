use std::collections::{BTreeMap, HashMap};
use std::fmt;
use std::str::FromStr;
use std::sync::Arc;

use crate::config::Network;
use crate::node::NodeInfo;

use bitcoincore_rpc::bitcoin::blockdata::block::Header;
use bitcoincore_rpc::bitcoin::BlockHash;
use bitcoincore_rpc::json::{GetChainTipsResultStatus, GetChainTipsResultTip};

use serde::{Deserialize, Serialize};

use petgraph::graph::DiGraph;
use petgraph::graph::NodeIndex;

use rusqlite::Connection;

use tokio::sync::Mutex;

#[derive(Clone)]
pub struct Cache {
    pub header_infos_json: Vec<HeaderInfoJson>,
    pub node_data: NodeData,
    pub forks: Vec<Fork>,
}

pub type NodeData = BTreeMap<u32, NodeDataJson>;
pub type Caches = Arc<Mutex<BTreeMap<u32, Cache>>>;
pub type TreeInfo = (DiGraph<HeaderInfo, bool>, HashMap<BlockHash, NodeIndex>);
pub type Tree = Arc<Mutex<TreeInfo>>;
pub type Db = Arc<Mutex<Connection>>;

#[derive(Debug, Eq, PartialEq, Clone)]
pub struct HeaderInfo {
    pub height: u64,
    pub header: Header,
    pub miner: String,
}

impl HeaderInfo {
    pub fn update_miner(&mut self, miner: String) {
        self.miner = miner;
    }
}

#[derive(Serialize, Clone)]
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
    pub miner: String,
}

impl HeaderInfoJson {
    pub fn new(hi: &HeaderInfo, id: usize, prev_id: usize) -> Self {
        HeaderInfoJson {
            id,
            prev_id,
            height: hi.height,
            hash: hi.header.block_hash().to_string(),
            version: hi.header.version.to_consensus() as u32,
            prev_blockhash: hi.header.prev_blockhash.to_string(),
            merkle_root: hi.header.merkle_root.to_string(),
            time: hi.header.time,
            bits: hi.header.bits.to_consensus(),
            nonce: hi.header.nonce,
            miner: hi.miner.clone(),
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
    pub nodes: Vec<NodeDataJson>,
}

#[derive(Serialize, Clone, Eq, Hash, PartialEq)]
pub struct TipInfoJson {
    pub hash: String,
    pub status: String,
    pub height: u64,
}

#[derive(Debug, Clone)]
pub struct Fork {
    pub common: HeaderInfo,
    pub children: Vec<HeaderInfo>,
}

impl TipInfoJson {
    pub fn new(tip: &ChainTip) -> Self {
        TipInfoJson {
            hash: tip.hash.clone(),
            status: tip.status.to_string(),
            height: tip.height,
        }
    }
}

#[derive(Serialize, Clone)]
pub struct NodeDataJson {
    pub id: u32,
    pub name: String,
    pub description: String,
    pub tips: Vec<TipInfoJson>,
    /// UTC timestamp when the tip information of the node was last changed.
    pub last_changed_timestamp: u64,
    /// The node subversion as advertised by the node on the network.
    pub version: String,
}

impl NodeDataJson {
    pub fn new(
        info: NodeInfo,
        tips: &Vec<ChainTip>,
        version: String,
        last_changed_timestamp: u64,
    ) -> Self {
        NodeDataJson {
            id: info.id,
            name: info.name,
            description: info.description,
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

#[derive(Deserialize, Clone, Debug, PartialEq, Eq)]
pub enum ChainTipStatus {
    #[serde(rename = "active")]
    Active,
    #[serde(rename = "invalid")]
    Invalid,
    #[serde(rename = "valid-fork")]
    ValidFork,
    #[serde(rename = "headers-only")]
    HeadersOnly,
    #[serde(rename = "valid-headers")]
    ValidHeaders,
    Unknown,
}

impl From<String> for ChainTipStatus {
    fn from(s: String) -> Self {
        match s.as_str() {
            "active" => ChainTipStatus::Active,
            "invalid" => ChainTipStatus::Invalid,
            "headers-only" => ChainTipStatus::HeadersOnly,
            "valid-headers" => ChainTipStatus::ValidHeaders,
            "valid-fork" => ChainTipStatus::ValidFork,
            _ => ChainTipStatus::Unknown,
        }
    }
}

impl From<GetChainTipsResultStatus> for ChainTipStatus {
    fn from(s: GetChainTipsResultStatus) -> Self {
        match s {
            GetChainTipsResultStatus::Active => ChainTipStatus::Active,
            GetChainTipsResultStatus::Invalid => ChainTipStatus::Invalid,
            GetChainTipsResultStatus::HeadersOnly => ChainTipStatus::HeadersOnly,
            GetChainTipsResultStatus::ValidHeaders => ChainTipStatus::ValidHeaders,
            GetChainTipsResultStatus::ValidFork => ChainTipStatus::ValidFork,
        }
    }
}

impl fmt::Display for ChainTipStatus {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            ChainTipStatus::Active => write!(f, "active"),
            ChainTipStatus::Invalid => write!(f, "invalid"),
            ChainTipStatus::HeadersOnly => write!(f, "headers-only"),
            ChainTipStatus::ValidHeaders => write!(f, "valid-headers"),
            ChainTipStatus::ValidFork => write!(f, "valid-fork"),
            ChainTipStatus::Unknown => write!(f, "unknown"),
        }
    }
}

#[derive(Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct ChainTip {
    pub height: u64,
    pub hash: String,
    pub branchlen: usize,
    pub status: ChainTipStatus,
}

impl From<GetChainTipsResultTip> for ChainTip {
    fn from(t: GetChainTipsResultTip) -> Self {
        ChainTip {
            height: t.height,
            hash: t.hash.to_string(),
            branchlen: t.branch_length,
            status: t.status.into(),
        }
    }
}

impl ChainTip {
    pub fn block_hash(&self) -> BlockHash {
        BlockHash::from_str(&self.hash).unwrap()
    }
}
