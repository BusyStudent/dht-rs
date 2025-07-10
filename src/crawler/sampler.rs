#![allow(dead_code)] // SHUTUP

use std::{collections::HashMap, net::SocketAddr, num::NonZero, sync::{Arc, Mutex, OnceLock, Weak}, time::{Duration, SystemTime}};

use tokio::{sync::Semaphore, task::JoinSet};
use async_trait::async_trait;
use lru::LruCache;
use tracing::{instrument, info};

use crate::{dht::DhtSession, InfoHash, NodeId};

struct SamplerInner {
    dht_session: DhtSession, // DHT session for using sample_infohashes
    sem: Semaphore, // Semaphore to limit the number of concurrent samples
    sampled: Mutex<LruCache<SocketAddr, Option<SystemTime> > >, // Mapping the sampled node to the next time we can sample it, None means we are currently sampling it
    observer: OnceLock<Weak<dyn SamplerObserver + Sync + Send> >,
}

#[async_trait]
pub trait SamplerObserver {
    /// Called when a node was sampled, the result in infohash => peers
    async fn on_hash_sampled(&self, hashes: HashMap<InfoHash, Vec<SocketAddr> >);

    /// Check if the node has metadata for the given infohash, used for optimization
    async fn has_metadata(&self, hash: InfoHash) -> bool;
}

/// Use sample_infohashes to sample a subset of infohashes from the given node
#[derive(Clone)]
pub struct Sampler {
    inner: Arc<SamplerInner>
}

impl Sampler {
    pub fn new(session: DhtSession) -> Self {
        let _100k = NonZero::try_from(100 * 1000).unwrap();
        return Self {
            inner: Arc::new(SamplerInner {
                dht_session: session,
                sem: Semaphore::new(5),
                sampled: Mutex::new(LruCache::new(_100k)), // 100K
                observer: OnceLock::new(),
            })
        }
    }

    pub fn set_observer(&self, observer: Weak<dyn SamplerObserver + Sync + Send>) {
        self.inner.observer.set(observer).unwrap();
    }

    // Add an node to sample right now
    pub fn add_sample_node(&self, ip: SocketAddr) {
        let mut sampled = self.inner.sampled.lock().unwrap();
        if let Some(item) = sampled.get(&ip) {
            match item {
                Some(time) => {
                    if *time > SystemTime::now() { // Sampled, and it's time to sample again
                        return;
                    }
                }
                None => {
                    return; // Currently sampling, ignore
                }
            }
        }
        // info!("Sampling {ip}");
        sampled.put(ip, None); // We are going to sample this node

        // Start it
        tokio::spawn(self.clone().do_sample(ip));
    }

    /// Do the actual sampling
    #[instrument(skip(self))]
    async fn do_sample(self, ip: SocketAddr) {
        let observer = match self.inner.observer.get().and_then(|o| o.upgrade()) {
            Some(o) => o,
            None => return,
        };
        let _premit = match self.inner.sem.acquire().await {
            Ok(p) => p,
            Err(_) => return,
        };
        let result =  self.inner.dht_session.clone().sample_infohashes(ip, NodeId::rand()).await;
        let (hashes, duration) = match result {
            Ok(mut reply) => {
                let subsem = Arc::new(Semaphore::new(5)); // Max 5 in parallel get_peers
                let dur = Duration::from_secs(reply.interval as u64);
                let mut map = HashMap::new();
                let mut set = JoinSet::new();

                // Avoid duplicated infohashes
                reply.info_hashes.sort();
                reply.info_hashes.dedup();

                // Start the workers
                for hash in reply.info_hashes.iter().cloned() {
                    if observer.has_metadata(hash).await {
                        continue;
                    }
                    let session = self.inner.dht_session.clone();
                    let sem = subsem.clone();
                    set.spawn(async move {
                        let _premit = sem.acquire().await.unwrap();
                        let peers = match session.get_peers_raw(ip, hash).await {
                            Ok(reply) => reply.values,
                            Err(_) => Vec::new(),
                        };

                        (hash, peers)
                    });
                }

                // Collect
                let mut count = 0;
                for (hash, mut peers) in set.join_all().await {
                    // Sort and dedup
                    peers.sort();
                    peers.dedup();
                    count += peers.len();
                    map.insert(hash, peers);
                }
                info!("Sampled {} infohashes {} peers", reply.info_hashes.len(), count);
                
                (map, dur)
            }
            Err(_err) => {
                // warn!("Failed to sample infohashes from {ip}: {err}");
                // Try Ping it
                let dur = if self.inner.dht_session.clone().ping(ip).await.is_ok() {
                    Duration::from_secs(60 * 60 * 4) // Not network error, this node unsupport sample_infohashes, try again in 4h
                }
                else {
                    Duration::from_secs(60 * 60) // 1 hour latency for retries if we fail
                };

                (HashMap::new(), dur) // 1 hour latency for retries if we fail
            }
        };
        drop(_premit);

        // Update the time...
        {
            let mut sampled = self.inner.sampled.lock().unwrap();
            sampled.put(ip, Some(SystemTime::now() + duration));
        }

        // Notify the observer if we have any hashes
        if !hashes.is_empty() {
            observer.on_hash_sampled(hashes).await;
        }
    }
}