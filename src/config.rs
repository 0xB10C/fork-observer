
use std::path::PathBuf;
use std::{env, error, fmt, fs, io};
use std::time::Duration;

use bitcoincore_rpc::Auth;
use serde::{Deserialize};


const ENVVAR_CONFIG_FILE: &str = "CONFIG_FILE";
const DEFAULT_CONFIG: &str = "config.toml";

#[derive(Deserialize)]
struct TomlConfig {
    rpc_host: String,
    rpc_port: u16,
    rpc_cookie_file: Option<PathBuf>,
    rpc_user: Option<String>,
    rpc_password: Option<String>,
    database_path: String,
    www_path: String,
    query_interval: u64,
}

pub struct Config {
    pub rpc_url: String,
    pub rpc_auth: Auth,
    pub database_path: PathBuf,
    pub www_path: PathBuf,
    pub query_interval: Duration,
}

pub fn load_config() -> Result<Config, ConfigError> {
    let config_file_path =
        env::var(ENVVAR_CONFIG_FILE).unwrap_or_else(|_| DEFAULT_CONFIG.to_string());
    println!("Reading configuration file from {}.", config_file_path);
    let config_string = fs::read_to_string(config_file_path)?;
    let config: TomlConfig = toml::from_str(&config_string)?;

    let rpc_auth: Auth;
    if config.rpc_cookie_file.is_some() {
        let rpc_cookie_file = config.rpc_cookie_file.unwrap();

        if !rpc_cookie_file.exists() {
            return Err(ConfigError::CookieFileDoesNotExist);
        }

        rpc_auth = Auth::CookieFile(rpc_cookie_file);
    } else if config.rpc_user.is_some() && config.rpc_password.is_some() {
        rpc_auth = Auth::UserPass(config.rpc_user.unwrap(), config.rpc_password.unwrap());
    } else {
        return Err(ConfigError::NoRpcAuth);
    }

    return Ok(Config {
        rpc_url: format!("{}:{}", config.rpc_host, config.rpc_port.to_string()),
        rpc_auth,
        database_path: PathBuf::from(config.database_path),
        www_path: PathBuf::from(config.www_path),
        query_interval: Duration::from_secs(config.query_interval),
    });
}


#[derive(Debug)]
pub enum ConfigError {
    CookieFileDoesNotExist,
    NoRpcAuth,
    TomlError(toml::de::Error),
    ReadError(io::Error),
}

impl fmt::Display for ConfigError {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            ConfigError::CookieFileDoesNotExist => write!(f, "the .cookie file path set via rpc_cookie_file does not exist"),
            ConfigError::NoRpcAuth => write!(f, "please specify a Bitcoin Core RPC .cookie file (option: 'rpc_cookie_file') or a rpc_user and rpc_password"),
            ConfigError::TomlError(e) => write!(f, "the TOML in the configuration file could not be parsed: {}", e),
            ConfigError::ReadError(e) => write!(f, "the configuration file could not be read: {}", e),
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
