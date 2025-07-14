#![allow(dead_code, unused_imports)] // Let it shutup!

// TODO: Did we need to split the tracker to a new compoment?
use std::{
    cell::OnceCell, collections::{HashMap, HashSet}, 
    net::{IpAddr, SocketAddr}, 
    sync::{Arc, Mutex, OnceLock, RwLock, Weak}
};

use crate::{
    bt::{AnnounceInfo, AnnounceResult, AnnounceTask, BtError, Event, TrackerError, TrackerManager, Tracker}, 
    dht::DhtSession, 
    InfoHash, PeerId
};
use tokio::{
    sync::{mpsc, Semaphore},
    task::{JoinHandle, JoinSet},
    net::UdpSocket,
    time,
};
use tracing::{info, instrument};

struct PeerFinderInner {
    dht_session: DhtSession, // The dht session we used to call get_peers
    udp_socket: Arc<UdpSocket>, // The udp socket we used to new the udp tracker
    pending: Mutex<HashMap<InfoHash, JoinHandle<()>>>,
    sem: Semaphore, // Semaphore to limit the number of concurrent peer finding tasks
    tracker_manager: TrackerManager,
    controller: OnceLock<Weak<dyn PeerFinderController + Sync + Send> >,

    // Config
    max_retries: usize,
    bind_ip: SocketAddr,
    peer_id: PeerId, // For Tracker Announcement

    // Status
}

#[derive(Clone)]
pub struct PeerFinder {
    inner: Arc<PeerFinderInner>,
}

pub struct PeerFinderConfig {
    pub tracker_manager: TrackerManager,
    pub dht_session: DhtSession,
    pub udp_socket: Arc<UdpSocket>,
    pub max_concurrent: usize,
    pub max_retries: usize,
    pub bind_ip: SocketAddr,
    pub peer_id: PeerId,
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

    fn is_same_family(&self, addr: &SocketAddr) -> bool {
        return self.inner.bind_ip.ip().is_ipv4() == addr.ip().is_ipv4();
    }

    pub fn new(config: PeerFinderConfig) -> Self {
        return Self {
            inner: Arc::new(PeerFinderInner {
                dht_session: config.dht_session,
                udp_socket: config.udp_socket,
                pending: Mutex::new(HashMap::new()),
                sem: Semaphore::new(config.max_concurrent),
                controller: OnceLock::new(),
                tracker_manager: config.tracker_manager,

                // Config
                max_retries: config.max_retries,
                bind_ip: config.bind_ip,
                peer_id: config.peer_id,
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

    pub fn cancel_all(&self) {
        info!("Canceling all peer finding tasks");
        let mut map = self.inner.pending.lock().unwrap();
        for (_, handle) in map.drain() {
            handle.abort();
        }
        self.notify_tasks_count_changed(0); // Notify the controller, the task count is 0
    }

    /// Get the number of pending tasks
    pub fn pending_len(&self) -> usize {
        return self.inner.pending.lock().unwrap().len();   
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
            return result.peers;
        }
        return Vec::new();
    }

    #[instrument(skip(self))]
    async fn find_peers(self, hash: InfoHash) {
        let mut first_time = true;
        for _ in 0..self.inner.max_retries {
            if !first_time {
                info!("Finished finding peers for {}, sleeping for 15 minutes", hash);
                // Next find_peers in 15 minutes
                time::sleep(time::Duration::from_secs(15 * 60)).await;
            }
            let premit = match self.inner.sem.acquire().await {
                Ok(p) => p,
                Err(_) => return, // May not happend
            };
            info!("Finding peers");
            // 1. Find peers on dht if not enough
            let peers = self.find_peers_on_dht(hash).await;
            if let Some(controller) = self.inner.controller.wait().upgrade() {
                if !peers.is_empty() {
                    controller.on_peers_found(hash, peers);
                }
            }

            drop(premit);
            first_time = false;
        }
        // Remove the task from the pending map
        let mut map = self.inner.pending.lock().unwrap();
       
        map.remove(&hash);
        self.notify_tasks_count_changed(map.len());
    }
}