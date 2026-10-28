-- peerconnect.dev's accounts and applications (docs/self-serve.md, "Who is
-- who"). No token and no app secret is stored here: the relay mints tokens on
-- request, and app secrets are derived from APP_SECRET_KEY.

CREATE TABLE accounts (
  id TEXT PRIMARY KEY,                   -- opaque: a-<20 base32>
  created_at INTEGER NOT NULL,           -- epoch seconds
  last_seen_at INTEGER NOT NULL,
  state TEXT NOT NULL DEFAULT 'active',  -- active | suspended
  client_limit INTEGER NOT NULL DEFAULT 20,
  app_limit INTEGER NOT NULL DEFAULT 10
);

-- How an account signs in. Other providers are new rows, not a new model.
CREATE TABLE identities (
  provider TEXT NOT NULL,                -- github
  provider_user_id TEXT NOT NULL,        -- GitHub's numeric ID (logins change)
  account_id TEXT NOT NULL REFERENCES accounts (id),
  login TEXT NOT NULL,
  avatar_url TEXT NOT NULL DEFAULT '',
  created_at INTEGER NOT NULL,
  PRIMARY KEY (provider, provider_user_id)
);
CREATE INDEX identities_account ON identities (account_id);

-- A developer's application. Its ID is the OAuth client ID and the tokens' `app`.
CREATE TABLE apps (
  id TEXT PRIMARY KEY,                   -- app_<16 base32>
  account_id TEXT NOT NULL REFERENCES accounts (id),
  name TEXT NOT NULL,
  redirect_uris TEXT NOT NULL DEFAULT '[]', -- JSON array, compared exactly
  loopback_path TEXT,                    -- desktop: http://127.0.0.1:<any port><path>
  secret_gen INTEGER NOT NULL DEFAULT 0, -- the app secret is HMAC(APP_SECRET_KEY, id, secret_gen)
  user_limit INTEGER NOT NULL DEFAULT 10000, -- clients its server may issue tokens for
  state TEXT NOT NULL DEFAULT 'active',  -- active | disabled
  created_at INTEGER NOT NULL,
  deleted_at INTEGER
);
CREATE INDEX apps_account ON apps (account_id, deleted_at);

-- Web origins whose pages may exchange a code from the browser (CORS).
CREATE TABLE app_origins (
  app_id TEXT NOT NULL REFERENCES apps (id),
  origin TEXT NOT NULL,
  PRIMARY KEY (app_id, origin)
);
CREATE INDEX app_origins_origin ON app_origins (origin);

-- One per app installation per account, one per user an app's server issued
-- a token for, or made by hand. The ID is the tokens' `sub`.
CREATE TABLE clients (
  id TEXT PRIMARY KEY,                   -- opaque: c-<20 base32>
  account_id TEXT NOT NULL REFERENCES accounts (id),
  app TEXT,                              -- the app ID, or NULL
  installation TEXT,                     -- the app's stable installation ID (sign-in clients)
  external_user TEXT,                    -- HMAC of the app's own user ID (server-issued clients)
  label TEXT NOT NULL,
  created_at INTEGER NOT NULL,
  deleted_at INTEGER
);
CREATE INDEX clients_account ON clients (account_id, deleted_at);
CREATE INDEX clients_app ON clients (app, deleted_at);
CREATE UNIQUE INDEX clients_installation ON clients (account_id, app, installation)
  WHERE deleted_at IS NULL AND installation IS NOT NULL;
CREATE UNIQUE INDEX clients_external_user ON clients (app, external_user)
  WHERE deleted_at IS NULL AND external_user IS NOT NULL;

-- Authorization codes: two minutes, once, bound to a PKCE challenge.
CREATE TABLE auth_codes (
  code_hash TEXT PRIMARY KEY,            -- SHA-256 of the code; the code itself is never stored
  client_id TEXT NOT NULL REFERENCES clients (id),
  account_id TEXT NOT NULL,
  app TEXT NOT NULL,
  redirect_uri TEXT NOT NULL,
  code_challenge TEXT NOT NULL,
  rotate INTEGER NOT NULL,               -- 1: the installation had a client; its token is rotated
  expires_at INTEGER NOT NULL,
  used_at INTEGER
);

-- The account side of the audit (the relay keeps the issuance side).
CREATE TABLE audit (
  at INTEGER NOT NULL,
  account_id TEXT,
  action TEXT NOT NULL,                  -- sign-in | app-created | client-created | token-exchanged | …
  detail TEXT NOT NULL DEFAULT '',
  actor TEXT NOT NULL
);
CREATE INDEX audit_account ON audit (account_id, at);
