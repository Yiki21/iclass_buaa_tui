# 🎓 北航 iClass 签到系统 TUI 版本

# ⚠️ 注意事项
- 本项目仅用于个人学习和研究交流，请勿用于违反学校规定的用途。
- 系统会话 (Session) 仅在本地存储登录状态，绝不会收集或上传个人的账号与密码。
- 若 iClass 系统接口更新，可能需要调整代码后才能继续使用，本项目无法保证长期及时更新。

# Todo:
1. 使用 systemd timer, 实现每天获取今日课程, 再创建对应的 systemd timer 任务自动签到
    - 导出今日课程(no TUI just cli)
    - 直接签到某个课程(no TUI just Cli)
2. 打包程序

Inspired By [iclass_buaa](https://github.com/zeroduhyy/iclass_buaa)
