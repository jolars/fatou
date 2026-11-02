import DefaultTheme from "vitepress/theme";
import { h } from "vue";
import BenchCharts from "./BenchCharts.vue";
import "./bench-charts.css";
import "./style.css";

export default {
  extends: DefaultTheme,
  Layout: () =>
    h(DefaultTheme.Layout, null, { "layout-bottom": () => h(BenchCharts) }),
};
