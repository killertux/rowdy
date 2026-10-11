-- Top customers by lifetime spend
SELECT u.name,
       u.email,
       count(o.id)                AS orders,
       sum(o.total_cents) / 100.0 AS spent
  FROM users u
  JOIN orders o ON o.user_id = u.id
 WHERE o.status <> 'refunded'
 GROUP BY u.id
 ORDER BY spent DESC
 LIMIT 10;
