#![allow(dead_code, unused_imports)] // Let it shutup!

use std::{
    collections::BTreeMap,
    net::SocketAddr,
    sync::{Arc, Mutex, OnceLock},
    cell::OnceCell,
};

use crate::{dht::DhtSession, InfoHash, PeerId};
use tokio::{
    sync::{mpsc, Semaphore},
    task::{JoinHandle, JoinSet},
    time,
};
use tracing::info;

pub type PeerFinderCallback = Box<dyn Fn(InfoHash, SocketAddr) + Send + Sync>;

struct PeerFinderInner {
    dht_session: DhtSession, // The dht session we used to call get_peers
    pending: Mutex<BTreeMap<InfoHash, JoinHandle<()>>>,
    sem: Semaphore, // Semaphore to limit the number of concurrent peer finding tasks
    callback: OnceLock<PeerFinderCallback>,

    // Config
    max_retries: usize,
}

#[derive(Clone)]
pub struct PeerFinder {
    inner: Arc<PeerFinderInner>,
}

pub struct PeerFinderConfig {
    pub dht_session: DhtSession,
    pub max_concurrent: usize,
    pub max_retries: usize,
}

impl PeerFinder {
    pub fn new(config: PeerFinderConfig) -> Self {
        return Self {
            inner: Arc::new(PeerFinderInner {
                dht_session: config.dht_session,
                pending: Mutex::new(BTreeMap::new()),
                sem: Semaphore::new(config.max_concurrent),
                max_retries: config.max_retries,
                callback: OnceLock::new(),
            }),
        };
    }

    /// Cancel the pending peer finding task for the given info hash
    pub fn cancel(&self, info_hash: InfoHash) {
        let mut map = self.inner.pending.lock().unwrap();
        if let Some(handle) = map.remove(&info_hash) {
            info!("Canceling peer finding task for {}", info_hash);
            handle.abort();
        }
    }

    pub fn set_callback(&self, callback: PeerFinderCallback) {
        let _ = self.inner.callback.set(callback);
    }

    /// Add a new info hash to the peer finding queue, MUST be called after set_callback
    pub fn add_hash(&self, info_hash: InfoHash) {
        debug_assert!(self.inner.callback.get().is_some(), "Must set callback before adding hashes");
        let mut map = self.inner.pending.lock().unwrap();
        if map.contains_key(&info_hash) {
            return;
        }
        let handle = tokio::spawn(self.clone().find_peers(info_hash));
        map.insert(info_hash, handle);
    }

    async fn find_peers(self, hash: InfoHash) {
        for _ in 0..self.inner.max_retries {
            let premit = match self.inner.sem.acquire().await {
                Ok(p) => p,
                Err(_) => return, // Too many tasks are running, abort
            };
            info!("Finding peers for {}", hash);
            if let Ok(result) = self.inner.dht_session.clone().get_peers(hash).await {
                info!("Found {} peers for {}", result.peers.len(), hash);
                // Call the callback for each peer
                let cb = self.inner.callback.wait();
                for peer in result.peers {
                    cb(hash, peer);
                }
                // Begin announcing the peers
                let mut set = JoinSet::new();
                for node in result.nodes {
                    set.spawn(
                        self.inner.dht_session.clone().announce_peer(node.ip, hash, None, node.token)
                    );
                }
                let _ = set.join_all().await;
            }
            drop(premit);
            info!("Finished finding peers for {}, sleeping for 15 minutes", hash);
            // Next find_peers in 15 minutes
            time::sleep(time::Duration::from_secs(15 * 60)).await;
        }
        // Remove the task from the pending map
        self.inner.pending.lock().unwrap().remove(&hash);
    }
}

impl Drop for PeerFinder {
    fn drop(&mut self) {
        let map = self.inner.pending.lock().unwrap();
        for (_, handle) in map.iter() {
            handle.abort();
        }
    }
}
