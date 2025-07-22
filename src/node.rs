use crate::error::{EsploraRESTError, FetchError, JsonRPCError};
use crate::types::{ChainTip, ChainTipStatus, HeaderInfo, Tree};
use async_trait::async_trait;
use bitcoincore_rpc::bitcoin;
use bitcoincore_rpc::bitcoin::blockdata::block::Header;
use bitcoincore_rpc::bitcoin::hex::FromHex;
use bitcoincore_rpc::bitcoin::{BlockHash, Transaction};
use bitcoincore_rpc::Auth;
use bitcoincore_rpc::Client;
use bitcoincore_rpc::RpcApi;
use electrum_client::{
    Client as ElectrumClient, ConfigBuilder as ElectrumClientConfigBuilder, ElectrumApi,
};
use log::{debug, error};
use std::cmp::max;
use std::fmt;
use std::str::FromStr;
use std::sync::OnceLock;
use std::thread::sleep;
use std::time::Duration;
use tokio::task;

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
    /// Fetches a batch of successive headers from the active chain.
    async fn batch_header_fetch(
        &self,
        start_hash: BlockHash,
        start_height: u64,
        count: u64,
    ) -> Result<Vec<Header>, FetchError>;

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
                    // get the header hash for a header STEP_SIZE away from query_height
                    let header_hash = self.block_hash(start_height as u64).await?;

                    // get STEP_SIZE headers
                    let headers = self
                        .batch_header_fetch(header_hash, start_height as u64, STEP_SIZE as u64)
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
                    if tree_locked.1.contains_key(&inactive_tip.block_hash()) {
                        break;
                    }
                }

                let height = inactive_tip.height - i as u64;
                debug!(
                    "loading non-active-chain header: hash={}, height={}",
                    next_header, height
                );

                let header = self.block_header_hash(&next_header).await?;
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

#[derive(Hash, Clone)]
pub struct BitcoinCoreNode {
    info: NodeInfo,
    rpc_url: String,
    rpc_auth: Auth,
    use_rest: bool,
}

impl BitcoinCoreNode {
    pub fn new(info: NodeInfo, rpc_url: String, rpc_auth: Auth, use_rest: bool) -> Self {
        BitcoinCoreNode {
            info,
            rpc_url,
            rpc_auth,
            use_rest,
        }
    }

    fn rpc_client(&self) -> Result<Client, FetchError> {
        match Client::new(&self.rpc_url, self.rpc_auth.clone()) {
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
        }
    }

    fn rpc_url(&self) -> String {
        self.rpc_url.clone()
    }

    async fn version(&self) -> Result<String, FetchError> {
        let rpc = self.rpc_client()?;
        match task::spawn_blocking(move || rpc.get_network_info()).await {
            Ok(result) => match result {
                Ok(result) => Ok(result.subversion),
                Err(e) => Err(e.into()),
            },
            Err(e) => Err(e.into()),
        }
    }

    async fn block_hash(&self, height: u64) -> Result<BlockHash, FetchError> {
        let rpc = self.rpc_client()?;
        match task::spawn_blocking(move || rpc.get_block_hash(height)).await {
            Ok(result) => match result {
                Ok(result) => Ok(result),
                Err(e) => Err(e.into()),
            },
            Err(e) => Err(e.into()),
        }
    }

    async fn block_header_hash(&self, hash: &BlockHash) -> Result<Header, FetchError> {
        let rpc = self.rpc_client()?;
        let hash_clone = hash.clone();
        match task::spawn_blocking(move || rpc.get_block_header(&hash_clone)).await {
            Ok(result) => match result {
                Ok(result) => Ok(result),
                Err(e) => Err(e.into()),
            },
            Err(e) => Err(e.into()),
        }
    }

    async fn block_header_height(&self, _: u64) -> Result<Header, FetchError> {
        assert_eq!(self.capabilities().header_fetch_type, HeaderFetchType::Hash);
        Err(FetchError::DataError(
            "fetch by block height not implemented".to_string(),
        ))
    }

    async fn coinbase(&self, hash: &BlockHash, _height: u64) -> Result<Transaction, FetchError> {
        let rpc = self.rpc_client()?;
        let hash_clone = hash.clone();
        match task::spawn_blocking(move || rpc.get_block(&hash_clone)).await {
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

    async fn tips(&self) -> Result<Vec<ChainTip>, FetchError> {
        let rpc = self.rpc_client()?;
        match task::spawn_blocking(move || rpc.get_chain_tips()).await {
            Ok(tips_result) => match tips_result {
                Ok(tips) => Ok(tips.iter().map(|t| t.clone().into()).collect()),
                Err(e) => Err(e.into()),
            },
            Err(e) => Err(e.into()),
        }
    }

    async fn batch_header_fetch(
        &self,
        start_hash: BlockHash,
        start_height: u64,
        count: u64,
    ) -> Result<Vec<Header>, FetchError> {
        debug!(
            "loading active-chain headers starting from {} ({})",
            start_height,
            start_hash.to_string()
        );

        let url = format!(
            "http://{}/rest/headers/{}/{}.bin",
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

        let header_results: Result<
            Vec<Header>,
            bitcoincore_rpc::bitcoin::consensus::encode::Error,
        > = res
            .as_bytes()
            .chunks(80)
            .map(bitcoin::consensus::deserialize::<Header>)
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
            start_hash.to_string()
        );

        Ok(headers)
    }
}

#[derive(Hash, Clone)]
pub struct BtcdNode {
    info: NodeInfo,
    rpc_url: String,
    rpc_user: String,
    rpc_password: String,
}

impl BtcdNode {
    pub fn new(info: NodeInfo, rpc_url: String, rpc_user: String, rpc_password: String) -> Self {
        BtcdNode {
            info,
            rpc_url,
            rpc_user,
            rpc_password,
        }
    }
}

#[async_trait]
impl Node for BtcdNode {
    fn info(&self) -> NodeInfo {
        self.info.clone()
    }

    fn capabilities(&self) -> Capabilities {
        Capabilities {
            header_fetch_type: HeaderFetchType::Hash,
            batch_header_fetch: false,
        }
    }

    fn rpc_url(&self) -> String {
        self.rpc_url.clone()
    }

    async fn version(&self) -> Result<String, FetchError> {
        Err(FetchError::BtcdRPC(JsonRPCError::NotImplemented))
    }

    async fn block_header_hash(&self, hash: &BlockHash) -> Result<Header, FetchError> {
        let url = format!("http://{}/", self.rpc_url);
        match crate::jsonrpc::btcd_blockheader(
            url,
            self.rpc_user.clone(),
            self.rpc_password.clone(),
            hash.to_string(),
        ) {
            Ok(header) => Ok(header),
            Err(error) => Err(FetchError::BtcdRPC(error)),
        }
    }

    async fn block_header_height(&self, _: u64) -> Result<Header, FetchError> {
        assert_eq!(self.capabilities().header_fetch_type, HeaderFetchType::Hash);
        Err(FetchError::DataError(
            "fetch by block height not implemented".to_string(),
        ))
    }

    async fn batch_header_fetch(
        &self,
        _start_hash: BlockHash,
        _start_height: u64,
        _count: u64,
    ) -> Result<Vec<Header>, FetchError> {
        assert!(self.capabilities().batch_header_fetch);
        Err(FetchError::DataError(
            "batch header fetch not implemented".to_string(),
        ))
    }

    async fn coinbase(&self, hash: &BlockHash, _height: u64) -> Result<Transaction, FetchError> {
        let url = format!("http://{}/", self.rpc_url);
        match crate::jsonrpc::btcd_block(
            url,
            self.rpc_user.clone(),
            self.rpc_password.clone(),
            hash.to_string(),
        ) {
            Ok(block) => Ok(block
                .txdata
                .first()
                .expect("Block should have a coinbase transaction")
                .clone()),
            Err(error) => Err(FetchError::BtcdRPC(error)),
        }
    }

    async fn block_hash(&self, height: u64) -> Result<BlockHash, FetchError> {
        let url = format!("http://{}/", self.rpc_url);
        match crate::jsonrpc::btcd_blockhash(
            url,
            self.rpc_user.clone(),
            self.rpc_password.clone(),
            height,
        ) {
            Ok(tips) => Ok(tips),
            Err(error) => Err(FetchError::BtcdRPC(error)),
        }
    }

    async fn tips(&self) -> Result<Vec<ChainTip>, FetchError> {
        let url = format!("http://{}/", self.rpc_url);
        match crate::jsonrpc::btcd_chaintips(url, self.rpc_user.clone(), self.rpc_password.clone())
        {
            Ok(tips) => Ok(tips),
            Err(error) => Err(FetchError::BtcdRPC(error)),
        }
    }
}

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
                    Ok(header_bytes) => match bitcoin::consensus::deserialize(&header_bytes) {
                        Ok(header) => Ok(header),
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
                let coinbase_hex = res.as_str()?;
                let res = minreq::get(url.clone())
                    .with_header("content-type", "plain/text")
                    .with_timeout(8)
                    .send()?;

                match res.status_code {
                    200 => match Vec::from_hex(coinbase_hex) {
                        Ok(coinbase_bytes) => {
                            match bitcoin::consensus::deserialize(&coinbase_bytes) {
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
        _start_hash: BlockHash,
        _start_height: u64,
        _count: u64,
    ) -> Result<Vec<Header>, FetchError> {
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

    fn get_client(&self) -> &ElectrumClient {
        self.client.get_or_init(|| {
            const ELECTRUM_RECONNECT_DURATION: Duration = Duration::from_secs(60);
            let config = ElectrumClientConfigBuilder::new()
                .timeout(Some(10))
                .retry(2)
                .validate_domain(false)
                .build();

            loop {
                match ElectrumClient::from_config(&self.url, config.clone()) {
                    Ok(client) => {
                        log::info!(
                            "Connected to Electrum server {} ({})",
                            self.info.name,
                            self.url
                        );
                        return client;
                    }
                    Err(e) => {
                        log::warn!(
                            "Could not connect to Electrum server {}. Retrying in {:?}. Error: {}",
                            self.url,
                            ELECTRUM_RECONNECT_DURATION,
                            e
                        );
                        sleep(ELECTRUM_RECONNECT_DURATION);
                    }
                }
            }
        })
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
        }
    }

    fn rpc_url(&self) -> String {
        return "not used".to_string();
    }

    async fn version(&self) -> Result<String, FetchError> {
        let client = self.get_client();
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
        let client = self.get_client();
        let header = client.block_header(height as usize)?;
        Ok(header)
    }

    async fn block_hash(&self, height: u64) -> Result<BlockHash, FetchError> {
        let client = self.get_client();
        let header = client.block_header(height as usize)?;
        Ok(header.block_hash())
    }

    async fn tips(&self) -> Result<Vec<ChainTip>, FetchError> {
        let client = self.get_client();

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
            let client = self.get_client();
            let txid = client.txid_from_pos(height as usize, /*coinbase*/ 0)?;
            return Ok(client.transaction_get(&txid)?);
        }

        return Err(FetchError::DataError(
            "Could not fetch coinbase from non-active chain. Not supported by Electrum."
                .to_string(),
        ));
    }

    async fn batch_header_fetch(
        &self,
        _start_hash: BlockHash,
        start_height: u64,
        count: u64,
    ) -> Result<Vec<Header>, FetchError> {
        let client = self.get_client();
        Ok(client
            .block_headers(start_height as usize, count as usize)?
            .headers)
    }
}
