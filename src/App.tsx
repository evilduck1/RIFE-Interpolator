import React, { useEffect, useMemo, useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { open, save } from "@tauri-apps/plugin-dialog";

type Status = "installed" | "missing" | "unknown";

type ToolValidation = {
  ok: boolean;
  path?: string | null;
  output: string;
};

type ValidateToolsResult = {
  ffmpeg: ToolValidation;
  rife: ToolValidation;
};

type ExtractFramesResult = {
  ok: boolean;
  frames_dir: string;
  frame_pattern: string;
  output: string;
};

function isTauriRuntime(): boolean {
  return typeof window !== "undefined" && Boolean((window as any).__TAURI__);
}


const darkTheme: React.CSSProperties = {
  background: "#1e1e1e",
  color: "#e6e6e6",
  minHeight: "100vh",
};

const inputStyle: React.CSSProperties = {
  background: "#2a2a2a",
  color: "#e6e6e6",
  border: "1px solid #444",
};

const buttonStyle: React.CSSProperties = {
  background: "#333",
  color: "#e6e6e6",
  border: "1px solid #555",
};

const boxStyle: React.CSSProperties = {
  border: "1px solid rgba(255,255,255,0.12)",
  borderRadius: 10,
  padding: 12,
  background: "rgba(255,255,255,0.03)",
};

export default function App() {
  const stageStartRef = useRef<number>(0);
  const [envMsg, setEnvMsg] = useState("");
  const [paths, setPaths] = useState<string[]>([]);
  const [ffmpegPath, setFfmpegPath] = useState("");
  const [rifePath, setRifePath] = useState("");
  const [ffmpegStatus, setFfmpegStatus] = useState<Status>("unknown");
  const [rifeStatus, setRifeStatus] = useState<Status>("unknown");

  const [validating, setValidating] = useState(false);
  const [validation, setValidation] = useState<ValidateToolsResult | null>(null);
  const [error, setError] = useState("");

  const [inputVideo, setInputVideo] = useState("");
  const [outputVideo, setOutputVideo] = useState("");
  const [framesDir, setFramesDir] = useState("");
  const [reencodeOnly, setReencodeOnly] = useState(false);
  const [framesOutDir, setFramesOutDir] = useState<string>(() => {
    try { return localStorage.getItem("lastFramesOutDir") || ""; } catch { return ""; }
  });
  const [maxThreads, setMaxThreads] = useState<number>(0);

  const [extracting, setExtracting] = useState(false);

  const [pipelineStatus, setPipelineStatus] = useState<string>("");
  const [pipelineError, setPipelineError] = useState<string>("");
  const [pipelineLog, setPipelineLog] = useState<string>("");
  const [pipelineEta, setPipelineEta] = useState<string>("");

  const canRun = useMemo(
    () =>
      inputVideo.trim().length > 0 &&
      outputVideo.trim().length > 0 &&
      (!reencodeOnly || framesOutDir.trim().length > 0),
    [inputVideo, outputVideo, reencodeOnly, framesOutDir]
  );

  async function refresh() {
    try {
      const p = await invoke<string[]>("get_app_paths");
      setPaths(p);

      const f = await invoke<string>("tool_status", { tool: "ffmpeg" });
      const r = await invoke<string>("tool_status", { tool: "rife" });
      setFfmpegStatus((f as Status) ?? "unknown");
      setRifeStatus((r as Status) ?? "unknown");
    } catch (e: any) {
      setError(String(e));
    }
  }

  async function checkEnv() {
    try {
      const result = await invoke<string>("check_environment");
      setEnvMsg(result);
    } catch (e: any) {
      setEnvMsg(String(e));
    }
  }

  async function validate() {
    setValidating(true);
    setError("");
    setPipelineError("");
    try {
      const res = await invoke<ValidateToolsResult>("validate_tools");
      setValidation(res);
    } catch (e: any) {
      setError(String(e));
    } finally {
      setValidating(false);
    }
  }

  async function install(tool: "ffmpeg" | "rife") {
    const sourcePath = tool === "ffmpeg" ? ffmpegPath : rifePath;
    if (!sourcePath.trim()) return;

    setError("");
    try {
      await invoke<string>("install_tool", {
        source_path: sourcePath,
        tool,
        version: "v1",
      });
      await refresh();
    } catch (e: any) {
      setError(String(e));
    }
  }

  async function pickInputVideo() {
    const file = await open({
      multiple: false,
      filters: [{ name: "Video", extensions: ["mp4", "mov", "mkv", "avi", "webm"] }],
    });
    if (typeof file === "string") setInputVideo(file);
  }

  async function pickOutputVideo() {
    const file = await save({
      filters: [{ name: "Video", extensions: ["mp4", "mov", "mkv"] }],
    });
    if (typeof file === "string") setOutputVideo(file);
  }

  async function pickFramesOutDir() {
    const dir = await open({
      multiple: false,
      directory: true,
    });
    if (typeof dir === "string") {
      setFramesOutDir(dir);
      try { localStorage.setItem("lastFramesOutDir", dir); } catch {}
    }
  }

  async function smoothVideo() {
    if (!canRun) return;

    setExtracting(true);
    setPipelineStatus("Starting…");
    setPipelineError("");
    setError("");
    setFramesDir("");
    setPipelineLog("");
    stageStartRef.current = 0;
    setPipelineEta("");

    try {
      if (reencodeOnly) {
        const dirToUse = framesOutDir || framesDir;
        if (!dirToUse) {
          setPipelineStatus("Failed.");
          const msg = "Pick a frames_out folder first.";
          setPipelineError(msg);
          setError(msg);
          setExtracting(false);
          return;
        }
        try { localStorage.setItem("lastFramesOutDir", dirToUse); } catch {}
        await invoke("reencode_only", {
          videoPath: inputVideo,
          outputPath: outputVideo,
          framesDir: dirToUse,
          maxThreads,
        });

        // re-encode runs synchronously; if the backend doesn't emit pipeline_done,
        // finalize UI state here.
        setFramesDir(dirToUse);
        setExtracting(false);
        setPipelineStatus("Done.");
        setPipelineError("");
        return;
      }

      const res = await invoke<ExtractFramesResult>("smooth_video", {
        videoPath: inputVideo,
        outputPath: outputVideo,
        maxThreads,
      });
      // backend returns frames_in/out folder (useful for debugging / reuse)
      setFramesDir(res.frames_dir);
      if (!framesOutDir) setFramesOutDir(res.frames_dir);
      try { localStorage.setItem("lastFramesOutDir", res.frames_dir); } catch {}
      // stage/progress/done are driven by events
    } catch (e: any) {
      const msg = String(e);
      setPipelineStatus("Failed.");
      setPipelineError(msg);
      setError(msg);
      setExtracting(false);
    }
  }
  useEffect(() => {
    refresh();
    checkEnv();
  }, []);

  useEffect(() => {
    if (!isTauriRuntime()) return;

    let unlistenProgress: null | (() => void) = null;
    let unlistenDone: null | (() => void) = null;
    let unlistenLog: null | (() => void) = null;

    (async () => {
      try {
        unlistenProgress = await listen<number>("pipeline_progress", (e) => {
          const pct = e.payload ?? 0;
          setPipelineStatus(`Running… ${Math.round(pct)}%`);
        });

        unlistenLog = await listen<string>("pipeline_log", (e) => {
          const msg = String(e.payload ?? "");
          setPipelineLog((prev) => (prev ? prev + "\n" + msg : msg));
        });

        unlistenDone = await listen<any>("pipeline_done", (e) => {
          const p: any = e.payload as any;
          setExtracting(false);
          if (p?.ok) {
            setPipelineStatus(p.message || "Done.");
            setPipelineError("");
          } else {
            const msg = p?.message ? String(p.message) : "Failed";
            setPipelineStatus("Failed.");
            setPipelineError(msg);
            setError(msg);
          }
        });
      } catch (err) {
        console.error("Failed to set up Tauri listeners:", err);
      }
    })();

    return () => {
      try { unlistenProgress?.(); } catch {}
      try { unlistenLog?.(); } catch {}
      try { unlistenDone?.(); } catch {}
    };
  }, []);

  return (
    <main style={{ ...darkTheme, padding: 18 }}>
      <h1 style={{ margin: "0 0 8px 0" }}>RIFE Interpolator</h1>

      <div style={{ marginBottom: 12, opacity: 0.85 }}>UI loaded.</div>

      <div style={{ display: "grid", gap: 12 }}>
        <div style={boxStyle}>
          <div style={{ fontWeight: 700, marginBottom: 6 }}>Environment</div>
          <div style={{ fontFamily: "monospace", fontSize: 12, whiteSpace: "pre-wrap", opacity: 0.9 }}>
            {envMsg || "(no env message)"}
          </div>
          <div style={{ marginTop: 10, display: "flex", gap: 8 }}>
            <button style={buttonStyle} onClick={refresh}>Refresh</button>
            <button style={buttonStyle} onClick={checkEnv}>Check Env</button>
            <button style={buttonStyle} onClick={validate} disabled={validating}>
              {validating ? "Validating…" : "Validate Tools"}
            </button>
          </div>

          {validation && (
            <div style={{ marginTop: 10, fontFamily: "monospace", fontSize: 12, whiteSpace: "pre-wrap", opacity: 0.9 }}>
              {JSON.stringify(validation, null, 2)}
            </div>
          )}

          {error && (
            <div style={{ marginTop: 10, fontFamily: "monospace", fontSize: 12, whiteSpace: "pre-wrap", opacity: 0.9 }}>
              {error}
            </div>
          )}
        </div>

        <div style={boxStyle}>
          <div style={{ fontWeight: 700, marginBottom: 6 }}>Tools</div>
          <div style={{ display: "grid", gap: 8 }}>
            <div>
              <div style={{ fontSize: 12, opacity: 0.85 }}>FFmpeg status: <strong>{ffmpegStatus}</strong></div>
              <div style={{ display: "flex", gap: 8, marginTop: 6 }}>
                <input style={{ ...inputStyle, flex: 1 }} value={ffmpegPath} onChange={(e) => setFfmpegPath(e.currentTarget.value)} placeholder="Path to ffmpeg folder or exe…" />
                <button style={buttonStyle} onClick={() => install("ffmpeg")} disabled={!ffmpegPath.trim()}>Install</button>
              </div>
            </div>

            <div>
              <div style={{ fontSize: 12, opacity: 0.85 }}>RIFE status: <strong>{rifeStatus}</strong></div>
              <div style={{ display: "flex", gap: 8, marginTop: 6 }}>
                <input style={{ ...inputStyle, flex: 1 }} value={rifePath} onChange={(e) => setRifePath(e.currentTarget.value)} placeholder="Path to rife folder or exe…" />
                <button style={buttonStyle} onClick={() => install("rife")} disabled={!rifePath.trim()}>Install</button>
              </div>
            </div>

            {paths.length > 0 && (
              <div style={{ fontFamily: "monospace", fontSize: 12, whiteSpace: "pre-wrap", opacity: 0.9 }}>
                {paths.join("\n")}
              </div>
            )}
          </div>
        </div>

        <div style={boxStyle}>
          <div style={{ fontWeight: 700, marginBottom: 8 }}>Pipeline</div>

          <div style={{ marginBottom: 6, opacity: 0.9 }}>
            <strong>Input video</strong>
          </div>
          <div style={{ display: "flex", gap: 8, marginBottom: 10 }}>
            <input style={{ ...inputStyle, flex: 1 }} value={inputVideo} readOnly placeholder="Choose input video…" />
            <button style={buttonStyle} onClick={pickInputVideo}>Browse…</button>
          </div>

          <div style={{ marginBottom: 6, opacity: 0.9 }}>
            <strong>Output video</strong>
          </div>
          <div style={{ display: "flex", gap: 8, marginBottom: 10 }}>
            <input style={{ ...inputStyle, flex: 1 }} value={outputVideo} readOnly placeholder="Choose output video…" />
            <button style={buttonStyle} onClick={pickOutputVideo}>Browse…</button>
          </div>

          <div style={{ display: "grid", gap: 8, marginBottom: 10 }}>
            <div style={{ marginBottom: 10 }}>
              <label style={{ display: "flex", gap: 8, alignItems: "center", fontSize: 12, opacity: 0.9 }}>
                <input
                  type="checkbox"
                  checked={reencodeOnly}
                  onChange={(e) => {
                    const v = e.currentTarget.checked;
                    setReencodeOnly(v);
                    if (v) {
                      const dir = framesOutDir || framesDir;
                      if (dir) {
                        setFramesOutDir(dir);
                        try { localStorage.setItem("lastFramesOutDir", dir); } catch {}
                      }
                    }
                  }}
                />
                Re-encode only (use existing interpolated frames)
              </label>

              {reencodeOnly && (
                <div style={{ display: "flex", gap: 8, marginTop: 8 }}>
                  <input
                    style={{ ...inputStyle, flex: 1 }}
                    value={framesOutDir}
                    readOnly
                    placeholder="Choose frames_out folder…"
                  />
                  <button style={buttonStyle} onClick={pickFramesOutDir}>Browse…</button>
                </div>
              )}
            </div>
<div style={{ display: "flex", gap: 8, alignItems: "center" }}>
              <button style={buttonStyle} onClick={smoothVideo} disabled={extracting || !canRun || (reencodeOnly && !(framesOutDir || framesDir))}>
                {extracting ? (reencodeOnly ? "Re-encoding…" : "Smoothing…") : (reencodeOnly ? "Re-encode Only" : "Smooth Video")}
              </button>
            </div>
          </div>

          

          {framesDir && (
            <div style={{ marginTop: 6, fontFamily: "monospace", fontSize: 12, opacity: 0.9 }}>
              Frames folder: {framesDir}
            </div>
          )}

          <div style={{ marginTop: 10 }}>
            <label style={{ fontSize: 12, opacity: 0.8 }}>Max threads (0 = auto)</label>
            <input
              type="number"
              min={0}
              value={maxThreads}
              onChange={(e) => setMaxThreads(Number(e.currentTarget.value))}
              style={{ width: 120, marginLeft: 8 }}
            />
          </div>

          {pipelineStatus && (
            <div style={{ marginTop: 12, fontFamily: "monospace", fontSize: 12, opacity: 0.9 }}>
              {pipelineStatus}
            </div>
          )}

          {pipelineError && (
            <div style={{ marginTop: 8, fontFamily: "monospace", fontSize: 12, opacity: 0.9, whiteSpace: "pre-wrap" }}>
              {pipelineError}
            </div>
          )}

          {pipelineLog && (
            <div style={{ marginTop: 8, fontFamily: "monospace", fontSize: 12, opacity: 0.9, whiteSpace: "pre-wrap", maxHeight: 220, overflow: "auto" }}>
              {pipelineLog}
            </div>
          )}
        </div>
      </div>
    </main>
  );
}