<?php

/**
 * 规则自动拉取与聚合 —— 配置文件
 *
 * 全部可调项集中在这里，改完无需动代码。
 */

declare(strict_types=1);

return [

    // ---------------------------------------------------------------- 基础
    'timezone' => 'Asia/Shanghai',

    'paths' => [
        'data'    => __DIR__ . '/data',
        'raw'     => __DIR__ . '/data/raw',      // 抓到的原始文件
        'cache'   => __DIR__ . '/data/cache',    // 解析结果缓存（304 时复用）
        'out'     => __DIR__ . '/data/out',      // 最终产物
        'logs'    => __DIR__ . '/data/logs',
        'archive' => __DIR__ . '/data/archive',  // 每日快照
        'state'   => __DIR__ . '/data/state.json',
        'lock'    => __DIR__ . '/data/.pull.lock',
        'settings' => __DIR__ . '/data/settings.json',   // 后台改过的配置覆盖
        'security' => __DIR__ . '/data/security.json',   // 登录失败计数 / 锁定
        'ratelimit' => __DIR__ . '/data/ratelimit.json', // 网页触发节流计数
    ],

    // ---------------------------------------------------------------- HTTP
    'http' => [
        'timeout'          => 10,   // 整体超时（秒），与说明文档一致
        'connect_timeout'  => 6,
        'retries'          => 3,    // 每个 URL 的尝试次数
        'retry_delay_ms'   => 1500,
        'follow_location'  => 5,
        'use_system_proxy' => false, // 禁用系统代理（UseProxy = false）
        'force_utf8'       => true,  // 响应头无 charset，按 UTF-8 处理并纠正乱码
        'doh_endpoints'    => [
            'https://doh.pub/dns-query',
            'https://223.5.5.5/dns-query',
            'https://1.1.1.1/dns-query',
        ],
        // 主源域名被污染时的兜底：从 TXT 记录取备用 IP，再以固定 IP 直连
        'txt_fallback' => [
            'www.dogfight360.com' => 'cf.dogfight360.com',
        ],
        'txt_max_ips' => 8,
    ],

    // ---------------------------------------------------------------- PHP CLI
    // 后台点「立即拉取」时会用 PHP CLI 起子进程。
    // 共享虚拟主机上 PHP_BINDIR / PHP_BINARY 往往指向 php-fpm 或不可访问的路径，
    // 这时按下面的顺序找；都不行则自动退回「同域 HTTP 自触发」（见 fetch.php）。
    'cli' => [
        // 显式指定（面板里通常能看到 PHP CLI 的绝对路径）——填了就优先用它
        'binary' => '',
        // 依次尝试的候选路径；{ver} 会替换成当前 PHP 版本（如 8.1）
        'candidates' => [
            '/usr/local/bin/php',
            '/usr/bin/php',
            '/bin/php',
            '/usr/local/php/bin/php',
            '/usr/local/php/{ver}/bin/php',
            '/www/server/php/{ver}/bin/php',          // 宝塔
            '/www/server/php/{ver}/bin/php-cli',
            '/opt/php/{ver}/bin/php',
            '/usr/local/bin/php{ver}',
            '/usr/bin/php{ver}',
            '/usr/local/php{ver}/bin/php',
        ],
        // 用 glob 兜底搜索的通配符（{ver} 同上）
        'glob_patterns' => [
            '/www/server/php/*/bin/php',
            '/usr/local/php*/bin/php',
            '/opt/php*/bin/php',
            '/usr/bin/php*',
            '/usr/local/bin/php*',
        ],
        // 允许通过写一个 php-cli-path.txt 来指定（内容为绝对路径）
        'path_file' => __DIR__ . '/data/php-cli-path.txt',
        // 找不到 CLI 时，是否允许用 HTTP 自触发兜底（需要 allow_url_fopen 或 curl）
        'http_fallback' => true,
    ],

    // ---------------------------------------------------------------- 数据源
    // enabled = false 可临时停用某个源（也可在后台里改，改的是 data/settings.json）
    'sources' => [
        'usbeam' => [
            'enabled'    => true,
            'label'      => 'UsbEAm Hosts 规则',
            'urls'       => [
                'https://www.dogfight360.com/Usbeam/usbeam_new_40.xml',
                'https://usbeam.steam302.xyz/Usbeam/usbeam_new_40.xml',
            ],
            'user_agent' => 'UsbEAm_Next',   // 必须，服务器靠它识别客户端
            'marker'     => '#end_usbeam',   // 完整性校验标志
            'min_bytes'  => 200000,
            'required'   => true,
        ],
        's302_rules' => [
            'enabled'    => true,
            'label'      => 'Steamcommunity 302 规则',
            'urls'       => [
                'https://www.dogfight360.com/Usbeam/13007P/S302_rules.ini',
            ],
            'user_agent' => 'Steam302',
            'marker'     => '[Rules]',
            'min_bytes'  => 50000,
            'required'   => true,
        ],
        's302_version' => [
            'enabled'    => true,
            'label'      => 'Steamcommunity 302 版本号',
            'urls'       => [
                'https://www.dogfight360.com/Usbeam/S302.ver',
            ],
            'user_agent' => 'Steam302',
            'marker'     => null,
            'min_bytes'  => 1,
            'required'   => false,
        ],
    ],

    // ------------------------------------------------- 占位符（{Cloudflare} 等）
    // 主来源是规则文件自带的 [Public] 段（作者维护的地址表，无需联网）。
    // 只有 [Public] 里没定义的占位符才会走下面的 DoH 兜底。
    'placeholders' => [
        'enabled'         => true,
        'resolve_via_doh' => true,  // [Public] 未定义时的兜底
        'max_lookups'     => 200,   // 单次运行最多解析多少个域名（0 = 不限）
        'concurrency'     => 6,     // 并发查询数（过高容易被 DoH 服务限流）
        'query_timeout'   => 5,     // 单个查询超时（秒）
        'cache_ttl'       => 604800, // 解析结果缓存有效期（秒），默认 7 天
        'static_ips'      => [],
    ],

    // ---------------------------------------------------------------- S302
    's302' => [
        // 双重混淆：明文 --重复异或--> 密文 --Base64--> INI 字段值
        'xor_key'           => "SteamCommunity 302\x124Vx",
        'obfuscated_fields' => ['Json', 'Domain_list', 'Forwarding'],
        // 生成 hosts_s302.txt 时，S302 域名指向的本地监听地址
        'listen_ip'         => '127.0.0.1',
        'hosts_tag'         => '#S302',
    ],

    // ---------------------------------------------------------------- 产物
    'output' => [
        'conflict_policy' => 'union', // union = 同域名多来源地址全部保留
        'hosts_comments'  => true,    // hosts.txt 里带分组注释
        'sort_domains'    => true,

        // 每个域名在 hosts.txt 里最多写几条。规则源里一个条目可能给上百个地址
        // （例如 Jsdelivr 给了 180 个），全写出来是几十万行、且语义上互相冲突。
        // 按文档顺序取前 N 个 —— 作者把更优的排在前面。0 = 不限。
        'max_ips_per_domain' => 3,
        // rules.json 里 domains 映射的每域名地址上限（保真度更高一些）。0 = 不限。
        'domain_map_max_ips' => 8,
        // 同一域名同时有 IPv4/IPv6 时优先保留 IPv4（大多数 hosts 工具只吃 IPv4）
        'prefer_ipv4' => true,
    ],

    // ---------------------------------------------------------------- 地址修正
    // 域名 => 地址列表。列在这里的域名，其地址**整体替换**上游给的那份，
    // 且不受 output.max_ips_per_domain / domain_map_max_ips 的截断影响。
    //
    // 为什么需要这一层：上游 UsbEAm 的某些条目是**为 Steamcommunity302 的本地
    // 反代（MITM）准备的**，它自己在 desc_zh 里就写了「请使用 Steamcommunity302
    // 访问」，cert= 字段也印证了这一点（声明该地址应当出示哪个域名的证书）。
    // 本项目的内核**刻意不做 MITM**，所以这类地址只会超时、或出示错误的证书。
    //
    // 为什么不能靠「按标记自动剔除」：带同一句标记的条目共 11 个，但其中只有
    // github.com 真的坏 —— 其余 10 个（raw.githubusercontent.com、codeload.github.com、
    // store.steampowered.com、discord、imgix、fandom …）排在首位的地址都是
    // Fastly / Akamai / Cloudflare 的真实边缘地址，能正常用。按标记一刀切会把这
    // 10 个一起打坏。所以这是一条**逐域名的、需要证据的判断**，不是规则。
    //
    // 为什么必须放在聚合阶段（而不是拉取阶段）：github.com 的上游条目 20004
    // 其实给了 39 个地址，里面**有** GitHub 的真实地址（140.82.x、20.205.243.166），
    // 但它们排在 11 位之后，而作者把 S302 取向的 Azure 地址排在了最前面
    // （51.142.105.107、20.218.253.22、20.12.240.255 …）。截断（默认只取前 3 条）
    // 恰好把真实地址全切掉了 —— 设备上表现为 3 个候选全部超时、内核
    // `flow.failed = true; socket.abort()`，浏览器看到 ERR_CONNECTION_RESET。
    // 所以修正必须发生在「域名地址表定稿之后」，才能覆盖截断的结果。
    //
    // github.com：20.205.243.166 经设备实测可用（替换后 github.com 整页加载成功、
    // tun0 RX +6.07 MB，githubassets / avatars.githubusercontent / collector 等
    // 子资源均正常）。其余为 GitHub 官方公布的 Web 段地址，作冗余。
    //
    // ⚠️ 删掉这条之前请先复现上面的问题：上游一旦把真实地址重新排到前面，
    // 这条修正才可能变得多余 —— 在那之前删掉它等于把 github.com 打回不可用。
    'corrections' => [
        'github.com' => [
            '20.205.243.166',
            '140.82.112.3',
            '140.82.113.3',
            '140.82.114.3',
        ],
    ],

    // ---------------------------------------------------------------- 中转站
    // 把拉取到的规则**对外发布**成固定短网址，供别人 / 别的客户端直接订阅。
    // 例：https://你的域名/1 → UsbEAm hosts；https://你的域名/2 → S302 hosts。
    //
    // 默认**关闭**：对外发布等于把流量和内容都公开出去，应当由你主动开启。
    'share' => [
        // 总开关。关掉后所有中转地址一律 404，不影响后台与定时任务。
        'enabled' => false,

        // 两个中转点的路径（不含前导斜杠）。改这里就能换地址。
        // 只允许字母数字、下划线、连字符、点，且不能和真实存在的文件同名。
        'usbeam_path' => '1',
        's302_path'   => '2',

        // 每个中转点允许的内容格式。默认 hosts 文本；带 ?format=json 时给 JSON。
        // 允许值：hosts、json（hosts 是默认，客户端不传 format 时用它）
        'default_format' => 'hosts',

        // 是否允许 ?format=json（关掉则只发 hosts 文本）
        'allow_json' => true,

        // 是否允许跨域读取（Access-Control-Allow-Origin: *）。
        // 手机上用浏览器 / 网页工具直接抓规则时需要；纯命令行客户端不需要。
        'cors' => true,

        // 下游可缓存多少秒（Cache-Control: public, max-age=N）。
        // 规则一天才更新一次，缓存久一点能省你主机的流量。
        'max_age' => 3600,

        // 是否带上 Source 说明（指向你的域名）与生成时间注释。
        'send_metadata' => true,
    ],

    // ---------------------------------------------------------------- 保留
    'retention' => [
        'archive_days' => 14,
        'log_days'     => 30,
    ],

    // ---------------------------------------------------------------- 安全
    // 这些值也可以在后台「安全」页里改（存到 data/settings.json，不写回本文件）。
    'security' => [
        // 管理员密码的 password_hash() 结果。
        // 初始设置：php admin-cli.php init
        // 也可以首次从本机 127.0.0.1 打开 admin.php 时在页面上设置。
        'admin_password_hash' => '',

        // 允许访问后台 / 状态页 / 网页触发的 IP 或 CIDR；留空 = 不限制。
        // 例：['127.0.0.1', '::1', '10.0.0.0/8', '203.0.113.7']
        'ip_allowlist' => [],

        // 反向代理（Nginx / Cloudflare）后面时，填可信代理 IP，
        // 否则一律用 REMOTE_ADDR —— 因为 X-Forwarded-For 可以被伪造。
        'trusted_proxies' => [],

        // 登录会话有效期（秒），超时需重新登录
        'session_lifetime' => 7200,
        // 连续登录失败几次后锁定
        'max_login_failures' => 5,
        // 锁定时长（秒）
        'lockout_seconds' => 900,

        // status.php 是否也需要登录（默认需要）
        'protect_status' => true,
        // 是否允许通过 HTTP 触发拉取（关掉就只能命令行 / cron）
        'allow_web_trigger' => true,
    ],

    // ---------------------------------------------------------------- 限制
    'limits' => [
        // ── 按 IP 的网页访问频率 ──
        // 每个 IP 每分钟最多访问几次网页界面（admin.php / status.php / fetch.php）。
        // 0 = 不限制。被拒绝的请求不计数，窗口由已放行的请求决定。
        'ip_requests_per_minute' => 5,
        // 已登录的管理员单独一套上限：后台有 6 个标签页，5 次/分钟会把管理员自己挡住。
        // 想对所有人一律 5 次/分钟，把这里也改成 5。
        'admin_requests_per_minute' => 120,
        // 不受频率限制的来源。默认放行本机 —— 阈值调狠了还能从 127.0.0.1 进去改回来。
        // 留空 = 所有 IP 一律受限。
        'ip_rate_limit_exempt' => ['127.0.0.1', '::1'],

        // 网页触发两次之间的最小间隔（秒）—— 防止被反复点击刷爆
        'web_min_interval'   => 300,
        // 网页触发在最近 24 小时内的最大次数
        'web_daily_max'      => 24,
        // 后台写操作（改配置 / 清缓存等）的最小间隔（秒）
        'admin_action_interval' => 10,

        // 单次运行的时间上限（秒），超时中止并保留上一次产物
        'max_runtime_seconds' => 600,

        // 产物规模上限，超过则判定为异常并放弃写入（防止规则源被投毒撑爆磁盘）
        'max_hosts_lines'  => 200000,
        'max_output_bytes' => 67108864,   // 单个产物 64 MB

        // 解析规模上限，0 = 不限
        'max_entries' => 0,
        'max_domains' => 0,
    ],

    // 允许通过 HTTP 触发拉取时的令牌；留空 = 只允许命令行触发
    'web_token' => '',

];
