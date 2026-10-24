class_name UiTheme
extends RefCounted
## The app's visual style: dark translucent panels, pill buttons and the brand accent.

const ACCENT: Color = Color(0.04, 0.61, 0.96)
const TEXT: Color = Color(0.94, 0.95, 0.97)
const MUTED: Color = Color(0.64, 0.68, 0.74)
const PANEL: Color = Color(0.06, 0.07, 0.09, 0.72)
const SURFACE: Color = Color(1, 1, 1, 0.07)
## Actions that delete something, and devices not found or failed (R76).
const DANGER: Color = Color(0.93, 0.33, 0.36)
## Something on its way, e.g. a device connecting (R76).
const WARNING: Color = Color(0.98, 0.75, 0.3)
## Something that worked, e.g. a device connected (R76).
const SUCCESS: Color = Color(0.36, 0.82, 0.56)
const RADIUS: int = 14
# i18n-begin: zone and climb names are shown translated.
## Power zones 1–7 (Coggan): name and colour, as commonly used by training platforms.
const POWER_ZONES: Array[Array] = [
	["Recovery", Color(0.6, 0.62, 0.66)],
	["Endurance", Color(0.25, 0.6, 0.95)],
	["Tempo", Color(0.3, 0.8, 0.45)],
	["Threshold", Color(0.98, 0.8, 0.2)],
	["VO2max", Color(0.98, 0.55, 0.2)],
	["Anaerobic", Color(0.95, 0.3, 0.3)],
	["Neuromuscular", Color(0.7, 0.4, 0.95)],
]
## Heart-rate zones 1–5: name and colour.
const HEART_RATE_ZONES: Array[Array] = [
	["Very light", Color(0.6, 0.62, 0.66)],
	["Light", Color(0.25, 0.6, 0.95)],
	["Moderate", Color(0.3, 0.8, 0.45)],
	["Hard", Color(0.98, 0.55, 0.2)],
	["Maximum", Color(0.95, 0.3, 0.3)],
]
## Climb categories as labelled by ORQWA, easiest first.
const CLIMB_COLORS: Dictionary[String, Color] = {
	"Climb": Color(0.6, 0.62, 0.66),
	"Cat 4": Color(0.3, 0.8, 0.45),
	"Cat 3": Color(0.98, 0.8, 0.2),
	"Cat 2": Color(0.98, 0.55, 0.2),
	"Cat 1": Color(0.95, 0.3, 0.3),
	"HC": Color(0.7, 0.4, 0.95),
}
## Ghost riders and pacers (R20) on the road, the map and the profile.
const GHOST_COLOR: Color = Color(0.98, 0.55, 0.2)
# i18n-end
const POWER_COLOR: Color = Color(0.04, 0.61, 0.96)
const HEART_RATE_COLOR: Color = Color(0.95, 0.33, 0.38)
const CADENCE_COLOR: Color = Color(0.36, 0.82, 0.56)


static func build() -> Theme:
	var theme: Theme = Theme.new()
	theme.default_font_size = 15

	theme.set_stylebox("panel", "PanelContainer", panel())
	theme.set_stylebox("panel", "Panel", panel())

	theme.set_color("font_color", "Label", TEXT)

	for type: String in ["Button", "OptionButton", "CheckBox"]:
		theme.set_color("font_color", type, TEXT)
		theme.set_color("font_hover_color", type, TEXT)
		theme.set_color("font_pressed_color", type, Color.WHITE)
		theme.set_color("font_disabled_color", type, Color(TEXT, 0.35))
	for type: String in ["Button", "OptionButton"]:
		theme.set_stylebox("normal", type, _button_box(SURFACE))
		theme.set_stylebox("hover", type, _button_box(Color(1, 1, 1, 0.13)))
		theme.set_stylebox("pressed", type, _button_box(Color(ACCENT, 0.85)))
		theme.set_stylebox("disabled", type, _button_box(Color(1, 1, 1, 0.04)))
	# The focus a keyboard or the remote moves (#228); a click shows none (the project's
	# gui/common/show_focus_state_on_pointer_event).
	for type: String in ["Button", "OptionButton", "CheckBox", "CheckButton"]:
		theme.set_stylebox("focus", type, focus_ring())
	theme.set_stylebox("tab_focus", "TabBar", focus_ring())
	# Every state: one left to Godot's default has margins, so a ticked box shifted and cut its
	# text off when hovered.
	for state: String in ["normal", "hover", "pressed", "hover_pressed", "disabled"]:
		theme.set_stylebox(state, "CheckBox", StyleBoxEmpty.new())
	# The default boxes are dark grey, invisible on dark panels.
	theme.set_icon("unchecked", "CheckBox", _check_icon(false))
	theme.set_icon("checked", "CheckBox", _check_icon(true))
	theme.set_icon("unchecked_disabled", "CheckBox", _check_icon(false, 0.35))
	theme.set_icon("checked_disabled", "CheckBox", _check_icon(true, 0.35))

	var field: StyleBoxFlat = _box(SURFACE, 10, 12, 8)
	theme.set_stylebox("normal", "LineEdit", field)
	theme.set_stylebox("focus", "LineEdit", _box(Color(1, 1, 1, 0.12), 10, 12, 8))
	theme.set_color("font_color", "LineEdit", TEXT)
	# Long texts to read, such as the licences, and lists to pick from beside them.
	theme.set_stylebox("normal", "TextEdit", field)
	theme.set_stylebox("read_only", "TextEdit", field)
	theme.set_stylebox("focus", "TextEdit", StyleBoxEmpty.new())
	theme.set_color("font_color", "TextEdit", TEXT)
	theme.set_color("font_readonly_color", "TextEdit", TEXT)
	theme.set_stylebox("panel", "ItemList", _box(SURFACE, 10, 6, 6))
	theme.set_stylebox("focus", "ItemList", StyleBoxEmpty.new())
	theme.set_stylebox("hovered", "ItemList", _box(SURFACE, 8, 8, 4))
	for state: String in [
		"selected", "selected_focus", "hovered_selected", "hovered_selected_focus"
	]:
		theme.set_stylebox(state, "ItemList", _box(Color(ACCENT, 0.18), 8, 8, 4))
	theme.set_color("font_color", "ItemList", MUTED)
	theme.set_color("font_hovered_color", "ItemList", TEXT)
	theme.set_color("font_selected_color", "ItemList", TEXT)
	theme.set_constant("v_separation", "ItemList", 6)
	theme.set_color("guide_color", "ItemList", Color(0, 0, 0, 0))

	theme.set_stylebox("background", "ProgressBar", _box(Color(1, 1, 1, 0.1), 6, 0, 4))
	theme.set_stylebox("fill", "ProgressBar", _box(ACCENT, 6, 0, 4))

	theme.set_stylebox("slider", "HSlider", _box(Color(1, 1, 1, 0.14), 4, 0, 3))
	theme.set_stylebox("grabber_area", "HSlider", _box(ACCENT, 4, 0, 3))
	theme.set_stylebox("grabber_area_highlight", "HSlider", _box(ACCENT, 4, 0, 3))

	# Dialogs are separate windows: they need the theme set on them and their own frame.
	# Square: the frame rounds the window; rounded corners here would let the window's grey
	# background show through. Not anti-aliased either: that fades the outermost pixels, and
	# the grey showed through them as thin lines along the dialog's edges (#48).
	var dialog: StyleBoxFlat = _box(Color(0.09, 0.1, 0.12), 0, 20, 16)
	dialog.anti_aliasing = false
	theme.set_stylebox("panel", "AcceptDialog", dialog)
	var frame: StyleBoxFlat = _box(Color(0.09, 0.1, 0.12), RADIUS, 0, 0)
	frame.expand_margin_top = 32
	frame.expand_margin_left = 6
	frame.expand_margin_right = 6
	frame.expand_margin_bottom = 6
	frame.border_color = Color(1, 1, 1, 0.08)
	frame.set_border_width_all(1)
	theme.set_stylebox("embedded_border", "Window", frame)
	theme.set_stylebox("embedded_unfocused_border", "Window", frame)
	theme.set_color("title_color", "Window", TEXT)

	# Tabs: plain captions, the current one underlined in the accent colour.
	var tab: StyleBoxFlat = _box(Color(0, 0, 0, 0), 0, 14, 8)
	var current_tab: StyleBoxFlat = tab.duplicate()
	current_tab.border_color = ACCENT
	current_tab.border_width_bottom = 2
	theme.set_stylebox("tab_selected", "TabContainer", current_tab)
	theme.set_stylebox("tab_unselected", "TabContainer", tab)
	theme.set_stylebox("tab_hovered", "TabContainer", tab)
	# The main navigation beside the logo: the current tab a soft accent pill.
	theme.set_stylebox("tab_selected", "TabBar", _box(Color(ACCENT, 0.18), 10, 14, 6))
	theme.set_stylebox("tab_unselected", "TabBar", _box(Color(0, 0, 0, 0), 10, 14, 6))
	theme.set_stylebox("tab_hovered", "TabBar", _box(SURFACE, 10, 14, 6))
	theme.set_constant("h_separation", "TabBar", 4)
	for tabs: String in ["TabContainer", "TabBar"]:
		theme.set_color("font_selected_color", tabs, TEXT)
		theme.set_color("font_unselected_color", tabs, MUTED)
		theme.set_color("font_hovered_color", tabs, TEXT)
	theme.set_stylebox("tabbar_background", "TabContainer", StyleBoxEmpty.new())
	var tab_panel: StyleBoxFlat = _box(Color(0, 0, 0, 0), 0, 0, 16)
	tab_panel.border_color = Color(1, 1, 1, 0.08)
	tab_panel.border_width_top = 1
	theme.set_stylebox("panel", "TabContainer", tab_panel)

	# Scroll bars: a slim light thumb, no track.
	for bar: String in ["VScrollBar", "HScrollBar"]:
		theme.set_stylebox("scroll", bar, _box(Color(0, 0, 0, 0), 3, 3, 3))
		theme.set_stylebox("grabber", bar, _box(Color(1, 1, 1, 0.18), 3, 3, 3))
		theme.set_stylebox("grabber_highlight", bar, _box(Color(1, 1, 1, 0.3), 3, 3, 3))
		theme.set_stylebox("grabber_pressed", bar, _box(Color(ACCENT, 0.6), 3, 3, 3))

	var popup: StyleBoxFlat = _box(Color(0.09, 0.1, 0.12, 0.98), 10, 6, 6)
	theme.set_stylebox("panel", "PopupMenu", popup)
	theme.set_stylebox("hover", "PopupMenu", _box(Color(ACCENT, 0.35), 6, 8, 4))
	theme.set_color("font_color", "PopupMenu", TEXT)
	# A panel opening under a button, e.g. the gallery's filters (R85).
	var popover: StyleBoxFlat = _box(Color(0.09, 0.1, 0.12, 0.98), 12, 16, 14)
	popover.border_color = Color(1, 1, 1, 0.1)
	popover.set_border_width_all(1)
	theme.set_stylebox("panel", "PopupPanel", popover)
	return theme


## The translucent card used for HUD and setup panels; opaque with `alpha` 1, for a panel
## over other windows rather than the 3D scene.
static func panel(alpha: float = PANEL.a) -> StyleBoxFlat:
	var box: StyleBoxFlat = _box(Color(PANEL, alpha), RADIUS, 16, 14)
	box.border_color = Color(1, 1, 1, 0.07)
	box.set_border_width_all(1)
	box.shadow_color = Color(0, 0, 0, 0.22)
	box.shadow_size = 12
	box.anti_aliasing = true
	return box


## The main action of a screen (e.g. Ride), in the accent colour.
static func accent_button() -> StyleBoxFlat:
	return _box(ACCENT, 12, 18, 10)


## Marks `button` as deleting something: tinted red with light red text and icon, solid red
## when pressed. Disabled, it looks like any other disabled button. The boxes keep the
## theme's margins so the button stays the size of its neighbours.
static func danger_button(button: Button) -> void:
	button.add_theme_stylebox_override("normal", _button_box(Color(DANGER, 0.2)))
	button.add_theme_stylebox_override("hover", _button_box(Color(DANGER, 0.34)))
	button.add_theme_stylebox_override("pressed", _button_box(Color(DANGER, 0.85)))
	var light: Color = DANGER.lightened(0.45)
	# Dialogs focus their OK button, which then draws in the focus colours.
	for state: String in ["font_color", "font_focus_color", "font_hover_color"]:
		button.add_theme_color_override(state, light)
	# Icons are drawn near-white and tinted by these.
	for state: String in ["icon_normal_color", "icon_focus_color", "icon_hover_color"]:
		button.add_theme_color_override(state, light)


## A button background for use over the 3D scene, as dark as the HUD panels.
static func hud_button() -> StyleBoxFlat:
	return _button_box(PANEL)


## The slim bar of icon buttons over the 3D scene (#189).
static func bar() -> StyleBoxFlat:
	return _box(PANEL, 12, 6, 4)


## An icon button's box: square around a 20 px icon and as tall as a text button beside it.
## Every state needs the same margins, or the icon shifts when the box changes on hover.
static func icon_button(color: Color) -> StyleBoxFlat:
	return _box(color, 10, 9, 9)


## A list entry that can be dragged; the highlighted one marks the HUD's large figure.
static func chip(highlighted: bool) -> StyleBoxFlat:
	var box: StyleBoxFlat = _box(Color(ACCENT, 0.22) if highlighted else SURFACE, 8, 12, 4)
	box.border_color = Color(ACCENT, 0.6) if highlighted else Color(1, 1, 1, 0.06)
	box.set_border_width_all(1)
	return box


## A chip that turns on and off, e.g. a filter (R67): highlighted while on. `text` is shown as
## given and translated by the button.
static func toggle_chip(text: String) -> Button:
	var button: Button = Button.new()
	button.text = text
	button.toggle_mode = true
	button.focus_mode = Control.FOCUS_NONE
	button.mouse_default_cursor_shape = Control.CURSOR_POINTING_HAND
	# Its own height, not that of a taller neighbour in its row.
	button.size_flags_vertical = Control.SIZE_SHRINK_CENTER
	button.add_theme_font_size_override("font_size", 13)
	var hover: StyleBoxFlat = chip(false)
	hover.bg_color = Color(1, 1, 1, 0.12)
	button.add_theme_stylebox_override("normal", chip(false))
	button.add_theme_stylebox_override("hover", hover)
	button.add_theme_stylebox_override("pressed", chip(true))
	button.add_theme_stylebox_override("hover_pressed", chip(true))
	return button


## A plain button as large as a `toggle_chip`, for an action in a row of chips: `text` in
## the accent colour, with `icon`.
static func chip_action(text: String, icon: String) -> Button:
	var button: Button = Button.new()
	button.text = text
	button.icon = UiIcons.texture(icon, 16, ACCENT)
	button.focus_mode = Control.FOCUS_NONE
	button.mouse_default_cursor_shape = Control.CURSOR_POINTING_HAND
	button.size_flags_vertical = Control.SIZE_SHRINK_CENTER
	button.add_theme_font_size_override("font_size", 13)
	button.add_theme_color_override("font_color", ACCENT)
	button.add_theme_color_override("font_hover_color", TEXT)
	button.add_theme_color_override("font_pressed_color", TEXT)
	var plain: StyleBoxFlat = chip(false)
	plain.bg_color = Color(0, 0, 0, 0)
	plain.border_color = Color(0, 0, 0, 0)
	for state: String in ["normal", "hover", "pressed", "hover_pressed", "disabled", "focus"]:
		button.add_theme_stylebox_override(state, plain)
	return button


## A highlighted chip as large as a button, for a mark in a row of buttons (the active rider).
static func button_chip() -> StyleBoxFlat:
	var box: StyleBoxFlat = _button_box(Color(ACCENT, 0.22))
	box.border_color = Color(ACCENT, 0.6)
	box.set_border_width_all(1)
	return box


## A rider's initial in a round accent badge, standing in for a face (#191).
static func initial(rider_name: String, size: int = 40) -> PanelContainer:
	var badge: PanelContainer = PanelContainer.new()
	badge.custom_minimum_size = Vector2(size, size)
	badge.size_flags_vertical = Control.SIZE_SHRINK_CENTER
	badge.add_theme_stylebox_override("panel", _box(ACCENT, size / 2, 0, 0))
	var label: Label = Label.new()
	label.auto_translate_mode = Node.AUTO_TRANSLATE_MODE_DISABLED
	label.text = rider_name.strip_edges().left(1).to_upper()
	label.horizontal_alignment = HORIZONTAL_ALIGNMENT_CENTER
	label.vertical_alignment = VERTICAL_ALIGNMENT_CENTER
	label.add_theme_font_size_override("font_size", size / 2)
	badge.add_child(label)
	return badge


## A caption label: small, muted, upper case; `text` follows the interface language.
static func caption(text: String) -> Label:
	var label: Label = Label.new()
	# Upper case only when drawn: the label looks up `text` itself, again whenever the rider's
	# language changes, as captions are built before the rider's profile picks the language.
	label.auto_translate_mode = Node.AUTO_TRANSLATE_MODE_ALWAYS
	label.uppercase = true
	label.text = text
	label.add_theme_font_size_override("font_size", 11)
	label.add_theme_color_override("font_color", MUTED)
	return label


## A value label in the given size.
static func value(size: int) -> Label:
	var label: Label = Label.new()
	label.text = "--"
	label.add_theme_font_size_override("font_size", size)
	return label


## Lets `option` be narrower than its longest item, which it then cuts short, so the row it is
## in fits a small window (R94); the list still shows the items whole.
static func narrowable(option: OptionButton) -> void:
	option.fit_to_longest_item = false
	option.clip_text = true


## `content` scrolling up and down, never sideways, for a page or dialog taller than a small
## window (R94).
static func scrolling(content: Control) -> ScrollContainer:
	var scroll: ScrollContainer = ScrollContainer.new()
	scroll_down_only(scroll)
	scroll.add_theme_constant_override("scrollbar_h_separation", 12)
	content.size_flags_horizontal = Control.SIZE_EXPAND_FILL
	scroll.add_child(content)
	return scroll


## Lets `scroll` scroll up and down but never sideways, and keeps its bar's room whether the
## bar shows or not (R53). Content that reflows (R94) is taller when narrower: had the bar
## taken its room as it came, the narrower content could need it, the wider one not, and the
## layout would swap between the two without end until the app crashed.
static func scroll_down_only(scroll: ScrollContainer) -> void:
	scroll.horizontal_scroll_mode = ScrollContainer.SCROLL_MODE_DISABLED
	scroll.vertical_scroll_mode = ScrollContainer.SCROLL_MODE_RESERVE


static func _box(color: Color, radius: int, horizontal: int, vertical: int) -> StyleBoxFlat:
	var box: StyleBoxFlat = StyleBoxFlat.new()
	box.bg_color = color
	box.set_corner_radius_all(radius)
	box.content_margin_left = horizontal
	box.content_margin_right = horizontal
	box.content_margin_top = vertical
	box.content_margin_bottom = vertical
	box.anti_aliasing = true
	return box


## The ring round what has the focus, for keyboard and remote navigation (#228): an accent
## line `outside` pixels outside a control's edge, rounded as its corners (`radius`) are.
static func focus_ring(radius: int = 10, outside: int = 3) -> StyleBoxFlat:
	var ring: StyleBoxFlat = StyleBoxFlat.new()
	ring.draw_center = false
	ring.set_border_width_all(2)
	ring.border_color = ACCENT
	ring.set_corner_radius_all(radius + outside)
	ring.set_expand_margin_all(outside)
	ring.anti_aliasing = true
	return ring


static func _button_box(color: Color) -> StyleBoxFlat:
	return _box(color, 10, 14, 9)


## A time as m:ss, or h:mm:ss from an hour.
static func duration(seconds: float) -> String:
	var total: int = roundi(seconds)
	if total >= 3600:
		return "%d:%02d:%02d" % [total / 3600, total / 60 % 60, total % 60]
	return "%d:%02d" % [total / 60, total % 60]


## A size on disk in the units riders know from their drives, e.g. "238 MB" or "1.3 GB":
## powers of 1000, not the 1024 of MiB and GiB.
static func bytes(size: int) -> String:
	var units: Array[String] = ["B", "kB", "MB", "GB", "TB"]
	var value: float = size
	var unit: int = 0
	# From 999.5 on, the value would round to 1000 of its unit: one of the next instead.
	while value >= 999.5 and unit < units.size() - 1:
		value /= 1000.0
		unit += 1
	if unit > 0 and value < 9.95:
		return "%.1f %s" % [value, units[unit]]
	return "%d %s" % [roundi(value), units[unit]]


## Local date and time of a Unix timestamp, e.g. "Sat 3 Oct 2026, 07:15"; the date alone is
## the part before the comma.
static func date(unix_s: int) -> String:
	var bias_minutes: int = Time.get_time_zone_from_system().get("bias", 0)
	var local: Dictionary = Time.get_datetime_dict_from_unix_time(unix_s + bias_minutes * 60)
	# i18n-begin
	var weekdays: Array[String] = ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"]
	var months: Array[String] = [
		"Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"
	]
	# i18n-end
	var weekday: int = local["weekday"]
	var month: int = local["month"]
	return (
		"%s %d %s %d, %02d:%02d"
		% [
			TranslationServer.translate(weekdays[weekday]),
			local["day"],
			TranslationServer.translate(months[month - 1]),
			local["year"],
			local["hour"],
			local["minute"]
		]
	)


## An 18 px check box: a light outline, or an accent square with a white tick.
## A ticked box, e.g. for a finished step.
static func check_icon() -> ImageTexture:
	return _check_icon(true)


static func _check_icon(checked: bool, alpha: float = 1.0) -> ImageTexture:
	const SIZE: int = 18
	var image: Image = Image.create_empty(SIZE, SIZE, false, Image.FORMAT_RGBA8)
	var outline: Color = Color(TEXT, 0.55 * alpha)
	var fill: Color = Color(ACCENT, alpha)
	for y: int in range(1, SIZE - 1):
		for x: int in range(1, SIZE - 1):
			# Corners left out for a slightly rounded look.
			var corner: bool = (x == 1 or x == SIZE - 2) and (y == 1 or y == SIZE - 2)
			if corner:
				continue
			var edge: bool = x <= 2 or y <= 2 or x >= SIZE - 3 or y >= SIZE - 3
			if checked:
				image.set_pixel(x, y, fill)
			elif edge:
				image.set_pixel(x, y, outline)
	if checked:
		var tick: Color = Color(1, 1, 1, alpha)
		for i: int in range(4):
			image.set_pixel(4 + i, 8 + i, tick)
			image.set_pixel(4 + i, 9 + i, tick)
		for i: int in range(7):
			image.set_pixel(7 + i, 11 - i, tick)
			image.set_pixel(7 + i, 10 - i, tick)
	return ImageTexture.create_from_image(image)
