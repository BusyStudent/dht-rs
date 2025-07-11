#![allow(dead_code)] // Let it shutup!
use rusqlite::{Connection, params, Result};
use serde::{Serialize, Deserialize};
use tracing::error;
use std::{sync::Mutex};

use crate::{Torrent, InfoHash};

pub use rusqlite::Error;

// The storage for store the torrent data
pub struct Storage {
    conn: Mutex<Connection>,
}

pub struct TorrentInfo {
    pub hash: InfoHash, // The hash of the torrent
    pub name: String, // The name of the torrent
    pub size: u64, // The size of the torrent (in bytes)
    pub files: Vec<TorrentFile> // The files in the torrent (name, size)
}

#[derive(Serialize, Deserialize)]
pub struct TorrentFile {
    pub name: String, // The name of the file
    pub size: u64, // The size of the file (in bytes)
}

/// The search result for the torrent search
pub struct TorrentInfoList {
    pub torrents: Vec<TorrentInfo>,
    pub total: u64, // The total number of torrents matching the query
}

impl Storage {
    pub fn open(path: &str) -> Result<Storage> {
        let conn = Connection::open(path)?;
        let _ = conn.execute_batch(include_str!("../sql/init.sql"))?; // Do basic init if needed

        return Ok(Storage {
            conn: Mutex::new(conn)
        });
    }

    pub fn add_torrent(&self, torrent: &Torrent) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        let info = TorrentInfo {
            hash: torrent.info_hash(),
            name: torrent.name().into(),
            size: torrent.length(),
            files: torrent.files().into_iter().filter_map(|(name, size)| {
                if name.contains("_____padding_file_") || name.contains(".____padding_file_") { // Junk files
                    return None;
                }
                return Some(TorrentFile {
                    name: name,
                    size: size
                });
            }).collect()
        };
        let files = serde_json::to_string(&info.files).unwrap();

        // Store the data...
        conn.execute(
            "INSERT OR REPLACE INTO torrents (hash, name, size, data, files) VALUES (?1, ?2, ?3, ?4, ?5)", 
            params![
                info.hash.as_slice(),
                info.name.as_str(),
                info.size,
                torrent.object().encode().as_slice(),
                files
            ]
        )?;

        return Ok(());
    }

    /// Get the torrent data from the database
    pub fn get_torrent(&self, hash: InfoHash) -> Result<Vec<u8> > {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare("SELECT data FROM torrents WHERE hash = ?1")?;
        let mut rows = stmt.query(params![hash.as_slice()])?;
        let row = rows.next()?.unwrap();
        let data = row.get(0)?;

        return Ok(data);
    }

    /// Check if the torrent is already in the database
    pub fn has_torrent(&self, hash: InfoHash) -> Result<bool> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare("SELECT 1 FROM torrents WHERE hash = ?1")?;
        let mut rows = stmt.query(params![hash.as_slice()])?;
        return Ok(rows.next()?.is_some());
    }

    /// Get the number of torrents in the database
    pub fn torrents_len(&self) -> Result<u64> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare("SELECT COUNT(*) FROM torrents")?;
        let mut rows = stmt.query(params![])?;
        let row = rows.next()?.unwrap();
        let count = row.get(0)?;

        return Ok(count);
    }

    /// Search the torrent from the database
    pub fn search_torrent(&self, query: &str, offset: u64, limit: u64) -> Result<TorrentInfoList> {
        let sql_search = "
            SELECT
                t.hash, t.name, t.size, t.files,
                COUNT(*) OVER () AS total_count
            FROM torrents t
            JOIN torrents_fts fts ON t.rowid = fts.rowid
            WHERE fts.name MATCH ?1
            ORDER BY rank
            LIMIT ?2 OFFSET ?3
        ";
        let sql_all = "
            SELECT
                hash, name, size, files,
                COUNT(*) OVER () AS total_count
            FROM torrents
            ORDER BY added DESC
            LIMIT ?1 OFFSET ?2
        ";
        let sql_like = "
            SELECT hash, name, size, files,
                   COUNT(*) OVER () AS total_count
            FROM torrents
            WHERE name LIKE '%' || ?1 || '%'
            ORDER BY added DESC
            LIMIT ?2 OFFSET ?3
        ";
        let trimmed_query = query.trim();
        let escape_query = Self::escape_fts_query(trimmed_query);
        let conn = self.conn.lock().unwrap();

        let (sql, param) = match trimmed_query {
            "*" => (sql_all, params![limit, offset]), // Search all torrents
            _ => {
                if trimmed_query.chars().count() < 3 || escape_query.is_none() {
                    (sql_like, params![query, limit, offset]) // Bad with FTS, use LIKE instead
                }
                else {
                    (sql_search, params![query, limit, offset]) // Use FTS
                }
            }
        };

        let mut stmt = conn.prepare(sql)?;
        let rows = stmt.query(param)?;

        return Self::collect_results(rows);
    }

    fn escape_fts_query(input: &str) -> Option<String> {
        // Reject if it contains characters that cause MATCH to panic
        if input.contains(|c: char| c == '"' || c == ':' || c == '*' || c == '-' || c == '[' || c == ']' ) {
            return None;
        }

        return Some(format!("\"{}\"", input))
    }

    fn collect_results(mut rows: rusqlite::Rows<'_>) -> Result<TorrentInfoList> {
        let mut torrents = Vec::new();
        let mut total = 0;

        let mut first_row = true;
        while let Some(row) = rows.next()? {
            if first_row {
                total = row.get(4)?;
                first_row = false;
            }

            let hash: [u8; 20] = row.get(0)?;
            let name: String = row.get(1)?;
            let size: u64 = row.get(2)?;
            let files_json: String = row.get(3)?;

            let files: Vec<TorrentFile> = match serde_json::from_str(files_json.as_str()) {
                Ok(val) => val,
                Err(e) => {
                    error!("Error parsing torrent files: {}, database CORRUPTED???", e);
                    total -= 1; // Skip this row
                    continue;
                }
            };

            torrents.push(TorrentInfo {
                hash: InfoHash::from(hash),
                name: name,
                size: size,
                files: files,
            });
        }

        return Ok(TorrentInfoList {
            torrents: torrents,
            total: total,
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[ignore]
    fn test_storage() {
        let storage = Storage::open("./data/torrents.db").unwrap();
        // let mut file = tokio::fs::File::open("./data/torrents/4ce5c1ec28454f6f0e5c009e74df3a62a9efafa8.torrent").await?;
        // let mut bytes = Vec::new();
        // file.read_to_end(&mut bytes).await?;
        // let torrent = Torrent::from_bytes(&bytes).unwrap();

        // storage.add_torrent(&torrent).await.unwrap();
        assert!(storage.has_torrent(InfoHash::from_hex("4ce5c1ec28454f6f0e5c009e74df3a62a9efafa8").unwrap()).unwrap());
        assert!(storage.has_torrent(InfoHash::zero()).unwrap() == false);
    }
}