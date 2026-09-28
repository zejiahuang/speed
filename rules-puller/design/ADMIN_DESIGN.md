# rules-puller · 管理控制台视觉规范（ADMIN_DESIGN.md）

> 面向 `admin.php` 及访问限制层的一份**可直接落地**的视觉规范。
> 约束前提：纯手写 HTML + 内联 `<style>`、无构建、无外部资源、浅色主题、系统字体、开发者/运维受众。
> 本文件只给规范与 CSS 骨架，不含任何 HTML 原型。

---

## 0. 设计系统选择

### 选定：**Linear**（开发工具类）

从知识库 71 套系统中，最贴合「开发者工具 / 运维控制台」的是 **Linear**。理由（全部落到本场景）：

1. **它就是为"开发者控制台"设计的**。Linear 的原生模式里本来就有：状态点、密集表格、等宽代码块、命令式操作区、分段控件——正是本控制台要覆盖的 7 个界面所需，不用东拼西凑。
2. **浅色主题天然克制**。Linear 的浅色语言 = 近白面板 + 发丝级边框 + 单一强调色 + 灰阶文字，**不靠大留白和光效**，天生适合信息密集的运维界面。
3. **与现有 `status.php` 无缝衔接**。`status.php` 已经用了 Primer 式的中性灰阶（`#f6f7f9 / #ffffff / #e3e6ea / #1f2328 / #656d76`）。Linear 的灰阶与之一脉相承，`admin.php` 和状态面板放在一起不会"两个产品"。**本规范直接继承并系统化了这套中性色**，只把强调色升级为 Linear 靛蓝。
4. **字体无关**。Linear 官方用 Inter，但它的观感 90% 来自**灰阶 + 边框 + 间距纪律**而非字体。退化到系统 sans 栈几乎无损——这正好满足"不能引外部字体"的硬约束。
5. **受众匹配**。Linear 的视觉性格是"精确、专业、不喧哗"，与运维受众一致；不像营销页那样夸张。

**对比过的其它候选**：

| 方案 | 系统 | 为何不选为主 |
|------|------|-------------|
| B | Sentry | 运维可观测领域更贴题，但品牌紫在"中性控制台"里偏跳，且其卡片/图表依赖较多自定义样式 |
| C | Vercel | 黑白过硬、对比过强，且视觉识别高度依赖 Geist 字体（无法加载） |
| 兜底 | Default (Neutral Modern) | 安全但无性格；本场景有明确偏好，无需退到兜底 |

**强调色决策**：默认用 Linear 靛蓝 `#5E6AD2`。若要与现有 `status.php` 的蓝色**完全统一**，把 §2.5 的 accent 三件套整体替换为 `#0969DA / #0550AE / rgba(9,105,218,.35)` 即可，其余令牌不变。

---

## 1. Visual Theme（视觉主题）

- **Philosophy**：像仪表盘一样组织信息——每一像素都为"快速读状态、快速做决定"服务。
- **Direction**：`utilitarian, data-dense, precise, low-chrome`（克制装饰，功能至上）。
- **Personality**：精确、可信、冷静。不讨好，不喧哗。
- **Reference**：Linear 浅色主题 + GitHub Primer 的中性灰阶（继承自现有 `status.php`）。
- **一句话基调**：**近白底、深灰字、发丝边框、一个强调色**。除了语义色（成功/警告/危险/信息），画面上不允许出现第五种颜色。

---

## 2. Color Palette（设计令牌 · 颜色）

> 全部为十六进制，**不使用 `oklch()`**，兼容老浏览器。语义色每个都给「前景 / 背景 / 边框」三态。
> 对比度标注基于 WCAG 2.1，正文阈值 4.5:1。全部通过 AA。

### 2.1 中性色阶（底色 / 面板 / 边框）

| Token | HEX | 用途 |
|-------|-----|------|
| `--bg-page` | `#F6F7F9` | 页面底色（略灰，衬托白色面板） |
| `--bg-surface` | `#FFFFFF` | 卡片 / 面板 / 表格容器底 |
| `--bg-subtle` | `#F1F3F5` | 表头、行 hover、次级填充 |
| `--bg-inset` | `#F6F8FA` | 代码块 / 日志窗口 / 只读区底 |
| `--border` | `#D8DEE4` | 默认边框、分隔线 |
| `--border-muted` | `#E8ECF0` | 次级边框、表格行内分隔 |
| `--border-strong` | `#C3CAD3` | 输入框描边、需要更实的边界 |

### 2.2 文字色阶

| Token | HEX | 对比度(白底) | 用途 |
|-------|-----|-------------|------|
| `--text-1` | `#1F2328` | ≈15.9:1 | 正文、标题、主信息 |
| `--text-2` | `#59636E` | ≈6.1:1 | 次级说明、表头、元信息 |
| `--text-3` | `#6E7781` | ≈4.55:1 | 弱提示、占位、时间戳（不承载关键信息） |
| `--text-disabled` | `#8C959F` | ≈3.1:1 | 禁用态文字（允许低于 AA，因不可交互） |
| `--text-on-accent` | `#FFFFFF` | — | 强调色实心按钮上的文字 |

### 2.3 语义色（成功 / 警告 / 危险 / 信息）

每态三件套：`fg`（文字/图标）、`bg`（浅底）、`border`（描边）。另有 `solid` 用于需要实心块的场合（如危险按钮）。

| 语义 | Token | HEX | 用途 |
|------|-------|-----|------|
| 成功 | `--ok-fg` | `#1A7F37` | 徽章文字、状态点、正常态（白底 5.1:1） |
| 成功 | `--ok-bg` | `#DAFBE1` | 徽章底、提示条底 |
| 成功 | `--ok-border` | `#ACE5BE` | 徽章/提示条描边 |
| 成功 | `--ok-solid` | `#1F883D` | 需要实心时的绿（极少用） |
| 警告 | `--warn-fg` | `#9A6700` | 警告文字（白底 4.9:1） |
| 警告 | `--warn-bg` | `#FFF6D5` | 警告底 |
| 警告 | `--warn-border` | `#EAC54F` | 警告描边 |
| 危险 | `--danger-fg` | `#CF222E` | 错误文字（白底 5.4:1） |
| 危险 | `--danger-bg` | `#FFEBE9` | 错误底 |
| 危险 | `--danger-border` | `#FFC1BC` | 错误描边 |
| 危险 | `--danger-solid` | `#CF222E` | 危险实心按钮底 |
| 危险 | `--danger-solid-hover` | `#A40E26` | 危险按钮 hover |
| 信息 | `--info-fg` | `#0969DA` | 信息文字（白底 5.2:1） |
| 信息 | `--info-bg` | `#DDF4FF` | 信息底 |
| 信息 | `--info-border` | `#A5D6FF` | 信息描边 |

> 规则：**语义色只用于传达状态，绝不用于装饰**。同一个界面里语义色出现的面积应 < 10%。

### 2.4 日志级别着色（浅底深字）

| 级别 | 颜色 | Token |
|------|------|-------|
| INFO | `#59636E`（次级灰） | `--log-info` |
| WARN | `#9A6700`（警告琥珀） | `--log-warn` |
| ERROR | `#CF222E`（危险红） | `--log-error` |
| DEBUG | `#6E7781`（弱灰） | `--log-debug` |

> 日志窗口保持**浅底**（`--bg-inset`），不采用深色终端面板——遵守"全站浅色"的硬约束。

### 2.5 强调色（主按钮 / 链接 / 焦点环）

| Token | HEX | 用途 |
|-------|-----|------|
| `--accent` | `#5E6AD2` | 主按钮底、链接、激活指示（白底 4.7:1；白字 4.7:1，AA 达标） |
| `--accent-hover` | `#4F5AC0` | 主按钮 hover / 链接 hover |
| `--accent-active` | `#434DAB` | 按下态 |
| `--accent-soft` | `#EEF0FB` | 选中行、当前导航项浅底 |
| `--accent-focus` | `rgba(94,106,210,.35)` | 焦点环（用 box-shadow 呈现，非彩色阴影） |

> 备选（对齐现有 `status.php`）：`--accent: #0969DA`、`--accent-hover: #0550AE`、`--accent-focus: rgba(9,105,218,.35)`。

---

## 3. Typography（排版）

### 3.1 字体栈（仅系统字体，含中文回退）

```css
--font-sans:
  -apple-system, BlinkMacSystemFont, "Segoe UI", "PingFang SC",
  "Hiragino Sans GB", "Microsoft YaHei", "微软雅黑",
  "Noto Sans CJK SC", "Source Han Sans SC", Roboto, "Helvetica Neue",
  Arial, sans-serif;

--font-mono:
  ui-monospace, SFMono-Regular, "SF Mono", "Cascadia Mono",
  "JetBrains Mono", Consolas, "Liberation Mono", Menlo,
  "Courier New", "PingFang SC", "Microsoft YaHei", monospace;
```

> **关键点**：等宽栈末尾要挂中文字体（`PingFang SC` / `Microsoft YaHei`）。否则日志里混排的中文会掉到某个丑陋的兜底字体，等宽窗口会"花"。

### 3.2 字号阶梯（基准 14px，面向密度）

| 层级 | Token | Size | Weight | Line-height | 用途 |
|------|-------|------|--------|-------------|------|
| 页标题 | `--fs-2xl` | 22px | 650 | 1.3 | 页面主标题 |
| 指标数字 | `--fs-xl` | 24px | 600 | 1.15 | 概览卡大数字 |
| 区块标题 | `--fs-lg` | 16px | 600 | 1.4 | 卡片/分区标题 |
| 正文强调 | `--fs-md` | 15px | 500 | 1.5 | 表单标签、按钮 |
| 正文 | `--fs-base` | 14px | 400 | 1.55 | 默认正文 |
| 表格/次要 | `--fs-sm` | 13px | 400 | 1.5 | 表格正文、说明文字 |
| 微字 | `--fs-xs` | 12px | 500 | 1.4 | 徽章、表头、时间戳 |
| 日志 | `--fs-log` | 12.5px | 400 | 1.6 | 等宽日志窗口 |

- **字重只用四档**：`400`（正文）/ `500`（标签、按钮）/ `600`（标题、数字）/ `650`（页标题）。不要用 700/800。
- 数字统一加 `font-variant-numeric: tabular-nums;`（指标卡、表格数值、耗时、大小），保证纵向对齐。
- 中文字号建议 ≥ 13px；12px 只用于拉丁字母为主的徽章/表头。

---

## 4. 尺寸与结构令牌（间距 / 圆角 / 边框 / 阴影 / 控件）

### 4.1 间距阶梯（4px 基准）

```css
--sp-1: 4px;  --sp-2: 8px;  --sp-3: 12px; --sp-4: 16px;
--sp-5: 20px; --sp-6: 24px; --sp-8: 32px; --sp-10: 40px; --sp-12: 48px;
```

- 卡片内边距：`16px 18px`（紧凑）或 `20px 22px`（常规）。
- 卡片之间：`16px`。区块之间：`24px`。

### 4.2 圆角

```css
--radius-xs: 4px;   /* 徽章、状态点容器 */
--radius-sm: 6px;   /* 按钮、输入框、提示条 */
--radius-md: 8px;   /* 卡片、面板、日志窗口 */
--radius-lg: 10px;  /* 登录卡、模态 */
--radius-pill: 999px;
```

### 4.3 边框宽度

```css
--bw: 1px;          /* 一切默认边框 */
--bw-accent: 2px;   /* 导航激活下划线 */
```
> 焦点态**不用**加粗边框，改用 `box-shadow` 焦点环（见 4.4），避免布局抖动。

### 4.4 阴影（克制，一律中性黑，禁止彩色阴影）

```css
--shadow-xs:  0 1px 1px rgba(31,35,40,.04);
--shadow-sm:  0 1px 2px rgba(31,35,40,.06), 0 1px 1px rgba(31,35,40,.04);
--shadow-md:  0 3px 6px rgba(31,35,40,.08);
--shadow-lg:  0 8px 24px rgba(31,35,40,.12);   /* 仅模态 */
--focus-ring: 0 0 0 3px var(--accent-focus);
```

### 4.5 控件尺寸

```css
--control-h: 34px;      /* 输入框 / 按钮默认高 */
--control-h-sm: 28px;   /* 表格内小按钮 */
--control-h-lg: 38px;   /* 登录页输入 / 主操作 */
--table-row-h: 36px;    /* 表格数据行 */
--table-head-h: 32px;   /* 表头行 */
--nav-h: 48px;          /* 顶部导航条 */
```

---

## 5. Component Styles（组件规范 + CSS 骨架）

> 所有骨架均以 §9 的 `:root` 令牌为前提，可直接复制使用。

### 5.1 按钮（主 / 次 / 危险 / 幽灵，四态含 disabled）

```css
.btn {
  display: inline-flex; align-items: center; gap: var(--sp-2);
  height: var(--control-h); padding: 0 var(--sp-4);
  font: 500 var(--fs-md)/1 var(--font-sans);
  border: var(--bw) solid transparent; border-radius: var(--radius-sm);
  cursor: pointer; user-select: none; white-space: nowrap;
  transition: background-color .12s, border-color .12s, color .12s;
}
.btn:focus-visible { outline: none; box-shadow: var(--focus-ring); }

/* 主：唯一实心强调色，一屏最多一个 */
.btn--primary { background: var(--accent); color: var(--text-on-accent); }
.btn--primary:hover { background: var(--accent-hover); }
.btn--primary:active { background: var(--accent-active); }

/* 次：白底 + 边框，最常用 */
.btn--secondary {
  background: var(--bg-surface); color: var(--text-1);
  border-color: var(--border-strong);
}
.btn--secondary:hover { background: var(--bg-subtle); }
.btn--secondary:active { background: #E9EDF1; }

/* 危险：仅用于破坏性操作（清理缓存 / 删除） */
.btn--danger { background: var(--danger-solid); color: #fff; }
.btn--danger:hover { background: var(--danger-solid-hover); }

/* 幽灵：透明底，用于低优先操作 */
.btn--ghost { background: transparent; color: var(--text-2); }
.btn--ghost:hover { background: var(--bg-subtle); color: var(--text-1); }

/* 尺寸修饰 */
.btn--sm { height: var(--control-h-sm); padding: 0 var(--sp-3); font-size: var(--fs-sm); }
.btn--lg { height: var(--control-h-lg); padding: 0 var(--sp-5); }

/* 禁用：统一降透明 + 禁止指针，不换色 */
.btn:disabled, .btn[aria-disabled="true"] {
  opacity: .5; cursor: not-allowed; pointer-events: none;
}
```

> 危险按钮在视觉上**不得**比主按钮更"亮眼"（同尺寸同字重），避免误点。

### 5.2 输入框 / 文本域 / 下拉

```css
.input, .textarea, .select {
  width: 100%; min-height: var(--control-h);
  padding: 6px var(--sp-3);
  font: 400 var(--fs-base)/1.5 var(--font-sans); color: var(--text-1);
  background: var(--bg-surface);
  border: var(--bw) solid var(--border-strong); border-radius: var(--radius-sm);
  transition: border-color .12s, box-shadow .12s;
}
.input::placeholder { color: var(--text-3); }
.input:focus, .textarea:focus, .select:focus {
  outline: none; border-color: var(--accent); box-shadow: var(--focus-ring);
}
.input[disabled], .textarea[disabled] {
  background: var(--bg-subtle); color: var(--text-disabled); cursor: not-allowed;
}
.input--error { border-color: var(--danger-fg); }
.input--error:focus { box-shadow: 0 0 0 3px rgba(207,34,46,.25); }
.textarea { min-height: 96px; resize: vertical; font-family: var(--font-mono); font-size: var(--fs-sm); }

/* 数字输入：右对齐 + 等宽数字 */
.input--num { text-align: right; font-variant-numeric: tabular-nums; }
```

**字段说明与默认值对比**（配置表单用）：

```css
.field { margin-bottom: var(--sp-5); }
.field__label { font: 500 var(--fs-md)/1.4 var(--font-sans); color: var(--text-1); }
.field__help { margin-top: 4px; font-size: var(--fs-sm); color: var(--text-2); }
.field__meta { margin-top: 6px; font-size: var(--fs-xs); color: var(--text-3); font-variant-numeric: tabular-nums; }
.field__meta .cur { color: var(--text-1); font-weight: 500; }   /* 当前值 */
.field__meta .def { color: var(--text-3); }                     /* 默认值 */
.field__meta .diff { color: var(--warn-fg); font-weight: 500; } /* 与默认值不同时高亮 */
```

### 5.3 开关（Switch）

```css
.switch { display: inline-flex; align-items: center; gap: var(--sp-2); cursor: pointer; }
.switch input { position: absolute; opacity: 0; width: 0; height: 0; }
.switch__track {
  width: 36px; height: 20px; border-radius: var(--radius-pill);
  background: var(--border-strong); position: relative;
  transition: background-color .15s;
}
.switch__track::after {
  content: ""; position: absolute; top: 2px; left: 2px;
  width: 16px; height: 16px; border-radius: 50%; background: #fff;
  box-shadow: var(--shadow-xs); transition: transform .15s;
}
.switch input:checked + .switch__track { background: var(--accent); }
.switch input:checked + .switch__track::after { transform: translateX(16px); }
.switch input:focus-visible + .switch__track { box-shadow: var(--focus-ring); }
.switch input:disabled + .switch__track { opacity: .5; cursor: not-allowed; }
```

### 5.4 徽章 / 状态点

```css
.badge {
  display: inline-flex; align-items: center; gap: 5px;
  padding: 1px var(--sp-2); border-radius: var(--radius-pill);
  font: 500 var(--fs-xs)/1.6 var(--font-sans);
  border: var(--bw) solid transparent; white-space: nowrap;
}
.badge--ok    { color: var(--ok-fg);    background: var(--ok-bg);    border-color: var(--ok-border); }
.badge--warn  { color: var(--warn-fg);  background: var(--warn-bg);  border-color: var(--warn-border); }
.badge--error { color: var(--danger-fg);background: var(--danger-bg);border-color: var(--danger-border); }
.badge--info  { color: var(--info-fg);  background: var(--info-bg);  border-color: var(--info-border); }
.badge--muted { color: var(--text-2);   background: var(--bg-subtle);border-color: var(--border-muted); }

/* 状态点：徽章前缀，或独立使用 */
.dot { width: 7px; height: 7px; border-radius: 50%; flex: none; display: inline-block; }
.dot--ok { background: var(--ok-fg); }
.dot--warn { background: var(--warn-fg); }
.dot--error { background: var(--danger-fg); }
.dot--muted { background: var(--text-3); }
```

> 状态**必须**同时用「文字 + 颜色」表达（如 `● 正常`），不能只靠颜色——色盲可用性 + 打印友好。

### 5.5 卡片 / 指标卡

```css
.card {
  background: var(--bg-surface);
  border: var(--bw) solid var(--border);
  border-radius: var(--radius-md);
  padding: var(--sp-4) 18px;
}
.card__title { font: 600 var(--fs-lg)/1.4 var(--font-sans); color: var(--text-1); margin: 0 0 var(--sp-3); }

/* 概览指标卡 */
.metric { }
.metric__k { font-size: var(--fs-xs); color: var(--text-2); letter-spacing: .02em; }
.metric__v {
  font: 600 var(--fs-xl)/1.15 var(--font-sans); color: var(--text-1);
  margin-top: 2px; font-variant-numeric: tabular-nums;
}
.metric__n { font-size: var(--fs-xs); color: var(--text-3); margin-top: 2px; }
```

### 5.6 表格（数据源状态 / 审计 / 运行历史）

```css
.table { width: 100%; border-collapse: collapse; font-size: var(--fs-sm); }
.table th {
  height: var(--table-head-h); padding: 0 var(--sp-3);
  text-align: left; font-weight: 500; color: var(--text-2);
  background: var(--bg-subtle);
  border-bottom: var(--bw) solid var(--border);
  white-space: nowrap;
}
.table td {
  height: var(--table-row-h); padding: 6px var(--sp-3);
  border-bottom: var(--bw) solid var(--border-muted); vertical-align: middle;
}
.table tbody tr:last-child td { border-bottom: 0; }
.table tbody tr:hover td { background: var(--bg-subtle); }         /* 整行 hover */
.table td.num { text-align: right; font-variant-numeric: tabular-nums; }
.table td.mono { font-family: var(--font-mono); font-size: var(--fs-xs); color: var(--text-2); }
.table--zebra tbody tr:nth-child(even) td { background: #FBFCFD; }
```

> 表格包一层 `.table-wrap { overflow-x: auto; }`，窄屏横向滚动而不是挤压换行。

### 5.7 提示条（info / warn / error / success）

```css
.alert {
  display: flex; gap: var(--sp-3); align-items: flex-start;
  padding: var(--sp-3) var(--sp-4);
  border: var(--bw) solid; border-radius: var(--radius-sm);
  font-size: var(--fs-sm); line-height: 1.55;
}
.alert__icon { flex: none; margin-top: 1px; }
.alert--info    { color: var(--text-1); background: var(--info-bg);   border-color: var(--info-border); }
.alert--warn    { color: var(--text-1); background: var(--warn-bg);   border-color: var(--warn-border); }
.alert--error   { color: var(--text-1); background: var(--danger-bg); border-color: var(--danger-border); }
.alert--success { color: var(--text-1); background: var(--ok-bg);     border-color: var(--ok-border); }
/* 左侧 3px 强调竖线，强化级别（可选） */
.alert--error { border-left-width: 3px; border-left-color: var(--danger-fg); }
```

### 5.8 模态确认（危险操作二次确认）

```css
.modal-backdrop {
  position: fixed; inset: 0; z-index: 300;
  background: rgba(31,35,40,.45);                /* 中性遮罩，非模糊玻璃 */
  display: flex; align-items: center; justify-content: center; padding: var(--sp-4);
}
.modal {
  width: 100%; max-width: 440px; background: var(--bg-surface);
  border: var(--bw) solid var(--border); border-radius: var(--radius-lg);
  box-shadow: var(--shadow-lg); padding: var(--sp-6);
}
.modal__title { font: 600 var(--fs-lg)/1.4 var(--font-sans); margin: 0 0 var(--sp-2); }
.modal__body  { font-size: var(--fs-sm); color: var(--text-2); line-height: 1.6; }
.modal__actions { display: flex; justify-content: flex-end; gap: var(--sp-3); margin-top: var(--sp-6); }
/* 危险确认：输入目标名称后才可点（服务端/JS 校验），按钮禁用态见 .btn:disabled */
```

### 5.9 日志窗口（等宽 + 级别着色）

```css
.log {
  background: var(--bg-inset);
  border: var(--bw) solid var(--border); border-radius: var(--radius-md);
  padding: var(--sp-3) var(--sp-4);
  font: 400 var(--fs-log)/1.6 var(--font-mono);
  overflow: auto; max-height: 420px;
  white-space: pre; tab-size: 4;
}
.log__line { display: block; }
.log__ts    { color: var(--text-3); }             /* 时间戳 */
.log--info  { color: var(--log-info); }
.log--warn  { color: var(--log-warn); }
.log--error { color: var(--log-error); font-weight: 500; }
.log--debug { color: var(--log-debug); }
/* 行级浅底提示（可选，仅 error/warn 用） */
.log__line.log--error { background: rgba(207,34,46,.06); }
```

### 5.10 导航栏（顶部）

```css
.nav {
  height: var(--nav-h); background: var(--bg-surface);
  border-bottom: var(--bw) solid var(--border);
  display: flex; align-items: center; gap: var(--sp-5);
  padding: 0 var(--sp-6);
  position: sticky; top: 0; z-index: 100;
}
.nav__brand { font: 650 var(--fs-md)/1 var(--font-sans); color: var(--text-1); }
.nav__link {
  height: var(--nav-h); display: inline-flex; align-items: center;
  font-size: var(--fs-base); color: var(--text-2); text-decoration: none;
  border-bottom: var(--bw-accent) solid transparent;      /* 预留激活位 */
}
.nav__link:hover { color: var(--text-1); }
.nav__link--active { color: var(--accent); border-bottom-color: var(--accent); }
.nav__spacer { margin-left: auto; }
.nav__meta { font: 400 var(--fs-xs)/1 var(--font-mono); color: var(--text-3); }
```

### 5.11 登录页卡片

```css
.login-wrap { min-height: 100vh; display: flex; align-items: center; justify-content: center; padding: var(--sp-4); }
.login-card {
  width: 100%; max-width: 360px; background: var(--bg-surface);
  border: var(--bw) solid var(--border); border-radius: var(--radius-lg);
  box-shadow: var(--shadow-sm); padding: var(--sp-8) var(--sp-6);
}
.login-card__title { font: 650 var(--fs-2xl)/1.3 var(--font-sans); margin: 0 0 4px; }
.login-card__sub   { font-size: var(--fs-sm); color: var(--text-2); margin-bottom: var(--sp-6); }
```

### 5.12 图标

- **只用内联 SVG**（`stroke="currentColor"`，`fill="none"`，`stroke-width="1.5"`，尺寸 16 或 18px），或纯 CSS 图形（`.dot`、箭头用 `border` 三角）。
- 图标颜色继承文字色（`currentColor`），不单独上色，保证与文字对比度一致。
- **禁止 emoji 当图标**（跨平台渲染不一致，且不像控制台）。

---

## 6. Layout（布局规则）

### 6.1 容器与栅格

```css
.page { max-width: 1120px; margin: 0 auto; padding: var(--sp-6) var(--sp-5) var(--sp-12); }
.grid { display: grid; gap: var(--sp-4); }
.grid--metrics { grid-template-columns: repeat(4, minmax(0,1fr)); } /* 概览指标卡 */
.grid--split   { grid-template-columns: minmax(0,2fr) minmax(0,1fr); } /* 主区 + 侧栏 */
```

- 页面最大宽度 **1120px**（信息密集，不要拉到 1440+ 让表格散架）。
- 指标卡一屏 **4 列**；数据表整宽单列。
- 卡片间距 `16px`；`section` 之间 `24px`。

### 6.2 响应式断点

| 断点 | 宽度 | 行为 |
|------|------|------|
| 宽屏 | ≥ 1024px | 4 列指标卡；侧栏并排；表格完整列 |
| 中屏 | 640–1023px | 指标卡 2 列；侧栏落到主区下方；表格横向滚动 |
| 窄屏 | < 640px | 指标卡 1 列；导航折叠为两行或横向滚动；**表格转卡片式堆叠** |

**窄屏表格折叠方案**（推荐，避免横向滚动丢列）：

```css
@media (max-width: 640px) {
  .grid--metrics { grid-template-columns: 1fr; }
  .grid--split   { grid-template-columns: 1fr; }
  .table--stack thead { display: none; }
  .table--stack tr {
    display: block; border: var(--bw) solid var(--border);
    border-radius: var(--radius-sm); margin-bottom: var(--sp-2); padding: var(--sp-2);
  }
  .table--stack td {
    display: flex; justify-content: space-between; gap: var(--sp-3);
    height: auto; border: 0; padding: 4px var(--sp-2);
  }
  .table--stack td::before { content: attr(data-label); color: var(--text-2); font-size: var(--fs-xs); }
}
```

> 折叠时每个 `<td>` 需带 `data-label="列名"`，否则堆叠后没有上下文。

---

## 7. Depth & Elevation（深度与层级）

| 层级 | 值 | 用途 |
|------|-----|------|
| Flat | `none` | 默认：卡片、表格、输入框——**用边框而非阴影分层** |
| Raised | `var(--shadow-sm)` | 登录卡、需要轻微浮起的容器 |
| Floating | `var(--shadow-md)` | 下拉菜单、弹出面板 |
| Overlay | `var(--shadow-lg)` | 模态 |

**Z-index 阶梯**：`--z-nav:100` · `--z-dropdown:200` · `--z-backdrop:300` · `--z-modal:310` · `--z-toast:400`。

> 分层原则：**先边框，后阴影**。控制台里 90% 的分层靠 1px 边框完成，阴影只留给真正"浮起来"的东西（模态、下拉）。

---

## 8. Cautions（反面清单 · 不要出现）

以下每一条都**明确禁止**，并说明理由：

1. **渐变按钮 / 渐变文字 / 渐变边框** —— 渐变是营销语言，控制台里它抢注意力且显得廉价。实心单色即可。
2. **玻璃拟态（`backdrop-filter: blur` + 半透明面板）** —— 在数据密集界面降低可读性，且老浏览器/低端设备性能差。用实色 + 边框。
3. **彩色阴影**（如 `0 4px 12px rgba(94,106,210,.4)`）—— 阴影必须是中性黑；彩色阴影是"光效"，与运维气质冲突。
4. **卡片嵌套超过 2 层** —— 每多一层边框/内边距就多一份视觉噪音，还会把信息密度稀释掉。需要分组时用**标题 + 分隔线**，而不是再套一个卡片。
5. **深色面板 / 深色日志窗** —— 全站强制浅底深字（用户 IDE 是 light 主题）。日志窗用浅底 `--bg-inset`。
6. **大标题 / 大留白 / Hero 区** —— 不要 `font-size: 40px+`、不要 `padding: 80px`。页标题封顶 22px。
7. **外部资源**：CDN 字体、图标字体（Font Awesome 等）、Tailwind CDN、外链图片、CSS 框架 —— 一律禁止。图标用内联 SVG 或纯 CSS。
8. **`oklch()` / `color-mix()` / 容器查询等新语法** —— 老浏览器不支持。只用 hex / `rgb()` / `rgba()`。
9. **全大写 + 大字号 + 大字距的标题**（`text-transform:uppercase; letter-spacing:2px`）—— 中文场景无意义，英文也显得花哨。
10. **emoji 当图标或状态标记** —— 跨平台渲染不一致、不可控大小、不像控制台。用 SVG 或 `.dot`。
11. **动画滥用**：不要持续动画、加载骨架屏霓虹闪烁、渐变流动。过渡只允许 `background/border/color`，时长 ≤ 150ms，且只用在 hover/focus。
12. **一屏多个实心强调色按钮** —— 主按钮一屏最多一个；其余用次按钮/幽灵按钮，否则主次不分。
13. **纯颜色传达状态** —— 状态必须"文字 + 颜色"双通道（`● 正常`）。
14. **超过 4 种语义色同屏** —— 成功/警告/危险/信息之外不再引入第五种彩色。
15. **超圆角**（`border-radius: 16px+` 用在卡片/按钮上）—— 显得卡通、不专业。卡片封顶 8–10px，按钮 6px。

**Prefer（推荐替代）**：
- 要强调 → 加粗字重 / 加深文字色，而不是加颜色。
- 要分组 → 分区标题 + 发丝分隔线，而不是套卡片。
- 要层级 → 1px 边框 + 背景微差（`#fff` vs `#f1f3f5`），而不是重阴影。
- 要图标 → 内联 SVG `stroke="currentColor"`，尺寸 16/18px。

---

## 9. Agent Prompt Guide（生成指南 + 可直接复制的 `:root`）

### 9.1 关键指令

- **只写浅色**：页面底 `#F6F7F9`，面板白，文字深灰。任何"深色块"都是错的。
- **只用系统字体**：直接用 §3.1 的 `--font-sans` / `--font-mono`，**不要**引任何字体。
- **只用令牌**：颜色/间距/圆角/字号全部走 `var(--...)`，不要在组件里硬编码色值（语义色除外，也必须走变量）。
- **图标内联 SVG**，`stroke="currentColor"`，16/18px。
- **语义色三件套成对用**：徽章/提示条永远 `fg + bg + border` 一起上。
- **密度优先**：默认 14px 正文、36px 表格行高、13px 表格字。不要为了"好看"把行高拉到 48px。
- **每个界面都要有明确的空状态**（无数据时给一行 `--text-3` 提示 + 建议操作），不要留白屏。

### 9.2 快速 `:root`（直接粘贴进 `<style>`）

```css
:root {
  /* —— 中性：底 / 面板 / 边框 —— */
  --bg-page: #F6F7F9;
  --bg-surface: #FFFFFF;
  --bg-subtle: #F1F3F5;
  --bg-inset: #F6F8FA;
  --border: #D8DEE4;
  --border-muted: #E8ECF0;
  --border-strong: #C3CAD3;

  /* —— 文字 —— */
  --text-1: #1F2328;
  --text-2: #59636E;
  --text-3: #6E7781;
  --text-disabled: #8C959F;
  --text-on-accent: #FFFFFF;

  /* —— 语义：成功 —— */
  --ok-fg: #1A7F37;  --ok-bg: #DAFBE1;  --ok-border: #ACE5BE;  --ok-solid: #1F883D;
  /* —— 语义：警告 —— */
  --warn-fg: #9A6700; --warn-bg: #FFF6D5; --warn-border: #EAC54F;
  /* —— 语义：危险 —— */
  --danger-fg: #CF222E; --danger-bg: #FFEBE9; --danger-border: #FFC1BC;
  --danger-solid: #CF222E; --danger-solid-hover: #A40E26;
  /* —— 语义：信息 —— */
  --info-fg: #0969DA; --info-bg: #DDF4FF; --info-border: #A5D6FF;

  /* —— 日志级别 —— */
  --log-info: #59636E; --log-warn: #9A6700; --log-error: #CF222E; --log-debug: #6E7781;

  /* —— 强调 —— */
  --accent: #5E6AD2;
  --accent-hover: #4F5AC0;
  --accent-active: #434DAB;
  --accent-soft: #EEF0FB;
  --accent-focus: rgba(94,106,210,.35);

  /* —— 字体 —— */
  --font-sans: -apple-system, BlinkMacSystemFont, "Segoe UI", "PingFang SC",
    "Hiragino Sans GB", "Microsoft YaHei", "微软雅黑", "Noto Sans CJK SC",
    "Source Han Sans SC", Roboto, "Helvetica Neue", Arial, sans-serif;
  --font-mono: ui-monospace, SFMono-Regular, "SF Mono", "Cascadia Mono",
    "JetBrains Mono", Consolas, "Liberation Mono", Menlo, "Courier New",
    "PingFang SC", "Microsoft YaHei", monospace;

  /* —— 字号 —— */
  --fs-xs: 12px; --fs-sm: 13px; --fs-base: 14px; --fs-md: 15px;
  --fs-lg: 16px; --fs-xl: 24px; --fs-2xl: 22px; --fs-log: 12.5px;

  /* —— 间距 —— */
  --sp-1: 4px; --sp-2: 8px; --sp-3: 12px; --sp-4: 16px;
  --sp-5: 20px; --sp-6: 24px; --sp-8: 32px; --sp-10: 40px; --sp-12: 48px;

  /* —— 圆角 / 边框 —— */
  --radius-xs: 4px; --radius-sm: 6px; --radius-md: 8px; --radius-lg: 10px;
  --radius-pill: 999px; --bw: 1px; --bw-accent: 2px;

  /* —— 阴影 —— */
  --shadow-xs: 0 1px 1px rgba(31,35,40,.04);
  --shadow-sm: 0 1px 2px rgba(31,35,40,.06), 0 1px 1px rgba(31,35,40,.04);
  --shadow-md: 0 3px 6px rgba(31,35,40,.08);
  --shadow-lg: 0 8px 24px rgba(31,35,40,.12);
  --focus-ring: 0 0 0 3px var(--accent-focus);

  /* —— 控件尺寸 —— */
  --control-h: 34px; --control-h-sm: 28px; --control-h-lg: 38px;
  --table-row-h: 36px; --table-head-h: 32px; --nav-h: 48px;

  /* —— 层级 —— */
  --z-nav: 100; --z-dropdown: 200; --z-backdrop: 300; --z-modal: 310; --z-toast: 400;
}

* { box-sizing: border-box; }
body {
  margin: 0; background: var(--bg-page); color: var(--text-1);
  font: 400 var(--fs-base)/1.55 var(--font-sans);
  -webkit-font-smoothing: antialiased;
}
```

---

## 10. 页面清单 → 组件映射（施工对照表）

| 界面 | 用到的组件（§5） | 关键令牌 |
|------|-----------------|---------|
| 登录页 | 登录卡 5.11、输入框 5.2、主按钮 5.1、提示条 5.7（错误/锁定） | `--radius-lg`、`--control-h-lg` |
| 概览页 | 指标卡 5.5、表格 5.6、徽章/状态点 5.4 | `--fs-xl`、`tabular-nums` |
| 操作区 | 按钮四态 5.1、模态确认 5.8 | `--danger-solid`、`--control-h` |
| 配置表单 | 输入/开关 5.2·5.3、字段说明 5.2 | `--bg-subtle`（只读）、`--warn-fg`（差异） |
| 日志查看 | 日志窗口 5.9 | `--font-mono`、`--log-*` |
| 审计记录 | 表格 5.6（`.mono` 列） | `--fs-xs`、`--text-2` |
| 运行历史 | 表格 5.6（`.num` 列）、徽章 5.4 | `tabular-nums` |

---

*规范版本 v1.0 · 设计系统：Linear（浅色）· 令牌已通过 WCAG AA 对比度检查 · 全部值兼容旧浏览器*
