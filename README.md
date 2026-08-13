# CaPilot IDE

> Local AI coding workspace — Tauri v2 + React + CodeMirror 6

CaPilot IDE is a lightweight desktop workspace for running AI coding CLIs in real PTY terminals, editing files, and using Git.

## Tech Stack

- **Desktop Shell:** Tauri v2 (Rust + system WebView)
- **Frontend:** React 19 + TypeScript + Vite
- **Editor:** CodeMirror 6
- **Terminal:** xterm.js
- **State:** zustand

## Development

### Prerequisites

- Rust 1.97+ (`rustup`)
- Node.js 24+
- pnpm
- Linux: `libwebkit2gtk-4.1-dev librsvg2-dev libgtk-3-dev libsoup-3.0-dev libjavascriptcoregtk-4.1-dev`

### Quick Start

```bash
pnpm install
pnpm tauri dev
pnpm tauri build
```

## Project Structure

```text
CaPilot-Ide/
├── src-tauri/         # Rust core and Tauri configuration
├── ui/                # React frontend
├── public/            # Static assets
├── docs/              # Documentation and design assets
└── package.json
```

## License

MIT
