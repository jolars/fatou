import { redirects } from "./redirects.mjs";

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
  async fetch(request, env) {
    const url = new URL(request.url);
    if (Object.hasOwn(redirects, url.pathname)) {
      url.pathname = redirects[url.pathname];
      return Response.redirect(url.href, 301);
    }
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
      // HTML validators and ranges describe a different representation.
      for (const header of [
        "Accept",
        "If-None-Match",
        "If-Modified-Since",
        "Range",
        "If-Range",
      ]) {
        markdownRequest.headers.delete(header);
      }
      const markdown = await env.ASSETS.fetch(markdownRequest);
      if (markdown.ok) {
        const headers = pageHeaders(markdown, url.pathname);
        headers.set("Content-Type", "text/markdown; charset=utf-8");
        return new Response(markdown.body, {
          status: markdown.status,
          headers,
        });
      }
    }

    // Explicit HTML handling preserves published .html URLs, including the root.
    const assetUrl = new URL(url);
    if (assetUrl.pathname === "/") assetUrl.pathname = "/index.html";
    const html = await env.ASSETS.fetch(new Request(assetUrl, request));
    if (url.pathname.endsWith(".md") && html.ok) {
      const headers = new Headers(html.headers);
      headers.set("Content-Type", "text/markdown; charset=utf-8");
      return new Response(html.body, { status: html.status, headers });
    }
    if (!PAGE_PATH.test(url.pathname)) return html;
    const headers = pageHeaders(html, url.pathname);
    return new Response(html.body, { status: html.status, headers });
  },
};
