use super::{Capabilities, HeaderFetchType, Node, NodeInfo};
use crate::error::FetchError;
use crate::types::ChainTip;
use async_trait::async_trait;
use corepc_client::bitcoin::blockdata::block::Header;
use corepc_client::bitcoin::{BlockHash, Transaction};
use corepc_client::client_sync::v31::Client;
use corepc_client::client_sync::Auth;
use log::{debug, error};
use std::time::Duration;
use tokio::task;

#[derive(Hash, Clone)]
pub struct BitcoinCoreNode {
    info: NodeInfo,
    rpc_url: String,
    rpc_auth: Auth,
    use_rest: bool,
    use_waitfornewblock: bool,
}

impl BitcoinCoreNode {
    pub fn new(
        info: NodeInfo,
        rpc_url: String,
        rpc_auth: Auth,
        use_rest: bool,
        use_waitfornewblock: bool,
    ) -> Self {
        BitcoinCoreNode {
            info,
            rpc_url,
            rpc_auth,
            use_rest,
            use_waitfornewblock,
        }
    }

    fn rpc_client(&self) -> Result<Client, FetchError> {
        match Client::new_with_auth(&self.rpc_url, self.rpc_auth.clone()) {
            Ok(c) => Ok(c),
            Err(e) => {
                error!(
                    "Could not create a RPC client for node {}: {:?}",
                    self.info(),
                    e
                );
                Err(FetchError::from(e))
            }
        }
    }
}

#[async_trait]
impl Node for BitcoinCoreNode {
    fn info(&self) -> NodeInfo {
        self.info.clone()
    }

    fn capabilities(&self) -> Capabilities {
        Capabilities {
            header_fetch_type: HeaderFetchType::Hash,
            batch_header_fetch: self.use_rest,
            partial_nonactive_headers: false,
        }
    }

    fn rpc_url(&self) -> String {
        self.rpc_url.clone()
    }

    async fn version(&self) -> Result<String, FetchError> {
        let rpc = self.rpc_client()?;
        task::spawn_blocking(move || -> Result<String, FetchError> {
            Ok(rpc
                .get_network_info()?
                .into_model()
                .map_err(|e| FetchError::DataError(e.to_string()))?
                .subversion)
        })
        .await?
    }

    async fn block_hash(&self, height: u64) -> Result<BlockHash, FetchError> {
        let rpc = self.rpc_client()?;
        task::spawn_blocking(move || -> Result<BlockHash, FetchError> {
            Ok(rpc
                .get_block_hash(height)?
                .into_model()
                .map_err(|e| FetchError::DataError(e.to_string()))?
                .0)
        })
        .await?
    }

    async fn block_header_hash(&self, hash: &BlockHash) -> Result<Header, FetchError> {
        let rpc = self.rpc_client()?;
        let hash_clone = *hash;
        task::spawn_blocking(move || -> Result<Header, FetchError> {
            Ok(rpc
                .get_block_header(&hash_clone)?
                .into_model()
                .map_err(|e| FetchError::DataError(e.to_string()))?
                .0)
        })
        .await?
    }

    async fn block_header_height(&self, _: u64) -> Result<Header, FetchError> {
        assert_eq!(self.capabilities().header_fetch_type, HeaderFetchType::Hash);
        Err(FetchError::DataError(
            "fetch by block height not implemented".to_string(),
        ))
    }

    async fn coinbase(&self, hash: &BlockHash, _height: u64) -> Result<Transaction, FetchError> {
        let rpc = self.rpc_client()?;
        let hash_clone = *hash;
        match task::spawn_blocking(move || rpc.get_block(hash_clone)).await {
            Ok(result) => match result {
                Ok(result) => Ok(result
                    .txdata
                    .first()
                    .expect("Block should have a coinbase transaction")
                    .clone()),
                Err(e) => Err(e.into()),
            },
            Err(e) => Err(e.into()),
        }
    }

    async fn block(&self, hash: &BlockHash) -> Result<Vec<u8>, FetchError> {
        // Prefer the REST interface (if enabled) as it returns the raw block
        // bytes directly and doesn't need an extra (de)serialization roundtrip.
        if self.use_rest {
            let url = format!("{}/rest/block/{}.bin", self.rpc_url(), hash);
            let res = minreq::get(url.clone()).with_timeout(8).send()?;
            if res.status_code != 200 {
                return Err(FetchError::BitcoinCoreREST(format!(
                    "could not load block from REST URL ({}): {} {}: {:?}",
                    url,
                    res.status_code,
                    res.reason_phrase,
                    res.as_str(),
                )));
            }
            return Ok(res.as_bytes().to_vec());
        }

        let rpc = self.rpc_client()?;
        let hash_clone = *hash;
        match task::spawn_blocking(move || rpc.get_block(hash_clone)).await {
            Ok(Ok(block)) => Ok(corepc_client::bitcoin::consensus::encode::serialize(&block)),
            Ok(Err(e)) => Err(e.into()),
            Err(e) => Err(e.into()),
        }
    }

    async fn tips(&self) -> Result<Vec<ChainTip>, FetchError> {
        let rpc = self.rpc_client()?;
        task::spawn_blocking(move || -> Result<Vec<ChainTip>, FetchError> {
            Ok(rpc
                .get_chain_tips()?
                .into_model()
                .map_err(|e| FetchError::DataError(e.to_string()))?
                .0
                .into_iter()
                .map(|t| t.into())
                .collect())
        })
        .await?
    }

    async fn wait_for_tip_change(&self, timeout: Duration) -> Result<(), FetchError> {
        if !self.use_waitfornewblock {
            tokio::time::sleep(timeout).await;
            return Ok(());
        }

        // The `corepc_client` `Client` has a hardcoded 60s HTTP transport
        // timeout. We cap the server-side `waitfornewblock` timeout safely below
        // that: on timeout Bitcoin Core returns the current tip (no error), the
        // caller sees no tip change and we simply re-issue on the next loop.
        const MAX_WAIT: Duration = Duration::from_secs(50);
        let timeout_ms = timeout.min(MAX_WAIT).as_millis() as u64;

        let rpc = self.rpc_client()?;
        task::spawn_blocking(move || -> Result<(), FetchError> {
            // `corepc_client`'s `wait_for_new_block()` helper sends no timeout
            // (i.e. blocks indefinitely), so we issue the raw call with an
            // explicit timeout in milliseconds instead. We only use the response
            // as a wake-up signal and re-fetch the tips afterwards.
            rpc.call::<serde_json::Value>("waitfornewblock", &[timeout_ms.into()])?;
            Ok(())
        })
        .await?
    }

    async fn batch_header_fetch(
        &self,
        start_height: u64,
        count: u64,
    ) -> Result<Vec<Header>, FetchError> {
        // The REST headers endpoint is addressed by hash; resolve the start
        // height with a (cheap) `getblockhash` RPC first.
        let start_hash = self.block_hash(start_height).await?;
        debug!(
            "loading active-chain headers starting from {} ({})",
            start_height, start_hash
        );

        let url = format!(
            "{}/rest/headers/{}/{}.bin",
            self.rpc_url(),
            count,
            start_hash
        );
        let res = minreq::get(url.clone()).with_timeout(8).send()?;

        if res.status_code != 200 {
            return Err(FetchError::BitcoinCoreREST(format!(
                "could not load headers from REST URL ({}): {} {}: {:?}",
                url,
                res.status_code,
                res.reason_phrase,
                res.as_str(),
            )));
        }

        let header_results: Result<Vec<Header>, corepc_client::bitcoin::consensus::encode::Error> =
            res.as_bytes()
                .chunks(80)
                .map(corepc_client::bitcoin::consensus::deserialize::<Header>)
                .collect();

        let headers = match header_results {
            Ok(headers) => headers,
            Err(e) => {
                return Err(FetchError::BitcoinCoreREST(format!(
                    "could not deserialize REST header response: {}",
                    e
                )))
            }
        };

        debug!(
            "loaded {} active-chain headers starting from {} ({})",
            headers.len(),
            start_height,
            start_hash
        );

        Ok(headers)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::ChainTipStatus;
    use corepc_node::Node as CoreNode;

    /// Launch a fresh regtest bitcoind for a test.
    fn start_bitcoind() -> CoreNode {
        let exe = corepc_node::exe_path()
            .expect("a bitcoind binary via BITCOIND_EXE or PATH (see shell.nix)");
        CoreNode::new(exe).expect("failed to launch bitcoind")
    }

    /// Build the `BitcoinCoreNode` under test, pointed at the given bitcoind
    /// using cookie-file authentication (mirroring the production config path).
    fn node_under_test(core: &CoreNode) -> BitcoinCoreNode {
        let info = NodeInfo {
            id: 0,
            name: "test-core".to_string(),
            description: "regtest node under test".to_string(),
            implementation: "Bitcoin Core".to_string(),
        };
        BitcoinCoreNode::new(
            info,
            core.rpc_url(),
            Auth::CookieFile(core.params.cookie_file.clone()),
            false, // use_rest
            true,  // use_waitfornewblock
        )
    }

    #[tokio::test]
    async fn version_returns_subversion() {
        let core = start_bitcoind();
        let node = node_under_test(&core);

        let version = node.version().await.expect("version RPC failed");
        assert!(
            version.contains("Satoshi"),
            "unexpected subversion string: {}",
            version
        );
    }

    #[tokio::test]
    async fn chain_data_matches_generated_blocks() {
        let core = start_bitcoind();
        let address = core.client.new_address().expect("new_address failed");
        core.client
            .generate_to_address(5, &address)
            .expect("generate_to_address failed");

        let node = node_under_test(&core);

        // getchaintips: exactly one active tip at the height we generated to.
        let tips = node.tips().await.expect("tips RPC failed");
        let active: Vec<&ChainTip> = tips
            .iter()
            .filter(|t| t.status == ChainTipStatus::Active)
            .collect();
        assert_eq!(active.len(), 1, "expected exactly one active chain tip");
        assert_eq!(active[0].height, 5, "active tip at unexpected height");

        // getblockhash + getblockheader: the header hashes back to the same hash.
        let hash = node.block_hash(3).await.expect("block_hash RPC failed");
        let header = node
            .block_header_hash(&hash)
            .await
            .expect("block_header RPC failed");
        assert_eq!(
            header.block_hash(),
            hash,
            "header does not hash to its hash"
        );

        // getblock: the first transaction is the coinbase.
        let coinbase = node.coinbase(&hash, 3).await.expect("coinbase RPC failed");
        assert!(
            coinbase.is_coinbase(),
            "first transaction in block should be the coinbase"
        );
    }

    #[tokio::test]
    async fn reorg_is_reflected_in_chaintips() {
        let core = start_bitcoind();
        let address = core.client.new_address().expect("new_address failed");
        core.client
            .generate_to_address(5, &address)
            .expect("generate_to_address failed");

        let node = node_under_test(&core);

        // Before the reorg there is a single active tip at height 5.
        let tips_before = node.tips().await.expect("tips RPC failed");
        let active_before: Vec<&ChainTip> = tips_before
            .iter()
            .filter(|t| t.status == ChainTipStatus::Active)
            .collect();
        assert_eq!(active_before.len(), 1, "expected exactly one active tip");
        assert_eq!(active_before[0].height, 5);
        let original_tip_hash = active_before[0].hash.clone();

        // Invalidate the block at height 4. This rolls the active chain back to
        // height 3 and turns the old blocks 4..5 into an invalid branch.
        let invalidate_at = node.block_hash(4).await.expect("block_hash RPC failed");
        core.client
            .invalidate_block(invalidate_at)
            .expect("invalidateblock failed");

        // Mine a new, longer branch from height 3 so the active chain switches
        // to it and overtakes the invalidated branch (a short reorg). A fresh
        // address makes the coinbase (and thus the blocks) differ from the
        // invalidated ones, which would otherwise be reproduced identically and
        // rejected as already-known-invalid blocks.
        let new_branch_address = core.client.new_address().expect("new_address failed");
        core.client
            .generate_to_address(4, &new_branch_address)
            .expect("generate_to_address failed");

        let tips_after = node.tips().await.expect("tips RPC failed");

        // The active tip now sits on the new, longer branch (height 7).
        let active_after: Vec<&ChainTip> = tips_after
            .iter()
            .filter(|t| t.status == ChainTipStatus::Active)
            .collect();
        assert_eq!(active_after.len(), 1, "expected exactly one active tip");
        assert_eq!(
            active_after[0].height, 7,
            "active tip should be on the new, longer branch"
        );
        assert_ne!(
            active_after[0].hash, original_tip_hash,
            "active tip should have changed after the reorg"
        );

        // The old branch is now reported as an invalid chain tip.
        let invalid_tip = tips_after
            .iter()
            .find(|t| t.status == ChainTipStatus::Invalid)
            .expect("expected an invalid chain tip after invalidateblock");
        assert_eq!(
            invalid_tip.hash, original_tip_hash,
            "invalid tip should be the previously active tip"
        );
        assert_eq!(
            invalid_tip.height, 5,
            "invalid tip should keep the old branch height"
        );
        assert_eq!(
            invalid_tip.branchlen, 2,
            "invalid branch (blocks 4 and 5) should have length 2"
        );
    }

    #[tokio::test]
    async fn wait_for_tip_change_wakes_on_new_block() {
        let core = start_bitcoind();
        let address = core.client.new_address().expect("new_address failed");
        // Mine one block so the node has a non-genesis tip to wait past.
        core.client
            .generate_to_address(1, &address)
            .expect("generate_to_address failed");

        let node = node_under_test(&core);

        // Wait for a tip change with a generous timeout while concurrently mining
        // a new block. `waitfornewblock` should return well before the timeout.
        let timeout = Duration::from_secs(30);
        let start = std::time::Instant::now();
        let (wait_result, _) = tokio::join!(node.wait_for_tip_change(timeout), async {
            // Give `wait_for_tip_change` a moment to issue waitfornewblock first.
            tokio::time::sleep(Duration::from_millis(500)).await;
            core.client
                .generate_to_address(1, &address)
                .expect("generate_to_address failed");
        });

        wait_result.expect("wait_for_tip_change should not error");
        assert!(
            start.elapsed() < timeout,
            "wait_for_tip_change should return promptly after a new block, not hit the timeout"
        );
    }

    #[tokio::test]
    async fn wait_for_tip_change_returns_on_timeout() {
        let core = start_bitcoind();
        let node = node_under_test(&core);

        // With no new block arriving, `waitfornewblock` returns the current tip
        // once the timeout elapses - without erroring.
        let timeout = Duration::from_secs(1);
        let start = std::time::Instant::now();
        node.wait_for_tip_change(timeout)
            .await
            .expect("wait_for_tip_change should return Ok on timeout");
        let elapsed = start.elapsed();
        assert!(
            elapsed >= timeout,
            "should wait out the full timeout, waited {:?}",
            elapsed
        );
        assert!(
            elapsed < Duration::from_secs(10),
            "should return shortly after the timeout, waited {:?}",
            elapsed
        );
    }

    #[tokio::test]
    async fn wait_for_tip_change_polling_fallback_when_disabled() {
        // With `use_waitfornewblock = false` the node must not contact the RPC at
        // all; it simply waits out the timeout. We therefore point it at an unused
        // address and still expect a clean, timely return (no bitcoind needed).
        let info = NodeInfo {
            id: 0,
            name: "disabled".to_string(),
            description: String::new(),
            implementation: "Bitcoin Core".to_string(),
        };
        let node = BitcoinCoreNode::new(
            info,
            "127.0.0.1:1".to_string(),
            Auth::UserPass("user".to_string(), "pass".to_string()),
            false, // use_rest
            false, // use_waitfornewblock
        );

        let timeout = Duration::from_millis(200);
        let start = std::time::Instant::now();
        node.wait_for_tip_change(timeout)
            .await
            .expect("polling fallback should return Ok");
        assert!(
            start.elapsed() >= timeout,
            "polling fallback should wait out the full timeout"
        );
    }
}
