const PAGE_PATH = /^\/$|^\/.*\.html$|^\/.*\/$/;
const HOMEPAGE_LINK = '</index.md>; rel="describedby"; type="text/markdown"';

export function acceptsMarkdown(accept) {
  return (
    accept?.split(",").some((range) => {
      const [type, ...parameters] = range.trim().toLowerCase().split(";");
      if (type.trim() !== "text/markdown") return false;
      return !parameters.some((parameter) =>
        /^q\s*=\s*0(?:\.0*)?$/.test(parameter.trim()),
      );
    }) ?? false
  );
}

function varyAccept(headers) {
  const vary = headers.get("Vary");
  if (
    !vary?.split(",").some((value) => value.trim().toLowerCase() === "accept")
  ) {
    headers.set("Vary", vary ? `${vary}, Accept` : "Accept");
  }
}

function pageHeaders(response, pathname) {
  const headers = new Headers(response.headers);
  varyAccept(headers);
  if (pathname === "/" && response.ok) headers.append("Link", HOMEPAGE_LINK);
  return headers;
}

export default {
  async fetch(request) {
    const url = new URL(request.url);
    const wantsMarkdown =
      (request.method === "GET" || request.method === "HEAD") &&
      PAGE_PATH.test(url.pathname) &&
      acceptsMarkdown(request.headers.get("Accept"));

    if (wantsMarkdown) {
      const markdownUrl = new URL(url);
      markdownUrl.pathname = url.pathname.endsWith("/")
        ? `${url.pathname}index.md`
        : url.pathname.replace(/\.html$/, ".md");
      const markdownRequest = new Request(markdownUrl, request);
      markdownRequest.headers.delete("Accept");
      const markdown = await fetch(markdownRequest);
      if (markdown.ok) {
        const headers = pageHeaders(markdown, url.pathname);
        headers.set("Content-Type", "text/markdown; charset=utf-8");
        return new Response(markdown.body, {
          status: markdown.status,
          headers,
        });
      }
    }

    const html = await fetch(request);
    if (!PAGE_PATH.test(url.pathname)) return html;
    const headers = pageHeaders(html, url.pathname);
    return new Response(html.body, { status: html.status, headers });
  },
};
