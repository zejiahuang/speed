<?php

declare(strict_types=1);

namespace RulesPuller;

/**
 * 运行期可覆盖的配置。
 *
 * 后台里改的值写在 `data/settings.json`，**不写回 config.php**：
 *   - 避免程序去改自己的源码（出错就再也起不来了）
 *   - 升级时 config.php 可以直接覆盖，用户设置不会丢
 * 只允许改白名单里的键，且每个键都有类型与范围校验。
 */
final class ConfigStore
{
    /**
     * 可编辑键的白名单：键 => [类型, 范围/说明, 分组, 显示名, 帮助文字]
     *
     * 类型：int | bool | ip | lines | url_lines
     */
    private const EDITABLE = [
        // ---------------------------------------------------------- 抓取
        'http.timeout' => [
            'type' => 'int', 'min' => 1, 'max' => 120,
            'group' => '抓取', 'label' => '整体超时（秒）',
            'help' => '与文档一致默认 10 秒。单个请求从连接到收完响应的总预算。',
        ],
        'http.connect_timeout' => [
            'type' => 'int', 'min' => 1, 'max' => 60,
            'group' => '抓取', 'label' => '连接超时（秒）',
            'help' => '仅建立 TCP + TLS 的时间，必须小于整体超时。',
        ],
        'http.retries' => [
            'type' => 'int', 'min' => 1, 'max' => 10,
            'group' => '抓取', 'label' => '每个地址重试次数',
            'help' => '主源失败后才会走备源，所以这个值不宜太大。',
        ],
        'cli.binary' => [
            'type' => 'path',
            'group' => '抓取', 'label' => 'PHP CLI 绝对路径',
            'help' => '留空 = 自动探测。共享主机上若报「找不到 PHP CLI」，'
                . '在面板里查到 php 命令行路径后填这里，例如 /www/server/php/81/bin/php。'
                . '填了会优先使用；填错会自动忽略并继续自动探测。',
        ],
        'cli.http_fallback' => [
            'type' => 'bool',
            'group' => '抓取', 'label' => '找不到 CLI 时用 HTTP 兜底',
            'help' => '开启后，后台点「立即拉取」在无 CLI 时会改为请求自身 fetch.php，'
                . '不依赖 PHP CLI 与 proc_open。这是共享主机上的推荐设置。',
        ],

        // ---------------------------------------------------------- 中转站
        'share.enabled' => [
            'type' => 'bool',
            'group' => '中转站', 'label' => '对外发布规则（中转站）',
            'help' => '开启后，别人可以通过固定短网址订阅你聚合好的规则。'
                . '关闭时所有中转地址返回 404，不影响后台与定时任务。',
        ],
        'share.usbeam_path' => [
            'type' => 'slug',
            'group' => '中转站', 'label' => 'UsbEAm 规则路径',
            'help' => '不含前导斜杠。默认 1 → 网址/1。只允许字母、数字、下划线、连字符、点。',
        ],
        'share.s302_path' => [
            'type' => 'slug',
            'group' => '中转站', 'label' => 'S302 规则路径',
            'help' => '不含前导斜杠。默认 2 → 网址/2。两条路径不能相同。',
        ],
        'share.allow_json' => [
            'type' => 'bool',
            'group' => '中转站', 'label' => '允许 JSON 输出',
            'help' => '开启后，在中转地址后加 ?format=json 会返回结构化 JSON；'
                . '不加则返回 hosts 文本。',
        ],
        'share.cors' => [
            'type' => 'bool',
            'group' => '中转站', 'label' => '允许跨域读取',
            'help' => '加上 Access-Control-Allow-Origin: *。手机浏览器 / 网页版工具直接抓规则时需要。',
        ],
        'share.max_age' => [
            'type' => 'int', 'min' => 0, 'max' => 604800,
            'group' => '中转站', 'label' => '下游缓存秒数',
            'help' => '告诉下游（浏览器 / 客户端）可以缓存多久。规则一天才更新一次，'
                . '默认 3600 秒；0 = 不缓存。',
        ],
        'share.send_metadata' => [
            'type' => 'bool',
            'group' => '中转站', 'label' => '输出里带来源注释',
            'help' => '在 hosts / JSON 顶部标出生成时间与来源说明，方便别人确认拿到的是新数据。',
        ],
        'placeholders.resolve_via_doh' => [
            'type' => 'bool',
            'group' => '抓取', 'label' => '占位符允许 DoH 兜底',
            'help' => '规则文件自带的 [Public] 段已覆盖全部占位符，这一项只在源格式变化时才用得上。',
        ],
        'placeholders.max_lookups' => [
            'type' => 'int', 'min' => 0, 'max' => 5000,
            'group' => '抓取', 'label' => 'DoH 解析域名上限',
            'help' => '0 = 不限。',
        ],
        'placeholders.concurrency' => [
            'type' => 'int', 'min' => 1, 'max' => 32,
            'group' => '抓取', 'label' => 'DoH 并发数',
            'help' => '过高会被公共 DoH 服务限流，实测 6 比较稳。',
        ],

        // ---------------------------------------------------------- 产物
        'output.max_ips_per_domain' => [
            'type' => 'int', 'min' => 0, 'max' => 100,
            'group' => '产物', 'label' => 'hosts 每域名最多几行',
            'help' => '0 = 不限。规则源里一个条目可能给上百个地址，全写出来会得到几十万行互相冲突的记录。',
        ],
        'output.domain_map_max_ips' => [
            'type' => 'int', 'min' => 0, 'max' => 500,
            'group' => '产物', 'label' => 'domains.json 每域名地址上限',
            'help' => '0 = 不限。',
        ],
        'output.hosts_comments' => [
            'type' => 'bool',
            'group' => '产物', 'label' => 'hosts.txt 带分组注释',
            'help' => '关掉可让文件更小、更适合直接喂给工具解析。',
        ],
        'output.prefer_ipv4' => [
            'type' => 'bool',
            'group' => '产物', 'label' => '优先保留 IPv4',
            'help' => '同域名同时有 v4/v6 时，避免 IPv6 把 IPv4 从上限里挤掉。',
        ],
        's302.listen_ip' => [
            'type' => 'ip',
            'group' => '产物', 'label' => 'S302 监听地址',
            'help' => '写 hosts_s302.txt 用。对应 S302.ini 的 listen_ip，默认 127.0.0.1。',
        ],
        'retention.archive_days' => [
            'type' => 'int', 'min' => 0, 'max' => 365,
            'group' => '产物', 'label' => '快照保留天数',
            'help' => '0 = 不清理。',
        ],
        'retention.log_days' => [
            'type' => 'int', 'min' => 0, 'max' => 365,
            'group' => '产物', 'label' => '日志保留天数',
            'help' => '0 = 不清理。',
        ],

        // ---------------------------------------------------------- 限制
        'limits.ip_requests_per_minute' => [
            'type' => 'int', 'min' => 0, 'max' => 100000,
            'group' => '限制', 'label' => '每 IP 每分钟访问上限',
            'help' => '默认 5 次。0 = 不限制。被拒绝的请求不计数。',
        ],
        'limits.admin_requests_per_minute' => [
            'type' => 'int', 'min' => 0, 'max' => 100000,
            'group' => '限制', 'label' => '已登录管理员的每分钟上限',
            'help' => '后台有 6 个标签页，沿用 5 次会把管理员自己挡住，所以单独给一个额度。要一律 5 次就填 5。',
        ],
        'limits.ip_rate_limit_exempt' => [
            'type' => 'lines',
            'group' => '限制', 'label' => '不受频率限制的 IP',
            'help' => '每行一个 IP 或 CIDR。默认含 127.0.0.1 —— 阈值调狠了还能从本机进去改回来。留空 = 所有 IP 一律受限。',
        ],
        'limits.web_min_interval' => [
            'type' => 'int', 'min' => 0, 'max' => 86400,
            'group' => '限制', 'label' => '网页触发最小间隔（秒）',
            'help' => '两次网页触发之间至少要隔这么久，防止被连点。0 = 不限制。',
        ],
        'limits.web_daily_max' => [
            'type' => 'int', 'min' => 0, 'max' => 1000,
            'group' => '限制', 'label' => '网页触发 24 小时上限',
            'help' => '按滚动 24 小时计。0 = 不限制。',
        ],
        'limits.admin_action_interval' => [
            'type' => 'int', 'min' => 0, 'max' => 3600,
            'group' => '限制', 'label' => '后台操作最小间隔（秒）',
            'help' => '改配置、清缓存等写操作的节流。0 = 不限制。',
        ],
        'limits.max_runtime_seconds' => [
            'type' => 'int', 'min' => 30, 'max' => 3600,
            'group' => '限制', 'label' => '单次运行时间上限（秒）',
            'help' => '超时中止并保留上一次的产物。',
        ],
        'limits.max_hosts_lines' => [
            'type' => 'int', 'min' => 0, 'max' => 5000000,
            'group' => '限制', 'label' => 'hosts 行数上限',
            'help' => '超过就判定异常、放弃写入。0 = 不限。防止规则源被投毒撑爆磁盘。',
        ],
        'limits.max_output_bytes' => [
            'type' => 'int', 'min' => 0, 'max' => 1073741824,
            'group' => '限制', 'label' => '单个产物大小上限（字节）',
            'help' => '0 = 不限。',
        ],
        'limits.max_entries' => [
            'type' => 'int', 'min' => 0, 'max' => 1000000,
            'group' => '限制', 'label' => '解析条目上限',
            'help' => '0 = 不限。',
        ],
        'limits.max_domains' => [
            'type' => 'int', 'min' => 0, 'max' => 1000000,
            'group' => '限制', 'label' => '聚合域名上限',
            'help' => '0 = 不限。超出后按字典序保留前 N 个。',
        ],

        // ---------------------------------------------------------- 安全
        'security.protect_status' => [
            'type' => 'bool',
            'group' => '安全', 'label' => 'status.php 需要登录',
            'help' => '关掉后状态页任何人都能看（只读，不含敏感信息，但会暴露规则规模）。',
        ],
        'security.allow_web_trigger' => [
            'type' => 'bool',
            'group' => '安全', 'label' => '允许网页触发拉取',
            'help' => '关掉后只能靠命令行 / 定时任务触发。',
        ],
        'security.session_lifetime' => [
            'type' => 'int', 'min' => 300, 'max' => 604800,
            'group' => '安全', 'label' => '登录会话有效期（秒）',
            'help' => '空闲超过这个时长就要重新登录。',
        ],
        'security.max_login_failures' => [
            'type' => 'int', 'min' => 1, 'max' => 100,
            'group' => '安全', 'label' => '登录失败几次后锁定',
            'help' => '按来源 IP 计数。',
        ],
        'security.lockout_seconds' => [
            'type' => 'int', 'min' => 0, 'max' => 86400,
            'group' => '安全', 'label' => '锁定时长（秒）',
            'help' => '0 = 只计数不锁定。',
        ],
        'security.ip_allowlist' => [
            'type' => 'lines',
            'group' => '安全', 'label' => 'IP 白名单',
            'help' => '每行一个 IP 或 CIDR。留空 = 不限制。填了之后，不在名单里的来源连后台和状态页都打不开。',
        ],
        'security.trusted_proxies' => [
            'type' => 'lines',
            'group' => '安全', 'label' => '可信反向代理 IP',
            'help' => '只有当直连来源在这个名单里，才会采信 X-Forwarded-For。留空 = 一律用 REMOTE_ADDR。',
        ],

        // ---------------------------------------------------------- 数据源
        'sources.usbeam.enabled' => [
            'type' => 'bool',
            'group' => '数据源', 'label' => '启用 UsbEAm Hosts 规则',
            'help' => '',
        ],
        'sources.usbeam.urls' => [
            'type' => 'url_lines',
            'group' => '数据源', 'label' => 'UsbEAm 源地址',
            'help' => '第一行是主源，其余按顺序作备源。只允许 http/https。',
        ],
        'sources.s302_rules.enabled' => [
            'type' => 'bool',
            'group' => '数据源', 'label' => '启用 Steamcommunity 302 规则',
            'help' => '',
        ],
        'sources.s302_rules.urls' => [
            'type' => 'url_lines',
            'group' => '数据源', 'label' => 'S302 源地址',
            'help' => '',
        ],
        'sources.s302_version.enabled' => [
            'type' => 'bool',
            'group' => '数据源', 'label' => '启用 S302 版本检查',
            'help' => '可选源，失败不影响整体。',
        ],
    ];

    private string $path;

    public function __construct(string $path)
    {
        $this->path = $path;
    }

    /**
     * 后台可编辑的键（供 UI 渲染表单）。
     *
     * @return array<string,array>
     */
    public static function editable(): array
    {
        return self::EDITABLE;
    }

    public static function isEditable(string $key): bool
    {
        return isset(self::EDITABLE[$key]);
    }

    /**
     * @return array<string,mixed>
     */
    public function overrides(): array
    {
        $data = Store::readJson($this->path);

        return is_array($data) ? $data : [];
    }

    public function reset(): bool
    {
        return Store::writeJson($this->path, []);
    }

    /**
     * 单独写入管理员密码哈希。
     *
     * 刻意不放进 EDITABLE 白名单：那是给通用表单用的，
     * 密码哈希既不该在表单里回显，也不该被批量保存覆盖掉。
     */
    public function setPasswordHash(string $hash): bool
    {
        $overrides = $this->overrides();
        if (!isset($overrides['security']) || !is_array($overrides['security'])) {
            $overrides['security'] = [];
        }
        $overrides['security']['admin_password_hash'] = $hash;

        return Store::writeJson($this->path, $overrides);
    }

    /**
     * 设置 / 清除网页触发令牌。空字符串表示关闭网页触发入口。
     */
    public function setWebToken(string $token): bool
    {
        $token     = trim($token);
        $overrides = $this->overrides();

        if ($token !== '' && strlen($token) < 16) {
            return false;
        }

        // 空串也要写进去：否则 config.php 里写过的旧令牌会继续生效
        $overrides['web_token'] = $token;

        return Store::writeJson($this->path, $overrides);
    }

    public function webTokenIsOverridden(): bool
    {
        return array_key_exists('web_token', $this->overrides());
    }

    /**
     * 清掉密码哈希（用于重置）。
     */
    public function clearPasswordHash(): bool
    {
        $overrides = $this->overrides();
        if (isset($overrides['security']['admin_password_hash'])) {
            unset($overrides['security']['admin_password_hash']);
        }

        return Store::writeJson($this->path, $overrides);
    }

    /**
     * 校验并写入一个键。
     *
     * @return array{ok:bool,error:?string,value:mixed}
     */
    public function set(string $key, $raw): array
    {
        $spec = self::EDITABLE[$key] ?? null;
        if ($spec === null) {
            return ['ok' => false, 'error' => '不允许修改这个配置项：' . $key, 'value' => null];
        }

        $parsed = $this->parseValue($key, $spec, $raw);
        if (!$parsed['ok']) {
            return $parsed;
        }

        $overrides = $this->overrides();
        self::assign($overrides, $key, $parsed['value']);
        if (!Store::writeJson($this->path, $overrides)) {
            return ['ok' => false, 'error' => '写入 settings.json 失败，请检查 data/ 目录权限', 'value' => null];
        }

        return ['ok' => true, 'error' => null, 'value' => $parsed['value']];
    }

    /**
     * 从请求里读值。
     *
     * ⚠️ 必须带完整性标记 `_form=config`。
     * 表单里开关「没提交」= 关闭，这是 HTML 的正常语义；但一旦请求体不完整
     * （表单结构被改坏、被程序截断、被中间件吃掉字段），所有开关会**静默全部关闭** ——
     * 关掉规则源、关掉网页触发、关掉状态页保护，而且没有任何报错。
     * 所以宁可拒掉一个不完整的请求，也不要写进一份「全关」的配置。
     *
     * @param array<string,mixed> $input
     * @return array{ok:bool,errors:string[],changed:string[]}
     */
    public function saveFromForm(array $input): array
    {
        if ((string) ($input['_form'] ?? '') !== 'config') {
            return [
                'ok'      => false,
                'errors'  => ['表单缺少完整性标记，已拒绝保存 —— 否则没提交的开关会被全部关闭'],
                'changed' => [],
            ];
        }

        $errors  = [];
        $changed = [];
        $seen    = [];   // 本次表单里真实出现的值，供跨字段检查用

        foreach (self::EDITABLE as $key => $spec) {
            $field = self::fieldName($key);

            if ($spec['type'] === 'bool') {
                // 开关必须带「已提交」伴随字段。只有确认这个开关真的出现在表单里，
                // 「没勾」才等价于「关闭」；否则视为不改动。
                // 这样即便请求体不完整，也不会把没提交的开关静默全部关掉。
                if (!array_key_exists($field . '__present', $input)) {
                    continue;
                }
                $value = !empty($input[$field]);
            } else {
                if (!array_key_exists($field, $input)) {
                    continue;
                }
                $value = $input[$field];
            }

            $result = $this->set($key, $value);
            if (!$result['ok']) {
                $errors[] = $result['error'];
                continue;
            }
            $seen[$key] = $result['value'];
            $changed[] = $key;
        }

        // 跨字段检查：两条中转路径不能撞在一起，否则后者永远读不到。
        // 用「本次提交的值」优先，没提交则回落到已保存的覆盖值。
        if ($errors === []) {
            $ov = $this->overrides();
            $usbeam = (string) ($seen['share.usbeam_path']
                ?? $ov['share']['usbeam_path'] ?? '1');
            $s302 = (string) ($seen['share.s302_path']
                ?? $ov['share']['s302_path'] ?? '2');
            if ($usbeam !== '' && strcasecmp($usbeam, $s302) === 0) {
                $errors[] = '两条中转路径不能相同（都是「' . $usbeam . '」）';
            }
        }

        return ['ok' => $errors === [], 'errors' => $errors, 'changed' => $changed];
    }

    /**
     * 表单字段名（把点换成下划线，避免 PHP 把 `.` 变成 `_`）。
     */
    public static function fieldName(string $key): string
    {
        return 'cfg_' . str_replace('.', '__', $key);
    }

    // ------------------------------------------------------------ 内部

    /**
     * @return array{ok:bool,error:?string,value:mixed}
     */
    private function parseValue(string $key, array $spec, $raw): array
    {
        $type = $spec['type'];

        switch ($type) {
            case 'int':
                $raw = is_string($raw) ? trim($raw) : $raw;
                if (!is_numeric($raw)) {
                    return ['ok' => false, 'error' => $spec['label'] . '：需要填数字', 'value' => null];
                }
                $value = (int) $raw;
                if (isset($spec['min']) && $value < $spec['min']) {
                    return ['ok' => false, 'error' => $spec['label'] . '：不能小于 ' . $spec['min'], 'value' => null];
                }
                if (isset($spec['max']) && $value > $spec['max']) {
                    return ['ok' => false, 'error' => $spec['label'] . '：不能大于 ' . $spec['max'], 'value' => null];
                }

                return ['ok' => true, 'error' => null, 'value' => $value];

            case 'bool':
                return ['ok' => true, 'error' => null, 'value' => (bool) $raw];

            case 'ip':
                $raw = trim((string) $raw);
                if ($raw === '') {
                    return ['ok' => false, 'error' => $spec['label'] . '：不能为空', 'value' => null];
                }
                if (!rp_is_ip($raw) && !self::isLoopbackName($raw)) {
                    return ['ok' => false, 'error' => $spec['label'] . '：不是合法 IP（如 127.0.0.1）', 'value' => null];
                }

                return ['ok' => true, 'error' => null, 'value' => $raw];

            case 'path':
                // 允许留空（表示"不指定，自动探测"）
                $raw = trim((string) $raw);
                if ($raw === '') {
                    return ['ok' => true, 'error' => null, 'value' => ''];
                }
                if (!rp_admin_path_is_absolute($raw)) {
                    return ['ok' => false, 'error' => $spec['label'] . '：需要填绝对路径（以 / 开头）', 'value' => null];
                }
                if (str_contains(str_replace('\\', '/', $raw), '..')) {
                    return ['ok' => false, 'error' => $spec['label'] . '：路径里不能出现 ..', 'value' => null];
                }

                return ['ok' => true, 'error' => null, 'value' => $raw];

            case 'slug':
                // 中转站路径：不含前导斜杠的 URL 片段，如 "1"、"usbeam"。
                // 只允许保守字符集 —— 它会被拼进 Location / 重写规则，
                // 放开就是路径穿越与开放重定向的口子。
                $raw = trim((string) $raw, " \t\n\r\0\x0B/");
                if ($raw === '') {
                    return ['ok' => false, 'error' => $spec['label'] . '：不能为空', 'value' => null];
                }
                if (!preg_match('/^[A-Za-z0-9._-]+$/', $raw)) {
                    return [
                        'ok' => false,
                        'error' => $spec['label'] . '：只能用字母、数字、下划线、连字符、点',
                        'value' => null,
                    ];
                }
                if (str_contains($raw, '..')) {
                    return ['ok' => false, 'error' => $spec['label'] . '：不能出现 ..', 'value' => null];
                }
                // 与真实文件同名会打不开（服务器优先给静态文件）
                $reserved = [
                    'index', 'admin', 'admin-cli', 'fetch', 'status', 'r',
                    'bootstrap', 'config', 'lib', 'cron', 'data', 'robots.txt',
                ];
                if (in_array(strtolower($raw), $reserved, true)) {
                    return [
                        'ok' => false,
                        'error' => $spec['label'] . '：「' . $raw . '」会和程序自己的文件冲突，换一个',
                        'value' => null,
                    ];
                }

                return ['ok' => true, 'error' => null, 'value' => $raw];

            case 'lines':
                $lines = self::toLines($raw);
                foreach ($lines as $line) {
                    if (!self::isIpOrCidr($line)) {
                        return ['ok' => false, 'error' => $spec['label'] . '：' . $line . ' 不是合法 IP 或 CIDR', 'value' => null];
                    }
                }

                return ['ok' => true, 'error' => null, 'value' => $lines];

            case 'url_lines':
                $lines = self::toLines($raw);
                if ($lines === []) {
                    return ['ok' => false, 'error' => $spec['label'] . '：至少要留一个地址', 'value' => null];
                }
                foreach ($lines as $line) {
                    $scheme = strtolower((string) parse_url($line, PHP_URL_SCHEME));
                    if (!in_array($scheme, ['http', 'https'], true) || parse_url($line, PHP_URL_HOST) === null) {
                        return ['ok' => false, 'error' => $spec['label'] . '：' . $line . ' 不是合法的 http/https 地址', 'value' => null];
                    }
                }

                return ['ok' => true, 'error' => null, 'value' => $lines];
        }

        return ['ok' => false, 'error' => '未知的配置类型：' . $type, 'value' => null];
    }

    /**
     * @param mixed $raw
     * @return string[]
     */
    private static function toLines($raw): array
    {
        if (is_array($raw)) {
            $pieces = $raw;
        } else {
            $pieces = preg_split('/[\r\n,]+/', (string) $raw) ?: [];
        }

        $out = [];
        foreach ($pieces as $piece) {
            $piece = trim((string) $piece);
            if ($piece !== '') {
                $out[] = $piece;
            }
        }

        return array_values(array_unique($out));
    }

    private static function isIpOrCidr(string $value): bool
    {
        if (rp_is_ip($value)) {
            return true;
        }
        if (strpos($value, '/') === false) {
            return false;
        }
        [$ip, $prefix] = explode('/', $value, 2) + [null, null];
        if ($ip === null || $prefix === null || !ctype_digit($prefix) || !rp_is_ip($ip)) {
            return false;
        }
        $max = strpos($ip, ':') === false ? 32 : 128;

        return (int) $prefix >= 0 && (int) $prefix <= $max;
    }

    private static function isLoopbackName(string $value): bool
    {
        return in_array(strtolower($value), ['localhost'], true);
    }

    private static function assign(array &$target, string $key, $value): void
    {
        $parts = explode('.', $key);
        $cursor = &$target;
        $last = array_pop($parts);
        foreach ($parts as $part) {
            if (!isset($cursor[$part]) || !is_array($cursor[$part])) {
                $cursor[$part] = [];
            }
            $cursor = &$cursor[$part];
        }
        $cursor[$last] = $value;
    }

    /**
     * 递归覆盖：$over 里的键覆盖 $base，数组递归合并（但列表整体替换）。
     */
    public static function deepMerge(array $base, array $over): array
    {
        foreach ($over as $key => $value) {
            if (is_array($value) && isset($base[$key]) && is_array($base[$key]) && self::isAssoc($value)) {
                $base[$key] = self::deepMerge($base[$key], $value);
            } else {
                $base[$key] = $value;
            }
        }

        return $base;
    }

    private static function isAssoc(array $array): bool
    {
        if ($array === []) {
            return false;
        }

        return array_keys($array) !== range(0, count($array) - 1);
    }
}
