SELECT name FROM torrents WHERE name IS NOT NULL AND name != '';

SELECT
    t.hash
FROM
    torrents t
LEFT JOIN
    torrent_tags tt ON t.hash = tt.torrent_hash
WHERE
    tt.torrent_hash IS NULL;