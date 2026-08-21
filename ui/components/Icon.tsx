// Themed icon set — source of truth is ui/assets/icons/*.svg (Lucide ISC /
// Simple Icons CC0). Vite's import.meta.glob loads them as raw strings at build
// time, so adding/removing an SVG is enough — no code generation step.
import type { CSSProperties } from 'react';

const rawIcons = import.meta.glob('../assets/icons/**/*.svg', {
  query: '?raw',
  import: 'default',
  eager: true,
}) as Record<string, string>;

/** Orca's bundled agent favicons (PNG). Used when a runtime has no SVG glyph. */
const rawFavicons = import.meta.glob('../assets/icons/agent-favicons/*.png', {
  query: '?url',
  import: 'default',
  eager: true,
}) as Record<string, string>;

const FAVICONS: Record<string, string> = Object.fromEntries(
  Object.entries(rawFavicons).map(([path, url]) => {
    const name = (path.split('/').pop() ?? '').replace(/\.png(\?.*)?$/, '');
    return [name, url];
  }),
);

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
  const cls = className ? `capilot-icon ${className}` : 'capilot-icon';
  const def = ICONS[name];
  if (def) {
    const fill = def.fill;
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
  const fav = FAVICONS[name];
  if (fav) {
    const px = typeof size === 'number' ? size : undefined;
    return (
      <img
        src={fav}
        alt=""
        width={px}
        height={px}
        className={cls}
        aria-hidden="true"
        style={{
          width: size,
          height: size,
          objectFit: 'contain',
          ...style,
        }}
      />
    );
  }
  return null;
}

/** Aliases from CaPilot / Orca runtime ids onto an icon filename (no extension).
 *  `Icon` prefers SVG glyphs; PNG favicons fill in the rest. */
const RUNTIME_ICON_ALIAS: Record<string, string> = {
  shell: 'terminal',
  cmd: 'terminal',
  powershell: 'terminal',
  pwsh: 'terminal',
  claude: 'claude',
  'claude-agent-teams': 'claude',
  codex: 'openai',
  dsh: 'deepseek',
  opencode: 'opencode',
  pi: 'pi',
  codebuddy: 'codebuddy',
  copilot: 'copilot',
  kilo: 'kilo',
  kilocode: 'kilo',
  auggie: 'aug',
  augment: 'aug',
  qwen: 'qwen-code',
  'qwen-code': 'qwen-code',
  'mimo': 'mimo-code',
  vibe: 'mistral-vibe',
  crush: 'crush',
  charm: 'crush',
  kiro: 'kiro',
  cursor: 'cursor',
  continue: 'continue',
  trae: 'trae',
  hermes: 'hermes',
  grok: 'grok',
  kimi: 'kimi',
  gemini: 'gemini',
  goose: 'goose',
  amp: 'amp',
  cline: 'cline',
  codebuff: 'codebuff',
  'command-code': 'command-code',
  droid: 'droid',
  openclaude: 'openclaude',
  autohand: 'autohand',
  'mimo-code': 'mimo-code',
  rovo: 'rovo',
  openclaw: 'openclaw',
  devin: 'devin',
  ante: 'ante',
  'prime-agent': 'prime-agent',
  antigravity: 'antigravity',
};

/** Icon name per agent runtime (brand marks, not the robot emoji). */
export function runtimeIcon(runtime: string): string {
  if (runtime.startsWith('bash')) return 'gnubash';
  const aliased = RUNTIME_ICON_ALIAS[runtime];
  if (aliased) return aliased;
  if (FAVICONS[runtime] || ICONS[runtime]) return runtime;
  return 'terminal';
}
