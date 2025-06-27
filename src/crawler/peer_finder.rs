#![allow(dead_code, unused_imports)] // Let it shutup!

use std::{
    cell::OnceCell, collections::BTreeMap, net::SocketAddr, sync::{Arc, Mutex, Weak, OnceLock}
};

use crate::{dht::DhtSession, InfoHash, PeerId};
use tokio::{
    sync::{mpsc, Semaphore},
    task::{JoinHandle, JoinSet},
    time,
};
use tracing::info;

struct PeerFinderInner {
    dht_session: DhtSession, // The dht session we used to call get_peers
    pending: Mutex<BTreeMap<InfoHash, JoinHandle<()>>>,
    sem: Semaphore, // Semaphore to limit the number of concurrent peer finding tasks
    controller: OnceLock<Weak<dyn PeerFinderController + Sync + Send> >,

    // Config
    max_retries: usize,
    port: u16,
}

#[derive(Clone)]
pub struct PeerFinder {
    inner: Arc<PeerFinderInner>,
}

pub struct PeerFinderConfig {
    pub dht_session: DhtSession,
    pub max_concurrent: usize,
    pub max_retries: usize,
    pub port: u16, // The local port we use to listen for incoming connections
}

pub trait PeerFinderController {
    fn on_peers_found(&self, hash: InfoHash, peers: Vec<SocketAddr>);

    // Called when the number of pending tasks changed, don't call any peer finder methods in this callback
    fn on_tasks_count_changed(&self, count: usize);
}

impl PeerFinder {
    fn notify_tasks_count_changed(&self, count: usize) {
        if let Some(controller) = self.inner.controller.wait().upgrade() {
            controller.on_tasks_count_changed(count);
        }
    }

    pub fn new(config: PeerFinderConfig) -> Self {
        return Self {
            inner: Arc::new(PeerFinderInner {
                dht_session: config.dht_session,
                pending: Mutex::new(BTreeMap::new()),
                sem: Semaphore::new(config.max_concurrent),
                controller: OnceLock::new(),

                max_retries: config.max_retries,
                port: config.port,
            }),
        };
    }

    /// Cancel the pending peer finding task for the given info hash
    pub fn cancel(&self, info_hash: InfoHash) {
        let mut map = self.inner.pending.lock().unwrap();
        if let Some(handle) = map.remove(&info_hash) {
            info!("Canceling peer finding task for {}, {} left", info_hash, map.len());
            
            self.notify_tasks_count_changed(map.len());
            handle.abort();
        }
    }

    pub fn set_controller(&self, callback: Weak<dyn PeerFinderController + Sync + Send>) {
        let _ = self.inner.controller.set(callback);
    }

    /// Add a new info hash to the peer finding queue, MUST be called after set_controller
    pub fn add_hash(&self, info_hash: InfoHash) {
        debug_assert!(self.inner.controller.get().is_some(), "Must set controller before adding hashes");
        let mut map = self.inner.pending.lock().unwrap();
        if map.contains_key(&info_hash) {
            return;
        }
        let handle = tokio::spawn(self.clone().find_peers(info_hash));
        
        map.insert(info_hash, handle);
        self.notify_tasks_count_changed(map.len());
    }

    async fn find_peers_on_dht(&self, hash: InfoHash) -> Vec<SocketAddr> {
        if let Ok(result) = self.inner.dht_session.clone().get_peers(hash).await {
            info!("Found {} peers for {} on DHT", result.peers.len(), hash);
            // If peers it not enough, announce the peers
            if result.peers.len() < 50 {
                // Begin announcing the peers
                let mut set = JoinSet::new();
                for node in result.nodes {
                    set.spawn(
                        self.inner.dht_session.clone().announce_peer(node.ip, hash, None, node.token)
                    );
                }
                let _ = set.join_all().await;
            }
            return result.peers;
        }
        return Vec::new();
    }

    async fn find_peers(self, hash: InfoHash) {
        for _ in 0..self.inner.max_retries {
            let premit = match self.inner.sem.acquire().await {
                Ok(p) => p,
                Err(_) => return, // May not happend
            };
            info!("Finding peers for {}", hash);
            // 1. Find peers on dht
            let peers = self.find_peers_on_dht(hash).await;
            if let Some(controller) = self.inner.controller.wait().upgrade() {
                if !peers.is_empty() {
                    controller.on_peers_found(hash, peers);
                }
            }

            drop(premit);
            info!("Finished finding peers for {}, sleeping for 15 minutes", hash);
            // Next find_peers in 15 minutes
            time::sleep(time::Duration::from_secs(15 * 60)).await;
        }
        // Remove the task from the pending map
        let mut map = self.inner.pending.lock().unwrap();
       
        map.remove(&hash);
        self.notify_tasks_count_changed(map.len());
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
