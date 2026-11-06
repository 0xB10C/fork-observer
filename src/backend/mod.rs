//! The data sources fork-observer fetches headers and chain tips from. Each
//! backend lives in its own submodule and implements the [`Node`] trait defined
//! here; the trait's default methods hold the fetching logic they all share.

mod bitcoin_core;
mod block_dn;
mod btcd;
mod electrum;
mod esplora;
mod mempool_space;
#[cfg(test)]
mod test_chain;
#[cfg(test)]
mod test_server;

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
use log::{debug, warn};
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
        tips: &[ChainTip],
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
        tips: &[ChainTip],
        tree: &Tree,
        min_fork_height: u64,
    ) -> Result<Vec<HeaderInfo>, FetchError> {
        let mut new_headers: Vec<HeaderInfo> = Vec::new();

        let active_tip = match tips
            .iter()
            .rfind(|tip| tip.status == ChainTipStatus::Active)
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
                    let header: Header = match self.capabilities().header_fetch_type {
                        HeaderFetchType::Hash => self.block_header_hash(&header_hash).await?,
                        HeaderFetchType::Height => {
                            self.block_header_height(query_height as u64).await?
                        }
                    };
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
        tips: &[ChainTip],
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
            // A branch can't be longer than the height of its tip, so a tip
            // claiming one isn't something we can walk down.
            .filter(|tip| tip.height.saturating_sub(tip.branchlen as u64) > min_fork_height)
            .filter(|tip| tip.status != ChainTipStatus::Active)
        {
            let mut next_header = match inactive_tip.block_hash() {
                Ok(hash) => hash,
                Err(e) => {
                    warn!(
                        "{} reported a tip at height {} whose hash '{}' isn't a block hash ({}) - skipping that branch",
                        self.info(),
                        inactive_tip.height,
                        inactive_tip.hash,
                        e
                    );
                    continue;
                }
            };
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

#[cfg(test)]
mod tests {
    use super::test_chain::{chain, header};
    use super::*;
    use crate::types::TreeInfo;
    use petgraph::graph::DiGraph;
    use std::collections::{BTreeSet, HashMap, HashSet};
    use std::sync::Arc;
    use tokio::sync::Mutex;

    // A chain of headers at the heights 0..=max_height.
    fn chain_of(max_height: u64) -> Vec<Header> {
        chain(max_height as u32 + 1)
    }

    // A tree that knows the given (height, header) pairs. Only the hash index
    // of the tree matters here, so the headers aren't connected.
    fn tree_with(headers: &[(u64, Header)]) -> Tree {
        let mut graph: DiGraph<HeaderInfo, bool> = DiGraph::new();
        let mut index = HashMap::new();
        for (height, header) in headers {
            let idx = graph.add_node(HeaderInfo {
                height: *height,
                header: *header,
                miner: String::new(),
            });
            index.insert(header.block_hash(), idx);
        }
        let tree_info: TreeInfo = (graph, index);
        Arc::new(Mutex::new(tree_info))
    }

    fn chain_tip(
        height: u64,
        hash: BlockHash,
        branchlen: usize,
        status: ChainTipStatus,
    ) -> ChainTip {
        ChainTip {
            height,
            hash: hash.to_string(),
            branchlen,
            status,
        }
    }

    /// How much a single test may ask of a FakeNode.
    const REQUEST_LIMIT: usize = 100;

    /// A node serving a chain from memory: `chain` is its active chain (the
    /// index being the height), `fork_headers` holds the headers of the
    /// branches added with `fork()`.
    struct FakeNode {
        chain: Vec<Header>,
        fork_headers: HashMap<BlockHash, Header>,
        tips: Vec<ChainTip>,
        batch_header_fetch: bool,
        fetch_by_hash: bool,
        partial_nonactive_headers: bool,
        /// Headers the node reports a tip for but can't serve.
        unavailable: HashSet<BlockHash>,
        /// What the node was asked for, in order.
        requests: std::sync::Mutex<Vec<String>>,
    }

    impl FakeNode {
        fn new(chain: Vec<Header>) -> Self {
            FakeNode {
                chain,
                fork_headers: HashMap::new(),
                tips: vec![],
                batch_header_fetch: true,
                fetch_by_hash: true,
                partial_nonactive_headers: false,
                unavailable: HashSet::new(),
                requests: std::sync::Mutex::new(vec![]),
            }
        }

        // Reports the highest header of the chain as the active tip.
        fn active_tip(mut self) -> Self {
            let height = self.chain.len() as u64 - 1;
            let hash = self.chain[height as usize].block_hash();
            self.tips
                .push(chain_tip(height, hash, 0, ChainTipStatus::Active));
            self
        }

        // Adds `len` headers building on the chain header at `fork_height` and
        // reports the highest of them as a tip. `nonce` keeps the branches of
        // one node apart.
        fn fork(mut self, fork_height: u64, len: u64, nonce: u32, status: ChainTipStatus) -> Self {
            let mut prev = self.chain[fork_height as usize].block_hash();
            for i in 0..len {
                let branch_header = header(prev, nonce + i as u32);
                prev = branch_header.block_hash();
                self.fork_headers.insert(prev, branch_header);
            }
            self.tips
                .push(chain_tip(fork_height + len, prev, len as usize, status));
            self
        }

        // Reports a tip as given, for the tips a node has no business
        // reporting.
        fn raw_tip(mut self, tip: ChainTip) -> Self {
            self.tips.push(tip);
            self
        }

        // The header of the branch tip added last.
        fn last_tip_header(&self) -> Header {
            let hash = self
                .tips
                .last()
                .expect("a tip")
                .block_hash()
                .expect("a block hash");
            self.fork_headers[&hash]
        }

        fn requests(&self) -> Vec<String> {
            self.requests.lock().expect("the lock").clone()
        }

        // Records a request, and refuses to serve more than REQUEST_LIMIT of
        // them: a walk that doesn't come to an end would otherwise hang.
        fn record(&self, request: String) -> Result<(), FetchError> {
            let mut requests = self.requests.lock().expect("the lock");
            requests.push(request);
            if requests.len() > REQUEST_LIMIT {
                return Err(FetchError::DataError(format!(
                    "asked for more than {} headers: the walk isn't getting anywhere",
                    REQUEST_LIMIT
                )));
            }
            Ok(())
        }

        fn header_by_hash(&self, hash: &BlockHash) -> Option<Header> {
            self.fork_headers.get(hash).copied().or_else(|| {
                self.chain
                    .iter()
                    .find(|header| header.block_hash() == *hash)
                    .copied()
            })
        }
    }

    #[async_trait]
    impl Node for FakeNode {
        fn info(&self) -> NodeInfo {
            NodeInfo {
                id: 0,
                name: "fake".to_string(),
                description: String::new(),
                implementation: "fake".to_string(),
            }
        }

        fn capabilities(&self) -> Capabilities {
            Capabilities {
                header_fetch_type: match self.fetch_by_hash {
                    true => HeaderFetchType::Hash,
                    false => HeaderFetchType::Height,
                },
                batch_header_fetch: self.batch_header_fetch,
                partial_nonactive_headers: self.partial_nonactive_headers,
            }
        }

        fn rpc_url(&self) -> String {
            "fake".to_string()
        }

        async fn version(&self) -> Result<String, FetchError> {
            Ok("fake".to_string())
        }

        async fn block_header_hash(&self, hash: &BlockHash) -> Result<Header, FetchError> {
            self.record(format!("header by hash {}", hash))?;
            if self.unavailable.contains(hash) {
                return Err(FetchError::DataError(format!("no header for {}", hash)));
            }
            self.header_by_hash(hash)
                .ok_or_else(|| FetchError::DataError(format!("unknown hash {}", hash)))
        }

        async fn block_header_height(&self, height: u64) -> Result<Header, FetchError> {
            self.record(format!("header at height {}", height))?;
            self.chain
                .get(height as usize)
                .copied()
                .ok_or_else(|| FetchError::DataError(format!("no header at height {}", height)))
        }

        async fn block_hash(&self, height: u64) -> Result<BlockHash, FetchError> {
            self.record(format!("hash at height {}", height))?;
            Ok(self
                .chain
                .get(height as usize)
                .ok_or_else(|| FetchError::DataError(format!("no block at height {}", height)))?
                .block_hash())
        }

        async fn tips(&self) -> Result<Vec<ChainTip>, FetchError> {
            Ok(self.tips.clone())
        }

        async fn coinbase(
            &self,
            _hash: &BlockHash,
            _height: u64,
        ) -> Result<Transaction, FetchError> {
            Err(FetchError::DataError(
                "these tests don't fetch coinbases".to_string(),
            ))
        }

        async fn block(&self, _hash: &BlockHash) -> Result<Vec<u8>, FetchError> {
            Err(FetchError::DataError(
                "these tests don't fetch blocks".to_string(),
            ))
        }

        async fn batch_header_fetch(
            &self,
            start_height: u64,
            count: u64,
        ) -> Result<Vec<Header>, FetchError> {
            self.record(format!("batch of {} from height {}", count, start_height))?;
            let start = start_height as usize;
            if start >= self.chain.len() {
                return Err(FetchError::DataError(format!(
                    "no header at height {}",
                    start_height
                )));
            }
            let end = (start + count as usize).min(self.chain.len());
            Ok(self.chain[start..end].to_vec())
        }
    }

    #[tokio::test]
    async fn waiting_for_a_tip_change_waits_out_the_timeout_by_default() {
        let node = FakeNode::new(chain_of(0));
        let timeout = Duration::from_millis(50);
        let start = tokio::time::Instant::now();

        node.wait_for_tip_change(timeout)
            .await
            .expect("waiting shouldn't fail");

        assert!(start.elapsed() >= timeout);
    }

    #[tokio::test]
    async fn batched_fetching_walks_down_in_steps_of_2000() {
        let node = FakeNode::new(chain_of(2500)).active_tip();
        let tree = tree_with(&[]);

        let headers = node
            .new_active_headers(&node.tips, &tree, 0)
            .await
            .expect("fetching headers");

        // Two batches: the 2000 headers up to the tip, then the rest.
        assert_eq!(
            node.requests(),
            [
                "batch of 2000 from height 501",
                "batch of 2000 from height 0"
            ]
        );
        let heights: BTreeSet<u64> = headers.iter().map(|h| h.height).collect();
        assert_eq!(heights.len(), 2501);
        assert_eq!(headers.first().expect("a header").height, 0);
        assert_eq!(headers.last().expect("a header").height, 2500);
    }

    #[tokio::test]
    async fn batched_fetching_stops_at_a_header_the_tree_knows() {
        let node = FakeNode::new(chain_of(9)).active_tip();
        let known: Vec<(u64, Header)> = (0..=7).map(|h| (h, node.chain[h as usize])).collect();
        let tree = tree_with(&known);

        let headers = node
            .new_active_headers(&node.tips, &tree, 0)
            .await
            .expect("fetching headers");

        let heights: Vec<u64> = headers.iter().map(|h| h.height).collect();
        assert_eq!(heights, [8, 9]);
        assert_eq!(node.requests(), ["batch of 2000 from height 0"]);
    }

    #[tokio::test]
    async fn fetching_one_by_one_starts_at_the_active_tip_and_stops_at_min_fork_height() {
        let mut node = FakeNode::new(chain_of(7))
            .active_tip()
            // A lower, non-active tip the walk must not start at.
            .fork(2, 1, 100, ChainTipStatus::ValidFork);
        node.batch_header_fetch = false;
        let tree = tree_with(&[]);

        let headers = node
            .new_active_headers(&node.tips, &tree, 5)
            .await
            .expect("fetching headers");

        let heights: Vec<u64> = headers.iter().map(|h| h.height).collect();
        assert_eq!(heights, [5, 6, 7]);
    }

    #[tokio::test]
    async fn a_node_without_hashes_is_asked_for_headers_by_height() {
        let mut node = FakeNode::new(chain_of(2)).active_tip();
        node.batch_header_fetch = false;
        node.fetch_by_hash = false;
        let tree = tree_with(&[]);

        let headers = node
            .new_active_headers(&node.tips, &tree, 0)
            .await
            .expect("fetching headers");

        assert_eq!(headers.len(), 3);
        assert!(node
            .requests()
            .iter()
            .any(|request| request == "header at height 2"));
        assert!(!node
            .requests()
            .iter()
            .any(|request| request.starts_with("header by hash")));
    }

    #[tokio::test]
    async fn without_an_active_tip_there_is_nothing_to_walk_down_from() {
        let node = FakeNode::new(chain_of(5)).fork(2, 1, 100, ChainTipStatus::ValidFork);
        let tree = tree_with(&[]);

        assert!(matches!(
            node.new_active_headers(&node.tips, &tree, 0).await,
            Err(FetchError::DataError(_))
        ));
    }

    #[tokio::test]
    async fn a_node_without_hashes_has_no_non_active_headers() {
        let mut node =
            FakeNode::new(chain_of(9))
                .active_tip()
                .fork(7, 2, 100, ChainTipStatus::ValidFork);
        node.fetch_by_hash = false;
        let tree = tree_with(&[]);

        assert!(node
            .new_nonactive_headers(&node.tips, &tree, 0)
            .await
            .expect("fetching headers")
            .is_empty());
    }

    #[tokio::test]
    async fn non_active_headers_walk_a_branch_down_to_the_block_it_forks_off() {
        let node =
            FakeNode::new(chain_of(9))
                .active_tip()
                .fork(7, 2, 100, ChainTipStatus::ValidFork);
        let branch_tip = node.last_tip_header();
        // The tree knows the active chain, so the walk stops at the fork point.
        let known: Vec<(u64, Header)> = (0..=9).map(|h| (h, node.chain[h as usize])).collect();
        let tree = tree_with(&known);

        let headers = node
            .new_nonactive_headers(&node.tips, &tree, 5)
            .await
            .expect("fetching headers");

        let heights: Vec<u64> = headers.iter().map(|h| h.height).collect();
        assert_eq!(heights, [9, 8]);
        assert_eq!(headers[0].header, branch_tip);
        assert_eq!(headers[1].header.block_hash(), branch_tip.prev_blockhash);
    }

    #[tokio::test]
    async fn a_branch_starting_at_min_fork_height_is_left_alone() {
        // The branch forks off the block at height 7, which is where
        // min_fork_height cuts the tree off.
        let node =
            FakeNode::new(chain_of(9))
                .active_tip()
                .fork(7, 2, 100, ChainTipStatus::ValidFork);
        let tree = tree_with(&[]);

        assert!(node
            .new_nonactive_headers(&node.tips, &tree, 7)
            .await
            .expect("fetching headers")
            .is_empty());
    }

    #[tokio::test]
    async fn a_header_a_node_cant_serve_only_skips_its_own_branch() {
        let mut node = FakeNode::new(chain_of(9))
            .active_tip()
            .fork(7, 2, 100, ChainTipStatus::ValidFork)
            .fork(5, 1, 200, ChainTipStatus::HeadersOnly);
        let unservable = node
            .tips
            .last()
            .expect("a tip")
            .block_hash()
            .expect("a block hash");
        node.unavailable.insert(unservable);
        let known: Vec<(u64, Header)> = (0..=9).map(|h| (h, node.chain[h as usize])).collect();
        let tree = tree_with(&known);

        // Without the capability, a header the node can't serve fails the
        // whole update.
        assert!(matches!(
            node.new_nonactive_headers(&node.tips, &tree, 0).await,
            Err(FetchError::DataError(_))
        ));

        // With it, the other branch is still fetched.
        node.partial_nonactive_headers = true;
        let headers = node
            .new_nonactive_headers(&node.tips, &tree, 0)
            .await
            .expect("fetching headers");
        let heights: Vec<u64> = headers.iter().map(|h| h.height).collect();
        assert_eq!(heights, [9, 8]);
    }

    #[tokio::test]
    async fn a_small_active_update_gets_miners_for_all_of_its_headers() {
        let node =
            FakeNode::new(chain_of(5))
                .active_tip()
                .fork(3, 2, 100, ChainTipStatus::ValidFork);
        let tree = tree_with(&[(3, node.chain[3])]);

        let (headers, needing_miners) = node
            .new_headers(&node.tips, &tree, 0)
            .await
            .expect("fetching headers");

        // 5 new active headers (the tree knows the one at height 3) and the 2
        // of the branch.
        assert_eq!(headers.len(), 7);
        assert_eq!(needing_miners.len(), 7);
    }

    #[tokio::test]
    async fn a_large_active_update_only_gets_miners_for_the_non_active_headers() {
        let node =
            FakeNode::new(chain_of(24))
                .active_tip()
                .fork(3, 2, 100, ChainTipStatus::ValidFork);
        let tree = tree_with(&[(3, node.chain[3])]);

        let (headers, needing_miners) = node
            .new_headers(&node.tips, &tree, 0)
            .await
            .expect("fetching headers");

        // 24 new active headers is more than the 20 we look up miners for, so
        // only the 2 branch headers are left.
        assert_eq!(headers.len(), 26);
        assert_eq!(needing_miners.len(), 2);
    }

    #[test]
    fn a_node_is_displayed_with_its_id_name_and_implementation() {
        let info = NodeInfo {
            id: 3,
            name: "block-dn.org".to_string(),
            description: "a description".to_string(),
            implementation: "block-dn".to_string(),
        };

        assert_eq!(
            info.to_string(),
            "Node(id=3, name='block-dn.org', implementation='block-dn')"
        );
    }

    #[tokio::test]
    async fn a_tip_whose_hash_isnt_a_block_hash_is_skipped() {
        let node = FakeNode::new(chain_of(9))
            .active_tip()
            .fork(7, 2, 100, ChainTipStatus::ValidFork)
            .raw_tip(ChainTip {
                height: 9,
                hash: "not a block hash".to_string(),
                branchlen: 2,
                status: ChainTipStatus::ValidFork,
            });
        let known: Vec<(u64, Header)> = (0..=9).map(|h| (h, node.chain[h as usize])).collect();
        let tree = tree_with(&known);

        // The branch that is usable is still fetched.
        let headers = node
            .new_nonactive_headers(&node.tips, &tree, 5)
            .await
            .expect("fetching headers");

        let heights: Vec<u64> = headers.iter().map(|h| h.height).collect();
        assert_eq!(heights, [9, 8]);
    }

    #[tokio::test]
    async fn a_tip_with_a_branch_longer_than_its_height_is_skipped() {
        // No branch can be longer than the height of its tip.
        let chain = chain_of(9);
        let node = FakeNode::new(chain.clone()).active_tip().raw_tip(ChainTip {
            height: 5,
            hash: chain[5].block_hash().to_string(),
            branchlen: 10,
            status: ChainTipStatus::ValidFork,
        });
        let tree = tree_with(&[]);

        assert!(node
            .new_nonactive_headers(&node.tips, &tree, 0)
            .await
            .expect("fetching headers")
            .is_empty());
    }
}
