#!/bin/bash
# 端到端：起 PHP 内置服务器 → 请求 admin.php 的拉取入口 → 检查结果。
# 关键：全程 web_token 为空，且强制走 HTTP 兜底（模拟共享主机无 CLI）。
set -u
APP=/mnt/d/4/rules-puller
PORT=8971
P=$(command -v php)

echo "=== 0. 备份并还原用户现场（清空 web_token）==="
cp "$APP/data/settings.json" /tmp/settings.bak 2>/dev/null
$P -r '
require "'"$APP"'/bootstrap.php";
$ov = rp_settings_store()->overrides();
unset($ov["web_token"]);
RulesPuller\Store::writeJson("'"$APP"'/data/settings.json", $ov);
$cfg = rp_config();
echo "  web_token = ", ($cfg["web_token"] === "" ? "(已清空，还原用户现场)" : "(非空!)"), "\n";
'

echo
echo "=== 1. 启动内置服务器 ==="
cd "$APP" || exit 1
$P -S 127.0.0.1:$PORT -t "$APP" > /tmp/php-srv.log 2>&1 &
SRV=$!
sleep 1.5
if ! kill -0 $SRV 2>/dev/null; then echo "  服务器启动失败"; cat /tmp/php-srv.log; exit 1; fi
echo "  已启动 pid=$SRV port=$PORT"

cleanup() {
  kill $SRV 2>/dev/null
  cp /tmp/settings.bak "$APP/data/settings.json" 2>/dev/null
  echo "  已清理"
}
trap cleanup EXIT

echo
echo "=== 2. 直接打 fetch.php，不带任何凭据（应 403）==="
CODE=$(curl -s -o /tmp/r1.json -w '%{http_code}' "http://127.0.0.1:$PORT/fetch.php?format=json&internal=1")
echo "  HTTP $CODE"
head -c 200 /tmp/r1.json; echo

echo
echo "=== 3. 带内部密钥（后台按钮就是这么发的，应成功）==="
SECRET=$($P -r 'require "'"$APP"'/bootstrap.php"; echo rp_internal_secret();')
echo "  密钥: ${SECRET:0:16}…"
CODE=$(curl -s -o /tmp/r2.json -w '%{http_code}' \
  "http://127.0.0.1:$PORT/fetch.php?format=json&quiet=1&internal=1&secret=$SECRET")
echo "  HTTP $CODE"
$P -r '
$j = json_decode(file_get_contents("/tmp/r2.json"), true);
if (!is_array($j)) { echo "  非 JSON: ", substr(file_get_contents("/tmp/r2.json"),0,200), "\n"; exit; }
printf("  ok=%s exit_code=%s domains=%s\n", var_export($j["ok"]??null,true), $j["exit_code"]??"?" ,
  $j["stats"]["merged"]["domains"] ?? "?");
'

echo
echo "=== 4. 错密钥（应 403）==="
CODE=$(curl -s -o /tmp/r3.json -w '%{http_code}' \
  "http://127.0.0.1:$PORT/fetch.php?format=json&internal=1&secret=wrongkey000")
echo "  HTTP $CODE"
head -c 160 /tmp/r3.json; echo

echo
echo "=== 5. web_token 仍然可用（非回环场景的凭据）==="
$P -r '
require "'"$APP"'/bootstrap.php";
rp_settings_store()->setWebToken("abcdefghijklmnop1234");
$cfg = rp_config();
echo "  已设 web_token = ", ($cfg["web_token"] !== "" ? "OK" : "FAIL"), "\n";
'
CODE=$(curl -s -o /tmp/r4.json -w '%{http_code}' \
  "http://127.0.0.1:$PORT/fetch.php?format=json&quiet=1&internal=1&token=abcdefghijklmnop1234")
echo "  带正确 token → HTTP $CODE"
$P -r '$j=json_decode(file_get_contents("/tmp/r4.json"),true); printf("  ok=%s\n", var_export($j["ok"]??null,true));'
