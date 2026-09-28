<?php

/**
 * 引导文件：自动加载 lib/ 下的类，并暴露少量全局工具函数。
 *
 * 所有入口（fetch.php / status.php）都应先 require 本文件。
 */

declare(strict_types=1);

if (PHP_VERSION_ID < 80000) {
    @file_put_contents('php://stderr', "需要 PHP 8.0 或更高版本，当前为 " . PHP_VERSION . PHP_EOL);
    exit(2);
}

spl_autoload_register(static function (string $class): void {
    $prefix = 'RulesPuller\\';
    if (strncmp($class, $prefix, strlen($prefix)) !== 0) {
        return;
    }
    $short = substr($class, strlen($prefix));
    $file  = __DIR__ . '/lib/' . $short . '.php';
    if (is_file($file)) {
        require $file;
    }
});

/**
 * 读取配置：config.php 打底，再用 data/settings.json 里后台改过的值覆盖。
 */
function rp_config(?string $path = null): array
{
    static $cache = null;
    if ($cache !== null) {
        return $cache;
    }

    $file = $path ?? __DIR__ . '/config.php';
    if (!is_file($file)) {
        throw new RuntimeException('找不到配置文件: ' . $file);
    }

    /** @var array $loaded */
    $loaded = require $file;

    $overridesFile = $loaded['paths']['settings'] ?? (__DIR__ . '/data/settings.json');
    if (is_file($overridesFile)) {
        $raw       = @file_get_contents($overridesFile);
        $overrides = is_string($raw) ? json_decode($raw, true) : null;
        if (is_array($overrides) && $overrides !== []) {
            $loaded = RulesPuller\ConfigStore::deepMerge($loaded, $overrides);
        }
    }

    $cache = $loaded;

    return $cache;
}

/**
 * 后台可编辑设置的读写入口。
 */
function rp_settings_store(): RulesPuller\ConfigStore
{
    $cfg = rp_config();

    return new RulesPuller\ConfigStore($cfg['paths']['settings'] ?? (__DIR__ . '/data/settings.json'));
}

/**
 * 把配置里的相对目录全部建成绝对目录。
 *
 * 注意：`paths` 里既有目录也有文件。这里按**白名单**判断哪些是目录 ——
 * 反过来写（列出哪些是文件）会踩坑：新增一个文件型的键时忘了同步，
 * 它就会被建成同名目录，之后所有写入都静默失败。
 */
function rp_prepare_dirs(array $cfg): array
{
    $dirKeys = ['data', 'raw', 'cache', 'out', 'logs', 'archive'];

    foreach ($cfg['paths'] as $key => $path) {
        if (!is_string($path) || $path === '') {
            continue;
        }

        if (in_array($key, $dirKeys, true)) {
            if (!is_dir($path) && !@mkdir($path, 0775, true) && !is_dir($path)) {
                throw new RuntimeException('无法创建目录: ' . $path);
            }
            continue;
        }

        // 文件型路径：只保证父目录存在
        $parent = dirname($path);
        if (!is_dir($parent) && !@mkdir($parent, 0775, true) && !is_dir($parent)) {
            throw new RuntimeException('无法创建目录: ' . $parent);
        }

        // 修复历史遗留：早期版本把文件型路径误建成了空目录，
        // 目录会一直挡着写入，且失败是静默的。空目录直接清掉。
        if (is_dir($path)) {
            $items = @scandir($path);
            if ($items !== false && count($items) <= 2 && @rmdir($path)) {
                // 已清掉，继续
            }
        }
    }

    return $cfg;
}

/**
 * 判断当前是否命令行运行。
 */
function rp_is_cli(): bool
{
    return PHP_SAPI === 'cli';
}

/**
 * 简单的人类可读字节数。
 */
function rp_human_bytes(int $bytes): string
{
    $units = ['B', 'KB', 'MB', 'GB'];
    $i     = 0;
    $value = (float) $bytes;
    while ($value >= 1024 && $i < count($units) - 1) {
        $value /= 1024;
        $i++;
    }

    return $i === 0 ? $bytes . ' B' : number_format($value, 2) . ' ' . $units[$i];
}

/**
 * 归一化域名：去空白、去末尾点、转小写、剔除明显非法的值。
 * 返回 null 表示不是合法域名。
 */
function rp_normalize_domain(string $domain): ?string
{
    $domain = strtolower(trim($domain));
    $domain = rtrim($domain, '.');
    if ($domain === '') {
        return null;
    }
    // 占位符 / 变量 / 纯数字 / 含空格或斜杠的一律不是域名
    if (strpbrk($domain, " \t{}/*\\\"'<>") !== false) {
        return null;
    }
    if (strlen($domain) > 253) {
        return null;
    }
    if (!preg_match('/^[a-z0-9_]([a-z0-9_\-]*[a-z0-9_])?(\.[a-z0-9_]([a-z0-9_\-]*[a-z0-9_])?)+$/', $domain)) {
        return null;
    }

    return $domain;
}

/**
 * 判断字符串是否为 IP 字面量（v4 / v6）。
 */
function rp_is_ip(string $value): bool
{
    return filter_var($value, FILTER_VALIDATE_IP) !== false;
}

/**
 * 判断是否为绝对路径（POSIX 或 Windows 盘符）。
 *
 * 注意：此处**不检查路径是否存在** —— 它只用于表单校验；
 * 真正的可用性由 rp_admin_resolve_php_binary() 执行验证。
 */
function rp_admin_path_is_absolute(string $path): bool
{
    if ($path === '') {
        return false;
    }
    $norm = str_replace('\\', '/', $path);

    return $norm[0] === '/' || (bool) preg_match('#^[A-Za-z]:/#', $norm);
}

/**
 * 本机「内部调用」密钥（internal secret）。
 *
 * 后台的「立即拉取」在找不到 PHP CLI 时会转而请求自己的 `fetch.php`。
 * 但 `web_token` 默认是空的（用户需要手动去「配置」页生成），
 * 于是这个按钮在全新安装上必然 403 —— 用户根本不知道要先设令牌。
 *
 * 所以这里再给一条**不需要任何配置**的内部通道：
 *   - 密钥由本机自己派生，写在 `data/internal-secret.txt`（0600）；
 *   - 只在「请求确实来自回环」**且**「密钥匹配」时被 `fetch.php` 接受；
 *   - 与 `web_token` 相互独立，任一成立即可放行。
 *
 * 它不是「后门」：要利用它，攻击者必须已经能在服务器上读文件
 * （那就已经拿到了 `config.php` 和 `settings.json`），而外部请求
 * 拿不到回环身份，也猜不到这个随机值。
 *
 * @return string 32 字节十六进制串；不可写时返回 ''（此时回退为要求 web_token）
 */
function rp_internal_secret(): string
{
    static $cached = null;
    if ($cached !== null) {
        return $cached;
    }

    $file = __DIR__ . '/data/internal-secret.txt';

    if (is_file($file)) {
        $existing = trim((string) @file_get_contents($file));
        // 只接受像样的长度，避免文件被写坏后当成有效密钥
        if (preg_match('/^[a-f0-9]{32,}$/i', $existing)) {
            return $cached = $existing;
        }
    }

    try {
        $secret = bin2hex(random_bytes(32));
    } catch (\Throwable $e) {
        // 极端环境下 random_bytes 不可用：退回哈希一个不可预测的组合
        $secret = hash('sha256', __DIR__ . '|' . (string) getmypid() . '|' . microtime(true));
    }

    $dir = dirname($file);
    if (!is_dir($dir)) {
        @mkdir($dir, 0700, true);
    }

    if (@file_put_contents($file, $secret, LOCK_EX) === false) {
        return $cached = '';
    }
    @chmod($file, 0600);

    return $cached = $secret;
}

/**
 * 校验一个「内部调用」密钥是否合法。
 */
function rp_internal_secret_matches(string $given): bool
{
    if ($given === '') {
        return false;
    }
    $secret = rp_internal_secret();

    return $secret !== '' && hash_equals($secret, $given);
}

/**
 * 从 `{Name}` 形式的占位符里取出名字；不是占位符则返回 null。
 */
function rp_placeholder_name(string $value): ?string
{
    if (preg_match('/^\{\s*([A-Za-z0-9_\-]+)\s*\}$/', trim($value), $m)) {
        return $m[1];
    }

    return null;
}
