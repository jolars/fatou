#!/usr/bin/env python3
"""Remove browser-only benchmark charts from mdBook's Markdown output."""

from html import unescape
from pathlib import Path
import re
import sys


def inline_text(html: str) -> str:
    html = re.sub(r"<code>(.*?)</code>", lambda match: f"`{match[1]}`", html)
    return unescape(re.sub(r"<[^>]+>", "", html)).strip()


def table_to_markdown(match: re.Match[str]) -> str:
    html = match[0]
    caption = re.search(r"<caption>(.*?)</caption>", html, re.S)
    rows = []
    for row in re.findall(r"<tr>(.*?)</tr>", html, re.S):
        cells = re.findall(r"<(?:td|th)>(.*?)</(?:td|th)>", row, re.S)
        rows.append("| " + " | ".join(inline_text(cell) for cell in cells) + " |")
    if not rows:
        return ""
    rows.insert(1, "| " + " | ".join("---" for _ in rows[0].split("|")[1:-1]) + " |")
    prefix = f"**{inline_text(caption[1])}**\n\n" if caption else ""
    return prefix + "\n".join(rows) + "\n"


def clean_chart(match: re.Match[str]) -> str:
    block = match[1]
    block = re.sub(r"<figure\b.*?</figure>\s*", "", block, flags=re.S)
    block = re.sub(r"<noscript>.*?</noscript>\s*", "", block, flags=re.S)
    block = re.sub(r"<summary>.*?</summary>\s*", "", block, flags=re.S)
    block = re.sub(r"</?details[^>]*>", "", block)
    block = re.sub(r"<h4>(.*?)</h4>", lambda m: f"#### {inline_text(m[1])}\n", block)
    block = re.sub(r"<table>.*?</table>", table_to_markdown, block, flags=re.S)
    block = re.sub(r"<p[^>]*>(.*?)</p>", lambda m: inline_text(m[1]) + "\n", block, flags=re.S)
    return "\n" + block.strip() + "\n"


def main() -> None:
    root = Path(sys.argv[1])
    performance = root / "guide/performance.md"
    source = performance.read_text()
    cleaned, count = re.subn(
        r'<div class="bench-chart-block">\n(.*?)\n</div>', clean_chart, source, flags=re.S
    )
    if count == 0 or 'class="bench-chart-block"' in cleaned:
        raise SystemExit("failed to remove benchmark charts from Markdown")
    performance.write_text(cleaned)

    introduction = root / "introduction.md"
    source = introduction.read_text()
    cleaned = re.sub(
        r'<img src="([^"]+)" alt="([^"]+)"[^>]*/>',
        lambda match: f"![{match[2]}]({match[1]})",
        source,
    )
    introduction.write_text(cleaned)


if __name__ == "__main__":
    main()
