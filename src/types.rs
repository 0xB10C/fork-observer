use bitcoincore_rpc::bitcoin::hash_types::BlockHash;
use bitcoincore_rpc::bitcoin::hashes::Hash;
use bitcoincore_rpc::bitcoin::BlockHeader;
use bitcoincore_rpc::json::{GetChainTipsResultStatus, GetChainTipsResultTip};
use byteorder::{BigEndian, LittleEndian};
use serde::{Deserialize, Serialize};
use zerocopy::{byteorder::U128, byteorder::U32, byteorder::U64, AsBytes, FromBytes, Unaligned};

use std::convert::{TryFrom, TryInto};
use std::str::FromStr;
use std::{error, fmt};

pub const PREFIX_BLOCKINFO: u8 = b'b';
pub const PREFIX_TIPINFO: u8 = b't';
pub const PREFIX_NETWORKS: u8 = b'n';
pub const PREFIX_NODES: u8 = b'm';

pub const MAX_NAME_LENGTH: usize = 64;
pub const MAX_DESCRIPTION_LENGTH: usize = 2048;

pub fn max_block_hash() -> BlockHash {
    return BlockHash::from_str("ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff")
        .unwrap();
}

#[repr(u8)]
pub enum TipStatus {
    Active = b'a',
    HeadersOnly = b'h',
    Invalid = b'i',
    ValidFork = b'f',
    ValidHeaders = b'p',
}

impl From<GetChainTipsResultStatus> for TipStatus {
    fn from(status: GetChainTipsResultStatus) -> Self {
        match status {
            GetChainTipsResultStatus::Active => TipStatus::Active,
            GetChainTipsResultStatus::HeadersOnly => TipStatus::HeadersOnly,
            GetChainTipsResultStatus::Invalid => TipStatus::Invalid,
            GetChainTipsResultStatus::ValidFork => TipStatus::ValidFork,
            GetChainTipsResultStatus::ValidHeaders => TipStatus::ValidHeaders,
        }
    }
}

impl TryFrom<u8> for TipStatus {
    type Error = &'static str;

    fn try_from(status: u8) -> Result<Self, Self::Error> {
        match status {
            b'a' => Ok(TipStatus::Active),
            b'h' => Ok(TipStatus::HeadersOnly),
            b'i' => Ok(TipStatus::Invalid),
            b'f' => Ok(TipStatus::ValidFork),
            b'p' => Ok(TipStatus::ValidHeaders),
            _ => Err("Invalid TipStatus"),
        }
    }
}

impl Into<u8> for TipStatus {
    fn into(self) -> u8 {
        match self {
            TipStatus::Active => TipStatus::Active as u8,
            TipStatus::HeadersOnly => TipStatus::HeadersOnly as u8,
            TipStatus::Invalid => TipStatus::Invalid as u8,
            TipStatus::ValidFork => TipStatus::ValidFork as u8,
            TipStatus::ValidHeaders => TipStatus::ValidHeaders as u8,
        }
    }
}

impl ToString for TipStatus {
    fn to_string(&self) -> String {
        match self {
            TipStatus::Active => String::from("active"),
            TipStatus::HeadersOnly => String::from("headers-only"),
            TipStatus::Invalid => String::from("invalid"),
            TipStatus::ValidFork => String::from("valid-fork"),
            TipStatus::ValidHeaders => String::from("valid-headers"),
        }
    }
}

/// Error occuring while creating a value for a Key-Value pair.
#[derive(Debug)]
pub enum ValueError {
    NameTooLongError(String, usize),
    DescriptionTooLongError(String, usize),
}

impl fmt::Display for ValueError {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            ValueError::NameTooLongError(name, length) => write!(f, "The name '{}' is {} bytes long, which is longer than the allowed length of {} bytes.", name, length, MAX_NAME_LENGTH),
            ValueError::DescriptionTooLongError(name, length) => write!(f, "The description of the network '{}' is {} bytes long, which is longer than the allowed length of {} bytes.", name, length, MAX_DESCRIPTION_LENGTH),
        }
    }
}

impl error::Error for ValueError {
    fn source(&self) -> Option<&(dyn error::Error + 'static)> {
        match *self {
            ValueError::NameTooLongError(_, _) => None,
            ValueError::DescriptionTooLongError(_, _) => None,
        }
    }
}

#[derive(FromBytes, AsBytes, Unaligned, Eq, Hash, PartialEq)]
#[repr(C)]
pub struct BlockInfoKey {
    prefix: u8,
    pub network: U32<LittleEndian>,
    pub height: U64<LittleEndian>,
    hash_part: U128<BigEndian>,
}

impl BlockInfoKey {
    pub fn new(height: u64, block_hash: &BlockHash, network: u32) -> BlockInfoKey {
        BlockInfoKey {
            prefix: PREFIX_BLOCKINFO,
            network: U32::new(network),
            height: U64::new(height),
            hash_part: U128::new(short_hash(&block_hash)),
        }
    }
}

#[derive(FromBytes, AsBytes, Unaligned, Debug, Eq, Hash, PartialEq)]
#[repr(C)]
pub struct NetworkInfo {
    pub id: U32<BigEndian>,
    pub name: [u8; MAX_NAME_LENGTH],
    pub description: [u8; MAX_DESCRIPTION_LENGTH],
}

impl NetworkInfo {
    pub fn new(id: u32, name: &str, description: &str) -> Result<NetworkInfo, ValueError> {
        let name_bytes = name.as_bytes();
        if name_bytes.len() > MAX_NAME_LENGTH {
            return Err(ValueError::NameTooLongError(
                name.to_string(),
                name_bytes.len(),
            ));
        }

        let description_bytes = description.as_bytes();
        if description_bytes.len() > MAX_DESCRIPTION_LENGTH {
            return Err(ValueError::DescriptionTooLongError(
                name.to_string(),
                description_bytes.len(),
            ));
        }

        let mut name_array = [0u8; MAX_NAME_LENGTH];
        let mut description_array = [0u8; MAX_DESCRIPTION_LENGTH];

        for (i, b) in name_bytes.iter().enumerate() {
            name_array[i] = *b;
        }

        for (i, b) in description_bytes.iter().enumerate() {
            description_array[i] = *b;
        }

        Ok(NetworkInfo {
            id: U32::new(id),
            name: name_array,
            description: description_array,
        })
    }
}

#[derive(FromBytes, AsBytes, Unaligned, Eq, Hash, PartialEq)]
#[repr(C)]
pub struct NetworkInfoKey {
    prefix: u8,
    id: U32<BigEndian>,
}

impl NetworkInfoKey {
    pub fn new(id: u32) -> NetworkInfoKey {
        NetworkInfoKey {
            prefix: PREFIX_NETWORKS,
            id: U32::new(id),
        }
    }
}

#[derive(FromBytes, AsBytes, Unaligned, Debug, Eq, Hash, PartialEq)]
#[repr(C)]
pub struct NodeInfo {
    pub id: u8,
    pub name: [u8; MAX_NAME_LENGTH],
    pub description: [u8; MAX_DESCRIPTION_LENGTH],
}

impl NodeInfo {
    pub fn new(id: u8, name: &str, description: &str) -> Result<NodeInfo, ValueError> {
        let name_bytes = name.as_bytes();
        if name_bytes.len() > MAX_NAME_LENGTH {
            return Err(ValueError::NameTooLongError(
                name.to_string(),
                name_bytes.len(),
            ));
        }

        let description_bytes = description.as_bytes();
        if description_bytes.len() > MAX_DESCRIPTION_LENGTH {
            return Err(ValueError::DescriptionTooLongError(
                name.to_string(),
                description_bytes.len(),
            ));
        }

        let mut name_array = [0u8; MAX_NAME_LENGTH];
        let mut description_array = [0u8; MAX_DESCRIPTION_LENGTH];

        for (i, b) in name_bytes.iter().enumerate() {
            name_array[i] = *b;
        }

        for (i, b) in description_bytes.iter().enumerate() {
            description_array[i] = *b;
        }

        Ok(NodeInfo {
            id,
            name: name_array,
            description: description_array,
        })
    }
}

#[derive(FromBytes, AsBytes, Unaligned, Eq, Hash, PartialEq)]
#[repr(C)]
pub struct NodeInfoKey {
    prefix: u8,
    network: U32<BigEndian>,
    id: u8,
}

impl NodeInfoKey {
    pub fn new(network: u32, id: u8) -> NodeInfoKey {
        NodeInfoKey {
            prefix: PREFIX_NODES,
            network: U32::new(network),
            id,
        }
    }
}

#[derive(FromBytes, AsBytes, Unaligned, Debug, Eq, Hash, PartialEq)]
#[repr(C)]
pub struct BlockInfo {
    pub height: U64<LittleEndian>,
    pub header: [u8; 80],
}

#[derive(FromBytes, AsBytes, Unaligned, Eq, Hash, PartialEq)]
#[repr(C)]
pub struct TipInfoKey {
    prefix: u8,
    network: U32<BigEndian>,
    node: u8,
    hash_part: U128<BigEndian>,
}

impl TipInfoKey {
    pub fn new(network: u32, node: u8, block_hash: &BlockHash) -> TipInfoKey {
        TipInfoKey {
            prefix: PREFIX_TIPINFO,
            network: U32::new(network),
            node,
            hash_part: U128::new(short_hash(&block_hash)),
        }
    }
}

#[derive(FromBytes, AsBytes, Unaligned, Debug, Eq, Hash, PartialEq)]
#[repr(C)]
pub struct TipInfo {
    height: U64<LittleEndian>,
    hash: [u8; 32],
    status: u8,
    node: u8,
}

impl TipInfo {
    pub fn new(tip: &GetChainTipsResultTip, node_id: u8) -> TipInfo {
        TipInfo {
            height: U64::new(tip.height),
            hash: tip.hash[0..32].try_into().unwrap(),
            status: TipStatus::from(tip.status).into(),
            node: node_id,
        }
    }
}

#[derive(Serialize)]
pub struct BlockInfoJson {
    pub block_height: u64,
    pub hash: String,
    pub prev: String,
}

impl BlockInfoJson {
    pub fn new(block_info: &BlockInfo) -> BlockInfoJson {
        let header: BlockHeader =
            bitcoincore_rpc::bitcoin::consensus::deserialize(&block_info.header).unwrap();
        BlockInfoJson {
            block_height: u64::from(block_info.height),
            hash: header.block_hash().to_string(),
            prev: if header.prev_blockhash == BlockHash::default() {
                "".to_string()
            } else {
                header.prev_blockhash.to_string()
            },
        }
    }
}

#[derive(Serialize)]
pub struct TipInfoJson {
    pub block_height: u64,
    pub hash: String,
    pub status: String,
    pub node: u8,
}

impl TipInfoJson {
    pub fn new(tip_info: &TipInfo) -> TipInfoJson {
        let block_hash = BlockHash::from_inner(tip_info.hash);

        TipInfoJson {
            block_height: u64::from(tip_info.height),
            hash: block_hash.to_string(),
            status: TipStatus::try_from(tip_info.status).unwrap().to_string(),
            node: u8::from(tip_info.node),
        }
    }
}

#[derive(Deserialize)]
pub struct DataQuery {
    pub network: u32,
}

#[derive(Serialize)]
pub struct DataJsonResponse {
    pub block_infos: Vec<BlockInfoJson>,
    pub tip_infos: Vec<TipInfoJson>,
    pub nodes: Vec<NodeJson>,
}

#[derive(Serialize)]
pub struct NetworkJson {
    pub id: u32,
    pub name: String,
    pub description: String,
}

impl NetworkJson {
    pub fn new(network_info: &NetworkInfo) -> NetworkJson {
        NetworkJson {
            id: u32::from(network_info.id),
            name: String::from_utf8(network_info.name.to_vec())
                .unwrap()
                .trim_matches(char::from(0))
                .to_string(),
            description: String::from_utf8(network_info.description.to_vec())
                .unwrap()
                .trim_matches(char::from(0))
                .to_string(),
        }
    }
}

#[derive(Serialize)]
pub struct NodeJson {
    pub id: u8,
    pub name: String,
    pub description: String,
}

impl NodeJson {
    pub fn new(node_info: &NodeInfo) -> NodeJson {
        NodeJson {
            id: u8::from(node_info.id),
            name: String::from_utf8(node_info.name.to_vec())
                .unwrap()
                .trim_matches(char::from(0))
                .to_string(),
            description: String::from_utf8(node_info.description.to_vec())
                .unwrap()
                .trim_matches(char::from(0))
                .to_string(),
        }
    }
}

#[derive(Serialize)]
pub struct NetworksJsonResponse {
    pub networks: Vec<NetworkJson>,
}

fn short_hash(hash: &BlockHash) -> u128 {
    assert_eq!(hash.len(), 32);
    let mut short_hash = 0u128;
    for i in 15..=31 {
        short_hash = short_hash + ((hash[i] as u128) << i) as u128;
    }
    short_hash
}
