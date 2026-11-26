// Chart specifications consume only committed benchmark data.
function orderedUnique(rows, field) {
  return [...new Set(rows.map((row) => row[field]))];
}

// Label 1, 2, and 5 in each decade.
function timeScale(points, field) {
  const values = points.map((point) => point[field]);
  const low = values.length ? Math.floor(Math.log10(Math.min(...values))) : 0;
  const high = values.length ? Math.ceil(Math.log10(Math.max(...values))) : 1;
  const ticks = [];
  for (let exponent = low; exponent < high; exponent++) {
    for (const multiple of [1, 2, 5]) {
      ticks.push(multiple * 10 ** exponent);
    }
  }
  ticks.push(10 ** high);
  return { domain: [10 ** low / 1.1, 10 ** high * 1.1], ticks };
}

function spec(rows, kind, inputTitle) {
  const lsp = kind.startsWith("lsp-");
  const memory = kind === "lsp-memory";
  const requests = kind === "lsp-requests";
  const field = lsp ? "value" : "median_ms";
  const category = lsp ? "metric" : "scenario";
  const series = lsp ? "server" : "tool";
  const points = rows.filter(
    (point) =>
      Number.isFinite(point[field]) &&
      (memory ? point[field] >= 0 : point[field] > 0),
  );
  const dark = document.documentElement.classList.contains("dark");
  const fg = dark ? "#c8c9db" : "#333333";
  const axisGray = dark ? "#40434d" : "#aaaaaa";
  const gridGray = dark ? "#535764" : "#eeeeee";
  const tools = orderedUnique(rows, series);
  const categories = orderedUnique(rows, category);
  const valueTitle =
    {
      cold: "Median cold-start time (ms)",
      "lsp-readiness": "Readiness time (s)",
      "lsp-requests": "Median request latency (ms)",
      "lsp-memory": "Process-tree RSS (MB)",
    }[kind] ?? "Median formatting time (ms)";

  const chart = {
    $schema: "https://vega.github.io/schema/vega-lite/v5.json",
    width: "container",
    height: Math.max(1, categories.length) * (memory ? 64 : 40),
    data: { values: points },
    config: {
      background: null,
      view: { stroke: null },
      axis: {
        labelColor: fg,
        titleColor: fg,
        gridColor: gridGray,
        domainColor: axisGray,
        tickColor: axisGray,
      },
      legend: { labelColor: fg, titleColor: fg },
    },
  };
  if (memory) {
    return {
      ...chart,
      description:
        "Grouped bar chart of resident memory in MB at each session stage. " +
        "Each server has its own bar on a linear axis starting at zero. " +
        "See the data table for measurements.",
      mark: { type: "bar" },
      encoding: {
        y: {
          field: category,
          type: "nominal",
          title: "Stage",
          sort: categories,
        },
        yOffset: { field: series, type: "nominal", sort: tools },
        x: {
          field,
          type: "quantitative",
          title: valueTitle,
          scale: { type: "linear", zero: true },
        },
        color: { field: series, type: "nominal", title: "Server", sort: tools },
        tooltip: [
          { field: category, title: "Stage" },
          { field: series, title: "Server" },
          { field, title: valueTitle, format: ".1f" },
        ],
      },
    };
  }

  const toolTitle = lsp ? "Server" : "Tool";
  const { domain, ticks } = timeScale(points, field);
  return {
    ...chart,
    description:
      `${valueTitle} on a logarithmic scale. Lower values mean shorter waits. ` +
      "See the data table for measurements.",
    mark: { type: "point", filled: true, size: 130, opacity: 0.9 },
    encoding: {
      y: {
        field: category,
        type: "nominal",
        title: inputTitle,
        sort: categories,
        axis: { grid: true },
      },
      x: {
        field,
        type: "quantitative",
        title: valueTitle,
        scale: { type: "log", domain, nice: false },
        axis: {
          values: ticks,
          labelExpr: "format(datum.value, ',~g')",
          labelOverlap: "greedy",
          grid: false,
        },
      },
      color: {
        field: series,
        type: "nominal",
        title: toolTitle,
        sort: tools,
        legend: { orient: "top" },
      },
      tooltip: [
        { field: category, title: inputTitle },
        { field: series, title: toolTitle },
        { field, title: valueTitle, format: ".3f" },
        ...(requests
          ? [
              { field: "p95", title: "p95 (ms)", format: ".3f" },
              { field: "returned_work", title: "Returned work" },
            ]
          : []),
        ...(!lsp
          ? [
              { field: "relative", title: "Relative to Fatou" },
              {
                field: "throughput_mbps",
                title: "Throughput (MB/s)",
                format: ".2f",
              },
            ]
          : []),
        ...(kind === "cold" &&
        points.some((point) => point.peak_rss_mib != null)
          ? [
              {
                field: "peak_rss_mib",
                title: "Median peak RSS (MiB)",
                format: ".1f",
              },
            ]
          : []),
        ...(!lsp && kind !== "cold"
          ? [
              { field: "files_ok", title: "Files" },
              { field: "total_bytes", title: "Bytes", format: "," },
            ]
          : []),
      ],
    },
  };
}

async function renderInto(container, points, isActive) {
  if (!window.vegaEmbed) {
    return;
  }
  const kind = container.dataset.kind || "throughput";
  // A running dev server may still serve cold-start data from before the file
  // label was added to the benchmark renderer.
  if (kind === "cold") {
    points = points.map((point) => ({
      ...point,
      scenario: point.scenario ?? "Cold start",
    }));
  }
  const inputTitle =
    {
      "lsp-readiness": "Phase",
      "lsp-requests": "Request",
      "lsp-memory": "Stage",
    }[kind] ??
    container.dataset.input ??
    container.dataset.legend ??
    "Input";
  const vlSpec = spec(points, kind, inputTitle);
  // Alt text on the container, mirroring the spec description Vega puts on the
  // rendered SVG, so the chart is labeled for assistive tech either way.
  container.setAttribute("role", "img");
  container.setAttribute("aria-label", vlSpec.description);
  return window
    .vegaEmbed(container, vlSpec, { actions: false, renderer: "svg" })
    .then((result) => {
      if (!isActive() || !container.isConnected) {
        result.view.finalize();
        return;
      }
      // The named container and adjacent table describe the whole chart.
      container.querySelector("svg")?.setAttribute("aria-hidden", "true");
      if (container.__benchView) container.__benchView.finalize();
      container.__benchView = result.view;
      if (!container.__benchRendered) {
        container
          .closest(".bench-chart-block")
          .querySelector(".bench-table")
          .removeAttribute("open");
        container.__benchRendered = true;
      }
    })
    .catch((err) => {
      // Leave the fallback table in place; surface the reason for debugging.
      console.error("bench-charts: failed to render", err);
    });
}

export function mountCharts() {
  let active = true;
  const containers = [...document.querySelectorAll(".bench-chart-block")].map(
    (block) => {
      const container = block.querySelector(".bench-chart");
      const points = JSON.parse(block.querySelector(".bench-data").textContent);
      renderInto(container, points, () => active);
      return container;
    },
  );
  return () => {
    active = false;
    for (const container of containers) {
      container.__benchView?.finalize();
      container.__benchView = null;
    }
  };
}
