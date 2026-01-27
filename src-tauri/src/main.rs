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

fn ffmpeg_has_libwebp(ffmpeg: &Path) -> bool {
    let out = std::process::Command::new(&ffmpeg)
        .arg("-hide_banner")
        .arg("-encoders")
        .output();
    if let Ok(o) = out {
        let s = String::from_utf8_lossy(&o.stdout);
        return s.to_lowercase().contains("libwebp");
    }
    false
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

        let mut cmd = Command::new(&rife_bin);
        cmd.arg("-i").arg(&in_dir)
            .arg("-o").arg(&out_dir)
            .arg("-m").arg(&model_path)
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

    let use_webp = ffmpeg_has_libwebp(&ffmpeg);
    let ext = if use_webp { "webp" } else { "png" };
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
            &pattern_clone,
            use_webp,
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
    use_webp: bool,
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

    if use_webp {
        cmd.arg("-c:v").arg("libwebp")
            .arg("-lossless").arg("1")
            .arg("-q:v").arg("100");
    } else {
        cmd.arg("-c:v").arg("png");
    }

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
            get_max_threads_string,
            get_default_rife_model_dir,
            run_rife_pipeline
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
