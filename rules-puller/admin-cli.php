<?php

declare(strict_types=1);

/**
 * 管理命令行：没有浏览器、或者被自己的白名单锁在门外时用这个。
 *
 *   php admin-cli.php status                     查看当前安全配置
 *   php admin-cli.php init --password=xxxx       设置管理员密码（首次）
 *   php admin-cli.php init                       从标准输入读密码（可 echo 管道进来）
 *   php admin-cli.php token                      生成并写入一个新的 web_token
 *   php admin-cli.php unlock                     清空登录失败计数、解除锁定
 *   php admin-cli.php allow 1.2.3.4 [10.0.0.0/8] 设置 IP 白名单
 *   php admin-cli.php allow --clear              清空白名单（不限制来源）
 *   php admin-cli.php reset                      清除后台改过的全部配置
 */

use RulesPuller\Audit;
use RulesPuller\Auth;
use RulesPuller\ConfigStore;
use RulesPuller\Logger;
use RulesPuller\RateLimit;
use RulesPuller\Store;

require __DIR__ . '/bootstrap.php';

if (PHP_SAPI !== 'cli') {
    http_response_code(403);
    exit('本脚本只能在命令行运行');
}

$cfg = rp_prepare_dirs(rp_config());
date_default_timezone_set((string) ($cfg['timezone'] ?? 'UTC'));

$log     = new Logger($cfg['paths']['logs'] . '/cli-' . date('Y-m-d') . '.log', false);
$auth    = new Auth($cfg, $log);
$audit   = new Audit($cfg['paths']['logs']);
$store   = rp_settings_store();
$command = (string) ($argv[1] ?? 'status');

/**
 * 取选项值：--name=value。
 */
function rp_cli_option(array $argv, string $name): ?string
{
    foreach ($argv as $arg) {
        if (strncmp((string) $arg, '--' . $name . '=', strlen($name) + 3) === 0) {
            return substr((string) $arg, strlen($name) + 3);
        }
    }

    return null;
}

function rp_cli_read_password(): string
{
    // 管道进来的优先（便于自动化），否则提示交互输入
    if (function_exists('posix_isatty') && !@posix_isatty(STDIN)) {
        return trim((string) stream_get_contents(STDIN));
    }

    @fwrite(STDOUT, '请输入新密码（至少 8 位，输入不回显）：');
    if (function_exists('shell_exec') && DIRECTORY_SEPARATOR !== '\\') {
        @shell_exec('stty -echo');
        $password = trim((string) fgets(STDIN));
        @shell_exec('stty echo');
        @fwrite(STDOUT, PHP_EOL);
    } else {
        $password = trim((string) fgets(STDIN));
    }

    return $password;
}

switch ($command) {
    // ---------------------------------------------------------------- status
    case 'status':
        $allow    = (array) ($cfg['security']['ip_allowlist'] ?? []);
        $lockLeft = $auth->lockRemaining();

        @fwrite(STDOUT, "rules-puller 安全配置\n");
        @fwrite(STDOUT, str_repeat('-', 52) . "\n");
        @fwrite(STDOUT, '管理员密码        : ' . ($auth->isConfigured() ? '已设置' : '未设置（请执行 init）') . "\n");
        @fwrite(STDOUT, 'IP 白名单         : ' . ($allow === [] ? '未设置（不限制）' : implode(', ', array_map('strval', $allow))) . "\n");
        @fwrite(STDOUT, '网页触发          : ' . (!empty($cfg['security']['allow_web_trigger']) ? '已开启' : '已关闭') . "\n");
        @fwrite(STDOUT, 'web_token         : ' . ((string) ($cfg['web_token'] ?? '') !== '' ? '已设置（' . strlen((string) $cfg['web_token']) . ' 位）' : '未设置') . "\n");
        @fwrite(STDOUT, 'status.php 保护   : ' . (!empty($cfg['security']['protect_status']) ? '需要登录' : '公开可读') . "\n");
        @fwrite(STDOUT, '会话有效期        : ' . Auth::humanDuration((int) ($cfg['security']['session_lifetime'] ?? 0)) . "\n");
        @fwrite(STDOUT, '登录锁定          : ' . ($lockLeft > 0 ? '锁定中，剩余 ' . Auth::humanDuration($lockLeft) : '未锁定') . "\n");
        @fwrite(STDOUT, '后台覆盖项        : ' . count($store->overrides()) . " 个顶层键\n");
        @fwrite(STDOUT, '网页触发限流      : 最小间隔 ' . (int) ($cfg['limits']['web_min_interval'] ?? 0) . ' 秒，24 小时上限 '
            . ((int) ($cfg['limits']['web_daily_max'] ?? 0) ?: '不限') . " 次\n");
        @fwrite(STDOUT, '配置文件          : ' . $cfg['paths']['settings'] . "\n");
        break;

    // ---------------------------------------------------------------- init
    case 'init':
    case 'passwd':
    case 'reset-password':
        $password = rp_cli_option($argv, 'password') ?? rp_cli_read_password();
        if (strlen($password) < 8) {
            fwrite(STDERR, "密码至少 8 位\n");
            exit(1);
        }

        $hash = password_hash($password, PASSWORD_DEFAULT);
        if ($hash === false || !$store->setPasswordHash($hash)) {
            fwrite(STDERR, "写入失败，请检查 data/ 目录权限\n");
            exit(1);
        }

        // 重置密码时顺手清掉失败计数，否则改完密码还是被锁着
        if (is_file($cfg['paths']['security'])) {
            @unlink($cfg['paths']['security']);
        }

        $audit->record('admin_password_set', ['方式' => '命令行']);
        @fwrite(STDOUT, "管理员密码已设置。现在可以打开 admin.php 登录了。\n");
        break;

    // ---------------------------------------------------------------- token
    case 'token':
        $token = rp_cli_option($argv, 'token');
        if ($token === null) {
            $token = bin2hex(random_bytes(24));
        }
        if (!$store->setWebToken($token)) {
            fwrite(STDERR, "令牌至少 16 位，或写入失败\n");
            exit(1);
        }
        $audit->record('set_token', ['方式' => '命令行']);

        @fwrite(STDOUT, "web_token 已写入：\n" . $token . "\n\n");
        @fwrite(STDOUT, "触发地址：\n  https://<你的域名>/fetch.php?token=" . $token . "\n");
        break;

    // ---------------------------------------------------------------- unlock
    case 'unlock':
        if (is_file($cfg['paths']['security'])) {
            @unlink($cfg['paths']['security']);
        }
        $limiter = new RateLimit((string) $cfg['paths']['ratelimit']);
        $limiter->reset('login');
        $limiter->reset('web_trigger');
        $audit->record('unlock_login', ['方式' => '命令行']);
        @fwrite(STDOUT, "登录失败计数与限流计数已清空。\n");
        break;

    // ---------------------------------------------------------------- allow
    case 'allow':
        if (($argv[2] ?? '') === '--clear') {
            $result = $store->set('security.ip_allowlist', '');
            @fwrite(STDOUT, $result['ok'] ? "白名单已清空（不限制来源）。\n" : ('失败：' . $result['error'] . "\n"));
            $audit->record('set_allowlist', ['值' => '清空']);
            break;
        }

        $rules = array_slice($argv, 2);
        if ($rules === []) {
            fwrite(STDERR, "用法：php admin-cli.php allow 1.2.3.4 10.0.0.0/8\n     php admin-cli.php allow --clear\n");
            exit(1);
        }

        $result = $store->set('security.ip_allowlist', implode("\n", $rules));
        if (!$result['ok']) {
            fwrite(STDERR, '失败：' . $result['error'] . "\n");
            exit(1);
        }
        $audit->record('set_allowlist', ['值' => $rules]);
        @fwrite(STDOUT, 'IP 白名单已设置为：' . implode(', ', $rules) . "\n");
        @fwrite(STDOUT, "提醒：确认当前出口 IP 在名单里，否则你自己也进不去。\n");
        break;

    // ---------------------------------------------------------------- reset
    case 'reset':
        $store->reset();
        $audit->record('reset_config', ['方式' => '命令行']);
        @fwrite(STDOUT, "后台覆盖配置已清空（含管理员密码）。请重新执行 init 设置密码。\n");
        break;

    // ---------------------------------------------------------------- prune
    case 'prune':
        $archive = Store::pruneDir($cfg['paths']['archive'], (int) $cfg['retention']['archive_days']);
        $logs    = Store::pruneDir($cfg['paths']['logs'], (int) $cfg['retention']['log_days']);
        $auditN  = $audit->prune((int) $cfg['retention']['log_days']);
        @fwrite(STDOUT, "清理完成：快照 {$archive} 项、日志 {$logs} 项、审计 {$auditN} 项。\n");
        break;

    default:
        @fwrite(STDOUT, <<<TXT
rules-puller 管理命令行

  status                        查看当前安全配置
  init [--password=xxxx]        设置 / 重置管理员密码
  token [--token=xxxx]          生成并写入 web_token
  unlock                        清空登录失败计数、解除锁定
  allow 1.2.3.4 10.0.0.0/8      设置 IP 白名单
  allow --clear                 清空白名单（不限制来源）
  reset                         清除后台改过的全部配置
  prune                         按保留天数清理快照 / 日志 / 审计

TXT);
        exit(0);
}
