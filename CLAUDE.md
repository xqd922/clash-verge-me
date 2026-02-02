# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Build & Development Commands

```bash
# Install dependencies
pnpm install

# Download Clash Mihomo core binary (required before first run)
pnpm run check
pnpm run check --force  # Force update to latest version

# Development
pnpm dev                # Run with Tauri hot reload
pnpm dev:diff           # Use when an app instance already exists
pnpm web:dev            # Run frontend only (port 3000)

# Production build
pnpm run build          # Build complete application

# Linting/Formatting
npx prettier --write .  # Format code (2-space indent, no semicolons)
```

**Requirements**: pnpm 9.13.2+, Rust 2021 edition, Node.js. Windows builds must use x86_64-pc-windows-msvc toolchain.

## Architecture Overview

This is a **Tauri 2** desktop application with React frontend and Rust backend, using Clash Mihomo as an external sidecar binary.

```
Frontend (React 18 + TypeScript + MUI 6)
    ↓ IPC via invoke()
Backend (Rust + Tauri 2)
    ↓ Process management
Clash Mihomo Sidecar (External binary)
```

### Entry Points

- **Frontend**: `src/main.tsx` → `src/pages/_layout.tsx` (routing)
- **Backend**: `src-tauri/src/main.rs` → `src-tauri/src/lib.rs` (IPC handlers, plugins)

### Key Rust Modules (`src-tauri/src/`)

| Module     | Purpose                                               |
| ---------- | ----------------------------------------------------- |
| `cmds.rs`  | All Tauri IPC command handlers (~40 commands)         |
| `config/`  | Configuration management with Draft pattern           |
| `core/`    | Clash core lifecycle, tray, hotkeys, system services  |
| `enhance/` | Config merging pipeline (merge profiles, run scripts) |
| `feat.rs`  | Feature implementations (window state, timers)        |

### Frontend Structure (`src/`)

| Directory             | Purpose                                                                  |
| --------------------- | ------------------------------------------------------------------------ |
| `pages/`              | Route components (profiles, proxies, connections, logs, rules, settings) |
| `services/cmds.ts`    | All IPC invoke() calls to Rust backend                                   |
| `services/api.ts`     | Clash HTTP API client (axios)                                            |
| `services/types.d.ts` | TypeScript interfaces                                                    |
| `hooks/`              | SWR-based data hooks (use-clash.ts, use-profiles.ts)                     |
| `components/`         | Reusable UI (base, profile, proxy, setting)                              |

## IPC Communication

**Frontend → Backend**: Use `invoke()` from `@tauri-apps/api/core`

```typescript
// All IPC calls are in src/services/cmds.ts
export async function getProfiles() {
  return invoke<IProfilesConfig>("get_profiles");
}
```

**Backend → Frontend**: Tauri events via `Handle::refresh_clash()`, `Handle::notice_message()`

## Configuration System

### Config Types

- **Clash config** (`clash.rs`): Proxy ports, DNS, TUN settings
- **Verge config** (`verge.rs`): App settings (theme, hotkeys, auto-launch)
- **Profiles** (`profiles.rs`, `prfitem.rs`): User subscription configs

### Draft Pattern

```rust
Config::clash().latest()  // Read-only current config
Config::clash().draft()   // Mutable working copy for changes
```

### Profile Types

- `local` - YAML stored locally
- `remote` - Downloaded from URL
- `merge` - Config overlay
- `script` - JavaScript transformation

### Enhancement Pipeline (`enhance/mod.rs`)

```
Base config → Global Merge → Global Script → Profile Merge/Script → TUN config → Final YAML
```

## State Management

- **SWR** for remote data with auto-revalidation
- **React Context** (foxact) for global UI state
- Pattern: Call IPC → mutate SWR cache

```typescript
const { data, mutate } = useSWR("getRuntimeConfig", getRuntimeConfig);
await patchClashConfig(patch);
mutate();
```

## Adding Features

1. Add IPC command in `src-tauri/src/cmds.rs` with `#[tauri::command]`
2. Define TypeScript types in `src/services/types.d.ts`
3. Wrap IPC call in `src/services/cmds.ts`
4. Use in components via hooks

## Error Handling

**Rust macros**:

- `log_err!(op)` - Log errors without failing
- `wrap_err!(op)` - Wrap as String for IPC
- `ret_err!(op)` - Early return on error

**TypeScript**: Try-catch with `Notice.error()` for UI feedback

## Debugging

- Frontend devtools available in dev mode
- Backend: `RUST_BACKTRACE=1 pnpm dev`
- Generated config: Check `clash-verge.yaml` in app data directory
- Clash logs: View in Logs page
