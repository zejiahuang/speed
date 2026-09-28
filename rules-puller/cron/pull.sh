#!/usr/bin/env bash
#
# 定时任务包装脚本：跑一次拉取，把结果 JSON 追加到 cron 日志。
#
# 用法：
#   bash cron/pull.sh                 正常拉取
#   bash cron/pull.sh --force         强制重新下载
#   PHP_BIN=/usr/bin/php8.1 bash cron/pull.sh
#
set -u

DIR="$(cd "$(dirname "$0")/.." && pwd)"
PHP_BIN="${PHP_BIN:-php}"
LOG_DIR="$DIR/data/logs"
LOG_FILE="$LOG_DIR/cron-$(date +%Y-%m-%d).log"

mkdir -p "$LOG_DIR"
cd "$DIR" || exit 2

if ! command -v "$PHP_BIN" >/dev/null 2>&1; then
    echo "[$(date '+%F %T')] 找不到 PHP 可执行文件：$PHP_BIN" >>"$LOG_FILE"
    exit 2
fi

{
    echo "----- $(date '+%F %T') 开始 -----"
} >>"$LOG_FILE"

"$PHP_BIN" fetch.php --quiet --json "$@" >>"$LOG_FILE" 2>&1
code=$?

echo "[$(date '+%F %T')] 退出码=$code" >>"$LOG_FILE"

exit "$code"
