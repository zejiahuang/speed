<?php

declare(strict_types=1);

/**
 * 规则自动拉取与聚合 —— 主入口。
 *
 * 命令行：
 *   php fetch.php                 正常拉取（带条件请求，内容未变则复用缓存）
 *   php fetch.php --force         忽略 ETag / Last-Modified，强制重新下载
 *   php fetch.php --only=usbeam   只拉某一个源
 *   php fetch.php --dry-run       只抓取与解析，不写产物
 *   php fetch.php --json          以 JSON 输出结果（供定时任务/监控采集）
 *   php fetch.php --verbose       打印 DEBUG 级日志
 *
 * 退出码：0 全部成功；1 部分失败（已用本地副本兜底）；2 关键源全部失败。
 *
 * 注意：本文件**不能**加 `#!/usr/bin/env php` shebang。
 * shebang 在非 CLI SAPI 下不会被剥掉，会让 declare(strict_types=1) 不再是
 * 首条语句，PHP 直接 Fatal error —— 而 `php -l` 检查不出来。
 */

use RulesPuller\Aggregator;
use RulesPuller\Auth;
use RulesPuller\Http;
use RulesPuller\IpRateGuard;
use RulesPuller\Lock;
use RulesPuller\Logger;
use RulesPuller\RateLimit;
use RulesPuller\S302Parser;
use RulesPuller\Store;
use RulesPuller\UsbeamParser;

require __DIR__ . '/bootstrap.php';

$isCli = rp_is_cli();

// ------------------------------------------------------------------ 参数
$opts = $isCli ? rp_parse_args($argv ?? []) : rp_parse_web_opts();

if ($opts['help']) {
    fwrite(STDOUT, rp_usage());
    exit(0);
}

if (!$isCli && PHP_SAPI !== 'cli') {
    header('Content-Type: application/json; charset=utf-8');
}

$cfg = rp_prepare_dirs(rp_config());

date_default_timezone_set((string) ($cfg['timezone'] ?? 'UTC'));

$logFile = $cfg['paths']['logs'] . '/pull-' . date('Y-m-d') . '.log';
$log     = new Logger($logFile, $isCli && !$opts['quiet'], $opts['verbose']);

// ------------------------------------------------------------------ 网页触发闸门
// 顺序按「代价从低到高」排：白名单 → 开关 → 令牌 → 限流。
// 这样被拦掉的请求不会去读文件、更不会去联网。
$limiter = new RateLimit((string) ($cfg['paths']['ratelimit'] ?? (__DIR__ . '/data/ratelimit.json')));

if (!$isCli) {
    $auth = new Auth($cfg, $log);

    if (!$auth->ipAllowed()) {
        $log->warn('网页触发被 IP 白名单拒绝', ['ip' => $auth->clientIp()]);
        rp_web_deny(403, '来源 IP 不在白名单内', $opts, $isCli);
    }

    // 每 IP 访问频率（这里没有会话，一律按公共额度算）
    $guard = (new IpRateGuard($cfg, $auth, $limiter, $log))->check(false);
    if (!$guard['allowed']) {
        if (!headers_sent()) {
            header('Retry-After: ' . max(1, (int) $guard['retry_after']));
        }
        rp_web_deny(429, '来源 IP ' . $guard['ip'] . ' 访问过于频繁（最近一分钟 ' . $guard['used']
            . ' 次，上限 ' . $guard['max'] . ' 次），请 ' . Auth::humanDuration((int) $guard['retry_after'])
            . '后再试', $opts, $isCli, $guard);
    }

    if (empty($cfg['security']['allow_web_trigger'])) {
        $log->warn('网页触发已关闭，请求被拒绝', ['ip' => $auth->clientIp()]);
        rp_web_deny(403, '网页触发已关闭，只能通过命令行或定时任务触发', $opts, $isCli);
    }

    // 两条互通的授权途径，任一成立即可：
    //   ① `web_token`（用户自己设的，给外部/脚本/定时任务用）；
    //   ② 本机内部密钥（后台「立即拉取」在无 CLI 时自触发用，无需任何配置）。
    // ② 还额外要求请求必须来自回环，避免外部拿它当免令牌入口。
    $token     = (string) ($cfg['web_token'] ?? '');
    $given     = (string) ($_GET['token'] ?? $_POST['token'] ?? '');
    $secretOk  = $auth->isLocalRequest()
        && rp_internal_secret_matches((string) ($_GET['secret'] ?? $_POST['secret'] ?? ''));
    $tokenOk   = $token !== '' && hash_equals($token, $given);

    if (!$secretOk && !$tokenOk) {
        $log->warn('网页触发令牌不匹配', ['ip' => $auth->clientIp()]);
        rp_web_deny(403, '未授权：需要 web_token，或从本机后台发起', $opts, $isCli);
    }

    // 带 `internal=1` 时视为「后台管理员点按钮」发起，跳过公共触发的
    // 最小间隔限制。理由：
    //   ① 该请求必须带正确 token 或内部密钥，和外部触发一样安全；
    //   ② 后台自己已有 admin_action_interval 节流，双重限流会让
    //      「点一下、等 5 分钟」变成常态，体验很差；
    //   ③ 仅当请求来自本机回环、或持有内部密钥时才认这个标记，
    //      防止外部伪造。
    $internal = ((string) ($_GET['internal'] ?? '')) === '1'
        && ($auth->isLocalRequest() || $secretOk);

    if (!$internal) {
        $gate = $limiter->check(
            'web_trigger',
            (int) ($cfg['limits']['web_min_interval'] ?? 0),
            (int) ($cfg['limits']['web_daily_max'] ?? 0)
        );
        if (!$gate['allowed']) {
            $log->warn('网页触发被限流', ['ip' => $auth->clientIp(), '原因' => $gate['reason']]);
            if (!headers_sent()) {
                header('Retry-After: ' . max(1, (int) $gate['retry_after']));
            }
            rp_web_deny(429, $gate['reason'] . '（' . Auth::humanDuration((int) $gate['retry_after']) . '后可重试）', $opts, $isCli, $gate);
        }
    } else {
        // 仍然记录一次，保证审计与统计完整
        $limiter->record('web_trigger');
    }

    @set_time_limit(max(60, (int) ($cfg['limits']['max_runtime_seconds'] ?? 600)));
}

// ------------------------------------------------------------------ 单实例锁
$lock = new Lock($cfg['paths']['lock']);
if (!$lock->acquire()) {
    $holder = $lock->holder();
    $log->warn('已有一次拉取正在进行，本次跳过', ['holder' => $holder]);
    rp_finish([
        'ok'     => false,
        'error'  => 'locked',
        'holder' => $holder,
    ], $opts, $isCli, 0);
}

// 拿到锁才算「真的开始跑」—— 没抢到锁的请求不该消耗触发配额
if (!$isCli) {
    $limiter->record('web_trigger');
}

$deadline  = time() + max(30, (int) ($cfg['limits']['max_runtime_seconds'] ?? 600));
$startedAt = microtime(true);
$log->info('=== 开始拉取 ===', ['php' => PHP_VERSION, 'sapi' => PHP_SAPI]);

$state = Store::readJson($cfg['paths']['state']) ?? ['version' => 1, 'sources' => []];
if (!isset($state['sources']) || !is_array($state['sources'])) {
    $state['sources'] = [];
}

$http = new Http((array) $cfg['http'], $log);

$results   = [];
$parsed    = ['usbeam' => null, 's302_rules' => null];
$meta      = ['sources' => [], 'warnings' => []];
$critical  = 0;
$partial   = 0;
$staleUsed = [];

foreach ((array) $cfg['sources'] as $key => $source) {
    if ($opts['only'] !== null && $opts['only'] !== (string) $key) {
        continue;
    }

    if (empty($source['enabled'])) {
        $log->info('数据源已停用，跳过', ['source' => $key]);
        continue;
    }

    if (time() > $deadline) {
        $critical++;
        $log->error('已超过单次运行时间上限，中止后续数据源', [
            '上限秒' => (int) ($cfg['limits']['max_runtime_seconds'] ?? 600),
        ]);
        break;
    }

    $label   = (string) ($source['label'] ?? $key);
    $required = (bool) ($source['required'] ?? false);
    $prev    = (array) ($state['sources'][$key] ?? []);
    $rawPath = $cfg['paths']['raw'] . '/' . $key . '.raw';

    $log->info('— 数据源：' . $label, ['url' => $source['urls'][0] ?? '']);

    $result = $http->fetchSource((array) $source, $prev, $opts['force']);
    $body   = null;

    if ($result['ok'] && !$result['not_modified']) {
        $body = (string) $result['body'];
        Store::writeAtomic($rawPath, $body);
        $log->info('原始文件已保存', ['path' => $rawPath, 'bytes' => strlen($body)]);
    } elseif ($result['ok'] && $result['not_modified']) {
        $body = Store::read($rawPath);
        if ($body === null) {
            // 本地没有副本，退回一次强制下载
            $log->warn('收到 304 但本地无原始文件，改为强制下载', ['source' => $key]);
            $result = $http->fetchSource((array) $source, [], true);
            if ($result['ok'] && !$result['not_modified']) {
                $body = (string) $result['body'];
                Store::writeAtomic($rawPath, $body);
            }
        }
    }

    if ($body === null || $body === '') {
        $stale = Store::read($rawPath);
        if ($stale !== null && $stale !== '') {
            $body        = $stale;
            $staleUsed[] = $key;
            $partial++;
            $log->warn('抓取失败，已回退到本地原始文件继续聚合', [
                'source' => $key,
                'error'  => $result['error'],
                'age'    => date('Y-m-d H:i:s', (int) (@filemtime($rawPath) ?: time())),
            ]);
        } else {
            if ($required) {
                $critical++;
            }
            $log->error('抓取失败且没有本地副本', ['source' => $key, 'error' => $result['error']]);
            $results[$key] = [
                'label'  => $label,
                'ok'     => false,
                'error'  => $result['error'],
                'attempts' => $result['attempts'],
            ];
            continue;
        }
    } else {
        $results[$key] = [
            'label'         => $label,
            'ok'            => true,
            'url'           => $result['url'],
            'http'          => $result['status'],
            'bytes'         => strlen($body),
            'sha256'        => Store::sha256($body),
            'etag'          => $result['etag'],
            'last_modified' => $result['last_modified'],
            'not_modified'  => $result['not_modified'],
            'attempts'      => $result['attempts'],
        ];
    }

    $meta['sources'][$key] = [
        'label'    => $label,
        'url'      => $results[$key]['url'] ?? null,
        'bytes'    => strlen($body),
        'sha256'   => Store::sha256($body),
        'fetched_at' => date('c'),
        'stale'    => in_array($key, $staleUsed, true),
    ];

    // -------------------------------------------------------- 解析（带缓存）
    $cachePath = $cfg['paths']['cache'] . '/' . $key . '.json';

    try {
        switch ($key) {
            case 'usbeam':
                $parsed['usbeam'] = rp_parse_with_cache(
                    $cachePath,
                    $results[$key]['not_modified'] ?? false,
                    $body,
                    static fn(string $raw): array => UsbeamParser::parse($raw, (int) (rp_config()['limits']['max_entries'] ?? 0)),
                    $log,
                    $key
                );
                $stats = $parsed['usbeam']['stats'];
                $log->info('UsbEAm 规则解析完成', [
                    'groups'    => $stats['groups'],
                    'entries'   => $stats['entries'],
                    'domains'   => $stats['domains'],
                    'addresses' => $stats['addresses'],
                    'version'   => $parsed['usbeam']['meta']['version'],
                    'updated'   => $parsed['usbeam']['meta']['update_time'],
                ]);
                foreach ($parsed['usbeam']['warnings'] as $warning) {
                    $meta['warnings'][] = $warning;
                    $log->warn($warning);
                }
                break;

            case 's302_rules':
                $parsed['s302_rules'] = rp_parse_with_cache(
                    $cachePath,
                    $results[$key]['not_modified'] ?? false,
                    $body,
                    static fn(string $raw): array => S302Parser::parse($raw, (array) rp_config()['s302']),
                    $log,
                    $key
                );
                $stats = $parsed['s302_rules']['stats'];
                $log->info('S302 规则解析完成', [
                    'services'    => $stats['services'],
                    'domains'     => $stats['domains'],
                    'upstreams'   => $stats['upstreams'],
                    'wildcards'   => $stats['wildcards'],
                    'last_update' => $parsed['s302_rules']['rules']['last_update_text'],
                ]);
                foreach ($parsed['s302_rules']['warnings'] as $warning) {
                    $meta['warnings'][] = $warning;
                    $log->warn($warning);
                }
                break;

            case 's302_version':
                $version = trim($body);
                $previous = $state['sources']['s302_version']['value'] ?? null;
                $results[$key]['value'] = $version;
                if ($previous !== null && $previous !== $version) {
                    $log->warn('S302 有新版本', ['旧' => $previous, '新' => $version]);
                } else {
                    $log->info('S302 版本', ['version' => $version]);
                }
                break;
        }
    } catch (Throwable $e) {
        $critical++;
        $results[$key]['ok']    = false;
        $results[$key]['error'] = '解析异常: ' . $e->getMessage();
        $log->error('解析失败', ['source' => $key, 'error' => $e->getMessage()]);
    }

    // -------------------------------------------------------- 记录状态
    $state['sources'][$key] = array_merge((array) ($state['sources'][$key] ?? []), [
        'etag'          => $results[$key]['etag'] ?? ($prev['etag'] ?? null),
        'last_modified' => $results[$key]['last_modified'] ?? ($prev['last_modified'] ?? null),
        'sha256'        => $results[$key]['sha256'] ?? ($prev['sha256'] ?? null),
        'bytes'         => $results[$key]['bytes'] ?? ($prev['bytes'] ?? 0),
        'url'           => $results[$key]['url'] ?? ($prev['url'] ?? null),
        'fetched_at'    => date('c'),
        'ok'            => (bool) ($results[$key]['ok'] ?? false),
    ]);
    if (isset($results[$key]['value'])) {
        $state['sources'][$key]['value'] = $results[$key]['value'];
    }
}

// ------------------------------------------------------------------ 未刷新源的兜底
// `--only=<x>` 只刷新一个源，但聚合需要全部源。若此时直接聚合，产物会被写空，
// 所以这里为未刷新的源补上解析缓存 —— 语义是「只刷新这一个，其余用上次的」。
foreach ($parsed as $key => $value) {
    if ($value !== null) {
        continue;
    }
    $cached = Store::readJson($cfg['paths']['cache'] . '/' . $key . '.json');
    if ($cached !== null) {
        $parsed[$key] = $cached;
        $log->info('复用未刷新数据源的解析缓存', ['source' => $key]);
    } else {
        $message = '数据源 ' . $key . ' 未刷新且没有解析缓存，聚合结果会缺少它的内容';
        $meta['warnings'][] = $message;
        $log->warn($message);
    }
}

// ------------------------------------------------------------------ 聚合
$log->info('— 聚合');

$dohCachePath = $cfg['paths']['cache'] . '/doh.json';
$resolver     = static function (array $domains) use ($http, $cfg, $log, $dohCachePath): array {
    $ttl   = (int) ($cfg['placeholders']['cache_ttl'] ?? 604800);
    $now   = time();
    $cache = Store::readJson($dohCachePath) ?? [];

    $fresh = [];
    $todo  = [];
    foreach ($domains as $domain) {
        $entry = $cache[$domain] ?? null;
        if (is_array($entry) && isset($entry['at'], $entry['ips']) && ($now - (int) $entry['at']) < $ttl) {
            $fresh[$domain] = (array) $entry['ips'];
        } else {
            $todo[] = $domain;
        }
    }

    if ($todo !== []) {
        $resolved = $http->resolveMany(
            $todo,
            (int) ($cfg['placeholders']['concurrency'] ?? 6),
            (int) ($cfg['placeholders']['query_timeout'] ?? 5)
        );
        foreach ($resolved as $domain => $ips) {
            // 只缓存成功的：失败的留到下次重试，避免把一次抖动固化下来
            $cache[$domain] = ['at' => $now, 'ips' => array_values((array) $ips)];
            $fresh[$domain] = array_values((array) $ips);
        }
        $log->info('占位符 DoH 兜底解析', ['待解析' => count($todo), '成功' => count($resolved)]);

        // 清理过期项
        foreach ($cache as $domain => $entry) {
            if (!is_array($entry) || !isset($entry['at']) || ($now - (int) $entry['at']) >= $ttl) {
                unset($cache[$domain]);
            }
        }
        if (!Store::writeJson($dohCachePath, $cache)) {
            $log->warn('DoH 缓存写入失败', ['path' => $dohCachePath]);
        }
    }

    return $fresh;
};

$aggregator = new Aggregator($cfg, $log);
$agg        = $aggregator->build($parsed['usbeam'], $parsed['s302_rules'], $resolver);

$log->info('聚合完成', [
    '唯一域名'   => $agg['stats']['merged']['domains'],
    '有地址'     => $agg['stats']['merged']['domains_with_ip'],
    '有上游'     => $agg['stats']['merged']['domains_with_upstream'],
    '仅 UsbEAm'  => $agg['stats']['merged']['only_usbeam'],
    '仅 S302'    => $agg['stats']['merged']['only_s302'],
    '两源共有'   => $agg['stats']['merged']['both'],
    'hosts 行数' => $agg['stats']['usbeam']['hosts_lines'],
]);

foreach ($agg['warnings'] as $warning) {
    $meta['warnings'][] = $warning;
    $log->warn($warning);
}
$meta['warnings'] = array_values(array_unique($meta['warnings']));

$files = $aggregator->render($agg, $parsed['usbeam'], $parsed['s302_rules'], $meta);

// ------------------------------------------------------------------ 规模闸门
// 规则源是「远端可下发、无签名校验」的。万一被投毒或格式突变，
// 这里先把异常规模挡在写盘之前 —— 上一轮的产物原样保留，比写坏了好。
$maxLines = (int) ($cfg['limits']['max_hosts_lines'] ?? 0);
$hostsLines = (int) ($agg['stats']['usbeam']['hosts_lines'] ?? 0);
if ($maxLines > 0 && $hostsLines > $maxLines) {
    $message = 'hosts 行数 ' . $hostsLines . ' 超过上限 ' . $maxLines . '，已放弃写入（上一轮产物保持不变）';
    $log->error($message);
    $critical++;
    $meta['warnings'][] = $message;
    unset($files['hosts.txt']);
}

$maxBytes = (int) ($cfg['limits']['max_output_bytes'] ?? 0);
if ($maxBytes > 0) {
    foreach ($files as $name => $content) {
        if (strlen($content) > $maxBytes) {
            $message = '产物 ' . $name . ' 大小 ' . strlen($content) . ' 字节超过上限 ' . $maxBytes . '，已跳过写入';
            $log->error($message);
            $critical++;
            $meta['warnings'][] = $message;
            unset($files[$name]);
        }
    }
}

if (time() > $deadline) {
    $message = '已超过单次运行时间上限，放弃写入产物（上一轮产物保持不变）';
    $log->error($message, ['上限秒' => (int) ($cfg['limits']['max_runtime_seconds'] ?? 600)]);
    $critical++;
    $meta['warnings'][] = $message;
    $files = [];
}

// ------------------------------------------------------------------ 落盘
$written = [];
if ($opts['dry_run']) {
    $log->warn('--dry-run：跳过写产物');
} else {
    foreach ($files as $name => $content) {
        $path = $cfg['paths']['out'] . '/' . $name;
        if (!Store::writeAtomic($path, $content)) {
            $log->error('写入失败', ['path' => $path]);
            $critical++;
            continue;
        }
        $written[$name] = [
            'bytes'  => strlen($content),
            'sha256' => Store::sha256($content),
        ];
    }
    $log->info('产物已写入', ['dir' => $cfg['paths']['out'], 'files' => count($written)]);

    // 清单
    $manifest = [
        'generated_at'  => date('c'),
        'duration_ms'   => (int) round((microtime(true) - $startedAt) * 1000),
        'sources'       => $meta['sources'],
        'files'         => $written,
        'stats'         => $agg['stats'],
        'warnings'      => $meta['warnings'],
        'exit'          => $critical > 0 ? 'partial' : 'ok',
    ];
    Store::writeAtomic(
        $cfg['paths']['out'] . '/manifest.json',
        json_encode($manifest, JSON_PRETTY_PRINT | JSON_UNESCAPED_UNICODE | JSON_UNESCAPED_SLASHES) . "\n"
    );

    // 每日快照
    if (empty($opts['no_archive'])) {
        $archiveDir = $cfg['paths']['archive'] . '/' . date('Y-m-d');
        foreach ($files as $name => $content) {
            Store::writeAtomic($archiveDir . '/' . $name, $content);
        }
        Store::writeAtomic(
            $archiveDir . '/manifest.json',
            json_encode($manifest, JSON_PRETTY_PRINT | JSON_UNESCAPED_UNICODE | JSON_UNESCAPED_SLASHES) . "\n"
        );
        $log->info('每日快照已保存', ['dir' => $archiveDir]);
    }
}

// ------------------------------------------------------------------ 清理
$removedArchive = Store::pruneDir($cfg['paths']['archive'], (int) $cfg['retention']['archive_days']);
$removedLogs    = Store::pruneDir($cfg['paths']['logs'], (int) $cfg['retention']['log_days']);
if ($removedArchive > 0 || $removedLogs > 0) {
    $log->info('清理过期文件', ['archive' => $removedArchive, 'logs' => $removedLogs]);
}

// ------------------------------------------------------------------ 状态
$state['version']      = 1;
$state['last_run']     = date('c');
$state['run_count']    = (int) ($state['run_count'] ?? 0) + 1;
$state['duration_ms']  = (int) round((microtime(true) - $startedAt) * 1000);
$state['last_exit']    = $critical > 0 ? 'partial' : ($partial > 0 ? 'degraded' : 'ok');
$state['last_stats']   = $agg['stats'];
$state['last_warnings'] = $meta['warnings'];
if ($critical === 0) {
    $state['last_success'] = date('c');
}
if (!$opts['dry_run']) {
    Store::writeJson($cfg['paths']['state'], $state);
}

$exitCode = $critical > 0 ? 2 : ($partial > 0 ? 1 : 0);

$summary = [
    'ok'          => $exitCode === 0,
    'exit_code'   => $exitCode,
    'duration_ms' => (int) round((microtime(true) - $startedAt) * 1000),
    'sources'     => $results,
    'stale_used'  => $staleUsed,
    'stats'       => $agg['stats'],
    'files'       => $written,
    'out_dir'     => $cfg['paths']['out'],
    'warnings'    => $meta['warnings'],
];

$log->info('=== 拉取结束 ===', [
    'exit'     => $exitCode,
    '耗时(ms)' => $summary['duration_ms'],
    '产物目录' => $cfg['paths']['out'],
]);

$lock->release();

rp_finish($summary, $opts, $isCli, $exitCode);

// ======================================================================
// 工具函数
// ======================================================================

/**
 * 解析或复用缓存。
 */
function rp_parse_with_cache(
    string $cachePath,
    bool $notModified,
    string $body,
    callable $parse,
    Logger $log,
    string $key
): array {
    if ($notModified) {
        $cached = Store::readJson($cachePath);
        if ($cached !== null) {
            $log->info('复用上次解析结果', ['source' => $key, 'cache' => $cachePath]);

            return $cached;
        }
        $log->warn('缓存缺失，重新解析原始文件', ['source' => $key]);
    }

    $result = $parse($body);
    if (!Store::writeJson($cachePath, $result)) {
        $log->warn('解析结果缓存写入失败', ['cache' => $cachePath]);
    }

    return $result;
}

/**
 * 网页触发被拒绝时的统一输出。
 *
 * @param array|null $extra 附加到 JSON 里的信息（例如限流详情）
 */
function rp_web_deny(int $status, string $message, array $opts, bool $isCli, ?array $extra = null): void
{
    if ($isCli) {
        fwrite(STDERR, $message . PHP_EOL);
        exit(2);
    }

    http_response_code($status);
    $payload = ['ok' => false, 'error' => $message];
    if ($extra !== null) {
        $payload['limit'] = $extra;
    }
    echo json_encode($payload, JSON_UNESCAPED_UNICODE | JSON_UNESCAPED_SLASHES);
    exit(2);
}

function rp_parse_args(array $argv): array
{
    $opts = [
        'force'      => false,
        'quiet'      => false,
        'verbose'    => false,
        'dry_run'    => false,
        'only'       => null,
        'json'       => false,
        'help'       => false,
        'no_archive' => false,
    ];

    foreach (array_slice($argv, 1) as $arg) {
        $arg = (string) $arg;
        if ($arg === '--force') {
            $opts['force'] = true;
        } elseif ($arg === '--quiet' || $arg === '-q') {
            $opts['quiet'] = true;
        } elseif ($arg === '--verbose' || $arg === '-v') {
            $opts['verbose'] = true;
        } elseif ($arg === '--dry-run') {
            $opts['dry_run'] = true;
        } elseif ($arg === '--json') {
            $opts['json'] = true;
        } elseif ($arg === '--no-archive') {
            $opts['no_archive'] = true;
        } elseif ($arg === '--help' || $arg === '-h') {
            $opts['help'] = true;
        } elseif (strncmp($arg, '--only=', 7) === 0) {
            $opts['only'] = substr($arg, 7);
        }
    }

    return $opts;
}

function rp_parse_web_opts(): array
{
    return [
        'force'      => !empty($_GET['force']),
        'quiet'      => true,
        'verbose'    => !empty($_GET['verbose']),
        'dry_run'    => !empty($_GET['dry_run']),
        'only'       => isset($_GET['only']) ? (string) $_GET['only'] : null,
        'json'       => true,
        'help'       => false,
        'no_archive' => false,
    ];
}

function rp_usage(): string
{
    return <<<TXT
规则自动拉取与聚合

用法:
  php fetch.php [选项]

选项:
  --force          忽略 ETag / Last-Modified，强制重新下载
  --only=<name>    只处理指定数据源（usbeam / s302_rules / s302_version）
  --dry-run        只抓取与解析，不写任何产物
  --no-archive     不写每日快照
  --json           以 JSON 输出结果
  --quiet, -q      不向标准输出打印日志（日志文件照写）
  --verbose, -v    打印 DEBUG 级日志
  --help, -h       显示本帮助

退出码:
  0  全部成功
  1  部分数据源失败，已用本地副本兜底
  2  关键数据源失败或产物写入失败

TXT;
}

/**
 * 统一收尾：按运行方式输出并退出。
 */
function rp_finish(array $summary, array $opts, bool $isCli, int $exitCode): void
{
    if ($isCli) {
        if ($opts['json']) {
            fwrite(STDOUT, (string) json_encode($summary, JSON_PRETTY_PRINT | JSON_UNESCAPED_UNICODE | JSON_UNESCAPED_SLASHES) . PHP_EOL);
        } elseif (!$opts['quiet']) {
            fwrite(STDOUT, PHP_EOL . '结果: ' . ($summary['ok'] ? '成功' : '失败')
                . '  退出码=' . $exitCode
                . '  耗时=' . ($summary['duration_ms'] ?? 0) . 'ms' . PHP_EOL);
            if (!empty($summary['stats']['merged'])) {
                $m = $summary['stats']['merged'];
                fwrite(STDOUT, '聚合: ' . $m['domains'] . ' 个域名（有地址 ' . $m['domains_with_ip']
                    . ' / 有上游 ' . $m['domains_with_upstream'] . '）' . PHP_EOL);
            }
            if (!empty($summary['out_dir'])) {
                fwrite(STDOUT, '产物: ' . $summary['out_dir'] . PHP_EOL);
            }
            foreach ((array) ($summary['warnings'] ?? []) as $warning) {
                fwrite(STDOUT, '警告: ' . $warning . PHP_EOL);
            }
        }
    } else {
        http_response_code($exitCode === 2 ? 500 : 200);
        echo json_encode($summary, JSON_UNESCAPED_UNICODE | JSON_UNESCAPED_SLASHES);
    }

    exit($exitCode);
}
