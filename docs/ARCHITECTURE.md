# 架构说明

## 总体结构

DSH Desktop 由三个层次组成：

1. Tauri 原生外壳。
2. 本地控制 WebView。
3. Harness WebView 和由其启动的 Node.js 服务。

原生窗口只有一个。窗口内部保留两个子 WebView：

- `main`：本地控制页，负责状态、日志、更新、重启和中英文切换。
- `harness`：加载 `127.0.0.1` 上的 Harness Web UI。

Harness 子 WebView 的顶部位置默认为 `64` 个逻辑像素。抽屉打开时，其顶部位置
增加 `300` 个逻辑像素，高度同步缩小。该方案不依赖 `iframe`，因此不会遇到 DSH
浏览器 Cookie 的 `SameSite=Strict` 限制。

## 启动流程

1. Tauri 创建主窗口和本地控制 WebView。
2. 控制页先注册状态事件监听器，再调用 `get_status`。
3. Rust 端在后台解压便携资源到 `%APPDATA%`。
4. Rust 端分配本机空闲端口。
5. 通过 sidecar 启动内置 `node.exe`。
6. 执行：

```text
node <runtime>\node_modules\@deepseek-ai\dsh\lib\bin.js web --port <port> --no-open
```

7. 从 stdout 中解析带随机 token 的 Harness URL。
8. 使用 token URL 执行健康检查。
9. 在同一原生窗口中创建 Harness 子 WebView。
10. 发布 `running` 状态并收起状态抽屉。

## 路径解析

便携程序：

```text
<portable>\DeepSeekHarness.exe
<portable>\node.exe
<portable>\runtime\
<portable>\tools\
```

可写运行目录：

```text
%APPDATA%\com.deepseek.harness.desktop\harness\source
%APPDATA%\com.deepseek.harness.desktop\harness\runtime
```

运行时资源通过 Tauri `resource_dir()` 解析，不依赖进程当前工作目录。Node.js
作为 sidecar 与主程序放在同一目录。

## 首次部署

便携包不携带 Harness 源码或构建产物，只有 Node.js、pnpm、MinGit 和修复脚本。

首次启动时（`ensure_installation`）：

1. 检查 `source`（`.git` + `package.json`）与 `runtime`（CLI 入口 + `package.json`）
   是否都可用；可用则直接进入启动流程。
2. 否则用内置 MinGit 克隆上游 `master`（`--depth 1 --single-branch`）。
3. 依次执行 `pnpm install`、`pnpm run clean`、`pnpm run build`、`pnpm deploy`
   到 `runtime-next`，再补跑 koffi / node-pty / spawn-helper 步骤。
4. 将 `runtime-next` 原子换入 `runtime`，删除备份。

因此首次启动必须联网。构建期间的状态停留在 `initializing`，界面显示下载与构建
进度；失败时不会写入任何半成品运行时。

## Harness 更新

更新流程不会直接修改当前正在运行的 runtime，且会同时充当修复入口：源码目录缺失
或损坏时会先重新克隆。

1. 停止 Node.js 服务进程树。
2. `git rev-parse HEAD` 读取本地提交，`git fetch origin master` 后读取 `FETCH_HEAD`；
   两者相同则跳过后面的重建，直接把服务拉起来并报告“已是最新”（强制重建按钮跳过
   这一步比较）。
3. `git reset --hard FETCH_HEAD`。
4. 清理未跟踪文件，但保留 `node_modules`。
5. 安装 workspace 依赖。
6. 清理并重新构建。
7. 部署到 `runtime-next`。
8. 修复 workspace peer 依赖。
9. 执行必要的 native postinstall。
10. 校验入口文件存在。
11. 将当前 `runtime` 重命名为 `runtime-backup`。
12. 将 `runtime-next` 重命名为 `runtime`。
13. 启动新运行时。

新运行时启动失败时：

1. 停止失败进程。
2. 删除新 runtime。
3. 将 `runtime-backup` 恢复为 `runtime`。
4. 启动旧运行时。

## 进程生命周期

Node.js 子进程的 PID 保存在 Tauri 状态中。

窗口关闭事件执行：

```text
taskkill /PID <pid> /T /F
```

使用 `/T` 会终止 Node.js 创建的全部子进程。应用退出事件也会再次执行清理，防止
窗口事件被绕过。

## 状态同步

每次状态更新都带有递增的 `revision`。前端只接受版本号不低于当前版本的状态，
避免首次 `get_status` 响应覆盖更晚到达的 `running` 事件。

控制页先完成事件监听注册，再请求当前状态。这样可以避免快速启动时错过状态事件。

## 诊断日志

桌面端将日志写入 `%APPDATA%\com.deepseek.harness.desktop\harness\logs`。
`desktop.log` 记录启动、服务状态和错误；每次更新另外生成
`update-YYYYMMDD-HHMMSS-mmm.log`。更新日志包含阶段、命令参数、工作目录、退出码和
失败命令的 stdout/stderr 末尾内容，并保留最近 30 次。

## 中英文

语言选择保存在 WebView 的 `localStorage`：

```text
deepseek-harness-language=en|zh
```

首次没有保存值时，根据系统语言决定。动态状态和常见日志在显示前通过前端词典
转换。
## 单实例保护

应用通过 Tauri single-instance 插件持有进程级单实例锁。

第二次启动 `DeepSeekHarness.exe` 时：

1. 新进程不会执行初始化或更新。
2. 新进程激活并聚焦已有主窗口。
3. 新进程随后退出。

该保护避免两个实例同时原子替换 `source`、`.git`、`apps` 和 `runtime` 目录。
