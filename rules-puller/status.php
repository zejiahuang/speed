<?php

declare(strict_types=1);

/**
 * 只读状态面板。
 *
 * 默认需要登录（security.protect_status = true）；关掉之后任何人都能看，
 * 但页面里只有规则规模与运行状态，没有令牌、密码等敏感内容。
 * 需要做操作请去 admin.php。
 */

use RulesPuller\Store;
use RulesPuller\View;
use RulesPuller\WebApp;

require __DIR__ . '/bootstrap.php';

$cfg = rp_prepare_dirs(rp_config());
date_default_timezone_set((string) ($cfg['timezone'] ?? 'UTC'));

$app         = new WebApp($cfg);
$protect     = !empty($cfg['security']['protect_status']);

$app->run(static function (WebApp $app): void {
    $cfg  = $app->cfg();
    $auth = $app->auth();

    $manifest = Store::readJson($cfg['paths']['out'] . '/manifest.json');
    $stats    = Store::readJson($cfg['paths']['out'] . '/stats.json');
    $state    = Store::readJson($cfg['paths']['state']) ?? [];

    if ($manifest === null) {
        $body = View::alert('info', '还没有任何产物。到管理后台点一次「立即拉取」，或先在命令行跑 php fetch.php。');
        echo View::page('规则拉取状态', $body, [
            'nav_links' => [
                ['label' => '概览', 'href' => 'status.php', 'active' => true],
                ['label' => '管理后台', 'href' => 'admin.php'],
            ],
            'show_logout' => !empty($app->cfg()['security']['protect_status']),
            'nav_meta'    => $auth->clientIp(),
        ]);

        return;
    }

    // ---------------------------------------------------------- 指标
    $exitMap = [
        'ok'       => ['正常', 'ok'],
        'degraded' => ['降级（用了本地副本）', 'warn'],
        'partial'  => ['部分失败', 'warn'],
    ];
    $exitKey  = (string) ($state['last_exit'] ?? ($manifest['exit'] ?? 'unknown'));
    $exitInfo = $exitMap[$exitKey] ?? ['未知', 'warn'];

    $m = (array) ($stats['stats']['merged'] ?? []);
    $u = (array) ($stats['stats']['usbeam'] ?? []);
    $s = (array) ($stats['stats']['s302'] ?? []);

    $metrics = '<div class="grid grid--metrics">'
        . View::metric('最近一次状态', $exitInfo[0], (string) ($state['last_run'] ?? ''))
        . View::metric('累计运行', (string) (int) ($state['run_count'] ?? 0), '上次耗时 ' . (int) ($state['duration_ms'] ?? 0) . ' ms')
        . View::metric('聚合域名', (string) (int) ($m['domains'] ?? 0), '有地址 ' . (int) ($m['domains_with_ip'] ?? 0) . ' · 有上游 ' . (int) ($m['domains_with_upstream'] ?? 0))
        . View::metric('hosts 记录', (string) (int) ($u['hosts_lines'] ?? 0), (int) ($u['groups'] ?? 0) . ' 分组 · ' . (int) ($u['addresses'] ?? 0) . ' 个地址')
        . '</div>';

    // ---------------------------------------------------------- 数据源
    $rows = '';
    foreach ((array) ($manifest['sources'] ?? []) as $key => $source) {
        $bytes = (int) ($source['bytes'] ?? 0);
        if (!empty($source['stale'])) {
            $badge = View::badge('warn', '本地副本');
        } elseif ($bytes > 0) {
            $badge = View::badge('ok', '正常');
        } else {
            $badge = View::badge('error', '失败');
        }

        $rows .= '<tr>'
            . '<td><code>' . View::e((string) $key) . '</code><div class="muted">' . View::e((string) ($source['label'] ?? '')) . '</div></td>'
            . '<td>' . $badge . '</td>'
            . '<td class="num">' . View::e((string) ($source['http'] ?? '-')) . '</td>'
            . '<td class="num">' . View::e(View::humanBytes($bytes)) . '</td>'
            . '<td class="mono">' . View::e((string) ($source['url'] ?? '-')) . '</td>'
            . '<td class="muted nowrap">' . View::e((string) ($source['fetched_at'] ?? '-')) . '</td>'
            . '</tr>';
    }
    if ($rows === '') {
        $rows = View::emptyRow(6, '没有数据源记录');
    }

    $sources = '<section class="block"><h2>数据源</h2><div class="card card--table"><div class="table-wrap"><table class="table">'
        . '<thead><tr><th>数据源</th><th>状态</th><th class="num">HTTP</th><th class="num">大小</th><th>地址</th><th>抓取时间</th></tr></thead>'
        . '<tbody>' . $rows . '</tbody></table></div></div></section>';

    // ---------------------------------------------------------- 产物
    $fileRows = '';
    foreach ((array) ($manifest['files'] ?? []) as $name => $info) {
        $fileRows .= '<tr><td><code>' . View::e((string) $name) . '</code></td>'
            . '<td class="num">' . View::e(View::humanBytes((int) ($info['bytes'] ?? 0))) . '</td>'
            . '<td class="mono">' . View::e(substr((string) ($info['sha256'] ?? ''), 0, 16)) . '…</td></tr>';
    }
    if ($fileRows === '') {
        $fileRows = View::emptyRow(3, '暂无产物');
    }

    $files = '<section class="block"><h2>产物</h2><div class="card card--table"><div class="table-wrap"><table class="table">'
        . '<thead><tr><th>文件</th><th class="num">大小</th><th>SHA256</th></tr></thead><tbody>' . $fileRows . '</tbody></table></div></div>'
        . '<p class="field__help" style="margin-top:10px">产物目录：<code>' . View::e((string) $cfg['paths']['out']) . '</code></p></section>';

    // ---------------------------------------------------------- 警告
    $warnings = '';
    if (!empty($manifest['warnings'])) {
        $items = '';
        foreach ($manifest['warnings'] as $warning) {
            $items .= View::alert('warn', (string) $warning);
        }
        $warnings = '<section class="block"><h2>警告</h2><div class="stack">' . $items . '</div></section>';
    }

    // ---------------------------------------------------------- 日志尾部
    $logs = glob($cfg['paths']['logs'] . '/pull-*.log') ?: [];
    usort($logs, static fn(string $a, string $b): int => (@filemtime($b) ?: 0) <=> (@filemtime($a) ?: 0));

    $logHtml = '';
    if ($logs !== []) {
        $content = Store::read((string) $logs[0]);
        $lines   = $content === null ? [] : array_slice(preg_split('/\r\n|\r|\n/', trim($content)) ?: [], -40);
        foreach ($lines as $line) {
            $class = 'log--info';
            if (strpos($line, '] ERROR') !== false) {
                $class = 'log--error';
            } elseif (strpos($line, '] WARN') !== false) {
                $class = 'log--warn';
            } elseif (strpos($line, '] DEBUG') !== false) {
                $class = 'log--debug';
            }
            $logHtml .= '<span class="log__line ' . $class . '">' . View::e($line) . '</span>';
        }
    }
    if ($logHtml === '') {
        $logHtml = '<span class="log__line log--debug">暂无日志</span>';
    }

    $logSection = '<section class="block"><h2>最近日志</h2><div class="log">' . $logHtml . '</div></section>';

    echo View::page('规则拉取状态', $metrics . $sources . $files . $warnings . $logSection, [
        'nav_links' => [
            ['label' => '概览', 'href' => 'status.php', 'active' => true],
            ['label' => '管理后台', 'href' => 'admin.php'],
        ],
        'show_logout' => !empty($cfg['security']['protect_status']),
        'nav_meta'    => $auth->clientIp(),
        'subtitle'    => '数据源：UsbEAm Hosts Editor 规则 + Steamcommunity 302 规则 · 只读视图',
    ]);
}, ['require_login' => $protect]);
