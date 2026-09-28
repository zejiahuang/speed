<?php

declare(strict_types=1);

/**
 * 管理控制台。
 *
 * 所有写操作都要求：已登录 + 有效 CSRF 令牌 + 通过写操作节流。
 * 危险操作（清缓存、重置配置、重置锁定）需要二次确认。
 */

use RulesPuller\Aggregator;
use RulesPuller\Audit;
use RulesPuller\Auth;
use RulesPuller\ConfigStore;
use RulesPuller\Store;
use RulesPuller\View;
use RulesPuller\WebApp;

require __DIR__ . '/bootstrap.php';

$cfg = rp_prepare_dirs(rp_config());
date_default_timezone_set((string) ($cfg['timezone'] ?? 'UTC'));

$app = new WebApp($cfg);

$app->run(static function (WebApp $app, string $action): void {
    $cfg = $app->cfg();

    // ---------------------------------------------------------- 写操作
    if ($action !== '') {
        $handled = rp_admin_handle_action($app, $action);
        if ($handled !== null) {
            $app->flash($handled['kind'], $handled['message']);
            $app->redirect($app->selfUrl(['tab' => $handled['tab'] ?? 'overview']));
        }
    }

    $tab = (string) ($_GET['tab'] ?? 'overview');
    if (!in_array($tab, ['overview', 'actions', 'share', 'config', 'logs', 'audit', 'security'], true)) {
        $tab = 'overview';
    }

    $data = rp_admin_load_data($cfg);
    $body = match ($tab) {
        'actions'  => rp_admin_tab_actions($app, $data),
        'share'    => rp_admin_tab_share($app, $data),
        'config'   => rp_admin_tab_config($app, $data),
        'logs'     => rp_admin_tab_logs($app, $data),
        'audit'    => rp_admin_tab_audit($app, $data),
        'security' => rp_admin_tab_security($app, $data),
        default    => rp_admin_tab_overview($app, $data),
    };

    $titles = [
        'overview' => '概览',
        'actions'  => '操作',
        'share'    => '中转站',
        'config'   => '配置',
        'logs'     => '日志',
        'audit'    => '审计',
        'security' => '安全',
    ];

    $state = (array) ($data['state'] ?? []);
    $meta  = '上次运行 ' . (string) ($state['last_run'] ?? '从未') . ' · ' . $app->auth()->clientIp();

    echo View::page($titles[$tab], $body, [
        'tab'       => $tab,
        'nav_meta'  => $meta,
        'subtitle'  => '数据源：UsbEAm Hosts Editor 规则 + Steamcommunity 302 规则',
        'flash'     => $app->takeFlash(),
    ]);
});

// ======================================================================
// 写操作
// ======================================================================

/**
 * @return array{kind:string,message:string,tab?:string}|null
 */
function rp_admin_handle_action(WebApp $app, string $action): ?array
{
    $cfg   = $app->cfg();
    $auth  = $app->auth();
    $audit = $app->audit();

    switch ($action) {
        // ---------------------------------------------------------- 触发拉取
        case 'pull':
        case 'pull_force':
            if (!$app->throttle('admin_pull')) {
                return ['kind' => 'warn', 'message' => '操作过于频繁，请稍后再试。', 'tab' => 'actions'];
            }
            $args   = $action === 'pull_force' ? ['--force'] : [];
            $result = rp_admin_run_pull($args);

            if (!$result['ok']) {
                $audit->record('pull_failed', ['error' => $result['error']]);
                return ['kind' => 'error', 'message' => '拉取未成功：' . $result['error'], 'tab' => 'actions'];
            }

            $summary = $result['summary'];
            $audit->record('pull', [
                'exit'     => $summary['exit_code'] ?? null,
                '耗时ms'   => $summary['duration_ms'] ?? null,
                '域名数'   => $summary['stats']['merged']['domains'] ?? null,
                '方式'     => $result['method'] ?? 'cli',
            ]);

            $kind = ($summary['exit_code'] ?? 0) === 0 ? 'success' : 'warn';

            $via = ($result['method'] ?? 'cli') === 'http'
                ? '，经 HTTP 自触发）'
                : '）';

            return [
                'kind'    => $kind,
                'message' => '拉取完成（退出码 ' . ($summary['exit_code'] ?? '?') . '，耗时 '
                    . ($summary['duration_ms'] ?? '?') . ' ms，聚合域名 '
                    . ($summary['stats']['merged']['domains'] ?? '?') . ' 个' . $via,
                'tab'     => 'overview',
            ];

        // ---------------------------------------------------------- 清缓存
        case 'clear_cache':
            $removed = 0;
            foreach (glob($cfg['paths']['cache'] . '/*.json') ?: [] as $file) {
                if (@unlink($file)) {
                    $removed++;
                }
            }
            $audit->record('clear_cache', ['删除文件数' => $removed]);

            return [
                'kind'    => 'success',
                'message' => '已清理 ' . $removed . ' 个解析缓存文件。下次拉取会重新解析（原始文件仍保留）。',
                'tab'     => 'actions',
            ];

        // ---------------------------------------------------------- 重置限流
        case 'reset_throttle':
            $app->limiter()->reset('web_trigger');
            $app->limiter()->reset('login');
            $app->limiter()->reset('admin_pull');
            $ipBuckets = $app->limiter()->resetPrefix('ip:');
            $audit->record('reset_throttle', ['清掉的 IP 桶' => $ipBuckets]);

            return [
                'kind'    => 'success',
                'message' => '网页触发、登录与按 IP 的访问计数已清零（清掉 ' . $ipBuckets . ' 个 IP 计数桶）。',
                'tab'     => 'actions',
            ];

        // ---------------------------------------------------------- 解锁登录
        case 'unlock_login':
            if (is_file($cfg['paths']['security'])) {
                @unlink($cfg['paths']['security']);
            }
            $audit->record('unlock_login');

            return ['kind' => 'success', 'message' => '登录失败计数已清空，锁定解除。', 'tab' => 'security'];

        // ---------------------------------------------------------- 清理旧文件
        case 'prune':
            $archive = Store::pruneDir($cfg['paths']['archive'], (int) $cfg['retention']['archive_days']);
            $logs    = Store::pruneDir($cfg['paths']['logs'], (int) $cfg['retention']['log_days']);
            $audit   = (new Audit($cfg['paths']['logs']))->prune((int) $cfg['retention']['log_days']);
            $app->audit()->record('prune', ['快照' => $archive, '日志' => $logs, '审计' => $audit]);

            return [
                'kind'    => 'success',
                'message' => '清理完成：快照 ' . $archive . ' 项、日志 ' . $logs . ' 项、审计 ' . $audit . ' 项。',
                'tab'     => 'actions',
            ];

        // ---------------------------------------------------------- 保存配置
        case 'save_config':
            if (!$app->throttle('admin_config')) {
                return ['kind' => 'warn', 'message' => '操作过于频繁，请稍后再试。', 'tab' => 'config'];
            }

            // 白名单自锁保护：改完之后自己都进不来就没意义了
            $selfLock = rp_admin_check_self_lockout($auth, (string) ($_POST['cfg_security__ip_allowlist'] ?? ''));
            if ($selfLock !== null) {
                return ['kind' => 'error', 'message' => $selfLock, 'tab' => 'config'];
            }

            $result = rp_settings_store()->saveFromForm($_POST);
            $app->audit()->record('save_config', ['变更项' => count($result['changed'])]);

            if (!$result['ok']) {
                return ['kind' => 'error', 'message' => '部分配置未保存：' . implode('；', $result['errors']), 'tab' => 'config'];
            }

            return [
                'kind'    => 'success',
                'message' => '配置已保存（' . count($result['changed']) . ' 项）。注意：这些值存在 data/settings.json，覆盖 config.php。',
                'tab'     => 'config',
            ];

        case 'reset_config':
            rp_settings_store()->reset();
            $app->audit()->record('reset_config');

            return ['kind' => 'success', 'message' => '后台改过的配置已全部清除，回到 config.php 的默认值。', 'tab' => 'config'];

        // ---------------------------------------------------------- 安全
        case 'change_password':
            if (!$app->throttle('admin_password')) {
                return ['kind' => 'warn', 'message' => '操作过于频繁，请稍后再试。', 'tab' => 'security'];
            }

            $current = (string) ($_POST['current_password'] ?? '');
            if (!password_verify($current, (string) ($cfg['security']['admin_password_hash'] ?? ''))) {
                $app->audit()->record('change_password', ['结果' => '当前密码不正确']);

                return ['kind' => 'error', 'message' => '当前密码不正确。', 'tab' => 'security'];
            }

            $result = $auth->setPassword(
                (string) ($_POST['new_password'] ?? ''),
                (string) ($_POST['new_password2'] ?? '')
            );
            if (!$result['ok']) {
                return ['kind' => 'error', 'message' => (string) $result['error'], 'tab' => 'security'];
            }
            $app->audit()->record('change_password', ['结果' => '成功']);

            return ['kind' => 'success', 'message' => '密码已更新。', 'tab' => 'security'];

        case 'set_token':
            $token = (string) ($_POST['web_token'] ?? '');
            if (!rp_settings_store()->setWebToken($token)) {
                return ['kind' => 'error', 'message' => '令牌至少 16 位，或写入 data/settings.json 失败。', 'tab' => 'security'];
            }
            $app->audit()->record('set_token', ['已设置' => $token !== '']);

            return [
                'kind'    => 'success',
                'message' => $token === ''
                    ? '已清空网页触发令牌 —— 现在只能靠命令行 / 定时任务触发。'
                    : '网页触发令牌已更新。请在触发地址里使用新令牌。',
                'tab'     => 'security',
            ];

        default:
            return null;
    }
}

/**
 * 保存配置前的自锁检查：白名单若把当前 IP 排除在外，直接拒绝。
 */
function rp_admin_check_self_lockout(Auth $auth, string $rawAllowlist): ?string
{
    $pieces = preg_split('/[\r\n,]+/', $rawAllowlist) ?: [];
    $rules  = [];
    foreach ($pieces as $piece) {
        $piece = trim($piece);
        if ($piece !== '') {
            $rules[] = $piece;
        }
    }

    if ($rules === []) {
        return null;   // 清空白名单 = 不限制，安全
    }

    $ip = $auth->clientIp();
    foreach ($rules as $rule) {
        if (Auth::ipMatches($ip, $rule)) {
            return null;
        }
    }

    return '白名单里没有当前来源 IP（' . $ip . '）。保存后你自己也会被挡在门外，已拒绝保存。'
        . '请把 ' . $ip . ' 加进去，或留空表示不限制。';
}

/**
 * 在后台触发一次拉取。
 *
 * 优先走独立的 PHP CLI 子进程，而不是把 fetch.php include 进来：
 * 后者会 exit()，把整个后台请求带走。
 *
 * 共享虚拟主机上常常找不到 PHP CLI（PHP_BINDIR 指向 php-fpm）。此时退回
 * 「同域 HTTP 自触发」：请求自己的 fetch.php?token=...&format=json。
 * 该路径由 Web SAPI 执行，不依赖 CLI，也不需要 proc_open。
 *
 * @return array{ok:bool,error:?string,summary:array,method:string}
 */
function rp_admin_run_pull(array $args = []): array
{
    $useHttp = in_array('--http', $args, true);
    $args    = array_values(array_filter($args, static fn($a) => $a !== '--http'));

    if (!$useHttp) {
        $binary = rp_admin_php_cli();
        if ($binary !== null && function_exists('proc_open')) {
            $result = rp_admin_run_pull_cli($binary, $args);
            // CLI 起不来（被 disable_functions / open_basedir 拦）时也退回 HTTP
            if ($result['ok']) {
                $result['method'] = 'cli';
                return $result;
            }
            $cliError = $result['error'];
        } elseif ($binary === null) {
            $cliError = '找不到 PHP CLI 可执行文件';
        } else {
            $cliError = 'proc_open 被禁用';
        }
    } else {
        $cliError = null;
    }

    // ---- 兜底：同域 HTTP 自触发（不依赖 CLI / proc_open）----
    $http = rp_admin_run_pull_http($args);
    if ($http['ok']) {
        $http['method'] = 'http';
        return $http;
    }

    $parts = [];
    if ($cliError !== null) {
        $parts[] = $cliError;
    }
    $parts[] = 'HTTP 自触发也失败：' . ($http['error'] ?? '未知原因');

    // 提示要指向**真正**的失败原因，别一律让人去填 CLI 路径。
    $httpErr = (string) ($http['error'] ?? '');
    if (str_contains($httpErr, '未授权') || str_contains($httpErr, '403')) {
        $hint = '。请在后台「配置」页设置 web_token，或确认 data/ 目录可写'
            . '（内部密钥需要写入 data/internal-secret.txt）。';
    } elseif ($cliError === '找不到 PHP CLI 可执行文件') {
        $hint = '。可在后台「配置」页填写 PHP CLI 路径，或在 data/php-cli-path.txt 里写入绝对路径。';
    } else {
        $hint = '。请检查 data/ 目录权限与 PHP 错误日志。';
    }

    return [
        'ok'      => false,
        'error'   => implode('；', $parts) . $hint,
        'summary' => [],
        'method'  => 'none',
    ];
}

/**
 * 用 PHP CLI 子进程跑 fetch.php。
 *
 * @return array{ok:bool,error:?string,summary:array}
 */
function rp_admin_run_pull_cli(string $binary, array $args = []): array
{
    $command = escapeshellarg($binary) . ' ' . escapeshellarg(__DIR__ . '/fetch.php') . ' --quiet --json';
    foreach ($args as $arg) {
        $command .= ' ' . escapeshellarg((string) $arg);
    }

    $descriptors = [1 => ['pipe', 'w'], 2 => ['pipe', 'w']];
    $pipes       = [];
    $process     = @proc_open($command, $descriptors, $pipes, __DIR__);

    if (!is_resource($process)) {
        return ['ok' => false, 'error' => '启动子进程失败（可能被 open_basedir 拦截）', 'summary' => []];
    }

    $stdout = stream_get_contents($pipes[1]) ?: '';
    $stderr = stream_get_contents($pipes[2]) ?: '';
    fclose($pipes[1]);
    fclose($pipes[2]);
    $code = proc_close($process);

    $summary = json_decode(trim($stdout), true);
    if (!is_array($summary)) {
        $tail = trim($stderr) !== '' ? trim($stderr) : substr(trim($stdout), 0, 300);

        return ['ok' => false, 'error' => '子进程没有返回可解析的 JSON（退出码 ' . $code . '）：' . $tail, 'summary' => []];
    }

    return ['ok' => true, 'error' => null, 'summary' => $summary];
}

/**
 * 同域 HTTP 自触发 fetch.php。
 *
 * 用本机回环地址，避免绕一圈公网 + CDN；再用 Host 头对齐域名，
 * 让 fetch.php 的 IP 白名单 / token 校验走到和外部一致的上下文。
 *
 * @return array{ok:bool,error:?string,summary:array}
 */
function rp_admin_run_pull_http(array $args = []): array
{
    $cfg = rp_config();
    if (($cfg['cli']['http_fallback'] ?? true) !== true) {
        return ['ok' => false, 'error' => 'HTTP 兜底已在配置里关闭', 'summary' => []];
    }

    $token = (string) ($cfg['web_token'] ?? '');
    $query = ['format' => 'json', 'quiet' => '1', 'internal' => '1'];
    if ($token !== '') {
        $query['token'] = $token;
    }
    // 内部密钥：全新安装上 web_token 是空的，但后台按钮必须能用。
    // 两条都带上，fetch.php 认可其一即可。
    $secret = rp_internal_secret();
    if ($secret !== '') {
        $query['secret'] = $secret;
    }
    if ($token === '' && $secret === '') {
        return [
            'ok'      => false,
            'error'   => '未能生成内部调用密钥（data/ 目录不可写），'
                . '请在「配置」页设置 web_token',
            'summary' => [],
        ];
    }
    foreach ($args as $arg) {
        $arg = (string) $arg;
        if (str_starts_with($arg, '--only=')) {
            $query['only'] = substr($arg, 7);
        } elseif ($arg === '--force') {
            $query['force'] = '1';
        }
    }

    $host = (string) ($_SERVER['HTTP_HOST'] ?? 'localhost');
    $scheme = (!empty($_SERVER['HTTPS']) && $_SERVER['HTTPS'] !== 'off')
        || (($_SERVER['HTTP_X_FORWARDED_PROTO'] ?? '') === 'https') ? 'https' : 'http';

    // 先试回环（不通再试自身域名）
    $targets = [
        $scheme . '://127.0.0.1' . rp_admin_base_path() . '/fetch.php',
        $scheme . '://' . $host . rp_admin_base_path() . '/fetch.php',
    ];

    $lastError = '未知原因';
    foreach ($targets as $url) {
        $full = $url . '?' . http_build_query($query);
        $res  = rp_admin_http_get($full, $host, 300);
        if ($res === null) {
            $lastError = 'HTTP 请求失败：' . $url;
            continue;
        }

        [$status, $body] = $res;
        $summary = json_decode(trim((string) $body), true);
        if (!is_array($summary)) {
            $lastError = 'HTTP ' . $status . ' 返回的不是 JSON：'
                . substr(trim((string) $body), 0, 200);
            continue;
        }

        // fetch.php 被拒时也会返回合法 JSON（ok=false），必须看 ok 字段，
        // 否则会把 403「令牌不匹配」误判成拉取成功。
        if (($summary['ok'] ?? null) !== true) {
            $lastError = 'HTTP ' . $status . '：'
                . (string) ($summary['error'] ?? '返回 ok=false');
            continue;
        }

        return ['ok' => true, 'error' => null, 'summary' => $summary];
    }

    return ['ok' => false, 'error' => $lastError, 'summary' => []];
}

/** 取当前脚本所在的 URL 路径前缀（去掉 admin.php 本身）。 */
function rp_admin_base_path(): string
{
    $script = (string) ($_SERVER['SCRIPT_NAME'] ?? '/admin.php');
    $dir    = rtrim(str_replace('\\', '/', dirname($script)), '/');

    return $dir === '' ? '' : $dir;
}

/**
 * 发起一次带 Host 头的 HTTP GET。
 *
 * @return array{0:int,1:string}|null [HTTP 状态码, 响应体]；失败返回 null。
 *
 * 优先 curl；没有 curl 时退回 allow_url_fopen 的流上下文。
 */
function rp_admin_http_get(string $url, string $host, int $timeout): ?array
{
    if (function_exists('curl_init')) {
        $ch = curl_init($url);
        curl_setopt_array($ch, [
            CURLOPT_RETURNTRANSFER => true,
            CURLOPT_TIMEOUT        => $timeout,
            CURLOPT_CONNECTTIMEOUT => 10,
            CURLOPT_HTTPHEADER     => ['Host: ' . $host],
            CURLOPT_SSL_VERIFYPEER => false,   // 回环自签场景
            CURLOPT_SSL_VERIFYHOST => 0,
            CURLOPT_FOLLOWLOCATION => true,
            CURLOPT_MAXREDIRS      => 3,
        ]);
        $body = curl_exec($ch);
        $err  = curl_errno($ch);
        $code = (int) curl_getinfo($ch, CURLINFO_HTTP_CODE);
        curl_close($ch);

        if ($err !== 0 || !is_string($body)) {
            return null;
        }

        return [$code, $body];
    }

    if (!filter_var(ini_get('allow_url_fopen'), FILTER_VALIDATE_BOOLEAN)) {
        return null;
    }

    $ctx = stream_context_create([
        'http' => [
            'method'        => 'GET',
            'header'        => 'Host: ' . $host . "\r\n",
            'timeout'       => $timeout,
            'ignore_errors' => true,
        ],
        'ssl' => ['verify_peer' => false, 'verify_peer_name' => false],
    ]);
    $body = @file_get_contents($url, false, $ctx);
    if (!is_string($body)) {
        return null;
    }
    $code = 0;
    foreach ($http_response_header ?? [] as $line) {
        if (preg_match('#^HTTP/\S+\s+(\d{3})#', (string) $line, $m)) {
            $code = (int) $m[1];
        }
    }

    return [$code, $body];
}

/**
 * 定位 PHP CLI 二进制。FPM 下 PHP_BINARY 指向 php-fpm，不能直接用。
 *
 * 查找顺序：
 *   1. config['cli']['binary'] 显式配置
 *   2. data/php-cli-path.txt 里写的路径（方便在面板里手填，无需改代码）
 *   3. PHP_BINDIR/php（若确实存在且不是 php-fpm）
 *   4. config['cli']['candidates'] 里的常见路径（{ver} 替换为当前版本）
 *   5. config['cli']['glob_patterns'] 通配符搜索
 *
 * 找到后还会做一次「能不能跑」的验证：执行 `-r 'echo PHP_SAPI;'`，
 * 只有输出 cli 才认。否则可能拿到的是 php-fpm / php-cgi。
 */
function rp_admin_php_cli(): ?string
{
    $cfg = rp_config();
    $cli = is_array($cfg['cli'] ?? null) ? $cfg['cli'] : [];

    // 当前 PHP 版本，如 "8.1"
    $ver = PHP_MAJOR_VERSION . '.' . PHP_MINOR_VERSION;

    $candidates = [];

    // 1) 显式配置
    $explicit = trim((string) ($cli['binary'] ?? ''));
    if ($explicit !== '') {
        if (!rp_admin_path_safe($explicit)) {
            return null;   // 配了但越界，视为无效，避免误导
        }
        $candidates[] = $explicit;
    }

    // 2) 面板里可写的路径文件
    $pathFile = (string) ($cli['path_file'] ?? (__DIR__ . '/data/php-cli-path.txt'));
    if ($pathFile !== '' && @is_file($pathFile)) {
        $fromFile = trim((string) @file_get_contents($pathFile));
        if ($fromFile !== '' && rp_admin_path_safe($fromFile)) {
            $candidates[] = $fromFile;
        }
    }

    // 3) PHP_BINDIR（FPM 下往往指向 php-fpm，后面会用 SAPI 验证剔除）
    if (defined('PHP_BINDIR') && PHP_BINDIR !== '') {
        $candidates[] = PHP_BINDIR . DIRECTORY_SEPARATOR
            . (DIRECTORY_SEPARATOR === '\\' ? 'php.exe' : 'php');
    }

    // 4) 常见路径
    foreach ((array) ($cli['candidates'] ?? []) as $tpl) {
        if (is_string($tpl) && $tpl !== '') {
            $candidates[] = str_replace('{ver}', $ver, $tpl);
        }
    }

    foreach ($candidates as $candidate) {
        $found = rp_admin_resolve_php_binary((string) $candidate);
        if ($found !== null) {
            return $found;
        }
    }

    // 5) glob 兜底（版本从高到低，避免挑到老的 5.x）
    $globs = [];
    foreach ((array) ($cli['glob_patterns'] ?? []) as $pat) {
        if (is_string($pat) && $pat !== '') {
            $globs[] = str_replace('{ver}', $ver, $pat);
        }
    }
    $matches = [];
    foreach ($globs as $pat) {
        if (!rp_admin_path_safe(dirname($pat))) {
            continue;
        }
        $hits = @glob($pat) ?: [];
        foreach ($hits as $hit) {
            $matches[$hit] = true;
        }
    }
    $matches = array_keys($matches);
    // 版本号大的排前面
    usort($matches, static function (string $a, string $b): int {
        preg_match('/(\d+)(?:\.(\d+))?/', $a, $ma);
        preg_match('/(\d+)(?:\.(\d+))?/', $b, $mb);
        $va = ((int) ($ma[1] ?? 0)) * 100 + (int) ($ma[2] ?? 0);
        $vb = ((int) ($mb[1] ?? 0)) * 100 + (int) ($mb[2] ?? 0);

        return $vb <=> $va;
    });

    foreach ($matches as $hit) {
        $found = rp_admin_resolve_php_binary($hit);
        if ($found !== null) {
            return $found;
        }
    }

    return null;
}

/**
 * 校验路径确实存在且能当 CLI 用（SAPI 必须是 cli）。
 * 返回可用路径，否则 null。
 */
function rp_admin_resolve_php_binary(string $path): ?string
{
    if ($path === '' || !@is_file($path) || !@is_executable($path)) {
        return null;
    }
    if (!function_exists('proc_open')) {
        // 不能验证时只能凭「文件存在」接受
        return $path;
    }

    $cmd = escapeshellarg($path) . ' -r ' . escapeshellarg('echo PHP_SAPI;');
    $descriptors = [1 => ['pipe', 'w'], 2 => ['pipe', 'w']];
    $pipes = [];
    $process = @proc_open($cmd, $descriptors, $pipes, __DIR__);
    if (!is_resource($process)) {
        return null;
    }
    $out = trim(stream_get_contents($pipes[1]) ?: '');
    fclose($pipes[1]);
    fclose($pipes[2]);
    @proc_close($process);

    return $out === 'cli' ? $path : null;
}

/**
 * 防目录穿越 / 防误配置到奇怪位置。
 * 仅允许绝对路径，且不含 .. 片段。
 */
function rp_admin_path_safe(string $path): bool
{
    if ($path === '') {
        return false;
    }
    $norm = str_replace('\\', '/', $path);
    if (str_contains($norm, '..')) {
        return false;
    }

    return $norm[0] === '/'
        || (bool) preg_match('#^[A-Za-z]:/#', $norm);   // Windows
}

// ======================================================================
// 数据读取
// ======================================================================

function rp_admin_load_data(array $cfg): array
{
    $outDir = $cfg['paths']['out'];

    $logs   = glob($cfg['paths']['logs'] . '/*.log') ?: [];
    usort($logs, static fn(string $a, string $b): int => (@filemtime($b) ?: 0) <=> (@filemtime($a) ?: 0));

    $archives = glob($cfg['paths']['archive'] . '/*', GLOB_ONLYDIR) ?: [];
    rsort($archives);

    return [
        'manifest' => Store::readJson($outDir . '/manifest.json'),
        'stats'    => Store::readJson($outDir . '/stats.json'),
        'state'    => Store::readJson($cfg['paths']['state']),
        'settings' => rp_settings_store()->overrides(),
        'defaults' => rp_admin_defaults(),
        'logs'     => $logs,
        'archives' => $archives,
        'out_dir'  => $outDir,
    ];
}

/**
 * config.php 的原始值（未经 data/settings.json 覆盖），用于「当前值 vs 默认值」对比。
 */
function rp_admin_defaults(): array
{
    static $defaults = null;
    if ($defaults === null) {
        /** @var array $loaded */
        $loaded   = require __DIR__ . '/config.php';
        $defaults = $loaded;
    }

    return $defaults;
}

function rp_admin_value(array $cfg, string $path): mixed
{
    $cursor = $cfg;
    foreach (explode('.', $path) as $part) {
        if (!is_array($cursor) || !array_key_exists($part, $cursor)) {
            return null;
        }
        $cursor = $cursor[$part];
    }

    return $cursor;
}

function rp_admin_format(mixed $value): string
{
    if (is_bool($value)) {
        return $value ? '开' : '关';
    }
    if (is_array($value)) {
        return $value === [] ? '（空）' : implode(', ', array_map('strval', $value));
    }
    if ($value === null) {
        return '（未设置）';
    }

    return (string) $value;
}

// ======================================================================
// 各标签页
// ======================================================================

function rp_admin_tab_overview(WebApp $app, array $data): string
{
    $manifest = $data['manifest'];
    $stats    = $data['stats'];
    $state    = (array) ($data['state'] ?? []);

    if ($manifest === null) {
        return View::alert('info', '还没有任何产物。到「操作」页点一次「立即拉取」，或先在命令行跑 php fetch.php。');
    }

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
        $stale = !empty($source['stale']);
        $bytes = (int) ($source['bytes'] ?? 0);
        if ($stale) {
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

    // ---------------------------------------------------------- 合并情况
    $merge = '<section class="block"><h2>两源合并</h2><div class="card card--table"><div class="table-wrap"><table class="table">'
        . '<thead><tr><th>口径</th><th class="num">数量</th><th>说明</th></tr></thead><tbody>'
        . '<tr><td>唯一域名</td><td class="num">' . (int) ($m['domains'] ?? 0) . '</td><td class="muted">两源去重后的域名总数</td></tr>'
        . '<tr><td>仅 UsbEAm 有</td><td class="num">' . (int) ($m['only_usbeam'] ?? 0) . '</td><td class="muted">只有域名 → 地址的映射</td></tr>'
        . '<tr><td>仅 S302 有</td><td class="num">' . (int) ($m['only_s302'] ?? 0) . '</td><td class="muted">只有域名 → 替代上游的映射</td></tr>'
        . '<tr><td>两源共有</td><td class="num">' . (int) ($m['both'] ?? 0) . '</td><td class="muted">既有地址又有上游</td></tr>'
        . '<tr><td>S302 服务段 / 通配符</td><td class="num">' . (int) ($s['services'] ?? 0) . ' / ' . (int) ($s['wildcards'] ?? 0) . '</td><td class="muted">来自 S302_rules.ini</td></tr>'
        . '</tbody></table></div></div></section>';

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
        . '<p class="field__help" style="margin-top:10px">产物目录：<code>' . View::e((string) $data['out_dir']) . '</code></p></section>';

    // ---------------------------------------------------------- 中转站
    // 概览页放一条入口，省得用户找不到「中转站」标签页。
    $share = (array) ($cfg['share'] ?? []);
    $shareOn = !empty($share['enabled']);
    $shareHost = (string) ($_SERVER['HTTP_HOST'] ?? '');
    $shareInfo = '<section class="block"><h2>中转站</h2><div class="card">'
        . '<div class="row row--between">'
        . '<span>' . ($shareOn
            ? '已开启 · 规则正对外发布'
            : '未开启 · 规则只在本机使用') . '</span>'
        . ($shareOn ? View::badge('ok', '运行中') : View::badge('warn', '已关闭'))
        . '</div>';

    if ($shareOn && $shareHost !== '') {
        $shareInfo .= '<div class="field" style="margin-top:10px">'
            . '<label class="field__label">对外地址</label>'
            . '<input class="input" type="text" readonly onclick="this.select()" '
            . 'value="http://' . View::e($shareHost . '/' . (string) ($share['usbeam_path'] ?? '1')) . '" '
            . 'style="font-family:var(--font-mono)">'
            . '<div class="field__help">另一条：<code>/' . View::e((string) ($share['s302_path'] ?? '2'))
            . '</code>（Steamcommunity 302 规则）</div></div>';
    } else {
        $shareInfo .= '<p class="field__help" style="margin-top:6px">'
            . '开启后可以把聚合好的规则发布成固定短网址，供别人或别的客户端订阅。</p>';
    }

    $shareInfo .= '<div class="row"><a class="btn btn--secondary" href="?tab=share">'
        . View::icon('link') . '<span>去中转站设置</span></a></div>'
        . '</div></section>';

    // ---------------------------------------------------------- 警告
    $warnings = '';
    if (!empty($manifest['warnings'])) {
        $items = '';
        foreach ($manifest['warnings'] as $warning) {
            $items .= View::alert('warn', (string) $warning);
        }
        $warnings = '<section class="block"><h2>警告</h2><div class="stack">' . $items . '</div></section>';
    }

    return $metrics . $sources . $merge . $files . $shareInfo . $warnings;
}

function rp_admin_tab_actions(WebApp $app, array $data): string
{
    $cfg    = $app->cfg();
    $csrf   = $app->auth()->csrfField();
    $limits = (array) ($cfg['limits'] ?? []);

    $gate = $app->limiter()->stats('web_trigger', (int) ($limits['web_min_interval'] ?? 0));
    $last = $gate['last_at'] !== null ? date('Y-m-d H:i:s', (int) $gate['last_at']) : '从未';

    $throttleInfo = '<section class="block"><h2>触发配额</h2><div class="card">'
        . '<table class="table"><tbody>'
        . '<tr><td style="width:220px">最近 24 小时网页触发</td><td class="num">' . (int) $gate['used_24h']
        . ' / ' . ((int) ($limits['web_daily_max'] ?? 0) ?: '不限') . '</td></tr>'
        . '<tr><td>上次网页触发</td><td class="num">' . View::e($last) . '</td></tr>'
        . '<tr><td>距下次可触发</td><td class="num">' . (int) $gate['next_allowed_in'] . ' 秒</td></tr>'
        . '<tr><td>单次运行时间上限</td><td class="num">' . (int) ($limits['max_runtime_seconds'] ?? 0) . ' 秒</td></tr>'
        . '<tr><td>hosts 行数上限</td><td class="num">' . ((int) ($limits['max_hosts_lines'] ?? 0) ?: '不限') . '</td></tr>'
        . '</tbody></table></div></section>';

    $form = static function (string $action, string $label, string $class, string $icon, bool $confirm, string $hint = '') use ($csrf): string {
        $onclick = $confirm
            ? ' onclick="return confirm(\'' . View::e('确认执行「' . $label . '」？此操作不可撤销。') . '\')"'
            : '';

        return '<form method="post" action="" style="display:inline">' . $csrf
            . '<input type="hidden" name="action" value="' . View::e($action) . '">'
            . '<button class="btn ' . $class . '" type="submit"' . $onclick . '>'
            . View::icon($icon) . '<span>' . View::e($label) . '</span></button></form>'
            . ($hint !== '' ? '<span class="muted" style="font-size:var(--fs-xs)">' . View::e($hint) . '</span>' : '');
    };

    $pull = '<section class="block"><h2>拉取</h2><div class="card">'
        . '<div class="row">'
        . $form('pull', '立即拉取', 'btn--primary', 'play', false)
        . $form('pull_force', '强制重新下载', 'btn--secondary', 'refresh', false, '忽略 ETag / Last-Modified')
        . '</div>'
        . '<p class="field__help" style="margin-top:12px">拉取在独立的 PHP CLI 子进程中执行。'
        . '如果同一时刻定时任务也在跑，会拿到单实例锁并直接跳过，不会互相踩。</p>'
        . '</div></section>';

    $maintenance = '<section class="block"><h2>维护</h2><div class="card">'
        . '<div class="row">'
        . $form('clear_cache', '清理解析缓存', 'btn--secondary', 'trash', true, '原始文件保留，下次重新解析')
        . $form('reset_throttle', '重置限流计数', 'btn--secondary', 'refresh', false, '解除网页触发与登录的节流')
        . $form('prune', '清理过期文件', 'btn--secondary', 'trash', true, '按保留天数删除快照 / 日志 / 审计')
        . '</div></div></section>';

    $archives = '';
    if (!empty($data['archives'])) {
        $rows = '';
        foreach (array_slice($data['archives'], 0, 14) as $dir) {
            $manifest = Store::readJson($dir . '/manifest.json');
            $rows .= '<tr><td>' . View::e(basename($dir)) . '</td>'
                . '<td>' . (($manifest['exit'] ?? 'ok') === 'ok' ? View::badge('ok', '正常') : View::badge('warn', '异常')) . '</td>'
                . '<td class="num">' . (int) ($manifest['stats']['merged']['domains'] ?? 0) . '</td>'
                . '<td class="num">' . (int) ($manifest['duration_ms'] ?? 0) . ' ms</td></tr>';
        }
        $archives = '<section class="block"><h2>运行历史（每日快照）</h2><div class="card card--table"><div class="table-wrap">'
            . '<table class="table"><thead><tr><th>日期</th><th>状态</th><th class="num">聚合域名</th><th class="num">耗时</th></tr></thead>'
            . '<tbody>' . $rows . '</tbody></table></div></div></section>';
    }

    return $pull . $maintenance . $throttleInfo . $archives;
}

/**
 * 中转站：把规则对外发布成固定短网址。
 *
 * 这一页的重点是**把可复制的地址摆在最显眼处** —— 用户来这里
 * 就是为了拿到「该发哪个链接给别人」。配置项本身在「配置」页。
 */
function rp_admin_tab_share(WebApp $app, array $data): string
{
    $cfg   = $app->cfg();
    $csrf  = $app->auth()->csrfField();
    $share = (array) ($cfg['share'] ?? []);
    $out   = rtrim((string) ($cfg['paths']['out'] ?? (__DIR__ . '/data/out')), '/');

    $enabled = !empty($share['enabled']);
    $host    = (string) ($_SERVER['HTTP_HOST'] ?? '你的域名');
    $https   = (!empty($_SERVER['HTTPS']) && $_SERVER['HTTPS'] !== 'off')
        || (($_SERVER['HTTP_X_FORWARDED_PROTO'] ?? '') === 'https');
    $origin  = ($https ? 'https' : 'http') . '://' . $host;

    $usbeamPath = (string) ($share['usbeam_path'] ?? '1');
    $s302Path   = (string) ($share['s302_path'] ?? '2');

    // 产物是否就绪 —— 没产物的话地址能用但会返回 503
    $manifest = Store::readJson($out . '/manifest.json');
    $generated = (string) ($manifest['generated_at'] ?? '');
    $ready = $generated !== '';

    // ---------------------------------------------------------- 状态卡
    $status = $enabled
        ? View::alert('info', $ready
            ? '中转站已开启。下面的地址可以直接分享，任何人无需登录即可获取规则。'
            : '中转站已开启，但还没有产物 —— 地址暂时返回「规则还没生成」。先去「操作」页点一次「立即拉取」。')
        : View::alert('warn', '中转站当前**关闭**，下面的地址一律返回 404。'
            . '到「配置」页把「对外发布规则（中转站）」打开即可。');

    // ---------------------------------------------------------- 地址卡
    $endpoints = [
        [
            'label' => 'UsbEAm Hosts 规则',
            'desc'  => 'UsbEAm Hosts Editor 的 hosts 记录（域名 → 可用 IP）',
            'path'  => $usbeamPath,
            'file'  => 'hosts.txt',
            'key'   => 'usbeam',
        ],
        [
            'label' => 'Steamcommunity 302 规则',
            'desc'  => 'Steamcommunity 302 的劫持域名（指向本地反代监听地址）',
            'path'  => $s302Path,
            'file'  => 'hosts_s302.txt',
            'key'   => 's302',
        ],
    ];

    $cards = '';
    foreach ($endpoints as $ep) {
        $short = $origin . '/' . $ep['path'];
        $plain = $origin . rp_admin_base_path() . '/r.php?p=' . $ep['key'];

        $size = 0;
        $f = $out . '/' . $ep['file'];
        if (is_file($f)) {
            $size = (int) (@filesize($f) ?: 0);
        }

        $cards .= '<div class="card">'
            . '<div class="row row--between"><h3 style="margin:0">' . View::e($ep['label']) . '</h3>'
            . ($size > 0 ? View::badge('ok', View::humanBytes($size)) : View::badge('warn', '无产物'))
            . '</div>'
            . '<p class="field__help" style="margin-top:6px">' . View::e($ep['desc']) . '</p>'

            . '<div class="field"><label class="field__label">短地址（推荐分享这个）</label>'
            . '<input class="input" type="text" readonly value="' . View::e($short) . '" '
            . 'onclick="this.select()" style="font-family:var(--font-mono)">'
            . '<div class="field__help">需要 <code>.htaccess</code> 重写生效（Apache 默认支持）。'
            . '如果打开是 404，改用下面那个。</div></div>'

            . '<div class="field"><label class="field__label">通用地址（任何环境都能用）</label>'
            . '<input class="input" type="text" readonly value="' . View::e($plain) . '" '
            . 'onclick="this.select()" style="font-family:var(--font-mono)">'
            . '<div class="field__help">不依赖重写规则，Nginx / 共享主机都可用。</div></div>'

            . '<div class="field"><label class="field__label">JSON 版本</label>'
            . '<input class="input" type="text" readonly value="' . View::e($short . '?format=json') . '" '
            . 'onclick="this.select()" style="font-family:var(--font-mono)">'
            . '<div class="field__help">结构化数据，适合程序直接消费。'
            . (!empty($share['allow_json']) ? '' : '<strong>注意：JSON 输出当前是关闭的。</strong>')
            . '</div></div>'

            . '<div class="row">'
            . '<a class="btn btn--secondary" href="' . View::e($plain) . '" target="_blank" rel="noopener">'
            . View::icon('eye') . '<span>预览</span></a>'
            . '<a class="btn btn--secondary" href="' . View::e($plain . '&format=json') . '" target="_blank" rel="noopener">'
            . View::icon('eye') . '<span>预览 JSON</span></a>'
            . '</div>'
            . '</div>';
    }

    $endpointSection = '<section class="block"><h2>对外地址</h2><div class="stack">' . $cards . '</div></section>';

    // ---------------------------------------------------------- 路径设置
    // 就地把路径做成一键表单，省得用户再去「配置」页翻。
    $pathForm = '<section class="block"><h2>地址路径</h2><div class="card">'
        . '<p class="field__help" style="margin-top:0">改完点保存，上面的地址会自动更新。'
        . '路径只能用字母、数字、下划线、连字符、点，且不能和程序自己的文件重名。</p>'
        . '<form method="post" action="">' . $csrf
        . '<input type="hidden" name="action" value="save_config">'
        . '<input type="hidden" name="_form" value="config">'
        . '<div class="field"><label class="field__label">UsbEAm 规则路径</label>'
        . '<input class="input" type="text" name="cfg_share__usbeam_path" value="' . View::e($usbeamPath) . '" '
        . 'style="font-family:var(--font-mono)"><div class="field__help">默认 <code>1</code></div></div>'
        . '<div class="field"><label class="field__label">S302 规则路径</label>'
        . '<input class="input" type="text" name="cfg_share__s302_path" value="' . View::e($s302Path) . '" '
        . 'style="font-family:var(--font-mono)"><div class="field__help">默认 <code>2</code></div></div>'
        // 开关必须带 __present，否则「没勾」会被当成关闭
        . '<label class="switch-row"><input type="hidden" name="cfg_share__enabled__present" value="1">'
        . '<input type="checkbox" name="cfg_share__enabled" value="1"' . ($enabled ? ' checked' : '') . '>'
        . '<span>对外发布规则（中转站总开关）</span></label>'
        . '<div class="row"><button class="btn btn--primary" type="submit">' . View::icon('check')
        . '<span>保存</span></button></div>'
        . '</form></div></section>';

    // ---------------------------------------------------------- 伪静态提示
    $nginx = 'location ~ ^/(1|2)$ {' . "\n"
        . '    rewrite ^/(1)$ ' . rp_admin_base_path() . '/r.php?p=usbeam last;' . "\n"
        . '    rewrite ^/(2)$ ' . rp_admin_base_path() . '/r.php?p=s302 last;' . "\n"
        . '}';

    $rewrite = '<section class="block"><h2>短地址没生效？</h2><div class="card">'
        . '<p class="field__help" style="margin-top:0">短地址 <code>/' . View::e($usbeamPath)
        . '</code> 依赖服务器的重写规则。三种情况：</p>'
        . '<ol style="margin:8px 0 0 20px;padding:0;line-height:1.9">'
        . '<li><strong>Apache</strong>：包里的 <code>.htaccess</code> 已配好，直接就能用。</li>'
        . '<li><strong>宝塔/面板有「伪静态」</strong>：选 Nginx，把下面这段粘进去。</li>'
        . '<li><strong>不想折腾</strong>：直接用上面的「通用地址」，功能完全一样，只是长一点。</li>'
        . '</ol>'
        . '<div class="field" style="margin-top:12px"><label class="field__label">Nginx 伪静态片段</label>'
        . '<textarea class="textarea" rows="5" readonly onclick="this.select()">'
        . View::e($nginx) . '</textarea></div>'
        . '</div></section>';

    return $status . $endpointSection . $pathForm . $rewrite;
}

function rp_admin_tab_config(WebApp $app, array $data): string
{
    $cfg      = $app->cfg();
    $csrf     = $app->auth()->csrfField();
    $defaults = (array) $data['defaults'];
    $editable = ConfigStore::editable();

    $groups = [];
    foreach ($editable as $key => $spec) {
        $groups[(string) $spec['group']][] = ['key' => $key, 'spec' => $spec];
    }

    $body = '';
    foreach ($groups as $group => $items) {
        $fields = '';
        foreach ($items as $item) {
            $key   = $item['key'];
            $spec  = $item['spec'];
            $name  = ConfigStore::fieldName($key);
            $value = rp_admin_value($cfg, $key);
            $def   = rp_admin_value($defaults, $key);
            $changed = $value !== $def;

            $control = match ($spec['type']) {
                // __present 是「这个开关确实出现在表单里」的标记，缺了它 ConfigStore 视为不改动
                'bool' => '<label class="switch"><input type="hidden" name="' . View::e($name) . '__present" value="1">'
                    . '<input type="checkbox" name="' . View::e($name) . '" value="1"'
                    . (!empty($value) ? ' checked' : '') . '><span class="switch__track"></span></label>',
                'int' => '<input class="input input--num" type="number" name="' . View::e($name) . '" value="'
                    . View::e((string) $value) . '"'
                    . (isset($spec['min']) ? ' min="' . (int) $spec['min'] . '"' : '')
                    . (isset($spec['max']) ? ' max="' . (int) $spec['max'] . '"' : '') . '>',
                'lines', 'url_lines' => '<textarea class="textarea" name="' . View::e($name) . '" rows="4">'
                    . View::e(implode("\n", (array) $value)) . '</textarea>',
                default => '<input class="input" type="text" name="' . View::e($name) . '" value="' . View::e((string) $value) . '">',
            };

            $meta = '<div class="field__meta">当前值 <span class="' . ($changed ? 'diff' : 'cur') . '">'
                . View::e(rp_admin_format($value)) . '</span> · 默认值 <span class="def">' . View::e(rp_admin_format($def)) . '</span>'
                . ($changed ? ' · 已被后台覆盖' : '') . '</div>';

            $fields .= '<div class="field"><label class="field__label" for="' . View::e($name) . '">'
                . View::e((string) $spec['label']) . '</label>' . $control
                . (!empty($spec['help']) ? '<div class="field__help">' . View::e((string) $spec['help']) . '</div>' : '')
                . $meta . '<div class="field__help"><code>' . View::e($key) . '</code></div></div>';
        }

        $body .= '<section class="block"><h2>' . View::e($group) . '</h2><div class="card">' . $fields . '</div></section>';
    }

    // 保存栏放在表单首尾各一个：表单有 30+ 项，只在底部会给「改完顶部几项就想保存」添麻烦
    $bar = static function (string $variant) use ($csrf): string {
        return '<div class="card"><div class="row">'
            . '<button class="btn btn--' . $variant . '" type="submit">' . View::icon('check')
            . '<span>保存配置</span></button>'
            . '<span class="muted" style="font-size:var(--fs-xs)">保存的是覆盖值，写在 data/settings.json，不会改动 config.php</span>'
            . '</div></div>';
    };

    $saveForm = '<form method="post" action="">' . $csrf
        . '<input type="hidden" name="action" value="save_config">'
        // 完整性标记：缺了它 ConfigStore 会拒绝保存，避免不完整的请求把开关全关掉
        . '<input type="hidden" name="_form" value="config">'
        . $bar('primary')
        . $body
        . $bar('secondary')
        . '</form>';

    $resetForm = '<section class="block"><h2>危险操作</h2><div class="card"><div class="row">'
        . '<form method="post" action="" onsubmit="return confirm(\'确认清除后台改过的全部配置？\')">'
        . $csrf
        . '<input type="hidden" name="action" value="reset_config">'
        . '<button class="btn btn--danger" type="submit">' . View::icon('trash') . '<span>恢复默认</span></button>'
        . '</form>'
        . '<span class="muted" style="font-size:var(--fs-xs)">清除 data/settings.json 里的全部覆盖值，回到 config.php 的默认（含管理员密码，之后需要重新 init）</span>'
        . '</div></div></section>';

    $overrides    = (array) $data['settings'];
    $overrideNote = $overrides === []
        ? View::alert('info', '当前没有任何后台覆盖，全部使用 config.php 里的默认值。')
        : View::alert('warn', '有 ' . count(rp_admin_flat_keys($overrides)) . ' 个配置项被后台覆盖（表中标为「已被后台覆盖」）。');

    return $overrideNote . '<div class="sp-4"></div>' . $saveForm . $resetForm;
}

/**
 * 统计覆盖项个数（拍平嵌套键）。
 */
function rp_admin_flat_keys(array $data, string $prefix = ''): array
{
    $out = [];
    foreach ($data as $key => $value) {
        $path = $prefix === '' ? (string) $key : $prefix . '.' . $key;
        if (is_array($value) && $value !== [] && array_keys($value) !== range(0, count($value) - 1)) {
            $out = array_merge($out, rp_admin_flat_keys($value, $path));
            continue;
        }
        $out[] = $path;
    }

    return $out;
}

function rp_admin_tab_logs(WebApp $app, array $data): string
{
    $files = (array) $data['logs'];
    if ($files === []) {
        return View::alert('info', '还没有日志文件。');
    }

    $selected = (string) ($_GET['file'] ?? basename((string) $files[0]));
    $selected = basename($selected);

    $allowed = array_map('basename', $files);
    if (!in_array($selected, $allowed, true)) {
        $selected = basename((string) $files[0]);
    }

    $path  = dirname((string) $files[0]) . '/' . $selected;
    $lines = (int) ($_GET['lines'] ?? 400);
    $lines = max(50, min(2000, $lines));

    $content = Store::read($path);
    $rows    = $content === null ? [] : (preg_split('/\r\n|\r|\n/', trim($content)) ?: []);
    $rows    = array_slice($rows, -$lines);

    $logHtml = '';
    foreach ($rows as $row) {
        $class = 'log--info';
        if (strpos($row, '] ERROR') !== false) {
            $class = 'log--error';
        } elseif (strpos($row, '] WARN') !== false) {
            $class = 'log--warn';
        } elseif (strpos($row, '] DEBUG') !== false) {
            $class = 'log--debug';
        }
        $logHtml .= '<span class="log__line ' . $class . '">' . View::e($row) . '</span>';
    }
    if ($logHtml === '') {
        $logHtml = '<span class="log__line log--debug">（空文件）</span>';
    }

    $options = '';
    foreach ($files as $file) {
        $name = basename($file);
        $options .= '<option value="' . View::e($name) . '"' . ($name === $selected ? ' selected' : '') . '>'
            . View::e($name) . ' (' . View::e(View::humanBytes((int) (@filesize($file) ?: 0))) . ')</option>';
    }

    $form = '<form method="get" action="" class="row">'
        . '<input type="hidden" name="tab" value="logs">'
        . '<select class="input" name="file" style="max-width:420px" onchange="this.form.submit()">' . $options . '</select>'
        . '<input class="input input--num" type="number" name="lines" value="' . $lines . '" min="50" max="2000">'
        . '<button class="btn btn--secondary" type="submit">显示</button>'
        . '</form>';

    return '<section class="block"><h2>日志文件</h2>' . $form . '</section>'
        . '<section class="block"><h2>末尾 ' . count($rows) . ' 行</h2><div class="log">' . $logHtml . '</div></section>';
}

function rp_admin_tab_audit(WebApp $app, array $data): string
{
    $rows = $app->audit()->recent(200);

    if ($rows === []) {
        return View::alert('info', '还没有审计记录。登录、改配置、触发拉取等操作都会记在这里。');
    }

    $body = '';
    foreach ($rows as $row) {
        $detail = $row['detail'] ?? null;
        $text   = is_array($detail) && $detail !== []
            ? (string) json_encode($detail, JSON_UNESCAPED_UNICODE | JSON_UNESCAPED_SLASHES)
            : '';

        $body .= '<tr>'
            . '<td class="nowrap">' . View::e((string) ($row['at'] ?? '')) . '</td>'
            . '<td class="mono">' . View::e((string) ($row['ip'] ?? '')) . '</td>'
            . '<td><code>' . View::e((string) ($row['action'] ?? '')) . '</code></td>'
            . '<td class="mono">' . View::e($text) . '</td>'
            . '</tr>';
    }

    return '<section class="block"><h2>最近 200 条操作</h2><div class="card card--table"><div class="table-wrap">'
        . '<table class="table"><thead><tr><th>时间</th><th>来源 IP</th><th>动作</th><th>详情</th></tr></thead>'
        . '<tbody>' . $body . '</tbody></table></div></div>'
        . '<p class="field__help" style="margin-top:10px">审计文件：<code>data/logs/audit-YYYY-MM.jsonl</code></p></section>';
}

function rp_admin_tab_security(WebApp $app, array $data): string
{
    $cfg  = $app->cfg();
    $auth = $app->auth();
    $csrf = $auth->csrfField();

    // ---------------------------------------------------------- 当前状态
    $allow = (array) ($cfg['security']['ip_allowlist'] ?? []);
    $quota = $app->ipGuard()->usage();

    $quotaText = $quota['exempt']
        ? View::badge('muted', '已豁免') . ' <span class="muted">来源在 limits.ip_rate_limit_exempt 名单里</span>'
        : $quota['used'] . ' / ' . ($quota['max'] > 0 ? $quota['max'] : '不限') . ' 次每分钟';

    $status = '<section class="block"><h2>当前防护状态</h2><div class="card"><table class="table"><tbody>'
        . '<tr><td style="width:260px">当前来源 IP</td><td><code>' . View::e($auth->clientIp()) . '</code>'
        . ($auth->isLocalRequest() ? ' ' . View::badge('info', '本机') : '') . '</td></tr>'
        . '<tr><td>本分钟访问额度</td><td>' . $quotaText . '</td></tr>'
        . '<tr><td>已登录管理员额度</td><td>' . ((int) $quota['admin_max'] > 0 ? (int) $quota['admin_max'] . ' 次每分钟' : '不限') . '</td></tr>'
        . '<tr><td>IP 白名单</td><td>' . ($allow === []
            ? View::badge('warn', '未设置（任何人可访问登录页）')
            : View::badge('ok', count($allow) . ' 条规则') . ' <span class="mono">' . View::e(implode(' · ', array_map('strval', $allow))) . '</span>')
        . '</td></tr>'
        . '<tr><td>网页触发</td><td>' . (!empty($cfg['security']['allow_web_trigger'])
            ? View::badge('ok', '已开启')
            : View::badge('muted', '已关闭'))
        . '</td></tr>'
        . '<tr><td>网页触发令牌</td><td>' . ((string) ($cfg['web_token'] ?? '') !== ''
            ? View::badge('ok', '已设置')
            : View::badge('warn', '未设置（网页触发不可用）'))
        . '</td></tr>'
        . '<tr><td>status.php 保护</td><td>' . (!empty($cfg['security']['protect_status'])
            ? View::badge('ok', '需要登录')
            : View::badge('warn', '公开可读'))
        . '</td></tr>'
        . '<tr><td>登录失败计数</td><td>' . $auth->failureCount() . ' 次'
        . ($auth->lockRemaining() > 0 ? ' · ' . View::badge('error', '已锁定 ' . Auth::humanDuration($auth->lockRemaining())) : '')
        . '</td></tr>'
        . '<tr><td>会话有效期</td><td>' . Auth::humanDuration((int) ($cfg['security']['session_lifetime'] ?? 7200)) . '</td></tr>'
        . '</tbody></table></div></section>';

    // ---------------------------------------------------------- 改密码
    $password = '<section class="block"><h2>修改密码</h2><div class="card">'
        . '<form method="post" action="" style="max-width:420px">' . $csrf
        . '<input type="hidden" name="action" value="change_password">'
        . '<div class="field"><label class="field__label">当前密码</label>'
        . '<input class="input" type="password" name="current_password" autocomplete="current-password" required></div>'
        . '<div class="field"><label class="field__label">新密码</label>'
        . '<input class="input" type="password" name="new_password" autocomplete="new-password" minlength="8" required>'
        . '<div class="field__help">至少 8 位。存储的是 password_hash() 结果，不可逆。</div></div>'
        . '<div class="field"><label class="field__label">再输一次</label>'
        . '<input class="input" type="password" name="new_password2" autocomplete="new-password" required></div>'
        . '<button class="btn btn--primary" type="submit">' . View::icon('key') . '<span>更新密码</span></button>'
        . '</form></div></section>';

    // ---------------------------------------------------------- 令牌
    $tokenValue = (string) ($cfg['web_token'] ?? '');
    $token = '<section class="block"><h2>网页触发令牌</h2><div class="card">'
        . '<p class="field__help" style="margin-top:0">后台的「立即拉取」按钮**不需要**这个令牌，'
        . '开箱即用。这里配的令牌是给<strong>外部调用</strong>用的：'
        . '定时任务、监控脚本、别的机器触发的场景。</p>'
        . '<form method="post" action="">' . $csrf
        . '<input type="hidden" name="action" value="set_token">'
        . '<div class="field"><label class="field__label">web_token</label>'
        . '<input class="input" type="text" name="web_token" value="' . View::e($tokenValue) . '" '
        . 'placeholder="留空 = 仅后台按钮 / 命令行 / 定时任务可用" style="font-family:var(--font-mono)">'
        . '<div class="field__help">至少 16 位。触发地址形如 '
        . '<code>fetch.php?token=&lt;web_token&gt;</code>。留空不影响后台按钮和定时任务。</div></div>'
        . '<div class="row"><button class="btn btn--primary" type="submit">' . View::icon('check') . '<span>保存令牌</span></button>'
        . '<button class="btn btn--secondary" type="button" onclick="rpGenToken()">生成随机令牌</button></div>'
        . '</form></div></section>'
        . '<script>function rpGenToken(){var a=new Uint8Array(24);(window.crypto||window.msCrypto).getRandomValues(a);'
        . 'var s=Array.prototype.map.call(a,function(b){return ("0"+b.toString(16)).slice(-2)}).join("");'
        . 'document.querySelector(\'input[name="web_token"]\').value=s;}</script>';

    // ---------------------------------------------------------- 解锁
    $unlock = '<section class="block"><h2>应急</h2><div class="card"><div class="row">'
        . '<form method="post" action="" onsubmit="return confirm(\'确认清空登录失败计数？\')">' . $csrf
        . '<input type="hidden" name="action" value="unlock_login">'
        . '<button class="btn btn--secondary" type="submit">' . View::icon('lock') . '<span>解除登录锁定</span></button>'
        . '</form>'
        . '<span class="muted" style="font-size:var(--fs-xs)">忘记密码时，在服务器上执行 '
        . '<code>php admin-cli.php reset-password</code></span>'
        . '</div></div></section>';

    $hint = View::alert('info',
        'IP 白名单、会话有效期、锁定阈值等都在「配置」页的「安全」分组里修改。'
        . '白名单如果填了却不包含当前 IP，保存会被拒绝 —— 避免把自己锁在门外。');

    return $hint . '<div class="sp-4"></div>' . $status . $password . $token . $unlock;
}
