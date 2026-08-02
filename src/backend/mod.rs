//! The data sources fork-observer fetches headers and chain tips from. Each
//! backend lives in its own submodule and implements the [`Node`] trait defined
//! here; the trait's default methods hold the fetching logic they all share.

mod bitcoin_core;
mod block_dn;
mod btcd;
mod electrum;
mod esplora;
mod mempool_space;

pub use bitcoin_core::BitcoinCoreNode;
pub use block_dn::BlockDn;
pub use btcd::BtcdNode;
pub use electrum::Electrum;
pub use esplora::Esplora;
pub use mempool_space::MempoolSpace;

use crate::error::FetchError;
use crate::types::{ChainTip, ChainTipStatus, HeaderInfo, Tree};
use async_trait::async_trait;
use corepc_client::bitcoin::blockdata::block::Header;
use corepc_client::bitcoin::{BlockHash, Transaction};
use log::debug;
use std::cmp::max;
use std::fmt;
use std::time::Duration;

const DEFAULT_EMPTY_MINER: &str = "";

/// Some data sources only support fetching headers by height, and some only by hash.
/// We set this for every implementation and choose accordingly when fetching.
#[derive(Debug, PartialEq)]
pub enum HeaderFetchType {
    Height,
    Hash,
}

pub struct Capabilities {
    /// The value set here indicates if we can use the BlockHash to to fetch headers or can only
    /// fetch via the block height. The latter one can only be used for headers in the
    /// active chain.
    header_fetch_type: HeaderFetchType,
    batch_header_fetch: bool,
    /// Set for data sources that report non-active tips they can't serve headers
    /// for (mempool.space, for example, 404s on `headers-only` tips). A failing
    /// header fetch then skips the rest of that branch instead of failing the
    /// whole update.
    partial_nonactive_headers: bool,
}

#[async_trait]
pub trait Node: Sync {
    fn info(&self) -> NodeInfo;
    /// Returns information about the capabilities the data source has.
    fn capabilities(&self) -> Capabilities;
    fn rpc_url(&self) -> String;
    async fn version(&self) -> Result<String, FetchError>;
    async fn block_header_hash(&self, hash: &BlockHash) -> Result<Header, FetchError>;
    async fn block_header_height(&self, height: u64) -> Result<Header, FetchError>;
    async fn block_hash(&self, height: u64) -> Result<BlockHash, FetchError>;
    async fn tips(&self) -> Result<Vec<ChainTip>, FetchError>;
    async fn coinbase(&self, hash: &BlockHash, height: u64) -> Result<Transaction, FetchError>;
    /// Fetches a full block by its hash, returning the consensus-serialized
    /// (raw) block bytes. Used to serve full stale blocks on demand.
    async fn block(&self, hash: &BlockHash) -> Result<Vec<u8>, FetchError>;
    /// Fetches a batch of successive headers from the active chain, starting
    /// at `start_height`. Implementations that address headers by hash (e.g.
    /// Bitcoin Core's REST interface) resolve the start hash themselves.
    async fn batch_header_fetch(
        &self,
        start_height: u64,
        count: u64,
    ) -> Result<Vec<Header>, FetchError>;

    /// Blocks until the node's tip likely changed, or until `timeout` elapses,
    /// whichever comes first. Returning is only a hint to re-fetch tips; callers
    /// must still call `tips()` and compare. The default implementation simply
    /// waits out the `timeout` (i.e. preserves the fixed-interval polling
    /// behaviour). Implementations that support a push/long-poll mechanism (e.g.
    /// Bitcoin Core's `waitfornewblock`) can override this to return as soon as a
    /// new block arrives.
    async fn wait_for_tip_change(&self, timeout: Duration) -> Result<(), FetchError> {
        tokio::time::sleep(timeout).await;
        Ok(())
    }

    async fn new_headers(
        &self,
        tips: &Vec<ChainTip>,
        tree: &Tree,
        min_fork_height: u64,
    ) -> Result<(Vec<HeaderInfo>, Vec<BlockHash>), FetchError> {
        let mut new_headers: Vec<HeaderInfo> = Vec::new();
        let mut headers_needing_miners: Vec<BlockHash> = Vec::new();

        let mut active_new_headers: Vec<HeaderInfo> =
            self.new_active_headers(tips, tree, min_fork_height).await?;
        // We only want miners for active headers if they are (smaller) tip updates.
        if active_new_headers.len() <= 20 {
            for h in active_new_headers.iter() {
                headers_needing_miners.push(h.header.block_hash());
            }
        }
        new_headers.append(&mut active_new_headers);

        let mut nonactive_new_headers: Vec<HeaderInfo> = self
            .new_nonactive_headers(tips, tree, min_fork_height)
            .await?;
        // We want miners for all headers in a non-active chain.
        for h in nonactive_new_headers.iter() {
            headers_needing_miners.push(h.header.block_hash());
        }
        new_headers.append(&mut nonactive_new_headers);
        Ok((new_headers, headers_needing_miners))
    }

    async fn new_active_headers(
        &self,
        tips: &Vec<ChainTip>,
        tree: &Tree,
        min_fork_height: u64,
    ) -> Result<Vec<HeaderInfo>, FetchError> {
        let mut new_headers: Vec<HeaderInfo> = Vec::new();

        let active_tip = match tips
            .iter()
            .filter(|tip| tip.status == ChainTipStatus::Active)
            .last()
        {
            Some(active_tip) => active_tip,
            None => {
                return Err(FetchError::DataError(String::from(
                    "No 'active' chain tip returned",
                )))
            }
        };
        const STEP_SIZE: i64 = 2000;
        let mut query_height: i64 = active_tip.height as i64;
        loop {
            match self.capabilities().batch_header_fetch {
                true => {
                    // We want to either start to query blocks at the `min_fork_height` or
                    // the `tip height - STEP_SIZE + 1` which ever is larger.
                    // (+ 1 as we would otherwise not query the tip)
                    let start_height = max(min_fork_height as i64, query_height - STEP_SIZE + 1);
                    let mut already_knew_a_header = false;

                    // get STEP_SIZE headers
                    let headers = self
                        .batch_header_fetch(start_height as u64, STEP_SIZE as u64)
                        .await?;

                    // zip heights and headers up and to iterate through them by descending height
                    // newest first
                    for height_header_pair in headers
                        .iter()
                        .zip(start_height..start_height + headers.len() as i64)
                    {
                        let locked_tree = tree.lock().await;
                        if !locked_tree
                            .1
                            .contains_key(&height_header_pair.0.block_hash())
                        {
                            new_headers.push(HeaderInfo {
                                header: *height_header_pair.0,
                                height: height_header_pair.1 as u64,
                                miner: DEFAULT_EMPTY_MINER.to_string(),
                            });
                        } else {
                            already_knew_a_header = true;
                        }
                    }

                    if already_knew_a_header {
                        break;
                    }

                    query_height -= STEP_SIZE;
                }
                false => {
                    // using RPC, not using REST
                    let header_hash = self.block_hash(query_height as u64).await?;
                    {
                        let locked_tree = tree.lock().await;
                        if locked_tree.1.contains_key(&header_hash) {
                            break;
                        }
                    }
                    // since we are fetching "active" (i.e. in the main chain) headers,
                    // we can fetch by block height here too.
                    let header: Header;
                    match self.capabilities().header_fetch_type {
                        HeaderFetchType::Hash => {
                            header = self.block_header_hash(&header_hash).await?;
                        }
                        HeaderFetchType::Height => {
                            header = self.block_header_height(query_height as u64).await?;
                        }
                    }
                    new_headers.push(HeaderInfo {
                        height: query_height as u64,
                        header,
                        miner: DEFAULT_EMPTY_MINER.to_string(),
                    });
                    query_height -= 1;
                }
            }

            if query_height < min_fork_height as i64 {
                break;
            }
        }
        new_headers.sort_by_key(|h| h.height);
        Ok(new_headers)
    }

    async fn new_nonactive_headers(
        &self,
        tips: &Vec<ChainTip>,
        tree: &Tree,
        min_fork_height: u64,
    ) -> Result<Vec<HeaderInfo>, FetchError> {
        let mut new_headers: Vec<HeaderInfo> = Vec::new();

        // Since some implementations can't fetch headers by hash (e.g. Electrum),
        // we can return early from them here. We can only fetch non-active headers
        // by hash.
        if self.capabilities().header_fetch_type == HeaderFetchType::Height {
            return Ok(new_headers);
        }

        for inactive_tip in tips
            .iter()
            .filter(|tip| tip.height - tip.branchlen as u64 > min_fork_height)
            .filter(|tip| tip.status != ChainTipStatus::Active)
        {
            let mut next_header = inactive_tip.block_hash();
            for i in 0..=inactive_tip.branchlen {
                {
                    let tree_locked = tree.lock().await;
                    if tree_locked.1.contains_key(&next_header) {
                        break;
                    }
                }

                let height = inactive_tip.height - i as u64;
                debug!(
                    "loading non-active-chain header: hash={}, height={}",
                    next_header, height
                );

                let header = match self.block_header_hash(&next_header).await {
                    Ok(header) => header,
                    Err(e) if self.capabilities().partial_nonactive_headers => {
                        debug!(
                            "could not fetch non-active-chain header hash={} height={} of tip {}: {} - skipping the rest of this branch",
                            next_header, height, inactive_tip.hash, e
                        );
                        break;
                    }
                    Err(e) => return Err(e),
                };
                new_headers.push(HeaderInfo {
                    height,
                    header,
                    miner: DEFAULT_EMPTY_MINER.to_string(),
                });
                next_header = header.prev_blockhash;
            }
        }
        Ok(new_headers)
    }
}

#[derive(Hash, Clone)]
pub struct NodeInfo {
    pub id: u32,
    pub name: String,
    pub description: String,
    pub implementation: String,
}

impl fmt::Display for NodeInfo {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(
            f,
            "Node(id={}, name='{}', implementation='{}')",
            self.id, self.name, self.implementation
        )
    }
}
