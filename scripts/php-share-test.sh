#!/usr/bin/env bash
# 中转站（r.php）回归测试。
set -u
cd /mnt/d/4/rules-puller || exit 2

PORT=8921
BASE="http://127.0.0.1:$PORT"
P=$(command -v php)
pass=0; fail=0
chk() { if [ "$2" = "$3" ]; then echo "  [PASS] $1  ($3)"; pass=$((pass+1));
        else echo "  [FAIL] $1  期望=$2 实际=$3"; fail=$((fail+1)); fi }

# 备份配置
cp data/settings.json /tmp/rp-share.bak 2>/dev/null || true

set_share() { # set_share enabled json cors max_age
  $P -r '
require "bootstrap.php";
$s = rp_settings_store();
$s->set("share.enabled", $argv[1] === "1");
$s->set("share.allow_json", $argv[2] === "1");
$s->set("share.cors", $argv[3] === "1");
$s->set("share.max_age", (int) $argv[4]);
$s->set("share.usbeam_path", "1");
$s->set("share.s302_path", "2");
' "$1" "$2" "$3" "$4" >/dev/null 2>&1
}

($P -S 127.0.0.1:$PORT -t . > /tmp/rp-share-srv.log 2>&1 &)
sleep 2
trap 'pkill -f "php -S 127.0.0.1:$PORT" 2>/dev/null; cp /tmp/rp-share.bak data/settings.json 2>/dev/null' EXIT

echo "===== 1. 关闭时应当 404 ====="
set_share 0 1 1 3600
chk "关闭 → /r.php?p=usbeam 404" "404" \
  "$(curl -s -o /dev/null -w '%{http_code}' "$BASE/r.php?p=usbeam")"

echo
echo "===== 2. 开启后返回 hosts 文本 ====="
set_share 1 1 1 3600
CODE=$(curl -s -o /tmp/sh1.txt -w '%{http_code}' "$BASE/r.php?p=usbeam")
chk "usbeam → 200"        "200" "$CODE"
chk "Content-Type 是文本" "1" "$(curl -s -D - -o /dev/null "$BASE/r.php?p=usbeam" | grep -ci 'Content-Type: text/plain')"
chk "有 ETag"             "1" "$(curl -s -D - -o /dev/null "$BASE/r.php?p=usbeam" | grep -ci '^ETag:')"
chk "有 Cache-Control"    "1" "$(curl -s -D - -o /dev/null "$BASE/r.php?p=usbeam" | grep -ci 'Cache-Control: public')"
chk "有 CORS 头"          "1" "$(curl -s -D - -o /dev/null "$BASE/r.php?p=usbeam" | grep -ci 'Access-Control-Allow-Origin: \*')"
chk "有生成时间头"        "1" "$(curl -s -D - -o /dev/null "$BASE/r.php?p=usbeam" | grep -ci 'X-Rules-Generated-At:')"
chk "禁止搜索引擎收录"    "1" "$(curl -s -D - -o /dev/null "$BASE/r.php?p=usbeam" | grep -ci 'X-Robots-Tag: noindex')"

echo
echo "  内容抽样:"
head -c 300 /tmp/sh1.txt | sed 's/^/    /'
echo

echo "===== 3. s302 返回另一份产物 ====="
CODE=$(curl -s -o /tmp/sh2.txt -w '%{http_code}' "$BASE/r.php?p=s302")
chk "s302 → 200" "200" "$CODE"
chk "两份内容不同" "1" "$(cmp -s /tmp/sh1.txt /tmp/sh2.txt && echo 0 || echo 1)"
chk "s302 含 302 特征" "1" "$(grep -qi 'S302\|Steamcommunity' /tmp/sh2.txt && echo 1 || echo 0)"

echo
echo "===== 4. 用配置的路径名访问 ====="
chk "?p=1 → 200" "200" "$(curl -s -o /dev/null -w '%{http_code}' "$BASE/r.php?p=1")"
chk "?p=2 → 200" "200" "$(curl -s -o /dev/null -w '%{http_code}' "$BASE/r.php?p=2")"
chk "?p=1 和 ?p=usbeam 一致" "1" \
  "$(curl -s "$BASE/r.php?p=1" | cmp -s - /tmp/sh1.txt && echo 1 || echo 0)"

echo
echo "===== 5. JSON 输出 ====="
CODE=$(curl -s -o /tmp/sh1.json -w '%{http_code}' "$BASE/r.php?p=usbeam&format=json")
chk "format=json → 200" "200" "$CODE"
chk "Content-Type 是 JSON" "1" "$(curl -s -D - -o /dev/null "$BASE/r.php?p=usbeam&format=json" | grep -ci 'application/json')"
R=$($P -r '
$j = json_decode(file_get_contents("/tmp/sh1.json"), true);
if (!is_array($j)) { echo "bad"; exit; }
printf("%s %s %d %d",
  $j["ok"] ? 1 : 0,
  $j["kind"] ?? "?",
  count($j["entries"] ?? []),
  $j["count"] ?? -1);')
chk "ok=true"           "1"      "$(echo $R | cut -d' ' -f1)"
chk "kind=usbeam"       "usbeam" "$(echo $R | cut -d' ' -f2)"
chk "entries 非空"      "1"      "$([ "$(echo $R | cut -d' ' -f3)" -gt 100 ] && echo 1 || echo 0)"
chk "count 与 entries 一致" "1"  "$([ "$(echo $R | cut -d' ' -f3)" = "$(echo $R | cut -d' ' -f4)" ] && echo 1 || echo 0)"

echo
echo "  条目抽样:"
$P -r '$j=json_decode(file_get_contents("/tmp/sh1.json"),true);
foreach (array_slice($j["entries"],0,3) as $e) printf("    %s -> %s\n", $e["domain"], $e["ip"]);'

echo
echo "===== 6. only=domains ====="
CODE=$(curl -s -o /tmp/sh1d.json -w '%{http_code}' "$BASE/r.php?p=usbeam&format=json&only=domains")
chk "only=domains → 200" "200" "$CODE"
chk "含 domains 数组"    "1" "$($P -r '$j=json_decode(file_get_contents("/tmp/sh1d.json"),true); echo isset($j["domains"]) && is_array($j["domains"]) ? 1 : 0;')"

echo
echo "===== 7. 关闭 JSON 后 format=json 应 403 ====="
set_share 1 0 1 3600
chk "JSON 关闭 → 403" "403" "$(curl -s -o /dev/null -w '%{http_code}' "$BASE/r.php?p=usbeam&format=json")"
chk "hosts 仍然可用"  "200" "$(curl -s -o /dev/null -w '%{http_code}' "$BASE/r.php?p=usbeam")"

echo
echo "===== 8. 非法参数 ====="
set_share 1 1 1 3600
chk "未知 p → 404"        "404" "$(curl -s -o /dev/null -w '%{http_code}' "$BASE/r.php?p=nope")"
chk "不传 p → 404"        "404" "$(curl -s -o /dev/null -w '%{http_code}' "$BASE/r.php")"
chk "坏 format → 400"     "400" "$(curl -s -o /dev/null -w '%{http_code}' "$BASE/r.php?p=usbeam&format=xml")"

echo
echo "===== 9. ETag / 304 ====="
ET=$(curl -s -D - -o /dev/null "$BASE/r.php?p=usbeam" | grep -i '^ETag:' | sed 's/^[Ee][Tt][Aa][Gg]: *//' | tr -d '\r')
chk "拿到 ETag"        "1" "$([ -n "$ET" ] && echo 1 || echo 0)"
CODE=$(curl -s -o /dev/null -w '%{http_code}' -H "If-None-Match: $ET" "$BASE/r.php?p=usbeam")
chk "带 ETag → 304"    "304" "$CODE"

echo
echo "===== 10. HEAD 请求 ====="
chk "HEAD → 200"  "200" "$(curl -s -I -o /dev/null -w '%{http_code}' "$BASE/r.php?p=usbeam")"
chk "HEAD 无正文" "0"   "$(curl -s -I "$BASE/r.php?p=usbeam" | wc -c | tr -d ' ' | awk '{print ($1<800)?0:1}')"

echo
echo "===== 11. 路径可配置 ====="
$P -r 'require "bootstrap.php"; rp_settings_store()->set("share.usbeam_path", "usbeam");' >/dev/null
chk "改成 usbeam → 200" "200" "$(curl -s -o /dev/null -w '%{http_code}' "$BASE/r.php?p=usbeam")"
chk "旧的 1 已失效"     "404" "$(curl -s -o /dev/null -w '%{http_code}' "$BASE/r.php?p=1")"
$P -r 'require "bootstrap.php"; rp_settings_store()->set("share.usbeam_path", "1");' >/dev/null

echo
echo "===== 12. 两条路径不能相同 ====="
R=$($P -r 'require "bootstrap.php";
$s = rp_settings_store();
$r = $s->saveFromForm(["_form"=>"config",
  "cfg_share__usbeam_path"=>"same",
  "cfg_share__s302_path"=>"same"]);
echo $r["ok"] ? "ok" : "rejected";')
chk "相同路径被拒" "rejected" "$R"
$P -r 'require "bootstrap.php"; $s=rp_settings_store();
$s->set("share.usbeam_path","1"); $s->set("share.s302_path","2");' >/dev/null

echo
echo "===== 13. slug 校验 ====="
R=$($P -r 'require "bootstrap.php";
$s = rp_settings_store();
$cases = ["good-path" => 1, "a.b" => 1, "../etc" => 0, "a/b" => 0, "" => 0,
          "admin" => 0, "has space" => 0, "中文" => 0];
foreach ($cases as $in => $want) {
  $r = $s->set("share.usbeam_path", (string) $in);
  $got = $r["ok"] ? 1 : 0;
  printf("%s=%s ", $in === "" ? "(empty)" : $in, $got === $want ? "OK" : "FAIL($got/$want)");
}
echo "\n";')
echo "  $R"
chk "slug 用例无 FAIL" "0" "$(echo "$R" | grep -c FAIL)"
$P -r 'require "bootstrap.php"; $s=rp_settings_store(); $s->set("share.usbeam_path","1");' >/dev/null

echo
echo "============ 结果 ============"
echo "PASS=$pass  FAIL=$fail"
[ "$fail" -eq 0 ] && echo "全部通过" || echo "有失败项"
