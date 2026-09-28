<?php

declare(strict_types=1);

namespace RulesPuller;

/**
 * 落盘工具：原子写入、JSON 读写、按天清理。
 */
final class Store
{
    /**
     * 先写临时文件再改名，避免下游读到写了一半的文件。
     * Windows 上 rename 覆盖已存在文件可能失败，故失败时退化为先删后改。
     */
    public static function writeAtomic(string $path, string $content): bool
    {
        $dir = dirname($path);
        if (!is_dir($dir) && !@mkdir($dir, 0775, true) && !is_dir($dir)) {
            return false;
        }

        $tmp = $path . '.tmp-' . getmypid();
        if (@file_put_contents($tmp, $content) === false) {
            return false;
        }

        if (@rename($tmp, $path)) {
            return true;
        }

        @unlink($path);
        if (@rename($tmp, $path)) {
            return true;
        }

        @unlink($tmp);

        return false;
    }

    public static function read(string $path): ?string
    {
        if (!is_file($path)) {
            return null;
        }
        $data = @file_get_contents($path);

        return $data === false ? null : $data;
    }

    public static function readJson(string $path): ?array
    {
        $raw = self::read($path);
        if ($raw === null || trim($raw) === '') {
            return null;
        }
        $decoded = json_decode($raw, true);

        return is_array($decoded) ? $decoded : null;
    }

    public static function writeJson(string $path, array $data): bool
    {
        // 空数组要写成 `{}` 而不是 `[]`：这些都是「映射」型文件，
        // 写成列表会让读它的代码（含其它语言的消费者）类型判断出错。
        $payload = $data === [] ? new \stdClass() : $data;

        $json = json_encode(
            $payload,
            JSON_PRETTY_PRINT | JSON_UNESCAPED_UNICODE | JSON_UNESCAPED_SLASHES
        );
        if ($json === false) {
            return false;
        }

        return self::writeAtomic($path, $json . PHP_EOL);
    }

    public static function sha256(string $content): string
    {
        return hash('sha256', $content);
    }

    /**
     * 清理 dir 下修改时间早于 N 天的文件或目录（只处理一层）。
     *
     * @return int 清理数量
     */
    public static function pruneDir(string $dir, int $days): int
    {
        if ($days <= 0 || !is_dir($dir)) {
            return 0;
        }

        $deadline = time() - $days * 86400;
        $removed  = 0;
        $items    = @scandir($dir);
        if ($items === false) {
            return 0;
        }

        foreach ($items as $item) {
            if ($item === '.' || $item === '..') {
                continue;
            }
            $path = $dir . DIRECTORY_SEPARATOR . $item;
            $mtime = @filemtime($path);
            if ($mtime === false || $mtime >= $deadline) {
                continue;
            }
            if (is_dir($path)) {
                self::removeDir($path);
                $removed++;
            } elseif (@unlink($path)) {
                $removed++;
            }
        }

        return $removed;
    }

    /**
     * 递归删除目录（仅用于本工具自己生成的 archive/logs 目录）。
     */
    public static function removeDir(string $dir): bool
    {
        if (!is_dir($dir)) {
            return false;
        }
        $items = @scandir($dir);
        if ($items === false) {
            return false;
        }
        foreach ($items as $item) {
            if ($item === '.' || $item === '..') {
                continue;
            }
            $path = $dir . DIRECTORY_SEPARATOR . $item;
            if (is_dir($path)) {
                self::removeDir($path);
            } else {
                @unlink($path);
            }
        }

        return @rmdir($dir);
    }
}
