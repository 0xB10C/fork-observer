//! Keeps nodes that serve another chain from filling the header tree with it.

use crate::backend::{Node, NodeInfo};
use corepc_client::bitcoin::BlockHash;
use log::{error, info, warn};
use std::time::{Duration, Instant};

/// Delay before a node that failed the check is checked again. Doubles with
/// every further failure.
const FIRST_RETRY_DELAY: Duration = Duration::from_secs(60);
/// Caps the retry delay (at `FIRST_RETRY_DELAY * 2^MAX_FAILURES`, about an
/// hour) and keeps the delay from overflowing.
const MAX_FAILURES: u32 = 6;

/// Checks a node's genesis block hash against the one the network expects,
/// before headers are fetched from it. A node that serves another chain shares
/// no block with the header tree, so nothing stops the backwards walk from
/// fetching that whole chain.
///
/// A node that fails is checked again after a delay that doubles with every
/// failure: the check itself is a single request, but on some backends it isn't
/// a cheap one.
pub struct GenesisCheck {
    expected: BlockHash,
    network_name: String,
    /// Consecutive failures, sets the retry delay.
    failures: u32,
    /// Earliest time of the next check. `None` means right away.
    retry_at: Option<Instant>,
}

impl GenesisCheck {
    pub fn new(expected: BlockHash, network_name: &str) -> Self {
        GenesisCheck {
            expected,
            network_name: network_name.to_string(),
            failures: 0,
            retry_at: None,
        }
    }

    /// Returns whether headers may be fetched from `node`. False while a failed
    /// node waits out its retry delay, and true when the genesis hash could not
    /// be fetched at all - an unreachable node is not one on another chain.
    pub async fn passes(&mut self, node: &dyn Node) -> bool {
        if let Some(retry_at) = self.retry_at {
            if Instant::now() < retry_at {
                return false;
            }
        }
        match node.block_hash(0).await {
            Ok(genesis) => self.record(genesis, &node.info()),
            Err(e) => {
                warn!(
                    "Could not check the genesis block hash of {} on network '{}': {} - fetching headers from it anyway",
                    node.info(), self.network_name, e
                );
                true
            }
        }
    }

    /// Records the genesis hash `node` served and returns whether it matched.
    fn record(&mut self, genesis: BlockHash, node: &NodeInfo) -> bool {
        if genesis == self.expected {
            if self.failures > 0 {
                info!(
                    "{} on network '{}' serves the expected genesis block {} again",
                    node, self.network_name, genesis
                );
            }
            self.failures = 0;
            self.retry_at = None;
            return true;
        }

        let delay = FIRST_RETRY_DELAY * 2u32.pow(self.failures);
        self.failures = (self.failures + 1).min(MAX_FAILURES);
        self.retry_at = Some(Instant::now() + delay);
        error!(
            "{} on network '{}' serves the chain with genesis block {}, not {}: not fetching headers from it, checking again in {}s",
            node, self.network_name, genesis, self.expected, delay.as_secs()
        );
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::FetchError;
    use corepc_client::bitcoin::hashes::Hash;

    fn node_info() -> NodeInfo {
        NodeInfo {
            id: 0,
            name: "node".to_string(),
            description: "a node".to_string(),
            implementation: "test".to_string(),
        }
    }

    fn check() -> GenesisCheck {
        GenesisCheck::new(BlockHash::all_zeros(), "testnetwork")
    }

    #[test]
    fn the_expected_genesis_hash_passes() {
        let mut check = check();
        assert!(check.record(BlockHash::all_zeros(), &node_info()));
        assert!(check.retry_at.is_none());
    }

    #[test]
    fn another_genesis_hash_fails_and_sets_a_retry_delay() {
        let mut check = check();
        assert!(!check.record(BlockHash::from_byte_array([1u8; 32]), &node_info()));
        assert!(check.retry_at.is_some());
    }

    #[test]
    fn the_retry_delay_doubles_and_is_capped() {
        let mut check = check();
        let other = BlockHash::from_byte_array([1u8; 32]);
        let mut delays: Vec<Duration> = Vec::new();
        for _ in 0..MAX_FAILURES + 3 {
            let before = Instant::now();
            check.record(other, &node_info());
            delays.push(check.retry_at.expect("a failure sets a retry time") - before);
        }

        assert!(delays[0] >= FIRST_RETRY_DELAY);
        assert!(delays[1] >= FIRST_RETRY_DELAY * 2);
        let capped = FIRST_RETRY_DELAY * 2u32.pow(MAX_FAILURES);
        for delay in delays.iter() {
            assert!(*delay <= capped + Duration::from_secs(1));
        }
        assert!(*delays.last().expect("delays are not empty") >= capped);
    }

    #[test]
    fn the_expected_genesis_hash_clears_the_retry_delay() {
        let mut check = check();
        check.record(BlockHash::from_byte_array([1u8; 32]), &node_info());
        assert!(check.record(BlockHash::all_zeros(), &node_info()));
        assert_eq!(check.failures, 0);
        assert!(check.retry_at.is_none());
    }

    /// A node serving `genesis`, or failing the request when it is `None`.
    struct FakeNode {
        genesis: Option<BlockHash>,
        /// How often the node was asked for its genesis block.
        asked: std::sync::Mutex<usize>,
    }

    impl FakeNode {
        fn serving(genesis: BlockHash) -> Self {
            FakeNode {
                genesis: Some(genesis),
                asked: std::sync::Mutex::new(0),
            }
        }

        fn unreachable() -> Self {
            FakeNode {
                genesis: None,
                asked: std::sync::Mutex::new(0),
            }
        }

        fn asked(&self) -> usize {
            *self.asked.lock().expect("the lock")
        }
    }

    #[async_trait::async_trait]
    impl Node for FakeNode {
        fn info(&self) -> NodeInfo {
            node_info()
        }

        fn capabilities(&self) -> crate::backend::Capabilities {
            unimplemented!("the genesis check only asks for a block hash")
        }

        fn rpc_url(&self) -> String {
            "fake".to_string()
        }

        async fn version(&self) -> Result<String, FetchError> {
            unimplemented!("the genesis check only asks for a block hash")
        }

        async fn block_header_hash(
            &self,
            _hash: &BlockHash,
        ) -> Result<corepc_client::bitcoin::blockdata::block::Header, FetchError> {
            unimplemented!("the genesis check only asks for a block hash")
        }

        async fn block_header_height(
            &self,
            _height: u64,
        ) -> Result<corepc_client::bitcoin::blockdata::block::Header, FetchError> {
            unimplemented!("the genesis check only asks for a block hash")
        }

        async fn block_hash(&self, height: u64) -> Result<BlockHash, FetchError> {
            assert_eq!(height, 0, "the genesis check asks for height 0");
            *self.asked.lock().expect("the lock") += 1;
            self.genesis
                .ok_or_else(|| FetchError::DataError("the node is not reachable".to_string()))
        }

        async fn tips(&self) -> Result<Vec<crate::types::ChainTip>, FetchError> {
            unimplemented!("the genesis check only asks for a block hash")
        }

        async fn coinbase(
            &self,
            _hash: &BlockHash,
            _height: u64,
        ) -> Result<corepc_client::bitcoin::Transaction, FetchError> {
            unimplemented!("the genesis check only asks for a block hash")
        }

        async fn block(&self, _hash: &BlockHash) -> Result<Vec<u8>, FetchError> {
            unimplemented!("the genesis check only asks for a block hash")
        }

        async fn batch_header_fetch(
            &self,
            _start_height: u64,
            _count: u64,
        ) -> Result<Vec<corepc_client::bitcoin::blockdata::block::Header>, FetchError> {
            unimplemented!("the genesis check only asks for a block hash")
        }
    }

    #[tokio::test]
    async fn a_node_on_the_expected_chain_passes() {
        let expected = BlockHash::all_zeros();
        let node = FakeNode::serving(expected);
        let mut check = GenesisCheck::new(expected, "Mainnet");

        assert!(check.passes(&node).await);
        assert_eq!(node.asked(), 1);
    }

    #[tokio::test]
    async fn a_node_on_another_chain_is_not_asked_again_right_away() {
        let node = FakeNode::serving(BlockHash::from_byte_array([1u8; 32]));
        let mut check = GenesisCheck::new(BlockHash::all_zeros(), "Mainnet");

        assert!(!check.passes(&node).await);
        assert_eq!(node.asked(), 1);

        // The retry delay hasn't passed, so the node isn't even asked.
        assert!(!check.passes(&node).await);
        assert_eq!(node.asked(), 1);
    }

    #[tokio::test]
    async fn a_node_is_asked_again_once_the_retry_delay_passed() {
        let node = FakeNode::serving(BlockHash::from_byte_array([1u8; 32]));
        let mut check = GenesisCheck::new(BlockHash::all_zeros(), "Mainnet");
        assert!(!check.passes(&node).await);

        // As if the delay had passed.
        check.retry_at = Some(Instant::now() - Duration::from_secs(1));

        assert!(!check.passes(&node).await);
        assert_eq!(node.asked(), 2);
    }

    #[tokio::test]
    async fn a_node_that_cant_be_asked_is_used_anyway() {
        let node = FakeNode::unreachable();
        let mut check = GenesisCheck::new(BlockHash::all_zeros(), "Mainnet");

        // An unreachable node is not one on another chain.
        assert!(check.passes(&node).await);
        assert_eq!(node.asked(), 1);
    }
}
