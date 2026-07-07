//! Manual import-folder authoring.
//!
//! Where `imports.rs` *consumes* a folder of numbered `NNNNNN.{ext}` +
//! `NNNNNN.json` pairs, this module helps the user *produce* one from an
//! arbitrary folder of images:
//!
//!   1. [`scan_prepare_folder`] lists the images and renders a preview for
//!      each (so TIFFs the webview can't display still show a thumbnail).
//!   2. The frontend lets the user reorder, disable, and fill in metadata.
//!   3. [`save_prepare_folder`] copies the enabled images into a fresh folder
//!      as a contiguous `000001..` series, each with a sidecar written to
//!      exactly the schema `imports::parse_sidecar` accepts — so the output
//!      is guaranteed to pass the import preflight.
//!
//! Both commands are purely local: no network, no tokens.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Mutex;

use rayon::prelude::*;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tauri::{Emitter, Manager};

use crate::preview_only;

/// Extensions accepted by the import API (see `imports::IMAGE_EXTENSIONS`).
/// The prepare tool only surfaces files it can actually turn into a valid
/// import, so it uses the same set — not the broader hashing set.
const PREPARE_EXTS: &[&str] = &["jpg", "jpeg", "tif", "tiff", "png", "webp"];

/// Previews live in a single app-wide cache dir keyed by source-path hash
/// (via `preview_only`), so re-scanning the same folder is cheap and the same
/// image referenced from two collections isn't rendered twice.
const PREPARE_PREVIEW_DIR: &str = "prepare_previews";

// ---------------------------------------------------------------------------
// Scan
// ---------------------------------------------------------------------------

#[derive(Serialize, Clone)]
pub struct PrepareImage {
    /// Absolute path of the original file. Doubles as the row's stable key.
    source_path: String,
    /// Original file name (e.g. `IMG_0421.tif`), shown to the user.
    filename: String,
    /// Lower-cased extension without the dot (e.g. `tif`).
    ext: String,
    /// Preview JPEG path for `convertFileSrc`, or `None` if decoding failed.
    preview_path: Option<String>,
    size: u64,
    /// Set when the image couldn't be decoded; the row is still listed so the
    /// user can see and disable it rather than have it silently vanish.
    error: Option<String>,
}

#[derive(Serialize)]
pub struct PrepareScanResult {
    folder: String,
    images: Vec<PrepareImage>,
}

#[derive(Serialize, Clone)]
struct PrepareScanProgress {
    done: u32,
    total: u32,
}

fn list_prepare_images(folder: &Path) -> Result<Vec<PathBuf>, String> {
    let mut out: Vec<PathBuf> = Vec::new();
    for entry in std::fs::read_dir(folder).map_err(|e| format!("read {}: {}", folder.display(), e))? {
        let entry = entry.map_err(|e| e.to_string())?;
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        let ext_ok = path
            .extension()
            .and_then(|e| e.to_str())
            .map(|e| PREPARE_EXTS.contains(&e.to_lowercase().as_str()))
            .unwrap_or(false);
        if ext_ok {
            out.push(path);
        }
    }
    Ok(out)
}

/// Order filenames the way a person would read them: `IMG_2` before `IMG_10`.
/// A plain lexicographic sort would interleave them; the user can still
/// reorder afterwards, but a sensible default saves a lot of dragging.
fn natural_cmp(a: &str, b: &str) -> std::cmp::Ordering {
    use std::cmp::Ordering;
    let mut ai = a.chars().peekable();
    let mut bi = b.chars().peekable();
    loop {
        match (ai.peek().copied(), bi.peek().copied()) {
            (None, None) => return Ordering::Equal,
            (None, Some(_)) => return Ordering::Less,
            (Some(_), None) => return Ordering::Greater,
            (Some(ca), Some(cb)) => {
                if ca.is_ascii_digit() && cb.is_ascii_digit() {
                    let mut na = String::new();
                    while let Some(c) = ai.peek().copied() {
                        if c.is_ascii_digit() {
                            na.push(c);
                            ai.next();
                        } else {
                            break;
                        }
                    }
                    let mut nb = String::new();
                    while let Some(c) = bi.peek().copied() {
                        if c.is_ascii_digit() {
                            nb.push(c);
                            bi.next();
                        } else {
                            break;
                        }
                    }
                    let ta = na.trim_start_matches('0');
                    let tb = nb.trim_start_matches('0');
                    let ord = ta.len().cmp(&tb.len()).then_with(|| ta.cmp(tb));
                    if ord != Ordering::Equal {
                        return ord;
                    }
                } else {
                    let la = ca.to_ascii_lowercase();
                    let lb = cb.to_ascii_lowercase();
                    if la != lb {
                        return la.cmp(&lb);
                    }
                    ai.next();
                    bi.next();
                }
            }
        }
    }
}

#[tauri::command]
pub async fn scan_prepare_folder(
    app: tauri::AppHandle,
    folder: String,
) -> Result<PrepareScanResult, String> {
    let folder_path = PathBuf::from(&folder);
    if !folder_path.is_dir() {
        return Err(format!(
            "Folder does not exist or is not a directory: {}",
            folder_path.display()
        ));
    }

    let mut files = list_prepare_images(&folder_path)?;
    files.sort_by(|a, b| {
        let an = a.file_name().and_then(|n| n.to_str()).unwrap_or_default();
        let bn = b.file_name().and_then(|n| n.to_str()).unwrap_or_default();
        natural_cmp(an, bn)
    });

    let preview_dir = app
        .path()
        .app_cache_dir()
        .map_err(|e| format!("cache dir: {}", e))?
        .join(PREPARE_PREVIEW_DIR);
    std::fs::create_dir_all(&preview_dir).map_err(|e| format!("create preview dir: {}", e))?;

    let total = files.len() as u32;
    let _ = app.emit("prepare-scan-progress", PrepareScanProgress { done: 0, total });

    let app_for_scan = app.clone();
    let images = tauri::async_runtime::spawn_blocking(move || {
        let done = AtomicU32::new(0);
        // Preview generation is parallel, so results arrive out of order —
        // carry the pre-sorted index and re-sort at the end to restore it.
        let collected: Mutex<Vec<(usize, PrepareImage)>> = Mutex::new(Vec::with_capacity(files.len()));

        files.par_iter().enumerate().for_each(|(idx, path)| {
            let filename = path
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or_default()
                .to_string();
            let ext = path
                .extension()
                .and_then(|e| e.to_str())
                .map(|e| e.to_lowercase())
                .unwrap_or_default();
            let size = std::fs::metadata(path).map(|m| m.len()).unwrap_or(0);
            let (preview_path, error) = match preview_only(path, &preview_dir) {
                Ok(p) => (Some(p.to_string_lossy().into_owned()), None),
                Err(e) => (None, Some(e)),
            };

            if let Ok(mut guard) = collected.lock() {
                guard.push((
                    idx,
                    PrepareImage {
                        source_path: path.to_string_lossy().into_owned(),
                        filename,
                        ext,
                        preview_path,
                        size,
                        error,
                    },
                ));
            }

            let n = done.fetch_add(1, Ordering::SeqCst) + 1;
            let _ = app_for_scan.emit("prepare-scan-progress", PrepareScanProgress { done: n, total });
        });

        let mut v = collected.into_inner().unwrap_or_default();
        v.sort_by_key(|(idx, _)| *idx);
        v.into_iter().map(|(_, img)| img).collect::<Vec<_>>()
    })
    .await
    .map_err(|e| format!("scan task: {}", e))?;

    Ok(PrepareScanResult { folder, images })
}

// ---------------------------------------------------------------------------
// Save
// ---------------------------------------------------------------------------

/// Metadata the frontend collects per image. Optional fields arrive as
/// (possibly empty) strings; empties become JSON `null` in the sidecar.
#[derive(Deserialize)]
pub struct SaveSidecar {
    title: String,
    original_date: String,
    edtf_date: String,
    source_url: String,
    description: String,
    creator: String,
    reference_id: String,
    license_name: String,
    rotation: i64,
    mirror: String,
}

#[derive(Deserialize)]
pub struct SaveItem {
    source_path: String,
    sidecar: SaveSidecar,
}

#[derive(Serialize)]
pub struct SaveResult {
    dest: String,
    written: u32,
}

#[derive(Serialize, Clone)]
struct PrepareSaveProgress {
    done: u32,
    total: u32,
    filename: String,
}

/// Defensive re-validation of the invariants the UI already enforces via its
/// "complete or disabled" save gate. If any of these fire it's a frontend bug,
/// but we'd rather stop than write a folder the import will later reject.
fn validate_sidecar(sc: &SaveSidecar) -> Result<(), String> {
    if sc.title.trim().is_empty() {
        return Err("title is required".into());
    }
    if sc.original_date.trim().is_empty() {
        return Err("original_date is required".into());
    }
    if sc.edtf_date.trim().is_empty() {
        return Err("edtf_date is required".into());
    }
    if !matches!(sc.rotation, 0 | 90 | 180 | 270) {
        return Err(format!("rotation must be 0, 90, 180, or 270 (got {})", sc.rotation));
    }
    if !matches!(sc.mirror.as_str(), "none" | "h" | "v") {
        return Err(format!("mirror must be \"none\", \"h\", or \"v\" (got \"{}\")", sc.mirror));
    }
    Ok(())
}

/// Empty optional string -> JSON `null` ("not provided"); otherwise the string.
fn opt(s: &str) -> Value {
    if s.trim().is_empty() {
        Value::Null
    } else {
        Value::String(s.to_string())
    }
}

fn build_sidecar_json(filename: &str, sc: &SaveSidecar) -> Value {
    json!({
        "filename": filename,
        "title": sc.title,
        "original_date": sc.original_date,
        "edtf_date": sc.edtf_date,
        "source_url": opt(&sc.source_url),
        "description": opt(&sc.description),
        "creator": opt(&sc.creator),
        "reference_id": opt(&sc.reference_id),
        "license_name": opt(&sc.license_name),
        "rotation": sc.rotation,
        "mirror": sc.mirror,
        "subjects": [],
    })
}

#[tauri::command]
pub async fn save_prepare_folder(
    app: tauri::AppHandle,
    dest: String,
    items: Vec<SaveItem>,
) -> Result<SaveResult, String> {
    if items.is_empty() {
        return Err("No enabled images to save.".into());
    }

    let dest_path = PathBuf::from(&dest);
    // Refuse to write into a populated folder: the import expects a clean
    // 000001.. series, and clobbering an existing folder is never what the
    // user meant.
    if dest_path.exists() {
        if !dest_path.is_dir() {
            return Err("Destination exists but is not a folder.".into());
        }
        let mut rd = std::fs::read_dir(&dest_path)
            .map_err(|e| format!("read destination: {}", e))?;
        if rd.next().is_some() {
            return Err("Destination folder is not empty. Choose an empty or new folder.".into());
        }
    } else {
        std::fs::create_dir_all(&dest_path)
            .map_err(|e| format!("create destination: {}", e))?;
    }

    let total = items.len() as u32;
    let app_for_save = app.clone();
    let dest_for_task = dest_path.clone();

    let written = tauri::async_runtime::spawn_blocking(move || -> Result<u32, String> {
        for (i, item) in items.iter().enumerate() {
            let stem = format!("{:06}", i + 1);
            let src = PathBuf::from(&item.source_path);

            let ext = src
                .extension()
                .and_then(|e| e.to_str())
                .map(|s| s.to_lowercase())
                .ok_or_else(|| format!("{}: file has no extension", item.source_path))?;
            if !PREPARE_EXTS.contains(&ext.as_str()) {
                return Err(format!("{}: unsupported extension `.{}`", item.source_path, ext));
            }

            let label = src
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or(&item.source_path);
            validate_sidecar(&item.sidecar)
                .map_err(|e| format!("{} ({}): {}", stem, label, e))?;

            let image_name = format!("{}.{}", stem, ext);

            // Copy image via a temp file + rename so a partially-written file
            // is never left looking complete.
            let dest_image = dest_for_task.join(&image_name);
            let tmp_image = dest_for_task.join(format!("{}.{}.tmp", stem, ext));
            std::fs::copy(&src, &tmp_image)
                .map_err(|e| format!("copy {} -> {}: {}", item.source_path, image_name, e))?;
            std::fs::rename(&tmp_image, &dest_image)
                .map_err(|e| format!("finalize {}: {}", image_name, e))?;

            // Sidecar, same temp+rename discipline.
            let sidecar_name = format!("{}.json", stem);
            let dest_sidecar = dest_for_task.join(&sidecar_name);
            let tmp_sidecar = dest_for_task.join(format!("{}.json.tmp", stem));
            let sidecar = build_sidecar_json(&image_name, &item.sidecar);
            let data = serde_json::to_vec_pretty(&sidecar)
                .map_err(|e| format!("serialize {}: {}", sidecar_name, e))?;
            std::fs::write(&tmp_sidecar, &data)
                .map_err(|e| format!("write {}: {}", sidecar_name, e))?;
            std::fs::rename(&tmp_sidecar, &dest_sidecar)
                .map_err(|e| format!("finalize {}: {}", sidecar_name, e))?;

            let _ = app_for_save.emit(
                "prepare-save-progress",
                PrepareSaveProgress {
                    done: (i + 1) as u32,
                    total,
                    filename: image_name,
                },
            );
        }
        Ok(total)
    })
    .await
    .map_err(|e| format!("save task: {}", e))??;

    Ok(SaveResult { dest, written })
}
