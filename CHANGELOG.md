# ChangeLog

## Unreleased

### 新增

- 新增课程课表工作区，专注跟随 UBAA 的课程能力，不引入图书馆、预约、打卡等非课程模块。
- 支持本科 BYXT 和研究生 GSMIS 整学期课表导入，按学期和周次浏览课程、地点、教师与节次。
- 课表按账号本地缓存，首次使用手动更新，更新失败不会覆盖旧课表，支持离线查看。
- 课程工作区新增今日课程、课程搜索、考试、成绩、空教室和只读作业摘要。
- 新增课表 Markdown、CSV、JSON、ICS 导出，以及本地课表快照差异比较。
- CLI 新增 `today`、`exams`、`grades`、`classrooms`、`tasks`、`schedule-export` 和 `schedule-diff`。

### 兼容性

- 直连和 WebVPN 登录统一走 SSO 会话；iClass 登录统一通过 MyCenter 跳转解析临时 `loginName`。

### 修复

- 修复 WebVPN 模式下 iClass 登录失败并返回 `ERRCODE=106 / 用户不存在` 的问题。
- 原因：旧流程在 SSO 登录后直接把学号作为 `app/user/login.action` 的 `phone` 参数传给 iClass；真实网页会先进入 WebVPN CAS service，再访问 iClass `jumpMyCenter`，从 302 `Location` 中获取临时 `loginName`，最后用 `loginName` 调 iClass app 登录接口。
- 修复方式：VPN 登录入口改为浏览器实际使用的 `service=https://d.buaa.edu.cn/login?cas_login=true`；HTTP 客户端共享同一个 cookie jar；iClass 登录前用 no-redirect client 逐跳处理 `jumpMyCenter` 的 302 并解析 `loginName`。
- 验证：用真实 WebVPN 链路复现 `ticket -> wengine-vpn-token-login -> token-login -> /`，再验证 `jumpMyCenter -> loginName -> app/user/login.action` 能成功返回 iClass session。
