#![allow(dead_code)] // Let it shutup!
use crate::bencode::Object;
use crate::InfoHash;
use sha1::{Sha1, Digest};

#[derive(Debug)]
pub struct Torrent {
    data: Object
}

impl Torrent {
    /// Create a torrent from the info dict
    pub fn from_info(info: Object) -> Option<Torrent> {
        let data = Object::from([(b"info".to_vec(), info)]);
        return Some(
            Torrent { data: data }
        );
    }

    /// Get the info dict from torrent
    pub fn info(&self) -> Option<&Object> {
        return self.data.get(b"info");
    }

    /// Get the info hash of the torrent
    pub fn info_hash(&self) -> Option<InfoHash> {
        let info = self.info()?;
        let encoded = info.encode();
        let hash = Sha1::digest(&encoded);
        let array: [u8; 20] = hash.try_into().unwrap();
        return Some(InfoHash::from(array));
    }

    /// Get the name of the torrent
    pub fn name(&self) -> Option<&[u8]> {
        return Some(
            self.info()?.get(b"name")?.as_string()?.as_slice()
        );
    }

    /// Get the piece length of the torrent
    pub fn piece_length(&self) -> Option<u64> {
        let len = self.info()?.get(b"piece length")?.as_int()?.clone();
        match len.try_into() {
            Ok(val) => return Some(val),
            Err(_) => return None,
        }
    }

    /// Get the length of the torrent
    pub fn length(&self) -> Option<u64> {
        let len = self.info()?.get(b"length")?.as_int()?.clone();
        match len.try_into() {
            Ok(val) => return Some(val),
            Err(_) => return None,
        }
    }

    /// Get the torrent object
    pub fn object(&self) -> &Object {
        return &self.data;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_torrent_basic() {
        // Create a basic torrent with minimal info
        let info = Object::from([
            (b"name".to_vec(), Object::from(b"test.txt")),
            (b"piece length".to_vec(), Object::from(16384i64)),
            (b"length".to_vec(), Object::from(1000i64)),
        ]);

        let torrent = Torrent::from_info(info).unwrap();
        assert_eq!(torrent.name().unwrap(), b"test.txt");
        assert_eq!(torrent.piece_length().unwrap(), 16384);
        assert_eq!(torrent.length().unwrap(), 1000);
    }
}