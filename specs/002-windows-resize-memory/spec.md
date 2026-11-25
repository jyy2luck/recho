# Feature Specification: Windows 窗口缩放内存

**Feature Branch** `main`

**Created** 2026-11-25

**Status** 已实现并通过 Windows 实机验证

**Input** Windows 上拖动 Recho 窗口边缘时，内存占用一度达到约 700 MB。需要保留实时缩放，并让修复版的空闲占用接近原版。

## User Scenarios & Testing

### User Story 1 - 连续调整工作台窗口（P1）

用户拖动窗口边缘调整工作台大小，界面跟随窗口更新，停止拖动后可以继续抓包。

**Why this priority** 大幅增加的内存占用发生在日常窗口操作中。

**Independent Test** 使用空数据目录启动 Windows Release 版，将窗口设为 1120 × 670 像素，再分 100 步增大到 1720 × 970。记录空闲工作集、缩放期间峰值及停止 4 秒后的工作集。

**Acceptance Scenarios**

1. 在工作台已打开时连续改变窗口尺寸，窗口随操作改变，进程持续运行，内存符合 SC-001。
2. 停止调整窗口后，经本地代理发送一条带正文的 HTTP 请求，确认收到成功状态和完整正文。

### User Story 2 - 保持低空闲内存（P2）

用户打开工作台等待流量时，Recho 的内存占用接近旧版基线。

**Why this priority** 用户会让工作台常驻，空闲时的固定开销也影响日常使用。

**Independent Test** 在同一台机器、同一窗口尺寸及空数据目录条件下，分别启动旧版和新版，比较窗口稳定后的进程工作集。

**Acceptance Scenarios**

1. 首次打开 1450 × 900 像素的窗口并等待界面稳定，测得的工作集符合 SC-002。

### Edge Cases

- 同一进程从默认窗口尺寸先缩小再连续放大，窗口和本地代理仍可使用。
- 刚完成一轮缩放时，工作集应在短时间内回到接近空闲的水平。

## Requirements

### Functional Requirements

- **FR-001** Windows 工作台必须支持实时调整窗口尺寸。
- **FR-002** Windows 工作台必须控制启动时的空闲内存以及连续变更窗口尺寸时的内存增长。
- **FR-003** 窗口缩放后，用户仍须能够经本地代理发送 HTTP 请求并收到完整响应。

## Success Criteria

### Measurable Outcomes

- **SC-001** 在本次 Windows 验证环境中，连续缩放 100 次的工作集峰值比初始空闲值高至多 20 MiB，停止 4 秒后的工作集比初始空闲值高至多 10 MiB。
- **SC-002** 在同机、相同窗口尺寸下，新版空闲工作集至多比旧版高 10%。
- **SC-003** 完成 100 次缩放后，回环 HTTP 请求收到 200 状态，响应正文与夹具返回的字节一致。

## Assumptions

- 验收环境为本次发生问题的 Windows x64 和 Intel UHD Graphics 630。上述比较以同机 Windows Release 样本为基线。
- 两版均使用隔离的空数据目录和随机代理端口。数值取 Windows 进程工作集。
