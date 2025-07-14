#![allow(dead_code)]
use std::{fmt, net::SocketAddr, sync::OnceLock};

/// Wrapper a type that can be used for both IPv4 and IPv6.
pub struct DualStack<T> {
    v4: OnceLock<T>,
    v6: OnceLock<T>,
}

impl<T> DualStack<T> {
    pub fn new() -> Self {
        return Self {
            v4: OnceLock::new(),
            v6: OnceLock::new(),
        }
    }

    /// Get the instance of the type for the given IP address.
    pub fn select(&self, ip: &SocketAddr) -> Option<&T> {
        match *ip {
            SocketAddr::V4(_) => return self.v4.get(),
            SocketAddr::V6(_) => return self.v6.get(),
        }
    }

    pub fn v4(&self) -> Option<&T> {
        return self.v4.get();
    }

    pub fn v6(&self) -> Option<&T> {
        return self.v6.get();
    }

    /// Set the v4 instance of the type.
    pub fn set_v4(&self, v4: T) {
        if self.v4.set(v4).is_err() {
            panic!("The set_v4 only can be called once");
        }
    }

    /// Set the v6 instance of the type.
    pub fn set_v6(&self, v6: T) {
        if self.v6.set(v6).is_err() {
            panic!("The set_v6 only can be called once");
        }
    }
    
    /// Get all instances of the type.
    pub fn all(&self) -> Vec<&T> {
        let mut ret = Vec::new();
        if let Some(v4) = self.v4.get() {
            ret.push(v4);
        }
        if let Some(v6) = self.v6.get() {
            ret.push(v6);
        }
        return ret;
    }

    /// Check if the DualStack is empty.
    pub fn is_empty(&self) -> bool {
        return self.v4.get().is_none() && self.v6.get().is_none();
    }
}

/// Impl Clone if T supports Clone.
impl<T: Clone> Clone for DualStack<T> {
    fn clone(&self) -> Self {
        let val = DualStack::new();
        if let Some(v4) = self.v4.get() {
            val.set_v4(v4.clone());
        }
        if let Some(v6) = self.v6.get() {
            val.set_v6(v6.clone());
        }
        return val;
    }
}

impl<T: fmt::Debug> fmt::Debug for DualStack<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        return f.debug_struct("DualStack")
            .field("v4", &self.v4)
            .field("v6", &self.v6)
            .finish();
    }
}