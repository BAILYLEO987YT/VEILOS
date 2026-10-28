set SHELL = "/SHELL.VSH";

// example vsh script, the goal for it is to be,
// simple and user friendly like python but keeping,
// some nice syntax such as the bracket use in rust or java

on_event start {
    // on_event is used to define a function that is called when a specific event is triggered.
    // start is the name of the event for when os launches.
    // { } is used to define the scope of the function.
    // The theme is set by the shell (SHELL.VSH imports /THEMES/DARK.VSH),
    // so this file only holds system settings.
    set WALLPAPER_PATH = "/WALLPAPR.JPG"; // path variable reletive from the root directory of the file system. UNIX STILE NOT WINDOWS (NO \)
    // sets the default wallpaper.
    // wallpaper_path is the system variable for setting the wallpaper, if not set it will default to the default blank colour it is already set to.
    set WALLPAPER_POSITION = "WINDOW_FIT";
    // sets the wallpaper position, STRETCH is the default value.
    // other values include CENTER, TILE, CROP, BASIC, WINDOW_FIT, WINDOW_CENTER, where they are essentially self explanitory;
    // CENTER places the wallpaper in the center of the screen without resizing the image.
    // TILE places the wallpaper in a tiled pattern across the screen, tile pattern repeats so it covers the entire screen.
    // CROP crops the wallpaper to the center of the screen while maintaining the aspect ratio.
    // BASIC places the wallpaper in the center of the screen but doesn't crop it
    // WINDOW_FIT fits the wallpaper to the size of the window while maintaining the aspect ratio.
    // WINDOW_CENTER fits the wallpaper to the center of the window without resizing the image.
    
    set WINDOW_TILES = (2,1); // 4 tiles horizontally, 4 tiles vertically.
    set WINDOW_TILE_SIZE_RATIO = ([1,1], [1,1]); // the first array is for the horizontal tile size ratio, the second array is for the vertical tile size ratio. the tile sizes affect the dimensions of the window enplaced in them.
    set WINDOW_SNAP = "MAGNETIC"; // the snap distance to the nearest tile
    // MAGNETIC, the highest snap distance, feels like the window is being magnetically pulled to the nearest tile.
    // STICKY, the medium snap distance,  feels like the window is gently nudged into place.
    // SLIPPERY, the lowest snap distance, feels like the window is very slightly nudged into place.
    // OFF, the snap distance is off, no snap distance.
    set WINDOW_STACK = False; // if true, windows will stack on top of each other, if false they will be pushed to the next tile over.

    set CLIPBOARD_CHANNELS_DEFAULT = "PRIVATE";
    // CLIPBOARD_CHANNELS_DEFAULT is a system variable for setting the clipboard channels, if not set it will default to the default value "PUBLIC".
    // the clipboard channels are used to define the type of clipboard data, if the data is sensitive it will be stored in the sensitive clipboard.
    // public is unencrypted, stored wherever the user tells it to be stored (in the VSH script).
    // private is encrypted with a key derived from the time and date copied in nanoseconds, stored wherever the user tells it to be stored (in the VSH script).
    // sensitive is encrypted with a key derived from the time and date copied in nanoseconds, stored only in memory.
    // app_restricted is encrypted with a key derived from the time and date copied in nanoseconds, stored only in memory but is only accessible by the app that created it.
    set CLIPBOARD_LOG_SIZE = 50; // the amount of previous clipboard data to be stored. defaults to 20.
    set CLIPBOARD_LOCATION = "/System/clipboard.dat"; // the location where the clipboard data will be stored. defaults to "/system/clipboard.dat"
    set CLIPBOARD_FILE_SIZE_LIMIT = 2048; // the maximum size of the clipboard data file in bytes. defaults to 2048.
    set CLIPBOARD_MAX_SIZE = 2048; // the maximum amount of clipboard data to be stored. defaults to 2048.
    set CLIPBOARD_STORAGE_DEFAULT = "PERMINANT"; // the default clipboard storage. 
    // PERMINANT, stored in the dat file. allows for a single copy and multiple pasting across sessions.
    // TRANSIENT, stored in memory. allows for a single copy and multiple pasting within a session.
    // CLEAR, cleared after pasting. allows for a single copy and single pasting.

    // -----| Mesh Offload Policy |-----
    // Controls whether worker threads are offloaded to remote mesh nodes.
    //
    //   "disabled" — offloading is completely off; all work runs locally.
    //                Use this if you want guaranteed local execution or if
    //                the mesh is not available.
    //
    //   "ask"      — the OffloadBroker will surface a desktop notification
    //                before each offload with APPROVE/DENY controls.
    //                Use this for interactive control over what leaves this node.
    //                This is the default.
    //
    //   "always"   — offload eligible threads automatically whenever a capable
    //                peer is available. Best for dedicated compute nodes where
    //                you want maximum mesh utilisation with no prompts.
    //
    // Win32 threads are handled best-effort regardless of this setting — they
    // are never checkpointed and are not retried on failure.
    var MESH_OFFLOAD_POLICY: string = "ask";
    // To change at runtime use the VScript engine or simply edit this file
    // and restart — the value is read live from the engine on every submit.
};