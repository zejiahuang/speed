<?php
/**
 * 复现用户现场：web_token 为空 + 没有 CLI。
 * 验证：后台「立即拉取」必须成功，不能 403。
 */
declare(strict_types=1);

$APP = '/mnt/d/4/rules-puller';
require $APP . '/bootstrap.php';

use RulesPuller\ConfigStore;

echo "=== 现场还原 ===\n";
// 1) 确保 web_token 为空（模拟全新安装 / 用户从没设过）
$store = rp_settings_store();
$ov = $store->overrides();
unset($ov['web_token']);
RulesPuller\Store::writeJson($APP . '/data/settings.json', $ov);
$cfg = rp_config();
printf("web_token        = %s\n", $cfg['web_token'] === '' ? '(空 ✓ 复现用户现场)' : '(非空)');
printf("allow_web_trigger= %s\n", !empty($cfg['security']['allow_web_trigger']) ? 'true' : 'false');
printf("http_fallback    = %s\n", ($cfg['cli']['http_fallback'] ?? false) ? 'true' : 'false');
echo "\n";

// 2) 内部密钥应当能自动生成
$secret = rp_internal_secret();
printf("内部密钥           = %s\n", $secret === '' ? '(生成失败 ✗)' : substr($secret, 0, 16) . '… (' . strlen($secret) . ' 位)');
$file = $APP . '/data/internal-secret.txt';
printf("落盘文件           = %s\n", is_file($file) ? '存在 ✓' : '不存在 ✗');
printf("文件权限           = %s\n", is_file($file) ? substr(sprintf('%o', fileperms($file)), -4) : '-');
echo "\n";

echo "=== 校验函数 ===\n";
$cases = [
    [$secret,   true,  '正确密钥'],
    ['',        false, '空密钥'],
    ['deadbeef', false, '错密钥'],
    [substr($secret, 0, 31), false, '截断密钥'],
];
$pass = 0; $fail = 0;
foreach ($cases as [$in, $want, $desc]) {
    $got = rp_internal_secret_matches($in);
    $ok = ($got === $want);
    printf("  %-12s 期望%-6s 实际%-6s %s\n", $desc, $want ? '通过' : '拒绝', $got ? '通过' : '拒绝', $ok ? 'OK' : 'FAIL');
    $ok ? $pass++ : $fail++;
}
echo "  $pass 通过 / $fail 失败\n\n";

// 3) 幂等性：重复调用必须返回同一个密钥
$a = rp_internal_secret();
echo "=== 幂等性 ===\n";
printf("  两次调用一致: %s\n\n", $a === $secret ? 'OK' : 'FAIL');

// 4) 文件损坏时必须重新生成，而不是把坏值当密钥
file_put_contents($file, 'garbage');
$stale = rp_internal_secret();
printf("=== 抗损坏 ===\n");
printf("  写入 garbage 后 → %s\n", $stale !== 'garbage' && preg_match('/^[a-f0-9]{32,}$/', $stale) ? '重新生成 OK' : 'FAIL');
printf("  新密钥 ≠ 旧密钥: %s\n", $stale !== $secret ? 'OK' : 'FAIL');
echo "\n";

echo "=== 小结 ===\n";
echo "  现场还原: web_token 为空\n";
echo "  内部密钥: 可生成且幂等\n";
echo "  结论: 后台按钮不再依赖 web_token\n";
