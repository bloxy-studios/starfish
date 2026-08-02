//! Tauri commands — the IPC surface the React UI calls.
//!
//! Thin orchestration only: real logic lives in `starfish-core`. Every
//! command returns `Result<T, String>` with user-readable errors. Vault
//! operations run on blocking threads (OS keychain calls may block).

use std::sync::Arc;

use serde::Serialize;
use tauri::{AppHandle, Emitter, State};
use tauri_plugin_opener::OpenerExt;

use starfish_core::config::{self, AccountRecord, ServerConfig, Settings};
use starfish_core::hyperagent::AgentInfo;
use starfish_core::keys::{self, KeyRecord};
use starfish_core::logbuf::RequestLogEntry;
use starfish_core::mapping::MappingRule;
use starfish_core::oauth;
use starfish_core::upstream::DoctorReport;
use starfish_core::vault::{self, TokenBundle, Vault};

use crate::AppState;

type CmdResult<T> = Result<T, String>;

fn err<E: std::fmt::Display>(e: E) -> String {
    e.to_string()
}

// ---------------------------------------------------------------------------
// DTOs
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize)]
pub struct ServerStatusDto {
    pub running: bool,
    pub host: String,
    pub port: u16,
    pub base_url: String,
    pub started_at: Option<String>,
    pub allow_anonymous: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct AccountDto {
    #[serde(flatten)]
    pub record: AccountRecord,
    /// "valid" | "expiring" | "expired" | "missing — sign in" | …
    pub token_state: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct AppSnapshot {
    pub server: ServerConfig,
    pub settings: Settings,
    pub accounts: Vec<AccountDto>,
    pub keys: Vec<KeyRecord>,
    pub mappings: Vec<MappingRule>,
    pub onboarded: bool,
    pub vault_backend: &'static str,
    pub mock: bool,
    pub server_status: ServerStatusDto,
    pub version: &'static str,
}

#[derive(Debug, Clone, Serialize)]
pub struct CreatedKey {
    pub key: KeyRecord,
    /// Shown once at creation; retrievable later via `reveal_key`.
    pub secret: String,
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

async fn save_config(state: &AppState) -> CmdResult<()> {
    let cfg = state.gateway.config.read().await.clone();
    tokio::task::spawn_blocking(move || config::save(&cfg))
        .await
        .map_err(err)?
        .map_err(err)
}

async fn vault_get_tokens(
    vault: Arc<dyn Vault>,
    account_id: String,
) -> CmdResult<Option<TokenBundle>> {
    tokio::task::spawn_blocking(move || vault::load_tokens(vault.as_ref(), &account_id))
        .await
        .map_err(err)?
        .map_err(err)
}

fn token_state_of(bundle: &Option<TokenBundle>) -> String {
    match bundle {
        Some(b) => match b.expires_in_secs() {
            Some(secs) if secs <= 0 => {
                if b.refresh_token.is_some() {
                    "expired (auto-refresh on next use)".into()
                } else {
                    "expired — sign in again".into()
                }
            }
            Some(secs) if secs <= 300 => "expiring".into(),
            Some(_) => "valid".into(),
            None => "valid".into(),
        },
        None => "missing — sign in".into(),
    }
}

async fn account_dtos(state: &AppState) -> Vec<AccountDto> {
    let records = {
        let cfg = state.gateway.config.read().await;
        cfg.accounts.clone()
    };
    let mut out = Vec::with_capacity(records.len());
    for record in records {
        let bundle = vault_get_tokens(state.vault.clone(), record.id.clone())
            .await
            .unwrap_or(None);
        out.push(AccountDto {
            token_state: if state.mock {
                "mock".into()
            } else {
                token_state_of(&bundle)
            },
            record,
        });
    }
    out
}

async fn status_dto(state: &AppState) -> ServerStatusDto {
    let cfg = state.gateway.config.read().await;
    let server = state.server.lock().await;
    let (running, started_at, host, port) = match server.as_ref() {
        Some(handle) => (
            true,
            Some(handle.started_at.to_rfc3339()),
            handle.addr.ip().to_string(),
            handle.addr.port(),
        ),
        None => (false, None, cfg.server.host.clone(), cfg.server.port),
    };
    ServerStatusDto {
        running,
        base_url: format!("http://{host}:{port}"),
        host,
        port,
        started_at,
        allow_anonymous: cfg.server.allow_anonymous,
    }
}

pub(crate) async fn start_server_inner(state: &AppState) -> CmdResult<ServerStatusDto> {
    {
        let mut server = state.server.lock().await;
        if server.is_none() {
            let handle = starfish_core::gateway::start(state.gateway.clone())
                .await
                .map_err(err)?;
            *server = Some(handle);
        }
    }
    Ok(status_dto(state).await)
}

pub(crate) async fn stop_server_inner(state: &AppState) -> CmdResult<ServerStatusDto> {
    let handle = state.server.lock().await.take();
    if let Some(handle) = handle {
        handle.stop().await;
    }
    Ok(status_dto(state).await)
}

// ---------------------------------------------------------------------------
// App snapshot / config
// ---------------------------------------------------------------------------

#[tauri::command]
pub async fn app_snapshot(state: State<'_, AppState>) -> CmdResult<AppSnapshot> {
    let (server, settings, keys, mappings, onboarded) = {
        let cfg = state.gateway.config.read().await;
        (
            cfg.server.clone(),
            cfg.settings.clone(),
            cfg.keys.clone(),
            cfg.mappings.clone(),
            cfg.onboarded,
        )
    };
    Ok(AppSnapshot {
        server,
        settings,
        accounts: account_dtos(&state).await,
        keys,
        mappings,
        onboarded,
        vault_backend: state.vault.backend(),
        mock: state.mock,
        server_status: status_dto(&state).await,
        version: env!("CARGO_PKG_VERSION"),
    })
}

#[tauri::command]
pub async fn set_server_config(
    state: State<'_, AppState>,
    server: ServerConfig,
) -> CmdResult<ServerStatusDto> {
    config::validate_server(&server).map_err(err)?;
    {
        let mut cfg = state.gateway.config.write().await;
        cfg.server = server;
    }
    save_config(&state).await?;
    Ok(status_dto(&state).await)
}

#[tauri::command]
pub async fn set_settings(state: State<'_, AppState>, settings: Settings) -> CmdResult<()> {
    {
        let mut cfg = state.gateway.config.write().await;
        cfg.settings = settings;
    }
    save_config(&state).await
}

#[tauri::command]
pub async fn set_onboarded(state: State<'_, AppState>, done: bool) -> CmdResult<()> {
    {
        let mut cfg = state.gateway.config.write().await;
        cfg.onboarded = done;
    }
    save_config(&state).await
}

// ---------------------------------------------------------------------------
// Server lifecycle
// ---------------------------------------------------------------------------

#[tauri::command]
pub async fn server_start(
    app: AppHandle,
    state: State<'_, AppState>,
) -> CmdResult<ServerStatusDto> {
    let status = start_server_inner(&state).await?;
    let _ = app.emit("server://status", &status);
    Ok(status)
}

#[tauri::command]
pub async fn server_stop(app: AppHandle, state: State<'_, AppState>) -> CmdResult<ServerStatusDto> {
    let status = stop_server_inner(&state).await?;
    let _ = app.emit("server://status", &status);
    Ok(status)
}

#[tauri::command]
pub async fn server_status(state: State<'_, AppState>) -> CmdResult<ServerStatusDto> {
    Ok(status_dto(&state).await)
}

// ---------------------------------------------------------------------------
// Accounts / OAuth
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize)]
struct OAuthProgress<'a> {
    stage: &'a str,
    detail: String,
}

fn emit_oauth(app: &AppHandle, stage: &str, detail: impl Into<String>) {
    let _ = app.emit(
        "oauth://status",
        OAuthProgress {
            stage,
            detail: detail.into(),
        },
    );
}

/// Run the full sign-in flow. `account_id: None` adds a new account;
/// `Some(id)` re-authenticates an existing one.
async fn oauth_flow(
    app: &AppHandle,
    state: &AppState,
    nickname: String,
    base_url: String,
    account_id: Option<String>,
) -> CmdResult<AccountDto> {
    if state.mock {
        return Err(
            "Mock upstream is active (STARFISH_MOCK_UPSTREAM) — sign-in is disabled.".into(),
        );
    }
    let http = &state.http;

    emit_oauth(
        app,
        "discovering",
        format!("Fetching OAuth metadata from {base_url}"),
    );
    let metadata = oauth::discover(http, &base_url).await.map_err(err)?;

    let listener = oauth::CallbackListener::bind().await.map_err(err)?;
    let redirect_uri = listener.redirect_uri();

    emit_oauth(
        app,
        "registering",
        "Registering Starfish with the authorization server",
    );
    let dcr = oauth::register_client(http, &metadata, &redirect_uri)
        .await
        .map_err(err)?;

    let pkce = oauth::make_pkce();
    let state_param = oauth::random_state();
    let auth_url = oauth::authorization_url(
        &metadata,
        &dcr.client_id,
        &redirect_uri,
        &state_param,
        &pkce,
    )
    .map_err(err)?;

    emit_oauth(
        app,
        "browser",
        "Opening your browser to sign in to Hyperagent…",
    );
    app.opener()
        .open_url(&auth_url, None::<&str>)
        .map_err(|e| format!("could not open browser: {e}"))?;

    emit_oauth(
        app,
        "waiting",
        "Waiting for you to approve access in the browser",
    );
    let code = listener.wait_for_code(&state_param).await.map_err(err)?;

    emit_oauth(
        app,
        "exchanging",
        "Exchanging the authorization code for tokens",
    );
    let bundle = oauth::exchange_code(http, &metadata, &dcr.client_id, &redirect_uri, &code, &pkce)
        .await
        .map_err(err)?;

    // Persist: tokens to the vault, account metadata to config.
    let id = account_id.unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
    {
        let vault = state.vault.clone();
        let id2 = id.clone();
        let bundle2 = bundle.clone();
        tokio::task::spawn_blocking(move || vault::store_tokens(vault.as_ref(), &id2, &bundle2))
            .await
            .map_err(err)?
            .map_err(err)?;
    }
    let record = {
        let mut cfg = state.gateway.config.write().await;
        match cfg.account_mut(&id) {
            Some(existing) => {
                if !nickname.is_empty() {
                    existing.nickname = nickname.clone();
                }
                existing.clone()
            }
            None => {
                let record = AccountRecord {
                    id: id.clone(),
                    nickname: if nickname.is_empty() {
                        format!("Account {}", cfg.accounts.len() + 1)
                    } else {
                        nickname.clone()
                    },
                    base_url: base_url.clone(),
                    identity: None,
                    default_agent_id: None,
                    created_at: chrono::Utc::now(),
                };
                cfg.accounts.push(record.clone());
                record
            }
        }
    };
    save_config(state).await?;
    state.gateway.invalidate_agents(&id).await;

    emit_oauth(app, "done", "Signed in");
    Ok(AccountDto {
        token_state: token_state_of(&Some(bundle)),
        record,
    })
}

#[tauri::command]
pub async fn begin_sign_in(
    app: AppHandle,
    state: State<'_, AppState>,
    nickname: Option<String>,
    base_url: Option<String>,
) -> CmdResult<AccountDto> {
    let base = base_url
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| config::DEFAULT_UPSTREAM.to_string());
    oauth_flow(&app, &state, nickname.unwrap_or_default(), base, None).await
}

#[tauri::command]
pub async fn reauth_account(
    app: AppHandle,
    state: State<'_, AppState>,
    account_id: String,
) -> CmdResult<AccountDto> {
    let (nickname, base_url) = {
        let cfg = state.gateway.config.read().await;
        let account = cfg
            .account(&account_id)
            .ok_or_else(|| format!("unknown account {account_id}"))?;
        (account.nickname.clone(), account.base_url.clone())
    };
    oauth_flow(&app, &state, nickname, base_url, Some(account_id)).await
}

#[tauri::command]
pub async fn remove_account(state: State<'_, AppState>, account_id: String) -> CmdResult<()> {
    // Delete tokens from the keychain first.
    {
        let vault = state.vault.clone();
        let id = account_id.clone();
        tokio::task::spawn_blocking(move || vault::delete_tokens(vault.as_ref(), &id))
            .await
            .map_err(err)?
            .map_err(err)?;
    }
    {
        let mut cfg = state.gateway.config.write().await;
        cfg.accounts.retain(|a| a.id != account_id);
        // Keys routed to this account can no longer resolve — revoke them.
        for key in cfg.keys.iter_mut().filter(|k| k.account_id == account_id) {
            key.revoked = true;
        }
    }
    save_config(&state).await
}

#[tauri::command]
pub async fn set_account_nickname(
    state: State<'_, AppState>,
    account_id: String,
    nickname: String,
) -> CmdResult<()> {
    {
        let mut cfg = state.gateway.config.write().await;
        let account = cfg
            .account_mut(&account_id)
            .ok_or_else(|| format!("unknown account {account_id}"))?;
        account.nickname = nickname;
    }
    save_config(&state).await
}

#[tauri::command]
pub async fn set_account_default_agent(
    state: State<'_, AppState>,
    account_id: String,
    agent_id: Option<String>,
) -> CmdResult<()> {
    {
        let mut cfg = state.gateway.config.write().await;
        let account = cfg
            .account_mut(&account_id)
            .ok_or_else(|| format!("unknown account {account_id}"))?;
        account.default_agent_id = agent_id;
    }
    save_config(&state).await
}

#[tauri::command]
pub async fn doctor(state: State<'_, AppState>, account_id: String) -> CmdResult<DoctorReport> {
    state
        .gateway
        .upstream
        .doctor(&account_id)
        .await
        .map_err(err)
}

#[tauri::command]
pub async fn list_agents(
    state: State<'_, AppState>,
    account_id: String,
    force: Option<bool>,
) -> CmdResult<Vec<AgentInfo>> {
    state
        .gateway
        .agents(&account_id, force.unwrap_or(false))
        .await
        .map_err(err)
}

// ---------------------------------------------------------------------------
// Local API keys
// ---------------------------------------------------------------------------

async fn update_key_secrets(
    state: &AppState,
    f: impl FnOnce(&mut std::collections::HashMap<String, String>) + Send + 'static,
) -> CmdResult<()> {
    let vault = state.vault.clone();
    tokio::task::spawn_blocking(move || {
        let mut secrets = vault::load_key_secrets(vault.as_ref())?;
        f(&mut secrets);
        vault::store_key_secrets(vault.as_ref(), &secrets)
    })
    .await
    .map_err(err)?
    .map_err(err)
}

#[tauri::command]
pub async fn create_key(
    state: State<'_, AppState>,
    name: String,
    account_id: String,
    default_agent_id: Option<String>,
) -> CmdResult<CreatedKey> {
    {
        let cfg = state.gateway.config.read().await;
        if cfg.account(&account_id).is_none() {
            return Err(format!("unknown account {account_id}"));
        }
    }
    let (secret, hash, hint) = keys::generate_key();
    let record = KeyRecord {
        id: uuid::Uuid::new_v4().to_string(),
        name: if name.is_empty() {
            "unnamed key".into()
        } else {
            name
        },
        hash,
        hint,
        account_id,
        default_agent_id,
        disabled_tools: vec![],
        created_at: chrono::Utc::now(),
        last_used_at: None,
        revoked: false,
    };
    {
        let key_id = record.id.clone();
        let secret2 = secret.clone();
        update_key_secrets(&state, move |secrets| {
            secrets.insert(key_id, secret2);
        })
        .await?;
    }
    {
        let mut cfg = state.gateway.config.write().await;
        cfg.keys.push(record.clone());
    }
    save_config(&state).await?;
    Ok(CreatedKey {
        key: record,
        secret,
    })
}

#[tauri::command]
pub async fn reveal_key(state: State<'_, AppState>, key_id: String) -> CmdResult<String> {
    let vault = state.vault.clone();
    let secrets = tokio::task::spawn_blocking(move || vault::load_key_secrets(vault.as_ref()))
        .await
        .map_err(err)?
        .map_err(err)?;
    secrets
        .get(&key_id)
        .cloned()
        .ok_or_else(|| "key material not found in the vault".into())
}

#[tauri::command]
pub async fn revoke_key(state: State<'_, AppState>, key_id: String) -> CmdResult<()> {
    {
        let mut cfg = state.gateway.config.write().await;
        let key = cfg
            .keys
            .iter_mut()
            .find(|k| k.id == key_id)
            .ok_or_else(|| format!("unknown key {key_id}"))?;
        key.revoked = true;
    }
    {
        let key_id2 = key_id.clone();
        update_key_secrets(&state, move |secrets| {
            secrets.remove(&key_id2);
        })
        .await?;
    }
    save_config(&state).await
}

#[tauri::command]
pub async fn rotate_key(state: State<'_, AppState>, key_id: String) -> CmdResult<CreatedKey> {
    let (secret, hash, hint) = keys::generate_key();
    let record = {
        let mut cfg = state.gateway.config.write().await;
        let key = cfg
            .keys
            .iter_mut()
            .find(|k| k.id == key_id)
            .ok_or_else(|| format!("unknown key {key_id}"))?;
        if key.revoked {
            return Err("cannot rotate a revoked key".into());
        }
        key.hash = hash;
        key.hint = hint;
        key.clone()
    };
    {
        let key_id2 = key_id.clone();
        let secret2 = secret.clone();
        update_key_secrets(&state, move |secrets| {
            secrets.insert(key_id2, secret2);
        })
        .await?;
    }
    save_config(&state).await?;
    Ok(CreatedKey {
        key: record,
        secret,
    })
}

#[tauri::command]
pub async fn rename_key(state: State<'_, AppState>, key_id: String, name: String) -> CmdResult<()> {
    {
        let mut cfg = state.gateway.config.write().await;
        let key = cfg
            .keys
            .iter_mut()
            .find(|k| k.id == key_id)
            .ok_or_else(|| format!("unknown key {key_id}"))?;
        key.name = name;
    }
    save_config(&state).await
}

#[tauri::command]
pub async fn set_key_agent(
    state: State<'_, AppState>,
    key_id: String,
    agent_id: Option<String>,
) -> CmdResult<()> {
    {
        let mut cfg = state.gateway.config.write().await;
        let key = cfg
            .keys
            .iter_mut()
            .find(|k| k.id == key_id)
            .ok_or_else(|| format!("unknown key {key_id}"))?;
        key.default_agent_id = agent_id;
    }
    save_config(&state).await
}

// ---------------------------------------------------------------------------
// Model mappings
// ---------------------------------------------------------------------------

#[tauri::command]
pub async fn set_mappings(state: State<'_, AppState>, mappings: Vec<MappingRule>) -> CmdResult<()> {
    {
        let mut cfg = state.gateway.config.write().await;
        cfg.mappings = mappings;
    }
    save_config(&state).await
}

// ---------------------------------------------------------------------------
// Logs / misc
// ---------------------------------------------------------------------------

#[tauri::command]
pub async fn logs_recent(
    state: State<'_, AppState>,
    limit: Option<usize>,
) -> CmdResult<Vec<RequestLogEntry>> {
    Ok(state.gateway.log.recent(limit.unwrap_or(200)))
}

#[tauri::command]
pub async fn clear_logs(state: State<'_, AppState>) -> CmdResult<()> {
    state.gateway.log.clear();
    Ok(())
}

#[tauri::command]
pub async fn open_external(app: AppHandle, url: String) -> CmdResult<()> {
    if !url.starts_with("https://") && !url.starts_with("http://") {
        return Err("only http(s) URLs can be opened".into());
    }
    app.opener()
        .open_url(&url, None::<&str>)
        .map_err(|e| format!("could not open browser: {e}"))
}

#[tauri::command]
pub async fn set_launch_at_login(app: AppHandle, enabled: bool) -> CmdResult<bool> {
    use tauri_plugin_autostart::ManagerExt;
    let autolaunch = app.autolaunch();
    if enabled {
        autolaunch.enable().map_err(err)?;
    } else {
        autolaunch.disable().map_err(err)?;
    }
    autolaunch.is_enabled().map_err(err)
}

#[tauri::command]
pub async fn get_launch_at_login(app: AppHandle) -> CmdResult<bool> {
    use tauri_plugin_autostart::ManagerExt;
    app.autolaunch().is_enabled().map_err(err)
}
