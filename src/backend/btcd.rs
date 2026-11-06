use super::{Capabilities, HeaderFetchType, Node, NodeInfo};
use crate::error::{FetchError, JsonRPCError};
use crate::types::ChainTip;
use async_trait::async_trait;
use corepc_client::bitcoin::blockdata::block::Header;
use corepc_client::bitcoin::{BlockHash, Transaction};

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
            partial_nonactive_headers: false,
        }
    }

    fn rpc_url(&self) -> String {
        self.rpc_url.clone()
    }

    async fn version(&self) -> Result<String, FetchError> {
        Err(FetchError::BtcdRPC(JsonRPCError::NotImplemented))
    }

    async fn block_header_hash(&self, hash: &BlockHash) -> Result<Header, FetchError> {
        let url = format!("{}/", self.rpc_url);
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
        _start_height: u64,
        _count: u64,
    ) -> Result<Vec<Header>, FetchError> {
        assert!(!self.capabilities().batch_header_fetch);
        Err(FetchError::DataError(
            "batch header fetch not implemented".to_string(),
        ))
    }

    async fn coinbase(&self, hash: &BlockHash, _height: u64) -> Result<Transaction, FetchError> {
        let url = format!("{}/", self.rpc_url);
        match crate::jsonrpc::btcd_block(
            url,
            self.rpc_user.clone(),
            self.rpc_password.clone(),
            hash.to_string(),
        ) {
            Ok(block) => block.txdata.first().cloned().ok_or_else(|| {
                FetchError::DataError(format!("block {} has no transactions", hash))
            }),
            Err(error) => Err(FetchError::BtcdRPC(error)),
        }
    }

    async fn block(&self, hash: &BlockHash) -> Result<Vec<u8>, FetchError> {
        let url = format!("{}/", self.rpc_url);
        match crate::jsonrpc::btcd_block(
            url,
            self.rpc_user.clone(),
            self.rpc_password.clone(),
            hash.to_string(),
        ) {
            Ok(block) => Ok(corepc_client::bitcoin::consensus::encode::serialize(&block)),
            Err(error) => Err(FetchError::BtcdRPC(error)),
        }
    }

    async fn block_hash(&self, height: u64) -> Result<BlockHash, FetchError> {
        let url = format!("{}/", self.rpc_url);
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
        let url = format!("{}/", self.rpc_url);
        match crate::jsonrpc::btcd_chaintips(url, self.rpc_user.clone(), self.rpc_password.clone())
        {
            Ok(tips) => Ok(tips),
            Err(error) => Err(FetchError::BtcdRPC(error)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::test_chain;
    use super::super::test_server::{serve, Request, Response};
    use super::*;
    use corepc_client::bitcoin::consensus::serialize;
    use corepc_client::bitcoin::hashes::Hash as _;
    use corepc_client::bitcoin::Block;

    fn node_for(rpc_url: &str) -> BtcdNode {
        BtcdNode::new(
            NodeInfo {
                id: 2,
                name: "btcd".to_string(),
                description: String::new(),
                implementation: "btcd".to_string(),
            },
            rpc_url.to_string(),
            "user".to_string(),
            "password".to_string(),
        )
    }

    // The method a JSON-RPC request asks for.
    fn method_of(request: &Request) -> String {
        serde_json::from_str::<serde_json::Value>(&request.body).expect("a JSON-RPC request")
            ["method"]
            .as_str()
            .expect("a method")
            .to_string()
    }

    // The envelope btcd answers with. `result` is the JSON of the result.
    fn rpc_response(result: &str) -> Response {
        Response::ok(&format!(
            r#"{{"jsonrpc":"1.0","id":45324,"result":{},"error":null}}"#,
            result
        ))
    }

    #[tokio::test]
    async fn the_chain_tips_come_from_getchaintips() {
        let server = serve(|request: &Request| match method_of(request).as_str() {
            "getchaintips" => rpc_response(
                r#"[{"height":100,"hash":"00000000000000000000c4dd83a223d8a32c9904c787948fd42c88c30739553a","branchlen":0,"status":"active"},
                    {"height":99,"hash":"00000000000000000001e9c1b31d923680b0472cc3d8e301210dfca4631e2996","branchlen":1,"status":"valid-fork"}]"#,
            ),
            _ => Response::status(500),
        });
        let node = node_for(&server.url);

        let tips = node.tips().await.expect("the tips");

        assert_eq!(tips.len(), 2);
        assert_eq!(tips[0].height, 100);
        assert_eq!(tips[0].status, crate::types::ChainTipStatus::Active);
        assert_eq!(tips[1].status, crate::types::ChainTipStatus::ValidFork);
        assert_eq!(server.paths(), ["/"]);
    }

    #[tokio::test]
    async fn headers_hashes_and_blocks_are_fetched_by_rpc() {
        let block = test_chain::block_with_a_coinbase();
        let header_hex = hex::encode(serialize(&block.header));
        let block_hex = hex::encode(serialize(&block));
        let hash = block.block_hash();
        let server = serve(move |request: &Request| match method_of(request).as_str() {
            "getblockheader" => rpc_response(&format!(r#""{}""#, header_hex)),
            "getblockhash" => rpc_response(&format!(r#""{}""#, hash)),
            "getblock" => rpc_response(&format!(r#""{}""#, block_hex)),
            _ => Response::status(500),
        });
        let node = node_for(&server.url);

        assert_eq!(
            node.block_header_hash(&hash).await.expect("the header"),
            block.header
        );
        assert_eq!(node.block_hash(100).await.expect("the hash"), hash);
        assert_eq!(
            node.block(&hash).await.expect("the block"),
            serialize(&block)
        );
        assert_eq!(
            node.coinbase(&hash, 100).await.expect("the coinbase"),
            block.txdata[0]
        );
    }

    #[tokio::test]
    async fn a_header_or_a_hash_of_the_wrong_length_is_refused() {
        // A header and a hash that are hex, but not 80 and 32 bytes of it.
        let server = serve(|request: &Request| match method_of(request).as_str() {
            "getblockheader" => rpc_response(r#""00112233""#),
            "getblockhash" => rpc_response(r#""00112233""#),
            _ => Response::status(500),
        });
        let node = node_for(&server.url);

        assert!(matches!(
            node.block_header_hash(&BlockHash::all_zeros()).await,
            Err(FetchError::BtcdRPC(
                JsonRPCError::RpcUnexpectedResponseContents(_)
            ))
        ));
        assert!(matches!(
            node.block_hash(100).await,
            Err(FetchError::BtcdRPC(
                JsonRPCError::RpcUnexpectedResponseContents(_)
            ))
        ));
    }

    #[tokio::test]
    async fn an_rpc_error_an_empty_result_or_a_failed_request_is_an_error() {
        let server = serve(|request: &Request| match method_of(request).as_str() {
            "getblockheader" => Response::ok(
                r#"{"jsonrpc":"1.0","id":45324,"result":null,"error":{"code":-8,"message":"nope"}}"#,
            ),
            "getchaintips" => rpc_response("null"),
            _ => Response::status(500),
        });
        let node = node_for(&server.url);

        // The error the node reported is carried in the message, code and all.
        let error = node
            .block_header_hash(&BlockHash::all_zeros())
            .await
            .expect_err("an error");
        assert!(matches!(
            error,
            FetchError::BtcdRPC(JsonRPCError::JsonRpc(_))
        ));
        let message = error.to_string();
        assert!(
            message.contains("code=-8"),
            "unexpected message: {}",
            message
        );
        assert!(message.contains("nope"), "unexpected message: {}", message);
        assert!(matches!(
            node.tips().await,
            Err(FetchError::BtcdRPC(JsonRPCError::JsonRpc(_)))
        ));
        // getblock isn't answered with a 200 here.
        assert!(matches!(
            node.block(&BlockHash::all_zeros()).await,
            Err(FetchError::BtcdRPC(JsonRPCError::Http(_)))
        ));
    }

    #[tokio::test]
    async fn btcd_has_no_version_no_batches_and_no_headers_by_height() {
        // Nothing here makes a request.
        let node = node_for("http://127.0.0.1:1");

        assert!(matches!(
            node.version().await,
            Err(FetchError::BtcdRPC(JsonRPCError::NotImplemented))
        ));
        assert!(matches!(
            node.block_header_height(1).await,
            Err(FetchError::DataError(_))
        ));
        assert!(matches!(
            node.batch_header_fetch(0, 10).await,
            Err(FetchError::DataError(_))
        ));
        assert_eq!(node.rpc_url(), "http://127.0.0.1:1");
        assert_eq!(node.info().id, 2);
        assert_eq!(node.capabilities().header_fetch_type, HeaderFetchType::Hash);
        assert!(!node.capabilities().batch_header_fetch);
    }

    #[tokio::test]
    async fn a_block_without_transactions_has_no_coinbase() {
        // A block with an empty transaction list: not something a node should
        // serve, but it shouldn't take the node's task down either.
        let block = Block {
            header: test_chain::block_with_a_coinbase().header,
            txdata: vec![],
        };
        let block_hex = hex::encode(serialize(&block));
        let server = serve(move |request: &Request| match method_of(request).as_str() {
            "getblock" => rpc_response(&format!(r#""{}""#, block_hex)),
            _ => Response::status(500),
        });
        let node = node_for(&server.url);

        assert!(matches!(
            node.coinbase(&BlockHash::all_zeros(), 100).await,
            Err(FetchError::DataError(_))
        ));
    }
}
