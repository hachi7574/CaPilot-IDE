// Themed icon set — source of truth is ui/assets/icons/*.svg (Lucide ISC /
// Simple Icons CC0). Vite's import.meta.glob loads them as raw strings at build
// time, so adding/removing an SVG is enough — no code generation step.
import type { CSSProperties } from 'react';

const rawIcons = import.meta.glob('../assets/icons/**/*.svg', {
  query: '?raw',
  import: 'default',
  eager: true,
}) as Record<string, string>;

type IconDef = { inner: string; fill: boolean };

/** Inner markup of a source SVG: drop the <svg> wrapper, comments, and <title>. */
function svgInner(src: string): string {
  return src
    .replace(/<!--[\s\S]*?-->/g, '')
    .replace(/^<svg[^>]*>/i, '')
    .replace(/<\/svg>\s*$/i, '')
    .replace(/<title>[\s\S]*?<\/title>/gi, '')
    .trim();
}

// name -> inner SVG markup (trusted static paths from Lucide ISC / Simple Icons CC0).
const ICONS: Record<string, IconDef> = Object.fromEntries(
  Object.entries(rawIcons).map(([path, src]) => {
    const name = (path.split('/').pop() ?? '').replace(/\.svg(\?.*)?$/, '');
    // brands/ + git.svg are Simple Icons marks (fill); everything else strokes.
    const fill = path.includes('/brands/') || name === 'git';
    return [name, { inner: svgInner(src), fill }];
  }),
);

export interface IconProps {
  name: string;
  size?: number | string;
  className?: string;
  style?: CSSProperties;
}

/**
 * Themed icon. Stroke icons follow var(--icon-color, currentColor);
 * brand/fill icons fill with var(--icon-color, currentColor).
 */
export function Icon({ name, size = 16, className, style }: IconProps) {
  const def = ICONS[name];
  if (!def) return null;
  const fill = def.fill;
  const cls = className ? `capilot-icon ${className}` : 'capilot-icon';
  const base: CSSProperties = fill
    ? { fill: 'var(--icon-color, currentColor)' }
    : { stroke: 'var(--icon-color, currentColor)' };
  return (
    <svg
      xmlns="http://www.w3.org/2000/svg"
      className={cls}
      width={size}
      height={size}
      viewBox="0 0 24 24"
      role="img"
      aria-hidden="true"
      fill={fill ? "currentColor" : "none"}
      stroke={fill ? undefined : "currentColor"}
      strokeWidth={fill ? undefined : 2}
      strokeLinecap={fill ? undefined : "round"}
      strokeLinejoin={fill ? undefined : "round"}
      style={fill ? { ...base, ...style } : { ...base, ...style }}
      dangerouslySetInnerHTML={{ __html: def.inner }}
    />
  );
}

/** Icon name per agent runtime (brand marks, not the robot emoji). */
export function runtimeIcon(runtime: string): string {
  if (runtime === 'shell' || runtime === 'cmd' || runtime === 'powershell') return 'terminal';
  if (runtime.startsWith('bash')) return 'gnubash';
  switch (runtime) {
    case 'claude': return 'claude';
    case 'codex': return 'openai';
    case 'dsh': return 'deepseek';
    case 'opencode': return 'opencode';
    case 'pi': return 'pi';
    default: return 'terminal';
  }
}
