use std::collections::{BTreeMap, HashMap};
use std::fmt;
use std::str::FromStr;
use std::sync::Arc;
use std::time::SystemTime;

use crate::backend::NodeInfo;
use crate::config::{Countdown, Network};

use bytes::Bytes;
use corepc_client::bitcoin::blockdata::block::Header;
use corepc_client::bitcoin::BlockHash;
use corepc_client::types::model::{ChainTips, ChainTipsStatus};
use log::{error, warn};
use petgraph::graph::DiGraph;
use petgraph::graph::NodeIndex;
use rusqlite::Connection;
use serde::{Deserialize, Serialize};
use tokio::sync::{Mutex, RwLock};

#[derive(Clone)]
pub struct Cache {
    /// The serialized `data.json` body. Rebuilt through [`Cache::rebuild_data_json`]
    /// whenever something it contains changes, so that serving a request is
    /// handing out a reference-counted buffer rather than walking the header
    /// list and serializing it again for every client.
    pub data_json: Bytes,
    /// This network's countdown, taken from the configuration. It's part of
    /// `data.json`, and keeping it here means rebuilding that response doesn't
    /// need access to the config.
    pub countdown: Option<Countdown>,
    pub header_infos_json: Vec<HeaderInfoJson>,
    pub node_data: NodeData,
    pub forks: Vec<Fork>,
    /// The (up to `MAX_STALE_BLOCKS`) most recent stale blocks we know about.
    /// A stale block is any block that is not part of the active chain, including
    /// intermediate (non-tip) blocks of a stale branch.
    pub stale_blocks: Vec<StaleBlockJson>,
    /// In-memory cache of full (raw, consensus-serialized) stale blocks, keyed by
    /// block hash. `Some(bytes)` is a block we fetched from a node; `None` means
    /// we asked every node and none had it, so we don't retry (a restart clears
    /// this and retries). Bounded to the blocks currently in `stale_blocks`
    /// (<= `MAX_STALE_BLOCKS`); entries are pruned as blocks leave that list.
    pub block_cache: HashMap<BlockHash, Option<Vec<u8>>>,
    /// Since strip_tree and identifying miners runs in parallel,
    /// the strip_tree result might not contain a miner yet. Keeping
    /// recent miners here and use + manage them when updating the cache.
    pub recent_miners: Vec<(String, String)>,
}

impl Cache {
    /// A network's cache, with its `data.json` body built from the data passed
    /// in.
    pub fn new(
        header_infos_json: Vec<HeaderInfoJson>,
        node_data: NodeData,
        forks: Vec<Fork>,
        stale_blocks: Vec<StaleBlockJson>,
        countdown: Option<Countdown>,
    ) -> Self {
        let mut cache = Cache {
            data_json: Bytes::new(),
            countdown,
            header_infos_json,
            node_data,
            forks,
            stale_blocks,
            block_cache: HashMap::new(),
            recent_miners: vec![],
        };
        cache.rebuild_data_json();
        cache
    }

    /// Serializes the current header and node data into the cached `data.json`
    /// body. Call this after changing anything that response contains.
    ///
    /// Serializing can only fail if a value can't be represented as JSON, which
    /// none of these can be. If it somehow does, we keep serving the previous
    /// body rather than an empty one.
    pub fn rebuild_data_json(&mut self) {
        let response = DataJsonRef {
            header_infos: &self.header_infos_json,
            nodes: self.node_data.values().collect(),
            countdown: self.countdown.as_ref(),
        };
        match serde_json::to_vec(&response) {
            Ok(bytes) => self.data_json = Bytes::from(bytes),
            Err(e) => error!("Could not serialize data.json: {}", e),
        }
    }
}

pub type NodeData = BTreeMap<u32, NodeDataJson>;

/// The cache of every network, one lock per network.
///
/// The map itself is built once at startup - the set of networks comes from the
/// configuration and never changes - so it needs no lock of its own, and a
/// request for one network never waits on another network's writer. Within a
/// network the lock is an `RwLock`: serving a request only reads.
pub type Caches = Arc<BTreeMap<u32, RwLock<Cache>>>;

/// Builds the [`Caches`] map out of each network's cache. Once built it isn't
/// added to or removed from.
pub fn caches_from(caches: impl IntoIterator<Item = (u32, Cache)>) -> Caches {
    Arc::new(
        caches
            .into_iter()
            .map(|(network_id, cache)| (network_id, RwLock::new(cache)))
            .collect(),
    )
}
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

#[derive(Serialize, Deserialize, Clone)]
pub struct NetworkJson {
    pub id: u32,
    pub name: String,
    pub slug: String,
    pub description: String,
}

impl NetworkJson {
    pub fn new(network: &Network) -> Self {
        NetworkJson {
            id: network.id,
            name: network.name.clone(),
            slug: network.slug.clone(),
            description: network.description.clone(),
        }
    }
}

#[derive(Serialize, Deserialize)]
pub struct NetworksJsonResponse {
    pub networks: Vec<NetworkJson>,
}

#[derive(Debug, Eq, PartialEq, Clone, Serialize, Deserialize)]
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
    // we don't need this to be a float (header.difficulty_float() returns an f64)
    // as the exact precision isn't too important for us. It would also require us
    // to implement Eq for HeaderInfoJson ourself.
    pub difficulty_int: u64,
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
            difficulty_int: hi.header.difficulty_float() as u64,
            nonce: hi.header.nonce,
            miner: hi.miner.clone(),
        }
    }

    pub fn update_miner(&mut self, miner: String) {
        self.miner = miner;
    }
}

#[derive(Serialize)]
pub struct InfoJsonResponse {
    pub footer: String,
}

/// `data.json` as another fork-observer instance serves it. Only used to parse
/// a remote instance's response; ours is serialized from [`DataJsonRef`].
#[derive(Serialize, Deserialize)]
pub struct DataJsonResponse {
    pub header_infos: Vec<HeaderInfoJson>,
    pub nodes: Vec<NodeDataJson>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub countdown: Option<Countdown>,
}

/// The same response, borrowing from the cache it is built out of, so that
/// rebuilding it doesn't first have to clone the whole header list and every
/// node's data into an owned [`DataJsonResponse`].
#[derive(Serialize)]
struct DataJsonRef<'a> {
    header_infos: &'a [HeaderInfoJson],
    nodes: Vec<&'a NodeDataJson>,
    #[serde(skip_serializing_if = "Option::is_none")]
    countdown: Option<&'a Countdown>,
}

/// A stale block: a block that is not part of the active chain. This includes
/// stale tips as well as intermediate (non-tip) blocks of a stale branch.
#[derive(Debug, Eq, PartialEq, Clone, Serialize)]
pub struct StaleBlockJson {
    pub height: u64,
    pub hash: String,
    /// The 80-byte block header, hex-encoded.
    pub header: String,
}

impl StaleBlockJson {
    pub fn new(hi: &HeaderInfo) -> Self {
        StaleBlockJson {
            height: hi.height,
            hash: hi.header.block_hash().to_string(),
            header: corepc_client::bitcoin::consensus::encode::serialize_hex(&hi.header),
        }
    }
}

#[derive(Serialize)]
pub struct StaleBlocksJsonResponse {
    pub stale_blocks: Vec<StaleBlockJson>,
}

#[derive(Serialize, Deserialize, Clone, Eq, Hash, PartialEq, Debug)]
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

#[derive(Serialize, Deserialize, Clone, Eq, PartialEq, Debug)]
pub struct NodeDataJson {
    pub id: u32,
    pub name: String,
    pub description: String,
    // The implementation of the node
    pub implementation: String,
    pub tips: Vec<TipInfoJson>,
    /// UTC timestamp when the tip information of the node was last changed.
    pub last_changed_timestamp: u64,
    /// The node subversion as advertised by the node on the network.
    pub version: String,
    /// If the last getchaintips RPC reached the node.
    pub reachable: bool,
    /// The name of the fork-observer instance this node entry was imported
    /// from (see `remote_forkobserver`), or `None` for a node configured here.
    /// Importing skips nodes that already have it set, so a node only ever
    /// travels one hop from where it's configured. That's what keeps two
    /// instances pointing at each other from accumulating each other's
    /// imports.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub remote_source: Option<String>,
}

impl NodeDataJson {
    pub fn new(
        info: NodeInfo,
        tips: &Vec<ChainTip>,
        version: String,
        last_changed_timestamp: u64,
        reachable: bool,
    ) -> Self {
        NodeDataJson {
            id: info.id,
            name: info.name,
            description: info.description,
            implementation: info.implementation,
            tips: tips.iter().map(TipInfoJson::new).collect(),
            last_changed_timestamp,
            version,
            reachable,
            remote_source: None,
        }
    }

    /// The node's name, qualified with the instance it was imported from for
    /// remote nodes. Used where there's no room for styling (RSS feeds); the
    /// frontend shows the two parts separately.
    pub fn display_name(&self) -> String {
        match &self.remote_source {
            Some(remote) => format!("{} via {}", self.name, remote),
            None => self.name.clone(),
        }
    }

    pub fn reachable(&mut self, r: bool) {
        self.reachable = r;
    }

    pub fn version(&mut self, v: String) {
        self.version = v;
    }

    pub fn tips(&mut self, tips: &[ChainTip]) {
        self.tips = tips.iter().map(TipInfoJson::new).collect();
        self.last_changed_timestamp = match SystemTime::now().duration_since(SystemTime::UNIX_EPOCH)
        {
            Ok(n) => n.as_secs(),
            Err(_) => {
                warn!("SystemTime is before UNIX_EPOCH time. Node last_change_timestamp set to 0.");
                0u64
            }
        };
    }
}

#[derive(Serialize, Clone)]
pub struct DataChanged {
    pub network_id: u32,
}

/// Deserialized via `From<String>` so that a status we don't know yet becomes
/// `Unknown` instead of failing the whole `getchaintips` response.
#[derive(Deserialize, Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
#[serde(from = "String")]
pub enum ChainTipStatus {
    Active,
    Invalid,
    ValidFork,
    HeadersOnly,
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

impl From<ChainTipsStatus> for ChainTipStatus {
    fn from(s: ChainTipsStatus) -> Self {
        match s {
            ChainTipsStatus::Active => ChainTipStatus::Active,
            ChainTipsStatus::Invalid => ChainTipStatus::Invalid,
            ChainTipsStatus::HeadersOnly => ChainTipStatus::HeadersOnly,
            ChainTipsStatus::ValidHeaders => ChainTipStatus::ValidHeaders,
            ChainTipsStatus::ValidFork => ChainTipStatus::ValidFork,
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

#[derive(Deserialize, Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct ChainTip {
    pub height: u64,
    pub hash: String,
    pub branchlen: usize,
    pub status: ChainTipStatus,
}

impl From<ChainTips> for ChainTip {
    fn from(t: ChainTips) -> Self {
        ChainTip {
            height: t.height as u64,
            hash: t.hash.to_string(),
            branchlen: t.branch_length as usize,
            status: t.status.into(),
        }
    }
}

impl ChainTip {
    pub fn block_hash(&self) -> BlockHash {
        BlockHash::from_str(&self.hash).unwrap()
    }
}
