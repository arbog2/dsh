# DSH Desktop v0.1.7

轻量化发布版本：便携包不再携带 Harness 源码与构建产物，首次启动从 GitHub 构建。

## 变更

- **便携包体积从 238 MiB 降到 78.6 MiB。** 包内只保留 Node.js、pnpm、MinGit 和
  运行时修复脚本；`harness-source.zip`、`harness-runtime.zip` 以及重复的
  `runtime\node\node.exe` 不再打包。
- **首次启动改为联网构建。** 程序用内置 MinGit 克隆上游 `master`，再执行
  `pnpm install` / `clean` / `build` / `deploy`，通常 5–15 分钟。后续启动复用结果，
  只需几秒。
- 首次构建失败不会留下半成品：新运行时先构建到 `runtime-next`，完成并验证后才
  原子换入。
- 顶栏显示本地实际安装的 Harness 版本号，更新后自动刷新。
- 更新前会比较本地提交与上游提交，相同时跳过重建并提示“已是最新”。
- 新增“强制重新构建”按钮（顶栏锤子图标），用于本地提交相同但运行时损坏的情况。
- 修复更新后所有控制按钮保持禁用直到重启的问题。
- 修复启动时用随包基线覆盖已更新目录、导致版本回退的问题。
- 启动占位文案不再声称“每次都是首次运行”，只有真正构建时才显示对应状态。

## 主要功能

- Windows x64 免安装运行。
- 内置 Node.js、pnpm 和 MinGit，用户无需自行安装。
- 单个桌面窗口加载 Harness Web UI。
- 顶部控制栏和可展开的状态/日志抽屉。
- 中英文即时切换。
- 一键更新 Harness，失败自动恢复上一个可用运行时。
- 关闭窗口时清理完整 Node.js 进程树。

## 系统要求

- Windows 10 / 11 x64，Microsoft Edge WebView2 Runtime。
- 首次启动需要可访问 GitHub，并建议预留至少 3 GB 可用空间。

## 下载

```text
DeepSeekHarness-portable-x64.zip
```