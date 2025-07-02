// MOD for decode and encode compact ip address
use std::net::{SocketAddr, Ipv4Addr, Ipv6Addr, IpAddr};

/// Parse the compressed ip string ipv4(4) or ipv6(16) addr, 2 bytes port
pub fn parse_ip(slice: &[u8]) -> Option<SocketAddr> {
    match slice.len() {
        6 => {
            let addr: [u8; 4] = slice[0..4].try_into().ok()?;
            let port: [u8; 2] = slice[4..6].try_into().ok()?;
            return Some(SocketAddr::new(IpAddr::V4(Ipv4Addr::from(addr)), u16::from_be_bytes(port)));
        },
        18 => {
            let addr: [u8; 16] = slice[0..16].try_into().ok()?;
            let port: [u8; 2] = slice[16..18].try_into().ok()?;
            return Some(SocketAddr::new(IpAddr::V6(Ipv6Addr::from(addr)), u16::from_be_bytes(port)));
        },
        _ => return None
    }
}

/// Encode the compact ip address
pub fn encode_ip_to(out: &mut Vec<u8>, ip: &SocketAddr) -> usize {
    match ip {
        SocketAddr::V4(v4) => {
            out.extend_from_slice(v4.ip().octets().as_slice());
            out.extend_from_slice(v4.port().to_be_bytes().as_slice());

            return 6;
        }
        SocketAddr::V6(v6) => {
            out.extend_from_slice(v6.ip().octets().as_slice());
            out.extend_from_slice(v6.port().to_be_bytes().as_slice());

            return 18;
        }
    }
}

/// Parse the compressed ip string ipv4(4) or ipv6(16) addr
pub fn parse_your_ip(slice: &[u8]) -> Option<IpAddr> {
    match slice.len() {
        4 => {
            let addr: [u8; 4] = slice.try_into().ok()?;
            return Some(IpAddr::V4(Ipv4Addr::from(addr)));
        }
        16 => {
            let addr: [u8; 16] = slice.try_into().ok()?;
            return Some(IpAddr::V6(Ipv6Addr::from(addr)));
        }
        _ => return None,
    }
}

pub fn encode_your_ip(ip: &IpAddr) -> Vec<u8> {
    match ip {
        IpAddr::V4(v4) => {
            return v4.octets().to_vec();
        }
        IpAddr::V6(v6) => {
            return v6.octets().to_vec();
        }
    }
}