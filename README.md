# 🎓 北航校园服务终端

北航（BUAA）课程与校园服务的终端工具：课表、成绩、作业、签到、博雅课程、研讨室、图书馆座位、阳光打卡、评教。同一个二进制既可以当交互式 TUI 用，也可以当脚本和 LLM 调用的 CLI 用。

[![Release](https://img.shields.io/github/v/release/Yiki21/iclass_buaa_tui)](https://github.com/Yiki21/iclass_buaa_tui/releases)

## 安装

优先从 [GitHub Releases](https://github.com/Yiki21/iclass_buaa_tui/releases) 下载对应平台的产物。

| 平台 | 文件 |
|---|---|
| macOS (Apple Silicon / Intel) | `iclass_buaa_tui-macos-arm64.dmg` / `-macos-x64.dmg` |
| Windows (x64 / ARM64) | `iclass_buaa_tui-windows-x64.exe` / `-windows-arm64.exe` |
| Debian / Ubuntu | `iclass-buaa-tui_<version>-1_amd64.deb`（`arm64` 同理） |
| Fedora / RHEL | `iclass_buaa_tui-<version>-1.x86_64.rpm`（`aarch64` 同理） |

```bash
# Debian / Ubuntu
sudo apt install ./iclass-buaa-tui_<version>-1_amd64.deb

# Fedora / RHEL
sudo dnf install ./iclass_buaa_tui-<version>-1.x86_64.rpm
```

GitHub 每次发版的 tag 是 `Build-vX.Y.Z`，Release 名就是它（[列表](https://github.com/Yiki21/iclass_buaa_tui/releases)）。

macOS：打开 `.dmg`，把 `iClass BUAA TUI.app` 拖进 `Applications`，然后从「应用程序」启动。这是终端程序，双击会自动拉起 `Terminal.app`。当前 release 未做 Apple Developer ID 签名与公证，若提示「已损坏」，确认文件来自本项目 Releases 后执行：

```bash
xattr -dr com.apple.quarantine ~/Downloads/iclass_buaa_tui-macos-arm64.dmg
# 若已拖入 Applications 仍打不开
xattr -dr com.apple.quarantine "/Applications/iClass BUAA TUI.app"
```

有 Rust 工具链也可以从源码安装：

```bash
cargo install --path .
```

## 快速开始

### 1. 写配置

登录只从配置文件读取凭据，命令行不接收密码。取第一个存在的文件：

Windows：

- `%APPDATA%\iclass-buaa\config.toml`
- `%XDG_CONFIG_HOME%\iclass-buaa\config.toml`
- `%USERPROFILE%\.config\iclass-buaa\config.toml`

Linux 与 macOS：

- `$XDG_CONFIG_HOME/iclass-buaa/config.toml`，默认 `~/.config/iclass-buaa/config.toml`
- `$XDG_CONFIG_DIRS/iclass-buaa/config.toml`，默认 `/etc/xdg`
- `/etc/iclass-buaa/config.toml`

Windows 上 `HOME` 通常不设，程序不再依赖它：配置放 `%APPDATA%`，日志等本机状态放 `%LOCALAPPDATA%\iclass-buaa\events.jsonl`。Linux 与 macOS 的日志仍在 `$XDG_STATE_HOME/iclass-buaa/events.jsonl`，默认 `~/.local/state/iclass-buaa/events.jsonl`。

```toml
student_id = "2337xxxx"
use_vpn = true
vpn_username = "2337xxxx"      # 统一认证账号，留空则用 student_id
vpn_password = "your-sso-password"   # 统一认证密码

enable_iclass = true
enable_bykc = false

advance_minutes = 5
retry_count = 6
retry_interval_seconds = 30

include_courses = ["*"]
exclude_courses = ["体育", "*实验课*"]
```

完整示例见 [config.example.toml](config.example.toml)。**配置文件含密码时权限必须是 `600`**，否则拒绝加载。

`vpn_username` / `vpn_password` 是历史字段名，现在含义就是统一认证账号和密码；`use_vpn` 控制是否经 WebVPN 访问。BYKC（博雅）必须 `use_vpn = true`。

课程过滤用 `include_courses` / `exclude_courses`（支持 `*` 通配符，也按 `course_id` 匹配），两侧还能分别用 `iclass_include_courses` / `bykc_include_courses` 等覆盖；专属列表为空时回退到通用列表。

### 2. 先自检

```bash
iclass_buaa_tui doctor        # 检查 WebVPN、统一认证、iClass、BYKC 连通性
iclass_buaa_tui notify        # 确认这台机器能弹桌面通知
```

### 3. 用起来

```bash
iclass_buaa_tui               # 不带子命令 = 进入 TUI
iclass_buaa_tui today         # 今日课程与下一节课
iclass_buaa_tui list-today    # 今天的签到目标
```

TUI 里按 `tab` 切换七个工作区：课表、签到、博雅、研讨室、图书馆、打卡、评教。所有写操作（预约、打卡、评教、签到）都会先弹确认框，说明将要做什么以及后果。

## 主要功能

**课表与学业**（只读）：本科从 BYXT、研究生从 GSMIS 读取课表；首次使用按 `u` 导入当前学期，之后可离线查看。`1`~`6` 切换今日课程、学期课表、考试、成绩、空教室、作业；`/` 搜索课程名/课程号/地点/教师；`,/.` 换学期，`[`/`]` 换周。课表可导出为 markdown / csv / json / ics，也可比较两个学期的差异。

**签到**：支持 iClass 签到和博雅签到/签退，失败按配置重试；TUI 里可生成签到二维码（`g` 终端内刷新、`G` 存成图片）。`plan` 跑一轮自动签到，`install-autologin` 把轮询交给系统调度器（Linux `systemd --user`、macOS `launchd`、Windows `schtasks`），`autologin-status` 看调度器健康状态。无人值守时失败会发桌面通知（`notify_on_failure`，默认开）。

**博雅课程**：浏览可选课程、已选课程、课程详情与学分统计，以及选课/退选（需 `--yes`）。

**预约**：研讨室按房间和时段预约；图书馆座位按图书馆 → 阅览区 → 座位逐级选，都能查看和取消自己的预约。座位相关命令不只是能约，也能查：`seats` 列图书馆，加 `--library` 列阅览区，`seat-map --area <id>` 列座位。

**打卡与评教**：阳光打卡查看各类别进度与历史记录，TUI 里按 `s` 一键随机时段打卡；评教列出待评课程、查看问卷并按 `--yes` 提交。这两类提交都无法撤销。

**CLI 与 TUI 的边界**：数据读取和写操作两边都有。只有二维码签到（TUI 里 `g` 生成、`G` 存成图片）没有 CLI 命令，`skills`、`schema` 这类命令只对调用方有意义，TUI 里没有。TUI 面向交互，CLI 面向脚本和自动调度。

## AI Native

这个工具是给 agent 用的第一等公民。

### 先读接口，不要猜

```bash
iclass_buaa_tui schema
```

输出全部命令的 JSON 描述：参数、`effect`（`read` / `write` / `write_irreversible`）、`confirmation_flag`、`supports_json`。参数由 clap 反射得到，不会与真实解析器脱节；命令组会摊平成叶子（`skills install`），因为不同子命令的 effect 不同。优先读它，不要抓 `--help`。

### 写操作只在显式确认后执行

**所有非 `read` 的命令没有 `--yes` 就不会写入，而且退出码仍然是 0。** 不加 `--yes` 时只做预览，输出 `"submitted": false`。判断是否真的写入要看 `--json` 里的 `submitted` 字段，不能只看退出码。

预览本身会重读实时状态，所以它也能告诉你这次写入现在是否还成立。工作方式建议是两次调用：先不带 `--yes` 预览给用户看，得到明确同意后再带 `--yes`。

`write_irreversible`（`clockin-submit`、`eval-submit`）没有任何撤销手段，只有在用户明确要求提交那一次时才执行。

### 失败有稳定错误码

带 `--json` 时失败会往 stderr 输出结构化报告：

```json
{"error": {"message": "...", "causes": ["..."]},
 "command": "seat-book", "retryable": false, "code": "resource_unavailable"}
```

按 `code` 决定动作：`not_authenticated` 重试一次后跑 `doctor`；`config_invalid` 让用户改配置（含密码时需 `600`）；`invalid_argument` 修参数、别原样重试；`resource_unavailable` 换一个目标；`account_locked` 等锁过期；`rate_limited` 退避；`upstream_timeout` / `network_error` / `upstream_error` 退避后重试；`unknown` 当不可重试处理。退出码只有 0 和 1。

### 内置 Agent Skills

描述各领域工作流的 Agent Skills 编译在二进制里，版本与命令始终一致：

```bash
iclass_buaa_tui skills list                                       # 列出 6 个 skill
iclass_buaa_tui skills show buaa-campus                           # 打印入口 skill
iclass_buaa_tui skills install --target ~/.claude/skills --yes     # 装到任意 agent 的 skills 目录
```

入口是 `buaa-campus`（通用规则与路由），其余按领域拆分：`buaa-academics`、`buaa-attendance`、`buaa-bykc`、`buaa-booking`、`buaa-clockin-eval`。源文件在 `skills/`。`install` 不会覆盖不是它自己写的文件，有冲突就整批拒绝；`--force` 只更新它自己之前装过的文件。

更完整的调用约定见 [AGENTS.md](AGENTS.md)。

## 注意事项

- 本项目仅供个人学习和研究交流，请勿用于违反学校规定的用途。
- 登录凭据只存在本机配置目录，不上传；课表缓存同样只存本地。
- 上游接口随时可能变化，不保证长期及时跟进。

Inspired by [iclass_buaa](https://github.com/zeroduhyy/iclass_buaa) && [UBAA](https://github.com/BUAASubnet/UBAA)
