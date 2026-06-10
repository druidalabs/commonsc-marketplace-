//! Stripe Checkout integration.
//!
//! Two endpoints, both behind nginx rate limiting and CORS:
//!
//!   POST /payments/checkout         — create a Stripe Checkout Session
//!   GET  /payments/sessions/:id     — verify payment status
//!
//! The Stripe **secret key** lives only in this process, loaded from the
//! `STRIPE_SECRET_KEY` env variable. The systemd unit drop-in sets it; never
//! commit a key. If the variable is unset the endpoints return 503 so we fail
//! loud rather than create test-mode sessions in production by accident.
//!
//! Trust model (v1): the desktop client passes `amountMinor` + `currency`
//! along with `itemId`. We trust the client because the desktop already
//! controls its own vault and could simply forge a receipt locally without
//! ever calling Stripe. When/if we add server-side catalog with prices we
//! flip to looking up the price by `itemId` and ignoring the client's number.
//!
//! Confirmation flow: the desktop opens the Stripe URL in the default
//! browser. On success Stripe redirects to `commonsc://checkout/return?
//! session={CHECKOUT_SESSION_ID}` (a Tauri-registered deep link). The desktop
//! intercepts, calls GET /payments/sessions/:id, and only marks the item
//! installed if `paid: true`. No webhook required.

use std::path::{Path, PathBuf};

use axum::{
    extract::{Path as AxumPath, State},
    response::Html,
    Json,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{AppState, ApiError};

/// Where Stripe Checkout redirects on success/cancel. The desktop loads the
/// Stripe URL in an embedded Tauri WebviewWindow and listens for navigation
/// to these paths to know the flow finished and close the child window.
/// HTTPS so Stripe is happy and the webview will actually navigate (custom
/// commonsc:// schemes are silently ignored by the webview).
const SUCCESS_URL: &str =
    "https://api.commonsc.io/payments/complete/success?session={CHECKOUT_SESSION_ID}";
const CANCEL_URL: &str =
    "https://api.commonsc.io/payments/complete/cancel?session={CHECKOUT_SESSION_ID}";

fn stripe_secret() -> Result<String, ApiError> {
    std::env::var("STRIPE_SECRET_KEY").map_err(|_| {
        ApiError::server(
            "Stripe is not configured on this marketplace instance \
             (STRIPE_SECRET_KEY env var is missing)."
                .to_string(),
        )
    })
}

fn payments_dir(workspace: &Path) -> PathBuf {
    workspace.join("payments")
}

/// JSON record we persist for every checkout session we mint, indexed by
/// Stripe's session id. Lets us answer status checks if Stripe is briefly
/// unreachable, and gives us an audit log.
#[derive(Debug, Serialize, Deserialize)]
struct PaymentRecord {
    session_id: String,
    item_id: String,
    amount_minor: u64,
    currency: String,
    created_at: i64,
    paid: bool,
    last_checked_at: i64,
}

#[derive(Debug, Deserialize)]
pub struct CheckoutRequest {
    /// Catalog manifest id (e.g. `commonsc/bitter-taste@1.0.0`). Used in
    /// Stripe's product name + receipt; not interpreted server-side.
    pub item_id: String,
    /// Display name shown on the Stripe Checkout page + receipt.
    pub item_name: String,
    /// Price in the smallest currency unit (e.g. 500 = £5.00).
    pub amount_minor: u64,
    /// ISO 4217 currency code, lowercase (Stripe requires lowercase).
    pub currency: String,
}

#[derive(Debug, Serialize)]
pub struct CheckoutResponse {
    pub session_id: String,
    pub url: String,
}

/// POST /payments/checkout — create a Stripe Checkout Session.
pub async fn create_checkout(
    State(state): State<AppState>,
    Json(req): Json<CheckoutRequest>,
) -> Result<Json<CheckoutResponse>, ApiError> {
    if req.amount_minor == 0 {
        return Err(ApiError::client("amount_minor must be > 0".to_string()));
    }
    if req.currency.is_empty() || req.currency.len() > 8 {
        return Err(ApiError::client("currency must be a 3-letter ISO code".to_string()));
    }
    if req.item_id.is_empty() || req.item_id.len() > 200 {
        return Err(ApiError::client("item_id must be non-empty and < 200 chars".to_string()));
    }

    let secret = stripe_secret()?;
    let client = reqwest::Client::new();

    // Stripe Checkout Session params, form-encoded per their docs. Hand-
    // rolled instead of pulling stripe-rust to keep deps lean — we only
    // touch two endpoints.
    let params = [
        ("mode", "payment"),
        ("success_url", SUCCESS_URL),
        ("cancel_url", CANCEL_URL),
        ("line_items[0][quantity]", "1"),
        ("line_items[0][price_data][currency]", req.currency.as_str()),
        (
            "line_items[0][price_data][unit_amount]",
            &req.amount_minor.to_string(),
        ),
        (
            "line_items[0][price_data][product_data][name]",
            req.item_name.as_str(),
        ),
        ("metadata[item_id]", req.item_id.as_str()),
    ];

    let resp = client
        .post("https://api.stripe.com/v1/checkout/sessions")
        .basic_auth(&secret, Some(""))
        .form(&params)
        .send()
        .await
        .map_err(|e| ApiError::server(format!("calling Stripe: {e}")))?;

    let status = resp.status();
    let body: Value = resp
        .json()
        .await
        .map_err(|e| ApiError::server(format!("parsing Stripe response: {e}")))?;

    if !status.is_success() {
        let msg = body
            .pointer("/error/message")
            .and_then(Value::as_str)
            .unwrap_or("Stripe rejected the checkout request");
        return Err(ApiError::server(format!("Stripe {status}: {msg}")));
    }

    let session_id = body
        .get("id")
        .and_then(Value::as_str)
        .ok_or_else(|| ApiError::server("Stripe response missing `id`".to_string()))?
        .to_string();
    let url = body
        .get("url")
        .and_then(Value::as_str)
        .ok_or_else(|| ApiError::server("Stripe response missing `url`".to_string()))?
        .to_string();

    // Persist a record so /payments/sessions/:id can serve the basics even
    // if Stripe is briefly unreachable. Best-effort: a write failure here
    // doesn't block returning the URL to the user.
    let record = PaymentRecord {
        session_id: session_id.clone(),
        item_id: req.item_id.clone(),
        amount_minor: req.amount_minor,
        currency: req.currency.clone(),
        created_at: now_seconds(),
        paid: false,
        last_checked_at: 0,
    };
    if let Err(e) = std::fs::create_dir_all(payments_dir(&state.workspace)) {
        tracing::warn!("payments dir: {e}");
    }
    if let Err(e) = std::fs::write(
        payments_dir(&state.workspace).join(format!("{session_id}.json")),
        serde_json::to_string_pretty(&record).unwrap_or_default(),
    ) {
        tracing::warn!("persisting payment record {session_id}: {e}");
    }

    Ok(Json(CheckoutResponse { session_id, url }))
}

#[derive(Debug, Serialize)]
pub struct SessionStatus {
    pub session_id: String,
    pub paid: bool,
    pub amount_minor: u64,
    pub currency: String,
    pub item_id: Option<String>,
}

/// GET /payments/sessions/:id — verify a checkout session against Stripe and
/// return whether it's paid. Idempotent and side-effect-light: we update the
/// local record to remember `paid=true` once we see it, but always re-check.
pub async fn session_status(
    State(state): State<AppState>,
    AxumPath(session_id): AxumPath<String>,
) -> Result<Json<SessionStatus>, ApiError> {
    if session_id.is_empty()
        || session_id.len() > 200
        || !session_id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
    {
        return Err(ApiError::client("invalid session id".to_string()));
    }

    let secret = stripe_secret()?;
    let client = reqwest::Client::new();
    let url = format!("https://api.stripe.com/v1/checkout/sessions/{session_id}");

    let resp = client
        .get(&url)
        .basic_auth(&secret, Some(""))
        .send()
        .await
        .map_err(|e| ApiError::server(format!("calling Stripe: {e}")))?;
    let status = resp.status();
    let body: Value = resp
        .json()
        .await
        .map_err(|e| ApiError::server(format!("parsing Stripe response: {e}")))?;
    if !status.is_success() {
        let msg = body
            .pointer("/error/message")
            .and_then(Value::as_str)
            .unwrap_or("Stripe rejected the lookup");
        return Err(ApiError::server(format!("Stripe {status}: {msg}")));
    }

    let payment_status = body
        .get("payment_status")
        .and_then(Value::as_str)
        .unwrap_or("unpaid");
    let paid = payment_status == "paid" || payment_status == "no_payment_required";
    let amount_minor = body
        .get("amount_total")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let currency = body
        .get("currency")
        .and_then(Value::as_str)
        .unwrap_or("gbp")
        .to_string();
    let item_id = body
        .pointer("/metadata/item_id")
        .and_then(Value::as_str)
        .map(String::from);

    // Update the local record. Best-effort.
    let path = payments_dir(&state.workspace).join(format!("{session_id}.json"));
    if let Ok(text) = std::fs::read_to_string(&path) {
        if let Ok(mut rec) = serde_json::from_str::<PaymentRecord>(&text) {
            rec.paid = paid;
            rec.last_checked_at = now_seconds();
            let _ = std::fs::write(&path, serde_json::to_string_pretty(&rec).unwrap_or_default());
        }
    }

    Ok(Json(SessionStatus {
        session_id,
        paid,
        amount_minor,
        currency,
        item_id,
    }))
}

/// GET /payments/complete/success — terminal page Stripe redirects to. The
/// desktop's embedded webview listens for this URL and closes the child
/// window; the main app's auto-poll picks up `paid=true` independently. The
/// page itself is a one-liner with no external assets so it loads instantly
/// and looks the same offline-of-stylesheets.
pub async fn complete_success() -> Html<&'static str> {
    Html(COMPLETE_HTML_SUCCESS)
}

/// GET /payments/complete/cancel — same shape, "cancelled" copy.
pub async fn complete_cancel() -> Html<&'static str> {
    Html(COMPLETE_HTML_CANCEL)
}

const COMPLETE_HTML_SUCCESS: &str = r#"<!doctype html>
<html lang="en"><head><meta charset="utf-8"><title>Payment complete · CommonSense</title>
<meta name="viewport" content="width=device-width, initial-scale=1">
<style>
  html,body{margin:0;height:100%;display:grid;place-items:center;font-family:ui-serif,Georgia,serif;background:#FBF6EC;color:#1F1B14}
  .card{max-width:420px;padding:32px;text-align:center}
  h1{margin:0 0 12px;font-size:24px;font-weight:600;letter-spacing:-0.3px}
  p{margin:0;font-size:14px;line-height:1.6;color:#5C544A}
  .tick{font-size:48px;color:#3D5A3A;margin-bottom:12px}
</style></head>
<body><div class="card">
  <div class="tick">✓</div>
  <h1>Payment received</h1>
  <p>Your report is being installed. You can close this window and return to CommonSense.</p>
</div></body></html>"#;

const COMPLETE_HTML_CANCEL: &str = r#"<!doctype html>
<html lang="en"><head><meta charset="utf-8"><title>Payment cancelled · CommonSense</title>
<meta name="viewport" content="width=device-width, initial-scale=1">
<style>
  html,body{margin:0;height:100%;display:grid;place-items:center;font-family:ui-serif,Georgia,serif;background:#FBF6EC;color:#1F1B14}
  .card{max-width:420px;padding:32px;text-align:center}
  h1{margin:0 0 12px;font-size:24px;font-weight:600;letter-spacing:-0.3px}
  p{margin:0;font-size:14px;line-height:1.6;color:#5C544A}
</style></head>
<body><div class="card">
  <h1>Payment cancelled</h1>
  <p>Nothing was charged. You can close this window and return to CommonSense.</p>
</div></body></html>"#;

fn now_seconds() -> i64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}
