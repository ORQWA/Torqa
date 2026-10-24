class_name SettingsDialog
extends AcceptDialog
## The settings, opened from the cog at the top right of the start page (R83): this
## computer's graphics and full screen (R43, R78, #218), applied as they change, its courses'
## kept worlds (R101), each to delete (R136), the app's own sounds (R125), and About: the
## version, the commit, looking for updates (R99) and the licences.

var _tabs: TabContainer = TabContainer.new()
var _graphics: GraphicsSettings = GraphicsSettings.new()
var _courses: CourseSettings = CourseSettings.new()
var _sound: SoundSettings = SoundSettings.new()
var _about: AboutPage = AboutPage.new()


func _ready() -> void:
	theme = UiTheme.build()
	title = tr("Settings")
	ok_button_text = tr("Done")
	min_size = Vector2i(760, 480)
	# The details (R90) make the page taller than a small window (R94).
	var graphics_page: ScrollContainer = UiTheme.scrolling(_graphics)
	graphics_page.name = tr("Graphics")
	_tabs.add_child(graphics_page)
	# Its kept worlds (R136) may be more than a window shows.
	var courses_page: ScrollContainer = UiTheme.scrolling(_courses)
	courses_page.name = tr("Courses")
	_tabs.add_child(courses_page)
	_sound.name = tr("Sound")
	_tabs.add_child(_sound)
	_about.name = tr("About")
	_tabs.add_child(_about)
	add_child(_tabs)


func bind(orqwa: OrqwaApp) -> void:
	_graphics.bind(orqwa)
	_courses.bind(orqwa)
	_sound.bind(orqwa)
	_about.bind(orqwa)


## Opens the settings on their first tab.
func open() -> void:
	_tabs.current_tab = 0
	_courses.refresh()
	_sound.refresh()
	UiScale.popup(self, Vector2i(960, 700))
