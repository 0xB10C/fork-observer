//! Lets the nodes a network prefers to sync from fetch headers first.

use log::{info, warn};
use std::collections::BTreeSet;
use std::time::Duration;
use tokio::sync::watch;
use tokio::time::timeout;

/// Only reached when a preferred node neither finishes nor fails its first
/// fetch, e.g. because its connection hangs rather than being refused.
const PREFERRED_SYNC_TIMEOUT: Duration = Duration::from_secs(30 * 60);

/// Holds a network's nodes back until the nodes preferred for syncing had
/// their turn. Whoever fills the header tree first spares the others the walk
/// back to `min_fork_height`, which is thousands of requests for a node
/// without batch header fetching.
///
/// A gate without preferred nodes never makes a node wait.
pub struct SyncGate {
    network_name: String,
    /// Preferred nodes that have yet to finish their first fetch. The gate is
    /// open once this is empty.
    pending: watch::Sender<BTreeSet<u32>>,
}

impl SyncGate {
    pub fn new(network_name: &str, preferred_node_ids: BTreeSet<u32>) -> Self {
        if !preferred_node_ids.is_empty() {
            info!(
                "network '{}' syncs headers from the node(s) with id {:?} first",
                network_name, preferred_node_ids
            );
        }
        SyncGate {
            network_name: network_name.to_string(),
            pending: watch::Sender::new(preferred_node_ids),
        }
    }

    /// Returns once `node_id` may fetch headers: right away for a node that is
    /// itself preferred for syncing, otherwise once the preferred nodes are
    /// done with their first fetch.
    pub async fn wait(&self, node_id: u32) {
        {
            let pending = self.pending.borrow();
            if pending.is_empty() || pending.contains(&node_id) {
                return;
            }
            info!(
                "node id={} on network '{}' waits for the node(s) with id {:?} to sync headers first",
                node_id, self.network_name, *pending
            );
        }

        // A receive error means the sender is gone, which can't happen while we
        // hold the gate it lives in. Either way, nothing left to wait for.
        let mut pending = self.pending.subscribe();
        if timeout(PREFERRED_SYNC_TIMEOUT, pending.wait_for(|p| p.is_empty()))
            .await
            .is_err()
        {
            warn!(
                "node id={} on network '{}' gave up waiting for the node(s) with id {:?} to sync headers and fetches on its own now",
                node_id,
                self.network_name,
                *self.pending.borrow()
            );
        }
    }

    /// Reports that `node_id` finished its first fetch, successfully or not.
    /// Reporting a node that isn't preferred, or reporting one twice, does
    /// nothing.
    pub fn first_fetch_done(&self, node_id: u32) {
        let mut gate_opened = false;
        self.pending.send_if_modified(|pending| {
            if !pending.remove(&node_id) {
                return false;
            }
            gate_opened = pending.is_empty();
            true
        });
        if gate_opened {
            info!(
                "network '{}': the node(s) preferred for syncing are done with their first fetch",
                self.network_name
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use tokio::time::Instant;

    fn gate(preferred_node_ids: &[u32]) -> Arc<SyncGate> {
        Arc::new(SyncGate::new(
            "testnetwork",
            preferred_node_ids.iter().copied().collect(),
        ))
    }

    #[tokio::test]
    async fn a_network_without_a_preferred_node_doesnt_wait() {
        // Waiting for nothing would hang the node task forever.
        gate(&[]).wait(7).await;
    }

    #[tokio::test]
    async fn a_preferred_node_doesnt_wait_for_itself() {
        gate(&[0, 1]).wait(0).await;
    }

    #[tokio::test]
    async fn a_node_waits_for_every_preferred_node() {
        let gate = gate(&[0, 1]);
        let waiter = {
            let gate = gate.clone();
            tokio::spawn(async move { gate.wait(7).await })
        };

        // One of the two is still fetching, so the gate stays closed.
        gate.first_fetch_done(0);
        tokio::task::yield_now().await;
        tokio::task::yield_now().await;
        assert!(!waiter.is_finished());

        gate.first_fetch_done(1);
        timeout(Duration::from_secs(5), waiter)
            .await
            .expect("the gate should open once both preferred nodes are done")
            .expect("the waiting task should not panic");
    }

    #[tokio::test]
    async fn a_node_reported_twice_doesnt_open_the_gate_for_another() {
        let gate = gate(&[0, 1]);
        let waiter = {
            let gate = gate.clone();
            tokio::spawn(async move { gate.wait(7).await })
        };

        gate.first_fetch_done(0);
        gate.first_fetch_done(0);
        tokio::task::yield_now().await;
        tokio::task::yield_now().await;
        assert!(!waiter.is_finished());
        waiter.abort();
    }

    #[tokio::test(start_paused = true)]
    async fn a_node_gives_up_on_a_preferred_node_that_never_finishes() {
        // Node 0 never reports back. Time is paused, so this returns as soon as
        // the runtime runs out of other work.
        let start = Instant::now();
        gate(&[0]).wait(7).await;
        assert!(start.elapsed() >= PREFERRED_SYNC_TIMEOUT);
    }
}
