# 字体来源

- `DroidSansFallback.ttf`：Android Open Source Project `kitkat-release`，
  https://github.com/aosp-mirror/platform_frameworks_base/blob/kitkat-release/data/fonts/DroidSansFallback.ttf
  Apache-2.0，原始版权及完整许可见 `NOTICE`。
- `MaterialIcons-Regular.ttf`：Google Material Design Icons，
  https://github.com/google/material-design-icons/blob/master/font/MaterialIcons-Regular.ttf
  Copyright 2014 Google Inc. All Rights Reserved. Apache-2.0（完整许可见 `NOTICE`）。
- `Geist-Regular.ttf` 与 `Geist-SemiBold.ttf`：Vercel Geist v1.9.2，
  https://github.com/vercel/geist-font/tree/v1.9.2/fonts/Geist/ttf
  SIL Open Font License 1.1（完整许可见 `Geist-OFL.txt`）。
- `RechoGeistTabular-Regular.ttf` 由上述 Geist Regular 派生。用 FontTools 将 U+0030–U+0039 的字符映射替换为 `tnum` 指定的字形，保留这十个字符并将字体家族命名为 Recho Geist Tabular。许可同上。

应用内置字体按可见字形栅格化。Windows 优先使用本机微软雅黑显示中文，其余平台使用内置中文字体作为回退。
