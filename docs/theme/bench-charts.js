// Renders the benchmark dot plots on the Performance page with Vega-Lite.
//
// Data is injected by the `doc-utils` mdbook preprocessor as an inline
// `<script type="application/json" class="bench-data">` next to a
// `<div class="bench-chart">` (see docs/doc-utils/src/lib.rs). The Vega runtime
// is vendored under theme/vendor/ and loaded before this file via book.toml's
// `additional-js`, so nothing is fetched at view time.
//
// Two chart kinds, selected by the container's `data-kind`. Both are dot plots,
// not bar charts: a log y-axis has no zero baseline for bars to grow from, but
// points sit cleanly at their value regardless of scale. Both plot time relative
// to Fatou (Fatou = 1) on a log axis, with a single dashed reference line at 1 in
// place of gridlines; lower is faster.
//   default ("throughput"): x = tool, one dot per scenario dodged within the
//                           tool's group (colored by scenario). The page draws
//                           two of these, one for single files and one for
//                           projects; the container's `data-legend` names what a
//                           dot is ("File", "Project").
//   "cold": x = tool, one dot per tool.
(() => {
  // mdBook keeps the active theme as a class on <html>; these three are dark.
  function isDark() {
    var c = document.documentElement.classList;
    return c.contains("coal") || c.contains("navy") || c.contains("ayu");
  }

  function gridColor(dark) {
    return dark ? "#3b3f5c" : "#dddddd";
  }

  // A padded log-scale domain for `field`, so the baseline at 1 (the data
  // minimum, since Fatou is always 1) doesn't sit flush against the axis floor
  // and the slowest tool doesn't touch the top. Padding is multiplicative
  // (a constant margin in log space) rather than additive.
  function logDomain(points, field) {
    var vals = points
      .map((p) => p[field])
      .filter((v) => typeof v === "number" && v > 0);
    var lo = Math.min.apply(null, vals);
    var hi = Math.max.apply(null, vals);
    var pad = 1.6;
    return [lo / pad, hi * pad];
  }

  // Tick values for the log axis, in decreasing order of preference: exact
  // powers of ten, then a 1-2-5 ladder. The first ladder placing at least three
  // ticks inside `domain` wins, so a wide chart reads 1, 10, 100 and a narrow
  // one 1, 2, 5 rather than the dozen-plus intermediate values the scale would
  // otherwise pick under two decades. Returns undefined when even the finer
  // ladder is too sparse; the scale then picks its own ticks, which "~f" still
  // renders as plain decimals.
  function logTicks(domain) {
    const [lo, hi] = domain;
    for (const ladder of [[1], [1, 2, 5]]) {
      const ticks = [];
      for (
        let e = Math.floor(Math.log10(lo));
        e <= Math.ceil(Math.log10(hi));
        e++
      ) {
        for (const m of ladder) {
          const v = m * 10 ** e;
          if (v >= lo && v <= hi) {
            ticks.push(v);
          }
        }
      }
      if (ticks.length >= 3) {
        return ticks;
      }
    }
    return undefined;
  }

  // A single dashed reference line at y = 1: the Fatou baseline every dot is
  // measured against. It replaces the log-scale gridlines, which read as clutter.
  function baselineLayer(dark) {
    return {
      mark: { type: "rule", strokeDash: [4, 4], color: gridColor(dark) },
      encoding: { y: { datum: 1, type: "quantitative" } },
    };
  }

  // Shared axis/legend theming so both chart kinds track the light/dark toggle.
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
              scale: { type: "log", domain: domain },
              // Plain-decimal tick labels: the default "~s" formatting turns
              // sub-1 ratios into SI-prefixed labels ("500m", "200m"), which
              // read as units rather than ratios. "~f" trims trailing zeros.
              axis: {
                values: logTicks(domain),
                format: "~f",
                grid: false,
              },
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
              scale: { type: "log", domain: domain },
              // Plain-decimal tick labels: the default "~s" formatting turns
              // sub-1 ratios into SI-prefixed labels ("500m", "200m"), which
              // read as units rather than ratios. "~f" trims trailing zeros.
              axis: {
                values: logTicks(domain),
                format: "~f",
                grid: false,
              },
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

  function renderInto(container, points) {
    if (!window.vegaEmbed) {
      return;
    }
    var vlSpec =
      container.dataset.kind === "cold"
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
