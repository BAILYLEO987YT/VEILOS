// =============================================================
//  /SETTINGS.VSH - the ready-made settings panel
//
//  Imported by /SHELL.VSH and opened from its Settings menu item.
//  It is an ordinary VSH script: copy it, change it, or write your
//  own. It uses only the public functions (ACCOUNTS_DESIGN.md,
//  section 9), so it can do exactly what the signed-in user could
//  do in the terminal - no more:
//
//    read    session_user() session_role() session_is_admin()
//            policy() users()
//    change  policy_set(name, value)   (admins)
//            my_lock_timeout(seconds)  (anyone, for themselves)
//            user_role / user_enable / user_delete (admins)
//    secrets request_password_change() user_create_dialog()
//            open the kernel's hidden prompts: no script ever sees
//            a password.
//
//  Every change returns true, or false with the reason shown as a
//  notification. The panel then re-reads everything.
// =============================================================

var SETTINGS_OPEN: bool = false;

// What the panel shows. settings_refresh() fills these; the labels
// read them, so they re-render when it does.
var S_USER:        string = "";
var S_ROLE:        string = "";
var S_ADMIN:       bool   = false;
var S_MY_LOCK:     string = "";
var S_SYS_LOCK:    string = "";
var S_GUEST:       string = "";
var S_BACKOFF:     string = "";
var S_LENGTHS:     string = "";
var S_STEPS_ADMIN: string = "";
var S_STEPS_STD:   string = "";
var S_POLICY = {};

var settings_ref:      element_ref;
var settings_list_ref: element_ref;
var settings_rows: list = [];

// -- Definitions ----------------------------------------------

var SettingsPanel: element.panel = {
    ...Surface,
    visible:          SETTINGS_OPEN,
    position:         (240, 40, 0),
    anchor:           ("left", "top"),
    size:             (560, 600),
    background:       THEME[surface_raised],
    border_thickness: 1,
    border_radius:    8,
    shadow:           (0, 8, 24, #00000088),
    flow: {
        axis:    "vertical",
        justify: "start",
        align:   "stretch",
        gap:     4,
        padding: 12,
    },
};

var SettingsRow: element.row = {
    size: ("100%", 28),
    flow: {
        axis:    "horizontal",
        justify: "start",
        align:   "center",
        gap:     6,
    },
};

var SettingsTitle: element.label = {
    ...Surface,
    background:  TRANSPARENT,
    font_weight: 700,
    size:        ("100%", 24),
};

var SettingsText: element.label = {
    ...Surface,
    background: TRANSPARENT,
    size:       ("auto", 24),
};

var SettingsButton: element.button = {
    ...Button,
    size:             ("auto", 24),
    background:       THEME[surface_sunken],
    border_thickness: 1,
    border_radius:    4,
    padding:          (2, 8),
};

var SettingsSmall: element.button = {
    ...SettingsButton,
    size: (28, 24),
};

// Live text and admin-only parts. Live properties (text, visible)
// must be in a definition: `add` arguments are evaluated once.
var AdminRow: element.row = {
    ...SettingsRow,
    visible: S_ADMIN,
};

var AdminTitle: element.label = {
    ...SettingsTitle,
    visible: S_ADMIN,
    text:    "System (admins)",
};

var AccountTitle: element.label = {
    ...SettingsTitle,
    text: "This account: {S_USER} ({S_ROLE})",
};

var MyLockText:   element.label = { ...SettingsText, text: "Lock my screen after {S_MY_LOCK}" };
var SysLockText:  element.label = { ...SettingsText, text: "Lock everyone after {S_SYS_LOCK}" };
var GuestText:    element.label = { ...SettingsText, text: "Guest account: {S_GUEST}" };
var BackoffText:  element.label = { ...SettingsText, text: "Sign-in delay {S_BACKOFF}" };
var LengthsText:  element.label = { ...SettingsText, text: "Shortest: {S_LENGTHS}" };
var StepsAdmText: element.label = { ...SettingsText, text: "Admins need a password: {S_STEPS_ADMIN}" };
var StepsStdText: element.label = { ...SettingsText, text: "Standard users need a password: {S_STEPS_STD}" };

var AccountList: element.column = {
    visible: S_ADMIN,
    size:    ("100%", 180),
    flow: {
        axis:    "vertical",
        justify: "start",
        align:   "stretch",
        gap:     2,
    },
};

// Buttons. Handlers must be in a definition (`add` takes on_click
// only from there); labels and parameters can be `add` arguments.
var CloseBtn:         element.button = { ...SettingsButton, label: "Close", on_click: settings_close };
var PasswordBtn:      element.button = { ...SettingsButton, label: "Change password...", on_click: settings_password };
var MyLockBtn:        element.button = { ...SettingsSmall, on_click: settings_my_lock_click };
var MyLockDefaultBtn: element.button = { ...SettingsButton, label: "Default", on_click: settings_my_lock_default };
var PolicyBtn:        element.button = { ...SettingsSmall, on_click: settings_policy_click };
var GuestBtn:         element.button = { ...SettingsButton, label: "Turn on/off", on_click: settings_toggle_guest };
var StepsBtn:         element.button = { ...SettingsButton, label: "Change", on_click: settings_steps_click };
var NewAccountBtn:    element.button = { ...SettingsButton, label: "New account...", on_click: settings_new_account };
var AccountToggleBtn: element.button = { ...SettingsButton, on_click: settings_toggle_account };
var AccountRoleBtn:   element.button = { ...SettingsButton, on_click: settings_toggle_role };
var AccountDeleteBtn: element.button = { ...SettingsButton, label: "Delete", color: THEME[danger], on_click: settings_delete_account };

// One account in the list: its name is published on each button
// (`account`), so the handlers know which one was clicked.
var AccountLabel: element.label = {
    ...SettingsText,
    size: (200, 24),
    text: "{account_text}",
};

// -- Reading ----------------------------------------------------

function minutes_text(seconds) {
    if (seconds == 0) {
        return "never";
    }
    return format(int, seconds / 60) + " min";
}

function yes_no(b) {
    return if(b, "yes", "no");
}

function settings_refresh() {
    S_USER  = session_user();
    S_ROLE  = session_role();
    S_ADMIN = session_is_admin();
    S_POLICY = policy();
    var p = S_POLICY;
    if (p.MY_LOCK_TIMEOUT == 0) {
        S_MY_LOCK = "the system's (" + minutes_text(p.LOCK_IDLE_TIMEOUT) + ")";
    } else {
        S_MY_LOCK = minutes_text(p.MY_LOCK_TIMEOUT);
    }
    S_SYS_LOCK    = minutes_text(p.LOCK_IDLE_TIMEOUT);
    S_GUEST       = if(p.LOGIN_GUEST, "on", "off");
    S_BACKOFF     = "after " + format(int, p.LOGIN_BACKOFF_AFTER) + " wrong tries, up to " + format(int, p.LOGIN_BACKOFF_MAX) + " s";
    S_LENGTHS     = "PIN " + format(int, p.PIN_MIN_LENGTH) + "+ digits, password " + format(int, p.PASSWORD_MIN_LENGTH) + "+";
    S_STEPS_ADMIN = yes_no(p.MIN_STEPS_ADMIN > 0);
    S_STEPS_STD   = yes_no(p.MIN_STEPS_STANDARD > 0);
    settings_rebuild_accounts();
}

// The account list: destroy and add again (structural change).
function settings_rebuild_accounts() {
    for ref in settings_rows {
        destroy(ref);
    }
    settings_rows = [];
    if (not S_ADMIN) {
        return;
    }
    for u in users() {
        if (u.role == "guest") {
            continue;
        }
        var row: element_ref = add element.row(SettingsRow, parent: settings_list_ref);
        settings_rows.append(row);
        add element.label(AccountLabel, parent: row,
            account_text: u.name + " (" + u.role + ")" + if(u.enabled, "", " - off"));
        add element.button(AccountToggleBtn, parent: row, account: u.name,
            label: if(u.enabled, "Turn off", "Turn on"),
            enabled_now: u.enabled);
        add element.button(AccountRoleBtn, parent: row, account: u.name,
            label: if(u.role == "admin", "Make standard", "Make admin"),
            role_now: u.role);
        add element.button(AccountDeleteBtn, parent: row, account: u.name);
    }
}

// -- Changing ---------------------------------------------------

function handle_settings() {
    MENU_OPEN = false;
    settings_refresh();
    SETTINGS_OPEN = true;
}

function settings_close() {
    SETTINGS_OPEN = false;
}

// The dialogs are terminal windows: close the panel so it doesn't cover them.
function settings_password() {
    SETTINGS_OPEN = false;
    request_password_change();
}

function settings_my_lock(delta) {
    var p = policy();
    var now = p.MY_LOCK_TIMEOUT;
    if (now == 0) {
        now = p.LOCK_IDLE_TIMEOUT;
    }
    var next = now + delta;
    if (next < 60) {
        next = 60;
    }
    my_lock_timeout(next);
    settings_refresh();
}

// The buttons carry their parameters (`delta`, `setting`, `lowest`):
// handlers in `add` arguments must be functions, not blocks.
function settings_my_lock_click(self) {
    settings_my_lock(self[delta]);
}

function settings_policy_click(self) {
    settings_policy_step(self[setting], self[delta], self[lowest]);
}

function settings_steps_click(self) {
    settings_toggle_steps(self[setting]);
}

function settings_my_lock_default() {
    my_lock_timeout(0);
    settings_refresh();
}

function settings_policy_step(name, delta, lowest) {
    var next = policy()[name] + delta;
    if (next < lowest) {
        next = lowest;
    }
    policy_set(name, next);
    settings_refresh();
}

function settings_toggle_guest() {
    policy_set("LOGIN_GUEST", not policy().LOGIN_GUEST);
    settings_refresh();
}

function settings_toggle_steps(name) {
    policy_set(name, if(policy()[name] > 0, 0, 1));
    settings_refresh();
}

function settings_new_account() {
    SETTINGS_OPEN = false;
    user_create_dialog();
}

function settings_toggle_account(self) {
    user_enable(self[account], not self[enabled_now]);
    settings_refresh();
}

function settings_toggle_role(self) {
    user_role(self[account], if(self[role_now] == "admin", "standard", "admin"));
    settings_refresh();
}

function settings_delete_account(self) {
    user_delete(self[account]);
    settings_refresh();
}

// -- Building (called once by SHELL.VSH's start) ----------------

function settings_build(parent) {
    settings_ref = add element.panel(SettingsPanel, parent: parent);
    var top: element_ref = add element.row(SettingsRow, parent: settings_ref);
    add element.label(SettingsTitle, parent: top, size: (460, 24), text: "Settings");
    add element.button(CloseBtn, parent: top);

    // This account: everyone.
    add element.label(AccountTitle, parent: settings_ref);
    var pw: element_ref = add element.row(SettingsRow, parent: settings_ref);
    add element.button(PasswordBtn, parent: pw);
    var lock: element_ref = add element.row(SettingsRow, parent: settings_ref);
    add element.label(MyLockText, parent: lock);
    add element.button(MyLockBtn, parent: lock, label: "-", delta: -60);
    add element.button(MyLockBtn, parent: lock, label: "+", delta: 60);
    add element.button(MyLockDefaultBtn, parent: lock);

    // System: admins (the buttons would be refused for anyone else).
    add element.label(AdminTitle, parent: settings_ref);
    var sl: element_ref = add element.row(AdminRow, parent: settings_ref);
    add element.label(SysLockText, parent: sl);
    add element.button(PolicyBtn, parent: sl, label: "-", setting: "LOCK_IDLE_TIMEOUT", delta: -60, lowest: 0);
    add element.button(PolicyBtn, parent: sl, label: "+", setting: "LOCK_IDLE_TIMEOUT", delta: 60, lowest: 0);
    var gr: element_ref = add element.row(AdminRow, parent: settings_ref);
    add element.label(GuestText, parent: gr);
    add element.button(GuestBtn, parent: gr);
    var bo: element_ref = add element.row(AdminRow, parent: settings_ref);
    add element.label(BackoffText, parent: bo);
    var bo2: element_ref = add element.row(AdminRow, parent: settings_ref);
    add element.label(SettingsText, parent: bo2, text: "  tries");
    add element.button(PolicyBtn, parent: bo2, label: "-", setting: "LOGIN_BACKOFF_AFTER", delta: -1, lowest: 1);
    add element.button(PolicyBtn, parent: bo2, label: "+", setting: "LOGIN_BACKOFF_AFTER", delta: 1, lowest: 1);
    add element.label(SettingsText, parent: bo2, text: "  longest");
    add element.button(PolicyBtn, parent: bo2, label: "-", setting: "LOGIN_BACKOFF_MAX", delta: -30, lowest: 1);
    add element.button(PolicyBtn, parent: bo2, label: "+", setting: "LOGIN_BACKOFF_MAX", delta: 30, lowest: 1);
    var ln: element_ref = add element.row(AdminRow, parent: settings_ref);
    add element.label(LengthsText, parent: ln);
    var ln2: element_ref = add element.row(AdminRow, parent: settings_ref);
    add element.label(SettingsText, parent: ln2, text: "  PIN");
    add element.button(PolicyBtn, parent: ln2, label: "-", setting: "PIN_MIN_LENGTH", delta: -1, lowest: 4);
    add element.button(PolicyBtn, parent: ln2, label: "+", setting: "PIN_MIN_LENGTH", delta: 1, lowest: 4);
    add element.label(SettingsText, parent: ln2, text: "  password");
    add element.button(PolicyBtn, parent: ln2, label: "-", setting: "PASSWORD_MIN_LENGTH", delta: -1, lowest: 4);
    add element.button(PolicyBtn, parent: ln2, label: "+", setting: "PASSWORD_MIN_LENGTH", delta: 1, lowest: 4);
    var st: element_ref = add element.row(AdminRow, parent: settings_ref);
    add element.label(StepsAdmText, parent: st);
    add element.button(StepsBtn, parent: st, setting: "MIN_STEPS_ADMIN");
    var st2: element_ref = add element.row(AdminRow, parent: settings_ref);
    add element.label(StepsStdText, parent: st2);
    add element.button(StepsBtn, parent: st2, setting: "MIN_STEPS_STANDARD");

    // Accounts: admins.
    var ah: element_ref = add element.row(AdminRow, parent: settings_ref);
    add element.label(SettingsTitle, parent: ah, size: (300, 24), text: "Accounts");
    add element.button(NewAccountBtn, parent: ah);
    settings_list_ref = add element.column(AccountList, parent: settings_ref);
}
