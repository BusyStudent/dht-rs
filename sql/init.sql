CREATE TABLE IF NOT EXISTS `schema_versions` (
    `version` INTEGER NOT NULL PRIMARY KEY,
    `name` TEXT NOT NULL,
    `applied_at` DATETIME NOT NULL DEFAULT CURRENT_TIMESTAMP
);

CREATE TABLE IF NOT EXISTS `torrents` (
    `hash` BLOB(20) NOT NULL PRIMARY KEY, 
    `name` TEXT NOT NULL,
    `size` BIGINT NOT NULL,
    `data` BLOB NOT NULL,
    `files` BLOB NOT NULL,
    `added` DATETIME NOT NULL DEFAULT CURRENT_TIMESTAMP
);

CREATE TABLE IF NOT EXISTS `tags` (
    `id` INTEGER PRIMARY KEY AUTOINCREMENT,
    `name` TEXT NOT NULL UNIQUE
);

CREATE TABLE IF NOT EXISTS `torrent_tags` (
    `torrent_hash` BLOB(20) NOT NULL,
    `tag_id` INTEGER NOT NULL,
    
    PRIMARY KEY (`torrent_hash`, `tag_id`),

    FOREIGN KEY (`torrent_hash`) REFERENCES `torrents`(`hash`) ON DELETE CASCADE,
    FOREIGN KEY (`tag_id`) REFERENCES `tags`(`id`) ON DELETE CASCADE
);

CREATE VIRTUAL TABLE IF NOT EXISTS torrents_fts USING fts5(
    name,
    content='torrents',
    content_rowid='rowid',
    tokenize='trigram'
);

DROP TRIGGER IF EXISTS torrents_ai; 
CREATE TRIGGER torrents_ai AFTER INSERT ON torrents BEGIN
    INSERT INTO torrents_fts(rowid, name) VALUES (new.rowid, new.name);
END;


DROP TRIGGER IF EXISTS torrents_ad;
CREATE TRIGGER torrents_ad AFTER DELETE ON torrents BEGIN
    INSERT INTO torrents_fts(torrents_fts, rowid, name) VALUES ('delete', old.rowid, old.name);
END;


DROP TRIGGER IF EXISTS torrents_au;
CREATE TRIGGER torrents_au AFTER UPDATE ON torrents BEGIN
    INSERT INTO torrents_fts(torrents_fts, rowid, name) VALUES ('delete', old.rowid, old.name);
    INSERT INTO torrents_fts(rowid, name) VALUES (new.rowid, new.name);
END;