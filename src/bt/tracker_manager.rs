#![allow(dead_code, unused_imports)]
use super::{Tracker, TrackerError, HttpTracker, AnnounceInfo, AnnounceResult, Event};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::{Arc, RwLockReadGuard};
use std::time::{Duration, Instant};
use std::net::SocketAddr;
use reqwest::Client;
use tokio::{task::JoinSet, sync::RwLock};
use tracing::info;
use url::Url;

type DynTracker = dyn Tracker + Send + Sync;

struct TrackerManagerInner {
    client: Client, // HttpClient
    trackers: RwLock<HashMap<Url, Arc<DynTracker> > >, // All trackers, Map Url to Tracker
}

struct TrackerState {
    tracker: Arc<DynTracker>,
    next_announce: Option<Instant>, // Next announce time (None means never announce)
    last_error: Option<TrackerError>, // Last error?
}

/// The manage for trackers, process the Io and the announce
#[derive(Clone)]
pub struct TrackerManager {
    inner: Arc<TrackerManagerInner>,
}

/// The RAII struct for the announce
pub struct AnnounceTask {
    actived: HashMap<Url, TrackerState>, // The actived trackers, i think we can't have more than 100 trackers, use directly for it
    manager: TrackerManager,
    info: AnnounceInfo,
}

impl TrackerManager {
    pub fn new() -> Self {
        return Self {
            inner: Arc::new(TrackerManagerInner {
                client: Client::new(),
                trackers: RwLock::new(HashMap::new()),
            })
        }
    }

    /// Get the trackers url name in the manager
    pub fn trackers(&self) -> Vec<String> {
        return self.inner.trackers.blocking_read()
            .keys()
            .map(|u| u.to_string())
            .collect();   
    }

    /// Add an instance of tracker to the manager
    pub fn add_tracker_instance(&self, tracker: Arc<DynTracker>) {
        self.inner.trackers.blocking_write()
            .insert(tracker.url(), tracker);
    }

    /// Process the data of the udp tracker
    pub fn process_udp(&self, _: &[u8], _: &SocketAddr) -> bool {
        // TODO: UDP tracker
        return false;
    }

    // Try to add a tracker to the manager
    pub async fn add_tracker(&self, url: &str) -> Option<()> {
        let url = Url::parse(url).ok()?;
        // Check if the tracker is already in the manager
        if self.inner.trackers.read().await.contains_key(&url) {
            return None;
        }
        // Create the tracker
        let trakcer = match url.scheme() {
            "http" | "https" => HttpTracker::new(&url, self.inner.client.clone())?,
            "udp" => return None, // TODO: UDP tracker
            _ => return None, // Invalid scheme
        };
        // Add it to the map
        self.inner.trackers.write().await
            .insert(trakcer.url(), trakcer.into_dyn());
        return Some(());
    }

    // Add a list of trackers to the manager
    pub async fn add_tracker_list(&self, urls: &[String]) -> usize {
        let mut set = JoinSet::new();
        let mut count = 0;
        for url in urls {
            if url.is_empty() {
                continue;
            }
            let url = url.to_string();
            let this = self.clone();
            set.spawn(async move { this.add_tracker(&url).await });
        }
        for item in set.join_all().await {
            if let Some(_) = item {
                count += 1;
            }
        }
        return count;
    }
}

impl AnnounceTask {
    /// Create an new announce task
    pub fn new(manager: TrackerManager, info: AnnounceInfo) -> Self {
        let mut actived = HashMap::new();
        for tracker in manager.inner.trackers.blocking_read().values() {
            let url = tracker.url();
            let state = TrackerState {
                tracker: tracker.clone(),
                next_announce: None,
                last_error: None,  
            };
            actived.insert(url, state);
        }
        return Self {
            manager: manager,
            info: info,
            actived: actived,
        };
    }

    /// Get the next announce time, None on no available trackers
    pub fn avg_next_announce(&self) -> Option<Instant> {
        // Get the average of the next announce time
        let mut sum: u128 = 0;
        let mut n = 0;
        let now = Instant::now();
        if self.actived.is_empty() {
            return None;
        }
        for (_, state) in self.actived.iter() {
            if let Some(next_announce) = state.next_announce {
                if now > next_announce {
                    return Some(Instant::now()); // We need to announce now
                }
                sum += next_announce.elapsed().as_millis();
                n += 1;
            }
        }
        return Some(now + Duration::from_millis((sum / n) as u64));
    }

    /// Do the announce
    pub async fn announce(&mut self) -> Vec<AnnounceResult> {
        match self.avg_next_announce() {
            Some(time) => tokio::time::sleep_until(time.into()).await,
            None => return Vec::new(),
        };
        let mut set = JoinSet::new();
        for (_, state) in self.actived.iter_mut() {
            let mut info = self.info.clone();
            if let Some(next_announce) = state.next_announce {
                if Instant::now() < next_announce {
                    continue;
                }
                info.event = Event::None; // Is not the first announce, use it
            }
            let tracker = state.tracker.clone();
            set.spawn(async move { 
                let res = tracker.announce(info).await;
                (res, tracker)
            });
        }
        let mut vec = Vec::new();
        for (result, tracker) in set.join_all().await {
            let state = self.actived.get_mut(&tracker.url());
            info!("Got reply from tracker: {}", tracker.url());
            match result {
                Ok(val) => {
                    // Update the next announce time if exists
                    if let Some(state) = state {
                        state.next_announce = Some(Instant::now() + Duration::from_millis(val.interval as u64));
                    }
                    vec.push(val);
                }
                Err(err) => {
                    if let Some(state) = state {
                        state.last_error = Some(err);
                    }
                }
            }
        }
        return vec;
    }

}


impl Drop for AnnounceTask {
    fn drop(&mut self) {
        let mut info = self.info;
        info.event = Event::Stopped; // Notice the tracker that the we are quitting
        for (_, state) in self.actived.iter().map(|t| t.clone()) {
            if state.next_announce.is_none() {
                // Never announce, skip it
                continue;
            }
            let tracker = state.tracker.clone();
            tokio::spawn(async move { tracker.announce(info).await });
        }
    }
}