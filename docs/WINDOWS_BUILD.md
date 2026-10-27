# Building for Windows

## Option A — GitHub Actions (recommended)

The `.github/workflows/release.yml` workflow builds for Windows automatically on every `v*` tag push.

```bash
git tag v0.2.1
git push origin v0.2.1
```

Then go to the GitHub Actions tab → the Release workflow produces:
- `.msi` (Windows Installer, recommended)
- `.exe` (NSIS setup)

These are attached to the GitHub Release draft automatically.

## Option B — Build on a Windows machine

1. Install prerequisites:
   - [Rust](https://rustup.rs/) (stable)
   - [Node.js 20+](https://nodejs.org/)
   - [Microsoft C++ Build Tools](https://visualstudio.microsoft.com/visual-cpp-build-tools/) (or Visual Studio)
   - [WebView2 Runtime](https://developer.microsoft.com/en-us/microsoft-edge/webview2/) (included in Windows 11, optional installer for Windows 10)

2. Clone the repo and run:
   ```powershell
   cd src-frontend && npm ci && cd ..
   cargo tauri build
   ```

3. Artifacts are in `target/release/bundle/`.

## Option C — Cross-compilation from Linux (experimental)

Cross-compiling Tauri apps from Linux to Windows requires:
- `x86_64-pc-windows-gnu` toolchain via rustup
- `mingw-w64` on the host machine
- A Windows WebView2 stub (not trivial)

Not officially supported by Tauri and fragile. Use Option A or B instead.

## Note on monero-wallet-rpc

The Windows build bundles the Rust binary only. The user must separately download
`monero-wallet-rpc.exe` from [getmonero.org/downloads](https://getmonero.org/downloads/) and
configure its path in Settings > RPC.

A future release may bundle the binary inside the installer.
