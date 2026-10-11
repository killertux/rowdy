SELECT id, user_id, status, total_cents, ordered_at
  FROM orders
 WHERE status = :status
   AND total_cents >= :min_cents
 ORDER BY ordered_at DESC;
