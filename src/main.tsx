import React from "react";
import ReactDOM from "react-dom/client";
import App from "./App";

function showFatal(msg: string, err?: unknown) {
  const el = document.getElementById("root");
  const detail =
    err instanceof Error
      ? `${err.name}: ${err.message}\n\n${err.stack ?? ""}`
      : err
      ? String(err)
      : "";
  const text = detail ? `${msg}\n\n${detail}` : msg;

  if (el) {
    el.innerHTML = "";
    const pre = document.createElement("pre");
    pre.style.whiteSpace = "pre-wrap";
    pre.style.fontFamily = "ui-monospace, SFMono-Regular, Menlo, Monaco, Consolas, monospace";
    pre.style.fontSize = "12px";
    pre.style.padding = "12px";
    pre.textContent = text;
    el.appendChild(pre);
  } else {
    document.body.textContent = text;
  }

  // also log for devtools
  // eslint-disable-next-line no-console
  console.error(msg, err);
}

window.addEventListener("error", (e) => {
  showFatal("UI crashed (window error).", (e as any).error ?? e);
});

window.addEventListener("unhandledrejection", (e) => {
  showFatal("UI crashed (unhandled promise rejection).", (e as any).reason ?? e);
});

const rootEl = document.getElementById("root");
if (!rootEl) {
  showFatal("Could not find #root element in index.html");
} else {
  try {
    ReactDOM.createRoot(rootEl as HTMLElement).render(
      <React.StrictMode>
        <App />
      </React.StrictMode>,
    );
  } catch (err) {
    showFatal("UI crashed during initial render.", err);
  }
}
