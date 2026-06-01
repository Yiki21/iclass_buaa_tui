# ChangeLog

## Unreleased

### 修复

- 修复 WebVPN 模式下 iClass 登录失败并返回 `ERRCODE=106 / 用户不存在` 的问题。
- 原因：旧流程在 SSO 登录后直接把学号作为 `app/user/login.action` 的 `phone` 参数传给 iClass；真实网页会先进入 WebVPN CAS service，再访问 iClass `jumpMyCenter`，从 302 `Location` 中获取临时 `loginName`，最后用 `loginName` 调 iClass app 登录接口。
- 修复方式：VPN 登录入口改为浏览器实际使用的 `service=https://d.buaa.edu.cn/login?cas_login=true`；HTTP 客户端共享同一个 cookie jar；iClass 登录前用 no-redirect client 逐跳处理 `jumpMyCenter` 的 302 并解析 `loginName`。
- 验证：用真实 WebVPN 链路复现 `ticket -> wengine-vpn-token-login -> token-login -> /`，再验证 `jumpMyCenter -> loginName -> app/user/login.action` 能成功返回 iClass session。
