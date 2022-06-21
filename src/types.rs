use bitcoincore_rpc::bitcoin::hash_types::BlockHash;
use bitcoincore_rpc::bitcoin::BlockHeader;
use bitcoincore_rpc::json::{GetChainTipsResultStatus, GetChainTipsResultTip};
use serde::{Deserialize, Serialize};

#[derive(Debug, Eq, PartialEq, Clone, Serialize)]
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
