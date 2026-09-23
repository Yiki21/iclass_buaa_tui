# 让校内服务绕过代理直连

## 现象

开着 Clash / Mihomo 一类代理（尤其是 TUN 模式）时，北航的校内服务全部不可用：

```
[ERR] 登录失败: 网络异常，阶段: iclass_login_name
ERR  研讨室加载失败: 未获取到研讨室 SSO Token
```

而普通外网（`google.com`）正常。

## 最快的判断方法（实测）

**关掉 TUN，问题立刻全部消失。** 这是唯一一次改动就让所有校内服务恢复的操作：

```bash
# mihomo 的控制接口，PATCH 一下即可，不用重启
SOCK=$(ss -lxnp | grep -o '/tmp/mihomo-party-[0-9-]\+\.sock' | head -1)
curl -sS --unix-socket "$SOCK" -X PATCH \
  -H 'Content-Type: application/json' \
  -d '{"tun":{"enable":false}}' http://localhost/configs
```

关闭后立即验证：

| 主机 | 开 TUN | 关 TUN |
| --- | --- | --- |
| `cgyy.buaa.edu.cn` | `000` | `302` |
| `sso.buaa.edu.cn` | `000` | `302` / `200` |

**根因不是路由，是 DNS。** TUN 开启时 `fake-ip-filter` 没能拦住校内域名，于是它们被解析成 fake-IP：

```
# 系统解析器（实测）
cgyy.buaa.edu.cn -> 198.18.9.34     # fake-IP，不可路由
sso.buaa.edu.cn  -> 198.18.4.40     # fake-IP
ygdk.buaa.edu.cn -> 198.18.9.29     # fake-IP
```

`198.18.0.0/16` 是 TUN 的虚拟段，校内根本没有这些地址。所以「把 `10.0.0.0/8` 排除出 TUN」只解决了一半问题——域名先被解析成 fake-IP，就永远轮不到那条路由规则生效。这也解释了为什么**手工钉住正确 IP 仍然失败**。

## 最容易走错的一步

改 `~/.config/mihomo-party/mihomo.yaml` 或 `work/config.yaml` **都不持久**。实测：`controlDns` 被改回 `false`、`route-exclude-address` 被重置，即使没有更新订阅。mihomo-party 的 GUI 状态才是权威，它会在运行时重新生成配置。

持久做法只有两个：

1. 在 GUI 里启用 `~/.config/mihomo-party/override/buaa-direct-dns.js`（JS 覆写，注册在 `override.yaml`），或
2. **直接关掉 TUN / 改用规则模式**——实测最省事，且不需要理解 override 机制。

## 原因

校内服务用的是**校内地址**，而代理把整个 `10.0.0.0/8` 也接管了：

```bash
ip route get 10.20.11.166
# 走代理时： 10.20.11.166 via 198.18.0.2 dev Mihomo
# 正常时：   10.20.11.166 via 10.135.0.1 dev wlp0s20f3
```

北航的域名解析结果：

| 域名 | 校内地址 |
|---|---|
| `iclass.buaa.edu.cn` | 10.20.11.166 |
| `d.buaa.edu.cn` | 10.254.72.31 |
| `spoc.buaa.edu.cn` | 10.212.30.71 |
| `judge.buaa.edu.cn` | 10.251.0.206 |
| `booking.lib.buaa.edu.cn` | 10.254.9.112 |
| `app.buaa.edu.cn` | 10.200.21.8 |
| `byxt` / `gsmis` / `ygdk` | 10.254.15.1 |

流量进了代理隧道之后，TLS 握手会被直接断开（`SSL routines::unexpected eof`），于是表现为「网络异常」。

即便代理规则里有 `IP-CIDR,10.0.0.0/8,DIRECT` 也没用——如果开了 fake-ip，DNS 返回的是 `198.18.x.x`，这条 IP 规则永远不会命中。

## 修复

### 方案一：把校内网段排除出 TUN（最彻底）

在 mihomo 配置里设置：

```yaml
tun:
  route-exclude-address:
    - 10.0.0.0/8
    - 172.16.0.0/12
    - 192.168.0.0/16
```

`route-exclude-address` 只在核心启动时生效，改完需要**重启核心**，热重载不够。

mihomo-party 用户：在「覆写」里放一个 JS 覆写（`override/*.js`），或直接在 TUN 设置面板里填「路由排除地址」。覆写的好处是订阅更新时不会被覆盖。

```js
function main(config) {
  config.tun = config.tun || {}
  const excluded = new Set(config.tun['route-exclude-address'] || [])
  for (const r of ['10.0.0.0/8', '172.16.0.0/12', '192.168.0.0/16']) excluded.add(r)
  config.tun['route-exclude-address'] = Array.from(excluded)
  return config
}
```

### 方案二：域名规则放到 `prepend`

如果不想排除整个网段，至少要让域名规则先生效。注意**必须放在 `prepend`**：

```yaml
prepend:
  - DOMAIN-SUFFIX,buaa.edu.cn,DIRECT
  - DOMAIN-SUFFIX,buaa.team,DIRECT
append: []
```

`append` 的条目会排在配置自带的 `MATCH` 规则**之后**，而 `MATCH` 匹配一切，所以追加的规则实际上永远不会命中。很多订阅配置的 `rules` 末尾就是 `MATCH,xxx`。

### 方案三：DNS 也交给系统解析

校内域名只能由校内 DNS 正确解析，公共 DoH 会返回错误地址（实测 `iclass.buaa.edu.cn` 被解析成 `106.39.41.15`，`spoc.buaa.edu.cn` 被解析成 `0.0.0.3`）：

```yaml
dns:
  nameserver-policy:
    "+.buaa.edu.cn": [system]
    "buaa.team": [system]
```

## 验证

```bash
# 1. 路由不再走代理
ip route get 10.20.11.166
# 期望：via 10.135.0.1 dev wlp0s20f3（不是 dev Mihomo）

# 2. 各服务连通性
iclass_buaa_tui doctor
# 期望：每项都 ok=yes
```

如果 `ip route get` 仍然显示 `dev Mihomo`，说明排除规则没生效：确认改的是**当前生效**的配置文件、并且重启过核心。

## 备注

- 方案一是根治；方案二、三只是缓解，因为只要流量还进隧道，代理自身的出站路径仍可能出问题。
- 本项目的 `doctor` 命令会给出每条上游的解析地址与状态，是排查这类问题最快的入口。
