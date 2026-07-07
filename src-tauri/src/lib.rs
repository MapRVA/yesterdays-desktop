mod imports;
mod prepare;

use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use futures::stream::{self, StreamExt};
use image_hasher::{HasherConfig, ImageHash};
use rand::Rng;
use rayon::prelude::*;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::io::{BufRead, BufReader, Write};
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::time::UNIX_EPOCH;
use tauri::{Emitter, Manager};
use url::Url;

const APP_NAME: &str = "Yesterdays Desktop";
const APP_REDIRECT_URI: &str = "http://127.0.0.1/callback";
const SCOPES: &str = "read import";
const CANCEL_PATH: &str = "/cancel";
const CLIENT_CACHE_FILE: &str = "clients.json";

#[derive(Default)]
pub struct AuthState {
    pub cancel_port: Mutex<Option<u16>>,
}

#[derive(Serialize)]
pub struct InstanceInfo {
    pub url: String,
    pub name: String,
}

#[derive(Serialize, Deserialize, Clone)]
pub struct TokenResponse {
    pub access_token: String,
    pub refresh_token: Option<String>,
    pub expires_in: u64,
    pub token_type: String,
    pub scope: String,
}

async fn refresh_tokens_impl(
    app: &tauri::AppHandle,
    instance_url: &str,
    refresh_token: &str,
) -> Result<TokenResponse, String> {
    let creds = ensure_client(app, instance_url).await?;
    let token_url = format!("{}/oauth/token/", instance_url);
    let resp = reqwest::Client::new()
        .post(&token_url)
        .form(&[
            ("grant_type", "refresh_token"),
            ("refresh_token", refresh_token),
            ("client_id", &creds.client_id),
        ])
        .send()
        .await
        .map_err(|e| format!("Token refresh failed: {}", e))?;

    if !resp.status().is_success() {
        let body = resp.text().await.unwrap_or_default();
        return Err(format!("Token refresh returned error: {}", body));
    }

    resp.json::<TokenResponse>()
        .await
        .map_err(|e| format!("Failed to parse token response: {}", e))
}

#[derive(Serialize, Deserialize)]
pub struct UserInfo {
    pub osm_id: u64,
    pub username: String,
    pub is_staff: bool,
    pub can_import: bool,
    pub scopes: Vec<String>,
}

#[derive(Serialize)]
pub struct AuthResult {
    pub user: UserInfo,
    pub tokens: TokenResponse,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
struct ClientCredentials {
    client_id: String,
    #[serde(default)]
    client_secret: String,
}

#[derive(Serialize, Deserialize, Default)]
struct ClientCache {
    #[serde(default)]
    instances: HashMap<String, ClientCredentials>,
}

fn client_cache_path(app: &tauri::AppHandle) -> Result<PathBuf, String> {
    let dir = app
        .path()
        .app_data_dir()
        .map_err(|e| format!("Could not resolve app data dir: {}", e))?;
    Ok(dir.join(CLIENT_CACHE_FILE))
}

fn load_cache(app: &tauri::AppHandle) -> ClientCache {
    let Ok(path) = client_cache_path(app) else {
        return ClientCache::default();
    };
    std::fs::read_to_string(&path)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

fn save_cache(app: &tauri::AppHandle, cache: &ClientCache) -> Result<(), String> {
    let path = client_cache_path(app)?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| format!("Failed to create app data dir: {}", e))?;
    }
    let json = serde_json::to_string_pretty(cache)
        .map_err(|e| format!("Failed to serialize client cache: {}", e))?;
    std::fs::write(&path, json).map_err(|e| format!("Failed to write client cache: {}", e))
}

async fn ensure_client(
    app: &tauri::AppHandle,
    instance_url: &str,
) -> Result<ClientCredentials, String> {
    let mut cache = load_cache(app);
    if let Some(creds) = cache.instances.get(instance_url) {
        return Ok(creds.clone());
    }

    let register_url = format!("{}/api/v2/apps/", instance_url);
    let resp = reqwest::Client::new()
        .post(&register_url)
        .json(&serde_json::json!({
            "name": APP_NAME,
            "redirect_uris": APP_REDIRECT_URI,
            "client_type": "public",
        }))
        .send()
        .await
        .map_err(|e| format!("App registration failed: {}", e))?;

    if !resp.status().is_success() {
        let body = resp.text().await.unwrap_or_default();
        return Err(format!("App registration returned error: {}", body));
    }

    let creds: ClientCredentials = resp
        .json()
        .await
        .map_err(|e| format!("Failed to parse registration response: {}", e))?;

    cache
        .instances
        .insert(instance_url.to_string(), creds.clone());
    save_cache(app, &cache)?;
    Ok(creds)
}

fn generate_pkce() -> (String, String) {
    let mut bytes = [0u8; 64];
    rand::rng().fill(&mut bytes);
    let verifier = URL_SAFE_NO_PAD.encode(bytes);

    let mut hasher = Sha256::new();
    hasher.update(verifier.as_bytes());
    let challenge = URL_SAFE_NO_PAD.encode(hasher.finalize());

    (verifier, challenge)
}

fn extract_code_from_request(request_line: &str) -> Result<String, String> {
    // Request line looks like: GET /callback?code=XXXX HTTP/1.1
    let path = request_line
        .split_whitespace()
        .nth(1)
        .ok_or("Invalid HTTP request")?;

    if path.starts_with(CANCEL_PATH) {
        return Err("Cancelled".to_string());
    }

    let full_url = format!("http://127.0.0.1{}", path);
    let parsed =
        Url::parse(&full_url).map_err(|e| format!("Failed to parse callback URL: {}", e))?;

    parsed
        .query_pairs()
        .find(|(key, _)| key == "code")
        .map(|(_, value)| value.to_string())
        .ok_or_else(|| {
            // Check for error parameter
            let error = parsed
                .query_pairs()
                .find(|(key, _)| key == "error")
                .map(|(_, v)| v.to_string())
                .unwrap_or_else(|| "unknown error".to_string());
            format!("Authorization failed: {}", error)
        })
}

#[tauri::command]
async fn validate_instance(url: String) -> Result<InstanceInfo, String> {
    let url = url.trim_end_matches('/').to_string();
    let stats_url = format!("{}/api/v2/stats/", url);

    let resp = reqwest::get(&stats_url)
        .await
        .map_err(|e| format!("Could not connect: {}", e))?;

    if !resp.status().is_success() {
        return Err(format!("Server returned status {}", resp.status()));
    }

    let body: serde_json::Value = resp
        .json()
        .await
        .map_err(|e| format!("Invalid response: {}", e))?;

    let name = body
        .get("name")
        .and_then(|v| v.as_str())
        .unwrap_or("Yesterdays Instance")
        .to_string();

    Ok(InstanceInfo { url, name })
}

#[tauri::command]
async fn start_oauth_login(
    instance_url: String,
    state: tauri::State<'_, AuthState>,
    app: tauri::AppHandle,
) -> Result<AuthResult, String> {
    let instance_url = instance_url.trim_end_matches('/').to_string();
    let creds = ensure_client(&app, &instance_url).await?;
    let (verifier, challenge) = generate_pkce();

    // Bind to an available port on localhost
    let listener =
        TcpListener::bind("127.0.0.1:0").map_err(|e| format!("Failed to bind listener: {}", e))?;
    let port = listener
        .local_addr()
        .map_err(|e| format!("Failed to get port: {}", e))?
        .port();
    let redirect_uri = format!("http://127.0.0.1:{}/callback", port);

    // Register the port so cancel_oauth_login can wake the listener
    {
        let mut guard = state
            .cancel_port
            .lock()
            .map_err(|e| format!("Failed to lock auth state: {}", e))?;
        *guard = Some(port);
    }

    // Build the authorization URL
    let auth_url = format!(
        "{}/oauth/authorize/?response_type=code&client_id={}&redirect_uri={}&code_challenge={}&code_challenge_method=S256&scope={}",
        instance_url,
        urlencoding::encode(&creds.client_id),
        urlencoding::encode(&redirect_uri),
        challenge,
        urlencoding::encode(SCOPES),
    );

    tauri_plugin_opener::open_url(&auth_url, None::<&str>)
        .map_err(|e| format!("Failed to open browser: {}", e))?;

    // Wait for the callback (blocking, but we're in an async command on a thread)
    let listen_result = tokio::task::spawn_blocking(move || -> Result<String, String> {
        // Set a timeout so we don't block forever
        listener
            .set_nonblocking(false)
            .map_err(|e| format!("Failed to configure listener: {}", e))?;

        let (mut stream, _) = listener
            .accept()
            .map_err(|e| format!("Failed to accept connection: {}", e))?;

        let mut reader = BufReader::new(&stream);
        let mut request_line = String::new();
        reader
            .read_line(&mut request_line)
            .map_err(|e| format!("Failed to read request: {}", e))?;

        let code = extract_code_from_request(&request_line)?;

        // Send a response to the browser
        let html = r#"<!DOCTYPE html>
<html><head><title>Yesterdays</title></head>
<body style="font-family: system-ui; text-align: center; padding-top: 80px;">
<h2>Authentication successful!</h2>
<p>You can close this tab and return to the app.</p>
</body></html>"#;
        let response = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
            html.len(),
            html
        );
        let _ = stream.write_all(response.as_bytes());

        Ok(code)
    })
    .await
    .map_err(|e| format!("Task failed: {}", e));

    // Clear the registered port — the listener has been consumed either way
    if let Ok(mut guard) = state.cancel_port.lock() {
        *guard = None;
    }

    let code = listen_result??;

    // Exchange the authorization code for tokens
    let token_url = format!("{}/oauth/token/", instance_url);
    let client = reqwest::Client::new();
    let token_resp = client
        .post(&token_url)
        .form(&[
            ("grant_type", "authorization_code"),
            ("code", &code),
            ("redirect_uri", &redirect_uri),
            ("client_id", &creds.client_id),
            ("code_verifier", &verifier),
        ])
        .send()
        .await
        .map_err(|e| format!("Token exchange failed: {}", e))?;

    if !token_resp.status().is_success() {
        let body = token_resp.text().await.unwrap_or_default();
        return Err(format!("Token exchange returned error: {}", body));
    }

    let tokens: TokenResponse = token_resp
        .json()
        .await
        .map_err(|e| format!("Failed to parse token response: {}", e))?;

    // Fetch user info
    let me_url = format!("{}/api/v2/auth/me/", instance_url);
    let me_resp = client
        .get(&me_url)
        .header("Authorization", format!("Bearer {}", tokens.access_token))
        .send()
        .await
        .map_err(|e| format!("Failed to fetch user info: {}", e))?;

    if !me_resp.status().is_success() {
        let body = me_resp.text().await.unwrap_or_default();
        return Err(format!("Failed to fetch user info: {}", body));
    }

    let user: UserInfo = me_resp
        .json()
        .await
        .map_err(|e| format!("Failed to parse user info: {}", e))?;

    Ok(AuthResult { user, tokens })
}

#[tauri::command]
async fn cancel_oauth_login(state: tauri::State<'_, AuthState>) -> Result<(), String> {
    let port = {
        let guard = state
            .cancel_port
            .lock()
            .map_err(|e| format!("Failed to lock auth state: {}", e))?;
        *guard
    };

    let Some(port) = port else {
        return Ok(());
    };

    // Wake the blocked listener with a throwaway loopback request
    let url = format!("http://127.0.0.1:{}{}", port, CANCEL_PATH);
    let _ = reqwest::Client::new()
        .get(&url)
        .timeout(std::time::Duration::from_secs(2))
        .send()
        .await;

    Ok(())
}

#[derive(Deserialize)]
struct ThumbnailItem {
    id: u64,
    url: String,
}

#[derive(Serialize, Clone)]
struct ThumbnailProgress {
    phase: &'static str,
    done: u32,
    total: u32,
    ok: u32,
    skipped: u32,
    failed: u32,
    hashed: u32,
    last_id: u64,
}

#[derive(Serialize)]
struct ThumbnailDownloadResult {
    cache_dir: String,
    total: u32,
    ok: u32,
    skipped: u32,
    failed: u32,
    hashed: u32,
    already_hashed: u32,
}

#[derive(Serialize, Clone)]
struct ThumbnailHashError {
    image_id: u64,
    path: String,
    error: String,
}

fn url_extension(url: &str) -> Option<String> {
    let parsed = Url::parse(url).ok()?;
    let last = parsed.path().rsplit('/').next()?;
    let (name, ext) = last.rsplit_once('.')?;
    if name.is_empty() || ext.is_empty() || ext.len() > 5 {
        return None;
    }
    Some(ext.to_lowercase())
}

async fn download_one(
    client: &reqwest::Client,
    dir: &Path,
    item: &ThumbnailItem,
) -> Result<bool, String> {
    let ext = url_extension(&item.url).unwrap_or_else(|| "jpg".to_string());
    let path = dir.join(format!("{}.{}", item.id, ext));
    if path.exists() {
        return Ok(false);
    }
    let bytes = client
        .get(&item.url)
        .send()
        .await
        .map_err(|e| e.to_string())?
        .error_for_status()
        .map_err(|e| e.to_string())?
        .bytes()
        .await
        .map_err(|e| e.to_string())?;
    let tmp = path.with_extension(format!("{}.tmp", ext));
    std::fs::write(&tmp, &bytes).map_err(|e| e.to_string())?;
    std::fs::rename(&tmp, &path).map_err(|e| e.to_string())?;
    Ok(true)
}

#[tauri::command]
async fn download_thumbnails(
    app: tauri::AppHandle,
    collection_id: u64,
    items: Vec<ThumbnailItem>,
) -> Result<ThumbnailDownloadResult, String> {
    let coll_dir = collection_dir(&app, collection_id)?;
    let dir = coll_dir.join("thumbnails");
    let hashes_path = coll_dir.join(HASHES_FILE);
    std::fs::create_dir_all(&dir).map_err(|e| format!("create dir: {}", e))?;

    let total = items.len() as u32;
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(30))
        .build()
        .map_err(|e| format!("client: {}", e))?;

    // --- Phase 1: download ---
    let counters = Arc::new(Mutex::new((0u32, 0u32, 0u32, 0u32)));

    stream::iter(items)
        .for_each_concurrent(8, |item| {
            let client = client.clone();
            let dir = dir.clone();
            let app = app.clone();
            let counters = counters.clone();
            async move {
                let id = item.id;
                let result = download_one(&client, &dir, &item).await;
                let progress = {
                    let mut c = counters.lock().unwrap();
                    c.0 += 1;
                    match result {
                        Ok(true) => c.1 += 1,
                        Ok(false) => c.2 += 1,
                        Err(_) => c.3 += 1,
                    }
                    ThumbnailProgress {
                        phase: "downloading",
                        done: c.0,
                        total,
                        ok: c.1,
                        skipped: c.2,
                        failed: c.3,
                        hashed: 0,
                        last_id: id,
                    }
                };
                let _ = app.emit("thumbnail-progress", progress);
            }
        })
        .await;

    let (download_ok, download_skipped, download_failed) = {
        let c = counters.lock().unwrap();
        (c.1, c.2, c.3)
    };

    // --- Phase 2: hash any thumbnails without a stored hash ---
    let mut hashes = load_hashes(&hashes_path);

    let mut thumb_entries: Vec<(u64, PathBuf)> = Vec::new();
    for entry in std::fs::read_dir(&dir).map_err(|e| e.to_string())? {
        let entry = entry.map_err(|e| e.to_string())?;
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        let stem = match path.file_stem().and_then(|s| s.to_str()) {
            Some(s) => s,
            None => continue,
        };
        let id: u64 = match stem.parse() {
            Ok(n) => n,
            Err(_) => continue,
        };
        thumb_entries.push((id, path));
    }

    let on_disk_total = thumb_entries.len() as u32;
    let to_hash: Vec<(u64, PathBuf)> = thumb_entries
        .into_iter()
        .filter(|(id, _)| !hashes.contains_key(&id.to_string()))
        .collect();

    let hash_total = to_hash.len() as u32;
    let already_hashed = on_disk_total - hash_total;
    let app_for_hash = app.clone();
    let new_hashes = tauri::async_runtime::spawn_blocking(move || {
        let mut out: HashMap<String, String> = HashMap::new();
        let mut hashed: u32 = 0;
        for (i, (id, path)) in to_hash.iter().enumerate() {
            match hash_file(path) {
                Ok(h) => {
                    out.insert(id.to_string(), h.to_base64());
                    hashed += 1;
                }
                Err(e) => {
                    let _ = app_for_hash.emit(
                        "thumbnail-hash-error",
                        ThumbnailHashError {
                            image_id: *id,
                            path: path.to_string_lossy().into_owned(),
                            error: e,
                        },
                    );
                }
            }
            let _ = app_for_hash.emit(
                "thumbnail-progress",
                ThumbnailProgress {
                    phase: "hashing",
                    done: (i + 1) as u32,
                    total: hash_total,
                    ok: download_ok,
                    skipped: download_skipped,
                    failed: download_failed,
                    hashed,
                    last_id: *id,
                },
            );
        }
        out
    })
    .await
    .map_err(|e| e.to_string())?;

    let newly_hashed = new_hashes.len() as u32;
    for (k, v) in new_hashes {
        hashes.insert(k, v);
    }
    save_hashes(&hashes_path, &hashes)?;

    Ok(ThumbnailDownloadResult {
        cache_dir: dir.to_string_lossy().into_owned(),
        total,
        ok: download_ok,
        skipped: download_skipped,
        failed: download_failed,
        hashed: newly_hashed,
        already_hashed,
    })
}

const HASHES_FILE: &str = "hashes.json";
const FOLDER_HASHES_FILE: &str = "folder_hashes.json";
const IMAGE_EXTS: &[&str] = &["jpg", "jpeg", "png", "webp", "gif", "bmp", "tif", "tiff"];

#[derive(Serialize, Deserialize, Clone)]
struct FolderHashEntry {
    mtime_secs: u64,
    size: u64,
    hash_b64: String,
    preview_filename: String,
}

pub(crate) fn collection_dir(
    app: &tauri::AppHandle,
    collection_id: u64,
) -> Result<PathBuf, String> {
    let cache_root = app
        .path()
        .app_cache_dir()
        .map_err(|e| format!("cache dir: {}", e))?;
    Ok(cache_root
        .join("collections")
        .join(collection_id.to_string()))
}

fn load_hashes(path: &Path) -> HashMap<String, String> {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|s| serde_json::from_str::<HashMap<String, String>>(&s).ok())
        .unwrap_or_default()
}

fn save_hashes(path: &Path, hashes: &HashMap<String, String>) -> Result<(), String> {
    let tmp = path.with_extension("json.tmp");
    let data = serde_json::to_vec_pretty(hashes).map_err(|e| e.to_string())?;
    std::fs::write(&tmp, &data).map_err(|e| e.to_string())?;
    std::fs::rename(&tmp, path).map_err(|e| e.to_string())?;
    Ok(())
}

fn load_folder_hashes(path: &Path) -> HashMap<String, FolderHashEntry> {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

fn save_folder_hashes(
    path: &Path,
    hashes: &HashMap<String, FolderHashEntry>,
) -> Result<(), String> {
    let tmp = path.with_extension("json.tmp");
    let data = serde_json::to_vec_pretty(hashes).map_err(|e| e.to_string())?;
    std::fs::write(&tmp, &data).map_err(|e| e.to_string())?;
    std::fs::rename(&tmp, path).map_err(|e| e.to_string())?;
    Ok(())
}

fn file_size_mtime(path: &Path) -> Option<(u64, u64)> {
    let meta = std::fs::metadata(path).ok()?;
    let size = meta.len();
    let mtime = meta
        .modified()
        .ok()?
        .duration_since(UNIX_EPOCH)
        .ok()?
        .as_secs();
    Some((size, mtime))
}

fn lookup_or_hash(
    path: &Path,
    preview_dir: &Path,
    cache: &HashMap<String, FolderHashEntry>,
    new_entries: &Mutex<Vec<(String, FolderHashEntry)>>,
) -> Option<(PathBuf, PathBuf, ImageHash)> {
    let path_key = path.to_string_lossy().into_owned();
    let (size, mtime) = file_size_mtime(path)?;

    if let Some(entry) = cache.get(&path_key) {
        if entry.size == size && entry.mtime_secs == mtime {
            let preview_path = preview_dir.join(&entry.preview_filename);
            if preview_path.exists() {
                if let Ok(hash) = ImageHash::from_base64(&entry.hash_b64) {
                    return Some((path.to_path_buf(), preview_path, hash));
                }
            }
        }
    }

    let (hash, preview_path) = hash_and_preview(path, preview_dir).ok()?;
    let preview_filename = preview_path
        .file_name()
        .and_then(|n| n.to_str())
        .map(|s| s.to_string())?;
    let entry = FolderHashEntry {
        mtime_secs: mtime,
        size,
        hash_b64: hash.to_base64(),
        preview_filename,
    };
    if let Ok(mut guard) = new_entries.lock() {
        guard.push((path_key, entry));
    }
    Some((path.to_path_buf(), preview_path, hash))
}

fn hash_file(path: &Path) -> Result<ImageHash, String> {
    let img = decode_image(path)?;
    let hasher = HasherConfig::new().to_hasher();
    Ok(hasher.hash_image(&img))
}

fn decode_image(path: &Path) -> Result<image::DynamicImage, String> {
    image::ImageReader::open(path)
        .map_err(|e| format!("{}: open: {}", path.display(), e))?
        .with_guessed_format()
        .map_err(|e| format!("{}: sniff: {}", path.display(), e))?
        .decode()
        .map_err(|e| format!("{}: {}", path.display(), e))
}

fn preview_key(path: &Path) -> String {
    let mut h = Sha256::new();
    h.update(path.to_string_lossy().as_bytes());
    let digest = URL_SAFE_NO_PAD.encode(h.finalize());
    digest[..16].to_string()
}

fn hash_and_preview(path: &Path, preview_dir: &Path) -> Result<(ImageHash, PathBuf), String> {
    let img = decode_image(path)?;
    let hasher = HasherConfig::new().to_hasher();
    let hash = hasher.hash_image(&img);

    let preview_path = preview_dir.join(format!("{}.jpg", preview_key(path)));
    if !preview_path.exists() {
        let preview = img.thumbnail(512, 512).to_rgb8();
        preview
            .save(&preview_path)
            .map_err(|e| format!("preview save: {}", e))?;
    }
    Ok((hash, preview_path))
}

/// Generate a preview thumbnail without computing the perceptual hash.
///
/// Used by the single-image replace flow, where the hash is never consumed.
/// Skipping it avoids a second full-image resampling pass over the source.
pub(crate) fn preview_only(path: &Path, preview_dir: &Path) -> Result<PathBuf, String> {
    let preview_path = preview_dir.join(format!("{}.jpg", preview_key(path)));
    if preview_path.exists() {
        return Ok(preview_path);
    }

    let img = decode_image(path)?;
    let preview = img.thumbnail(512, 512).to_rgb8();
    preview
        .save(&preview_path)
        .map_err(|e| format!("preview save: {}", e))?;

    Ok(preview_path)
}

#[derive(Serialize, Clone)]
struct MatchProgress {
    phase: &'static str,
    done: u32,
    total: u32,
}

#[derive(Serialize)]
struct Match {
    image_id: u64,
    thumbnail_path: String,
    file_path: String,
    file_preview_path: String,
    distance: u32,
}

#[derive(Serialize)]
struct MatchResult {
    matches: Vec<Match>,
    unmatched_thumbnails: Vec<u64>,
    unmatched_files: Vec<String>,
    threshold: u32,
}

fn emit_match_progress(app: &tauri::AppHandle, phase: &'static str, done: u32, total: u32) {
    let _ = app.emit("match-progress", MatchProgress { phase, done, total });
}

#[derive(Serialize)]
struct CacheStatus {
    cache_dir: String,
    exists: bool,
    thumbnail_count: u32,
    hashed_count: u32,
}

#[tauri::command]
async fn get_thumbnail_cache_status(
    app: tauri::AppHandle,
    collection_id: u64,
) -> Result<CacheStatus, String> {
    let dir = collection_dir(&app, collection_id)?;
    let thumbs_dir = dir.join("thumbnails");
    let hashes_path = dir.join(HASHES_FILE);

    if !thumbs_dir.is_dir() {
        return Ok(CacheStatus {
            cache_dir: thumbs_dir.to_string_lossy().into_owned(),
            exists: false,
            thumbnail_count: 0,
            hashed_count: 0,
        });
    }

    let thumbnail_count = std::fs::read_dir(&thumbs_dir)
        .map_err(|e| e.to_string())?
        .filter_map(|e| e.ok())
        .filter(|e| {
            let path = e.path();
            path.is_file()
                && path
                    .file_stem()
                    .and_then(|s| s.to_str())
                    .and_then(|s| s.parse::<u64>().ok())
                    .is_some()
        })
        .count() as u32;

    let hashed_count = load_hashes(&hashes_path).len() as u32;

    Ok(CacheStatus {
        cache_dir: thumbs_dir.to_string_lossy().into_owned(),
        exists: true,
        thumbnail_count,
        hashed_count,
    })
}

#[tauri::command]
async fn clear_thumbnail_cache(app: tauri::AppHandle, collection_id: u64) -> Result<(), String> {
    // Blow away the raw thumbnail files and their hashes. folder_hashes.json,
    // folder_previews/, and replacements.json are preserved so that re-running
    // a match doesn't re-hash the user's (potentially thousands of) TIFs.
    let coll_dir = collection_dir(&app, collection_id)?;
    let thumbs_dir = coll_dir.join("thumbnails");
    if thumbs_dir.exists() {
        std::fs::remove_dir_all(&thumbs_dir).map_err(|e| e.to_string())?;
    }
    let hashes_path = coll_dir.join(HASHES_FILE);
    if hashes_path.exists() {
        std::fs::remove_file(&hashes_path).map_err(|e| e.to_string())?;
    }
    Ok(())
}

#[derive(Serialize, Clone)]
struct FolderHashProgress {
    done: u32,
    total: u32,
}

#[derive(Serialize)]
struct FolderHashResult {
    folder: String,
    total: u32,
    hashed: u32,
    cached: u32,
    failed: u32,
}

fn list_folder_image_files(folder: &Path) -> Result<Vec<PathBuf>, String> {
    let mut out: Vec<PathBuf> = Vec::new();
    for entry in std::fs::read_dir(folder).map_err(|e| e.to_string())? {
        let entry = entry.map_err(|e| e.to_string())?;
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        let ext_ok = path
            .extension()
            .and_then(|e| e.to_str())
            .map(|e| IMAGE_EXTS.contains(&e.to_lowercase().as_str()))
            .unwrap_or(false);
        if ext_ok {
            out.push(path);
        }
    }
    Ok(out)
}

#[tauri::command]
async fn hash_replacement_folder(
    app: tauri::AppHandle,
    collection_id: u64,
    folder: String,
) -> Result<FolderHashResult, String> {
    let dir = collection_dir(&app, collection_id)?;
    let folder_path = PathBuf::from(&folder);
    let folder_files = list_folder_image_files(&folder_path)?;

    let preview_dir = dir.join("folder_previews");
    std::fs::create_dir_all(&preview_dir).map_err(|e| e.to_string())?;
    let folder_hashes_path = dir.join(FOLDER_HASHES_FILE);

    let total = folder_files.len() as u32;
    let _ = app.emit(
        "folder-hash-progress",
        FolderHashProgress { done: 0, total },
    );

    let app_for_folder = app.clone();
    let preview_dir_cloned = preview_dir.clone();
    let folder_hashes_path_cloned = folder_hashes_path.clone();
    let (hashed, failed) = tauri::async_runtime::spawn_blocking(move || {
        let cache = load_folder_hashes(&folder_hashes_path_cloned);
        let new_entries: Mutex<Vec<(String, FolderHashEntry)>> = Mutex::new(Vec::new());
        let done = AtomicU32::new(0);
        let failed = AtomicU32::new(0);

        folder_files.par_iter().for_each(|p| {
            let result = lookup_or_hash(p, &preview_dir_cloned, &cache, &new_entries);
            if result.is_none() {
                failed.fetch_add(1, Ordering::SeqCst);
            }
            let n = done.fetch_add(1, Ordering::SeqCst) + 1;
            let _ = app_for_folder.emit(
                "folder-hash-progress",
                FolderHashProgress { done: n, total },
            );
        });

        let new_pairs = new_entries.into_inner().unwrap_or_default();
        let hashed_count = new_pairs.len() as u32;
        if !new_pairs.is_empty() {
            let mut updated = cache;
            for (k, v) in new_pairs {
                updated.insert(k, v);
            }
            let _ = save_folder_hashes(&folder_hashes_path_cloned, &updated);
        }

        (hashed_count, failed.into_inner())
    })
    .await
    .map_err(|e| e.to_string())?;

    let cached = total.saturating_sub(hashed).saturating_sub(failed);

    Ok(FolderHashResult {
        folder,
        total,
        hashed,
        cached,
        failed,
    })
}

#[tauri::command]
async fn match_replacement_folder(
    app: tauri::AppHandle,
    collection_id: u64,
    folder: String,
    max_distance: u32,
) -> Result<MatchResult, String> {
    let dir = collection_dir(&app, collection_id)?;
    let thumbs_dir = dir.join("thumbnails");
    let hashes_path = dir.join(HASHES_FILE);

    if !thumbs_dir.is_dir() {
        return Err("Thumbnails have not been downloaded yet.".into());
    }

    // --- Phase 1: hash any cached thumbnails that don't have hashes yet ---
    let mut hashes = load_hashes(&hashes_path);

    let mut thumb_entries: Vec<(u64, PathBuf)> = Vec::new();
    for entry in std::fs::read_dir(&thumbs_dir).map_err(|e| e.to_string())? {
        let entry = entry.map_err(|e| e.to_string())?;
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        let stem = match path.file_stem().and_then(|s| s.to_str()) {
            Some(s) => s,
            None => continue,
        };
        let id: u64 = match stem.parse() {
            Ok(n) => n,
            Err(_) => continue,
        };
        thumb_entries.push((id, path));
    }

    let missing: Vec<(u64, PathBuf)> = thumb_entries
        .iter()
        .filter(|(id, _)| !hashes.contains_key(&id.to_string()))
        .cloned()
        .collect();

    let missing_total = missing.len() as u32;
    emit_match_progress(&app, "hashing_thumbnails", 0, missing_total);

    let app_for_hash = app.clone();
    let new_hashes = tauri::async_runtime::spawn_blocking(move || {
        let mut out: HashMap<String, String> = HashMap::new();
        for (i, (id, path)) in missing.iter().enumerate() {
            match hash_file(path) {
                Ok(h) => {
                    out.insert(id.to_string(), h.to_base64());
                }
                Err(e) => {
                    let _ = app_for_hash.emit(
                        "thumbnail-hash-error",
                        ThumbnailHashError {
                            image_id: *id,
                            path: path.to_string_lossy().into_owned(),
                            error: e,
                        },
                    );
                }
            }
            emit_match_progress(
                &app_for_hash,
                "hashing_thumbnails",
                (i + 1) as u32,
                missing_total,
            );
        }
        out
    })
    .await
    .map_err(|e| e.to_string())?;

    for (k, v) in new_hashes {
        hashes.insert(k, v);
    }
    save_hashes(&hashes_path, &hashes)?;

    // --- Phase 2: load folder hashes from cache (populated by hash_replacement_folder) ---
    let folder_path = PathBuf::from(&folder);
    let folder_files = list_folder_image_files(&folder_path)?;

    let preview_dir = dir.join("folder_previews");
    let folder_hashes_path = dir.join(FOLDER_HASHES_FILE);
    let folder_cache = load_folder_hashes(&folder_hashes_path);

    let mut folder_hashes: Vec<(PathBuf, PathBuf, ImageHash)> = Vec::new();
    let mut missing = 0u32;
    for p in folder_files {
        let key = p.to_string_lossy().into_owned();
        let entry = match folder_cache.get(&key) {
            Some(e) => e,
            None => {
                missing += 1;
                continue;
            }
        };
        let preview_path = preview_dir.join(&entry.preview_filename);
        if !preview_path.exists() {
            missing += 1;
            continue;
        }
        let hash = match ImageHash::from_base64(&entry.hash_b64) {
            Ok(h) => h,
            Err(_) => {
                missing += 1;
                continue;
            }
        };
        folder_hashes.push((p, preview_path, hash));
    }

    if missing > 0 {
        return Err(format!(
            "{} folder file{} {} not been hashed yet. Re-run \"Hash folder\".",
            missing,
            if missing == 1 { "" } else { "s" },
            if missing == 1 { "has" } else { "have" }
        ));
    }

    // --- Phase 3: greedy one-to-one match ---
    let thumb_hashes: Vec<(u64, PathBuf, ImageHash)> = thumb_entries
        .into_iter()
        .filter_map(|(id, path)| {
            hashes
                .get(&id.to_string())
                .and_then(|b64| ImageHash::from_base64(b64).ok())
                .map(|h| (id, path, h))
        })
        .collect();

    let pairs_total = (thumb_hashes.len() * folder_hashes.len()) as u32;
    emit_match_progress(&app, "matching", 0, pairs_total);

    let mut pairs: Vec<(u32, usize, usize)> = Vec::with_capacity(pairs_total as usize);
    for (ti, (_, _, th)) in thumb_hashes.iter().enumerate() {
        for (fi, (_, _, fh)) in folder_hashes.iter().enumerate() {
            let d = th.dist(fh);
            if d <= max_distance {
                pairs.push((d, ti, fi));
            }
        }
    }
    pairs.sort_by_key(|(d, _, _)| *d);

    let mut taken_thumbs = vec![false; thumb_hashes.len()];
    let mut taken_files = vec![false; folder_hashes.len()];
    let mut matches: Vec<Match> = Vec::new();

    for (d, ti, fi) in pairs {
        if taken_thumbs[ti] || taken_files[fi] {
            continue;
        }
        taken_thumbs[ti] = true;
        taken_files[fi] = true;
        let (id, thumb_path, _) = &thumb_hashes[ti];
        let (file_path, preview_path, _) = &folder_hashes[fi];
        matches.push(Match {
            image_id: *id,
            thumbnail_path: thumb_path.to_string_lossy().into_owned(),
            file_path: file_path.to_string_lossy().into_owned(),
            file_preview_path: preview_path.to_string_lossy().into_owned(),
            distance: d,
        });
    }

    let unmatched_thumbnails: Vec<u64> = thumb_hashes
        .iter()
        .enumerate()
        .filter(|(i, _)| !taken_thumbs[*i])
        .map(|(_, (id, _, _))| *id)
        .collect();
    let unmatched_files: Vec<String> = folder_hashes
        .iter()
        .enumerate()
        .filter(|(i, _)| !taken_files[*i])
        .map(|(_, (p, _, _))| p.to_string_lossy().into_owned())
        .collect();

    emit_match_progress(&app, "matching", pairs_total, pairs_total);

    Ok(MatchResult {
        matches,
        unmatched_thumbnails,
        unmatched_files,
        threshold: max_distance,
    })
}

// ---------------------------------------------------------------------------
// Replacement uploads
// ---------------------------------------------------------------------------

const REPLACEMENTS_FILE: &str = "replacements.json";

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
enum ReplacementStatus {
    Pending,
    Committed,
    Failed,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
struct ReplacementItem {
    image_id: u64,
    file_path: String,
    status: ReplacementStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    error: Option<String>,
}

#[derive(Serialize, Deserialize, Default, Clone)]
struct ReplacementsState {
    #[serde(default)]
    items: Vec<ReplacementItem>,
    #[serde(default)]
    delete_on_success: bool,
}

#[derive(Deserialize)]
struct ReplacementInput {
    image_id: u64,
    file_path: String,
}

#[derive(Serialize)]
struct ReplacementsStateResponse {
    items: Vec<ReplacementItem>,
    active: bool,
    paused: bool,
}

#[derive(Serialize, Clone)]
struct ReplacementProgress {
    image_id: u64,
    status: ReplacementStatus,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
    done: u32,
    total: u32,
}

#[derive(Serialize, Clone)]
struct ReplacementActivity {
    image_id: u64,
    phase: &'static str,
}

#[derive(Serialize, Clone)]
struct ReplacementComplete {
    paused: bool,
}

#[derive(Serialize, Clone)]
struct ReplacementError {
    message: String,
}

struct ActiveReplacements {
    collection_id: u64,
    paused: AtomicBool,
}

#[derive(Default)]
pub struct ReplacementsAppState {
    inner: Mutex<Option<Arc<ActiveReplacements>>>,
}

/// Resets `ReplacementsAppState.inner` to `None` on drop. This runs whether
/// the dispatcher returns normally, panics, or is cancelled by the runtime —
/// without it, a panic inside the spawned task would leave `inner = Some(...)`
/// and every subsequent `start_replacements` would report "already in progress"
/// until the app restarts.
struct DispatcherGuard {
    app: tauri::AppHandle,
}

impl Drop for DispatcherGuard {
    fn drop(&mut self) {
        if let Some(state) = self.app.try_state::<ReplacementsAppState>() {
            if let Ok(mut guard) = state.inner.lock() {
                *guard = None;
            }
        }
    }
}

fn replacements_path(app: &tauri::AppHandle, collection_id: u64) -> Result<PathBuf, String> {
    Ok(collection_dir(app, collection_id)?.join(REPLACEMENTS_FILE))
}

fn load_replacements(path: &Path) -> ReplacementsState {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

fn save_replacements(path: &Path, state: &ReplacementsState) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    let tmp = path.with_extension("json.tmp");
    let data = serde_json::to_vec_pretty(state).map_err(|e| e.to_string())?;
    std::fs::write(&tmp, &data).map_err(|e| e.to_string())?;
    std::fs::rename(&tmp, path).map_err(|e| e.to_string())?;
    Ok(())
}

fn update_replacement_status(
    path: &Path,
    file_lock: &Mutex<()>,
    image_id: u64,
    status: ReplacementStatus,
    error: Option<String>,
) -> Result<(), String> {
    let _guard = file_lock.lock().map_err(|e| e.to_string())?;
    let mut state = load_replacements(path);
    for item in state.items.iter_mut() {
        if item.image_id == image_id {
            item.status = status.clone();
            item.error = error.clone();
            break;
        }
    }
    save_replacements(path, &state)
}

pub(crate) fn content_type_for_file(path: &Path) -> &'static str {
    match path
        .extension()
        .and_then(|e| e.to_str())
        .map(|s| s.to_lowercase())
        .as_deref()
    {
        Some("jpg") | Some("jpeg") | Some("jpe") => "image/jpeg",
        Some("png") => "image/png",
        Some("webp") => "image/webp",
        Some("gif") => "image/gif",
        Some("bmp") => "image/bmp",
        Some("tif") | Some("tiff") => "image/tiff",
        _ => "application/octet-stream",
    }
}

#[derive(Deserialize)]
pub(crate) struct UploadUrlResponse {
    pub(crate) slot_id: String,
    pub(crate) upload_url: String,
    #[serde(default)]
    pub(crate) upload_headers: HashMap<String, String>,
}

pub(crate) struct TokenState {
    pub(crate) access_token: String,
    pub(crate) refresh_token: String,
}

pub(crate) async fn send_with_refresh<F>(
    app: &tauri::AppHandle,
    instance_url: &str,
    tokens: &Mutex<TokenState>,
    build: F,
) -> Result<reqwest::Response, String>
where
    F: Fn(&str) -> reqwest::RequestBuilder,
{
    let access = {
        let t = tokens.lock().map_err(|e| format!("token lock: {}", e))?;
        t.access_token.clone()
    };
    let resp = build(&access).send().await.map_err(|e| e.to_string())?;
    if resp.status() != reqwest::StatusCode::UNAUTHORIZED {
        return Ok(resp);
    }
    drop(resp);

    let refresh = {
        let t = tokens.lock().map_err(|e| format!("token lock: {}", e))?;
        t.refresh_token.clone()
    };
    if refresh.is_empty() {
        return Err("access token expired and no refresh token available".into());
    }
    let new = refresh_tokens_impl(app, instance_url, &refresh).await?;
    let new_access = new.access_token.clone();
    {
        let mut t = tokens.lock().map_err(|e| format!("token lock: {}", e))?;
        t.access_token = new_access.clone();
        if let Some(rt) = &new.refresh_token {
            t.refresh_token = rt.clone();
        }
    }
    let _ = app.emit("tokens-refreshed", &new);

    build(&new_access).send().await.map_err(|e| e.to_string())
}

async fn upload_and_commit_one(
    client: &reqwest::Client,
    app: &tauri::AppHandle,
    instance_url: &str,
    tokens: &Mutex<TokenState>,
    collection_id: u64,
    image_id: u64,
    file_path: &str,
) -> Result<(), String> {
    let path = PathBuf::from(file_path);
    let mime = content_type_for_file(&path);

    let upload_url_endpoint = format!("{}/api/v2/import/upload-url/", instance_url);
    let url_resp = send_with_refresh(app, instance_url, tokens, |token| {
        client
            .get(&upload_url_endpoint)
            .query(&[
                ("collection", collection_id.to_string()),
                ("content_type", mime.to_string()),
            ])
            .bearer_auth(token)
    })
    .await
    .map_err(|e| format!("upload-url request failed: {}", e))?;
    if !url_resp.status().is_success() {
        let status = url_resp.status();
        let body = url_resp.text().await.unwrap_or_default();
        return Err(format!("upload-url {}: {}", status, body.trim()));
    }
    let upload_info: UploadUrlResponse = url_resp
        .json()
        .await
        .map_err(|e| format!("upload-url parse failed: {}", e))?;

    let metadata = tokio::fs::metadata(&path)
        .await
        .map_err(|e| format!("stat {}: {}", file_path, e))?;
    let content_length = metadata.len();
    let file = tokio::fs::File::open(&path)
        .await
        .map_err(|e| format!("open {}: {}", file_path, e))?;
    let stream = tokio_util::io::ReaderStream::new(file);
    let body = reqwest::Body::wrap_stream(stream);

    let mut put_req = client
        .put(&upload_info.upload_url)
        .header("Content-Length", content_length)
        .body(body);
    for (k, v) in upload_info.upload_headers.iter() {
        put_req = put_req.header(k.as_str(), v.as_str());
    }
    let put_resp = put_req
        .send()
        .await
        .map_err(|e| format!("upload PUT failed: {}", e))?;
    if !put_resp.status().is_success() {
        let status = put_resp.status();
        let body = put_resp.text().await.unwrap_or_default();
        return Err(format!("R2 PUT {}: {}", status, body.trim()));
    }

    let replace_endpoint = format!("{}/api/v2/images/{}/replace/", instance_url, image_id);
    let payload = serde_json::json!({ "slot_id": upload_info.slot_id });
    let commit_resp = send_with_refresh(app, instance_url, tokens, |token| {
        client
            .post(&replace_endpoint)
            .bearer_auth(token)
            .json(&payload)
    })
    .await
    .map_err(|e| format!("replace request failed: {}", e))?;
    if !commit_resp.status().is_success() {
        let status = commit_resp.status();
        let body = commit_resp.text().await.unwrap_or_default();
        return Err(format!("replace {}: {}", status, body.trim()));
    }

    Ok(())
}

async fn run_replacements_dispatcher(
    app: tauri::AppHandle,
    state_path: PathBuf,
    active: Arc<ActiveReplacements>,
    instance_url: String,
    tokens: Arc<Mutex<TokenState>>,
    collection_id: u64,
) {
    let client = reqwest::Client::builder()
        .build()
        .expect("failed to build reqwest client");

    let (pending, delete_on_success): (Vec<ReplacementItem>, bool) = {
        let state = load_replacements(&state_path);
        let delete_on_success = state.delete_on_success;
        let pending = state
            .items
            .into_iter()
            .filter(|i| i.status == ReplacementStatus::Pending)
            .collect();
        (pending, delete_on_success)
    };
    let total = pending.len() as u32;
    let done = Arc::new(AtomicU32::new(0));
    let file_lock = Arc::new(Mutex::new(()));

    let paused = active.clone();
    stream::iter(pending)
        .take_while(move |_| {
            let paused = paused.clone();
            async move { !paused.paused.load(Ordering::Relaxed) }
        })
        .for_each_concurrent(2, |item| {
            let app = app.clone();
            let client = client.clone();
            let instance_url = instance_url.clone();
            let tokens = tokens.clone();
            let state_path = state_path.clone();
            let file_lock = file_lock.clone();
            let done = done.clone();
            let active = active.clone();
            async move {
                let _ = app.emit(
                    "replacement-activity",
                    ReplacementActivity {
                        image_id: item.image_id,
                        phase: "uploading",
                    },
                );

                let result = upload_and_commit_one(
                    &client,
                    &app,
                    &instance_url,
                    &tokens,
                    collection_id,
                    item.image_id,
                    &item.file_path,
                )
                .await;

                let (new_status, error) = match result {
                    Ok(()) => (ReplacementStatus::Committed, None),
                    Err(e) => (ReplacementStatus::Failed, Some(e)),
                };

                // Critical: if we can't persist the item's new status, the
                // on-disk batch state diverges from reality (committed items
                // would appear pending on next Resume, causing re-uploads).
                // Surface it and pause the dispatcher so the user notices
                // before the whole batch runs on stale state.
                let persist_result = update_replacement_status(
                    &state_path,
                    &file_lock,
                    item.image_id,
                    new_status.clone(),
                    error.clone(),
                );
                if let Err(e) = &persist_result {
                    active.paused.store(true, Ordering::SeqCst);
                    let _ = app.emit(
                        "replacement-error",
                        ReplacementError {
                            message: format!(
                                "Failed to persist batch state: {}. Paused to prevent re-upload on resume.",
                                e
                            ),
                        },
                    );
                }

                // Only delete the local file after the Committed status is
                // safely on disk — otherwise a persistence failure could leave
                // a deleted file paired with a Pending row that resume tries
                // to re-upload.
                if delete_on_success
                    && new_status == ReplacementStatus::Committed
                    && persist_result.is_ok()
                {
                    if let Err(e) = std::fs::remove_file(&item.file_path) {
                        let _ = app.emit(
                            "replacement-error",
                            ReplacementError {
                                message: format!(
                                    "Uploaded image #{} but failed to delete {}: {}",
                                    item.image_id, item.file_path, e
                                ),
                            },
                        );
                    }
                }

                let n = done.fetch_add(1, Ordering::SeqCst) + 1;
                let _ = app.emit(
                    "replacement-progress",
                    ReplacementProgress {
                        image_id: item.image_id,
                        status: new_status,
                        error,
                        done: n,
                        total,
                    },
                );
            }
        })
        .await;

    let paused_final = active.paused.load(Ordering::Relaxed);
    let _ = app.emit(
        "replacement-complete",
        ReplacementComplete {
            paused: paused_final,
        },
    );
}

#[tauri::command]
async fn start_replacements(
    app: tauri::AppHandle,
    app_state: tauri::State<'_, ReplacementsAppState>,
    collection_id: u64,
    instance_url: String,
    access_token: String,
    refresh_token: String,
    items: Vec<ReplacementInput>,
    delete_on_success: Option<bool>,
) -> Result<(), String> {
    {
        let guard = app_state
            .inner
            .lock()
            .map_err(|e| format!("app state lock: {}", e))?;
        if guard.is_some() {
            return Err("An upload batch is already in progress.".into());
        }
    }

    let state_path = replacements_path(&app, collection_id)?;
    let state = ReplacementsState {
        items: items
            .into_iter()
            .map(|i| ReplacementItem {
                image_id: i.image_id,
                file_path: i.file_path,
                status: ReplacementStatus::Pending,
                error: None,
            })
            .collect(),
        delete_on_success: delete_on_success.unwrap_or(false),
    };
    save_replacements(&state_path, &state)?;

    let active = Arc::new(ActiveReplacements {
        collection_id,
        paused: AtomicBool::new(false),
    });
    {
        let mut guard = app_state
            .inner
            .lock()
            .map_err(|e| format!("app state lock: {}", e))?;
        *guard = Some(active.clone());
    }

    let tokens = Arc::new(Mutex::new(TokenState {
        access_token,
        refresh_token,
    }));
    let app_for_task = app.clone();
    let instance_url = instance_url.trim_end_matches('/').to_string();
    tauri::async_runtime::spawn(async move {
        let _guard = DispatcherGuard {
            app: app_for_task.clone(),
        };
        run_replacements_dispatcher(
            app_for_task,
            state_path,
            active,
            instance_url,
            tokens,
            collection_id,
        )
        .await;
    });

    Ok(())
}

#[tauri::command]
async fn pause_replacements(
    app_state: tauri::State<'_, ReplacementsAppState>,
) -> Result<(), String> {
    let guard = app_state
        .inner
        .lock()
        .map_err(|e| format!("app state lock: {}", e))?;
    if let Some(active) = guard.as_ref() {
        active.paused.store(true, Ordering::SeqCst);
    }
    Ok(())
}

#[tauri::command]
async fn resume_replacements(
    app: tauri::AppHandle,
    app_state: tauri::State<'_, ReplacementsAppState>,
    collection_id: u64,
    instance_url: String,
    access_token: String,
    refresh_token: String,
) -> Result<(), String> {
    {
        let guard = app_state
            .inner
            .lock()
            .map_err(|e| format!("app state lock: {}", e))?;
        if guard.is_some() {
            return Err("An upload batch is already in progress.".into());
        }
    }

    let state_path = replacements_path(&app, collection_id)?;
    if !state_path.exists() {
        return Err("No replacements batch to resume.".into());
    }

    // Resume retries anything not yet committed — failed items get another
    // chance, pending items run for the first time.
    let mut state = load_replacements(&state_path);
    for item in state.items.iter_mut() {
        if item.status == ReplacementStatus::Failed {
            item.status = ReplacementStatus::Pending;
            item.error = None;
        }
    }
    save_replacements(&state_path, &state)?;

    let active = Arc::new(ActiveReplacements {
        collection_id,
        paused: AtomicBool::new(false),
    });
    {
        let mut guard = app_state
            .inner
            .lock()
            .map_err(|e| format!("app state lock: {}", e))?;
        *guard = Some(active.clone());
    }

    let tokens = Arc::new(Mutex::new(TokenState {
        access_token,
        refresh_token,
    }));
    let app_for_task = app.clone();
    let instance_url = instance_url.trim_end_matches('/').to_string();
    tauri::async_runtime::spawn(async move {
        let _guard = DispatcherGuard {
            app: app_for_task.clone(),
        };
        run_replacements_dispatcher(
            app_for_task,
            state_path,
            active,
            instance_url,
            tokens,
            collection_id,
        )
        .await;
    });

    Ok(())
}

#[tauri::command]
async fn get_replacements_state(
    app: tauri::AppHandle,
    app_state: tauri::State<'_, ReplacementsAppState>,
    collection_id: u64,
) -> Result<ReplacementsStateResponse, String> {
    let state_path = replacements_path(&app, collection_id)?;
    let state = load_replacements(&state_path);
    let (active, paused) = {
        let guard = app_state
            .inner
            .lock()
            .map_err(|e| format!("app state lock: {}", e))?;
        match guard.as_ref() {
            Some(a) if a.collection_id == collection_id => (true, a.paused.load(Ordering::Relaxed)),
            _ => (false, false),
        }
    };
    Ok(ReplacementsStateResponse {
        items: state.items,
        active,
        paused,
    })
}

#[tauri::command]
async fn clear_all_collection_caches(app: tauri::AppHandle) -> Result<(), String> {
    // Called on logout. Collection caches aren't scoped by instance on disk,
    // so logging out of any instance clears the whole collections/ tree.
    let cache_root = app
        .path()
        .app_cache_dir()
        .map_err(|e| format!("cache dir: {}", e))?;
    let collections_dir = cache_root.join("collections");
    if collections_dir.exists() {
        std::fs::remove_dir_all(&collections_dir).map_err(|e| e.to_string())?;
    }
    Ok(())
}

#[tauri::command]
async fn clear_replacements(
    app: tauri::AppHandle,
    app_state: tauri::State<'_, ReplacementsAppState>,
    collection_id: u64,
) -> Result<(), String> {
    {
        let guard = app_state
            .inner
            .lock()
            .map_err(|e| format!("app state lock: {}", e))?;
        if let Some(active) = guard.as_ref() {
            if active.collection_id == collection_id {
                return Err("Pause the in-progress batch before clearing.".into());
            }
        }
    }
    let path = replacements_path(&app, collection_id)?;
    if path.exists() {
        std::fs::remove_file(&path).map_err(|e| e.to_string())?;
    }
    Ok(())
}

#[tauri::command]
async fn preview_replacement_file(
    app: tauri::AppHandle,
    file_path: String,
) -> Result<String, String> {
    let cache_root = app
        .path()
        .app_cache_dir()
        .map_err(|e| format!("cache dir: {}", e))?;
    let preview_dir = cache_root.join("replacement_previews");
    let path = PathBuf::from(&file_path);
    tauri::async_runtime::spawn_blocking(move || {
        std::fs::create_dir_all(&preview_dir).map_err(|e| e.to_string())?;
        let preview_path = preview_only(&path, &preview_dir)?;
        Ok::<String, String>(preview_path.to_string_lossy().into_owned())
    })
    .await
    .map_err(|e| format!("preview task: {}", e))?
}

#[tauri::command]
async fn replace_single_image(
    app: tauri::AppHandle,
    instance_url: String,
    access_token: String,
    refresh_token: String,
    collection_id: u64,
    image_id: u64,
    file_path: String,
) -> Result<(), String> {
    let client = reqwest::Client::builder()
        .build()
        .map_err(|e| e.to_string())?;
    let tokens = Mutex::new(TokenState {
        access_token,
        refresh_token,
    });
    let instance_url = instance_url.trim_end_matches('/').to_string();
    upload_and_commit_one(
        &client,
        &app,
        &instance_url,
        &tokens,
        collection_id,
        image_id,
        &file_path,
    )
    .await
}

#[tauri::command]
async fn refresh_access_token(
    app: tauri::AppHandle,
    instance_url: String,
    refresh_token: String,
) -> Result<TokenResponse, String> {
    let instance_url = instance_url.trim_end_matches('/').to_string();
    refresh_tokens_impl(&app, &instance_url, &refresh_token).await
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_store::Builder::default().build())
        .manage(AuthState::default())
        .manage(ReplacementsAppState::default())
        .manage(imports::ImportsAppState::default())
        .invoke_handler(tauri::generate_handler![
            validate_instance,
            start_oauth_login,
            cancel_oauth_login,
            refresh_access_token,
            download_thumbnails,
            get_thumbnail_cache_status,
            clear_thumbnail_cache,
            clear_all_collection_caches,
            hash_replacement_folder,
            match_replacement_folder,
            start_replacements,
            pause_replacements,
            resume_replacements,
            get_replacements_state,
            clear_replacements,
            preview_replacement_file,
            replace_single_image,
            imports::preflight_imports,
            imports::start_imports,
            imports::pause_imports,
            imports::resume_imports,
            imports::get_imports_state,
            imports::clear_imports,
            prepare::scan_prepare_folder,
            prepare::save_prepare_folder,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
