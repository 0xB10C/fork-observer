use super::{Capabilities, HeaderFetchType, Node, NodeInfo};
use crate::error::{EsploraRESTError, FetchError};
use crate::types::{ChainTip, ChainTipStatus};
use async_trait::async_trait;
use corepc_client::bitcoin::blockdata::block::Header;
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

    async fn block_header_hash(&self, hash: &BlockHash) -> Result<Header, FetchError> {
        let url = format!("{}/block/{}/header", self.api_url, hash);

        let res = minreq::get(url.clone())
            .with_header("content-type", "plain/text")
            .with_timeout(8)
            .send()?;

        match res.status_code {
            200 => {
                let header_str = res.as_str()?;
                match Vec::from_hex(header_str) {
                    Ok(header_bytes) => {
                        match corepc_client::bitcoin::consensus::deserialize(&header_bytes) {
                            Ok(header) => Ok(header),
                            Err(e) => Err(FetchError::DataError(format!(
                                "Can't deserialize block header '{}': {}",
                                header_str, e
                            ))),
                        }
                    }
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

    async fn block_header_height(&self, _: u64) -> Result<Header, FetchError> {
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
                let res = minreq::get(url.clone())
                    .with_header("content-type", "plain/text")
                    .with_timeout(8)
                    .send()?;

                match res.status_code {
                    200 => {
                        let coinbase_hex = res.as_str()?;
                        match Vec::from_hex(coinbase_hex) {
                            Ok(coinbase_bytes) => {
                                match corepc_client::bitcoin::consensus::deserialize(
                                    &coinbase_bytes,
                                ) {
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
    ) -> Result<Vec<Header>, FetchError> {
        assert!(!self.capabilities().batch_header_fetch);
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

#[cfg(test)]
mod tests {
    use super::super::test_chain;
    use super::super::test_server::{serve, Request, Response};
    use super::*;
    use corepc_client::bitcoin::consensus::serialize;
    use corepc_client::bitcoin::hashes::Hash as _;

    fn node_for(api_url: &str) -> Esplora {
        Esplora::new(
            NodeInfo {
                id: 4,
                name: "esplora".to_string(),
                description: String::new(),
                implementation: "esplora".to_string(),
            },
            api_url.to_string(),
        )
    }

    #[tokio::test]
    async fn a_header_is_fetched_by_hash_as_hex() {
        let header = test_chain::header(BlockHash::all_zeros(), 1);
        let hash = header.block_hash();
        let header_hex = hex::encode(serialize(&header));
        let server = serve(move |request: &Request| {
            match request.path == format!("/block/{}/header", hash) {
                true => Response::ok(&header_hex),
                false => Response::status(404),
            }
        });
        let node = node_for(&server.url);

        assert_eq!(
            node.block_header_hash(&hash).await.expect("the header"),
            header
        );
        // A block the instance doesn't have.
        assert!(matches!(
            node.block_header_hash(&BlockHash::all_zeros()).await,
            Err(FetchError::EsploraREST(_))
        ));
    }

    #[tokio::test]
    async fn a_header_that_isnt_hex_or_isnt_a_header_is_an_error() {
        let not_hex = test_chain::header(BlockHash::all_zeros(), 1).block_hash();
        let too_short = test_chain::header(BlockHash::all_zeros(), 2).block_hash();
        let server = serve(move |request: &Request| {
            if request.path == format!("/block/{}/header", not_hex) {
                Response::ok("this is not hex")
            } else if request.path == format!("/block/{}/header", too_short) {
                Response::ok("00112233")
            } else {
                Response::status(404)
            }
        });
        let node = node_for(&server.url);

        assert!(matches!(
            node.block_header_hash(&not_hex).await,
            Err(FetchError::DataError(_))
        ));
        assert!(matches!(
            node.block_header_hash(&too_short).await,
            Err(FetchError::DataError(_))
        ));
    }

    #[tokio::test]
    async fn the_active_tip_is_the_hash_at_the_tip_height() {
        let hash = test_chain::header(BlockHash::all_zeros(), 1).block_hash();
        let server = serve(move |request: &Request| match request.path.as_str() {
            "/blocks/tip/height" => Response::ok("830000"),
            "/block-height/830000" => Response::ok(&hash.to_string()),
            _ => Response::status(404),
        });
        let node = node_for(&server.url);

        // Esplora has no getchaintips, so the active tip is all there is.
        let tips = node.tips().await.expect("the tips");
        assert_eq!(tips.len(), 1);
        assert_eq!(tips[0].height, 830_000);
        assert_eq!(tips[0].hash, hash.to_string());
        assert_eq!(tips[0].branchlen, 0);
        assert_eq!(tips[0].status, ChainTipStatus::Active);

        assert_eq!(node.block_hash(830_000).await.expect("the hash"), hash);
        assert_eq!(
            server.paths(),
            [
                "/blocks/tip/height",
                "/block-height/830000",
                "/block-height/830000"
            ]
        );
    }

    #[tokio::test]
    async fn a_tip_height_or_a_block_hash_that_doesnt_parse_is_an_error() {
        let server = serve(|request: &Request| match request.path.as_str() {
            "/blocks/tip/height" => Response::ok("soon"),
            "/block-height/1" => Response::ok("not a block hash"),
            _ => Response::status(500),
        });
        let node = node_for(&server.url);

        assert!(matches!(node.tips().await, Err(FetchError::DataError(_))));
        assert!(matches!(
            node.block_hash(1).await,
            Err(FetchError::DataError(_))
        ));
        assert!(matches!(
            node.block_hash(2).await,
            Err(FetchError::EsploraREST(_))
        ));
    }

    #[tokio::test]
    async fn the_coinbase_is_the_transaction_behind_the_blocks_first_txid() {
        let block = test_chain::block_with_a_coinbase();
        let hash = block.block_hash();
        let coinbase = block.txdata[0].clone();
        let txid = coinbase.compute_txid();
        let coinbase_hex = hex::encode(serialize(&coinbase));
        let raw_block = serialize(&block);
        let served_block = raw_block.clone();
        let server = serve(move |request: &Request| {
            if request.path == format!("/block/{}/txid/0", hash) {
                Response::ok(&txid.to_string())
            } else if request.path == format!("/tx/{}/hex", txid) {
                Response::ok(&coinbase_hex)
            } else if request.path == format!("/block/{}/raw", hash) {
                Response::bytes(200, served_block.clone())
            } else {
                Response::status(404)
            }
        });
        let node = node_for(&server.url);

        assert_eq!(
            node.coinbase(&hash, 1).await.expect("the coinbase"),
            coinbase
        );
        assert_eq!(node.block(&hash).await.expect("the block"), raw_block);
        assert!(matches!(
            node.block(&BlockHash::all_zeros()).await,
            Err(FetchError::EsploraREST(_))
        ));
    }

    #[tokio::test]
    async fn a_coinbase_the_instance_doesnt_serve_is_an_error() {
        let block = test_chain::block_with_a_coinbase();
        let hash = block.block_hash();
        let txid = block.txdata[0].compute_txid();
        let server = serve(move |request: &Request| {
            match request.path == format!("/block/{}/txid/0", hash) {
                true => Response::ok(&txid.to_string()),
                // The transaction itself isn't served.
                false => Response::status(404),
            }
        });
        let node = node_for(&server.url);

        assert!(matches!(
            node.coinbase(&hash, 1).await,
            Err(FetchError::EsploraREST(_))
        ));
        // A block the instance doesn't have.
        assert!(matches!(
            node.coinbase(&BlockHash::all_zeros(), 1).await,
            Err(FetchError::EsploraREST(_))
        ));
    }

    #[tokio::test]
    async fn esplora_has_no_version_no_batches_and_no_headers_by_height() {
        // Nothing here makes a request.
        let node = node_for("http://127.0.0.1:1");

        assert!(matches!(
            node.version().await,
            Err(FetchError::EsploraREST(EsploraRESTError::NotImplemented))
        ));
        assert!(matches!(
            node.block_header_height(1).await,
            Err(FetchError::DataError(_))
        ));
        assert!(matches!(
            node.batch_header_fetch(0, 10).await,
            Err(FetchError::DataError(_))
        ));
        assert_eq!(node.rpc_url(), "http://127.0.0.1:1");
        assert_eq!(node.info().name, "esplora");
        assert!(!node.capabilities().batch_header_fetch);
        assert_eq!(node.capabilities().header_fetch_type, HeaderFetchType::Hash);
    }
}
