<?php

declare(strict_types=1);

namespace RulesPuller;

/**
 * 按 IP 的网页访问频率闸门。
 *
 * 「每个 IP 每分钟最多访问 N 次」—— 用来兜住爬取、扫描和误点刷新。
 *
 * 两个刻意的设计：
 *   1. **已登录的管理员用另一套上限**。后台有 6 个标签页，5 次/分钟会让
 *      管理员自己点两下就被挡住。默认给管理员更高的额度，公共入口维持 5 次。
 *   2. **豁免名单默认含本机**。万一阈值调狠了把自己锁在外面，
 *      从 127.0.0.1 还能进去改回来 —— 否则只能去服务器上跑 CLI。
 *
 * 被拒绝的请求**不计数**：窗口由已放行的请求决定，否则攻击者只要不停
 * 触发拒绝就能把窗口无限往后推，把正常用户一起锁死。
 */
final class IpRateGuard
{
    private array $cfg;
    private Auth $auth;
    private RateLimit $limiter;
    private Logger $log;

    public function __construct(array $cfg, Auth $auth, RateLimit $limiter, Logger $log)
    {
        $this->cfg     = $cfg;
        $this->auth    = $auth;
        $this->limiter = $limiter;
        $this->log     = $log;
    }

    /**
     * @param bool $authenticatedAdmin 已登录的管理员走更高的额度
     * @return array{allowed:bool,used:int,max:int,retry_after:int,window:int,ip:string,exempt:bool}
     */
    public function check(bool $authenticatedAdmin = false): array
    {
        $limits = (array) ($this->cfg['limits'] ?? []);
        $ip     = $this->auth->clientIp();

        $max = (int) ($limits['ip_requests_per_minute'] ?? 0);
        if ($authenticatedAdmin) {
            $adminMax = (int) ($limits['admin_requests_per_minute'] ?? 0);
            if ($adminMax > 0) {
                $max = $adminMax;
            }
        }

        $pass = static fn(bool $exempt): array => [
            'allowed' => true, 'used' => 0, 'max' => $max,
            'retry_after' => 0, 'window' => 60, 'ip' => $ip, 'exempt' => $exempt,
        ];

        if ($max <= 0 || $ip === '') {
            return $pass(true);
        }

        // 豁免名单：本机默认在名单里
        foreach ((array) ($limits['ip_rate_limit_exempt'] ?? []) as $rule) {
            if (Auth::ipMatches($ip, (string) $rule)) {
                return $pass(true);
            }
        }

        $state = $this->limiter->checkQuota('ip:' . $ip, $max, 60);
        if (!$state['allowed']) {
            $this->log->warn('IP 访问频率超限', [
                'ip'      => $ip,
                '窗口内'  => $state['used'],
                '上限'    => $max,
                '重试秒'  => $state['retry_after'],
                '管理员'  => $authenticatedAdmin,
            ]);

            return [
                'allowed'     => false,
                'used'        => $state['used'],
                'max'         => $max,
                'retry_after' => $state['retry_after'],
                'window'      => 60,
                'ip'          => $ip,
                'exempt'      => false,
            ];
        }

        $this->limiter->record('ip:' . $ip);

        return [
            'allowed'     => true,
            'used'        => $state['used'] + 1,
            'max'         => $max,
            'retry_after' => 0,
            'window'      => 60,
            'ip'          => $ip,
            'exempt'      => false,
        ];
    }

    /**
     * 当前 IP 的配额使用情况（给后台展示用，不计数）。
     *
     * @return array{used:int,max:int,admin_max:int,exempt:bool}
     */
    public function usage(): array
    {
        $limits = (array) ($this->cfg['limits'] ?? []);
        $ip     = $this->auth->clientIp();

        $exempt = false;
        foreach ((array) ($limits['ip_rate_limit_exempt'] ?? []) as $rule) {
            if (Auth::ipMatches($ip, (string) $rule)) {
                $exempt = true;
                break;
            }
        }

        return [
            'used'      => $exempt ? 0 : count($this->limiter->events('ip:' . $ip, 60)),
            'max'       => (int) ($limits['ip_requests_per_minute'] ?? 0),
            'admin_max' => (int) ($limits['admin_requests_per_minute'] ?? 0),
            'exempt'    => $exempt,
        ];
    }
}
