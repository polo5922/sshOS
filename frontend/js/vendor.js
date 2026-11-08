// Third-party widgets for the Leptos UI, bundled by esbuild into frontend/public/vendor.js.
// Exposed as window.SshOsTerm / window.SshOsEditor with a tiny handle-based API so the Rust
// side only exchanges numbers and strings.
import { Terminal } from "@xterm/xterm";
import { FitAddon } from "@xterm/addon-fit";
import { WebLinksAddon } from "@xterm/addon-web-links";

import { EditorView, basicSetup } from "codemirror";
import { keymap } from "@codemirror/view";
import { EditorState, Compartment } from "@codemirror/state";
import { indentWithTab } from "@codemirror/commands";
import { StreamLanguage } from "@codemirror/language";
import { oneDark } from "@codemirror/theme-one-dark";
import { javascript } from "@codemirror/lang-javascript";
import { json } from "@codemirror/lang-json";
import { yaml } from "@codemirror/lang-yaml";
import { python } from "@codemirror/lang-python";
import { markdown } from "@codemirror/lang-markdown";
import { html } from "@codemirror/lang-html";
import { css } from "@codemirror/lang-css";
import { nginx } from "@codemirror/legacy-modes/mode/nginx";
import { shell } from "@codemirror/legacy-modes/mode/shell";
import { properties } from "@codemirror/legacy-modes/mode/properties";
import { toml } from "@codemirror/legacy-modes/mode/toml";
import { dockerFile } from "@codemirror/legacy-modes/mode/dockerfile";
import { sql } from "@codemirror/legacy-modes/mode/sql";

let next = 1;
const terms = new Map();
const editors = new Map();

const enc = new TextEncoder();
function toB64(str) {
  const bytes = enc.encode(str);
  let bin = "";
  for (let i = 0; i < bytes.length; i += 0x8000) bin += String.fromCharCode.apply(null, bytes.subarray(i, i + 0x8000));
  return btoa(bin);
}
function fromB64(b64) {
  const bin = atob(b64);
  const out = new Uint8Array(bin.length);
  for (let i = 0; i < bin.length; i++) out[i] = bin.charCodeAt(i);
  return out;
}

const darkTheme = { background: "#14161b", foreground: "#d8dee9", cursor: "#88c0d0", selectionBackground: "#3b4252" };
const lightTheme = { background: "#fbfbfd", foreground: "#1f2328", cursor: "#0969da", selectionBackground: "#c8d7f5" };

window.SshOsTerm = {
  /** onData(base64) is called with user input; onResize(cols, rows) after fits. */
  create(el, dark, onData, onResize) {
    const term = new Terminal({
      cursorBlink: true,
      fontFamily: '"Cascadia Code", "JetBrains Mono", Menlo, Consolas, monospace',
      fontSize: 13,
      scrollback: 10000,
      allowProposedApi: true,
      theme: dark ? darkTheme : lightTheme,
    });
    const fit = new FitAddon();
    term.loadAddon(fit);
    term.loadAddon(new WebLinksAddon());
    term.open(el);
    term.onData((d) => onData(toB64(d)));
    term.onBinary((d) => {
      const b = new Uint8Array(d.length);
      for (let i = 0; i < d.length; i++) b[i] = d.charCodeAt(i) & 255;
      let bin = "";
      b.forEach((c) => (bin += String.fromCharCode(c)));
      onData(btoa(bin));
    });
    term.onResize(({ cols, rows }) => onResize(cols, rows));
    const ro = new ResizeObserver(() => {
      try { if (el.offsetWidth > 0 && el.offsetHeight > 0) fit.fit(); } catch (_) {}
    });
    ro.observe(el);
    const id = next++;
    terms.set(id, { term, fit, ro });
    try { fit.fit(); } catch (_) {}
    return id;
  },
  size(id) {
    const t = terms.get(id);
    return t ? [t.term.cols, t.term.rows] : [80, 24];
  },
  write(id, b64) {
    const t = terms.get(id);
    if (t) t.term.write(fromB64(b64));
  },
  writeText(id, text) {
    const t = terms.get(id);
    if (t) t.term.write(text);
  },
  fit(id) {
    const t = terms.get(id);
    if (t) try { t.fit.fit(); } catch (_) {}
  },
  focus(id) {
    const t = terms.get(id);
    if (t) t.term.focus();
  },
  setDark(id, dark) {
    const t = terms.get(id);
    if (t) t.term.options.theme = dark ? darkTheme : lightTheme;
  },
  dispose(id) {
    const t = terms.get(id);
    if (t) { t.ro.disconnect(); t.term.dispose(); terms.delete(id); }
  },
};

function langFor(name) {
  const n = (name || "").toLowerCase();
  const base = n.split("/").pop();
  if (n.includes("nginx") || n.includes("sites-available") || n.includes("sites-enabled") || base.endsWith(".conf") && n.includes("/nginx/"))
    return StreamLanguage.define(nginx);
  if (base === "dockerfile" || base.endsWith(".dockerfile")) return StreamLanguage.define(dockerFile);
  const ext = base.includes(".") ? base.split(".").pop() : "";
  switch (ext) {
    case "js": case "mjs": case "cjs": case "jsx": return javascript({ jsx: true });
    case "ts": case "tsx": return javascript({ typescript: true, jsx: ext === "tsx" });
    case "json": return json();
    case "yml": case "yaml": return yaml();
    case "py": return python();
    case "md": case "markdown": return markdown();
    case "html": case "htm": case "php": return html();
    case "css": case "scss": return css();
    case "sh": case "bash": case "zsh": case "profile": case "bashrc": return StreamLanguage.define(shell);
    case "toml": return StreamLanguage.define(toml);
    case "ini": case "cfg": case "env": case "properties": return StreamLanguage.define(properties);
    case "sql": return StreamLanguage.define(sql({}));
    case "conf": return StreamLanguage.define(nginx);
  }
  if (base.startsWith(".bash") || base === ".profile" || base === ".zshrc") return StreamLanguage.define(shell);
  if (base.startsWith(".env")) return StreamLanguage.define(properties);
  return [];
}

window.SshOsEditor = {
  /** onChange() fires on every edit, onSave() on Ctrl/Cmd+S. */
  create(el, text, filename, dark, onChange, onSave) {
    const theme = new Compartment();
    const view = new EditorView({
      parent: el,
      state: EditorState.create({
        doc: text,
        extensions: [
          basicSetup,
          keymap.of([indentWithTab, { key: "Mod-s", preventDefault: true, run: () => { onSave(); return true; } }]),
          langFor(filename),
          theme.of(dark ? oneDark : []),
          EditorView.updateListener.of((u) => { if (u.docChanged) onChange(); }),
          EditorView.theme({ "&": { height: "100%" }, ".cm-scroller": { fontFamily: '"Cascadia Code", "JetBrains Mono", Menlo, Consolas, monospace' } }),
        ],
      }),
    });
    const id = next++;
    editors.set(id, { view, theme });
    return id;
  },
  getText(id) {
    const e = editors.get(id);
    return e ? e.view.state.doc.toString() : "";
  },
  setText(id, text) {
    const e = editors.get(id);
    if (e) e.view.dispatch({ changes: { from: 0, to: e.view.state.doc.length, insert: text } });
  },
  setDark(id, dark) {
    const e = editors.get(id);
    if (e) e.view.dispatch({ effects: e.theme.reconfigure(dark ? oneDark : []) });
  },
  focus(id) {
    const e = editors.get(id);
    if (e) e.view.focus();
  },
  dispose(id) {
    const e = editors.get(id);
    if (e) { e.view.destroy(); editors.delete(id); }
  },
};
