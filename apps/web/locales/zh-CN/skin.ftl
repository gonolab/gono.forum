skin-source-builtin = 内置
skin-source-imported = 导入
skin-source-instance = 实例目录

skin-mode-light = 浅色模式
skin-mode-dark = 深色模式

skin-finding-file = { $file }：{ $text }
skin-more-findings = { $count ->
   *[other] 另有 { $count } 处问题
}
skin-css-too-long = 皮肤样式表最多 { $max } KiB（含注释）
skin-css-important = 皮肤里不能用 !important
skin-css-reserved-variable = { $name } 由核心保留
skin-css-colophon = 规则不能指向页脚的“Built on Gono Forum”一行（colophon）
skin-css-guarded-root = 不能对 html、body、footer、:root 或所有元素设置 { $property }：这可能藏起页脚的“Built on Gono Forum”一行
skin-css-masthead-size = { $property } 会改动页头导航测量时所用的页头尺寸
skin-css-focus-hidden = 键盘焦点必须保持可见
skin-css-actions-hidden = 不能用 { $property } 隐藏帖子操作按钮：键盘和触屏用户会找不到它们
skin-css-breakpoint = { $condition } 用了 900、760、700、640、480、420 px 以外的宽度
skin-css-physical = { $property } 是物理属性，请改用逻辑属性（inline 或 block）
skin-css-fixed-value = { $property }：{ $value } 不在样式规范的固定数值里
skin-css-design-variable = { $name } 只能取样式规范里的值
skin-css-fragile-selector = 选择器依赖 aria-label、title 或元素的位置；请改用 docs/skins.md 列出的类名

skin-manifest-missing = 缺少 skin.toml
skin-manifest-syntax = 无法读取 skin.toml：{ $detail }
skin-manifest-too-long = 清单最多 { $max } KiB
skin-manifest-contract = 皮肤按约定版本 { $found } 编写；当前版本的 Gono Forum 读取约定版本 { $expected }
skin-manifest-id = id“{ $value }”无效：须为 2 到 32 个小写字母、数字和连字符，以字母开头
skin-manifest-directory = 目录 { $directory } 里是皮肤 { $id }；皮肤目录须以其 id 命名
skin-manifest-version = { $field } 须为 1.2.0 这样的版本号
skin-manifest-name = { $field } 必须有英文（en）
skin-manifest-text = { $field } 含有未知的语言标签、空文字或过长的文字
skin-manifest-value = { $field } 的值无效
skin-manifest-unsupported = 当前版本的 Gono Forum 不支持 { $field }
skin-manifest-min-forum = 皮肤需要 Gono Forum { $version } 或更新版本；当前是 { $current }

skin-palette-syntax = 无法读取配色：{ $detail }
skin-palette-too-long = 所有配色合计最多 { $max } KiB
skin-palette-missing = 缺少 skin.toml 中指定的配色 { $name }
skin-palette-unused = 配色 { $name } 未在 skin.toml 中指定，不会使用
skin-palette-token = { $name } 不是有效的颜色变量名
skin-palette-value = { $name } 的值不被允许（不能有花括号、分号、!、url() 或清单外的函数）
skin-palette-unknown = { $name } 不是核心的颜色变量，已忽略
skin-palette-reference = { $mode }下，{ $name } 引用了不存在的 { $target }
skin-palette-cycle = { $mode }下，变量相互循环引用：{ $chain }
skin-contrast = { $mode }下，{ $background } 上的 { $foreground } 对比度为 { $ratio }:1，至少需要 { $min }:1
skin-contrast-unknown = { $mode }下，无法检查 { $background } 上 { $foreground } 的对比度：请使用普通颜色（十六进制、rgb()、hsl()、oklch()）
skin-colophon-color = { $mode }下，页脚颜色（--text-muted、--text 和 --panel）必须是普通颜色
skin-colophon-contrast = { $mode }下，--panel 上的 { $foreground } 对比度为 { $ratio }:1；页脚的“Built on Gono Forum”一行需要 4.5:1

skin-pack-file = 该文件不属于皮肤，已忽略
skin-pack-too-long = 皮肤包最多 { $max } KiB
skin-pack-syntax = 无法读取皮肤：{ $detail }
skin-pack-format = 该文件不是 Gono Forum 皮肤包

skin-page-title = 皮肤
skin-page-description = 站点外观：选择皮肤、预览、导入皮肤包，并查看检查结果。
skin-current-heading = 站点皮肤
skin-current-lead = 皮肤决定每个页面的颜色和样式表。各域名可以在{ $domains_html }中各自的页面上另选皮肤。
skin-field-skin = 皮肤
skin-field-palette = 配色
skin-select-submit = 用于全站
skin-preview-submit = 预览
skin-list-heading = 全部皮肤
skin-column-skin = 皮肤
skin-column-source = 来源
skin-column-state = 状态
skin-column-actions = 操作
skin-state-usable = 可用
skin-state-current = 全站正在使用
skin-state-domains = { $domains } 正在使用
skin-state-shadowed = 不可用：使用的是来自“{ $source }”的同名皮肤
skin-state-invalid = 不可用：未通过检查
skin-findings-errors = 问题
skin-findings-warnings = 警告
skin-imported-at = 导入于 { $time }
skin-delete-submit = 删除
skin-import-heading = 导入皮肤
skin-import-lead = 皮肤包是一个文件，用 gono-forum-cli skin pack 从皮肤目录生成。保存前会先检查；导入的皮肤由站点的所有服务器共享。
skin-import-field = 皮肤包
skin-import-submit = 检查并导入
skin-import-confirm-heading = 导入 { $name } { $version }？
skin-import-confirm-warnings = 这个皮肤可以使用，但不符合样式规范的以下规则。仍要导入吗？
skin-import-confirm-none = 检查没有发现问题。
skin-import-replaces = 它将替换已导入的版本 { $version }。
skin-import-confirm-submit = 导入
skin-import-cancel = 取消
skin-import-refused = 无法导入这个皮肤：
skin-rescan-heading = 实例目录
skin-rescan-lead = { $dir_html } 中的皮肤在论坛启动和重新扫描时读取。每台服务器读取自己的目录，目录里的皮肤只在有它的服务器上可用。
skin-rescan-submit = 重新扫描
skin-saved-imported = 皮肤已导入。
skin-saved-deleted = 皮肤已删除。
skin-saved-selected = 站点已改用这个皮肤。
skin-saved-rescanned = 已重新读取实例目录。
skin-error-file = 请选择皮肤包文件。
skin-error-unknown = 没有这个 id 的可用皮肤。
skin-error-gone = 这个皮肤已被删除。
skin-error-changed = 这个皮肤已被修改，请重新检查。
skin-error-in-use = 这个皮肤正在使用中。请先为站点和使用它的域名另选皮肤。

skin-preview-bar = 正在预览 { $name }，只有你能看到。
skin-preview-publish = 发布
skin-preview-exit = 退出预览

skin-domain-heading = 皮肤
skin-domain-follow = 与全站相同（{ $name }）
skin-domain-lead = 这个域名使用的皮肤。所选皮肤没有的配色会改用它的默认配色。
skin-domain-submit = 保存皮肤
skin-domain-saved = 已保存域名的皮肤。

skin-avatar-fixed = 当前皮肤固定为所有头像显示{ $shape }。
skin-avatar-circle = 圆形
skin-avatar-rounded = 圆角
skin-avatar-square = 方形
