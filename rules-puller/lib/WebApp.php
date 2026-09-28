<?php

declare(strict_types=1);

namespace RulesPuller;

/**
 * Web 请求管线：安全响应头 → IP 白名单 → 会话 → 登录 / 登出 / 首次设置 → 交给页面处理。
 *
 * admin.php 与 status.php 共用这一层，保证两处的鉴权行为完全一致 ——
 * 鉴权逻辑一旦分叉，迟早有一边被漏掉。
 */
final class WebApp
{
    private array $cfg;
    private Logger $log;
    private Auth $auth;
    private Audit $audit;
    private RateLimit $limiter;
    private IpRateGuard $ipGuard;
    private string $self;

    public function __construct(array $cfg)
    {
        $this->cfg     = $cfg;
        $this->log     = new Logger($cfg['paths']['logs'] . '/web-' . date('Y-m') . '.log', false);
        $this->auth    = new Auth($cfg, $this->log);
        $this->audit   = new Audit($cfg['paths']['logs']);
        $this->limiter = new RateLimit((string) ($cfg['paths']['ratelimit'] ?? (__DIR__ . '/../data/ratelimit.json')));
        $this->ipGuard = new IpRateGuard($cfg, $this->auth, $this->limiter, $this->log);
        $this->self    = basename((string) ($_SERVER['SCRIPT_NAME'] ?? 'admin.php'));
    }

    public function auth(): Auth
    {
        return $this->auth;
    }

    public function ipGuard(): IpRateGuard
    {
        return $this->ipGuard;
    }

    public function audit(): Audit
    {
        return $this->audit;
    }

    public function limiter(): RateLimit
    {
        return $this->limiter;
    }

    public function log(): Logger
    {
        return $this->log;
    }

    public function cfg(): array
    {
        return $this->cfg;
    }

    public function selfUrl(array $query = []): string
    {
        return $query === [] ? $this->self : $this->self . '?' . http_build_query($query);
    }

    // ------------------------------------------------------------ 提示与跳转

    public function flash(string $kind, string $message): void
    {
        if (PHP_SAPI === 'cli' || session_status() !== PHP_SESSION_ACTIVE) {
            return;
        }
        $_SESSION['rp_flash'][] = ['kind' => $kind, 'message' => $message];
    }

    /**
     * @return array<int,array{kind:string,message:string}>
     */
    public function takeFlash(): array
    {
        if (PHP_SAPI === 'cli' || session_status() !== PHP_SESSION_ACTIVE) {
            return [];
        }
        $items = (array) ($_SESSION['rp_flash'] ?? []);
        unset($_SESSION['rp_flash']);

        return $items;
    }

    public function redirect(string $url, int $status = 303): void
    {
        if (!headers_sent()) {
            header('Location: ' . $url, true, $status);
        }
        exit(0);
    }

    // ------------------------------------------------------------ 主流程

    /**
     * @param callable          $handler function(WebApp $app, string $action): void
     * @param array{require_login?:bool} $options
     *        require_login = false 用于「公开只读」的页面（status.php 关掉保护时）
     */
    public function run(callable $handler, array $options = []): void
    {
        $requireLogin = $options['require_login'] ?? true;

        $this->sendSecurityHeaders();

        // 1) IP 白名单 —— 最先拦，被拦的请求不碰会话、不读文件
        if (!$this->auth->ipAllowed()) {
            $ip = $this->auth->clientIp();
            $this->log->warn('来源不在 IP 白名单内', ['ip' => $ip, 'script' => $this->self]);
            $this->audit->record('ip_denied', ['script' => $this->self]);
            echo View::deniedPage(403, '拒绝访问', '来源 IP ' . $ip . ' 不在 security.ip_allowlist 白名单内。');
            exit(0);
        }

        $this->auth->startSession();

        // 2) 每 IP 访问频率（已登录的管理员走另一套更高的额度）
        $guard = $this->ipGuard->check($this->auth->isLoggedIn());
        if (!$guard['allowed']) {
            if (!headers_sent()) {
                header('Retry-After: ' . max(1, (int) $guard['retry_after']));
            }
            $this->audit->record('ip_rate_limited', [
                'ip'   => $guard['ip'],
                '窗口内' => $guard['used'],
                '上限'  => $guard['max'],
            ]);
            echo View::deniedPage(
                429,
                '访问过于频繁',
                '来源 IP ' . $guard['ip'] . ' 在最近一分钟内已访问 ' . $guard['used']
                . ' 次，超过上限 ' . $guard['max'] . ' 次。'
                . '请 ' . Auth::humanDuration((int) $guard['retry_after']) . '后再试。'
            );
            exit(0);
        }

        $method = strtoupper((string) ($_SERVER['REQUEST_METHOD'] ?? 'GET'));
        $action = (string) ($_POST['action'] ?? $_GET['action'] ?? '');

        // 2) 登出
        if ($action === 'logout') {
            $this->audit->record('logout');
            $this->auth->logout();
            $this->redirect($this->selfUrl());
        }

        // 3) 首次设置密码（只允许本机，且必须尚未设置过）
        if ($requireLogin && !$this->auth->isConfigured()) {
            if (!$this->auth->isLocalRequest()) {
                echo View::deniedPage(
                    503,
                    '尚未初始化',
                    '还没有设置管理员密码。请先在本机执行 `php admin-cli.php init`，'
                    . '或从 127.0.0.1 打开本页面完成设置。'
                );
                exit(0);
            }

            if ($method === 'POST' && $action === 'setup') {
                if (!$this->auth->verifyCsrf($_POST['_csrf'] ?? null)) {
                    echo View::deniedPage(400, '请求被拒绝', '表单令牌无效，请刷新页面后重试。');
                    exit(0);
                }
                $result = $this->auth->setPassword(
                    (string) ($_POST['password'] ?? ''),
                    (string) ($_POST['password2'] ?? '')
                );
                if ($result['ok']) {
                    $this->auth->login((string) ($_POST['password'] ?? ''));
                    $this->audit->record('admin_password_set', ['方式' => '网页首次设置']);
                    $this->flash('success', '管理员密码已设置。');
                    $this->redirect($this->selfUrl());
                }
                echo View::loginPage([
                    'setup' => true,
                    'error' => $result['error'],
                    'csrf'  => $this->auth->csrfToken(),
                    'ip'    => $this->auth->clientIp(),
                ]);
                exit(0);
            }

            echo View::loginPage([
                'setup' => true,
                'csrf'  => $this->auth->csrfToken(),
                'ip'    => $this->auth->clientIp(),
            ]);
            exit(0);
        }

        // 4) 登录
        if ($method === 'POST' && $action === 'login') {
            if (!$this->auth->verifyCsrf($_POST['_csrf'] ?? null)) {
                echo View::deniedPage(400, '请求被拒绝', '表单令牌无效，请刷新页面后重试。');
                exit(0);
            }

            // 轻微节流：即使没到锁定阈值，也把爆破速度压下来
            $throttle = $this->limiter->checkInterval('login', 2);
            if (!$throttle['allowed']) {
                $this->flash('warn', '请求过于频繁，请稍后再试。');
                $this->redirect($this->selfUrl());
            }
            $this->limiter->record('login');

            $result = $this->auth->login((string) ($_POST['password'] ?? ''));
            if ($result['ok']) {
                $this->audit->record('login', ['结果' => '成功']);
                $this->flash('success', '已登录。');
                $this->redirect($this->selfUrl());
            }

            $this->audit->record('login', ['结果' => '失败', '原因' => $result['error']]);
            echo View::loginPage([
                'error' => $result['error'],
                'csrf'  => $this->auth->csrfToken(),
                'ip'    => $this->auth->clientIp(),
            ]);
            exit(0);
        }

        // 5) 未登录 → 登录页
        if ($requireLogin && !$this->auth->isLoggedIn()) {
            echo View::loginPage([
                'csrf' => $this->auth->csrfToken(),
                'ip'   => $this->auth->clientIp(),
            ]);
            exit(0);
        }

        // 6) 已登录：写操作必须带 CSRF
        if ($method === 'POST' && !$this->auth->verifyCsrf($_POST['_csrf'] ?? null)) {
            $this->audit->record('csrf_rejected', ['action' => $action]);
            echo View::deniedPage(400, '请求被拒绝', '表单令牌无效或已过期，请返回上一页刷新后重试。');
            exit(0);
        }

        $handler($this, $action);
    }

    /**
     * 写操作的统一节流闸门。
     */
    public function throttle(string $bucket): bool
    {
        $seconds = (int) ($this->cfg['limits']['admin_action_interval'] ?? 0);
        if ($seconds <= 0) {
            return true;
        }

        $state = $this->limiter->checkInterval($bucket, $seconds);
        if (!$state['allowed']) {
            $this->flash('warn', '操作过于频繁，请 ' . Auth::humanDuration((int) $state['retry_after']) . '后再试。');

            return false;
        }

        $this->limiter->record($bucket);

        return true;
    }

    private function sendSecurityHeaders(): void
    {
        if (headers_sent()) {
            return;
        }
        header('Content-Type: text/html; charset=utf-8');
        header('Cache-Control: no-store, no-cache, must-revalidate');
        header('Pragma: no-cache');
        header('X-Content-Type-Options: nosniff');
        header('X-Frame-Options: DENY');
        header('Referrer-Policy: no-referrer');
        header('X-Robots-Tag: noindex, nofollow');
    }
}
