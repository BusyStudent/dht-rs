#![allow(dead_code)] // Let it shutup!
use rusqlite::{Connection, params, Result, Transaction};
use serde::{Serialize, Deserialize};
use tracing::{error};
use std::{sync::Mutex};

use crate::{tags, InfoHash, Torrent};

pub use rusqlite::Error;

// The storage for store the torrent data
pub struct Storage {
    conn: Mutex<Connection>,
}

pub struct TorrentInfo {
    pub hash: InfoHash, // The hash of the torrent
    pub name: String, // The name of the torrent
    pub size: u64, // The size of the torrent (in bytes)
    pub tags: Vec<String>, // The tags of the torrent
    pub files: Vec<TorrentFile>, // The files in the torrent (name, size)
    pub added: String, // The time when the torrent was added
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

const DB_COMPRESS_LEVEL: i32 = 22; // ZSTD MAX_COMPRESSION
const DB_VERSION: i32 = 1;

impl Storage {
    pub fn open(path: &str) -> Result<Storage> {
        let conn = Connection::open(path)?;
        let _ = conn.execute_batch(include_str!("../sql/init.sql"))?; // Do basic init if needed

        let db = Storage {
            conn: Mutex::new(conn)
        };
        if let Err(e) = db.check_and_upgrade_db() { // Upgrade the db if needed
            error!("Failed to upgrade the database: {e}");
            return Err(e);
        }
        return Ok(db);
    }

    /// Add a torrent to the database
    pub fn add_torrent(&self, torrent: &Torrent) -> Result<()> {
        let hash = torrent.info_hash();
        let name = torrent.name();
        let size = torrent.length();
        let files: Vec<TorrentFile> = torrent.files_filtered().into_iter().map(|(name, len)| TorrentFile {
            name: name,
            size: len,
        }).collect();
        
        let data = zstd::encode_all(torrent.encode().as_slice(), DB_COMPRESS_LEVEL).unwrap(); // Emm I think encode never fail, except for OOM
        let files_json = zstd::encode_all(serde_json::to_string(&files).unwrap().as_bytes(), DB_COMPRESS_LEVEL).unwrap();

        // Store the data...
        let mut conn = self.conn.lock().unwrap();

        // Use a transaction to make sure the data is consistent
        let transaction = conn.transaction()?;
        transaction.execute(
            "INSERT OR REPLACE INTO torrents (hash, name, size, data, files) VALUES (?1, ?2, ?3, ?4, ?5)", 
            params![
                hash.as_slice(),
                name,
                size,
                data.as_slice(),
                files_json.as_slice(),
            ]
        )?;

        // Add tags...
        Self::generate_apply_tags(&transaction, hash, name, &files)?;

        // For ACID
        transaction.commit()?;
        return Ok(());
    }

    /// Add a tag to a torrent
    /// 
    /// `tag` must be non-empty
    pub fn add_torrent_tag(&self, hash: InfoHash, tag: &str) -> Result<()> {
        let mut conn = self.conn.lock().unwrap();
        let transaction = conn.transaction()?;
        Self::add_tag_impl(&transaction, hash, tag)?;
        transaction.commit()?;
        return Ok(());
    }

    /// Remove a tag from a torrent
    pub fn remove_torrent_tag(&self, hash: InfoHash, tag: &str) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare("DELETE FROM torrent_tags WHERE torrent_hash = ?1 AND tag_id = (SELECT id FROM tags WHERE name = ?2)")?;
        stmt.execute(params![hash.as_slice(), tag])?;

        return Ok(());
    }

    /// Get the torrent data from the database
    pub fn get_torrent(&self, hash: InfoHash) -> Result<(String, Vec<u8>)> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare("SELECT name, data FROM torrents WHERE hash = ?1")?;
        let mut rows = stmt.query(params![hash.as_slice()])?;
        let row = rows.next()?.unwrap();
        let name: String = row.get(0)?;
        let data: Vec<u8> = row.get(1)?;

        // Decompress the data
        let decompressed = zstd::decode_all(data.as_slice()).unwrap();
        return Ok((name, decompressed));
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
                t.hash, t.name, t.size, t.files, t.added,
                GROUP_CONCAT(tg.name) AS tags,
                COUNT(*) OVER () AS total_count
            FROM torrents t
            JOIN torrents_fts fts ON t.rowid = fts.rowid
            LEFT JOIN torrent_tags tt ON t.hash = tt.torrent_hash
            LEFT JOIN tags tg ON tt.tag_id = tg.id
            WHERE fts.name MATCH ?1
            GROUP BY t.hash
            ORDER BY rank
            LIMIT ?2 OFFSET ?3
        ";
        let sql_all = "
            SELECT
                t.hash, t.name, t.size, t.files, t.added,
                GROUP_CONCAT(tg.name) AS tags,
                COUNT(*) OVER () AS total_count
            FROM torrents t
            LEFT JOIN torrent_tags tt ON t.hash = tt.torrent_hash
            LEFT JOIN tags tg ON tt.tag_id = tg.id
            GROUP BY t.hash
            ORDER BY t.added DESC
            LIMIT ?1 OFFSET ?2
        ";
        let sql_like = "
            SELECT
                t.hash, t.name, t.size, t.files, t.added,
                GROUP_CONCAT(tg.name) AS tags,
                COUNT(*) OVER () AS total_count
            FROM torrents t
            LEFT JOIN torrent_tags tt ON t.hash = tt.torrent_hash
            LEFT JOIN tags tg ON tt.tag_id = tg.id
            WHERE t.name LIKE '%' || ?1 || '%'
            GROUP BY t.hash
            ORDER BY t.added DESC
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

    pub fn all_tags(&self) -> Result<Vec<String>> {
        let mut tags = Vec::new();
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare("SELECT name FROM tags")?;
        let mut rows = stmt.query(params![])?;

        while let Some(row) = rows.next()? {
            let tag: String = row.get(0)?;
            tags.push(tag);
        }
        return Ok(tags);
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
                total = row.get(6)?;
                first_row = false;
            }

            let hash: [u8; 20] = row.get(0)?;
            let name: String = row.get(1)?;
            let size: u64 = row.get(2)?;
            let files_json_data: Vec<u8> = row.get(3)?;
            let added: String = row.get(4)?;
            let tags_str: Option<String> = row.get(5)?;

            let files_json = match zstd::decode_all(files_json_data.as_slice()) {
                Ok(val) => val,
                Err(e) => {
                    error!("Error decompressing torrent files: {}, database CORRUPTED???", e);
                    total -= 1; // Skip this row
                    continue;
                }
            };

            let files: Vec<TorrentFile> = match serde_json::from_slice(&files_json) {
                Ok(val) => val,
                Err(e) => {
                    error!("Error parsing torrent files: {}, database CORRUPTED???", e);
                    total -= 1; // Skip this row
                    continue;
                }
            };

            let tags = tags_str.map_or(Vec::new(), |s| { // A, B, C
                return s.split(',').map(|s| s.to_string()).collect()
            });

            torrents.push(TorrentInfo {
                hash: InfoHash::from(hash),
                name: name,
                size: size,
                tags: tags,
                files: files,
                added: added,
            });
        }

        return Ok(TorrentInfoList {
            torrents: torrents,
            total: total,
        });
    }

    /// Calc the tags for the torrent and store them in the database
    fn generate_apply_tags(t: &Transaction, hash: InfoHash, name: &str, files: &[TorrentFile]) -> Result<()> {
        let mut tags = tags::generate_torrent_tags(name, files);
        if tags.is_empty() {
            tags.push("other".into()); // 
        }
        for tag in tags.iter()
            .map(|t| t.trim())
            .filter(|t| !t.is_empty()) 
        {
            Self::add_tag_impl(t, hash, tag)?;
        }

        return Ok(());
    }

    /// Do the actual work of adding a tag to the database
    fn add_tag_impl(t: &Transaction, hash: InfoHash, tag: &str) -> Result<()> {
        debug_assert!(!tag.is_empty(), "Tag cannot be empty");
        t.execute("INSERT OR IGNORE INTO tags (name) VALUES (?1)", params![tag])?;

        // Get the tag id
        let tag_id: i64 = t.query_row("SELECT id FROM tags WHERE name = ?1", params![tag], |row| row.get(0))?;

        // Insert the tag into the torrent_tags table
        t.execute("INSERT OR IGNORE INTO torrent_tags (torrent_hash, tag_id) VALUES (?1, ?2)", params![hash.as_slice(), tag_id])?;
        return Ok(());
    }

    fn check_and_upgrade_db(&self) -> Result<()> {
        return Ok(());
    }

    /// Get all torrents with no tags
    fn untagged_torrents(&self) -> Result<Vec<InfoHash> > {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare("
            SELECT
                t.hash
            FROM
            torrents t
            LEFT JOIN
            torrent_tags tt ON t.hash = tt.torrent_hash
            WHERE
            tt.torrent_hash IS NULL;
        ")?;
        let mut rows = stmt.query(params![])?;

        let mut torrents = Vec::new();
        while let Some(row) = rows.next()? {
            let hash: [u8; 20] = row.get(0)?;
            torrents.push(InfoHash::from(hash));
        }

        return Ok(torrents);
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

    #[test]
    #[ignore]
    fn test_tags() {
        let storage = Storage::open("./data/torrents.db").unwrap();
        let mut torrents = storage.untagged_torrents().unwrap();

        fastrand::shuffle(&mut torrents);
        torrents.truncate(50); // Max 50 torrents

        for hash in torrents {
            let (_, bytes) = storage.get_torrent(hash).unwrap();
            let torrent = Torrent::from_bytes(&bytes).unwrap();

            let files: Vec<TorrentFile> = torrent.files_filtered().into_iter().map(|(name, size)| TorrentFile {
                name: name,
                size: size,
            }).collect();
            let tags_vec = tags::generate_torrent_tags(torrent.name(), &files);
            // Show it
            tracing::trace!("{}: {:?}", torrent.name(), tags_vec);
        }
    }
}