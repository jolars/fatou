<script setup>
import { onMounted, onBeforeUnmount, watch, nextTick } from "vue";
import { useRoute, useData } from "vitepress";
import { mountCharts } from "./bench-charts.js";

const route = useRoute();
const { isDark } = useData();
let dispose = () => {};
let generation = 0;
let runtime;

async function render() {
  const current = ++generation;
  dispose();
  await nextTick();
  if (!document.querySelector(".bench-chart")) return;
  runtime ??= (async () => {
    for (const name of ["vega", "vega-lite", "vega-embed"]) {
      await new Promise((resolve, reject) => {
        const script = document.createElement("script");
        script.src = `/theme/vendor/${name}.min.js`;
        script.onload = resolve;
        script.onerror = reject;
        document.head.append(script);
      });
    }
  })();
  try {
    await runtime;
    if (generation === current) dispose = mountCharts();
  } catch (error) {
    runtime = undefined;
    console.error("Could not load benchmark charts", error);
  }
}

onMounted(render);
watch([() => route.path, isDark], render);
onBeforeUnmount(() => {
  generation++;
  dispose();
});
</script>

<template><span hidden /></template>
