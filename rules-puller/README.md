# rules-puller —— 规则自动拉取与聚合

把两份文档里的数据来源与解析规则，落成一套**每天自动跑一次**的 PHP 程序：
拉取远端规则 → 解析 → 合并去重 → 产出可直接使用的 hosts 文件与结构化 JSON。

对应文档：

| 文档 | 提供的部分 |
| --- | --- |
| `PROXY_APK_SPEC.md`（UsbEAm Hosts 代理 APK 开发说明） | 主/备数据源、请求要求（UA、禁用代理、超时、强制 UTF-8、`#end_usbeam` 校验）、INI 数据格式与字段含义、占位符处理、22 个分组 |
| `转发机制与IP来源.md`（Steamcommunity 302 运行逻辑） | S302 规则源地址、`S302_rules.ini` 的双重混淆算法与密钥、`[Rules]` 各字段语义、服务段的 `Domain` / `Json` 结构、上游替代源站的三类来源 |

---

## 一、快速开始

```bash
# 1) 先跑一次，确认能通
php fetch.php

# 2) 挂上定时任务（Linux）
crontab -e
17 4 * * * /bin/bash /opt/rules-puller/cron/pull.sh
```

需要 **PHP 8.0+**，建议装 `php-curl`（没有 curl 扩展时会退化成 stream，可用但更慢）。

Windows 用计划任务：

```cmd
schtasks /create /tn "rules-puller" /sc daily /st 04:17 ^
  /tr "php.exe D:\4\rules-puller\fetch.php --quiet --json"
```

---

## 二、数据来源

下表是 rules-puller **拉取的原始上游**（Dogfight360 / UsbEAm 官方站点），不是它**发布出去**的短路径。两者别混：
`/1`、`/2` 是本程序聚合后**产出**的地址（见「四点五」），原始源站是**输入**。
本表每一行都用 `curl -sS -o /dev/null -w '%{http_code}'` 实测过（2026-09-26 全部 `200`）：

| 键名 | 用途 | 地址 | 实测 | 请求要求 |
| --- | --- | --- | --- | --- |
| `usbeam` | 主规则（域名 → 地址） | 主 `https://www.dogfight360.com/Usbeam/usbeam_new_40.xml`<br>备 `https://usbeam.steam302.xyz/Usbeam/usbeam_new_40.xml` | 均 `200` | UA `UsbEAm_Next`、禁用系统代理、10 秒超时、强制 UTF-8、必须含 `#end_usbeam` |
| `s302_rules` | 补充规则（域名 → 替代上游） | `https://www.dogfight360.com/Usbeam/13007P/S302_rules.ini` | `200` | UA `Steam302` |
| `s302_version` | 版本号（可选） | `https://www.dogfight360.com/Usbeam/S302.ver` | `200` | UA `Steam302` |

**抗封锁兜底**：主源域名被 DNS 污染时，从 `cf.dogfight360.com` 的 TXT 记录取备用 IP，
再以固定 IP 直连（`CURLOPT_RESOLVE`），对应文档 1.3 节的「多 IP 解析」。

---

## 三、解析要点

### 3.1 UsbEAm 规则（INI 文本，非 XML）

```
[usbeam]           VERSION / Latest_Update_Time / Group
[Public]           ★ 占位符定义表
[usbeam_ad_*]      ★ 广告位配置，不是规则，已排除
[分组名]           NNNNN.name= / .ip= / .domain= / .port= / .cert= / .dltest=
```

**`[Public]` 是占位符的真正来源**（实测）：

```ini
[Public]
IPLIST       = 140.245.84.81,92.223.30.44,43.224.23.8
Cloudflare   = 104.16.18.94,104.16.19.94,...        ← 约 200 个 IPv4 + 100 个 IPv6
Cloudfront   = 13.32.54.227,13.32.54.64,...
Gcore_CDN    = 193.242.96.6,193.242.96.5,...
Akamai_a248  = ...
Cloudflare_DL= https://speed.cloudflare.com/__down?bytes=90000000   ← 是 URL 不是地址
```

所以 `21001.ip={Cloudflare}` 直接展开成 `[Public]` 里的地址列表，**不需要联网**。
只有 `[Public]` 里没定义的占位符才会走 DoH 兜底（并带 7 天磁盘缓存）。

### 3.2 S302 规则（双重混淆）

```
明文 --重复异或--> 密文 --Base64--> INI 字段值
密钥 = "SteamCommunity 302\x12\x34\x56\x78"（22 字节，循环使用）
```

程序对 `Json` / `Domain_list` / `Forwarding` 三个字段解密，然后：

- 从每个服务段的 `Json` 里解出 Caddyfile，**按大括号切分站点块**（跳过 `{port}` 这类占位符），
  抽出块头的站点主机名与块内的 `reverse_proxy` 上游 → 得到「域名 → 替代源站」映射
- `Domain_list` 解密后是通配符域名表 → 单独成表，不进 hosts
- 所有含 `CIDR` 的键（`Akamai_IP_CIDR`、`Fastly_*_CIDR`…）按名收集为优选候选池

解密后会做**合理性检查**（控制字符比例），密钥或字段格式不符时不会静默产出乱码。

---

## 四、产物

全部写在 `data/out/`，同时每天留一份快照在 `data/archive/YYYY-MM-DD/`。

| 文件 | 说明 |
| --- | --- |
| `hosts.txt` | **标准 hosts**：`IP\tdomain`，按分组加注释。可直接追加进系统 hosts |
| `hosts_s302.txt` | S302 劫持块：`127.0.0.1\tdomain\t#S302`（`listen_ip` 可配） |
| `domains.txt` | 全部域名（去重排序） |
| `wildcards.txt` | 通配符域名表（用于 PAC） |
| `upstreams.json` | 域名 → S302 反代上游（替代源站） |
| `s302_caddyfile.txt` | 解密出来的完整 Caddyfile 片段（按服务段标注） |
| `domains.json` | **聚合表**：域名 → {地址, 来源, 分组, S302 替代上游} |
| `rules.json` | 结构化规则：条目元信息、`[Public]` 占位符表、S302 服务与 CIDR、统计 |
| `stats.json` | 统计摘要 |
| `manifest.json` | 本次运行的清单：各源状态、产物大小与 SHA256、警告 |

合并策略：**同域名多来源给出的地址全部保留（union）并标注来源**；
同一域名的 hosts 行数按 `max_ips_per_domain` 截断（默认 3，见配置说明）。

---

## 四点五、中转站（`r.php`，对外发布）

把聚合好的规则发布成**固定短网址**，别人 / 别的客户端可以直接订阅：

```
https://你的域名/1    → UsbEAm 规则集（默认 hosts 文本）
https://你的域名/2    → Steamcommunity 302 规则集（默认 hosts 文本）
https://你的域名/1?format=json   → UsbEAm 规则集的结构化 JSON
https://你的域名/2?format=json   → S302 规则集的结构化 JSON
```

**`/1` 与 `/2` 是两套不同的规则集，不是同一套规则的两种格式。**

- `/1` = UsbEAm host records：实测 15971 条记录 / 5225 域名 / 15952 个地址（19 条跳过）。
- `/2` = Steamcommunity 302 劫持块：实测 862 条记录 / 862 域名，地址全部是 `127.0.0.1`。

两者**各自**都能渲染成 `hosts` 文本**或** `?format=json`。所以「hosts 还是 JSON」是**格式**维度的选择，与「`/1` 还是 `/2`」这个**规则集**维度互相独立 —— 旧文档把这两个维度混为一谈（说成「一份是 hosts、一份是 JSON」）是错的。

**`/2` 的语义只在本程序里成立，别把它并进 watt 的默认规则。** `/2` 的地址全是 `127.0.0.1`：

- 在本程序的世界里，这**就是**劫持的意义 —— 有个本地反代在环回监听，把域名指过去。
- 在 watt 内核里，规则**不允许**中继环回目标：关掉它的是 `Planner::can_relay`
  （`core-rs/crates/watt-stack/src/planner.rs`），它对非 override 的环回目标返回 false，
  `tcp.rs` 随即 `socket.abort()` 并记一次 `tcp_flows_rejected`（发 RST）。
  （注意不是 `is_blocked_target` —— 那个函数只在 `tcp.rs` 里遍历 `decision.alternatives`
  即候选尾部，从不看 `decision.target`。）

所以把 `/2` 并进 watt 不是「多覆盖 862 个域名」，而是**弄坏**它们。实测（把两份合并后统计）：

| | 值 |
| --- | ---: |
| 合并域名 | 5905 |
| 其中有真实 IP 的 | 5230 |
| **只有 `127.0.0.1` 的** | **675（全部是 `/2` 独有，`/1` 里没有）** |

这 675 个域名本来不在规则里，会**直连并正常工作**；一旦并入 `/2`，它们编译成单个
`127.0.0.1`，被 `can_relay` 拒绝 → RST。所以 watt 的 daemon 与 App **默认只并 `/1`**；
真有环回反代的部署，显式加 `--hosts-url https://abhuang.dpdns.org/2`。

`login.steampowered.com` 若并入 `/2` 会规划到 `127.0.0.1`（唯一地址，被拒），
`store.steampowered.com` 有 7 个地址（来自 `/1` 的非环回地址，加上那一个被拒的环回地址）。

**另一个实测的坑**：`?format=json` 的 schema 和内核解析的文档**不同**。
端点发出 `{"entries":[{"ip":..,"domain":..,"comment":..}]}`，而 `RuleDocument` 期望
`{"groups":[{"entries":[{"domains":[..],"ips":[..]}]}]}`。因为 `RuleDocument` 每个字段都是
`#[serde(default)]`，这个不匹配会被**当成** `Ok(零条目的文档)`，而两条调用链后果不同：
daemon 的 `--rules-url` 走 `RuleSet::from_document`，空集合会被 `NoUsableEntries` 拒掉（下载失败、
保留内置）；C ABI 的 `merge` 直接用 `parse_document`，**862 条进、0 条出、不报错**。
所以：需要把规则喂给内核时，用 **hosts 文本**，不要用 `?format=json`。

**默认关闭**。去后台「中转站」标签页打开，那里也有一键复制的地址。

### 路径与方法

| 方式 | 地址 | 依赖 |
| --- | --- | --- |
| 短路径 | `/1`、`/2` | 需要 `.htaccess` 重写（Apache 默认支持）或面板伪静态 |
| 通用 | `r.php?p=usbeam`、`r.php?p=s302` | **任何环境都能用** |
| 语义名 | `r.php?p=usbeam` / `r.php?p=s302` | 与路径配置无关，永远可用 |

路径在后台「中转站」里可改（`share.usbeam_path` / `share.s302_path`）。
改了短路径要同步改 `.htaccess` 里的两条 `RewriteRule`；用通用地址则不受影响。

### 内容协商与缓存

- 不传 `format` → hosts 文本；`?format=json` → 结构化 JSON（可被 `share.allow_json` 关掉）。
- `?only=domains`（配合 JSON）→ 只返回域名数组，省流量。
- 带 `ETag` + `If-None-Match` → 产物没变时返回 **304**。
- `Cache-Control: public, max-age=<share.max_age>`，默认 1 小时。
- `X-Rules-Generated-At` 头告诉下游规则什么时候生成的。
- 可选 CORS（`share.cors`），方便手机浏览器 / 网页工具直接抓。

### 为什么公开且不限流

规则本身就是公开数据，下游客户端会定时来拉。防滥用靠
**「产物是静态文件 + 下游缓存」**，而不是靠 IP 限流 ——
限流只会让正常客户端拉不到。所以 `r.php` 刻意**不走** `WebApp`
的登录与频率闸门。

安全上只做两件事：**关闭时返回 404**（而不是 403，不暴露功能存在），
以及 `.htaccess` 里挡掉 `data/` 与密钥文件。

### 配置项（`config.php` 的 `share`）

| 键 | 默认 | 说明 |
| --- | --- | --- |
| `enabled` | `false` | 总开关。关闭时中转地址一律 404 |
| `usbeam_path` | `1` | UsbEAm 规则路径（不含前导斜杠） |
| `s302_path` | `2` | S302 规则路径；两条不能相同 |
| `default_format` | `hosts` | 不传 `format` 时用哪种格式 |
| `allow_json` | `true` | 是否允许 `?format=json` |
| `cors` | `true` | 是否加 `Access-Control-Allow-Origin: *` |
| `max_age` | `3600` | 下游缓存秒数，0 = 不缓存 |
| `send_metadata` | `true` | hosts 里是否附上「由谁中转 + 订阅地址」注释 |

---

## 五、常用命令

```bash
php fetch.php                 正常拉取（带 ETag/Last-Modified 条件请求）
php fetch.php --force         忽略条件请求，强制重下
php fetch.php --only=usbeam   只处理一个源
php fetch.php --dry-run       只抓取解析，不写产物
php fetch.php --json          以 JSON 输出结果（定时任务/监控用）
php fetch.php --verbose       打印 DEBUG 日志
bash check.sh                 对全部 PHP 文件跑语法自检（需 php-cli）
```

退出码：`0` 全部成功 · `1` 部分源失败（已用本地副本兜底）· `2` 关键源失败或写盘失败。

**幂等与安全**：`flock` 单实例锁防止定时任务与手动触发重叠；产物先写临时文件再改名，
下游不会读到半截文件；抓取失败时自动回退到 `data/raw/` 里的上一次原始文件继续聚合，
而不是让当天的产物变成空的。

---

## 六、管理后台（`admin.php`）

一个自带鉴权的控制台，六个标签页：

| 标签 | 内容 |
| --- | --- |
| 概览 | 关键指标卡、数据源状态表、两源合并口径、产物清单与 SHA256、警告 |
| 操作 | 立即拉取 / 强制重下、清理解析缓存、重置限流、清理过期文件、运行历史 |
| 配置 | 分组表单，逐项显示「当前值 vs 默认值」，被覆盖的项高亮 |
| 日志 | 选择日志文件、按级别着色查看末尾 N 行 |
| 审计 | 谁、从哪个 IP、什么时候做了什么 |
| 安全 | 防护状态、改密码、网页触发令牌（带随机生成）、解除登录锁定 |

### 「立即拉取」是怎么跑的（共享主机重点）

后台点按钮时需要**在另一个进程里**执行 `fetch.php`（因为 `fetch.php` 会 `exit()`，
直接 include 会把整个后台请求带走）。按优先级两条路：

1. **PHP CLI 子进程**（首选）：找到可用的 php 命令行，`proc_open` 起进程。
2. **HTTP 自触发兜底**：找不到 CLI 或 `proc_open` 被禁时，改为请求自己的
   `fetch.php?format=json&internal=1&token=...`。走 Web SAPI，**不需要 CLI
   也不需要 `proc_open`**，功能完全等价。

CLI 的查找顺序（`config['cli']`）：

| 顺序 | 来源 | 说明 |
| --- | --- | --- |
| 1 | `cli.binary` | 后台「配置」页可填，优先级最高 |
| 2 | `data/php-cli-path.txt` | 不想改配置时，把绝对路径写进这个文件 |
| 3 | `PHP_BINDIR/php` | FPM 下常指向 php-fpm，**会用 `-r 'echo PHP_SAPI;'` 验证后剔除** |
| 4 | `cli.candidates` | 常见路径，`{ver}` 替换为当前版本（如 `/www/server/php/{ver}/bin/php`） |
| 5 | `cli.glob_patterns` | 通配搜索，版本号大的优先 |

验证方式是**实际执行一次并检查输出 `cli`** —— 所以不会误把 `php-fpm` / `php-cgi`
当 CLI 用。

`internal=1` 的作用：跳过「网页触发最小间隔」（默认 300 秒）。后台自己已有
`admin_action_interval` 节流，双重限流会让「点一下、等 5 分钟」成为常态。
**该标记只在请求来自本机回环时生效**，且仍需正确 `web_token`，不会削弱安全性。

**首次使用**（二选一）：

```bash
php admin-cli.php init          # 命令行设置密码（推荐）
```

或从 **127.0.0.1** 打开 `admin.php` 在页面上设置 —— 只允许本机，公网访问会被告知去跑命令行。

### 命令行管理（`admin-cli.php`）

没有浏览器、或者被自己的白名单锁在门外时用这个：

```bash
php admin-cli.php status                      # 查看当前安全配置
php admin-cli.php init --password=xxxx        # 设置 / 重置管理员密码
php admin-cli.php token                       # 生成并写入 web_token
php admin-cli.php unlock                      # 清空登录失败计数、解除锁定
php admin-cli.php allow 1.2.3.4 10.0.0.0/8    # 设置 IP 白名单
php admin-cli.php allow --clear               # 清空白名单
php admin-cli.php reset                       # 清除后台改过的全部配置
php admin-cli.php prune                       # 按保留天数清理快照 / 日志 / 审计
```

---

## 七、访问限制

### 7.1 安全（`config.php` 的 `security`）

| 键 | 默认 | 作用 |
| --- | --- | --- |
| `admin_password_hash` | 空 | `password_hash()` 结果，**不可逆**。不要手工填明文 |
| `ip_allowlist` | `[]` | IP / CIDR 白名单。**空 = 不限制；一旦填了就默认拒绝**（fail-closed） |
| `trusted_proxies` | `[]` | 只有直连来源在名单里，才采信 `X-Forwarded-For`。否则一律用 `REMOTE_ADDR` |
| `session_lifetime` | 7200 | 会话空闲超时，超时自动登出 |
| `max_login_failures` | 5 | 按来源 IP 计数，达到阈值后锁定 |
| `lockout_seconds` | 900 | 锁定时长 |
| `protect_status` | `true` | `status.php` 是否也需要登录 |
| `allow_web_trigger` | `true` | 关掉后只能命令行 / 定时任务触发 |

### 7.2 限制（`config.php` 的 `limits`）

| 键 | 默认 | 作用 |
| --- | --- | --- |
| `ip_requests_per_minute` | **5** | 每个 IP 每分钟最多访问几次网页界面（`admin.php` / `status.php` / `fetch.php`）。`0` = 不限 |
| `admin_requests_per_minute` | 120 | 已登录管理员单独一套额度（见下方说明）。`0` = 沿用公共额度 |
| `ip_rate_limit_exempt` | `['127.0.0.1','::1']` | 不受频率限制的来源。**默认放行本机**，阈值调狠了还能进去改回来 |
| `web_min_interval` | 300 | 网页触发两次之间的最小间隔（秒） |
| `web_daily_max` | 24 | 网页触发在滚动 24 小时内的次数上限 |
| `admin_action_interval` | 10 | 后台写操作的最小间隔（秒） |
| `max_runtime_seconds` | 600 | 单次运行时间上限，超时中止并**保留上一轮产物** |
| `max_hosts_lines` | 200000 | hosts 行数上限，超过判定异常、放弃写入 |
| `max_output_bytes` | 67108864 | 单个产物大小上限（64 MB） |
| `max_entries` / `max_domains` | 0 | 解析条目 / 聚合域名上限，`0` = 不限 |

#### 每 IP 每分钟 5 次：两个刻意的设计

超限返回 **429** 并带 `Retry-After` 头，页面会说明「最近一分钟已访问 N 次，超过上限 M 次」。

1. **被拒绝的请求不计数。** 窗口只由**已放行**的请求决定。
   否则攻击者只要不停触发拒绝，就能把窗口无限往后推，把正常用户一起锁死。
2. **管理员用另一套额度（默认 120/分钟）。** 后台有 6 个标签页，沿用 5 次/分钟
   会让管理员自己点两下就被挡住 —— 那等于把这个功能做成了故障。
   想对所有人一律 5 次/分钟，把 `admin_requests_per_minute` 也改成 5 即可。

另外 `ip_rate_limit_exempt` 默认含 `127.0.0.1` / `::1`：万一把阈值调得连自己都进不去，
从本机仍然能打开后台改回来；即使本机也不通，还有 `php admin-cli.php` 这条 CLI 通道。

**为什么要有规模上限**：两个规则源都是「远端可下发、无签名校验」的。万一被投毒或格式突变，
规模闸门会在**写盘之前**拦住，上一轮的产物原样保留 —— 比写出一个坏文件好。

### 7.3 已实现的防护

- **密码**：只存 `password_hash()`；登录用 `password_verify()`（常量时间）
- **CSRF**：所有 POST 都要带会话里的令牌，缺失或不匹配直接 400
- **会话**：cookie 为 `HttpOnly` + `SameSite=Lax`，HTTPS 下自动加 `Secure`；登录时 `session_regenerate_id(true)`
- **暴力破解**：按 IP 计数并锁定 + 登录请求 2 秒节流
- **访问频率**：每个 IP 每分钟最多 5 次网页访问（超限 429 + `Retry-After`），
  已登录管理员走独立额度，本机默认豁免 —— 详见 7.2
- **响应头**：`X-Frame-Options: DENY`、`X-Content-Type-Options: nosniff`、`Referrer-Policy: no-referrer`、`Cache-Control: no-store`、`X-Robots-Tag: noindex`
- **自锁保护**：保存 IP 白名单时若当前 IP 不在名单里，**直接拒绝保存**并提示
- **审计**：登录、改配置、改密码、触发拉取、被拒绝的请求都写 `data/logs/audit-YYYY-MM.jsonl`
- **触发鉴权顺序**：白名单 → 开关 → 令牌 → 限流。被拦掉的请求不读文件、不联网

### 7.4 配置的存放

后台改的值写在 **`data/settings.json`**，不写回 `config.php`：

- 程序不去改自己的源码（改坏了就再也起不来了）
- 升级时可以直接覆盖 `config.php`，用户设置不会丢
- 只有白名单里的键能改，每个键都有类型与范围校验（`ConfigStore::EDITABLE`）

**「保存」是安全的**：表单带了完整性标记 `_form=config`，且每个开关都带一个 `__present`
伴随字段 —— 只有确认该开关真的出现在请求里，「没勾」才等价于「关闭」。
缺标记的请求直接拒绝，缺伴随字段的开关视为「不改动」。
否则一个不完整的请求体就会把规则源、网页触发、状态页保护**全部静默关掉**。

---

## 八、网页触发（可选）

不想用 cron 时，把 `fetch.php` 放到 PHP 站点目录，用外部监控服务定时请求即可：

```
https://example.com/fetch.php?token=<web_token>
```

`web_token` 在 `config.php` 里设置，**留空表示只允许命令行触发**。
`status.php` 提供了一个只读状态面板（最近一次结果、各源状态、产物清单、日志尾部），
配好 `web_token` 后也能在上面点按钮手动触发。

---

## 七、配置要点（`config.php`）

| 键 | 默认 | 说明 |
| --- | --- | --- |
| `http.timeout` / `connect_timeout` | 10 / 6 | 与文档一致；主备源各重试 3 次 |
| `http.use_system_proxy` | `false` | 对应 `UseProxy = false` |
| `http.txt_fallback` | `www.dogfight360.com → cf.dogfight360.com` | 抗污染兜底 |
| `cli.binary` | `''` | **PHP CLI 绝对路径**。留空 = 自动探测；共享主机上填这里最省事 |
| `cli.http_fallback` | `true` | 找不到 CLI 时改用 HTTP 自触发跑拉取（不依赖 `proc_open`） |
| `placeholders.max_lookups` | 200 | DoH 兜底解析的域名上限 |
| `placeholders.cache_ttl` | 604800 | DoH 结果缓存 7 天，避免每天重查 |
| `s302.listen_ip` | `127.0.0.1` | 写 `hosts_s302.txt` 用的监听地址 |
| `output.max_ips_per_domain` | 3 | **每个域名在 hosts.txt 里最多几行**。规则源一个条目可能给上百个地址（Jsdelivr 给了 180 个），全写出来是几十万行且互相冲突；按文档顺序取前 N 个。`0` = 不限 |
| `output.domain_map_max_ips` | 8 | `domains.json` 里每域名的地址上限 |
| `output.prefer_ipv4` | `true` | 同域名同时有 v4/v6 时优先保留 v4 |
| `share.enabled` | `false` | **中转站总开关**。开启后规则通过 `/1`、`/2` 对外发布 |
| `share.usbeam_path` / `s302_path` | `1` / `2` | 两条中转路径，可改；不能相同、不能与程序文件重名 |
| `share.allow_json` | `true` | 是否允许 `?format=json` |
| `share.cors` | `true` | 是否加跨域头（手机浏览器直接抓时需要） |
| `share.max_age` | `3600` | 下游缓存秒数 |
| `retention.archive_days` / `log_days` | 14 / 30 | 快照与日志保留天数 |

---

## 八、目录结构

```
rules-puller/
├── fetch.php              拉取与聚合主入口（CLI / 网页触发）
├── r.php                  中转站：对外发布规则（公开只读，无需登录）
├── admin.php              管理后台（需要登录）
├── status.php             只读状态面板（可选登录）
├── admin-cli.php          命令行管理：init / token / unlock / allow / status / reset / prune
├── config.php             全部可调项与默认值
├── bootstrap.php          自动加载、配置合并、工具函数
├── .htaccess              短网址重写（/1、/2）与 data/ 保护
├── check.sh               语法自检
├── design/
│   └── ADMIN_DESIGN.md    后台视觉规范（设计令牌 + 组件骨架）
├── lib/
│   ├── Http.php           抓取：UA、禁用代理、强制 UTF-8、多源回退、TXT 兜底、并发 DoH
│   ├── Ini.php            保留文档顺序的宽容 INI 解析器
│   ├── UsbeamParser.php   UsbEAm 规则 → 结构化条目（含 [Public] 占位符展开）
│   ├── S302Parser.php     S302 规则解密 + Caddyfile 块解析
│   ├── Aggregator.php     合并去重、规模截断、渲染全部产物
│   ├── Auth.php           会话 / CSRF / 登录节流 / IP 白名单 / 密码哈希
│   ├── RateLimit.php      基于文件的滑动窗口限流
│   ├── ConfigStore.php    后台可改配置的白名单与校验（写 data/settings.json）
│   ├── Audit.php          操作审计（按月 JSONL）
│   ├── WebApp.php         Web 请求管线（安全头 → 白名单 → 会话 → 登录 → 页面）
│   ├── View.php           设计令牌、组件与页面骨架（浅色，无外部资源）
│   ├── Store.php          原子写入、JSON、按天清理
│   ├── Lock.php           单实例锁
│   └── Logger.php         文件 + 标准输出日志
├── cron/
│   ├── pull.sh            定时任务包装脚本
│   └── crontab.example    crontab 示例
└── data/                  运行产物（raw / cache / out / logs / archive / settings.json / state.json）
```

---

## 九、注意事项

1. **法律合规**：本工具仅用于网络加速（游戏平台、开发者资源等），不得用于绕过国家防火墙或访问违法内容。
2. **尊重原作者**：数据来自 UsbEAm Hosts Editor（作者 羽翼城 / Dogfight360）与 Steamcommunity 302，
   产出的 hosts 文件头部已注明来源，请勿移除。
3. **信任边界**：两个规则源都是**远端可下发**且**无签名校验**的（S302 的 `reverse_proxy` 上游完全由服务器决定）。
   本程序只做「拉取 + 合并 + 落盘」，不自动把它们应用到系统 hosts——应用前请自行确认内容。
4. `data/raw/` 里的原始文件是排障的第一手材料，出问题时先看它和 `data/logs/`。
