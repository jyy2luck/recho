# recho

Windows/macOS 抓包工具的纯 Rust 重写验证版，使用 **egui/eframe + Glow** 构建界面，
直接链接 Rust 引擎。无需 Flutter、Dart 或 WebView，无账号和云服务。

源项目 [Respable](https://github.com/jyy2luck/Respable) 是重写参照；引擎来自提交
`762a5902c84f91ec250640c6123c1a5286cd658d` 的独立副本，本项目不依赖旁边的源码目录。

界面沿用参照版的 40px 导航栏、260px 侧栏、代理工具栏、调试标签、九列表格、
双击打开详情及请求/响应上下分区。包含抓包启停、端口设置、HTTPS 解密开关、
URL/方法/状态搜索、应用/域名与快捷筛选、列宽调整/排序、请求/响应头、完整归档正文读取、
Text/Hex 虚拟滚动、完整文本复制及清空记录。列表只同步摘要，打开详情时读取请求与响应正文；
空闲不连续重绘，后台命令队列和结果邮箱有界。

这是第一阶段原型，尚未迁移 API 编辑器、规则/断点管理界面、历史浏览、多标签、
系统代理或证书安装界面。中文界面及中文正文使用内嵌 Droid Sans Fallback，图标使用 Material Icons；复杂输入法体验待验。
OpenGL 在 macOS 已弃用，Windows 的驱动、DPI 和辅助功能需实机验证。

![recho 工作台](research/workbench.png)

[查看请求/响应详情截图](research/details.png)

## 构建和运行

需要 Rust 1.95+ 和平台 C/C++ 编译工具。检查还需要 Node.js、Python 3 和 OpenSSL。

```sh
./scripts/build.sh
open app/build/release/recho.app
```

Windows 在 Visual Studio Developer PowerShell 中执行：

```powershell
./scripts/build.ps1
./app/build/release/recho.exe
```

也可直接 `cargo run --release --locked -p recho`。构建缓存及产物统一在 `app/build/`，
macOS app 使用本地 ad-hoc 签名，尚未公证；Windows 尚未签名。

默认数据目录为 macOS `~/Library/Application Support/recho`、Windows `%LOCALAPPDATA%\recho`。
默认随机代理端口，窗口顶部显示实际地址；客户端需手动配置该代理。
开启 HTTPS 解密后，客户端还需显式信任窗口显示的本地 CA。
程序不会自动修改系统代理或系统证书信任。

隔离运行示例（macOS）：

```sh
RECHO_DATA_DIR="$(mktemp -d)" RECHO_PROXY_PORT=0 app/build/release/recho
```

`RECHO_DATA_DIR` 必须是绝对路径；`RECHO_PROXY_PORT` 可指定 0–65535。
请为 recho 使用独立目录，不要指向正在运行的 Respable 数据目录。
抓包默认落盘，界面的清空只移出当前捕获列表，保留已保存历史和正文归档。
历史浏览界面尚未迁移；当前可通过引擎本地 API 访问，清空前会弹出确认框。

## 验证和范围

```sh
./scripts/check.sh
# Windows: ./scripts/check.ps1
```

详见 [阶段规格](specs/001-rust-desktop.md) 与 [验证记录](research/VALIDATION.md)。
内存以实际 Release 测量为准，不设固定的 50 MB 门槛。
