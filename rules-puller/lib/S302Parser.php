<?php

declare(strict_types=1);

namespace RulesPuller;

/**
 * 解析 S302_rules.ini。
 *
 * 规则文件里 `Json` / `Domain_list` / `Forwarding` 是双重混淆：
 *   明文 --重复异或--> 密文 --Base64--> INI 字段值
 * 密钥 22 字节，循环使用。
 *
 * 见《Steamcommunity 302 运行逻辑与转发机制》第 12 节。
 */
final class S302Parser
{
    /** 非服务段 */
    private const RESERVED_SECTIONS = ['Rules', 'ICON', 'Caddy_init', 'Caddy_end', ''];

    /**
     * 解密一个字段。
     */
    public static function deobfuscate(string $value, string $key): ?string
    {
        $value = preg_replace('/\s+/', '', trim($value));
        if (!is_string($value) || $value === '') {
            return null;
        }

        $raw = base64_decode($value, true);
        if ($raw === false) {
            // 少数情况下会写成 URL-safe Base64
            $raw = base64_decode(strtr($value, '-_', '+/'), true);
        }
        if ($raw === false || $raw === '') {
            return null;
        }

        $keyLength = strlen($key);
        if ($keyLength === 0) {
            return $raw;
        }

        $out = '';
        for ($i = 0, $n = strlen($raw); $i < $n; $i++) {
            $out .= $raw[$i] ^ $key[$i % $keyLength];
        }

        return $out;
    }

    /**
     * @return array{ok:bool,warnings:string[],rules:array,services:array,icons:string[],
     *               caddy_global:?string,caddy_tail:?string,stats:array}
     */
    public static function parse(string $text, array $cfg): array
    {
        $key      = (string) ($cfg['xor_key'] ?? '');
        $warnings = [];
        $ini      = Ini::parse($text);

        if (!Ini::hasSection($ini, 'Rules')) {
            $warnings[] = '未找到 [Rules] 段，规则文件可能不是预期格式';
        }

        // ------------------------------------------------------------ [Rules]
        $rulesRaw = Ini::section($ini, 'Rules');

        $rules = [
            'last_update'      => null,
            'last_update_text' => null,
            'enabled'          => [],
            'domain_list'      => [],
            'forwarding'       => [],
            'cidr'             => [],
            'keys'             => array_keys($rulesRaw),
        ];

        $timestamp = $rulesRaw['Last_update'] ?? null;
        if ($timestamp !== null && is_numeric(trim($timestamp))) {
            $rules['last_update']      = (int) trim($timestamp);
            $rules['last_update_text'] = date('Y-m-d H:i:s', (int) trim($timestamp));
        }

        if (!empty($rulesRaw['enabled'])) {
            $rules['enabled'] = self::splitList($rulesRaw['enabled']);
        }

        // 通配符域名表（混淆）
        if (!empty($rulesRaw['Domain_list'])) {
            $decoded = self::deobfuscate($rulesRaw['Domain_list'], $key);
            if ($decoded === null) {
                $warnings[] = 'Domain_list 解密失败（密钥或字段格式不符）';
            } else {
                foreach (self::splitList(preg_replace('/[\r\n]+/', ',', $decoded) ?? '') as $item) {
                    $item = strtolower($item);
                    if ($item !== '') {
                        $rules['domain_list'][] = $item;
                    }
                }
            }
        }

        // 走 HTTP 代理而不走 hosts 的域名（混淆）
        if (!empty($rulesRaw['Forwarding'])) {
            $decoded = self::deobfuscate($rulesRaw['Forwarding'], $key);
            if ($decoded === null) {
                $warnings[] = 'Forwarding 解密失败';
            } else {
                $rules['forwarding'] = array_map('strtolower', self::splitList(preg_replace('/[\r\n]+/', ',', $decoded) ?? ''));
            }
        }

        // 优选候选池 CIDR（明文，键名不固定，按包含 CIDR 的键收集）
        foreach ($rulesRaw as $ruleKey => $ruleValue) {
            if (stripos((string) $ruleKey, 'CIDR') === false) {
                continue;
            }
            $cidrs = self::splitList((string) $ruleValue);
            if ($cidrs !== []) {
                $rules['cidr'][(string) $ruleKey] = $cidrs;
            }
        }

        // ------------------------------------------------------------ 全局 Caddyfile
        $caddyGlobal = null;
        if (!empty($ini['sections']['Caddy_init']['Json'])) {
            $caddyGlobal = self::deobfuscate((string) $ini['sections']['Caddy_init']['Json'], $key);
        }
        $caddyTail = null;
        if (!empty($ini['sections']['Caddy_end']['Json'])) {
            $caddyTail = self::deobfuscate((string) $ini['sections']['Caddy_end']['Json'], $key);
        }

        // ------------------------------------------------------------ 服务段
        $services     = [];
        $decodeFail   = 0;

        foreach ($ini['order'] as $section) {
            if (in_array($section, self::RESERVED_SECTIONS, true)) {
                continue;
            }
            $raw = Ini::section($ini, $section);
            if ($raw === []) {
                continue;
            }

            $caddy = null;
            if (!empty($raw['Json'])) {
                $caddy = self::deobfuscate((string) $raw['Json'], $key);
                if ($caddy === null || !self::looksLikeCaddyfile($caddy)) {
                    $decodeFail++;
                    $warnings[] = $section . ' 的 Json 解密结果不像 Caddyfile，已忽略其上游信息';
                    $caddy = null;
                }
            }

            $upstreams = [];
            $siteHosts = [];
            $directives = [];
            if ($caddy !== null) {
                foreach (self::splitBlocks($caddy) as $block) {
                    foreach ($block['hosts'] as $host) {
                        $siteHosts[$host] = true;
                    }
                    foreach ($block['upstreams'] as $upstream) {
                        $upstreams[$upstream] = true;
                    }
                    foreach ($block['directives'] as $directive) {
                        $directives[$directive] = true;
                    }
                }
            }

            // Domain 字段是明文域名列表，是 hosts 条目的主来源
            $domains    = [];
            $wildcards  = [];
            foreach (self::splitList((string) ($raw['Domain'] ?? '')) as $token) {
                $token = strtolower(trim($token));
                if ($token === '') {
                    continue;
                }
                if (strpos($token, '*') !== false) {
                    $wildcards[] = $token;
                    continue;
                }
                $domain = rp_normalize_domain($token);
                if ($domain !== null) {
                    $domains[$domain] = true;
                }
            }
            foreach (array_keys($siteHosts) as $host) {
                $domains[$host] = true;
            }

            $domainList = array_keys($domains);

            $services[] = [
                'section'    => $section,
                'title'      => $raw['Title'] ?? $section,
                'group'      => $raw['Group'] ?? null,
                'icon'       => $raw['icon'] ?? null,
                'tips'       => $raw['Tips'] ?? null,
                'domains'    => $domainList,
                'wildcards'  => array_values(array_unique($wildcards)),
                'upstreams'  => array_keys($upstreams),
                'site_hosts' => array_keys($siteHosts),
                'directives' => array_keys($directives),
                'caddy'      => $caddy,
            ];
        }

        // ------------------------------------------------------------ 图标段
        $icons = array_keys(Ini::section($ini, 'ICON'));

        // ------------------------------------------------------------ 统计
        $allDomains = [];
        $allUpstreams = [];
        foreach ($services as $service) {
            foreach ($service['domains'] as $domain) {
                $allDomains[$domain] = true;
            }
            foreach ($service['upstreams'] as $upstream) {
                $allUpstreams[$upstream] = true;
            }
        }

        $ok = $services !== [] && $rules['last_update'] !== null;

        return [
            'ok'           => $ok,
            'warnings'     => $warnings,
            'rules'        => $rules,
            'services'     => $services,
            'icons'        => $icons,
            'caddy_global' => $caddyGlobal,
            'caddy_tail'   => $caddyTail,
            'stats'        => [
                'services'        => count($services),
                'domains'         => count($allDomains),
                'upstreams'       => count($allUpstreams),
                'wildcards'       => count($rules['domain_list']),
                'forwarding'      => count($rules['forwarding']),
                'cidr_groups'     => count($rules['cidr']),
                'icons'           => count($icons),
                'decode_failures' => $decodeFail,
            ],
        ];
    }

    /**
     * 解密结果的合理性检查：解错密钥会得到乱码。
     */
    private static function looksLikeCaddyfile(string $text): bool
    {
        if ($text === '' || strpos($text, '{') === false) {
            return false;
        }
        if (function_exists('mb_check_encoding') && !mb_check_encoding($text, 'UTF-8')) {
            return false;
        }
        // 控制字符比例过高说明是乱码
        $control = preg_match_all('/[\x00-\x08\x0B\x0C\x0E-\x1F]/', $text);

        return $control !== false && $control < max(4, (int) (strlen($text) / 100));
    }

    /**
     * 按大括号切分 Caddyfile 的站点块，返回每块的头部与主体。
     *
     * `{port}` 这类占位符里的花括号必须跳过，否则块结构会被误判。
     *
     * @return array<int,array{header:string,body:string}>
     */
    private static function splitBlocks(string $text): array
    {
        $blocks = [];
        $length = strlen($text);
        $i      = 0;
        $header = '';

        while ($i < $length) {
            $char = $text[$i];

            if ($char === '#') {
                $newline = strpos($text, "\n", $i);
                $i = $newline === false ? $length : $newline + 1;
                continue;
            }

            if ($char === '{') {
                $close = strpos($text, '}', $i);
                if ($close !== false && $close - $i <= 24) {
                    $inner = substr($text, $i + 1, $close - $i - 1);
                    if (preg_match('/^[A-Za-z0-9_\-]+$/', $inner) === 1) {
                        // 占位符，如 {port} / {bind_ip}
                        $header .= substr($text, $i, $close - $i + 1);
                        $i = $close + 1;
                        continue;
                    }
                }

                $depth     = 1;
                $j         = $i + 1;
                $bodyStart = $j;
                while ($j < $length) {
                    if ($text[$j] === '{') {
                        $depth++;
                    } elseif ($text[$j] === '}') {
                        $depth--;
                        if ($depth === 0) {
                            break;
                        }
                    }
                    $j++;
                }

                $blocks[] = self::parseBlock($header, substr($text, $bodyStart, max(0, $j - $bodyStart)));
                $header = '';
                $i      = $j + 1;
                continue;
            }

            $header .= $char;
            $i++;
        }

        return $blocks;
    }

    /**
     * 从一个站点块里抽出席位主机名、反代上游、指令名。
     */
    private static function parseBlock(string $header, string $body): array
    {
        $hosts = [];

        if (preg_match_all('#https?://([a-z0-9][a-z0-9.\-]*)#i', $header, $m)) {
            foreach ($m[1] as $host) {
                $hosts[strtolower($host)] = true;
            }
        }
        foreach (preg_split('/\s+/', $header) ?: [] as $token) {
            $token = trim($token);
            if ($token === '') {
                continue;
            }
            if (preg_match('/^([a-z0-9][a-z0-9.\-]*\.[a-z]{2,})(?::[0-9{}A-Za-z_]+)?$/i', $token, $m)) {
                $hosts[strtolower($m[1])] = true;
            }
        }

        $upstreams = [];
        if (preg_match_all('/reverse_proxy\s+((?:[^\n]*\\\\\s*\n)*[^\n}]*)/i', $body, $m)) {
            foreach ($m[1] as $chunk) {
                $chunk = str_replace('\\', ' ', $chunk);
                foreach (preg_split('/\s+/', $chunk) ?: [] as $token) {
                    $token = trim($token);
                    if ($token === '' || $token[0] === '{' || $token === '}') {
                        continue;
                    }
                    $token = (string) preg_replace('#^https?://#i', '', $token);
                    $token = (string) preg_replace('#:\d+$#', '', $token);
                    $token = strtolower(rtrim($token, '/'));
                    if (preg_match('/^[a-z0-9][a-z0-9.\-]*\.[a-z]{2,}$/', $token) === 1) {
                        $upstreams[$token] = true;
                    }
                }
            }
        }

        $directives = [];
        if (preg_match_all('/^[ \t]*([a-z_][a-z0-9_]*)\b/m', $body, $m)) {
            foreach ($m[1] as $directive) {
                $directives[strtolower($directive)] = true;
            }
        }

        return [
            'hosts'      => array_keys($hosts),
            'upstreams'  => array_keys($upstreams),
            'directives' => array_keys($directives),
        ];
    }

    /**
     * 逗号（或换行）分隔列表。
     *
     * @return string[]
     */
    private static function splitList(string $value): array
    {
        if (trim($value) === '') {
            return [];
        }
        $out = [];
        foreach (preg_split('/[,\r\n]+/', $value) ?: [] as $piece) {
            $piece = trim($piece);
            if ($piece !== '') {
                $out[] = $piece;
            }
        }

        return $out;
    }
}
