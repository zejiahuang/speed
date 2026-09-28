#!/bin/bash
# 抗损坏：跨进程验证
APP=/mnt/d/4/rules-puller
F=$APP/data/internal-secret.txt
P=$(command -v php)

echo "=== 1. 先取一个正常密钥（进程 A）==="
A=$($P -r 'require "'"$APP"'/bootstrap.php"; echo rp_internal_secret();')
echo "  进程 A: ${A:0:16}…"

echo "=== 2. 故意写坏文件 ==="
echo "garbage-not-a-hex-key" > "$F"
echo "  文件内容: $(cat "$F")"

echo "=== 3. 新进程 B 读取 → 应当重新生成 ==="
B=$($P -r 'require "'"$APP"'/bootstrap.php"; echo rp_internal_secret();')
echo "  进程 B: ${B:0:16}…"
if [ "$B" != "garbage-not-a-hex-key" ] && echo "$B" | grep -qE '^[a-f0-9]{32,}$'; then
  echo "  → 重新生成 OK"
else
  echo "  → FAIL"
fi
[ "$B" != "$A" ] && echo "  → 新密钥 ≠ 旧密钥 OK" || echo "  → 新密钥 = 旧密钥 FAIL"

echo "=== 4. 再开进程 C → 应读到与 B 相同（已落盘并复用）==="
C=$($P -r 'require "'"$APP"'/bootstrap.php"; echo rp_internal_secret();')
echo "  进程 C: ${C:0:16}…"
[ "$C" = "$B" ] && echo "  → 跨进程稳定 OK" || echo "  → 不稳定 FAIL"

echo "=== 5. 文件权限 ==="
stat -c '  %a %n' "$F"
echo "  （/mnt/d 是 Windows 挂载，chmod 不生效；真实 Linux 主机上应为 600）"
