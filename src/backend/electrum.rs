use super::{Capabilities, HeaderFetchType, Node, NodeInfo};
use crate::error::FetchError;
use crate::types::{ChainTip, ChainTipStatus};
use async_trait::async_trait;
use corepc_client::bitcoin::blockdata::block::Header;
use corepc_client::bitcoin::{BlockHash, Transaction};
use electrum_client::{
    Client as ElectrumClient, ConfigBuilder as ElectrumClientConfigBuilder, ElectrumApi,
};
use std::sync::OnceLock;
use std::time::Duration;

pub struct Electrum {
    info: NodeInfo,
    url: String,
    client: OnceLock<ElectrumClient>,
}

impl Electrum {
    pub fn new(info: NodeInfo, url: String) -> Self {
        Electrum {
            info,
            url,
            client: OnceLock::new(),
        }
    }

    /// The connection to the server, opened on first use. Failing to connect
    /// is returned to the caller rather than retried here: the node task tries
    /// again at the next poll, the same way it does for every other backend.
    fn get_client(&self) -> Result<&ElectrumClient, FetchError> {
        if let Some(client) = self.client.get() {
            return Ok(client);
        }

        let config = ElectrumClientConfigBuilder::new()
            .timeout(Some(Duration::from_secs(10)))
            .retry(2)
            .validate_domain(false)
            .build();
        let client = ElectrumClient::from_config(&self.url, config)?;
        log::info!(
            "Connected to Electrum server {} ({})",
            self.info.name,
            self.url
        );
        // A call racing with this one may have set the client first, which
        // only means dropping the one opened here.
        let _ = self.client.set(client);
        Ok(self.client.get().expect("the client was just set"))
    }
}

#[async_trait]
impl Node for Electrum {
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
        "not used".to_string()
    }

    async fn version(&self) -> Result<String, FetchError> {
        let client = self.get_client()?;
        let response = client.server_features()?;
        Ok(response.server_version)
    }

    async fn block_header_hash(&self, _hash: &BlockHash) -> Result<Header, FetchError> {
        // hm, no lookup via BlockHash possible I think?
        return Err(FetchError::DataError(
            "block_header not implemented".to_string(),
        ));
    }

    async fn block_header_height(&self, height: u64) -> Result<Header, FetchError> {
        let client = self.get_client()?;
        let header = client.block_header(height as usize)?;
        Ok(header)
    }

    async fn block_hash(&self, height: u64) -> Result<BlockHash, FetchError> {
        let client = self.get_client()?;
        let header = client.block_header(height as usize)?;
        Ok(header.block_hash())
    }

    async fn tips(&self) -> Result<Vec<ChainTip>, FetchError> {
        let client = self.get_client()?;

        // Check if we got a header notification since we checked last time.
        let mut last_header_notification = None;
        loop {
            match client.block_headers_pop() {
                Ok(option) => match option {
                    Some(notification) => last_header_notification = Some(notification),
                    None => break,
                },
                Err(e) => {
                    log::debug!("could not pop block header notification: {}", e);
                    break;
                }
            }
        }
        if let Some(notification) = last_header_notification {
            return Ok(vec![ChainTip {
                height: notification.height as u64,
                hash: notification.header.block_hash().to_string(),
                branchlen: 0,
                status: ChainTipStatus::Active,
            }]);
        }

        // We don't keep state here about the last block. To return the chain tip
        // we can subscribe again as this will return the tip. This works,
        // but it would probably nicer if we'd keep the last header around to avoid
        // the roundtrip here.
        match client.block_headers_subscribe() {
            Ok(response) => Ok(vec![ChainTip {
                height: response.height as u64,
                hash: response.header.block_hash().to_string(),
                branchlen: 0,
                status: ChainTipStatus::Active,
            }]),
            Err(e) => {
                log::warn!("block headers subscribe error, {:?}", e);
                Err(FetchError::ElectrumClient(e))
            }
        }
    }

    async fn coinbase(&self, hash: &BlockHash, height: u64) -> Result<Transaction, FetchError> {
        // We can't fetch the coinbase transaction by block hash (not supported by the electrum protocol).
        // However, we can fetch the block by height and compare the hash to the expected hash. If these
        // match (they only match if the block is on the active chain), then we can fetch the coinbase by
        // height too.

        let hash_electrum = self.block_hash(height).await?;

        if *hash == hash_electrum {
            let client = self.get_client()?;
            let txid = client.txid_from_pos(height as usize, /*coinbase*/ 0)?;
            return Ok(client.transaction_get(&txid)?);
        }

        return Err(FetchError::DataError(
            "Could not fetch coinbase from non-active chain. Not supported by Electrum."
                .to_string(),
        ));
    }

    async fn block(&self, _hash: &BlockHash) -> Result<Vec<u8>, FetchError> {
        // The Electrum protocol has no way to fetch a full block by its hash.
        Err(FetchError::DataError(
            "Fetching full blocks by hash is not supported by Electrum.".to_string(),
        ))
    }

    async fn batch_header_fetch(
        &self,
        start_height: u64,
        count: u64,
    ) -> Result<Vec<Header>, FetchError> {
        let client = self.get_client()?;
        Ok(client
            .block_headers(start_height as usize, count as usize)?
            .headers)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn a_server_that_cant_be_reached_is_an_error() {
        // Nothing listens on this port.
        let node = Electrum::new(
            NodeInfo {
                id: 3,
                name: "electrum".to_string(),
                description: String::new(),
                implementation: "electrum".to_string(),
            },
            "tcp://127.0.0.1:1".to_string(),
        );

        assert!(matches!(
            node.tips().await,
            Err(FetchError::ElectrumClient(_))
        ));
    }
}
