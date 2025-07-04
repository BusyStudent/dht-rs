#![allow(dead_code, unused_imports)] // Let it shutup!

// TODO: Did we need to split the tracker to a new compoment?
use std::{
    cell::OnceCell, collections::{HashMap, HashSet}, 
    net::{IpAddr, SocketAddr}, 
    sync::{Arc, Mutex, OnceLock, RwLock, Weak}
};

use crate::{
    bt::{AnnounceInfo, AnnounceResult, AnnounceTask, BtError, Event, TrackerError, TrackerManager}, 
    dht::DhtSession, 
    InfoHash, PeerId
};
use tokio::{
    sync::{mpsc, Semaphore},
    task::{JoinHandle, JoinSet},
    net::UdpSocket,
    time,
};
use tracing::info;

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

    // async fn find_peers_on_tracker(&self, hash: InfoHash, guard: &mut TaskStateGuard) -> Vec<SocketAddr> {
    //     let mut peers = Vec::new();

    //     // Try to find peers on all the trackers
    //     let trackers: Vec<UdpTracker> =  self.inner.trackers.read().unwrap().values().map(|f| f.clone()).collect();
    //     for trakcer in trackers.iter() {
    //         let result = self.clone().tracker_announce(trakcer.clone(), hash, Event::None).await;
    //         info!("Got result from tracker");
    //         match result {
    //             Ok(result) => {
    //                 info!("Found {} peers for {} on tracker", result.peers.len(), hash);
    //                 // guard.add_tracker_announced(tracker); // Add the cleanup guard
    //                 peers.extend_from_slice(&result.peers);
    //             }
    //             Err(e) => {
    //                 info!("Failed to announce to tracker : {}", e);
    //             }
    //         }
    //     }
    //     peers.sort(); // Remove the duplicate
    //     peers.dedup();
    //     return peers;
    // }

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
        let info = AnnounceInfo {
            hash: hash,
            peer_id: self.inner.peer_id,
            port: self.inner.bind_ip.port(),
            downloaded: 0,
            uploaded: 0,
            left: 1, // We need peers,
            event: Event::Started,
            num_want: None,
        };
        let mut task = AnnounceTask::new(self.inner.tracker_manager.clone(), info).await;
        for _ in 0..self.inner.max_retries {
            let premit = match self.inner.sem.acquire().await {
                Ok(p) => p,
                Err(_) => return, // May not happend
            };
            info!("Finding peers for {}", hash);
            // 1. Find peers on tracker
            let mut peers = Vec::new();
            for item in task.announce().await {
                for peer in item.peers.iter().filter(|addr| self.is_same_family(addr)) {
                    peers.push(peer.clone());
                }
            }
            peers.sort(); // Remove the duplicate
            peers.dedup();
            info!("Found {} peers for {} on tracker", peers.len(), hash);
            
            // 2. Find peers on dht if not enough
            if peers.len() < 50 {
                peers.extend_from_slice(&self.find_peers_on_dht(hash).await);
                peers.sort(); // Remove the duplicate
                peers.dedup();
            }
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
