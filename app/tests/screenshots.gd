extends SceneTree
## Renders the ride screen in each camera mode to PNG files, for checking visuals without a
## GPU (software Vulkan). Run via scripts/screenshots.sh.

const DEFAULT_ROUTE: String = "res://../core/fixtures/gurtenstrasse.gpx"
const TIMEOUT_S: float = 180.0

var _main: Control


func _initialize() -> void:
	_create_main()
	_run.call_deferred()


func _create_main() -> void:
	_main = (load("res://scenes/main.tscn") as PackedScene).instantiate()
	root.add_child(_main)


func _run() -> void:
	var out_dir: String = OS.get_environment("SCREENSHOT_DIR")
	if OS.get_environment("SCREENSHOT_ERRORS_ONLY") == "1":
		await _error_details(out_dir)
		quit()
		return
	if OS.get_environment("SCREENSHOT_SUMMARY_ONLY") == "1":
		await _summary_only(out_dir)
		quit()
		return
	var orqwa: OrqwaApp = _main.get_node("Orqwa")
	var world: RideWorld = _main.get_node("World")
	var start: StartPage = _main.get_node("StartPage")
	_software_render(world)

	var route: String = OS.get_environment("SCREENSHOT_ROUTE")
	if route.is_empty():
		route = ProjectSettings.globalize_path(DEFAULT_ROUTE)
	var ride_s: float = OS.get_environment("SCREENSHOT_RIDE_S").to_float()
	# Another interface language, e.g. SCREENSHOT_LOCALE=de.
	var locale: String = OS.get_environment("SCREENSHOT_LOCALE")
	if not locale.is_empty():
		TranslationServer.set_locale(locale)
	if OS.get_environment("SCREENSHOT_SETTINGS_ONLY") == "1":
		await _settings_screens(start, out_dir)
		quit(0)
		return
	await create_timer(0.5).timeout
	root.get_texture().get_image().save_png(out_dir.path_join("start-courses.png"))
	# Import the route as a new course, as the Courses tab's import does.
	var courses: CoursesTab = start.find_children("*", "CoursesTab", true, false)[0]
	var prepared: Array[bool] = [false]
	orqwa.course_added.connect(func(_path: String) -> void: prepared[0] = true, CONNECT_ONE_SHOT)
	courses.call("_on_file_selected", route)
	await create_timer(0.5).timeout
	root.get_texture().get_image().save_png(out_dir.path_join("import-name.png"))
	# The course is named as suggested (#40); confirming closes the dialog.
	var name_dialog: ConfirmationDialog = courses.get("_name_dialog")
	name_dialog.hide()
	courses.call("_on_name_confirmed")
	await create_timer(1.0).timeout
	root.get_texture().get_image().save_png(out_dir.path_join("loading.png"))
	# The rider settings with the HUD editor.
	var dialog: ProfileDialog = start.find_children("*", "ProfileDialog", true, false)[0]
	dialog.edit(orqwa.profile(), orqwa.hud_layout())
	var dialog_tabs: TabContainer = dialog.find_children("*", "TabContainer", true, false)[0]
	dialog_tabs.current_tab = 1
	await create_timer(0.5).timeout
	root.get_texture().get_image().save_png(out_dir.path_join("rider-settings.png"))
	dialog.hide()
	var until: int = Time.get_ticks_msec() + int(TIMEOUT_S * 1000.0)
	while not prepared[0] and Time.get_ticks_msec() < until:
		await process_frame
	_check(prepared[0], "course prepared")
	# The prepared course opens on its detail page.
	await create_timer(1.0).timeout
	root.get_texture().get_image().save_png(out_dir.path_join("course-detail.png"))
	print("saved course detail")
	# The pencil beside the course's name: its name and video.
	var course_page: CourseDetail = start.find_children("*", "CourseDetail", true, false)[0]
	course_page.call("_open_editor")
	await create_timer(0.5).timeout
	root.get_texture().get_image().save_png(out_dir.path_join("course-edit.png"))
	var edit_dialog: Window = course_page.get("_edit_dialog")
	edit_dialog.hide()
	# Let the world stream its chunks in.
	while true:
		var next_chunk: int = world.get("_next_chunk")
		var chunk_count: int = world.get("_chunk_count")
		if next_chunk >= chunk_count:
			break
		await process_frame
	# A custom HUD, e.g. SCREENSHOT_HUD=power_3s,speed,normalized_power,upcoming_grade
	var hud: String = OS.get_environment("SCREENSHOT_HUD")
	if not hud.is_empty():
		orqwa.set_hud_layout(PackedStringArray(hud.split(",")))
	# A graphics preset, e.g. SCREENSHOT_QUALITY=ultra (R43).
	var quality: String = OS.get_environment("SCREENSHOT_QUALITY")
	if not quality.is_empty():
		_check(orqwa.set_graphics_quality(quality), "graphics quality %s" % quality)
	_software_render(world)
	_check(orqwa.connect_fake_trainer(250.0, 90.0), "fake trainer")
	# A pacer to race, e.g. SCREENSHOT_GHOST=300 (watts).
	var ghost: Dictionary = {"kind": "none"}
	var ghost_watts: String = OS.get_environment("SCREENSHOT_GHOST")
	if not ghost_watts.is_empty():
		ghost = {"kind": "power", "watts": ghost_watts.to_float()}
	_check(orqwa.start_ride(50.0, false, ghost), "ride started")
	start.ride_started.emit(start.ride_options())
	_software_render(world)
	var time: String = OS.get_environment("SCREENSHOT_TIME")
	var weather: String = OS.get_environment("SCREENSHOT_WEATHER")
	if not time.is_empty() or not weather.is_empty():
		world.apply_conditions(
			time if not time.is_empty() else "Midday",
			weather if not weather.is_empty() else "Clear"
		)
	await create_timer(maxf(ride_s, 4.0)).timeout
	if OS.get_environment("SCREENSHOT_RIDE_OPTIONS_ONLY") == "1":
		var screen: RideScreen = _main.get_node("RideScreen")
		screen.call("_open_settings")
		await create_timer(0.5).timeout
		root.get_texture().get_image().save_jpg(out_dir.path_join("ride-settings.jpg"), 0.9)
		print("saved ride settings")
		quit(0)
		return

	for mode: int in range(3):
		await create_timer(1.5).timeout
		var image: Image = root.get_texture().get_image()
		var path: String = out_dir.path_join("ride-%d.png" % mode)
		image.save_png(path)
		print("saved ", path)
		world.cycle_camera()

	# A moment of the ride (R124) on its card, mid-burst.
	var moment_screen: RideScreen = _main.get_node("RideScreen")
	moment_screen.call(
		"_on_moment", {"kind": "climb_record", "index": 1, "elapsed_s": 842.0, "gain_s": 12.0}
	)
	await create_timer(0.6).timeout
	root.get_texture().get_image().save_png(out_dir.path_join("ride-moment.png"))
	print("saved ride moment")
	var moment_card: MomentCard = moment_screen.get("_moment_card")
	moment_card.clear()

	# A close side view of the rider, for checking the avatar.
	var rider: Node3D = world.get_node("Rider")
	var side: Camera3D = Camera3D.new()
	world.add_child(side)
	var target: Vector3 = rider.global_position + Vector3.UP * 0.8
	var right: Vector3 = rider.global_transform.basis.x
	side.global_position = target + right * 3.0 + Vector3.UP * 0.3
	side.look_at(target, Vector3.UP)
	side.current = true
	for frame: int in range(3):
		await create_timer(0.25).timeout
		var image: Image = root.get_texture().get_image()
		image.save_png(out_dir.path_join("side-%d.png" % frame))
		print("saved side-%d" % frame)

	# The HUD editor.
	side.current = false
	var ride_screen: RideScreen = _main.get_node("RideScreen")
	for child: Node in ride_screen.get_children():
		if child is RideSettingsDialog:
			var settings_dialog: RideSettingsDialog = child
			settings_dialog.edit(start.ride_options(), orqwa.hud_layout(), false)
			await create_timer(0.5).timeout
			root.get_texture().get_image().save_png(out_dir.path_join("ride-settings.png"))
			print("saved ride settings")
			var tabs: TabContainer = (
				settings_dialog.find_children("*", "TabContainer", true, false)[0]
			)
			var hud_editor: Node = settings_dialog.find_children("*", "HudEditor", true, false)[0]
			tabs.current_tab = hud_editor.get_index()
			await process_frame
			# Hover a figure over the HUD's grid, so the drop indicator shows.
			var preview: HudPanel = settings_dialog.find_children("*", "HudPanel", true, false)[0]
			var grid: Node = preview.get_child(preview.get_child_count() - 1)
			grid.get_child(2).call(
				"_can_drop_data", Vector2(80, 10), {HudPanel.DRAG_KEY: "power_3s"}
			)
			await create_timer(0.5).timeout
			root.get_texture().get_image().save_png(out_dir.path_join("hud-settings_dialog.png"))
			print("saved hud settings_dialog")
			# The devices, to reconnect them (R69).
			var devices: Node = settings_dialog.find_children("*", "RideDevices", true, false)[0]
			tabs.current_tab = devices.get_index()
			await create_timer(0.5).timeout
			root.get_texture().get_image().save_png(out_dir.path_join("ride-devices.png"))
			print("saved ride devices")
			# The rider's profile, changed during the ride (R98).
			var profile: Node = settings_dialog.find_children("*", "RideProfile", true, false)[0]
			tabs.current_tab = profile.get_parent().get_index()
			await create_timer(0.5).timeout
			root.get_texture().get_image().save_png(out_dir.path_join("ride-profile.png"))
			print("saved ride profile")
			settings_dialog.hide()

	# The ride's summary (R42), then the history.
	await _quick_summary(orqwa, out_dir)
	var quick_summary: RideSummaryDialog = _main.get("_quick_summary")
	quick_summary.custom_action.emit(&"details")
	await create_timer(1.0).timeout
	root.get_texture().get_image().save_png(out_dir.path_join("summary.png"))
	print("saved summary")
	# Further down, the ride's power curve over the rider's best (R62).
	var summary: HistoryScreen = _main.get_node("HistoryScreen")
	var ride_detail: Control = summary.get("_detail")
	var detail_scroll: ScrollContainer = ride_detail.get_parent() as ScrollContainer
	var ride_curve: Control = summary.get("_power_curve")
	detail_scroll.ensure_control_visible(ride_curve)
	await create_timer(0.5).timeout
	root.get_texture().get_image().save_png(out_dir.path_join("summary-curve.png"))
	print("saved summary curve")
	detail_scroll.scroll_vertical = 0
	# The menu opens where its button is once the scroll is laid out again.
	await process_frame
	# Its upload menu (R29): the rider has no account yet, so it offers to connect first.
	var upload: MenuButton = summary.get("_upload_button")
	upload.show_popup()
	await create_timer(0.5).timeout
	root.get_texture().get_image().save_png(out_dir.path_join("upload-menu.png"))
	upload.get_popup().hide()
	(_main.get_node("HistoryScreen") as HistoryScreen).closed.emit()
	var start_tabs: TabContainer = start.get("_tabs")
	start_tabs.current_tab = StartPage.Tab.HISTORY
	await create_timer(1.0).timeout
	root.get_texture().get_image().save_png(out_dir.path_join("history.png"))
	start_tabs.current_tab = StartPage.Tab.COURSES
	await create_timer(0.5).timeout
	root.get_texture().get_image().save_png(out_dir.path_join("start-after-ride.png"))
	var detail: CourseDetail = start.find_children("*", "CourseDetail", true, false)[0]
	detail.back_requested.emit()
	await create_timer(0.5).timeout
	root.get_texture().get_image().save_png(out_dir.path_join("start-gallery.png"))
	# The remote's pad, or the keyboard, on the first course: its ring shows (#228).
	var first_card: CourseCard = courses.find_children("*", "CourseCard", true, false)[0]
	first_card.grab_focus()
	await create_timer(0.3).timeout
	root.get_texture().get_image().save_png(out_dir.path_join("start-gallery-focus.png"))
	first_card.release_focus()
	# The filters' panel open, a chip on (R85).
	var chips: GridContainer = courses.get("_chips")
	for chip: Node in chips.find_children("*", "Button", true, false):
		if (chip as Button).text == "Mountainous":
			(chip as Button).button_pressed = true
	var filters: Button = courses.get("_filters")
	filters.pressed.emit()
	await create_timer(0.5).timeout
	root.get_texture().get_image().save_png(out_dir.path_join("start-gallery-filters.png"))
	var filter_panel: PopupPanel = courses.get("_filter_panel")
	filter_panel.hide()
	courses.clear_filters()
	for tab: Array in [
		[StartPage.Tab.ANALYSIS, "start-analysis"],
		[StartPage.Tab.PROFILE, "start-profile"],
		[StartPage.Tab.DEVICES, "start-devices"],
	]:
		var index: int = tab[0]
		var file: String = tab[1]
		start_tabs.current_tab = index
		if index == StartPage.Tab.ANALYSIS:
			_compare_periods(start)
		if index == StartPage.Tab.PROFILE:
			_unfold_rider(start)
		await create_timer(0.5).timeout
		root.get_texture().get_image().save_png(out_dir.path_join(file + ".png"))
		if index == StartPage.Tab.ANALYSIS:
			for section: Array in [
				["_fitness", "fitness"], ["_weekly", "weekly"], ["_trend", "progress"]
			]:
				var chart: String = section[0]
				var shot: String = section[1]
				await _analysis_below(start, chart)
				var path: String = out_dir.path_join("start-analysis-%s.png" % shot)
				root.get_texture().get_image().save_png(path)
		if index == StartPage.Tab.PROFILE:
			# Connecting an account (R29), from the card's Accounts: intervals.icu, Strava.
			for platform: Array in [[0, "account-dialog"], [1, "account-dialog-strava"]]:
				var which: int = platform[0]
				var shot: String = platform[1]
				var dialog_shown: AccountDialog = _connect_account(start, which)
				await create_timer(0.5).timeout
				root.get_texture().get_image().save_png(out_dir.path_join(shot + ".png"))
				if dialog_shown != null:
					dialog_shown.hide()
		if index == StartPage.Tab.DEVICES:
			# The phone remote's code to scan (#228); off again afterwards, as it was.
			var devices_tab: DevicesTab = start.find_children("*", "DevicesTab", true, false)[0]
			var remote: CheckButton = devices_tab.get("_remote")
			remote.button_pressed = true
			await create_timer(0.5).timeout
			root.get_texture().get_image().save_png(out_dir.path_join("start-devices-remote.png"))
			var remote_dialog: RemoteDialog = devices_tab.get("_remote_dialog")
			remote_dialog.hide()
			# What the phone's buttons do, behind the pen beside its row.
			var buttons_dialog: ButtonDialog = devices_tab.get("_button_dialog")
			buttons_dialog.open(ButtonDialog.PHONE_REMOTE, OrqwaApp.phone_remote_name())
			await create_timer(0.5).timeout
			var buttons_shot: String = out_dir.path_join("start-devices-remote-buttons.png")
			root.get_texture().get_image().save_png(buttons_shot)
			buttons_dialog.hide()
			remote.button_pressed = false
			await _found_devices(devices_tab, out_dir)
	await _settings_screens(start, out_dir)
	print("saved history")
	await _workout_screens(orqwa, start)
	quit(0)


func _settings_screens(start: StartPage, out_dir: String) -> void:
	var orqwa: OrqwaApp = _main.get_node("Orqwa")
	var kept: bool = orqwa.keeps_worlds()
	await _keep_a_world(orqwa)
	var settings: SettingsDialog = start.get("_settings")
	var settings_button: Button = start.get("_settings_button")
	settings_button.pressed.emit()
	var settings_tabs: TabContainer = settings.get("_tabs")
	# By page, not position: a new tab (Sound) once moved About from where the index pointed.
	for page: Array in [
		[&"_graphics", "settings-graphics"],
		[&"_courses", "settings-courses"],
		[&"_about", "settings-about"],
	]:
		var property: StringName = page[0]
		var tab: Node = settings.get(property)
		while tab.get_parent() != settings_tabs:
			tab = tab.get_parent()
		var file: String = page[1]
		settings_tabs.current_tab = tab.get_index()
		await create_timer(0.5).timeout
		root.get_texture().get_image().save_png(out_dir.path_join(file + ".png"))
		print("saved ", file)
	settings.hide()
	# As this computer had it: turned off, the worlds kept for the picture go.
	orqwa.set_keeps_worlds(kept)


## A course's world kept, for the Courses tab to list (R136): the loaded course's, or else
## that of the library's first, the sample course on a new library, built for it.
func _keep_a_world(orqwa: OrqwaApp) -> void:
	orqwa.set_keeps_worlds(true)
	if orqwa.loaded_course().is_empty():
		var library: Array = orqwa.courses()
		if library.is_empty():
			return
		var first: Dictionary = library[0]
		var path: String = first["path"]
		orqwa.route_loaded.connect(
			func(_info: Variant = null) -> void: orqwa.call_deferred("build_world"),
			CONNECT_ONE_SHOT
		)
		orqwa.open_course(path)
	# The world is kept in the background once built; a debug build takes minutes for it.
	var until: int = Time.get_ticks_msec() + 600000
	while orqwa.kept_worlds().is_empty() and Time.get_ticks_msec() < until:
		await create_timer(0.5).timeout
	_check(not orqwa.kept_worlds().is_empty(), "a world kept")


func _software_render(world: RideWorld) -> void:
	if OS.get_environment("ORQWA_RENDER_SOFTWARE") != "1":
		return
	root.msaa_3d = Viewport.MSAA_DISABLED
	root.screen_space_aa = Viewport.SCREEN_SPACE_AA_FXAA
	var environment: Environment = world.get("_environment")
	environment.glow_enabled = false


## The last six weeks against the six before, read off at 5 minutes, as wiki/Analysis.md shows.
## the power curve (R62).
func _compare_periods(start: StartPage) -> void:
	var analysis: AnalysisTab = start.find_children("*", "AnalysisTab", true, false)[0]
	var compare: OptionButton = analysis.get("_compare")
	compare.select(1)
	analysis.refresh()
	var chart: PowerCurveChart = analysis.get("_chart")
	chart.set("_hover_s", 300.0)
	chart.queue_redraw()


## Further down the Analysis tab, scrolled to the card of `chart`, as wiki/Analysis.md shows:
## fitness, fatigue and form read off at today, and the weeks' time in power zones (R64).
func _analysis_below(start: StartPage, chart: String) -> void:
	var analysis: AnalysisTab = start.find_children("*", "AnalysisTab", true, false)[0]
	var metric: OptionButton = analysis.get("_weekly_metric")
	metric.select(5)
	analysis.call("_fill_weekly")
	var fitness: FitnessChart = analysis.get("_fitness")
	var days: PackedInt64Array = fitness.get("_days")
	fitness.set("_hover", days.size() - 1)
	fitness.queue_redraw()
	var shown: Control = analysis.get(chart)
	var card: Control = shown.get_parent().get_parent()
	var sections: Control = analysis.get("_content")
	var scroll: ScrollContainer = analysis.get("_scroll")
	scroll.scroll_vertical = int(sections.position.y + card.position.y)
	await create_timer(0.5).timeout


## Unfolds the rider's card to their whole setup and zones (#194), as wiki/Riders.md shows.
func _unfold_rider(start: StartPage) -> void:
	for found: Node in start.find_children("*", "Button", true, false):
		var button: Button = found
		if button.is_visible_in_tree() and button.tooltip_text == tr("All settings"):
			button.pressed.emit()
			return


## What a scan found, behind its result (R107), as wiki/Devices-and-riding.md tells: made-up
## devices, listed without connecting any, then the tab as it was.
func _found_devices(devices_tab: DevicesTab, out_dir: String) -> void:
	var found: Array = [
		_scanned("KICKR CORE 2", "trainer", -52),
		_scanned("Zwift Click", "controller", -55),
		_scanned("Polar H9 A1B2C3D4", "heart_rate", -61),
		_scanned("ASSIOMA34567L", "power_meter", -64),
		_scanned("Venu 3", "heart_rate", -73),
	]
	devices_tab._show_scan(tr("Found %d device(s)") % found.size(), found)
	# Laid out first, as before a click: the list opens under the result where it is drawn.
	await process_frame
	devices_tab._open_found()
	await create_timer(0.5).timeout
	root.get_texture().get_image().save_png(out_dir.path_join("start-devices-found.png"))
	devices_tab._show_scan(tr("Wake the devices first."))


static func _scanned(device_name: String, kind: String, rssi: int) -> Dictionary:
	return {"name": device_name, "kind": kind, "rssi": rssi}


## Presses Connect… on the unfolded rider's account number `which`; the dialog it opens, if
## any.
func _connect_account(start: StartPage, which: int) -> AccountDialog:
	var seen: int = 0
	for found: Node in start.find_children("*", "Button", true, false):
		var button: Button = found
		if not button.is_visible_in_tree() or button.text != tr("Connect…"):
			continue
		seen += 1
		if seen == which + 1:
			button.pressed.emit()
			# The History tab has one too.
			for dialog: Node in start.find_children("*", "AccountDialog", true, false):
				if (dialog as AccountDialog).visible:
					return dialog
	return null


## The Workouts tab (R58) and a heart-rate workout on its own (R56), with its settings.
func _workout_screens(orqwa: OrqwaApp, start: StartPage) -> void:
	var out_dir: String = OS.get_environment("SCREENSHOT_DIR")
	var start_tabs: TabContainer = start.get("_tabs")
	start_tabs.current_tab = StartPage.Tab.WORKOUTS
	var tab: WorkoutsTab = start.find_children("*", "WorkoutsTab", true, false)[0]
	var options: WorkoutOptions = tab.find_children("*", "WorkoutOptions", true, false)[0]
	options.set_workout({"kind": "zone", "zone": 3})
	await create_timer(0.5).timeout
	root.get_texture().get_image().save_png(out_dir.path_join("start-workouts.png"))
	var workout: Dictionary = tab.workout()
	_check(orqwa.start_workout(workout, false, false), "workout started")
	start.ride_started.emit({"workout": workout, "on_course": false, "difficulty": 50.0})
	await create_timer(maxf(OS.get_environment("SCREENSHOT_WORKOUT_S").to_float(), 20.0)).timeout
	root.get_texture().get_image().save_png(out_dir.path_join("workout.png"))
	var ride_screen: RideScreen = _main.get_node("RideScreen")
	ride_screen.call("_open_settings")
	await create_timer(0.5).timeout
	root.get_texture().get_image().save_png(out_dir.path_join("workout-settings.png"))
	print("saved workout")
	for dialog: Node in ride_screen.find_children("*", "RideSettingsDialog", true, false):
		(dialog as Window).hide()
	# The overlay (R55): the window shrinks to the HUD, on top and see-through.
	ride_screen.overlay_requested.emit(true)
	await create_timer(1.5).timeout
	var window: Window = root
	print(
		(
			"overlay window: %s, borderless %s, on top %s, transparent %s"
			% [window.size, window.borderless, window.always_on_top, window.transparent]
		)
	)
	root.get_texture().get_image().save_png(out_dir.path_join("overlay.png"))
	ride_screen.overlay_requested.emit(false)
	await create_timer(0.5).timeout
	orqwa.finish_ride()

	# A structured workout (R21): the plan on the tab, then ridden on its own.
	await process_frame
	_main.call("_back_home")
	start_tabs.current_tab = StartPage.Tab.WORKOUTS
	options.set_workout({"kind": "plan", "id": "builtin:threshold-2x15"})
	options.changed.emit()
	await create_timer(0.5).timeout
	root.get_texture().get_image().save_png(out_dir.path_join("start-structured.png"))
	var structured: Dictionary = tab.workout()
	_check(orqwa.start_workout(structured, false, false), "structured workout started")
	start.ride_started.emit({"workout": structured, "on_course": false, "difficulty": 50.0})
	await create_timer(25.0).timeout
	root.get_texture().get_image().save_png(out_dir.path_join("structured.png"))
	print("saved structured workout")
	orqwa.finish_ride()
	# The FTP test (R22) on the tab.
	await process_frame
	_main.call("_back_home")
	options.set_workout({"kind": "ftp_test"})
	options.changed.emit()
	await create_timer(0.5).timeout
	root.get_texture().get_image().save_png(out_dir.path_join("start-ftp-test.png"))
	# The 20-minute test (#125): its all-out part is a free step.
	options.set_workout({"kind": "ftp_test", "test": "twenty_minutes"})
	options.changed.emit()
	await create_timer(0.5).timeout
	root.get_texture().get_image().save_png(out_dir.path_join("start-ftp-test-20.png"))
	# The workout editor (R21) on a copy of a built-in workout.
	options.set_workout({"kind": "plan", "id": "builtin:threshold-2x15"})
	options.changed.emit()
	var editor: WorkoutEditor = tab.find_children("*", "WorkoutEditor", true, false)[0]
	editor.edit(options.plan(), 250.0)
	await create_timer(0.5).timeout
	root.get_texture().get_image().save_png(out_dir.path_join("workout-editor.png"))
	editor.hide()


func _summary_only(out_dir: String) -> void:
	root.disable_3d = true
	var locale: String = OS.get_environment("SCREENSHOT_LOCALE")
	if not locale.is_empty():
		TranslationServer.set_locale(locale)
	var orqwa: OrqwaApp = _main.get_node("Orqwa")
	var start: StartPage = _main.get_node("StartPage")
	_check(orqwa.connect_fake_trainer(180.0, 90.0), "fake trainer")
	var workout: Dictionary = {
		"kind": "power", "power_w": 180.0, "name": tr("Constant power %d W") % 180
	}
	_check(orqwa.start_workout(workout, false, false), "summary workout")
	start.ride_started.emit({"workout": workout, "on_course": false})
	await create_timer(12.0).timeout
	await _quick_summary(orqwa, out_dir)


func _quick_summary(orqwa: OrqwaApp, out_dir: String) -> void:
	orqwa.finish_ride()
	await create_timer(1.0).timeout
	root.get_texture().get_image().save_png(out_dir.path_join("summary-quick.png"))
	print("saved quick summary")


func _error_details(out_dir: String) -> void:
	for locale: String in ["en", "de"]:
		_main.free()
		TranslationServer.set_locale(locale)
		_create_main()
		await process_frame
		# The rider's profile selects its language at bind time; this fixture chooses one.
		TranslationServer.set_locale(locale)
		var orqwa: OrqwaApp = _main.get_node("Orqwa")
		orqwa.ride_detail("/missing-261.fit", 10)
		var details: FailureDetails = _main.get("_failures")
		var toggle: Button = details.get("_toggle")
		toggle.button_pressed = true
		await create_timer(0.5).timeout
		var file: String = "error-details-de.png" if locale == "de" else "error-details.png"
		root.get_texture().get_image().save_png(out_dir.path_join(file))
		print("saved " + file)


func _check(condition: bool, what: String) -> void:
	if not condition:
		push_error("SCREENSHOTS FAILED: " + what)
		quit(1)
