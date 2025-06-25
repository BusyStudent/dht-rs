// credit libtorrent
#![allow(dead_code, unused_imports)] // Let it shutup!

use std::collections::{BTreeSet, VecDeque};
use std::net::{SocketAddr};
use std::time::{Duration, SystemTime};
use std::cmp;
use tracing::{debug, error, info, trace};

use crate::NodeId;

pub const KBUCKET_SIZE: usize = 8;

#[derive(Debug, Clone, PartialEq, Eq)]
enum NodeStatus {
    Good,
    Questionable,
    Bad,
}

/// The internal Node
#[derive(Debug, Clone)]
struct Node {
    pub id: NodeId,
    pub ip: SocketAddr,
    pub last_seen: SystemTime, // Using Instant may better, but SystemTime is better for debug
    pub status: NodeStatus
}

/// The internal KBucket
#[derive(Debug)]
struct KBucket {
    pub nodes: Vec<Node>,
    pub pending: VecDeque<(NodeId, SocketAddr)>, // The pending nodes of the kbucket, the limits is K, TODO
    pub last_seen: SystemTime,
}

/// The routing table of the DHT
pub struct RoutingTable {
    routers: BTreeSet<SocketAddr>, // The endpoints of the bootstrap node, ignore it
    ips: BTreeSet<SocketAddr>, // The endpoints is to limits the same endpoint but id is different
    buckets: Vec<KBucket>,
    id: NodeId, // The self id
}

#[derive(Debug)]
pub enum UpdateNodeError {
    Failed,
    NeedSplit,
}

#[derive(Debug, PartialEq)]
enum SplitBucketPosition {
    CurrentBucket,      // The node should in the current bucket
    NewBucket,          // The node should in the new splited bucket
    WrongBucket(usize), // The node should not on the current or the new bucket, wrong state!
}

impl Node {
    pub fn new(id: NodeId, ip: SocketAddr) -> Node {
        return Node {
            id: id,
            ip: ip,
            last_seen: SystemTime::now(),
            status: NodeStatus::Good,
        }
    }
}

impl KBucket {
    pub fn new() -> KBucket {
        return KBucket { 
            nodes: Vec::new(), 
            pending: VecDeque::new(), 
            last_seen: SystemTime::now()
        };
    }
}

/// The Routing Table [0] on farest bucket, the back is the closest bucket, only the back bucket can be split
/// - [1] [0, 2 ** 160]
/// - [2] [2 ** 159, 2 ** 160] [0, 2 ** 159] 
/// - [3] [2 ** 159, 2 ** 160] [2 ** 158, 2 ** 159] [0, 2 ** 158] 
impl RoutingTable {

    /// Create an new routing table
    pub fn new(id: NodeId) -> RoutingTable {
        return RoutingTable { 
            routers: BTreeSet::new(),
            ips: BTreeSet::new(), 
            buckets: vec![KBucket::new()], // The new routing table have one buckets
            id: id
        };
    }

    /// Calc the bucket index by given node id, it return the location of the bucket
    pub fn calc_bucket_index(&self, target: NodeId) -> usize {
        // the self.buckets.len() at least is 1
        let clz = (self.id ^ target).leading_zeros() as usize;
        return cmp::min(clz, self.buckets.len() - 1);
    }

    /// Calc the node position after the bucket split
    /// 
    /// It means each we split, we spilt the far bucket out
    /// 
    fn calc_node_pos_after_split(self_id: NodeId, buckets_len: usize, target: NodeId) -> SplitBucketPosition {
        let clz = (self_id ^ target).leading_zeros() as usize;
        let idx = cmp::min(clz, buckets_len);
        if idx == buckets_len {
            return SplitBucketPosition::NewBucket;
        }
        else if idx == buckets_len - 1 {
            return SplitBucketPosition::CurrentBucket;
        }
        else {
            return SplitBucketPosition::WrongBucket(idx);
        }
    }

    /// Find the node, by given id, return the bucket and the index in the bucket's vector
    // fn index_node(&self, target: NodeId) -> Option<(&KBucket, usize)> {
    //     let idx = self.calc_bucket_index(target);
    //     let bucket = &self.buckets[idx];
    //     let pos = bucket.nodes.iter().position(|node| node.id == target )?;
    //     return Some((bucket, pos));
    // }

    fn index_node_mut(&mut self, target: NodeId) -> Option<(&mut KBucket, usize)> {
        let idx = self.calc_bucket_index(target);
        let bucket = &mut self.buckets[idx];
        let pos = bucket.nodes.iter().position(|node| node.id == target )?;
        return Some((bucket, pos));
    }

    /// Find the closest nodes of the target, the return's max len is K
    pub fn find_node(&self, target: NodeId) -> Vec<(NodeId, SocketAddr)> {
        let mut idx = self.calc_bucket_index(target);
        let mut res = Vec::new();
        let forward = idx < self.buckets.len() / 2; // If the idx is on the front side, we should find the forward nodes
        loop {
            let bucket = &self.buckets[idx];
            for node in &bucket.nodes {
                res.push((node.id, node.ip));
            }
            if res.len() >= KBUCKET_SIZE {
                break;
            }
            if forward {
                idx += 1;
            }
            else if idx > 0 {
                idx -= 1;
            }
            if idx == 0 || idx == self.buckets.len() - 1 { // Reach the edge
                break;
            }
        }
        // Sort it by distance
        res.sort_by(|a, b| {
            let ld = a.0 ^ target;
            let rd = b.0 ^ target;
            return ld.cmp(&rd);
        });
        res.truncate(KBUCKET_SIZE);
        return res;
    }

    /// Update the routing table by given info
    pub fn update_node(&mut self, target: NodeId, ip: &SocketAddr) -> Result<(), UpdateNodeError> {
        if target.is_zero() {
            return Err(UpdateNodeError::Failed); // Ignore the zero id
        }
        let idx = self.calc_bucket_index(target);
        let buckets_len = self.buckets.len();
        let bucket = &mut self.buckets[idx];
        if self.ips.contains(ip) {
            let node = match bucket.nodes.iter_mut().find(|node| node.id == target) {
                Some(node) => node,
                None => return Err(UpdateNodeError::Failed), // Ip exists but can not find the id
            };
            if node.ip != *ip {
                return Err(UpdateNodeError::Failed);
            }
            // Got it, just update the timestamp and move
            trace!("Update node {}: {}", target, ip);
            node.status = NodeStatus::Good;
            node.last_seen = SystemTime::now();
            bucket.last_seen = SystemTime::now();
            return Ok(());
        }
        if bucket.nodes.len() == KBUCKET_SIZE { // Full!
            if idx != buckets_len - 1 || buckets_len == 160 {
                return Err(UpdateNodeError::Failed);
            }
            return Err(UpdateNodeError::NeedSplit);
        }
        // Doing add logic, do filter it
        if target == self.id || self.routers.contains(ip) { // If is self or ip from router nodes, ignore it!
            return Err(UpdateNodeError::Failed);
        }
        let node = Node::new(target, ip.clone());
        bucket.nodes.push(node);
        bucket.last_seen = SystemTime::now();
        self.ips.insert(ip.clone());
        return Ok(());
    }

    /// Add node into the routing table, it will handle the split logic automatically
    /// If the node already exists, it will update the timestamp and return Ok(()), if the bucket is full, it will split the bucket
    pub fn add_node(&mut self, target: NodeId, ip: &SocketAddr) -> Result<(), UpdateNodeError> {
        loop {
            match self.update_node(target, ip) {
                Ok(_) => return Ok(()),
                Err(UpdateNodeError::NeedSplit) => {
                    self.split_bucket();
                },
                Err(UpdateNodeError::Failed) => return Err(UpdateNodeError::Failed),
            }
        }
    }

    /// Remove the node by given id, return the removed node 
    pub fn remove_node(&mut self, target: NodeId) -> Option<(NodeId, SocketAddr)> {
        let (bucket, pos) = self.index_node_mut(target)?;
        let node = bucket.nodes.swap_remove(pos);
        self.ips.remove(&node.ip);
        return Some((node.id, node.ip));
    }

    /// Mark an node as timeout
    pub fn node_timeout(&mut self, target: NodeId) {
        if let Some((bucket, pos)) = self.index_node_mut(target) {
            let node = &mut bucket.nodes[pos];
            match node.status {
                NodeStatus::Questionable => {
                    // debug!("Node {} is timeout, before that is question, mark it as bad", node.id);
                    node.status = NodeStatus::Bad;
                },
                NodeStatus::Good => {
                    // debug!("Node {} is timeout, mark it as questionable", node.id);
                    node.status = NodeStatus::Questionable;
                },
                NodeStatus::Bad => {
                    // info!("Node {} is timeout, and bad, remove it", node.id);
                    bucket.nodes.remove(pos);
                },
            }
        }
    }

    /// Split the bucket
    pub fn split_bucket(&mut self) {
        let buckets_len = self.buckets.len();
        if buckets_len == 160 { // MAX Buckets
            return;
        }
        let last = self.buckets.last_mut().expect("WTF Impossible!");
        let mut new_bucket = KBucket::new();
        let mut cur_vec = Vec::new();
        let new_vec = &mut new_bucket.nodes;

        for node in std::mem::take(&mut last.nodes) {
            match RoutingTable::calc_node_pos_after_split(self.id, buckets_len, node.id) {
                SplitBucketPosition::CurrentBucket => cur_vec.push(node),
                SplitBucketPosition::NewBucket => new_vec.push(node),
                SplitBucketPosition::WrongBucket(_idx) => { // It should not happen, node in wrong bucket
                    error!("WTF: Node {:?} in wrong bucket, should not happen!", node.id);
                    new_vec.push(node);
                },
            }
        }
        last.nodes = cur_vec;
        new_bucket.last_seen = last.last_seen.clone();
        self.buckets.push(new_bucket);
    }

    /// Get the next node we need to refresh
    pub fn next_refresh_node(&mut self, min_duration: Duration) -> Option<(NodeId, SocketAddr, Duration)> {  
        let mut vec = Vec::new();
        for bucket in self.buckets.iter_mut() {
            for node in bucket.nodes.iter_mut() {
                vec.push(node);
            }
        }
        vec.sort_by(|a, b| a.last_seen.cmp(&b.last_seen) );
        let node = vec.first_mut()?;
        let duration = match node.last_seen.elapsed() {
            Ok(duration) => duration,
            Err(_) => return None,
        };
        if duration < min_duration {
            return None;
        }
        node.last_seen = SystemTime::now(); // Avoid select the node twice
        return Some((node.id, node.ip, duration));
    }

    /// Get the bucket node size is less than it
    pub fn less_node_buckets_indexes(&self, len: usize) -> Vec<usize> {
        let mut res = Vec::new();
        for (i, bucket) in self.buckets.iter().enumerate() {
            if bucket.nodes.len() < len {
                res.push(i);
            }
        }
        return res;
    }

    pub fn add_router(&mut self, ip: &SocketAddr) {
        self.routers.insert(ip.clone());
    }

    /// Get the num of the nodes in the table
    pub fn nodes_len(&self) -> usize {
        return self.ips.len(); // Use ips to count, it is more accurate
    }

    /// Iterate the nodes in the routing table, from the closest to the farest
    pub fn iter(&self) -> impl Iterator<Item = (NodeId, SocketAddr)> + '_ {
        let mut vec = Vec::new();
        for bucket in self.buckets.iter() {
            for node in bucket.nodes.iter() {
                vec.push((node.id.clone(), node.ip.clone()));
            }
        }
        vec.sort_by(|a, b| {
            let ld = a.0 ^ self.id;
            let rd = b.0 ^ self.id;
            return ld.cmp(&rd);
        });
        return vec.into_iter();
    }
}

impl std::fmt::Debug for RoutingTable {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Print the routing table in a readable format
        let zero = Duration::from_secs(0);
        write!(f, "RoutingTable {{ id: {}, buckets: [\n", self.id.hex())?;
        for (i, bucket) in self.buckets.iter().enumerate() {
            write!(f, "  Bucket {}: [\n", i)?;
            for node in &bucket.nodes {
                let duration = node.last_seen.elapsed().unwrap_or(zero);
                write!(f, "    {}: {} Last seen {}s , \n", node.id.hex(), node.ip, duration.as_secs())?;
            }
            write!(f, "  ], \n")?;
        }
        return write!(f, "] }}");
    }
}

#[cfg(test)]
mod tests {
    use std::net::{IpAddr, Ipv4Addr};
    use ctor::ctor;

    use super::*;


    fn make_addr(ip: u32, port: u16) -> SocketAddr {
        return SocketAddr::new(IpAddr::V4(Ipv4Addr::from(ip)), port);
    }

    #[ctor]
    fn init_color_backtrace() {
        color_backtrace::install();
    }

    #[test]
    fn test_routing_table_basic() {
        let id = NodeId::new([1; 20]);
        let mut table = RoutingTable::new(id);
        
        // Test empty table
        assert_eq!(table.buckets.len(), 1);
        assert!(table.find_node(NodeId::new([2; 20])).is_empty());

        // Test adding nodes
        let addr = make_addr(0, 80);
        let node_id = NodeId::new([2; 20]);
        assert!(table.update_node(node_id, &addr).is_ok());

        // Test finding added node
        let closest = table.find_node(node_id);
        assert_eq!(closest.len(), 1);
        assert_eq!(closest[0].0, node_id);
        assert_eq!(closest[0].1, addr);

        // Test removing node
        let removed = table.remove_node(node_id);
        assert!(removed.is_some());
        assert_eq!(removed.unwrap().0, node_id);
        assert_eq!(removed.unwrap().1, addr);
        
        // Test bucket need split
        for i in 0..=KBUCKET_SIZE {
            let test_addr = make_addr(i as u32 + 1, 80);
            if i < KBUCKET_SIZE {
                let test_id = NodeId::new([(i + 2) as u8; 20]);
                assert!(table.update_node(test_id, &test_addr).is_ok());
            }
            else {
                let mut test_id = id.clone();
                test_id[19] = 0;
                // assert!(matches!(table.update_node(test_id, &test_addr), Err(UpdateNodeError::NeedSplit)));
            }
        }
    }

    #[test]
    fn test_bucket_split() {
        let id = NodeId::new([0; 20]);
        let mut table = RoutingTable::new(id);

        // Fill first bucket
        for i in 0..KBUCKET_SIZE {
            let test_id = NodeId::new([(i + 1) as u8; 20]);
            let addr = SocketAddr::new(IpAddr::V4(Ipv4Addr::new(127, 0, 0, (i + 1) as u8)), 8000);
            assert!(table.update_node(test_id, &addr).is_ok());
        }

        // Trigger split
        let mut split_id = NodeId::new([1; 20]);
        split_id[19] = 0;
        let split_addr = SocketAddr::new(IpAddr::V4(Ipv4Addr::new(127, 0, 0, 99)), 8000);
        assert!(matches!(table.update_node(split_id, &split_addr), Err(UpdateNodeError::NeedSplit)));
        table.split_bucket();
        assert_eq!(table.buckets.len(), 2);
    }
}
