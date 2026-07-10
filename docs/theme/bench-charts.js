// Renders the benchmark grouped bar chart on the Performance page with Vega-Lite.
//
// Data is injected by the `doc-utils` mdbook preprocessor as an inline
// `<script type="application/json" class="bench-data">` next to a
// `<div class="bench-chart">` (see docs/doc-utils/src/lib.rs). The Vega runtime
// is vendored under theme/vendor/ and loaded before this file via book.toml's
// `additional-js`, so nothing is fetched at view time.
//
// Chart: x = scenario, one bar per tool (xOffset + color), y = throughput in
// MB/s (linear, higher is faster), with a hover tooltip.
(() => {
	// mdBook keeps the active theme as a class on <html>; these three are dark.
	function isDark() {
		var c = document.documentElement.classList;
		return c.contains("coal") || c.contains("navy") || c.contains("ayu");
	}

	// Unique values in first-appearance (corpus / results) order, so the axis and
	// legend read single -> project and Fatou -> Runic -> JuliaFormatter rather
	// than alphabetized.
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

	function spec(points) {
		var dark = isDark();
		var fg = dark ? "#c8c9db" : "#333333";
		var grid = dark ? "#3b3f5c" : "#dddddd";
		var scenarios = orderedUnique(points, "scenario");
		var tools = orderedUnique(points, "tool");

		return {
			$schema: "https://vega.github.io/schema/vega-lite/v5.json",
			description:
				"Grouped bar chart of formatting throughput in megabytes per second, " +
				"grouped by scenario with one bar per tool; taller bars are faster. " +
				"Runic is absent from the project scenario because it has no in-process " +
				"directory API. See the data table for the underlying numbers.",
			width: "container",
			height: 340,
			data: { values: points },
			mark: { type: "bar" },
			encoding: {
				x: {
					field: "scenario",
					type: "nominal",
					title: "Scenario",
					sort: scenarios,
					axis: { labelAngle: 0 },
				},
				xOffset: { field: "tool", type: "nominal", sort: tools },
				y: {
					field: "throughput_mbps",
					type: "quantitative",
					title: "Throughput (MB/s)",
				},
				color: {
					field: "tool",
					type: "nominal",
					title: "Tool",
					sort: tools,
				},
				tooltip: [
					{ field: "scenario", title: "Scenario" },
					{ field: "tool", title: "Tool" },
					{
						field: "throughput_mbps",
						title: "Throughput (MB/s)",
						format: ".2f",
					},
					{ field: "relative", title: "Relative to Fatou" },
					{ field: "median_ms", title: "Median (ms)", format: ".1f" },
					{ field: "files_ok", title: "Files" },
					{ field: "total_bytes", title: "Bytes", format: "," },
				],
			},
			config: {
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
			},
		};
	}

	function renderInto(container, points) {
		if (!window.vegaEmbed) {
			return;
		}
		var vlSpec = spec(points);
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
