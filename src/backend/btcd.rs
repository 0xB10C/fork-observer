use super::{Capabilities, HeaderFetchType, Node, NodeInfo};
use crate::error::{FetchError, JsonRPCError};
use crate::types::ChainTip;
use async_trait::async_trait;
use corepc_client::bitcoin::blockdata::block::Header;
use corepc_client::bitcoin::{BlockHash, Transaction};

#[derive(Hash, Clone)]
pub struct BtcdNode {
    info: NodeInfo,
    rpc_url: String,
    rpc_user: String,
    rpc_password: String,
}

impl BtcdNode {
    pub fn new(info: NodeInfo, rpc_url: String, rpc_user: String, rpc_password: String) -> Self {
        BtcdNode {
            info,
            rpc_url,
            rpc_user,
            rpc_password,
        }
    }
}

#[async_trait]
impl Node for BtcdNode {
    fn info(&self) -> NodeInfo {
        self.info.clone()
    }

    fn capabilities(&self) -> Capabilities {
        Capabilities {
            header_fetch_type: HeaderFetchType::Hash,
            batch_header_fetch: false,
            partial_nonactive_headers: false,
        }
    }

    fn rpc_url(&self) -> String {
        self.rpc_url.clone()
    }

    async fn version(&self) -> Result<String, FetchError> {
        Err(FetchError::BtcdRPC(JsonRPCError::NotImplemented))
    }

    async fn block_header_hash(&self, hash: &BlockHash) -> Result<Header, FetchError> {
        let url = format!("{}/", self.rpc_url);
        match crate::jsonrpc::btcd_blockheader(
            url,
            self.rpc_user.clone(),
            self.rpc_password.clone(),
            hash.to_string(),
        ) {
            Ok(header) => Ok(header),
            Err(error) => Err(FetchError::BtcdRPC(error)),
        }
    }

    async fn block_header_height(&self, _: u64) -> Result<Header, FetchError> {
        assert_eq!(self.capabilities().header_fetch_type, HeaderFetchType::Hash);
        Err(FetchError::DataError(
            "fetch by block height not implemented".to_string(),
        ))
    }

    async fn batch_header_fetch(
        &self,
        _start_height: u64,
        _count: u64,
    ) -> Result<Vec<Header>, FetchError> {
        assert!(self.capabilities().batch_header_fetch);
        Err(FetchError::DataError(
            "batch header fetch not implemented".to_string(),
        ))
    }

    async fn coinbase(&self, hash: &BlockHash, _height: u64) -> Result<Transaction, FetchError> {
        let url = format!("{}/", self.rpc_url);
        match crate::jsonrpc::btcd_block(
            url,
            self.rpc_user.clone(),
            self.rpc_password.clone(),
            hash.to_string(),
        ) {
            Ok(block) => Ok(block
                .txdata
                .first()
                .expect("Block should have a coinbase transaction")
                .clone()),
            Err(error) => Err(FetchError::BtcdRPC(error)),
        }
    }

    async fn block(&self, hash: &BlockHash) -> Result<Vec<u8>, FetchError> {
        let url = format!("{}/", self.rpc_url);
        match crate::jsonrpc::btcd_block(
            url,
            self.rpc_user.clone(),
            self.rpc_password.clone(),
            hash.to_string(),
        ) {
            Ok(block) => Ok(corepc_client::bitcoin::consensus::encode::serialize(&block)),
            Err(error) => Err(FetchError::BtcdRPC(error)),
        }
    }

    async fn block_hash(&self, height: u64) -> Result<BlockHash, FetchError> {
        let url = format!("{}/", self.rpc_url);
        match crate::jsonrpc::btcd_blockhash(
            url,
            self.rpc_user.clone(),
            self.rpc_password.clone(),
            height,
        ) {
            Ok(tips) => Ok(tips),
            Err(error) => Err(FetchError::BtcdRPC(error)),
        }
    }

    async fn tips(&self) -> Result<Vec<ChainTip>, FetchError> {
        let url = format!("{}/", self.rpc_url);
        match crate::jsonrpc::btcd_chaintips(url, self.rpc_user.clone(), self.rpc_password.clone())
        {
            Ok(tips) => Ok(tips),
            Err(error) => Err(FetchError::BtcdRPC(error)),
        }
    }
}
