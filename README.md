# Herdr WebView Adapter

Standalone Rust implementation of the embedded browser feature for [Herdr WebView](https://github.com/TonyPythoneer/herdr-webview).

This repository owns the custom browser features and their versioned host integration surface. The upstream-derived Herdr application consumes a pinned adapter revision rather than maintaining duplicate browser logic.

## Scope

- WKWebView and Chrome CDP backends
- Browser session lifecycle, pane-scoped operations and viewport updates
- Agent and MCP browser automation, screenshots and cancellation
- Security policies and explicit human approval integration
- Herdr Desktop host bindings and compatibility tests

**Integration contract:** Herdr Tab/Pane persistence and trusted native approval UI remain owned by the host; adapter code must not bypass user permission gates.

The initial source migration targets the existing `feature/native-browser-tabs` baseline of `herdr-webview`. Until integration and shipped-app E2E checks are green, this repository is not a production release.

## Automated upstream compatibility

The scheduled `adapter-upstream-compat` workflow checks the official stable upstream tag every Monday. Changes create a pinned, auditable PR; missing host API anchors or a `gpui-pre` mismatch **fail Adapter CI closed**. No arbitrary upstream code is executed. The scan is a preflight, never a replacement for Host dual-browser and native-approval E2E. The workflow explicitly dispatches CI for GITHUB_TOKEN-created PRs.
