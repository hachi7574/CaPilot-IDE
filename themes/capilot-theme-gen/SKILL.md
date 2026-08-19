---
name: capilot-theme-gen
description: Generate a CaPilot IDE visual theme cartridge (themes/<id>.json + i18n + optional CSS) from a user's mood, palette, hex colors, film, anime, or game character. Use when asked to make/add/design a CaPilot theme, 做一套主题, 新主题, or to turn a color/character/film into a settings theme.
argument-hint: "[mood / palette / hex / film / character]"
---

# capilot-theme-gen

Turn a user's visual brief into a complete CaPilot theme cartridge that
shows up in Settings → 主题 after a Vite reload.

This skill lives next to the cartridges it writes:
`themes/capilot-theme-gen/`. Read
[references/cartridge.md](references/cartridge.md) before writing any
JSON. Other paths below are relative to the **repo root**.

This folder is not a cartridge — Vite only globs `themes/*.json`.

## 1. Collect the brief — do not invent one

If `$ARGUMENTS` is non-empty, treat it as the brief and skip questions
already answered in it.

Otherwise **stop and ask**. Use `AskUserQuestion` when available; otherwise
ask in chat. Gather enough to lock a palette. One round is enough if the
answers are concrete; don't interview past that.

Ask for (all optional except that **at least one** color or reference
must exist):

| prompt | why |
|---|---|
| 印象 / 风格 | mood words: 冷、脏、赛博、和风、CRT、实验室、夜航… |
| 色系 | red/black/grey, sakura, ice, olive… |
| 色号 | hex like `#bc2939`. Honor these exactly. |
| 影视 / 动漫 / 游戏角色 | extract a real palette from costume, key art, lighting. |
| 明暗 | dark (default) vs light. |
| 壁纸 | reuse `themes/wallpapers/*`, a path they give, or none. |

If they named a film/character/game and no hex, look the palette up
(search / fetch official art or a well-known still). Quote 4–6 hexes
back before writing files so they can veto.

**Do not** start from "I'll just make a nice red theme." No brief → ask.

## 2. Lock the design (show it, then write)

State, in chat, before touching files:

```
id:        <kebab>
name:      <中文>
note:      <意象 · 意象>
scheme:    dark | light
swatches:  [bg, mid, brand, accent]
hero:      --brand  #rrggbb
accent:    --ai     #rrggbb   (the "small amount of X")
wallpaper: <file | none>
signature: <none | one-line CSS idea>
```

If a named color was given, that hex **is** `--brand` (or `--ai` if they
said it's the accent). Don't "improve" `#bc2939` to `#c0392b`.

Palette rules:

- One hero. Everything else is supporting.
- Neutrals (black/grey/paper) do the surface work. Don't rainbow the UI.
- `--ai` is the reserved "little bit of the other hue."
- `--success` / `--warn` / `--danger` stay semantically green / amber / red
  even on a red theme (shift danger brighter than brand so errors read).
- Contrast: `--ink` on `--bg`, `--accent-ink` on `--brand`, `--pl-fg` on
  `--term-bg`. If a pair would fail a glance test, darken/lighten the
  surface, not the hero.

## 3. Write the cartridge

Copy structure from the closest existing file in `themes/`:

- dark + hard chrome → `themes/cinnabar.json` or `themes/quantum.json`
- dark + neon → `themes/arcade.json`
- light + soft → `themes/bilibili.json`
- light + worn → `themes/handheld.json`

Checklist (fail the task if any miss):

- [ ] `themes/<id>.json` with every required var in cartridge.md
- [ ] **no** `--wallpaper-surface` / `--wallpaper-chrome` keys
- [ ] `termVeil` and `--term-veil` both `"0"` / `0` unless asked otherwise
- [ ] `--*-rgb` triples match their hex; `--scan-rgb` = `--brand-rgb`
- [ ] `--accent-ink` is the on-brand text color, not body ink
- [ ] `ui/i18n/zh.ts` and `ui/i18n/en.ts` gained `themes.<id>`
- [ ] `id` unique vs `themes/*.json`
- [ ] wallpaper basename exists under `themes/wallpapers/` if declared
- [ ] light themes that would inherit `text-shadow: 1px 1px 0 #000`
      get a thin `html[data-theme="<id>"]` override in `ui/App.css`
      (mirror bilibili: drop muddy shadows, caret/outline → brand)
- [ ] optional `.app::before` signature is `pointer-events: none` and
      does not paint shell fills

Pretty-print the JSON (cinnabar-style, one key per line). Don't minify
like amber/handheld unless you're editing those files.

## 4. Verify

```bash
# JSON parses and id matches filename
node -e "const t=require('./themes/<id>.json'); if(t.id!=='<id>') process.exit(1)"
```

On Windows PowerShell, equivalent:

```powershell
Get-ChildItem themes/*.json | ForEach-Object { $_.BaseName }
```

Confirm the new id is in that list and in both i18n files.

Do **not** run `pnpm tauri dev` unless the user asked to preview. Tell
them: Settings → 主题, pick the new name (Vite reload if dev is already
running). Fine-tune veil/mix in Theme Lab — don't hardcode a second
opacity in CSS.

## 5. Report

One short block:

- id / 中文名 / English name
- 4 swatches
- which existing theme you structured after
- wallpaper (or none)
- CSS signature (or none)
- anything you had to assume

## Anti-patterns

- Shipping a theme with no user color/reference input
- Hardcoding shell/composer/terminal alpha as `rgba(...)`
- A second hero color fighting `--brand`
- Reusing `quantum` / `cinnabar` / … as the new id
- Forgetting i18n (picker shows the key path)
- Inventing a wallpaper filename
- Light theme without killing the global black text-shadow
- Writing into `themes/capilot-theme-gen/` (this folder is the skill, not a theme)
