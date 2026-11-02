// Chart specifications consume only committed benchmark data.
function gridColor(dark) {
  return dark ? "#3b3f5c" : "#dddddd";
}

// Whole decades keep the log scale easy to read; a small margin keeps dots
// at the domain boundaries clear of the plot edges.
function logDomain(points, field) {
  const values = points
    .map((p) => p[field])
    .filter((value) => Number.isFinite(value) && value > 0);
  return logExtent(values.concat([1]));
}

function logExtent(values) {
  values = values.filter((value) => Number.isFinite(value) && value > 0);
  if (!values.length) return [0.1, 10];
  let lo = Math.floor(Math.log10(Math.min.apply(null, values)));
  let hi = Math.ceil(Math.log10(Math.max.apply(null, values)));
  if (lo === hi) {
    lo--;
    hi++;
  }
  return [10 ** lo / 1.1, 10 ** hi * 1.1];
}

// Match ggplot2's log ticks: long at powers of ten, medium at five, and
// short at the other subdivisions. Only powers of ten receive labels.
function logAxis(domain, color, absolute) {
  const ticks = [];
  const major = [];
  const middle = [];
  for (
    let e = Math.floor(Math.log10(domain[0]));
    e <= Math.ceil(Math.log10(domain[1]));
    e++
  ) {
    for (let m = 1; m < 10; m++) {
      const value = m * 10 ** e;
      if (value >= domain[0] && value <= domain[1]) {
        ticks.push(value);
        if (m === 1) major.push(value);
        if (m === 5) middle.push(value);
      }
    }
  }
  const isMajor = `indexof(${JSON.stringify(major)}, datum.value) >= 0`;
  const isMiddle = `indexof(${JSON.stringify(middle)}, datum.value) >= 0`;
  return {
    values: ticks,
    labelExpr: `${isMajor} ? format(datum.value, ',~g') : ''`,
    labelOverlap: false,
    labelPadding: 4,
    tickColor: color,
    tickSize: {
      condition: [
        { test: isMajor, value: 9 },
        { test: isMiddle, value: 6 },
      ],
      value: 3,
    },
    grid: true,
    gridOpacity: {
      condition: {
        test: isMajor + (absolute ? "" : " && datum.value !== 1"),
        value: 1,
      },
      value: 0,
    },
  };
}

// The dashed reference line distinguishes Fatou's baseline from the grid.
function baselineLayer(dark) {
  return {
    mark: { type: "rule", strokeDash: [4, 4], color: gridColor(dark) },
    encoding: { y: { datum: 1, type: "quantitative" } },
  };
}

// Shared axis/legend theming so every chart tracks the light/dark toggle.
function themeConfig(dark) {
  var fg = dark ? "#c8c9db" : "#333333";
  var grid = gridColor(dark);
  return {
    background: null,
    view: { stroke: null },
    axis: {
      labelColor: fg,
      titleColor: fg,
      gridColor: grid,
      domainColor: grid,
      tickColor: grid,
    },
    legend: { labelColor: fg, titleColor: fg },
  };
}

// Unique values in first-appearance (benchmark artifact) order, so the legend
// keeps the corpus order the run recorded and the axis reads
// Fatou -> Runic -> JuliaFormatter rather than alphabetized.
function orderedUnique(rows, key) {
  var seen = Object.create(null);
  var out = [];
  rows.forEach((r) => {
    if (!(r[key] in seen)) {
      seen[r[key]] = true;
      out.push(r[key]);
    }
  });
  return out;
}

function seriesColor(field, title, values, compact) {
  return {
    field: field,
    type: "nominal",
    title: title,
    sort: values,
    legend: {
      orient: "top",
      direction: "horizontal",
      columns: compact ? 1 : 2,
      labelLimit: 240,
    },
  };
}

// Warm-loop dot plot: x = tool, one dot per scenario dodged within the tool's
// group, colored by scenario, y = time relative to Fatou on a log axis
// (Fatou = 1). Single files and projects get one of these each, so what a dot
// stands for ("File" or "Project") comes in as `legend`.
function spec(points, legend, compact) {
  var dark = document.documentElement.classList.contains("dark");
  var scenarios = orderedUnique(points, "scenario");
  var tools = orderedUnique(points, "tool");
  var domain = logDomain(points, "relative_time");
  var what = legend.toLowerCase();

  return {
    $schema: "https://vega.github.io/schema/vega-lite/v5.json",
    description:
      "Dot plot of formatting time relative to Fatou on a logarithmic scale; for " +
      "each tool one dot per " +
      what +
      ", with Fatou on a dashed baseline at 1 and " +
      "slower tools above. See the data table for the underlying numbers.",
    width: "container",
    height: 340,
    data: { values: points },
    layer: [
      baselineLayer(dark),
      {
        mark: { type: "point", filled: true, size: 130, opacity: 0.9 },
        encoding: {
          x: {
            field: "tool",
            type: "nominal",
            title: "Tool",
            sort: tools,
            axis: { labelAngle: 0 },
          },
          // Dodge the scenarios within each tool group; with several
          // scenarios per tool they otherwise collide wherever their
          // relative times are close.
          xOffset: {
            field: "scenario",
            type: "nominal",
            sort: scenarios,
          },
          y: {
            field: "relative_time",
            type: "quantitative",
            title: "Time relative to Fatou",
            scale: { type: "log", domain: domain, nice: false },
            axis: logAxis(domain, dark ? "#c8c9db" : "#333333"),
          },
          color: seriesColor("scenario", legend, scenarios, compact),
          tooltip: [
            { field: "scenario", title: legend },
            { field: "tool", title: "Tool" },
            { field: "relative", title: "Relative to Fatou" },
            { field: "median_ms", title: "Median (ms)", format: ".1f" },
            {
              field: "throughput_mbps",
              title: "Throughput (MB/s)",
              format: ".2f",
            },
            { field: "files_ok", title: "Files" },
            { field: "total_bytes", title: "Bytes", format: "," },
          ],
        },
      },
    ],
    config: themeConfig(dark),
  };
}

// Cold-start dot plot: one dot per tool, y = cold time relative to Fatou on a
// log axis (Fatou = 1).
function coldSpec(points) {
  var dark = document.documentElement.classList.contains("dark");
  var tools = orderedUnique(points, "tool");
  var domain = logDomain(points, "relative_time");

  return {
    $schema: "https://vega.github.io/schema/vega-lite/v5.json",
    description:
      "Dot plot of cold-start formatting time relative to Fatou on a logarithmic " +
      "scale; one dot per tool, with Fatou on a dashed baseline at 1 and slower " +
      "tools above. See the data table for the underlying numbers.",
    width: "container",
    height: 340,
    data: { values: points },
    layer: [
      baselineLayer(dark),
      {
        mark: { type: "point", filled: true, size: 130, opacity: 0.9 },
        encoding: {
          x: {
            field: "tool",
            type: "nominal",
            title: "Tool",
            sort: tools,
            axis: { labelAngle: 0 },
          },
          y: {
            field: "relative_time",
            type: "quantitative",
            title: "Cold-start time relative to Fatou",
            scale: { type: "log", domain: domain, nice: false },
            axis: logAxis(domain, dark ? "#c8c9db" : "#333333"),
          },
          color: {
            field: "tool",
            type: "nominal",
            sort: tools,
            legend: null,
          },
          tooltip: [
            { field: "tool", title: "Tool" },
            { field: "relative", title: "Relative to Fatou" },
            { field: "median_ms", title: "Cold start (ms)", format: ".1f" },
            {
              field: "throughput_mbps",
              title: "Throughput (MB/s)",
              format: ".2f",
            },
          ],
        },
      },
    ],
    config: themeConfig(dark),
  };
}

function serverSpec(points, kind, compact) {
  const dark = document.documentElement.classList.contains("dark");
  const config = themeConfig(dark);
  const memory = kind === "lsp-memory";
  const requests = kind === "lsp-requests";
  const servers = orderedUnique(points, "server");
  const metrics = orderedUnique(points, "metric");
  const tooltip = [
    { field: "server", title: "Server" },
    { field: "metric", title: requests ? "Request" : "Milestone" },
    {
      field: "value",
      title: memory ? "RSS (MB)" : requests ? "Median (ms)" : "Time (s)",
      format: memory ? ".1f" : ".3f",
    },
  ];
  if (requests) {
    tooltip.push(
      { field: "p95", title: "p95 (ms)", format: ".3f" },
      { field: "returned_work", title: "Returned work" },
    );
  }
  const chart = {
    $schema: "https://vega.github.io/schema/vega-lite/v5.json",
    description: memory
      ? "Resident memory in MB for each server at baseline, settled, and peak. " +
        "Lower bars use less memory. Expand Memory data for values."
      : requests
        ? "Median request latency in milliseconds on a log scale, grouped by " +
          "server and colored by request. Lower is faster. Expand the table " +
          "for timings, p95, and returned work."
        : "Readiness time in seconds on a log scale, grouped by server and " +
          "colored by phase. Lower is faster. Expand Readiness data for values.",
    width: "container",
    height: memory ? 260 : 340,
    data: { values: points },
    // Missing or zero timings remain in the table rather than on a log axis.
    transform: [
      {
        filter: memory
          ? "isValid(datum.value) && datum.value >= 0"
          : "isValid(datum.value) && datum.value > 0",
      },
    ],
    config: config,
  };
  if (memory) {
    chart.encoding = {
      x: {
        field: "metric",
        type: "nominal",
        sort: metrics,
        title: null,
        axis: { labelAngle: 0 },
      },
      xOffset: { field: "server", sort: servers },
      y: {
        field: "value",
        type: "quantitative",
        title: "Process-tree RSS (MB)",
        scale: { zero: true },
      },
    };
    chart.layer = [
      {
        mark: { type: "bar" },
        encoding: {
          color: {
            field: "server",
            type: "nominal",
            title: null,
            scale: {
              domain: servers,
              range: ["#4e79a7", "#f28e2c", "#e15759"],
            },
            legend: { orient: "top", direction: "horizontal" },
          },
          tooltip: tooltip,
        },
      },
      {
        mark: { type: "text", dy: -8, color: config.axis.labelColor },
        encoding: { text: { field: "value", format: ".1f" } },
      },
    ];
    return chart;
  }

  const domain = logExtent(points.map((point) => point.value));
  chart.encoding = {
    x: {
      field: "server",
      type: "nominal",
      sort: servers,
      title: "Server",
      axis: { labelAngle: 0 },
    },
    y: {
      field: "value",
      type: "quantitative",
      title: requests
        ? "Request latency (ms, log scale)"
        : "Elapsed time (s, log scale)",
      scale: { type: "log", domain: domain, nice: false },
      axis: logAxis(domain, config.axis.labelColor, true),
    },
    color: seriesColor(
      "metric",
      requests ? "Request" : "Wait",
      metrics,
      compact,
    ),
    tooltip: tooltip,
  };
  chart.mark = { type: "point", filled: true, size: 130, opacity: 0.9 };
  return chart;
}

async function renderInto(container, points, isActive) {
  if (!window.vegaEmbed) {
    return;
  }
  const kind = container.dataset.kind || "throughput";
  const compact = container.clientWidth < 500;
  const vlSpec = kind.startsWith("lsp-")
    ? serverSpec(points, kind, compact)
    : kind === "cold"
      ? coldSpec(points)
      : spec(points, container.dataset.legend || "Scenario", compact);
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
