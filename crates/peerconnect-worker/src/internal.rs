//! The relay's internal API (`docs/self-serve.md`, "Issuance: only the relay
//! signs"): the site asks, the relay mints. The token secret never leaves the
//! relay.
//!
//! Reached over a Service Binding from the site's Worker, and authenticated
//! besides by `PEERCONNECT_SERVICE_SECRET` in `x-peerconnect-service`.
//! Without that secret configured, or with any other value, every path here
//! answers `404`, as an unknown path does.
//!
//! - `GET  /internal/clients/<id>`         the client's generation and app
//! - `GET  /internal/clients/<id>/usage`   the days not yet written out,
//!   today's included
//! - `POST /internal/clients/<id>/mint`    `{exp, app?, actor}` → the token
//!   at the current generation (the same token every time)
//! - `POST /internal/clients/<id>/rotate`  `{exp, actor}` → a token at the
//!   next generation; the old one stops, and its connections close
//! - `POST /internal/clients/<id>/revoke`  `{actor}`: every token stops
//! - `POST /internal/clients/<id>/delete`  `{actor}`: for good
//! - `GET  /internal/usage?days=30`        finished days' usage, per client
//! - `GET  /internal/audit?limit=100`      the latest issuance audit rows

use peerconnect::token::{Claims, Keys, is_id};
use serde_json::{Value, json};
use worker::*;

use crate::client::{CLIENT_HEADER, Issued};

/// The header carrying the service secret.
const SERVICE_HEADER: &str = "x-peerconnect-service";
/// The longest validity a minted token may have: a little over a year.
const MAX_EXP_SECONDS: u64 = 400 * 24 * 60 * 60;

pub(crate) async fn handle(mut req: Request, env: &Env, keys: Option<&Keys>) -> Result<Response> {
    let Some(keys) = keys.filter(|_| authorized(&req, env)) else {
        return Response::error("not found\n", 404);
    };
    match req.path().as_str() {
        "/internal/usage" => return usage_days(&req, env).await,
        "/internal/audit" => return audit_log(&req, env).await,
        _ => {}
    }
    let path = req.path();
    let mut parts = path.trim_start_matches("/internal/clients/").splitn(2, '/');
    let (Some(id), action) = (parts.next(), parts.next()) else {
        return Response::error("not found\n", 404);
    };
    if !is_id(id) {
        return Response::error("not a client ID", 400);
    }
    let body: Value = if req.method() == Method::Post {
        match req.json().await {
            Ok(body) => body,
            Err(_) => return Response::error("the body must be JSON", 400),
        }
    } else {
        Value::Null
    };
    let actor = body.get("actor").and_then(Value::as_str).unwrap_or("").to_owned();
    if req.method() == Method::Post && actor.is_empty() {
        return Response::error("say who asked: `actor`", 400);
    }
    let now = Date::now().as_millis() / 1000;
    let exp = body.get("exp").and_then(Value::as_u64).filter(|exp| *exp > now && *exp - now <= MAX_EXP_SECONDS);
    const BAD_EXP: &str = "`exp` must be in the future, within 400 days";
    let app = body.get("app").and_then(Value::as_str);
    if app.is_some_and(|app| !is_id(app)) {
        return Response::error("`app` is not an application ID", 400);
    }
    let object = env.durable_object("CLIENTS")?.get_by_name(id)?;
    let ask = |what: &str, payload: &Value| -> Result<Request> {
        let headers = Headers::new();
        headers.set(CLIENT_HEADER, id)?;
        let method = if payload.is_null() { Method::Get } else { Method::Post };
        let body = (!payload.is_null()).then(|| payload.to_string().into());
        Request::new_with_init(
            &format!("https://client/internal/{what}"),
            RequestInit::new().with_method(method).with_headers(headers).with_body(body),
        )
    };
    match (req.method(), action) {
        (Method::Get, None) => object.fetch_with_request(ask("state", &Value::Null)?).await,
        (Method::Get, Some("usage")) => object.fetch_with_request(ask("usage", &Value::Null)?).await,
        (Method::Post, Some("mint")) => {
            let Some(exp) = exp else { return Response::error(BAD_EXP, 400) };
            let answer = object.fetch_with_request(ask("mint", &json!({ "exp": exp, "app": app }))?).await?;
            signed(answer, keys, env, id, &actor, "mint").await
        }
        (Method::Post, Some("rotate")) => {
            let Some(exp) = exp else { return Response::error(BAD_EXP, 400) };
            let answer = object.fetch_with_request(ask("rotate", &json!({ "exp": exp }))?).await?;
            signed(answer, keys, env, id, &actor, "rotate").await
        }
        (Method::Post, Some(action @ ("revoke" | "delete"))) => {
            let answer = object.fetch_with_request(ask(action, &json!({}))?).await?;
            if answer.status_code() == 200 {
                audit(env, id, action, None, &actor).await;
            }
            Ok(answer)
        }
        _ => Response::error("not found\n", 404),
    }
}

/// Whether the request carries the service secret. Compared in constant time.
fn authorized(req: &Request, env: &Env) -> bool {
    let expected = crate::setting(env, "PEERCONNECT_SERVICE_SECRET");
    // A short or absent secret leaves the internal API closed.
    if expected.len() < peerconnect::token::MIN_SECRET_LEN {
        return false;
    }
    let offered = req.headers().get(SERVICE_HEADER).ok().flatten().unwrap_or_default();
    offered.len() == expected.len() && offered.bytes().zip(expected.bytes()).fold(0u8, |acc, (a, b)| acc | (a ^ b)) == 0
}

/// The object's credential, signed. A new one is audited as `action`; one
/// shown again as `reveal`: the same credential, not a new one.
async fn signed(mut answer: Response, keys: &Keys, env: &Env, id: &str, actor: &str, action: &str) -> Result<Response> {
    if answer.status_code() != 200 {
        return Ok(answer);
    }
    let issued: Issued = answer.json().await?;
    let token = keys.sign(&Claims {
        app: issued.app.clone(),
        exp: Some(issued.exp),
        generation: Some(issued.generation),
        ..Claims::new(id)
    });
    audit(env, id, if issued.new { action } else { "reveal" }, Some(issued.generation), actor).await;
    Response::from_json(&json!({ "token": token, "gen": issued.generation, "exp": issued.exp, "app": issued.app }))
}

/// A query parameter as a number within `1..=max`, else `default`.
fn bounded(req: &Request, name: &str, default: u32, max: u32) -> u32 {
    req.url()
        .ok()
        .and_then(|url| url.query_pairs().find(|(k, _)| k == name).and_then(|(_, v)| v.parse::<u32>().ok()))
        .filter(|n| (1..=max).contains(n))
        .unwrap_or(default)
}

/// Finished days' exact usage per client (written by the Client objects
/// after midnight), newest first. Today's are in each client's object.
async fn usage_days(req: &Request, env: &Env) -> Result<Response> {
    let days = bounded(req, "days", 30, 400);
    let since =
        peerconnect::usage::day_of(Date::now().as_millis().saturating_sub(u64::from(days) * 24 * 60 * 60 * 1000));
    let rows = env
        .d1("RELAY_DB")?
        .prepare("SELECT * FROM usage_daily WHERE day >= ?1 ORDER BY day DESC, client LIMIT 10000")
        .bind(&[since.into()])?
        .all()
        .await?
        .results::<Value>()?;
    Response::from_json(&rows)
}

/// The latest issuance audit rows, newest first.
async fn audit_log(req: &Request, env: &Env) -> Result<Response> {
    let limit = bounded(req, "limit", 100, 1000);
    let rows = env
        .d1("RELAY_DB")?
        .prepare("SELECT at, client, action, gen, actor FROM issuance_audit ORDER BY at DESC LIMIT ?1")
        .bind(&[f64::from(limit).into()])?
        .all()
        .await?
        .results::<Value>()?;
    Response::from_json(&rows)
}

/// The issuance audit: what was minted, shown again, rotated, revoked or
/// deleted, for which client, at whose request. A failure to write is logged.
async fn audit(env: &Env, client: &str, action: &str, generation: Option<u64>, actor: &str) {
    let Ok(db) = env.d1("RELAY_DB") else {
        console_log!("peerconnect: no RELAY_DB: {action} of {client} not audited");
        return;
    };
    let values: Vec<wasm_bindgen::JsValue> = vec![
        (Date::now().as_millis() as f64).into(),
        client.into(),
        action.into(),
        generation.map_or(wasm_bindgen::JsValue::NULL, |g| (g as f64).into()),
        actor.into(),
    ];
    let written = match db
        .prepare("INSERT INTO issuance_audit (at, client, action, gen, actor) VALUES (?1, ?2, ?3, ?4, ?5)")
        .bind(&values)
    {
        Ok(statement) => statement.run().await.map(|_| ()),
        Err(e) => Err(e),
    };
    if let Err(e) = written {
        console_error!("peerconnect: the issuance audit was not written: {e}");
    }
}
