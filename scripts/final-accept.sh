#!/bin/bash
# 交付前总验收：解压 zip，在**那份代码**上跑全部套件（必须串行）。
# 串行的理由：套件共用 data/settings.json 与 data/ratelimit.json，
# 并行会互相污染（曾误判为代码回归）。
set -u
ZIP=/mnt/d/4/tmp/rules-puller-deploy.zip
DEST=/tmp/rpz-final
rm -rf "$DEST"; mkdir -p "$DEST"
unzip -q "$ZIP" -d "$DEST"
echo "已解压 zip → $DEST   文件数: $(find "$DEST" -type f | wc -l)"
echo

echo "############ 1. 语法（zip 内全部 .php）############"
f=0; t=0
while IFS= read -r x; do
  t=$((t+1))
  php -l "$x" >/dev/null 2>&1 || { echo "  FAIL $x"; php -l "$x"; f=$((f+1)); }
done < <(find "$DEST" -name '*.php' | sort)
echo "  $t 个 .php，失败 $f"
echo

echo "############ 2. 基础回归（从 zip 解压目录）############"
bash /mnt/d/4/scripts/zip-regress.sh 2>&1 | tail -24
echo

echo "############ 3. 中转站 r.php ############"
bash /mnt/d/4/tmp/php-share-test.sh 2>&1 | tail -6
echo

echo "############ 4. 中转站后台标签页 ############"
bash /mnt/d/4/tmp/php-share-tab-test.sh 2>&1 | tail -6
echo

echo "############ 5. 内部密钥 ############"
bash /mnt/d/4/tmp/php-internal-secret-test.sh 2>&1 | tail -5
echo

echo "############ 6. admin 全套 ############"
bash /mnt/d/4/tmp/php-admin-test.sh 2>&1 | tail -5
echo

echo "############ 7. IP 限流 ############"
bash /mnt/d/4/tmp/php-ip-limit-test.sh 2>&1 | tail -5
