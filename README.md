# iclass_buaa_tui

北航（BUAA）校园服务的终端客户端。课表、成绩、作业、签到、博雅课程、研讨室、图书馆座位、阳光打卡和评教，都在一个程序里：不带参数启动是交互式 TUI，带子命令就是可以写进脚本、交给定时任务或 LLM agent 调用的 CLI。

[![Release](https://img.shields.io/github/v/release/Yiki21/iclass_buaa_tui)](https://github.com/Yiki21/iclass_buaa_tui/releases)
[![CI](https://github.com/Yiki21/iclass_buaa_tui/actions/workflows/ci.yml/badge.svg)](https://github.com/Yiki21/iclass_buaa_tui/actions/workflows/ci.yml)
[![License: GPL-3.0](https://img.shields.io/github/license/Yiki21/iclass_buaa_tui)](LICENSE)

> 这是学生个人维护的非官方项目，与北京航空航天大学及其各业务系统没有关联。

## 功能

| 领域 | 能做什么 |
|---|---|
| 课表与学业 | 本科读 BYXT、研究生读 GSMIS 的学期课表，缓存后可离线查看；考试安排、成绩、空教室、作业汇总；课表导出为 markdown / csv / json / ics，比较两个学期的差异 |
| 签到 | iClass 课堂签到，博雅签到与签退，失败按配置重试；TUI 里显示签到二维码；`install-autologin` 把定时签到交给系统调度器，`autologin-status` 查看定时任务状态 |
| 博雅课程 | 可选课程、已选课程、课程详情、各类别学分统计，选课与退选 |
| 研讨室 | 按楼层、房间、时段预约，查看并取消自己的预约 |
| 图书馆座位 | 图书馆 → 阅览区 → 座位逐级浏览，座位按平面图坐标摆放；预约、查看「我的预约」、取消 |
| 阳光打卡 | 各类别进度与历史记录，TUI 里一键随机时段打卡；CLI 用 `clockin-submit` 提交（需附照片） |
| 评教 | 待评课程、问卷内容及将要选中的选项、提交（`eval-submit`） |

所有会改变远端状态的操作，TUI 里先弹确认框，CLI 里不加 `--yes` 只做预览。打卡和评教提交后无法撤销，确认框和预览都会写明这一点。

## 安装

从 [Releases](https://github.com/Yiki21/iclass_buaa_tui/releases) 下载对应平台的文件。发版 tag 的格式是 `Build-vX.Y.Z`。

| 平台 | 文件 |
|---|---|
| macOS（Apple Silicon） | `iclass_buaa_tui-macos-arm64.dmg` |
| Windows（x64 / ARM64） | `iclass_buaa_tui-windows-x64.exe` / `-windows-arm64.exe` |
| Debian / Ubuntu | `iclass-buaa-tui_<version>-1_amd64.deb`（`arm64` 同理） |
| Fedora / RHEL | `iclass_buaa_tui-<version>-1.x86_64.rpm`（`aarch64` 同理） |

```bash
sudo apt install ./iclass-buaa-tui_<version>-1_amd64.deb   # Debian / Ubuntu
sudo dnf install ./iclass_buaa_tui-<version>-1.x86_64.rpm   # Fedora / RHEL
```

macOS：打开 `.dmg`，把 `iClass BUAA TUI.app` 拖进「应用程序」。它是终端程序，启动时会打开 `Terminal.app`。发布包没有 Apple Developer ID 签名和公证，提示「已损坏」时，先确认文件来自本项目的 Releases，再执行：

```bash
xattr -dr com.apple.quarantine "/Applications/iClass BUAA TUI.app"
```

只发布 Apple Silicon 版。Intel Mac 请按下面的方式从源码安装。

从源码安装（工具链版本由 `rust-toolchain.toml` 固定，rustup 会自动安装）：

```bash
git clone https://github.com/Yiki21/iclass_buaa_tui.git
cd iclass_buaa_tui
cargo install --path .
```

## 快速开始

### 1. 写配置文件

凭据只从配置文件读取，命令行不接收密码。按顺序查找，用第一个存在的文件：

Windows：

- `%APPDATA%\iclass-buaa\config.toml`
- `%XDG_CONFIG_HOME%\iclass-buaa\config.toml`
- `%USERPROFILE%\.config\iclass-buaa\config.toml`

Linux 与 macOS：

- `$XDG_CONFIG_HOME/iclass-buaa/config.toml`，默认 `~/.config/iclass-buaa/config.toml`
- `$XDG_CONFIG_DIRS/iclass-buaa/config.toml`（默认 `/etc/xdg`）
- `/etc/iclass-buaa/config.toml`

Windows 上 `HOME` 通常不设，程序不再依赖它：配置放 `%APPDATA%`，日志等本机状态放 `%LOCALAPPDATA%\iclass-buaa\events.jsonl`。Linux 与 macOS 的日志仍在 `$XDG_STATE_HOME/iclass-buaa/events.jsonl`，默认 `~/.local/state/iclass-buaa/events.jsonl`。

```toml
student_id = "2337xxxx"
use_vpn = true                       # 经 WebVPN 访问；博雅课程必须为 true
vpn_username = "2337xxxx"            # 统一认证账号，留空时使用 student_id
vpn_password = "your-sso-password"   # 统一认证密码

enable_iclass = true
enable_bykc = false

include_courses = ["*"]
exclude_courses = ["体育", "*实验课*"]
```

文件里有密码时，权限必须是 `600`，否则程序拒绝读取：

```bash
chmod 600 ~/.config/iclass-buaa/config.toml
```

`vpn_username` / `vpn_password` 是沿用下来的字段名，现在表示统一认证的账号和密码。签到重试、通知、课程过滤等其余选项见 [config.example.toml](config.example.toml)。

### 2. 检查连通性

```bash
iclass_buaa_tui doctor   # WebVPN、统一认证、iClass、博雅是否可达
iclass_buaa_tui notify   # 这台机器能否弹出桌面通知
```

### 3. 启动

```bash
iclass_buaa_tui          # 进入 TUI
```

第一次使用时，在课表页按 `u` 导入当前学期课表。

## 使用

### TUI

`Tab` / `Shift+Tab` 在七个工作区之间切换：课表、签到、博雅、研讨室、图书馆、打卡、评教。导入课表后，第一个页签按门户显示为「本科课表」或「研究生课表」。每个页面底部都列出当前可用的按键，常用的几个：

| 页面 | 按键 |
|---|---|
| 课表 | `1`–`6` 切换今日、学期课表、考试、成绩、空教室、作业；`/` 搜索；`,` `.` 换学期；`[` `]` 换周；`u` 更新课表 |
| 签到 | `s` 签到选中的课；`g` 在终端里显示签到二维码，`G` 在浏览器里打开持续刷新的二维码页面 |
| 研讨室 | `O` 我的预约，`x` 取消 |
| 图书馆 | `enter` 逐级进入，`b` 返回；座位平面图里 `hjkl` 或方向键移动，`f` 跳到下一个空位，`enter` 预约；`B` 我的预约，`x` 取消 |
| 打卡 | `s` 随机时段打卡 |
| 通用 | `r` 刷新，`q` 退出 |

座位平面图使用图书馆接口返回的座位坐标，桌子和过道的形状与官方页面一致。终端放不下全部座位号时，每个座位缩成一个字符（`o` 可预约，`x` 不可预约），当前选中座位的编号显示在信息行。

### CLI

```bash
iclass_buaa_tui today                          # 今日课程与下一节课
iclass_buaa_tui grades --all --json            # 全部学期成绩
iclass_buaa_tui seats --date 2026-10-08        # 各图书馆空闲座位
iclass_buaa_tui seat-orders                    # 我的座位预约
iclass_buaa_tui seat-orders --cancel <id>      # 预览取消，不加 --yes 不会执行
iclass_buaa_tui plan --yes                     # 跑一轮自动签到
iclass_buaa_tui install-autologin --yes        # 安装定时签到（systemd --user / launchd / schtasks）
```

完整命令列表用 `iclass_buaa_tui --help` 查看，或者用 `iclass_buaa_tui schema` 拿到 JSON 格式的描述。

## 给 agent 调用

CLI 按程序调用的需要设计，有四条约定：

- **接口可查询**：`schema` 输出每个命令的参数、是读还是写、确认标志是什么、是否支持 `--json`。参数直接取自命令行解析器，不会和实际行为不一致。`venue-orders`、`seat-orders` 平时只读，带 `--cancel` 才写，`schema` 把两种形态分成两条列出。
- **写操作要确认**：不加 `--yes` 时只预览，退出码依然是 0，输出里 `"submitted": false`。是否真的写入，以 `submitted` 字段为准，不要看退出码。
- **错误可分类**：带 `--json` 失败时，stderr 输出带 `code` 和 `retryable` 的报告。`code` 区分未登录、参数错误、资源被占用、限流、上游超时等情况，调用方据此决定换目标、退避还是放弃。
- **自带 Agent Skills**：各领域的操作流程写成 SKILL.md，编译进二进制，版本和命令保持一致。

```bash
iclass_buaa_tui skills list
iclass_buaa_tui skills show buaa-campus                          # 入口 skill
iclass_buaa_tui skills install --target ~/.claude/skills --yes   # 安装到 agent 的 skills 目录
```

`install` 不会覆盖不是它写入的文件，有冲突时整批放弃。错误码表和调用顺序见 [AGENTS.md](AGENTS.md)。

## 开发

```bash
cargo run                                # 启动 TUI
cargo run -- doctor                      # 运行某个子命令
cargo fmt --all -- --check
cargo check --locked
cargo test --locked
cargo clippy --locked --all-targets
```

CI 运行的就是后四条。`rustfmt.toml` 用到了 nightly 才有的格式选项，所以工具链固定为 nightly，版本见 `rust-toolchain.toml`。

代码结构：

| 路径 | 内容 |
|---|---|
| `src/iclass/` | 统一认证、WebVPN 和 iClass 客户端 |
| `src/bykc/` | 博雅课程 |
| `src/cgyy/` | 研讨室 |
| `src/libbook/` | 图书馆座位，`grid.rs` 把座位坐标换算成终端网格 |
| `src/ygdk/` | 阳光打卡 |
| `src/evaluation/` | 评教 |
| `src/academic.rs` `src/schedule.rs` `src/tasks.rs` | 课表、考试、成绩、作业 |
| `src/app.rs` `src/ui.rs` | TUI 状态与渲染 |
| `src/cli/` | 子命令、`schema`、内置 skills |
| `skills/` | Agent Skills 源文件 |
| `tests/fixtures/` | 上游页面与接口的样本 |

## 参与贡献

欢迎提 issue 和 PR。提交前请跑通上面四条 CI 命令。

- 上游接口变了的话，请附上能复现的响应样本（去掉学号、姓名等个人信息），解析代码改动应带上对应测试。
- 新增的写操作需要遵守预览约定：不加 `--yes` 时不发送任何写请求，并输出 `"submitted": false`；同时在 `src/cli/schema.rs` 里登记它的类型。
- 修改命令时同步更新 `skills/` 里的说明，测试会检查 skill 中引用的命令是否存在。

## 注意事项

- 仅供个人学习和研究使用，请遵守学校的相关规定。自动签到、打卡等功能的使用后果由使用者自己承担。
- 凭据和课表缓存只保存在本机，不会上传到任何第三方。
- 学校系统的接口随时可能变化，本项目不保证及时跟进。

## 致谢

- [iclass_buaa](https://github.com/zeroduhyy/iclass_buaa)：iClass 签到流程的参考。
- [UBAA](https://github.com/BUAASubnet/UBAA)：图书馆、研讨室、阳光打卡等接口的参考实现。

## 许可证

[GPL-3.0](LICENSE)
