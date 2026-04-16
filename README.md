# 🎓 北航 iClass 签到系统 TUI 版本

## 注意事项
- 本项目仅用于个人学习和研究交流，请勿用于违反学校规定的用途。
- 系统会话 (Session) 仅在本地存储登录状态，绝不会收集或上传个人的账号与密码。
- 若 iClass 系统接口更新，可能需要调整代码后才能继续使用，本项目无法保证长期及时更新。

## 安装向导

优先前往 [GitHub Releases](https://github.com/Yiki21/iclass_buaa_tui/releases) 下载对应平台的构建产物。

### macOS
在 Releases 中选择对应架构的 `.dmg` 安装包：
- Apple Silicon: `macos-arm64`
- Intel: `macos-x64`

### Windows
在 Releases 中选择对应架构的 `.exe`：
- x64: `windows-x64`
- ARM64: `windows-arm64`

下载后直接运行，或自行放到已加入 `PATH` 的目录中。

### Debian / Ubuntu
下载 `.deb` 后安装：

```bash
sudo apt install iclass_buaa_tui_<version>_amd64.deb
```

ARM64 设备请改用：

```bash
sudo apt install iclass_buaa_tui_<version>_arm64.deb
```

### Fedora / RHEL
下载 `.rpm` 后安装：

```bash
sudo dnf install iclass_buaa_tui-<version>-1.x86_64.rpm
```

ARM64 设备请改用：

```bash
sudo dnf install iclass_buaa_tui-<version>-1.aarch64.rpm
```

### Default
如果你已经安装 Rust 工具链，也可以直接从源码安装：

```bash
cargo install --path .
```

## 自动签到 CLI
无参数运行时仍然进入 TUI；带子命令时进入 CLI 自动化模式。

自动签到目前是实验性功能，仅支持带 `systemd` 的 Linux 环境。

默认会按 XDG 顺序查找配置文件：
- `$XDG_CONFIG_HOME/iclass-buaa/config.toml`
- `~/.config/iclass-buaa/config.toml`
- `$XDG_CONFIG_DIRS/iclass-buaa/config.toml`
- `/etc/iclass-buaa/config.toml`

如果 `use_vpn = true` 且配置文件里包含 `vpn_password`，权限必须是 `600`。系统级配置更适合放不含密码的默认项。

示例配置：

```toml
student_id = "2337xxxx"
use_vpn = true
vpn_username = "your-vpn-user"
vpn_password = "your-vpn-password"
enable_iclass = true
enable_bykc = false

advance_minutes = 5
retry_count = 6
retry_interval_seconds = 30

include_courses = ["*"]
exclude_courses = ["体育", "*实验课*"]
iclass_include_courses = []
iclass_exclude_courses = []
bykc_include_courses = []
bykc_exclude_courses = []

planner_time = "07:00:00"
planner_interval_minutes = 10
```

`enable_iclass` 和 `enable_bykc` 分别控制自动化是否纳入 `iClass` 和 `BYKC`。如果启用 `BYKC`，自动轮询会同时处理签到和签退。

`include_courses = ["*"]` 表示默认包含全部课程，然后再应用 `exclude_courses` 过滤。你也可以分别配置：
- `iclass_include_courses` / `iclass_exclude_courses`
- `bykc_include_courses` / `bykc_exclude_courses`

如果某一侧的专属过滤为空，就回退到通用的 `include_courses` / `exclude_courses`。过滤模式支持：
- 课程名精确匹配
- `*` 通配符
- `course_id` / `course_sched_id` / BYKC `course_id` 匹配

`enable_bykc = true` 时，`plan` 和 `list-today` 会额外纳入博雅已选课程里“今天存在签到窗口或签退窗口”的项目。该能力要求 `use_vpn = true`。

### CLI 命令说明
常用命令：

```bash
# 输出今日匹配签到目标（iClass + 可选 BYKC，含 BYKC 签退）
iclass_buaa_tui list-today --json

# iClass 直接签到，失败后按配置重试
iclass_buaa_tui sign --course-sched-id 123456789

# BYKC 手动签到
iclass_buaa_tui sign --source bykc --bykc-course-id 12345 --course-name "博雅课程名"

# BYKC 手动签退
iclass_buaa_tui sign --source bykc --action sign-out --bykc-course-id 12345

# 执行一次自动签到轮询：抓今天签到/签退目标并直接尝试执行到点项目
iclass_buaa_tui plan

# 查看完整参数
iclass_buaa_tui --help
iclass_buaa_tui plan --help
iclass_buaa_tui install-systemd --help
iclass_buaa_tui uninstall-systemd --help
```

主要参数：
- `--config <PATH>`: 显式指定配置文件路径，覆盖默认的 XDG 查找顺序。
- `plan --dry-run`: 只输出今日课程的自动签到评估结果，不实际签到。
- `install-systemd --output-dir <PATH>`: 指定生成 `.service`/`.timer` 文件的目录。
- `install-systemd --planner-time <HH:MM[:SS]>`: 覆盖配置里的 `planner_time`。
- `install-systemd --planner-interval-minutes <N>`: 覆盖配置里的轮询周期，单位分钟。
- `uninstall-systemd --output-dir <PATH>`: 指定需要删除的 `.service`/`.timer` 所在目录。
- `uninstall-systemd --unit-prefix <PREFIX>`: 指定需要卸载的 systemd unit 名前缀。

### 启用自动签到
先安装周期轮询的 systemd user service/timer：

```bash
iclass_buaa_tui install-systemd

# 启用新添加的 systemd user service
systemctl --user daemon-reload
# Or
systemctl --user enable --now iclass-buaa-planner.timer
```

`install-systemd` 生成的 `ExecStart=` 会写当前可执行文件的绝对路径，这是故意的。`systemd` 不应该依赖当前 shell 的工作目录，也不应该假设你的 `PATH` 一定包含该程序。

`planner_time` 定义每天开始自动签到轮询的最早时间；`planner_interval_minutes` 定义轮询间隔。timer 会周期触发，但程序在 `planner_time` 之前只会检查并直接退出，不会提前签到。

卸载自动签到：

```bash
iclass_buaa_tui uninstall-systemd
```

自动签到流程：
1. `planner.timer` 按 `planner_interval_minutes` 周期触发一次。
2. `plan` 登录并读取今天的 iClass 课程，以及可选的 BYKC 签到/签退窗口。
3. 已签到项目会被跳过，未到开始窗口的项目会等待下一轮。
4. 对已经进入窗口的项目，直接执行签到或签退；每次操作前都会重新登录，并按配置重试 `retry_count` 次。

**注意**
CLI 参数对于登陆只支持配置文件写入!

## Todo
- 自动签到功能支持 Windows/MacOS/Linux without systemd
- 更多其他功能?

Inspired By [iclass_buaa](https://github.com/zeroduhyy/iclass_buaa)
