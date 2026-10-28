//! A client's Durable Object (`docs/self-serve.md`, "Tokens on the relay"):
//! one per token subject, beside the Room objects.
//!
//! It decides whether a token is still good (its generation), admits or
//! refuses connections (the rate limit, the trial's allowance), counts the
//! client's use exactly per UTC day, writes finished days to the relay's
//! database, and closes the client's connections when its token is rotated
//! or the client deleted. It holds no sockets: the rooms do, and they report
//! here.
//!
//! Requests come from this Worker alone: from `fetch` (`/admit`), from Room
//! objects (`/usage`), and from the internal API (`/internal/…`).

use peerconnect::token::{Generation, Refusal, Window};
use peerconnect::usage::{Day, Thresholds, Traffic, day_of, next_day_ms, split_days};
use serde::{Deserialize, Serialize};
use serde_json::json;
use worker::*;

/// The header naming the client, set by this Worker: an object does not
/// know its own name.
pub(crate) const CLIENT_HEADER: &str = "x-peerconnect-client";
/// The application of the built-in public trial (`docs/self-serve.md`, "The
/// public trial").
pub(crate) const TRIAL_APP: &str = "trial";

const ID: &str = "id";
const APP: &str = "app";
const GENERATION: &str = "gen";
const ADMITS: &str = "admits";
const ROOMS: &str = "rooms";
/// One key per UTC day still to be written out.
const DAY_PREFIX: &str = "usage:";

/// New connections per client per minute; the trial's, and its bytes a day.
const ADMITS_PER_MINUTE: u32 = 120;
const TRIAL_ADMITS_PER_MINUTE: u32 = 20;
const TRIAL_BYTES_PER_DAY: u64 = 50_000_000;
const MINUTE_MS: u64 = 60_000;
/// At most this many new generations per client per hour.
const ROTATIONS_PER_HOUR: u32 = 30;
const HOUR_MS: u64 = 60 * MINUTE_MS;
/// The rooms whose connections a rotation closes: the most recent ones.
const MAX_ROOMS: usize = 64;
/// A minute after midnight, so a day's last reports are in.
const AFTER_MIDNIGHT_MS: u64 = 60_000;

/// What `fetch` asks before letting a connection through (`lib.rs`).
#[derive(Deserialize)]
pub(crate) struct Admit {
    #[serde(rename = "gen")]
    pub generation: u64,
    pub app: Option<String>,
    pub room: String,
}

/// What a room reports: a connection's traffic and the time from `from` to `to`.
#[derive(Serialize, Deserialize)]
pub(crate) struct Report {
    pub traffic: Traffic,
    pub from: u64,
    pub to: u64,
}

/// One credential as the object hands it out: what to sign.
#[derive(Serialize, Deserialize)]
pub(crate) struct Issued {
    #[serde(rename = "gen")]
    pub generation: u64,
    pub exp: u64,
    pub app: Option<String>,
    /// Issued just now, or the current credential shown again.
    pub new: bool,
}

#[durable_object]
pub struct Client {
    state: State,
    env: Env,
}

impl DurableObject for Client {
    fn new(state: State, env: Env) -> Self {
        Client { state, env }
    }

    async fn fetch(&self, mut req: Request) -> Result<Response> {
        if let Some(id) = req.headers().get(CLIENT_HEADER)? {
            self.name(&id).await?;
        }
        let path = req.path();
        let now = now();
        match path.as_str() {
            "/admit" => {
                let admit: Admit = req.json().await?;
                self.admit(admit, now).await
            }
            "/usage" => {
                let report: Report = req.json().await?;
                self.report(report).await?;
                Response::empty()
            }
            "/internal/state" => {
                let generation = self.generation().await;
                let app = self.app().await;
                Response::from_json(&json!({ "gen": generation.current, "deleted": generation.deleted, "app": app }))
            }
            "/internal/usage" => Response::from_json(&self.pending().await?),
            "/internal/mint" => {
                let body: serde_json::Value = req.json().await?;
                let exp = body.get("exp").and_then(serde_json::Value::as_u64).unwrap_or(0);
                let app = body.get("app").and_then(serde_json::Value::as_str).map(str::to_owned);
                self.mint(exp, app).await
            }
            "/internal/rotate" => {
                let body: serde_json::Value = req.json().await?;
                let exp = body.get("exp").and_then(serde_json::Value::as_u64).unwrap_or(0);
                let mut generation = self.generation().await;
                match generation.rotate(ROTATIONS_PER_HOUR, HOUR_MS, now) {
                    Ok(_) => {}
                    Err(refusal) => return refused(refusal),
                }
                self.state.storage().put(GENERATION, &generation).await?;
                self.close_connections(&generation).await;
                self.mint(exp, None).await
            }
            "/internal/revoke" => {
                let mut generation = self.generation().await;
                // A revocation is not limited: it only ever takes access away.
                if generation.deleted {
                    return refused(Refusal::Deleted);
                }
                generation.current += 1;
                generation.exp = None;
                self.state.storage().put(GENERATION, &generation).await?;
                self.close_connections(&generation).await;
                Response::from_json(&json!({ "gen": generation.current }))
            }
            "/internal/delete" => {
                let mut generation = self.generation().await;
                generation.delete();
                self.state.storage().put(GENERATION, &generation).await?;
                self.close_connections(&generation).await;
                Response::from_json(&json!({ "deleted": true }))
            }
            _ => Response::error("not found", 404),
        }
    }

    /// Writes out every finished day, and wakes again after the next midnight
    /// if there is more to write.
    async fn alarm(&self) -> Result<Response> {
        let today = day_of(now());
        let id = self.id().await;
        let mut left = false;
        for (key, day) in self.days().await? {
            if day.day >= today {
                left = true;
                continue;
            }
            // Kept until written: a failed write is tried again tomorrow.
            match write_day(&self.env, &id, &day).await {
                Ok(()) => {
                    self.state.storage().delete(&key).await?;
                }
                Err(e) => {
                    console_error!("peerconnect: usage of {id} on {} not written: {e}", day.day);
                    left = true;
                }
            }
        }
        if left {
            self.state.storage().set_alarm((next_day_ms(now()) + AFTER_MIDNIGHT_MS) as i64).await?;
        }
        Response::empty()
    }
}

impl Client {
    async fn name(&self, id: &str) -> Result<()> {
        let known: Option<String> = self.state.storage().get(ID).await.ok().flatten();
        if known.as_deref() != Some(id) {
            self.state.storage().put(ID, id).await?;
        }
        Ok(())
    }

    async fn id(&self) -> String {
        self.state.storage().get(ID).await.ok().flatten().unwrap_or_default()
    }

    async fn app(&self) -> Option<String> {
        self.state.storage().get(APP).await.ok().flatten()
    }

    async fn generation(&self) -> Generation {
        self.state.storage().get(GENERATION).await.ok().flatten().unwrap_or_default()
    }

    async fn admit(&self, admit: Admit, now: u64) -> Result<Response> {
        let generation = self.generation().await;
        let app = self.app().await;
        // A token names the application its client was issued for; a client
        // issued here keeps it.
        let app_matches = app.is_none() || admit.app == app;
        if !generation.accepts(admit.generation) || !app_matches {
            self.count(now, |d| d.rejections += 1).await?;
            return Response::error("token revoked or replaced", 403);
        }
        let trial = admit.app.as_deref() == Some(TRIAL_APP);
        let mut admits: Window = self.state.storage().get(ADMITS).await.ok().flatten().unwrap_or_default();
        let per_minute = if trial { TRIAL_ADMITS_PER_MINUTE } else { ADMITS_PER_MINUTE };
        let allowed = admits.take(per_minute, MINUTE_MS, now);
        self.state.storage().put(ADMITS, &admits).await?;
        let over_allowance = trial && self.today(now).await.bytes() >= TRIAL_BYTES_PER_DAY;
        if !allowed || over_allowance {
            self.count(now, |d| d.limited += 1).await?;
            return Response::error("too many connections", 429);
        }
        let mut rooms: Vec<String> = self.state.storage().get(ROOMS).await.ok().flatten().unwrap_or_default();
        rooms.retain(|r| *r != admit.room);
        rooms.push(admit.room);
        let excess = rooms.len().saturating_sub(MAX_ROOMS);
        rooms.drain(..excess);
        self.state.storage().put(ROOMS, &rooms).await?;
        self.count(now, |d| d.connections += 1).await?;
        Response::empty()
    }

    async fn report(&self, report: Report) -> Result<()> {
        // Traffic goes on the day it was reported; time on the days it spans.
        self.count(report.to, |d| d.traffic(report.traffic)).await?;
        for (day, ms) in split_days(report.from, report.to) {
            self.count_on(day, |d| d.connected_ms += ms).await?;
        }
        Ok(())
    }

    async fn mint(&self, exp: u64, app: Option<String>) -> Result<Response> {
        let stored = self.app().await;
        let app = match (stored, app) {
            (Some(stored), Some(asked)) if stored != asked => {
                return Response::error("this client belongs to another application", 409);
            }
            (Some(stored), _) => Some(stored),
            (None, Some(asked)) => {
                self.state.storage().put(APP, &asked).await?;
                Some(asked)
            }
            (None, None) => None,
        };
        let mut generation = self.generation().await;
        let credential = match generation.credential(exp) {
            Ok(credential) => credential,
            Err(refusal) => return refused(refusal),
        };
        if credential.new {
            self.state.storage().put(GENERATION, &generation).await?;
        }
        Response::from_json(&Issued {
            generation: credential.generation,
            exp: credential.exp,
            app,
            new: credential.new,
        })
    }

    /// Tells each recent room to close this client's connections from
    /// before `generation` (all of them, if the client is deleted).
    async fn close_connections(&self, generation: &Generation) {
        let id = self.id().await;
        let rooms: Vec<String> = self.state.storage().get(ROOMS).await.ok().flatten().unwrap_or_default();
        let Ok(namespace) = self.env.durable_object("ROOMS") else { return };
        let body = json!({ "client": id, "gen": generation.current, "deleted": generation.deleted }).to_string();
        for room in rooms {
            let request = Request::new_with_init(
                &format!("https://room/internal/kick/{room}"),
                RequestInit::new().with_method(Method::Post).with_body(Some(body.clone().into())),
            );
            let (Ok(request), Ok(stub)) = (request, namespace.get_by_name(&room)) else { continue };
            if let Err(e) = stub.fetch_with_request(request).await {
                console_error!("peerconnect: room {room} not told to close {id}'s connections: {e}");
            }
        }
    }

    async fn today(&self, now: u64) -> Day {
        let day = day_of(now);
        self.state.storage().get(&format!("{DAY_PREFIX}{day}")).await.ok().flatten().unwrap_or_else(|| Day::new(day))
    }

    async fn count(&self, at: u64, f: impl FnOnce(&mut Day)) -> Result<()> {
        self.count_on(day_of(at), f).await
    }

    /// Applies `f` to `day`'s counts and keeps them, with an alarm to write
    /// them out once the day is over.
    async fn count_on(&self, day: String, f: impl FnOnce(&mut Day)) -> Result<()> {
        let key = format!("{DAY_PREFIX}{day}");
        let mut counts: Day = self.state.storage().get(&key).await.ok().flatten().unwrap_or_else(|| Day::new(day));
        let before = counts.clone();
        f(&mut counts);
        self.state.storage().put(&key, &counts).await?;
        let crossed = thresholds(&self.env).crossed(&before, &counts);
        if !crossed.is_empty() {
            let id = self.id().await;
            for (kind, value, threshold) in crossed {
                alert(&self.env, &counts.day, &id, kind, value, threshold).await;
            }
        }
        if !matches!(self.state.storage().get_alarm().await, Ok(Some(_))) {
            self.state.storage().set_alarm((next_day_ms(now()) + AFTER_MIDNIGHT_MS) as i64).await?;
        }
        Ok(())
    }

    async fn days(&self) -> Result<Vec<(String, Day)>> {
        let stored = self.state.storage().list_with_options(ListOptions::new().prefix(DAY_PREFIX)).await?;
        let mut days = Vec::new();
        for entry in stored.entries() {
            let entry = entry?;
            let pair: js_sys::Array = entry.into();
            let (Some(key), Ok(day)) = (pair.get(0).as_string(), serde_wasm_bindgen::from_value::<Day>(pair.get(1)))
            else {
                continue;
            };
            days.push((key, day));
        }
        days.sort_by(|a, b| a.1.day.cmp(&b.1.day));
        Ok(days)
    }

    /// The days not yet written out, today's included, oldest first.
    async fn pending(&self) -> Result<Vec<Day>> {
        Ok(self.days().await?.into_iter().map(|(_, day)| day).collect())
    }
}

/// The alert thresholds: `ALERT_CONNECTIONS`, `ALERT_BYTES`,
/// `ALERT_REJECTIONS`, `ALERT_LIMITED` (per client per day; 0 turns one off),
/// else the defaults.
fn thresholds(env: &Env) -> Thresholds {
    let defaults = Thresholds::default();
    let get = |name: &str, default: u64| crate::setting(env, name).parse().unwrap_or(default);
    Thresholds {
        connections: get("ALERT_CONNECTIONS", defaults.connections),
        bytes: get("ALERT_BYTES", defaults.bytes),
        rejections: get("ALERT_REJECTIONS", defaults.rejections),
        limited: get("ALERT_LIMITED", defaults.limited),
    }
}

/// Records that `client` crossed a threshold today. Once per client, day and
/// kind (the table's key); best effort, like the rest of metering.
async fn alert(env: &Env, day: &str, client: &str, kind: &str, value: u64, threshold: u64) {
    console_log!("peerconnect: alert: {client} {kind} {value} >= {threshold} on {day}");
    let Ok(db) = env.d1("RELAY_DB") else { return };
    let values: Vec<wasm_bindgen::JsValue> = vec![
        (now() as f64).into(),
        day.into(),
        client.into(),
        kind.into(),
        (value as f64).into(),
        (threshold as f64).into(),
    ];
    let written = match db
        .prepare(
            "INSERT OR IGNORE INTO alerts (at, day, client, kind, value, threshold) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        )
        .bind(&values)
    {
        Ok(statement) => statement.run().await.map(|_| ()),
        Err(e) => Err(e),
    };
    if let Err(e) = written {
        console_error!("peerconnect: alert not written: {e}");
    }
}

fn refused(refusal: Refusal) -> Result<Response> {
    match refusal {
        Refusal::Deleted => Response::error("the client is deleted", 410),
        Refusal::TooMany => Response::error("too many new tokens for this client; try again in an hour", 429),
    }
}

/// Writes one finished day. Absolute counts, so writing it twice is harmless.
async fn write_day(env: &Env, client: &str, d: &Day) -> Result<()> {
    let db = env.d1("RELAY_DB")?;
    let values: Vec<wasm_bindgen::JsValue> = vec![
        d.day.as_str().into(),
        client.into(),
        (d.connections as f64).into(),
        (d.connected_ms as f64).into(),
        (d.frames_in as f64).into(),
        (d.frames_out as f64).into(),
        (d.bytes_in as f64).into(),
        (d.bytes_out as f64).into(),
        (d.rejections as f64).into(),
        (d.limited as f64).into(),
    ];
    db.prepare(
        "INSERT OR REPLACE INTO usage_daily (day, client, connections, connected_ms, frames_in, frames_out, \
         bytes_in, bytes_out, rejections, limited) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
    )
    .bind(&values)?
    .run()
    .await?;
    Ok(())
}

fn now() -> u64 {
    Date::now().as_millis()
}
