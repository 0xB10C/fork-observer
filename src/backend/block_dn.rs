use super::{Capabilities, HeaderFetchType, Node, NodeInfo};
use crate::error::FetchError;
use crate::types::{ChainTip, ChainTipStatus};
use async_trait::async_trait;
use corepc_client::bitcoin::blockdata::block::Header;
use corepc_client::bitcoin::{Block, BlockHash, Transaction};
use serde::Deserialize;

const BLOCKDN_HEADER_SIZE: u64 = 80;

/// Longer than the 8s used elsewhere in this module: the still-growing tip
/// file is served whole and can be several MB.
const BLOCKDN_HEADER_FILE_TIMEOUT_SECS: u64 = 30;

/// The number of headers per `/headers/<file_start>` file. The public mainnet,
/// testnet3, testnet4 and signet instances all use this; a `--regtest` instance
/// uses 2,000 instead and is not supported (see README).
const BLOCKDN_HEADERS_PER_FILE: u64 = 100_000;

/// Computes the `(file_start_height, byte_start, byte_end_inclusive)` requests
/// covering `count` headers from `height`. block-dn serves headers in files of
/// `BLOCKDN_HEADERS_PER_FILE` headers, addressed by a `file_start` that is a
/// multiple of that.
fn blockdn_header_requests(height: u64, count: u64) -> Vec<(u64, u64, u64)> {
    let mut requests = Vec::new();
    let mut remaining = count;
    let mut current = height;
    while remaining > 0 {
        let file_start = (current / BLOCKDN_HEADERS_PER_FILE) * BLOCKDN_HEADERS_PER_FILE;
        let offset_in_file = current - file_start;
        let available_in_file = BLOCKDN_HEADERS_PER_FILE - offset_in_file;
        let take = remaining.min(available_in_file);
        let byte_start = offset_in_file * BLOCKDN_HEADER_SIZE;
        let byte_end = byte_start + take * BLOCKDN_HEADER_SIZE - 1;
        requests.push((file_start, byte_start, byte_end));
        current += take;
        remaining -= take;
    }
    requests
}

#[derive(Deserialize)]
struct BlockDnStatus {
    best_block_height: u64,
    best_block_hash: String,
    /// Undocumented, so optional. Only `version()` needs it.
    version: Option<String>,
}

pub struct BlockDn {
    info: NodeInfo,
    api_url: String,
}

impl BlockDn {
    pub fn new(info: NodeInfo, api_url: String) -> Self {
        BlockDn { info, api_url }
    }

    async fn fetch_status(&self) -> Result<BlockDnStatus, FetchError> {
        let url = format!("{}/status", self.api_url);
        let res = minreq::get(url.clone()).with_timeout(8).send()?;

        if res.status_code != 200 {
            return Err(FetchError::BlockDnREST(format!(
                "could not load status from {}: {} {}: {:?}",
                url,
                res.status_code,
                res.reason_phrase,
                res.as_str(),
            )));
        }

        res.json()
            .map_err(|e| FetchError::BlockDnREST(format!("could not parse status: {}", e)))
    }

    /// Fetches `byte_start..=byte_end` of `/headers/<file_start>` via an HTTP
    /// `Range` header, returning just those bytes. A short (or empty) result
    /// means the range ran past the current chain tip.
    ///
    /// block-dn groups the chain's headers into files of
    /// `BLOCKDN_HEADERS_PER_FILE` headers, each addressed by the height of its
    /// first header. A file is only written to disk once its full height range
    /// is complete - block-dn calls such a file "sealed" and keeps the headers
    /// above the newest sealed file in an in-memory tail. The two are served
    /// differently, which is why the response code matters here:
    ///
    /// - A sealed file is served from disk through Go's `http.ServeContent`,
    ///   which handles `Range` and answers `206` with exactly the bytes asked
    ///   for.
    /// - The unsealed tail is streamed from memory with a plain `200` and
    ///   `Range` ignored, so the response carries every header from
    ///   `file_start` up to the current tip and we slice out our part locally.
    async fn fetch_header_slice(
        &self,
        file_start: u64,
        byte_start: u64,
        byte_end: u64,
    ) -> Result<Vec<u8>, FetchError> {
        let url = format!("{}/headers/{}", self.api_url, file_start);
        let res = minreq::get(url.clone())
            .with_header("Range", format!("bytes={}-{}", byte_start, byte_end))
            .with_timeout(BLOCKDN_HEADER_FILE_TIMEOUT_SECS)
            .send()?;

        let expected_len = (byte_end - byte_start + 1) as usize;
        match res.status_code {
            // A sealed file, or a CDN serving our range out of its cached copy
            // of an unsealed one.
            206 => {
                let mut bytes = res.into_bytes();
                bytes.truncate(expected_len);
                Ok(bytes)
            }
            // The unsealed tail, served whole. This grows to
            // `BLOCKDN_HEADERS_PER_FILE * BLOCKDN_HEADER_SIZE` (8 MB) just
            // before the file is sealed, and we pay for it once per new block.
            // Public instances are behind a CDN that caches the response and
            // answers our `Range` from it, so in practice this arm is only
            // reached when talking to an instance directly.
            200 => {
                let file = res.into_bytes();
                let start = byte_start as usize;
                if start >= file.len() {
                    return Ok(Vec::new());
                }
                let end = std::cmp::min(file.len(), start + expected_len);
                Ok(file[start..end].to_vec())
            }
            // The range starts past the chain tip.
            416 => Ok(Vec::new()),
            _ => Err(FetchError::BlockDnREST(format!(
                "could not load headers from {}: {} {}: {:?}",
                url,
                res.status_code,
                res.reason_phrase,
                res.as_str(),
            ))),
        }
    }

    /// Fetches `count` consecutive active-chain headers starting at `height`,
    /// returning fewer if the range runs past the chain tip.
    async fn fetch_headers_range(
        &self,
        height: u64,
        count: u64,
    ) -> Result<Vec<Header>, FetchError> {
        let mut bytes: Vec<u8> = Vec::new();
        for (file_start, byte_start, byte_end) in blockdn_header_requests(height, count) {
            let expected_len = (byte_end - byte_start + 1) as usize;
            let slice = self
                .fetch_header_slice(file_start, byte_start, byte_end)
                .await?;

            let usable_len = slice.len() - (slice.len() % BLOCKDN_HEADER_SIZE as usize);
            bytes.extend_from_slice(&slice[..usable_len]);
            // A short slice means we reached the chain tip.
            if slice.len() < expected_len {
                break;
            }
        }

        let header_results: Result<Vec<Header>, corepc_client::bitcoin::consensus::encode::Error> =
            bytes
                .chunks(BLOCKDN_HEADER_SIZE as usize)
                .map(corepc_client::bitcoin::consensus::deserialize::<Header>)
                .collect();

        header_results
            .map_err(|e| FetchError::BlockDnREST(format!("could not deserialize header: {}", e)))
    }
}

#[async_trait]
impl Node for BlockDn {
    fn info(&self) -> NodeInfo {
        self.info.clone()
    }

    fn capabilities(&self) -> Capabilities {
        Capabilities {
            header_fetch_type: HeaderFetchType::Height,
            batch_header_fetch: true,
            partial_nonactive_headers: false,
        }
    }

    fn rpc_url(&self) -> String {
        self.api_url.clone()
    }

    async fn version(&self) -> Result<String, FetchError> {
        self.fetch_status()
            .await?
            .version
            .ok_or_else(|| FetchError::BlockDnREST("/status did not include a version".to_string()))
    }

    async fn block_header_hash(&self, _hash: &BlockHash) -> Result<Header, FetchError> {
        assert_eq!(
            self.capabilities().header_fetch_type,
            HeaderFetchType::Height
        );
        Err(FetchError::DataError(
            "fetch by block hash not implemented".to_string(),
        ))
    }

    async fn block_header_height(&self, height: u64) -> Result<Header, FetchError> {
        self.fetch_headers_range(height, 1)
            .await?
            .into_iter()
            .next()
            .ok_or_else(|| FetchError::DataError(format!("no header at height {}", height)))
    }

    async fn block_hash(&self, height: u64) -> Result<BlockHash, FetchError> {
        Ok(self.block_header_height(height).await?.block_hash())
    }

    async fn tips(&self) -> Result<Vec<ChainTip>, FetchError> {
        // block-dn has no `getchaintips`-like endpoint, so - like the Esplora
        // backend - we report the single active tip from `/status`. Forks are
        // only visible if another Bitcoin Core or btcd backend covers the same
        // network.
        let status = self.fetch_status().await?;

        Ok(vec![ChainTip {
            height: status.best_block_height,
            hash: status.best_block_hash,
            branchlen: 0,
            status: ChainTipStatus::Active,
        }])
    }

    async fn coinbase(&self, hash: &BlockHash, _height: u64) -> Result<Transaction, FetchError> {
        let block_bytes = self.block(hash).await?;
        let block: Block = corepc_client::bitcoin::consensus::deserialize(&block_bytes)
            .map_err(|e| FetchError::BlockDnREST(format!("could not deserialize block: {}", e)))?;
        // block-dn is an untrusted source, so verify the block is the one we
        // asked for and that its transactions match the header's merkle root
        // before handing the coinbase to pool identification.
        if block.block_hash() != *hash {
            return Err(FetchError::DataError(format!(
                "requested block {} but got block {}",
                hash,
                block.block_hash()
            )));
        }
        if !block.check_merkle_root() {
            return Err(FetchError::DataError(format!(
                "transactions of block {} don't match its merkle root",
                hash
            )));
        }
        block
            .txdata
            .first()
            .cloned()
            .ok_or_else(|| FetchError::DataError(format!("block {} has no transactions", hash)))
    }

    async fn block(&self, hash: &BlockHash) -> Result<Vec<u8>, FetchError> {
        let url = format!("{}/block/{}", self.api_url, hash);
        let res = minreq::get(url.clone()).with_timeout(8).send()?;

        match res.status_code {
            200 => Ok(res.as_bytes().to_vec()),
            _ => Err(FetchError::BlockDnREST(format!(
                "could not load block from {}: {} {}: {:?}",
                url,
                res.status_code,
                res.reason_phrase,
                res.as_str(),
            ))),
        }
    }

    async fn batch_header_fetch(
        &self,
        start_height: u64,
        count: u64,
    ) -> Result<Vec<Header>, FetchError> {
        self.fetch_headers_range(start_height, count).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn single_header_within_a_file() {
        let requests = blockdn_header_requests(5, 1);
        assert_eq!(requests, vec![(0, 400, 479)]);
    }

    #[test]
    fn range_within_a_file() {
        let requests = blockdn_header_requests(100, 50);
        assert_eq!(requests, vec![(0, 8000, 11999)]);
    }

    #[test]
    fn range_starting_at_a_file_boundary() {
        let requests = blockdn_header_requests(100_000, 10);
        assert_eq!(requests, vec![(100_000, 0, 799)]);
    }

    #[test]
    fn range_spanning_a_file_boundary() {
        // Last 3 headers of file 0, first 3 headers of file 1.
        let requests = blockdn_header_requests(99_997, 6);
        assert_eq!(requests, vec![(0, 7_999_760, 7_999_999), (100_000, 0, 239)]);
    }

    #[test]
    fn range_spanning_multiple_file_boundaries() {
        let requests = blockdn_header_requests(99_999, 100_002);
        assert_eq!(
            requests,
            vec![
                (0, 7_999_920, 7_999_999),
                (100_000, 0, 7_999_999),
                (200_000, 0, 79),
            ]
        );
    }
}
