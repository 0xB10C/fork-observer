//! Synthetic headers and blocks for the backend tests.
//!
//! They are structurally valid but carry no valid proof of work, which is
//! enough: the backends deserialize and hash what they are served, they don't
//! validate it.

use corepc_client::bitcoin::blockdata::block::{Header, Version};
use corepc_client::bitcoin::hashes::Hash as _;
use corepc_client::bitcoin::{
    absolute, transaction, Amount, Block, BlockHash, CompactTarget, OutPoint, ScriptBuf, Sequence,
    Transaction, TxIn, TxMerkleNode, TxOut, Witness,
};

/// A header building on `prev`. The nonce gives headers that share a parent
/// distinct block hashes.
pub fn header(prev: BlockHash, nonce: u32) -> Header {
    Header {
        version: Version::from_consensus(0x2000_0000),
        prev_blockhash: prev,
        merkle_root: TxMerkleNode::all_zeros(),
        time: 1_600_000_000,
        bits: CompactTarget::from_consensus(0x207f_ffff),
        nonce,
    }
}

/// A chain of `count` headers, the first one building on the zero hash.
pub fn chain(count: u32) -> Vec<Header> {
    let mut headers: Vec<Header> = Vec::new();
    let mut prev = BlockHash::all_zeros();
    for nonce in 0..count {
        let next = header(prev, nonce);
        prev = next.block_hash();
        headers.push(next);
    }
    headers
}

/// A block with a single (coinbase) transaction and a matching merkle root.
pub fn block_with_a_coinbase() -> Block {
    let coinbase = Transaction {
        version: transaction::Version::ONE,
        lock_time: absolute::LockTime::ZERO,
        input: vec![TxIn {
            previous_output: OutPoint::null(),
            script_sig: ScriptBuf::from_bytes(vec![0x03, 0x01, 0x02, 0x03]),
            sequence: Sequence::MAX,
            witness: Witness::new(),
        }],
        output: vec![TxOut {
            value: Amount::from_sat(50 * 100_000_000),
            script_pubkey: ScriptBuf::new(),
        }],
    };
    let mut block = Block {
        header: header(BlockHash::all_zeros(), 42),
        txdata: vec![coinbase],
    };
    block.header.merkle_root = block.compute_merkle_root().expect("a merkle root");
    block
}
