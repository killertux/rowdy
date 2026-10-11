SELECT u.name, 
  FROM orders o
  JOIN users u ON u.id = o.user_id;
