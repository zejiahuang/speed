<?php

declare(strict_types=1);

/**
 * 中转站（公开只读规则发布）。
 *
 * 把拉取产物以**固定短网址**对外发布，供别人订阅：
 *   https://你的域名/1  → UsbEAm Hosts 规则（hosts 文本）
 *   https://你的域名/2  → Steamcommunity 302 规则（hosts 文本）
 *
 * 两种访问方式（用哪个取决于你的主机能不能配重写）：
 *   A. 短路径       /1            需要 .htaccess（Apache）或面板伪静态（Nginx）
 *   B. 带参数       r.php?p=1     任何环境都能用，不需要重写
 *
 * 内容协商：
 *   默认            hosts 文本（可直接粘进 hosts 文件）
 *   ?format=json    结构化 JSON（含 IP、域名、来源等字段）
 *
 * 这是个**公开**端点 —— 刻意不做登录，也不受 IP 频率闸门限制：
 * 规则本身就是公开数据，而且下游客户端会定时来拉。防滥用靠
 * 「产物是静态文件 + 下游缓存」，而不是靠限流。
 */

use RulesPuller\Store;

require __DIR__ . '/bootstrap.php';

$cfg = rp_config();

// ---------------------------------------------------------------- 总开关
// 关掉时按「这个地址不存在」处理 —— 返回 404 而不是 403，
// 免得让外部知道这里藏着一个功能。
$share = (array) ($cfg['share'] ?? []);
if (empty($share['enabled'])) {
    rp_share_404();
}

date_default_timezone_set((string) ($cfg['timezone'] ?? 'UTC'));

// ---------------------------------------------------------------- 定位中转点
// 路径优先取重写传来的 `p`；没有就自己从 REQUEST_URI 尾部反推
// （覆盖「没法写重写、只能直接访问 r.php」的降级场景之外的情况）。
$key = rp_share_resolve_key($share);

if ($key === null) {
    rp_share_404();
}

$points = [
    'usbeam' => [
        'path'  => (string) ($share['usbeam_path'] ?? '1'),
        'label' => 'UsbEAm Hosts 规则',
        'file'  => 'hosts.txt',
        'kind'  => 'usbeam',
    ],
    's302' => [
        'path'  => (string) ($share['s302_path'] ?? '2'),
        'label' => 'Steamcommunity 302 规则',
        'file'  => 'hosts_s302.txt',
        'kind'  => 's302',
    ],
];

$point = $points[$key];
$outDir = rtrim((string) ($cfg['paths']['out'] ?? (__DIR__ . '/data/out')), '/');
$file   = $outDir . '/' . $point['file'];

$raw = Store::read($file);
if ($raw === null || trim($raw) === '') {
    // 还没跑过拉取 —— 说清楚而不是给个空白
    rp_share_error(503, '规则还没生成',
        '中转站已开启，但还没有任何产物。到后台点一次「立即拉取」，或等定时任务跑一轮。');
}

// ---------------------------------------------------------------- 内容协商
// ?format=json 走 JSON；其余（含不传）一律 hosts 文本。
$format = strtolower(trim((string) ($_GET['format'] ?? '')));
if ($format === '') {
    $format = (string) ($share['default_format'] ?? 'hosts');
}
$allowJson = !empty($share['allow_json']);

if (!in_array($format, ['hosts', 'json'], true)) {
    rp_share_error(400, '不支持的格式',
        '只支持 hosts（默认）与 json。例如 ?format=json');
}
if ($format === 'json' && !$allowJson) {
    rp_share_error(403, 'JSON 输出已关闭',
        '管理员关闭了 JSON 输出，只能获取 hosts 文本。');
}

// ---------------------------------------------------------------- 组装响应
$manifest = Store::readJson($outDir . '/manifest.json') ?? [];
$stats    = Store::readJson($outDir . '/stats.json') ?? [];
$meta     = rp_share_metadata($manifest, $stats, $point, $share);

if ($format === 'hosts') {
    $body = rp_share_hosts_body($raw, $meta, !empty($share['send_metadata']));
    $type = 'text/plain; charset=utf-8';
} else {
    $body = rp_share_json_body($raw, $meta, $point);
    $type = 'application/json; charset=utf-8';
}

// ---------------------------------------------------------------- 响应头
// ETag 基于内容算 —— 产物没变时下游拿到 304，省流量也省你的带宽。
$etag = '"' . substr(hash('sha256', $body), 0, 32) . '"';
$maxAge = max(0, (int) ($share['max_age'] ?? 3600));

if (!headers_sent()) {
    header('Content-Type: ' . $type);
    header('ETag: ' . $etag);
    header('Cache-Control: public, max-age=' . $maxAge);
    header('X-Robots-Tag: noindex');   // 别让搜索引擎把规则页收进去

    // 让下游知道产物是什么时候生成的，便于判断新鲜度
    $generated = (string) ($manifest['generated_at'] ?? '');
    if ($generated !== '') {
        header('X-Rules-Generated-At: ' . $generated);
    }
    header('X-Rules-Source: ' . $point['kind']);

    if (!empty($share['cors'])) {
        header('Access-Control-Allow-Origin: *');
        header('Access-Control-Allow-Methods: GET, HEAD');
        header('Access-Control-Expose-Headers: ETag, X-Rules-Generated-At');
    }

    // 条件请求：下游带了匹配的 If-None-Match 就直接 304
    $inm = trim((string) ($_SERVER['HTTP_IF_NONE_MATCH'] ?? ''));
    if ($inm !== '' && $inm === $etag) {
        http_response_code(304);
        exit;
    }

    // HEAD 请求：只要头，不要体
    if (strtoupper((string) ($_SERVER['REQUEST_METHOD'] ?? 'GET')) === 'HEAD') {
        header('Content-Length: ' . strlen($body));
        exit;
    }
}

echo $body;

// ==================================================================== 辅助

/**
 * 判断这次请求要哪个中转点。
 *
 * 依次尝试：?p= 参数 → 从 REQUEST_URI 尾部匹配配置的路径 → 从 r.php 后的路径段匹配。
 * 全部落空返回 null（调用方给 404）。
 */
function rp_share_resolve_key(array $share): ?string
{
    $map = [
        'usbeam' => (string) ($share['usbeam_path'] ?? '1'),
        's302'   => (string) ($share['s302_path'] ?? '2'),
    ];

    // ① 显式参数：r.php?p=usbeam / ?p=1 —— 任何环境都能用
    $p = trim((string) ($_GET['p'] ?? ''), " \t\n\r\0\x0B/");
    if ($p !== '') {
        // 允许用语义名，也允许用配置的路径
        if (isset($map[$p])) {
            return $p;
        }
        foreach ($map as $key => $path) {
            if ($path !== '' && strcasecmp($p, $path) === 0) {
                return $key;
            }
        }

        return null;
    }

    // ② 从 REQUEST_URI 里找 —— 面板重写成 /1 → r.php?p=1 时通常已带参数，
    //    但有的环境只做内部转发、不带 query，所以这里再兜一层。
    $uri  = (string) ($_SERVER['REQUEST_URI'] ?? '');
    $path = parse_url($uri, PHP_URL_PATH);
    $path = is_string($path) ? trim($path, '/') : '';
    if ($path === '') {
        return null;
    }

    // 去掉可能的入口文件名与目录前缀，取最后一段
    $segments = array_values(array_filter(explode('/', $path), static fn($s) => $s !== ''));
    $last     = $segments === [] ? '' : (string) end($segments);
    $last     = preg_replace('/\.php$/i', '', $last) ?? $last;

    if ($last !== '') {
        foreach ($map as $key => $configured) {
            if ($configured !== '' && strcasecmp($last, $configured) === 0) {
                return $key;
            }
        }
    }

    // ③ 路径里直接出现语义名（例如 /usbeam → r.php）
    foreach ($segments as $segment) {
        $segment = preg_replace('/\.php$/i', '', (string) $segment) ?? (string) $segment;
        if (isset($map[$segment])) {
            return $segment;
        }
    }

    return null;
}

/**
 * 从 manifest / stats 里汇总出给下游看的元信息。
 *
 * @return array<string,mixed>
 */
function rp_share_metadata(array $manifest, array $stats, array $point, array $share): array
{
    $merged = (array) ($stats['stats']['merged'] ?? []);
    $usbeam = (array) ($stats['stats']['usbeam'] ?? []);
    $s302   = (array) ($stats['stats']['s302'] ?? []);

    $count = $point['kind'] === 's302'
        ? (int) ($s302['domains'] ?? 0)
        : (int) ($usbeam['hosts_lines'] ?? 0);

    return [
        'generated_at' => (string) ($manifest['generated_at'] ?? ''),
        'label'        => (string) $point['label'],
        'kind'         => (string) $point['kind'],
        'count'        => $count,
        'merged_domains' => (int) ($merged['domains'] ?? 0),
        'exit'         => (string) ($manifest['exit'] ?? 'unknown'),
        'include_note' => !empty($share['send_metadata']),
    ];
}

/**
 * hosts 文本输出：原样给产物，必要时在顶部补一段来源注释。
 *
 * 产物本身已有注释头；这里只在用户要求时**追加**一行「由谁中转」，
 * 方便拿到规则的人知道去哪更新。
 */
function rp_share_hosts_body(string $raw, array $meta, bool $withNote): string
{
    if (!$withNote) {
        return $raw;
    }

    $host = rp_share_host();
    $note = [];
    $note[] = '# 由 ' . $host . ' 中转发布 · ' . $meta['label'];
    if ($meta['generated_at'] !== '') {
        $note[] = '# 生成时间: ' . $meta['generated_at'];
    }
    $note[] = '# 订阅地址: ' . rp_share_url($meta['kind']);

    // 塞在原有注释头之后、第一条规则之前，避免破坏 hosts 解析
    $lines = preg_split('/\r\n|\r|\n/', $raw) ?: [];
    $insertAt = 0;
    foreach ($lines as $i => $line) {
        $t = trim($line);
        if ($t === '' || str_starts_with($t, '#')) {
            $insertAt = $i + 1;
            continue;
        }
        break;
    }

    array_splice($lines, $insertAt, 0, array_merge($note, ['']));

    return implode("\n", $lines);
}

/**
 * JSON 输出：结构化给程序用。
 *
 * hosts 产物是文本，所以这里把「IP + 域名 + 注释」重新解析成数组，
 * 比让下游自己正则解析友好得多。
 */
function rp_share_json_body(string $raw, array $meta, array $point): string
{
    $entries = rp_share_parse_hosts($raw);

    $payload = [
        'ok'           => true,
        'kind'         => $meta['kind'],
        'label'        => $meta['label'],
        'generated_at' => $meta['generated_at'],
        'count'        => count($entries),
        'source'       => rp_share_host(),
        'subscribe'    => rp_share_url($meta['kind']),
        'entries'      => $entries,
    ];

    // 只给域名列表时更省流量的用法
    if (trim((string) ($_GET['only'] ?? '')) === 'domains') {
        $payload['domains'] = array_values(array_unique(array_map(
            static fn(array $e): string => (string) $e['domain'],
            $entries
        )));
    }

    return json_encode(
        $payload,
        JSON_PRETTY_PRINT | JSON_UNESCAPED_UNICODE | JSON_UNESCAPED_SLASHES
    ) . "\n";
}

/**
 * 把 hosts 文本解析成 [{ip, domain, comment}, ...]。
 *
 * 容忍分隔符是空格或制表符、以及行尾注释。
 *
 * @return list<array{ip:string,domain:string,comment:string}>
 */
function rp_share_parse_hosts(string $raw): array
{
    $out = [];
    $lines = preg_split('/\r\n|\r|\n/', $raw) ?: [];

    foreach ($lines as $line) {
        $line = trim($line);
        if ($line === '' || str_starts_with($line, '#')) {
            continue;
        }

        // 行内注释：# 之后的部分
        $comment = '';
        $hashPos = strpos($line, '#');
        if ($hashPos !== false) {
            $comment = trim(substr($line, $hashPos + 1));
            $line    = trim(substr($line, 0, $hashPos));
        }
        if ($line === '') {
            continue;
        }

        $parts = preg_split('/\s+/', $line) ?: [];
        if (count($parts) < 2) {
            continue;
        }

        $ip     = (string) array_shift($parts);
        $domain = (string) array_shift($parts);

        $out[] = [
            'ip'      => $ip,
            'domain'  => $domain,
            // 第一个域名词之后若还有内容，一并当作注释（hosts 一行可写多个域名）
            'comment' => trim($comment . ($parts === [] ? '' : ' ' . implode(' ', $parts))),
        ];
    }

    return $out;
}

/** 当前请求的域名（含端口），用于生成给下游显示的订阅地址。 */
function rp_share_host(): string
{
    $host = (string) ($_SERVER['HTTP_HOST'] ?? '');
    if ($host !== '') {
        return $host;
    }

    return 'localhost';
}

/** 生成某个中转点的对外完整 URL。 */
function rp_share_url(string $kind): string
{
    $cfg   = rp_config();
    $share = (array) ($cfg['share'] ?? []);
    $path  = $kind === 's302'
        ? (string) ($share['s302_path'] ?? '2')
        : (string) ($share['usbeam_path'] ?? '1');

    $https = (!empty($_SERVER['HTTPS']) && $_SERVER['HTTPS'] !== 'off')
        || (($_SERVER['HTTP_X_FORWARDED_PROTO'] ?? '') === 'https');

    return ($https ? 'https' : 'http') . '://' . rp_share_host() . '/' . $path;
}

/** 404：按「不存在」处理。 */
function rp_share_404(): void
{
    rp_share_error(404, 'Not Found', '这个地址没有对应的规则。');
}

/**
 * 输出一个简洁的纯文本错误（不做 HTML 页面）。
 *
 * 下游多半是程序，给纯文本比给一片 HTML 更好处理。
 */
function rp_share_error(int $status, string $title, string $detail): void
{
    if (!headers_sent()) {
        http_response_code($status);
        header('Content-Type: text/plain; charset=utf-8');
        header('X-Robots-Tag: noindex');
    }

    echo $title . "\n\n" . $detail . "\n";
    exit;
}
