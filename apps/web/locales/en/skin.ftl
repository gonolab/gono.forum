## Skins (docs/skins.md): the admin page Appearance › Skins, the preview bar, and the problems the skin checks report.

## Where a skin comes from.
skin-source-builtin = Built in
skin-source-imported = Imported
skin-source-instance = Instance directory

## Light or dark mode, the $mode of the contrast and palette problems below.
skin-mode-light = light mode
skin-mode-dark = dark mode

## The names and descriptions of the built-in skins (apps/web/skins/<id>/), shown in the admin area and the preview
## bar. External skins write their own names in each language in skin.toml.
skin-builtin-default-name = Default
skin-builtin-default-description = The standard look of Gono Forum.

## Problems found in a skin. $file is the file of the skin (skin.toml, palettes/<name>.json, skin.css), $text the
## problem.
skin-finding-file = { $file }: { $text }
# $count problems were not listed.
skin-more-findings = { $count ->
    [one] and one more problem
   *[other] and { $count } more problems
}
# $max is the size limit in KiB.
skin-css-too-long = A skin stylesheet can be at most { $max } KiB, comments included
skin-css-important = !important is not allowed in skins
# $name is a CSS custom property.
skin-css-reserved-variable = { $name } is reserved for the core
skin-css-colophon = Rules may not name the footer's "Built on Gono Forum" line (colophon)
# $property is a CSS property.
skin-css-guarded-root = { $property } may not be set on html, body, footer, :root or every element: it could hide the footer's "Built on Gono Forum" line
skin-css-masthead-size = { $property } changes a header size the header navigation was measured with
skin-css-focus-hidden = Keyboard focus must stay visible
skin-css-actions-hidden = { $property } may not hide the post actions: keyboard and touch users could not find them
# $condition is an @media condition.
skin-css-breakpoint = { $condition } uses a width other than 900, 760, 700, 640, 480 or 420 px
skin-css-physical = { $property } is a physical property; use its logical form (inline or block)
# $value is the value written.
skin-css-fixed-value = { $property }: { $value } is not one of the fixed values of the style guide
skin-css-design-variable = { $name } keeps the values of the style guide
skin-css-fragile-selector = The selector depends on aria-label, title or the position of an element; use the classes of docs/skins.md

skin-manifest-missing = skin.toml is missing
# $detail is the parser's description, in English.
skin-manifest-syntax = skin.toml cannot be read: { $detail }
skin-manifest-too-long = The manifest can be at most { $max } KiB
# $found is the contract version the skin states, $expected the one this version of Gono Forum reads.
skin-manifest-contract = The skin is written for contract { $found }; this version of Gono Forum reads contract { $expected }
# $value is the id as written.
skin-manifest-id = The id "{ $value }" is not valid: 2 to 32 lowercase letters, digits and hyphens, starting with a letter
# $directory is the name of the skin's directory.
skin-manifest-directory = The directory { $directory } holds the skin { $id }; a skin's directory is named after its id
# $field is a field of skin.toml.
skin-manifest-version = { $field } must be a version number such as 1.2.0
skin-manifest-name = { $field } needs an English text (en)
skin-manifest-text = { $field } has an unknown language tag, an empty text or a text that is too long
skin-manifest-value = { $field } has an invalid value
skin-manifest-unsupported = { $field } is not supported by this version of Gono Forum
skin-manifest-layout = The [layout] of skin.toml is not valid: { $message }
# $version is the version the skin needs, $current the running one.
skin-manifest-min-forum = The skin needs Gono Forum { $version } or later; this is { $current }
# $field is name or description.
skin-manifest-builtin-text = A built-in skin writes { $field } only in English (en): its other languages are translations in skin.ftl
# $id is the skin's id.
skin-manifest-builtin-messages = The built-in skin { $id } has no name in the translation files: add skin-builtin-{ $id }-name and -description to skin.ftl and to the list in skins.rs

skin-palette-syntax = The palette cannot be read: { $detail }
skin-palette-too-long = The palettes can be at most { $max } KiB together
# $name is a palette name.
skin-palette-missing = The palette { $name } named in skin.toml is missing
skin-palette-unused = The palette { $name } is not named in skin.toml and is not used
skin-palette-token = { $name } is not a valid color token name
skin-palette-value = { $name } has a value that is not allowed (no braces, semicolons, !, url() or functions outside the list)
skin-palette-unknown = { $name } is not a color token of the core; it is ignored
# $target is the token a value refers to.
skin-palette-reference = In { $mode }, { $name } refers to { $target }, which does not exist
# $chain lists the tokens that refer to each other.
skin-palette-cycle = In { $mode }, tokens refer to each other in a circle: { $chain }
# $foreground and $background are color tokens, $ratio the contrast found, $min the contrast required.
skin-contrast = In { $mode }, { $foreground } on { $background } has a contrast of { $ratio }:1; at least { $min }:1 is required
skin-contrast-unknown = In { $mode }, the contrast of { $foreground } on { $background } cannot be checked: use plain colors (hex, rgb(), hsl(), oklch())
skin-colophon-color = In { $mode }, the footer colors (--text-muted, --text and --panel) must be plain colors
skin-colophon-contrast = In { $mode }, { $foreground } on --panel has a contrast of { $ratio }:1; the footer's "Built on Gono Forum" line needs 4.5:1

skin-pack-file = The file is not part of a skin and is ignored
skin-pack-too-long = A skin pack can be at most { $max } KiB
skin-pack-syntax = The skin cannot be read: { $detail }
skin-pack-format = The file is not a Gono Forum skin pack

## The admin page.
skin-page-title = Skins
skin-page-description = The look of the site: choose a skin, preview it, import skin packs and see what their checks found.
skin-current-heading = Skin of the site
skin-current-lead = The skin decides the colors and the stylesheet of every page. Domains can choose their own on their page under { $domains_html }.
skin-field-skin = Skin
skin-field-palette = Palette
skin-select-submit = Use for the whole site
skin-preview-submit = Preview
skin-list-heading = All skins
skin-column-skin = Skin
skin-column-source = Source
skin-column-state = State
skin-column-actions = Actions
skin-state-usable = Usable
skin-state-current = Used by the whole site
# $domains lists the domains whose own choice it is.
skin-state-domains = Used by { $domains }
# $source is where the skin that is used instead comes from (Built in, Imported, Instance directory).
skin-state-shadowed = Not usable: the skin with this id from "{ $source }" is used
skin-state-invalid = Not usable: it fails its checks
# $count problems or warnings a skin's checks found; the list opens below.
skin-findings-errors-count = { $count ->
    [one] 1 problem
   *[other] { $count } problems
}
skin-findings-warnings-count = { $count ->
    [one] 1 warning
   *[other] { $count } warnings
}
# $time is when the skin was imported.
skin-imported-at = Imported { $time }
skin-delete-submit = Delete
skin-import-heading = Import a skin
skin-import-lead = A skin pack is one file, made from a skin directory with gono-forum-cli skin pack. It is checked before anything is saved; an imported skin is shared by every machine of the site.
skin-import-field = Skin pack
skin-import-submit = Check and import
# $name and $version are the skin's name and version.
skin-import-confirm-heading = Import { $name } { $version }?
skin-import-confirm-warnings = The skin can be used, but it does not follow these rules of the style guide. Import it anyway?
skin-import-confirm-none = The checks found no problems.
# $version is the version imported earlier.
skin-import-replaces = It replaces the imported version { $version }.
skin-import-confirm-submit = Import
skin-import-cancel = Cancel
skin-import-refused = The skin cannot be imported:
skin-rescan-heading = Instance directory
# $dir_html is the directory.
skin-rescan-lead = Skins in { $dir_html } are read when the forum starts and when you rescan. Every machine reads its own directory, and a skin there is only used where it exists.
skin-rescan-submit = Rescan
skin-saved-imported = The skin was imported.
skin-saved-deleted = The skin was deleted.
skin-saved-selected = The site uses the skin now.
skin-saved-rescanned = The instance directory was read again.
skin-error-file = Choose a skin pack file.
skin-error-unknown = There is no usable skin with this id.
skin-error-gone = The skin was deleted meanwhile.
skin-error-changed = The skin was changed meanwhile. Please check it again.
skin-error-in-use = The skin is in use. Choose another skin for the site and for the domains that use it first.

## The preview bar at the top of every page while an administrator previews a skin.
# $name is the skin's name.
skin-preview-bar = Previewing { $name }. Only you see it.
# While an administrator previews unsaved seed colors; $name is the skin they are laid over.
skin-preview-colors-bar = Previewing new colors on { $name }. Only you see them.
skin-preview-publish = Publish
skin-preview-exit = Exit preview

## Appearance › Colors (/manage/appearance/colors): two seed colors from which the colors of the skin are derived.
skin-colors-page-title = Colors
skin-colors-page-description = Pick a primary and a secondary color, and every color of the skin is derived from them in light and dark mode.
skin-colors-heading = Seed colors
# $domains_html links to Multiple domains.
skin-colors-lead = The primary color goes to buttons, the brand block, links, current items, avatars and the focus ring. The secondary color lends its hue, lightly, to grounds, panels, lines and text. Lightness follows the skin, and every text is raised to a contrast of 4.5:1 on its ground, field borders and the focus ring to 3:1, in light and dark mode. The colors stay when you change skins and are derived again for the new one. Domains follow them or choose their own on their page under { $domains_html }; visitors cannot choose colors.
# $name is the site's skin.
skin-colors-state-skin = The site uses the colors that come with { $name }.
# $primary and $secondary are colors written #rrggbb; $name is the site's skin.
skin-colors-state-seeds = The site uses the seed colors { $primary } and { $secondary } on { $name }.
skin-colors-field-primary = Primary color
skin-colors-field-primary-hint = Buttons, the brand block, links, current items, avatars and the focus ring.
skin-colors-field-secondary = Secondary color
skin-colors-field-secondary-hint = Its hue tints grounds, panels, lines and text; a grey keeps them neutral.
skin-colors-submit = Save colors
skin-colors-show = Show the derived colors
skin-colors-preview = Preview
skin-colors-restore = Use the skin's own colors
skin-colors-saved = The colors were saved.
skin-colors-restored = The site uses the skin's own colors again.
skin-colors-error-format = Write each color as # and six hexadecimal digits, such as #4f46e5.
skin-colors-derived-heading = Derived colors
# $primary and $secondary are the seed colors shown; $name is the skin and $palette its palette.
skin-colors-derived-lead = What { $primary } and { $secondary } make of { $name } (palette { $palette }). Tokens not listed keep the skin's values.
skin-colors-column-token = Token
skin-colors-column-light = Light mode
skin-colors-column-dark = Dark mode
skin-colors-column-text = Text
skin-colors-column-ground = Ground
skin-colors-column-required = At least
# Shown in the light or dark column when that mode keeps the skin's value.
skin-colors-kept = The skin's value
skin-colors-contrast-heading = Contrast
skin-colors-contrast-lead = Every pair of text and ground the derivation keeps legible, with its contrast in each mode.
# $ratio is a contrast ratio such as 4.62.
skin-colors-ratio = { $ratio }:1
skin-colors-ratio-low = { $ratio }:1, too low
skin-colors-ratio-unknown = Cannot be measured

## A domain's own choice, on its page under Multiple domains.
skin-domain-heading = Appearance
# $name is the site's skin.
skin-domain-follow = Same as the whole site ({ $name })
skin-domain-lead = The skin this domain uses. A palette not offered by the chosen skin falls back to its default.
skin-domain-submit = Save skin
skin-domain-saved = The domain's skin was saved.
skin-domain-colors-legend = Colors
skin-domain-colors-lead = Seed colors for this domain, over its skin (see Appearance › Colors).
# $state says what the whole site uses (skin-colors-state-seeds or skin-colors-state-skin).
skin-domain-colors-follow = Same as the whole site: { $state }
skin-domain-colors-skin = The skin's own colors
skin-domain-colors-own = Own seed colors
skin-domain-colors-submit = Save colors
skin-domain-colors-saved = The domain's colors were saved.

## Personal settings: the avatar shape when the skin fixes it. $shape is one of the shapes below.
skin-avatar-fixed = The current skin shows every avatar { $shape }.
skin-avatar-circle = round
skin-avatar-rounded = with rounded corners
skin-avatar-square = square
