// =============================================================
//  /THEMES/DARK.VSH - the default dark theme
//
//  Defines DARK, an object with every key of the theme contract
//  in SHELL.VSH Section 2. Copy this file, change the values and
//  `set THEME = YOURS;` to switch. Missing keys show the error
//  colour (magenta) on the elements that use them.
// =============================================================

var DARK = {
    background:     #1e1e2e,   // base window and panel surface
    foreground:     #eaeaf5,   // primary text
    foreground_dim: #9696aa,   // secondary text, tray indicators
    accent:         #5882dc,   // primary action / selection
    accent_hover:   #6e9bf0,   // accent on hover
    danger:         #d23737,   // destructive actions
    border:         #46466a,   // borders, dividers, separators
    surface_raised: #2a2a40,   // menus, dropdowns
    surface_sunken: #141420,   // inputs, wells
    border_radius:  6,
    font_family:    "default", // the first font loaded from /FONTS
    font_size:      14,
};
