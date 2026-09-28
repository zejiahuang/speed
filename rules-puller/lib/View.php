<?php

declare(strict_types=1);

namespace RulesPuller;

/**
 * 视图层：设计令牌 + 组件 + 页面骨架。
 *
 * 设计依据 `design/ADMIN_DESIGN.md`（Linear 浅色体系）：
 *   - 近白底、深灰字、发丝边框、单一强调色
 *   - 只用系统字体与内联 SVG，无任何外部资源
 *   - 强调色取 #0969DA，与既有的 status.php 保持同一套语言
 */
final class View
{
    private const NAV_ITEMS = [
        'overview' => '概览',
        'actions'  => '操作',
        'share'    => '中转站',
        'config'   => '配置',
        'logs'     => '日志',
        'audit'    => '审计',
        'security' => '安全',
    ];

    public static function e(?string $value): string
    {
        return htmlspecialchars((string) $value, ENT_QUOTES | ENT_SUBSTITUTE, 'UTF-8');
    }

    public static function humanBytes(int $bytes): string
    {
        return rp_human_bytes($bytes);
    }

    /**
     * 内联 SVG 图标。stroke 用 currentColor，颜色继承文字色。
     */
    public static function icon(string $name, int $size = 16): string
    {
        $paths = [
            'play'     => '<path d="M4 2.5v11l9-5.5-9-5.5z"/>',
            'refresh'  => '<path d="M13.5 8a5.5 5.5 0 1 1-1.6-3.9"/><path d="M13.5 1.5V5H10"/>',
            'trash'    => '<path d="M2.5 4h11M6 4V2.5h4V4M4 4l.7 9.5h6.6L12 4"/>',
            'home'     => '<path d="M2.5 7L8 2.5 13.5 7v6.5h-11z"/>',
            'list'     => '<path d="M3 4.5h10M3 8h10M3 11.5h10"/>',
            'sliders'  => '<path d="M3 5h10M3 11h10"/><circle cx="6" cy="5" r="1.6"/><circle cx="10" cy="11" r="1.6"/>',
            'file'     => '<path d="M4 2h5l3 3v9H4z"/><path d="M9 2v3h3"/>',
            'shield'   => '<path d="M8 2l5 2v4.5c0 3-2.1 5-5 5.5-2.9-.5-5-2.5-5-5.5V4z"/>',
            'download' => '<path d="M8 2v8"/><path d="M4.5 7L8 10.5 11.5 7"/><path d="M3 13h10"/>',
            'key'      => '<circle cx="5.5" cy="8" r="3"/><path d="M8.5 8H14M12 8v2.5"/>',
            'logout'   => '<path d="M6.5 3H3v10h3.5"/><path d="M9 5.5L11.5 8 9 10.5"/><path d="M11.5 8H6"/>',
            'warning'  => '<path d="M8 2.5l6 11H2z"/><path d="M8 6.5v3.2"/><path d="M8 11.6v.1"/>',
            'info'     => '<circle cx="8" cy="8" r="6"/><path d="M8 7v4.5"/><path d="M8 4.8v.1"/>',
            'lock'     => '<rect x="3.5" y="7" width="9" height="6.5" rx="1.5"/><path d="M5.5 7V5.2a2.5 2.5 0 0 1 5 0V7"/>',
            'check'    => '<path d="M3 8.5l3.2 3.2L13 4.5"/>',
        ];

        $body = $paths[$name] ?? $paths['info'];

        return '<svg width="' . $size . '" height="' . $size . '" viewBox="0 0 16 16" fill="none" '
            . 'stroke="currentColor" stroke-width="1.5" stroke-linecap="round" stroke-linejoin="round" '
            . 'aria-hidden="true">' . $body . '</svg>';
    }

    // ------------------------------------------------------------ 片段

    /**
     * 状态徽章。状态必须「文字 + 颜色」双通道。
     */
    public static function badge(string $kind, string $text): string
    {
        $allowed = ['ok', 'warn', 'error', 'info', 'muted'];
        $kind    = in_array($kind, $allowed, true) ? $kind : 'muted';
        $dot     = $kind === 'muted' ? 'muted' : $kind;

        return '<span class="badge badge--' . $kind . '"><span class="dot dot--' . $dot . '"></span>'
            . self::e($text) . '</span>';
    }

    public static function alert(string $kind, string $message, bool $rawHtml = false): string
    {
        $allowed = ['info', 'warn', 'error', 'success'];
        $kind    = in_array($kind, $allowed, true) ? $kind : 'info';
        $icon    = $kind === 'success' ? 'check' : ($kind === 'error' || $kind === 'warn' ? 'warning' : 'info');

        return '<div class="alert alert--' . $kind . '"><span class="alert__icon">' . self::icon($icon)
            . '</span><div>' . ($rawHtml ? $message : self::e($message)) . '</div></div>';
    }

    public static function metric(string $key, string $value, string $note = ''): string
    {
        return '<div class="card metric"><div class="metric__k">' . self::e($key) . '</div>'
            . '<div class="metric__v">' . self::e($value) . '</div>'
            . ($note !== '' ? '<div class="metric__n">' . self::e($note) . '</div>' : '')
            . '</div>';
    }

    public static function emptyRow(int $colspan, string $message): string
    {
        return '<tr><td colspan="' . $colspan . '" class="muted">' . self::e($message) . '</td></tr>';
    }

    // ------------------------------------------------------------ 页面骨架

    public static function nav(array $context): string
    {
        $current = (string) ($context['tab'] ?? 'overview');
        $links   = '';

        // 自定义导航（status.php 用），否则用后台的标签页
        if (!empty($context['nav_links']) && is_array($context['nav_links'])) {
            foreach ($context['nav_links'] as $item) {
                $active = !empty($item['active']) ? ' nav__link--active' : '';
                $links .= '<a class="nav__link' . $active . '" href="' . self::e((string) $item['href']) . '">'
                    . self::e((string) $item['label']) . '</a>';
            }
        } else {
            foreach (self::NAV_ITEMS as $key => $label) {
                $active = $key === $current ? ' nav__link--active' : '';
                $links .= '<a class="nav__link' . $active . '" href="?tab=' . self::e($key) . '">' . self::e($label) . '</a>';
            }
        }

        $meta = '';
        if (!empty($context['nav_meta'])) {
            $meta = '<span class="nav__meta">' . self::e((string) $context['nav_meta']) . '</span>';
        }

        $logout = !empty($context['show_logout'])
            ? '<a class="btn btn--ghost btn--sm" href="?action=logout">' . self::icon('logout') . '<span>退出</span></a>'
            : '';

        return '<nav class="nav"><span class="nav__brand">rules-puller</span>' . $links
            . '<span class="nav__spacer"></span>' . $meta . $logout . '</nav>';
    }

    /**
     * 完整页面。
     */
    public static function page(string $title, string $bodyHtml, array $context = []): string
    {
        $nav = isset($context['no_nav']) && $context['no_nav'] ? '' : self::nav($context);
        $sub = '';
        if (!empty($context['subtitle'])) {
            $sub = '<p class="page__sub">' . self::e((string) $context['subtitle']) . '</p>';
        }

        return self::documentHead($title) . $nav
            . '<main class="page"><h1 class="page__title">' . self::e($title) . '</h1>' . $sub
            . self::flash($context) . $bodyHtml . '</main>' . self::documentFoot();
    }

    /**
     * 一次性提示（存 session，读后即清）。
     */
    private static function flash(array $context): string
    {
        $out = '';
        foreach ((array) ($context['flash'] ?? []) as $item) {
            $out .= self::alert((string) $item['kind'], (string) $item['message']) . "\n";
        }
        if ($out !== '') {
            return '<div class="stack">' . $out . '</div>';
        }

        return '';
    }

    /**
     * 登录页（也用于「首次设置密码」）。
     */
    public static function loginPage(array $context): string
    {
        $error   = $context['error'] ?? null;
        $setup   = !empty($context['setup']);
        $title   = $setup ? '设置管理员密码' : 'rules-puller 管理后台';
        $sub     = $setup
            ? '首次使用，请先设置一个管理员密码（至少 8 位）。设置完成后本机将直接进入后台。'
            : '请输入管理员密码。连续输错会被暂时锁定。';
        $action  = $setup ? 'setup' : 'login';
        $token   = self::e((string) ($context['csrf'] ?? ''));

        $fields = $setup
            ? '<label class="field"><span class="field__label">新密码</span>'
                . '<input class="input" type="password" name="password" autocomplete="new-password" autofocus required></label>'
                . '<label class="field"><span class="field__label">再输一次</span>'
                . '<input class="input" type="password" name="password2" autocomplete="new-password" required></label>'
            : '<label class="field"><span class="field__label">密码</span>'
                . '<input class="input" type="password" name="password" autocomplete="current-password" autofocus required></label>';

        $button = $setup ? '设置并登录' : '登录';

        $body = '<div class="login-wrap"><div class="login-card">'
            . '<h1 class="login-card__title">' . self::e($title) . '</h1>'
            . '<p class="login-card__sub">' . self::e($sub) . '</p>'
            . ($error !== null ? self::alert('error', (string) $error) . '<div class="sp-4"></div>' : '')
            . '<form method="post" action="">'
            . '<input type="hidden" name="action" value="' . $action . '">'
            . '<input type="hidden" name="_csrf" value="' . $token . '">'
            . $fields
            . '<button class="btn btn--primary btn--lg btn--block" type="submit">' . self::e($button) . '</button>'
            . '</form>'
            . '<p class="login-card__foot">来源 IP：' . self::e((string) ($context['ip'] ?? '未知')) . '</p>'
            . '</div></div>';

        return self::documentHead($title) . $body . self::documentFoot();
    }

    /**
     * 拒绝页（IP 白名单 / 未配置）。
     */
    public static function deniedPage(int $status, string $title, string $message): string
    {
        http_response_code($status);

        $body = '<div class="login-wrap"><div class="login-card">'
            . '<h1 class="login-card__title">' . self::e($title) . '</h1>'
            . '<p class="login-card__sub">' . self::e($message) . '</p>'
            . '</div></div>';

        return self::documentHead($title) . $body . self::documentFoot();
    }

    // ------------------------------------------------------------ 文档头尾

    public static function documentHead(string $title): string
    {
        return "<!DOCTYPE html>\n<html lang=\"zh-CN\">\n<head>\n"
            . "<meta charset=\"UTF-8\">\n"
            . "<meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">\n"
            . "<meta name=\"robots\" content=\"noindex, nofollow\">\n"
            . '<title>' . self::e($title) . "</title>\n<style>\n" . self::css() . "</style>\n</head>\n<body>\n";
    }

    public static function documentFoot(): string
    {
        return "\n</body>\n</html>\n";
    }

    /**
     * 全部样式。令牌与组件取自 design/ADMIN_DESIGN.md。
     */
    private static function css(): string
    {
        return <<<'CSS'
:root {
  --bg-page: #F6F7F9;
  --bg-surface: #FFFFFF;
  --bg-subtle: #F1F3F5;
  --bg-inset: #F6F8FA;
  --border: #D8DEE4;
  --border-muted: #E8ECF0;
  --border-strong: #C3CAD3;

  --text-1: #1F2328;
  --text-2: #59636E;
  --text-3: #6E7781;
  --text-disabled: #8C959F;
  --text-on-accent: #FFFFFF;

  --ok-fg: #1A7F37;  --ok-bg: #DAFBE1;  --ok-border: #ACE5BE;
  --warn-fg: #9A6700; --warn-bg: #FFF6D5; --warn-border: #EAC54F;
  --danger-fg: #CF222E; --danger-bg: #FFEBE9; --danger-border: #FFC1BC;
  --danger-solid: #CF222E; --danger-solid-hover: #A40E26;
  --info-fg: #0969DA; --info-bg: #DDF4FF; --info-border: #A5D6FF;

  --log-info: #59636E; --log-warn: #9A6700; --log-error: #CF222E; --log-debug: #6E7781;

  --accent: #0969DA;
  --accent-hover: #0550AE;
  --accent-active: #033D8B;
  --accent-soft: #DDF4FF;
  --accent-focus: rgba(9,105,218,.35);

  --font-sans: -apple-system, BlinkMacSystemFont, "Segoe UI", "PingFang SC",
    "Hiragino Sans GB", "Microsoft YaHei", "微软雅黑", "Noto Sans CJK SC",
    "Source Han Sans SC", Roboto, "Helvetica Neue", Arial, sans-serif;
  --font-mono: ui-monospace, SFMono-Regular, "SF Mono", "Cascadia Mono",
    "JetBrains Mono", Consolas, "Liberation Mono", Menlo, "Courier New",
    "PingFang SC", "Microsoft YaHei", monospace;

  --fs-xs: 12px; --fs-sm: 13px; --fs-base: 14px; --fs-md: 15px;
  --fs-lg: 16px; --fs-xl: 24px; --fs-2xl: 22px; --fs-log: 12.5px;

  --sp-1: 4px; --sp-2: 8px; --sp-3: 12px; --sp-4: 16px;
  --sp-5: 20px; --sp-6: 24px; --sp-8: 32px; --sp-12: 48px;

  --radius-xs: 4px; --radius-sm: 6px; --radius-md: 8px; --radius-lg: 10px;
  --radius-pill: 999px; --bw: 1px; --bw-accent: 2px;

  --shadow-xs: 0 1px 1px rgba(31,35,40,.04);
  --shadow-sm: 0 1px 2px rgba(31,35,40,.06), 0 1px 1px rgba(31,35,40,.04);
  --shadow-md: 0 3px 6px rgba(31,35,40,.08);
  --shadow-lg: 0 8px 24px rgba(31,35,40,.12);
  --focus-ring: 0 0 0 3px var(--accent-focus);

  --control-h: 34px; --control-h-sm: 28px; --control-h-lg: 38px;
  --table-row-h: 36px; --table-head-h: 32px; --nav-h: 48px;

  --z-nav: 100; --z-backdrop: 300; --z-modal: 310;
}

* { box-sizing: border-box; }
body {
  margin: 0; background: var(--bg-page); color: var(--text-1);
  font: 400 var(--fs-base)/1.55 var(--font-sans);
  -webkit-font-smoothing: antialiased;
}
a { color: var(--accent); text-decoration: none; }
a:hover { color: var(--accent-hover); text-decoration: underline; }
.mono { font-family: var(--font-mono); font-size: var(--fs-xs); }
.muted { color: var(--text-3); }
.num { text-align: right; font-variant-numeric: tabular-nums; }
.sp-4 { height: var(--sp-4); }
.nowrap { white-space: nowrap; }

/* ---------------------------------------------------------------- 导航 */
.nav {
  height: var(--nav-h); background: var(--bg-surface);
  border-bottom: var(--bw) solid var(--border);
  display: flex; align-items: center; gap: var(--sp-4);
  padding: 0 var(--sp-5); position: sticky; top: 0; z-index: var(--z-nav);
  overflow-x: auto;
}
.nav__brand { font: 650 var(--fs-md)/1 var(--font-sans); color: var(--text-1); white-space: nowrap; }
.nav__link {
  height: var(--nav-h); display: inline-flex; align-items: center; white-space: nowrap;
  font-size: var(--fs-base); color: var(--text-2); text-decoration: none;
  border-bottom: var(--bw-accent) solid transparent;
}
.nav__link:hover { color: var(--text-1); text-decoration: none; }
.nav__link--active { color: var(--accent); border-bottom-color: var(--accent); }
.nav__spacer { margin-left: auto; }
.nav__meta { font: 400 var(--fs-xs)/1 var(--font-mono); color: var(--text-3); white-space: nowrap; }

/* ---------------------------------------------------------------- 布局 */
.page { max-width: 1120px; margin: 0 auto; padding: var(--sp-6) var(--sp-5) var(--sp-12); }
.page__title { font: 650 var(--fs-2xl)/1.3 var(--font-sans); margin: 0 0 4px; }
.page__sub { color: var(--text-2); font-size: var(--fs-sm); margin: 0 0 var(--sp-6); }
.grid { display: grid; gap: var(--sp-4); }
.grid--metrics { grid-template-columns: repeat(4, minmax(0,1fr)); }
.grid--2 { grid-template-columns: repeat(2, minmax(0,1fr)); }
.grid--split { grid-template-columns: minmax(0,2fr) minmax(0,1fr); }
.stack { display: flex; flex-direction: column; gap: var(--sp-3); margin-bottom: var(--sp-4); }
section.block { margin-bottom: var(--sp-6); }
section.block > h2 {
  font: 600 var(--fs-lg)/1.4 var(--font-sans); margin: 0 0 var(--sp-3);
  padding-bottom: var(--sp-2); border-bottom: var(--bw) solid var(--border-muted);
}
.row { display: flex; gap: var(--sp-2); align-items: center; flex-wrap: wrap; }

/* ---------------------------------------------------------------- 卡片 */
.card {
  background: var(--bg-surface); border: var(--bw) solid var(--border);
  border-radius: var(--radius-md); padding: var(--sp-4) 18px;
}
.card--table { padding: 0; overflow: hidden; }
.metric__k { font-size: var(--fs-xs); color: var(--text-2); letter-spacing: .02em; }
.metric__v {
  font: 600 var(--fs-xl)/1.15 var(--font-sans); color: var(--text-1);
  margin-top: 2px; font-variant-numeric: tabular-nums;
}
.metric__n { font-size: var(--fs-xs); color: var(--text-3); margin-top: 2px; }

/* ---------------------------------------------------------------- 按钮 */
.btn {
  display: inline-flex; align-items: center; gap: var(--sp-2);
  height: var(--control-h); padding: 0 var(--sp-4);
  font: 500 var(--fs-md)/1 var(--font-sans);
  border: var(--bw) solid transparent; border-radius: var(--radius-sm);
  cursor: pointer; user-select: none; white-space: nowrap; text-decoration: none;
  transition: background-color .12s, border-color .12s, color .12s;
}
.btn:hover { text-decoration: none; }
.btn:focus-visible { outline: none; box-shadow: var(--focus-ring); }
.btn--primary { background: var(--accent); color: var(--text-on-accent); }
.btn--primary:hover { background: var(--accent-hover); color: var(--text-on-accent); }
.btn--primary:active { background: var(--accent-active); }
.btn--secondary { background: var(--bg-surface); color: var(--text-1); border-color: var(--border-strong); }
.btn--secondary:hover { background: var(--bg-subtle); color: var(--text-1); }
.btn--danger { background: var(--danger-solid); color: #fff; }
.btn--danger:hover { background: var(--danger-solid-hover); color: #fff; }
.btn--ghost { background: transparent; color: var(--text-2); }
.btn--ghost:hover { background: var(--bg-subtle); color: var(--text-1); }
.btn--sm { height: var(--control-h-sm); padding: 0 var(--sp-3); font-size: var(--fs-sm); }
.btn--lg { height: var(--control-h-lg); padding: 0 var(--sp-5); }
.btn--block { width: 100%; justify-content: center; }
.btn:disabled, .btn[aria-disabled="true"] { opacity: .5; cursor: not-allowed; pointer-events: none; }

/* ---------------------------------------------------------------- 表单 */
.input, .textarea {
  width: 100%; min-height: var(--control-h); padding: 6px var(--sp-3);
  font: 400 var(--fs-base)/1.5 var(--font-sans); color: var(--text-1);
  background: var(--bg-surface);
  border: var(--bw) solid var(--border-strong); border-radius: var(--radius-sm);
  transition: border-color .12s, box-shadow .12s;
}
.input:focus, .textarea:focus { outline: none; border-color: var(--accent); box-shadow: var(--focus-ring); }
.input[disabled], .textarea[disabled] { background: var(--bg-subtle); color: var(--text-disabled); cursor: not-allowed; }
.textarea { min-height: 84px; resize: vertical; font-family: var(--font-mono); font-size: var(--fs-sm); }
.input--num { text-align: right; font-variant-numeric: tabular-nums; max-width: 160px; }
.field { display: block; margin-bottom: var(--sp-5); }
.field__label { display: block; font: 500 var(--fs-md)/1.4 var(--font-sans); color: var(--text-1); margin-bottom: 5px; }
.field__help { margin-top: 4px; font-size: var(--fs-sm); color: var(--text-2); }
.field__meta { margin-top: 5px; font-size: var(--fs-xs); color: var(--text-3); font-variant-numeric: tabular-nums; }
.field__meta .cur { color: var(--text-1); font-weight: 500; }
.field__meta .def { color: var(--text-3); }
.field__meta .diff { color: var(--warn-fg); font-weight: 500; }
.switch { display: inline-flex; align-items: center; gap: var(--sp-2); cursor: pointer; }
.switch input { position: absolute; opacity: 0; width: 0; height: 0; }
.switch__track {
  width: 36px; height: 20px; border-radius: var(--radius-pill);
  background: var(--border-strong); position: relative; transition: background-color .15s;
}
.switch__track::after {
  content: ""; position: absolute; top: 2px; left: 2px;
  width: 16px; height: 16px; border-radius: 50%; background: #fff;
  box-shadow: var(--shadow-xs); transition: transform .15s;
}
.switch input:checked + .switch__track { background: var(--accent); }
.switch input:checked + .switch__track::after { transform: translateX(16px); }
.switch input:focus-visible + .switch__track { box-shadow: var(--focus-ring); }

/* ---------------------------------------------------------------- 徽章 */
.badge {
  display: inline-flex; align-items: center; gap: 5px;
  padding: 1px var(--sp-2); border-radius: var(--radius-pill);
  font: 500 var(--fs-xs)/1.6 var(--font-sans);
  border: var(--bw) solid transparent; white-space: nowrap;
}
.badge--ok { color: var(--ok-fg); background: var(--ok-bg); border-color: var(--ok-border); }
.badge--warn { color: var(--warn-fg); background: var(--warn-bg); border-color: var(--warn-border); }
.badge--error { color: var(--danger-fg); background: var(--danger-bg); border-color: var(--danger-border); }
.badge--info { color: var(--info-fg); background: var(--info-bg); border-color: var(--info-border); }
.badge--muted { color: var(--text-2); background: var(--bg-subtle); border-color: var(--border-muted); }
.dot { width: 7px; height: 7px; border-radius: 50%; flex: none; display: inline-block; }
.dot--ok { background: var(--ok-fg); }
.dot--warn { background: var(--warn-fg); }
.dot--error { background: var(--danger-fg); }
.dot--muted { background: var(--text-3); }

/* ---------------------------------------------------------------- 表格 */
.table-wrap { overflow-x: auto; }
.table { width: 100%; border-collapse: collapse; font-size: var(--fs-sm); }
.table th {
  height: var(--table-head-h); padding: 0 var(--sp-3); text-align: left;
  font-weight: 500; color: var(--text-2); background: var(--bg-subtle);
  border-bottom: var(--bw) solid var(--border); white-space: nowrap;
}
.table td {
  height: var(--table-row-h); padding: 6px var(--sp-3);
  border-bottom: var(--bw) solid var(--border-muted); vertical-align: middle;
}
.table tbody tr:last-child td { border-bottom: 0; }
.table tbody tr:hover td { background: var(--bg-subtle); }
.table td.mono { font-family: var(--font-mono); font-size: var(--fs-xs); color: var(--text-2); }

/* ---------------------------------------------------------------- 提示条 */
.alert {
  display: flex; gap: var(--sp-3); align-items: flex-start;
  padding: var(--sp-3) var(--sp-4);
  border: var(--bw) solid; border-radius: var(--radius-sm);
  font-size: var(--fs-sm); line-height: 1.55;
}
.alert__icon { flex: none; margin-top: 1px; }
.alert--info { color: var(--text-1); background: var(--info-bg); border-color: var(--info-border); }
.alert--warn { color: var(--text-1); background: var(--warn-bg); border-color: var(--warn-border); }
.alert--error { color: var(--text-1); background: var(--danger-bg); border-color: var(--danger-border); border-left-width: 3px; border-left-color: var(--danger-fg); }
.alert--success { color: var(--text-1); background: var(--ok-bg); border-color: var(--ok-border); }

/* ---------------------------------------------------------------- 日志 */
.log {
  background: var(--bg-inset); border: var(--bw) solid var(--border);
  border-radius: var(--radius-md); padding: var(--sp-3) var(--sp-4);
  font: 400 var(--fs-log)/1.6 var(--font-mono);
  overflow: auto; max-height: 460px; white-space: pre; tab-size: 4;
}
.log__line { display: block; }
.log--info { color: var(--log-info); }
.log--warn { color: var(--log-warn); }
.log--error { color: var(--log-error); font-weight: 500; }
.log--debug { color: var(--log-debug); }

/* ---------------------------------------------------------------- 登录 */
.login-wrap { min-height: 100vh; display: flex; align-items: center; justify-content: center; padding: var(--sp-4); }
.login-card {
  width: 100%; max-width: 380px; background: var(--bg-surface);
  border: var(--bw) solid var(--border); border-radius: var(--radius-lg);
  box-shadow: var(--shadow-sm); padding: var(--sp-8) var(--sp-6);
}
.login-card__title { font: 650 var(--fs-2xl)/1.3 var(--font-sans); margin: 0 0 4px; }
.login-card__sub { font-size: var(--fs-sm); color: var(--text-2); margin: 0 0 var(--sp-6); }
.login-card__foot { font-size: var(--fs-xs); color: var(--text-3); margin: var(--sp-5) 0 0; }

/* ---------------------------------------------------------------- 响应式 */
@media (max-width: 1023px) {
  .grid--metrics { grid-template-columns: repeat(2, minmax(0,1fr)); }
  .grid--split, .grid--2 { grid-template-columns: minmax(0,1fr); }
}
@media (max-width: 640px) {
  .grid--metrics { grid-template-columns: 1fr; }
  .page { padding: var(--sp-4) var(--sp-3) var(--sp-10); }
  .nav { gap: var(--sp-3); padding: 0 var(--sp-3); }
}
CSS;
    }
}
