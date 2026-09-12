# DSH Desktop v0.1.6

单实例与并发更新修复版本。

## 修复

- 增加进程级单实例锁。重复启动 `DeepSeekHarness.exe` 时只聚焦已有窗口。
- 防止两个实例并发初始化或更新 Harness 源码目录。
- 修复重复实例同时修改 `source`、`.git`、`apps` 和 `runtime` 时触发 `pnpm run build` 失败的问题。

## 主要功能

- Windows x64 免安装运行。
- 内置 Node.js、pnpm、MinGit 和 Harness 运行时。
- 无需用户安装 Node.js、pnpm 或 Git。
- 单个桌面窗口加载 Harness Web UI。
- 顶部控制栏和可展开的状态/日志抽屉。
- 中英文即时切换。
- 一键更新 Harness。
- 更新失败自动恢复上一个可用运行时。
- 关闭窗口时清理完整 Node.js 进程树。

## 下载

```text
DeepSeekHarness-portable-x64.zip
```

解压后运行：

```powershell
.\DeepSeekHarness\DeepSeekHarness.exe
```

## 系统要求

- Windows 10/11 x64。
- Microsoft Edge WebView2 Runtime。
- 首次启动需要可写的用户目录和足够磁盘空间。

## SHA-256

```text
B9C6E86073BF4174B245497144D29217DF842199866F4B9A84C29CED2894C510
```
