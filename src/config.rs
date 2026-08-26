use crate::backend::{
    BitcoinCoreNode, BlockDn, BtcdNode, Electrum, Esplora, MempoolSpace, Node, NodeInfo,
};
use crate::error::ConfigError;
use corepc_client::bitcoin::Network as BitcoinNetwork;
use corepc_client::client_sync::Auth;
use log::{error, info, warn};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use std::hash::Hash;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::str::FromStr;
use std::sync::Arc;
use std::time::Duration;
use std::{env, fmt, fs};

pub const ENVVAR_CONFIG_FILE: &str = "CONFIG_FILE";
const DEFAULT_CONFIG: &str = "config.toml";
const DEFAULT_BACKEND: Backend = Backend::BitcoinCore;
const DEFAULT_USE_REST: bool = true;
const DEFAULT_USE_WAITFORNEWBLOCK: bool = true;
const DEFAULT_RPC_PORT: u16 = 8332;

pub type BoxedSyncSendNode = Arc<dyn Node + Send + Sync>;

#[derive(Clone, Deserialize, Debug)]
pub enum PoolIdentificationNetwork {
    Mainnet,
    Testnet,
    Signet,
    Regtest,
}

impl PoolIdentificationNetwork {
    pub fn to_network(&self) -> BitcoinNetwork {
        match self {
            PoolIdentificationNetwork::Mainnet => BitcoinNetwork::Bitcoin,
            PoolIdentificationNetwork::Testnet => BitcoinNetwork::Testnet,
            PoolIdentificationNetwork::Signet => BitcoinNetwork::Signet,
            PoolIdentificationNetwork::Regtest => BitcoinNetwork::Regtest,
        }
    }
}

#[derive(Deserialize)]
struct TomlConfig {
    address: String,
    database_path: String,
    www_path: String,
    rss_base_url: Option<String>,
    query_interval: u64,
    networks: Vec<TomlNetwork>,
    footer_html: String,
    activity: Option<TomlActivity>,
}

#[derive(Deserialize)]
struct TomlActivity {
    database_path: String,
    archive_directory: Option<String>,
    retention_days: Option<u64>,
}

/// Configuration of the activity log. The activity log is only enabled when
/// the `[activity]` section is present in the configuration file.
#[derive(Clone)]
pub struct ActivityConfig {
    /// Path of the activity SQLite database (separate from the headers
    /// database).
    pub database_path: PathBuf,
    /// Directory the retention task writes monthly archive files to.
    /// Required when a retention is configured.
    pub archive_directory: Option<PathBuf>,
    /// Events older than this many days are archived and purged. Networks
    /// can override this with `activity_retention_days`. Unset means events
    /// are kept forever.
    pub retention_days: Option<u64>,
}

#[derive(Clone)]
pub struct Config {
    pub database_path: PathBuf,
    pub www_path: PathBuf,
    pub query_interval: Duration,
    pub address: SocketAddr,
    pub networks: Vec<Network>,
    pub footer_html: String,
    pub rss_base_url: String,
    pub activity: Option<ActivityConfig>,
}

#[derive(Debug, Deserialize, Clone, Default)]
pub struct PoolIdentification {
    pub enable: bool,
    pub network: Option<PoolIdentificationNetwork>,
}

/// A countdown to a specific block height, shown in the frontend. At most one
/// per network; when unset for a network, no countdown is shown.
#[derive(Debug, Deserialize, Serialize, Clone, PartialEq)]
pub struct Countdown {
    pub height: u64,
    pub label: String,
}

#[derive(Debug, Deserialize)]
struct TomlNetwork {
    id: u32,
    name: String,
    description: String,
    /// An optional URL-friendly identifier for the network (e.g. `testnet4`).
    /// When omitted it is derived from the name. Used for friendly URLs like
    /// `/testnet4` and `?network=testnet4`.
    slug: Option<String>,
    min_fork_height: u64,
    max_interesting_heights: usize,
    nodes: Vec<TomlNode>,
    forkobservers: Option<Vec<TomlRemoteForkObserver>>,
    pool_identification: Option<PoolIdentification>,
    countdown: Option<Countdown>,
    activity_retention_days: Option<u64>,
}

#[derive(Clone)]
pub struct Network {
    pub id: u32,
    pub description: String,
    pub name: String,
    pub slug: String,
    pub min_fork_height: u64,
    pub max_interesting_heights: usize,
    pub nodes: Vec<BoxedSyncSendNode>,
    pub remote_forkobservers: Vec<RemoteForkObserver>,
    pub pool_identification: PoolIdentification,
    pub countdown: Option<Countdown>,
    /// Per-network override of `[activity].retention_days`.
    pub activity_retention_days: Option<u64>,
    /// The ids of this network's nodes that opted into activity logging
    /// (`activity_log = true`).
    pub activity_log_node_ids: BTreeSet<u32>,
}

/// Another fork-observer instance used as a data source: its header and node
/// information is fetched via its HTTP API and shown alongside the local nodes.
#[derive(Debug, Deserialize, Clone)]
struct TomlRemoteForkObserver {
    name: String,
    description: Option<String>,
    url: String,
    network_id: u32,
    node_id_offset: u32,
}

#[derive(Debug, Clone)]
pub struct RemoteForkObserver {
    pub name: String,
    pub description: String,
    /// Base URL of the remote instance, normalized: carries a scheme and has
    /// no trailing slash.
    pub url: String,
    /// The id of the network ON THE REMOTE instance to fetch data from.
    pub network_id: u32,
    /// Added to the remote node ids to avoid collisions with local node ids.
    pub node_id_offset: u32,
}

impl fmt::Display for TomlNetwork {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(
            f,"Network (id={}, description='{}', name='{}', min_fork_height={}, max_interesting_heights={}, nodes={:?})",
            self.id,
            self.description,
            self.name,
            self.min_fork_height,
            self.max_interesting_heights,
            self.nodes,
        )
    }
}

#[derive(Debug, Deserialize)]
struct TomlNode {
    id: u32,
    description: String,
    name: String,
    rpc_host: String,
    rpc_port: Option<u16>,
    rpc_cookie_file: Option<PathBuf>,
    rpc_user: Option<String>,
    rpc_password: Option<String>,
    use_rest: Option<bool>,
    use_waitfornewblock: Option<bool>,
    implementation: Option<String>,
    activity_log: Option<bool>,
}

impl fmt::Display for TomlNode {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(
            f,"Node (id={}, description='{}', name='{}', rpc_host='{}', rpc_port={}, rpc_user='{}', rpc_password='***', rpc_cookie_file={:?}, use_rest={}, use_waitfornewblock={}, implementation='{}')",
            self.id,
            self.description,
            self.name,
            self.rpc_host,
            self.rpc_port.unwrap_or(DEFAULT_RPC_PORT),
            self.rpc_user.as_ref().unwrap_or(&"".to_string()),
            self.rpc_cookie_file,
            self.use_rest.unwrap_or(DEFAULT_USE_REST),
            self.use_waitfornewblock.unwrap_or(DEFAULT_USE_WAITFORNEWBLOCK),
            self.implementation.as_ref().unwrap_or(&"".to_string()),
        )
    }
}

#[derive(Hash, Clone)]
pub enum Backend {
    BitcoinCore,
    Btcd,
    /// An esplora based backend.
    Esplora,
    /// An Electrum server as backend.
    Electrum,
    /// A mempool.space instance, additionally exposing stale chain tips via
    /// its `/api/v1/chain-tips` endpoint.
    MempoolSpace,
    /// A block-dn server as backend.
    BlockDn,
}

impl FromStr for Backend {
    type Err = ConfigError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_lowercase().as_str() {
            "bitcoincore" => Ok(Backend::BitcoinCore),
            "bitcoin core" => Ok(Backend::BitcoinCore),
            "core" => Ok(Backend::BitcoinCore),
            "btcd" => Ok(Backend::Btcd),
            "esplora" => Ok(Backend::Esplora),
            "electrum" => Ok(Backend::Electrum),
            "mempoolspace" => Ok(Backend::MempoolSpace),
            "mempool.space" => Ok(Backend::MempoolSpace),
            "mempool" => Ok(Backend::MempoolSpace),
            "blockdn" => Ok(Backend::BlockDn),
            "block-dn" => Ok(Backend::BlockDn),
            _ => Err(ConfigError::UnknownImplementation),
        }
    }
}

impl fmt::Display for Backend {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            Backend::BitcoinCore => write!(f, "Bitcoin Core"),
            Backend::Btcd => write!(f, "btcd"),
            Backend::Esplora => write!(f, "esplora"),
            Backend::Electrum => write!(f, "electrum"),
            Backend::MempoolSpace => write!(f, "mempool.space"),
            Backend::BlockDn => write!(f, "block-dn"),
        }
    }
}

fn parse_rpc_auth(node_config: &TomlNode) -> Result<Auth, ConfigError> {
    if node_config.rpc_cookie_file.is_some() {
        if let Some(rpc_cookie_file) = node_config.rpc_cookie_file.clone() {
            if !rpc_cookie_file.exists() {
                return Err(ConfigError::CookieFileDoesNotExist);
            }
            return Ok(Auth::CookieFile(rpc_cookie_file));
        }
    } else if let (Some(user), Some(password)) = (
        node_config.rpc_user.clone(),
        node_config.rpc_password.clone(),
    ) {
        return Ok(Auth::UserPass(user, password));
    }
    Err(ConfigError::NoBitcoinCoreRpcAuth)
}

pub fn load_config() -> Result<Config, ConfigError> {
    let config_file_path =
        env::var(ENVVAR_CONFIG_FILE).unwrap_or_else(|_| DEFAULT_CONFIG.to_string());
    info!("Reading configuration file from {}.", config_file_path);
    let config_string = fs::read_to_string(config_file_path)?;
    parse_config(&config_string)
}

fn parse_config(config_str: &str) -> Result<Config, ConfigError> {
    let toml_config: TomlConfig = toml::from_str(config_str)?;

    let mut networks: Vec<Network> = vec![];
    let mut network_ids: Vec<u32> = vec![];
    let mut network_slugs: Vec<String> = vec![];
    for toml_network in toml_config.networks.iter() {
        let mut nodes: Vec<BoxedSyncSendNode> = vec![];
        let mut node_ids: Vec<u32> = vec![];
        let mut activity_log_node_ids: BTreeSet<u32> = BTreeSet::new();
        for toml_node in toml_network.nodes.iter() {
            if toml_node.activity_log.unwrap_or(false) {
                if toml_config.activity.is_some() {
                    activity_log_node_ids.insert(toml_node.id);
                } else {
                    warn!(
                        "node '{}' (id={}) on network '{}' sets activity_log = true, but there \
                         is no [activity] section in the configuration: activity logging is off",
                        toml_node.name, toml_node.id, toml_network.name
                    );
                }
            }
            match parse_toml_node(toml_node) {
                Ok(node) => {
                    if !node_ids.contains(&node.info().id) {
                        node_ids.push(node.info().id);
                        nodes.push(node);
                    } else {
                        error!(
                            "Duplicate node id {}: The node {} could not be loaded.",
                            node.info().id,
                            node.info()
                        );
                        return Err(ConfigError::DuplicateNodeId);
                    }
                }
                Err(e) => {
                    error!("Error while parsing a node configuration: {}", toml_node);
                    return Err(e);
                }
            }
        }
        match parse_toml_network(toml_network, nodes, activity_log_node_ids) {
            Ok(network) => {
                if network_ids.contains(&network.id) {
                    error!(
                        "Duplicate network id {}: The network {} could not be loaded.",
                        network.id, network.name
                    );
                    return Err(ConfigError::DuplicateNetworkId);
                }
                if network_slugs.contains(&network.slug) {
                    error!(
                        "Duplicate network slug '{}': The network {} could not be loaded.",
                        network.slug, network.name
                    );
                    return Err(ConfigError::DuplicateNetworkSlug);
                }
                network_ids.push(network.id);
                network_slugs.push(network.slug.clone());
                networks.push(network);
            }
            Err(e) => {
                error!(
                    "Error while parsing a network configuration: {:?}",
                    toml_network,
                );
                return Err(e);
            }
        }
    }

    if networks.is_empty() {
        return Err(ConfigError::NoNetworks);
    }

    let activity = match toml_config.activity {
        Some(toml_activity) => {
            let activity = ActivityConfig {
                database_path: PathBuf::from(toml_activity.database_path),
                archive_directory: toml_activity.archive_directory.map(PathBuf::from),
                retention_days: toml_activity.retention_days,
            };
            // Archive-then-purge: a retention without a place to archive to
            // would mean deleting events without keeping them.
            let retention_configured = activity.retention_days.is_some()
                || networks.iter().any(|n| n.activity_retention_days.is_some());
            if retention_configured && activity.archive_directory.is_none() {
                return Err(ConfigError::ActivityRetentionWithoutArchiveDir);
            }
            Some(activity)
        }
        None => None,
    };

    Ok(Config {
        database_path: PathBuf::from(toml_config.database_path),
        www_path: PathBuf::from(toml_config.www_path),
        query_interval: Duration::from_secs(toml_config.query_interval),
        address: SocketAddr::from_str(&toml_config.address)?,
        footer_html: toml_config.footer_html.clone(),
        rss_base_url: toml_config.rss_base_url.unwrap_or_default().clone(),
        networks,
        activity,
    })
}

fn parse_toml_network(
    toml_network: &TomlNetwork,
    nodes: Vec<BoxedSyncSendNode>,
    activity_log_node_ids: BTreeSet<u32>,
) -> Result<Network, ConfigError> {
    // Use the configured slug if present, otherwise derive one from the name.
    // Either way we slugify to guarantee a URL-safe result. As a last resort
    // (e.g. an empty or purely-symbolic name) we fall back to the network id.
    let mut slug = slugify(toml_network.slug.as_deref().unwrap_or(&toml_network.name));
    if slug.is_empty() {
        slug = toml_network.id.to_string();
    }

    let max_local_node_id = nodes.iter().map(|n| n.info().id).max();
    let mut remote_forkobservers: Vec<RemoteForkObserver> = vec![];
    let mut offsets: Vec<u32> = vec![];
    for toml_remote in toml_network
        .forkobservers
        .clone()
        .unwrap_or_default()
        .iter()
    {
        let remote = parse_toml_remote_forkobserver(toml_remote, max_local_node_id)?;
        if offsets.contains(&remote.node_id_offset) {
            error!(
                "Duplicate node_id_offset {}: The remote fork-observer '{}' could not be loaded.",
                remote.node_id_offset, remote.name
            );
            return Err(ConfigError::DuplicateRemoteNodeIdOffset);
        }
        offsets.push(remote.node_id_offset);
        remote_forkobservers.push(remote);
    }

    Ok(Network {
        id: toml_network.id,
        name: toml_network.name.clone(),
        slug,
        description: toml_network.description.clone(),
        min_fork_height: toml_network.min_fork_height,
        max_interesting_heights: toml_network.max_interesting_heights,
        nodes,
        remote_forkobservers,
        pool_identification: toml_network.pool_identification.clone().unwrap_or_default(),
        countdown: toml_network.countdown.clone(),
        activity_retention_days: toml_network.activity_retention_days,
        activity_log_node_ids,
    })
}

fn parse_toml_remote_forkobserver(
    toml_remote: &TomlRemoteForkObserver,
    max_local_node_id: Option<u32>,
) -> Result<RemoteForkObserver, ConfigError> {
    if toml_remote.url.trim().is_empty() {
        return Err(ConfigError::InvalidRemoteForkObserver(format!(
            "the remote fork-observer '{}' has an empty url",
            toml_remote.name
        )));
    }
    // The offset is added to the remote node ids. Requiring it to be larger
    // than every local node id makes a collision with a local node impossible,
    // so the poller doesn't have to handle one.
    if toml_remote.node_id_offset <= max_local_node_id.unwrap_or(0) {
        return Err(ConfigError::InvalidRemoteForkObserver(format!(
            "the remote fork-observer '{}' has a node_id_offset of {} - it must be larger than every node id in this network (the largest is {}) to avoid id collisions",
            toml_remote.name,
            toml_remote.node_id_offset,
            max_local_node_id.unwrap_or(0),
        )));
    }
    Ok(RemoteForkObserver {
        name: toml_remote.name.clone(),
        description: toml_remote.description.clone().unwrap_or_default(),
        url: ensure_scheme(toml_remote.url.trim())
            .trim_end_matches('/')
            .to_string(),
        network_id: toml_remote.network_id,
        node_id_offset: toml_remote.node_id_offset,
    })
}

/// Turns an arbitrary string into a URL-friendly slug: ASCII alphanumerics are
/// lowercased and any run of other characters becomes a single `-`. Leading and
/// trailing dashes are trimmed (e.g. `"Testnet 4!"` becomes `"testnet-4"`).
fn slugify(s: &str) -> String {
    let mut slug = String::new();
    let mut pending_dash = false;
    for c in s.chars() {
        if c.is_ascii_alphanumeric() {
            if pending_dash && !slug.is_empty() {
                slug.push('-');
            }
            pending_dash = false;
            slug.push(c.to_ascii_lowercase());
        } else {
            pending_dash = true;
        }
    }
    slug
}

/// Ensures the host carries a URL scheme. For backwards compatibility we default
/// to `http://` when none is given (older configs used a bare host like
/// `127.0.0.1`); an explicit scheme (`http://`, `https://`) is preserved.
fn ensure_scheme(host: &str) -> String {
    if host.contains("://") {
        host.to_string()
    } else {
        format!("http://{}", host)
    }
}

fn parse_toml_node(toml_node: &TomlNode) -> Result<BoxedSyncSendNode, ConfigError> {
    let implementation = toml_node
        .implementation
        .as_ref()
        .unwrap_or(&DEFAULT_BACKEND.to_string())
        .parse::<Backend>()?;

    let node_info = NodeInfo {
        id: toml_node.id,
        name: toml_node.name.clone(),
        description: toml_node.description.clone(),
        implementation: implementation.to_string(),
    };

    let node: BoxedSyncSendNode = match implementation {
        Backend::BitcoinCore => Arc::new(BitcoinCoreNode::new(
            node_info,
            format!(
                "{}:{}",
                ensure_scheme(&toml_node.rpc_host),
                toml_node.rpc_port.unwrap_or(DEFAULT_RPC_PORT)
            ),
            parse_rpc_auth(toml_node)?,
            toml_node.use_rest.unwrap_or(DEFAULT_USE_REST),
            toml_node
                .use_waitfornewblock
                .unwrap_or(DEFAULT_USE_WAITFORNEWBLOCK),
        )),
        Backend::Btcd => {
            if toml_node.rpc_user.is_none() || toml_node.rpc_password.is_none() {
                return Err(ConfigError::NoBtcdRpcAuth);
            }

            Arc::new(BtcdNode::new(
                node_info,
                format!(
                    "{}:{}",
                    ensure_scheme(&toml_node.rpc_host),
                    toml_node.rpc_port.unwrap_or(DEFAULT_RPC_PORT)
                ),
                toml_node.rpc_user.clone().expect("a rpc_user for btcd"),
                toml_node
                    .rpc_password
                    .clone()
                    .expect("a rpc_password for btcd"),
            ))
        }
        Backend::Esplora => Arc::new(Esplora::new(node_info, toml_node.rpc_host.clone())),
        Backend::MempoolSpace => Arc::new(MempoolSpace::new(node_info, toml_node.rpc_host.clone())),
        Backend::BlockDn => Arc::new(BlockDn::new(node_info, toml_node.rpc_host.clone())),
        Backend::Electrum => {
            let url = format!(
                "{}:{}",
                toml_node.rpc_host.clone(),
                toml_node.rpc_port.unwrap_or(50002)
            );
            Arc::new(Electrum::new(node_info, url))
        }
    };
    Ok(node)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::ConfigError;

    #[test]
    fn load_example_config() {
        use std::env;

        const FILENAME_EXAMPLE_CONFIG: &str = "config.toml.example";
        env::set_var(ENVVAR_CONFIG_FILE, FILENAME_EXAMPLE_CONFIG);
        let cfg = load_config().unwrap_or_else(|_| {
            panic!(
                "We should be able to load the {} file.",
                FILENAME_EXAMPLE_CONFIG
            )
        });

        assert_eq!(cfg.address.to_string(), "127.0.0.1:2323");
        assert_eq!(cfg.networks.len(), 2);
        assert_eq!(cfg.query_interval, std::time::Duration::from_secs(15));
        assert!(cfg.networks[0].pool_identification.enable);
    }

    #[test]
    fn ensure_scheme_defaults_to_http() {
        // A bare host (the old config style) gets http:// prepended.
        assert_eq!(ensure_scheme("127.0.0.1"), "http://127.0.0.1");
        assert_eq!(ensure_scheme("localhost"), "http://localhost");
        // An explicit scheme is preserved.
        assert_eq!(ensure_scheme("http://localhost"), "http://localhost");
        assert_eq!(ensure_scheme("https://example.org"), "https://example.org");
    }

    #[test]
    fn use_waitfornewblock_parsing() {
        // Defaults to None (=> DEFAULT_USE_WAITFORNEWBLOCK = true) when omitted.
        let node: TomlNode = toml::from_str(
            r#"
            id = 0
            name = "n"
            description = ""
            rpc_host = "127.0.0.1"
            "#,
        )
        .expect("node without use_waitfornewblock should parse");
        assert_eq!(node.use_waitfornewblock, None);
        assert!(node
            .use_waitfornewblock
            .unwrap_or(DEFAULT_USE_WAITFORNEWBLOCK));

        // Honors an explicit `false`.
        let node: TomlNode = toml::from_str(
            r#"
            id = 0
            name = "n"
            description = ""
            rpc_host = "127.0.0.1"
            use_waitfornewblock = false
            "#,
        )
        .expect("node with use_waitfornewblock should parse");
        assert_eq!(node.use_waitfornewblock, Some(false));
    }

    #[test]
    fn slugify_test() {
        assert_eq!(slugify("Testnet4"), "testnet4");
        assert_eq!(slugify("Mainnet"), "mainnet");
        assert_eq!(slugify("Testnet 4!"), "testnet-4");
        assert_eq!(slugify("  Signet  "), "signet");
        assert_eq!(slugify("a__b--c"), "a-b-c");
        assert_eq!(slugify("!!!"), "");
    }

    #[test]
    fn slug_derived_from_name_when_absent() {
        let config = parse_config(
            r#"
            database_path = ""
            www_path = "./www"
            query_interval = 15
            address = "127.0.0.1:2323"
            rss_base_url = ""
            footer_html = ""

            [[networks]]
            id = 1
            name = "Testnet 4"
            description = ""
            min_fork_height = 0
            max_interesting_heights = 0

                [[networks.nodes]]
                id = 0
                name = "Node A"
                description = ""
                rpc_host = "127.0.0.1"
                rpc_port = 0
                rpc_user = ""
                rpc_password = ""
        "#,
        )
        .expect("config should parse");
        assert_eq!(config.networks[0].slug, "testnet-4");
    }

    #[test]
    fn explicit_slug_is_used() {
        let config = parse_config(
            r#"
            database_path = ""
            www_path = "./www"
            query_interval = 15
            address = "127.0.0.1:2323"
            rss_base_url = ""
            footer_html = ""

            [[networks]]
            id = 1
            name = "Testnet 4"
            slug = "tn4"
            description = ""
            min_fork_height = 0
            max_interesting_heights = 0

                [[networks.nodes]]
                id = 0
                name = "Node A"
                description = ""
                rpc_host = "127.0.0.1"
                rpc_port = 0
                rpc_user = ""
                rpc_password = ""
        "#,
        )
        .expect("config should parse");
        assert_eq!(config.networks[0].slug, "tn4");
    }

    #[test]
    fn countdown_is_parsed_when_present() {
        let config = parse_config(
            r#"
            database_path = ""
            www_path = "./www"
            query_interval = 15
            address = "127.0.0.1:2323"
            rss_base_url = ""
            footer_html = ""

            [[networks]]
            id = 1
            name = "Testnet 4"
            description = ""
            min_fork_height = 0
            max_interesting_heights = 0

                [networks.countdown]
                height = 105
                label = "Halving"

                [[networks.nodes]]
                id = 0
                name = "Node A"
                description = ""
                rpc_host = "127.0.0.1"
                rpc_port = 0
                rpc_user = ""
                rpc_password = ""
        "#,
        )
        .expect("config should parse");
        let countdown = config.networks[0]
            .countdown
            .as_ref()
            .expect("countdown should be set");
        assert_eq!(countdown.height, 105);
        assert_eq!(countdown.label, "Halving");
    }

    #[test]
    fn countdown_is_none_when_absent() {
        let config = parse_config(
            r#"
            database_path = ""
            www_path = "./www"
            query_interval = 15
            address = "127.0.0.1:2323"
            rss_base_url = ""
            footer_html = ""

            [[networks]]
            id = 1
            name = "Testnet 4"
            description = ""
            min_fork_height = 0
            max_interesting_heights = 0

                [[networks.nodes]]
                id = 0
                name = "Node A"
                description = ""
                rpc_host = "127.0.0.1"
                rpc_port = 0
                rpc_user = ""
                rpc_password = ""
        "#,
        )
        .expect("config should parse");
        assert!(config.networks[0].countdown.is_none());
    }

    #[test]
    fn error_on_duplicate_network_slug_test() {
        // Two different names that slugify to the same slug.
        match parse_config(
            r#"
            database_path = ""
            www_path = "./www"
            query_interval = 15
            address = "127.0.0.1:2323"
            rss_base_url = ""
            footer_html = ""

            [[networks]]
            id = 1
            name = "Testnet 4"
            description = ""
            min_fork_height = 0
            max_interesting_heights = 0

                [[networks.nodes]]
                id = 0
                name = "Node A"
                description = ""
                rpc_host = "127.0.0.1"
                rpc_port = 0
                rpc_user = ""
                rpc_password = ""
            [[networks]]
            id = 2
            name = "testnet-4"
            description = ""
            min_fork_height = 0
            max_interesting_heights = 0

                [[networks.nodes]]
                id = 0
                name = "Node B"
                description = ""
                rpc_host = "127.0.0.1"
                rpc_port = 0
                rpc_user = ""
                rpc_password = ""
        "#,
        ) {
            Err(ConfigError::DuplicateNetworkSlug) => {
                // test OK, as we expect this to error
            }
            _ => panic!("expected DuplicateNetworkSlug error"),
        }
    }

    #[test]
    fn activity_config_parsing() {
        let config = parse_config(
            r#"
            database_path = ""
            www_path = "./www"
            query_interval = 15
            address = "127.0.0.1:2323"
            rss_base_url = ""
            footer_html = ""

            [activity]
            database_path = "./activity.sqlite"
            archive_directory = "./activity-archive"
            retention_days = 90

            [[networks]]
            id = 1
            name = ""
            description = ""
            min_fork_height = 0
            max_interesting_heights = 0
            activity_retention_days = 30

                [[networks.nodes]]
                id = 0
                name = "Node A"
                description = ""
                rpc_host = "127.0.0.1"
                rpc_user = ""
                rpc_password = ""
                activity_log = true

                [[networks.nodes]]
                id = 1
                name = "Node B"
                description = ""
                rpc_host = "127.0.0.1"
                rpc_user = ""
                rpc_password = ""
        "#,
        )
        .expect("the activity configuration should parse");

        let activity = config.activity.expect("activity config should be set");
        assert_eq!(activity.database_path, PathBuf::from("./activity.sqlite"));
        assert_eq!(
            activity.archive_directory,
            Some(PathBuf::from("./activity-archive"))
        );
        assert_eq!(activity.retention_days, Some(90));
        assert_eq!(config.networks[0].activity_retention_days, Some(30));
        // Only the opted-in node is in the set.
        assert!(config.networks[0].activity_log_node_ids.contains(&0));
        assert!(!config.networks[0].activity_log_node_ids.contains(&1));
    }

    #[test]
    fn activity_disabled_without_section() {
        let config = parse_config(
            r#"
            database_path = ""
            www_path = "./www"
            query_interval = 15
            address = "127.0.0.1:2323"
            rss_base_url = ""
            footer_html = ""

            [[networks]]
            id = 1
            name = ""
            description = ""
            min_fork_height = 0
            max_interesting_heights = 0

                [[networks.nodes]]
                id = 0
                name = "Node A"
                description = ""
                rpc_host = "127.0.0.1"
                rpc_user = ""
                rpc_password = ""
                activity_log = true
        "#,
        )
        .expect("a config without [activity] should parse");

        // Without an [activity] section the feature is off, even when nodes
        // opted in (a warning is logged).
        assert!(config.activity.is_none());
        assert!(config.networks[0].activity_log_node_ids.is_empty());
    }

    #[test]
    fn error_on_activity_retention_without_archive_dir() {
        if let Err(ConfigError::ActivityRetentionWithoutArchiveDir) = parse_config(
            r#"
            database_path = ""
            www_path = "./www"
            query_interval = 15
            address = "127.0.0.1:2323"
            rss_base_url = ""
            footer_html = ""

            [activity]
            database_path = "./activity.sqlite"
            retention_days = 90

            [[networks]]
            id = 1
            name = ""
            description = ""
            min_fork_height = 0
            max_interesting_heights = 0

                [[networks.nodes]]
                id = 0
                name = "Node A"
                description = ""
                rpc_host = "127.0.0.1"
                rpc_user = ""
                rpc_password = ""
        "#,
        ) {
            // test OK, as we expect this to error
        } else {
            panic!("Test did not error!");
        }
    }

    #[test]
    fn error_on_duplicate_node_id_test() {
        if let Err(ConfigError::DuplicateNodeId) = parse_config(
            r#"
            database_path = ""
            www_path = "./www"
            query_interval = 15
            address = "127.0.0.1:2323"
            rss_base_url = ""
            footer_html = ""

            [[networks]]
            id = 1
            name = ""
            description = ""
            min_fork_height = 0
            max_interesting_heights = 0

                [[networks.nodes]]
                id = 0
                name = "Node A"
                description = ""
                rpc_host = "127.0.0.1"
                rpc_port = 0
                rpc_user = ""
                rpc_password = ""

                [[networks.nodes]]
                id = 0
                name = "Node B"
                description = ""
                rpc_host = "127.0.0.1"
                rpc_port = 0
                rpc_user = ""
                rpc_password = ""
        "#,
        ) {
            // test OK, as we expect this to error
        } else {
            panic!("Test did not error!");
        }
    }

    #[test]
    fn error_on_duplicate_network_id_test() {
        if let Err(ConfigError::DuplicateNetworkId) = parse_config(
            r#"
            database_path = ""
            www_path = "./www"
            query_interval = 15
            address = "127.0.0.1:2323"
            rss_base_url = ""
            footer_html = ""

            [[networks]]
            id = 1
            name = ""
            description = ""
            min_fork_height = 0
            max_interesting_heights = 0

                [[networks.nodes]]
                id = 0
                name = "Node B"
                description = ""
                rpc_host = "127.0.0.1"
                rpc_port = 0
                rpc_user = ""
                rpc_password = ""
            [[networks]]
            id = 1
            name = ""
            description = ""
            min_fork_height = 0
            max_interesting_heights = 0

                [[networks.nodes]]
                id = 0
                name = "Node B"
                description = ""
                rpc_host = "127.0.0.1"
                rpc_port = 0
                rpc_user = ""
                rpc_password = ""
        "#,
        ) {
            // test OK, as we expect this to error
        } else {
            panic!("Test did not error!");
        }
    }

    #[test]
    fn remote_forkobserver_parsing() {
        let config = parse_config(
            r#"
            database_path = ""
            www_path = "./www"
            query_interval = 15
            address = "127.0.0.1:2323"
            rss_base_url = ""
            footer_html = ""

            [[networks]]
            id = 1
            name = "Mainnet"
            description = ""
            min_fork_height = 0
            max_interesting_heights = 0

                [[networks.nodes]]
                id = 0
                name = "Node A"
                description = ""
                rpc_host = "127.0.0.1"
                rpc_port = 0
                rpc_user = ""
                rpc_password = ""

                [[networks.forkobservers]]
                name = "example observer"
                url = "fork-observer.example.com/"
                network_id = 7
                node_id_offset = 1000
        "#,
        )
        .expect("config with a remote fork-observer should parse");
        let remotes = &config.networks[0].remote_forkobservers;
        assert_eq!(remotes.len(), 1);
        assert_eq!(remotes[0].name, "example observer");
        // Scheme is added and the trailing slash is trimmed.
        assert_eq!(remotes[0].url, "http://fork-observer.example.com");
        assert_eq!(remotes[0].network_id, 7);
        assert_eq!(remotes[0].node_id_offset, 1000);
        assert_eq!(remotes[0].description, "");
    }

    #[test]
    fn error_on_duplicate_remote_forkobserver_offset() {
        match parse_config(
            r#"
            database_path = ""
            www_path = "./www"
            query_interval = 15
            address = "127.0.0.1:2323"
            rss_base_url = ""
            footer_html = ""

            [[networks]]
            id = 1
            name = "Mainnet"
            description = ""
            min_fork_height = 0
            max_interesting_heights = 0

                [[networks.nodes]]
                id = 0
                name = "Node A"
                description = ""
                rpc_host = "127.0.0.1"
                rpc_port = 0
                rpc_user = ""
                rpc_password = ""

                [[networks.forkobservers]]
                name = "observer 1"
                url = "https://one.example.com"
                network_id = 1
                node_id_offset = 1000

                [[networks.forkobservers]]
                name = "observer 2"
                url = "https://two.example.com"
                network_id = 1
                node_id_offset = 1000
        "#,
        ) {
            Err(ConfigError::DuplicateRemoteNodeIdOffset) => {
                // test OK, as we expect this to error
            }
            _ => panic!("expected DuplicateRemoteNodeIdOffset error"),
        }
    }

    #[test]
    fn error_on_invalid_remote_forkobserver() {
        for (url, node_id_offset) in [("https://example.com", 0), ("  ", 1000)] {
            match parse_config(&format!(
                r#"
                database_path = ""
                www_path = "./www"
                query_interval = 15
                address = "127.0.0.1:2323"
                rss_base_url = ""
                footer_html = ""

                [[networks]]
                id = 1
                name = "Mainnet"
                description = ""
                min_fork_height = 0
                max_interesting_heights = 0

                    [[networks.nodes]]
                    id = 0
                    name = "Node A"
                    description = ""
                    rpc_host = "127.0.0.1"
                    rpc_port = 0
                    rpc_user = ""
                    rpc_password = ""

                    [[networks.forkobservers]]
                    name = "observer"
                    url = "{}"
                    network_id = 1
                    node_id_offset = {}
            "#,
                url, node_id_offset
            )) {
                Err(ConfigError::InvalidRemoteForkObserver(_)) => {
                    // test OK, as we expect this to error
                }
                _ => panic!("expected InvalidRemoteForkObserver error"),
            }
        }
    }

    #[test]
    fn esplora_backend_test() {
        match parse_config(
            r#"
            database_path = ""
            www_path = "./www"
            query_interval = 15
            address = "127.0.0.1:2323"
            rss_base_url = ""
            footer_html = ""

            [[networks]]
            id = 1
            name = ""
            description = ""
            min_fork_height = 0
            max_interesting_heights = 0

                [[networks.nodes]]
                id = 123
                name = "Esplora Node"
                description = "A test explora node"
                rpc_host = "https://esplora.example.org/api"
                implementation = "esplora"
        "#,
        ) {
            Ok(config) => {
                let network = &config.networks[0];
                let node: &BoxedSyncSendNode = &network.nodes[0];
                let node_info = node.info();
                assert_eq!(node_info.name, "Esplora Node");
                assert_eq!(node_info.id, 123);
                assert_eq!(node_info.implementation, "esplora");
            }
            Err(e) => {
                panic!("Esplora backend config invalid: {}", e);
            }
        }
    }

    #[test]
    fn mempool_space_backend_test() {
        match parse_config(
            r#"
            database_path = ""
            www_path = "./www"
            query_interval = 15
            address = "127.0.0.1:2323"
            rss_base_url = ""
            footer_html = ""

            [[networks]]
            id = 1
            name = ""
            description = ""
            min_fork_height = 0
            max_interesting_heights = 0

                [[networks.nodes]]
                id = 5
                name = "mempool.space"
                description = "mempool.space public API"
                rpc_host = "https://mempool.space/api"
                implementation = "mempoolspace"
        "#,
        ) {
            Ok(config) => {
                let network = &config.networks[0];
                let node: &BoxedSyncSendNode = &network.nodes[0];
                let node_info = node.info();
                assert_eq!(node_info.name, "mempool.space");
                assert_eq!(node_info.id, 5);
                assert_eq!(node_info.implementation, "mempool.space");
            }
            Err(e) => {
                panic!("mempool.space backend config invalid: {}", e);
            }
        }
    }

    #[test]
    fn backend_from_str_accepts_mempool_space_aliases() {
        for alias in ["mempoolspace", "mempool.space", "mempool", "MempoolSpace"] {
            assert!(
                matches!(alias.parse::<Backend>(), Ok(Backend::MempoolSpace)),
                "expected '{}' to parse as Backend::MempoolSpace",
                alias
            );
        }
    }

    #[test]
    fn blockdn_backend_test() {
        match parse_config(
            r#"
            database_path = ""
            www_path = "./www"
            query_interval = 15
            address = "127.0.0.1:2323"
            rss_base_url = ""
            footer_html = ""

            [[networks]]
            id = 1
            name = ""
            description = ""
            min_fork_height = 0
            max_interesting_heights = 0

                [[networks.nodes]]
                id = 124
                name = "block-dn.org"
                description = "A test block-dn node"
                rpc_host = "https://block-dn.org"
                implementation = "block-dn"
        "#,
        ) {
            Ok(config) => {
                let network = &config.networks[0];
                let node: &BoxedSyncSendNode = &network.nodes[0];
                let node_info = node.info();
                assert_eq!(node_info.name, "block-dn.org");
                assert_eq!(node_info.id, 124);
                assert_eq!(node_info.implementation, "block-dn");
            }
            Err(e) => {
                panic!("block-dn backend config invalid: {}", e);
            }
        }
    }
}

#[test]
fn esplora_backend_test() {
    match parse_config(
        r#"
            database_path = ""
            www_path = "./www"
            query_interval = 15
            address = "127.0.0.1:2323"
            rss_base_url = ""
            footer_html = ""

            [[networks]]
            id = 1
            name = ""
            description = ""
            min_fork_height = 0
            max_interesting_heights = 0

                [[networks.nodes]]
                id = 421
                name = "Electrum"
                description = "electrum"
                rpc_host = "tcp://localhost"
                rpc_port = 1337
                implementation = "electrum"
        "#,
    ) {
        Ok(config) => {
            let network = &config.networks[0];
            let node: &BoxedSyncSendNode = &network.nodes[0];
            let node_info = node.info();
            assert_eq!(node_info.name, "Electrum");
            assert_eq!(node_info.id, 421);
            assert_eq!(node_info.implementation, "electrum");
        }
        Err(e) => {
            panic!("Electrum backend config invalid: {}", e);
        }
    }
}
