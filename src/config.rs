use std::hash::Hash;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::str::FromStr;
use std::time::Duration;
use std::{env, fmt, fs};

use bitcoincore_rpc::Auth;
use log::info;
use serde::Deserialize;

use crate::error::ConfigError;

const ENVVAR_CONFIG_FILE: &str = "CONFIG_FILE";
const DEFAULT_CONFIG: &str = "config.toml";
const DEFAULT_NODE_IMPL: NodeImplementation = NodeImplementation::BitcoinCore;

#[derive(Deserialize)]
struct TomlConfig {
    address: String,
    database_path: String,
    www_path: String,
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
}

#[derive(Deserialize)]
struct TomlNetwork {
    id: u32,
    name: String,
    description: String,
    min_fork_height: u64,
    max_forks: u64,
    nodes: Vec<TomlNode>,
}

#[derive(Hash, Clone)]
pub struct Network {
    pub id: u32,
    pub description: String,
    pub name: String,
    pub min_fork_height: u64,
    pub max_forks: u64,
    pub nodes: Vec<Node>,
}

#[derive(Deserialize)]
struct TomlNode {
    id: u8,
    description: String,
    name: String,
    rpc_host: String,
    rpc_port: u16,
    rpc_cookie_file: Option<PathBuf>,
    rpc_user: Option<String>,
    rpc_password: Option<String>,
    use_rest: bool,
    implementation: Option<String>,
}

#[derive(Hash, Clone)]
pub enum NodeImplementation {
    BitcoinCore,
    Btcd,
}

impl FromStr for NodeImplementation {
    type Err = ConfigError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_lowercase().as_str() {
            "bitcoincore" => Ok(NodeImplementation::BitcoinCore),
            "core" => Ok(NodeImplementation::BitcoinCore),
            "btcd" => Ok(NodeImplementation::Btcd),
            _ => Err(ConfigError::UnknownImplementation),
        }
    }
}

impl fmt::Display for NodeImplementation {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            NodeImplementation::BitcoinCore => write!(f, "BitcoinCore"),
            NodeImplementation::Btcd => write!(f, "btcd"),
        }
    }
}

#[derive(Hash, Clone)]
pub struct Node {
    pub id: u8,
    pub description: String,
    pub name: String,
    pub rpc_url: String,
    pub rpc_auth: Auth,
    pub use_rest: bool,
    pub implementation: NodeImplementation,
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
    Err(ConfigError::NoRpcAuth)
}

pub fn load_config() -> Result<Config, ConfigError> {
    let config_file_path =
        env::var(ENVVAR_CONFIG_FILE).unwrap_or_else(|_| DEFAULT_CONFIG.to_string());
    info!("Reading configuration file from {}.", config_file_path);
    let config_string = fs::read_to_string(config_file_path)?;
    let toml_config: TomlConfig = toml::from_str(&config_string)?;

    let mut networks: Vec<Network> = vec![];
    for toml_network in toml_config.networks.iter() {
        let mut nodes: Vec<Node> = vec![];
        for toml_node in toml_network.nodes.iter() {
            nodes.push(Node {
                id: toml_node.id,
                name: toml_node.name.clone(),
                description: toml_node.description.clone(),
                rpc_url: format!("{}:{}", toml_node.rpc_host, toml_node.rpc_port),
                rpc_auth: parse_rpc_auth(toml_node)?,
                use_rest: toml_node.use_rest,
                implementation: toml_node
                    .implementation
                    .as_ref()
                    .unwrap_or(&DEFAULT_NODE_IMPL.to_string())
                    .parse::<NodeImplementation>()?,
            })
        }

        networks.push(Network {
            id: toml_network.id,
            name: toml_network.name.clone(),
            description: toml_network.description.clone(),
            min_fork_height: toml_network.min_fork_height,
            max_forks: toml_network.max_forks,
            nodes,
        });
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
        networks,
    })
}
