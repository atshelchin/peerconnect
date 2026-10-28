-- The public trial's rate limit (docs/self-serve.md, "The public trial"):
-- trial tokens per address per window. The address is kept only as an HMAC.
CREATE TABLE trial_limits (
  key TEXT PRIMARY KEY,                  -- HMAC(APP_SECRET_KEY, address)
  window_start INTEGER NOT NULL,         -- epoch seconds
  count INTEGER NOT NULL
);
