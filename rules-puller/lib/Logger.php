<?php

declare(strict_types=1);

namespace RulesPuller;

/**
 * 极简日志器：同时写文件与标准输出（CLI 下）。
 */
final class Logger
{
    private string $file;
    private bool $echo;
    private bool $verbose;

    /** @var string[] 本次运行产生的日志行，供末尾汇总 */
    private array $lines = [];

    public function __construct(string $file, bool $echo = true, bool $verbose = false)
    {
        $this->file    = $file;
        $this->echo    = $echo && PHP_SAPI === 'cli';
        $this->verbose = $verbose;
    }

    public function debug(string $message, array $context = []): void
    {
        if ($this->verbose) {
            $this->write('DEBUG', $message, $context);
        }
    }

    public function info(string $message, array $context = []): void
    {
        $this->write('INFO', $message, $context);
    }

    public function warn(string $message, array $context = []): void
    {
        $this->write('WARN', $message, $context);
    }

    public function error(string $message, array $context = []): void
    {
        $this->write('ERROR', $message, $context);
    }

    public function write(string $level, string $message, array $context = []): void
    {
        $line = '[' . date('Y-m-d H:i:s') . '] ' . str_pad($level, 5) . ' ' . $message;
        if ($context !== []) {
            $line .= '  ' . json_encode($context, JSON_UNESCAPED_UNICODE | JSON_UNESCAPED_SLASHES);
        }

        $this->lines[] = $line;

        $dir = dirname($this->file);
        if (!is_dir($dir)) {
            @mkdir($dir, 0775, true);
        }
        @file_put_contents($this->file, $line . PHP_EOL, FILE_APPEND | LOCK_EX);

        if ($this->echo) {
            fwrite(STDOUT, $line . PHP_EOL);
        }
    }

    /** @return string[] */
    public function tail(int $limit = 50): array
    {
        return array_slice($this->lines, -$limit);
    }

    public function logFile(): string
    {
        return $this->file;
    }
}
