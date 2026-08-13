#![cfg_attr(feature = "strict", deny(warnings))]

//! The fork-observer binary: loads the configuration, spawns a task per node
//! that keeps the cache up to date, and serves the API.

use bitcoin_pool_identification::{default_data, PoolIdentification};
use corepc_client::bitcoin::{BlockHash, Network};
use env_logger::Env;
use log::{debug, error, info, warn};
use petgraph::graph::NodeIndex;
use rusqlite::Connection;
use std::cmp::max;
use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;
use tokio::sync::mpsc::unbounded_channel;
use tokio::sync::{broadcast, Mutex};
use tokio::task;
use tokio::time::{sleep, Duration};

use fork_observer::activity::{self, Activity, ActivityEvent, ActivityEventKind};
use fork_observer::api;
use fork_observer::cache::{
    self, active_tip_heights, insert_new_headers_into_tree, is_node_reachable, load_node_version,
    send_activity, tip_heights, update_cache, update_header_tree_cache, CacheUpdate, MINER_UNKNOWN,
};
use fork_observer::config;
use fork_observer::db;
use fork_observer::error::{DbError, MainError};
use fork_observer::headertree;
use fork_observer::remote_forkobserver;
use fork_observer::types::{Caches, ChainTip, Db, HeaderInfo, NetworkJson, Tree};

async fn startup() -> Result<(config::Config, Db, Option<Activity>), MainError> {
    let config: config::Config = match config::load_config() {
        Ok(config) => {
            info!("Configuration loaded");
            config
        }
        Err(e) => {
            error!("Could not load the configuration: {}", e);
            return Err(e.into());
        }
    };

    let connection = match Connection::open(config.database_path.clone()) {
        Ok(db) => {
            info!("Opened database: {:?}", config.database_path);
            db
        }
        Err(e) => {
            error!(
                "Could not open the database {:?}: {}",
                config.database_path, e
            );
            return Err(DbError::from(e).into());
        }
    };

    let db: Db = Arc::new(Mutex::new(connection));

    match db::setup_db(db.clone()).await {
        Ok(_) => info!("Database setup successful"),
        Err(e) => {
            error!(
                "Could not setup the database {:?}: {}",
                config.database_path, e
            );
            return Err(e.into());
        }
    };

    // The activity log lives in its own database and is only enabled when
    // the configuration has an [activity] section.
    let activity: Option<Activity> = match &config.activity {
        Some(activity_config) => {
            let connection = match Connection::open(activity_config.database_path.clone()) {
                Ok(connection) => {
                    info!(
                        "Opened activity database: {:?}",
                        activity_config.database_path
                    );
                    connection
                }
                Err(e) => {
                    error!(
                        "Could not open the activity database {:?}: {}",
                        activity_config.database_path, e
                    );
                    return Err(DbError::from(e).into());
                }
            };
            let activity = Activity::new(connection);
            match activity.setup().await {
                Ok(_) => info!("Activity database setup successful"),
                Err(e) => {
                    error!(
                        "Could not setup the activity database {:?}: {}",
                        activity_config.database_path, e
                    );
                    return Err(e.into());
                }
            };
            Some(activity)
        }
        None => None,
    };

    Ok((config, db, activity))
}

#[tokio::main]
async fn main() -> Result<(), MainError> {
    env_logger::Builder::from_env(Env::default().default_filter_or("info")).init();
    let (config, db, activity) = startup().await?;

    // A channel to notify about changes via ServerSentEvents to clients.
    let (cache_changed_tx, _) = broadcast::channel(16);
    let cache_changed_tx_warp = cache_changed_tx.clone();
    let network_infos: Vec<NetworkJson> = config.networks.iter().map(NetworkJson::new).collect();
    let db_clone = db.clone();

    // Load every network's header tree and build its cache up front. The set of
    // networks comes from the configuration and doesn't change while we run, so
    // the map of caches can be built once here and then shared without a lock
    // around it - a request for one network never waits on another's writer.
    let mut networks_with_trees: Vec<(config::Network, Tree)> = vec![];
    for network in config.networks.iter().cloned() {
        let tree: Tree = Arc::new(Mutex::new(
            match db::load_treeinfos(db_clone.clone(), network.id).await {
                Ok(tree) => tree,
                Err(e) => {
                    error!(
                        "Could not load tree_infos (headers) from the database {:?}: {}",
                        config.database_path, e
                    );
                    return Err(e.into());
                }
            },
        ));
        networks_with_trees.push((network, tree));
    }
    let caches: Caches = cache::build_caches(&networks_with_trees).await;

    // Activity log: preload the in-memory ring buffers with recent events,
    // spawn the writer task the event generation sites send into, and (when
    // a retention is configured) the archive-then-purge retention task.
    let (activity_tx, activity_rx) = unbounded_channel::<ActivityEvent>();
    if let Some(ref activity) = activity {
        let network_ids: Vec<u32> = config.networks.iter().map(|n| n.id).collect();
        if let Err(e) = activity.preload_cache(&network_ids).await {
            warn!("Could not preload the activity event cache: {}", e);
        }
        task::spawn(activity::run_activity_writer(activity.clone(), activity_rx));

        if let Some(ref activity_config) = config.activity {
            let retentions: Vec<(u32, u64)> = config
                .networks
                .iter()
                .filter_map(|network| {
                    network
                        .activity_retention_days
                        .or(activity_config.retention_days)
                        .map(|days| (network.id, days))
                })
                .collect();
            if !retentions.is_empty() {
                let archive_directory = activity_config
                    .archive_directory
                    .clone()
                    .expect("the config parser rejects a retention without an archive_directory");
                task::spawn(activity::run_retention(
                    activity.clone(),
                    archive_directory,
                    retentions,
                ));
            }
        }
    }

    for (network, tree) in networks_with_trees.into_iter() {
        let (pool_id_tx, mut pool_id_rx) = unbounded_channel::<BlockHash>();

        info!(
            "network '{}' (id={}) has {} nodes and {} remote fork-observer source(s)",
            network.name,
            network.id,
            network.nodes.len(),
            network.remote_forkobservers.len()
        );

        // Lagging/caught-up state of this network's nodes, shared by its node
        // tasks: a stuck node can't observe itself falling behind, so lagging
        // is (re-)evaluated whenever any node of the network updates its tips.
        let lagging_state: Arc<Mutex<BTreeMap<u32, bool>>> = Arc::new(Mutex::new(BTreeMap::new()));
        let network_logs_activity = activity.is_some() && !network.activity_log_node_ids.is_empty();

        for node in network.nodes.iter().cloned() {
            let network = network.clone();
            let query_interval = config.query_interval;
            // Spread the initial query times apart to even out network/CPU load
            // on startup. Afterwards each node waits for its own tip changes (via
            // `wait_for_tip_change`), so the load naturally spreads out.
            let initial_stagger = Duration::from_millis(
                (query_interval.as_millis() / network.nodes.len() as u128) as u64,
            ) + Duration::from_secs((network.id % 10) as u64);
            let db_write = db.clone();
            let tree_clone = tree.clone();
            let caches_clone = caches.clone();
            let cache_changed_tx_cloned = cache_changed_tx.clone();
            let pool_id_tx_clone = pool_id_tx.clone();
            // For the node's own activity events (only when it opted in) and
            // for network-wide lagging evaluation (when any node opted in).
            let node_activity_tx =
                if activity.is_some() && network.activity_log_node_ids.contains(&node.info().id) {
                    Some(activity_tx.clone())
                } else {
                    None
                };
            let lagging_activity_tx = if network_logs_activity {
                Some(activity_tx.clone())
            } else {
                None
            };
            let lagging_state = lagging_state.clone();

            let mut last_tips: Vec<ChainTip> = vec![];
            task::spawn(async move {
                // Try to load the node version an update the cache with it.
                let version = load_node_version(node.clone(), &network.name).await;
                update_cache(
                    &caches_clone,
                    network.id,
                    CacheUpdate::NodeVersion {
                        node_id: node.info().id,
                        version,
                    },
                    &cache_changed_tx_cloned,
                )
                .await;

                let mut first_iteration = true;
                loop {
                    // We specifically wait at the beginning of the loop, as we
                    // are using 'continue' on errors. If we would wait at the end,
                    // we might skip the waiting.
                    if first_iteration {
                        // Fetch immediately on startup (after a small stagger) so
                        // we catch up on headers we missed while we were down.
                        sleep(initial_stagger).await;
                        first_iteration = false;
                    } else if let Err(e) = node.wait_for_tip_change(query_interval).await {
                        // `wait_for_tip_change` failed (e.g. `waitfornewblock` is
                        // not whitelisted on the node). Fall back to a fixed-delay
                        // poll so we don't busy-loop.
                        debug!(
                            "wait_for_tip_change failed for {} on network '{}' (id={}): {} - falling back to polling",
                            node.info(),
                            network.name,
                            network.id,
                            e
                        );
                        sleep(query_interval).await;
                    }
                    let mut tips = match node.tips().await {
                        Ok(tips) => {
                            if !is_node_reachable(&caches_clone, network.id, node.info().id).await {
                                if let Some(ref activity_tx) = node_activity_tx {
                                    send_activity(
                                        activity_tx,
                                        network.id,
                                        node.info().id,
                                        ActivityEventKind::NodeReachable {},
                                    );
                                }
                                update_cache(
                                    &caches_clone,
                                    network.id,
                                    CacheUpdate::NodeReachability {
                                        node_id: node.info().id,
                                        reachable: true,
                                    },
                                    &cache_changed_tx_cloned,
                                )
                                .await;
                            }
                            tips
                        }
                        Err(e) => {
                            error!(
                                "Could not fetch chaintips from {} on network '{}' (id={}): {:?}",
                                node.info(),
                                network.name,
                                network.id,
                                e
                            );
                            if is_node_reachable(&caches_clone, network.id, node.info().id).await {
                                if let Some(ref activity_tx) = node_activity_tx {
                                    send_activity(
                                        activity_tx,
                                        network.id,
                                        node.info().id,
                                        ActivityEventKind::NodeUnreachable {},
                                    );
                                }
                                update_cache(
                                    &caches_clone,
                                    network.id,
                                    CacheUpdate::NodeReachability {
                                        node_id: node.info().id,
                                        reachable: false,
                                    },
                                    &cache_changed_tx_cloned,
                                )
                                .await;
                            }
                            continue;
                        }
                    };

                    // For example, btcd doesn't gurantee the order of the chain
                    // tips returned. This means, while they are equal, the order
                    // can differ and we will treat them as unequal.
                    tips.sort();

                    if last_tips != tips {
                        let (new_headers, miners_needed): (Vec<HeaderInfo>, Vec<BlockHash>) =
                            match node
                                .new_headers(&tips, &tree_clone, network.min_fork_height)
                                .await
                            {
                                Ok(headers) => headers,
                                Err(e) => {
                                    error!(
                                    "Could not fetch headers from {} on network '{}' (id={}): {}",
                                    node.info(),
                                    network.name,
                                    network.id,
                                    e
                                );
                                    continue;
                                }
                            };

                        // Identify the miner of the new header(s)
                        for hash in miners_needed.iter() {
                            if let Err(e) = pool_id_tx_clone.send(*hash) {
                                error!(
                                    "Could not send a block hash into the pool identification channel: {}",
                                    e
                                );
                            }
                        }

                        let old_tips = std::mem::replace(&mut last_tips, tips.clone());
                        let db_write = db_write.clone();
                        // We want to avoid stripping the tree (strip_tree()) if it didn't change.
                        // Keeping tracking of changes:
                        let mut tree_changed = false;
                        if !new_headers.is_empty() {
                            tree_changed =
                                insert_new_headers_into_tree(&tree_clone, &new_headers).await;

                            match db::write_to_db(&new_headers, db_write, network.id).await {
                                Ok(_) => info!(
                                    "Written {} headers to database for network '{}' by node {}",
                                    new_headers.len(),
                                    network.name,
                                    node.info()
                                ),
                                Err(e) => {
                                    error!("Could not write new headers for network '{}' by node {} to database: {}", network.name, node.info(), e);
                                    return MainError::Db(e);
                                }
                            }
                        }

                        // Log tip activity after the new headers were inserted
                        // into the tree, so reorg detection sees the new blocks.
                        if let Some(ref activity_tx) = node_activity_tx {
                            for kind in activity::tip_events(&old_tips, &tips, &tree_clone).await {
                                send_activity(activity_tx, network.id, node.info().id, kind);
                            }
                        }

                        // Update node tips in cache
                        update_cache(
                            &caches_clone,
                            network.id,
                            CacheUpdate::NodeTips {
                                node_id: node.info().id,
                                tips: tips.clone(),
                            },
                            &cache_changed_tx_cloned,
                        )
                        .await;

                        // With this node's tips updated in the cache, re-evaluate
                        // which of the network's opted-in nodes are lagging.
                        if let Some(ref activity_tx) = lagging_activity_tx {
                            let heights = active_tip_heights(network.id, &caches_clone).await;
                            let events = {
                                let mut state = lagging_state.lock().await;
                                activity::lagging_events(&heights, &mut state)
                            };
                            for (node_id, kind) in events {
                                if network.activity_log_node_ids.contains(&node_id) {
                                    send_activity(activity_tx, network.id, node_id, kind);
                                }
                            }
                        }

                        if tree_changed {
                            update_header_tree_cache(
                                &network,
                                &tree_clone,
                                &caches_clone,
                                tips.iter().map(|t| t.height),
                                &cache_changed_tx_cloned,
                            )
                            .await;
                        }
                    }
                }
            });
        }

        for remote in network.remote_forkobservers.iter().cloned() {
            task::spawn(remote_forkobserver::run_poller(
                remote,
                network.clone(),
                tree.clone(),
                db.clone(),
                caches.clone(),
                cache_changed_tx.clone(),
                config.query_interval,
            ));
        }

        // A one-shot thread trying to identify all unidentified miners. This
        // runs once after startup (with a 5 minutes delay to be sure nodes
        // are ready and the headertree is loaded).
        let tree_clone = tree.clone();
        let caches_clone = caches.clone();
        let network_clone = network.clone();
        let pool_id_tx_clone = pool_id_tx.clone();
        task::spawn(async move {
            sleep(Duration::from_secs(5 * 60)).await;

            let tip_heights: BTreeSet<u64> = tip_heights(network_clone.id, &caches_clone).await;
            let interesting_heights = headertree::sorted_interesting_heights(
                &tree_clone,
                network_clone.max_interesting_heights,
                tip_heights,
            )
            .await;

            let tree_locked = tree_clone.lock().await;

            for header_info in tree_locked
                .0
                .raw_nodes()
                .iter()
                .filter(|node| node.weight.miner == "" || node.weight.miner == MINER_UNKNOWN)
                .filter(|node| {
                    let h = node.weight.height;
                    interesting_heights.contains(&h)
                        || interesting_heights.contains(&(h + 1))
                        || interesting_heights.contains(&(h + 2))
                        || interesting_heights.contains(&(max(h, 1) - 1))
                        || network_clone
                            .countdown
                            .as_ref()
                            .is_some_and(|c| h + 2 >= c.height && h <= c.height.saturating_add(2))
                })
                .map(|node| node.weight.clone())
            {
                if let Err(e) = pool_id_tx_clone.send(header_info.header.block_hash()) {
                    error!(
                        "Could not send block hash into the pool identification channel: {}",
                        e
                    );
                }
            }
        });

        // A thread that identifies miners for each header send into the pool
        // id channel
        let tree_clone = tree.clone();
        let db_clone2 = db_clone.clone();
        let caches_clone = caches.clone();
        let network_clone = network.clone();
        let cache_changed_tx_clone = cache_changed_tx.clone();
        task::spawn(async move {
            let pool_identification_network = match network.pool_identification.network {
                Some(ref network) => network.to_network(),
                None => Network::Regtest,
            };
            let pool_identification_data = default_data(pool_identification_network);

            let limit = 100;
            let mut buffer: Vec<BlockHash> = Vec::with_capacity(limit);
            loop {
                buffer.clear();
                pool_id_rx.recv_many(&mut buffer, limit).await;
                for hash in buffer.iter() {
                    if !network_clone.pool_identification.enable {
                        continue;
                    }

                    let idx: NodeIndex = {
                        let tree_locked = tree_clone.lock().await;
                        match tree_locked.1.get(hash) {
                            Some(idx) => *idx,
                            None => {
                                error!("Block hash {} not (yet) present in tree for network: {}. Skipping identification...", hash.to_string(), network_clone.name);
                                continue;
                            }
                        }
                    };

                    let mut header_info = {
                        let tree_locked = tree_clone.lock().await;
                        tree_locked.0[idx].clone()
                    };

                    // skip miner identification if we previously identified a miner
                    if !(header_info.miner == MINER_UNKNOWN.to_string() || header_info.miner == "")
                    {
                        continue;
                    }

                    let mut miner = MINER_UNKNOWN.to_string();
                    for node in network_clone.nodes.iter().cloned() {
                        match node
                            .coinbase(&header_info.header.block_hash(), header_info.height)
                            .await
                        {
                            Ok(coinbase) => {
                                miner = match coinbase.identify_pool(
                                    pool_identification_network,
                                    &pool_identification_data,
                                ) {
                                    Some(result) => result.pool.name,
                                    None => MINER_UNKNOWN.to_string(),
                                };
                            }
                            Err(e) => {
                                warn!(
                                    "Could not get coinbase for block {} from node {}: {}",
                                    header_info.header.block_hash().to_string(),
                                    node.info().name,
                                    e
                                );
                            }
                        }
                        if miner != MINER_UNKNOWN.to_string() {
                            info!(
                                "Updated miner for block {} from node {}: {}",
                                header_info.height,
                                node.info().name,
                                miner
                            );
                            break;
                        }
                    }
                    header_info.update_miner(miner);

                    // update in-memory graph
                    {
                        let mut tree_locked = tree_clone.lock().await;
                        tree_locked.0[idx] = header_info.clone();
                    }
                    // write to db
                    if let Err(e) = db::update_miner(
                        db_clone2.clone(),
                        &header_info.header.block_hash(),
                        header_info.miner.clone(),
                    )
                    .await
                    {
                        warn!(
                            "Could not update miner to {} for block {}: {}",
                            header_info.miner.clone(),
                            &header_info.header.block_hash(),
                            e
                        );
                    }
                    // update cache
                    update_cache(
                        &caches_clone,
                        network.id,
                        CacheUpdate::HeaderMiner { header_info },
                        &cache_changed_tx_clone,
                    )
                    .await;
                }
            }
        });
    }

    let routes = api::build_routes(
        &network_infos,
        &config,
        &caches,
        cache_changed_tx_warp,
        &activity,
    );

    warp::serve(routes).run(config.address).await;
    Ok(())
}
