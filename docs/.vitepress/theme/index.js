import DefaultTheme from "vitepress/theme";
import { h } from "vue";
import BenchCharts from "./BenchCharts.vue";
import Playground from "./Playground.vue";
import "./bench-charts.css";
import "./style.css";

export default {
  extends: DefaultTheme,
  enhanceApp({ app }) {
    app.component("FatouPlayground", Playground);
  },
  Layout: () =>
    h(DefaultTheme.Layout, null, { "layout-bottom": () => h(BenchCharts) }),
};
