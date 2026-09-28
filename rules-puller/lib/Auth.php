<?php

declare(strict_types=1);

namespace RulesPuller;

/**
 * 后台鉴权：会话、CSRF、登录节流、IP 白名单。
 *
 * 安全取向：宁可麻烦一点。
 *   - 只存 password_hash()，不存明文，也不可逆推
 *   - 连续失败按 IP 计数并锁定（防暴力破解）
 *   - 白名单为空时不限制来源，但一旦填了就默认拒绝（fail-closed）
 *   - X-Forwarded-For 只在直连来源是可信代理时才采信（否则可以伪造）
 */
final class Auth
{
    private const SESSION_NAME = 'rp_admin';

    private array $cfg;
    private Logger $log;
    private string $securityPath;
    private bool $sessionStarted = false;

    public function __construct(array $cfg, Logger $log)
    {
        $this->cfg          = $cfg;
        $this->log          = $log;
        $this->securityPath = (string) ($cfg['paths']['security'] ?? (__DIR__ . '/../data/security.json'));
    }

    /** @return array<string,mixed> */
    public function security(): array
    {
        return (array) ($this->cfg['security'] ?? []);
    }

    /** @return array<string,mixed> */
    public function limits(): array
    {
        return (array) ($this->cfg['limits'] ?? []);
    }

    public function isConfigured(): bool
    {
        return trim((string) ($this->security()['admin_password_hash'] ?? '')) !== '';
    }

    // ------------------------------------------------------------ 来源判定

    /**
     * 真实客户端 IP。只有直连来源在 trusted_proxies 里才采信 X-Forwarded-For。
     */
    public function clientIp(): string
    {
        $remote = (string) ($_SERVER['REMOTE_ADDR'] ?? '');

        $trusted = array_map('strval', (array) ($this->security()['trusted_proxies'] ?? []));
        if ($trusted !== [] && $remote !== '' && $this->matchesAny($remote, $trusted)) {
            $forwarded = (string) ($_SERVER['HTTP_X_FORWARDED_FOR'] ?? '');
            if ($forwarded !== '') {
                $first = trim(explode(',', $forwarded)[0]);
                if (filter_var($first, FILTER_VALIDATE_IP) !== false) {
                    return $first;
                }
            }
        }

        return $remote;
    }

    public function isLocalRequest(): bool
    {
        $ip = $this->clientIp();

        return $ip === '127.0.0.1' || $ip === '::1' || $ip === '::ffff:127.0.0.1';
    }

    /**
     * 白名单为空 = 不限制；非空 = 不在名单里一律拒绝（fail-closed）。
     */
    public function ipAllowed(): bool
    {
        $allow = array_map('strval', (array) ($this->security()['ip_allowlist'] ?? []));
        if ($allow === []) {
            return true;
        }

        return $this->matchesAny($this->clientIp(), $allow);
    }

    /**
     * @param string[] $rules IP 或 CIDR
     */
    private function matchesAny(string $ip, array $rules): bool
    {
        if ($ip === '') {
            return false;
        }
        foreach ($rules as $rule) {
            if (self::ipMatches($ip, $rule)) {
                return true;
            }
        }

        return false;
    }

    public static function ipMatches(string $ip, string $rule): bool
    {
        $rule = trim($rule);
        if ($rule === '') {
            return false;
        }
        if (strpos($rule, '/') === false) {
            return $ip === $rule;
        }

        [$subnet, $bits] = explode('/', $rule, 2) + [null, null];
        if ($subnet === null || $bits === null || !ctype_digit($bits)) {
            return false;
        }

        $ipBin    = @inet_pton($ip);
        $subnetBin = @inet_pton($subnet);
        if ($ipBin === false || $subnetBin === false || strlen($ipBin) !== strlen($subnetBin)) {
            return false;
        }

        $bits  = (int) $bits;
        $bytes = intdiv($bits, 8);
        $rest  = $bits % 8;

        if ($bytes > 0 && strncmp($ipBin, $subnetBin, $bytes) !== 0) {
            return false;
        }
        if ($rest === 0) {
            return true;
        }

        $mask = (0xFF << (8 - $rest)) & 0xFF;

        return (ord($ipBin[$bytes]) & $mask) === (ord($subnetBin[$bytes]) & $mask);
    }

    // ------------------------------------------------------------ 会话

    public function startSession(): void
    {
        if ($this->sessionStarted || PHP_SAPI === 'cli') {
            return;
        }
        if (session_status() === PHP_SESSION_ACTIVE) {
            $this->sessionStarted = true;

            return;
        }

        session_name(self::SESSION_NAME);
        session_set_cookie_params([
            'lifetime' => 0,
            'path'     => '/',
            'httponly' => true,
            'secure'   => self::isHttps(),
            'samesite' => 'Lax',
        ]);
        @session_start();
        $this->sessionStarted = true;
    }

    public function isLoggedIn(): bool
    {
        if (!$this->sessionStarted || PHP_SAPI === 'cli') {
            return false;
        }
        if (empty($_SESSION['rp_user'])) {
            return false;
        }

        $lifetime = (int) ($this->security()['session_lifetime'] ?? 7200);
        $lastSeen = (int) ($_SESSION['rp_last_seen'] ?? 0);
        if ($lifetime > 0 && $lastSeen > 0 && (time() - $lastSeen) > $lifetime) {
            $this->logout();
            $this->log->info('会话超时，已自动登出', ['ip' => $this->clientIp()]);

            return false;
        }

        $_SESSION['rp_last_seen'] = time();

        return true;
    }

    /**
     * @return array{ok:bool,error:?string}
     */
    public function login(string $password): array
    {
        if (!$this->isConfigured()) {
            return ['ok' => false, 'error' => '还没有设置管理员密码，请先执行 php admin-cli.php init'];
        }

        $remaining = $this->lockRemaining();
        if ($remaining > 0) {
            return ['ok' => false, 'error' => '尝试次数过多，已锁定，请 ' . self::humanDuration($remaining) . '后再试'];
        }

        $hash = (string) ($this->security()['admin_password_hash'] ?? '');
        if (!password_verify($password, $hash)) {
            $count = $this->recordFailure();
            $max   = (int) ($this->security()['max_login_failures'] ?? 5);
            $this->log->warn('登录失败', ['ip' => $this->clientIp(), '连续失败' => $count]);

            $message = '密码不正确';
            if ($max > 0 && $count >= $max && $this->lockRemaining() > 0) {
                $message .= '；已连续失败 ' . $count . ' 次，来源已锁定';
            }

            return ['ok' => false, 'error' => $message];
        }

        $this->clearFailures();
        if (session_status() === PHP_SESSION_ACTIVE) {
            session_regenerate_id(true);
        }
        $_SESSION['rp_user']      = 'admin';
        $_SESSION['rp_last_seen'] = time();
        $_SESSION['rp_ip']        = $this->clientIp();
        $this->log->info('登录成功', ['ip' => $this->clientIp()]);

        return ['ok' => true, 'error' => null];
    }

    public function logout(): void
    {
        if (PHP_SAPI === 'cli' || session_status() !== PHP_SESSION_ACTIVE) {
            return;
        }
        $_SESSION = [];
        if (ini_get('session.use_cookies')) {
            $params = session_get_cookie_params();
            setcookie(self::SESSION_NAME, '', [
                'expires'  => time() - 42000,
                'path'     => $params['path'],
                'domain'   => $params['domain'],
                'secure'   => (bool) $params['secure'],
                'httponly' => (bool) $params['httponly'],
                'samesite' => 'Lax',
            ]);
        }
        @session_destroy();
    }

    // ------------------------------------------------------------ CSRF

    public function csrfToken(): string
    {
        if (PHP_SAPI === 'cli' || session_status() !== PHP_SESSION_ACTIVE) {
            return '';
        }
        if (empty($_SESSION['rp_csrf'])) {
            $_SESSION['rp_csrf'] = bin2hex(random_bytes(32));
        }

        return (string) $_SESSION['rp_csrf'];
    }

    public function csrfField(): string
    {
        return '<input type="hidden" name="_csrf" value="' . htmlspecialchars($this->csrfToken(), ENT_QUOTES, 'UTF-8') . '">';
    }

    public function verifyCsrf(?string $token): bool
    {
        if (PHP_SAPI === 'cli') {
            return true;
        }
        $expected = (string) ($_SESSION['rp_csrf'] ?? '');
        if ($expected === '' || $token === null || $token === '') {
            return false;
        }

        return hash_equals($expected, $token);
    }

    // ------------------------------------------------------------ 密码

    /**
     * @return array{ok:bool,error:?string}
     */
    public function setPassword(string $password, ?string $confirm = null): array
    {
        if (strlen($password) < 8) {
            return ['ok' => false, 'error' => '密码至少 8 位'];
        }
        if ($confirm !== null && $password !== $confirm) {
            return ['ok' => false, 'error' => '两次输入的密码不一致'];
        }
        if (strlen($password) > 200) {
            return ['ok' => false, 'error' => '密码过长'];
        }

        $hash = password_hash($password, PASSWORD_DEFAULT);
        if ($hash === false) {
            return ['ok' => false, 'error' => '生成密码哈希失败'];
        }

        if (!rp_settings_store()->setPasswordHash($hash)) {
            return ['ok' => false, 'error' => '写入 data/settings.json 失败，请检查目录权限'];
        }

        $this->log->info('管理员密码已更新', ['ip' => $this->clientIp()]);

        return ['ok' => true, 'error' => null];
    }

    // ------------------------------------------------------------ 失败计数

    public function failureCount(): int
    {
        $entry = $this->failureEntry();

        return (int) ($entry['count'] ?? 0);
    }

    /**
     * 锁定时长（秒）。0 = 未锁定。
     */
    public function lockRemaining(): int
    {
        $max = (int) ($this->security()['max_login_failures'] ?? 5);
        if ($max <= 0) {
            return 0;
        }
        $entry = $this->failureEntry();
        if ((int) ($entry['count'] ?? 0) < $max) {
            return 0;
        }

        $lockout = (int) ($this->security()['lockout_seconds'] ?? 900);
        if ($lockout <= 0) {
            return 0;
        }

        $elapsed = time() - (int) ($entry['last'] ?? 0);

        return max(0, $lockout - $elapsed);
    }

    public function clearFailures(): void
    {
        $data = Store::readJson($this->securityPath) ?? [];
        unset($data['failures'][$this->clientIp()]);
        Store::writeJson($this->securityPath, $data);
    }

    private function recordFailure(): int
    {
        $data = Store::readJson($this->securityPath) ?? [];
        if (!isset($data['failures']) || !is_array($data['failures'])) {
            $data['failures'] = [];
        }

        $ip     = $this->clientIp();
        $lockout = (int) ($this->security()['lockout_seconds'] ?? 900);
        $entry  = $data['failures'][$ip] ?? ['count' => 0, 'last' => 0];

        // 上一次失败已经过了锁定时长 → 计数清零重新开始
        if ($lockout > 0 && (time() - (int) $entry['last']) > $lockout) {
            $entry['count'] = 0;
        }

        $entry['count'] = (int) $entry['count'] + 1;
        $entry['last']  = time();
        $data['failures'][$ip] = $entry;

        // 顺手清理过期记录，避免文件无限增长
        foreach ($data['failures'] as $key => $value) {
            if (!is_array($value) || (time() - (int) ($value['last'] ?? 0)) > max(86400, $lockout * 2)) {
                unset($data['failures'][$key]);
            }
        }

        Store::writeJson($this->securityPath, $data);

        return (int) $entry['count'];
    }

    private function failureEntry(): array
    {
        $data = Store::readJson($this->securityPath) ?? [];
        $entry = $data['failures'][$this->clientIp()] ?? null;
        if (!is_array($entry)) {
            return ['count' => 0, 'last' => 0];
        }
        $lockout = (int) ($this->security()['lockout_seconds'] ?? 900);
        if ($lockout > 0 && (time() - (int) ($entry['last'] ?? 0)) > $lockout) {
            return ['count' => 0, 'last' => 0];
        }

        return $entry;
    }

    // ------------------------------------------------------------ 杂项

    public static function isHttps(): bool
    {
        if (!empty($_SERVER['HTTPS']) && strtolower((string) $_SERVER['HTTPS']) !== 'off') {
            return true;
        }
        if ((string) ($_SERVER['SERVER_PORT'] ?? '') === '443') {
            return true;
        }

        return strtolower((string) ($_SERVER['HTTP_X_FORWARDED_PROTO'] ?? '')) === 'https';
    }

    public static function humanDuration(int $seconds): string
    {
        if ($seconds < 60) {
            return $seconds . ' 秒';
        }
        if ($seconds < 3600) {
            return floor($seconds / 60) . ' 分 ' . ($seconds % 60) . ' 秒';
        }

        return floor($seconds / 3600) . ' 小时 ' . floor(($seconds % 3600) / 60) . ' 分';
    }
}
