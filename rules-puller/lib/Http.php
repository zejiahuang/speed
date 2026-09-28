<?php

declare(strict_types=1);

namespace RulesPuller;

/**
 * HTTP 抓取：UA 识别、禁用系统代理、强制 UTF-8、完整性校验、多源回退。
 *
 * 对应说明文档《UsbEAm Hosts 代理 APK 开发说明》1.2 / 1.3 节。
 */
final class Http
{
    private array $cfg;
    private Logger $log;
    private string $userAgent = '';

    public function __construct(array $httpCfg, Logger $log)
    {
        $this->cfg = $httpCfg;
        $this->log = $log;
    }

    public function setUserAgent(string $userAgent): void
    {
        $this->userAgent = $userAgent;
    }

    /**
     * 单次 GET。
     *
     * @param array $opts etag / last_modified / headers / timeout / resolve
     * @return array{ok:bool,status:int,body:?string,headers:array,error:?string,url:string,ms:int,bytes:int,not_modified:bool}
     */
    public function get(string $url, array $opts = []): array
    {
        $started = microtime(true);

        $headers = ['Accept: */*', 'Accept-Language: zh-CN,zh;q=0.9,en;q=0.8', 'Connection: close'];
        foreach ((array) ($opts['headers'] ?? []) as $extra) {
            $headers[] = (string) $extra;
        }
        if (!empty($opts['etag'])) {
            $headers[] = 'If-None-Match: ' . $opts['etag'];
        }
        if (!empty($opts['last_modified'])) {
            $headers[] = 'If-Modified-Since: ' . $opts['last_modified'];
        }

        $timeout = (int) ($opts['timeout'] ?? $this->cfg['timeout']);

        $result = function_exists('curl_init')
            ? $this->curlGet($url, $headers, $timeout, $opts)
            : $this->streamGet($url, $headers, $timeout);

        if ($result['ok'] && !$result['not_modified'] && $result['body'] !== null && !empty($this->cfg['force_utf8'])) {
            $result['body'] = $this->toUtf8($result['body'], $result['headers']);
        }

        $result['ms']    = (int) round((microtime(true) - $started) * 1000);
        $result['bytes'] = $result['body'] === null ? 0 : strlen($result['body']);

        return $result;
    }

    /**
     * 按「主源 → 备源 → TXT 备用 IP」的顺序抓取一个数据源，并做完整性校验。
     *
     * @param array $source  config['sources'][x]
     * @param array $prev    上一次的 state（用于条件请求）
     * @return array{ok:bool,not_modified:bool,url:?string,status:int,body:?string,bytes:int,
     *               sha256:?string,etag:?string,last_modified:?string,attempts:array,error:?string}
     */
    public function fetchSource(array $source, array $prev = [], bool $force = false): array
    {
        $urls      = array_values((array) ($source['urls'] ?? []));
        $retries   = max(1, (int) ($this->cfg['retries'] ?? 1));
        $delayUs   = max(0, (int) ($this->cfg['retry_delay_ms'] ?? 0)) * 1000;
        $attempts  = [];
        $lastError = '没有可用的数据源地址';

        if ($urls === []) {
            return $this->failureResult($attempts, $lastError);
        }

        $useConditional = !$force && (!empty($prev['etag']) || !empty($prev['last_modified']));

        $this->setUserAgent((string) ($source['user_agent'] ?? ''));

        foreach ($urls as $url) {
            for ($attempt = 1; $attempt <= $retries; $attempt++) {
                $opts = [];
                if ($useConditional) {
                    $opts['etag']          = $prev['etag'] ?? null;
                    $opts['last_modified'] = $prev['last_modified'] ?? null;
                }

                $res = $this->get((string) $url, $opts);

                if ($res['ok'] && $res['not_modified']) {
                    $attempts[] = ['url' => $url, 'attempt' => $attempt, 'status' => 304, 'ms' => $res['ms'], 'error' => null];
                    $this->log->info('内容未变化（304），复用上次解析结果', ['url' => $url, 'ms' => $res['ms']]);

                    return [
                        'ok'            => true,
                        'not_modified'  => true,
                        'url'           => (string) $url,
                        'status'        => 304,
                        'body'          => null,
                        'bytes'         => 0,
                        'sha256'        => $prev['sha256'] ?? null,
                        'etag'          => $prev['etag'] ?? null,
                        'last_modified' => $prev['last_modified'] ?? null,
                        'attempts'      => $attempts,
                        'error'         => null,
                    ];
                }

                if ($res['ok']) {
                    $problem = $this->validate($source, (string) $res['body']);
                    if ($problem === null) {
                        $attempts[] = ['url' => $url, 'attempt' => $attempt, 'status' => $res['status'], 'ms' => $res['ms'], 'bytes' => $res['bytes'], 'error' => null];
                        $this->log->info('抓取成功', [
                            'url'   => $url,
                            'http'  => $res['status'],
                            'bytes' => $res['bytes'],
                            'ms'    => $res['ms'],
                        ]);

                        return [
                            'ok'            => true,
                            'not_modified'  => false,
                            'url'           => (string) ($res['url'] ?: $url),
                            'status'        => $res['status'],
                            'body'          => (string) $res['body'],
                            'bytes'         => (int) $res['bytes'],
                            'sha256'        => Store::sha256((string) $res['body']),
                            'etag'          => $res['headers']['etag'] ?? null,
                            'last_modified' => $res['headers']['last-modified'] ?? null,
                            'attempts'      => $attempts,
                            'error'         => null,
                        ];
                    }
                    $lastError  = $problem;
                    $attempts[] = ['url' => $url, 'attempt' => $attempt, 'status' => $res['status'], 'ms' => $res['ms'], 'bytes' => $res['bytes'], 'error' => $problem];
                    $this->log->warn('数据校验未通过，重试', ['url' => $url, 'reason' => $problem]);
                } else {
                    $lastError  = (string) ($res['error'] ?? '未知错误');
                    $attempts[] = ['url' => $url, 'attempt' => $attempt, 'status' => $res['status'], 'ms' => $res['ms'], 'error' => $lastError];
                    $this->log->warn('请求失败', ['url' => $url, 'attempt' => $attempt, 'error' => $lastError]);
                }

                if ($attempt < $retries && $delayUs > 0) {
                    usleep($delayUs);
                }
            }
        }

        // 所有地址都失败 —— 尝试 TXT 备用 IP（对应文档 1.3「网络抗封锁机制」）
        $fallback = $this->fetchViaTxtIps($source, $urls, $attempts);
        if ($fallback !== null) {
            return $fallback;
        }

        return $this->failureResult($attempts, $lastError);
    }

    /**
     * 主源域名被污染时的兜底：DoH 查 TXT 拿备用 IP，再以固定 IP 直连。
     */
    private function fetchViaTxtIps(array $source, array $urls, array &$attempts): ?array
    {
        $map = (array) ($this->cfg['txt_fallback'] ?? []);
        if ($map === []) {
            return null;
        }

        $host = parse_url((string) $urls[0], PHP_URL_HOST);
        if (!is_string($host) || $host === '' || !isset($map[$host])) {
            return null;
        }

        $txtName = (string) $map[$host];
        $this->log->warn('主备源均失败，尝试从 TXT 记录获取备用 IP', ['host' => $host, 'txt' => $txtName]);

        $ips = $this->resolveTxtIps($txtName);
        $ips = array_slice($ips, 0, max(1, (int) ($this->cfg['txt_max_ips'] ?? 8)));
        if ($ips === []) {
            $this->log->warn('TXT 记录中没有可用 IP', ['txt' => $txtName]);

            return null;
        }
        $this->log->info('TXT 备用 IP', ['ips' => $ips]);

        $this->setUserAgent((string) ($source['user_agent'] ?? ''));

        foreach ($ips as $ip) {
            foreach ($urls as $url) {
                $res = $this->get((string) $url, ['resolve' => [$host . ':443:' . $ip]]);
                if (!$res['ok']) {
                    $attempts[] = ['url' => $url, 'via' => $ip, 'status' => $res['status'], 'ms' => $res['ms'], 'error' => $res['error']];
                    continue;
                }
                $problem = $this->validate($source, (string) $res['body']);
                if ($problem !== null) {
                    $attempts[] = ['url' => $url, 'via' => $ip, 'status' => $res['status'], 'ms' => $res['ms'], 'error' => $problem];
                    continue;
                }
                $attempts[] = ['url' => $url, 'via' => $ip, 'status' => $res['status'], 'ms' => $res['ms'], 'bytes' => $res['bytes'], 'error' => null];
                $this->log->info('经 TXT 备用 IP 抓取成功', ['ip' => $ip, 'bytes' => $res['bytes'], 'ms' => $res['ms']]);

                return [
                    'ok'            => true,
                    'not_modified'  => false,
                    'url'           => (string) $url . ' @' . $ip,
                    'status'        => $res['status'],
                    'body'          => (string) $res['body'],
                    'bytes'         => (int) $res['bytes'],
                    'sha256'        => Store::sha256((string) $res['body']),
                    'etag'          => $res['headers']['etag'] ?? null,
                    'last_modified' => $res['headers']['last-modified'] ?? null,
                    'attempts'      => $attempts,
                    'error'         => null,
                ];
            }
        }

        return null;
    }

    /**
     * 完整性校验：长度下限 + 标志串。
     */
    private function validate(array $source, string $body): ?string
    {
        $minBytes = (int) ($source['min_bytes'] ?? 0);
        if ($minBytes > 0 && strlen($body) < $minBytes) {
            return '内容过短（' . strlen($body) . ' < ' . $minBytes . ' 字节），疑似不完整';
        }

        $marker = $source['marker'] ?? null;
        if (is_string($marker) && $marker !== '' && strpos($body, $marker) === false) {
            return '缺少校验标志 ' . $marker . '，数据不完整';
        }

        return null;
    }

    private function failureResult(array $attempts, string $error): array
    {
        return [
            'ok'            => false,
            'not_modified'  => false,
            'url'           => null,
            'status'        => 0,
            'body'          => null,
            'bytes'         => 0,
            'sha256'        => null,
            'etag'          => null,
            'last_modified' => null,
            'attempts'      => $attempts,
            'error'         => $error,
        ];
    }

    // ------------------------------------------------------------ DoH

    /**
     * DoH 查询，返回全部 Answer 的 data 字段。
     *
     * @return string[]
     */
    public function dohQuery(string $name, string $type = 'A'): array
    {
        $answers = [];
        foreach ((array) ($this->cfg['doh_endpoints'] ?? []) as $endpoint) {
            $separator = strpos($endpoint, '?') === false ? '?' : '&';
            $url       = $endpoint . $separator . http_build_query(['name' => $name, 'type' => $type]);

            $res = $this->get($url, [
                'headers' => ['Accept: application/dns-json'],
                'timeout' => max(3, min(8, (int) $this->cfg['timeout'])),
            ]);
            if (!$res['ok'] || $res['body'] === null) {
                continue;
            }

            $json = json_decode($res['body'], true);
            if (!is_array($json) || empty($json['Answer']) || !is_array($json['Answer'])) {
                continue;
            }

            foreach ($json['Answer'] as $answer) {
                if (isset($answer['data']) && is_scalar($answer['data'])) {
                    $answers[] = trim((string) $answer['data'], "\" \t");
                }
            }

            if ($answers !== []) {
                break;
            }
        }

        return array_values(array_unique($answers));
    }

    /**
     * 解析 A 记录。
     *
     * @return string[]
     */
    public function resolveA(string $name): array
    {
        $ips = [];
        foreach ($this->dohQuery($name, 'A') as $data) {
            if (filter_var($data, FILTER_VALIDATE_IP, FILTER_FLAG_IPV4) !== false) {
                $ips[] = $data;
            }
        }

        return array_values(array_unique($ips));
    }

    /**
     * 并发批量解析 A 记录。
     *
     * 逐个解析时每次要等一个完整往返（实测约 1 秒），几百个域名就是几分钟；
     * 这里用 curl_multi 并发，几百个域名降到几十秒内。
     *
     * @param string[] $domains
     * @return array<string,string[]> 只包含解析成功的域名
     */
    public function resolveMany(array $domains, int $concurrency = 12, int $timeout = 5): array
    {
        $domains = array_values(array_unique(array_filter(
            array_map(static fn($d): string => (string) $d, $domains),
            static fn(string $d): bool => $d !== ''
        )));
        if ($domains === []) {
            return [];
        }

        $endpoints = array_values((array) ($this->cfg['doh_endpoints'] ?? []));

        if ($endpoints === [] || !function_exists('curl_multi_init')) {
            $fallback = [];
            foreach ($domains as $domain) {
                $ips = $this->resolveA($domain);
                if ($ips !== []) {
                    $fallback[$domain] = $ips;
                }
            }

            return $fallback;
        }

        $resolved  = [];
        $remaining = $domains;

        foreach ($endpoints as $endpoint) {
            if ($remaining === []) {
                break;
            }
            $batch = $this->multiDoh($remaining, (string) $endpoint, $concurrency, $timeout);
            foreach ($batch as $domain => $ips) {
                $resolved[$domain] = $ips;
            }
            $remaining = array_values(array_diff($remaining, array_keys($batch)));
        }

        return $resolved;
    }

    /**
     * 一轮并发的 DoH 查询。
     *
     * @param string[] $domains
     * @return array<string,string[]>
     */
    private function multiDoh(array $domains, string $endpoint, int $concurrency, int $timeout): array
    {
        $result    = [];
        $queue     = array_values($domains);
        $active    = [];
        $separator = strpos($endpoint, '?') === false ? '?' : '&';
        $multi     = curl_multi_init();

        $start = function () use (&$queue, &$active, $multi, $endpoint, $separator, $timeout): void {
            if ($queue === []) {
                return;
            }
            $domain = array_shift($queue);
            $url    = $endpoint . $separator . http_build_query(['name' => $domain, 'type' => 'A']);

            $handle = curl_init();
            $set    = [
                CURLOPT_URL            => $url,
                CURLOPT_RETURNTRANSFER => true,
                CURLOPT_CONNECTTIMEOUT => min(4, (int) ($this->cfg['connect_timeout'] ?? 4)),
                CURLOPT_TIMEOUT        => $timeout,
                CURLOPT_USERAGENT      => $this->userAgent,
                CURLOPT_HTTPHEADER     => ['Accept: application/dns-json'],
                CURLOPT_SSL_VERIFYPEER => true,
                CURLOPT_SSL_VERIFYHOST => 2,
                CURLOPT_FOLLOWLOCATION => true,
                CURLOPT_ENCODING       => '',
            ];
            if (empty($this->cfg['use_system_proxy'])) {
                $set[CURLOPT_PROXY]   = '';
                $set[CURLOPT_NOPROXY] = '*';
            }
            curl_setopt_array($handle, $set);

            curl_multi_add_handle($multi, $handle);
            $active[spl_object_id($handle)] = ['handle' => $handle, 'domain' => $domain];
        };

        for ($i = 0; $i < $concurrency; $i++) {
            $start();
        }

        $running = 0;
        do {
            @curl_multi_exec($multi, $running);
            if ($running > 0) {
                @curl_multi_select($multi, 1.0);
            }

            while ($info = curl_multi_info_read($multi)) {
                $handle = $info['handle'];
                $id     = spl_object_id($handle);
                if (!isset($active[$id])) {
                    continue;
                }

                $domain = $active[$id]['domain'];
                $body   = curl_multi_getcontent($handle);
                $code   = (int) curl_getinfo($handle, CURLINFO_RESPONSE_CODE);

                if ($info['result'] === CURLE_OK && $code === 200 && is_string($body)) {
                    $json = json_decode($body, true);
                    if (is_array($json) && !empty($json['Answer']) && is_array($json['Answer'])) {
                        $ips = [];
                        foreach ($json['Answer'] as $answer) {
                            $data = $answer['data'] ?? null;
                            if (is_string($data) && filter_var($data, FILTER_VALIDATE_IP, FILTER_FLAG_IPV4) !== false) {
                                $ips[] = $data;
                            }
                        }
                        if ($ips !== []) {
                            $result[$domain] = array_values(array_unique($ips));
                        }
                    }
                }

                curl_multi_remove_handle($multi, $handle);
                curl_close($handle);
                unset($active[$id]);

                $start();
            }
        } while ($running > 0 || $active !== []);

        curl_multi_close($multi);

        return $result;
    }

    /**
     * 解析 TXT 记录并抽出其中的 IPv4 地址。
     *
     * @return string[]
     */
    public function resolveTxtIps(string $name): array
    {
        $ips = [];
        foreach ($this->dohQuery($name, 'TXT') as $data) {
            if (preg_match_all('/\b(?:\d{1,3}\.){3}\d{1,3}\b/', $data, $m)) {
                foreach ($m[0] as $ip) {
                    if (filter_var($ip, FILTER_VALIDATE_IP, FILTER_FLAG_IPV4) !== false) {
                        $ips[] = $ip;
                    }
                }
            }
        }

        return array_values(array_unique($ips));
    }

    // ------------------------------------------------------------ 底层实现

    private function curlGet(string $url, array $headers, int $timeout, array $opts): array
    {
        $responseHeaders = [];

        $ch = curl_init();
        $set = [
            CURLOPT_URL            => $url,
            CURLOPT_RETURNTRANSFER => true,
            CURLOPT_FOLLOWLOCATION => true,
            CURLOPT_MAXREDIRS      => (int) ($this->cfg['follow_location'] ?? 5),
            CURLOPT_CONNECTTIMEOUT => (int) ($this->cfg['connect_timeout'] ?? 6),
            CURLOPT_TIMEOUT        => $timeout,
            CURLOPT_USERAGENT      => $this->userAgent,
            CURLOPT_HTTPHEADER     => $headers,
            CURLOPT_ENCODING       => '',
            CURLOPT_SSL_VERIFYPEER => true,
            CURLOPT_SSL_VERIFYHOST => 2,
            CURLOPT_HEADER         => false,
            CURLOPT_HTTP_VERSION   => CURL_HTTP_VERSION_1_1,
            CURLOPT_HEADERFUNCTION => static function ($handle, string $line) use (&$responseHeaders): int {
                $colon = strpos($line, ':');
                if ($colon !== false) {
                    $name                   = strtolower(trim(substr($line, 0, $colon)));
                    $responseHeaders[$name] = trim(substr($line, $colon + 1));
                }

                return strlen($line);
            },
        ];

        if (empty($this->cfg['use_system_proxy'])) {
            $set[CURLOPT_PROXY]   = '';
            $set[CURLOPT_NOPROXY] = '*';
        }

        $resolve = $opts['resolve'] ?? [];
        if (is_array($resolve) && $resolve !== []) {
            $set[CURLOPT_RESOLVE] = array_values($resolve);
        }

        curl_setopt_array($ch, $set);

        $body   = curl_exec($ch);
        $errno  = curl_errno($ch);
        $error  = curl_error($ch);
        $status = (int) curl_getinfo($ch, CURLINFO_RESPONSE_CODE);
        $final  = (string) curl_getinfo($ch, CURLINFO_EFFECTIVE_URL);
        curl_close($ch);

        if ($body === false) {
            return [
                'ok' => false, 'status' => 0, 'body' => null, 'headers' => $responseHeaders,
                'error' => 'curl(' . $errno . '): ' . $error, 'url' => $url, 'not_modified' => false,
            ];
        }

        $notModified = $status === 304;
        $ok          = $notModified || ($status >= 200 && $status < 300);

        return [
            'ok'           => $ok,
            'status'       => $status,
            'body'         => (string) $body,
            'headers'      => $responseHeaders,
            'error'        => $ok ? null : 'HTTP ' . $status,
            'url'          => $final !== '' ? $final : $url,
            'not_modified' => $notModified,
        ];
    }

    /**
     * 无 curl 扩展时的退化实现（stream wrapper）。
     */
    private function streamGet(string $url, array $headers, int $timeout): array
    {
        $context = stream_context_create([
            'http' => [
                'method'          => 'GET',
                'header'          => implode("\r\n", $headers),
                'timeout'         => $timeout,
                'ignore_errors'   => true,
                'follow_location' => 1,
                'max_redirects'   => (int) ($this->cfg['follow_location'] ?? 5),
                'user_agent'      => $this->userAgent,
            ],
            'ssl' => [
                'verify_peer'      => true,
                'verify_peer_name' => true,
            ],
        ]);

        $body = @file_get_contents($url, false, $context);

        $responseHeaders = [];
        $status          = 0;
        if (isset($http_response_header) && is_array($http_response_header)) {
            foreach ($http_response_header as $line) {
                if (preg_match('#^HTTP/\S+\s+(\d{3})#', $line, $m)) {
                    $status = (int) $m[1];
                    $responseHeaders = [];   // 重定向后只保留最后一跳
                    continue;
                }
                $colon = strpos($line, ':');
                if ($colon !== false) {
                    $responseHeaders[strtolower(trim(substr($line, 0, $colon)))] = trim(substr($line, $colon + 1));
                }
            }
        }

        if ($body === false) {
            return [
                'ok' => false, 'status' => $status, 'body' => null, 'headers' => $responseHeaders,
                'error' => 'stream 请求失败', 'url' => $url, 'not_modified' => false,
            ];
        }

        $notModified = $status === 304;
        $ok          = $notModified || ($status >= 200 && $status < 300);

        return [
            'ok'           => $ok,
            'status'       => $status,
            'body'         => $body,
            'headers'      => $responseHeaders,
            'error'        => $ok ? null : 'HTTP ' . $status,
            'url'          => $url,
            'not_modified' => $notModified,
        ];
    }

    /**
     * 强制 UTF-8：HTTP 头无 charset 时，客户端默认按 ISO-8859-1 解会乱码。
     * 这里直接按字节处理，只在确实不是合法 UTF-8 时按声明或常见中文编码纠正。
     */
    private function toUtf8(string $body, array $headers): string
    {
        if ($body === '' || !function_exists('mb_check_encoding')) {
            return $body;
        }
        if (mb_check_encoding($body, 'UTF-8')) {
            return $body;
        }

        $declared = null;
        if (isset($headers['content-type']) && preg_match('/charset\s*=\s*"?([a-z0-9_\-]+)"?/i', $headers['content-type'], $m)) {
            $declared = strtoupper($m[1]);
        }

        $candidates = [];
        if ($declared !== null && !in_array($declared, ['ISO-8859-1', 'LATIN1', 'US-ASCII'], true)) {
            $candidates[] = $declared;
        }
        $candidates[] = 'GB18030';
        $candidates[] = 'GBK';
        $candidates[] = 'BIG5';
        $candidates[] = 'UTF-8';

        foreach ($candidates as $encoding) {
            $converted = @mb_convert_encoding($body, 'UTF-8', $encoding);
            if (is_string($converted) && $converted !== '' && mb_check_encoding($converted, 'UTF-8')) {
                $this->log->warn('响应不是合法 UTF-8，已按 ' . $encoding . ' 转码', ['declared' => $declared]);

                return $converted;
            }
        }

        return $body;
    }
}
