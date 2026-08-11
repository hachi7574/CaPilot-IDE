#!/usr/bin/env python3
"""Regenerate ui/components/Icon.tsx from docs/Assets/Icons/*.svg.

Each Lucide stroke icon becomes a themed inline <svg> whose stroke follows
`var(--icon-color, currentColor)`; Simple Icons brand marks (brands/) and
git.svg fill with the same variable. Run after adding/removing icons:

    python3 scripts/gen_icon_component.py
"""
import os
import re
import json

ROOT = os.path.join(os.path.dirname(__file__), "..", "docs", "Assets", "Icons")
OUT = os.path.join(os.path.dirname(__file__), "..", "ui", "components", "Icon.tsx")
BRAND_DIR = "brands"


def extract_inner(svg_text: str) -> str:
    m = re.search(r"<svg[^>]*>(.*?)</svg>", svg_text, re.S)
    inner = m.group(1) if m else ""
    inner = re.sub(r"<title>.*?</title>", "", inner, flags=re.S)
    return re.sub(r"\n\s*\n", "\n", inner).strip()


def main() -> None:
    entries: dict[str, dict] = {}
    for fn in sorted(os.listdir(ROOT)):
        if not fn.endswith(".svg"):
            continue
        name = fn[:-4]
        text = open(os.path.join(ROOT, fn), encoding="utf-8").read()
        entries[name] = {"inner": extract_inner(text), "fill": fn == "git.svg"}
    for fn in sorted(os.listdir(os.path.join(ROOT, BRAND_DIR))):
        if not fn.endswith(".svg"):
            continue
        name = fn[:-4]
        text = open(os.path.join(ROOT, BRAND_DIR, fn), encoding="utf-8").read()
        entries[name] = {"inner": extract_inner(text), "fill": True}

    fill_names = sorted(n for n, e in entries.items() if e["fill"])
    lines = [
        "// GENERATED FILE - do not edit by hand. Source: docs/Assets/Icons/*.svg",
        "// Regenerate: python3 scripts/gen_icon_component.py",
        "import type { CSSProperties } from 'react';",
        "",
        "type IconDef = { inner: string; fill: boolean };",
        "",
        "// name -> inner SVG markup (trusted static paths from Lucide ISC / Simple Icons CC0).",
        "const ICONS: Record<string, IconDef> = {",
    ]
    for name, e in sorted(entries.items()):
        esc = e["inner"].replace("\\", "\\\\").replace("`", "\\`").replace("${", "\\${")
        lines.append(f'  {json.dumps(name)}: {{ inner: `{esc}`, fill: {"true" if e["fill"] else "false"} }},')
    lines += [
        "};",
        "",
        "export interface IconProps {",
        "  name: string;",
        "  size?: number | string;",
        "  className?: string;",
        "  style?: CSSProperties;",
        "}",
        "",
        "/**",
        " * Themed icon. Stroke icons follow var(--icon-color, currentColor);",
        " * brand/fill icons fill with var(--icon-color, currentColor).",
        " */",
        "export function Icon({ name, size = 16, className, style }: IconProps) {",
        "  const def = ICONS[name];",
        "  if (!def) return null;",
        "  const fill = def.fill;",
        "  const cls = className ? `capilot-icon ${className}` : 'capilot-icon';",
        "  const base: CSSProperties = fill",
        "    ? { fill: 'var(--icon-color, currentColor)' }",
        "    : { stroke: 'var(--icon-color, currentColor)' };",
        "  return (",
        "    <svg",
        '      xmlns="http://www.w3.org/2000/svg"',
        "      className={cls}",
        "      width={size}",
        "      height={size}",
        '      viewBox="0 0 24 24"',
        '      role="img"',
        '      aria-hidden="true"',
        '      fill={fill ? "currentColor" : "none"}',
        '      stroke={fill ? undefined : "currentColor"}',
        "      strokeWidth={fill ? undefined : 2}",
        '      strokeLinecap={fill ? undefined : "round"}',
        '      strokeLinejoin={fill ? undefined : "round"}',
        "      style={fill ? { ...base, ...style } : { ...base, ...style }}",
        "      dangerouslySetInnerHTML={{ __html: def.inner }}",
        "    />",
        "  );",
        "}",
        "",
        "/** Icon name per agent runtime (brand marks, not the robot emoji). */",
        "export function runtimeIcon(runtime: string): string {",
        "  if (runtime.startsWith('bash')) return 'gnubash';",
        "  switch (runtime) {",
        "    case 'claude': return 'claude';",
        "    case 'codex': return 'openai';",
        "    case 'opencode': return 'opencode';",
        "    default: return 'terminal';",
        "  }",
        "}",
        "",
    ]
    open(OUT, "w", encoding="utf-8").write("\n".join(lines))
    print(f"wrote {OUT}: {len(entries)} icons, {len(fill_names)} fill icons")


if __name__ == "__main__":
    main()
