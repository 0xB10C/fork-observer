use super::{Capabilities, HeaderFetchType, Node, NodeInfo};
use crate::blake2b::ParsedHeader;
use crate::error::{EsploraRESTError, FetchError};
use crate::types::{ChainTip, ChainTipStatus};
use async_trait::async_trait;
use corepc_client::bitcoin::hex::FromHex;
use corepc_client::bitcoin::{BlockHash, Transaction};
use std::str::FromStr;

#[derive(Hash, Clone)]
pub struct Esplora {
    info: NodeInfo,
    api_url: String,
}

impl Esplora {
    pub fn new(info: NodeInfo, api_url: String) -> Self {
        Esplora { info, api_url }
    }
}

#[async_trait]
impl Node for Esplora {
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
        self.api_url.clone()
    }

    async fn version(&self) -> Result<String, FetchError> {
        Err(FetchError::EsploraREST(EsploraRESTError::NotImplemented))
    }

    async fn block_header_hash(&self, hash: &BlockHash) -> Result<ParsedHeader, FetchError> {
        let url = format!("{}/block/{}/header", self.api_url, hash);

        let res = minreq::get(url.clone())
            .with_header("content-type", "plain/text")
            .with_timeout(8)
            .send()?;

        match res.status_code {
            200 => {
                let header_str = res.as_str()?;
                match Vec::from_hex(header_str) {
                    Ok(header_bytes) => match crate::blake2b::parse_header(&header_bytes) {
                        Ok((header, v2, _)) => Ok(ParsedHeader { header, v2 }),
                        Err(e) => Err(FetchError::DataError(format!(
                            "Can't deserialize block header '{}': {}",
                            header_str, e
                        ))),
                    },
                    Err(e) => Err(FetchError::DataError(format!(
                        "Can't hex decode block header '{}': {}",
                        header_str, e
                    ))),
                }
            }
            _ => {
                return Err(FetchError::EsploraREST(EsploraRESTError::Http(format!(
                    "HTTP request to {} failed: {} {}: {}",
                    url,
                    res.status_code,
                    res.reason_phrase,
                    res.as_str()?
                ))));
            }
        }
    }

    async fn block_header_height(&self, _: u64) -> Result<ParsedHeader, FetchError> {
        assert_eq!(self.capabilities().header_fetch_type, HeaderFetchType::Hash);
        Err(FetchError::DataError(
            "fetch by block height not implemented".to_string(),
        ))
    }

    async fn coinbase(&self, hash: &BlockHash, _height: u64) -> Result<Transaction, FetchError> {
        let url = format!("{}/block/{}/txid/0", self.api_url, hash);

        let res = minreq::get(url.clone())
            .with_header("content-type", "plain/text")
            .with_timeout(8)
            .send()?;

        match res.status_code {
            200 => {
                let url = format!("{}/tx/{}/hex", self.api_url, res.as_str()?);
                let coinbase_hex = res.as_str()?;
                let res = minreq::get(url.clone())
                    .with_header("content-type", "plain/text")
                    .with_timeout(8)
                    .send()?;

                match res.status_code {
                    200 => match Vec::from_hex(coinbase_hex) {
                        Ok(coinbase_bytes) => {
                            match corepc_client::bitcoin::consensus::deserialize(&coinbase_bytes) {
                                Ok(tx) => Ok(tx),
                                Err(e) => Err(FetchError::DataError(format!(
                                    "Can't deserialize coinbase transaction '{}': {}",
                                    coinbase_hex, e
                                ))),
                            }
                        }
                        Err(e) => Err(FetchError::DataError(format!(
                            "Can't hex decode coinbase transaction '{}': {}",
                            coinbase_hex, e
                        ))),
                    },
                    _ => {
                        return Err(FetchError::EsploraREST(EsploraRESTError::Http(format!(
                            "HTTP request to {} failed: {} {}: {}",
                            url,
                            res.status_code,
                            res.reason_phrase,
                            res.as_str()?
                        ))));
                    }
                }
            }
            _ => {
                return Err(FetchError::EsploraREST(EsploraRESTError::Http(format!(
                    "HTTP request to {} failed: {} {}: {}",
                    url,
                    res.status_code,
                    res.reason_phrase,
                    res.as_str()?
                ))));
            }
        }
    }

    async fn block(&self, hash: &BlockHash) -> Result<Vec<u8>, FetchError> {
        let url = format!("{}/block/{}/raw", self.api_url, hash);

        let res = minreq::get(url.clone()).with_timeout(8).send()?;

        match res.status_code {
            200 => Ok(res.as_bytes().to_vec()),
            _ => Err(FetchError::EsploraREST(EsploraRESTError::Http(format!(
                "HTTP request to {} failed: {} {}: {}",
                url,
                res.status_code,
                res.reason_phrase,
                res.as_str()?
            )))),
        }
    }

    async fn block_hash(&self, height: u64) -> Result<BlockHash, FetchError> {
        let url = format!("{}/block-height/{}", self.api_url, height);

        let res = minreq::get(url.clone())
            .with_header("content-type", "plain/text")
            .with_timeout(8)
            .send()?;

        match res.status_code {
            200 => {
                let hash_str = res.as_str()?;
                match BlockHash::from_str(hash_str) {
                    Ok(hash) => Ok(hash),
                    Err(e) => Err(FetchError::DataError(format!(
                        "Invalid block hash '{}': {}",
                        hash_str, e
                    ))),
                }
            }
            _ => {
                return Err(FetchError::EsploraREST(EsploraRESTError::Http(format!(
                    "HTTP request to {} failed: {} {}: {}",
                    url,
                    res.status_code,
                    res.reason_phrase,
                    res.as_str()?
                ))));
            }
        }
    }

    async fn batch_header_fetch(
        &self,
        _start_height: u64,
        _count: u64,
    ) -> Result<Vec<ParsedHeader>, FetchError> {
        assert!(self.capabilities().batch_header_fetch);
        Err(FetchError::DataError(
            "batch header fetch not implemented".to_string(),
        ))
    }

    async fn tips(&self) -> Result<Vec<ChainTip>, FetchError> {
        // https://mempool.space/api/blocks/tip/height
        // The Esplora API doesn't have an endpoint similar to getchaintips.
        // However, we can get the active tip and "fake" a getchaintips result.
        // This only properly works with at least one other Bitcoin Core or btcd
        // backend for the same network.
        let url = format!("{}/blocks/tip/height", self.api_url);

        let res = minreq::get(url.clone())
            .with_header("content-type", "plain/text")
            .with_timeout(8)
            .send()?;

        match res.status_code {
            200 => {
                let height_str = res.as_str()?;
                match height_str.parse::<u64>() {
                    Ok(height) => {
                        let hash = self.block_hash(height).await?;
                        Ok(vec![ChainTip {
                            height,
                            hash: hash.to_string(),
                            branchlen: 0,
                            status: ChainTipStatus::Active,
                        }])
                    }
                    Err(e) => Err(FetchError::DataError(format!(
                        "Invalid block height '{}': {}",
                        height_str, e
                    ))),
                }
            }
            _ => {
                return Err(FetchError::EsploraREST(EsploraRESTError::Http(format!(
                    "HTTP request to {} failed: {} {}: {}",
                    url,
                    res.status_code,
                    res.reason_phrase,
                    res.as_str()?
                ))));
            }
        }
    }
}
