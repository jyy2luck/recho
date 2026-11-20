# recho 验证记录

日期：2026-11-20。参照源：Respable `762a5902c84f91ec250640c6123c1a5286cd658d`。
第一阶段采用 egui/eframe 0.36.2、Glow、同进程 Rust 引擎。原 Respable 工作区保持干净。

## 本地验证

- `./scripts/check.sh` 通过：桌面端 3 项、引擎 69 项 Rust 测试，67 项 Node 引擎集成测试；fmt、workspace build 通过。
- `./scripts/build.sh` 通过：生成可启动、ad-hoc 签名的 macOS app，嵌入中文字体及 Material Icons。
- `python3 scripts/smoke-macos.py` 通过：真实启动 Release 原生窗口，通过 macOS 辅助功能和鼠标事件操作，不以截图替代交互断言。
- 代理真实转发 80,342 字节中文文本；双击开详情，请求体、响应体与 gzip 解码后的正文复制与原始字节一致；请求头可正确切换查看。
- 文本、文本 Hex、81,930 字节二进制 Hex 均可滚动至末尾；JSON/All 快捷筛选能隐藏/恢复记录。
- 真实点击停止/调试；清空需确认，列表为空，SQLite 仍保留 3 条历史。后续 200 条请求不会使清空前记录重现。
- 正常关闭窗口 exit 0、control descriptor 删除、代理端口可重新绑定。未更改系统代理或证书信任。
- 引擎测试另覆盖暂停恢复、占用端口配置失败时保留旧监听、按需正文读取及归档保留。

## Release 内存：macOS arm64 单次样本

环境：macOS 26.6.2、arm64；1450×900 窗口，辅助功能已激活。
隔离用户数据目录、随机端口；单一桌面进程内包含引擎。HTTP 夹具与测试工具是外部测试进程，不计入应用。
通过 `proc_pid_rusage(v2)` 每 200 ms 采样。每个稳定阶段等待 5 秒，取末尾 3 秒中位数；CPU 为同期占一个核心百分比。

| 场景 | Physical footprint | RSS | 空闲 CPU |
| --- | ---: | ---: | ---: |
| 空列表 | 124.99 MiB | 115.06 MiB | 0.00% |
| 完成交互并抓取 200 条后 | 186.92 MiB | 154.97 MiB | 0.00% |

负载：200 次串行 POST，请求与响应正文各 64,160 字节，间隔 30 ms；正文归档落盘，保留 200 条实时预览。
整个启动、交互、负载过程的采样峰值 footprint 为 201.95 MiB；可执行文件 15,618,672 字节（14.90 MiB）。
数据见 [macos-release.json](macos-release.json)，采样脚本见 [smoke-macos.py](../scripts/smoke-macos.py)。

此结果说明空闲无持续绘制、单文件体积较小，但目前完整窗口与引擎的内存仍有进一步优化空间。
不能据此宣称达到 50 MB，或比用户 Windows 上的 Flutter 测量降低了某个比例：平台、负载与计量口径均不同。

## 界面与范围

[工作台截图](workbench.png) / [详情截图](details.png)。沿用原版炭黑橙配色、导航栏与侧栏分组、
代理栏、调试标签、快捷筛选、九列表格及右侧请求/响应分区；支持拖动列宽和分区、排序、单击选中、双击详情、右键复制 URL。
现阶段菜单在窗口内。灰色入口带未迁移说明。

尚未迁移 API 编辑器、规则/断点管理 UI、历史浏览、多标签、macOS 系统菜单、JSON/Tree、批量选择、
系统代理/证书安装 UI 和正式签名安装包。Windows 实机的内存、DPI、显卡驱动、输入法及辅助功能尚未验证。
GitHub Actions 配置了 macOS/Windows 的构建和测试；自动构建不等同于 Windows 实机验收。
