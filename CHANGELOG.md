# ChangeLog

## 0.6.0

### 破坏性变更

- 二进制名从 `iclass_buaa_tui` 改为 `iclass-buaa-tui`。原因：Cargo 对非 kebab-case 的 bin 名会持续告警，而这个名字同时出现在打包产物、安装路径和文档里。
- 升级后需要重新执行一次 `install-autologin`。已安装的调度器（systemd `.service`、launchd `.plist`、Windows `.cmd`）里写的是旧二进制的绝对路径，重命名后不会再指向有效文件。
- 安装包与产物文件名同步变化：`iclass-buaa-tui-macos-*.dmg`、`iclass-buaa-tui-windows-*.exe`、`iclass-buaa-tui_*_amd64.deb`、`iclass-buaa-tui-*-1.x86_64.rpm`。

### 修复

- BYKC 不再重复提交统一认证密码，改为复用已认证会话读取 `token` 并激活，修复 `423 Locked` 与「无法从 SSO 登录页面解析登录表单」两类失败。
- BYKC 与主登录共享同一个 cookie 会话，此前各建一份 cookie jar 导致 CAS 请求没有会话。
- 删除 `bykc/helpers.rs` 中重复的 CAS 登录实现及其三个只服务于它的辅助函数。
- iClass `STATUS=2` 按「该学期无数据」处理，不再被当成课程拉取失败。
- 修复 WebVPN 模式下 iClass 登录失败并返回 `ERRCODE=106 / 用户不存在` 的问题。
- 原因：旧流程在 SSO 登录后直接把学号作为 `app/user/login.action` 的 `phone` 参数传给 iClass；真实网页会先进入 WebVPN CAS service，再访问 iClass `jumpMyCenter`，从 302 `Location` 中获取临时 `loginName`，最后用 `loginName` 调 iClass app 登录接口。
- 修复方式：VPN 登录入口改为浏览器实际使用的 `service=https://d.buaa.edu.cn/login?cas_login=true`；HTTP 客户端共享同一个 cookie jar；iClass 登录前用 no-redirect client 逐跳处理 `jumpMyCenter` 的 302 并解析 `loginName`。
- 验证：用真实 WebVPN 链路复现 `ticket -> wengine-vpn-token-login -> token-login -> /`，再验证 `jumpMyCenter -> loginName -> app/user/login.action` 能成功返回 iClass session。

### 新增

- 新增课程课表工作区，专注跟随 UBAA 的课程能力，不引入图书馆、预约、打卡等非课程模块。
- 支持本科 BYXT 和研究生 GSMIS 整学期课表导入，按学期和周次浏览课程、地点、教师与节次。
- 课表按账号本地缓存，首次使用手动更新，更新失败不会覆盖旧课表，支持离线查看。
- 课程工作区新增今日课程、课程搜索、考试、成绩、空教室和只读作业摘要。
- 新增课表 Markdown、CSV、JSON、ICS 导出，以及本地课表快照差异比较。
- CLI 新增 `today`、`exams`、`grades`、`classrooms`、`tasks`、`schedule-export` 和 `schedule-diff`。

### 兼容性

- 直连和 WebVPN 登录统一走 SSO 会话；iClass 登录统一通过 MyCenter 跳转解析临时 `loginName`。
