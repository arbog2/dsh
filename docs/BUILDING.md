# 构建与发布

## 工具版本

项目针对以下版本设计和验证：

| 工具 | 版本 |
| --- | --- |
| Node.js | 22.19.0 或 24+ |
| pnpm | 11.7.0 |
| Rust | 1.77.2+ |
| Tauri CLI | 2.x |
| MinGit | 2.55.0.5 |
| DeepSeek Harness | 0.1.5-rc.2 基线 |

Windows 构建还需要 Visual Studio 2022 C++ Build Tools。

## 安装项目依赖

```powershell
pnpm install
```

## 准备内置运行时

```powershell
pnpm prepare:runtime
```

准备过程包括：

- 下载 Node.js 22.19.0。
- 下载 pnpm 11.7.0。
- 下载 MinGit 2.55.0.5。
- 下载 Harness `master` 源码。
- 初始化可更新的 Git 基线。
- 安装依赖。
- 执行 `pnpm run clean`。
- 执行 `pnpm run build`。
- 生成生产部署。
- 修复 workspace peer 依赖。
- 执行 node-pty、koffi 和 subprocess helper 安装步骤。

这些目录（`runtime\node`、`runtime\pnpm`、`runtime\git`、`runtime\harness-source`、
`runtime\harness-runtime`）留在磁盘上供本地开发与排查使用，但**不再打包进便携版**：
桌面程序首次启动时自行克隆并构建 Harness。因此发布包只包含 Node.js、pnpm、
MinGit 和修复脚本。

如果下载源不可访问，脚本会尝试备用镜像。

## 代码托管与镜像发布

同一份历史发布在两个远端：

| 远端 | 地址 | 说明 |
| --- | --- | --- |
| `origin` | `https://github.com/arbog2/dsh.git` | 主仓库 |
| `gitee` | `https://gitee.com/arbog/dsh.git` | 国内镜像 |

两个远端的 `main` 指向同一个提交，工作树内容逐文件一致。仓库里唯一的镜像专用
改动是 `LICENSE`（木兰宽松许可证 v2）：它由 `2a622ab` 引入，并通过合并提交
`839777f` 并入主分支，从而在保留该提交的同时让两个远端合流。

```powershell
git push origin main
git push gitee main
git push origin --tags
git push gitee --tags
```

### Gitee Release 附件

Gitee 的单个 Release 附件上限为 100 MB，便携包约 79 MiB，可以直接整包上传，无需分卷：

```text
DeepSeekHarness-portable-x64.zip
```

上传用 Gitee OpenAPI v5，token 必须放在查询串里；放进表单会返回 401：

```powershell
curl.exe -X POST "https://gitee.com/api/v5/repos/arbog/dsh/releases/<release_id>/attach_files?access_token=<token>" -F "file=@DeepSeekHarness-portable-x64.zip"
```

Release 正文记录整包 SHA-256，与 GitHub Release 上的一致。

## 开发

```powershell
pnpm tauri dev
```

首次运行前必须已经完成 `pnpm prepare:runtime`。

## 检查

前端检查：

```powershell
pnpm build:web
```

Rust 检查：

```powershell
cargo check --manifest-path src-tauri/Cargo.toml
```

Rust 测试：

```powershell
cargo test --manifest-path src-tauri/Cargo.toml --lib
```

## 构建便携版

```powershell
pnpm package:portable
```

脚本执行：

1. `pnpm tauri build --no-bundle`
2. 创建 `DeepSeekHarness` 目录。
3. 复制 `DeepSeekHarness.exe`。
4. 从 `runtime\node\node.exe` 复制 `node.exe`。
5. 从 `runtime` 复制 `pnpm` 和 `git`。
6. 从 `scripts` 复制 `repair-runtime.mjs` 到 `tools`。
7. 生成 `DeepSeekHarness-portable-x64.zip`。

便携包直接从源码树组装，不读取 `src-tauri\target\release` 下 Tauri 的增量资源
目录——那里的历史资源不会被自动清理。`runtime\harness-source.zip` 和
`runtime\harness-runtime.zip` 即使本地存在也不会被打包。

发布时应上传 ZIP，不需要上传解压后的目录。当前产物约 79 MiB（解压后约 201 MiB）。

## 构建安装包

```powershell
pnpm tauri build
```

会生成：

```text
src-tauri\target\release\bundle\nsis\DeepSeek Harness_0.1.7_x64-setup.exe
src-tauri\target\release\bundle\msi\DeepSeek Harness_0.1.7_x64_en-US.msi
```

当前主要发布目标是免安装便携包。

## 发布检查清单

- `pnpm build:web` 通过。
- `cargo check` 通过。
- `cargo test --lib` 通过。
- 从空 `%APPDATA%` 联网启动便携版成功（首次运行克隆并构建 Harness）。
- 首次运行期间显示“正在准备首次运行环境”与构建进度，构建完成后切换到
  “Harness 已就绪”。
- 第二次启动复用已有安装，日志出现 `Harness is already installed`。
- 版本相同的情况下点击更新返回“已是最新”，不触发重建。
- 强制重新构建按钮可以完整重建并恢复服务。
- 中英文切换正常。
- 状态与日志抽屉可以展开和收起。
- 更新成功后 Harness 能重新启动。
- 关闭窗口后 Node.js 进程树已清理。
- ZIP 可以在另一个目录解压并运行。
- 重复启动 exe 时第二个实例会退出并聚焦已有窗口。
- Release 页面记录 ZIP 的 SHA-256。

## 发布资产

```text
DeepSeekHarness-portable-x64.zip
```

不要提交以下内容到 Git：

```text
node_modules\
dist\
src-tauri\target\
runtime\node\
runtime\pnpm\
runtime\git\
runtime\harness-source\
runtime\harness-runtime\
runtime\*.zip
DeepSeekHarness\
DeepSeekHarness-portable-x64.zip
```

这些内容由源码和构建脚本重新生成。
