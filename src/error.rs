use std::fmt;
use std::net::AddrParseError;
use std::{error, io};

use bitcoincore_rpc::bitcoin;
use bitcoincore_rpc::bitcoin::hashes::hex::parse::HexToArrayError;

#[derive(Debug)]
pub enum FetchError {
    TokioJoin(tokio::task::JoinError),
    BitcoinCoreRPC(bitcoincore_rpc::Error),
    BitcoinCoreREST(String),
    BtcdRPC(JsonRPCError),
    MinReq(minreq::Error),
    DataError(String),
}

impl fmt::Display for FetchError {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            FetchError::TokioJoin(e) => write!(f, "TokioJoin Error: {:?}", e),
            FetchError::BitcoinCoreRPC(e) => write!(f, "Bitcoin Core RPC Error: {}", e),
            FetchError::BtcdRPC(e) => write!(f, "btcd Error: {}", e),
            FetchError::BitcoinCoreREST(e) => write!(f, "Bitcoin Core REST Error: {}", e),
            FetchError::MinReq(e) => write!(f, "MinReq HTTP GET request error: {:?}", e),
            FetchError::DataError(e) => write!(f, "Invalid data response error {}", e),
        }
    }
}

impl error::Error for FetchError {
    fn source(&self) -> Option<&(dyn error::Error + 'static)> {
        match *self {
            FetchError::TokioJoin(ref e) => Some(e),
            FetchError::BitcoinCoreRPC(ref e) => Some(e),
            FetchError::BtcdRPC(ref e) => Some(e),
            FetchError::BitcoinCoreREST(_) => None,
            FetchError::MinReq(ref e) => Some(e),
            FetchError::DataError(_) => None,
        }
    }
}

impl From<minreq::Error> for FetchError {
    fn from(e: minreq::Error) -> Self {
        FetchError::MinReq(e)
    }
}

impl From<tokio::task::JoinError> for FetchError {
    fn from(e: tokio::task::JoinError) -> Self {
        FetchError::TokioJoin(e)
    }
}

impl From<bitcoincore_rpc::Error> for FetchError {
    fn from(e: bitcoincore_rpc::Error) -> Self {
        FetchError::BitcoinCoreRPC(e)
    }
}

#[derive(Debug)]
pub enum DbError {
    Rusqlite(rusqlite::Error),
    DecodeHex(hex::FromHexError),
    BitcoinDeserialize(bitcoin::consensus::encode::Error),
}

impl fmt::Display for DbError {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            DbError::DecodeHex(e) => write!(f, "hex decoding error: {:?}", e),
            DbError::BitcoinDeserialize(e) => write!(f, "Bitcoin deserialization error: {:?}", e),
            DbError::Rusqlite(e) => write!(f, "Rusqlite SQL error: {:?}", e),
        }
    }
}

impl error::Error for DbError {
    fn source(&self) -> Option<&(dyn error::Error + 'static)> {
        match *self {
            DbError::DecodeHex(ref e) => Some(e),
            DbError::BitcoinDeserialize(ref e) => Some(e),
            DbError::Rusqlite(ref e) => Some(e),
        }
    }
}

impl From<rusqlite::Error> for DbError {
    fn from(e: rusqlite::Error) -> Self {
        DbError::Rusqlite(e)
    }
}

impl From<hex::FromHexError> for DbError {
    fn from(e: hex::FromHexError) -> Self {
        DbError::DecodeHex(e)
    }
}

impl From<bitcoin::consensus::encode::Error> for DbError {
    fn from(e: bitcoin::consensus::encode::Error) -> Self {
        DbError::BitcoinDeserialize(e)
    }
}

#[derive(Debug)]
pub enum ConfigError {
    CookieFileDoesNotExist,
    NoBitcoinCoreRpcAuth,
    NoBtcdRpcAuth,
    NoNetworks,
    UnknownImplementation,
    DuplicateNodeId,
    DuplicateNetworkId,
    TomlError(toml::de::Error),
    ReadError(io::Error),
    AddrError(AddrParseError),
}

impl fmt::Display for ConfigError {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            ConfigError::CookieFileDoesNotExist => write!(f, "the .cookie file path set via rpc_cookie_file does not exist"),
            ConfigError::NoBitcoinCoreRpcAuth => write!(f, "please specify a Bitcoin Core RPC .cookie file (option: 'rpc_cookie_file') or a rpc_user and rpc_password"),
            ConfigError::NoBtcdRpcAuth => write!(f, "no values for rpc_user and rpc_password"),
            ConfigError::NoNetworks => write!(f, "no networks defined in the configuration"),
            ConfigError::UnknownImplementation => write!(f, "the node implementation defined in the config is not supported"),
            ConfigError::DuplicateNodeId => write!(f, "a node id has been used multiple times in the same network"),
            ConfigError::DuplicateNetworkId => write!(f, "a network id has been used multiple times"),
            ConfigError::TomlError(e) => write!(f, "the TOML in the configuration file could not be parsed: {}", e),
            ConfigError::ReadError(e) => write!(f, "the configuration file could not be read: {}", e),
            ConfigError::AddrError(e) => write!(f, "the address could not be parsed: {}", e),
        }
    }
}

impl error::Error for ConfigError {
    fn source(&self) -> Option<&(dyn error::Error + 'static)> {
        match *self {
            ConfigError::NoBitcoinCoreRpcAuth => None,
            ConfigError::NoBtcdRpcAuth => None,
            ConfigError::CookieFileDoesNotExist => None,
            ConfigError::NoNetworks => None,
            ConfigError::UnknownImplementation => None,
            ConfigError::TomlError(ref e) => Some(e),
            ConfigError::ReadError(ref e) => Some(e),
            ConfigError::AddrError(ref e) => Some(e),
            ConfigError::DuplicateNodeId => None,
            ConfigError::DuplicateNetworkId => None,
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

#[derive(Debug)]
pub enum MainError {
    Db(DbError),
    Fetch(FetchError),
    Config(ConfigError),
}

impl fmt::Display for MainError {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            MainError::Db(e) => write!(f, "database error: {:?}", e),
            MainError::Fetch(e) => write!(f, "fetch error: {:?}", e),
            MainError::Config(e) => write!(f, "config error: {:?}", e),
        }
    }
}

impl error::Error for MainError {
    fn source(&self) -> Option<&(dyn error::Error + 'static)> {
        match *self {
            MainError::Db(ref e) => Some(e),
            MainError::Fetch(ref e) => Some(e),
            MainError::Config(ref e) => Some(e),
        }
    }
}

impl From<DbError> for MainError {
    fn from(e: DbError) -> Self {
        MainError::Db(e)
    }
}

impl From<FetchError> for MainError {
    fn from(e: FetchError) -> Self {
        MainError::Fetch(e)
    }
}

impl From<ConfigError> for MainError {
    fn from(e: ConfigError) -> Self {
        MainError::Config(e)
    }
}

#[derive(Debug)]
pub enum JsonRPCError {
    Http(String),
    JsonRpc(String),
    RpcUnexpectedResponseContents(String),
    MinReq(minreq::Error),
    FromHex(hex::FromHexError),
    BitcoinFromHex(HexToArrayError),
    BitcoinDeserializeError(bitcoin::consensus::encode::Error),
    NotImplemented,
}

impl fmt::Display for JsonRPCError {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            JsonRPCError::MinReq(e) => write!(f, "minreq error: {:?}", e),
            JsonRPCError::Http(s) => write!(f, "HTTP error: {}", s),
            JsonRPCError::JsonRpc(s) => write!(f, "json-rpc error: {}", s),
            JsonRPCError::RpcUnexpectedResponseContents(s) => {
                write!(f, "unexpected contents in RPC response: {}", s)
            }
            JsonRPCError::BitcoinDeserializeError(e) => {
                write!(f, "bitcoin deserialize error: {}", e)
            }
            JsonRPCError::FromHex(e) => write!(f, "from-hex error: {}", e),
            JsonRPCError::BitcoinFromHex(e) => write!(f, "bitcoin from-hex error: {}", e),
            JsonRPCError::NotImplemented => write!(f, "NotImplemented",),
        }
    }
}

impl error::Error for JsonRPCError {
    fn source(&self) -> Option<&(dyn error::Error + 'static)> {
        match *self {
            JsonRPCError::Http(_) => None,
            JsonRPCError::JsonRpc(_) => None,
            JsonRPCError::RpcUnexpectedResponseContents(_) => None,
            JsonRPCError::NotImplemented => None,
            JsonRPCError::MinReq(ref e) => Some(e),
            JsonRPCError::FromHex(ref e) => Some(e),
            JsonRPCError::BitcoinFromHex(ref e) => Some(e),
            JsonRPCError::BitcoinDeserializeError(ref e) => Some(e),
        }
    }
}

impl From<minreq::Error> for JsonRPCError {
    fn from(e: minreq::Error) -> Self {
        JsonRPCError::MinReq(e)
    }
}

impl From<hex::FromHexError> for JsonRPCError {
    fn from(e: hex::FromHexError) -> Self {
        JsonRPCError::FromHex(e)
    }
}

impl From<bitcoin::consensus::encode::Error> for JsonRPCError {
    fn from(e: bitcoin::consensus::encode::Error) -> Self {
        JsonRPCError::BitcoinDeserializeError(e)
    }
}

impl From<HexToArrayError> for JsonRPCError {
    fn from(e: HexToArrayError) -> Self {
        JsonRPCError::BitcoinFromHex(e)
    }
}
