// Renders the benchmark dot plots on the Performance page with Vega-Lite.
//
// Data is injected by the `doc-utils` mdbook preprocessor as an inline
// `<script type="application/json" class="bench-data">` next to a
// `<div class="bench-chart">` (see docs/doc-utils/src/lib.rs). The Vega runtime
// is vendored under theme/vendor/ and loaded before this file via book.toml's
// `additional-js`, so nothing is fetched at view time.
//
// Chart kinds are selected by the container's `data-kind`. Formatter plots show
// time relative to Fatou (Fatou = 1) on a log axis, with a dashed baseline at 1;
// lower is faster. Server plots show absolute time or memory on horizontal axes.
//   default ("throughput"): x = tool, one dot per scenario dodged within the
//                           tool's group (colored by scenario). The page draws
//                           two of these, one for single files and one for
//                           projects; the container's `data-legend` names what a
//                           dot is ("File", "Project").
//   "cold": x = tool, one dot per tool.
//   "lsp-readiness", "lsp-requests", "lsp-memory": y = phase, request, or
//   milestone, with one dot per server. Warm requests join medians to p95s.
(() => {
  // mdBook keeps the active theme as a class on <html>; these three are dark.
  function isDark() {
    var c = document.documentElement.classList;
    return c.contains("coal") || c.contains("navy") || c.contains("ayu");
  }

  function gridColor(dark) {
    return dark ? "#3b3f5c" : "#dddddd";
  }

  // Whole decades keep the log scale easy to read; a small margin keeps dots
  // at the domain boundaries clear of the plot edges.
  function logDomain(points, field) {
    const values = points
      .map((p) => p[field])
      .filter((value) => Number.isFinite(value) && value > 0);
    let lo = Math.floor(Math.log10(Math.min.apply(null, values.concat([1]))));
    let hi = Math.ceil(Math.log10(Math.max.apply(null, values.concat([1]))));
    if (lo === hi) {
      lo--;
      hi++;
    }
    return [10 ** lo / 1.1, 10 ** hi * 1.1];
  }

  // Match ggplot2's log ticks: long at powers of ten, medium at five, and
  // short at the other subdivisions. Only powers of ten receive labels.
  function logAxis(domain, color) {
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
      grid: false,
    };
  }

  // A single dashed reference line at y = 1: the Fatou baseline every dot is
  // measured against. It replaces the log-scale gridlines, which read as clutter.
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

  // Warm-loop dot plot: x = tool, one dot per scenario dodged within the tool's
  // group, colored by scenario, y = time relative to Fatou on a log axis
  // (Fatou = 1). Single files and projects get one of these each, so what a dot
  // stands for ("File" or "Project") comes in as `legend`.
  function spec(points, legend) {
    var dark = isDark();
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
            color: {
              field: "scenario",
              type: "nominal",
              title: legend,
              sort: scenarios,
            },
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
    var dark = isDark();
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

  function serverSpec(points, kind) {
    const dark = isDark();
    const config = themeConfig(dark);
    config.axis.labelFontSize = 12;
    config.legend.labelFontSize = 12;
    const memory = kind === "lsp-memory";
    const requests = kind === "lsp-requests";
    const servers = orderedUnique(points, "server");
    const metrics = orderedUnique(points, "metric");
    // Include p95 in the domain so the tail of a request never gets clipped.
    const domain = logDomain(
      points.flatMap((p) => [{ value: p.value }, { value: p.p95 }]),
      "value",
    );
    const colors = dark
      ? ["#56b4e9", "#e69f00", "#009e73"]
      : ["#0072b2", "#d55e00", "#009e73"];
    const serverOrder = ["Fatou", "LanguageServer.jl", "JETLS"];
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
    const layers = [];
    if (requests) {
      // This is the observed median-to-p95 span, not uncertainty in the median.
      layers.push({
        transform: [{ filter: "isValid(datum.p95) && datum.p95 > 0" }],
        mark: { type: "rule", strokeWidth: 2, opacity: 0.6 },
        encoding: { x2: { field: "p95" } },
      });
    }
    layers.push({
      mark: { type: "point", filled: true, size: 85, opacity: 1 },
    });
    if (requests) {
      layers.push({
        transform: [{ filter: "isValid(datum.p95) && datum.p95 > 0" }],
        mark: {
          type: "point",
          filled: false,
          size: 85,
          strokeWidth: 2,
          opacity: 1,
        },
        encoding: { x: { field: "p95", type: "quantitative" } },
      });
    }
    return {
      $schema: "https://vega.github.io/schema/vega-lite/v5.json",
      description: memory
        ? "Resident memory in MB for each server at baseline, settled, and peak, " +
          "on a linear scale. Left uses less memory. Expand Memory data for values."
        : requests
          ? "Warm request latency in milliseconds on a log scale, grouped by " +
            "request and colored by server. Filled dots are medians, hollow dots " +
            "are p95. Left is faster. Expand the table for timings and returned work."
          : "Readiness time in seconds on a log scale, grouped by phase and " +
            "colored by server. Left is faster. Expand Readiness data for values.",
      width: "container",
      autosize: { type: "fit-x", contains: "padding" },
      height: metrics.length * 75,
      data: { values: points },
      // A missing or zero timing cannot be placed on a log axis; it remains in
      // the detail table rather than being presented as a measured positive time.
      transform: [
        {
          filter: memory
            ? "isValid(datum.value) && datum.value >= 0"
            : "isValid(datum.value) && datum.value > 0",
        },
      ],
      encoding: {
        y: {
          field: "metric",
          type: "nominal",
          sort: metrics,
          title: null,
          axis: {
            labelLimit: 130,
            ticks: false,
            domain: false,
            labelPadding: 10,
          },
        },
        yOffset: { field: "server", type: "nominal", sort: servers },
        x: {
          field: "value",
          type: "quantitative",
          title: memory
            ? "Resident memory (MB)"
            : requests
              ? "Request latency (ms, log scale)"
              : "Readiness time (s, log scale)",
          scale: memory
            ? { zero: true }
            : { type: "log", domain: domain, nice: false },
          axis: memory
            ? { tickCount: 5, format: ",.0f", grid: false }
            : {
                ...logAxis(domain, dark ? "#c8c9db" : "#333333"),
                labelOverlap: "greedy",
              },
        },
        color: {
          field: "server",
          type: "nominal",
          sort: servers,
          scale: {
            domain: servers,
            range: servers.map((server, i) => {
              const index = serverOrder.indexOf(server);
              return colors[(index < 0 ? i : index) % colors.length];
            }),
          },
          legend: {
            title: null,
            orient: "bottom",
            columns: 1,
            symbolOpacity: 1,
          },
        },
        tooltip: tooltip,
      },
      layer: layers,
      config: config,
    };
  }

  function renderInto(container, points) {
    if (!window.vegaEmbed) {
      return;
    }
    const kind = container.dataset.kind || "throughput";
    const vlSpec = kind.startsWith("lsp-")
      ? serverSpec(points, kind)
      : kind === "cold"
        ? coldSpec(points)
        : spec(points, container.dataset.legend || "Scenario");
    // Alt text on the container, mirroring the spec description Vega puts on the
    // rendered SVG, so the chart is labeled for assistive tech either way.
    container.setAttribute("role", "img");
    container.setAttribute("aria-label", vlSpec.description);
    window
      .vegaEmbed(container, vlSpec, { actions: false, renderer: "svg" })
      .catch((err) => {
        // Leave the fallback table in place; surface the reason for debugging.
        console.error("bench-charts: failed to render", err);
      });
  }

  function init() {
    var blocks = document.querySelectorAll(".bench-chart-block");
    if (!blocks.length) {
      return;
    }
    blocks.forEach((block) => {
      var container = block.querySelector(".bench-chart");
      var data = block.querySelector("script.bench-data");
      if (!container || !data) {
        return;
      }
      var points;
      try {
        points = JSON.parse(data.textContent);
      } catch (err) {
        console.error("bench-charts: bad data payload", err);
        return;
      }
      if (!Array.isArray(points) || !points.length) {
        return;
      }
      container.__benchPoints = points;
      renderInto(container, points);
    });

    // Re-render on light/dark toggle so axis and legend colors track the theme.
    var observer = new MutationObserver(() => {
      document.querySelectorAll(".bench-chart").forEach((container) => {
        if (container.__benchPoints) {
          renderInto(container, container.__benchPoints);
        }
      });
    });
    observer.observe(document.documentElement, {
      attributes: true,
      attributeFilter: ["class"],
    });
  }

  if (document.readyState === "loading") {
    document.addEventListener("DOMContentLoaded", init);
  } else {
    init();
  }
})();
