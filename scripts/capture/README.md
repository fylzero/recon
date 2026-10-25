# Screenshot Capture Automation

Automated tooling to generate README screenshots and demo GIF for Recon. This runs on the maintainer's Mac against the installed app without touching real connections or settings.

## How It Works

The capture system creates an isolated environment for Recon:

1. **Demo Database Generator** (`generate-demo-db.ts`): Creates a realistic SQLite database with:
   - E-commerce schema (customers, orders, products, order_items)
   - Polymorphic relationships (`attachments` and `comments` tables with `*_type` and `*_id` columns)
   - Deliberately missing foreign keys (order_items.product_id → products.id has no FK constraint)
   - Believable fake data (no real people or companies)

2. **Connection Setup** (`setup-demo-connection.ts`): Creates an isolated Tauri app data directory with a pre-configured SQLite connection pointing to the demo database.

3. **Capture Script** (`capture.sh`): Launches Recon with an isolated `HOME` environment variable, drives the UI using AppleScript/System Events to navigate through the five required screens, captures window screenshots with `screencapture -l`, and records a 10-15 second demo that's converted to an optimized GIF using ffmpeg's palettegen/paletteuse.

4. **Window ID Helper** (`get-window-id.jxa`): Uses JavaScript for Automation (JXA) to find Recon's CGWindowID needed for `screencapture -l` (shadow-free window capture).

## Isolation Strategy

The app is launched with a temporary `HOME` directory, which redirects Tauri's app data path:

```bash
HOME=/tmp/recon-demo/home /Applications/Recon.app/Contents/MacOS/Recon
```

This means:
- **Real settings** remain at: `~/Library/Application Support/com.recon.app/`
- **Demo settings** are created at: `/tmp/recon-demo/home/Library/Application Support/com.recon.app/`
- **Keychain** is not affected (demo uses SQLite with no password)
- **Window state** from the isolated settings.json sets the size to 1440×900

### How This Was Determined

From `src-tauri/src/persist.rs`:
```rust
fn app_dir(app: &AppHandle) -> Result<PathBuf, String> {
    let dir = app
        .path()
        .app_data_dir()  // Uses Tauri's standard path resolution
        .map_err(|err| format!("Could not resolve the app data directory: {err}"))?;
    // ...
}
```

And from `src-tauri/tauri.conf.json`:
```json
{
  "identifier": "com.recon.app"
}
```

On macOS, Tauri's `app_data_dir()` resolves to `~/Library/Application Support/<identifier>`, so changing `HOME` isolates the entire app data directory without modifying any app source code.

## Prerequisites

### System Requirements

- **macOS** with Apple Silicon (the app is Apple Silicon only)
- **Recon 1.9.0** installed at `/Applications/Recon.app`
- **Screen Recording permission** granted to Terminal (or whichever terminal app you use)
- **Accessibility permission** granted to Terminal (for UI automation via System Events)

### Tools

- **bun**: For running the TypeScript data generators
- **ffmpeg**: For GIF conversion (no gifski needed)
- **osascript**: Included with macOS

Install bun:
```bash
curl -fsSL https://bun.sh/install | bash
```

Install ffmpeg:
```bash
brew install ffmpeg
```

### Permissions

Grant Screen Recording and Accessibility permissions to your terminal app:

1. Open **System Settings** → **Privacy & Security**
2. Click **Screen Recording** and enable your terminal app (e.g., Terminal.app, iTerm.app)
3. Click **Accessibility** and enable your terminal app

The script will warn you if permissions are missing.

## Running the Capture

From the repository root:

```bash
./scripts/capture/capture.sh
```

This will:
1. Generate a demo SQLite database at `/tmp/recon-demo/demo.db` with 13 tables and rich data
2. Create isolated app data at `/tmp/recon-demo/home/Library/Application Support/com.recon.app/`
3. Launch Recon in isolated mode
4. Position and resize the window to 1440×900
5. Navigate through the five required screens using coordinate-based clicking and capture each:
   - `docs/screenshots/01-connections.png` — Connection dashboard (before opening)
   - `docs/screenshots/02-table-browser.png` — Table browser (customers, Data tab)
   - `docs/screenshots/03-query-editor.png` — Query editor (SQL tab with beautified query and results)
   - `docs/screenshots/04-schema-diagram.png` — Schema diagram (with polymorphic edges on and Fit clicked)
   - `docs/screenshots/05-table-structure.png` — Table structure (orders table, Structure tab)
6. Record a 15-second demo flow and convert it to `docs/demo.gif` (960px wide, 12fps, under 8MB)
7. Quit Recon and clean up

### Options

**Retry a single screenshot:**
```bash
./scripts/capture/capture.sh --only 03-query-editor
```

**Dry run** (show what would happen without capturing):
```bash
./scripts/capture/capture.sh --dry-run
```

## After Capturing

The script outputs screenshots and the demo GIF but **does not commit them**. This lets you review the images first.

To add them to the repository:

```bash
git add docs/screenshots/*.png docs/demo.gif
git commit -m "Update screenshots and demo"
git push
```

The main README already references these paths, so once the images are committed, they'll display automatically.

## Refreshing After UI Changes

When Recon's UI changes:

1. **Schema changes**: Edit `generate-demo-db.ts` to adjust the demo data if needed (currently 13 tables with rich, deterministic seeded data)
2. **Navigation changes**: If button positions change, update the reference coordinates in `capture.sh`. The coordinates are based on a 1024×640 reference window scaled to 1440×900.
3. **New screenshots**: Update the screenshot list in `capture.sh` and add corresponding `capture_screenshot` calls
4. **Window size**: Adjust `WINDOW_WIDTH`, `WINDOW_HEIGHT`, `REF_WIDTH`, and `REF_HEIGHT` in `capture.sh`

Run the script again to regenerate all images:

```bash
./scripts/capture/capture.sh
```

### Finding New Coordinates

To find coordinates for new UI elements:

1. Take a screenshot of the window at 1440×900
2. Downscale it to 1024px wide: `sips -Z 1024 screenshot.png`
3. Open in Preview and use the Inspector (⌘I) to measure pixel coordinates
4. Use those reference coordinates in `click_ref x y` calls

## Troubleshooting

### "Recon is already running" warning

The script detects if Recon is already running and exits to avoid killing the wrong instance. Quit Recon before running the capture script.

### Screenshots are blank or show the wrong window

Check that Screen Recording permission is granted to your terminal app. The script checks for this and will print instructions if capture fails.

Go to: **System Settings** → **Privacy & Security** → **Screen Recording** and enable your terminal.

### Clicks don't hit the right elements

The coordinate mapping may need adjustment. The script uses a reference window size of 1024×640 that scales to 1440×900. If the window size differs or UI layout changes:

1. Capture a screenshot at 1440×900
2. Downscale to 1024px wide: `sips -Z 1024 screenshot.png`
3. Measure new coordinates in Preview (⌘I shows pixel position)
4. Update the `click_ref` calls in `capture.sh`

### Demo GIF is too large

The script targets 960px wide at 12fps. To reduce size further, edit the ffmpeg commands in `capture.sh`:

```bash
# Current: 960px wide, 12fps
scale=960:-1
fps=12

# To reduce: 800px wide, 10fps
scale=800:-1
fps=10
```

### GIF recording fails

The GIF recording is optional. If it fails, the script continues and produces the five screenshots. You can:
- Record manually and convert with ffmpeg
- Skip the GIF entirely
- Check that `screencapture -v` works on your system

### Script fails partway through

- Use `--only <name>` to retry a single screenshot: `./scripts/capture/capture.sh --only 03-query-editor`
- Add more `sleep` delays if timing issues occur (increase sleep durations after tab switches)
- Run with `bash -x` for verbose output: `bash -x scripts/capture/capture.sh`

## Technical Details

### Coordinate-Based Clicking

The app's UI is a webview, so accessibility-based clicking doesn't work. Instead, the script uses screen coordinate clicking via `osascript -e 'tell application "System Events" to click at {X,Y}'`.

Coordinates are defined relative to a 1024×640 reference window (the 2x retina capture downscaled):
- Reference coordinates are converted to screen coordinates: `screen_x = 100 + ref_x * 1440 / 1024`
- The window must be frontmost before clicking: `set frontmost of process "Recon" to true`
- AppleScript stderr contains harmless `-25208` errors that name the clicked element; these are ignored

**Verified click targets on Recon 1.9.0:**
- Connection list row 'Demo Store': (120, 181)
- Tabs: Tables (62, 85), SQL (120, 85), Diagram (175, 85)
- Table sidebar rows at x=45: attachments 135, comments 152, customers 169, order_items 186, orders 203, products 220
- Data/Structure/Indexes sub-tabs: Data (219, 134), Structure (273, 134)
- Diagram pills: '2 foreign keys' (305, 111), '2 polymorphic' (396, 111), '1 missing key' (476, 111), Fit (952, 111)
- SQL editor: editor area (400, 200), Beautify (926, 133), Run (27, 133)

### Window ID Detection

The script uses inline JXA to find the CGWindowID needed for `screencapture -l`:

```javascript
ObjC.import("CoreGraphics"); 
var l=ObjC.deepUnwrap(ObjC.castRefToObject($.CGWindowListCopyWindowInfo($.kCGWindowListOptionOnScreenOnly,0))); 
l.filter(w=>w.kCGWindowOwnerName=="Recon"&&w.kCGWindowLayer==0).map(w=>w.kCGWindowNumber).join("")
```

The window ID is queried twice: once after launch, and again after resizing (the ID can change).

### Safe Instance Management

The script only kills the demo Recon instance it launched (by remembering its PID). If a non-demo Recon is already running when the script starts, it warns and exits rather than risking killing the wrong instance.

### Permission Detection

After each `screencapture -l` call, the script checks that the command exited 0 and the file exists. If not, it prints instructions to grant Screen Recording permission in System Settings → Privacy & Security.

## Additional Technical Notes

### Why Not `open -a`?

Using `open -a Recon --env HOME=/tmp/...` doesn't work reliably for overriding `HOME`. Launching the binary directly with `HOME=... /Applications/Recon.app/Contents/MacOS/Recon` ensures the environment variable is set before the app initializes.

### Why Inline JXA?

The original approach used a separate `.jxa` file, but `osascript file.jxa` doesn't treat it as JavaScript (the `#!` line causes syntax errors). Inline JXA with `osascript -l JavaScript -e '...'` works without a separate file.

### Why palettegen/paletteuse?

The Mac doesn't have gifski installed, and ffmpeg's two-pass palette method produces good-quality GIFs under 8MB without requiring additional tools.

### Why Bun?

Bun includes built-in TypeScript support and a native SQLite driver, so the data generators run without installing dependencies or separate compilation steps.
