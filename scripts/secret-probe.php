<?php
/**
 * 抗损坏测试必须在新进程里做——同一进程内有 static 缓存，
 * 第二次调用必然返回缓存值（这是设计如此，不是 bug）。
 */
declare(strict_types=1);
$APP = '/mnt/d/4/rules-puller';
require $APP . '/bootstrap.php';

$file = $APP . '/data/internal-secret.txt';
$before = is_file($file) ? trim(file_get_contents($file)) : null;
$now = rp_internal_secret();

printf("文件里现有的         = %s\n", $before === null ? '(无)' : substr($before, 0, 16) . '…');
printf("本进程读到的         = %s\n", substr($now, 0, 16) . '…');
printf("两者一致             = %s\n", $before === $now ? 'OK（本进程内幂等）' : 'DIFF');
