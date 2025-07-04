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
        let client = Client::builder()
            .timeout(Duration::from_secs(10))
            .no_proxy()
            .build()
            .expect("Can't create http client");
        return Self {
            inner: Arc::new(TrackerManagerInner {
                client: client,
                trackers: RwLock::new(HashMap::new()),
            })
        }
    }

    /// Get the trackers url name in the manager
    pub async fn trackers(&self) -> Vec<String> {
        return self.inner.trackers.read().await
            .keys()
            .map(|u| u.to_string())
            .collect();   
    }

    /// Add an instance of tracker to the manager
    pub async fn add_tracker_instance(&self, tracker: Arc<DynTracker>) {
        self.inner.trackers.write().await
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
            "http" | "https" => HttpTracker::new(&url, self.inner.client.clone())?.into_dyn(),
            "udp" => return None, // TODO: UDP tracker
            _ => return None, // Invalid scheme
        };
        info!("Added tracker {}", trakcer.url());
        // Add it to the map
        self.inner.trackers.write().await
            .insert(trakcer.url(), trakcer);
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
    pub async fn new(manager: TrackerManager, info: AnnounceInfo) -> Self {
        let mut actived = HashMap::new();
        for tracker in manager.inner.trackers.read().await.values() {
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
        if self.actived.is_empty() {
            return None;
        }
        // Get the average of the next announce time
        let now = Instant::now();
        let durations: Vec<Duration> = self.actived.values()
            .filter_map(|state| state.next_announce )
            .filter_map(|time| time.checked_duration_since(now))
            .collect(); // Get all valid duration 
        if durations.is_empty() {
            return Some(now); // Announce now
        }
        let sum: Duration = durations.iter().sum();
        let avg = sum / durations.len() as u32;
        return Some(now + avg);
    }

    /// Do the announce
    pub async fn announce(&mut self) -> Vec<AnnounceResult> {
        match self.avg_next_announce() {
            Some(time) => tokio::time::sleep_until(time.into()).await,
            None => return Vec::new(),
        };
        let mut set = JoinSet::new();
        for (_, state) in self.actived.iter() {
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
            match result {
                Ok(val) => {
                    info!("Got reply of {} from tracker: {}, {} peers", self.info.hash, tracker.url(), val.peers.len());
                    // Update the next announce time if exists
                    if let Some(state) = state {
                        state.next_announce = Some(Instant::now() + Duration::from_millis(val.interval as u64));
                        state.last_error = None;
                    }
                    vec.push(val);
                }
                Err(err) => {
                    info!("Got error of {} from tracker: {}, {}", self.info.hash, tracker.url(), err);
                    if let Some(state) = state {
                        state.next_announce = Some(Instant::now() + Duration::from_secs(60 * 30)); // Next 30 minutes
                        state.last_error = Some(err);
                    }
                }
            }
        }
        return vec;
    }

    /// Shutdown the announce task, send a stopped event to all trackers
    pub async fn shutdown(&mut self) {
        let mut info = self.info.clone();
        let mut set = JoinSet::new();
        info.event = Event::Stopped;
        for (_, state) in self.actived.iter() {
            if state.next_announce.is_none() {
                // Never announce, skip it
                continue;
            }
            let tracker = state.tracker.clone();
            set.spawn(async move { tracker.announce(info).await });
        }
        let _ = set.join_all().await;
        self.actived.clear();
    }

}


impl Drop for AnnounceTask {
    fn drop(&mut self) {
        let mut info = self.info;
        info.event = Event::Stopped; // Notice the tracker that the we are quitting
        for (_, state) in self.actived.iter() {
            if state.next_announce.is_none() {
                // Never announce, skip it
                continue;
            }
            let tracker = state.tracker.clone();
            tokio::spawn(async move { tracker.announce(info).await });
        }
    }
}