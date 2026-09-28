#!/bin/bash
# 从交付 zip 里解出代码，直接在解压目录跑全套回归。
# 目的：验证「用户实际拿到的那份文件」而不是工作副本。
set -u
ZIP_SRC=/mnt/d/4/tmp/rules-puller-deploy.zip
WORK=/tmp/rpz
rm -rf "$WORK"; mkdir -p "$WORK"
cd "$WORK" || exit 1
unzip -q "$ZIP_SRC" -d "$WORK" || { echo "unzip 失败"; exit 1; }
echo "解压目录: $WORK"
echo "文件数: $(find "$WORK" -type f | wc -l)"
echo

P=$(command -v php)
echo "PHP: $($P -v 2>&1 | head -1)"
echo
echo "=== 1. 全量语法检查（zip 内所有 .php）==="
fails=0; total=0
while IFS= read -r f; do
  total=$((total+1))
  out=$($P -l "$f" 2>&1)
  case "$out" in
    *"No syntax errors"*) ;;
    *) echo "  FAIL $f"; echo "  $out"; fails=$((fails+1));;
  esac
done < <(find "$WORK" -name '*.php' | sort)
echo "  检查 $total 个文件, 失败 $fails"
echo

echo "=== 2. cli.* 是否进 EDITABLE ==="
$P -r 'require "'"$WORK"'/bootstrap.php";
$e = RulesPuller\ConfigStore::editable();
foreach (["cli.binary","cli.http_fallback"] as $k) {
  printf("  %-22s %s\n", $k, isset($e[$k]) ? "OK type=".$e[$k]["type"] : "缺失");
}'
echo

echo "=== 3. path 类型校验（走公开 API ConfigStore::set）==="
$P -r 'require "'"$WORK"'/bootstrap.php";
$cases = [
  ["/usr/bin/php", true],
  ["/www/server/php/81/bin/php", true],
  ["", true],
  ["../etc/passwd", false],
  ["/usr/../etc/passwd", false],
  ["relative/php", false],
];
$pass=0; $fail=0;
$tmp = sys_get_temp_dir() . "/rp_settest_" . getmypid() . ".json";
@unlink($tmp);
$store = new RulesPuller\ConfigStore($tmp);
foreach ($cases as [$in,$want]) {
  $r = $store->set("cli.binary", $in);
  $got = (bool) ($r["ok"] ?? false);
  $ok = ($got === $want);
  printf("  %-32s 期望%-6s 实际%-6s %s %s\n", json_encode($in),
      $want?"接受":"拒绝", $got?"接受":"拒绝", $ok?"OK":"FAIL",
      $got ? "" : (string)($r["error"] ?? ""));
  $ok ? $pass++ : $fail++;
}
@unlink($tmp);
echo "  通过 $pass / 失败 $fail\n";'
echo

echo "=== 4. 关键函数是否真的声明在 zip 内 ==="
# 注意：rp_admin_path_is_absolute / rp_internal_secret* 定义在 bootstrap.php，
# 其余在 admin.php —— 别把 grep 指向错的文件。
declare_pairs=(
  "rp_admin_php_cli:admin.php"
  "rp_admin_resolve_php_binary:admin.php"
  "rp_admin_run_pull:admin.php"
  "rp_admin_run_pull_cli:admin.php"
  "rp_admin_run_pull_http:admin.php"
  "rp_admin_http_get:admin.php"
  "rp_admin_path_is_absolute:bootstrap.php"
  "rp_internal_secret:bootstrap.php"
  "rp_internal_secret_matches:bootstrap.php"
)
for pair in "${declare_pairs[@]}"; do
  fn="${pair%%:*}"; file="${pair##*:}"
  n=$(grep -c "function $fn" "$WORK/$file" 2>/dev/null)
  n=${n:-0}
  printf "  %-32s %-16s %s\n" "$fn" "$file" "$([ "$n" -ge 1 ] && echo OK || echo 缺失)"
done
echo

echo "=== 5. fetch.php 的 internal 旁路 ==="
grep -q "internal" "$WORK/fetch.php" && echo "  internal 逻辑 OK" || echo "  缺失"
grep -q "isLocalRequest" "$WORK/fetch.php" && echo "  isLocalRequest 校验 OK" || echo "  缺失"
echo

echo "=== 6. 功能冒烟：CLI 路径真的能跑通 ==="
cd "$WORK" || exit 1
$P admin-cli.php init --password='tmp-test-pass-123' >/dev/null 2>&1
$P fetch.php --quiet --json > /tmp/rp_fetch_out.json 2>/tmp/rp_fetch_err.txt
echo "  fetch 退出码: $?"
if [ -s /tmp/rp_fetch_out.json ]; then
  $P -r '$j = json_decode(file_get_contents("/tmp/rp_fetch_out.json"), true);
    if (!is_array($j)) { echo "  非 JSON 输出\n"; exit; }
    printf("  ok=%s exit_code=%s\n", var_export($j["ok"] ?? null, true), var_export($j["exit_code"] ?? null, true));
    printf("  merged.domains=%s\n", $j["stats"]["merged"]["domains"] ?? "?");'
else
  echo "  无输出，stderr 尾部："
  tail -5 /tmp/rp_fetch_err.txt
fi
echo

echo "=== 汇总 ==="
echo "  语法失败: $fails"
