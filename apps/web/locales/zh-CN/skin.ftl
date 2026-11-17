skin-source-builtin = 内置
skin-source-imported = 导入
skin-source-instance = 实例目录

skin-mode-light = 浅色模式
skin-mode-dark = 深色模式

skin-builtin-default-name = 默认
skin-builtin-default-description = Gono Forum 的标准外观。

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
skin-manifest-side-nav = frame.nav = "side" 需要在 skin.toml 的 [layout] 里给 side.nav 位置至少放一个块
skin-manifest-layout = skin.toml 的 [layout] 不正确：{ $message }
skin-manifest-min-forum = 皮肤需要 Gono Forum { $version } 或更新版本；当前是 { $current }
skin-manifest-builtin-text = 内置皮肤的 { $field } 只写英文（en）：其他语言写在 skin.ftl 的翻译里
skin-manifest-builtin-messages = 翻译文件里没有内置皮肤 { $id } 的名称：请在 skin.ftl 加上 skin-builtin-{ $id }-name 和 -description，并加进 skins.rs 的列表

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
skin-findings-errors-count = { $count ->
   *[other] { $count } 处问题
}
skin-findings-warnings-count = { $count ->
   *[other] { $count } 条警告
}
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
skin-preview-colors-bar = 正在 { $name } 上预览新的配色，只有你能看到。
skin-preview-publish = 发布
skin-preview-exit = 退出预览

skin-colors-page-title = 配色
skin-colors-page-description = 选一个主色和一个次色，皮肤的全部颜色都由它们推导出来，浅色、深色各一套。
skin-colors-heading = 种子色
skin-colors-lead = 主色用于按钮、品牌块、链接、当前项、头像和焦点环。次色只把它的色相淡淡地染到底色、面板、线条和文字上。亮度沿用皮肤自己的，浅色和深色模式下每处文字与底色都调到不低于 4.5:1，输入框边框和焦点环不低于 3:1。换皮肤后配色保留，并按新皮肤重新推导。域名沿用这里的配色，也可以在 { $domains_html } 里各自的页面另选；访客不能自选配色。
skin-colors-state-skin = 站点使用 { $name } 自带的配色。
skin-colors-state-seeds = 站点在 { $name } 上使用种子色 { $primary } 和 { $secondary }。
skin-colors-field-primary = 主色
skin-colors-field-primary-hint = 按钮、品牌块、链接、当前项、头像和焦点环。
skin-colors-field-secondary = 次色
skin-colors-field-secondary-hint = 它的色相给底色、面板、线条和文字染色；选灰色则保持中性。
skin-colors-submit = 保存配色
skin-colors-show = 显示推导结果
skin-colors-preview = 预览
skin-colors-restore = 恢复为皮肤自带配色
skin-colors-saved = 配色已保存。
skin-colors-restored = 站点已恢复使用皮肤自带的配色。
skin-colors-error-format = 颜色要写成 # 加六位十六进制数字，例如 #4f46e5。
skin-colors-derived-heading = 推导出的配色
skin-colors-derived-lead = { $primary } 和 { $secondary } 在 { $name }（配色 { $palette }）上推导出的结果。没有列出的变量保持皮肤的值。
skin-colors-column-token = 变量
skin-colors-column-light = 浅色模式
skin-colors-column-dark = 深色模式
skin-colors-column-text = 文字
skin-colors-column-ground = 底色
skin-colors-column-required = 至少
skin-colors-kept = 皮肤的值
skin-colors-contrast-heading = 对比度
skin-colors-contrast-lead = 推导时保证可读的每一对文字与底色，以及它们在两种模式下的对比度。
skin-colors-ratio = { $ratio }:1
skin-colors-ratio-low = { $ratio }:1，不够
skin-colors-ratio-unknown = 无法测量

skin-domain-heading = 外观
skin-domain-follow = 与全站相同（{ $name }）
skin-domain-lead = 这个域名使用的皮肤。所选皮肤没有的配色会改用它的默认配色。
skin-domain-submit = 保存皮肤
skin-domain-saved = 已保存域名的皮肤。
skin-domain-colors-legend = 配色
skin-domain-colors-lead = 这个域名在它的皮肤上使用的种子色（参见“外观 › 配色”）。
skin-domain-colors-follow = 与全站相同：{ $state }
skin-domain-colors-skin = 皮肤自带的配色
skin-domain-colors-own = 另设种子色
skin-domain-colors-submit = 保存配色
skin-domain-colors-saved = 已保存域名的配色。

skin-avatar-fixed = 当前皮肤固定为所有头像显示{ $shape }。
skin-avatar-circle = 圆形
skin-avatar-rounded = 圆角
skin-avatar-square = 方形
