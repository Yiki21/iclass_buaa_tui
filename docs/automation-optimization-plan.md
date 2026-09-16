# 自动化稳定性优化计划

## 目标

提高自动签到在真实调度环境中的成功率、可诊断性和资源使用效率，同时保持现有课程筛选规则、签到语义和跨平台调度器兼容性。

自动化范围只包含：

- iClass 课程签到
- BYKC 课程签到/签退
- `list-today`、`plan`、`sign`
- Linux systemd user、macOS launchd、Windows schtasks 调度

不在本计划中加入作业提交、评教、预约或其他不可逆课程外操作。

## 现状基线

- `plan` 每次调度都会重新创建客户端并登录。
- iClass 和 BYKC 目标获取采用串行链路，一个来源失败可能阻断另一个来源。
- 每次签到重试都会重新登录并重新创建客户端。
- 重试使用指数退避，但目前不知道课程窗口剩余时间。
- 详情请求已经并发，但没有显式并发上限。
- planner 使用机器本地时区判断时间窗口。
- 调度器有健康检查和结构化事件日志，但没有完整的单次运行摘要。
- 直连和 VPN 都使用统一认证密码；配置权限检查需要覆盖两种模式。

## P0：先提高正确性和成功率

### P0.1 统一认证密码权限

**目标**：直连和 VPN 模式只要配置了密码，就要求配置文件权限为 `600`。

**改动**：

- 移除权限检查对 `use_vpn` 的依赖。
- 保留无密码公共配置可以使用普通权限的行为。
- 更新错误提示、README 和示例配置说明。

**验收**：

- 直连 + 密码 + 非 `600` → 配置加载失败。
- VPN + 密码 + 非 `600` → 配置加载失败。
- 无密码配置 → 不因权限失败。

### P0.2 planner 周期复用登录会话

**目标**：一次 `plan` 周期最多建立一次统一认证/iClass 会话。

**改动**：

- 引入周期级 `PlannerSession`。
- iClass 和 BYKC 目标获取共享同一个 `IClassApi` 和 `Session`。
- 单次签到重试优先复用会话；明确发现认证失效时才刷新会话。
- `list-today` 和 `plan` 共用目标获取上下文。

**验收**：

- 启用 iClass + BYKC 时一次周期不重复 SSO 登录。
- iClass 获取失败不影响 BYKC 独立获取。
- 测试可验证登录调用次数和来源结果。

### P0.3 iClass/BYKC 独立失败

**目标**：一个上游系统失败时，另一个系统仍然可以完成目标获取和签到。

**改动**：

- 目标获取返回按来源拆分的结果。
- 输出每个来源的状态、目标数和错误摘要。
- 最终退出码仍反映是否存在失败，但不丢弃已经成功获取的目标。

**验收**：

- iClass 成功、BYKC 失败 → iClass 目标仍执行。
- iClass 失败、BYKC 成功 → BYKC 目标仍执行。
- 两者都失败 → 返回汇总错误。

## P1：控制重试、并发和调度竞争

### P1.1 时间窗口感知重试

- 增加最大单次等待和最大总重试时长。
- 下一次退避不能超过课程结束时间。
- 对账号、验证码、缺少目标 ID 等错误立即停止。
- 对网络错误、超时、5xx 使用带抖动的退避。

### P1.2 planner 单实例锁

- 账号/配置维度建立跨平台单实例锁。
- 上一轮仍在运行时，本轮输出 `already-running` 并安全退出。
- 异常退出后锁可恢复，不留下永久阻塞。

### P1.3 上游请求并发上限

- iClass 课程详情请求使用 `Semaphore` 限制并发，默认 8。
- 将并发数配置化，但设置合理上下限。
- 失败时保留已成功解析的数据，不因单个详情请求失败丢弃整个列表。

### P1.4 固定业务时区

- 自动化时间判断统一使用 `Asia/Shanghai`。
- 日志同时输出业务时间和本机时间（如有差异）。
- 覆盖夏令时/非中国时区测试。

## P2：可观测性和策略优化

### P2.1 单次运行摘要

- 每次 planner 生成 `run_id`。
- 记录目标数、待签到数、成功数、已签到数、等待数、过期数、失败数和耗时。
- `autologin-status` 显示最近一次运行摘要和最近失败原因。

### P2.2 目标紧迫程度排序

- 优先处理距离窗口结束最近的目标。
- 同一课程的签到和签退保持稳定顺序。
- 失败重试不能长期阻塞即将结束的其他课程。

### P2.3 更严格配置校验

- `advance_minutes >= 0` 且不超过合理上限。
- 重试间隔、重试次数和最大退避时间有上限。
- 开启 BYKC 时明确要求 VPN。
- 启动日志记录有效配置摘要，但不记录密码。

## 执行顺序

1. P0.1：密码权限检查
2. P0.2：周期级会话复用
3. P0.3：来源独立失败
4. P1.1：时间窗口感知重试
5. P1.2：单实例锁
6. P1.3：请求并发上限
7. P1.4：固定业务时区
8. P2：运行摘要、排序和配置校验

## 每轮完成标准

每个切片必须同时完成：

- 实现
- 单元测试或 fixture 测试
- `cargo fmt --check`
- `cargo check`
- `cargo test`
- 更新本文件的完成状态和剩余风险

## 当前进度

- [x] P0.1 统一认证密码权限
- [~] P0.2 planner 目标发现阶段复用登录会话；签到执行阶段仍待复用
- [x] P0.3 iClass/BYKC 独立失败
- [~] P1.1 已限制最大退避并约束课程截止时间；总时长和随机抖动待补
- [x] P1.2 planner 单实例锁
- [x] P1.3 上游请求并发上限
- [ ] P1.4 固定业务时区
- [x] P2.1 单次运行摘要
- [x] P2.2 目标紧迫程度排序
- [ ] P2.3 更严格配置校验

## 2026-09-16 BYKC 423 与 iClass STATUS=2

报告现象（测试账号，已脱敏）：

- BYKC：`VPN 登录失败，仍停留在统一认证页面`，最终 URL 落在 WebVPN `/login`，响应体为 `status:423, error:Locked, Access Denied`。
- iClass：`登录成功，但拉取课程失败: 课程列表: iClass API 返回错误: {"STATUS":"2"}`。

根因：

1. BYKC 在统一认证会话已建立之后，又用独立的 Cookie Jar 提交了一次账号密码。学校网关把重复的凭据提交判定为风险并返回 `423 Locked`。上游 UBAA 的 BYKC 实现只在已验证的会话上读取 CAS 重定向里的 `token`，不会重复提交密码。
2. `extract_bykc_token` 只识别 `?token=`。当 `token` 出现在其他参数之后（`&token=`）时解析为空，代码会退回到再一次的凭据提交。
3. iClass 的 `ensure_status_ok` 把任何非 `0` 的 `STATUS` 都当作接口错误，于是 `STATUS=2`（该学期没有数据）被报成「拉取课程失败」，即使合并结果里另一个来源已经有课程。

修复：

- BYKC `ensure_login` 先读现有会话的 CAS 重定向取 `token`，不再默认重复提交密码；只有在会话确实失效时才重新建立一次会话。
- `extract_bykc_token` 支持任意查询参数位置的 `token`，并去掉 `#fragment`。
- 新增 `is_locked_response`，在 BYKC 登录和 `vpn_login` 两处识别 423/Locked，返回明确的「暂时锁定，稍后重试」提示。
- planner 的重试分类把 423/Locked/Access Denied 视为不可重试，避免继续冲击已锁定的账号。
- iClass 新增 `status_means_no_data`，`STATUS=2` 按「该学期无数据」处理，不再当作接口失败。

验证：

- 新增 4 个测试：BYKC token 参数位置、423 锁定识别、`STATUS=2` 语义、planner 锁定不重试。
- `cargo fmt --check`、`cargo check`、`cargo test` 全部通过（31 passed）。

未验证：

- 真实账号在本机复测尚未执行，因为读取仓库外 `~/.config/iclass-buaa/config.toml` 被工具权限阻止，未擅自绕过。
- 如果学校侧仍在锁定窗口内，复测前需要等待锁定过期。

### 后续确认：cookie 会话未共享（真正根因）

iClass 侧已由真实账号确认修复。BYKC 仍未确认，但对照上游 UBAA 实现后发现了更直接的根因：

- UBAA 的 BYKC 客户端复用共享的 `LocalUpstreamClientProvider.shared()`，也就是同一个已认证的 cookie 存储。
- 我们的 `BykcApi` 自己新建了空的 `Jar`。它的 CAS 请求因此没有 SSO 会话，拿不到 `token`，然后落到「重新提交一次密码」，换来 423。

修复：

- `IClassApi` 现在持有自己的 cookie jar，并通过 `session_cookie_jar()` 暴露。
- `BykcApi` 改为 `with_cookie_jar(...)`，与主登录共享同一个 jar。
- 删除了会自行建 jar 的 `BykcApi::new`，防止后续再引入同一个错误。

验证：

- `cargo fmt --check`、`cargo check`、`cargo test` 全部通过（31 passed），无编译警告。

仍未验证：

- 真实账号下的 BYKC 课程列表与已选课程拉取尚未复测，需要你在授权配置读取后或自行运行一次确认。

### 后续确认（二）：重复的 CAS 登录实现被删除

cookie 共享修复后，BYKC 又报出另一个错误：

```
博雅操作失败: 无法从 SSO 登录页面解析登录表单，最终 URL:
https://d.buaa.edu.cn/https/77726476706e69737468656265737421e5f40f9e3231691e7b0c9ce29b5b/,
页面线索: title=<none>, markers=portal
```

把加密后的主机段解出来是 `uc.buaa.edu.cn`：拿回来的是统一认证门户（SPA），不是 CAS 表单。也就是说，`bykc/helpers.rs` 里那套重复的 CAS 登录在重定向后落到门户页，再去抓表单就必然失败；而它的孪生 no-redirect client 又看不到重定向目标里的 `token`，于是两边都退回到重新提交密码（换来 423）。

上游 UBAA 的 BYKC 从不自己登录：它在已认证的共享会话上读 `token`，然后请求 `cas-login?token=` 激活。

修复：

- 删掉 `bykc/helpers.rs` 里重复的 `vpn_login`、`resolve_login_form_action`、`build_cas_login_form`、`summarize_vpn_login_page`。CAS 登录只由 `iclass/api.rs` 负责一份。
- 新增 `activate_bykc_session`，对齐 UBAA 的 `cas-login?token=` 激活。
- `ensure_login` 改为跟随重定向，从最终 URL 读 `token`，激活会话，全程不再提交密码；取不到 token 不再当作致命错误。
- 删掉因此失效的 no-redirect client、`login_client` 和无用导入。

验证：

- `cargo fmt --check`、`cargo check`、`cargo test` 全部通过（31 passed），无编译警告。
- 本次净删 352 行、新增 50 行。

仍未验证：

- 真实账号下的 BYKC 仍未复测。这是三次修改中唯一还没被真实环境确认的一条。
