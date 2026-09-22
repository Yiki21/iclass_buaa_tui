# ChangeLog

## 0.6.0

### 破坏性变更

- 直连模式配置里带有统一认证密码时，配置文件权限现在也必须是 `600`。此前该校验只在 `use_vpn = true` 时生效，而直连模式同样使用统一认证密码。

### 新增

- 新增课程课表工作区，专注跟随 UBAA 的课程能力，不引入图书馆、预约、打卡等非课程模块。
- 支持本科 BYXT 和研究生 GSMIS 整学期课表导入，按学期和周次浏览课程、地点、教师与节次。
- 课表按账号本地缓存，首次使用手动更新，更新失败不会覆盖旧课表，支持离线查看。
- 课程工作区新增今日课程、课程搜索、考试、成绩、空教室和只读作业摘要。
- 新增课表 Markdown、CSV、JSON、ICS 导出，以及本地课表快照差异比较。
- CLI 新增 `today`、`exams`、`grades`、`classrooms`、`tasks`、`schedule-export` 和 `schedule-diff`。

### 兼容性

- 直连和 WebVPN 登录统一走 SSO 会话；iClass 登录统一通过 MyCenter 跳转解析临时 `loginName`。

### 界面

- 改为 24 位真彩色配色，色调偏低饱和，接近 btop 的观感；进度条使用渐变色。
- 动画只用于表示真实活动：导入课表、加载课程、登录处理中会显示一个小旋转指示，空闲时界面完全静止。
- 今日课程页新增签到进度条；成绩按分段着色，低分与高分一眼可辨。

### 修复

- 修复课表门户判断错误：此前把“BYXT 会话未激活”当成“研究生账号”，导致本科生按 `u` 导入课表时被送到 GSMIS 并报 `研究生课表导入失败: HTTP 401`。现在先探测门户，探测不出时不会冒充研究生课表。
- 修复主页事件日志区提示 `C 清空` 与 `y 复制最近错误` 无法使用的问题：这两个快捷键此前只在日志弹窗打开后生效。现在任何界面都可使用，输入账号密码或课程搜索时不会误触发。
- 修复 WebVPN 模式下 iClass 登录失败并返回 `ERRCODE=106 / 用户不存在` 的问题。
- 原因：旧流程在 SSO 登录后直接把学号作为 `app/user/login.action` 的 `phone` 参数传给 iClass；真实网页会先进入 WebVPN CAS service，再访问 iClass `jumpMyCenter`，从 302 `Location` 中获取临时 `loginName`，最后用 `loginName` 调 iClass app 登录接口。
- 修复方式：VPN 登录入口改为浏览器实际使用的 `service=https://d.buaa.edu.cn/login?cas_login=true`；HTTP 客户端共享同一个 cookie jar；iClass 登录前用 no-redirect client 逐跳处理 `jumpMyCenter` 的 302 并解析 `loginName`。
- 验证：用真实 WebVPN 链路复现 `ticket -> wengine-vpn-token-login -> token-login -> /`，再验证 `jumpMyCenter -> loginName -> app/user/login.action` 能成功返回 iClass session。
