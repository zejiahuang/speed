<?php

declare(strict_types=1);

namespace RulesPuller;

/**
 * 解析 usbeam_new_40.xml（实为 INI 文本）。
 *
 * 结构：
 *   [usbeam]      版本、更新时间、分组声明
 *   [Public]      **占位符定义表** —— `{Cloudflare}` 这类占位符的地址/测速 URL 就写在这里
 *   [usbeam_ad_*] 广告位配置，不是规则，必须排除
 *   [分组名]      条目由 `NNNNN.field=value` 组成，NNNNN 为条目 ID
 *
 * 见《UsbEAm Hosts 代理 APK 开发说明》2.1 ~ 2.4 节。
 */
final class UsbeamParser
{
    /** 非服务段的保留段名 */
    private const RESERVED_SECTIONS = ['usbeam', 'Announcement', 'language', 'DNS', 'Public', ''];

    /** 广告位段名前缀 */
    private const AD_SECTION_PREFIX = 'usbeam_ad';

    /**
     * @param int $maxEntries 条目数上限，0 = 不限
     * @return array{meta:array,announcement:array,language:array,dns:array,placeholders:array,
     *               groups:string[],sections:array,entries:array,warnings:string[],stats:array}
     */
    public static function parse(string $text, int $maxEntries = 0): array
    {
        $ini      = Ini::parse($text);
        $warnings = [];
        $truncated = false;

        $meta = [
            'version'     => Ini::get($ini, 'usbeam', 'VERSION'),
            'update_time' => Ini::get($ini, 'usbeam', 'Latest_Update_Time'),
            'group_raw'   => Ini::get($ini, 'usbeam', 'Group'),
        ];

        $groups = self::parseGroups((string) ($meta['group_raw'] ?? ''));

        // ------------------------------------------------ [Public] 占位符定义表
        $placeholderTable = self::parsePlaceholderTable(Ini::section($ini, 'Public'));

        // ------------------------------------------------ 服务分组
        $sections = [];
        $entries  = [];
        $index    = 0;
        $skipped  = [];

        foreach ($ini['order'] as $section) {
            if (self::isReserved($section)) {
                if (strncmp($section, self::AD_SECTION_PREFIX, strlen(self::AD_SECTION_PREFIX)) === 0) {
                    $skipped[] = $section;
                }
                continue;
            }
            if ($maxEntries > 0 && count($entries) >= $maxEntries) {
                $truncated = true;
                break;
            }

            $byId = self::collectEntries(Ini::section($ini, $section));
            if ($byId === []) {
                continue;
            }

            $ids = [];
            foreach ($byId as $id => $fields) {
                if ($maxEntries > 0 && count($entries) >= $maxEntries) {
                    $truncated = true;
                    break 2;
                }
                $entries[] = self::buildEntry((string) $id, $section, $fields, $index, $placeholderTable, $warnings);
                $ids[]     = (string) $id;
                $index++;
            }

            $sections[$section] = ['count' => count($ids), 'entry_ids' => $ids];
        }

        if ($truncated) {
            $warnings[] = '条目数达到上限 ' . $maxEntries . '，其余条目已丢弃（见 limits.max_entries）';
        }

        // 分组顺序：优先用 [usbeam] Group 声明的顺序，未声明的按出现顺序补在后面
        $ordered = [];
        foreach ($groups as $group) {
            if (isset($sections[$group]) && !in_array($group, $ordered, true)) {
                $ordered[] = $group;
            }
        }
        foreach (array_keys($sections) as $name) {
            if (!in_array($name, $ordered, true)) {
                $ordered[] = $name;
            }
        }

        $undeclared = array_values(array_diff(array_keys($sections), $groups));
        if ($undeclared !== []) {
            $warnings[] = '有 ' . count($undeclared) . ' 个分组未在 Group 里声明，已追加到末尾：' . implode(', ', $undeclared);
        }

        // ------------------------------------------------ 统计
        $domainSet = [];
        $ipSet     = [];
        $phSet     = [];
        foreach ($entries as $entry) {
            foreach ($entry['domains'] as $domain) {
                $domainSet[$domain] = true;
            }
            foreach ($entry['ips'] as $ip) {
                $ipSet[$ip] = true;
            }
            foreach ($entry['placeholders'] as $name) {
                $phSet[$name] = true;
            }
        }

        return [
            'meta'         => $meta,
            'announcement' => Ini::section($ini, 'Announcement'),
            'language'     => Ini::section($ini, 'language'),
            'dns'          => Ini::section($ini, 'DNS'),
            'placeholders' => $placeholderTable,
            'groups'       => $ordered,
            'sections'     => $sections,
            'entries'      => $entries,
            'warnings'     => $warnings,
            'stats'        => [
                'groups'            => count($sections),
                'entries'           => count($entries),
                'domains'           => count($domainSet),
                'addresses'         => count($ipSet),
                'placeholders'      => count($phSet),
                'placeholder_names' => array_keys($phSet),
                'placeholder_defined' => array_keys($placeholderTable),
                'ad_sections_skipped' => $skipped,
            ],
        ];
    }

    /**
     * [Public] 段 → 占位符定义表。
     *
     * 值可能是 IP 列表（`{Cloudflare}`），也可能是测速 URL（`{Cloudflare_DL}`）。
     *
     * @return array<string,array{ips:string[],url:?string,raw:string}>
     */
    private static function parsePlaceholderTable(array $public): array
    {
        $table = [];
        foreach ($public as $name => $value) {
            $name  = (string) $name;
            $value = (string) $value;
            if ($name === '') {
                continue;
            }

            $ips = [];
            $url = null;
            foreach (self::splitList($value) as $token) {
                if (rp_is_ip($token)) {
                    $ips[] = $token;
                } elseif (preg_match('#^https?://#i', $token) === 1) {
                    $url = $token;
                }
            }

            $table[$name] = [
                'ips' => array_values(array_unique($ips)),
                'url' => $url,
                'raw' => $value,
            ];
        }

        return $table;
    }

    private static function isReserved(string $section): bool
    {
        if (in_array($section, self::RESERVED_SECTIONS, true)) {
            return true;
        }

        return strncmp($section, self::AD_SECTION_PREFIX, strlen(self::AD_SECTION_PREFIX)) === 0;
    }

    /**
     * 段内 `NNNNN.field=value` → [id => [field => value]]，保留文档顺序。
     */
    private static function collectEntries(array $raw): array
    {
        $byId = [];
        foreach ($raw as $key => $value) {
            if (preg_match('/^(\d+)[._]([A-Za-z_][A-Za-z0-9_]*)$/', (string) $key, $m) !== 1) {
                continue;
            }
            $byId[$m[1]][$m[2]] = (string) $value;
        }

        return $byId;
    }

    /**
     * Group=CDN for open-source,Academic,...,|,Steam,... —— 逗号分隔，`|` 是排版分隔符。
     *
     * @return string[]
     */
    private static function parseGroups(string $raw): array
    {
        if (trim($raw) === '') {
            return [];
        }
        $groups = [];
        foreach (explode(',', $raw) as $piece) {
            $piece = trim($piece);
            if ($piece === '' || $piece === '|') {
                continue;
            }
            $groups[] = $piece;
        }

        return $groups;
    }

    private static function buildEntry(
        string $id,
        string $section,
        array $fields,
        int $index,
        array $placeholderTable,
        array &$warnings
    ): array {
        $ips             = [];
        $placeholders    = [];
        $resolvedNames   = [];
        $dialNames       = [];

        foreach (self::splitList($fields['ip'] ?? '') as $token) {
            $placeholderName = rp_placeholder_name($token);

            if ($placeholderName !== null) {
                $definition = $placeholderTable[$placeholderName] ?? null;
                if ($definition !== null && $definition['ips'] !== []) {
                    // [Public] 里定义了地址 —— 直接展开，不需要联网
                    $ips           = array_merge($ips, $definition['ips']);
                    $resolvedNames[] = $placeholderName;
                } else {
                    // 未定义，留给聚合阶段用 DoH 兜底解析
                    $placeholders[] = $placeholderName;
                }
                continue;
            }

            if (rp_is_ip($token)) {
                $ips[] = $token;
                continue;
            }

            // 地址位上出现主机名：记录但不进 hosts（需要解析，见拨号名机制）
            $host = rp_normalize_domain($token);
            if ($host !== null) {
                $dialNames[] = $host;
            }
        }

        $domains = [];
        foreach (self::splitList($fields['domain'] ?? '') as $token) {
            $domain = rp_normalize_domain($token);
            if ($domain !== null) {
                $domains[] = $domain;
            }
        }

        if ($domains === [] && $ips === [] && $placeholders === []) {
            $warnings[] = '条目 ' . $id . '（段 ' . $section . '）既无域名也无地址，已跳过';
        }

        $cert = [];
        foreach (self::splitList($fields['cert'] ?? '') as $token) {
            $token = strtolower(trim($token));
            if ($token !== '') {
                $cert[] = $token;
            }
        }

        $port = null;
        if (isset($fields['port']) && $fields['port'] !== '' && ctype_digit(trim($fields['port']))) {
            $port = (int) trim($fields['port']);
        }

        return [
            'id'           => $id,
            'section'      => $section,
            'index'        => $index,
            'name'         => $fields['name'] ?? null,
            'name_zh'      => $fields['name_zh'] ?? null,
            'desc'         => $fields['desc'] ?? null,
            'desc_zh'      => $fields['desc_zh'] ?? null,
            'domains'      => array_values(array_unique($domains)),
            'ips'          => array_values(array_unique($ips)),
            'placeholders' => array_values(array_unique($placeholders)),
            'resolved_placeholders' => array_values(array_unique($resolvedNames)),
            'dial_names'   => array_values(array_unique($dialNames)),
            'cert'         => array_values(array_unique($cert)),
            'port'         => $port,
            'guide'        => $fields['guide'] ?? null,
            'dltest'       => self::expandPlaceholderUrl($fields['dltest'] ?? null, $placeholderTable),
            'fields'       => $fields,
        ];
    }

    /**
     * `dltest` 里的 `{Cloudflare_DL}` 展开成真实测速 URL。
     */
    private static function expandPlaceholderUrl(?string $value, array $placeholderTable): ?string
    {
        if ($value === null || $value === '') {
            return $value;
        }
        $trimmed = trim($value);
        if (preg_match('/^\{([A-Za-z0-9_\-]+)\}$/', $trimmed, $m) !== 1) {
            return $trimmed;
        }
        $definition = $placeholderTable[$m[1]] ?? null;
        if ($definition !== null && is_string($definition['url']) && $definition['url'] !== '') {
            return $definition['url'];
        }

        return $trimmed;
    }

    /**
     * 逗号分隔列表 → 去空白、去空项。
     *
     * @return string[]
     */
    private static function splitList(string $value): array
    {
        if (trim($value) === '') {
            return [];
        }
        $out = [];
        foreach (explode(',', $value) as $piece) {
            $piece = trim($piece);
            if ($piece !== '') {
                $out[] = $piece;
            }
        }

        return $out;
    }
}
