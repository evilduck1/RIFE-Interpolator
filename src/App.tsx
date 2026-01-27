import { useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { open, save } from "@tauri-apps/plugin-dialog";
import "./App.css";

type Status = "missing" | "installed" | "unknown";

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

function App() {
  function isTauriRuntime(): boolean {
    return typeof window !== "undefined" && (!!(window as any).__TAURI_INTERNALS__ || !!(window as any).__TAURI__);
  }

  const [envMsg, setEnvMsg] = useState<string>("");
  const [paths, setPaths] = useState<string[]>([]);
  const [ffmpegStatus, setFfmpegStatus] = useState<Status>("unknown");
  const [rifeStatus, setRifeStatus] = useState<Status>("unknown");

  const [ffmpegPath, setFfmpegPath] = useState<string>("");
  const [rifePath, setRifePath] = useState<string>("");

  const [validation, setValidation] = useState<ValidateToolsResult | null>(null);
  const [validating, setValidating] = useState(false);
  const [error, setError] = useState<string>("");

  const [inputVideo, setInputVideo] = useState<string>("");
  const [framesDir, setFramesDir] = useState<string>("");
  const [extracting, setExtracting] = useState(false);
  const [pipelineStatus, setPipelineStatus] = useState<string>("");
  const [pipelineError, setPipelineError] = useState<string>("");
  const [maxThreads, setMaxThreads] = useState<number>(0);
  const [outputVideo, setOutputVideo] = useState<string>("");

  async function checkEnv() {
    const result = await invoke<string>("check_environment");
    setEnvMsg(result);
  }

  async function refresh() {
    const p = await invoke<string[]>("get_app_paths");
    setPaths(p);

    const f = await invoke<string>("tool_status", { tool: "ffmpeg" });
    const r = await invoke<string>("tool_status", { tool: "rife" });

    setFfmpegStatus((f as Status) ?? "unknown");
    setRifeStatus((r as Status) ?? "unknown");
  }

  async function install(tool: "ffmpeg" | "rife") {
    const sourcePath = tool === "ffmpeg" ? ffmpegPath : rifePath;
    if (!sourcePath.trim()) return;

    setError("");
    await invoke<string>("install_tool", {
      sourcePath,
      tool,
      version: "v1",
    });

    await refresh();
  }

  async function validate() {
    setValidating(true);
    setError("");
    try {
      const res = await invoke<ValidateToolsResult>("validate_tools");
      setValidation(res);
        } catch (e: any) {
      setPipelineStatus("Failed.");
      setError(String(e));
      } finally {
      setValidating(false);
    }
  }

      async function pickInputVideo() {
    const file = await open({ multiple: false, filters: [{ name: "Video", extensions: ["mp4","mov","mkv","avi","webm"] }] });
    if (typeof file === "string") setInputVideo(file);
  }

  async function pickOutputVideo() {
    const file = await save({ filters: [{ name: "Video", extensions: ["mp4","mov","mkv"] }] });
    if (typeof file === "string") setOutputVideo(file);
  }

async function extractFrames() {
    if (!inputVideo.trim()) return;

    setExtracting(true);
    setPipelineStatus("Extracting frames…");
    setPipelineError("");
    setError("");
    try {
      const res = await invoke<ExtractFramesResult>("extract_frames", { videoPath: inputVideo, maxThreads });
      setFramesDir(res.frames_dir);
      // Job started; final status comes from pipeline_done event.
      setPipelineStatus("Running…");
      if (!isTauriRuntime()) {
        // In web-only dev mode there are no backend events.
        setExtracting(false);
      }
    } catch (e: any) {
      setPipelineStatus("Failed.");
      const msg = String(e);
      setPipelineError(msg);
      setError(msg);
      setExtracting(false);
    }
  }

useEffect(() => {
    refresh();
  }, []);

  useEffect(() => {
    let unlistenProgress: null | (() => void) = null;
    let unlistenDone: null | (() => void) = null;
    let unlistenLog: null | (() => void) = null;

    (async () => {
      if (!isTauriRuntime()) {
        return;
      }
      try {
      unlistenProgress = await listen<number>("pipeline_progress", (e) => {
        const pct = e.payload;
        if (typeof pct === "number") {
          setPipelineStatus(`Extracting… ${pct.toFixed(1)}%`);
        }
      });

      unlistenLog = await listen<string>("pipeline_log", (e) => {
        const msg = (e.payload || "").toString();
        if (msg) setPipelineStatus(msg);
      });

      unlistenDone = await listen<any>("pipeline_done", (e) => {
        const p = e.payload as any;
        setExtracting(false);

        if (p?.ok) {
          setPipelineStatus(p.message || "Done.");
          setPipelineError("");
          if (p.frames_dir) setFramesDir(p.frames_dir);
        } else {
          setPipelineStatus("Failed.");
          const msg = p?.message ? String(p.message) : "Unknown error";
          setPipelineError(msg);
          setError(msg);
        }
      });
      } catch (e: any) {
        const msg = `Event listener setup failed: ${String(e)}`;
        setPipelineStatus("Failed.");
        setPipelineError(msg);
        setError(msg);
        setExtracting(false);
      }
    })();

    return () => {
      try { unlistenProgress?.(); } catch {}
      try { unlistenLog?.(); } catch {}
      try { unlistenDone?.(); } catch {}
    };
  }, []);

  const boxStyle: React.CSSProperties = {
    padding: 12,
    borderRadius: 12,
    background: "rgba(255,255,255,0.04)",
    marginTop: 10,
    fontFamily: "monospace",
    fontSize: 12,
    whiteSpace: "pre-wrap",
    overflowWrap: "anywhere",
  };
  return (
    <main className="container">
      <h1>RIFE Interpolator</h1>

      <button onClick={checkEnv}>Check Environment</button>
      {envMsg && <p>{envMsg}</p>}

      <hr />

      <h3>Managed app paths</h3>
      <div style={{ textAlign: "left", maxWidth: 900, margin: "0 auto" }}>
        {paths.map((p) => (
          <div key={p} style={{ fontFamily: "monospace", fontSize: 13, opacity: 0.9 }}>
            {p}
          </div>
        ))}
      </div>

      <hr />

      <h3>Tools</h3>

      <div style={{ maxWidth: 900, margin: "0 auto", textAlign: "left" }}>
        <div style={{ marginBottom: 16 }}>
          <div style={{ marginBottom: 6 }}>
            <strong>ffmpeg</strong>: {ffmpegStatus}
          </div>
          <input
            value={ffmpegPath}
            onChange={(e) => setFfmpegPath(e.currentTarget.value)}
            placeholder="Paste full path to ffmpeg (e.g. /opt/homebrew/bin/ffmpeg or C:\\path\\ffmpeg.exe)"
            style={{ width: "100%", marginBottom: 8 }}
          />
          <button onClick={() => install("ffmpeg")}>Install ffmpeg into app</button>
        </div>

        <div>
          <div style={{ marginBottom: 6 }}>
            <strong>RIFE</strong>: {rifeStatus}
          </div>
          <input
            value={rifePath}
            onChange={(e) => setRifePath(e.currentTarget.value)}
            placeholder="Paste full path to RIFE folder OR binary (e.g. /Downloads/rife-ncnn-vulkan-... or C:\\path\\rife.exe)"
            style={{ width: "100%", marginBottom: 8 }}
          />
          <button onClick={() => install("rife")}>Install RIFE into app</button>
        </div>

        <hr />

        <button onClick={validate} disabled={validating}>
          {validating ? "Validating..." : "Validate Tools"}
        </button>

        {error && <div style={{ ...boxStyle, marginTop: 12 }}>{error}</div>}

        {validation && (
          <div style={{ marginTop: 12 }}>
            <div style={{ marginBottom: 10 }}>
              <strong>ffmpeg</strong>: {validation.ffmpeg.ok ? "OK" : "FAILED"}
              {validation.ffmpeg.path ? (
                <div style={{ opacity: 0.8, fontFamily: "monospace", fontSize: 12 }}>
                  {validation.ffmpeg.path}
                </div>
              ) : null}
              <div style={boxStyle}>{validation.ffmpeg.output || "(no output)"}</div>
            </div>

            <div>
              <strong>RIFE</strong>: {validation.rife.ok ? "OK" : "FAILED"}
              {validation.rife.path ? (
                <div style={{ opacity: 0.8, fontFamily: "monospace", fontSize: 12 }}>
                  {validation.rife.path}
                </div>
              ) : null}
              <div style={boxStyle}>{validation.rife.output || "(no output)"}</div>
            </div>
          </div>
        )}
      </div>
    
      <hr />

      <h3>Pipeline</h3>

      <div style={{ maxWidth: 900, margin: "0 auto", textAlign: "left" }}>
        <div style={{ marginBottom: 6, opacity: 0.9 }}>
          <strong>Input video</strong>
        </div>
        <div style={{ display: "flex", gap: 8, marginBottom: 8 }}>
          <input value={inputVideo} readOnly placeholder="Choose input video…" style={{ flex: 1 }} />
          <button onClick={pickInputVideo}>Browse…</button>
        </div>

        <div style={{ marginBottom: 6, opacity: 0.9 }}><strong>Output video</strong></div>
        <div style={{ display: "flex", gap: 8, marginBottom: 8 }}>
          <input value={outputVideo} readOnly placeholder="Choose output video…" style={{ flex: 1 }} />
          <button onClick={pickOutputVideo}>Browse…</button>
        </div>

<button onClick={extractFrames} disabled={extracting || !inputVideo.trim()}>
          {extracting ? "Extracting…" : "Extract Frames"}
        </button>

        {framesDir && (
          <div style={{ marginTop: 10, fontFamily: "monospace", fontSize: 12, opacity: 0.9 }}>
            Frames folder: {framesDir}
          </div>
        )}
        
        <div style={{ marginTop: 10 }}>
          <label style={{ fontSize: 12, opacity: 0.8 }}>
            Max threads (0 = auto)
          </label>
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
      </div>

    </main>
  );
}

export default App;
