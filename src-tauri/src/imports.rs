use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use serde::{Deserialize, Serialize};
use serde_json::Value;
use tauri::{Emitter, Manager};

use crate::{
    collection_dir, content_type_for_file, send_with_refresh, TokenState, UploadUrlResponse,
};

const IMAGE_EXTENSIONS: &[&str] = &["jpg", "jpeg", "tif", "tiff", "png", "webp"];
const SIDECAR_FIELDS: &[&str] = &[
    "filename",
    "title",
    "original_date",
    "edtf_date",
    "source_url",
    "description",
    "creator",
    "reference_id",
    "license_name",
    "rotation",
    "mirror",
    "subjects",
];

// ---------------------------------------------------------------------------
// Sidecar schema
// ---------------------------------------------------------------------------

/// Parsed, validated sidecar — the canonical form the dispatcher works with.
#[derive(Clone, Debug, Serialize)]
pub struct Sidecar {
    pub filename: String,
    pub title: String,
    pub original_date: String,
    pub edtf_date: String,
    pub source_url: Option<String>,
    pub description: Option<String>,
    pub creator: Option<String>,
    pub reference_id: Option<String>,
    pub license_name: Option<String>,
    pub rotation: i64,
    pub mirror: String,
    /// Subject slugs to tag, in order. Absent in the sidecar means none.
    pub subjects: Vec<String>,
}

fn require_present<'a>(
    map: &'a serde_json::Map<String, Value>,
    key: &str,
) -> Result<&'a Value, String> {
    map.get(key)
        .ok_or_else(|| format!("missing required field `{}`", key))
}

fn read_string_required(
    map: &serde_json::Map<String, Value>,
    key: &str,
) -> Result<String, String> {
    let v = require_present(map, key)?;
    match v {
        Value::String(s) if !s.is_empty() => Ok(s.clone()),
        Value::String(_) => Err(format!("`{}` must be a non-empty string", key)),
        Value::Null => Err(format!("`{}` cannot be null", key)),
        _ => Err(format!("`{}` must be a string", key)),
    }
}

fn read_string_or_null(
    map: &serde_json::Map<String, Value>,
    key: &str,
) -> Result<Option<String>, String> {
    let v = require_present(map, key)?;
    match v {
        Value::String(s) => Ok(Some(s.clone())),
        Value::Null => Ok(None),
        _ => Err(format!("`{}` must be a string or null", key)),
    }
}

fn read_rotation(map: &serde_json::Map<String, Value>) -> Result<i64, String> {
    let v = require_present(map, "rotation")?;
    let n = v
        .as_i64()
        .ok_or_else(|| "`rotation` must be an integer".to_string())?;
    if matches!(n, 0 | 90 | 180 | 270) {
        Ok(n)
    } else {
        Err(format!("`rotation` must be 0, 90, 180, or 270 (got {})", n))
    }
}

fn read_mirror(map: &serde_json::Map<String, Value>) -> Result<String, String> {
    let v = require_present(map, "mirror")?;
    match v {
        Value::String(s) if matches!(s.as_str(), "none" | "h" | "v") => Ok(s.clone()),
        Value::String(other) => Err(format!(
            "`mirror` must be \"none\", \"h\", or \"v\" (got \"{}\")",
            other
        )),
        _ => Err("`mirror` must be a string".to_string()),
    }
}

/// Read the optional `subjects` list of subject slugs. An absent key or an
/// explicit `null` both mean "no subjects" (`[]`), so sidecars written before
/// subject support stay valid. When present it must be an array of strings;
/// whether each slug actually *exists* is checked later against the server's
/// set, not here.
fn read_subjects(map: &serde_json::Map<String, Value>) -> Result<Vec<String>, String> {
    match map.get("subjects") {
        None | Some(Value::Null) => Ok(Vec::new()),
        Some(Value::Array(items)) => {
            let mut slugs = Vec::with_capacity(items.len());
            for item in items {
                match item {
                    Value::String(s) => slugs.push(s.clone()),
                    _ => return Err("`subjects` must be an array of strings".to_string()),
                }
            }
            Ok(slugs)
        }
        Some(_) => Err("`subjects` must be an array of strings or null".to_string()),
    }
}

/// Parse a sidecar JSON blob into a [`Sidecar`], rejecting unknown keys,
/// missing keys, and wrong-typed values. Returns the first error encountered.
fn parse_sidecar(raw: &str) -> Result<Sidecar, String> {
    let value: Value =
        serde_json::from_str(raw).map_err(|e| format!("invalid JSON: {}", e))?;
    let map = match value {
        Value::Object(m) => m,
        _ => return Err("sidecar must be a JSON object".to_string()),
    };

    let allowed: HashSet<&str> = SIDECAR_FIELDS.iter().copied().collect();
    let mut unknown: Vec<String> = map
        .keys()
        .filter(|k| !allowed.contains(k.as_str()))
        .cloned()
        .collect();
    unknown.sort();
    if let Some(first) = unknown.first() {
        return Err(format!("unknown field `{}`", first));
    }

    Ok(Sidecar {
        filename: read_string_required(&map, "filename")?,
        title: read_string_required(&map, "title")?,
        original_date: read_string_required(&map, "original_date")?,
        edtf_date: read_string_required(&map, "edtf_date")?,
        source_url: read_string_or_null(&map, "source_url")?,
        description: read_string_or_null(&map, "description")?,
        creator: read_string_or_null(&map, "creator")?,
        reference_id: read_string_or_null(&map, "reference_id")?,
        license_name: read_string_or_null(&map, "license_name")?,
        rotation: read_rotation(&map)?,
        mirror: read_mirror(&map)?,
        subjects: read_subjects(&map)?,
    })
}

// ---------------------------------------------------------------------------
// Pre-flight
// ---------------------------------------------------------------------------

#[derive(Serialize)]
pub struct PreflightPair {
    pub stem: String,
    pub sidecar_path: String,
    pub image_path: String,
    pub sidecar: Sidecar,
}

#[derive(Serialize)]
pub struct CompletedPair {
    pub stem: String,
    pub done_path: String,
    pub image_id: Option<i64>,
}

#[derive(Serialize)]
pub struct PreflightError {
    /// File or stem the error relates to (e.g. "000003.json"), or "folder"
    /// for whole-folder issues.
    pub source: String,
    pub message: String,
}

#[derive(Serialize)]
pub struct PreflightResult {
    pub folder: String,
    pub pending: Vec<PreflightPair>,
    pub completed: Vec<CompletedPair>,
    pub errors: Vec<PreflightError>,
    pub license_names: Vec<String>,
}

#[derive(Deserialize)]
struct LicensesItem {
    name: String,
}

/// Fetch the server's recognized license names so the client can validate
/// every sidecar's `license_name` before any upload.
async fn fetch_license_names(
    client: &reqwest::Client,
    instance_url: &str,
    access_token: &str,
) -> Result<Vec<String>, String> {
    let resp = client
        .get(format!("{}/api/v2/licenses/", instance_url))
        .bearer_auth(access_token)
        .send()
        .await
        .map_err(|e| format!("licenses request failed: {}", e))?;
    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        return Err(format!("licenses {}: {}", status, body.trim()));
    }
    let items: Vec<LicensesItem> = resp
        .json()
        .await
        .map_err(|e| format!("licenses parse failed: {}", e))?;
    Ok(items.into_iter().map(|i| i.name).collect())
}

/// Scan `folder` for paired sidecars and image files, validating numbering,
/// schema, and license names. Returns the pairs ready to import, the
/// already-committed pairs, and a list of any blocking errors.
pub async fn preflight_folder(
    folder: &Path,
    instance_url: &str,
    access_token: &str,
) -> Result<PreflightResult, String> {
    let mut errors: Vec<PreflightError> = Vec::new();

    if !folder.is_dir() {
        return Err(format!(
            "Folder does not exist or is not a directory: {}",
            folder.display()
        ));
    }

    // Gather entries by stem.
    #[derive(Default)]
    struct Entry {
        sidecar: Option<PathBuf>,
        done: Option<PathBuf>,
        image: Option<PathBuf>,
        // Files that *look like* candidates but failed the rules (e.g. .json
        // with bad stem, or extra image extensions). Surfaced as errors.
        strays: Vec<PathBuf>,
    }

    let mut entries: HashMap<String, Entry> = HashMap::new();
    let read_dir = std::fs::read_dir(folder)
        .map_err(|e| format!("read {}: {}", folder.display(), e))?;

    for entry in read_dir {
        let entry = match entry {
            Ok(e) => e,
            Err(e) => {
                errors.push(PreflightError {
                    source: "folder".into(),
                    message: format!("directory entry error: {}", e),
                });
                continue;
            }
        };
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        let name = match path.file_name().and_then(|n| n.to_str()) {
            Some(n) => n.to_string(),
            None => continue,
        };

        // Sidecar: NNNNNN.json
        // Done sidecar: NNNNNN.json.done
        // Image: NNNNNN.{ext}
        if let Some(stem) = name.strip_suffix(".json.done") {
            if !is_six_digits(stem) {
                errors.push(PreflightError {
                    source: name.clone(),
                    message: "stem must be six digits".into(),
                });
                continue;
            }
            entries
                .entry(stem.to_string())
                .or_default()
                .done
                .replace(path.clone());
        } else if let Some(stem) = name.strip_suffix(".json") {
            if !is_six_digits(stem) {
                errors.push(PreflightError {
                    source: name.clone(),
                    message: "stem must be six digits".into(),
                });
                continue;
            }
            entries
                .entry(stem.to_string())
                .or_default()
                .sidecar
                .replace(path.clone());
        } else if let Some(dot) = name.rfind('.') {
            let (stem, ext) = name.split_at(dot);
            let ext = ext.trim_start_matches('.').to_lowercase();
            if !IMAGE_EXTENSIONS.contains(&ext.as_str()) {
                errors.push(PreflightError {
                    source: name.clone(),
                    message: format!(
                        "unrecognized file extension `.{}` (allowed: {})",
                        ext,
                        IMAGE_EXTENSIONS.join(", ")
                    ),
                });
                continue;
            }
            if !is_six_digits(stem) {
                errors.push(PreflightError {
                    source: name.clone(),
                    message: "stem must be six digits".into(),
                });
                continue;
            }
            let e = entries.entry(stem.to_string()).or_default();
            if e.image.is_some() {
                e.strays.push(path.clone());
            } else {
                e.image.replace(path.clone());
            }
        } else {
            errors.push(PreflightError {
                source: name.clone(),
                message: "file has no extension".into(),
            });
        }
    }

    // Drain stray duplicate images into errors.
    for (stem, entry) in entries.iter() {
        for stray in entry.strays.iter() {
            errors.push(PreflightError {
                source: stray
                    .file_name()
                    .and_then(|n| n.to_str())
                    .unwrap_or(stem)
                    .to_string(),
                message: format!(
                    "duplicate image file for stem `{}` (only one image per stem)",
                    stem
                ),
            });
        }
    }

    // Strict contiguous numbering check.
    let mut all_stems: Vec<&String> = entries.keys().collect();
    all_stems.sort();
    if !all_stems.is_empty() {
        let total = all_stems.len();
        for (i, stem) in all_stems.iter().enumerate() {
            let expected = format!("{:06}", i + 1);
            if stem.as_str() != expected {
                errors.push(PreflightError {
                    source: "folder".into(),
                    message: format!(
                        "numbering must be contiguous 000001..{:06}; expected `{}` at position {}, found `{}`",
                        total,
                        expected,
                        i + 1,
                        stem
                    ),
                });
                break;
            }
        }
    }

    // Pair-level checks (each stem's two files match) + parse + license validate.
    // We collect both pending and completed pairs; only pending ones need
    // sidecar parsing.
    let mut pending: Vec<PreflightPair> = Vec::new();
    let mut completed: Vec<CompletedPair> = Vec::new();

    // Fetch license set only if we have at least one pending pair, so a folder
    // of all `.done` pairs (resuming after success) doesn't need a network
    // round-trip.
    let needs_licenses = entries
        .iter()
        .any(|(_, e)| e.sidecar.is_some() && e.image.is_some());
    let licenses: HashSet<String> = if needs_licenses {
        let client = reqwest::Client::new();
        match fetch_license_names(&client, instance_url, access_token).await {
            Ok(names) => names.into_iter().collect(),
            Err(e) => {
                errors.push(PreflightError {
                    source: "server".into(),
                    message: format!("could not fetch license list: {}", e),
                });
                HashSet::new()
            }
        }
    } else {
        HashSet::new()
    };
    let licenses_lower: HashSet<String> = licenses.iter().map(|n| n.to_lowercase()).collect();

    for stem in all_stems {
        let entry = entries.get(stem).expect("stem in map");

        // Already-committed pair: just `.done`, image may or may not still exist.
        if let Some(done_path) = &entry.done {
            if entry.sidecar.is_some() {
                errors.push(PreflightError {
                    source: format!("{}.json", stem),
                    message: "both `.json` and `.json.done` present for the same stem".into(),
                });
                continue;
            }
            let image_id =
                read_done_image_id(done_path).unwrap_or(None);
            completed.push(CompletedPair {
                stem: stem.clone(),
                done_path: done_path.display().to_string(),
                image_id,
            });
            continue;
        }

        let sidecar_path = match &entry.sidecar {
            Some(p) => p,
            None => {
                errors.push(PreflightError {
                    source: format!("{}.*", stem),
                    message: format!("no sidecar `{}.json` for image", stem),
                });
                continue;
            }
        };
        let image_path = match &entry.image {
            Some(p) => p,
            None => {
                errors.push(PreflightError {
                    source: format!("{}.json", stem),
                    message: format!("no image file paired with `{}.json`", stem),
                });
                continue;
            }
        };

        let raw = match std::fs::read_to_string(sidecar_path) {
            Ok(s) => s,
            Err(e) => {
                errors.push(PreflightError {
                    source: format!("{}.json", stem),
                    message: format!("could not read sidecar: {}", e),
                });
                continue;
            }
        };
        let sidecar = match parse_sidecar(&raw) {
            Ok(s) => s,
            Err(e) => {
                errors.push(PreflightError {
                    source: format!("{}.json", stem),
                    message: e,
                });
                continue;
            }
        };

        let image_filename = image_path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("")
            .to_string();
        if sidecar.filename != image_filename {
            errors.push(PreflightError {
                source: format!("{}.json", stem),
                message: format!(
                    "`filename` is \"{}\" but paired image is `{}`",
                    sidecar.filename, image_filename
                ),
            });
            continue;
        }

        if let Some(name) = &sidecar.license_name {
            if !licenses_lower.contains(&name.to_lowercase()) {
                errors.push(PreflightError {
                    source: format!("{}.json", stem),
                    message: format!(
                        "license `{}` is not recognized by this server",
                        name
                    ),
                });
                continue;
            }
        }

        pending.push(PreflightPair {
            stem: stem.clone(),
            sidecar_path: sidecar_path.display().to_string(),
            image_path: image_path.display().to_string(),
            sidecar,
        });
    }

    // Subject slugs are validated server-side at commit, not here: the public
    // subjects list only includes subjects with at least one public image, so a
    // valid subject whose first image is in this very import would be hidden
    // from it — pre-validating against that list would wrongly block the import.
    pending.sort_by(|a, b| a.stem.cmp(&b.stem));
    completed.sort_by(|a, b| a.stem.cmp(&b.stem));

    Ok(PreflightResult {
        folder: folder.display().to_string(),
        pending,
        completed,
        errors,
        license_names: {
            let mut v: Vec<String> = licenses.into_iter().collect();
            v.sort();
            v
        },
    })
}

fn is_six_digits(s: &str) -> bool {
    s.len() == 6 && s.chars().all(|c| c.is_ascii_digit())
}

/// A `.done` sidecar is the original sidecar with an extra `committed` block
/// containing the assigned image_id. Read it back so the UI can show what was
/// imported.
fn read_done_image_id(path: &Path) -> Result<Option<i64>, ()> {
    let raw = std::fs::read_to_string(path).map_err(|_| ())?;
    let v: Value = serde_json::from_str(&raw).map_err(|_| ())?;
    Ok(v.get("committed")
        .and_then(|c| c.get("image_id"))
        .and_then(|i| i.as_i64()))
}

// ---------------------------------------------------------------------------
// Import dispatcher
// ---------------------------------------------------------------------------

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum ImportStatus {
    Pending,
    Committed,
    Failed,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct ImportItem {
    pub stem: String,
    pub sidecar_path: String,
    pub image_path: String,
    pub status: ImportStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub image_id: Option<i64>,
}

#[derive(Serialize, Deserialize, Default, Clone)]
pub struct ImportsState {
    #[serde(default)]
    pub items: Vec<ImportItem>,
}

#[derive(Serialize)]
pub struct ImportsStateResponse {
    items: Vec<ImportItem>,
    active: bool,
    paused: bool,
    folder: Option<String>,
}

#[derive(Serialize, Clone)]
struct ImportProgress {
    stem: String,
    status: ImportStatus,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    image_id: Option<i64>,
    done: u32,
    total: u32,
}

#[derive(Serialize, Clone)]
struct ImportActivity {
    stem: String,
    phase: &'static str,
}

#[derive(Serialize, Clone)]
struct ImportComplete {
    paused: bool,
    /// Stopped on first failure (vs. paused by user vs. ran to completion).
    stopped_on_error: bool,
}

#[derive(Serialize, Clone)]
struct ImportError {
    message: String,
}

pub struct ActiveImports {
    collection_id: u64,
    folder: PathBuf,
    paused: AtomicBool,
}

#[derive(Default)]
pub struct ImportsAppState {
    inner: Mutex<Option<Arc<ActiveImports>>>,
}

/// Resets `ImportsAppState.inner` to `None` whenever the dispatcher task
/// returns — including on panic. Without it, a panic would leave the state
/// marked active forever.
struct ImportsDispatcherGuard {
    app: tauri::AppHandle,
}

impl Drop for ImportsDispatcherGuard {
    fn drop(&mut self) {
        if let Some(state) = self.app.try_state::<ImportsAppState>() {
            if let Ok(mut guard) = state.inner.lock() {
                *guard = None;
            }
        }
    }
}

const IMPORTS_FILE: &str = "imports.json";

fn imports_state_path(app: &tauri::AppHandle, collection_id: u64) -> Result<PathBuf, String> {
    Ok(collection_dir(app, collection_id)?.join(IMPORTS_FILE))
}

fn load_imports_state(path: &Path) -> ImportsState {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

fn save_imports_state(path: &Path, state: &ImportsState) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    let tmp = path.with_extension("json.tmp");
    let data = serde_json::to_vec_pretty(state).map_err(|e| e.to_string())?;
    std::fs::write(&tmp, &data).map_err(|e| e.to_string())?;
    std::fs::rename(&tmp, path).map_err(|e| e.to_string())?;
    Ok(())
}

fn update_import_status(
    path: &Path,
    file_lock: &Mutex<()>,
    stem: &str,
    status: ImportStatus,
    error: Option<String>,
    image_id: Option<i64>,
) -> Result<(), String> {
    let _guard = file_lock.lock().map_err(|e| e.to_string())?;
    let mut state = load_imports_state(path);
    for item in state.items.iter_mut() {
        if item.stem == stem {
            item.status = status.clone();
            item.error = error.clone();
            if let Some(id) = image_id {
                item.image_id = Some(id);
            }
            break;
        }
    }
    save_imports_state(path, &state)
}

/// Build the JSON body for the `/api/v2/import/commit/` endpoint from a
/// sidecar. Strings that the sidecar represents as `null` get serialized as
/// empty strings — the server treats blank as "not provided" for these fields.
fn commit_payload(slot_id: &str, sc: &Sidecar) -> serde_json::Value {
    serde_json::json!({
        "slot_id": slot_id,
        "title": sc.title,
        "original_date": sc.original_date,
        "edtf_date": sc.edtf_date,
        "source_url": sc.source_url.clone().unwrap_or_default(),
        "description": sc.description.clone().unwrap_or_default(),
        "creator": sc.creator.clone().unwrap_or_default(),
        "reference_id": sc.reference_id.clone().unwrap_or_default(),
        "license_name": sc.license_name.clone().unwrap_or_default(),
        "rotation": sc.rotation,
        "mirror": sc.mirror,
        "subjects": sc.subjects,
    })
}

#[derive(Deserialize)]
struct CommitResponse {
    image_id: i64,
}

/// Upload one sidecar+image pair and commit. Returns the new image_id on
/// success.
async fn upload_and_commit_pair(
    client: &reqwest::Client,
    app: &tauri::AppHandle,
    instance_url: &str,
    tokens: &Mutex<TokenState>,
    collection_id: u64,
    sidecar: &Sidecar,
    image_path: &Path,
) -> Result<i64, String> {
    let mime = content_type_for_file(image_path);

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

    let metadata = tokio::fs::metadata(image_path)
        .await
        .map_err(|e| format!("stat {}: {}", image_path.display(), e))?;
    let content_length = metadata.len();
    let file = tokio::fs::File::open(image_path)
        .await
        .map_err(|e| format!("open {}: {}", image_path.display(), e))?;
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

    let commit_endpoint = format!("{}/api/v2/import/commit/", instance_url);
    let payload = commit_payload(&upload_info.slot_id, sidecar);
    let commit_resp = send_with_refresh(app, instance_url, tokens, |token| {
        client
            .post(&commit_endpoint)
            .bearer_auth(token)
            .json(&payload)
    })
    .await
    .map_err(|e| format!("commit request failed: {}", e))?;
    if !commit_resp.status().is_success() {
        let status = commit_resp.status();
        let body = commit_resp.text().await.unwrap_or_default();
        return Err(format!("commit {}: {}", status, body.trim()));
    }
    let parsed: CommitResponse = commit_resp
        .json()
        .await
        .map_err(|e| format!("commit parse failed: {}", e))?;
    Ok(parsed.image_id)
}

/// Rename `NNNNNN.json` → `NNNNNN.json.done`, embedding the new image_id in
/// the renamed file's `committed` block. Returns the path of the renamed file.
fn mark_sidecar_done(sidecar_path: &Path, image_id: i64) -> Result<PathBuf, String> {
    let raw = std::fs::read_to_string(sidecar_path)
        .map_err(|e| format!("read sidecar: {}", e))?;
    let mut v: Value = serde_json::from_str(&raw)
        .map_err(|e| format!("parse sidecar for done-marking: {}", e))?;
    if let Some(map) = v.as_object_mut() {
        map.insert(
            "committed".into(),
            serde_json::json!({
                "image_id": image_id,
                "at": chrono_now_iso8601(),
            }),
        );
    } else {
        return Err("sidecar root must be an object".into());
    }
    let pretty = serde_json::to_vec_pretty(&v).map_err(|e| e.to_string())?;

    let done_path = {
        let mut p = sidecar_path.as_os_str().to_owned();
        p.push(".done");
        PathBuf::from(p)
    };
    let tmp = {
        let mut p = sidecar_path.as_os_str().to_owned();
        p.push(".done.tmp");
        PathBuf::from(p)
    };
    std::fs::write(&tmp, &pretty).map_err(|e| format!("write done sidecar: {}", e))?;
    std::fs::rename(&tmp, &done_path)
        .map_err(|e| format!("install done sidecar: {}", e))?;
    // The original `.json` becomes a stale duplicate; remove it.
    std::fs::remove_file(sidecar_path)
        .map_err(|e| format!("remove original sidecar: {}", e))?;
    Ok(done_path)
}

/// Avoid pulling in the chrono crate; emit RFC 3339 UTC manually.
fn chrono_now_iso8601() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let dur = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    let secs = dur.as_secs();
    let micros = dur.subsec_micros();
    let (sec, min_total, hour_total, mut days) = {
        let s = (secs % 60) as u32;
        let m_total = secs / 60;
        let m = (m_total % 60) as u32;
        let h_total = m_total / 60;
        let h = (h_total % 24) as u32;
        let d = h_total / 24;
        (s, m, h, d)
    };
    // Days since 1970-01-01.
    let mut year = 1970i32;
    loop {
        let leap = is_leap_year(year);
        let days_in_year = if leap { 366 } else { 365 };
        if days < days_in_year {
            break;
        }
        days -= days_in_year;
        year += 1;
    }
    let months_normal = [31, 28, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31];
    let mut month = 1u32;
    let leap = is_leap_year(year);
    for (i, &d_in_m) in months_normal.iter().enumerate() {
        let d_in_m = if i == 1 && leap { 29 } else { d_in_m };
        if (days as u32) < d_in_m {
            month = (i as u32) + 1;
            break;
        }
        days -= d_in_m as u64;
    }
    let day = (days as u32) + 1;
    format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}.{:06}Z",
        year, month, day, hour_total, min_total, sec, micros
    )
}

fn is_leap_year(y: i32) -> bool {
    (y % 4 == 0 && y % 100 != 0) || y % 400 == 0
}

async fn run_imports_dispatcher(
    app: tauri::AppHandle,
    state_path: PathBuf,
    folder: PathBuf,
    active: Arc<ActiveImports>,
    instance_url: String,
    tokens: Arc<Mutex<TokenState>>,
    collection_id: u64,
) {
    let client = reqwest::Client::builder()
        .build()
        .expect("failed to build reqwest client");

    let pending: Vec<ImportItem> = {
        let state = load_imports_state(&state_path);
        state
            .items
            .into_iter()
            .filter(|i| i.status == ImportStatus::Pending)
            .collect()
    };
    let total = pending.len() as u32;
    let mut done: u32 = 0;
    let file_lock = Mutex::new(());
    let mut stopped_on_error = false;

    for item in pending {
        if active.paused.load(Ordering::Relaxed) {
            break;
        }

        let _ = app.emit(
            "import-activity",
            ImportActivity {
                stem: item.stem.clone(),
                phase: "uploading",
            },
        );

        // Re-parse the sidecar fresh from disk: rigorously checked by
        // pre-flight but the operator may have edited a file between
        // pre-flight and start; trust the parser as truth.
        let sidecar_path = PathBuf::from(&item.sidecar_path);
        let raw = match std::fs::read_to_string(&sidecar_path) {
            Ok(s) => s,
            Err(e) => {
                let msg = format!("read sidecar: {}", e);
                let _ = update_import_status(
                    &state_path,
                    &file_lock,
                    &item.stem,
                    ImportStatus::Failed,
                    Some(msg.clone()),
                    None,
                );
                done += 1;
                let _ = app.emit(
                    "import-progress",
                    ImportProgress {
                        stem: item.stem.clone(),
                        status: ImportStatus::Failed,
                        error: Some(msg),
                        image_id: None,
                        done,
                        total,
                    },
                );
                stopped_on_error = true;
                break;
            }
        };
        let sidecar = match parse_sidecar(&raw) {
            Ok(s) => s,
            Err(e) => {
                let _ = update_import_status(
                    &state_path,
                    &file_lock,
                    &item.stem,
                    ImportStatus::Failed,
                    Some(e.clone()),
                    None,
                );
                done += 1;
                let _ = app.emit(
                    "import-progress",
                    ImportProgress {
                        stem: item.stem.clone(),
                        status: ImportStatus::Failed,
                        error: Some(e),
                        image_id: None,
                        done,
                        total,
                    },
                );
                stopped_on_error = true;
                break;
            }
        };

        let image_path = PathBuf::from(&item.image_path);
        let result = upload_and_commit_pair(
            &client,
            &app,
            &instance_url,
            &tokens,
            collection_id,
            &sidecar,
            &image_path,
        )
        .await;

        match result {
            Ok(image_id) => {
                let rename_result = mark_sidecar_done(&sidecar_path, image_id);
                if let Err(e) = rename_result {
                    // The image is uploaded and the DB row exists on the
                    // server, but we couldn't mark the local sidecar as done.
                    // Persist the success state-side so a resume doesn't
                    // re-upload, then stop and surface the issue.
                    let persist = update_import_status(
                        &state_path,
                        &file_lock,
                        &item.stem,
                        ImportStatus::Committed,
                        Some(format!("imported as #{} but failed to rename sidecar: {}", image_id, e)),
                        Some(image_id),
                    );
                    if let Err(pe) = persist {
                        let _ = app.emit(
                            "import-error",
                            ImportError {
                                message: format!(
                                    "Imported #{} but failed to persist state: {}. Resume may re-upload — investigate before continuing.",
                                    image_id, pe
                                ),
                            },
                        );
                    }
                    done += 1;
                    let _ = app.emit(
                        "import-progress",
                        ImportProgress {
                            stem: item.stem.clone(),
                            status: ImportStatus::Committed,
                            error: Some(format!(
                                "imported as #{} but local sidecar rename failed: {}",
                                image_id, e
                            )),
                            image_id: Some(image_id),
                            done,
                            total,
                        },
                    );
                    stopped_on_error = true;
                    break;
                }

                if let Err(pe) = update_import_status(
                    &state_path,
                    &file_lock,
                    &item.stem,
                    ImportStatus::Committed,
                    None,
                    Some(image_id),
                ) {
                    let _ = app.emit(
                        "import-error",
                        ImportError {
                            message: format!(
                                "Imported #{} but failed to persist state: {}. Investigate before resuming.",
                                image_id, pe
                            ),
                        },
                    );
                    stopped_on_error = true;
                    done += 1;
                    let _ = app.emit(
                        "import-progress",
                        ImportProgress {
                            stem: item.stem.clone(),
                            status: ImportStatus::Committed,
                            error: None,
                            image_id: Some(image_id),
                            done,
                            total,
                        },
                    );
                    break;
                }
                done += 1;
                let _ = app.emit(
                    "import-progress",
                    ImportProgress {
                        stem: item.stem.clone(),
                        status: ImportStatus::Committed,
                        error: None,
                        image_id: Some(image_id),
                        done,
                        total,
                    },
                );
            }
            Err(e) => {
                let _ = update_import_status(
                    &state_path,
                    &file_lock,
                    &item.stem,
                    ImportStatus::Failed,
                    Some(e.clone()),
                    None,
                );
                done += 1;
                let _ = app.emit(
                    "import-progress",
                    ImportProgress {
                        stem: item.stem.clone(),
                        status: ImportStatus::Failed,
                        error: Some(e),
                        image_id: None,
                        done,
                        total,
                    },
                );
                stopped_on_error = true;
                break;
            }
        }
    }

    let paused_final = active.paused.load(Ordering::Relaxed);
    let _ = folder; // currently unused inside the loop, but retained for future events
    let _ = app.emit(
        "import-complete",
        ImportComplete {
            paused: paused_final,
            stopped_on_error,
        },
    );
}

// ---------------------------------------------------------------------------
// Tauri commands
// ---------------------------------------------------------------------------

#[tauri::command]
pub async fn preflight_imports(
    folder: String,
    instance_url: String,
    access_token: String,
) -> Result<PreflightResult, String> {
    let folder = PathBuf::from(folder);
    let instance_url = instance_url.trim_end_matches('/').to_string();
    preflight_folder(&folder, &instance_url, &access_token).await
}

#[derive(Deserialize)]
pub struct ImportInput {
    stem: String,
    sidecar_path: String,
    image_path: String,
}

#[tauri::command]
pub async fn start_imports(
    app: tauri::AppHandle,
    app_state: tauri::State<'_, ImportsAppState>,
    collection_id: u64,
    folder: String,
    instance_url: String,
    access_token: String,
    refresh_token: String,
    items: Vec<ImportInput>,
) -> Result<(), String> {
    {
        let guard = app_state
            .inner
            .lock()
            .map_err(|e| format!("app state lock: {}", e))?;
        if guard.is_some() {
            return Err("An import batch is already in progress.".into());
        }
    }

    let state_path = imports_state_path(&app, collection_id)?;
    let state = ImportsState {
        items: items
            .into_iter()
            .map(|i| ImportItem {
                stem: i.stem,
                sidecar_path: i.sidecar_path,
                image_path: i.image_path,
                status: ImportStatus::Pending,
                error: None,
                image_id: None,
            })
            .collect(),
    };
    save_imports_state(&state_path, &state)?;

    let folder = PathBuf::from(folder);
    let active = Arc::new(ActiveImports {
        collection_id,
        folder: folder.clone(),
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
        let _guard = ImportsDispatcherGuard {
            app: app_for_task.clone(),
        };
        run_imports_dispatcher(
            app_for_task,
            state_path,
            folder,
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
pub async fn pause_imports(
    app_state: tauri::State<'_, ImportsAppState>,
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
pub async fn resume_imports(
    app: tauri::AppHandle,
    app_state: tauri::State<'_, ImportsAppState>,
    collection_id: u64,
    folder: String,
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
            return Err("An import batch is already in progress.".into());
        }
    }

    let state_path = imports_state_path(&app, collection_id)?;
    if !state_path.exists() {
        return Err("No import batch to resume.".into());
    }

    // Failed items get another chance; pending items run for the first time.
    let mut state = load_imports_state(&state_path);
    for item in state.items.iter_mut() {
        if item.status == ImportStatus::Failed {
            item.status = ImportStatus::Pending;
            item.error = None;
        }
    }
    save_imports_state(&state_path, &state)?;

    let folder = PathBuf::from(folder);
    let active = Arc::new(ActiveImports {
        collection_id,
        folder: folder.clone(),
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
        let _guard = ImportsDispatcherGuard {
            app: app_for_task.clone(),
        };
        run_imports_dispatcher(
            app_for_task,
            state_path,
            folder,
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
pub async fn get_imports_state(
    app: tauri::AppHandle,
    app_state: tauri::State<'_, ImportsAppState>,
    collection_id: u64,
) -> Result<ImportsStateResponse, String> {
    let state_path = imports_state_path(&app, collection_id)?;
    let state = load_imports_state(&state_path);
    let (active, paused, folder) = {
        let guard = app_state
            .inner
            .lock()
            .map_err(|e| format!("app state lock: {}", e))?;
        match guard.as_ref() {
            Some(a) if a.collection_id == collection_id => (
                true,
                a.paused.load(Ordering::Relaxed),
                Some(a.folder.display().to_string()),
            ),
            _ => (false, false, None),
        }
    };
    Ok(ImportsStateResponse {
        items: state.items,
        active,
        paused,
        folder,
    })
}

#[tauri::command]
pub async fn clear_imports(
    app: tauri::AppHandle,
    app_state: tauri::State<'_, ImportsAppState>,
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
    let path = imports_state_path(&app, collection_id)?;
    if path.exists() {
        std::fs::remove_file(&path).map_err(|e| e.to_string())?;
    }
    Ok(())
}
