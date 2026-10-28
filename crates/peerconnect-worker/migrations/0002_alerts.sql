-- Abuse alerts (docs/self-serve.md, "Metering and alerts"): a client's use
-- on one UTC day crossing a threshold, written once per client, day and kind
-- by its Client object as it counts.
CREATE TABLE alerts (
  at INTEGER NOT NULL,                   -- epoch ms
  day TEXT NOT NULL,                     -- YYYY-MM-DD, UTC
  client TEXT NOT NULL,
  kind TEXT NOT NULL,                    -- connections | bytes | rejections | limited
  value INTEGER NOT NULL,                -- what the day had reached
  threshold INTEGER NOT NULL,
  PRIMARY KEY (day, client, kind)
);
CREATE INDEX alerts_at ON alerts (at);
