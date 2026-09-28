#!/usr/bin/env bash
# 内部密钥通道回归测试。
#
# 背景：共享主机找不到 PHP CLI 时，后台「立即拉取」改为 HTTP 自触发。
# 但 web_token 默认是空的，导致该按钮在全新安装上必然 403。
# 本测试锁定「无 web_token 也能拉取成功」这一行为。
set -u
cd /mnt/d/4/rules-puller || exit 2

PORT=8912
BASE="http://127.0.0.1:$PORT"
P=$(command -v php)
pass=0; fail=0
chk() { if [ "$2" = "$3" ]; then echo "  [PASS] $1  ($3)"; pass=$((pass+1));
        else echo "  [FAIL] $1  期望=$2 实际=$3"; fail=$((fail+1)); fi }

# --- 准备：清空 web_token，还原用户现场 ---
cp data/settings.json /tmp/is-settings.bak 2>/dev/null || true
$P -r 'require "bootstrap.php";
$ov = rp_settings_store()->overrides(); unset($ov["web_token"]);
RulesPuller\Store::writeJson("data/settings.json", $ov);' >/dev/null
rm -f data/ratelimit.json data/internal-secret.txt

echo "===== 1. 密钥生成与幂等 ====="
S1=$($P -r 'require "bootstrap.php"; echo rp_internal_secret();')
chk "密钥已生成"          "1" "$([ ${#S1} -ge 32 ] && echo 1 || echo 0)"
chk "密钥为十六进制"      "1" "$(echo "$S1" | grep -qE '^[a-f0-9]{32,}$' && echo 1 || echo 0)"
chk "已落盘"              "1" "$([ -f data/internal-secret.txt ] && echo 1 || echo 0)"
S2=$($P -r 'require "bootstrap.php"; echo rp_internal_secret();')
chk "跨进程稳定"          "$S1" "$S2"

echo
echo "===== 2. 校验函数 ====="
R=$($P -r 'require "bootstrap.php";
$s = rp_internal_secret();
printf("%s %s %s %s",
  rp_internal_secret_matches($s) ? 1 : 0,
  rp_internal_secret_matches("") ? 1 : 0,
  rp_internal_secret_matches("deadbeef") ? 1 : 0,
  rp_internal_secret_matches(substr($s, 0, 31)) ? 1 : 0);')
chk "正确密钥通过"        "1" "$(echo $R | cut -d' ' -f1)"
chk "空密钥拒绝"          "0" "$(echo $R | cut -d' ' -f2)"
chk "错密钥拒绝"          "0" "$(echo $R | cut -d' ' -f3)"
chk "截断密钥拒绝"        "0" "$(echo $R | cut -d' ' -f4)"

echo
echo "===== 3. 抗损坏（写坏后重新生成） ====="
echo "garbage" > data/internal-secret.txt
S3=$($P -r 'require "bootstrap.php"; echo rp_internal_secret();')
chk "坏内容被替换"        "1" "$(echo "$S3" | grep -qE '^[a-f0-9]{32,}$' && echo 1 || echo 0)"
chk "新密钥不同于旧的"    "1" "$([ "$S3" != "$S1" ] && echo 1 || echo 0)"

echo
echo "===== 4. HTTP 端到端（无 web_token） ====="
($P -S 127.0.0.1:$PORT -t . > /tmp/is-srv.log 2>&1 &)
sleep 2
SECRET=$($P -r 'require "bootstrap.php"; echo rp_internal_secret();')
WT=$($P -r 'require "bootstrap.php"; echo rp_config()["web_token"];')
chk "web_token 确实为空"  "" "$WT"

CODE=$(curl -s -o /tmp/is1.json -w '%{http_code}' "$BASE/fetch.php?format=json&internal=1")
chk "无凭据 → 403"        "403" "$CODE"

CODE=$(curl -s -o /tmp/is2.json -w '%{http_code}' \
  "$BASE/fetch.php?format=json&quiet=1&internal=1&secret=$SECRET")
chk "内部密钥 → 200"      "200" "$CODE"
OK=$($P -r '$j=json_decode(file_get_contents("/tmp/is2.json"),true); echo ($j["ok"]??false)?"1":"0";')
chk "返回 ok=true"        "1" "$OK"

CODE=$(curl -s -o /dev/null -w '%{http_code}' "$BASE/fetch.php?format=json&internal=1&secret=bad")
chk "错密钥 → 403"        "403" "$CODE"

# 收尾
pkill -f "php -S 127.0.0.1:$PORT" 2>/dev/null
cp /tmp/is-settings.bak data/settings.json 2>/dev/null

echo
echo "============ 结果 ============"
echo "PASS=$pass  FAIL=$fail"
[ "$fail" -eq 0 ] && echo "全部通过" || echo "有失败项"
exit $([ "$fail" -eq 0 ] && echo 0 || echo 1)
