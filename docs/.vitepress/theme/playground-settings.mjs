const defaults = { lineWidth: 92, indentWidth: 4 };

function width(value, fallback) {
  if (!/^\d+$/.test(value ?? "")) return fallback;
  const number = Number(value);
  return number >= 1 && number <= 1000 ? number : fallback;
}

export function readSettings(search) {
  const params = new URLSearchParams(search);
  return {
    lineWidth: width(params.get("lw"), defaults.lineWidth),
    indentWidth: width(params.get("iw"), defaults.indentWidth),
  };
}

export function settingsUrl(location, settings) {
  const url = new URL(location);
  url.search = new URLSearchParams({
    lw: settings.lineWidth,
    iw: settings.indentWidth,
  }).toString();
  url.hash = "";
  return url;
}
