# scvd

SJTU Canvas 视频下载器（Rust CLI）。用于从上海交通大学 Canvas（`oc.sjtu.edu.cn`）课程中导出课程信息并下载视频资源。

> 说明：本工具仅用于你对自己有访问权限的课程内容进行备份与离线学习。请遵守学校/课程的使用条款与版权要求。

## 功能概览

- 登录 Canvas（支持验证码流程）
- 列出/导出课程信息
- 按课程与讲次选择器下载视频

## 环境要求

- Rust toolchain（stable）

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

### 典型工作流（示例）

1. 先登录（如需验证码，按提示操作）
2. 查询课程并拿到 `course-id`
3. 使用 `course-id` + 讲次选择器下载

> 由于具体子命令/参数可能会调整，请以 `--help` 输出为准。
