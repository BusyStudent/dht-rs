#![allow(dead_code)] // Let it shutup!

use std::collections::{BTreeMap, BTreeSet, HashSet, HashMap};
use std::net::SocketAddr;
use std::sync::{Arc, Mutex, MutexGuard, OnceLock, Weak};
use std::time::Duration;
use tokio::{task::JoinSet, sync::RwLock};
use tracing::{debug, error, info, trace, warn};
use thiserror::Error;
use async_trait::async_trait;

use crate::{NodeId, dht::RoutingTable};
use crate::krpc::*;

const KRPC_TIMEOUT_DRUATION: Duration = Duration::from_secs(3); // The default timeout of krpc
const MIN_REFRESH_INTERVAL: Duration = Duration::from_secs(60); // The minimum interval for node refresh
const MIN_CLEAR_INTERVAL: Duration = Duration::from_secs(60 * 15); // The minimum interval for clear the peers announced
const NUM_QUERIES_PER_ITERATION: usize = 3; // The queries of per iterations
const MAX_ITERATIONS_WITH_CONVERGENCE: usize = 3; // The max iterations with convergence
const MAX_ITERATIONS: usize = 16; // The max iterations of the find_node / get_peers
const MAX_ALPHA: usize = 10; // Use more cocorrent requests, to speed up the process
const MAX_PEERS: usize = 100; // The max peers we can get from the get_peers

struct DhtSessionInner {
    routing_table: Mutex<RoutingTable>,
    peers: RwLock<BTreeMap<InfoHash, BTreeSet<SocketAddr> > >, // The peers we know about, indexed by infohash
    krpc: KrpcContext,
    id: NodeId, // The self id
    ipv4: bool, // does it run on ipv4? ,false on ipv6
    timeout: Duration, // The timeout for the all krpc requests
    observer: OnceLock<Weak<dyn DhtSessionObserver + Sync + Send> >
}

#[derive(Clone)]
pub struct DhtSession {
    inner: Arc<DhtSessionInner>
}

#[async_trait]
pub trait DhtSessionObserver {
    async fn on_peer_announce(&self, hash: InfoHash, addr: SocketAddr);
    async fn on_query(&self, method: &[u8], addr: SocketAddr);
}

/// The Error from the find_node
#[derive(Error, Debug)]
pub enum FindNodeError {
    #[error("No node found")]
    AllFailed, // We try to do rpc to the nodes, but all failed, no-one give us reply
}

#[derive(Debug)]
pub struct GetPeersResult {
    pub nodes: Vec<GetPeersNode>, // The nodes we found, id, ip, token
    pub peers: Vec<SocketAddr>
}


#[derive(Debug)]
pub struct GetPeersNode {
    pub id: NodeId,
    pub ip: SocketAddr,
    pub token: Vec<u8>
}

type GetPeersError = FindNodeError;

type NodeEndpoint = (NodeId, SocketAddr);

/// Sort the nodes by distance to the target node, and remove duplicates
/// based on the NodeId.
fn sort_node_and_unique(nodes: &mut Vec<NodeEndpoint>, target: NodeId) {
    nodes.sort_by(|(a, _), (b, _)| {
        let a_dis = *a ^ target;
        let b_dis = *b ^ target;
        return a_dis.cmp(&b_dis);
    });
    nodes.dedup_by(|a, b| a == b);
}

// Extract the query part from the message
// Returns the tid, query method, and the "a" part of the message
fn extract_query_part(msg: &Object) -> Option<(&[u8], &[u8], &Object)> {
    // ping Query = {"t":"aa", "y":"q", "q":"ping", "a":{"id":"abcdefghij0123456789"}}
    let dict = msg.as_dict()?;
    let tid = dict.get(b"t".as_slice())?.as_string()?;
    let y = dict.get(b"y".as_slice())?.as_string()?;
    let q = dict.get(b"q".as_slice())?.as_string()?;
    let a = dict.get(b"a".as_slice())?;
    if y != b"q" {
        return None; // Not a query
    }
    return Some((tid, q, a));
}

impl DhtSession {
    /// Check if the given address is a native address for this session.
    fn is_native_addr(&self, addr: &SocketAddr) -> bool {
        match addr {
            SocketAddr::V4(_) => return self.inner.ipv4,
            SocketAddr::V6(_) => return !self.inner.ipv4,
        }
    }

    fn routing_table_mut(&self) -> MutexGuard<'_, RoutingTable> {
        return self.inner.routing_table.lock().expect("Mutex poisoned");
    }

    pub fn routing_table(&self) -> MutexGuard<'_, RoutingTable> {
        return self.inner.routing_table.lock().expect("Mutex poisoned");
    }

    pub fn new(id: NodeId, krpc: KrpcContext) -> DhtSession {
        let ipv4 = krpc.is_ipv4();
        return DhtSession {
            inner: Arc::new(
                DhtSessionInner {
                    routing_table: Mutex::new(RoutingTable::new(id)),
                    peers: RwLock::new(BTreeMap::new()),
                    krpc: krpc,
                    id: id,
                    ipv4: ipv4,
                    timeout: KRPC_TIMEOUT_DRUATION,
                    observer: OnceLock::new(),
                }
            )
        };
    }

    /// Wrapper for do krpc, manage the life time
    /// 
    /// `id` The node id we are querying, if it is zero, we don't know the id 
    /// 
    /// `ip` The ip address we are querying
    async fn do_krpc<T: KrpcQuery>(self, msg: T, id: NodeId, ip: SocketAddr) -> (Result<T::Reply, KrpcError>, SocketAddr) {
        let mut idx = 0;
        loop {
            trace!("Sending {} KRPC message to {}, Try {}", std::str::from_utf8(T::method_name()).unwrap(), ip, idx);
            let result = self.inner.krpc.send_krpc_with_timeout(&msg, &ip, self.inner.timeout).await;
            if let Err(KrpcError::TimedOut) = result {
                trace!("KRPC request timed out for {}, retrying...", ip);
                idx += 1;
                if idx < 2 {
                    continue;
                }
            }
            if let Err(e) = &result {
                trace!("KRPC request failed for {}: {}", ip, e);
                if !id.is_zero() { // We known the id, mark it as bad
                    self.routing_table_mut().node_timeout(id);
                }
            }
            else if !id.is_zero() { // is OK, we need to update it in routing table
                let _ = self.routing_table_mut().update_node(id, &ip);
            }
            return (result, ip); // Done
        }
    }
    
    /// Doing the find logical of find_node
    async fn find_node_impl(&self, mut queue: Vec<NodeEndpoint>, target: NodeId) -> Result<Vec<NodeEndpoint>, FindNodeError> {
        sort_node_and_unique(&mut queue, target);
        let mut join_set = JoinSet::new();
        let mut visited = HashSet::new();
        let mut collected = Vec::new();
        let mut output = Vec::new();
        let mut iteration_with_convergence = 0;
        let mut queries_completed = 0; // The number of queries completed
        while queries_completed < MAX_ITERATIONS * NUM_QUERIES_PER_ITERATION {
            if queue.is_empty() && join_set.is_empty() {
                warn!("No more nodes to query(find_node), stopping the search.");
                break;
            }
            if join_set.len() < MAX_ALPHA {
                // We can spawn more futures
                let num = std::cmp::min(MAX_ALPHA - join_set.len(), queue.len());

                for (id, ip) in queue.drain(0..num) {
                    // Send the find_node request
                    let msg = FindNodeQuery {
                        id: self.inner.id,
                        target: target,
                    };

                    let fut = self.clone().do_krpc(msg, id, ip);
                    join_set.spawn(fut);
                }
            }

            // Try to wait one future to complete
            let res = match join_set.join_next().await {
                Some(res) => res,
                None => continue,
            };
            // One future completed, check the result
            queries_completed += 1;
            let (result, ip) = match res {
                Ok(what) => what,
                Err(_) => continue, // Set is empty...
            };
            let reply = match result {
                Ok(reply) => reply,
                Err(_) => {
                    visited.insert(ip.clone()); // Mark failed as visited
                    continue;
                }
            };
            // Try add it to the routing table, it give us reply
            let _ = self.routing_table_mut().add_node(reply.id, &ip);
            visited.insert(ip);

            // Update the nodes...
            for (id, ip) in reply.nodes.iter() {
                if !self.is_native_addr(ip) {
                    continue; // Skip the non-native addresses
                }
                if visited.contains(ip) {
                    continue; // Already visited this node
                }
                queue.push((*id, *ip));
            }
            collected.extend_from_slice(reply.nodes.as_slice());
            
            sort_node_and_unique(&mut queue, target);
            sort_node_and_unique(&mut collected, target);

            // Check if the K-nearest nodes are the same, if so more than 3 iterations, we can stop
            let collected_len = collected.len().min(8);
            let len = output.len().min(collected_len);
            if output[0..len] != collected[0..len] || len < 8 { // If the output is not the same as collected(0..K), or we have less than 8 nodes
                // Update the K-nearest nodes
                output.clear();
                output.extend_from_slice(&collected[0..collected_len]);
                iteration_with_convergence = 0; // Reset the convergence counter
            }
            else {
                iteration_with_convergence += 1;
                if iteration_with_convergence >= NUM_QUERIES_PER_ITERATION * MAX_ITERATIONS_WITH_CONVERGENCE {
                    debug!("Convergence reached, stopping the search.");
                    break; // We have convergence, stop the search
                }
            }
            
            // Avoid to grow too large
            queue.truncate(8 * 8);
            collected.truncate(8 * 8);

            trace!("find_node: queries_completed {queries_completed}, iteration_with_convergence {iteration_with_convergence}");
        }
        // Done
        join_set.shutdown().await;
        if output.is_empty() {
            error!("Failed to find any nodes for target: {}", target);
            return Err(FindNodeError::AllFailed);
        }
        debug!("Found {} nodes for target: {}", output.len(), target);
        return Ok(output);
    }

    async fn get_peers_impl(&self, mut queue: Vec<(NodeId, SocketAddr)>, hash: InfoHash) -> Result<GetPeersResult, GetPeersError> {
        let mut peers = Vec::new(); // The peers we found
        let mut collected = Vec::new(); // The nodes we found when get_peers
        let mut nearest = Vec::new(); // The K-nearest nodes
        let mut visited = HashMap::new(); // The nodes we visited, map ip to the token
        let mut join_set = JoinSet::new();
        let mut iteration_with_convergence = 0;
        let mut queries_completed = 0; // The number of queries completed

        // The code as same as find_node_impl
        while queries_completed < MAX_ITERATIONS * NUM_QUERIES_PER_ITERATION {
            if queue.is_empty() && join_set.is_empty() {
                warn!("No more nodes to query(get_peers), stopping the search.");
                break;
            }
            if peers.len() >= MAX_PEERS {
                break;
            }
            if join_set.len() < MAX_ALPHA {
                // We can spawn more futures
                let num = std::cmp::min(MAX_ALPHA - join_set.len(), queue.len());

                for (id, ip) in queue.drain(0..num) {
                    // Send the find_node request
                    let msg = GetPeersQuery {
                        id: self.inner.id,
                        info_hash: hash,
                    };

                    let fut = self.clone().do_krpc(msg, id, ip);
                    join_set.spawn(fut);
                }
            }

            // Try to wait one future to complete
            let res = match join_set.join_next().await {
                Some(res) => res,
                None => continue,
            };
            // One future completed, check the result
            queries_completed += 1;
            let (result, ip) = match res {
                Ok(what) => what,
                Err(_) => continue, // Set is empty...
            };
            let reply = match result {
                Ok(reply) => reply,
                Err(_) => {
                    visited.insert(ip.clone(), Vec::new()); // Mark failed as visited
                    continue;
                }
            };
            // Try add it to the routing table, it give us reply
            let _ = self.routing_table_mut().add_node(reply.id, &ip);
            visited.insert(ip, reply.token);

            // Update the nodes...
            for (id, ip) in reply.nodes.iter() {
                if !self.is_native_addr(ip) {
                    continue; // Skip the non-native addresses
                }
                if visited.contains_key(ip) {
                    continue; // Already visited this node
                }
                queue.push((*id, *ip));
            }
            peers.extend_from_slice(reply.values.as_slice());
            collected.extend_from_slice(reply.nodes.as_slice());
            
            peers.sort();
            peers.dedup();
            sort_node_and_unique(&mut queue, hash);
            sort_node_and_unique(&mut collected, hash);

            // Check if the K-nearest nodes are the same, if so more than 3 iterations, we can stop
            let collected_len = collected.len().min(8);
            let len = nearest.len().min(collected_len);
            if nearest[0..len] != collected[0..len] || len < 8 { // If the nearest is not the same as collected(0..K), or we have less than 8 nodes
                // Update the K-nearest nodes
                nearest.clear();
                nearest.extend_from_slice(&collected[0..collected_len]);
                iteration_with_convergence = 0; // Reset the convergence counter
            }
            else {
                iteration_with_convergence += 1;
                if iteration_with_convergence >= NUM_QUERIES_PER_ITERATION * MAX_ITERATIONS_WITH_CONVERGENCE {
                    trace!("Convergence reached, stopping the search.");
                    break; // We have convergence, stop the search
                }
            }
            
            // Avoid to grow too large
            queue.truncate(8 * 8);
            collected.truncate(8 * 8);

            trace!("get_peers: queries_completed {queries_completed}, iteration_with_convergence {iteration_with_convergence}");
        }

        // Done
        let mut nodes = Vec::new();
        let null = Vec::new();
        for (id, ip) in collected.iter() {
            let token = visited.get(ip).unwrap_or(&null);
            if token.is_empty() {
                continue;
            }
            nodes.push(GetPeersNode { id: *id, ip: *ip, token: token.clone() });
            if nodes.len() >= 8 {
                break;
            }
        }

        join_set.shutdown().await;
        if peers.is_empty() && collected.is_empty() {
            return Err(GetPeersError::AllFailed);
        }
        return Ok(GetPeersResult {
            peers: peers,
            nodes: nodes,
        });
    }

    /// Find the nodes for the target node id, return the K-nearest nodes
    pub async fn find_node(self, target: NodeId) -> Result<Vec<NodeEndpoint>, FindNodeError> {
        // Get the nodes from routing table
        let queue = self.routing_table_mut().find_node(target);
        if queue.is_empty() {
            warn!("No nodes found in the routing table when finding node");
        }
        return self.find_node_impl(queue, target).await;
    }

    /// Get the peers for the given infohash, return the peers and the K-nearest nodes
    pub async fn get_peers(self, hash: InfoHash) -> Result<GetPeersResult, GetPeersError> {
        let queue = self.routing_table_mut().find_node(hash);
        if queue.is_empty() {
            warn!("No nodes found in the routing table when getting peers");
        }
        return self.get_peers_impl(queue, hash).await;
    }

    /// Ping the nodes for the given ips
    pub async fn ping(self, ip: SocketAddr) -> Result<NodeId, KrpcError> {
        let msg = PingQuery {
            id: self.inner.id
        };
        let (reply, _) = self.do_krpc(msg, NodeId::zero(), ip).await;
        return Ok(reply?.id);
    }

    /// Announce the given infohash to the given node
    /// 
    /// `ip` The ip of the node
    /// 
    /// `hash` The infohash to announce
    /// 
    /// `port` The port to announce, if None, use implied port and the default port 6881
    /// 
    /// `token` The token to announce, can't be empty
    pub async fn announce_peer(self, ip: SocketAddr, hash: InfoHash, port: Option<u16>, token: Vec<u8>) -> Result<(), KrpcError> {
        let msg = AnnouncePeerQuery {
            id: self.inner.id,
            info_hash: hash,
            port: port.unwrap_or(6881),
            implied_port: port.is_none(),
            token: token,
        };
        let (_reply, _) = self.do_krpc(msg, NodeId::zero(), ip).await;
        return Ok(());
    }

    /// Sample the infohashes from the given node
    pub async fn sample_infohashes(self, ip: SocketAddr, target: NodeId) -> Result<SampleInfoHashesReply, KrpcError> {
        let msg = SampleInfoHashesQuery {
            id: self.inner.id,
            target: target
        };
        let reply = self.inner.krpc.send_krpc_with_timeout(&msg, &ip, self.inner.timeout).await?;
        return Ok(reply);
    }
    
    /// Process the incoming udp packet
    pub async fn process_udp(&self, bytes: &[u8], ip: &SocketAddr) -> bool {
        let query = match self.inner.krpc.process_udp(bytes, ip) {
            KrpcProcess::Query(query) => query,
            KrpcProcess::Reply => return true,
            KrpcProcess::InvalidMessage => return false,
        };
        let (tid, method, msg) = match extract_query_part(&query) {
            Some(val) => val,
            None => {
                error!("Invalid query format: {:?}", query);
                return false; // Invalid query
            }
        };
        let id = match method {
            b"ping" => {
                let query = match PingQuery::from_bencode(msg) {
                    Some(val) => val,
                    None => return false,
                };
                let reply = PingReply {
                    id: self.inner.id,
                };
                let _ = self.inner.krpc.send_reply(tid, reply, ip).await;

                query.id
            },
            b"find_node" => {
                let query = match FindNodeQuery::from_bencode(msg) {
                    Some(val) => val,
                    None => return false,
                };
                let nodes = self.routing_table_mut().find_node(query.target);
                let reply = FindNodeReply {
                    id: self.inner.id,
                    nodes: nodes,
                };
                let _ = self.inner.krpc.send_reply(tid, reply, ip).await;

                query.id
            },
            b"get_peers" => {
                let query = match GetPeersQuery::from_bencode(msg) {
                    Some(val) => val,
                    None => return false,
                };
                // Find the infohash in the routing table and collect the peers if exists
                let mut values = Vec::new();
                let nodes = self.routing_table_mut().find_node(query.info_hash);
                if let Some(peers) = self.inner.peers.read().await.get(&query.info_hash) {
                    for peer in peers {
                        values.push(peer.clone());
                    }
                }
                if values.len() > 8 {
                    // Randomly select 8 peers
                    fastrand::shuffle(values.as_mut_slice());
                    values.truncate(8);
                }
                let reply = GetPeersReply {
                    id: self.inner.id,
                    nodes: nodes,
                    token: ip.to_string().into_bytes(), // Use the ip as token
                    values: values,
                };
                let _ = self.inner.krpc.send_reply(tid, reply, ip).await;

                query.id
            },
            b"announce_peer" => {
                let query = match AnnouncePeerQuery::from_bencode(msg) {
                    Some(val) => val,
                    None => return false,
                };
                // info!("Collecting infohash {} from {}", query.info_hash, ip);
                if query.token != ip.to_string().into_bytes() {
                    // Invalid token, ignore it
                    error!("Invalid token: {:?}", query.token);
                    let reply = ErrorReply {
                        code: 203, // Invalid token
                        msg: "Invalid token".to_string(),
                    };
                    let _ = self.inner.krpc.send_reply(tid, reply, ip).await;
                    return true; // Still a valid query for process udp
                }
                let mut peer_ip = ip.clone();
                if !query.implied_port {
                    peer_ip.set_port(query.port);
                }

                // Add the peer to the routing table
                {
                    let mut set = self.inner.peers.write().await;
                    set.entry(query.info_hash).or_default().insert(peer_ip);
                } // Avoid the lock across the await point
                let reply = AnnouncePeerReply {
                    id: self.inner.id,
                };
                let _ = self.inner.krpc.send_reply(tid, reply, ip).await;

                // Notify the observer if exists
                if let Some(observer) = self.inner.observer.get().and_then(|o| o.upgrade()) {
                    observer.on_peer_announce(query.info_hash, peer_ip).await;                        
                }

                query.id
            },
            _ => {
                error!("Unknown method: {}", String::from_utf8_lossy(method));
                let reply = ErrorReply {
                    code: 202, // Unknown method
                    msg: format!("Unknown method: {}", String::from_utf8_lossy(method)),
                };
                let _ = self.inner.krpc.send_reply(tid, reply, ip).await;
                return true; // Unknown method, but still a valid query for process udp
            },
        };
        trace!("Received query method {} from {}: {} ", String::from_utf8_lossy(method), id, ip);
        let _ = self.routing_table_mut().update_node(id, ip);

        // Notify the observer if exists
        if let Some(observer) = self.inner.observer.get().and_then(|o| o.upgrade()) {
            observer.on_query(method, ip.clone()).await;
        }
        return true;
    }

    /// Set the observer, NOTE: it only can set once
    pub fn set_observer(&self, observer: Weak<dyn DhtSessionObserver + Sync + Send>) {
        let _ = self.inner.observer.set(observer);
    }

    async fn bootstrap(&self) -> bool {
        let routers = [
            "dht.transmissionbt.com:6881",
            "router.bittorrent.com:6881",
            "router.utorrent.com:6881",
        ];
        // Use the nodes from routing table as init queue (emm, default is 0 nodes)
        let mut queue: Vec<NodeEndpoint> = self.routing_table_mut().iter().collect();
        for each in routers {
            let res = match tokio::net::lookup_host(each).await {
                Ok(res) => res,
                Err(e) => {
                    error!("Failed to lookup host {}: {}", each, e);
                    continue; // Skip this router if lookup fails
                }
            };
            let mut table = self.routing_table_mut();
            for ip in res {
                table.add_router(&ip); // Add this ip as router, to filter 
                if self.is_native_addr(&ip) {
                    debug!("Adding router {} to the queue", ip);
                    queue.push((NodeId::zero(), ip)); // Use zero node id as an placeholder
                }
            }
        }
        // Begin find self
        let _ = match self.find_node_impl(queue, self.inner.id).await {
            Ok(nodes) => nodes,
            Err(_) => {
                error!("Failed to bootstrap, no nodes found");
                return false;
            }
        };
        let mut len = self.routing_table_mut().nodes_len();
        let mut iter = 0;
        if len == 0 {
            // Emm, we have no nodes in the routing table, let run retry
            return false;
        }
        while len <= 50 && iter < 10 { // Doing random search to fill our bucket
            let id = NodeId::rand();
            let nodes = self.routing_table_mut().find_node(id);
            if nodes.is_empty() {
                // No nodes found, try again
                iter += 1;
                continue;
            }
            let _ = self.find_node_impl(nodes, id).await;
            len = self.routing_table_mut().nodes_len();
            iter += 1;
        }
        return true;
    }

    /// Doing to refresh the routing table
    async fn refresh_routing_table(&self) {
        loop {
            let mut ping_set = JoinSet::new();
            if self.routing_table_mut().nodes_len() < 8 {
                // Not enough nodes, try to bootstrap again
                return;
            }
            while let Some((id, ip, duration)) = self.routing_table_mut().next_refresh_node(MIN_REFRESH_INTERVAL) {
                debug!("Refreshing node {} at {}, last seen in {:?}", id, ip, duration);
                let query = FindNodeQuery {
                    id: self.inner.id,
                    target: NodeId::rand(),
                };
                ping_set.spawn(self.clone().do_krpc(query, id, ip));
            }
            let _ = ping_set.join_all().await;

            // Get the buckets indexes which node.len() < K / 2, refresh them
            let mut refresh_set = JoinSet::new();
            for i in self.routing_table_mut().less_node_buckets_indexes(8 / 2) { // Less than HALF K
                // We need to fill the empty buckets, so we can find more nodes
                debug!("Refreshing bucket at index {}", i);
                let id = NodeId::rand_with_prefix(self.inner.id, i);
                refresh_set.spawn(self.clone().find_node(id));
            }
            let _ = refresh_set.join_all().await;

            // Sleep for a while before next refresh
            tokio::time::sleep(MIN_REFRESH_INTERVAL).await;
            // debug dump the routing table
            debug!("Refresh done, routing table {:?}", self.routing_table_mut());
        }
    }

    async fn clear_peers(&self) {
        loop {
            tokio::time::sleep(MIN_CLEAR_INTERVAL).await;
            self.inner.peers.write().await.clear();
            debug!("Clear the peers we have");
        }
    }

    async fn main_loop(&self) {
        // Doing the bootstrap
        loop {
            if !self.bootstrap().await {
                error!("DHT session bootstrap failed, retrying...");
                tokio::time::sleep(std::time::Duration::from_secs(30)).await;
                continue;
            } 
            info!("DHT session bootstrap done");
            self.refresh_routing_table().await;
        }
    }

    pub async fn run(&self) {
        tokio::join!(self.main_loop(), self.clear_peers());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::{io, net};

    #[tokio::test]
    #[ignore]
    async fn test_dht_session() -> io::Result<()> {
        let sockfd = Arc::new(net::UdpSocket::bind("[::0]:11451").await?);
        let ctxt = KrpcContext::new(sockfd.clone());
        let id = NodeId::from_hex("38c14d2d6652a0c1f97db18f67609eceaabe6ce3").unwrap();
        let session = DhtSession::new(id, ctxt);
        let session_ = session.clone();
        
        // Start the DHT session
        let worker = tokio::spawn(async move {
            let mut buffer = [0u8; 65535];
            loop {
                let (len, ip) = match sockfd.recv_from(&mut buffer).await {
                    Ok(res) => res,
                    Err(e) => {
                        error!("Failed to receive from socket: {}", e);
                        continue;
                    }
                };
                // debug!("Received packet from {}", ip);
                session.process_udp(&buffer[0..len], &ip).await;
            }
        });
        session_.run().await;
        let _ = worker.await;
        
        Ok(())
    }
}