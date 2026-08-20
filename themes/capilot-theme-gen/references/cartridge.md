# CaPilot theme cartridge

A theme is one JSON file `themes/<id>.json`, discovered by
`ui/state/themes.ts` via `import.meta.glob("../../themes/*.json")`.
Settings shows it after Vite reload. Display names live in i18n, not
only in the JSON.

`themes/capilot-theme-gen/` is this generator skill, not a cartridge.

## Identity

| field | rule |
|---|---|
| `id` | `^[a-z0-9][a-z0-9_-]*$`, 1–64 chars. Must be unique vs existing `themes/*.json`. |
| `name` | Short Chinese display name (2–6 汉字 typical). Fallback if i18n missing. |
| `note` | `意象A · 意象B` — two short images, same cadence as existing themes. |
| `swatches` | Exactly 4 hex colors: `[bg, mid-surface, brand, accent]`. Used only in the Settings picker chips. |
| `colorScheme` | `"dark"` or `"light"`. |
| `termVeil` | Number 0–1. **Default 0** (terminal cells transparent so wallpaper shows). Do not ship 1 unless the user asked for an opaque terminal. |
| `wallpaper` | Optional `{ "file": "<basename under themes/wallpapers/>" }`. Also accepts `opacity` (0–1, default 0.55), `size` (`cover`/`contain`), `position`. |

Existing ids (do not reuse): `amber`, `arcade`, `bilibili`, `blue-whale`,
`cinnabar`, `field`, `glacier`, `handheld`, `miku`, `quantum`.

## Required `vars`

Every key below must be present. Values are CSS (hex / `r g b` triples /
`Npx` / shadow strings). Ratios are unitless `0`–`1` strings.

### Layers (ratios only — never the mixed result)

```
--term-veil                  "0"
--wallpaper-surface-mix      "0.72"
--wallpaper-chrome-mix       "0.78"
```

**Forbidden in JSON:** `--wallpaper-surface`, `--wallpaper-chrome`.
Hydrate deletes them. Alpha is owned by `ui/App.css` under
`html[data-wallpaper="on"]`. To retint the mix, set
`--wallpaper-surface-base` / `--wallpaper-chrome-base` (usually
`var(--bg2)` / `var(--bg3)`) — only if a light theme needs a warmer
chrome, as bilibili does in CSS, not as hardcoded rgba.

### Surfaces

```
--bg --bg2 --bg3 --bg4       four-step ramp
--term-bg                    terminal / composer / empty-state fill
--rule --rule2               hairline, then stronger rule
--search-match-bg            editor/terminal search hit
```

Dark: `--bg` darkest → `--bg4` lightest raised surface.
Light: `--bg` page, `--bg2` raised, `--bg3`/`--bg4` can go darker
(handheld) or more saturated (bilibili).

### Ink

```
--ink          primary text, high contrast on --bg
--ink2         secondary
--muted        tertiary / comments
--accent-ink   text ON --brand / --brand-dim (buttons, active rows)
```

`--accent-ink` is not body text. Dark themes: near-white tinted toward
brand. Light themes: a pale wash of the page (handheld `#D7DCAF` on
olive brand; bilibili `#f6eeee` on pink brand).

### Brand / status

```
--brand --brand-dim
--primary --primary-dim      companion accent (often cooler or brighter)
--ai --ai-dim                agent / AI chrome — keep this the "few blue"
--success --warn --danger
```

`--brand` is the hero. `--primary` is a sibling, not a second hero.
`--ai` is the reserved accent (blue / teal / whatever the brief's
"small amount of X" is). Status colors must stay readable as status —
don't recast success as the brand red.

### Derived companions (keep in sync with the hex they come from)

```
--brand-rgb          "R G B"          from --brand
--success-rgb        "R G B"
--warn-rgb           "R G B"
--danger-rgb         "R G B"
--white-rgb          "R G B"          page highlight / ink-white
--black-rgb          "R G B"          deepest shadow, not always #000
--brand-selection    "rgb(R G B / 0.22–0.36)"
--scan-rgb           same triple as --brand
--scan-alpha         dark ~0.02 ; light ~0.05
```

Theme Lab's `derivedVarsFor` rewrites `--brand-rgb`, `--brand-selection`,
`--scan-rgb` when `--brand` changes. Ship them already consistent.

### Terminal ANSI (`--pl-*`)

Foreground / black / red / green / yellow / blue / magenta / cyan /
white / orange / purple / blue-purple / comment / cursor / selection,
plus the `bright-*` set.

- `--pl-fg` ≈ body ink, slightly softer.
- `--pl-black` ≈ `--term-bg`.
- `--pl-cursor` = `--brand` (or `--primary` if the brief wants a
  cooler caret).
- `--pl-selection` = a mid surface stained with brand, not a neon wash.
- `--pl-blue` may equal `--ai`. `--pl-red` may echo brand on a red
  theme — then push `--danger` a step brighter so errors still pop.
- Brights are the same hues, lifted. Don't invent a second palette.

### Lanes `--lane-0` … `--lane-6`

Git / chart series. `--lane-0` = brand. Spread the rest across
primary, ai, status, and two neutrals. All must be distinct on `--bg`.

### Shape

```
--control-radius     "0px" | "1px" | "2px" | "4px"
--panel-radius       same family, ≥ control
--shadow-hard        "4px 4px 0 <deepest>"
--control-shadow     inset highlight + hard drop
--panel-shadow       1px rule + hard drop; optional brand glow ≤ 0.10
```

Default dark chrome (quantum / cinnabar):

```
--control-radius: 1px
--panel-radius: 1px
--control-shadow: inset 0 1px 0 rgb(<white-rgb> / 0.06–0.10), 2px 2px 0 <black>
--panel-shadow: 0 0 0 1px <black>, 4px 4px 0 <black>, 0 0 18px rgb(<brand-rgb> / 0.08)
--shadow-hard: 4px 4px 0 <black>
```

Light chrome (bilibili-like): softer inset white, brand-tinted drop,
slightly larger radius (2–4px). Handheld-like: `0px`, olive/ink hard
shadow, no glow.

## i18n (required)

`ui/i18n/zh.ts` → `themes.<id> = { name, note }`
`ui/i18n/en.ts` → same keys, English name + note.

`themeLabel()` looks up `themes.${id}.name`. Missing key falls back to
the JSON `name`, but the picker is bilingual — always add both.

## Optional CSS signature

Only if the brief wants a recognizable texture (quantum's 24px lattice).
Keep it thin:

- Anchor to `html[data-theme="<id>"] .app::before` (not `body`) so
  resident runtimes can stack above it. `pointer-events: none`.
- Light themes that inherit muddy `text-shadow: 1px 1px 0 #000` should
  copy the bilibili overrides (drop text-shadow on chrome labels,
  lighter modal scrim, caret/outline → brand).
- Never paint `--wallpaper-surface` / `--wallpaper-chrome` here.

## Wallpaper files

- Live in `themes/wallpapers/`. Basename only in JSON (`"远.jpg"`).
- Reuse an existing file if it matches. Do not invent a filename that
  isn't on disk.
- If the user supplies an image path, copy it into `themes/wallpapers/`
  with a safe basename and point `wallpaper.file` at that basename.
- **Shipped videos must be H.264 / `avc1`, yuv420p.** Prefer
  `themes/wallpapers/to-720p.sh <src>` → `<stem>-720p.mp4` (≤720p, no
  audio, faststart). Do **not** bundle HEVC (`hvc1`/`hev1`) or AV1 for
  theme defaults — Linux WebKitGTK often lacks those decoders even when
  H.264 works via `gstreamer1.0-libav`.
- Missing file → console warning, theme still loads without art.

## Hydrate behavior (do not fight it)

`hydrateTheme` in `ui/state/themes.ts`:

1. Writes `--term-veil` from top-level `termVeil`, else vars, else `0`.
2. Deletes `--wallpaper-surface` / `--wallpaper-chrome`.
3. Fills missing mix ratios with `0.72` / `0.78`.

## Closest existing palettes (steal structure, not hues)

| id | scheme | hero | accent | notes |
|---|---|---|---|---|
| quantum | dark | violet `#9A86FF` | cyan `#58C7FF` | default theme; grid overlay |
| cinnabar | dark | red `#BC2939` | steel `#4E7FA6` | red/black/grey + little blue |
| blue-whale | dark | blue `#4B8BFF` | mint `#6EE7C5` | termVeil 0 already |
| glacier | dark | ice `#7CCAF2` | lilac `#AEB4FF` | glass, tight 2px radius |
| arcade | dark | magenta `#F06AC7` | teal `#55E6D2` | neon, brand glow |
| amber | dark | amber `#F2B84B` | teal `#7ED0C7` | CRT, 0 radius |
| field | dark | teal `#63D9C4` | gold `#F0B65B` | phosphor |
| bilibili | light | pink `#FB7299` | sky `#00AEEC` | CSS chrome overrides |
| handheld | light | olive `#29351F` | rust `#9B4F36` | LCD, 0 radius |
| miku | dark | teal `#39C5BB` | pink `#FF3488` | no wallpaper |
