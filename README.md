

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

安装和启动方式：
- 打开 `.dmg` 后，把 `iClass BUAA TUI.app` 拖到 `Applications`
- 不要直接从桌面上的磁盘映像图标运行，也不要停留在挂载出来的 DMG 里直接启动
- 这是终端 TUI 程序；新版本会在双击 `.app` 时自动拉起 `Terminal.app`
- 如果 macOS 提示“磁盘映像损坏”或 App 已损坏，通常是因为当前 release 未做 Apple Developer ID 签名和公证，被 Gatekeeper 加了隔离标记。确认文件来自本项目 Releases 后，可在终端执行：

```bash
xattr -dr com.apple.quarantine ~/Downloads/iclass_buaa_tui-macos-arm64.dmg
```

Intel 版本请把文件名改成 `iclass_buaa_tui-macos-x64.dmg`。如果已经拖入 Applications 后仍打不开，再执行：

```bash
xattr -dr com.apple.quarantine /Applications/iClass\ BUAA\ TUI.app
```

- 如果你使用的是旧版本，双击没有反应时，请在终端手动运行：

```bash
/Applications/iClass\ BUAA\ TUI.app/Contents/MacOS/iclass_buaa_tui
```

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

自动签到目前仍是实验性功能，但已经支持三套原生调度器：
- Linux: `systemd --user`
- macOS: `launchd`
- Windows: `schtasks`

其中 Linux 支持相对稳定；Windows 和 macOS 版本目前仍属实验性功能，仍需要更多用户帮助测试和反馈。

默认会按 XDG 顺序查找配置文件：
- `$XDG_CONFIG_HOME/iclass-buaa/config.toml`
- `~/.config/iclass-buaa/config.toml`
- `$XDG_CONFIG_DIRS/iclass-buaa/config.toml`
- `/etc/iclass-buaa/config.toml`

配置文件里包含统一认证密码时权限必须是 `600`。系统级配置更适合放不含密码的默认项。
历史字段名仍为 `vpn_username` / `vpn_password`，现在含义是统一认证账号和密码；`vpn_username` 留空时默认使用 `student_id` 作为统一认证账号。直连模式会先建立 SSO 会话，再按 UBAA 的 MyCenter 流程获取 `loginName` 后登录 iClass。

### 课程课表

课表工作区跟随 UBAA 的课程主线，但只保留与课程直接相关的能力：

- 本科账号从 BYXT 读取学期、教学周和整周课表。
- 研究生账号从 GSMIS“我的课表”读取整学期排课，支持完整节次和晚间课程。
- 导入前会先探测账号所属门户，标签页和标题按实际门户显示为“本科课表”或“研究生课表”；探测不出时不冒充研究生课表。
- 首次使用时按 `u` 手动导入当前学期，更新成功后才能离线查看。
- 课表按账号保存在本地，学期和周次可以切换；导入失败不会覆盖已有缓存。
- `,` / `.` 切换已保存学期，`[` / `]` 或 `h` / `l` 切换周，`j` / `k` 选择课程。
- 支持鼠标：点击顶部页签切换工作区，点击课程导航切换视图，点击列表行选中，滚轮滚动列表；弹窗可用滚轮滚动、左键关闭。

界面为 24 位真彩色，采用与 btop 类似的渐变配色；动画只用于表示真实活动（正在导入、正在加载课程等），空闲时不会闪烁。

课表缓存与登录信息都只保存在本机配置目录，不上传到 UBAA 服务端。研究生课表使用与当前登录模式相同的直连或 WebVPN 会话。

示例配置：

```toml
student_id = "2337xxxx"
use_vpn = false
vpn_username = ""
vpn_password = "your-sso-password"
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

#### 桌面通知

自动化在无人值守时运行，所以失败必须能主动找到你：

- `notify_on_failure`（默认 `true`）：签到失败时发桌面通知。账号被锁、会话过期这类情况下，没有通知就只能靠翻调度器日志。
- `notify_on_success`（默认 `false`）：签到成功也通知。默认关闭，避免每次都提醒导致真正重要的失败提醒被忽略。
- 先跑一次 `iclass_buaa_tui notify` 确认这台机器能发出通知；没有 `notify-send`、或没有图形会话（SSH、容器、systemd 服务）时会明确报错并给出原因。
- 通知是尽力而为：发不出去不会影响签到结果和退出码，只多打一行 stderr。

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

# 今日课程、考试、成绩、空教室和作业
iclass_buaa_tui today
iclass_buaa_tui exams --term 2025-2026-1
iclass_buaa_tui grades --term 2025-2026-1
iclass_buaa_tui classrooms --campus 2 --date 2026-09-14 --section 3
iclass_buaa_tui tasks

# 课表导出与缓存差异
iclass_buaa_tui schedule-export --format markdown --term 2025-2026-1
iclass_buaa_tui schedule-export --format ics --output schedule.ics
iclass_buaa_tui schedule-diff --json

# 查看完整参数
iclass_buaa_tui --help
iclass_buaa_tui plan --help
iclass_buaa_tui install-autologin --help
iclass_buaa_tui autologin-status --help
iclass_buaa_tui uninstall-autologin --help
```

课程工作区快捷键：
- `1` 今日课程，`2` 学期课表，`3` 考试，`4` 成绩，`5` 空教室，`6` 作业。
- `/` 搜索课程名、课程号、地点或教师，`j/k` 选择课程，`r` 刷新当前只读数据。
- `u` 手动导入整学期课表；`,/.`、`[`/`]` 用于切换学期和周次。

考试、成绩、空教室和作业都是只读功能。TUI 不提供作业提交、评教、预约或其他不可逆写操作。

主要参数：
- `--config <PATH>`: 显式指定配置文件路径，覆盖默认的 XDG 查找顺序。
- `--log-level <LEVEL>`: 设置结构化日志等级，支持 `error`、`warn`、`info`、`debug`。
- `--log-file <PATH>`: 指定 JSONL 结构化日志路径；不指定时写入用户 state 目录。
- `plan --dry-run`: 输出今日课程的自动签到评估结果、include/exclude 命中规则、时间窗口和跳过原因，不实际签到。
- `install-autologin --output-dir <PATH>`: 指定平台调度文件的输出目录。Linux 写 `.service`/`.timer`，macOS 写 `.plist`，Windows 写包装 `.cmd`。
- `install-autologin --planner-time <HH:MM[:SS]>`: 覆盖配置里的 `planner_time`。
- `install-autologin --planner-interval-minutes <N>`: 覆盖配置里的轮询周期，单位分钟。
- `autologin-status --unit-prefix <PREFIX>`: 查看当前平台调度器状态、下次触发信息和最近日志位置。
- `uninstall-autologin --output-dir <PATH>`: 指定需要删除的平台调度文件目录。
- `uninstall-autologin --unit-prefix <PREFIX>`: 指定需要卸载的任务名前缀。

### 启用自动签到
先安装自动签到调度器：

```bash
iclass_buaa_tui install-autologin
```

安装后可以立即检查调度器健康状态：

```bash
iclass_buaa_tui autologin-status
```

各平台行为：
- Linux: 生成 `systemd --user` 的 `.service` 和 `.timer`，随后需要执行 `systemctl --user daemon-reload && systemctl --user enable --now iclass-buaa-planner.timer`
- macOS: 生成并加载 `~/Library/LaunchAgents/<prefix>.planner.plist`
- Windows: 生成包装 `.cmd` 并注册 `schtasks` 计划任务

Linux 下等价于：

```bash
# 启用新添加的 systemd user service
systemctl --user daemon-reload
systemctl --user enable --now iclass-buaa-planner.timer
```

`install-autologin` 生成的调度配置都会写入当前可执行文件的绝对路径。这是故意的，调度器不应该依赖当前 shell 的工作目录，也不应该假设你的 `PATH` 一定包含该程序。

`planner_time` 定义每天开始自动签到轮询的最早时间；`planner_interval_minutes` 定义轮询间隔。不同平台的调度器会按自己的方式周期触发，但程序在 `planner_time` 之前只会检查并直接退出，不会提前签到。

卸载自动签到：

```bash
iclass_buaa_tui uninstall-autologin
```

自动签到流程：
1. 平台调度器按 `planner_interval_minutes` 周期触发一次。
2. `plan` 登录并读取今天的 iClass 课程，以及可选的 BYKC 签到/签退窗口。
3. 已签到项目会被跳过，未到开始窗口的项目会等待下一轮。
4. 对已经进入窗口的项目，直接执行签到或签退；每次操作前都会重新登录，并按配置重试 `retry_count` 次。

**注意**
CLI 参数对于登录只支持配置文件写入!

## Todo
- 更多其他功能?
- 代码库似乎有些膨胀了, a little bit sucks

Inspired By [iclass_buaa](https://github.com/zeroduhyy/iclass_buaa) && [UBAA](https://github.com/BUAASubnet/UBAA)
