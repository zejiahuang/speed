#!/usr/bin/env bash
# 语法自检：对本目录下所有 PHP 文件跑 php -l。
# 用法（Windows 侧）：wsl.exe -e bash -lc "bash /mnt/d/4/rules-puller/check.sh"
set -u

cd "$(dirname "$0")" || exit 2

if ! command -v php >/dev/null 2>&1; then
    echo "未找到 php，可先安装：sudo apt-get install -y php-cli" >&2
    exit 2
fi

fail=0
count=0
while IFS= read -r -d '' file; do
    count=$((count + 1))
    if ! out=$(php -l "$file" 2>&1); then
        echo "$out"
        fail=1
    fi
done < <(find . -name '*.php' -not -path './data/*' -print0)

if [ "$fail" -eq 0 ]; then
    echo "OK: $count 个 PHP 文件语法检查全部通过"
fi

exit "$fail"
