import { defaultKeymap, history, historyKeymap } from "@codemirror/commands";
import {
  defaultHighlightStyle,
  StreamLanguage,
  syntaxHighlighting,
} from "@codemirror/language";
import { julia } from "@codemirror/legacy-modes/mode/julia";
import { lintGutter, setDiagnostics } from "@codemirror/lint";
import { Compartment, EditorState } from "@codemirror/state";
import { oneDark } from "@codemirror/theme-one-dark";
import {
  EditorView,
  highlightActiveLine,
  keymap,
  lineNumbers,
} from "@codemirror/view";
import { withBase } from "vitepress";
import { readSettings, settingsUrl } from "./playground-settings.mjs";

const sample = `# Summarize a vector, ignoring missing values.
function summarize(xs)
values=[x for x in xs if !ismissing(x)]
μ=sum(values)/length(values)
centered=values .- μ
(mean=μ,variance=sum(centered.^2)/length(values),n=length(values))
end

results=summarize([1,missing,3,5,8])
`;

let wasmReady;
async function loadWasm() {
  const url = new URL(
    withBase("/playground/pkg/fatou_wasm.js"),
    window.location.href,
  );
  const wasm = await import(/* @vite-ignore */ url.href);
  await wasm.default();
  return wasm;
}

export async function mountPlayground(root, signal) {
  wasmReady ??= loadWasm().catch((error) => {
    wasmReady = undefined;
    throw error;
  });
  const wasm = await wasmReady;
  if (signal.aborted) return;

  const get = (id) => root.querySelector(`#pg-${id}`);
  const controls = get("controls");
  const lineWidth = get("line-width");
  const indentWidth = get("indent-width");
  const copy = get("copy");
  const share = get("share");
  const status = get("status");
  const errors = get("errors");
  const initial = readSettings(window.location.search);
  lineWidth.value = initial.lineWidth;
  indentWidth.value = initial.indentWidth;

  const language = StreamLanguage.define(julia);
  const theme = () =>
    document.documentElement.classList.contains("dark")
      ? oneDark
      : syntaxHighlighting(defaultHighlightStyle, { fallback: true });
  const inputTheme = new Compartment();
  const outputTheme = new Compartment();
  let debounce;
  let formattedOutput = "";

  const output = new EditorView({
    parent: get("output"),
    state: EditorState.create({
      extensions: [
        EditorView.contentAttributes.of({
          "aria-labelledby": "pg-output-label",
          tabindex: "0",
        }),
        lineNumbers(),
        language,
        outputTheme.of(theme()),
        EditorState.readOnly.of(true),
        EditorView.editable.of(false),
      ],
    }),
  });
  const input = new EditorView({
    parent: get("input"),
    state: EditorState.create({
      doc: sample,
      extensions: [
        EditorView.contentAttributes.of({
          "aria-labelledby": "pg-input-label",
        }),
        lineNumbers(),
        history(),
        highlightActiveLine(),
        keymap.of([...defaultKeymap, ...historyKeymap]),
        language,
        inputTheme.of(theme()),
        lintGutter(),
        EditorView.updateListener.of((update) => {
          if (update.docChanged) {
            clearTimeout(debounce);
            setOutput("");
            errors.replaceChildren();
            status.textContent = "Formatting…";
            debounce = setTimeout(run, 200);
          }
        }),
      ],
    }),
  });

  function setOutput(text) {
    formattedOutput = text;
    copy.disabled = true;
    output.dispatch({
      changes: { from: 0, to: output.state.doc.length, insert: text },
    });
  }

  function options() {
    if (!lineWidth.validity.valid || !indentWidth.validity.valid) {
      throw new Error(
        "Line width and indent width must be whole numbers between 1 and 1000.",
      );
    }
    return {
      lineWidth: lineWidth.valueAsNumber,
      indentWidth: indentWidth.valueAsNumber,
    };
  }

  function run() {
    if (signal.aborted) return;
    clearTimeout(debounce);
    errors.replaceChildren();
    input.dispatch(setDiagnostics(input.state, []));
    try {
      const settings = options();
      const text = input.state.doc.toString();
      const raw = wasm.check(text);
      let diagnostics;
      try {
        diagnostics = raw.map((d) => ({
          message: d.message,
          start: d.start,
          end: d.end,
          line: d.line,
          column: d.column,
        }));
      } finally {
        for (const diagnostic of raw) diagnostic.free();
      }
      input.dispatch(
        setDiagnostics(
          input.state,
          diagnostics.map((d) => ({
            from: d.start,
            to: d.end,
            severity: "error",
            message: d.message,
          })),
        ),
      );
      errors.replaceChildren(
        ...diagnostics.map((d) => {
          const item = document.createElement("li");
          item.textContent = `${d.line}:${d.column} ${d.message}`;
          return item;
        }),
      );
      if (diagnostics.length) {
        setOutput("");
        status.textContent = `${diagnostics.length} parse error${diagnostics.length === 1 ? "" : "s"}. Fix the input to format it.`;
        return;
      }
      setOutput(wasm.format(text, settings.lineWidth, settings.indentWidth));
      copy.disabled = false;
      status.textContent = "";
    } catch (error) {
      setOutput("");
      status.textContent = error.message ?? String(error);
    }
  }

  const observer = new MutationObserver(() => {
    input.dispatch({ effects: inputTheme.reconfigure(theme()) });
    output.dispatch({ effects: outputTheme.reconfigure(theme()) });
  });
  observer.observe(document.documentElement, {
    attributes: true,
    attributeFilter: ["class"],
  });

  controls.addEventListener(
    "input",
    () => {
      try {
        const url = settingsUrl(window.location.href, options());
        window.history.replaceState(window.history.state, "", url);
      } catch {
        // Keep the previous shareable settings while a numeric field is incomplete.
      }
      run();
    },
    { signal },
  );

  async function writeClipboard(text, message) {
    try {
      await navigator.clipboard.writeText(text);
      if (!signal.aborted) status.textContent = message;
    } catch {
      if (!signal.aborted)
        status.textContent =
          "Could not access the clipboard. Select and copy the text manually.";
    }
  }
  copy.addEventListener(
    "click",
    () => {
      void writeClipboard(formattedOutput, "Output copied to clipboard.");
    },
    { signal },
  );
  share.addEventListener(
    "click",
    () => {
      try {
        const url = settingsUrl(window.location.href, options());
        void writeClipboard(
          url.href,
          "Settings link copied. Your code is not included.",
        );
      } catch (error) {
        status.textContent = error.message;
      }
    },
    { signal },
  );

  controls.disabled = false;
  run();
  return () => {
    clearTimeout(debounce);
    observer.disconnect();
    input.destroy();
    output.destroy();
  };
}
