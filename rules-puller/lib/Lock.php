<?php

declare(strict_types=1);

namespace RulesPuller;

/**
 * 单实例锁：防止定时任务与手动触发（或两次 cron）重叠执行。
 *
 * 用 flock 而非「检查文件是否存在」——进程崩溃时内核会自动释放，
 * 不会留下需要人工清理的僵尸锁。
 */
final class Lock
{
    private string $path;

    /** @var resource|null */
    private $handle = null;

    public function __construct(string $path)
    {
        $this->path = $path;
    }

    public function acquire(): bool
    {
        $dir = dirname($this->path);
        if (!is_dir($dir) && !@mkdir($dir, 0775, true) && !is_dir($dir)) {
            return false;
        }

        $handle = @fopen($this->path, 'c');
        if ($handle === false) {
            return false;
        }

        if (!@flock($handle, LOCK_EX | LOCK_NB)) {
            fclose($handle);

            return false;
        }

        ftruncate($handle, 0);
        fwrite($handle, (string) json_encode([
            'pid'  => function_exists('getmypid') ? getmypid() : null,
            'at'   => date('c'),
            'host' => function_exists('gethostname') ? gethostname() : null,
            'sapi' => PHP_SAPI,
        ], JSON_UNESCAPED_UNICODE));
        fflush($handle);

        $this->handle = $handle;

        return true;
    }

    /**
     * 读出当前持锁者的信息，仅用于日志提示。
     */
    public function holder(): ?array
    {
        if (!is_file($this->path)) {
            return null;
        }
        $raw = @file_get_contents($this->path);
        if ($raw === false || trim($raw) === '') {
            return null;
        }
        $decoded = json_decode($raw, true);

        return is_array($decoded) ? $decoded : null;
    }

    public function release(): void
    {
        if ($this->handle !== null) {
            @flock($this->handle, LOCK_UN);
            @fclose($this->handle);
            $this->handle = null;
        }
    }
}
