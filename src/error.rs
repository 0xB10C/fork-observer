use std::fmt;

#[derive(Debug)]
pub enum FetchError {
    TokioJoin(tokio::task::JoinError),
    BitcoinCoreRPC(bitcoincore_rpc::Error),
    BitcoinCoreREST(String),
    MinReq(minreq::Error),
}

impl fmt::Display for FetchError {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            FetchError::TokioJoin(e) => write!(f, "TokioJoin Error: {:?}", e),
            FetchError::BitcoinCoreRPC(e) => write!(f, "Bitcoin Core RPC Error: {}", e),
            FetchError::BitcoinCoreREST(e) => write!(f, "Bitcoin Core REST Error: {}", e),
            FetchError::MinReq(e) => write!(f, "MinReq HTTP GET request error: {:?}", e),
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