<?php
// 诊断：为什么 cli.binary 的路径校验会拒绝 /usr/bin/php
require '/tmp/rpz/bootstrap.php';

$keys = ['cli.binary', 'cli.http_fallback'];
$e = RulesPuller\ConfigStore::editable();
foreach ($keys as $k) {
    printf("EDITABLE[%s]: %s\n", $k, isset($e[$k]) ? json_encode($e[$k]) : 'MISSING');
}
echo str_repeat('-', 60), "\n";

$inputs = ['/usr/bin/php', '/www/server/php/81/bin/php', '', '../etc/passwd'];
foreach ($inputs as $in) {
    try {
        $v = RulesPuller\ConfigStore::parseValue('cli.binary', $in);
        printf("parseValue(%-32s) => ACCEPT %s\n", json_encode($in), json_encode($v));
    } catch (\Throwable $ex) {
        printf("parseValue(%-32s) => REJECT %s\n", json_encode($in), $ex->getMessage());
    }
}
echo str_repeat('-', 60), "\n";

// 直接看两个 helper 的真实结论
var_dump(function_exists('rp_admin_path_is_absolute'));
if (function_exists('rp_admin_path_is_absolute')) {
    foreach (['/usr/bin/php', '/www/server/php/81/bin/php', '', 'C:/php/php.exe', 'relative/php'] as $p) {
        printf("rp_admin_path_is_absolute(%-28s) = %s\n", json_encode($p),
            rp_admin_path_is_absolute($p) ? 'true' : 'false');
    }
}
echo str_repeat('-', 60), "\n";

$rc = new ReflectionClass('RulesPuller\ConfigStore');
$m = $rc->getMethod('parseValue');
echo "parseValue 声明于: ", basename($m->getFileName()), " 行 ", $m->getStartLine(), "\n";
