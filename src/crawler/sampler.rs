#![allow(dead_code)] // SHUTUP

use std::{collections::{HashMap}, net::SocketAddr, sync::{Arc, Mutex, OnceLock, Weak}, time::{Duration, SystemTime}};

use tokio::{sync::Semaphore};
use async_trait::async_trait;
// use tracing::{info};

use crate::{dht::DhtSession, InfoHash, NodeId};

struct SamplerInner {
    dht_session: DhtSession, // DHT session for using sample_infohashes
    sem: Semaphore, // Semaphore to limit the number of concurrent samples
    sampled: Mutex<HashMap<SocketAddr, Option<SystemTime> > >, // Mapping the sampled node to the next time we can sample it, None means we are currently sampling it
    observer: OnceLock<Weak<dyn SamplerObserver + Sync + Send> >,
}

#[async_trait]
pub trait SamplerObserver {
    async fn on_hash_sampled(&self, hashes: Vec<InfoHash>);
}

/// Use sample_infohashes to sample a subset of infohashes from the given node
#[derive(Clone)]
pub struct Sampler {
    inner: Arc<SamplerInner>
}

impl Sampler {
    pub fn new(session: DhtSession) -> Self {
        return Self {
            inner: Arc::new(SamplerInner {
                dht_session: session,
                sem: Semaphore::new(5),
                sampled: Mutex::new(HashMap::new()),
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
        sampled.insert(ip, None); // We are going to sample this node

        // Start it
        tokio::spawn(self.clone().do_sample(ip));
    }

    /// Do the actual sampling
    async fn do_sample(self, ip: SocketAddr) {
        let observer = match self.inner.observer.get().and_then(|o| o.upgrade()) {
            Some(o) => o,
            None => return,
        };
        let result = {
            let _premit = match self.inner.sem.acquire().await {
                Ok(p) => p,
                Err(_) => return,
            };

            self.inner.dht_session.clone().sample_infohashes(ip, NodeId::rand()).await
        };

        let (hashes, duration) = match result {
            Ok(reply) => {
                // info!("Sampled {} infohashes from {ip}", reply.info_hashes.len());
                let hashes = reply.info_hashes;
                let dur = Duration::from_secs(reply.interval as u64);
                
                (hashes, dur)
            }
            Err(_err) => {
                // warn!("Failed to sample infohashes from {ip}: {err}");

                (Vec::new(), Duration::from_secs(60 * 60)) // 1 hour latency for retries if we fail
            }
        };

        // Update the time...
        {
            let mut sampled = self.inner.sampled.lock().unwrap();
            sampled.insert(ip, Some(SystemTime::now() + duration));
        }

        // Notify the observer if we have any hashes
        if !hashes.is_empty() {
            observer.on_hash_sampled(hashes).await;
        }
    }

    pub async fn run(&self) {
        // TODO:
        loop {
            tokio::time::sleep(Duration::from_secs(60 * 60 * 2)).await; // 2 hours clean up
            let mut sampled = self.inner.sampled.lock().unwrap();
            sampled.clear();
        }
    }
}