> **文档状态：设计存档，不是当前说明。**
>
> 这份文档是内核在开发过程中写下的完整设计记录（数据流、四级路由决策、正确性约束、
> 构建与测试），保留它的原因是里面的**推理过程**——为什么这样选、以及为什么那个看起来
> 更简单的替代方案是错的——比结论本身更有价值。
>
> 它**不保证与当前代码同步**，其中两类内容明确已经过时，阅读时请以代码为准：
>
> - **规则源地址**：文中出现的 `abhuang.dpdns.org/...` 已经下线。当前默认规则源见
>   应用的「规则 → 数据源」，或 `android/app/src/main/java/dev/detour/core/` 下的规则仓库实现。
> - **参数与默认值**：文中写死的超时、并发上限等数值后来被拆成多个语义不同的设置项
>   （例如连接超时按「有没有备选地址」拆成两个），以 `core-rs/crates/watt-stack/src/config.rs` 为准。
>
> 面向使用者的介绍在仓库根目录的 [`README.md`](../README.md)。

---

# watt — Android 版 Watt Toolkit 网络核心

参考 [BeyondDimension/SteamTools](https://github.com/BeyondDimension/SteamTools)（Watt Toolkit）的代理设计，为 Android 重新实现的**免 Root 全流量用户态网络内核**。

规则来自 `https://abhuang.dpdns.org/1`（UsbEAm host 记录），**默认只拉这一套**；
`https://abhuang.dpdns.org/2` 是 Steamcommunity 302 劫持块，需要时由操作者显式 `--hosts-url` 追加（它不是子集，
但它的地址全是 `127.0.0.1`，本内核不中继环回——见 5.7）。旧的 `https://abhuang.dpdns.org/rules` 与
`https://abhuang.dpdns.org/hosts?all=1` **已下线**（现在返回 nginx 404），实测见 5.5 与 5.7。

---

## 0. 这份规则文档里有什么

`version=1.0.47`，316 个条目 / 2452 个域名 / 24435 个具体 IP / 119 个占位符条目，10 个分组：

| 分组 | 条目 | 域名 | IP |
| --- | ---: | ---: | ---: |
| For Web | 81 | 874 | 3879 |
| In Game | 60 | 293 | 7670 |
| For Tools | 41 | 254 | 2490 |
| For Service | 39 | 363 | 4511 |
| Microsoft Live | 33 | 195 | 3941 |
| developer | 26 | 171 | 1122 |
| XBOX/Microsoft Store | 15 | 35 | 1454 |
| Other Platforms | 14 | 210 | 1592 |
| CDN for open-source | 6 | 25 | 141 |
| Academic | 2 | 34 | 2 |

**Steam 相关域名不在这里，这是设计如此。** 文档顶层的 `filtered` 列出了 12 个被排除的分组，原因全部是 `PC_ONLY`：Keylol、Steam、EA Desktop、Origin、Ubisoft Connect、GOG、Battle.net、Epic Games、Rockstar Launcher、Riot、Reverse proxy、Reverse proxy(platform)。

换句话说，**这份文档本身就是面向非 PC 平台（也就是 Android）的子集**。所以「Android 版 Watt Toolkit」在这个规则源下的实际行为是：加速 GitHub、Microsoft/XBOX、学术资源、开源 CDN 与各类 Web/工具服务，而 Steam 商店与社区流量直连。

如果目标是 Steam 本体加速，需要换一个不过滤 `PC_ONLY` 的规则源——内核本身不关心规则来自哪里，换 URL 即可。

---

## 1. 为什么要自己实现

SteamTools 的网络加速在 Windows 上依赖 **WinDivert**（Windows 专属内核驱动）做流量重定向，Android 侧无法复用；它的反向代理基于 .NET 的 `YARP.ReverseProxy`，也绑定在 .NET 运行时上。所以这里只参考它的**设计**——「按域名把流量导向规则给出的 IP」——核心用 Rust 重写。

Android 侧的约束决定了整体形状：

| 约束 | 后果 |
| --- | --- |
| 免 Root | 只能用 `VpnService` + TUN，不能改 iptables |
| TUN 交出的是**原始 IP 包** | 主机 socket 收不到 IP 包，必须用**用户态协议栈**终结客户端的 TCP 连接，再用真实 socket 转发字节 |
| `VpnService.protect(fd)` 必须在上游 `connect` **之前**调用 | 内核自己的出站连接不能被自己的 VPN 捕获，否则形成死循环。因此上游 socket 用 `libc` 手写而非 `std::net` |
| 不解密 HTTPS | 只做 TLS 直连与域名/IP 分流，HTTP CONNECT 只是众多被透明转发的协议之一 |

---

## 2. 数据流

```
应用（普通 socket）
   │  connect 203.0.113.10:443
   ▼
Linux 路由表 ──▶ TUN 设备 watt0 ──read──▶ 内核
                                          │
                       ┌──────────────────┼──────────────────┐
                       ▼                  ▼                  ▼
                    TCP (smoltcp)      UDP (NAT 表)        ICMP
                    终结连接            按五元组转发        计数后丢弃
                       │                  │
                       ▼                  ▼
                  真实 socket ──────▶ 规则给出的 IP
                       │
                       ▼
              写回 TUN ──▶ 客户端看到「203.0.113.10 回复了我」
```

关键点：**客户端始终以为自己在和 203.0.113.10 说话**。内核对外拨号的是规则里的地址，回包再从**客户端当初拨号的那个地址**发出。

---

## 3. 代码结构

```
core-rs/
├── crates/watt-rules/     规则文档 → 可查询的路由表
│   ├── model.rs           serde 模型（id/port 同时接受字符串与数字）
│   ├── domain.rs          域名规范化、后缀匹配（避免 notexample.com 命中 example.com）
│   ├── ruleset.rs         domain_index + ip_index 双索引
│   ├── selector.rs        多 IP 排序：冷却 > RTT > 连续失败数 > 文档顺序
│   ├── router.rs          plan / plan_for_ip：四级策略的决策入口
│   ├── cache.rs           磁盘缓存（原子替换）
│   ├── update.rs          规则生命周期：内置 / 缓存 / 下载 / 指定
│   └── builtin.rs         内置兜底文档（只含占位符，不含具体 IP）
│
├── crates/watt-net/       设备抽象
│   ├── tun.rs             TUN 创建/接管、`IfReq` 40 字节断言
│   └── device.rs          `PacketDevice` trait + 测试用内存设备
│
├── crates/watt-stack/     网络内核
│   ├── engine.rs          主循环：poll → 收包 → 两个中继 → 回写
│   ├── tcp.rs             用户态 TCP（smoltcp）+ 监听器池
│   ├── udp.rs             NAT 表 + DNS 拦截与观测
│   ├── dns.rs             DNS wire 编解码
│   ├── planner.rs         四级路由决策
│   ├── packet.rs          手写 IP/TCP/UDP/ICMP 编解码
│   ├── upstream.rs        libc 非阻塞 socket + `Protector`
│   ├── poller.rs          poll(2) 封装
│   └── config.rs          配置与计数器
│
└── crates/watt-daemon/    命令行驱动
    ├── main.rs            主循环、信号、计数器输出
    ├── options.rs         参数解析
    └── fetcher.rs         curl 取规则

└── crates/watt-ffi/       C ABI：安卓壳层与内核之间的接缝
    ├── lib.rs             extern "C" 入口 + Protector 回调
    └── include/watt_ffi.h 给 JNI 的头文件
```

约 15,200 行，225 个测试。

---

## 4. 四级路由决策

按优先级从高到低：

1. **静态改写**（`DestinationOverride`）——显式运维意图，最高优先级。
2. **规则地址**——域名命中规则，且 DNS 观测已建立「地址 → 域名」映射。
3. **按地址归属规则**——客户端用了 DoH，内核只看到 IP；此时按规则的 `ips` 反查。
4. **直连**——没命中任何规则。

**客户端端口始终保留**：规则的 `port` 字段描述的是「这个服务在哪个端口」，不是「把它改到哪个端口」。改写端口会让 TLS SNI 与实际目的地不一致。

### 规则里的占位符

上游文档的 `ips` 里既有真实地址，也有 `{Cloudflare}` / `{Cloudfront}` 这类占位符。占位符**不猜**：它进不了 `RuleAddresses` 策略，DNS 查询会被转发给真实解析器，答案再被观测记录，于是后续连接走第 3 级策略。

---

## 5. 四个非显而易见的正确性要求

### 5.1 每个初始 SYN 都必须有自己的监听 socket

smoltcp 在**没有任何 socket 接受**一个 TCP 包时，会**自己回一个 RST**（`iface/interface/mod.rs` 的 `process_tcp`）。客户端把 RST 报成 `Connection refused`——也就是「服务器挂了」的样子。

所以监听 socket 不是「优化」，是正确性要求。`listener_pool` 是**静止时保留的备用数**，`max_listeners_per_endpoint` 是**突发上限**；补池（`top_up_listeners`）只填到静止水位，为 SYN 预留（`reserve_listener`）才可以长到上限。

> 这个缺陷在单元测试里完全看不出来：3 个并发连接一切正常，200 个并发连接只有 5 个建立成功。

### 5.2 流表满了要**淘汰**，不是拒绝

UDP 流表有上限（默认 1024）。表满时如果直接拒绝新流，客户端无法区分「表满了」和「服务器挂了」；更糟的是，用 ICMP port-unreachable 拒绝等于**撒谎**——那个端口上明明有服务。

正确行为是 NAT 的标准行为：**忘掉最久未用的那条**（`make_room`）。代价几乎为零，因为 UDP 对端只会看到一个新的源端口。

但淘汰要带**空闲下限**（空闲超时 / 4）：一条刚刚发出请求、正在等回包的流如果被淘汰，它的回包会落在已经关闭的 socket 上——新老两条报文一起丢。所以只淘汰真正已经结束的，剩下的宁可拒绝。

> 这个缺陷也是压力测试才发现的：每轮 200 条流、每轮约 5 秒，而 UDP 空闲超时 60 秒——第 6 轮正好撞上 1024。客户端 200 个**未 connect** 的 UDP socket 收不到 ICMP 错误（Linux 不把 ICMP 错误报给未连接 socket），于是每个各等 15 秒，**整个测试挂住 50 分钟**。

### 5.3 内核自己的出站 socket 必须被豁免，否则它会中继自己

这是本项目的**头号陷阱**，也是唯一一个能让内核彻底失控的缺陷。

内核的上游 socket 和所有其它流量走同一张路由表。所以当客户端连接一个被中转的地址时：

```
客户端 ──SYN──▶ 隧道 ──▶ 内核接受，向上游 connect 同一个地址
                            │
                            ▼
                    这个 SYN 被自己的路由送回隧道
                            │
                            ▼
                    内核把它当成新客户端，再中继一次 …… 无界循环
```

实测：**一次**客户端连接尝试，0.7 秒内 `tcp_open=555`，描述符 4 → 553。更糟的是客户端收到的是「连接成功」——因为内核应答了自己的 SYN。

**Android 上 `VpnService.protect(fd)` 正是为此存在**，`Protector` trait 就是它的挂载点，且必须在 `connect` **之前**调用（`upstream.rs` 有测试锁住这个顺序，并带阳性对照）。

**纯 Linux 主机没有 protect**，所以内核提供 `MarkProtector`：给上游 socket 打 `SO_MARK`，再由主机把它路由到一张不含隧道路由的表——和 `wg-quick` 处理 WireGuard 自己报文的做法完全一样：

```bash
ip route add default via <网关> dev <物理网卡> table 5754
ip rule add fwmark 0x5754 lookup 5754 pref 100
watt-daemon --tun watt0 --protect-mark 0x5754 ...
```

`scripts/probe-selfloop.sh` 是这条缺陷的复现与验证：不带 mark 时看门狗会在一秒内触发，带上 mark 后 `tcp_open=1`、描述符稳定。

### 5.4 Android 的免 Root 首选路径是 CONNECT，不是「内核自己启动一个 TLS 服务器」

Android 版的核心必须遵守原始设计：**客户端自己建立 TLS，核心只转发加密字节**。因此第一条可在模拟器上验证的产品路径是 HTTP CONNECT：

```text
Android 应用 / curl -x 127.0.0.1:1080 https://listed.example/
        │  CONNECT listed.example:443
        ▼
Rust watt-daemon --proxy-listen 127.0.0.1:1080
        │  只读取 CONNECT 的域名和端口
        │  规则命中 → 连接规则 ips 中排序后的地址
        │  未命中 → 403；不偷偷直连
        ▼
真实网站（TLS 端到端，核心看不到明文）
```

启动：

```bash
watt-daemon --proxy-listen 127.0.0.1:1080 --rules-file rules.json
curl -x 127.0.0.1:1080 https://nikke-en.com/
```

这个模式不创建 TUN、不改路由、不需要 Root，也不启动本地 TLS 服务器。它是「先制作内核」阶段最符合规格的真实产品闭环：域名匹配、规则 IP 选择、HTTPS CONNECT、TLS 直连、未列域名拒绝。带 `{Cloudflare}` 等占位符的已列域名会走系统 DNS；完全不在 JSON 里的域名返回 `403 Forbidden`。

真实站点验证结果：`nikke-en.com` 通过 CONNECT 返回 `HTTP 200`、87578 字节、`ssl_verify_result=0`；`example.com` 不在规则中，代理返回 `403`，不会直连。测试没有启动任何本地 TLS 服务器，也没有伪造上游站点。

在 Windows 雷电 Android 14（SDK 34，x86_64，root adbd）模拟器上，使用 NDK r30 交叉编译出的 Android 二进制实际运行在设备内：通过 `adb forward` 暴露设备上的 `--proxy-listen 0.0.0.0:18080`，Windows `curl` 只作为外部真实客户端。结果仍然是 `nikke-en.com → HTTP 200 / 87578 B / ssl_verify_result=0`，`example.com → CONNECT 403`。这验收的是免 Root CONNECT 内核路径；最终 Android 产品形态仍由 Kotlin `VpnService` 持有 TUN，并调用 `protect(fd)`，不应把「在内核里启动 TLS 服务器」当作实现方式。

### 5.5 上游换成了两套规则集，按 hosts 文本拉取

> **端点变更（实测 2026-09-26）。** 旧的 `https://abhuang.dpdns.org/rules` 与
> `https://abhuang.dpdns.org/hosts?all=1` 现在都返回 **nginx 404**，已下线。
> 替代它们的是两个**不同规则集**的短路径：
>
> | 路径 | 规则集 | 实测 |
> |---|---|---|
> | `/1` | UsbEAm host records | 15971 条记录 / 5225 域名 / 15952 个地址（19 跳过） |
> | `/2` | Steamcommunity 302 劫持块 | 862 条记录 / 862 域名，地址全是 `127.0.0.1` |
>
> 两者都返回 `Content-Type: text/plain; charset=utf-8`，且**各自**都能用
> `?format=json` 渲染 —— 但那个 JSON 是**另一种 schema**（见 5.7 的坑），
> 内核读不了，所以本篇一律按 hosts 文本拉取。

`/1` 与 `/2` **不是子集关系**，而且「hosts 还是 JSON」在这里是**格式**维度，
与「`/1` 还是 `/2`」这个**规则集**维度互相独立 —— 旧文档把两者说成「一份 hosts、一份 JSON」是错的。

`/1` 提供全部可拨号地址（5225 域名、15952 个地址，其中 5225 个域名有多于一个地址）。
`/2` 是劫持块，862 个地址**全是 `127.0.0.1`** —— 在 watt 里这类目标**不被允许中继**
（见 5.7），所以 `/2` **不进默认源**：用它只会把 675 个本可直连的域名变成 RST。默认只拉 `/1`：

```bash
watt-daemon --proxy-listen 127.0.0.1:1080 --hosts-file hosts.txt
watt-daemon --proxy-listen 127.0.0.1:1080            # 默认源即 /1
```

hosts 语法就是普通 hosts 文件，外加一条上游约定：`# === [分组名] ===` 开启一节，所以分组会保留到编译后的规则集里，而不是塌成一个大桶。`0.0.0.0 name` 这类屏蔽行会被丢弃——在 hosts 文件里它的意思是「不要解析」，把它当成目的地等于把流量送进黑洞。

**已确认的救援案例。** 在雷电 Android 14 模拟器上，用 hosts 作为规则源：

```text
nikke-en.com
  直连：  000（curl exit 35，TLS 连接失败）   连续 3 次一致
  经内核：HTTP 200，87578 字节，证书校验通过  连续 3 次一致
```

hosts 给这个域名的是 `43.175.120.74`，直连时系统 DNS 解析到的地址无法完成 TLS。这就是规则源存在的意义：**不是加速，是换一条能走通的地址。**

反过来的情形同样存在，而且必须承认：`github.com` 在 `/1` 里给三个地址，其中 `51.142.105.107` 从这段网络不可达（另两个是 `20.12.240.255`、`20.218.253.22`）。规则源的质量和它给的地址是否可达是两件事。

因此**规则地址是偏好，不是唯一出路**。规则给出的地址全部连接失败时，内核回退到系统解析再试一次，只有两条路都不通才返回 `502`：

```text
规则候选（按规则排序）──失败──▶ 系统解析候选 ──失败──▶ 502 Bad Gateway
        │                              │
        └──────第一个成功即建连─────────┘
```

这个回退是必需的，否则一条过期的规则会把它本来承诺服务的域名变成一次故障：`github.com` 修复前走内核 `000`、直连 `200`，修复后走内核 `200`。回退发生时内核会在日志里写一行

```text
proxy: rule addresses for github.com all failed; served via system resolution
```

这行是规则集需要刷新的信号，不是错误。**选中的地址仍然必须先命中规则**——未列域名在规划阶段就被 `403` 挡掉，回退路径根本不会被执行到。

### 5.6 DNS 应答必须自己装得进一个报文，而且不能靠「截断」来装

一条规则可以给一个域名列几百个地址——上游文档里最多的一条是 971 个。应答把这些地址逐条写回客户端时：

- 每条 A 记录如果不做**名字压缩**，就要重复一遍完整域名，占 `12 + 域名长度 + 4` 字节而不是 16 字节；
- 超过 MTU 时置 `TC` 位、清空 answers，是 RFC 1035 的做法，但它的含义是「请改用 TCP 重试」——**而这个内核不提供 TCP DNS**，应答根本没有离开过设备。

两者叠加的后果是：**规则里地址越多，客户端拿到的越少，超过约 50 个就一个都拿不到。**

实测（MTU 1500，可用 1452 字节）：

| 域名 | 规则里的 v4 地址 | 修复前 | 修复后 |
|---|---:|---|---|
| `www.xbox.com` | 43 | 43 条 / 1234 B | 43 条 / 718 B |
| `nikke-en.com` | 52 | **0 条（`tc=1`）** | 52 条 / 862 B |
| `c.fivem.net` | 99 | **0 条（`tc=1`）** | 88 条 / 1437 B |
| `d2.baidupcs.com` | 114 | **0 条（`tc=1`）** | 88 条 / 1441 B |

按整份文档算：**1274 个可重定向域名里 1066 个（84.1%）的应答曾经会超出一个报文**，也就是说客户端拿到的是空答案。

两处修复：

1. `Message::encode` 现在做名字压缩（RFC 1035 §4.1.4）——同一名字只写一次，后续记录写两字节指针；
2. 仍然装不下时，`build_response_fitting` **裁剪**到装得下的条数，而不是置 `TC`。地址已按选择器排过序，留下的就是最该留下的。

顺带修掉一个说谎的计数器：`dns_answered_locally` 原先在**发出报文之前**就自增，于是截断的应答也被算成「已应答」。现在只有真的发出去了才计数，另加 `dns_trimmed` 记录被裁剪的次数——非零不是故障，但意味着客户端只看到了一个子集。

`dns.rs` 里三条测试锁住这些行为：压缩后每条记录正好 16 字节、超限应答被裁剪且不带 `TC`、连一条都装不下时返回 `None`。

### 5.7 两套规则集 `/1` 与 `/2`，默认只用 `/1`（`/2` 是劫持块）

上游发布**两套规则集**（`/1`、`/2`），**互不为子集**，实测：

| | 域名 | 地址 | 多地址域名 |
|---|---:|---:|---:|
| UsbEAm（`/1`） | 5225 | 15952 | **5225** |
| S302（`/2`） | 862 | 862 | **0** |
| **合并** | **5900** | — | **5225** |

**默认只拉 `/1`，不拉 `/2` —— 这是刻意的。** `/2` 的 862 个地址**全是 `127.0.0.1`**，
而规则**不允许**中继环回目标：关掉它的是 `Planner::can_relay`（`watt-stack/src/planner.rs`，
对非 override 的环回目标返回 false），`tcp.rs` 随即 `socket.abort()` 并记一次
`tcp_flows_rejected`（发 RST）。注意不是 `is_blocked_target` —— 那个只在 `tcp.rs` 遍历
候选尾部的 `decision.alternatives`，从不看 `decision.target`。

所以把 `/2` 并进来不是「多 862 个域名」，而是**弄坏**它们。实测（合并两源后统计）：

| | 值 |
| --- | ---: |
| 合并域名 | 5905 |
| 其中有真实 IP 的 | 5230 |
| **只有 `127.0.0.1` 的** | **675（全部 `/2` 独有）** |

这 675 个本来不在规则里，会**直连并正常工作**；并入 `/2` 后编译成单个 `127.0.0.1`，
被 `can_relay` 拒绝 → RST。环回地址在 `rules-puller` 里是「指向本地反代」的劫持语义，
在 watt 里是没有意义的 —— 同一份数据，两套相反的含义。

裸跑一次即可（daemon 默认 hosts 源只有 `/1`）：

```bash
watt-daemon --check            # 默认抓 /1
```

实测（无 root，`--check` 不开隧道）：

```text
hosts    origin=https://abhuang.dpdns.org/1 lines=15971 addresses=15952 domains=5225 skipped=19
merge    sources=2 domains=5245 multi_address=5225
rules    origin=builtin version= entries=5245 domains=5245
report   groups=4 entries=5245 domains=5245 concrete_ips=1081 placeholders=20
```

`merge sources=2` 是「`/1` + 内置集合」两个来源（不是 `/1`+`/2`）；`domains=5245` 比 `/1` 单独的 5225
多 20 个，正是内置集合里 `/1` 没有的条目。**默认跑没有任何 `127.0.0.1` 条目**（实测
`login.steampowered.com` 落到 `strategy=direct`），这正是「不带 `/2`」的意义所在。

真的运行环回反代的部署，显式加第二个源：

```bash
watt-daemon --check --hosts-url https://abhuang.dpdns.org/2
```

`--hosts-url` **可重复**（与 `--rules-url` 的「后一个覆盖前一个」相反），所以多个源都能命名；
第一次出现的 `--hosts-url` 会丢掉内置默认。命名了 `--rules-file` / `--hosts-file` 时，内置默认
hosts 源也会被丢掉（「用这个文件」就是只用这个文件），除非又显式给了 `--hosts-url`。

> JSON 通道（`--rules-url`）默认是**空**的：公开端点里没有能提供 `groups` 形状的了，
> 所以它只吃缓存或内置集合。要让它工作，得指向自建 `rules-puller` 实例。

Android 端不经过 CLI，但结论一致：`RulesRepository` 的默认合并集也是 **`/1` 单独**，
daemon 与 App 对「默认规则」的定义必须一致。

> **不要用 `?format=json`。** 两个端点的 `?format=json` 发出的是另一种
> schema（`{"entries":[{"ip":..,"domain":..}]}`），而 `RuleDocument` 期望
> `{"groups":[{"entries":[{"domains":[..],"ips":[..]}]}]}`。因为每个字段都是
> `#[serde(default)]`，这个不匹配会被当成 `Ok(零条目的文档)`。**两条调用链后果不同**：
>
> - **daemon `--rules-url`**：`RuleSet::from_document` 对空集合返回
>   `Err(NoUsableEntries)`，所以下载**失败**、保留上一份/内置规则（实测：
>   `refresh failed, keeping builtin`，且不写 cache 目录）。响，但仍是错 —— 规则根本没更新。
> - **App / C ABI `merge`**：直接用 `parse_document`，不拒空集合，于是
>   862 条进、**0 条出、不报错**，隧道起来了却什么都不转发（见 `RulesRepository.kt`）。
>
> 两种都是坑。需要把规则喂给内核时，一律拉 **hosts 文本**。

合并必须发生在**编译之前**（`watt_rules::merge_documents`）：编译器对同一个域名只保留
第一个声明它的条目，所以「各自编译再合并」会把要保的地址丢掉。

**但地址多不等于能连上，而内核看不出哪一个是错的。** 这是本项目的边界，实测如下。

`github.com` 在合并后的候选表里含 `20.12.240.255`，它 TCP 立刻连通、TLS 立刻应答，但证书是别人的：

```text
curl -sv --resolve github.com:443:20.12.240.255 https://github.com/
  subject: CN=*.feedingbird.com
  subjectAltName does not match github.com
```

（注：本节原先引用的「4 个可用地址排在第 11 位往后」出自**旧的** hosts+JSON 合并集。
换成 `/1`+`/2` 后，`github.com` 实测只有 3 个候选 —— `51.142.105.107`、`20.12.240.255`、
`20.218.253.22`。「地址多不等于能连上」这个结论不变，但上面那组 4 个地址与排序位次
**在新规则源下已无法复现**，故不再列出具体数字，以免拿旧测量值当成现况。）

真正服务 `github.com` 的地址（旧实测，仅供说明现象）：`20.200.245.247` / `20.205.243.166` /
`20.207.73.82` / `20.233.83.145`，均 `exit=0`。这组值来自退出前的旧合并集，须用新源重新测量才能引用。

客户端于是报 `exit=60`。**内核无法避免这一步**：证书在 TLS 记录里，要读懂它就必须终结 TLS，
而这正是本项目放弃的东西。不要用「会话太短」「上行字节太少」之类的启发式去猜——
那会把正常的短响应也判成坏地址（见 `docs/健康判据方案.md` 第 2 节）。

能做的只有让**顺序**更可信，并让**并行的窗口**覆盖掉前面的死候选，这正是候选链的做法：

* **并行竞速窗口**（`RACE_WIDTH = 3`）：一个连接同时拨最多 3 个候选，第一个
  `SO_ERROR == 0` 的胜出，其余在飞候选立即关闭。`github.com` 的实测分布是「前 2 死、
  第 3 活」：串行要付两次 `first_connect_timeout`（各 3s），窗口下只要一个启动间隔。
  设成 `1` 即逐字退回旧的串行行为，是这一改动的回退开关。
* **竞速启动间隔**（`RACE_LAUNCH_INTERVAL = 250ms`，与旧的 `CONNECT_STAGGER` 默认值
  相同，实际取两者的较大值）：窗口每个 tick 最多启动一个候选，既让窗口尽快铺满，
  又不把整批 SYN 一次打出去。两者默认相同，所以 `max` 平时是空操作，字段报出的就是
  竞速真正使用的值（早先是 150ms vs 250ms，字段报 150 而实际跑 250）。
* **全局拨号并发上限**（`MAX_DIALING = 256`）：所有连接**同时在握手**的上游 socket
  总数上限。竞速把单流在飞 fd 从 1 抬到 ≤ `RACE_WIDTH`，没有这个上限，
  一次 SYN 风暴会逼近 `RLIMIT_NOFILE`。
* **单次上限**（`MAX_CANDIDATES = 12`）：`d2.baidupcs.com` 这类域名有 114 个地址，
  逐个给线程不是方案。失败的地址会在排序里下沉，下一次请求从没试过的开始。
* **窗口轮换预算**（`FIRST_CONNECT_TIMEOUT`，默认 3s）：窗口里还有冗余（有未启动候选，
  或窗口内还有别的在飞）时，一个候选只等 3s 就让位；当它成为唯一希望时切回完整的
  `CONNECT_TIMEOUT`，慢而诚实的服务器仍拿到全部预算。
* **首个字节超时**（`FIRST_BYTE_DEADLINE = 5s`）：连上却不出声的地址，
  在客户端自己的超时之前就被换掉，并把 `Silent` 记回排序。

竞速不会让「握手最快的」盲目获胜，因为窗口仍由既有的判据链把关：首批拨号由证书探针
（`verify_by`）门控，补位前先跳过判据表已否决的候选。竞速期间也**不向任何上游写客户端
字节**——它们先堆在缓冲区里，只有胜者确定后才冲刷，所以「客户端数据只进胜者」这条
不变量与「胜者产生前谁也不知道谁会赢」并不冲突。

三个开关都能从 App 的「高级：内核参数」里调，并经由 `kernelSettingsJson()` 透传到
`StackConfig`：`race_width`（1..4）、`race_launch_milliseconds`（0..1000）、
`max_dialing`（16..1024）。

---

## 6. 构建

WSL2 Ubuntu 22.04：

```bash
scripts/bootstrap-wsl.sh          # 一次性：apt 基础工具、rustup、Android target
scripts/cargo.sh build --workspace
scripts/cargo.sh test --workspace
scripts/cargo.sh clippy --workspace --all-targets -- -D warnings
```

`scripts/cargo.sh` 把 `CARGO_TARGET_DIR` 指到 `~/.cache/watt-target`，避免在 `/mnt/d`（9p）上写大量小文件。

### Android 目标

两个 Android target 都能编译（`cargo check` 覆盖全部四个 crate）：

```bash
scripts/cargo.sh check --workspace --target aarch64-linux-android
scripts/cargo.sh check --workspace --target x86_64-linux-android
```

这里踩过一个坑值得记下来：**`ioctl` 的请求码参数在 Linux 上是 `c_ulong`，在 Android 上是 `c_int`**。把 `TUNSETIFF` 之类的常量原样传进去，Linux 一路绿灯，Android 直接编译失败。`tun.rs` 里一律写成 `CONST as _`，让编译器按目标平台推断——请求码在两边都放得下 32 位，所以收窄是安全的。

`cargo check` 验证的是代码可编译；真正产出 `.so` 还需要 NDK 提供链接器，那是下一步的事。

---

## 7. 测试

### 单元测试

```bash
scripts/cargo.sh test --workspace     # 225 个
```

### 全部验证，一条命令

```bash
bash scripts/verify-all.sh
```

按"证明力从低到高"的顺序跑九个阶段（单元测试、clippy、脚本语法、真实流量 ×3、真实上游、通路、daemon、规则文档），最后打印一张汇总表。任何一阶段失败都会立刻停下来。

### 真实 TUN 上的端到端测试

这些脚本都需要 `sudo`（创建 TUN 需要 `CAP_NET_ADMIN`），它们自己提权。**调用 `wsl.exe` 时不要用管道**——PowerShell 会把 `tail` 之类当成 Windows 命令。

```bash
# 1. 通路：DNS 本地应答、并发 TCP、UDP 往返、ICMP 计数
bash scripts/tun-smoke.sh

# 2. 真实应用流量：curl 走真实 TLS 握手穿过隧道（本地服务器，不需要外网）
bash scripts/real-traffic.sh

# 3. 真实互联网：三个真实网站，真实证书校验（需要外网）
bash scripts/real-upstream.sh

# 4. 负载与耐久：并发连接、bulk 背压、宽 UDP 表、描述符与内存趋势
bash scripts/tun-stress.sh

# 5. 命令行驱动：规则生命周期、信号热替换、干净退出
bash scripts/daemon-smoke.sh

# 6. 真实上游文档：真 1 MiB 文档、真 curl 取回、缓存与内置兜底（不需要 root）
bash scripts/daemon-rules-check.sh

# 7. 真实 HTTP CONNECT：不启动本地服务器，不解密 TLS
bash scripts/proxy-smoke.sh

# 8. 安卓真机/模拟器上的内核：设备内建 TUN、真实规则、真实证书（这是交付目标平台）
bash scripts/android-e2e.sh
```

前五个脚本都自建接口、配好地址与路由、跑完清理。判定以 `CHECK PASS/FAIL/WARN` 与 `RESULT` 输出。

**第 8 个是唯一在交付目标平台上跑的。** 它把内核推到设备里，用真实规则文档在设备内建
`watt0`、装路由、发 DNS 查询、跑真实 TLS。设备端需要 root——`adb shell` 给的是 `shell`
用户，建 TUN 和改路由表都要提权，所以整个设备脚本一次 `su -c` 执行（半途提权会以
「运行到一半才失败」的形式出现，比一开始就失败更难查）。

**这个 `su` 是测试脚手架，不是产品形态。** 产品里 TUN 由 Kotlin 的 `VpnService` 持有，
不需要 root（第 5.4 节）。这条脚本存在的意义是：在 `VpnService` 壳层写好之前，
内核本身仍然能在真机上被端到端验证，而不是只能靠单元测试。

```bash
DEVICE="-s emulator-5554" bash scripts/android-e2e.sh
```

### 逐域名核对真实规则文档

前六个脚本验证的是**内核**。「规则文档本身靠不靠得住」是另一个问题，四个脚本回答它：

```bash
# 1. 文档里每个域名问一遍内核命中了什么（离线、不需要 root）
bash scripts/probe-all-domains.sh          # -> tmp/probe-all-domains.tsv

# 2. 逐个拨号规则给的地址，做 TLS/HTTP 握手，并同时测直连做对照
python3 scripts/probe-forward.py           # -> tmp/probe-forward.tsv

# 3. 真实文档 + 真实隧道 + 真实证书，端到端
bash scripts/verify-rule-sites.sh

# 4. 单个「地址是否真服务这个域名」的快速判定
bash scripts/precheck-rule-ip.sh <domain-ip-pairs.tsv>
```

第 2 步**必须带直连对照**，否则失败无法归因：一个不通的规则地址，可能是规则写错了，也可能是这台机器根本到不了那里。

最新一轮对 `v1.0.47`（316 条规则 / 2454 域名 / 24435 地址）的结果：

| | 数量 | 说明 |
|---|---:|---|
| 域名总数 | 2454 | |
| 占位符条目（不计） | 1178 | `ips` 是 `{Cloudflare}` 等，按设计交给上游 DNS |
| 上游脏数据 | 2 | `wdcp(remove).microsoft.com` 字面带 `(remove)`，不是合法主机名 |
| **规则提供真实地址** | **1274** | 本次测试的范围 |
| 转发成功 | 1011（80.2%） | 连上并完成证书校验 |
| 规则地址不通、直连通 | 147 | 规则的地址在这段网络里不可达 |
| 连上但证书不属于该域名 | 22 | 共享 CDN IP 上没有配置这个 SNI |
| 两条路都不通 | 81 | 网络层阻断，与内核无关 |

其中 **307 个域名是直连失败、规则地址成功** —— 这是这类规则集真正的价值：本机 DNS 被投毒到 `163.70.148.13`、`185.45.5.35`、`0.0.0.0`，而规则给的真实地址能通过完整证书校验。代价是 147 个相反方向（直连通、规则地址不通），且规则地址的中位延迟 358 ms 高于直连的 185 ms——**它不是加速器，是绕过 DNS 投毒的手段**。

按分组看转发成功率（排除占位符）：`CDN for open-source` 100%、`For Tools` 92.1%、`developer` 91.1%、`Other Platforms` 89.1%、`For Web` 88.9%、`For Service` 85.2%、`In Game` 72.0%、`Microsoft Live` 64.2%、`XBOX/Microsoft Store` 37.1%。`Academic` 两条规则全是占位符，无可测域名。

`scripts/verify-rule-sites.sh` 用**真实文档**（不是合成的）在隧道里跑 16 个跨 9 个分组的站点，`RESULT PASS checks=36`：DNS 全部由规则集应答且地址都在规则里、16 条流全部命中规则（`matched=16 direct=0`）、证书链全部校验通过。

### 用 DNS 观测运行中的规则集

`daemon-smoke.sh` 的热替换验证值得单独一说：脚本把一个规则文档写到磁盘（域名 → 地址 A），启动 daemon，用 DNS 问这个域名——因为内核从规则集直接应答，**答案就是规则集本身**。然后把同一个路径上的文档改成地址 B，发 `SIGUSR1`，再问一次。答案变了，就证明了「信号 → 取文档 → 解析 → 替换运行中引擎的路由表」这条链路整条通畅。

这个技巧不需要外网、不需要真实连接，就能把运行中内核的路由表读出来，是排查"规则没生效"这类问题最快的办法。

### 判定不采信内核自己的计数器

`real-traffic.sh` 和 `real-upstream.sh` 的判定来自三个**不知道内核存在**的来源：

1. `curl` 的退出码与它收到的 HTTP 状态码；
2. 真实 TLS 服务器自己的连接与请求计数；
3. 响应体的 sha256，与同一个 URL **直连**抓取的结果对比。

内核的计数器只打印出来供诊断，**不参与判定**。理由是：一个丢包的实现照样能报告两个方向都有字节，而一个"连接成功"也可能只是它应答了自己的 SYN——第 5.3 节那个例子就是这么骗过客户端的。

`real-upstream.sh` 的差分设计尤其值得注意：它先用 `curl` **直连**抓一份，再从隧道抓一份，两份的哈希必须相同。这样期望值不是硬编码的，而是由同一次运行产生。

### 复现自环

```bash
bash scripts/probe-selfloop.sh                      # 看门狗一秒内触发
PROTECT_MARK=0x5754 bash scripts/probe-selfloop.sh  # 稳定在 1 条流
```

### 耐久运行的参数

```bash
STRESS_ROUNDS=150 STRESS_SETTLE=90 STRESS_SAMPLE_SECONDS=15 bash scripts/tun-stress.sh
```

`STRESS_SETTLE=90` 很关键：负载结束后静默 90 秒，超过 60 秒的 UDP 空闲超时，所有流都会被回收。**只有这时「描述符回到基线」才是一个真正的泄漏判定**——否则仍然存活的流本来就合法地持有描述符。

脚本会按轮数推导内核寿命（`STRESS_SECONDS`），调用方给的值只会被抬高、不会被压低：寿命短于计划不会报错，只会让内核先收工、客户端空等，最后给出一个与内核无关的判定。同理，等判定的窗口是 `STRESS_SETTLE + 60` 而不是写死的 60 秒——判定是在 settle **之后**才打印的。

其余旋钮：`STRESS_PARALLEL`（每波连接数，默认 25）、`STRESS_WAVES`（每轮波数，默认 8）、`STRESS_UDP_FLOWS`（每轮独立 UDP 流，默认 200）、`STRESS_BULK_BYTES`（每次 bulk 传输字节，默认 8 MiB）、`STRESS_LISTENERS`（单目的地监听上限，默认 64）。

`STRESS_UDP_CEILING`（默认 0 = 用内核自己的上限）是唯一一个**为了压出缺陷而故意调小**的旋钮。默认运行测的是内核的默认配置——那才是应该被测的东西；把它调到 8 之类的值，是为了让淘汰路径在几秒内跑几千次，而不是等上几分钟。

### 诊断

```bash
bash scripts/check-scripts.sh      # 所有脚本的语法检查
bash scripts/diag-stress.sh        # 运行中采集：进程状态、线程、fd、采样序列、接口
sudo bash scripts/stop-stress.sh   # 清掉残留进程与接口
```

`SAMPLE` 行同时携带 fd、RSS 与**流表规模和各拒绝计数**。这是刻意的：负载测试卡住时最终报告没用——等判定打印出来，值得看的那一刻早就过去了。

---

## 8. 命令行驱动

```bash
# 只看规则，不需要 root
watt-daemon --check --probe steamcommunity.com

# 跑隧道（需要 root；接口地址与路由由调用方配置）
sudo watt-daemon --tun watt0 --address 198.18.0.1/15
```

| 信号 | 含义 |
| --- | --- |
| `SIGUSR1` | 立刻刷新规则 |
| `SIGINT` / `SIGTERM` | 停止，打印最终计数 |

规则来源优先级：`--rules-file`（指定文档，永不取网）> 磁盘缓存 > 内置文档。缓存陈旧时（默认 6 小时）启动即刷新，之后按 `--tick` 检查。

**daemon 刻意不配置接口**：设地址、装路由在主机上是 `ip addr` / `ip route`，在 Android 上是 `VpnService.Builder`。把它留在库外面，内核就永远不需要 shell out，而这也正是两端唯一不同的部分。

---

## 9. 接入 Android

内核是库，安卓上的壳层是 Kotlin。两侧之间是 **`watt-ffi`：一个小而稳定的 C ABI**
（`core-rs/crates/watt-ffi/`，头文件在 `include/watt_ffi.h`）。JNI 调它，壳层照着它写。

```bash
scripts/cargo.sh build -p watt-ffi --release --target aarch64-linux-android
# -> target/aarch64-linux-android/release/libwatt_ffi.so
#    放进 app 的 jniLibs/arm64-v8a/
```

Kotlin 侧要做的事只有两件：

```kotlin
// 1. VpnService.protect，转成一个 C 回调。
//    少了它，内核的上游连接会被它自己刚建的隧道捕获，中继变成自环——
//    实测：16 条客户端连接产生 tcp_open=2048，上游一个字节都没回来。
//    这不是「可选优化」，也没有兜底。
val protector = watt_protect_fn { _, fd -> if (vpn.protect(fd)) 1 else 0 }

val engine = watt_engine_new(
    vpnFd,                 // 接管，不拥有：关闭是 VpnService 的事
    "watt0",
    rulesBytes, rulesBytes.size.toULong(),
    protector, null
) ?: error(watt_last_error()!!.toKString())

// 2. 自己的线程里循环 step。引擎不占线程，停机时机留给壳层。
while (running) {
    if (watt_engine_step(engine) < 0) { log(watt_last_error()!!.toKString()); break }
}
```

规则热替换走 `watt_engine_replace_rules`：**在跑的流不受影响**，它们已经拿着当初规划好的地址，为了应用一次规则更新而掐断一条正在工作的连接，比让它跑完更糟。

**这个 ABI 刻意不提供的**：`Fetcher`。取规则用什么 HTTP 客户端是壳层的事——只有 App 知道用户的数据预算与代理设置。把规则文档的字节交给 `watt_engine_new` 或 `watt_engine_replace_rules` 就够了。

`watt_engine_new` 在**没有 protector 时拒绝构建**（返回 NULL 并说明原因），而不是给一个默认值：在安卓上，一个没有 protector 的引擎会中继到它自己，而自环比一个错误更糟。

---

## 10. 当前状态

**已验证**

- 168 个测试全绿 → **现为 225 个**，`clippy --workspace --all-targets -- -D warnings` 零告警。
- **性能改动后的 TUN 回归（WSL，改动涉及 `rank`/`plan`/`ruleset`，所以重跑）**：
  - `tun-smoke.sh` → **`RESULT PASS checks=12`**（DNS 本地应答、3 条并发 TCP、UDP 往返、ICMP 计数、无连接失败、无监听器耗尽）
  - `real-traffic.sh` → **`RESULT PASS checks=11`**（真实 TLS 穿隧道：8/8 并发客户端、17 个请求、0 次握手失败、单连接复用 5 次、全部 200）
  - `check-scripts.sh` → **PASS**
- **雷电 Android 14 模拟器上，双源合并（旧端点时代：`/hosts?all=1` + `/rules`）的 CONNECT 代理 7/7 域名全通**（`exit=0`，全部 2 秒内）：
  `nikke-en.com` 200 / `www.xbox.com` 307 / `cfx.re` 200 / `i.scdn.co` 403 /
  `login.live.com` 200 / `code.jquery.com` 301 / `elytra.ac` 404。
  状态码是站点自己的应答，`exit=0` 说明证书校验全部通过。
  （此条是端点变更**之前**的历史记录，`domains=4044` 是当时那次合并的计数；变更后的默认见 5.7。）
- **性能改动实测（设备 `--check`，同一份输入，旧端点时代）**：`merge` 2.5s → **0.5s**，
  规则编译完成 3.0s → **1.4s**，结果一致（`domains=4044 multi_address=1312`）。
- 合并结果在设备上核对（旧端点时代）：`merge sources=2 domains=4044 multi_address=1312`，
  `probe github.com` 报 `addresses=[51.142.105.107, 20.218.253.22, 20.12.240.255, +36 more]`。
- 真实 TUN 上的端到端：DNS 本地应答、并发 TCP、UDP 往返、8 MiB 逐字节校验、ICMP 计数。
- 10 轮负载回归 `RESULT PASS checks=13`，客户端 10/10 通过。
- **150 轮长稳测试 `RESULT PASS checks=13`**（13 分 6 秒，`STRESS_SETTLE=90`）：30000 连接 / 30000 UDP 流 / 1,258,291,200 字节（1.17 GiB），客户端 **150/150 通过、0 失败**。
  - `tcp_open=30150 tcp_closed=30150 tcp_rejected=0 tcp_resets=0 tcp_failed=0`
  - `udp_open=28994 udp_reused=1006 udp_evicted=27970 udp_rejected=0 udp_dropped=0`
  - `dns_local=150 observed=300 matched=59144 direct=0`；`u2c=1258861350`（逐字节校验通过）
  - 收尾 `FLOWS tcp=0 udp=0 listeners=4`；**描述符精确回到基线**：`baseline=5 final=5 live_flows=0`（allowed 13）
  - 内存 `baseline=3560KiB final=7140KiB growth=3580KiB`（上限 32768KiB）——RSS 呈**台阶式**（6544 → 7108 KiB 后长期平直），是 glibc 分配器扩展堆区，不是泄漏
  - 打印 14 行 `CHECK`（13 条 required + 1 条 advisory），`checks=13` 统计的是 required 部分；唯一的 `CHECK WARN` 是 advisory 的「UDP 表不必 churn」——150 轮 × 200 条互不重复的流 = 30000 条流对一个 1024 的表，淘汰是**必然且刻意**的
- daemon 17/17：规则生命周期、`SIGUSR1` 热替换（应答从 `203.0.113.10` 变为 `203.0.113.20`）、`SIGTERM` 干净退出。
- 真实上游文档：`version=1.0.47`，316 条目 / 2452 域名 / 24435 IP 全部编译通过；真 curl 取回 1,045,066 字节并落盘；第二次运行命中缓存；无缓存且离线时回落内置文档。

**未做**

- **Android 侧**：C ABI（`watt-ffi`）已写好并在两个安卓 target 上通过 `cargo check`（见第 9 节），**Kotlin `VpnService` 壳层本身还没写**——那需要 Gradle、Android SDK 与 JDK，当前环境里都没有。**免 Root 的交付路径是 CONNECT 代理**（第 5.4 节），它不需要 TUN、不需要 Root；TUN 那条要等这个壳层。`scripts/android-e2e.sh` 里的 `su` 只是**测试工具**——它让内核在没有 `VpnService` 的情况下也能在真机上被端到端验证，不是产品形态。
- **安卓上的 TUN 路径目前不通，这是预期的。** 实测（雷电 Android 14，`android-e2e.sh`）：16 个 `curl` 请求产生 **`tcp_open=2048`**（正好是 `max_tcp_flows` 上限），`u2c=0`、`c2u=1058816`（÷2048 = 517 字节，正好一个 ClientHello），`tcp_failed=0`。**16 条客户端连接放大了 128 倍**，且上游一个字节都没回来。
  **可确认的**：脚本确实装了 `ip rule add fwmark 0x5754 lookup 5755 pref 4000`（手动执行返回 `rule_exit=0`，规则出现在 `ip rule show` 里），`MarkProtector` 也确实用 `setsockopt(SO_MARK)` 打了标（有单元测试）。**没能钉死的**：为什么这套在纯 Linux 上有效的组合（`scripts/probe-selfloop.sh` 在 WSL 上验证过）在安卓上不生效——可能是 netd 对 socket 的 network 绑定覆盖了标记，也可能是路由链的其它环节。**没有做进一步归因，因为结论已经足够**：安卓为这件事提供的机制是 `VpnService.protect(fd)`，`MarkProtector` 是**纯 Linux 主机**的替代品，不是安卓的。
  **所以：TUN 的 TCP 中继在安卓上要等 Kotlin 壳层。** 不受影响的是：单元测试、WSL 上的 TUN 端到端（`tun-smoke.sh` / `real-traffic.sh` / `tun-stress.sh`）、以及本脚本的 DNS 腿（leg 1：16/16 全部由规则集应答，`rcode=0`，答案 10~89 条）。
- **证书不匹配无法在内核侧察觉**（第 5.7 节，实测）。内核只能按规则给的顺序去试，由客户端做最终判断；规则把错证书的地址排在前面时，那个域名就会失败。
- **模拟器测试环境**：按约定构建在 WSL、模拟器运行在 Windows，尚未搭建。
- **应用级分流**：按需求暂缓。
- **TCP 流表满时仍是拒绝**（`max_tcp_flows`，默认 2048）：TCP 有真实的拒绝信号（RST），且连接由客户端主动关闭，正常使用下不会填满，所以暂不做淘汰。若将来需要，`udp.rs` 的 `make_room` 是现成的模板。
