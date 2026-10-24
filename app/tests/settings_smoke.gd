extends SceneTree
## Headless checks of the settings (R83): the cog at the top right of the start page opens
## them, with this computer's graphics, no longer among the devices, its courses' kept worlds
## (R101), and About: the version, the commit and the licences of everything in the app.
## Run: godot --headless --path app -s res://tests/settings_smoke.gd

const MAIN_SCENE: String = "res://scenes/main.tscn"

var _failed: bool = false


func _initialize() -> void:
	_run.call_deferred()


func _run() -> void:
	TranslationServer.set_locale("en")
	var main: Control = (load(MAIN_SCENE) as PackedScene).instantiate()
	root.add_child(main)
	await process_frame
	var start: StartPage = main.get_node("StartPage")
	await _cog(start)
	var dialog: SettingsDialog = start.get("_settings")
	var orqwa: OrqwaApp = main.get_node("Orqwa")
	await _graphics(orqwa, dialog)
	var world: RideWorld = main.get_node("World")
	_tree_quality(world)
	_tree_density_override(world)
	_courses(orqwa, dialog)
	_sound(orqwa, dialog)
	_about(dialog)
	_updates(orqwa, dialog, main)
	await _in_german(dialog)
	dialog.hide()
	main.free()
	if not _failed:
		print("SETTINGS SMOKE TEST PASSED")
	quit(1 if _failed else 0)


## The cog closes the header's row, right of the tabs, and opens the settings on Graphics; the
## Devices tab keeps the devices alone.
func _cog(start: StartPage) -> void:
	var header: HBoxContainer = start.get_node("%Header")
	var cog: Button = start.get("_settings_button")
	var tab_bar: TabBar = start.get("_tab_bar")
	_check(cog.get_parent() == header, "the cog is in the header")
	_check(cog.get_index() == header.get_child_count() - 1, "at its end")
	_check(
		absf(cog.get_global_rect().end.x - header.get_global_rect().end.x) < 1.0,
		"at the top right: %s of %s" % [cog.get_global_rect(), header.get_global_rect()]
	)
	_check(cog.get_global_rect().position.x >= tab_bar.get_global_rect().end.x, "beside the tabs")
	_check(cog.tooltip_text == "Settings", "it says what it opens")
	_check(tab_bar.get_tab_title(StartPage.Tab.DEVICES) == "Devices", "the tab is Devices")
	var devices: DevicesTab = start.get("_devices")
	for text: String in ["Graphics", "Quality", "Full screen (F11)"]:
		_check(not _shows(devices, text), "no %s among the devices" % text)
	var dialog: SettingsDialog = start.get("_settings")
	var tabs: TabContainer = dialog.get("_tabs")
	tabs.current_tab = 1
	# Headless, the window is tiny unless sized like the app's.
	root.size = Vector2i(1600, 900)
	await process_frame
	cog.pressed.emit()
	await process_frame
	_check(dialog.visible, "the cog opens the settings")
	var window: Rect2i = Rect2i(Vector2i.ZERO, root.size)
	_check(
		window.encloses(Rect2i(dialog.position, dialog.size)),
		"within the window: %s in %s" % [Rect2i(dialog.position, dialog.size), window]
	)
	_check(tabs.get_tab_count() == 4, "four tabs")
	var titles: Array = range(tabs.get_tab_count()).map(tabs.get_tab_title)
	_check(titles == ["Graphics", "Courses", "Sound", "About"], "titled: %s" % [titles])
	_check(tabs.current_tab == 0, "opening on Graphics")


## The quality is kept for this computer (R43, R78); full screen fills the app's window, not
## the settings' own (#218).
func _graphics(orqwa: OrqwaApp, dialog: SettingsDialog) -> void:
	var graphics: GraphicsSettings = dialog.get("_graphics")
	var quality: OptionButton = graphics.get("_quality")
	var before: String = orqwa.graphics_quality()
	for i: int in range(quality.item_count):
		quality.select(i)
		quality.item_selected.emit(i)
		var key: String = quality.get_item_metadata(i)
		_check(orqwa.graphics_quality() == key, "choosing %s keeps it" % key)
	orqwa.set_graphics_quality(before)
	var box: CheckBox = graphics.get("_fullscreen")
	# Headless, the app's window has no mode of its own: it is checked on a real display.
	var headless: bool = DisplayServer.get_name() == "headless"
	orqwa.set_fullscreen(false)
	box.button_pressed = true
	await process_frame
	_check(orqwa.fullscreen(), "full screen, also next time")
	_check(headless or FullScreen.is_on(root), "the app's window fills the screen")
	_check(not FullScreen.is_on(dialog), "not the settings' window")
	box.button_pressed = false
	_check(not orqwa.fullscreen(), "and back in a window")
	_details(orqwa, graphics)


## The details set apart from the preset (R90): one choice per option the core knows, Auto
## first; a choice is kept for this computer, Auto and the reset follow the preset again.
func _details(orqwa: OrqwaApp, graphics: GraphicsSettings) -> void:
	var details: Dictionary = graphics.get("_details")
	var options: Dictionary = OrqwaApp.graphics_option_values()
	_check(details.size() == options.size(), "a choice per option: %d" % details.size())
	var shadows: OptionButton = details["shadows"]
	var first: String = shadows.get_item_metadata(0)
	_check(first == "auto", "Auto first")
	_check(_shadows(orqwa) == "auto", "following the preset at first")
	var last: int = shadows.item_count - 1
	shadows.select(last)
	shadows.item_selected.emit(last)
	_check(_shadows(orqwa) == "ultra", "the choice is kept")
	_check(not orqwa.set_graphics_option("shadows", "extreme"), "unknown values refused")
	var trees: OptionButton = details["tree_density"]
	_check(trees.item_count == 5, "tree density offers Auto and all four levels")
	var first_density: String = trees.get_item_metadata(0)
	_check(first_density == "auto", "tree density follows the preset by default")
	trees.select(1)
	trees.item_selected.emit(1)
	var chosen: Dictionary = orqwa.graphics_options()
	var chosen_density: String = chosen["tree_density"]
	_check(chosen_density == "low", "choosing a density stores it on this computer")
	graphics.call("_reset_details")
	_check(_shadows(orqwa) == "auto", "the reset follows the preset again")
	_check(shadows.selected == 0, "and shows it")
	_check(trees.selected == 0, "reset also returns tree density to Auto")
	var reset: Dictionary = orqwa.graphics_options()
	var reset_density: String = reset["tree_density"]
	_check(reset_density == "auto", "reset removes the stored density override")


## R108: tree distance increases with quality, also for chunks uploaded before a ride;
## View distance changes range independently from the preset's tree density.
func _tree_quality(world: RideWorld) -> void:
	var terrain: Node3D = world.get_node("Terrain")
	var buffer: PackedFloat32Array = PackedFloat32Array(
		[1.0, 0.0, 0.0, 20.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 30.0, 1.0, 1.0, 1.0, 1.0]
	)
	var previous_range: float = 0.0
	var previous_density: float = 0.0
	for preset: String in ["low", "medium", "high", "ultra"]:
		world.apply_quality(preset)
		var quality: Dictionary = world.get("_quality")
		var density: float = quality["tree_density"]
		var tree_range: float = quality["tree_range"]
		var tree: MultiMeshInstance3D = VegetationModels.instance(
			"conifer_tall", buffer, tree_range, 1.0
		)
		_check(tree.visibility_range_end > previous_range, preset + " draws trees farther")
		_check(density > previous_density, preset + " draws more trees")
		previous_range = tree.visibility_range_end
		previous_density = density
		tree.free()
	# Range changes alone update uploaded trees without rebuilding the chunks.
	world.apply_quality("low")
	var low_quality: Dictionary = world.get("_quality")
	var low_density: float = low_quality["tree_density"]
	var low_range: float = low_quality["tree_range"]
	var tree: MultiMeshInstance3D = VegetationModels.instance(
		"conifer_tall", buffer, low_range, 1.0
	)
	terrain.add_child(tree)
	_check(low_range <= 600.0, "Low keeps trees close")
	_check(tree.visibility_range_end_margin <= low_range * 0.2, "Low keeps the fade short")
	world.apply_quality("low", {"view_distance": "ultra"})
	var overridden: Dictionary = world.get("_quality")
	var overridden_density: float = overridden["tree_density"]
	_check(tree.visibility_range_end == previous_range, "View distance controls tree range")
	_check(overridden_density == low_density, "density stays with Low")
	world.apply_quality("low", {"view_distance": "low"})
	_check(tree.visibility_range_end == low_range, "range returns without accumulating scaling")
	terrain.remove_child(tree)
	tree.free()
	world.apply_quality("ultra", {"view_distance": "low"})
	var dense_nearby: Dictionary = world.get("_quality")
	var dense_range: float = dense_nearby["tree_range"]
	_check(dense_range == low_range, "a short range also works with dense trees")
	# Changing density discards already uploaded chunks so the next ride uses fewer trees.
	terrain.add_child(Node3D.new())
	world.set("_next_chunk", 1)
	world.apply_quality("low")
	var next_chunk: int = world.get("_next_chunk")
	_check(terrain.get_child_count() == 0, "old density is removed from the scene")
	_check(next_chunk == 0, "a changed density restarts chunk uploads")
	world.apply_quality("medium")


## R109: density overrides only the uploaded population; distance and other effects stay
## with their own settings, and Auto resumes following the current preset.
func _tree_density_override(world: RideWorld) -> void:
	var previous_density: float = 0.0
	for level: String in ["low", "medium", "high", "ultra"]:
		world.apply_quality("ultra", {"tree_density": level, "view_distance": "low"})
		var quality: Dictionary = world.get("_quality")
		var density: float = quality["tree_density"]
		var tree_range: float = quality["tree_range"]
		var shadow_distance: float = quality["shadow_distance"]
		_check(density > previous_density, level + " raises the overridden density")
		_check(tree_range == 600.0, "View distance stays independent from density")
		_check(shadow_distance == 900.0, "Ultra shadows stay with the quality preset")
		previous_density = density
	world.apply_quality("low", {"tree_density": "ultra"})
	var dense: Dictionary = world.get("_quality")
	var dense_density: float = dense["tree_density"]
	var nearby: float = dense["tree_range"]
	_check(dense_density == 1.0 and nearby == 600.0, "dense trees can stay nearby on Low")
	world.apply_quality("low", {"tree_density": "auto"})
	var automatic: Dictionary = world.get("_quality")
	var auto_density: float = automatic["tree_density"]
	_check(auto_density == 0.125, "Auto follows Low again")
	world.apply_quality("medium")


func _shadows(orqwa: OrqwaApp) -> String:
	var options: Dictionary = orqwa.graphics_options()
	return options["shadows"]


## About: the version, the branch and the commit, then each part's licence notice with its full
## text.
func _about(dialog: SettingsDialog) -> void:
	var about: AboutPage = dialog.get("_about")
	var version: Label = about.get("_version")
	var commit: String = OrqwaCore.commit()
	var branch: String = OrqwaCore.branch()
	_check(version.text.contains(OrqwaCore.version()), "the version: %s" % version.text)
	_check(commit.is_empty() or version.text.contains(commit), "and the commit: %s" % commit)
	_check(
		commit.is_empty() or branch.is_empty() or version.text.contains(branch),
		"and the branch: %s" % branch
	)
	var report: String = about.call("_version_report")
	_check(report.begins_with("ORQWA " + OrqwaCore.version()), "copied for a bug report")
	_check(report.contains(OrqwaCore.build()), "with the branch and commit")
	var godot: String = Engine.get_version_info()["string"]
	_check(report.contains(godot), "with Godot's version")
	var parts: ItemList = about.get("_parts")
	var titles: PackedStringArray = []
	for i: int in range(parts.item_count):
		titles.append(parts.get_item_text(i))
	var listed: PackedStringArray = [
		"ORQWA", "Godot Engine", "FFmpeg", "Rust libraries", "Models and data"
	]
	_check(titles == listed, "ORQWA, then what it is built from: %s" % titles)
	var notice: TextEdit = about.get("_notice")
	_check(notice.text.contains("GNU GENERAL PUBLIC LICENSE"), "ORQWA's licence shows first")
	_check(notice.text.contains(OrqwaCore.source_url()), "with where its source code is")
	var expected: Array = [
		[1, Engine.get_license_text().left(60)],
		[1, "FreeType"],
		[2, "GNU LESSER GENERAL PUBLIC LICENSE"],
		[3, "Permission is hereby granted, free of charge"],
		[4, "Human Base Meshes"],
	]
	for part: Array in expected:
		var index: int = part[0]
		var text: String = part[1]
		parts.select(index)
		parts.item_selected.emit(index)
		_check(notice.text.contains(text), "%s: %s" % [titles[index], text])


## The chime of the ride's moments (R125): on until turned off, kept for the installation.
func _sound(orqwa: OrqwaApp, dialog: SettingsDialog) -> void:
	var sound: SoundSettings = dialog.get("_sound")
	var sounds: CheckBox = sound.get("_sounds")
	sound.refresh()
	_check(sound.is_on() == orqwa.sounds(), "the box shows the setting")
	sounds.button_pressed = false
	_check(not orqwa.sounds(), "turned off")
	sounds.button_pressed = true
	_check(orqwa.sounds(), "and on again")


## Keeping the courses' worlds (R101): off until this computer turns it on, which lists them;
## off again, they are deleted.
func _courses(orqwa: OrqwaApp, dialog: SettingsDialog) -> void:
	var courses: CourseSettings = dialog.get("_courses")
	var keep: CheckBox = courses.get("_keep")
	var note: Label = courses.get("_note")
	var worlds: VBoxContainer = courses.get("_worlds")
	orqwa.set_keeps_worlds(false)
	courses.refresh()
	_check(not keep.button_pressed, "worlds are not kept until asked")
	_check(keep.text == "Keep courses ready to ride", "the switch: %s" % keep.text)
	_check(note.text.contains("10 MB per kilometre"), "telling the room they take: %s" % note.text)
	_check(not worlds.visible, "no kept worlds to list")
	keep.button_pressed = true
	_check(orqwa.keeps_worlds(), "kept on this computer")
	_check(worlds.visible, "listing the kept worlds")
	_kept_worlds(courses, worlds)
	keep.button_pressed = false
	_check(not orqwa.keeps_worlds(), "and off again")
	_check(not worlds.visible, "they are deleted")


## Each kept world by the names of its courses, the room it takes and a bin to delete it
## (R136), and the room they take together in a row of its own, not the note.
func _kept_worlds(courses: CourseSettings, worlds: VBoxContainer) -> void:
	var note: Label = courses.get("_note")
	_check(not note.text.contains("kept worlds take"), "no room told in the note: %s" % note.text)
	courses.call("_list", [], 0)
	_check(_shows(worlds, "None yet."), "none kept yet")
	_check(not _shows(worlds, "Total"), "no total of nothing")
	var shared: PackedStringArray = PackedStringArray(["Lakeside", "Lakeside again"])
	var listed: Array[Dictionary] = [
		{"path": "/courses/lakeside.tqc", "names": shared, "size": 1288490189},
		{"path": "/courses/hills.tqc", "names": PackedStringArray(["Hills"]), "size": 104857600},
	]
	courses.call("_list", listed, 1288490189 + 104857600)
	_check(not _shows(worlds, "None yet."), "two kept")
	_check(_shows(worlds, "Lakeside, Lakeside again"), "a world two courses share, once")
	_check(_shows(worlds, "Hills"), "the other")
	for room: String in ["1.3 GB", "105 MB"]:
		_check(_shows(worlds, room), "each with the room it takes: %s" % room)
	_check(_shows(worlds, "Total") and _shows(worlds, "1.4 GB"), "and their total")
	_sizes()
	var bins: Array[Node] = worlds.find_children("*", "Button", true, false)
	_check(bins.size() == 2, "a bin for each: %d" % bins.size())
	for bin: Node in bins:
		var button: Button = bin
		var box: StyleBoxFlat = button.get_theme_stylebox("normal") as StyleBoxFlat
		_check(
			button.tooltip_text == "Delete world" and button.icon == UiIcons.texture("bin"),
			"a bin saying what it deletes: %s" % button.tooltip_text
		)
		_check(box != null and Color(box.bg_color, 1.0).is_equal_approx(UiTheme.DANGER), "in red")


## Sizes in the units riders know from their drives: powers of 1000, MB and GB, not MiB and
## GiB; one decimal below 10, as in "1.3 GB".
func _sizes() -> void:
	var expected: Dictionary[int, String] = {
		0: "0 B",
		999: "999 B",
		1500: "1.5 kB",
		104857600: "105 MB",
		238000000: "238 MB",
		999600000: "1.0 GB",
		1288490189: "1.3 GB",
		12400000000: "12 GB",
	}
	for size: int in expected:
		var shown: String = UiTheme.bytes(size)
		_check(shown == expected[size], "%d bytes: %s" % [size, shown])


## Looking for updates (R99): on at start-up until this computer turns it off; a newer
## version is offered on the start page, or once a ride is over, never during it.
func _updates(orqwa: OrqwaApp, dialog: SettingsDialog, main: Control) -> void:
	var about: AboutPage = dialog.get("_about")
	var at_start: CheckBox = about.get("_check_at_start")
	_check(at_start.button_pressed and orqwa.checks_for_updates(), "looks for updates at start")
	at_start.button_pressed = false
	_check(not orqwa.checks_for_updates(), "turned off for this computer")
	at_start.button_pressed = true
	_check(orqwa.checks_for_updates(), "and on again")
	var offer: UpdateDialog = main.get("_update")
	orqwa.update_available.emit("99.0.0", "https://example.org/v99.0.0", false)
	var status: Label = about.get("_update_status")
	_check(status.text.contains("99.0.0"), "the About tab tells: %s" % status.text)
	_check(offer.visible and offer.dialog_text.contains("99.0.0"), "offered on the start page")
	_check(offer.dialog_text.contains(OrqwaCore.version()), "beside the running version")
	_check(offer.ok_button_text == "Open the release page", "elsewhere, its page")
	offer.hide()
	offer.offer("99.0.0", "https://example.org/v99.0.0", true)
	_check(offer.ok_button_text == "Update now", "on Windows, installed by ORQWA")
	offer.hide()
	var start: StartPage = main.get_node("StartPage")
	start.hide()
	orqwa.update_available.emit("99.0.0", "https://example.org/v99.0.0", false)
	_check(not offer.visible, "not during a ride")
	main.call("_back_home")
	_check(offer.visible, "once the ride is over")
	offer.hide()


## Built in English before the rider's language is known, as the start page builds it.
func _in_german(dialog: SettingsDialog) -> void:
	TranslationServer.set_locale("de")
	await process_frame
	var tabs: TabContainer = dialog.get("_tabs")
	# The tab bar translates its titles as it draws them.
	var graphics: String = tabs.get_tab_bar().atr(tabs.get_tab_title(0))
	var courses_title: String = tabs.get_tab_bar().atr(tabs.get_tab_title(1))
	var sound_title: String = tabs.get_tab_bar().atr(tabs.get_tab_title(2))
	var about_title: String = tabs.get_tab_bar().atr(tabs.get_tab_title(3))
	_check(sound_title == "Ton", "Sound in German: %s" % sound_title)
	_check(graphics == "Grafik", "Graphics in German: %s" % graphics)
	_check(courses_title == "Routen", "Courses in German: %s" % courses_title)
	_check(about_title == "Über ORQWA", "About in German: %s" % about_title)
	_check(dialog.atr(dialog.title) == "Einstellungen", "titled: %s" % dialog.atr(dialog.title))
	var about: AboutPage = dialog.get("_about")
	var parts: ItemList = about.get("_parts")
	_check(parts.get_item_text(3) == "Rust-Bibliotheken", "parts: %s" % parts.get_item_text(3))
	var version: Label = about.get("_version")
	_check(version.text.begins_with("Version"), "the version: %s" % version.text)
	TranslationServer.set_locale("en")
	await process_frame


## Whether a label or button under `node` shows `text`.
func _shows(node: Node, text: String) -> bool:
	for control: Node in node.find_children("*", "", true, false):
		if (control is Label or control is Button) and control.get("text") == text:
			return true
	return false


func _check(condition: bool, what: String) -> void:
	if not condition:
		push_error("SETTINGS SMOKE TEST FAILED: " + what)
		_failed = true
