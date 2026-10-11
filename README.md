<div align="center">

# speed

[**简体中文**](README.md) · [English](README.en.md)

**Android 网络绕行工具 · 默认免 Root**

规则表里命中的域名，改从规则给出的地址拨号；其余流量原样直连。

[![License](https://img.shields.io/badge/license-GPL--3.0-blue.svg)](LICENSE)
[![Platform](https://img.shields.io/badge/platform-Android%208.0%2B%20(API%2026)-3ddc84.svg)](#下载)
[![ABI](https://img.shields.io/badge/ABI-arm64--v8a%20%7C%20armeabi--v7a%20%7C%20x86__64-lightgrey.svg)](#下载)
[![Release](https://img.shields.io/github/v/release/zejiahuang/speed?label=release&color=3ddc84)](../../releases/latest)

[下载](#下载) · [工作原理](#工作原理) · [从源码构建](#从源码构建) · [免责声明](DISCLAIMER.md)

</div>

---

## 解决什么问题

有些服务在你的网络里连不上，原因不是服务本身不可用，而是**域名解析出来的那个地址**在你这里不通。
同一个服务通常在多个地址上都有入口，换一个地址就能连——问题只在于你不知道该换成哪个。

`speed` 就是补上这一步：它带一份「域名 → 已知可用地址」的规则表，
在系统把连接交出去之前截住它；域名在表里，就改从表里的地址拨号。

默认的两种模式不需要 Root，不需要安装证书，不需要框架。设备已有 Root 时，还可以选第三种
模式——它走的是另一条路：需要 Root，且会在本机终结 TLS。

接管方式有三种，同一个安装包，连接页上切换：

| | VPN 模式 | 代理模式 | Root 模式 |
| --- | --- | --- | --- |
| 接管谁 | 公网流量全部，内网直连 | 只有把 HTTP 代理指过来的软件 | 只有规则表里的域名（改系统 hosts） |
| 覆盖范围 | 全部 TCP + UDP | 只处理 TCP，且只对设了 HTTP 代理的软件 | 只处理 TCP **443** |
| 系统授权 | 需要一次 VPN 授权 | 不需要 | 需要 Root |
| 解密 TLS | 否 | 否 | **是**（本机签发证书，不校验上游） |
| 内核参数 | 设置页里的全部生效 | **不生效**，用内核固定的一套 | **不生效**，用内核固定的一套 |
| 适合 | 手机、平板 | 模拟器、无法授权的环境、只想给个别软件加速 | 有 Root，且需要覆盖「规则地址证书不匹配」的域名 |

**前两种模式下，规则未命中的域名都直连**——规则表回答的是「优先试哪个地址」，不是「允不允许出去」。
Root 模式只接管规则表里的域名，表外流量完全不经过它。

## 工作原理

三种接管方式共用同一套内核与规则表，区别在于**流量从哪个入口进来**、以及**要不要终结 TLS**：

<div align="center">
<picture>
<source media="(prefers-color-scheme: dark)" srcset="assets/flow-dark.svg">
<img src="assets/flow-light.svg" alt="三种接管方式的流量路径：VPN 模式经 tun0、代理模式经本地 HTTP 代理端口、Root 模式经 hosts + 系统 CA + iptables 到本地反代；前两者不终结 TLS，Root 模式终结；命中规则改从规则地址拨号，未命中直连；三条路都通到上游服务器" width="900">
</picture>
</div>

### 一次连接里发生了什么

1. 应用照常发起连接，目标是一个域名——它不知道下面发生了什么。
2. 系统把这条流量交给 `speed`：经 `VpnService` 的 `tun0` 接管整机。
3. 内核在**用户态**终止这条连接（自带一套 TCP/IP 协议栈），读出目标域名。
4. 查规则表：
   - **命中** → 不再理会系统 DNS 的结果，改从规则给出的地址里拨号；
   - **未命中** → 按系统 DNS 的结果原样直连，不参与拨号。
5. 两条路都通到上游服务器，区别只在于**连的是哪个地址**。

整个内核跑在应用进程里，不加载内核模块，不修改系统分区——这是前两种模式「免 Root」的全部含义。

### 代理模式：不建隧道的那条路

有些环境给不了 VPN 授权（模拟器、受管控的设备），或者你只想让某一个软件加速。
代理模式不建 `tun0`，而是在内核里开一个本地 HTTP 代理端口：

1. 连接页把模式切成**代理模式**，点连接。页面出现一个 `127.0.0.1:<端口>` 的地址，一键复制。
2. 把它填进目标软件（浏览器、终端、包管理器）的 HTTP 代理设置。
3. 这个软件的请求就进了内核——读同一份规则表，走同一套「命中改道 / 未命中直连」策略。

它同时处理两类请求：

- **`CONNECT` 隧道**（HTTPS 走这条）：内核读出目标主机名后开始转发，之后的字节原样透传，**不解密**。
- **普通 HTTP 请求**：把请求行里的绝对 URI 改写成目标服务器认识的形式再转发。同样只看地址，不看内容。

**代理模式不读设置页的内核参数。** 它只需要一份规则表——C ABI 里 `watt_proxy_start` 的入参
就是「端口 + 规则」，没有第三个参数。所以连接超时、候选数量、竞速宽度、上游出口、上游解析器
这些行只对 VPN 模式生效；代理模式用的是写死在 `watt-proxy` 里的那一套。连接页的代理卡片上也写着这一条。

一个例外值得单独说：**证书预检在代理模式里是开着的**，只是设置页的开关管不到它。
它是「规则给的地址是不是真的能服务这个域名」的唯一判据，关掉它等于把规则表的正确性当成默认成立——
所以这个模式不给这个选项。VPN 模式下可以关（会快一些，代价见「证书预检」）。

**只处理 TCP。** UDP 流量（含 QUIC、DNS）不会被代理接管，这是 HTTP 代理这个形态本身的边界，
不是配置问题；要覆盖 UDP 就用 VPN 模式。

### Root 模式：终结 TLS 的那条路

前两种模式都不解密 TLS，因此有一条硬边界：**规则给出的地址，其证书必须覆盖目标域名**，
否则这个地址用不了。有些域名在所有已知地址上都不满足这一条，前两种模式就无能为力。

Root 模式换掉了这个前提——它在本机以中间人身份终结 TLS：

1. 连接页把模式切成 **Root 模式**，点连接（首次会弹一次说明，需要确认）。
2. 应用用 `su` 自动做三件事：把规则表里的域名写进**系统 hosts**、把一张**自签 CA** 装进系统信任库、
   加一条 **iptables 重定向**把发往 `127.0.0.2:443` 的流量转到本机代理端口。
3. 客户端按 hosts 连到 `127.0.0.2:443`，本机代理用自签证书完成握手，再以普通客户端身份连真实服务器。

**这三件事都不需要手工操作**——证书由内核现生成，装进信任库、改写 hosts、加防火墙规则全部自动完成，
唯一的用户动作是那一次 Root 授权。**断开连接时三件事全部自动还原**：hosts 恢复成安装前的原文件，
信任库里的那张 CA 被删除，iptables 规则按原样删除。

代价必须说清楚：

- **上游证书不再被校验。** 本机代理不检查真实服务器的证书，因此它能连上那些「证书不覆盖该域名」的
  地址——这正是它存在的理由，也意味着**你与服务器之间的身份保证被移除了**。这条只在你主动选择
  Root 模式时成立。
- **只覆盖 TCP 443，且只覆盖规则表里的域名。** hosts 改写只对走系统解析器的应用生效；
  启用 Private DNS（DoT）或自带 Secure DNS / DoH 的应用会自行解析、绕过 hosts。
- **它改的是系统分区与信任库。** 这是与「免 Root」相对的另一端：一次真实的系统级改动，
  而不是应用内的开关。断开时自动还原，但改动本身确实发生过。
- 已在运行的应用可能要到**重启一次**之后才会读到新装的 CA。
- Root 模式**不读设置页的内核参数**，与代理模式同理——它的 C ABI 入参同样只有「端口 + 规则 + CA 目录」。

## 两个关键机制

### 多候选并行竞速

一个域名在规则表里往往有多个候选地址，但其中一部分是死的。如果逐个试，
排在可用地址**前面**的那个死地址会让这个域名白等一整个超时。

`speed` 改成同时开拨：同一时刻最多 `race_width` 个候选在拨，每 250 ms 再发射下一个，
谁先握手成功就用谁，其余立刻关闭。

<div align="center">
<picture>
<source media="(prefers-color-scheme: dark)" srcset="assets/race-dark.svg">
<img src="assets/race-light.svg" alt="三个候选地址并行拨号的时间线：候选①在 300 毫秒握手成功并胜出，候选②因证书不覆盖域名被丢弃，候选③超时被丢弃" width="900">
</picture>
</div>

最坏情况从「所有死地址的超时之和」压到「最快那个候选的握手时间」。
`race_width = 1` 就退回逐个串行尝试，和传统做法一样。

这套机制只在规则表**确实给出多个候选**时才有用。实测默认那份文档 40 条条目**每条都只有一个地址**，
此时并行无从展开，`race_width` 调多大都一样：一个死候选就等于这个域名必然失败。要让并行真正生效，
规则表得先给出第二个地址。

### 证书预检：地址能连，不等于地址是它

拨号之前，内核会先确认这个地址出示的证书是否覆盖目标域名。这一步是必需的：

<div align="center">
<picture>
<source media="(prefers-color-scheme: dark)" srcset="assets/cert-dark.svg">
<img src="assets/cert-light.svg" alt="两个规则地址的对比：地址①的证书覆盖 example.com 被采用，地址②的证书是另一个域名被弃用" width="900">
</picture>
</div>

少了这一步，一个证书不匹配的地址会被当成可用地址用很久，失败会以一种难以归因的形式暴露出来。
应用提供「校验规则地址的证书」开关，关掉会快一些，但可能连到证书不对的地址。
这是有意的取舍：**宁可让调用方看见一个明确的失败，也不要给它一个看起来成功、实际连错机器的连接。**

## 规则从哪来

<div align="center">
<picture>
<source media="(prefers-color-scheme: dark)" srcset="assets/rules-dark.svg">
<img src="assets/rules-light.svg" alt="规则流水线：规则源 → 解析 → 路由表 → 内核，缓存以 sidecar 形式记录来源身份" width="900">
</picture>
</div>

规则文档就是一份「域名 → 已知可用地址」的清单。内置两个可选源，Root 模式另有一份固定文档，你也可以添加自己的：

| 来源 | 形态 | 规模 | 谁在用 |
| --- | --- | --- | --- |
| `HelloGitHub（GitHub520）` —— **默认** | hosts | 40 个域名 | VPN / 代理模式 |
| `github-hosts` —— 第二源 | hosts | 37 个域名 | VPN / 代理模式 |
| `s302 替代上游` —— 内置但不可选 | `<拨号名> <TAB> <原域名>` | 846 个域名 | **仅 Root 模式**，固定加载 |
| 自定义源 / 本地文件 | 任意 | 你导入的 | VPN / 代理模式 |

**默认源有三个事实要先说：** 主地址 `raw.hellogithub.com` 在香港，实测延迟最低（0.10–0.12s）；
**服务器 2026-12-31 到期**——镜像取的是同一份上游文档的另一个端点，能把「立刻不可用」降级成「降速可用」，
但到期前需要换掉主地址；**许可为 CC BY-NC-ND 4.0**（禁商用、禁演绎）。应用只按 URL 在运行时引用该文档，
不打包进安装包。

**第二个源走反代，不是图方便。** GitHub 的 IPv4 在大陆不可达，而 Android 的 `HttpURLConnection`
不会在 A 记录不通时回落 IPv6 ⇒ 直连对国内用户是确定性失败。反代提供逐字节相同的文档，换的是传输，不是内容。

**Root 模式固定用它自己的那份文档，而且不出现在可选列表里。** 原因是它必须逐域名说清「这条连接该去哪里」：
root 模式把域名劫持到本机终结 TLS 的代理，代理要**重新发起**每一条连接。同一套规则的 hosts 形态做不到
这件事——862 个域名全部指向 `127.0.0.1`，那是 s302 自己机器上的监听地址，而手机上 443 端口没有进程在监听。
应用加载的是 `<拨号名> <TAB> <原域名>` 形态：第一个字段是名字，代理因此拿到一个能从当前网络解析的地址；
`!<地址>` 则表示「拨这个地址、一个 SNI 都不发」。

其余是管理面：三级下钻（**分组 → 域名 → 地址**）、搜索覆盖域名 / 地址 / 分组名、分组级开关与全开 / 全关、
规则热替换不打断正在传输的连接。**缓存记录的是「它是哪一套规则」，不只是「它有多旧」**——换了数据源或
升级应用之后，来源身份不符即视为未命中并重新拉取，而不是继续用一套形状完全不同的旧规则（那样界面显示
规则正常，实际所有站点都在直连）。

## 界面

四个页面：

- **连接** —— 模式切换（VPN / 代理 / Root，Root 只在设备有可用 `su` 时出现，首次进入需确认）、
  总开关、实时上下行速率、本次会话流量与命中规则次数、规则缓存新鲜度；代理模式下出现代理地址卡片
  （含上面那条「内核参数不生效」的说明），Root 模式下显示固定的规则来源。
- **规则** —— 分组 → 域名 → 地址三级下钻，搜索覆盖域名 / 地址 / 分组名，分组级开关与全开 / 全关。
- **日志** —— 实时事件流，按级别过滤，可导出与归档。
- **设置** —— **按当前模式分栏**：TUN 模式显示内核参数（MTU、连接与空闲超时、候选数、拨号名）与
  上游出口 / 上游解析器，代理模式显示代理端口，Root 模式两者都不显示——另外两个模式的内核入口
  根本不接收这些设置文档。另有外观（玻璃材质 / 主题色 / 深色模式 / 圆角 / 字号）、数据导出导入、
  关于与更新检查。

界面语言跟随系统：简体中文 / English。

界面截图不随仓库分发：它们会随本机的壁纸、规则规模与语言设置变化，
放一张在仓库里，除了过时没有别的前途。

## 特性

- **路由** —— 规则驱动改道 · 多候选并行竞速 · 证书预检 · 失败冷却与排序 · 命中时 DNS 本地应答 · UDP 流表满时淘汰最久未用 · 规则热替换
- **三种接管** —— VPN 模式（`tun0` 全流量，需一次系统授权）· 代理模式（本地 HTTP 代理，免授权，`CONNECT` 隧道 + 普通 HTTP 转发）· Root 模式（改系统 hosts + 装自签 CA + iptables 重定向，本机终结 TLS，断开自动全部还原）
- **规则** —— 三级下钻与搜索 · 分组开关 · 自定义规则源与本地文件导入 · 缓存来源身份校验
- **模式感知** —— 设置页按当前模式分栏，只显示这个模式真正生效的设置项；首页的「命中规则」按模式读对应的计数器
- **观测** —— 实时速率与命中率 · 实时 / 归档日志与过滤导出 · 开发者视图（候选地址、冷却、静默计数）· 内核命令行
- **数据** —— 设置导出 / 导入（JSON 文档，导入前二次确认，来自更新版本的文档会被拒绝）
- **更新** —— 默认指向本仓库 Releases（经 `gh-proxy.com` 反代，因为应用自身流量不走自己的隧道）· 启动时静默检查，每天最多一次且可关 · 有新版弹窗并内联更新日志 · 按设备 ABI 挑下载包
- **外观** —— 玻璃材质（折射与模糊合并为一个开关）与自定义壁纸 · 主题色 / 深色模式 / 圆角 / 字号 · 界面语言跟随系统（简体中文 / English）

几个值得单独说的点：

- **失败分三类归因**：连接被拒 / 超时 / 「连上但零字节」。第三类只降低排序，不写失败记录——它是服务器侧的静默，不代表地址坏了。**不用启发式去猜**，因为「连上但零字节」可测，而证书不匹配不可测（服务器先发证书、客户端才拒绝，双向都有字节）。
- **超时按语义拆分**：没有备选地址时给足预算，有备选地址时用一个短窗口快速试错。一个死候选等于一次必然失败，因为预算是客户端给的。
- **内核命令行**：与 adb 控制面同一套命令（`status`、`set mtu 1400`、`dump`、`help`），直接在手机上驱动内核，不必连电脑。

## 已知限制

- 规则表里地址过期时，对应域名会失败或直连——这是数据问题，不是内核问题。
- 地址过期的一种具体表现：挂满超时之后报「候选耗尽」。若该条目只有一个地址，就没有第二条路可走。
- 被按名字干扰的域名（TCP 通、TLS 第一个记录之后被静默）无法处理：换地址无效，放宽证书校验也无效。
- 不解密 TLS 意味着无法处理「所有已知地址的证书都不覆盖该域名」的情况——除非改用 Root 模式，那是这条的例外。
- 代理模式只对能设置 HTTP 代理的软件生效，也不会替你捕获别的应用的流量。
- 代理模式只处理 TCP：UDP 流量（含 QUIC）不被接管；且它不读设置页的内核参数，只用内核固定的一套。
- Root 模式需要 Root，且**不校验上游证书**——本机代理不检查真实服务器的证书，这正是它能连上「证书不覆盖该域名」的地址的原因，也意味着你与服务器之间的身份保证被移除了。
- Root 模式只处理 **TCP 443**，且只对规则表里的域名生效；启用 Private DNS（DoT）或自带 Secure DNS / DoH 的应用自行解析，会绕过 hosts。
- Root 模式改的是系统 hosts、系统信任库与 iptables——断开时自动全部还原，但它确实是一次系统级改动；已在运行的应用可能要重启后才读到新装的 CA。
- 连通性在分钟尺度上抖动，通过率会随时间窗大幅变化。这不是内核状态，日志里的失败计数要连着时间窗一起看。
- 最低 Android 8.0（API 26）。

## 下载

到 [Releases](../../releases) 下载对应 ABI 的 APK：

| 文件 | 适用设备 |
| --- | --- |
| `speed-<版本>-arm64-v8a.apk` | 绝大多数现代手机与平板 |
| `speed-<版本>-armeabi-v7a.apk` | 较老的 32 位 ARM 手机与平板 |
| `speed-<版本>-x86_64.apk` | 模拟器、少数 x86 平板 |

三个包用**同一个发布密钥**签名。密钥不在仓库里——一个会安装 VPN 的应用，「谁能签出构建」就是它的信任边界，
所以密钥由维护者保管，经仓库 secret 交给 CI。若签名与你已装的版本不一致，Android 会拒绝覆盖安装；先卸载再装。

应用启动时会自己查一次更新（每天最多一次，可在关于页关掉），有新版会弹出窗口、附上这个版本的更新日志，
并直接给出对应 ABI 的下载链接。

**更新检查与 APK 下载都经 `gh-proxy.com` 这一第三方镜像中转**：应用自身流量被刻意排除在隧道之外
（`addDisallowedApplication`，防自环），走的是裸网络，而 GitHub 在部分网络环境下直连不可达。中转意味着
**你的下载请求会经过第三方**，完整性由 **APK 签名**兜底——被改过的包装不上，但这不保证第三方不记录下载行为。
不接受的话，请从源码自行构建（见下一节）。Releases 页面本身打不开时，用
`https://gh-proxy.com/https://github.com/zejiahuang/speed/releases`。

## 从源码构建

需要：Rust（含 `aarch64-linux-android`、`armv7-linux-androideabi` 与 `x86_64-linux-android` target）、
Android NDK r30、Android SDK（platform 36 + build-tools 36.0.0）、JDK 17。

两条命令就够了（在仓库根目录执行）：

```bash
PROFILE=release bash scripts/app-native-build.sh aarch64-linux-android armv7-linux-androideabi x86_64-linux-android
SKIP_NATIVE=1 bash scripts/app-build.sh assembleRelease -PabiSplits
```

`PROFILE=release` 不能省：脚本默认编 `debug` profile，产物功能一样但大二十多倍，打出来的包会是 60 MB
而不是 13 MB。CI 用的就是这两个脚本（`.github/workflows/release.yml`），本地配方跟着它走。

脚本是薄封装，大部分坑它们已经填好。下面这些是出问题时要知道的：

- **Cargo 对 Android target 没有默认链接器**，脚本会指到 NDK 的 clang。32 位 ARM 的目标名两边不一样
  （Cargo 叫 `armv7-linux-androideabi`，NDK 的 clang wrapper 叫 `armv7a-linux-androideabi`，多一个 `a`），
  脚本里转一次。弄错会得到一个编得出来、却永远不会被加载的库。
- **`--manifest-path core-rs/Cargo.toml` 不能省**：仓库根目录没有 `Cargo.toml`，workspace 在 `core-rs/` 下。
- **`--features jni-bridge` 不能省**：少了它 `System.loadLibrary` 照样成功，但每次调用都抛
  `UnsatisfiedLinkError`。
- **仓库里没有 Gradle wrapper**：一份没人能重新生成的二进制比一次下载更糟，发行包由脚本取到
  `~/.cache/gradle-8.14.3`。
- **签名完全由环境驱动，仓库里没有任何密钥**：设好 `WATT_KEYSTORE`、`WATT_KEYSTORE_PASSWORD`、
  `WATT_KEY_ALIAS`、`WATT_KEY_PASSWORD` 后产出已签名包；不设则停在未签名状态——这是刻意的停止点，
  不是遗漏。
- `-PabiSplits` 可选：不加产出单个 universal APK（会带一份设备永远用不到的库）。产物在
  `android/app/build/outputs/apk/release/`；只调 UI 时把 `assembleRelease` 换成 `assembleDebug`。


## 发布流程

[`.github/workflows/release.yml`](.github/workflows/release.yml)：推一个 `v*` 标签即触发。

CI 从源码编 Rust（三个 ABI）→ `assembleRelease -PabiSplits` → 用仓库 secret 里的密钥签名 →
校验签名与包信息 → 把三个 APK 连同 `CHANGELOG.md` 里对应版本的正文一起挂到 GitHub Release。

密钥以 base64 存进四个仓库 secret：`KEYSTORE_BASE64`、`KEYSTORE_PASSWORD`、`KEY_ALIAS`、`KEY_PASSWORD`。

## 目录结构

```
core-rs/            内核（Rust workspace）
  crates/watt-rules   规则文档 → 路由表，含缓存与更新生命周期
  crates/watt-net     TUN 设备
  crates/watt-proxy   代理模式：CONNECT 隧道 + 普通 HTTP 转发（免 Root、不建隧道）
  crates/watt-mitm    Root 模式：自签 CA 与本地终结 TLS 的反向代理
  crates/watt-stack   内核本体：engine / tcp / udp / dns / planner / upstream_proxy
  crates/watt-daemon  CLI 驱动
  crates/watt-ffi     C ABI 与 JNI 入口
android/            Android 壳层（Kotlin + Jetpack Compose）
scripts/            构建入口：编内核 `.so` 与打包 APK 的两个脚本
assets/             README 里的示意图（`flow-*` 由 `tools/readme-diagrams.py` 生成，其余三张手写）
tools/              `readme-diagrams.py`（生成 README 示意图）；`make-values-en.py`（从中文
                    `values/` 生成英文 `values-en/`）；DoH 探针：同一个解析器问两次
                    （明文 UDP/53 与 DoH），对比两次的答案
```

`assets/` 里每张图都有浅色与深色两套（`*-light.svg` / `*-dark.svg`），README 用
`<picture>` + `prefers-color-scheme` 切换。**GitHub 不会给 `<img>` 里的 SVG 重新上色**，
所以深色模式必须自带一份深色文件——这是唯一手段。`flow-*` 那对改
`tools/readme-diagrams.py`（一次描述、两个配色）；`cert-*` / `race-*` / `rules-*` 三对手写，
改图直接改文件。

界面文字同理只有一份源头：`android/app/src/main/res/values/strings.xml`（简体中文）。
英文版由 `tools/make-values-en.py` 从它生成——元素与注释原样拷贝、只换文字，名字或格式
占位符对不上时**拒绝写出**。改中文那份，再跑一次脚本。

## 测试

内核是纯 Rust，不需要设备。workspace 在 `core-rs/` 下，所以先切进去：

```bash
cd core-rs
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
```

需要真实设备的端到端测试（真实 `tun0` 的连通与长稳、CLI 驱动、UI 探针）用的是内部 harness，
**不随仓库分发**：它们要真实 TUN、`sudo`，以及一套只在本机成立的环境假设，
放进公开仓库只会让读者以为照着跑就能复现。

一条贯穿全仓库的约定：**网络行为只在设备上测量**。主机（含 WSL）不经 `tun0`，
在主机上量到的「通 / 不通」与隧道无关，量出来的是测量方法的错误，不是内核的结论。

在设备上测量时还要确认探针的 uid 真的被送进了隧道：`VpnService` 按设计排除本应用自己
（防自环），所以从应用进程内部发起的请求永远不走隧道。用 `adb shell` 的 `curl` 当探针之前，
先看 `ip rule show` 里 shell 的 uid 落在哪个区间——本项目是 `uidrange 0-10074` 进 `tun0`，
`10075` 正是 `dev.detour` 自己。计数方向也容易读反：`rx_bytes` 是**下行**（服务器回来的页面），
`tx_bytes` 是上行请求；一次正常的页面加载应当是 RX 大、TX 小，RX 为 0 只说明根本没有应用发过流量。

## 许可

GNU General Public License v3.0，见 [LICENSE](LICENSE)。

**使用前请先读 [DISCLAIMER.md](DISCLAIMER.md)（免责声明）。** 它把 GPL-3.0 第 15、16 条的免责条款
具体化到本项目的实际情况：不提供任何网络出口、默认不解密 TLS（Root 模式是例外，且代价写明了）、
更新经第三方镜像中转、规则源另有其许可、不收集数据、无技术支持与 SLA，以及一份明确列出的、
由你自担的风险清单。下载、安装或使用本项目，即视为接受该声明。

## 致谢

- [BeyondDimension/SteamTools](https://github.com/BeyondDimension/SteamTools)（Watt Toolkit）——
  代理设计的参考。
- [521xueweihan/GitHub520](https://github.com/521xueweihan/GitHub520) —— 默认规则源（HelloGitHub）。
- [maxiaof/github-hosts](https://github.com/maxiaof/github-hosts) —— 可选规则源。
- [smoltcp](https://github.com/smoltcp-rs/smoltcp) —— 用户态 TCP/IP 协议栈。
- [Kyant0/AndroidLiquidGlass](https://github.com/Kyant0/AndroidLiquidGlass)（发布在 Maven 上的坐标是
  `io.github.kyant0:backdrop`）—— 玻璃材质的背景采样。
