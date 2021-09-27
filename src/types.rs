use bitcoincore_rpc::bitcoin::hash_types::BlockHash;
use bitcoincore_rpc::bitcoin::hashes::Hash;
use bitcoincore_rpc::bitcoin::BlockHeader;
use bitcoincore_rpc::json::{GetChainTipsResultStatus, GetChainTipsResultTip};
use byteorder::{BigEndian, LittleEndian};
use serde::{Deserialize, Serialize};
use zerocopy::{byteorder::U128, byteorder::U64, AsBytes, FromBytes, Unaligned};

use std::convert::{TryFrom, TryInto};

pub const PREFIX_BLOCKINFO: u8 = b'b';
pub const PREFIX_TIPINFO: u8 = b't';

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

#[derive(FromBytes, AsBytes, Unaligned, Eq, Hash, PartialEq)]
#[repr(C)]
pub struct BlockInfoKey {
    prefix: u8,
    height: U64<BigEndian>,
    hash_part: U128<BigEndian>,
}

impl BlockInfoKey {
    pub fn new(height: u64, block_hash: &BlockHash) -> BlockInfoKey {
        BlockInfoKey {
            prefix: PREFIX_BLOCKINFO,
            height: U64::new(height),
            hash_part: U128::new(short_hash(&block_hash)),
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
    network_id: U64<BigEndian>,
    node_id: U64<BigEndian>,
    hash_part: U128<BigEndian>,
}

impl TipInfoKey {
    pub fn new(block_hash: &BlockHash, network_id: u64, node_id: u64) -> TipInfoKey {
        TipInfoKey {
            prefix: PREFIX_TIPINFO,
            network_id: U64::new(network_id),
            node_id: U64::new(node_id),
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
}

impl TipInfo {
    pub fn new(tip: &GetChainTipsResultTip) -> TipInfo {
        TipInfo {
            height: U64::new(tip.height),
            hash: tip.hash[0..32].try_into().unwrap(),
            status: TipStatus::from(tip.status).into(),
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
}

impl TipInfoJson {
    pub fn new(tip_info: &TipInfo) -> TipInfoJson {
        let block_hash = BlockHash::from_inner(tip_info.hash);

        TipInfoJson {
            block_height: u64::from(tip_info.height),
            hash: block_hash.to_string(),
            status: TipStatus::try_from(tip_info.status).unwrap().to_string(),
        }
    }
}

#[derive(Deserialize)]
pub struct DataQuery {
    pub network: u64,
}

#[derive(Serialize)]
pub struct JsonResponse {
    pub block_infos: Vec<BlockInfoJson>,
    pub tip_infos: Vec<TipInfoJson>,
}

fn short_hash(hash: &BlockHash) -> u128 {
    assert_eq!(hash.len(), 32);
    let mut short_hash = 0u128;
    for i in 15..=31 {
        short_hash = short_hash + ((hash[i] as u128) << i) as u128;
    }
    short_hash
}
