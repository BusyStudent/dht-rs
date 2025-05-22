use std::collections::{BTreeSet, VecDeque};
use std::net::{SocketAddr};
use std::time::{Duration, SystemTime};
use super::NodeId;

pub const KBUCKET_SIZE: usize = 8;

#[derive(Debug, Clone)]
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
#[derive(Debug)]
pub struct RoutingTable {
    endpoints: BTreeSet<SocketAddr>, // The endpoints is to limits the same endpoint but id is different
    buckets: Vec<KBucket>,
    id: NodeId, // The self id
}

#[derive(Debug)]
pub enum UpdateNodeError {
    Full, // The bucket of the node is full, can not insert
    Mismatch, // The given id, ip has exists in the table, but it doesn't match
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

impl RoutingTable {

    /// Create an new routing table
    pub fn new(id: NodeId) -> RoutingTable {
        return RoutingTable { 
            endpoints: BTreeSet::new(), 
            buckets: vec![KBucket::new()], // The new routing table have one buckets
            id: id
        };
    }

    /// Calc the bucket index by given node id, it return the location of the bucket
    pub fn calc_bucket_index(&self, target: NodeId) -> usize {
        if target == self.id {
            return 0;
        }
        let clz = (self.id ^ target).leading_zeros() as usize;
        debug_assert!(clz <= 160);
        if clz >= self.buckets.len().saturating_sub(1) { // In first bucket range
            return 0;
        }
        return self.buckets.len() - 1 - clz;
    }

    /// Calc the node position after the bucket split
    fn calc_node_pos_after_split(self_id: NodeId, buckets_len: usize, target: NodeId) -> SplitBucketPosition {
        let clz = (self_id ^ target).leading_zeros() as usize;
        if clz >= buckets_len { // In cur bucket
            return SplitBucketPosition::CurrentBucket;
        }
        else if clz >= buckets_len - 1 { // In new bucket
            return SplitBucketPosition::NewBucket;
        }
        else {
            return SplitBucketPosition::WrongBucket(clz);
        }
    }

    /// Find the node, by given id, return the bucket and the index in the bucket's vector
    fn find_node(&self, target: NodeId) -> Option<(&KBucket, usize)> {
        let idx = self.calc_bucket_index(target);
        let bucket = &self.buckets[idx];
        let pos = bucket.nodes.iter().position(|node| node.id == target )?;
        return Some((bucket, pos));
    }

    fn find_node_mut(&mut self, target: NodeId) -> Option<(&mut KBucket, usize)> {
        let idx = self.calc_bucket_index(target);
        let bucket = &mut self.buckets[idx];
        let pos = bucket.nodes.iter().position(|node| node.id == target )?;
        return Some((bucket, pos));
    }

    /// Find the closest nodes of the target, the return's max len is K
    pub fn find_closest_nodes(&self, target: NodeId) -> Vec<(NodeId, SocketAddr)> {
        let idx = self.calc_bucket_index(target);
        let bucket = &self.buckets[idx];
        let mut res = Vec::new();
        for node in &bucket.nodes {
            res.push((node.id, node.ip));
        }
        return res;
    }

    /// Update the routing table by given info
    pub fn update_node(&mut self, target: NodeId, ip: &SocketAddr) -> Result<(), UpdateNodeError> {
        let idx = self.calc_bucket_index(target);
        let buckets_len = self.buckets.len();
        let bucket = &mut self.buckets[idx];
        let it = bucket.nodes.iter_mut().find(|node| node.id == target );
        if let Some(node) = it { // Already exists
            if node.ip != *ip {
                return Err(UpdateNodeError::Mismatch);
            }
            node.last_seen = SystemTime::now();
            bucket.last_seen = SystemTime::now();
            return Ok(()); // TODO: Did we need to distinguish updated and inserted?
        }
        if self.endpoints.contains(ip) { // We can not find the id, but the ip exists?
            return Err(UpdateNodeError::Mismatch);
        }
        if bucket.nodes.len() == KBUCKET_SIZE { // Check if we can add node?
            if !(idx == 0 && buckets_len < 160) { // The bucket can't split or max bucket
                return Err(UpdateNodeError::Full);
            }
            // We need to check if split, the id's bucket has space to insert?, if not, return full, because if we still can't insert it after split, it is meaningless
            let mut new_counts = 0;
            let mut cur_counts = 0;
            for node in &bucket.nodes {
                match RoutingTable::calc_node_pos_after_split(self.id, buckets_len, node.id) {
                    SplitBucketPosition::CurrentBucket => cur_counts += 1,
                    SplitBucketPosition::NewBucket => new_counts += 1,
                    _ => new_counts += 1, // As same as split 
                }
            }
            // Check witch bucket we belong, 
            let can_split = match RoutingTable::calc_node_pos_after_split(self.id, buckets_len, target) {
                SplitBucketPosition::CurrentBucket => cur_counts + 1 < KBUCKET_SIZE,
                SplitBucketPosition::NewBucket => new_counts + 1 < KBUCKET_SIZE,
                _ => new_counts + 1 < KBUCKET_SIZE, // As same as split 
            };
            if !can_split {
                return Err(UpdateNodeError::Full);
            }
            return Err(UpdateNodeError::NeedSplit);
        }
        // Doing add
        let node = Node::new(target, ip.clone());
        bucket.nodes.push(node);
        bucket.last_seen = SystemTime::now();
        self.endpoints.insert(ip.clone());
        return Ok(());
    }

    /// Remove the node by given id, return the removed node 
    pub fn remove_node(&mut self, target: NodeId) -> Option<(NodeId, SocketAddr)> {
        let (bucket, pos) = self.find_node_mut(target)?;
        let node = bucket.nodes.swap_remove(pos);
        self.endpoints.remove(&node.ip);
        return Some((node.id, node.ip));
    }

    /// Split the bucket
    /// The KAD Says only the bucket's range contains self id can be splited, so the only bucket can be splited is on the self.buckets.first()
    pub fn split_bucket(&mut self) {
        let buckets_len = self.buckets.len();
        if buckets_len == 160 { // MAX Buckets
            return;
        }
        let first = self.buckets.first_mut().expect("WTF Impossible!");
        let mut new_bucket = KBucket::new();
        let mut cur_vec = Vec::new();
        let new_vec = &mut new_bucket.nodes;

        for node in std::mem::take(&mut first.nodes) {
            match RoutingTable::calc_node_pos_after_split(self.id, buckets_len, node.id) {
                SplitBucketPosition::CurrentBucket => cur_vec.push(node),
                SplitBucketPosition::NewBucket => new_vec.push(node),
                SplitBucketPosition::WrongBucket(clz) => { // It should not happen, node in wrong bucket
                    eprintln!("Warning: Node {:?} with clz {} found in bucket 0 (len {} prior to split), expected clz >= {}. Moving to new bucket 1.", node.id, clz, buckets_len, buckets_len - 1);
                    new_vec.push(node);
                },
            }
        }
        first.nodes = cur_vec;
        new_bucket.last_seen = first.last_seen.clone();
        self.buckets.insert(1, new_bucket);
    }

    /// Get the next node we need to refresh
    pub fn next_refresh_node(&self) -> Option<(NodeId, SocketAddr, Duration)> {  
        let bucket = self.buckets.iter().min_by_key(|bucket| bucket.last_seen )?;
        let node = bucket.nodes.iter().min_by_key(|node| node.last_seen )?;
        match node.last_seen.elapsed() {
            Ok(duration) => return Some((node.id, node.ip, duration)),
            Err(_) => return None, // Why it happen?, the system clock changed?
        }
    }

    /// Get the num of the nodes in the table
    pub fn nodes_len(&self) -> usize {
        let mut len = 0;
        for bucket in self.buckets.iter() {
            len += bucket.nodes.len();
        }
        return len;
    }
}


#[cfg(test)]
mod tests {
    use std::net::{IpAddr, Ipv4Addr};

    use super::*;

    #[test]
    fn test_routing_table_basic() {
        let id = NodeId::new([1; 20]);
        let mut table = RoutingTable::new(id);
        
        // Test empty table
        assert_eq!(table.buckets.len(), 1);
        assert!(table.find_closest_nodes(NodeId::new([2; 20])).is_empty());

        // Test adding nodes
        let addr = SocketAddr::new(IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1)), 8000);
        let node_id = NodeId::new([2; 20]);
        assert!(table.update_node(node_id, &addr).is_ok());

        // Test finding added node
        let closest = table.find_closest_nodes(node_id);
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
            let test_addr = SocketAddr::new(IpAddr::V4(Ipv4Addr::new(127, 0, 0, i as u8)), 8000);
            if i < KBUCKET_SIZE {
                let test_id = NodeId::new([i as u8; 20]);
                assert!(table.update_node(test_id, &test_addr).is_ok());
            }
            else {
                let mut test_id = id.clone();
                test_id[19] = 0;
                // assert!(matches!(table.update_node(test_id, &test_addr), Err(UpdateNodeError::NeedSplit)));
            }
        }

        // Test duplicate IP error
        let dup_id = NodeId::new([99; 20]);
        assert!(matches!(table.update_node(dup_id, &addr), Err(UpdateNodeError::Mismatch)));
    }

    #[test]
    fn test_bucket_split() {
        let id = NodeId::new([1; 20]);
        let mut table = RoutingTable::new(id);

        // Fill first bucket
        for i in 0..KBUCKET_SIZE {
            let test_id = NodeId::new([(i + 1) as u8; 20]);
            let addr = SocketAddr::new(IpAddr::V4(Ipv4Addr::new(127, 0, 0, (i + 1) as u8)), 8000);
            assert!(table.update_node(test_id, &addr).is_ok());
        }

        // Trigger split
        let split_id = NodeId::new([0xFF; 20]);
        let split_addr = SocketAddr::new(IpAddr::V4(Ipv4Addr::new(127, 0, 0, 99)), 8000);
        assert!(matches!(table.update_node(split_id, &split_addr), Err(UpdateNodeError::NeedSplit)));
        table.split_bucket();
        assert_eq!(table.buckets.len(), 2);
    }
}
