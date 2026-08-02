//! OAuth 2.1 against the Hyperagent authorization server (MISSION.md §4).
//!
//! Flow: metadata discovery → Dynamic Client Registration → Authorization
//! Code + PKCE (S256) via the system browser and a loopback (`127.0.0.1`)
//! redirect listener (RFC 8252) → token exchange → refresh-token grant
//! ~30s before expiry.
//!
//! Scopes requested: `threads:read threads:write approvals:read
//! approvals:write offline_access`.

use base64::Engine;
use rand::RngCore;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

use crate::error::{CoreError, Result};
use crate::vault::TokenBundle;

pub const SCOPES: &str = "threads:read threads:write approvals:read approvals:write offline_access";
pub const CLIENT_NAME: &str = "Starfish";
/// How long we wait for the user to complete the browser flow.
pub const CALLBACK_TIMEOUT_SECS: u64 = 300;

#[derive(Debug, Clone, Deserialize)]
pub struct AuthServerMetadata {
    pub issuer: Option<String>,
    pub authorization_endpoint: String,
    pub token_endpoint: String,
    #[serde(default)]
    pub registration_endpoint: Option<String>,
    #[serde(default)]
    pub code_challenge_methods_supported: Option<Vec<String>>,
    #[serde(default)]
    pub scopes_supported: Option<Vec<String>>,
}

/// Fetch `/.well-known/oauth-authorization-server` (with an OIDC fallback).
pub async fn discover(http: &reqwest::Client, base_url: &str) -> Result<AuthServerMetadata> {
    let base = base_url.trim_end_matches('/');
    for path in [
        "/.well-known/oauth-authorization-server",
        "/.well-known/openid-configuration",
    ] {
        let url = format!("{base}{path}");
        match http.get(&url).send().await {
            Ok(resp) if resp.status().is_success() => {
                return resp
                    .json::<AuthServerMetadata>()
                    .await
                    .map_err(|e| CoreError::OAuth(format!("bad metadata at {url}: {e}")));
            }
            Ok(_) | Err(_) => continue,
        }
    }
    Err(CoreError::OAuth(format!(
        "no OAuth metadata found under {base}"
    )))
}

#[derive(Debug, Serialize)]
struct DcrRequest<'a> {
    client_name: &'a str,
    redirect_uris: Vec<String>,
    grant_types: Vec<&'a str>,
    response_types: Vec<&'a str>,
    token_endpoint_auth_method: &'a str,
    scope: &'a str,
}

#[derive(Debug, Deserialize)]
pub struct DcrResponse {
    pub client_id: String,
    #[serde(default)]
    pub client_secret: Option<String>,
}

/// Dynamic Client Registration (RFC 7591) for a public loopback client.
pub async fn register_client(
    http: &reqwest::Client,
    metadata: &AuthServerMetadata,
    redirect_uri: &str,
) -> Result<DcrResponse> {
    let endpoint = metadata.registration_endpoint.as_deref().ok_or_else(|| {
        CoreError::OAuth("authorization server does not support Dynamic Client Registration".into())
    })?;
    let body = DcrRequest {
        client_name: CLIENT_NAME,
        redirect_uris: vec![redirect_uri.to_string()],
        grant_types: vec!["authorization_code", "refresh_token"],
        response_types: vec!["code"],
        token_endpoint_auth_method: "none",
        scope: SCOPES,
    };
    let resp = http.post(endpoint).json(&body).send().await?;
    let status = resp.status();
    if !status.is_success() {
        let text = resp.text().await.unwrap_or_default();
        return Err(CoreError::OAuth(format!(
            "client registration failed ({status}): {}",
            crate::logbuf::snapshot(&text)
        )));
    }
    resp.json::<DcrResponse>()
        .await
        .map_err(|e| CoreError::OAuth(format!("bad DCR response: {e}")))
}

/// PKCE verifier + S256 challenge (RFC 7636).
pub struct Pkce {
    pub verifier: String,
    pub challenge: String,
}

pub fn make_pkce() -> Pkce {
    let mut bytes = [0u8; 48];
    rand::thread_rng().fill_bytes(&mut bytes);
    let verifier = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes);
    let digest = Sha256::digest(verifier.as_bytes());
    let challenge = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(digest);
    Pkce {
        verifier,
        challenge,
    }
}

pub fn random_state() -> String {
    let mut bytes = [0u8; 24];
    rand::thread_rng().fill_bytes(&mut bytes);
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes)
}

/// Build the authorization URL the system browser should open.
pub fn authorization_url(
    metadata: &AuthServerMetadata,
    client_id: &str,
    redirect_uri: &str,
    state: &str,
    pkce: &Pkce,
) -> Result<String> {
    let mut url = url::Url::parse(&metadata.authorization_endpoint)
        .map_err(|e| CoreError::OAuth(format!("bad authorization endpoint: {e}")))?;
    url.query_pairs_mut()
        .append_pair("response_type", "code")
        .append_pair("client_id", client_id)
        .append_pair("redirect_uri", redirect_uri)
        .append_pair("scope", SCOPES)
        .append_pair("state", state)
        .append_pair("code_challenge", &pkce.challenge)
        .append_pair("code_challenge_method", "S256");
    Ok(url.to_string())
}

/// A bound loopback listener waiting for the OAuth redirect.
pub struct CallbackListener {
    listener: TcpListener,
    pub port: u16,
}

impl CallbackListener {
    /// Bind an ephemeral loopback port (RFC 8252 §7.3 — servers must accept
    /// variable loopback ports).
    pub async fn bind() -> Result<Self> {
        let listener = TcpListener::bind(("127.0.0.1", 0)).await?;
        let port = listener.local_addr()?.port();
        Ok(Self { listener, port })
    }

    pub fn redirect_uri(&self) -> String {
        format!("http://127.0.0.1:{}/callback", self.port)
    }

    /// Wait (with timeout) for the browser redirect and return `code` after
    /// validating `state`. Serves a tiny "you can close this tab" page.
    pub async fn wait_for_code(self, expected_state: &str) -> Result<String> {
        let deadline = tokio::time::Duration::from_secs(CALLBACK_TIMEOUT_SECS);
        let fut = self.accept_loop(expected_state);
        tokio::time::timeout(deadline, fut)
            .await
            .map_err(|_| CoreError::OAuth("sign-in timed out — no browser callback".into()))?
    }

    async fn accept_loop(self, expected_state: &str) -> Result<String> {
        loop {
            let (mut stream, _) = self.listener.accept().await?;
            let mut buf = vec![0u8; 8192];
            let n = stream.read(&mut buf).await?;
            let req = String::from_utf8_lossy(&buf[..n]);
            let first_line = req.lines().next().unwrap_or_default();
            // Expect: GET /callback?code=...&state=... HTTP/1.1
            let path = first_line.split_whitespace().nth(1).unwrap_or("/");
            if !path.starts_with("/callback") {
                let _ = respond(&mut stream, 404, "Not found").await;
                continue;
            }
            let parsed = url::Url::parse(&format!("http://localhost{path}"))
                .map_err(|e| CoreError::OAuth(format!("bad callback url: {e}")))?;
            let mut code = None;
            let mut state = None;
            let mut error = None;
            let mut error_desc = None;
            for (k, v) in parsed.query_pairs() {
                match k.as_ref() {
                    "code" => code = Some(v.to_string()),
                    "state" => state = Some(v.to_string()),
                    "error" => error = Some(v.to_string()),
                    "error_description" => error_desc = Some(v.to_string()),
                    _ => {}
                }
            }
            if let Some(err) = error {
                let desc = error_desc.unwrap_or_default();
                let _ = respond(
                    &mut stream,
                    200,
                    "Sign-in was not completed. You can close this tab and try again from Starfish.",
                )
                .await;
                return Err(CoreError::OAuth(format!(
                    "authorization denied: {err} {desc}"
                )));
            }
            if state.as_deref() != Some(expected_state) {
                let _ = respond(
                    &mut stream,
                    400,
                    "State mismatch — please retry from Starfish.",
                )
                .await;
                return Err(CoreError::OAuth("state mismatch on callback".into()));
            }
            match code {
                Some(code) => {
                    let _ = respond(
                        &mut stream,
                        200,
                        "Signed in to Hyperagent. You can close this tab and return to Starfish.",
                    )
                    .await;
                    return Ok(code);
                }
                None => {
                    let _ = respond(&mut stream, 400, "Missing authorization code.").await;
                    return Err(CoreError::OAuth("callback had no code".into()));
                }
            }
        }
    }
}

async fn respond(stream: &mut tokio::net::TcpStream, status: u16, message: &str) -> Result<()> {
    let reason = match status {
        200 => "OK",
        400 => "Bad Request",
        _ => "Not Found",
    };
    let body = format!(
        "<!doctype html><html><head><meta charset=\"utf-8\"><title>Starfish</title>\
         <style>body{{font-family:system-ui,sans-serif;background:#0b1220;color:#e8edf6;\
         display:grid;place-items:center;height:100vh;margin:0}}\
         .card{{background:#131a2a;border:1px solid #24304a;border-radius:12px;\
         padding:32px 40px;max-width:420px;text-align:center}}\
         .star{{font-size:40px}}</style></head><body><div class=\"card\">\
         <div class=\"star\">⭐</div><h2>Starfish</h2><p>{message}</p></div></body></html>"
    );
    let resp = format!(
        "HTTP/1.1 {status} {reason}\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    stream.write_all(resp.as_bytes()).await?;
    let _ = stream.shutdown().await;
    Ok(())
}

#[derive(Debug, Deserialize)]
struct TokenResponse {
    access_token: String,
    #[serde(default)]
    refresh_token: Option<String>,
    #[serde(default)]
    expires_in: Option<i64>,
    #[serde(default)]
    scope: Option<String>,
}

fn bundle_from_response(
    tr: TokenResponse,
    client_id: &str,
    metadata: &AuthServerMetadata,
    previous_refresh: Option<String>,
) -> TokenBundle {
    TokenBundle {
        access_token: tr.access_token,
        // Some servers rotate refresh tokens, some omit them on refresh —
        // keep the previous one when the response has none.
        refresh_token: tr.refresh_token.or(previous_refresh),
        expires_at: tr
            .expires_in
            .map(|secs| chrono::Utc::now().timestamp() + secs),
        client_id: client_id.to_string(),
        token_endpoint: metadata.token_endpoint.clone(),
        authorization_endpoint: Some(metadata.authorization_endpoint.clone()),
        scope: tr.scope.or_else(|| Some(SCOPES.to_string())),
    }
}

/// Exchange an authorization code for tokens.
pub async fn exchange_code(
    http: &reqwest::Client,
    metadata: &AuthServerMetadata,
    client_id: &str,
    redirect_uri: &str,
    code: &str,
    pkce: &Pkce,
) -> Result<TokenBundle> {
    let params = [
        ("grant_type", "authorization_code"),
        ("code", code),
        ("redirect_uri", redirect_uri),
        ("client_id", client_id),
        ("code_verifier", &pkce.verifier),
    ];
    let resp = http
        .post(&metadata.token_endpoint)
        .form(&params)
        .send()
        .await?;
    let status = resp.status();
    if !status.is_success() {
        let text = resp.text().await.unwrap_or_default();
        return Err(CoreError::OAuth(format!(
            "token exchange failed ({status}): {}",
            crate::logbuf::snapshot(&text)
        )));
    }
    let tr: TokenResponse = resp
        .json()
        .await
        .map_err(|e| CoreError::OAuth(format!("bad token response: {e}")))?;
    Ok(bundle_from_response(tr, client_id, metadata, None))
}

/// Refresh an access token via the `refresh_token` grant. Returns the new
/// bundle. A definitive `invalid_grant` means the account needs re-auth.
pub async fn refresh(http: &reqwest::Client, bundle: &TokenBundle) -> Result<TokenBundle> {
    let refresh_token = bundle
        .refresh_token
        .clone()
        .ok_or_else(|| CoreError::OAuth("no refresh token — sign in again".into()))?;
    let params = [
        ("grant_type", "refresh_token"),
        ("refresh_token", &refresh_token),
        ("client_id", &bundle.client_id),
    ];
    let resp = http
        .post(&bundle.token_endpoint)
        .form(&params)
        .send()
        .await?;
    let status = resp.status();
    if !status.is_success() {
        let text = resp.text().await.unwrap_or_default();
        return Err(CoreError::OAuth(format!(
            "token refresh failed ({status}): {}",
            crate::logbuf::snapshot(&text)
        )));
    }
    let tr: TokenResponse = resp
        .json()
        .await
        .map_err(|e| CoreError::OAuth(format!("bad refresh response: {e}")))?;
    let metadata = AuthServerMetadata {
        issuer: None,
        authorization_endpoint: bundle.authorization_endpoint.clone().unwrap_or_default(),
        token_endpoint: bundle.token_endpoint.clone(),
        registration_endpoint: None,
        code_challenge_methods_supported: None,
        scopes_supported: None,
    };
    Ok(bundle_from_response(
        tr,
        &bundle.client_id,
        &metadata,
        Some(refresh_token),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pkce_is_s256_of_verifier() {
        let p = make_pkce();
        let digest = Sha256::digest(p.verifier.as_bytes());
        let expect = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(digest);
        assert_eq!(p.challenge, expect);
        assert!(p.verifier.len() >= 43); // RFC 7636 minimum
    }

    #[test]
    fn authorization_url_carries_pkce_and_state() {
        let md = AuthServerMetadata {
            issuer: None,
            authorization_endpoint: "https://hyperagent.com/oauth/authorize".into(),
            token_endpoint: "https://hyperagent.com/oauth/token".into(),
            registration_endpoint: Some("https://hyperagent.com/oauth/register".into()),
            code_challenge_methods_supported: Some(vec!["S256".into()]),
            scopes_supported: None,
        };
        let pkce = make_pkce();
        let url = authorization_url(&md, "cid", "http://127.0.0.1:9999/callback", "st4te", &pkce)
            .unwrap();
        assert!(url.contains("code_challenge_method=S256"));
        assert!(url.contains("state=st4te"));
        assert!(url.contains("client_id=cid"));
        assert!(url.contains("response_type=code"));
    }

    #[test]
    fn metadata_parses_recorded_shape() {
        // Recorded-shape unit test per ROADMAP ("unit test against recorded metadata").
        let raw = r#"{
            "issuer": "https://hyperagent.com",
            "authorization_endpoint": "https://hyperagent.com/oauth/authorize",
            "token_endpoint": "https://hyperagent.com/oauth/token",
            "registration_endpoint": "https://hyperagent.com/oauth/register",
            "code_challenge_methods_supported": ["S256"],
            "scopes_supported": ["threads:read","threads:write","offline_access"]
        }"#;
        let md: AuthServerMetadata = serde_json::from_str(raw).unwrap();
        assert_eq!(md.token_endpoint, "https://hyperagent.com/oauth/token");
        assert!(md.registration_endpoint.is_some());
    }

    #[tokio::test]
    async fn callback_listener_extracts_code() {
        let listener = CallbackListener::bind().await.unwrap();
        let port = listener.port;
        let state = "abc123";
        let handle = tokio::spawn(async move { listener.wait_for_code("abc123").await });

        // Simulate the browser redirect.
        let url = format!("http://127.0.0.1:{port}/callback?code=thecode&state={state}");
        let resp = reqwest::get(&url).await.unwrap();
        assert!(resp.status().is_success());

        let code = handle.await.unwrap().unwrap();
        assert_eq!(code, "thecode");
    }

    #[tokio::test]
    async fn callback_listener_rejects_bad_state() {
        let listener = CallbackListener::bind().await.unwrap();
        let port = listener.port;
        let handle = tokio::spawn(async move { listener.wait_for_code("expected").await });
        let url = format!("http://127.0.0.1:{port}/callback?code=x&state=wrong");
        let _ = reqwest::get(&url).await.unwrap();
        assert!(handle.await.unwrap().is_err());
    }
}
