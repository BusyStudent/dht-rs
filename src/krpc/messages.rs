#![allow(dead_code)] // Let it shutup!

use super::{InfoHash, NodeId, Object };
use std::{collections::BTreeMap, net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr}};

pub trait Message {
    fn to_bencode(&self) -> Object;
    fn from_bencode(obj: &Object) -> Option<Self> where Self: Sized;
}

pub trait Query : Message {

}

#[derive(Debug, PartialEq, Eq)]
pub struct FindNodeQuery {
    id: NodeId, // Which node send this query?
    target: NodeId, // The target node id want to find
}

#[derive(Debug, PartialEq, Eq)]
pub struct FindNodeReply {
    id: NodeId,
    nodes: Vec<(NodeId, SocketAddr)>,
}

#[derive(Debug, PartialEq, Eq)]
pub struct PingQuery {
    id: NodeId,
}

#[derive(Debug, PartialEq, Eq)]
pub struct PingReply {
    id: NodeId,
}

#[derive(Debug, PartialEq, Eq)]
pub struct GetPeersQuery {
    id: NodeId,
    info_hash: InfoHash,
}

#[derive(Debug, PartialEq, Eq)]
pub struct GetPeersReply {
    id: NodeId,
    token: Vec<u8>,
    nodes: Vec<(NodeId, SocketAddr)>,
    values: Vec<SocketAddr>,
}

fn parse_node_id(id: &Vec<u8>) -> Option<NodeId> {
    let id = match id.as_slice().try_into() {
        Ok(id) => id,
        Err(_) => return None,
    };
    return Some(id);
}

/// Parse the compressed node string (NodeId 20 bytes, ipv4(4) or ipv6(16) addr, 2 bytes port)
fn parse_nodes(mut slice: &[u8]) -> Option<Vec<(NodeId, SocketAddr)> > {
    let v4;
    if slice.len() % 26 == 0 {
        v4 = true;
    }
    else if slice.len() % 38 == 0 {
        v4 = false;
    }
    else {
        return None;
    }

    let mut vec = Vec::new();
    while !slice.is_empty() {
        // Parse the id first
        if slice.len() < 20 {
            return None;
        }
        let id: NodeId = match slice[0..20].try_into() {
            Ok(id) => id,
            Err(_) => return None,
        };
        slice = &slice[20..];
        // Check how many bytes we left
        let ip = if v4 {
            if slice.len() < 6 {
                return None;
            }
            let addr: [u8; 4] = match slice[0..4].try_into() {
                Ok(addr) => addr,
                Err(_) => return None,
            };
            let port: [u8; 2] = match slice[4..6].try_into() {
                Ok(port) => port,
                Err(_) => return None,
            };
            slice = &slice[6..];
            
            SocketAddr::new(IpAddr::V4(Ipv4Addr::from(addr)), u16::from_be_bytes(port))
        }
        else {
            if slice.len() < 18 {
                return None;
            }
            let addr: [u8; 16] = match slice[0..16].try_into() {
                Ok(addr) => addr,
                Err(_) => return None,
            };
            let port: [u8; 2] = match slice[16..18].try_into() {
                Ok(port) => port,
                Err(_) => return None,
            };
            slice = &slice[18..];

            SocketAddr::new(IpAddr::V6(Ipv6Addr::from(addr)), u16::from_be_bytes(port))
        };

        vec.push((id, ip));
    }

    return Some(vec);
}

/// Parse the compressed ip string ipv4(4) or ipv6(16) addr, 2 bytes port
fn parse_ip(slice: &[u8]) -> Option<SocketAddr> {
    match slice.len() {
        6 => {
            let addr: [u8; 4] = match slice[0..4].try_into() {
                Ok(addr) => addr,
                Err(_) => return None,
            };
            let port: [u8; 2] = match slice[4..6].try_into() {
                Ok(port) => port,
                Err(_) => return None,
            };
            return Some(SocketAddr::new(IpAddr::V4(Ipv4Addr::from(addr)), u16::from_be_bytes(port)));
        },
        18 => {
            let addr: [u8; 16] = match slice[0..16].try_into() {
                Ok(addr) => addr,
                Err(_) => return None,
            };
            let port: [u8; 2] = match slice[16..18].try_into() {
                Ok(port) => port,
                Err(_) => return None,
            };
            return Some(SocketAddr::new(IpAddr::V6(Ipv6Addr::from(addr)), u16::from_be_bytes(port)));
        },
        _ => return None
    }
}

fn encode_ip_to(out: &mut Vec<u8>, ip: &SocketAddr) {
    match ip {
        SocketAddr::V4(v4) => {
            out.extend_from_slice(v4.ip().octets().as_slice());
            out.extend_from_slice(v4.port().to_be_bytes().as_slice());
        },
        SocketAddr::V6(v6) => {
            out.extend_from_slice(v6.ip().octets().as_slice());
            out.extend_from_slice(v6.port().to_be_bytes().as_slice());
        },
    }
}

// Encode the (NodeId 20 bytes, ipv4(4) or ipv6(16) addr, 2 bytes port) to the vec's back
fn encode_nodes_to(out: &mut Vec<u8>, nodes: &[(NodeId, SocketAddr)]) {
    for (id, ip) in nodes {
        out.extend_from_slice(id.as_slice());
        encode_ip_to(out, ip);
    }
}

// FindNode...
impl Message for FindNodeQuery {
    fn to_bencode(&self) -> Object {
        // {"id" : "<querying nodes id>", "target" : "<id of target node>"}
        let dict = BTreeMap::from([
            (b"id".to_vec(),     Object::from(self.id.as_slice())),
            (b"target".to_vec(), Object::from(self.target.as_slice())),
        ]);
        return Object::from(dict);
    }

    fn from_bencode(obj: &Object) -> Option<Self> {
        let dict = obj.as_dict()?;
        let id = dict.get(b"id".as_slice())?.as_string()?;
        let target = dict.get(b"target".as_slice())?.as_string()?;

        return Some(Self {
            id: parse_node_id(id)?,
            target: parse_node_id(target)?,
        });
    }
}

impl Message for FindNodeReply {
    fn to_bencode(&self) -> Object {
        // {"id" : "<querying nodes id>", "nodes" : "<compressed nodes>"} ipv4 format
        // {"id" : "<querying nodes id>", "nodes6" : "<compressed nodes>"} ipv6 format
        let mut nodes = Vec::new();
        let v4 = if self.nodes.len() > 0 {
            self.nodes[0].1.is_ipv4()
        }
        else {
            true // Emm, empty list, default to v4
        };
        encode_nodes_to(&mut nodes, &self.nodes);
        let mut dict = BTreeMap::from([
            (b"id".to_vec(), Object::from(self.id.as_slice())),
        ]);
        if v4 {
            dict.insert(b"nodes".to_vec(), Object::from(nodes));
        }
        else {
            dict.insert(b"nodes6".to_vec(), Object::from(nodes));
        }
        return Object::from(dict);
    }

    fn from_bencode(obj: &Object) -> Option<Self> {
        let dict = obj.as_dict()?;
        let id = dict.get(b"id".as_slice())?.as_string()?;
        let mut nodes = dict.get(b"nodes".as_slice());
        if nodes.is_none() {
            nodes = dict.get(b"nodes6".as_slice());
        }
        let nodes = nodes?.as_string()?;

        return Some(Self {
            id: parse_node_id(id)?,
            nodes: parse_nodes(nodes)?,
        });
    }
}

/// Ping...
impl Message for PingQuery {
    fn to_bencode(&self) -> Object {
        // {"id" : "<querying nodes id>"}
        let dict = BTreeMap::from([
            (b"id".to_vec(), Object::from(self.id.as_slice())),
        ]);
        return Object::from(dict);
    }

    fn from_bencode(obj: &Object) -> Option<Self> {
        let dict = obj.as_dict()?;
        let id = dict.get(b"id".as_slice())?.as_string()?;

        return Some(Self {
            id: parse_node_id(id)?,
        });
    }
}

impl Message for PingReply {
    fn to_bencode(&self) -> Object {
        // {"id" : "<querying nodes id>"}
        let dict = BTreeMap::from([
            (b"id".to_vec(), Object::from(self.id.as_slice())),
        ]);
        return Object::from(dict);
    }

    fn from_bencode(obj: &Object) -> Option<Self> {
        let dict = obj.as_dict()?;
        let id = dict.get(b"id".as_slice())?.as_string()?;

        return Some(Self {
            id: parse_node_id(id)?,
        });
    }
}

/// GetPeers...
impl Message for GetPeersQuery {
    fn to_bencode(&self) -> Object {
        // {"id" : "<querying nodes id>", "info_hash" : "<info hash>"}
        let dict = BTreeMap::from([
            (b"id".to_vec(), Object::from(self.id.as_slice())),
            (b"info_hash".to_vec(), Object::from(self.info_hash.as_slice())),
        ]);
        return Object::from(dict);
    }

    fn from_bencode(obj: &Object) -> Option<Self> {
        let dict = obj.as_dict()?;
        let id = dict.get(b"id".as_slice())?.as_string()?;
        let info_hash = dict.get(b"info_hash".as_slice())?.as_string()?;

        return Some(Self {
            id: parse_node_id(id)?,
            info_hash: parse_node_id(info_hash)?,
        });
    }
}

impl Message for GetPeersReply {
    fn to_bencode(&self) -> Object {
        // {"id" : "<queried nodes id>", "token" :"<opaque write token>",  "nodes" : "<compact node info>", "values" : ["<peer 1 info string>", "<peer 2 info string>"]}
        let mut nodes = Vec::new();
        let v4 = if self.nodes.len() > 0 {
            self.nodes[0].1.is_ipv4()
        }
        else {
            true // Emm, empty list, default to v4
        };
        encode_nodes_to(&mut nodes, &self.nodes);

        let mut dict = BTreeMap::from([
            (b"id".to_vec(), Object::from(self.id.as_slice())),
            (b"token".to_vec(), Object::from(self.token.as_slice())),
        ]);

        if v4 {
            dict.insert(b"nodes".to_vec(), Object::from(nodes));
        }
        else {
            dict.insert(b"nodes6".to_vec(), Object::from(nodes));
        }
        if !self.values.is_empty() {
            let mut values = Vec::new();
            for ip in &self.values {
                let mut buf = Vec::new();
                encode_ip_to(&mut buf, ip);
                values.push(Object::from(buf));
            }
            dict.insert(b"values".to_vec(), Object::from(values));
        }
        return Object::from(dict);
    }

    fn from_bencode(obj: &Object) -> Option<Self> {
        let dict = obj.as_dict()?;
        let id = dict.get(b"id".as_slice())?.as_string()?;
        let token = dict.get(b"token".as_slice())?.as_string()?;
        let values = dict.get(b"values".as_slice());
        let mut nodes = dict.get(b"nodes".as_slice());
        if nodes.is_none() {
            nodes = dict.get(b"nodes6".as_slice());
        }

        let mut out_nodes = Vec::new();
        if let Some(val) = nodes {
            let nodes = val.as_string()?;
            out_nodes = parse_nodes(nodes)?;
        }

        let mut out_values = Vec::new();
        if let Some(val) = values {
            for value in val.as_list()? {
                let ip = value.as_string()?;
                let ip = parse_ip(ip)?;
                out_values.push(ip);
            }
        }

        return Some(Self {
            id: parse_node_id(id)?,
            token: token.to_vec(),
            nodes: out_nodes,
            values: out_values,
        });
    }
}

#[cfg(test)]
mod tests {
    use ctor::ctor;

    use super::*;

    #[ctor]
    fn init_color_backtrace() {
        color_backtrace::install();
    }

    fn encode_and_parse<T>(msg: T) where T: Message + std::fmt::Debug + PartialEq {
        let obj = msg.to_bencode();
        let back = T::from_bencode(&obj);
        assert!(back.is_some(), "Failed to parse back the object");
        assert_eq!(msg, back.unwrap(), "Failed to parse back the object, not eq");
    }

    #[test]
    fn test_find_node_query() {
        let id = NodeId::new([1u8; 20]);
        let target = NodeId::new([2u8; 20]);
        let msg = FindNodeQuery { id, target };
        encode_and_parse(msg);
    }

    #[test]
    fn test_find_node_reply() {
        let id = NodeId::new([1u8; 20]);
        let nodes = vec![
            (NodeId::new([2u8; 20]), SocketAddr::from(([127, 0, 0, 1], 6881))),
            (NodeId::new([3u8; 20]), SocketAddr::from(([127, 0, 0, 1], 6882))),
        ];
        let msg = FindNodeReply { id, nodes };
        encode_and_parse(msg);
    }

    #[test]
    fn test_ping_query() {
        let id = NodeId::new([1u8; 20]);
        let msg = PingQuery { id };
        encode_and_parse(msg);
    }

    #[test]
    fn test_ping_reply() {
        let id = NodeId::new([1u8; 20]);
        let msg = PingReply { id };
        encode_and_parse(msg);
    }

    #[test]
    fn test_get_peers_query() {
        let id = NodeId::new([1u8; 20]);
        let info_hash = InfoHash::new([2u8; 20]);
        let msg = GetPeersQuery { id, info_hash };
        encode_and_parse(msg);
    }

    #[test]
    fn test_get_peers_reply() {
        let id = NodeId::new([1u8; 20]);
        let nodes = vec![
            (NodeId::new([2u8; 20]), SocketAddr::from(([127, 0, 0, 1], 6881))),
            (NodeId::new([3u8; 20]), SocketAddr::from(([127, 0, 0, 1], 6882))),
        ];
        let values = vec![
            SocketAddr::from(([127, 0, 0, 1], 6881)),
            SocketAddr::from(([127, 0, 0, 1], 6882))
        ];
        let msg = GetPeersReply {
            id: id,
            token: b"token".to_vec(),
            nodes: nodes,
            values: values
        };
        encode_and_parse(msg);
    }
}