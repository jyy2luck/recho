# 本地打包

- 用户明确指定目标平台、架构或包类型时，以指定要求为准。目标平台和架构默认采用当前构建机器的操作系统和架构。未指定包类型时（如「打一个包」「打一个新包」），默认打增量包。包名中的架构使用产物的实际架构，如 `arm64` 或 `x64`。
- 打增量包（预览包）时，复用已有构建缓存，运行 `cargo build --locked -p recho`。
  - 在 macOS 上运行 `python3 scripts/bundle-macos.py debug` 和 `codesign --force --sign - app/build/debug/recho.app`，将可直接打开的 `.app` 复制到 `dist/preview/recho-macos-<arch>-preview-YYYYMMDD-HHMMSS/recho.app`。
  - 在 Windows 上将 `app/build/debug/recho.exe` 和 `desktop/assets/fonts/Geist-OFL.txt` 压缩为 `dist/preview/recho-windows-<arch>-preview-YYYYMMDD-HHMMSS.zip`。
- 打正式包时，macOS 运行 `./scripts/build.sh`，Windows 运行 `./scripts/build.ps1`。按 `.github/workflows/package.yml` 中对应平台的步骤，将正式 ZIP 输出到 `dist/` 根目录。
