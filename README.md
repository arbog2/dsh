# DSH Desktop

DSH Desktop 是 DeepSeek Harness Web UI 的 Windows x64 桌面封装。它把
Node.js、pnpm、MinGit、Harness 源码和生产依赖放入便携包，用户不需要单独安装
Node.js、pnpm、Git 或 Harness。

## 下载与运行

从 [Releases](https://github.com/arbog2/dsh/releases/latest) 下载：

```text
DeepSeekHarness-portable-x64.zip
```

解压后运行：

```powershell
.\DeepSeekHarness\DeepSeekHarness.exe
```

这是免安装版本。程序只依赖自身目录中的运行时文件，不需要执行 MSI 或 NSIS
安装程序。

## 系统要求

- Windows 10 或 Windows 11，x64。
- Microsoft Edge WebView2 Runtime。绝大多数现代 Windows 10/11 已预装。
- 用户目录可写。
- 初次启动建议预留至少 1 GB 可用空间。
- Harness 更新功能需要可访问 GitHub；初始运行不需要网络。

## 首次启动

首次启动时，程序会在用户可写目录中部署 Harness：

```text
%APPDATA%\com.deepseek.harness.desktop\harness\
├── source\       # Harness Git 源码工作目录
└── runtime\      # 当前生产依赖和 Web 构建产物
```

同时会解压便携包中的 `runtime/harness-source.zip` 和
`runtime/harness-runtime.zip`。因此首次启动需要等待一段时间，具体取决于磁盘
性能和杀毒软件扫描速度。

后续启动会复用已部署的 `source` 和 `runtime`，速度会明显加快。

## 界面

应用使用一个原生窗口：

- 顶部控制栏始终可用。
- Harness Web 界面位于顶部控制栏下方。
- “状态与日志”按钮会展开顶部抽屉，显示启动状态、更新进度和运行日志。
- 再次点击按钮后，Harness 界面恢复到完整高度。
- 关闭这一个窗口时，Harness Node 进程树会被终止。
- 应用使用单实例锁；重复启动只会聚焦已有窗口，不会并发修改 runtime。

顶部控制栏提供：

- 当前 Harness 状态。
- 重启 Harness。
- 更新 Harness。
- 中英文切换。
- 展开或收起状态与日志抽屉。

## Harness 更新流程

点击更新按钮后，程序会依次执行：

1. 停止当前 Harness 服务。
2. 获取上游 `master` 最新提交。
3. 重置可写源码目录。
4. 执行 `pnpm install --frozen-lockfile`。
5. 执行 `pnpm run clean`。
6. 执行 `pnpm run build`。
7. 生成新的生产运行时目录。
8. 修复生产树中可能缺失的 workspace peer 依赖。
9. 运行 node-pty、koffi 等必要的安装步骤。
10. 切换到新运行时并重新启动 Harness。

更新期间会保留旧运行时。如果新版本构建失败或启动失败，应用会恢复旧运行时并
继续提供服务。

## 便携包结构

```text
DeepSeekHarness\
├── DeepSeekHarness.exe
├── node.exe
├── runtime\
│   ├── node\
│   ├── pnpm\
│   ├── git\
│   ├── harness-source.zip
│   └── harness-runtime.zip
└── tools\
    └── repair-runtime.mjs
```

`node.exe` 是 Tauri sidecar。`runtime` 和 `tools` 通过相对路径定位，因此整个
`DeepSeekHarness` 目录可以移动到其他位置或其他 Windows x64 电脑。

## 数据与重置

程序文件位于便携目录，Harness 工作副本位于：

```text
%APPDATA%\com.deepseek.harness.desktop\harness
```

如果首次部署被中断：

1. 关闭 DSH Desktop。
2. 删除 `%APPDATA%\com.deepseek.harness.desktop\harness`。
3. 重新启动 `DeepSeekHarness.exe`。

DSH 的会话、凭据和工作区设置由 Harness 自身管理，删除 Harness 工作副本不一定
会删除这些用户数据。

## 从源码构建

### 环境要求

- Node.js `^22.19.0` 或 `>=24.0.0`
- pnpm `11.7.0`
- Git
- Rust `1.77.2+`
- Visual Studio 2022 C++ Build Tools，包含 MSVC x64 工具链
- Windows WebView2 Runtime

### 安装依赖

```powershell
pnpm install
```

### 准备运行时

```powershell
pnpm prepare:runtime
```

该命令会：

- 下载并固定 Node.js、pnpm 和 MinGit。
- 下载 DeepSeek Harness 源码。
- 初始化用于增量更新的 Git 基线。
- 构建 Host、Client 和 Web 前端。
- 生成生产依赖树。
- 修复上游 runtime 清单可能遗漏的 workspace peer。
- 生成 Tauri 使用的 ZIP 资源。
- 生成 Tauri Node.js sidecar。

首次执行下载量较大，通常需要数分钟。

### 开发模式

```powershell
pnpm tauri dev
```

### 构建便携版

```powershell
pnpm package:portable
```

输出：

```text
DeepSeekHarness\DeepSeekHarness.exe
DeepSeekHarness-portable-x64.zip
```

### 构建安装包

```powershell
pnpm tauri build
```

输出位于：

```text
src-tauri\target\release\bundle\
```

## 项目结构

```text
.
├── index.html
├── src\
│   ├── main.ts
│   └── styles.css
├── scripts\
│   ├── prepare-runtime.mjs
│   ├── repair-runtime.mjs
│   └── build-portable.mjs
├── src-tauri\
│   ├── src\
│   ├── icons\
│   ├── binaries\
│   ├── capabilities\
│   └── tauri.conf.json
└── docs\
    ├── ARCHITECTURE.md
    └── BUILDING.md
```

## 常见问题

### 窗口显示“正在准备本地运行环境”

这是首次部署过程。请等待状态切换为“Harness 已就绪”，不要在部署过程中关闭
程序。

### 提示 WebView2 不可用

安装 Microsoft Edge WebView2 Evergreen Runtime 后重新启动程序。

### Windows Defender 或 SmartScreen 警告

当前发布包未进行商业代码签名。允许运行前，请先核对 Release 页面提供的
SHA-256。

### Node 进程残留

正常关闭窗口会调用：

```text
taskkill /PID <pid> /T /F
```

如果系统强制终止进程或断电，可能留下进程。可在任务管理器结束
`DeepSeekHarness.exe` 和对应的 `node.exe`。

### 更新失败

应用会尝试恢复旧运行时。日志抽屉中会显示失败步骤和错误信息。确认网络、代理
和磁盘空间正常后可以再次更新。

## 架构与构建文档

- [架构说明](docs/ARCHITECTURE.md)
- [构建与发布](docs/BUILDING.md)
