use super::esplora::Esplora;
use super::{Capabilities, HeaderFetchType, Node, NodeInfo};
use crate::error::{EsploraRESTError, FetchError};
use crate::types::ChainTip;
use async_trait::async_trait;
use corepc_client::bitcoin::blockdata::block::Header;
use corepc_client::bitcoin::hex::FromHex;
use corepc_client::bitcoin::{BlockHash, Transaction};
use log::debug;
use serde::Deserialize;

/// The subset of mempool.space's `/api/v1/block/{hash}` JSON we use:
/// `extras.header` is the consensus-serialized block header in hex.
#[derive(Deserialize)]
struct MempoolSpaceBlock {
    extras: MempoolSpaceBlockExtras,
}

#[derive(Deserialize)]
struct MempoolSpaceBlockExtras {
    header: String,
}

#[derive(Deserialize)]
struct MempoolSpaceBackendInfo {
    version: String,
}

/// Deserializes the header in `block.extras` and checks that it hashes to `hash`,
/// the hash the caller asked for. mempool.space's chain-tips API isn't stabilized
/// yet, so this guards against schema drift silently attributing a header to the
/// wrong block.
fn header_from_mempool_space_block(
    hash: &BlockHash,
    block: &MempoolSpaceBlock,
) -> Result<Header, FetchError> {
    let header_bytes = Vec::from_hex(&block.extras.header).map_err(|e| {
        FetchError::DataError(format!(
            "Can't hex decode block header '{}' for block {}: {}",
            block.extras.header, hash, e
        ))
    })?;
    let header: Header =
        corepc_client::bitcoin::consensus::deserialize(&header_bytes).map_err(|e| {
            FetchError::DataError(format!(
                "Can't deserialize block header '{}' for block {}: {}",
                block.extras.header, hash, e
            ))
        })?;

    if header.block_hash() != *hash {
        return Err(FetchError::DataError(format!(
            "mempool.space block JSON for {} produced a header with a different hash ({})",
            hash,
            header.block_hash()
        )));
    }

    Ok(header)
}

/// mempool.space is Esplora-API-compatible for active-chain blocks, but additionally
/// exposes a getchaintips-like `/api/v1/chain-tips` endpoint. This is used to fetch
/// stale/fork chain tips, which plain Esplora instances can't report. Fork-branch
/// headers are not available through the Esplora header endpoint (it 404s for
/// blocks off the active chain), so they are read from `/api/v1/block/{hash}`
/// instead.
pub struct MempoolSpace {
    info: NodeInfo,
    esplora: Esplora,
}

impl MempoolSpace {
    pub fn new(info: NodeInfo, api_url: String) -> Self {
        MempoolSpace {
            esplora: Esplora::new(info.clone(), api_url),
            info,
        }
    }

    /// GETs `{api_url}/v1{path}` and deserializes the JSON response body.
    async fn get_json<T: serde::de::DeserializeOwned>(&self, path: &str) -> Result<T, FetchError> {
        let url = format!("{}/v1{}", self.rpc_url(), path);
        debug!("mempool.space: GET {}", url);

        let res = minreq::get(url.as_str()).with_timeout(8).send()?;
        if res.status_code != 200 {
            return Err(FetchError::EsploraREST(EsploraRESTError::Http(format!(
                "HTTP request to {} failed: {} {}",
                url, res.status_code, res.reason_phrase,
            ))));
        }

        res.json().map_err(|e| {
            FetchError::DataError(format!("Can't parse JSON response from {}: {}", url, e))
        })
    }
}

#[async_trait]
impl Node for MempoolSpace {
    fn info(&self) -> NodeInfo {
        self.info.clone()
    }

    fn capabilities(&self) -> Capabilities {
        Capabilities {
            header_fetch_type: HeaderFetchType::Hash,
            // mempool.space has no bulk header endpoint - the best it offers is
            // 15 blocks per request, so a batch fetch of `new_active_headers`'
            // 2000 block window would cost ~134 requests every time the tip
            // moves. Fetching header by header instead lets `new_active_headers`
            // stop at the first header already in the tree, which is normally
            // the very first one: this backend is meant to run alongside a
            // Bitcoin Core node that shares the network's tree and has already
            // supplied the active chain.
            batch_header_fetch: false,
            // mempool.space 404s on `headers-only` tips it reports itself.
            partial_nonactive_headers: true,
        }
    }

    fn rpc_url(&self) -> String {
        self.esplora.rpc_url()
    }

    async fn version(&self) -> Result<String, FetchError> {
        let info: MempoolSpaceBackendInfo = self.get_json("/backend-info").await?;
        Ok(info.version)
    }

    async fn block_header_hash(&self, hash: &BlockHash) -> Result<Header, FetchError> {
        let block: MempoolSpaceBlock = self.get_json(&format!("/block/{}", hash)).await?;
        header_from_mempool_space_block(hash, &block)
    }

    async fn block_header_height(&self, _: u64) -> Result<Header, FetchError> {
        assert_eq!(self.capabilities().header_fetch_type, HeaderFetchType::Hash);
        Err(FetchError::DataError(
            "fetch by block height not implemented".to_string(),
        ))
    }

    async fn block_hash(&self, height: u64) -> Result<BlockHash, FetchError> {
        self.esplora.block_hash(height).await
    }

    async fn coinbase(&self, hash: &BlockHash, height: u64) -> Result<Transaction, FetchError> {
        self.esplora.coinbase(hash, height).await
    }

    async fn block(&self, hash: &BlockHash) -> Result<Vec<u8>, FetchError> {
        self.esplora.block(hash).await
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

    async fn tips(&self) -> Result<Vec<ChainTip>, FetchError> {
        self.get_json("/chain-tips").await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::ChainTipStatus;
    use std::str::FromStr;

    // A real response (trimmed to the fields we use) from
    // `https://mempool.space/api/v1/block/00000000000000000001e9c1b31d923680b0472cc3d8e301210dfca4631e2996`,
    // a `valid-fork` (stale) tip reported by `/api/v1/chain-tips` at the time of writing.
    const STALE_BLOCK_JSON: &str = r#"{
        "id": "00000000000000000001e9c1b31d923680b0472cc3d8e301210dfca4631e2996",
        "height": 951052,
        "extras": {
            "header": "00000038610942633517e2cd09edd97db9476d906ae59ea66ab400000000000000000000c65ea6e40547282a7d996633fe4f0484b81a870ea6a3376257180b26124eabd1c9f3146a790f02178ab1690f"
        }
    }"#;

    #[test]
    fn header_from_mempool_space_block_reconstructs_known_header() {
        let hash =
            BlockHash::from_str("00000000000000000001e9c1b31d923680b0472cc3d8e301210dfca4631e2996")
                .unwrap();
        let block: MempoolSpaceBlock = serde_json::from_str(STALE_BLOCK_JSON).unwrap();

        let header = header_from_mempool_space_block(&hash, &block)
            .expect("known-good block JSON should reconstruct a valid header");

        assert_eq!(header.block_hash(), hash);
        assert_eq!(
            header.prev_blockhash.to_string(),
            "00000000000000000000b46aa69ee56a906d47b97dd9ed09cde2173563420961"
        );
    }

    #[test]
    fn header_from_mempool_space_block_rejects_hash_mismatch() {
        // A hash that doesn't match the block content: the reconstructed header
        // must not silently be attributed to the wrong block.
        let wrong_hash =
            BlockHash::from_str("00000000000000000001e9c1b31d923680b0472cc3d8e301210dfca4631eDEAD")
                .unwrap();
        let block: MempoolSpaceBlock = serde_json::from_str(STALE_BLOCK_JSON).unwrap();

        let result = header_from_mempool_space_block(&wrong_hash, &block);
        assert!(result.is_err());
    }

    // `/api/v1/chain-tips` reports tips in the same shape as Bitcoin Core's
    // `getchaintips`, so they deserialize straight into `ChainTip`.
    #[test]
    fn chain_tips_json_parses() {
        let json = r#"[
            {"height":960315,"hash":"00000000000000000000c4dd83a223d8a32c9904c787948fd42c88c30739553a","branchlen":0,"status":"active"},
            {"height":951052,"hash":"00000000000000000001e9c1b31d923680b0472cc3d8e301210dfca4631e2996","branchlen":1,"status":"valid-fork"},
            {"height":922047,"hash":"00000000000000000000f05494b5ced664d0553b16d9dc188faa4eea96aa437c","branchlen":1,"status":"headers-only"},
            {"height":900000,"hash":"00000000000000000000e2ea2b4b3b594a6806d35319b85bc12c418dbbe2b566","branchlen":1,"status":"some-future-status"}
        ]"#;
        let tips: Vec<ChainTip> = serde_json::from_str(json).unwrap();

        assert_eq!(tips.len(), 4);
        assert_eq!(tips[0].status, ChainTipStatus::Active);
        assert_eq!(tips[1].status, ChainTipStatus::ValidFork);
        assert_eq!(tips[2].status, ChainTipStatus::HeadersOnly);
        // A status mempool.space might add later must not fail the whole response.
        assert_eq!(tips[3].status, ChainTipStatus::Unknown);
    }

    #[test]
    fn backend_info_json_parses() {
        let json = r#"{"hostname":"node208.fra.mempool.space","version":"3.4-dev","gitCommit":"3da515d4f","lightning":false,"backend":"esplora"}"#;
        let info: MempoolSpaceBackendInfo = serde_json::from_str(json).unwrap();
        assert_eq!(info.version, "3.4-dev");
    }
}
