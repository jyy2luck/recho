# Windows 本地打包

- 用户说「打一个包」「打一个新包」或没有指定包类型时，默认打预览包。运行 `cargo build --locked -p recho`，将 `app/build/debug/recho.exe` 和 `desktop/assets/fonts/Geist-OFL.txt` 压缩为 `dist/preview/recho-windows-x64-preview-YYYYMMDD-HHMMSS.zip`。
- 用户明确说「打正式包」时，运行 `./scripts/build.ps1` 构建 Release 版，并按 `.github/workflows/package.yml` 的 Windows 打包步骤，将正式 ZIP 输出到 `dist/` 根目录。
