<?php

declare(strict_types=1);

namespace RulesPuller;

/**
 * 操作审计：谁、从哪个 IP、什么时候、做了什么。
 *
 * 按月写 JSONL，追加即可，便于事后追责与排障。
 */
final class Audit
{
    private string $dir;

    public function __construct(string $dir)
    {
        $this->dir = $dir;
    }

    public function record(string $action, array $detail = [], string $actor = 'admin'): void
    {
        if (!is_dir($this->dir) && !@mkdir($this->dir, 0775, true) && !is_dir($this->dir)) {
            return;
        }

        $line = json_encode([
            'at'     => date('c'),
            'actor'  => $actor,
            'ip'     => (string) ($_SERVER['REMOTE_ADDR'] ?? 'cli'),
            'action' => $action,
            'detail' => $detail,
        ], JSON_UNESCAPED_UNICODE | JSON_UNESCAPED_SLASHES);

        if ($line === false) {
            return;
        }

        @file_put_contents($this->file(), $line . PHP_EOL, FILE_APPEND | LOCK_EX);
    }

    /**
     * 最近的审计记录（新的在前）。
     *
     * @return array<int,array{at:string,actor:string,ip:string,action:string,detail:mixed}>
     */
    public function recent(int $limit = 50): array
    {
        $rows  = [];
        $month = date('Y-m');
        $files = [$this->dir . '/audit-' . $month . '.jsonl'];

        // 当月记录不够就往回翻一个月，避免月初看起来「什么都没发生过」
        $previous = date('Y-m', strtotime('-1 month'));
        if ($previous !== $month) {
            $files[] = $this->dir . '/audit-' . $previous . '.jsonl';
        }

        foreach ($files as $file) {
            if (!is_file($file)) {
                continue;
            }
            $handle = @fopen($file, 'r');
            if ($handle === false) {
                continue;
            }
            while (($line = fgets($handle)) !== false) {
                $line = trim($line);
                if ($line === '') {
                    continue;
                }
                $decoded = json_decode($line, true);
                if (is_array($decoded)) {
                    $rows[] = $decoded;
                }
            }
            fclose($handle);
            if (count($rows) >= $limit * 3) {
                break;
            }
        }

        $rows = array_reverse($rows);

        return array_slice($rows, 0, max(1, $limit));
    }

    /**
     * 删除早于 N 天的审计文件。
     */
    public function prune(int $days): int
    {
        if ($days <= 0) {
            return 0;
        }
        $deadline = time() - $days * 86400;
        $removed  = 0;
        $files    = glob($this->dir . '/audit-*.jsonl') ?: [];

        foreach ($files as $file) {
            $mtime = @filemtime($file);
            if ($mtime !== false && $mtime < $deadline && @unlink($file)) {
                $removed++;
            }
        }

        return $removed;
    }

    private function file(): string
    {
        return $this->dir . '/audit-' . date('Y-m') . '.jsonl';
    }
}
