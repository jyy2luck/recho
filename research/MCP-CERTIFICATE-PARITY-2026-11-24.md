# Reqable 3.3.11 的 MCP 与证书安装

2026-11-24，在 macOS 打开 `/Applications/Reqable.app` 3.3.11（233），从菜单与弹窗实测。主程序 SHA-256 为 `de94f86a8dcfa90d5cdd2c8a3e24dea03cc9411942298492e569af871b026f00`。以下 ID 来自 REA 对原版窗口的截图与可访问性观察。

## MCP

| 原版路径 | 实测行为 | REA Evidence ID |
| --- | --- | --- |
| 工具 → MCP | 客户端选择器列出 Claude Code、Cursor、Codex、VS Code、Windsurf、Copilot、Kiro、Gemini CLI、TRAE、Qoder、CodeBuddy、更多 | `ev_4d2ca1c923b53edb080b8fe200562d444527b94b50e4aca85401ac0b0eabc9ff` |
| Codex → Stdio | 可复制 CLI 命令和助手路径，内联打开令牌管理，说明 host、port、scope | `ev_c13b050c588854747f2d34bfbdaad5e3b85188cb1dde3088ad2e319ebc177350` |
| Codex → HTTP | 可复制注册命令、TOML URL 与 Bearer 请求头；提示外部设备需要关闭本机监听限制 | `ev_1209235f1f8de3c7b043695daae8ab166c179c703f1a1811f826c3fa7c24efbb` |
| Claude Code → Stdio / HTTP | 两种模式各提供终端注册命令及参数说明 | `ev_b055472410a908c557e25a7df399146b374caa7c1dcf19869350a30ceb89ec69` / `ev_e8254ceefcfed3c793843dafd3b7f74f4f3f3d762361329d2fe7a57758b4b4fd` |
| 工具 → 访问令牌 | 独立的令牌列表，显示名称、掩码与访问记录，有新建和关闭入口 | `ev_fca6cb236fb68d9a5e5b3041e76296ad81ef4ff9e81e71cba3af7f72ffe4d5eb` |

Recho 接入现有的 `/mcp` 和内置 Stdio 进程，按客户端生成可复制配置。配置默认使用令牌占位符。远程连接设置采用 Recho 自己的访问开关，连接地址和端口来自实际运行状态。

其他客户端的配置页分别提供所需字段。观察到 Copilot CLI 使用 `mcp-config.json`，Gemini CLI 提供终端命令，Kiro 提供配置文件和 `disabled` 字段，Qoder 的 HTTP 类型为 `streamable-http`，CodeBuddy 的 HTTP 配置包含 `type` 字段。「更多」提供通用 JSON。

## 证书安装

| 原版路径 | 实测行为 | REA Evidence ID |
| --- | --- | --- |
| 顶部盾牌 / 证书 → 安装到本机 | 居中弹窗，自动页显示安装状态和安装动作，下方可前往五个设备或应用 | `ev_45c5a475129f7d7f90b8cd93cc61d6f619c94b76243d1d50d8b8ee0348e1435c` |
| 本机 → 手动 | 导出 CA、导入登录钥匙串、设置信任的滚动指引 | `ev_698e79fff8c9c45bbc890efae2ed1aea3ea88d7169cf436d2c0b9203301641bb` |
| Android | 设备列表或空状态，可重新加载和设置 ADB 路径 | `ev_1f32dbd9c42883600280ac70376abf867402725a433f028036f8a2c0fcc6884f` |
| iOS | Wi-Fi 手动代理地址和端口、证书下载地址及二维码、安装与信任步骤 | `ev_16906bacd98fa47eb2b1fb42b263205355399337cf17a21fbcfa890cc8cf0da0` |
| Firefox | 公钥证书导出、证书管理器导入、勾选网站信任 | `ev_98f11c0fe3041fa78ba9fd6640864b62a171f7519adf082f433be1ef2e73d5bb` |
| Java VM | 选择 Java 安装目录，选择前显示未知安装状态 | `ev_3c1712745118aa95889d24a42c3d99b24a36bce835b445a769b3bf54e0312564` |
| Magisk | 下载模块 ZIP，从 Magisk 本地安装 | `ev_a1c12241e1b7f5ac5d83aa4d0d227dd8d3fba056e08d1c4c3481555db4adc547` |
| 证书 → 查看 CA 证书 | 显示主体、签发者等字段与当前信任状态 | `ev_0bfdf98c140502c5dd9033994afc7e043edf6b94f0189ae029e1fda1b2c7772e` |

Recho 用自身主题呈现这些入口与步骤。macOS 自动安装使用当前用户登录钥匙串。iOS 扫码地址为 `http://cert.recho.local/ca`，经 Recho 代理返回当前 CA 公钥证书。Magisk ZIP 适用于 Android 7–13 的传统系统证书目录。

观察中使用了原版的未安装、无连接设备、未选择 JDK 状态。安装授权后的原版行为和每个第三方 MCP 客户端实际的安装结果仍待真机验证。

## Recho 验证

- macOS 隔离实例通过原生窗口走通顶部盾牌、自动和手动切换、五个设备或应用入口、CA 详情、Codex Stdio 和 HTTP，以及独立的访问令牌列表。通过系统保存窗口导出 DER，并和当前 CA 的 DER 逐字节比较。用 macOS Vision 读取界面上的二维码，解码结果为实际下载地址。
- 集成测试验证 MCP HTTP 与 Stdio 鉴权、撤销、访问时间在重启后保留。代理下载测试检查 GET、HEAD、精确路径和响应证书。Android 使用模拟 ADB，Java 使用临时信任库。
- Windows 通过 MSVC 目标交叉检查。系统信任授权、手机安装和第三方客户端安装确认仍待实际环境验证。
