// =============================================================
//  /System/ui/desktop.vsh
//
//  Default VEILOS desktop shell.
//
//  This file is BOTH the default configuration AND the
//  reference implementation for the VSH UI toolkit. It is
//  written to be read top to bottom as documentation.
//
//  If you are:
//    - A USER customising your desktop   -> read Sections 3-6
//    - IMPLEMENTING the toolkit in Rust  -> read all sections
//    - DEBUGGING shell behaviour         -> read Section 7
//
//  ---------------------------------------------------------
//  THE MODEL IN ONE PARAGRAPH
//  ---------------------------------------------------------
//  A UI element is declared as a typed variable (a
//  "definition"). Nothing renders until you `add` it, at which
//  point the runtime takes a snapshot and creates an on-screen
//  instance. You cannot mutate a live instance from code.
//
//  Three things can change on screen, and only three:
//
//    1. LIVE PROPERTIES re-evaluate when their inputs change.
//       text:       "{time()}"       updates every frame
//       background: THEME[accent]    updates when THEME changes
//       visible:    MENU_OPEN        updates when MENU_OPEN is
//                                    reassigned
//
//    2. SNAPSHOT PROPERTIES are baked at publish, never change.
//       border_radius: 4
//       padding: (4, 8)
//       on_click: handle_start
//
//    3. STRUCTURAL CHANGES (adding/removing elements) require
//       destroy + add. Never mutate in place.
//
//  This is what "deterministic" means here: every screen change
//  comes from either a variable write or a live expression
//  re-evaluation, nothing else. No hidden mutation, no order of
//  operations surprises.
//
//  ---------------------------------------------------------
//  AUDITABILITY RULE
//  ---------------------------------------------------------
//  All state writes live in Section 7. Element scripts never
//  write variables directly; they call a top-level handler.
//  This means the entire search space for shell misbehaviour is
//  a handful of named functions. Keep it that way.
//
//  ---------------------------------------------------------
//  Z-ORDER RULE
//  ---------------------------------------------------------
//  z = 0 is the element's own level (default).
//  z = -1 sits at the parent's level.
//  z = -2 sits behind the parent.
//  z =  1 sits at the child's level.
//  z =  2 sits above the children.
//
//  Within the same z level, elements are drawn in insertion
//  order: first added is behind, last added is on top.
// =============================================================


// CHANGED (shell runtime): DARK is defined in the theme file, so it is
// imported here. Without this, `set THEME = DARK;` in `start` fails.
import "/THEMES/DARK.VSH";

// =============================================================
//  SECTION 1 - System variables
//
//  These are the OS-level settings the shell reads and writes.
//  They are defined here so the file is self-contained; a user
//  theme file may override them via `set` at boot.
// =============================================================

// --- Wallpaper -------------------------------------------------
// If WALLPAPER_PATH is unset, the desktop falls back to a flat
// background colour taken from THEME[background].
// Paths are UNIX-style, relative to filesystem root.
// CHANGED (shell runtime): the wallpaper is set in /config.vsh
// (WALLPAPER_PATH, WALLPAPER_POSITION). Setting it here as well
// replaced config's choice, and the old path
// "/System/wallpaper/default.jpg" does not exist (and is not an
// 8.3 name), so the desktop ended up with no wallpaper at all.
// Valid positions: STRETCH (default), CENTER, TILE, CROP,
//                  BASIC, WINDOW_FIT, WINDOW_CENTER

// --- Window tiling --------------------------------------------
// These drive the window manager's snap grid. The shell does
// not need to know about them; they are read by the WM.
set WINDOW_TILES           = (2, 2);      // cols, rows
set WINDOW_TILE_SIZE_RATIO = ([1,1], [1,1]);
set WINDOW_SNAP            = "MAGNETIC";  // OFF | SLIPPERY | STICKY | MAGNETIC
set WINDOW_STACK           = false;       // stack on top vs push to next tile

// --- Clipboard ------------------------------------------------
set CLIPBOARD_CHANNELS_DEFAULT = "PRIVATE";
set CLIPBOARD_LOG_SIZE         = 50;
set CLIPBOARD_LOCATION         = "/System/clipboard.dat";
set CLIPBOARD_FILE_SIZE_LIMIT  = 2048;
set CLIPBOARD_MAX_SIZE         = 2048;
set CLIPBOARD_STORAGE_DEFAULT  = "PERMINANT";

// --- Mesh offload ---------------------------------------------
// Controls whether worker threads may be offloaded to remote
// mesh nodes. Read live by the OffloadBroker on every submit.
// "disabled" | "ask" (default) | "always"
var MESH_OFFLOAD_POLICY: string = "ask";

// --- Shell state ----------------------------------------------
// These are read by element definitions as live expressions.
// The kernel updates CPU_USAGE and MEM_USAGE; the shell only
// reads them.
var MENU_OPEN:      bool  = false;   // start menu visibility
var NOTIFY_COUNT:   int   = 0;       // unread notifications
var CPU_USAGE:      float = 0.0;     // 0.0 - 100.0
var MEM_USAGE:      float = 0.0;     // 0.0 - 100.0
var VOLUME_LEVEL:   int   = 65;      // 0 - 100
var NETWORK_UP:     bool  = true;

// Transient: holds references to open-window taskbar buttons.
// Populated and drained by handlers in Section 7.
var window_refs: list = [];


// =============================================================
//  SECTION 2 - Theme contract
//
//  A theme is any object exposing the keys below. The default
//  theme file at /System/ui/themes/dark.vsh defines them; drop
//  in your own and `set THEME = YOURS;` to switch.
//
//  The toolkit does NOT require any of these keys. An element
//  that references a missing key evaluates to a visible error
//  colour rather than crashing, so themes can be built
//  incrementally.
//
//  Key               Purpose
//  ----------------  ----------------------------------------
//  background        Base window and panel surface
//  foreground        Primary text
//  foreground_dim    Secondary text, tray indicators
//  accent            Primary action / selection colour
//  accent_hover      Accent on hover
//  danger            Destructive actions (shutdown, delete)
//  border            Panel borders, dividers, separators
//  surface_raised    Menus, dropdowns (one shade up)
//  surface_sunken    Inputs, wells (one shade down)
//  border_radius     Default corner radius in pixels
//  font_family       Default UI font name
//  font_size         Default body size in pixels
// =============================================================


// =============================================================
//  SECTION 3 - Reusable bases
//
//  Bases are element definitions that never get added directly.
//  They exist to be spread (`...Base`) into concrete elements.
//  Spread order matters: later keys override earlier ones, so
//  the pattern is always `...Parent, ...overrides`.
//
//  A base can itself spread another base; inheritance chains
//  arbitrarily deep. In this file:
//      Surface  ->  Button  ->  BarButton  ->  concrete buttons
// =============================================================

// -- Surface: the visual foundation for anything with a body ---
// Every themed element starts here. Concrete elements override
// the pieces they need; everything else cascades.
var Surface: element = {
    background:       THEME[background],
    border:           THEME[border],
    border_thickness: 0,
    border_radius:    THEME[border_radius],
    color:            THEME[foreground],
    font_family:      THEME[font_family],
    font_size:        THEME[font_size],
    padding:          0,
};

// -- Button: base for anything clickable ----------------------
// Adds cursor feedback and default padding. Concrete buttons
// extend this to set their own hover/pressed visuals.
var Button: element.button = {
    // CHANGED (shell runtime): removed `...Button,` - a definition cannot
    // spread itself (it does not exist yet), which made every button fail.
    ...Surface,
    padding: (4, 10),
    cursor:  "pointer",
    // text_align defaults to "center" for buttons.
    // Every button inherits the same interaction behaviour:
    //   on_hover_enter raises the element by 1 visual level
    //   on_press        lowers it
    // Implemented by the runtime, not by scripts.
};

// -- BarButton: compact button for the taskbar -----------------
// Narrower padding, no border, tighter radius. Used everywhere
// on the bar so the whole row feels coherent.
var BarButton: element.button = {
    ...Button,
    border_thickness: 0,
    border_radius:    4,
    padding:          (4, 8),
    background:       TRANSPARENT,
};

// -- TrayLabel: small readout text for the tray region ---------
// Tabular numerals so digits do not jitter as they tick.
var TrayLabel: element.label = {
    ...Surface,
    padding:       (4, 6),
    align:         "center",
    font_size:     THEME[font_size] - 1,
    color:         THEME[foreground_dim],
    font_features: "tnum",
};

// -- MenuItem: base for rows inside the start menu -------------
// Full width, left-aligned, generous padding.
var MenuItem: element.button = {
    ...Surface,
    size:         ("100%", 30),
    padding:      (0, 10),
    border_radius: 4,
    text_align:   "left",
    background:   TRANSPARENT,
};

// -- TrayDivider: a 1px vertical separator ---------------------
var TrayDivider: element.divider = {
    size:       (1, 16),
    background: THEME[border],
    // Divider has no other properties. It is a straight line.
};


// =============================================================
//  SECTION 4 - Taskbar
//
//  The taskbar is a horizontal row pinned to the bottom of the
//  screen. It contains two flow groups:
//
//      [ left group ] ............ [ right group ]
//
//  The two groups are pushed to opposite ends by the parent's
//  `justify: "space_between"`. Neither group needs coordinates;
//  the flow engine places them.
//
//  Children added to a flow container WITHOUT a `position:`
//  argument are auto-placed in insertion order. Children added
//  WITH a `position:` argument are absolutely positioned inside
//  the container, ignoring flow. Mixing the two is allowed.
// =============================================================

// -- Desktop root ----------------------------------------------
// Full-screen container. Everything else hangs off this.
var DesktopRoot: element.panel = {
    size:       ("100%", "100%"),
    background: THEME[background],
    // Background image is composited by the runtime from
    // WALLPAPER_PATH / WALLPAPER_POSITION, layered under
    // everything else in this panel. It is not an element.
};

// -- Taskbar ---------------------------------------------------
var Taskbar: element.row = {
    ...Surface,
    size:             ("100%", 32),
    border_thickness: (0, 0, 1, 0),   // top edge only
    flow: {
        axis:    "horizontal",
        justify: "space_between",
        align:   "center",
        padding: (0, 4),
    },
};

// -- Taskbar left group ----------------------------------------
// Start, Search, and the dynamic window list.
var TaskbarLeft: element.row = {
    flow: {
        axis:    "horizontal",
        justify: "start",
        align:   "center",
        gap:     4,
        padding: (0, 6),
    },
};

// -- Taskbar right group ---------------------------------------
// Tray indicators and the clock, packed to the right edge.
var TaskbarRight: element.row = {
    flow: {
        axis:    "horizontal",
        justify: "end",
        align:   "center",
        gap:     4,
        padding: (0, 6),
    },
};

// -- Window list container -------------------------------------
// Empty row that handlers in Section 7 append window buttons to.
// Making it a dedicated container keeps window buttons visually
// separated from the Start/Search buttons on the left.
var WindowList: element.row = {
    flow: {
        axis:    "horizontal",
        justify: "start",
        align:   "center",
        gap:     2,
        padding: (0, 4),
    },
};

// -- Start button ----------------------------------------------
// Live expression on the label means the glyph flips when
// MENU_OPEN changes without republishing the button.
var StartButton: element.button = {
    ...BarButton,
    size:  (60, 24),
    label: "{if(MENU_OPEN, \"MENU *\", \"MENU\")}",
    on_click: handle_start_click,
};

// -- Search button ---------------------------------------------
// Opens the search app. Search app itself is not part of the
// shell; the shell just launches it.
var SearchButton: element.button = {
    ...BarButton,
    size:  (72, 24),
    label: "Search",
    on_click: handle_search_click,
};

// -- Per-window taskbar button ---------------------------------
// One instance per open window. `label` is a live expression
// reading `window_title` from the current window record, so the
// taskbar entry tracks its window's title without republishing.
//
// `window_id` and `window_title` are provided by the runtime
// when a WindowButton instance is published - see Section 6.
var WindowButton: element.button = {
    ...BarButton,
    size:      ("auto", 24),
    min_width: 80,
    max_width: 200,
    label:     "{window_title}",
    on_click:  handle_window_click,
};

// -- Tray: volume indicator ------------------------------------
// Text-only for now. A real shell would use element.icon here
// and swap the icon on VOLUME_LEVEL changes via a live
// expression.
var VolumeLabel: element.label = {
    ...TrayLabel,
    text: "VOL {format(int, VOLUME_LEVEL)}",
    on_click: handle_volume_click,
};

// -- Tray: network indicator -----------------------------------
var NetworkLabel: element.label = {
    ...TrayLabel,
    text: "{if(NETWORK_UP, \"NET\", \"NO NET\")}",
    on_click: handle_network_click,
};

// -- Tray: CPU readout -----------------------------------------
// Reading CPU_USAGE, which the kernel updates. One decimal
// place keeps the label width stable.
var CpuLabel: element.label = {
    ...TrayLabel,
    text: "CPU {format(float, CPU_USAGE, 1)}%",
};

// -- Tray: clock -----------------------------------------------
// %H:%M - 24-hour time without seconds. Add :%S if you want
// ticking seconds; it forces a re-render every second.
var ClockLabel: element.label = {
    ...TrayLabel,
    text:        "{format(string, time(\"%H:%M\"))}",
    font_weight: 600,
    color:       THEME[foreground],
    on_click:    handle_clock_click,
};


// =============================================================
//  SECTION 5 - Start menu
//
//  A floating menu anchored to the bottom-left corner, above
//  the taskbar. Its visibility is bound to MENU_OPEN via a live
//  expression, so flipping the variable shows and hides it
//  without republishing - the structure stays put.
//
//  The menu body uses a `flow` column with `align: "stretch"`,
//  which makes every child fill the menu's width. Menu items
//  therefore do not need their own width set.
// =============================================================

var StartMenu: element.menu = {
    ...Surface,
    visible:          MENU_OPEN,        // live expression
    position:         (8, 40, 0),       // 8 from left, 40 from bottom
    anchor:           ("left", "bottom"),
    size:             (240, 340),
    background:       THEME[surface_raised],
    border_thickness: 1,
    border_radius:    8,
    shadow:           (0, 8, 24, #00000088),
    flow: {
        axis:    "vertical",
        justify: "start",
        align:   "stretch",
        gap:     2,
        padding: 6,
    },
};

// -- Menu: quick-launch items ----------------------------------
// Declared as standalone elements so handlers can reuse them.
// They get added to StartMenu in the boot sequence.
var MenuFiles: element.button = {
    ...MenuItem,
    label:    "Files",
    on_click: { handle_launch("files"); },
};

var MenuTerminal: element.button = {
    ...MenuItem,
    label:    "Terminal",
    on_click: { handle_launch("terminal"); },
};

var MenuSettings: element.button = {
    ...MenuItem,
    label:    "Settings",
    on_click: { handle_launch("settings"); },
};

var MenuDivider: element.divider = {
    size:       ("100%", 1),
    background: THEME[border],
    // Margin is deliberately zero; the menu's own flow gap
    // provides the vertical rhythm.
};

// -- Menu: power actions ---------------------------------------
// Danger colour for anything that ends the session.
var MenuLock: element.button = {
    ...MenuItem,
    label:    "Lock",
    on_click: { lock_session(); },
};

var MenuShutdown: element.button = {
    ...MenuItem,
    label:    "Shut down",
    color:    THEME[danger],
    on_click: { shutdown(); },
};


// =============================================================
//  SECTION 6 - Dynamic elements
//
//  Window taskbar buttons are the shell's one dynamic element
//  list. They are the canonical example of the "structural
//  change" pattern: destroy + add, never mutate.
//
//  The runtime exposes two system events for this:
//      window_opened(id, title)
//      window_closed(id)
//
//  Handlers for these events live in Section 7. Every button
//  created here is tracked in `window_refs` so the close handler
//  can find it by id later.
// =============================================================

// The definition itself lives in Section 4 (WindowButton). It
// reads `window_id` and `window_title`, which the runtime
// injects into each instance at publish time.


// =============================================================
//  SECTION 7 - Top-level handlers
//
//  Every state write in the shell goes through a function in
//  this section. Element scripts call these by name; they never
//  write a variable directly.
//
//  Why: when the shell misbehaves, the entire search space is
//  this section. That is the whole point.
//
//  Naming convention: handle_* for UI events, on_event blocks
//  for system events (start, shutdown, etc).
// =============================================================

// -- Start menu ------------------------------------------------

function handle_start_click() {
    // MENU_OPEN is read by the StartButton label (live) and by
    // the StartMenu visible property (live). Flipping it is the
    // only action needed; the rest of the shell reacts.
    MENU_OPEN = not MENU_OPEN;
}

// -- Search ----------------------------------------------------

function handle_search_click() {
    // Search app owns its own window. The shell just launches it
    // and closes the menu.
    MENU_OPEN = false;
    launch("search");
}

// -- Window list -----------------------------------------------

function handle_window_click(self) {
    // `self` is a reference to the button instance that fired.
    // Every window button carries its window's id as a
    // published property, so we can look it up directly.
    focus_window(self[window_id]);
}

on_event window_opened(id, title) {
    // Publish a new button into the window list. The runtime
    // provides `window_id` and `window_title` as injectable
    // values for this specific event; the WindowButton
    // definition reads them by name.
    var btn: element_ref = add element.button(
        WindowButton,
        parent:      window_list_ref,
        id:          "win_" + id,
        window_id:    id,
        window_title: title,
    );
    window_refs.append(btn);
}

on_event window_closed(id) {
    // Find the button whose window_id matches, destroy it, and
    // remove it from the tracking list. This is the ONLY way to
    // remove a window button - never mutate in place.
    for ref in window_refs {
        if (ref[window_id] == id) {
            destroy(ref);
            window_refs.remove(ref);
            break;
        }
    }
}

// -- Tray ------------------------------------------------------

function handle_volume_click() {
    // Placeholder: cycle volume in 25% steps for demonstration.
    // A real shell would open a mixer popup here.
    VOLUME_LEVEL = (VOLUME_LEVEL + 25) % 125;
}

function handle_network_click() {
    launch("network_settings");
}

function handle_clock_click() {
    launch("calendar");
}

// -- App launching ---------------------------------------------

function handle_launch(app_name) {
    MENU_OPEN = false;
    launch(app_name);
}

// -- Notifications ---------------------------------------------
// Placeholder: real notifications are a dynamic element list
// living in the top-right corner. Left as an exercise - the
// pattern is identical to the window list above.


// =============================================================
//  SECTION 8 - Boot sequence
//
//  Runs once when the OS launches. Sets the theme, builds the
//  desktop tree, and publishes every element. Order matters:
//  THEME must be assigned before any element that references
//  it is published, or the reference evaluates to an error.
// =============================================================

// Reference holders. These are module-level because handlers in
// Section 7 need them (specifically: the window list).
var desktop_ref:     element_ref;
var taskbar_ref:     element_ref;
var taskbar_left_ref: element_ref;
var taskbar_right_ref: element_ref;
var window_list_ref: element_ref;

on_event start {
    // 1. Theme first. Every element below references THEME.
    set THEME = DARK;

    // 2. Desktop root. Everything hangs off this.
    desktop_ref = add element.panel(
        DesktopRoot,
        position: (0, 0, 0),
    );

    // 3. Taskbar and its two flow groups.
    taskbar_ref = add element.row(
        Taskbar,
        parent:   desktop_ref,
        position: (0, "100% - 32", 0),
        anchor:   ("left", "top"),
    );

    taskbar_left_ref = add element.row(
        TaskbarLeft,
        parent: taskbar_ref,
    );

    taskbar_right_ref = add element.row(
        TaskbarRight,
        parent: taskbar_ref,
    );

    // 4. Left group: Start, Search, WindowList.
    add element.button(StartButton,  parent: taskbar_left_ref, id: "start");
    add element.button(SearchButton, parent: taskbar_left_ref, id: "search");
    window_list_ref = add element.row(WindowList, parent: taskbar_left_ref);

    // 5. Right group: tray indicators, divider, clock.
    //    Order within the group matters - flow places them left
    //    to right in the order added.
    add element.label(VolumeLabel,    parent: taskbar_right_ref);
    add element.label(NetworkLabel,   parent: taskbar_right_ref);
    add element.label(CpuLabel,       parent: taskbar_right_ref);
    add element.divider(TrayDivider,  parent: taskbar_right_ref);
    add element.label(ClockLabel,     parent: taskbar_right_ref);

    // 6. Start menu, published last so it sits above the
    //    taskbar in the same z-level (see Z-ORDER RULE header).
    //    Its children are added with `parent:` and no position,
    //    so the menu's flow engine stacks them vertically.
    var menu_ref: element_ref = add element.menu(StartMenu, parent: desktop_ref);

    add element.button(MenuFiles,      parent: menu_ref);
    add element.button(MenuTerminal,   parent: menu_ref);
    add element.button(MenuSettings,   parent: menu_ref);
    add element.divider(MenuDivider,   parent: menu_ref);
    add element.button(MenuLock,       parent: menu_ref);
    add element.button(MenuShutdown,   parent: menu_ref);

    // 7. Optionally open one window on first boot so the user
    //    lands somewhere useful. Comment out to start with an
    //    empty desktop.
    launch("files");
}