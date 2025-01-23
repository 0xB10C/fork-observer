use std::hash::Hash;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::str::FromStr;
use std::sync::Arc;
use std::time::Duration;
use std::{env, fmt, fs};

use bitcoincore_rpc::bitcoin::Network as BitcoinNetwork;
use bitcoincore_rpc::Auth;
use log::{error, info};
use serde::Deserialize;

use crate::error::ConfigError;
use crate::node::{BitcoinCoreNode, BtcdNode, Node, NodeInfo};

pub const ENVVAR_CONFIG_FILE: &str = "CONFIG_FILE";
const DEFAULT_CONFIG: &str = "config.toml";
const DEFAULT_BACKEND: Backend = Backend::BitcoinCore;
const DEFAULT_USE_REST: bool = true;

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
}

#[derive(Debug, Deserialize, Clone, Default)]
pub struct PoolIdentification {
    pub enable: bool,
    pub network: Option<PoolIdentificationNetwork>,
}

#[derive(Debug, Deserialize)]
struct TomlNetwork {
    id: u32,
    name: String,
    description: String,
    min_fork_height: u64,
    max_interesting_heights: usize,
    nodes: Vec<TomlNode>,
    pool_identification: Option<PoolIdentification>,
}

#[derive(Clone)]
pub struct Network {
    pub id: u32,
    pub description: String,
    pub name: String,
    pub min_fork_height: u64,
    pub max_interesting_heights: usize,
    pub nodes: Vec<BoxedSyncSendNode>,
    pub pool_identification: PoolIdentification,
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
    rpc_port: u16,
    rpc_cookie_file: Option<PathBuf>,
    rpc_user: Option<String>,
    rpc_password: Option<String>,
    use_rest: Option<bool>,
    implementation: Option<String>,
}

impl fmt::Display for TomlNode {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(
            f,"Node (id={}, description='{}', name='{}', rpc_host='{}', rpc_port={}, rpc_user='{}', rpc_password='***', rpc_cookie_file={:?}, use_rest={}, implementation='{}')",
            self.id,
            self.description,
            self.name,
            self.rpc_host,
            self.rpc_port,
            self.rpc_user.as_ref().unwrap_or(&"".to_string()),
            self.rpc_cookie_file,
            self.use_rest.unwrap_or(DEFAULT_USE_REST),
            self.implementation.as_ref().unwrap_or(&"".to_string()),
        )
    }
}

#[derive(Hash, Clone)]
pub enum Backend {
    BitcoinCore,
    Btcd,
}

impl FromStr for Backend {
    type Err = ConfigError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_lowercase().as_str() {
            "bitcoincore" => Ok(Backend::BitcoinCore),
            "bitcoin core" => Ok(Backend::BitcoinCore),
            "core" => Ok(Backend::BitcoinCore),
            "btcd" => Ok(Backend::Btcd),
            _ => Err(ConfigError::UnknownImplementation),
        }
    }
}

impl fmt::Display for Backend {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            Backend::BitcoinCore => write!(f, "Bitcoin Core"),
            Backend::Btcd => write!(f, "btcd"),
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
    for toml_network in toml_config.networks.iter() {
        let mut nodes: Vec<BoxedSyncSendNode> = vec![];
        let mut node_ids: Vec<u32> = vec![];
        for toml_node in toml_network.nodes.iter() {
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
        match parse_toml_network(toml_network, nodes) {
            Ok(network) => {
                if !network_ids.contains(&network.id) {
                    network_ids.push(network.id);
                    networks.push(network);
                } else {
                    error!(
                        "Duplicate network id {}: The network {} could not be loaded.",
                        network.id, network.name
                    );
                    return Err(ConfigError::DuplicateNetworkId);
                }
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

    Ok(Config {
        database_path: PathBuf::from(toml_config.database_path),
        www_path: PathBuf::from(toml_config.www_path),
        query_interval: Duration::from_secs(toml_config.query_interval),
        address: SocketAddr::from_str(&toml_config.address)?,
        footer_html: toml_config.footer_html.clone(),
        rss_base_url: toml_config.rss_base_url.unwrap_or_default().clone(),
        networks,
    })
}

fn parse_toml_network(
    toml_network: &TomlNetwork,
    nodes: Vec<BoxedSyncSendNode>,
) -> Result<Network, ConfigError> {
    Ok(Network {
        id: toml_network.id,
        name: toml_network.name.clone(),
        description: toml_network.description.clone(),
        min_fork_height: toml_network.min_fork_height,
        max_interesting_heights: toml_network.max_interesting_heights,
        nodes,
        pool_identification: toml_network.pool_identification.clone().unwrap_or_default(),
    })
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
            format!("{}:{}", toml_node.rpc_host, toml_node.rpc_port),
            parse_rpc_auth(toml_node)?,
            toml_node.use_rest.unwrap_or(DEFAULT_USE_REST),
        )),
        Backend::Btcd => {
            if toml_node.rpc_user.is_none() || toml_node.rpc_password.is_none() {
                return Err(ConfigError::NoBtcdRpcAuth);
            }

            Arc::new(BtcdNode::new(
                node_info,
                format!("{}:{}", toml_node.rpc_host, toml_node.rpc_port),
                toml_node.rpc_user.clone().expect("a rpc_user for btcd"),
                toml_node
                    .rpc_password
                    .clone()
                    .expect("a rpc_password for btcd"),
            ))
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
        let cfg = load_config().expect(&format!(
            "We should be able to load the {} file.",
            FILENAME_EXAMPLE_CONFIG
        ));

        assert_eq!(cfg.address.to_string(), "127.0.0.1:2323");
        assert_eq!(cfg.networks.len(), 2);
        assert_eq!(cfg.query_interval, std::time::Duration::from_secs(15));
        assert_eq!(cfg.networks[0].pool_identification.enable, true);
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
}
