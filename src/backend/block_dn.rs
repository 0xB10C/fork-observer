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

    use super::super::test_chain;
    use super::super::test_server::{serve, Request, Response};
    use corepc_client::bitcoin::consensus::serialize;
    use corepc_client::bitcoin::hashes::Hash as _;

    fn node_for(api_url: &str) -> BlockDn {
        BlockDn::new(
            NodeInfo {
                id: 7,
                name: "block-dn".to_string(),
                description: String::new(),
                implementation: "block-dn".to_string(),
            },
            api_url.to_string(),
        )
    }

    // `count` headers and the bytes a block-dn header file holds for them.
    fn header_file(count: u32) -> (Vec<Header>, Vec<u8>) {
        let headers = test_chain::chain(count);
        let bytes = headers.iter().flat_map(serialize).collect();
        (headers, bytes)
    }

    #[tokio::test]
    async fn the_status_gives_the_active_tip_and_the_version() {
        const TIP_HASH: &str = "00000000000000000000000000000000000000000000000000000000000000ab";
        let server = serve(|request: &Request| match request.path.as_str() {
            "/status" => Response::ok(&format!(
                r#"{{"best_block_height": 879000, "best_block_hash": "{}", "version": "block-dn 1.2.3"}}"#,
                TIP_HASH
            )),
            _ => Response::status(404),
        });
        let node = node_for(&server.url);

        let tips = node.tips().await.expect("the tips");

        // block-dn only knows the one tip it serves headers for.
        assert_eq!(tips.len(), 1);
        assert_eq!(tips[0].height, 879_000);
        assert_eq!(tips[0].hash, TIP_HASH);
        assert_eq!(tips[0].branchlen, 0);
        assert_eq!(tips[0].status, ChainTipStatus::Active);

        assert_eq!(node.version().await.expect("the version"), "block-dn 1.2.3");
        assert_eq!(node.rpc_url(), server.url);
        assert_eq!(node.info().id, 7);
        assert_eq!(server.paths(), ["/status", "/status"]);
    }

    #[tokio::test]
    async fn a_status_without_a_version_or_without_a_200_is_an_error() {
        let server = serve(|request: &Request| match request.path.as_str() {
            "/status" => Response::ok(
                r#"{"best_block_height": 1, "best_block_hash": "00000000000000000000000000000000000000000000000000000000000000ab"}"#,
            ),
            _ => Response::status(500),
        });

        // The version field is undocumented, so it can be missing.
        assert!(matches!(
            node_for(&server.url).version().await,
            Err(FetchError::BlockDnREST(_))
        ));

        // A status request that doesn't answer 200.
        let elsewhere = node_for(&format!("{}/not-block-dn", server.url));
        assert!(matches!(
            elsewhere.tips().await,
            Err(FetchError::BlockDnREST(_))
        ));
    }

    #[tokio::test]
    async fn a_sealed_file_answers_with_the_range_it_was_asked_for() {
        let (headers, bytes) = header_file(10);
        let server = serve(move |request: &Request| {
            if request.path != "/headers/0" || request.range.as_deref() != Some("bytes=400-479") {
                return Response::status(500);
            }
            // The range asked for, plus bytes we didn't ask for.
            Response::bytes(206, bytes[400..560].to_vec())
        });
        let node = node_for(&server.url);

        assert_eq!(
            node.batch_header_fetch(5, 1).await.expect("the header"),
            [headers[5]]
        );
        assert_eq!(
            node.block_header_height(5).await.expect("the header"),
            headers[5]
        );
        assert_eq!(
            node.block_hash(5).await.expect("the hash"),
            headers[5].block_hash()
        );
    }

    #[tokio::test]
    async fn the_unsealed_tail_is_sliced_out_locally() {
        let (headers, bytes) = header_file(10);
        // The tail is streamed whole with a plain 200, ignoring the range.
        let server = serve(move |_: &Request| Response::bytes(200, bytes.clone()));
        let node = node_for(&server.url);

        assert_eq!(
            node.batch_header_fetch(7, 2).await.expect("the headers"),
            headers[7..9]
        );
    }

    #[tokio::test]
    async fn a_range_past_the_tip_has_no_headers() {
        let server = serve(|_: &Request| Response::status(416));
        let node = node_for(&server.url);

        assert!(node
            .batch_header_fetch(100, 5)
            .await
            .expect("no headers")
            .is_empty());
        assert!(matches!(
            node.block_header_height(100).await,
            Err(FetchError::DataError(_))
        ));
    }

    #[tokio::test]
    async fn only_whole_headers_are_taken_from_a_file() {
        let (headers, bytes) = header_file(10);
        // A tail ending in the middle of a header, as a growing file can.
        let partial = bytes[..100].to_vec();
        let server = serve(move |_: &Request| Response::bytes(200, partial.clone()));
        let node = node_for(&server.url);

        assert_eq!(
            node.batch_header_fetch(0, 5).await.expect("the header"),
            [headers[0]]
        );
    }

    #[tokio::test]
    async fn a_range_spanning_two_files_is_fetched_from_both() {
        // The last 3 headers of one file and the first 3 of the next.
        let (headers, bytes) = header_file(6);
        let first_file = bytes[..240].to_vec();
        let second_file = bytes[240..].to_vec();
        let server = serve(move |request: &Request| match request.path.as_str() {
            "/headers/0" => Response::bytes(206, first_file.clone()),
            "/headers/100000" => Response::bytes(206, second_file.clone()),
            _ => Response::status(500),
        });
        let node = node_for(&server.url);

        assert_eq!(
            node.batch_header_fetch(99_997, 6)
                .await
                .expect("the headers"),
            headers
        );
        assert_eq!(server.paths(), ["/headers/0", "/headers/100000"]);
    }

    #[tokio::test]
    async fn a_short_slice_ends_the_fetch_before_the_next_file() {
        // A range over two files where the first answer is one byte short of
        // what was asked for, as a CDN serving a growing file can be.
        let (headers, bytes) = header_file(6);
        let short_file = bytes[..239].to_vec();
        let second_file = bytes[240..].to_vec();
        let server = serve(move |request: &Request| match request.path.as_str() {
            "/headers/0" => Response::bytes(206, short_file.clone()),
            "/headers/100000" => Response::bytes(206, second_file.clone()),
            _ => Response::status(500),
        });
        let node = node_for(&server.url);

        // Only the two whole headers of the short answer, and the second file
        // is left alone.
        assert_eq!(
            node.batch_header_fetch(99_997, 6)
                .await
                .expect("the headers"),
            headers[..2]
        );
        assert_eq!(server.paths(), ["/headers/0"]);
    }

    #[tokio::test]
    async fn a_block_is_served_as_its_bytes_and_its_coinbase_is_checked() {
        let block = test_chain::block_with_a_coinbase();
        let hash = block.block_hash();
        let raw = serialize(&block);
        let served = raw.clone();
        let server =
            serve(
                move |request: &Request| match request.path == format!("/block/{}", hash) {
                    true => Response::bytes(200, served.clone()),
                    false => Response::status(404),
                },
            );
        let node = node_for(&server.url);

        assert_eq!(node.block(&hash).await.expect("the block"), raw);
        assert_eq!(
            node.coinbase(&hash, 1).await.expect("the coinbase"),
            block.txdata[0]
        );

        // A block the node doesn't have.
        assert!(matches!(
            node.block(&BlockHash::all_zeros()).await,
            Err(FetchError::BlockDnREST(_))
        ));
    }

    #[tokio::test]
    async fn a_block_that_isnt_the_one_asked_for_is_refused() {
        let raw = serialize(&test_chain::block_with_a_coinbase());
        // Answers with that block whatever hash is asked for.
        let server = serve(move |_: &Request| Response::bytes(200, raw.clone()));
        let node = node_for(&server.url);

        assert!(matches!(
            node.coinbase(&BlockHash::all_zeros(), 1).await,
            Err(FetchError::DataError(_))
        ));
    }

    #[tokio::test]
    async fn a_block_whose_transactions_dont_match_its_merkle_root_is_refused() {
        let mut block = test_chain::block_with_a_coinbase();
        // A transaction the merkle root in the header doesn't account for.
        let another = block.txdata[0].clone();
        block.txdata.push(another);
        let hash = block.block_hash();
        let raw = serialize(&block);
        let server = serve(move |_: &Request| Response::bytes(200, raw.clone()));
        let node = node_for(&server.url);

        assert!(matches!(
            node.coinbase(&hash, 1).await,
            Err(FetchError::DataError(_))
        ));
    }
}
