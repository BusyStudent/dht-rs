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
        return Torrent::from(
            Object::from([
                (b"info".to_vec(), info)
            ])
        );
    }

    pub fn from_info_bytes(bytes: &[u8]) -> Option<Torrent> {
        let (object, _) = Object::decode(bytes)?;
        return Torrent::from_info(object);
    }

    pub fn from(data: Object) -> Option<Torrent> {
        let torrent = Torrent { data: data };
        // Begin check
        torrent.info_checked()?;
        torrent.name_checked()?;
        torrent.piece_length_checked()?;

        // In Single file torrent length exists, in multi file torrent files exists
        if torrent.length_checked().is_none() && torrent.files_checked().is_none() {
            return None;
        }
        // End check
        return Some(torrent);
    }

    pub fn from_bytes(bytes: &[u8]) -> Option<Torrent> {
        let (object, _) = Object::decode(bytes)?;
        return Torrent::from(object);
    }

    /// Get the info hash of the torrent
    pub fn info_hash(&self) -> InfoHash {
        let info = self.info_checked().unwrap(); // Using unwrap because we already checked it in from()
        let encoded = info.encode();
        let hash: [u8; 20] = Sha1::digest(&encoded).into();
        return InfoHash::from(hash);
    }

    /// Get the name of the torrent
    pub fn name(&self) -> &str {
        return self.name_checked().unwrap();
    }

    /// Get the piece length of the torrent
    pub fn piece_length(&self) -> u64 {
        return self.piece_length_checked().unwrap();
    }

    /// Get the length of the torrent (if multi file torrent, we will sum up the length of all files)
    pub fn length(&self) -> u64 {
        let files = self.files_checked();
        match files {
            None => return self.length_checked().unwrap(),
            Some(val) => {
                let mut len = 0;
                for (_, length) in val {
                    len += length;
                }
                return len;
            }
        }
    }

    /// Get the files of the torrent (if single file torrent, we will return the name and length of the file)
    pub fn files(&self) -> Vec<(String, u64)> {
        let files = self.files_checked();
        match files {
            Some(val) => return val,
            None => return vec![(self.name().into(), self.length_checked().unwrap())],
        }
    }

    /// Get the torrent object
    pub fn object(&self) -> &Object {
        return &self.data;
    }

    /// Check version
    fn info_checked(&self) -> Option<&Object> {
        return self.data.get(b"info");
    }

    fn name_checked(&self) -> Option<&str> {
        let info = self.info_checked()?;
        let name = info
            .get(b"name.utf-8") // Historical issues :(
            .or_else(|| info.get(b"name"))?
            .as_string()?;
        match std::str::from_utf8(name) {
            Ok(val) => return Some(val),
            Err(_) => return None,
        }
    }

    fn piece_length_checked(&self) -> Option<u64> {
        let len = self.info_checked()?.get(b"piece length")?.as_int()?.clone();
        match len.try_into() {
            Ok(val) => return Some(val),
            Err(_) => return None,
        }
    }

    fn length_checked(&self) -> Option<u64> {
        let len = self.info_checked()?.get(b"length")?.as_int()?.clone();
        match len.try_into() {
            Ok(val) => return Some(val),
            Err(_) => return None,
        }
    }

    fn files_checked(&self) -> Option<Vec<(String, u64)> > {
        let files = self.info_checked()?.get(b"files")?.as_list()?;
        let mut vec = Vec::new();
        for file in files {
            let length = file
                .get(b"length")?
                .as_int()?;
            let paths = file
                .get(b"path.utf-8")
                .or_else(|| file.get(b"path"))?
                .as_list()?;
            let mut path = String::new();

            for path_obj in paths {
                let path_str = path_obj.as_string()?.as_slice();
                let utf8 = match std::str::from_utf8(path_str) {
                    Ok(val) => val,
                    Err(_) => return None,
                };
                path.push_str(utf8);
                path.push('/');
            }
            if path.ends_with('/') {
                path.pop();
            }

            let length: u64 = match (*length).try_into() {
                Ok(val) => val,
                Err(_) => return None,
            };
            vec.push((path, length));
        }
        return Some(vec);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{fs::File, io::Read};

    #[test]
    fn test_torrent_basic() {
        // Create a basic torrent with minimal info
        let info = Object::from([
            (b"name".to_vec(), Object::from(b"test.txt")),
            (b"piece length".to_vec(), Object::from(16384i64)),
            (b"length".to_vec(), Object::from(1000i64)),
        ]);

        let torrent = Torrent::from_info(info).unwrap();
        assert_eq!(torrent.name(), "test.txt");
        assert_eq!(torrent.piece_length(), 16384);
        assert_eq!(torrent.length(), 1000);
    }

    #[test]
    #[ignore]
    fn test_local_file() {
        let mut file = File::open("./data/torrents/4ce5c1ec28454f6f0e5c009e74df3a62a9efafa8.torrent").unwrap(); // Why this file is not working?
        let mut buf = Vec::new();
        file.read_to_end(&mut buf).unwrap();

        let object = Object::parse(&buf).unwrap();
        tracing::trace!("{:?}", object);
        let torrent = Torrent::from(object).unwrap();
        tracing::trace!("{:?}", torrent);
    }
}