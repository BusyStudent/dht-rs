use std::net::{Ipv4Addr, Ipv6Addr, SocketAddr, SocketAddrV4, SocketAddrV6};

use libc::{sockaddr, c_int};
// FUCK WINDOWS
#[cfg(target_os = "windows")]
use windows_sys::Win32::Networking::WinSock::{
    AF_INET, AF_INET6, SOCKADDR_IN as sockaddr_in, SOCKADDR_IN6 as sockaddr_in6,
};

#[cfg(target_os = "linux")]
use libc::{AF_INET, AF_INET6, sockaddr_in, sockaddr_in6};


#[derive(Clone, Copy)]
pub enum OsSocketAddr {
    V4(sockaddr_in),
    V6(sockaddr_in6),
}

pub unsafe fn rs_addr_to_c(addr: &SocketAddr) -> OsSocketAddr {
    match addr {
        SocketAddr::V4(v4) => {
            let mut addr: sockaddr_in = std::mem::zeroed();
            addr.sin_family = AF_INET as u16;
            addr.sin_port = v4.port().to_be();

            #[cfg(target_os = "windows")]
            {
                addr.sin_addr.S_un.S_addr = v4.ip().to_bits();
            }

            #[cfg(target_os = "linux")]
            {
                addr.sin_addr.s_addr = v4.ip().to_bits();
            }

            return OsSocketAddr::V4(addr);
        },
        SocketAddr::V6(v6) => {
            let mut addr: sockaddr_in6 = std::mem::zeroed();
            addr.sin6_family = AF_INET6 as u16;
            addr.sin6_port = v6.port().to_be();
            addr.sin6_flowinfo = v6.flowinfo();

            #[cfg(target_os = "windows")]
            {
                addr.Anonymous.sin6_scope_id = v6.scope_id();
                addr.sin6_addr.u.Byte = v6.ip().octets();
            }

            #[cfg(target_os = "linux")]
            {
                addr.sin6_scope_id = v6.scope_id();
                addr.sin6_addr.s6_addr = v6.ip().octets();
            }

            return OsSocketAddr::V6(addr);
        },
    }
}

pub unsafe fn c_addr_to_rs(addr: *const libc::sockaddr) -> SocketAddr {
    match (*addr).sa_family {
        AF_INET => {
            let addr = &*(addr as *const sockaddr_in);
            let bits;

            #[cfg(target_os = "windows")]
            {
                bits = addr.sin_addr.S_un.S_addr;
            }

            #[cfg(target_os = "linux")]
            {
                bits = addr.sin_addr.s_addr;
            }

            let v4addr = Ipv4Addr::from_bits(bits);
            let port = u16::from_be(addr.sin_port);

            return SocketAddr::V4(SocketAddrV4::new(v4addr, port));
        },
        AF_INET6 => {
            let addr = &*(addr as *const sockaddr_in6);
            let sin6_scope_id;
            let octets;

            #[cfg(target_os = "windows")]
            {
                octets = addr.sin6_addr.u.Byte;
                sin6_scope_id = addr.Anonymous.sin6_scope_id;
            }

            #[cfg(target_os = "linux")]
            {
                octets = addr.sin6_addr.s6_addr;
                sin6_scope_id = addr.sin6_scope_id;
            }

            let v6addr = Ipv6Addr::from(octets);
            let port = u16::from_be(addr.sin6_port);
            let flowinfo = addr.sin6_flowinfo;

            return SocketAddr::V6(SocketAddrV6::new(v6addr, port, flowinfo, sin6_scope_id));
        },
        _ => panic!("Unknown address family"),
    }
}

// For OsSocketAddr
impl OsSocketAddr {
    pub fn to_ptr_len(&self) -> (*const sockaddr, c_int) {
        match self {
            OsSocketAddr::V4(addr) => {
                return (addr as *const sockaddr_in as *const sockaddr, std::mem::size_of::<sockaddr_in>() as c_int);
            },
            OsSocketAddr::V6(addr) => {
                return (addr as *const sockaddr_in6 as *const sockaddr, std::mem::size_of::<sockaddr_in6>() as c_int);
            },
        }
    }
}