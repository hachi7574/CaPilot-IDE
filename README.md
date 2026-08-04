# CaPilot IDE

> AI Agent Orchestration Workbench — Tauri v2 + React + CodeMirror 6

CaPilot IDE is the desktop application for [CaPilot](https://github.com/hachi7574/CaPilot), an AI agent orchestration platform. Built with Tauri v2 for a lightweight, native desktop experience.

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
# Install dependencies
pnpm install

# Run in development mode
pnpm tauri dev

# Build for production
pnpm tauri build
```

## Project Structure

```
CaPilot-Ide/
├── src-tauri/         # Rust core (Tauri backend)
│   ├── src/           # lib.rs, main.rs
│   ├── capabilities/  # Tauri v2 permissions
│   ├── icons/         # App icons
│   └── tauri.conf.json
├── ui/                # React frontend
│   ├── App.tsx
│   └── main.tsx
├── public/            # Static assets (logos, etc.)
├── docs/              # Documentation
└── package.json
```

## License

MIT
