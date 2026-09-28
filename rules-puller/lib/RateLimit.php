<?php

declare(strict_types=1);

namespace RulesPuller;

/**
 * 基于文件的滑动窗口限流。
 *
 * 用来管住「谁能多频繁地让服务器干活」：
 *   - 网页触发拉取：最小间隔 + 滚动 24 小时次数上限
 *   - 后台写操作：最小间隔
 *
 * 计数落盘而不是放内存：PHP 每个请求都是独立进程，内存计数等于没计数。
 */
final class RateLimit
{
    /** 事件保留时长：要能覆盖 24 小时窗口，多留几天便于排障 */
    private const RETENTION = 604800;

    private string $path;

    public function __construct(string $path)
    {
        $this->path = $path;
    }

    /**
     * 检查是否放行（不写入记录，调用方在真正执行后再 record）。
     *
     * @return array{allowed:bool,reason:?string,retry_after:int,used_24h:int,last_at:?int,limit:int}
     */
    public function check(string $bucket, int $minInterval, int $dailyMax): array
    {
        $events = $this->events($bucket, 86400);
        $now    = time();
        $last   = $events === [] ? null : (int) max($events);

        if ($minInterval > 0 && $last !== null) {
            $elapsed = $now - $last;
            if ($elapsed < $minInterval) {
                return [
                    'allowed'     => false,
                    'reason'      => '距上次触发只过了 ' . $elapsed . ' 秒，最小间隔为 ' . $minInterval . ' 秒',
                    'retry_after' => $minInterval - $elapsed,
                    'used_24h'    => count($events),
                    'last_at'     => $last,
                    'limit'       => $minInterval,
                ];
            }
        }

        if ($dailyMax > 0 && count($events) >= $dailyMax) {
            $oldest = (int) min($events);
            $freesAt = $oldest + 86400;

            return [
                'allowed'     => false,
                'reason'      => '最近 24 小时已触发 ' . count($events) . ' 次，达到上限 ' . $dailyMax . ' 次',
                'retry_after' => max(1, $freesAt - $now),
                'used_24h'    => count($events),
                'last_at'     => $last,
                'limit'       => $dailyMax,
            ];
        }

        return [
            'allowed'     => true,
            'reason'      => null,
            'retry_after' => 0,
            'used_24h'    => count($events),
            'last_at'     => $last,
            'limit'       => $dailyMax,
        ];
    }

    /**
     * 单纯的最小间隔节流（用于后台写操作）。
     *
     * @return array{allowed:bool,retry_after:int,last_at:?int}
     */
    public function checkInterval(string $bucket, int $seconds): array
    {
        if ($seconds <= 0) {
            return ['allowed' => true, 'retry_after' => 0, 'last_at' => null];
        }

        $events = $this->events($bucket, max($seconds, 60));
        $last   = $events === [] ? null : (int) max($events);
        if ($last === null) {
            return ['allowed' => true, 'retry_after' => 0, 'last_at' => null];
        }

        $elapsed = time() - $last;
        if ($elapsed < $seconds) {
            return ['allowed' => false, 'retry_after' => $seconds - $elapsed, 'last_at' => $last];
        }

        return ['allowed' => true, 'retry_after' => 0, 'last_at' => $last];
    }

    /**
     * 固定窗口配额：window 秒内最多 max 次。
     *
     * 用于「每个 IP 每分钟只能访问 N 次」这类硬上限。
     *
     * @return array{allowed:bool,used:int,max:int,retry_after:int,window:int}
     */
    public function checkQuota(string $bucket, int $max, int $window): array
    {
        if ($max <= 0 || $window <= 0) {
            return ['allowed' => true, 'used' => 0, 'max' => 0, 'retry_after' => 0, 'window' => $window];
        }

        $events = $this->events($bucket, $window);
        $used   = count($events);

        if ($used >= $max) {
            $oldest = (int) min($events);

            return [
                'allowed'     => false,
                'used'        => $used,
                'max'         => $max,
                'retry_after' => max(1, $oldest + $window - time()),
                'window'      => $window,
            ];
        }

        return ['allowed' => true, 'used' => $used, 'max' => $max, 'retry_after' => 0, 'window' => $window];
    }

    /**
     * 记一次事件。
     */
    public function record(string $bucket): void
    {
        $data = $this->load();
        $now  = time();

        $events = array_values(array_filter(
            (array) ($data[$bucket] ?? []),
            static fn($ts): bool => is_numeric($ts) && $ts > $now - self::RETENTION
        ));
        $events[] = $now;

        $data[$bucket] = array_values(array_slice($events, -1000));
        $this->save($data);
    }

    public function reset(string $bucket): void
    {
        $data = $this->load();
        unset($data[$bucket]);
        $this->save($data);
    }

    /**
     * 按前缀清空多个桶（例如一次性清掉所有 `ip:` 的计数）。
     *
     * @return int 清掉的桶数量
     */
    public function resetPrefix(string $prefix): int
    {
        $data    = $this->load();
        $removed = 0;
        foreach (array_keys($data) as $bucket) {
            if (strncmp((string) $bucket, $prefix, strlen($prefix)) === 0) {
                unset($data[$bucket]);
                $removed++;
            }
        }
        if ($removed > 0) {
            $this->save($data);
        }

        return $removed;
    }

    /**
     * 某桶最近 window 秒内的事件时间戳。
     *
     * @return int[]
     */
    public function events(string $bucket, int $window): array
    {
        $data   = $this->load();
        $now    = time();
        $events = [];
        foreach ((array) ($data[$bucket] ?? []) as $ts) {
            if (is_numeric($ts) && $ts > $now - $window) {
                $events[] = (int) $ts;
            }
        }
        sort($events);

        return $events;
    }

    /**
     * @return array{bucket:string,used_24h:int,last_at:?int,next_allowed_in:int}
     */
    public function stats(string $bucket, int $minInterval): array
    {
        $events = $this->events($bucket, 86400);
        $last   = $events === [] ? null : (int) max($events);

        return [
            'bucket'          => $bucket,
            'used_24h'        => count($events),
            'last_at'         => $last,
            'next_allowed_in' => ($last !== null && $minInterval > 0)
                ? max(0, $minInterval - (time() - $last))
                : 0,
        ];
    }

    private function load(): array
    {
        $data = Store::readJson($this->path);

        return is_array($data) ? $data : [];
    }

    private function save(array $data): void
    {
        Store::writeJson($this->path, $this->prune($data));
    }

    /**
     * 丢弃已经彻底过期的事件，并删掉空桶。
     *
     * 按 IP 计数的桶会随来源数量增长，不清理的话这个文件会一直涨。
     */
    private function prune(array $data): array
    {
        $now     = time();
        $out     = [];
        $changed = false;

        foreach ($data as $bucket => $events) {
            if (!is_array($events)) {
                $changed = true;
                continue;
            }
            $kept = [];
            foreach ($events as $ts) {
                if (is_numeric($ts) && $ts > $now - self::RETENTION) {
                    $kept[] = (int) $ts;
                }
            }
            if ($kept === []) {
                $changed = true;
                continue;
            }
            if (count($kept) !== count($events)) {
                $changed = true;
            }
            $out[$bucket] = $kept;
        }

        // 只在真的清理过时才写盘，避免每次读都产生一次写入
        return $out;
    }
}
