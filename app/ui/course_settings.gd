class_name CourseSettings
extends VBoxContainer
## How this computer keeps its courses (R78), on the settings' Courses tab: their 3D worlds
## kept once built, so that riding a course again starts at once (R101), each with the room it
## takes, to delete (R136), and the room they take together.

## Keeping the courses' worlds, on this computer.
var _keep: CheckBox = CheckBox.new()
var _note: Label = Label.new()
## The kept worlds by course name: the room each takes and a bin to delete it, and the total.
var _worlds: VBoxContainer = VBoxContainer.new()
var _orqwa: OrqwaApp


func _init() -> void:
	add_theme_constant_override("separation", 16)
	_keep.toggled.connect(_on_keep_toggled)
	add_child(_keep)
	_note.add_theme_color_override("font_color", UiTheme.MUTED)
	_note.autowrap_mode = TextServer.AUTOWRAP_WORD_SMART
	# A width to wrap at before the dialog lays it out, as on the Graphics tab.
	_note.custom_minimum_size = Vector2(600, 0)
	add_child(_note)
	_worlds.add_theme_constant_override("separation", 8)
	add_child(_worlds)
	_translate()


func bind(orqwa: OrqwaApp) -> void:
	_orqwa = orqwa
	refresh()


## Shows the setting and each kept world with the room it takes now: they grow with every
## course ridden.
func refresh() -> void:
	if _orqwa != null:
		_keep.set_pressed_no_signal(_orqwa.keeps_worlds())
	_translate()
	_show_worlds()


func _notification(what: int) -> void:
	if what == NOTIFICATION_TRANSLATION_CHANGED and is_node_ready():
		_translate()
		_show_worlds()


func _on_keep_toggled(on: bool) -> void:
	if _orqwa != null:
		_orqwa.set_keeps_worlds(on)
	_show_worlds()


func _translate() -> void:
	_keep.text = tr("Keep courses ready to ride")
	var note: String = (
		tr(
			"Each course's 3D world is kept on this computer once built: the next ride starts at once."
		)
		+ " "
		+ tr("A world takes about 10 MB per kilometre of route.")
		+ " "
		+ tr(
			"After an update that changes how worlds look, a course's page offers to regenerate it."
		)
	)
	_note.text = note


func _show_worlds() -> void:
	# Turned off, they are being deleted.
	var shown: bool = _orqwa != null and _keep.button_pressed
	if shown:
		_list(_orqwa.kept_worlds(), _orqwa.kept_worlds_size())
	_worlds.visible = shown


## Lists `worlds` as `OrqwaApp.kept_worlds()` gives them, under them the `total` bytes the kept
## worlds take.
func _list(worlds: Array, total: int) -> void:
	for child: Node in _worlds.get_children():
		_worlds.remove_child(child)
		child.queue_free()
	_worlds.add_child(UiTheme.caption(tr("Kept worlds")))
	if worlds.is_empty():
		var none: Label = Label.new()
		none.text = tr("None yet.")
		none.add_theme_color_override("font_color", UiTheme.MUTED)
		_worlds.add_child(none)
		return
	# The sizes in a column of their own, the bins in another.
	var grid: GridContainer = GridContainer.new()
	grid.columns = 3
	grid.add_theme_constant_override("h_separation", 16)
	grid.add_theme_constant_override("v_separation", 8)
	for world: Dictionary in worlds:
		var names: PackedStringArray = world.get("names", PackedStringArray())
		var course: Label = Label.new()
		course.auto_translate_mode = Node.AUTO_TRANSLATE_MODE_DISABLED
		course.text = ", ".join(names)
		course.size_flags_horizontal = Control.SIZE_EXPAND_FILL
		# A long name is cut short rather than pushing the bins out of a small window (R94).
		course.text_overrun_behavior = TextServer.OVERRUN_TRIM_ELLIPSIS
		grid.add_child(course)
		var size: Label = Label.new()
		size.auto_translate_mode = Node.AUTO_TRANSLATE_MODE_DISABLED
		var bytes: int = world.get("size", 0)
		size.text = UiTheme.bytes(bytes)
		size.horizontal_alignment = HORIZONTAL_ALIGNMENT_RIGHT
		size.add_theme_color_override("font_color", UiTheme.MUTED)
		grid.add_child(size)
		var delete: Button = Button.new()
		delete.icon = UiIcons.texture("bin")
		delete.tooltip_text = tr("Delete world")
		delete.focus_mode = Control.FOCUS_NONE
		var path: String = world.get("path", "")
		# Deferred: the list it is in is built anew.
		delete.pressed.connect(_delete.bind(path), CONNECT_DEFERRED)
		UiTheme.danger_button(delete)
		grid.add_child(delete)
	# All of it, also a world still being written.
	var caption: Label = UiTheme.caption(tr("Total"))
	grid.add_child(caption)
	var sum: Label = Label.new()
	sum.auto_translate_mode = Node.AUTO_TRANSLATE_MODE_DISABLED
	sum.text = UiTheme.bytes(total)
	sum.horizontal_alignment = HORIZONTAL_ALIGNMENT_RIGHT
	grid.add_child(sum)
	# No bin: turning the setting off deletes them all.
	grid.add_child(Control.new())
	_worlds.add_child(grid)


## Deletes the kept world of the course at `path`; its next ride builds it again.
func _delete(path: String) -> void:
	if _orqwa != null:
		_orqwa.delete_kept_world(path)
	_show_worlds()
