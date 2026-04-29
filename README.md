# scvd

SJTU Canvas 视频下载器（Rust CLI）。用于从上海交通大学 Canvas（`oc.sjtu.edu.cn`）课程中下载视频资源。

> 说明：本工具仅用于你对自己有访问权限的课程内容进行备份与离线学习。请遵守学校/课程的使用条款与版权要求。

## 功能概览

- 登录 Canvas（支持验证码流程）
- 通过 `course-id` + 讲次选择器下载视频
  - 默认使用 `aria2c` 作为下载器（更快、更稳）
  - 若未安装或运行失败，会自动回退到内置下载器（reqwest）
- 查看/管理下载历史（支持重新下载、清空）

## 环境要求

- Rust toolchain（stable）
- （推荐）`aria2c`
  - 本项目默认使用 `aria2c` 下载
  - 若系统中找不到 `aria2c`（或执行失败），会自动 fallback 到内置下载器

## Build

```bash
cargo build --release
```

产物位于：`target/release/sjtu-canvas-video-download`

## 使用方法

该项目是一个命令行工具。常见方式：

### 1) 直接运行

```bash
cargo run --release -- <args>
```

### 2) 先编译再运行

```bash
./target/release/sjtu-canvas-video-download <args>
```

### 查看帮助

```bash
cargo run --release -- --help
```

或：

```bash
./target/release/sjtu-canvas-video-download --help
```

### 下载器说明（aria2c 默认）

下载开始时会显示下载器选择：

- 正常情况：`Downloader: aria2c (default)`
- 若出现 `aria2c not found` / `aria2c exited with status ...`：说明已自动回退到内置下载器（reqwest）

> 本次版本不提供额外 CLI 参数切换下载器；默认优先使用 aria2c，失败则 fallback。

### Verbose / 调试输出

当你需要排查网络流程（例如 v2 OIDC/LTI3 流程）时，可以开启 verbose 模式输出调试信息到 stderr：

```bash
./target/release/sjtu-canvas-video-download -v <subcommand> ...
# 或
./target/release/sjtu-canvas-video-download --verbose <subcommand> ...
```

未开启 verbose 时，不会输出调试信息（但会保留必要的进度提示）。

### 典型工作流（示例）

1. 先登录（如需验证码，按提示操作）
2. 获取课程的 `course-id`
3. 使用 `course-id` + 讲次选择器下载

> 由于具体子命令/参数可能会调整，请以 `--help` 输出为准。
