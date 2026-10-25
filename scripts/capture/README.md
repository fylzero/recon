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
1. Generate the demo SQLite database at `/tmp/recon-demo/demo.db`
2. Create isolated app data at `/tmp/recon-demo/home/Library/Application Support/com.recon.app/`
3. Launch Recon in isolated mode
4. Position and resize the window to 1440×900
5. Navigate through the five required screens and capture each:
   - `docs/screenshots/01-connections.png` — Connection dashboard
   - `docs/screenshots/02-table-browser.png` — Table browser with data grid
   - `docs/screenshots/03-query-editor.png` — Query editor with results
   - `docs/screenshots/04-schema-diagram.png` — Schema diagram with relationships
   - `docs/screenshots/05-table-structure.png` — Table structure view
6. Record a 10-15 second demo flow and convert it to `docs/demo.gif` (under ~8MB, 960px wide, 15fps)
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

1. **Schema changes**: Edit `generate-demo-db.ts` to adjust the demo data if needed
2. **Navigation changes**: Edit `capture.sh` to update button names, menu paths, or keyboard shortcuts
3. **New screenshots**: Update the screenshot list in `capture.sh` and add corresponding `capture_screenshot` calls
4. **Window size**: Adjust `WINDOW_WIDTH` and `WINDOW_HEIGHT` in `capture.sh`

Run the script again to regenerate all images:

```bash
./scripts/capture/capture.sh
```

## Troubleshooting

### "Recon has no windows" or window ID not found

The app may be taking longer to launch. Increase the `sleep` durations in `capture.sh` after launching the app and after window operations.

### Screenshots are blank or show the wrong window

Check that Screen Recording permission is granted. The script uses `screencapture -l <windowid>` which requires this permission.

### UI automation doesn't click the right elements

AppleScript UI automation depends on accessibility labels. If button names or UI structure change:

1. Open **Script Editor.app**
2. Run this to inspect the UI hierarchy:
   ```applescript
   tell application "System Events"
     tell process "Recon"
       entire contents of window 1
     end
   end tell
   ```
3. Update `capture.sh` with the correct element names

### Demo GIF is too large

Reduce the recording length or lower the scale/fps in the ffmpeg commands in `capture.sh`:

```bash
# Current: 960px wide, 15fps
scale=960:-1
fps=15

# To reduce size: 800px wide, 12fps
scale=800:-1
fps=12
```

### Script fails partway through

The capture script uses `set -e` (exit on error). Check the output to see which step failed, then:

- Use `--only <name>` to retry a single screenshot
- Add more `sleep` delays if timing issues occur
- Run the script with `bash -x` for verbose output: `bash -x scripts/capture/capture.sh`

## Technical Notes

### Why Not `open -a`?

Using `open -a Recon --env HOME=/tmp/...` doesn't work reliably for overriding `HOME`. Launching the binary directly with `HOME=... /Applications/Recon.app/Contents/MacOS/Recon` ensures the environment variable is set before the app initializes.

### Why JXA Instead of Swift?

A Swift helper would require compilation, adding Xcode as a dependency. JXA (JavaScript for Automation) runs with `osascript -l JavaScript` and is included with macOS, so there's no build step.

### Why palettegen/paletteuse Instead of gifski?

The Mac doesn't have gifski installed, and ffmpeg's two-pass palette method produces good-quality GIFs under 8MB without requiring additional tools.

### Why Bun Instead of Node?

Bun includes built-in TypeScript support and a native SQLite driver, so the data generators run without installing dependencies or separate compilation steps.
