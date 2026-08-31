//! BLAKE2b header support for the BIP-110 chain.
//!
//! The BLAKE2b hardfork activated at mainnet height 961,640. Blocks from that
//! height carry a "header v2": the classic 80-byte header with the top version
//! bit set, followed by 84 bytes of extra fields, and a block hash built from
//! BLAKE2b rather than SHA256d.

use std::convert::TryInto;

use blake2::digest::consts::U32;
use blake2::{Blake2b, Digest};
use corepc_client::bitcoin::blockdata::block::{Header, Version};
use corepc_client::bitcoin::hashes::{sha256, Hash};
use corepc_client::bitcoin::{BlockHash, CompactTarget, TxMerkleNode};

/// Top bit of the on-wire version field, flagging a v2 header.
pub const VERSION_HEADER_V2_FLAG: u32 = 0x8000_0000;
/// Set when `nTime` is carried as `time_on_wire + time_offset`.
const FLAG_USE_TIME_OFFSET: u8 = 4;

pub const HEADER_V1_SIZE: usize = 80;
pub const HEADER_V2_EXTRA_SIZE: usize = 84;
pub const HEADER_V2_SIZE: usize = HEADER_V1_SIZE + HEADER_V2_EXTRA_SIZE;

/// The extra fields a v2 header carries after the classic 80 bytes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HeaderV2 {
    /// The version exactly as it appeared on the wire, v2 flag included. The
    /// block hash commits to this, so it cannot be recovered from `Header`.
    pub complete_version: u32,
    /// `nTime` before the offset is folded in; also what the hash commits to.
    pub time_on_wire: u32,
    pub nonce2: u32,
    pub nonce3: u32,
    pub extranonce: [u8; 16],
    pub time_offset: u32,
    pub txcount: u16,
    pub flags: u8,
    pub xor_key_mask_clear_bits: u8,
    pub xor_key: [u8; 16],
    pub height: i32,
    pub mm_rhs: [u8; 32],
}

#[derive(Debug)]
pub enum ParseError {
    Truncated { need: usize, got: usize },
}

impl std::fmt::Display for ParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        match self {
            ParseError::Truncated { need, got } => {
                write!(f, "truncated header: need {} bytes, got {}", need, got)
            }
        }
    }
}

struct Reader<'a> {
    bytes: &'a [u8],
    pos: usize,
}

impl<'a> Reader<'a> {
    fn take(&mut self, n: usize) -> Result<&'a [u8], ParseError> {
        let end = self.pos + n;
        if end > self.bytes.len() {
            return Err(ParseError::Truncated {
                need: end,
                got: self.bytes.len(),
            });
        }
        let out = &self.bytes[self.pos..end];
        self.pos = end;
        Ok(out)
    }

    fn u32(&mut self) -> Result<u32, ParseError> {
        Ok(u32::from_le_bytes(
            self.take(4)?.try_into().expect("4 bytes"),
        ))
    }

    fn u16(&mut self) -> Result<u16, ParseError> {
        Ok(u16::from_le_bytes(
            self.take(2)?.try_into().expect("2 bytes"),
        ))
    }

    fn u8(&mut self) -> Result<u8, ParseError> {
        Ok(self.take(1)?[0])
    }

    fn array<const N: usize>(&mut self) -> Result<[u8; N], ParseError> {
        Ok(self.take(N)?.try_into().expect("N bytes"))
    }
}

/// Parses one header, returning it and how many bytes it consumed. A v1 header
/// consumes 80 bytes and a v2 header 164, which is why the REST header stream
/// cannot be split on a fixed stride.
pub fn parse_header(bytes: &[u8]) -> Result<(Header, Option<HeaderV2>, usize), ParseError> {
    let mut r = Reader { bytes, pos: 0 };

    let complete_version = r.u32()?;
    let prev_blockhash = BlockHash::from_byte_array(r.array::<32>()?);
    let merkle_root = TxMerkleNode::from_byte_array(r.array::<32>()?);
    let time_on_wire = r.u32()?;
    let bits = CompactTarget::from_consensus(r.u32()?);
    let nonce = r.u32()?;

    let is_v2 = complete_version & VERSION_HEADER_V2_FLAG != 0;
    // Consensus strips the flag before treating the field as a version, so the
    // deployment bits line up with pre-fork blocks.
    let version = Version::from_consensus((complete_version & !VERSION_HEADER_V2_FLAG) as i32);

    if !is_v2 {
        let header = Header {
            version,
            prev_blockhash,
            merkle_root,
            time: time_on_wire,
            bits,
            nonce,
        };
        return Ok((header, None, r.pos));
    }

    let v2 = HeaderV2 {
        complete_version,
        time_on_wire,
        nonce2: r.u32()?,
        nonce3: r.u32()?,
        extranonce: r.array::<16>()?,
        time_offset: r.u32()?,
        txcount: r.u16()?,
        flags: r.u8()?,
        xor_key_mask_clear_bits: r.u8()?,
        xor_key: r.array::<16>()?,
        height: r.u32()? as i32,
        mm_rhs: r.array::<32>()?,
    };

    let time = if v2.flags & FLAG_USE_TIME_OFFSET != 0 {
        time_on_wire.wrapping_add(v2.time_offset)
    } else {
        time_on_wire
    };

    let header = Header {
        version,
        prev_blockhash,
        merkle_root,
        time,
        bits,
        nonce,
    };

    Ok((header, Some(v2), r.pos))
}

/// Parses a concatenated header stream, as returned by `/rest/headers/*.bin`.
pub fn parse_headers(bytes: &[u8]) -> Result<Vec<ParsedHeader>, ParseError> {
    let mut out = Vec::new();
    let mut offset = 0;

    while offset < bytes.len() {
        let (header, v2, used) = parse_header(&bytes[offset..])?;
        out.push(ParsedHeader { header, v2 });
        offset += used;
    }

    Ok(out)
}

fn tagged(tag: &str) -> sha256::HashEngine {
    use corepc_client::bitcoin::hashes::HashEngine;

    let tag_hash = sha256::Hash::hash(tag.as_bytes());
    let mut engine = sha256::Hash::engine();
    engine.input(tag_hash.as_byte_array());
    engine.input(tag_hash.as_byte_array());
    engine
}

fn blake2b256(data: &[u8]) -> [u8; 32] {
    let mut hasher = Blake2b::<U32>::new();
    hasher.update(data);
    hasher.finalize().into()
}

/// The block hash of a v2 header.
///
/// Mirrors `CBlockHeader::GetHash()` in Bitcoin Knots: two tagged SHA256
/// commitments, a BLAKE2b pass over the Sv1 coinbase prefix, a second BLAKE2b
/// over the fields the mining hardware sees, then an XOR mask.
pub fn block_hash_v2(header: &Header, v2: &HeaderV2) -> BlockHash {
    use corepc_client::bitcoin::hashes::HashEngine;

    const ZEROS: [u8; 16] = [0u8; 16];

    let mut e = tagged("Bitcoin block hash PoW XOR key");
    e.input(&v2.xor_key);
    let xor_key_hash = sha256::Hash::from_engine(e);

    let mut xor_key_mask = [0u8; 32];
    if v2.xor_key != ZEROS {
        let mut e = tagged("Bitcoin block hash PoW XOR mask");
        e.input(&v2.xor_key);
        xor_key_mask = sha256::Hash::from_engine(e).to_byte_array();

        let clear_bytes = (v2.xor_key_mask_clear_bits / 8) as usize;
        for byte in xor_key_mask.iter_mut().take(clear_bytes) {
            *byte = 0;
        }
        xor_key_mask[clear_bytes] &= 0xffu8 >> (v2.xor_key_mask_clear_bits % 8);
    }

    // Knots hashes the prevhash in "sane" (big-endian) order.
    let mut prev_sane = header.prev_blockhash.to_byte_array();
    prev_sane.reverse();

    let mut e = tagged("Bitcoin prevblock header, hashed");
    e.input(&prev_sane);
    let mut prev_hidden = sha256::Hash::from_engine(e).to_byte_array();

    let mut h1 = tagged("Bitcoin block header 1");
    h1.input(&v2.complete_version.to_le_bytes());
    h1.input(&prev_sane);
    h1.input(&v2.height.to_le_bytes());
    h1.input(header.merkle_root.as_byte_array());
    h1.input(&v2.time_on_wire.to_le_bytes());
    h1.input(&[0u8]); // reserved for extended 40-bit time
    h1.input(&header.bits.to_consensus().to_le_bytes());
    h1.input(&(v2.txcount as u32).to_le_bytes());
    h1.input(&[v2.flags]);
    h1.input(&[v2.xor_key_mask_clear_bits]);
    h1.input(xor_key_hash.as_byte_array());
    let h1_hash = sha256::Hash::from_engine(h1);

    let mut h2 = tagged("Merge-mining hook");
    h2.input(h1_hash.as_byte_array());
    h2.input(&ZEROS);
    h2.input(&ZEROS);
    h2.input(&v2.mm_rhs);
    let h2_hash = sha256::Hash::from_engine(h2).to_byte_array();

    let mut ss = Vec::with_capacity(52);
    ss.extend_from_slice(&0u32.to_le_bytes());
    ss.extend_from_slice(&h2_hash);
    ss.extend_from_slice(&v2.extranonce);
    let mut hash = blake2b256(&ss);

    let nonce = header.nonce.to_le_bytes();
    let nonce2 = v2.nonce2.to_le_bytes();
    let nonce3 = v2.nonce3.to_le_bytes();
    let time_offset = v2.time_offset.to_le_bytes();

    let mut ss = Vec::new();
    match v2.flags & 3 {
        3 | 2 => {
            if v2.flags & 3 == 3 {
                ss.extend_from_slice(&ZEROS);
                ss.extend_from_slice(&ZEROS);
            }
            ss.extend_from_slice(&ZEROS);
            ss.extend_from_slice(&ZEROS);
            ss.extend_from_slice(&ZEROS);
            ss.extend_from_slice(&h2_hash);
            ss.extend_from_slice(&nonce);
            ss.extend_from_slice(&nonce2);
            ss.extend_from_slice(&time_offset);
            ss.extend_from_slice(&nonce3);
            ss.extend_from_slice(&hash);
        }
        0 => {
            for byte in prev_hidden.iter_mut().take(6) {
                *byte = 0;
            }
            ss.extend_from_slice(&prev_hidden);
            ss.extend_from_slice(&nonce);
            ss.extend_from_slice(&nonce2);
            ss.extend_from_slice(&time_offset);
            ss.extend_from_slice(&nonce3);
            ss.extend_from_slice(&hash);
        }
        _ => {
            ss.extend_from_slice(&nonce);
            ss.extend_from_slice(&nonce2);
            ss.extend_from_slice(&nonce3);
            ss.extend_from_slice(&time_offset);
            ss.extend_from_slice(&hash);
            ss.extend_from_slice(&h2_hash);
        }
    }
    hash = blake2b256(&ss);

    let mut out = [0u8; 32];
    for i in 0..32 {
        out[i] = hash[i] ^ xor_key_mask[i];
    }
    out.reverse();

    BlockHash::from_byte_array(out)
}

/// Serializes a header back to its exact wire form: 80 bytes for SHA256d, 164
/// for BLAKE2b. `consensus::serialize` cannot be used for v2, since it drops
/// the flag and the extra fields, which the block hash commits to.
pub fn serialize(header: &Header, v2: Option<&HeaderV2>) -> Vec<u8> {
    let (complete_version, time_on_wire) = match v2 {
        Some(v2) => (v2.complete_version, v2.time_on_wire),
        None => (header.version.to_consensus() as u32, header.time),
    };

    let mut out = Vec::with_capacity(match v2 {
        Some(_) => HEADER_V2_SIZE,
        None => HEADER_V1_SIZE,
    });
    out.extend_from_slice(&complete_version.to_le_bytes());
    out.extend_from_slice(&header.prev_blockhash.to_byte_array());
    out.extend_from_slice(header.merkle_root.as_byte_array());
    out.extend_from_slice(&time_on_wire.to_le_bytes());
    out.extend_from_slice(&header.bits.to_consensus().to_le_bytes());
    out.extend_from_slice(&header.nonce.to_le_bytes());

    if let Some(v2) = v2 {
        out.extend_from_slice(&v2.nonce2.to_le_bytes());
        out.extend_from_slice(&v2.nonce3.to_le_bytes());
        out.extend_from_slice(&v2.extranonce);
        out.extend_from_slice(&v2.time_offset.to_le_bytes());
        out.extend_from_slice(&v2.txcount.to_le_bytes());
        out.push(v2.flags);
        out.push(v2.xor_key_mask_clear_bits);
        out.extend_from_slice(&v2.xor_key);
        out.extend_from_slice(&v2.height.to_le_bytes());
        out.extend_from_slice(&v2.mm_rhs);
    }

    out
}

/// A header together with its v2 fields, if it has any. Backends that only
/// ever see SHA256d blocks can build one with `Header::into()`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedHeader {
    pub header: Header,
    pub v2: Option<HeaderV2>,
}

impl ParsedHeader {
    /// The block hash under whichever proof-of-work this block was mined with.
    /// This shadows `Header::block_hash()`, which is SHA256d-only and therefore
    /// wrong for v2 headers, so existing call sites stay correct as they are.
    pub fn block_hash(&self) -> BlockHash {
        block_hash(&self.header, self.v2.as_ref())
    }
}

/// Lets a `ParsedHeader` be read exactly like the `Header` it wraps.
impl std::ops::Deref for ParsedHeader {
    type Target = Header;

    fn deref(&self) -> &Header {
        &self.header
    }
}

impl From<Header> for ParsedHeader {
    fn from(header: Header) -> Self {
        ParsedHeader { header, v2: None }
    }
}

/// The block hash of a header, whichever proof-of-work it was mined under.
pub fn block_hash(header: &Header, v2: Option<&HeaderV2>) -> BlockHash {
    match v2 {
        Some(v2) => block_hash_v2(header, v2),
        None => header.block_hash(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Real mainnet headers: the last SHA256d block and three BLAKE2b blocks.
    const HEADERS: [(&str, &str); 4] = [
    // mainnet block 961,639
    ("10000a205fca17a6566978303e989d163e1aa9dc6715eef5542e0000000000000000000080fe52c98f1c1f8484213dff5a88315f7c334d0705f7d79579b289781868c0dff5c1916a3d350217510c87ed", "00000000000000000001bbc439e13f749dca850d32c7a2834165338713027e65"),
    // mainnet block 961,640
    ("000000a0657e02138733654183a2c7320d85ca9d743fe139c4bb01000000000000000000c137a8515a0f6b3aaf6049cc7611787c022ad523d51094be0a0363d0dc0bc7684dca936a4f8d001a5671798c84daeb494dca936a00000000b1ccf00d0300000000000000000000001e0300000000000000000000000000000000000068ac0e000000000000000000000000000000000000000000000000000000000000000000", "0000000000000050c1e5f69672f459293be14f46e5a494e7a8c8541396f18eeb"),
    // mainnet block 962,000
    ("000000a0f71a4ef4bee6b127cc3702bd08b4f58c8f8e8c1ceff76d6b62000000000000006eb7af7c0e41375c874f4875d8c6a7c569db2c46815bc3904e94a866234f3983d3d7946a4f8d001aa9fd379f4da6971bd3d7946a00000000b10cf00d030000000000000000000000c403000000000000000000000000000000000000d0ad0e000000000000000000000000000000000000000000000000000000000000000000", "000000000000003a65dc8991928c4d2865a424ac7ab5de981cc959828784ea0e"),
    // mainnet block 962,400
    ("000000a0ea0603d0c8fc0f3a5faf1ff68d762a9d891da3eeefd708b66e00000000000000f79a91deb8b15796b494fe57f2da2856eba100822b3564a3cd1b9386fb0acb5ed6a5956a4f8d001af4b630187635ee04d6a5956a00000000b14cf00d030000000000000000000000480300000000000000000000000000000000000060af0e000000000000000000000000000000000000000000000000000000000000000000", "000000000000005d5e0941a3c747bfd24e8f1abe945f04d2243a3cb6128b76f5"),
    ];

    fn parse(hex_str: &str) -> (Header, Option<HeaderV2>, usize) {
        parse_header(&hex::decode(hex_str).expect("valid hex")).expect("parses")
    }

    #[test]
    fn parses_v1_and_v2_sizes() {
        let (_, v2, used) = parse(HEADERS[0].0);
        assert!(v2.is_none(), "961,639 predates the fork");
        assert_eq!(used, HEADER_V1_SIZE);

        for (hex_str, _) in HEADERS.iter().skip(1) {
            let (_, v2, used) = parse(hex_str);
            assert!(v2.is_some(), "post-fork blocks carry a v2 header");
            assert_eq!(used, HEADER_V2_SIZE);
        }
    }

    #[test]
    fn computes_block_hashes() {
        for (hex_str, expected) in HEADERS.iter() {
            let (header, v2, _) = parse(hex_str);
            assert_eq!(block_hash(&header, v2.as_ref()).to_string(), *expected);
        }
    }

    #[test]
    fn strips_the_v2_flag_from_the_version() {
        let (header, v2, _) = parse(HEADERS[1].0);
        let v2 = v2.expect("v2 header");

        assert_ne!(v2.complete_version & VERSION_HEADER_V2_FLAG, 0);
        // Consensus sees 0x20000000, so BIP-9 bit reads still work.
        assert_eq!(header.version.to_consensus(), 0x2000_0000);
    }

    #[test]
    fn parses_a_mixed_header_stream() {
        let mut stream = Vec::new();
        for (hex_str, _) in HEADERS.iter() {
            stream.extend_from_slice(&hex::decode(hex_str).expect("valid hex"));
        }

        let headers = parse_headers(&stream).expect("stream parses");
        assert_eq!(headers.len(), HEADERS.len());

        for (parsed, (_, expected)) in headers.iter().zip(HEADERS.iter()) {
            assert_eq!(parsed.block_hash().to_string(), *expected);
        }
    }

    /// Real testnet4 headers straight off a Bitcoin Knots v29.4.1 node, spanning
    /// its own BLAKE2b activation at height 150,308.
    const TESTNET4_HEADERS: [(&str, &str); 4] = [
        // testnet4 block 150,307
        ("00e0572cb60133b39f77761d271c389f3de1cd9079db9fd46df7eb6b8d8a230000000000b78b78c0f2dacc8b911d5c3b439de9a03148729bb0a482205f2a5493f05817f94378936affff001d2200f068", "000000000017ec2251d81c8d2ca401c713e98e85196c7f660a4088a7ca57b1cc"),
        // testnet4 block 150,308
        ("000000a0ccb157caa788400a667f6c19858ee913c701a42c8d1cd85122ec17000000000043d2e57990429ae581621ce01aa5fbf5e4c2723996be18660a4930b91e96d6c871b4946affff001dce0ac801d123881f71b4946a00000000b10cf00d0100000000000000000000008e00000000000000000000000000000000000000244b02000000000000000000000000000000000000000000000000000000000000000000", "000000000000b9d1b7e1bb0e77215ee92c6ef7ec8f4473e23908380649e779b6"),
        // testnet4 block 150,309
        ("000000a0b679e74906380839e273448fecf76e2ce95e21770ebbe1b7d1b9000000000000b89a9a63a3e422bc9e7a866a2979dcdb4f4a63df0958fa798d3645fbfb1cac1487b9946affff001d73541009b64dc60f87b9946a00000000b10cf00d0100000000000000000000002200000000000000000000000000000000000000254b02000000000000000000000000000000000000000000000000000000000000000000", "00000000000096ebd9ecd086095f024d8eb4d1cdb9d8b124dc0e610a4f24f858"),
        // testnet4 block 150,310
        ("000000a058f8244f0a610edc24b1d8b9cdd1b48e4d025f0986d0ecd9eb96000000000000ff7848f442580dfb1d962ded6ec4bb676e3f4e434b1d5d6d0bb74a9d444f4c3d47c3946affff001d01540859a6125c2b47c3946a00000000b10cf00d0100000000000000000000000400000000000000000000000000000000000000264b02000000000000000000000000000000000000000000000000000000000000000000", "0000000000013427d1a6ec47d2fab6ebdc2e06a02b4bdd6e5d92d2ad2e897111"),
    ];

    #[test]
    fn matches_a_live_knots_node_across_activation() {
        let (_, v2, used) = parse(TESTNET4_HEADERS[0].0);
        assert!(v2.is_none(), "150,307 is the last SHA256d block");
        assert_eq!(used, HEADER_V1_SIZE);

        for (hex_str, expected) in TESTNET4_HEADERS.iter() {
            let (header, v2, _) = parse(hex_str);
            assert_eq!(block_hash(&header, v2.as_ref()).to_string(), *expected);
        }

        for (hex_str, _) in TESTNET4_HEADERS.iter().skip(1) {
            let (_, v2, used) = parse(hex_str);
            assert_eq!(used, HEADER_V2_SIZE);
            assert!(v2.is_some());
        }
    }

    #[test]
    fn round_trips_through_serialize() {
        for (hex_str, expected) in HEADERS.iter() {
            let (header, v2, _) = parse(hex_str);
            let bytes = serialize(&header, v2.as_ref());

            assert_eq!(hex::encode(&bytes), *hex_str);

            let (header, v2, _) = parse_header(&bytes).expect("re-parses");
            assert_eq!(block_hash(&header, v2.as_ref()).to_string(), *expected);
        }
    }

    #[test]
    fn rejects_a_truncated_header() {
        let bytes = hex::decode(HEADERS[1].0).expect("valid hex");
        assert!(parse_header(&bytes[..HEADER_V1_SIZE + 4]).is_err());
    }
}
