SELECT
    t.hash, t.name
FROM torrents t
WHERE t.name LIKE '%' || ?1 || '%'
ORDER BY LENGTH(name) ASC
LIMIT 30;
