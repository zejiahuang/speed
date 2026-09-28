<?php

declare(strict_types=1);

namespace RulesPuller;

/**
 * 宽容的 INI 解析器。
 *
 * 与 PHP 内置 parse_ini_string() 的区别：
 *  - 保留段与键的**文档顺序**（IP 优先级依赖这个顺序，不能丢）
 *  - 允许重复键（后者覆盖前者，位置保留）
 *  - 允许键名带点号、值里带 `=`、超长 Base64
 *  - 不因单个畸形行放弃整个文件
 */
final class Ini
{
    /**
     * @return array{sections:array<string,array<string,string>>,order:string[],keys:array<string,string[]>,bare:array<string,string[]>}
     */
    public static function parse(string $text): array
    {
        $text  = self::stripBom($text);
        $lines = preg_split('/\r\n|\r|\n/', $text);
        if ($lines === false) {
            $lines = [];
        }

        $sections = ['' => []];
        $keys     = ['' => []];
        $bare     = ['' => []];
        $order    = [''];
        $current  = '';

        foreach ($lines as $line) {
            $trimmed = trim($line);
            if ($trimmed === '') {
                continue;
            }
            $first = $trimmed[0];
            if ($first === ';' || $first === '#') {
                continue;
            }

            if ($first === '[') {
                $end = strpos($trimmed, ']');
                if ($end !== false) {
                    $name = trim(substr($trimmed, 1, $end - 1));
                    if (!isset($sections[$name])) {
                        $sections[$name] = [];
                        $keys[$name]     = [];
                        $bare[$name]     = [];
                        $order[]         = $name;
                    }
                    $current = $name;
                    continue;
                }
            }

            $eq = strpos($trimmed, '=');
            if ($eq === false) {
                $bare[$current][] = $trimmed;
                continue;
            }

            $key = trim(substr($trimmed, 0, $eq));
            if ($key === '') {
                continue;
            }
            $value = trim(substr($trimmed, $eq + 1));

            if (!array_key_exists($key, $sections[$current])) {
                $keys[$current][] = $key;
            }
            $sections[$current][$key] = $value;
        }

        return [
            'sections' => $sections,
            'order'    => $order,
            'keys'     => $keys,
            'bare'     => $bare,
        ];
    }

    /**
     * 取某个段的某个键。
     */
    public static function get(array $ini, string $section, string $key, ?string $default = null): ?string
    {
        $value = $ini['sections'][$section][$key] ?? null;

        return ($value === null || $value === '') ? $default : (string) $value;
    }

    /**
     * 取整段（不含段名）。
     *
     * @return array<string,string>
     */
    public static function section(array $ini, string $section): array
    {
        return $ini['sections'][$section] ?? [];
    }

    public static function hasSection(array $ini, string $section): bool
    {
        return isset($ini['sections'][$section]);
    }

    /**
     * 去掉 UTF-8 BOM，否则第一行的段名会带上不可见字符。
     */
    private static function stripBom(string $text): string
    {
        if (strncmp($text, "\xEF\xBB\xBF", 3) === 0) {
            return substr($text, 3);
        }

        return $text;
    }
}
