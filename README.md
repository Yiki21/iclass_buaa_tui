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

## Todo
1. 使用 systemd timer, 实现每天获取今日课程, 再创建对应的 systemd timer 任务自动签到
    - 导出今日课程(no TUI just cli)
    - 直接签到某个课程(no TUI just Cli)
2. 打包程序

Inspired By [iclass_buaa](https://github.com/zeroduhyy/iclass_buaa)
