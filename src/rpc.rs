use std::cmp::max;

use crate::error::FetchError;

use bitcoincore_rpc::bitcoin;
use bitcoincore_rpc::bitcoin::BlockHash;
use bitcoincore_rpc::json::{GetChainTipsResult, GetChainTipsResultStatus};
use bitcoincore_rpc::RpcApi;

use log::{debug, warn};

use tokio::task;

use crate::types::{HeaderInfo, Rpc, Tree};

async fn get_new_nonactive_headers(
    tips: &GetChainTipsResult,
    tree: &Tree,
    rpc: Rpc,
    min_fork_height: u64,
) -> Result<Vec<HeaderInfo>, FetchError> {
    let mut new_headers: Vec<HeaderInfo> = Vec::new();

    for inactive_tip in tips
        .iter()
        .filter(|tip| tip.height - tip.branch_length as u64 > min_fork_height)
        .filter(|tip| tip.status != GetChainTipsResultStatus::Active)
    {
        let mut next_header = inactive_tip.hash;
        for i in 0..=inactive_tip.branch_length {
            {
                let tree_locked = tree.lock().await;
                if tree_locked.1.contains_key(&inactive_tip.hash) {
                    break;
                }
            }

            let height = inactive_tip.height - i as u64;
            debug!(
                "loading non-active-chain header: hash={}, height={}",
                next_header.to_string(),
                height
            );

            let header = rpc.get_block_header(&next_header)?;

            new_headers.push(HeaderInfo { height, header });
            next_header = header.prev_blockhash;
        }
    }

    Ok(new_headers)
}

pub async fn get_new_headers(
    tips: &GetChainTipsResult,
    tree: &Tree,
    rpc: Rpc,
    rest_url: String,
    use_rest: bool,
    min_fork_height: u64,
) -> Result<Vec<HeaderInfo>, FetchError> {
    let mut new_headers: Vec<HeaderInfo> = Vec::new();
    let mut active_new_headers: Vec<HeaderInfo> = get_new_active_headers(
        tips,
        rest_url.clone(),
        tree,
        rpc.clone(),
        use_rest,
        min_fork_height,
    )
    .await?;
    new_headers.append(&mut active_new_headers);
    let mut nonactive_new_headers: Vec<HeaderInfo> =
        get_new_nonactive_headers(tips, tree, rpc.clone(), min_fork_height).await?;
    new_headers.append(&mut nonactive_new_headers);
    Ok(new_headers)
}

async fn get_new_active_headers(
    tips: &GetChainTipsResult,
    rest_url: String,
    tree: &Tree,
    rpc: Rpc,
    use_rest: bool,
    min_fork_height: u64,
) -> Result<Vec<HeaderInfo>, FetchError> {
    let mut new_headers: Vec<HeaderInfo> = Vec::new();
    let first_fork_tip = match tips
        .iter()
        .filter(|tip| tip.height - tip.branch_length as u64 > min_fork_height)
        .min_by_key(|tip| tip.height - tip.branch_length as u64)
    {
        Some(tip) => tip,
        None => {
            warn!("No tip qualifies as first_fork_tip. Is min_fork_height={} reasonable for this network?", min_fork_height);
            return Ok(new_headers);
        }
    };
    let min_height = first_fork_tip.height - first_fork_tip.branch_length as u64;
    let scan_start_height = max(min_height as u64 - 5, 0);

    let current_height: u64;
    {
        let locked_tree = tree.lock().await;
        if locked_tree.0.node_count() == 0 {
            current_height = scan_start_height;
        } else {
            let max_tip_idx = locked_tree
                .0
                .externals(petgraph::Direction::Outgoing)
                .max_by_key(|idx| locked_tree.0[*idx].height)
                .expect("we have at least one node in the tree so we should also have a max height node in the tree");
            current_height = locked_tree.0[max_tip_idx].height;
        }
    }

    let active_tip = match tips
        .iter()
        .filter(|tip| tip.status == GetChainTipsResultStatus::Active)
        .last()
    {
        Some(active_tip) => active_tip,
        None => {
            return Err(FetchError::DataError(String::from(
                "No 'active' chain tip returned",
            )))
        }
    };

    if use_rest {
        let mut headers: Vec<bitcoin::BlockHeader>;
        const STEP_SIZE: u64 = 2000;
        for query_height in (current_height + 1..=active_tip.height).step_by(STEP_SIZE as usize) {
            let header_hash = rpc.get_block_hash(query_height)?;
            {
                let locked_tree = tree.lock().await;
                if locked_tree.1.contains_key(&header_hash) {
                    continue;
                }
            }
            headers =
                get_active_chain_headers_rest(rest_url.clone(), STEP_SIZE, header_hash).await?;
            for height_header_pair in
                (query_height..(query_height + headers.len() as u64)).zip(headers)
            {
                new_headers.push(HeaderInfo {
                    height: height_header_pair.0,
                    header: height_header_pair.1,
                });
            }
        }
    } else {
        for height in current_height + 1..=active_tip.height {
            let header_hash = rpc.get_block_hash(height)?;
            {
                let locked_tree = tree.lock().await;
                if locked_tree.1.contains_key(&header_hash) {
                    continue;
                }
            }
            let header = rpc.get_block_header(&header_hash)?;
            new_headers.push(HeaderInfo { height, header });
        }
    }

    Ok(new_headers)
}

pub async fn get_tips(rpc: Rpc) -> Result<GetChainTipsResult, FetchError> {
    match task::spawn_blocking(move || rpc.get_chain_tips()).await {
        Ok(tips_result) => match tips_result {
            Ok(tips) => Ok(tips),
            Err(e) => Err(e.into()),
        },
        Err(e) => Err(e.into()),
    }
}

pub async fn get_version_info(rpc: Rpc) -> Result<String, FetchError> {
    match task::spawn_blocking(move || rpc.get_network_info()).await {
        Ok(result) => match result {
            Ok(result) => Ok(result.subversion),
            Err(e) => Err(e.into()),
        },
        Err(e) => Err(e.into()),
    }
}

async fn get_active_chain_headers_rest(
    rest_url: String,
    count: u64,
    start: BlockHash,
) -> Result<Vec<bitcoin::BlockHeader>, FetchError> {
    debug!(
        "loading active-chain headers starting from {}",
        start.to_string()
    );

    let url = format!("http://{}/rest/headers/{}/{}.bin", rest_url, count, start);
    let res = minreq::get(url.clone()).with_timeout(8).send()?;

    if res.status_code != 200 {
        return Err(FetchError::BitcoinCoreREST(format!(
            "could not load headers from REST URL ({}): {} {}: {:?}",
            url,
            res.status_code,
            res.reason_phrase,
            res.as_str(),
        )));
    }

    let header_results: Result<
        Vec<bitcoin::BlockHeader>,
        bitcoincore_rpc::bitcoin::consensus::encode::Error,
    > = res
        .as_bytes()
        .chunks(80)
        .map(|hbytes| bitcoin::consensus::deserialize::<bitcoin::BlockHeader>(hbytes))
        .collect();

    let headers = match header_results {
        Ok(headers) => headers,
        Err(e) => {
            return Err(FetchError::BitcoinCoreREST(format!(
                "could not deserialize REST header response: {}",
                e
            )))
        }
    };

    debug!(
        "loaded {} active-chain headers starting from {}",
        headers.len(),
        start.to_string()
    );

    Ok(headers)
}
