use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::net::{AddrParseError, SocketAddr};
use std::path::PathBuf;
use std::str::FromStr;
use std::time::Duration;
use std::{env, error, fmt, fs, io};

use bitcoincore_rpc::Auth;
use serde::Deserialize;

const ENVVAR_CONFIG_FILE: &str = "CONFIG_FILE";
const DEFAULT_CONFIG: &str = "config.toml";

#[derive(Deserialize)]
struct TomlConfig {
    address: String,
    database_path: String,
    www_path: String,
    query_interval: u64,
    networks: Vec<TomlNetwork>,
}

#[derive(Clone)]
pub struct Config {
    pub database_path: PathBuf,
    pub www_path: PathBuf,
    pub query_interval: Duration,
    pub address: SocketAddr,
    pub networks: Vec<Network>,
}

#[derive(Deserialize)]
struct TomlNetwork {
    id: u32,
    name: String,
    description: String,
    nodes: Vec<TomlNode>,
}

#[derive(Hash, Clone)]
pub struct Network {
    pub id: u32,
    pub description: String,
    pub name: String,
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
}

#[derive(Hash, Clone)]
pub struct Node {
    pub id: u8,
    pub description: String,
    pub name: String,
    pub rpc_url: String,
    pub rpc_auth: Auth,
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
    return Err(ConfigError::NoRpcAuth);
}

pub fn load_config() -> Result<Config, ConfigError> {
    let config_file_path =
        env::var(ENVVAR_CONFIG_FILE).unwrap_or_else(|_| DEFAULT_CONFIG.to_string());
    println!("Reading configuration file from {}.", config_file_path);
    let config_string = fs::read_to_string(config_file_path)?;
    let toml_config: TomlConfig = toml::from_str(&config_string)?;

    return Ok(Config {
        database_path: PathBuf::from(toml_config.database_path),
        www_path: PathBuf::from(toml_config.www_path),
        query_interval: Duration::from_secs(toml_config.query_interval),
        address: SocketAddr::from_str(&toml_config.address)?,
        networks: toml_config
            .networks
            .iter()
            .map(|network| Network {
                id: network.id,
                name: network.name.clone(),
                description: network.description.clone(),
                nodes: network
                    .nodes
                    .iter()
                    .map(|node| Node {
                        id: node.id,
                        name: node.name.clone(),
                        description: node.description.clone(),
                        rpc_url: format!("{}:{}", node.rpc_host, node.rpc_port.to_string()),
                        rpc_auth: parse_rpc_auth(node).unwrap(),
                    })
                    .collect(),
            })
            .collect(),
    });
}

#[derive(Debug)]
pub enum ConfigError {
    CookieFileDoesNotExist,
    NoRpcAuth,
    TomlError(toml::de::Error),
    ReadError(io::Error),
    AddrError(AddrParseError),
}

impl fmt::Display for ConfigError {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            ConfigError::CookieFileDoesNotExist => write!(f, "the .cookie file path set via rpc_cookie_file does not exist"),
            ConfigError::NoRpcAuth => write!(f, "please specify a Bitcoin Core RPC .cookie file (option: 'rpc_cookie_file') or a rpc_user and rpc_password"),
            ConfigError::TomlError(e) => write!(f, "the TOML in the configuration file could not be parsed: {}", e),
            ConfigError::ReadError(e) => write!(f, "the configuration file could not be read: {}", e),
            ConfigError::AddrError(e) => write!(f, "the address could not be parsed: {}", e),
        }
    }
}

impl error::Error for ConfigError {
    fn source(&self) -> Option<&(dyn error::Error + 'static)> {
        match *self {
            ConfigError::NoRpcAuth => None,
            ConfigError::CookieFileDoesNotExist => None,
            ConfigError::TomlError(ref e) => Some(e),
            ConfigError::ReadError(ref e) => Some(e),
            ConfigError::AddrError(ref e) => Some(e),
        }
    }
}

impl From<io::Error> for ConfigError {
    fn from(err: io::Error) -> ConfigError {
        ConfigError::ReadError(err)
    }
}

impl From<toml::de::Error> for ConfigError {
    fn from(err: toml::de::Error) -> ConfigError {
        ConfigError::TomlError(err)
    }
}

impl From<AddrParseError> for ConfigError {
    fn from(err: AddrParseError) -> ConfigError {
        ConfigError::AddrError(err)
    }
}
