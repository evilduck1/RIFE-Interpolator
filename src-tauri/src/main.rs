// Prevents additional console window on Windows in release, DO NOT REMOVE!!
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn emit_log_limited(app: &tauri::AppHandle, msg: &str) {
    let mut s = msg.trim().to_string();
    if s.len() > 400 {
        s.truncate(400);
        s.push_str("…");
    }
    if !s.is_empty() {
        let _ = app.emit("pipeline_log", s);
    }
}

fn emit_stage(app: &tauri::AppHandle, msg: &str) {
    let _ = app.emit("pipeline_stage", msg.to_string());
}


fn parse_ffmpeg_progress_line(line: &str) -> Option<(&str, &str)> {
    let mut it = line.splitn(2, '=');
    let k = it.next()?.trim();
    let v = it.next()?.trim();
    if k.is_empty() { return None; }
    Some((k, v))
}

fn probe_duration_and_fps(ffmpeg: &Path, input: &Path) -> Option<(f64, f64)> {
    let ffprobe = ffmpeg.parent()?.join("ffprobe");
    if !ffprobe.exists() { return None; }

    let dur_out = Command::new(&ffprobe)
        .arg("-v").arg("error")
        .arg("-show_entries").arg("format=duration")
        .arg("-of").arg("default=noprint_wrappers=1:nokey=1")
        .arg(input)
        .output().ok()?;
    let duration = String::from_utf8_lossy(&dur_out.stdout).trim().parse::<f64>().ok()?;

    let fps_out = Command::new(&ffprobe)
        .arg("-v").arg("error")
        .arg("-select_streams").arg("v:0")
        .arg("-show_entries").arg("stream=r_frame_rate")
        .arg("-of").arg("default=noprint_wrappers=1:nokey=1")
        .arg(input)
        .output().ok()?;
    let fps_s = String::from_utf8_lossy(&fps_out.stdout).trim().to_string();
    let fps = if let Some((a,b)) = fps_s.split_once('/') {
        let na = a.parse::<f64>().ok()?;
        let nb = b.parse::<f64>().ok()?;
        if nb != 0.0 { na/nb } else { 0.0 }
    } else {
        fps_s.parse::<f64>().unwrap_or(0.0)
    };

    Some((duration, fps))
}


fn preferred_ffmpeg_path() -> Option<PathBuf> {
    let candidates = [
        "/opt/homebrew/opt/ffmpeg-full/bin/ffmpeg",
        "/opt/homebrew/bin/ffmpeg",
        "/usr/local/bin/ffmpeg",
        "/usr/bin/ffmpeg",
    ];
    for c in candidates {
        let p = PathBuf::from(c);
        if p.exists() {
            return Some(p);
        }
    }
    None
}


use std::fs;
use std::io::BufRead;
use std::io::BufReader;
use std::process::Stdio;
use std::path::{Path, PathBuf};
use std::process::Command;

use tauri::{AppHandle, Manager};
use tauri::Emitter;

#[tauri::command]
fn check_environment() -> String {
    let os = std::env::consts::OS;
    let arch = std::env::consts::ARCH;
    format!("Environment OK | OS: {} | ARCH: {}", os, arch)
}


#[tauri::command]
fn get_max_threads_string() -> String {
    let n = std::thread::available_parallelism().map(|v| v.get()).unwrap_or(4);
    format!("{0}:{0}:{0}", n)
}

#[tauri::command]
fn get_default_rife_model_dir(app: AppHandle) -> Option<String> {
    let root = app_root(&app).ok()?;
    ensure_dirs(&root).ok()?;
    let (_ffmpeg, _rife, rife_models) = find_installed_tool_paths(&root);
    rife_models.map(|p| p.to_string_lossy().to_string())
}


fn app_root(app: &AppHandle) -> Result<PathBuf, String> {
    let data_dir = app
        .path()
        .app_data_dir()
        .map_err(|e| format!("Failed to resolve app data dir: {e}"))?;
    Ok(data_dir.join("RIFE-Interpolator"))
}

fn ensure_dirs(root: &Path) -> Result<(), String> {
    let dirs = [
        root.join("bin"),
        root.join("bin/ffmpeg"),
        root.join("bin/rife"),
        root.join("models"),
        root.join("temp"),
        root.join("cache"),
    ];

    for d in dirs {
        fs::create_dir_all(&d)
            .map_err(|e| format!("Failed to create dir {}: {e}", d.to_string_lossy()))?;
    }
    Ok(())
}

fn copy_dir_recursive(src: &Path, dst: &Path) -> Result<(), String> {
    fs::create_dir_all(dst).map_err(|e| e.to_string())?;

    for entry in fs::read_dir(src).map_err(|e| e.to_string())? {
        let entry = entry.map_err(|e| e.to_string())?;
        let src_path = entry.path();
        let dst_path = dst.join(entry.file_name());

        if src_path.is_dir() {
            copy_dir_recursive(&src_path, &dst_path)?;
        } else {
            fs::copy(&src_path, &dst_path).map_err(|e| e.to_string())?;

            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let mut perms =
                    fs::metadata(&dst_path).map_err(|e| e.to_string())?.permissions();
                perms.set_mode(0o755);
                fs::set_permissions(&dst_path, perms).map_err(|e| e.to_string())?;
            }
        }
    }
    Ok(())
}

#[tauri::command]
fn get_app_paths(app: AppHandle) -> Result<Vec<String>, String> {
    let root = app_root(&app)?;
    ensure_dirs(&root)?;

    let paths = vec![
        root.clone(),
        root.join("bin/ffmpeg"),
        root.join("bin/rife"),
        root.join("models"),
        root.join("temp"),
        root.join("cache"),
    ];

    Ok(paths
        .into_iter()
        .map(|p| p.to_string_lossy().to_string())
        .collect())
}

fn has_rife_executable(dir: &Path) -> bool {
    if let Ok(entries) = fs::read_dir(dir) {
        for e in entries.flatten() {
            let p = e.path();
            if p.is_file() {
                if let Some(name) = p.file_name().and_then(|n| n.to_str()) {
                    if name.to_ascii_lowercase().starts_with("rife") {
                        return true;
                    }
                }
            }
        }
    }
    false
}

#[tauri::command]
fn tool_status(app: AppHandle, tool: String) -> Result<String, String> {
    let root = app_root(&app)?;
    let tool_dir = root.join("bin").join(&tool);

    if !tool_dir.exists() {
        return Ok("missing".into());
    }

    if let Ok(versions) = fs::read_dir(&tool_dir) {
        for v in versions.flatten() {
            let p = v.path();
            if !p.is_dir() {
                continue;
            }

            if tool == "rife" {
                if has_rife_executable(&p) {
                    return Ok("installed".into());
                }
            } else {
                // ffmpeg: any file in any version folder counts as installed
                if let Ok(files) = fs::read_dir(&p) {
                    for f in files.flatten() {
                        if f.path().is_file() {
                            return Ok("installed".into());
                        }
                    }
                }
            }
        }
    }

    Ok("missing".into())
}

#[tauri::command]
fn install_tool(
    app: AppHandle,
    source_path: String,
    tool: String,
    version: String,
) -> Result<String, String> {
    let src = Path::new(&source_path);
    if !src.exists() {
        return Err("Source path does not exist".into());
    }

    let root = app_root(&app)?;
    ensure_dirs(&root)?;

    let dest_dir = root.join("bin").join(&tool).join(&version);
    fs::create_dir_all(&dest_dir).map_err(|e| e.to_string())?;

    if src.is_dir() {
        // RIFE-style folder install (binary + models)
        copy_dir_recursive(src, &dest_dir)?;
        Ok(dest_dir.to_string_lossy().to_string())
    } else {
        // ffmpeg-style single binary
        let filename = src.file_name().ok_or("Invalid file")?;
        let dest = dest_dir.join(filename);
        fs::copy(src, &dest).map_err(|e| e.to_string())?;

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut perms =
                fs::metadata(&dest).map_err(|e| e.to_string())?.permissions();
            perms.set_mode(0o755);
            fs::set_permissions(&dest, perms).map_err(|e| e.to_string())?;
        }

        Ok(dest.to_string_lossy().to_string())
    }
}

// -------------------- Validation helpers --------------------

fn find_ffmpeg_in_version_dir(dir: &Path) -> Option<PathBuf> {
    let entries = fs::read_dir(dir).ok()?;
    for e in entries.flatten() {
        let p = e.path();
        if p.is_file() {
            return Some(p);
        }
    }
    None
}

fn find_rife_in_version_dir(dir: &Path) -> Option<PathBuf> {
    let entries = fs::read_dir(dir).ok()?;
    for e in entries.flatten() {
        let p = e.path();
        if !p.is_file() {
            continue;
        }
        let name = p.file_name()?.to_str()?.to_ascii_lowercase();
        if name.starts_with("rife") {
            return Some(p);
        }
    }
    None
}


fn resolve_rife_model_path(models_root_or_model: &str) -> PathBuf {
    let p = PathBuf::from(models_root_or_model);
    if p.join("flownet.bin").exists() || p.join("model.param").exists() {
        return p;
    }
    // Prefer rife-v2.3 if present
    let preferred = p.join("rife-v2.3");
    if preferred.exists() {
        return preferred;
    }
    // Otherwise pick the first subdirectory
    if let Ok(rd) = std::fs::read_dir(&p) {
        for e in rd.flatten() {
            let path = e.path();
            if path.is_dir() {
                return path;
            }
        }
    }
    p
}

/// Some Windows builds of rife-ncnn-vulkan treat drive-letter absolute paths (e.g. `C:\\...`)
/// as *relative* paths when loading model files, resulting in `_wfopen .../flownet.param failed`.
///
/// The most reliable way to run these builds is:
/// - set the working directory to the directory containing the RIFE executable
/// - pass a *relative* model folder name (e.g. `rife-v4.6`) to `-m`
///
/// If the selected model folder is not next to the executable, we fall back to passing an
/// absolute path, and (on Windows) we optionally prefix with `\\?\`.
fn compute_rife_cwd_and_model_arg(rife_bin: &Path, model_path: &Path) -> (Option<PathBuf>, std::ffi::OsString) {
    let rife_dir = rife_bin.parent().map(|p| p.to_path_buf());

    if let Some(ref dir) = rife_dir {
        if let Some(model_parent) = model_path.parent() {
            if model_parent == dir {
                if let Some(name) = model_path.file_name() {
                    return (Some(dir.clone()), name.to_os_string());
                }
            }
        }
    }

    // Fallback: absolute model path.
    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStrExt;
        // If it's already a verbatim path (\\?\), leave it.
        let s = model_path.to_string_lossy();
        if s.starts_with("\\\\?\\") {
            return (rife_dir, model_path.as_os_str().to_os_string());
        }
        // Prefix with \\?\ to avoid drive-letter absolute path mishandling.
        let mut wide: Vec<u16> = "\\\\?\\".encode_utf16().collect();
        wide.extend(model_path.as_os_str().encode_wide());
        return (rife_dir, std::ffi::OsString::from_wide(&wide));
    }

    #[cfg(not(windows))]
    {
        (rife_dir, model_path.as_os_str().to_os_string())
    }
}

fn find_models_dir(dir: &Path) -> Option<PathBuf> {
    // Accept common RIFE layouts:
    // - folders like 'rife-v2.3', 'rife-v4', 'rife-anime', 'rife-UHD', etc.
    // - models/ (some older builds)

    let direct = dir.join("models");
    if direct.is_dir() {
        return Some(direct);
    }

    // Prefer the default model folder if present
    let preferred = dir.join("rife-v2.3");
    if preferred.is_dir() {
        return Some(preferred);
    }

    // Otherwise pick the first folder starting with "rife-"
    let mut candidates: Vec<PathBuf> = Vec::new();
    if let Ok(entries) = fs::read_dir(dir) {
        for e in entries.flatten() {
            let p = e.path();
            if p.is_dir() {
                if let Some(name) = p.file_name().and_then(|n| n.to_str()) {
                    if name.starts_with("rife-") {
                        candidates.push(p);
                    }
                }
            }
        }
    }
    candidates.sort_by(|a, b| a.file_name().cmp(&b.file_name()));
    candidates.into_iter().next()
}

fn find_installed_tool_paths(root: &Path) -> (Option<PathBuf>, Option<PathBuf>, Option<PathBuf>) {
    let ffmpeg_root = root.join("bin/ffmpeg");
    let rife_root = root.join("bin/rife");

    let mut ffmpeg_path: Option<PathBuf> = None;
    let mut rife_path: Option<PathBuf> = None;
    let mut rife_models: Option<PathBuf> = None;

    if let Ok(versions) = fs::read_dir(&ffmpeg_root) {
        for v in versions.flatten() {
            let p = v.path();
            if p.is_dir() {
                if let Some(bin) = find_ffmpeg_in_version_dir(&p) {
                    ffmpeg_path = Some(bin);
                    break;
                }
            }
        }
    }

    if let Ok(versions) = fs::read_dir(&rife_root) {
        for v in versions.flatten() {
            let p = v.path();
            if p.is_dir() {
                if let Some(bin) = find_rife_in_version_dir(&p) {
                    rife_path = Some(bin);
                    rife_models = find_models_dir(&p);
                    break;
                }
            }
        }
    }

    (ffmpeg_path, rife_path, rife_models)
}

#[derive(serde::Serialize)]
struct ToolValidation {
    ok: bool,
    path: Option<String>,
    output: String,
}

#[derive(serde::Serialize)]
struct ValidateToolsResult {
    ffmpeg: ToolValidation,
    rife: ToolValidation,
}

#[derive(serde::Serialize)]
struct ExtractFramesResult {
    ok: bool,
    frames_dir: String,
    frame_pattern: String,
    output: String,
}

fn make_job_id() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    format!("job-{}-{}", now.as_secs(), now.subsec_nanos())
}

fn count_files_in_dir(dir: &Path) -> usize {
    match fs::read_dir(dir) {
        Ok(rd) => rd.flatten().filter(|e| e.path().is_file()).count(),
        Err(_) => 0,
    }
}

#[tauri::command]
fn run_rife_pipeline(
    app: AppHandle,
    input_frames: String,
    output_frames: String,
    model_dir: String,
    threads: String,
) -> Result<(), String> {
    let root = app_root(&app)?;
    ensure_dirs(&root)?;

    let (_ffmpeg_path, rife_path, _rife_models) = find_installed_tool_paths(&root);
    let rife_bin = rife_path.ok_or("rife not installed (install rife first)")?;

    let model_path = resolve_rife_model_path(model_dir.trim());
    if !model_path.exists() {
        return Err(format!("Model path does not exist: {}", model_path.to_string_lossy()));
    }

    let in_dir = PathBuf::from(input_frames.trim());
    if !in_dir.exists() {
        return Err(format!("Input frames dir does not exist: {}", in_dir.to_string_lossy()));
    }

    let out_dir = PathBuf::from(output_frames.trim());
    std::fs::create_dir_all(&out_dir)
        .map_err(|e| format!("Failed to create output frames dir: {e}"))?;

    let app_for_task = app.clone();
    tauri::async_runtime::spawn_blocking(move || {
        let _ = app_for_task.emit("pipeline_log", "Starting RIFE (GPU/Vulkan)…");
        let _ = app_for_task.emit("pipeline_log", format!("RIFE: {}", rife_bin.to_string_lossy()));
        let _ = app_for_task.emit("pipeline_log", format!("Model: {}", model_path.to_string_lossy()));
        let _ = app_for_task.emit("pipeline_log", format!("Threads (-j): {}", threads));

        let (cwd, model_arg) = compute_rife_cwd_and_model_arg(&rife_bin, &model_path);
        if let Some(ref d) = cwd {
            let _ = app_for_task.emit(
                "pipeline_log",
                format!("Working dir: {}", d.to_string_lossy()),
            );
        }
        let _ = app_for_task.emit(
            "pipeline_log",
            format!("Model arg (-m): {}", model_arg.to_string_lossy()),
        );

        let mut cmd = Command::new(&rife_bin);
        if let Some(d) = cwd {
            cmd.current_dir(d);
        }
        cmd.arg("-v")
            .arg("-i").arg(&in_dir)
            .arg("-o").arg(&out_dir)
            .arg("-m").arg(model_arg)
            .arg("-f").arg("%08d.png")
            .arg("-j").arg(&threads)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());

        let mut child = match cmd.spawn() {
            Ok(c) => c,
            Err(e) => {
                let _ = app_for_task.emit("pipeline_log", format!("RIFE failed to start: {e}"));
                let _ = app_for_task.emit("pipeline_done", "failed");
                return;
            }
        };

        let stdout = child.stdout.take();
        let stderr = child.stderr.take();

        let app_stdout = app_for_task.clone();
        let t1 = std::thread::spawn(move || {
            if let Some(out) = stdout {
                let reader = BufReader::new(out);
                for line in reader.lines().flatten() {
                    if !line.trim().is_empty() {
                        let _ = app_stdout.emit("pipeline_log", line);
                    }
                }
            }
        });

        let app_stderr = app_for_task.clone();
        let t2 = std::thread::spawn(move || {
            if let Some(err) = stderr {
                let reader = BufReader::new(err);
                for line in reader.lines().flatten() {
                    if !line.trim().is_empty() {
                        let _ = app_stderr.emit("pipeline_log", line);
                    }
                }
            }
        });

        let status = match child.wait() {
            Ok(s) => s,
            Err(e) => {
                let _ = app_for_task.emit("pipeline_log", format!("Failed waiting for RIFE: {e}"));
                let _ = app_for_task.emit("pipeline_done", "failed");
                return;
            }
        };

        let _ = t1.join();
        let _ = t2.join();

        if status.success() {
            let _ = app_for_task.emit("pipeline_done", "ok");
        } else {
            let _ = app_for_task.emit("pipeline_log", format!("RIFE exited with {}", status));
            let _ = app_for_task.emit("pipeline_done", "failed");
        }
    });

    Ok(())
}



#[tauri::command]
fn extract_frames(app: AppHandle, video_path: String) -> Result<ExtractFramesResult, String> {
    // IMPORTANT: non-blocking. We return immediately and run ffmpeg in a background thread.
    let root = app_root(&app)?;
    ensure_dirs(&root)?;

    let (ffmpeg_path, _rife_path, _rife_models) = find_installed_tool_paths(&root);
    let ffmpeg = preferred_ffmpeg_path()
        .or(ffmpeg_path)
        .ok_or("ffmpeg not installed (install ffmpeg first)")?;

    let input = PathBuf::from(video_path.trim());
    if !input.exists() {
        return Err("Input video does not exist".into());
    }

    let job_id = make_job_id();
    let frames_dir = root.join("temp").join("frames_in").join(&job_id);
    fs::create_dir_all(&frames_dir).map_err(|e| e.to_string())?;

    let ext = "jpg";
    let pattern = frames_dir.join(format!("%08d.{ext}"));

    // Tiny, safe UI notes (no streaming logs).
    emit_log_limited(&app, &format!("Frames folder: {}", frames_dir.to_string_lossy()));
    emit_log_limited(&app, &format!("Extract format: {}", ext));

    // Spawn background worker so the UI stays responsive.
    let app_clone = app.clone();
    let ffmpeg_clone = ffmpeg.clone();
    let input_clone = input.clone();
    let frames_dir_clone = frames_dir.clone();
    let pattern_clone = pattern.clone();

    std::thread::spawn(move || {
        let done = match extract_frames_worker(
            &app_clone,
            &ffmpeg_clone,
            &input_clone,
            &frames_dir_clone,
            &pattern_clone
        ) {
            Ok(msg) => PipelineDoneEvent {
                ok: true,
                message: msg,
                frames_dir: frames_dir_clone.to_string_lossy().to_string(),
                frame_pattern: pattern_clone.to_string_lossy().to_string(),
            },
            Err(err) => PipelineDoneEvent {
                ok: false,
                message: err,
                frames_dir: frames_dir_clone.to_string_lossy().to_string(),
                frame_pattern: pattern_clone.to_string_lossy().to_string(),
            },
        };

        let _ = app_clone.emit("pipeline_done", done);
    });

    // Return immediately.
    Ok(ExtractFramesResult {
        ok: true, // accepted / started
        frames_dir: frames_dir.to_string_lossy().to_string(),
        frame_pattern: pattern.to_string_lossy().to_string(),
        output: "Started frame extraction in background".to_string(),
    })
}






#[derive(Clone, serde::Serialize)]
struct PipelineDoneEvent {
    ok: bool,
    message: String,
    frames_dir: String,
    frame_pattern: String,
}

fn extract_frames_worker(
    app: &tauri::AppHandle,
    ffmpeg: &PathBuf,
    input: &PathBuf,
    frames_dir: &PathBuf,
    pattern: &PathBuf,
) -> Result<String, String> {
    let (duration_secs, fps) = probe_duration_and_fps(ffmpeg, input).unwrap_or((0.0, 0.0));
    let total_frames_est = if duration_secs > 0.0 && fps > 0.0 {
        (duration_secs * fps).round() as i64
    } else {
        0
    };

    if total_frames_est > 0 {
        emit_log_limited(app, &format!("Estimated frames: {}", total_frames_est));
    }

    let mut cmd = Command::new(ffmpeg);
    cmd.arg("-hide_banner")
        .arg("-y")
        .arg("-nostdin")
        .arg("-loglevel").arg("error")
        .arg("-stats_period").arg("0.5")
        .arg("-i").arg(input)
        .arg("-fps_mode").arg("passthrough")
        .arg("-progress").arg("pipe:1");

// Hardware decode when available (platform default).
let jpg_quality = 2;

// hwaccel name (ffmpeg): macOS=videotoolbox, Windows=d3d11va (fallback).
let hwaccel = if cfg!(target_os = "macos") {
    Some("videotoolbox")
} else if cfg!(target_os = "windows") {
    Some("d3d11va")
} else {
    None
};

if let Some(hw) = hwaccel {
    cmd.arg("-hwaccel").arg(hw);
}

// Fast path for interpolation: decode frames as JPG (much faster IO than PNG/WebP lossless).
cmd.arg("-threads").arg("0")
    .arg("-c:v").arg("mjpeg")
    .arg("-q:v").arg(jpg_quality.to_string());
    cmd.arg(pattern.as_os_str())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    let mut child = cmd.spawn().map_err(|e| format!("Failed to start ffmpeg: {e}"))?;

    // Drain stderr (collect a short snippet for errors).
    let stderr_snippet = std::sync::Arc::new(std::sync::Mutex::new(String::new()));
    if let Some(err) = child.stderr.take() {
        let snip = stderr_snippet.clone();
        std::thread::spawn(move || {
            let reader = BufReader::new(err);
            for line in reader.lines().flatten() {
                let mut s = snip.lock().unwrap();
                if s.len() < 1200 {
                    if !s.is_empty() { s.push('\n'); }
                    s.push_str(&line);
                } else {
                    break;
                }
            }
        });
    }

    // Drain progress output so ffmpeg can't block on full buffers.
    if let Some(out) = child.stdout.take() {
        let reader = BufReader::new(out);
        let mut last_emit = std::time::Instant::now();
        let mut frame = 0i64;

        for line in reader.lines().flatten() {
            if let Some((k, v)) = parse_ffmpeg_progress_line(&line) {
                if k == "frame" {
                    frame = v.parse::<i64>().unwrap_or(frame);
                }
                // throttle UI events
                if last_emit.elapsed().as_millis() >= 250 {
                    if total_frames_est > 0 && frame > 0 {
                        let pct = ((frame as f64 / total_frames_est as f64) * 100.0).min(100.0);
                        let _ = app.emit("pipeline_progress", pct);
                    }
                    last_emit = std::time::Instant::now();
                }
            }
        }
    }

    let status = child.wait().map_err(|e| format!("Failed waiting for ffmpeg: {e}"))?;
    let frame_count = count_files_in_dir(frames_dir);

    if frame_count > 0 {
        let _ = app.emit("pipeline_progress", 100.0f64);
    }

    if !status.success() {
        let snip = stderr_snippet.lock().unwrap().trim().to_string();
        if !snip.is_empty() {
            emit_log_limited(app, &format!("ffmpeg error: {}", snip.lines().next().unwrap_or("")));
            return Err(format!("ffmpeg exited with {status}\n{snip}"));
        }
        emit_log_limited(app, &format!("ffmpeg exited with {status}"));
        return Err(format!("ffmpeg exited with {status}"));
    }

    if frame_count <= 0 {
        return Err("No frames were extracted".to_string());
    }

    Ok(format!("Frames extracted: {frame_count}"))
}



fn run_and_capture(mut cmd: Command) -> ToolValidation {
    match cmd.output() {
        Ok(out) => {
            let mut text = String::new();
            if !out.stdout.is_empty() {
                text.push_str(&String::from_utf8_lossy(&out.stdout));
            }
            if !out.stderr.is_empty() {
                if !text.is_empty() {
                    text.push('\n');
                }
                text.push_str(&String::from_utf8_lossy(&out.stderr));
            }
            ToolValidation {
                ok: out.status.success(),
                path: None,
                output: text.trim().to_string(),
            }
        }
        Err(e) => ToolValidation {
            ok: false,
            path: None,
            output: format!("Failed to run: {e}"),
        },
    }
}


#[tauri::command]
fn smooth_video(
    app: AppHandle,
    video_path: String,
    output_path: String,
    max_threads: Option<i32>,
) -> Result<ExtractFramesResult, String> {
    // Non-blocking: returns immediately; work is done on a background thread.
    let root = app_root(&app)?;
    ensure_dirs(&root)?;

    let (ffmpeg_path, rife_path, rife_models) = find_installed_tool_paths(&root);
    let ffmpeg = preferred_ffmpeg_path()
        .or(ffmpeg_path)
        .ok_or("ffmpeg not installed (install ffmpeg first)")?;
    let rife_bin = rife_path.ok_or("rife not installed (install rife first)")?;
    let model_dir = rife_models.ok_or("RIFE models folder not found (install rife first)")?;

    let input = PathBuf::from(video_path.trim());
    if !input.exists() {
        return Err("Input video does not exist".into());
    }
    let output = PathBuf::from(output_path.trim());
    if output_path.trim().is_empty() {
        return Err("Output path is required".into());
    }

    // Create a job folder
    let job_id = format!("job-{}", chrono::Utc::now().timestamp_millis());
    let frames_in_dir = root.join("temp").join("frames_in").join(&job_id);
    let frames_out_dir = root.join("temp").join("frames_out").join(&job_id);
    std::fs::create_dir_all(&frames_in_dir).map_err(|e| format!("Failed to create frames_in dir: {e}"))?;
    std::fs::create_dir_all(&frames_out_dir).map_err(|e| format!("Failed to create frames_out dir: {e}"))?;

    let pattern = frames_in_dir.join("%08d.png");
    let frames_dir_str = frames_in_dir.to_string_lossy().to_string();
    let frame_pattern_str = pattern.to_string_lossy().to_string();

    // Make thread string for RIFE (-j x:x:x)
    let threads = match max_threads.unwrap_or(0) {
        t if t <= 0 => "2:2:2".to_string(),
        t => {
            // Clamp to sane range
            let t = t.clamp(1, 12);
            format!("{t}:{t}:{t}")
        }
    };

    // Emit initial stage immediately
    emit_stage(&app, "Extracting frames… (step 1/3)");
    let _ = app.emit("pipeline_progress", 0.0_f64);
    let _ = app.emit("pipeline_log", format!("Smooth Video job: {}", job_id));

    let app_for_task = app.clone();
    let ffmpeg_for_task = ffmpeg.clone();
    let rife_for_task = rife_bin.clone();
    let model_dir_for_task = model_dir.clone();
    let input_for_task = input.clone();
    let output_for_task = output.clone();
    let frames_in_for_task = frames_in_dir.clone();
    let frames_out_for_task = frames_out_dir.clone();
    let threads_for_task = threads.clone();
    let frames_dir_for_task = frames_dir_str.clone();
    let frame_pattern_for_task = frame_pattern_str.clone();

    tauri::async_runtime::spawn_blocking(move || {
        // STEP 1: Extract frames
        let _ = app_for_task.emit("pipeline_log", format!("FFmpeg: {}", ffmpeg_for_task.to_string_lossy()));
        let _ = app_for_task.emit("pipeline_log", format!("Input: {}", input_for_task.to_string_lossy()));
        let _ = app_for_task.emit("pipeline_log", format!("Frames in: {}", frames_in_for_task.to_string_lossy()));

        let mut cmd = Command::new(&ffmpeg_for_task);
        cmd.arg("-hide_banner").arg("-y")
            .arg("-i").arg(&input_for_task)
            // png is a good middle-ground for now
            .arg("-vsync").arg("0")
            .arg(frames_in_for_task.join("%08d.png"))
            .stdout(Stdio::null())
            .stderr(Stdio::piped());

        let mut child = match cmd.spawn() {
            Ok(c) => c,
            Err(e) => {
                let _ = app_for_task.emit("pipeline_done", PipelineDoneEvent {
                    ok: false,
                    message: format!("FFmpeg failed to start: {e}"),
                    frames_dir: frames_dir_for_task.clone(),
                    frame_pattern: frame_pattern_for_task.clone(),
                });
                return;
            }
        };

        // stream ffmpeg stderr lightly
        if let Some(stderr) = child.stderr.take() {
            let reader = std::io::BufReader::new(stderr);
            for line in reader.lines().flatten() {
                let line = line.trim().to_string();
                if !line.is_empty() {
                    let _ = app_for_task.emit("pipeline_log", line);
                }
            }
        }
        let ok = child.wait().map(|s| s.success()).unwrap_or(false);
        if !ok {
            let _ = app_for_task.emit("pipeline_done", PipelineDoneEvent {
                ok: false,
                message: "Frame extraction failed".into(),
                frames_dir: frames_dir_for_task.clone(),
                frame_pattern: frame_pattern_for_task.clone(),
            });
            return;
        }

        // Count frames
        let in_count = count_files_in_dir(&frames_in_for_task).max(1) as f64;

        // STEP 2: RIFE
        emit_stage(&app_for_task, "Interpolating (RIFE)… (step 2/3)");
        let _ = app_for_task.emit("pipeline_log", format!("RIFE: {}", rife_for_task.to_string_lossy()));
        let _ = app_for_task.emit("pipeline_log", format!("Model dir: {}", model_dir_for_task.to_string_lossy()));
        let _ = app_for_task.emit("pipeline_log", format!("Threads (-j): {}", threads_for_task));

        let (cwd, model_arg) = compute_rife_cwd_and_model_arg(&rife_for_task, &model_dir_for_task);
        let mut rife_cmd = Command::new(&rife_for_task);
        if let Some(d) = cwd {
            rife_cmd.current_dir(d);
        }
        rife_cmd.arg("-v")
            .arg("-i").arg(&frames_in_for_task)
            .arg("-o").arg(&frames_out_for_task)
            .arg("-m").arg(model_arg)
            .arg("-f").arg("%08d.png")
            .arg("-j").arg(&threads_for_task)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());

        let mut rife_child = match rife_cmd.spawn() {
            Ok(c) => c,
            Err(e) => {
                let _ = app_for_task.emit("pipeline_done", PipelineDoneEvent {
                    ok: false,
                    message: format!("RIFE failed to start: {e}"),
                    frames_dir: frames_dir_for_task.clone(),
                    frame_pattern: frame_pattern_for_task.clone(),
                });
                return;
            }
        };

        // stream logs from RIFE stderr on a background thread (prevents pipe buffer deadlocks)
        use std::sync::{Arc, Mutex};
        let stderr_tail: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        let stderr_tail_for_thread = stderr_tail.clone();

        let stderr_handle = rife_child.stderr.take().map(|st| {
            let app = app_for_task.clone();
            std::thread::spawn(move || {
                let reader = std::io::BufReader::new(st);
                for line in reader.lines().flatten() {
                    let line = line.trim().to_string();
                    if line.is_empty() { continue; }
                    // keep a small tail for error reporting
                    {
                        let mut t = stderr_tail_for_thread.lock().unwrap();
                        t.push(line.clone());
                        let len = t.len();
                        if len > 64 {
                            t.drain(0..(len - 64));
                        }
                    }
                    let _ = app.emit("pipeline_log", line);
                }
            })
        });

        // update progress based on output frame count while RIFE runs
        while rife_child.try_wait().ok().flatten().is_none() {
            let out_count = count_files_in_dir(&frames_out_for_task) as f64;
            // For 2x interpolation, output is roughly ~2x input frames. Clamp to the middle-third segment.
            let pct = 33.0 + ((out_count / (in_count * 2.0)) * 33.0).max(0.0).min(33.0);
            let _ = app_for_task.emit("pipeline_progress", pct);
            std::thread::sleep(std::time::Duration::from_millis(300));
        }

        // ensure stderr thread finishes draining
        if let Some(h) = stderr_handle {
            let _ = h.join();
        }

        let ok = rife_child.wait().map(|s| s.success()).unwrap_or(false);
        if !ok {
            let tail = {
                let t = stderr_tail.lock().unwrap();
                t.iter().rev().take(8).cloned().collect::<Vec<_>>().into_iter().rev().collect::<Vec<_>>().join("\n")
            };
            let msg = if tail.trim().is_empty() { "RIFE failed".into() } else { tail };
            let _ = app_for_task.emit("pipeline_done", PipelineDoneEvent {
                ok: false,
                message: msg,
                frames_dir: frames_dir_for_task.clone(),
                frame_pattern: frame_pattern_for_task.clone(),
            });
            return;
        }

        // STEP 3: Encode video
        emit_stage(&app_for_task, "Encoding video… (step 3/3)");
        let out_pattern = frames_out_for_task.join("%08d.png");
        let mut enc = Command::new(&ffmpeg_for_task);
        enc.arg("-hide_banner").arg("-y")
            .arg("-framerate").arg("30")
            .arg("-i").arg(out_pattern)
            .arg("-c:v").arg("libx264")
            .arg("-pix_fmt").arg("yuv420p")
            .arg(&output_for_task)
            .stdout(Stdio::null())
            .stderr(Stdio::piped());

        let mut enc_child = match enc.spawn() {
            Ok(c) => c,
            Err(e) => {
                let _ = app_for_task.emit("pipeline_done", PipelineDoneEvent {
                    ok: false,
                    message: format!("Encode failed to start: {e}"),
                    frames_dir: frames_dir_for_task.clone(),
                    frame_pattern: frame_pattern_for_task.clone(),
                });
                return;
            }
        };

        if let Some(stderr) = enc_child.stderr.take() {
            let reader = std::io::BufReader::new(stderr);
            for line in reader.lines().flatten() {
                let line = line.trim().to_string();
                if !line.is_empty() {
                    let _ = app_for_task.emit("pipeline_log", line);
                }
            }
        }
        let ok = enc_child.wait().map(|s| s.success()).unwrap_or(false);
        if !ok {
            let _ = app_for_task.emit("pipeline_done", PipelineDoneEvent {
                ok: false,
                message: "Encoding failed".into(),
                frames_dir: frames_dir_for_task.clone(),
                frame_pattern: frame_pattern_for_task.clone(),
            });
            return;
        }

        let _ = app_for_task.emit("pipeline_progress", 100.0_f64);
        let _ = app_for_task.emit("pipeline_done", PipelineDoneEvent {
            ok: true,
            message: format!("Done: {}", output_for_task.to_string_lossy()),
            frames_dir: frames_dir_for_task.clone(),
            frame_pattern: frame_pattern_for_task.clone(),
        });
    });

    Ok(ExtractFramesResult {
        ok: true,
        frames_dir: frames_dir_str,
        frame_pattern: frame_pattern_str,
        output: output.to_string_lossy().to_string(),
    })
}

#[tauri::command]
fn reencode_only(
    app: AppHandle,
    video_path: String,
    output_path: String,
    frames_dir: Option<String>,
    max_threads: Option<i32>,
) -> Result<ExtractFramesResult, String> {
    // Non-blocking: returns immediately; work is done on a background thread.
    let root = app_root(&app)?;
    ensure_dirs(&root)?;

    let (ffmpeg_path, _rife_path, _rife_models) = find_installed_tool_paths(&root);
    let ffmpeg = preferred_ffmpeg_path()
        .or(ffmpeg_path)
        .ok_or("ffmpeg not installed (install ffmpeg first)")?;

    let input = PathBuf::from(video_path.trim());
    if !input.exists() {
        return Err("Input video does not exist".into());
    }

    let output = PathBuf::from(output_path.trim());
    if output_path.trim().is_empty() {
        return Err("Output path is required".into());
    }

    let frames_dir_str = frames_dir.unwrap_or_default().trim().to_string();
    if frames_dir_str.is_empty() {
        return Err("Frames folder is required for re-encode only".into());
    }
    let frames_dir_path = PathBuf::from(&frames_dir_str);
    if !frames_dir_path.exists() {
        return Err("Frames folder does not exist".into());
    }

    let pattern = frames_dir_path.join("%08d.png");
    let frame_pattern_str = pattern.to_string_lossy().to_string();

    // Estimate total frames for progress
    let total_frames_est = count_files_in_dir(&frames_dir_path).max(0) as i64;

    let (_dur, fps_in) = probe_duration_and_fps(&ffmpeg, &input).unwrap_or((0.0, 30.0));
    let fps_out = (fps_in * 2.0).max(1.0);
    let fps_out_str = format!("{:.6}", fps_out);

    let output_ext = output
        .extension()
        .and_then(|s| s.to_str())
        .unwrap_or("")
        .to_lowercase();

    let app_for_task = app.clone();
    let input_for_task = input.clone();
    let output_for_task = output.clone();
    let frames_dir_for_task = frames_dir_str.clone();
    let frame_pattern_for_task = frame_pattern_str.clone();
    let ffmpeg_for_task = ffmpeg.clone();
    let max_threads_for_task = max_threads.unwrap_or(0);

    std::thread::spawn(move || {
        let _ = app_for_task.emit("pipeline_progress", 0.0_f64);
        emit_log_limited(&app_for_task, "Re-encode only: starting ffmpeg…");

        let mut cmd = Command::new(&ffmpeg_for_task);
        cmd.arg("-hide_banner").arg("-y");

        if max_threads_for_task > 0 {
            cmd.arg("-threads").arg(max_threads_for_task.to_string());
        }

        cmd.arg("-progress").arg("pipe:1")
            .arg("-nostats")
            .arg("-framerate").arg(&fps_out_str)
            .arg("-i").arg(&frame_pattern_for_task)
            .arg("-i").arg(&input_for_task)
            .arg("-map").arg("0:v:0")
            .arg("-map").arg("1:a:0?")
            .arg("-c:v").arg("libx264")
            .arg("-preset").arg("ultrafast")
            .arg("-crf").arg("18");

        // Audio: Opus-in-MP4 can be finicky; AAC is safest for mp4/mov.
        if output_ext == "mp4" || output_ext == "mov" || output_ext == "m4v" {
            cmd.arg("-c:a").arg("aac").arg("-b:a").arg("192k");
        } else {
            cmd.arg("-c:a").arg("copy");
        }

        cmd.arg("-shortest").arg(&output_for_task)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());

        let mut child = match cmd.spawn() {
            Ok(c) => c,
            Err(e) => {
                let _ = app_for_task.emit("pipeline_done", PipelineDoneEvent {
                    ok: false,
                    message: format!("ffmpeg failed to start: {e}"),
                    frames_dir: frames_dir_for_task.clone(),
                    frame_pattern: frame_pattern_for_task.clone(),
                });
                return;
            }
        };

        // stderr -> log
        if let Some(stderr) = child.stderr.take() {
            let app_log = app_for_task.clone();
            std::thread::spawn(move || {
                let reader = BufReader::new(stderr);
                for line in reader.lines().flatten() {
                    emit_log_limited(&app_log, &line);
                }
            });
        }

        // stdout (-progress) -> progress percent
        let mut last_emit = std::time::Instant::now();
        if let Some(stdout) = child.stdout.take() {
            let reader = BufReader::new(stdout);
            let mut frame: i64 = 0;
            for line in reader.lines().flatten() {
                let line = line.trim().to_string();
                if line.is_empty() { continue; }
                if let Some((k, v)) = line.split_once('=') {
                    if k == "frame" {
                        frame = v.parse::<i64>().unwrap_or(frame);
                    }
                    if last_emit.elapsed().as_millis() >= 250 {
                        if total_frames_est > 0 && frame > 0 {
                            let pct = ((frame as f64 / total_frames_est as f64) * 100.0).min(99.9);
                            let _ = app_for_task.emit("pipeline_progress", pct);
                        }
                        last_emit = std::time::Instant::now();
                    }
                }
            }
        }

        let ok = child.wait().map(|s| s.success()).unwrap_or(false);
        if ok {
            let _ = app_for_task.emit("pipeline_progress", 100.0_f64);
            let _ = app_for_task.emit("pipeline_done", PipelineDoneEvent {
                ok: true,
                message: format!("Done: {}", output_for_task.to_string_lossy()),
                frames_dir: frames_dir_for_task.clone(),
                frame_pattern: frame_pattern_for_task.clone(),
            });
        } else {
            let _ = app_for_task.emit("pipeline_done", PipelineDoneEvent {
                ok: false,
                message: "Re-encode failed".into(),
                frames_dir: frames_dir_for_task.clone(),
                frame_pattern: frame_pattern_for_task.clone(),
            });
        }
    });

    Ok(ExtractFramesResult {
        ok: true,
        frames_dir: frames_dir_str,
        frame_pattern: frame_pattern_str,
        output: output.to_string_lossy().to_string(),
    })
}


#[tauri::command]
fn validate_tools(app: AppHandle) -> Result<ValidateToolsResult, String> {
    let root = app_root(&app)?;
    ensure_dirs(&root)?;

    let (ffmpeg_path, rife_path, rife_models) = find_installed_tool_paths(&root);

    // ffmpeg
    let ffmpeg = if let Some(p) = ffmpeg_path {
        let mut cmd = Command::new(&p);
        cmd.arg("-version");
        let mut tv = run_and_capture(cmd);
        tv.path = Some(p.to_string_lossy().to_string());
        tv
    } else {
        ToolValidation {
            ok: false,
            path: None,
            output: "ffmpeg not installed (no binary found in app-managed bin/ffmpeg)".into(),
        }
    };

    // rife
    let rife = if let Some(p) = rife_path {
        // This RIFE build expects model folders like 'rife-v2.3' next to the binary and uses '-h' for help.
        let mut cmd = Command::new(&p);
        cmd.arg("-h");
        let mut tv = run_and_capture(cmd);

        let models_found = rife_models.is_some();
        let models_note = match &rife_models {
            Some(m) => format!("Models: {}", m.to_string_lossy()),
            None => "Models: NOT FOUND (expected a folder like 'rife-v2.3', 'rife-v4', etc. next to the RIFE binary)".into(),
        };

        if !tv.output.is_empty() {
            tv.output = format!("{models_note}

{}", tv.output);
        } else {
            tv.output = models_note;
        }

        tv.path = Some(p.to_string_lossy().to_string());

        // Some RIFE builds return non-zero for '-h'; treat as OK if we got Usage text and models exist.
        let usage_like = tv.output.contains("Usage:");
        tv.ok = models_found && (tv.ok || usage_like);

        tv
    } else {
        ToolValidation {
            ok: false,
            path: None,
            output: "RIFE not installed (no 'rife*' binary found in app-managed bin/rife)".into(),
        }
    };


    Ok(ValidateToolsResult { ffmpeg, rife })
}

fn main() {
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .invoke_handler(tauri::generate_handler![
            check_environment,
            get_app_paths,
            tool_status,
            install_tool,
            validate_tools,
            extract_frames,
            smooth_video,
            reencode_only,
            get_max_threads_string,
            get_default_rife_model_dir,
            run_rife_pipeline
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
