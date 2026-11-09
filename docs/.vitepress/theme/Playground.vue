<script setup>
import { onBeforeUnmount, onMounted, ref } from "vue";

const root = ref(null);
const status = ref(null);
const controller = new AbortController();
let dispose;

onMounted(async () => {
  try {
    const { mountPlayground } = await import("./playground.mjs");
    if (controller.signal.aborted) return;
    const cleanup = await mountPlayground(root.value, controller.signal);
    if (controller.signal.aborted) cleanup?.();
    else dispose = cleanup;
  } catch (error) {
    if (!controller.signal.aborted) {
      status.value.textContent = `Failed to load the playground: ${error.message ?? error}`;
    }
  }
});

onBeforeUnmount(() => {
  controller.abort();
  dispose?.();
});
</script>

<template>
  <section ref="root" class="fatou-playground" aria-label="Julia formatter">
    <noscript>The playground requires JavaScript and WebAssembly.</noscript>
    <fieldset id="pg-controls" class="pg-controls" disabled>
      <legend class="visually-hidden">Formatting options</legend>
      <label>
        Line width
        <input
          id="pg-line-width"
          type="number"
          min="1"
          max="1000"
          step="1"
          value="92"
          required
        />
      </label>
      <label>
        Indent width
        <input
          id="pg-indent-width"
          type="number"
          min="1"
          max="1000"
          step="1"
          value="4"
          required
        />
      </label>
      <button
        id="pg-share"
        class="pg-share"
        type="button"
        title="Copy a link with formatting settings only"
      >
        Share settings
      </button>
    </fieldset>
    <div class="pg-panes">
      <div class="pg-pane">
        <div id="pg-input-label" class="pg-pane-title">Input</div>
        <div id="pg-input" class="pg-editor"></div>
      </div>
      <div class="pg-pane">
        <div class="pg-pane-title">
          <span id="pg-output-label">Output</span>
          <button id="pg-copy" type="button" disabled>Copy</button>
        </div>
        <div id="pg-output" class="pg-editor"></div>
      </div>
    </div>
    <div id="pg-status" ref="status" role="status" aria-live="polite">
      Loading the playground…
    </div>
    <ul id="pg-errors" class="pg-errors" aria-label="Parse errors"></ul>
  </section>
</template>

<style scoped>
.fatou-playground {
  display: flex;
  flex-direction: column;
  gap: 16px;
  margin-top: 24px;
}

.pg-controls {
  display: flex;
  flex-wrap: wrap;
  align-items: end;
  gap: 16px;
  margin: 0;
  padding: 0;
  border: 0;
}

.visually-hidden {
  position: absolute;
  width: 1px;
  height: 1px;
  overflow: hidden;
  clip-path: inset(50%);
}

.pg-controls label {
  display: flex;
  flex-direction: column;
  gap: 4px;
  font-size: 14px;
}

.pg-controls input[type="number"] {
  width: 96px;
  padding: 4px 8px;
  color: var(--vp-c-text-1);
  background: var(--vp-c-bg);
  border: 1px solid var(--vp-c-divider);
  border-radius: 6px;
  font: inherit;
}

.pg-share {
  margin-left: auto;
}

button {
  padding: 4px 12px;
  color: var(--vp-c-text-1);
  background: var(--vp-c-bg-soft);
  border: 1px solid var(--vp-c-divider);
  border-radius: 6px;
  font-size: 14px;
  cursor: pointer;
}

button:hover:enabled {
  border-color: var(--vp-c-brand-1);
}

button:disabled,
.pg-controls:disabled {
  opacity: 0.6;
  cursor: default;
}

button:focus-visible,
input:focus-visible {
  outline: 2px solid var(--vp-c-brand-1);
  outline-offset: 2px;
}

.pg-panes {
  display: grid;
  grid-template-columns: minmax(0, 1fr) minmax(0, 1fr);
  gap: 16px;
  height: max(28rem, calc(100dvh - var(--vp-nav-height) - 22rem));
}

.pg-pane {
  display: flex;
  flex-direction: column;
  min-width: 0;
  min-height: 0;
  overflow: hidden;
  border: 1px solid var(--vp-c-divider);
  border-radius: 8px;
}

.pg-pane-title {
  display: flex;
  align-items: center;
  justify-content: space-between;
  min-height: 42px;
  padding: 4px 12px;
  background: var(--vp-c-bg-soft);
  border-bottom: 1px solid var(--vp-c-divider);
  font-size: 13px;
  font-weight: 600;
}

.pg-editor {
  flex: 1;
  min-height: 0;
  overflow: hidden;
}

.pg-editor :deep(.cm-editor) {
  height: 100%;
  font-size: 13px;
  line-height: 1.6;
  text-align: left;
}

.pg-editor :deep(.cm-scroller) {
  overflow: auto;
  font-family: var(--vp-font-family-mono);
}

.pg-editor :deep(.cm-editor.cm-focused) {
  outline: 2px solid var(--vp-c-brand-1);
  outline-offset: -2px;
}

#pg-status {
  min-height: 24px;
  font-size: 14px;
}

.pg-errors {
  margin: 0;
  color: var(--vp-c-danger-1);
  font-family: var(--vp-font-family-mono);
  font-size: 14px;
}

.pg-errors:empty {
  display: none;
}

@media (max-width: 900px) {
  .pg-panes {
    grid-template-columns: minmax(0, 1fr);
    height: auto;
  }

  .pg-editor {
    flex: none;
    height: 24rem;
  }
}

@media print {
  .fatou-playground {
    display: none;
  }
}
</style>
