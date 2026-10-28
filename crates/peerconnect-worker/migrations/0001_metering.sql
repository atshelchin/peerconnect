-- The relay's own records (docs/self-serve.md): exact daily usage per
-- client, and the issuance audit. Resource use and actions only: never a
-- token, a room, an address or anything a peer said.

CREATE TABLE usage_daily (
  day TEXT NOT NULL,                     -- YYYY-MM-DD, UTC
  client TEXT NOT NULL,                  -- the opaque client ID (the token's sub)
  connections INTEGER NOT NULL DEFAULT 0,
  connected_ms INTEGER NOT NULL DEFAULT 0,
  frames_in INTEGER NOT NULL DEFAULT 0,
  frames_out INTEGER NOT NULL DEFAULT 0,
  bytes_in INTEGER NOT NULL DEFAULT 0,
  bytes_out INTEGER NOT NULL DEFAULT 0,
  rejections INTEGER NOT NULL DEFAULT 0, -- refused: a rotated or deleted token
  limited INTEGER NOT NULL DEFAULT 0,    -- refused: too many connections, or the trial's allowance
  PRIMARY KEY (day, client)
);
CREATE INDEX usage_daily_client ON usage_daily (client, day);

CREATE TABLE issuance_audit (
  at INTEGER NOT NULL,                   -- epoch ms
  client TEXT NOT NULL,
  action TEXT NOT NULL,                  -- mint | reveal | rotate | revoke | delete
  gen INTEGER,                           -- the generation minted or rotated to
  actor TEXT NOT NULL                    -- who asked, as the site says it
);
CREATE INDEX issuance_audit_client ON issuance_audit (client, at);
