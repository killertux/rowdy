#!/usr/bin/env bash
# Called from inside a tape: reset per-project state, preload the editor
# buffer from seeds/<name>.sql, and start rowdy on the `shop` connection.
cd "$ROWDY_SHOTS_WORK/db"
rm -rf .rowdy/sessions .rowdy/queries .rowdy/params .rowdy/chats .rowdy/*.log
mkdir -p .rowdy/sessions/shop
cp "$ROWDY_SHOTS_SEEDS/$1.sql" .rowdy/sessions/shop/session_0.sql
clear
HOME="$ROWDY_SHOTS_WORK/home" exec "$ROWDY_SHOTS_BIN" -c shop
