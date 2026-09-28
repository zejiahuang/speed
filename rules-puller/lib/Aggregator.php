<?php

declare(strict_types=1);

namespace RulesPuller;

/**
 * 聚合器：把 UsbEAm hosts 规则与 S302 规则合并成统一产物。
 *
 * 合并规则：
 *  - 域名 → 地址集合：同一域名多个来源给出的地址**全部保留**（union），并记录来源
 *  - 域名 → 替代上游：来自 S302 各服务段的 reverse_proxy 目标
 *  - 通配符域名单独成表，不写进 hosts
 */
final class Aggregator
{
    /**
     * hosts.txt 里给「被 corrections 修正过的域名」用的分组名。
     *
     * 单独成组而不是塞回原分组，是为了让产物自己说明哪些地址是人工改过的 ——
     * 下一眼看到 hosts.txt 的人能立刻分辨「上游给错了、我们改过」和「上游本来
     * 就这样」。原分组里那个域名的上游行会被删掉，不会两份并存。
     */
    private const CORRECTION_SECTION = 'Corrections';

    private array $cfg;
    private Logger $log;

    /** @var array<string,string[]> DoH 解析结果缓存，避免同一域名重复查询 */
    private array $resolveCache = [];

    public function __construct(array $cfg, Logger $log)
    {
        $this->cfg = $cfg;
        $this->log = $log;
    }

    /**
     * config.php 的 `corrections`，过滤成「域名 => 有效地址列表」。
     *
     * 域名统一小写 —— 上游域名表本来就是小写，修正表里手写的大写若直接参与
     * 比较会匹配不上，表现为「改了配置却没生效」。
     *
     * 地址先过 `rp_is_ip`：修正表是人手写的，写错一个字符就应当在这里被丢掉
     * 并留下一条日志，而不是把一行非法内容写进 hosts.txt —— 那会让整份产物
     * 在客户端那边解析失败。
     *
     * @return array<string,string[]>
     */
    private function corrections(): array
    {
        $raw = (array) ($this->cfg['corrections'] ?? []);
        $out = [];
        foreach ($raw as $domain => $ips) {
            $domain = strtolower(trim((string) $domain));
            if ($domain === '') {
                continue;
            }
            $clean = array_values(array_unique(array_filter((array) $ips, 'rp_is_ip')));
            if ($clean === []) {
                $this->log->warn('地址修正里没有有效地址，已忽略该域名', ['域名' => $domain]);
                continue;
            }
            $out[$domain] = $clean;
        }

        return $out;
    }

    /**
     * @param array|null    $usbeam  UsbeamParser::parse() 的结果
     * @param array|null    $s302    S302Parser::parse() 的结果
     * @param callable|null $resolver fn(string[] $domains): array<string,string[]>
     *                               批量解析域名，用于展开 {Cloudflare} 这类占位符
     */
    public function build(?array $usbeam, ?array $s302, ?callable $resolver = null): array
    {
        $domainMap   = [];
        $hostsBlocks = [];
        $seenPairs   = [];
        $wildcards   = [];
        $warnings    = [];
        $truncated   = 0;

        // 人工地址修正，见 config.php 的 corrections。在这里取一次，下面
        // 既要用它覆盖地址表，也要用它决定哪些域名不受 domain_map_max_ips 截断。
        $corrections = $this->corrections();

        $placeholderStats = [
            'entries'          => 0,
            'resolved_doh'     => 0,
            'resolved_static'  => 0,
            'unresolved'       => 0,
            'unresolved_names' => [],
        ];

        // ------------------------------------------------ 占位符域名预解析（一次批量）
        $this->primeResolveCache($usbeam, $resolver);

        // ---------------------------------------------------------- UsbEAm
        if ($usbeam !== null) {
            foreach ($usbeam['entries'] as $entry) {
                $ips = $entry['ips'];

                foreach ($entry['placeholders'] as $name) {
                    $placeholderStats['entries']++;
                    [$resolved, $how] = $this->resolvePlaceholder($name, $entry);
                    if ($resolved === []) {
                        $placeholderStats['unresolved']++;
                        $placeholderStats['unresolved_names'][$name] = true;
                        continue;
                    }
                    $placeholderStats[$how === 'doh' ? 'resolved_doh' : 'resolved_static']++;
                    $ips = array_merge($ips, $resolved);
                }

                $ips = array_values(array_unique(array_filter($ips, 'rp_is_ip')));
                if ($ips === []) {
                    continue;
                }
                $ips = self::preferIpv4($ips, !empty($this->cfg['output']['prefer_ipv4']));

                $section = (string) $entry['section'];
                if (!isset($hostsBlocks[$section])) {
                    $hostsBlocks[$section] = [];
                }

                $hostsIps = $ips;
                $cap      = (int) ($this->cfg['output']['max_ips_per_domain'] ?? 0);
                if ($cap > 0 && count($hostsIps) > $cap) {
                    $hostsIps = array_slice($hostsIps, 0, $cap);
                    $truncated++;
                }

                foreach ($entry['domains'] as $domain) {
                    self::addDomain($domainMap, $domain, 'usbeam', $section, null);
                    foreach ($ips as $ip) {
                        self::addIp($domainMap, $domain, $ip);
                    }
                    foreach ($hostsIps as $ip) {
                        $pairKey = $ip . '|' . $domain;
                        if (isset($seenPairs[$pairKey])) {
                            continue;
                        }
                        $seenPairs[$pairKey] = true;
                        $hostsBlocks[$section][] = [
                            'ip'     => $ip,
                            'domain' => $domain,
                            'entry'  => $entry['id'],
                        ];
                    }
                }
            }
        }

        // ---------------------------------------------------------- S302
        $upstreams   = [];
        $s302Domains = [];

        if ($s302 !== null) {
            foreach ($s302['rules']['domain_list'] as $wildcard) {
                $wildcards[$wildcard] = true;
            }

            foreach ($s302['services'] as $service) {
                foreach ($service['wildcards'] as $wildcard) {
                    $wildcards[$wildcard] = true;
                }

                foreach ($service['domains'] as $domain) {
                    $s302Domains[$domain] = true;
                    self::addDomain($domainMap, $domain, 's302', (string) $service['section'], null);
                    foreach ($service['upstreams'] as $target) {
                        self::addUpstream($domainMap, $domain, $target);
                    }
                    if ($service['upstreams'] !== []) {
                        if (!isset($upstreams[$domain])) {
                            $upstreams[$domain] = ['services' => [], 'targets' => []];
                        }
                        if (!in_array($service['section'], $upstreams[$domain]['services'], true)) {
                            $upstreams[$domain]['services'][] = $service['section'];
                        }
                        foreach ($service['upstreams'] as $target) {
                            if (!in_array($target, $upstreams[$domain]['targets'], true)) {
                                $upstreams[$domain]['targets'][] = $target;
                            }
                        }
                    }
                }
            }
        }

        // ---------------------------------------------------------- 手动地址修正
        // 位置是刻意的：放在两个来源都合并完之后、产物定稿之前。上游把
        // S302/MITM 取向的地址排在前面，`max_ips_per_domain` 的截断在上面
        // 已经发生过，所以只有在这里覆盖才能赢过截断（原因见 config.php）。
        //
        // 「替换」而不是「追加」：追加会让 hosts.txt 里同一个域名同时出现
        // 坏地址和好地址，内核按顺序拨号，坏地址仍然排在前面 —— 症状只是
        // 从「全部超时」变成「慢三倍」，等于没修。
        if ($corrections !== []) {
            foreach ($corrections as $domain => $ips) {
                // ① 先摘掉上游为这个域名产生的所有 hosts 行。要遍历全部分组：
                //    同一个域名可能出现在多个分组里（例如 Github 与 Microsoft），
                //    只清当前分组会漏掉其它分组里的同名行。
                foreach ($hostsBlocks as $section => $rows) {
                    $kept = array_values(array_filter(
                        $rows,
                        static fn(array $row): bool => $row['domain'] !== $domain
                    ));
                    if ($kept === []) {
                        unset($hostsBlocks[$section]);
                        continue;
                    }
                    $hostsBlocks[$section] = $kept;
                }

                // ② 域名表：确保存在（上游没有这个域名时也要产出），并整体替换地址。
                self::addDomain($domainMap, $domain, 'correction', self::CORRECTION_SECTION, null);
                $domainMap[$domain]['ips'] = array_fill_keys($ips, true);

                // ③ hosts 行：用全部修正地址，且**不受** max_ips_per_domain 限制 ——
                //    修正表本身就是人工挑过的最小可用集，再截断就违背了它的目的。
                //    这里不再查 `$seenPairs`：上游的同名行已在 ① 删除，而修正表
                //    已去重，直接追加既正确又省一次全表扫描。
                if (!isset($hostsBlocks[self::CORRECTION_SECTION])) {
                    $hostsBlocks[self::CORRECTION_SECTION] = [];
                }
                foreach ($ips as $ip) {
                    $hostsBlocks[self::CORRECTION_SECTION][] = [
                        'ip'     => $ip,
                        'domain' => $domain,
                        'entry'  => 'correction',
                    ];
                }
            }

            $this->log->info('已应用人工地址修正', [
                '域名数' => count($corrections),
                '域名'   => array_keys($corrections),
            ]);
        }

        // ---------------------------------------------------------- 输出结构
        ksort($domainMap);

        // 域名总量上限（limits.max_domains）。超出按字典序保留前 N 个，
        // 并同步裁剪 hosts 块 —— 否则 hosts.txt 里会出现域名表里没有的条目。
        $maxDomains = (int) ($this->cfg['limits']['max_domains'] ?? 0);
        if ($maxDomains > 0 && count($domainMap) > $maxDomains) {
            $keep = array_slice(array_keys($domainMap), 0, $maxDomains);
            $keepSet = array_fill_keys($keep, true);
            $domainMap = array_intersect_key($domainMap, $keepSet);
            foreach ($hostsBlocks as $section => $rows) {
                $filtered = array_values(array_filter(
                    $rows,
                    static fn(array $row): bool => isset($keepSet[$row['domain']])
                ));
                if ($filtered === []) {
                    unset($hostsBlocks[$section]);
                    continue;
                }
                $hostsBlocks[$section] = $filtered;
            }
            $warnings[] = '聚合域名数超过上限 ' . $maxDomains . '，已按字典序截断（见 limits.max_domains）';
        }

        $domainRows = [];
        $mapCap     = (int) ($this->cfg['output']['domain_map_max_ips'] ?? 0);
        foreach ($domainMap as $domain => $info) {
            $ips = array_keys($info['ips']);
            // 修正过的域名原样保留：不做 IPv4 重排、也不截断。顺序和内容都是
            // 人工定过的（见 config.php），任何自动改动都可能把可用地址再次
            // 挤掉 —— 而这正是这一层要修的那个 bug。
            if (!isset($corrections[$domain])) {
                $ips = self::preferIpv4($ips, !empty($this->cfg['output']['prefer_ipv4']));
                if ($mapCap > 0 && count($ips) > $mapCap) {
                    $ips = array_slice($ips, 0, $mapCap);
                }
            }
            $domainRows[$domain] = [
                'ips'       => $ips,
                'sources'   => array_keys($info['sources']),
                'sections'  => array_keys($info['sections']),
                'upstreams' => array_keys($info['upstreams']),
            ];
        }

        ksort($upstreams);
        ksort($wildcards);

        $stats = [
            'usbeam' => [
                'groups'    => count($hostsBlocks),
                'entries'   => $usbeam['stats']['entries'] ?? 0,
                'domains'   => $usbeam['stats']['domains'] ?? 0,
                'addresses' => $usbeam['stats']['addresses'] ?? 0,
                'hosts_lines' => array_sum(array_map('count', $hostsBlocks)),
                'truncated_entries' => $truncated,
                'max_ips_per_domain' => (int) ($this->cfg['output']['max_ips_per_domain'] ?? 0),
            ],
            's302' => [
                'services'  => $s302['stats']['services'] ?? 0,
                'domains'   => count($s302Domains),
                'upstreams' => $s302['stats']['upstreams'] ?? 0,
                'wildcards' => count($wildcards),
            ],
            'merged' => [
                'domains'         => count($domainRows),
                'domains_with_ip' => count(array_filter($domainRows, static fn(array $row): bool => $row['ips'] !== [])),
                'domains_with_upstream' => count(array_filter($domainRows, static fn(array $row): bool => $row['upstreams'] !== [])),
                'only_usbeam'     => count(array_filter($domainRows, static fn(array $row): bool => self::sourcesWithoutCorrection($row['sources']) === ['usbeam'])),
                'only_s302'       => count(array_filter($domainRows, static fn(array $row): bool => self::sourcesWithoutCorrection($row['sources']) === ['s302'])),
                'both'            => count(array_filter($domainRows, static fn(array $row): bool => count(self::sourcesWithoutCorrection($row['sources'])) > 1)),
                // 有多少个域名被 config.php 的 corrections 覆盖过。放在统计里
                // 是为了让「修正生效了没有」在 status / 监控里一眼可见，而不必
                // 去 diff hosts.txt。
                'corrected_domains' => count($corrections),
            ],
            'placeholders' => $placeholderStats,
        ];

        if ($placeholderStats['unresolved'] > 0) {
            $warnings[] = '有 ' . $placeholderStats['unresolved'] . ' 个占位符条目未能解析出地址：'
                . implode(', ', array_keys($placeholderStats['unresolved_names']));
        }

        return [
            'domains'      => $domainRows,
            'hosts_blocks' => $hostsBlocks,
            's302_domains' => array_keys($s302Domains),
            'wildcards'    => array_keys($wildcards),
            'upstreams'    => $upstreams,
            'stats'        => $stats,
            'warnings'     => $warnings,
        ];
    }

    /**
     * 渲染全部文本产物。
     *
     * @return array<string,string> 文件名 => 内容
     */
    public function render(array $agg, ?array $usbeam, ?array $s302, array $meta = []): array
    {
        $now      = date('Y-m-d H:i:s P');
        $files    = [];

        // ---------------------------------------------------------- hosts.txt
        $lines = [];
        $lines[] = '# ' . str_repeat('=', 76);
        $lines[] = '# 由 rules-puller 自动生成，请勿手工编辑（下次拉取会覆盖）';
        $lines[] = '# 生成时间 : ' . $now;
        $lines[] = '# 数据来源 : UsbEAm Hosts Editor（作者 羽翼城 / Dogfight360）';
        if ($usbeam !== null) {
            $lines[] = '#            ' . ($usbeam['meta']['version'] !== null ? 'VERSION=' . $usbeam['meta']['version'] . '  ' : '')
                . ($usbeam['meta']['update_time'] !== null ? 'Latest_Update_Time=' . $usbeam['meta']['update_time'] : '');
        }
        $lines[] = '# 统计     : ' . ($agg['stats']['usbeam']['groups'] ?? 0) . ' 分组 / '
            . ($agg['stats']['usbeam']['entries'] ?? 0) . ' 条目 / '
            . ($agg['stats']['usbeam']['hosts_lines'] ?? 0) . ' 条 hosts 记录 / '
            . ($agg['stats']['usbeam']['addresses'] ?? 0) . ' 个地址';
        $lines[] = '# ' . str_repeat('=', 76);
        $lines[] = '';

        if (!empty($this->cfg['output']['hosts_comments'])) {
            foreach ($agg['hosts_blocks'] as $section => $rows) {
                $lines[] = '';
                $lines[] = '# === [' . $section . '] ' . count($rows) . ' 条 ===';
                foreach ($rows as $row) {
                    $lines[] = $row['ip'] . "\t" . $row['domain'];
                }
            }
        } else {
            foreach ($agg['hosts_blocks'] as $rows) {
                foreach ($rows as $row) {
                    $lines[] = $row['ip'] . "\t" . $row['domain'];
                }
            }
        }
        $files['hosts.txt'] = implode("\n", $lines) . "\n";

        // ------------------------------------------------------ hosts_s302.txt
        $listenIp = (string) ($this->cfg['s302']['listen_ip'] ?? '127.0.0.1');
        $tag      = (string) ($this->cfg['s302']['hosts_tag'] ?? '#S302');

        $s302Lines = [];
        $s302Lines[] = '# ' . str_repeat('=', 76);
        $s302Lines[] = '# Steamcommunity 302 劫持块：把这些域名指向本地反代监听地址';
        $s302Lines[] = '# 生成时间 : ' . $now;
        $s302Lines[] = '# 监听地址 : ' . $listenIp . '（对应 S302.ini 的 listen_ip，可改）';
        $s302Lines[] = '# 域名数   : ' . count($agg['s302_domains']);
        $s302Lines[] = '# ' . str_repeat('=', 76);
        $s302Lines[] = '';
        foreach ($agg['s302_domains'] as $domain) {
            $s302Lines[] = $listenIp . "\t" . $domain . "\t" . $tag;
        }
        $files['hosts_s302.txt'] = implode("\n", $s302Lines) . "\n";

        // --------------------------------------------------------- domains.txt
        $domains = array_keys($agg['domains']);
        if (!empty($this->cfg['output']['sort_domains'])) {
            sort($domains, SORT_STRING);
        }
        $files['domains.txt'] = '# ' . count($domains) . ' 个域名，' . $now . "\n"
            . implode("\n", $domains) . "\n";

        // ------------------------------------------------------- wildcards.txt
        $files['wildcards.txt'] = '# ' . count($agg['wildcards']) . ' 条通配符规则，' . $now . "\n"
            . implode("\n", $agg['wildcards']) . "\n";

        // ------------------------------------------------------ upstreams.json
        $upstreamOut = [
            'generated_at' => $now,
            'count'        => count($agg['upstreams']),
            'note'         => '域名 → S302 反代上游（替代源站）。TLS 在 S302 本地终结，故可改 Host/SNI。',
            'map'          => $agg['upstreams'],
        ];
        $files['upstreams.json'] = json_encode($upstreamOut, JSON_PRETTY_PRINT | JSON_UNESCAPED_UNICODE | JSON_UNESCAPED_SLASHES) . "\n";

        // ------------------------------------------------- s302_caddyfile.txt
        if ($s302 !== null) {
            $caddy = [];
            $caddy[] = '# ' . str_repeat('=', 76);
            $caddy[] = '# Steamcommunity 302 规则里解码出来的 Caddyfile 片段';
            $caddy[] = '# 生成时间 : ' . $now;
            $caddy[] = '# 规则版本 : ' . ($s302['rules']['last_update_text'] ?? '未知');
            $caddy[] = '# 说明     : 每段前的注释标出它属于哪个服务段；上游即 reverse_proxy 目标';
            $caddy[] = '# ' . str_repeat('=', 76);
            if (!empty($s302['caddy_global'])) {
                $caddy[] = '';
                $caddy[] = '# ---------------- [Caddy_init] ----------------';
                $caddy[] = $s302['caddy_global'];
            }
            foreach ($s302['services'] as $service) {
                if (empty($service['caddy'])) {
                    continue;
                }
                $caddy[] = '';
                $caddy[] = '# ---------------- [' . $service['section'] . '] ' . (string) $service['title'] . ' ----------------';
                $caddy[] = $service['caddy'];
            }
            if (!empty($s302['caddy_tail'])) {
                $caddy[] = '';
                $caddy[] = '# ---------------- [Caddy_end] ----------------';
                $caddy[] = $s302['caddy_tail'];
            }
            $files['s302_caddyfile.txt'] = implode("\n", $caddy) . "\n";
        }

        // ------------------------------------------------------- domains.json
        $domainOut = [
            'generated_at' => $now,
            'count'        => count($agg['domains']),
            'note'         => '域名 → {地址, 来源, 分组, S302 替代上游}。ips 按 output.domain_map_max_ips 截断。',
            'map'          => $agg['domains'],
        ];
        $files['domains.json'] = json_encode($domainOut, JSON_PRETTY_PRINT | JSON_UNESCAPED_UNICODE | JSON_UNESCAPED_SLASHES) . "\n";

        // ---------------------------------------------------------- rules.json
        $rules = [
            'generated_at' => $now,
            'sources'      => $meta['sources'] ?? [],
            'usbeam'       => $usbeam === null ? null : [
                'meta'    => [
                    'version'     => $usbeam['meta']['version'],
                    'update_time' => $usbeam['meta']['update_time'],
                ],
                'placeholders' => $usbeam['placeholders'],
                'groups'  => $usbeam['groups'],
                'sections'=> $usbeam['sections'],
                'stats'   => $usbeam['stats'],
                // 逐条目的地址明细在 domains.json 与 data/raw/ 里，这里只保留元信息
                'entries' => array_map(static function (array $entry): array {
                    return [
                        'id'           => $entry['id'],
                        'section'      => $entry['section'],
                        'name'         => $entry['name'],
                        'name_zh'      => $entry['name_zh'],
                        'desc_zh'      => $entry['desc_zh'],
                        'domains'      => $entry['domains'],
                        'ip_count'     => count($entry['ips']),
                        'placeholders' => $entry['placeholders'],
                        'dial_names'   => $entry['dial_names'],
                        'port'         => $entry['port'],
                        'dltest'       => $entry['dltest'],
                    ];
                }, $usbeam['entries']),
            ],
            's302'         => $s302 === null ? null : [
                'rules'    => [
                    'last_update'      => $s302['rules']['last_update'],
                    'last_update_text' => $s302['rules']['last_update_text'],
                    'enabled'          => $s302['rules']['enabled'],
                    'forwarding'       => $s302['rules']['forwarding'],
                    'cidr'             => $s302['rules']['cidr'],
                    'wildcards'        => $s302['rules']['domain_list'],
                ],
                'stats'    => $s302['stats'],
                'services' => array_map(static function (array $service): array {
                    return [
                        'section'    => $service['section'],
                        'title'      => $service['title'],
                        'group'      => $service['group'],
                        'domains'    => $service['domains'],
                        'upstreams'  => $service['upstreams'],
                        'directives' => $service['directives'],
                        'tips'       => $service['tips'],
                    ];
                }, $s302['services']),
            ],
            'stats'        => $agg['stats'],
            'warnings'     => array_merge($agg['warnings'], $meta['warnings'] ?? []),
        ];
        $files['rules.json'] = json_encode($rules, JSON_PRETTY_PRINT | JSON_UNESCAPED_UNICODE | JSON_UNESCAPED_SLASHES) . "\n";

        // ---------------------------------------------------------- stats.json
        $files['stats.json'] = json_encode([
            'generated_at' => $now,
            'stats'        => $agg['stats'],
            'warnings'     => $rules['warnings'],
        ], JSON_PRETTY_PRINT | JSON_UNESCAPED_UNICODE | JSON_UNESCAPED_SLASHES) . "\n";

        return $files;
    }

    // ------------------------------------------------------------ 内部工具

    /**
     * IPv4 排在前面。
     *
     * 同一个条目往往同时给出 IPv4 与 IPv6；hosts 条目有数量上限时，
     * 若 IPv6 排在前面会把 IPv4 全挤掉，而绝大多数工具只吃 IPv4。
     *
     * @param string[] $ips
     * @return string[]
     */
    /**
     * 去掉 `correction` 之后的来源列表。
     *
     * `correction` 是**标注**不是来源 —— 它说明「这个域名的地址被人改过」，
     * 而不是「地址来自第三个数据源」。统计 only_usbeam / only_s302 / both 时
     * 把它算进去，会让一个只来自 UsbEAm 的域名被算成「两个来源」，把监控读歪。
     *
     * @param string[] $sources
     * @return string[]
     */
    private static function sourcesWithoutCorrection(array $sources): array
    {
        return array_values(array_diff($sources, ['correction']));
    }

    private static function preferIpv4(array $ips, bool $enabled): array
    {
        if (!$enabled) {
            return $ips;
        }

        $v4 = [];
        $v6 = [];
        foreach ($ips as $ip) {
            if (strpos($ip, ':') === false) {
                $v4[] = $ip;
            } else {
                $v6[] = $ip;
            }
        }

        return array_merge($v4, $v6);
    }

    private static function addDomain(array &$map, string $domain, string $source, string $section, ?string $note): void
    {
        if (!isset($map[$domain])) {
            $map[$domain] = ['ips' => [], 'sources' => [], 'sections' => [], 'upstreams' => []];
        }
        if (!isset($map[$domain]['sources'][$source])) {
            $map[$domain]['sources'][$source] = true;
        }
        if (!isset($map[$domain]['sections'][$section])) {
            $map[$domain]['sections'][$section] = true;
        }
    }

    private static function addIp(array &$map, string $domain, string $ip): void
    {
        if (!isset($map[$domain])) {
            return;
        }
        $map[$domain]['ips'][$ip] = true;
    }

    private static function addUpstream(array &$map, string $domain, string $target): void
    {
        if (!isset($map[$domain])) {
            return;
        }
        $map[$domain]['upstreams'][$target] = true;
    }

    /**
     * 解析 `{Cloudflare}` 这类占位符。
     *
     * 优先用条目自身的域名做 DoH 解析 —— 拿到的是当下真实可用的边缘地址，
     * 比硬编码一个「优选 IP」更可靠；解析不到才退回静态兜底列表。
     *
     * @return array{0:string[],1:string} [地址列表, 'doh'|'static']
     */
    private function resolvePlaceholder(string $name, array $entry): array
    {
        if (empty($this->cfg['placeholders']['enabled'])) {
            return [[], 'static'];
        }

        $first = $entry['domains'][0] ?? null;
        if ($first !== null && !empty($this->resolveCache[$first])) {
            return [$this->resolveCache[$first], 'doh'];
        }

        $static = (array) ($this->cfg['placeholders']['static_ips'][$name] ?? []);

        return [array_values(array_filter($static, 'rp_is_ip')), 'static'];
    }

    /**
     * 把所有含占位符的条目的域名收集起来，一次性批量解析。
     *
     * 逐个解析时每个域名要等一个完整往返，几百个域名就是几分钟；
     * 批量并发后同样的工作量降到几十秒。
     */
    private function primeResolveCache(?array $usbeam, ?callable $resolver): void
    {
        if ($usbeam === null || $resolver === null) {
            return;
        }
        if (empty($this->cfg['placeholders']['enabled']) || empty($this->cfg['placeholders']['resolve_via_doh'])) {
            return;
        }

        $wanted = [];
        foreach ($usbeam['entries'] as $entry) {
            if ($entry['placeholders'] === [] || $entry['domains'] === []) {
                continue;
            }
            // 一个条目里的域名都指向同一张 CDN，解析第一个就够 —— 解析全部是白付查询。
            $wanted[$entry['domains'][0]] = true;
        }
        if ($wanted === []) {
            return;
        }

        $domains = array_keys($wanted);
        $limit   = (int) ($this->cfg['placeholders']['max_lookups'] ?? 0);
        $total   = count($domains);
        if ($limit > 0 && $total > $limit) {
            $domains = array_slice($domains, 0, $limit);
            $this->log->warn('占位符解析域名过多，已按上限截断', ['总数' => $total, '上限' => $limit]);
        }

        $started = microtime(true);
        $map     = $resolver($domains);
        if (!is_array($map)) {
            $map = [];
        }

        foreach ($map as $domain => $ips) {
            $clean = array_values(array_filter((array) $ips, 'rp_is_ip'));
            if ($clean !== []) {
                $this->resolveCache[(string) $domain] = $clean;
            }
        }

        $this->log->info('占位符域名批量解析完成', [
            '请求'   => count($domains),
            '成功'   => count($this->resolveCache),
            '耗时ms' => (int) round((microtime(true) - $started) * 1000),
        ]);
    }
}
