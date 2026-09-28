#!/usr/bin/env bash
# 验证后台「中转站」标签页渲染 + 就地保存路径。
set -u
cd /mnt/d/4/rules-puller || exit 2
PORT=8933
BASE="http://127.0.0.1:$PORT"
JAR=/tmp/rp-share-cookies.txt
P=$(command -v php)
PW='ShareTab!2026'
pass=0; fail=0
chk() { if [ "$2" = "$3" ]; then echo "  [PASS] $1  ($3)"; pass=$((pass+1));
        else echo "  [FAIL] $1  期望=$2 实际=$3"; fail=$((fail+1)); fi }

cp data/settings.json /tmp/rp-shtab.bak 2>/dev/null || true
rm -f "$JAR" data/ratelimit.json data/security.json
$P admin-cli.php init --password="$PW" >/dev/null 2>&1
$P -r 'require "bootstrap.php"; rp_settings_store()->set("share.enabled", true);' >/dev/null

($P -S 127.0.0.1:$PORT -t . > /tmp/rp-shtab.log 2>&1 &)
sleep 2
trap 'pkill -f "php -S 127.0.0.1:$PORT" 2>/dev/null; cp /tmp/rp-shtab.bak data/settings.json 2>/dev/null' EXIT

# 登录
curl -s -c "$JAR" -b "$JAR" -o /tmp/st-login.html "$BASE/admin.php"
CSRF=$(grep -o 'name="_csrf" value="[a-f0-9]*"' /tmp/st-login.html | head -1 | sed 's/.*value="//;s/"$//')
curl -s -c "$JAR" -b "$JAR" -o /dev/null -w '' -X POST \
  -d "action=login&_csrf=$CSRF&password=$PW" "$BASE/admin.php"
sleep 1

echo "===== 1. 标签页存在且可访问 ====="
code=$(curl -s -b "$JAR" -o /tmp/st-share.html -w '%{http_code}' "$BASE/admin.php?tab=share")
chk "tab=share → 200" "200" "$code"
chk "导航里有「中转站」" "1" "$(grep -c 'tab=share' /tmp/st-share.html)"

echo
echo "===== 2. 渲染出地址 ====="
# 提取所有渲染出来的地址（去重），再逐条判断存在性 —— 比 grep -c 可靠
ADDR=$(grep -o "value=\"$BASE[^\"]*\"" /tmp/st-share.html | sed 's/^value="//;s/"$//' | sort -u)
has() { echo "$ADDR" | grep -qxF "$1" && echo 1 || echo 0; }
chk "含 /1 短地址"        "1" "$(has "$BASE/1")"
chk "含 /2 短地址"        "1" "$(has "$BASE/2")"
chk "含 r.php 通用地址"   "1" "$(has "$BASE/r.php?p=usbeam")"
chk "含 r.php?p=s302"     "1" "$(has "$BASE/r.php?p=s302")"
chk "含 JSON 地址"        "1" "$(has "$BASE/1?format=json")"
chk "含 Nginx 片段"       "1" "$(grep -q 'rewrite' /tmp/st-share.html && echo 1 || echo 0)"

echo
echo "===== 3. 无 PHP 致命错误 ====="
chk "页面无 Fatal/Parse" "0" "$(grep -ci 'Fatal error\|Parse error\|Warning:' /tmp/st-share.html)"

echo
echo "===== 4. 就地改路径 ====="
CSRF2=$(grep -o 'name="_csrf" value="[a-f0-9]*"' /tmp/st-share.html | head -1 | sed 's/.*value="//;s/"$//')
curl -s -b "$JAR" -c "$JAR" -o /dev/null -X POST \
  -d "action=save_config&_form=config&_csrf=$CSRF2&cfg_share__usbeam_path=mine&cfg_share__s302_path=2&cfg_share__enabled__present=1&cfg_share__enabled=1" \
  "$BASE/admin.php"
sleep 1
GOT=$($P -r 'require "bootstrap.php"; echo rp_config()["share"]["usbeam_path"];')
chk "路径已保存为 mine" "mine" "$GOT"

echo
echo "===== 5. 保存后仍能访问新地址 ====="
chk "r.php?p=mine → 200" "200" "$(curl -s -o /dev/null -w '%{http_code}' "$BASE/r.php?p=mine")"
chk "r.php?p=1 已失效"   "404" "$(curl -s -o /dev/null -w '%{http_code}' "$BASE/r.php?p=1")"

echo
echo "===== 6. 拒绝非法路径（页面上直接提示）====="
CSRF3=$(curl -s -b "$JAR" "$BASE/admin.php?tab=share" | grep -o 'name="_csrf" value="[a-f0-9]*"' | head -1 | sed 's/.*value="//;s/"$//')
curl -s -b "$JAR" -c "$JAR" -o /tmp/st-bad.html -X POST \
  -d "action=save_config&_form=config&_csrf=$CSRF3&cfg_share__usbeam_path=../evil&cfg_share__s302_path=2&cfg_share__enabled__present=1&cfg_share__enabled=1" \
  "$BASE/admin.php"
sleep 1
STILL=$($P -r 'require "bootstrap.php"; echo rp_config()["share"]["usbeam_path"];')
chk "非法路径未写入" "mine" "$STILL"

echo
echo "============ 结果 ============"
echo "PASS=$pass  FAIL=$fail"
[ "$fail" -eq 0 ] && echo "全部通过" || echo "有失败项"
