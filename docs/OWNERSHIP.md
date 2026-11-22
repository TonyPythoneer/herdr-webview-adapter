# Adapter ownership, compatibility and migration

**This is the authoritative source of custom browser implementation for Herdr WebView.**

Baseline from `TonyPythoneer/herdr-webview` at `feature/native-browser-tabs` / `9668c46a740f4e58d8c0cb6a8bbec42dfbd952b8`.

| Adapter package implementation | Herdr Desktop integration |
| --- | --- |
| URL security, engine precedence and persisted pane metadata rules | Original Herdr Tab/Pane state, layout, persistence |
| Private Chrome CDP pipe, profile, sessions, screencast, input | Native AppWindow parenting, Z-order and launch lifecycle |
| WKWebView native input, WebKit snapshots and Agent operations | Approval/denial GUI, permission decision authority |
| Agent command execution, bounded screenshot output | UI command dispatcher, in-app state/event ownership |
| MCP stdio JSON-RPC protocol and socket transport | Host command descriptions/schema source + validation |
| Browser unit tests | Packaged desktop E2E, release CI, App updater |

## Required host bridge

- Existing `src/browser.rs::create` (GPUI Context<AppWindow>) remains a small host-specific native child factory; URL policy is adapter-owned.
- Existing `src/browser_mcp.rs` becomes a thin bridge that supplies `commands::COMMANDS` and delegates JSON-RPC handling to `adapter::browser_mcp::serve_stdio`, socket forwarding to `adapter::browser_mcp::forward_socket`.
- Host imports `ChromeView`, `BrowserEngine`, Chrome sessions and WK agents from the adapter; host does not keep a duplicate implementation.
- `src/window.rs` still acts as the native Host event router, but the engine-specific implementation is in this crate.
- Browser process directory resolution must match upstream Herdr's home, including `HERDR_APP_HOME` and `XDG_CONFIG_HOME`.

## Non-negotiable compatibility

- Reserved `Content::Mod` browser marker `__herdr_native_browser` stays stable.
- Historical panes missing an engine always restore WKWebView; newly opened auto engine prefers installed Chrome where usable.
- Explicit engine never silently falls back.
- Chrome profile remains private and separate; no remote debugging TCP port or Chrome auto-install.
- No self-approval via CLI/MCP/page JavaScript; consequential actions require real native consent.
- No host data migration and no stable release before both engines pass shipped-app E2E and full exact-SHA CI.

## Release and rollback

The consuming app must pin this repository to a **full immutable SHA** in Cargo.toml, regenerate Cargo.lock and keep the old SHA available to revert. Published App releases bundle the compiled adapter; the app must not dynamically download or execute adapter versions separately. `Issue #1` in the host repository retains the final acceptance/release gate. All upstream sync changes must validate the host integration layer against this adapter before deployment.

Status: repository-first port in progress. A copied/moved module is not considered complete until this repository's native tests and the downstream packaged-app tests pass.
