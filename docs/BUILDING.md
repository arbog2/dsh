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
- 生成 `harness-source.zip` 和 `harness-runtime.zip`。
- 生成 Tauri sidecar。

如果下载源不可访问，脚本会尝试备用镜像。

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
4. 复制 `node.exe`。
5. 复制 `runtime`。
6. 复制 `tools`。
7. 生成 `DeepSeekHarness-portable-x64.zip`。

发布时应上传 ZIP，不需要上传解压后的目录。

## 构建安装包

```powershell
pnpm tauri build
```

会生成：

```text
src-tauri\target\release\bundle\nsis\DeepSeek Harness_0.1.5_x64-setup.exe
src-tauri\target\release\bundle\msi\DeepSeek Harness_0.1.5_x64_en-US.msi
```

当前主要发布目标是免安装便携包。

## 发布检查清单

- `pnpm build:web` 通过。
- `cargo check` 通过。
- `cargo test --lib` 通过。
- 从空 `%APPDATA%` 启动便携版成功。
- 状态从“正在准备本地运行环境”切换到“Harness 已就绪”。
- 中英文切换正常。
- 状态与日志抽屉可以展开和收起。
- 更新成功后 Harness 能重新启动。
- 关闭窗口后 Node.js 进程树已清理。
- ZIP 可以在另一个目录解压并运行。
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
